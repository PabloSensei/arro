//! The saved layout of one project. Stored as JSON in the state database.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use store::Store;
use ui_kit::metrics::*;

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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Layout {
    pub window: Option<WindowPlacement>,
    pub left: DockState,
    pub right: DockState,
    pub bottom: DockState,
    pub panes: Vec<PaneState>,
    pub active_pane: usize,
    /// Groups of tabs sit one under another instead of side by side.
    pub split_down: bool,
    pub tree_expanded: Vec<PathBuf>,
    pub tree_selected: Option<PathBuf>,
    /// The tool window the bottom dock showed.
    pub bottom_tool: Option<String>,
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
            tree_expanded: Vec::new(),
            tree_selected: None,
            bottom_tool: None,
        }
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

pub fn load_dark_theme(store: &Store) -> bool {
    store.get(THEME_KEY).ok().flatten().as_deref() != Some(LIGHT)
}

pub fn save_dark_theme(store: &Store, dark: bool) -> Result<(), store::Error> {
    store.put(THEME_KEY, if dark { DARK } else { LIGHT })
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
        layout.tree_expanded = vec![PathBuf::from("C:/project/src")];
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
    fn theme_defaults_to_dark() {
        let store = Store::in_memory().unwrap();
        assert!(load_dark_theme(&store));
        save_dark_theme(&store, false).unwrap();
        assert!(!load_dark_theme(&store));
    }
}
