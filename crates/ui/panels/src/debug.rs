//! The Debug tool window: threads, the stack, variables with lazy expansion, watch
//! expressions, the console and the breakpoints of the project.
//!
//! The panel is the window's end of a `dap_client::Debugger`: it sends commands and
//! draws the newest snapshot, so an adapter never holds a frame.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use dap_client::{
    Breakpoint, BreakpointSpec, Command, ConsoleLine, DataAccessType, DataBreakpointSpec,
    DebugService, DebugSessionToken, DebugState, Debugger, Frame, FunctionBreakpointSpec, Scope,
    Snapshot, StepTarget, Thread, TypeRenderer, Variable,
};
use runner::{Console, ConsoleEvent, RunPlan, Step, Stream};
use serde::{Deserialize, Serialize};
use store::Store;
use tool_gateway::{
    BreakpointInfo, BreakpointOwner, DebugAnswer, DebugBreakpoint, DebugBreakpointQuery,
    DebugControlAction, DebugControlOutcome, DebugExpressionQuery, DebugFrame, DebugQuery,
    DebugRemoveBreakpointsQuery, DebugScope, DebugSessionState, DebugSetVariableOutcome,
    DebugSetVariableQuery, DebugSnapshotSource, DebugStack, DebugStartQuery, DebugThread,
    DebugThreadInfo, DebugThreadPage, DebugThreadsQuery, DebugValue, DebugValueNode, DebugVariable,
    DebugVariables,
};
use ui_kit::base::InteractiveElementExt;
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::{DEBUG_TOOL, Workspace};

use crate::ansi::{self, OutputBuffers};
use crate::console_follow::ConsoleFollow;
use crate::console_input::{ConsoleInput, ConsoleKey};
use crate::console_links;
use crate::console_search::{ConsoleSearch, SearchToggle};
use crate::mcp_debug::{Answer as ToolAnswer, DebugBridge, Job as ToolJob, Request as ToolRequest};
use crate::navigation;
use crate::prompt::Prompt;

const CONTEXT: &str = "DebugPanel";

actions!(
    debug_panel,
    [NewWatch, DeleteWatch, SetValue, MarkObject, CancelInput]
);

macro_rules! debug_keys {
    ($(($keys:literal, $action:ident, $idea:expr, $mac:expr)),* $(,)?) => {
        /// The keys of the Debug tool window, while it has the focus.
        fn bind_keys(cx: &mut App) {
            bind_keys_for(cx, keymap::platform());
        }

        /// Binds the keys of one platform, so that a test reads the macOS table on every
        /// runner.
        fn bind_keys_for(cx: &mut App, platform: keymap::Platform) {
            $(
                let row = workspace::KeyRow {
                    keys: $keys,
                    mac: $mac,
                    action: concat!("debug_panel::", stringify!($action)),
                    context: Some(CONTEXT),
                    idea: $idea,
                };
                for key in row.keys_for(platform) {
                    cx.bind_keys([KeyBinding::new(key, $action, Some(CONTEXT))]);
                }
            )*
        }

        #[cfg(test)]
        pub(crate) const KEYS: &[workspace::KeyRow] = &[
            $(workspace::KeyRow {
                keys: $keys,
                mac: $mac,
                action: concat!("debug_panel::", stringify!($action)),
                context: Some(CONTEXT),
                idea: $idea,
            }),*
        ];
    };
}

debug_keys![
    ("insert", NewWatch, Some("XDebugger.NewWatch"), None),
    ("f2", SetValue, Some("XDebugger.SetValue"), None),
    ("f11", MarkObject, None, Some("f11")),
    ("delete", DeleteWatch, None, Some("delete")),
    ("escape", CancelInput, None, Some("escape")),
];

const POLL: Duration = Duration::from_millis(50);
const GROUP: &str = "bottom-panel";
/// The width of the frames column at rest and the default part of the panel.
const TREE_WIDTH: f32 = 460.;
/// The least the frames column can become when the divider moves.
const MIN_TREE_WIDTH: f32 = 160.;
/// The least the variables column keeps when the divider moves.
const MIN_SIDE_WIDTH: f32 = 240.;
/// The width of the divider between the two columns of the debugger.
const SPLITTER_WIDTH: f32 = SPACE_S;
/// How many pixels of the watch list stay visible before it scrolls.
const WATCH_HEIGHT: f32 = 120.;
/// The marks of the virtual thread page in `expanded`. Variable references are
/// positive, so these keys never collide.
const VIRTUAL_PAGE_BASE: i64 = -10;
const MAX_CONSOLE: usize = 2_000;
const EVENTS: usize = 2_000;

/// The panel of the window, for other panels and scripts.
pub struct ActiveDebug(pub Entity<DebugPanel>);

impl Global for ActiveDebug {}

/// The newest snapshot, shared with the MCP tools. The window fills it on its own
/// thread; an agent reads it from a worker and never waits for a frame.
#[derive(Default)]
pub struct DebugHub {
    snapshot: RwLock<Option<Arc<Snapshot>>>,
}

impl DebugHub {
    pub fn set(&self, snapshot: Arc<Snapshot>) {
        *self
            .snapshot
            .write()
            .unwrap_or_else(|error| error.into_inner()) = Some(snapshot);
    }

    pub fn clear(&self) {
        *self
            .snapshot
            .write()
            .unwrap_or_else(|error| error.into_inner()) = None;
    }
}

impl DebugSnapshotSource for DebugHub {
    fn snapshot(&self) -> Option<Arc<Snapshot>> {
        self.snapshot
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }
}

/// The hub of the window, for `debug_get_state`.
pub struct ActiveDebugHub(pub Arc<DebugHub>);

impl Global for ActiveDebugHub {}

/// The breakpoints or the watch list changed, so an editor refreshes its gutter.
#[derive(Clone, Copy, Debug)]
pub struct BreakpointsChanged;

/// The values of the stopped frame changed, so an editor refreshes its inline values.
#[derive(Clone, Copy, Debug)]
pub struct ValuesChanged;

/// One breakpoint with the properties the user gave it. Kept with the project.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct BreakpointSetting {
    file: PathBuf,
    line: u32,
    #[serde(default)]
    condition: Option<String>,
    #[serde(default)]
    hit_condition: Option<String>,
    #[serde(default = "enabled")]
    enabled: bool,
    /// A breakpoint that stops the program when it is reached.
    #[serde(default = "enabled")]
    suspend: bool,
    /// Whether the console says where the program reached the breakpoint.
    #[serde(default)]
    hit_message: bool,
    /// An expression whose value the console shows on every hit.
    #[serde(default)]
    log_expression: Option<String>,
    /// The text the console prints on every hit instead of stopping, as an agent sets
    /// it. A point with a message does not suspend the program.
    #[serde(default)]
    log_message: Option<String>,
    /// A breakpoint that goes away at the first stop.
    #[serde(default)]
    temporary: bool,
    /// The point goes away once the program reaches its line.
    #[serde(default)]
    remove_once: bool,
    /// Stop every thread, or only the thread that reached the point. The adapter has no
    /// such setting, so the window keeps it for the next session.
    #[serde(default)]
    suspend_thread: bool,
    /// Whether an agent or the user set the point. An agent sees its own points in an
    /// answer with `owner: agent`; the editor paints them apart.
    #[serde(default)]
    owner: BreakpointOwner,
}

fn enabled() -> bool {
    true
}

/// The entry point of a program, as the adapter names the frame: `main` or
/// `demo.Main.main(String[])`. It has no caller to return to.
fn is_main_frame(frame: &Frame) -> bool {
    let name = frame.name.trim();
    name == "main" || name.starts_with("main(") || name.contains(".main(")
}

/// An inline watch the user set in this project: an expression and the line it names.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct InlineWatchSetting {
    file: PathBuf,
    /// The line, from one.
    line: u32,
    expression: String,
}

/// The name the user gave an object, kept by what the program says the object is rather
/// than by a handle of the adapter: a handle may name another value at the next stop.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ObjectLabel {
    name: String,
    type_name: String,
    /// The number the language expression gave for the object.
    identity: i64,
}

/// A question about the identity of a value the tree or a watch shows.
#[derive(Clone, Debug, PartialEq, Eq)]
struct IdentityQuestion {
    reference: i64,
}

/// A value the window shows, whose identity it may need: the handle and the expression
/// that names the value in its frame.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ShownObject {
    reference: i64,
    name: String,
}

/// Whether two type names are the same type. A nested type is written with `.` in one
/// place and `$` in another; both name the same type.
fn same_type(left: &str, right: &str) -> bool {
    let left = left.trim();
    let right = right.trim();
    !left.is_empty() && left.replace('$', ".") == right.replace('$', ".")
}

impl BreakpointSetting {
    fn new(file: PathBuf, line: u32) -> Self {
        Self {
            file,
            line,
            condition: None,
            hit_condition: None,
            enabled: true,
            suspend: true,
            hit_message: false,
            log_expression: None,
            log_message: None,
            temporary: false,
            remove_once: false,
            suspend_thread: false,
            owner: BreakpointOwner::User,
        }
    }

    /// The message the adapter prints instead of stopping the program. A point that
    /// suspends the program has none: the window prints its message at the stop, and the
    /// program stays where it is. A non-suspending point without a message prints
    /// nothing, so it is not sent to the adapter at all.
    fn log_message(&self) -> Option<String> {
        if self.suspend {
            return None;
        }
        if let Some(message) = &self.log_message {
            return Some(message.clone());
        }
        let mut parts = Vec::new();
        if self.hit_message {
            parts.push(self.reached_text());
        }
        if let Some(expression) = self
            .log_expression
            .as_deref()
            .filter(|text| !text.trim().is_empty())
        {
            parts.push(format!("{{{expression}}}"));
        }
        (!parts.is_empty()).then(|| parts.join("\n"))
    }

    /// The place of the point in the text of the IDEA message. The adapter knows the
    /// file at the time the breakpoint is set, not the method that will reach it.
    fn reached_text(&self) -> String {
        let name = self
            .file
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        format!("Breakpoint reached at {name}:{}", self.line)
    }

    fn spec(&self) -> BreakpointSpec {
        let log_message = self.log_message();
        BreakpointSpec {
            file: self.file.clone(),
            line: self.line,
            condition: self.condition.clone(),
            hit_condition: self.hit_condition.clone(),
            enabled: self.enabled && (self.suspend || log_message.is_some()),
            log_message,
        }
    }
}

/// A method breakpoint: the name the adapter gets and the properties the user set.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct FunctionBreakpointSetting {
    file: PathBuf,
    /// The line of the declaration, from one; zero for a point that did not come from
    /// the gutter.
    #[serde(default)]
    line: u32,
    /// The type that declares the method, when the language named it.
    #[serde(default)]
    owner: Option<String>,
    name: String,
    #[serde(default)]
    condition: Option<String>,
    #[serde(default)]
    hit_condition: Option<String>,
    #[serde(default = "enabled")]
    enabled: bool,
}

impl FunctionBreakpointSetting {
    fn new(file: PathBuf, line: u32, owner: Option<String>, name: String) -> Self {
        Self {
            file,
            line,
            owner,
            name,
            condition: None,
            hit_condition: None,
            enabled: true,
        }
    }

    /// The name the adapter reads: `com.example.Foo#bar`.
    fn dap_name(&self) -> String {
        match self.owner.as_deref().filter(|owner| !owner.is_empty()) {
            Some(owner) => format!("{owner}#{}", self.name),
            None => self.name.clone(),
        }
    }

    fn spec(&self) -> FunctionBreakpointSpec {
        FunctionBreakpointSpec {
            name: self.dap_name(),
            condition: self.condition.clone(),
            hit_condition: self.hit_condition.clone(),
            enabled: self.enabled,
        }
    }
}

/// A field watchpoint: the field the adapter stops on and what stops it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct DataBreakpointSetting {
    /// The file of the declaration, when the point came from the gutter.
    #[serde(default)]
    file: PathBuf,
    /// The line of the declaration, from one; zero for a point that came from Variables.
    #[serde(default)]
    line: u32,
    /// The type that declares the field, when the language named it.
    #[serde(default)]
    owner: Option<String>,
    #[serde(default)]
    field: String,
    /// The id the adapter gave the field, `com.example.Foo#bar`.
    data_id: String,
    #[serde(default)]
    read: bool,
    #[serde(default = "enabled")]
    write: bool,
    #[serde(default)]
    condition: Option<String>,
    #[serde(default)]
    hit_condition: Option<String>,
    #[serde(default = "enabled")]
    enabled: bool,
}

impl DataBreakpointSetting {
    fn from_declaration(file: PathBuf, line: u32, owner: String, field: String) -> Self {
        let data_id = format!("{owner}#{field}");
        Self {
            file,
            line,
            owner: Some(owner),
            field,
            data_id,
            read: false,
            write: true,
            condition: None,
            hit_condition: None,
            enabled: true,
        }
    }

    fn from_variable(data_id: String, field: String) -> Self {
        Self {
            file: PathBuf::new(),
            line: 0,
            owner: data_id
                .split_once('#')
                .map(|(owner, _)| owner.to_owned())
                .filter(|owner| !owner.is_empty()),
            field,
            data_id,
            read: false,
            write: true,
            condition: None,
            hit_condition: None,
            enabled: true,
        }
    }

    fn access(&self) -> DataAccessType {
        match (self.read, self.write) {
            (true, true) => DataAccessType::ReadWrite,
            (true, false) => DataAccessType::Read,
            _ => DataAccessType::Write,
        }
    }

    fn spec(&self) -> DataBreakpointSpec {
        DataBreakpointSpec {
            data_id: self.data_id.clone(),
            access_type: self.access(),
            condition: self.condition.clone(),
            hit_condition: self.hit_condition.clone(),
            enabled: self.enabled,
        }
    }
}

/// The exception breakpoints of the window that do not come from a file: the one the
/// window always lists, and the ones the user named by exception class.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
struct ExceptionSetting {
    /// Whether the point is on.
    enabled: bool,
    /// Whether the program stops on a caught exception.
    caught: bool,
    /// Whether the program stops on an uncaught exception.
    uncaught: bool,
}

/// A named exception breakpoint: the class the user typed and how it stops.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct ExceptionPointSetting {
    /// The name of the exception class, as the user typed it.
    class: String,
    #[serde(flatten)]
    setting: ExceptionSetting,
}

impl ExceptionPointSetting {
    fn new(class: String) -> Self {
        Self {
            class,
            setting: ExceptionSetting {
                enabled: true,
                caught: false,
                uncaught: true,
            },
        }
    }
}

fn none_if_empty(text: String) -> Option<String> {
    let text = text.trim().to_owned();
    (!text.is_empty()).then_some(text)
}

fn apply_exception_edit(setting: &mut ExceptionSetting, edit: RowEdit) {
    match edit {
        RowEdit::Enabled(on) => setting.enabled = on,
        RowEdit::Caught(on) => setting.caught = on,
        RowEdit::Uncaught(on) => setting.uncaught = on,
        _ => {}
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct SavedState {
    breakpoints: Vec<BreakpointSetting>,
    function_breakpoints: Vec<FunctionBreakpointSetting>,
    data_breakpoints: Vec<DataBreakpointSetting>,
    exception_filters: Vec<String>,
    /// The point of any exception and the names the user gave the adapter's filters.
    any_exception: ExceptionSetting,
    /// The exception breakpoints the user named by class.
    exception_points: Vec<ExceptionPointSetting>,
    #[serde(default)]
    inline_watches: Vec<InlineWatchSetting>,
    /// The window muted the breakpoints; the settings above stay as they are.
    #[serde(default)]
    muted: bool,
}

fn state_key(root: &Path) -> String {
    format!("debug:{}", root.display())
}

/// A start that waits for the build step of its plan.
struct PendingLaunch {
    service: Arc<dyn DebugService>,
    program: proc_sup::ProcessSpec,
    name: String,
    entry: Option<PathBuf>,
}

/// The tab of the panel that is in front.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Front {
    #[default]
    Debugger,
    Console,
}

/// The last launch of the panel, so that Rerun starts the same program again.
#[derive(Clone)]
struct Restart {
    service: Arc<dyn DebugService>,
    before: Option<proc_sup::ProcessSpec>,
    program: proc_sup::ProcessSpec,
    grace: Duration,
    name: String,
    entry: Option<PathBuf>,
}

impl Restart {
    fn plan(&self) -> RunPlan {
        RunPlan {
            before: self.before.clone(),
            program: self.program.clone(),
            grace: self.grace,
        }
    }
}

/// A variable the user picked in the tree, with the group or object that holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct VariableTarget {
    reference: i64,
    name: String,
    value: String,
    /// The handle of the value itself, zero for a primitive.
    object: i64,
    type_name: String,
    /// The expression that names the value in its frame, when the adapter sent one.
    evaluate_name: Option<String>,
}

/// A value being typed in place of its row.
struct VariableEdit {
    target: VariableTarget,
    field: TextField,
}

/// A breakpoint Run to Cursor put on the line and takes away at the next stop.
struct TemporaryBreakpoint {
    file: PathBuf,
    line: u32,
    /// The line had no breakpoint of the user: it goes away with the temporary one.
    added: bool,
    /// The breakpoints Force Run to Cursor disabled until the line is reached.
    backup: Option<Vec<BreakpointSpec>>,
}

/// A tool that waits for the program to stop or the session to end.
struct PendingWait {
    job: ToolJob,
    deadline: Instant,
}

/// A tool that stepped the program and waits for the next stop.
struct PendingStep {
    job: ToolJob,
    /// The stops of the session before the step, so the stop left over from before does
    /// not end the wait.
    stops: u64,
    deadline: Instant,
}

/// A tool that reads a stack, and the thread whose stack it asked for.
struct PendingStack {
    job: ToolJob,
    thread: Option<i64>,
    frames: usize,
    deadline: Instant,
}

/// A tool that reads the variables of a frame. Children are asked for one level per
/// poll, so the window answers without waiting.
struct PendingVariables {
    job: ToolJob,
    frame: usize,
    depth: usize,
    variables: usize,
    /// The frame the adapter selected for the answer.
    selected: Option<i64>,
    /// The references whose children were asked for already.
    expanded: BTreeSet<i64>,
    deadline: Instant,
}

/// What a session tool waits for before it answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ToolControlKind {
    /// The program pauses, or the session ends.
    WaitForPause,
    /// The program moves and then pauses, ends or keeps running.
    Step,
    /// The program runs on and then pauses, ends or keeps running.
    Resume,
    /// A line is reached, the session ends, or the time is over.
    RunToLine,
}

/// A session tool that waits for the program while the window keeps drawing.
struct PendingToolControl {
    job: ToolJob,
    session: String,
    kind: ToolControlKind,
    /// The stops of the session before the step, so a stop left over from before does not
    /// end the wait.
    stops: u64,
    deadline: Instant,
}

pub struct DebugPanel {
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    debugger: Option<Debugger>,
    snapshot: Snapshot,
    /// The variable row the keys of the tree act on.
    selected_variable: Option<VariableTarget>,
    /// The row whose value is being edited, if any.
    variable_edit: Option<VariableEdit>,
    /// The new watch expression being typed, if any.
    watch_input: Option<TextField>,
    /// The watch row Delete removes.
    selected_watch: Option<String>,
    /// The expressions of the Evaluate window, oldest first.
    evaluate_history: Vec<String>,
    /// The temporary breakpoint of Run to Cursor.
    temporary: Option<TemporaryBreakpoint>,
    /// The breakpoints Force Step Over turned off until the step ends.
    forced_step: Option<Vec<BreakpointSpec>>,
    /// The stop the one-stop settings (temporary breakpoints, Force Step Over) last
    /// ended at; a snapshot of the same stop must not end the ones set after it.
    seen_stops: u64,
    /// The stop and the frame whose first scope the Variables list opened last.
    opened_frame: Option<(u64, Option<i64>)>,
    /// Smart Step Into waits for the list of calls from the adapter.
    asking_targets: bool,
    asking_targets_window: Option<AnyWindowHandle>,
    /// The window that holds the panel: a stop shows the file of its frame in it.
    window: Option<AnyWindowHandle>,
    /// The stop whose frame the window shows once the snapshot brings the frames of it.
    reveal_stop: Option<u64>,
    /// The window muted the breakpoints: the gutter greys them and the adapter hears an
    /// empty list, while the settings stay.
    muted: bool,
    /// Breakpoints in file and line order, with their properties.
    breakpoints: Vec<BreakpointSetting>,
    /// Method breakpoints, with their properties.
    function_breakpoints: Vec<FunctionBreakpointSetting>,
    /// Field watchpoints, with their properties.
    data_breakpoints: Vec<DataBreakpointSetting>,
    /// The point the Breakpoints window always lists.
    any_exception: ExceptionSetting,
    /// The exception breakpoints the user named by class.
    exception_points: Vec<ExceptionPointSetting>,
    /// Filters of the adapter the user turned on.
    enabled_filters: BTreeSet<String>,
    watch: Vec<String>,
    /// Inline watches of this project, kept until the user removes them.
    inline_watches: Vec<InlineWatchSetting>,
    /// Labels the user gave objects of this session, by the identity the program keeps
    /// for each object.
    labels: Vec<ObjectLabel>,
    /// The identity of the objects the adapter shows at the current stop, by handle. A
    /// handle may name another object at the next stop, so the map is emptied then.
    identities: BTreeMap<i64, Option<i64>>,
    /// The identity questions the panel waits for, by the number the session got.
    identity_questions: BTreeMap<u64, IdentityQuestion>,
    /// Marks whose identity was asked but has not arrived yet: the request, the name the
    /// user gave and the type of the object.
    pending_marks: Vec<(u64, String, String)>,
    /// The stop the identity map belongs to.
    identity_stop: u64,
    /// The number of the next identity question.
    next_identity: u64,
    /// The hide setting of this session when the button changed it; `None` follows the
    /// window setting.
    hide_library_frames: Option<bool>,
    /// The first frame of each library group the user opened.
    expanded_library_groups: BTreeSet<i64>,
    /// The renderers the language offers, sent to the session beside the user rules.
    language_renderers: Vec<TypeRenderer>,
    /// The service of the current session, for the probes the language answers.
    service: Option<Arc<dyn DebugService>>,
    /// The renderers the last stop was asked for, so an unchanged list is not sent again.
    sent_renderers: Vec<TypeRenderer>,
    /// The inline watch expressions the session already has.
    sent_inline_watches: Vec<String>,
    expanded: BTreeSet<i64>,
    /// The breakpoints of the last snapshot, to notice the adapter's answers.
    shown_breakpoints: Vec<Breakpoint>,
    /// The method breakpoint answers of the last snapshot.
    shown_function_breakpoints: Vec<Breakpoint>,
    /// The field watchpoint answers of the last snapshot.
    shown_data_breakpoints: Vec<Breakpoint>,
    /// Output of the build step and of a test run, before the adapter's own console.
    local_console: Vec<ConsoleLine>,
    /// The console the adapter sent last, to keep the shown log in order as the lines the
    /// user types join it.
    seen_console: Vec<ConsoleLine>,
    /// The line the user types into the console, while the program reads input.
    console_input: ConsoleInput,
    /// The search bar of the console and the lines its filter keeps.
    console_search: ConsoleSearch,
    search_focus: FocusHandle,
    filter_focus: FocusHandle,
    /// The project files, to find the sources named in the console.
    files: Option<Entity<Arc<Vec<PathBuf>>>>,
    /// The build step of a debug launch, before the debuggee starts.
    build: Option<Console>,
    /// The unfinished lines of the build process, each stream on its own.
    build_output: OutputBuffers,
    /// The process of the build step whose output comes now.
    build_process: Option<u32>,
    /// The build step of the last launch failed: the status says so until the next session.
    build_failed: bool,
    /// The user stopped the build step, so the exit of the build is no failure.
    build_stopped: bool,
    /// A stop of the session has been shown in the Debugger tab. Until then a program that
    /// reads its input keeps the console in front.
    stop_shown: bool,
    pending: Option<PendingLaunch>,
    /// The launch of the last session, for Rerun.
    restart: Option<Restart>,
    /// Which tab is in front, the debugger trees or the console.
    front: Front,
    /// The width of the frames column, changed by the divider.
    tree_width: f32,
    /// Where the pointer was when the divider drag started.
    split_from: Option<f32>,
    /// Held while a program this window launched is under the debugger; dropping it
    /// lets the language turn compilation on save back on.
    session: Option<Box<dyn DebugSessionToken>>,
    store: Option<Arc<Store>>,
    root: PathBuf,
    /// The state folder of the window, for the file of the whole console of a tool's
    /// session.
    state_dir: PathBuf,
    /// The file of the whole console of the session a tool started, and how many lines
    /// of it are already there.
    tool_output_file: Option<PathBuf>,
    tool_output_lines: usize,
    title: SharedString,
    scroll_tree: ScrollHandle,
    /// The console list and the rule that keeps it at the last line while the user
    /// reads the end.
    console_scroll: ConsoleFollow,
    scroll_variables: ScrollHandle,
    scroll_watch: ScrollHandle,
    /// The values of the frame at the last stop, by name, for the colour of changed ones.
    stop_values: BTreeMap<String, (i64, String, String)>,
    /// The names whose value differs from the previous stop.
    changed_values: BTreeSet<String>,
    /// The stop whose values are in `stop_values`; zero before the first stop.
    values_stop: u64,
    /// The bridge of the MCP debug tools, taken in every poll.
    tool_debug: Option<Arc<DebugBridge>>,
    /// The tools that wait for the program while the window keeps drawing.
    tool_waits: Vec<PendingWait>,
    tool_steps: Vec<PendingStep>,
    tool_stacks: Vec<PendingStack>,
    tool_variables: Vec<PendingVariables>,
    /// The id an answer of `debug_run` names.
    next_tool_session: u64,
    /// The client whose tool asked for the session that is about to start, until the
    /// console says so.
    tool_client: Option<String>,
    /// The id and the name a tool gave the session it started.
    tool_session: Option<(String, String)>,
    /// The session tools that wait for the program to pause or end.
    tool_controls: Vec<PendingToolControl>,
}

impl EventEmitter<BreakpointsChanged> for DebugPanel {}
impl EventEmitter<ValuesChanged> for DebugPanel {}

impl DebugPanel {
    /// `root` and `store` come from the caller: the panel is made while the workspace is
    /// still being updated, so its handle cannot be read here.
    pub fn create(
        root: PathBuf,
        store: Option<Arc<Store>>,
        state_dir: PathBuf,
        workspace: WeakEntity<Workspace>,
        cx: &mut App,
    ) -> Entity<Self> {
        let saved = store
            .as_ref()
            .and_then(|store| store.get(&state_key(&root)).ok().flatten())
            .and_then(|json| serde_json::from_str::<SavedState>(&json).ok())
            .unwrap_or_default();
        // A list saved before the window had the point of any exception keeps its
        // filters: the two the point owns move into it.
        let mut enabled_filters: BTreeSet<String> =
            saved.exception_filters.iter().cloned().collect();
        let mut any_exception = saved.any_exception;
        if !any_exception.enabled {
            let caught = enabled_filters.remove("caught");
            let uncaught = enabled_filters.remove("uncaught");
            if caught || uncaught {
                any_exception.enabled = true;
                any_exception.caught = caught;
                any_exception.uncaught = uncaught;
            }
        }
        bind_keys(cx);
        let panel = cx.new(|cx| Self {
            workspace: workspace.clone(),
            focus: cx.focus_handle(),
            debugger: None,
            snapshot: Snapshot::default(),
            selected_variable: None,
            variable_edit: None,
            watch_input: None,
            selected_watch: None,
            evaluate_history: Vec::new(),
            temporary: None,
            forced_step: None,
            seen_stops: 0,
            opened_frame: None,
            asking_targets: false,
            asking_targets_window: None,
            window: None,
            reveal_stop: None,
            muted: saved.muted,
            breakpoints: saved.breakpoints,
            function_breakpoints: saved.function_breakpoints,
            data_breakpoints: saved.data_breakpoints,
            any_exception,
            exception_points: saved.exception_points,
            enabled_filters,
            watch: Vec::new(),
            inline_watches: saved.inline_watches,
            labels: Vec::new(),
            identities: BTreeMap::new(),
            identity_questions: BTreeMap::new(),
            pending_marks: Vec::new(),
            identity_stop: 0,
            next_identity: 1,
            hide_library_frames: None,
            expanded_library_groups: BTreeSet::new(),
            language_renderers: Vec::new(),
            service: None,
            sent_renderers: Vec::new(),
            sent_inline_watches: Vec::new(),
            expanded: BTreeSet::new(),
            shown_breakpoints: Vec::new(),
            shown_function_breakpoints: Vec::new(),
            shown_data_breakpoints: Vec::new(),
            local_console: Vec::new(),
            seen_console: Vec::new(),
            console_input: ConsoleInput::new(),
            console_search: ConsoleSearch::default(),
            search_focus: cx.focus_handle(),
            filter_focus: cx.focus_handle(),
            files: None,
            build: None,
            build_output: OutputBuffers::default(),
            build_process: None,
            build_failed: false,
            build_stopped: false,
            stop_shown: false,
            pending: None,
            restart: None,
            front: Front::Debugger,
            tree_width: TREE_WIDTH,
            split_from: None,
            session: None,
            store,
            root,
            state_dir,
            tool_output_file: None,
            tool_output_lines: 0,
            title: "Debug".into(),
            scroll_tree: ScrollHandle::new(),
            console_scroll: ConsoleFollow::new(),
            scroll_variables: ScrollHandle::new(),
            scroll_watch: ScrollHandle::new(),
            stop_values: BTreeMap::new(),
            changed_values: BTreeSet::new(),
            values_stop: 0,
            tool_debug: None,
            tool_waits: Vec::new(),
            tool_steps: Vec::new(),
            tool_stacks: Vec::new(),
            tool_variables: Vec::new(),
            next_tool_session: 0,
            tool_client: None,
            tool_session: None,
            tool_controls: Vec::new(),
        });
        let polling = panel.downgrade();
        cx.spawn(async move |cx| {
            loop {
                cx.background_executor().timer(POLL).await;
                if polling.update(cx, |panel, cx| panel.poll(cx)).is_err() {
                    return;
                }
            }
        })
        .detach();
        panel
    }

    pub fn focus(&self) -> FocusHandle {
        self.focus.clone()
    }

    pub fn debugger(&self) -> Option<&Debugger> {
        self.debugger.as_ref()
    }

