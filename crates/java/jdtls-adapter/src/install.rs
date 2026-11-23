//! Which installation of the server a state folder belongs to.
//!
//! The OSGi configuration folder keeps the paths of the bundles of the release it was
//! built from, and the Gradle import writes the path of its `init.gradle` into the
//! arguments of the daemon. A release unpacked into a new folder, as every update of the
//! IDE does, therefore leaves a configuration folder that names files of the old folder,
//! and the import fails until the folder is removed. The mark below tells the releases
//! apart.
//!
//! The workspace folder (`-data`) keeps the model of the projects: their source folders,
//! their output and the class path the previous import produced, including the JVM Gradle
//! ran on. A configuration folder that is gone cannot refresh that model, so an import of
//! the new release that fails leaves the model of the old one in place and the editor
//! sees the dependencies of that import. The two folders are therefore a pair: the model
//! goes whenever the configuration goes.

use std::fs;
use std::io;
use std::path::Path;

use platform::{CanonPath, path_key};
use serde_json::{Value, json};

/// The mark under the state folder of the server.
const MARK_FILE: &str = "jdtls-install.json";
/// The name jdtls gives the OSGi configuration folder under the state folder.
const CONFIG_DIR: &str = "config";
/// The name jdtls gives the folder of its workspace (`-data`) under the state folder.
const WORKSPACE_DIR: &str = "workspace";
/// The bundle whose jar name carries the build of the release.
const CORE_BUNDLE_PREFIX: &str = "org.eclipse.jdt.ls.core_";
const JAR_SUFFIX: &str = ".jar";

/// One installation of the server: where the release is unpacked and which launcher runs it.
///
/// The path alone is not enough: an update may unpack a new release over the old folder.
/// The launcher jar name carries the version of the launcher, and the name of the core
/// bundle carries the build of jdtls, which is the `Bundle-Version` of its manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InstallMark {
    /// The canonical path of the unpacked release.
    home: String,
    /// The launcher jar of the release.
    launcher: String,
    /// The build of the release, when its plugins name one.
    build: Option<String>,
}

impl InstallMark {
    /// The mark of the release at `jdtls_home` with `launcher` in its `plugins` folder.
    pub(crate) fn of(jdtls_home: &Path, launcher: &Path) -> io::Result<Self> {
        let home = CanonPath::new(jdtls_home)?;
        let launcher = launcher
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        Ok(Self {
            home: home.to_string(),
            launcher,
            build: build_version(jdtls_home),
        })
    }

    fn names_same_installation(&self, current: &Self) -> bool {
        self.launcher == current.launcher
            && self.build == current.build
            && path_key(Path::new(&self.home)) == path_key(Path::new(&current.home))
    }

    fn to_json(&self) -> Value {
        json!({
            "home": self.home,
            "launcher": self.launcher,
            "build": self.build,
        })
    }

    fn from_json(value: &Value) -> Option<Self> {
        Some(Self {
            home: value.get("home")?.as_str()?.to_owned(),
            launcher: value.get("launcher")?.as_str()?.to_owned(),
            build: value
                .get("build")
                .and_then(Value::as_str)
                .map(str::to_owned),
        })
    }
}

/// The build of the release from the name of the core bundle jar: `1.63.0.202610191322`
/// is the `Bundle-Version` its manifest carries.
fn build_version(jdtls_home: &Path) -> Option<String> {
    fs::read_dir(jdtls_home.join("plugins"))
        .ok()?
        .filter_map(Result::ok)
        .find_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let version = name
                .strip_prefix(CORE_BUNDLE_PREFIX)?
                .strip_suffix(JAR_SUFFIX)?;
            Some(version.to_owned())
        })
}

/// The mark a previous start wrote, if there is one a reader understands.
fn recorded(state_dir: &Path) -> Option<InstallMark> {
    let text = fs::read_to_string(state_dir.join(MARK_FILE)).ok()?;
    InstallMark::from_json(&serde_json::from_str(&text).ok()?)
}

