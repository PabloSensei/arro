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
/// zero when it has none. `evaluate_name` is the expression that names the value in the
/// frame, when the adapter sends one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Variable {
    pub name: String,
    pub value: String,
    pub type_name: String,
    pub reference: i64,
    pub evaluate_name: Option<String>,
}

/// One watch expression and its last value.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WatchValue {
    pub expression: String,
    pub value: String,
    pub type_name: String,
    /// Handle of the fields of the value, zero when it has none.
    pub reference: i64,
    pub error: Option<String>,
}

/// How the window shows a value of a type: `display` is an expression evaluated with the
/// value as the receiver, `children` an expression that gives the children instead of
/// the ones of the adapter.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TypeRenderer {
    /// The full name of the type, with `.` or `$` before a nested type.
    pub type_name: String,
    pub display: String,
    pub children: Option<String>,
    /// An expression of the language that says whether a value is of this type or of a
    /// subtype, `{name}` standing for the value. When it is absent, the renderer is for
    /// the exact type only.
    pub subtype_probe: Option<String>,
}

impl TypeRenderer {
    /// Whether this renderer is for `type_name`. A nested type is written with `.` in
    /// one place and `$` in another; both name the same type.
    pub fn matches(&self, type_name: &str) -> bool {
        let wanted = self.type_name.trim().replace('$', ".");
        !wanted.is_empty() && type_name.trim().replace('$', ".") == wanted
    }

    /// The expression that asks the program whether `evaluate_name` is of this type or
    /// of a subtype, when the language gave one.
    pub fn subtype_expression(&self, evaluate_name: &str) -> Option<String> {
        Some(
            self.subtype_probe
                .as_deref()?
                .replace("{name}", evaluate_name),
        )
    }
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

/// What the program answered when the window asked for the identity of an object: a
/// number the language keeps for the object itself, unlike a handle the adapter may give
/// to another value at the next stop.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IdentityAnswer {
    /// The request the answer belongs to, from [`Command::EvaluateIdentity`].
    pub request: u64,
    /// The number the expression gave, none when the answer was not a number.
    pub identity: Option<i64>,
    /// Why the question failed, when the adapter refused it.
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
    /// The adapter reads `hitCondition` of a breakpoint. Without it the window hides the
    /// pass count.
    pub hit_condition: bool,
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
/// may read it as a number or as an expression. A `log_message` makes the breakpoint a
/// logpoint of the adapter: the adapter prints it instead of stopping, and `{expression}`
/// in the text is replaced with the value of the expression when the adapter supports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BreakpointSpec {
    pub file: PathBuf,
    pub line: u32,
    pub condition: Option<String>,
    pub hit_condition: Option<String>,
    pub log_message: Option<String>,
    pub enabled: bool,
}

