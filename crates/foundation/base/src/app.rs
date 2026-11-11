//! The folder of the product under the user profile: one name, not copies of the string.

use std::path::{Path, PathBuf};

/// The name of the folder in `%APPDATA%` and `%LOCALAPPDATA%`.
pub const FOLDER: &str = "Arro";

/// `base/FOLDER`.
pub fn folder_under(base: &Path) -> PathBuf {
    base.join(FOLDER)
}

/// `FOLDER` under the profile variable, the temporary folder when the variable is missing.
pub fn folder(variable: &str) -> PathBuf {
    let base = std::env::var_os(variable).map_or_else(std::env::temp_dir, PathBuf::from);
    folder_under(&base)
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
