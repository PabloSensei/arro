//! The bridge between the MCP database tools and the Database window. A tool runs on the
//! thread of the server, so it leaves a job here and waits; the Database panel takes the
//! job in its poll and answers from its connections, its schema tree and its console.
//! Nothing in this module knows a language or a driver: it speaks the neutral types of
//! `tool_gateway`.

use std::collections::VecDeque;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use tool_gateway::{DatabaseAnswer, DatabaseControl, DatabaseQuery, SessionScope, ToolError};

/// How long a tool waits for the Database window. A statement of a big table takes its
/// time, and the window polls the queue while it is open.
const ANSWER_WAIT: Duration = Duration::from_secs(600);

/// A job the Database window took and will answer; a dropped sender means the window went
/// away.
pub(crate) struct Job {
    pub(crate) query: DatabaseQuery,
    pub(crate) reply: mpsc::Sender<Result<DatabaseAnswer, String>>,
}

impl Job {
    pub(crate) fn answer(self, answer: DatabaseAnswer) {
        let _ = self.reply.send(Ok(answer));
    }

    pub(crate) fn fail(self, message: impl Into<String>) {
        let _ = self.reply.send(Err(message.into()));
    }
}

#[derive(Default)]
struct State {
    waiting: VecDeque<Job>,
}

/// The queue of the window: every job is answered by the poll that can finish it.
#[derive(Default)]
pub(crate) struct DatabaseBridge {
    state: Mutex<State>,
}

impl DatabaseBridge {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// The next job the window has to take.
    pub(crate) fn take_job(&self) -> Option<Job> {
        self.lock().waiting.pop_front()
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    fn submit(&self, query: DatabaseQuery) -> Result<DatabaseAnswer, ToolError> {
        let (reply, answered) = mpsc::channel();
        self.lock().waiting.push_back(Job { query, reply });
        match answered.recv_timeout(ANSWER_WAIT) {
            Ok(Ok(answer)) => Ok(answer),
            Ok(Err(message)) => Err(ToolError::Failed(message)),
            Err(RecvTimeoutError::Timeout) => Err(ToolError::Failed(
                "the Database window did not answer in time".to_string(),
            )),
            Err(RecvTimeoutError::Disconnected) => Err(ToolError::Failed(
                "the Database window is closed".to_string(),
            )),
        }
    }
}

impl DatabaseControl for DatabaseBridge {
    fn call(
        &self,
        _scope: &SessionScope,
        query: &DatabaseQuery,
    ) -> Result<DatabaseAnswer, ToolError> {
        self.submit(query.clone())
    }
}
