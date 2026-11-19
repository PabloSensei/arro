//! The terminal tool window: the sessions of the user in tabs, the split of a tab into
//! two screens, the search over the scrollback and the links of what the programs print.

use std::cell::Cell as SharedCell;
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use futures_util::StreamExt;
use keymap::Platform;
use project_model::EnvironmentProvider;
use terminal::{SearchMatch, SearchQuery, ShiftEnter, SpawnSpec, TERM_VARS, TerminalSession};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::{Settings, ToggleTerminal, Workspace};

use crate::navigation;
use crate::project_structure::dialog_button;
use crate::prompt::{Place, Prompt, TextField};
use crate::term_grid::{
    CellMetrics, GridPanel, GridView, Position, Selection, every_frame, grid, pressed,
    scroll_lines, send_key,
};
use crate::term_links::{LinkKind, LinkText};
use crate::term_tabs::{self, Split, TabState, TabsState};

actions!(
    terminal_panel,
    [
        NewTab,
        CloseTab,
        PreviousTab,
        NextTab,
        Find,
        CopySelection,
        PasteText
    ]
);

const GROUP: &str = "bottom-panel";

/// The `bin` folder of a build tool the IDE downloaded, which the terminals may put on
/// their `PATH`. The window fills it when the tool is found, and a setting decides
/// whether the next shell sees it.
#[derive(Clone, Debug, Default)]
pub struct ToolTerminalPath(pub Option<PathBuf>);

impl Global for ToolTerminalPath {}
/// How the two panes of a split share the room when the user splits a tab.
const SPLIT_RATIO: f32 = 0.5;
/// How near the middle a dragged divider may come, so that neither pane goes away.
const SPLIT_LIMIT: f32 = 0.15;

/// The panel of the window, for scripts.
pub struct ActiveTerminal(pub Entity<TerminalPanel>);

impl Global for ActiveTerminal {}

enum State {
    Starting,
    Running(Arc<TerminalSession>),
    Failed(SharedString),
}

/// Where a tab stands: the sessions it holds and how they share its room.
struct Tab {
    name: String,
    directory: PathBuf,
    /// The command line of the shell of the tab, `None` for the one of the settings.
    shell: Option<String>,
    panes: Vec<Pane>,
    active: usize,
    split: Option<Split>,
    ratio: f32,
}

impl Tab {
    fn new(name: String, directory: PathBuf, shell: Option<String>, cx: &mut App) -> Self {
        Self {
            name,
            directory,
            shell,
            panes: vec![Pane::new(cx)],
            active: 0,
            split: None,
            ratio: SPLIT_RATIO,
        }
    }

    fn active(&self) -> usize {
        self.active.min(self.panes.len().saturating_sub(1))
    }

    fn pane(&self) -> &Pane {
        &self.panes[self.active()]
    }

    fn pane_mut(&mut self) -> &mut Pane {
        let active = self.active();
        &mut self.panes[active]
    }
}

/// One program of a tab and what the user did with its screen.
struct Pane {
    state: State,
    version: u64,
    exit_code: Option<u32>,
    /// The folder the shell reported with OSC 7, when it reports one.
    cwd: Option<PathBuf>,
    selection: Option<Selection>,
    /// The place the pointer pressed, while the button is held.
    dragging: Option<Position>,
    /// The link under the pointer with the control key held.
    hover: Option<SearchMatch>,
    search: Search,
    /// Where the last frame put the grid, for a press and for the divider.
    metrics: Rc<SharedCell<Option<CellMetrics>>>,
}

impl Pane {
    fn new(_cx: &mut App) -> Self {
        Self {
            state: State::Starting,
            version: 0,
            exit_code: None,
            cwd: None,
            selection: None,
            dragging: None,
            hover: None,
            search: Search::default(),
            metrics: Rc::new(SharedCell::new(None)),
        }
    }

    fn session(&self) -> Option<&Arc<TerminalSession>> {
        match &self.state {
            State::Running(session) => Some(session),
            _ => None,
        }
    }
}

/// The search bar of one pane and what the last search found.
#[derive(Default)]
struct Search {
    open: bool,
    field: TextField,
    case_sensitive: bool,
    regex: bool,
    /// The query the results belong to.
    generation: u64,
    results: terminal::SearchResults,
    /// The match the search moved to, an index into `results.matches`.
    current: Option<usize>,
    /// A new query waits for its first answer before it moves the view.
    jump: bool,
}

impl Search {
    fn matches(&self) -> &[SearchMatch] {
        &self.results.matches
    }
}

/// The divider of a split under the pointer.
struct SplitDrag {
    start: Point<Pixels>,
    ratio: f32,
}

pub struct TerminalPanel {
    root: PathBuf,
    workspace: WeakEntity<Workspace>,
    environment: Option<Arc<dyn EnvironmentProvider>>,
    store: Option<Arc<store::Store>>,
    tabs: Vec<Tab>,
    active: usize,
    /// The keyboard platform the panel was opened with: Command leads on macOS.
    keys_platform: Platform,
    focus: FocusHandle,
    search_focus: FocusHandle,
    /// The tab whose name is being typed, and the field of the new name.
    renaming: Option<(usize, TextField)>,
    rename_focus: FocusHandle,
    dragging_split: Option<SplitDrag>,
    /// The area of the panel on screen, for the questions that stand over it.
    bounds: Rc<RefCell<Option<Bounds<Pixels>>>>,
    /// Whether a harness held the IDE protocol when the panel last looked.
    ide_connected: bool,
    /// The client whose command started in the shell and whose settings have no entry of
    /// Arro, for the bar above the grid.
    terminal_suggestion: Option<mcp_discovery::Client>,
    /// Numbers the reads behind the bar, so a later command wins.
    suggestion_generation: u64,
}

/// The client a command line of the prompt starts, when it is `claude` or `codex`.
fn client_of_command(line: &str) -> Option<mcp_discovery::Client> {
    let command = line.rsplit(['$', '>', '#', '%']).next()?.trim_start();
    let word = command.split_whitespace().next()?;
    let program = word.rsplit(['\\', '/']).next().unwrap_or(word);
    let program = program.strip_suffix(".exe").unwrap_or(program);
    match program {
        "claude" => Some(mcp_discovery::Client::ClaudeCode),
        "codex" => Some(mcp_discovery::Client::Codex),
        _ => None,
    }
}

/// The shell sees what the user sees in any console, plus the terminal variables and the
/// ones the project decides, so that its SDK is the SDK of the build.
fn shell_spec(
    root: PathBuf,
    directory: PathBuf,
    shell: Option<&str>,
    variables: Vec<(String, String)>,
    ide: Vec<(String, String)>,
    settings: &Settings,
    tool_path: Option<PathBuf>,
) -> SpawnSpec {
    let shell = match shell {
        Some(shell) if !shell.trim().is_empty() => shell.to_owned(),
        _ => match settings.terminal_shell.trim() {
            "" => terminal::default_shell().unwrap_or_else(|| {
                if cfg!(windows) {
                    std::env::var("ComSpec").unwrap_or_else(|_| "cmd.exe".to_string())
                } else {
                    "/bin/sh".to_string()
                }
            }),
            configured => configured.to_owned(),
        },
    };
    let cwd = if directory.is_dir() {
        directory
    } else {
        start_directory(settings, &root)
    };
    let mut env: Vec<(String, String)> = std::env::vars().collect();
    for (key, value) in TERM_VARS {
        env.retain(|(known, _)| !known.eq_ignore_ascii_case(key));
        env.push((key.to_string(), value.to_string()));
    }
    for (key, value) in variables {
        env.retain(|(known, _)| !known.eq_ignore_ascii_case(&key));
        env.push((key, value));
    }
    for (key, value) in ide {
        env.retain(|(known, _)| !known.eq_ignore_ascii_case(&key));
        env.push((key, value));
    }
    for (key, value) in terminal_environment(&settings.terminal_environment) {
        env.retain(|(known, _)| !known.eq_ignore_ascii_case(&key));
        env.push((key, value));
    }
    // The run panel names the folder of the downloaded tool only when its settings page
    // asks for it, so the switch lives there and not here.
    if let Some(bin) = tool_path {
        prepend_path(&mut env, &bin);
    }
    SpawnSpec {
        command_line: shell,
        cwd: Some(cwd),
        env,
        cols: 120,
        rows: 12,
        shift_enter: ShiftEnter::default(),
    }
}

