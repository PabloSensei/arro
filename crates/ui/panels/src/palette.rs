//! The popup of Search Everywhere and of what shares its look: Go to File, Go to Class,
//! Go to Symbol, Find Action, Recent Files, Switcher and the file structure. One view
//! with a mode; rows come from the project index, from the symbol sources and from the
//! commands of the window.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use editor_view::{EditorView, OutlineItem, OutlineKind, Spot};
use futures_util::StreamExt;
use lang_api::{SymbolKind, ask};
use search::{
    FuzzyMatcher, GotoQuery, NameMatcher, SymbolRequest, SymbolRow, SymbolScope, Tab, Target,
    merge_rows, rank, rank_files,
};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::{
    AI_TOOL, COMMIT_TOOL, DEBUG_TOOL, FileIcons, LOG_TOOL, PROBLEMS_TOOL, PROJECT_TOOL, RUN_TOOL,
    SERVICES_TOOL, STRUCTURE_TOOL, Settings, TERMINAL_TOOL, TODO_TOOL, USAGES_TOOL, Workspace,
};

use crate::index::ProjectIndex;
use crate::navigation;
use crate::settings::SettingsView;
use crate::settings_catalog::{self, SettingKind};

const CONTEXT: &str = "SearchEverywhere";
const WIDTH: f32 = 702.;
const HEIGHT: f32 = 600.;
const GROUP_ROWS: usize = 5;
const MAX_ROWS: usize = 200;

actions!(
    search_everywhere,
    [
        Previous,
        Next,
        Open,
        OpenDebug,
        NextTab,
        PreviousTab,
        NextGroup,
        PreviousGroup,
        ToggleEdited
    ]
);

/// One key of the Search Everywhere popup and the IDEA action it implements, if any.
#[cfg(test)]
pub(crate) struct PaletteKey {
    pub(crate) keys: &'static str,
    pub(crate) action: &'static str,
    pub(crate) idea: Option<&'static str>,
}

macro_rules! palette_keys {
    ($(($keys:literal, $action:ident, $idea:expr)),* $(,)?) => {
        pub fn bind_keys(cx: &mut App) {
            cx.bind_keys([$(KeyBinding::new($keys, $action, Some(CONTEXT))),*]);
        }

        #[cfg(test)]
        pub(crate) const KEYS: &[PaletteKey] = &[
            $(PaletteKey {
                keys: $keys,
                action: concat!("search_everywhere::", stringify!($action)),
                idea: $idea,
            }),*
        ];
    };
}

palette_keys![
    ("up", Previous, None),
    ("down", Next, None),
    ("enter", Open, Some("SearchEverywhere.SelectItem")),
    ("shift-enter", OpenDebug, None),
    ("tab", NextTab, Some("SearchEverywhere.NextTab")),
    ("shift-tab", PreviousTab, Some("SearchEverywhere.PrevTab")),
    (
        "pagedown",
        NextGroup,
        Some("SearchEverywhere.NavigateToNextGroup")
    ),
    (
        "ctrl-down",
        NextGroup,
        Some("SearchEverywhere.NavigateToNextGroup")
    ),
    (
        "pageup",
        PreviousGroup,
        Some("SearchEverywhere.NavigateToPrevGroup")
    ),
    (
        "ctrl-up",
        PreviousGroup,
        Some("SearchEverywhere.NavigateToPrevGroup")
    ),
    // While Ctrl is held the Switcher keeps its list under the same keys.
    ("ctrl-tab", NextTab, None),
    ("ctrl-shift-tab", PreviousTab, None),
    (
        "ctrl-e",
        ToggleEdited,
        Some("SwitcherRecentEditedChangedToggleCheckBox")
    ),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Everywhere(Tab),
    /// Ctrl+E lists the files recently active, Ctrl+Tab does the same and starts on the
    /// previous one.
    Recent,
    /// Ctrl+Tab while Ctrl is held: the recent files and the tool windows. Releasing
    /// Ctrl opens what is selected.
    Switcher,
    /// The declarations of the file that is open.
    Structure,
    /// Double Ctrl lists the run configurations of the project.
    RunAnything,
    /// Alt+Shift+F10 and Alt+Shift+F9: pick the configuration to run or to debug.
    ChooseRun {
        debug: bool,
    },
}

/// A command as Find Action lists it.
#[derive(Clone)]
struct ActionEntry {
    id: &'static str,
    title: String,
    shortcut: Option<String>,
}

