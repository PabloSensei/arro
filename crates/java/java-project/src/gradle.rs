use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use base::{Cancel, Result};
use proc_sup::{ProcessClass, ProcessSpec};
use project_model::{
    BuildGroup, BuildModule, BuildSelection, BuildSystem, BuildTask, Exclusion, FileEdit, Module,
    ModuleDependencies, PathEntry, PathKind, ProjectModel, RecordedTask, TaskOptions, ToolHome,
    ToolProgress, ToolSetup, Unit, cache_file,
};
use serde::Deserialize;

use crate::dependencies::{ReportTrees, exclude_in_script};
use crate::env::{fresh_directory, run_tool};
use crate::fingerprint::{BuildStamp, has_classes};
use crate::gradle_download;
use crate::tool_download;
use crate::tool_home::{self, Tool};
use crate::{BuildEnv, Jdk, find_jdks};

pub use crate::tool_home::{
    MissingTool as MissingGradle, ToolLocation as GradleHome, ToolOrigin as GradleOrigin,
};

const INIT_SCRIPT: &str = include_str!("../../../../helpers/gradle-init/ide-model.init.gradle");
/// The init script of a profile run: it adds the recording flag to the JVMs of the task.
const PROFILE_SCRIPT: &str =
    include_str!("../../../../helpers/gradle-init/ide-profile.init.gradle");
const WRAPPER_JAR: &str = "gradle/wrapper/gradle-wrapper.jar";
const WRAPPER_PROPERTIES: &str = "gradle/wrapper/gradle-wrapper.properties";
const BUILD_FILES: &[&str] = &[
    "settings.gradle",
    "settings.gradle.kts",
    "build.gradle",
    "build.gradle.kts",
];

/// The folder of the downloaded copies under the data folder of the IDE.
const GRADLE: &str = "gradle";

#[cfg(windows)]
const WRAPPERS: &[&str] = &["gradlew.bat", "gradlew.cmd"];
#[cfg(not(windows))]
const WRAPPERS: &[&str] = &["gradlew"];
#[cfg(windows)]
const PROGRAMS: &[&str] = &["gradle.bat", "gradle.cmd"];
#[cfg(not(windows))]
const PROGRAMS: &[&str] = &["gradle"];

/// The names of the scripts and the folder of the downloaded copies of Gradle.
const TOOL: Tool = Tool {
    folder: GRADLE,
    label: "Gradle",
    wrappers: WRAPPERS,
    programs: PROGRAMS,
};

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
    version_in_name(name)
}

/// The version a distribution name carries: `gradle-9.5.1-bin.zip` gives `(9, 5)`.
fn version_in_name(name: &str) -> Option<(u32, u32)> {
    let version = name.strip_prefix("gradle-").unwrap_or(name);
    let mut parts = version.split(['.', '-']);
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().and_then(|part| part.parse().ok()).unwrap_or(0);
    Some((major, minor))
}

/// The version of a Gradle the search found, when its name or the wrapper says it. The
/// copy on `PATH` may be anything, so it is not guessed.
fn version_of(gradle: &GradleHome, root: &Path) -> Option<(u32, u32)> {
    match gradle.origin {
        GradleOrigin::Wrapper => wrapper_version(root),
        GradleOrigin::Configured | GradleOrigin::Bundled => {
            version_in_name(gradle.home.file_name()?.to_str()?)
        }
        GradleOrigin::Path => None,
    }
}

pub struct Gradle {
    pub(crate) env: BuildEnv,
    /// The inputs of a build that started and has not reported success yet.
    stamp: BuildStamp,
    /// The home the user chose in this session, which wins over the settings file.
    chosen: Mutex<Option<PathBuf>>,
}

impl Gradle {
    pub fn new(env: BuildEnv) -> Self {
        Self {
            env,
            stamp: BuildStamp::new(),
            chosen: Mutex::new(None),
        }
    }

