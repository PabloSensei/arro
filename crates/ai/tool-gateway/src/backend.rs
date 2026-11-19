//! What the gateway needs from the rest of the IDE. `workspace-core` and the language layer
//! implement this when they exist, tests use a fake.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::database_tools::{DatabaseAnswer, DatabaseQuery};
use crate::xdebug::{DebugAnswer, DebugQuery};

/// Which session a call belongs to. Comes from the token of the connection, never from the agent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionScope {
    pub session: String,
    pub project: PathBuf,
    pub cwd: PathBuf,
}

impl SessionScope {
    /// Whether the session belongs to an ACP agent rather than to a client outside the
    /// window: the window names those sessions `acp-<agent>`.
    pub fn is_acp(&self) -> bool {
        self.session == "acp" || self.session.starts_with("acp-")
    }
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

/// How many lines `read_file` shows at most, so that one answer fits a context window.
pub const READ_LINE_LIMIT: u32 = 2_000;

/// A place where a symbol is used, as `find_usages` shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Usage {
    pub path: PathBuf,
    /// 1-based.
    pub line: u32,
    /// 1-based, in characters.
    pub column: u32,
    /// The text of that line.
    pub content: String,
}

/// The uses of one symbol. `total` counts every use the service knows, `items` holds the
/// page the caller asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsagePage {
    pub items: Vec<Usage>,
    pub total: usize,
    pub clipped: bool,
    pub source: String,
    pub confidence: Confidence,
}

/// The symbol whose uses the agent wants, by a place or by a name.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsagesQuery {
    /// A file of the project, relative to its root or absolute.
    #[serde(default)]
    pub path: Option<PathBuf>,
    #[serde(default)]
    pub line: Option<u32>,
    #[serde(default)]
    pub column: Option<u32>,
    /// The name of a declaration, when no place is given.
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default = "default_page")]
    pub limit: usize,
}

fn default_page() -> usize {
    50
}

impl UsagesQuery {
    pub fn clipped(mut self) -> Self {
        self.limit = self.limit.clamp(1, 200);
        self
    }

    /// The place the query asks about, `None` for a query by name.
    pub fn position(&self) -> Option<(&Path, u32, u32)> {
        let (path, line, column) = (self.path.as_deref()?, self.line?, self.column?);
        Some((path, line, column))
    }
}

/// One type of a hierarchy: its name, what the service says about it and where it stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HierarchyNode {
    pub name: String,
    pub detail: Option<String>,
    pub location: Option<Location>,
    /// 1 for a direct parent or child, more for the levels above or below it.
    pub depth: u32,
}

/// The types above and below the one at a place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypeHierarchy {
    pub supertypes: Vec<HierarchyNode>,
    pub subtypes: Vec<HierarchyNode>,
    pub source: String,
    pub confidence: Confidence,
    pub clipped: bool,
}

/// The calls that reach the method at a place, one level per requested depth.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallHierarchy {
    pub callers: Vec<HierarchyNode>,
    pub source: String,
    pub confidence: Confidence,
    pub clipped: bool,
}

/// The place of the type or method whose hierarchy the agent wants.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TypeHierarchyQuery {
    pub path: PathBuf,
    /// 1-based.
    pub line: u32,
    /// 1-based, in characters.
    pub column: u32,
}

/// The place of the method and how many levels of callers to follow.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CallHierarchyQuery {
    pub path: PathBuf,
    /// 1-based.
    pub line: u32,
    /// 1-based, in characters.
    pub column: u32,
    #[serde(default = "default_depth")]
    pub depth: usize,
}

fn default_depth() -> usize {
    1
}

impl CallHierarchyQuery {
    pub fn clipped(mut self) -> Self {
        self.depth = self.depth.clamp(1, 5);
        self
    }
}

/// One module of the project as `get_project_structure` shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectModule {
    pub name: String,
    /// Relative to the project root when it is inside it.
    pub path: PathBuf,
    pub source_roots: Vec<PathBuf>,
    pub test_roots: Vec<PathBuf>,
    /// `group:artifact:version` of the libraries the module uses.
    pub dependencies: Vec<String>,
    pub dependencies_clipped: bool,
}

/// The modules of the project, the build system and the environment it builds with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectStructure {
    pub modules: Vec<ProjectModule>,
    pub total_modules: usize,
    pub clipped: bool,
    /// The name of the build system, such as the tool that reads the project.
    pub build_system: String,
    pub build_system_version: Option<String>,
    /// Whether the build system resolved the dependencies or only scanned the files.
    pub resolved: bool,
    /// The runtime the project builds and runs with.
    pub runtime: Option<String>,
    /// The language level of the build.
    pub language_level: Option<String>,
    /// What the build system could not read, if anything.
    pub problems: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectStructureQuery {
    /// How many modules to show, 50 by default.
    #[serde(default = "default_page")]
    pub limit: usize,
}

impl ProjectStructureQuery {
    pub fn clipped(mut self) -> Self {
        self.limit = self.limit.clamp(1, 500);
        self
    }
}

/// One line a text search found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchHit {
    pub path: PathBuf,
    /// 1-based.
    pub line: u32,
    /// 1-based, in characters, of the first match in the line.
    pub column: u32,
    /// The text of the line.
    pub content: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchPage {
    pub items: Vec<SearchHit>,
    pub total: usize,
    pub clipped: bool,
    /// How many files the hits come from.
    pub files: usize,
}

