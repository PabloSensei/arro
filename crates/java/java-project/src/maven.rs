use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use base::program::cmd_script;
use base::{Cancel, Result};
use proc_sup::ProcessSpec;
use project_model::{
    BuildGroup, BuildModule, BuildSystem, BuildTask, Exclusion, FileEdit, MAIN_UNIT, Module,
    ModuleDependencies, PathEntry, PathKind, ProjectModel, RecordedTask, TEST_UNIT, TaskOptions,
    Unit, cache_file,
};
use serde::Deserialize;

use crate::BuildEnv;
use crate::dependencies::{ReportTrees, exclude_in_pom};
use crate::env::{fresh_directory, run_tool};
use crate::fingerprint::{BuildStamp, has_classes};
use crate::pom::scan_poms;

const EXTENSION: &str = "maven-ext.jar";
/// The earliest phase with no plugin bound to it: the extension does its work before any
/// plugin runs, and the phase must not touch the project (G0 decision).
const MODEL_PHASE: &str = "pre-clean";
/// Classes and resources are in place at this phase, nothing gets packaged or tested.
const ASSEMBLE_PHASE: &str = "process-classes";
/// The lifecycle phases people run by hand.
const PHASES: &[&str] = &[
    "clean", "validate", "compile", "test", "package", "verify", "install",
];

/// `mvnw` of the project first: it pins the version the project was written for.
pub fn find_maven(root: &Path) -> Option<PathBuf> {
    let wrapper = root.join(cmd_script("mvnw"));
    if wrapper.is_file() {
        return Some(wrapper);
    }
    let names: &[&str] = if cfg!(windows) {
        &["mvn.cmd", "mvn.bat"]
    } else {
        &["mvn"]
    };
    let path = env::var_os("PATH")?;
    env::split_paths(&path)
        .flat_map(|directory| {
            names
                .iter()
                .map(move |name| directory.join(name))
                .collect::<Vec<_>>()
        })
        .find(|file| file.is_file())
}

pub struct Maven {
    pub(crate) env: BuildEnv,
    /// The inputs of a build that started and has not reported success yet.
    stamp: BuildStamp,
}

impl Maven {
    pub fn new(env: BuildEnv) -> Self {
        Self {
            env,
            stamp: BuildStamp::new(),
        }
    }

    pub(crate) fn command(&self, root: &Path) -> Result<ProcessSpec> {
        let settings = &self.env.build_tools.first;
        let maven = match settings.home_path() {
            Some(home) => {
                let program = Path::new(home).join("bin").join(cmd_script("mvn"));
                if !program.is_file() {
                    return Err(format!("no {} in {}", cmd_script("mvn"), program.display()).into());
                }
                program
            }
            None => find_maven(root).ok_or_else(|| {
                format!(
                    "no {} in {} and no mvn in PATH",
                    cmd_script("mvnw"),
                    root.display()
                )
            })?,
        };
        let jdk = self.env.project_jdk(root)?;
        let mut spec = ProcessSpec::new(maven)
            .cwd(root)
            .args(["-B", "-ntp"])
            .label("Build tool");
        if settings.offline {
            spec = spec.arg("-o");
        }
        if settings.update_snapshots {
            spec = spec.arg("-U");
        }
        if let Some(file) = settings.settings_path() {
            spec = spec.arg("-s").arg(file);
        }
        if let Some(options) = project_model::BuildToolSettings::path(&settings.vm_options) {
            spec = spec.env("MAVEN_OPTS", options);
        }
        Ok(self.env.apply(spec, &jdk).args(&self.env.maven_args))
    }

    /// Where the fingerprint of the last successful build waits.
    fn stamp_file(&self, root: &Path) -> PathBuf {
        cache_file(&self.env.cache_root(), root).with_file_name("build-stamp")
    }
}

impl BuildSystem for Maven {
    fn detect(&self, root: &Path) -> bool {
        root.join("pom.xml").is_file()
    }

    fn scan(&self, root: &Path) -> Result<ProjectModel> {
        scan_poms(root)
    }

