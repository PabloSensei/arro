//! The Commit tool window: the changed files with check boxes, the message, Commit and
//! Commit and Push. A double click on a file shows its diff.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use editor_view::EditorView;
use lang_api::Severity;
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use vcs::{CommitOptions, CommitRequest};
use workspace::{COMMIT_TOOL, Settings, Workspace};

use crate::changelists::{Changelists, Grouping};
use crate::diff_view::DiffView;
use crate::git::{GitState, Kind, relative_of};
use crate::inspect::ActiveInspect;
use crate::project_state::project_key;
use crate::prompt::{Prompt, TextField};
use crate::shelf::{Shelf, ShelfStore, UnshelveDialog};

const GROUP: &str = "left-panel";
const ROW_MARGIN: f32 = SPACE_L;
const MESSAGE_HEIGHT: f32 = 84.;
const CONTEXT: &str = "CommitPanel";

actions!(
    commit_panel,
    [
        RevertSelected,
        ToggleAmend,
        MessageHistory,
        MoveToChangelist,
        SetActiveChangelist,
        Rename,
        GroupByDirectory,
        GroupByModule,
        ShowCommitOptions,
        Shelve,
        Unshelve,
        ShowDiff
    ]
);

macro_rules! commit_keys {
    ($(($keys:literal, $action:ident, $idea:expr, $mac:expr)),* $(,)?) => {
        /// Ctrl+Alt+Z, Alt+M, Ctrl+M and the keys of the change lists and the shelves.
        fn bind_keys(cx: &mut App) {
            bind_keys_for(cx, keymap::platform());
        }

        /// Binds the keys of one platform, so that a test reads the macOS table on every
        /// runner.
        fn bind_keys_for(cx: &mut App, platform: keymap::Platform) {
            $(
                let row = workspace::KeyRow {
                    keys: $keys,
                    mac: $mac,
                    action: concat!("commit_panel::", stringify!($action)),
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
                action: concat!("commit_panel::", stringify!($action)),
                context: Some(CONTEXT),
                idea: $idea,
            }),*
        ];
    };
}

commit_keys![
    (
        "ctrl-alt-z",
        RevertSelected,
        Some("ChangesView.Revert"),
        None
    ),
    (
        "alt-m",
        ToggleAmend,
        Some("Vcs.ToggleAmendCommitMode"),
        None
    ),
    (
        "ctrl-m",
        MessageHistory,
        Some("Vcs.ShowMessageHistory"),
        None
    ),
    (
        "alt-shift-m",
        MoveToChangelist,
        Some("ChangesView.Move"),
        None
    ),
    (
        "ctrl-space",
        SetActiveChangelist,
        Some("ChangesView.SetDefault"),
        None
    ),
    ("f2", Rename, Some("ChangesView.Rename"), None),
    ("shift-f6", Rename, Some("ChangesView.Rename"), None),
    (
        "ctrl-alt-p",
        GroupByDirectory,
        Some("ChangesView.GroupBy.Directory"),
        None
    ),
    (
        "ctrl-alt-m",
        GroupByModule,
        Some("ChangesView.GroupBy.Module"),
        None
    ),
    (
        "ctrl-o",
        ShowCommitOptions,
        Some("ChangesView.ShowCommitOptions"),
        None
    ),
    (
        "ctrl-shift-u",
        Unshelve,
        Some("ShelveChanges.UnshelveWithDialog"),
        None
    ),
    ("ctrl-d", ShowDiff, Some("Diff.ShowDiff"), None),
];

/// F2 and Shift+F6 rename the selected shelf on the Shelf tab: the same keys and action.
#[cfg(test)]
pub(crate) const SHELF_RENAME: &[workspace::KeyRow] = &[workspace::KeyRow {
    keys: "f2",
    mac: None,
    action: "commit_panel::Rename",
    context: Some(CONTEXT),
    idea: Some("ShelvedChanges.Rename"),
}];

/// Which tab of the Commit tool window is shown.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tab {
    #[default]
    Commit,
    Shelf,
}

/// An open editor that may hold text the file on disk does not have yet.
struct OpenFile {
    /// The path the editor writes.
    path: PathBuf,
    /// The path as git names it in the status.
    relative: String,
    dirty: bool,
}

/// Why the Commit button did not run git. The message in the field stays as the user left it.
#[derive(Clone, Debug, PartialEq, Eq)]
enum CommitProblem {
    NoMessage,
    NoFiles,
    Save(String),
}

impl CommitProblem {
    fn text(&self) -> &str {
        match self {
            Self::NoMessage => "Write a commit message first",
            Self::NoFiles => "Tick at least one file",
            Self::Save(message) => message,
        }
    }
}

/// Builds the request for git from the message and the ticked files, writing the open
/// editors of those files first. One failed save stops the commit: nothing may go to git
/// while the user sees text the disk does not have.
#[cfg(test)]
fn prepare_commit(
    message: &str,
    paths: &[String],
    open: Vec<OpenFile>,
    save: impl FnMut(&Path) -> Result<(), String>,
) -> Result<CommitRequest, CommitProblem> {
    prepare_commit_with(message, paths, open, false, save)
}

/// Like [`prepare_commit`], with `amend` saying whether the last commit is rewritten.
fn prepare_commit_with(
    message: &str,
    paths: &[String],
    open: Vec<OpenFile>,
    amend: bool,
    mut save: impl FnMut(&Path) -> Result<(), String>,
) -> Result<CommitRequest, CommitProblem> {
    let message = message.trim();
    if message.is_empty() {
        return Err(CommitProblem::NoMessage);
    }
    if paths.is_empty() {
        return Err(CommitProblem::NoFiles);
    }
    for file in open {
        if !file.dirty || !paths.contains(&file.relative) {
            continue;
        }
        save(&file.path).map_err(CommitProblem::Save)?;
    }
    Ok(CommitRequest {
        message: message.to_string(),
        paths: paths.to_vec(),
        amend,
    })
}

/// The diff of the shelf file on screen, read in the background.
#[derive(Clone)]
struct ShelfPreview {
    base: String,
    current: String,
    hunks: Vec<vcs::Hunk>,
}

/// One row of the tree of a shelf: a folder or one of its files.
struct ShelfEntry {
    depth: usize,
    label: String,
    file: Option<usize>,
}

/// The files of a shelf as a tree: folders first, then the files inside them.
fn shelf_entries(files: &[String]) -> Vec<ShelfEntry> {
    let mut paths: Vec<(usize, Vec<&str>)> = files
        .iter()
        .enumerate()
        .map(|(index, file)| (index, file.split('/').collect()))
        .collect();
    paths.sort_by(|a, b| a.1.cmp(&b.1));
    let mut shown: HashSet<String> = HashSet::new();
    let mut entries = Vec::new();
    for (index, parts) in paths {
        for depth in 1..parts.len() {
            let folder = parts[..depth].join("/");
            if shown.insert(folder) {
                entries.push(ShelfEntry {
                    depth: depth - 1,
                    label: parts[depth - 1].to_string(),
                    file: None,
                });
            }
        }
        entries.push(ShelfEntry {
            depth: parts.len().saturating_sub(1),
            label: parts.last().unwrap_or(&"").to_string(),
            file: Some(index),
        });
    }
    entries
}

/// A commit waiting for the answer about the problems of the analysis.
struct PendingCommit {
    request: CommitRequest,
    push: bool,
    options: CommitOptions,
}

pub struct CommitPanel {
    git: Entity<GitState>,
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    message: TextField,
    /// Files the user took the tick off. A new file starts ticked.
    unchecked: HashSet<String>,
    selected: Option<usize>,
    scroll_files: ScrollHandle,
    scroll_shelves: ScrollHandle,
    scroll_shelf_files: ScrollHandle,
    scroll_shelf_diff: ScrollHandle,
    /// The keys go to the list of files rather than to the message field.
    files_focused: bool,
    /// Alt+M: the next commit rewrites the last one.
    amend: bool,
    /// The switches of the commit options popup.
    sign_off: bool,
    author: Option<String>,
    /// Run Inspect Code over the chosen files before the commit.
    analysis: bool,
    tab: Tab,
    lists: Changelists,
    /// The store key of [`CommitPanel::lists`].
    lists_key: String,
    store: Option<std::sync::Arc<store::Store>>,
    shelves: Vec<Shelf>,
    shelf_store: ShelfStore,
    shelf_selected: Option<usize>,
    shelf_file: Option<usize>,
    shelf_preview: Option<ShelfPreview>,
    /// The serial of the preview read in the background, so a late one is dropped.
    preview_serial: u64,
    /// The commit whose analysis is running.
    analyzing: Option<PendingCommit>,
    analysis_observe: Option<Subscription>,
    /// The commit waiting for Review or Commit anyway.
    pending_commit: Option<PendingCommit>,
    /// The window of the panel, for the question after the analysis.
    window: AnyWindowHandle,
    row_bounds: Rc<std::cell::RefCell<HashMap<usize, Bounds<Pixels>>>>,
    root: PathBuf,
    _git: Subscription,
}

