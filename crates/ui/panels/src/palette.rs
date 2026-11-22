//! The popup of Search Everywhere and of what shares its look: Go to File, Go to Class,
//! Go to Symbol, Find Action, Recent Files, Switcher and the file structure. One view
//! with a mode; rows come from the project index, from the symbol sources and from the
//! commands of the window.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use editor_view::{EditorView, OutlineItem, OutlineKind, Spot};
use futures_util::StreamExt;
use lang_api::{GraphVisibility, SymbolKind, ask};
use project_model::TEST_UNIT;
use search::{
    FuzzyMatcher, GotoQuery, MatchKind, NameMatcher, SymbolRequest, SymbolRow, SymbolScope, Tab,
    Target, merge_rows, rank, rank_files,
};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::{
    AI_TOOL, BUILD_TOOL, COMMIT_TOOL, COVERAGE_TOOL, DATABASE_TOOL, DEBUG_TOOL, ENDPOINTS_TOOL,
    FileIcons, HTTP_TOOL, LOG_TOOL, PROBLEMS_TOOL, PROFILER_TOOL, PROJECT_TOOL, RUN_TOOL,
    SERVICES_TOOL, STRUCTURE_TOOL, Settings, TERMINAL_TOOL, TODO_TOOL, USAGES_TOOL, Workspace,
};

use crate::index::ProjectIndex;
use crate::navigation;
use crate::preview::{self, FilePreview, PreviewTarget};
use crate::project::{ActiveProjectPanel, NEW_GROUP, NewAction};
use crate::project_structure::ProjectEnvironmentGlobal;
use crate::settings::SettingsView;
use crate::settings_catalog::{self, SettingKind};

const CONTEXT: &str = "SearchEverywhere";
const WIDTH: f32 = 702.;
const HEIGHT: f32 = 600.;
/// The smallest size the window resizes to.
pub(crate) const MIN_WIDTH: f32 = 420.;
pub(crate) const MIN_HEIGHT: f32 = 300.;
/// The width of the preview right of the list of the palette.
const PREVIEW_WIDTH: f32 = 330.;
const GROUP_ROWS: usize = 5;
const MAX_ROWS: usize = 200;
/// The width of the column of tool windows in the Switcher, left of its files.
const SWITCHER_TOOLS_WIDTH: f32 = 240.;

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

macro_rules! palette_keys {
    ($(($keys:literal, $action:ident, $idea:expr, $mac:expr)),* $(,)?) => {
        pub fn bind_keys(cx: &mut App) {
            bind_keys_for(cx, keymap::platform());
        }

        /// Binds the keys of one platform, so that a test reads the macOS table on every
        /// runner.
        fn bind_keys_for(cx: &mut App, platform: keymap::Platform) {
            $(
                let row = workspace::KeyRow {
                    keys: $keys,
                    mac: $mac,
                    action: concat!("search_everywhere::", stringify!($action)),
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
                action: concat!("search_everywhere::", stringify!($action)),
                context: Some(CONTEXT),
                idea: $idea,
            }),*
        ];
    };
}

palette_keys![
    ("up", Previous, None, Some("up")),
    ("down", Next, None, Some("down")),
    ("enter", Open, Some("SearchEverywhere.SelectItem"), None),
    ("shift-enter", OpenDebug, None, Some("shift-enter")),
    ("tab", NextTab, Some("SearchEverywhere.NextTab"), None),
    (
        "shift-tab",
        PreviousTab,
        Some("SearchEverywhere.PrevTab"),
        None
    ),
    (
        "pagedown",
        NextGroup,
        Some("SearchEverywhere.NavigateToNextGroup"),
        None
    ),
    (
        "ctrl-down",
        NextGroup,
        Some("SearchEverywhere.NavigateToNextGroup"),
        None
    ),
    (
        "pageup",
        PreviousGroup,
        Some("SearchEverywhere.NavigateToPrevGroup"),
        None
    ),
    (
        "ctrl-up",
        PreviousGroup,
        Some("SearchEverywhere.NavigateToPrevGroup"),
        None
    ),
    // While Ctrl is held the Switcher keeps its list under the same keys.
    ("ctrl-tab", NextTab, None, Some("ctrl-tab")),
    ("ctrl-shift-tab", PreviousTab, None, Some("ctrl-shift-tab")),
    (
        "ctrl-e",
        ToggleEdited,
        Some("SwitcherRecentEditedChangedToggleCheckBox"),
        None
    ),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Everywhere(Tab),
    /// Ctrl+E lists the files recently active, Ctrl+Tab does the same and starts on the
    /// previous one.
    Recent,
    /// The files that were edited, the latest first, with the checkbox of Recent Files
    /// already on.
    RecentChanged,
    /// Ctrl+Tab while Ctrl is held: the open files in a column on the right and the tool
    /// windows in a column on the left. Left and Right switch the column, and releasing
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
    /// The text the query is matched against: the title, and the alias of the command.
    search: String,
    /// The main shortcut, spelled as the menus spell it.
    shortcut: Option<String>,
    /// The icon the menu row of the command shows, or `""`.
    icon: &'static str,
}

#[derive(Clone, PartialEq)]
enum Item {
    File {
        path: PathBuf,
        spot: Option<Spot>,
    },
    Symbol(SymbolRow),
    /// A declaration of the File Structure: the offset of its name and its visibility.
    Declaration {
        offset: usize,
        visibility: Option<GraphVisibility>,
    },
    Action(&'static str),
    /// A row of the New menu of the project tree, by its place in [`Palette::new_actions`].
    NewAction(usize),
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
    /// The character positions of the title that the query matched, for the accent colour.
    matched: Vec<usize>,
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
            matched: Vec::new(),
            item: None,
        }
    }
}

/// Whether a row is a tool window, which the Switcher lists in its left column.
fn is_tool(row: &RowData) -> bool {
    matches!(row.item, Some(Item::Tool(_)))
}

const TABS: [(Tab, &str); 5] = [
    (Tab::All, "All"),
    (Tab::Classes, "Classes"),
    (Tab::Files, "Files"),
    (Tab::Symbols, "Symbols"),
    (Tab::Actions, "Actions"),
];

/// The tool windows the Switcher lists, in the order they appear there.
const TOOLS: [(&str, &str, &str); 18] = [
    (PROJECT_TOOL, "Project", "toolwindows/project"),
    (COMMIT_TOOL, "Commit", "toolwindows/commit"),
    (LOG_TOOL, "Git Log", "general/vcs"),
    (STRUCTURE_TOOL, "Structure", "toolwindows/structure"),
    (RUN_TOOL, "Run", "toolwindows/run"),
    (DEBUG_TOOL, "Debug", "toolwindows/debug"),
    (TERMINAL_TOOL, "Terminal", "toolwindows/terminal"),
    (SERVICES_TOOL, "Services", "toolwindows/services"),
    (DATABASE_TOOL, "Database", "toolwindows/database"),
    (TODO_TOOL, "TODO", "toolwindows/todo"),
    (PROBLEMS_TOOL, "Problems", "toolwindows/problems"),
    (USAGES_TOOL, "Usages", "codeInsight/search"),
    (AI_TOOL, "AI Sessions", "own/aiSessions"),
    (COVERAGE_TOOL, "Coverage", "run/coverage"),
    (PROFILER_TOOL, "Profiler", "run/profiler"),
    (HTTP_TOOL, "HTTP Response", "toolwindows/http"),
    (ENDPOINTS_TOOL, "Endpoints", "toolwindows/endpoints"),
    (BUILD_TOOL, "Build", "toolwindows/tasks"),
];

/// The commands that open a tool window, with the icon of that window: the icon of its stripe
/// button, or of its row in the Switcher. Beans has neither, so it takes the leaf of the
/// Spring icons. Data Sources opens a dialog of the Database tool, so it takes that icon too.
/// Every other command keeps the icon of its menu row, or none.
const TOOL_COMMAND_ICONS: [(&str, &str); 12] = [
    ("window.show_bookmarks", "toolwindows/bookmarks"),
    ("window.toggle_project", "toolwindows/project"),
    ("window.toggle_terminal", "toolwindows/terminal"),
    ("window.toggle_assistant", "own/aiSessions"),
    ("window.database", "toolwindows/database"),
    ("window.show_endpoints", "toolwindows/endpoints"),
    ("window.show_beans", "run/springBoot"),
    ("console.open", "toolwindows/run"),
    ("git.show_log", "general/vcs"),
    ("git.show_file_history", "general/vcs"),
    ("git.show_console", "general/vcs"),
    ("database.data_sources", "toolwindows/database"),
];

