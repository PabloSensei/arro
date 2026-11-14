//! What the window shows of a session, and what it can ask for.
//!
//! Plain data only: the session task fills it, the window reads a snapshot.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use proc_sup::Input;
use serde_json::Value;

/// Where a session is.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum DebugState {
    /// Nothing started yet.
    #[default]
    Inactive,
    /// Waiting for the adapter to say it is ready.
    Starting,
    Running,
    Stopped {
        reason: String,
    },
    Terminated,
}

/// One thread of the debuggee.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Thread {
    pub id: i64,
    pub name: String,
}

/// One frame of a stack trace. `source` is absent when the adapter sent none, and
/// `line` is zero when it did not know the place.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Frame {
    pub id: i64,
    pub name: String,
    pub source: Option<PathBuf>,
    pub line: u32,
    pub column: u32,
}

/// A group of variables of one frame.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Scope {
    pub name: String,
    pub reference: i64,
    pub expensive: bool,
}

/// One variable or watch result. `reference` is the handle its children are asked with,
/// zero when it has none.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Variable {
    pub name: String,
    pub value: String,
    pub type_name: String,
    pub reference: i64,
}

/// One watch expression and its last value.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WatchValue {
    pub expression: String,
    pub value: String,
    pub type_name: String,
    pub error: Option<String>,
}

/// One place a step into may go, as the adapter lists it on a line with several calls.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StepTarget {
    pub id: i64,
    pub label: String,
}

/// The answer of an expression the user evaluated without adding it to the watch list.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Evaluation {
    pub expression: String,
    pub value: String,
    pub type_name: String,
    /// Handle of the fields of an object, zero when it has none.
    pub reference: i64,
    pub error: Option<String>,
}

/// Why the adapter refused a new value for a variable.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VariableError {
    /// The group or object the variable belongs to.
    pub reference: i64,
    pub name: String,
    pub message: String,
}

/// What an adapter said it can do with the requests added for methods and fields.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Capabilities {
    pub function_breakpoints: bool,
    pub data_breakpoints: bool,
    pub restart_frame: bool,
}

/// The kind of a breakpoint: a place in a file, a method, or a field.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BreakpointKind {
    #[default]
    Line,
    Function,
    Data,
}

/// A breakpoint as the adapter knows it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Breakpoint {
    pub file: PathBuf,
    /// The line the adapter reports, zero when it did not.
    pub line: u32,
    pub kind: BreakpointKind,
    /// The id the adapter gave the breakpoint, when it gave one.
    pub id: Option<i64>,
    pub verified: bool,
    pub message: Option<String>,
}

/// A method breakpoint: the adapter stops at the entry of `name`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FunctionBreakpointSpec {
    /// The name the adapter expects, `com.example.Foo#bar` for a JVM one.
    pub name: String,
    pub condition: Option<String>,
    pub hit_condition: Option<String>,
    /// A disabled breakpoint is not sent to the adapter.
    pub enabled: bool,
}

/// How a field watchpoint reacts to the field.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DataAccessType {
    Read,
    #[default]
    Write,
    ReadWrite,
}

impl DataAccessType {
    pub fn as_dap(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::ReadWrite => "readWrite",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::ReadWrite => "read and write",
        }
    }
}

/// A field watchpoint: the adapter stops when the field is read or written.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DataBreakpointSpec {
    /// The id the adapter gave the field, `com.example.Foo#bar` for a JVM one.
    pub data_id: String,
    pub access_type: DataAccessType,
    pub condition: Option<String>,
    pub hit_condition: Option<String>,
    /// A disabled breakpoint is not sent to the adapter.
    pub enabled: bool,
}

/// A breakpoint as the window sets it. `hit_condition` is the DAP text form, so an adapter
/// may read it as a number or as an expression.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BreakpointSpec {
    pub file: PathBuf,
    pub line: u32,
    pub condition: Option<String>,
    pub hit_condition: Option<String>,
    pub enabled: bool,
}

impl Default for BreakpointSpec {
    fn default() -> Self {
        Self {
            file: PathBuf::new(),
            line: 0,
            condition: None,
            hit_condition: None,
            enabled: true,
        }
    }
}

/// A filter of the adapter for the exceptions the debuggee may stop on.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExceptionFilter {
    pub filter: String,
    pub label: String,
    pub default: bool,
    pub description: Option<String>,
}