impl CommitPanel {
    pub fn create(
        git: Entity<GitState>,
        workspace: WeakEntity<Workspace>,
        root: PathBuf,
        state_dir: PathBuf,
        store: Option<std::sync::Arc<store::Store>>,
        window: &Window,
        cx: &mut App,
    ) -> Entity<Self> {
        bind_keys(cx);
        let lists_key = format!("changelists:{}", project_key(&root));
        let lists = Changelists::load(store.as_deref(), &lists_key);
        let shelf_store = ShelfStore::new(&state_dir, &root);
        let shelves = shelf_store.list();
        let window = window.window_handle();
        cx.new(|cx| {
            let observe = cx.observe(&git, |this: &mut Self, git, cx| {
                let status = git.read(cx).status().clone();
                this.status_changed(&status, cx);
                cx.notify();
            });
            Self {
                git,
                workspace,
                focus: cx.focus_handle(),
                message: TextField::default(),
                unchecked: HashSet::new(),
                selected: None,
                scroll_files: ScrollHandle::new(),
                scroll_shelves: ScrollHandle::new(),
                scroll_shelf_files: ScrollHandle::new(),
                scroll_shelf_diff: ScrollHandle::new(),
                files_focused: false,
                amend: false,
                sign_off: false,
                author: None,
                analysis: cx
                    .try_global::<Settings>()
                    .is_some_and(|settings| settings.git_analyze_before_commit),
                tab: Tab::Commit,
                lists,
                lists_key,
                store,
                shelves,
                shelf_store,
                shelf_selected: None,
                shelf_file: None,
                shelf_preview: None,
                preview_serial: 0,
                analyzing: None,
                analysis_observe: None,
                pending_commit: None,
                window,
                row_bounds: Rc::new(std::cell::RefCell::new(HashMap::new())),
                root,
                _git: observe,
            }
        })
    }

    pub fn focus(&self) -> FocusHandle {
        self.focus.clone()
    }

    pub fn workspace(&self) -> WeakEntity<Workspace> {
        self.workspace.clone()
    }

    /// The message typed so far, for scripts.
    pub fn message(&self) -> &str {
        &self.message.text
    }

    /// Whether the next commit rewrites the last one, for scripts and tests.
    #[cfg(test)]
    pub fn amending(&self) -> bool {
        self.amend
    }

    #[cfg(test)]
    pub fn tab(&self) -> Tab {
        self.tab
    }

    #[cfg(test)]
    pub fn active_changelist(&self) -> &str {
        &self.lists.active
    }

    #[cfg(test)]
    pub fn changelist_names(&self) -> &[String] {
        self.lists.names()
    }

    #[cfg(test)]
    pub fn grouping(&self) -> Grouping {
        self.lists.grouping
    }

    #[cfg(test)]
    pub fn shelves(&self) -> &[Shelf] {
        &self.shelves
    }

    #[cfg(test)]
    pub fn signs_off(&self) -> bool {
        self.sign_off
    }

    /// Whether the commit runs Inspect Code first, for tests.
    #[cfg(test)]
    pub fn analysis_enabled(&self) -> bool {
        self.analysis
    }

    /// Whether the file belongs to a list other than the active one.
    #[cfg(test)]
    pub fn assigned_to_another_list(&self, path: &str) -> bool {
        self.lists.assigned.contains_key(path)
    }

    /// The heading of the file in the grouping of the window, for tests.
    #[cfg(test)]
    pub fn group_of_path(&self, path: &str, cx: &App) -> String {
        self.group_of(path, cx)
    }

    /// How many hunks the preview of the shelf file shows, for tests.
    #[cfg(test)]
    pub fn shelf_preview_hunks(&self) -> usize {
        self.shelf_preview
            .as_ref()
            .map_or(0, |preview| preview.hunks.len())
    }

    pub fn set_message(&mut self, text: &str, cx: &mut Context<Self>) {
        self.message.text = text.to_string();
        cx.notify();
    }

    pub fn set_checked(&mut self, path: &str, checked: bool, cx: &mut Context<Self>) {
        if checked {
            self.unchecked.remove(path);
        } else {
            self.unchecked.insert(path.to_string());
        }
        cx.notify();
    }

