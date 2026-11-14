//! State and behaviour of the main window. What it looks like is in `chrome`.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use keymap::{
    ActionRegistry, Dispatch, KEYMAP_FILE, KeyDispatcher, Keymap, KeymapFile, Keystroke,
    Modifiers as HeldModifiers, Shortcut,
};
use store::Store;
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace_core::CommandRegistry;

use crate::about::AboutView;
use crate::chrome::{ChromeView, EditorChromeSignal, StatusBarView, TabBarView};
use crate::editor::{EditorFactory, EditorStatus, FileIcons};
use crate::launch::{AppInfo, Launch};
use crate::layout::{
    DockState, Layout, PaneState, WindowPlacement, has_saved_layout, save_dark_theme, save_layout,
};
use crate::pane::{Pane, Tab, active_after_removal, next_tab, previous_tab, remember_closed};
use crate::run::{DebugControls, DebugRunState, RunControls, RunState};
use crate::settings::{DefaultLayout, SETTINGS_FILE, Settings};

/// Where the README that explains the setup of a language server lives.
const README_URL: &str = "https://github.com/PabloSensei/arro";

/// The command of a second search for a tool that a language server needs.
pub const CHECK_LANGUAGE_SETUP: &str = "language.check_setup";

actions!(
    workspace,
    [
        ToggleTheme,
        ToggleProject,
        ToggleTerminal,
        ToggleRun,
        ToggleAi,
        ToggleCommit,
        ToggleStructure,
        ToggleServices,
        ToggleLog,
        Run,
        Stop,
        Debug,
        BuildProject,
        StepOver,
        StepInto,
        StepOut,
        Resume,
        Evaluate,
        ToggleBreakpoint,
        ViewBreakpoints,
        ToggleDebug,
        HideToolWindow,
        // The window only declares these, whoever owns the repository handles them.
        ShowCommit,
        PushChanges,
        UpdateProject,
        RevertChange,
        ShowBranches,
        ShowSearch,
        Dismiss,
        NextTab,
        CloseTab,
        SplitRight,
        SplitDown,
        MoveToOtherGroup,
        FocusNextGroup,
        FocusPreviousGroup,
        ResizeToolWindowLeft,
        ResizeToolWindowRight,
        ResizeToolWindowUp,
        ResizeToolWindowDown,
        FullyExpandTreeNode,
        ReopenTab,
        TabLeft,
        TabRight,
        PinTab,
        SaveAll,
        OpenFolder,
        // The window only declares it, the project tree selects the file.
        SelectInProject,
        ToggleProblems,
        ToggleUsages,
        ToggleTodo,
        // The window only declares it, the settings window is in `panels`.
        ShowSettings,
        // The window only declares these, the problems window moves the caret.
        NextProblem,
        PreviousProblem,
        ForceStepInto,
        SmartStepInto,
        QuickEvaluate,
        RunToCursor,
        ForceRunToCursor,
        ShowExecutionPoint,
        ChooseRunConfiguration,
        ChooseDebugConfiguration,
        Rerun,
        // The IDE actions of M11 added after the first batch.
        Compile,
        RerunTests,
        StopBackgroundProcesses,
        Synchronize,
        VcsQuickList,
        ForceStepOver,
        ToggleTemporaryLineBreakpoint,
        EditBreakpoint,
        NewElementSamePlace,
        CopyElement,
        GotoRelated,
        NextProjectWindow,
        PreviousProjectWindow,
        // The window only declares it, the menu of the screen of macOS runs it.
        QuitApp,
        RerunInspection,
        // Project Structure, Ctrl+Alt+Shift+S: the dialog is in `panels`.
        ProjectStructure,
        RunWithCoverage,
        RunTestsWithCoverage,
        HideCoverage,
        ToggleCoverage,
        // The HTTP Response window, opened automatically when a request runs.
        ToggleHttp,
    ]
);

/// Told about a new keymap, so that the open editors take it.
type KeymapListener = Rc<dyn Fn(&mut App, Rc<Keymap>)>;

/// Opens a project in a window of its own.
pub type ProjectOpener = Rc<dyn Fn(&Path, &mut App)>;

/// One key of the window and the IDEA action it implements, if any.
pub type WindowKey = crate::keys::KeyRow;

/// The window keys and, from the same rows, the table the keymap check reads.
macro_rules! window_keys {
    ($(($keys:literal, $action:ident, $context:expr, $idea:expr, $mac:expr)),* $(,)?) => {
        pub fn bind_keys(cx: &mut App) {
            bind_keys_for(cx, keymap::platform());
        }

        /// Binds the keys of one platform. A test reads the macOS table on every runner.
        pub fn bind_keys_for(cx: &mut App, platform: keymap::Platform) {
            $(
                let row = WindowKey {
                    keys: $keys,
                    mac: $mac,
                    action: concat!("workspace::", stringify!($action)),
                    context: $context,
                    idea: $idea,
                };
                for key in row.keys_for(platform) {
                    cx.bind_keys([KeyBinding::new(key, $action, $context)]);
                }
            )*
            // Every application of macOS quits with Command+Q, from its own menu.
            #[cfg(target_os = "macos")]
            cx.bind_keys([KeyBinding::new("cmd-q", QuitApp, None)]);
        }

        /// The same bindings as [`bind_keys`], for the keymap check.
        pub const WINDOW_KEYS: &[WindowKey] = &[
            $(WindowKey {
                keys: $keys,
                mac: $mac,
                action: concat!("workspace::", stringify!($action)),
                context: $context,
                idea: $idea,
            }),*
        ];
    };
}

#[rustfmt::skip]
window_keys![
    ("alt-1", ToggleProject, None, Some("ActivateProjectToolWindow"), None),
    ("alt-2", ToggleAi, None, None, Some("cmd-2")),
    ("alt-3", ToggleUsages, None, Some("ActivateFindToolWindow"), None),
    ("alt-4", ToggleRun, None, Some("ActivateRunToolWindow"), None),
    ("alt-5", ToggleDebug, None, Some("ActivateDebugToolWindow"), None),
    ("alt-6", ToggleProblems, None, Some("ActivateProblemsViewToolWindow"), None),
    ("alt-7", ToggleStructure, None, Some("ActivateStructureToolWindow"), None),
    ("alt-8", ToggleServices, None, Some("ActivateServicesToolWindow"), None),
    ("alt-9", ToggleLog, None, Some("ActivateVersionControlToolWindow"), None),
    ("alt-0", ToggleCommit, None, Some("ActivateCommitToolWindow"), None),
    ("alt-f12", ToggleTerminal, None, Some("ActivateTerminalToolWindow"), None),
    ("ctrl-k", ShowCommit, None, None, Some("cmd-k")),
    ("ctrl-shift-k", PushChanges, None, None, Some("cmd-shift-k")),
    ("ctrl-t", UpdateProject, None, None, Some("cmd-t")),
    ("shift-f10", Run, None, Some("Run"), None),
    ("shift-f9", Debug, None, Some("Debug"), None),
    ("ctrl-f9", BuildProject, None, Some("CompileDirty"), None),
    ("f8", StepOver, None, Some("StepOver"), None),
    ("f7", StepInto, None, Some("StepInto"), None),
    ("shift-f8", StepOut, None, Some("StepOut"), None),
    ("f9", Resume, None, Some("Resume"), None),
    ("alt-f8", Evaluate, None, Some("EvaluateExpression"), None),
    ("ctrl-f8", ToggleBreakpoint, None, Some("ToggleLineBreakpoint"), None),
    ("ctrl-shift-f8", EditBreakpoint, None, Some("EditBreakpoint"), None),
    ("ctrl-f2", Stop, None, Some("Stop"), None),
    ("escape", Dismiss, None, None, Some("escape")),
    ("shift-escape", HideToolWindow, None, Some("HideActiveWindow"), None),
    ("ctrl-f4", CloseTab, None, Some("CloseContent"), None),
    ("alt-left", TabLeft, None, Some("PreviousTab"), None),
    ("alt-right", TabRight, None, Some("NextTab"), None),
    ("ctrl-alt-shift-right", FocusNextGroup, None, None, Some("ctrl-alt-shift-right")),
    ("ctrl-alt-shift-left", FocusPreviousGroup, None, None, Some("ctrl-alt-shift-left")),
    ("ctrl-s", SaveAll, Some("Workspace"), Some("SaveAll"), None),
    ("alt-f1", SelectInProject, None, Some("SelectIn"), None),
    ("ctrl-alt-s", ShowSettings, None, Some("ShowSettings"), None),
    ("f2", NextProblem, Some("Workspace"), Some("GotoNextError"), None),
    ("shift-f2", PreviousProblem, Some("Workspace"), Some("GotoPreviousError"), None),
    ("alt-shift-f10", ChooseRunConfiguration, None, Some("ChooseRunConfiguration"), None),
    ("alt-shift-f9", ChooseDebugConfiguration, None, Some("ChooseDebugConfiguration"), None),
    ("ctrl-f5", Rerun, None, Some("Rerun"), None),
    ("alt-shift-f7", ForceStepInto, None, Some("ForceStepInto"), None),
    ("shift-f7", SmartStepInto, None, Some("SmartStepInto"), None),
    ("alt-f9", RunToCursor, None, Some("RunToCursor"), None),
    ("ctrl-alt-f9", ForceRunToCursor, None, Some("ForceRunToCursor"), None),
    ("alt-f10", ShowExecutionPoint, None, Some("ShowExecutionPoint"), None),
    ("ctrl-alt-f8", QuickEvaluate, None, Some("QuickEvaluateExpression"), None),
    ("ctrl-shift-f9", Compile, None, Some("Compile"), None),
    ("alt-shift-r", RerunTests, None, Some("RerunTests"), None),
    ("ctrl-shift-f2", StopBackgroundProcesses, None, Some("StopBackgroundProcesses"), None),
    ("ctrl-alt-y", Synchronize, None, Some("Synchronize"), None),
    ("alt-`", VcsQuickList, None, Some("Vcs.QuickListPopupAction"), None),
    ("alt-shift-f8", ForceStepOver, None, Some("ForceStepOver"), None),
    ("ctrl-alt-shift-f8", ToggleTemporaryLineBreakpoint, None, Some("ToggleTemporaryLineBreakpoint"), None),
    ("ctrl-alt-insert", NewElementSamePlace, None, Some("NewElementSamePlace"), None),
    ("f5", CopyElement, None, Some("CopyElement"), None),
    ("ctrl-alt-home", GotoRelated, None, Some("GotoRelated"), None),
    ("ctrl-alt-]", NextProjectWindow, None, Some("NextProjectWindow"), None),
    ("ctrl-alt-[", PreviousProjectWindow, None, Some("PreviousProjectWindow"), None),
    ("ctrl-f5", RerunInspection, Some("InspectionResults"), None, Some("cmd-f5")),
    ("ctrl-alt-shift-s", ProjectStructure, None, Some("ShowProjectStructureSettings"), None),
    ("ctrl-shift-`", ShowBranches, None, None, Some("cmd-shift-`")),
    ("ctrl-alt-shift-left", ResizeToolWindowLeft, Some("ToolWindow"), Some("ResizeToolWindowLeft"), None),
    ("ctrl-alt-shift-right", ResizeToolWindowRight, Some("ToolWindow"), Some("ResizeToolWindowRight"), None),
    ("ctrl-alt-shift-up", ResizeToolWindowUp, Some("ToolWindow"), Some("ResizeToolWindowUp"), None),
    ("ctrl-alt-shift-down", ResizeToolWindowDown, Some("ToolWindow"), Some("ResizeToolWindowDown"), None),
    ("ctrl-shift-left", ResizeToolWindowLeft, Some("ToolWindow"), None, Some("ctrl-shift-left")),
    ("ctrl-shift-right", ResizeToolWindowRight, Some("ToolWindow"), None, Some("ctrl-shift-right")),
    ("ctrl-shift-up", ResizeToolWindowUp, Some("ToolWindow"), None, Some("ctrl-shift-up")),
    ("ctrl-shift-down", ResizeToolWindowDown, Some("ToolWindow"), None, Some("ctrl-shift-down")),
    ("numpad_multiply", FullyExpandTreeNode, Some("ToolWindow"), Some("FullyExpandTreeNode"), None),
    ("shift-numpad_add", FullyExpandTreeNode, Some("ToolWindow"), None, Some("shift-numpad_add")),
    ("ctrl-f5", Synchronize, Some("ProjectPanel"), Some("Refresh"), None),
    ("ctrl-shift-f5", Synchronize, Some("ProjectPanel"), Some("ForceRefresh"), None),
    ("ctrl-alt-f6", ToggleCoverage, None, Some("SwitchCoverage"), None),
];

/// The first key of a window action on one platform, `ctrl-f9`, or `None` when the
/// window has no row for it.
pub fn window_key(action: &str, platform: keymap::Platform) -> Option<&'static str> {
    WINDOW_KEYS
        .iter()
        .find(|binding| binding.action == action)
        .and_then(|binding| binding.keys_for(platform).first().copied())
}

/// The index of the window a project switch goes to. `None` when there is no other one.
pub fn window_step(len: usize, current: usize, step: isize) -> Option<usize> {
    if len < 2 || current >= len {
        return None;
    }
    let len = len as isize;
    Some(((current as isize + step).rem_euclid(len)) as usize)
}

const SAVE_DELAY: Duration = Duration::from_millis(300);
/// Files the edited list keeps.
const RECENT_EDITED_LIMIT: usize = 50;
/// The automatic update check waits for the window to settle first.
const UPDATE_DELAY: Duration = Duration::from_secs(10);
/// After the check at the start, a window that stays open asks again this often.
const UPDATE_INTERVAL: Duration = Duration::from_secs(12 * 60 * 60);
const LAST_CHECK_KEY: &str = "updates.last_check";
const SKIPPED_KEY: &str = "updates.skipped_version";
/// A submenu opens only after the pointer rests on its row, as IDEA does.
const MENU_OPEN_DELAY: Duration = Duration::from_millis(220);

/// The build the distribution README names: the commit and its date.
pub fn build_version() -> &'static str {
    env!("IDE_BUILD_VERSION")
}

pub fn build_date() -> &'static str {
    env!("IDE_BUILD_DATE")
}

/// How many times the window has drawn a change of its own state, for tests that check
/// what a keystroke does not redraw. GPUI redraws the root view when any view under it
/// repaints, so a plain count of renders would also include the editor's own frames.
#[cfg(any(test, feature = "test-support"))]
#[derive(Default)]
pub struct WindowRenderCount(pub Cell<usize>);

#[cfg(any(test, feature = "test-support"))]
impl Global for WindowRenderCount {}

/// The number of draws that showed a change of the window's own state, for tests.
#[cfg(any(test, feature = "test-support"))]
pub fn window_render_count(cx: &App) -> usize {
    cx.try_global::<WindowRenderCount>()
        .map_or(0, |count| count.0.get())
}

/// How many times each heavy view drew itself, by the name of the view, for tests that
/// check which changes rebuild a cached view and which do not.
#[cfg(any(test, feature = "test-support"))]
#[derive(Default)]
pub struct ViewRenderCounts(pub RefCell<HashMap<&'static str, usize>>);

#[cfg(any(test, feature = "test-support"))]
impl Global for ViewRenderCounts {}

/// Counts one draw of the view with this name, for tests.
#[cfg(any(test, feature = "test-support"))]
pub fn note_view_render(cx: &mut App, view: &'static str) {
    let counts = cx.default_global::<ViewRenderCounts>();
    *counts.0.borrow_mut().entry(view).or_default() += 1;
}

/// The number of draws of the view with this name, for tests.
#[cfg(any(test, feature = "test-support"))]
pub fn view_render_count(cx: &App, view: &str) -> usize {
    cx.try_global::<ViewRenderCounts>()
        .and_then(|counts| counts.0.borrow().get(view).copied())
        .unwrap_or(0)
}

/// `ctrl-shift-n` as the menus write it: `Ctrl+Shift+N`, or `⇧⌘N` on macOS.
fn shortcut_label(text: &str) -> String {
    keymap::shortcut_label(text, keymap::platform())
}
pub const MIN_DOCK_SIZE: f32 = 160.;
/// What a dock leaves to the editor when it is dragged to the limit.
pub const MIN_EDITOR_SIZE: f32 = 320.;
/// How much one press of a resize key changes a dock.
const KEYBOARD_RESIZE_STEP: f32 = 40.;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DockSide {
    Left,
    Right,
    Bottom,
}

/// The arrow of a resize key: where the edge of the tool window moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ResizeStep {
    Left,
    Right,
    Up,
    Down,
}