/// Puts `bin` in front of the `PATH` of the shell, keeping the rest of it.
fn prepend_path(env: &mut Vec<(String, String)>, bin: &Path) {
    let separator = if cfg!(windows) { ';' } else { ':' };
    let bin = bin.display().to_string();
    match env
        .iter_mut()
        .find(|(name, _)| name.eq_ignore_ascii_case("PATH"))
    {
        Some((_, value)) if value.is_empty() => *value = bin,
        Some((_, value)) => *value = format!("{bin}{separator}{value}"),
        None => env.push(("PATH".to_owned(), bin)),
    }
}

/// A press the terminal takes for itself instead of writing it into the shell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TerminalCommand {
    NewTab,
    CloseTab,
    Find,
    Copy,
    /// Copies a selection and otherwise goes to the shell, where Ctrl+C interrupts.
    CopyIfSelected,
    Paste,
}

/// The keys of the terminal that act on the window rather than reach the shell. macOS
/// takes them with Command, as IDEA does, and leaves Control to the shell: Ctrl+C there
/// interrupts the program. The other systems keep the Control keys of IDEA.
struct TerminalKeys {
    new_tab: &'static [&'static str],
    close_tab: &'static [&'static str],
    find: &'static [&'static str],
    copy: &'static [&'static str],
    copy_selection: &'static [&'static str],
    paste: &'static [&'static str],
}

const TERMINAL_KEYS_WINDOWS: TerminalKeys = TerminalKeys {
    new_tab: &["ctrl-shift-t"],
    close_tab: &["ctrl-f4", "ctrl-shift-f4"],
    find: &["ctrl-f"],
    copy: &["ctrl-shift-c"],
    copy_selection: &["ctrl-c"],
    paste: &["ctrl-shift-v", "ctrl-v", "shift-insert"],
};

const TERMINAL_KEYS_MACOS: TerminalKeys = TerminalKeys {
    new_tab: &["cmd-t", "cmd-shift-t"],
    close_tab: &["cmd-w"],
    find: &["cmd-f"],
    copy: &["cmd-c"],
    // Control belongs to the shell on macOS: Ctrl+C keeps interrupting the program.
    copy_selection: &[],
    paste: &["cmd-v"],
};

/// The keys of the terminal of one platform, for the bindings and the press handler.
fn terminal_keys(platform: Platform) -> &'static TerminalKeys {
    if platform.uses_command() {
        &TERMINAL_KEYS_MACOS
    } else {
        &TERMINAL_KEYS_WINDOWS
    }
}

/// Whether a press is the key of `shortcut`, as the keymap writes it.
fn is_key(stroke: &Keystroke, shortcut: &str) -> bool {
    let Ok(wanted) = keymap::Keystroke::parse(shortcut) else {
        return false;
    };
    let modifiers = stroke.modifiers;
    wanted.key == stroke.key
        && wanted.ctrl == modifiers.control
        && wanted.alt == modifiers.alt
        && wanted.shift == modifiers.shift
        && wanted.cmd == modifiers.platform
}

/// What a press means to the terminal of `platform`, `None` when the shell gets it.
fn terminal_command(stroke: &Keystroke, platform: Platform) -> Option<TerminalCommand> {
    let keys = terminal_keys(platform);
    let any = |shortcuts: &[&str]| shortcuts.iter().any(|shortcut| is_key(stroke, shortcut));
    if any(keys.new_tab) {
        Some(TerminalCommand::NewTab)
    } else if any(keys.close_tab) {
        Some(TerminalCommand::CloseTab)
    } else if any(keys.find) {
        Some(TerminalCommand::Find)
    } else if any(keys.copy) {
        Some(TerminalCommand::Copy)
    } else if any(keys.copy_selection) {
        Some(TerminalCommand::CopyIfSelected)
    } else if any(keys.paste) {
        Some(TerminalCommand::Paste)
    } else {
        None
    }
}

/// The keys of the terminal that the window would take for a tab of the editor or for
/// going back: in the terminal they mean the sessions, as in IDEA.
pub(crate) fn bind_keys(cx: &mut App) {
    bind_keys_for(cx, keymap::platform());
}

/// Binds the terminal keys of one platform, so that a test reads the macOS keys on every
/// runner.
pub(crate) fn bind_keys_for(cx: &mut App, platform: Platform) {
    let keys = terminal_keys(platform);
    let context = Some("Terminal");
    for key in keys.new_tab {
        cx.bind_keys([KeyBinding::new(key, NewTab, context)]);
    }
    for key in keys.close_tab {
        cx.bind_keys([KeyBinding::new(key, CloseTab, context)]);
    }
    for key in keys.find {
        cx.bind_keys([KeyBinding::new(key, Find, context)]);
    }
    for key in keys.copy {
        cx.bind_keys([KeyBinding::new(key, CopySelection, context)]);
    }
    for key in keys.paste {
        cx.bind_keys([KeyBinding::new(key, PasteText, context)]);
    }
    cx.bind_keys([
        KeyBinding::new("alt-left", PreviousTab, context),
        KeyBinding::new("alt-right", NextTab, context),
    ]);
}

/// Where a new terminal starts: the setting, the root of the project when it is empty
/// or gone. A relative path counts from the root.
fn start_directory(settings: &Settings, root: &Path) -> PathBuf {
    let chosen = settings.terminal_start_directory.trim();
    if chosen.is_empty() {
        return root.to_path_buf();
    }
    let path = PathBuf::from(chosen);
    let path = if path.is_absolute() {
        path
    } else {
        root.join(path)
    };
    if path.is_dir() {
        path
    } else {
        root.to_path_buf()
    }
}

/// The `NAME=value` lines of the setting, in their order. A line without `=` is skipped.
pub(crate) fn terminal_environment(text: &str) -> Vec<(String, String)> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| {
            let (name, value) = line.split_once('=')?;
            let name = name.trim();
            (!name.is_empty()).then(|| (name.to_owned(), value.trim().to_owned()))
        })
        .collect()
}

impl TerminalPanel {
    pub fn create(
        workspace: WeakEntity<Workspace>,
        root: PathBuf,
        environment: Option<Arc<dyn EnvironmentProvider>>,
        store: Option<Arc<store::Store>>,
        cx: &mut App,
    ) -> Entity<Self> {
        let saved = store
            .as_ref()
            .map(|store| term_tabs::load(store, &root))
            .unwrap_or_default();
        cx.new(|cx| {
            let settings = cx.try_global::<Settings>().cloned().unwrap_or_default();
            let mut tabs: Vec<Tab> = saved
                .restored()
                .into_iter()
                .map(|tab| Tab::new(tab.name, tab.directory, tab.shell, cx))
                .collect();
            if tabs.is_empty() {
                let directory = start_directory(&settings, &root);
                tabs.push(Tab::new(
                    term_tabs::next_name(Vec::new()),
                    directory,
                    None,
                    cx,
                ));
            }
            let active = saved.active().min(tabs.len() - 1);
            let mut panel = Self {
                root,
                workspace,
                environment,
                store,
                tabs,
                active,
                keys_platform: keymap::platform(),
                focus: cx.focus_handle(),
                search_focus: cx.focus_handle(),
                renaming: None,
                rename_focus: cx.focus_handle(),
                dragging_split: None,
                bounds: Rc::new(RefCell::new(None)),
                ide_connected: false,
                terminal_suggestion: None,
                suggestion_generation: 0,
            };
            for tab in 0..panel.tabs.len() {
                for pane in 0..panel.tabs[tab].panes.len() {
                    panel.spawn(tab, pane, cx);
                }
            }
            every_frame(cx, Self::refresh);
            panel
        })
    }

    pub fn focus(&self) -> FocusHandle {
        self.focus.clone()
    }

    /// The client whose bar the panel shows, for tests.
    #[cfg(all(test, windows))]
    pub(crate) fn suggestion_client(&self) -> Option<mcp_discovery::Client> {
        self.terminal_suggestion
    }

    /// The session of the tab the user looks at.
    pub fn session(&self) -> Option<&Arc<TerminalSession>> {
        self.tabs.get(self.active)?.pane().session()
    }

    /// What the panel shows now, for tests: the shell, its start or the reason it failed.
    #[cfg(test)]
    pub fn report(&self) -> String {
        self.pane_report(self.active, 0)
    }