    /// Alt+M: the last commit is rewritten with the message of the field, which starts as
    /// the message of that commit.
    fn toggle_amend(&mut self, cx: &mut Context<Self>) {
        self.amend = !self.amend;
        if !self.amend {
            self.message.text.clear();
            cx.notify();
            return;
        }
        let Some(repo) = self.git.read(cx).repo() else {
            cx.notify();
            return;
        };
        let job = cx.background_executor().spawn(async move {
            repo.log(1, 0)
                .ok()
                .and_then(|mut commits| commits.pop())
                .map(|commit| commit.subject)
        });
        cx.spawn(async move |this, cx| {
            let message = job.await;
            this.update(cx, |this, cx| {
                if this.amend
                    && let Some(message) = message
                {
                    this.message.text = message;
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Ctrl+M: the messages of the last commits as a popup.
    fn show_message_history(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(repo) = self.git.read(cx).repo() else {
            return;
        };
        let job = cx.background_executor().spawn(async move {
            repo.log(20, 0)
                .map(|commits| {
                    commits
                        .into_iter()
                        .map(|commit| commit.subject)
                        .filter(|subject| !subject.trim().is_empty())
                        .collect::<Vec<_>>()
                })
                .map_err(|error| error.to_string())
        });
        let me = cx.weak_entity();
        let workspace = self.workspace.clone();
        cx.spawn(async move |this, cx| {
            let result = job.await;
            this.update(cx, |this, cx| match result {
                Ok(messages) if messages.is_empty() => {
                    this.git.update(cx, |git, cx| {
                        git.tell("Message History", "There are no earlier messages", cx)
                    });
                }
                Ok(messages) => {
                    let me = me.clone();
                    let entries = messages
                        .into_iter()
                        .map(|message| {
                            let me = me.clone();
                            let shown = message.clone();
                            MenuEntry::new(shown, move |_, cx| {
                                me.update(cx, |this, cx| {
                                    this.set_message(&message, cx);
                                })
                                .ok();
                            })
                            .icon("general/vcs")
                        })
                        .collect();
                    workspace
                        .update(cx, |workspace, cx| {
                            workspace.show_menu(position, entries, cx)
                        })
                        .ok();
                }
                Err(message) => this
                    .git
                    .update(cx, |git, cx| git.report("Message History", message, cx)),
            })
            .ok();
        })
        .detach();
    }

    /// Ctrl+Alt+Z: the selected file, or the ticked ones, go back to the last commit.
    fn revert_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths: Vec<String> = match self.selected {
            Some(index) => self
                .git
                .read(cx)
                .status()
                .files
                .get(index)
                .map(|file| vec![file.path.clone()])
                .unwrap_or_default(),
            None => self.checked_paths(cx),
        };
        if paths.is_empty() {
            return;
        }
        let count = paths.len();
        let question = if count == 1 {
            format!("Roll back {}?", paths[0])
        } else {
            format!("Roll back {count} files?")
        };
        let git = self.git.clone();
        self.workspace
            .update(cx, |workspace, cx| {
                Prompt::ask(
                    "Rollback",
                    &question,
                    move |cx| {
                        git.update(cx, |git, cx| git.revert_paths(paths.clone(), cx));
                    },
                    workspace,
                    window,
                    cx,
                );
            })
            .ok();
    }

    /// The ticks of the files the active list shows.
    fn checked_paths(&self, cx: &App) -> Vec<String> {
        let files = &self.git.read(cx).status().files;
        self.visible_files(cx)
            .into_iter()
            .filter_map(|index| files.get(index))
            .filter(|file| !self.unchecked.contains(&file.path))
            .map(|file| file.path.clone())
            .collect()
    }

    /// The indices of the status the active change list shows, in the order of the status.
    fn visible_files(&self, cx: &App) -> Vec<usize> {
        let git = self.git.read(cx);
        git.status()
            .files
            .iter()
            .enumerate()
            .filter(|(_, file)| self.lists.list_of(&file.path) == self.lists.active)
            .map(|(index, _)| index)
            .collect()
    }

    /// The files the list actions work on: the file under the cursor, or every ticked one.
    fn target_paths(&self, cx: &App) -> Vec<String> {
        let visible = self.visible_files(cx);
        if let Some(selected) = self.selected
            && visible.contains(&selected)
            && let Some(file) = self.git.read(cx).status().files.get(selected)
        {
            return vec![file.path.clone()];
        }
        self.checked_paths(cx)
    }

    fn status_changed(&mut self, status: &vcs::Status, cx: &mut Context<Self>) {
        let paths: HashSet<String> = status.files.iter().map(|file| file.path.clone()).collect();
        if self.lists.retain_paths(&paths) {
            self.save_lists();
        }
        if self.selected.is_some() && !self.visible_files(cx).contains(&self.selected.unwrap_or(0))
        {
            self.selected = None;
        }
    }

    fn save_lists(&self) {
        self.lists.save(self.store.as_deref(), &self.lists_key);
    }

    pub fn set_tab(&mut self, tab: Tab, cx: &mut Context<Self>) {
        if self.tab == tab {
            return;
        }
        self.tab = tab;
        self.files_focused = false;
        self.selected = None;
        if tab == Tab::Shelf {
            self.reload_shelves(cx);
            if self.shelf_selected.is_none() && !self.shelves.is_empty() {
                self.shelf_selected = Some(0);
                self.shelf_file = Some(0);
            }
            self.refresh_shelf_preview(cx);
        }
        cx.notify();
    }

    fn reload_shelves(&mut self, cx: &mut Context<Self>) {
        self.shelves = self.shelf_store.list();
        if self
            .shelf_selected
            .is_some_and(|index| index >= self.shelves.len())
        {
            self.shelf_selected = None;
        }
        self.shelf_file = None;
        self.shelf_preview = None;
        cx.notify();
    }

    fn select_shelf(&mut self, index: usize, cx: &mut Context<Self>) {
        self.shelf_selected = Some(index);
        self.shelf_file = self
            .shelves
            .get(index)
            .and_then(|shelf| (!shelf.files.is_empty()).then_some(0));
        self.refresh_shelf_preview(cx);
        cx.notify();
    }

    fn select_shelf_file(&mut self, index: usize, cx: &mut Context<Self>) {
        self.shelf_file = Some(index);
        self.refresh_shelf_preview(cx);
        cx.notify();
    }

    /// Reads the diff of the selected shelf file in the background for the preview.
    fn refresh_shelf_preview(&mut self, cx: &mut Context<Self>) {
        self.preview_serial += 1;
        let serial = self.preview_serial;
        let Some(shelf) = self.selected_shelf() else {
            self.shelf_preview = None;
            return;
        };
        let Some(file) = self
            .shelf_file
            .and_then(|index| shelf.files.get(index))
            .cloned()
        else {
            self.shelf_preview = None;
            return;
        };
        let Some(repo) = self.git.read(cx).repo() else {
            self.shelf_preview = None;
            return;
        };
        let absolute = self.root.join(&file);
        let patch = shelf.patch.clone();
        let job = cx.background_executor().spawn(async move {
            let relative = relative_of(repo.root(), &absolute);
            repo.shelf_diff(&patch, &relative).ok().flatten()
        });
        cx.spawn(async move |this, cx| {
            let diff = job.await;
            this.update(cx, |this, cx| {
                if serial != this.preview_serial {
                    return;
                }
                this.shelf_preview = diff.map(|diff| ShelfPreview {
                    base: diff.base,
                    current: diff.current,
                    hunks: diff.hunks,
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Ctrl+D on a sheet file, or a double click: the diff of `HEAD` and the shelf.
    fn open_shelf_diff(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(shelf) = self.selected_shelf() else {
            return;
        };
        let Some(file) = shelf.files.get(index).cloned() else {
            return;
        };
        let path = self.root.join(&file);
        DiffView::open_shelf(
            self.git.clone(),
            self.workspace.clone(),
            shelf.name.clone(),
            path,
            shelf.patch.clone(),
            window,
            cx,
        );
    }

    fn show_diff(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tab == Tab::Shelf {
            if let Some(index) = self.shelf_file {
                self.open_shelf_diff(index, window, cx);
            }
            return;
        }
        if let Some(index) = self.selected {
            self.open_diff(index, window, cx);
        }
    }

    fn add_changelist(&mut self, name: String, cx: &mut Context<Self>) {
        if self.lists.add_list(&name) {
            self.save_lists();
            self.selected = None;
            self.unchecked.clear();
            cx.notify();
        } else {
            let name = name.trim().to_string();
            self.git.update(cx, |git, cx| {
                git.report(
                    "Changelists",
                    format!("A list named {name} exists already or the name is empty"),
                    cx,
                )
            });
        }
    }

    fn ask_new_changelist(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let me = cx.weak_entity();
        self.workspace
            .update(cx, |workspace, cx| {
                Prompt::show(
                    "New Changelist",
                    "Changelist name",
                    move |name, cx| {
                        me.update(cx, |this, cx| this.add_changelist(name, cx)).ok();
                    },
                    workspace,
                    window,
                    cx,
                );
            })
            .ok();
    }

    fn set_active_list(&mut self, name: String, cx: &mut Context<Self>) {
        if self.lists.set_active(&name) {
            self.save_lists();
            self.selected = None;
            self.unchecked.clear();
            cx.notify();
        }
    }

    fn ask_rename_changelist(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tab == Tab::Shelf {
            self.ask_rename_shelf(window, cx);
            return;
        }
        let name = self.lists.active.clone();
        let me = cx.weak_entity();
        self.workspace
            .update(cx, |workspace, cx| {
                Prompt::show_with(
                    "Rename Changelist",
                    &name,
                    move |name, cx| {
                        me.update(cx, |this, cx| {
                            if this.lists.rename_active(&name) {
                                this.save_lists();
                                this.selected = None;
                                cx.notify();
                            }
                        })
                        .ok();
                    },
                    workspace,
                    window,
                    cx,
                );
            })
            .ok();
    }

    fn delete_changelist(&mut self, cx: &mut Context<Self>) {
        if self.tab == Tab::Shelf {
            return;
        }
        if self.lists.delete_active() {
            self.save_lists();
            self.selected = None;
            self.unchecked.clear();
            cx.notify();
        } else {
            self.git.update(cx, |git, cx| {
                git.report(
                    "Changelists",
                    "The last changelist cannot be deleted".to_string(),
                    cx,
                )
            });
        }
    }

    fn move_to_list(&mut self, list: String, cx: &mut Context<Self>) {
        let paths = self.target_paths(cx);
        if paths.is_empty() {
            self.git.update(cx, |git, cx| {
                git.report("Changelists", "Choose a file first".to_string(), cx)
            });
            return;
        }
        self.lists.move_paths(&paths, &list);
        self.save_lists();
        self.selected = None;
        let count = paths.len();
        self.git.update(cx, |git, cx| {
            git.tell(
                "Changelists",
                format!("Moved {count} file(s) to {list}"),
                cx,
            )
        });
        cx.notify();
    }

    fn set_grouping(&mut self, grouping: Grouping, cx: &mut Context<Self>) {
        self.lists.grouping = grouping;
        self.save_lists();
        cx.notify();
    }

    fn show_changelist_menu(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let me = cx.weak_entity();
        let mut entries = Vec::new();
        for name in self.lists.names() {
            let me = me.clone();
            let list = name.clone();
            let label = if *name == self.lists.active {
                format!("{name}  (active)")
            } else {
                name.clone()
            };
            entries.push(
                MenuEntry::new(label, move |_, cx| {
                    let list = list.clone();
                    me.update(cx, |this, cx| this.set_active_list(list, cx))
                        .ok();
                })
                .icon("general/vcs"),
            );
        }
        entries.push(MenuEntry::separator());
        {
            let me = me.clone();
            entries.push(
                MenuEntry::new("New Changelist...", move |window, cx| {
                    me.update(cx, |this, cx| this.ask_new_changelist(window, cx))
                        .ok();
                })
                .icon("general/add"),
            );
        }
        {
            let me = me.clone();
            entries.push(
                MenuEntry::new("Move to Another Changelist...", move |window, cx| {
                    let position = window.mouse_position();
                    me.update(cx, |this, cx| this.show_move_menu(position, cx))
                        .ok();
                })
                .icon("general/vcs"),
            );
        }
        {
            let me = me.clone();
            entries.push(
                MenuEntry::new("Rename...", move |window, cx| {
                    me.update(cx, |this, cx| this.ask_rename_changelist(window, cx))
                        .ok();
                })
                .icon("general/vcs"),
            );
        }
        {
            let me = me.clone();
            entries.push(
                MenuEntry::new("Delete", move |_, cx| {
                    me.update(cx, |this, cx| this.delete_changelist(cx)).ok();
                })
                .icon("general/delete"),
            );
        }
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.show_menu(position, entries, cx)
            })
            .ok();
    }

    fn show_move_menu(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let me = cx.weak_entity();
        let mut entries = Vec::new();
        for name in self.lists.names() {
            if *name == self.lists.active {
                continue;
            }
            let me = me.clone();
            let list = name.clone();
            entries.push(
                MenuEntry::new(name.clone(), move |_, cx| {
                    let list = list.clone();
                    me.update(cx, |this, cx| this.move_to_list(list, cx)).ok();
                })
                .icon("general/vcs"),
            );
        }
        if entries.is_empty() {
            entries.push(MenuEntry::disabled("No other changelist"));
        }
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.show_menu(position, entries, cx)
            })
            .ok();
    }

    fn show_commit_options(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let me = cx.weak_entity();
        let amend = if self.amend { "✓ Amend" } else { "Amend" };
        let sign_off = if self.sign_off {
            "✓ Sign-off"
        } else {
            "Sign-off"
        };
        let author = self.author.as_ref().map_or_else(
            || "Author...".to_string(),
            |author| format!("Author: {author}"),
        );
        let mut entries = Vec::new();
        {
            let me = me.clone();
            entries.push(MenuEntry::new(amend, move |_, cx| {
                me.update(cx, |this, cx| this.toggle_amend(cx)).ok();
            }));
        }
        {
            let me = me.clone();
            entries.push(MenuEntry::new(sign_off, move |_, cx| {
                me.update(cx, |this, cx| {
                    this.sign_off = !this.sign_off;
                    cx.notify();
                })
                .ok();
            }));
        }
        {
            let me = me.clone();
            entries.push(MenuEntry::new(author, move |window, cx| {
                me.update(cx, |this, cx| this.ask_author(window, cx)).ok();
            }));
        }
        let analysis = self
            .workspace
            .read_with(cx, |workspace, _| {
                workspace
                    .commands()
                    .get(keymap::workspace::INSPECT_CODE)
                    .is_some()
            })
            .unwrap_or(false);
        if analysis {
            let label = if self.analysis {
                "✓ Run code analysis"
            } else {
                "Run code analysis"
            };
            let me = me.clone();
            entries.push(MenuEntry::new(label, move |_, cx| {
                me.update(cx, |this, cx| {
                    this.analysis = !this.analysis;
                    cx.notify();
                })
                .ok();
            }));
        }
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.show_menu(position, entries, cx)
            })
            .ok();
    }

    fn ask_author(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let me = cx.weak_entity();
        let current = self.author.clone().unwrap_or_default();
        self.workspace
            .update(cx, |workspace, cx| {
                Prompt::show_with(
                    "Commit Author",
                    &current,
                    move |author, cx| {
                        let author = author.trim().to_string();
                        me.update(cx, |this, cx| {
                            this.author = (!author.is_empty()).then_some(author);
                            cx.notify();
                        })
                        .ok();
                    },
                    workspace,
                    window,
                    cx,
                );
            })
            .ok();
    }

    /// Writes the open editors of the files, so that the patch holds what the user sees.
    fn save_paths(&self, paths: &[String], cx: &mut Context<Self>) -> Result<(), String> {
        for path in paths {
            let absolute = self.root.join(path);
            self.workspace
                .update(cx, |workspace, cx| workspace.save_editor_now(&absolute, cx))
                .unwrap_or_else(|_| Err("the window is gone".to_string()))?;
        }
        Ok(())
    }

    fn shelve(&mut self, cx: &mut Context<Self>) {
        let paths = self.checked_paths(cx);
        if paths.is_empty() {
            self.git.update(cx, |git, cx| {
                git.report("Shelve", "Tick at least one file".to_string(), cx)
            });
            return;
        }
        if let Err(message) = self.save_paths(&paths, cx) {
            self.git
                .update(cx, |git, cx| git.report("Shelve", message, cx));
            return;
        }
        let name = self.message.text.trim();
        let name = self
            .shelf_store
            .unique_name(if name.is_empty() { "Changes" } else { name });
        let store = self.shelf_store.clone();
        let files = paths.clone();
        let shown = name.clone();
        let me = cx.weak_entity();
        self.git.update(cx, |git, cx| {
            git.run(
                "Shelve",
                move |repo| {
                    let patch = repo.shelve(&paths)?;
                    // The shelf is on disk before the files go back to HEAD, so a failed
                    // write loses nothing.
                    store.create(&name, files, patch)?;
                    repo.reset_paths_to_head(&paths)?;
                    Ok(())
                },
                move |git, (), cx| {
                    git.reload_everything_from_disk(cx);
                    me.update(cx, |this, cx| {
                        this.message.text.clear();
                        this.unchecked.clear();
                        this.reload_shelves(cx);
                    })
                    .ok();
                    git.tell("Shelve", format!("Shelved to {shown}"), cx);
                },
                cx,
            )
        });
    }

    fn selected_shelf(&self) -> Option<Shelf> {
        let index = self
            .shelf_selected
            .or((self.shelves.len() == 1).then_some(0))?;
        self.shelves.get(index).cloned()
    }

    fn ask_unshelve(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(shelf) = self.selected_shelf() else {
            self.git.update(cx, |git, cx| {
                git.report("Unshelve", "Choose a shelf first".to_string(), cx)
            });
            return;
        };
        let me = cx.weak_entity();
        self.workspace
            .update(cx, |workspace, cx| {
                UnshelveDialog::show(
                    &shelf,
                    move |files, delete_after, cx| {
                        me.update(cx, |this, cx| this.apply_shelf(delete_after, files, cx))
                            .ok();
                    },
                    workspace,
                    window,
                    cx,
                );
            })
            .ok();
    }

    fn apply_shelf(&mut self, delete_after: bool, files: Vec<String>, cx: &mut Context<Self>) {
        let Some(shelf) = self.selected_shelf() else {
            return;
        };
        if files.is_empty() {
            return;
        }
        for file in &files {
            let absolute = self.root.join(file);
            if absolute.exists() {
                crate::history::snapshot_file(&absolute, local_history::Cause::BeforeReplace, cx);
            }
        }
        if let Err(message) = self.save_paths(&files, cx) {
            self.git
                .update(cx, |git, cx| git.report("Unshelve", message, cx));
            return;
        }
        let patch = shelf.patch.clone();
        let shown = shelf.name.clone();
        let id = shelf.id.clone();
        let store = self.shelf_store.clone();
        let count = files.len();
        let paths = files.clone();
        let me = cx.weak_entity();
        self.git.update(cx, |git, cx| {
            git.run(
                "Unshelve",
                move |repo| repo.apply_patch(&patch, Some(&paths)),
                move |git, (), cx| {
                    if delete_after {
                        store.delete(&id);
                    }
                    git.reload_everything_from_disk(cx);
                    me.update(cx, |this, cx| this.reload_shelves(cx)).ok();
                    let text = if delete_after {
                        format!("Unshelved {count} file(s) from {shown} and deleted it")
                    } else {
                        format!("Unshelved {count} file(s) from {shown}")
                    };
                    git.tell("Unshelve", text, cx);
                },
                cx,
            )
        });
    }

    fn ask_rename_shelf(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(shelf) = self.selected_shelf() else {
            return;
        };
        let id = shelf.id.clone();
        let name = shelf.name.clone();
        let current = name.clone();
        let store = self.shelf_store.clone();
        let me = cx.weak_entity();
        self.workspace
            .update(cx, |workspace, cx| {
                Prompt::show_with(
                    "Rename Shelf",
                    &name,
                    move |new_name, cx| {
                        if new_name.trim().is_empty() || new_name.trim() == current {
                            return;
                        }
                        let new_name = store.unique_name(&new_name);
                        if let Err(error) = store.rename(&id, &new_name) {
                            eprintln!("shelf is not renamed: {error}");
                        }
                        me.update(cx, |this, cx| this.reload_shelves(cx)).ok();
                    },
                    workspace,
                    window,
                    cx,
                );
            })
            .ok();
    }

    fn delete_shelf(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(shelf) = self.selected_shelf() else {
            return;
        };
        let id = shelf.id.clone();
        let store = self.shelf_store.clone();
        let me = cx.weak_entity();
        let question = format!("Delete shelf {}?", shelf.name);
        self.workspace
            .update(cx, |workspace, cx| {
                Prompt::ask(
                    "Delete Shelf",
                    &question,
                    move |cx| {
                        store.delete(&id);
                        me.update(cx, |this, cx| this.reload_shelves(cx)).ok();
                    },
                    workspace,
                    window,
                    cx,
                );
            })
            .ok();
    }

    /// The open editors, with the path git knows them by and whether they have unsaved text.
    fn open_files(&self, cx: &App) -> Vec<OpenFile> {
        let Some(repo) = self.git.read(cx).repo() else {
            return Vec::new();
        };
        let open = self
            .workspace
            .read_with(cx, |workspace, _| workspace.open_editors())
            .unwrap_or_default();
        let root = repo.root();
        open.into_iter()
            .filter_map(|(path, view)| {
                let view = view.downcast::<EditorView>().ok()?;
                Some(OpenFile {
                    relative: relative_of(root, &path),
                    dirty: view.read(cx).is_dirty(),
                    path,
                })
            })
            .collect()
    }

    /// Commits the ticked files with the message, and sends them when `push` is set.
    pub fn commit(&mut self, push: bool, cx: &mut Context<Self>) {
        let paths = self.checked_paths(cx);
        self.warn_crlf(&paths, cx);
        let open = self.open_files(cx);
        let request =
            match prepare_commit_with(&self.message.text, &paths, open, self.amend, |path| {
                self.workspace
                    .update(cx, |workspace, cx| workspace.save_editor_now(path, cx))
                    .unwrap_or_else(|_| Err("the window is gone".to_string()))
            }) {
                Ok(request) => request,
                Err(problem) => {
                    let text = problem.text().to_string();
                    self.git
                        .update(cx, |git, cx| git.report("Commit", text, cx));
                    return;
                }
            };
        let options = CommitOptions {
            sign_off: self.sign_off,
            author: self.author.clone(),
        };
        if self.analysis && self.start_analysis(&request, push, cx) {
            return;
        }
        self.commit_now(request, push, options, cx);
    }

    /// Warns about CRLF line separators of the ticked files, as the settings ask. The
    /// commit goes on: the warning is the point.
    fn warn_crlf(&self, paths: &[String], cx: &mut Context<Self>) {
        let warn = cx
            .try_global::<Settings>()
            .is_none_or(|settings| settings.git_warn_crlf);
        if !warn {
            return;
        }
        let turned: Vec<&String> = paths
            .iter()
            .filter(|path| {
                std::fs::read(self.root.join(path))
                    .is_ok_and(|text| text.windows(2).any(|pair| pair == b"\r\n"))
            })
            .collect();
        let text = match turned.as_slice() {
            [] => return,
            [path] => format!("{path} has CRLF line separators. The commit goes on."),
            [_, ..] => format!(
                "{} files have CRLF line separators. The commit goes on.",
                turned.len()
            ),
        };
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.notify_user("Commit", text, cx)
            })
            .ok();
    }

    /// Sends the request to git. The message field stays as the user left it when git
    /// fails.
    fn commit_now(
        &mut self,
        request: CommitRequest,
        push: bool,
        options: CommitOptions,
        cx: &mut Context<Self>,
    ) {
        let root = self
            .workspace
            .read_with(cx, |workspace, _| workspace.root().to_path_buf());
        if let Ok(root) = root {
            crate::history::put_label(&root, "Before commit".to_string(), cx);
        }
        let me = cx.weak_entity();
        let amend = request.amend;
        self.git.update(cx, |git, cx| {
            git.run(
                "Commit",
                move |repo| {
                    let id = repo.commit_with(&request, &options)?;
                    if push {
                        repo.push()?;
                    }
                    Ok(id)
                },
                move |git, id, cx| {
                    let short = &id[..id.len().min(7)];
                    let text = if amend && push {
                        format!("Amended {short} and pushed")
                    } else if amend {
                        format!("Amended {short}")
                    } else if push {
                        format!("Committed {short} and pushed")
                    } else {
                        format!("Committed {short}")
                    };
                    git.tell("Commit", text, cx);
                    me.update(cx, |this, cx| {
                        this.message.text.clear();
                        this.amend = false;
                        cx.notify();
                    })
                    .ok();
                },
                cx,
            )
        });
    }

    /// Runs Inspect Code over the files of the commit. `false` means the inspection
    /// cannot run, so the commit goes on without it.
    fn start_analysis(
        &mut self,
        request: &CommitRequest,
        push: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(hub) = cx.try_global::<ActiveInspect>().map(|hub| hub.0.clone()) else {
            return false;
        };
        let files: Vec<PathBuf> = request
            .paths
            .iter()
            .map(|path| self.root.join(path))
            .collect();
        let started = hub.update(cx, |hub, cx| hub.start_files(files, cx));
        if !started {
            return false;
        }
        self.analyzing = Some(PendingCommit {
            request: request.clone(),
            push,
            options: CommitOptions {
                sign_off: self.sign_off,
                author: self.author.clone(),
            },
        });
        let observe = cx.observe(&hub, |this, hub, cx| {
            if hub.read(cx).is_running() {
                return;
            }
            this.analysis_finished(cx);
        });
        self.analysis_observe = Some(observe);
        cx.notify();
        true
    }

    /// The inspection is over: no errors commit at once, errors wait for the answer.
    fn analysis_finished(&mut self, cx: &mut Context<Self>) {
        self.analysis_observe = None;
        let Some(pending) = self.analyzing.take() else {
            return;
        };
        let Some(hub) = cx.try_global::<ActiveInspect>().map(|hub| hub.0.clone()) else {
            self.commit_now(pending.request, pending.push, pending.options, cx);
            return;
        };
        let errors = hub
            .read(cx)
            .findings()
            .iter()
            .filter(|finding| finding.severity == Severity::Error)
            .count();
        if errors == 0 {
            self.commit_now(pending.request, pending.push, pending.options, cx);
            return;
        }
        self.pending_commit = Some(pending);
        self.ask_about_analysis(errors, cx);
    }

    /// Review / Commit anyway, as IDEA asks after an inspection with errors.
    fn ask_about_analysis(&mut self, errors: usize, cx: &mut Context<Self>) {
        let me = cx.weak_entity();
        let window = self.window;
        let workspace = self.workspace.clone();
        let review_workspace = workspace.clone();
        let review = move |window: &mut Window, cx: &mut App| {
            let workspace = review_workspace.clone();
            let _ = me.update(cx, |this, cx| {
                this.pending_commit = None;
                workspace
                    .update(cx, |workspace, cx| {
                        workspace.show_tool(workspace::INSPECTION_TOOL, window, cx)
                    })
                    .ok();
            });
        };
        let commit_me = cx.weak_entity();
        let commit = move |_: &mut Window, cx: &mut App| {
            let _ = commit_me.update(cx, |this, cx| {
                let Some(pending) = this.pending_commit.take() else {
                    return;
                };
                this.commit_now(pending.request, pending.push, pending.options, cx);
            });
        };
        window
            .update(cx, |_, window, cx| {
                workspace
                    .update(cx, |workspace, cx| {
                        AnalysisDialog::show(errors, review, commit, workspace, window, cx);
                    })
                    .ok();
            })
            .ok();
    }

    fn open_diff(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let git = self.git.read(cx);
        let Some(file) = git.status().files.get(index) else {
            return;
        };
        let Some(path) = git.absolute(&file.path) else {
            return;
        };
        let files: Vec<PathBuf> = git
            .status()
            .files
            .iter()
            .filter_map(|file| git.absolute(&file.path))
            .collect();
        let index = files.iter().position(|file| *file == path).unwrap_or(0);
        DiffView::open_list(
            self.git.clone(),
            self.workspace.clone(),
            files,
            index,
            window,
            cx,
        );
    }

    fn open_file(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let git = self.git.read(cx);
        let Some(path) = git
            .status()
            .files
            .get(index)
            .and_then(|f| git.absolute(&f.path))
        else {
            return;
        };
        self.workspace
            .update(cx, |workspace, cx| workspace.open_file(path, window, cx))
            .ok();
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = &event.keystroke;
        if self.tab == Tab::Shelf {
            match key.key.as_str() {
                "up" => {
                    self.shelf_selected = Some(
                        self.shelf_selected
                            .map_or(0, |index| index.saturating_sub(1)),
                    );
                    self.shelf_file = None;
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                "down" => {
                    let count = self.shelves.len().saturating_sub(1);
                    self.shelf_selected = Some(
                        self.shelf_selected
                            .map_or(0, |index| (index + 1).min(count)),
                    );
                    self.shelf_file = None;
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                "enter" | "f4" => {
                    self.ask_unshelve(window, cx);
                    cx.stop_propagation();
                    return;
                }
                _ => {}
            }
        }
        if key.key == "enter" && key.modifiers.control {
            cx.stop_propagation();
            self.commit(key.modifiers.shift, cx);
            return;
        }
        if key.key == "tab" {
            cx.stop_propagation();
            self.files_focused = !self.files_focused;
            if self.files_focused && self.selected.is_none() {
                self.selected = self.visible_files(cx).first().copied();
            }
            cx.notify();
            return;
        }
        if self.files_focused {
            let visible = self.visible_files(cx);
            match key.key.as_str() {
                "up" => {
                    let position = self
                        .selected
                        .and_then(|selected| visible.iter().position(|index| *index == selected));
                    let next = position.map_or(0, |position| position.saturating_sub(1));
                    self.selected = visible.get(next).copied();
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                "down" => {
                    let position = self
                        .selected
                        .and_then(|selected| visible.iter().position(|index| *index == selected));
                    let last = visible.len().saturating_sub(1);
                    let next = position.map_or(0, |position| (position + 1).min(last));
                    self.selected = visible.get(next).copied();
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                "space" | " " => {
                    self.toggle_selected(cx);
                    cx.stop_propagation();
                    return;
                }
                _ => {}
            }
        }
        if self.message.key_with_clipboard(event, true, cx) {
            cx.stop_propagation();
            cx.notify();
        }
    }

    /// The space bar of the list: the file under the cursor gets the tick or loses it.
    fn toggle_selected(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self
            .selected
            .and_then(|index| self.git.read(cx).status().files.get(index))
            .map(|file| file.path.clone())
        else {
            return;
        };
        let checked = !self.unchecked.contains(&path);
        self.set_checked(&path, !checked, cx);
    }

    fn popup_anchor(&self, window: &Window) -> Point<Pixels> {
        if self.tab == Tab::Commit
            && let Some(bounds) = self
                .selected
                .and_then(|index| self.row_bounds.borrow().get(&index).copied())
        {
            return point(bounds.left() + px(8.), bounds.bottom() - px(4.));
        }
        window.mouse_position()
    }

    fn file_row(&self, index: usize, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let git = self.git.read(cx);
        let file = &git.status().files[index];
        let path = file.path.clone();
        let kind = Kind::of(file);
        let (folder, name) = match path.rsplit_once('/') {
            Some((folder, name)) => (folder.to_string(), name.to_string()),
            None => (String::new(), path.clone()),
        };
        let checked = !self.unchecked.contains(&path);
        let toggle_path = path.clone();
        let boxed = div()
            .id(("commit-check", index))
            .flex_none()
            .size(px(14.))
            .mr(px(SPACE_M))
            .rounded(px(3.))
            .border_1()
            .border_color(if checked {
                theme.accent
            } else {
                theme.text_secondary
            })
            .when(checked, |this| this.bg(theme.accent))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.files_focused = true;
                let now = this.unchecked.contains(&toggle_path);
                this.set_checked(&toggle_path, now, cx);
            }))
            .child(
                div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .when(checked, |this| {
                        this.child(div().size(px(6.)).rounded(px(1.)).bg(theme.over_accent))
                    }),
            );
        let selected = self.selected == Some(index);
        let hover = theme.selection_hover;
        let row = div()
            .id(("commit-row", index))
            .debug_selector(move || format!("commit-row-{index}"))
            .flex()
            .items_center()
            .flex_none()
            .w_full()
            .h(px(ROW_HEIGHT))
            .pl(px(ROW_PADDING_X))
            .rounded(px(ROW_RADIUS))
            .when(selected, |this| this.bg(theme.selection_inactive))
            .when(!selected, |this| this.hover(move |style| style.bg(hover)))
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                this.files_focused = true;
                this.selected = Some(index);
                if event.click_count() >= 2 {
                    this.open_diff(index, window, cx);
                }
                cx.notify();
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, _, window, cx| this.open_file(index, window, cx)),
            )
            .child(boxed)
            .child(
                div()
                    .text_color(kind.color(theme.dark))
                    .flex_none()
                    .child(name),
            )
            .child(
                div()
                    .ml(px(8.))
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_color(theme.text_secondary)
                    .child(folder),
            );
        let row_bounds = self.row_bounds.clone();
        div()
            .w_full()
            .px(px(ROW_MARGIN))
            .on_children_prepainted(move |bounds, _, _| {
                if let Some(bounds) = bounds.first() {
                    row_bounds.borrow_mut().insert(index, *bounds);
                }
            })
            .child(row)
    }

    fn button(
        &self,
        id: &'static str,
        label: &'static str,
        primary: bool,
        theme: &Theme,
    ) -> Stateful<Div> {
        let (hover, pressed) = (theme.hover, theme.pressed);
        div()
            .id(id)
            .debug_selector(move || id.to_string())
            .flex()
            .items_center()
            .justify_center()
            .h(px(26.))
            .px(px(12.))
            .rounded(px(CONTROL_RADIUS))
            .border_1()
            .border_color(if primary {
                theme.accent
            } else {
                theme.popup_border
            })
            .when(primary, |this| {
                this.bg(theme.accent).text_color(theme.over_accent)
            })
            .when(!primary, |this| {
                this.hover(move |style| style.bg(hover))
                    .active(move |style| style.bg(pressed))
            })
            .child(label)
    }

    fn project_name(&self) -> String {
        self.root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Project".to_string())
    }

    /// The heading a file is grouped under, in the choice of the user.
    fn group_of(&self, path: &str, cx: &App) -> String {
        match self.lists.grouping {
            Grouping::Directory => path
                .rsplit_once('/')
                .map_or_else(|| self.project_name(), |(folder, _)| folder.to_string()),
            Grouping::Module => {
                let absolute = self.root.join(path);
                cx.try_global::<crate::script::ActiveRunPanel>()
                    .and_then(|panel| panel.0.read(cx).module_of(&absolute))
                    .unwrap_or_else(|| self.project_name())
            }
        }
    }

    fn group_row(&self, group: &str, index: usize, theme: &Theme) -> Stateful<Div> {
        div()
            .id(("commit-group", index))
            .debug_selector(move || format!("commit-group-{index}"))
            .flex()
            .items_center()
            .flex_none()
            .h(px(ROW_HEIGHT))
            .px(px(ROW_MARGIN + 6.))
            .text_color(theme.text_secondary)
            .font_weight(FontWeight::MEDIUM)
            .child(group.to_string())
    }

    fn grouped_rows(&self, theme: &Theme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut rows = Vec::new();
        let mut last: Option<String> = None;
        for index in self.visible_files(cx) {
            let Some(path) = self
                .git
                .read(cx)
                .status()
                .files
                .get(index)
                .map(|file| file.path.clone())
            else {
                continue;
            };
            let group = self.group_of(&path, cx);
            if last.as_deref() != Some(group.as_str()) {
                rows.push(self.group_row(&group, rows.len(), theme).into_any_element());
                last = Some(group);
            }
            rows.push(self.file_row(index, theme, cx).into_any_element());
        }
        rows
    }

    fn tab_row(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let tab = |id: &'static str, label: &'static str, active: bool, cx: &mut Context<Self>| {
            let target = if id == "commit-tab" {
                Tab::Commit
            } else {
                Tab::Shelf
            };
            div()
                .id(id)
                .debug_selector(move || id.to_string())
                .flex()
                .items_center()
                .h(px(24.))
                .px(px(8.))
                .rounded(px(CONTROL_RADIUS))
                .when(active, |this| this.bg(theme.selection_inactive))
                .when(!active, |this| {
                    let hover = theme.hover;
                    this.hover(move |style| style.bg(hover))
                })
                .text_color(if active {
                    theme.text
                } else {
                    theme.text_secondary
                })
                .on_click(cx.listener(move |this, _, _, cx| this.set_tab(target, cx)))
                .child(label)
        };
        div()
            .flex()
            .items_center()
            .gap(px(4.))
            .flex_none()
            .px(px(ROW_MARGIN))
            .pb(px(4.))
            .child(tab("commit-tab", "Commit", self.tab == Tab::Commit, cx))
            .child(tab("shelf-tab", "Shelf", self.tab == Tab::Shelf, cx))
    }

    fn changelist_row(&self, theme: &Theme, cx: &mut Context<Self>) -> Stateful<Div> {
        let label = format!("{}  ▾", self.lists.active);
        div()
            .id("changelist-active")
            .debug_selector(|| "changelist-active".to_owned())
            .flex()
            .items_center()
            .flex_none()
            .h(px(ROW_HEIGHT))
            .mx(px(ROW_MARGIN))
            .px(px(ROW_PADDING_X))
            .rounded(px(ROW_RADIUS))
            .text_color(theme.text_secondary)
            .hover({
                let hover = theme.hover;
                move |style| style.bg(hover)
            })
            .on_click(cx.listener(|this, _, window, cx| {
                this.show_changelist_menu(window.mouse_position(), cx)
            }))
            .child(label)
    }

    fn shelf_row(&self, index: usize, theme: &Theme, cx: &mut Context<Self>) -> Stateful<Div> {
        let shelf = &self.shelves[index];
        let selected = self.shelf_selected == Some(index);
        let name = shelf.name.clone();
        let files = shelf.files.len();
        div()
            .id(("shelf-row", index))
            .debug_selector(move || format!("shelf-row-{index}"))
            .flex()
            .items_center()
            .gap(px(8.))
            .flex_none()
            .h(px(ROW_HEIGHT))
            .mx(px(ROW_MARGIN))
            .px(px(ROW_PADDING_X))
            .rounded(px(ROW_RADIUS))
            .when(selected, |this| this.bg(theme.selection_inactive))
            .when(!selected, |this| {
                let hover = theme.selection_hover;
                this.hover(move |style| style.bg(hover))
            })
            .on_click(cx.listener(move |this, _, _, cx| this.select_shelf(index, cx)))
            .child(div().flex_1().min_w_0().truncate().child(name))
            .child(
                div()
                    .text_color(theme.text_secondary)
                    .child(format!("{files} file(s)")),
            )
    }

    fn shelf_body(
        &mut self,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if self.shelves.is_empty() {
            return div()
                .flex()
                .flex_1()
                .items_center()
                .justify_center()
                .text_color(theme.text_secondary)
                .child("No shelves")
                .into_any_element();
        }
        let rows: Vec<AnyElement> = (0..self.shelves.len())
            .map(|index| self.shelf_row(index, theme, cx).into_any_element())
            .collect();
        let selected = self
            .shelf_selected
            .and_then(|index| self.shelves.get(index));
        let mut files: Vec<AnyElement> = Vec::new();
        let mut diff: Vec<AnyElement> = Vec::new();
        if let Some(shelf) = selected {
            let names = shelf.files.clone();
            for (row, entry) in shelf_entries(&names).into_iter().enumerate() {
                match entry.file {
                    Some(index) => {
                        let chosen = self.shelf_file == Some(index);
                        let name = entry.label.clone();
                        let indent = ROW_PADDING_X + entry.depth as f32 * SPACE_L;
                        files.push(
                            div()
                                .id(("shelf-file", index))
                                .debug_selector(move || format!("shelf-file-{index}"))
                                .flex()
                                .items_center()
                                .flex_none()
                                .h(px(ROW_HEIGHT))
                                .mx(px(ROW_MARGIN))
                                .pl(px(indent))
                                .pr(px(ROW_PADDING_X))
                                .rounded(px(ROW_RADIUS))
                                .when(chosen, |this| this.bg(theme.selection_inactive))
                                .hover({
                                    let hover = theme.selection_hover;
                                    move |style| style.bg(hover)
                                })
                                .on_click(cx.listener(
                                    move |this, event: &ClickEvent, window, cx| {
                                        this.select_shelf_file(index, cx);
                                        if event.click_count() >= 2 {
                                            this.open_shelf_diff(index, window, cx);
                                        }
                                    },
                                ))
                                .child(name)
                                .into_any_element(),
                        );
                    }
                    None => {
                        let name = entry.label.clone();
                        let indent = ROW_PADDING_X + entry.depth as f32 * SPACE_L;
                        files.push(
                            div()
                                .id(("shelf-folder", row))
                                .debug_selector(move || format!("shelf-folder-{row}"))
                                .flex()
                                .items_center()
                                .flex_none()
                                .h(px(ROW_HEIGHT))
                                .mx(px(ROW_MARGIN))
                                .pl(px(indent))
                                .pr(px(ROW_PADDING_X))
                                .text_color(theme.text_secondary)
                                .child(name)
                                .into_any_element(),
                        );
                    }
                }
            }
            if let Some(preview) = &self.shelf_preview {
                for hunk in &preview.hunks {
                    let rows =
                        crate::diff_view::hunk_lines(&preview.base, &preview.current, hunk, theme);
                    diff.push(rows.into_any_element());
                }
            }
            if diff.is_empty() {
                diff.push(
                    div()
                        .text_color(theme.text_secondary)
                        .child("No changes")
                        .into_any_element(),
                );
            }
        }
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(
                scrollable(
                    "shelf-list",
                    &self.scroll_shelves,
                    div()
                        .id("shelf-list")
                        .flex()
                        .flex_col()
                        .max_h(px(220.))
                        .pt(px(SPACE_S))
                        .overflow_y_scroll()
                        .track_scroll(&self.scroll_shelves)
                        .children(rows),
                    theme,
                    window,
                    cx,
                )
                .flex_none(),
            )
            .when(selected.is_some(), |this| {
                this.child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .flex_none()
                        .px(px(ROW_MARGIN))
                        .h(px(24.))
                        .text_color(theme.text_secondary)
                        .child("Files")
                        .child(
                            div()
                                .flex()
                                .gap(px(8.))
                                .child(
                                    div()
                                        .id("shelf-unshelve")
                                        .debug_selector(|| "shelf-unshelve".to_owned())
                                        .px(px(SPACE_M))
                                        .rounded(px(CONTROL_RADIUS))
                                        .hover({
                                            let hover = theme.hover;
                                            move |style| style.bg(hover)
                                        })
                                        .child("Unshelve...")
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.ask_unshelve(window, cx)
                                        })),
                                )
                                .child(
                                    div()
                                        .id("shelf-rename")
                                        .debug_selector(|| "shelf-rename".to_owned())
                                        .px(px(SPACE_M))
                                        .rounded(px(CONTROL_RADIUS))
                                        .hover({
                                            let hover = theme.hover;
                                            move |style| style.bg(hover)
                                        })
                                        .child("Rename...")
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.ask_rename_shelf(window, cx)
                                        })),
                                )
                                .child(
                                    div()
                                        .id("shelf-delete")
                                        .debug_selector(|| "shelf-delete".to_owned())
                                        .px(px(SPACE_M))
                                        .rounded(px(CONTROL_RADIUS))
                                        .hover({
                                            let hover = theme.hover;
                                            move |style| style.bg(hover)
                                        })
                                        .child("Delete...")
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.delete_shelf(window, cx)
                                        })),
                                ),
                        ),
                )
                .child(
                    scrollable(
                        "shelf-files",
                        &self.scroll_shelf_files,
                        div()
                            .id("shelf-files")
                            .flex()
                            .flex_col()
                            .max_h(px(160.))
                            .overflow_y_scroll()
                            .track_scroll(&self.scroll_shelf_files)
                            .children(files),
                        theme,
                        window,
                        cx,
                    )
                    .flex_none(),
                )
                .child(
                    scrollable(
                        "shelf-diff",
                        &self.scroll_shelf_diff,
                        div()
                            .id("shelf-diff")
                            .debug_selector(|| "shelf-diff".to_owned())
                            .flex()
                            .flex_col()
                            .px(px(SPACE_M))
                            .pt(px(SPACE_S))
                            .overflow_y_scroll()
                            .track_scroll(&self.scroll_shelf_diff)
                            .children(diff),
                        theme,
                        window,
                        cx,
                    )
                    .flex_1()
                    .min_h_0(),
                )
            })
            .into_any_element()
    }
}