impl Default for BreakpointSpec {
    fn default() -> Self {
        Self {
            file: PathBuf::new(),
            line: 0,
            condition: None,
            hit_condition: None,
            log_message: None,
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
    /// The inline watches of the window and their last values, in the order they were
    /// set.
    pub inline_watch: Vec<WatchValue>,
    /// Answers to [`Command::EvaluateIdentity`], by the number the caller gave. The
    /// answers are kept for a while, so a caller that polls the newest snapshot does not
    /// lose one that came with an earlier one.
    pub identities: HashMap<u64, IdentityAnswer>,
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
    /// The exit code of the debuggee, when the adapter reported one.
    pub exit_code: Option<i32>,
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
    /// Mutes or unmutes every breakpoint of the session: a muted session sends the
    /// adapter no breakpoints and keeps the settings for the moment it is unmuted.
    SetMuteBreakpoints(bool),
    /// Replaces every method breakpoint of the session.
    SetFunctionBreakpoints(Vec<FunctionBreakpointSpec>),
    /// Replaces every field watchpoint of the session.
    SetDataBreakpoints(Vec<DataBreakpointSpec>),
    /// Returns the execution of a frame to its start, when the adapter allows it.
    RestartFrame(i64),
    AddWatch(String),
    RemoveWatch(String),
    /// Replaces the inline watches of the window. Every expression is evaluated at each
    /// stop, and the values arrive in the snapshot in the same order.
    SetInlineWatches(Vec<String>),
    /// Replaces how values of some types are shown; a later stop and its variables use
    /// the new rules.
    SetTypeRenderers(Vec<TypeRenderer>),
    /// Evaluate an expression in the console of the selected frame.
    Evaluate(String),
    /// Evaluate an expression for the Evaluate window: the value goes to the window as
    /// well as to the console, so its fields can be read there.
    EvaluateWindow(String),
    /// Evaluate an expression at a breakpoint that suspends the program and print its
    /// value in the console, one line per hit.
    EvaluateLog(String),
    /// Evaluate an expression in the selected frame for the popup at the caret.
    EvaluateQuick(String),
    /// Evaluates an expression of the caller that names the identity of an object, such
    /// as the number the language keeps for it. The answer arrives in the snapshot's
    /// `identities` under `request`.
    EvaluateIdentity {
        request: u64,
        expression: String,
    },
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
/// A `file:` URI is read as the path it names; any other text stays as the adapter wrote it.
pub fn source_path(source: Option<&Value>) -> Option<PathBuf> {
    let path = source?.get("path")?.as_str()?.trim();
    if path.is_empty() || path == "<unknown>" {
        return None;
    }
    Some(platform::path_from_uri(path).unwrap_or_else(|| PathBuf::from(path)))
}

/// A line from a DAP body, zero when it is absent or not a number.
pub fn line_of(body: &Value) -> u32 {
    body.get("line")
        .and_then(Value::as_i64)
        .filter(|line| *line > 0)
        .map(|line| line as u32)
        .unwrap_or(0)
}

/// The frames of one thread as a stack trace text, one `at name(File:line)` line per
/// frame. A frame without a place names no file, as a trace of an unsourced class does.
pub fn stack_trace_text(frames: &[Frame]) -> String {
    let mut text = String::new();
    for frame in frames {
        let name = frame.name.trim();
        let name = if name.is_empty() { "<unknown>" } else { name };
        let place = match (&frame.source, frame.line) {
            (Some(path), line) if line > 0 => {
                let file = path
                    .file_name()
                    .map(|file| file.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.to_string_lossy().into_owned());
                format!("{file}:{line}")
            }
            _ => "Unknown Source".to_owned(),
        };
        text.push_str("at ");
        text.push_str(name);
        text.push('(');
        text.push_str(&place);
        text.push_str(")\n");
    }
    text
}

/// An expression of a renderer with the value as its receiver: a call without a receiver
/// is a call of the value. `getName() + " (" + getId() + ")"` becomes
/// `(item).getName() + " (" + (item).getId() + ")"` for a value named `item`.
pub fn object_expression(expression: &str, evaluate_name: &str) -> String {
    let receiver = format!("({})", evaluate_name.trim());
    let mut out = String::with_capacity(expression.len() + 8);
    let chars: Vec<char> = expression.chars().collect();
    let mut at = 0;
    while at < chars.len() {
        let ch = chars[at];
        if ch == '"' || ch == '\'' {
            let quote = ch;
            out.push(ch);
            at += 1;
            while at < chars.len() {
                let current = chars[at];
                out.push(current);
                at += 1;
                if current == '\\' && at < chars.len() {
                    out.push(chars[at]);
                    at += 1;
                } else if current == quote {
                    break;
                }
            }
            continue;
        }
        if ch.is_alphanumeric() || ch == '_' || ch == '$' {
            let start = at;
            while at < chars.len()
                && (chars[at].is_alphanumeric() || chars[at] == '_' || chars[at] == '$')
            {
                at += 1;
            }
            let word: String = chars[start..at].iter().collect();
            let previous = chars[..start]
                .iter()
                .rev()
                .find(|ch| !ch.is_whitespace())
                .copied();
            let next = chars[at..].iter().find(|ch| !ch.is_whitespace()).copied();
            let plain = !word.chars().next().is_some_and(|ch| ch.is_ascii_digit())
                && word != "this"
                && word != "new"
                && previous != Some('.');
            if plain && next == Some('(') {
                out.push_str(&receiver);
                out.push('.');
            }
            out.push_str(&word);
            continue;
        }
        out.push(ch);
        at += 1;
    }
    out
}

/// The expression that lists the children of a value with a renderer, with the value as
/// its receiver. None when the renderer leaves the children of the adapter in place.
pub fn children_expression(renderer: &TypeRenderer, evaluate_name: &str) -> Option<String> {
    let children = renderer
        .children
        .as_deref()
        .filter(|text| !text.trim().is_empty())?;
    Some(object_expression(children, evaluate_name))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_file_uri_of_a_source_is_read_as_the_path_it_names() {
        let uri = "file:///c:/work/a%20b/Main.txt";
        let read = source_path(Some(&json!({"path": uri})));
        assert_eq!(read, platform::path_from_uri(uri));
        assert!(
            read.is_some_and(|path| !path.to_string_lossy().starts_with("file:")),
            "the URI is read as a path"
        );
        // Plain paths and the names of other schemes stay exactly as the adapter wrote them.
        assert_eq!(
            source_path(Some(&json!({"path": "C:\\work\\Main.txt"}))),
            Some(PathBuf::from("C:\\work\\Main.txt"))
        );
        assert_eq!(
            source_path(Some(&json!({"path": "lib://contents/a.jar/X.class"}))),
            Some(PathBuf::from("lib://contents/a.jar/X.class"))
        );
    }

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

    #[test]
    fn a_stack_text_names_each_frame_with_its_file_and_line() {
        let frames = vec![
            Frame {
                id: 1,
                name: "demo.Probe.work".into(),
                source: Some(PathBuf::from("C:/src/demo/Probe.txt")),
                line: 42,
                column: 1,
            },
            Frame {
                id: 2,
                name: "demo.Probe.main".into(),
                source: None,
                line: 0,
                column: 0,
            },
        ];
        assert_eq!(
            stack_trace_text(&frames),
            "at demo.Probe.work(Probe.txt:42)\nat demo.Probe.main(Unknown Source)\n"
        );
        assert_eq!(stack_trace_text(&[]), "");
    }

    #[test]
    fn a_renderer_names_a_nested_type_with_a_dot_or_a_dollar() {
        let renderer = TypeRenderer {
            type_name: "pkg.Map$Entry".into(),
            display: "getKey()".into(),
            children: None,
            subtype_probe: None,
        };
        assert!(renderer.matches("pkg.Map$Entry"));
        assert!(renderer.matches("pkg.Map.Entry"));
        assert!(!renderer.matches("pkg.HashMap$Node"));
        assert!(!TypeRenderer::default().matches(""));
        let probe = TypeRenderer {
            type_name: "pkg.List".into(),
            subtype_probe: Some("({name} instanceof pkg.List)".into()),
            ..TypeRenderer::default()
        };
        assert_eq!(
            probe.subtype_expression("items").as_deref(),
            Some("(items instanceof pkg.List)")
        );
        assert_eq!(renderer.subtype_expression("items"), None);
    }

    #[test]
    fn a_renderer_expression_receives_the_value_as_its_receiver() {
        assert_eq!(
            object_expression("getName() + \" (\" + getId() + \")\"", "item"),
            "(item).getName() + \" (\" + (item).getId() + \")\""
        );
        // A call of another object and the text of a string stay as they are.
        assert_eq!(
            object_expression("map.getKey() + \"getName()\"", "item"),
            "map.getKey() + \"getName()\""
        );
    }

    #[test]
    fn the_children_of_a_renderer_receive_the_value_as_their_receiver() {
        let renderer = |children: Option<&str>| TypeRenderer {
            type_name: "pkg.Items".into(),
            display: "size()".into(),
            children: children.map(str::to_owned),
            subtype_probe: None,
        };
        assert_eq!(
            children_expression(&renderer(Some("getItems()")), "items").as_deref(),
            Some("(items).getItems()")
        );
        assert_eq!(
            children_expression(&renderer(Some("getMap().values()")), "items").as_deref(),
            Some("(items).getMap().values()")
        );
        assert_eq!(children_expression(&renderer(None), "items"), None);
        assert_eq!(children_expression(&renderer(Some("  ")), "items"), None);
    }
}