/// Tool windows that come with the product. Their stripe buttons are always there.
pub const PROJECT_TOOL: &str = "project";
pub const TERMINAL_TOOL: &str = "terminal";
pub const RUN_TOOL: &str = "run";
pub const DEBUG_TOOL: &str = "debug";
pub const AI_TOOL: &str = "ai";
pub const TASKS_TOOL: &str = "tasks";
pub const COMMIT_TOOL: &str = "commit";
pub const STRUCTURE_TOOL: &str = "structure";
pub const PROBLEMS_TOOL: &str = "problems";
pub const USAGES_TOOL: &str = "usages";
pub const TODO_TOOL: &str = "todo";
pub const SERVICES_TOOL: &str = "services";
pub const PERFORMANCE_TOOL: &str = "performance";
pub const DOCUMENTATION_TOOL: &str = "documentation";
pub const LOG_TOOL: &str = "log";
pub const CALL_HIERARCHY_TOOL: &str = "call-hierarchy";
pub const METHOD_HIERARCHY_TOOL: &str = "method-hierarchy";
pub const INSPECTION_TOOL: &str = "inspection-results";
pub const COVERAGE_TOOL: &str = "coverage";
pub const HTTP_TOOL: &str = "http-response";

pub(crate) struct DockTool {
    pub id: &'static str,
    pub view: AnyView,
    pub focus: Option<FocusHandle>,
}

/// One side of the window. It holds several tool windows and shows one of them.
pub(crate) struct Dock {
    pub slide: Slide,
    pub size: f32,
    pub tools: Vec<DockTool>,
    pub active: usize,
}

impl Dock {
    fn new(state: DockState) -> Self {
        Self {
            slide: Slide::new(state.open),
            size: state.size,
            tools: Vec::new(),
            active: 0,
        }
    }

    pub fn active_tool(&self) -> Option<&DockTool> {
        self.tools.get(self.active)
    }

    fn position(&self, id: &str) -> Option<usize> {
        self.tools.iter().position(|tool| tool.id == id)
    }

    fn state(&self) -> DockState {
        DockState {
            open: self.slide.is_open(),
            size: self.size,
        }
    }
}

struct ThemeFade {
    from: Theme,
    to: Theme,
    started: Instant,
}

pub(crate) enum Overlay {
    Menu {
        position: Point<Pixels>,
        entries: Vec<MenuEntry>,
        state: MenuState,
        geometry: MenuGeometry,
        /// Keys a menu of its own understands, such as the branch keys.
        keys: Vec<MenuShortcut>,
        /// The section of the menu bar this menu belongs to, when it was opened from it.
        bar: Option<usize>,
    },
    View {
        view: AnyView,
        /// The rows a popup that stands in for a menu shows, such as the branch popup.
        /// A popup of its own kind leaves them empty.
        rows: Option<Vec<MenuEntry>>,
        /// The name a button of the window gave its popup, so that the button finds the
        /// popup again and closes it on a second press. A popup of its own has none.
        tag: Option<&'static str>,
    },
}

/// The popup of the Bookmarks button of the right stripe.
pub const BOOKMARKS_POPUP: &str = "bookmarks";
/// The popup of Recent Locations, which shares the class of the bookmarks popup.
pub const RECENT_PLACES_POPUP: &str = "recent-places";

/// What a key pressed inside a menu runs. The index of the highlighted row comes along
/// when the menu knows it.
pub type MenuKeyAction = Rc<dyn Fn(Option<usize>, &mut Window, &mut App)>;

/// One key of a menu that does something besides walking the rows.
#[derive(Clone)]
pub struct MenuShortcut {
    /// `ctrl-alt-n`, as the keymap writes it.
    pub keys: &'static str,
    pub action: MenuKeyAction,
}

impl MenuShortcut {
    pub fn new(
        keys: &'static str,
        action: impl Fn(Option<usize>, &mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            keys,
            action: Rc::new(action),
        }
    }
}

/// A row of the menu of the screen of macOS, named by the path of its rows through the menu
/// of the window: the section, then the row, then its submenu and so on. The row is looked
/// up in the menu of the moment when it is chosen, so the menu of the screen can be built
/// once and still run what the window would run.
#[derive(Clone, PartialEq, Debug, ui_kit::gpui::Action)]
#[action(no_json, no_register)]
pub struct MenuRow {
    pub path: Vec<usize>,
    /// The key of the row as the keymap writes it, `ctrl-shift-n`. GPUI shows a key beside
    /// a row only for an action with a binding, so the key is part of the action: a binding
    /// of the row is the action of that row alone.
    pub key: Option<SharedString>,
}

pub(crate) struct Balloon {
    pub title: SharedString,
    pub text: SharedString,
    /// A button of the balloon, and the file it opens.
    pub action: Option<(SharedString, PathBuf)>,
    /// More buttons, in the order they are shown.
    pub buttons: Vec<BalloonButton>,
}

/// What a button of a notification does besides opening a file.
#[derive(Clone)]
pub(crate) enum BalloonAction {
    Url(String),
    InstallUpdate,
    SkipUpdate,
    /// Runs a command of the window by id, such as a restart of a language server.
    Command(&'static str),
}

/// One button of a notification.
#[derive(Clone)]
pub(crate) struct BalloonButton {
    /// Stable name for the debug selector of the button.
    pub id: &'static str,
    pub label: SharedString,
    pub action: BalloonAction,
}

type SearchHook = Rc<dyn Fn(&mut Workspace, &mut Window, &mut Context<Workspace>)>;

/// The list of run configurations, `true` for the debug one.
type ConfigChooseHook = Rc<dyn Fn(bool, &mut Workspace, &mut Window, &mut Context<Workspace>)>;

/// What a command does. Commands are found by id: from the keymap, from Find Action
/// and from scripts.
pub type CommandFn = Rc<dyn Fn(&mut Workspace, &mut Window, &mut Context<Workspace>)>;

/// Told which file became the active tab.
pub type TabListener = Rc<dyn Fn(&Path, &mut Context<Workspace>)>;

pub struct Workspace {
    pub(crate) root: PathBuf,
    pub(crate) name: SharedString,
    pub(crate) branch: Option<SharedString>,
    pub(crate) focus_handle: FocusHandle,
    /// The signal that editor events go through to the status bar and the tabs.
    pub(crate) chrome_signal: Entity<EditorChromeSignal>,
    /// The status bar, which follows the active editor on its own.
    pub(crate) status_view: Entity<StatusBarView>,
    /// The title bar, kept out of the window's own frames.
    pub(crate) title_view: Entity<ChromeView>,
    /// The stripes of the tool window docks.
    pub(crate) left_stripe_view: Entity<ChromeView>,
    pub(crate) right_stripe_view: Entity<ChromeView>,
    /// The status bar and the tabs follow the changes of the window once it is built.
    chrome_started: bool,
    /// The number of changes of what the window itself shows, for tests.
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) state_revision: usize,
    /// The last change of the window that a draw has shown, for tests.
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) counted_revision: usize,
    pub(crate) left: Dock,
    pub(crate) right: Dock,
    pub(crate) bottom: Dock,
    pub(crate) panes: Vec<Pane>,
    pub(crate) active_pane: usize,
    /// Groups of tabs sit one under another instead of side by side.
    pub(crate) split_down: bool,
    /// Files of closed tabs, the latest last, for Ctrl+Shift+T.
    closed: Vec<PathBuf>,
    /// Projects opened before, for the main menu.
    pub(crate) recent: Vec<PathBuf>,
    /// Files edited in this project, the latest first, for Recent Files.
    recent_edited: Vec<PathBuf>,
    state_dir: PathBuf,
    settings_dir: PathBuf,
    pub(crate) overlay: Option<Overlay>,
    /// Bumped on every popup open so its entry animation restarts.
    pub(crate) overlay_serial: usize,
    /// The delayed opening of a submenu under the pointer, when one is pending.
    menu_task: Option<Task<()>>,
    /// The area of every section of the menu bar, measured while drawing.
    pub(crate) menu_bar_bounds: Rc<RefCell<HashMap<usize, Bounds<Pixels>>>>,
    /// The area of the overflow row of the menu bar, measured while drawing.
    pub(crate) menu_bar_overflow: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// The areas of the controls that open menus, measured while drawing.
    pub(crate) t_config_anchor: MenuAnchor,
    pub(crate) t_project_anchor: MenuAnchor,
    pub(crate) t_branch_anchor: MenuAnchor,
    pub(crate) st_branch_anchor: MenuAnchor,
    pub(crate) st_eol_anchor: MenuAnchor,
    pub(crate) st_enc_anchor: MenuAnchor,
    pub(crate) st_indent_anchor: MenuAnchor,
    /// The button that asked an action to open its menu, until the action reads it.
    action_menu_anchor: RefCell<Option<Bounds<Pixels>>>,
    /// How many sections of the menu bar fitted in the last frame.
    pub(crate) menu_bar_visible: Cell<usize>,
    /// The section the keyboard picked in the menu bar, when no submenu is open.
    pub(crate) menu_bar_focus: Cell<Option<usize>>,
    /// Whether Alt is held, so the mnemonics of the menu bar show.
    pub(crate) menu_bar_alt: Cell<bool>,
    /// Bumped on every tab switch so the fade restarts.
    pub(crate) tab_serial: usize,
    pub(crate) balloon: Option<Balloon>,
    /// The dock of the tool window shown last, for Shift+Esc while the editor has the focus.
    last_tool: Option<DockSide>,
    /// Docks that Hide All Windows closed, so that the same key brings them back.
    hidden_docks: Option<Vec<DockSide>>,
    /// The dock that fills the window while a tool window is maximized.
    maximized: Option<DockSide>,
    pub(crate) balloon_visible: bool,
    pub(crate) balloon_serial: usize,
    icons: FileIcons,
    editor_factory: Option<EditorFactory>,
    search: Option<SearchHook>,
    config_choose: Option<ConfigChooseHook>,
    /// The state of the language server, shown in the status bar.
    pub(crate) language_status: Option<SharedString>,
    /// The explanation behind the language status line, for the balloon of a click.
    pub(crate) language_details: Option<SharedString>,
    /// A page with the tool the language server did not find, and the label of its button.
    pub(crate) language_download: Option<(SharedString, String)>,
    /// Whether the balloon also offers a second search for the tool.
    pub(crate) language_check: bool,
    pub(crate) run: Option<RunControls>,
    pub(crate) debug: Option<DebugControls>,
    /// Version and channel of this build.
    info: AppInfo,
    /// The newer release the notification offers, while it is open.
    update_release: Option<updater::Release>,
    /// An update is downloading or being installed; a second one does not start.
    installing_update: bool,
    theme_fade: Option<ThemeFade>,
    keymap: Keymap,
    /// The actions the keymap knows: the default ones and the plugin commands.
    keymap_actions: ActionRegistry,
    /// What the user adds to and removes from the default layout.
    keymap_file: KeymapFile,
    keymap_path: PathBuf,
    /// The file did not parse: it is not overwritten.
    keymap_broken: bool,
    keymap_generation: usize,
    /// Keys of the plugin commands, put back after every change of the layout.
    extra_bindings: Vec<(Shortcut, &'static str)>,
    /// Told about every keymap change, so that open editors take the new one.
    keymap_listener: Option<KeymapListener>,
    keys: KeyDispatcher,
    /// Set by the keystroke interceptor when a key other than a modifier went down.
    key_between: std::rc::Rc<std::cell::Cell<bool>>,
    commands: CommandRegistry,
    handlers: HashMap<&'static str, CommandFn>,
    tab_listener: Option<TabListener>,
    layout: Layout,
    placement: Option<WindowPlacement>,
    store: Option<Arc<Store>>,
    save_serial: usize,
    saved_json: String,
    /// Opens a project elsewhere; `None` starts the program of the application.
    project_opener: Option<ProjectOpener>,
    /// The window closes without writing its unsaved text.
    skip_save_on_close: bool,
    _subscriptions: Vec<Subscription>,
}

/// Whether leaving a tab or the window writes unsaved text by itself.
fn autosave_on(cx: &App) -> bool {
    cx.try_global::<Settings>()
        .is_none_or(|settings| settings.autosave)
}

fn placement_of(window: &Window) -> Option<WindowPlacement> {
    let (bounds, maximized) = match window.window_bounds() {
        WindowBounds::Windowed(bounds) => (bounds, false),
        WindowBounds::Maximized(bounds) => (bounds, true),
        WindowBounds::Fullscreen(_) => return None,
    };
    Some(WindowPlacement {
        x: f32::from(bounds.origin.x),
        y: f32::from(bounds.origin.y),
        width: f32::from(bounds.size.width),
        height: f32::from(bounds.size.height),
        maximized,
    })
}

fn read_branch(root: &Path) -> Option<String> {
    let head = std::fs::read_to_string(root.join(".git").join("HEAD")).ok()?;
    let branch = head.trim().strip_prefix("ref: refs/heads/")?;
    Some(branch.to_string())
}

/// The names a README of the project root may have, in the order the window tries them: a
/// markdown one first, then plain text.
const README_NAMES: [&str; 3] = ["readme.md", "readme.txt", "readme"];

/// The README of the project root, spelled as the disk spells it.
fn readme_path(root: &Path) -> Option<PathBuf> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(root)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .collect();
    // Two spellings of one name on a case-sensitive system give a steady choice.
    entries.sort();
    README_NAMES.iter().find_map(|wanted| {
        entries
            .iter()
            .find(|path| {
                path.is_file()
                    && path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.eq_ignore_ascii_case(wanted))
            })
            .cloned()
    })
}

/// The path with its file name as the disk spells it, so that a tab carries the name the
/// file has there. The folder stays as given: resolving it would move a path out of the
/// project root when the root or one of its folders is a link.
fn on_disk_path(path: PathBuf) -> PathBuf {
    #[cfg(any(windows, target_os = "macos"))]
    {
        let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
            return path;
        };
        let wanted = name.to_string_lossy();
        let found = std::fs::read_dir(parent).ok().and_then(|entries| {
            entries.flatten().find(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .eq_ignore_ascii_case(&wanted)
            })
        });
        match found {
            Some(entry) => parent.join(entry.file_name()),
            None => path,
        }
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        path
    }
}

fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

