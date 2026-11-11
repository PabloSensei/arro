use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use crossbeam_channel::Receiver;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use terminal::TerminalSession;

/// Sidebar states. `WaitingForUser` also covers screens only a person can answer, such as login.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Starting,
    Running,
    WaitingForUser,
    Done,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

#[derive(Clone, Debug, PartialEq)]
pub enum AgentEvent {
    Status(SessionStatus),
    Message {
        role: Role,
        text: String,
    },
    TextDelta(String),
    ToolCall {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        id: String,
        output: String,
    },
    PermissionRequest {
        id: String,
        tool: String,
        description: String,
    },
    TurnStarted,
    /// The agent stopped working on the last prompt. Accuracy depends on the runtime.
    TurnEnded,
    /// Output stopped after a burst, and files may have settled. A user typing into a TUI makes
    /// no turns the IDE can see, this is the checkpoint for that.
    Idle,
    Title(String),
    /// Something the user may want to be told about, a terminal bell for one.
    Notification(String),
    Error(String),
    Exited(Option<u32>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PermissionReply {
    AllowOnce,
    AllowAlways,
    Deny,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SurfaceKind {
    Transcript,
    Terminal,
}

/// A TUI does not pretend to be a transcript: the UI draws its terminal.
#[derive(Clone)]
pub enum Surface {
    Transcript,
    Terminal(Arc<TerminalSession>),
}

impl Surface {
    pub fn kind(&self) -> SurfaceKind {
        match self {
            Self::Transcript => SurfaceKind::Transcript,
            Self::Terminal(_) => SurfaceKind::Terminal,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RuntimeCaps {
    pub streaming: bool,
    pub permissions: bool,
    pub resume: bool,
    pub modes: bool,
    pub surface: SurfaceKind,
    /// `prompt` works. A harness without a known input box takes input from the user only.
    pub prompts: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentError {
    Unsupported(&'static str),
    Closed,
    Busy,
    Io(String),
}

impl fmt::Display for AgentError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::Unsupported(what) => write!(f, "not supported: {what}"),
            Self::Closed => f.write_str("the session is closed"),
            Self::Busy => f.write_str("the session queue is full"),
            Self::Io(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for AgentError {}

impl From<std::io::Error> for AgentError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error.to_string())
    }
}

/// The MCP server of the IDE as a session sees it. The token lives in memory only.
#[derive(Clone)]
pub struct ToolEndpoint {
    pub url: String,
    pub token: String,
}

impl fmt::Debug for ToolEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let url = &self.url;
        write!(f, "ToolEndpoint {{ url: {url:?} }}")
    }
}

#[derive(Clone, Debug)]
pub struct SessionSpec {
    pub project: PathBuf,
    pub cwd: PathBuf,
    pub cols: u16,
    pub rows: u16,
    pub tools: Option<ToolEndpoint>,
}

pub trait AgentRuntime: Send + Sync {
    fn name(&self) -> &str;

    fn caps(&self) -> RuntimeCaps;

    fn start(&self, spec: SessionSpec) -> Result<Box<dyn AgentSession>, AgentError>;

    fn resume(&self, spec: SessionSpec, id: &str) -> Result<Box<dyn AgentSession>, AgentError> {
        let _ = (spec, id);
        Err(AgentError::Unsupported("resume"))
    }
}

/// Every call returns at once, the work happens behind the event stream. Safe on the UI thread.
pub trait AgentSession: Send + Sync {
    fn surface(&self) -> Surface;

    /// There is one stream per session, a second reader would steal events.
    fn events(&self) -> Receiver<AgentEvent>;

    /// Queues a message. A busy agent gets it when the current turn ends.
    fn prompt(&self, text: &str) -> Result<(), AgentError>;

    fn cancel(&self) -> Result<(), AgentError>;

    fn respond(&self, request: &str, reply: PermissionReply) -> Result<(), AgentError> {
        let _ = (request, reply);
        Err(AgentError::Unsupported("permission replies"))
    }

    fn set_config(&self, key: &str, value: &str) -> Result<(), AgentError> {
        let _ = (key, value);
        Err(AgentError::Unsupported("configuration"))
    }

    /// Ends the session and every process in it.
    fn close(&self);
}
