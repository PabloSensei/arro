//! Who called what and how it went. Neither inputs nor outputs are written: they may hold source
//! code and secrets of the project.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

const RECENT: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AuditEntry {
    pub time_ms: u64,
    pub session: String,
    pub tool: String,
    pub class: &'static str,
    pub decision: &'static str,
    pub ok: bool,
}

#[derive(Default)]
struct State {
    file: Option<File>,
    recent: VecDeque<AuditEntry>,
}

/// In memory by default, on disk when opened with a path.
#[derive(Default)]
pub struct AuditLog {
    state: Mutex<State>,
}

impl AuditLog {
    pub fn open(path: &Path) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        let state = State {
            file: Some(file),
            recent: VecDeque::new(),
        };
        Ok(Self {
            state: Mutex::new(state),
        })
    }

    pub(crate) fn record(
        &self,
        session: &str,
        tool: &str,
        class: &'static str,
        decision: &'static str,
        ok: bool,
    ) {
        let since_epoch = SystemTime::now().duration_since(UNIX_EPOCH);
        let entry = AuditEntry {
            time_ms: since_epoch.map_or(0, |time| time.as_millis() as u64),
            session: session.to_string(),
            tool: tool.to_string(),
            class,
            decision,
            ok,
        };
        let mut state = self.state.lock().unwrap();
        if let Some(file) = &mut state.file
            && let Ok(line) = serde_json::to_string(&entry)
        {
            let _ = writeln!(file, "{line}");
        }
        if state.recent.len() == RECENT {
            state.recent.pop_front();
        }
        state.recent.push_back(entry);
    }

    pub fn recent(&self) -> Vec<AuditEntry> {
        let state = self.state.lock().unwrap();
        state.recent.iter().cloned().collect()
    }
}