    fn sync(&self, root: &Path, cancel: &Cancel) -> Result<ProjectModel> {
        let extension = self.env.helpers_dir()?.join(EXTENSION);
        if !extension.is_file() {
            return Err(format!("no helper at {}", extension.display()).into());
        }
        let report = cache_file(&self.env.cache_root(), root).with_file_name("sync");
        let report = fresh_directory(&report)?.join("maven.json");
        let spec = self
            .command(root)?
            .arg(format!("-Dmaven.ext.class.path={}", extension.display()))
            .arg(format!("-Dide.model.out={}", report.display()))
            .arg(MODEL_PHASE);
        run_tool(&spec, cancel)?;
        parse_report(&fs::read(&report)?, root)
    }

    fn assemble(&self, root: &Path) -> Result<ProcessSpec> {
        Ok(self.command(root)?.arg(ASSEMBLE_PHASE))
    }

    fn assemble_if_needed(&self, root: &Path) -> Result<Option<ProcessSpec>> {
        // The same phase as a full assemble: plugins bound to process-classes (bytecode
        // enhancers) must run too. Online, so a new dependency in the POM still resolves.
        self.stamp
            .needed(root, &self.stamp_file(root), has_classes(root), || {
                self.assemble(root)
            })
    }

    fn built(&self, root: &Path) {
        self.stamp.built(root, &self.stamp_file(root));
    }

    fn build(&self, root: &Path, tests: bool) -> Result<ProcessSpec> {
        let spec = self.command(root)?.arg("compile");
        Ok(if tests {
            spec.arg("test-compile")
        } else {
            spec
        })
    }

    fn compile(&self, root: &Path, module: Option<&Module>) -> Result<ProcessSpec> {
        let spec = self.command(root)?;
        let directory = module
            .map(|module| module.dir.strip_prefix(root).unwrap_or(&module.dir))
            .filter(|directory| !directory.as_os_str().is_empty());
        Ok(match directory {
            // `-am` builds the modules it depends on too, as the reactor would.
            Some(directory) => spec.arg("-pl").arg(directory).arg("-am").arg("compile"),
            None => spec.arg("compile"),
        })
    }

    fn tasks(&self, _model: &ProjectModel) -> Vec<String> {
        PHASES.iter().map(|phase| (*phase).to_owned()).collect()
    }
    fn task_tree(&self, model: &ProjectModel) -> Vec<BuildModule> {
        let mut modules: Vec<&Module> = model.modules.iter().collect();
        // The root module comes first; the rest keep the order of the files.
        modules.sort_by_key(|module| module.dir != model.root);
        modules
            .into_iter()
            .map(|module| {
                let lifecycle = PHASES
                    .iter()
                    .map(|phase| BuildTask {
                        name: (*phase).to_owned(),
                        program: false,
                    })
                    .collect();
                let mut groups = vec![BuildGroup {
                    name: "Lifecycle".into(),
                    tasks: lifecycle,
                }];
                if has_spring_boot(module) {
                    groups.push(BuildGroup {
                        name: "Plugins".into(),
                        tasks: vec![BuildTask {
                            name: "spring-boot:run".into(),
                            program: true,
                        }],
                    });
                }
                if has_quarkus(module) {
                    groups.push(BuildGroup {
                        name: "Quarkus".into(),
                        tasks: vec![BuildTask {
                            name: "quarkus:dev".into(),
                            program: true,
                        }],
                    });
                }
                BuildModule {
                    id: module.id.clone(),
                    name: module.name.clone(),
                    groups,
                }
            })
            .collect()
    }

    fn task(&self, root: &Path, task: &str) -> Result<ProcessSpec> {
        Ok(self.command(root)?.arg(task))
    }

    fn task_in(
        &self,
        root: &Path,
        module: Option<&Module>,
        task: &str,
        options: TaskOptions,
    ) -> Result<ProcessSpec> {
        let spec = self
            .command(root)?
            .args(run_args(root, module, task, options));
        // The application the plugin starts may read its input, as a console program does.
        Ok(
            match task.ends_with("spring-boot:run") || task.ends_with("quarkus:dev") {
                true => spec.input(),
                false => spec,
            },
        )
    }

    fn goal(&self, root: &Path, line: &str, options: TaskOptions) -> Result<ProcessSpec> {
        let mut spec = self.command(root)?;
        let mut runs_program = false;
        for word in line.split_whitespace() {
            runs_program |= word.ends_with("spring-boot:run") || word.ends_with("quarkus:dev");
            spec = spec.arg(word);
        }
        if options.skip_tests {
            spec = spec.arg("-DskipTests");
        }
        Ok(match runs_program {
            true => spec.input(),
            false => spec,
        })
    }