/// One line of the debug console. Lines of the error stream and of the adapter are marked.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConsoleLine {
    pub text: String,
    pub error: bool,
}

impl ConsoleLine {
    pub fn new(text: impl Into<String>, error: bool) -> Self {
        Self {
            text: text.into(),
            error,
        }
    }
}

/// Output of the debuggee that does not come through the adapter.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OutputLine {
    pub text: String,
    pub error: bool,
}

/// Everything the window draws of a session.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    pub state: DebugState,
    /// Shared, because a debuggee may have thousands of threads and the window copies
    /// the snapshot on every event.
    pub threads: Arc<Vec<Thread>>,
    pub current_thread: Option<i64>,
    pub frames: Vec<Frame>,
    pub selected_frame: Option<i64>,
    pub scopes: Vec<Scope>,
    /// Children of each scope or variable, by reference, filled on demand.
    pub variables: HashMap<i64, Vec<Variable>>,
    pub watch: Vec<WatchValue>,
    /// Output events and notes of the adapter, in arrival order.
    pub console: Vec<ConsoleLine>,
    pub breakpoints: Vec<Breakpoint>,
    /// What the adapter said about the method breakpoints, in the order they were sent.
    pub function_breakpoints: Vec<Breakpoint>,
    /// What the adapter said about the field watchpoints, in the order they were sent.
    pub data_breakpoints: Vec<Breakpoint>,
    /// The requests the adapter supports, from its capabilities.
    pub capabilities: Capabilities,
    /// What the adapter offers for exceptions, from its capabilities.
    pub exception_filters: Vec<ExceptionFilter>,
    /// The process id the adapter reported for the debuggee, when it did.
    pub process_id: Option<u32>,
    /// The places Smart Step Into offers, `None` before it asked.
    pub step_targets: Option<Vec<StepTarget>>,
    /// The last Quick Evaluate answer.
    pub evaluation: Option<Evaluation>,
    /// The last value the adapter refused.
    pub variable_error: Option<VariableError>,
    /// How many times the program has stopped in this session. A new stop is a new
    /// number even when two stops look alike, so one-stop settings end exactly once.
    pub stops: u64,
}

/// What the window asks of a running session.
#[derive(Debug)]
pub enum Command {
    Continue,
    Pause,
    Next,
    StepIn,
    /// Steps into a call without the filters of the adapter: library code too.
    ForceStepIn,
    /// Asks the adapter where a step into may go on the current line.
    StepInTargets,
    /// Steps into one of the places the last [`Command::StepInTargets`] named.
    StepInTarget(i64),
    StepOut,
    Disconnect,
    SelectThread(i64),
    SelectFrame(i64),
    /// Ask for the children of a scope or a variable.
    Expand(i64),
    /// Sets, changes or clears one breakpoint. A disabled breakpoint is not sent to the
    /// adapter.
    SetBreakpoint(BreakpointSpec),
    RemoveBreakpoint {
        file: PathBuf,
        line: u32,
    },
    /// The filters the adapter should stop on, by id from its capabilities.
    SetExceptionBreakpoints(Vec<String>),
    /// Replaces every method breakpoint of the session.
    SetFunctionBreakpoints(Vec<FunctionBreakpointSpec>),
    /// Replaces every field watchpoint of the session.
    SetDataBreakpoints(Vec<DataBreakpointSpec>),
    /// Returns the execution of a frame to its start, when the adapter allows it.
    RestartFrame(i64),
    AddWatch(String),
    RemoveWatch(String),
    /// Evaluate an expression in the console of the selected frame.
    Evaluate(String),
    /// Evaluate an expression in the selected frame for the popup at the caret.
    EvaluateQuick(String),
    /// Gives a variable a new value; `reference` is the group or object that holds it.
    SetVariable {
        reference: i64,
        name: String,
        value: String,
    },
    /// Set every breakpoint of the files again, after an adapter restart.
    Refresh,
    /// A request of the adapter this crate does not know, sent as it is. The answer
    /// comes back through `reply`.
    Extension {
        command: String,
        arguments: Value,
        reply: tokio::sync::oneshot::Sender<Result<Value, String>>,
    },
}

/// What an adapter answered to a reload of the classes under a running program.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReloadOutcome {
    /// The classes the adapter replaced, by name.
    pub reloaded: Vec<String>,
    /// Why the adapter refused, usually a changed signature or a new method.
    pub error: Option<String>,
}

