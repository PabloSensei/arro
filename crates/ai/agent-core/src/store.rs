//! The session list that survives a restart: what the sidebar shows before anything runs.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::runtime::SessionStatus;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRecord {
    pub id: String,
    pub project: PathBuf,
    pub cwd: PathBuf,
    pub runtime: String,
    pub title: String,
    pub status: SessionStatus,
    pub created_ms: u64,
    pub updated_ms: u64,
}

pub struct SessionStore {
    path: PathBuf,
    records: Vec<SessionRecord>,
}

impl SessionStore {
    /// An unreadable file counts as empty: the list is a convenience, not the user's data.
    /// What was alive when the IDE stopped is not alive now.
    pub fn open(path: &Path) -> Self {
        let text = fs::read_to_string(path).unwrap_or_default();
        let mut records: Vec<SessionRecord> = serde_json::from_str(&text).unwrap_or_default();
        for record in &mut records {
            let live = matches!(
                record.status,
                SessionStatus::Starting | SessionStatus::Running | SessionStatus::WaitingForUser
            );
            if live {
                record.status = SessionStatus::Done;
            }
        }
        Self {
            path: path.to_path_buf(),
            records,
        }
    }

    pub fn records(&self) -> &[SessionRecord] {
        &self.records
    }

    /// Sessions of one project, the latest first.
    pub fn for_project(&self, project: &Path) -> Vec<&SessionRecord> {
        let mut found: Vec<_> = self
            .records
            .iter()
            .filter(|record| record.project == project)
            .collect();
        found.sort_by_key(|record| std::cmp::Reverse(record.updated_ms));
        found
    }

    pub fn upsert(&mut self, record: SessionRecord) -> io::Result<()> {
        match self.records.iter_mut().find(|known| known.id == record.id) {
            Some(known) => *known = record,
            None => self.records.push(record),
        }
        self.save()
    }

    pub fn set_status(&mut self, id: &str, status: SessionStatus, now_ms: u64) -> io::Result<()> {
        if let Some(record) = self.records.iter_mut().find(|record| record.id == id) {
            record.status = status;
            record.updated_ms = now_ms;
        }
        self.save()
    }

    pub fn remove(&mut self, id: &str) -> io::Result<()> {
        self.records.retain(|record| record.id != id);
        self.save()
    }

    /// Through a temporary file: a crash in the middle must not eat the list.
    fn save(&self) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temporary = self.path.with_extension("tmp");
        let text = serde_json::to_string_pretty(&self.records)?;
        fs::write(&temporary, text)?;
        fs::rename(&temporary, &self.path)
    }
}

#[cfg(test)]
mod tests {
    use std::env::temp_dir;

    use super::*;

    struct TempFile(PathBuf);

    impl TempFile {
        fn new(name: &str) -> Self {
            let name = format!("sessions-{name}-{}.json", std::process::id());
            let path = temp_dir().join(name);
            let _ = fs::remove_file(&path);
            Self(path)
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

    fn record(id: &str, project: &str, updated_ms: u64) -> SessionRecord {
        SessionRecord {
            id: id.to_string(),
            project: project.into(),
            cwd: project.into(),
            runtime: "mock".to_string(),
            title: "title".to_string(),
            status: SessionStatus::Running,
            created_ms: 1,
            updated_ms,
        }
    }

    #[test]
    fn sessions_survive_a_restart_as_finished() {
        let file = TempFile::new("restart");
        let mut store = SessionStore::open(&file.0);
        store.upsert(record("a", "one", 10)).expect("save");

        let reopened = SessionStore::open(&file.0);

        assert_eq!(reopened.records().len(), 1);
        assert_eq!(reopened.records()[0].status, SessionStatus::Done);
    }

    #[test]
    fn a_project_lists_its_own_sessions_latest_first() {
        let file = TempFile::new("project");
        let mut store = SessionStore::open(&file.0);
        store.upsert(record("old", "one", 10)).expect("save");
        store.upsert(record("new", "one", 20)).expect("save");
        store.upsert(record("other", "two", 30)).expect("save");

        let ids: Vec<_> = store
            .for_project(Path::new("one"))
            .iter()
            .map(|record| record.id.as_str())
            .collect();

        assert_eq!(ids, ["new", "old"]);
    }

    #[test]
    fn a_broken_file_starts_an_empty_list() {
        let file = TempFile::new("broken");
        fs::write(&file.0, "{not json").expect("write");

        assert!(SessionStore::open(&file.0).records().is_empty());
    }

    #[test]
    fn status_changes_and_removal_are_saved() {
        let file = TempFile::new("status");
        let mut store = SessionStore::open(&file.0);
        store.upsert(record("a", "one", 10)).expect("save");
        store.upsert(record("b", "one", 10)).expect("save");

        store
            .set_status("a", SessionStatus::Error, 20)
            .expect("save");
        store.remove("b").expect("save");

        let reopened = SessionStore::open(&file.0);
        assert_eq!(reopened.records().len(), 1);
        assert_eq!(reopened.records()[0].status, SessionStatus::Error);
        assert_eq!(reopened.records()[0].updated_ms, 20);
    }
}