/// A search over the project as Find in Files does it: the index of the project decides
/// which files are read, so the exclusions of the project stay in force.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchQuery {
    #[serde(default, alias = "q")]
    pub pattern: String,
    /// Treat the pattern as a regular expression instead of plain text.
    #[serde(default)]
    pub regex: bool,
    /// File patterns, as Find in Files takes them: `*.txt` or `!*Test.txt`.
    #[serde(default)]
    pub mask: Option<String>,
    /// Folders of the project the search stays in, a leading `!` excludes one.
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default = "default_page")]
    pub limit: usize,
}

impl SearchQuery {
    pub fn clipped(mut self) -> Self {
        self.limit = self.limit.clamp(1, 500);
        self
    }
}

/// What `read_file` shows: the lines the editor has, unsaved edits included.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileText {
    pub path: PathBuf,
    /// The lines without their line ends, joined by `\n`.
    pub text: String,
    pub start_line: u32,
    pub end_line: u32,
    pub total_lines: u32,
    /// The editor holds edits that are not on disk.
    pub dirty: bool,
    /// The range was cut short by the size of an answer or by the end of the file.
    pub clipped: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadFileQuery {
    pub path: PathBuf,
    /// 1-based, 1 by default.
    #[serde(default)]
    pub start_line: Option<u32>,
    /// 1-based and inclusive, [`READ_LINE_LIMIT`] lines by default.
    #[serde(default)]
    pub end_line: Option<u32>,
}

/// The whole text of a file as the editor has it, for the tools that work on all of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileContent {
    pub path: PathBuf,
    /// The lines without their line ends, joined by `\n`.
    pub text: String,
    /// The editor holds edits that are not on disk.
    pub dirty: bool,
}

impl ReadFileQuery {
    /// Fills the range and keeps it inside what one answer may carry.
    pub fn clipped(mut self) -> Self {
        let start = self.start_line.unwrap_or(1).max(1);
        let last = start.saturating_add(READ_LINE_LIMIT - 1);
        let end = self.end_line.unwrap_or(last).max(start).min(last);
        self.start_line = Some(start);
        self.end_line = Some(end);
        self
    }
}

/// One changed file of the repository.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VcsFile {
    /// Relative to the project root, with `/`, when the file is inside it.
    pub path: String,
    /// `modified`, `added`, `deleted`, `renamed`, `untracked` or `conflicted`.
    pub status: String,
    /// The old path of a rename.
    pub original: Option<String>,
}

/// A range of a file the change touches: `before` and `after` are 0-based line ranges.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VcsHunk {
    pub before: (u32, u32),
    pub after: (u32, u32),
    pub removed: Vec<String>,
    pub added: Vec<String>,
}

/// One file against `HEAD`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VcsDiff {
    pub path: String,
    pub hunks: Vec<VcsHunk>,
    pub clipped: bool,
}

/// One replacement of `propose_edit`: the exact text to find and what takes its place.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextReplacement {
    pub old_text: String,
    pub new_text: String,
}

/// A file the agent offers to change: its whole new text, or replacements of the text it
/// has. Exactly one of the two is given.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProposeEditQuery {
    /// A file of the project, relative to its root or absolute.
    pub path: PathBuf,
    /// The text the file should have after the edit.
    #[serde(default)]
    pub new_text: Option<String>,
    /// Replacements of the text on disk, applied one after another.
    #[serde(default)]
    pub replacements: Vec<TextReplacement>,
    /// Removes the file instead of writing it; `new_text` and `replacements` stay empty.
    #[serde(default)]
    pub delete: bool,
}

/// How the user took a proposed edit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditOutcome {
    /// Every hunk was kept.
    Accepted,
    /// No hunk was kept, and the file is as it was.
    Rejected,
    /// Some hunks were kept and some were not.
    Partial,
    /// The user did not decide in time, and nothing was written.
    NoDecision,
}

impl EditOutcome {
    pub fn name(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
            Self::Partial => "partial",
            Self::NoDecision => "no decision",
        }
    }
}

/// What became of a proposed edit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProposedEdit {
    pub path: PathBuf,
    pub outcome: EditOutcome,
    /// The text of the file after the decision, `None` when the user did not decide.
    pub text: Option<String>,
    pub hunks: usize,
    pub kept: usize,
}

/// A symbol to rename, by the place of its name and the name it should get.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenameSymbolQuery {
    /// A file of the project, relative to its root or absolute.
    pub path: PathBuf,
    /// 1-based.
    pub line: u32,
    /// 1-based, in characters.
    pub column: u32,
    pub new_name: String,
}

/// The files a confirmed rename changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenamedSymbol {
    /// Relative to the project, with forward slashes.
    pub files: Vec<PathBuf>,
    pub edits: usize,
}

/// One file an action of the IDE works on.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileActionQuery {
    /// A file of the project, relative to its root or absolute.
    pub path: PathBuf,
}

/// What an action of the IDE did to a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileActionOutcome {
    pub path: PathBuf,
    /// Whether the text of the file changed.
    pub changed: bool,
    pub edits: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VcsChanges {
    pub files: Vec<VcsFile>,
    pub total: usize,
    pub clipped: bool,
    pub branch: Option<String>,
    /// The diff of the requested file, when the query asked for it.
    pub diff: Option<VcsDiff>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VcsQuery {
    /// One file of the project to diff against the last commit.
    #[serde(default)]
    pub path: Option<PathBuf>,
    /// How many changed files to show, 50 by default.
    #[serde(default = "default_page")]
    pub limit: usize,
}

impl VcsQuery {
    pub fn clipped(mut self) -> Self {
        self.limit = self.limit.clamp(1, 500);
        self
    }
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

/// A breakpoint as an agent asks for it. `condition` and `log_message` are optional: a
/// point with a log message prints it and lets the program run on.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BreakpointQuery {
    /// A file of the project, relative to its root or absolute.
    pub path: PathBuf,
    /// 1-based.
    pub line: u32,
    #[serde(default)]
    pub condition: Option<String>,
    #[serde(default)]
    pub log_message: Option<String>,
}

impl BreakpointQuery {
    /// Empty optional fields say the same as absent ones.
    pub fn clipped(mut self) -> Self {
        self.condition = self
            .condition
            .map(|text| text.trim().to_string())
            .filter(|text| !text.is_empty());
        self.log_message = self
            .log_message
            .map(|text| text.trim().to_string())
            .filter(|text| !text.is_empty());
        self
    }
}

/// The place of a breakpoint the agent wants gone.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BreakpointId {
    /// A file of the project, relative to its root or absolute.
    pub path: PathBuf,
    /// 1-based.
    pub line: u32,
}

