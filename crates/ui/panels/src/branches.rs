//! The branch popup of the status bar and the title bar, as the new UI of IDEA shows it:
//! a search field with Fetch and Settings beside it, the actions above the list, and the
//! Recent, Local, Remote and Tags branches below, folded by the slashes of their names.
//! A branch opens its actions in a submenu. The list comes from `git for-each-ref` off
//! the UI thread.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use search::{FuzzyMatcher, NameMatcher};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use vcs::{Branch, Operation};
use workspace::{GitProjectSettings, Settings, Workspace};

use crate::git::GitState;

const CONTEXT: &str = "BranchPopup";

actions!(branches, [Rename, NewBranch]);

macro_rules! branch_keys {
    ($(($keys:literal, $action:ident, $idea:expr, $mac:expr)),* $(,)?) => {
        /// The keys of the popup while it has the focus. They are actions of their own,
        /// so that F2 renames a branch here and goes to the next problem everywhere else.
        pub(crate) fn bind_keys(cx: &mut App) {
            bind_keys_for(cx, keymap::platform());
        }

        /// Binds the keys of one platform, so that a test reads the macOS table on every
        /// runner.
        fn bind_keys_for(cx: &mut App, platform: keymap::Platform) {
            $(
                let row = workspace::KeyRow {
                    keys: $keys,
                    mac: $mac,
                    action: concat!("branches::", stringify!($action)),
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
                action: concat!("branches::", stringify!($action)),
                context: Some(CONTEXT),
                idea: $idea,
            }),*
        ];
    };
}

branch_keys![
    ("f2", Rename, Some("Git.Rename.Local.Branch"), None),
    ("shift-f6", Rename, Some("Git.Rename.Local.Branch"), None),
    ("ctrl-alt-n", NewBranch, Some("Git.CreateNewBranch"), None),
];

const WIDTH: f32 = 470.;
const MAX_HEIGHT: f32 = 560.;
/// The pointer rests on a branch this long before its actions open, so a diagonal move
/// into the submenu does not flap.
const SUBMENU_DELAY: Duration = Duration::from_millis(220);
/// Recent branches the popup keeps, as IDEA does.
const RECENT_MAX: usize = 5;
/// The ids of the branch rows of Local and Remote start here, as they did before the popup
/// became a view: `menu-0` is New Branch and the first branch is `menu-2`.
const BRANCH_ROWS: usize = 2;

/// One tag the popup lists under Tags.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Tag {
    pub name: String,
    /// The commit the tag points at, for Checkout.
    pub commit: String,
}

/// What the popup shows when it opens.
pub(crate) struct OpenedBranches {
    pub branches: Vec<Branch>,
    pub tags: Vec<Tag>,
    pub recent: Vec<String>,
    pub favourites: Vec<String>,
}

