//! The neutral types of the debugger tools that IDEA's MCP server names `xdebug_*`.
//!
//! A tool of this module picks a session, asks the debug window that owns it and renders
//! the answer as JSON with the field names of IDEA. No type here knows a language: a
//! language plugs its own debugger behind [`crate::backend::DebugControl`].

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::backend::ToolError;

/// How a session moves when `xdebug_control_session` asks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DebugControlAction {
    StepInto,
    StepOver,
    StepOut,
    Resume,
    Pause,
    Stop,
    WaitForPause,
    DrainEvents,
}

impl DebugControlAction {
    /// The name IDEA uses, or `None` for an action this server does not know.
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim() {
            "STEP_INTO" => Some(Self::StepInto),
            "STEP_OVER" => Some(Self::StepOver),
            "STEP_OUT" => Some(Self::StepOut),
            "RESUME" => Some(Self::Resume),
            "PAUSE" => Some(Self::Pause),
            "STOP" => Some(Self::Stop),
            "WAIT_FOR_PAUSE" => Some(Self::WaitForPause),
            "DRAIN_EVENTS" => Some(Self::DrainEvents),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::StepInto => "STEP_INTO",
            Self::StepOver => "STEP_OVER",
            Self::StepOut => "STEP_OUT",
            Self::Resume => "RESUME",
            Self::Pause => "PAUSE",
            Self::Stop => "STOP",
            Self::WaitForPause => "WAIT_FOR_PAUSE",
            Self::DrainEvents => "DRAIN_EVENTS",
        }
    }
}

/// One debug session as `xdebug_get_debugger_status` lists it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DebuggerSession {
    /// The id the other `xdebug_*` tools pass as `sessionId`.
    pub id: String,
    pub name: String,
    /// `starting`, `running`, `paused` or `stopped`.
    pub status: String,
    pub run_configuration: Option<String>,
    /// The newest lines the session printed.
    pub output_preview: Option<String>,
    pub full_output_path: Option<PathBuf>,
    pub exit_code: Option<i32>,
}

/// Every session of the debug window and the one it shows.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DebuggerStatus {
    pub sessions: Vec<DebuggerSession>,
    pub active_session_id: Option<String>,
}

/// A session to start: a stored configuration or a place in a file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DebugStartQuery {
    /// From `get_run_configurations`; the place is used when it is absent.
    pub configuration_name: Option<String>,
    /// An absolute place inside the project, when no configuration is named.
    pub file_path: Option<PathBuf>,
    /// 1-based.
    pub line: Option<u32>,
    /// Milliseconds to wait for the session, 60 000 by default.
    pub timeout_ms: u64,
    /// Milliseconds to wait for a pause after the session exists, 2 000 by default.
    pub grace_wait_ms: u64,
    pub program_arguments: Option<String>,
    pub working_directory: Option<PathBuf>,
    /// `NAME=VALUE` pairs, as the window passes them to a process.
    pub envs: Vec<String>,
    /// The MCP client that asked, for the console of the window; not an argument.
    pub client: Option<String>,
}

/// What `xdebug_control_session` asks of one session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DebugControlQuery {
    /// `None` selects the only session; several sessions make it an error.
    pub session_id: Option<String>,
    pub action: DebugControlAction,
    /// Milliseconds to wait for a pause, 30 000 by default.
    pub timeout_ms: u64,
    /// How many lines of the console the answer may carry, 100 by default.
    pub events_limit: usize,
}

/// Where the session is after a control action.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugControlOutcome {
    pub session_id: String,
    /// `running`, `paused` or `stopped`.
    pub status: String,
    pub paused: bool,
    pub reason: Option<String>,
    /// The values of the top frame, when the program paused.
    pub frame_values: Vec<DebugValueNode>,
    /// Breakpoints the adapter refused, newest last.
    pub breakpoint_errors_tail: Vec<String>,
    /// What the logpoints printed, newest last.
    pub tracepoint_outputs_tail: Vec<String>,
}

/// How many threads to show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DebugThreadsQuery {
    pub session_id: Option<String>,
    pub limit: usize,
    pub offset: usize,
}

/// One thread of the debuggee.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugThreadInfo {
    pub id: i64,
    pub name: String,
    /// `running`, `paused` or `unknown`, as the adapter says it.
    pub state: String,
    pub is_current: bool,
    pub additional_info: Option<String>,
    pub additional_info_tooltip: Option<String>,
    pub frame_count: usize,
}

