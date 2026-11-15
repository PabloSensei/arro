//! The tabs of the terminal: the names of the sessions, the folders they open in, and
//! what a window that opens again puts back. Nothing here starts a program.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use store::Store;

/// The first tab and the prefix of the others.
const LOCAL: &str = "Local";

/// Where the second pane of a tab sits when the user splits it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Split {
    /// The panes side by side.
    Right,
    /// The panes one under another.
    Down,
}

/// One tab as the layout keeps it. The programs are not a part of it: a window that
/// opens again starts them empty, in the folder the tab had.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TabState {
    pub name: String,
    pub directory: PathBuf,
    /// The command line of the shell; `None` means the shell of the settings.
    #[serde(default)]
    pub shell: Option<String>,
}

/// Every tab of the terminal and the one the window showed.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TabsState {
    pub tabs: Vec<TabState>,
    #[serde(default)]
    pub active: usize,
}

impl TabsState {
    /// The tab the window shows: an index that the saved layout cannot point past.
    pub fn active(&self) -> usize {
        self.active.min(self.tabs.len().saturating_sub(1))
    }

    /// The tabs to open for a state that came back, without the ones that name no
    /// folder: a broken value is not worth, and the window falls back to one tab.
    pub fn restored(&self) -> Vec<TabState> {
        self.tabs
            .iter()
            .filter(|tab| !tab.name.trim().is_empty() && !tab.directory.as_os_str().is_empty())
            .cloned()
            .collect()
    }
}

/// The name of a tab that joins `taken`: `Local`, then `Local (2)`, `Local (3)` and so
/// on, the way the window names its sessions.
pub fn next_name(taken: impl IntoIterator<Item = String>) -> String {
    let taken: Vec<String> = taken.into_iter().collect();
    if !taken.iter().any(|name| name == LOCAL) {
        return LOCAL.to_string();
    }
    (2..)
        .map(|number| format!("{LOCAL} ({number})"))
        .find(|name| !taken.contains(name))
        .unwrap_or_else(|| LOCAL.to_string())
}

fn key(root: &Path) -> String {
    format!("terminal:{}", root.display())
}

/// The tabs of the project that was open last. A damaged or older value is no tabs.
pub fn load(store: &Store, root: &Path) -> TabsState {
    let saved = store.get(&key(root)).ok().flatten();
    saved
        .and_then(|json| serde_json::from_str(&json).ok())
        .unwrap_or_default()
}

pub fn save(store: &Store, root: &Path, state: &TabsState) -> Result<(), store::Error> {
    let json = serde_json::to_string(state).unwrap_or_default();
    store.put(&key(root), &json)
}

#[cfg(test)]
mod tests {
    use super::{Split, TabState, TabsState, load, next_name, save};
    use std::path::PathBuf;
    use store::Store;

    fn tab(name: &str, directory: &str) -> TabState {
        TabState {
            name: name.to_string(),
            directory: PathBuf::from(directory),
            shell: None,
        }
    }

    #[test]
    fn the_names_of_the_new_tabs_follow_the_one_of_the_idea() {
        assert_eq!(next_name([]), "Local");
        assert_eq!(next_name(["Local".to_string()]), "Local (2)");
        assert_eq!(
            next_name(["Local".to_string(), "Local (2)".to_string()]),
            "Local (3)"
        );
        assert_eq!(
            next_name(["Build".to_string(), "Local (2)".to_string()]),
            "Local"
        );
    }

    #[test]
    fn the_tabs_of_a_project_survive_a_restart_in_their_folder() {
        let store = Store::in_memory().unwrap();
        let root = PathBuf::from("C:/project");
        let state = TabsState {
            tabs: vec![
                tab("Local", "C:/project"),
                TabState {
                    name: "Build".to_string(),
                    directory: PathBuf::from("C:/project/tools"),
                    shell: Some("\"C:\\Program Files\\Git\\bin\\bash.exe\"".to_string()),
                },
            ],
            active: 1,
        };

        save(&store, &root, &state).unwrap();

        let restored = load(&store, &root);
        assert_eq!(restored, state);
        assert_eq!(restored.active(), 1);
        assert_eq!(restored.restored()[1].name, "Build");
        assert_eq!(
            restored.restored()[1].directory,
            PathBuf::from("C:/project/tools")
        );
    }

    #[test]
    fn a_damaged_value_opens_one_tab_and_an_index_past_the_end_keeps_the_last() {
        let store = Store::in_memory().unwrap();
        let root = PathBuf::from("C:/project");
        store.put("terminal:C:/project", "{broken").unwrap();
        assert_eq!(load(&store, &root), TabsState::default());

        let state = TabsState {
            tabs: vec![tab("Local", "C:/project"), tab("Run", "C:/project")],
            active: 7,
        };
        assert_eq!(state.active(), 1);

        let empty = TabsState {
            tabs: vec![TabState {
                name: "  ".to_string(),
                directory: PathBuf::new(),
                shell: None,
            }],
            active: 0,
        };
        assert!(empty.restored().is_empty());
    }

    #[test]
    fn a_split_names_the_two_ways_a_tab_can_hold_two_sessions() {
        assert_ne!(Split::Right, Split::Down);
    }
}
