//! Editor panes and their tabs. Pure bookkeeping, rendering lives in `chrome`.

use std::path::{Path, PathBuf};

use ui_kit::*;

use crate::editor::EditorTab;

/// Closed tabs the window remembers for Ctrl+Shift+T.
const CLOSED_LIMIT: usize = 20;

pub struct Tab {
    pub path: PathBuf,
    pub title: SharedString,
    pub icon: &'static str,
    pub editor: EditorTab,
    pub pinned: bool,
}

#[derive(Default)]
pub struct Pane {
    pub tabs: Vec<Tab>,
    pub active: usize,
    pub scroll: ScrollHandle,
    /// The bar of this group, when the window has built it.
    pub bar: Option<Entity<crate::chrome::TabBarView>>,
}

impl Pane {
    pub fn active_tab(&self) -> Option<&Tab> {
        self.tabs.get(self.active)
    }

    pub fn position(&self, path: &Path) -> Option<usize> {
        self.tabs.iter().position(|tab| tab.path == path)
    }

    /// Removes a tab and keeps the selection on the tab that was next to it.
    pub fn remove(&mut self, index: usize) -> Tab {
        let tab = self.tabs.remove(index);
        if index < self.active {
            self.active -= 1;
        }
        self.active = self.active.min(self.tabs.len().saturating_sub(1));
        tab
    }

    /// Puts a tab in at `index` (clamped by the pins) and makes it the selected one.
    /// Returns where it landed.
    pub fn insert(&mut self, index: usize, tab: Tab) -> usize {
        let pinned: Vec<bool> = self.tabs.iter().map(|tab| tab.pinned).collect();
        let index = clamp_drop(&pinned, tab.pinned, index);
        self.tabs.insert(index, tab);
        self.active = index;
        index
    }

    /// Pins or unpins a tab, which then moves to the end of the pinned ones or the start
    /// of the others. Returns the new index of the tab.
    pub fn set_pinned(&mut self, index: usize, pinned: bool) -> usize {
        self.tabs[index].pinned = pinned;
        let order = pinned_first(&self.tabs.iter().map(|tab| tab.pinned).collect::<Vec<_>>());
        let mut old: Vec<Option<Tab>> = self.tabs.drain(..).map(Some).collect();
        self.tabs = order.iter().filter_map(|&at| old[at].take()).collect();
        let moved = |from: usize| order.iter().position(|&at| at == from).unwrap_or(0);
        self.active = moved(self.active);
        moved(index)
    }

    /// Indexes of tabs that a bulk close may remove: the pinned ones stay.
    pub fn closable(&self, except: Option<usize>) -> Vec<usize> {
        let tabs = self.tabs.iter().enumerate();
        let open = tabs.filter(|(index, tab)| !tab.pinned && Some(*index) != except);
        open.map(|(index, _)| index).collect()
    }
}

/// Which pane keeps the focus after pane `removed` goes away.
pub fn active_after_removal(active: usize, removed: usize, remaining: usize) -> usize {
    let shifted = if removed < active { active - 1 } else { active };
    shifted.min(remaining.saturating_sub(1))
}

/// The tab after `active`, wrapping around.
pub fn next_tab(active: usize, count: usize) -> usize {
    (active + 1) % count.max(1)
}

/// The tab before `active`, wrapping around.
pub fn previous_tab(active: usize, count: usize) -> usize {
    (active + count.max(1) - 1) % count.max(1)
}

/// The order that puts pinned tabs first and keeps the order inside both groups.
pub fn pinned_first(pinned: &[bool]) -> Vec<usize> {
    let (first, rest): (Vec<usize>, Vec<usize>) = (0..pinned.len()).partition(|&at| pinned[at]);
    first.into_iter().chain(rest).collect()
}

/// A tab dropped among others stays on its side of the line between pinned and not pinned.
pub fn clamp_drop(pinned: &[bool], moving_pinned: bool, index: usize) -> usize {
    let line = pinned.iter().filter(|pinned| **pinned).count();
    if moving_pinned {
        index.min(line)
    } else {
        index.clamp(line, pinned.len())
    }
}

/// Remembers a closed file, the latest last, without repeats.
pub fn remember_closed(closed: &mut Vec<PathBuf>, path: PathBuf) {
    closed.retain(|known| *known != path);
    closed.push(path);
    if closed.len() > CLOSED_LIMIT {
        closed.remove(0);
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{
        CLOSED_LIMIT, active_after_removal, clamp_drop, next_tab, pinned_first, previous_tab,
        remember_closed,
    };

    #[test]
    fn removal_keeps_the_neighbour_selected() {
        assert_eq!(active_after_removal(2, 0, 2), 1);
        assert_eq!(active_after_removal(0, 1, 1), 0);
        assert_eq!(active_after_removal(1, 1, 1), 0);
    }

    #[test]
    fn next_tab_wraps() {
        assert_eq!(next_tab(0, 3), 1);
        assert_eq!(next_tab(2, 3), 0);
        assert_eq!(next_tab(0, 0), 0);
    }

    #[test]
    fn previous_tab_wraps() {
        assert_eq!(previous_tab(1, 3), 0);
        assert_eq!(previous_tab(0, 3), 2);
        assert_eq!(previous_tab(0, 0), 0);
    }

    #[test]
    fn pinned_tabs_go_first_in_their_own_order() {
        assert_eq!(pinned_first(&[false, true, false, true]), [1, 3, 0, 2]);
        assert_eq!(pinned_first(&[false, false]), [0, 1]);
        assert!(pinned_first(&[]).is_empty());
    }

    #[test]
    fn a_dropped_tab_stays_on_its_side_of_the_pins() {
        let pinned = [true, true, false, false];
        assert_eq!(
            clamp_drop(&pinned, false, 0),
            2,
            "not pinned: after the pins"
        );
        assert_eq!(clamp_drop(&pinned, false, 9), 4);
        assert_eq!(clamp_drop(&pinned, true, 3), 2, "pinned: before the others");
        assert_eq!(clamp_drop(&pinned, true, 1), 1);
    }

    #[test]
    fn closed_files_are_remembered_once_and_the_list_is_short() {
        let mut closed = Vec::new();
        for name in ["a", "b", "a"] {
            remember_closed(&mut closed, PathBuf::from(name));
        }
        assert_eq!(closed, [PathBuf::from("b"), PathBuf::from("a")]);
        for index in 0..30 {
            remember_closed(&mut closed, PathBuf::from(format!("f{index}")));
        }
        assert_eq!(closed.len(), CLOSED_LIMIT);
        assert_eq!(closed.last(), Some(&PathBuf::from("f29")));
    }
}
