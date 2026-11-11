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
    Breakpoint, BreakpointSpec, Command, ConsoleLine, DebugService, DebugSessionToken, DebugState,
    Debugger, Frame, Scope, Snapshot, StepTarget, Thread, Variable,
};
use runner::{Console, ConsoleEvent, RunPlan, Step, Stream};
use serde::{Deserialize, Serialize};
use store::Store;
use tool_gateway::DebugSnapshotSource;
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::{DEBUG_TOOL, Workspace};

use crate::navigation;
use crate::prompt::Prompt;

const CONTEXT: &str = "DebugPanel";

actions!(debug_panel, [NewWatch, DeleteWatch, SetValue, CancelInput]);

/// One key of the Debug tool window and the IDEA action it implements, if any.
#[cfg(test)]
pub(crate) struct DebugKey {
    pub(crate) keys: &'static str,
    pub(crate) action: &'static str,
    pub(crate) idea: Option<&'static str>,
}

macro_rules! debug_keys {
    ($(($keys:literal, $action:ident, $idea:expr)),* $(,)?) => {
        /// The keys of the Debug tool window, while it has the focus.
        fn bind_keys(cx: &mut App) {
            cx.bind_keys([$(KeyBinding::new($keys, $action, Some(CONTEXT))),*]);
        }

        #[cfg(test)]
        pub(crate) const KEYS: &[DebugKey] = &[
            $(DebugKey {
                keys: $keys,
                action: concat!("debug_panel::", stringify!($action)),
                idea: $idea,
            }),*
        ];
    };
}

debug_keys![
    ("insert", NewWatch, Some("XDebugger.NewWatch")),
    ("f2", SetValue, Some("XDebugger.SetValue")),
    ("delete", DeleteWatch, None),
    ("escape", CancelInput, None),
];

const POLL: Duration = Duration::from_millis(50);
const GROUP: &str = "bottom-panel";
const TREE_WIDTH: f32 = 460.;
const MAX_CONSOLE: usize = 2_000;
const EVENTS: usize = 2_000;
/// The marks of the virtual group in `expanded`: the group is open or it is not, and
/// the shown page. Variable references are positive, so these keys never collide.
const VIRTUAL_GROUP: i64 = -1;
const VIRTUAL_PAGE_BASE: i64 = -10;

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
    /// A breakpoint that goes away at the first stop.
    #[serde(default)]
    temporary: bool,
}