    /// Where the fingerprint of the last successful build waits.
    fn stamp_file(&self, root: &Path) -> PathBuf {
        cache_file(&self.env.cache_root(), root).with_file_name("build-stamp")
    }

    /// The home of the Gradle of this project, in the order the settings name.
    pub fn resolve(&self, root: &Path) -> std::result::Result<GradleHome, MissingGradle> {
        let settings = &self.env.build_tools.second;
        let chosen = self
            .chosen
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone();
        let (kind, configured) = match &chosen {
            Some(home) => (ToolHome::Custom, Some(home.as_path())),
            None => (settings.home_kind, settings.home_path().map(Path::new)),
        };
        tool_home::find(
            &TOOL,
            root,
            configured,
            &self.env.data_dir(),
            env::var_os("PATH").as_deref(),
            kind,
        )
    }

    /// The JDK the Gradle server runs on. The project JDK when Gradle starts on it, else
    /// the closest one it does start on: compiling for the project stays the job of
    /// toolchains in the build script.
    ///
    /// A copy the IDE downloaded is not replaced behind the back of the user: when the
    /// project JDK does not run it, the failure says so and the window offers the choice.
    fn tool_jdk(&self, root: &Path, gradle: &GradleHome) -> Result<Jdk> {
        let project = self.env.project_jdk(root)?;
        let Some(version) = version_of(gradle, root) else {
            return Ok(project);
        };
        if gradle_supports(version, project.feature) {
            return Ok(project);
        }
        if gradle.origin != GradleOrigin::Bundled {
            let usable = find_jdks()
                .into_iter()
                .filter(|jdk| gradle_supports(version, jdk.feature));
            let nearest = usable.min_by_key(|jdk| jdk.feature.abs_diff(project.feature));
            if let Some(nearest) = nearest {
                return Ok(nearest);
            }
        }
        let (major, minor) = version;
        let jdk = project.feature;
        Err(format!(
            "Gradle {major}.{minor} does not run on JDK {jdk}; choose a Gradle distribution \
             that does, or another JVM for Gradle"
        )
        .into())
    }

    /// The real client and not the wrapper script: the script would be the process to
    /// stop and own, with the client behind it. Works with no job around it, so that the
    /// server outlives the client.
    pub(crate) fn command(&self, root: &Path) -> Result<ProcessSpec> {
        let settings = &self.env.build_tools.second;
        let gradle = self.resolve(root)?;
        let jdk = match settings.runtime_home_path() {
            Some(home) => Jdk::at(Path::new(home)).ok_or_else(|| {
                format!("no JDK of the settings at {home}, so the build tool cannot start")
            })?,
            None => self.tool_jdk(root, &gradle)?,
        };
        let timeout = daemon_idle_timeout(root);
        // A wrapper that came without the execute bit, from an archive made on Windows or
        // from a copy, is made runnable here and the output of the build says so. The client
        // of a wrapper starts from its jar, so a script whose bits cannot be changed only
        // brings a line; a program the build starts itself has to be runnable.
        let note = match gradle.origin {
            GradleOrigin::Wrapper => match crate::wrapper::prepare(&gradle.program) {
                Ok(note) => note,
                Err(error) => Some(format!(
                    "{error}; Gradle starts from the wrapper jar without the script"
                )),
            },
            _ => crate::wrapper::prepare(&gradle.program)?,
        };
        let spec = match gradle.origin {
            GradleOrigin::Wrapper => {
                let wrapper = root.join(WRAPPER_JAR);
                if !wrapper.is_file() {
                    return Err(format!(
                        "no {} next to {}, so the wrapper cannot start",
                        WRAPPER_JAR,
                        gradle.program.display()
                    )
                    .into());
                }
                ProcessSpec::new(jdk.java())
                    .args(CLIENT_JVM_ARGS)
                    .args(timeout)
                    .arg("-classpath")
                    .arg(&wrapper)
                    .arg("org.gradle.wrapper.GradleWrapperMain")
            }
            _ => match timeout {
                Some(timeout) => ProcessSpec::new(&gradle.program).arg(timeout),
                None => ProcessSpec::new(&gradle.program),
            },
        };
        let mut spec = spec
            .cwd(root)
            .class(ProcessClass::Client)
            .args(COMMON_ARGS)
            .label("Build tool");
        if let Some(note) = note {
            spec = spec.note(note);
        }
        if settings.offline {
            spec = spec.arg("--offline");
        }
        if let Some(home) = settings.user_home_path() {
            spec = spec.env("GRADLE_USER_HOME", home);
        }
        // Every command of every window and provider goes through here, so the chosen
        // values reach a build, a task, the tests and the import alike.
        let selection = crate::build_selection::load_selection(root);
        Ok(self
            .env
            .apply(spec, &jdk)
            .args(crate::build_selection::property_args(&selection)))
    }
}