/// The threads of a session, with the number the answer does not carry.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DebugThreadPage {
    pub threads: Vec<DebugThreadInfo>,
    pub total: usize,
}

/// Which thread of the session a stack is asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DebugThreadSelector {
    Id(i64),
    /// The adapter shows a thread by this name; the active thread when it matches.
    Name(String),
}

/// A line to continue to with a temporary point.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DebugRunToLineQuery {
    pub session_id: Option<String>,
    pub file: PathBuf,
    /// 1-based.
    pub line: u32,
    /// Milliseconds to wait for the line, 30 000 by default.
    pub timeout_ms: u64,
}

/// How much of a stack to show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DebugFramesQuery {
    pub session_id: Option<String>,
    pub thread: Option<DebugThreadSelector>,
    pub limit: usize,
    pub offset: usize,
}

/// One frame of a stack, as IDEA names it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugFrameInfo {
    /// The index in the whole stack, from zero.
    pub index: usize,
    pub file: Option<PathBuf>,
    /// 1-based, zero when the adapter did not know the place.
    pub line: u32,
    pub is_current: bool,
    /// The frame as the window shows it, such as `demo.Probe.run(Probe.txt:12)`.
    pub presentation: String,
}

/// The frames of one thread, with the number the answer does not carry.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DebugFramePage {
    pub thread_id: Option<i64>,
    pub thread_name: Option<String>,
    pub frames: Vec<DebugFrameInfo>,
    pub total: usize,
}

/// How much of the values of a frame to show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DebugValuesQuery {
    pub session_id: Option<String>,
    /// The frame by its index in the stack; zero, the top frame, by default.
    pub frame_index: usize,
    pub depth: usize,
}

/// One value of a frame with the children the caller asked for.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugValueNode {
    pub name: String,
    pub value: String,
    #[serde(rename = "type")]
    pub type_name: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<DebugValueNode>,
}

/// The values of one frame, in the order the window shows them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DebugFrameValues {
    pub frame_index: usize,
    pub values: Vec<DebugValueNode>,
}

/// A value named by a path of names and `[index]` steps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DebugPathQuery {
    pub session_id: Option<String>,
    pub frame_index: usize,
    pub path: Vec<String>,
    pub depth: usize,
}

/// An expression to evaluate, with the children of its value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DebugExpressionQuery {
    pub session_id: Option<String>,
    pub frame_index: Option<usize>,
    pub expression: String,
    pub depth: usize,
}

/// A new value for a variable named by a path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DebugSetVariableQuery {
    pub session_id: Option<String>,
    pub frame_index: usize,
    pub path: Vec<String>,
    pub new_value: String,
}

/// What a new value of a variable did.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugSetVariableOutcome {
    pub old_value: Option<String>,
    pub new_value: String,
    pub applied: bool,
}

/// Who set a breakpoint.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BreakpointOwner {
    Agent,
    #[default]
    User,
}

impl BreakpointOwner {
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "agent" => Some(Self::Agent),
            "user" => Some(Self::User),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::User => "user",
        }
    }
}

/// One breakpoint as the debugger window holds it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugBreakpoint {
    /// Stable for the session: `<relative path>:<line>`.
    pub id: String,
    /// `LINE`, the only kind a tool sets.
    #[serde(rename = "type")]
    pub kind: String,
    pub file: PathBuf,
    /// 1-based.
    pub line: u32,
    pub enabled: bool,
    /// Whether the adapter accepted the place.
    pub verified: bool,
    pub owner: BreakpointOwner,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
    pub is_log_message: bool,
    pub is_log_stack: bool,
    pub temporary: bool,
    /// `ALL` or `NONE`; `THREAD` is not supported by this debugger.
    pub suspend_policy: String,
    /// Stops on the given hit of the line; zero when the point has no count.
    pub hit_count: u32,
    /// The text of the line, when the file could be read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_text: Option<String>,
}

/// The fields of a breakpoint a tool may set. An absent field keeps its value; a
/// condition of `null` clears one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DebugBreakpointQuery {
    /// Edits the point of this id when it is given.
    pub breakpoint_id: Option<String>,
    pub file_path: Option<PathBuf>,
    /// 1-based.
    pub line: Option<u32>,
    /// `None` keeps the condition, `Some(None)` clears it.
    pub condition: Option<Option<String>>,
    pub is_log_message: Option<bool>,
    pub is_log_stack: Option<bool>,
    pub temporary: Option<bool>,
    /// `ALL` or `NONE`.
    pub suspend_policy: Option<String>,
    pub hit_count: Option<u32>,
    pub enabled: Option<bool>,
    /// `agent` for a point of an agent, `user` otherwise.
    pub owner: Option<BreakpointOwner>,
}

