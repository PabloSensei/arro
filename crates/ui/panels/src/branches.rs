//! The branch popup of the status bar and the title bar, as the new UI of IDEA shows it:
//! a search field over the Recent, Local and Remote branches, with the actions of a
//! branch in a submenu. The list comes from `git for-each-ref` off the UI thread.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use vcs::{Branch, Operation};
use workspace::{Settings, Workspace};

use crate::git::GitState;

const CONTEXT: &str = "BranchPopup";

actions!(branches, [Rename, NewBranch]);

/// The keys of the popup while it has the focus. They are actions of their own, so that
/// F2 renames a branch here and goes to the next problem everywhere else.
pub(crate) fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("f2", Rename, Some(CONTEXT)),
        KeyBinding::new("shift-f6", Rename, Some(CONTEXT)),
        KeyBinding::new("ctrl-alt-n", NewBranch, Some(CONTEXT)),
    ]);
}

const WIDTH: f32 = 470.;
const MAX_HEIGHT: f32 = 560.;
/// The pointer rests on a branch this long before its actions open, so a diagonal move
/// into the submenu does not flap.
const SUBMENU_DELAY: Duration = Duration::from_millis(220);
/// Recent branches the popup keeps, as IDEA does.
const RECENT_MAX: usize = 5;
/// The row of New Branch and the row of the search field, the ids the menu rows had
/// before the popup became a view. Branch rows follow from here.
const BRANCH_ROWS: usize = 2;

