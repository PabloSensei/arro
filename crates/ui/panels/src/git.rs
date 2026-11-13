//! Git in the window. The status of the repository is kept up to date in the background:
//! colours for the project tree, marks for the gutter of open editors, the branch in the
//! status bar. The actions behind the keys live here too, and every call to git runs off the
//! UI thread.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use editor_view::{EditorView, GutterKind, GutterMark};
use ui_kit::*;
use vcs::{
    Change, ConflictSide, FileStatus, GitRunner, Hunk, LineChangeKind, Operation, Repository,
    Stash, Status, UpdateMode, UpdateReport,
};
use workspace::{COMMIT_TOOL, Settings, VcsQuickList, Workspace};

use crate::conflict::ConflictView;
use crate::project::ActiveProjectPanel;
use crate::prompt::Prompt;

/// The Git of the settings: the configured program, or the one on `PATH`.
pub(crate) fn configured_git(settings: &Settings) -> Option<GitRunner> {
    let path = settings.git_path.trim();
    if path.is_empty() {
        GitRunner::from_env()
    } else {
        Some(GitRunner::new(PathBuf::from(path)))
    }
}

/// How Update Project brings the upstream in, as the settings page chose.
pub(crate) fn update_mode(settings: &Settings) -> UpdateMode {
    match settings.git_update_method {
        workspace::GitUpdateMethod::Merge => UpdateMode::Merge,
        workspace::GitUpdateMethod::Rebase => UpdateMode::Rebase,
    }
}

/// Whether git refused a push because the branch is behind its remote.
fn push_rejected(error: &std::io::Error) -> bool {
    let text = error.to_string();
    text.contains("rejected") || text.contains("non-fast-forward") || text.contains("fetch first")
}

/// A command that puts the branch of the last Delete back.
pub const RESTORE_BRANCH_COMMAND: &str = "git.restore_deleted_branch";

/// A change on disk waits for this long, so a save of many files is one read of the status.
const DEBOUNCE: Duration = Duration::from_millis(300);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Modified,
    Added,
    Deleted,
    Conflicted,
    Untracked,
}

impl Kind {
    pub fn of(file: &FileStatus) -> Self {
        let sides = [file.index, file.worktree];
        if sides.contains(&Change::Conflicted) {
            Self::Conflicted
        } else if file.worktree == Change::Untracked {
            Self::Untracked
        } else if file.index == Change::Added {
            Self::Added
        } else if sides.contains(&Change::Deleted) {
            Self::Deleted
        } else {
            Self::Modified
        }
    }

    /// The colour of a file name in the tree and of a mark in the gutter.
    pub fn color(self, dark: bool) -> Hsla {
        let hex = match (self, dark) {
            (Self::Modified, true) => 0x6f9ee8,
            (Self::Modified, false) => 0x2f5fc7,
            (Self::Added, true) => 0x5fb865,
            (Self::Added, false) => 0x067d17,
            (Self::Deleted, true) => 0x868a91,
            (Self::Deleted, false) => 0x6f737a,
            (Self::Conflicted, true) => 0xf75464,
            (Self::Conflicted, false) => 0xc7222d,
            (Self::Untracked, true) => 0xd1855c,
            (Self::Untracked, false) => 0xa65a2a,
        };
        rgb(hex).into()
    }
}

/// The repository of the project and what is known about it. `None` repository: not a
/// Git project, and everything here stays quiet.
pub struct GitState {
    repo: Option<Arc<Repository>>,
    status: Status,
    kinds: HashMap<PathBuf, Kind>,
    folders: HashSet<PathBuf>,
    workspace: WeakEntity<Workspace>,
    refreshing: bool,
    /// What is going on that the user waits for, shown by the commit window.
    busy: Option<&'static str>,
    /// The last failure of an action, for the commit window and scripts.
    last_error: Option<String>,
    /// A merge or a rebase git waits to end.
    operation: Operation,
    /// Git holds local changes and puts them back when the operation ends.
    autostash: bool,
    /// The message a merge commit starts with.
    merge_message: String,
    /// The branches the user left, newest first, as the Recent section of the popup lists
    /// them.
    recent: Vec<String>,
    /// The branch the last Delete took away, and its commit, for Restore.
    last_deleted: Option<(String, String)>,
    /// The window the actions open popups in; set when the keys are bound.
    window: Option<AnyWindowHandle>,
    /// Whether the window is in front. A change behind it only marks the status stale.
    active: bool,
    /// A change on disk has not reached the status yet.
    stale: bool,
    /// A read waits for the debounce to pass; further changes need no second read.
    scheduled: bool,
}

pub struct ActiveGit(pub Entity<GitState>);

impl Global for ActiveGit {}

/// What one background read of the repository brought back.
struct Refreshed {
    status: Result<Status, String>,
    hunks: HashMap<PathBuf, Vec<Hunk>>,
    operation: Operation,
    autostash: bool,
    merge_message: String,
}

impl GitState {
    pub fn create(root: PathBuf, workspace: WeakEntity<Workspace>, cx: &mut App) -> Entity<Self> {
        let git = cx.new(|_| Self {
            repo: None,
            status: Status::default(),
            kinds: HashMap::new(),
            folders: HashSet::new(),
            workspace,
            refreshing: false,
            busy: None,
            last_error: None,
            operation: Operation::None,
            autostash: false,
            merge_message: String::new(),
            recent: Vec::new(),
            last_deleted: None,
            window: None,
            active: true,
            stale: false,
            scheduled: false,
        });
        let configured = cx.try_global::<Settings>().cloned().unwrap_or_default();
        let found = cx.background_executor().spawn(async move {
            let git = configured_git(&configured)?;
            Repository::open(git, &root).ok().map(Arc::new)
        });
        let weak = git.downgrade();
        cx.spawn(async move |cx| {
            let repo = found.await;
            weak.update(cx, |this, cx| {
                this.repo = repo;
                this.refresh(cx);
            })
            .ok();
        })
        .detach();
        git
    }

    pub fn repo(&self) -> Option<Arc<Repository>> {
        self.repo.clone()
    }

    pub fn status(&self) -> &Status {
        &self.status
    }

    /// Whether a read of the repository is on its way, for the tests of the idle window.
    #[cfg(test)]
    pub(crate) fn refreshing(&self) -> bool {
        self.refreshing
    }

    /// Where a popup of a Git key goes: the caret, the selected tree row, or the pointer.
    fn popup_anchor(&self, window: &Window, cx: &App) -> Point<Pixels> {
        self.workspace.upgrade().map_or_else(
            || window.mouse_position(),
            |workspace| {
                let workspace = workspace.read(cx);
                if let Some(anchor) = workspace.take_action_menu_anchor() {
                    return point(
                        anchor.left(),
                        anchor.bottom() + px(ui_kit::metrics::SPACE_XS),
                    );
                }
                crate::commands::popup_anchor(workspace, window, cx)
            },
        )
    }