    /// The newest snapshot of the session, for scripts and tests.
    pub fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }

    /// Shows a stack without an adapter, for the tests of the list of frames.
    #[cfg(test)]
    pub(crate) fn show_snapshot_for_test(&mut self, snapshot: Snapshot, cx: &mut Context<Self>) {
        let (commands, _commands) = tokio::sync::mpsc::unbounded_channel();
        let (_snapshots, incoming) = std::sync::mpsc::channel();
        self.debugger = Some(Debugger::new(commands, incoming));
        self.seen_stops = snapshot.stops;
        self.values_stop = snapshot.stops;
        self.opened_frame = Some((snapshot.stops, snapshot.selected_frame));
        self.snapshot = snapshot;
        self.front = Front::Debugger;
        cx.notify();
    }

    /// The project files, to find the sources named in the console.
    pub fn set_files(&mut self, files: Entity<Arc<Vec<PathBuf>>>) {
        self.files = Some(files);
    }

    /// Whether the breakpoints are muted.
    pub fn muted(&self) -> bool {
        self.muted
    }

    /// Mutes the breakpoints, or brings them back into the program without a restart.
    pub fn toggle_mute(&mut self, cx: &mut Context<Self>) {
        self.muted = !self.muted;
        self.send(Command::SetMuteBreakpoints(self.muted), cx);
        if !self.muted {
            // A session that started muted never heard these breakpoints, so the panel
            // sends every one of them again.
            for spec in self.all_specs() {
                self.send(Command::SetBreakpoint(spec), cx);
            }
            self.send_function_breakpoints(cx);
            self.send_data_breakpoints(cx);
        }
        self.changed(cx);
    }

    /// Connects the tools of the MCP server to this window: their breakpoints, their
    /// session and their steps go through the same gutter, trees and console as the
    /// buttons of the user.
    pub(crate) fn set_tool_debug(&mut self, bridge: Arc<DebugBridge>) {
        self.tool_debug = Some(bridge);
    }

    /// Takes the jobs the MCP tools left and answers the ones that can be answered now.
    fn poll_tool_jobs(&mut self, cx: &mut Context<Self>) {
        let Some(bridge) = self.tool_debug.clone() else {
            return;
        };
        while let Some(job) = bridge.take_job() {
            self.take_tool_job(job, cx);
        }
        self.answer_tool_jobs(cx);
    }

    fn take_tool_job(&mut self, job: ToolJob, cx: &mut Context<Self>) {
        match job.request.clone() {
            ToolRequest::SetBreakpoint(query) => {
                let message = query.log_message.clone();
                match self.set_breakpoint_from_tool(
                    &query.path,
                    query.line,
                    query.condition,
                    message,
                    cx,
                ) {
                    Ok(info) => job.answer(ToolAnswer::Breakpoint(info)),
                    Err(message) => job.fail(message),
                }
            }
            ToolRequest::RemoveBreakpoint(id) => {
                self.remove_breakpoint(id.path, id.line, cx);
                job.answer(ToolAnswer::Removed);
            }
            ToolRequest::Breakpoints => {
                job.answer(ToolAnswer::Breakpoints(self.tool_breakpoints()));
            }
            ToolRequest::Run(request) => {
                match self.start_tool_debug(&request, job.client.as_deref(), cx) {
                    Ok(session) => job.answer(ToolAnswer::Started(session)),
                    Err(message) => job.fail(message),
                }
            }
            ToolRequest::Wait(query) => {
                let seconds = query.seconds.clamp(1, 120);
                self.tool_waits.push(PendingWait {
                    job,
                    deadline: Instant::now() + Duration::from_secs(seconds),
                });
            }
            ToolRequest::Step { step, seconds } => {
                if self.debugger.is_none()
                    || !matches!(self.snapshot.state, DebugState::Stopped { .. })
                {
                    job.fail("the program is not stopped");
                    return;
                }
                let stops = self.snapshot.stops;
                let command = match step {
                    tool_gateway::DebugStep::Over => Command::Next,
                    tool_gateway::DebugStep::Into => Command::StepIn,
                    tool_gateway::DebugStep::Out => Command::StepOut,
                    tool_gateway::DebugStep::Continue => Command::Continue,
                };
                self.run_away(cx);
                self.send(command, cx);
                let seconds = seconds.clamp(1, 120);
                self.tool_steps.push(PendingStep {
                    job,
                    stops,
                    deadline: Instant::now() + Duration::from_secs(seconds),
                });
            }
            ToolRequest::Stack(query) => {
                let thread = query.thread;
                if let (Some(id), Some(current)) = (thread, self.snapshot.current_thread)
                    && id != current
                {
                    self.send(Command::SelectThread(id), cx);
                }
                self.tool_stacks.push(PendingStack {
                    job,
                    thread,
                    frames: query.frames,
                    deadline: Instant::now() + Duration::from_secs(10),
                });
            }
            ToolRequest::Variables(query) => {
                self.tool_variables.push(PendingVariables {
                    job,
                    frame: query.frame,
                    depth: query.depth,
                    variables: query.variables,
                    selected: None,
                    expanded: BTreeSet::new(),
                    deadline: Instant::now() + Duration::from_secs(10),
                });
            }
            ToolRequest::Evaluate(query) => self.evaluate_for_tool(job, query, cx),
            ToolRequest::Debug(query) => self.take_debug_job(job, query, cx),
            ToolRequest::Stop => {
                if self.debugger.is_none()
                    || matches!(
                        self.snapshot.state,
                        DebugState::Terminated | DebugState::Inactive
                    )
                {
                    job.fail("debugging is not running");
                    return;
                }
                self.stop(cx);
                job.answer(ToolAnswer::Stopped);
            }
        }
    }

    /// The sessions of the window as `xdebug_get_debugger_status` names them. The window
    /// runs one session at a time; a session a tool started keeps the id it got.
    fn tool_sessions(&self) -> tool_gateway::DebuggerStatus {
        let Some(entry) = self.tool_session_entry() else {
            return tool_gateway::DebuggerStatus::default();
        };
        tool_gateway::DebuggerStatus {
            active_session_id: Some(entry.id.clone()),
            sessions: vec![entry],
        }
    }

    fn tool_session_entry(&self) -> Option<tool_gateway::DebuggerSession> {
        let status = match &self.snapshot.state {
            DebugState::Inactive | DebugState::Terminated => return None,
            DebugState::Starting => "starting",
            DebugState::Running => "running",
            DebugState::Stopped { .. } => "paused",
        };
        // A session of the Run window may be on its way: the tools see it as starting
        // before the debugger of the window exists.
        if self.debugger.is_none() && status != "starting" {
            return None;
        }
        let (id, name) = self
            .tool_session
            .clone()
            .unwrap_or_else(|| ("debug-current".to_string(), "Debug session".to_string()));
        let preview = self
            .local_console
            .iter()
            .rev()
            .take(20)
            .map(|line| ansi::plain(&line.text))
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n");
        Some(tool_gateway::DebuggerSession {
            id,
            name: name.clone(),
            status: status.to_string(),
            run_configuration: Some(name),
            output_preview: (!preview.is_empty()).then_some(preview),
            full_output_path: self
                .tool_output_file
                .as_ref()
                .filter(|path| path.is_file())
                .cloned(),
            exit_code: self.snapshot.exit_code,
        })
    }

    /// The file with the whole console of a session a tool started.
    fn tool_output_path(&self, id: &str) -> PathBuf {
        self.state_dir
            .join("debug-output")
            .join(crate::project_state::project_key(&self.root))
            .join(format!("{id}.log"))
    }

    /// Writes the lines of the console that are not in the file of a tool's session yet.
    fn flush_tool_output(&mut self) {
        let (Some(_), Some(path)) = (&self.tool_session, &self.tool_output_file) else {
            return;
        };
        let count = self.local_console.len();
        if count == self.tool_output_lines {
            return;
        }
        let path = path.clone();
        let lines: Vec<String> = self
            .local_console
            .iter()
            .skip(self.tool_output_lines)
            .map(|line| ansi::plain(&line.text))
            .collect();
        let rewrite =
            count < self.tool_output_lines || self.tool_output_lines == 0 || count >= MAX_CONSOLE;
        if rewrite {
            let text: String = self
                .local_console
                .iter()
                .map(|line| format!("{}\n", ansi::plain(&line.text)))
                .collect();
            if std::fs::write(&path, text).is_ok() {
                self.tool_output_lines = count;
            }
            return;
        }
        let mut text = String::new();
        for line in lines {
            text.push_str(&line);
            text.push('\n');
        }
        use std::io::Write as _;
        let Ok(mut file) = std::fs::OpenOptions::new().append(true).open(&path) else {
            return;
        };
        if file.write_all(text.as_bytes()).is_ok() {
            self.tool_output_lines = count;
        }
    }

    /// The id of the session a session tool works on, or the reason it cannot.
    fn tool_session_id(&self, requested: &Option<String>) -> Result<String, String> {
        let Some(entry) = self.tool_session_entry() else {
            return Err("no debug session is running".to_string());
        };
        match requested {
            Some(id) if id != &entry.id => Err(format!("no debug session has the id {id}")),
            _ => Ok(entry.id),
        }
    }

    fn console_tail(&self, limit: usize) -> Vec<String> {
        self.local_console
            .iter()
            .rev()
            .take(limit)
            .map(|line| ansi::plain(&line.text))
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect()
    }

    fn breakpoint_errors_tail(&self, limit: usize) -> Vec<String> {
        self.snapshot
            .breakpoints
            .iter()
            .filter(|breakpoint| !breakpoint.verified)
            .map(|breakpoint| {
                let message = breakpoint
                    .message
                    .as_deref()
                    .unwrap_or("the adapter did not accept the place");
                format!(
                    "{}:{}: {message}",
                    breakpoint.file.display(),
                    breakpoint.line
                )
            })
            .rev()
            .take(limit)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect()
    }

    /// The state a control tool answers with.
    fn tool_control_outcome(
        &self,
        session: &str,
        events_limit: usize,
        tail: bool,
    ) -> DebugControlOutcome {
        let (status, paused, reason) = match &self.snapshot.state {
            DebugState::Stopped { reason } => ("paused".to_string(), true, Some(reason.clone())),
            DebugState::Running => ("running".to_string(), false, None),
            DebugState::Starting => ("running".to_string(), false, None),
            DebugState::Inactive | DebugState::Terminated => ("stopped".to_string(), false, None),
        };
        DebugControlOutcome {
            session_id: session.to_string(),
            status,
            paused,
            reason,
            frame_values: Vec::new(),
            breakpoint_errors_tail: if tail {
                self.breakpoint_errors_tail(events_limit)
            } else {
                Vec::new()
            },
            tracepoint_outputs_tail: if tail {
                self.console_tail(events_limit)
            } else {
                Vec::new()
            },
        }
    }

    fn take_debug_job(&mut self, job: ToolJob, query: DebugQuery, cx: &mut Context<Self>) {
        match query {
            DebugQuery::Status => {
                job.answer(ToolAnswer::Debug(DebugAnswer::Status(self.tool_sessions())));
            }
            DebugQuery::Start(query) => match self.start_tool_session(&query, cx) {
                Ok(session) => job.answer(ToolAnswer::Debug(DebugAnswer::Started(session))),
                Err(message) => job.fail(message),
            },
            DebugQuery::Control(query) => self.control_tool_session(job, query, cx),
            DebugQuery::RunToLine(query) => {
                let session = match self.tool_session_id(&query.session_id) {
                    Ok(session) => session,
                    Err(message) => return job.fail(message),
                };
                if !matches!(self.snapshot.state, DebugState::Stopped { .. }) {
                    return job.fail("the program is not stopped");
                }
                let stops = self.snapshot.stops;
                self.start_run_to_cursor(query.file.clone(), query.line, false, cx);
                self.tool_controls.push(PendingToolControl {
                    job,
                    session,
                    kind: ToolControlKind::RunToLine,
                    stops,
                    deadline: Instant::now() + Duration::from_millis(query.timeout_ms.min(600_000)),
                });
            }
            DebugQuery::Threads(query) => {
                let page = self.tool_threads(&query);
                job.answer(ToolAnswer::Debug(DebugAnswer::Threads(page)));
            }
            DebugQuery::Evaluate(query) => self.evaluate_tree_for_tool(job, query, cx),
            DebugQuery::SetVariable(query) => self.set_variable_for_tool(job, query, cx),
            DebugQuery::Breakpoints(file) => {
                let points = self.tool_breakpoints_in(file.as_deref());
                job.answer(ToolAnswer::Debug(DebugAnswer::Breakpoints(points)));
            }
            DebugQuery::SetBreakpoint(query) => match self.set_tool_point(&query, cx) {
                Ok(point) => job.answer(ToolAnswer::Debug(DebugAnswer::Breakpoint(point))),
                Err(message) => job.fail(message),
            },
            DebugQuery::RemoveBreakpoints(query) => {
                let removed = self.remove_tool_points(&query, cx);
                job.answer(ToolAnswer::Debug(DebugAnswer::Removed(removed)));
            }
            // The bridge reads the frames and the values through the older helpers.
            DebugQuery::Frames(_) | DebugQuery::Values(_) => {
                job.fail("the bridge answers the frames and the values of a frame");
            }
        }
    }

    fn control_tool_session(
        &mut self,
        job: ToolJob,
        query: tool_gateway::DebugControlQuery,
        cx: &mut Context<Self>,
    ) {
        let session = match self.tool_session_id(&query.session_id) {
            Ok(session) => session,
            Err(message) => return job.fail(message),
        };
        let deadline = Instant::now() + Duration::from_millis(query.timeout_ms.min(600_000));
        let stopped = matches!(self.snapshot.state, DebugState::Stopped { .. });
        let running =
            self.debugger.is_some() || matches!(self.snapshot.state, DebugState::Starting);
        let events_limit = query.events_limit;
        match query.action {
            DebugControlAction::DrainEvents => {
                let mut outcome = self.tool_control_outcome(&session, events_limit, true);
                outcome.frame_values.clear();
                job.answer(ToolAnswer::Debug(DebugAnswer::Controlled(outcome)));
            }
            DebugControlAction::Stop => {
                if !running {
                    return job.fail("debugging is not running");
                }
                self.stop(cx);
                let outcome = self.tool_control_outcome(&session, events_limit, false);
                job.answer(ToolAnswer::Debug(DebugAnswer::Controlled(outcome)));
            }
            DebugControlAction::Pause => {
                if !running {
                    return job.fail("debugging is not running");
                }
                self.send(Command::Pause, cx);
                let outcome = self.tool_control_outcome(&session, events_limit, false);
                job.answer(ToolAnswer::Debug(DebugAnswer::Controlled(outcome)));
            }
            DebugControlAction::Resume
            | DebugControlAction::StepInto
            | DebugControlAction::StepOver
            | DebugControlAction::StepOut => {
                if !stopped {
                    return job.fail("the program is not stopped");
                }
                let stops = self.snapshot.stops;
                self.run_away(cx);
                let command = match query.action {
                    DebugControlAction::StepInto => Command::StepIn,
                    DebugControlAction::StepOver => Command::Next,
                    DebugControlAction::StepOut => Command::StepOut,
                    _ => Command::Continue,
                };
                self.send(command, cx);
                let kind = if matches!(
                    query.action,
                    DebugControlAction::StepInto
                        | DebugControlAction::StepOver
                        | DebugControlAction::StepOut
                ) {
                    ToolControlKind::Step
                } else {
                    ToolControlKind::Resume
                };
                self.tool_controls.push(PendingToolControl {
                    job,
                    session,
                    kind,
                    stops,
                    deadline,
                });
            }
            DebugControlAction::WaitForPause => {
                if stopped {
                    let outcome = self.tool_control_outcome(&session, events_limit, false);
                    job.answer(ToolAnswer::Debug(DebugAnswer::Controlled(outcome)));
                    return;
                }
                if !running {
                    return job.fail("debugging is not running");
                }
                self.tool_controls.push(PendingToolControl {
                    job,
                    session,
                    kind: ToolControlKind::WaitForPause,
                    stops: self.snapshot.stops,
                    deadline,
                });
            }
        }
    }

    /// Answers the session tools whose program moved.
    fn answer_tool_controls(&mut self, now: Instant) {
        let controls = std::mem::take(&mut self.tool_controls);
        let mut keep = Vec::new();
        for control in controls {
            let stopped = matches!(self.snapshot.state, DebugState::Stopped { .. });
            let over = matches!(
                self.snapshot.state,
                DebugState::Terminated | DebugState::Inactive
            ) || (self.debugger.is_none()
                && !matches!(self.snapshot.state, DebugState::Starting));
            let moved = stopped && self.snapshot.stops > control.stops;
            let reached = match control.kind {
                ToolControlKind::WaitForPause | ToolControlKind::Step => moved || over,
                ToolControlKind::Resume => moved || over || now >= control.deadline,
                ToolControlKind::RunToLine => moved || over,
            };
            if reached || now >= control.deadline {
                let tail = matches!(
                    control.kind,
                    ToolControlKind::RunToLine | ToolControlKind::Step
                );
                let mut outcome = self.tool_control_outcome(&control.session, 100, tail);
                if !reached && matches!(control.kind, ToolControlKind::RunToLine) {
                    // IDEA names the end of the wait of Run to Line a timeout.
                    outcome.status = "timeout".to_string();
                }
                control
                    .job
                    .answer(ToolAnswer::Debug(DebugAnswer::Controlled(outcome)));
            } else {
                keep.push(control);
            }
        }
        self.tool_controls = keep;
    }

    fn tool_threads(&self, query: &DebugThreadsQuery) -> DebugThreadPage {
        let current = self.snapshot.current_thread;
        let state = match &self.snapshot.state {
            DebugState::Running | DebugState::Starting => "running",
            DebugState::Stopped { .. } => "paused",
            _ => "unknown",
        };
        let total = self.snapshot.threads.len();
        let threads = self
            .snapshot
            .threads
            .iter()
            .skip(query.offset)
            .take(query.limit)
            .map(|thread| {
                let virtual_thread = dap_client::is_virtual_thread(&thread.name);
                DebugThreadInfo {
                    id: thread.id,
                    name: thread.name.clone(),
                    state: state.to_string(),
                    is_current: current == Some(thread.id),
                    additional_info: virtual_thread.then(|| "virtual".to_string()),
                    additional_info_tooltip: virtual_thread
                        .then(|| "A virtual thread of the runtime".to_string()),
                    frame_count: if current == Some(thread.id) {
                        self.snapshot.frames.len()
                    } else {
                        0
                    },
                }
            })
            .collect();
        DebugThreadPage { threads, total }
    }

    fn evaluate_tree_for_tool(
        &mut self,
        job: ToolJob,
        query: DebugExpressionQuery,
        cx: &mut Context<Self>,
    ) {
        let Some(debugger) = self.debugger.clone() else {
            return job.fail("debugging is not running");
        };
        if !matches!(self.snapshot.state, DebugState::Stopped { .. }) {
            return job.fail("the program is not stopped");
        }
        let index = query.frame_index.unwrap_or(0);
        let Some(frame) = self.snapshot.frames.get(index).map(|frame| frame.id) else {
            return job.fail("the stack has no such frame");
        };
        let expression = query.expression;
        let depth = query.depth;
        cx.spawn(async move |this, cx| {
            let arguments = serde_json::json!({
                "expression": expression,
                "frameId": frame,
                "context": "repl",
            });
            let answer = debugger.request("evaluate", arguments).await;
            let (mut node, reference) = match answer {
                Ok(body) => dap_value(&expression, &body),
                Err(error) => {
                    this.update(cx, |_, cx| {
                        job.fail(format!(
                            "the program could not evaluate the expression: {error}"
                        ));
                        cx.notify();
                    })
                    .ok();
                    return;
                }
            };
            fill_tree(&debugger, &mut node, reference, depth).await;
            this.update(cx, |_, cx| {
                job.answer(ToolAnswer::Debug(DebugAnswer::Value(node)));
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn set_variable_for_tool(
        &mut self,
        job: ToolJob,
        query: DebugSetVariableQuery,
        cx: &mut Context<Self>,
    ) {
        let Some(debugger) = self.debugger.clone() else {
            return job.fail("debugging is not running");
        };
        if !matches!(self.snapshot.state, DebugState::Stopped { .. }) {
            return job.fail("the program is not stopped");
        }
        let Some(frame) = self
            .snapshot
            .frames
            .get(query.frame_index)
            .map(|frame| frame.id)
        else {
            return job.fail("the stack has no such frame");
        };
        let Some((name, reference)) = self.value_reference(&query.path) else {
            return job.fail(format!(
                "the frame has no variable {}",
                expression_of(&query.path)
            ));
        };
        let parent = parent_expression(&query.path);
        let expression = expression_of(&query.path);
        let value = query.new_value;
        let old_value = self.value_text(&query.path);
        cx.spawn(async move |this, cx| {
            let mut holder = reference;
            if holder.is_none()
                && let Some(parent) = parent
                && let Ok(body) = debugger
                    .request(
                        "evaluate",
                        serde_json::json!({
                            "expression": parent,
                            "frameId": frame,
                            "context": "repl",
                        }),
                    )
                    .await
            {
                holder = body
                    .get("variablesReference")
                    .and_then(serde_json::Value::as_i64)
                    .filter(|reference| *reference > 0);
            }
            let mut changed = None;
            let mut error = None;
            if let Some(reference) = holder {
                match debugger
                    .request(
                        "setVariable",
                        serde_json::json!({
                            "variablesReference": reference,
                            "name": name,
                            "value": value,
                        }),
                    )
                    .await
                {
                    Ok(body) => changed = Some(body),
                    Err(reason) => error = Some(reason),
                }
            }
            if changed.is_none() {
                match debugger
                    .request(
                        "setExpression",
                        serde_json::json!({
                            "expression": expression,
                            "value": value,
                            "frameId": frame,
                        }),
                    )
                    .await
                {
                    Ok(body) => changed = Some(body),
                    Err(reason) => error = Some(reason),
                }
            }
            let answer = match changed {
                Some(body) => {
                    let new_value = body
                        .get("value")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or(&value)
                        .to_string();
                    Ok(DebugSetVariableOutcome {
                        old_value,
                        new_value,
                        applied: true,
                    })
                }
                None => Err(format!(
                    "the adapter could not set the value: {}",
                    error.unwrap_or_else(|| "no answer".to_string())
                )),
            };
            this.update(cx, |_, cx| {
                match answer {
                    Ok(outcome) => job.answer(ToolAnswer::Debug(DebugAnswer::Set(outcome))),
                    Err(message) => job.fail(message),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The group that holds the first name of a path and the value of the name, for the
    /// request that sets a variable of it.
    fn value_reference(&self, path: &[String]) -> Option<(String, Option<i64>)> {
        let first = path.first()?;
        if path.len() > 1 {
            return Some((path.last()?.trim_matches(['[', ']']).to_string(), None));
        }
        let name = first.trim_matches(['[', ']']).to_string();
        for scope in &self.snapshot.scopes {
            let Some(variables) = self.snapshot.variables.get(&scope.reference) else {
                continue;
            };
            if variables.iter().any(|variable| variable.name == name) {
                return Some((name, Some(scope.reference)));
            }
        }
        Some((name, None))
    }

    /// The value a path names in the loaded tree of the frame.
    fn value_text(&self, path: &[String]) -> Option<String> {
        let first = path.first()?.trim_matches(['[', ']']);
        let mut current = self
            .snapshot
            .scopes
            .iter()
            .filter_map(|scope| self.snapshot.variables.get(&scope.reference))
            .flat_map(|variables| variables.iter())
            .find(|variable| variable.name == first)?;
        let mut text = current.value.clone();
        for step in &path[1..] {
            let inner = step.trim_matches(['[', ']']);
            let children = self.snapshot.variables.get(&current.reference)?;
            current = children
                .iter()
                .find(|variable| variable.name == inner || variable.name == format!("[{inner}]"))?;
            text = current.value.clone();
        }
        Some(text)
    }

    fn tool_breakpoints_in(&self, file: Option<&Path>) -> Vec<DebugBreakpoint> {
        self.breakpoints
            .iter()
            .filter(|setting| file.is_none_or(|file| setting.file == file))
            .map(|setting| self.tool_point(setting))
            .collect()
    }

    fn tool_point(&self, setting: &BreakpointSetting) -> DebugBreakpoint {
        let is_log =
            !setting.suspend && (setting.log_expression.is_some() || setting.log_message.is_some());
        DebugBreakpoint {
            id: tool_gateway::breakpoint_id(&setting.file, setting.line),
            kind: "LINE".to_string(),
            file: setting.file.clone(),
            line: setting.line,
            enabled: setting.enabled,
            verified: self.verified_breakpoint(&setting.file, setting.line),
            owner: setting.owner,
            condition: setting.condition.clone(),
            is_log_message: is_log,
            is_log_stack: false,
            temporary: setting.temporary,
            suspend_policy: if setting.suspend { "ALL" } else { "NONE" }.to_string(),
            hit_count: setting
                .hit_condition
                .as_deref()
                .and_then(hit_count_of)
                .unwrap_or(0),
            line_text: line_text(&setting.file, setting.line),
        }
    }

    fn set_tool_point(
        &mut self,
        query: &DebugBreakpointQuery,
        cx: &mut Context<Self>,
    ) -> Result<DebugBreakpoint, String> {
        let file = query
            .file_path
            .clone()
            .ok_or_else(|| "a breakpoint needs a file".to_string())?;
        let line = query
            .line
            .filter(|line| *line > 0)
            .ok_or_else(|| "lines start at 1".to_string())?;
        let existing = self
            .breakpoints
            .iter()
            .position(|breakpoint| breakpoint.file == file && breakpoint.line == line);
        let mut setting = match existing {
            Some(index) => self.breakpoints.remove(index),
            None => BreakpointSetting::new(file.clone(), line),
        };
        if let Some(condition) = &query.condition {
            setting.condition = condition
                .as_deref()
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_string);
        }
        if let Some(log) = query.is_log_message {
            if log {
                setting.suspend = false;
                setting.hit_message = false;
                setting.log_expression = setting.condition.clone();
            } else {
                setting.suspend = true;
                setting.log_expression = None;
            }
        }
        if let Some(policy) = &query.suspend_policy {
            match policy.as_str() {
                "ALL" => {
                    setting.suspend = true;
                    setting.hit_message = false;
                }
                "NONE" => {
                    setting.suspend = false;
                    setting.hit_message = true;
                    setting.log_expression = None;
                }
                other => return Err(format!("suspendPolicy is ALL or NONE, not {other}")),
            }
        }
        if let Some(count) = query.hit_count {
            if count == 0 {
                return Err("hitCount starts at 1".to_string());
            }
            setting.hit_condition = Some(format!(">= {count}"));
        }
        if let Some(temporary) = query.temporary {
            setting.temporary = temporary;
        }
        if let Some(enabled) = query.enabled {
            setting.enabled = enabled;
        }
        setting.owner = query.owner.unwrap_or(BreakpointOwner::Agent);
        let spec = setting.spec();
        self.send(Command::SetBreakpoint(spec), cx);
        let point = self.tool_point(&setting);
        self.breakpoints.push(setting);
        self.breakpoints
            .sort_by(|left, right| (&left.file, left.line).cmp(&(&right.file, right.line)));
        self.changed(cx);
        Ok(point)
    }

    fn remove_tool_points(
        &mut self,
        query: &DebugRemoveBreakpointsQuery,
        cx: &mut Context<Self>,
    ) -> usize {
        let mut removed = Vec::new();
        self.breakpoints.retain(|setting| {
            let owner = query.owner.is_none_or(|owner| setting.owner == owner);
            let id = query
                .breakpoint_id
                .as_ref()
                .is_none_or(|id| tool_gateway::breakpoint_id(&setting.file, setting.line) == *id);
            let file = query
                .file_path
                .as_ref()
                .is_none_or(|file| &setting.file == file);
            let line = query.line.is_none_or(|line| setting.line == line);
            if owner && id && file && line {
                removed.push((setting.file.clone(), setting.line));
                false
            } else {
                true
            }
        });
        for (file, line) in &removed {
            self.send(
                Command::RemoveBreakpoint {
                    file: file.clone(),
                    line: *line,
                },
                cx,
            );
        }
        if !removed.is_empty() {
            self.changed(cx);
        }
        removed.len()
    }

    fn start_tool_session(
        &mut self,
        query: &DebugStartQuery,
        cx: &mut Context<Self>,
    ) -> Result<tool_gateway::DebuggerSession, String> {
        if self.debugger.is_some()
            && !matches!(
                self.snapshot.state,
                DebugState::Terminated | DebugState::Inactive
            )
        {
            return Err("a debug session is already running".to_string());
        }
        let Some(name) = query.configuration_name.clone() else {
            return Err("the debug start needs the name of a configuration".to_string());
        };
        let runs = cx
            .try_global::<crate::script::ActiveRunPanel>()
            .map(|panel| panel.0.clone())
            .ok_or_else(|| "the Run window is not connected yet".to_string())?;
        runs.update(cx, |runs, cx| {
            runs.debug_configuration_from_tool_with(
                &name,
                query.program_arguments.as_deref(),
                query.working_directory.as_deref(),
                &query.envs,
                query.client.as_deref(),
                cx,
            )
        })?;
        self.next_tool_session += 1;
        let id = format!("debug-{}", self.next_tool_session);
        self.tool_client = query.client.clone();
        if self.debugger.is_some() {
            self.reset_session();
        }
        self.snapshot = Snapshot {
            state: DebugState::Starting,
            ..Snapshot::default()
        };
        self.tool_session = Some((id.clone(), name.clone()));
        self.tool_output_lines = 0;
        self.tool_output_file = Some(self.tool_output_path(&id));
        if let Some(path) = &self.tool_output_file
            && let Some(parent) = path.parent()
        {
            let _ = std::fs::create_dir_all(parent);
            let _ = std::fs::write(path, b"");
        }
        cx.notify();
        Ok(tool_gateway::DebuggerSession {
            id,
            name: name.clone(),
            status: "starting".to_string(),
            run_configuration: Some(name),
            full_output_path: self
                .tool_output_file
                .as_ref()
                .filter(|path| path.is_file())
                .cloned(),
            exit_code: None,
            ..tool_gateway::DebuggerSession::default()
        })
    }

    /// Sets a breakpoint of the tool as the gutter sets one: the editor and the
    /// Breakpoints window show it at once.
    fn set_breakpoint_from_tool(
        &mut self,
        file: &Path,
        line: u32,
        condition: Option<String>,
        log_message: Option<String>,
        cx: &mut Context<Self>,
    ) -> Result<BreakpointInfo, String> {
        if line == 0 {
            return Err("lines start at 1".to_string());
        }
        let file = file.to_path_buf();
        let existing = self
            .breakpoints
            .iter()
            .position(|breakpoint| breakpoint.file == file && breakpoint.line == line);
        let mut setting = match existing {
            Some(index) => self.breakpoints.remove(index),
            None => BreakpointSetting::new(file.clone(), line),
        };
        setting.log_message = log_message.clone();
        setting.condition = condition.clone();
        setting.suspend = log_message.is_none();
        setting.enabled = true;
        // The point came from a tool, and the editor paints it apart from the ones of the
        // user.
        setting.owner = BreakpointOwner::Agent;
        let spec = setting.spec();
        self.send(Command::SetBreakpoint(spec), cx);
        let info = BreakpointInfo {
            path: file,
            line,
            enabled: setting.enabled,
            verified: self.verified_breakpoint(&setting.file, line),
            condition,
            log_message,
        };
        self.breakpoints.push(setting);
        self.breakpoints
            .sort_by(|left, right| (&left.file, left.line).cmp(&(&right.file, right.line)));
        self.changed(cx);
        Ok(info)
    }

    /// Whether the adapter accepted the breakpoint of that line.
    fn verified_breakpoint(&self, file: &Path, line: u32) -> bool {
        self.shown_breakpoints
            .iter()
            .find(|breakpoint| breakpoint.file == file && breakpoint.line == line)
            .is_some_and(|breakpoint| breakpoint.verified)
    }

    fn tool_breakpoints(&self) -> Vec<BreakpointInfo> {
        self.breakpoints
            .iter()
            .map(|breakpoint| BreakpointInfo {
                path: breakpoint.file.clone(),
                line: breakpoint.line,
                enabled: breakpoint.enabled,
                verified: self.verified_breakpoint(&breakpoint.file, breakpoint.line),
                condition: breakpoint.condition.clone(),
                log_message: breakpoint.log_message.clone(),
            })
            .collect()
    }

    /// Starts a test or a configuration under the debugger through the Run window, the
    /// same way Shift+F9 does, and answers the id of the session.
    fn start_tool_debug(
        &mut self,
        request: &tool_gateway::DebugRunRequest,
        client: Option<&str>,
        cx: &mut Context<Self>,
    ) -> Result<String, String> {
        if self.debugger.is_some()
            && !matches!(
                self.snapshot.state,
                DebugState::Terminated | DebugState::Inactive
            )
        {
            return Err("a debug session is already running".to_string());
        }
        let runs = cx
            .try_global::<crate::script::ActiveRunPanel>()
            .map(|panel| panel.0.clone())
            .ok_or_else(|| "the Run window is not connected yet".to_string())?;
        let started = match (&request.configuration, &request.suite) {
            (Some(name), _) => runs.update(cx, |runs, cx| {
                runs.debug_configuration_from_tool(name, client, cx)
            }),
            (None, Some(suite)) => {
                let test = runner::TestRequest {
                    module: request.module.clone(),
                    package: None,
                    suite: Some(suite.clone()),
                    case: request.case.clone(),
                    cases: Vec::new(),
                };
                runs.update(cx, |runs, cx| runs.debug_tests_from_tool(test, client, cx))
            }
            _ => Err("pass a suite or a configuration".to_string()),
        };
        started?;
        self.next_tool_session += 1;
        let session = format!("debug-{}", self.next_tool_session);
        let name = request
            .configuration
            .clone()
            .or_else(|| request.suite.clone())
            .unwrap_or_else(|| "Debug session".to_string());
        self.tool_client = client.map(str::to_string);
        // A session that ended is gone before the new one starts, so a stop of the old
        // session cannot answer the wait of the new one.
        if self.debugger.is_some() {
            self.reset_session();
        }
        self.tool_session = Some((session.clone(), name));
        // The start of the Run window is asynchronous. The session counts as starting
        // at once, so a tool that waits for the first stop does not read the end of the
        // previous session.
        self.snapshot = Snapshot {
            state: DebugState::Starting,
            ..Snapshot::default()
        };
        cx.notify();
        Ok(session)
    }

    /// Evaluates an expression of a tool through the adapter and answers its value.
    fn evaluate_for_tool(
        &mut self,
        job: ToolJob,
        query: tool_gateway::DebugEvaluateQuery,
        cx: &mut Context<Self>,
    ) {
        let Some(debugger) = self.debugger.clone() else {
            job.fail("debugging is not running");
            return;
        };
        if !matches!(self.snapshot.state, DebugState::Stopped { .. }) {
            job.fail("the program is not stopped");
            return;
        }
        let index = query.frame.unwrap_or(0);
        let Some(frame) = self.snapshot.frames.get(index).map(|frame| frame.id) else {
            job.fail("the stack has no such frame");
            return;
        };
        let expression = query.expression;
        let arguments = serde_json::json!({
            "expression": expression,
            "frameId": frame,
            "context": "repl",
        });
        let asked = expression.clone();
        cx.spawn(async move |this, cx| {
            let answer = debugger.request("evaluate", arguments).await;
            this.update(cx, |panel, cx| panel.tool_evaluated(job, asked, answer, cx))
                .ok();
        })
        .detach();
    }

    fn tool_evaluated(
        &mut self,
        job: ToolJob,
        expression: String,
        answer: Result<serde_json::Value, String>,
        cx: &mut Context<Self>,
    ) {
        match answer {
            Ok(body) => {
                let value = body
                    .get("result")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let type_name = body
                    .get("type")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                job.answer(ToolAnswer::Value(DebugValue {
                    expression,
                    value,
                    type_name,
                }));
            }
            Err(error) => job.fail(format!(
                "the program could not evaluate the expression: {error}"
            )),
        }
        cx.notify();
    }

    /// Answers the tools that wait for the session with the newest state.
    fn answer_tool_jobs(&mut self, cx: &mut Context<Self>) {
        let now = Instant::now();
        self.answer_tool_waits(now);
        self.answer_tool_steps(now);
        self.answer_tool_stacks(now);
        self.answer_tool_variables(now, cx);
        self.answer_tool_controls(now);
    }

    fn answer_tool_waits(&mut self, now: Instant) {
        let waits = std::mem::take(&mut self.tool_waits);
        let mut keep = Vec::new();
        for wait in waits {
            let stopped = matches!(self.snapshot.state, DebugState::Stopped { .. });
            // A start of the Run window is not the end of the session: the debugger of
            // the new one comes a moment later.
            let over = matches!(
                self.snapshot.state,
                DebugState::Terminated | DebugState::Inactive
            );
            if stopped || over || now >= wait.deadline {
                wait.job.answer(ToolAnswer::State(self.tool_state()));
            } else {
                keep.push(wait);
            }
        }
        self.tool_waits = keep;
    }

    fn answer_tool_steps(&mut self, now: Instant) {
        let steps = std::mem::take(&mut self.tool_steps);
        let mut keep = Vec::new();
        for step in steps {
            let stopped = matches!(self.snapshot.state, DebugState::Stopped { .. })
                && self.snapshot.stops > step.stops;
            let over = self.debugger.is_none()
                || matches!(
                    self.snapshot.state,
                    DebugState::Terminated | DebugState::Inactive
                );
            if stopped || over || now >= step.deadline {
                step.job.answer(ToolAnswer::State(self.tool_state()));
            } else {
                keep.push(step);
            }
        }
        self.tool_steps = keep;
    }

    fn answer_tool_stacks(&mut self, now: Instant) {
        let stacks = std::mem::take(&mut self.tool_stacks);
        let mut keep = Vec::new();
        for stack in stacks {
            if self.debugger.is_none()
                || matches!(
                    self.snapshot.state,
                    DebugState::Terminated | DebugState::Inactive
                )
            {
                stack.job.fail("debugging is not running");
                continue;
            }
            if !matches!(self.snapshot.state, DebugState::Stopped { .. }) {
                stack
                    .job
                    .fail("the program is running, wait for a stop with debug_wait".to_string());
                continue;
            }
            if let Some(thread) = stack.thread
                && self.snapshot.current_thread != Some(thread)
            {
                if now >= stack.deadline {
                    stack
                        .job
                        .fail(format!("the adapter did not show thread {thread}"));
                } else {
                    keep.push(stack);
                }
                continue;
            }
            let thread = self
                .snapshot
                .current_thread
                .and_then(|id| self.snapshot.threads.iter().find(|thread| thread.id == id))
                .map(|thread| DebugThread {
                    id: thread.id,
                    name: thread.name.clone(),
                });
            let total = self.snapshot.frames.len();
            let frames = self
                .snapshot
                .frames
                .iter()
                .take(stack.frames)
                .enumerate()
                .map(|(index, frame)| self.tool_frame(index, frame))
                .collect();
            stack.job.answer(ToolAnswer::Stack(DebugStack {
                thread,
                total,
                frames,
            }));
        }
        self.tool_stacks = keep;
    }

    fn answer_tool_variables(&mut self, now: Instant, cx: &mut Context<Self>) {
        let variables = std::mem::take(&mut self.tool_variables);
        let mut keep = Vec::new();
        for mut pending in variables {
            if self.debugger.is_none()
                || matches!(
                    self.snapshot.state,
                    DebugState::Terminated | DebugState::Inactive
                )
            {
                pending.job.fail("debugging is not running");
                continue;
            }
            if !matches!(self.snapshot.state, DebugState::Stopped { .. }) {
                pending
                    .job
                    .fail("the program is running, wait for a stop with debug_wait".to_string());
                continue;
            }
            let Some(frame) = self
                .snapshot
                .frames
                .get(pending.frame)
                .map(|frame| frame.id)
            else {
                pending.job.fail("the stack has no such frame");
                continue;
            };
            if self.snapshot.selected_frame != Some(frame) {
                if pending.selected != Some(frame) {
                    pending.selected = Some(frame);
                    self.send(Command::SelectFrame(frame), cx);
                }
                if now >= pending.deadline {
                    pending
                        .job
                        .fail("the adapter did not show the frame of the stack");
                } else {
                    keep.push(pending);
                }
                continue;
            }
            let mut missing = Vec::new();
            self.missing_children(pending.depth, &mut missing);
            let mut waiting = false;
            for reference in missing {
                waiting = true;
                if pending.expanded.insert(reference) {
                    self.send(Command::Expand(reference), cx);
                }
            }
            if waiting && now < pending.deadline {
                keep.push(pending);
                continue;
            }
            let answer = self.tool_variables_of(&pending);
            pending.job.answer(ToolAnswer::Variables(answer));
        }
        self.tool_variables = keep;
    }

    /// The references whose children the answer needs but the snapshot does not have.
    fn missing_children(&self, depth: usize, out: &mut Vec<i64>) {
        fn walk(
            snapshot: &Snapshot,
            variables: &[Variable],
            level: usize,
            depth: usize,
            out: &mut Vec<i64>,
        ) {
            for variable in variables {
                if variable.reference == 0 || level >= depth {
                    continue;
                }
                match snapshot.variables.get(&variable.reference) {
                    Some(children) => walk(snapshot, children, level + 1, depth, out),
                    None => out.push(variable.reference),
                }
            }
        }
        for scope in &self.snapshot.scopes {
            match self.snapshot.variables.get(&scope.reference) {
                Some(children) => walk(&self.snapshot, children, 0, depth, out),
                None => out.push(scope.reference),
            }
        }
    }

    fn tool_variables_of(&self, pending: &PendingVariables) -> DebugVariables {
        let mut total = 0;
        let mut clipped = false;
        let mut scopes = Vec::new();
        for scope in &self.snapshot.scopes {
            let children = self
                .snapshot
                .variables
                .get(&scope.reference)
                .map(Vec::as_slice)
                .unwrap_or_default();
            let mut shown = 0;
            let variables = children
                .iter()
                .filter_map(|variable| {
                    self.tool_variable(
                        variable,
                        0,
                        pending.depth,
                        pending.variables,
                        &mut shown,
                        &mut clipped,
                    )
                })
                .collect();
            total += shown;
            scopes.push(DebugScope {
                name: scope.name.clone(),
                variables,
            });
        }
        DebugVariables {
            scopes,
            total,
            clipped,
        }
    }

    fn tool_variable(
        &self,
        variable: &Variable,
        level: usize,
        depth: usize,
        limit: usize,
        shown: &mut usize,
        clipped: &mut bool,
    ) -> Option<DebugVariable> {
        if *shown >= limit {
            *clipped = true;
            return None;
        }
        *shown += 1;
        let children = if level < depth {
            self.snapshot
                .variables
                .get(&variable.reference)
                .map(|children| {
                    children
                        .iter()
                        .filter_map(|child| {
                            self.tool_variable(child, level + 1, depth, limit, shown, clipped)
                        })
                        .collect()
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        Some(DebugVariable {
            name: variable.name.clone(),
            value: variable.value.clone(),
            type_name: variable.type_name.clone(),
            children,
        })
    }

    fn tool_state(&self) -> DebugSessionState {
        let state = match &self.snapshot.state {
            DebugState::Inactive | DebugState::Terminated => "ended",
            DebugState::Starting => "starting",
            DebugState::Running => "running",
            DebugState::Stopped { .. } => "stopped",
        };
        let reason = match &self.snapshot.state {
            DebugState::Stopped { reason } => Some(reason.clone()),
            _ => None,
        };
        let thread = self
            .snapshot
            .current_thread
            .and_then(|id| self.snapshot.threads.iter().find(|thread| thread.id == id))
            .map(|thread| DebugThread {
                id: thread.id,
                name: thread.name.clone(),
            });
        let frame = self
            .snapshot
            .frames
            .first()
            .map(|frame| self.tool_frame(0, frame));
        DebugSessionState {
            state: state.to_string(),
            reason,
            thread,
            frame,
        }
    }

    fn tool_frame(&self, index: usize, frame: &Frame) -> DebugFrame {
        DebugFrame {
            index,
            name: frame.name.clone(),
            path: frame.source.clone(),
            line: frame.line,
            column: frame.column,
        }
    }

    /// The values of the frame at the stop, with the ones that changed since the
    /// previous stop, for the editor to show after the lines that name them.
    pub fn stopped_values(&self) -> Vec<editor_view::DebugValue> {
        let changed = &self.changed_values;
        self.frame_values()
            .into_iter()
            .map(
                |(name, (reference, type_name, value))| editor_view::DebugValue {
                    changed: changed.contains(&name),
                    name,
                    value: self.display_value(reference, &type_name, &value),
                },
            )
            .collect()
    }

    /// The file and the buffer line of the frame the window shows while stopped.
    pub fn stop_location(&self) -> Option<(PathBuf, usize)> {
        if !matches!(self.snapshot.state, DebugState::Stopped { .. }) {
            return None;
        }
        let frame = self
            .snapshot
            .frames
            .iter()
            .find(|frame| Some(frame.id) == self.snapshot.selected_frame && frame.source.is_some())
            .or_else(|| {
                self.snapshot
                    .frames
                    .iter()
                    .find(|frame| frame.source.is_some())
            })?;
        Some((frame.source.clone()?, frame.line.saturating_sub(1) as usize))
    }

    /// Puts the caret of the stopped frame on its line, and opens the file of the frame when
    /// it is not open yet, as a frame click does. It runs after the update that asks for it,
    /// in the window that holds the panel. It takes no keyboard focus: the focus the window
    /// had stays with it, and a caret that is on the line already stays where it is. A file
    /// that is neither open nor on disk has no place to show, and a line past the end of a
    /// loaded text is no line of its file.
    fn show_stop_place(&mut self, cx: &mut Context<Self>) {
        let (Some((path, line)), Some(handle)) = (self.stop_location(), self.window) else {
            return;
        };
        let workspace = self.workspace.clone();
        let spot = editor_view::Spot::Line { line, column: 0 };
        cx.defer(move |cx| {
            handle
                .update(cx, |_, window, cx| {
                    workspace
                        .update(cx, |workspace, cx| {
                            let view = workspace
                                .view_of(&path)
                                .and_then(|view| view.downcast::<editor_view::EditorView>().ok());
                            let open = view.is_some();
                            let in_front =
                                workspace.active_path().as_deref() == Some(path.as_path());
                            match view {
                                // The file in front keeps its tab and its focus: only the caret moves.
                                Some(editor) if in_front => {
                                    let (caret, text) = {
                                        let editor = editor.read(cx);
                                        (editor.caret_line(), editor.text())
                                    };
                                    let past_end =
                                        text.is_some_and(|text| line > text.matches('\n').count());
                                    if caret != Some(line) && !past_end {
                                        editor.update(cx, |editor, cx| editor.go_to(spot, cx));
                                    }
                                }
                                _ if open || path.is_file() => {
                                    // Selecting a tab takes the focus. The focus the window had
                                    // comes back, unless it was the editor in front: its focus
                                    // follows the file that is shown now.
                                    let front_editor = workspace
                                        .active_path()
                                        .and_then(|active| workspace.view_of(&active))
                                        .and_then(|view| {
                                            view.downcast::<editor_view::EditorView>().ok()
                                        })
                                        .map(|editor| editor.read(cx).focus_handle(cx));
                                    let keep = window
                                        .focused(cx)
                                        .filter(|focused| front_editor.as_ref() != Some(focused));
                                    navigation::go_to(
                                        workspace,
                                        path,
                                        Some(spot),
                                        true,
                                        window,
                                        cx,
                                    );
                                    if let Some(keep) = keep {
                                        window.focus(&keep, cx);
                                    }
                                }
                                _ => {}
                            }
                        })
                        .ok();
                })
                .ok();
        });
    }

    /// Gives the panel the window it sits in, so that a stop can show its file there.
    pub(crate) fn bind_window(&mut self, window: AnyWindowHandle) {
        self.window = Some(window);
    }

    /// The values of the scopes of the selected frame, by name. A name of the locals
    /// wins over a field of the same name, as the debugger names the one in force.
    fn frame_values(&self) -> BTreeMap<String, (i64, String, String)> {
        let mut values = BTreeMap::new();
        for scope in &self.snapshot.scopes {
            let Some(variables) = self.snapshot.variables.get(&scope.reference) else {
                continue;
            };
            for variable in variables {
                if variable.name.is_empty() || variable.name == "this" {
                    continue;
                }
                values.entry(variable.name.clone()).or_insert_with(|| {
                    (
                        variable.reference,
                        variable.type_name.clone(),
                        variable.value.clone(),
                    )
                });
            }
        }
        values
    }

    /// The value as the tree shows it: with the label of the object, if the user marked
    /// the object this handle names at this stop.
    fn display_value(&self, reference: i64, type_name: &str, value: &str) -> String {
        match self.label(reference, type_name) {
            Some(label) => format!("{value} ({label}_DebugLabel)"),
            None => value.to_owned(),
        }
    }

    /// The label of the object a handle names at this stop, when that object was marked.
    /// The identity the program gave for the handle is the key; the type guards against
    /// two objects of different types with the same number.
    fn label(&self, reference: i64, type_name: &str) -> Option<&str> {
        let identity = self.identities.get(&reference).copied().flatten()?;
        self.labels
            .iter()
            .find(|label| label.identity == identity && same_type(&label.type_name, type_name))
            .map(|label| label.name.as_str())
    }

    /// Whether a scope or a variable of the tree is open, for tests.
    pub fn is_expanded(&self, reference: i64) -> bool {
        self.expanded.contains(&reference)
    }

    /// The watch expressions the window keeps, for tests.
    pub fn watch_expressions(&self) -> Vec<String> {
        self.watch.clone()
    }

    /// The inline watch expressions of this project, for tests.
    pub fn inline_watches(&self) -> Vec<String> {
        self.inline_watches
            .iter()
            .map(|watch| watch.expression.clone())
            .collect()
    }

    /// The value of a variable as the tree shows it, for tests.
    #[cfg(test)]
    pub(crate) fn variable_value_for_test(&self, reference: i64, name: &str) -> Option<String> {
        let variable = self
            .snapshot
            .variables
            .get(&reference)?
            .iter()
            .find(|variable| variable.name == name)?;
        Some(self.display_value(variable.reference, &variable.type_name, &variable.value))
    }

    /// The label of an object, for tests.
    #[cfg(test)]
    pub(crate) fn label_for_test(&self, reference: i64) -> Option<String> {
        let identity = self.identities.get(&reference).copied().flatten()?;
        self.labels
            .iter()
            .find(|label| label.identity == identity)
            .map(|label| label.name.clone())
    }

    /// The identity the program gave for a handle at this stop, for tests.
    #[cfg(test)]
    pub(crate) fn identity_for_test(&self, reference: i64) -> Option<i64> {
        self.identities.get(&reference).copied().flatten()
    }

    /// The objects the window shows whose type has a mark: the variables of every scope
    /// and of the open objects, the watch list, the inline watches and the popup. A value
    /// the adapter gave no expression for cannot be asked for its identity.
    fn shown_objects(&self) -> Vec<ShownObject> {
        if self.labels.is_empty() {
            return Vec::new();
        }
        let marked = |type_name: &str| {
            self.labels
                .iter()
                .any(|label| same_type(&label.type_name, type_name))
        };
        let mut shown = Vec::new();
        let mut push = |variable: &Variable| {
            let Some(name) = variable
                .evaluate_name
                .as_deref()
                .map(str::trim)
                .filter(|name| !name.is_empty())
            else {
                return;
            };
            if variable.reference != 0 && marked(&variable.type_name) {
                shown.push(ShownObject {
                    reference: variable.reference,
                    name: name.to_owned(),
                });
            }
        };
        // The variables of a scope stand in the inline values of the editor even when
        // the scope is folded, so every scope is read rather than only the open ones.
        for scope in &self.snapshot.scopes {
            if let Some(variables) = self.snapshot.variables.get(&scope.reference) {
                let mut open = vec![variables];
                while let Some(variables) = open.pop() {
                    for variable in variables {
                        push(variable);
                        if variable.reference != 0
                            && self.expanded.contains(&variable.reference)
                            && let Some(children) = self.snapshot.variables.get(&variable.reference)
                        {
                            open.push(children);
                        }
                    }
                }
            }
        }
        for watch in self
            .snapshot
            .watch
            .iter()
            .chain(&self.snapshot.inline_watch)
        {
            if watch.reference != 0 && marked(&watch.type_name) {
                shown.push(ShownObject {
                    reference: watch.reference,
                    name: watch.expression.clone(),
                });
            }
        }
        if let Some(evaluation) = &self.snapshot.evaluation
            && evaluation.reference != 0
            && marked(&evaluation.type_name)
        {
            shown.push(ShownObject {
                reference: evaluation.reference,
                name: evaluation.expression.clone(),
            });
        }
        shown
    }

    /// Asks the program for the identity of the objects it shows, so a mark appears on
    /// the object it was put on rather than on a handle the adapter reused. The answers
    /// of a stop are kept in `identities`; a new stop forgets them.
    fn sync_identities(&mut self, cx: &mut Context<Self>) {
        if self.identity_stop != self.snapshot.stops {
            self.identity_stop = self.snapshot.stops;
            self.identities.clear();
            self.identity_questions.clear();
        }
        let answers = self.snapshot.identities.clone();
        let mut changed = false;
        let mut waiting = Vec::new();
        for (request, name, type_name) in std::mem::take(&mut self.pending_marks) {
            let Some(answer) = answers.get(&request) else {
                waiting.push((request, name, type_name));
                continue;
            };
            changed = true;
            match answer.identity {
                Some(identity) => self.labels.push(ObjectLabel {
                    name,
                    type_name,
                    identity,
                }),
                None => {
                    let reason = answer
                        .error
                        .clone()
                        .unwrap_or_else(|| "the program gave no number for the object".to_owned());
                    self.local_console
                        .push(ConsoleLine::new(format!("Mark Object: {reason}"), true));
                    self.trim_console();
                }
            }
        }
        self.pending_marks = waiting;
        let answered: Vec<u64> = self
            .identity_questions
            .iter()
            .filter(|(request, _)| answers.contains_key(request))
            .map(|(request, _)| *request)
            .collect();
        for request in answered {
            if let Some(question) = self.identity_questions.remove(&request)
                && let Some(answer) = answers.get(&request)
            {
                self.identities.insert(question.reference, answer.identity);
                changed = true;
            }
        }
        if matches!(self.snapshot.state, DebugState::Stopped { .. }) {
            let mut asked = false;
            for object in self.shown_objects() {
                if self.identities.contains_key(&object.reference)
                    || self
                        .identity_questions
                        .values()
                        .any(|question| question.reference == object.reference)
                {
                    continue;
                }
                let expression = self
                    .service
                    .as_ref()
                    .and_then(|service| service.object_identity_expression(&object.name));
                let Some(expression) = expression else {
                    continue;
                };
                let request = self.next_identity;
                self.next_identity += 1;
                self.identity_questions.insert(
                    request,
                    IdentityQuestion {
                        reference: object.reference,
                    },
                );
                self.send(
                    Command::EvaluateIdentity {
                        request,
                        expression,
                    },
                    cx,
                );
                asked = true;
            }
            if asked {
                // The identity of a value may arrive after the values of the stop: the
                // tree draws the mark when it does.
                cx.notify();
            }
        }
        if changed {
            cx.emit(ValuesChanged);
            cx.notify();
        }
    }

    /// The renderers of the window: the rules of the language first, then the rules of
    /// the settings, which win for the same type.
    fn type_renderers(&self, cx: &App) -> Vec<TypeRenderer> {
        let text = cx
            .try_global::<workspace::Settings>()
            .map(|settings| settings.debug_type_renderers.clone())
            .unwrap_or_default();
        let mut renderers = self.language_renderers.clone();
        for mut renderer in parse_renderers(&text) {
            renderer.subtype_probe = self
                .service
                .as_ref()
                .and_then(|service| service.subtype_probe(&renderer.type_name));
            if let Some(index) = renderers
                .iter()
                .position(|existing| existing.matches(&renderer.type_name))
            {
                renderers[index] = renderer;
            } else {
                renderers.push(renderer);
            }
        }
        renderers
    }

    /// Sends the language and settings renderers to the session when they changed.
    fn sync_renderers(&mut self, cx: &mut Context<Self>) {
        let renderers = self.type_renderers(cx);
        if renderers == self.sent_renderers {
            return;
        }
        self.sent_renderers = renderers.clone();
        self.send(Command::SetTypeRenderers(renderers), cx);
        if matches!(self.snapshot.state, DebugState::Stopped { .. }) {
            let mut references: Vec<i64> = self
                .snapshot
                .scopes
                .iter()
                .map(|scope| scope.reference)
                .collect();
            references.extend(self.expanded.iter().copied().filter(|key| *key > 0));
            references.sort_unstable();
            references.dedup();
            for reference in references {
                self.send(Command::Expand(reference), cx);
            }
        }
    }

    /// Sends the inline watch expressions of the project to the session when a session
    /// starts or the list changes.
    fn sync_inline_watches(&mut self, cx: &mut Context<Self>) {
        let expressions: Vec<String> = self
            .inline_watches
            .iter()
            .map(|watch| watch.expression.clone())
            .collect();
        if expressions == self.sent_inline_watches {
            return;
        }
        self.sent_inline_watches = expressions.clone();
        self.send(Command::SetInlineWatches(expressions), cx);
    }

    /// The lines of the debug console, for tests.
    #[cfg(test)]
    pub(crate) fn console_lines(&self) -> Vec<String> {
        self.local_console
            .iter()
            .map(|line| ansi::plain(&line.text))
            .collect()
    }

    /// The parts of a console line as the console paints them, with the terminal colours,
    /// for tests.
    #[cfg(test)]
    pub(crate) fn console_line_parts(&self, index: usize) -> Vec<(String, Option<u8>)> {
        let Some(line) = self.local_console.get(index) else {
            return Vec::new();
        };
        let parsed = ansi::parse_line(&line.text, false);
        parsed
            .spans
            .iter()
            .map(|(range, style)| (parsed.text[range.clone()].to_owned(), style.color))
            .collect()
    }

    /// The counter of the search bar of the console, for tests.
    #[cfg(test)]
    pub(crate) fn console_search_status(&self) -> String {
        self.console_search.status().0
    }

    /// The lines the filter of the console keeps, for tests.
    #[cfg(test)]
    pub(crate) fn visible_console_lines(&self) -> Vec<String> {
        let total = self.local_console.len();
        (0..self.console_search.kept_count(total))
            .filter_map(|row| {
                let index = self.console_search.line_of_row(total, row);
                self.local_console
                    .get(index)
                    .map(|line| ansi::plain(&line.text))
            })
            .collect()
    }

    /// The scroll offset of the console, in pixels, for tests.
    #[cfg(test)]
    pub(crate) fn console_scroll_y(&self) -> f32 {
        let handle = self.console_scroll.handle();
        let state = handle.0.borrow();
        f32::from(state.base_handle.offset().y)
    }

    /// Shows the Debugger tab, for a test whose program takes input and brings the
    /// console to the front.
    #[cfg(test)]
    pub(crate) fn show_debugger_front(&mut self, cx: &mut Context<Self>) {
        self.front = Front::Debugger;
        cx.notify();
    }

    /// Selects a variable row the way a click does, for tests that cannot reach a row
    /// scrolled out of the window.
    #[cfg(test)]
    pub fn select_variable_for_test(&mut self, reference: i64, name: &str) {
        let variable = self
            .snapshot
            .variables
            .get(&reference)
            .and_then(|variables| variables.iter().find(|variable| variable.name == name));
        let value = variable
            .map(|variable| variable.value.clone())
            .unwrap_or_default();
        let object = variable.map(|variable| variable.reference).unwrap_or(0);
        let type_name = variable
            .map(|variable| variable.type_name.clone())
            .unwrap_or_default();
        let evaluate_name = variable.and_then(|variable| variable.evaluate_name.clone());
        self.selected_variable = Some(VariableTarget {
            reference,
            name: name.to_owned(),
            value,
            object,
            type_name,
            evaluate_name,
        });
    }

    fn virtual_page(&self) -> usize {
        self.expanded
            .iter()
            .filter(|key| **key <= VIRTUAL_PAGE_BASE)
            .map(|key| (VIRTUAL_PAGE_BASE - *key) as usize)
            .max()
            .unwrap_or(0)
    }

    fn set_virtual_page(&mut self, page: usize, cx: &mut Context<Self>) {
        self.expanded.retain(|key| *key >= VIRTUAL_PAGE_BASE);
        self.expanded.insert(VIRTUAL_PAGE_BASE - page as i64);
        cx.notify();
    }

    /// Opens a scope the way a click on it does.
    pub fn expand(&mut self, reference: i64, cx: &mut Context<Self>) {
        self.expanded.insert(reference);
        self.send(Command::Expand(reference), cx);
        cx.notify();
    }

    /// Selects a frame the way a click on its row does.
    pub fn select_frame(&mut self, frame: i64, cx: &mut Context<Self>) {
        self.send(Command::SelectFrame(frame), cx);
        cx.notify();
    }

    /// Opens or closes a variable with children the way a click on it does.
    pub fn toggle_expanded(&mut self, reference: i64, cx: &mut Context<Self>) {
        if self.expanded.contains(&reference) {
            self.expanded.remove(&reference);
        } else {
            self.expanded.insert(reference);
            self.send(Command::Expand(reference), cx);
        }
        cx.notify();
    }

    /// Starts a session for a prepared plan. A build step runs first, in this console,
    /// and the program starts only when it succeeds.
    pub fn start_plan(
        &mut self,
        service: Arc<dyn DebugService>,
        plan: RunPlan,
        name: String,
        entry: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        self.restart = Some(Restart {
            service: service.clone(),
            before: plan.before.clone(),
            program: plan.program.clone(),
            grace: plan.grace,
            name: name.clone(),
            entry: entry.clone(),
        });
        self.reset_session();
        self.note_tool_client();
        self.session = service.begin_session();
        if let Some(hub) = cx.try_global::<ActiveDebugHub>() {
            hub.0.clear();
        }
        let pending = PendingLaunch {
            service,
            program: plan.program,
            name,
            entry,
        };
        match plan.before {
            Some(before) => {
                self.push_output("Building the project", false, cx);
                self.pending = Some(pending);
                self.build = Some(Console::start_step(before, Step::Before, plan.grace));
                // The build is what the window shows first: its output is in the Console.
                self.front = Front::Console;
            }
            None => {
                self.pending = Some(pending);
                self.launch_pending(cx);
            }
        }
        cx.notify();
    }

    /// Attaches to a JVM that is already running.
    pub fn start_attach(
        &mut self,
        service: Arc<dyn DebugService>,
        host: String,
        port: u16,
        name: String,
        project: Option<String>,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        self.language_renderers = service.type_renderers();
        self.service = Some(service.clone());
        let debugger = service.attach_with_breakpoints(
            host,
            port,
            name,
            project,
            self.specs(),
            self.function_specs(),
            self.data_specs(),
            self.watch.clone(),
            self.filter_list(),
        )?;
        self.begin_attach(debugger, cx);
        Ok(())
    }

    /// Attaches to a JVM whose debug port was already announced by its output, so the
    /// port is not probed with a connection that the JVM would see as a broken handshake.
    pub fn start_attach_ready(
        &mut self,
        service: Arc<dyn DebugService>,
        host: String,
        port: u16,
        name: String,
        project: Option<String>,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let session = service.begin_session();
        self.language_renderers = service.type_renderers();
        self.service = Some(service.clone());
        let debugger = service.attach_ready_with_breakpoints(
            host,
            port,
            name,
            project,
            self.specs(),
            self.function_specs(),
            self.data_specs(),
            self.watch.clone(),
            self.filter_list(),
        )?;
        self.begin_attach(debugger, cx);
        self.session = session;
        Ok(())
    }

    fn begin_attach(&mut self, debugger: Debugger, cx: &mut Context<Self>) {
        self.reset_session();
        self.note_tool_client();
        if let Some(hub) = cx.try_global::<ActiveDebugHub>() {
            hub.0.clear();
        }
        self.debugger = Some(debugger);
        self.snapshot.state = DebugState::Starting;
        cx.notify();
    }

    fn reset_session(&mut self) {
        self.debugger = None;
        self.snapshot = Snapshot::default();
        self.shown_breakpoints.clear();
        self.shown_function_breakpoints.clear();
        self.shown_data_breakpoints.clear();
        self.expanded.clear();
        self.selected_variable = None;
        self.variable_edit = None;
        self.watch_input = None;
        self.selected_watch = None;
        self.labels.clear();
        self.identities.clear();
        self.identity_questions.clear();
        self.pending_marks.clear();
        self.identity_stop = 0;
        self.next_identity = 1;
        self.expanded_library_groups.clear();
        self.sent_renderers.clear();
        self.sent_inline_watches.clear();
        self.temporary = None;
        self.forced_step = None;
        self.seen_stops = 0;
        self.asking_targets = false;
        self.asking_targets_window = None;
        self.reveal_stop = None;
        self.opened_frame = None;
        self.local_console.clear();
        self.seen_console.clear();
        self.console_search.clear();
        self.console_input.restart();
        self.build = None;
        self.build_output = OutputBuffers::default();
        self.build_process = None;
        self.build_failed = false;
        self.build_stopped = false;
        self.stop_shown = false;
        self.pending = None;
        self.session = None;
        self.tool_output_file = None;
        self.tool_output_lines = 0;
        self.front = Front::Debugger;
        self.stop_values.clear();
        self.changed_values.clear();
        self.values_stop = 0;
    }

    fn launch_pending(&mut self, cx: &mut Context<Self>) {
        let Some(pending) = self.pending.take() else {
            return;
        };
        self.language_renderers = pending.service.type_renderers();
        self.service = Some(pending.service.clone());
        match pending.service.start_with_breakpoints(
            pending.program,
            pending.name,
            pending.entry,
            self.specs(),
            self.function_specs(),
            self.data_specs(),
            self.watch.clone(),
            self.filter_list(),
        ) {
            Ok(debugger) => {
                self.debugger = Some(debugger);
                self.snapshot.state = DebugState::Starting;
            }
            Err(error) => {
                self.local_console
                    .push(ConsoleLine::new("Cannot debug", true));
                self.local_console.push(ConsoleLine::new(error, true));
                self.snapshot.state = DebugState::Terminated;
            }
        }
        cx.notify();
    }

    /// The lines of the breakpoints of this file, from one.
    pub fn breakpoint_lines(&self, file: &Path) -> Vec<u32> {
        let mut lines: Vec<u32> = self
            .breakpoints
            .iter()
            .filter(|breakpoint| breakpoint.file == file)
            .map(|breakpoint| breakpoint.line)
            .collect();
        lines.sort_unstable();
        lines.dedup();
        lines
    }

    /// The lines of the temporary breakpoints of this file, from one.
    pub fn temporary_breakpoint_lines(&self, file: &Path) -> Vec<u32> {
        let mut lines: Vec<u32> = self
            .breakpoints
            .iter()
            .filter(|breakpoint| breakpoint.file == file && breakpoint.temporary)
            .map(|breakpoint| breakpoint.line)
            .collect();
        lines.sort_unstable();
        lines.dedup();
        lines
    }

    /// The lines of the breakpoints an agent set in this file, from one.
    pub fn agent_breakpoint_lines(&self, file: &Path) -> Vec<u32> {
        let mut lines: Vec<u32> = self
            .breakpoints
            .iter()
            .filter(|breakpoint| {
                breakpoint.file == file && breakpoint.owner == BreakpointOwner::Agent
            })
            .map(|breakpoint| breakpoint.line)
            .collect();
        lines.sort_unstable();
        lines.dedup();
        lines
    }

    /// The owner of the breakpoint on the line, for the dialog and tests.
    pub fn breakpoint_owner(&self, file: &Path, line: u32) -> Option<BreakpointOwner> {
        self.setting(file, line).map(|setting| setting.owner)
    }

    /// Whether the breakpoint on the line is on, for the properties popup and tests.
    pub fn breakpoint_enabled(&self, file: &Path, line: u32) -> Option<bool> {
        self.setting(file, line).map(|setting| setting.enabled)
    }

    /// Whether the point of any exception is on, for tests.
    pub fn any_exception_enabled(&self) -> bool {
        self.any_exception.enabled
    }

    /// Whether the breakpoint on the line suspends the program, for tests.
    pub fn breakpoint_suspends(&self, file: &Path, line: u32) -> Option<bool> {
        self.setting(file, line).map(|setting| setting.suspend)
    }

    /// Whether the breakpoint on the line goes away at the first stop.
    pub fn breakpoint_temporary(&self, file: &Path, line: u32) -> Option<bool> {
        self.setting(file, line).map(|setting| setting.temporary)
    }

    /// Whether the console says where the breakpoint was reached, for tests.
    pub fn breakpoint_hit_message(&self, file: &Path, line: u32) -> Option<bool> {
        self.setting(file, line).map(|setting| setting.hit_message)
    }

    /// The expression a breakpoint logs on every hit, for scripts and tests.
    pub fn breakpoint_log_expression(&self, file: &Path, line: u32) -> Option<&str> {
        self.setting(file, line)
            .and_then(|setting| setting.log_expression.as_deref())
    }

    /// The lines of the breakpoints that do not stop the program, for the gutter.
    pub fn non_suspending_breakpoint_lines(&self, file: &Path) -> Vec<u32> {
        let mut lines: Vec<u32> = self
            .breakpoints
            .iter()
            .filter(|breakpoint| breakpoint.file == file && !breakpoint.suspend)
            .map(|breakpoint| breakpoint.line)
            .collect();
        lines.sort_unstable();
        lines.dedup();
        lines
    }

    /// The condition of a breakpoint, for scripts and tests.
    pub fn condition(&self, file: &Path, line: u32) -> Option<&str> {
        self.breakpoints
            .iter()
            .find(|breakpoint| breakpoint.file == file && breakpoint.line == line)
            .and_then(|breakpoint| breakpoint.condition.as_deref())
    }

    pub fn toggle_breakpoint(&mut self, file: PathBuf, line: u32, cx: &mut Context<Self>) {
        if line == 0 {
            return;
        }
        let found = self
            .breakpoints
            .iter()
            .position(|breakpoint| breakpoint.file == file && breakpoint.line == line);
        match found {
            Some(index) => {
                self.breakpoints.remove(index);
                self.send(Command::RemoveBreakpoint { file, line }, cx);
            }
            None => {
                let setting = BreakpointSetting::new(file, line);
                self.send(Command::SetBreakpoint(setting.spec()), cx);
                self.breakpoints.push(setting);
            }
        }
        self.breakpoints
            .sort_by(|left, right| (&left.file, left.line).cmp(&(&right.file, right.line)));
        self.changed(cx);
    }

    /// Ctrl+Alt+Shift+F8: a breakpoint that goes away at the first stop.
    pub fn toggle_temporary_breakpoint(
        &mut self,
        file: PathBuf,
        line: u32,
        cx: &mut Context<Self>,
    ) {
        if line == 0 {
            return;
        }
        let found = self
            .breakpoints
            .iter()
            .position(|breakpoint| breakpoint.file == file && breakpoint.line == line);
        match found {
            Some(index) => {
                self.breakpoints.remove(index);
                self.send(Command::RemoveBreakpoint { file, line }, cx);
            }
            None => {
                let mut setting = BreakpointSetting::new(file, line);
                setting.temporary = true;
                self.send(Command::SetBreakpoint(setting.spec()), cx);
                self.breakpoints.push(setting);
            }
        }
        self.breakpoints
            .sort_by(|left, right| (&left.file, left.line).cmp(&(&right.file, right.line)));
        self.changed(cx);
    }

    pub fn remove_breakpoint(&mut self, file: PathBuf, line: u32, cx: &mut Context<Self>) {
        self.breakpoints
            .retain(|breakpoint| breakpoint.file != file || breakpoint.line != line);
        self.send(Command::RemoveBreakpoint { file, line }, cx);
        self.changed(cx);
    }

    /// The row of the gutter menu that adds a breakpoint with a condition.
    pub fn add_conditional_breakpoint(
        &mut self,
        file: PathBuf,
        line: u32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.setting(&file, line).is_none() {
            self.toggle_breakpoint(file.clone(), line, cx);
        }
        self.ask_condition(file, line, window, cx);
    }

    /// The row of the gutter menu that adds a breakpoint which logs instead of stopping.
    pub fn add_logging_breakpoint(
        &mut self,
        file: PathBuf,
        line: u32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.setting(&file, line).is_none() {
            self.toggle_non_suspending_breakpoint(file.clone(), line, cx);
        }
        self.ask_log_expression(file, line, window, cx);
    }

    /// The row of the gutter menu that opens the properties of the breakpoint there.
    pub fn edit_breakpoint_at(
        &mut self,
        file: PathBuf,
        line: u32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.setting(&file, line).is_some() {
            self.show_breakpoint_popup(file, line, window, cx);
        }
    }

    /// The row of the gutter menu that turns the breakpoint off without removing it.
    pub fn disable_breakpoint(&mut self, file: PathBuf, line: u32, cx: &mut Context<Self>) {
        self.set_enabled(file, line, false, cx);
    }

    /// Shift+click on the gutter: a breakpoint that does not stop the program and says
    /// where it was reached, as IDEA has it.
    pub fn toggle_non_suspending_breakpoint(
        &mut self,
        file: PathBuf,
        line: u32,
        cx: &mut Context<Self>,
    ) {
        if line == 0 {
            return;
        }
        let found = self
            .breakpoints
            .iter()
            .position(|breakpoint| breakpoint.file == file && breakpoint.line == line);
        match found {
            Some(index) => {
                self.breakpoints.remove(index);
                self.send(Command::RemoveBreakpoint { file, line }, cx);
            }
            None => {
                let mut setting = BreakpointSetting::new(file, line);
                setting.suspend = false;
                setting.hit_message = true;
                self.send(Command::SetBreakpoint(setting.spec()), cx);
                self.breakpoints.push(setting);
            }
        }
        self.breakpoints
            .sort_by(|left, right| (&left.file, left.line).cmp(&(&right.file, right.line)));
        self.changed(cx);
    }

    /// The method breakpoints of this file, from one, for the gutter and tests.
    pub fn function_breakpoint_lines(&self, file: &Path) -> Vec<u32> {
        let mut lines: Vec<u32> = self
            .function_breakpoints
            .iter()
            .filter(|breakpoint| breakpoint.file == file && breakpoint.line > 0)
            .map(|breakpoint| breakpoint.line)
            .collect();
        lines.sort_unstable();
        lines.dedup();
        lines
    }

    /// The field watchpoints of this file, from one, for the gutter and tests.
    pub fn field_watchpoint_lines(&self, file: &Path) -> Vec<u32> {
        let mut lines: Vec<u32> = self
            .data_breakpoints
            .iter()
            .filter(|watch| watch.file == file && watch.line > 0)
            .map(|watch| watch.line)
            .collect();
        lines.sort_unstable();
        lines.dedup();
        lines
    }

    /// The names the adapter receives for the method breakpoints, for tests.
    pub fn function_breakpoint_names(&self) -> Vec<String> {
        self.function_breakpoints
            .iter()
            .map(FunctionBreakpointSetting::dap_name)
            .collect()
    }

    /// The ids the adapter receives for the field watchpoints, for tests.
    pub fn data_breakpoint_ids(&self) -> Vec<String> {
        self.data_breakpoints
            .iter()
            .map(|watch| watch.data_id.clone())
            .collect()
    }

    /// A click on the gutter of a method declaration: a method breakpoint, or none.
    pub fn toggle_function_breakpoint(
        &mut self,
        file: PathBuf,
        line: u32,
        owner: Option<String>,
        name: String,
        cx: &mut Context<Self>,
    ) {
        let found = self
            .function_breakpoints
            .iter()
            .position(|breakpoint| breakpoint.file == file && breakpoint.line == line);
        match found {
            Some(index) => {
                self.function_breakpoints.remove(index);
            }
            None => {
                self.function_breakpoints
                    .push(FunctionBreakpointSetting::new(file, line, owner, name));
            }
        }
        self.send_function_breakpoints(cx);
        self.changed(cx);
    }

    /// A click on the gutter of a field declaration: a watchpoint on its writes.
    pub fn toggle_field_watchpoint(
        &mut self,
        file: PathBuf,
        line: u32,
        owner: Option<String>,
        name: String,
        cx: &mut Context<Self>,
    ) {
        let Some(owner) = owner.filter(|owner| !owner.is_empty()) else {
            // Without the type the adapter cannot name the field: the line stops the
            // program instead of the field.
            self.toggle_breakpoint(file, line, cx);
            return;
        };
        let found = self
            .data_breakpoints
            .iter()
            .position(|watch| watch.file == file && watch.line == line);
        match found {
            Some(index) => {
                self.data_breakpoints.remove(index);
            }
            None => {
                self.data_breakpoints
                    .push(DataBreakpointSetting::from_declaration(
                        file, line, owner, name,
                    ));
            }
        }
        self.send_data_breakpoints(cx);
        self.changed(cx);
    }

    /// Add Field Watchpoint of a variable in the Variables tree. The adapter names the
    /// field from the object that holds it.
    pub fn add_field_watchpoint(&mut self, reference: i64, name: String, cx: &mut Context<Self>) {
        let Some(debugger) = self.debugger.clone() else {
            return;
        };
        let requested = name.clone();
        let panel = cx.weak_entity();
        cx.spawn(async move |_, cx| {
            let answer = debugger
                .request(
                    "dataBreakpointInfo",
                    serde_json::json!({"variablesReference": reference, "name": requested}),
                )
                .await;
            panel
                .update(cx, |panel, cx| {
                    panel.receive_field_watchpoint(requested, answer, cx)
                })
                .ok();
        })
        .detach();
    }

    fn receive_field_watchpoint(
        &mut self,
        name: String,
        answer: Result<serde_json::Value, String>,
        cx: &mut Context<Self>,
    ) {
        let data_id = answer
            .as_ref()
            .ok()
            .and_then(|body| body.get("dataId"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .filter(|data_id| !data_id.is_empty());
        let Some(data_id) = data_id else {
            let message = match answer {
                Err(error) => error,
                Ok(body) => body
                    .get("description")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
                    .unwrap_or_else(|| "the adapter cannot watch this field".to_owned()),
            };
            self.local_console.push(ConsoleLine::new(
                format!("Add Field Watchpoint: {message}"),
                true,
            ));
            self.trim_console();
            cx.notify();
            return;
        };
        if !self
            .data_breakpoints
            .iter()
            .any(|watch| watch.data_id == data_id)
        {
            self.data_breakpoints
                .push(DataBreakpointSetting::from_variable(data_id, name));
        }
        self.send_data_breakpoints(cx);
        self.changed(cx);
    }

    /// Drop Frame: the selected frame starts again from its first line.
    pub fn restart_frame(&mut self, cx: &mut Context<Self>) {
        if let Some(frame) = self.selected_frame().cloned() {
            self.restart_frame_of(frame.id, cx);
        }
    }

    fn restart_frame_of(&mut self, frame: i64, cx: &mut Context<Self>) {
        let Some(found) = self
            .snapshot
            .frames
            .iter()
            .find(|candidate| candidate.id == frame)
            .cloned()
        else {
            return;
        };
        if self.restart_frame_enabled_for(&found) {
            self.send(Command::RestartFrame(frame), cx);
        }
    }

    /// Whether the adapter can drop the selected frame. The outermost frame has no
    /// caller to return to, and an adapter may refuse the request.
    pub fn restart_frame_enabled(&self) -> bool {
        self.selected_frame()
            .is_some_and(|frame| self.restart_frame_enabled_for(frame))
    }

    fn restart_frame_enabled_for(&self, frame: &Frame) -> bool {
        self.snapshot.capabilities.restart_frame
            && matches!(self.snapshot.state, DebugState::Stopped { .. })
            && !self
                .snapshot
                .frames
                .last()
                .is_some_and(|last| last.id == frame.id)
            && !is_main_frame(frame)
    }

    fn selected_frame(&self) -> Option<&Frame> {
        let selected = self.snapshot.selected_frame?;
        self.snapshot
            .frames
            .iter()
            .find(|frame| frame.id == selected)
    }

    /// A condition of the DAP form. An empty text clears it.
    fn set_condition(&mut self, file: PathBuf, line: u32, text: String, cx: &mut Context<Self>) {
        let text = text.trim().to_owned();
        if let Some(setting) = self.find(file, line) {
            setting.condition = (!text.is_empty()).then_some(text);
            let spec = setting.spec();
            self.send(Command::SetBreakpoint(spec), cx);
            self.changed(cx);
        }
    }

    fn set_enabled(&mut self, file: PathBuf, line: u32, on: bool, cx: &mut Context<Self>) {
        if let Some(setting) = self.find(file, line) {
            setting.enabled = on;
            let spec = setting.spec();
            self.send(Command::SetBreakpoint(spec), cx);
            self.changed(cx);
        }
    }

    fn set_suspend(&mut self, file: PathBuf, line: u32, on: bool, cx: &mut Context<Self>) {
        if let Some(setting) = self.find(file, line) {
            setting.suspend = on;
            let spec = setting.spec();
            self.send(Command::SetBreakpoint(spec), cx);
            self.changed(cx);
        }
    }

    pub(crate) fn set_hit_message(
        &mut self,
        file: PathBuf,
        line: u32,
        on: bool,
        cx: &mut Context<Self>,
    ) {
        if let Some(setting) = self.find(file, line) {
            setting.hit_message = on;
            let spec = setting.spec();
            self.send(Command::SetBreakpoint(spec), cx);
            self.changed(cx);
        }
    }

    /// The expression of Evaluate and log. An empty text turns it off.
    pub(crate) fn set_log_expression(
        &mut self,
        file: PathBuf,
        line: u32,
        text: String,
        cx: &mut Context<Self>,
    ) {
        let text = text.trim().to_owned();
        if let Some(setting) = self.find(file, line) {
            // The user edits the point: the message an agent set gives way.
            setting.log_message = None;
            setting.log_expression = (!text.is_empty()).then_some(text);
            let spec = setting.spec();
            self.send(Command::SetBreakpoint(spec), cx);
            self.changed(cx);
        }
    }

    /// The lines of the breakpoints the user turned off, for the gutter.
    fn disabled_breakpoint_lines(&self, file: &Path) -> Vec<u32> {
        self.breakpoints
            .iter()
            .filter(|breakpoint| breakpoint.file == file && !breakpoint.enabled)
            .map(|breakpoint| breakpoint.line)
            .collect()
    }

    /// Applies one property of a row of the Breakpoints window. The line, method and field
    /// points reach the adapter at once; the exception points become the filter list.
    fn edit_breakpoint_row(&mut self, row: &BreakpointRow, edit: RowEdit, cx: &mut Context<Self>) {
        match row {
            BreakpointRow::Line { file, line } => {
                let Some(setting) = self.find(file.clone(), *line) else {
                    return;
                };
                match edit {
                    RowEdit::Enabled(on) => setting.enabled = on,
                    RowEdit::Suspend(on) => setting.suspend = on,
                    RowEdit::SuspendThread(on) => setting.suspend_thread = on,
                    RowEdit::Condition(text) => setting.condition = none_if_empty(text),
                    RowEdit::HitMessage(on) => setting.hit_message = on,
                    RowEdit::LogExpression(text) => {
                        setting.log_message = None;
                        setting.log_expression = none_if_empty(text);
                    }
                    RowEdit::RemoveOnce(on) => setting.remove_once = on,
                    RowEdit::HitCondition(text) => setting.hit_condition = none_if_empty(text),
                    RowEdit::Caught(_) | RowEdit::Uncaught(_) => return,
                }
                let spec = setting.spec();
                self.send(Command::SetBreakpoint(spec), cx);
                self.changed(cx);
            }
            BreakpointRow::Function(index) => {
                let Some(setting) = self.function_breakpoints.get_mut(*index) else {
                    return;
                };
                match edit {
                    RowEdit::Enabled(on) => setting.enabled = on,
                    RowEdit::Condition(text) => setting.condition = none_if_empty(text),
                    RowEdit::HitCondition(text) => setting.hit_condition = none_if_empty(text),
                    _ => return,
                }
                self.send_function_breakpoints(cx);
                self.changed(cx);
            }
            BreakpointRow::Data(data_id) => {
                let Some(setting) = self
                    .data_breakpoints
                    .iter_mut()
                    .find(|setting| &setting.data_id == data_id)
                else {
                    return;
                };
                match edit {
                    RowEdit::Enabled(on) => setting.enabled = on,
                    RowEdit::Condition(text) => setting.condition = none_if_empty(text),
                    RowEdit::HitCondition(text) => setting.hit_condition = none_if_empty(text),
                    _ => return,
                }
                self.send_data_breakpoints(cx);
                self.changed(cx);
            }
            BreakpointRow::AnyException => {
                apply_exception_edit(&mut self.any_exception, edit);
                self.send_exception_filters(cx);
            }
            BreakpointRow::Exception(index) => {
                let Some(setting) = self.exception_points.get_mut(*index) else {
                    return;
                };
                apply_exception_edit(&mut setting.setting, edit);
                self.send_exception_filters(cx);
            }
            BreakpointRow::Filter(filter) => {
                let RowEdit::Enabled(on) = edit else {
                    return;
                };
                self.set_filter_enabled(filter.clone(), on, cx);
            }
        }
    }

    /// Turns every point of a group on or off, as the flag of the group does.
    fn set_group_enabled(&mut self, group: BreakpointGroup, on: bool, cx: &mut Context<Self>) {
        match group {
            BreakpointGroup::Line => {
                for setting in &mut self.breakpoints {
                    setting.enabled = on;
                }
                for spec in self.all_specs() {
                    self.send(Command::SetBreakpoint(spec), cx);
                }
                self.changed(cx);
            }
            BreakpointGroup::Function => {
                for setting in &mut self.function_breakpoints {
                    setting.enabled = on;
                }
                self.send_function_breakpoints(cx);
                self.changed(cx);
            }
            BreakpointGroup::Data => {
                for setting in &mut self.data_breakpoints {
                    setting.enabled = on;
                }
                self.send_data_breakpoints(cx);
                self.changed(cx);
            }
            BreakpointGroup::Exception => {
                self.any_exception.enabled = on;
                for setting in &mut self.exception_points {
                    setting.setting.enabled = on;
                }
                let extra: Vec<String> = self.enabled_filters.iter().cloned().collect();
                for filter in extra {
                    if on {
                        self.enabled_filters.insert(filter);
                    } else {
                        self.enabled_filters.remove(&filter);
                    }
                }
                self.send_exception_filters(cx);
            }
        }
    }

    fn group_enabled(&self, group: BreakpointGroup) -> bool {
        match group {
            BreakpointGroup::Line => {
                !self.breakpoints.is_empty()
                    && self.breakpoints.iter().all(|breakpoint| breakpoint.enabled)
            }
            BreakpointGroup::Function => {
                !self.function_breakpoints.is_empty()
                    && self
                        .function_breakpoints
                        .iter()
                        .all(|breakpoint| breakpoint.enabled)
            }
            BreakpointGroup::Data => {
                !self.data_breakpoints.is_empty()
                    && self
                        .data_breakpoints
                        .iter()
                        .all(|breakpoint| breakpoint.enabled)
            }
            BreakpointGroup::Exception => {
                self.any_exception.enabled
                    || self
                        .exception_points
                        .iter()
                        .any(|point| point.setting.enabled)
                    || !self.enabled_filters.is_empty()
            }
        }
    }

    /// A method breakpoint the user named in the Breakpoints window.
    pub(crate) fn add_named_function_breakpoint(&mut self, name: String, cx: &mut Context<Self>) {
        let name = name.trim().to_owned();
        if name.is_empty() {
            return;
        }
        let (owner, method) = match name.split_once('#') {
            Some((owner, method)) => (Some(owner.to_owned()), method.to_owned()),
            None => (None, name),
        };
        self.function_breakpoints
            .push(FunctionBreakpointSetting::new(
                PathBuf::new(),
                0,
                owner,
                method,
            ));
        self.send_function_breakpoints(cx);
        self.changed(cx);
    }

    /// A field watchpoint the user named in the Breakpoints window as `Owner#field`.
    pub(crate) fn add_named_data_breakpoint(&mut self, name: String, cx: &mut Context<Self>) {
        let name = name.trim().to_owned();
        if name.is_empty() {
            return;
        }
        let setting = match name.split_once('#') {
            Some((owner, field)) => DataBreakpointSetting::from_declaration(
                PathBuf::new(),
                0,
                owner.to_owned(),
                field.to_owned(),
            ),
            None => DataBreakpointSetting::from_variable(name.clone(), name),
        };
        if !self
            .data_breakpoints
            .iter()
            .any(|watch| watch.data_id == setting.data_id)
        {
            self.data_breakpoints.push(setting);
        }
        self.send_data_breakpoints(cx);
        self.changed(cx);
    }

    /// An exception breakpoint the user named by class.
    pub(crate) fn add_named_exception_breakpoint(&mut self, class: String, cx: &mut Context<Self>) {
        let class = class.trim().to_owned();
        if class.is_empty() {
            return;
        }
        self.exception_points
            .push(ExceptionPointSetting::new(class));
        self.send_exception_filters(cx);
    }

    fn remove_breakpoint_row(&mut self, row: &BreakpointRow, cx: &mut Context<Self>) {
        match row {
            BreakpointRow::Line { file, line } => {
                self.remove_breakpoint(file.clone(), *line, cx);
            }
            BreakpointRow::Function(index) => {
                if *index < self.function_breakpoints.len() {
                    self.function_breakpoints.remove(*index);
                    self.send_function_breakpoints(cx);
                    self.changed(cx);
                }
            }
            BreakpointRow::Data(data_id) => {
                self.data_breakpoints
                    .retain(|setting| &setting.data_id != data_id);
                self.send_data_breakpoints(cx);
                self.changed(cx);
            }
            BreakpointRow::AnyException | BreakpointRow::Filter(_) => {}
            BreakpointRow::Exception(index) => {
                if *index < self.exception_points.len() {
                    self.exception_points.remove(*index);
                    self.send_exception_filters(cx);
                }
            }
        }
    }

    fn find(&mut self, file: PathBuf, line: u32) -> Option<&mut BreakpointSetting> {
        self.breakpoints
            .iter_mut()
            .find(|breakpoint| breakpoint.file == file && breakpoint.line == line)
    }

    fn setting(&self, file: &Path, line: u32) -> Option<&BreakpointSetting> {
        self.breakpoints
            .iter()
            .find(|breakpoint| breakpoint.file == file && breakpoint.line == line)
    }

    /// Every breakpoint of the window, whether the list is muted or not.
    fn all_specs(&self) -> Vec<BreakpointSpec> {
        self.breakpoints
            .iter()
            .map(BreakpointSetting::spec)
            .collect()
    }

    /// The breakpoints a started session hears: none while the window mutes them.
    fn specs(&self) -> Vec<BreakpointSpec> {
        if self.muted {
            Vec::new()
        } else {
            self.all_specs()
        }
    }

    /// Every method breakpoint of the window, whether the list is muted or not.
    fn all_function_specs(&self) -> Vec<FunctionBreakpointSpec> {
        self.function_breakpoints
            .iter()
            .map(FunctionBreakpointSetting::spec)
            .collect()
    }

    fn function_specs(&self) -> Vec<FunctionBreakpointSpec> {
        if self.muted {
            Vec::new()
        } else {
            self.all_function_specs()
        }
    }

    /// Every field watchpoint of the window, whether the list is muted or not.
    fn all_data_specs(&self) -> Vec<DataBreakpointSpec> {
        self.data_breakpoints
            .iter()
            .map(DataBreakpointSetting::spec)
            .collect()
    }

    fn data_specs(&self) -> Vec<DataBreakpointSpec> {
        if self.muted {
            Vec::new()
        } else {
            self.all_data_specs()
        }
    }

    fn send_function_breakpoints(&self, cx: &mut Context<Self>) {
        self.send(Command::SetFunctionBreakpoints(self.function_specs()), cx);
    }

    fn send_data_breakpoints(&self, cx: &mut Context<Self>) {
        self.send(Command::SetDataBreakpoints(self.data_specs()), cx);
    }

    fn filter_list(&self) -> Vec<String> {
        let mut filters = self.enabled_filters.clone();
        if self.any_exception.enabled {
            if self.any_exception.caught {
                filters.insert("caught".to_owned());
            }
            if self.any_exception.uncaught {
                filters.insert("uncaught".to_owned());
            }
        }
        filters.into_iter().collect()
    }

    /// Turns one of the filters of the adapter on or off.
    fn set_filter_enabled(&mut self, filter: String, on: bool, cx: &mut Context<Self>) {
        if on {
            self.enabled_filters.insert(filter);
        } else {
            self.enabled_filters.remove(&filter);
        }
        self.send(Command::SetExceptionBreakpoints(self.filter_list()), cx);
        self.save_state();
        cx.notify();
    }

    fn send_exception_filters(&mut self, cx: &mut Context<Self>) {
        self.send(Command::SetExceptionBreakpoints(self.filter_list()), cx);
        self.save_state();
        cx.notify();
    }

    fn send(&self, command: Command, cx: &mut Context<Self>) {
        if let Some(debugger) = &self.debugger {
            let _ = debugger.send(command);
        }
        cx.notify();
    }

    /// Says in the console which client asked for the session, once the session starts.
    fn note_tool_client(&mut self) {
        let Some(client) = self.tool_client.take() else {
            return;
        };
        self.local_console
            .push(ConsoleLine::new(format!("Started by {client}"), false));
    }

    fn changed(&mut self, cx: &mut Context<Self>) {
        self.save_state();
        cx.emit(BreakpointsChanged);
        cx.notify();
    }

    fn save_state(&self) {
        let Some(store) = &self.store else {
            return;
        };
        let saved = SavedState {
            breakpoints: self
                .breakpoints
                .iter()
                .filter(|breakpoint| !breakpoint.temporary)
                .cloned()
                .collect(),
            function_breakpoints: self.function_breakpoints.clone(),
            data_breakpoints: self.data_breakpoints.clone(),
            exception_filters: self.enabled_filters.iter().cloned().collect(),
            any_exception: self.any_exception.clone(),
            exception_points: self.exception_points.clone(),
            inline_watches: self.inline_watches.clone(),
            muted: self.muted,
        };
        let Ok(json) = serde_json::to_string(&saved) else {
            return;
        };
        if let Err(error) = store.put(&state_key(&self.root), &json) {
            eprintln!("breakpoints are not saved: {error}");
        }
    }

    fn poll(&mut self, cx: &mut Context<Self>) {
        // New output reaches the search bar and the filter before the frame draws.
        if self.console_search.open || self.console_search.filtering() {
            self.rebuild_console_filter();
        }
        let mut build_changed = self.poll_build(cx);
        self.flush_tool_output();
        // The tools answer from the snapshot of this poll, so their jobs come first.
        self.poll_tool_jobs(cx);
        if let Some(reason) = self.debugger.as_ref().and_then(Debugger::take_input_error) {
            self.input_error_line(&reason);
            build_changed = true;
        }
        if self.debugger.is_some() {
            self.sync_renderers(cx);
            self.sync_inline_watches(cx);
        }
        let Some(debugger) = &self.debugger else {
            if matches!(self.snapshot.state, DebugState::Terminated) {
                self.session = None;
            }
            if let Some(hub) = cx.try_global::<ActiveDebugHub>() {
                hub.0.clear();
            }
            self.answer_tool_jobs(cx);
            if build_changed {
                cx.notify();
            }
            return;
        };
        let Some(snapshot) = debugger.poll() else {
            self.answer_tool_jobs(cx);
            if build_changed {
                cx.notify();
            }
            return;
        };
        let breakpoints_changed = snapshot.breakpoints != self.shown_breakpoints
            || snapshot.function_breakpoints != self.shown_function_breakpoints
            || snapshot.data_breakpoints != self.shown_data_breakpoints;
        self.shown_breakpoints = snapshot.breakpoints.clone();
        self.shown_function_breakpoints = snapshot.function_breakpoints.clone();
        self.shown_data_breakpoints = snapshot.data_breakpoints.clone();
        if let Some(hub) = cx.try_global::<ActiveDebugHub>() {
            hub.0.set(snapshot.clone());
        }
        // A snapshot is only sent when something changed; comparing two lists of ten
        // thousand threads here would be the work the grouping avoids.
        self.snapshot = (*snapshot).clone();
        self.merge_console();
        self.flush_tool_output();
        self.sync_identities(cx);
        // A program that reads its input needs the line of the console at hand until its first
        // stop. After a stop the tab stays where the user left it until the next stop selects
        // the Debugger; a program that asks for input later gets the console by a click.
        if self.takes_input()
            && !self.stop_shown
            && !matches!(self.snapshot.state, DebugState::Stopped { .. })
            && self.front != Front::Console
        {
            self.front = Front::Console;
        }
        if matches!(self.snapshot.state, DebugState::Terminated) {
            self.session = None;
        }
        // The temporary breakpoint lives only until the program stops anywhere. Only a
        // new stop counts: a snapshot of the stop they were set at leaves them be.
        if matches!(self.snapshot.state, DebugState::Stopped { .. })
            && self.snapshot.stops != self.seen_stops
        {
            self.seen_stops = self.snapshot.stops;
            if self.log_stop(cx) {
                self.send(Command::Continue, cx);
            } else {
                // A stop that stays shows the Debugger tab, and its frame in the editor once
                // the snapshot brings the frames.
                self.front = Front::Debugger;
                self.reveal_stop = Some(self.snapshot.stops);
                self.stop_shown = true;
            }
            self.finish_temporary(cx);
            self.finish_temporary_breakpoints(cx);
            self.finish_remove_once(cx);
            self.finish_forced_step(cx);
        }
        // The frame of a new stop is shown once the snapshot brings the frames of it.
        if self.reveal_stop == Some(self.snapshot.stops)
            && matches!(self.snapshot.state, DebugState::Stopped { .. })
            && !self.snapshot.frames.is_empty()
        {
            self.reveal_stop = None;
            self.show_stop_place(cx);
        }
        if self.asking_targets
            && let Some(targets) = self.snapshot.step_targets.clone()
        {
            self.asking_targets = false;
            if targets.len() > 1 {
                self.show_step_targets(targets, cx);
            } else if self.debugger.is_some() {
                // One call or none: a plain Step Into is what the user wanted.
                self.send(Command::StepIn, cx);
            }
        }
        // The inline values follow the stop that the stack and its scopes belong to.
        let stopped = matches!(self.snapshot.state, DebugState::Stopped { .. });
        // A stop opens its first scope, the locals, as the IDE does, and so does a frame that
        // is picked in its stack: each `scopes` request gets references of its own, so the
        // scope of one request is not the scope of the next.
        let opening = (self.snapshot.stops, self.snapshot.selected_frame);
        if stopped && !self.snapshot.frames.is_empty() && self.opened_frame != Some(opening) {
            self.opened_frame = Some(opening);
            if let Some(scope) = self.snapshot.scopes.first() {
                self.expanded.insert(scope.reference);
            }
        }
        let values_changed = if stopped
            && !self.snapshot.frames.is_empty()
            && self.values_stop != self.snapshot.stops
        {
            self.values_stop = self.snapshot.stops;
            let current = self.frame_values();
            self.changed_values = current
                .iter()
                .filter(|(name, value)| {
                    self.stop_values
                        .get(*name)
                        .is_some_and(|previous| previous.2 != value.2)
                })
                .map(|(name, _)| name.clone())
                .collect();
            self.stop_values = current;
            true
        } else if stopped && !self.snapshot.frames.is_empty() {
            // A later snapshot of the same stop may carry the values of a new inline
            // watch or of a renderer.
            true
        } else if !stopped && self.values_stop != 0 {
            self.values_stop = 0;
            self.changed_values.clear();
            true
        } else {
            false
        };
        if values_changed {
            cx.emit(ValuesChanged);
        }
        if breakpoints_changed {
            cx.emit(BreakpointsChanged);
        }
        self.answer_tool_jobs(cx);
        cx.notify();
    }

    /// The list of Shift+F7, when the line has several calls.
    fn show_step_targets(&mut self, targets: Vec<StepTarget>, cx: &mut Context<Self>) {
        let Some(handle) = self.asking_targets_window.take() else {
            return;
        };
        let panel = cx.weak_entity();
        let workspace = self.workspace.clone();
        handle
            .update(cx, |_, window, cx| {
                let view = cx.new(|cx| StepTargetsView {
                    panel: panel.clone(),
                    workspace: workspace.clone(),
                    focus: cx.focus_handle(),
                    targets,
                    selected: 0,
                });
                let focus = view.read(cx).focus.clone();
                workspace
                    .update(cx, |workspace, cx| {
                        workspace.show_overlay(view.into(), focus, window, cx)
                    })
                    .ok();
            })
            .ok();
    }

    /// Reads the build step. Returns true when the console changed.
    fn poll_build(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(console) = &self.build else {
            return false;
        };
        let mut changed = false;
        let mut exit: Option<Option<i32>> = None;
        let mut lines: Vec<(String, bool)> = Vec::new();
        let mut done = Vec::new();
        for _ in 0..EVENTS {
            let Ok(event) = console.events().try_recv() else {
                break;
            };
            changed = true;
            match event {
                ConsoleEvent::Output {
                    process,
                    stream,
                    text,
                } => {
                    self.build_output
                        .push(process, stream == Stream::Stderr, &text, &mut done);
                }
                ConsoleEvent::Started { pid, .. } => self.build_process = Some(pid),
                ConsoleEvent::Failed { message } => lines.push((message, true)),
                ConsoleEvent::Exited { code, .. } => {
                    match self.build_process.take() {
                        Some(process) => self.build_output.finish(process, &mut done),
                        None => self.build_output.finish_all(&mut done),
                    }
                    exit = Some(code);
                    break;
                }
                ConsoleEvent::Closed => {
                    self.build_output.finish_all(&mut done);
                    self.build_process = None;
                    break;
                }
            }
        }
        for line in done {
            if line.text.is_empty() {
                continue;
            }
            let error = line.spans.first().is_some_and(|(_, style)| style.error);
            lines.push((line.text, error));
        }
        for (text, error) in lines {
            self.push_output(text, error, cx);
        }
        if let Some(code) = exit {
            self.build = None;
            if code == Some(0) {
                self.push_output(
                    format!("Build finished with exit code {}", code_text(code)),
                    false,
                    cx,
                );
                self.launch_pending(cx);
            } else if self.build_stopped {
                // Stop asked for the build: the program does not start, and nothing failed.
                self.push_output("Build stopped", false, cx);
                self.pending = None;
            } else {
                self.build_failed = true;
                self.push_output(
                    format!("Build failed: exit code {}", code_text(code)),
                    true,
                    cx,
                );
                self.pending = None;
                // A failed launch keeps the console in front, and the balloon names the failure.
                self.front = Front::Console;
                self.notify_build_failed(cx);
            }
        }
        changed
    }

    /// The balloon of a build that failed: the launch that did not start, and a link that
    /// shows the Console of this window.
    fn notify_build_failed(&self, cx: &mut Context<Self>) {
        let name = self
            .restart
            .as_ref()
            .map(|restart| restart.name.clone())
            .unwrap_or_default();
        let workspace = self.workspace.clone();
        // Deferred, like every call into the workspace: the poll may be what the window is in.
        cx.defer(move |cx| {
            workspace
                .update(cx, |workspace, cx| {
                    workspace.notify_user_with_command(
                        "Build failed",
                        format!("{name} did not start: the build output is in the Console."),
                        "Show Console",
                        "debug.show_console",
                        cx,
                    )
                })
                .ok();
        });
    }

    /// Output that comes from the process of a test run, forwarded by the Run panel.
    pub fn push_output(&mut self, text: impl Into<String>, error: bool, cx: &mut Context<Self>) {
        let text = text.into();
        for line in text.lines() {
            if !line.is_empty() {
                self.local_console
                    .push(ConsoleLine::new(line.to_owned(), error));
            }
        }
        {
            let queue = cx.default_global::<crate::run::DebugOutput>();
            let mut queue = queue.0.lock().unwrap_or_else(|poison| poison.into_inner());
            queue.extend(
                text.lines()
                    .filter(|line| !line.is_empty())
                    .map(str::to_owned),
            );
        }
        self.trim_console();
        cx.notify();
    }

    /// Appends the lines the adapter sent since the last snapshot, so a line the user
    /// typed and the output that follows it stay in the order they came.
    fn merge_console(&mut self) {
        let keep = console_overlap(&self.seen_console, &self.snapshot.console);
        let appended: Vec<ConsoleLine> =
            self.snapshot.console[keep.min(self.snapshot.console.len())..].to_vec();
        self.seen_console.clone_from(&self.snapshot.console);
        if !appended.is_empty() {
            self.local_console.extend(appended);
            self.trim_console();
        }
    }

    /// A write into the program failed: the line says why, the console keeps working.
    fn input_error_line(&mut self, reason: &str) {
        self.local_console.push(ConsoleLine::new(
            format!("Cannot send input to the program: {reason}"),
            true,
        ));
        self.trim_console();
    }

    fn trim_console(&mut self) {
        let extra = self.local_console.len().saturating_sub(MAX_CONSOLE);
        if extra > 0 {
            self.local_console.drain(..extra);
        }
    }

    /// Whether the console waits for a line: the program reads its input and the input
    /// did not end.
    pub(crate) fn takes_input(&self) -> bool {
        self.console_input.is_open() && self.debugger.as_ref().is_some_and(Debugger::takes_input)
    }

    /// The line typed into the console, for tests.
    #[cfg(test)]
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn input_text(&self) -> &str {
        self.console_input.text()
    }

    fn send_console_line(&mut self, line: &str, cx: &mut Context<Self>) {
        if let Some(debugger) = &self.debugger {
            debugger.send_line(line);
        }
        self.local_console
            .push(ConsoleLine::new(line.to_owned(), false));
        self.trim_console();
        self.console_input.remember(line);
        cx.notify();
    }

    /// Send EOF: the program sees the end of its input, and nothing more is typed.
    pub(crate) fn send_eof(&mut self, cx: &mut Context<Self>) {
        if !self.takes_input() {
            return;
        }
        if let Some(debugger) = &self.debugger {
            debugger.send_eof();
        }
        self.console_input.close();
        cx.notify();
    }

    /// The context menu of the console: Send EOF, as IDEA has it.
    fn console_menu(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        if !self.takes_input() {
            return;
        }
        let panel = cx.weak_entity();
        let entry = MenuEntry::new("Send EOF", move |_, cx| {
            panel.update(cx, |panel, cx| panel.send_eof(cx)).ok();
        });
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.show_menu(position, vec![entry], cx)
            })
            .ok();
    }

    /// The context menu of a frame: Copy Stack and Drop Frame, when the adapter allows
    /// the restart. The adapter offers no request that returns from a method early, so
    /// there is no row for it: a row that silently did nothing else would lie.
    fn frame_menu(&mut self, frame: Frame, position: Point<Pixels>, cx: &mut Context<Self>) {
        let panel = cx.weak_entity();
        let frame_id = frame.id;
        let copy_panel = panel.clone();
        let copy = MenuEntry::new("Copy Stack", move |_, cx| {
            copy_panel.update(cx, |panel, cx| panel.copy_stack(cx)).ok();
        });
        let entry = if self.restart_frame_enabled_for(&frame) {
            MenuEntry::new("Drop Frame", move |_, cx| {
                panel
                    .update(cx, |panel, cx| panel.restart_frame_of(frame_id, cx))
                    .ok();
            })
        } else {
            MenuEntry::disabled("Drop Frame")
        };
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.show_menu(position, vec![copy, entry], cx)
            })
            .ok();
    }

    /// The context menu of a variable: Mark Object, Set Value and Add Field Watchpoint.
    fn variable_menu(
        &mut self,
        target: VariableTarget,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let panel = cx.weak_entity();
        let reference = target.reference;
        let name = target.name.clone();
        let watch_panel = panel.clone();
        let watch = MenuEntry::new("Add Field Watchpoint", move |_, cx| {
            watch_panel
                .update(cx, |panel, cx| {
                    panel.add_field_watchpoint(reference, name.clone(), cx)
                })
                .ok();
        });
        let mark_panel = panel.clone();
        let mark_target = target.clone();
        let mark = if self.can_mark(&target) {
            MenuEntry::new("Mark Object", move |window, cx| {
                mark_panel
                    .update(cx, |panel, cx| {
                        panel.mark_object(mark_target.clone(), window, cx)
                    })
                    .ok();
            })
        } else {
            // A value without an expression of its own cannot be asked for its identity,
            // and a mark that would follow a reused handle would lie.
            MenuEntry::disabled("Mark Object (the adapter gives no expression for this value)")
        };
        let set_panel = panel;
        let set = MenuEntry::new("Set Value", move |_, cx| {
            set_panel
                .update(cx, |panel, cx| {
                    panel.selected_variable = Some(target.clone());
                    panel.start_value_edit(cx);
                })
                .ok();
        });
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.show_menu(position, vec![mark, watch, set], cx)
            })
            .ok();
    }

    pub fn resume(&mut self, _cx: &mut Context<Self>) {
        self.run_away(_cx);
        self.send(Command::Continue, _cx);
    }

    pub fn pause(&mut self, cx: &mut Context<Self>) {
        self.send(Command::Pause, cx);
    }

    /// F11: gives the selected object a name that every value with the same identity shows.
    pub(crate) fn mark_selected_object(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(target) = self.selected_variable.clone()
            && self.can_mark(&target)
        {
            self.mark_object(target, window, cx);
        }
    }

    /// Whether the value can be marked: it is an object, the adapter named it in its
    /// frame, and the language can ask the program for the identity of what it names.
    fn can_mark(&self, target: &VariableTarget) -> bool {
        matches!(self.snapshot.state, DebugState::Stopped { .. })
            && target.object != 0
            && !target.type_name.trim().is_empty()
            && self.identity_expression(target).is_some()
    }

    /// The expression that gives the identity of the value of `target`.
    fn identity_expression(&self, target: &VariableTarget) -> Option<String> {
        let name = target
            .evaluate_name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())?;
        self.service
            .as_ref()
            .and_then(|service| service.object_identity_expression(name))
    }

    /// The Mark Object dialog: the name the user gives the object. The identity of the
    /// object is asked in the frame first, because the handle alone may name another
    /// value at the next stop.
    fn mark_object(&mut self, target: VariableTarget, window: &mut Window, cx: &mut Context<Self>) {
        let Some(expression) = self.identity_expression(&target) else {
            return;
        };
        let reference = target.object;
        let type_name = target.type_name.clone();
        let panel = cx.weak_entity();
        let submit = move |text: String, cx: &mut App| {
            let name = text.trim().to_owned();
            if name.is_empty() {
                return;
            }
            panel
                .update(cx, |panel, cx| {
                    let request = panel.next_identity;
                    panel.next_identity += 1;
                    panel.pending_marks.push((request, name, type_name.clone()));
                    panel
                        .identity_questions
                        .insert(request, IdentityQuestion { reference });
                    panel.send(
                        Command::EvaluateIdentity {
                            request,
                            expression: expression.clone(),
                        },
                        cx,
                    );
                })
                .ok();
        };
        self.workspace
            .update(cx, |workspace, cx| {
                Prompt::show("Mark Object", "Label", submit, workspace, window, cx)
            })
            .ok();
    }

    /// Copies the stack of the current thread as a text that Analyze Stack Trace reads.
    fn copy_stack(&mut self, cx: &mut Context<Self>) {
        let text = dap_client::stack_trace_text(&self.snapshot.frames);
        if !text.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    /// Adds an inline watch on a line of `file`, the line from one.
    pub(crate) fn add_inline_watch(
        &mut self,
        file: PathBuf,
        line: u32,
        expression: String,
        cx: &mut Context<Self>,
    ) {
        let expression = expression.trim().to_owned();
        if expression.is_empty() || line == 0 {
            return;
        }
        if self
            .inline_watches
            .iter()
            .any(|watch| watch.file == file && watch.line == line && watch.expression == expression)
        {
            return;
        }
        self.inline_watches.push(InlineWatchSetting {
            file,
            line,
            expression,
        });
        self.save_state();
        self.sync_inline_watches(cx);
        cx.emit(ValuesChanged);
        cx.notify();
    }

    /// Takes the inline watch of a line away, the line from one.
    pub(crate) fn remove_inline_watch_at(
        &mut self,
        file: &Path,
        line: u32,
        expression: &str,
        cx: &mut Context<Self>,
    ) {
        let before = self.inline_watches.len();
        self.inline_watches.retain(|watch| {
            !(watch.file == file && watch.line == line && watch.expression == expression)
        });
        if self.inline_watches.len() == before {
            return;
        }
        self.save_state();
        self.sync_inline_watches(cx);
        cx.emit(ValuesChanged);
        cx.notify();
    }

    /// The inline watches that stand after the lines of `file`, with their last values.
    fn inline_watch_rows(&self, file: &Path) -> Vec<editor_view::InlineWatchRow> {
        if !matches!(self.snapshot.state, DebugState::Stopped { .. }) {
            return Vec::new();
        }
        self.inline_watches
            .iter()
            .filter(|watch| same_file(&watch.file, file))
            .map(|watch| {
                let value = self
                    .snapshot
                    .inline_watch
                    .iter()
                    .find(|value| value.expression == watch.expression);
                let text = match value {
                    Some(value) => match &value.error {
                        Some(error) => format!("{} = <{}>", watch.expression, error),
                        None => format!(
                            "{} = {}",
                            watch.expression,
                            self.display_value(value.reference, &value.type_name, &value.value)
                        ),
                    },
                    None => format!("{} = …", watch.expression),
                };
                editor_view::InlineWatchRow {
                    line: watch.line.saturating_sub(1) as usize,
                    expression: watch.expression.clone(),
                    text,
                }
            })
            .collect()
    }

    /// Whether the library group with this first frame is open, for tests.
    #[cfg(test)]
    pub(crate) fn library_group_open_for_test(&self, first: i64) -> bool {
        self.expanded_library_groups.contains(&first)
    }

    /// The hide setting of the frames of libraries: the button of this session, or the
    /// window setting until the button changed it.
    fn hides_library_frames(&self, cx: &App) -> bool {
        self.hide_library_frames.unwrap_or_else(|| {
            cx.try_global::<workspace::Settings>()
                .is_none_or(|settings| settings.debug_hide_library_frames)
        })
    }

    /// The button of the frames header: folds or opens the frames of libraries.
    fn toggle_library_frames(&mut self, cx: &mut Context<Self>) {
        let hidden = self.hides_library_frames(cx);
        self.hide_library_frames = Some(!hidden);
        self.expanded_library_groups.clear();
        cx.notify();
    }

    /// Ctrl+F5: the last launch starts again, in place of the running session.
    pub fn rerun(&mut self, cx: &mut Context<Self>) {
        let Some(restart) = self.restart.clone() else {
            return;
        };
        if self.debugger.is_some() {
            self.send(Command::Disconnect, cx);
        }
        let plan = restart.plan();
        let (name, entry) = (restart.name.clone(), restart.entry.clone());
        self.start_plan(restart.service, plan, name, entry, cx);
    }

    pub fn step_over(&mut self, _cx: &mut Context<Self>) {
        self.run_away(_cx);
        self.send(Command::Next, _cx);
    }

    pub fn step_into(&mut self, _cx: &mut Context<Self>) {
        self.run_away(_cx);
        self.send(Command::StepIn, _cx);
    }

    pub fn step_out(&mut self, _cx: &mut Context<Self>) {
        self.run_away(_cx);
        self.send(Command::StepOut, _cx);
    }

    /// The program runs on: the values of the stop go away at once, without waiting for
    /// the snapshot of the command, so a step does not flash them again.
    fn run_away(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.snapshot.state, DebugState::Stopped { .. }) {
            return;
        }
        self.snapshot.state = DebugState::Running;
        self.values_stop = 0;
        self.changed_values.clear();
        cx.emit(ValuesChanged);
        cx.notify();
    }

    /// Shows the Console tab, as the link of a failed build does.
    pub fn show_console(&mut self, cx: &mut Context<Self>) {
        self.front = Front::Console;
        cx.notify();
    }

    pub fn stop(&mut self, cx: &mut Context<Self>) {
        if let Some(build) = &self.build {
            build.stop();
            self.build_stopped = true;
        }
        self.pending = None;
        self.session = None;
        self.send(Command::Disconnect, cx);
        cx.notify();
    }

    /// Ctrl+F8: a breakpoint on the caret line of the active editor.
    pub fn toggle_at_caret(&mut self, cx: &mut Context<Self>) {
        let panel = cx.weak_entity();
        let workspace = self.workspace.clone();
        // The key comes through the window while it is being updated: the editor is read
        // after that update is over.
        cx.defer(move |cx| {
            let found = workspace.read_with(cx, |workspace, cx| {
                let (path, view) = workspace.active_editor()?;
                let view = view.downcast::<editor_view::EditorView>().ok()?;
                let line = view.read(cx).caret_line()? as u32;
                Some((path, line + 1))
            });
            if let Ok(Some((path, line))) = found {
                panel
                    .update(cx, |panel, cx| panel.toggle_breakpoint(path, line, cx))
                    .ok();
            }
        });
    }

    /// Alt+F9 and Ctrl+Alt+F9: a temporary breakpoint on the caret line, then continue.
    /// Force Run to Cursor turns the other breakpoints off until the line is reached.
    pub fn run_to_cursor(&mut self, force: bool, cx: &mut Context<Self>) {
        let (active, stopped) = self.state();
        if !active || !stopped {
            return;
        }
        let panel = cx.weak_entity();
        let workspace = self.workspace.clone();
        // The key comes through the window while it is being updated.
        cx.defer(move |cx| {
            let found = workspace.read_with(cx, |workspace, cx| {
                let (path, view) = workspace.active_editor()?;
                let view = view.downcast::<editor_view::EditorView>().ok()?;
                let line = view.read(cx).caret_line()? as u32;
                Some((path, line + 1))
            });
            if let Ok(Some((path, line))) = found {
                panel
                    .update(cx, |panel, cx| {
                        panel.start_run_to_cursor(path, line, force, cx)
                    })
                    .ok();
            }
        });
    }

    fn start_run_to_cursor(
        &mut self,
        file: PathBuf,
        line: u32,
        force: bool,
        cx: &mut Context<Self>,
    ) {
        if line == 0 || self.debugger.is_none() {
            return;
        }
        self.finish_temporary(cx);
        let existing = self.setting(&file, line).is_some();
        let backup = force.then(|| self.specs());
        if let Some(specs) = &backup {
            for spec in specs {
                let mut off = spec.clone();
                off.enabled = false;
                self.send(Command::SetBreakpoint(off), cx);
            }
        }
        // The line stops whatever the user set there before: the condition of a
        // breakpoint would keep Run to Cursor from reaching it.
        let spec = BreakpointSpec {
            file: file.clone(),
            line,
            condition: None,
            hit_condition: None,
            log_message: None,
            enabled: true,
        };
        self.send(Command::SetBreakpoint(spec), cx);
        self.temporary = Some(TemporaryBreakpoint {
            file,
            line,
            added: !existing,
            backup,
        });
        self.send(Command::Continue, cx);
    }

    /// Takes the temporary breakpoint away and gives the breakpoints of Force Run to
    /// Cursor their settings back.
    fn finish_temporary(&mut self, cx: &mut Context<Self>) {
        let Some(temporary) = self.temporary.take() else {
            return;
        };
        if temporary.added {
            self.send(
                Command::RemoveBreakpoint {
                    file: temporary.file.clone(),
                    line: temporary.line,
                },
                cx,
            );
        } else if let Some(setting) = self.setting(&temporary.file, temporary.line) {
            let spec = setting.spec();
            self.send(Command::SetBreakpoint(spec), cx);
        }
        if let Some(specs) = temporary.backup {
            for spec in specs {
                self.send(Command::SetBreakpoint(spec), cx);
            }
        }
    }

    /// Writes what a breakpoint that does not stop the program says, and evaluates its
    /// expression. A message of a breakpoint that suspends the program is written here
    /// too. Returns true when the program has to continue: the adapter stopped on a
    /// point whose `logMessage` it did not read.
    fn log_stop(&mut self, cx: &mut Context<Self>) -> bool {
        // Only a breakpoint of the adapter says anything: a step or a pause lands on a
        // line for reasons of its own.
        let DebugState::Stopped { reason } = &self.snapshot.state else {
            return false;
        };
        if !matches!(reason.as_str(), "breakpoint" | "data breakpoint") {
            return false;
        }
        let Some(frame) = self
            .snapshot
            .frames
            .iter()
            .find(|frame| frame.source.is_some() && frame.line > 0)
        else {
            return false;
        };
        let (Some(file), line, name) = (frame.source.clone(), frame.line, frame.name.clone())
        else {
            return false;
        };
        let Some(setting) = self.setting(&file, line).cloned() else {
            return false;
        };
        if setting.hit_message {
            let file_name = file
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            let text = format!("Breakpoint reached at {name}({file_name}:{line})");
            self.local_console.push(ConsoleLine::new(text, false));
            self.trim_console();
        }
        if let Some(expression) = setting.log_expression.clone() {
            self.send(Command::EvaluateLog(expression), cx);
        }
        !setting.suspend
    }

    /// A temporary breakpoint of Ctrl+Alt+Shift+F8 stops the program once and goes away.
    fn finish_temporary_breakpoints(&mut self, cx: &mut Context<Self>) {
        let temporary: Vec<(PathBuf, u32)> = self
            .breakpoints
            .iter()
            .filter(|breakpoint| breakpoint.temporary)
            .map(|breakpoint| (breakpoint.file.clone(), breakpoint.line))
            .collect();
        if temporary.is_empty() {
            return;
        }
        self.breakpoints.retain(|breakpoint| !breakpoint.temporary);
        for (file, line) in temporary {
            self.send(Command::RemoveBreakpoint { file, line }, cx);
        }
        self.changed(cx);
    }

    /// Takes away the breakpoints the user marked as removed once hit, when the program
    /// stopped on their line.
    fn finish_remove_once(&mut self, cx: &mut Context<Self>) {
        let Some(frame) = self.snapshot.frames.first() else {
            return;
        };
        let Some(source) = frame.source.as_deref() else {
            return;
        };
        let remove: Vec<(PathBuf, u32)> = self
            .breakpoints
            .iter()
            .filter(|breakpoint| {
                breakpoint.enabled
                    && breakpoint.remove_once
                    && breakpoint.file == source
                    && breakpoint.line == frame.line
            })
            .map(|breakpoint| (breakpoint.file.clone(), breakpoint.line))
            .collect();
        if remove.is_empty() {
            return;
        }
        for (file, line) in remove {
            self.remove_breakpoint(file, line, cx);
        }
    }

    /// Gives the breakpoints Force Step Over turned off their settings back.
    fn finish_forced_step(&mut self, cx: &mut Context<Self>) {
        let Some(specs) = self.forced_step.take() else {
            return;
        };
        for spec in specs {
            self.send(Command::SetBreakpoint(spec), cx);
        }
    }

    /// Alt+F10: the file and line of the selected frame.
    pub fn show_execution_point(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let frame = self
            .snapshot
            .frames
            .iter()
            .find(|frame| Some(frame.id) == self.snapshot.selected_frame && frame.source.is_some())
            .or_else(|| {
                self.snapshot
                    .frames
                    .iter()
                    .find(|frame| frame.source.is_some())
            })
            .cloned();
        let Some(path) = frame.and_then(|frame| frame.source) else {
            return;
        };
        let line = self
            .snapshot
            .frames
            .iter()
            .find(|frame| frame.source.as_deref() == Some(path.as_path()))
            .map_or(0, |frame| frame.line);
        let spot = Some(editor_view::Spot::Line {
            line: line.saturating_sub(1) as usize,
            column: 0,
        });
        let workspace = self.workspace.clone();
        let handle = window.window_handle();
        // The key comes through the window while the workspace is being updated: the
        // jump has to wait for that update to end.
        cx.defer(move |cx| {
            handle
                .update(cx, |_, window, cx| {
                    workspace
                        .update(cx, |workspace, cx| {
                            navigation::go_to(workspace, path.clone(), spot, true, window, cx)
                        })
                        .ok();
                })
                .ok();
        });
    }

    /// Shift+F7: asks the adapter where a step into may go on the current line.
    pub fn smart_step_into(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (_, stopped) = self.state();
        if !stopped || self.debugger.is_none() {
            return;
        }
        self.asking_targets = true;
        self.asking_targets_window = Some(window.window_handle());
        self.send(Command::StepInTargets, cx);
    }

    /// Alt+Shift+F7: a step into library code too.
    pub fn force_step_into(&mut self, cx: &mut Context<Self>) {
        self.send(Command::ForceStepIn, cx);
    }

    /// Alt+Shift+F8: a step over that turns the breakpoints off until the call ends, so
    /// the program does not stop inside it.
    pub fn force_step_over(&mut self, cx: &mut Context<Self>) {
        let (_, stopped) = self.state();
        if !stopped || self.debugger.is_none() {
            return;
        }
        self.finish_forced_step(cx);
        let specs = self.specs();
        for spec in &specs {
            let mut off = spec.clone();
            off.enabled = false;
            self.send(Command::SetBreakpoint(off), cx);
        }
        self.forced_step = Some(specs);
        self.send(Command::Next, cx);
    }

    /// Ctrl+Alt+Shift+F8: a breakpoint on the caret line that goes away at the first stop.
    pub fn toggle_temporary_at_caret(&mut self, cx: &mut Context<Self>) {
        let panel = cx.weak_entity();
        let workspace = self.workspace.clone();
        cx.defer(move |cx| {
            let found = workspace.read_with(cx, |workspace, cx| {
                let (path, view) = workspace.active_editor()?;
                let view = view.downcast::<editor_view::EditorView>().ok()?;
                let line = view.read(cx).caret_line()? as u32;
                Some((path, line + 1))
            });
            if let Ok(Some((path, line))) = found {
                panel
                    .update(cx, |panel, cx| {
                        panel.toggle_temporary_breakpoint(path, line, cx)
                    })
                    .ok();
            }
        });
    }

    /// Ctrl+Shift+F8: the properties of the breakpoint at the caret, or the whole list
    /// when the line has none.
    pub fn edit_or_view_breakpoints(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let panel = cx.weak_entity();
        let workspace = self.workspace.clone();
        let handle = window.window_handle();
        cx.defer(move |cx| {
            let found = workspace.read_with(cx, |workspace, cx| {
                let (path, view) = workspace.active_editor()?;
                let view = view.downcast::<editor_view::EditorView>().ok()?;
                let line = view.read(cx).caret_line()? as u32;
                Some((path, line + 1))
            });
            match found {
                Ok(Some((path, line)))
                    if panel
                        .read_with(cx, |panel, _| panel.setting(&path, line).is_some())
                        .unwrap_or(false) =>
                {
                    handle
                        .update(cx, |_, window, cx| {
                            panel.update(cx, |panel, cx| {
                                panel.show_breakpoint_popup(path, line, window, cx)
                            })
                        })
                        .ok();
                }
                _ => {
                    handle
                        .update(cx, |_, window, cx| {
                            panel.update(cx, |panel, cx| panel.show_breakpoints(window, cx))
                        })
                        .ok();
                }
            }
        });
    }

    /// The popup of Ctrl+Shift+F8 on a line with a breakpoint.
    fn show_breakpoint_popup(
        &mut self,
        file: PathBuf,
        line: u32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let panel = cx.entity();
        let workspace = self.workspace.clone();
        let handle = window.window_handle();
        let view = cx.new(|cx| BreakpointPopup {
            panel,
            workspace: workspace.clone(),
            focus: cx.focus_handle(),
            file,
            line,
        });
        let focus = view.read(cx).focus.clone();
        cx.defer(move |cx| {
            handle
                .update(cx, |_, window, cx| {
                    workspace
                        .update(cx, |workspace, cx| {
                            workspace.show_overlay(view.into(), focus, window, cx)
                        })
                        .ok();
                })
                .ok();
        });
    }

    /// Ctrl+Alt+F8: the value of the selection, or of the word under the caret, in a
    /// popup at the caret.
    pub fn quick_evaluate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (_, stopped) = self.state();
        if !stopped || self.debugger.is_none() {
            return;
        }
        let panel = cx.entity();
        let workspace = self.workspace.clone();
        let handle = window.window_handle();
        cx.defer(move |cx| {
            let found = workspace.read_with(cx, |workspace, cx| {
                let (_, view) = workspace.active_editor()?;
                let view = view.downcast::<editor_view::EditorView>().ok()?;
                let editor = view.read(cx);
                let text = editor.text()?;
                let (range, head) = editor.selection()?;
                let expression = if range.is_empty() {
                    expression_at(&text, head)?
                } else {
                    text.get(range)?.to_owned()
                };
                let (position, _) = editor.popup_at_caret(160.)?;
                Some((expression, position))
            });
            if let Ok(Some((expression, position))) = found {
                let expression = expression.trim().to_owned();
                if expression.is_empty() {
                    return;
                }
                panel.update(cx, |panel, cx| {
                    panel.send(Command::EvaluateQuick(expression.clone()), cx)
                });
                handle
                    .update(cx, |_, window, cx| {
                        workspace
                            .update(cx, |workspace, cx| {
                                let weak = cx.weak_entity();
                                let view = cx.new(|cx| {
                                    QuickEvaluateView::new(
                                        panel.clone(),
                                        weak,
                                        expression,
                                        position,
                                        cx,
                                    )
                                });
                                workspace.show_popup(view.into(), window, cx);
                            })
                            .ok();
                    })
                    .ok();
            }
        });
    }

    /// Ctrl+Shift+F8: every breakpoint with its properties and the exception filters.
    pub fn show_breakpoints(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let panel = cx.weak_entity();
        let workspace = self.workspace.clone();
        let handle = window.window_handle();
        let selected = Some(initial_breakpoint_row(self));
        let view = cx.new(|cx| BreakpointsView {
            panel: panel.clone(),
            workspace: workspace.clone(),
            focus: cx.focus_handle(),
            scroll_tree: ScrollHandle::new(),
            scroll_properties: ScrollHandle::new(),
            selected,
            loaded: None,
            condition: TextField::default(),
            log_expression: TextField::default(),
            hit_condition: TextField::default(),
            focus_field: None,
            add_menu: false,
            collapsed: BTreeSet::new(),
            _panel: None,
        });
        // A point the editor or an agent adds or takes away reaches the open window.
        if let Some(panel) = panel.upgrade() {
            let subscription = view.update(cx, |_, cx| {
                cx.subscribe(
                    &panel,
                    |_: &mut BreakpointsView, _, _: &BreakpointsChanged, cx| {
                        cx.notify();
                    },
                )
            });
            view.update(cx, |view, _| view._panel = Some(subscription));
        }
        let focus = view.read(cx).focus.clone();
        // The key comes through the window while it is being updated: the overlay opens
        // after that update is over.
        cx.defer(move |cx| {
            handle
                .update(cx, |_, window, cx| {
                    workspace
                        .update(cx, |workspace, cx| {
                            workspace.show_overlay(view.into(), focus, window, cx)
                        })
                        .ok();
                })
                .ok();
        });
    }

    pub fn state(&self) -> (bool, bool) {
        let active = self.debugger.is_some()
            && !matches!(
                self.snapshot.state,
                DebugState::Inactive | DebugState::Terminated
            );
        let stopped = matches!(self.snapshot.state, DebugState::Stopped { .. });
        (active, stopped)
    }

    fn open_frame(&mut self, frame: &Frame, window: &mut Window, cx: &mut App) {
        let Some(path) = frame.source.clone() else {
            return;
        };
        if path.to_string_lossy().starts_with("jdt://") {
            self.open_class_frame(frame, window, cx);
            return;
        }
        let spot = editor_view::Spot::Line {
            line: frame.line.saturating_sub(1) as usize,
            column: 0,
        };
        self.workspace
            .update(cx, |workspace, cx| {
                navigation::go_to(workspace, path, Some(spot), true, window, cx)
            })
            .ok();
    }

    /// Opens the class of a `jdt://` frame through the language, as a jump into a class of
    /// a library does. A language without the text of the class does nothing: the frame
    /// still names its place in the list.
    fn open_class_frame(&mut self, frame: &Frame, window: &mut Window, cx: &mut App) {
        let Some(uri) = frame
            .source
            .as_ref()
            .map(|source| source.to_string_lossy().into_owned())
        else {
            return;
        };
        let line = frame.line.saturating_sub(1);
        let span = lang_api::SourceSpan {
            uri,
            span: lang_api::TextSpan {
                start: lang_api::TextPoint { line, character: 0 },
                end: lang_api::TextPoint { line, character: 0 },
            },
        };
        // The text of a class gets the language of the project files of the stack, so it
        // is highlighted and navigable as a source of that language.
        let extension = self
            .snapshot
            .frames
            .iter()
            .filter_map(|frame| frame.source.as_deref())
            .filter(|source| path_under(source, &self.root))
            .find_map(|source| source.extension())
            .map(|extension| extension.to_string_lossy().into_owned())
            .unwrap_or_default();
        let workspace = self.workspace.clone();
        let handle = window.window_handle();
        editor_view::show_location(
            &span,
            &extension,
            cx,
            move |path, cx| {
                cx.defer(move |cx| {
                    handle
                        .update(cx, |_, window, cx| {
                            workspace
                                .update(cx, |workspace, cx| {
                                    let spot = Some(editor_view::Spot::Line {
                                        line: line as usize,
                                        column: 0,
                                    });
                                    navigation::go_to(workspace, path, spot, true, window, cx)
                                })
                                .ok();
                        })
                        .ok();
                });
            },
            |_| {},
        );
    }

    fn ask_condition(
        &mut self,
        file: PathBuf,
        line: u32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let current = self
            .setting(&file, line)
            .and_then(|setting| setting.condition.clone())
            .unwrap_or_default();
        let panel = cx.weak_entity();
        let submit = move |text: String, cx: &mut App| {
            panel
                .update(cx, |panel, cx| {
                    panel.set_condition(file.clone(), line, text, cx)
                })
                .ok();
        };
        self.workspace
            .update(cx, |workspace, cx| {
                Prompt::show_with(
                    "Breakpoint Condition",
                    &current,
                    submit,
                    workspace,
                    window,
                    cx,
                )
            })
            .ok();
    }

    fn ask_log_expression(
        &mut self,
        file: PathBuf,
        line: u32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let current = self
            .setting(&file, line)
            .and_then(|setting| setting.log_expression.clone())
            .unwrap_or_default();
        let panel = cx.weak_entity();
        let submit = move |text: String, cx: &mut App| {
            panel
                .update(cx, |panel, cx| {
                    panel.set_log_expression(file.clone(), line, text, cx)
                })
                .ok();
        };
        self.workspace
            .update(cx, |workspace, cx| {
                Prompt::show_with("Evaluate and log", &current, submit, workspace, window, cx)
            })
            .ok();
    }

    /// The state of the session for the right end of the toolbar. Nothing before the
    /// first session: the empty body says it in place. A build before the program shows
    /// its own state, and so does a build that failed.
    pub(crate) fn state_text(&self) -> Option<&str> {
        if self.build.is_some() {
            return Some("Building…");
        }
        if self.build_failed {
            return Some("Build failed");
        }
        match &self.snapshot.state {
            DebugState::Inactive => None,
            DebugState::Starting => Some("Starting"),
            DebugState::Running => Some("Running"),
            DebugState::Stopped { reason } => Some(reason.as_str()),
            DebugState::Terminated => Some("Finished"),
        }
    }

    /// Whether Rerun has a launch to start again.
    pub(crate) fn can_rerun(&self) -> bool {
        self.restart.is_some()
    }

    /// The line the Debugger tab shows in place of the frames while a session has none: the
    /// build before the program, and the program that starts or runs. `None` shows the frames,
    /// or the launch screen when there is no session at all.
    pub(crate) fn live_text(&self) -> Option<&'static str> {
        if self.build.is_some() {
            return Some("Building…");
        }
        if !self.state().0 || !self.snapshot.frames.is_empty() {
            return None;
        }
        match self.snapshot.state {
            DebugState::Starting => Some("Starting…"),
            DebugState::Running => Some("The program is running"),
            _ => None,
        }
    }

    /// The Debugger tab: the threads and frames on the left, the variables and watches
    /// on the right, a divider between them.
    fn render_debugger(
        &mut self,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let body = if let Some(text) = self.live_text() {
            // A session with no frames yet: its state in place of the launch screen.
            empty_pane("debug-live", text, theme)
        } else if self.state().0 {
            div()
                .flex()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .h_full()
                .child(self.render_frames(theme, window, cx))
                .child(self.splitter(theme, cx))
                .child(self.render_variables(theme, window, cx))
                .into_any_element()
        } else if matches!(self.snapshot.state, DebugState::Terminated) {
            empty_pane("debug-empty", "Finished", theme)
        } else if let Some(name) = selected_configuration(cx) {
            self.debug_idle_body(name, theme, cx)
        } else {
            let open = cx.listener(|this, _, window, cx| {
                let workspace = this.workspace.clone();
                window.defer(cx, move |window, cx| {
                    workspace
                        .update(cx, |workspace, cx| {
                            workspace.choose_config(true, window, cx)
                        })
                        .ok();
                });
            });
            div()
                .debug_selector(|| "debug-empty".to_owned())
                .flex()
                .flex_col()
                .flex_1()
                .items_center()
                .justify_center()
                .gap(px(SPACE_S))
                .text_size(px(TEXT_SMALL))
                .text_color(theme.text_secondary)
                .child("No debug sessions")
                .child(
                    div()
                        .id("debug-empty-debug")
                        .debug_selector(|| "debug-empty-debug".to_owned())
                        .cursor_pointer()
                        .text_color(theme.accent)
                        .child("Debug…")
                        .on_click(open),
                )
                .into_any_element()
        };
        div()
            .debug_selector(|| "debug-body".to_owned())
            .flex()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .child(body)
            .into_any_element()
    }

    /// The Debug window before a session, when a configuration is selected: the configuration
    /// that Shift+F9 debugs, with the shortcut, and the link to the configurations.
    fn debug_idle_body(&self, name: String, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let platform = keymap::platform();
        let shortcut = workspace::window_key("workspace::Debug", platform)
            .map(|keys| keymap::shortcut_label(keys, platform));
        // The link runs what Shift+F9 runs, so both starts take the same path.
        let debug = cx.listener(|this, _, window, cx| {
            let workspace = this.workspace.clone();
            window.defer(cx, move |window, cx| {
                workspace
                    .update(cx, |workspace, cx| workspace.debug_selected(window, cx))
                    .ok();
            });
        });
        let edit = cx.listener(|this, _, window, cx| {
            let workspace = this.workspace.clone();
            window.defer(cx, move |window, cx| {
                workspace
                    .update(cx, |workspace, cx| {
                        workspace.run_command("run.edit_configurations", window, cx)
                    })
                    .ok();
            });
        });
        div()
            .debug_selector(|| "debug-idle".to_owned())
            .flex()
            .flex_col()
            .flex_1()
            .items_center()
            .justify_center()
            .gap(px(SPACE_S))
            .text_size(px(TEXT_SMALL))
            .text_color(theme.text_secondary)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(SPACE_M))
                    .child(
                        div()
                            .id("debug-idle-debug")
                            .debug_selector(|| "debug-idle-debug".to_owned())
                            .cursor_pointer()
                            .text_color(theme.accent)
                            .child(format!("Debug '{name}'"))
                            .on_click(debug),
                    )
                    .children(shortcut.map(|keys| div().child(keys))),
            )
            .child(
                div()
                    .id("debug-idle-edit")
                    .debug_selector(|| "debug-idle-edit".to_owned())
                    .cursor_pointer()
                    .text_color(theme.accent)
                    .child("Edit Configurations…")
                    .on_click(edit),
            )
            .into_any_element()
    }

    /// The thread of the stack: its name at the top of the frames, a click lists the
    /// threads and selects one.
    fn thread_picker(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let current = self
            .snapshot
            .current_thread
            .and_then(|id| self.snapshot.threads.iter().find(|thread| thread.id == id))
            .or_else(|| self.snapshot.threads.first());
        let Some(thread) = current else {
            return empty_line("debug-empty-threads", "No threads", theme);
        };
        let id = thread.id;
        let label = format!("{}  #{}", thread.name, thread.id);
        let hover = theme.hover;
        div()
            .id("debug-thread-picker")
            .debug_selector(move || format!("thread-{id}"))
            .flex()
            .items_center()
            .gap(px(SPACE_S))
            .flex_none()
            .h(px(ROW_HEIGHT))
            .px(px(ROW_PADDING_X))
            .cursor_pointer()
            .text_size(px(CODE_SIZE))
            .text_color(theme.text)
            .hover(move |style| style.bg(hover))
            .child(icon_sized("general/chevronDown", TEXT_SMALL, theme))
            .child(label)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    this.show_threads(event.position, cx);
                    cx.stop_propagation();
                }),
            )
            .into_any_element()
    }

    /// The list of threads under the picker. Only the current page of the virtual
    /// threads is listed, so ten thousand of them do not become ten thousand rows.
    fn show_threads(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let page = self.virtual_page();
        let list =
            dap_client::thread_list(&self.snapshot.threads, self.snapshot.current_thread, page);
        let panel = cx.weak_entity();
        let mut entries: Vec<MenuEntry> = Vec::new();
        if let Some(thread) = &list.stopped {
            entries.push(thread_entry(panel.clone(), thread));
        }
        for thread in &list.platform {
            entries.push(thread_entry(panel.clone(), thread));
        }
        for thread in &list.virtual_threads.threads {
            entries.push(thread_entry(panel.clone(), thread));
        }
        if page > 0 {
            let previous = panel.clone();
            entries.push(MenuEntry::new("Previous virtual threads", move |_, cx| {
                previous
                    .update(cx, |panel, cx| panel.set_virtual_page(page - 1, cx))
                    .ok();
            }));
        }
        if page + 1 < list.virtual_threads.pages {
            let next = panel;
            entries.push(MenuEntry::new("Next virtual threads", move |_, cx| {
                next.update(cx, |panel, cx| panel.set_virtual_page(page + 1, cx))
                    .ok();
            }));
        }
        if entries.is_empty() {
            entries.push(MenuEntry::disabled("No threads"));
        }
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.show_menu(position, entries, cx)
            })
            .ok();
    }

    /// The left column of the Debugger tab: the thread picker and the frames.
    fn render_frames(
        &mut self,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let frames = self.snapshot.frames.clone();
        let selected_frame = self.snapshot.selected_frame;
        let hide = self.hides_library_frames(cx);
        let roots = project_source_roots(cx);
        let body = if frames.is_empty() {
            empty_pane("debug-empty-frames", "No frames", theme)
        } else {
            let mut rows: Vec<AnyElement> = Vec::new();
            let mut index = 0;
            while index < frames.len() {
                if hide && is_library_frame(&frames[index], &self.root) {
                    let start = index;
                    while index < frames.len() && is_library_frame(&frames[index], &self.root) {
                        index += 1;
                    }
                    rows.push(self.library_group_row(&frames[start..index], theme, cx));
                    if self.expanded_library_groups.contains(&frames[start].id) {
                        for frame in &frames[start..index] {
                            let selected = selected_frame == Some(frame.id);
                            rows.push(self.frame_row(frame.clone(), selected, &roots, theme, cx));
                        }
                    }
                    continue;
                }
                let frame = frames[index].clone();
                let selected = selected_frame == Some(frame.id);
                rows.push(self.frame_row(frame, selected, &roots, theme, cx));
                index += 1;
            }
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .child(self.frames_header(theme, hide, cx))
                .child(
                    scrollable(
                        "debug-tree",
                        &self.scroll_tree,
                        div()
                            .id("debug-tree")
                            .flex()
                            .flex_col()
                            .overflow_y_scroll()
                            .track_scroll(&self.scroll_tree)
                            .children(rows),
                        theme,
                        window,
                        cx,
                    )
                    .flex_1()
                    .min_h_0(),
                )
                .into_any_element()
        };
        div()
            .debug_selector(|| "debug-frames".to_owned())
            .flex()
            .flex_col()
            .flex_none()
            .w(px(self.tree_width))
            .h_full()
            .min_h_0()
            .child(self.thread_picker(theme, cx))
            .child(body)
            .into_any_element()
    }

    /// The title of the frames and the button that folds the frames of libraries.
    fn frames_header(&self, theme: &Theme, hide: bool, cx: &mut Context<Self>) -> AnyElement {
        let label = if hide {
            "Show Frames from Libraries"
        } else {
            "Hide Frames from Libraries"
        };
        div()
            .flex()
            .items_center()
            .justify_between()
            .flex_none()
            .h(px(ROW_HEIGHT))
            .pl(px(SPACE_M))
            .pr(px(SPACE_S))
            .child(
                div()
                    .text_size(px(TEXT_SMALL))
                    .text_color(theme.line_number)
                    .child("Frames"),
            )
            .child(
                icon_button("debug-hide-libraries", "general/hide", label, None, theme)
                    .debug_selector(|| "debug-hide-libraries".to_owned())
                    .size(px(20.))
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_library_frames(cx))),
            )
            .into_any_element()
    }

    /// One frame of the stack, with its menu. A long place is cut at its end, and the
    /// tooltip names the whole line.
    fn frame_row(
        &mut self,
        frame: Frame,
        selected: bool,
        source_roots: &[PathBuf],
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let frame_id = frame.id;
        let menu_frame = frame.clone();
        let library = is_library_frame(&frame, &self.root);
        let text = frame_place(&frame, source_roots);
        let hint = text.clone();
        let element = row(("frame-row", frame.id as u64), &text, selected, theme)
            .text_color(if library {
                theme.text_secondary
            } else {
                theme.text
            })
            .debug_selector(move || format!("frame-{frame_id}"))
            .tooltip(move |_, cx| tooltip(hint.clone(), None, cx))
            .tooltip_show_delay(TOOLTIP_CREATE_DELAY);
        #[cfg(test)]
        let element = element.child(frame_color_probe(frame_id));
        element
            .on_click(cx.listener(move |this, _, window, cx| {
                this.send(Command::SelectFrame(frame.id), cx);
                this.open_frame(&frame, window, cx);
                cx.notify();
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    this.frame_menu(menu_frame.clone(), event.position, cx);
                    cx.stop_propagation();
                }),
            )
            .into_any_element()
    }

    /// A collapsed run of library frames: a click opens it, a second click folds it.
    fn library_group_row(
        &mut self,
        frames: &[Frame],
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(first) = frames.first() else {
            return div().into_any_element();
        };
        let key = first.id;
        let open = self.expanded_library_groups.contains(&key);
        let mut names: Vec<String> = Vec::new();
        for frame in frames {
            let name = library_group_name(frame);
            if !name.is_empty() && !names.contains(&name) {
                names.push(name);
            }
        }
        let count = frames.len();
        let text = if open {
            if names.is_empty() {
                format!("-{count} frames from libraries")
            } else {
                format!("-{count} frames from {}", names.join(", "))
            }
        } else if names.is_empty() {
            format!("+{count} frames from libraries")
        } else {
            format!("+{count} frames from {}", names.join(", "))
        };
        let selector = format!("frames-group-{key}");
        let group = div()
            .id(("frames-group", key as u64))
            .debug_selector(move || selector)
            .flex()
            .items_center()
            .h(px(ROW_HEIGHT))
            .px(px(ROW_PADDING_X))
            .cursor_pointer()
            .text_size(px(CODE_SIZE))
            .text_color(theme.text_secondary)
            .hover(move |style| style.bg(theme.caret_row))
            .child(text)
            .on_click(cx.listener(move |this, _, _, cx| {
                if !this.expanded_library_groups.remove(&key) {
                    this.expanded_library_groups.insert(key);
                }
                cx.notify();
            }));
        group.into_any_element()
    }

    /// The right column of the Debugger tab: the scopes and the watch list.
    fn render_variables(
        &mut self,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let scopes = self.snapshot.scopes.clone();
        let body = if scopes.is_empty() {
            empty_pane("debug-empty-variables", "No variables", theme)
        } else {
            let mut rows: Vec<AnyElement> = Vec::new();
            for scope in &scopes {
                self.push_scope(scope, theme, 0, &mut rows, cx);
            }
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .child(section("Variables", theme))
                .child(
                    scrollable(
                        "debug-variables-list",
                        &self.scroll_variables,
                        div()
                            .id("debug-variables-list")
                            .flex()
                            .flex_col()
                            .overflow_y_scroll()
                            .track_scroll(&self.scroll_variables)
                            .children(rows),
                        theme,
                        window,
                        cx,
                    )
                    .flex_1()
                    .min_h_0(),
                )
                .into_any_element()
        };
        let watch = self.render_watch(theme, window, cx);
        div()
            .debug_selector(|| "debug-variables".to_owned())
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .child(body)
            .child(watch)
            .into_any_element()
    }

    /// The watch list under the variables, with the button that adds an expression.
    fn render_watch(
        &mut self,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut rows: Vec<AnyElement> = Vec::new();
        for watch in &self.snapshot.watch {
            let text = match &watch.error {
                Some(error) => format!("{} = <{error}>", watch.expression),
                None => format!(
                    "{} = {}",
                    watch.expression,
                    self.display_value(watch.reference, &watch.type_name, &watch.value)
                ),
            };
            let expression = watch.expression.clone();
            let selected = self.selected_watch.as_deref() == Some(expression.as_str());
            let clicked = expression.clone();
            rows.push(
                row(format!("watch-{expression}"), &text, selected, theme)
                    .debug_selector(move || format!("watch-{expression}"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.selected_watch = Some(clicked.clone());
                        cx.notify();
                    }))
                    .into_any_element(),
            );
        }
        let mut list = div().flex().flex_col().flex_none().min_h_0();
        if rows.is_empty() {
            list = list.child(empty_line("debug-empty-watch", "No watches", theme));
        } else {
            list = list.child(
                scrollable(
                    "debug-watch-list",
                    &self.scroll_watch,
                    div()
                        .id("debug-watch-list")
                        .flex()
                        .flex_col()
                        .overflow_y_scroll()
                        .track_scroll(&self.scroll_watch)
                        .children(rows),
                    theme,
                    window,
                    cx,
                )
                .max_h(px(WATCH_HEIGHT)),
            );
        }
        if let Some(field) = &self.watch_input {
            list = list.child(
                div()
                    .id("watch-input")
                    .debug_selector(|| "watch-input".to_owned())
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .px(px(SPACE_M))
                    .py(px(SPACE_XS))
                    .text_size(px(CODE_SIZE))
                    .child("New watch: ")
                    .children(text_field_children(
                        FieldMouse::new(cx, |this| this.watch_input.as_mut()),
                        field,
                        theme.text,
                        theme.text,
                        theme.selection,
                        theme.caret,
                        CODE_SIZE,
                        true,
                    )),
            );
        }
        let header = div()
            .flex()
            .items_center()
            .justify_between()
            .flex_none()
            .h(px(ROW_HEIGHT))
            .pl(px(SPACE_M))
            .pr(px(SPACE_S))
            .child(
                div()
                    .text_size(px(TEXT_SMALL))
                    .text_color(theme.text_secondary)
                    .child("Watch"),
            )
            .child(
                icon_button("debug-watch", "general/add", "Add Watch", None, theme)
                    .debug_selector(|| "debug-watch".to_owned())
                    .size(px(20.))
                    .on_click(cx.listener(|this, _, window, cx| this.ask_watch(window, cx))),
            );
        div()
            .debug_selector(|| "debug-watch-part".to_owned())
            .flex()
            .flex_col()
            .flex_none()
            .min_h_0()
            .child(header)
            .child(list)
            .into_any_element()
    }

    /// The project files, to find the sources named in the console.
    fn file_list(&self, cx: &App) -> Arc<Vec<PathBuf>> {
        self.files
            .as_ref()
            .map_or_else(Default::default, |files| files.read(cx).clone())
    }

    /// Runs the query of the search bar over the lines of the console.
    fn rebuild_console_search(&mut self) {
        let Self {
            local_console,
            console_search,
            ..
        } = self;
        let plain: Vec<String> = local_console
            .iter()
            .map(|line| ansi::plain(&line.text))
            .collect();
        let texts: Vec<&str> = plain.iter().map(String::as_str).collect();
        console_search.run(&texts);
    }

    /// Applies the filter to the lines of the console and runs the query over the lines
    /// it keeps.
    fn rebuild_console_filter(&mut self) {
        let Self {
            local_console,
            console_search,
            ..
        } = self;
        let plain: Vec<String> = local_console
            .iter()
            .map(|line| ansi::plain(&line.text))
            .collect();
        let texts: Vec<&str> = plain.iter().map(String::as_str).collect();
        let current = console_search.current_line();
        console_search.apply_filter(&texts);
        if console_search.open {
            console_search.run(&texts);
            if let Some(line) = current {
                console_search.focus_line(line);
            }
        }
    }

    /// Ctrl+F: the search bar above the output of the console.
    fn open_console_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.front = Front::Console;
        self.console_search.open = true;
        self.rebuild_console_filter();
        window.focus(&self.search_focus, cx);
        self.scroll_to_match();
        cx.notify();
    }

    fn close_console_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.console_search.open = false;
        self.console_search.clear();
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn search_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let stroke = &event.keystroke;
        if stroke.key == "escape" {
            cx.stop_propagation();
            self.close_console_search(window, cx);
            return;
        }
        if stroke.key == "enter" || stroke.key == "f3" {
            cx.stop_propagation();
            self.search_step(!stroke.modifiers.shift, cx);
            return;
        }
        if stroke.modifiers.alt && !stroke.modifiers.control {
            let toggle = match stroke.key.as_str() {
                "c" => Some(SearchToggle::Case),
                "w" => Some(SearchToggle::Word),
                "x" => Some(SearchToggle::Regex),
                _ => None,
            };
            if let Some(toggle) = toggle {
                cx.stop_propagation();
                self.toggle_search_option(toggle, window, cx);
                return;
            }
        }
        if self
            .console_search
            .field
            .key_with_clipboard(event, false, cx)
        {
            cx.stop_propagation();
            self.rebuild_console_search();
            self.scroll_to_match();
            cx.notify();
        }
    }

    /// F3 and Shift+F3: the next match of the console, or the one before it.
    fn search_step(&mut self, forward: bool, cx: &mut Context<Self>) {
        self.console_search.step(forward);
        self.scroll_to_match();
        cx.notify();
    }

    fn scroll_to_match(&self) {
        let Some(line) = self.console_search.current_line() else {
            return;
        };
        let row = (0..line)
            .filter(|index| {
                self.local_console
                    .get(*index)
                    .is_some_and(|line| self.console_search.accepts_text(&line.text))
            })
            .count();
        self.console_scroll.scroll_to(row, ScrollStrategy::Center);
    }

    fn toggle_search_option(
        &mut self,
        toggle: SearchToggle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.console_search.toggle(toggle);
        self.rebuild_console_search();
        window.focus(&self.search_focus, cx);
        self.scroll_to_match();
        cx.notify();
    }

    /// The funnel of the console: only the lines with the text of the field stay.
    fn toggle_console_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.console_search.filter_open = !self.console_search.filter_open;
        if self.console_search.filter_open {
            window.focus(&self.filter_focus, cx);
        } else {
            self.console_search.clear_filter();
            self.rebuild_console_filter();
            window.focus(&self.focus, cx);
        }
        cx.notify();
    }

    fn filter_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key == "escape" {
            cx.stop_propagation();
            self.toggle_console_filter(window, cx);
            return;
        }
        if self
            .console_search
            .filter
            .key_with_clipboard(event, false, cx)
        {
            cx.stop_propagation();
            self.rebuild_console_filter();
            cx.notify();
        }
    }

    /// Opens a frame of the console or a place of a compiler message.
    fn open_console_line(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = self
            .local_console
            .get(index)
            .map(|line| ansi::plain(&line.text))
        else {
            return;
        };
        let files = self.file_list(cx);
        if let Some(site) = console_links::frame_site(&text) {
            if let Some(path) = site.project_file(&files) {
                self.open_console_place(path, site.line, window, cx);
            } else {
                console_links::open_frame(
                    &text,
                    site.extension(),
                    self.workspace.clone(),
                    window,
                    cx,
                );
            }
            return;
        }
        if let Some((candidate, line)) = console_links::compiler_site(&text)
            && let Some(path) = console_links::compiler_file(&candidate, &self.root, &files)
        {
            self.open_console_place(path, line, window, cx);
        }
    }

    fn open_console_place(
        &mut self,
        path: PathBuf,
        line: u32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let workspace = self.workspace.clone();
        let spot = editor_view::Spot::Line {
            line: line.saturating_sub(1) as usize,
            column: 0,
        };
        window.defer(cx, move |window, cx| {
            workspace
                .update(cx, |workspace, cx| {
                    navigation::go_to(workspace, path, Some(spot), true, window, cx)
                })
                .ok();
        });
    }

    /// The Console tab: the output of the build and of the program, and the line the
    /// program reads while it waits for input.
    fn render_console(
        &mut self,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *theme;
        let kept = self.console_search.kept_count(self.local_console.len());
        // A reader at the end follows the output; one who scrolled up keeps their
        // place until they come back to the end. A search keeps its match on screen.
        if kept > 0 && !self.console_search.open {
            self.console_scroll.follow_end(kept - 1);
        }
        let scrolled = if kept == 0 {
            scrollable(
                "debug-console-scroll",
                self.console_scroll.handle(),
                div()
                    .id("debug-console")
                    .debug_selector(|| "debug-console".to_owned())
                    .flex()
                    .flex_col()
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(|this, event: &MouseDownEvent, _, cx| {
                            this.console_menu(event.position, cx)
                        }),
                    )
                    .child(empty_line("debug-empty-console", "No output", &theme)),
                &theme,
                window,
                cx,
            )
        } else {
            scrollable(
                "debug-console-scroll",
                self.console_scroll.handle(),
                div()
                    .id("debug-console")
                    .debug_selector(|| "debug-console".to_owned())
                    .size_full()
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(|this, event: &MouseDownEvent, _, cx| {
                            this.console_menu(event.position, cx)
                        }),
                    )
                    .child(
                        uniform_list(
                            "debug-console-list",
                            kept,
                            cx.processor(move |this, range, _, cx| {
                                this.console_rows(range, &theme, cx)
                            }),
                        )
                        .track_scroll(self.console_scroll.list_handle())
                        .size_full(),
                    ),
                &theme,
                window,
                cx,
            )
        }
        .flex_1()
        .min_h_0();
        let tools = debug_console_tools(self, &theme, cx);
        let search = self
            .console_search
            .open
            .then(|| debug_search_row(self, &theme, cx));
        let filter = self
            .console_search
            .filter_open
            .then(|| debug_filter_row(self, &theme, cx));
        // The line the user types stays at the bottom of the console, so it is at hand
        // however far the output above has scrolled.
        let mut body = div()
            .debug_selector(|| "debug-console-body".to_owned())
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(tools)
            .children(search)
            .children(filter)
            .child(scrolled);
        if self.takes_input() {
            body = body.child(
                div()
                    .id("debug-console-input-row")
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(|this, event: &MouseDownEvent, _, cx| {
                            this.console_menu(event.position, cx)
                        }),
                    )
                    .child(self.console_input.element(
                        FieldMouse::new(cx, |this| Some(&mut this.console_input.field)),
                        "debug-console-input",
                        &theme,
                    )),
            );
        }
        body.into_any_element()
    }

    /// The console rows the list draws: the lines the filter keeps, with the places the
    /// search found painted over them. A row has the height of a code line, so the list
    /// knows where every line sits without drawing the lines out of view.
    fn console_rows(
        &mut self,
        range: std::ops::Range<usize>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let total = self.local_console.len();
        range
            .map(|row| {
                let index = self.console_search.line_of_row(total, row);
                let Some(line) = self.local_console.get(index) else {
                    return div().into_any_element();
                };
                let color = if line.error {
                    theme.warning_mark
                } else {
                    theme.code
                };
                let text = ansi::plain(&line.text);
                let linked = console_links::looks_like_link(&text);
                let ranges = self.console_search.ranges(index).to_vec();
                let parsed = ansi::parse_line(&line.text, false);
                let mut element = div()
                    .id(("debug-console-line", index))
                    .debug_selector(move || format!("debug-console-line-{index}"))
                    .h(px(CODE_LINE_HEIGHT))
                    .px(px(SPACE_M))
                    .whitespace_nowrap()
                    .font_family(MONO_FONT)
                    .text_size(px(CODE_SIZE))
                    .text_color(color)
                    .child(ansi::highlighted(&parsed, theme, &ranges));
                if linked {
                    element = element
                        .cursor_pointer()
                        .hover(|style| style.underline())
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_console_line(index, window, cx)
                        }));
                }
                element.into_any_element()
            })
            .collect()
    }

    /// The divider between the frames and the variables: dragging it changes the width
    /// of the frames column.
    fn splitter(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let hover = theme.hover;
        div()
            .id("debug-splitter")
            .debug_selector(|| "debug-splitter".to_owned())
            .flex_none()
            .w(px(SPLITTER_WIDTH))
            .h_full()
            .bg(theme.border)
            .cursor_col_resize()
            .hover(move |style| style.bg(hover))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    this.split_from = Some(f32::from(event.position.x));
                    cx.stop_propagation();
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, window, cx| {
                let Some(from) = this.split_from else {
                    return;
                };
                let now = f32::from(event.position.x);
                let max = (f32::from(window.viewport_size().width) - MIN_SIDE_WIDTH - 2. * SPACE_M)
                    .max(MIN_TREE_WIDTH);
                this.tree_width = (this.tree_width + now - from).clamp(MIN_TREE_WIDTH, max);
                this.split_from = Some(now);
                cx.notify();
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.split_from = None;
                    cx.notify();
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.split_from = None;
                    cx.notify();
                }),
            )
            .into_any_element()
    }

    fn push_scope(
        &self,
        scope: &Scope,
        theme: &Theme,
        depth: usize,
        rows: &mut Vec<AnyElement>,
        cx: &mut Context<Self>,
    ) {
        let reference = scope.reference;
        let arrow = if self.expanded.contains(&reference) {
            "▾ "
        } else {
            "▸ "
        };
        rows.push(
            row(
                format!("scope-{reference}"),
                &format!("{}{arrow}{}", indent(depth), scope.name),
                false,
                theme,
            )
            .debug_selector(move || format!("scope-{reference}"))
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_expanded(reference, cx)))
            .into_any_element(),
        );
        if !self.expanded.contains(&reference) {
            return;
        }
        let Some(variables) = self.snapshot.variables.get(&reference) else {
            return;
        };
        for variable in variables {
            self.push_variable(variable, reference, theme, depth + 1, rows, cx);
        }
    }

    fn push_variable(
        &self,
        variable: &Variable,
        parent: i64,
        theme: &Theme,
        depth: usize,
        rows: &mut Vec<AnyElement>,
        cx: &mut Context<Self>,
    ) {
        let reference = variable.reference;
        let open = reference != 0 && self.expanded.contains(&reference);
        let arrow = if reference == 0 {
            "  "
        } else if open {
            "▾ "
        } else {
            "▸ "
        };
        let type_name = if variable.type_name.is_empty() {
            String::new()
        } else {
            format!(" ({})", variable.type_name)
        };
        let text = format!(
            "{}{}{}{} = {}",
            indent(depth),
            arrow,
            variable.name,
            type_name,
            self.display_value(reference, &variable.type_name, &variable.value)
        );
        let name = variable.name.clone();
        let target = VariableTarget {
            reference: parent,
            name: name.clone(),
            value: variable.value.clone(),
            object: reference,
            type_name: variable.type_name.clone(),
            evaluate_name: variable.evaluate_name.clone(),
        };
        let selected = self.selected_variable.as_ref() == Some(&target);
        let editing = self
            .variable_edit
            .as_ref()
            .is_some_and(|edit| edit.target == target);
        if editing {
            let Some(field) = self.variable_edit.as_ref().map(|edit| &edit.field) else {
                return;
            };
            let children = text_field_children(
                FieldMouse::new(cx, move |this| {
                    this.variable_edit.as_mut().map(|edit| &mut edit.field)
                }),
                field,
                theme.text,
                theme.text,
                theme.selection,
                theme.caret,
                CODE_SIZE,
                true,
            );
            rows.push(
                div()
                    .id("variable-edit")
                    .debug_selector(|| "variable-edit".to_owned())
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .px(px(SPACE_M))
                    .py(px(SPACE_XS))
                    .text_size(px(CODE_SIZE))
                    .child(format!("{}{name} = ", indent(depth)))
                    .children(children)
                    .into_any_element(),
            );
        } else {
            let click_target = target.clone();
            let menu_target = target.clone();
            let selector_name = name.clone();
            let row = row(format!("var-{name}"), &text, selected, theme)
                .debug_selector(move || format!("var-{selector_name}"))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.selected_variable = Some(click_target.clone());
                    if reference != 0 {
                        this.toggle_expanded(reference, cx);
                    }
                    cx.notify();
                }))
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                        this.selected_variable = Some(menu_target.clone());
                        this.variable_menu(menu_target.clone(), event.position, cx);
                        cx.stop_propagation();
                    }),
                );
            rows.push(row.into_any_element());
        }
        if let Some(error) = self
            .snapshot
            .variable_error
            .as_ref()
            .filter(|error| error.reference == parent && error.name == name)
        {
            rows.push(
                div()
                    .debug_selector(|| "variable-error".to_owned())
                    .px(px(20. + 8. * depth as f32))
                    .pb(px(2.))
                    .text_color(theme.warning_mark)
                    .text_size(px(TEXT_SMALL))
                    .child(error.message.clone())
                    .into_any_element(),
            );
        }
        if !open {
            return;
        }
        let Some(children) = self.snapshot.variables.get(&reference) else {
            return;
        };
        for child in children {
            self.push_variable(child, reference, theme, depth + 1, rows, cx);
        }
    }

    /// The keys of the panel while it has the focus: the fields of New Watch and of Set
    /// Value, Enter and Escape.
    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = &event.keystroke;
        if key.modifiers.control && !key.modifiers.alt && !key.modifiers.shift && key.key == "f" {
            cx.stop_propagation();
            self.open_console_search(window, cx);
            return;
        }
        if self.console_search.open && key.key == "f3" {
            cx.stop_propagation();
            self.search_step(!key.modifiers.shift, cx);
            return;
        }
        if self.variable_edit.is_some() {
            if key.key == "enter" {
                let Some(edit) = self.variable_edit.take() else {
                    return;
                };
                let value = edit.field.text.trim().to_owned();
                if !value.is_empty() {
                    self.send(
                        Command::SetVariable {
                            reference: edit.target.reference,
                            name: edit.target.name,
                            value,
                        },
                        cx,
                    );
                }
                cx.stop_propagation();
                cx.notify();
                return;
            }
            if key.key == "escape" {
                self.variable_edit = None;
                cx.stop_propagation();
                cx.notify();
                return;
            }
            if let Some(edit) = &mut self.variable_edit
                && edit.field.key_with_clipboard(event, false, cx)
            {
                cx.stop_propagation();
                cx.notify();
            }
            return;
        }
        if self.watch_input.is_some() {
            if key.key == "enter" {
                let field = self.watch_input.take().unwrap_or_default();
                self.add_watch(field.text.trim().to_owned(), cx);
                cx.stop_propagation();
                cx.notify();
                return;
            }
            if key.key == "escape" {
                self.watch_input = None;
                cx.stop_propagation();
                cx.notify();
                return;
            }
            if let Some(field) = &mut self.watch_input
                && field.key_with_clipboard(event, false, cx)
            {
                cx.stop_propagation();
                cx.notify();
            }
            return;
        }
        if self.takes_input() {
            match self.console_input.key(event, cx) {
                ConsoleKey::Ignored => {}
                ConsoleKey::Edited => {
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                ConsoleKey::Lines(lines) => {
                    cx.stop_propagation();
                    for line in lines {
                        self.send_console_line(&line, cx);
                    }
                    return;
                }
                ConsoleKey::Eof => {
                    cx.stop_propagation();
                    self.send_eof(cx);
                    return;
                }
            }
        }
        // Escape keeps its meaning of the window when no field is open.
        if key.key == "escape" {
            let workspace = self.workspace.clone();
            workspace
                .update(cx, |workspace, cx| workspace.dismiss(window, cx))
                .ok();
            cx.stop_propagation();
        }
    }

    fn add_watch(&mut self, expression: String, cx: &mut Context<Self>) {
        if expression.is_empty() || self.watch.contains(&expression) {
            return;
        }
        self.watch.push(expression.clone());
        self.selected_watch = Some(expression.clone());
        self.send(Command::AddWatch(expression), cx);
        cx.notify();
    }

    /// Insert: a row for a new watch expression.
    fn start_watch(&mut self, cx: &mut Context<Self>) {
        if self.watch_input.is_none() {
            self.watch_input = Some(TextField::default());
            self.variable_edit = None;
        }
        cx.notify();
    }

    /// Delete: the selected watch expression goes away.
    fn delete_watch(&mut self, cx: &mut Context<Self>) {
        let Some(expression) = self.selected_watch.take() else {
            return;
        };
        self.watch.retain(|item| item != &expression);
        self.send(Command::RemoveWatch(expression), cx);
        cx.notify();
    }

    /// F2: a field in place of the value of the selected variable.
    fn start_value_edit(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self.selected_variable.clone() else {
            return;
        };
        self.watch_input = None;
        // A refusal of the last edit belongs to the row that was edited.
        self.snapshot.variable_error = None;
        // Typing replaces the old value at once, as the field of IDEA does.
        let mut field = TextField::from_text(target.value.clone());
        field.select_all();
        self.variable_edit = Some(VariableEdit { field, target });
        cx.notify();
    }

    fn ask_watch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let panel = cx.weak_entity();
        let submit = move |text: String, cx: &mut App| {
            panel
                .update(cx, |panel, cx| {
                    let expression = text.trim().to_owned();
                    if !expression.is_empty() && !panel.watch.contains(&expression) {
                        panel.watch.push(expression.clone());
                        panel.send(Command::AddWatch(expression), cx);
                        cx.notify();
                    }
                })
                .ok();
        };
        self.workspace
            .update(cx, |workspace, cx| {
                Prompt::show("Add Watch", "Expression", submit, workspace, window, cx)
            })
            .ok();
    }

    /// Alt+F8: the Evaluate window, with the selected text of the editor as the first
    /// expression.
    fn ask_evaluate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let panel = cx.entity();
        let workspace = self.workspace.clone();
        let handle = window.window_handle();
        // Alt+F8 comes through the window while it is being updated, so the text of the
        // editor and the overlay wait for that update to end.
        cx.defer(move |cx| {
            let workspace = workspace.clone();
            handle
                .update(cx, |_, window, cx| {
                    let selected = workspace
                        .upgrade()
                        .and_then(|workspace| {
                            let (_, view) = workspace.read(cx).active_editor()?;
                            let view = view.downcast::<editor_view::EditorView>().ok()?;
                            let editor = view.read(cx);
                            let (range, _) = editor.selection()?;
                            let text = editor.text()?;
                            (!range.is_empty()).then(|| text.get(range).map(str::to_owned))?
                        })
                        .unwrap_or_default();
                    let view = cx.new(|cx| {
                        EvaluateView::new(panel.clone(), workspace.clone(), selected, cx)
                    });
                    let focus = view.read(cx).focus.clone();
                    workspace
                        .update(cx, |workspace, cx| {
                            workspace.show_overlay(view.into(), focus, window, cx)
                        })
                        .ok();
                })
                .ok();
        });
    }

    /// Runs an expression in the selected frame for the Evaluate window and keeps it in
    /// the history of the window.
    pub(crate) fn evaluate(&mut self, expression: String, cx: &mut Context<Self>) {
        let expression = expression.trim().to_owned();
        if expression.is_empty() {
            return;
        }
        if self.evaluate_history.last() != Some(&expression) {
            self.evaluate_history.push(expression.clone());
        }
        self.send(Command::EvaluateWindow(expression), cx);
        cx.notify();
    }

    /// The expressions of the Evaluate window, for a test.
    #[cfg(test)]
    pub(crate) fn evaluate_history(&self) -> &[String] {
        &self.evaluate_history
    }
}

