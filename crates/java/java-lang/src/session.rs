//! A project as the window holds it: jdtls on a runtime of its own, so nothing in the
//! window waits on it, and a line of text for the status bar.

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use java_project::Jdk;
use jdtls_adapter::{ImportFailure, JdtlsConfig, Runtime};
use sidecar::{State, Status, Trust};
use tokio::runtime;
use tokio::sync::watch;

use crate::setup::Toolchain;
use crate::{JavaProject, OpenError};

/// Where jdtls and its JDK live until the download with consent arrives.
pub const JDTLS_HOME: &str = "IDE_JDTLS_HOME";
pub const DEBUG_PLUGIN: &str = "IDE_JAVA_DEBUG_PLUGIN";
pub const JAVA_HOME: &str = "IDE_JAVA_HOME";
pub const MAVEN_SETTINGS: &str = "IDE_MAVEN_SETTINGS";
pub const OFFLINE: &str = "IDE_OFFLINE";

/// The settings for `root` from the environment, or `None` when one of the three
/// variables is not set.
pub fn config_from_env(root: &Path, state_dir: &Path) -> Option<JdtlsConfig> {
    let var = |name| std::env::var_os(name).map(PathBuf::from);
    let toolchain = Toolchain {
        java_home: var(JAVA_HOME)?,
        jdtls_home: var(JDTLS_HOME)?,
        debug_plugin: var(DEBUG_PLUGIN)?,
    };
    Some(config_for(
        &toolchain,
        root,
        state_dir,
        &project_model::BuildTools::default(),
    ))
}

/// The settings for `root` from a toolchain that the search already found, which is
/// what a start without `run-arro.cmd` uses.
pub fn config_of(toolchain: &Toolchain, root: &Path, state_dir: &Path) -> JdtlsConfig {
    config_of_with(
        toolchain,
        root,
        state_dir,
        &project_model::BuildTools::default(),
    )
}

/// The settings for `root`, with what the Settings pages say about the build tools.
pub fn config_of_with(
    toolchain: &Toolchain,
    root: &Path,
    state_dir: &Path,
    build_tools: &project_model::BuildTools,
) -> JdtlsConfig {
    config_for(toolchain, root, state_dir, build_tools)
}

fn config_for(
    toolchain: &Toolchain,
    root: &Path,
    state_dir: &Path,
    build_tools: &project_model::BuildTools,
) -> JdtlsConfig {
    let var = |name| std::env::var_os(name).map(PathBuf::from);
    // The project compiles on its own SDK; the one that runs the server is a separate
    // thing and may be newer. Without a project SDK the server's own one stands in.
    let environment = java_project::resolve_environment(root);
    let runtimes = runtimes_for(&environment, &toolchain.java_home);
    // Gradle runs on the project SDK only when the user chose one: Gradle 9 itself
    // needs 17 or newer, and an old project may ask for 8.
    let gradle_java_home = build_tools
        .second
        .runtime_home_path()
        .map(PathBuf::from)
        .or_else(|| {
            (environment.reason == project_model::RuntimeReason::User)
                .then(|| environment.selected.as_ref().map(|jdk| jdk.home.clone()))
                .flatten()
        });
    let maven_settings = build_tools
        .first
        .settings_path()
        .map(PathBuf::from)
        .or_else(|| var(MAVEN_SETTINGS));
    // The offline flag of the page of the tool that imports this project. Maven wins
    // when a project carries both builds, as `detect_build_system` does.
    let gradle_project = [
        "build.gradle",
        "build.gradle.kts",
        "settings.gradle",
        "settings.gradle.kts",
    ]
    .iter()
    .any(|name| root.join(name).is_file());
    let offline = if gradle_project {
        build_tools.second.offline
    } else {
        build_tools.first.offline
    };
    JdtlsConfig {
        jdtls_home: toolchain.jdtls_home.clone(),
        debug_plugin: toolchain.debug_plugin.clone(),
        java_home: toolchain.java_home.clone(),
        project_root: root.to_path_buf(),
        state_dir: project_state_dir(state_dir, root),
        runtimes,
        maven_settings,
        gradle_home: build_tools.second.home_path().map(PathBuf::from),
        gradle_user_home: build_tools.second.user_home_path().map(PathBuf::from),
        gradle_java_home,
        offline: std::env::var_os(OFFLINE).is_some() || offline,
        environment: HashMap::new(),
    }
}

