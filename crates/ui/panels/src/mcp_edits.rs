//! The edits an MCP client asks the window for: a proposed file the user decides hunk by
//! hunk, a rename the user confirms, and the file actions that run at once. The tool call
//! waits on its own thread in [`EditRequests::ask`]; the panel takes the request on the UI
//! thread, shows it and answers it.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use lang_api::WorkspaceChange;
use tool_gateway::EditOutcome;
use workspace::Workspace;

/// How long a tool waits for the user before it says that nobody decided.
pub(crate) const DECISION_TIMEOUT: Duration = Duration::from_secs(120);

/// What the user has to do for one tool call.
#[derive(Clone, Debug)]
pub enum EditKind {
    /// The disk text of a file and the text the agent offers. Nothing is written until
    /// every hunk is decided.
    Propose {
        path: PathBuf,
        base: Option<String>,
        proposed: String,
    },
    /// The edits of a rename, which the window applies after the user confirms them.
    Rename {
        new_name: String,
        change: WorkspaceChange,
    },
    /// Formatting or Organize Imports: applied at once, with no question of its own.
    Apply { change: WorkspaceChange },
}

/// What became of one request. A request that times out says so and leaves the file as it
/// was; a decision that comes later still reaches the file.
#[derive(Clone, Debug)]
pub enum EditReply {
    Proposal {
        outcome: EditOutcome,
        text: Option<String>,
        hunks: usize,
        kept: usize,
    },
    Rename {
        /// Relative to the project, with forward slashes.
        files: Vec<PathBuf>,
        edits: usize,
    },
    Applied {
        changed: bool,
    },
    /// The user said no to a preview.
    Cancelled,
    NoDecision,
    Failed(String),
}

/// A request the panel has not taken yet.
#[derive(Clone, Debug)]
pub struct EditRequest {
    pub id: u64,
    pub kind: EditKind,
}

struct State {
    next_id: u64,
    queue: VecDeque<EditRequest>,
    answers: HashMap<u64, Option<EditReply>>,
}

/// The requests of the tools of one window and the threads that wait on them.
pub struct EditRequests {
    state: Mutex<State>,
    answered: Condvar,
    timeout: Duration,
}

impl EditRequests {
    pub fn new() -> std::sync::Arc<Self> {
        Self::with_timeout(DECISION_TIMEOUT)
    }

    /// For tests that should not wait two minutes for a decision nobody makes.
    pub fn with_timeout(timeout: Duration) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            state: Mutex::new(State {
                next_id: 0,
                queue: VecDeque::new(),
                answers: HashMap::new(),
            }),
            answered: Condvar::new(),
            timeout,
        })
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    /// Puts the request to the window and waits for its answer. Blocks, so it belongs on
    /// the worker thread of the tool call.
    pub fn ask(&self, kind: EditKind) -> EditReply {
        let mut state = self.lock();
        let id = state.next_id;
        state.next_id += 1;
        state.queue.push_back(EditRequest { id, kind });
        state.answers.insert(id, None);
        let deadline = Instant::now() + self.timeout;
        loop {
            let answered = state
                .answers
                .get(&id)
                .is_some_and(|answer| answer.is_some());
            if answered {
                break;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                state.answers.remove(&id);
                return EditReply::NoDecision;
            }
            let waited = self
                .answered
                .wait_timeout(state, left)
                .unwrap_or_else(|poison| poison.into_inner());
            state = waited.0;
        }
        state
            .answers
            .remove(&id)
            .flatten()
            .unwrap_or(EditReply::NoDecision)
    }

    /// The requests that arrived since the last frame.
    pub fn take(&self) -> Vec<EditRequest> {
        let mut state = self.lock();
        state.queue.drain(..).collect()
    }

    /// Answers one request. A request whose thread already timed out is ignored.
    pub fn resolve(&self, id: u64, reply: EditReply) {
        let mut state = self.lock();
        if let Some(slot) = state.answers.get_mut(&id) {
            *slot = Some(reply);
            self.answered.notify_all();
        }
    }
}

/// The buttons of the preview of a rename: the change reaches the files only on Apply.
pub(crate) fn bind_actions(workspace: &mut Workspace) {
    for (id, title, apply) in [
        ("ai.mcp.rename-apply", "Apply Rename", true),
        ("ai.mcp.rename-cancel", "Cancel Rename", false),
    ] {
        workspace.register_command(id, title, move |_, _, cx| {
            let Some(panel) = cx.try_global::<crate::agents::ActiveAgents>() else {
                return;
            };
            let panel = panel.0.clone();
            panel.update(cx, |panel, cx| panel.answer_rename(apply, cx));
        });
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use lang_api::WorkspaceChange;

    use super::{DECISION_TIMEOUT, EditKind, EditReply, EditRequests};

    #[test]
    fn the_window_default_is_two_minutes() {
        assert_eq!(DECISION_TIMEOUT, Duration::from_secs(120));
    }

    #[test]
    fn an_answer_reaches_the_waiting_call() {
        let requests = EditRequests::with_timeout(Duration::from_secs(10));
        let waiting = requests.clone();
        let ask = std::thread::spawn(move || {
            waiting.ask(EditKind::Apply {
                change: WorkspaceChange::default(),
            })
        });
        let started = std::time::Instant::now();
        let request = loop {
            let request = requests.take().into_iter().next();
            if let Some(request) = request {
                break request;
            }
            assert!(started.elapsed() < Duration::from_secs(10), "no request");
            std::thread::sleep(Duration::from_millis(5));
        };

        requests.resolve(request.id, EditReply::Applied { changed: true });

        assert!(matches!(
            ask.join().expect("the thread of the call"),
            EditReply::Applied { changed: true }
        ));
    }

    #[test]
    fn a_decision_nobody_makes_is_no_decision() {
        let requests = EditRequests::with_timeout(Duration::from_millis(30));
        let waiting = Arc::clone(&requests);
        let ask = std::thread::spawn(move || {
            waiting.ask(EditKind::Apply {
                change: WorkspaceChange::default(),
            })
        });

        assert!(matches!(
            ask.join().expect("the thread of the call"),
            EditReply::NoDecision
        ));
    }
}