/// One row of the popup. Group headers and separators are not selectable.
enum Row {
    Header(&'static str),
    Separator,
    Command(Command),
    /// A collapsible group of the list: Recent, Local, Remote or Tags.
    Group {
        key: String,
        label: &'static str,
        open: bool,
    },
    /// A remote or a folder of the branch tree. `key` is the same across rebuilds, so
    /// the node keeps its state while the search changes.
    Node {
        key: String,
        label: String,
        depth: usize,
    },
    Branch {
        /// The branch in [`BranchPopup::branches`].
        index: usize,
        /// The id of a row of Local or Remote in the `menu-N` form of the tests and
        /// scripts; a row of Recent repeats a branch and has an id of its own.
        menu: Option<usize>,
        /// What the row shows; `None` is the name of the branch as git gives it.
        display: Option<String>,
        /// The depth of the row in the tree; zero for a branch outside a folder.
        depth: usize,
    },
    Tag {
        /// The tag in [`BranchPopup::tags`].
        index: usize,
        menu: usize,
        display: Option<String>,
        depth: usize,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Command {
    NewBranch,
    CheckoutTag,
    Commit,
    Continue,
    Abort,
    Conflicts,
    Fetch,
    Update,
    Push,
    Stash,
    Unstash,
    GroupByDirectory,
}

/// The actions above the list, in the order IDEA lists them: an empty line of the menu
/// separates the groups.
const ACTION_GROUPS: &[&[Command]] = &[
    &[Command::Update, Command::Commit, Command::Push],
    &[
        Command::NewBranch,
        Command::CheckoutTag,
        Command::Stash,
        Command::Unstash,
    ],
    &[Command::GroupByDirectory],
];

/// The actions of a merge or a rebase that git waits to end.
const OPERATION_COMMANDS: &[Command] = &[Command::Continue, Command::Abort, Command::Conflicts];

pub struct BranchPopup {
    git: Entity<GitState>,
    workspace: WeakEntity<Workspace>,
    /// The root of the project, which keeps the favourite branches.
    root: PathBuf,
    focus: FocusHandle,
    scroll: ScrollHandle,
    /// Where the button that opened the popup is.
    position: Point<Pixels>,
    branches: Vec<Branch>,
    tags: Vec<Tag>,
    recent: Vec<String>,
    /// The branches the user starred, by their full names.
    favourites: HashSet<String>,
    query: TextField,
    rows: Vec<Row>,
    /// How many leading rows are the actions above the list.
    actions_end: usize,
    selected: usize,
    /// The nodes and groups the user folded; a node not in the set is open.
    collapsed: HashSet<String>,
    /// The text the rows were built for, so that a change of the search opens the
    /// nodes with the matches.
    last_query: String,
    /// Local and remote branches sit in folders of their names; the switch of the menu
    /// mirrors the Git page of Settings.
    group_by_directory: bool,
    /// What git waits for when the popup opened, for the Continue and Abort rows.
    operation: Operation,
    /// The row of the branch whose actions are open.
    submenu: Option<usize>,
    submenu_entries: Vec<MenuEntry>,
    submenu_state: MenuState,
    submenu_geometry: MenuGeometry,
    /// The area of every row, so the actions know where to stand.
    rows_bounds: Rc<RefCell<HashMap<usize, Bounds<Pixels>>>>,
    hovered: Option<usize>,
    hover_serial: usize,
    hover_task: Option<Task<()>>,
}

impl BranchPopup {
    pub(crate) fn open(
        git: Entity<GitState>,
        opened: OpenedBranches,
        position: Point<Pixels>,
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        let weak = cx.weak_entity();
        let operation = git.read(cx).operation();
        let root = workspace.root().to_path_buf();
        let group_by_directory = cx
            .try_global::<Settings>()
            .is_none_or(|settings| settings.git_group_by_directory);
        let view = cx.new(|cx| {
            let mut popup = Self {
                git,
                workspace: weak,
                root,
                focus: cx.focus_handle(),
                scroll: ScrollHandle::new(),
                position,
                branches: opened.branches,
                tags: opened.tags,
                recent: opened.recent,
                favourites: opened.favourites.into_iter().collect(),
                query: TextField::default(),
                rows: Vec::new(),
                actions_end: 0,
                selected: 0,
                collapsed: HashSet::new(),
                last_query: String::new(),
                group_by_directory,
                operation,
                submenu: None,
                submenu_entries: Vec::new(),
                submenu_state: MenuState::default(),
                submenu_geometry: MenuGeometry::default(),
                rows_bounds: Rc::new(RefCell::new(HashMap::new())),
                hovered: None,
                hover_serial: 0,
                hover_task: None,
            };
            popup.rebuild();
            popup
        });
        let focus = view.read(cx).focus.clone();
        let rows = view.read(cx).menu_rows();
        workspace.show_overlay_with_rows(view.into(), focus, Some(rows), window, cx);
    }

    /// The labels of the rows the popup shows, as a menu would list them: the checks of
    /// the window look at the rows of the popup the branch button opens.
    fn menu_rows(&self) -> Vec<MenuEntry> {
        self.rows
            .iter()
            .filter_map(|row| match row {
                Row::Header(label) => Some(MenuEntry::disabled(*label)),
                Row::Separator => None,
                Row::Group { label, .. } => Some(MenuEntry::disabled(*label)),
                Row::Command(command) => Some(MenuEntry::disabled(command_label(*command))),
                Row::Node { label, .. } => Some(MenuEntry::disabled(label.clone())),
                Row::Branch { index, display, .. } => self.branches.get(*index).map(|branch| {
                    MenuEntry::disabled(display.clone().unwrap_or_else(|| branch.name.clone()))
                }),
                Row::Tag { index, display, .. } => self.tags.get(*index).map(|tag| {
                    MenuEntry::disabled(display.clone().unwrap_or_else(|| tag.name.clone()))
                }),
            })
            .collect()
    }

    /// The branches the search keeps, in the order of the list, for a test.
    #[cfg(test)]
    pub(crate) fn visible(&self) -> Vec<String> {
        self.rows
            .iter()
            .filter_map(|row| match row {
                Row::Branch { index, .. } => Some(self.branches.get(*index)?.name.clone()),
                _ => None,
            })
            .collect()
    }

    /// The labels of the tree as the popup shows them, with the depth of each, for a
    /// test: nodes and branches only.
    #[cfg(test)]
    pub(crate) fn tree(&self) -> Vec<(usize, String)> {
        self.rows
            .iter()
            .filter_map(|row| match row {
                Row::Node { label, depth, .. } => Some((*depth, label.clone())),
                Row::Branch {
                    index,
                    display,
                    depth,
                    ..
                } => self.branches.get(*index).map(|branch| {
                    (
                        *depth,
                        display.clone().unwrap_or_else(|| branch.name.clone()),
                    )
                }),
                Row::Tag {
                    index,
                    display,
                    depth,
                    ..
                } => self
                    .tags
                    .get(*index)
                    .map(|tag| (*depth, display.clone().unwrap_or_else(|| tag.name.clone()))),
                _ => None,
            })
            .collect()
    }

    /// The labels of the nodes of the tree, for a test.
    #[cfg(test)]
    pub(crate) fn nodes(&self) -> Vec<String> {
        self.rows
            .iter()
            .filter_map(|row| match row {
                Row::Node { label, .. } => Some(label.clone()),
                _ => None,
            })
            .collect()
    }

    /// The labels of the actions of the branch whose submenu is open, for a test.
    #[cfg(test)]
    pub(crate) fn submenu_labels(&self) -> Vec<String> {
        self.submenu_entries
            .iter()
            .filter(|entry| !entry.label.is_empty())
            .map(|entry| entry.label.to_string())
            .collect()
    }

    /// The labels of the actions above the list, for a test.
    #[cfg(test)]
    pub(crate) fn actions(&self) -> Vec<String> {
        self.rows
            .iter()
            .take(self.actions_end)
            .filter_map(|row| match row {
                Row::Command(command) => Some(command_label(*command).to_owned()),
                _ => None,
            })
            .collect()
    }

    fn current(&self) -> String {
        self.branches
            .iter()
            .find(|branch| branch.current)
            .map(|branch| branch.name.clone())
            .unwrap_or_else(|| "HEAD".to_owned())
    }

    /// Builds the rows of the list for the text of the search field.
    fn rebuild(&mut self) {
        let query = self.query.text.trim().to_owned();
        if query != self.last_query {
            self.last_query = query.clone();
            // The search shows the nodes with the branches it keeps, folded or not.
            self.collapsed.clear();
        }
        let selected = self.selection_id_at(self.selected);
        self.rows_bounds.borrow_mut().clear();
        self.rows.clear();
        let matched = matching_branches(&self.branches, &query);
        let tag_indexes = matching_names(self.tags.iter().map(|tag| tag.name.as_str()), &query);
        let mut rows = Vec::new();
        let operations = matching(&query, OPERATION_COMMANDS);
        if self.operation != Operation::None && !operations.is_empty() {
            rows.push(Row::Header("In Progress"));
            for command in operations {
                rows.push(Row::Command(command));
            }
        }
        let mut commands_present = false;
        for group in ACTION_GROUPS {
            let present = matching(&query, group);
            if present.is_empty() {
                continue;
            }
            if commands_present {
                rows.push(Row::Separator);
            }
            commands_present = true;
            for command in present {
                rows.push(Row::Command(command));
            }
        }
        if rows.iter().any(|row| matches!(row, Row::Command(_))) {
            rows.push(Row::Separator);
        }
        let actions_end = rows.len();

        let recent: Vec<usize> = self
            .recent
            .iter()
            .take(RECENT_MAX)
            .filter_map(|name| {
                self.branches
                    .iter()
                    .position(|branch| !branch.remote && &branch.name == name)
            })
            .filter(|index| matched.contains(index))
            .collect();
        if !recent.is_empty() {
            rows.push(self.group("recent", "Recent"));
            for index in recent {
                rows.push(Row::Branch {
                    index,
                    menu: None,
                    display: None,
                    depth: 0,
                });
            }
        }

        let mut menu = BRANCH_ROWS;
        let local: Vec<usize> = matched
            .iter()
            .copied()
            .filter(|index| !self.branches[*index].remote)
            .collect();
        if !local.is_empty() {
            rows.push(self.group("local", "Local"));
            self.push_branches(&mut rows, &local, &mut menu);
        }
        let remote: Vec<usize> = matched
            .iter()
            .copied()
            .filter(|index| self.branches[*index].remote)
            .collect();
        if !remote.is_empty() {
            rows.push(self.group("remote", "Remote"));
            self.push_remote(&mut rows, &remote, &mut menu);
        }
        if !tag_indexes.is_empty() {
            rows.push(self.group("tags", "Tags"));
            self.push_tags(&mut rows, &tag_indexes, &mut menu);
        }
        self.rows = rows;
        self.actions_end = actions_end;
        self.keep_selection(selected);
    }

    fn group(&self, name: &str, label: &'static str) -> Row {
        let key = format!("group/{name}");
        Row::Group {
            open: !self.collapsed.contains(&key),
            key,
            label,
        }
    }

    /// The branches of Local: the favourites first and flat, then the rest folded by
    /// the slashes of their names when the switch is on.
    fn push_branches(&self, rows: &mut Vec<Row>, indexes: &[usize], menu: &mut usize) {
        let (stars, rest) = split_favourites(indexes, &self.branches, &self.favourites);
        for index in stars {
            rows.push(Row::Branch {
                index,
                menu: Some(*menu),
                display: None,
                depth: 0,
            });
            *menu += 1;
        }
        if self.group_by_directory {
            let entries = self.entries(&rest, false);
            self.push_branch_tree(rows, &entries, "local", 0, menu);
        } else {
            for index in rest {
                rows.push(Row::Branch {
                    index,
                    menu: Some(*menu),
                    display: None,
                    depth: 0,
                });
                *menu += 1;
            }
        }
    }

    /// The branches of Remote: the favourites first and flat, then a node for every
    /// remote and the rest of its names folded by their slashes.
    fn push_remote(&self, rows: &mut Vec<Row>, indexes: &[usize], menu: &mut usize) {
        let (stars, rest) = split_favourites(indexes, &self.branches, &self.favourites);
        for index in stars {
            rows.push(Row::Branch {
                index,
                menu: Some(*menu),
                display: None,
                depth: 0,
            });
            *menu += 1;
        }
        if !self.group_by_directory {
            for index in rest {
                rows.push(Row::Branch {
                    index,
                    menu: Some(*menu),
                    display: None,
                    depth: 0,
                });
                *menu += 1;
            }
            return;
        }
        for (remote, entries) in remote_groups(&self.branches, &rest) {
            let key = format!("remote/{remote}");
            rows.push(Row::Node {
                key: key.clone(),
                label: remote,
                depth: 0,
            });
            if self.collapsed.contains(&key) {
                continue;
            }
            self.push_branch_tree(rows, &entries, &key, 1, menu);
        }
    }

    /// The tags of Tags, folded by the slashes of their names.
    fn push_tags(&self, rows: &mut Vec<Row>, indexes: &[usize], menu: &mut usize) {
        if !self.group_by_directory {
            for index in indexes {
                rows.push(Row::Tag {
                    index: *index,
                    menu: *menu,
                    display: None,
                    depth: 0,
                });
                *menu += 1;
            }
            return;
        }
        let mut entries: Vec<(String, usize)> = indexes
            .iter()
            .map(|index| (self.tags[*index].name.clone(), *index))
            .collect();
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        for row in tree_rows(&entries, "tags", 0, &self.collapsed) {
            match row {
                TreeRow::Folder { key, label, depth } => rows.push(Row::Node { key, label, depth }),
                TreeRow::Leaf { name, index, depth } => {
                    rows.push(Row::Tag {
                        index,
                        menu: *menu,
                        display: Some(name),
                        depth,
                    });
                    *menu += 1;
                }
            }
        }
    }

    /// The index and the last part of the names of a group, for the folder tree.
    fn entries(&self, indexes: &[usize], remote: bool) -> Vec<(String, usize)> {
        let mut entries: Vec<(String, usize)> = indexes
            .iter()
            .map(|index| {
                let name = &self.branches[*index].name;
                let name = match (remote, name.split_once('/')) {
                    (true, Some((_, rest))) => rest,
                    _ => name.as_str(),
                };
                (name.to_owned(), *index)
            })
            .collect();
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        entries
    }

    fn push_branch_tree(
        &self,
        rows: &mut Vec<Row>,
        entries: &[(String, usize)],
        prefix: &str,
        depth: usize,
        menu: &mut usize,
    ) {
        for row in tree_rows(entries, prefix, depth, &self.collapsed) {
            match row {
                TreeRow::Folder { key, label, depth } => rows.push(Row::Node { key, label, depth }),
                TreeRow::Leaf { name, index, depth } => {
                    rows.push(Row::Branch {
                        index,
                        menu: Some(*menu),
                        display: Some(name),
                        depth,
                    });
                    *menu += 1;
                }
            }
        }
    }

    fn selection_id_at(&self, row: usize) -> Option<String> {
        self.rows.get(row).and_then(|row| self.selection_id(row))
    }

    fn selection_id(&self, row: &Row) -> Option<String> {
        match row {
            Row::Branch { index, .. } => self
                .branches
                .get(*index)
                .map(|branch| format!("branch:{}", branch.name)),
            Row::Tag { index, .. } => self.tags.get(*index).map(|tag| format!("tag:{}", tag.name)),
            Row::Node { key, .. } | Row::Group { key, .. } => Some(format!("node:{key}")),
            _ => None,
        }
    }

    /// The selection stays on its branch or node across a change of the search, or moves
    /// to the current branch, or to the first branch, or to the first row.
    fn keep_selection(&mut self, selected: Option<String>) {
        let found = selected.and_then(|wanted| {
            self.rows.iter().position(|row| {
                self.selection_id(row).as_deref() == Some(wanted.as_str()) && row_selectable(row)
            })
        });
        self.selected = found
            .or_else(|| {
                let current = format!("branch:{}", self.current());
                self.rows.iter().position(|row| {
                    self.selection_id(row).as_deref() == Some(current.as_str())
                        && row_selectable(row)
                })
            })
            .or_else(|| {
                self.rows
                    .iter()
                    .position(|row| matches!(row, Row::Branch { .. } | Row::Tag { .. }))
            })
            .or_else(|| self.rows.iter().position(row_selectable))
            .unwrap_or(0);
    }

    fn move_selection(&mut self, step: isize, cx: &mut Context<Self>) {
        if self.rows.is_empty() {
            return;
        }
        let len = self.rows.len() as isize;
        let mut at = self.selected as isize;
        for _ in 0..len {
            at = (at + step).rem_euclid(len);
            if row_selectable(&self.rows[at as usize]) {
                self.selected = at as usize;
                cx.notify();
                return;
            }
        }
    }

    /// Folds a node or opens it, keeping the selection on it.
    fn set_node_open(&mut self, row: usize, open: bool, cx: &mut Context<Self>) {
        let Some(key) = self.rows.get(row).and_then(node_key) else {
            return;
        };
        let was_open = !self.collapsed.contains(&key);
        if was_open == open {
            return;
        }
        if open {
            self.collapsed.remove(&key);
        } else {
            self.collapsed.insert(key.clone());
        }
        self.rebuild();
        if let Some(at) = self
            .rows
            .iter()
            .position(|row| node_key(row).is_some_and(|known| known == key))
        {
            self.selected = at;
        }
        cx.notify();
    }

    fn toggle_node(&mut self, row: usize, cx: &mut Context<Self>) {
        let Some(key) = self.rows.get(row).and_then(node_key) else {
            return;
        };
        let open = self.collapsed.contains(&key);
        self.set_node_open(row, open, cx);
    }

    /// Folds or opens Recent, Local, Remote or Tags.
    fn toggle_group(&mut self, row: usize, cx: &mut Context<Self>) {
        let Some(Row::Group { key, .. }) = self.rows.get(row) else {
            return;
        };
        let key = key.clone();
        if !self.collapsed.remove(&key) {
            self.collapsed.insert(key);
        }
        self.rebuild();
        cx.notify();
    }

    /// The switch of the menu and of the Git page of Settings: the popup folds the
    /// branches into the folders of their names or lists them flat.
    fn toggle_group_by_directory(&mut self, cx: &mut Context<Self>) {
        let group = !self.group_by_directory;
        self.group_by_directory = group;
        if let Some(workspace) = self.workspace.upgrade() {
            workspace.update(cx, |workspace, cx| {
                workspace.update_settings(|settings| settings.git_group_by_directory = group, cx)
            });
        }
        self.rebuild();
        cx.notify();
    }

    /// The star of a branch: it moves the branch to the top of its group and keeps the
    /// choice in the settings of the project.
    fn toggle_favourite(&mut self, row: usize, cx: &mut Context<Self>) {
        let Some(Row::Branch { index, .. }) = self.rows.get(row) else {
            return;
        };
        let Some(branch) = self.branches.get(*index) else {
            return;
        };
        let name = branch.name.clone();
        if !self.favourites.remove(&name) {
            self.favourites.insert(name);
        }
        let mut favourite_branches: Vec<String> = self.favourites.iter().cloned().collect();
        favourite_branches.sort();
        let settings = GitProjectSettings { favourite_branches };
        let root = self.root.clone();
        cx.background_executor()
            .spawn(async move {
                let _ = workspace::save_git_settings(&root, &settings);
            })
            .detach();
        self.rebuild();
        cx.notify();
    }

    /// Right: the actions of the selected branch.
    fn open_submenu(&mut self, cx: &mut Context<Self>) {
        self.open_submenu_at(self.selected, cx);
    }

    fn open_submenu_at(&mut self, row: usize, cx: &mut Context<Self>) {
        let entries = match self.rows.get(row) {
            Some(Row::Branch { index, .. }) => self
                .branches
                .get(*index)
                .map(|branch| self.actions_of(branch)),
            Some(Row::Tag { index, .. }) => {
                self.tags.get(*index).map(|tag| self.actions_of_tag(tag))
            }
            _ => None,
        };
        let Some(entries) = entries else {
            return;
        };
        if entries.is_empty() {
            return;
        }
        self.submenu = Some(row);
        self.submenu_entries = entries;
        self.submenu_state.clear();
        self.submenu_state.hover(&[0]);
        // The lists of a new submenu have their own scroll, not the one of the last submenu.
        self.submenu_geometry = MenuGeometry::default();
        cx.notify();
    }

    fn close_submenu(&mut self, cx: &mut Context<Self>) -> bool {
        if self.submenu.take().is_none() {
            return false;
        }
        self.submenu_entries.clear();
        self.submenu_state.clear();
        self.submenu_geometry = MenuGeometry::default();
        cx.notify();
        true
    }

    /// The actions of one branch, in the order IDEA lists them.
    fn actions_of(&self, branch: &Branch) -> Vec<MenuEntry> {
        let current = self.current();
        let mut entries = Vec::new();
        if branch.remote {
            entries.push(self.checkout_action(&branch.name, true));
        } else if !branch.current {
            entries.push(self.checkout_action(&branch.name, false));
        }
        entries.push(self.new_branch_action(&branch.name));
        if !branch.remote && !branch.current {
            entries.push(self.checkout_and_rebase_action(&branch.name, &current));
        }
        if !branch.current {
            entries.push(self.compare_action(&branch.name, &current));
        }
        entries.push(self.diff_action(&branch.name));
        if !branch.current {
            entries.push(self.rebase_action(&current, &branch.name));
            entries.push(self.merge_action(&branch.name));
        }
        if !branch.remote {
            entries.push(self.push_action());
            entries.push(self.rename_action(&branch.name));
            if !branch.current {
                entries.push(self.delete_action(&branch.name, false));
            }
        } else {
            entries.push(self.delete_action(&branch.name, true));
        }
        entries
    }

    /// The actions of a tag: what git here does with the commit it points at.
    fn actions_of_tag(&self, tag: &Tag) -> Vec<MenuEntry> {
        let current = self.current();
        vec![
            self.checkout_tag_action(&tag.commit),
            self.new_branch_action(&tag.name),
            self.compare_action(&tag.name, &current),
            self.diff_action(&tag.name),
        ]
    }

    fn checkout_action(&self, name: &str, remote: bool) -> MenuEntry {
        let git = self.git.clone();
        let branch = name.to_owned();
        MenuEntry::new("Checkout", move |_, cx| {
            let name = branch.clone();
            git.update(cx, |git, cx| {
                if remote {
                    git.checkout_remote(name, cx);
                } else {
                    git.switch(name, cx);
                }
            })
        })
        .icon("general/vcs")
    }

    fn checkout_tag_action(&self, commit: &str) -> MenuEntry {
        let git = self.git.clone();
        let commit = commit.to_owned();
        MenuEntry::new("Checkout", move |_, cx| {
            let commit = commit.clone();
            git.update(cx, |git, cx| git.checkout_revision(commit, cx))
        })
        .icon("general/vcs")
    }

    fn new_branch_action(&self, name: &str) -> MenuEntry {
        let git = self.git.clone();
        let start = name.to_owned();
        let label = format!("New Branch from '{name}'...");
        MenuEntry::new(label, move |window, cx| {
            let start = start.clone();
            git.update(cx, |git, cx| git.ask_branch_name_from(start, window, cx))
        })
        .icon("general/add")
    }

    fn checkout_and_rebase_action(&self, name: &str, current: &str) -> MenuEntry {
        let git = self.git.clone();
        let (branch, onto) = (name.to_owned(), current.to_owned());
        let label = format!("Checkout and Rebase onto '{current}'");
        MenuEntry::new(label, move |_, cx| {
            let (branch, onto) = (branch.clone(), onto.clone());
            git.update(cx, |git, cx| git.checkout_and_rebase(branch, onto, cx))
        })
        .icon("general/vcs")
    }

    fn compare_action(&self, name: &str, current: &str) -> MenuEntry {
        let git = self.git.clone();
        let branch = name.to_owned();
        let label = format!("Compare with '{current}'");
        MenuEntry::new(label, move |_, cx| {
            let branch = branch.clone();
            git.update(cx, |git, cx| git.compare_branch(branch, cx));
        })
        .icon("general/vcs")
    }

    fn diff_action(&self, name: &str) -> MenuEntry {
        let git = self.git.clone();
        let branch = name.to_owned();
        MenuEntry::new("Show Diff with Working Tree", move |_, cx| {
            let branch = branch.clone();
            git.update(cx, |git, cx| git.show_working_tree_diff(branch, cx))
        })
        .icon("general/vcs")
    }

    fn rebase_action(&self, current: &str, onto: &str) -> MenuEntry {
        let git = self.git.clone();
        let branch = onto.to_owned();
        let label = format!("Rebase '{current}' onto '{onto}'");
        MenuEntry::new(label, move |_, cx| {
            let branch = branch.clone();
            git.update(cx, |git, cx| git.rebase_onto(branch, cx));
        })
        .icon("general/vcs")
    }

    fn merge_action(&self, name: &str) -> MenuEntry {
        let git = self.git.clone();
        let branch = name.to_owned();
        let current = self.current();
        let label = format!("Merge '{name}' into '{current}'");
        MenuEntry::new(label, move |_, cx| {
            let branch = branch.clone();
            git.update(cx, |git, cx| git.merge_branch(branch, cx));
        })
        .icon("general/vcs")
    }

    fn push_action(&self) -> MenuEntry {
        let git = self.git.clone();
        MenuEntry::new("Push...", move |window, cx| {
            git.update(cx, |git, cx| git.show_push_window(window, cx));
        })
        .icon("general/vcs")
    }

    fn rename_action(&self, name: &str) -> MenuEntry {
        let git = self.git.clone();
        let branch = name.to_owned();
        MenuEntry::new("Rename...", move |window, cx| {
            let branch = branch.clone();
            git.update(cx, |git, cx| git.ask_rename_branch(branch, window, cx))
        })
        .icon("general/vcs")
    }

    fn delete_action(&self, name: &str, remote: bool) -> MenuEntry {
        let git = self.git.clone();
        let branch = name.to_owned();
        MenuEntry::new("Delete", move |_, cx| {
            let branch = branch.clone();
            git.update(cx, |git, cx| {
                if remote {
                    git.delete_remote_branch(branch, cx);
                } else {
                    git.delete_branch(branch, false, cx);
                }
            })
        })
        .icon("general/delete")
    }

    fn run_command(&mut self, command: Command, window: &mut Window, cx: &mut Context<Self>) {
        if command == Command::GroupByDirectory {
            self.toggle_group_by_directory(cx);
            return;
        }
        let position = self.position;
        let git = self.git.clone();
        self.dismiss(window, cx);
        git.update(cx, |git, cx| match command {
            Command::NewBranch => git.ask_branch_name(window, cx),
            Command::CheckoutTag => git.ask_checkout_revision(window, cx),
            Command::Commit => git.show_commit_window(window, cx),
            Command::Continue => {
                let message = git.merge_message().to_owned();
                git.finish_operation(message, cx);
            }
            Command::Abort => git.abort_operation(cx),
            Command::Conflicts => git.open_conflicts(cx),
            Command::Fetch => git.fetch(cx),
            Command::Update => git.update_project(position, cx),
            Command::Push => git.show_push_window(window, cx),
            Command::Stash => git.ask_stash_message(window, cx),
            Command::Unstash => git.show_stashes(position, cx),
            Command::GroupByDirectory => {}
        })
    }

    /// A click on a branch checks it out; the current branch only closes the popup.
    fn checkout_row(&mut self, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        let target = match self.rows.get(row) {
            Some(Row::Branch { index, .. }) => self.branches.get(*index).map(|branch| {
                (
                    branch.name.clone(),
                    branch.remote,
                    branch.current,
                    None::<String>,
                )
            }),
            Some(Row::Tag { index, .. }) => self
                .tags
                .get(*index)
                .map(|tag| (tag.name.clone(), false, false, Some(tag.commit.clone()))),
            _ => None,
        };
        let Some((name, remote, current, commit)) = target else {
            return;
        };
        self.dismiss(window, cx);
        if current {
            return;
        }
        if let Some(commit) = commit {
            self.git
                .update(cx, |git, cx| git.checkout_revision(commit, cx));
            return;
        }
        self.git.update(cx, |git, cx| {
            if remote {
                git.checkout_remote(name, cx);
            } else {
                git.switch(name, cx);
            }
        })
    }

    fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace
            .update(cx, |workspace, cx| workspace.dismiss(window, cx))
            .ok();
    }

    /// Escape or F2: the keys the popup answers itself.
    fn rename_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = match self.rows.get(self.selected) {
            Some(Row::Tag { .. }) => return,
            Some(Row::Branch { index, .. }) => self
                .branches
                .get(*index)
                .filter(|branch| !branch.remote)
                .map(|branch| branch.name.clone())
                .unwrap_or_else(|| self.current()),
            _ => self.current(),
        };
        self.dismiss(window, cx);
        self.git
            .update(cx, |git, cx| git.ask_rename_branch(name, window, cx));
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let stroke = &event.keystroke;
        let key = stroke.key.as_str();
        let modifiers = stroke.modifiers;
        if self.submenu.is_some() && !modifiers.control && !modifiers.alt && !modifiers.platform {
            match key {
                "down" => {
                    self.submenu_state.move_down(&self.submenu_entries);
                    self.submenu_geometry.reveal(&self.submenu_state);
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                "up" => {
                    self.submenu_state.move_up(&self.submenu_entries);
                    self.submenu_geometry.reveal(&self.submenu_state);
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                "home" | "end" => {
                    if key == "home" {
                        self.submenu_state.move_first(&self.submenu_entries);
                    } else {
                        self.submenu_state.move_last(&self.submenu_entries);
                    }
                    self.submenu_geometry.reveal(&self.submenu_state);
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                "right" => {
                    self.submenu_state.open_submenu(&self.submenu_entries);
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                "left" | "escape" => {
                    self.close_submenu(cx);
                    cx.stop_propagation();
                    return;
                }
                "enter" => {
                    let action = self
                        .submenu_state
                        .current(&self.submenu_entries)
                        .and_then(|entry| entry.on_click.clone());
                    if let Some(action) = action {
                        self.dismiss(window, cx);
                        action(window, cx);
                    }
                    cx.stop_propagation();
                    return;
                }
                _ => {}
            }
        }
        if modifiers.control && modifiers.alt && key == "n" {
            self.run_command(Command::NewBranch, window, cx);
            cx.stop_propagation();
            return;
        }
        if (key == "f2" || modifiers.shift && key == "f6") && !modifiers.control && !modifiers.alt {
            self.rename_selected(window, cx);
            cx.stop_propagation();
            return;
        }
        match key {
            "escape" => {
                self.dismiss(window, cx);
                cx.stop_propagation();
            }
            "up" => {
                self.move_selection(-1, cx);
                cx.stop_propagation();
            }
            "down" => {
                self.move_selection(1, cx);
                cx.stop_propagation();
            }
            "right" => {
                if node_key_at(&self.rows, self.selected).is_some() {
                    self.set_node_open(self.selected, true, cx);
                } else {
                    self.open_submenu(cx);
                }
                cx.stop_propagation();
            }
            "left" => {
                if !self.close_submenu(cx) {
                    if node_key_at(&self.rows, self.selected).is_some() {
                        self.set_node_open(self.selected, false, cx);
                    } else {
                        self.move_selection(-1, cx);
                    }
                }
                cx.stop_propagation();
            }
            "enter" => {
                match self.rows.get(self.selected) {
                    Some(Row::Node { .. }) => self.toggle_node(self.selected, cx),
                    Some(Row::Branch { .. } | Row::Tag { .. }) => {
                        self.checkout_row(self.selected, window, cx)
                    }
                    Some(Row::Command(command)) => {
                        let command = *command;
                        self.run_command(command, window, cx)
                    }
                    _ => {}
                }
                cx.stop_propagation();
            }
            _ => {
                if self.query.key_with_clipboard(event, false, cx) {
                    self.rebuild();
                    cx.stop_propagation();
                    cx.notify();
                }
            }
        }
    }

    fn row_hover(&mut self, row: usize, entered: bool, cx: &mut Context<Self>) {
        if !entered {
            return;
        }
        if self.submenu == Some(row) {
            return;
        }
        if !matches!(
            self.rows.get(row),
            Some(Row::Branch { .. } | Row::Tag { .. })
        ) {
            return;
        }
        self.hovered = Some(row);
        self.hover_serial += 1;
        let serial = self.hover_serial;
        self.hover_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SUBMENU_DELAY).await;
            this.update(cx, |this, cx| {
                if this.hover_serial == serial && this.hovered == Some(row) {
                    this.open_submenu_at(row, cx);
                }
            })
            .ok();
        }));
    }

    fn header(&self, label: &'static str, theme: &Theme) -> impl IntoElement {
        div()
            .flex_none()
            .h(px(CONTROL_HEIGHT))
            .px(px(ROW_PADDING_X))
            .flex()
            .items_center()
            .text_color(theme.text_muted)
            .child(label)
    }

    fn group_row(
        &self,
        row: usize,
        label: &'static str,
        open: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let arrow = if open {
            "general/chevronDown"
        } else {
            "general/chevronRight"
        };
        div()
            .id(("branch-group", row))
            .debug_selector(move || format!("branch-group-{row}"))
            .flex()
            .items_center()
            .gap(px(SPACE_XS))
            .flex_none()
            .h(px(CONTROL_HEIGHT))
            .px(px(ROW_PADDING_X))
            .rounded(px(ROW_RADIUS))
            .hover({
                let hover = theme.hover;
                move |style| style.bg(hover)
            })
            .child(
                div()
                    .size(px(14.))
                    .flex_none()
                    .child(icon_sized(arrow, 10., theme)),
            )
            .child(label)
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_group(row, cx)))
    }

    fn command_row(
        &self,
        index: usize,
        command: Command,
        label: &str,
        icon_name: Option<&'static str>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let selector = match command {
            Command::NewBranch => "menu-0".to_owned(),
            Command::CheckoutTag => "branch-checkout-tag".to_owned(),
            Command::Commit => "branch-commit".to_owned(),
            Command::Continue => "branch-continue".to_owned(),
            Command::Abort => "branch-abort".to_owned(),
            Command::Conflicts => "branch-conflicts".to_owned(),
            Command::Fetch => "branch-fetch".to_owned(),
            Command::Update => "branch-update".to_owned(),
            Command::Push => "branch-push".to_owned(),
            Command::Stash => "branch-stash".to_owned(),
            Command::Unstash => "branch-unstash".to_owned(),
            Command::GroupByDirectory => "branch-group-directories".to_owned(),
        };
        let shortcut =
            command_shortcut(command).map(|keys| keymap::shortcut_label(keys, keymap::platform()));
        let chosen = self.selected == index;
        div()
            .id(("branch-command", index))
            .debug_selector(move || selector.clone())
            .flex()
            .items_center()
            .gap(px(SPACE_S))
            .flex_none()
            .h(px(ROW_HEIGHT))
            .px(px(ROW_PADDING_X))
            .rounded(px(ROW_RADIUS))
            .when(chosen, |this| this.bg(theme.selection))
            .hover({
                let hover = theme.hover;
                move |style| style.bg(hover)
            })
            .child(
                div()
                    .size(px(16.))
                    .flex_none()
                    .children(icon_name.map(|name| icon(name, theme))),
            )
            .child(div().flex_1().min_w_0().truncate().child(label.to_owned()))
            .child(
                div()
                    .flex_none()
                    .text_color(theme.text_secondary)
                    .children(shortcut),
            )
            .on_click(cx.listener(move |this, _, window, cx| this.run_command(command, window, cx)))
    }

    #[allow(clippy::too_many_arguments)]
    fn branch_row(
        &self,
        row: usize,
        index: usize,
        menu: Option<usize>,
        label: String,
        depth: usize,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let branch = &self.branches[index];
        let current = branch.current;
        let favourite = self.favourites.contains(&branch.name);
        let upstream = branch.upstream.clone().unwrap_or_default();
        let (ahead, behind) = (branch.ahead, branch.behind);
        let counts = match (ahead, behind) {
            (0, 0) => String::new(),
            (ahead, 0) => format!("↑{ahead}"),
            (0, behind) => format!("↓{behind}"),
            (ahead, behind) => format!("↑{ahead} ↓{behind}"),
        };
        let chosen = self.selected == row;
        let open = self.submenu == Some(row);
        let selector = match menu {
            Some(menu) => format!("menu-{menu}"),
            None => format!("branch-recent-{row}"),
        };
        let indent = ROW_PADDING_X + depth as f32 * SPACE_L;
        let leading = if current {
            "general/check"
        } else if favourite {
            "general/star"
        } else {
            "general/vcs"
        };
        let star = if favourite {
            "general/star"
        } else {
            "general/starOutline"
        };
        let star_label = if favourite {
            "Remove from Favourites"
        } else {
            "Add to Favourites"
        };
        let measure = {
            let bounds = self.rows_bounds.clone();
            canvas(
                move |area, _, _| {
                    bounds.borrow_mut().insert(row, area);
                },
                |_, _, _, _| {},
            )
            .absolute()
            .top_0()
            .left_0()
            .size_full()
        };
        div()
            .id(("branch-row", row))
            .debug_selector(move || selector.clone())
            .group("branch-row")
            .relative()
            .flex()
            .items_center()
            .gap(px(SPACE_XS))
            .flex_none()
            .h(px(ROW_HEIGHT))
            .pl(px(indent))
            .pr(px(ROW_PADDING_X))
            .rounded(px(ROW_RADIUS))
            .when(chosen || open, |this| this.bg(theme.selection))
            .hover({
                let hover = theme.hover;
                move |style| style.bg(hover)
            })
            .child(measure)
            .child(
                div()
                    .size(px(16.))
                    .flex_none()
                    .child(icon_sized(leading, 12., theme)),
            )
            .child(div().flex_1().min_w_0().truncate().child(label))
            .child(
                div()
                    .flex_none()
                    .text_color(theme.text_secondary)
                    .text_size(px(TEXT_SMALL))
                    .children((!upstream.is_empty()).then_some(upstream)),
            )
            .child(
                div()
                    .flex_none()
                    .text_color(theme.text_secondary)
                    .child(counts),
            )
            .child(
                div()
                    .id(("branch-favourite", row))
                    .debug_selector(move || format!("branch-favourite-{row}"))
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(18.))
                    .flex_none()
                    .rounded(px(ROW_RADIUS))
                    // A favourite branch already shows its star on the left: the switch
                    // that takes the star away stands under the pointer, as in IDEA. It
                    // keeps its place and its click, so the row does not move.
                    .opacity(0.)
                    .group_hover("branch-row", |style| style.opacity(1.))
                    .hover({
                        let hover = theme.pressed;
                        move |style| style.bg(hover)
                    })
                    .child(icon_sized(star, 12., theme))
                    .tooltip(move |_, cx| tooltip(star_label, None, cx))
                    .tooltip_show_delay(ui_kit::tooltip_show_delay())
                    .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                        cx.stop_propagation();
                        let _ = event;
                        this.toggle_favourite(row, cx);
                    })),
            )
            .child(
                div()
                    .id(("branch-actions", row))
                    .debug_selector(move || format!("branch-actions-{row}"))
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(18.))
                    .flex_none()
                    .rounded(px(ROW_RADIUS))
                    .hover({
                        let hover = theme.pressed;
                        move |style| style.bg(hover)
                    })
                    .child(icon_sized("general/chevronRight", 10., theme))
                    .tooltip(|_, cx| tooltip("Show Branch Actions", None, cx))
                    .tooltip_show_delay(ui_kit::tooltip_show_delay())
                    .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                        cx.stop_propagation();
                        let _ = event;
                        this.open_submenu_at(row, cx);
                    })),
            )
            .on_hover(cx.listener(move |this, hovered, _, cx| this.row_hover(row, *hovered, cx)))
            .on_click(cx.listener(move |this, _, window, cx| this.checkout_row(row, window, cx)))
    }

    fn tag_row(
        &self,
        row: usize,
        menu: usize,
        label: String,
        depth: usize,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let chosen = self.selected == row;
        let open = self.submenu == Some(row);
        let selector = format!("menu-{menu}");
        let indent = ROW_PADDING_X + depth as f32 * SPACE_L;
        let measure = {
            let bounds = self.rows_bounds.clone();
            canvas(
                move |area, _, _| {
                    bounds.borrow_mut().insert(row, area);
                },
                |_, _, _, _| {},
            )
            .absolute()
            .top_0()
            .left_0()
            .size_full()
        };
        div()
            .id(("branch-tag", row))
            .debug_selector(move || selector.clone())
            .relative()
            .flex()
            .items_center()
            .gap(px(SPACE_XS))
            .flex_none()
            .h(px(ROW_HEIGHT))
            .pl(px(indent))
            .pr(px(ROW_PADDING_X))
            .rounded(px(ROW_RADIUS))
            .when(chosen || open, |this| this.bg(theme.selection))
            .hover({
                let hover = theme.hover;
                move |style| style.bg(hover)
            })
            .child(measure)
            .child(
                div()
                    .size(px(16.))
                    .flex_none()
                    .child(icon_sized("general/vcs", 12., theme)),
            )
            .child(div().flex_1().min_w_0().truncate().child(label))
            .child(
                div()
                    .id(("branch-tag-actions", row))
                    .debug_selector(move || format!("branch-actions-{row}"))
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(18.))
                    .flex_none()
                    .rounded(px(ROW_RADIUS))
                    .hover({
                        let hover = theme.pressed;
                        move |style| style.bg(hover)
                    })
                    .child(icon_sized("general/chevronRight", 10., theme))
                    .tooltip(|_, cx| tooltip("Show Tag Actions", None, cx))
                    .tooltip_show_delay(ui_kit::tooltip_show_delay())
                    .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                        cx.stop_propagation();
                        let _ = event;
                        this.open_submenu_at(row, cx);
                    })),
            )
            .on_hover(cx.listener(move |this, hovered, _, cx| this.row_hover(row, *hovered, cx)))
            .on_click(cx.listener(move |this, _, window, cx| this.checkout_row(row, window, cx)))
    }

    fn node_row(
        &self,
        row: usize,
        label: &str,
        depth: usize,
        open: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let chosen = self.selected == row;
        let indent = ROW_PADDING_X + depth as f32 * SPACE_L;
        let arrow = if open {
            "general/chevronDown"
        } else {
            "general/chevronRight"
        };
        div()
            .id(("branch-node", row))
            .debug_selector(move || format!("branch-node-{row}"))
            .flex()
            .items_center()
            .gap(px(SPACE_XS))
            .flex_none()
            .h(px(ROW_HEIGHT))
            .pl(px(indent))
            .pr(px(ROW_PADDING_X))
            .rounded(px(ROW_RADIUS))
            .when(chosen, |this| this.bg(theme.selection))
            .hover({
                let hover = theme.hover;
                move |style| style.bg(hover)
            })
            .child(
                div()
                    .size(px(14.))
                    .flex_none()
                    .child(icon_sized(arrow, 10., theme)),
            )
            .child(div().flex_1().min_w_0().truncate().child(label.to_owned()))
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_node(row, cx)))
    }
}