/// One folder per project under the IDE state, so two projects never share an index.
/// File > Invalidate Caches deletes it for the project.
pub fn project_state_dir(state_dir: &Path, root: &Path) -> PathBuf {
    let mut hasher = DefaultHasher::new();
    root.to_string_lossy().to_lowercase().hash(&mut hasher);
    let name = root
        .file_name()
        .map(|name| name.to_string_lossy().into_owned());
    let name = name.unwrap_or_default();
    state_dir
        .join("jdtls")
        .join(format!("{name}-{:016x}", hasher.finish()))
}

/// `JavaSE-<major>` from the `release` file of a JDK.
fn runtime_of(home: &Path) -> Option<Runtime> {
    let release = fs::read_to_string(home.join("release")).ok()?;
    let version = release
        .lines()
        .find_map(|line| line.strip_prefix("JAVA_VERSION="))?
        .trim_matches('"');
    let major = version.split('.').next()?;
    Some(Runtime {
        name: format!("JavaSE-{major}"),
        home: home.to_path_buf(),
        default: true,
    })
}

/// The execution environment name jdtls knows for a feature version.
fn environment_name(feature: u32) -> String {
    if feature <= 8 {
        format!("JavaSE-1.{feature}")
    } else {
        format!("JavaSE-{feature}")
    }
}

/// `java.configuration.runtimes`: every known SDK, one per feature version, so that a
/// module asking for another release still finds one. The project SDK is the default
/// and wins its version; the SDK that runs the server stands in when nothing is found.
pub(crate) fn runtimes_for(
    environment: &java_project::ResolvedEnvironment,
    server_home: &Path,
) -> Vec<Runtime> {
    let mut jdks: Vec<Jdk> = environment.selected.iter().cloned().collect();
    jdks.extend(environment.jdks());
    let mut runtimes: Vec<Runtime> = Vec::new();
    for jdk in &jdks {
        let name = environment_name(jdk.feature);
        if runtimes.iter().any(|runtime| runtime.name == name) {
            continue;
        }
        runtimes.push(Runtime {
            name,
            home: jdk.home.clone(),
            default: environment
                .selected
                .as_ref()
                .is_some_and(|selected| selected.home == jdk.home),
        });
    }
    if let Some(mut server) = runtime_of(server_home)
        && !runtimes.iter().any(|runtime| runtime.home == server.home)
    {
        if let Some(feature) = Jdk::at(server_home).map(|jdk| jdk.feature) {
            server.name = environment_name(feature);
        }
        if !runtimes.iter().any(|runtime| runtime.name == server.name) {
            server.default = runtimes.iter().all(|runtime| !runtime.default);
            runtimes.push(server);
        }
    }
    if runtimes.iter().all(|runtime| !runtime.default)
        && let Some(first) = runtimes.first_mut()
    {
        first.default = true;
    }
    runtimes
}

#[cfg(test)]
mod runtime_tests {
    use super::*;

    fn jdk(feature: u32, home: &str) -> Jdk {
        Jdk {
            home: PathBuf::from(home),
            version: feature.to_string(),
            feature,
        }
    }

    #[test]
    fn every_version_is_offered_and_the_project_sdk_is_the_default() {
        let environment = java_project::ResolvedEnvironment {
            runtimes: vec![
                (jdk(17, "C:/jdk17"), false),
                (jdk(21, "C:/jdk21"), false),
                (jdk(8, "C:/jdk8"), true),
            ],
            selected: Some(jdk(17, "C:/jdk17")),
            ..Default::default()
        };
        let runtimes = runtimes_for(&environment, Path::new("C:/missing"));
        let names: Vec<_> = runtimes
            .iter()
            .map(|runtime| runtime.name.as_str())
            .collect();
        assert_eq!(names, ["JavaSE-17", "JavaSE-21", "JavaSE-1.8"]);
        let defaults: Vec<_> = runtimes.iter().filter(|runtime| runtime.default).collect();
        assert_eq!(defaults.len(), 1);
        assert_eq!(defaults[0].home, PathBuf::from("C:/jdk17"));
    }