    #[cfg(test)]
    fn pane_report(&self, tab: usize, pane: usize) -> String {
        let Some(pane) = self.tabs.get(tab).and_then(|tab| tab.panes.get(pane)) else {
            return "no pane".to_owned();
        };
        match &pane.state {
            State::Starting => "starting".to_owned(),
            State::Running(_) => "running".to_owned(),
            State::Failed(text) => text.to_string(),
        }
    }

    /// Types a command line into the shell and runs it, the way the user would.
    pub fn run_command(&self, line: &str) -> Result<(), String> {
        let session = self.session().ok_or("The shell is not running")?;
        session
            .send(format!("{line}\r"))
            .map_err(|error| format!("The shell did not take the command: {error:?}"))
    }

    fn spawn(&mut self, tab: usize, pane: usize, cx: &mut Context<Self>) {
        let Some(chosen) = self.tabs.get(tab) else {
            return;
        };
        let (directory, shell) = (chosen.directory.clone(), chosen.shell.clone());
        let root = self.root.clone();
        let environment = self.environment.clone();
        let settings = cx.try_global::<Settings>().cloned().unwrap_or_default();
        let tool_path = cx
            .try_global::<ToolTerminalPath>()
            .and_then(|path| path.0.clone());
        let ide = cx
            .try_global::<crate::claude_ide::ActiveClaude>()
            .map(crate::claude_ide::ActiveClaude::environment)
            .unwrap_or_default();
        let spawn = cx.background_executor().spawn(async move {
            let variables = environment
                .map(|environment| environment.describe().variables)
                .unwrap_or_default();
            TerminalSession::spawn(&shell_spec(
                root,
                directory,
                shell.as_deref(),
                variables,
                ide,
                &settings,
                tool_path,
            ))
        });
        cx.spawn(async move |this, cx| {
            let result = spawn.await;
            this.update(cx, |this, cx| {
                let Some(pane) = this
                    .tabs
                    .get_mut(tab)
                    .and_then(|tab| tab.panes.get_mut(pane))
                else {
                    return;
                };
                pane.state = match result {
                    Ok(session) => {
                        ::platform::label_process(session.pid(), "Terminal");
                        State::Running(Arc::new(session))
                    }
                    Err(error) => State::Failed(format!("The shell did not start: {error}").into()),
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let mut changed = false;
        let connected = cx
            .try_global::<crate::claude_ide::ActiveClaude>()
            .is_some_and(|active| active.connected());
        if connected != self.ide_connected {
            self.ide_connected = connected;
            changed = true;
        }
        for tab in &mut self.tabs {
            for pane in &mut tab.panes {
                let Some(session) = pane.session().cloned() else {
                    continue;
                };
                let screen = session.screen();
                let version = screen.version;
                let exit_code = session.exit_code();
                if version != pane.version || exit_code != pane.exit_code {
                    pane.version = version;
                    pane.exit_code = exit_code;
                    changed = true;
                }
                if pane.search.open {
                    let results = session.search_results();
                    if *results != pane.search.results {
                        // An answer of an older query is kept as it is: the one of the
                        // query in the field comes next and replaces it.
                        let current = results.generation == pane.search.generation;
                        let jump = current && pane.search.jump;
                        if current {
                            pane.search.jump = false;
                        }
                        pane.search.results = (*results).clone();
                        let count = pane.search.matches().len();
                        if current {
                            pane.search.current = if count == 0 {
                                None
                            } else if jump {
                                Some(0)
                            } else {
                                pane.search.current.map(|current| current.min(count - 1))
                            };
                        }
                        if jump && let Some(index) = pane.search.current {
                            session.reveal_match(index);
                        }
                        changed = true;
                    }
                }
                let cwd = screen.cwd.clone();
                if let Some(cwd) = cwd {
                    let directory = crate::term_links::directory(&cwd);
                    if directory != pane.cwd {
                        pane.cwd = directory;
                        changed = true;
                    }
                }
            }
        }
        if changed {
            cx.notify();
        }
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let stroke = &event.keystroke;
        let modifiers = stroke.modifiers;
        match terminal_command(stroke, self.keys_platform) {
            Some(TerminalCommand::NewTab) => {
                cx.stop_propagation();
                self.new_tab(None, window, cx);
                return;
            }
            Some(TerminalCommand::CloseTab) => {
                cx.stop_propagation();
                self.close_tab(self.active, window, cx);
                return;
            }
            Some(TerminalCommand::Find) => {
                cx.stop_propagation();
                self.open_search(window, cx);
                return;
            }
            Some(TerminalCommand::Copy) => {
                cx.stop_propagation();
                self.copy_selection(cx);
                return;
            }
            Some(TerminalCommand::CopyIfSelected) => {
                if self.copy_selection(cx) {
                    cx.stop_propagation();
                }
                // Without a selection the press goes to the shell, where Ctrl+C interrupts.
            }
            Some(TerminalCommand::Paste) => {
                cx.stop_propagation();
                self.paste(window, cx);
                return;
            }
            None => {}
        }
        if modifiers.alt
            && !modifiers.control
            && matches!(stroke.key.as_str(), "left" | "arrowleft")
        {
            cx.stop_propagation();
            self.step_tab(-1, window, cx);
            return;
        }
        if modifiers.alt
            && !modifiers.control
            && matches!(stroke.key.as_str(), "right" | "arrowright")
        {
            cx.stop_propagation();
            self.step_tab(1, window, cx);
            return;
        }
        if self.search_open() && matches!(stroke.key.as_str(), "f3") {
            cx.stop_propagation();
            self.search_step(!modifiers.shift, cx);
            return;
        }
        let Some(session) = self.session().cloned() else {
            return;
        };
        // A finished shell starts again on Enter.
        if session.is_closed() {
            if stroke.key == "enter" {
                cx.stop_propagation();
                let tab = self.active;
                let pane = self.tabs[tab].active();
                self.restart(tab, pane, cx);
            }
            return;
        }
        if stroke.key == "enter" && !modifiers.control && !modifiers.alt {
            self.note_terminal_command(cx);
        }
        if send_key(&session, stroke, self.keys_platform, cx) {
            cx.stop_propagation();
        }
    }

    /// The command line the prompt holds, for the bar of the MCP clients: `claude` and
    /// `codex` start without an entry of Arro, so the window offers to write one.
    fn note_terminal_command(&mut self, cx: &mut Context<Self>) {
        let enabled = cx
            .try_global::<Settings>()
            .is_none_or(|settings| settings.mcp_terminal_suggestions);
        if !enabled {
            return;
        }
        let Some(session) = self.session().cloned() else {
            return;
        };
        let screen = session.screen();
        let Some((row, _)) = screen.cursor else {
            return;
        };
        let Some(client) = client_of_command(&screen.line(row)) else {
            return;
        };
        self.suggestion_generation += 1;
        let generation = self.suggestion_generation;
        let file = client.user_config(&crate::settings::mcp_home_folder(cx));
        let read = cx
            .background_executor()
            .spawn(async move { mcp_discovery::has_entry(client, &file).unwrap_or(false) });
        cx.spawn(async move |this, cx| {
            let configured = read.await;
            this.update(cx, |this, cx| {
                if this.suggestion_generation == generation {
                    this.terminal_suggestion = (!configured).then_some(client);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// The presses of the search bar, before the ones of the terminal.
    fn search_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let stroke = &event.keystroke;
        if stroke.key == "escape" {
            cx.stop_propagation();
            self.close_search(window, cx);
            return;
        }
        if stroke.key == "enter" {
            cx.stop_propagation();
            self.search_step(!stroke.modifiers.shift, cx);
            return;
        }
        let changed = match self.tabs.get_mut(self.active).map(|tab| tab.pane_mut()) {
            Some(pane) => pane.search.field.key_with_clipboard(event, false, cx),
            None => false,
        };
        if changed {
            cx.stop_propagation();
            self.run_search(cx);
        }
    }

    fn search_open(&self) -> bool {
        self.tabs
            .get(self.active)
            .is_some_and(|tab| tab.pane().search.open)
    }

    fn open_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let open = match self.tabs.get_mut(self.active).map(|tab| tab.pane_mut()) {
            Some(pane) => {
                pane.search.open = true;
                !pane.search.field.text.is_empty()
            }
            None => return,
        };
        window.focus(&self.search_focus, cx);
        if open {
            self.run_search(cx);
        }
        cx.notify();
    }

    fn close_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let close = match self.tabs.get_mut(self.active).map(|tab| tab.pane_mut()) {
            Some(pane) => {
                pane.search.open = false;
                pane.search.results = terminal::SearchResults::default();
                pane.search.current = None;
                pane.search.jump = false;
                pane.search.generation += 1;
                pane.session().cloned()
            }
            None => return,
        };
        if let Some(session) = close {
            session.clear_search();
        }
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn run_search(&mut self, cx: &mut Context<Self>) {
        let Some(pane) = self.tabs.get_mut(self.active).map(|tab| tab.pane_mut()) else {
            return;
        };
        let Some(session) = pane.session().cloned() else {
            return;
        };
        pane.search.generation += 1;
        pane.search.results = terminal::SearchResults::default();
        pane.search.current = None;
        pane.search.jump = true;
        let query = SearchQuery {
            text: pane.search.field.text.clone(),
            case_sensitive: pane.search.case_sensitive,
            regex: pane.search.regex,
        };
        if query.text.is_empty() {
            pane.search.jump = false;
            session.clear_search();
        } else {
            session.search(query, pane.search.generation);
        }
        cx.notify();
    }

    fn search_step(&mut self, forward: bool, cx: &mut Context<Self>) {
        let Some(pane) = self.tabs.get_mut(self.active).map(|tab| tab.pane_mut()) else {
            return;
        };
        let count = pane.search.matches().len();
        if count == 0 {
            return;
        }
        let next = match pane.search.current {
            Some(current) if forward => (current + 1) % count,
            Some(current) => (current + count - 1) % count,
            None if forward => 0,
            None => count - 1,
        };
        pane.search.current = Some(next);
        if let Some(session) = pane.session() {
            session.reveal_match(next);
        }
        cx.notify();
    }

    fn copy_selection(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(pane) = self.tabs.get(self.active).map(|tab| tab.pane()) else {
            return false;
        };
        let Some(session) = pane.session() else {
            return false;
        };
        crate::term_grid::copy_selection(session, pane.selection, cx)
    }

    fn select_all(&mut self, cx: &mut Context<Self>) {
        let Some(pane) = self.tabs.get_mut(self.active).map(|tab| tab.pane_mut()) else {
            return;
        };
        let Some(session) = pane.session() else {
            return;
        };
        let screen = session.screen();
        if screen.rows == 0 || screen.cols == 0 {
            return;
        }
        let offset = screen.display_offset as i32;
        pane.selection = Some(Selection::new(
            Position::new(-offset, 0),
            Position::new(
                screen.rows as i32 - 1 - offset,
                screen.cols.saturating_sub(1),
            ),
        ));
        cx.notify();
    }

    fn clear_pane(&mut self, cx: &mut Context<Self>) {
        if let Some(session) = self.session() {
            session.clear();
        }
        if let Some(pane) = self.tabs.get_mut(self.active).map(|tab| tab.pane_mut()) {
            pane.selection = None;
        }
        cx.notify();
    }

    fn paste(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        let Some(session) = self.session().cloned() else {
            return;
        };
        let multiline = text.contains('\n') || text.contains('\r');
        let confirm = cx
            .try_global::<Settings>()
            .is_none_or(|settings| settings.terminal_confirm_multiline_paste);
        if !multiline || !confirm {
            let _ = session.paste(&text);
            return;
        }
        let workspace = self.workspace.clone();
        let paste = text.clone();
        let place = self.bounds.borrow().map_or(Place::Center, |bounds| {
            Place::At(point(bounds.center().x, bounds.top() + px(SPACE_M)))
        });
        let _ = workspace.update(cx, |workspace, cx| {
            Prompt::ask_at(
                "Paste",
                "Paste multi-line text?",
                place,
                move |_| {
                    let _ = session.paste(&paste);
                },
                workspace,
                window,
                cx,
            );
        });
    }

    fn restart(&mut self, tab: usize, pane: usize, cx: &mut Context<Self>) {
        let Some(found) = self
            .tabs
            .get_mut(tab)
            .and_then(|tab| tab.panes.get_mut(pane))
        else {
            return;
        };
        found.state = State::Starting;
        found.selection = None;
        self.spawn(tab, pane, cx);
    }

    fn new_tab(&mut self, shell: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let settings = cx.try_global::<Settings>().cloned().unwrap_or_default();
        let directory = start_directory(&settings, &self.root);
        self.push_tab(directory, shell, window, cx);
    }

    /// Opens a tab in `directory`, as the tree of the project asks.
    pub fn open_tab_in(&mut self, directory: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.push_tab(directory, None, window, cx);
    }

    fn push_tab(
        &mut self,
        directory: PathBuf,
        shell: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = term_tabs::next_name(self.tabs.iter().map(|tab| tab.name.clone()));
        let tab = Tab::new(name, directory, shell, cx);
        self.tabs.push(tab);
        self.active = self.tabs.len() - 1;
        self.spawn(self.active, 0, cx);
        window.focus(&self.focus, cx);
        self.save_tabs(cx);
        cx.notify();
    }

    fn close_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.tabs.len() <= 1 || index >= self.tabs.len() {
            return;
        }
        self.tabs.remove(index);
        self.active = self.active.min(self.tabs.len() - 1);
        if self.renaming.as_ref().is_some_and(|(at, _)| *at == index) {
            self.renaming = None;
        }
        window.focus(&self.focus, cx);
        self.save_tabs(cx);
        cx.notify();
    }

    fn select_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.tabs.len() {
            return;
        }
        self.active = index;
        window.focus(&self.focus, cx);
        self.save_tabs(cx);
        cx.notify();
    }

    fn step_tab(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        if self.tabs.len() < 2 {
            return;
        }
        let count = self.tabs.len() as isize;
        let next = (self.active as isize + delta).rem_euclid(count) as usize;
        self.select_tab(next, window, cx);
    }

    pub(crate) fn begin_rename(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        self.renaming = Some((index, TextField::from_text(tab.name.clone())));
        if let Some((_, field)) = self.renaming.as_mut() {
            field.select_all();
        }
        window.focus(&self.rename_focus, cx);
        cx.notify();
    }

    fn rename_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let stroke = &event.keystroke;
        if stroke.key == "escape" {
            cx.stop_propagation();
            self.renaming = None;
            window.focus(&self.focus, cx);
            cx.notify();
            return;
        }
        if stroke.key == "enter" {
            cx.stop_propagation();
            let Some((index, field)) = self.renaming.take() else {
                return;
            };
            let name = field.text.trim().to_string();
            if !name.is_empty()
                && let Some(tab) = self.tabs.get_mut(index)
            {
                tab.name = name;
            }
            window.focus(&self.focus, cx);
            self.save_tabs(cx);
            cx.notify();
            return;
        }
        if let Some((_, field)) = self.renaming.as_mut()
            && field.key_with_clipboard(event, false, cx)
        {
            cx.stop_propagation();
            cx.notify();
        }
    }

    pub(crate) fn split(&mut self, way: Split, cx: &mut Context<Self>) {
        let tab = self.active;
        let Some(chosen) = self.tabs.get_mut(tab) else {
            return;
        };
        if chosen.split.is_some() {
            return;
        }
        chosen.panes.push(Pane::new(cx));
        chosen.split = Some(way);
        chosen.active = chosen.panes.len() - 1;
        self.spawn(tab, 1, cx);
        cx.notify();
    }

    pub(crate) fn close_pane(&mut self, cx: &mut Context<Self>) {
        let tab = self.active;
        let Some(chosen) = self.tabs.get_mut(tab) else {
            return;
        };
        if chosen.panes.len() < 2 {
            return;
        }
        // The pane the user looks at goes, and the other one takes its room.
        let active = chosen.active;
        if active == 0 {
            chosen.panes.remove(1);
        } else {
            chosen.panes.remove(active);
        }
        chosen.active = 0;
        chosen.split = None;
        cx.notify();
    }

    /// Writes the tabs of this project down, so that the next window opens them empty in
    /// the same folders.
    fn save_tabs(&mut self, cx: &mut App) {
        let Some(store) = self.store.clone() else {
            return;
        };
        let root = self.root.clone();
        let state = TabsState {
            tabs: self
                .tabs
                .iter()
                .map(|tab| TabState {
                    name: tab.name.clone(),
                    directory: tab.directory.clone(),
                    shell: tab.shell.clone(),
                })
                .collect(),
            active: self.active,
        };
        cx.background_executor()
            .spawn(async move {
                let _ = term_tabs::save(&store, &root, &state);
            })
            .detach();
    }

    /// The link under `position` of the active tab.
    pub(crate) fn link_at(&self, pane: usize, position: Position) -> Option<LinkText> {
        let text = self.row_at(pane, position.line)?;
        crate::term_links::link_at(&text, position.column)
    }

    /// The text of a line of the screen of a pane, one character per column.
    fn row_at(&self, pane: usize, line: i32) -> Option<String> {
        let chosen = self.tabs.get(self.active)?;
        let screen = chosen.panes.get(pane)?.session()?.screen();
        let row = line + screen.display_offset as i32;
        (row >= 0 && row < screen.rows as i32)
            .then(|| crate::term_grid::row_text(&screen, row as u16))
    }

    pub(crate) fn pane_focus(&self, _pane: usize) -> FocusHandle {
        self.focus.clone()
    }

    pub(crate) fn grid_pressed(
        &mut self,
        pane: usize,
        position: Position,
        clicks: usize,
        cx: &mut Context<Self>,
    ) {
        let Some(chosen) = self.tabs.get_mut(self.active) else {
            return;
        };
        let Some(found) = chosen.panes.get_mut(pane) else {
            return;
        };
        let Some(session) = found.session().cloned() else {
            return;
        };
        found.hover = None;
        (found.selection, found.dragging) = pressed(&session.screen(), position, clicks);
        chosen.active = pane;
        cx.notify();
    }

    pub(crate) fn grid_moved(
        &mut self,
        pane: usize,
        position: Option<Position>,
        control: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(chosen) = self.tabs.get_mut(self.active) else {
            return false;
        };
        let Some(found) = chosen.panes.get_mut(pane) else {
            return false;
        };
        if let Some(pressed) = found.dragging
            && let Some(position) = position
        {
            found.selection = Some(Selection::new(pressed, position));
            cx.notify();
            return false;
        }
        let hover = match (control, position) {
            (true, Some(position)) => {
                let line = found
                    .session()
                    .and_then(|session| {
                        let screen = session.screen();
                        let row = position.line + screen.display_offset as i32;
                        (row >= 0 && row < screen.rows as i32)
                            .then(|| crate::term_grid::row_text(&screen, row as u16))
                    })
                    .and_then(|text| crate::term_links::link_at(&text, position.column))
                    .map(|link| SearchMatch {
                        line: position.line,
                        start_col: link.start,
                        end_col: link.end,
                    });
                (line.is_some(), line)
            }
            _ => (false, None),
        };
        let (over, hover) = hover;
        if found.hover != hover {
            found.hover = hover;
            cx.notify();
        }
        over
    }
    pub(crate) fn grid_released(&mut self, pane: usize, cx: &mut Context<Self>) {
        let Some(found) = self
            .tabs
            .get_mut(self.active)
            .and_then(|tab| tab.panes.get_mut(pane))
        else {
            return;
        };
        found.dragging = None;
        cx.notify();
    }

    pub(crate) fn grid_context_menu(
        &mut self,
        pane: usize,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        if let Some(chosen) = self.tabs.get_mut(self.active) {
            chosen.active = pane;
        }
        let split = self
            .tabs
            .get(self.active)
            .is_some_and(|tab| tab.panes.len() > 1);
        let panel = cx.entity();
        let copy = panel.clone();
        let paste = panel.clone();
        let all = panel.clone();
        let clear = panel.clone();
        let find = panel.clone();
        let close = panel.clone();
        let has_selection = self
            .tabs
            .get(self.active)
            .and_then(|tab| tab.panes.get(pane))
            .is_some_and(|pane| pane.selection.is_some());
        let mut entries = vec![
            MenuEntry::new("Copy", move |_, cx| {
                copy.update(cx, |panel, cx| {
                    panel.copy_selection(cx);
                });
            }),
            MenuEntry::new("Paste", move |window, cx| {
                paste.update(cx, |panel, cx| panel.paste(window, cx));
            }),
            MenuEntry::new("Select All", move |_, cx| {
                all.update(cx, |panel, cx| panel.select_all(cx));
            }),
            MenuEntry::new("Clear Buffer", move |_, cx| {
                clear.update(cx, |panel, cx| panel.clear_pane(cx));
            }),
            MenuEntry::separator(),
            MenuEntry::new("Find...", move |window, cx| {
                find.update(cx, |panel, cx| panel.open_search(window, cx));
            }),
        ];
        if split {
            entries.push(MenuEntry::new("Close Split", move |_, cx| {
                close.update(cx, |panel, cx| panel.close_pane(cx));
            }));
        }
        let entries = entries
            .into_iter()
            .enumerate()
            .map(|(index, entry)| {
                if index == 0 && !has_selection {
                    MenuEntry::disabled(entry.label.clone())
                } else {
                    entry
                }
            })
            .collect();
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.show_menu(position, entries, cx)
            })
            .ok();
    }

    fn tab_menu(&mut self, index: usize, position: Point<Pixels>, cx: &mut Context<Self>) {
        // The rows of the menu act on the tab of the click, not on the one that was active.
        if index < self.tabs.len() {
            self.active = index;
            cx.notify();
        }
        let panel = cx.entity();
        let rename = panel.clone();
        let right = panel.clone();
        let down = panel.clone();
        let close = panel.clone();
        let entries = vec![
            MenuEntry::new("Rename Session", move |window, cx| {
                rename.update(cx, |panel, cx| panel.begin_rename(index, window, cx));
            }),
            MenuEntry::new("Split Right", move |_, cx| {
                right.update(cx, |panel, cx| panel.split(Split::Right, cx));
            }),
            MenuEntry::new("Split Down", move |_, cx| {
                down.update(cx, |panel, cx| panel.split(Split::Down, cx));
            }),
            MenuEntry::separator(),
            MenuEntry::new("Close Session", move |window, cx| {
                close.update(cx, |panel, cx| panel.close_tab(index, window, cx));
            }),
        ];
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.show_menu(position, entries, cx)
            })
            .ok();
    }

    fn shells_menu(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let panel = cx.entity();
        let mut entries = Vec::new();
        for shell in terminal::available_shells() {
            let chosen = panel.clone();
            let command = shell.command_line.clone();
            entries.push(MenuEntry::new(shell.name.clone(), move |window, cx| {
                chosen.update(cx, |panel, cx| {
                    panel.new_tab(Some(command.clone()), window, cx)
                });
            }));
        }
        if entries.is_empty() {
            entries.push(MenuEntry::disabled("No shell found"));
        }
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.show_menu(position, entries, cx)
            })
            .ok();
    }