#[derive(Clone)]
enum Item {
    File {
        path: PathBuf,
        spot: Option<Spot>,
    },
    Symbol(SymbolRow),
    Declaration {
        offset: usize,
    },
    Action(&'static str),
    /// A row of the settings list; choosing it shows that page of Settings.
    Setting(&'static str),
    /// A run configuration of the project, by name.
    RunConfig(String),
    /// A tool window of the Switcher, by id.
    Tool(&'static str),
}

/// A row on screen: a group header when `item` is empty.
#[derive(Clone)]
struct RowData {
    title: String,
    detail: String,
    icon: &'static str,
    shortcut: Option<String>,
    indent: u32,
    /// The switch of a boolean setting, when this row is one.
    toggle: Option<bool>,
    item: Option<Item>,
}

impl RowData {
    fn header(title: &str) -> Self {
        Self {
            title: title.to_owned(),
            detail: String::new(),
            icon: "",
            shortcut: None,
            indent: 0,
            toggle: None,
            item: None,
        }
    }
}

const TABS: [(Tab, &str); 5] = [
    (Tab::All, "All"),
    (Tab::Classes, "Classes"),
    (Tab::Files, "Files"),
    (Tab::Symbols, "Symbols"),
    (Tab::Actions, "Actions"),
];

/// The tool windows the Switcher lists, in the order they appear there.
const TOOLS: [(&str, &str, &str); 12] = [
    (PROJECT_TOOL, "Project", "toolwindows/project"),
    (COMMIT_TOOL, "Commit", "toolwindows/commit"),
    (LOG_TOOL, "Git Log", "general/vcs"),
    (STRUCTURE_TOOL, "Structure", "toolwindows/structure"),
    (RUN_TOOL, "Run", "toolwindows/run"),
    (DEBUG_TOOL, "Debug", "toolwindows/debug"),
    (TERMINAL_TOOL, "Terminal", "toolwindows/terminal"),
    (SERVICES_TOOL, "Services", "toolwindows/services"),
    (TODO_TOOL, "TODO", "status/info"),
    (PROBLEMS_TOOL, "Problems", "toolwindows/problems"),
    (USAGES_TOOL, "Usages", "codeInsight/search"),
    (AI_TOOL, "AI Sessions", "own/aiSessions"),
];

/// The commands of Search Everywhere that begin with `/`.
const COMMANDS: [(&str, &str); 8] = [
    ("file", "workspace.goto_file"),
    ("class", "workspace.goto_class"),
    ("symbol", "workspace.goto_symbol"),
    ("search", "workspace.search_everywhere"),
    ("action", "workspace.find_action"),
    ("run", "workspace.run_anything"),
    ("recent", "workspace.recent_files"),
    ("switcher", "workspace.switcher"),
];

fn kind_icon(kind: SymbolKind) -> &'static str {
    match kind {
        SymbolKind::Module => "nodes/package",
        SymbolKind::Class => "nodes/class",
        SymbolKind::Interface => "nodes/interface",
        SymbolKind::Enum => "nodes/enum",
        SymbolKind::Function | SymbolKind::Method | SymbolKind::Constructor => "nodes/method",
        SymbolKind::Field => "nodes/field",
        SymbolKind::Constant => "nodes/constant",
    }
}

pub struct Palette {
    project: ProjectIndex,
    files: Entity<Arc<Vec<PathBuf>>>,
    workspace: WeakEntity<Workspace>,
    icons: FileIcons,
    focus: FocusHandle,
    scroll: ScrollHandle,
    mode: Mode,
    query: TextField,
    selected: usize,
    rows: Vec<RowData>,
    actions: Vec<ActionEntry>,
    recent: Vec<PathBuf>,
    /// The files of the open tabs, which is what the Switcher lists.
    open_tabs: Vec<PathBuf>,
    /// The Recent Files checkbox: only the open files with unsaved text.
    edited_only: bool,
    structure: Option<(PathBuf, Arc<[OutlineItem]>)>,
    /// Answers of the symbol sources for the query on screen, by scope and source.
    symbol_batches: HashMap<(usize, usize), Vec<SymbolRow>>,
    serial: usize,
    tasks: Vec<Task<()>>,
}

impl Palette {
    pub fn show(
        mode: Mode,
        project: ProjectIndex,
        files: Entity<Arc<Vec<PathBuf>>>,
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        let actions = workspace
            .commands()
            .all()
            .map(|command| ActionEntry {
                id: command.id,
                title: command.title.clone(),
                shortcut: workspace.shortcut_of(command.id),
            })
            .collect();
        let recent = navigation::recent(cx);
        let open_tabs: Vec<PathBuf> = workspace.open_tabs().into_iter().flatten().collect();
        // The open editor knows the signatures the language sent; the project index only
        // has names, so it is the fallback for a tab that is not an editor.
        let structure = workspace
            .active_editor()
            .and_then(|(path, view)| {
                let editor = view.downcast::<EditorView>().ok()?;
                let items = editor.read(cx).outline().to_vec();
                Some((path, Arc::<[OutlineItem]>::from(items)))
            })
            .or_else(|| {
                let path = workspace.active_path()?;
                let found = project.symbols.read().ok()?.file(&path)?;
                let items = crate::outline::items_of(&found);
                Some((path, Arc::<[OutlineItem]>::from(items)))
            });
        let weak = cx.weak_entity();
        let icons = workspace.icons().clone();
        let view = cx.new(|cx| {
            let mut palette = Self {
                project,
                files,
                workspace: weak,
                icons,
                focus: cx.focus_handle(),
                scroll: ScrollHandle::new(),
                mode,
                query: TextField::default(),
                selected: 0,
                rows: Vec::new(),
                actions,
                recent,
                open_tabs,
                edited_only: false,
                structure,
                symbol_batches: HashMap::new(),
                serial: 0,
                tasks: Vec::new(),
            };
            palette.refresh(cx);
            palette
        });
        let focus = view.read(cx).focus.clone();
        workspace.show_overlay(view.into(), focus, window, cx);
    }

    fn tab(&self) -> Tab {
        match self.mode {
            Mode::Everywhere(tab) => tab,
            Mode::Recent | Mode::Switcher => Tab::Files,
            Mode::Structure => Tab::Symbols,
            Mode::RunAnything | Mode::ChooseRun { .. } => Tab::Actions,
        }
    }

    /// Whether the query is `/` or `/name`: the list of commands.
    fn command_query(&self) -> bool {
        self.query.text.trim_start().starts_with('/')
    }

    fn set_tab(&mut self, tab: Tab, cx: &mut Context<Self>) {
        if let Mode::Everywhere(_) = self.mode {
            self.mode = Mode::Everywhere(tab);
            self.refresh(cx);
        }
    }

    fn cycle_tab(&mut self, step: isize, cx: &mut Context<Self>) {
        let at = TABS
            .iter()
            .position(|(tab, _)| *tab == self.tab())
            .unwrap_or(0);
        let next = (at as isize + step).rem_euclid(TABS.len() as isize) as usize;
        self.set_tab(TABS[next].0, cx);
    }

    /// Starts everything for the query and shows what is known at once.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.serial += 1;
        self.tasks.clear();
        self.symbol_batches.clear();
        self.rebuild(cx);
        if let Mode::Everywhere(tab) = self.mode
            && !self.command_query()
        {
            let scopes: &[SymbolScope] = match tab {
                Tab::All => &[SymbolScope::Types, SymbolScope::Members],
                Tab::Classes => &[SymbolScope::Types],
                Tab::Symbols => &[SymbolScope::Members],
                Tab::Files | Tab::Actions => &[],
            };
            let pattern = GotoQuery::parse(&self.query.text).pattern;
            if !pattern.is_empty() {
                for (scope_at, scope) in scopes.iter().enumerate() {
                    self.ask_sources(scope_at, *scope, &pattern, cx);
                }
            }
        }
    }

    fn ask_sources(
        &mut self,
        scope_at: usize,
        scope: SymbolScope,
        pattern: &str,
        cx: &mut Context<Self>,
    ) {
        let limit = if self.tab() == Tab::All {
            GROUP_ROWS
        } else {
            MAX_ROWS
        };
        let serial = self.serial;
        for (source_at, source) in self.project.sources.iter().enumerate() {
            let request = SymbolRequest {
                pattern: pattern.to_owned(),
                scope,
                limit,
            };
            let mut replies = ask(&**source, request);
            let task = cx.spawn(async move |this, cx| {
                while let Some(answer) = replies.next().await {
                    let rows = answer.value;
                    let arrived = this.update(cx, |this, cx| {
                        this.symbols_arrived((scope_at, source_at), serial, rows, cx)
                    });
                    if arrived.is_err() {
                        break;
                    }
                }
            });
            self.tasks.push(task);
        }
    }

    fn symbols_arrived(
        &mut self,
        key: (usize, usize),
        serial: usize,
        rows: Vec<SymbolRow>,
        cx: &mut Context<Self>,
    ) {
        if serial != self.serial {
            return;
        }
        self.symbol_batches.insert(key, rows);
        self.rebuild(cx);
    }

    fn batches_of(&self, scope_at: usize) -> Vec<Vec<SymbolRow>> {
        let mut keys: Vec<_> = self
            .symbol_batches
            .keys()
            .filter(|key| key.0 == scope_at)
            .collect();
        keys.sort();
        keys.into_iter()
            .map(|key| self.symbol_batches[key].clone())
            .collect()
    }

    fn file_rows(&self, cx: &App, limit: usize) -> Vec<RowData> {
        let query = GotoQuery::parse(&self.query.text);
        let spot = query.line.map(|line| Spot::Line {
            line: (line as usize).saturating_sub(1),
            column: query
                .column
                .map_or(0, |column| (column as usize).saturating_sub(1)),
        });
        let root = self.project.root.as_path();
        let paths: Vec<PathBuf> = if query.pattern.is_empty() {
            self.recent.iter().take(limit).cloned().collect()
        } else {
            let files = self.files.read(cx).clone();
            let all = files.iter().map(PathBuf::as_path);
            rank_files(&query.pattern, root, all, limit)
                .into_iter()
                .map(|ranked| ranked.item)
                .collect()
        };
        let paths: Vec<PathBuf> = if self.edited_only {
            let dirty = self.dirty_paths(cx);
            paths
                .into_iter()
                .filter(|path| dirty.iter().any(|open| open == path))
                .collect()
        } else {
            paths
        };
        paths
            .into_iter()
            .map(|path| {
                let name = path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                let parent = path.parent().unwrap_or(&path);
                let detail = parent
                    .strip_prefix(root)
                    .unwrap_or(parent)
                    .display()
                    .to_string();
                let icon = self.icons.icon_for(&path);
                RowData {
                    title: name,
                    detail,
                    icon,
                    shortcut: None,
                    indent: 0,
                    toggle: None,
                    item: Some(Item::File { path, spot }),
                }
            })
            .collect()
    }

    /// The open files that have unsaved text, for the "Show edited only" checkbox.
    fn dirty_paths(&self, cx: &App) -> Vec<PathBuf> {
        let Some(workspace) = self.workspace.upgrade() else {
            return Vec::new();
        };
        workspace
            .read(cx)
            .open_editors()
            .into_iter()
            .filter(|(_, view)| {
                view.clone()
                    .downcast::<EditorView>()
                    .is_ok_and(|editor| editor.read(cx).is_dirty())
            })
            .map(|(path, _)| path)
            .collect()
    }

    /// The tool windows of the Switcher, with the rows of the recent files.
    fn tool_rows(&self) -> Vec<RowData> {
        TOOLS
            .iter()
            .map(|(id, title, icon)| RowData {
                title: (*title).to_owned(),
                detail: String::new(),
                icon,
                shortcut: None,
                indent: 0,
                toggle: None,
                item: Some(Item::Tool(id)),
            })
            .collect()
    }

    /// The commands of a query that begins with `/`.
    fn command_rows(&self) -> Vec<RowData> {
        let prefix = self
            .query
            .text
            .trim_start()
            .trim_start_matches('/')
            .to_lowercase();
        COMMANDS
            .iter()
            .filter(|(token, _)| token.starts_with(&prefix))
            .map(|(token, id)| {
                let detail = self
                    .actions
                    .iter()
                    .find(|action| action.id == *id)
                    .map(|action| action.title.clone())
                    .unwrap_or_default();
                RowData {
                    title: format!("/{token}"),
                    detail,
                    icon: "",
                    shortcut: None,
                    indent: 0,
                    toggle: None,
                    item: Some(Item::Action(id)),
                }
            })
            .collect()
    }

    fn symbol_rows(&self, scope_at: usize, limit: usize) -> Vec<RowData> {
        let root = self.project.root.as_path();
        merge_rows(self.batches_of(scope_at), limit)
            .into_iter()
            .map(|row| {
                let place = match &row.target {
                    Target::Offset { path, .. } | Target::Line { path, .. } => {
                        let name = path.file_name().unwrap_or_default();
                        let _ = root;
                        name.to_string_lossy().into_owned()
                    }
                    Target::Uri(uri) => uri.clone(),
                };
                let detail = match &row.container {
                    Some(container) => format!("{container}  {place}"),
                    None => place,
                };
                RowData {
                    title: row.name.clone(),
                    detail,
                    icon: kind_icon(row.kind),
                    shortcut: None,
                    indent: 0,
                    toggle: None,
                    item: Some(Item::Symbol(row)),
                }
            })
            .collect()
    }

    fn action_rows(&self, limit: usize) -> Vec<RowData> {
        let mut matcher = NameMatcher::new();
        let matcher: &mut dyn FuzzyMatcher = &mut matcher;
        let found = rank(
            matcher,
            &self.query.text,
            self.actions.iter(),
            |action| &action.title,
            limit,
        );
        found
            .into_iter()
            .map(|ranked| RowData {
                title: ranked.item.title.clone(),
                detail: String::new(),
                icon: "",
                shortcut: ranked.item.shortcut.clone(),
                indent: 0,
                toggle: None,
                item: Some(Item::Action(ranked.item.id)),
            })
            .collect()
    }

    fn setting_rows(&self, limit: usize, cx: &App) -> Vec<RowData> {
        let settings = cx.try_global::<Settings>().cloned().unwrap_or_default();
        settings_catalog::search(&self.query.text, limit)
            .into_iter()
            .map(|setting| {
                let toggle = if setting.kind == SettingKind::Toggle {
                    settings_catalog::value(&settings, setting.id)
                } else {
                    None
                };
                RowData {
                    title: setting.title.to_owned(),
                    detail: setting.page.title().to_owned(),
                    icon: "general/settings",
                    shortcut: None,
                    indent: 0,
                    toggle,
                    item: Some(Item::Setting(setting.id)),
                }
            })
            .collect()
    }

    fn structure_rows(&self) -> Vec<RowData> {
        let Some((_, items)) = &self.structure else {
            return Vec::new();
        };
        let wanted = |item: &&OutlineItem| item.kind != OutlineKind::Module;
        let mut shown: Vec<&OutlineItem> = items.iter().filter(wanted).collect();
        if !self.query.text.trim().is_empty() {
            let mut matcher = NameMatcher::new();
            let matcher: &mut dyn FuzzyMatcher = &mut matcher;
            let found = rank(
                matcher,
                &self.query.text,
                shown,
                |item| &item.name,
                MAX_ROWS,
            );
            shown = found.into_iter().map(|ranked| ranked.item).collect();
        }
        shown
            .into_iter()
            .map(|item| RowData {
                title: item.name.clone(),
                // The signature without the name is shown in grey after it, as IDEA does.
                detail: item
                    .signature
                    .as_deref()
                    .map(|signature| {
                        signature
                            .strip_prefix(item.name.as_str())
                            .unwrap_or(signature)
                            .to_owned()
                    })
                    .unwrap_or_default(),
                icon: crate::outline::icon_of(item.kind),
                shortcut: None,
                indent: if self.query.text.trim().is_empty() {
                    item.depth
                } else {
                    0
                },
                toggle: None,
                item: Some(Item::Declaration {
                    offset: item.name_range.start,
                }),
            })
            .collect()
    }

    fn run_rows(&self, limit: usize, cx: &App) -> Vec<RowData> {
        let names: Vec<String> = cx
            .try_global::<crate::script::ActiveRunPanel>()
            .map(|active| {
                active
                    .0
                    .read(cx)
                    .state()
                    .configs
                    .iter()
                    .map(ToString::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let mut matcher = NameMatcher::new();
        let matcher: &mut dyn FuzzyMatcher = &mut matcher;
        let found = rank(
            matcher,
            &self.query.text,
            names.iter(),
            |name| name.as_str(),
            limit,
        );
        found
            .into_iter()
            .map(|ranked| RowData {
                title: ranked.item.clone(),
                detail: String::new(),
                icon: "nodes/module",
                shortcut: None,
                indent: 0,
                toggle: None,
                item: Some(Item::RunConfig(ranked.item.clone())),
            })
            .collect()
    }

    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let mut rows = Vec::new();
        match self.mode {
            Mode::Recent => rows = self.file_rows(cx, MAX_ROWS),
            Mode::Switcher => {
                rows = self.file_rows(cx, MAX_ROWS);
                rows.retain(|row| match &row.item {
                    Some(Item::File { path, .. }) => self.open_tabs.contains(path),
                    _ => true,
                });
                rows.extend(self.tool_rows());
            }
            Mode::Structure => rows = self.structure_rows(),
            Mode::RunAnything | Mode::ChooseRun { .. } => rows = self.run_rows(MAX_ROWS, cx),
            Mode::Everywhere(tab) => {
                if self.command_query() {
                    rows = self.command_rows();
                    self.rows = rows;
                    self.selected = self.first_selectable();
                    cx.notify();
                    return;
                }
                let all = tab == Tab::All;
                let limit = if all { GROUP_ROWS } else { MAX_ROWS };
                let searching = !GotoQuery::parse(&self.query.text).pattern.is_empty();
                let group = |title: &str, found: Vec<RowData>, rows: &mut Vec<RowData>| {
                    if found.is_empty() {
                        return;
                    }
                    if all {
                        rows.push(RowData::header(title));
                    }
                    rows.extend(found);
                };
                if matches!(tab, Tab::All | Tab::Classes) && searching {
                    group("Classes", self.symbol_rows(0, limit), &mut rows);
                }
                if matches!(tab, Tab::All | Tab::Files) && (searching || tab == Tab::Files) {
                    group("Files", self.file_rows(cx, limit), &mut rows);
                }
                if matches!(tab, Tab::All | Tab::Symbols) && searching {
                    let at = usize::from(all);
                    group("Symbols", self.symbol_rows(at, limit), &mut rows);
                }
                if matches!(tab, Tab::All | Tab::Actions) && searching {
                    let settings = self.setting_rows(limit, cx);
                    if all {
                        group("Settings", settings, &mut rows);
                    } else {
                        // Find Action lists settings before commands, as IDEA does.
                        rows.extend(settings);
                    }
                }
                if matches!(tab, Tab::All | Tab::Actions) && searching {
                    group("Actions", self.action_rows(limit), &mut rows);
                }
                if tab == Tab::Actions && !searching {
                    rows = self.action_rows(MAX_ROWS);
                    rows.extend(self.setting_rows(MAX_ROWS, cx));
                }
            }
        }
        self.rows = rows;
        let first = self.first_selectable();
        self.selected = match (self.mode, self.query.text.is_empty()) {
            // The previous file, so that a second Ctrl+E or Ctrl+Tab goes back.
            (Mode::Recent, true) if self.rows.len() > 1 => 1,
            // The Switcher starts on the previous file; with one file it stays on the
            // current one and the first Tab reaches the tool windows.
            (Mode::Switcher, true) => self
                .rows
                .iter()
                .enumerate()
                .skip(1)
                .find(|(_, row)| matches!(row.item, Some(Item::File { .. })))
                .map_or(first, |(index, _)| index),
            _ => first,
        };
        cx.notify();
    }

    fn first_selectable(&self) -> usize {
        self.rows
            .iter()
            .position(|row| row.item.is_some())
            .unwrap_or(0)
    }

    /// The toggle that Find Action flips in place: the selected row when it is a switch.
    fn toggle_selected_setting(&mut self, cx: &mut Context<Self>) -> bool {
        if self.mode != Mode::Everywhere(Tab::Actions) {
            return false;
        }
        let id = match self
            .rows
            .get(self.selected)
            .and_then(|row| row.item.as_ref())
        {
            Some(Item::Setting(id)) => *id,
            _ => return false,
        };
        self.toggle_setting(id, cx)
    }

    fn toggle_setting(&mut self, id: &'static str, cx: &mut Context<Self>) -> bool {
        let toggle =
            settings_catalog::find(id).is_some_and(|setting| setting.kind == SettingKind::Toggle);
        if !toggle {
            return false;
        }
        let workspace = self.workspace.clone();
        let changed = workspace
            .update(cx, |workspace, cx| {
                let mut changed = false;
                workspace.update_settings(
                    |settings| changed = settings_catalog::toggle(settings, id),
                    cx,
                );
                changed
            })
            .unwrap_or(false);
        if changed {
            self.rebuild(cx);
        }
        changed
    }

    /// The query field, for a test of selection and editing.
    #[cfg(test)]
    pub(crate) fn query_field(&self) -> &TextField {
        &self.query
    }

    /// The names of the rows with the grey parts after them, for scripts and tests.
    #[cfg(test)]
    pub fn rows_text(&self) -> Vec<(String, String)> {
        self.rows
            .iter()
            .map(|row| (row.title.clone(), row.detail.clone()))
            .collect()
    }

    /// The row the keys act on, for scripts and tests.
    #[cfg(test)]
    pub(crate) fn selected_row(&self) -> usize {
        self.selected
    }

    /// The keys that toggle the checkbox of Recent Files: Ctrl+E there does not reopen
    /// the list it is already in.
    fn toggle_edited(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.mode, Mode::Recent | Mode::Switcher) {
            return;
        }
        self.edited_only = !self.edited_only;
        self.rebuild(cx);
    }

    /// Tab, Shift+Tab and the arrow keys: the Switcher walks its list, the other lists
    /// of Search Everywhere keep Tab for their tabs.
    fn iterate(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.mode == Mode::Switcher {
            self.move_selection_wrapping(delta, cx);
            return;
        }
        if delta > 0 && self.complete_command(cx) {
            return;
        }
        self.cycle_tab(delta, cx);
    }

    /// Tab after `/`: the selected command replaces the name of the command in the query.
    fn complete_command(&mut self, cx: &mut Context<Self>) -> bool {
        if !matches!(self.mode, Mode::Everywhere(_)) || !self.command_query() {
            return false;
        }
        let Some(Item::Action(id)) = self
            .rows
            .get(self.selected)
            .and_then(|row| row.item.clone())
        else {
            return false;
        };
        let Some((token, _)) = COMMANDS.iter().find(|(_, command)| *command == id) else {
            return false;
        };
        self.query.select_all();
        self.query.insert_text(&format!("/{token}"));
        self.refresh(cx);
        true
    }

    /// PgDn and Ctrl+Down: the first row of the next group of Search Everywhere. The
    /// groups that find nothing are not in the list at all.
    fn next_group(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.mode == Mode::Switcher {
            self.move_selection_wrapping(delta, cx);
            return;
        }
        let headers: Vec<usize> = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.item.is_none())
            .map(|(index, _)| index)
            .collect();
        let target = if delta > 0 {
            headers
                .iter()
                .copied()
                .find(|header| *header > self.selected)
        } else {
            let current = headers
                .iter()
                .copied()
                .filter(|header| *header <= self.selected)
                .max();
            current.and_then(|current| {
                headers
                    .iter()
                    .copied()
                    .rev()
                    .find(|header| *header < current)
            })
        };
        let Some(header) = target else {
            return;
        };
        self.selected = header + 1;
        self.scroll.scroll_to_item(self.selected);
        cx.notify();
    }

    fn move_selection_wrapping(&mut self, delta: isize, cx: &mut Context<Self>) {
        let count = self.rows.len();
        if count == 0 {
            return;
        }
        let mut at = self.selected as isize;
        for _ in 0..count {
            at = (at + delta).rem_euclid(count as isize);
            if self.rows[at as usize].item.is_some() {
                self.selected = at as usize;
                break;
            }
        }
        self.scroll.scroll_to_item(self.selected);
        cx.notify();
    }

    /// Ctrl released: the Switcher opens what its list points at.
    fn ctrl_released(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.mode == Mode::Switcher {
            self.open(self.selected, false, window, cx);
        }
    }

    fn key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let key = &event.keystroke;
        if key.key == "right"
            && !key.modifiers.control
            && !key.modifiers.alt
            && self.toggle_selected_setting(cx)
        {
            cx.stop_propagation();
            return;
        }
        if !self.query.key_with_clipboard(event, false, cx) {
            return;
        }
        cx.stop_propagation();
        self.refresh(cx);
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let mut at = self.selected as isize;
        loop {
            at += delta;
            match self.rows.get(at as usize).filter(|_| at >= 0) {
                Some(row) if row.item.is_some() => {
                    self.selected = at as usize;
                    break;
                }
                Some(_) => continue,
                None => break,
            }
        }
        self.scroll.scroll_to_item(self.selected);
        cx.notify();
    }