/// The words, besides its title, that Find Action matches a command by. The Data Sources window
/// is also called Data Sources and Drivers, and IDEA's search finds it by that name.
const COMMAND_ALIASES: [(&str, &str); 1] = [("database.data_sources", "Data Sources and Drivers")];

/// The commands that already open a tool window, by tool id. Find Action lists such a
/// command instead of a second row for the same window, and finds it by the name of the
/// window as well.
const TOOL_COMMANDS: [(&str, &str); 6] = [
    (PROJECT_TOOL, "window.toggle_project"),
    (TERMINAL_TOOL, "window.toggle_terminal"),
    (AI_TOOL, "window.toggle_assistant"),
    (DATABASE_TOOL, "window.database"),
    (LOG_TOOL, "git.show_log"),
    (ENDPOINTS_TOOL, "window.show_endpoints"),
];

/// The window action that opens a tool, by tool id. Its key is the one Find Action writes
/// at the right edge of the row of that window.
const TOOL_WINDOW_ACTIONS: [(&str, &str); 12] = [
    (PROJECT_TOOL, "workspace::ToggleProject"),
    (COMMIT_TOOL, "workspace::ToggleCommit"),
    (LOG_TOOL, "workspace::ToggleLog"),
    (STRUCTURE_TOOL, "workspace::ToggleStructure"),
    (RUN_TOOL, "workspace::ToggleRun"),
    (DEBUG_TOOL, "workspace::ToggleDebug"),
    (TERMINAL_TOOL, "workspace::ToggleTerminal"),
    (SERVICES_TOOL, "workspace::ToggleServices"),
    (DATABASE_TOOL, "workspace::ToggleDatabase"),
    (PROBLEMS_TOOL, "workspace::ToggleProblems"),
    (USAGES_TOOL, "workspace::ToggleUsages"),
    (COVERAGE_TOOL, "workspace::ToggleCoverage"),
];

/// The text that Find Action matches a command against: its title, and its alias if it has one.
fn search_text(id: &str, title: &str) -> String {
    match COMMAND_ALIASES.iter().find(|(command, _)| *command == id) {
        Some((_, alias)) => format!("{title} {alias}"),
        None => title.to_owned(),
    }
}

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

/// The name of a declaration without the parameters a server may put after it.
fn bare_name(name: &str) -> &str {
    name.split('(').next().unwrap_or(name)
}

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

/// Rows in alphabetical order, without regard to case. The name as written and the id
/// come last, so that the order is the same from one run to the next.
fn row_order(left: (&str, &str), right: (&str, &str)) -> std::cmp::Ordering {
    let ((left_title, left_id), (right_title, right_id)) = (left, right);
    left_title
        .to_lowercase()
        .cmp(&right_title.to_lowercase())
        .then_with(|| left_title.cmp(right_title))
        .then_with(|| left_id.cmp(right_id))
}

/// The test source folders of the project model. Empty when the model has no test unit,
/// and then the `test` segment of a path tells a test file.
fn test_source_roots(cx: &App) -> Vec<PathBuf> {
    cx.try_global::<ProjectEnvironmentGlobal>()
        .and_then(|found| found.0.as_ref())
        .and_then(|environment| environment.model())
        .map(|model| {
            model
                .modules
                .iter()
                .flat_map(|module| module.units.iter())
                .filter(|unit| unit.name == TEST_UNIT)
                .flat_map(|unit| unit.source_roots.iter().cloned())
                .collect()
        })
        .unwrap_or_default()
}

/// Whether a file lies in a test source folder: the folders of the model when it has
/// them, else a path with a `test` segment.
fn is_test_file(root: &Path, path: &Path, test_roots: &[PathBuf]) -> bool {
    let relative = path.strip_prefix(root).unwrap_or(path);
    if test_roots.is_empty() {
        return relative
            .components()
            .any(|part| part.as_os_str().eq_ignore_ascii_case("test"));
    }
    test_roots.iter().any(|folder| {
        let folder = folder.strip_prefix(root).unwrap_or(folder);
        relative.starts_with(folder)
    })
}

/// The place of a file among the open tabs and the recent files, the earlier the better.
/// A file that neither list knows comes last.
fn recent_place(open: &[PathBuf], recent: &[PathBuf], path: &Path) -> usize {
    let key = ::platform::path_key(path);
    open.iter()
        .chain(recent.iter())
        .position(|candidate| ::platform::path_key(candidate) == key)
        .unwrap_or(usize::MAX)
}

/// The name of a file, cut to the spelling of the list.
fn file_name_of(path: &Path) -> String {
    path.file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned())
}

/// Whether `path` is the file of `name` with any extension: `Owner.txt` for `Owner`.
fn file_names_class(path: &Path, name: &str) -> bool {
    path.file_stem()
        .is_some_and(|stem| stem.to_string_lossy() == bare_name(name))
}

/// How a file of the project is placed against another match of the same kind: the
/// sources before the tests, then the shorter name, then the open and recent files, then
/// alphabetically. `Own` puts `Owner` before `OwnerController` even when only the second
/// is open.
fn file_order(
    left: &Path,
    right: &Path,
    root: &Path,
    open: &[PathBuf],
    recent: &[PathBuf],
    test_roots: &[PathBuf],
) -> std::cmp::Ordering {
    is_test_file(root, left, test_roots)
        .cmp(&is_test_file(root, right, test_roots))
        .then_with(|| file_name_of(left).len().cmp(&file_name_of(right).len()))
        .then_with(|| recent_place(open, recent, left).cmp(&recent_place(open, recent, right)))
        .then_with(|| {
            file_name_of(left)
                .to_lowercase()
                .cmp(&file_name_of(right).to_lowercase())
        })
        .then_with(|| file_name_of(left).cmp(&file_name_of(right)))
        .then_with(|| left.cmp(right))
}

/// The file a symbol stands in, when it stands in one of the project.
fn symbol_path(row: &SymbolRow) -> Option<&Path> {
    match &row.target {
        Target::Offset { path, .. } | Target::Line { path, .. } => Some(path.as_path()),
        Target::Uri(_) => None,
    }
}

/// How a symbol of the project is placed against another match of the same kind: the
/// sources before the tests, then the shorter name, then the open and recent order of the
/// file it stands in, then alphabetically.
fn symbol_order(
    left: &SymbolRow,
    right: &SymbolRow,
    root: &Path,
    open: &[PathBuf],
    recent: &[PathBuf],
    test_roots: &[PathBuf],
) -> std::cmp::Ordering {
    let place = |row: &SymbolRow| {
        symbol_path(row).map_or(usize::MAX, |path| recent_place(open, recent, path))
    };
    let test =
        |row: &SymbolRow| symbol_path(row).is_some_and(|path| is_test_file(root, path, test_roots));
    left.match_kind
        .cmp(&right.match_kind)
        .then_with(|| test(left).cmp(&test(right)))
        .then_with(|| left.name.len().cmp(&right.name.len()))
        .then_with(|| place(left).cmp(&place(right)))
        .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
        .then_with(|| left.name.cmp(&right.name))
        .then_with(|| left.container.cmp(&right.container))
}

/// A row of Find Action with the score of the query, before the cut.
struct RankedRow {
    /// Which of the five kinds of a match this row is.
    kind: MatchKind,
    /// The query spells a word of the row's title in full, `whitespace` for `Show whitespace`.
    word_exact: bool,
    /// Settings come after commands and windows with the same match, as IDEA lists them.
    setting: bool,
    /// The row opens a tool window, so it comes before another command that matches its
    /// name as well: `Project` finds the Project window, not Project Structure.
    tool: bool,
    /// The place of a setting in the catalog: settings with the same match keep the
    /// order of the page.
    order: usize,
    score: u32,
    /// What breaks a tie of the titles: the id of a command or the place of a New row.
    id: String,
    row: RowData,
}

/// A tool window that no command opens yet, as Find Action lists it: the row runs the
/// window itself.
struct ToolEntry {
    id: &'static str,
    title: &'static str,
    icon: &'static str,
    shortcut: Option<String>,
}