/// Removes the configuration folder of another installation, with the model that folder
/// produced, and records `current`.
///
/// A configuration folder that is gone while the mark already names `current` was
/// removed between two starts, and the model left behind is of the same unknown age, so
/// it goes as well.
///
/// The caller holds the lock of the state folder, so no other server of this project
/// reads it. A failure keeps the old mark, so the next start tries again instead of
/// launching a server on the cache and the model of another release.
pub(crate) fn prepare(state_dir: &Path, current: &InstallMark) -> io::Result<()> {
    let config = state_dir.join(CONFIG_DIR);
    let same = recorded(state_dir).is_some_and(|mark| mark.names_same_installation(current))
        && config.is_dir();
    if same {
        return Ok(());
    }
    if config.is_dir() {
        fs::remove_dir_all(&config)?;
    }
    let workspace = state_dir.join(WORKSPACE_DIR);
    if workspace.is_dir() {
        fs::remove_dir_all(&workspace)?;
    }
    let text = serde_json::to_string_pretty(&current.to_json())
        .unwrap_or_else(|_| current.to_json().to_string());
    fs::write(state_dir.join(MARK_FILE), text)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::fs;
    use std::path::{Path, PathBuf};

    use sidecar::ServerSpec;

    use super::*;
    use crate::{Jdtls, JdtlsConfig};

    const LAUNCHER: &str = "org.eclipse.equinox.launcher_1.8.0.v20260804-1928.jar";
    const BUILD: &str = "1.63.0.202610191322";

    /// A release with one launcher jar and the core bundle of `build`.
    fn release(name: &str, launcher: &str, build: &str) -> PathBuf {
        let home = test_support::scratch(&format!("jdtls-install-{name}"));
        let plugins = home.join("plugins");
        fs::create_dir_all(&plugins).unwrap();
        fs::write(plugins.join(launcher), b"").unwrap();
        fs::write(
            plugins.join(format!("{CORE_BUNDLE_PREFIX}{build}{JAR_SUFFIX}")),
            b"",
        )
        .unwrap();
        home
    }

    fn mark(name: &str) -> InstallMark {
        let home = release(name, LAUNCHER, BUILD);
        InstallMark::of(&home, &home.join("plugins").join(LAUNCHER)).unwrap()
    }

    /// A state folder of a server that ran before: the OSGi cache and the model.
    fn state(name: &str) -> PathBuf {
        let state = test_support::scratch(&format!("jdtls-install-state-{name}"));
        let cache = state.join("config").join("org.eclipse.osgi");
        fs::create_dir_all(&cache).unwrap();
        fs::write(cache.join("init.gradle"), b"old").unwrap();
        fs::create_dir_all(state.join("workspace")).unwrap();
        fs::write(state.join("workspace").join("index.txt"), b"model").unwrap();
        state
    }

    fn jdtls(state_dir: &Path, home: &Path) -> Jdtls {
        let config = JdtlsConfig {
            jdtls_home: home.to_path_buf(),
            debug_plugin: home.join("java-debug.jar"),
            java_home: home.join("jdk"),
            project_root: home.join("project"),
            state_dir: state_dir.to_path_buf(),
            runtimes: Vec::new(),
            maven_settings: None,
            gradle_home: None,
            gradle_user_home: None,
            gradle_java_home: None,
            offline: true,
            environment: HashMap::new(),
            max_heap_mb: None,
        };
        Jdtls::new(config).unwrap()
    }

    #[test]
    fn a_state_folder_without_a_mark_loses_the_configuration_and_the_model() {
        let current = mark("no-mark");
        let state = state("no-mark");

        prepare(&state, &current).unwrap();

        assert!(!state.join("config").exists());
        assert!(
            !state.join("workspace").exists(),
            "the model of the projects is of the same unknown release as the configuration"
        );
        assert_eq!(recorded(&state), Some(current));
    }

    #[test]
    fn a_mark_of_another_folder_clears_the_configuration_and_the_model() {
        let old = mark("other-old");
        let state = state("other");
        fs::write(state.join(MARK_FILE), old.to_json().to_string()).unwrap();

        let current = mark("other-new");
        prepare(&state, &current).unwrap();

        assert!(!state.join("config").exists());
        assert!(!state.join("workspace").exists());
        assert_eq!(recorded(&state), Some(current));
    }

    #[test]
    fn a_new_launcher_of_the_same_folder_clears_the_configuration_and_the_model() {
        let home = release("new-launcher", LAUNCHER, BUILD);
        let old = InstallMark::of(&home, &home.join("plugins").join(LAUNCHER)).unwrap();
        let state = state("new-launcher");
        fs::write(state.join(MARK_FILE), old.to_json().to_string()).unwrap();

        let launcher = "org.eclipse.equinox.launcher_1.9.0.v20261101-1200.jar";
        fs::write(home.join("plugins").join(launcher), b"").unwrap();
        let current = InstallMark::of(&home, &home.join("plugins").join(launcher)).unwrap();
        prepare(&state, &current).unwrap();

        assert!(!state.join("config").exists());
        assert!(!state.join("workspace").exists());
        assert_eq!(recorded(&state), Some(current));
    }

    #[test]
    fn a_configuration_removed_between_starts_takes_the_model_with_it() {
        let current = mark("removed");
        let state = state("removed");
        fs::write(state.join(MARK_FILE), current.to_json().to_string()).unwrap();
        fs::remove_dir_all(state.join("config")).unwrap();

        prepare(&state, &current).unwrap();

        assert!(
            !state.join("workspace").exists(),
            "a model the removed configuration cannot refresh is not reused"
        );
        assert_eq!(recorded(&state), Some(current));
    }

    #[test]
    fn the_same_installation_keeps_the_configuration_and_the_model() {
        let current = mark("same");
        let state = state("same");
        fs::write(state.join(MARK_FILE), current.to_json().to_string()).unwrap();

        prepare(&state, &current).unwrap();

        assert_eq!(
            fs::read_to_string(
                state
                    .join("config")
                    .join("org.eclipse.osgi")
                    .join("init.gradle")
            )
            .unwrap(),
            "old"
        );
        assert!(state.join("workspace").join("index.txt").is_file());
    }

    #[test]
    fn a_new_install_gives_the_start_a_clean_configuration_folder_and_model() {
        let state = test_support::scratch("jdtls-install-plan-state");
        let first = jdtls(&state, &release("plan-old", LAUNCHER, BUILD));
        first.preflight().unwrap();
        drop(first);
        let cache = state.join("config").join("org.eclipse.osgi").join("138");
        fs::create_dir_all(&cache).unwrap();
        fs::write(cache.join("init.gradle"), b"old").unwrap();
        fs::create_dir_all(state.join("workspace").join(".metadata")).unwrap();

        let second = jdtls(
            &state,
            &release(
                "plan-new",
                "org.eclipse.equinox.launcher_1.9.0.v20261101-1200.jar",
                "1.64.0.202612011200",
            ),
        );
        second.preflight().unwrap();
        let plan = second.plan();

        let at = plan
            .args
            .iter()
            .position(|arg| arg == "-configuration")
            .unwrap();
        assert_eq!(Path::new(&plan.args[at + 1]), state.join("config"));
        assert!(
            !state.join("config").exists(),
            "the arguments name the folder the new release builds from scratch"
        );
        assert!(
            !state.join("workspace").exists(),
            "the import of the new release writes the model again"
        );
    }

    #[test]
    fn the_logs_of_an_older_installation_are_not_the_cause_of_a_failed_start() {
        let state = test_support::scratch("jdtls-install-crash");
        let jdtls = jdtls(&state, &release("crash", LAUNCHER, BUILD));
        let cache = state.join("config").join("org.eclipse.osgi");
        fs::create_dir_all(&cache).unwrap();
        fs::write(
            cache.join("1.log"),
            "!MESSAGE Application error: the old release\n",
        )
        .unwrap();

        jdtls.preflight().unwrap();

        assert_eq!(jdtls.crash_reason(), None);
    }
}