    fn debug_task_in(
        &self,
        root: &Path,
        module: Option<&Module>,
        task: &str,
        options: TaskOptions,
    ) -> Result<ProcessSpec> {
        let spec = self.task_in(root, module, task, options)?;
        if task.ends_with("spring-boot:run") {
            // The application starts inside Maven: the debug agent belongs on that JVM.
            return Ok(spec.arg(
                "-Dspring-boot.run.jvmArguments=-agentlib:jdwp=transport=dt_socket,server=y,suspend=n,address=*:0",
            ));
        }
        Ok(spec)
    }

    fn supports_skip_tests(&self) -> bool {
        true
    }

    fn supports_profiler(&self) -> bool {
        true
    }

    /// The run goal gets the flag through `spring-boot.run.jvmArguments`, the tests
    /// through the `argLine` of Surefire.
    fn profile_task_in(
        &self,
        root: &Path,
        module: Option<&Module>,
        task: &str,
        options: TaskOptions,
    ) -> Result<Option<RecordedTask>> {
        let recording = crate::profile::recording_file(&self.env.cache_root(), &task_slug(task))?;
        let flag = crate::profile::recording_flag(&recording);
        let spec = self
            .command(root)?
            .args(run_args(root, module, task, options));
        let Some(argument) = profile_argument(task, &flag, crate::test_runs::has_argline(root))
        else {
            return Ok(None);
        };
        let spec = match task.ends_with("spring-boot:run") {
            true => spec.input(),
            false => spec,
        }
        .arg(argument);
        Ok(Some(RecordedTask {
            program: spec,
            recording,
        }))
    }

    fn profile_report(
        &self,
        recording: &Path,
        model: &ProjectModel,
    ) -> Result<lang_api::ProfileReport> {
        crate::profile::report_of(&self.env, recording, model)
    }

    fn profile_recordings(&self, _model: &ProjectModel) -> Result<Vec<lang_api::ProfileRecording>> {
        Ok(crate::profile::recordings(&self.env.cache_root()))
    }

    fn profile_run_recordings(&self, recording: &Path) -> Vec<PathBuf> {
        crate::profile::run_recordings(recording)
    }

    fn exclude(
        &self,
        _root: &Path,
        _module: &Module,
        build_file: &Path,
        request: &Exclusion,
        text: &str,
    ) -> Result<Option<FileEdit>> {
        Ok(exclude_in_pom(text, request, build_file))
    }
}

/// The reactor arguments for one module: `-pl` from the root, which works with a wrapper
/// there. A module outside the root, which a POM may still name, is read by its own file.
fn module_args(root: &Path, module: Option<&Module>) -> Vec<String> {
    let Some(module) = module else {
        return Vec::new();
    };
    let directory = module.dir.strip_prefix(root).unwrap_or(&module.dir);
    if directory.as_os_str().is_empty() {
        return Vec::new();
    }
    if module.dir.starts_with(root) {
        return vec!["-pl".into(), directory.display().to_string()];
    }
    vec![
        "-f".into(),
        module.dir.join("pom.xml").display().to_string(),
    ]
}

/// The arguments of a task of a module, with the flag that leaves the tests out.
fn run_args(root: &Path, module: Option<&Module>, task: &str, options: TaskOptions) -> Vec<String> {
    let mut args = module_args(root, module);
    args.push(task.to_owned());
    if options.skip_tests {
        args.push("-DskipTests".into());
    }
    args
}

/// The argument that puts the recording flag into the JVM of a task. The run goal starts
/// its JVMs through `spring-boot.run.jvmArguments`, the tests through Surefire's
/// `argLine`, which keeps the flags of the project through `@{argLine}`.
fn profile_argument(task: &str, flag: &str, keep_argline: bool) -> Option<String> {
    if task.ends_with("spring-boot:run") {
        return Some(format!("-Dspring-boot.run.jvmArguments={flag}"));
    }
    if task == "test" {
        return Some(crate::test_runs::profile_argline(flag, keep_argline));
    }
    None
}

