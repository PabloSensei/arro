use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use base::{Cancel, Result};
use proc_sup::{ProcessClass, ProcessSpec};
use project_model::{
    BuildGroup, BuildModule, BuildSystem, BuildTask, Module, PathEntry, PathKind, ProjectModel,
    TaskOptions, Unit, cache_file,
};
use serde::Deserialize;

use crate::env::{fresh_directory, run_tool};
use crate::fingerprint::{BuildStamp, has_classes};
use crate::{BuildEnv, Jdk, find_jdks};

const INIT_SCRIPT: &str = include_str!("../../../../helpers/gradle-init/ide-model.init.gradle");
const WRAPPER_JAR: &str = "gradle/wrapper/gradle-wrapper.jar";
const WRAPPER_PROPERTIES: &str = "gradle/wrapper/gradle-wrapper.properties";
const BUILD_FILES: &[&str] = &[
    "settings.gradle",
    "settings.gradle.kts",
    "build.gradle",
    "build.gradle.kts",
];

/// Never a scan: it would publish the build to a public server. And no configuration cache:
/// the init script reads the model at execution time.
const COMMON_ARGS: &[&str] = &["--no-scan", "--no-configuration-cache", "--console=plain"];
/// The tasks the base and java plugins give every project. Tasks a build adds itself
/// would need a run of the tool to list.
const TASKS: &[&str] = &[
    "clean", "classes", "test", "assemble", "build", "check", "jar",
];
/// What `gradlew.bat` passes to the client JVM.
const CLIENT_JVM_ARGS: &[&str] = &["-Xmx64m", "-Xms64m", "-Dorg.gradle.appname=gradlew"];
/// A daemon started by a build of the window stops after ten idle minutes, so it does
/// not keep hundreds of megabytes for hours after the window is done. A property the
/// project sets stays as it is; the file of the user is never written.
const DAEMON_IDLE_TIMEOUT: &str = "org.gradle.daemon.idletimeout";
const DAEMON_IDLE_TIMEOUT_MS: u64 = 600_000;

/// Lowest Gradle that runs on a JVM, from the compatibility matrix in the Gradle manual.
const FIRST_GRADLE_ON: &[(u32, (u32, u32))] = &[
    (17, (7, 3)),
    (18, (7, 5)),
    (19, (7, 6)),
    (20, (8, 3)),
    (21, (8, 5)),
    (22, (8, 8)),
    (23, (8, 10)),
    (24, (8, 14)),
    (25, (9, 1)),
];
/// Gradle 9 and later do not start on an older JVM.
const OLDEST_JVM_FOR_GRADLE_9: u32 = 17;

/// False when this Gradle is known not to start on this JVM. A JVM newer than the table
/// counts as supported: Gradle says so itself, more exactly than a table here could.
pub fn gradle_supports(gradle: (u32, u32), jdk_feature: u32) -> bool {
    if gradle.0 >= 9 && jdk_feature < OLDEST_JVM_FOR_GRADLE_9 {
        return false;
    }
    let first = FIRST_GRADLE_ON
        .iter()
        .find(|(feature, _)| *feature == jdk_feature);
    first.is_none_or(|(_, version)| gradle >= *version)
}

/// The Gradle version the project wrapper pins, as major and minor.
pub fn wrapper_version(root: &Path) -> Option<(u32, u32)> {
    let text = fs::read_to_string(root.join(WRAPPER_PROPERTIES)).ok()?;
    let url = text
        .lines()
        .find_map(|line| line.strip_prefix("distributionUrl="))?;
    let name = url.rsplit('/').next()?;
    let version = name.strip_prefix("gradle-")?;
    let mut parts = version.split(['.', '-']);
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().and_then(|part| part.parse().ok()).unwrap_or(0);
    Some((major, minor))
}

pub struct Gradle {
    pub(crate) env: BuildEnv,
    /// The inputs of a build that started and has not reported success yet.
    stamp: BuildStamp,
}

impl Gradle {
    pub fn new(env: BuildEnv) -> Self {
        Self {
            env,
            stamp: BuildStamp::new(),
        }
    }

    /// Where the fingerprint of the last successful build waits.
    fn stamp_file(&self, root: &Path) -> PathBuf {
        cache_file(&self.env.cache_root(), root).with_file_name("build-stamp")
    }