/// One breakpoint as the window holds it: what the user sees in the editor gutter and in
/// the Breakpoints window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BreakpointInfo {
    pub path: PathBuf,
    /// 1-based.
    pub line: u32,
    pub enabled: bool,
    /// Whether the adapter accepted the place. False while no session runs.
    pub verified: bool,
    pub condition: Option<String>,
    pub log_message: Option<String>,
}

/// What a `debug_run` asks: a test suite or case, or a stored run configuration.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DebugRunRequest {
    pub module: Option<String>,
    /// A class or a file of tests, in the terms the project uses.
    pub suite: Option<String>,
    /// One test of the suite; it needs `suite`.
    pub case: Option<String>,
    /// The name from `list_run_configurations`.
    pub configuration: Option<String>,
    /// How long to wait for the first stop, 30 seconds by default.
    pub wait_seconds: u64,
}

impl DebugRunRequest {
    pub fn clipped(mut self) -> Self {
        if self.wait_seconds == 0 {
            self.wait_seconds = default_wait_seconds();
        }
        self.wait_seconds = self.wait_seconds.clamp(1, 120);
        self
    }
}

fn default_wait_seconds() -> u64 {
    30
}

/// How long `debug_wait` waits for a stop or the end of the session.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DebugWaitQuery {
    pub seconds: u64,
}

impl Default for DebugWaitQuery {
    fn default() -> Self {
        Self { seconds: 10 }
    }
}

impl DebugWaitQuery {
    pub fn clipped(mut self) -> Self {
        self.seconds = self.seconds.clamp(1, 120);
        self
    }
}

/// One thread of the debuggee, as an answer names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DebugThread {
    pub id: i64,
    pub name: String,
}

/// One frame of a stack, with the place when the adapter sent one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DebugFrame {
    /// Zero for the top frame.
    pub index: usize,
    pub name: String,
    pub path: Option<PathBuf>,
    /// 1-based, zero when the adapter did not know the line.
    pub line: u32,
    pub column: u32,
}

/// Where the session is after a run, a wait or a step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DebugSessionState {
    /// `starting`, `running`, `stopped` or `ended`.
    pub state: String,
    /// Why the program stopped, such as `breakpoint`, `step` or `exception`.
    pub reason: Option<String>,
    pub thread: Option<DebugThread>,
    /// The top frame of the stopped thread.
    pub frame: Option<DebugFrame>,
}

impl DebugSessionState {
    pub fn stopped_reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }
}

/// What a `debug_run` answered: the id of the session and where it got to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DebugRunOutcome {
    pub session: String,
    pub state: DebugSessionState,
}

/// How much of a stack `debug_stack` shows.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DebugStackQuery {
    /// The thread by id, the selected one by default.
    pub thread: Option<i64>,
    pub frames: usize,
}

impl Default for DebugStackQuery {
    fn default() -> Self {
        Self {
            thread: None,
            frames: 10,
        }
    }
}

impl DebugStackQuery {
    pub fn clipped(mut self) -> Self {
        self.frames = self.frames.clamp(1, 100);
        self
    }
}

/// The frames of one thread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DebugStack {
    pub thread: Option<DebugThread>,
    /// How many frames the adapter sent.
    pub total: usize,
    pub frames: Vec<DebugFrame>,
}

/// How much of the variables of a frame `debug_variables` shows.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DebugVariablesQuery {
    /// The frame by its index in the stack, zero for the top one.
    pub frame: usize,
    /// How many levels of children of a variable to follow.
    pub depth: usize,
    /// The longest value to show, in characters.
    pub length: usize,
    /// How many variables of a scope to show.
    pub variables: usize,
}

impl Default for DebugVariablesQuery {
    fn default() -> Self {
        Self {
            frame: 0,
            depth: 2,
            length: 200,
            variables: 50,
        }
    }
}

impl DebugVariablesQuery {
    pub fn clipped(mut self) -> Self {
        self.frame = self.frame.min(99);
        self.depth = self.depth.min(5);
        self.length = self.length.clamp(20, 2_000);
        self.variables = self.variables.clamp(1, 200);
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DebugVariable {
    pub name: String,
    pub value: String,
    pub type_name: String,
    pub children: Vec<DebugVariable>,
}

/// One group of variables of a frame, such as the locals or the static fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DebugScope {
    pub name: String,
    pub variables: Vec<DebugVariable>,
}

/// The variables of one frame, with the children the caller asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DebugVariables {
    pub scopes: Vec<DebugScope>,
    /// How many variables the answer carries at every level.
    pub total: usize,
    pub clipped: bool,
}

/// An expression to evaluate in a frame. Evaluation may change the program, so it asks
/// the user.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DebugEvaluateQuery {
    pub expression: String,
    /// The frame by its index in the stack, zero for the top one.
    #[serde(default)]
    pub frame: Option<usize>,
}

