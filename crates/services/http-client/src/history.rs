//! The last answers kept in the state folder: one metadata file and one body file per
//! response, the newest first, at most [`crate::limits::HISTORY`] of them.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::execute::Response;
use crate::variables::PreparedRequest;

/// What the history list shows of an answer.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct HistoryEntry {
    /// The number of the entry: the time it was written, in milliseconds.
    pub id: u64,
    /// When the answer came, in seconds since the start of the epoch.
    pub at: u64,
    pub method: String,
    pub url: String,
    pub name: Option<String>,
    pub status: u16,
    pub reason: String,
    pub elapsed_ms: u64,
    pub size: usize,
}

impl HistoryEntry {
    /// The status line of the answer.
    pub fn status_line(&self) -> String {
        if self.reason.is_empty() {
            self.status.to_string()
        } else {
            format!("{} {}", self.status, self.reason)
        }
    }

    /// The label of the row: the name of the request, or its method and URL.
    pub fn label(&self) -> String {
        match &self.name {
            Some(name) if !name.is_empty() => name.clone(),
            _ => format!("{} {}", self.method, self.url),
        }
    }
}

static SERIAL: AtomicU64 = AtomicU64::new(1);

/// The folder of the history under the state folder of the window.
pub fn history_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("http-history")
}

fn entry_file(dir: &Path, id: u64) -> PathBuf {
    dir.join(format!("{id}.json"))
}

/// The body file of an entry.
pub fn body_file(state_dir: &Path, entry: &HistoryEntry) -> PathBuf {
    history_dir(state_dir).join(format!("{}.body", entry.id))
}

fn now() -> (u64, u64) {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    (duration.as_millis() as u64, duration.as_secs())
}

/// Writes the answer and its body into the history, newest first, and drops the entries
/// above the limit. Blocks: the caller runs it away from the window.
pub fn save_response(
    state_dir: &Path,
    request: &PreparedRequest,
    response: &Response,
) -> std::io::Result<HistoryEntry> {
    let dir = history_dir(state_dir);
    std::fs::create_dir_all(&dir)?;
    let (millis, at) = now();
    // One more digit, so two answers of the same millisecond differ.
    let id = millis
        .saturating_mul(1000)
        .saturating_add(SERIAL.fetch_add(1, Ordering::Relaxed) % 1000);
    let entry = HistoryEntry {
        id,
        at,
        method: request.method.as_str().to_owned(),
        url: request.url.clone(),
        name: request.name.clone(),
        status: response.status,
        reason: response.reason.clone(),
        elapsed_ms: response.elapsed.as_millis() as u64,
        size: response.size(),
    };
    std::fs::write(body_file(state_dir, &entry), &response.body)?;
    let text = serde_json::to_string(&entry).map_err(std::io::Error::other)?;
    std::fs::write(entry_file(&dir, id), text)?;
    prune(&dir);
    Ok(entry)
}

/// The entries of the state folder, the newest first.
pub fn load_history(state_dir: &Path) -> Vec<HistoryEntry> {
    let dir = history_dir(state_dir);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut found: Vec<HistoryEntry> = entries
        .flatten()
        .filter(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "json")
        })
        .filter_map(|entry| std::fs::read_to_string(entry.path()).ok())
        .filter_map(|text| serde_json::from_str(&text).ok())
        .collect();
    found.sort_by_key(|entry| std::cmp::Reverse(entry.id));
    found
}

/// The body of an entry as text, with the bytes that are not UTF-8 replaced.
pub fn read_body(state_dir: &Path, entry: &HistoryEntry) -> Option<String> {
    let bytes = std::fs::read(body_file(state_dir, entry)).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// Keeps the newest entries and removes the files of the older ones.
fn prune(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut ids: Vec<u64> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name();
            let name = name.to_str()?;
            name.strip_suffix(".json")?.parse().ok()
        })
        .collect();
    ids.sort_unstable_by(|left, right| right.cmp(left));
    for (index, id) in ids.into_iter().enumerate() {
        if index < crate::limits::HISTORY {
            continue;
        }
        std::fs::remove_file(entry_file(dir, id)).ok();
        std::fs::remove_file(dir.join(format!("{id}.body"))).ok();
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{history_dir, load_history, read_body, save_response};
    use crate::execute::Response;
    use crate::parse::Method;
    use crate::variables::PreparedRequest;

    fn request() -> PreparedRequest {
        PreparedRequest {
            name: Some("Create".to_owned()),
            method: Method::Post,
            url: "http://localhost/api".to_owned(),
            headers: Vec::new(),
            body: None,
            line: 0,
        }
    }

    fn response(body: &str) -> Response {
        Response {
            status: 200,
            reason: "OK".to_owned(),
            headers: Vec::new(),
            body: body.as_bytes().to_vec(),
            elapsed: Duration::from_millis(12),
        }
    }

    fn state(name: &str) -> std::path::PathBuf {
        test_support::scratch(&format!("http-history-{name}"))
    }

    #[test]
    fn an_answer_is_kept_with_its_body() {
        let state = state("one");
        let entry = save_response(&state, &request(), &response("{\"id\":1}")).unwrap();
        assert_eq!(entry.status_line(), "200 OK");
        assert_eq!(entry.size, 8);
        assert_eq!(entry.label(), "Create");
        let entries = load_history(&state);
        assert_eq!(entries.as_slice(), std::slice::from_ref(&entry));
        assert_eq!(read_body(&state, &entry).as_deref(), Some("{\"id\":1}"));
        assert!(history_dir(&state).is_dir());
    }

    #[test]
    fn only_the_newest_answers_stay() {
        let state = state("limit");
        for number in 0..(crate::limits::HISTORY + 3) {
            save_response(&state, &request(), &response(&format!("answer {number}"))).unwrap();
        }
        let entries = load_history(&state);
        assert_eq!(entries.len(), crate::limits::HISTORY);
        assert!(
            read_body(&state, &entries[0])
                .unwrap()
                .starts_with("answer "),
            "the newest entry comes first"
        );
        let files = std::fs::read_dir(history_dir(&state)).unwrap().count();
        assert_eq!(
            files,
            crate::limits::HISTORY * 2,
            "no body without an entry"
        );
    }
}