    #[test]
    fn without_any_sdk_nothing_breaks() {
        let runtimes = runtimes_for(
            &java_project::ResolvedEnvironment::default(),
            Path::new("C:/missing"),
        );
        assert!(runtimes.is_empty());
    }
}

/// A failed import that another import alone can clear: on a fresh OSGi cache the server
/// hands Gradle the path of its initialization script before the cache unpacks the
/// script. The file appears a moment later, so the second import finds it. The first
/// such failure of a server session is repeated once instead of shown, and a second one
/// is a real error, as it would otherwise repeat forever.
pub(crate) struct ImportRetries {
    /// The `config` folder of the server, where the script lives.
    config: PathBuf,
    /// Whether this server session has already asked for the repeat import.
    used: AtomicBool,
}

impl ImportRetries {
    pub(crate) fn new(state_dir: &Path) -> Self {
        Self {
            config: state_dir.join("config"),
            used: AtomicBool::new(false),
        }
    }

    /// True when `failure` names a missing initialization script inside the cache of
    /// this server and the session has not retried such a failure yet.
    pub(crate) fn retry(&self, failure: &ImportFailure) -> bool {
        if !names_missing_init_script(failure, &self.config) {
            return false;
        }
        !self.used.swap(true, Ordering::SeqCst)
    }
}

/// The server neither imports nor names the script in `failure`, or names one outside
/// its own cache, which another import does not create.
fn names_missing_init_script(failure: &ImportFailure, config: &Path) -> bool {
    let Some(script) = missing_init_script(&failure.message) else {
        return false;
    };
    platform::path_key(Path::new(script)).starts_with(platform::path_key(config))
}

/// The script of `The specified initialization script '<path>' does not exist`.
fn missing_init_script(message: &str) -> Option<&str> {
    let after = message.split_once("initialization script '")?.1;
    let (script, rest) = after.split_once('\'')?;
    rest.trim_start()
        .starts_with("does not exist")
        .then_some(script)
}

/// Follows the failures of the server and marks the first one that another import alone
/// may clear. The status bar and the window read the same channel, so both see one
/// decision: the line says `importing`, and the window runs the import instead of
/// telling the user about a failure that is about to go away.
pub(crate) fn follow_failures(
    mut raw: watch::Receiver<Option<ImportFailure>>,
    retries: Arc<ImportRetries>,
    runtime: &tokio::runtime::Handle,
) -> watch::Receiver<Option<ImportFailure>> {
    let (sender, receiver) = watch::channel(None);
    runtime.spawn(async move {
        loop {
            let failure = raw.borrow_and_update().clone().map(|mut failure| {
                failure.retry = retries.retry(&failure);
                failure
            });
            sender.send_replace(failure);
            if sender.is_closed() || raw.changed().await.is_err() {
                return;
            }
        }
    });
    receiver
}

/// The status bar line for a server state. A failed import is the line itself: the
/// server may still say `ready`, but completion and checks do not work until the project
/// is imported, and a `ready` line would hide the reason. A failure that another import
/// is about to clear, and the import that runs after it, read as `importing`.
pub fn status_line(status: &Status, failure: Option<&ImportFailure>, importing: bool) -> String {
    match failure {
        Some(failure) if failure.retry => "Java: importing".to_owned(),
        Some(failure) => format!("Java: import failed: {}", failure.summary),
        None if importing => "Java: importing".to_owned(),
        None => status_text(status),
    }
}

/// The status bar line for a server state.
pub fn status_text(status: &Status) -> String {
    let state = match status.state {
        State::Stopped => "stopped",
        State::Starting if status.crashes > 0 => "restarting",
        State::Starting => "starting",
        State::Importing => "importing",
        State::Ready => "ready",
        State::Degraded => "failed",
        State::Stopping => "stopping",
    };
    match (&status.state, &status.last_error) {
        (State::Stopping, Some(error))
        | (State::Degraded, Some(error))
        | (State::Stopped, Some(error)) => {
            format!("Java: {state}: {error}")
        }
        _ => format!("Java: {state}"),
    }
}