/// What an expression gave.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DebugValue {
    pub expression: String,
    pub value: String,
    pub type_name: String,
}

/// Which way a step goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DebugStep {
    Over,
    Into,
    Out,
    Continue,
}

impl DebugStep {
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim() {
            "over" => Some(Self::Over),
            "into" => Some(Self::Into),
            "out" => Some(Self::Out),
            "continue" => Some(Self::Continue),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Over => "over",
            Self::Into => "into",
            Self::Out => "out",
            Self::Continue => "continue",
        }
    }
}

/// A step the agent asks for, with how long to wait for the next stop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DebugStepQuery {
    pub step: DebugStep,
    /// How long to wait for the next stop, 10 seconds by default.
    pub seconds: u64,
}

impl DebugStepQuery {
    pub fn clipped(mut self) -> Self {
        self.seconds = self.seconds.clamp(1, 120);
        self
    }
}

/// What a call tree follows: the calls that reach a symbol, or the calls it makes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CallDirection {
    IncomingCalls,
    OutgoingCalls,
}

impl CallDirection {
    pub fn name(self) -> &'static str {
        match self {
            Self::IncomingCalls => "INCOMING_CALLS",
            Self::OutgoingCalls => "OUTGOING_CALLS",
        }
    }
}

/// A call tree the agent asks for by the fully qualified name of a symbol.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnalyzeCallsQuery {
    #[serde(rename = "symbolFqn")]
    pub symbol_fqn: String,
    #[serde(rename = "analysisKind")]
    pub analysis_kind: CallDirection,
    #[serde(default = "default_call_depth")]
    pub depth: usize,
    #[serde(default = "default_call_children", rename = "maxChildren")]
    pub max_children: usize,
    #[serde(default = "default_call_nodes", rename = "maxNodes")]
    pub max_nodes: usize,
    /// The path of the node the answer starts at; the root is empty.
    #[serde(default, rename = "treePath")]
    pub tree_path: Vec<usize>,
    #[serde(default, rename = "childOffset")]
    pub child_offset: usize,
    #[serde(default)]
    pub timeout: Option<u64>,
}

fn default_call_depth() -> usize {
    5
}

fn default_call_children() -> usize {
    50
}

fn default_call_nodes() -> usize {
    1_000
}

impl AnalyzeCallsQuery {
    pub fn clipped(mut self) -> Self {
        self.depth = self.depth.min(20);
        self.max_children = self.max_children.clamp(1, 200);
        self.max_nodes = self.max_nodes.clamp(1, 5_000);
        self
    }
}

/// One node of a call tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallNode {
    pub name: String,
    pub detail: Option<String>,
    /// The file the call stands in, when the language service named it.
    pub file_path: Option<PathBuf>,
    /// 1-based.
    pub line: u32,
    /// The place of the node among its siblings: the root is empty.
    pub tree_path: Vec<usize>,
    pub children: Vec<CallNode>,
}

/// The call tree of one symbol.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallTree {
    pub symbol: String,
    pub direction: CallDirection,
    pub root: Option<CallNode>,
    /// More nodes exist than the answer carries.
    pub more: bool,
}

/// A file search: a glob, optional folders and exclusions, and a page size.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PathQuery {
    #[serde(rename = "q")]
    pub pattern: String,
    /// Folders the search visits, a leading `!` excludes one.
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default, rename = "includeExcluded")]
    pub include_excluded: bool,
    #[serde(default = "default_path_limit")]
    pub limit: usize,
}

fn default_path_limit() -> usize {
    100
}

impl PathQuery {
    pub fn clipped(mut self) -> Self {
        self.limit = self.limit.clamp(1, 1_000);
        self
    }
}

/// A file the search found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PathPage {
    pub items: Vec<PathBuf>,
    pub total: usize,
    pub clipped: bool,
}

/// One file of a directory tree, a folder when it has children.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeEntry {
    /// Relative to the project, with `/`.
    pub path: String,
    pub name: String,
    pub directory: bool,
    /// The depth from the folder the answer starts at, zero for its children.
    pub depth: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectoryTree {
    pub entries: Vec<TreeEntry>,
    /// A folder below the requested depth was not read.
    pub clipped: bool,
}

/// One gutter mark of a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunPointQuery {
    pub file_path: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunPointInfo {
    /// `program` or `test`.
    pub kind: String,
    pub name: String,
    pub container: String,
    pub file: PathBuf,
    /// 1-based.
    pub line: u32,
    pub framework: bool,
}

/// One changed file with the codes of the index and the working tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitFileEntry {
    /// Relative to the repository root, with `/`.
    pub path: String,
    /// The porcelain code of the index side: `M`, `A`, `D`, `R`, `?` and so on.
    pub index: String,
    /// The porcelain code of the working tree side.
    pub worktree: String,
}

/// The status of one repository of the project.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitRepositoryStatus {
    /// Relative to the project, empty for the root repository.
    pub path: String,
    pub branch: Option<String>,
    pub files: Vec<GitFileEntry>,
    pub total: usize,
    pub clipped: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitStatus {
    pub repositories: Vec<GitRepositoryStatus>,
}

/// How much of a repository status the agent wants.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GitStatusQuery {
    #[serde(rename = "repositoryPathRelativeToProject")]
    pub repository_path_relative_to_project: Option<PathBuf>,
    #[serde(rename = "includeUntracked")]
    pub include_untracked: bool,
    #[serde(rename = "includeIgnored")]
    pub include_ignored: bool,
    pub limit: usize,
}

