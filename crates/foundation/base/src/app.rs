//! The folder of the product under the user profile: one name, not copies of the string.

use std::path::{Path, PathBuf};

/// The name of the folder in `%APPDATA%` and `%LOCALAPPDATA%`.
pub const FOLDER: &str = "Arro";

/// `base/FOLDER`.
pub fn folder_under(base: &Path) -> PathBuf {
    base.join(FOLDER)
}

/// `FOLDER` under the profile variable, the temporary folder when the variable is missing.
/// On Unix the two Windows variables map to the folders of the desktop: `APPDATA` to the
/// configuration folder and `LOCALAPPDATA` to the data folder. macOS keeps both under
/// `~/Library/Application Support`, where its applications keep theirs.
pub fn folder(variable: &str) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        let base = match variable {
            "APPDATA" | "LOCALAPPDATA" => library("Application Support"),
            other => std::env::var_os(other)
                .map(PathBuf::from)
                .unwrap_or_else(std::env::temp_dir),
        };
        folder_under(&base)
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let base = match variable {
            "APPDATA" => xdg("XDG_CONFIG_HOME", ".config"),
            "LOCALAPPDATA" => xdg("XDG_DATA_HOME", ".local/share"),
            other => std::env::var_os(other)
                .map(PathBuf::from)
                .unwrap_or_else(std::env::temp_dir),
        };
        folder_under(&base)
    }
    #[cfg(windows)]
    {
        let base = std::env::var_os(variable).map_or_else(std::env::temp_dir, PathBuf::from);
        folder_under(&base)
    }
}

/// `~/Library/<name>`, or the temporary folder when there is no home folder.
#[cfg(target_os = "macos")]
fn library(name: &str) -> PathBuf {
    match std::env::var_os("HOME").map(PathBuf::from) {
        Some(home) => home.join("Library").join(name),
        None => std::env::temp_dir(),
    }
}

/// `$XDG_...` when it is absolute, else the folder of the same name under the home folder.
#[cfg(all(unix, not(target_os = "macos")))]
fn xdg(variable: &str, fallback: &str) -> PathBuf {
    if let Some(value) = std::env::var_os(variable).map(PathBuf::from)
        && value.is_absolute()
    {
        return value;
    }
    match std::env::var_os("HOME").map(PathBuf::from) {
        Some(home) => home.join(fallback),
        None => std::env::temp_dir(),
    }
}

/// Moves the folder of an old name to `FOLDER` when the new folder is absent. `legacy` lists
/// the profile variable and the old folder names under it, in the order they are tried.
/// Returns the reasons of the moves that failed, one line each: a failed move leaves the new
/// folder empty, which the caller logs before it starts using it.
pub fn take_over_old_folders(legacy: &[(&str, &[&str])]) -> Vec<String> {
    let mut failed = Vec::new();
    for (variable, names) in legacy {
        let Some(base) = std::env::var_os(variable).map(PathBuf::from) else {
            continue;
        };
        let new = folder_under(&base);
        if new.exists() {
            continue;
        }
        for name in *names {
            let old = base.join(name);
            if !old.exists() {
                continue;
            }
            if let Err(error) = std::fs::rename(&old, &new) {
                failed.push(format!(
                    "could not move {} to {}: {error}",
                    old.display(),
                    new.display()
                ));
            }
            break;
        }
    }
    failed
}