/// The name of a recording: the task without the punctuation a file name cannot keep.
fn task_slug(task: &str) -> String {
    task.chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '-'
            }
        })
        .collect()
}

/// Whether the module applies the Spring Boot plugin, so that its run goal exists.
fn has_spring_boot(module: &Module) -> bool {
    let Ok(text) = fs::read_to_string(module.dir.join("pom.xml")) else {
        return false;
    };
    text.contains("spring-boot-maven-plugin")
}

/// Whether the module applies a Quarkus plugin, so that its dev goal exists.
fn has_quarkus(module: &Module) -> bool {
    let Ok(text) = fs::read_to_string(module.dir.join("pom.xml")) else {
        return false;
    };
    text.contains("io.quarkus")
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Report {
    tool_version: Option<String>,
    projects: Vec<ReportProject>,
}

/// The dependency trees of one module, one per scope.
///
/// The parser lives in [`crate::dependencies`], shared with the other tool.
/// The report of a `pom.xml` names the file it came from.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReportProject {
    id: String,
    base_dir: PathBuf,
    #[serde(default)]
    build_file: Option<PathBuf>,
    source_roots: Vec<PathBuf>,
    test_source_roots: Vec<PathBuf>,
    #[serde(default)]
    resource_roots: Vec<PathBuf>,
    #[serde(default)]
    test_resource_roots: Vec<PathBuf>,
    output_dir: PathBuf,
    test_output_dir: PathBuf,
    compile_classpath: Vec<ReportEntry>,
    runtime_classpath: Vec<ReportEntry>,
    test_classpath: Vec<ReportEntry>,
    #[serde(default)]
    dependencies: ReportTrees,
    problems: Vec<String>,
}

#[derive(Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum ReportEntry {
    Module {
        module: String,
        path: PathBuf,
        built: bool,
    },
    Library {
        coordinates: String,
        path: Option<PathBuf>,
    },
}

impl ReportEntry {
    /// A library that did not resolve has no file, the report lists it among the problems.
    fn into_path(self) -> Option<PathEntry> {
        match self {
            Self::Module {
                module,
                path,
                built,
            } => Some(PathEntry {
                path,
                kind: PathKind::Module { id: module, built },
            }),
            Self::Library { coordinates, path } => Some(PathEntry {
                path: path?,
                kind: PathKind::Library { coordinates },
            }),
        }
    }
}

fn paths(entries: Vec<ReportEntry>) -> Vec<PathEntry> {
    entries
        .into_iter()
        .filter_map(ReportEntry::into_path)
        .collect()
}