impl DebugBreakpointQuery {
    /// The place the query names, `None` for an edit of an id.
    pub fn place(&self) -> Option<(&PathBuf, u32)> {
        Some((self.file_path.as_ref()?, self.line?))
    }
}

/// Which breakpoints to take away: every selector must match, and one `owner` alone
/// takes away every point of that owner.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DebugRemoveBreakpointsQuery {
    pub breakpoint_id: Option<String>,
    pub file_path: Option<PathBuf>,
    pub line: Option<u32>,
    pub owner: Option<BreakpointOwner>,
    /// Set by the caller, never by an agent: only the Debug window of the user may take
    /// away the points of the user without naming their places.
    pub all_of_owner_allowed: bool,
}

/// The id of a breakpoint of a file and a line, as the tools name it.
pub fn breakpoint_id(file: &std::path::Path, line: u32) -> String {
    format!("{}:{line}", file.to_string_lossy().replace('\\', "/"))
}

/// Reads a session id of a request; an empty string selects the only session.
pub fn session_id_of(input: &Value) -> Result<Option<String>, ToolError> {
    match input.get("sessionId") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) if text.trim().is_empty() => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.trim().to_string())),
        Some(_) => Err(ToolError::BadInput(
            "sessionId is a string or null".to_string(),
        )),
    }
}

/// The session a call belongs to. One running session is chosen by itself; several
/// need their id, so an agent never acts on the wrong one.
pub fn select_session<'a>(
    sessions: &'a [DebuggerSession],
    requested: Option<&str>,
) -> Result<&'a DebuggerSession, ToolError> {
    match requested {
        Some(id) => sessions
            .iter()
            .find(|session| session.id == id)
            .ok_or_else(|| ToolError::BadInput(format!("no debug session has the id {id}"))),
        None => match sessions {
            [] => Err(ToolError::Failed("no debug session is running".to_string())),
            [only] => Ok(only),
            _ => Err(ToolError::BadInput(format!(
                "{} debug sessions are running: pass sessionId",
                sessions.len()
            ))),
        },
    }
}

/// A path of a value of a frame: names and `[index]` steps, as
/// `["items","[0]","name"]` names the name of the first item.
pub fn parse_path(value: Option<&Value>) -> Result<Vec<String>, ToolError> {
    let list = value.and_then(Value::as_array).ok_or_else(|| {
        ToolError::BadInput("path is a list such as [\"items\",\"[0]\"]".to_string())
    })?;
    let mut path = Vec::with_capacity(list.len());
    for step in list {
        let text = step
            .as_str()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .ok_or_else(|| {
                ToolError::BadInput("every step of a path is a name or [index]".to_string())
            })?;
        path.push(text.to_string());
    }
    if path.is_empty() {
        return Err(ToolError::BadInput("the path is empty".to_string()));
    }
    Ok(path)
}

/// The children of a node that a path step names: an index for `[0]`, a name otherwise.
pub fn child_of<'a>(children: &'a [DebugValueNode], step: &str) -> Option<&'a DebugValueNode> {
    if let Some(inner) = step
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
    {
        if let Ok(index) = inner.trim().parse::<usize>() {
            return children.get(index);
        }
        return children
            .iter()
            .find(|node| node.name == inner.trim() || node.name == format!("[{inner}]"));
    }
    children.iter().find(|node| node.name == step).or_else(|| {
        children
            .iter()
            .find(|node| node.name == format!("[{step}]"))
    })
}

/// The value a path names, with the children the caller asked for.
pub fn value_at<'a>(values: &'a [DebugValueNode], path: &[String]) -> Option<&'a DebugValueNode> {
    let (first, rest) = path.split_first()?;
    let mut node = child_of(values, first)?;
    for step in rest {
        node = child_of(&node.children, step)?;
    }
    Some(node)
}

/// A short text of a path, for an error or a title.
pub fn path_text(path: &[String]) -> String {
    let mut text = String::new();
    for step in path {
        if step.starts_with('[') {
            text.push_str(step);
        } else if !text.is_empty() {
            text.push('.');
            text.push_str(step);
        } else {
            text.push_str(step);
        }
    }
    text
}