    pub(crate) fn open_link(
        &mut self,
        pane: usize,
        position: Position,
        link: LinkText,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text = self
            .row_at(pane, position.line)
            .map(|line| link.text(&line).to_string())
            .unwrap_or_default();
        match link.kind {
            LinkKind::Url => {
                ::platform::open_url(&text);
            }
            LinkKind::File { path, line, column } => {
                self.open_file(path, line, column, window, cx);
            }
            LinkKind::Frame { file, line } => {
                self.open_frame(file, line, text, window, cx);
            }
        }
    }

    fn open_file(
        &mut self,
        path: String,
        line: Option<u32>,
        column: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let cwd = self
            .tabs
            .get(self.active)
            .map(|tab| {
                tab.pane()
                    .cwd
                    .clone()
                    .unwrap_or_else(|| tab.directory.clone())
            })
            .unwrap_or_else(|| self.root.clone());
        let Some(path) = crate::term_links::resolve(&path, Some(&cwd), &self.root) else {
            self.workspace
                .update(cx, |workspace, cx| {
                    workspace.notify_user("Terminal", "No such file", cx)
                })
                .ok();
            return;
        };
        let spot = line.map(|line| editor_view::Spot::Line {
            line: line.saturating_sub(1) as usize,
            column: column.unwrap_or(1).saturating_sub(1) as usize,
        });
        let workspace = self.workspace.clone();
        let handle = window.window_handle();
        cx.defer(move |cx| {
            handle
                .update(cx, |_, window, cx| {
                    workspace
                        .update(cx, |workspace, cx| {
                            navigation::go_to(workspace, path, spot, true, window, cx)
                        })
                        .ok();
                })
                .ok();
        });
    }