fn row_selectable(row: &Row) -> bool {
    matches!(
        row,
        Row::Command(_) | Row::Node { .. } | Row::Branch { .. } | Row::Tag { .. }
    )
}

/// The key of a node of the tree, for the fold state that outlives a rebuild.
fn node_key(row: &Row) -> Option<String> {
    match row {
        Row::Node { key, .. } => Some(key.clone()),
        _ => None,
    }
}

fn node_key_at(rows: &[Row], at: usize) -> Option<String> {
    rows.get(at).and_then(node_key)
}

/// One row of a folder tree, before it becomes a row of the popup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TreeRow {
    Folder {
        key: String,
        label: String,
        depth: usize,
    },
    Leaf {
        name: String,
        index: usize,
        depth: usize,
    },
}

/// A folder of branch names: `feature/login` and `feature/signup` sit under `feature`,
/// and a deeper name adds a deeper folder. Folders and leaves are sorted by name.
#[derive(Default)]
struct FolderTree {
    folders: BTreeMap<String, FolderTree>,
    leaves: BTreeMap<String, usize>,
}

impl FolderTree {
    fn insert(&mut self, path: &[&str], index: usize) {
        let Some((head, rest)) = path.split_first() else {
            return;
        };
        if rest.is_empty() {
            self.leaves.insert((*head).to_owned(), index);
            return;
        }
        self.folders
            .entry((*head).to_owned())
            .or_default()
            .insert(rest, index);
    }