impl Default for GitStatusQuery {
    fn default() -> Self {
        Self {
            repository_path_relative_to_project: None,
            include_untracked: true,
            include_ignored: false,
            limit: 200,
        }
    }
}

impl GitStatusQuery {
    pub fn clipped(mut self) -> Self {
        self.limit = self.limit.clamp(1, 1_000);
        self
    }
}

/// One command for the terminal of the window.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalQuery {
    pub command: String,
    /// Run the line through the shell; a plain program otherwise. True by default.
    #[serde(default = "default_true", rename = "executeInShell")]
    pub execute_in_shell: bool,
    #[serde(default, rename = "reuseExistingTerminalWindow")]
    pub reuse_existing_terminal_window: bool,
    #[serde(default)]
    pub timeout: Option<u64>,
    #[serde(default = "default_terminal_lines", rename = "maxLinesCount")]
    pub max_lines_count: usize,
    /// `start`, `middle`, `end` or `none`.
    #[serde(default = "default_truncate", rename = "truncateMode")]
    pub truncate_mode: String,
}

fn default_true() -> bool {
    true
}

fn default_terminal_lines() -> usize {
    2_000
}

fn default_truncate() -> String {
    "end".to_string()
}

impl TerminalQuery {
    pub fn clipped(mut self) -> Self {
        self.max_lines_count = self.max_lines_count.clamp(1, 20_000);
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalOutput {
    pub output: String,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    /// The output was longer than `maxLinesCount` and was cut.
    pub truncated: bool,
}

/// What `execute_run_configuration` asks for.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ExecuteRunQuery {
    #[serde(rename = "configurationName")]
    pub configuration_name: Option<String>,
    /// A temporary configuration at this place, when no name is given.
    #[serde(rename = "filePath")]
    pub file_path: Option<PathBuf>,
    /// 1-based.
    pub line: Option<u32>,
    /// Milliseconds.
    pub timeout: Option<u64>,
    #[serde(rename = "waitForExit")]
    pub wait_for_exit: bool,
    #[serde(rename = "programArguments")]
    pub program_arguments: Option<String>,
    #[serde(rename = "workingDirectory")]
    pub working_directory: Option<PathBuf>,
    pub envs: Vec<String>,
}

impl Default for ExecuteRunQuery {
    fn default() -> Self {
        Self {
            configuration_name: None,
            file_path: None,
            line: None,
            timeout: None,
            wait_for_exit: true,
            program_arguments: None,
            working_directory: None,
            envs: Vec::new(),
        }
    }
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

    /// The uses of a symbol, by a place or by a name.
    fn find_usages(
        &self,
        _scope: &SessionScope,
        _query: &UsagesQuery,
    ) -> Result<UsagePage, ToolError> {
        Err(not_connected("the symbol index"))
    }

    /// The types above and below the one at a place.
    fn type_hierarchy(
        &self,
        _scope: &SessionScope,
        _query: &TypeHierarchyQuery,
    ) -> Result<TypeHierarchy, ToolError> {
        Err(not_connected("the language server"))
    }

    /// The calls that reach the method at a place.
    fn call_hierarchy(
        &self,
        _scope: &SessionScope,
        _query: &CallHierarchyQuery,
    ) -> Result<CallHierarchy, ToolError> {
        Err(not_connected("the language server"))
    }

    /// The modules, roots, build system and runtime of the project.
    fn project_structure(
        &self,
        _scope: &SessionScope,
        _query: &ProjectStructureQuery,
    ) -> Result<ProjectStructure, ToolError> {
        Err(not_connected("the project model"))
    }

    /// A text search over the files the project index holds.
    fn search_text(
        &self,
        _scope: &SessionScope,
        _query: &SearchQuery,
    ) -> Result<SearchPage, ToolError> {
        Err(not_connected("the project index"))
    }

    /// The text of a file as the editor sees it, unsaved edits included.
    fn read_file(
        &self,
        _scope: &SessionScope,
        _query: &ReadFileQuery,
    ) -> Result<FileText, ToolError> {
        Err(not_connected("the editor"))
    }

    /// The whole text of a file as the editor sees it, for the tools that need every line.
    fn file_text(&self, _scope: &SessionScope, path: &Path) -> Result<FileContent, ToolError> {
        let text = std::fs::read_to_string(path).map_err(|error| {
            ToolError::Failed(format!("could not read {}: {error}", path.display()))
        })?;
        Ok(FileContent {
            path: path.to_path_buf(),
            text,
            dirty: false,
        })
    }

    /// The files of the project the pattern names, for the file search of an agent.
    fn find_files(&self, scope: &SessionScope, query: &PathQuery) -> Result<PathPage, ToolError> {
        let mut found = Vec::new();
        collect_files(&scope.project, &mut found);
        let mut items: Vec<PathBuf> = found
            .into_iter()
            .filter(|path| {
                let relative = crate::relative_path(&scope.project, path);
                crate::path_allowed(&query.paths, &relative)
                    && crate::matches_glob(&query.pattern, &relative)
            })
            .collect();
        items.sort();
        let total = items.len();
        let limit = query.limit.max(1);
        items.truncate(limit);
        Ok(PathPage {
            items,
            total,
            clipped: total > limit,
        })
    }

    /// The symbols the language service knows by a part of a name.
    fn search_symbols(
        &self,
        scope: &SessionScope,
        query: &SymbolQuery,
    ) -> Result<SymbolPage, ToolError> {
        self.find_symbol(scope, query)
    }

    /// The calls that reach a symbol or the calls it makes.
    fn analyze_calls(
        &self,
        _scope: &SessionScope,
        _query: &AnalyzeCallsQuery,
    ) -> Result<CallTree, ToolError> {
        Err(not_connected("the language server"))
    }