/// The key that opens a tool window, as the menus write it, when the window has one.
fn tool_shortcut(workspace: &Workspace, tool: &str) -> Option<String> {
    TOOL_WINDOW_ACTIONS
        .iter()
        .find(|(id, _)| *id == tool)
        .and_then(|(_, action)| workspace.shortcut_of_key(action))
}

/// Whether a command is the row of a tool window in Find Action.
fn is_tool_command(id: &str) -> bool {
    TOOL_COMMANDS.iter().any(|(_, command)| *command == id)
}

/// `title` as one run of text, with the characters the query matched in the accent colour
/// and bold. One run keeps the letters together: parts in a row would stand apart by its
/// gap. The positions are character indices, as [`NameMatcher::indices`] gives them.
fn title_text(title: &str, matched: &[usize], theme: &Theme) -> StyledText {
    let mut highlights = Vec::new();
    let mut run: Option<std::ops::Range<usize>> = None;
    for (at, (offset, character)) in title.char_indices().enumerate() {
        let end = offset + character.len_utf8();
        if matched.binary_search(&at).is_ok() {
            match &mut run {
                Some(range) if range.end == offset => range.end = end,
                _ => {
                    highlights.extend(run.take());
                    run = Some(offset..end);
                }
            }
        }
    }
    highlights.extend(run);
    let style = HighlightStyle {
        color: Some(theme.accent),
        font_weight: Some(FontWeight::BOLD),
        ..HighlightStyle::default()
    };
    StyledText::new(title.to_owned())
        .with_highlights(highlights.into_iter().map(|range| (range, style)))
}

/// The text a command is matched by, with the name of its tool window when the title of
/// the command does not carry that name: `AI Assistant Tool Window` is found as `AI Sessions`.
fn search_with_window_name(search: &str, name: &str) -> String {
    if search.to_lowercase().contains(&name.to_lowercase()) {
        search.to_owned()
    } else {
        format!("{search} {name}")
    }
}

/// Adds a row with the kind of the match of its text, when the text matches at all.
fn push_ranked(
    matcher: &mut NameMatcher,
    found: &mut Vec<RankedRow>,
    search: &str,
    setting: bool,
    tool: bool,
    id: String,
    row: RowData,
) {
    if let Some((kind, score)) = matcher.classify(search) {
        let word_exact = matcher.spells_word(search);
        found.push(RankedRow {
            kind,
            word_exact,
            setting,
            tool,
            order: 0,
            score,
            id,
            row,
        });
    }
}

/// Drops the matches of scattered letters when a row matches better: IDEA does not show
/// `Toggle rendered doc` for `TODO` beside the names that begin with it.
fn keep_best_kinds(found: &mut Vec<RankedRow>) {
    let best = found.iter().map(|row| row.kind).min();
    if best.is_some_and(|kind| kind < MatchKind::Scattered) {
        found.retain(|row| row.kind < MatchKind::Scattered);
    }
}

/// The order of Find Action: the better kind of a match first, then the row that spells
/// a whole word, then an action before a setting, a tool window before another command,
/// and the score.
fn sort_ranked(found: &mut [RankedRow]) {
    found.sort_by(|left, right| {
        left.kind
            .cmp(&right.kind)
            .then_with(|| right.word_exact.cmp(&left.word_exact))
            .then_with(|| left.setting.cmp(&right.setting))
            .then_with(|| right.tool.cmp(&left.tool))
            .then_with(|| right.score.cmp(&left.score))
            .then_with(|| left.order.cmp(&right.order))
            .then_with(|| row_order((&left.row.title, &left.id), (&right.row.title, &right.id)))
    });
}

/// The better kind of the match of a setting: its title, or all the words the settings
/// search reads, which carry its page and the extra words.
fn best_kind(matcher: &mut NameMatcher, title: &str, text: &str) -> (MatchKind, u32) {
    match (matcher.classify(title), matcher.classify(text)) {
        (Some(title), Some(text)) => {
            if text.0 < title.0 {
                text
            } else {
                title
            }
        }
        (Some(found), None) | (None, Some(found)) => found,
        (None, None) => (MatchKind::Scattered, 0),
    }
}

/// The icon of a command that opens a tool window, from [`TOOL_COMMAND_ICONS`].
fn tool_command_icon(id: &str) -> Option<&'static str> {
    TOOL_COMMAND_ICONS
        .iter()
        .find(|(command, _)| *command == id)
        .map(|(_, icon)| *icon)
}

/// The icon of each menu row that runs a command, by the command id, from the rows of the
/// main menu and of their submenus.
fn menu_icons(entries: &[MenuEntry]) -> HashMap<String, &'static str> {
    let mut found = HashMap::new();
    collect_menu_icons(entries, &mut found);
    found
}

fn collect_menu_icons(entries: &[MenuEntry], found: &mut HashMap<String, &'static str>) {
    for entry in entries {
        if let (Some(id), Some(icon)) = (&entry.id, entry.icon) {
            found.entry(id.to_string()).or_insert(icon);
        }
        collect_menu_icons(&entry.submenu, found);
    }
}

/// The open tabs in the order the Switcher lists them: the tab on screen first, the files
/// by last use, then the tabs that no editor has shown in this run, in tab order. The
/// rows keep the spelling of `open`, so that Enter opens the tab itself.
pub(crate) fn switcher_order(
    recent: &[PathBuf],
    open: &[PathBuf],
    active: Option<&Path>,
) -> Vec<PathBuf> {
    let open_by_key: HashMap<PathBuf, &PathBuf> = open
        .iter()
        .map(|path| (::platform::path_key(path), path))
        .collect();
    let mut placed = HashSet::new();
    let mut order = Vec::with_capacity(open.len());
    let by_use = active
        .into_iter()
        .chain(recent.iter().map(PathBuf::as_path));
    for path in by_use.chain(open.iter().map(PathBuf::as_path)) {
        let key = ::platform::path_key(path);
        if let Some(open_path) = open_by_key.get(&key)
            && placed.insert(key)
        {
            order.push(open_path.to_path_buf());
        }
    }
    order
}

/// The line and the column that a query names, as the spot of the file rows.
fn file_spot(query: &GotoQuery) -> Option<Spot> {
    query.line.map(|line| Spot::Line {
        line: (line as usize).saturating_sub(1),
        column: query
            .column
            .map_or(0, |column| (column as usize).saturating_sub(1)),
    })
}