/// The identifier chain, with dots, around a byte offset of the text.
fn expression_at(text: &str, head: usize) -> Option<String> {
    if text.is_empty() {
        return None;
    }
    let head = head.min(text.len());
    let ident = |ch: char| ch.is_alphanumeric() || ch == '_' || ch == '$' || ch == '.';
    let mut start = head;
    while start > 0 {
        let previous = text[..start].chars().next_back()?;
        if !ident(previous) {
            break;
        }
        start -= previous.len_utf8();
    }
    let mut end = head;
    while end < text.len() {
        let next = text[end..].chars().next()?;
        if !ident(next) {
            break;
        }
        end += next.len_utf8();
    }
    let expression = text.get(start..end)?.trim_matches('.');
    if expression.is_empty()
        || !expression
            .chars()
            .next()
            .is_some_and(|ch| ch.is_alphanumeric() || ch == '_' || ch == '$')
    {
        return None;
    }
    Some(expression.to_owned())
}

/// The popup of Ctrl+Alt+F8: the value at the caret, with the fields of an object.
struct QuickEvaluateView {
    panel: Entity<DebugPanel>,
    scroll: ScrollHandle,
    workspace: WeakEntity<Workspace>,
    expression: String,
    position: Point<Pixels>,
    expanded: BTreeSet<i64>,
    _panel: Subscription,
}