/// The question after Inspect Code found errors: look at them, or commit anyway.
type DialogAction = Rc<dyn Fn(&mut Window, &mut App)>;

struct AnalysisDialog {
    errors: usize,
    focus: FocusHandle,
    workspace: WeakEntity<Workspace>,
    review: DialogAction,
    commit: DialogAction,
}

impl AnalysisDialog {
    fn show(
        errors: usize,
        review: impl Fn(&mut Window, &mut App) + 'static,
        commit: impl Fn(&mut Window, &mut App) + 'static,
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        let weak = cx.weak_entity();
        let view = cx.new(|cx| Self {
            errors,
            focus: cx.focus_handle(),
            workspace: weak,
            review: Rc::new(review),
            commit: Rc::new(commit),
        });
        let focus = view.read(cx).focus.clone();
        workspace.show_overlay(view.into(), focus, window, cx);
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "enter" => {
                self.confirm(window, cx);
                cx.stop_propagation();
            }
            "escape" => {
                self.close(window, cx);
                cx.stop_propagation();
            }
            _ => {}
        }
    }

    fn review(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let review = self.review.clone();
        self.close(window, cx);
        (review)(window, cx);
    }

    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let commit = self.commit.clone();
        self.close(window, cx);
        (commit)(window, cx);
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace
            .update(cx, |workspace, cx| workspace.dismiss(window, cx))
            .ok();
    }
}