    /// The JDK the Gradle server runs on. The project JDK when Gradle starts on it, else
    /// the closest one it does start on: compiling for the project stays the job of
    /// toolchains in the build script.
    fn tool_jdk(&self, root: &Path) -> Result<Jdk> {
        let project = self.env.project_jdk(root)?;
        let Some(version) = wrapper_version(root) else {
            return Ok(project);
        };
        if gradle_supports(version, project.feature) {
            return Ok(project);
        }
        let usable = find_jdks()
            .into_iter()
            .filter(|jdk| gradle_supports(version, jdk.feature));
        let nearest = usable.min_by_key(|jdk| jdk.feature.abs_diff(project.feature));
        nearest.ok_or_else(|| {
            let (major, minor) = version;
            let jdk = project.feature;
            format!("Gradle {major}.{minor} does not run on JDK {jdk}, and no other JDK found")
                .into()
        })
    }

    /// The real client and not `gradlew.bat`: the batch file would be the process to stop
    /// and own, with the client behind it. Works with no job around it, so that the
    /// server outlives the client.
    pub(crate) fn command(&self, root: &Path) -> Result<ProcessSpec> {
        let settings = &self.env.build_tools.second;
        let jdk = match settings.runtime_home_path() {
            Some(home) => Jdk::at(Path::new(home)).ok_or_else(|| {
                format!("no JDK of the settings at {home}, so the build tool cannot start")
            })?,
            None => self.tool_jdk(root)?,
        };
        let wrapper = root.join(WRAPPER_JAR);
        let timeout = daemon_idle_timeout(root);
        let spec = if let Some(home) = settings.home_path() {
            let program = Path::new(home).join("bin").join("gradle.bat");
            if !program.is_file() {
                return Err(format!("no gradle.bat in {}", program.display()).into());
            }
            let spec = ProcessSpec::new(program);
            match timeout {
                Some(timeout) => spec.arg(timeout),
                None => spec,
            }
        } else if wrapper.is_file() {
            ProcessSpec::new(jdk.java())
                .args(CLIENT_JVM_ARGS)
                .args(timeout)
                .arg("-classpath")
                .arg(&wrapper)
                .arg("org.gradle.wrapper.GradleWrapperMain")
        } else {
            let spec =
                ProcessSpec::new(find_in_path().ok_or("no Gradle wrapper and no gradle in PATH")?);
            match timeout {
                Some(timeout) => spec.arg(timeout),
                None => spec,
            }
        };
        let mut spec = spec
            .cwd(root)
            .class(ProcessClass::Client)
            .args(COMMON_ARGS)
            .label("Build tool");
        if settings.offline {
            spec = spec.arg("--offline");
        }
        if let Some(home) = settings.user_home_path() {
            spec = spec.env("GRADLE_USER_HOME", home);
        }
        Ok(self.env.apply(spec, &jdk))
    }
}

fn find_in_path() -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    env::split_paths(&path)
        .map(|directory| directory.join("gradle.bat"))
        .find(|file| file.is_file())
}

impl BuildSystem for Gradle {
    fn detect(&self, root: &Path) -> bool {
        BUILD_FILES.iter().any(|name| root.join(name).is_file())
    }

    fn scan(&self, root: &Path) -> Result<ProjectModel> {
        Ok(ProjectModel {
            root: root.to_owned(),
            tool: "gradle".into(),
            ..ProjectModel::default()
        })
    }

    fn sync(&self, root: &Path, cancel: &Cancel) -> Result<ProjectModel> {
        let directory = cache_file(&self.env.cache_root(), root).with_file_name("sync");
        let directory = fresh_directory(&directory)?;
        let script = directory.join("ide-model.init.gradle");
        fs::write(&script, INIT_SCRIPT)?;
        let reports = directory.join("gradle");
        let spec = self
            .command(root)?
            .arg("--init-script")
            .arg(&script)
            .arg(format!("-Pide.model.out={}", reports.display()))
            .arg("ideModel");
        run_tool(&spec, cancel)?;
        let mut files = Vec::new();
        for entry in fs::read_dir(&reports)? {
            files.push(fs::read(entry?.path())?);
        }
        parse_reports(&files, root)
    }

    fn assemble(&self, root: &Path) -> Result<ProcessSpec> {
        Ok(self.command(root)?.arg("classes"))
    }

    fn assemble_if_needed(&self, root: &Path) -> Result<Option<ProcessSpec>> {
        // The `classes` task is what a run reads, and it does nothing when its own
        // outputs are current. Skipping the client entirely saves the JVM start.
        self.stamp
            .needed(root, &self.stamp_file(root), has_classes(root), || {
                self.assemble(root)
            })
    }