fn enabled() -> bool {
    true
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
            temporary: false,
        }
    }

    fn spec(&self) -> BreakpointSpec {
        BreakpointSpec {
            file: self.file.clone(),
            line: self.line,
            condition: self.condition.clone(),
            hit_condition: self.hit_condition.clone(),
            enabled: self.enabled && self.suspend,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct SavedState {
    breakpoints: Vec<BreakpointSetting>,
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
    /// Filters of the adapter the user turned on.
    enabled_filters: BTreeSet<String>,
    watch: Vec<String>,
    expanded: BTreeSet<i64>,
    /// The breakpoints of the last snapshot, to notice the adapter's answers.
    shown_breakpoints: Vec<Breakpoint>,
    /// Output of the build step and of a test run, before the adapter's own console.
    local_console: Vec<ConsoleLine>,
    /// The build step of a debug launch, before the debuggee starts.
    build: Option<Console>,
    pending: Option<PendingLaunch>,
    /// Held while a program this window launched is under the debugger; dropping it
    /// lets the language turn compilation on save back on.
    session: Option<Box<dyn DebugSessionToken>>,
    store: Option<Arc<Store>>,
    root: PathBuf,
    title: SharedString,
    scroll_tree: ScrollHandle,
    scroll_side: ScrollHandle,
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
            enabled_filters: saved.exception_filters.into_iter().collect(),
            watch: Vec::new(),
            expanded: BTreeSet::new(),
            shown_breakpoints: Vec::new(),
            local_console: Vec::new(),
            build: None,
            pending: None,
            session: None,
            store,
            root,
            title: "Debug".into(),
            scroll_tree: ScrollHandle::new(),
            scroll_side: ScrollHandle::new(),
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

    fn virtual_open(&self) -> bool {
        self.expanded.contains(&VIRTUAL_GROUP)
    }

    fn virtual_page(&self) -> usize {
        self.expanded
            .iter()
            .filter(|key| **key <= VIRTUAL_PAGE_BASE)
            .map(|key| (VIRTUAL_PAGE_BASE - *key) as usize)
            .max()
            .unwrap_or(0)
    }

    fn toggle_virtual_group(&mut self, cx: &mut Context<Self>) {
        if self.expanded.contains(&VIRTUAL_GROUP) {
            self.expanded.remove(&VIRTUAL_GROUP);
        } else {
            self.expanded.insert(VIRTUAL_GROUP);
        }
        cx.notify();
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
        let debugger = service.attach(
            host,
            port,
            name,
            project,
            self.specs(),
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
        let debugger = service.attach_ready(
            host,
            port,
            name,
            project,
            self.specs(),
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
        self.build = None;
        self.pending = None;
        self.session = None;
    }

    fn launch_pending(&mut self, cx: &mut Context<Self>) {
        let Some(pending) = self.pending.take() else {
            return;
        };
        match pending.service.start(
            pending.program,
            pending.name,
            pending.entry,
            self.specs(),
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
        let build_changed = self.poll_build(cx);
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
        let breakpoints_changed = snapshot.breakpoints != self.shown_breakpoints;
        self.shown_breakpoints = snapshot.breakpoints.clone();
        if let Some(hub) = cx.try_global::<ActiveDebugHub>() {
            hub.0.set(snapshot.clone());
        }
        // A snapshot is only sent when something changed; comparing two lists of ten
        // thousand threads here would be the work the grouping avoids.
        self.snapshot = (*snapshot).clone();
        if matches!(self.snapshot.state, DebugState::Terminated) {
            self.session = None;
        }
        // The temporary breakpoint lives only until the program stops anywhere. Only a
        // new stop counts: a snapshot of the stop they were set at leaves them be.
        if matches!(self.snapshot.state, DebugState::Stopped { .. })
            && self.snapshot.stops != self.seen_stops
        {
            self.seen_stops = self.snapshot.stops;
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
        for _ in 0..EVENTS {
            let Ok(event) = console.events().try_recv() else {
                break;
            };
            changed = true;
            match event {
                ConsoleEvent::Output { stream, text } => {
                    lines.push((text, stream == Stream::Stderr));
                }
                ConsoleEvent::Started { .. } => {}
                ConsoleEvent::Failed { message } => lines.push((message, true)),
                ConsoleEvent::Exited { code, step } => {
                    exit = Some((code, step));
                    break;
                }
                ConsoleEvent::Closed => break,
            }
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
        let extra = self.local_console.len().saturating_sub(MAX_CONSOLE);
        if extra > 0 {
            self.local_console.drain(..extra);
        }
        cx.notify();
    }

    pub fn resume(&mut self, _cx: &mut Context<Self>) {
        self.send(Command::Continue, _cx);
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

    fn ask_hit_condition(
        &mut self,
        file: PathBuf,
        line: u32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let current = self
            .setting(&file, line)
            .and_then(|setting| setting.hit_condition.clone())
            .unwrap_or_default();
        let panel = cx.weak_entity();
        let submit = move |text: String, cx: &mut App| {
            panel
                .update(cx, |panel, cx| {
                    panel.set_hit_condition(file.clone(), line, text, cx)
                })
                .ok();
        };
        self.workspace
            .update(cx, |workspace, cx| {
                Prompt::show_with(
                    "Breakpoint Hit Count",
                    &current,
                    submit,
                    workspace,
                    window,
                    cx,
                )
            })
            .ok();
    }

    fn render_tree(
        &mut self,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut rows: Vec<AnyElement> = Vec::new();
        rows.push(section("Threads", theme));
        let threads = dap_client::thread_list(
            &self.snapshot.threads,
            self.snapshot.current_thread,
            self.virtual_page(),
        );
        if let Some(thread) = &threads.stopped {
            rows.push(self.thread_row(thread, true, theme, cx));
        }
        if !threads.platform.is_empty() {
            rows.push(
                list_header(
                    "threads-platform",
                    &format!("Platform threads ({})", threads.platform.len()),
                    theme,
                )
                .into_any_element(),
            );
            for thread in &threads.platform {
                rows.push(self.thread_row(thread, false, theme, cx));
            }
        }
        if threads.virtual_threads.total > 0 {
            let open = self.virtual_open();
            let label = format!(
                "{} {}",
                if open { "▾" } else { "▸" },
                threads.virtual_threads.label()
            );
            rows.push(
                list_header("threads-virtual", &label, theme)
                    .cursor_pointer()
                    .debug_selector(|| "threads-virtual".to_owned())
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_virtual_group(cx)))
                    .into_any_element(),
            );
            if open {
                for thread in &threads.virtual_threads.threads {
                    rows.push(self.thread_row(thread, false, theme, cx));
                }
                if threads.virtual_threads.has_pages() {
                    rows.push(self.virtual_pager(&threads.virtual_threads, theme, cx));
                }
            }
        }
        rows.push(section("Frames", theme));
        for frame in &self.snapshot.frames {
            let selected = self.snapshot.selected_frame == Some(frame.id);
            let frame = frame.clone();
            let frame_id = frame.id;
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
            rows.push(
                row(format!("frame-{}", frame.id), &text, selected, theme)
                    .debug_selector(move || format!("frame-{frame_id}"))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.send(Command::SelectFrame(frame.id), cx);
                        this.open_frame(&frame, window, cx);
                        cx.notify();
                    }))
                    .into_any_element(),
            );
        }
        rows.push(section("Variables", theme));
        for scope in &self.snapshot.scopes {
            self.push_scope(scope, theme, 0, &mut rows, cx);
        }
        rows.push(section("Watch", theme));
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
        if let Some(field) = &self.watch_input {
            rows.push(
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
                    ))
                    .into_any_element(),
            );
        }
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
        .w(px(TREE_WIDTH))
        .flex_none()
        .h_full()
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
            let selector_name = name.clone();
            let row = row(format!("var-{name}"), &text, selected, theme)
                .debug_selector(move || format!("var-{selector_name}"))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.selected_variable = Some(click_target.clone());
                    if reference != 0 {
                        this.toggle_expanded(reference, cx);
                    }
                    cx.notify();
                }));
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

    fn thread_row(
        &self,
        thread: &Thread,
        selected: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = thread.id;
        row(
            format!("thread-{id}"),
            &format!("{}  #{}", thread.name, thread.id),
            selected,
            theme,
        )
        .debug_selector(move || format!("thread-{id}"))
        .on_click(cx.listener(move |this, _, _, cx| {
            this.send(Command::SelectThread(id), cx);
            cx.notify();
        }))
        .into_any_element()
    }

    fn virtual_pager(
        &self,
        threads: &dap_client::VirtualThreads,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let page = threads.page;
        let pages = threads.pages;
        let mut pager = div()
            .id("threads-virtual-pager")
            .debug_selector(|| "threads-virtual-pager".to_owned())
            .flex()
            .items_center()
            .gap(px(8.))
            .px(px(24.))
            .py(px(2.))
            .text_size(px(TEXT_SMALL))
            .text_color(theme.text_secondary)
            .child(format!("page {}/{}", page + 1, pages));
        if page > 0 {
            pager = pager.child(
                div()
                    .id("threads-virtual-prev")
                    .debug_selector(|| "threads-virtual-prev".to_owned())
                    .cursor_pointer()
                    .child("‹ previous")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_virtual_page(page - 1, cx);
                    })),
            );
        }
        if page + 1 < pages {
            pager = pager.child(
                div()
                    .id("threads-virtual-next")
                    .debug_selector(|| "threads-virtual-next".to_owned())
                    .cursor_pointer()
                    .child("next ›")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_virtual_page(page + 1, cx);
                    })),
            );
        }
        pager.into_any_element()
    }

    fn render_side(
        &mut self,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut rows: Vec<AnyElement> = Vec::new();
        rows.push(section("Console", theme));
        let total = self.local_console.len() + self.snapshot.console.len();
        let skip = total.saturating_sub(MAX_CONSOLE);
        let lines = self
            .local_console
            .iter()
            .chain(self.snapshot.console.iter())
            .skip(skip);
        for line in lines {
            let color = if line.error {
                theme.warning_mark
            } else {
                theme.code
            };
            rows.push(
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
        rows.push(section("Breakpoints", theme));
        for (index, breakpoint) in self.breakpoints.iter().enumerate() {
            rows.push(
                div()
                    .debug_selector(move || format!("debug-breakpoint-{index}"))
                    .child(self.breakpoint_row(breakpoint, theme, cx))
                    .into_any_element(),
            );
        }
        scrollable(
            "debug-side",
            &self.scroll_side,
            div()
                .id("debug-side")
                .flex()
                .flex_col()
                .overflow_y_scroll()
                .track_scroll(&self.scroll_side)
                .children(rows),
            theme,
            window,
            cx,
        )
        .flex_1()
        .min_w_0()
        .h_full()
        .into_any_element()
    }

    fn breakpoint_row(
        &self,
        breakpoint: &BreakpointSetting,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let answer = self
            .snapshot
            .breakpoints
            .iter()
            .find(|answer| answer.file == breakpoint.file && answer.line == breakpoint.line);
        let message = answer.and_then(|answer| answer.message.clone());
        let verified = answer.is_none_or(|answer| answer.verified);
        let color = if breakpoint.enabled && !verified {
            theme.warning_mark
        } else if breakpoint.enabled {
            theme.code
        } else {
            theme.text_disabled
        };
        let name = breakpoint
            .file
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let line = breakpoint.line;
        let condition = breakpoint.condition.clone().unwrap_or_default();
        let hit = breakpoint.hit_condition.clone().unwrap_or_default();
        let on = breakpoint.enabled;
        let enabled_file = breakpoint.file.clone();
        let condition_file = breakpoint.file.clone();
        let hit_file = breakpoint.file.clone();
        let remove_file = breakpoint.file.clone();
        let row = div()
            .id(SharedString::from(format!(
                "breakpoint-{}-{line}",
                breakpoint.file.display()
            )))
            .flex()
            .items_center()
            .gap(px(SPACE_S))
            .h(px(ROW_HEIGHT))
            .px(px(ROW_PADDING_X))
            .text_color(color)
            .child(
                div()
                    .id(SharedString::from(format!(
                        "breakpoint-enabled-{}-{line}",
                        breakpoint.file.display()
                    )))
                    .w(px(12.))
                    .cursor_pointer()
                    .child(if on { "☑" } else { "☐" })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_enabled(enabled_file.clone(), line, !on, cx);
                    })),
            )
            .child(div().flex_none().child(format!("{name}:{line}")))
            .child(
                div()
                    .id(SharedString::from(format!(
                        "breakpoint-condition-{}-{line}",
                        breakpoint.file.display()
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
                        this.ask_condition(condition_file.clone(), line, window, cx);
                    })),
            )
            .child(
                div()
                    .id(SharedString::from(format!(
                        "breakpoint-hit-{}-{line}",
                        breakpoint.file.display()
                    )))
                    .flex_none()
                    .cursor_pointer()
                    .text_color(theme.text_secondary)
                    .child(if hit.is_empty() {
                        "hit: <any>".to_owned()
                    } else {
                        format!("hit: {hit}")
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.ask_hit_condition(hit_file.clone(), line, window, cx);
                    })),
            )
            .child(
                icon_button(
                    SharedString::from(format!(
                        "breakpoint-remove-{}-{line}",
                        breakpoint.file.display()
                    )),
                    "general/closeSmall",
                    "Remove Breakpoint",
                    None,
                    theme,
                )
                .size(px(20.))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.remove_breakpoint(remove_file.clone(), line, cx);
                })),
            );
        let row = match message {
            Some(message) => row.child(
                div()
                    .text_color(theme.warning_mark)
                    .truncate()
                    .child(message),
            ),
            None => row,
        };
        row.into_any_element()
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