    /// A frame of a stack trace: the language knows where its source file is, and
    /// answers with the place of the class.
    fn open_frame(
        &mut self,
        file: String,
        line: u32,
        text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let support = cx
            .try_global::<editor_view::Semantic>()
            .map(|semantic| semantic.0.clone());
        let Some(provider) = support.as_ref().and_then(|support| support.stack_traces()) else {
            self.open_file(file, Some(line), None, window, cx);
            return;
        };
        let extension = Path::new(&file)
            .extension()
            .map(|extension| extension.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut replies = lang_api::ask(provider, lang_api::TraceQuery(format!("at {text}")));
        let workspace = self.workspace.clone();
        let missing = self.workspace.clone();
        let handle = window.window_handle();
        cx.spawn(async move |_, cx| {
            while let Some(answer) = replies.next().await {
                let lang_api::Report::Trace { lines } = answer.value else {
                    continue;
                };
                let place = lines
                    .iter()
                    .filter_map(|line| line.frame.as_ref())
                    .find_map(|frame| frame.place.clone());
                let Some(place) = place else {
                    continue;
                };
                let spot = editor_view::Spot::Line {
                    line: place.span.start.line as usize,
                    column: place.span.start.character as usize,
                };
                cx.update(|cx| {
                    editor_view::show_location(
                        &place,
                        &extension,
                        cx,
                        move |path, cx| {
                            let workspace = workspace.clone();
                            cx.defer(move |cx| {
                                handle
                                    .update(cx, |_, window, cx| {
                                        workspace
                                            .update(cx, |workspace, cx| {
                                                navigation::go_to(
                                                    workspace,
                                                    path,
                                                    Some(spot),
                                                    true,
                                                    window,
                                                    cx,
                                                )
                                            })
                                            .ok();
                                    })
                                    .ok();
                            });
                        },
                        move |cx| {
                            missing
                                .update(cx, |workspace, cx| {
                                    workspace.notify_user(
                                        "Terminal",
                                        "No source for this frame",
                                        cx,
                                    )
                                })
                                .ok();
                        },
                    );
                });
                break;
            }
        })
        .detach();
    }

    /// The split the pointer drags, from the place it started from.
    fn drag_divider(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        let Some(drag) = &self.dragging_split else {
            return;
        };
        let Some(tab) = self.tabs.get(self.active) else {
            return;
        };
        if tab.split.is_none() {
            return;
        }
        let room: f32 = tab
            .panes
            .iter()
            .filter_map(|pane| {
                let metrics = pane.metrics.get()?;
                Some(match tab.split {
                    Some(Split::Right) => f32::from(metrics.bounds.size.width),
                    _ => f32::from(metrics.bounds.size.height),
                })
            })
            .sum();
        if room <= 0. {
            return;
        }
        let moved = match tab.split {
            Some(Split::Right) => f32::from(event.position.x - drag.start.x),
            _ => f32::from(event.position.y - drag.start.y),
        };
        let ratio = (drag.ratio + moved / room).clamp(SPLIT_LIMIT, 1. - SPLIT_LIMIT);
        let drag = self.dragging_split.take();
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.ratio = ratio;
        }
        self.dragging_split = drag;
        cx.notify();
    }
}