    /// The gutter marks of a file: the programs and the tests it can start.
    fn run_points(
        &self,
        _scope: &SessionScope,
        _query: &RunPointQuery,
    ) -> Result<Vec<RunPointInfo>, ToolError> {
        Err(not_connected("the language"))
    }

    /// Opens a file in an editor of the window.
    fn open_file(&self, _scope: &SessionScope, _path: &Path) -> Result<(), ToolError> {
        Err(not_connected("the window"))
    }

    /// The files open in the editors of the window.
    fn open_files(&self, scope: &SessionScope) -> Result<Vec<PathBuf>, ToolError> {
        Ok(self.editor_context(scope)?.open_files)
    }

    /// The text of a class or a source the language service can show, such as a `jdt://` one.
    fn source_text(&self, _scope: &SessionScope, _uri: &str) -> Result<String, ToolError> {
        Err(ToolError::Failed(
            "no decompiler is connected for this file".to_string(),
        ))
    }

    /// The roots of the repositories of the project.
    fn repositories(&self, _scope: &SessionScope) -> Result<Vec<PathBuf>, ToolError> {
        Err(not_connected("the version control"))
    }

    /// The status of the repositories of the project, with the porcelain codes.
    fn git_status(
        &self,
        _scope: &SessionScope,
        _query: &GitStatusQuery,
    ) -> Result<GitStatus, ToolError> {
        Err(not_connected("the version control"))
    }

    /// Runs a line in the terminal of the window and captures what it printed.
    fn execute_terminal(
        &self,
        _scope: &SessionScope,
        _query: &TerminalQuery,
    ) -> Result<TerminalOutput, ToolError> {
        Err(not_connected("the terminal"))
    }

    /// Starts a configuration made from a place in a file, such as a `main` method.
    fn start_temporary_run(
        &self,
        _scope: &SessionScope,
        _query: &ExecuteRunQuery,
        _client: Option<&str>,
    ) -> Result<String, ToolError> {
        Err(ToolError::Failed(
            "temporary run configurations are not connected yet".to_string(),
        ))
    }

    /// The changed files of the repository and the diff of one of them.
    fn vcs_changes(
        &self,
        _scope: &SessionScope,
        _query: &VcsQuery,
    ) -> Result<VcsChanges, ToolError> {
        Err(not_connected("the version control"))
    }

    /// Offers an edit of a file to the user and waits for the decision. The file is not
    /// written while the user has not decided.
    fn propose_edit(
        &self,
        _scope: &SessionScope,
        _query: &ProposeEditQuery,
    ) -> Result<ProposedEdit, ToolError> {
        Err(not_connected("the editor"))
    }

    /// Renames the symbol at a place through the language service, after the user
    /// confirms the change.
    fn rename_symbol(
        &self,
        _scope: &SessionScope,
        _query: &RenameSymbolQuery,
    ) -> Result<RenamedSymbol, ToolError> {
        Err(not_connected("the language server"))
    }

    /// Sorts and removes the imports of a file as the language service says.
    fn organize_imports(
        &self,
        _scope: &SessionScope,
        _query: &FileActionQuery,
    ) -> Result<FileActionOutcome, ToolError> {
        Err(not_connected("the language server"))
    }

    /// Formats a file as the language service says.
    fn format_file(
        &self,
        _scope: &SessionScope,
        _query: &FileActionQuery,
    ) -> Result<FileActionOutcome, ToolError> {
        Err(not_connected("the language server"))
    }

    /// Builds the project with its own build tool and answers what the build said.
    fn build_project(
        &self,
        _scope: &SessionScope,
        _request: &BuildRequest,
    ) -> Result<BuildOutcome, ToolError> {
        Err(ToolError::Failed(
            "the run window of this project is not connected yet".to_string(),
        ))
    }

    /// Runs tests of the project and answers their results.
    fn run_tests(
        &self,
        _scope: &SessionScope,
        _request: &TestsRequest,
    ) -> Result<TestsOutcome, ToolError> {
        Err(ToolError::Failed(
            "the run window of this project is not connected yet".to_string(),
        ))
    }

    /// The run configurations of the project the user can start.
    fn run_configurations(
        &self,
        _scope: &SessionScope,
    ) -> Result<Vec<RunConfiguration>, ToolError> {
        Err(ToolError::Failed(
            "the run window of this project is not connected yet".to_string(),
        ))
    }

    /// Starts a run configuration and answers the id of the run.
    fn start_run(
        &self,
        _scope: &SessionScope,
        _request: &StartRunRequest,
        _client: Option<&str>,
    ) -> Result<String, ToolError> {
        Err(ToolError::Failed(
            "the run window of this project is not connected yet".to_string(),
        ))
    }

    /// The latest lines of a run and its exit code.
    fn run_output(&self, _scope: &SessionScope, id: &str) -> Result<RunOutput, ToolError> {
        let _ = id;
        Err(ToolError::Failed(
            "the run window of this project is not connected yet".to_string(),
        ))
    }

    /// Stops the program of a run.
    fn stop_run(&self, _scope: &SessionScope, id: &str) -> Result<(), ToolError> {
        let _ = id;
        Err(ToolError::Failed(
            "the run window of this project is not connected yet".to_string(),
        ))
    }

    /// Sets a breakpoint the user sees in the editor and the Breakpoints window.
    fn set_breakpoint(
        &self,
        _scope: &SessionScope,
        _query: &BreakpointQuery,
    ) -> Result<BreakpointInfo, ToolError> {
        Err(not_connected("the debug window"))
    }

