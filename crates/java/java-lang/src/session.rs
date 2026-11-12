//! A project as the window holds it: jdtls on a runtime of its own, so nothing in the
//! window waits on it, and a line of text for the status bar.

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use java_project::Jdk;
use jdtls_adapter::{JdtlsConfig, Runtime};
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
        self.runtime.spawn(async move {
            let mut shown = String::new();
            loop {
                let text = status_text(&status.borrow_and_update());
                let text = match failure.borrow_and_update().as_ref() {
                    Some(failure) => format!("{text}: import failed: {}", failure.summary),
                    None => text,
                };
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
    fn a_jdk_release_file_names_the_runtime() {
        let home = std::env::temp_dir().join("java-lang-release-test");
        fs::create_dir_all(&home).unwrap();
        fs::write(
            home.join("release"),
            "IMPLEMENTOR=\"x\"\nJAVA_VERSION=\"25.0.1\"\n",
        )
        .unwrap();
        let runtime = runtime_of(&home).unwrap();
        assert_eq!(runtime.name, "JavaSE-25");
        assert!(runtime.default);
    }

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