    fn open(&mut self, index: usize, debug: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = self.rows.get(index).and_then(|row| row.item.clone()) else {
            return;
        };
        let structure_path = self.structure.as_ref().map(|(path, _)| path.clone());
        // Shift+Enter opens the file in a split. In the list of run configurations it
        // still starts the chosen one under the debugger.
        let split = debug && !matches!(self.mode, Mode::ChooseRun { .. });
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.dismiss(window, cx);
                match item {
                    Item::File { path, spot } => {
                        if split {
                            open_in_split(workspace, path, spot, window, cx);
                        } else {
                            navigation::go_to(workspace, path, spot, true, window, cx)
                        }
                    }
                    Item::Symbol(row) => open_symbol(workspace, row, split, window, cx),
                    Item::Declaration { offset } => {
                        if let Some(path) = structure_path {
                            let spot = Some(Spot::Offset(offset));
                            if split {
                                open_in_split(workspace, path, spot, window, cx);
                            } else {
                                navigation::go_to(workspace, path, spot, true, window, cx);
                            }
                        }
                    }
                    Item::Action(id) => {
                        workspace.run_command(id, window, cx);
                    }
                    Item::Setting(id) => {
                        SettingsView::show_at(workspace, window, cx, Some(id));
                    }
                    Item::Tool(id) => workspace.show_tool(id, window, cx),
                    Item::RunConfig(name) => {
                        let Some(runs) = cx
                            .try_global::<crate::script::ActiveRunPanel>()
                            .map(|active| active.0.clone())
                        else {
                            return;
                        };
                        let debug_here =
                            debug || matches!(self.mode, Mode::ChooseRun { debug: true });
                        let started = runs.update(cx, |panel, cx| {
                            if !panel.select_named(&name, cx) {
                                return false;
                            }
                            if debug_here {
                                panel.debug(window, cx);
                            } else {
                                panel.run(cx);
                            }
                            true
                        });
                        if started && !debug_here {
                            workspace.show_tool(RUN_TOOL, window, cx);
                        }
                    }
                }
            })
            .ok();
    }

    fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace
            .update(cx, |workspace, cx| workspace.dismiss(window, cx))
            .ok();
    }

    fn row(&self, index: usize, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let data = self.rows[index].clone();
        let debug = matches!(self.mode, Mode::ChooseRun { debug: true });
        if data.item.is_none() {
            return div()
                .id(("palette-row", index))
                .debug_selector(move || format!("palette-row-{index}"))
                .flex_none()
                .h(px(ROW_HEIGHT))
                .px(px(ROW_PADDING_X))
                .pt(px(SPACE_S))
                .text_color(theme.text_secondary)
                .child(data.title);
        }
        let selected = index == self.selected;
        let hover = theme.selection_hover;
        let declaration = matches!(data.item, Some(Item::Declaration { .. }));
        let signature = declaration
            .then(|| data.detail.clone())
            .filter(|detail| !detail.is_empty());
        let setting_id = match &data.item {
            Some(Item::Setting(id)) => Some(*id),
            _ => None,
        };
        let right = match (setting_id, data.toggle) {
            (Some(id), Some(on)) if self.mode == Mode::Everywhere(Tab::Actions) => div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(
                    div()
                        .text_color(theme.text_secondary)
                        .child(data.detail.clone()),
                )
                .child(
                    div()
                        .id(("setting-toggle", index))
                        .flex()
                        .items_center()
                        .justify_center()
                        .min_w(px(38.))
                        .h(px(20.))
                        .px(px(SPACE_M))
                        .rounded(px(CONTROL_RADIUS))
                        .when(on, |this| {
                            this.bg(theme.accent).text_color(theme.over_accent)
                        })
                        .when(!on, |this| this.bg(theme.pressed))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.toggle_setting(id, cx);
                        }))
                        .child(if on { "On" } else { "Off" }),
                )
                .into_any_element(),
            _ if declaration => div().into_any_element(),
            _ => div()
                .text_color(theme.text_secondary)
                .child(data.shortcut.clone().unwrap_or(data.detail.clone()))
                .into_any_element(),
        };
        div()
            .id(("palette-row", index))
            .flex()
            .items_center()
            .justify_between()
            .debug_selector(move || format!("palette-row-{index}"))
            .flex_none()
            .h(px(ROW_HEIGHT))
            .pl(px(ROW_PADDING_X + SPACE_L * data.indent as f32))
            .pr(px(ROW_PADDING_X))
            .rounded(px(ROW_RADIUS))
            .when(selected, |this| this.bg(theme.selection))
            .when(!selected, |this| this.hover(move |style| style.bg(hover)))
            .on_click(cx.listener(move |this, _, window, cx| this.open(index, debug, window, cx)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(SPACE_S))
                    .when(!data.icon.is_empty(), |this| {
                        this.child(icon(data.icon, theme))
                    })
                    .child(data.title)
                    .children(signature.map(|signature| {
                        div()
                            .debug_selector(move || format!("palette-signature-{index}"))
                            .text_color(theme.text_secondary)
                            .child(signature)
                    })),
            )
            .child(right)
    }
}