impl Render for AnalysisDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let width = px(420.);
        let left = (window.viewport_size().width - width) / 2.;
        let top = window.viewport_size().height * 0.32;
        let button = |id: &'static str, label: &'static str, primary: bool| {
            div()
                .id(id)
                .debug_selector(move || id.to_string())
                .px(px(12.))
                .h(px(26.))
                .flex()
                .items_center()
                .rounded(px(CONTROL_RADIUS))
                .border_1()
                .border_color(if primary {
                    theme.accent
                } else {
                    theme.popup_border
                })
                .when(primary, |this| {
                    this.bg(theme.accent).text_color(theme.over_accent)
                })
                .when(!primary, |this| {
                    let hover = theme.hover;
                    this.hover(move |style| style.bg(hover))
                })
                .child(label)
        };
        let body = surface(&theme)
            .id("analysis-dialog")
            .debug_selector(|| "analysis-dialog".to_owned())
            .track_focus(&self.focus)
            .occlude()
            .w(width)
            .p(px(DIALOG_PADDING))
            .gap(px(DIALOG_GAP))
            .on_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)))
            .on_mouse_down_out(cx.listener(|this, _, window, cx| this.close(window, cx)))
            .child(
                div()
                    .font_weight(FontWeight::MEDIUM)
                    .child("Code analysis found problems"),
            )
            .child(div().text_color(theme.text_secondary).child(format!(
                "{} error(s) in the chosen files. Review them or commit anyway?",
                self.errors
            )))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(8.))
                    .child(
                        button("analysis-review", "Review", false)
                            .on_click(cx.listener(|this, _, window, cx| this.review(window, cx))),
                    )
                    .child(
                        button("analysis-commit", "Commit anyway", true)
                            .on_click(cx.listener(|this, _, window, cx| this.confirm(window, cx))),
                    ),
            );
        div()
            .absolute()
            .left(left)
            .top(top)
            .child(enter("analysis", body))
    }
}