    fn built(&self, root: &Path) {
        self.stamp.built(root, &self.stamp_file(root));
    }

    fn build(&self, root: &Path, tests: bool) -> Result<ProcessSpec> {
        let spec = self.command(root)?.arg("classes");
        Ok(if tests { spec.arg("testClasses") } else { spec })
    }

    fn compile(&self, root: &Path, module: Option<&Module>) -> Result<ProcessSpec> {
        let task = match module {
            // The root project's path is ":", its task is the plain one.
            Some(module) if module.id != ":" => format!("{}:classes", module.id),
            Some(_) => "classes".to_owned(),
            None => "classes".to_owned(),
        };
        Ok(self.command(root)?.arg(task))
    }

    fn tasks(&self, _model: &ProjectModel) -> Vec<String> {
        TASKS.iter().map(|task| (*task).to_owned()).collect()
    }

    fn task_tree(&self, model: &ProjectModel) -> Vec<BuildModule> {
        let mut modules: Vec<&Module> = model.modules.iter().collect();
        // The root project comes first; the rest keep the order the tool reported.
        modules.sort_by_key(|module| module.id != ":");
        modules
            .into_iter()
            .map(|module| {
                let task = |name: &str, program| BuildTask {
                    name: name.to_owned(),
                    program,
                };
                let build = ["clean", "classes", "assemble", "build", "jar"]
                    .into_iter()
                    .map(|name| task(name, false))
                    .collect();
                let verification = ["test", "check"]
                    .into_iter()
                    .map(|name| task(name, false))
                    .collect();
                let mut application = Vec::new();
                if has_application(module) {
                    application.push(task("run", true));
                }
                if has_spring_boot(module) {
                    application.push(task("bootRun", true));
                }
                BuildModule {
                    id: module.id.clone(),
                    name: module.name.clone(),
                    groups: vec![
                        BuildGroup {
                            name: "build".into(),
                            tasks: build,
                        },
                        BuildGroup {
                            name: "verification".into(),
                            tasks: verification,
                        },
                        BuildGroup {
                            name: "application".into(),
                            tasks: application,
                        },
                        BuildGroup {
                            name: "other".into(),
                            tasks: vec![task("help", false)],
                        },
                    ],
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
        _options: TaskOptions,
    ) -> Result<ProcessSpec> {
        let spec = self.command(root)?.arg(task_name(module, task));
        // The application these tasks start may read its input, as a console program does.
        Ok(match task {
            "run" | "bootRun" => spec.input(),
            _ => spec,
        })
    }

    fn goal(&self, root: &Path, line: &str, _options: TaskOptions) -> Result<ProcessSpec> {
        let mut spec = self.command(root)?;
        let mut runs_program = false;
        for word in line.split_whitespace() {
            runs_program |= matches!(word, "run" | "bootRun");
            spec = spec.arg(word);
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
        // The Java plugin's run tasks listen for the debugger with this flag.
        Ok(match task {
            "run" | "bootRun" => spec.arg("--debug-jvm"),
            _ => spec,
        })
    }
}

/// The idle timeout of the daemon this command may start, unless the project chose one
/// in `gradle.properties`. The property goes to the JVM of the client, which hands it to
/// the daemon it starts.
fn daemon_idle_timeout(root: &Path) -> Option<String> {
    let properties = fs::read_to_string(root.join("gradle.properties")).ok();
    let chosen = properties.is_some_and(|text| {
        text.lines().any(|line| {
            let line = line.split(['#', '!']).next().unwrap_or_default().trim();
            line.split_once(['=', ':'])
                .is_some_and(|(key, _)| key.trim() == DAEMON_IDLE_TIMEOUT)
        })
    });
    (!chosen).then(|| format!("-D{DAEMON_IDLE_TIMEOUT}={DAEMON_IDLE_TIMEOUT_MS}"))
}

/// The path of a task in the build: the root project's tasks have no prefix.
fn task_name(module: Option<&Module>, task: &str) -> String {
    match module {
        Some(module) if !module.id.is_empty() && module.id != ":" => {
            format!("{}:{task}", module.id.trim_end_matches(':'))
        }
        _ => task.to_owned(),
    }
}

/// Whether the module applies the application plugin, which gives it a run task.
fn has_application(module: &Module) -> bool {
    build_text(module).is_some_and(|text| text.contains("application"))
}

/// Whether the module applies the Spring Boot plugin, which gives it a run task.
fn has_spring_boot(module: &Module) -> bool {
    build_text(module).is_some_and(|text| text.contains("org.springframework.boot"))
}

fn build_text(module: &Module) -> Option<String> {
    ["build.gradle", "build.gradle.kts"]
        .into_iter()
        .find_map(|name| fs::read_to_string(module.dir.join(name)).ok())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Report {
    tool_version: Option<String>,
    project: ReportProject,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReportProject {
    path: String,
    name: String,
    project_dir: PathBuf,
    source_sets: Vec<ReportSet>,
    problems: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReportSet {
    name: String,
    all_source_roots: Vec<PathBuf>,
    resource_roots: Vec<PathBuf>,
    classes_dirs: Vec<PathBuf>,
    resources_dir: Option<PathBuf>,
    compile_classpath: Vec<ReportEntry>,
    runtime_classpath: Vec<ReportEntry>,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum ReportEntry {
    Module {
        module: String,
        path: PathBuf,
        built: bool,
    },
    Library {
        coordinates: String,
        path: PathBuf,
    },
    File {
        path: PathBuf,
    },
}

impl From<ReportEntry> for PathEntry {
    fn from(entry: ReportEntry) -> Self {
        match entry {
            ReportEntry::Module {
                module,
                path,
                built,
            } => Self {
                path,
                kind: PathKind::Module { id: module, built },
            },
            ReportEntry::Library { coordinates, path } => Self {
                path,
                kind: PathKind::Library { coordinates },
            },
            ReportEntry::File { path } => Self {
                path,
                kind: PathKind::File,
            },
        }
    }
}

impl From<ReportSet> for Unit {
    fn from(set: ReportSet) -> Self {
        let resources = set.resource_roots;
        let mut source_roots = set.all_source_roots;
        source_roots.retain(|root| !resources.contains(root));
        let mut output_dirs = set.classes_dirs;
        output_dirs.extend(set.resources_dir);
        Self {
            name: set.name,
            source_roots,
            resource_roots: resources,
            output_dirs,
            compile_paths: set.compile_classpath.into_iter().map(Into::into).collect(),
            runtime_paths: set.runtime_classpath.into_iter().map(Into::into).collect(),
        }
    }
}

/// One report per project, in no particular order.
fn parse_reports(files: &[Vec<u8>], root: &Path) -> Result<ProjectModel> {
    let mut model = ProjectModel {
        root: root.to_owned(),
        tool: "gradle".into(),
        resolved: true,
        ..ProjectModel::default()
    };
    for file in files {
        let report: Report = serde_json::from_slice(file)?;
        model.tool_version = report.tool_version;
        model.problems.extend(report.project.problems);
        model.modules.push(Module {
            id: report.project.path,
            name: report.project.name,
            dir: report.project.project_dir,
            units: report
                .project
                .source_sets
                .into_iter()
                .map(Into::into)
                .collect(),
        });
    }
    model.modules.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(model)
}

#[cfg(test)]
mod tests {
    use project_model::MAIN_UNIT;

    use super::*;
    use crate::fingerprint::{fingerprint, read_stamp};

    const APP: &str = r#"{
      "schema": 1, "tool": "gradle", "toolVersion": "9.5.1", "javaVersion": "17",
      "project": {
        "path": ":app", "name": "app", "projectDir": "C:/p/app", "buildDir": "C:/p/app/build",
        "plugins": {"java": true, "kotlin": false},
        "sourceSets": [{
          "name": "main",
          "sourceRoots": ["C:/p/app/src/main/java"],
          "allSourceRoots": ["C:/p/app/src/main/java", "C:/p/app/src/main/kotlin",
                             "C:/p/app/src/main/resources"],
          "resourceRoots": ["C:/p/app/src/main/resources"],
          "classesDirs": ["C:/p/app/build/classes/java/main"],
          "resourcesDir": "C:/p/app/build/resources/main",
          "compileClasspath": [
            {"path": "C:/p/lib/build/libs/lib.jar", "kind": "module", "module": ":lib",
             "built": false}
          ],
          "runtimeClasspath": [
            {"path": "C:/p/lib/build/libs/lib.jar", "kind": "module", "module": ":lib",
             "built": false},
            {"path": "C:/r/a.jar", "kind": "library", "coordinates": "g:a:1"},
            {"path": "C:/r/extra", "kind": "file"}
          ]
        }],
        "problems": []
      }
    }"#;
    const LIB: &str = r#"{
      "schema": 1, "tool": "gradle", "toolVersion": "9.5.1", "javaVersion": "17",
      "project": {
        "path": ":lib", "name": "lib", "projectDir": "C:/p/lib", "buildDir": "C:/p/lib/build",
        "plugins": {"java": true, "kotlin": true},
        "sourceSets": [
          {"name": "jmh", "sourceRoots": [], "allSourceRoots": [], "resourceRoots": [],
           "classesDirs": ["C:/p/lib/build/classes/java/jmh"], "resourcesDir": null,
           "compileClasspath": [], "runtimeClasspath": []},
          {"name": "main", "sourceRoots": [], "allSourceRoots": [], "resourceRoots": [],
           "classesDirs": ["C:/p/lib/build/classes/kotlin/main"], "resourcesDir": null,
           "compileClasspath": [], "runtimeClasspath": []}
        ],
        "problems": ["runtimeClasspath: broken"]
      }
    }"#;

    fn model() -> ProjectModel {
        let files = [LIB.as_bytes().to_vec(), APP.as_bytes().to_vec()];
        parse_reports(&files, Path::new("C:/p")).unwrap()
    }

    #[test]
    fn reports_of_all_projects_form_one_model() {
        let model = model();

        let ids: Vec<_> = model
            .modules
            .iter()
            .map(|module| module.id.as_str())
            .collect();
        assert_eq!(ids, [":app", ":lib"]);
        assert_eq!(model.tool_version.as_deref(), Some("9.5.1"));
        assert_eq!(model.problems, ["runtimeClasspath: broken"]);
    }

    #[test]
    fn sources_and_resources_are_told_apart() {
        let model = model();

        let main = model.module(":app").unwrap().unit(MAIN_UNIT).unwrap();

        let sources = ["C:/p/app/src/main/java", "C:/p/app/src/main/kotlin"];
        assert_eq!(main.source_roots, sources.map(PathBuf::from));
        let outputs = [
            "C:/p/app/build/classes/java/main",
            "C:/p/app/build/resources/main",
        ];
        assert_eq!(main.output_dirs, outputs.map(PathBuf::from));
    }

    /// A neighbour arrives as a jar nobody built yet, and a Kotlin neighbour keeps its
    /// classes elsewhere than the jar suggests. Its main output is what must run.
    #[test]
    fn a_neighbour_runs_from_its_main_classes() {
        let paths = model().runtime_paths(":app", MAIN_UNIT);

        let expected = [
            "C:/p/app/build/classes/java/main",
            "C:/p/app/build/resources/main",
            "C:/p/lib/build/classes/kotlin/main",
            "C:/r/a.jar",
            "C:/r/extra",
        ];
        assert_eq!(paths, expected.map(PathBuf::from));
    }

    #[test]
    fn the_table_of_jvms_holds() {
        assert!(gradle_supports((9, 5), 25));
        assert!(gradle_supports((9, 1), 25));
        assert!(!gradle_supports((9, 0), 25));
        assert!(!gradle_supports((8, 14), 25));
        assert!(gradle_supports((8, 14), 8));
        assert!(!gradle_supports((9, 5), 8));
        assert!(gradle_supports((9, 5), 99));
    }

    #[test]
    fn an_unchanged_project_after_a_successful_build_needs_no_gradle() {
        let root = std::env::temp_dir().join(format!("gradle-fast-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src/main/java/demo")).unwrap();
        fs::write(root.join("build.gradle"), "plugins { id 'java' }").unwrap();
        fs::write(root.join("src/main/java/demo/App.java"), "class App {}").unwrap();
        fs::create_dir_all(root.join("build/classes/java/main/demo")).unwrap();
        fs::write(
            root.join("build/classes/java/main/demo/App.class"),
            b"class",
        )
        .unwrap();
        let tool = Gradle::new(crate::BuildEnv {
            cache_root: Some(root.join("cache")),
            ..crate::BuildEnv::default()
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
    fn the_wrapper_version_is_read_from_its_url() {
        let root = std::env::temp_dir().join(format!("java-project-gradle-{}", std::process::id()));
        fs::create_dir_all(root.join("gradle/wrapper")).unwrap();
        let text =
            "distributionUrl=https\\://services.gradle.org/distributions/gradle-9.5.1-bin.zip\n";
        fs::write(root.join(WRAPPER_PROPERTIES), text).unwrap();

        assert_eq!(wrapper_version(&root), Some((9, 5)));
        let _ = fs::remove_dir_all(root);
    }

    fn tree_project(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "java-project-gradle-tree-{}-{name}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("app")).unwrap();
        fs::write(root.join("build.gradle"), "plugins { id 'application' }").unwrap();
        fs::write(
            root.join("app/build.gradle"),
            "plugins { id 'org.springframework.boot' version '3.5.0' }",
        )
        .unwrap();
        root
    }

    #[test]
    fn a_task_of_a_module_keeps_its_project_path() {
        let root = Module {
            id: ":".into(),
            dir: PathBuf::from("C:/p"),
            ..Module::default()
        };
        let app = Module {
            id: ":app".into(),
            dir: PathBuf::from("C:/p/app"),
            ..Module::default()
        };
        let nested = Module {
            id: ":a:b".into(),
            ..Module::default()
        };

        assert_eq!(task_name(None, "build"), "build");
        assert_eq!(task_name(Some(&root), "build"), "build");
        assert_eq!(task_name(Some(&app), "build"), ":app:build");
        assert_eq!(task_name(Some(&nested), "check"), ":a:b:check");
    }

    #[test]
    fn the_tree_groups_the_tasks_of_every_project() {
        let root = tree_project("groups");
        let model = ProjectModel {
            root: root.clone(),
            tool: "gradle".into(),
            resolved: true,
            modules: vec![
                Module {
                    id: ":app".into(),
                    name: "app".into(),
                    dir: root.join("app"),
                    ..Module::default()
                },
                Module {
                    id: ":".into(),
                    name: "root".into(),
                    dir: root.clone(),
                    ..Module::default()
                },
            ],
            ..ProjectModel::default()
        };

        let tree = Gradle::new(crate::BuildEnv::default()).task_tree(&model);

        let names: Vec<_> = tree.iter().map(|module| module.name.as_str()).collect();
        assert_eq!(names, ["root", "app"], "the root project comes first");
        let groups: Vec<_> = tree[0]
            .groups
            .iter()
            .map(|group| group.name.as_str())
            .collect();
        assert_eq!(groups, ["build", "verification", "application", "other"]);
        let build: Vec<_> = tree[0].groups[0]
            .tasks
            .iter()
            .map(|task| task.name.as_str())
            .collect();
        assert!(build.contains(&"clean") && build.contains(&"build"));
        let root_app = tree[0]
            .groups
            .iter()
            .find(|group| group.name == "application")
            .unwrap();
        assert!(root_app.tasks.iter().any(|task| task.name == "run"));
        assert!(!root_app.tasks.iter().any(|task| task.name == "bootRun"));
        let app_app = tree[1]
            .groups
            .iter()
            .find(|group| group.name == "application")
            .unwrap();
        assert!(app_app.tasks.iter().any(|task| task.name == "bootRun"));
        assert!(
            app_app
                .tasks
                .iter()
                .find(|task| task.name == "bootRun")
                .is_some_and(|task| task.program)
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_build_shortens_the_daemon_idle_timeout_unless_the_project_sets_one() {
        let root = std::env::temp_dir().join(format!("gradle-daemon-idle-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();

        assert_eq!(
            daemon_idle_timeout(&root).as_deref(),
            Some("-Dorg.gradle.daemon.idletimeout=600000")
        );

        fs::write(
            root.join("gradle.properties"),
            "org.gradle.daemon.idletimeout=3600000\n",
        )
        .unwrap();
        assert_eq!(
            daemon_idle_timeout(&root),
            None,
            "the project keeps its value"
        );

        fs::write(
            root.join("gradle.properties"),
            "# org.gradle.daemon.idletimeout=3600000\n",
        )
        .unwrap();
        assert!(
            daemon_idle_timeout(&root).is_some(),
            "a commented property is not a choice"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn without_the_plugin_there_is_no_run_task_for_the_boot() {
        let root =
            std::env::temp_dir().join(format!("java-project-gradle-plain-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("build.gradle"), "plugins { id 'java' }").unwrap();
        let model = ProjectModel {
            root: root.clone(),
            tool: "gradle".into(),
            resolved: true,
            modules: vec![Module {
                id: ":".into(),
                name: "root".into(),
                dir: root.clone(),
                ..Module::default()
            }],
            ..ProjectModel::default()
        };

        let tree = Gradle::new(crate::BuildEnv::default()).task_tree(&model);

        let application = tree[0]
            .groups
            .iter()
            .find(|group| group.name == "application")
            .unwrap();
        assert!(application.tasks.is_empty());
        let _ = fs::remove_dir_all(root);
    }
}