/// jdtls for one open project. Dropping it ends the server.
pub struct JavaSession {
    // Dropped first, while the runtime still runs.
    project: Arc<JavaProject>,
    runtime: runtime::Runtime,
}

impl JavaSession {
    /// Returns at once: the server starts in the background.
    pub fn open(config: JdtlsConfig, trust: Trust) -> Result<Self, OpenError> {
        if trust == Trust::Untrusted {
            return Err(OpenError::NotTrusted);
        }
        let runtime = runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("java-lang")
            .enable_all()
            .build()
            .map_err(OpenError::Setup)?;
        let project = {
            let _entered = runtime.enter();
            JavaProject::open(config, trust)?
        };
        Ok(Self {
            project: Arc::new(project),
            runtime,
        })
    }

    pub fn project(&self) -> &Arc<JavaProject> {
        &self.project
    }

    pub fn status(&self) -> watch::Receiver<Status> {
        self.project.status()
    }

    /// The tokio runtime of the session, for work that needs the same server.
    pub fn handle(&self) -> tokio::runtime::Handle {
        self.runtime.handle().clone()
    }

    /// Runs `work` on the session runtime, for callers without one.
    pub fn spawn<F>(&self, work: F) -> tokio::task::JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.runtime.spawn(work)
    }

    /// Calls `on_change` with the status line now and whenever it changes, from a
    /// thread of the session. Stops with the session.
    pub fn watch_status(&self, on_change: impl Fn(String) + Send + 'static) {
        let mut status = self.status();
        let mut failure = self.project().watch_import_failure();
        let mut importing = self.project().watch_importing();
        self.runtime.spawn(async move {
            let mut shown = String::new();
            loop {
                let failure_now = failure.borrow_and_update().clone();
                let importing_now = *importing.borrow_and_update();
                let text = status_line(
                    &status.borrow_and_update(),
                    failure_now.as_ref(),
                    importing_now,
                );
                if text != shown {
                    on_change(text.clone());
                    shown = text;
                }
                tokio::select! {
                    changed = status.changed() => {
                        if changed.is_err() {
                            return;
                        }
                    }
                    changed = failure.changed() => {
                        if changed.is_err() {
                            return;
                        }
                    }
                    changed = importing.changed() => {
                        if changed.is_err() {
                            return;
                        }
                    }
                }
            }
        });
    }
}