    /// Removes a breakpoint of the project.
    fn remove_breakpoint(
        &self,
        _scope: &SessionScope,
        _id: &BreakpointId,
    ) -> Result<(), ToolError> {
        Err(not_connected("the debug window"))
    }

    /// Every breakpoint of the project.
    fn breakpoints(&self, _scope: &SessionScope) -> Result<Vec<BreakpointInfo>, ToolError> {
        Err(not_connected("the debug window"))
    }

    /// Starts a test or a configuration under the debugger.
    fn debug_run(
        &self,
        _scope: &SessionScope,
        _request: &DebugRunRequest,
        _client: Option<&str>,
    ) -> Result<DebugRunOutcome, ToolError> {
        Err(not_connected("the debug window"))
    }

    /// Waits for a stop or the end of the session.
    fn debug_wait(
        &self,
        _scope: &SessionScope,
        _query: &DebugWaitQuery,
    ) -> Result<DebugSessionState, ToolError> {
        Err(not_connected("the debug window"))
    }

    /// The frames of a thread.
    fn debug_stack(
        &self,
        _scope: &SessionScope,
        _query: &DebugStackQuery,
    ) -> Result<DebugStack, ToolError> {
        Err(not_connected("the debug window"))
    }

    /// The variables of a frame.
    fn debug_variables(
        &self,
        _scope: &SessionScope,
        _query: &DebugVariablesQuery,
    ) -> Result<DebugVariables, ToolError> {
        Err(not_connected("the debug window"))
    }

    /// Evaluates an expression in a frame.
    fn debug_evaluate(
        &self,
        _scope: &SessionScope,
        _query: &DebugEvaluateQuery,
    ) -> Result<DebugValue, ToolError> {
        Err(not_connected("the debug window"))
    }

    /// Moves the program and answers where it got.
    fn debug_step(
        &self,
        _scope: &SessionScope,
        _query: &DebugStepQuery,
    ) -> Result<DebugSessionState, ToolError> {
        Err(not_connected("the debug window"))
    }

    /// Ends the debug session, as the Stop button does.
    fn debug_stop(&self, _scope: &SessionScope) -> Result<(), ToolError> {
        Err(not_connected("the debug window"))
    }

    /// The session tools of IDEA: several sessions of the debug window, their threads and
    /// values, a step that waits for a pause and points that name their owner. A window
    /// that answers one of them implements this method; [`Self::debug`] reaches it.
    fn debug_sessions(
        &self,
        _scope: &SessionScope,
        _query: &DebugQuery,
    ) -> Result<DebugAnswer, ToolError> {
        Err(not_connected("the debug window"))
    }

    /// The tools of the Database window: its connections, the schemas of the driver and
    /// the statements of its console.
    fn database(
        &self,
        _scope: &SessionScope,
        _query: &DatabaseQuery,
    ) -> Result<DatabaseAnswer, ToolError> {
        Err(not_connected("the Database window"))
    }
}

fn not_connected(what: &str) -> ToolError {
    ToolError::Failed(format!("{what} is not connected yet"))
}

/// A path relative to `root` with `/`, or the whole path when it is outside.
pub(crate) fn relative_path(root: &Path, path: &Path) -> String {
    let root_text = root.to_string_lossy().replace('\\', "/");
    let text = path.to_string_lossy().replace('\\', "/");
    if text.eq_ignore_ascii_case(&root_text) {
        return String::new();
    }
    let prefix = format!("{}/", root_text.trim_end_matches('/'));
    if text.len() > prefix.len() && text[..prefix.len()].eq_ignore_ascii_case(&prefix) {
        return text[prefix.len()..].to_string();
    }
    text
}

/// Every file under a folder, without the metadata folder of the tools.
fn collect_files(folder: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            if path.file_name().is_some_and(|name| name == ".git") {
                continue;
            }
            collect_files(&path, out);
        } else if kind.is_file() {
            out.push(path);
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolError {
    UnknownTool(String),
    BadInput(String),
    Denied(String),
    Failed(String),
    /// The window is there, but this debugger or this adapter does not do it.
    Unsupported(String),
}

impl fmt::Display for ToolError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::UnknownTool(name) => write!(f, "unknown tool {name}"),
            Self::BadInput(message) => write!(f, "bad input: {message}"),
            Self::Denied(message) => write!(f, "denied: {message}"),
            Self::Failed(message) => f.write_str(message),
            Self::Unsupported(message) => write!(f, "not supported: {message}"),
        }
    }
}

impl std::error::Error for ToolError {}

/// Until the editor and the language layer answer, a call says so instead of making things up.
pub struct Unavailable;

/// What a build covers: the whole project, or one module of it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BuildRequest {
    pub module: Option<String>,
    /// Build every module again instead of the changed ones.
    pub rebuild: bool,
    /// Only these files of the project; `rebuild` does not act then.
    #[serde(rename = "filesToRebuild")]
    pub files_to_rebuild: Vec<PathBuf>,
    /// Milliseconds.
    pub timeout: Option<u64>,
}

/// One error the build tool reported, with the place in the sources it named.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuildProblem {
    pub path: PathBuf,
    /// 1-based.
    pub line: u32,
    pub message: String,
}

/// The end of a build: its verdict, what it complained about and how long it took.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuildOutcome {
    pub success: bool,
    pub problems: Vec<BuildProblem>,
    pub duration_ms: u64,
    /// The id `run_output` reads the whole log by.
    pub output: String,
}

/// The tests to run: one case, its suite, a package of suites, or all of them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TestsRequest {
    pub module: Option<String>,
    /// The package or folder of the suites.
    pub package: Option<String>,
    /// A suite: a class or a file of tests, in the terms of the project.
    pub suite: Option<String>,
    /// One test of the suite.
    pub case: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TestOutcome {
    Passed,
    Failed,
    Skipped,
}

