//! The data folders of the old product names: settings and plugins from `%APPDATA%`,
//! state and caches from `%LOCALAPPDATA%`. The new name is `base::app::FOLDER`.

/// The profile variable and the old folder names under it, in the order they are tried.
const LEGACY: &[(&str, &[&str])] = &[
    ("APPDATA", &["ide"]),
    ("LOCALAPPDATA", &["java-ide", "ide"]),
];

/// Moves the old folders when the folder of the new name is absent.
pub fn take_over_old_folders() -> Vec<String> {
    let state = base::app::folder("LOCALAPPDATA");
    let had_state = state.exists();
    let mut failed = base::app::take_over_old_folders(LEGACY);
    if !had_state && state.exists() {
        failed.extend(drop_jdtls_caches(&state));
    }
    failed
}

/// jdtls keeps absolute paths of its own folder in its OSGi and Gradle caches, so after a
/// move they point at the old name: imports fail on a missing `init.gradle` and the first
/// start can hang on a bundle lock. The folder is a cache; jdtls rebuilds it on its next start.
fn drop_jdtls_caches(state: &std::path::Path) -> Option<String> {
    let jdtls = state.join("jdtls");
    if !jdtls.exists() {
        return None;
    }
    std::fs::remove_dir_all(&jdtls)
        .err()
        .map(|error| format!("could not remove {}: {error}", jdtls.display()))
}

#[cfg(all(test, windows))]
mod tests {
    use super::take_over_old_folders;
    use std::path::PathBuf;

    /// Puts the profile variables back when the test ends, even on a panic.
    struct Variables(Vec<(&'static str, Option<std::ffi::OsString>)>);

    impl Variables {
        fn set(values: &[(&'static str, PathBuf)]) -> Self {
            let previous = values
                .iter()
                .map(|(name, _)| (*name, std::env::var_os(name)))
                .collect();
            for (name, value) in values {
                unsafe { std::env::set_var(name, value) };
            }
            Self(previous)
        }
    }

    impl Drop for Variables {
        fn drop(&mut self) {
            for (name, value) in &self.0 {
                unsafe {
                    match value {
                        Some(value) => std::env::set_var(name, value),
                        None => std::env::remove_var(name),
                    }
                }
            }
        }
    }

    #[cfg(windows)]
    #[test]
    fn the_old_folders_move_once_and_the_new_name_wins() {
        let root = test_support::scratch("app-folders");
        let appdata = root.join("appdata");
        let local = root.join("local");
        std::fs::create_dir_all(appdata.join("ide").join("plugins")).unwrap();
        std::fs::create_dir_all(local.join("java-ide").join("logs")).unwrap();
        let jdtls_config = local
            .join("java-ide")
            .join("jdtls")
            .join("p-1")
            .join("config");
        std::fs::create_dir_all(&jdtls_config).unwrap();
        let _variables = Variables::set(&[
            ("APPDATA", appdata.clone()),
            ("LOCALAPPDATA", local.clone()),
        ]);

        assert!(take_over_old_folders().is_empty());
        assert!(base::app::folder_under(&appdata).join("plugins").is_dir());
        assert!(!appdata.join("ide").exists());
        assert!(base::app::folder_under(&local).join("logs").is_dir());
        assert!(!local.join("java-ide").exists());
        // jdtls caches hold the old absolute paths, so they do not move.
        assert!(!base::app::folder_under(&local).join("jdtls").exists());

        // A second start finds the new folder and leaves the old name alone.
        std::fs::create_dir_all(appdata.join("ide")).unwrap();
        assert!(take_over_old_folders().is_empty());
        assert!(appdata.join("ide").exists());

        // The cache folder is taken over when the state folder of the old name is gone.
        std::fs::remove_dir_all(base::app::folder_under(&local)).unwrap();
        std::fs::create_dir_all(local.join("ide").join("projects")).unwrap();
        assert!(take_over_old_folders().is_empty());
        assert!(base::app::folder_under(&local).join("projects").is_dir());
        assert!(!local.join("ide").exists());
    }
}
