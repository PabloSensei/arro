//! The saved layout of one project. Stored as JSON in the state database.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use store::Store;
use ui_kit::metrics::*;

use crate::editor::BundleLayout;
use crate::editor::PreviewLayout;
use crate::split::SplitLayout;
use crate::window::DockSide;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct DockState {
    pub open: bool,
    pub size: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct WindowPlacement {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub maximized: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PaneState {
    pub tabs: Vec<PathBuf>,
    pub active: usize,
    /// Line and column of the caret of each tab, in the order of `tabs`.
    pub carets: Vec<(usize, usize)>,
    /// Tabs that stay when the others are closed.
    pub pinned: Vec<PathBuf>,
}

/// One bookmark of a saved project: the line it points at and what the user wrote.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BookmarkState {
    pub path: PathBuf,
    /// Buffer line from zero.
    pub line: u32,
    pub mnemonic: Option<char>,
    pub description: Option<String>,
}

/// A group of saved bookmarks, such as the one named after the project.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BookmarkGroupState {
    pub name: String,
    pub bookmarks: Vec<BookmarkState>,
}

/// A line of a file the user was at, with the text it had then.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Location {
    pub path: PathBuf,
    /// Buffer line from zero.
    pub line: usize,
    /// The text of the line when the place was remembered. It is not saved: the window
    /// reads the line again from the file when it shows the list.
    #[serde(skip)]
    pub text: String,
}

/// The places of Recent Locations, the latest first.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LocationLog(Vec<Location>);

impl LocationLog {
    /// A place on a line that is already in the log moves to the front.
    pub fn push(&mut self, place: Location) {
        self.0
            .retain(|known| !(known.path == place.path && known.line == place.line));
        self.0.insert(0, place);
        self.0.truncate(LOG_MAX);
    }

    pub fn places(&self) -> &[Location] {
        &self.0
    }

    /// Drops the places the caller does not keep.
    pub fn retain(&mut self, keep: impl FnMut(&Location) -> bool) {
        self.0.retain(keep);
    }
}

/// Places the log keeps.
const LOG_MAX: usize = 50;

/// The bookmarks of a project. A layout saved before the groups existed holds the plain
/// list of the old store, or the map the store itself kept: either opens as one group
/// named after the project.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum BookmarkLayout {
    Grouped {
        groups: Vec<BookmarkGroupState>,
        default: usize,
    },
    Flat(Vec<BookmarkState>),
    /// The map of files the store of the earlier build held: the lines of each file and
    /// the mnemonic of a line.
    ByFile(std::collections::HashMap<PathBuf, Vec<(u32, Option<char>)>>),
}

impl Default for BookmarkLayout {
    fn default() -> Self {
        Self::Grouped {
            groups: Vec::new(),
            default: 0,
        }
    }
}

impl BookmarkLayout {
    pub fn grouped(groups: Vec<BookmarkGroupState>, default: usize) -> Self {
        Self::Grouped { groups, default }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Layout {
    pub window: Option<WindowPlacement>,
    pub left: DockState,
    pub right: DockState,
    pub bottom: DockState,
    pub panes: Vec<PaneState>,
    pub active_pane: usize,
    /// Groups of tabs sit one under another instead of side by side. A layout without
    /// `splits` opens as one row or one column of them, which is what this meant before
    /// the groups could nest.
    pub split_down: bool,
    /// The shape of the groups, nested as the splits were. `None` in a layout saved
    /// before the groups nested: `panes` then sit in one row or one column.
    pub splits: Option<SplitLayout>,
    pub tree_expanded: Vec<PathBuf>,
    pub tree_selected: Option<PathBuf>,
    /// The tool window the bottom dock showed.
    pub bottom_tool: Option<String>,
    /// The dock of each tool window. A window without a row opens where its kind puts
    /// it; Move to writes the row, and the next opening of the project follows it.
    pub tool_sides: Vec<(String, DockSide)>,
    /// Files that were edited in the project, the latest first. Opening a file alone
    /// does not put it here; saving does not take it away.
    pub recent_edited: Vec<PathBuf>,
    /// Files that were active in the editors, the latest first, as the editors kept them
    /// at the last save. Recent Files and the Switcher start from this list when the
    /// project opens; files that are gone are left out then.
    pub recent_files: Vec<PathBuf>,
    /// What each file with a rendered document showed: the text, the document, or both.
    pub preview_modes: Vec<(PathBuf, PreviewLayout)>,
    /// What each file of a localized set showed: its text or the table of the set.
    pub bundle_modes: Vec<(PathBuf, BundleLayout)>,
    /// The groups of the Services tree that are collapsed, by their key.
    pub services_collapsed: Vec<String>,
    /// The node selected in the Services tree, by its key.
    pub services_selected: Option<String>,
    /// The optional groups of the Services tree list their configurations.
    pub services_show_all: bool,
    /// The groups of the bookmarks with the description of each one.
    pub bookmarks: BookmarkLayout,
    /// The places of Recent Locations, the latest first.
    pub recent_places: LocationLog,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            window: None,
            left: DockState {
                open: true,
                size: LEFT_PANEL_WIDTH,
            },
            right: DockState {
                open: false,
                size: RIGHT_PANEL_WIDTH,
            },
            bottom: DockState {
                open: false,
                size: BOTTOM_PANEL_HEIGHT,
            },
            panes: Vec::new(),
            active_pane: 0,
            split_down: false,
            splits: None,
            tree_expanded: Vec::new(),
            tree_selected: None,
            bottom_tool: None,
            tool_sides: Vec::new(),
            recent_edited: Vec::new(),
            recent_files: Vec::new(),
            preview_modes: Vec::new(),
            bundle_modes: Vec::new(),
            services_collapsed: Vec::new(),
            services_selected: None,
            services_show_all: false,
            bookmarks: BookmarkLayout::default(),
            recent_places: LocationLog::default(),
        }
    }
}

