//! The Debug tool window: threads, the stack, variables with lazy expansion, watch
//! expressions, the console and the breakpoints of the project.
//!
//! The panel is the window's end of a `dap_client::Debugger`: it sends commands and
//! draws the newest snapshot, so an adapter never holds a frame.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use dap_client::{
    Breakpoint, BreakpointSpec, Command, ConsoleLine, DataAccessType, DataBreakpointSpec,
    DebugService, DebugSessionToken, DebugState, Debugger, Frame, FunctionBreakpointSpec, Scope,
    Snapshot, StepTarget, Thread, Variable,
};
use runner::{Console, ConsoleEvent, RunPlan, Step, Stream};
use serde::{Deserialize, Serialize};
use store::Store;
use tool_gateway::DebugSnapshotSource;
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::{DEBUG_TOOL, Workspace};

use crate::ansi::OutputBuffers;
use crate::console_input::{ConsoleInput, ConsoleKey};
use crate::navigation;
use crate::prompt::Prompt;

const CONTEXT: &str = "DebugPanel";

actions!(debug_panel, [NewWatch, DeleteWatch, SetValue, CancelInput]);

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
    /// A breakpoint that goes away at the first stop.
    #[serde(default)]
    temporary: bool,
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
            temporary: false,
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

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct SavedState {
    breakpoints: Vec<BreakpointSetting>,
    function_breakpoints: Vec<FunctionBreakpointSetting>,
    data_breakpoints: Vec<DataBreakpointSetting>,
    exception_filters: Vec<String>,
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
    /// The temporary breakpoint of Run to Cursor.
    temporary: Option<TemporaryBreakpoint>,
    /// The breakpoints Force Step Over turned off until the step ends.
    forced_step: Option<Vec<BreakpointSpec>>,
    /// The stop the one-stop settings (temporary breakpoints, Force Step Over) last
    /// ended at; a snapshot of the same stop must not end the ones set after it.
    seen_stops: u64,
    /// Smart Step Into waits for the list of calls from the adapter.
    asking_targets: bool,
    asking_targets_window: Option<AnyWindowHandle>,
    /// Breakpoints in file and line order, with their properties.
    breakpoints: Vec<BreakpointSetting>,
    /// Method breakpoints, with their properties.
    function_breakpoints: Vec<FunctionBreakpointSetting>,
    /// Field watchpoints, with their properties.
    data_breakpoints: Vec<DataBreakpointSetting>,
    /// Filters of the adapter the user turned on.
    enabled_filters: BTreeSet<String>,
    watch: Vec<String>,
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
    /// The build step of a debug launch, before the debuggee starts.
    build: Option<Console>,
    /// The unfinished lines of the build process, each stream on its own.
    build_output: OutputBuffers,
    /// The process of the build step whose output comes now.
    build_process: Option<u32>,
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
    title: SharedString,
    scroll_tree: ScrollHandle,
    scroll_side: ScrollHandle,
    scroll_variables: ScrollHandle,
    scroll_watch: ScrollHandle,
}

impl EventEmitter<BreakpointsChanged> for DebugPanel {}