impl QuickEvaluateView {
    fn new(
        panel: Entity<DebugPanel>,
        workspace: WeakEntity<Workspace>,
        expression: String,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) -> Self {
        let _panel = cx.observe(&panel, |_, _, cx| cx.notify());
        Self {
            panel,
            scroll: ScrollHandle::new(),
            workspace,
            expression,
            position,
            expanded: BTreeSet::new(),
            _panel,
        }
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace
            .update(cx, |workspace, cx| workspace.dismiss(window, cx))
            .ok();
    }

    fn toggle(&mut self, reference: i64, cx: &mut Context<Self>) {
        if !self.expanded.remove(&reference) {
            self.expanded.insert(reference);
            self.panel
                .update(cx, |panel, cx| panel.expand(reference, cx));
        }
        cx.notify();
    }

    fn value_rows(
        &self,
        reference: i64,
        depth: usize,
        rows: &mut Vec<AnyElement>,
        cx: &mut Context<Self>,
    ) {
        let Some(variables) = self
            .panel
            .read(cx)
            .snapshot()
            .variables
            .get(&reference)
            .cloned()
        else {
            return;
        };
        let theme = *cx.global::<Theme>();
        for variable in variables {
            let open = variable.reference != 0 && self.expanded.contains(&variable.reference);
            let arrow = if variable.reference == 0 {
                "  "
            } else if open {
                "▾ "
            } else {
                "▸ "
            };
            let name = variable.name.clone();
            let child = variable.reference;
            let selector = format!("quick-evaluate-row-{name}");
            let row = div()
                .id(SharedString::from(format!("quick-row-{depth}-{name}")))
                .debug_selector(move || selector.clone())
                .flex()
                .items_center()
                .gap(px(4.))
                .px(px(ROW_PADDING_X))
                .py(px(SPACE_XS))
                .cursor_pointer()
                .text_color(theme.text)
                .child(format!(
                    "{}{}{} = {}",
                    "  ".repeat(depth),
                    arrow,
                    variable.name,
                    variable.value
                ))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if child != 0 {
                        this.toggle(child, cx);
                    }
                }));
            rows.push(row.into_any_element());
            if open {
                self.value_rows(variable.reference, depth + 1, rows, cx);
            }
        }
    }
}

