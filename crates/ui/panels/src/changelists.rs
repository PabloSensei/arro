//! The change lists of the Commit window. The active list takes the files no list owns;
//! the choice and the grouping live in the store of the window, one entry per project.

use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};
use store::Store;

pub const DEFAULT_LIST: &str = "Changes";

/// How the Commit window sorts the files of the active list.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Grouping {
    #[default]
    Directory,
    Module,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Changelists {
    pub lists: Vec<String>,
    pub active: String,
    /// The list of a file the user moved there. Files that have no entry belong to the
    /// active list.
    pub assigned: BTreeMap<String, String>,
    pub grouping: Grouping,
}

impl Default for Changelists {
    fn default() -> Self {
        Self {
            lists: vec![DEFAULT_LIST.to_string()],
            active: DEFAULT_LIST.to_string(),
            assigned: BTreeMap::new(),
            grouping: Grouping::default(),
        }
    }
}

impl Changelists {
    pub fn load(store: Option<&Store>, key: &str) -> Self {
        let saved = store
            .and_then(|store| store.get(key).ok().flatten())
            .and_then(|json| serde_json::from_str::<Self>(&json).ok());
        let mut lists = saved.unwrap_or_default();
        lists.repair();
        lists
    }

    pub fn save(&self, store: Option<&Store>, key: &str) {
        let Some(store) = store else {
            return;
        };
        if let Ok(json) = serde_json::to_string(self) {
            let _ = store.put(key, &json);
        }
    }

    /// The single default list comes back if the store held something unusable.
    fn repair(&mut self) {
        if self.lists.is_empty() {
            self.lists.push(DEFAULT_LIST.to_string());
        }
        if !self.lists.contains(&self.active) {
            self.active = self.lists[0].clone();
        }
        self.assigned.retain(|_, list| self.lists.contains(list));
    }

    /// The list the file is shown in. A file without a list belongs to the active one.
    pub fn list_of<'a>(&'a self, path: &str) -> &'a str {
        self.assigned
            .get(path)
            .filter(|list| self.lists.contains(list))
            .map_or(self.active.as_str(), String::as_str)
    }

    pub fn names(&self) -> &[String] {
        &self.lists
    }

    pub fn add_list(&mut self, name: &str) -> bool {
        let name = name.trim();
        if name.is_empty() || self.lists.iter().any(|list| list == name) {
            return false;
        }
        self.lists.push(name.to_string());
        self.active = name.to_string();
        true
    }

    pub fn rename_active(&mut self, name: &str) -> bool {
        let name = name.trim();
        if name.is_empty() || name == self.active || self.lists.iter().any(|list| list == name) {
            return false;
        }
        let old = self.active.clone();
        if let Some(list) = self.lists.iter_mut().find(|list| **list == old) {
            *list = name.to_string();
        }
        for list in self.assigned.values_mut() {
            if *list == old {
                *list = name.to_string();
            }
        }
        self.active = name.to_string();
        true
    }

    /// Removes the active list; its files go back to the list that becomes active.
    pub fn delete_active(&mut self) -> bool {
        if self.lists.len() <= 1 {
            return false;
        }
        let removed = self.active.clone();
        self.lists.retain(|list| *list != removed);
        self.assigned.retain(|_, list| *list != removed);
        self.active = self.lists[0].clone();
        true
    }

    pub fn set_active(&mut self, name: &str) -> bool {
        if !self.lists.iter().any(|list| list == name) {
            return false;
        }
        self.active = name.to_string();
        true
    }

    pub fn move_paths(&mut self, paths: &[String], list: &str) {
        if !self.lists.iter().any(|known| known == list) {
            return;
        }
        for path in paths {
            if list == self.active {
                self.assigned.remove(path);
            } else {
                self.assigned.insert(path.clone(), list.to_string());
            }
        }
    }

    /// A file that has no change leaves its list. Returns whether anything changed.
    pub fn retain_paths(&mut self, paths: &HashSet<String>) -> bool {
        let before = self.assigned.len();
        self.assigned.retain(|path, _| paths.contains(path));
        before != self.assigned.len()
    }
}

#[cfg(test)]
mod tests {
    use super::{Changelists, DEFAULT_LIST};

    fn paths(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    #[test]
    fn a_file_without_a_list_belongs_to_the_active_one() {
        let mut lists = Changelists::default();
        assert_eq!(lists.list_of("a.txt"), DEFAULT_LIST);

        assert!(lists.add_list("Feature"));
        assert_eq!(lists.active, "Feature");
        assert_eq!(lists.list_of("a.txt"), "Feature");

        lists.move_paths(&paths(&["a.txt"]), DEFAULT_LIST);
        assert_eq!(lists.list_of("a.txt"), "Changes");
        assert_eq!(lists.list_of("b.txt"), "Feature");
    }

    #[test]
    fn a_rename_takes_the_files_and_a_delete_gives_them_back() {
        let mut lists = Changelists::default();
        lists.add_list("Feature");
        lists.move_paths(&paths(&["a.txt"]), "Feature");

        assert!(lists.rename_active("Topic"));
        assert_eq!(lists.list_of("a.txt"), "Topic");
        assert!(lists.delete_active());
        assert_eq!(lists.active, DEFAULT_LIST);
        assert_eq!(lists.list_of("a.txt"), DEFAULT_LIST);
        assert!(!lists.delete_active(), "the last list stays");
    }

    #[test]
    fn a_file_without_changes_leaves_its_list() {
        let mut lists = Changelists::default();
        lists.add_list("Feature");
        lists.set_active(DEFAULT_LIST);
        lists.move_paths(&paths(&["a.txt", "b.txt"]), "Feature");

        let changed = ["a.txt".to_string()].into_iter().collect();
        assert!(lists.retain_paths(&changed));
        assert_eq!(lists.list_of("a.txt"), "Feature");
        assert_eq!(lists.list_of("b.txt"), DEFAULT_LIST);
    }
}