/// The window's end of a session. Cheap to clone and safe to use from any thread.
#[derive(Clone)]
pub struct Debugger {
    id: u64,
    commands: tokio::sync::mpsc::UnboundedSender<Command>,
    snapshots:
        std::sync::Arc<std::sync::Mutex<std::sync::mpsc::Receiver<std::sync::Arc<Snapshot>>>>,
    /// The standard input of a program this session launched; a session that attaches
    /// to somebody else's program has none.
    input: Option<Input>,
}

impl Debugger {
    pub fn new(
        commands: tokio::sync::mpsc::UnboundedSender<Command>,
        snapshots: std::sync::mpsc::Receiver<std::sync::Arc<Snapshot>>,
    ) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self {
            id: NEXT.fetch_add(1, Ordering::Relaxed),
            commands,
            snapshots: std::sync::Arc::new(std::sync::Mutex::new(snapshots)),
            input: None,
        }
    }

    /// Gives the session the standard input of the program it launched, so the console
    /// can type into it.
    pub fn with_input(mut self, input: Input) -> Self {
        self.input = Some(input);
        self
    }

    /// Whether the program of this session reads its standard input from the console.
    pub fn takes_input(&self) -> bool {
        self.input.is_some()
    }

    /// A line for the program, with the line break that Enter adds.
    pub fn send_line(&self, line: &str) {
        if let Some(input) = &self.input {
            input.send_line(line);
        }
    }

    /// The program sees the end of its input.
    pub fn send_eof(&self) {
        if let Some(input) = &self.input {
            input.send_eof();
        }
    }

    /// Why the last write of the input failed, since the last call.
    pub fn take_input_error(&self) -> Option<String> {
        self.input.as_ref().and_then(Input::take_error)
    }

    /// A number of this session that no later session has, so a caller can keep state
    /// per session.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Never waits. `false` means the session has ended.
    pub fn send(&self, command: Command) -> bool {
        self.commands.send(command).is_ok()
    }

    /// Sends an adapter-specific request and waits for its answer. The caller knows the
    /// name and the shape; this crate only carries them.
    pub async fn request(
        &self,
        command: impl Into<String>,
        arguments: Value,
    ) -> Result<Value, String> {
        let (reply, answer) = tokio::sync::oneshot::channel();
        let sent = self.send(Command::Extension {
            command: command.into(),
            arguments,
            reply,
        });
        if !sent {
            return Err("the debug session has ended".to_owned());
        }
        answer
            .await
            .map_err(|_| "the debug session has ended".to_owned())?
    }

    /// The newest snapshot, if one arrived since the last call.
    pub fn poll(&self) -> Option<std::sync::Arc<Snapshot>> {
        let snapshots = self
            .snapshots
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut latest = None;
        while let Ok(snapshot) = snapshots.try_recv() {
            latest = Some(snapshot);
        }
        latest
    }
}

/// Reads a path sent in a `source`, `None` when the adapter sent an empty or unusable one.
pub fn source_path(source: Option<&Value>) -> Option<PathBuf> {
    let path = source?.get("path")?.as_str()?.trim();
    if path.is_empty() || path == "<unknown>" {
        return None;
    }
    Some(PathBuf::from(path))
}

/// A line from a DAP body, zero when it is absent or not a number.
pub fn line_of(body: &Value) -> u32 {
    body.get("line")
        .and_then(Value::as_i64)
        .filter(|line| *line > 0)
        .map(|line| line as u32)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_missing_source_and_a_zero_line_are_not_errors() {
        assert_eq!(source_path(None), None);
        assert_eq!(source_path(Some(&json!({}))), None);
        assert_eq!(source_path(Some(&json!({"path": ""}))), None);
        assert_eq!(source_path(Some(&json!({"path": "<unknown>"}))), None);
        assert_eq!(
            source_path(Some(&json!({"path": "C:/src/a.txt"}))),
            Some(PathBuf::from("C:/src/a.txt"))
        );
        assert_eq!(line_of(&json!({"line": 0})), 0);
        assert_eq!(line_of(&json!({})), 0);
        assert_eq!(line_of(&json!({"line": 7})), 7);
    }

    #[test]
    fn a_breakpoint_is_enabled_unless_it_says_otherwise() {
        assert!(BreakpointSpec::default().enabled);
    }
}