/// A row that names a group of threads. It is not selectable; the group row adds its
/// own click when it can be opened.
fn list_header(id: impl Into<ElementId>, text: &str, theme: &Theme) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .h(px(ROW_HEIGHT))
        .px(px(SPACE_XL))
        .text_size(px(TEXT_SMALL))
        .text_color(theme.text_secondary)
        .child(text.to_owned())
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

fn tool_button(
    id: &'static str,
    label: &'static str,
    hint: Option<&'static str>,
    enabled: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let shortcut = hint.map(Into::into);
    let button = button_frame(id, label, shortcut, theme)
        .debug_selector(move || id.to_owned())
        .child(
            div()
                .px(px(SPACE_S))
                .text_size(px(TEXT_SMALL))
                .child(label.to_owned()),
        );
    if enabled { button } else { button.opacity(0.4) }
}

impl Render for DebugPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let (active, stopped) = self.state();
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
                tool_button("debug-continue", "Resume", Some("F9"), stopped, &theme)
                    .on_click(cx.listener(|this, _, _, cx| this.resume(cx))),
            )
            .child(
                tool_button("debug-over", "Step Over", Some("F8"), stopped, &theme)
                    .on_click(cx.listener(|this, _, _, cx| this.step_over(cx))),
            )
            .child(
                tool_button("debug-into", "Step Into", Some("F7"), stopped, &theme)
                    .on_click(cx.listener(|this, _, _, cx| this.step_into(cx))),
            )
            .child(
                tool_button("debug-out", "Step Out", Some("Shift+F8"), stopped, &theme)
                    .on_click(cx.listener(|this, _, _, cx| this.step_out(cx))),
            )
            .child(
                tool_button("debug-stop", "Stop", Some("Ctrl+F2"), active, &theme)
                    .on_click(cx.listener(|this, _, _, cx| this.stop(cx))),
            )
            .child(
                tool_button(
                    "debug-evaluate",
                    "Evaluate",
                    Some("Alt+F8"),
                    stopped,
                    &theme,
                )
                .on_click(cx.listener(|this, _, window, cx| this.ask_evaluate(window, cx))),
            )
            .child(
                tool_button("debug-watch", "Add Watch", None, true, &theme)
                    .on_click(cx.listener(|this, _, window, cx| this.ask_watch(window, cx))),
            )
            .child(
                tool_button(
                    "debug-breakpoints",
                    "Breakpoints",
                    Some("Ctrl+Shift+F8"),
                    true,
                    &theme,
                )
                .on_click(cx.listener(|this, _, window, cx| this.show_breakpoints(window, cx))),
            );
        let state = match &self.snapshot.state {
            DebugState::Inactive => "Not started",
            DebugState::Starting => "Starting",
            DebugState::Running => "Running",
            DebugState::Stopped { reason } => reason.as_str(),
            DebugState::Terminated => "Finished",
        };
        let state = state.to_owned();
        let body = div()
            .flex()
            .flex_1()
            .min_h_0()
            .child(self.render_tree(&theme, window, cx))
            .child(div().w(px(1.)).h_full().bg(theme.popup_border))
            .child(self.render_side(&theme, window, cx));
        island(&theme)
            .id("debug-panel")
            .key_context(CONTEXT)
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
                    .px(px(SPACE_M))
                    .pb(px(SPACE_XS))
                    .text_size(px(TEXT_SMALL))
                    .text_color(theme.line_number)
                    .child(state.to_owned()),
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
        let (condition, enabled, suspend) = match &setting {
            Some(setting) => (
                setting.condition.clone().unwrap_or_default(),
                setting.enabled,
                setting.suspend,
            ),
            None => (String::new(), false, false),
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
        let condition_text = if condition.is_empty() {
            "Condition: <none>".to_owned()
        } else {
            format!("Condition: {condition}")
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
                    format!("Suspend: {}", if suspend { "on" } else { "off" }),
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
        let filters = panel
            .as_ref()
            .map(|panel| panel.read(cx).snapshot.exception_filters.clone())
            .unwrap_or_default();
        let enabled_filters = panel
            .as_ref()
            .map(|panel| panel.read(cx).enabled_filters.clone())
            .unwrap_or_default();
        let mut rows: Vec<AnyElement> = Vec::new();
        for setting in &settings {
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
            let line = setting.line;
            let enable_file = setting.file.clone();
            let condition_file = setting.file.clone();
            let hit_file = setting.file.clone();
            let remove_file = setting.file.clone();
            let row = div()
                .id(SharedString::from(format!(
                    "dialog-breakpoint-{}-{line}",
                    setting.file.display()
                )))
                .flex()
                .items_center()
                .gap(px(8.))
                .py(px(SPACE_S))
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
                        tool_button("breakpoints-close", "Close", Some("Escape"), true, &theme)
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
    let temporary = panel.read(cx).temporary_breakpoint_lines(view.path());
    let temporary: Vec<u32> = temporary
        .into_iter()
        .filter(|line| *line > 0)
        .map(|line| line - 1)
        .collect();
    view.set_temporary_breakpoints(temporary, cx);
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

#[cfg(test)]
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
        BreakpointSpec, ConsoleEvent, DebugPanel, DebugService, Debugger, RunPlan, Snapshot,
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
                enabled_filters: BTreeSet::new(),
                watch: Vec::new(),
                expanded: BTreeSet::new(),
                shown_breakpoints: Vec::new(),
                local_console: Vec::new(),
                build: None,
                pending: None,
                session: None,
                store: None,
                root: PathBuf::new(),
                title: "Debug".into(),
                scroll_tree: ScrollHandle::new(),
                scroll_side: ScrollHandle::new(),
            })
        })
    }

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