/// A runtime error while preparing a session, for the status bar.
pub fn open_error_text(error: &OpenError) -> String {
    match error {
        OpenError::NotTrusted => "Java: off in an untrusted project".to_owned(),
        OpenError::Setup(error) if error.kind() == io::ErrorKind::NotFound => {
            format!("Java: jdtls not found: {error}")
        }
        OpenError::Setup(error) => format!("Java: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(state: State) -> Status {
        Status {
            state,
            generation: 1,
            alive: true,
            client: None,
            process_id: None,
            crashes: 0,
            last_error: None,
        }
    }

    fn failure_with(message: &str) -> ImportFailure {
        ImportFailure {
            file: PathBuf::new(),
            line: 0,
            message: message.to_owned(),
            summary: message.to_owned(),
            jdk: None,
            tool: None,
            retry: false,
        }
    }

    fn state_dir() -> PathBuf {
        PathBuf::from("state").join("jdtls").join("demo")
    }

    fn init_script(state: &Path) -> PathBuf {
        state
            .join("config")
            .join("org.eclipse.osgi")
            .join("57")
            .join("0")
            .join(".cp")
            .join("gradle")
            .join("init")
            .join("init.gradle")
    }

    fn missing_script(script: &Path) -> String {
        format!(
            "The specified initialization script '{}' does not exist",
            script.display()
        )
    }

    #[test]
    fn states_read_as_short_lines() {
        assert_eq!(status_text(&status(State::Importing)), "Java: importing");
        let mut restarting = status(State::Starting);
        restarting.crashes = 1;
        assert_eq!(status_text(&restarting), "Java: restarting");
        let mut failed = status(State::Degraded);
        failed.last_error = Some("exited".to_owned());
        assert_eq!(status_text(&failed), "Java: failed: exited");
        let mut out_of_memory = status(State::Stopped);
        out_of_memory.last_error = Some("memory is low".to_owned());
        assert_eq!(status_text(&out_of_memory), "Java: stopped: memory is low");
    }

    #[test]
    fn a_failed_import_is_the_line_and_hides_ready() {
        let mut ready = status(State::Ready);
        ready.last_error = None;
        assert_eq!(status_text(&ready), "Java: ready");
        let failure = ImportFailure {
            file: PathBuf::new(),
            line: 0,
            message: "the cause".into(),
            summary: "the cause".into(),
            jdk: None,
            tool: None,
            retry: false,
        };
        assert_eq!(
            status_line(&ready, Some(&failure), false),
            "Java: import failed: the cause"
        );
    }

    #[test]
    fn a_missing_initialization_script_in_the_server_cache_is_retried_once() {
        let state = state_dir();
        let retries = ImportRetries::new(&state);
        let failure = failure_with(&missing_script(&init_script(&state)));

        assert!(
            retries.retry(&failure),
            "the fresh cache asks for one more import"
        );
        assert!(
            !retries.retry(&failure),
            "a second refusal of the same kind is a real error"
        );
    }

    #[test]
    fn another_script_or_cause_is_not_retried() {
        let state = state_dir();
        let retries = ImportRetries::new(&state);

        let outside = PathBuf::from("elsewhere")
            .join("gradle")
            .join("init")
            .join("init.gradle");
        assert!(!retries.retry(&failure_with(&missing_script(&outside))));
        assert!(!retries.retry(&failure_with(
            "Task with name 'postRelease' not found in root project 'demo'."
        )));
        assert!(!retries.retry(&failure_with(&format!(
            "The specified initialization script '{}' does not work",
            init_script(&state).display()
        ))));

        // Neither refusal used the one repeat of the session.
        assert!(retries.retry(&failure_with(&missing_script(&init_script(&state)))));
    }

    #[test]
    fn an_import_in_flight_or_about_to_start_reads_as_importing() {
        let ready = status(State::Ready);
        assert_eq!(status_line(&ready, None, true), "Java: importing");

        let mut failure = failure_with(&missing_script(&init_script(&state_dir())));
        failure.retry = true;
        assert_eq!(
            status_line(&ready, Some(&failure), false),
            "Java: importing",
            "the failure the window repeats is not shown"
        );

        failure.retry = false;
        assert_eq!(
            status_line(&ready, Some(&failure), true),
            format!("Java: import failed: {}", failure.summary),
            "a real failure is not hidden by an import in flight"
        );
    }

    #[test]
    fn a_jdk_release_file_names_the_runtime() {
        let home = test_support::scratch("lang-release");
        fs::write(
            home.join("release"),
            "IMPLEMENTOR=\"x\"\nJAVA_VERSION=\"25.0.1\"\n",
        )
        .unwrap();
        let runtime = runtime_of(&home).unwrap();
        assert_eq!(runtime.name, "JavaSE-25");
        assert!(runtime.default);
    }

    #[cfg(windows)]
    #[test]
    fn projects_get_separate_state_folders() {
        let state = Path::new(r"C:\state");
        let one = project_state_dir(state, Path::new(r"C:\work\one"));
        let two = project_state_dir(state, Path::new(r"C:\other\one"));
        assert_ne!(one, two);
        assert!(one.starts_with(r"C:\state\jdtls"));
    }

    #[test]
    fn an_untrusted_session_starts_nothing() {
        let missing = PathBuf::from(r"Z:\does\not\exist");
        let config = config_from_env(&missing, &missing);
        let config = config.unwrap_or(JdtlsConfig {
            jdtls_home: missing.clone(),
            debug_plugin: missing.clone(),
            java_home: missing.clone(),
            project_root: missing.clone(),
            state_dir: missing,
            runtimes: Vec::new(),
            maven_settings: None,
            gradle_home: None,
            gradle_user_home: None,
            gradle_java_home: None,
            offline: true,
            environment: HashMap::new(),
        });
        let outcome = JavaSession::open(config, Trust::Untrusted);
        assert!(matches!(outcome, Err(OpenError::NotTrusted)));
    }
}