pub struct Palette {
    project: ProjectIndex,
    files: Entity<Arc<Vec<PathBuf>>>,
    workspace: WeakEntity<Workspace>,
    icons: FileIcons,
    focus: FocusHandle,
    scroll: ScrollHandle,
    /// The scroll of the Switcher's column of tool windows; `scroll` is the one of its files.
    tool_scroll: ScrollHandle,
    mode: Mode,
    query: TextField,
    selected: usize,
    rows: Vec<RowData>,
    actions: Vec<ActionEntry>,
    /// The tool windows without a command of their own, which Find Action lists as rows
    /// that open the window.
    tools: Vec<ToolEntry>,
    /// Every tool window of the Switcher's left column, in the order of [`TOOLS`], with the
    /// key that opens it.
    switcher_tools: Vec<ToolEntry>,
    /// The rows of the New menu of the project tree, which Find Action lists with the
    /// commands and runs as the menu does.
    new_actions: Vec<NewAction>,
    recent: Vec<PathBuf>,
    /// The files that were edited in the project, the latest first.
    edited: Vec<PathBuf>,
    /// The open tabs in the order the Switcher lists them, see [`switcher_order`].
    switcher: Vec<PathBuf>,
    /// The Recent Files checkbox: only the files that were edited in the project.
    edited_only: bool,
    structure: Option<(PathBuf, Arc<[OutlineItem]>)>,
    /// Answers of the symbol sources for the query on screen, by scope and source.
    symbol_batches: HashMap<(usize, usize), Vec<SymbolRow>>,
    serial: usize,
    /// The keys moved the selection: a late group keeps that row, not the first one.
    navigated: bool,
    /// The reply streams of the query on screen that are not over yet, for a test that
    /// reads the rows: only the last answer of a group may still change them.
    pending_replies: usize,
    tasks: Vec<Task<()>>,
    /// The preview of the selected row, right of the list.
    preview: Entity<FilePreview>,
    /// The Preview button of the header shows it.
    preview_open: bool,
    /// The state folder that remembers the button.
    store: Option<Arc<store::Store>>,
    /// How many times the popup asked the window for a frame, for a test of typing.
    #[cfg(test)]
    pub(crate) frames_asked: usize,
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
        // The icons come from the rows of the main menu, by command: the same rows the menus
        // show, so that a command has the icon of its menu row here as well. A command that
        // opens a tool window has the icon of that window instead.
        let menu_icons = menu_icons(&workspace.main_menu(cx));
        let mut actions: Vec<ActionEntry> = workspace
            .commands()
            .visible()
            .map(|command| ActionEntry {
                id: command.id,
                title: command.title.clone(),
                search: search_text(command.id, &command.title),
                shortcut: workspace.shortcut_of_key(command.id),
                icon: tool_command_icon(command.id)
                    .or_else(|| menu_icons.get(command.id).copied())
                    .unwrap_or(""),
            })
            .collect();
        // Every tool window is a row of Find Action. A window that already has a command
        // keeps that row, found by the name of the window too and with the key of the
        // window; the others get a row that opens the window itself.
        let mut tools = Vec::new();
        for (id, title, icon) in TOOLS {
            let shortcut = tool_shortcut(workspace, id);
            let command = TOOL_COMMANDS
                .iter()
                .find(|(tool, _)| *tool == id)
                .map(|(_, command)| *command);
            if let Some(action) =
                command.and_then(|command| actions.iter_mut().find(|action| action.id == command))
            {
                action.search = search_with_window_name(&action.search, title);
                if action.shortcut.is_none() {
                    action.shortcut = shortcut;
                }
                if action.icon.is_empty() {
                    action.icon = icon;
                }
            } else {
                tools.push(ToolEntry {
                    id,
                    title,
                    icon,
                    shortcut,
                });
            }
        }
        let switcher_tools: Vec<ToolEntry> = TOOLS
            .iter()
            .map(|&(id, title, icon)| ToolEntry {
                id,
                title,
                icon,
                shortcut: tool_shortcut(workspace, id),
            })
            .collect();
        let recent = navigation::recent(cx);
        let edited = workspace.recent_edited().to_vec();
        let open_tabs: Vec<PathBuf> = workspace.open_tabs().into_iter().flatten().collect();
        let switcher = switcher_order(&recent, &open_tabs, workspace.active_path().as_deref());
        let new_actions = cx
            .try_global::<ActiveProjectPanel>()
            .map(|panel| panel.0.read(cx).new_actions())
            .unwrap_or_default();
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
        let edited_only = mode == Mode::RecentChanged;
        let store = workspace.store();
        let preview_open = preview::load(store.as_deref(), project.root.as_path()).palette;
        let preview = cx.new(|_| FilePreview::default());
        let view = cx.new(|cx| {
            let mut palette = Self {
                project,
                files,
                workspace: weak,
                icons,
                focus: cx.focus_handle(),
                scroll: ScrollHandle::new(),
                tool_scroll: ScrollHandle::new(),
                mode,
                query: TextField::default(),
                selected: 0,
                rows: Vec::new(),
                actions,
                tools,
                switcher_tools,
                new_actions,
                recent,
                edited,
                switcher,
                edited_only,
                structure,
                symbol_batches: HashMap::new(),
                serial: 0,
                navigated: false,
                pending_replies: 0,
                tasks: Vec::new(),
                preview,
                preview_open,
                store,
                #[cfg(test)]
                frames_asked: 0,
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
            Mode::Recent | Mode::RecentChanged | Mode::Switcher => Tab::Files,
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
        self.pending_replies = 0;
        self.navigated = false;
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
            self.pending_replies += 1;
            let mut replies = ask(&**source, request);
            let task = cx.spawn(async move |this, cx| {
                while let Some(answer) = replies.next().await {
                    let rows = answer.value;
                    let arrived = this.update(cx, |this, cx| {
                        this.symbols_arrived((scope_at, source_at), serial, rows, cx)
                    });
                    if arrived.is_err() {
                        return;
                    }
                }
                this.update(cx, |this, _| this.replies_finished(serial))
                    .ok();
            });
            self.tasks.push(task);
        }
    }

    /// One stream of the query on screen is over: its group has all the rows it will
    /// send.
    fn replies_finished(&mut self, serial: usize) {
        if serial == self.serial {
            self.pending_replies = self.pending_replies.saturating_sub(1);
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
        self.rebuild_keeping_selection(cx);
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
        let root = self.project.root.as_path();
        // The edited list outlives the tabs and the saves: a file lands there on the
        // first edit and stays after the tab is closed.
        let edited = self.edited_only.then(|| self.edited_paths());
        let paths: Vec<PathBuf> = if query.pattern.is_empty() {
            match &edited {
                Some(edited) => edited.iter().take(limit).cloned().collect(),
                None => self.recent.iter().take(limit).cloned().collect(),
            }
        } else {
            let files;
            let all: Box<dyn Iterator<Item = &std::path::Path>> = match &edited {
                Some(edited) => Box::new(edited.iter().map(PathBuf::as_path)),
                None => {
                    files = self.files.read(cx).clone();
                    Box::new(files.iter().map(PathBuf::as_path))
                }
            };
            let mut ranked = rank_files(&query.pattern, root, all, limit);
            let roots = test_source_roots(cx);
            ranked.sort_by(|left, right| {
                left.kind.cmp(&right.kind).then_with(|| {
                    file_order(
                        &left.item,
                        &right.item,
                        root,
                        &self.switcher,
                        &self.recent,
                        &roots,
                    )
                })
            });
            ranked.into_iter().map(|ranked| ranked.item).collect()
        };
        self.file_rows_of(paths, file_spot(&query))
    }

    /// The Switcher's rows: the open tabs in the order of `switcher`. A query ranks those
    /// tabs only, so that a tab is not lost behind the files of the whole project.
    fn switcher_rows(&self, limit: usize, cx: &App) -> Vec<RowData> {
        let query = GotoQuery::parse(&self.query.text);
        let paths: Vec<PathBuf> = if query.pattern.is_empty() {
            self.switcher.iter().take(limit).cloned().collect()
        } else {
            let root = self.project.root.as_path();
            let mut ranked = rank_files(
                &query.pattern,
                root,
                self.switcher.iter().map(PathBuf::as_path),
                limit,
            );
            let roots = test_source_roots(cx);
            ranked.sort_by(|left, right| {
                left.kind.cmp(&right.kind).then_with(|| {
                    file_order(
                        &left.item,
                        &right.item,
                        root,
                        &self.switcher,
                        &self.recent,
                        &roots,
                    )
                })
            });
            ranked.into_iter().map(|ranked| ranked.item).collect()
        };
        self.file_rows_of(paths, file_spot(&query))
    }

    /// The rows of the files `paths`, each with the spot of the query.
    fn file_rows_of(&self, paths: Vec<PathBuf>, spot: Option<Spot>) -> Vec<RowData> {
        let root = self.project.root.as_path();
        paths
            .into_iter()
            .map(|path| {
                let name = path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                let parent = path.parent().unwrap_or(&path);
                // Rebuilt from its parts, so that a path spelled with `/` on Windows shows
                // the same separators as the others in the list.
                let detail = parent
                    .strip_prefix(root)
                    .unwrap_or(parent)
                    .components()
                    .collect::<PathBuf>()
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
                    matched: Vec::new(),
                    item: Some(Item::File { path, spot }),
                }
            })
            .collect()
    }

    /// The files that were edited in the project, for the "Show edited only" checkbox.
    fn edited_paths(&self) -> Vec<PathBuf> {
        self.edited.clone()
    }

    /// The tool windows of the Switcher's left column, each with the key that opens it. The
    /// query keeps the windows whose names it matches, as it keeps the files.
    fn tool_rows(&self) -> Vec<RowData> {
        let rows = self.switcher_tools.iter().map(|tool| RowData {
            title: tool.title.to_owned(),
            detail: String::new(),
            icon: tool.icon,
            shortcut: tool.shortcut.clone(),
            indent: 0,
            toggle: None,
            matched: Vec::new(),
            item: Some(Item::Tool(tool.id)),
        });
        let pattern = GotoQuery::parse(&self.query.text).pattern;
        if pattern.is_empty() {
            return rows.collect();
        }
        let mut matcher = NameMatcher::new();
        let matcher: &mut dyn FuzzyMatcher = &mut matcher;
        rank(matcher, &pattern, rows, |row| row.title.as_str(), MAX_ROWS)
            .into_iter()
            .map(|ranked| ranked.item)
            .collect()
    }

    /// The titles and the icons of the tool rows, as the Switcher draws them.
    #[cfg(test)]
    pub(crate) fn tool_icons(&self) -> Vec<(String, &'static str)> {
        self.tool_rows()
            .into_iter()
            .map(|row| (row.title, row.icon))
            .collect()
    }

    /// The titles and the keys of the tool rows, as the Switcher draws them, for tests.
    #[cfg(test)]
    pub(crate) fn tool_shortcuts(&self) -> Vec<(String, Option<String>)> {
        self.tool_rows()
            .into_iter()
            .map(|row| (row.title, row.shortcut))
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
                    matched: Vec::new(),
                    item: Some(Item::Action(id)),
                }
            })
            .collect()
    }

    fn symbol_rows(&self, scope_at: usize, limit: usize, classes: bool, cx: &App) -> Vec<RowData> {
        let root = self.project.root.as_path();
        let mut rows = merge_rows(self.batches_of(scope_at), limit);
        let roots = test_source_roots(cx);
        rows.sort_by(|left, right| {
            symbol_order(left, right, root, &self.switcher, &self.recent, &roots)
        });
        rows.into_iter()
            .map(|row| {
                let place = match &row.target {
                    Target::Offset { path, .. } | Target::Line { path, .. } => {
                        // A class in the file of its own name says nothing new with the
                        // name of that file; in any other file the name is shown.
                        if classes && file_names_class(path, &row.name) {
                            String::new()
                        } else {
                            file_name_of(path)
                        }
                    }
                    Target::Uri(uri) => uri.clone(),
                };
                let detail = match (&row.container, place.is_empty()) {
                    (Some(container), false) => format!("{container}  {place}"),
                    (Some(container), true) => container.clone(),
                    (None, false) => place,
                    (None, true) => String::new(),
                };
                RowData {
                    title: row.name.clone(),
                    detail,
                    icon: kind_icon(row.kind),
                    shortcut: None,
                    indent: 0,
                    toggle: None,
                    matched: Vec::new(),
                    item: Some(Item::Symbol(row)),
                }
            })
            .collect()
    }

    /// The commands, the rows of the New menu and the tool windows as rows of Find
    /// Action, with the kind of their match but without the settings.
    fn action_rows(&self, limit: usize) -> Vec<RowData> {
        let mut matcher = NameMatcher::new();
        matcher.set_pattern(&self.query.text);
        let mut found = self.matched_actions(&mut matcher);
        keep_best_kinds(&mut found);
        sort_ranked(&mut found);
        found.truncate(limit);
        found.into_iter().map(|ranked| ranked.row).collect()
    }

    /// Find Action ranks the commands, the tool windows and the settings together: a
    /// name that matches better comes first, whatever the kind of the row, and an
    /// action comes before a setting with the same match.
    fn find_action_rows(&self, limit: usize, cx: &App) -> Vec<RowData> {
        let settings = cx.try_global::<Settings>().cloned().unwrap_or_default();
        let mut matcher = NameMatcher::new();
        matcher.set_pattern(&self.query.text);
        let mut found = self.matched_actions(&mut matcher);
        for (order, setting) in settings_catalog::all()
            .iter()
            .enumerate()
            .filter(|(_, setting)| setting.matches(&self.query.text))
        {
            let text = format!(
                "{} {} {}",
                setting.title,
                setting.page.title(),
                setting.keywords
            );
            let (kind, score) = best_kind(&mut matcher, setting.title, &text);
            let word_exact = matcher.spells_word(setting.title) || matcher.spells_word(&text);
            let toggle = if setting.kind == SettingKind::Toggle {
                settings_catalog::value(&settings, setting.id)
            } else {
                None
            };
            found.push(RankedRow {
                kind,
                word_exact,
                setting: true,
                tool: false,
                order,
                score,
                id: format!("setting:{}", setting.id),
                row: RowData {
                    title: setting.title.to_owned(),
                    detail: setting.page.title().to_owned(),
                    icon: "general/settings",
                    shortcut: None,
                    indent: 0,
                    toggle,
                    matched: Vec::new(),
                    item: Some(Item::Setting(setting.id)),
                },
            });
        }
        keep_best_kinds(&mut found);
        sort_ranked(&mut found);
        found.truncate(limit);
        found.into_iter().map(|ranked| ranked.row).collect()
    }

    fn matched_actions(&self, matcher: &mut NameMatcher) -> Vec<RankedRow> {
        let mut found = Vec::new();
        // The windows lead the rows of their names while the user searches; without a
        // query the list keeps the alphabetical order of every action.
        let searching = !self.query.text.trim().is_empty();
        for action in &self.actions {
            push_ranked(
                matcher,
                &mut found,
                &action.search,
                false,
                searching && is_tool_command(action.id),
                action.id.to_owned(),
                RowData {
                    title: action.title.clone(),
                    detail: String::new(),
                    icon: action.icon,
                    shortcut: action.shortcut.clone(),
                    indent: 0,
                    toggle: None,
                    matched: Vec::new(),
                    item: Some(Item::Action(action.id)),
                },
            );
        }
        // The rows of the New menu of the project tree are found by their titles and by
        // the word of their group, which stands at their right.
        for (at, action) in self.new_actions.iter().enumerate() {
            push_ranked(
                matcher,
                &mut found,
                &action.search,
                false,
                false,
                format!("new:{at}"),
                RowData {
                    title: action.title.clone(),
                    detail: NEW_GROUP.to_owned(),
                    icon: action.icon,
                    shortcut: None,
                    indent: 0,
                    toggle: None,
                    matched: Vec::new(),
                    item: Some(Item::NewAction(at)),
                },
            );
        }
        for tool in &self.tools {
            push_ranked(
                matcher,
                &mut found,
                tool.title,
                false,
                searching,
                format!("tool:{}", tool.id),
                RowData {
                    title: tool.title.to_owned(),
                    detail: String::new(),
                    icon: tool.icon,
                    shortcut: tool.shortcut.clone(),
                    indent: 0,
                    toggle: None,
                    matched: Vec::new(),
                    item: Some(Item::Tool(tool.id)),
                },
            );
        }
        found
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
                    matched: Vec::new(),
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
            // The filter looks at the name alone: a type in a signature finds no row.
            let found = rank(
                matcher,
                &self.query.text,
                shown,
                |item| bare_name(&item.name),
                MAX_ROWS,
            );
            shown = found.into_iter().map(|ranked| ranked.item).collect();
        }
        shown
            .into_iter()
            .map(|item| {
                let title = bare_name(&item.name);
                // The signature without the name is shown in grey after it, as IDEA does; a
                // declaration without a signature shows the parameters the name carries.
                let label = item.signature.as_deref().unwrap_or(&item.name);
                RowData {
                    title: title.to_owned(),
                    detail: label.strip_prefix(title).unwrap_or(label).to_owned(),
                    icon: crate::outline::icon_of(item.kind),
                    shortcut: None,
                    indent: if self.query.text.trim().is_empty() {
                        item.depth
                    } else {
                        0
                    },
                    toggle: None,
                    matched: Vec::new(),
                    item: Some(Item::Declaration {
                        offset: item.name_range.start,
                        visibility: item.visibility,
                    }),
                }
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
                matched: Vec::new(),
                item: Some(Item::RunConfig(ranked.item.clone())),
            })
            .collect()
    }

    fn rebuild(&mut self, cx: &mut Context<Self>) {
        self.rebuild_keeping(false, cx);
    }

    /// Marks the letters of every title that the query matched, so that the row shows them
    /// in the accent colour. The same pass serves every tab of the popup.
    fn mark_matches(&mut self) {
        let pattern = match self.mode {
            Mode::Everywhere(Tab::Actions)
            | Mode::Structure
            | Mode::RunAnything
            | Mode::ChooseRun { .. } => self.query.text.trim().to_owned(),
            _ => GotoQuery::parse(&self.query.text).pattern,
        };
        if pattern.is_empty() {
            for row in &mut self.rows {
                row.matched.clear();
            }
            return;
        }
        let mut matcher = NameMatcher::new();
        matcher.set_pattern(&pattern);
        let mut indices = Vec::new();
        for row in &mut self.rows {
            row.matched.clear();
            if row.item.is_some() && matcher.indices(&row.title, &mut indices).is_some() {
                row.matched = indices.iter().map(|&at| at as usize).collect();
            }
        }
    }

    /// Rows from a late answer: the row the user is on stays selected, even when the
    /// answer puts rows above it. Another query starts again from the first row.
    fn rebuild_keeping_selection(&mut self, cx: &mut Context<Self>) {
        self.rebuild_keeping(true, cx);
    }

    fn rebuild_keeping(&mut self, keep_selection: bool, cx: &mut Context<Self>) {
        let previous = (keep_selection && self.navigated)
            .then(|| {
                self.rows
                    .get(self.selected)
                    .and_then(|row| row.item.clone())
            })
            .flatten();
        let mut rows = Vec::new();
        match self.mode {
            Mode::Recent | Mode::RecentChanged => rows = self.file_rows(cx, MAX_ROWS),
            Mode::Switcher => {
                // The checkbox brings the whole edited list, also the files whose tab
                // is closed; without it the Switcher lists the open tabs.
                rows = if self.edited_only {
                    self.file_rows(cx, MAX_ROWS)
                } else {
                    self.switcher_rows(MAX_ROWS, cx)
                };
                rows.extend(self.tool_rows());
            }
            Mode::Structure => rows = self.structure_rows(),
            Mode::RunAnything | Mode::ChooseRun { .. } => rows = self.run_rows(MAX_ROWS, cx),
            Mode::Everywhere(tab) => {
                if self.command_query() {
                    rows = self.command_rows();
                } else {
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
                        group("Classes", self.symbol_rows(0, limit, true, cx), &mut rows);
                    }
                    if matches!(tab, Tab::All | Tab::Files) && (searching || tab == Tab::Files) {
                        group("Files", self.file_rows(cx, limit), &mut rows);
                    }
                    if matches!(tab, Tab::All | Tab::Symbols) && searching {
                        let at = usize::from(all);
                        group("Symbols", self.symbol_rows(at, limit, false, cx), &mut rows);
                    }
                    if matches!(tab, Tab::All | Tab::Actions) && searching {
                        if all {
                            group("Settings", self.setting_rows(limit, cx), &mut rows);
                            group("Actions", self.action_rows(limit), &mut rows);
                        } else {
                            // Find Action ranks commands, tool windows and settings
                            // together: the name that matches better comes first, and an
                            // action comes before a setting with the same match.
                            rows = self.find_action_rows(limit, cx);
                        }
                    }
                    if tab == Tab::Actions && !searching {
                        // Every command, so that the alphabetical end of the list is reachable
                        // without a query; the settings follow them.
                        rows = self.action_rows(
                            self.actions.len() + self.new_actions.len() + self.tools.len(),
                        );
                        rows.extend(self.setting_rows(MAX_ROWS, cx));
                    }
                }
            }
        }
        self.rows = rows;
        self.mark_matches();
        let first = self.first_selectable();
        self.selected = match previous.and_then(|item| {
            self.rows
                .iter()
                .position(|row| row.item.as_ref() == Some(&item))
        }) {
            Some(index) => index,
            None => match (self.mode, self.query.text.is_empty()) {
                // The previous file, so that a second Ctrl+E or Ctrl+Tab goes back.
                (Mode::Recent | Mode::RecentChanged, true) if self.rows.len() > 1 => 1,
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
            },
        };
        self.show_selected_preview(cx);
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

    /// The names of the rows with the character positions the query matched, for tests.
    #[cfg(test)]
    pub(crate) fn rows_matched(&self) -> Vec<(String, Vec<usize>)> {
        self.rows
            .iter()
            .map(|row| (row.title.clone(), row.matched.clone()))
            .collect()
    }

    /// The declarations of the File Structure with the mark of their visibility, for tests.
    #[cfg(test)]
    pub(crate) fn declaration_marks(&self) -> Vec<(String, &'static str)> {
        self.rows
            .iter()
            .filter_map(|row| match &row.item {
                Some(Item::Declaration { visibility, .. }) => Some((
                    row.title.clone(),
                    crate::outline::visibility_icon(*visibility),
                )),
                _ => None,
            })
            .collect()
    }

    /// The commands on screen with the key at their right edge and the icon at their left,
    /// for scripts and tests. The settings below them are left out.
    #[cfg(test)]
    pub(crate) fn action_rows_text(&self) -> Vec<(String, Option<String>, &'static str)> {
        self.rows
            .iter()
            .filter(|row| matches!(row.item, Some(Item::Action(_))))
            .map(|row| (row.title.clone(), row.shortcut.clone(), row.icon))
            .collect()
    }

    /// The rows of the New menu on screen with the group label at their right edge and the
    /// icon at their left, for scripts and tests.
    #[cfg(test)]
    pub(crate) fn new_rows_text(&self) -> Vec<(String, String, &'static str)> {
        self.rows
            .iter()
            .filter(|row| matches!(row.item, Some(Item::NewAction(_))))
            .map(|row| (row.title.clone(), row.detail.clone(), row.icon))
            .collect()
    }

    /// The row the keys act on, for scripts and tests.
    #[cfg(test)]
    pub(crate) fn selected_row(&self) -> usize {
        self.selected
    }

    /// The tool window the selected row opens, for a row of a window or of a command
    /// that opens one; `None` for every other row.
    #[cfg(test)]
    pub(crate) fn selected_tool(&self) -> Option<&'static str> {
        match self.rows.get(self.selected)?.item.as_ref()? {
            Item::Tool(id) => Some(*id),
            Item::Action(id) => TOOL_COMMANDS
                .iter()
                .find(|(_, command)| command == id)
                .map(|(tool, _)| *tool),
            _ => None,
        }
    }

    /// Whether every source has answered for the query on screen, for a test that reads
    /// the rows: a late group would move them.
    #[cfg(test)]
    pub(crate) fn replies_settled(&self) -> bool {
        self.pending_replies == 0
    }

    /// A late answer of the class source for the query on screen, for the test of a list
    /// that grows after a key moved the selection.
    #[cfg(test)]
    pub(crate) fn apply_late_symbols(&mut self, rows: Vec<SymbolRow>, cx: &mut Context<Self>) {
        let serial = self.serial;
        self.symbols_arrived((0, usize::MAX), serial, rows, cx);
    }

    /// The keys that toggle the checkbox of Recent Files: Ctrl+E there does not reopen
    /// the list it is already in.
    fn toggle_edited(&mut self, cx: &mut Context<Self>) {
        if !matches!(
            self.mode,
            Mode::Recent | Mode::RecentChanged | Mode::Switcher
        ) {
            return;
        }
        self.edited_only = !self.edited_only;
        self.rebuild(cx);
    }

    /// Whether this mode has a preview: the lists of files and symbols that belong to
    /// the project. The commands, the settings and the tool windows have no file.
    fn preview_applies(&self) -> bool {
        matches!(
            self.mode,
            Mode::Everywhere(_) | Mode::Recent | Mode::RecentChanged
        )
    }

    /// The file of the selected row: the file itself, or the one its symbol stands in.
    fn selected_target(&self) -> Option<PreviewTarget> {
        match self.rows.get(self.selected)?.item.as_ref()? {
            Item::File { path, spot } => Some(match *spot {
                Some(spot) => PreviewTarget::at(path.clone(), spot),
                None => PreviewTarget::file(path.clone()),
            }),
            Item::Symbol(row) => match &row.target {
                Target::Offset { path, offset } => {
                    Some(PreviewTarget::at(path.clone(), Spot::Offset(*offset)))
                }
                Target::Line { path, line, column } => Some(PreviewTarget::at(
                    path.clone(),
                    Spot::Line {
                        line: *line,
                        column: *column,
                    },
                )),
                Target::Uri(_) => None,
            },
            Item::Declaration { offset, .. } => {
                let (path, _) = self.structure.as_ref()?;
                Some(PreviewTarget::at(path.clone(), Spot::Offset(*offset)))
            }
            _ => None,
        }
    }

    /// Shows the file of the selected row in the preview, or nothing when the Preview
    /// button is off or the row has no file.
    fn show_selected_preview(&mut self, cx: &mut Context<Self>) {
        let target = (self.preview_open && self.preview_applies())
            .then(|| self.selected_target())
            .flatten();
        self.preview
            .update(cx, |preview, cx| preview.show(target, cx));
    }

    /// The Preview button of the header: the preview right of the list appears or goes
    /// away, and the state folder remembers the choice.
    fn toggle_preview(&mut self, cx: &mut Context<Self>) {
        self.preview_open = !self.preview_open;
        preview::save_palette(
            self.store.as_deref(),
            self.project.root.as_path(),
            self.preview_open,
        );
        self.show_selected_preview(cx);
        cx.notify();
    }

    /// The preview right of the list, for a test.
    #[cfg(test)]
    pub(crate) fn preview(&self) -> &Entity<FilePreview> {
        &self.preview
    }

    /// Whether the Preview button is on, for a test.
    #[cfg(test)]
    pub(crate) fn preview_open(&self) -> bool {
        self.preview_open
    }

    /// Tab, Shift+Tab and the arrow keys: the Switcher walks its list, the other lists
    /// of Search Everywhere keep Tab for their tabs.
    fn iterate(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.mode == Mode::Switcher {
            self.move_in_column(delta, true, cx);
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
            self.move_in_column(delta, true, cx);
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
        self.navigated = true;
        self.scroll_to_selected();
        self.show_selected_preview(cx);
        cx.notify();
    }

    /// Whether the selected row is a tool window, in the Switcher's left column.
    fn selected_is_tool(&self) -> bool {
        self.rows.get(self.selected).is_some_and(is_tool)
    }

    /// The rows of one column of the Switcher, the tool windows or the files, in order. Only
    /// the Switcher has columns; the other modes never ask for one.
    fn column_of(&self, tool: bool) -> Vec<usize> {
        self.rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.item.is_some() && is_tool(row) == tool)
            .map(|(index, _)| index)
            .collect()
    }

    /// Selects the row `index` and brings it on screen.
    fn select_index(&mut self, index: usize, cx: &mut Context<Self>) {
        self.selected = index;
        self.navigated = true;
        self.scroll_to_selected();
        self.show_selected_preview(cx);
        cx.notify();
    }

    /// Scrolls the list that holds the selected row so that the row is on screen. The
    /// Switcher has two lists, and each counts only its own rows.
    fn scroll_to_selected(&self) {
        if self.mode != Mode::Switcher {
            self.scroll.scroll_to_item(self.selected);
            return;
        }
        let tool = self.selected_is_tool();
        let at = self
            .rows
            .iter()
            .take(self.selected)
            .filter(|row| is_tool(row) == tool)
            .count();
        if tool {
            self.tool_scroll.scroll_to_item(at);
        } else {
            self.scroll.scroll_to_item(at);
        }
    }

    /// The Switcher's Tab, Shift+Tab and PgDn walk the column of the selected row and wrap
    /// from its end to its start; the arrow keys stop at either end.
    fn move_in_column(&mut self, delta: isize, wrap: bool, cx: &mut Context<Self>) {
        let column = self.column_of(self.selected_is_tool());
        let Some(at) = column.iter().position(|index| *index == self.selected) else {
            return;
        };
        let count = column.len() as isize;
        let next = at as isize + delta;
        let target = if wrap {
            next.rem_euclid(count)
        } else if (0..count).contains(&next) {
            next
        } else {
            return;
        };
        self.select_index(column[target as usize], cx);
    }

    /// Left goes to the tool windows and Right to the files of the Switcher. The selection
    /// keeps its place in the column, or takes the last row of a shorter column.
    fn switch_column(&mut self, to_tools: bool, cx: &mut Context<Self>) {
        if self.mode != Mode::Switcher || self.selected_is_tool() == to_tools {
            return;
        }
        let from = self.column_of(self.selected_is_tool());
        let Some(at) = from.iter().position(|index| *index == self.selected) else {
            return;
        };
        let target = self.column_of(to_tools);
        let Some(last) = target.len().checked_sub(1) else {
            return;
        };
        self.select_index(target[at.min(last)], cx);
    }

    /// Ctrl released: the Switcher opens what its list points at.
    fn ctrl_released(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.mode == Mode::Switcher {
            self.open(self.selected, false, window, cx);
        }
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = &event.keystroke;
        if key.key == "escape" {
            cx.stop_propagation();
            self.dismiss(window, cx);
            return;
        }
        // The Switcher's columns: Left goes to the tool windows and Right to the files, also
        // with Ctrl held. The keys do not reach the query, so its caret stays where it is.
        if self.mode == Mode::Switcher
            && !key.modifiers.alt
            && matches!(key.key.as_str(), "left" | "right")
        {
            cx.stop_propagation();
            self.switch_column(key.key == "left", cx);
            self.ask_for_frame(window);
            return;
        }
        if key.key == "right"
            && !key.modifiers.control
            && !key.modifiers.alt
            && self.toggle_selected_setting(cx)
        {
            cx.stop_propagation();
            self.ask_for_frame(window);
            return;
        }
        if !self.query.key_with_clipboard(event, false, cx) {
            return;
        }
        cx.stop_propagation();
        self.refresh(cx);
        self.ask_for_frame(window);
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.mode == Mode::Switcher {
            self.move_in_column(delta, false, cx);
            return;
        }
        let mut at = self.selected as isize;
        loop {
            at += delta;
            match self.rows.get(at as usize).filter(|_| at >= 0) {
                Some(row) if row.item.is_some() => {
                    self.selected = at as usize;
                    self.navigated = true;
                    break;
                }
                Some(_) => continue,
                None => break,
            }
        }
        self.scroll_to_selected();
        self.show_selected_preview(cx);
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
        let new_action = match &item {
            Item::NewAction(at) => self.new_actions.get(*at).cloned(),
            _ => None,
        };
        let project_panel = cx
            .try_global::<ActiveProjectPanel>()
            .map(|panel| panel.0.clone());
        let handle = window.window_handle();
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
                    Item::Declaration { offset, .. } => {
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
                    Item::NewAction(_) => {
                        // The window of the row opens after the popup has closed, as the
                        // row of the menu opens it: the prompt cannot be shown while the
                        // window is being updated here.
                        if let (Some(action), Some(panel)) = (new_action, project_panel) {
                            cx.defer(move |cx| {
                                let run = move |_: AnyView, window: &mut Window, cx: &mut App| {
                                    panel.update(cx, |panel, cx| {
                                        panel.ask_new_action(action, window, cx)
                                    });
                                };
                                handle.update(cx, run).ok();
                            });
                        }
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
        // The window draws the file without waiting for the pointer to move.
        self.ask_for_frame(window);
    }

    fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace
            .update(cx, |workspace, cx| workspace.dismiss(window, cx))
            .ok();
        self.ask_for_frame(window);
    }

    /// Asks the window for the frame that shows the change. The popup is drawn in a
    /// deferred pass of the window, and its own notify alone does not always wake the
    /// frame source of the platform: typing then waits for the next mouse move.
    fn ask_for_frame(&mut self, window: &mut Window) {
        #[cfg(test)]
        {
            self.frames_asked += 1;
        }
        window.refresh();
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
        let badge = match &data.item {
            Some(Item::Declaration { visibility, .. }) => {
                crate::outline::visibility_icon(*visibility)
            }
            _ => "",
        };
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
            // The path or the key stands at the right edge; a long path shrinks and is cut with
            // an ellipsis instead of running past the row.
            _ => div()
                .debug_selector(move || format!("palette-detail-{index}"))
                .min_w_0()
                .truncate()
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
                        this.child(
                            div()
                                .debug_selector(move || format!("palette-icon-{index}"))
                                .flex_none()
                                .child(icon(data.icon, theme)),
                        )
                    })
                    // The mark of the visibility stands beside the icon of the kind.
                    .when(!badge.is_empty(), |this| {
                        this.child(
                            div()
                                .debug_selector(move || format!("palette-badge-{index}"))
                                .flex_none()
                                .child(icon(badge, theme)),
                        )
                    })
                    // A command without an icon keeps the place of one, as a menu row does, so
                    // that the names of the list stand in one column.
                    .when(
                        data.icon.is_empty() && matches!(data.item, Some(Item::Action(_))),
                        |this| this.child(div().size(px(ICON)).flex_none()),
                    )
                    // The name and its signature stand together, as IDEA draws them: the gap of
                    // the row separates the icon from the name, and no gap comes after the name.
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .child(
                                div()
                                    .debug_selector(move || format!("palette-title-{index}"))
                                    .child(title_text(&data.title, &data.matched, theme)),
                            )
                            .children(signature.map(|signature| {
                                div()
                                    .debug_selector(move || format!("palette-signature-{index}"))
                                    .text_color(theme.text_secondary)
                                    .child(signature)
                            })),
                    ),
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
        Target::Uri(uri) => workspace.notify_error("Cannot open", uri, cx),
    }
}