/// The Gradle of this machine, `None` when only a wrapper can start it.
pub fn find_gradle() -> Option<PathBuf> {
    tool_home::on_path(env::var_os("PATH").as_deref(), &TOOL).map(|found| found.program)
}

/// The order of the automatic search: the wrapper of the project, the home of the
/// settings, the copy the IDE downloaded, and then `PATH`. The other kinds name one
/// source only. `path` is the search path; `None` means the machine has no `PATH`.
pub fn find_gradle_home(
    root: &Path,
    configured: Option<&Path>,
    data_dir: &Path,
    path: Option<&OsStr>,
    kind: ToolHome,
) -> std::result::Result<GradleHome, MissingGradle> {
    tool_home::find(&TOOL, root, configured, data_dir, path, kind)
}

/// The newest copy of Gradle the IDE downloaded, `<data>/tools/gradle/<version>`, when it
/// carries the program.
pub fn bundled_home(data_dir: &Path) -> Option<PathBuf> {
    tool_download::newest_home(data_dir, GRADLE, PROGRAMS)
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

    fn missing_tool(&self, root: &Path) -> Option<ToolSetup> {
        match self.resolve(root) {
            Ok(gradle) => {
                // The tool is there, so the failure may be the JVM that runs it.
                let jdk = self.env.project_jdk(root).ok()?;
                let version = version_of(&gradle, root)?;
                if gradle_supports(version, jdk.feature) {
                    return None;
                }
                let (major, minor) = version;
                Some(ToolSetup {
                    name: "Gradle".into(),
                    title: "Gradle does not run on this JDK".into(),
                    text: format!(
                        "Gradle {major}.{minor} does not run on JDK {}. Choose a Gradle \
                         distribution for this JDK: another version is not downloaded \
                         automatically.",
                        jdk.feature
                    ),
                    download: (gradle.origin != GradleOrigin::Bundled)
                        .then(|| "Download Gradle".to_owned()),
                    choose: Some("Choose Gradle home\u{2026}".into()),
                    settings: Some((
                        "Build Tools \u{2192} Gradle".into(),
                        "build_tool_second_home",
                    )),
                })
            }
            Err(missing) => Some(ToolSetup {
                name: "Gradle".into(),
                title: "Gradle not found".into(),
                text: format!(
                    "{missing}. Download Gradle from services.gradle.org or choose a Gradle \
                     home folder."
                ),
                download: Some("Download Gradle".into()),
                choose: Some("Choose Gradle home\u{2026}".into()),
                settings: Some((
                    "Build Tools \u{2192} Gradle".into(),
                    "build_tool_second_home",
                )),
            }),
        }
    }

    fn install_tool(&self, progress: &ToolProgress, cancel: &Cancel) -> Result<Option<PathBuf>> {
        let home = gradle_download::download(
            gradle_download::REPOSITORY,
            &self.env.data_dir(),
            None,
            progress,
            cancel,
        )?;
        Ok(Some(home))
    }

    fn use_home(&self, home: &Path) {
        *self
            .chosen
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = Some(home.to_path_buf());
    }

    fn tool_bin(&self) -> Option<PathBuf> {
        bundled_home(&self.env.data_dir()).map(|home| home.join("bin"))
    }

    fn terminal_path(&self) -> bool {
        self.env.build_tools.second.terminal_path
    }

    fn takes_properties(&self) -> bool {
        true
    }

    fn selection(&self, root: &Path) -> BuildSelection {
        crate::build_selection::load_selection(root)
    }

    fn choose(&self, root: &Path, selection: &BuildSelection) -> Result {
        crate::build_selection::save_selection(root, selection)
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

    fn rebuild(&self, root: &Path, tests: bool) -> Result<ProcessSpec> {
        let spec = self.command(root)?.arg("clean").arg("classes");
        Ok(if tests { spec.arg("testClasses") } else { spec })
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
                if has_quarkus(module) {
                    application.push(task("quarkusDev", true));
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
            "run" | "bootRun" | "quarkusDev" => spec.input(),
            _ => spec,
        })
    }

    fn goal(&self, root: &Path, line: &str, _options: TaskOptions) -> Result<ProcessSpec> {
        let mut spec = self.command(root)?;
        let mut runs_program = false;
        for word in line.split_whitespace() {
            runs_program |= matches!(word, "run" | "bootRun" | "quarkusDev");
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

    fn exclude(
        &self,
        _root: &Path,
        _module: &Module,
        build_file: &Path,
        request: &Exclusion,
        text: &str,
    ) -> Result<Option<FileEdit>> {
        Ok(exclude_in_script(text, request, build_file))
    }

    fn supports_profiler(&self) -> bool {
        true
    }

    /// The init script adds the flag to the JVMs of the task: the `JavaExec` of `run` and
    /// `bootRun`, the forks of `test`.
    fn profile_task_in(
        &self,
        root: &Path,
        module: Option<&Module>,
        task: &str,
        _options: TaskOptions,
    ) -> Result<Option<RecordedTask>> {
        if !matches!(task, "run" | "bootRun" | "test") {
            return Ok(None);
        }
        let recording = crate::profile::recording_file(&self.env.cache_root(), task)?;
        let flag = crate::profile::recording_flag(&recording);
        let directory = cache_file(&self.env.cache_root(), root).with_file_name("profile");
        let directory = fresh_directory(&directory)?;
        let script = directory.join("ide-profile.init.gradle");
        fs::write(&script, PROFILE_SCRIPT)?;
        let spec = self
            .command(root)?
            .arg("--init-script")
            .arg(&script)
            .arg(format!("-Pide.profile.flag={flag}"))
            .arg(task_name(module, task));
        let spec = match task {
            "run" | "bootRun" => spec.input(),
            _ => spec,
        };
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

/// Whether the module applies the Quarkus plugin, which gives it a dev task.
fn has_quarkus(module: &Module) -> bool {
    build_text(module).is_some_and(|text| text.contains("io.quarkus"))
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
    #[serde(default)]
    build_dir: Option<PathBuf>,
    #[serde(default)]
    build_file: Option<PathBuf>,
    source_sets: Vec<ReportSet>,
    #[serde(default)]
    dependencies: ReportTrees,
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
        model.dependencies.push(ModuleDependencies {
            module: report.project.path.clone(),
            build_file: report.project.build_file.clone(),
            scopes: report.project.dependencies.into_scopes(),
        });
        let units: Vec<Unit> = report
            .project
            .source_sets
            .into_iter()
            .map(Into::into)
            .collect();
        // The build folder holds what the compiler writes, generated sources included.
        let mut generated_roots: Vec<PathBuf> = units
            .iter()
            .flat_map(|unit| unit.source_roots.iter())
            .filter(|source| {
                report
                    .project
                    .build_dir
                    .as_ref()
                    .is_some_and(|dir| source.starts_with(dir))
            })
            .cloned()
            .collect();
        generated_roots.dedup();
        let excluded_roots: Vec<PathBuf> = report.project.build_dir.into_iter().collect();
        model.modules.push(Module {
            id: report.project.path,
            name: report.project.name,
            dir: report.project.project_dir,
            build_file: report.project.build_file,
            generated_roots,
            excluded_roots,
            units,
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
        "path": ":app", "name": "app", "projectDir": "C:/p/app", "buildFile": "C:/p/app/build.gradle",
        "buildDir": "C:/p/app/build",
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
        "dependencies": {
          "compile": [{
            "coordinates": "g:starter:1", "scope": "compileClasspath",
            "children": [
              {"coordinates": "g:core:2", "scope": "compileClasspath",
               "requested": "g:core:1"},
              {"coordinates": "g:old:1", "scope": "compileClasspath", "module": ":lib"}
            ]
          }],
          "runtime": [{
            "coordinates": "g:starter:1", "scope": "runtimeClasspath"
          }]
        },
        "problems": []
      }
    }"#;
    const LIB: &str = r#"{
      "schema": 1, "tool": "gradle", "toolVersion": "9.5.1", "javaVersion": "17",
      "project": {
        "path": ":lib", "name": "lib", "projectDir": "C:/p/lib", "buildFile": "C:/p/lib/build.gradle.kts",
        "buildDir": "C:/p/lib/build",
        "plugins": {"java": true, "kotlin": true},
        "sourceSets": [
          {"name": "jmh", "sourceRoots": [], "allSourceRoots": [], "resourceRoots": [],
           "classesDirs": ["C:/p/lib/build/classes/java/jmh"], "resourcesDir": null,
           "compileClasspath": [], "runtimeClasspath": []},
          {"name": "main", "sourceRoots": [], "allSourceRoots": [], "resourceRoots": [],
           "classesDirs": ["C:/p/lib/build/classes/kotlin/main"], "resourcesDir": null,
           "compileClasspath": [], "runtimeClasspath": []}
        ],
        "dependencies": {},
        "problems": ["runtimeClasspath: broken"]
      }
    }"#;

    fn model() -> ProjectModel {
        let files = [LIB.as_bytes().to_vec(), APP.as_bytes().to_vec()];
        parse_reports(&files, Path::new("C:/p")).unwrap()
    }

    #[test]
    fn the_profile_init_script_adds_the_flag_to_the_task_jvms() {
        assert!(PROFILE_SCRIPT.contains("ide.profile.flag"));
        assert!(PROFILE_SCRIPT.contains("JavaExec"));
        assert!(PROFILE_SCRIPT.contains("tasks.withType(Test)"));
        assert!(PROFILE_SCRIPT.contains("jvmArgs flag"));
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

    #[test]
    fn a_report_keeps_the_dependency_trees_and_the_build_file() {
        let model = model();

        let app = model.dependencies_of(":app").unwrap();
        assert_eq!(
            app.build_file.as_deref(),
            Some(Path::new("C:/p/app/build.gradle"))
        );
        let scopes: Vec<_> = app.scopes.iter().map(|scope| scope.name.as_str()).collect();
        assert_eq!(scopes, ["compile", "runtime"], "an empty scope is left out");
        let compile = app.scope("compile").unwrap();
        assert_eq!(compile.roots[0].children.len(), 2);
        assert_eq!(
            compile.roots[0].children[0].requested_version().as_deref(),
            Some("1"),
            "the version asked for stays next to the chosen one"
        );
        assert_eq!(
            compile.roots[0].children[1].module.as_deref(),
            Some(":lib"),
            "a neighbour is a module, not a library"
        );
        assert!(
            model
                .dependencies_of(":lib")
                .is_none_or(|found| found.scopes.is_empty()),
            "a report without dependencies adds no scope"
        );
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
        let root = test_support::scratch("gradle-fast");
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
    }

    #[test]
    fn the_wrapper_version_is_read_from_its_url() {
        let root = test_support::scratch("project-gradle");
        fs::create_dir_all(root.join("gradle/wrapper")).unwrap();
        let text =
            "distributionUrl=https\\://services.gradle.org/distributions/gradle-9.5.1-bin.zip\n";
        fs::write(root.join(WRAPPER_PROPERTIES), text).unwrap();

        assert_eq!(wrapper_version(&root), Some((9, 5)));
    }

    fn program() -> &'static str {
        if cfg!(windows) {
            "gradle.bat"
        } else {
            "gradle"
        }
    }

    fn wrapper() -> &'static str {
        if cfg!(windows) {
            "gradlew.bat"
        } else {
            "gradlew"
        }
    }

    fn write_program(home: &Path) {
        let bin = home.join("bin");
        fs::create_dir_all(&bin).unwrap();
        fs::write(bin.join(program()), b"@echo off\n").unwrap();
    }

    /// A program on `PATH`: a folder whose own file the search finds.
    fn write_program_file(directory: &Path) {
        fs::create_dir_all(directory).unwrap();
        fs::write(directory.join(program()), b"@echo off\n").unwrap();
    }

    fn search_path(directory: &Path) -> std::ffi::OsString {
        env::join_paths([directory]).unwrap()
    }

    #[test]
    fn the_wrapper_of_the_project_comes_before_every_other_home() {
        let root = test_support::TempDir::new("gradle-order-wrapper");
        let data = test_support::TempDir::new("gradle-order-data");
        let path = test_support::TempDir::new("gradle-order-path");
        fs::write(root.path().join(wrapper()), b"@echo off\n").unwrap();
        let configured = root.path().join("configured");
        write_program(&configured);
        write_program(&data.path().join("tools/gradle/9.1.0"));
        write_program_file(path.path());

        let found = find_gradle_home(
            root.path(),
            Some(&configured),
            data.path(),
            Some(&search_path(path.path())),
            ToolHome::Auto,
        )
        .expect("a wrapper is there");

        assert_eq!(found.origin, GradleOrigin::Wrapper);
        assert_eq!(found.program, root.path().join(wrapper()));
    }

    #[test]
    fn the_configured_home_comes_next_then_the_downloaded_copy_then_path() {
        let root = test_support::TempDir::new("gradle-order-configured");
        let data = test_support::TempDir::new("gradle-order-bundled");
        let path = test_support::TempDir::new("gradle-order-path-only");
        let configured = root.path().join("configured");
        let bundled = data.path().join("tools/gradle/9.1.0");
        write_program(&configured);
        write_program(&bundled);
        write_program_file(path.path());
        let search = search_path(path.path());

        let found = find_gradle_home(
            root.path(),
            Some(&configured),
            data.path(),
            Some(&search),
            ToolHome::Auto,
        )
        .unwrap();
        assert_eq!(found.origin, GradleOrigin::Configured);
        assert_eq!(found.home, configured);

        fs::remove_dir_all(&configured).unwrap();
        let found = find_gradle_home(
            root.path(),
            Some(&configured),
            data.path(),
            Some(&search),
            ToolHome::Auto,
        )
        .unwrap();
        assert_eq!(found.origin, GradleOrigin::Bundled);
        assert_eq!(found.home, bundled, "the downloaded copy comes next");

        fs::remove_dir_all(data.path().join("tools")).unwrap();
        let found = find_gradle_home(
            root.path(),
            Some(&configured),
            data.path(),
            Some(&search),
            ToolHome::Auto,
        )
        .unwrap();
        assert_eq!(found.origin, GradleOrigin::Path);
        assert_eq!(found.program, path.path().join(program()));
    }

    #[test]
    fn a_missing_gradle_offers_a_download_and_the_second_settings_page() {
        let root = test_support::TempDir::new("gradle-missing");
        let data = test_support::TempDir::new("gradle-missing-data");
        let mut env = BuildEnv {
            data_dir: Some(data.path().to_path_buf()),
            ..BuildEnv::default()
        };
        env.build_tools.second.home_kind = ToolHome::Custom;
        let tool = Gradle::new(env);

        let setup = tool
            .missing_tool(root.path())
            .expect("no home is chosen, so Gradle is missing");

        assert_eq!(setup.name, "Gradle");
        assert_eq!(setup.title, "Gradle not found");
        assert_eq!(setup.download.as_deref(), Some("Download Gradle"));
        assert_eq!(setup.choose.as_deref(), Some("Choose Gradle home\u{2026}"));
        assert_eq!(
            setup.settings.as_ref().map(|(_, id)| *id),
            Some("build_tool_second_home")
        );
        assert!(
            setup.text.contains("no Gradle home is chosen"),
            "{}",
            setup.text
        );
    }

    #[test]
    fn a_downloaded_gradle_that_does_not_run_on_the_project_jdk_is_named() {
        let root = test_support::TempDir::new("gradle-jdk-project");
        let data = test_support::TempDir::new("gradle-jdk-data");
        write_program(&data.path().join("tools/gradle/9.0.0"));
        let env = BuildEnv {
            data_dir: Some(data.path().to_path_buf()),
            jdk: Some(Jdk {
                home: root.path().join("jdk"),
                version: "25.0.3".to_owned(),
                feature: 25,
            }),
            ..BuildEnv::default()
        };
        let tool = Gradle::new(env);

        let setup = tool
            .missing_tool(root.path())
            .expect("the downloaded Gradle does not start on JDK 25");

        assert_eq!(setup.title, "Gradle does not run on this JDK");
        assert!(setup.text.contains("Gradle 9.0"), "{}", setup.text);
        assert!(setup.text.contains("JDK 25"), "{}", setup.text);
        assert!(
            setup.download.is_none(),
            "another version is not downloaded silently"
        );
        assert_eq!(
            setup.settings.as_ref().map(|(_, id)| *id),
            Some("build_tool_second_home")
        );
    }

    fn tree_project(name: &str) -> PathBuf {
        let root = test_support::scratch(&format!("project-gradle-tree-{name}"));
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
    }

    #[test]
    fn a_quarkus_module_gets_its_dev_task() {
        let root = tree_project("quarkus");
        fs::write(
            root.join("app/build.gradle"),
            "plugins { id 'io.quarkus' version '3.9.0' }",
        )
        .unwrap();
        let model = ProjectModel {
            root: root.clone(),
            tool: "gradle".into(),
            resolved: true,
            modules: vec![
                Module {
                    id: ":".into(),
                    name: "root".into(),
                    dir: root.clone(),
                    ..Module::default()
                },
                Module {
                    id: ":app".into(),
                    name: "app".into(),
                    dir: root.join("app"),
                    ..Module::default()
                },
            ],
            ..ProjectModel::default()
        };

        let tree = Gradle::new(crate::BuildEnv::default()).task_tree(&model);

        let app = tree.iter().find(|module| module.name == "app").unwrap();
        let application = app
            .groups
            .iter()
            .find(|group| group.name == "application")
            .unwrap();
        assert!(
            application
                .tasks
                .iter()
                .any(|task| task.name == "quarkusDev" && task.program)
        );
        let root = tree.iter().find(|module| module.name == "root").unwrap();
        assert!(
            !root
                .groups
                .iter()
                .flat_map(|group| group.tasks.iter())
                .any(|task| task.name == "quarkusDev")
        );
    }

    #[test]
    fn a_build_shortens_the_daemon_idle_timeout_unless_the_project_sets_one() {
        let root = test_support::scratch("gradle-daemon-idle");
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
    }

    #[test]
    fn without_the_plugin_there_is_no_run_task_for_the_boot() {
        let root = test_support::scratch("project-gradle-plain");
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
    }
}