    fn rows(
        &self,
        prefix: &str,
        depth: usize,
        collapsed: &HashSet<String>,
        out: &mut Vec<TreeRow>,
    ) {
        for (label, child) in &self.folders {
            let key = format!("{prefix}/{label}");
            out.push(TreeRow::Folder {
                key: key.clone(),
                label: label.clone(),
                depth,
            });
            if !collapsed.contains(&key) {
                child.rows(&key, depth + 1, collapsed, out);
            }
        }
        for (name, index) in &self.leaves {
            out.push(TreeRow::Leaf {
                name: name.clone(),
                index: *index,
                depth,
            });
        }
    }
}

/// The rows of a folder tree over `(name, index)` entries.
pub(crate) fn tree_rows(
    entries: &[(String, usize)],
    prefix: &str,
    depth: usize,
    collapsed: &HashSet<String>,
) -> Vec<TreeRow> {
    let mut tree = FolderTree::default();
    for (name, index) in entries {
        let parts: Vec<&str> = name.split('/').collect();
        tree.insert(&parts, *index);
    }
    let mut rows = Vec::new();
    tree.rows(prefix, depth, collapsed, &mut rows);
    rows
}

/// The remote branches by their remote, then the rest of their names.
pub(crate) fn remote_groups(
    branches: &[Branch],
    indexes: &[usize],
) -> Vec<(String, Vec<(String, usize)>)> {
    let mut groups: BTreeMap<String, Vec<(String, usize)>> = BTreeMap::new();
    for index in indexes {
        let Some((remote, rest)) = branches[*index].name.split_once('/') else {
            continue;
        };
        groups
            .entry(remote.to_owned())
            .or_default()
            .push((rest.to_owned(), *index));
    }
    for entries in groups.values_mut() {
        entries.sort_by(|left, right| left.0.cmp(&right.0));
    }
    groups.into_iter().collect()
}

