//! The Git choices of the project, in `.arro/git.toml`: the branches the branch popup
//! keeps above the others. The file belongs to the project and travels with it, as the
//! other project settings do.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The folder of the project that keeps the IDE's own files.
const METADATA_FOLDER: &str = ".arro";
/// The Git choices of the project.
const SETTINGS_FILE: &str = "git.toml";

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitProjectSettings {
    /// The branches the popup lists before the others, by their full names.
    #[serde(default)]
    pub favourite_branches: Vec<String>,
}

impl GitProjectSettings {
    /// The branches IDEA stars for a project that has not chosen its own.
    pub fn with_defaults() -> Self {
        Self {
            favourite_branches: ["main", "master", "develop"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
        }
    }
}

pub fn git_settings_file(root: &Path) -> PathBuf {
    root.join(METADATA_FOLDER).join(SETTINGS_FILE)
}

/// A missing or broken file is the defaults: nothing here can lose text.
pub fn load_git_settings(root: &Path) -> GitProjectSettings {
    fs::read_to_string(git_settings_file(root))
        .ok()
        .and_then(|text| toml::from_str(&text).ok())
        .unwrap_or_else(GitProjectSettings::with_defaults)
}

pub fn save_git_settings(root: &Path, settings: &GitProjectSettings) -> Result<(), String> {
    let file = git_settings_file(root);
    if let Some(directory) = file.parent() {
        fs::create_dir_all(directory).map_err(|error| error.to_string())?;
    }
    let text = toml::to_string_pretty(settings).map_err(|error| error.to_string())?;
    fs::write(file, text).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_favourites_of_a_new_project_are_the_common_branches() {
        let root = test_support::scratch("git-settings-defaults");

        let settings = load_git_settings(&root);

        assert_eq!(
            settings.favourite_branches,
            ["main", "master", "develop"],
            "a project without the file starts with the branches IDEA stars"
        );
    }

    #[test]
    fn the_favourites_of_the_project_survive_a_round_trip() {
        let root = test_support::scratch("git-settings-round-trip");
        let settings = GitProjectSettings {
            favourite_branches: vec!["feature/a".to_owned(), "main".to_owned()],
        };

        save_git_settings(&root, &settings).unwrap();
        let read = load_git_settings(&root);

        assert_eq!(read, settings);
        assert!(
            git_settings_file(&root).is_file(),
            "the file belongs to the project"
        );
    }

    #[test]
    fn a_broken_file_is_the_defaults() {
        let root = test_support::scratch("git-settings-broken");
        fs::create_dir_all(root.join(METADATA_FOLDER)).unwrap();
        fs::write(git_settings_file(&root), "[broken").unwrap();

        assert_eq!(
            load_git_settings(&root),
            GitProjectSettings::with_defaults()
        );
    }
}