/// The JSON an `xdebug_*` tool answers with.
pub fn json<T: Serialize>(value: &T) -> String {
    match serde_json::to_string_pretty(value) {
        Ok(mut text) => {
            text.push('\n');
            text
        }
        Err(error) => format!("the answer could not be written: {error}\n"),
    }
}

/// The answer of `xdebug_start_debugger_session`: the session under the name IDEA uses.
pub fn started(session: &DebuggerSession) -> String {
    json(&serde_json::json!({
        "sessionId": session.id,
        "name": session.name,
        "status": session.status,
        "runConfigurationName": session.run_configuration,
        "outputPreview": session.output_preview,
        "fullOutputPath": session.full_output_path,
        "exitCode": session.exit_code,
    }))
}

/// What the gateway asks of the debug window for one `xdebug_*` tool. A window answers
/// one of these calls at a time on the worker thread of the tool.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DebugQuery {
    Status,
    Start(DebugStartQuery),
    Control(DebugControlQuery),
    RunToLine(DebugRunToLineQuery),
    Threads(DebugThreadsQuery),
    Frames(DebugFramesQuery),
    Values(DebugValuesQuery),
    Evaluate(DebugExpressionQuery),
    SetVariable(DebugSetVariableQuery),
    /// The breakpoints of one file, or of the project when the path is absent.
    Breakpoints(Option<PathBuf>),
    SetBreakpoint(DebugBreakpointQuery),
    RemoveBreakpoints(DebugRemoveBreakpointsQuery),
}

/// What the debug window answers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DebugAnswer {
    Status(DebuggerStatus),
    Started(DebuggerSession),
    Controlled(DebugControlOutcome),
    Threads(DebugThreadPage),
    Frames(DebugFramePage),
    Values(DebugFrameValues),
    /// The value of an expression, with the children that were asked for.
    Value(DebugValueNode),
    Set(DebugSetVariableOutcome),
    Breakpoints(Vec<DebugBreakpoint>),
    Breakpoint(DebugBreakpoint),
    /// How many breakpoints went away.
    Removed(usize),
}

impl DebugAnswer {
    /// The answer a query must give; any other one is a bug of the window.
    pub fn expect(self, expected: &str) -> Result<Self, ToolError> {
        if self.kind() == expected {
            Ok(self)
        } else {
            Err(ToolError::Failed(format!(
                "the debug window answered {expected} wrongly"
            )))
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            Self::Status(_) => "status",
            Self::Started(_) => "started",
            Self::Controlled(_) => "controlled",
            Self::Threads(_) => "threads",
            Self::Frames(_) => "frames",
            Self::Values(_) => "values",
            Self::Value(_) => "value",
            Self::Set(_) => "set",
            Self::Breakpoints(_) => "breakpoints",
            Self::Breakpoint(_) => "breakpoint",
            Self::Removed(_) => "removed",
        }
    }
}

/// The threads a tool asks for.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ThreadsInput {
    #[serde(rename = "sessionId", default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub offset: Option<usize>,
}

/// The stack a tool asks for.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FramesInput {
    #[serde(rename = "sessionId", default)]
    pub session_id: Option<String>,
    /// An id or the name the window shows.
    #[serde(rename = "threadId", default)]
    pub thread_id: Option<Value>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub offset: Option<usize>,
}

/// The values of a frame a tool asks for.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ValuesInput {
    #[serde(rename = "sessionId", default)]
    pub session_id: Option<String>,
    #[serde(rename = "frameIndex", default)]
    pub frame_index: Option<usize>,
    #[serde(default)]
    pub depth: Option<usize>,
}

/// A value by its path.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PathInput {
    #[serde(rename = "sessionId", default)]
    pub session_id: Option<String>,
    #[serde(rename = "frameIndex", default)]
    pub frame_index: Option<usize>,
    pub path: Value,
    #[serde(default)]
    pub depth: Option<usize>,
}

/// An expression of a frame.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExpressionInput {
    #[serde(rename = "sessionId", default)]
    pub session_id: Option<String>,
    #[serde(rename = "frameIndex", default)]
    pub frame_index: Option<usize>,
    pub expression: String,
    #[serde(default)]
    pub depth: Option<usize>,
}

/// A new value of a variable.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SetVariableInput {
    #[serde(rename = "sessionId", default)]
    pub session_id: Option<String>,
    #[serde(rename = "frameIndex", default)]
    pub frame_index: Option<usize>,
    pub path: Value,
    #[serde(rename = "newValue")]
    pub new_value: String,
}