/// The indexes of `branches` the search keeps, in the order of the list. The pattern is
/// fuzzy, as Search Everywhere finds a name, and a slash of a branch past a slash of the
/// pattern.
pub(crate) fn matching_branches(branches: &[Branch], query: &str) -> Vec<usize> {
    let query = query.trim();
    if query.is_empty() {
        return (0..branches.len()).collect();
    }
    let mut matcher = NameMatcher::for_paths();
    matcher.set_pattern(query);
    (0..branches.len())
        .filter(|index| matcher.score(&branches[*index].name).is_some())
        .collect()
}

/// The indexes of `names` the search keeps.
pub(crate) fn matching_names<'a>(
    names: impl IntoIterator<Item = &'a str>,
    query: &str,
) -> Vec<usize> {
    let query = query.trim();
    let names: Vec<&str> = names.into_iter().collect();
    if query.is_empty() {
        return (0..names.len()).collect();
    }
    let mut matcher = NameMatcher::for_paths();
    matcher.set_pattern(query);
    names
        .iter()
        .enumerate()
        .filter(|(_, name)| matcher.score(name).is_some())
        .map(|(index, _)| index)
        .collect()
}

/// The commands of `commands` the search keeps, in their order.
fn matching(query: &str, commands: &[Command]) -> Vec<Command> {
    let query = query.trim();
    if query.is_empty() {
        return commands.to_vec();
    }
    let mut matcher = NameMatcher::for_paths();
    matcher.set_pattern(query);
    commands
        .iter()
        .copied()
        .filter(|command| matcher.score(command_label(*command)).is_some())
        .collect()
}