    pub fn busy(&self) -> Option<&'static str> {
        self.busy
    }

    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    pub fn operation(&self) -> Operation {
        self.operation
    }

    /// Git holds local changes it puts back when the operation ends.
    pub fn autostash(&self) -> bool {
        self.autostash
    }

    pub fn merge_message(&self) -> &str {
        &self.merge_message
    }

    /// The files git left in conflict.
    pub fn conflicts(&self) -> Vec<String> {
        self.status
            .files
            .iter()
            .filter(|file| file.index == Change::Conflicted || file.worktree == Change::Conflicted)
            .map(|file| file.path.clone())
            .collect()
    }

    pub fn kind_of(&self, path: &Path) -> Option<Kind> {
        match self.kinds.get(path) {
            Some(kind) => Some(*kind),
            None => self.folders.contains(path).then_some(Kind::Modified),
        }
    }

    /// Absolute path of a file of the status.
    pub fn absolute(&self, relative: &str) -> Option<PathBuf> {
        Some(self.repo.as_ref()?.root().join(relative))
    }

    /// A file of the working tree or of the repository metadata changed. The read waits
    /// for the debounce, and a window in the background only marks the status stale.
    pub fn repository_changed(&mut self, cx: &mut Context<Self>) {
        if self.repo.is_none() {
            return;
        }
        if !self.active {
            self.stale = true;
            return;
        }
        self.schedule(cx);
    }

    /// The window came to the front or went behind another one.
    pub fn window_activation(&mut self, active: bool, cx: &mut Context<Self>) {
        self.active = active;
        // Whatever happened while the window was behind another one is read once, now.
        if active {
            self.stale = false;
            self.refresh(cx);
        }
    }

    fn schedule(&mut self, cx: &mut Context<Self>) {
        if self.scheduled {
            return;
        }
        self.scheduled = true;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(DEBOUNCE).await;
            this.update(cx, |this, cx| {
                this.scheduled = false;
                if !this.active {
                    this.stale = true;
                    return;
                }
                this.stale = false;
                this.refresh(cx);
            })
            .ok();
        })
        .detach();
    }

    /// Reads the status and the diffs of the open editors, off the UI thread.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.clone() else {
            return;
        };
        if self.refreshing {
            // A read that is running already sees most of this change; one more follows.
            self.stale = true;
            return;
        }
        self.refreshing = true;
        let editors = self.editors(cx);
        let inputs: Vec<(PathBuf, String)> = editors
            .iter()
            .filter_map(|(path, view)| Some((path.clone(), view.read(cx).text()?)))
            .collect();
        let job = cx.background_executor().spawn(async move {
            let status = repo.status().map_err(|error| error.to_string());
            let operation = repo.operation().unwrap_or_default();
            let autostash = operation != Operation::None && repo.autostash().unwrap_or(false);
            let merge_message = match operation {
                Operation::Merge => repo.merge_message().unwrap_or_default(),
                _ => String::new(),
            };
            let mut hunks = HashMap::new();
            if let Ok(mut blobs) = repo.blobs() {
                for (path, text) in inputs {
                    let relative = relative_of(repo.root(), &path);
                    if let Ok(diff) = repo.file_diff(&mut blobs, &relative, &text) {
                        hunks.insert(path, diff.hunks);
                    }
                }
            }
            Refreshed {
                status,
                hunks,
                operation,
                autostash,
                merge_message,
            }
        });
        cx.spawn(async move |this, cx| {
            let refreshed = job.await;
            this.update(cx, |this, cx| {
                this.refreshing = false;
                this.apply(refreshed, editors, cx);
                if this.stale && this.active {
                    this.stale = false;
                    this.schedule(cx);
                }
            })
            .ok();
        })
        .detach();
    }

    fn editors(&self, cx: &mut Context<Self>) -> Vec<(PathBuf, Entity<EditorView>)> {
        let open = self
            .workspace
            .read_with(cx, |workspace, _| workspace.open_editors())
            .unwrap_or_default();
        open.into_iter()
            .filter_map(|(path, view)| Some((path, view.downcast::<EditorView>().ok()?)))
            .collect()
    }

    fn apply(
        &mut self,
        refreshed: Refreshed,
        editors: Vec<(PathBuf, Entity<EditorView>)>,
        cx: &mut Context<Self>,
    ) {
        let Refreshed {
            status,
            hunks,
            operation,
            autostash,
            merge_message,
        } = refreshed;
        let dark = cx.global::<Theme>().dark;
        match status {
            Ok(status) => {
                if status != self.status
                    || operation != self.operation
                    || autostash != self.autostash
                {
                    let branch = branch_label(&status, operation, autostash);
                    self.set_status(status);
                    self.operation = operation;
                    self.autostash = autostash;
                    self.merge_message = merge_message;
                    self.workspace
                        .update(cx, |workspace, cx| workspace.set_branch(branch, cx))
                        .ok();
                    cx.notify();
                }
            }
            Err(message) => self.last_error = Some(message),
        }
        for (path, view) in editors {
            let marks = hunks.get(&path).map(|hunks| marks_of(hunks, dark));
            if let Some(marks) = marks {
                view.update(cx, |view, cx| view.set_gutter_marks(marks, cx));
            }
        }
    }

    fn set_status(&mut self, status: Status) {
        let root = self
            .repo
            .as_ref()
            .map(|repo| repo.root().to_path_buf())
            .unwrap_or_default();
        self.kinds.clear();
        self.folders.clear();
        for file in &status.files {
            let path = root.join(&file.path);
            let kind = Kind::of(file);
            for parent in path.ancestors().skip(1) {
                if parent.starts_with(&root) {
                    self.folders.insert(parent.to_path_buf());
                }
            }
            self.kinds.insert(path, kind);
        }
        self.status = status;
    }

    pub fn report(&mut self, title: &str, message: String, cx: &mut Context<Self>) {
        self.last_error = Some(message.clone());
        let title = title.to_string();
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.notify_user(title, message, cx)
            })
            .ok();
    }

    /// Runs `work` off the UI thread, then refreshes and lets `done` look at the result.
    pub fn run<T: Send + 'static>(
        &mut self,
        what: &'static str,
        work: impl FnOnce(&Repository) -> std::io::Result<T> + Send + 'static,
        done: impl FnOnce(&mut Self, T, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) {
        let Some(repo) = self.repo.clone() else {
            return;
        };
        if self.busy.is_some() {
            self.report(
                "Git",
                format!("Still busy: {}", self.busy.unwrap_or("")),
                cx,
            );
            return;
        }
        self.busy = Some(what);
        self.last_error = None;
        cx.notify();
        let job = cx
            .background_executor()
            .spawn(async move { work(&repo).map_err(|error| error.to_string()) });
        cx.spawn(async move |this, cx| {
            let result = job.await;
            this.update(cx, |this, cx| {
                this.busy = None;
                match result {
                    Ok(value) => done(this, value, cx),
                    Err(message) => this.report(what, message, cx),
                }
                this.refresh(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The files changed on disk behind the back of the editors and the tree.
    pub fn reload_everything(&mut self, cx: &mut Context<Self>) {
        self.reload_all(false, cx);
    }

    /// Reads every open file from disk, even one that had edits, as Reload from Disk.
    pub fn reload_everything_from_disk(&mut self, cx: &mut Context<Self>) {
        self.reload_all(true, cx);
    }

    fn reload_all(&mut self, forced: bool, cx: &mut Context<Self>) {
        for (_, view) in self.editors(cx) {
            view.update(cx, |view, cx| {
                if forced {
                    view.reload_from_disk(cx);
                } else {
                    view.reload(cx);
                }
            });
        }
        if let Some(panel) = cx.try_global::<ActiveProjectPanel>().map(|p| p.0.clone()) {
            panel.update(cx, |panel, cx| panel.reload(cx));
        }
    }

    /// Writes the open files before a git command that reads the working tree.
    fn save_open_files(&mut self, cx: &mut Context<Self>) -> Result<(), String> {
        let mut paths: Vec<PathBuf> = self.editors(cx).into_iter().map(|(path, _)| path).collect();
        paths.sort();
        paths.dedup();
        for path in paths {
            self.workspace
                .update(cx, |workspace, cx| workspace.save_editor_now(&path, cx))
                .unwrap_or_else(|_| Err("the window is gone".to_string()))?;
        }
        Ok(())
    }

    pub fn push(&mut self, cx: &mut Context<Self>) {
        if let Err(message) = self.save_open_files(cx) {
            self.report("Push", message, cx);
            return;
        }
        let settings = cx.try_global::<Settings>().cloned().unwrap_or_default();
        let auto = settings.git_auto_update_on_reject;
        let mode = update_mode(&settings);
        self.run(
            "Push",
            move |repo| {
                let first = repo.push();
                if !auto {
                    return first;
                }
                match first {
                    Err(error) if push_rejected(&error) => {
                        repo.update_with(mode)?;
                        repo.push()
                    }
                    other => other,
                }
            },
            |this, (), cx| this.tell("Push", "Pushed to the remote", cx),
            cx,
        );
    }

    /// Fetch without merging; the status and the ahead/behind counts follow.
    pub fn fetch(&mut self, cx: &mut Context<Self>) {
        self.run(
            "Fetch",
            |repo| repo.fetch(),
            |this, (), cx| this.tell("Fetch", "Fetched from the remote", cx),
            cx,
        );
    }

    pub fn stash(&mut self, message: String, cx: &mut Context<Self>) {
        if let Err(message) = self.save_open_files(cx) {
            self.report("Stash", message, cx);
            return;
        }
        let shown = message.clone();
        self.run(
            "Stash",
            move |repo| repo.stash_push(&message),
            move |this, (), cx| {
                this.reload_everything_from_disk(cx);
                this.tell("Stash", format!("Stashed: {shown}"), cx)
            },
            cx,
        );
    }

    /// The stashes as a menu at `position`: Apply and Pop each one, Drop asks first.
    pub fn show_stashes(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.clone() else {
            return;
        };
        let job = cx
            .background_executor()
            .spawn(async move { repo.stash_list().map_err(|error| error.to_string()) });
        cx.spawn(async move |this, cx| {
            let result = job.await;
            this.update(cx, |this, cx| match result {
                Ok(stashes) => this.stash_menu(stashes, position, cx),
                Err(message) => this.report("Unstash", message, cx),
            })
            .ok();
        })
        .detach();
    }

    fn stash_menu(&mut self, stashes: Vec<Stash>, position: Point<Pixels>, cx: &mut Context<Self>) {
        let me = cx.weak_entity();
        let mut entries = Vec::new();
        if stashes.is_empty() {
            entries.push(MenuEntry::disabled("No stashes"));
        }
        for stash in stashes {
            let label = format!("stash@{{{}}}: {}", stash.index, stash.message);
            entries.push(MenuEntry::disabled(label));
            for (text, pop) in [("Apply", false), ("Pop", true)] {
                let me = me.clone();
                let index = stash.index;
                entries.push(
                    MenuEntry::new(format!("    {text} stash@{{{index}}}"), move |_, cx| {
                        me.update(cx, |this, cx| this.apply_stash(index, pop, cx))
                            .ok();
                    })
                    .icon("general/vcs"),
                );
            }
            let me = me.clone();
            let index = stash.index;
            entries.push(
                MenuEntry::new(format!("    Drop stash@{{{index}}}"), move |window, cx| {
                    let state = me.clone();
                    let question = format!("Drop stash@{{{index}}}?");
                    me.update(cx, |this, cx| {
                        this.workspace
                            .update(cx, |workspace, cx| {
                                Prompt::ask(
                                    "Drop Stash",
                                    &question,
                                    move |cx| {
                                        state
                                            .update(cx, |this, cx| this.drop_stash(index, cx))
                                            .ok();
                                    },
                                    workspace,
                                    window,
                                    cx,
                                );
                            })
                            .ok();
                    })
                    .ok();
                })
                .icon("general/delete"),
            );
            entries.push(MenuEntry::separator());
        }
        if entries.last().is_some_and(|entry| entry.label.is_empty()) {
            entries.pop();
        }
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.show_menu(position, entries, cx)
            })
            .ok();
    }

    pub fn apply_stash(&mut self, index: usize, pop: bool, cx: &mut Context<Self>) {
        if let Err(message) = self.save_open_files(cx) {
            self.report("Unstash", message, cx);
            return;
        }
        let what = if pop { "Pop stash" } else { "Apply stash" };
        self.run(
            what,
            move |repo| {
                if pop {
                    repo.stash_pop(index)
                } else {
                    repo.stash_apply(index)
                }
            },
            move |this, (), cx| {
                this.reload_everything_from_disk(cx);
                let text = if pop {
                    format!("Popped stash@{{{index}}}")
                } else {
                    format!("Applied stash@{{{index}}}")
                };
                this.tell("Unstash", text, cx);
            },
            cx,
        );
    }

    pub fn drop_stash(&mut self, index: usize, cx: &mut Context<Self>) {
        self.run(
            "Drop stash",
            move |repo| repo.stash_drop(index),
            move |this, (), cx| this.tell("Unstash", format!("Dropped stash@{{{index}}}"), cx),
            cx,
        );
    }

    /// Ctrl+T: fetch, then merge, or ask Merge or Rebase when the branch has diverged.
    pub fn update_project(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        if let Err(message) = self.save_open_files(cx) {
            self.report("Update", message, cx);
            return;
        }
        self.run(
            "Fetch",
            |repo| {
                repo.fetch()?;
                repo.status()
            },
            move |this, status, cx| {
                let settings = cx.global::<Settings>().clone();
                let mode = update_mode(&settings);
                let diverged = status.upstream.is_some() && status.ahead > 0 && status.behind > 0;
                // A rebase is the answer of the page: the divergence needs no question.
                if diverged && mode == UpdateMode::Merge {
                    this.show_update_choice(position, cx);
                } else {
                    this.update_with(mode, cx);
                }
            },
            cx,
        );
    }

    pub fn update_with(&mut self, mode: UpdateMode, cx: &mut Context<Self>) {
        if let Err(message) = self.save_open_files(cx) {
            self.report("Update", message, cx);
            return;
        }
        self.run(
            "Update",
            move |repo| repo.update_with(mode),
            move |this, report, cx| this.finish_update(report, cx),
            cx,
        );
    }

    fn show_update_choice(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let me = cx.weak_entity();
        let mut entries = Vec::new();
        for (label, mode) in [("Merge", UpdateMode::Merge), ("Rebase", UpdateMode::Rebase)] {
            let me = me.clone();
            entries.push(
                MenuEntry::new(label, move |_, cx| {
                    me.update(cx, |this, cx| this.update_with(mode, cx)).ok();
                })
                .icon("general/vcs"),
            );
        }
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.show_menu(position, entries, cx)
            })
            .ok();
    }

    fn finish_update(&mut self, report: UpdateReport, cx: &mut Context<Self>) {
        self.operation = report.operation;
        self.autostash = report.autostash;
        self.merge_message = report.merge_message;
        self.reload_everything_from_disk(cx);
        if let Some(stash) = &report.stash {
            self.report("Update", stash_left_message(stash), cx);
        } else if !report.conflicts.is_empty() {
            self.tell(
                "Update",
                format!("{} file(s) in conflict", report.conflicts.len()),
                cx,
            );
        } else if report.operation == Operation::None {
            self.tell("Update", "The project is up to date", cx);
        }
        if !report.conflicts.is_empty() || report.operation != Operation::None {
            self.open_conflicts(cx);
        }
    }

    /// Opens the Conflicts window, which asks for the rest of a merge or a rebase.
    pub fn open_conflicts(&mut self, cx: &mut Context<Self>) {
        let Some(window) = self.window else {
            return;
        };
        let git = cx.entity();
        let workspace = self.workspace.clone();
        cx.defer(move |cx| {
            window
                .update(cx, |_, window, cx| {
                    workspace
                        .update(cx, |workspace, cx| {
                            ConflictView::open(git.clone(), workspace, window, cx)
                        })
                        .ok();
                })
                .ok();
        });
    }

    /// Ends the operation git waits for: a merge commit with the message, or a rebase.
    pub fn finish_operation(&mut self, message: String, cx: &mut Context<Self>) {
        if let Err(message) = self.save_open_files(cx) {
            self.report("Update", message, cx);
            return;
        }
        self.run(
            "Update",
            move |repo| repo.finish(&message),
            |this, stash, cx| {
                let operation = this.operation;
                this.reload_everything_from_disk(cx);
                if operation == Operation::Merge {
                    this.merge_message.clear();
                }
                this.refresh(cx);
                if let Some(stash) = stash {
                    this.report("Update", stash_left_message(&stash), cx);
                } else {
                    this.tell("Update", "The operation is finished", cx);
                }
            },
            cx,
        );
    }

    pub fn abort_operation(&mut self, cx: &mut Context<Self>) {
        if let Err(message) = self.save_open_files(cx) {
            self.report("Update", message, cx);
            return;
        }
        self.run(
            "Abort",
            |repo| repo.abort(),
            |this, stash, cx| {
                this.operation = Operation::None;
                this.autostash = false;
                this.merge_message.clear();
                this.reload_everything_from_disk(cx);
                if let Some(stash) = stash {
                    this.report("Update", stash_left_message(&stash), cx);
                } else {
                    this.tell("Update", "The operation was aborted", cx);
                }
            },
            cx,
        );
    }

    pub fn accept_side(&mut self, path: String, side: ConflictSide, cx: &mut Context<Self>) {
        if let Err(message) = self.save_open_files(cx) {
            self.report("Conflict", message, cx);
            return;
        }
        self.run(
            "Conflict",
            move |repo| repo.accept_side(&path, side),
            |this, (), cx| {
                this.reload_everything_from_disk(cx);
                this.refresh(cx);
            },
            cx,
        );
    }

    pub fn mark_resolved(&mut self, path: String, cx: &mut Context<Self>) {
        if let Err(message) = self.save_open_files(cx) {
            self.report("Conflict", message, cx);
            return;
        }
        self.run(
            "Conflict",
            move |repo| repo.mark_resolved(&path),
            |this, (), cx| {
                this.refresh(cx);
            },
            cx,
        );
    }

    pub fn switch(&mut self, branch: String, cx: &mut Context<Self>) {
        let name = branch.clone();
        self.run(
            "Switch branch",
            move |repo| repo.switch(&name),
            move |this, (), cx| {
                this.remember_recent(&branch);
                this.reload_everything(cx);
                this.tell("Branch", format!("Switched to {branch}"), cx)
            },
            cx,
        );
    }

    /// A local branch of a remote one: checkout creates it tracking the remote.
    pub fn checkout_remote(&mut self, branch: String, cx: &mut Context<Self>) {
        let name = branch.clone();
        let local = branch
            .split_once('/')
            .map_or_else(|| branch.clone(), |(_, name)| name.to_owned());
        self.run(
            "Checkout branch",
            move |repo| repo.checkout_remote(&name),
            move |this, (), cx| {
                this.remember_recent(&local);
                this.reload_everything(cx);
                this.tell("Branch", format!("Checked out {branch}"), cx)
            },
            cx,
        );
    }

    pub fn create_branch(&mut self, name: String, cx: &mut Context<Self>) {
        self.create_branch_from(name, "HEAD".to_owned(), cx);
    }

    pub fn create_branch_from(&mut self, name: String, start: String, cx: &mut Context<Self>) {
        let (made, shown) = (name.clone(), name);
        self.run(
            "New branch",
            move |repo| {
                repo.create_branch(&made, &start)?;
                repo.switch(&made)
            },
            move |this, (), cx| {
                this.remember_recent(&shown);
                this.tell("Branch", format!("Created {shown}"), cx)
            },
            cx,
        );
    }

    /// Checks `branch` out and rebases it onto the branch the user was on, as IDEA does.
    pub fn checkout_and_rebase(&mut self, branch: String, onto: String, cx: &mut Context<Self>) {
        let (name, target) = (branch.clone(), onto.clone());
        let shown = (branch.clone(), onto.clone());
        self.run(
            "Checkout and Rebase",
            move |repo| {
                repo.switch(&name)?;
                repo.rebase_onto(&target)
            },
            move |this, report, cx| {
                this.remember_recent(&shown.0);
                this.finish_branch_update(
                    report,
                    "Rebase",
                    format!("Rebased {} onto {}", shown.0, shown.1),
                    cx,
                )
            },
            cx,
        );
    }

    /// Keeps the branches the user switched to, newest first, at most five of them.
    fn remember_recent(&mut self, branch: &str) {
        self.recent.retain(|known| known != branch);
        self.recent.insert(0, branch.to_owned());
        self.recent.truncate(5);
    }

    /// Merges `branch` into the current one; a conflict opens the Conflicts window.
    pub fn merge_branch(&mut self, branch: String, cx: &mut Context<Self>) {
        if let Err(message) = self.save_open_files(cx) {
            self.report("Merge", message, cx);
            return;
        }
        let name = branch.clone();
        self.run(
            "Merge",
            move |repo| repo.merge_branch(&name),
            move |this, report, cx| {
                this.finish_branch_update(
                    report,
                    "Merge",
                    format!("Merged {branch} into the current branch"),
                    cx,
                )
            },
            cx,
        );
    }

    /// Rebases the current branch onto `branch`; a conflict opens the Conflicts window.
    pub fn rebase_onto(&mut self, branch: String, cx: &mut Context<Self>) {
        if let Err(message) = self.save_open_files(cx) {
            self.report("Rebase", message, cx);
            return;
        }
        let name = branch.clone();
        self.run(
            "Rebase",
            move |repo| repo.rebase_onto(&name),
            move |this, report, cx| {
                this.finish_branch_update(
                    report,
                    "Rebase",
                    format!("Rebased the current branch onto {branch}"),
                    cx,
                )
            },
            cx,
        );
    }

    /// What a merge or a rebase left behind: the reload, the note the user reads and the
    /// Conflicts window when git waits for the user.
    fn finish_branch_update(
        &mut self,
        report: UpdateReport,
        what: &str,
        done: String,
        cx: &mut Context<Self>,
    ) {
        self.operation = report.operation;
        self.autostash = report.autostash;
        self.merge_message = report.merge_message;
        self.reload_everything_from_disk(cx);
        if let Some(stash) = &report.stash {
            self.report(what, stash_left_message(stash), cx);
        } else if !report.conflicts.is_empty() {
            self.tell(
                what,
                format!("{} file(s) in conflict", report.conflicts.len()),
                cx,
            );
        } else if report.operation == Operation::None {
            self.tell(what, done, cx);
        }
        if !report.conflicts.is_empty() || report.operation != Operation::None {
            self.open_conflicts(cx);
        }
    }

    /// Deletes a local branch. A branch git sees as not merged waits for Force Delete.
    pub fn delete_branch(&mut self, name: String, force: bool, cx: &mut Context<Self>) {
        if force {
            self.force_delete_branch(name, cx);
            return;
        }
        let Some(repo) = self.repo.clone() else {
            return;
        };
        let job_name = name.clone();
        let job = cx.background_executor().spawn(async move {
            let head = repo
                .branch_head(&job_name)
                .map_err(|error| error.to_string())?;
            let merged = repo
                .is_branch_merged(&job_name)
                .map_err(|error| error.to_string())?;
            if merged {
                repo.delete_branch(&job_name)
                    .map_err(|error| error.to_string())?;
            }
            Ok((head, merged))
        });
        cx.spawn(async move |this, cx| {
            let result = job.await;
            this.update(cx, |this, cx| match result {
                Ok((head, true)) => this.finish_delete(name, head, cx),
                Ok((_, false)) => this.ask_force_delete(name, cx),
                Err(message) => this.report("Delete Branch", message, cx),
            })
            .ok();
        })
        .detach();
    }

    pub fn force_delete_branch(&mut self, name: String, cx: &mut Context<Self>) {
        let shown = name.clone();
        self.run(
            "Delete Branch",
            move |repo| {
                let head = repo.branch_head(&name)?;
                repo.force_delete_branch(&name)?;
                Ok(head)
            },
            move |this, head, cx| this.finish_delete(shown, head, cx),
            cx,
        );
    }

    /// The note a deleted branch leaves: Restore puts it back at its commit.
    fn finish_delete(&mut self, name: String, head: String, cx: &mut Context<Self>) {
        let short: String = head.chars().take(7).collect();
        self.last_deleted = Some((name.clone(), head));
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.notify_user_with_command(
                    "Branches",
                    format!("Deleted branch {name} (was {short})"),
                    "Restore",
                    RESTORE_BRANCH_COMMAND,
                    cx,
                )
            })
            .ok();
        cx.notify();
    }

    /// Puts the branch of the last Delete back at the commit it pointed at.
    pub fn restore_deleted_branch(&mut self, cx: &mut Context<Self>) {
        let Some((name, head)) = self.last_deleted.take() else {
            return;
        };
        let shown = name.clone();
        self.run(
            "Restore Branch",
            move |repo| repo.restore_branch(&name, &head),
            move |this, (), cx| this.tell("Branches", format!("Restored branch {shown}"), cx),
            cx,
        );
    }

    fn ask_force_delete(&mut self, name: String, cx: &mut Context<Self>) {
        let Some(window) = self.window else {
            return;
        };
        let git = cx.entity();
        let workspace = self.workspace.clone();
        let question = format!("Branch '{name}' is not merged. Force Delete?");
        let shown = name.clone();
        cx.defer(move |cx| {
            window
                .update(cx, |_, window, cx| {
                    workspace
                        .update(cx, |workspace, cx| {
                            let git = git.clone();
                            let name = shown.clone();
                            Prompt::ask(
                                "Delete Branch",
                                &question,
                                move |cx| {
                                    git.update(cx, |this, cx| {
                                        this.force_delete_branch(name.clone(), cx);
                                    });
                                },
                                workspace,
                                window,
                                cx,
                            );
                        })
                        .ok();
                })
                .ok();
        });
    }

    /// Deletes a branch on its remote after a question, as Delete of a remote branch does.
    pub fn delete_remote_branch(&mut self, branch: String, cx: &mut Context<Self>) {
        let Some(window) = self.window else {
            return;
        };
        let Some((remote, name)) = branch.split_once('/') else {
            return;
        };
        let remote = remote.to_owned();
        let name = name.to_owned();
        let git = cx.entity();
        let workspace = self.workspace.clone();
        let question = format!("Delete '{branch}' on the remote?");
        cx.defer(move |cx| {
            window
                .update(cx, |_, window, cx| {
                    workspace
                        .update(cx, |workspace, cx| {
                            let git = git.clone();
                            let (remote, name) = (remote.clone(), name.clone());
                            let shown = format!("{remote}/{name}");
                            Prompt::ask(
                                "Delete Remote Branch",
                                &question,
                                move |cx| {
                                    let (remote, name) = (remote.clone(), name.clone());
                                    let shown = shown.clone();
                                    git.update(cx, |this, cx| {
                                        this.run(
                                            "Delete Remote Branch",
                                            move |repo| repo.delete_remote_branch(&remote, &name),
                                            move |this, (), cx| {
                                                this.tell(
                                                    "Branches",
                                                    format!("Deleted {shown} on the remote"),
                                                    cx,
                                                )
                                            },
                                            cx,
                                        );
                                    })
                                },
                                workspace,
                                window,
                                cx,
                            );
                        })
                        .ok();
                })
                .ok();
        });
    }

    /// The commits of the branch and of the current one side by side, in the Compare
    /// window.
    pub fn compare_branch(&mut self, branch: String, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.clone() else {
            return;
        };
        let current = self.current_branch_name();
        let name = branch.clone();
        let job = cx
            .background_executor()
            .spawn(async move { repo.compare_with(&name).map_err(|error| error.to_string()) });
        let Some(window) = self.window else {
            return;
        };
        let git = cx.entity();
        let workspace = self.workspace.clone();
        cx.spawn(async move |this, cx| {
            let result = job.await;
            match result {
                Ok(report) => window
                    .update(cx, |_, window, cx| {
                        workspace
                            .update(cx, |workspace, cx| {
                                crate::compare::CompareView::open(
                                    git, report, branch, current, false, workspace, window, cx,
                                )
                            })
                            .ok();
                    })
                    .ok(),
                Err(message) => this
                    .update(cx, |this, cx| this.report("Compare", message, cx))
                    .ok(),
            }
        })
        .detach();
    }

    /// The files the working tree changed against `branch`, in the Compare window.
    pub fn show_working_tree_diff(&mut self, branch: String, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.clone() else {
            return;
        };
        let name = branch.clone();
        let job = cx.background_executor().spawn(async move {
            repo.working_tree_compare(&name)
                .map_err(|error| error.to_string())
        });
        let Some(window) = self.window else {
            return;
        };
        let git = cx.entity();
        let workspace = self.workspace.clone();
        cx.spawn(async move |this, cx| {
            let result = job.await;
            match result {
                Ok(report) => window
                    .update(cx, |_, window, cx| {
                        workspace
                            .update(cx, |workspace, cx| {
                                crate::compare::CompareView::open(
                                    git,
                                    report,
                                    branch,
                                    "Working Tree".to_owned(),
                                    true,
                                    workspace,
                                    window,
                                    cx,
                                )
                            })
                            .ok();
                    })
                    .ok(),
                Err(message) => this
                    .update(cx, |this, cx| this.report("Compare", message, cx))
                    .ok(),
            }
        })
        .detach();
    }

    /// The branch checked out, or `HEAD` when git points at a commit.
    pub fn current_branch_name(&self) -> String {
        self.status
            .branch
            .clone()
            .unwrap_or_else(|| "HEAD".to_owned())
    }

    /// Gives a local branch another name. The one checked out keeps its work.
    pub fn rename_branch(&mut self, from: String, to: String, cx: &mut Context<Self>) {
        let (old, new) = (from.clone(), to.clone());
        self.run(
            "Rename branch",
            move |repo| repo.rename_branch(&old, &new),
            move |this, (), cx| this.tell("Branch", format!("Renamed {from} to {to}"), cx),
            cx,
        );
    }

    /// Fetches the upstream and merges it in, as Pull does.
    pub fn pull(&mut self, cx: &mut Context<Self>) {
        self.update_with(UpdateMode::Merge, cx);
    }

    /// Takes the named files back to the last commit, as Rollback does.
    pub fn revert_paths(&mut self, paths: Vec<String>, cx: &mut Context<Self>) {
        if paths.is_empty() {
            return;
        }
        let count = paths.len();
        self.run(
            "Rollback",
            move |repo| repo.revert_paths(&paths),
            move |this, (), cx| {
                this.reload_everything_from_disk(cx);
                this.tell("Rollback", format!("Rolled back {count} file(s)"), cx)
            },
            cx,
        );
    }

    /// Alt+`: the most common VCS operations as one popup.
    pub fn quick_list(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let me = cx.weak_entity();
        let mut entries = Vec::new();
        let commit = me.clone();
        entries.push(
            MenuEntry::new("Commit...", move |window, cx| {
                commit
                    .update(cx, |this, cx| this.show_commit_window(window, cx))
                    .ok();
            })
            .icon("general/vcs"),
        );
        let update = me.clone();
        entries.push(
            MenuEntry::new("Update Project", move |_, cx| {
                update
                    .update(cx, |this, cx| this.update_project(position, cx))
                    .ok();
            })
            .icon("general/vcs"),
        );
        let push = me.clone();
        entries.push(
            MenuEntry::new("Push...", move |_, cx| {
                push.update(cx, |this, cx| this.push(cx)).ok();
            })
            .icon("general/vcs"),
        );
        let pull = me.clone();
        entries.push(
            MenuEntry::new("Pull", move |_, cx| {
                pull.update(cx, |this, cx| this.pull(cx)).ok();
            })
            .icon("general/vcs"),
        );
        entries.push(MenuEntry::separator());
        let branches = me.clone();
        entries.push(
            MenuEntry::new("Branches...", move |_, cx| {
                branches
                    .update(cx, |this, cx| this.show_branches(position, cx))
                    .ok();
            })
            .icon("general/vcs"),
        );
        let history = me.clone();
        entries.push(
            MenuEntry::new("Show History", move |window, cx| {
                history
                    .update(cx, |this, cx| {
                        this.workspace
                            .update(cx, |workspace, cx| {
                                workspace.run_command("git.show_log", window, cx)
                            })
                            .ok();
                    })
                    .ok();
            })
            .icon("general/vcs"),
        );
        let rollback = me.clone();
        entries.push(
            MenuEntry::new("Rollback...", move |window, cx| {
                rollback
                    .update(cx, |this, cx| this.show_commit_window(window, cx))
                    .ok();
            })
            .icon("general/delete"),
        );
        let stash = me;
        entries.push(
            MenuEntry::new("Stash Changes...", move |window, cx| {
                stash
                    .update(cx, |this, cx| this.ask_stash_message(window, cx))
                    .ok();
            })
            .icon("general/vcs"),
        );
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.show_menu(position, entries, cx)
            })
            .ok();
    }

    pub fn tell(&mut self, title: &str, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        let (title, text) = (title.to_string(), text.into());
        self.workspace
            .update(cx, |workspace, cx| workspace.notify_user(title, text, cx))
            .ok();
    }

    /// Takes the change under the caret, or every change the selection touches, back to
    /// the committed text.
    pub fn revert_at_caret(&mut self, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.clone() else {
            return;
        };
        let active = self
            .workspace
            .read_with(cx, |workspace, _| workspace.active_editor())
            .ok()
            .flatten();
        let Some((path, view)) = active else {
            return;
        };
        let Ok(view) = view.downcast::<EditorView>() else {
            return;
        };
        let (line, end_line) = {
            let view = view.read(cx);
            let caret = view.caret().0 as u32;
            let selection = view.selection();
            let text = view.text().unwrap_or_default();
            match selection {
                Some((range, _)) if range.start != range.end => {
                    let start = text[..range.start]
                        .bytes()
                        .filter(|byte| *byte == b'\n')
                        .count();
                    let end = text[..range.end]
                        .bytes()
                        .filter(|byte| *byte == b'\n')
                        .count();
                    (start as u32, end as u32)
                }
                _ => (caret, caret),
            }
        };
        let Some(text) = view.read(cx).text() else {
            return;
        };
        let (job_path, job_text) = (path.clone(), text.clone());
        let job = cx.background_executor().spawn(async move {
            let relative = relative_of(repo.root(), &job_path);
            let mut blobs = repo.blobs().ok()?;
            let diff = repo.file_diff(&mut blobs, &relative, &job_text).ok()?;
            let touched: Vec<Hunk> = if line == end_line {
                hunk_at(&diff.hunks, line).cloned().into_iter().collect()
            } else {
                diff.hunks
                    .iter()
                    .filter(|hunk| {
                        if hunk.after.is_empty() {
                            (line..=end_line).contains(&hunk.after.start)
                        } else {
                            hunk.after.start <= end_line && hunk.after.end > line
                        }
                    })
                    .cloned()
                    .collect()
            };
            if touched.is_empty() {
                return None;
            }
            Some(
                touched
                    .iter()
                    .map(|hunk| vcs::revert_hunk(&diff.base, &job_text, hunk))
                    .collect::<Vec<_>>(),
            )
        });
        let workspace = self.workspace.clone();
        cx.spawn(async move |this, cx| {
            let Some(edits) = job.await else {
                return;
            };
            this.update(cx, |this, cx| {
                // The text moved on while git was asked: the edit would land wrong.
                if view.read(cx).text().as_deref() != Some(text.as_str()) {
                    return;
                }
                let bytes = view.read(cx).file_bytes();
                let cause = local_history::Cause::BeforeGitRevert;
                crate::history::snapshot_bytes(&path, bytes, cause, cx);
                let edits = edits
                    .into_iter()
                    .map(|edit| text_core::Edit::new(edit.range, edit.text))
                    .collect();
                view.update(cx, |view, cx| view.apply_edits(edits, cx));
                workspace
                    .update(cx, |workspace, cx| workspace.save_editor(&path, cx))
                    .ok();
                this.refresh(cx);
            })
            .ok();
        })
        .detach();
    }

    /// Ctrl+Alt+Shift+Down and Ctrl+Alt+Shift+Up: the next or previous changed line.
    pub fn show_next_change(&mut self, cx: &mut Context<Self>) {
        self.show_change(true, cx);
    }

    pub fn show_previous_change(&mut self, cx: &mut Context<Self>) {
        self.show_change(false, cx);
    }

    fn show_change(&mut self, forward: bool, cx: &mut Context<Self>) {
        let active = self
            .workspace
            .read_with(cx, |workspace, _| workspace.active_editor())
            .ok()
            .flatten();
        let Some((_, view)) = active else {
            return;
        };
        let Ok(view) = view.downcast::<EditorView>() else {
            return;
        };
        let (line, marks) = {
            let view = view.read(cx);
            let line = view.caret().0 as u32;
            let mut marks: Vec<u32> = view
                .gutter_marks()
                .iter()
                .map(|mark| mark.lines.start)
                .collect();
            marks.sort_unstable();
            marks.dedup();
            (line, marks)
        };
        let target = if forward {
            marks.iter().copied().find(|mark| *mark > line)
        } else {
            marks.iter().rev().copied().find(|mark| *mark < line)
        };
        if let Some(target) = target {
            view.update(cx, |view, cx| view.go_to_line(target as usize, cx));
        }
    }

    /// The branch popup at `position`: search, Recent, Local and Remote branches, each
    /// with the actions of the branch. The list comes from `git for-each-ref` off the
    /// UI thread.
    pub fn show_branches(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.clone() else {
            return;
        };
        let Some(window) = self.window else {
            return;
        };
        let recent = self.recent.clone();
        let git = cx.entity();
        let workspace = self.workspace.clone();
        let job = cx
            .background_executor()
            .spawn(async move { repo.branches().map_err(|error| error.to_string()) });
        cx.spawn(async move |this, cx| {
            let result = job.await;
            match result {
                Ok(branches) => {
                    window
                        .update(cx, |_, window, cx| {
                            workspace
                                .update(cx, |workspace, cx| {
                                    crate::branches::BranchPopup::open(
                                        git, branches, recent, position, workspace, window, cx,
                                    )
                                })
                                .ok();
                        })
                        .ok();
                }
                Err(message) => {
                    this.update(cx, |this, cx| this.report("Branches", message, cx))
                        .ok();
                }
            }
        })
        .detach();
    }

    pub(crate) fn ask_stash_message(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let me = cx.weak_entity();
        self.workspace
            .update(cx, |workspace, cx| {
                Prompt::show(
                    "Stash Changes",
                    "Stash message",
                    move |message, cx| {
                        me.update(cx, |this, cx| this.stash(message, cx)).ok();
                    },
                    workspace,
                    window,
                    cx,
                );
            })
            .ok();
    }

    pub(crate) fn ask_branch_name(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ask_branch_name_from("HEAD".to_owned(), window, cx);
    }

    /// New Branch from '<branch>...': the name starts a branch at `start`.
    pub(crate) fn ask_branch_name_from(
        &mut self,
        start: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let me = cx.weak_entity();
        self.workspace
            .update(cx, |workspace, cx| {
                crate::prompt::Prompt::show(
                    "New Branch",
                    "Branch name",
                    move |name, cx| {
                        let start = start.clone();
                        me.update(cx, |this, cx| this.create_branch_from(name, start, cx))
                            .ok();
                    },
                    workspace,
                    window,
                    cx,
                );
            })
            .ok();
    }

    pub(crate) fn ask_rename_branch(
        &mut self,
        from: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let me = cx.weak_entity();
        let original = from.clone();
        self.workspace
            .update(cx, |workspace, cx| {
                crate::prompt::Prompt::show_with(
                    "Rename Branch",
                    &from,
                    move |name, cx| {
                        let name = name.trim().to_owned();
                        if name.is_empty() || name == original {
                            return;
                        }
                        me.update(cx, |this, cx| {
                            this.rename_branch(original.clone(), name, cx)
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

    pub fn show_commit_window(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.show_tool(COMMIT_TOOL, window, cx)
            })
            .ok();
    }
}

/// Marks of the hunks for the gutter of an editor.
pub fn marks_of(hunks: &[Hunk], dark: bool) -> Vec<GutterMark> {
    vcs::gutter(hunks)
        .into_iter()
        .map(|change| {
            let (kind, color) = match change.kind {
                LineChangeKind::Added => (GutterKind::Added, Kind::Added),
                LineChangeKind::Modified => (GutterKind::Modified, Kind::Modified),
                LineChangeKind::Deleted => (GutterKind::Deleted, Kind::Deleted),
            };
            GutterMark {
                lines: change.lines,
                kind,
                color: color.color(dark),
            }
        })
        .collect()
}

/// The hunk the line belongs to; a removal belongs to the line it sits before.
pub fn hunk_at(hunks: &[Hunk], line: u32) -> Option<&Hunk> {
    hunks.iter().find(|hunk| {
        if hunk.after.is_empty() {
            hunk.after.start == line
        } else {
            hunk.after.contains(&line)
        }
    })
}

/// `path` relative to the repository, with `/`, as git names it.
pub fn relative_of(root: &Path, path: &Path) -> String {
    let relative = path.strip_prefix(root).unwrap_or(path);
    relative.to_string_lossy().replace('\\', "/")
}

/// The branch in the status bar, with ahead/behind counts and the operation in progress.
fn branch_label(status: &Status, operation: Operation, autostash: bool) -> Option<String> {
    let mut label = status.branch.clone().or_else(|| {
        status
            .head
            .as_ref()
            .map(|head| format!("detached {}", &head[..head.len().min(7)]))
    })?;
    if status.ahead > 0 {
        label.push_str(&format!(" ↑{}", status.ahead));
    }
    if status.behind > 0 {
        label.push_str(&format!(" ↓{}", status.behind));
    }
    match operation {
        Operation::Merge => label.push_str(" (merging)"),
        Operation::Rebase => label.push_str(" (rebasing)"),
        Operation::None => {}
    }
    if autostash && operation != Operation::None {
        label.push_str(" · local changes will be restored when the operation ends");
    }
    Some(label)
}

/// The text for local changes git could not put back and kept in the stash.
fn stash_left_message(stash: &Stash) -> String {
    format!(
        "Local changes did not apply cleanly and are kept in stash@{{{}}}: {}, see Unstash Changes",
        stash.index, stash.message
    )
}

/// Handles the actions of the keys. The window only declares them. `window` is the one the
/// repository belongs to: the active window may be none while a script drives it.
pub fn bind_actions(git: Entity<GitState>, window: AnyWindowHandle, cx: &mut App) {
    use workspace::{PushChanges, RevertChange, ShowBranches, ShowCommit, UpdateProject};
    crate::diff_view::bind_keys(cx);
    crate::branches::bind_keys(cx);
    git.update(cx, |this, _| this.window = Some(window));
    let state = git.clone();
    cx.on_action(move |_: &PushChanges, cx| {
        let state = state.clone();
        // The window is being updated while its action runs, so the save waits for it.
        cx.defer(move |cx| state.update(cx, |this, cx| this.push(cx)));
    });
    let state = git.clone();
    cx.on_action(move |_: &UpdateProject, cx| {
        let state = state.clone();
        // The window is being updated while its action runs, so the rest waits for the effects.
        cx.defer(move |cx| {
            let position = window
                .update(cx, |_, window, cx| state.read(cx).popup_anchor(window, cx))
                .unwrap_or_default();
            state.update(cx, |this, cx| this.update_project(position, cx));
        });
    });
    let state = git.clone();
    cx.on_action(move |_: &RevertChange, cx| state.update(cx, |this, cx| this.revert_at_caret(cx)));
    let state = git.clone();
    // The window is being updated while its action runs, so the rest waits for the effects.
    cx.on_action(move |_: &ShowBranches, cx| {
        let state = state.clone();
        cx.defer(move |cx| {
            let position = window
                .update(cx, |_, window, cx| state.read(cx).popup_anchor(window, cx))
                .unwrap_or_default();
            state.update(cx, |this, cx| this.show_branches(position, cx));
        });
    });
    let state = git.clone();
    let shown = state.clone();
    cx.on_action(move |_: &ShowCommit, cx| {
        let state = shown.clone();
        cx.defer(move |cx| {
            window
                .update(cx, |_, window, cx| {
                    state.update(cx, |this, cx| this.show_commit_window(window, cx))
                })
                .ok();
        });
    });
    let state = state.clone();
    cx.on_action(move |_: &VcsQuickList, cx| {
        let state = state.clone();
        cx.defer(move |cx| {
            let position = window
                .update(cx, |_, window, cx| state.read(cx).popup_anchor(window, cx))
                .unwrap_or_default();
            state.update(cx, |this, cx| this.quick_list(position, cx));
        });
    });
}

#[cfg(test)]
mod tests {
    #[cfg(windows)]
    use std::path::Path;

    use vcs::{Change, FileStatus, Hunk};

    #[cfg(windows)]
    use super::relative_of;
    use super::{Kind, hunk_at};

    fn file(index: Change, worktree: Change) -> FileStatus {
        FileStatus {
            path: "a".into(),
            original: None,
            index,
            worktree,
        }
    }

    #[test]
    fn the_kind_says_what_the_user_cares_about() {
        let kind = |index, worktree| Kind::of(&file(index, worktree));
        assert_eq!(kind(Change::Unmodified, Change::Modified), Kind::Modified);
        assert_eq!(kind(Change::Added, Change::Unmodified), Kind::Added);
        assert_eq!(kind(Change::Unmodified, Change::Untracked), Kind::Untracked);
        assert_eq!(kind(Change::Unmodified, Change::Deleted), Kind::Deleted);
        assert_eq!(
            kind(Change::Conflicted, Change::Conflicted),
            Kind::Conflicted
        );
    }

    #[test]
    fn a_hunk_is_found_by_its_line_and_a_removal_by_the_line_after_it() {
        let hunks = [
            Hunk {
                before: 2..3,
                after: 2..4,
            },
            Hunk {
                before: 8..10,
                after: 9..9,
            },
        ];
        assert_eq!(hunk_at(&hunks, 3), Some(&hunks[0]));
        assert_eq!(hunk_at(&hunks, 9), Some(&hunks[1]));
        assert_eq!(hunk_at(&hunks, 5), None);
    }

    #[cfg(windows)]
    #[test]
    fn paths_are_relative_with_forward_slashes() {
        let root = Path::new("C:/repo");
        assert_eq!(
            relative_of(root, Path::new("C:\\repo\\src\\A.txt")),
            "src/A.txt"
        );
    }
}