impl Render for QuickEvaluateView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let evaluation = self
            .panel
            .read(cx)
            .snapshot()
            .evaluation
            .clone()
            .filter(|evaluation| evaluation.expression == self.expression);
        let mut rows: Vec<AnyElement> = Vec::new();
        let reference = evaluation
            .as_ref()
            .map_or(0, |evaluation| evaluation.reference);
        if let Some(evaluation) = &evaluation {
            if let Some(error) = &evaluation.error {
                rows.push(
                    div()
                        .px(px(ROW_PADDING_X))
                        .py(px(SPACE_XS))
                        .text_color(theme.warning_mark)
                        .child(error.clone())
                        .into_any_element(),
                );
            } else {
                let type_name = if evaluation.type_name.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", evaluation.type_name)
                };
                let open = reference != 0 && self.expanded.contains(&reference);
                let arrow = if reference == 0 {
                    ""
                } else if open {
                    "▾ "
                } else {
                    "▸ "
                };
                let target = reference;
                rows.push(
                    div()
                        .id("quick-evaluate-value")
                        .debug_selector(|| "quick-evaluate-value".to_owned())
                        .flex()
                        .items_center()
                        .gap(px(4.))
                        .px(px(ROW_PADDING_X))
                        .py(px(SPACE_XS))
                        .cursor_pointer()
                        .text_color(theme.text)
                        .child({
                            let shown = self.panel.read(cx).display_value(
                                reference,
                                &evaluation.type_name,
                                &evaluation.value,
                            );
                            format!("{arrow}{}{type_name} = {shown}", evaluation.expression)
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if target != 0 {
                                this.toggle(target, cx);
                            }
                        }))
                        .into_any_element(),
                );
                if open {
                    self.value_rows(reference, 1, &mut rows, cx);
                }
            }
        } else {
            rows.push(
                div()
                    .px(px(ROW_PADDING_X))
                    .py(px(SPACE_XS))
                    .text_color(theme.text_secondary)
                    .child("Evaluating…")
                    .into_any_element(),
            );
        }
        let shell = surface(&theme)
            .id("quick-evaluate")
            .debug_selector(|| "quick-evaluate".to_owned());
        let contents = scrollable(
            "quick-evaluate",
            &self.scroll,
            div()
                .id("quick-evaluate-list")
                .flex()
                .flex_col()
                .max_h(px(320.))
                .overflow_y_scroll()
                .track_scroll(&self.scroll)
                .p(px(POPUP_PADDING))
                .gap(px(2.))
                .text_size(px(CODE_SIZE))
                .children(rows),
            &theme,
            window,
            cx,
        );
        let close = cx.listener(|this, _: &ClickEvent, window, cx| this.close(window, cx));
        dialog_window(
            DialogSpec {
                key: "quick-evaluate",
                title: "Quick Evaluate".into(),
                extra: None,
                place: DialogPlace {
                    left: f32::from(self.position.x),
                    top: f32::from(self.position.y),
                    width: 420.,
                    height: None,
                },
                min: (280., 120.),
            },
            shell,
            window,
            cx,
            close,
            contents,
        )
    }
}