/// The window takes its terminal with it: the program of the shell stops even when a
/// frame that still holds the session outlives the panel.
impl Drop for TerminalPanel {
    fn drop(&mut self) {
        for tab in &self.tabs {
            for pane in &tab.panes {
                if let State::Running(session) = &pane.state {
                    session.terminate();
                }
            }
        }
    }
}

impl TerminalPanel {
    fn tabs_row(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let mut row = div()
            .id("terminal-tabs")
            .flex()
            .items_center()
            .flex_none()
            .overflow_hidden()
            .h(px(CONTROL_HEIGHT))
            .px(px(SPACE_S))
            .gap(px(SPACE_XS));
        for index in 0..self.tabs.len() {
            let selected = index == self.active;
            let name = self.tabs[index].name.clone();
            let editing = self.renaming.as_ref().is_some_and(|(at, _)| *at == index);
            let mut tab = div()
                .id(("terminal-tab", index))
                .debug_selector(move || format!("terminal-tab-{index}"))
                .flex()
                .items_center()
                .gap(px(SPACE_S))
                .flex_none()
                .h(px(CONTROL_HEIGHT - SPACE_S))
                .px(px(SPACE_M))
                .rounded(px(CONTROL_RADIUS))
                .cursor_pointer()
                .text_size(px(UI_SIZE))
                .text_color(if selected {
                    theme.text
                } else {
                    theme.text_secondary
                })
                .when(selected, |this| {
                    this.bg(theme.tab_selected)
                        .border_1()
                        .border_color(theme.tab_selected_border)
                })
                .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                    if event.click_count() >= 2 {
                        this.begin_rename(index, window, cx);
                    } else {
                        this.select_tab(index, window, cx);
                    }
                }))
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                        this.tab_menu(index, event.position, cx)
                    }),
                );
            if editing {
                tab =
                    tab.child(
                        div()
                            .track_focus(&self.rename_focus)
                            .on_key_down(cx.listener(|this, event, window, cx| {
                                this.rename_key(event, window, cx)
                            }))
                            .child(
                                div()
                                    .debug_selector(|| "terminal-rename".to_owned())
                                    .flex()
                                    .items_center()
                                    .min_w(px(90.))
                                    .children(text_field_children_named(
                                        "terminal-rename-field",
                                        self.renaming.as_ref().map(|(_, field)| field).unwrap(),
                                        theme.text,
                                        theme.text,
                                        theme.selection,
                                        theme.caret,
                                        16.,
                                        true,
                                    )),
                            ),
                    );
            } else {
                tab = tab.child(div().max_w(px(220.)).truncate().child(name));
            }
            if self.tabs.len() > 1 {
                tab = tab.child(
                    icon_button(
                        format!("terminal-tab-close-{index}"),
                        "general/closeSmall",
                        "Close Session",
                        Some("Ctrl+F4".into()),
                        theme,
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.close_tab(index, window, cx);
                    })),
                );
            }
            row = row.child(tab);
        }
        let new_tab = icon_button(
            "terminal-new-tab",
            "general/add",
            "New Session",
            Some("Ctrl+Shift+T".into()),
            theme,
        )
        .on_click(cx.listener(|this, _, window, cx| this.new_tab(None, window, cx)));
        let shells = icon_button(
            "terminal-shells",
            "general/chevronDown",
            "New Session in Another Shell",
            None,
            theme,
        )
        .on_click(
            cx.listener(|this, event: &ClickEvent, _, cx| this.shells_menu(event.position(), cx)),
        );
        if let Some((text, error)) = crate::claude_ide::status(cx) {
            let color = if error {
                theme.close_hover
            } else {
                theme.success
            };
            row = row.child(div().flex_1()).child(
                div()
                    .debug_selector(|| "terminal-ide-status".to_owned())
                    .flex_none()
                    .px(px(SPACE_S))
                    .text_size(px(UI_SIZE))
                    .text_color(color)
                    .child(text),
            );
        }
        row.child(new_tab).child(shells).into_any_element()
    }

    fn search_row(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let tab = self.tabs.get(self.active)?;
        let pane = tab.pane();
        if !pane.search.open {
            return None;
        }
        let search = &pane.search;
        let status = if let Some(error) = &search.results.error {
            error.clone()
        } else if search.matches().is_empty() {
            if search.field.text.is_empty() {
                String::new()
            } else {
                "No matches".to_string()
            }
        } else {
            let current = search.current.unwrap_or(0) + 1;
            format!("{current}/{}", search.matches().len())
        };
        let mut toggles = Vec::new();
        for (id, icon, label, shortcut, on, regex, case) in [
            (
                "terminal-search-case",
                "general/caseSensitive",
                "Match Case",
                "Alt+C",
                search.case_sensitive,
                false,
                true,
            ),
            (
                "terminal-search-regex",
                "general/regex",
                "Regex",
                "Alt+X",
                search.regex,
                true,
                false,
            ),
        ] {
            let mut button = icon_button(id, icon, label, Some(shortcut.into()), theme);
            if on {
                button = button
                    .bg(theme.tab_selected)
                    .border_1()
                    .border_color(theme.tab_selected_border);
            }
            toggles.push(button.on_click(cx.listener(move |this, _, window, cx| {
                if let Some(pane) = this.tabs.get_mut(this.active).map(|tab| tab.pane_mut()) {
                    if regex {
                        pane.search.regex = !pane.search.regex;
                    }
                    if case {
                        pane.search.case_sensitive = !pane.search.case_sensitive;
                    }
                }
                window.focus(&this.search_focus, cx);
                this.run_search(cx);
            })));
        }
        let close = icon_button(
            "terminal-search-close",
            "general/closeSmall",
            "Close Search",
            Some("Escape".into()),
            theme,
        )
        .on_click(cx.listener(|this, _, window, cx| this.close_search(window, cx)));
        let mut row = div()
            .id("terminal-search")
            .debug_selector(|| "terminal-search".to_owned())
            .flex()
            .items_center()
            .flex_none()
            .h(px(CONTROL_HEIGHT))
            .mx(px(SPACE_M))
            .mb(px(SPACE_S))
            .gap(px(SPACE_S));
        row = row.child(
            div()
                .track_focus(&self.search_focus)
                .on_key_down(
                    cx.listener(|this, event, window, cx| this.search_key(event, window, cx)),
                )
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
                .children(text_field_children_named(
                    "terminal-search-field",
                    &search.field,
                    theme.text,
                    theme.text,
                    theme.selection,
                    theme.caret,
                    16.,
                    true,
                ))
                .when(search.field.text.is_empty(), |this| {
                    this.child(div().text_color(theme.text_secondary).child("Find"))
                }),
        );
        for toggle in toggles {
            row = row.child(toggle);
        }
        Some(row.child(status).child(close).into_any_element())
    }
    fn pane_body(
        &self,
        tab: usize,
        pane: usize,
        focused: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(chosen) = self.tabs.get(tab) else {
            return div().into_any_element();
        };
        let Some(found) = chosen.panes.get(pane) else {
            return div().into_any_element();
        };
        match &found.state {
            State::Running(session) => {
                let current = found
                    .search
                    .current
                    .and_then(|current| found.search.matches().get(current).copied());
                let scrolled = session.clone();
                let view = GridView {
                    session: session.clone(),
                    focused,
                    theme: *theme,
                    selection: found.selection,
                    matches: found.search.matches().to_vec(),
                    current,
                    hover: found.hover,
                    host: Some(cx.entity()),
                    pane,
                    metrics: found.metrics.clone(),
                };
                let mut body = div()
                    .id(("terminal-body", pane))
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .on_scroll(move |event, _, _| scrolled.scroll(scroll_lines(event)))
                    .child(grid(view));
                if let Some(code) = session.exit_code().filter(|_| session.is_closed()) {
                    body = body.child(
                        div()
                            .debug_selector(|| "terminal-exit".to_owned())
                            .flex_none()
                            .px(px(SPACE_M))
                            .pb(px(SPACE_M))
                            .text_size(px(UI_SIZE))
                            .text_color(theme.text_secondary)
                            .child(format!(
                                "Process finished with exit code {code}. Press Enter to start again"
                            )),
                    );
                }
                body.into_any_element()
            }
            State::Starting => message("Starting the shell", theme).into_any_element(),
            State::Failed(text) => message(text.clone(), theme).into_any_element(),
        }
    }

    /// The entity of the panel itself, for the grid that reports the pointer.
    fn pane_element(
        &self,
        tab: usize,
        pane: usize,
        focused: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Div {
        let active = self.active == tab && self.tabs[tab].active() == pane;
        let mut element = div()
            .debug_selector(move || format!("terminal-pane-{pane}"))
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .min_w_0();
        if active {
            element = element.border_1().border_color(theme.border);
        }
        element.child(self.pane_body(tab, pane, focused && active, theme, cx))
    }

    fn divider(&self, way: Split, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let size = match way {
            Split::Right => div().w(px(SPACE_S)).h_full().cursor_col_resize(),
            Split::Down => div().h(px(SPACE_S)).w_full().cursor_row_resize(),
        };
        size.flex_none().bg(theme.border).on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                let ratio = this
                    .tabs
                    .get(this.active)
                    .map(|tab| tab.ratio)
                    .unwrap_or(SPLIT_RATIO);
                this.dragging_split = Some(SplitDrag {
                    start: event.position,
                    ratio,
                });
                cx.notify();
            }),
        )
    }

    fn panes(&mut self, focused: bool, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let Some(chosen) = self.tabs.get(self.active) else {
            return div();
        };
        let suggestion = self.terminal_suggestion;
        let body = div()
            .debug_selector(|| "terminal-panes".to_owned())
            .flex()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .on_mouse_move(
                cx.listener(|this, event: &MouseMoveEvent, _, cx| this.drag_divider(event, cx)),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.dragging_split = None;
                    cx.notify();
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.dragging_split = None;
                    cx.notify();
                }),
            );
        let grid = match chosen.split {
            None => body
                .flex_col()
                .child(self.pane_element(self.active, 0, focused, theme, cx)),
            Some(way) => {
                let first = self.pane_element(self.active, 0, focused, theme, cx);
                let second = self.pane_element(self.active, 1, focused, theme, cx);
                let ratio = chosen.ratio.clamp(SPLIT_LIMIT, 1. - SPLIT_LIMIT);
                let body = match way {
                    Split::Right => body.flex_row(),
                    Split::Down => body.flex_col(),
                };
                body.child(first.flex_grow(ratio))
                    .child(self.divider(way, theme, cx))
                    .child(second.flex_grow(1. - ratio))
            }
        };
        match suggestion {
            Some(client) => div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .min_w_0()
                .child(self.suggestion_bar(client, theme, cx))
                .child(grid),
            None => grid,
        }
    }

    /// The bar above the grid that offers the entry of Arro to a client which just started
    /// in the shell.
    fn suggestion_bar(
        &mut self,
        client: mcp_discovery::Client,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let configure = dialog_button(
            "terminal-mcp-suggestion-configure",
            "terminal-mcp-suggestion-configure".to_owned(),
            "Auto-Configure",
            true,
            true,
            theme,
        )
        .on_click(cx.listener(move |this, _, _, cx| {
            this.configure_suggestion(client, cx);
        }));
        let close = icon_button(
            "terminal-mcp-suggestion-close",
            "general/close",
            "Hide the suggestion",
            None,
            theme,
        )
        .debug_selector(|| "terminal-mcp-suggestion-close".to_owned())
        .on_click(cx.listener(|this, _, _, cx| {
            this.terminal_suggestion = None;
            cx.notify();
        }));
        div()
            .id("terminal-mcp-suggestion")
            .debug_selector(|| "terminal-mcp-suggestion".to_owned())
            .flex()
            .items_center()
            .gap(px(SPACE_M))
            .px(px(SPACE_M))
            .h(px(32.))
            .flex_none()
            .bg(theme.island_bg)
            .border_b_1()
            .border_color(theme.border)
            .child(div().flex_1().text_size(px(TEXT_SMALL)).child(format!(
                "Configure Arro for {} to give it the tools of the project",
                client.title()
            )))
            .child(configure)
            .child(close)
    }

    fn configure_suggestion(&mut self, client: mcp_discovery::Client, cx: &mut Context<Self>) {
        let home = crate::settings::mcp_home_folder(cx);
        let project = self.root.clone();
        let settings = cx.try_global::<Settings>().cloned().unwrap_or_default();
        let command = crate::settings_catalog::mcp_binary(&settings);
        self.terminal_suggestion = None;
        cx.notify();
        let write = cx.background_executor().spawn(async move {
            mcp_discovery::configure(
                client,
                mcp_discovery::Scope::User,
                mcp_discovery::Transport::Stdio,
                &mcp_discovery::Server {
                    command,
                    url: None,
                    token: None,
                },
                &home,
                &project,
            )
        });
        cx.spawn(async move |this, cx| {
            let written = write.await;
            this.update(cx, |this, cx| {
                if let Err(error) = written {
                    this.terminal_suggestion = None;
                    let _ = error;
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

impl Render for TerminalPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let hide = |_: &ClickEvent, window: &mut Window, cx: &mut App| {
            window.dispatch_action(Box::new(ToggleTerminal), cx);
        };
        let focused = self.focus.is_focused(window);
        let body = div()
            .id("terminal")
            .key_context("Terminal ToolWindow")
            .track_focus(&self.focus)
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .on_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)))
            .on_action(cx.listener(|this, _: &NewTab, window, cx| this.new_tab(None, window, cx)))
            .on_action(
                cx.listener(|this, _: &CloseTab, window, cx| {
                    this.close_tab(this.active, window, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &PreviousTab, window, cx| this.step_tab(-1, window, cx)),
            )
            .on_action(cx.listener(|this, _: &NextTab, window, cx| this.step_tab(1, window, cx)))
            .on_action(cx.listener(|this, _: &Find, window, cx| this.open_search(window, cx)))
            .on_action(cx.listener(|this, _: &CopySelection, _, cx| {
                this.copy_selection(cx);
            }))
            .on_action(cx.listener(|this, _: &PasteText, window, cx| this.paste(window, cx)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| window.focus(&this.focus, cx)),
            )
            .child(self.tabs_row(&theme, cx))
            .children(self.search_row(&theme, cx))
            .child(self.panes(focused, &theme, cx));
        let measure = {
            let bounds = self.bounds.clone();
            canvas(
                move |area, _, _| *bounds.borrow_mut() = Some(area),
                |_, _, _, _| {},
            )
            .absolute()
            .top_0()
            .left_0()
            .size_full()
        };
        island(&theme)
            .group(GROUP)
            .relative()
            .h_full()
            .child(measure)
            .child(panel_header("Terminal", GROUP, hide, &theme))
            .child(body)
    }
}

impl GridPanel for TerminalPanel {
    fn pane_focus(&self, pane: usize) -> FocusHandle {
        TerminalPanel::pane_focus(self, pane)
    }

    fn grid_pressed(
        &mut self,
        pane: usize,
        position: Position,
        clicks: usize,
        cx: &mut Context<Self>,
    ) {
        TerminalPanel::grid_pressed(self, pane, position, clicks, cx)
    }

    fn grid_moved(
        &mut self,
        pane: usize,
        position: Option<Position>,
        control: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        TerminalPanel::grid_moved(self, pane, position, control, cx)
    }

    fn grid_released(&mut self, pane: usize, cx: &mut Context<Self>) {
        TerminalPanel::grid_released(self, pane, cx)
    }

    fn grid_context_menu(&mut self, pane: usize, position: Point<Pixels>, cx: &mut Context<Self>) {
        TerminalPanel::grid_context_menu(self, pane, position, cx)
    }

    fn link_at(&self, pane: usize, position: Position) -> Option<LinkText> {
        TerminalPanel::link_at(self, pane, position)
    }

    fn open_link(
        &mut self,
        pane: usize,
        position: Position,
        link: LinkText,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        TerminalPanel::open_link(self, pane, position, link, window, cx)
    }
}

pub(crate) fn message(text: impl Into<SharedString>, theme: &Theme) -> Div {
    div()
        .flex()
        .flex_1()
        .items_center()
        .justify_center()
        .text_size(px(UI_SIZE))
        .text_color(theme.text_secondary)
        .child(text.into())
}

#[cfg(test)]
impl TerminalPanel {
    /// Takes the keys of `platform` for this panel alone: a test of the macOS keys does
    /// not choose the platform of the whole process.
    pub(crate) fn use_keys_of(&mut self, platform: Platform) {
        self.keys_platform = platform;
    }

    pub(crate) fn tabs_len(&self) -> usize {
        self.tabs.len()
    }

    pub(crate) fn tab_names(&self) -> Vec<String> {
        self.tabs.iter().map(|tab| tab.name.clone()).collect()
    }

    pub(crate) fn tab_directories(&self) -> Vec<PathBuf> {
        self.tabs.iter().map(|tab| tab.directory.clone()).collect()
    }

    pub(crate) fn active_tab(&self) -> usize {
        self.active
    }

    pub(crate) fn split_way(&self) -> Option<Split> {
        self.tabs.get(self.active).and_then(|tab| tab.split)
    }

    pub(crate) fn panes_len(&self) -> usize {
        self.tabs.get(self.active).map_or(0, |tab| tab.panes.len())
    }

    pub(crate) fn pane_session(&self, tab: usize, pane: usize) -> Option<Arc<TerminalSession>> {
        self.tabs.get(tab)?.panes.get(pane)?.session().cloned()
    }

    pub(crate) fn selection_text(&self) -> Option<String> {
        let pane = self.tabs.get(self.active)?.pane();
        let selection = pane.selection?;
        let screen = pane.session()?.screen();
        Some(crate::term_grid::selection_text(&screen, selection))
    }

    pub(crate) fn search_matches(&self) -> Vec<SearchMatch> {
        self.tabs
            .get(self.active)
            .map(|tab| tab.pane().search.matches().to_vec())
            .unwrap_or_default()
    }

    pub(crate) fn search_bar_open(&self) -> bool {
        self.search_open()
    }

    pub(crate) fn current_match(&self) -> Option<usize> {
        self.tabs.get(self.active)?.pane().search.current
    }

    pub(crate) fn saved_state(&self) -> TabsState {
        TabsState {
            tabs: self
                .tabs
                .iter()
                .map(|tab| TabState {
                    name: tab.name.clone(),
                    directory: tab.directory.clone(),
                    shell: tab.shell.clone(),
                })
                .collect(),
            active: self.active,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{TerminalCommand, terminal_command};
    use keymap::Platform;
    use ui_kit::gpui::Keystroke;

    fn stroke(text: &'static str) -> Keystroke {
        Keystroke::parse(text).expect("the test keystroke parses")
    }

    /// macOS takes the window keys with Command and leaves Control to the shell: Ctrl+C
    /// interrupts the program there, so the terminal does not copy it.
    #[test]
    fn the_macos_terminal_takes_command_and_leaves_control_to_the_shell() {
        assert_eq!(
            terminal_command(&stroke("cmd-c"), Platform::MacOs),
            Some(TerminalCommand::Copy)
        );
        assert_eq!(
            terminal_command(&stroke("ctrl-c"), Platform::MacOs),
            None,
            "Ctrl+C goes to the shell"
        );
        assert_eq!(
            terminal_command(&stroke("cmd-v"), Platform::MacOs),
            Some(TerminalCommand::Paste)
        );
        assert_eq!(
            terminal_command(&stroke("ctrl-v"), Platform::MacOs),
            None,
            "Ctrl+V goes to the shell"
        );
        assert_eq!(
            terminal_command(&stroke("cmd-t"), Platform::MacOs),
            Some(TerminalCommand::NewTab)
        );
        assert_eq!(
            terminal_command(&stroke("cmd-shift-t"), Platform::MacOs),
            Some(TerminalCommand::NewTab)
        );
        assert_eq!(
            terminal_command(&stroke("cmd-w"), Platform::MacOs),
            Some(TerminalCommand::CloseTab)
        );
        assert_eq!(
            terminal_command(&stroke("cmd-f"), Platform::MacOs),
            Some(TerminalCommand::Find)
        );
    }

    /// Windows keeps the Control keys of the terminal, and Ctrl+C without a selection goes
    /// to the shell when nothing is selected.
    #[test]
    fn the_windows_terminal_keeps_its_control_keys() {
        assert_eq!(
            terminal_command(&stroke("ctrl-shift-c"), Platform::Windows),
            Some(TerminalCommand::Copy)
        );
        assert_eq!(
            terminal_command(&stroke("ctrl-c"), Platform::Windows),
            Some(TerminalCommand::CopyIfSelected)
        );
        assert_eq!(
            terminal_command(&stroke("ctrl-shift-v"), Platform::Windows),
            Some(TerminalCommand::Paste)
        );
        assert_eq!(
            terminal_command(&stroke("ctrl-v"), Platform::Windows),
            Some(TerminalCommand::Paste)
        );
        assert_eq!(
            terminal_command(&stroke("ctrl-shift-t"), Platform::Windows),
            Some(TerminalCommand::NewTab)
        );
        assert_eq!(
            terminal_command(&stroke("ctrl-f4"), Platform::Windows),
            Some(TerminalCommand::CloseTab)
        );
        assert_eq!(
            terminal_command(&stroke("ctrl-f"), Platform::Windows),
            Some(TerminalCommand::Find)
        );
        assert_eq!(
            terminal_command(&stroke("cmd-c"), Platform::Windows),
            None,
            "the Super key is not ours"
        );
    }
}