impl Workspace {
    pub(crate) fn new(
        launch: Launch,
        layout: Layout,
        store: Option<Arc<Store>>,
        recent: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let Launch {
            root,
            open,
            icons,
            state_dir,
            settings_dir,
            info,
            ..
        } = launch;
        let name: SharedString = root
            .file_name()
            .map_or_else(
                || root.display().to_string(),
                |name| name.to_string_lossy().into_owned(),
            )
            .into();
        window.set_window_title(&format!("{name} - Arro"));

        let weak = cx.weak_entity();
        window.on_window_should_close(cx, move |window, cx| {
            let saved = weak.update(cx, |this, cx| {
                if !this.skip_save_on_close {
                    this.save_now(window, cx);
                }
            });
            saved.ok();
            true
        });
        let chrome_weak = cx.weak_entity();
        let chrome_signal = cx.new(|_| EditorChromeSignal);
        let status_view = StatusBarView::create(chrome_weak.clone(), chrome_signal.clone(), cx);
        let title_view = ChromeView::title_bar(chrome_weak.clone(), cx);
        let left_stripe_view = ChromeView::left_stripe(chrome_weak.clone(), cx);
        let right_stripe_view = ChromeView::right_stripe(chrome_weak.clone(), cx);
        let bounds = cx.observe_window_bounds(window, |this, window, cx| {
            this.placement = placement_of(window).or(this.placement);
            this.layout_changed(cx);
        });
        // Leaving the window for another program writes what is unsaved, as the IDE does.
        let activation = cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active() {
                // The menu of the screen follows what the window holds: recent projects,
                // run configurations, the tabs of the window.
                this.refresh_platform_menus(cx);
            } else if autosave_on(cx) {
                this.save_all_now(cx);
            }
        });
        // A key bound as an action, Ctrl+C in the project tree for one, never reaches
        // the key listeners of the window, yet it must break a Double Ctrl as well.
        // The interceptor only raises a flag: it may run while the window is updated.
        let key_between = std::rc::Rc::new(std::cell::Cell::new(false));
        let raised = key_between.clone();
        let keystrokes = cx.intercept_keystrokes(move |event, _, _| {
            if !is_modifier_key(&event.keystroke.key) {
                raised.set(true);
            }
        });

        let branch_root = root.clone();
        let read = cx
            .background_executor()
            .spawn(async move { read_branch(&branch_root) });
        cx.spawn(async move |this, cx| {
            let branch = read.await;
            this.update(cx, |this, cx| {
                this.branch = branch.map(Into::into);
                cx.notify();
            })
            .ok();
        })
        .detach();

        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);
        let mut layout = layout;
        if let Some(path) = open {
            let panes = &mut layout.panes;
            if panes.is_empty() {
                panes.push(PaneState::default());
            }
            let pane = layout.active_pane.min(panes.len() - 1);
            let state = &mut panes[pane];
            let key = ::platform::path_key(&path);
            state.active = state
                .tabs
                .iter()
                .position(|tab| ::platform::path_key(tab) == key)
                .unwrap_or_else(|| {
                    state.tabs.push(path);
                    state.tabs.len() - 1
                });
        }
        let saved_json = serde_json::to_string(&layout).unwrap_or_default();
        // A console has nothing to restore: a project that opens with the Run window
        // left from the last time would show an empty console until the first run.
        let bottom = match layout.bottom_tool.as_deref() {
            Some(RUN_TOOL) => crate::layout::DockState {
                open: false,
                ..layout.bottom
            },
            _ => layout.bottom,
        };

        let keymap_path = settings_dir.join(KEYMAP_FILE);
        let (keymap_actions, keymap) = keymap::default_keymap();

        let mut workspace = Self {
            name,
            branch: None,
            focus_handle,
            chrome_signal,
            status_view,
            title_view,
            left_stripe_view,
            right_stripe_view,
            chrome_started: false,
            #[cfg(any(test, feature = "test-support"))]
            state_revision: 0,
            #[cfg(any(test, feature = "test-support"))]
            counted_revision: 0,
            left: Dock::new(layout.left),
            right: Dock::new(layout.right),
            bottom: Dock::new(bottom),
            panes: vec![Pane::default()],
            active_pane: 0,
            split_down: layout.split_down,
            closed: Vec::new(),
            recent,
            recent_edited: layout.recent_edited.clone(),
            state_dir,
            settings_dir,
            overlay: None,
            overlay_serial: 0,
            menu_task: None,
            menu_bar_bounds: Rc::new(RefCell::new(HashMap::new())),
            menu_bar_overflow: Rc::new(Cell::new(None)),
            t_config_anchor: MenuAnchor::new(),
            t_project_anchor: MenuAnchor::new(),
            t_branch_anchor: MenuAnchor::new(),
            st_branch_anchor: MenuAnchor::new(),
            st_eol_anchor: MenuAnchor::new(),
            st_enc_anchor: MenuAnchor::new(),
            st_indent_anchor: MenuAnchor::new(),
            action_menu_anchor: RefCell::new(None),
            menu_bar_visible: Cell::new(0),
            menu_bar_focus: Cell::new(None),
            menu_bar_alt: Cell::new(false),
            tab_serial: 0,
            balloon: None,
            last_tool: None,
            hidden_docks: None,
            maximized: None,
            balloon_visible: false,
            balloon_serial: 0,
            icons,
            editor_factory: None,
            search: None,
            config_choose: None,
            language_status: None,
            language_details: None,
            language_download: None,
            language_check: false,
            run: None,
            debug: None,
            info,
            update_release: None,
            installing_update: false,
            theme_fade: None,
            keymap,
            keymap_actions,
            keymap_file: KeymapFile::default(),
            keymap_path,
            keymap_broken: false,
            keymap_generation: 0,
            extra_bindings: Vec::new(),
            keymap_listener: None,
            keys: KeyDispatcher::new(),
            key_between,
            commands: CommandRegistry::new(),
            handlers: HashMap::new(),
            tab_listener: None,
            placement: layout.window.or_else(|| placement_of(window)),
            layout,
            store,
            save_serial: 0,
            saved_json,
            project_opener: None,
            skip_save_on_close: false,
            root,
            _subscriptions: vec![bounds, activation, keystrokes],
        };
        workspace.panes[0].bar = Some(TabBarView::create(
            chrome_weak,
            workspace.chrome_signal.clone(),
            cx,
        ));
        let chrome = cx.weak_entity();
        cx.defer(move |cx| {
            chrome
                .update(cx, |workspace, cx| workspace.start_chrome(cx))
                .ok();
        });
        workspace.start_update_tasks(cx);
        workspace.reload_keymap(cx);
        workspace.refresh_platform_menus(cx);
        workspace
    }

    /// Builds a window on the test platform of GPUI, without a real window. The ordinary
    /// start goes through [`Workspace::new`] from [`crate::launch::run`].
    #[cfg(any(test, feature = "test-support"))]
    pub fn new_for_test(
        launch: Launch,
        layout: Layout,
        store: Option<Arc<Store>>,
        recent: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new(launch, layout, store, recent, window, cx)
    }

    pub fn info(&self) -> &AppInfo {
        &self.info
    }

    /// The small label next to the project name.
    pub fn channel_label(&self) -> &'static str {
        self.info.channel_label()
    }

    /// Replaces the build information of a test window.
    #[cfg(any(test, feature = "test-support"))]
    pub fn set_info(&mut self, info: AppInfo, cx: &mut Context<Self>) {
        self.info = info;
        cx.notify();
    }

    pub fn show_about(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        AboutView::show(self, window, cx);
    }

    /// Deletes the files of an earlier update and, in a release build, asks for a newer
    /// version once the window has been open for a while. The request never runs on the
    /// interface thread.
    fn start_update_tasks(&mut self, cx: &mut Context<Self>) {
        if let Ok(folder) = updater::install_dir() {
            let cleanup = cx
                .background_executor()
                .spawn(async move { updater::cleanup(&folder) });
            cx.spawn(async move |_, _| {
                if let Err(error) = cleanup.await {
                    eprintln!("old update files are not removed: {error}");
                }
            })
            .detach();
        }

        let off = std::env::var(updater::NO_UPDATE_CHECK_ENV).as_deref() == Ok("1");
        if cfg!(debug_assertions)
            || cfg!(test)
            || cfg!(feature = "test-support")
            || off
            || updater::Platform::current().is_none()
        {
            return;
        }
        // Every start asks, so a release is offered the first time the program opens after
        // it; a window left open asks again twice a day.
        cx.spawn(async move |this, cx| {
            let mut delay = UPDATE_DELAY;
            loop {
                cx.background_executor().timer(delay).await;
                let Ok(due) = this.update(cx, |this, cx| this.auto_update_due(cx)) else {
                    break;
                };
                if due {
                    this.update(cx, |this, cx| this.start_update_check(true, cx))
                        .ok();
                }
                delay = UPDATE_INTERVAL;
            }
        })
        .detach();
    }

    /// Whether the automatic check may ask now: the setting is on and no update is being
    /// installed.
    fn auto_update_due(&self, cx: &App) -> bool {
        !self.installing_update
            && cx
                .try_global::<Settings>()
                .is_none_or(|settings| settings.updates_check_automatically)
    }

    fn state_value(&self, key: &str) -> Option<String> {
        self.store.as_ref()?.get(key).ok().flatten()
    }

    fn set_state(&self, key: &str, value: &str) {
        if let Some(store) = self.store.as_ref()
            && let Err(error) = store.put(key, value)
        {
            eprintln!("{key} is not saved: {error}");
        }
    }

    /// The version the user asked not to be offered again.
    pub fn skipped_version(&self) -> Option<String> {
        self.state_value(SKIPPED_KEY)
    }

    /// Checks for a newer release; the answer comes back as a notification.
    pub fn check_updates(&mut self, cx: &mut Context<Self>) {
        self.start_update_check(false, cx);
    }

    fn start_update_check(&mut self, automatic: bool, cx: &mut Context<Self>) {
        // A system without a release has nothing to fetch: a manual check only says so,
        // and no request leaves the machine.
        if let Some(notice) = updater::manual_check_notice(updater::Platform::current()) {
            if !automatic {
                self.notify_user("No updates", notice, cx);
            }
            return;
        }
        let Some(current) = updater::parse_version(self.info.version) else {
            if !automatic {
                self.notify_user(
                    "Update check failed",
                    format!("The build version {} is not a version", self.info.version),
                    cx,
                );
            }
            return;
        };
        let source = updater::UpdateSource::new();
        let check = cx
            .background_executor()
            .spawn(async move { source.check(&current) });
        cx.spawn(async move |this, cx| {
            let result = check.await;
            this.update(cx, |this, cx| this.update_checked(result, automatic, cx))
                .ok();
        })
        .detach();
    }

    fn update_checked(
        &mut self,
        result: Result<Option<updater::Release>, updater::Error>,
        automatic: bool,
        cx: &mut Context<Self>,
    ) {
        self.set_state(LAST_CHECK_KEY, &now_seconds().to_string());
        match result {
            Ok(Some(release)) => {
                if automatic && self.skipped_version().as_deref() == Some(&release.version_text()) {
                    return;
                }
                self.update_release = Some(release.clone());
                self.show_update(release, cx);
            }
            Ok(None) if !automatic => {
                self.notify_user("No updates", "You're up to date", cx);
            }
            Ok(None) => {}
            Err(error) => {
                eprintln!("update check failed: {error}");
                if !automatic {
                    self.notify_user("Update check failed", error.to_string(), cx);
                }
            }
        }
    }

    fn show_update(&mut self, release: updater::Release, cx: &mut Context<Self>) {
        let version = release.version_text();
        let line = release
            .notes
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("A newer version of Arro is ready to install.")
            .trim()
            .to_owned();
        let mut buttons = vec![BalloonButton {
            id: "update",
            label: "Update and Restart".into(),
            action: BalloonAction::InstallUpdate,
        }];
        buttons.push(BalloonButton {
            id: "release-notes",
            label: "Release Notes".into(),
            action: BalloonAction::Url(release.page_url.clone()),
        });
        buttons.push(BalloonButton {
            id: "skip",
            label: "Skip This Version".into(),
            action: BalloonAction::SkipUpdate,
        });
        self.balloon = Some(Balloon {
            title: format!("Arro {version} is available").into(),
            text: line.into(),
            action: None,
            buttons,
        });
        self.balloon_visible = true;
        self.balloon_serial += 1;
        cx.notify();
    }

    /// Keeps the offered version out of the automatic checks from now on.
    pub(crate) fn skip_update(&mut self, cx: &mut Context<Self>) {
        if let Some(release) = self.update_release.take() {
            self.set_state(SKIPPED_KEY, &release.version_text());
        }
        self.balloon_visible = false;
        self.balloon_serial += 1;
        cx.notify();
    }

    /// Downloads and installs the offered release away from the interface, then starts
    /// the new executable and closes this window.
    pub(crate) fn install_update(&mut self, cx: &mut Context<Self>) {
        if self.installing_update {
            return;
        }
        let Some(release) = self.update_release.clone() else {
            return;
        };
        let Ok(install) = updater::install_dir() else {
            self.notify_user(
                "The update did not install",
                "The program folder is unknown",
                cx,
            );
            return;
        };
        let folder = self.state_dir.join("updates");
        self.installing_update = true;
        self.notify_user(
            "Updating Arro",
            format!("Downloading Arro {}...", release.version_text()),
            cx,
        );
        let source = updater::UpdateSource::new();
        let install_task = cx.background_executor().spawn(async move {
            let archive = source.download(&release, &folder)?;
            updater::stage(&archive, &install)
        });
        cx.spawn(async move |this, cx| {
            let result = install_task.await;
            this.update(cx, |this, cx| match result {
                Ok(program) => this.restart_after_update(program, cx),
                Err(error) => {
                    this.installing_update = false;
                    this.notify_user("The update did not install", error.to_string(), cx)
                }
            })
            .ok();
        })
        .detach();
    }

    /// Writes the open files and the layout, then opens the project again in a new
    /// process and quits this one, as Invalidate Caches > Restart does. A test or a
    /// script that replaced the project opener sees the opening instead.
    pub fn restart_project(&mut self, cx: &mut Context<Self>) {
        self.save_all_now(cx);
        if let (Some(store), Some(json)) = (self.store.clone(), self.fresh_json(cx))
            && let Err(error) = save_layout(&store, &self.root, &json)
        {
            eprintln!("layout is not saved: {error}");
        }
        let root = self.root.clone();
        if let Some(opener) = self.project_opener.clone() {
            opener(&root, cx);
            return;
        }
        match crate::launch::spawn_project_after(&self.state_dir, &root, Some(std::process::id())) {
            Ok(()) => cx.quit(),
            Err(error) => self.notify_user("Cannot restart the project", error.to_string(), cx),
        }
    }

    /// Writes the open files, then starts the new program on the same project and quits.
    /// `program` is the file the update just put in place: the running one was renamed,
    /// or the whole bundle was replaced on macOS.
    fn restart_after_update(&mut self, program: PathBuf, cx: &mut Context<Self>) {
        self.save_all_now(cx);
        if let (Some(store), Some(json)) = (self.store.clone(), self.fresh_json(cx))
            && let Err(error) = save_layout(&store, &self.root, &json)
        {
            eprintln!("layout is not saved: {error}");
        }
        let root = self.root.clone();
        let started = crate::launch::spawn_project_from(
            program,
            &self.state_dir,
            &root,
            Some(std::process::id()),
        );
        match started {
            Ok(()) => cx.quit(),
            Err(error) => self.notify_user(
                "The update is installed",
                format!("Restart Arro: {error}"),
                cx,
            ),
        }
    }

    /// Opens the tabs of the saved layout, after the panels are in place. A project that
    /// opens for the first time shows its README, rendered when it is markdown.
    pub fn restore_tabs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let first_open = self
            .store
            .as_ref()
            .is_none_or(|store| !has_saved_layout(store, &self.root));
        let states = std::mem::take(&mut self.layout.panes);
        let mut panes = Vec::with_capacity(states.len());
        let mut seen = HashSet::new();
        for state in &states {
            let mut tabs: Vec<Tab> = Vec::new();
            let mut active = 0;
            for (index, path) in state.tabs.iter().enumerate() {
                if !path.is_file() || !seen.insert(::platform::path_key(path)) {
                    continue;
                }
                let mut tab = self.tab_for(path.clone(), window, cx);
                tab.pinned = state.pinned.contains(path);
                if let Some(caret) = state.carets.get(index) {
                    (tab.editor.restore_caret)(*caret, cx);
                }
                if index == state.active {
                    active = tabs.len();
                }
                tabs.push(tab);
            }
            if !tabs.is_empty() {
                panes.push(self.new_pane(tabs, active, cx));
            }
        }
        if !panes.is_empty() {
            self.active_pane = self.layout.active_pane.min(panes.len() - 1);
            self.panes = panes;
            self.renumber_bars(cx);
        }
        self.focus_active(window, cx);
        if first_open {
            self.show_readme(window, cx);
        }
    }

    /// Opens the README of the project root on the first open of the project: alone when
    /// it is markdown, as plain text otherwise.
    fn show_readme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let asked = cx
            .try_global::<Settings>()
            .is_none_or(|settings| settings.open_readme_on_first_open);
        if !asked {
            return;
        }
        let Some(path) = readme_path(&self.root) else {
            return;
        };
        self.open_file(path.clone(), window, cx);
        if let Some(index) = self.panes[self.active_pane].position(&path) {
            let tab = &self.panes[self.active_pane].tabs[index];
            if let Some(show) = &tab.editor.show_preview_only {
                show(cx);
            }
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The layout the window opened with.
    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    /// The layout as it stands now, the one that is saved.
    pub fn current_layout(&self, cx: &App) -> Layout {
        self.capture(cx)
    }

    /// The width or height of a dock, for scripts and tests.
    pub fn dock_size(&self, side: DockSide) -> f32 {
        self.dock(side).size
    }

    pub fn icons(&self) -> &FileIcons {
        &self.icons
    }

    pub fn set_editor_factory(&mut self, factory: EditorFactory) {
        self.editor_factory = Some(factory);
    }

    /// Makes the status bar and the tabs follow the window after it was built. The
    /// window is not reachable as an entity while its constructor runs, so this waits
    /// for the first effect cycle.
    fn start_chrome(&mut self, cx: &mut Context<Self>) {
        if self.chrome_started {
            return;
        }
        self.chrome_started = true;
        let subscription = cx.observe_self(|this, cx| this.notify_chrome_views(cx));
        self._subscriptions.push(subscription);
        self.notify_chrome_views(cx);
    }

    /// The status bar, the tab strips, the title bar, the stripes and the tool windows
    /// show what the window itself shows, such as the active tab or the layout, so they
    /// redraw with it.
    fn notify_chrome_views(&mut self, cx: &mut Context<Self>) {
        #[cfg(any(test, feature = "test-support"))]
        {
            self.state_revision += 1;
        }
        self.status_view.update(cx, |_, cx| cx.notify());
        self.title_view.update(cx, |_, cx| cx.notify());
        self.left_stripe_view.update(cx, |_, cx| cx.notify());
        self.right_stripe_view.update(cx, |_, cx| cx.notify());
        for pane in &self.panes {
            if let Some(bar) = &pane.bar {
                bar.update(cx, |_, cx| cx.notify());
            }
        }
        // A tool window may show what the window holds (the branch, the open editors,
        // the project model), and its cached frame would keep the old one. A keystroke
        // does not notify the window, so typing still leaves the tool windows alone.
        let tools: Vec<_> = [&self.left, &self.right, &self.bottom]
            .into_iter()
            .flat_map(|dock| dock.tools.iter().map(|tool| tool.view.entity_id()))
            .collect();
        for tool in tools {
            App::notify(cx, tool);
        }
    }

    /// The text or the caret of an editor changed: what shows it redraws itself, the
    /// rest of the window stays as it is.
    pub fn editor_chrome_changed(&mut self, cx: &mut Context<Self>) {
        self.chrome_signal
            .update(cx, |signal, cx| signal.changed(cx));
    }

    /// Gives every tab strip the index of its group after a group was added or removed.
    fn renumber_bars(&mut self, cx: &mut Context<Self>) {
        for (index, pane) in self.panes.iter().enumerate() {
            if let Some(bar) = pane.bar.clone() {
                bar.update(cx, |bar, cx| bar.set_pane(index, cx));
            }
        }
    }

    /// A group with its own tab strip.
    fn new_pane(&self, tabs: Vec<Tab>, active: usize, cx: &mut Context<Self>) -> Pane {
        Pane {
            tabs,
            active,
            scroll: ScrollHandle::new(),
            bar: Some(TabBarView::create(
                cx.weak_entity(),
                self.chrome_signal.clone(),
                cx,
            )),
        }
    }

    /// Puts a tool window into a dock, in place of one with the same id. The window keeps
    /// the last frame of the tool window until the tool window itself changes, the theme
    /// or the scale changes, or the window changes what it shows.
    pub fn set_dock(
        &mut self,
        side: DockSide,
        id: &'static str,
        view: AnyView,
        focus: Option<FocusHandle>,
        cx: &mut App,
    ) {
        let view = ChromeView::tool(view, cx);
        let restore = side == DockSide::Bottom && self.layout.bottom_tool.as_deref() == Some(id);
        let dock = self.dock_mut(side);
        let tool = DockTool { id, view, focus };
        match dock.position(id) {
            Some(index) => dock.tools[index] = tool,
            None => dock.tools.push(tool),
        }
        if restore {
            dock.active = dock.position(id).unwrap_or_default();
        }
    }

    /// The branch the status bar and the toolbar show. Whoever watches the repository sets it.
    pub fn set_branch(&mut self, branch: Option<String>, cx: &mut Context<Self>) {
        let branch = branch.map(SharedString::from);
        if self.branch != branch {
            self.branch = branch;
            cx.notify();
        }
    }

    /// The text of the branch in the status bar, for scripts and tests.
    pub fn branch_text(&self) -> Option<&str> {
        self.branch.as_deref()
    }

    /// The open editors with their paths, for panels that decorate them.
    pub fn open_editors(&self) -> Vec<(PathBuf, AnyView)> {
        let tabs = self.panes.iter().flat_map(|pane| pane.tabs.iter());
        tabs.map(|tab| (tab.path.clone(), tab.editor.view.clone()))
            .collect()
    }

    /// The editor of the active tab with its path.
    pub fn active_editor(&self) -> Option<(PathBuf, AnyView)> {
        let tab = self.active_tab()?;
        Some((tab.path.clone(), tab.editor.view.clone()))
    }

    /// Writes the open editor of `path` if it has unsaved text.
    pub fn save_editor(&self, path: &Path, cx: &mut App) {
        let key = ::platform::path_key(path);
        let tabs = self.panes.iter().flat_map(|pane| pane.tabs.iter());
        for tab in tabs.filter(|tab| ::platform::path_key(&tab.path) == key) {
            (tab.editor.save)(cx);
        }
    }

    /// Writes the open editor of `path` before it returns. `Err` says why the text is still
    /// not on disk, for a caller that needs the file written, like Commit.
    pub fn save_editor_now(&self, path: &Path, cx: &mut App) -> Result<(), String> {
        let key = ::platform::path_key(path);
        let tabs = self.panes.iter().flat_map(|pane| pane.tabs.iter());
        for tab in tabs.filter(|tab| ::platform::path_key(&tab.path) == key) {
            if !(tab.editor.status)(cx).dirty {
                continue;
            }
            if let Some(error) = (tab.editor.save_now)(cx) {
                return Err(format!("{}: {error}", path.display()));
            }
            if (tab.editor.status)(cx).dirty {
                return Err(format!("{}: the text is still not on disk", path.display()));
            }
        }
        Ok(())
    }

    /// What the toolbar run widget shows and does.
    pub fn set_run_controls(&mut self, controls: RunControls) {
        self.run = Some(controls);
    }

    /// Whether the window has something to run, so a menu can offer Run and Debug.
    pub fn can_run(&self) -> bool {
        self.run.is_some()
    }

    pub fn can_debug(&self) -> bool {
        self.debug.is_some()
    }

    pub(crate) fn run_state(&self, cx: &App) -> RunState {
        self.run
            .as_ref()
            .map(|run| (run.state)(cx))
            .unwrap_or_default()
    }

    pub fn run_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(run) = self.run.clone() {
            (run.run)(window, cx);
            if self.run_state(cx).running {
                self.show_tool(RUN_TOOL, window, cx);
            }
        }
    }

    /// Writes the selected temporary configuration into `.arro/run.toml`.
    pub(crate) fn save_run(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(run) = self.run.clone() {
            (run.save)(window, cx);
        }
    }

    /// Ctrl+F9: builds the project with its build tool and shows the build console.
    pub fn build_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(run) = self.run.clone() {
            (run.build)(window, cx);
            self.show_tool(RUN_TOOL, window, cx);
        }
    }

    /// Ctrl+F5: the last configuration or test request again.
    pub fn rerun(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(run) = self.run.clone() {
            (run.rerun)(window, cx);
            self.show_tool(RUN_TOOL, window, cx);
        }
    }

    /// Ctrl+Shift+F9: compiles the module of `path` with the build tool.
    pub fn compile_path(
        &mut self,
        path: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(run) = self.run.clone() {
            (run.compile)(path, window, cx);
            self.show_tool(RUN_TOOL, window, cx);
        }
    }

    /// Alt+Shift+R: the tests of the last run again.
    pub fn rerun_tests(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(run) = self.run.clone() {
            (run.rerun_tests)(window, cx);
            self.show_tool(RUN_TOOL, window, cx);
        }
    }

    /// Runs the selected configuration with the coverage agent and shows its window.
    pub fn run_with_coverage(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(run) = self.run.clone() {
            (run.coverage)(window, cx);
            self.show_tool(COVERAGE_TOOL, window, cx);
        }
    }

    /// Runs the tests of the last run with the coverage agent.
    pub fn run_tests_with_coverage(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(run) = self.run.clone() {
            (run.run_tests_coverage)(window, cx);
            self.show_tool(COVERAGE_TOOL, window, cx);
        }
    }

    /// Hides the coverage stripes and the rows of the Coverage window, and closes it.
    pub fn hide_coverage(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(run) = self.run.clone() {
            (run.hide_coverage)(window, cx);
        }
        if self.tool_open(COVERAGE_TOOL) {
            self.toggle_tool(COVERAGE_TOOL, window, cx);
        }
    }

    /// Ctrl+Alt+F6: shows or hides the coverage stripes without closing the window.
    pub fn toggle_coverage(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(run) = self.run.clone() {
            (run.toggle_coverage)(window, cx);
        }
    }

    /// Ctrl+Shift+F2: stops the import or the build that runs in the background.
    pub fn stop_background(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(run) = self.run.clone() {
            (run.stop_background)(window, cx);
        }
    }

    /// Alt+Shift+F10 and Alt+Shift+F9: the list of run configurations.
    pub fn choose_config(&mut self, debug: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !debug && self.show_run_menu_under_button(window, cx) {
            return;
        }
        if let Some(choose) = self.config_choose.clone() {
            choose(debug, self, window, cx);
        }
    }

    pub fn stop_run(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(run) = self.run.clone() {
            (run.stop)(window, cx);
        }
        self.stop_debug(window, cx);
    }

    /// What the debug keys and the toolbar button call.
    pub fn set_debug_controls(&mut self, controls: DebugControls) {
        self.debug = Some(controls);
    }

    pub(crate) fn debug_state(&self, cx: &App) -> DebugRunState {
        self.debug
            .as_ref()
            .map(|debug| (debug.state)(cx))
            .unwrap_or_default()
    }

    pub fn debug_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(debug) = self.debug.clone() {
            (debug.start)(window, cx);
            if self.debug_state(cx).active {
                self.show_tool(DEBUG_TOOL, window, cx);
            }
        }
    }

    pub fn step_over(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(debug) = self.debug.clone() {
            (debug.step_over)(window, cx);
        }
    }

    pub fn step_into(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(debug) = self.debug.clone() {
            (debug.step_into)(window, cx);
        }
    }

    /// Alt+Shift+F7: a step into library code too.
    pub fn force_step_into(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(debug) = self.debug.clone() {
            (debug.force_step_into)(window, cx);
        }
    }

    /// Alt+Shift+F8: a step over that does not stop on the breakpoints inside the call.
    pub fn force_step_over(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(debug) = self.debug.clone() {
            (debug.force_step_over)(window, cx);
        }
    }

    /// Ctrl+Alt+Shift+F8: a breakpoint that goes away at the first stop.
    pub fn toggle_temporary_breakpoint(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(debug) = self.debug.clone() {
            (debug.toggle_temporary_breakpoint)(window, cx);
        }
    }

    /// Ctrl+Shift+F8: the properties of the breakpoint at the caret, or the list of them.
    pub fn edit_breakpoint(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(debug) = self.debug.clone() {
            (debug.edit_breakpoint)(window, cx);
        }
    }

    /// Shift+F7: choose the call to step into.
    pub fn smart_step_into(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(debug) = self.debug.clone() {
            (debug.smart_step_into)(window, cx);
        }
    }

    /// Alt+F9 and Ctrl+Alt+F9: a temporary breakpoint on the caret line.
    pub fn run_to_cursor(&mut self, force: bool, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(debug) = self.debug.clone() {
            (debug.run_to_cursor)(force, window, cx);
        }
    }

    /// Alt+F10: the file and line of the selected frame.
    pub fn show_execution_point(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(debug) = self.debug.clone() {
            (debug.show_execution_point)(window, cx);
        }
    }

    /// Ctrl+Alt+F8: the popup with the value at the caret.
    pub fn quick_evaluate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(debug) = self.debug.clone() {
            (debug.quick_evaluate)(window, cx);
        }
    }

    pub fn step_out(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(debug) = self.debug.clone() {
            (debug.step_out)(window, cx);
        }
    }

    pub fn resume_debug(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(debug) = self.debug.clone() {
            (debug.resume)(window, cx);
        }
    }

    pub fn evaluate_debug(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(debug) = self.debug.clone() {
            (debug.evaluate)(window, cx);
        }
    }

    pub fn toggle_breakpoint(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(debug) = self.debug.clone() {
            (debug.toggle_breakpoint)(window, cx);
        }
    }

    /// Ctrl+Shift+F8: the breakpoints and their properties.
    pub fn view_breakpoints(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(debug) = self.debug.clone() {
            (debug.view_breakpoints)(window, cx);
        }
    }

    pub fn stop_debug(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(debug) = self.debug.clone() {
            (debug.stop)(window, cx);
        }
    }

    pub(crate) fn select_run(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(run) = self.run.clone() {
            (run.select)(index, window, cx);
        }
        cx.notify();
    }

    fn find_tool(&self, id: &str) -> Option<(DockSide, usize)> {
        [DockSide::Left, DockSide::Bottom, DockSide::Right]
            .into_iter()
            .find_map(|side| Some((side, self.dock(side).position(id)?)))
    }

    /// The id of the tool window in front of the bottom dock, if any.
    pub fn active_tool_id(&self) -> Option<&'static str> {
        self.bottom.active_tool().map(|tool| tool.id)
    }

    /// The id of the tool window that has the focus, if any.
    pub fn focused_tool_id(&self, window: &Window, cx: &App) -> Option<&'static str> {
        let side = self.focused_dock(window, cx)?;
        self.dock(side).active_tool().map(|tool| tool.id)
    }

    /// Whether the tool window is the one shown in an open dock.
    pub fn tool_open(&self, id: &str) -> bool {
        self.find_tool(id).is_some_and(|(side, index)| {
            let dock = self.dock(side);
            dock.slide.is_open() && dock.active == index
        })
    }

    /// Opens the dock of the tool window on it, and focuses it.
    pub fn show_tool(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some((side, index)) = self.find_tool(id) else {
            return;
        };
        self.last_tool = Some(side);
        let dock = self.dock_mut(side);
        dock.active = index;
        if !dock.slide.is_open() {
            dock.slide.toggle();
        }
        match dock.tools[index].focus.clone() {
            Some(focus) => window.focus(&focus, cx),
            None => self.focus_active(window, cx),
        }
        self.layout_changed(cx);
        cx.notify();
    }

    /// The stripe button: shows the tool window, or hides it when it is already shown.
    pub fn toggle_tool(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        match self.find_tool(id) {
            Some((side, _)) if self.tool_open(id) => self.toggle_panel(side, window, cx),
            Some(_) => self.show_tool(id, window, cx),
            None => {}
        }
    }

    /// The dock whose tool window holds the focus, if any.
    fn focused_dock(&self, window: &Window, cx: &App) -> Option<DockSide> {
        [DockSide::Left, DockSide::Right, DockSide::Bottom]
            .into_iter()
            .find(|side| {
                let dock = self.dock(*side);
                dock.slide.is_open()
                    && dock
                        .active_tool()
                        .and_then(|tool| tool.focus.as_ref())
                        .is_some_and(|focus| focus.contains_focused(window, cx))
            })
    }

    /// Shift+Esc: the tool window with the focus goes away, otherwise the one shown last.
    /// The terminal and the AI sessions keep Escape for the program inside; Shift+Esc
    /// there closes the tool window and gives the editor the focus.
    pub fn hide_active_tool(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let focused = self.focused_dock(window, cx);
        let side = focused.or_else(|| {
            self.last_tool
                .filter(|side| self.dock(*side).slide.is_open())
        });
        if let Some(side) = side {
            self.toggle_panel(side, window, cx);
        }
    }

    /// Ctrl+Shift+F4: the active content tab of a tool window closes. A tool window has
    /// no tabs of its own here, so closing the last tab hides the window.
    pub fn close_active_tool_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let side = self.focused_dock(window, cx).or_else(|| {
            self.last_tool
                .filter(|side| self.dock(*side).slide.is_open())
        });
        if let Some(side) = side {
            self.maximized = None;
            self.toggle_panel(side, window, cx);
        }
    }

    /// Ctrl+Shift+F12: closes every dock, and the same key opens the ones it closed.
    pub fn toggle_all_windows(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(sides) = self.hidden_docks.take() {
            for side in sides {
                let dock = self.dock_mut(side);
                if !dock.slide.is_open() {
                    dock.slide.toggle();
                }
            }
            self.last_tool = [DockSide::Left, DockSide::Bottom, DockSide::Right]
                .into_iter()
                .find(|side| self.dock(*side).slide.is_open());
        } else {
            let sides = [DockSide::Left, DockSide::Right, DockSide::Bottom]
                .into_iter()
                .filter(|side| self.dock(*side).slide.is_open())
                .collect::<Vec<_>>();
            self.hidden_docks = Some(sides.clone());
            for side in sides {
                let dock = self.dock_mut(side);
                if dock.slide.is_open() {
                    dock.slide.toggle();
                }
            }
            self.maximized = None;
        }
        self.focus_active(window, cx);
        self.layout_changed(cx);
        cx.notify();
    }

    /// Whether Hide All Windows is holding the docks closed, for scripts and tests.
    pub fn all_windows_hidden(&self) -> bool {
        self.hidden_docks.is_some()
    }

    /// Ctrl+Shift+': the focused tool window fills the window, and the same key restores
    /// the size it had.
    pub fn toggle_maximize_tool_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.maximized.take().is_some() {
            cx.notify();
            return;
        }
        let side = self.focused_dock(window, cx).or_else(|| {
            self.last_tool
                .filter(|side| self.dock(*side).slide.is_open())
        });
        if let Some(side) = side {
            let dock = self.dock_mut(side);
            if !dock.slide.is_open() {
                dock.slide.toggle();
            }
            self.maximized = Some(side);
            cx.notify();
        }
    }

    /// The dock that is maximized, for scripts and tests.
    pub fn maximized_tool_window(&self) -> Option<DockSide> {
        self.maximized
    }

    /// F12: the focus goes to the tool window that had it last.
    pub fn jump_to_last_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let side = self
            .last_tool
            .or_else(|| {
                [DockSide::Left, DockSide::Bottom, DockSide::Right]
                    .into_iter()
                    .find(|side| self.dock(*side).slide.is_open())
            })
            .filter(|side| self.dock(*side).active_tool().is_some());
        let Some(side) = side else {
            return;
        };
        let dock = self.dock_mut(side);
        if !dock.slide.is_open() {
            dock.slide.toggle();
        }
        if let Some(focus) = dock.active_tool().and_then(|tool| tool.focus.clone()) {
            window.focus(&focus, cx);
        }
        self.last_tool = Some(side);
        self.layout_changed(cx);
        cx.notify();
    }

    /// Window > Store Current Layout as Default: remembers which tool windows are open
    /// and how wide they are.
    pub fn store_default_layout(&mut self, cx: &mut Context<Self>) {
        let layout = DefaultLayout {
            left: self.left.state(),
            right: self.right.state(),
            bottom: self.bottom.state(),
        };
        self.update_settings(|settings| settings.default_layout = Some(layout), cx);
    }

    /// Shift+F12: the docks go back to the sizes and the state the user stored, or to the
    /// ones of a new window when nothing was stored.
    pub fn restore_default_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let stored = cx
            .try_global::<Settings>()
            .and_then(|settings| settings.default_layout);
        let defaults = crate::layout::Layout::default();
        let (left, right, bottom) = match stored {
            Some(layout) => (layout.left, layout.right, layout.bottom),
            None => (defaults.left, defaults.right, defaults.bottom),
        };
        self.hidden_docks = None;
        self.maximized = None;
        for (side, state) in [
            (DockSide::Left, left),
            (DockSide::Right, right),
            (DockSide::Bottom, bottom),
        ] {
            let dock = self.dock_mut(side);
            dock.size = state.size;
            if dock.slide.is_open() != state.open {
                dock.slide.toggle();
            }
        }
        self.last_tool = self.left.slide.is_open().then_some(DockSide::Left);
        self.focus_active(window, cx);
        self.layout_changed(cx);
        cx.notify();
    }

    /// Which tool window dock is open and how wide it is, for scripts and tests.
    pub fn dock_state(&self, side: DockSide) -> DockState {
        self.dock(side).state()
    }

    /// Esc in a tool window, and the escape from the terminal and the AI sessions.
    pub fn focus_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_active(window, cx);
        cx.notify();
    }

    /// `zoom` scales the text of the interface and of the editors; 1 is the default size.
    pub fn zoom_ide(&mut self, amount: f32, cx: &mut Context<Self>) {
        self.update_settings(
            |settings| *settings = settings.with_ui_scale_step(amount),
            cx,
        );
    }

    /// The scale of the interface in Settings, for scripts and tests.
    pub fn ui_scale(&self, cx: &App) -> f32 {
        cx.try_global::<Settings>()
            .map_or(1., |settings| settings.ui_scale)
    }

    pub fn reset_ide_scale(&mut self, cx: &mut Context<Self>) {
        self.update_settings(|settings| *settings = settings.with_ui_scale(1.), cx);
    }

    /// Alt+Shift+. and Alt+Shift+,: the font of every open editor.
    pub fn change_editor_font(&mut self, step: f32, cx: &mut Context<Self>) {
        self.update_settings(|settings| *settings = settings.with_font_step(step), cx);
    }

    /// Double Shift and the toolbar button call this.
    pub fn set_search(
        &mut self,
        hook: impl Fn(&mut Workspace, &mut Window, &mut Context<Workspace>) + 'static,
    ) {
        self.search = Some(Rc::new(hook));
    }

    /// Alt+Shift+F10 and Alt+Shift+F9: the popup of run configurations.
    pub fn set_config_choose(
        &mut self,
        hook: impl Fn(bool, &mut Workspace, &mut Window, &mut Context<Workspace>) + 'static,
    ) {
        self.config_choose = Some(Rc::new(hook));
    }

    pub fn language_status(&self) -> Option<&SharedString> {
        self.language_status.as_ref()
    }

    /// The first line of `text` stands in the status bar, the rest explains the state
    /// in a balloon that a click on the line opens.
    pub fn set_language_status(&mut self, text: Option<SharedString>, cx: &mut Context<Self>) {
        let (line, details) = match text.as_deref().and_then(|text| text.split_once('\n')) {
            Some((line, details)) => (Some(SharedString::from(line)), Some(details.trim())),
            None => (text, None),
        };
        self.language_status = line;
        self.language_details = details.map(SharedString::from);
        cx.notify();
    }

    /// The extra buttons of the balloon behind the language status line: a page with
    /// the tool that was not found and, when it exists, a second search for it.
    pub fn set_language_setup(
        &mut self,
        download: Option<(SharedString, String)>,
        check: bool,
        cx: &mut Context<Self>,
    ) {
        self.language_download = download;
        self.language_check = check;
        cx.notify();
    }

    /// Opens the balloon behind the language status line, with a link to the README.
    pub(crate) fn show_language_help(&mut self, cx: &mut Context<Self>) {
        let (Some(title), Some(details)) =
            (self.language_status.clone(), self.language_details.clone())
        else {
            return;
        };
        let mut buttons = Vec::new();
        if let Some((label, url)) = &self.language_download {
            buttons.push(BalloonButton {
                id: "download",
                label: label.clone(),
                action: BalloonAction::Url(url.clone()),
            });
        }
        if self.language_check {
            buttons.push(BalloonButton {
                id: "check",
                label: "Check again".into(),
                action: BalloonAction::Command(CHECK_LANGUAGE_SETUP),
            });
        }
        buttons.push(BalloonButton {
            id: "readme",
            label: "README".into(),
            action: BalloonAction::Url(README_URL.to_owned()),
        });
        self.balloon = Some(Balloon {
            title,
            text: details,
            action: None,
            buttons,
        });
        self.balloon_visible = true;
        self.balloon_serial += 1;
        cx.notify();
    }

    /// What the project tree wants to be restored next time.
    pub fn set_tree_state(
        &mut self,
        expanded: Vec<PathBuf>,
        selected: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        self.layout.tree_expanded = expanded;
        self.layout.tree_selected = selected;
        self.layout_changed(cx);
    }

    pub(crate) fn dock(&self, side: DockSide) -> &Dock {
        match side {
            DockSide::Left => &self.left,
            DockSide::Right => &self.right,
            DockSide::Bottom => &self.bottom,
        }
    }

    fn dock_mut(&mut self, side: DockSide) -> &mut Dock {
        match side {
            DockSide::Left => &mut self.left,
            DockSide::Right => &mut self.right,
            DockSide::Bottom => &mut self.bottom,
        }
    }

    pub(crate) fn step_animations(&mut self, cx: &mut Context<Self>) -> bool {
        let mut active = false;
        for dock in [&mut self.left, &mut self.right, &mut self.bottom] {
            active |= dock.slide.step();
        }
        if let Some(fade) = &self.theme_fade {
            let t = (fade.started.elapsed().as_secs_f32() * 1000. / motion::THEME as f32).min(1.);
            // Text and background meet halfway, so that moment has to pass quickly.
            let eased = smoothstep(smoothstep(t));
            cx.set_global(Theme::mix(&fade.from, &fade.to, eased));
            if t >= 1. {
                let to = fade.to;
                cx.set_global(to);
                self.theme_fade = None;
            }
            active = true;
        }
        active
    }

    pub fn toggle_theme(&mut self, cx: &mut Context<Self>) {
        let from = *cx.global::<Theme>();
        let to = match &self.theme_fade {
            Some(fade) if fade.to.dark => Theme::light(),
            Some(_) => Theme::dark(),
            None if from.dark => Theme::light(),
            None => Theme::dark(),
        };
        self.set_theme(to, cx);
    }

    /// Fades to `to` and remembers whether it is dark. A theme of a plugin is not kept
    /// across restarts.
    pub fn set_theme(&mut self, to: Theme, cx: &mut Context<Self>) {
        let from = *cx.global::<Theme>();
        self.theme_fade = Some(ThemeFade {
            from,
            to,
            started: Instant::now(),
        });
        if let Some(store) = self.store.clone() {
            let dark = to.dark;
            let write = async move { save_dark_theme(&store, dark) };
            let write = cx.background_executor().spawn(write);
            cx.spawn(async move |_, _| {
                if let Err(error) = write.await {
                    eprintln!("theme is not saved: {error}");
                }
            })
            .detach();
        }
        cx.notify();
    }

    pub fn toggle_panel(&mut self, side: DockSide, window: &mut Window, cx: &mut Context<Self>) {
        let (focus, opened) = {
            let dock = self.dock_mut(side);
            dock.slide.toggle();
            let focus = dock.active_tool().and_then(|tool| tool.focus.clone());
            (focus, dock.slide.is_open())
        };
        if opened {
            self.last_tool = Some(side);
        }
        let focus = focus.filter(|_| opened);
        match focus {
            Some(focus) => window.focus(&focus, cx),
            None => self.focus_active(window, cx),
        }
        self.layout_changed(cx);
        cx.notify();
    }

    /// Follows the pointer while a dock edge is dragged.
    pub(crate) fn resize_dock(
        &mut self,
        side: DockSide,
        position: Point<Pixels>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let viewport = window.viewport_size();
        let (x, y) = (f32::from(position.x), f32::from(position.y));
        let (width, height) = (f32::from(viewport.width), f32::from(viewport.height));
        let (size, limit) = match side {
            DockSide::Left => (x - STRIPE_WIDTH, width),
            DockSide::Right => (width - RIGHT_STRIPE_WIDTH - x, width),
            DockSide::Bottom => (height - STATUS_HEIGHT - y, height),
        };
        let most = (limit - MIN_EDITOR_SIZE).max(MIN_DOCK_SIZE);
        self.dock_mut(side).size = size.clamp(MIN_DOCK_SIZE, most);
        self.layout_changed(cx);
        cx.notify();
    }

    /// The dock that holds the focus changes its size by one step, within the same
    /// limits as a drag of its edge.
    pub(crate) fn resize_focused_dock(
        &mut self,
        step: ResizeStep,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let Some(side) = self.focused_dock(window, cx) else {
            return;
        };
        let delta = match (side, step) {
            (DockSide::Left, ResizeStep::Left) | (DockSide::Right, ResizeStep::Right) => {
                -KEYBOARD_RESIZE_STEP
            }
            (DockSide::Left, ResizeStep::Right) | (DockSide::Right, ResizeStep::Left) => {
                KEYBOARD_RESIZE_STEP
            }
            (DockSide::Bottom, ResizeStep::Up) => KEYBOARD_RESIZE_STEP,
            (DockSide::Bottom, ResizeStep::Down) => -KEYBOARD_RESIZE_STEP,
            _ => return,
        };
        let viewport = window.viewport_size();
        let limit = match side {
            DockSide::Left | DockSide::Right => f32::from(viewport.width),
            DockSide::Bottom => f32::from(viewport.height),
        };
        let most = (limit - MIN_EDITOR_SIZE).max(MIN_DOCK_SIZE);
        let dock = self.dock_mut(side);
        dock.size = (dock.size + delta).clamp(MIN_DOCK_SIZE, most);
        self.layout_changed(cx);
        cx.notify();
    }

    /// The group and the index of the tab of one file, whatever case its path is typed in.
    fn open_tab(&self, path: &Path) -> Option<(usize, usize)> {
        self.panes
            .iter()
            .enumerate()
            .find_map(|(pane, item)| Some((pane, item.position(path)?)))
    }

    fn tab_for(&self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Tab {
        let title = path.file_name().unwrap_or_default().to_string_lossy();
        let factory = self.editor_factory.as_ref();
        let factory = factory.expect("setup gives the workspace an editor factory");
        Tab {
            title: title.into_owned().into(),
            icon: self.icons.icon_for(&path),
            editor: factory(path.clone(), window, cx),
            path,
            pinned: false,
        }
    }

    /// A file has one view: two buffers of it would overwrite each other on save.
    pub fn open_file(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((pane, index)) = self.open_tab(&path) {
            self.select_tab(pane, index, window, cx);
            return;
        }
        let path = on_disk_path(path);
        let tab = self.tab_for(path, window, cx);
        self.save_active(cx);
        let pane = &mut self.panes[self.active_pane];
        let index = pane.insert(pane.tabs.len(), tab);
        self.select_tab(self.active_pane, index, window, cx);
    }

    /// Opens a file without writing the unsaved text of the active tab, for Save As: the
    /// old file keeps its edits, as IDEA keeps them.
    pub fn open_file_keeping_edits(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some((pane, index)) = self.open_tab(&path) {
            self.select_tab_keeping_edits(pane, index, window, cx);
            return;
        }
        let tab = self.tab_for(on_disk_path(path), window, cx);
        let pane = self.active_pane;
        let index = {
            let pane = &mut self.panes[pane];
            pane.insert(pane.tabs.len(), tab)
        };
        self.select_tab_keeping_edits(pane, index, window, cx);
    }

    /// Shows a tab without writing the unsaved text of the one that is left.
    fn select_tab_keeping_edits(
        &mut self,
        pane: usize,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.active_pane = pane;
        self.panes[pane].active = index;
        self.tab_serial += 1;
        self.focus_active(window, cx);
        self.layout_changed(cx);
        if let (Some(listener), Some(path)) = (self.tab_listener.clone(), self.active_path()) {
            listener(&path, cx);
        }
        cx.notify();
    }

    /// Shift+Enter: the file opens in a group of its own on the right, or below.
    pub fn open_file_in_split(
        &mut self,
        path: PathBuf,
        down: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some((pane, index)) = self.open_tab(&path) {
            self.select_tab(pane, index, window, cx);
            return;
        }
        let tab = self.tab_for(on_disk_path(path), window, cx);
        self.save_active(cx);
        if self.panes.len() == 1 {
            self.split_down = down;
        }
        let at = (self.active_pane + 1).min(self.panes.len());
        let pane = self.new_pane(vec![tab], 0, cx);
        self.panes.insert(at, pane);
        self.renumber_bars(cx);
        self.active_pane = at;
        self.tab_serial += 1;
        self.focus_active(window, cx);
        self.layout_changed(cx);
        cx.notify();
    }

    /// Writes the selected tab when it has unsaved text: leaving a tab saves it.
    fn save_active(&self, cx: &mut App) {
        if !autosave_on(cx) {
            return;
        }
        // Written before it returns: the switch is not over until the text is on disk.
        if let Some(tab) = self.active_tab() {
            (tab.editor.save_now)(cx);
        }
    }

    /// Like [`Workspace::save_all`], but each file is on disk when this returns.
    fn save_all_now(&self, cx: &mut App) {
        for tab in self.panes.iter().flat_map(|pane| &pane.tabs) {
            (tab.editor.save_now)(cx);
        }
    }

    /// Writes every open file that has unsaved text. Ctrl+S, and the window losing the
    /// focus, do this.
    pub fn save_all(&self, cx: &mut App) {
        for tab in self.panes.iter().flat_map(|pane| &pane.tabs) {
            (tab.editor.save)(cx);
        }
    }

    /// The files edited in this project, the latest first. Opening a file alone does
    /// not put it here.
    pub fn recent_edited(&self) -> &[PathBuf] {
        &self.recent_edited
    }

    /// A buffer of `path` was edited: the file goes to the front of the edited list and
    /// the layout is written a moment later, so the list outlives the window.
    pub fn remember_edited(&mut self, path: &Path, cx: &mut Context<Self>) {
        let key = ::platform::path_key(path);
        if self
            .recent_edited
            .first()
            .is_some_and(|known| ::platform::path_key(known) == key)
        {
            return;
        }
        self.recent_edited
            .retain(|known| ::platform::path_key(known) != key);
        self.recent_edited.insert(0, path.to_path_buf());
        self.recent_edited.truncate(RECENT_EDITED_LIMIT);
        self.layout_changed(cx);
    }

    /// The folder with the settings file, and what plugins and keys add to it.
    pub fn settings_dir(&self) -> &Path {
        &self.settings_dir
    }

    /// The folder of the state database, and of what a project keeps beside it.
    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    /// The state database of the application, when it opened.
    pub fn store(&self) -> Option<Arc<Store>> {
        self.store.clone()
    }

    /// Changes the settings, applies them at once and writes the file in the background.
    pub fn update_settings(&mut self, change: impl FnOnce(&mut Settings), cx: &mut Context<Self>) {
        let mut settings = cx.try_global::<Settings>().cloned().unwrap_or_default();
        let before = settings.clone();
        change(&mut settings);
        if settings == before {
            return;
        }
        self.store_settings(settings, cx);
    }

    /// Applies a dialog's draft over the settings in force. Only the leaves that differ
    /// from `saved` come from the draft, so a field another part of the window changed
    /// meanwhile keeps its newer value.
    pub fn apply_settings(&mut self, saved: &Settings, draft: &Settings, cx: &mut Context<Self>) {
        let mut settings = cx.try_global::<Settings>().cloned().unwrap_or_default();
        settings.merge_draft(saved, draft);
        if cx.try_global::<Settings>() == Some(&settings) {
            return;
        }
        self.store_settings(settings, cx);
    }

    /// Puts the settings in force and writes the file in the background.
    fn store_settings(&mut self, settings: Settings, cx: &mut Context<Self>) {
        cx.set_global(settings.clone());
        let path = self.settings_dir.join(SETTINGS_FILE);
        let write = cx
            .background_executor()
            .spawn(async move { settings.save(&path) });
        cx.spawn(async move |_, _| {
            if let Err(error) = write.await {
                eprintln!("settings are not saved: {error}");
            }
        })
        .detach();
        cx.notify();
    }

    /// Writes the files of tabs under `path` before it returns: a rename comes next.
    pub fn save_tabs_under(&self, path: &Path, cx: &mut App) {
        let tabs = self.panes.iter().flat_map(|pane| &pane.tabs);
        for tab in tabs.filter(|tab| tab.path.starts_with(path)) {
            (tab.editor.save_now)(cx);
        }
    }

    /// The shortcuts in force, the default layout with the file of the user applied.
    pub fn keymap(&self) -> &Keymap {
        &self.keymap
    }

    /// What the user added to and removed from the default layout.
    pub fn keymap_file(&self) -> &KeymapFile {
        &self.keymap_file
    }

    /// The file of the user, `%APPDATA%/Arro/keymap.json` in the application.
    pub fn keymap_path(&self) -> &Path {
        &self.keymap_path
    }

    /// The journal of what the keymap file reader skipped.
    pub fn keymap_log_path(&self) -> PathBuf {
        self.keymap_path.with_file_name("keymap.log")
    }

    /// Bumped every time the keymap of the window changes.
    pub fn keymap_generation(&self) -> usize {
        self.keymap_generation
    }

    /// Whether the file of the user is broken and is left as it is.
    pub fn keymap_is_broken(&self) -> bool {
        self.keymap_broken
    }

    /// Whether an action has a keymap entry that a shortcut can be added to.
    pub fn keymap_has_action(&self, action: &str) -> bool {
        self.keymap_actions.contains(action)
    }

    /// The other actions that already run on a shortcut, for the conflict of a keymap edit.
    pub fn keymap_conflicts(&self, action: &str, shortcut: &Shortcut) -> Vec<&'static str> {
        let Some(context) = self.keymap_actions.context(action) else {
            return Vec::new();
        };
        self.keymap
            .actions_on(context, shortcut)
            .into_iter()
            .filter(|other| *other != action)
            .collect()
    }

    /// Reads the keymap file of the user in the background and applies it to the window
    /// and to every open editor. A broken file is reported and left alone.
    pub fn reload_keymap(&mut self, cx: &mut Context<Self>) {
        let path = self.keymap_path.clone();
        let registry = self.keymap_actions.clone();
        let view = cx.weak_entity();
        let read = cx
            .background_executor()
            .spawn(async move { KeymapFile::load(&path, &registry) });
        cx.spawn(async move |_, cx| {
            let loaded = read.await;
            view.update(cx, |workspace, cx| match loaded {
                Ok(loaded) => {
                    if !loaded.warnings.is_empty() {
                        let log = workspace.keymap_log_path();
                        let text = format!("{}\n", loaded.warnings.join("\n"));
                        for warning in &loaded.warnings {
                            eprintln!("keymap: {warning}");
                        }
                        cx.background_executor()
                            .spawn(async move {
                                use std::io::Write as _;
                                if let Some(parent) = log.parent() {
                                    let _ = std::fs::create_dir_all(parent);
                                }
                                if let Ok(mut file) = std::fs::OpenOptions::new()
                                    .create(true)
                                    .append(true)
                                    .open(&log)
                                {
                                    file.write_all(text.as_bytes()).ok();
                                }
                            })
                            .detach();
                    }
                    workspace.keymap_file = loaded.file;
                    workspace.keymap_broken = false;
                    workspace.rebuild_keymap(cx);
                }
                Err(error) => {
                    workspace.keymap_broken = true;
                    workspace.notify_user(
                        "Keymap",
                        format!("{KEYMAP_FILE} is not read: {error}"),
                        cx,
                    );
                }
            })
            .ok();
        })
        .detach();
    }

    /// Gives an action one more shortcut and writes the file in the background.
    pub fn add_keymap_shortcut(
        &mut self,
        action: &str,
        shortcut: &Shortcut,
        cx: &mut Context<Self>,
    ) {
        self.assign_keymap_shortcut(action, shortcut, &[], cx);
    }

    /// Gives an action a shortcut, taking it away from the actions the user chose to
    /// strip. The file is written once, after every change of the edit.
    pub fn assign_keymap_shortcut(
        &mut self,
        action: &str,
        shortcut: &Shortcut,
        others: &[&'static str],
        cx: &mut Context<Self>,
    ) {
        if self.keymap_broken {
            self.refuse_keymap_edit(cx);
            return;
        }
        for other in others {
            self.keymap_file.remove_shortcut(other, shortcut);
        }
        self.keymap_file.add_shortcut(action, shortcut);
        self.keymap_changed(cx);
    }

    /// Takes a shortcut away from an action and writes the file in the background.
    pub fn remove_keymap_shortcut(
        &mut self,
        action: &str,
        shortcut: &Shortcut,
        cx: &mut Context<Self>,
    ) {
        if self.keymap_broken {
            self.refuse_keymap_edit(cx);
            return;
        }
        self.keymap_file.remove_shortcut(action, shortcut);
        self.keymap_changed(cx);
    }

    /// Drops every change of an action: its default shortcuts come back.
    pub fn reset_keymap_action(&mut self, action: &str, cx: &mut Context<Self>) {
        if self.keymap_broken {
            self.refuse_keymap_edit(cx);
            return;
        }
        self.keymap_file.reset_action(action);
        self.keymap_changed(cx);
    }

    /// Drops every change: the whole layout is the default one again.
    pub fn reset_keymap(&mut self, cx: &mut Context<Self>) {
        if self.keymap_broken {
            self.refuse_keymap_edit(cx);
            return;
        }
        self.keymap_file.reset_all();
        self.keymap_changed(cx);
    }

    fn refuse_keymap_edit(&mut self, cx: &mut Context<Self>) {
        self.notify_user(
            "Keymap",
            format!("{KEYMAP_FILE} is broken and left as it is. Fix it and try again."),
            cx,
        );
    }

    fn keymap_changed(&mut self, cx: &mut Context<Self>) {
        self.rebuild_keymap(cx);
        self.save_keymap(cx);
        cx.notify();
    }

    /// The default layout with the file of the user on top, then the plugin keys.
    fn rebuild_keymap(&mut self, cx: &mut Context<Self>) {
        let (_, base) = keymap::default_keymap();
        let mut keymap = match self.keymap_file.apply(&self.keymap_actions, &base) {
            Ok(keymap) => keymap,
            Err(error) => {
                eprintln!("keymap: {error}");
                base
            }
        };
        for (shortcut, action) in &self.extra_bindings {
            if let Err(error) = keymap.bind(
                &self.keymap_actions,
                keymap::workspace::CONTEXT,
                shortcut,
                action,
            ) {
                eprintln!("keymap: {action}: {error}");
            }
        }
        self.keymap = keymap;
        // Open editors keep the keymap of a global of the view crate; the listener is
        // how a crate above both puts the new one there.
        if let Some(listener) = &self.keymap_listener {
            listener(cx, Rc::new(self.keymap.clone()));
        }
        self.keymap_generation += 1;
        cx.notify();
    }

    /// Sets what is told about a new keymap, so that the open editors take it at once.
    /// The application gives `editor_view::install_keymap` here.
    pub fn set_keymap_listener(&mut self, listener: impl Fn(&mut App, Rc<Keymap>) + 'static) {
        self.keymap_listener = Some(Rc::new(listener));
    }

    fn save_keymap(&self, cx: &mut Context<Self>) {
        let file = self.keymap_file.clone();
        let path = self.keymap_path.clone();
        let write = cx
            .background_executor()
            .spawn(async move { file.save(&path) });
        cx.spawn(async move |_, _| {
            if let Err(error) = write.await {
                eprintln!("keymap is not saved: {error}");
            }
        })
        .detach();
    }

    pub(crate) fn select_tab(
        &mut self,
        pane: usize,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let leaving = (self.active_pane, self.panes[self.active_pane].active);
        if leaving != (pane, index) {
            self.save_active(cx);
        }
        self.active_pane = pane;
        self.panes[pane].active = index;
        self.tab_serial += 1;
        self.focus_active(window, cx);
        self.layout_changed(cx);
        if let (Some(listener), Some(path)) = (self.tab_listener.clone(), self.active_path()) {
            listener(&path, cx);
        }
        cx.notify();
    }

    pub fn next_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pane = &self.panes[self.active_pane];
        if !pane.tabs.is_empty() {
            let next = next_tab(pane.active, pane.tabs.len());
            self.select_tab(self.active_pane, next, window, cx);
        }
    }

    pub fn previous_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pane = &self.panes[self.active_pane];
        if !pane.tabs.is_empty() {
            let previous = previous_tab(pane.active, pane.tabs.len());
            self.select_tab(self.active_pane, previous, window, cx);
        }
    }

    /// Takes a tab out of its group, and the group too when it was the last tab of a group
    /// that is not the only one. Nothing is written.
    fn drop_tab(&mut self, pane: usize, index: usize, cx: &mut Context<Self>) -> Tab {
        let tab = self.panes[pane].remove(index);
        if self.panes[pane].tabs.is_empty() && self.panes.len() > 1 {
            self.panes.remove(pane);
            self.active_pane = active_after_removal(self.active_pane, pane, self.panes.len());
            self.renumber_bars(cx);
        }
        tab
    }

    fn tab_removed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.tab_serial += 1;
        self.focus_active(window, cx);
        self.layout_changed(cx);
        cx.notify();
    }

    pub(crate) fn close_tab(
        &mut self,
        pane: usize,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        (self.panes[pane].tabs[index].editor.save)(cx);
        let tab = self.drop_tab(pane, index, cx);
        remember_closed(&mut self.closed, tab.path);
        self.tab_removed(window, cx);
    }

    pub fn close_active_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.active_pane;
        if !self.panes[pane].tabs.is_empty() {
            let index = self.panes[pane].active;
            self.close_tab(pane, index, window, cx);
        }
    }

    /// Closes the tabs of a group except `keep` and the pinned ones.
    pub(crate) fn close_other_tabs(
        &mut self,
        pane: usize,
        keep: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let kept = self.panes[pane].tabs[keep].path.clone();
        let closing = self.panes[pane].closable(Some(keep));
        self.close_tabs(pane, closing, window, cx);
        // The group of the kept tab stays: it still has that tab.
        if let Some(index) = self.panes[pane].position(&kept) {
            self.select_tab(pane, index, window, cx);
        }
    }

    /// Closes the tabs of the active group except the selected one and the pinned ones.
    pub fn close_other_tabs_in_active_group(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let pane = self.active_pane;
        if !self.panes[pane].tabs.is_empty() {
            let keep = self.panes[pane].active;
            self.close_other_tabs(pane, keep, window, cx);
        }
    }

    pub(crate) fn close_all_tabs(
        &mut self,
        pane: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let closing = self.panes[pane].closable(None);
        self.close_tabs(pane, closing, window, cx);
    }

    /// Window > Editor Tabs > Close All Tabs, on the active group.
    pub fn close_all_tabs_in_active_group(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.active_pane;
        self.close_all_tabs(pane, window, cx);
    }

    /// Window > Editor Tabs > Close Unmodified Tabs: the tabs without unsaved text go,
    /// the pinned ones stay.
    pub fn close_unmodified_tabs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.active_pane;
        let closing = self.panes[pane]
            .tabs
            .iter()
            .enumerate()
            .filter(|(_, tab)| !tab.pinned && !(tab.editor.status)(cx).dirty)
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        self.close_tabs(pane, closing, window, cx);
    }

    /// Window > Editor Tabs > Close Tabs to the Left.
    pub fn close_tabs_to_left(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.active_pane;
        let active = self.panes[pane].active;
        let closing = self.panes[pane]
            .closable(Some(active))
            .into_iter()
            .filter(|index| *index < active)
            .collect::<Vec<_>>();
        self.close_tabs(pane, closing, window, cx);
    }

    /// Window > Editor Tabs > Close Tabs to the Right.
    pub fn close_tabs_to_right(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.active_pane;
        let active = self.panes[pane].active;
        let closing = self.panes[pane]
            .closable(Some(active))
            .into_iter()
            .filter(|index| *index > active)
            .collect::<Vec<_>>();
        self.close_tabs(pane, closing, window, cx);
    }

    /// Closes the tabs at `indexes` of a group, from the last, so the indexes stay right.
    fn close_tabs(
        &mut self,
        pane: usize,
        indexes: Vec<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        for index in indexes.into_iter().rev() {
            (self.panes[pane].tabs[index].editor.save)(cx);
            let tab = self.drop_tab(pane, index, cx);
            remember_closed(&mut self.closed, tab.path);
        }
        self.tab_removed(window, cx);
    }

    /// Ctrl+Shift+T: opens the file of the tab closed last that is still there.
    pub fn reopen_closed_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        while let Some(path) = self.closed.pop() {
            let open = self.panes.iter().any(|pane| pane.position(&path).is_some());
            if !open && path.is_file() {
                self.open_file(path, window, cx);
                return;
            }
        }
    }

    /// Files of closed tabs, the latest last, for scripts and tests.
    pub fn closed_tabs(&self) -> &[PathBuf] {
        &self.closed
    }

    /// Paths of pinned tabs, for scripts and tests.
    pub fn pinned_tabs(&self) -> Vec<PathBuf> {
        let tabs = self.panes.iter().flat_map(|pane| &pane.tabs);
        tabs.filter(|tab| tab.pinned)
            .map(|tab| tab.path.clone())
            .collect()
    }

    pub(crate) fn toggle_pin(
        &mut self,
        pane: usize,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let pinned = !self.panes[pane].tabs[index].pinned;
        self.panes[pane].set_pinned(index, pinned);
        self.focus_active(window, cx);
        self.layout_changed(cx);
        cx.notify();
    }

    pub fn pin_active_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.active_pane;
        if !self.panes[pane].tabs.is_empty() {
            let index = self.panes[pane].active;
            self.toggle_pin(pane, index, window, cx);
        }
    }

    /// Moves a tab, by drag and drop or from a menu, inside its group or to another one.
    /// `to` is a group and a place in it.
    pub(crate) fn move_tab(
        &mut self,
        from: (usize, usize),
        to: (usize, usize),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (source, index) = from;
        let (target, at) = to;
        let known = |pane: usize| pane < self.panes.len();
        if !known(source) || !known(target) || index >= self.panes[source].tabs.len() {
            return;
        }
        let same = source == target;
        if same && self.panes[source].tabs.len() == 1 {
            return;
        }
        let source_vanishes = !same && self.panes[source].tabs.len() == 1;
        let tab = self.drop_tab(source, index, cx);
        let target = if source_vanishes && source < target {
            target - 1
        } else {
            target
        };
        // Taking the tab out shifted everything after it.
        let at = if same && at > index { at - 1 } else { at };
        let pane = &mut self.panes[target];
        let landed = pane.insert(at.min(pane.tabs.len()), tab);
        self.select_tab(target, landed, window, cx);
    }

    /// Opens a new group next to the active one, to the right or below. The first split
    /// decides the direction of the others. The selected tab goes to the new group when
    /// the old one keeps another tab, otherwise the new group is empty, ready for a file.
    pub fn split(&mut self, down: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.panes.len() == 1 {
            self.split_down = down;
        }
        let source = self.active_pane;
        let pane = if self.panes[source].tabs.len() > 1 {
            let active = self.panes[source].active;
            let tab = self.panes[source].remove(active);
            self.new_pane(vec![tab], 0, cx)
        } else {
            self.new_pane(Vec::new(), 0, cx)
        };
        self.panes.insert(source + 1, pane);
        self.renumber_bars(cx);
        self.active_pane = source + 1;
        self.tab_serial += 1;
        self.focus_active(window, cx);
        self.layout_changed(cx);
        cx.notify();
    }

    pub fn split_right(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.split(false, window, cx);
    }

    pub fn split_down(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.split(true, window, cx);
    }

    /// Sends the selected tab to the next group; with one group it splits first.
    pub fn move_to_other_group(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.panes[self.active_pane].tabs.is_empty() {
            return;
        }
        if self.panes.len() == 1 {
            self.split(self.split_down, window, cx);
            return;
        }
        let from = (self.active_pane, self.panes[self.active_pane].active);
        let target = next_tab(self.active_pane, self.panes.len());
        let end = self.panes[target].tabs.len();
        self.move_tab(from, (target, end), window, cx);
    }

    /// Moves the focus to the next group, or to the previous one for a negative `step`.
    pub fn focus_group(&mut self, step: isize, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.panes.len();
        if count < 2 {
            return;
        }
        self.active_pane = if step < 0 {
            previous_tab(self.active_pane, count)
        } else {
            next_tab(self.active_pane, count)
        };
        self.focus_active(window, cx);
        self.layout_changed(cx);
        cx.notify();
    }

    /// Ctrl+Alt+] and Ctrl+Alt+[: the next or previous window of the application, which
    /// is the next or previous open project.
    pub fn activate_project_window(
        &mut self,
        step: isize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let current = window.window_handle();
        let windows = cx.windows();
        let Some(at) = windows.iter().position(|handle| *handle == current) else {
            return;
        };
        let Some(next) = window_step(windows.len(), at, step) else {
            self.notify_user("Project Windows", "Only one project window is open", cx);
            return;
        };
        let handle = windows[next];
        handle
            .update(cx, |_, window, _| window.activate_window())
            .ok();
    }

    /// Closes the tabs of a file, or of the files in a folder, that is about to disappear
    /// from disk. Nothing is written back: the text would bring the file back.
    pub fn discard_tabs_under(&mut self, path: &Path, window: &mut Window, cx: &mut Context<Self>) {
        loop {
            let found = self.panes.iter().enumerate().find_map(|(pane, item)| {
                let index = item
                    .tabs
                    .iter()
                    .position(|tab| tab.path.starts_with(path))?;
                Some((pane, index))
            });
            let Some((pane, index)) = found else {
                break;
            };
            self.drop_tab(pane, index, cx);
        }
        self.tab_removed(window, cx);
    }

    /// A file or a folder was renamed: its tabs show the new place, with carets and pins.
    /// Write the unsaved text first, see [`Workspace::save_tabs_under`].
    pub fn paths_moved(
        &mut self,
        from: &Path,
        to: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        for pane in 0..self.panes.len() {
            for index in 0..self.panes[pane].tabs.len() {
                let old = &self.panes[pane].tabs[index];
                let Ok(inside) = old.path.strip_prefix(from) else {
                    continue;
                };
                let new = to.join(inside);
                let (caret, pinned) = ((old.editor.caret)(cx), old.pinned);
                let mut tab = self.tab_for(new, window, cx);
                tab.pinned = pinned;
                (tab.editor.restore_caret)(caret, cx);
                self.panes[pane].tabs[index] = tab;
            }
        }
        self.tab_serial += 1;
        self.focus_active(window, cx);
        self.layout_changed(cx);
        cx.notify();
    }

    /// File > Open Folder: asks for a folder and opens it in a window of its own.
    pub fn choose_project(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: None,
        });
        cx.spawn(async move |this, cx| {
            let chosen = paths.await.ok().and_then(Result::ok).flatten();
            if let Some(folder) = chosen.and_then(|paths| paths.into_iter().next()) {
                this.update(cx, |this, cx| this.open_project(folder, cx))
                    .ok();
            }
        })
        .detach();
    }

    /// Opens a project in a new window: a program of its own, as the IDE does it.
    pub fn open_project(&mut self, folder: PathBuf, cx: &mut Context<Self>) {
        if let Some(opener) = self.project_opener.clone() {
            opener(&folder, cx);
            return;
        }
        let state_dir = self.state_dir.clone();
        let start = cx
            .background_executor()
            .spawn(async move { crate::launch::spawn_project(&state_dir, &folder) });
        cx.spawn(async move |this, cx| {
            if let Err(error) = start.await {
                let text = error.to_string();
                let report = |this: &mut Self, cx: &mut Context<Self>| {
                    this.notify_user("Cannot open the project", text, cx)
                };
                this.update(cx, report).ok();
            }
        })
        .detach();
    }

    /// Replaces where a project opens; the scripts and tests of the window use it.
    pub fn set_project_opener(&mut self, opener: ProjectOpener) {
        self.project_opener = Some(opener);
    }

    /// Puts a project at the front of the recent list, as opening it does.
    pub fn remember_recent_project(&mut self, folder: PathBuf, cx: &mut Context<Self>) {
        self.recent.retain(|path| *path != folder);
        self.recent.insert(0, folder.clone());
        self.recent.truncate(10);
        if let Some(store) = &self.store
            && let Err(error) = crate::layout::remember_project(store, &folder)
        {
            eprintln!("recent projects are not saved: {error}");
        }
        cx.notify();
    }

    /// File > Open Recent > Clear List.
    pub fn clear_recent_projects(&mut self, cx: &mut Context<Self>) {
        self.recent.clear();
        if let Some(store) = &self.store
            && let Err(error) = crate::layout::clear_recent(store)
        {
            eprintln!("recent projects are not saved: {error}");
        }
        cx.notify();
    }

    /// Takes the text on disk for the tab at `index` of a group.
    pub(crate) fn reload_tab(&mut self, pane: usize, index: usize, cx: &mut Context<Self>) {
        if let Some(tab) = self.panes.get(pane).and_then(|pane| pane.tabs.get(index)) {
            (tab.editor.reload_from_disk)(cx);
        }
    }

    /// Changes the line end or the encoding of the file in the selected tab.
    pub fn set_active_format(&mut self, name: &str, cx: &mut Context<Self>) {
        if let Some(tab) = self.active_tab() {
            (tab.editor.set_format)(name, cx);
            cx.notify();
        }
    }

    /// Changes the indentation of the file in the selected tab until it closes.
    pub fn set_active_indent(
        &mut self,
        choice: crate::editor::IndentChoice,
        cx: &mut Context<Self>,
    ) {
        if let Some(tab) = self.active_tab() {
            (tab.editor.set_indent)(choice, cx);
            cx.notify();
        }
    }

    pub(crate) fn focus_active(&self, window: &mut Window, cx: &mut Context<Self>) {
        match self.panes[self.active_pane].active_tab() {
            Some(tab) => window.focus(&tab.editor.focus, cx),
            None => window.focus(&self.focus_handle, cx),
        }
    }

    pub(crate) fn active_tab(&self) -> Option<&Tab> {
        self.panes[self.active_pane].active_tab()
    }

    pub(crate) fn active_status(&self, cx: &App) -> Option<EditorStatus> {
        self.active_tab().map(|tab| (tab.editor.status)(cx))
    }

    /// Paths of open tabs pane by pane, for scripts and tests.
    pub fn open_tabs(&self) -> Vec<Vec<PathBuf>> {
        let paths = |pane: &Pane| pane.tabs.iter().map(|tab| tab.path.clone()).collect();
        self.panes.iter().map(paths).collect()
    }

    /// Titles of open tabs pane by pane, for scripts and tests.
    pub fn tab_titles(&self) -> Vec<Vec<String>> {
        let titles = |pane: &Pane| pane.tabs.iter().map(|tab| tab.title.to_string()).collect();
        self.panes.iter().map(titles).collect()
    }

    /// Whether the editor of the active tab holds the focus, for scripts and tests.
    pub fn editor_has_focus(&self, window: &Window, cx: &App) -> bool {
        self.active_tab()
            .is_some_and(|tab| tab.editor.focus.contains_focused(window, cx))
    }

    /// The group that has the focus, for scripts and tests.
    pub fn active_group(&self) -> usize {
        self.active_pane
    }

    /// Whether groups sit one under another, for scripts and tests.
    pub fn split_is_down(&self) -> bool {
        self.split_down
    }

    /// Projects opened before, the latest first.
    pub fn recent_projects(&self) -> &[PathBuf] {
        &self.recent
    }

    /// The status of the active tab, for scripts and tests.
    pub fn editor_status(&self, cx: &App) -> Option<EditorStatus> {
        self.active_status(cx)
    }

    /// The caret the status bar drew last, for tests of what a keystroke redraws.
    #[cfg(any(test, feature = "test-support"))]
    pub fn status_caret_on_screen(&self, cx: &App) -> Option<String> {
        self.status_view.read(cx).shown_caret.clone()
    }

    /// The dirty marks the tab strips drew last, group by group, for tests.
    #[cfg(any(test, feature = "test-support"))]
    pub fn tab_dirty_on_screen(&self, cx: &App) -> Vec<Vec<bool>> {
        self.panes
            .iter()
            .map(|pane| {
                pane.bar
                    .as_ref()
                    .map(|bar| bar.read(cx).shown_dirty.clone())
                    .unwrap_or_default()
            })
            .collect()
    }

    /// Whether the dock is open, for scripts and tests.
    pub fn dock_open(&self, side: DockSide) -> bool {
        self.dock(side).slide.is_open()
    }

    pub fn show_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(hook) = self.search.clone() {
            hook(self, window, cx);
        }
    }

    pub fn show_overlay(
        &mut self,
        view: AnyView,
        focus: FocusHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_overlay_with_rows(view, focus, None, window, cx);
    }

    /// Shows a popup that stands in for a menu and reports the rows it shows, so the
    /// checks and scripts that look for the rows of a menu find it.
    pub fn show_overlay_with_rows(
        &mut self,
        view: AnyView,
        focus: FocusHandle,
        rows: Option<Vec<MenuEntry>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_view(view, focus, rows, None, window, cx);
    }

    /// Shows a popup under a name of its own, so that the button that opens it finds it
    /// again and closes it on a second press.
    pub fn show_overlay_tagged(
        &mut self,
        view: AnyView,
        focus: FocusHandle,
        tag: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_view(view, focus, None, Some(tag), window, cx);
    }

    fn show_view(
        &mut self,
        view: AnyView,
        focus: FocusHandle,
        rows: Option<Vec<MenuEntry>>,
        tag: Option<&'static str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.overlay_serial += 1;
        self.action_menu_anchor.borrow_mut().take();
        self.overlay = Some(Overlay::View { view, rows, tag });
        window.focus(&focus, cx);
        cx.notify();
    }

    /// Shows a popup that keeps the focus where it is, such as the value at the caret.
    pub fn show_popup(&mut self, view: AnyView, window: &mut Window, cx: &mut Context<Self>) {
        self.overlay_serial += 1;
        self.action_menu_anchor.borrow_mut().take();
        self.overlay = Some(Overlay::View {
            view,
            rows: None,
            tag: None,
        });
        let _ = window;
        cx.notify();
    }

    /// The name the open popup was shown with, if a button of the window gave it one.
    pub fn overlay_tag(&self) -> Option<&'static str> {
        match self.overlay.as_ref()? {
            Overlay::View { tag, .. } => *tag,
            Overlay::Menu { .. } => None,
        }
    }

    /// Closes the open popup and gives the focus back to the editor, as a second press on
    /// the button that opened it does.
    pub(crate) fn close_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.overlay.take().is_some() {
            self.menu_task = None;
            self.focus_active(window, cx);
            cx.notify();
        }
    }

    pub fn show_menu(
        &mut self,
        position: Point<Pixels>,
        entries: Vec<MenuEntry>,
        cx: &mut Context<Self>,
    ) {
        self.show_menu_with_keys(position, entries, Vec::new(), cx);
    }

    /// A menu that a button opens. It stands under that button, not at the point of the
    /// click: its size comes from the same metrics that draw it, so its place is known
    /// before the first frame.
    pub fn show_menu_under(
        &mut self,
        anchor: Bounds<Pixels>,
        entries: Vec<MenuEntry>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let scale = cx
            .try_global::<Settings>()
            .map_or(1., |settings| settings.ui_scale);
        let size = menu_size(window, &entries, px(UI_SIZE * scale));
        let position = menu_position(anchor, size, window.viewport_size());
        self.show_menu(position, entries, cx);
    }

    /// The button that asked an action to open its menu, so that the owner of the action
    /// places it under the button instead of at the caret.
    pub fn set_action_menu_anchor(&mut self, bounds: Bounds<Pixels>) {
        *self.action_menu_anchor.borrow_mut() = Some(bounds);
    }

    /// Reads the button of the menu an action is about to open, once.
    pub fn take_action_menu_anchor(&self) -> Option<Bounds<Pixels>> {
        self.action_menu_anchor.borrow_mut().take()
    }

    /// A menu that also answers keys of its own, such as the branch popup.
    pub fn show_menu_with_keys(
        &mut self,
        position: Point<Pixels>,
        entries: Vec<MenuEntry>,
        keys: Vec<MenuShortcut>,
        cx: &mut Context<Self>,
    ) {
        self.show_menu_inner(position, entries, keys, None, cx);
    }

    /// The submenu of a section of the menu bar. The bar keeps answering the pointer
    /// while it is open, so that a neighbouring section takes over at a hover.
    pub(crate) fn show_menu_bar_section(
        &mut self,
        bar: Option<usize>,
        position: Point<Pixels>,
        entries: Vec<MenuEntry>,
        cx: &mut Context<Self>,
    ) {
        self.show_menu_inner(position, entries, Vec::new(), bar, cx);
    }

    fn show_menu_inner(
        &mut self,
        position: Point<Pixels>,
        entries: Vec<MenuEntry>,
        keys: Vec<MenuShortcut>,
        bar: Option<usize>,
        cx: &mut Context<Self>,
    ) {
        self.overlay_serial += 1;
        self.menu_task = None;
        self.action_menu_anchor.borrow_mut().take();
        self.overlay = Some(Overlay::Menu {
            position,
            entries,
            state: MenuState::default(),
            geometry: MenuGeometry::default(),
            keys,
            bar,
        });
        self.focus_menu(cx);
        cx.notify();
    }

    /// The window takes the keys while a menu is open. The editor keeps its caret, and
    /// the menu hands the focus back when it closes.
    fn focus_menu(&self, cx: &mut Context<Self>) {
        let weak = cx.weak_entity();
        cx.defer(move |cx| {
            let Some(window) = cx
                .active_window()
                .or_else(|| cx.windows().into_iter().next())
            else {
                return;
            };
            window
                .update(cx, |_, window, cx| {
                    weak.update(cx, |this, cx| window.focus(&this.focus_handle, cx))
                        .ok();
                })
                .ok();
        });
    }

    /// Whether the focus is in the tool window shown in one of the docks. The terminal
    /// and the AI sessions are left out: Escape there belongs to the program inside,
    /// such as a harness that stops its turn on it.
    fn tool_has_focus(&self, window: &Window, cx: &App) -> bool {
        [DockSide::Left, DockSide::Right, DockSide::Bottom]
            .into_iter()
            .any(|side| {
                let dock = self.dock(side);
                dock.slide.is_open()
                    && dock.active_tool().is_some_and(|tool| {
                        tool.id != TERMINAL_TOOL
                            && tool.id != AI_TOOL
                            && tool
                                .focus
                                .as_ref()
                                .is_some_and(|focus| focus.contains_focused(window, cx))
                    })
            })
    }

    /// Escape: a submenu closes first, then the menu itself.
    pub fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(Overlay::Menu { state, .. }) = self.overlay.as_mut()
            && state.close_submenu()
        {
            self.menu_task = None;
            cx.notify();
            return;
        }
        self.close_menu(window, cx);
    }

    /// Closes the menu with all its submenus, as a picked row or a click outside does.
    pub fn close_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.menu_bar_focus.set(None);
        if self.overlay.take().is_some() {
            self.menu_task = None;
            self.focus_active(window, cx);
            cx.notify();
        } else if self.tool_has_focus(window, cx) {
            // Escape in a tool window gives the focus back to the editor and leaves the
            // tool window open, as IDEA does.
            self.focus_active(window, cx);
            cx.notify();
        } else {
            // Without a popup Escape belongs to whoever has the focus.
            cx.propagate();
        }
    }

    pub fn has_overlay(&self) -> bool {
        self.overlay.is_some()
    }

    /// The view of the open popup, for a test that checks what it shows.
    #[cfg(any(test, feature = "test-support"))]
    pub fn overlay_view(&self) -> Option<AnyView> {
        match self.overlay.as_ref()? {
            Overlay::View { view, .. } => Some(view.clone()),
            Overlay::Menu { .. } => None,
        }
    }

    /// The rows of the open menu, for Find Action style checks and tests. A popup that
    /// stands in for a menu, such as the branch popup, reports its rows too.
    pub fn menu_entries(&self) -> Option<&[MenuEntry]> {
        match self.overlay.as_ref()? {
            Overlay::Menu { entries, .. } => Some(entries),
            Overlay::View { rows, .. } => rows.as_deref(),
        }
    }

    /// The section of the menu bar whose submenu is open, if any.
    pub fn menu_bar_section(&self) -> Option<usize> {
        match self.overlay.as_ref()? {
            Overlay::Menu { bar, .. } => *bar,
            Overlay::View { .. } => None,
        }
    }

    /// The section the keyboard picked in the menu bar, for tests.
    pub fn menu_bar_focus(&self) -> Option<usize> {
        self.menu_bar_focus.get()
    }

    /// How many sections of the menu bar showed in the last frame, for tests.
    pub fn menu_bar_visible(&self) -> usize {
        self.menu_bar_visible.get()
    }

    /// Where the open menu goes, for tests of the caret and tree anchors.
    #[cfg(any(test, feature = "test-support"))]
    pub fn menu_position(&self) -> Option<Point<Pixels>> {
        match self.overlay.as_ref()? {
            Overlay::Menu { position, .. } => Some(*position),
            Overlay::View { .. } => None,
        }
    }

    /// The highlighted rows of the open menu, from the top level down.
    pub fn menu_highlight(&self) -> Option<Vec<usize>> {
        match self.overlay.as_ref()? {
            Overlay::Menu { state, .. } => Some(state.highlight().to_vec()),
            Overlay::View { .. } => None,
        }
    }

    /// A key of the open menu that does something of its own, such as the branch keys.
    /// Runs during the capture of the key, before a window action takes it.
    pub(crate) fn menu_shortcut(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let key = event.keystroke.key.as_str();
        let modifiers = event.keystroke.modifiers;
        let shortcut = self.overlay.as_ref().and_then(|overlay| match overlay {
            Overlay::Menu { keys, state, .. } => {
                let pressed = |text: &str| {
                    keymap::Keystroke::parse(text).is_ok_and(|stroke| {
                        stroke.key == key
                            && stroke.ctrl == modifiers.control
                            && stroke.alt == modifiers.alt
                            && stroke.shift == modifiers.shift
                            && !modifiers.platform
                    })
                };
                keys.iter()
                    .find(|shortcut| pressed(shortcut.keys))
                    .map(|shortcut| (shortcut.action.clone(), state.highlight().last().copied()))
            }
            Overlay::View { .. } => None,
        });
        let Some((action, index)) = shortcut else {
            return false;
        };
        self.close_menu(window, cx);
        // The menu runs inside the update of the workspace: the action waits for it.
        let handle = window.window_handle();
        cx.defer(move |cx| {
            handle
                .update(cx, |_, window, cx| action(index, window, cx))
                .ok();
        });
        cx.stop_propagation();
        true
    }

    /// The keys of the open menu: arrows walk the rows and open or close submenus,
    /// Enter picks the row.
    pub(crate) fn menu_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.menu_shortcut(event, window, cx) {
            return true;
        }
        let key = event.keystroke.key.as_str();
        let modifiers = event.keystroke.modifiers;
        if modifiers.control || modifiers.alt || modifiers.platform || modifiers.shift {
            return false;
        }
        enum Step {
            Done,
            CloseMenu,
            Run(MenuAction),
        }
        let step = {
            let Some(Overlay::Menu { entries, state, .. }) = self.overlay.as_mut() else {
                return false;
            };
            match key {
                "down" => {
                    state.move_down(entries);
                    Step::Done
                }
                "up" => {
                    state.move_up(entries);
                    Step::Done
                }
                "right" => {
                    state.open_submenu(entries);
                    Step::Done
                }
                "left" => {
                    if state.close_submenu() {
                        Step::Done
                    } else {
                        Step::CloseMenu
                    }
                }
                "enter" => {
                    if !state.open_submenu(entries) {
                        match state
                            .current(entries)
                            .and_then(|entry| entry.on_click.clone())
                        {
                            Some(action) => Step::Run(action),
                            None => Step::Done,
                        }
                    } else {
                        Step::Done
                    }
                }
                "escape" => Step::CloseMenu,
                _ => return false,
            }
        };
        match step {
            Step::Done => cx.notify(),
            Step::CloseMenu => self.dismiss(window, cx),
            Step::Run(action) => {
                self.close_menu(window, cx);
                // The row may update the workspace, which is the view being updated
                // here: the action waits until this update is over, as a key shortcut
                // of the menu does.
                let handle = window.window_handle();
                cx.defer(move |cx| {
                    handle.update(cx, |_, window, cx| action(window, cx)).ok();
                });
            }
        }
        true
    }

    /// The pointer entered or left a row of the open menu. Opening a submenu waits a
    /// little, so a diagonal move into it does not close it again on the way.
    pub fn menu_hover(&mut self, path: &[usize], entered: bool, cx: &mut Context<Self>) {
        if !entered {
            return;
        }
        let Some(Overlay::Menu { entries, state, .. }) = self.overlay.as_mut() else {
            return;
        };
        let already = state.is_submenu_open(path);
        state.hover(path);
        let opens = !already
            && state
                .entry(entries, path)
                .is_some_and(MenuEntry::has_submenu);
        cx.notify();
        if !opens {
            if !already {
                self.menu_task = None;
            }
            return;
        }
        let serial = self.overlay_serial;
        let path = path.to_vec();
        self.menu_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(MENU_OPEN_DELAY).await;
            this.update(cx, |this, cx| {
                if this.overlay_serial != serial {
                    return;
                }
                let Some(Overlay::Menu { entries, state, .. }) = this.overlay.as_mut() else {
                    return;
                };
                if state.is_highlighted(&path) && state.open_row(entries, &path) {
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    /// A click on a row with a submenu opens it at once.
    pub fn menu_open(&mut self, path: &[usize], cx: &mut Context<Self>) {
        let Some(Overlay::Menu { entries, state, .. }) = self.overlay.as_mut() else {
            return;
        };
        state.hover(path);
        state.open_row(entries, path);
        cx.notify();
    }

    pub fn notify_user(
        &mut self,
        title: impl Into<SharedString>,
        text: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) {
        self.balloon = Some(Balloon {
            title: title.into(),
            text: text.into(),
            action: None,
            buttons: Vec::new(),
        });
        self.balloon_visible = true;
        self.balloon_serial += 1;
        cx.notify();
    }

    /// A notification with one button that runs a command of the window.
    pub fn notify_user_with_command(
        &mut self,
        title: impl Into<SharedString>,
        text: impl Into<SharedString>,
        button: impl Into<SharedString>,
        command: &'static str,
        cx: &mut Context<Self>,
    ) {
        self.balloon = Some(Balloon {
            title: title.into(),
            text: text.into(),
            action: None,
            buttons: vec![BalloonButton {
                id: "command",
                label: button.into(),
                action: BalloonAction::Command(command),
            }],
        });
        self.balloon_visible = true;
        self.balloon_serial += 1;
        cx.notify();
    }

    /// A notification with one button that opens `action_path`.
    pub fn notify_user_with_action(
        &mut self,
        title: impl Into<SharedString>,
        text: impl Into<SharedString>,
        action: impl Into<SharedString>,
        action_path: PathBuf,
        cx: &mut Context<Self>,
    ) {
        self.balloon = Some(Balloon {
            title: title.into(),
            text: text.into(),
            action: Some((action.into(), action_path)),
            buttons: Vec::new(),
        });
        self.balloon_visible = true;
        self.balloon_serial += 1;
        cx.notify();
    }

    /// The last notification, for scripts and tests.
    pub fn last_notification(&self) -> Option<(SharedString, SharedString)> {
        self.balloon
            .as_ref()
            .map(|balloon| (balloon.title.clone(), balloon.text.clone()))
    }

    /// How many notifications the window has shown, for a test that tells a new one
    /// from a repeated one with the same text.
    pub fn notification_serial(&self) -> usize {
        self.balloon_serial
    }

    /// The button of the last notification and the file it opens.
    pub fn last_notification_action(&self) -> Option<(SharedString, PathBuf)> {
        self.balloon
            .as_ref()
            .and_then(|balloon| balloon.action.clone())
    }

    /// The buttons of the last notification, for scripts and tests.
    pub fn last_notification_buttons(&self) -> Vec<SharedString> {
        self.balloon
            .as_ref()
            .map(|balloon| {
                balloon
                    .buttons
                    .iter()
                    .map(|button| button.label.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(crate) fn toggle_balloon(&mut self, cx: &mut Context<Self>) {
        self.balloon_visible = !self.balloon_visible;
        self.balloon_serial += 1;
        cx.notify();
    }

    /// Hides the balloon as its close button does, for a script that has to know whether
    /// a notification is on screen.
    pub fn close_notification(&mut self, cx: &mut Context<Self>) {
        if self.balloon_visible {
            self.balloon_visible = false;
            self.balloon_serial += 1;
            cx.notify();
        }
    }

    /// Double Shift and Double Ctrl: the keymap says what they run. Alt alone, pressed
    /// and released without another key, picks the first section of the menu bar.
    pub(crate) fn modifiers_changed(
        &mut self,
        modifiers: Modifiers,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let held = HeldModifiers {
            ctrl: modifiers.control,
            alt: modifiers.alt,
            shift: modifiers.shift,
        };
        let at = Instant::now();
        let key_between = self.key_between.replace(false);
        if key_between {
            self.keys.key_seen();
        }
        let alt_before = self.menu_bar_alt.replace(modifiers.alt);
        if alt_before != modifiers.alt {
            cx.notify();
        }
        if alt_before
            && !modifiers.alt
            && !key_between
            && !modifiers.control
            && !modifiers.shift
            && !self.has_overlay()
        {
            self.menu_bar_focus.set(Some(0));
            window.focus(&self.focus_handle, cx);
            cx.notify();
        }
        if let Dispatch::Action(id) = self.keys.modifiers_changed(&self.keymap, held, at) {
            self.run_command(id, window, cx);
        }
    }

    /// Every key down reaches here before the focused element, so Ctrl+Z and similar
    /// keys inside an editor break a double press of a modifier.
    pub(crate) fn note_key(&mut self, event: &KeyDownEvent) {
        if is_modifier_key(&event.keystroke.key) {
            return;
        }
        self.keys.key_seen();
    }

    /// Keys the editor and the popups did not take.
    pub(crate) fn key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = event.keystroke.key.as_str();
        let modifiers = event.keystroke.modifiers;
        // Command is the primary modifier of macOS; the Super key of the other systems is
        // not ours and never reaches the keymap.
        if (modifiers.platform && !cfg!(target_os = "macos"))
            || matches!(
                key,
                "shift" | "control" | "alt" | "platform" | "cmd" | "command"
            )
        {
            return;
        }
        if self.overlay.is_some() && self.menu_key(event, window, cx) {
            cx.stop_propagation();
            return;
        }
        let stroke = Keystroke {
            ctrl: modifiers.control,
            alt: modifiers.alt,
            shift: modifiers.shift,
            cmd: modifiers.platform,
            key: key.to_lowercase(),
        };
        let contexts = [keymap::workspace::CONTEXT];
        match self.keys.key_down(&self.keymap, stroke, &contexts) {
            Dispatch::Action(id) => {
                cx.stop_propagation();
                self.run_command(id, window, cx);
            }
            Dispatch::Pending => cx.stop_propagation(),
            Dispatch::Unhandled => {}
        }
    }

    /// Adds a command. A later one with the same id replaces the earlier.
    pub fn register_command(
        &mut self,
        id: &'static str,
        title: impl Into<String>,
        handler: impl Fn(&mut Workspace, &mut Window, &mut Context<Workspace>) + 'static,
    ) {
        self.commands
            .register(id, title, keymap::workspace::CONTEXT);
        self.handlers.insert(id, Rc::new(handler));
    }

    /// Binds `key`, `ctrl-alt-u` or a chord, to a registered command on top of the keymap.
    /// A later binding of the same key wins.
    pub fn bind_command(&mut self, key: &str, id: &'static str) -> Result<(), String> {
        let shortcut = keymap::Shortcut::parse(key).map_err(|error| error.to_string())?;
        self.keymap_actions.register(&[id]);
        self.keymap
            .bind(
                &self.keymap_actions,
                keymap::workspace::CONTEXT,
                &shortcut,
                id,
            )
            .map_err(|error| error.to_string())?;
        if !self
            .extra_bindings
            .iter()
            .any(|(bound, action)| *action == id && *bound == shortcut)
        {
            self.extra_bindings.push((shortcut, id));
        }
        Ok(())
    }

    /// Lists a command that something else runs, such as an editor action, so that
    /// Find Action shows it. It has no handler here.
    pub fn list_command(&mut self, id: &'static str, title: impl Into<String>) {
        self.commands.register(id, title, keymap::editor::CONTEXT);
    }

    pub fn commands(&self) -> &CommandRegistry {
        &self.commands
    }

    /// The keys of a command as the menus write them, `ctrl-shift-n`.
    pub fn shortcut_of(&self, id: &str) -> Option<String> {
        let shortcuts = self.keymap.shortcuts_for(id);
        shortcuts.first().map(ToString::to_string)
    }

    /// The key of a command id or of a window action name, as the keymap writes it,
    /// `ctrl-shift-n`. The menu of the window spells the label from it; the menu of the
    /// screen of macOS gives it to GPUI, which shows the key beside the row.
    pub fn key_of_key(&self, key: &str) -> Option<String> {
        self.shortcut_of(key)
            .or_else(|| window_key(key, keymap::platform()).map(str::to_owned))
            .or_else(|| {
                // Ctrl+Alt+Y runs Synchronize, which is Reload All from Disk; Tab indents
                // the selection, which is Indent Selection.
                match key {
                    "file.reload_all" => window_key("workspace::Synchronize", keymap::platform()),
                    "editor.indent_selection" => Some("tab"),
                    _ => None,
                }
                .map(str::to_owned)
            })
    }

    /// The key of a command id or of a window action name, as a menu writes it. The
    /// menu does not spell keys itself: they come from the keymap or `WINDOW_KEYS`.
    pub fn shortcut_of_key(&self, key: &str) -> Option<String> {
        self.key_of_key(key).map(|text| shortcut_label(&text))
    }

    /// Whether the keymap already takes Alt+`key`. A binding wins over the same letter
    /// as a menu bar mnemonic.
    pub(crate) fn alt_letter_is_bound(&self, key: &str) -> bool {
        let stroke = Keystroke {
            ctrl: false,
            alt: true,
            shift: false,
            cmd: false,
            key: key.to_lowercase(),
        };
        !matches!(
            self.keymap.lookup(
                std::slice::from_ref(&stroke),
                &[keymap::workspace::CONTEXT, keymap::editor::CONTEXT]
            ),
            keymap::Lookup::None
        )
    }

    /// False when nothing here runs `id`.
    pub fn run_command(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(handler) = self.handlers.get(id).cloned() else {
            return false;
        };
        handler(self, window, cx);
        true
    }

    pub fn set_tab_listener(
        &mut self,
        listener: impl Fn(&Path, &mut Context<Workspace>) + 'static,
    ) {
        self.tab_listener = Some(Rc::new(listener));
    }

    /// The view of the active tab, for the commands that act on the editor.
    pub fn active_view(&self) -> Option<AnyView> {
        self.active_tab().map(|tab| tab.editor.view.clone())
    }

    pub fn active_path(&self) -> Option<PathBuf> {
        self.active_tab().map(|tab| tab.path.clone())
    }

    /// The view of the tab that holds `path`, if one is open.
    pub fn view_of(&self, path: &Path) -> Option<AnyView> {
        let key = ::platform::path_key(path);
        let mut tabs = self.panes.iter().flat_map(|pane| &pane.tabs);
        tabs.find(|tab| ::platform::path_key(&tab.path) == key)
            .map(|tab| tab.editor.view.clone())
    }

    fn capture(&self, cx: &App) -> Layout {
        let pane_state = |pane: &Pane| PaneState {
            tabs: pane.tabs.iter().map(|tab| tab.path.clone()).collect(),
            active: pane.active,
            carets: pane.tabs.iter().map(|tab| (tab.editor.caret)(cx)).collect(),
            pinned: pane
                .tabs
                .iter()
                .filter(|tab| tab.pinned)
                .map(|tab| tab.path.clone())
                .collect(),
        };
        // A group without tabs is not saved, so the active one is counted without them.
        let before = self.panes[..self.active_pane]
            .iter()
            .filter(|pane| !pane.tabs.is_empty())
            .count();
        let panes = self.panes.iter().filter(|pane| !pane.tabs.is_empty());
        Layout {
            window: self.placement,
            left: self.left.state(),
            right: self.right.state(),
            bottom: self.bottom.state(),
            panes: panes.map(pane_state).collect(),
            active_pane: before,
            split_down: self.split_down,
            tree_expanded: self.layout.tree_expanded.clone(),
            tree_selected: self.layout.tree_selected.clone(),
            bottom_tool: self.bottom.active_tool().map(|tool| tool.id.to_string()),
            recent_edited: self.recent_edited.clone(),
        }
    }

    /// Saves a moment after the last change, off the UI thread.
    fn layout_changed(&mut self, cx: &mut Context<Self>) {
        self.save_serial += 1;
        let serial = self.save_serial;
        let delay = cx.background_executor().timer(SAVE_DELAY);
        cx.spawn(async move |this, cx| {
            delay.await;
            this.update(cx, |this, cx| {
                if this.save_serial == serial {
                    this.save_in_background(cx);
                }
            })
            .ok();
        })
        .detach();
    }

    fn fresh_json(&mut self, cx: &App) -> Option<String> {
        let json = serde_json::to_string(&self.capture(cx)).ok()?;
        (json != self.saved_json).then(|| {
            self.saved_json.clone_from(&json);
            json
        })
    }

    fn save_in_background(&mut self, cx: &mut Context<Self>) {
        let (Some(store), Some(json)) = (self.store.clone(), self.fresh_json(cx)) else {
            return;
        };
        let root = self.root.clone();
        let write = async move { save_layout(&store, &root, &json) };
        let write = cx.background_executor().spawn(write);
        cx.spawn(async move |_, _| {
            if let Err(error) = write.await {
                eprintln!("layout is not saved: {error}");
            }
        })
        .detach();
    }

    /// Closes the window the way its close button does.
    pub fn close_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.save_now(window, cx);
        window.remove_window();
    }

    /// File > Exit: closes every window of the application. `save` writes the unsaved text
    /// first; without it the windows close as they are.
    pub fn close_all_windows(&mut self, save: bool, window: &mut Window, cx: &mut Context<Self>) {
        if save {
            self.save_all_now(cx);
        }
        self.skip_save_on_close = !save;
        let current = window.window_handle();
        for handle in cx.windows() {
            let Some(handle) = handle.downcast::<Workspace>() else {
                continue;
            };
            if Some(handle) == current.downcast::<Workspace>() {
                window.remove_window();
                continue;
            }
            handle
                .update(cx, |workspace, window, _| {
                    workspace.skip_save_on_close = !save;
                    window.remove_window();
                })
                .ok();
        }
    }

    /// The window is going away, there is no later moment to write.
    fn save_now(&mut self, window: &Window, cx: &mut App) {
        for tab in self.panes.iter().flat_map(|pane| &pane.tabs) {
            (tab.editor.save_now)(cx);
        }
        self.placement = placement_of(window).or(self.placement);
        if let (Some(store), Some(json)) = (self.store.clone(), self.fresh_json(cx))
            && let Err(error) = save_layout(&store, &self.root, &json)
        {
            eprintln!("layout is not saved: {error}");
        }
    }
}

impl Focusable for Workspace {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

/// A key that only modifies others: it never breaks a Double Shift or a Double Ctrl.
fn is_modifier_key(key: &str) -> bool {
    matches!(key, "shift" | "control" | "alt" | "platform" | "cmd" | "fn")
}