impl TestOutcome {
    pub fn name(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
        }
    }
}

/// One test as the run left it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestCase {
    pub suite: String,
    pub case: Option<String>,
    pub outcome: TestOutcome,
    pub message: Option<String>,
    /// The first lines of the stack of a failure.
    pub trace: Vec<String>,
    pub duration_ms: u64,
}

/// The end of a test run: the counts, the cases and the id of its output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestsOutcome {
    pub passed: usize,
    pub failed: usize,
    pub skipped: usize,
    pub cases: Vec<TestCase>,
    pub duration_ms: u64,
    /// The id `run_output` reads the whole log by.
    pub output: String,
}

/// One configuration the user can start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunConfiguration {
    pub name: String,
    /// What kind of run it is: a program, a task, several configurations at once.
    pub kind: String,
    /// The line the run starts, when the project knows it.
    pub command_line: Option<String>,
    /// The folder the program starts in, relative to the project when it is inside it.
    pub working_directory: Option<String>,
    /// Variables the run sets.
    pub environment: Vec<(String, String)>,
    /// Whether a single run may change its arguments, folder and variables.
    pub supports_dynamic_launch_overrides: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StartRunRequest {
    pub name: String,
    #[serde(rename = "programArguments")]
    pub program_arguments: Option<String>,
    #[serde(rename = "workingDirectory")]
    pub working_directory: Option<PathBuf>,
    pub envs: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunId {
    pub id: String,
}

/// How many of the last lines of a run an answer carries.
pub const OUTPUT_TAIL: usize = 200;

/// The state of a run and the tail of what it printed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunOutput {
    pub id: String,
    pub running: bool,
    pub exit_code: Option<i32>,
    pub lines: Vec<String>,
    /// The file with the whole log, when the window keeps one.
    pub full_output_path: Option<PathBuf>,
}

/// What the debug window of a project offers beyond the snapshot `debug_get_state`
/// reads: breakpoints the user sees, a session started the way the Debug button starts
/// it, and the commands of a stopped program. A window registers one when its Debug
/// panel opens; every call blocks the worker thread until the panel answers, and the
/// panel itself never waits.
pub trait DebugControl: Send + Sync {
    fn set_breakpoint(
        &self,
        scope: &SessionScope,
        query: &BreakpointQuery,
    ) -> Result<BreakpointInfo, ToolError>;

    fn remove_breakpoint(&self, scope: &SessionScope, id: &BreakpointId) -> Result<(), ToolError>;

    fn breakpoints(&self, scope: &SessionScope) -> Result<Vec<BreakpointInfo>, ToolError>;

    /// Starts a test or a configuration under the debugger and answers the id of the
    /// session, waited for its first stop for `request.wait_seconds`.
    fn run(
        &self,
        scope: &SessionScope,
        request: &DebugRunRequest,
        client: Option<&str>,
    ) -> Result<DebugRunOutcome, ToolError>;

    fn wait(
        &self,
        scope: &SessionScope,
        query: &DebugWaitQuery,
    ) -> Result<DebugSessionState, ToolError>;

    fn stack(&self, scope: &SessionScope, query: &DebugStackQuery)
    -> Result<DebugStack, ToolError>;

    fn variables(
        &self,
        scope: &SessionScope,
        query: &DebugVariablesQuery,
    ) -> Result<DebugVariables, ToolError>;

    fn evaluate(
        &self,
        scope: &SessionScope,
        query: &DebugEvaluateQuery,
    ) -> Result<DebugValue, ToolError>;

    fn step(
        &self,
        scope: &SessionScope,
        query: &DebugStepQuery,
    ) -> Result<DebugSessionState, ToolError>;

    fn stop(&self, scope: &SessionScope) -> Result<(), ToolError>;

    /// The session tools of IDEA, which the earlier helpers above do not carry: several
    /// sessions, their threads, the values of a frame and the points with an owner.
    fn call(&self, _scope: &SessionScope, _query: &DebugQuery) -> Result<DebugAnswer, ToolError> {
        Err(not_connected("the debug session tools"))
    }
}

/// What the Database window of a project offers: its connections, the schema the driver
/// answered and the statements of its console. A window registers one when its Database
/// panel opens; every call blocks the worker thread until the panel answers.
pub trait DatabaseControl: Send + Sync {
    fn call(
        &self,
        scope: &SessionScope,
        query: &DatabaseQuery,
    ) -> Result<DatabaseAnswer, ToolError>;
}

/// What the run window of a project offers: the build tool, the tests and the stored
/// configurations. A window registers one when its Run panel opens; the gateway asks
/// before it reaches it, so the panel never blocks on a call that waits for a user.
pub trait ProjectRunner: Send + Sync {
    fn build(
        &self,
        scope: &SessionScope,
        request: &BuildRequest,
    ) -> Result<BuildOutcome, ToolError>;

    fn test(&self, scope: &SessionScope, request: &TestsRequest)
    -> Result<TestsOutcome, ToolError>;

    /// The configurations the user can start.
    fn configurations(&self, scope: &SessionScope) -> Result<Vec<RunConfiguration>, ToolError>;

    fn start(
        &self,
        scope: &SessionScope,
        request: &StartRunRequest,
        client: Option<&str>,
    ) -> Result<String, ToolError>;

    fn output(&self, scope: &SessionScope, id: &str) -> Result<RunOutput, ToolError>;

    fn stop(&self, scope: &SessionScope, id: &str) -> Result<(), ToolError>;
}

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