impl Layout {
    /// The dock the project kept for a tool window, if a Move to ever chose one.
    pub fn tool_side(&self, id: &str) -> Option<DockSide> {
        self.tool_sides
            .iter()
            .find(|(tool, _)| tool == id)
            .map(|(_, side)| *side)
    }

    /// Remembers the dock of a tool window.
    pub fn set_tool_side(&mut self, id: &str, side: DockSide) {
        self.tool_sides.retain(|(tool, _)| tool != id);
        self.tool_sides.push((id.to_owned(), side));
    }
}

const THEME_KEY: &str = "ui.theme";
const RECENT_KEY: &str = "recent.projects";
const RECENT_LIMIT: usize = 10;
const LIGHT: &str = "light";
const DARK: &str = "dark";

fn layout_key(root: &Path) -> String {
    format!("layout:{}", root.display())
}

/// A damaged or older value is not an error: the project opens with the default layout.
pub fn load_layout(store: &Store, root: &Path) -> Layout {
    let saved = store.get(&layout_key(root)).ok().flatten();
    saved
        .and_then(|json| serde_json::from_str(&json).ok())
        .unwrap_or_default()
}

/// Whether this project was opened before: it has a saved layout, however damaged it is.
/// A project without one opens for the first time.
pub fn has_saved_layout(store: &Store, root: &Path) -> bool {
    store.get(&layout_key(root)).ok().flatten().is_some()
}

pub fn save_layout(store: &Store, root: &Path, json: &str) -> Result<(), store::Error> {
    store.put(&layout_key(root), json)
}

/// Projects opened before, the latest first. A damaged value is an empty list.
pub fn load_recent(store: &Store) -> Vec<PathBuf> {
    let saved = store.get(RECENT_KEY).ok().flatten();
    saved
        .and_then(|json| serde_json::from_str(&json).ok())
        .unwrap_or_default()
}

/// `root` goes to the front of `recent`, once, and the list stays short.
fn with_recent(mut recent: Vec<PathBuf>, root: &Path) -> Vec<PathBuf> {
    recent.retain(|path| path != root);
    recent.insert(0, root.to_path_buf());
    recent.truncate(RECENT_LIMIT);
    recent
}

pub fn remember_project(store: &Store, root: &Path) -> Result<(), store::Error> {
    let recent = with_recent(load_recent(store), root);
    let json = serde_json::to_string(&recent).unwrap_or_default();
    store.put(RECENT_KEY, &json)
}

/// File > Open Recent > Clear List.
pub fn clear_recent(store: &Store) -> Result<(), store::Error> {
    store.put(RECENT_KEY, "[]")
}

/// The theme of the window as the state database holds it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SavedTheme {
    pub dark: bool,
    /// The user chose a theme at least once. A window that never chose one follows the
    /// system while Sync with OS is on.
    pub chosen: bool,
}

impl Default for SavedTheme {
    fn default() -> Self {
        Self {
            dark: true,
            chosen: false,
        }
    }
}

/// A missing or damaged value is the default theme that no one chose yet. A saved name
/// this build does not know keeps its look of the base dark theme.
pub fn load_theme(store: &Store) -> SavedTheme {
    match store.get(THEME_KEY).ok().flatten().as_deref() {
        Some(LIGHT) => SavedTheme {
            dark: false,
            chosen: true,
        },
        Some(DARK) => SavedTheme {
            dark: true,
            chosen: true,
        },
        Some(name) => SavedTheme {
            dark: ui_kit::Theme::named(name).dark,
            chosen: true,
        },
        None => SavedTheme::default(),
    }
}

pub fn save_dark_theme(store: &Store, dark: bool) -> Result<(), store::Error> {
    store.put(THEME_KEY, if dark { DARK } else { LIGHT })
}