fn parse_report(bytes: &[u8], root: &Path) -> Result<ProjectModel> {
    let report: Report = serde_json::from_slice(bytes)?;
    let mut problems = Vec::new();
    let modules = report.projects.into_iter().map(|project| {
        problems.extend(project.problems);
        let name = project
            .id
            .split(':')
            .nth(1)
            .unwrap_or(&project.id)
            .to_owned();
        let main_output = PathEntry {
            path: project.output_dir.clone(),
            kind: PathKind::File,
        };
        let mut test_paths = vec![main_output];
        test_paths.extend(paths(project.test_classpath));
        let main = Unit {
            name: MAIN_UNIT.into(),
            source_roots: project.source_roots,
            resource_roots: project.resource_roots,
            output_dirs: vec![project.output_dir],
            compile_paths: paths(project.compile_classpath),
            runtime_paths: paths(project.runtime_classpath),
        };
        let test = Unit {
            name: TEST_UNIT.into(),
            source_roots: project.test_source_roots,
            resource_roots: project.test_resource_roots,
            output_dirs: vec![project.test_output_dir],
            compile_paths: test_paths.clone(),
            runtime_paths: test_paths,
        };
        let dependencies = ModuleDependencies {
            module: project.id.clone(),
            build_file: project.build_file,
            scopes: project.dependencies.into_scopes(),
        };
        (
            Module {
                id: project.id,
                name,
                dir: project.base_dir,
                units: vec![main, test],
            },
            dependencies,
        )
    });
    let pairs: Vec<(Module, ModuleDependencies)> = modules.collect();
    let (modules, dependencies): (Vec<Module>, Vec<ModuleDependencies>) = pairs.into_iter().unzip();
    Ok(ProjectModel {
        root: root.to_owned(),
        tool: "maven".into(),
        tool_version: report.tool_version,
        modules,
        dependencies,
        resolved: true,
        problems,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fingerprint::{fingerprint, read_stamp};

    const REPORT: &str = r#"{
      "schema": 1, "tool": "maven", "toolVersion": "3.9.16", "javaVersion": "17",
      "projects": [{
        "id": "demo:app:1.0", "packaging": "jar", "baseDir": "C:/p/app",
        "buildFile": "C:/p/app/pom.xml",
        "sourceRoots": ["C:/p/app/src/main/java"],
        "testSourceRoots": ["C:/p/app/src/test/java"],
        "resourceRoots": ["C:/p/app/src/main/resources"],
        "outputDir": "C:/p/app/target/classes",
        "testOutputDir": "C:/p/app/target/test-classes",
        "compileClasspath": [
          {"kind": "module", "module": "demo:core:1.0", "path": "C:/p/core/target/classes",
           "built": false}
        ],
        "runtimeClasspath": [
          {"kind": "library", "coordinates": "g:a:jar:1", "path": "C:/r/a.jar"},
          {"kind": "library", "coordinates": "g:gone:jar:1", "path": null}
        ],
        "testClasspath": [
          {"kind": "library", "coordinates": "g:t:jar:1", "path": "C:/r/t.jar"}
        ],
        "dependencies": {
          "compile": [{
            "coordinates": "org.springframework.boot:spring-boot-starter-web:3.5.0",
            "scope": "compile", "path": "C:/r/starter.jar",
            "children": [
              {"coordinates": "org.springframework:spring-web:6.2.0", "scope": "compile",
               "requested": "org.springframework:spring-web:6.1.0", "path": "C:/r/web.jar"},
              {"coordinates": "com.fasterxml.jackson.core:jackson-databind:2.15.0",
               "scope": "compile", "winner": "com.fasterxml.jackson.core:jackson-databind:2.17.0",
               "path": "C:/r/old-databind.jar"}
            ]
          }],
          "runtime": [{
            "coordinates": "org.springframework.boot:spring-boot-starter-web:3.5.0",
            "scope": "compile",
            "children": [
              {"coordinates": "org.springframework:spring-web:6.2.0", "scope": "compile"}
            ]
          }],
          "test": [{
            "coordinates": "org.junit.jupiter:junit-jupiter:5.11.0", "scope": "test",
            "path": "C:/r/junit.jar"
          }]
        },
        "problems": ["not resolved: g:gone:jar:1"]
      }]
    }"#;

    #[test]
    fn an_unchanged_project_after_a_successful_build_needs_no_maven() {
        let root = test_support::scratch("maven-fast");
        fs::create_dir_all(root.join("src/main/java/demo")).unwrap();
        fs::write(root.join("pom.xml"), "<project/>").unwrap();
        fs::write(root.join("src/main/java/demo/App.java"), "class App {}").unwrap();
        fs::create_dir_all(root.join("target/classes/demo")).unwrap();
        fs::write(root.join("target/classes/demo/App.class"), b"class").unwrap();
        let tool = Maven::new(BuildEnv {
            cache_root: Some(root.join("cache")),
            ..BuildEnv::default()
        });

        tool.built(&root);

        assert!(
            tool.assemble_if_needed(&root).unwrap().is_none(),
            "the classes and the stamp make the build unnecessary"
        );
        fs::write(
            root.join("src/main/java/demo/App.java"),
            "class App { int value; }",
        )
        .unwrap();
        assert_ne!(
            fingerprint(&root).unwrap(),
            read_stamp(&tool.stamp_file(&root)).unwrap(),
            "a changed source makes the stamp old"
        );
    }

    #[test]
    fn a_report_becomes_a_model() {
        let model = parse_report(REPORT.as_bytes(), Path::new("C:/p")).unwrap();

        assert!(model.resolved);
        assert_eq!(model.tool_version.as_deref(), Some("3.9.16"));
        assert_eq!(model.problems, ["not resolved: g:gone:jar:1"]);
        let module = model.module("demo:app:1.0").unwrap();
        assert_eq!(module.name, "app");
        let main = module.unit(MAIN_UNIT).unwrap();
        let neighbour = PathKind::Module {
            id: "demo:core:1.0".into(),
            built: false,
        };
        assert_eq!(main.compile_paths[0].kind, neighbour);
        // The entry without a file is left out, not passed along as an empty path.
        assert_eq!(main.runtime_paths.len(), 1);
    }

    #[test]
    fn tests_see_the_main_output_first() {
        let model = parse_report(REPORT.as_bytes(), Path::new("C:/p")).unwrap();

        let paths = model.runtime_paths("demo:app:1.0", TEST_UNIT);

        let expected = [
            "C:/p/app/target/test-classes",
            "C:/p/app/target/classes",
            "C:/r/t.jar",
        ];
        assert_eq!(paths, expected.map(PathBuf::from));
    }

    #[test]
    fn a_broken_report_is_an_error() {
        assert!(parse_report(b"{}", Path::new("C:/p")).is_err());
    }

    #[test]
    fn the_report_keeps_the_dependency_tree_of_every_scope() {
        let model = parse_report(REPORT.as_bytes(), Path::new("C:/p")).unwrap();

        let module = model.module("demo:app:1.0").unwrap();
        assert_eq!(module.name, "app");
        let found = model.dependencies_of("demo:app:1.0").unwrap();
        assert_eq!(
            found.build_file.as_deref(),
            Some(Path::new("C:/p/app/pom.xml"))
        );
        let scopes: Vec<_> = found
            .scopes
            .iter()
            .map(|scope| scope.name.as_str())
            .collect();
        assert_eq!(scopes, ["compile", "runtime", "test"]);
        let compile = found.scope("compile").unwrap();
        let root = &compile.roots[0];
        assert_eq!(
            root.coordinates,
            "org.springframework.boot:spring-boot-starter-web:3.5.0"
        );
        assert_eq!(root.children.len(), 2);
        assert_eq!(
            root.children[0].requested.as_deref(),
            Some("org.springframework:spring-web:6.1.0"),
            "the version the build asks for stays next to the chosen one"
        );
        assert_eq!(
            root.children[1].winner.as_deref(),
            Some("com.fasterxml.jackson.core:jackson-databind:2.17.0"),
            "the edge that lost a conflict names the winner"
        );
        assert_eq!(
            root.children[1].requested_version(),
            None,
            "an edge without a requested version asks for nothing else"
        );
        assert_eq!(found.scope("test").unwrap().roots[0].scope, "test");
    }

    fn multi_module(name: &str, core_plugin: &str) -> PathBuf {
        let root = test_support::scratch(&format!("project-maven-tree-{name}"));
        fs::create_dir_all(root.join("core")).unwrap();
        fs::create_dir_all(root.join("app")).unwrap();
        fs::write(
            root.join("pom.xml"),
            "<project><groupId>demo</groupId><artifactId>parent</artifactId>\
             <version>1.0</version><modules><module>core</module>\
             <module>app/pom.xml</module></modules></project>",
        )
        .unwrap();
        fs::write(
            root.join("core/pom.xml"),
            format!(
                "<project><parent><groupId>demo</groupId><version>1.0</version></parent>\
                 <artifactId>core</artifactId>{core_plugin}</project>"
            ),
        )
        .unwrap();
        fs::write(
            root.join("app/pom.xml"),
            "<project><parent><groupId>demo</groupId><version>1.0</version></parent>\
             <artifactId>app</artifactId></project>",
        )
        .unwrap();
        root
    }

    #[test]
    fn the_tree_lists_the_root_module_first_and_its_lifecycle() {
        let root = multi_module("plain", "");
        let model = scan_poms(&root).unwrap();

        let tree = Maven::new(BuildEnv::default()).task_tree(&model);

        let names: Vec<_> = tree.iter().map(|module| module.name.as_str()).collect();
        assert_eq!(names, ["parent", "core", "app"]);
        assert_eq!(tree[0].groups[0].name, "Lifecycle");
        let phases: Vec<_> = tree[0].groups[0]
            .tasks
            .iter()
            .map(|task| task.name.as_str())
            .collect();
        assert_eq!(phases, PHASES);
        assert!(
            !tree[1].groups.iter().any(|group| group.name == "Plugins"),
            "no plugin means no run goal"
        );
    }

    #[test]
    fn a_spring_boot_module_gets_its_run_goal_only() {
        let plugin = "<build><plugins><plugin>\
                      <groupId>org.springframework.boot</groupId>\
                      <artifactId>spring-boot-maven-plugin</artifactId>\
                      </plugin></plugins></build>";
        let root = multi_module("boot", plugin);
        let model = scan_poms(&root).unwrap();

        let tree = Maven::new(BuildEnv::default()).task_tree(&model);

        let core = tree.iter().find(|module| module.name == "core").unwrap();
        let plugins = core
            .groups
            .iter()
            .find(|group| group.name == "Plugins")
            .expect("the plugin group is there");
        assert_eq!(plugins.tasks.len(), 1);
        assert_eq!(plugins.tasks[0].name, "spring-boot:run");
        assert!(plugins.tasks[0].program);
        let app = tree.iter().find(|module| module.name == "app").unwrap();
        assert!(!app.groups.iter().any(|group| group.name == "Plugins"));
    }

    #[test]
    fn a_quarkus_module_gets_its_dev_goal() {
        let plugin = "<build><plugins><plugin>\
                      <groupId>io.quarkus</groupId>\
                      <artifactId>quarkus-maven-plugin</artifactId>\
                      </plugin></plugins></build>";
        let root = multi_module("quarkus", plugin);
        let model = scan_poms(&root).unwrap();

        let tree = Maven::new(BuildEnv::default()).task_tree(&model);

        let core = tree.iter().find(|module| module.name == "core").unwrap();
        let quarkus = core
            .groups
            .iter()
            .find(|group| group.name == "Quarkus")
            .expect("the Quarkus group is there");
        assert_eq!(quarkus.tasks.len(), 1);
        assert_eq!(quarkus.tasks[0].name, "quarkus:dev");
        assert!(quarkus.tasks[0].program);
        let app = tree.iter().find(|module| module.name == "app").unwrap();
        assert!(!app.groups.iter().any(|group| group.name == "Quarkus"));
    }

    #[test]
    fn a_module_runs_from_the_reactor_root() {
        let root = Path::new("C:/p");
        let module = Module {
            id: "demo:core:1.0".into(),
            name: "core".into(),
            dir: PathBuf::from("C:/p/core"),
            ..Module::default()
        };

        assert_eq!(
            module_args(Path::new("C:/p/core"), Some(&module)),
            Vec::<String>::new(),
            "the root module needs no selector"
        );
        assert_eq!(
            module_args(root, Some(&module)),
            ["-pl".to_owned(), "core".to_owned()]
        );
        let outside = Module {
            dir: PathBuf::from("C:/other"),
            ..Module::default()
        };
        let args = module_args(root, Some(&outside));
        assert_eq!(args[0], "-f");
        assert_eq!(
            Path::new(&args[1]),
            PathBuf::from("C:/other").join("pom.xml"),
            "a module outside the root is read by its own file"
        );
    }

    #[test]
    fn the_skip_flag_joins_a_task_and_not_a_root_run() {
        let root = Path::new("C:/p");
        let module = Module {
            dir: PathBuf::from("C:/p/core"),
            ..Module::default()
        };
        let skip = TaskOptions { skip_tests: true };

        assert_eq!(
            run_args(root, Some(&module), "test", skip),
            [
                "-pl".to_owned(),
                "core".to_owned(),
                "test".to_owned(),
                "-DskipTests".to_owned()
            ]
        );
        assert_eq!(
            run_args(root, None, "compile", TaskOptions::default()),
            ["compile".to_owned()]
        );
    }

    #[test]
    fn a_recorded_task_takes_the_flag_of_its_kind() {
        let flag = "-XX:StartFlightRecording=filename=\"C:/cache/profiler/test-1.jfr\"";

        assert_eq!(
            profile_argument("spring-boot:run", flag, false).as_deref(),
            Some(format!("-Dspring-boot.run.jvmArguments={flag}").as_str())
        );
        assert_eq!(
            profile_argument("test", flag, false).as_deref(),
            Some(format!("-DargLine={flag}").as_str())
        );
        assert_eq!(
            profile_argument("test", flag, true).as_deref(),
            Some(format!("-DargLine=@{{argLine}} {flag}").as_str())
        );
        assert_eq!(profile_argument("compile", flag, false), None);
    }
}