/// One row of the popup. Headers are not selectable.
enum Row {
    Header(&'static str),
    Separator,
    Command(Command),
    Branch {
        /// The branch in [`BranchPopup::branches`].
        index: usize,
        /// The id of a row of Local or Remote in the `menu-N` form of the tests and
        /// scripts; a row of Recent repeats a branch and has an id of its own.
        menu: Option<usize>,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Command {
    NewBranch,
    Continue,
    Abort,
    Conflicts,
    Fetch,
    Update,
    Stash,
    Unstash,
}

pub struct BranchPopup {
    git: Entity<GitState>,
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    scroll: ScrollHandle,
    /// Where the button that opened the popup is.
    position: Point<Pixels>,
    branches: Vec<Branch>,
    recent: Vec<String>,
    query: TextField,
    rows: Vec<Row>,
    selected: usize,
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
    serial: usize,
}

impl BranchPopup {
    pub(crate) fn open(
        git: Entity<GitState>,
        branches: Vec<Branch>,
        recent: Vec<String>,
        position: Point<Pixels>,
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        let weak = cx.weak_entity();
        let operation = git.read(cx).operation();
        let view = cx.new(|cx| {
            let mut popup = Self {
                git,
                workspace: weak,
                focus: cx.focus_handle(),
                scroll: ScrollHandle::new(),
                position,
                branches,
                recent,
                query: TextField::default(),
                rows: Vec::new(),
                selected: 0,
                operation,
                submenu: None,
                submenu_entries: Vec::new(),
                submenu_state: MenuState::default(),
                submenu_geometry: MenuGeometry::default(),
                rows_bounds: Rc::new(RefCell::new(HashMap::new())),
                hovered: None,
                hover_serial: 0,
                hover_task: None,
                serial: 0,
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
                Row::Command(command) => Some(MenuEntry::disabled(command_label(*command))),
                Row::Branch { index, .. } => self
                    .branches
                    .get(*index)
                    .map(|branch| MenuEntry::disabled(branch.name.clone())),
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

    /// The labels of the actions of the branch whose submenu is open, for a test.
    #[cfg(test)]
    pub(crate) fn submenu_labels(&self) -> Vec<String> {
        self.submenu_entries
            .iter()
            .filter(|entry| !entry.label.is_empty())
            .map(|entry| entry.label.to_string())
            .collect()
    }

    fn current(&self) -> String {
        self.branches
            .iter()
            .find(|branch| branch.current)
            .map(|branch| branch.name.clone())
            .unwrap_or_else(|| "HEAD".to_owned())
    }

    fn matches(&self, branch: &Branch) -> bool {
        let query = self.query.text.trim().to_lowercase();
        query.is_empty() || branch.name.to_lowercase().contains(&query)
    }

    /// Builds the rows of the list for the text of the search field.
    fn rebuild(&mut self) {
        self.rows_bounds.borrow_mut().clear();
        self.rows.clear();
        let mut rows = Vec::new();
        let mut menu = BRANCH_ROWS;
        if self.operation != Operation::None {
            rows.push(Row::Header("In Progress"));
            rows.push(Row::Command(Command::Continue));
            rows.push(Row::Command(Command::Abort));
            rows.push(Row::Command(Command::Conflicts));
        }
        let recent: Vec<usize> = self
            .recent
            .iter()
            .take(RECENT_MAX)
            .filter_map(|name| {
                self.branches
                    .iter()
                    .position(|branch| !branch.remote && &branch.name == name)
            })
            .filter(|index| self.matches(&self.branches[*index]))
            .collect();
        if !recent.is_empty() {
            rows.push(Row::Header("Recent"));
            for index in recent {
                rows.push(Row::Branch { index, menu: None });
            }
        }
        let local: Vec<usize> = (0..self.branches.len())
            .filter(|index| !self.branches[*index].remote && self.matches(&self.branches[*index]))
            .collect();
        if !local.is_empty() {
            rows.push(Row::Header("Local"));
            for index in local {
                rows.push(Row::Branch {
                    index,
                    menu: Some(menu),
                });
                menu += 1;
            }
        }
        let remote: Vec<usize> = (0..self.branches.len())
            .filter(|index| self.branches[*index].remote && self.matches(&self.branches[*index]))
            .collect();
        if !remote.is_empty() {
            rows.push(Row::Header("Remote"));
            for index in remote {
                rows.push(Row::Branch {
                    index,
                    menu: Some(menu),
                });
                menu += 1;
            }
        }
        if rows.is_empty() {
            rows.push(Row::Header("No branches match"));
        }
        rows.push(Row::Separator);
        rows.push(Row::Command(Command::NewBranch));
        rows.push(Row::Command(Command::Fetch));
        rows.push(Row::Command(Command::Update));
        rows.push(Row::Command(Command::Stash));
        rows.push(Row::Command(Command::Unstash));
        self.rows = rows;
        self.keep_selection();
    }

    /// The selection stays on its branch across a change of the search, or moves to the
    /// current branch, or to the first row.
    fn keep_selection(&mut self) {
        let selected_name = self
            .rows
            .get(self.selected)
            .and_then(|row| self.branch_name(row));
        let current = self.current();
        let wanted = selected_name.unwrap_or(current);
        let found = self.rows.iter().position(|row| {
            self.branch_name(row).is_some_and(|name| name == wanted) && row_selectable(row)
        });
        self.selected = found
            .or_else(|| self.rows.iter().position(row_selectable))
            .unwrap_or(0);
    }

    fn branch_name(&self, row: &Row) -> Option<String> {
        match row {
            Row::Branch { index, .. } => Some(self.branches.get(*index)?.name.clone()),
            _ => None,
        }
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

    /// Right: the actions of the selected branch.
    fn open_submenu(&mut self, cx: &mut Context<Self>) {
        self.open_submenu_at(self.selected, cx);
    }

    fn open_submenu_at(&mut self, row: usize, cx: &mut Context<Self>) {
        let Some(Row::Branch { index, .. }) = self.rows.get(row) else {
            return;
        };
        let Some(branch) = self.branches.get(*index) else {
            return;
        };
        let entries = self.actions_of(branch);
        if entries.is_empty() {
            return;
        }
        self.submenu = Some(row);
        self.submenu_entries = entries;
        self.submenu_state.clear();
        self.submenu_state.hover(&[0]);
        self.serial += 1;
        cx.notify();
    }

    fn close_submenu(&mut self, cx: &mut Context<Self>) -> bool {
        if self.submenu.take().is_none() {
            return false;
        }
        self.submenu_entries.clear();
        self.submenu_state.clear();
        self.serial += 1;
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
        MenuEntry::new("Push...", move |_, cx| {
            git.update(cx, |git, cx| git.push(cx));
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
        let position = self.position;
        let git = self.git.clone();
        self.dismiss(window, cx);
        git.update(cx, |git, cx| match command {
            Command::NewBranch => git.ask_branch_name(window, cx),
            Command::Continue => {
                let message = git.merge_message().to_owned();
                git.finish_operation(message, cx);
            }
            Command::Abort => git.abort_operation(cx),
            Command::Conflicts => git.open_conflicts(cx),
            Command::Fetch => git.fetch(cx),
            Command::Update => git.update_project(position, cx),
            Command::Stash => git.ask_stash_message(window, cx),
            Command::Unstash => git.show_stashes(position, cx),
        })
    }

    /// A click on a branch checks it out; the current branch only closes the popup.
    fn checkout_row(&mut self, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Row::Branch { index, .. }) = self.rows.get(row) else {
            return;
        };
        let Some(branch) = self.branches.get(*index) else {
            return;
        };
        let (name, remote, current) = (branch.name.clone(), branch.remote, branch.current);
        self.dismiss(window, cx);
        if current {
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
        let name = self
            .rows
            .get(self.selected)
            .and_then(|row| match row {
                Row::Branch { index, .. } => self.branches.get(*index),
                _ => None,
            })
            .filter(|branch| !branch.remote)
            .map(|branch| branch.name.clone())
            .unwrap_or_else(|| self.current());
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
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                "up" => {
                    self.submenu_state.move_up(&self.submenu_entries);
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
                self.open_submenu(cx);
                cx.stop_propagation();
            }
            "left" => {
                if !self.close_submenu(cx) {
                    self.move_selection(-1, cx);
                }
                cx.stop_propagation();
            }
            "enter" => {
                match self.rows.get(self.selected) {
                    Some(Row::Branch { .. }) => self.checkout_row(self.selected, window, cx),
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
        if !matches!(self.rows.get(row), Some(Row::Branch { .. })) {
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

    fn command_row(
        &self,
        index: usize,
        command: Command,
        label: &str,
        icon_name: &'static str,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let selector = match command {
            Command::NewBranch => "menu-0".to_owned(),
            Command::Continue => "branch-continue".to_owned(),
            Command::Abort => "branch-abort".to_owned(),
            Command::Conflicts => "branch-conflicts".to_owned(),
            Command::Fetch => "branch-fetch".to_owned(),
            Command::Update => "branch-update".to_owned(),
            Command::Stash => "branch-stash".to_owned(),
            Command::Unstash => "branch-unstash".to_owned(),
        };
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
                    .child(icon(icon_name, theme)),
            )
            .child(label.to_owned())
            .on_click(cx.listener(move |this, _, window, cx| this.run_command(command, window, cx)))
    }

    #[allow(clippy::too_many_arguments)]
    fn branch_row(
        &self,
        row: usize,
        index: usize,
        menu: Option<usize>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let branch = &self.branches[index];
        let current = branch.current;
        let ahead = branch.ahead;
        let behind = branch.behind;
        let counts = match (ahead, behind) {
            (0, 0) => String::new(),
            (ahead, 0) => format!("↑{ahead}"),
            (0, behind) => format!("↓{behind}"),
            (ahead, behind) => format!("↑{ahead} ↓{behind}"),
        };
        let chosen = self.selected == row;
        let open = self.submenu == Some(row);
        let label = branch.name.clone();
        let selector = match menu {
            Some(menu) => format!("menu-{menu}"),
            None => format!("branch-recent-{row}"),
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
            .relative()
            .flex()
            .items_center()
            .gap(px(SPACE_S))
            .flex_none()
            .h(px(ROW_HEIGHT))
            .px(px(ROW_PADDING_X))
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
                    .children(current.then(|| icon_sized("general/check", 12., theme))),
            )
            .child(div().flex_1().min_w_0().truncate().child(label))
            .child(
                div()
                    .flex_none()
                    .text_color(theme.text_secondary)
                    .child(counts),
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
}

fn row_selectable(row: &Row) -> bool {
    matches!(row, Row::Command(_) | Row::Branch { .. })
}

fn command_label(command: Command) -> &'static str {
    match command {
        Command::NewBranch => "New Branch...",
        Command::Continue => "Continue",
        Command::Abort => "Abort",
        Command::Conflicts => "Conflicts...",
        Command::Fetch => "Fetch",
        Command::Update => "Update Project",
        Command::Stash => "Stash Changes...",
        Command::Unstash => "Unstash Changes...",
    }
}

impl Render for BranchPopup {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let viewport = window.viewport_size();
        let scale = cx
            .try_global::<Settings>()
            .map_or(1., |settings| settings.ui_scale);
        let width = px(WIDTH);
        let left = self.position.x.clamp(
            px(SPACE_M),
            (viewport.width - width - px(SPACE_M)).max(px(SPACE_M)),
        );
        let top = self.position.y.clamp(
            px(SPACE_M),
            (viewport.height - px(MAX_HEIGHT) - px(SPACE_M)).max(px(SPACE_M)),
        );
        let mut list = Vec::new();
        for at in 0..self.rows.len() {
            match &self.rows[at] {
                Row::Header(label) => list.push(self.header(label, &theme).into_any_element()),
                Row::Separator => list.push(
                    div()
                        .h(px(1.))
                        .mx(px(SPACE_M))
                        .my(px(SPACE_S))
                        .bg(theme.popup_border)
                        .into_any_element(),
                ),
                Row::Command(command) => {
                    let (label, icon_name) = match command {
                        Command::NewBranch => ("New Branch...", "general/add"),
                        Command::Continue => ("Continue", "general/vcs"),
                        Command::Abort => ("Abort", "general/delete"),
                        Command::Conflicts => ("Conflicts...", "general/vcs"),
                        Command::Fetch => ("Fetch", "general/vcs"),
                        Command::Update => ("Update Project", "general/vcs"),
                        Command::Stash => ("Stash Changes...", "general/vcs"),
                        Command::Unstash => ("Unstash Changes...", "general/vcs"),
                    };
                    list.push(
                        self.command_row(at, *command, label, icon_name, &theme, cx)
                            .into_any_element(),
                    );
                }
                Row::Branch { index, menu } => {
                    let (branch, menu) = (*index, *menu);
                    list.push(
                        self.branch_row(at, branch, menu, &theme, cx)
                            .into_any_element(),
                    );
                }
            }
        }
        let empty = self.rows.is_empty();
        let search = div()
            .flex()
            .items_center()
            .flex_none()
            .gap(px(SPACE_S))
            .h(px(CONTROL_HEIGHT))
            .mb(px(SPACE_XS))
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
                        16.,
                        true,
                    )),
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
            let size = menu_size(window, &self.submenu_entries, px(UI_SIZE * scale));
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
            let menu = menu(entries, state, geometry, &callbacks, viewport, &theme);
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
                .child(enter(("branches-enter", self.serial), body)),
        )
        .with_priority(1);
        div().absolute().size_full().child(backdrop).child(popup)
    }
}