fn open_in_split(
    workspace: &mut Workspace,
    path: PathBuf,
    spot: Option<Spot>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    workspace.open_file_in_split(path.clone(), false, window, cx);
    if let (Some(spot), Some(view)) = (spot, workspace.view_of(&path))
        && let Ok(editor) = view.downcast::<EditorView>()
    {
        editor.update(cx, |editor, cx| editor.go_to(spot, cx));
    }
}

fn open_symbol(
    workspace: &mut Workspace,
    row: SymbolRow,
    split: bool,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    match row.target {
        Target::Offset { path, offset } => {
            let spot = Some(Spot::Offset(offset));
            if split {
                open_in_split(workspace, path, spot, window, cx);
            } else {
                navigation::go_to(workspace, path, spot, true, window, cx);
            }
        }
        Target::Line { path, line, column } => {
            let spot = Some(Spot::Line { line, column });
            if split {
                open_in_split(workspace, path, spot, window, cx);
            } else {
                navigation::go_to(workspace, path, spot, true, window, cx);
            }
        }
        Target::Uri(uri) => workspace.notify_user("Cannot open", uri, cx),
    }
}

impl Render for Palette {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let viewport = window.viewport_size();
        let width = px(WIDTH);
        let left = (viewport.width - width) / 2.;
        let top = viewport.height * 0.205;
        let query = self.query.clone();
        let collapsed =
            self.rows.is_empty() && query.text.is_empty() && self.mode != Mode::Structure;
        let empty = self.rows.is_empty();
        let rows: Vec<_> = (0..self.rows.len())
            .map(|index| self.row(index, &theme, cx))
            .collect();
        let active = self.tab();
        let active_name = TABS
            .iter()
            .find(|(tab, _)| *tab == active)
            .map_or("All", |(_, name)| *name);
        let heading = match self.mode {
            Mode::Recent => Some("Recent Files"),
            Mode::Switcher => Some("Switcher"),
            Mode::Structure => Some("File Structure"),
            Mode::RunAnything => Some("Run Anything"),
            Mode::ChooseRun { debug: false } => Some("Choose Run Configuration"),
            Mode::ChooseRun { debug: true } => Some("Choose Debug Configuration"),
            Mode::Everywhere(_) => None,
        };
        let chips = div()
            .flex()
            .items_center()
            .gap(px(2.))
            .children(TABS.iter().map(|(tab, name)| {
                let tab = *tab;
                let name = *name;
                let chosen = tab == active;
                div()
                    .id(name)
                    .debug_selector(move || format!("palette-chip-{name}"))
                    .flex()
                    .items_center()
                    .h(px(CONTROL_HEIGHT))
                    .px(px(SPACE_M))
                    .rounded(px(CONTROL_RADIUS))
                    .border_1()
                    .border_color(if chosen {
                        theme.tab_selected_border
                    } else {
                        theme.popup_bg
                    })
                    .when(chosen, |this| this.bg(theme.tab_selected))
                    .on_click(cx.listener(move |this, _, _, cx| this.set_tab(tab, cx)))
                    .child(name)
            }));