/// Remembers the chosen theme by its id, so that a theme of the settings folder opens
/// again after a restart. The look of an id this build does not know stays dark.
pub fn save_theme(store: &Store, id: &str, dark: bool) -> Result<(), store::Error> {
    if id.is_empty() {
        return save_dark_theme(store, dark);
    }
    store.put(THEME_KEY, id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_round_trips_through_the_store() {
        let store = Store::in_memory().unwrap();
        let root = Path::new("C:/project");
        let mut layout = load_layout(&store, root);
        assert_eq!(layout, Layout::default());

        layout.left.size = 300.;
        layout.bottom.open = true;
        layout.panes = vec![PaneState {
            tabs: vec![PathBuf::from("C:/project/pom.xml")],
            active: 0,
            carets: vec![(12, 4)],
            pinned: vec![PathBuf::from("C:/project/pom.xml")],
        }];
        layout.split_down = true;
        layout.splits = Some(SplitLayout::Split {
            down: false,
            ratio: 0.3,
            first: Box::new(SplitLayout::Group),
            second: Box::new(SplitLayout::Group),
        });
        layout.tree_expanded = vec![PathBuf::from("C:/project/src")];
        let json = serde_json::to_string(&layout).unwrap();
        save_layout(&store, root, &json).unwrap();
        assert_eq!(load_layout(&store, root), layout);
    }

    #[test]
    fn a_layout_saved_before_the_groups_nested_opens_as_one_row() {
        let store = Store::in_memory().unwrap();
        let root = Path::new("C:/project");
        let old = r#"{"panes":[{"tabs":["C:/project/a.txt"],"active":0},
            {"tabs":["C:/project/b.txt"],"active":0}],"split_down":true}"#;
        save_layout(&store, root, old).unwrap();
        let layout = load_layout(&store, root);
        assert_eq!(layout.panes.len(), 2);
        assert!(layout.split_down);
        assert_eq!(layout.splits, None, "the old file has no tree");
    }

    #[test]
    fn the_services_tree_remembers_its_groups_and_its_node() {
        let store = Store::in_memory().unwrap();
        let root = Path::new("C:/project");
        let mut layout = load_layout(&store, root);
        layout.services_collapsed = vec!["run:boot".to_owned()];
        layout.services_selected = Some("run:App".to_owned());
        layout.services_show_all = true;

        let json = serde_json::to_string(&layout).unwrap();
        save_layout(&store, root, &json).unwrap();

        assert_eq!(load_layout(&store, root), layout);
    }

    #[test]
    fn damaged_or_partial_values_fall_back_to_defaults() {
        let store = Store::in_memory().unwrap();
        let root = Path::new("C:/project");
        save_layout(&store, root, "{broken").unwrap();
        assert_eq!(load_layout(&store, root), Layout::default());

        save_layout(&store, root, r#"{"active_pane":1,"future_key":true}"#).unwrap();
        let layout = load_layout(&store, root);
        assert_eq!(layout.active_pane, 1);
        assert_eq!(layout.left, Layout::default().left);
    }

    #[test]
    fn a_layout_saved_before_carets_and_pins_still_opens() {
        let store = Store::in_memory().unwrap();
        let root = Path::new("C:/project");
        let old = r#"{"panes":[{"tabs":["C:/project/a.txt"],"active":0}]}"#;
        save_layout(&store, root, old).unwrap();
        let pane = &load_layout(&store, root).panes[0];
        assert_eq!(pane.tabs.len(), 1);
        assert!(pane.carets.is_empty() && pane.pinned.is_empty());
    }

    #[test]
    fn recent_projects_keep_the_latest_first_without_repeats() {
        let store = Store::in_memory().unwrap();
        assert!(load_recent(&store).is_empty());
        for name in ["a", "b", "a", "c"] {
            remember_project(&store, Path::new(name)).unwrap();
        }
        let recent = load_recent(&store);
        assert_eq!(recent, [PathBuf::from("c"), "a".into(), "b".into()]);

        for index in 0..20 {
            remember_project(&store, Path::new(&format!("p{index}"))).unwrap();
        }
        assert_eq!(load_recent(&store).len(), RECENT_LIMIT);
        assert_eq!(load_recent(&store)[0], PathBuf::from("p19"));
    }

    #[test]
    fn recent_files_are_saved_with_the_layout() {
        let store = Store::in_memory().unwrap();
        let root = Path::new("C:/project");
        let mut layout = load_layout(&store, root);
        assert!(layout.recent_files.is_empty());
        layout.recent_files = vec![
            PathBuf::from("C:/project/b.txt"),
            PathBuf::from("C:/project/a.txt"),
        ];
        let json = serde_json::to_string(&layout).unwrap();
        save_layout(&store, root, &json).unwrap();
        assert_eq!(load_layout(&store, root).recent_files, layout.recent_files);
    }

    #[test]
    fn theme_defaults_to_dark() {
        let store = Store::in_memory().unwrap();
        assert_eq!(load_theme(&store), SavedTheme::default());
        assert!(!load_theme(&store).chosen, "no theme was chosen yet");
        save_dark_theme(&store, false).unwrap();
        assert_eq!(
            load_theme(&store),
            SavedTheme {
                dark: false,
                chosen: true
            }
        );
    }
}