impl DebugPanel {
    /// `root` and `store` come from the caller: the panel is made while the workspace is
    /// still being updated, so its handle cannot be read here.
    pub fn create(
        root: PathBuf,
        store: Option<Arc<Store>>,
        workspace: WeakEntity<Workspace>,
        cx: &mut App,
    ) -> Entity<Self> {
        let saved = store
            .as_ref()
            .and_then(|store| store.get(&state_key(&root)).ok().flatten())
            .and_then(|json| serde_json::from_str::<SavedState>(&json).ok())
            .unwrap_or_default();
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
            temporary: None,
            forced_step: None,
            seen_stops: 0,
            asking_targets: false,
            asking_targets_window: None,
            breakpoints: saved.breakpoints,
            function_breakpoints: saved.function_breakpoints,
            data_breakpoints: saved.data_breakpoints,
            enabled_filters: saved.exception_filters.into_iter().collect(),
            watch: Vec::new(),
            expanded: BTreeSet::new(),
            shown_breakpoints: Vec::new(),
            shown_function_breakpoints: Vec::new(),
            shown_data_breakpoints: Vec::new(),
            local_console: Vec::new(),
            seen_console: Vec::new(),
            console_input: ConsoleInput::new(),
            build: None,
            build_output: OutputBuffers::default(),
            build_process: None,
            pending: None,
            restart: None,
            front: Front::Debugger,
            tree_width: TREE_WIDTH,
            split_from: None,
            session: None,
            store,
            root,
            title: "Debug".into(),
            scroll_tree: ScrollHandle::new(),
            scroll_side: ScrollHandle::new(),
            scroll_variables: ScrollHandle::new(),
            scroll_watch: ScrollHandle::new(),
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

    /// Whether a scope or a variable of the tree is open, for tests.
    pub fn is_expanded(&self, reference: i64) -> bool {
        self.expanded.contains(&reference)
    }

    /// The watch expressions the window keeps, for tests.
    pub fn watch_expressions(&self) -> Vec<String> {
        self.watch.clone()
    }

    /// The lines of the debug console, for tests.
    #[cfg(test)]
    pub(crate) fn console_lines(&self) -> Vec<String> {
        self.local_console
            .iter()
            .map(|line| line.text.clone())
            .collect()
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
        let value = self
            .snapshot
            .variables
            .get(&reference)
            .and_then(|variables| variables.iter().find(|variable| variable.name == name))
            .map(|variable| variable.value.clone())
            .unwrap_or_default();
        self.selected_variable = Some(VariableTarget {
            reference,
            name: name.to_owned(),
            value,
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
        self.temporary = None;
        self.forced_step = None;
        self.seen_stops = 0;
        self.asking_targets = false;
        self.asking_targets_window = None;
        self.local_console.clear();
        self.seen_console.clear();
        self.console_input.restart();
        self.build = None;
        self.build_output = OutputBuffers::default();
        self.build_process = None;
        self.pending = None;
        self.session = None;
        self.front = Front::Debugger;
    }

    fn launch_pending(&mut self, cx: &mut Context<Self>) {
        let Some(pending) = self.pending.take() else {
            return;
        };
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

    /// Whether the breakpoint on the line is on, for the properties popup and tests.
    pub fn breakpoint_enabled(&self, file: &Path, line: u32) -> Option<bool> {
        self.setting(file, line).map(|setting| setting.enabled)
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

    fn remove_function_breakpoint(&mut self, file: PathBuf, line: u32, cx: &mut Context<Self>) {
        self.function_breakpoints
            .retain(|breakpoint| breakpoint.file != file || breakpoint.line != line);
        self.send_function_breakpoints(cx);
        self.changed(cx);
    }

    fn remove_data_breakpoint(&mut self, data_id: String, cx: &mut Context<Self>) {
        self.data_breakpoints
            .retain(|watch| watch.data_id != data_id);
        self.send_data_breakpoints(cx);
        self.changed(cx);
    }

    fn set_function_enabled(&mut self, file: PathBuf, line: u32, on: bool, cx: &mut Context<Self>) {
        if let Some(setting) = self
            .function_breakpoints
            .iter_mut()
            .find(|breakpoint| breakpoint.file == file && breakpoint.line == line)
        {
            setting.enabled = on;
        }
        self.send_function_breakpoints(cx);
        self.changed(cx);
    }

    fn set_function_condition(
        &mut self,
        file: PathBuf,
        line: u32,
        text: String,
        cx: &mut Context<Self>,
    ) {
        let text = text.trim().to_owned();
        if let Some(setting) = self
            .function_breakpoints
            .iter_mut()
            .find(|breakpoint| breakpoint.file == file && breakpoint.line == line)
        {
            setting.condition = (!text.is_empty()).then_some(text);
        }
        self.send_function_breakpoints(cx);
        self.changed(cx);
    }

    fn set_function_hit_condition(
        &mut self,
        file: PathBuf,
        line: u32,
        text: String,
        cx: &mut Context<Self>,
    ) {
        let text = text.trim().to_owned();
        if let Some(setting) = self
            .function_breakpoints
            .iter_mut()
            .find(|breakpoint| breakpoint.file == file && breakpoint.line == line)
        {
            setting.hit_condition = (!text.is_empty()).then_some(text);
        }
        self.send_function_breakpoints(cx);
        self.changed(cx);
    }

    fn set_data_enabled(&mut self, data_id: String, on: bool, cx: &mut Context<Self>) {
        if let Some(setting) = self
            .data_breakpoints
            .iter_mut()
            .find(|watch| watch.data_id == data_id)
        {
            setting.enabled = on;
        }
        self.send_data_breakpoints(cx);
        self.changed(cx);
    }

    /// Cycles the access of a field watchpoint: write, read, both.
    fn cycle_data_access(&mut self, data_id: String, cx: &mut Context<Self>) {
        if let Some(setting) = self
            .data_breakpoints
            .iter_mut()
            .find(|watch| watch.data_id == data_id)
        {
            match setting.access() {
                DataAccessType::Write => {
                    setting.write = false;
                    setting.read = true;
                }
                DataAccessType::Read => {
                    setting.write = true;
                    setting.read = true;
                }
                DataAccessType::ReadWrite => {
                    setting.write = true;
                    setting.read = false;
                }
            }
        }
        self.send_data_breakpoints(cx);
        self.changed(cx);
    }

    fn set_data_condition(&mut self, data_id: String, text: String, cx: &mut Context<Self>) {
        let text = text.trim().to_owned();
        if let Some(setting) = self
            .data_breakpoints
            .iter_mut()
            .find(|watch| watch.data_id == data_id)
        {
            setting.condition = (!text.is_empty()).then_some(text);
        }
        self.send_data_breakpoints(cx);
        self.changed(cx);
    }

    fn set_data_hit_condition(&mut self, data_id: String, text: String, cx: &mut Context<Self>) {
        let text = text.trim().to_owned();
        if let Some(setting) = self
            .data_breakpoints
            .iter_mut()
            .find(|watch| watch.data_id == data_id)
        {
            setting.hit_condition = (!text.is_empty()).then_some(text);
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

    fn set_hit_condition(
        &mut self,
        file: PathBuf,
        line: u32,
        text: String,
        cx: &mut Context<Self>,
    ) {
        let text = text.trim().to_owned();
        if let Some(setting) = self.find(file, line) {
            setting.hit_condition = (!text.is_empty()).then_some(text);
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
            setting.log_expression = (!text.is_empty()).then_some(text);
            let spec = setting.spec();
            self.send(Command::SetBreakpoint(spec), cx);
            self.changed(cx);
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

    fn specs(&self) -> Vec<BreakpointSpec> {
        self.breakpoints
            .iter()
            .map(BreakpointSetting::spec)
            .collect()
    }

    fn function_specs(&self) -> Vec<FunctionBreakpointSpec> {
        self.function_breakpoints
            .iter()
            .map(FunctionBreakpointSetting::spec)
            .collect()
    }

    fn data_specs(&self) -> Vec<DataBreakpointSpec> {
        self.data_breakpoints
            .iter()
            .map(DataBreakpointSetting::spec)
            .collect()
    }

    fn send_function_breakpoints(&self, cx: &mut Context<Self>) {
        self.send(Command::SetFunctionBreakpoints(self.function_specs()), cx);
    }

    fn send_data_breakpoints(&self, cx: &mut Context<Self>) {
        self.send(Command::SetDataBreakpoints(self.data_specs()), cx);
    }

    fn filter_list(&self) -> Vec<String> {
        self.enabled_filters.iter().cloned().collect()
    }

    fn toggle_filter(&mut self, filter: String, cx: &mut Context<Self>) {
        if !self.enabled_filters.remove(&filter) {
            self.enabled_filters.insert(filter);
        }
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
            exception_filters: self.filter_list(),
        };
        let Ok(json) = serde_json::to_string(&saved) else {
            return;
        };
        if let Err(error) = store.put(&state_key(&self.root), &json) {
            eprintln!("breakpoints are not saved: {error}");
        }
    }

    fn poll(&mut self, cx: &mut Context<Self>) {
        let mut build_changed = self.poll_build(cx);
        if let Some(reason) = self.debugger.as_ref().and_then(Debugger::take_input_error) {
            self.input_error_line(&reason);
            build_changed = true;
        }
        let Some(debugger) = &self.debugger else {
            if matches!(self.snapshot.state, DebugState::Terminated) {
                self.session = None;
            }
            if let Some(hub) = cx.try_global::<ActiveDebugHub>() {
                hub.0.clear();
            }
            if build_changed {
                cx.notify();
            }
            return;
        };
        let Some(snapshot) = debugger.poll() else {
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
        // A program that reads its input needs the line of the console at hand.
        if self.takes_input() && self.front != Front::Console {
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
            }
            self.finish_temporary(cx);
            self.finish_temporary_breakpoints(cx);
            self.finish_forced_step(cx);
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
        if breakpoints_changed {
            cx.emit(BreakpointsChanged);
        }
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
        let mut exit = None;
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
                ConsoleEvent::Exited { code, step } => {
                    match self.build_process.take() {
                        Some(process) => self.build_output.finish(process, &mut done),
                        None => self.build_output.finish_all(&mut done),
                    }
                    exit = Some((code, step));
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
        if let Some((code, step)) = exit {
            self.build = None;
            let text = if step == Step::Before {
                format!("Build finished with exit code {}", code_text(code))
            } else {
                format!("Process finished with exit code {}", code_text(code))
            };
            self.push_output(text, code != Some(0), cx);
            if code == Some(0) {
                self.launch_pending(cx);
            } else {
                self.pending = None;
            }
        }
        changed
    }

    /// Output that comes from the process of a test run, forwarded by the Run panel.
    pub fn push_output(&mut self, text: impl Into<String>, error: bool, cx: &mut Context<Self>) {
        for line in text.into().lines() {
            if !line.is_empty() {
                self.local_console
                    .push(ConsoleLine::new(line.to_owned(), error));
            }
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

    /// The context menu of a frame: Drop Frame, when the adapter allows it.
    fn frame_menu(&mut self, frame: Frame, position: Point<Pixels>, cx: &mut Context<Self>) {
        let panel = cx.weak_entity();
        let frame_id = frame.id;
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
                workspace.show_menu(position, vec![entry], cx)
            })
            .ok();
    }

    /// The context menu of a variable: Set Value and Add Field Watchpoint.
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
                workspace.show_menu(position, vec![watch, set], cx)
            })
            .ok();
    }

    pub fn resume(&mut self, _cx: &mut Context<Self>) {
        self.send(Command::Continue, _cx);
    }

    pub fn pause(&mut self, cx: &mut Context<Self>) {
        self.send(Command::Pause, cx);
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
        self.send(Command::Next, _cx);
    }

    pub fn step_into(&mut self, _cx: &mut Context<Self>) {
        self.send(Command::StepIn, _cx);
    }

    pub fn step_out(&mut self, _cx: &mut Context<Self>) {
        self.send(Command::StepOut, _cx);
    }

    pub fn stop(&mut self, cx: &mut Context<Self>) {
        if let Some(build) = &self.build {
            build.stop();
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
        let view = cx.new(|cx| BreakpointsView {
            panel,
            workspace: workspace.clone(),
            focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
        });
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

    fn ask_function_condition(
        &mut self,
        file: PathBuf,
        line: u32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let current = self
            .function_breakpoints
            .iter()
            .find(|setting| setting.file == file && setting.line == line)
            .and_then(|setting| setting.condition.clone())
            .unwrap_or_default();
        let panel = cx.weak_entity();
        let submit = move |text: String, cx: &mut App| {
            panel
                .update(cx, |panel, cx| {
                    panel.set_function_condition(file.clone(), line, text, cx)
                })
                .ok();
        };
        self.workspace
            .update(cx, |workspace, cx| {
                Prompt::show_with(
                    "Method Breakpoint Condition",
                    &current,
                    submit,
                    workspace,
                    window,
                    cx,
                )
            })
            .ok();
    }

    fn ask_function_hit_condition(
        &mut self,
        file: PathBuf,
        line: u32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let current = self
            .function_breakpoints
            .iter()
            .find(|setting| setting.file == file && setting.line == line)
            .and_then(|setting| setting.hit_condition.clone())
            .unwrap_or_default();
        let panel = cx.weak_entity();
        let submit = move |text: String, cx: &mut App| {
            panel
                .update(cx, |panel, cx| {
                    panel.set_function_hit_condition(file.clone(), line, text, cx)
                })
                .ok();
        };
        self.workspace
            .update(cx, |workspace, cx| {
                Prompt::show_with(
                    "Method Breakpoint Hit Count",
                    &current,
                    submit,
                    workspace,
                    window,
                    cx,
                )
            })
            .ok();
    }

    fn ask_data_condition(&mut self, data_id: String, window: &mut Window, cx: &mut Context<Self>) {
        let current = self
            .data_breakpoints
            .iter()
            .find(|setting| setting.data_id == data_id)
            .and_then(|setting| setting.condition.clone())
            .unwrap_or_default();
        let panel = cx.weak_entity();
        let submit = move |text: String, cx: &mut App| {
            panel
                .update(cx, |panel, cx| {
                    panel.set_data_condition(data_id.clone(), text, cx)
                })
                .ok();
        };
        self.workspace
            .update(cx, |workspace, cx| {
                Prompt::show_with(
                    "Field Watchpoint Condition",
                    &current,
                    submit,
                    workspace,
                    window,
                    cx,
                )
            })
            .ok();
    }

    fn ask_data_hit_condition(
        &mut self,
        data_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let current = self
            .data_breakpoints
            .iter()
            .find(|setting| setting.data_id == data_id)
            .and_then(|setting| setting.hit_condition.clone())
            .unwrap_or_default();
        let panel = cx.weak_entity();
        let submit = move |text: String, cx: &mut App| {
            panel
                .update(cx, |panel, cx| {
                    panel.set_data_hit_condition(data_id.clone(), text, cx)
                })
                .ok();
        };
        self.workspace
            .update(cx, |workspace, cx| {
                Prompt::show_with(
                    "Field Watchpoint Hit Count",
                    &current,
                    submit,
                    workspace,
                    window,
                    cx,
                )
            })
            .ok();
    }

    /// The state of the session for the right end of the toolbar. Nothing before the
    /// first session: the empty body says it in place.
    fn state_text(&self) -> Option<&str> {
        match &self.snapshot.state {
            DebugState::Inactive => None,
            DebugState::Starting => Some("Starting"),
            DebugState::Running => Some("Running"),
            DebugState::Stopped { reason } => Some(reason.as_str()),
            DebugState::Terminated => Some("Finished"),
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
        let body = if self.state().0 {
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
        } else {
            empty_pane("debug-empty", "Not started", theme)
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
        let body = if frames.is_empty() {
            empty_pane("debug-empty-frames", "No frames", theme)
        } else {
            let rows = frames
                .iter()
                .map(|frame| {
                    let selected = selected_frame == Some(frame.id);
                    let frame = frame.clone();
                    let frame_id = frame.id;
                    let menu_frame = frame.clone();
                    let where_ = frame
                        .source
                        .as_ref()
                        .and_then(|path| path.file_name())
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let text = if frame.line == 0 {
                        format!("{}  {}", frame.name, where_)
                    } else {
                        format!("{}  {}:{}", frame.name, where_, frame.line)
                    };
                    row(format!("frame-{}", frame.id), &text, selected, theme)
                        .debug_selector(move || format!("frame-{frame_id}"))
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
                })
                .collect::<Vec<AnyElement>>();
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .child(section("Frames", theme))
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
                None => format!("{} = {}", watch.expression, watch.value),
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

    /// The Console tab: the output of the build and of the program, and the line the
    /// program reads while it waits for input.
    fn render_console(
        &mut self,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut console_rows: Vec<AnyElement> = Vec::new();
        for line in &self.local_console {
            let color = if line.error {
                theme.warning_mark
            } else {
                theme.code
            };
            console_rows.push(
                div()
                    .px(px(SPACE_M))
                    .py(px(SPACE_XS))
                    .font_family(MONO_FONT)
                    .text_size(px(CODE_SIZE))
                    .text_color(color)
                    .child(line.text.clone())
                    .into_any_element(),
            );
        }
        if console_rows.is_empty() {
            console_rows.push(empty_line("debug-empty-console", "No output", theme));
        }
        let console = div()
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
            .children(console_rows);
        let scrolled = scrollable(
            "debug-console-scroll",
            &self.scroll_side,
            div()
                .id("debug-console-scroll")
                .flex()
                .flex_col()
                .overflow_y_scroll()
                .track_scroll(&self.scroll_side)
                .child(console),
            theme,
            window,
            cx,
        )
        .flex_1()
        .min_h_0();
        // The line the user types stays at the bottom of the console, so it is at hand
        // however far the output above has scrolled.
        let mut body = div()
            .debug_selector(|| "debug-console-body".to_owned())
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
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
                    .child(self.console_input.element("debug-console-input", theme)),
            );
        }
        body.into_any_element()
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
        rows.push(
            row(
                format!("scope-{reference}"),
                &format!("{}{}", indent(depth), scope.name),
                false,
                theme,
            )
            .debug_selector(move || format!("scope-{reference}"))
            .on_click(cx.listener(move |this, _, _, cx| this.expand(reference, cx)))
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
            variable.value
        );
        let name = variable.name.clone();
        let target = VariableTarget {
            reference: parent,
            name: name.clone(),
            value: variable.value.clone(),
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

    fn ask_evaluate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let panel = cx.weak_entity();
        let workspace = self.workspace.clone();
        let handle = window.window_handle();
        // Alt+F8 comes through the window while it is being updated.
        cx.defer(move |cx| {
            let panel = panel.clone();
            handle
                .update(cx, |_, window, cx| {
                    workspace
                        .update(cx, |workspace, cx| {
                            Prompt::show(
                                "Evaluate",
                                "Expression",
                                move |text, cx| {
                                    panel
                                        .update(cx, |panel, cx| {
                                            let expression = text.trim().to_owned();
                                            if !expression.is_empty() {
                                                panel.send(Command::Evaluate(expression), cx);
                                                cx.notify();
                                            }
                                        })
                                        .ok();
                                },
                                workspace,
                                window,
                                cx,
                            )
                        })
                        .ok();
                })
                .ok();
        });
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
                        .child(format!(
                            "{arrow}{}{type_name} = {}",
                            evaluation.expression, evaluation.value
                        ))
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
        let body = scrollable(
            "quick-evaluate",
            &self.scroll,
            surface(&theme)
                .id("quick-evaluate")
                .debug_selector(|| "quick-evaluate".to_owned())
                .occlude()
                .w(px(420.))
                .max_h(px(320.))
                .overflow_y_scroll()
                .track_scroll(&self.scroll)
                .p(px(POPUP_PADDING))
                .gap(px(2.))
                .text_size(px(CODE_SIZE))
                .on_mouse_down_out(cx.listener(|this, _, window, cx| this.close(window, cx)))
                .child(
                    div()
                        .px(px(SPACE_M))
                        .text_size(px(TEXT_SMALL))
                        .text_color(theme.text_secondary)
                        .child("Quick Evaluate"),
                )
                .children(rows),
            &theme,
            window,
            cx,
        );
        div()
            .absolute()
            .left(self.position.x)
            .top(self.position.y)
            .child(enter("quick-evaluate-enter", body))
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
        let body = surface(&theme)
            .id("step-targets")
            .debug_selector(|| "step-targets".to_owned())
            .track_focus(&self.focus)
            .key_context("StepTargets")
            .occlude()
            .w(px(360.))
            .p(px(SPACE_M))
            .gap(px(SPACE_S))
            .text_size(px(UI_SIZE))
            .on_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)))
            .on_mouse_down_out(cx.listener(|this, _, window, cx| this.close(window, cx)))
            .child(
                div()
                    .px(px(SPACE_M))
                    .text_size(px(TEXT_SMALL))
                    .text_color(theme.text_secondary)
                    .child(format!("Step Into ({total} calls)")),
            )
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
        div()
            .absolute()
            .left(left)
            .top(top)
            .child(enter("step-targets-enter", body))
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
        .px(px(ROW_PADDING_X))
        .cursor_pointer()
        .text_size(px(CODE_SIZE))
        .when(selected, |row| row.bg(theme.selection))
        .hover(|row| row.bg(theme.caret_row))
        .child(text.to_owned())
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
        let running = matches!(self.snapshot.state, DebugState::Running);
        let can_drop = self.restart_frame_enabled();
        let can_restart = self.restart.is_some();
        let workspace = self.workspace.clone();
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
                    active,
                    &theme,
                )
                .when(active, |button| {
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
                panel_header(self.title.clone(), GROUP, hide, &theme)
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
                setting.log_expression.clone().unwrap_or_default(),
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
        let body = surface(&theme)
            .id("breakpoint-popup")
            .debug_selector(|| "breakpoint-popup".to_owned())
            .track_focus(&self.focus)
            .occlude()
            .w(px(420.))
            .pb(px(SPACE_S))
            .on_mouse_down_out(cx.listener(|this, _, window, cx| this.close(window, cx)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .h(px(34.))
                    .px(px(SPACE_M))
                    .font_weight(FontWeight::MEDIUM)
                    .child(format!("Breakpoint at {name}:{line}")),
            )
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
        div()
            .absolute()
            .left(left)
            .top(top)
            .child(enter("breakpoint-popup-enter", body))
    }
}

struct BreakpointsView {
    panel: WeakEntity<DebugPanel>,
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    scroll: ScrollHandle,
}

impl BreakpointsView {
    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace
            .update(cx, |workspace, cx| workspace.dismiss(window, cx))
            .ok();
    }
}

impl Render for BreakpointsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let panel = self.panel.upgrade();
        let settings = panel
            .as_ref()
            .map(|panel| panel.read(cx).breakpoints.clone())
            .unwrap_or_default();
        let answers = panel
            .as_ref()
            .map(|panel| panel.read(cx).snapshot.breakpoints.clone())
            .unwrap_or_default();
        let function_settings = panel
            .as_ref()
            .map(|panel| panel.read(cx).function_breakpoints.clone())
            .unwrap_or_default();
        let data_settings = panel
            .as_ref()
            .map(|panel| panel.read(cx).data_breakpoints.clone())
            .unwrap_or_default();
        let function_answers = panel
            .as_ref()
            .map(|panel| panel.read(cx).snapshot.function_breakpoints.clone())
            .unwrap_or_default();
        let data_answers = panel
            .as_ref()
            .map(|panel| panel.read(cx).snapshot.data_breakpoints.clone())
            .unwrap_or_default();
        let filters = panel
            .as_ref()
            .map(|panel| panel.read(cx).snapshot.exception_filters.clone())
            .unwrap_or_default();
        let enabled_filters = panel
            .as_ref()
            .map(|panel| panel.read(cx).enabled_filters.clone())
            .unwrap_or_default();
        let mut rows: Vec<AnyElement> = Vec::new();
        for (index, setting) in settings.iter().enumerate() {
            let answer = answers
                .iter()
                .find(|answer| answer.file == setting.file && answer.line == setting.line);
            let message = answer.and_then(|answer| answer.message.clone());
            let verified = answer.is_none_or(|answer| answer.verified);
            let name = setting
                .file
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            let condition = setting.condition.clone().unwrap_or_default();
            let hit = setting.hit_condition.clone().unwrap_or_default();
            let on = setting.enabled;
            let suspend = setting.suspend;
            let hit_message = setting.hit_message;
            let log_expression = setting.log_expression.clone().unwrap_or_default();
            let line = setting.line;
            let enable_file = setting.file.clone();
            let condition_file = setting.file.clone();
            let hit_file = setting.file.clone();
            let suspend_file = setting.file.clone();
            let message_file = setting.file.clone();
            let log_file = setting.file.clone();
            let remove_file = setting.file.clone();
            let row = div()
                .id(SharedString::from(format!(
                    "dialog-breakpoint-{}-{line}",
                    setting.file.display()
                )))
                .debug_selector(move || format!("debug-breakpoint-row-{index}"))
                .flex()
                .items_center()
                .gap(px(8.))
                .h(px(ROW_HEIGHT))
                .child(
                    div()
                        .id(SharedString::from(format!(
                            "dialog-enabled-{}-{line}",
                            setting.file.display()
                        )))
                        .w(px(14.))
                        .cursor_pointer()
                        .child(if on { "☑" } else { "☐" })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(panel) = this.panel.upgrade() {
                                panel.update(cx, |panel, cx| {
                                    panel.set_enabled(enable_file.clone(), line, !on, cx)
                                });
                            }
                            cx.notify();
                        })),
                )
                .child(
                    div()
                        .w(px(210.))
                        .truncate()
                        .text_color(if on { theme.text } else { theme.text_disabled })
                        .child(format!("{name}:{line}")),
                )
                .child(
                    div()
                        .id(SharedString::from(format!(
                            "dialog-condition-{}-{line}",
                            setting.file.display()
                        )))
                        .debug_selector(|| "breakpoint-dialog-condition".to_owned())
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .cursor_pointer()
                        .text_color(theme.text_secondary)
                        .child(if condition.is_empty() {
                            "if: <none>".to_owned()
                        } else {
                            format!("if: {condition}")
                        })
                        .on_click(cx.listener(move |this, _, window, cx| {
                            let file = condition_file.clone();
                            let panel = this.panel.clone();
                            this.workspace
                                .update(cx, |workspace, cx| {
                                    Prompt::show_with(
                                        "Breakpoint Condition",
                                        &condition,
                                        move |text, cx| {
                                            panel
                                                .update(cx, |panel, cx| {
                                                    panel.set_condition(
                                                        file.clone(),
                                                        line,
                                                        text,
                                                        cx,
                                                    )
                                                })
                                                .ok();
                                        },
                                        workspace,
                                        window,
                                        cx,
                                    )
                                })
                                .ok();
                        })),
                )
                .child(
                    div()
                        .id(SharedString::from(format!(
                            "dialog-hit-{}-{line}",
                            setting.file.display()
                        )))
                        .w(px(110.))
                        .truncate()
                        .cursor_pointer()
                        .text_color(theme.text_secondary)
                        .child(if hit.is_empty() {
                            "hit: <any>".to_owned()
                        } else {
                            format!("hit: {hit}")
                        })
                        .on_click(cx.listener(move |this, _, window, cx| {
                            let file = hit_file.clone();
                            let panel = this.panel.clone();
                            let hit = hit.clone();
                            this.workspace
                                .update(cx, |workspace, cx| {
                                    Prompt::show_with(
                                        "Breakpoint Hit Count",
                                        &hit,
                                        move |text, cx| {
                                            panel
                                                .update(cx, |panel, cx| {
                                                    panel.set_hit_condition(
                                                        file.clone(),
                                                        line,
                                                        text,
                                                        cx,
                                                    )
                                                })
                                                .ok();
                                        },
                                        workspace,
                                        window,
                                        cx,
                                    )
                                })
                                .ok();
                        })),
                )
                .child(
                    div()
                        .id(SharedString::from(format!(
                            "dialog-suspend-{}-{line}",
                            setting.file.display()
                        )))
                        .debug_selector(|| "breakpoint-dialog-suspend".to_owned())
                        .w(px(14.))
                        .truncate()
                        .cursor_pointer()
                        .text_color(theme.text_secondary)
                        .child(if suspend {
                            "☑ suspend"
                        } else {
                            "☐ suspend"
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(panel) = this.panel.upgrade() {
                                panel.update(cx, |panel, cx| {
                                    panel.set_suspend(suspend_file.clone(), line, !suspend, cx)
                                });
                            }
                            cx.notify();
                        })),
                )
                .child(
                    div()
                        .id(SharedString::from(format!(
                            "dialog-hit-message-{}-{line}",
                            setting.file.display()
                        )))
                        .debug_selector(|| "breakpoint-dialog-hit-message".to_owned())
                        .w(px(48.))
                        .truncate()
                        .cursor_pointer()
                        .text_color(theme.text_secondary)
                        .child(if hit_message { "☑ msg" } else { "☐ msg" })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(panel) = this.panel.upgrade() {
                                panel.update(cx, |panel, cx| {
                                    panel.set_hit_message(
                                        message_file.clone(),
                                        line,
                                        !hit_message,
                                        cx,
                                    )
                                });
                            }
                            cx.notify();
                        })),
                )
                .child(
                    div()
                        .id(SharedString::from(format!(
                            "dialog-log-{}-{line}",
                            setting.file.display()
                        )))
                        .debug_selector(|| "breakpoint-dialog-log-expression".to_owned())
                        .w(px(100.))
                        .truncate()
                        .cursor_pointer()
                        .text_color(theme.text_secondary)
                        .child(if log_expression.is_empty() {
                            "log: <none>".to_owned()
                        } else {
                            format!("log: {log_expression}")
                        })
                        .on_click(cx.listener(move |this, _, window, cx| {
                            let file = log_file.clone();
                            if let Some(panel) = this.panel.upgrade() {
                                panel.update(cx, |panel, cx| {
                                    panel.ask_log_expression(file, line, window, cx)
                                });
                            }
                        })),
                )
                .child(
                    icon_button(
                        SharedString::from(format!(
                            "dialog-remove-{}-{line}",
                            setting.file.display()
                        )),
                        "general/closeSmall",
                        "Remove Breakpoint",
                        None,
                        &theme,
                    )
                    .size(px(20.))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(panel) = this.panel.upgrade() {
                            panel.update(cx, |panel, cx| {
                                panel.remove_breakpoint(remove_file.clone(), line, cx)
                            });
                        }
                        cx.notify();
                    })),
                );
            let row = if !verified && on {
                row.child(
                    div()
                        .text_color(theme.warning_mark)
                        .child(message.unwrap_or_else(|| "not accepted".into())),
                )
            } else {
                row
            };
            rows.push(row.into_any_element());
        }
        rows.push(section("Method breakpoints", &theme));
        for (index, setting) in function_settings.iter().enumerate() {
            let on = setting.enabled;
            let file = setting.file.clone();
            let line = setting.line;
            let enabled_file = file.clone();
            let condition_file = file.clone();
            let hit_file = file.clone();
            let remove_file = file.clone();
            let condition = setting.condition.clone().unwrap_or_default();
            let hit = setting.hit_condition.clone().unwrap_or_default();
            let answer = function_answers.get(index);
            let message = answer.and_then(|answer| answer.message.clone());
            let verified = answer.is_none_or(|answer| answer.verified);
            let row = div()
                .id(SharedString::from(format!(
                    "dialog-function-{}-{line}",
                    setting.file.display()
                )))
                .flex()
                .items_center()
                .gap(px(8.))
                .py(px(SPACE_S))
                .child(
                    div()
                        .id(SharedString::from(format!(
                            "dialog-function-enabled-{}-{line}",
                            setting.file.display()
                        )))
                        .w(px(14.))
                        .cursor_pointer()
                        .child(if on { "☑" } else { "☐" })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(panel) = this.panel.upgrade() {
                                panel.update(cx, |panel, cx| {
                                    panel.set_function_enabled(enabled_file.clone(), line, !on, cx)
                                });
                            }
                            cx.notify();
                        })),
                )
                .child(icon_sized("nodes/method", 14., &theme))
                .child(
                    div()
                        .w(px(210.))
                        .truncate()
                        .text_color(if on { theme.text } else { theme.text_disabled })
                        .child(setting.dap_name()),
                )
                .child(
                    div()
                        .id(SharedString::from(format!(
                            "dialog-function-condition-{}-{line}",
                            setting.file.display()
                        )))
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .cursor_pointer()
                        .text_color(theme.text_secondary)
                        .child(if condition.is_empty() {
                            "if: <none>".to_owned()
                        } else {
                            format!("if: {condition}")
                        })
                        .on_click(cx.listener(move |this, _, window, cx| {
                            let file = condition_file.clone();
                            if let Some(panel) = this.panel.upgrade() {
                                panel.update(cx, |panel, cx| {
                                    panel.ask_function_condition(file, line, window, cx)
                                });
                            }
                        })),
                )
                .child(
                    div()
                        .id(SharedString::from(format!(
                            "dialog-function-hit-{}-{line}",
                            setting.file.display()
                        )))
                        .w(px(110.))
                        .truncate()
                        .cursor_pointer()
                        .text_color(theme.text_secondary)
                        .child(if hit.is_empty() {
                            "hit: <any>".to_owned()
                        } else {
                            format!("hit: {hit}")
                        })
                        .on_click(cx.listener(move |this, _, window, cx| {
                            let file = hit_file.clone();
                            if let Some(panel) = this.panel.upgrade() {
                                panel.update(cx, |panel, cx| {
                                    panel.ask_function_hit_condition(file, line, window, cx)
                                });
                            }
                        })),
                )
                .child(
                    icon_button(
                        SharedString::from(format!(
                            "dialog-function-remove-{}-{line}",
                            setting.file.display()
                        )),
                        "general/closeSmall",
                        "Remove Method Breakpoint",
                        None,
                        &theme,
                    )
                    .size(px(20.))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(panel) = this.panel.upgrade() {
                            panel.update(cx, |panel, cx| {
                                panel.remove_function_breakpoint(remove_file.clone(), line, cx)
                            });
                        }
                        cx.notify();
                    })),
                );
            rows.push(if !verified && on {
                row.child(
                    div()
                        .text_color(theme.warning_mark)
                        .child(message.unwrap_or_else(|| "not accepted".into())),
                )
                .into_any_element()
            } else {
                row.into_any_element()
            });
        }
        rows.push(section("Field watchpoints", &theme));
        for (index, setting) in data_settings.iter().enumerate() {
            let on = setting.enabled;
            let data_id = setting.data_id.clone();
            let enabled_id = data_id.clone();
            let condition_id = data_id.clone();
            let hit_id = data_id.clone();
            let access_id = data_id.clone();
            let remove_id = data_id.clone();
            let condition = setting.condition.clone().unwrap_or_default();
            let hit = setting.hit_condition.clone().unwrap_or_default();
            let answer = data_answers.get(index);
            let message = answer.and_then(|answer| answer.message.clone());
            let verified = answer.is_none_or(|answer| answer.verified);
            let row = div()
                .id(SharedString::from(format!("dialog-watch-{data_id}")))
                .flex()
                .items_center()
                .gap(px(8.))
                .py(px(SPACE_S))
                .child(
                    div()
                        .id(SharedString::from(format!(
                            "dialog-watch-enabled-{data_id}"
                        )))
                        .w(px(14.))
                        .cursor_pointer()
                        .child(if on { "☑" } else { "☐" })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(panel) = this.panel.upgrade() {
                                panel.update(cx, |panel, cx| {
                                    panel.set_data_enabled(enabled_id.clone(), !on, cx)
                                });
                            }
                            cx.notify();
                        })),
                )
                .child(icon_sized("nodes/field", 14., &theme))
                .child(
                    div()
                        .w(px(180.))
                        .truncate()
                        .text_color(if on { theme.text } else { theme.text_disabled })
                        .child(data_id.clone()),
                )
                .child(
                    div()
                        .id(SharedString::from(format!("dialog-watch-access-{data_id}")))
                        .w(px(110.))
                        .cursor_pointer()
                        .text_color(theme.text_secondary)
                        .child(format!("on: {}", setting.access().label()))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(panel) = this.panel.upgrade() {
                                panel.update(cx, |panel, cx| {
                                    panel.cycle_data_access(access_id.clone(), cx)
                                });
                            }
                            cx.notify();
                        })),
                )
                .child(
                    div()
                        .id(SharedString::from(format!(
                            "dialog-watch-condition-{data_id}"
                        )))
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .cursor_pointer()
                        .text_color(theme.text_secondary)
                        .child(if condition.is_empty() {
                            "if: <none>".to_owned()
                        } else {
                            format!("if: {condition}")
                        })
                        .on_click(cx.listener(move |this, _, window, cx| {
                            let id = condition_id.clone();
                            if let Some(panel) = this.panel.upgrade() {
                                panel.update(cx, |panel, cx| {
                                    panel.ask_data_condition(id, window, cx)
                                });
                            }
                        })),
                )
                .child(
                    div()
                        .id(SharedString::from(format!("dialog-watch-hit-{data_id}")))
                        .w(px(90.))
                        .truncate()
                        .cursor_pointer()
                        .text_color(theme.text_secondary)
                        .child(if hit.is_empty() {
                            "hit: <any>".to_owned()
                        } else {
                            format!("hit: {hit}")
                        })
                        .on_click(cx.listener(move |this, _, window, cx| {
                            let id = hit_id.clone();
                            if let Some(panel) = this.panel.upgrade() {
                                panel.update(cx, |panel, cx| {
                                    panel.ask_data_hit_condition(id, window, cx)
                                });
                            }
                        })),
                )
                .child(
                    icon_button(
                        SharedString::from(format!("dialog-watch-remove-{data_id}")),
                        "general/closeSmall",
                        "Remove Field Watchpoint",
                        None,
                        &theme,
                    )
                    .size(px(20.))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(panel) = this.panel.upgrade() {
                            panel.update(cx, |panel, cx| {
                                panel.remove_data_breakpoint(remove_id.clone(), cx)
                            });
                        }
                        cx.notify();
                    })),
                );
            rows.push(if !verified && on {
                row.child(
                    div()
                        .text_color(theme.warning_mark)
                        .child(message.unwrap_or_else(|| "not accepted".into())),
                )
                .into_any_element()
            } else {
                row.into_any_element()
            });
        }
        rows.push(section("Exception breakpoints", &theme));
        if filters.is_empty() {
            rows.push(
                div()
                    .px(px(4.))
                    .pb(px(4.))
                    .text_color(theme.text_secondary)
                    .child("The adapter lists them when a session starts")
                    .into_any_element(),
            );
        }
        for filter in &filters {
            let on = enabled_filters.contains(&filter.filter);
            let id = filter.filter.clone();
            let label = filter.label.clone();
            rows.push(
                div()
                    .id(SharedString::from(format!("dialog-filter-{id}")))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .py(px(SPACE_S))
                    .cursor_pointer()
                    .child(if on { "☑" } else { "☐" })
                    .child(label)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(panel) = this.panel.upgrade() {
                            panel.update(cx, |panel, cx| panel.toggle_filter(id.clone(), cx));
                        }
                        cx.notify();
                    }))
                    .into_any_element(),
            );
        }
        let body = scrollable(
            "breakpoints-dialog",
            &self.scroll,
            surface(&theme)
                .id("breakpoints-dialog")
                .debug_selector(|| "breakpoints-dialog".to_owned())
                .track_focus(&self.focus)
                .occlude()
                .w(px(760.))
                .max_h(px(640.))
                .overflow_y_scroll()
                .track_scroll(&self.scroll)
                .p(px(DIALOG_PADDING))
                .gap(px(SPACE_M))
                .text_size(px(UI_SIZE))
                .text_color(theme.text)
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                    if event.keystroke.key == "escape" {
                        this.close(window, cx);
                        cx.stop_propagation();
                    }
                }))
                .on_mouse_down_out(cx.listener(|this, _, window, cx| this.close(window, cx)))
                .child(div().font_weight(FontWeight::SEMIBOLD).child("Breakpoints"))
                .children(rows)
                .child(
                    div().flex().justify_end().child(
                        tool_button(
                            "breakpoints-close",
                            "general/close",
                            "Close",
                            Some("Escape"),
                            true,
                            &theme,
                        )
                        .on_click(cx.listener(|this, _, window, cx| this.close(window, cx))),
                    ),
                ),
            &theme,
            window,
            cx,
        );
        let viewport = window.viewport_size();
        let width = px(760.);
        let left = (viewport.width - width) / 2.;
        let top = viewport.height * 0.15;
        div()
            .absolute()
            .left(left)
            .top(top)
            .child(enter("breakpoints-enter", body))
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
    view.set_breakpoints(lines, cx);
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
    use std::collections::BTreeSet;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use proc_sup::ProcessSpec;
    use ui_kit::gpui;
    use ui_kit::gpui::{AppContext, Entity, ScrollHandle, TestAppContext, WeakEntity};

    use super::{
        BreakpointSpec, ConsoleEvent, DataBreakpointSetting, DebugPanel, DebugService, Debugger,
        FunctionBreakpointSetting, OutputBuffers, RunPlan, SavedState, Snapshot,
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
                temporary: None,
                forced_step: None,
                seen_stops: 0,
                asking_targets: false,
                asking_targets_window: None,
                breakpoints: Vec::new(),
                function_breakpoints: Vec::new(),
                data_breakpoints: Vec::new(),
                enabled_filters: BTreeSet::new(),
                watch: Vec::new(),
                expanded: BTreeSet::new(),
                shown_breakpoints: Vec::new(),
                shown_function_breakpoints: Vec::new(),
                shown_data_breakpoints: Vec::new(),
                local_console: Vec::new(),
                seen_console: Vec::new(),
                console_input: crate::console_input::ConsoleInput::new(),
                build: None,
                build_output: OutputBuffers::default(),
                build_process: None,
                pending: None,
                restart: None,
                front: super::Front::Debugger,
                tree_width: super::TREE_WIDTH,
                split_from: None,
                session: None,
                store: None,
                root: PathBuf::new(),
                title: "Debug".into(),
                scroll_tree: ScrollHandle::new(),
                scroll_side: ScrollHandle::new(),
                scroll_variables: ScrollHandle::new(),
                scroll_watch: ScrollHandle::new(),
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