        let body = surface(&theme)
            .id("palette")
            .debug_selector(|| "palette".to_owned())
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .occlude()
            .w(width)
            .p(px(POPUP_PADDING))
            .when(!collapsed, |this| this.h(px(HEIGHT)))
            .on_key_down(cx.listener(|this, event, _, cx| this.key(event, cx)))
            .on_action(cx.listener(|this, _: &Previous, _, cx| this.move_selection(-1, cx)))
            .on_action(cx.listener(|this, _: &Next, _, cx| this.move_selection(1, cx)))
            .on_action(cx.listener(|this, _: &NextTab, _, cx| this.iterate(1, cx)))
            .on_action(cx.listener(|this, _: &PreviousTab, _, cx| this.iterate(-1, cx)))
            .on_action(cx.listener(|this, _: &NextGroup, _, cx| this.next_group(1, cx)))
            .on_action(cx.listener(|this, _: &PreviousGroup, _, cx| this.next_group(-1, cx)))
            .on_action(cx.listener(|this, _: &ToggleEdited, _, cx| this.toggle_edited(cx)))
            .on_modifiers_changed(
                cx.listener(|this, event: &ModifiersChangedEvent, window, cx| {
                    if !event.modifiers.control {
                        this.ctrl_released(window, cx);
                    }
                }),
            )
            .on_action(
                cx.listener(|this, _: &Open, window, cx| {
                    this.open(this.selected, false, window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &OpenDebug, window, cx| {
                this.open(this.selected, true, window, cx)
            }))
            .on_mouse_down_out(cx.listener(|this, _, window, cx| this.dismiss(window, cx)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_none()
                    .h(px(42.))
                    .px(px(SPACE_XS))
                    .child(match heading {
                        Some(title) => div().px(px(ROW_PADDING_X)).child(title).into_any_element(),
                        None => chips.into_any_element(),
                    }),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_none()
                    .debug_selector(|| "palette-input".to_owned())
                    .gap(px(SPACE_S))
                    .h(px(CONTROL_HEIGHT))
                    .mb(px(POPUP_PADDING))
                    .px(px(SPACE_M))
                    .rounded(px(CONTROL_RADIUS))
                    .border_2()
                    .border_color(theme.accent)
                    .child(icon("general/search", &theme))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .children(text_field_children(
                                &self.query,
                                theme.text,
                                theme.text,
                                theme.selection,
                                theme.caret,
                                18.,
                                true,
                            )),
                    ),
            )
            .when(matches!(self.mode, Mode::Recent | Mode::Switcher), |this| {
                this.child(
                    div()
                        .id("palette-edited-only")
                        .debug_selector(|| "palette-edited-only".to_owned())
                        .flex()
                        .items_center()
                        .flex_none()
                        .gap(px(SPACE_S))
                        .h(px(ROW_HEIGHT))
                        .mb(px(SPACE_S))
                        .px(px(ROW_PADDING_X))
                        .rounded(px(ROW_RADIUS))
                        .hover(move |style| style.bg(theme.selection_hover))
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_edited(cx)))
                        .child(checkbox(self.edited_only, &theme))
                        .child("Show edited only"),
                )
            })
            .when(!collapsed, |this| {
                this.child(
                    scrollable(
                        "palette-list",
                        &self.scroll,
                        div()
                            .id("palette-list")
                            .flex()
                            .flex_col()
                            .overflow_y_scroll()
                            .track_scroll(&self.scroll)
                            .children(rows)
                            .when(empty, |this| {
                                this.items_center()
                                    .justify_center()
                                    .text_color(theme.text_secondary)
                                    .child("Nothing found")
                            }),
                        &theme,
                        window,
                        cx,
                    )
                    .flex_1()
                    .min_h_0(),
                )
            });

        div()
            .absolute()
            .left(left)
            .top(top)
            .debug_selector(move || format!("palette-tab-{active_name}"))
            .child(enter("palette-enter", body))
    }
}