/// The Evaluate window of Alt+F8: an expression, its result with the fields of the value
/// and the history of the expressions of the session.
struct EvaluateView {
    panel: Entity<DebugPanel>,
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    scroll: ScrollHandle,
    field: TextField,
    /// The expression of the last answer, `None` before the first run.
    expression: Option<String>,
    /// The objects whose fields the window shows.
    expanded: BTreeSet<i64>,
    /// The root the window opened by itself, and the expression it belongs to, so a new
    /// answer opens its own root once.
    opened: Option<(String, i64)>,
    _panel: Subscription,
}

impl EvaluateView {
    fn new(
        panel: Entity<DebugPanel>,
        workspace: WeakEntity<Workspace>,
        initial: String,
        cx: &mut Context<Self>,
    ) -> Self {
        let _panel = cx.observe(&panel, |this, panel, cx| this.sync(panel, cx));
        let mut field = TextField::from_text(initial);
        field.select_all();
        Self {
            panel,
            workspace,
            focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            field,
            expression: None,
            expanded: BTreeSet::new(),
            opened: None,
            _panel,
        }
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace
            .update(cx, |workspace, cx| workspace.dismiss(window, cx))
            .ok();
    }

    /// Opens the root of a new answer, so the fields of the value stand under its row as
    /// they do in the tree of Variables.
    fn sync(&mut self, panel: Entity<DebugPanel>, cx: &mut Context<Self>) {
        let Some(expression) = self.expression.clone() else {
            return;
        };
        let reference = panel
            .read(cx)
            .snapshot()
            .evaluation
            .as_ref()
            .filter(|evaluation| evaluation.expression == expression)
            .map_or(0, |evaluation| evaluation.reference);
        if reference != 0
            && self.opened.as_ref() != Some(&(expression.clone(), reference))
            && self.expanded.insert(reference)
        {
            self.opened = Some((expression, reference));
            panel.update(cx, |panel, cx| panel.expand(reference, cx));
        }
        cx.notify();
    }

    fn run(&mut self, cx: &mut Context<Self>) {
        let expression = self.field.text.trim().to_owned();
        if expression.is_empty() {
            return;
        }
        self.expression = Some(expression.clone());
        self.expanded.clear();
        self.opened = None;
        self.panel
            .update(cx, |panel, cx| panel.evaluate(expression, cx));
        cx.notify();
    }

    fn add_watch(&mut self, cx: &mut Context<Self>) {
        let expression = match &self.expression {
            Some(expression) => expression.clone(),
            None => self.field.text.trim().to_owned(),
        };
        if expression.is_empty() {
            return;
        }
        self.panel
            .update(cx, |panel, cx| panel.add_watch(expression, cx));
    }

    fn toggle(&mut self, reference: i64, cx: &mut Context<Self>) {
        if !self.expanded.remove(&reference) {
            self.expanded.insert(reference);
            self.panel
                .update(cx, |panel, cx| panel.expand(reference, cx));
        }
        cx.notify();
    }

    fn value_rows(
        &self,
        reference: i64,
        depth: usize,
        rows: &mut Vec<AnyElement>,
        cx: &mut Context<Self>,
    ) {
        let Some(variables) = self
            .panel
            .read(cx)
            .snapshot()
            .variables
            .get(&reference)
            .cloned()
        else {
            return;
        };
        let theme = *cx.global::<Theme>();
        for variable in variables {
            let open = variable.reference != 0 && self.expanded.contains(&variable.reference);
            let arrow = if variable.reference == 0 {
                "  "
            } else if open {
                "▾ "
            } else {
                "▸ "
            };
            let name = variable.name.clone();
            let child = variable.reference;
            let selector = format!("evaluate-row-{depth}-{name}");
            let row = div()
                .id(SharedString::from(format!("evaluate-row-{depth}-{name}")))
                .debug_selector(move || selector.clone())
                .flex()
                .items_center()
                .gap(px(4.))
                .px(px(ROW_PADDING_X))
                .py(px(SPACE_XS))
                .cursor_pointer()
                .text_color(theme.text)
                .child(format!(
                    "{}{}{} = {}",
                    "  ".repeat(depth),
                    arrow,
                    variable.name,
                    variable.value
                ))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if child != 0 {
                        this.toggle(child, cx);
                    }
                }));
            rows.push(row.into_any_element());
            if open {
                self.value_rows(variable.reference, depth + 1, rows, cx);
            }
        }
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = &event.keystroke;
        if key.key == "escape" {
            self.close(window, cx);
            cx.stop_propagation();
            return;
        }
        if key.key == "enter" && !key.modifiers.control && !key.modifiers.alt {
            if key.modifiers.shift {
                self.field.insert_text("\n");
            } else {
                self.run(cx);
            }
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if self.field.key_with_clipboard(event, true, cx) {
            cx.stop_propagation();
            cx.notify();
        }
    }
}

impl Render for EvaluateView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let history = self.panel.read(cx).evaluate_history.clone();
        let evaluation = self.expression.as_ref().and_then(|expression| {
            self.panel
                .read(cx)
                .snapshot()
                .evaluation
                .clone()
                .filter(|evaluation| &evaluation.expression == expression)
        });
        let mut rows: Vec<AnyElement> = Vec::new();
        if let Some(evaluation) = &evaluation {
            if let Some(error) = &evaluation.error {
                rows.push(
                    div()
                        .px(px(ROW_PADDING_X))
                        .py(px(SPACE_XS))
                        .text_color(theme.warning_mark)
                        .child(error.to_string())
                        .into_any_element(),
                );
            } else {
                let reference = evaluation.reference;
                let type_name = if evaluation.type_name.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", evaluation.type_name)
                };
                let open = reference != 0 && self.expanded.contains(&reference);
                let arrow = if reference == 0 {
                    ""
                } else if open {
                    "▾ "
                } else {
                    "▸ "
                };
                let shown = self.panel.read(cx).display_value(
                    reference,
                    &evaluation.type_name,
                    &evaluation.value,
                );
                rows.push(
                    div()
                        .id("evaluate-value")
                        .debug_selector(|| "evaluate-value".to_owned())
                        .flex()
                        .items_center()
                        .gap(px(4.))
                        .px(px(ROW_PADDING_X))
                        .py(px(SPACE_XS))
                        .cursor_pointer()
                        .text_color(theme.text)
                        .child(format!(
                            "{arrow}{}{type_name} = {shown}",
                            evaluation.expression
                        ))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if reference != 0 {
                                this.toggle(reference, cx);
                            }
                        }))
                        .into_any_element(),
                );
                if open {
                    self.value_rows(reference, 1, &mut rows, cx);
                }
            }
        }
        let has_text = !self.field.text.trim().is_empty();
        let result = scrollable(
            "evaluate-result",
            &self.scroll,
            div().flex().flex_col().gap(px(2.)).children(rows),
            &theme,
            window,
            cx,
        )
        .max_h(px(CODE_LINE_HEIGHT * 8.));
        let field = div()
            .flex()
            .flex_col()
            .flex_none()
            .min_h(px(CODE_LINE_HEIGHT * 2.))
            .px(px(SPACE_S))
            .py(px(SPACE_XS))
            .rounded(px(CONTROL_RADIUS))
            .border_1()
            .border_color(theme.control_border)
            .text_color(theme.text)
            .children(text_field_lines_named(
                FieldMouse::new(cx, |this| Some(&mut this.field)),
                "evaluate-field",
                &self.field,
                theme.text,
                theme.text,
                theme.selection,
                theme.caret,
                CODE_LINE_HEIGHT,
                true,
            ));
        let history_rows: Vec<AnyElement> = history
            .iter()
            .rev()
            .take(6)
            .enumerate()
            .map(|(index, expression)| {
                let text = expression.clone();
                div()
                    .id(SharedString::from(format!("evaluate-history-{index}")))
                    .debug_selector(move || format!("evaluate-history-{index}"))
                    .flex()
                    .items_center()
                    .px(px(ROW_PADDING_X))
                    .py(px(SPACE_XS))
                    .cursor_pointer()
                    .text_color(theme.text_secondary)
                    .hover(move |style| style.bg(theme.hover))
                    .child(expression.clone())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.field = TextField::from_text(text.clone());
                        this.field.select_all();
                        cx.notify();
                    }))
                    .into_any_element()
            })
            .collect();
        let shell = surface(&theme)
            .id("evaluate-window")
            .debug_selector(|| "evaluate-window".to_owned())
            .track_focus(&self.focus)
            .text_size(px(CODE_SIZE))
            .on_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)));
        // The buttons close the window's contents, below the result, as in the other dialogs.
        let buttons = DialogButtons::new()
            .primary(
                dialog_button(
                    "evaluate-run",
                    "evaluate-run".to_owned(),
                    "Evaluate",
                    true,
                    has_text,
                    &theme,
                )
                .on_click(cx.listener(|this, _, _, cx| this.run(cx))),
            )
            .other(
                dialog_button(
                    "evaluate-add-watch",
                    "evaluate-add-watch".to_owned(),
                    "Add to Watches",
                    false,
                    has_text,
                    &theme,
                )
                .on_click(cx.listener(|this, _, _, cx| this.add_watch(cx))),
            )
            .into_row();
        let contents = div()
            .flex()
            .flex_col()
            .gap(px(SPACE_S))
            .p(px(DIALOG_PADDING))
            .child(field)
            .child(result)
            .when(!history.is_empty(), |this| {
                this.child(
                    div()
                        .px(px(SPACE_M))
                        .pt(px(SPACE_S))
                        .text_size(px(TEXT_SMALL))
                        .text_color(theme.line_number)
                        .child("History"),
                )
                .children(history_rows)
            })
            .child(buttons);
        let viewport = window.viewport_size();
        let width = px(560.);
        let left = (viewport.width - width) / 2.;
        let top = viewport.height * 0.2;
        let close = cx.listener(|this, _: &ClickEvent, window, cx| this.close(window, cx));
        dialog_window(
            DialogSpec {
                key: "evaluate",
                title: "Evaluate Expression".into(),
                extra: None,
                place: DialogPlace {
                    left: f32::from(left),
                    top: f32::from(top),
                    width: f32::from(width),
                    height: None,
                },
                min: (360., 200.),
            },
            shell,
            window,
            cx,
            close,
            contents,
        )
    }
}

/// The list of Shift+F7 when the line has several calls.
struct StepTargetsView {
    panel: WeakEntity<DebugPanel>,
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    targets: Vec<StepTarget>,
    selected: usize,
}

impl StepTargetsView {
    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace
            .update(cx, |workspace, cx| workspace.dismiss(window, cx))
            .ok();
    }

    fn choose(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.targets.get(index) else {
            return;
        };
        let id = target.id;
        self.panel
            .update(cx, |panel, cx| {
                panel.send(Command::StepInTarget(id), cx);
                cx.notify();
            })
            .ok();
        self.close(window, cx);
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "up" => {
                self.selected = self.selected.saturating_sub(1);
                cx.stop_propagation();
                cx.notify();
            }
            "down" => {
                if self.selected + 1 < self.targets.len() {
                    self.selected += 1;
                }
                cx.stop_propagation();
                cx.notify();
            }
            "enter" => {
                cx.stop_propagation();
                self.choose(self.selected, window, cx);
            }
            _ => {}
        }
    }
}

impl Render for StepTargetsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let total = self.targets.len();
        let rows = self.targets.iter().enumerate().map(|(index, target)| {
            let id = target.id;
            let label = if target.label.is_empty() {
                format!("call #{id}")
            } else {
                target.label.clone()
            };
            let selected = index == self.selected;
            div()
                .id(SharedString::from(format!("step-target-{id}")))
                .debug_selector(move || format!("step-target-{id}"))
                .flex()
                .items_center()
                .gap(px(SPACE_S))
                .px(px(8.))
                .py(px(SPACE_S))
                .cursor_pointer()
                .text_color(theme.text)
                .when(selected, |row| row.bg(theme.selection))
                .when(!selected, |row| row.hover(|row| row.bg(theme.hover)))
                .child(label)
                .on_click(cx.listener(move |this, _, window, cx| this.choose(index, window, cx)))
        });
        let shell = surface(&theme)
            .id("step-targets")
            .debug_selector(|| "step-targets".to_owned())
            .track_focus(&self.focus)
            .key_context("StepTargets")
            .text_size(px(UI_SIZE))
            .on_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)));
        let contents = div()
            .flex()
            .flex_col()
            .gap(px(SPACE_S))
            .p(px(SPACE_M))
            .children(rows)
            .child(
                div()
                    .px(px(SPACE_M))
                    .pt(px(SPACE_S))
                    .text_size(px(TEXT_SMALL))
                    .text_color(theme.text_secondary)
                    .child("Enter steps into the chosen call, Escape cancels"),
            );
        let viewport = window.viewport_size();
        let left = (viewport.width - px(360.)) / 2.;
        let top = viewport.height * 0.25;
        let close = cx.listener(|this, _: &ClickEvent, window, cx| this.close(window, cx));
        dialog_window(
            DialogSpec {
                key: "step-targets",
                title: format!("Step Into ({total} calls)").into(),
                extra: None,
                place: DialogPlace {
                    left: f32::from(left),
                    top: f32::from(top),
                    width: 360.,
                    height: None,
                },
                min: (280., 120.),
            },
            shell,
            window,
            cx,
            close,
            contents,
        )
    }
}

/// The rules of the settings: `type = display` or `type = display | children`, one rule
/// per semicolon. A rule without a type or a display is skipped.
fn parse_renderers(text: &str) -> Vec<TypeRenderer> {
    text.split(';')
        .filter_map(|rule| {
            let (type_name, rest) = rule.split_once('=')?;
            let type_name = type_name.trim();
            if type_name.is_empty() {
                return None;
            }
            let (display, children) = match rest.split_once('|') {
                Some((display, children)) => (display, Some(children.trim())),
                None => (rest, None),
            };
            let display = display.trim();
            if display.is_empty() {
                return None;
            }
            Some(TypeRenderer {
                type_name: type_name.to_owned(),
                display: display.to_owned(),
                children: children
                    .filter(|children| !children.is_empty())
                    .map(str::to_owned),
                subtype_probe: None,
            })
        })
        .collect()
}

/// The source roots of the project model the run panel holds: the package of a frame of
/// the project is the path of its file below one of them.
fn project_source_roots(cx: &App) -> Vec<PathBuf> {
    let Some(runs) = cx
        .try_global::<crate::script::ActiveRunPanel>()
        .map(|active| active.0.clone())
    else {
        return Vec::new();
    };
    let Some(model) = runs.read(cx).model() else {
        return Vec::new();
    };
    model
        .modules
        .iter()
        .flat_map(|module| module.units.iter())
        .flat_map(|unit| unit.source_roots.iter().cloned())
        .collect()
}

/// The place of a frame in the list, as IDEA names it: `method:line, Class (package)`. The
/// class and the package come from the name of the frame, from its source or from both. A
/// line the adapter does not know, as a native method has, is `-1`.
fn frame_place(frame: &Frame, source_roots: &[PathBuf]) -> String {
    let (method, name_class, name_package) = name_place(&decode_uri(&frame.name));
    let source = frame
        .source
        .as_deref()
        .map(|source| source_place(source, source_roots))
        .unwrap_or_default();
    let class = name_class.or(source.class);
    let package = name_package.or(source.package);
    let line = if frame.line == 0 {
        "-1".to_owned()
    } else {
        frame.line.to_string()
    };
    match (class, package.filter(|package| !package.is_empty())) {
        (Some(class), Some(package)) => format!("{method}:{line}, {class} ({package})"),
        (Some(class), None) => format!("{method}:{line}, {class}"),
        (None, _) => format!("{method}:{line}"),
    }
}

/// The method, the class and the package of a frame name: `Class.method(arguments)` or
/// `package.Class.method`.
fn name_place(name: &str) -> (String, Option<String>, Option<String>) {
    let head = name.split_once('(').map_or(name, |(head, _)| head).trim();
    let Some((owner, method)) = head.rsplit_once('.') else {
        return (head.to_owned(), None, None);
    };
    let (owner, method) = (owner.trim(), method.trim());
    if owner.is_empty() || method.is_empty() {
        return (head.to_owned(), None, None);
    }
    match owner.rsplit_once('.') {
        Some((package, class)) if !package.is_empty() && !class.is_empty() => (
            method.to_owned(),
            Some(class.to_owned()),
            Some(package.to_owned()),
        ),
        _ => (method.to_owned(), Some(owner.to_owned()), None),
    }
}

/// The class and the package a source names, when it names them: a class of a runtime
/// image, of an archive or of a `jdt://` buffer, or a file below one of the source roots.
#[derive(Default)]
struct SourcePlace {
    class: Option<String>,
    package: Option<String>,
}

fn source_place(source: &Path, source_roots: &[PathBuf]) -> SourcePlace {
    let text = decode_uri(&source.to_string_lossy());
    if let Some(rest) = text.strip_prefix("jdt://contents/") {
        let path = rest.split(['?', '#']).next().unwrap_or(rest);
        let segments: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
        // The first segment names the container: a module of the runtime image or the
        // archive. A path of an archive may take several segments once its escapes are
        // decoded, so the package starts after its file. The rest are the package and
        // the class file.
        let skip = segments
            .iter()
            .position(|part| part.ends_with(".jar") || part.ends_with(".zip"))
            .map_or(1, |at| at + 1);
        return class_in_path(&segments, skip);
    }
    if let Some(rest) = text.strip_prefix("jrt:/") {
        let path = rest.split(['?', '#']).next().unwrap_or(rest);
        let segments: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
        let skip = if segments.first() == Some(&"modules") {
            2
        } else {
            1
        };
        return class_in_path(&segments, skip);
    }
    if let Some((_, inner)) = text.split_once('!') {
        let segments: Vec<&str> = inner.split('/').filter(|part| !part.is_empty()).collect();
        return class_in_path(&segments, 0);
    }
    let decoded = Path::new(&text);
    for root in source_roots {
        if let Some(relative) = path_below(decoded, root) {
            let text = relative.to_string_lossy().into_owned();
            let segments: Vec<&str> = text
                .split(['/', '\\'])
                .filter(|part| !part.is_empty())
                .collect();
            return class_in_path(&segments, 0);
        }
    }
    SourcePlace::default()
}

/// The class and the package of the segments after the first `skip` ones. The last segment
/// is the file of the class; the rest are its packages, which may be written with dots or
/// with separators of their own.
fn class_in_path(segments: &[&str], skip: usize) -> SourcePlace {
    let Some((class, _)) = segments.split_last() else {
        return SourcePlace::default();
    };
    if segments.len() <= skip {
        return SourcePlace::default();
    }
    let class =
        class.rsplit_once('.').map_or(
            *class,
            |(stem, extension)| {
                if extension.is_empty() { *class } else { stem }
            },
        );
    let package = segments[skip..segments.len() - 1].join(".");
    SourcePlace {
        class: (!class.is_empty()).then(|| class.to_owned()),
        package: (!package.is_empty()).then_some(package),
    }
}

/// `path` below `root`, ignoring the case of the drive and of the folders on Windows.
fn path_below(path: &Path, root: &Path) -> Option<PathBuf> {
    if let Ok(relative) = path.strip_prefix(root) {
        return Some(relative.to_path_buf());
    }
    #[cfg(windows)]
    {
        let path_text = path.to_string_lossy().replace('/', "\\");
        let mut root_text = root.to_string_lossy().replace('/', "\\");
        if !root_text.ends_with('\\') {
            root_text.push('\\');
        }
        if path_text
            .to_lowercase()
            .starts_with(&root_text.to_lowercase())
        {
            return Some(PathBuf::from(&path_text[root_text.len()..]));
        }
    }
    None
}

/// The text of a source path or a name an adapter sends in the URI form of `jrt:`:
/// `%3C` is `<`, `%28` is `(`. A malformed escape stays as it came.
fn decode_uri(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%'
            && let (Some(high), Some(low)) = (
                bytes.get(at + 1).copied().and_then(hex_digit),
                bytes.get(at + 2).copied().and_then(hex_digit),
            )
        {
            out.push(high * 16 + low);
            at += 3;
            continue;
        }
        out.push(bytes[at]);
        at += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Whether a source is a runtime image, an archive or a `jdt://` buffer: it holds classes
/// and no files of the project.
fn archive_or_image(source: &Path) -> bool {
    let path = decode_uri(&source.to_string_lossy());
    path.starts_with("jrt:/")
        || path.starts_with("jdt://")
        || path.contains(".jar!")
        || path.contains(".zip!")
        || path.ends_with(".jar")
        || path.ends_with(".zip")
}

/// Whether a frame belongs to a library rather than to the project: its source is known,
/// lies outside the project and comes from an archive of dependencies or from a runtime
/// image. A frame without a source, or one whose source is a directory beside the
/// project, stays: the adapter did not say it is a library, and hiding it would hide the
/// debuggee itself.
fn is_library_frame(frame: &Frame, root: &Path) -> bool {
    let Some(source) = &frame.source else {
        return false;
    };
    if path_under(source, root) {
        return false;
    }
    archive_or_image(source)
}

/// Whether `path` is in the directory of `root`, ignoring the case on Windows.
fn path_under(path: &Path, root: &Path) -> bool {
    #[cfg(windows)]
    {
        let path = path.to_string_lossy().replace('/', "\\").to_lowercase();
        let mut root = root.to_string_lossy().replace('/', "\\").to_lowercase();
        if !root.ends_with('\\') {
            root.push('\\');
        }
        path.starts_with(&root) || path == root.trim_end_matches('\\')
    }
    #[cfg(not(windows))]
    {
        path.starts_with(root)
    }
}

/// The name of the group of a library frame: the module of a runtime image, the artifact
/// of an archive, or the first packages of a class that no source names.
fn library_group_name(frame: &Frame) -> String {
    let Some(source) = &frame.source else {
        return String::new();
    };
    let path = decode_uri(&source.to_string_lossy());
    if let Some(rest) = path.strip_prefix("jrt:/") {
        return rest
            .split('/')
            .find(|part| !part.is_empty())
            .unwrap_or_default()
            .to_owned();
    }
    if let Some(rest) = path.strip_prefix("jdt://contents/") {
        let container = rest.split('/').next().unwrap_or_default();
        let file = Path::new(container)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        if file.ends_with(".jar") || file.ends_with(".zip") {
            let name = file.trim_end_matches(".jar").trim_end_matches(".zip");
            return artifact_name(name);
        }
        return container.to_owned();
    }
    let archive = if let Some(at) = path.find(".jar!") {
        &path[..at + 4]
    } else if path.ends_with(".jar") || path.ends_with(".zip") {
        &path[..]
    } else {
        return package_prefix(&frame.name);
    };
    let name = Path::new(archive)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let name = name
        .trim_end_matches(".jar")
        .trim_end_matches(".zip")
        .to_owned();
    artifact_name(&name)
}

/// An artifact without the version that follows the last dash before a number:
/// `spring-core-6.1.0` is `spring-core`.
fn artifact_name(name: &str) -> String {
    let Some((head, tail)) = name.rsplit_once('-') else {
        return name.to_owned();
    };
    if tail.chars().next().is_some_and(|ch| ch.is_ascii_digit()) {
        head.to_owned()
    } else {
        name.to_owned()
    }
}

/// The first two packages of a class name, `pkg.util` for `pkg.util.Items.add`.
fn package_prefix(name: &str) -> String {
    let mut parts = name.split('.');
    let first = parts.next().unwrap_or_default();
    match parts.next() {
        Some(second) => format!("{first}.{second}"),
        None => first.to_owned(),
    }
}

fn code_text(code: Option<i32>) -> String {
    code.map_or_else(|| "unknown".to_owned(), |code| code.to_string())
}

/// How many leading lines of `new` were the tail of `old`: the adapter sends the whole
/// console each time and may drop the oldest lines of it.
fn console_overlap(old: &[ConsoleLine], new: &[ConsoleLine]) -> usize {
    if old.len() <= new.len() && new[..old.len()] == *old {
        return old.len();
    }
    let most = old.len().min(new.len());
    (0..=most)
        .rev()
        .find(|&keep| old[old.len() - keep..] == new[..keep])
        .unwrap_or(0)
}

fn indent(depth: usize) -> String {
    "  ".repeat(depth)
}

fn section(title: &str, theme: &Theme) -> AnyElement {
    div()
        .px(px(SPACE_M))
        .pt(px(SPACE_S))
        .pb(px(SPACE_XS))
        .text_size(px(TEXT_SMALL))
        .text_color(theme.line_number)
        .child(title.to_owned())
        .into_any_element()
}

fn row(id: impl Into<ElementId>, text: &str, selected: bool, theme: &Theme) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .h(px(ROW_HEIGHT))
        .min_w_0()
        .px(px(ROW_PADDING_X))
        .cursor_pointer()
        .text_size(px(CODE_SIZE))
        .when(selected, |row| row.bg(theme.selection))
        .hover(|row| row.bg(theme.caret_row))
        .child(div().flex_1().min_w_0().truncate().child(text.to_owned()))
}

/// A toolbar button that shows an icon and names itself, with its key, in a tooltip.
/// A disabled button is dimmed and carries no click handler.
fn tool_button(
    id: &'static str,
    icon_name: &'static str,
    label: &'static str,
    hint: Option<&'static str>,
    enabled: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let image = if enabled {
        icon(icon_name, theme)
    } else {
        icon(icon_name, theme).grayscale(true).opacity(0.4)
    };
    let selector = format!("{id}-icon");
    let button = button_frame(id, label, hint.map(Into::into), theme)
        .debug_selector(move || id.to_owned())
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .debug_selector(move || selector.clone())
                .child(image),
        );
    if enabled { button } else { button.opacity(0.4) }
}