/// The breakpoints to list.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ListBreakpointsInput {
    #[serde(rename = "filePath", default)]
    pub file_path: Option<PathBuf>,
}

/// A breakpoint to set or to edit, as the agent wrote it.
#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct SetBreakpointInput {
    #[serde(rename = "breakpointId")]
    pub breakpoint_id: Option<String>,
    #[serde(rename = "filePath")]
    pub file_path: Option<PathBuf>,
    pub line: Option<u32>,
    /// `null` clears the condition, an absent field keeps it.
    #[serde(default)]
    pub condition: Option<Value>,
    #[serde(rename = "isLogMessage")]
    pub is_log_message: Option<bool>,
    #[serde(rename = "isLogStack")]
    pub is_log_stack: Option<bool>,
    pub temporary: Option<bool>,
    #[serde(rename = "suspendPolicy")]
    pub suspend_policy: Option<String>,
    #[serde(rename = "hitCount")]
    pub hit_count: Option<u32>,
    pub enabled: Option<bool>,
    pub owner: Option<String>,
}

/// The breakpoints to take away.
#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct RemoveBreakpointsInput {
    #[serde(rename = "breakpointId")]
    pub breakpoint_id: Option<String>,
    #[serde(rename = "filePath")]
    pub file_path: Option<PathBuf>,
    pub line: Option<u32>,
    pub owner: Option<String>,
}

/// The condition of a query: absent, cleared or set.
pub(crate) fn condition_of(value: Option<&Value>) -> Result<Option<Option<String>>, ToolError> {
    match value {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(None)),
        Some(Value::String(text)) => Ok(Some(Some(text.clone()))),
        Some(other) => Err(ToolError::BadInput(format!(
            "condition is a string or null, not {other}"
        ))),
    }
}

/// A query of a tool of the debugger, as the raw input chooses it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StartInput {
    #[serde(rename = "configurationName", default)]
    pub configuration_name: Option<String>,
    #[serde(rename = "filePath", default)]
    pub file_path: Option<PathBuf>,
    #[serde(default)]
    pub line: Option<u32>,
    #[serde(default)]
    pub timeout: Option<u64>,
    #[serde(rename = "graceWaitMs", default)]
    pub grace_wait_ms: Option<u64>,
    #[serde(rename = "programArguments", default)]
    pub program_arguments: Option<String>,
    #[serde(rename = "workingDirectory", default)]
    pub working_directory: Option<PathBuf>,
    #[serde(default)]
    pub envs: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(name: &str, children: Vec<DebugValueNode>) -> DebugValueNode {
        DebugValueNode {
            name: name.to_string(),
            value: name.to_string(),
            type_name: "T".to_string(),
            children,
        }
    }

    #[test]
    fn one_session_is_chosen_and_several_need_an_id() {
        let sessions = vec![
            DebuggerSession {
                id: "one".to_string(),
                ..DebuggerSession::default()
            },
            DebuggerSession {
                id: "two".to_string(),
                ..DebuggerSession::default()
            },
        ];
        assert_eq!(
            select_session(&sessions, Some("two")).map(|s| s.id.as_str()),
            Ok("two")
        );
        assert_eq!(
            select_session(&sessions[..1], None).map(|s| s.id.as_str()),
            Ok("one")
        );
        assert!(select_session(&sessions, None).is_err());
        assert!(select_session(&[], None).is_err());
        assert!(select_session(&sessions, Some("three")).is_err());
    }

    #[test]
    fn a_path_finds_a_named_value_and_an_index() {
        let values = vec![node(
            "items",
            vec![
                node("0", vec![node("name", Vec::new())]),
                node("1", Vec::new()),
            ],
        )];
        let path: Vec<String> = ["items", "[0]", "name"]
            .into_iter()
            .map(str::to_string)
            .collect();

        let found = value_at(&values, &path).expect("the value of the path");

        assert_eq!(found.name, "name");
        assert_eq!(path_text(&path), "items[0].name");
        assert!(value_at(&values, &["items".to_string(), "[2]".to_string()]).is_none());
        assert_eq!(
            child_of(&values, "items").map(|node| node.name.as_str()),
            Some("items")
        );
    }

    #[test]
    fn a_breakpoint_id_names_the_file_and_the_line() {
        assert_eq!(
            breakpoint_id(std::path::Path::new("C:/work/src/Main.txt"), 12),
            "C:/work/src/Main.txt:12"
        );
    }
}