impl Render for CommitPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(test)]
        workspace::note_view_render(cx, "commit");
        let theme = *cx.global::<Theme>();
        let count = self.visible_files(cx).len();
        let busy = self.git.read(cx).busy();
        let branch = self.git.read(cx).status().branch.clone();
        let workspace = self.workspace.clone();
        let hide = move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
            workspace
                .update(cx, |workspace, cx| {
                    workspace.toggle_tool(COMMIT_TOOL, window, cx)
                })
                .ok();
        };
        let rows = self.grouped_rows(&theme, cx);
        let message_empty = self.message.text.is_empty();
        let shown: SharedString = if message_empty {
            "Commit message".into()
        } else {
            self.message.text.clone().into()
        };
        let summary = match (busy, branch) {
            (Some(what), _) => format!("{what}..."),
            (None, Some(branch)) => format!("{count} changed on {branch}"),
            (None, None) => format!("{count} changed"),
        };
        let summary = if self.amend {
            format!("Amend · {summary}")
        } else {
            summary
        };
        let title = match self.tab {
            Tab::Commit => "Commit",
            Tab::Shelf => "Shelf",
        };
        let body = if self.tab == Tab::Shelf {
            self.shelf_body(&theme, window, cx)
        } else {
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .child(self.changelist_row(&theme, cx))
                .child(
                    scrollable(
                        "commit-files",
                        &self.scroll_files,
                        div()
                            .id("commit-files")
                            .flex()
                            .flex_col()
                            .pt(px(SPACE_S))
                            .overflow_y_scroll()
                            .track_scroll(&self.scroll_files)
                            .children(rows)
                            .when(count == 0, |this| {
                                this.items_center()
                                    .justify_center()
                                    .text_color(theme.text_secondary)
                                    .child("No changes")
                            }),
                        &theme,
                        window,
                        cx,
                    )
                    .flex_1()
                    .min_h_0(),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_none()
                        .gap(px(SPACE_M))
                        .p(px(SPACE_M))
                        .child(
                            div()
                                .h(px(MESSAGE_HEIGHT))
                                .p(px(SPACE_M))
                                .rounded(px(CONTROL_RADIUS))
                                .border_1()
                                .border_color(theme.popup_border)
                                .overflow_hidden()
                                .text_color(if message_empty {
                                    theme.text_secondary
                                } else {
                                    theme.text
                                })
                                .child(shown),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(
                                    div()
                                        .flex()
                                        .gap(px(8.))
                                        .child(
                                            self.button("commit-go", "Commit", true, &theme)
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.commit(false, cx)
                                                })),
                                        )
                                        .child(
                                            self.button(
                                                "commit-push",
                                                "Commit and Push",
                                                false,
                                                &theme,
                                            )
                                            .on_click(
                                                cx.listener(|this, _, _, cx| this.commit(true, cx)),
                                            ),
                                        )
                                        .child(
                                            self.button(
                                                "commit-shelve",
                                                "Shelve Changes",
                                                false,
                                                &theme,
                                            )
                                            .on_click(
                                                cx.listener(|this, _, _, cx| this.shelve(cx)),
                                            ),
                                        ),
                                )
                                .child(div().text_color(theme.text_secondary).child(summary)),
                        ),
                )
                .into_any_element()
        };
        island(&theme)
            .id("commit-panel")
            .key_context("CommitPanel ToolWindow")
            .track_focus(&self.focus)
            .group(GROUP)
            .h_full()
            .on_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)))
            .on_action(
                cx.listener(|this, _: &RevertSelected, window, cx| {
                    this.revert_selected(window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &ToggleAmend, _, cx| this.toggle_amend(cx)))
            .on_action(cx.listener(|this, _: &MessageHistory, window, cx| {
                this.show_message_history(this.popup_anchor(window), cx)
            }))
            .on_action(cx.listener(|this, _: &MoveToChangelist, window, cx| {
                this.show_move_menu(this.popup_anchor(window), cx)
            }))
            .on_action(cx.listener(|this, _: &SetActiveChangelist, window, cx| {
                this.show_changelist_menu(this.popup_anchor(window), cx)
            }))
            .on_action(
                cx.listener(|this, _: &Rename, window, cx| this.ask_rename_changelist(window, cx)),
            )
            .on_action(cx.listener(|this, _: &GroupByDirectory, _, cx| {
                this.set_grouping(Grouping::Directory, cx)
            }))
            .on_action(
                cx.listener(|this, _: &GroupByModule, _, cx| {
                    this.set_grouping(Grouping::Module, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &ShowCommitOptions, window, cx| {
                this.show_commit_options(this.popup_anchor(window), cx)
            }))
            .on_action(cx.listener(|this, _: &Shelve, _, cx| this.shelve(cx)))
            .on_action(cx.listener(|this, _: &Unshelve, window, cx| this.ask_unshelve(window, cx)))
            .on_action(cx.listener(|this, _: &ShowDiff, window, cx| this.show_diff(window, cx)))
            .child(
                panel_header(title, GROUP, hide, &theme)
                    .debug_selector(|| "commit-header".to_owned()),
            )
            .child(self.tab_row(&theme, cx))
            .child(body)
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{CommitProblem, OpenFile, prepare_commit};

    fn open(path: &str, relative: &str, dirty: bool) -> OpenFile {
        OpenFile {
            path: PathBuf::from(path),
            relative: relative.into(),
            dirty,
        }
    }

    #[test]
    fn unsaved_files_of_the_commit_are_saved_before_it() {
        let paths = vec!["src/a.txt".to_string(), "src/b.txt".to_string()];
        let open = vec![
            open("C:/repo/src/a.txt", "src/a.txt", true),
            open("C:/repo/src/b.txt", "src/b.txt", false),
            open("C:/repo/src/c.txt", "src/c.txt", true),
        ];
        let mut saved = Vec::new();

        let request = prepare_commit("  Fix the build  ", &paths, open, |path| {
            saved.push(path.to_path_buf());
            Ok(())
        })
        .unwrap();

        assert_eq!(saved, [PathBuf::from("C:/repo/src/a.txt")]);
        assert_eq!(request.message, "Fix the build");
        assert_eq!(request.paths, paths);
        assert!(!request.amend);
    }

    #[test]
    fn a_failed_save_stops_the_commit_before_git() {
        let paths = vec!["src/a.txt".to_string(), "src/b.txt".to_string()];
        let open = vec![
            open("C:/repo/src/a.txt", "src/a.txt", true),
            open("C:/repo/src/b.txt", "src/b.txt", true),
        ];
        let mut saved = Vec::new();

        let result = prepare_commit("Fix the build", &paths, open, |path| {
            saved.push(path.to_path_buf());
            Err("disk is full".into())
        });

        assert_eq!(result, Err(CommitProblem::Save("disk is full".into())));
        assert_eq!(saved, [PathBuf::from("C:/repo/src/a.txt")]);
    }

    #[test]
    fn an_empty_message_or_no_files_saves_nothing() {
        let mut no_save = |_: &Path| -> Result<(), String> { panic!("nothing may be saved") };

        assert_eq!(
            prepare_commit("  ", &["a".to_string()], Vec::new(), &mut no_save),
            Err(CommitProblem::NoMessage)
        );
        assert_eq!(
            prepare_commit(
                "Done",
                &[],
                vec![open("C:/repo/a", "a", true)],
                &mut no_save
            ),
            Err(CommitProblem::NoFiles)
        );
        assert_eq!(
            CommitProblem::NoMessage.text(),
            "Write a commit message first"
        );
        assert_eq!(CommitProblem::NoFiles.text(), "Tick at least one file");
    }
}