/// The branches of one group that are favourites, and the rest, both by name. The
/// favourites stand above the folders of their group.
pub(crate) fn split_favourites(
    indexes: &[usize],
    branches: &[Branch],
    favourites: &HashSet<String>,
) -> (Vec<usize>, Vec<usize>) {
    let mut stars = Vec::new();
    let mut rest = Vec::new();
    for index in indexes {
        if favourites.contains(&branches[*index].name) {
            stars.push(*index);
        } else {
            rest.push(*index);
        }
    }
    stars.sort_by(|left, right| branches[*left].name.cmp(&branches[*right].name));
    rest.sort_by(|left, right| branches[*left].name.cmp(&branches[*right].name));
    (stars, rest)
}

fn command_label(command: Command) -> &'static str {
    match command {
        Command::NewBranch => "New Branch...",
        Command::CheckoutTag => "Checkout Tag or Revision...",
        Command::Commit => "Commit...",
        Command::Continue => "Continue",
        Command::Abort => "Abort",
        Command::Conflicts => "Conflicts...",
        Command::Fetch => "Fetch",
        Command::Update => "Update Project...",
        Command::Push => "Push...",
        Command::Stash => "Stash Changes...",
        Command::Unstash => "Unstash Changes...",
        Command::GroupByDirectory => "Group by Directory",
    }
}

