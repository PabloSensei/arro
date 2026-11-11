use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use base::{Cancel, Result};
use proc_sup::ProcessSpec;
use project_model::{
    BuildGroup, BuildModule, BuildSystem, BuildTask, MAIN_UNIT, Module, PathEntry, PathKind,
    ProjectModel, TEST_UNIT, TaskOptions, Unit, cache_file,
};
use serde::Deserialize;

use crate::BuildEnv;
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
    let wrapper = root.join("mvnw.cmd");
    if wrapper.is_file() {
        return Some(wrapper);
    }
    let path = env::var_os("PATH")?;
    env::split_paths(&path)
        .flat_map(|directory| ["mvn.cmd", "mvn.bat"].map(|name| directory.join(name)))
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
        let maven = find_maven(root)
            .ok_or_else(|| format!("no mvnw.cmd in {} and no mvn in PATH", root.display()))?;
        let jdk = self.env.project_jdk(root)?;
        let spec = ProcessSpec::new(maven)
            .cwd(root)
            .args(["-B", "-ntp"])
            .label("Build tool");
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
        Ok(self
            .command(root)?
            .args(run_args(root, module, task, options)))
    }

    fn goal(&self, root: &Path, line: &str, options: TaskOptions) -> Result<ProcessSpec> {
        let mut spec = self.command(root)?;
        for word in line.split_whitespace() {
            spec = spec.arg(word);
        }
        if options.skip_tests {
            spec = spec.arg("-DskipTests");
        }
        Ok(spec)
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

/// Whether the module applies the Spring Boot plugin, so that its run goal exists.
fn has_spring_boot(module: &Module) -> bool {
    let Ok(text) = fs::read_to_string(module.dir.join("pom.xml")) else {
        return false;
    };
    text.contains("spring-boot-maven-plugin")
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Report {
    tool_version: Option<String>,
    projects: Vec<ReportProject>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReportProject {
    id: String,
    base_dir: PathBuf,
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
        Module {
            id: project.id,
            name,
            dir: project.base_dir,
            units: vec![main, test],
        }
    });
    let modules = modules.collect();
    Ok(ProjectModel {
        root: root.to_owned(),
        tool: "maven".into(),
        tool_version: report.tool_version,
        modules,
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
        "problems": ["not resolved: g:gone:jar:1"]
      }]
    }"#;

    #[test]
    fn an_unchanged_project_after_a_successful_build_needs_no_maven() {
        let root = std::env::temp_dir().join(format!("maven-fast-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
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
        let _ = fs::remove_dir_all(&root);
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

    fn multi_module(name: &str, core_plugin: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "java-project-maven-tree-{}-{name}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
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
        let _ = fs::remove_dir_all(root);
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
        let _ = fs::remove_dir_all(root);
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
}