impl Render for Palette {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let viewport = window.viewport_size();
        // The Switcher is as tall as its taller column, up to HEIGHT. It has a key of its own,
        // because the window keeps the size of each key and Search Everywhere keeps a height.
        let switcher = self.mode == Mode::Switcher;
        let key: &'static str = if switcher { "switcher" } else { "palette" };
        let mut top = f32::from(viewport.height) * 0.205;
        if switcher {
            // Its height is only known after the layout and may reach HEIGHT, so on a short
            // window it starts as high as a window of HEIGHT would, to stay on screen.
            top = top.min(f32::from(viewport.height) - HEIGHT).max(0.);
        }
        let first = DialogPlace {
            left: (f32::from(viewport.width) - WIDTH) / 2.,
            top,
            width: WIDTH,
            height: (!switcher).then_some(HEIGHT),
        };
        let query = self.query.clone();
        // The Switcher shows its query line only once something is typed.
        let show_input = !switcher || !query.text.is_empty();
        let collapsed =
            self.rows.is_empty() && query.text.is_empty() && self.mode != Mode::Structure;
        let empty = self.rows.is_empty();
        // The tool windows of the Switcher stand in their own column; the rest share one list.
        let mut tool_column = Vec::new();
        let mut rows = Vec::new();
        for index in 0..self.rows.len() {
            let row = self.row(index, &theme, cx);
            if switcher && is_tool(&self.rows[index]) {
                tool_column.push(row);
            } else {
                rows.push(row);
            }
        }
        let active = self.tab();
        let active_name = TABS
            .iter()
            .find(|(tab, _)| *tab == active)
            .map_or("All", |(_, name)| *name);
        let heading = match self.mode {
            Mode::Recent => Some("Recent Files"),
            Mode::RecentChanged => Some("Recently Changed Files"),
            Mode::Switcher => Some("Switcher"),
            Mode::Structure => Some("File Structure"),
            Mode::RunAnything => Some("Run Anything"),
            Mode::ChooseRun { debug: false } => Some("Choose Run Configuration"),
            Mode::ChooseRun { debug: true } => Some("Choose Debug Configuration"),
            Mode::Everywhere(_) => None,
        };
        let preview_button = (self.preview_applies() && !collapsed).then(|| {
            let open = self.preview_open;
            icon_button("palette-preview", "general/show", "Preview", None, &theme)
                .debug_selector(|| "palette-preview".to_owned())
                .when(open, |this| this.bg(theme.selection))
                // A press on the button is not the start of a drag of the header.
                .on_mouse_down(
                    MouseButton::Left,
                    |_: &MouseDownEvent, _: &mut Window, cx: &mut App| {
                        cx.stop_propagation();
                    },
                )
                .on_click(cx.listener(|this, _, _, cx| this.toggle_preview(cx)))
        });
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
                    // A press on a chip is not the start of a drag of the header.
                    .on_mouse_down(
                        MouseButton::Left,
                        |_: &MouseDownEvent, _: &mut Window, cx: &mut App| {
                            cx.stop_propagation();
                        },
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.set_tab(tab, cx)))
                    .child(name)
            }));

        let header = div()
            .flex_none()
            .h(px(42.))
            .debug_selector(move || format!("palette-tab-{active_name}"))
            .child(popup_drag_header(
                cx,
                key,
                (MIN_WIDTH, MIN_HEIGHT),
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .h_full()
                    .px(px(SPACE_XS))
                    .debug_selector(|| "palette-header".to_owned())
                    .child(match heading {
                        Some(title) => div().px(px(ROW_PADDING_X)).child(title).into_any_element(),
                        None => chips.into_any_element(),
                    })
                    .children(preview_button),
            ));

        let body =
            surface(&theme)
                .id("palette")
                .debug_selector(|| "palette".to_owned())
                .key_context(CONTEXT)
                .track_focus(&self.focus)
                .occlude()
                .p(px(POPUP_PADDING))
                .on_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)))
                .on_action(cx.listener(|this, _: &Previous, window, cx| {
                    this.move_selection(-1, cx);
                    this.ask_for_frame(window);
                }))
                .on_action(cx.listener(|this, _: &Next, window, cx| {
                    this.move_selection(1, cx);
                    this.ask_for_frame(window);
                }))
                .on_action(cx.listener(|this, _: &NextTab, window, cx| {
                    this.iterate(1, cx);
                    this.ask_for_frame(window);
                }))
                .on_action(cx.listener(|this, _: &PreviousTab, window, cx| {
                    this.iterate(-1, cx);
                    this.ask_for_frame(window);
                }))
                .on_action(cx.listener(|this, _: &NextGroup, window, cx| {
                    this.next_group(1, cx);
                    this.ask_for_frame(window);
                }))
                .on_action(cx.listener(|this, _: &PreviousGroup, window, cx| {
                    this.next_group(-1, cx);
                    this.ask_for_frame(window);
                }))
                .on_action(cx.listener(|this, _: &ToggleEdited, window, cx| {
                    this.toggle_edited(cx);
                    this.ask_for_frame(window);
                }))
                .on_modifiers_changed(cx.listener(
                    |this, event: &ModifiersChangedEvent, window, cx| {
                        if !event.modifiers.control {
                            this.ctrl_released(window, cx);
                        }
                    },
                ))
                .on_action(cx.listener(|this, _: &Open, window, cx| {
                    this.open(this.selected, false, window, cx)
                }))
                .on_action(cx.listener(|this, _: &OpenDebug, window, cx| {
                    this.open(this.selected, true, window, cx)
                }))
                .on_mouse_down_out(cx.listener(|this, _, window, cx| this.dismiss(window, cx)))
                .child(header)
                .when(show_input, |this| {
                    this.child(
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
                                        FieldMouse::new(cx, |this| Some(&mut this.query)),
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
                })
                .when(
                    matches!(
                        self.mode,
                        Mode::Recent | Mode::RecentChanged | Mode::Switcher
                    ),
                    |this| {
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
                    },
                )
                .when(!collapsed, |this| {
                    // Two lists side by side, each with its own scroll and bar: the tool
                    // windows on the left, the files on the right. The popup is as tall as
                    // the taller of them.
                    if switcher {
                        let tools = scrollable(
                            "palette-tools",
                            &self.tool_scroll,
                            div()
                                .id("palette-tools")
                                .flex()
                                .flex_col()
                                .overflow_y_scroll()
                                .track_scroll(&self.tool_scroll)
                                .children(tool_column),
                            &theme,
                            window,
                            cx,
                        )
                        .flex_1()
                        .min_h_0();
                        let files = scrollable(
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
                        .min_h_0();
                        return this.child(
                            div()
                                .flex()
                                .flex_1()
                                .min_h_0()
                                .w_full()
                                .child(
                                    div()
                                        .debug_selector(|| "palette-tools-column".to_owned())
                                        .flex()
                                        .flex_col()
                                        .flex_none()
                                        .w(px(SWITCHER_TOOLS_WIDTH))
                                        .min_h_0()
                                        .border_r_1()
                                        .border_color(theme.border)
                                        .child(tools),
                                )
                                .child(
                                    div()
                                        .debug_selector(|| "palette-files-column".to_owned())
                                        .flex()
                                        .flex_col()
                                        .flex_1()
                                        .min_w_0()
                                        .min_h_0()
                                        .child(files),
                                ),
                        );
                    }
                    let list = scrollable(
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
                    .min_h_0();
                    if self.preview_open && self.preview_applies() {
                        this.child(
                            div().flex().flex_1().min_h_0().w_full().child(list).child(
                                div()
                                    .id("palette-preview-box")
                                    .debug_selector(|| "palette-preview-box".to_owned())
                                    .flex_none()
                                    .w(px(PREVIEW_WIDTH))
                                    .h_full()
                                    .overflow_hidden()
                                    .border_l_1()
                                    .border_color(theme.border)
                                    .child(self.preview.clone()),
                            ),
                        )
                    } else {
                        this.child(list)
                    }
                });

        popup_window(
            PopupSpec {
                key,
                place: first,
                min: (MIN_WIDTH, MIN_HEIGHT),
                content_height: switcher,
                max_height: switcher.then_some(HEIGHT),
            },
            body,
            window,
            cx,
        )
    }
}