fn command_shortcut(command: Command) -> Option<&'static str> {
    match command {
        Command::Update => Some("ctrl-t"),
        Command::Commit => Some("ctrl-k"),
        Command::Push => Some("ctrl-shift-k"),
        Command::NewBranch => Some("ctrl-alt-n"),
        _ => None,
    }
}

fn command_icon(command: Command, group_by_directory: bool) -> Option<&'static str> {
    match command {
        Command::NewBranch => Some("general/add"),
        Command::CheckoutTag => Some("general/vcs"),
        Command::Commit => Some("toolwindows/commit"),
        Command::Continue => Some("general/vcs"),
        Command::Abort => Some("general/delete"),
        Command::Conflicts => Some("general/vcs"),
        Command::Fetch => Some("general/download"),
        Command::Update => Some("general/refresh"),
        Command::Push => Some("general/arrowUp"),
        Command::Stash => Some("general/download"),
        Command::Unstash => Some("general/arrowUp"),
        Command::GroupByDirectory => Some(if group_by_directory {
            "general/check"
        } else {
            "general/filter"
        }),
    }
}

impl Render for BranchPopup {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let viewport = window.viewport_size();
        let width = px(WIDTH);
        let left = self.position.x.clamp(
            px(SPACE_M),
            (viewport.width - width - px(SPACE_M)).max(px(SPACE_M)),
        );
        let top = self.position.y.clamp(
            px(SPACE_M),
            (viewport.height - px(MAX_HEIGHT) - px(SPACE_M)).max(px(SPACE_M)),
        );
        let mut actions = Vec::new();
        let mut list = Vec::new();
        for at in 0..self.rows.len() {
            let target = if at < self.actions_end {
                &mut actions
            } else {
                &mut list
            };
            match &self.rows[at] {
                Row::Header(label) => target.push(self.header(label, &theme).into_any_element()),
                Row::Separator => target.push(
                    div()
                        .h(px(1.))
                        .mx(px(SPACE_M))
                        .my(px(SPACE_S))
                        .bg(theme.popup_border)
                        .into_any_element(),
                ),
                Row::Group { label, open, .. } => {
                    let (label, open) = (*label, *open);
                    target.push(
                        self.group_row(at, label, open, &theme, cx)
                            .into_any_element(),
                    );
                }
                Row::Command(command) => {
                    let label = command_label(*command);
                    let icon_name = command_icon(*command, self.group_by_directory);
                    target.push(
                        self.command_row(at, *command, label, icon_name, &theme, cx)
                            .into_any_element(),
                    );
                }
                Row::Node {
                    label, depth, key, ..
                } => {
                    let (label, depth, key) = (label.clone(), *depth, key.clone());
                    let open = !self.collapsed.contains(&key);
                    target.push(
                        self.node_row(at, &label, depth, open, &theme, cx)
                            .into_any_element(),
                    );
                }
                Row::Branch {
                    index,
                    menu,
                    display,
                    depth,
                } => {
                    let (branch, menu, display, depth) = (*index, *menu, display.clone(), *depth);
                    let label = display.unwrap_or_else(|| self.branches[branch].name.clone());
                    target.push(
                        self.branch_row(at, branch, menu, label, depth, &theme, cx)
                            .into_any_element(),
                    );
                }
                Row::Tag {
                    index,
                    menu,
                    display,
                    depth,
                } => {
                    let (tag, menu, display, depth) = (*index, *menu, display.clone(), *depth);
                    let label = display.unwrap_or_else(|| self.tags[tag].name.clone());
                    target.push(
                        self.tag_row(at, menu, label, depth, &theme, cx)
                            .into_any_element(),
                    );
                }
            }
        }
        let empty = list.is_empty();
        let empty_query = self.query.text.is_empty();
        let search = div()
            .flex()
            .items_center()
            .flex_none()
            .gap(px(SPACE_S))
            .h(px(CONTROL_HEIGHT))
            .mb(px(SPACE_XS))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .items_center()
                    .gap(px(SPACE_S))
                    .h_full()
                    .px(px(SPACE_M))
                    .rounded(px(CONTROL_RADIUS))
                    .border_2()
                    .border_color(theme.accent)
                    .child(icon("general/search", &theme))
                    .child(
                        div()
                            .flex()
                            .flex_1()
                            .items_center()
                            .relative()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .children(text_field_children(
                                &self.query,
                                theme.text,
                                theme.text,
                                theme.selection,
                                theme.caret,
                                16.,
                                !empty_query,
                            ))
                            .when(empty_query, |this| {
                                this.child(
                                    div()
                                        .absolute()
                                        .left_0()
                                        .top_0()
                                        .text_color(theme.text_muted)
                                        .child("Search for branches and actions"),
                                )
                            }),
                    ),
            )
            .child(
                icon_button("branch-fetch", "general/refresh", "Fetch", None, &theme).on_click(
                    cx.listener(|this, _, window, cx| this.run_command(Command::Fetch, window, cx)),
                ),
            )
            .child(
                icon_button(
                    "branch-settings",
                    "general/settings",
                    "Settings",
                    None,
                    &theme,
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    this.dismiss(window, cx);
                    window.dispatch_action(Box::new(workspace::ShowSettings), cx);
                })),
            );
        let mut body = surface(&theme)
            .id("branches-popup")
            .debug_selector(|| "branches-popup".to_owned())
            .track_focus(&self.focus)
            .key_context(CONTEXT)
            .occlude()
            .w(width)
            .max_h(px(MAX_HEIGHT))
            .p(px(POPUP_PADDING))
            .flex()
            .flex_col()
            .on_action(cx.listener(|this, _: &Rename, window, cx| this.rename_selected(window, cx)))
            .on_action(cx.listener(|this, _: &NewBranch, window, cx| {
                this.run_command(Command::NewBranch, window, cx)
            }))
            .capture_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)))
            .child(search)
            .children(actions)
            .child(
                scrollable(
                    "branches-list",
                    &self.scroll,
                    div()
                        .id("branches-list")
                        .debug_selector(|| "branches-list".to_owned())
                        .flex()
                        .flex_col()
                        .overflow_y_scroll()
                        .track_scroll(&self.scroll)
                        .children(list)
                        .when(empty, |this| {
                            this.h(px(ROW_HEIGHT)).items_center().child("No branches")
                        }),
                    &theme,
                    window,
                    cx,
                )
                .min_h_0(),
            );
        if let Some(row) = self.submenu
            && !self.submenu_entries.is_empty()
            && let Some(bounds) = self.rows_bounds.borrow().get(&row).copied()
        {
            let size = menu_size(window, &self.submenu_entries, px(UI_SIZE));
            let position = submenu_position(bounds, size, viewport);
            let weak = cx.weak_entity();
            let close = weak.clone();
            let on_pick: MenuAction = Rc::new(move |window, cx| {
                close.update(cx, |this, cx| this.dismiss(window, cx)).ok();
            });
            let hover = weak.clone();
            let on_hover: MenuHover = Rc::new(move |path, entered, _, cx| {
                if !entered {
                    return;
                }
                hover
                    .update(cx, |this, cx| {
                        this.submenu_state.hover(path);
                        cx.notify();
                    })
                    .ok();
            });
            let open = weak;
            let on_open: MenuOpen = Rc::new(move |path, _, cx| {
                open.update(cx, |this, cx| {
                    this.submenu_state.open_row(&this.submenu_entries, path);
                    cx.notify();
                })
                .ok();
            });
            let callbacks = MenuCallbacks {
                on_pick,
                on_hover,
                on_open,
            };
            let entries = &self.submenu_entries;
            let state = &self.submenu_state;
            let geometry = &self.submenu_geometry;
            let menu = menu(entries, state, geometry, &callbacks, &theme, window, cx);
            body = body.child(
                deferred(
                    anchored()
                        .position(position)
                        .snap_to_window_with_margin(px(SPACE_M))
                        .child(menu),
                )
                .with_priority(2),
            );
        }
        let backdrop = deferred(
            div()
                .id("branches-backdrop")
                .absolute()
                .size_full()
                .occlude()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| this.dismiss(window, cx)),
                )
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(|this, _, window, cx| this.dismiss(window, cx)),
                ),
        )
        .with_priority(0);
        let popup = deferred(
            anchored()
                .position(point(left, top))
                .snap_to_window_with_margin(px(SPACE_M))
                // The fade runs once, when the popup opens. A key that changed with the
                // submenu would fade the whole popup again on every hover.
                .child(enter("branches-enter", body)),
        )
        .with_priority(1);
        div().absolute().size_full().child(backdrop).child(popup)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::{
        Branch, Command, TreeRow, command_icon, matching, matching_branches, remote_groups,
        split_favourites, tree_rows,
    };

    fn branch(name: &str, remote: bool, current: bool) -> Branch {
        Branch {
            name: name.to_owned(),
            remote,
            current,
            head: String::new(),
            upstream: None,
            ahead: 0,
            behind: 0,
        }
    }

    fn labels(rows: &[TreeRow]) -> Vec<(usize, String)> {
        rows.iter()
            .map(|row| match row {
                TreeRow::Folder { label, depth, .. } => (*depth, label.clone()),
                TreeRow::Leaf { name, depth, .. } => (*depth, name.clone()),
            })
            .collect()
    }

    #[test]
    fn the_tree_folds_the_branch_names_by_their_slashes() {
        let entries = [
            ("feature/b/c".to_owned(), 0),
            ("feature/a".to_owned(), 1),
            ("fix-1".to_owned(), 2),
        ];

        let rows = tree_rows(&entries, "local", 0, &HashSet::new());

        let expected = [
            (0, "feature".to_owned()),
            (1, "b".to_owned()),
            (2, "c".to_owned()),
            (1, "a".to_owned()),
            (0, "fix-1".to_owned()),
        ];
        assert_eq!(
            labels(&rows),
            expected,
            "folders hold the deeper names before the branches, and every level is sorted"
        );
    }

    #[test]
    fn a_folder_and_a_branch_of_the_same_name_are_two_rows() {
        let entries = [
            ("feature".to_owned(), 0),
            ("feature/a".to_owned(), 1),
            ("feature/b/c".to_owned(), 2),
        ];

        let rows = tree_rows(&entries, "local", 0, &HashSet::new());

        assert_eq!(
            labels(&rows),
            [
                (0, "feature".to_owned()),
                (1, "b".to_owned()),
                (2, "c".to_owned()),
                (1, "a".to_owned()),
                (0, "feature".to_owned()),
            ],
            "the folder `feature` stands above the branch of the same name"
        );
    }

    #[test]
    fn a_folded_folder_hides_its_rows() {
        let entries = [("feature/a".to_owned(), 0), ("fix-1".to_owned(), 1)];
        let collapsed = HashSet::from(["local/feature".to_owned()]);

        let rows = tree_rows(&entries, "local", 0, &collapsed);

        assert_eq!(
            labels(&rows),
            [(0, "feature".to_owned()), (0, "fix-1".to_owned())]
        );
    }

    #[test]
    fn the_remote_branches_group_by_the_remote_then_by_the_slashes() {
        let branches = [
            branch("feature/a", false, false),
            branch("origin/main", true, false),
            branch("origin/feature/a", true, false),
            branch("origin/feature/b/c", true, false),
            branch("upstream/main", true, false),
        ];

        let groups = remote_groups(&branches, &[1, 2, 3, 4]);

        assert_eq!(
            groups
                .iter()
                .map(|(remote, _)| remote.as_str())
                .collect::<Vec<_>>(),
            ["origin", "upstream"]
        );
        let origin: Vec<(usize, String)> = labels(&tree_rows(
            &groups[0].1,
            "remote/origin",
            0,
            &HashSet::new(),
        ));
        assert_eq!(
            origin,
            [
                (0, "feature".to_owned()),
                (1, "b".to_owned()),
                (2, "c".to_owned()),
                (1, "a".to_owned()),
                (0, "main".to_owned()),
            ]
        );
        assert_eq!(groups[1].1, [("main".to_owned(), 4)]);
    }

    #[test]
    fn the_search_keeps_the_branches_and_the_actions_that_match() {
        let branches = [
            branch("main", false, true),
            branch("feature/a", false, false),
            branch("feature/b/c", false, false),
            branch("fix-1", false, false),
        ];

        assert_eq!(matching_branches(&branches, "b/c"), [2]);
        assert_eq!(matching_branches(&branches, "ftr"), [1, 2]);
        assert!(matching_branches(&branches, "zzz").is_empty());

        let actions = [
            Command::Update,
            Command::Commit,
            Command::Push,
            Command::NewBranch,
            Command::CheckoutTag,
            Command::Stash,
            Command::Unstash,
        ];
        assert_eq!(matching("checkout", &actions), [Command::CheckoutTag]);
        assert_eq!(
            matching("stash", &actions),
            [Command::Stash, Command::Unstash]
        );
        assert_eq!(matching("commit", &actions), [Command::Commit]);
    }

    #[test]
    fn the_commit_action_carries_the_commit_picture() {
        assert_eq!(
            command_icon(Command::Commit, false),
            Some("toolwindows/commit"),
            "Commit... has an icon of its own, not the check mark of a current branch"
        );
    }

    #[test]
    fn the_favourite_branches_come_first_in_their_group() {
        let branches = [
            branch("feature/zeta", false, false),
            branch("main", false, true),
            branch("fix-1", false, false),
            branch("feature/a", false, false),
        ];
        let favourites = HashSet::from(["main".to_owned(), "feature/zeta".to_owned()]);

        let (stars, rest) = split_favourites(&[0, 1, 2, 3], &branches, &favourites);

        assert_eq!(
            stars
                .iter()
                .map(|index| branches[*index].name.as_str())
                .collect::<Vec<_>>(),
            ["feature/zeta", "main"],
            "the favourites stand above the folders, by name"
        );
        assert_eq!(
            rest.iter()
                .map(|index| branches[*index].name.as_str())
                .collect::<Vec<_>>(),
            ["feature/a", "fix-1"]
        );
    }
}
