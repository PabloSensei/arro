//! Caches the user invalidates go away at the next start of the project, before any
//! process of the project opens them: a running language server keeps its folder locked.

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

/// The list of folders to delete at the next start, one per line.
const PENDING: &str = "invalidate.pending";

/// How long a start waits for the previous process of the project to let go of a folder.
const TRIES: u32 = 50;
const PAUSE: Duration = Duration::from_millis(200);

/// Remembers `paths` for deletion at the next start of a project.
pub fn schedule_invalidation(state_dir: &Path, paths: &[PathBuf]) -> io::Result<()> {
    fs::create_dir_all(state_dir)?;
    let file = state_dir.join(PENDING);
    let mut text = fs::read_to_string(&file).unwrap_or_default();
    for path in paths {
        let line = path.to_string_lossy();
        if !text.lines().any(|known| known == line) {
            text.push_str(&line);
            text.push('\n');
        }
    }
    fs::write(file, text)
}

/// The folders waiting for deletion.
pub fn pending_invalidation(state_dir: &Path) -> Vec<PathBuf> {
    fs::read_to_string(state_dir.join(PENDING))
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(PathBuf::from)
        .collect()
}

/// Deletes the folders an Invalidate Caches left for this start. Only a folder inside one
/// of `allowed` goes, never `state_dir` itself, so a damaged list cannot reach anything
/// else. A folder still held by the previous process is retried for a while. Returns
/// what could not be deleted.
pub fn apply_pending_invalidation(state_dir: &Path, allowed: &[&Path]) -> Vec<String> {
    let paths = pending_invalidation(state_dir);
    if paths.is_empty() {
        return Vec::new();
    }
    let mut failed = Vec::new();
    for path in paths {
        let inside = path.is_absolute()
            && !path.components().any(|part| part == Component::ParentDir)
            && path != state_dir
            && allowed.iter().any(|root| path.starts_with(root));
        if !inside {
            failed.push(format!("{}: outside the caches of Arro", path.display()));
            continue;
        }
        if let Err(error) = remove_with_retries(&path) {
            failed.push(format!("{}: {error}", path.display()));
        }
    }
    let _ = fs::remove_file(state_dir.join(PENDING));
    failed
}

fn remove_with_retries(path: &Path) -> io::Result<()> {
    let mut tries = 0;
    loop {
        match fs::remove_dir_all(path) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                tries += 1;
                if tries >= TRIES {
                    return Err(error);
                }
                std::thread::sleep(PAUSE);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join("arro-invalidate-tests")
            .join(format!("{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_scheduled_folder_goes_at_the_next_start() {
        let state = scratch("scheduled");
        let cache = state.join("projects").join("abc");
        fs::create_dir_all(cache.join("inner")).unwrap();
        schedule_invalidation(&state, std::slice::from_ref(&cache)).unwrap();
        schedule_invalidation(&state, std::slice::from_ref(&cache)).unwrap();
        assert_eq!(pending_invalidation(&state), vec![cache.clone()]);

        let failed = apply_pending_invalidation(&state, &[&state]);

        assert!(failed.is_empty(), "{failed:?}");
        assert!(!cache.exists());
        assert!(state.exists(), "the state folder stays");
        assert!(pending_invalidation(&state).is_empty());
    }

    #[test]
    fn a_folder_outside_the_allowed_ones_stays() {
        let state = scratch("outside");
        let other = scratch("outside-other");
        schedule_invalidation(&state, &[other.clone(), state.clone()]).unwrap();

        let failed = apply_pending_invalidation(&state, &[&state]);

        assert_eq!(failed.len(), 2, "{failed:?}");
        assert!(other.exists());
        assert!(state.exists());
    }
}