/// The buttons above the console: the search bar and the filter.
fn debug_console_tools(
    panel: &DebugPanel,
    theme: &Theme,
    cx: &mut Context<DebugPanel>,
) -> AnyElement {
    let filtering = panel.console_search.filter_open;
    div()
        .id("debug-console-tools")
        .debug_selector(|| "debug-console-tools".to_owned())
        .flex()
        .items_center()
        .gap(px(SPACE_XS))
        .flex_none()
        .px(px(SPACE_M))
        .pt(px(SPACE_S))
        .child(
            icon_button(
                "debug-console-find",
                "general/search",
                "Find in Console",
                Some("Ctrl+F".into()),
                theme,
            )
            .debug_selector(|| "debug-console-find".to_owned())
            .on_click(cx.listener(|this, _, window, cx| this.open_console_search(window, cx))),
        )
        .child(
            icon_button(
                "debug-console-filter",
                "general/filter",
                "Filter Lines",
                None,
                theme,
            )
            .debug_selector(|| "debug-console-filter".to_owned())
            .when(filtering, |button| button.bg(theme.selection))
            .on_click(cx.listener(|this, _, window, cx| this.toggle_console_filter(window, cx))),
        )
        .into_any_element()
}

/// The search bar of the console: the query, its options, the counter and the moves
/// between the matches.
fn debug_search_row(panel: &DebugPanel, theme: &Theme, cx: &mut Context<DebugPanel>) -> AnyElement {
    let search = &panel.console_search;
    let (status, error) = search.status();
    let case = debug_search_toggle(
        "debug-search-case",
        "general/caseSensitive",
        "Match Case",
        "Alt+C",
        search.case_sensitive,
        SearchToggle::Case,
        theme,
        cx,
    );
    let word = debug_search_toggle(
        "debug-search-word",
        "general/wholeWord",
        "Whole Words",
        "Alt+W",
        search.whole_word,
        SearchToggle::Word,
        theme,
        cx,
    );
    let regex = debug_search_toggle(
        "debug-search-regex",
        "general/regex",
        "Regex",
        "Alt+X",
        search.regex,
        SearchToggle::Regex,
        theme,
        cx,
    );
    let field = div()
        .id("debug-search-field")
        .track_focus(&panel.search_focus)
        .on_key_down(cx.listener(|this, event, window, cx| this.search_key(event, window, cx)))
        .flex()
        .items_center()
        .flex_1()
        .h(px(CONTROL_HEIGHT - SPACE_M))
        .px(px(SPACE_M))
        .rounded(px(CONTROL_RADIUS))
        .border_1()
        .border_color(theme.accent)
        .text_size(px(UI_SIZE))
        .text_color(theme.text)
        .overflow_hidden()
        .children(crate::console_search::field_parts(
            FieldMouse::new(cx, |this| Some(&mut this.console_search.field)),
            "debug-search-field",
            &search.field,
            theme,
            true,
        ))
        .when(search.field.text.is_empty(), |this| {
            this.child(div().text_color(theme.text_secondary).child("Find"))
        });
    div()
        .id("debug-console-search")
        .debug_selector(|| "debug-console-search".to_owned())
        .flex()
        .items_center()
        .flex_none()
        .h(px(CONTROL_HEIGHT))
        .mx(px(SPACE_M))
        .mb(px(SPACE_S))
        .gap(px(SPACE_S))
        .child(field)
        .child(case)
        .child(word)
        .child(regex)
        .child(
            icon_button(
                "debug-search-prev",
                "general/arrowUp",
                "Previous Match",
                Some("Shift+F3".into()),
                theme,
            )
            .on_click(cx.listener(|this, _, _, cx| this.search_step(false, cx))),
        )
        .child(
            icon_button(
                "debug-search-next",
                "general/arrowDown",
                "Next Match",
                Some("F3".into()),
                theme,
            )
            .on_click(cx.listener(|this, _, _, cx| this.search_step(true, cx))),
        )
        .child(
            div()
                .debug_selector(|| "debug-search-status".to_owned())
                .text_color(if error {
                    theme.error
                } else {
                    theme.text_secondary
                })
                .child(status),
        )
        .child(
            icon_button(
                "debug-search-close",
                "general/closeSmall",
                "Close Search",
                Some("Escape".into()),
                theme,
            )
            .on_click(cx.listener(|this, _, window, cx| this.close_console_search(window, cx))),
        )
        .into_any_element()
}

/// The field of the filter of the console: only the lines with its text stay.
fn debug_filter_row(panel: &DebugPanel, theme: &Theme, cx: &mut Context<DebugPanel>) -> AnyElement {
    div()
        .id("debug-console-filter-row")
        .debug_selector(|| "debug-console-filter-row".to_owned())
        .flex()
        .items_center()
        .flex_none()
        .h(px(CONTROL_HEIGHT))
        .mx(px(SPACE_M))
        .mb(px(SPACE_S))
        .px(px(SPACE_M))
        .rounded(px(CONTROL_RADIUS))
        .border_1()
        .border_color(theme.accent)
        .track_focus(&panel.filter_focus)
        .on_key_down(cx.listener(|this, event, window, cx| this.filter_key(event, window, cx)))
        .children(crate::console_search::field_parts(
            FieldMouse::new(cx, |this| Some(&mut this.console_search.filter)),
            "debug-filter-field",
            &panel.console_search.filter,
            theme,
            true,
        ))
        .when(panel.console_search.filter.text.is_empty(), |this| {
            this.child(div().text_color(theme.text_secondary).child("Filter lines"))
        })
        .into_any_element()
}

/// A button of the search bar of the console that turns one option on or off.
#[allow(clippy::too_many_arguments)]
fn debug_search_toggle(
    id: &'static str,
    icon_name: &'static str,
    label: &'static str,
    hint: &'static str,
    on: bool,
    toggle: SearchToggle,
    theme: &Theme,
    cx: &mut Context<DebugPanel>,
) -> Stateful<Div> {
    let button = icon_button(id, icon_name, label, Some(hint.into()), theme);
    let button = if on {
        button
            .bg(theme.tab_selected)
            .border_1()
            .border_color(theme.tab_selected_border)
    } else {
        button
    };
    button.on_click(
        cx.listener(move |this, _, window, cx| this.toggle_search_option(toggle, window, cx)),
    )
}

/// The line between two groups of toolbar buttons.
fn toolbar_divider(theme: &Theme) -> Div {
    div()
        .flex_none()
        .w(px(DIVIDER_WIDTH))
        .h(px(SPACE_XL))
        .mx(px(SPACE_XS))
        .bg(theme.border)
}

/// One of the two tabs above the body of the panel.
fn front_tab(
    id: &'static str,
    label: &'static str,
    front: Front,
    current: Front,
    theme: &Theme,
    cx: &mut Context<DebugPanel>,
) -> AnyElement {
    let selected = front == current;
    div()
        .id(id)
        .debug_selector(move || id.to_owned())
        .flex()
        .items_center()
        .flex_none()
        .h(px(24.))
        .px(px(SPACE_M))
        .rounded(px(CONTROL_RADIUS))
        .cursor_pointer()
        .occlude()
        .text_size(px(TEXT_SMALL))
        .text_color(if selected {
            theme.text
        } else {
            theme.text_secondary
        })
        .when(selected, |tab| tab.bg(theme.selection))
        .when(!selected, |tab| {
            tab.hover(move |style| style.bg(theme.hover))
        })
        .child(label)
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _, _, cx| {
                this.front = front;
                cx.notify();
            }),
        )
        .into_any_element()
}

/// The configuration the Run window selects, for the empty Debug window to name it.
fn selected_configuration(cx: &App) -> Option<String> {
    let panel = cx.try_global::<crate::script::ActiveRunPanel>()?.0.clone();
    let state = panel.read(cx).state();
    state
        .selected
        .and_then(|index| state.configs.get(index))
        .map(|name| name.to_string())
}

/// One gray line in the middle of a part that has nothing to show.
fn empty_pane(id: &'static str, text: &'static str, theme: &Theme) -> AnyElement {
    div()
        .debug_selector(move || id.to_owned())
        .flex()
        .flex_1()
        .min_h_0()
        .items_center()
        .justify_center()
        .text_size(px(TEXT_SMALL))
        .text_color(theme.text_secondary)
        .child(text)
        .into_any_element()
}

/// The same line with the height of a row, for a small part.
fn empty_line(id: &'static str, text: &'static str, theme: &Theme) -> AnyElement {
    div()
        .debug_selector(move || id.to_owned())
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .h(px(ROW_HEIGHT))
        .px(px(ROW_PADDING_X))
        .text_size(px(TEXT_SMALL))
        .text_color(theme.text_secondary)
        .child(text)
        .into_any_element()
}

/// A row of the thread drop-down that selects the thread of the stack.
fn thread_entry(panel: WeakEntity<DebugPanel>, thread: &Thread) -> MenuEntry {
    let id = thread.id;
    let label = format!("{}  #{}", thread.name, thread.id);
    MenuEntry::new(label, move |_, cx| {
        panel
            .update(cx, |panel, cx| {
                panel.send(Command::SelectThread(id), cx);
                cx.notify();
            })
            .ok();
    })
}

impl Render for DebugPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let (active, stopped) = self.state();
        // Stop ends a build too, so it is at hand while the build runs.
        let stoppable = active || self.build.is_some();
        let running = matches!(self.snapshot.state, DebugState::Running);
        let can_drop = self.restart_frame_enabled();
        let can_restart = self.can_rerun();
        let workspace = self.workspace.clone();
        let options =
            crate::tool_menu::options_menu(workspace.clone(), workspace::DEBUG_TOOL, |_| {
                Vec::new()
            });
        let hide = move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
            workspace
                .update(cx, |workspace, cx| {
                    workspace.toggle_tool(DEBUG_TOOL, window, cx)
                })
                .ok();
        };
        let toolbar = div()
            .flex()
            .items_center()
            .gap(px(SPACE_XS))
            .h(px(CONTROL_HEIGHT))
            .px(px(SPACE_M))
            .debug_selector(|| "debug-toolbar".to_owned())
            .child(
                tool_button(
                    "debug-rerun",
                    "run/restart",
                    "Rerun",
                    Some("Ctrl+F5"),
                    can_restart,
                    &theme,
                )
                .when(can_restart, |button| {
                    button.on_click(cx.listener(|this, _, _, cx| this.rerun(cx)))
                }),
            )
            .child(
                tool_button(
                    "debug-continue",
                    "run/run",
                    "Resume",
                    Some("F9"),
                    stopped,
                    &theme,
                )
                .when(stopped, |button| {
                    button.on_click(cx.listener(|this, _, _, cx| this.resume(cx)))
                }),
            )
            .child(
                tool_button("debug-pause", "debug/pause", "Pause", None, running, &theme)
                    .when(running, |button| {
                        button.on_click(cx.listener(|this, _, _, cx| this.pause(cx)))
                    }),
            )
            .child(
                tool_button(
                    "debug-stop",
                    "run/stop",
                    "Stop",
                    Some("Ctrl+F2"),
                    stoppable,
                    &theme,
                )
                .when(stoppable, |button| {
                    button.on_click(cx.listener(|this, _, _, cx| this.stop(cx)))
                }),
            )
            .child(toolbar_divider(&theme))
            .child(
                tool_button(
                    "debug-over",
                    "debug/stepOver",
                    "Step Over",
                    Some("F8"),
                    stopped,
                    &theme,
                )
                .when(stopped, |button| {
                    button.on_click(cx.listener(|this, _, _, cx| this.step_over(cx)))
                }),
            )
            .child(
                tool_button(
                    "debug-into",
                    "debug/stepInto",
                    "Step Into",
                    Some("F7"),
                    stopped,
                    &theme,
                )
                .when(stopped, |button| {
                    button.on_click(cx.listener(|this, _, _, cx| this.step_into(cx)))
                }),
            )
            .child(
                tool_button(
                    "debug-out",
                    "debug/stepOut",
                    "Step Out",
                    Some("Shift+F8"),
                    stopped,
                    &theme,
                )
                .when(stopped, |button| {
                    button.on_click(cx.listener(|this, _, _, cx| this.step_out(cx)))
                }),
            )
            .child(
                tool_button(
                    "debug-run-to-cursor",
                    "debug/runToCursor",
                    "Run to Cursor",
                    Some("Alt+F9"),
                    stopped,
                    &theme,
                )
                .when(stopped, |button| {
                    button.on_click(cx.listener(|this, _, _, cx| this.run_to_cursor(false, cx)))
                }),
            )
            .child(
                tool_button(
                    "debug-drop-frame",
                    "debug/dropFrame",
                    "Drop Frame",
                    None,
                    can_drop,
                    &theme,
                )
                .when(can_drop, |button| {
                    button.on_click(cx.listener(|this, _, _, cx| this.restart_frame(cx)))
                }),
            )
            .child(toolbar_divider(&theme))
            .child(
                tool_button(
                    "debug-evaluate",
                    "debug/evaluate",
                    "Evaluate Expression",
                    Some("Alt+F8"),
                    stopped,
                    &theme,
                )
                .when(stopped, |button| {
                    button
                        .on_click(cx.listener(|this, _, window, cx| this.ask_evaluate(window, cx)))
                }),
            )
            .child(
                tool_button(
                    "debug-breakpoints",
                    "debug/breakpoints",
                    "View Breakpoints",
                    Some("Ctrl+Shift+F8"),
                    true,
                    &theme,
                )
                .on_click(cx.listener(|this, _, window, cx| this.show_breakpoints(window, cx))),
            )
            .child(
                tool_button(
                    "debug-mute",
                    "debug/breakpointMuted",
                    "Mute Breakpoints",
                    None,
                    true,
                    &theme,
                )
                .when(self.muted, |button| button.bg(theme.selection))
                .on_click(cx.listener(|this, _, _, cx| this.toggle_mute(cx))),
            )
            .child(
                tool_button(
                    "debug-heap-dump",
                    "general/logs",
                    "Capture Memory Snapshot",
                    None,
                    active,
                    &theme,
                )
                .when(active, |button| {
                    button.on_click(cx.listener(|this, _, window, cx| {
                        let workspace = this.workspace.clone();
                        let pid = this.snapshot.process_id;
                        workspace
                            .update(cx, |workspace, cx| {
                                crate::heap_dump::capture_pid(workspace, pid, window, cx)
                            })
                            .ok();
                    }))
                }),
            );
        let state = self.state_text().map(str::to_owned);
        let toolbar = match state {
            Some(state) => toolbar.child(div().flex_1()).child(
                div()
                    .px(px(SPACE_S))
                    .text_size(px(TEXT_SMALL))
                    .text_color(theme.text_secondary)
                    .child(state),
            ),
            None => toolbar,
        };
        let body = div()
            .flex()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .child(match self.front {
                Front::Debugger => self.render_debugger(&theme, window, cx),
                Front::Console => self.render_console(&theme, window, cx),
            });
        island(&theme)
            .id("debug-panel")
            .key_context("DebugPanel ToolWindow")
            .track_focus(&self.focus)
            .group(GROUP)
            .h_full()
            .on_click(cx.listener(|this, _, window, cx| window.focus(&this.focus, cx)))
            .on_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)))
            .on_action(cx.listener(|this, _: &NewWatch, _, cx| this.start_watch(cx)))
            .on_action(cx.listener(|this, _: &DeleteWatch, _, cx| this.delete_watch(cx)))
            .on_action(cx.listener(|this, _: &SetValue, _, cx| this.start_value_edit(cx)))
            .on_action(
                cx.listener(|this, _: &MarkObject, window, cx| {
                    this.mark_selected_object(window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &CancelInput, window, cx| {
                this.key(
                    &KeyDownEvent {
                        keystroke: Keystroke {
                            modifiers: Modifiers::default(),
                            key: "escape".into(),
                            key_char: None,
                        },
                        is_held: false,
                        prefer_character_input: false,
                    },
                    window,
                    cx,
                )
            }))
            .child(
                panel_header(self.title.clone(), GROUP, "debug", options, hide, &theme)
                    .debug_selector(|| "debug-header".to_owned()),
            )
            .child(toolbar)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(SPACE_XS))
                    .flex_none()
                    .h(px(CONTROL_HEIGHT))
                    .px(px(SPACE_M))
                    .debug_selector(|| "debug-tabs".to_owned())
                    .child(front_tab(
                        "debug-tab-debugger",
                        "Debugger",
                        Front::Debugger,
                        self.front,
                        &theme,
                        cx,
                    ))
                    .child(front_tab(
                        "debug-tab-console",
                        "Console",
                        Front::Console,
                        self.front,
                        &theme,
                        cx,
                    )),
            )
            .child(body)
            .into_any_element()
    }
}

/// The list of Ctrl+Shift+F8: breakpoint properties and the exception filters.
/// The popup of Ctrl+Shift+F8 on a line with a breakpoint: its condition, whether it is
/// on, whether it suspends the program, and a way to the whole list.
struct BreakpointPopup {
    panel: Entity<DebugPanel>,
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    file: PathBuf,
    line: u32,
}

impl BreakpointPopup {
    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace
            .update(cx, |workspace, cx| workspace.dismiss(window, cx))
            .ok();
    }

    fn row(id: &'static str, label: String, theme: &Theme) -> Stateful<Div> {
        div()
            .id(id)
            .debug_selector(move || id.to_owned())
            .flex()
            .items_center()
            .gap(px(SPACE_S))
            .px(px(SPACE_M))
            .h(px(ROW_HEIGHT))
            .cursor_pointer()
            .text_color(theme.text)
            .hover({
                let hover = theme.hover;
                move |style| style.bg(hover)
            })
            .child(label)
    }
}

impl Render for BreakpointPopup {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let setting = self.panel.read(cx).setting(&self.file, self.line).cloned();
        let (condition, enabled, suspend, hit_message, log_expression) = match &setting {
            Some(setting) => (
                setting.condition.clone().unwrap_or_default(),
                setting.enabled,
                setting.suspend,
                setting.hit_message,
                setting
                    .log_expression
                    .clone()
                    .or_else(|| setting.log_message.clone())
                    .unwrap_or_default(),
            ),
            None => (String::new(), false, false, false, String::new()),
        };
        let name = self
            .file
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let line = self.line;
        let panel = self.panel.clone();
        let condition_file = self.file.clone();
        let enabled_file = self.file.clone();
        let suspend_file = self.file.clone();
        let message_file = self.file.clone();
        let log_file = self.file.clone();
        let condition_text = if condition.is_empty() {
            "Condition: <none>".to_owned()
        } else {
            format!("Condition: {condition}")
        };
        let log_text = if log_expression.is_empty() {
            "Evaluate and log: <none>".to_owned()
        } else {
            format!("Evaluate and log: {log_expression}")
        };
        let title = format!("Breakpoint at {name}:{line}");
        let shell = surface(&theme)
            .id("breakpoint-popup")
            .debug_selector(|| "breakpoint-popup".to_owned())
            .track_focus(&self.focus);
        let contents = div()
            .flex()
            .flex_col()
            .pb(px(SPACE_S))
            .child(
                Self::row("breakpoint-popup-condition", condition_text, &theme).on_click(
                    cx.listener(move |this, _, window, cx| {
                        this.panel.update(cx, |panel, cx| {
                            panel.ask_condition(condition_file.clone(), line, window, cx)
                        });
                        cx.notify();
                    }),
                ),
            )
            .child(
                Self::row(
                    "breakpoint-popup-enabled",
                    format!("Enabled: {}", if enabled { "on" } else { "off" }),
                    &theme,
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.panel.update(cx, |panel, cx| {
                        panel.set_enabled(enabled_file.clone(), line, !enabled, cx)
                    });
                    this.close(window, cx);
                })),
            )
            .child(
                Self::row(
                    "breakpoint-popup-suspend",
                    format!("{} Suspend", if suspend { "☑" } else { "☐" }),
                    &theme,
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.panel.update(cx, |panel, cx| {
                        panel.set_suspend(suspend_file.clone(), line, !suspend, cx)
                    });
                    this.close(window, cx);
                })),
            )
            .child(
                Self::row(
                    "breakpoint-popup-hit-message",
                    format!(
                        "{} Breakpoint hit message",
                        if hit_message { "☑" } else { "☐" }
                    ),
                    &theme,
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.panel.update(cx, |panel, cx| {
                        panel.set_hit_message(message_file.clone(), line, !hit_message, cx)
                    });
                    this.close(window, cx);
                })),
            )
            .child(
                Self::row("breakpoint-popup-log-expression", log_text, &theme).on_click(
                    cx.listener(move |this, _, window, cx| {
                        this.panel.update(cx, |panel, cx| {
                            panel.ask_log_expression(log_file.clone(), line, window, cx)
                        });
                        cx.notify();
                    }),
                ),
            )
            .child(
                Self::row("breakpoint-popup-more", "More".to_owned(), &theme).on_click(
                    cx.listener(move |this, _, window, cx| {
                        this.close(window, cx);
                        panel.update(cx, |panel, cx| panel.show_breakpoints(window, cx));
                    }),
                ),
            );
        let viewport = window.viewport_size();
        let width = px(420.);
        let left = (viewport.width - width) / 2.;
        let top = viewport.height * 0.2;
        let close = cx.listener(|this, _: &ClickEvent, window, cx| this.close(window, cx));
        dialog_window(
            DialogSpec {
                key: "breakpoint-popup",
                title: title.into(),
                extra: None,
                place: DialogPlace {
                    left: f32::from(left),
                    top: f32::from(top),
                    width: f32::from(width),
                    height: None,
                },
                min: (320., 120.),
            },
            shell,
            window,
            cx,
            close,
            contents,
        )
    }
}

/// A row of the tree of the Breakpoints window.
#[derive(Clone, Debug, PartialEq, Eq)]
enum BreakpointRow {
    /// A breakpoint of a file and a line.
    Line { file: PathBuf, line: u32 },
    /// A method breakpoint, by its place in the list of the panel.
    Function(usize),
    /// A field watchpoint, by the id the adapter gave the field.
    Data(String),
    /// The point of any exception, which the window always lists.
    AnyException,
    /// A named exception breakpoint, by its place in the list of the panel.
    Exception(usize),
    /// A filter of the adapter.
    Filter(String),
}

/// A group of the tree of the Breakpoints window.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum BreakpointGroup {
    Line,
    Function,
    Data,
    Exception,
}

/// A property of the selected row that is typed in a field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PropertyField {
    Condition,
    LogExpression,
    HitCondition,
}

/// One property the user changed on the selected row.
#[derive(Clone, Debug)]
enum RowEdit {
    Enabled(bool),
    Suspend(bool),
    SuspendThread(bool),
    Condition(String),
    HitMessage(bool),
    LogExpression(String),
    RemoveOnce(bool),
    HitCondition(String),
    Caught(bool),
    Uncaught(bool),
}

/// The properties of a row, as the window draws them.
#[derive(Default)]
struct RowValues {
    enabled: bool,
    suspend: bool,
    suspend_thread: bool,
    hit_message: bool,
    remove_once: bool,
    caught: bool,
    uncaught: bool,
    condition: String,
    log_expression: String,
    hit_condition: String,
}

fn row_values(panel: &DebugPanel, row: &BreakpointRow) -> RowValues {
    let line = |setting: &BreakpointSetting| RowValues {
        enabled: setting.enabled,
        suspend: setting.suspend,
        suspend_thread: setting.suspend_thread,
        hit_message: setting.hit_message,
        remove_once: setting.remove_once,
        condition: setting.condition.clone().unwrap_or_default(),
        log_expression: setting
            .log_expression
            .clone()
            .or_else(|| setting.log_message.clone())
            .unwrap_or_default(),
        hit_condition: setting.hit_condition.clone().unwrap_or_default(),
        ..RowValues::default()
    };
    match row {
        BreakpointRow::Line { file, line: number } => panel
            .breakpoints
            .iter()
            .find(|setting| &setting.file == file && setting.line == *number)
            .map(line)
            .unwrap_or_default(),
        BreakpointRow::Function(index) => panel
            .function_breakpoints
            .get(*index)
            .map(|setting| RowValues {
                enabled: setting.enabled,
                condition: setting.condition.clone().unwrap_or_default(),
                hit_condition: setting.hit_condition.clone().unwrap_or_default(),
                ..RowValues::default()
            })
            .unwrap_or_default(),
        BreakpointRow::Data(data_id) => panel
            .data_breakpoints
            .iter()
            .find(|setting| &setting.data_id == data_id)
            .map(|setting| RowValues {
                enabled: setting.enabled,
                condition: setting.condition.clone().unwrap_or_default(),
                hit_condition: setting.hit_condition.clone().unwrap_or_default(),
                ..RowValues::default()
            })
            .unwrap_or_default(),
        BreakpointRow::AnyException => RowValues {
            enabled: panel.any_exception.enabled,
            caught: panel.any_exception.caught,
            uncaught: panel.any_exception.uncaught,
            ..RowValues::default()
        },
        BreakpointRow::Exception(index) => panel
            .exception_points
            .get(*index)
            .map(|point| RowValues {
                enabled: point.setting.enabled,
                caught: point.setting.caught,
                uncaught: point.setting.uncaught,
                ..RowValues::default()
            })
            .unwrap_or_default(),
        BreakpointRow::Filter(filter) => RowValues {
            enabled: panel.enabled_filters.contains(filter),
            ..RowValues::default()
        },
    }
}

fn row_exists(panel: &DebugPanel, row: &BreakpointRow) -> bool {
    match row {
        BreakpointRow::Line { file, line } => panel
            .breakpoints
            .iter()
            .any(|setting| &setting.file == file && setting.line == *line),
        BreakpointRow::Function(index) => *index < panel.function_breakpoints.len(),
        BreakpointRow::Data(data_id) => panel
            .data_breakpoints
            .iter()
            .any(|setting| &setting.data_id == data_id),
        BreakpointRow::AnyException => true,
        BreakpointRow::Exception(index) => *index < panel.exception_points.len(),
        BreakpointRow::Filter(filter) => {
            panel.enabled_filters.contains(filter)
                || panel
                    .snapshot
                    .exception_filters
                    .iter()
                    .any(|item| &item.filter == filter)
        }
    }
}

fn initial_breakpoint_row(panel: &DebugPanel) -> BreakpointRow {
    panel
        .breakpoints
        .first()
        .map(|setting| BreakpointRow::Line {
            file: setting.file.clone(),
            line: setting.line,
        })
        .unwrap_or(BreakpointRow::AnyException)
}

struct BreakpointsView {
    panel: WeakEntity<DebugPanel>,
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    scroll_tree: ScrollHandle,
    scroll_properties: ScrollHandle,
    selected: Option<BreakpointRow>,
    /// The row whose values the fields hold, so a repaint of the same row keeps them.
    loaded: Option<BreakpointRow>,
    condition: TextField,
    log_expression: TextField,
    hit_condition: TextField,
    focus_field: Option<PropertyField>,
    add_menu: bool,
    collapsed: BTreeSet<BreakpointGroup>,
    _panel: Option<Subscription>,
}

