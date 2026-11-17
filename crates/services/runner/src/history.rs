//! The last test runs of a project: when each ran, how it ended, and the events that
//! build its tree again. The file lives in the state folder of the project, not in the
//! settings, so it stays on one machine.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{TestEvent, TestRequest};

/// The runs kept. Older ones go first when another one arrives.
pub const HISTORY_LIMIT: usize = 10;

/// The file of the history, under the state folder of the project.
pub const HISTORY_FILE: &str = "test-history.toml";

/// The history of a project whose state lives in `state_root`.
pub fn test_history_file(state_root: &Path) -> PathBuf {
    state_root.join(HISTORY_FILE)
}

/// One finished test run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TestHistoryRun {
    /// Seconds since the Unix epoch.
    pub at: u64,
    /// What the run was started for, as the test window names it.
    pub name: String,
    pub passed: usize,
    pub failed: usize,
    pub aborted: usize,
    pub skipped: usize,
    pub request: TestRequest,
    /// The events the tree of that run was built from.
    #[serde(default)]
    pub events: Vec<TestEvent>,
}

#[derive(Default, Serialize, Deserialize)]
struct HistoryFile {
    #[serde(default, rename = "run")]
    runs: Vec<TestHistoryRun>,
}

/// A missing file is an empty history, a broken one is an error: overwriting it would
/// lose the runs.
pub fn load_test_history(file: &Path) -> Result<Vec<TestHistoryRun>, String> {
    match fs::read_to_string(file) {
        Ok(text) => toml::from_str::<HistoryFile>(&text)
            .map(|file| file.runs)
            .map_err(|error| format!("cannot read {}: {error}", file.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(format!("cannot read {}: {error}", file.display())),
    }
}

/// Writes the newest [`HISTORY_LIMIT`] runs, the newest first.
pub fn save_test_history(file: &Path, runs: &[TestHistoryRun]) -> Result<(), String> {
    if let Some(directory) = file.parent() {
        fs::create_dir_all(directory)
            .map_err(|error| format!("cannot create {}: {error}", directory.display()))?;
    }
    let kept: Vec<TestHistoryRun> = runs.iter().take(HISTORY_LIMIT).cloned().collect();
    let text = toml::to_string_pretty(&HistoryFile { runs: kept })
        .map_err(|error| format!("cannot write {}: {error}", file.display()))?;
    fs::write(file, text).map_err(|error| format!("cannot write {}: {error}", file.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(at: u64) -> TestHistoryRun {
        TestHistoryRun {
            at,
            name: format!("Tests {at}"),
            passed: 3,
            failed: 1,
            aborted: 0,
            skipped: 0,
            request: TestRequest::default(),
            events: Vec::new(),
        }
    }

    #[test]
    fn runs_survive_a_round_trip() {
        let file = test_support::scratch("test-history").join("history.toml");
        let runs = vec![run(20), run(10)];

        save_test_history(&file, &runs).unwrap();

        assert_eq!(load_test_history(&file).unwrap(), runs);
    }

    #[test]
    fn only_the_newest_runs_are_kept() {
        let file = test_support::scratch("test-history-limit").join("history.toml");
        let runs: Vec<TestHistoryRun> = (0..20).map(run).collect();

        save_test_history(&file, &runs).unwrap();

        let loaded = load_test_history(&file).unwrap();
        assert_eq!(loaded.len(), HISTORY_LIMIT);
        assert_eq!(loaded[0].at, 0);
        assert_eq!(loaded[HISTORY_LIMIT - 1].at, 9);
    }

    #[test]
    fn a_missing_file_is_an_empty_history() {
        let file = test_support::scratch("test-history-missing").join("history.toml");
        assert!(load_test_history(&file).unwrap().is_empty());
    }
}
