//! What the gateway needs from the rest of the IDE. `workspace-core` and the language layer
//! implement this when they exist, tests use a fake.

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// Which session a call belongs to. Comes from the token of the connection, never from the agent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionScope {
    pub session: String,
    pub project: PathBuf,
    pub cwd: PathBuf,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct EditorContext {
    pub file: Option<PathBuf>,
    pub line: Option<u32>,
    pub column: Option<u32>,
    pub selection: Option<String>,
    pub open_files: Vec<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct SymbolQuery {
    pub query: String,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: usize,
    /// The continuation token of a truncated answer.
    #[serde(default)]
    pub after: Option<String>,
}

fn default_limit() -> usize {
    20
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    Certain,
    Likely,
    Unknown,
}

impl Confidence {
    pub fn name(self) -> &'static str {
        match self {
            Self::Certain => "certain",
            Self::Likely => "likely",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SymbolHit {
    pub name: String,
    pub kind: String,
    pub path: PathBuf,
    /// 1-based, the line of `content`.
    pub line: u32,
    pub content: String,
    pub before: Vec<String>,
    pub after: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SymbolPage {
    pub hits: Vec<SymbolHit>,
    pub source: String,
    pub confidence: Confidence,
    /// Present when more hits exist.
    pub next: Option<String>,
}

/// A place in a file of the project. The gateway resolves the path against the project and
/// refuses one outside it, so a backend gets an absolute path it may read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Position {
    pub path: PathBuf,
    /// 1-based.
    pub line: u32,
    /// 1-based, in characters.
    pub column: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Location {
    pub path: PathBuf,
    /// 1-based.
    pub line: u32,
    /// The text of that line.
    pub content: String,
}

/// What stands at a position: the symbol, its declaration and its documentation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SymbolInfo {
    pub name: String,
    pub kind: String,
    pub signature: Option<String>,
    pub documentation: Option<String>,
    pub declaration: Option<Location>,
    pub source: String,
    pub confidence: Confidence,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Error,
    Warning,
    Info,
    Hint,
}

impl Severity {
    pub fn name(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Info => "info",
            Self::Hint => "hint",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct DiagnosticsQuery {
    /// One file, or the whole project when absent. Resolved like `Position::path`.
    #[serde(default)]
    pub path: Option<PathBuf>,
    /// The least severe kind to include.
    #[serde(default = "default_severity")]
    pub severity: Severity,
    #[serde(default = "default_limit")]
    pub limit: usize,
    /// The continuation token of a truncated answer.
    #[serde(default)]
    pub after: Option<String>,
}

fn default_severity() -> Severity {
    Severity::Warning
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub path: PathBuf,
    /// 1-based.
    pub line: u32,
    /// 1-based, in characters.
    pub column: u32,
    pub severity: Severity,
    pub message: String,
    pub code: Option<String>,
    /// The text of the line.
    pub content: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiagnosticsPage {
    pub items: Vec<Diagnostic>,
    pub source: String,
    pub confidence: Confidence,
    /// Present when more items exist.
    pub next: Option<String>,
}

/// How much of a stopped debug session `debug_get_state` shows.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DebugStateQuery {
    /// The top frames of the selected thread.
    pub frames: usize,
    /// The variables of the top frame.
    pub variables: usize,
    /// How many levels of children of a variable to follow.
    pub depth: usize,
    /// The longest value to show, in characters.
    pub length: usize,
}

impl Default for DebugStateQuery {
    fn default() -> Self {
        Self {
            frames: 10,
            variables: 20,
            depth: 2,
            length: 200,
        }
    }
}

impl DebugStateQuery {
    /// Keeps an agent from asking for an answer that would not fit a context window.
    pub fn clipped(mut self) -> Self {
        self.frames = self.frames.clamp(1, 100);
        self.variables = self.variables.min(200);
        self.depth = self.depth.min(5);
        self.length = self.length.clamp(20, 2_000);
        self
    }
}

/// The newest debug snapshot, filled by the window and read by a worker thread.
/// A source answers `None` when no session runs in its project.
pub trait DebugSnapshotSource: Send + Sync {
    fn snapshot(&self) -> Option<Arc<dap_client::Snapshot>>;
}

pub trait ToolBackend: Send + Sync {
    fn editor_context(&self, scope: &SessionScope) -> Result<EditorContext, ToolError>;

    fn find_symbol(
        &self,
        scope: &SessionScope,
        query: &SymbolQuery,
    ) -> Result<SymbolPage, ToolError>;

    fn symbol_info(
        &self,
        scope: &SessionScope,
        position: &Position,
    ) -> Result<SymbolInfo, ToolError>;

    fn diagnostics(
        &self,
        scope: &SessionScope,
        query: &DiagnosticsQuery,
    ) -> Result<DiagnosticsPage, ToolError>;

    /// The state of the debug session of the project, `None` when none is running.
    fn debug_snapshot(&self, _scope: &SessionScope) -> Option<Arc<dap_client::Snapshot>> {
        None
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolError {
    UnknownTool(String),
    BadInput(String),
    Denied(String),
    Failed(String),
}

impl fmt::Display for ToolError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::UnknownTool(name) => write!(f, "unknown tool {name}"),
            Self::BadInput(message) => write!(f, "bad input: {message}"),
            Self::Denied(message) => write!(f, "denied: {message}"),
            Self::Failed(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for ToolError {}

/// Until the editor and the language layer answer, a call says so instead of making things up.
pub struct Unavailable;

impl ToolBackend for Unavailable {
    fn editor_context(&self, _: &SessionScope) -> Result<EditorContext, ToolError> {
        Err(ToolError::Failed(
            "the editor context is not connected yet".to_string(),
        ))
    }

    fn find_symbol(&self, _: &SessionScope, _: &SymbolQuery) -> Result<SymbolPage, ToolError> {
        Err(ToolError::Failed(
            "the symbol index is not connected yet".to_string(),
        ))
    }

    fn symbol_info(&self, _: &SessionScope, _: &Position) -> Result<SymbolInfo, ToolError> {
        Err(ToolError::Failed(
            "the language server is not connected yet".to_string(),
        ))
    }

    fn diagnostics(
        &self,
        _: &SessionScope,
        _: &DiagnosticsQuery,
    ) -> Result<DiagnosticsPage, ToolError> {
        Err(ToolError::Failed(
            "the language server is not connected yet".to_string(),
        ))
    }
}