impl BreakpointsView {
    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace
            .update(cx, |workspace, cx| workspace.dismiss(window, cx))
            .ok();
    }

    fn edit(&mut self, row: BreakpointRow, edit: RowEdit, cx: &mut Context<Self>) {
        if let Some(panel) = self.panel.upgrade() {
            panel.update(cx, |panel, cx| panel.edit_breakpoint_row(&row, edit, cx));
        }
        cx.notify();
    }

    fn load_fields(&mut self, cx: &mut Context<Self>) {
        self.focus_field = None;
        let Some(row) = self.selected.clone() else {
            self.loaded = None;
            return;
        };
        let values = self
            .panel
            .upgrade()
            .map(|panel| row_values(panel.read(cx), &row))
            .unwrap_or_default();
        self.condition = TextField::from_text(values.condition);
        self.log_expression = TextField::from_text(values.log_expression);
        self.hit_condition = TextField::from_text(values.hit_condition);
        self.loaded = Some(row);
    }

    fn commit_field(&mut self, field: PropertyField, cx: &mut Context<Self>) {
        let Some(row) = self.selected.clone() else {
            return;
        };
        let edit = match field {
            PropertyField::Condition => RowEdit::Condition(self.condition.text.clone()),
            PropertyField::LogExpression => {
                RowEdit::LogExpression(self.log_expression.text.clone())
            }
            PropertyField::HitCondition => RowEdit::HitCondition(self.hit_condition.text.clone()),
        };
        self.edit(row, edit, cx);
    }

    /// The keys of the window: Escape closes it, and a property field takes the typing and
    /// writes it into the point at once.
    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key == "escape" {
            self.close(window, cx);
            cx.stop_propagation();
            return;
        }
        if let Some(field) = self.focus_field {
            let changed = match field {
                PropertyField::Condition => self.condition.key_with_clipboard(event, false, cx),
                PropertyField::LogExpression => {
                    self.log_expression.key_with_clipboard(event, false, cx)
                }
                PropertyField::HitCondition => {
                    self.hit_condition.key_with_clipboard(event, false, cx)
                }
            };
            if changed {
                self.commit_field(field, cx);
                cx.stop_propagation();
                cx.notify();
                return;
            }
            if event.keystroke.key == "enter" {
                self.commit_field(field, cx);
                cx.stop_propagation();
                return;
            }
            return;
        }
        if event.keystroke.key == "delete" {
            self.remove_selected(cx);
            cx.stop_propagation();
        }
    }

    fn remove_selected(&mut self, cx: &mut Context<Self>) {
        let Some(row) = self.selected.take() else {
            return;
        };
        if let Some(panel) = self.panel.upgrade() {
            panel.update(cx, |panel, cx| panel.remove_breakpoint_row(&row, cx));
        }
        self.loaded = None;
        cx.notify();
    }

    /// Opens the file of a line breakpoint at its line and closes the window.
    fn open_line(&mut self, file: PathBuf, line: u32, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace
            .update(cx, |workspace, cx| {
                navigation::go_to(
                    workspace,
                    file,
                    Some(editor_view::Spot::Line {
                        line: line.saturating_sub(1) as usize,
                        column: 0,
                    }),
                    true,
                    window,
                    cx,
                );
                workspace.dismiss(window, cx);
            })
            .ok();
    }

    /// Asks for a name and adds the point the menu named.
    fn ask_add(&mut self, kind: BreakpointGroup, window: &mut Window, cx: &mut Context<Self>) {
        let panel = self.panel.clone();
        self.add_menu = false;
        match kind {
            BreakpointGroup::Exception => self.workspace.update(cx, |workspace, cx| {
                Prompt::show_with(
                    "Exception Breakpoint",
                    "",
                    move |text, cx| {
                        panel
                            .update(cx, |panel, cx| {
                                panel.add_named_exception_breakpoint(text, cx)
                            })
                            .ok();
                    },
                    workspace,
                    window,
                    cx,
                );
            }),
            BreakpointGroup::Function => self.workspace.update(cx, |workspace, cx| {
                Prompt::show_with(
                    "Method Breakpoint",
                    "",
                    move |text, cx| {
                        panel
                            .update(cx, |panel, cx| {
                                panel.add_named_function_breakpoint(text, cx)
                            })
                            .ok();
                    },
                    workspace,
                    window,
                    cx,
                );
            }),
            BreakpointGroup::Data => self.workspace.update(cx, |workspace, cx| {
                Prompt::show_with(
                    "Field Watchpoint",
                    "",
                    move |text, cx| {
                        panel
                            .update(cx, |panel, cx| panel.add_named_data_breakpoint(text, cx))
                            .ok();
                    },
                    workspace,
                    window,
                    cx,
                );
            }),
            BreakpointGroup::Line => return,
        }
        .ok();
    }

    /// A row of a group: a flag that turns the group on or off, a chevron that folds it,
    /// the icon of the kind and its name.
    #[allow(clippy::too_many_arguments)]
    fn group_row(
        &self,
        id: &'static str,
        icon_name: &'static str,
        label: &str,
        on: bool,
        open: bool,
        group: BreakpointGroup,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let panel = self.panel.clone();
        let flag = format!("{id}-flag");
        let click_flag = flag.clone();
        div()
            .id(SharedString::from(id))
            .debug_selector(move || id.to_owned())
            .flex()
            .items_center()
            .gap(px(SPACE_S))
            .h(px(ROW_HEIGHT))
            .pl(px(SPACE_M))
            .pr(px(SPACE_S))
            .rounded(px(ROW_RADIUS))
            .child(
                div()
                    .id(SharedString::from(flag.clone()))
                    .debug_selector(move || click_flag.clone())
                    .flex_none()
                    .cursor_pointer()
                    .child(checkbox(on, theme))
                    .on_click(cx.listener(move |_this, _, _, cx| {
                        let on = panel
                            .read_with(cx, |panel, _| panel.group_enabled(group))
                            .unwrap_or(false);
                        if let Some(panel) = panel.upgrade() {
                            panel.update(cx, |panel, cx| panel.set_group_enabled(group, !on, cx));
                        }
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .id(SharedString::from(format!("{id}-name")))
                    .flex()
                    .items_center()
                    .gap(px(SPACE_S))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.collapsed.remove(&group) {
                            this.collapsed.insert(group);
                        }
                        cx.notify();
                    }))
                    .child(icon_sized(
                        if open {
                            "general/chevronDown"
                        } else {
                            "general/chevronRight"
                        },
                        TEXT_SMALL,
                        theme,
                    ))
                    .child(icon_sized(icon_name, 14., theme))
                    .child(div().text_color(theme.text).child(label.to_owned())),
            )
            .into_any_element()
    }

    /// A checkbox of the tree, on its own, with the row it belongs to.
    fn tree_flag(
        &self,
        id: String,
        on: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
        handler: impl Fn(&mut Self, &mut Context<Self>) + 'static,
    ) -> AnyElement {
        let selector = id.clone();
        div()
            .id(SharedString::from(id))
            .debug_selector(move || selector.clone())
            .flex_none()
            .cursor_pointer()
            .child(checkbox(on, theme))
            .on_click(cx.listener(move |this, _, _, cx| handler(this, cx)))
            .into_any_element()
    }

    /// One property with a field for a line of text.
    fn property_field(
        &self,
        which: PropertyField,
        id: &'static str,
        value: &TextField,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mouse = FieldMouse::new(cx, move |this| {
            Some(match which {
                PropertyField::Condition => &mut this.condition,
                PropertyField::LogExpression => &mut this.log_expression,
                PropertyField::HitCondition => &mut this.hit_condition,
            })
        })
        .starting(cx, move |this, cx| {
            if this.focus_field == Some(which) {
                return false;
            }
            this.focus_field = Some(which);
            cx.notify();
            true
        });
        let text = value.text.clone();
        div()
            .id(SharedString::from(id))
            .debug_selector(move || id.to_owned())
            .flex()
            .items_center()
            .flex_1()
            .min_w_0()
            .h(px(CONTROL_HEIGHT))
            .px(px(SPACE_M))
            .gap(px(SPACE_XS))
            .rounded(px(CONTROL_RADIUS))
            .border_1()
            .border_color(if self.focus_field == Some(which) {
                theme.accent
            } else {
                theme.control_border
            })
            .bg(theme.popup_bg)
            .text_size(px(CODE_SIZE))
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .debug_selector(move || format!("breakpoint-condition-value-{text}")),
            )
            .children(text_field_children(
                mouse,
                value,
                theme.text,
                theme.text,
                theme.selection,
                theme.caret,
                CODE_SIZE,
                self.focus_field == Some(which),
            ))
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn property_check(
        &self,
        id: &'static str,
        on: bool,
        row: BreakpointRow,
        edit: impl Fn(bool) -> RowEdit + 'static,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .id(SharedString::from(id))
            .debug_selector(move || id.to_owned())
            .flex()
            .items_center()
            .cursor_pointer()
            .child(checkbox(on, theme))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.edit(row.clone(), edit(!on), cx);
            }))
            .into_any_element()
    }

    fn property_row(label: &str, control: AnyElement, theme: &Theme) -> AnyElement {
        div()
            .flex()
            .items_center()
            .gap(px(SPACE_L))
            .min_h(px(CONTROL_HEIGHT))
            .child(
                div()
                    .w(px(150.))
                    .flex_none()
                    .text_color(theme.text_secondary)
                    .child(label.to_owned()),
            )
            .child(div().flex().flex_1().min_w_0().child(control))
            .into_any_element()
    }

    /// The properties of the selected row on the right of the tree.
    fn properties(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let Some(row) = self.selected.clone() else {
            return div().into_any_element();
        };
        let Some(panel) = self.panel.upgrade() else {
            return div().into_any_element();
        };
        let values = row_values(panel.read(cx), &row);
        let pass_count = panel.read_with(cx, |panel, _| {
            panel.debugger.is_none() || panel.snapshot.capabilities.hit_condition
        });
        let mut children: Vec<AnyElement> = Vec::new();
        match &row {
            BreakpointRow::Line { .. } => {
                children.push(Self::property_row(
                    "Enabled",
                    self.property_check(
                        "breakpoint-dialog-enabled",
                        values.enabled,
                        row.clone(),
                        RowEdit::Enabled,
                        theme,
                        cx,
                    ),
                    theme,
                ));
                children.push(Self::property_row(
                    "Suspend",
                    div()
                        .flex()
                        .items_center()
                        .gap(px(SPACE_M))
                        .child(self.property_check(
                            "breakpoint-dialog-suspend",
                            values.suspend,
                            row.clone(),
                            RowEdit::Suspend,
                            theme,
                            cx,
                        ))
                        .child(Self::radio(
                            "breakpoint-dialog-suspend-all",
                            "All",
                            !values.suspend_thread,
                            values.suspend,
                            theme,
                            cx.listener({
                                let row = row.clone();
                                move |this, _, _, cx| {
                                    this.edit(row.clone(), RowEdit::SuspendThread(false), cx);
                                }
                            }),
                        ))
                        .child(Self::radio(
                            "breakpoint-dialog-suspend-thread",
                            "Thread",
                            values.suspend_thread,
                            values.suspend,
                            theme,
                            cx.listener({
                                let row = row.clone();
                                move |this, _, _, cx| {
                                    this.edit(row.clone(), RowEdit::SuspendThread(true), cx);
                                }
                            }),
                        ))
                        .into_any_element(),
                    theme,
                ));
                children.push(Self::property_row(
                    "Condition",
                    self.property_field(
                        PropertyField::Condition,
                        "breakpoint-dialog-condition",
                        &self.condition,
                        theme,
                        cx,
                    ),
                    theme,
                ));
                children.push(Self::property_row(
                    "Log message to console",
                    self.property_check(
                        "breakpoint-dialog-hit-message",
                        values.hit_message,
                        row.clone(),
                        RowEdit::HitMessage,
                        theme,
                        cx,
                    ),
                    theme,
                ));
                children.push(Self::property_row(
                    "Evaluate and log",
                    self.property_field(
                        PropertyField::LogExpression,
                        "breakpoint-dialog-log-expression",
                        &self.log_expression,
                        theme,
                        cx,
                    ),
                    theme,
                ));
                children.push(Self::property_row(
                    "Remove once hit",
                    self.property_check(
                        "breakpoint-dialog-temporary",
                        values.remove_once,
                        row.clone(),
                        RowEdit::RemoveOnce,
                        theme,
                        cx,
                    ),
                    theme,
                ));
                if pass_count {
                    children.push(Self::property_row(
                        "Pass count",
                        self.property_field(
                            PropertyField::HitCondition,
                            "breakpoint-dialog-hit-condition",
                            &self.hit_condition,
                            theme,
                            cx,
                        ),
                        theme,
                    ));
                }
            }
            BreakpointRow::Function(_) | BreakpointRow::Data(_) => {
                children.push(Self::property_row(
                    "Enabled",
                    self.property_check(
                        "breakpoint-dialog-enabled",
                        values.enabled,
                        row.clone(),
                        RowEdit::Enabled,
                        theme,
                        cx,
                    ),
                    theme,
                ));
                children.push(Self::property_row(
                    "Condition",
                    self.property_field(
                        PropertyField::Condition,
                        "breakpoint-dialog-condition",
                        &self.condition,
                        theme,
                        cx,
                    ),
                    theme,
                ));
                if pass_count {
                    children.push(Self::property_row(
                        "Pass count",
                        self.property_field(
                            PropertyField::HitCondition,
                            "breakpoint-dialog-hit-condition",
                            &self.hit_condition,
                            theme,
                            cx,
                        ),
                        theme,
                    ));
                }
            }
            BreakpointRow::AnyException | BreakpointRow::Exception(_) => {
                children.push(Self::property_row(
                    "Enabled",
                    self.property_check(
                        "breakpoint-dialog-enabled",
                        values.enabled,
                        row.clone(),
                        RowEdit::Enabled,
                        theme,
                        cx,
                    ),
                    theme,
                ));
                children.push(Self::property_row(
                    "Caught",
                    self.property_check(
                        "breakpoint-dialog-caught",
                        values.caught,
                        row.clone(),
                        RowEdit::Caught,
                        theme,
                        cx,
                    ),
                    theme,
                ));
                children.push(Self::property_row(
                    "Uncaught",
                    self.property_check(
                        "breakpoint-dialog-uncaught",
                        values.uncaught,
                        row.clone(),
                        RowEdit::Uncaught,
                        theme,
                        cx,
                    ),
                    theme,
                ));
            }
            BreakpointRow::Filter(_) => {
                children.push(Self::property_row(
                    "Enabled",
                    self.property_check(
                        "breakpoint-dialog-enabled",
                        values.enabled,
                        row.clone(),
                        RowEdit::Enabled,
                        theme,
                        cx,
                    ),
                    theme,
                ));
            }
        }
        div()
            .flex()
            .flex_col()
            .gap(px(SPACE_S))
            .p(px(DIALOG_PADDING))
            .children(children)
            .into_any_element()
    }

    fn radio(
        id: &'static str,
        label: &str,
        chosen: bool,
        enabled: bool,
        theme: &Theme,
        click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> AnyElement {
        div()
            .id(SharedString::from(id))
            .debug_selector(move || id.to_owned())
            .flex()
            .items_center()
            .gap(px(SPACE_S))
            .cursor_pointer()
            .when(!enabled, |row| row.opacity(0.4))
            .child(
                div()
                    .text_color(if chosen {
                        theme.accent
                    } else {
                        theme.text_secondary
                    })
                    .child(if chosen { "●" } else { "○" }),
            )
            .child(div().text_color(theme.text).child(label.to_owned()))
            .on_click(click)
            .into_any_element()
    }

    /// The tree of the left column: the groups with their points.
    fn tree(&self, theme: &Theme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(panel) = self.panel.upgrade() else {
            return Vec::new();
        };
        let (
            breakpoints,
            function_breakpoints,
            data_breakpoints,
            any_exception,
            exception_points,
            exception_filters,
            enabled_filters,
            line_on,
            function_on,
            data_on,
            exception_on,
        ) = {
            let panel = panel.read(cx);
            (
                panel.breakpoints.clone(),
                panel.function_breakpoints.clone(),
                panel.data_breakpoints.clone(),
                panel.any_exception.clone(),
                panel.exception_points.clone(),
                panel.snapshot.exception_filters.clone(),
                panel.enabled_filters.clone(),
                panel.group_enabled(BreakpointGroup::Line),
                panel.group_enabled(BreakpointGroup::Function),
                panel.group_enabled(BreakpointGroup::Data),
                panel.group_enabled(BreakpointGroup::Exception),
            )
        };
        let mut rows: Vec<AnyElement> = Vec::new();
        let line_open = !self.collapsed.contains(&BreakpointGroup::Line);
        if !breakpoints.is_empty() {
            rows.push(self.group_row(
                "breakpoints-group-line",
                "debug/breakpoints",
                "Line Breakpoints",
                line_on,
                line_open,
                BreakpointGroup::Line,
                theme,
                cx,
            ));
            if line_open {
                for (index, setting) in breakpoints.iter().enumerate() {
                    let row = BreakpointRow::Line {
                        file: setting.file.clone(),
                        line: setting.line,
                    };
                    let selected = self.selected.as_ref() == Some(&row);
                    let on = setting.enabled;
                    let name = setting
                        .file
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let label = if setting.owner == BreakpointOwner::Agent {
                        format!("{name}:{} (agent)", setting.line)
                    } else {
                        format!("{name}:{}", setting.line)
                    };
                    let file = setting.file.clone();
                    let line = setting.line;
                    let click_file = file.clone();
                    let selector = format!("breakpoints-line-row-{index}");
                    rows.push(
                        div()
                            .id(SharedString::from(format!("breakpoints-line-{index}")))
                            .debug_selector(move || selector.clone())
                            .flex()
                            .items_center()
                            .gap(px(SPACE_S))
                            .h(px(ROW_HEIGHT))
                            .pl(px(SPACE_XL))
                            .pr(px(SPACE_S))
                            .rounded(px(ROW_RADIUS))
                            .cursor_pointer()
                            .when(selected, |row| row.bg(theme.selection))
                            .child(self.tree_flag(
                                format!("breakpoints-line-enabled-{index}"),
                                on,
                                theme,
                                cx,
                                {
                                    let row = row.clone();
                                    move |this, cx| {
                                        this.edit(row.clone(), RowEdit::Enabled(!on), cx);
                                    }
                                },
                            ))
                            .child(icon_sized("debug/breakpoints", 14., theme))
                            .child(
                                div()
                                    .truncate()
                                    .text_color(if on { theme.text } else { theme.text_disabled })
                                    .child(label),
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.selected = Some(BreakpointRow::Line {
                                    file: click_file.clone(),
                                    line,
                                });
                                cx.notify();
                            }))
                            .on_double_click(cx.listener(move |this, _, window, cx| {
                                this.open_line(file.clone(), line, window, cx);
                            }))
                            .into_any_element(),
                    );
                }
            }
        }
        let function_open = !self.collapsed.contains(&BreakpointGroup::Function);
        if !function_breakpoints.is_empty() {
            rows.push(self.group_row(
                "breakpoints-group-function",
                "debug/functionBreakpoint",
                "Method Breakpoints",
                function_on,
                function_open,
                BreakpointGroup::Function,
                theme,
                cx,
            ));
            if function_open {
                for (index, setting) in function_breakpoints.iter().enumerate() {
                    let row = BreakpointRow::Function(index);
                    let selected = self.selected.as_ref() == Some(&row);
                    let on = setting.enabled;
                    let selector = format!("breakpoints-function-row-{index}");
                    rows.push(
                        div()
                            .id(SharedString::from(format!("breakpoints-function-{index}")))
                            .debug_selector(move || selector.clone())
                            .flex()
                            .items_center()
                            .gap(px(SPACE_S))
                            .h(px(ROW_HEIGHT))
                            .pl(px(SPACE_XL))
                            .pr(px(SPACE_S))
                            .rounded(px(ROW_RADIUS))
                            .cursor_pointer()
                            .when(selected, |row| row.bg(theme.selection))
                            .child(self.tree_flag(
                                format!("breakpoints-function-enabled-{index}"),
                                on,
                                theme,
                                cx,
                                {
                                    let row = row.clone();
                                    move |this, cx| {
                                        this.edit(row.clone(), RowEdit::Enabled(!on), cx);
                                    }
                                },
                            ))
                            .child(icon_sized("debug/functionBreakpoint", 14., theme))
                            .child(
                                div()
                                    .truncate()
                                    .text_color(if on { theme.text } else { theme.text_disabled })
                                    .child(setting.dap_name()),
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.selected = Some(BreakpointRow::Function(index));
                                cx.notify();
                            }))
                            .into_any_element(),
                    );
                }
            }
        }
        let data_open = !self.collapsed.contains(&BreakpointGroup::Data);
        if !data_breakpoints.is_empty() {
            rows.push(self.group_row(
                "breakpoints-group-data",
                "debug/fieldWatchpoint",
                "Field Watchpoints",
                data_on,
                data_open,
                BreakpointGroup::Data,
                theme,
                cx,
            ));
            if data_open {
                for (index, setting) in data_breakpoints.iter().enumerate() {
                    let row = BreakpointRow::Data(setting.data_id.clone());
                    let selected = self.selected.as_ref() == Some(&row);
                    let on = setting.enabled;
                    let data_id = setting.data_id.clone();
                    let selector = format!("breakpoints-data-row-{index}");
                    rows.push(
                        div()
                            .id(SharedString::from(format!("breakpoints-data-{index}")))
                            .debug_selector(move || selector.clone())
                            .flex()
                            .items_center()
                            .gap(px(SPACE_S))
                            .h(px(ROW_HEIGHT))
                            .pl(px(SPACE_XL))
                            .pr(px(SPACE_S))
                            .rounded(px(ROW_RADIUS))
                            .cursor_pointer()
                            .when(selected, |row| row.bg(theme.selection))
                            .child(self.tree_flag(
                                format!("breakpoints-data-enabled-{index}"),
                                on,
                                theme,
                                cx,
                                {
                                    let row = row.clone();
                                    move |this, cx| {
                                        this.edit(row.clone(), RowEdit::Enabled(!on), cx);
                                    }
                                },
                            ))
                            .child(icon_sized("debug/fieldWatchpoint", 14., theme))
                            .child(
                                div()
                                    .truncate()
                                    .text_color(if on { theme.text } else { theme.text_disabled })
                                    .child(data_id.clone()),
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.selected = Some(BreakpointRow::Data(data_id.clone()));
                                cx.notify();
                            }))
                            .into_any_element(),
                    );
                }
            }
        }
        let exception_open = !self.collapsed.contains(&BreakpointGroup::Exception);
        rows.push(self.group_row(
            "breakpoints-group-exception",
            "status/error",
            "Exception Breakpoints",
            exception_on,
            exception_open,
            BreakpointGroup::Exception,
            theme,
            cx,
        ));
        if exception_open {
            let row = BreakpointRow::AnyException;
            let selected = self.selected.as_ref() == Some(&row);
            let on = any_exception.enabled;
            rows.push(
                div()
                    .id("breakpoints-exception-any")
                    .debug_selector(|| "breakpoints-exception-any".to_owned())
                    .flex()
                    .items_center()
                    .gap(px(SPACE_S))
                    .h(px(ROW_HEIGHT))
                    .pl(px(SPACE_XL))
                    .pr(px(SPACE_S))
                    .rounded(px(ROW_RADIUS))
                    .cursor_pointer()
                    .when(selected, |row| row.bg(theme.selection))
                    .child(self.tree_flag(
                        "breakpoints-exception-any-enabled".to_owned(),
                        on,
                        theme,
                        cx,
                        {
                            let row = row.clone();
                            move |this, cx| {
                                this.edit(row.clone(), RowEdit::Enabled(!on), cx);
                            }
                        },
                    ))
                    .child(icon_sized("status/error", 14., theme))
                    .child(
                        div()
                            .truncate()
                            .text_color(if on { theme.text } else { theme.text_disabled })
                            .child("Any exception"),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.selected = Some(BreakpointRow::AnyException);
                        cx.notify();
                    }))
                    .into_any_element(),
            );
            for (index, point) in exception_points.iter().enumerate() {
                let row = BreakpointRow::Exception(index);
                let selected = self.selected.as_ref() == Some(&row);
                let on = point.setting.enabled;
                let selector = format!("breakpoints-exception-row-{index}");
                rows.push(
                    div()
                        .id(SharedString::from(format!("breakpoints-exception-{index}")))
                        .debug_selector(move || selector.clone())
                        .flex()
                        .items_center()
                        .gap(px(SPACE_S))
                        .h(px(ROW_HEIGHT))
                        .pl(px(SPACE_XL))
                        .pr(px(SPACE_S))
                        .rounded(px(ROW_RADIUS))
                        .cursor_pointer()
                        .when(selected, |row| row.bg(theme.selection))
                        .child(self.tree_flag(
                            format!("breakpoints-exception-enabled-{index}"),
                            on,
                            theme,
                            cx,
                            {
                                let row = row.clone();
                                move |this, cx| {
                                    this.edit(row.clone(), RowEdit::Enabled(!on), cx);
                                }
                            },
                        ))
                        .child(icon_sized("status/error", 14., theme))
                        .child(
                            div()
                                .truncate()
                                .text_color(if on { theme.text } else { theme.text_disabled })
                                .child(point.class.clone()),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.selected = Some(BreakpointRow::Exception(index));
                            cx.notify();
                        }))
                        .into_any_element(),
                );
            }
            for filter in &exception_filters {
                if filter.filter == "caught" || filter.filter == "uncaught" {
                    continue;
                }
                let row = BreakpointRow::Filter(filter.filter.clone());
                let selected = self.selected.as_ref() == Some(&row);
                let on = enabled_filters.contains(&filter.filter);
                let id = filter.filter.clone();
                let label = filter.label.clone();
                let selector = format!("breakpoints-filter-{id}");
                rows.push(
                    div()
                        .id(SharedString::from(format!("breakpoints-filter-{id}")))
                        .debug_selector(move || selector.clone())
                        .flex()
                        .items_center()
                        .gap(px(SPACE_S))
                        .h(px(ROW_HEIGHT))
                        .pl(px(SPACE_XL))
                        .pr(px(SPACE_S))
                        .rounded(px(ROW_RADIUS))
                        .cursor_pointer()
                        .when(selected, |row| row.bg(theme.selection))
                        .child(self.tree_flag(
                            format!("breakpoints-filter-enabled-{id}"),
                            on,
                            theme,
                            cx,
                            {
                                let row = row.clone();
                                move |this, cx| {
                                    this.edit(row.clone(), RowEdit::Enabled(!on), cx);
                                }
                            },
                        ))
                        .child(icon_sized("status/error", 14., theme))
                        .child(
                            div()
                                .truncate()
                                .text_color(if on { theme.text } else { theme.text_disabled })
                                .child(label),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.selected = Some(BreakpointRow::Filter(id.clone()));
                            cx.notify();
                        }))
                        .into_any_element(),
                );
            }
        }
        rows
    }

    fn add_menu(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let (method_supported, field_supported) =
            self.panel.upgrade().map_or((true, true), |panel| {
                let panel = panel.read(cx);
                (
                    panel.debugger.is_none() || panel.snapshot.capabilities.function_breakpoints,
                    panel.debugger.is_none() || panel.snapshot.capabilities.data_breakpoints,
                )
            });
        let mut menu = select_menu(theme).child(
            select_option(
                "breakpoints-add-exception",
                "Exception Breakpoint\u{2026}",
                false,
                false,
                theme,
            )
            .on_click(cx.listener(|this, _, window, cx| {
                this.ask_add(BreakpointGroup::Exception, window, cx);
            })),
        );
        if method_supported {
            menu = menu.child(
                select_option(
                    "breakpoints-add-method",
                    "Method Breakpoint\u{2026}",
                    false,
                    false,
                    theme,
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    this.ask_add(BreakpointGroup::Function, window, cx);
                })),
            );
        }
        if field_supported {
            menu = menu.child(
                select_option(
                    "breakpoints-add-field",
                    "Field Watchpoint\u{2026}",
                    false,
                    false,
                    theme,
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    this.ask_add(BreakpointGroup::Data, window, cx);
                })),
            );
        }
        menu.into_any_element()
    }
}

impl Render for BreakpointsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        if let Some(panel) = self.panel.upgrade() {
            let row = self.selected.clone();
            let valid = row
                .as_ref()
                .is_some_and(|row| row_exists(panel.read(cx), row));
            if !valid {
                self.selected = Some(initial_breakpoint_row(panel.read(cx)));
            }
        }
        if self.loaded != self.selected {
            self.load_fields(cx);
        }
        let rows = self.tree(&theme, cx);
        let header = div()
            .flex()
            .items_center()
            .gap(px(SPACE_S))
            .pl(px(SPACE_M))
            .pr(px(SPACE_S))
            .h(px(CONTROL_HEIGHT))
            .flex_none()
            .child(
                icon_button(
                    "breakpoints-add",
                    "general/add",
                    "Add Breakpoint",
                    None,
                    &theme,
                )
                .debug_selector(|| "breakpoints-add".to_owned())
                .size(px(20.))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.add_menu = !this.add_menu;
                    cx.notify();
                })),
            )
            .child(
                icon_button(
                    "breakpoints-remove",
                    "general/minus",
                    "Remove Breakpoint",
                    None,
                    &theme,
                )
                .debug_selector(|| "breakpoints-remove".to_owned())
                .size(px(20.))
                .on_click(cx.listener(|this, _, _, cx| this.remove_selected(cx))),
            )
            .child(div().flex_1());
        let list = scrollable(
            "breakpoints-tree",
            &self.scroll_tree,
            div()
                .id("breakpoints-tree")
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(&self.scroll_tree)
                .p(px(SPACE_S))
                .children(rows),
            &theme,
            window,
            cx,
        );
        let mut left = div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(340.))
            .h_full()
            .min_h_0()
            .child(header);
        if self.add_menu {
            left = left.child(div().px(px(SPACE_M)).child(self.add_menu(&theme, cx)));
        }
        left = left.child(list.flex_1());
        let properties = scrollable(
            "breakpoints-properties",
            &self.scroll_properties,
            div()
                .id("breakpoints-properties")
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(&self.scroll_properties)
                .child(self.properties(&theme, cx)),
            &theme,
            window,
            cx,
        );
        let body = div()
            .flex()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .child(left)
            .child(div().w(px(1.)).flex_none().h_full().bg(theme.border))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .h_full()
                    .min_w_0()
                    .min_h_0()
                    .child(properties.flex_1()),
            );
        let done = DialogButtons::new()
            .primary(
                dialog_button(
                    "breakpoints-done",
                    "breakpoints-done".to_owned(),
                    "Done",
                    true,
                    true,
                    &theme,
                )
                .on_click(cx.listener(|this, _, window, cx| this.close(window, cx))),
            )
            .into_row();
        let contents = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(body)
            .child(
                div()
                    .flex_none()
                    .px(px(DIALOG_PADDING))
                    .py(px(SPACE_M))
                    .child(done),
            );
        let shell = surface(&theme)
            .id("breakpoints-dialog")
            .debug_selector(|| "breakpoints-dialog".to_owned())
            .track_focus(&self.focus)
            .text_color(theme.text)
            .text_size(px(UI_SIZE))
            .on_key_down(
                cx.listener(|this, event: &KeyDownEvent, window, cx| this.key(event, window, cx)),
            );
        let viewport = window.viewport_size();
        let width = px(820.);
        let left = (viewport.width - width) / 2.;
        let top = viewport.height * 0.12;
        let close = cx.listener(|this, _: &ClickEvent, window, cx| this.close(window, cx));
        dialog_window(
            DialogSpec {
                key: "breakpoints-dialog",
                title: "Breakpoints".into(),
                extra: None,
                place: DialogPlace {
                    left: f32::from(left),
                    top: f32::from(top),
                    width: f32::from(width),
                    height: Some(420.),
                },
                min: (620., 320.),
            },
            shell,
            window,
            cx,
            close,
            contents,
        )
    }
}
/// A path of a value as the language writes it, such as `items[0].name`.
fn expression_of(path: &[String]) -> String {
    let mut text = String::new();
    for step in path {
        if step.starts_with('[') || text.is_empty() {
            text.push_str(step);
        } else {
            text.push('.');
            text.push_str(step);
        }
    }
    text
}

/// The expression that names the value that holds the last step of a path.
fn parent_expression(path: &[String]) -> Option<String> {
    (path.len() > 1).then(|| expression_of(&path[..path.len() - 1]))
}

/// The hit count a tool set, from the condition the adapter got.
fn hit_count_of(text: &str) -> Option<u32> {
    let text = text.trim().trim_start_matches(['>', '=', '<', ' ']);
    text.parse().ok()
}

/// The text of a line of a file, when the file is on disk.
fn line_text(file: &Path, line: u32) -> Option<String> {
    let text = std::fs::read_to_string(file).ok()?;
    text.lines()
        .nth(line.saturating_sub(1) as usize)
        .map(|line| line.trim_end_matches('\r').to_string())
}

/// A value the adapter answered, with the handle of its children.
fn dap_value(expression: &str, body: &serde_json::Value) -> (DebugValueNode, i64) {
    let value = body
        .get("result")
        .or_else(|| body.get("value"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    let type_name = body
        .get("type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    let reference = body
        .get("variablesReference")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);
    (
        DebugValueNode {
            name: expression.to_string(),
            value,
            type_name,
            children: Vec::new(),
        },
        reference,
    )
}

/// The node a path of child indexes names.
fn node_at_mut<'a>(root: &'a mut DebugValueNode, path: &[usize]) -> &'a mut DebugValueNode {
    let mut node = root;
    for index in path {
        node = &mut node.children[*index];
    }
    node
}

/// Fills the children of a value down to `depth`, one request per level, as the tree of
/// the window would be expanded.
async fn fill_tree(debugger: &Debugger, root: &mut DebugValueNode, reference: i64, depth: usize) {
    if depth == 0 || reference == 0 {
        return;
    }
    let mut queue = vec![(Vec::<usize>::new(), reference, depth)];
    while let Some((path, reference, remaining)) = queue.pop() {
        if remaining == 0 || reference == 0 {
            continue;
        }
        let Ok(body) = debugger
            .request(
                "variables",
                serde_json::json!({ "variablesReference": reference }),
            )
            .await
        else {
            continue;
        };
        let list = body
            .get("variables")
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut children = Vec::with_capacity(list.len());
        let mut references = Vec::new();
        for item in &list {
            let name = item
                .get("name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            let value = item
                .get("value")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            let type_name = item
                .get("type")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            let handle = item
                .get("variablesReference")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0);
            if handle > 0 {
                references.push((children.len(), handle));
            }
            children.push(DebugValueNode {
                name,
                value,
                type_name,
                children: Vec::new(),
            });
        }
        node_at_mut(root, &path).children = children;
        for (index, handle) in references {
            let mut child_path = path.clone();
            child_path.push(index);
            queue.push((child_path, handle, remaining - 1));
        }
    }
}

/// Puts the breakpoints of the panel into the gutter of `view`.
pub(crate) fn refresh_breakpoints(
    view: &mut editor_view::EditorView,
    cx: &mut Context<editor_view::EditorView>,
) {
    let Some(panel) = cx.try_global::<ActiveDebug>() else {
        return;
    };
    let panel = panel.0.clone();
    let lines = panel.read(cx).breakpoint_lines(view.path());
    let lines: Vec<u32> = lines
        .into_iter()
        .filter(|line| *line > 0)
        .map(|line| line - 1)
        .collect();
    view.set_breakpoints(lines.clone(), cx);
    // A muted list is grey in the gutter, as the button of the Debug toolbar greys it;
    // a point the user turned off is grey too and keeps its place.
    let mut disabled: Vec<u32> = if panel.read(cx).muted() {
        lines.clone()
    } else {
        Vec::new()
    };
    disabled.extend(
        panel
            .read(cx)
            .disabled_breakpoint_lines(view.path())
            .into_iter()
            .filter(|line| *line > 0)
            .map(|line| line - 1),
    );
    disabled.sort_unstable();
    disabled.dedup();
    view.set_disabled_breakpoints(disabled, cx);
    let muted = panel.read(cx).non_suspending_breakpoint_lines(view.path());
    let muted: Vec<u32> = muted
        .into_iter()
        .filter(|line| *line > 0)
        .map(|line| line - 1)
        .collect();
    view.set_muted_breakpoints(muted, cx);
    let temporary = panel.read(cx).temporary_breakpoint_lines(view.path());
    let temporary: Vec<u32> = temporary
        .into_iter()
        .filter(|line| *line > 0)
        .map(|line| line - 1)
        .collect();
    view.set_temporary_breakpoints(temporary, cx);
    let agent = panel.read(cx).agent_breakpoint_lines(view.path());
    let agent: Vec<u32> = agent
        .into_iter()
        .filter(|line| *line > 0)
        .map(|line| line - 1)
        .collect();
    view.set_agent_breakpoints(agent, cx);
    let function = panel.read(cx).function_breakpoint_lines(view.path());
    let function: Vec<u32> = function
        .into_iter()
        .filter(|line| *line > 0)
        .map(|line| line - 1)
        .collect();
    view.set_function_breakpoints(function, cx);
    let fields = panel.read(cx).field_watchpoint_lines(view.path());
    let fields: Vec<u32> = fields
        .into_iter()
        .filter(|line| *line > 0)
        .map(|line| line - 1)
        .collect();
    view.set_field_watchpoints(fields, cx);
}

/// Shows the values of the stopped frame after the lines of the method the editor
/// shows, or takes them away when the program runs or Settings hides them. The line the
/// program stopped on is tinted in the file of its frame.
pub(crate) fn refresh_inline_values(
    view: &mut editor_view::EditorView,
    cx: &mut Context<editor_view::EditorView>,
) {
    let enabled = cx
        .try_global::<workspace::Settings>()
        .is_none_or(|settings| settings.debug_show_values_inline);
    let Some(panel) = cx.try_global::<ActiveDebug>() else {
        view.set_execution_line(None, cx);
        view.set_debug_values(None, &[], cx);
        view.set_inline_watches(&[], cx);
        view.set_inline_watch_ready(false, cx);
        return;
    };
    let panel = panel.0.clone();
    let path = view.path().to_path_buf();
    let shown = panel.read_with(cx, |panel, _| {
        let (file, line) = panel.stop_location()?;
        let values = enabled.then(|| panel.stopped_values());
        let watches = panel.inline_watch_rows(&path);
        Some((file, line, values, watches))
    });
    match shown {
        Some((file, line, values, watches)) if same_file(&file, &path) => {
            view.set_execution_line(Some(line), cx);
            match values {
                Some(values) => view.set_debug_values(Some(line), &values, cx),
                None => view.set_debug_values(None, &[], cx),
            }
            view.set_inline_watches(&watches, cx);
            view.set_inline_watch_ready(true, cx);
        }
        _ => {
            view.set_execution_line(None, cx);
            view.set_debug_values(None, &[], cx);
            view.set_inline_watches(&[], cx);
            view.set_inline_watch_ready(false, cx);
        }
    }
}

/// Whether two paths name the same file. They are compared the way the window compares the
/// paths of its tabs: by their components, so a `\` of the adapter and a `/` of the join of
/// a project are one separator, and on Windows the case does not count.
fn same_file(left: &Path, right: &Path) -> bool {
    ::platform::path_key(left) == ::platform::path_key(right)
}

/// The toolbar and the debug keys of the window.
pub fn controls(
    panel: &Entity<DebugPanel>,
    runs: &Entity<crate::run::RunPanel>,
) -> workspace::DebugControls {
    use std::rc::Rc;

    let (state, start, over, into, force_into, smart, out, resume) = (
        panel.clone(),
        runs.clone(),
        panel.clone(),
        panel.clone(),
        panel.clone(),
        panel.clone(),
        panel.clone(),
        panel.clone(),
    );
    let (evaluate, quick, toggle, view, run_to, execution, stop) = (
        panel.clone(),
        panel.clone(),
        panel.clone(),
        panel.clone(),
        panel.clone(),
        panel.clone(),
        panel.clone(),
    );
    let (force_over, temporary, edit) = (panel.clone(), panel.clone(), panel.clone());
    workspace::DebugControls {
        state: Rc::new(move |cx| {
            let (active, stopped) = state.read(cx).state();
            workspace::DebugRunState { active, stopped }
        }),
        start: Rc::new(move |window, cx| {
            start.update(cx, |runs, cx| runs.debug(window, cx));
        }),
        step_over: Rc::new(move |_, cx| over.update(cx, |panel, cx| panel.step_over(cx))),
        step_into: Rc::new(move |_, cx| into.update(cx, |panel, cx| panel.step_into(cx))),
        force_step_into: Rc::new(move |_, cx| {
            force_into.update(cx, |panel, cx| panel.force_step_into(cx))
        }),
        force_step_over: Rc::new(move |_, cx| {
            force_over.update(cx, |panel, cx| panel.force_step_over(cx))
        }),
        toggle_temporary_breakpoint: Rc::new(move |_, cx| {
            temporary.update(cx, |panel, cx| panel.toggle_temporary_at_caret(cx))
        }),
        edit_breakpoint: Rc::new(move |window, cx| {
            edit.update(cx, |panel, cx| panel.edit_or_view_breakpoints(window, cx))
        }),
        smart_step_into: Rc::new(move |window, cx| {
            smart.update(cx, |panel, cx| panel.smart_step_into(window, cx))
        }),
        step_out: Rc::new(move |_, cx| out.update(cx, |panel, cx| panel.step_out(cx))),
        resume: Rc::new(move |_, cx| resume.update(cx, |panel, cx| panel.resume(cx))),
        evaluate: Rc::new(move |window, cx| {
            evaluate.update(cx, |panel, cx| panel.ask_evaluate(window, cx))
        }),
        quick_evaluate: Rc::new(move |window, cx| {
            quick.update(cx, |panel, cx| panel.quick_evaluate(window, cx))
        }),
        toggle_breakpoint: Rc::new(move |_, cx| {
            toggle.update(cx, |panel, cx| panel.toggle_at_caret(cx))
        }),
        view_breakpoints: Rc::new(move |window, cx| {
            view.update(cx, |panel, cx| panel.show_breakpoints(window, cx))
        }),
        run_to_cursor: Rc::new(move |force, window, cx| {
            run_to.update(cx, |panel, cx| panel.run_to_cursor(force, cx));
            let _ = window;
        }),
        show_execution_point: Rc::new(move |window, cx| {
            execution.update(cx, |panel, cx| panel.show_execution_point(window, cx))
        }),
        stop: Rc::new(move |_, cx| stop.update(cx, |panel, cx| panel.stop(cx))),
    }
}

// The only test here runs ping.exe and cmd.exe.
#[cfg(all(test, windows))]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use proc_sup::ProcessSpec;
    use ui_kit::gpui;
    use ui_kit::gpui::{AppContext, Entity, ScrollHandle, TestAppContext, WeakEntity};

    use super::{
        BreakpointSpec, ConsoleEvent, ConsoleFollow, DataBreakpointSetting, DebugPanel,
        DebugService, Debugger, FunctionBreakpointSetting, OutputBuffers, RunPlan, SavedState,
        Snapshot,
    };

    /// Counts the launches; a start after Stop would be a failure of the panel.
    struct NeverStarted {
        starts: Arc<AtomicUsize>,
    }

    impl DebugService for NeverStarted {
        fn start(
            &self,
            _program: ProcessSpec,
            _name: String,
            _entry: Option<PathBuf>,
            _breakpoints: Vec<BreakpointSpec>,
            _watch: Vec<String>,
            _exception_filters: Vec<String>,
        ) -> Result<Debugger, String> {
            self.starts.fetch_add(1, Ordering::Relaxed);
            Err("the program must not start after Stop".into())
        }

        fn attach(
            &self,
            _host: String,
            _port: u16,
            _name: String,
            _project: Option<String>,
            _breakpoints: Vec<BreakpointSpec>,
            _watch: Vec<String>,
            _exception_filters: Vec<String>,
        ) -> Result<Debugger, String> {
            Err("nothing to attach to in this test".into())
        }
    }

    /// The panel without its polling task: the test calls `poll` itself.
    fn debug_panel(cx: &mut TestAppContext) -> Entity<DebugPanel> {
        cx.update(|cx| {
            cx.new(|cx| DebugPanel {
                workspace: WeakEntity::new_invalid(),
                focus: cx.focus_handle(),
                debugger: None,
                snapshot: Snapshot::default(),
                selected_variable: None,
                variable_edit: None,
                watch_input: None,
                selected_watch: None,
                evaluate_history: Vec::new(),
                temporary: None,
                forced_step: None,
                seen_stops: 0,
                opened_frame: None,
                asking_targets: false,
                asking_targets_window: None,
                window: None,
                reveal_stop: None,
                muted: false,
                breakpoints: Vec::new(),
                function_breakpoints: Vec::new(),
                data_breakpoints: Vec::new(),
                any_exception: super::ExceptionSetting::default(),
                exception_points: Vec::new(),
                enabled_filters: BTreeSet::new(),
                watch: Vec::new(),
                inline_watches: Vec::new(),
                labels: Vec::new(),
                identities: BTreeMap::new(),
                identity_questions: BTreeMap::new(),
                pending_marks: Vec::new(),
                identity_stop: 0,
                next_identity: 1,
                hide_library_frames: None,
                expanded_library_groups: BTreeSet::new(),
                language_renderers: Vec::new(),
                service: None,
                sent_renderers: Vec::new(),
                sent_inline_watches: Vec::new(),
                expanded: BTreeSet::new(),
                shown_breakpoints: Vec::new(),
                shown_function_breakpoints: Vec::new(),
                shown_data_breakpoints: Vec::new(),
                local_console: Vec::new(),
                seen_console: Vec::new(),
                console_input: crate::console_input::ConsoleInput::new(),
                console_search: Default::default(),
                search_focus: cx.focus_handle(),
                filter_focus: cx.focus_handle(),
                files: None,
                build: None,
                build_output: OutputBuffers::default(),
                build_process: None,
                build_failed: false,
                build_stopped: false,
                stop_shown: false,
                pending: None,
                restart: None,
                front: super::Front::Debugger,
                tree_width: super::TREE_WIDTH,
                split_from: None,
                session: None,
                store: None,
                root: PathBuf::new(),
                state_dir: test_support::scratch("debug-panel-state"),
                tool_output_file: None,
                tool_output_lines: 0,
                title: "Debug".into(),
                scroll_tree: ScrollHandle::new(),
                console_scroll: ConsoleFollow::new(),
                scroll_variables: ScrollHandle::new(),
                scroll_watch: ScrollHandle::new(),
                stop_values: BTreeMap::new(),
                changed_values: BTreeSet::new(),
                values_stop: 0,
                tool_debug: None,
                tool_waits: Vec::new(),
                tool_steps: Vec::new(),
                tool_stacks: Vec::new(),
                tool_variables: Vec::new(),
                next_tool_session: 0,
                tool_client: None,
                tool_session: None,
                tool_controls: Vec::new(),
            })
        })
    }

    #[test]
    fn method_and_field_settings_survive_the_saved_state() {
        let saved = SavedState {
            breakpoints: Vec::new(),
            function_breakpoints: vec![FunctionBreakpointSetting::new(
                PathBuf::from("C:/demo/Probe"),
                4,
                Some("demo.Probe".into()),
                "work".into(),
            )],
            data_breakpoints: vec![DataBreakpointSetting::from_declaration(
                PathBuf::from("C:/demo/Probe"),
                2,
                "demo.Probe".into(),
                "value".into(),
            )],
            exception_filters: vec!["uncaught".into()],
            any_exception: super::ExceptionSetting::default(),
            exception_points: Vec::new(),
            inline_watches: Vec::new(),
            muted: false,
        };
        let json = serde_json::to_string(&saved).unwrap();
        let back: SavedState = serde_json::from_str(&json).unwrap();
        assert_eq!(back.function_breakpoints[0].dap_name(), "demo.Probe#work");
        assert_eq!(back.data_breakpoints[0].data_id, "demo.Probe#value");
        assert!(back.data_breakpoints[0].write);
        assert_eq!(back.data_breakpoints[0].line, 2);
        assert_eq!(back.exception_filters, ["uncaught"]);
    }

    // ping.exe and cmd.exe stand in for the build and the program.
    #[ui_kit::gpui::test]
    fn stop_during_the_build_of_a_debug_launch_does_not_start_the_program(cx: &mut TestAppContext) {
        let starts = Arc::new(AtomicUsize::new(0));
        let service = Arc::new(NeverStarted {
            starts: starts.clone(),
        });
        let panel = debug_panel(cx);
        let plan = RunPlan {
            before: Some(
                ProcessSpec::new("ping.exe")
                    .args(["-n", "120", "127.0.0.1"])
                    // No helper in this test binary, so Stop goes straight to force.
                    .ctrl_helper("no-such-helper.exe"),
            ),
            program: ProcessSpec::new("cmd.exe").args(["/d", "/c", "exit", "0"]),
            grace: Duration::ZERO,
        };
        panel.update(cx, |panel, cx| {
            panel.start_plan(service, plan, "Demo".into(), None, cx);
        });

        // Stop only once the build really runs, as the user would after seeing it start.
        let started = Instant::now();
        loop {
            let running = panel.read_with(cx, |panel, _| {
                panel.build.as_ref().is_some_and(|console| {
                    while let Ok(event) = console.events().try_recv() {
                        if matches!(event, ConsoleEvent::Started { .. }) {
                            return true;
                        }
                    }
                    false
                })
            });
            if running {
                break;
            }
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "the build did not start"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        panel.update(cx, |panel, cx| panel.stop(cx));

        let start = Instant::now();
        loop {
            panel.update(cx, |panel, cx| panel.poll(cx));
            let (building, pending) = panel.read_with(cx, |panel, _| {
                (panel.build.is_some(), panel.pending.is_some())
            });
            if !building && !pending {
                break;
            }
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "the build did not stop"
            );
            std::thread::sleep(Duration::from_millis(20));
        }

        assert_eq!(
            starts.load(Ordering::Relaxed),
            0,
            "the program started after Stop"
        );
    }
}

// The text colours of the frame rows drawn in the last frame, for the tests of the list.
#[cfg(test)]
thread_local! {
    static FRAME_TEXT_COLORS: std::cell::RefCell<BTreeMap<i64, ui_kit::gpui::Hsla>> =
        const { std::cell::RefCell::new(BTreeMap::new()) };
}

/// Records the text colour its row gives it, without painting or taking room.
#[cfg(test)]
fn frame_color_probe(frame: i64) -> AnyElement {
    canvas(
        move |_, window, _| {
            let color = window.text_style().color;
            FRAME_TEXT_COLORS.with(|colors| colors.borrow_mut().insert(frame, color));
        },
        |_, _, _, _| {},
    )
    .absolute()
    .top_0()
    .left_0()
    .w(px(0.))
    .h(px(0.))
    .into_any_element()
}

/// Forgets the colours of the frames drawn before.
#[cfg(test)]
pub(crate) fn clear_frame_colors() {
    FRAME_TEXT_COLORS.with(|colors| colors.borrow_mut().clear());
}

/// The colour the frame `id` was drawn in the last frame, for tests.
#[cfg(test)]
pub(crate) fn frame_text_color(id: i64) -> Option<ui_kit::gpui::Hsla> {
    FRAME_TEXT_COLORS.with(|colors| colors.borrow().get(&id).copied())
}

#[cfg(test)]
#[path = "debug_depth_tests.rs"]
mod depth_tests;

#[cfg(test)]
#[path = "debug_frame_tests.rs"]
mod frame_tests;
