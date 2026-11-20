//! The Git Log tool window: the commits of the branch or of one file, the files a commit
//! changed, the diff of one of them and the actions a right click on a commit opens.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use vcs::graph;
use vcs::{
    Branch, Change, CommandRecord, Commit, CommitDetails, RebaseAction, RebasePlan, RefKind,
    RefLabel, Repository, ResetMode,
};
use workspace::{FileIcons, LOG_TOOL, Workspace};

use crate::ansi::AnsiParser;
use crate::commit::CommitPanel;
use crate::diff_view::DiffView;
use crate::git::{GitState, Kind, relative_of, short_commit};
use crate::prompt::{Prompt, TextField};
use crate::run::wrapped_line_element;

/// The commits one read of the log brings. A long history grows as the pointer goes down.
const PAGE: usize = 500;
const CONTEXT: &str = "GitLog";
/// The room one lane of the graph takes, the width of its line inside it and the size
/// of the dot of a commit.
const LANE_WIDTH: f32 = 12.;
const LINE_WIDTH: f32 = 2.;
const DOT_SIZE: f32 = 8.;
/// The widest graph column. A row with more lanes than this keeps them out of sight:
/// the graph is cut at the right edge of its column instead of taking the room of the
/// subject.
const MAX_LANES: usize = 10;
/// The height of a label of a branch or a tag after the subject of a commit, and the
/// width of the longest of them: a longer name is cut.
const LABEL_HEIGHT: f32 = 16.;
const LABEL_WIDTH: f32 = 180.;
/// The estimated width of one character of a label, a little above the small text, and
/// the width the labels of a row may take before the rest of them collapse into a count.
/// The layout keeps the labels within a third of the text column whatever this says.
const LABEL_CHAR_WIDTH: f32 = 7.;
const LABELS_BUDGET: f32 = 300.;
/// The width of the Rebase dialog, the width of its action cell and the height of the
/// message field of a Reword.
const REBASE_WIDTH: f32 = 760.;
const REBASE_ACTION_WIDTH: f32 = 96.;
const REBASE_MESSAGE_HEIGHT: f32 = 96.;
/// The panes of the Log and the limits of their drags. The narrower one of them keeps
/// room for a readable commit row and for the files and the details on the right.
const LEFT_WIDTH: f32 = 230.;
const RIGHT_WIDTH: f32 = 360.;
const FILES_HEIGHT: f32 = 120.;
const LEFT_MIN: f32 = 140.;
const LEFT_MAX: f32 = 520.;
const RIGHT_MIN: f32 = 260.;
const RIGHT_MAX: f32 = 720.;
const FILES_MIN: f32 = 60.;
const FILES_MAX: f32 = 640.;
const CENTER_MIN: f32 = 320.;
/// The grip between two panes, wider than its line so that the pointer finds it.
const SPLIT_WIDTH: f32 = 5.;
/// How close to the bottom of the Console the view has to be to follow a new command.
const CONSOLE_FOLLOW_MARGIN: f32 = 32.;

actions!(
    git_log_panel,
    [RewordCommit, BranchAtCommit, ShowAllAffected]
);

/// Which tab of the Git window is shown.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GitTab {
    /// The changed files of the working tree, in place of the Commit window.
    LocalChanges,
    /// The branches, the commits and the details of one of them.
    #[default]
    Log,
    /// The git commands the IDE ran, with their output.
    Console,
}

/// The Time filter of the log.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum DateFilter {
    #[default]
    All,
    Day,
    Week,
    Month,
}

impl DateFilter {
    fn label(self) -> &'static str {
        match self {
            Self::All => "All time",
            Self::Day => "Last 24 hours",
            Self::Week => "Last 7 days",
            Self::Month => "Last 30 days",
        }
    }

    /// How many seconds back the filter keeps, `None` for every commit.
    fn seconds(self) -> Option<i64> {
        match self {
            Self::All => None,
            Self::Day => Some(86_400),
            Self::Week => Some(7 * 86_400),
            Self::Month => Some(30 * 86_400),
        }
    }
}

/// The group of the branch tree of the Log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RefGroup {
    Head,
    Local,
    Remote,
    Tags,
}

impl RefGroup {
    fn key(self) -> &'static str {
        match self {
            Self::Head => "head",
            Self::Local => "local",
            Self::Remote => "remote",
            Self::Tags => "tags",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Head => "HEAD (Current Branch)",
            Self::Local => "Local",
            Self::Remote => "Remote",
            Self::Tags => "Tags",
        }
    }
}

/// One row of the branch tree: a group, a folder or a remote, a branch or a tag.
#[derive(Clone, Debug)]
struct RefRow {
    depth: usize,
    label: String,
    key: String,
    kind: RefRowKind,
}

#[derive(Clone, Copy, Debug)]
enum RefRowKind {
    Group(RefGroup),
    /// A folder or a remote, folded by the slashes of the names inside it.
    Node,
    Branch(usize),
    Tag(usize),
}

/// One row of the tree of the files of a commit.
#[derive(Clone, Debug)]
struct FileRow {
    depth: usize,
    label: String,
    key: String,
    /// The number of files under a folder, `None` for a file.
    count: Option<usize>,
    /// The index of the file in `GitLogPanel::files`.
    file: Option<usize>,
}

/// The prefix of the keys of the folders in the tree of the changed files.
const FILES_TREE: &str = "files";
/// The icon of a folder in the tree of the changed files.
const FOLDER_ICON: &str = "nodes/folder";

/// The colour kind of a file a commit changed, as Commit colours the same change: a new
/// file is green, a deleted one grey, and every other change, a move included, blue.
fn commit_change_kind(change: Change) -> Option<Kind> {
    match change {
        Change::Added => Some(Kind::Added),
        Change::Deleted => Some(Kind::Deleted),
        Change::Modified | Change::Renamed | Change::Copied | Change::TypeChanged => {
            Some(Kind::Modified)
        }
        Change::Unmodified | Change::Untracked | Change::Ignored | Change::Conflicted => None,
    }
}

/// The divider between two panes that the pointer moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SplitKind {
    Left,
    Right,
    Files,
}

struct SplitDrag {
    kind: SplitKind,
    /// Where the pointer was and what the pane measured when the drag began.
    start: f32,
    value: f32,
}

macro_rules! log_keys {
    ($(($keys:literal, $action:ident, $idea:expr, $mac:expr)),* $(,)?) => {
        /// F2, Shift+F6, Ctrl+Alt+N and Alt+Shift+A of the log.
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
                    action: concat!("git_log_panel::", stringify!($action)),
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
                action: concat!("git_log_panel::", stringify!($action)),
                context: Some(CONTEXT),
                idea: $idea,
            }),*
        ];
    };
}

log_keys![
    ("f2", RewordCommit, Some("Git.Reword.Commit"), None),
    ("shift-f6", RewordCommit, Some("Git.Reword.Commit"), None),
    (
        "ctrl-alt-n",
        BranchAtCommit,
        Some("Git.New.Branch.In.Log"),
        None
    ),
    (
        "alt-shift-a",
        ShowAllAffected,
        Some("VcsHistory.ShowAllAffected"),
        None
    ),
];

pub struct GitLogPanel {
    git: Entity<GitState>,
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    /// The Commit window as the Local Changes tab shows it. Its own header and tabs stay
    /// out of sight there.
    commit: Option<Entity<CommitPanel>>,
    tab: GitTab,
    /// `None` for the branch, `Some` for Show History of one file.
    path: Option<PathBuf>,
    /// The root chosen in the Paths filter, `None` for every root of the project.
    root: Option<String>,
    /// The roots the project has: their prefixes inside it and their names.
    roots: Vec<(String, String)>,
    commits: Vec<Commit>,
    /// The prefix of the root of each commit of `commits`.
    commit_roots: Vec<String>,
    /// The graph rows of `commits`, laid out once when the log is loaded.
    graph: Vec<graph::Row>,
    /// The branches and tags of the repository, in the order `for-each-ref` gives them.
    labels: Vec<RefLabel>,
    branches: Vec<Branch>,
    /// The tags of the tree, with the commit each one points at.
    tags: Vec<RefLabel>,
    /// The branches the user starred, by their full names.
    favourites: HashSet<String>,
    branch_query: TextField,
    /// The keys go to the branch search rather than to the filter of the commits.
    branch_search: bool,
    /// The rows of the branch tree for the text of its search.
    branch_rows: Vec<RefRow>,
    /// The groups and nodes the user folded; one not in the set is open.
    collapsed: HashSet<String>,
    /// The branch or tag the log is filtered by, `None` for the branch of HEAD.
    branch_filter: Option<String>,
    /// The branches and tags panel is out of sight.
    left_visible: bool,
    left_width: f32,
    right_width: f32,
    files_height: f32,
    split: Option<SplitDrag>,
    /// The lanes the shown rows use, for the width of the graph column.
    lanes: usize,
    /// The indices of `commits` that the filter keeps.
    shown: Vec<usize>,
    files: Vec<String>,
    /// The kind of each changed file of the selected commit by its path, for the colour of
    /// its name. A file without a kind keeps the colour of the text.
    file_kinds: HashMap<String, Kind>,
    /// The row the details and the changed files follow.
    selected: Option<usize>,
    /// Every selected row of the log, for Cherry-Pick and Compare Revisions.
    picked: HashSet<usize>,
    /// The row a Shift click extends the selection from.
    anchor: Option<usize>,
    /// The file of the right tree the pointer chose, for Diff and Revert.
    selected_file: Option<usize>,
    /// The folders of the file tree the user folded.
    files_collapsed: HashSet<String>,
    filter: TextField,
    /// The Paths filter: a folder or a file of the project, relative to its root.
    path_query: TextField,
    /// The keys go to the Paths field rather than to the filter of the commits.
    path_search: bool,
    regex: bool,
    match_case: bool,
    user: Option<String>,
    date: DateFilter,
    loading: bool,
    files_loading: bool,
    head: Option<String>,
    /// The revision of the git state the labels and the commits were loaded for, so that
    /// an operation which leaves HEAD in place, such as New Tag, still reloads the log.
    revision: u64,
    /// The commit that is selected again after the next load, as Show All Affected does.
    want: Option<String>,
    /// A load that was asked for while another one was running.
    reload_pending: bool,
    /// How many commits of the whole history are loaded, and whether more of them wait.
    offset: usize,
    more: bool,
    /// The message and the branches of the selected commit.
    details: Option<CommitDetails>,
    details_loading: bool,
    scroll_commits: ScrollHandle,
    scroll_files: ScrollHandle,
    scroll_details: ScrollHandle,
    scroll_console: ScrollHandle,
    /// The revision of the journal of git commands the Console followed last.
    console_revision: u64,
    /// The user asked for the commands the IDE runs on its own, such as `status`.
    console_background: bool,
    /// The id of the command the user picked in the Console, for Copy.
    console_selected: Option<u64>,
    _git: Subscription,
}

pub struct ActiveGitLog(pub Entity<GitLogPanel>);

impl Global for ActiveGitLog {}

impl GitLogPanel {
    pub fn create(
        git: Entity<GitState>,
        workspace: WeakEntity<Workspace>,
        commit: Option<Entity<CommitPanel>>,
        cx: &mut App,
    ) -> Entity<Self> {
        bind_keys(cx);
        let head = git.read(cx).status().head.clone();
        let revision = git.read(cx).revision();
        let panel = cx.new(|cx| {
            let observe = cx.observe(&git, |this: &mut Self, git, cx| {
                let head = git.read(cx).status().head.clone();
                let revision = git.read(cx).revision();
                if head != this.head || revision != this.revision {
                    this.head = head;
                    this.revision = revision;
                    this.reload(cx);
                }
                // Commands of the ide itself change no branch, so the Console follows the
                // journal instead.
                if this.tab == GitTab::Console && this.console_changed() {
                    cx.notify();
                }
            });
            Self {
                git,
                workspace,
                focus: cx.focus_handle(),
                commit,
                tab: GitTab::Log,
                path: None,
                root: None,
                roots: Vec::new(),
                commits: Vec::new(),
                commit_roots: Vec::new(),
                graph: Vec::new(),
                labels: Vec::new(),
                branches: Vec::new(),
                tags: Vec::new(),
                favourites: HashSet::new(),
                branch_query: TextField::default(),
                branch_search: false,
                branch_rows: Vec::new(),
                collapsed: HashSet::new(),
                branch_filter: None,
                left_visible: true,
                left_width: LEFT_WIDTH,
                right_width: RIGHT_WIDTH,
                files_height: FILES_HEIGHT,
                split: None,
                lanes: 0,
                shown: Vec::new(),
                files: Vec::new(),
                file_kinds: HashMap::new(),
                selected: None,
                picked: HashSet::new(),
                anchor: None,
                selected_file: None,
                files_collapsed: HashSet::new(),
                filter: TextField::default(),
                path_query: TextField::default(),
                path_search: false,
                regex: false,
                match_case: false,
                user: None,
                date: DateFilter::All,
                loading: false,
                files_loading: false,
                head,
                revision,
                want: None,
                reload_pending: false,
                offset: 0,
                more: false,
                details: None,
                details_loading: false,
                scroll_commits: ScrollHandle::new(),
                scroll_files: ScrollHandle::new(),
                scroll_details: ScrollHandle::new(),
                scroll_console: ScrollHandle::new(),
                console_revision: vcs::command_console().revision(),
                console_background: false,
                console_selected: None,
                _git: observe,
            }
        });
        let loaded = panel.clone();
        cx.defer(move |cx| loaded.update(cx, |panel, cx| panel.reload(cx)));
        panel
    }

    /// The tab of the Git window, for scripts and tests.
    #[cfg(test)]
    pub fn tab(&self) -> GitTab {
        self.tab
    }

    /// The Commit window as the Local Changes tab shows it, for tests.
    #[cfg(test)]
    pub fn commit_panel(&self) -> Option<Entity<CommitPanel>> {
        self.commit.clone()
    }

    /// Switches the tab of the Git window, as a click on it does.
    pub fn set_tab(&mut self, tab: GitTab, cx: &mut Context<Self>) {
        if self.tab == tab {
            return;
        }
        self.tab = tab;
        cx.notify();
    }

    /// The log of the current branch, as Alt+9 opens the window. The answer says whether
    /// the tab moved from Local Changes.
    pub fn show_log(&mut self, cx: &mut Context<Self>) -> bool {
        let moved = self.tab != GitTab::Log;
        self.set_tab(GitTab::Log, cx);
        moved
    }

    /// Opens the Console tab on the command of the last failure, as Show in Console of a
    /// notification does. A parallel command of the IDE can fail too, so the number the
    /// failure carries wins; without it the last command of the project that the user
    /// asked for is shown. The Console of one window shows the commands of its own
    /// project, never those of another one that the process runs at the same time.
    pub fn show_console(&mut self, cx: &mut Context<Self>) {
        self.set_tab(GitTab::Console, cx);
        let wanted = self.git.read(cx).last_failure();
        let entries = self.console_project_entries(cx);
        let chosen = wanted
            .and_then(|id| entries.iter().find(|entry| entry.id == id))
            .or_else(|| entries.iter().rev().find(|entry| !entry.background))
            .or_else(|| entries.iter().rev().find(|entry| entry.failed()))
            .or_else(|| entries.last());
        let Some(chosen) = chosen.cloned() else {
            cx.notify();
            return;
        };
        if chosen.background {
            self.console_background = true;
        }
        self.console_selected = Some(chosen.id);
        self.console_revision = vcs::command_console().revision();
        let index = self
            .console_rows(cx)
            .iter()
            .position(|entry| entry.id == chosen.id);
        if let Some(index) = index {
            self.scroll_console.scroll_to_item(index);
        }
        cx.notify();
    }

    /// The id of the command the Console shows as picked, for tests.
    #[cfg(test)]
    pub fn console_selected(&self) -> Option<u64> {
        self.console_selected
    }

    /// The commands the Console shows, as the tab draws them.
    #[cfg(test)]
    pub fn console_entries(&self, cx: &App) -> Vec<CommandRecord> {
        self.console_rows(cx)
    }

    /// The whole branch again, and no filter.
    pub fn show_root(&mut self, cx: &mut Context<Self>) {
        self.path = None;
        self.filter.text.clear();
        self.branch_filter = None;
        self.clear_log();
        self.want = None;
        self.reload(cx);
        cx.notify();
    }

    /// The log of one file, newest first.
    pub fn show_path(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.path = Some(path);
        self.filter.text.clear();
        self.branch_filter = None;
        self.clear_log();
        self.want = None;
        self.reload(cx);
        cx.notify();
    }

    /// Forgets the commits of the last source, keeping the tree and the filters.
    fn clear_log(&mut self) {
        self.commits.clear();
        self.commit_roots.clear();
        self.graph.clear();
        self.shown.clear();
        self.files.clear();
        self.selected = None;
        self.picked.clear();
        self.anchor = None;
        self.selected_file = None;
        self.details = None;
        self.offset = 0;
        self.more = false;
    }

    /// The Paths > Roots filter of IDEA: one root of the project, or all of them.
    pub fn set_root(&mut self, root: Option<String>, cx: &mut Context<Self>) {
        self.root = root;
        self.selected = None;
        self.picked.clear();
        self.anchor = None;
        self.selected_file = None;
        self.files.clear();
        self.details = None;
        self.want = None;
        self.reload(cx);
        cx.notify();
    }

    /// The Paths field: a folder or a file of the project, absolute or relative to the
    /// root. An empty field goes back to the log of the branch.
    fn apply_path_filter(&mut self, cx: &mut Context<Self>) {
        let text = self.path_query.text.trim().to_owned();
        if text.is_empty() {
            self.path = None;
            self.clear_log();
            self.want = None;
            self.reload(cx);
            cx.notify();
            return;
        }
        let Some(root) = self
            .git
            .read(cx)
            .repo()
            .map(|repo| repo.root().to_path_buf())
        else {
            return;
        };
        let given = PathBuf::from(&text);
        let path = if given.is_absolute() {
            given
        } else {
            root.join(text.replace('\\', "/"))
        };
        self.show_path(path, cx);
        self.path_query = TextField::from_text(text);
        self.path_search = false;
    }

    /// The file or folder of the Paths filter, for a test.
    #[cfg(test)]
    pub fn path_filter(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// The menu of the root filter: every root and the project as a whole.
    fn root_menu(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let me = cx.weak_entity();
        let mut entries = Vec::new();
        {
            let me = me.clone();
            entries.push(
                MenuEntry::new("All Roots", move |_, cx| {
                    me.update(cx, |this, cx| this.set_root(None, cx)).ok();
                })
                .icon("general/vcs"),
            );
        }
        for (prefix, label) in self.roots.clone() {
            let me = me.clone();
            entries.push(
                MenuEntry::new(label, move |_, cx| {
                    let prefix = prefix.clone();
                    me.update(cx, |this, cx| this.set_root(Some(prefix), cx))
                        .ok();
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

    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let Some(repo) = self.git.read(cx).repo() else {
            return;
        };
        if self.loading {
            self.reload_pending = true;
            return;
        }
        self.loading = true;
        self.offset = 0;
        self.more = false;
        let path = self.path.clone();
        let chosen = self.root.clone();
        let revision = self.branch_filter.clone();
        let project = repo.root().to_path_buf();
        let job = cx.background_executor().spawn(async move {
            let root_repo = chosen.clone().and_then(|prefix| {
                roots_of(&repo)
                    .into_iter()
                    .find(|(root, _)| *root == prefix)
                    .map(|(_, repository)| repository)
            });
            let (commits, commit_roots, roots) = match path {
                Some(path) => {
                    let (prefix, repository) = repository_for_path(&repo, &path);
                    let relative = relative_of(repository.root(), &path);
                    let commits = repository.file_log(&relative, PAGE, 0)?;
                    let count = commits.len();
                    (commits, vec![prefix; count], roots_of(&repo))
                }
                None if revision.is_some() => {
                    let repository = root_repo.unwrap_or_else(|| repo.fork());
                    let commits =
                        repository.log_ref(revision.as_deref().unwrap_or_default(), PAGE, 0)?;
                    let prefix = chosen.clone().unwrap_or_default();
                    let count = commits.len();
                    (commits, vec![prefix; count], roots_of(&repo))
                }
                None => match chosen {
                    Some(prefix) => {
                        let repository = root_repo.unwrap_or_else(|| repo.fork());
                        let commits = repository.log(PAGE, 0)?;
                        let count = commits.len();
                        (commits, vec![prefix; count], roots_of(&repo))
                    }
                    None => {
                        let roots = roots_of(&repo);
                        let mut merged: Vec<(String, Commit)> = Vec::new();
                        for (prefix, repository) in &roots {
                            for commit in repository.log(PAGE, 0).unwrap_or_default() {
                                merged.push((prefix.clone(), commit));
                            }
                        }
                        // One root keeps the order of git: a cherry-pick has an author
                        // time older than the commits below it.
                        if roots.len() > 1 {
                            merged.sort_by_key(|(_, commit)| std::cmp::Reverse(commit.time));
                        }
                        let (commit_roots, commits): (Vec<String>, Vec<Commit>) =
                            merged.into_iter().unzip();
                        (commits, commit_roots, roots)
                    }
                },
            };
            // The layout of a long log and the labels are the slow part of a load, so
            // they are read here and never while the window draws. A log without labels
            // is still a log.
            let graph = graph::layout(&commits);
            let mut labels = Vec::new();
            let mut branches = Vec::new();
            for (_, repository) in roots_of(&repo) {
                labels.extend(repository.labels().unwrap_or_default());
                branches.extend(repository.branches().unwrap_or_default());
            }
            let tags: Vec<RefLabel> = labels
                .iter()
                .filter(|label| label.kind == RefKind::Tag)
                .cloned()
                .collect();
            let favourites = workspace::load_git_settings(&project).favourite_branches;
            Ok::<_, std::io::Error>((
                commits,
                commit_roots,
                graph,
                labels,
                branches,
                tags,
                favourites,
                roots,
            ))
        });
        cx.spawn(async move |this, cx| {
            let result = job.await;
            this.update(cx, |this, cx| {
                this.loading = false;
                match result.map_err(|error| error.to_string()) {
                    Ok((
                        commits,
                        commit_roots,
                        graph,
                        labels,
                        branches,
                        tags,
                        favourites,
                        roots,
                    )) => {
                        this.offset = commits.len();
                        this.more = commits.len() >= PAGE;
                        this.commits = commits;
                        this.commit_roots = commit_roots;
                        this.graph = graph;
                        this.labels = labels;
                        this.branches = branches;
                        this.tags = tags;
                        this.favourites = favourites.into_iter().collect();
                        this.roots = roots
                            .into_iter()
                            .map(|(prefix, repository)| {
                                let label = root_label(repository.root(), &prefix);
                                (prefix, label)
                            })
                            .collect();
                        this.rebuild_branches();
                        this.selected = None;
                        this.picked.clear();
                        this.anchor = None;
                        this.selected_file = None;
                        this.files.clear();
                        this.details = None;
                        this.apply_filter();
                        if let Some(want) = this.want.take()
                            && let Some(index) = this
                                .shown
                                .iter()
                                .position(|index| this.commits[*index].id == want)
                        {
                            this.select(index, cx);
                        }
                    }
                    Err(message) => {
                        this.git
                            .update(cx, |git, cx| git.report("Git Log", message, cx));
                    }
                }
                if this.reload_pending {
                    this.reload_pending = false;
                    this.reload(cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Reads the next page of the same source and appends it to the log.
    fn load_more(&mut self, cx: &mut Context<Self>) {
        if self.loading || !self.more {
            return;
        }
        let Some(repo) = self.git.read(cx).repo() else {
            return;
        };
        let path = self.path.clone();
        let chosen = self.root.clone();
        let revision = self.branch_filter.clone();
        let skip = self.offset;
        self.loading = true;
        let job = cx.background_executor().spawn(async move {
            let root_repo = chosen.clone().and_then(|prefix| {
                roots_of(&repo)
                    .into_iter()
                    .find(|(root, _)| *root == prefix)
                    .map(|(_, repository)| repository)
            });
            match path {
                Some(path) => {
                    let (prefix, repository) = repository_for_path(&repo, &path);
                    let relative = relative_of(repository.root(), &path);
                    let commits = repository.file_log(&relative, PAGE, skip)?;
                    let count = commits.len();
                    Ok::<_, std::io::Error>((commits, vec![prefix; count]))
                }
                None if revision.is_some() => {
                    let repository = root_repo.unwrap_or_else(|| repo.fork());
                    let commits =
                        repository.log_ref(revision.as_deref().unwrap_or_default(), PAGE, skip)?;
                    let prefix = chosen.unwrap_or_default();
                    let count = commits.len();
                    Ok((commits, vec![prefix; count]))
                }
                None => {
                    let repository = root_repo.unwrap_or_else(|| repo.fork());
                    let commits = repository.log(PAGE, skip)?;
                    let prefix = chosen.unwrap_or_default();
                    let count = commits.len();
                    Ok((commits, vec![prefix; count]))
                }
            }
        });
        cx.spawn(async move |this, cx| {
            let result = job.await;
            this.update(cx, |this, cx| {
                this.loading = false;
                match result.map_err(|error| error.to_string()) {
                    Ok((commits, roots)) => {
                        if commits.len() < PAGE {
                            this.more = false;
                        }
                        this.offset += commits.len();
                        this.commits.extend(commits);
                        this.commit_roots.extend(roots);
                        this.graph = graph::layout(&this.commits);
                        this.apply_filter();
                    }
                    Err(message) => {
                        this.more = false;
                        this.git
                            .update(cx, |git, cx| git.report("Git Log", message, cx));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn apply_filter(&mut self) {
        let needle = self.filter.text.trim().to_owned();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs() as i64);
        let since = self.date.seconds().map(|seconds| now - seconds);
        let user = self.user.clone();
        let root = self.root.clone();
        let matcher = search::TextMatcher::new(&needle, self.regex, self.match_case);
        self.shown = self
            .commits
            .iter()
            .enumerate()
            .filter(|(index, commit)| {
                if let Some(user) = &user
                    && &commit.author != user
                {
                    return false;
                }
                if let Some(since) = since
                    && commit.time < since
                {
                    return false;
                }
                if let Some(root) = &root
                    && self.commit_roots.get(*index) != Some(root)
                {
                    return false;
                }
                needle.is_empty()
                    || matcher.is_match(&commit.id)
                    || matcher.is_match(&commit.subject)
                    || matcher.is_match(&commit.author)
            })
            .map(|(index, _)| index)
            .collect();
        // The width of the graph column follows the widest shown row, not the widest row
        // of the whole log.
        let mut lanes = 0;
        for index in &self.shown {
            let Some(row) = self.graph.get(*index) else {
                continue;
            };
            lanes = lanes.max(row.column + 1);
            for line in &row.lines {
                lanes = lanes.max(line.from + 1).max(line.to + 1);
            }
        }
        self.lanes = lanes;
        if self
            .selected
            .is_some_and(|selected| selected >= self.shown.len())
        {
            self.selected = None;
            self.picked.clear();
            self.anchor = None;
            self.selected_file = None;
            self.files.clear();
            self.details = None;
        }
    }

    /// The filter field changed: keeps the selected commit when it still matches.
    fn refilter(&mut self, cx: &mut Context<Self>) {
        let selected = self.selected_commit().map(|commit| commit.id);
        self.apply_filter();
        if let Some(id) = selected
            && let Some(index) = self
                .shown
                .iter()
                .position(|index| self.commits[*index].id == id)
        {
            self.selected = Some(index);
        }
        cx.notify();
    }

    fn set_user(&mut self, user: Option<String>, cx: &mut Context<Self>) {
        self.user = user;
        self.refilter(cx);
    }

    fn set_date(&mut self, date: DateFilter, cx: &mut Context<Self>) {
        self.date = date;
        self.refilter(cx);
    }

    /// The Branch dropdown: one branch or tag, or the branch of HEAD.
    fn set_branch_filter(&mut self, branch: Option<String>, cx: &mut Context<Self>) {
        self.branch_filter = branch;
        self.path = None;
        self.clear_log();
        self.want = None;
        self.reload(cx);
        cx.notify();
    }

    /// The user menu of the log: every author of the loaded commits.
    fn user_menu(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let me = cx.weak_entity();
        let mut names: Vec<String> = self
            .commits
            .iter()
            .map(|commit| commit.author.clone())
            .collect();
        names.sort();
        names.dedup();
        let mut entries = Vec::new();
        {
            let me = me.clone();
            entries.push(
                MenuEntry::new("All Users", move |_, cx| {
                    me.update(cx, |this, cx| this.set_user(None, cx)).ok();
                })
                .icon("general/user"),
            );
        }
        for name in names {
            let me = me.clone();
            entries.push(
                MenuEntry::new(name.clone(), move |_, cx| {
                    me.update(cx, |this, cx| this.set_user(Some(name.clone()), cx))
                        .ok();
                })
                .icon("general/user"),
            );
        }
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.show_menu(position, entries, cx)
            })
            .ok();
    }

    /// The Date menu: the last day, week or month, or every commit.
    fn date_menu(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let me = cx.weak_entity();
        let entries = [
            DateFilter::All,
            DateFilter::Day,
            DateFilter::Week,
            DateFilter::Month,
        ]
        .into_iter()
        .map(|date| {
            let me = me.clone();
            MenuEntry::new(date.label(), move |_, cx| {
                me.update(cx, |this, cx| this.set_date(date, cx)).ok();
            })
            .icon("general/calendar")
        });
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.show_menu(position, entries.collect(), cx)
            })
            .ok();
    }

    /// The Branch dropdown: the current branch and every branch and tag of the tree.
    fn branch_menu(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let me = cx.weak_entity();
        let mut entries = Vec::new();
        {
            let me = me.clone();
            entries.push(
                MenuEntry::new("All Branches", move |_, cx| {
                    me.update(cx, |this, cx| this.set_branch_filter(None, cx))
                        .ok();
                })
                .icon("general/vcs"),
            );
        }
        for branch in &self.branches {
            let me = me.clone();
            let name = branch.name.clone();
            entries.push(
                MenuEntry::new(name.clone(), move |_, cx| {
                    me.update(cx, |this, cx| {
                        this.set_branch_filter(Some(name.clone()), cx)
                    })
                    .ok();
                })
                .icon("general/vcs"),
            );
        }
        for tag in &self.tags {
            let me = me.clone();
            let name = tag.name.clone();
            entries.push(
                MenuEntry::new(name.clone(), move |_, cx| {
                    me.update(cx, |this, cx| {
                        this.set_branch_filter(Some(name.clone()), cx)
                    })
                    .ok();
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

    fn select(&mut self, index: usize, cx: &mut Context<Self>) {
        self.picked.clear();
        self.picked.insert(index);
        self.anchor = Some(index);
        self.select_row(index, cx);
    }

    /// Makes `index` the row whose files and details show, keeping the other selected
    /// rows as they are.
    fn select_row(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(commit) = self
            .shown
            .get(index)
            .and_then(|index| self.commits.get(*index))
        else {
            return;
        };
        let id = commit.id.clone();
        let source = self.shown.get(index).copied().unwrap_or(index);
        let prefix = self.commit_roots.get(source).cloned().unwrap_or_default();
        self.selected = Some(index);
        self.selected_file = None;
        self.files.clear();
        self.details = None;
        self.files_loading = true;
        self.details_loading = true;
        let Some(repo) = self.git.read(cx).repo() else {
            return;
        };
        let job_id = id.clone();
        let job = cx.background_executor().spawn(async move {
            let repository = repo
                .roots()
                .unwrap_or_default()
                .into_iter()
                .find(|(root, _)| *root == prefix)
                .map_or_else(|| repo.fork(), |(_, repository)| repository);
            let changes = repository.commit_changes(&job_id)?;
            let details = repository.commit_details(&job_id)?;
            Ok::<_, std::io::Error>((changes, details))
        });
        cx.spawn(async move |this, cx| {
            let result = job.await;
            this.update(cx, |this, cx| {
                this.files_loading = false;
                this.details_loading = false;
                // A load for a commit the user has left is dropped.
                let current = this.selected_commit().is_some_and(|commit| commit.id == id);
                if !current {
                    return;
                }
                match result {
                    Ok((changes, details)) => {
                        this.file_kinds = changes
                            .iter()
                            .filter_map(|change| {
                                commit_change_kind(change.change)
                                    .map(|kind| (change.path.clone(), kind))
                            })
                            .collect();
                        this.files = changes.into_iter().map(|change| change.path).collect();
                        this.details = Some(details);
                    }
                    Err(message) => this
                        .git
                        .update(cx, |git, cx| git.report("Git Log", message.to_string(), cx)),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Builds the rows of the branch tree for the search field and the folded nodes.
    fn rebuild_branches(&mut self) {
        let query = self.branch_query.text.trim().to_owned();
        let matched = crate::branches::matching_branches(&self.branches, &query);
        let tag_indexes =
            crate::branches::matching_names(self.tags.iter().map(|tag| tag.name.as_str()), &query);
        let collapsed = &self.collapsed;
        let mut rows: Vec<RefRow> = Vec::new();
        let group = |rows: &mut Vec<RefRow>, group: RefGroup, depth: usize| {
            rows.push(RefRow {
                depth,
                label: group.label().to_owned(),
                key: format!("group/{}", group.key()),
                kind: RefRowKind::Group(group),
            });
        };
        // HEAD (Current Branch): the branch the repository is on, and nothing else.
        let current = self.branches.iter().position(|branch| branch.current);
        if let Some(index) = current
            && (query.is_empty() || matched.contains(&index))
        {
            group(&mut rows, RefGroup::Head, 0);
            let key = "group/head";
            if !collapsed.contains(key) {
                rows.push(RefRow {
                    depth: 1,
                    label: self.branches[index].name.clone(),
                    key: format!("branch:{}", self.branches[index].name),
                    kind: RefRowKind::Branch(index),
                });
            }
        }
        let local: Vec<usize> = matched
            .iter()
            .copied()
            .filter(|index| !self.branches[*index].remote)
            .collect();
        if !local.is_empty() {
            group(&mut rows, RefGroup::Local, 0);
            let key = "group/local";
            if !collapsed.contains(key) {
                let (stars, rest) =
                    crate::branches::split_favourites(&local, &self.branches, &self.favourites);
                for index in stars {
                    let name = self.branches[index].name.clone();
                    rows.push(RefRow {
                        depth: 1,
                        label: format!("★ {name}"),
                        key: format!("branch:{name}"),
                        kind: RefRowKind::Branch(index),
                    });
                }
                let entries: Vec<(String, usize)> = rest
                    .iter()
                    .map(|index| (self.branches[*index].name.clone(), *index))
                    .collect();
                for row in crate::branches::tree_rows(&entries, "local", 1, collapsed) {
                    match row {
                        crate::branches::TreeRow::Folder { key, label, depth } => {
                            rows.push(RefRow {
                                depth,
                                label,
                                key,
                                kind: RefRowKind::Node,
                            });
                        }
                        crate::branches::TreeRow::Leaf { name, index, depth } => {
                            rows.push(RefRow {
                                depth,
                                label: name.clone(),
                                key: format!("branch:{}", self.branches[index].name),
                                kind: RefRowKind::Branch(index),
                            });
                        }
                    }
                }
            }
        }
        let remote: Vec<usize> = matched
            .iter()
            .copied()
            .filter(|index| self.branches[*index].remote)
            .collect();
        if !remote.is_empty() {
            group(&mut rows, RefGroup::Remote, 0);
            let key = "group/remote";
            if !collapsed.contains(key) {
                let (stars, rest) =
                    crate::branches::split_favourites(&remote, &self.branches, &self.favourites);
                for index in stars {
                    let name = self.branches[index].name.clone();
                    rows.push(RefRow {
                        depth: 1,
                        label: format!("★ {name}"),
                        key: format!("branch:{name}"),
                        kind: RefRowKind::Branch(index),
                    });
                }
                for (remote, entries) in crate::branches::remote_groups(&self.branches, &rest) {
                    let key = format!("remote/{remote}");
                    rows.push(RefRow {
                        depth: 1,
                        label: remote,
                        key: key.clone(),
                        kind: RefRowKind::Node,
                    });
                    if collapsed.contains(&key) {
                        continue;
                    }
                    for row in crate::branches::tree_rows(&entries, &key, 2, collapsed) {
                        match row {
                            crate::branches::TreeRow::Folder { key, label, depth } => {
                                rows.push(RefRow {
                                    depth,
                                    label,
                                    key,
                                    kind: RefRowKind::Node,
                                });
                            }
                            crate::branches::TreeRow::Leaf { name, index, depth } => {
                                rows.push(RefRow {
                                    depth,
                                    label: name.clone(),
                                    key: format!("branch:{}", self.branches[index].name),
                                    kind: RefRowKind::Branch(index),
                                });
                            }
                        }
                    }
                }
            }
        }
        if !tag_indexes.is_empty() {
            group(&mut rows, RefGroup::Tags, 0);
            let key = "group/tags";
            if !collapsed.contains(key) {
                let entries: Vec<(String, usize)> = tag_indexes
                    .iter()
                    .map(|index| (self.tags[*index].name.clone(), *index))
                    .collect();
                for row in crate::branches::tree_rows(&entries, "tags", 1, collapsed) {
                    match row {
                        crate::branches::TreeRow::Folder { key, label, depth } => {
                            rows.push(RefRow {
                                depth,
                                label,
                                key,
                                kind: RefRowKind::Node,
                            });
                        }
                        crate::branches::TreeRow::Leaf { name, index, depth } => {
                            rows.push(RefRow {
                                depth,
                                label: name.clone(),
                                key: format!("tag:{}", self.tags[index].name),
                                kind: RefRowKind::Tag(index),
                            });
                        }
                    }
                }
            }
        }
        self.branch_rows = rows;
    }

    /// A click on the tree: a group or a folder folds, a branch or a tag filters the
    /// log, and a double click checks it out.
    fn branch_click(
        &mut self,
        row: usize,
        double: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(entry) = self.branch_rows.get(row).cloned() else {
            return;
        };
        match entry.kind {
            RefRowKind::Group(_) | RefRowKind::Node => {
                if !self.collapsed.remove(&entry.key) {
                    self.collapsed.insert(entry.key);
                }
                self.rebuild_branches();
                cx.notify();
            }
            RefRowKind::Branch(index) => {
                let Some(branch) = self.branches.get(index).cloned() else {
                    return;
                };
                if double {
                    self.ask_checkout_branch(branch, window, cx);
                } else {
                    self.set_branch_filter(Some(branch.name), cx);
                }
            }
            RefRowKind::Tag(index) => {
                let Some(tag) = self.tags.get(index).cloned() else {
                    return;
                };
                if double {
                    let commit = tag.commit.clone();
                    let name = tag.name.clone();
                    self.ask_checkout_tag(name, commit, window, cx);
                } else {
                    self.set_branch_filter(Some(tag.name), cx);
                }
            }
        }
    }

    /// A double click on a branch: Switch asks first, as the log of IDEA does.
    fn ask_checkout_branch(&mut self, branch: Branch, window: &mut Window, cx: &mut Context<Self>) {
        if branch.current {
            return;
        }
        let name = branch.name.clone();
        let remote = branch.remote;
        let question = format!("Switch to branch {name}?");
        let git = self.git.clone();
        self.workspace
            .update(cx, |workspace, cx| {
                Prompt::ask(
                    "Checkout",
                    &question,
                    move |cx| {
                        if remote {
                            git.update(cx, |git, cx| git.checkout_remote(name.clone(), cx));
                        } else {
                            git.update(cx, |git, cx| git.switch(name.clone(), cx));
                        }
                    },
                    workspace,
                    window,
                    cx,
                );
            })
            .ok();
    }

    /// A double click on a tag: the commit itself is checked out, detached as git leaves it.
    fn ask_checkout_tag(
        &mut self,
        name: String,
        commit: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let question = format!("Checkout tag {name}?");
        let git = self.git.clone();
        self.workspace
            .update(cx, |workspace, cx| {
                Prompt::ask(
                    "Checkout",
                    &question,
                    move |cx| git.update(cx, |git, cx| git.checkout_revision(commit.clone(), cx)),
                    workspace,
                    window,
                    cx,
                );
            })
            .ok();
    }

    /// The star of a branch: the favourites of the project settings.
    fn toggle_favourite(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(branch) = self.branches.get(index) else {
            return;
        };
        let name = branch.name.clone();
        if !self.favourites.remove(&name) {
            self.favourites.insert(name);
        }
        let mut names: Vec<String> = self.favourites.iter().cloned().collect();
        names.sort();
        let project = self
            .workspace
            .read_with(cx, |workspace, _| workspace.root().to_path_buf())
            .ok();
        if let Some(project) = project {
            cx.background_executor()
                .spawn(async move {
                    let settings = workspace::GitProjectSettings {
                        favourite_branches: names,
                    };
                    workspace::save_git_settings(&project, &settings).ok();
                })
                .detach();
        }
        self.rebuild_branches();
        cx.notify();
    }

    /// The actions of the narrow column left of the tree.
    fn branch_toolbar(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let selected = self
            .branch_filter
            .as_ref()
            .and_then(|name| self.branches.iter().find(|branch| &branch.name == name));
        let can_delete = selected.is_some_and(|branch| !branch.current);
        let can_compare = selected.is_some();
        let buttons = vec![
            action_button(
                "log-action-panel",
                if self.left_visible {
                    "general/chevronLeft"
                } else {
                    "general/chevronRight"
                },
                if self.left_visible {
                    "Hide Branches"
                } else {
                    "Show Branches"
                },
                theme,
                cx,
                |this, _, cx| {
                    this.left_visible = !this.left_visible;
                    cx.notify();
                },
            ),
            action_button(
                "log-action-new",
                "general/add",
                "New Branch",
                theme,
                cx,
                |this, window, cx| {
                    this.git
                        .update(cx, |git, cx| git.ask_branch_name(window, cx));
                },
            ),
            action_button(
                "log-action-update",
                "general/refresh",
                "Update Project",
                theme,
                cx,
                |this, window, cx| {
                    let position = window.mouse_position();
                    this.git
                        .update(cx, |git, cx| git.update_project(position, cx));
                },
            ),
            action_button(
                "log-action-delete",
                "general/delete",
                "Delete Branch",
                theme,
                cx,
                move |this, window, cx| {
                    if can_delete {
                        this.ask_delete_branch(window, cx);
                    }
                },
            ),
            action_button(
                "log-action-compare",
                "general/diff",
                "Compare with Current",
                theme,
                cx,
                move |this, _, cx| {
                    if !can_compare {
                        return;
                    }
                    if let Some(name) = this.branch_filter.clone() {
                        this.git.update(cx, |git, cx| git.compare_branch(name, cx));
                    }
                },
            ),
            action_button(
                "log-action-more",
                "general/moreVertical",
                "More Actions",
                theme,
                cx,
                |this, window, cx| this.more_menu(window.mouse_position(), cx),
            ),
        ];
        div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(SPACE_XS))
            .flex_none()
            .w(px(CONTROL_HEIGHT))
            .py(px(SPACE_XS))
            .border_r_1()
            .border_color(theme.toolbar_border)
            .children(buttons)
            .into_any_element()
    }

    /// The rest of the actions of the branch window that do not fit the narrow column.
    fn more_menu(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let git = self.git.clone();
        let fetch = {
            let git = git.clone();
            MenuEntry::new("Fetch", move |_, cx| {
                git.update(cx, |git, cx| git.fetch(cx));
            })
            .icon("general/download")
        };
        let push = {
            let git = git.clone();
            MenuEntry::new("Push", move |window, cx| {
                git.update(cx, |git, cx| git.show_push_window(window, cx));
            })
            .icon("general/upload")
        };
        let stash = {
            let git = git.clone();
            MenuEntry::new("Stash Changes...", move |window, cx| {
                git.update(cx, |git, cx| git.ask_stash_message(window, cx));
            })
            .icon("general/vcs")
        };
        let me = cx.weak_entity();
        let collapse = MenuEntry::new("Collapse All", move |_, cx| {
            me.update(cx, |this, cx| {
                let keys: Vec<String> = this
                    .branch_rows
                    .iter()
                    .filter(|row| !matches!(row.kind, RefRowKind::Branch(_) | RefRowKind::Tag(_)))
                    .map(|row| row.key.clone())
                    .collect();
                this.collapsed.extend(keys);
                this.rebuild_branches();
                cx.notify();
            })
            .ok();
        })
        .icon("general/chevronDown");
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.show_menu(position, vec![fetch, push, stash, collapse], cx)
            })
            .ok();
    }

    /// Delete of the selected branch, after the question of the branch window.
    fn ask_delete_branch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(name) = self.branch_filter.clone() else {
            return;
        };
        let remote = self
            .branches
            .iter()
            .find(|branch| branch.name == name)
            .is_some_and(|branch| branch.remote);
        let question = format!("Delete branch {name}?");
        let git = self.git.clone();
        self.workspace
            .update(cx, |workspace, cx| {
                Prompt::ask(
                    "Delete Branch",
                    &question,
                    move |cx| {
                        if remote {
                            git.update(cx, |git, cx| git.delete_remote_branch(name.clone(), cx));
                        } else {
                            git.update(cx, |git, cx| git.delete_branch(name.clone(), false, cx));
                        }
                    },
                    workspace,
                    window,
                    cx,
                );
            })
            .ok();
    }

    fn open_diff(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(commit) = self
            .selected
            .and_then(|selected| self.shown.get(selected))
            .and_then(|index| self.commits.get(*index))
        else {
            return;
        };
        let id = commit.id.clone();
        let prefix = self
            .selected
            .and_then(|selected| self.shown.get(selected))
            .and_then(|index| self.commit_roots.get(*index))
            .cloned()
            .unwrap_or_default();
        let Some(root) = self.git.read(cx).repo().and_then(|repo| {
            repo.roots()
                .ok()
                .and_then(|roots| {
                    roots
                        .into_iter()
                        .find(|(root, _)| *root == prefix)
                        .map(|(_, repository)| repository.root().to_path_buf())
                })
                .or_else(|| Some(repo.root().to_path_buf()))
        }) else {
            return;
        };
        let files: Vec<PathBuf> = self.files.iter().map(|file| root.join(file)).collect();
        DiffView::open_commit(
            self.git.clone(),
            self.workspace.clone(),
            files,
            index,
            id,
            window,
            cx,
        );
    }

    fn selected_commit(&self) -> Option<Commit> {
        self.selected
            .and_then(|selected| self.shown.get(selected))
            .and_then(|index| self.commits.get(*index))
            .cloned()
    }

    /// F2: another message for the selected commit of the user.
    fn ask_reword(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(commit) = self.selected_commit() else {
            return;
        };
        let id = commit.id.clone();
        let subject = commit.subject.clone();
        let me = cx.weak_entity();
        self.workspace
            .update(cx, |workspace, cx| {
                Prompt::show_with(
                    "Reword Commit",
                    &subject,
                    move |message, cx| {
                        let id = id.clone();
                        me.update(cx, |this, cx| this.reword_commit(id, message, cx))
                            .ok();
                    },
                    workspace,
                    window,
                    cx,
                );
            })
            .ok();
    }

    fn reword_commit(&mut self, id: String, message: String, cx: &mut Context<Self>) {
        self.git.update(cx, |git, cx| {
            git.run(
                "Reword",
                move |repo| repo.reword(&id, &message),
                move |git, (), cx| {
                    git.tell("Reword", "The commit message is changed", cx);
                },
                cx,
            )
        });
    }

    /// Interactively Rebase from Here: reads the plan of the commits from the chosen one
    /// to HEAD, then opens the dialog of the plan. A commit that is not on the branch, or
    /// a range with a merge, is a notification instead of a dialog.
    pub(crate) fn open_rebase(
        &mut self,
        commit: String,
        window: AnyWindowHandle,
        cx: &mut Context<Self>,
    ) {
        let Some(repo) = self.git.read(cx).repo() else {
            return;
        };
        let job = cx
            .background_executor()
            .spawn(async move { repo.rebase_plan(&commit).map_err(|error| error.to_string()) });
        let workspace = self.workspace.clone();
        let git = self.git.clone();
        cx.spawn(async move |this, cx| {
            let result = job.await;
            this.update(cx, |_, cx| match result {
                Ok(plan) => {
                    window
                        .update(cx, |_, window, cx| {
                            workspace
                                .update(cx, |workspace, cx| {
                                    RebaseView::open(git.clone(), plan, workspace, window, cx)
                                })
                                .ok();
                        })
                        .ok();
                }
                Err(message) => git.update(cx, |git, cx| git.report("Rebase", message, cx)),
            })
            .ok();
        })
        .detach();
    }

    /// Ctrl+Alt+N: a branch that starts at the selected commit.
    fn ask_branch_from_commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(commit) = self.selected_commit() else {
            return;
        };
        let start = commit.id.clone();
        let me = cx.weak_entity();
        self.workspace
            .update(cx, |workspace, cx| {
                Prompt::show(
                    "New Branch",
                    "Branch name",
                    move |name, cx| {
                        let name = name.trim().to_string();
                        if name.is_empty() {
                            return;
                        }
                        let start = start.clone();
                        me.update(cx, |this, cx| this.create_branch(name, start, cx))
                            .ok();
                    },
                    workspace,
                    window,
                    cx,
                );
            })
            .ok();
    }

    fn create_branch(&mut self, name: String, start: String, cx: &mut Context<Self>) {
        let shown = name.clone();
        self.git.update(cx, |git, cx| {
            git.run(
                "New branch",
                move |repo| repo.create_branch(&name, &start),
                move |git, (), cx| {
                    git.reload_everything(cx);
                    git.tell("Branch", format!("Created {shown}"), cx);
                },
                cx,
            )
        });
    }

    /// Alt+Shift+A in the history of a file: the whole commit instead of the one file.
    fn show_all_affected(&mut self, cx: &mut Context<Self>) {
        if self.path.is_none() {
            return;
        }
        let Some(commit) = self.selected_commit() else {
            return;
        };
        self.want = Some(commit.id);
        self.path = None;
        self.filter.text.clear();
        self.reload(cx);
        cx.notify();
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let stroke = &event.keystroke;
        if self.path_search {
            if stroke.key == "escape" {
                self.path_search = false;
                cx.stop_propagation();
                cx.notify();
                return;
            }
            if stroke.key == "enter" {
                self.apply_path_filter(cx);
                cx.stop_propagation();
                cx.notify();
                return;
            }
            if self.path_query.key(event, false) {
                cx.stop_propagation();
                cx.notify();
            }
            return;
        }
        if self.branch_search {
            if stroke.key == "escape" {
                self.branch_search = false;
                cx.stop_propagation();
                cx.notify();
                return;
            }
            if self.branch_query.key(event, false) {
                self.rebuild_branches();
                cx.stop_propagation();
                cx.notify();
            }
            return;
        }
        if stroke.modifiers.control && !stroke.modifiers.alt && stroke.key == "c" {
            if self.tab == GitTab::Console {
                self.copy_console(cx);
            } else {
                self.copy_selected(cx);
            }
            cx.stop_propagation();
            return;
        }
        match stroke.key.as_str() {
            "up" => {
                self.select_relative(-1, cx);
                cx.stop_propagation();
                return;
            }
            "down" => {
                self.select_relative(1, cx);
                cx.stop_propagation();
                return;
            }
            "enter" | "f4" => {
                if self.selected.is_some() && !self.files.is_empty() {
                    let file = self.selected_file.unwrap_or(0);
                    self.open_diff(file, window, cx);
                } else if self.selected.is_none() {
                    self.select_relative(1, cx);
                }
                cx.stop_propagation();
                return;
            }
            _ => {}
        }
        if self.filter.key(event, false) {
            cx.stop_propagation();
            self.refilter(cx);
        }
    }

    /// A click on a row: a plain click selects it, Ctrl adds or removes it, and Shift
    /// takes the range from the last clicked row.
    fn click_row(&mut self, index: usize, modifiers: Modifiers, cx: &mut Context<Self>) {
        if modifiers.shift {
            let anchor = self.anchor.unwrap_or(index);
            let (from, to) = if anchor <= index {
                (anchor, index)
            } else {
                (index, anchor)
            };
            if !modifiers.control {
                self.picked.clear();
            }
            self.picked.extend(from..=to);
            self.select_row(index, cx);
        } else if modifiers.control {
            if !self.picked.remove(&index) {
                self.picked.insert(index);
            }
            if self.picked.contains(&index) {
                self.select_row(index, cx);
            } else if self.selected == Some(index) {
                self.selected = None;
                self.selected_file = None;
                self.files.clear();
                self.details = None;
                if let Some(next) = self.picked.iter().min().copied() {
                    self.select_row(next, cx);
                }
            }
            self.anchor = Some(index);
        } else {
            self.select(index, cx);
        }
    }

    /// The selected commits in the order the log shows them, newest first.
    fn picked_commits(&self) -> Vec<Commit> {
        let mut rows: Vec<usize> = self.picked.iter().copied().collect();
        rows.sort_unstable();
        rows.into_iter()
            .filter_map(|index| self.shown.get(index))
            .filter_map(|index| self.commits.get(*index))
            .cloned()
            .collect()
    }

    /// The commits the git menu and the log actions use: every picked row.
    pub fn picked_ids(&self) -> Vec<String> {
        self.picked_commits()
            .into_iter()
            .map(|commit| commit.id)
            .collect()
    }

    /// The row the details follow, for Rebase, Revert and Reset.
    pub fn focused_commit_id(&self) -> Option<String> {
        self.selected_commit().map(|commit| commit.id)
    }

    /// The two picked commits as `(older, newer)`, for Compare Revisions.
    fn picked_pair(&self) -> Option<(Commit, Commit)> {
        let mut rows: Vec<usize> = self.picked.iter().copied().collect();
        rows.sort_unstable();
        if rows.len() != 2 {
            return None;
        }
        let newer = self
            .shown
            .get(rows[0])
            .and_then(|index| self.commits.get(*index))
            .cloned()?;
        let older = self
            .shown
            .get(rows[1])
            .and_then(|index| self.commits.get(*index))
            .cloned()?;
        Some((older, newer))
    }

    /// Cherry-Pick of every picked commit, in the order git replays them.
    pub(crate) fn cherry_pick_picked(&mut self, cx: &mut Context<Self>) {
        let commits = self.picked_ids();
        if commits.is_empty() {
            return;
        }
        self.git
            .update(cx, |git, cx| git.cherry_pick_many(commits, cx));
    }

    /// Moves the selection of the commits, loading their files as a click does.
    fn select_relative(&mut self, delta: isize, cx: &mut Context<Self>) {
        let count = self.shown.len();
        if count == 0 {
            return;
        }
        let last = count as isize - 1;
        let next = match self.selected {
            Some(index) => (index as isize + delta).clamp(0, last),
            None if delta < 0 => last,
            None => 0,
        };
        self.select(next as usize, cx);
    }

    /// Ctrl+C puts the details of the selected commit on the clipboard, as the log of IDEA
    /// copies its text, and the summary when its details are still on their way.
    fn copy_selected(&mut self, cx: &mut Context<Self>) {
        let Some(commit) = self
            .selected
            .and_then(|selected| self.shown.get(selected))
            .and_then(|index| self.commits.get(*index))
        else {
            return;
        };
        let text = self.details_text().unwrap_or_else(|| {
            format!(
                "{} {} {} {}",
                commit.id,
                commit.subject,
                commit.author,
                date_of(commit.time)
            )
        });
        cx.write_to_clipboard(ClipboardItem::new_string(text));
    }

    /// The text of the details pane, as Copy of the log writes it.
    fn details_text(&self) -> Option<String> {
        let commit = self.selected_commit()?;
        let mut text = format!("{}\n\n{}", commit.subject, commit.id);
        if let Some(details) = &self.details {
            if details.message != commit.subject {
                text.push_str("\n\n");
                text.push_str(&details.message);
            }
            if !details.branches.is_empty() {
                text.push_str(&format!(
                    "\n\nIn {} branches: {}",
                    details.branches.len(),
                    details.branches.join(", ")
                ));
            }
        }
        text.push_str(&format!(
            "\n\n{} <{}>\n{} {}",
            commit.author,
            commit.email,
            date_of(commit.time),
            time_of(commit.time)
        ));
        Some(text)
    }

    /// The menu of a commit, as a right click of the log of IDEA opens it. A click inside
    /// the selected rows keeps the whole selection.
    fn commit_menu(&mut self, index: usize, position: Point<Pixels>, cx: &mut Context<Self>) {
        if self.picked.contains(&index) && self.picked.len() > 1 {
            self.select_row(index, cx);
        } else {
            self.select(index, cx);
        }
        let Some(commit) = self
            .shown
            .get(index)
            .and_then(|index| self.commits.get(*index))
            .cloned()
        else {
            return;
        };
        let id = commit.id.clone();
        let git = self.git.clone();
        let mut entries = Vec::new();

        let copied = id.clone();
        entries.push(
            MenuEntry::new("Copy Revision Number", move |_, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(copied.clone()));
            })
            .icon("general/copy"),
        );

        let checkout = id.clone();
        let state = git.clone();
        entries.push(
            MenuEntry::new("Checkout Revision", move |_, cx| {
                let commit = checkout.clone();
                state.update(cx, |git, cx| git.checkout_revision(commit, cx));
            })
            .icon("general/vcs"),
        );

        let start = id.clone();
        let state = git.clone();
        entries.push(
            MenuEntry::new("New Branch...", move |window, cx| {
                let start = start.clone();
                state.update(cx, |git, cx| git.ask_branch_at_commit(start, window, cx));
            })
            .icon("general/add"),
        );

        let tagged = id.clone();
        let state = git.clone();
        entries.push(
            MenuEntry::new("New Tag...", move |window, cx| {
                let commit = tagged.clone();
                state.update(cx, |git, cx| git.ask_tag_at_commit(commit, window, cx));
            })
            .icon("general/vcs"),
        );

        entries.push(MenuEntry::separator());

        let picked = self.picked_ids();
        let state = git.clone();
        entries.push(
            MenuEntry::new("Cherry-Pick", move |_, cx| {
                let commits = picked.clone();
                state.update(cx, |git, cx| git.cherry_pick_many(commits, cx));
            })
            .icon("general/vcs"),
        );

        let reverted = id.clone();
        let state = git.clone();
        entries.push(
            MenuEntry::new("Revert Commit", move |_, cx| {
                let commit = reverted.clone();
                state.update(cx, |git, cx| git.revert_commit(commit, cx));
            })
            .icon("general/vcs"),
        );

        entries.push(MenuEntry::separator());

        let reset = id.clone();
        let state = git.clone();
        let workspace = self.workspace.clone();
        entries.push(
            MenuEntry::new("Reset Current Branch to Here...", move |window, cx| {
                let commit = reset.clone();
                let git = state.clone();
                workspace
                    .update(cx, |workspace, cx| {
                        ResetView::open(git, commit, workspace, window, cx)
                    })
                    .ok();
            })
            .icon("general/vcs"),
        );

        let rebased = id.clone();
        let panel = cx.weak_entity();
        entries.push(
            MenuEntry::new("Interactively Rebase from Here...", move |window, cx| {
                let commit = rebased.clone();
                let window = window.window_handle();
                panel
                    .update(cx, |panel, cx| panel.open_rebase(commit, window, cx))
                    .ok();
            })
            .icon("general/vcs")
            .id("git-log-interactive-rebase"),
        );

        let compared = short_commit(&id);
        let state = git;
        entries.push(
            MenuEntry::new("Compare with Local", move |_, cx| {
                let commit = compared.clone();
                state.update(cx, |git, cx| git.show_working_tree_diff(commit, cx));
            })
            .icon("general/vcs"),
        );

        let pair = self
            .picked_pair()
            .map(|(older, newer)| (older.id, newer.id));
        let pair_enabled = pair.is_some();
        let panel = cx.weak_entity();
        entries.push(
            MenuEntry::new("Compare Revisions", move |_, cx| {
                if let Some((older, newer)) = pair.clone() {
                    panel
                        .update(cx, |panel, cx| {
                            panel
                                .git
                                .update(cx, |git, cx| git.compare_revisions(older, newer, cx));
                        })
                        .ok();
                }
            })
            .icon("general/vcs")
            .enabled(pair_enabled),
        );

        self.workspace
            .update(cx, |workspace, cx| {
                workspace.show_menu(position, entries, cx)
            })
            .ok();
    }

    pub fn focus(&self) -> FocusHandle {
        self.focus.clone()
    }

    /// The file of Show History, `None` for the branch.
    #[cfg(test)]
    pub fn path(&self) -> Option<&PathBuf> {
        self.path.as_ref()
    }

    /// The rows of the branch tree as the panel builds them, for a test.
    #[cfg(test)]
    pub fn branch_tree(&self) -> Vec<(usize, String)> {
        self.branch_rows
            .iter()
            .map(|row| (row.depth, row.label.clone()))
            .collect()
    }

    /// The row of the first branch of that name, for a click in a test.
    #[cfg(test)]
    pub fn branch_row(&self, name: &str) -> Option<usize> {
        self.branch_rows.iter().position(|row| {
            matches!(row.kind, RefRowKind::Branch(index) if self.branches.get(index).is_some_and(|branch| branch.name == name))
        })
    }

    /// The branch or tag the log is filtered by, for a test.
    #[cfg(test)]
    pub fn branch_filter(&self) -> Option<&str> {
        self.branch_filter.as_deref()
    }

    /// Types into the branch search, as the keys of its field do.
    #[cfg(test)]
    pub fn set_branch_query(&mut self, text: &str, cx: &mut Context<Self>) {
        self.branch_query = TextField::from_text(text.to_owned());
        self.branch_search = true;
        self.rebuild_branches();
        cx.notify();
    }

    /// Types into the filter of the commits, as the keys of its field do.
    #[cfg(test)]
    pub fn set_filter(&mut self, text: &str, cx: &mut Context<Self>) {
        self.filter = TextField::from_text(text.to_owned());
        self.branch_search = false;
        self.refilter(cx);
    }

    /// Chooses the author of the User filter, as its menu does.
    #[cfg(test)]
    pub fn set_user_filter(&mut self, user: Option<String>, cx: &mut Context<Self>) {
        self.set_user(user, cx);
    }

    /// Turns the regex and the case switches of the Text filter, as their buttons do.
    #[cfg(test)]
    pub fn set_regex(&mut self, regex: bool, match_case: bool, cx: &mut Context<Self>) {
        self.regex = regex;
        self.match_case = match_case;
        self.refilter(cx);
    }

    /// The names of the branches the tree shows, for a test.
    #[cfg(test)]
    pub fn visible_branches(&self) -> Vec<String> {
        self.branch_rows
            .iter()
            .filter_map(|row| match row.kind {
                RefRowKind::Branch(index) => self.branches.get(index).map(|b| b.name.clone()),
                _ => None,
            })
            .collect()
    }

    /// The files of the selected commit, for a test.
    #[cfg(test)]
    pub fn files(&self) -> Vec<String> {
        self.files.clone()
    }

    /// The details of the selected commit, for a test.
    #[cfg(test)]
    pub fn details(&self) -> Option<&CommitDetails> {
        self.details.as_ref()
    }

    /// How many commits of the history are loaded, for a test.
    #[cfg(test)]
    pub fn loaded(&self) -> usize {
        self.commits.len()
    }

    /// Whether the history has more commits than the loaded pages, for a test.
    #[cfg(test)]
    pub fn more(&self) -> bool {
        self.more
    }

    /// Reads the next page, as the scroll to its end does.
    #[cfg(test)]
    pub fn load_next(&mut self, cx: &mut Context<Self>) {
        self.load_more(cx);
    }

    /// The widths of the panes of the Log, for a test.
    #[cfg(test)]
    pub fn panes(&self) -> (f32, f32, f32) {
        (self.left_width, self.right_width, self.files_height)
    }

    /// The roots the filter offers, for a test.
    #[cfg(test)]
    pub fn roots(&self) -> Vec<(String, String)> {
        self.roots.clone()
    }

    /// The root of each shown commit, for a test.
    #[cfg(test)]
    pub fn shown_roots(&self) -> Vec<String> {
        self.shown
            .iter()
            .filter_map(|index| self.commit_roots.get(*index).cloned())
            .collect()
    }

    /// The commits the filter leaves, newest first.
    #[cfg(test)]
    pub fn commits(&self) -> Vec<&Commit> {
        self.shown
            .iter()
            .filter_map(|index| self.commits.get(*index))
            .collect()
    }

    /// The labels of one shown commit, with `true` for the branch of HEAD, for a test.
    #[cfg(test)]
    pub fn labels_of(&self, index: usize) -> Vec<(String, bool)> {
        self.shown
            .get(index)
            .and_then(|index| self.commits.get(*index))
            .map(|commit| {
                self.commit_labels(commit)
                    .into_iter()
                    .map(|label| (label.name, label.current))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The branches and tags of one commit, and HEAD when it points at no branch.
    fn commit_labels(&self, commit: &Commit) -> Vec<RefLabel> {
        let mut labels: Vec<RefLabel> = self
            .labels
            .iter()
            .filter(|label| label.commit == commit.id)
            .cloned()
            .collect();
        if self.head.as_deref() == Some(commit.id.as_str())
            && !labels.iter().any(|label| label.current)
        {
            labels.insert(
                0,
                RefLabel {
                    commit: commit.id.clone(),
                    name: "HEAD".to_owned(),
                    kind: RefKind::Local,
                    current: true,
                },
            );
        }
        labels
    }

    /// The name of the root a commit belongs to, when the project has more than one.
    fn root_of_commit(&self, index: usize) -> Option<String> {
        let prefix = self.commit_roots.get(index)?;
        self.roots
            .iter()
            .find(|(root, _)| root == prefix)
            .map(|(_, label)| label.clone())
    }

    fn commit_rows(&self, theme: &Theme, cx: &mut Context<Self>) -> Vec<Stateful<Div>> {
        let colors = graph_colors(theme);
        let lanes = self.lanes;
        let show_roots = self.roots.len() > 1;
        self.shown
            .iter()
            .enumerate()
            .map(|(index, source)| {
                let commit = &self.commits[*source];
                let selected = self.picked.contains(&index);
                let short = &commit.id[..commit.id.len().min(7)];
                let hover = theme.selection_hover;
                let subject = commit.subject.clone();
                let author = commit.author.clone();
                let committer = !commit.committer.is_empty() && commit.committer != commit.author;
                let date = format!("{} {}", date_of(commit.time), time_of(commit.time));
                let row = self.graph.get(*source).cloned().unwrap_or_default();
                let labels = self.commit_labels(commit);
                let root = show_roots.then(|| self.root_of_commit(*source)).flatten();
                div()
                    .id(("log-commit", index))
                    .debug_selector(move || format!("log-commit-{index}"))
                    .flex()
                    .items_center()
                    .gap(px(SPACE_S))
                    .flex_none()
                    .h(px(ROW_HEIGHT))
                    .px(px(ROW_PADDING_X))
                    .rounded(px(ROW_RADIUS))
                    .when(selected, |this| this.bg(theme.selection_inactive))
                    .when(!selected, |this| this.hover(move |style| style.bg(hover)))
                    .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                        this.click_row(index, event.modifiers(), cx);
                        if event.click_count() >= 2 {
                            cx.notify();
                        }
                    }))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                            this.commit_menu(index, event.position, cx)
                        }),
                    )
                    .child(graph_cell(index, row, lanes, colors))
                    .child(
                        div()
                            .w(px(64.))
                            .flex_none()
                            .font_family(MONO_FONT)
                            .text_color(theme.link)
                            .child(short.to_string()),
                    )
                    .children(root.map(|root| {
                        div()
                            .flex_none()
                            .max_w(px(LABEL_WIDTH))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .px(px(SPACE_S))
                            .rounded(px(CONTROL_RADIUS))
                            .bg(theme.selection_muted)
                            .text_color(theme.text_secondary)
                            .text_size(px(TEXT_SMALL))
                            .debug_selector(move || format!("log-root-{index}"))
                            .child(root)
                    }))
                    .child(
                        div()
                            .debug_selector(move || format!("log-text-{index}"))
                            .flex()
                            .items_center()
                            .gap(px(SPACE_S))
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .debug_selector(move || format!("log-subject-{index}"))
                                    .child(subject),
                            )
                            .child(labels_cell(index, &labels, theme)),
                    )
                    .child(
                        div()
                            .w(px(140.))
                            .flex_none()
                            .flex()
                            .items_center()
                            .gap(px(SPACE_XS))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_color(theme.text_secondary)
                            .debug_selector(move || format!("log-author-{index}"))
                            .child(div().min_w_0().truncate().child(author))
                            .when(committer, |this| {
                                this.child(
                                    div()
                                        .debug_selector(move || format!("log-author-star-{index}"))
                                        .text_color(theme.text_secondary)
                                        .child("★"),
                                )
                            }),
                    )
                    .child(
                        div()
                            .w(px(116.))
                            .flex_none()
                            .truncate()
                            .text_color(theme.text_secondary)
                            .debug_selector(move || format!("log-date-{index}"))
                            .child(date),
                    )
            })
            .collect()
    }

    /// The files of the selected commit as a tree, folded by the slashes of their names.
    fn file_tree_rows(&self) -> Vec<FileRow> {
        let entries: Vec<(String, usize)> = self
            .files
            .iter()
            .enumerate()
            .map(|(index, path)| (path.clone(), index))
            .collect();
        // The keys of the folders carry the prefix of the tree, so a count finds its folder.
        let mut counts: HashMap<String, usize> = HashMap::new();
        for path in &self.files {
            let parts: Vec<&str> = path.split('/').collect();
            for depth in 1..parts.len() {
                let key = format!("{FILES_TREE}/{}", parts[..depth].join("/"));
                *counts.entry(key).or_default() += 1;
            }
        }
        crate::branches::tree_rows(&entries, FILES_TREE, 0, &self.files_collapsed)
            .into_iter()
            .map(|row| match row {
                crate::branches::TreeRow::Folder { key, label, depth } => {
                    let last = key.rsplit('/').next().unwrap_or(&label).to_owned();
                    FileRow {
                        depth,
                        count: counts.get(&key).copied(),
                        label: last,
                        key,
                        file: None,
                    }
                }
                crate::branches::TreeRow::Leaf { name, index, depth } => FileRow {
                    depth,
                    label: name.rsplit('/').next().unwrap_or(&name).to_owned(),
                    key: format!("file/{index}"),
                    count: None,
                    file: Some(index),
                },
            })
            .collect()
    }

    /// The icons of the files, as the workspace gives them to the tabs and the project tree.
    fn file_icons(&self, cx: &App) -> FileIcons {
        self.workspace
            .read_with(cx, |workspace, _| workspace.icons().clone())
            .unwrap_or_default()
    }

    /// The icon of a row of the Changed files tree: a folder has the folder icon, and a file
    /// the icon of its type.
    fn row_icon(&self, icons: &FileIcons, entry: &FileRow) -> &'static str {
        match entry.file {
            Some(index) => match self.files.get(index) {
                Some(path) => icons.icon_for(Path::new(path)),
                None => icons.fallback,
            },
            None => FOLDER_ICON,
        }
    }

    /// The label and the icon of each row of the Changed files tree, in the order of the rows.
    #[cfg(test)]
    pub(crate) fn tree_icons(&self, cx: &App) -> Vec<(String, &'static str)> {
        let icons = self.file_icons(cx);
        self.file_tree_rows()
            .iter()
            .map(|entry| (entry.label.clone(), self.row_icon(&icons, entry)))
            .collect()
    }

    fn file_rows(&self, theme: &Theme, cx: &mut Context<Self>) -> Vec<Stateful<Div>> {
        let icons = self.file_icons(cx);
        self.file_tree_rows()
            .into_iter()
            .enumerate()
            .map(|(row, entry)| {
                let selected = entry.file.is_some() && self.selected_file == entry.file;
                let debug = entry.file.map_or_else(
                    || format!("log-folder-{row}"),
                    |file| format!("log-file-{file}"),
                );
                // A file takes the colour of its change, as Commit colours it; a folder keeps
                // the colour of the text.
                let kind = entry
                    .file
                    .and_then(|file| self.files.get(file))
                    .and_then(|path| self.file_kinds.get(path))
                    .copied();
                let file = entry.file;
                let key = entry.key.clone();
                let node = TreeRow {
                    id: ("log-file-row", row).into(),
                    depth: entry.depth,
                    label: entry.label.clone().into(),
                    hint: entry.count.map(|count| format!("({count})").into()),
                    icon: self.row_icon(&icons, &entry),
                    expandable: file.is_none(),
                    open: !self.files_collapsed.contains(&key),
                    selected,
                };
                tree_row(node, theme)
                    .debug_selector(move || debug)
                    .when_some(kind, |this, kind| this.text_color(kind.color(theme)))
                    .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                        if let Some(file) = file {
                            this.selected_file = Some(file);
                            if event.click_count() >= 2 {
                                this.open_diff(file, window, cx);
                            }
                            cx.notify();
                        } else if !this.files_collapsed.remove(&key) {
                            this.files_collapsed.insert(key.clone());
                            cx.notify();
                        } else {
                            cx.notify();
                        }
                    }))
            })
            .collect()
    }

    /// Diff of the file chosen in the tree of the commit.
    fn open_selected_diff(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(file) = self.selected_file {
            self.open_diff(file, window, cx);
        }
    }

    /// Revert of the chosen file of the commit: the file goes back to the state of the
    /// parent of the commit, in the index and the working tree.
    fn revert_selected_file(&mut self, cx: &mut Context<Self>) {
        let Some(commit) = self.selected_commit() else {
            return;
        };
        let Some(parent) = commit.parents.first().cloned() else {
            self.report_log("Revert", "The first commit has no parent to go back to", cx);
            return;
        };
        let Some(file) = self
            .selected_file
            .and_then(|index| self.files.get(index).cloned())
        else {
            return;
        };
        let shown = file.clone();
        self.git.update(cx, |git, cx| {
            git.run(
                "Revert",
                move |repo| repo.restore_paths_from(&parent, std::slice::from_ref(&file)),
                move |git, (), cx| git.tell("Revert", format!("Reverted {shown}"), cx),
                cx,
            )
        });
    }

    fn report_log(&mut self, title: &str, message: &str, cx: &mut Context<Self>) {
        let message = message.to_owned();
        self.git
            .update(cx, |git, cx| git.report(title, message, cx));
    }

    fn expand_files(&mut self, open: bool, cx: &mut Context<Self>) {
        self.files_collapsed.clear();
        if !open {
            for row in self.file_tree_rows() {
                if row.file.is_none() {
                    self.files_collapsed.insert(row.key);
                }
            }
        }
        cx.notify();
    }

    /// The right top pane: the files of the selected commit.
    fn files_panel(
        &self,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let rows = self.file_rows(theme, cx);
        let has_file = self.selected_file.is_some();
        let button = |id: &'static str,
                      name: &'static str,
                      label: &'static str,
                      enabled: bool,
                      theme: &Theme| {
            icon_button(id, name, label, None, theme)
                .debug_selector(move || id.to_owned())
                .when(!enabled, |this| this.opacity(0.4))
        };
        div()
            .id("log-files")
            .debug_selector(|| "log-files".to_owned())
            .flex()
            .flex_col()
            .flex_none()
            .h(px(self.files_height))
            .w_full()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .flex_none()
                    .h(px(CONTROL_HEIGHT))
                    .px(px(SPACE_S))
                    .border_b_1()
                    .border_color(theme.toolbar_border)
                    .child(
                        div()
                            .text_color(theme.text_secondary)
                            .debug_selector(|| "log-files-title".to_owned())
                            .child(if self.selected.is_some() {
                                format!("Changed files ({})", self.files.len())
                            } else {
                                "Changed files".to_owned()
                            }),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(SPACE_XS))
                            .child(
                                button("log-files-diff", "general/diff", "Diff", has_file, theme)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.open_selected_diff(window, cx)
                                    })),
                            )
                            .child(
                                button(
                                    "log-files-revert",
                                    "general/rollback",
                                    "Revert",
                                    has_file,
                                    theme,
                                )
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.revert_selected_file(cx)),
                                ),
                            )
                            .child(
                                button(
                                    "log-files-expand",
                                    "general/expandAll",
                                    "Expand",
                                    true,
                                    theme,
                                )
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.expand_files(true, cx)),
                                ),
                            )
                            .child(
                                button(
                                    "log-files-collapse",
                                    "general/collapseAll",
                                    "Collapse",
                                    true,
                                    theme,
                                )
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.expand_files(false, cx)),
                                ),
                            ),
                    ),
            )
            .child(
                scrollable(
                    "log-file-list",
                    &self.scroll_files,
                    div()
                        .id("log-file-list")
                        .flex()
                        .flex_col()
                        .p(px(SPACE_XS))
                        .overflow_y_scroll()
                        .track_scroll(&self.scroll_files)
                        .children(rows)
                        .when(self.files.is_empty(), |this| {
                            this.p_1().text_color(theme.text_secondary).child(
                                if self.selected.is_some() {
                                    "No files"
                                } else {
                                    "Select commit to view changes"
                                },
                            )
                        }),
                    theme,
                    window,
                    cx,
                )
                .flex_1()
                .min_h_0(),
            )
            .into_any_element()
    }

    /// The right bottom pane: the message of the commit, its author, its hash and the
    /// branches it belongs to.
    fn details_panel(
        &self,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(commit) = self.selected_commit() else {
            return div()
                .id("log-details")
                .debug_selector(|| "log-details".to_owned())
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .border_t_1()
                .border_color(theme.toolbar_border)
                .child(
                    div()
                        .p(px(SPACE_M))
                        .text_color(theme.text_secondary)
                        .child("Commit details"),
                )
                .into_any_element();
        };
        let message = self
            .details
            .as_ref()
            .map(|details| details.message.clone())
            .filter(|message| !message.is_empty())
            .unwrap_or_else(|| commit.subject.clone());
        let branches = self
            .details
            .as_ref()
            .map(|details| details.branches.clone())
            .unwrap_or_default();
        let branches = if branches.is_empty() {
            "In 0 branches".to_owned()
        } else {
            format!("In {} branches: {}", branches.len(), branches.join(", "))
        };
        let rows = vec![
            div()
                .text_size(px(TEXT_TITLE))
                .font_weight(FontWeight::MEDIUM)
                .debug_selector(|| "log-details-subject".to_owned())
                .child(commit.subject.clone())
                .into_any_element(),
            div()
                .debug_selector(|| "log-details-message".to_owned())
                .whitespace_normal()
                .child(message)
                .into_any_element(),
            div()
                .font_family(MONO_FONT)
                .text_color(theme.link)
                .debug_selector(|| "log-details-hash".to_owned())
                .child(commit.id.clone())
                .into_any_element(),
            div()
                .text_color(theme.text_secondary)
                .debug_selector(|| "log-details-author".to_owned())
                .child(format!("{} <{}>", commit.author, commit.email))
                .into_any_element(),
            div()
                .text_color(theme.text_secondary)
                .debug_selector(|| "log-details-date".to_owned())
                .child(format!("{} {}", date_of(commit.time), time_of(commit.time)))
                .into_any_element(),
            div()
                .text_color(theme.text_secondary)
                .debug_selector(|| "log-details-branches".to_owned())
                .child(branches)
                .into_any_element(),
        ];
        div()
            .id("log-details")
            .debug_selector(|| "log-details".to_owned())
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .border_t_1()
            .border_color(theme.toolbar_border)
            .child(
                scrollable(
                    "log-details-scroll",
                    &self.scroll_details,
                    div()
                        .id("log-details-scroll")
                        .flex()
                        .flex_col()
                        .gap(px(SPACE_S))
                        .p(px(SPACE_M))
                        .overflow_y_scroll()
                        .track_scroll(&self.scroll_details)
                        .children(rows),
                    theme,
                    window,
                    cx,
                )
                .flex_1()
                .min_h_0(),
            )
            .into_any_element()
    }

    /// The text of a branch or tag with the part of the search highlighted.
    fn highlighted(&self, label: &str, theme: &Theme) -> AnyElement {
        let query = self.branch_query.text.trim();
        if query.is_empty() {
            return div().child(label.to_owned()).into_any_element();
        }
        let lower = label.to_lowercase();
        let needle = query.to_lowercase();
        let Some(at) = lower.find(&needle) else {
            return div().child(label.to_owned()).into_any_element();
        };
        let end = at + needle.len();
        div()
            .flex()
            .items_center()
            .child(label[..at].to_owned())
            .child(
                div()
                    .text_color(theme.over_accent)
                    .bg(theme.accent)
                    .rounded(px(2.))
                    .child(label[at..end].to_owned()),
            )
            .child(label[end..].to_owned())
            .into_any_element()
    }

    /// The View Options of the Log: the branch tree and the folders of the commit.
    fn view_menu(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let me = cx.weak_entity();
        let left = me.clone();
        let show_branches = MenuEntry::new("Show Branch Tree", move |_, cx| {
            left.update(cx, |this, cx| {
                this.left_visible = !this.left_visible;
                cx.notify();
            })
            .ok();
        })
        .icon("general/vcs");
        let open = me.clone();
        let expand = MenuEntry::new("Expand All Folders", move |_, cx| {
            open.update(cx, |this, cx| this.expand_files(true, cx)).ok();
        })
        .icon("general/chevronDown");
        let close = me.clone();
        let collapse = MenuEntry::new("Collapse All Folders", move |_, cx| {
            close
                .update(cx, |this, cx| this.expand_files(false, cx))
                .ok();
        })
        .icon("general/chevronRight");
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.show_menu(position, vec![show_branches, expand, collapse], cx)
            })
            .ok();
    }

    /// The left pane: the action column and the tree of branches and tags.
    fn branches_panel(
        &self,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let branch_empty = self.branch_query.text.is_empty();
        let toolbar = self.branch_toolbar(theme, cx);
        let rows: Vec<AnyElement> = self
            .branch_rows
            .iter()
            .enumerate()
            .map(|(index, row)| {
                let depth = row.depth;
                let label = row.label.clone();
                let tip: SharedString = row.label.clone().into();
                let debug = match row.kind {
                    RefRowKind::Group(group) => format!("log-branch-group-{}", group.key()),
                    RefRowKind::Node => format!("log-branch-node-{index}"),
                    RefRowKind::Branch(_) => format!("log-branch-{index}"),
                    RefRowKind::Tag(_) => format!("log-tag-{index}"),
                };
                let selected = match row.kind {
                    RefRowKind::Branch(branch) => self
                        .branches
                        .get(branch)
                        .is_some_and(|branch| self.branch_filter.as_deref() == Some(&branch.name)),
                    RefRowKind::Tag(tag) => self
                        .tags
                        .get(tag)
                        .is_some_and(|tag| self.branch_filter.as_deref() == Some(&tag.name)),
                    _ => false,
                };
                let leaf = !matches!(row.kind, RefRowKind::Group(_) | RefRowKind::Node);
                let open = !self.collapsed.contains(&row.key);
                let icon = if leaf {
                    match row.kind {
                        RefRowKind::Tag(_) => "general/tag",
                        _ => "general/vcs",
                    }
                } else if open {
                    "general/chevronDown"
                } else {
                    "general/chevronRight"
                };
                let kind = row.kind;
                div()
                    .id(("log-branch-row", index))
                    .debug_selector(move || debug)
                    .flex()
                    .items_center()
                    .gap(px(SPACE_XS))
                    .flex_none()
                    .h(px(ROW_HEIGHT))
                    .pl(px(ROW_PADDING_X + depth as f32 * SPACE_L))
                    .pr(px(ROW_PADDING_X))
                    .rounded(px(ROW_RADIUS))
                    .when(selected, |this| this.bg(theme.selection_inactive))
                    .hover({
                        let hover = theme.selection_hover;
                        move |style| style.bg(hover)
                    })
                    .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                        this.branch_click(index, event.click_count() >= 2, window, cx)
                    }))
                    .tooltip(move |_, cx| tooltip(tip.clone(), None, cx))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                            if let RefRowKind::Branch(branch) = kind {
                                this.favourite_menu(branch, event.position, cx);
                            }
                        }),
                    )
                    .child(icon_sized(icon, TEXT_SMALL, theme))
                    .children((!leaf).then(|| {
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .font_weight(FontWeight::MEDIUM)
                            .child(label.clone())
                    }))
                    .children(leaf.then(|| {
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .child(self.highlighted(&label, theme))
                    }))
                    .into_any_element()
            })
            .collect();
        div()
            .id("log-branches")
            .debug_selector(|| "log-branches".to_owned())
            .flex()
            .flex_row()
            .flex_none()
            .w(px(self.left_width))
            .h_full()
            .child(toolbar)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .id("log-branch-search")
                            .debug_selector(|| "log-branch-search".to_owned())
                            .flex()
                            .items_center()
                            .flex_none()
                            .h(px(CONTROL_HEIGHT))
                            .mx(px(SPACE_S))
                            .my(px(SPACE_XS))
                            .px(px(SPACE_S))
                            .rounded(px(CONTROL_RADIUS))
                            .border_1()
                            .border_color(theme.popup_border)
                            .children(text_field_children_named(
                                "git-log-branch-search",
                                &self.branch_query,
                                theme.text,
                                theme.text,
                                theme.selection,
                                theme.caret,
                                16.,
                                self.branch_search,
                            ))
                            .when(branch_empty, |this| {
                                this.child(
                                    div()
                                        .text_color(theme.text_secondary)
                                        .child("Branch or tag"),
                                )
                            })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.branch_search = true;
                                this.path_search = false;
                                cx.notify();
                            })),
                    )
                    .child(
                        scrollable(
                            "log-branch-list",
                            &self.scroll_commits,
                            div()
                                .id("log-branch-list")
                                .flex()
                                .flex_col()
                                .p(px(SPACE_XS))
                                .overflow_y_scroll()
                                .children(rows),
                            theme,
                            window,
                            cx,
                        )
                        .flex_1()
                        .min_h_0(),
                    ),
            )
            .into_any_element()
    }

    /// The menu of a branch of the tree: the star of the favourites.
    fn favourite_menu(&mut self, index: usize, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(branch) = self.branches.get(index) else {
            return;
        };
        let name = branch.name.clone();
        let favourite = self.favourites.contains(&name);
        let label = if favourite {
            "Remove from Favourites"
        } else {
            "Add to Favourites"
        };
        let entry = MenuEntry::new(label, move |_, cx| {
            if let Some(panel) = cx.try_global::<ActiveGitLog>() {
                let panel = panel.0.clone();
                panel.update(cx, |panel, cx| panel.toggle_favourite(index, cx));
            }
        })
        .icon("general/star");
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.show_menu(position, vec![entry], cx)
            })
            .ok();
    }

    /// The commands of the Console tab: the commands of the IDE itself only when the user
    /// asked for them. The journal of the process holds them, so the tab reads it as it
    /// draws.
    fn console_rows(&self, cx: &App) -> Vec<CommandRecord> {
        self.console_project_entries(cx)
            .into_iter()
            .filter(|entry| self.console_background || !entry.background)
            .collect()
    }

    /// The commands of this project: the working directories inside the root, which is
    /// the repository and its checked-out submodules. Commands of another project that
    /// the process runs at the same time stay out of the Console of this window.
    fn console_project_entries(&self, cx: &App) -> Vec<CommandRecord> {
        match self.project_root(cx) {
            Some(root) => vcs::command_console().entries_under(&root),
            None => Vec::new(),
        }
    }

    /// The root of the repository of the window.
    fn project_root(&self, cx: &App) -> Option<PathBuf> {
        self.git
            .read(cx)
            .repo()
            .map(|repo| repo.root().to_path_buf())
    }

    /// Notices the commands that ran since the tab read the journal last. The answer says
    /// whether there are new ones.
    fn console_changed(&mut self) -> bool {
        let revision = vcs::command_console().revision();
        if revision == self.console_revision {
            return false;
        }
        self.console_revision = revision;
        if self.console_at_bottom() {
            self.scroll_console.scroll_to_bottom();
        }
        true
    }

    /// Whether the view is at the bottom of the Console, so a new command scrolls it
    /// down; a reader who scrolled up keeps the command in sight.
    fn console_at_bottom(&self) -> bool {
        let max = self.scroll_console.max_offset().y;
        max <= px(0.) || self.scroll_console.offset().y <= -max + px(CONSOLE_FOLLOW_MARGIN)
    }

    /// Clear: drops the commands of this project from the journal. The commands of
    /// another project of the same process stay, as they belong to its own Console.
    fn clear_console(&mut self, cx: &mut Context<Self>) {
        if let Some(root) = self.project_root(cx) {
            vcs::command_console().clear_under(&root);
        }
        self.console_selected = None;
        self.console_revision = vcs::command_console().revision();
        cx.notify();
    }

    /// Ctrl+C and Copy of the toolbar: the picked command, or every command the tab shows.
    fn copy_console(&mut self, cx: &mut Context<Self>) {
        let rows = self.console_rows(cx);
        let text = match self.console_selected {
            Some(id) => rows
                .iter()
                .find(|entry| entry.id == id)
                .map(CommandRecord::text)
                .unwrap_or_default(),
            None => rows
                .iter()
                .map(CommandRecord::text)
                .collect::<Vec<_>>()
                .join("\n"),
        };
        if text.is_empty() {
            return;
        }
        cx.write_to_clipboard(ClipboardItem::new_string(text));
    }

    /// The Console tab: the tools over the commands, in the order they ran.
    fn console_panel(
        &mut self,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.console_changed();
        let theme = *theme;
        let rows = self.console_rows(cx);
        let empty = rows.is_empty();
        let list = scrollable(
            "git-console",
            &self.scroll_console,
            div()
                .id("git-console")
                .debug_selector(|| "git-console".to_owned())
                .flex()
                .flex_col()
                .gap(px(SPACE_S))
                .p(px(SPACE_M))
                .overflow_y_scroll()
                .track_scroll(&self.scroll_console)
                .font_family(MONO_FONT)
                .text_size(px(CODE_SIZE))
                .text_color(theme.text)
                .children(
                    rows.iter()
                        .enumerate()
                        .map(|(index, entry)| self.console_row(index, entry, &theme, cx)),
                )
                .when(empty, |this| {
                    this.items_center()
                        .justify_center()
                        .py(px(SPACE_M))
                        .text_color(theme.text_secondary)
                        .child("No git commands yet")
                }),
            &theme,
            window,
            cx,
        );
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(self.console_toolbar(&theme, cx))
            .child(list.flex_1().min_h_0())
            .into_any_element()
    }

    /// Show background commands, Copy and Clear of the Console.
    fn console_toolbar(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let background = self.console_background;
        let (hover, pressed) = (theme.hover, theme.pressed);
        div()
            .flex()
            .items_center()
            .gap(px(SPACE_S))
            .flex_none()
            .h(px(CONTROL_HEIGHT))
            .px(px(SPACE_M))
            .border_b_1()
            .border_color(theme.toolbar_border)
            .child(
                div()
                    .id("git-console-background")
                    .debug_selector(|| "git-console-background".to_owned())
                    .flex()
                    .items_center()
                    .gap(px(SPACE_XS))
                    .flex_none()
                    .h(px(CONTROL_HEIGHT))
                    .px(px(SPACE_S))
                    .rounded(px(CONTROL_RADIUS))
                    .text_size(px(TEXT_SMALL))
                    .when(background, |this| this.bg(pressed))
                    .when(!background, |this| this.hover(move |style| style.bg(hover)))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.console_background = !this.console_background;
                        this.console_selected = None;
                        cx.notify();
                    }))
                    .child(checkbox(background, theme))
                    .child("Show background commands"),
            )
            .child(div().flex_1())
            .child(
                icon_button("git-console-copy", "general/copy", "Copy", None, theme)
                    .debug_selector(|| "git-console-copy".to_owned())
                    .on_click(cx.listener(|this, _, _, cx| this.copy_console(cx))),
            )
            .child(
                icon_button("git-console-clear", "general/clear", "Clear", None, theme)
                    .debug_selector(|| "git-console-clear".to_owned())
                    .on_click(cx.listener(|this, _, _, cx| this.clear_console(cx))),
            )
            .into_any_element()
    }

    /// One command of the Console: when it ran, its line and its output. A command that
    /// failed, and what came from stderr, are red.
    fn console_row(
        &self,
        index: usize,
        entry: &CommandRecord,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let failed = entry.failed();
        let color = if failed { theme.error } else { theme.text };
        let selected = self.console_selected == Some(entry.id);
        let row = format!("git-console-{index}");
        let error = format!("git-console-error-{index}");
        let id = entry.id;
        let time = time_of((entry.time / 1_000) as i64);
        let duration = format!("{} ms", entry.duration.as_millis());
        let code = match entry.code {
            Some(code) => format!("exit {code}"),
            None => "not started".to_owned(),
        };
        let mut output: Vec<AnyElement> = Vec::new();
        for line in output_lines(&entry.stdout, false) {
            output.push(wrapped_line_element(&line, theme));
        }
        for line in output_lines(&entry.stderr, true) {
            output.push(wrapped_line_element(&line, theme));
        }
        div()
            .id(("git-console-row", index))
            .debug_selector(move || row.clone())
            .flex()
            .flex_col()
            .gap(px(SPACE_XS))
            .p(px(SPACE_S))
            .rounded(px(CONTROL_RADIUS))
            .when(selected, |this| this.bg(theme.selection))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    window.focus(&this.focus, cx);
                    this.console_selected = Some(id);
                    cx.notify();
                }),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(SPACE_S))
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(TEXT_SMALL))
                            .text_color(theme.text_muted)
                            .child(time),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .font_family(MONO_FONT)
                            .text_size(px(CODE_SIZE))
                            .whitespace_normal()
                            .text_color(color)
                            .when(failed, |this| this.debug_selector(move || error.clone()))
                            .child(entry.line()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(TEXT_SMALL))
                            .text_color(theme.text_muted)
                            .child(duration),
                    )
                    .when(failed, |this| {
                        this.child(
                            div()
                                .flex_none()
                                .text_size(px(TEXT_SMALL))
                                .text_color(theme.error)
                                .child(code),
                        )
                    }),
            )
            .children(output)
            .into_any_element()
    }

    /// The tab strip of the Git window.
    fn tab_strip(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let tab = |id: &'static str, label: &'static str, target: GitTab, theme: &Theme| {
            let active = self.tab == target;
            div()
                .id(id)
                .debug_selector(move || id.to_owned())
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
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, _, cx| this.set_tab(target, cx)),
                )
                .child(label)
        };
        div()
            .flex()
            .items_center()
            .gap(px(SPACE_XS))
            .flex_none()
            .child(tab(
                "git-tab-local",
                "Local Changes",
                GitTab::LocalChanges,
                theme,
            ))
            .child(tab("git-tab-log", "Log", GitTab::Log, theme))
            .child(tab("git-tab-console", "Console", GitTab::Console, theme))
            .into_any_element()
    }

    /// The filters over the commits: the text, the Branch, User, Date and Paths
    /// dropdowns, Refresh and Cherry-pick.
    fn filter_bar(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let filter_empty = self.filter.text.is_empty();
        let path_empty = self.path_query.text.is_empty();
        let dropdown = |id: &'static str, label: String, theme: &Theme| {
            div()
                .id(id)
                .debug_selector(move || id.to_owned())
                .flex()
                .items_center()
                .flex_none()
                .h(px(CONTROL_HEIGHT))
                .px(px(SPACE_S))
                .rounded(px(CONTROL_RADIUS))
                .border_1()
                .border_color(theme.popup_border)
                .cursor_pointer()
                .hover({
                    let hover = theme.hover;
                    move |style| style.bg(hover)
                })
                .child(label)
        };
        let can_cherry = !self.picked.is_empty();
        let branch_label = self
            .branch_filter
            .clone()
            .unwrap_or_else(|| "All Branches".to_owned());
        let user_label = self.user.clone().unwrap_or_else(|| "All Users".to_owned());
        let paths = match self.path.as_ref() {
            Some(path) => {
                let root = self
                    .git
                    .read(cx)
                    .repo()
                    .map(|repo| repo.root().to_path_buf())
                    .unwrap_or_default();
                relative_of(&root, path)
            }
            None => self
                .root
                .as_ref()
                .and_then(|prefix| {
                    self.roots
                        .iter()
                        .find(|(root, _)| root == prefix)
                        .map(|(_, label)| label.clone())
                })
                .unwrap_or_else(|| "All Paths".to_owned()),
        };
        div()
            .id("log-filter-bar")
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(SPACE_XS))
            .px(px(SPACE_M))
            .py(px(SPACE_XS))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(SPACE_XS))
                    .child(
                        div()
                            .id("log-filter")
                            .debug_selector(|| "log-filter".to_owned())
                            .flex()
                            .items_center()
                            .flex_1()
                            .min_w_0()
                            .h(px(CONTROL_HEIGHT))
                            .px(px(SPACE_M))
                            .rounded(px(CONTROL_RADIUS))
                            .border_1()
                            .border_color(theme.popup_border)
                            .text_color(if filter_empty {
                                theme.text_secondary
                            } else {
                                theme.text
                            })
                            .children(text_field_children_named(
                                "git-log-filter",
                                &self.filter,
                                theme.text,
                                theme.text,
                                theme.selection,
                                theme.caret,
                                16.,
                                !self.branch_search,
                            ))
                            .when(filter_empty, |this| this.child("Text or hash"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.branch_search = false;
                                this.path_search = false;
                                cx.notify();
                            })),
                    )
                    .child(
                        icon_button("log-filter-regex", "general/regex", "Regex", None, theme)
                            .debug_selector(|| "log-filter-regex".to_owned())
                            .when(self.regex, |this| this.bg(theme.selection_inactive))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.regex = !this.regex;
                                this.refilter(cx);
                            })),
                    )
                    .child(
                        icon_button(
                            "log-filter-case",
                            "general/caseSensitive",
                            "Match Case",
                            None,
                            theme,
                        )
                        .debug_selector(|| "log-filter-case".to_owned())
                        .when(self.match_case, |this| this.bg(theme.selection_inactive))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.match_case = !this.match_case;
                            this.refilter(cx);
                        })),
                    )
                    .child(
                        icon_button("log-refresh", "general/refresh", "Refresh", None, theme)
                            .debug_selector(|| "log-refresh".to_owned())
                            .on_click(cx.listener(|this, _, _, cx| this.reload(cx))),
                    )
                    .child(
                        icon_button(
                            "log-cherry-pick",
                            "general/cherryPick",
                            "Cherry-pick",
                            None,
                            theme,
                        )
                        .debug_selector(|| "log-cherry-pick".to_owned())
                        .when(!can_cherry, |this| this.opacity(0.4))
                        .when(can_cherry, |this| {
                            this.on_click(cx.listener(|this, _, _, cx| {
                                let commits = this.picked_ids();
                                if !commits.is_empty() {
                                    this.git
                                        .update(cx, |git, cx| git.cherry_pick_many(commits, cx));
                                }
                            }))
                        }),
                    )
                    .child(
                        icon_button(
                            "log-view-options",
                            "general/settings",
                            "View Options",
                            None,
                            theme,
                        )
                        .debug_selector(|| "log-view-options".to_owned())
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.view_menu(window.mouse_position(), cx)
                        })),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(SPACE_XS))
                    .child(dropdown("log-filter-branch", branch_label, theme).on_click(
                        cx.listener(|this, _, window, cx| {
                            this.branch_menu(window.mouse_position(), cx)
                        }),
                    ))
                    .child(
                        dropdown("log-filter-user", user_label, theme).on_click(cx.listener(
                            |this, _, window, cx| this.user_menu(window.mouse_position(), cx),
                        )),
                    )
                    .child(
                        dropdown("log-filter-date", self.date.label().to_owned(), theme).on_click(
                            cx.listener(|this, _, window, cx| {
                                this.date_menu(window.mouse_position(), cx)
                            }),
                        ),
                    )
                    .child(dropdown("log-roots", paths, theme).on_click(cx.listener(
                        |this, _, window, cx| this.root_menu(window.mouse_position(), cx),
                    )))
                    .child(
                        div()
                            .id("log-filter-path")
                            .debug_selector(|| "log-filter-path".to_owned())
                            .flex()
                            .items_center()
                            .flex_1()
                            .min_w_0()
                            .h(px(CONTROL_HEIGHT))
                            .px(px(SPACE_M))
                            .rounded(px(CONTROL_RADIUS))
                            .border_1()
                            .border_color(theme.popup_border)
                            .text_color(if path_empty {
                                theme.text_secondary
                            } else {
                                theme.text
                            })
                            .children(text_field_children_named(
                                "git-log-path-filter",
                                &self.path_query,
                                theme.text,
                                theme.text,
                                theme.selection,
                                theme.caret,
                                16.,
                                self.path_search,
                            ))
                            .when(path_empty, |this| this.child("Path or folder"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.branch_search = false;
                                this.path_search = true;
                                cx.notify();
                            })),
                    ),
            )
            .into_any_element()
    }

    /// The grip between two panes.
    fn splitter(&self, kind: SplitKind, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let id = match kind {
            SplitKind::Left => "log-split-left",
            SplitKind::Right => "log-split-right",
            SplitKind::Files => "log-split-files",
        };
        let value = match kind {
            SplitKind::Left => self.left_width,
            SplitKind::Right => self.right_width,
            SplitKind::Files => self.files_height,
        };
        let line = |theme: &Theme| div().bg(theme.border);
        let mut grip = div()
            .id(id)
            .debug_selector(move || id.to_owned())
            .flex()
            .justify_center()
            .flex_none()
            .occlude()
            .hover({
                let hover = theme.hover;
                move |style| style.bg(hover)
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    let start = match kind {
                        SplitKind::Files => f32::from(event.position.y),
                        _ => f32::from(event.position.x),
                    };
                    this.split = Some(SplitDrag { kind, start, value });
                    cx.notify();
                }),
            );
        grip = match kind {
            SplitKind::Files => grip
                .w_full()
                .h(px(SPLIT_WIDTH))
                .cursor_row_resize()
                .child(line(theme).w_full().h(px(1.))),
            _ => grip
                .w(px(SPLIT_WIDTH))
                .h_full()
                .cursor_col_resize()
                .child(line(theme).w(px(1.)).h_full()),
        };
        grip.into_any_element()
    }

    /// The pointer of a drag of a divider.
    fn drag_split(&mut self, event: &MouseMoveEvent, window: &Window, cx: &mut Context<Self>) {
        let Some(drag) = &self.split else {
            return;
        };
        let kind = drag.kind;
        let delta = match kind {
            SplitKind::Files => f32::from(event.position.y) - drag.start,
            _ => f32::from(event.position.x) - drag.start,
        };
        match kind {
            SplitKind::Left => {
                let most = (f32::from(window.viewport_size().width) - CENTER_MIN - RIGHT_MIN)
                    .clamp(LEFT_MIN, LEFT_MAX);
                self.left_width = (drag.value + delta).clamp(LEFT_MIN, most);
            }
            SplitKind::Right => {
                let most = (f32::from(window.viewport_size().width) - CENTER_MIN - LEFT_MIN)
                    .clamp(RIGHT_MIN, RIGHT_MAX);
                self.right_width = (drag.value - delta).clamp(RIGHT_MIN, most);
            }
            SplitKind::Files => {
                let most = (f32::from(window.viewport_size().height) - CENTER_MIN)
                    .clamp(FILES_MIN, FILES_MAX);
                self.files_height = (drag.value + delta).clamp(FILES_MIN, most);
            }
        }
        cx.notify();
    }
}

/// The roots of the project, or the project alone when git cannot list them.
fn roots_of(repo: &Repository) -> Vec<(String, Repository)> {
    repo.roots()
        .unwrap_or_else(|_| vec![(String::new(), repo.fork())])
}

/// The root a file belongs to and the repository of that root.
fn repository_for_path(repo: &Repository, path: &Path) -> (String, Repository) {
    let mut chosen = (String::new(), repo.fork());
    for (prefix, repository) in roots_of(repo) {
        if path.starts_with(repository.root()) {
            chosen = (prefix, repository);
        }
    }
    chosen
}

/// The name of a root: the folder of the project, or the path of the submodule.
fn root_label(root: &Path, prefix: &str) -> String {
    if !prefix.is_empty() {
        return prefix.to_owned();
    }
    root.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Project".to_owned())
}

/// `HH:MM` in UTC.
pub(crate) fn time_of(seconds: i64) -> String {
    let seconds = seconds.rem_euclid(86_400);
    format!("{:02}:{:02}", seconds / 3600, (seconds % 3600) / 60)
}

/// `YYYY-MM-DD` in UTC.
pub(crate) fn date_of(seconds: i64) -> String {
    let days = seconds.div_euclid(86_400);
    // The civil date from days since 1970-01-01, as the calendar repeats every 400 years.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

/// The width of the graph column: a lane for each line of the widest shown row, up to
/// `MAX_LANES`. A wider graph is squeezed into the same column, so every dot stays in
/// sight and the lanes only get thinner.
fn graph_width(lanes: usize) -> f32 {
    lanes.clamp(1, MAX_LANES) as f32 * LANE_WIDTH
}

/// The colours of the lanes. A line takes the colour of its column and keeps it while
/// the column lives.
fn graph_colors(theme: &Theme) -> [Hsla; graph::COLORS] {
    [
        theme.link,
        theme.function,
        theme.field,
        theme.semantic_field,
        theme.annotation,
        theme.number,
        theme.keyword,
        theme.string,
    ]
}

/// The dot of one commit and the lines down to the next row, in a canvas of its own so
/// that a row paints a few quads and a path instead of a tree of elements.
fn graph_cell(
    index: usize,
    row: graph::Row,
    lanes: usize,
    colors: [Hsla; graph::COLORS],
) -> impl IntoElement {
    let width = graph_width(lanes);
    let step = width / lanes.max(1) as f32;
    div()
        .id(("log-graph", index))
        .debug_selector(move || format!("log-graph-dot-{index}"))
        .flex_none()
        .w(px(width))
        .h(px(ROW_HEIGHT))
        .overflow_hidden()
        .child(
            canvas(
                move |_, _, _| {},
                move |bounds, _, window, _| paint_graph(bounds, &row, &colors, step, window),
            )
            .size_full(),
        )
}

/// Paints the lines of `row` and its dot in `bounds`, the lanes `step` pixels apart. A
/// line of the dot leaves the dot, a line of a lane that only passes through runs through
/// the whole row, and a diagonal goes from one column to the other.
fn paint_graph(
    bounds: Bounds<Pixels>,
    row: &graph::Row,
    colors: &[Hsla; graph::COLORS],
    step: f32,
    window: &mut Window,
) {
    let lane = |column: usize| bounds.left() + px(step * (column as f32 + 0.5));
    let top = bounds.top();
    let bottom = bounds.bottom();
    let middle = top + bounds.size.height / 2.;
    // The line from the row above reaches the dot. A straight line through the dot covers
    // this half itself; a line that ends at its root or joins another lane does not, and
    // would stop half a row above its dot without this stub.
    let covered = row
        .lines
        .iter()
        .any(|line| line.from == row.column && line.to == row.column);
    if !row.starts && !covered {
        let mut path = PathBuilder::stroke(px(LINE_WIDTH));
        path.move_to(point(lane(row.column), top));
        path.line_to(point(lane(row.column), middle));
        if let Ok(path) = path.build() {
            window.paint_path(path, colors[row.color % graph::COLORS]);
        }
    }
    for line in &row.lines {
        let color = colors[line.from % graph::COLORS];
        // A straight line of the dot joins the line above it, except on the row where the
        // line starts: there the dot is the top of the line.
        let from_dot = line.from == row.column;
        let start = if from_dot && (row.starts || line.from != line.to) {
            middle
        } else {
            top
        };
        let mut path = PathBuilder::stroke(px(LINE_WIDTH));
        path.move_to(point(lane(line.from), start));
        path.line_to(point(lane(line.to), bottom));
        if let Ok(path) = path.build() {
            window.paint_path(path, color);
        }
    }
    let radius = px(DOT_SIZE / 2.);
    let dot = Bounds::new(
        point(lane(row.column) - radius, middle - radius),
        size(px(DOT_SIZE), px(DOT_SIZE)),
    );
    window.paint_quad(fill(dot, colors[row.color % graph::COLORS]).corner_radii(radius));
}

/// A branch or a tag after the subject of a commit. The branch HEAD points at takes the
/// accent colour, a remote branch and a tag stay quieter than a local one. A name too long
/// for the pill is cut, and the tooltip of the pill gives all of it.
fn label_pill(index: usize, position: usize, label: &RefLabel, theme: &Theme) -> impl IntoElement {
    let (background, color) = match (label.kind, label.current) {
        (_, true) => (theme.accent, theme.over_accent),
        (RefKind::Local, false) => (theme.selection, theme.text),
        (RefKind::Remote, false) => (theme.selection_muted, theme.text_secondary),
        (RefKind::Tag, false) => (theme.fold_bg, theme.fold_text),
    };
    let name = div().min_w_0().truncate().child(label.name.clone());
    let name = if label.current {
        name.debug_selector(move || format!("log-ref-head-{index}"))
    } else {
        name
    };
    let full = label.name.clone();
    div()
        .id(("log-ref", position))
        .debug_selector(move || format!("log-ref-{index}-{position}"))
        .flex()
        .items_center()
        .min_w_0()
        .h(px(LABEL_HEIGHT))
        .max_w(px(LABEL_WIDTH))
        .px(px(SPACE_S))
        .rounded(px(CONTROL_RADIUS))
        .bg(background)
        .text_color(color)
        .text_size(px(TEXT_SMALL))
        .tooltip(move |_, cx| tooltip(full.clone(), None, cx))
        .child(name)
}

/// The labels of a commit after its subject, in a box a third of the text column wide at
/// most. The labels that do not fit are replaced by the first one by priority and a `+N`
/// badge for the rest.
fn labels_cell(index: usize, labels: &[RefLabel], theme: &Theme) -> impl IntoElement {
    let (shown, hidden) = drawn_labels(labels);
    div()
        .flex()
        .items_center()
        .gap(px(SPACE_S))
        .flex_none()
        .max_w(relative(1. / 3.))
        .overflow_hidden()
        .debug_selector(move || format!("log-labels-{index}"))
        .children(
            shown
                .into_iter()
                .map(|position| label_pill(index, position, &labels[position], theme)),
        )
        .when(hidden > 0, |this| {
            let names = labels
                .iter()
                .map(|label| label.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            this.child(more_badge(index, hidden, names, theme))
        })
}

/// The `+N` badge of the labels that do not fit. Its tooltip names every label of the commit.
fn more_badge(index: usize, hidden: usize, names: String, theme: &Theme) -> impl IntoElement {
    div()
        .id(("log-ref-more", index))
        .debug_selector(move || format!("log-ref-more-{index}"))
        .flex()
        .items_center()
        .flex_none()
        .h(px(LABEL_HEIGHT))
        .px(px(SPACE_S))
        .rounded(px(CONTROL_RADIUS))
        .bg(theme.selection_muted)
        .text_color(theme.text_secondary)
        .text_size(px(TEXT_SMALL))
        .whitespace_nowrap()
        .tooltip(move |_, cx| tooltip(names.clone(), None, cx))
        .child(format!("+{hidden}"))
}

/// The labels a row draws, as indices into `labels` in the order they show, and the count
/// that the `+N` badge stands for. Every label is drawn while the estimated widths fit
/// `LABELS_BUDGET`; otherwise only the first one by priority is drawn.
fn drawn_labels(labels: &[RefLabel]) -> (Vec<usize>, usize) {
    let mut order: Vec<usize> = (0..labels.len()).collect();
    order.sort_by_key(|index| label_priority(&labels[*index]));
    let gaps = labels.len().saturating_sub(1) as f32 * SPACE_S;
    let width = labels.iter().map(label_estimate).sum::<f32>() + gaps;
    if order.len() <= 1 || width <= LABELS_BUDGET {
        return (order, 0);
    }
    (order[..1].to_vec(), order.len() - 1)
}

/// The order of the labels of a row: HEAD and the current branch first, then the local
/// branches, then the remote branches and the tags.
fn label_priority(label: &RefLabel) -> u8 {
    if label.current {
        0
    } else if label.kind == RefKind::Local {
        1
    } else {
        2
    }
}

/// The width a label takes, estimated from the length of its name. The row is laid out
/// only after its labels are chosen, so the estimate decides how many of them are drawn.
fn label_estimate(label: &RefLabel) -> f32 {
    let text = label.name.chars().count() as f32 * LABEL_CHAR_WIDTH;
    (text + 2. * SPACE_S).min(LABEL_WIDTH)
}

/// A button of the narrow column of the Log: an icon with the name of its action as the
/// tooltip, as the buttons of the tool windows are built.
fn action_button(
    selector: &'static str,
    icon_name: &'static str,
    label: &'static str,
    theme: &Theme,
    cx: &mut Context<GitLogPanel>,
    run: impl Fn(&mut GitLogPanel, &mut Window, &mut Context<GitLogPanel>) + 'static,
) -> AnyElement {
    icon_button(selector, icon_name, label, None, theme)
        .debug_selector(move || selector.to_owned())
        .on_click(cx.listener(move |this, _, window, cx| run(this, window, cx)))
        .into_any_element()
}

/// Reset Current Branch to Here: the four modes of git, each with what it keeps, as the
/// dialog of IDEA explains them. Hard asks again when the local changes would be lost.
pub(crate) struct ResetView {
    git: Entity<GitState>,
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    commit: String,
}

impl ResetView {
    pub(crate) fn open(
        git: Entity<GitState>,
        commit: String,
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        let weak = cx.weak_entity();
        let view = cx.new(|cx| Self {
            git,
            workspace: weak,
            focus: cx.focus_handle(),
            commit,
        });
        let focus = view.read(cx).focus.clone();
        workspace.show_overlay(view.into(), focus, window, cx);
    }

    fn choose(&mut self, mode: ResetMode, window: &mut Window, cx: &mut Context<Self>) {
        let commit = self.commit.clone();
        if mode == ResetMode::Hard && self.has_changes(cx) {
            let git = self.git.clone();
            let question = "Hard reset throws away all uncommitted changes. Continue?";
            self.workspace
                .update(cx, |workspace, cx| {
                    Prompt::ask(
                        "Reset Current Branch",
                        question,
                        move |cx| {
                            git.update(cx, |git, cx| git.reset_to(commit.clone(), mode, cx));
                        },
                        workspace,
                        window,
                        cx,
                    );
                })
                .ok();
            return;
        }
        self.git
            .update(cx, |git, cx| git.reset_to(commit, mode, cx));
        self.close(window, cx);
    }

    /// Whether the index, the working tree or an open editor holds changes that a hard
    /// reset would throw away before the editors are written.
    fn has_changes(&self, cx: &App) -> bool {
        if self.git.read(cx).has_local_changes() {
            return true;
        }
        self.workspace
            .read_with(cx, |workspace, cx| {
                workspace.open_editors().into_iter().any(|(_, view)| {
                    view.downcast::<editor_view::EditorView>()
                        .is_ok_and(|view| view.read(cx).is_dirty())
                })
            })
            .unwrap_or(false)
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace
            .update(cx, |workspace, cx| workspace.dismiss(window, cx))
            .ok();
    }

    fn row(
        &self,
        mode: ResetMode,
        selector: &'static str,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let hover = theme.hover;
        div()
            .id(selector)
            .debug_selector(move || selector.to_owned())
            .flex()
            .flex_col()
            .gap(px(SPACE_XS))
            .flex_none()
            .px(px(SPACE_M))
            .py(px(SPACE_S))
            .rounded(px(ROW_RADIUS))
            .cursor_pointer()
            .hover(move |style| style.bg(hover))
            .child(div().font_weight(FontWeight::MEDIUM).child(mode.label()))
            .child(div().text_color(theme.text_secondary).child(mode.hint()))
            .on_click(cx.listener(move |this, _, window, cx| this.choose(mode, window, cx)))
    }
}

impl Render for ResetView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let viewport = window.viewport_size();
        let width = px(560.).min(viewport.width - px(40.));
        let left = (viewport.width - width) / 2.;
        let top = viewport.height * 0.2;
        let short = short_commit(&self.commit);
        let rows: Vec<Stateful<Div>> = [
            (ResetMode::Soft, "reset-soft"),
            (ResetMode::Mixed, "reset-mixed"),
            (ResetMode::Hard, "reset-hard"),
            (ResetMode::Keep, "reset-keep"),
        ]
        .into_iter()
        .map(|(mode, selector)| self.row(mode, selector, &theme, cx))
        .collect();
        let title = format!("Reset Current Branch to {short}");
        let shell = surface(&theme)
            .id("reset-view")
            .debug_selector(|| "reset-view".to_owned())
            .key_context("ResetCurrentBranch")
            .track_focus(&self.focus)
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    this.close(window, cx);
                    cx.stop_propagation();
                }
            }));
        let contents = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .p(px(POPUP_PADDING))
            .gap(px(SPACE_S))
            .child(
                div()
                    .text_color(theme.text_secondary)
                    .child("The current branch and HEAD move to the chosen commit."),
            )
            .children(rows);
        let close = cx.listener(|this, _: &ClickEvent, window, cx| this.close(window, cx));
        dialog_window(
            DialogSpec {
                key: "reset-branch",
                title: title.into(),
                extra: None,
                place: DialogPlace {
                    left: f32::from(left),
                    top: f32::from(top),
                    width: f32::from(width),
                    height: None,
                },
                min: (360., 200.),
            },
            shell,
            window,
            cx,
            close,
            contents,
        )
    }
}

/// Interactively Rebase from Here: the commits from the chosen one to HEAD, oldest first,
/// with the action of each of them. The plan starts the rebase; a Reword opens a field
/// with the message of the commit in the dialog itself.
pub struct RebaseView {
    git: Entity<GitState>,
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    plan: RebasePlan,
    /// The plan as the dialog opened it, for Reset.
    original: RebasePlan,
    selected: usize,
    /// The row whose list of actions is open.
    open_action: Option<usize>,
    /// The message field takes the keys of the user.
    editing: bool,
    message: TextField,
    scroll: ScrollHandle,
}

impl RebaseView {
    fn open(
        git: Entity<GitState>,
        plan: RebasePlan,
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Entity<Self> {
        let weak = cx.weak_entity();
        let message = plan
            .steps
            .first()
            .map(|step| TextField::from_text(step.message.clone()))
            .unwrap_or_default();
        let original = plan.clone();
        let view = cx.new(|cx| Self {
            git,
            workspace: weak,
            focus: cx.focus_handle(),
            plan,
            original,
            selected: 0,
            open_action: None,
            editing: false,
            message,
            scroll: ScrollHandle::new(),
        });
        let focus = view.read(cx).focus.clone();
        workspace.show_overlay(view.clone().into(), focus, window, cx);
        view
    }

    /// Writes the text of the field back into the step it belongs to.
    fn sync_message(&mut self) {
        if let Some(step) = self.plan.steps.get_mut(self.selected)
            && step.action == RebaseAction::Reword
        {
            step.message = self.message.text.clone();
        }
    }

    fn select(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.plan.steps.len() {
            return;
        }
        self.sync_message();
        self.selected = index;
        self.editing = false;
        self.open_action = None;
        let message = self.plan.steps[index].message.clone();
        self.message = TextField::from_text(message);
        cx.notify();
    }

    fn set_action(&mut self, index: usize, action: RebaseAction, cx: &mut Context<Self>) {
        if index >= self.plan.steps.len() {
            return;
        }
        // The first commit of the list has nothing before it to squash or fix up.
        if index == 0 && matches!(action, RebaseAction::Squash | RebaseAction::Fixup) {
            return;
        }
        self.sync_message();
        self.plan.steps[index].action = action;
        self.selected = index;
        self.open_action = None;
        if action == RebaseAction::Reword {
            let message = self.plan.steps[index].message.clone();
            self.message = TextField::from_text(message);
            self.editing = true;
        } else {
            self.editing = false;
        }
        cx.notify();
    }

    /// Alt+Up and Alt+Down: the selected commit takes the place of its neighbour.
    fn move_selected(&mut self, delta: isize, cx: &mut Context<Self>) {
        let target = self.selected as isize + delta;
        if target < 0 || target as usize >= self.plan.steps.len() {
            return;
        }
        let target = target as usize;
        self.sync_message();
        self.plan.steps.swap(self.selected, target);
        self.selected = target;
        let first = &mut self.plan.steps[0];
        if matches!(first.action, RebaseAction::Squash | RebaseAction::Fixup) {
            first.action = RebaseAction::Pick;
        }
        cx.notify();
    }

    fn reset_plan(&mut self, cx: &mut Context<Self>) {
        self.plan = self.original.clone();
        self.selected = 0;
        self.open_action = None;
        self.editing = false;
        let message = self.plan.steps[0].message.clone();
        self.message = TextField::from_text(message);
        cx.notify();
    }

    fn start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_message();
        let plan = self.plan.clone();
        self.workspace
            .update(cx, |workspace, cx| workspace.dismiss(window, cx))
            .ok();
        self.git.update(cx, |git, cx| git.start_rebase(plan, cx));
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace
            .update(cx, |workspace, cx| workspace.dismiss(window, cx))
            .ok();
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let stroke = &event.keystroke;
        let key = stroke.key.as_str();
        if self.editing {
            if key == "escape" {
                self.sync_message();
                self.editing = false;
                cx.stop_propagation();
                cx.notify();
            } else if self.message.key_with_clipboard(event, true, cx) {
                self.sync_message();
                cx.stop_propagation();
                cx.notify();
            } else {
                cx.stop_propagation();
            }
            return;
        }
        if key == "escape" {
            self.close(window, cx);
            cx.stop_propagation();
            return;
        }
        // Enter presses Start Rebasing, the primary button.
        if key == "enter" && !stroke.modifiers.alt {
            self.start(window, cx);
            cx.stop_propagation();
            return;
        }
        if stroke.modifiers.alt && key == "up" {
            self.move_selected(-1, cx);
            cx.stop_propagation();
            return;
        }
        if stroke.modifiers.alt && key == "down" {
            self.move_selected(1, cx);
            cx.stop_propagation();
            return;
        }
        match key {
            "up" => {
                self.select(self.selected.saturating_sub(1), cx);
                cx.stop_propagation();
                return;
            }
            "down" => {
                let last = self.plan.steps.len().saturating_sub(1);
                self.select((self.selected + 1).min(last), cx);
                cx.stop_propagation();
                return;
            }
            _ => {}
        }
        let plain =
            !stroke.modifiers.control && !stroke.modifiers.alt && !stroke.modifiers.platform;
        if plain && key.len() == 1 {
            let pressed = key.chars().next().unwrap_or_default();
            if let Some(action) = RebaseAction::from_key(pressed) {
                let selected = self.selected;
                self.set_action(selected, action, cx);
                cx.stop_propagation();
            }
        }
    }

    fn row(&self, index: usize, theme: &Theme, cx: &mut Context<Self>) -> Stateful<Div> {
        let step = &self.plan.steps[index];
        let selected = self.selected == index;
        let short = short_commit(&step.id);
        let action = step.action;
        let subject = step.subject.clone();
        let author = step.author.clone();
        let hover = theme.selection_hover;
        div()
            .id(("rebase-row", index))
            .debug_selector(move || format!("rebase-row-{index}"))
            .flex()
            .items_center()
            .gap(px(SPACE_S))
            .flex_none()
            .h(px(ROW_HEIGHT))
            .px(px(ROW_PADDING_X))
            .rounded(px(ROW_RADIUS))
            .when(selected, |this| this.bg(theme.selection_inactive))
            .when(!selected, |this| this.hover(move |style| style.bg(hover)))
            .on_click(cx.listener(move |this, _, _, cx| {
                let open = this.open_action != Some(index);
                this.select(index, cx);
                this.open_action = open.then_some(index);
                cx.notify();
            }))
            .child(
                div()
                    .id(("rebase-action", index))
                    .debug_selector(move || format!("rebase-action-{index}"))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(SPACE_XS))
                    .flex_none()
                    .w(px(REBASE_ACTION_WIDTH))
                    .h(px(CONTROL_HEIGHT))
                    .px(px(SPACE_S))
                    .rounded(px(CONTROL_RADIUS))
                    .border_1()
                    .border_color(theme.popup_border)
                    .hover({
                        let hover = theme.hover;
                        move |style| style.bg(hover)
                    })
                    .child(action.label())
                    .child(icon_sized(
                        if self.open_action == Some(index) {
                            "general/chevronUp"
                        } else {
                            "general/chevronDown"
                        },
                        TEXT_SMALL,
                        theme,
                    ))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let open = this.open_action != Some(index);
                        this.select(index, cx);
                        this.open_action = open.then_some(index);
                        cx.stop_propagation();
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .w(px(64.))
                    .flex_none()
                    .font_family(MONO_FONT)
                    .text_color(theme.link)
                    .child(short),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(subject),
            )
            .child(
                div()
                    .w(px(120.))
                    .flex_none()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_color(theme.text_secondary)
                    .child(author),
            )
    }

    /// The list of actions of one row, open under the row as a dropdown.
    fn action_menu(&self, index: usize, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let first = index == 0;
        let entries = RebaseAction::ALL.into_iter().map(move |action| {
            let disabled = first && matches!(action, RebaseAction::Squash | RebaseAction::Fixup);
            let hover = theme.hover;
            let name: SharedString = format!("rebase-option-{index}-{}", action.name()).into();
            div()
                .id(name.clone())
                .debug_selector(move || name.to_string())
                .flex()
                .items_center()
                .justify_between()
                .gap(px(SPACE_M))
                .flex_none()
                .h(px(ROW_HEIGHT))
                .px(px(SPACE_S))
                .rounded(px(ROW_RADIUS))
                .text_color(if disabled {
                    theme.text_muted
                } else {
                    theme.text
                })
                .when(!disabled, |this| {
                    this.cursor_pointer()
                        .hover(move |style| style.bg(hover))
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.set_action(index, action, cx)),
                        )
                })
                .child(action.label())
                .child(action.key().to_ascii_uppercase().to_string())
        });
        div()
            .id(("rebase-menu", index))
            .debug_selector(move || format!("rebase-menu-{index}"))
            .flex()
            .flex_col()
            .flex_none()
            .mx(px(SPACE_M))
            .my(px(SPACE_XS))
            .p(px(SPACE_XS))
            .rounded(px(CONTROL_RADIUS))
            .border_1()
            .border_color(theme.border)
            .bg(theme.popup_bg)
            .children(entries)
    }

    fn message_editor(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let step = &self.plan.steps[self.selected];
        let show_caret = self.editing;
        div()
            .id("rebase-message")
            .debug_selector(|| "rebase-message".to_owned())
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(SPACE_XS))
            .child(
                div()
                    .text_color(theme.text_secondary)
                    .child(format!("Message of {}", short_commit(&step.id))),
            )
            .child(
                div()
                    .id("rebase-message-field")
                    .debug_selector(|| "rebase-message-field".to_owned())
                    .h(px(REBASE_MESSAGE_HEIGHT))
                    .p(px(SPACE_M))
                    .rounded(px(CONTROL_RADIUS))
                    .border_1()
                    .border_color(theme.popup_border)
                    .overflow_hidden()
                    .flex()
                    .flex_col()
                    .text_color(theme.text)
                    .children(text_field_lines_named(
                        "rebase-message-field",
                        &self.message,
                        theme.text,
                        theme.text,
                        theme.selection,
                        theme.caret,
                        18.,
                        show_caret,
                    ))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.editing = true;
                        cx.notify();
                    })),
            )
    }
}

impl Render for RebaseView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let viewport = window.viewport_size();
        let width = px(REBASE_WIDTH).min(viewport.width - px(40.));
        let height = viewport.height * 0.7;
        let left = (viewport.width - width) / 2.;
        let top = viewport.height * 0.14;
        let mut rows: Vec<AnyElement> = Vec::new();
        for index in 0..self.plan.steps.len() {
            rows.push(self.row(index, &theme, cx).into_any_element());
            if self.open_action == Some(index) {
                rows.push(self.action_menu(index, &theme, cx).into_any_element());
            }
        }
        let editing_message = self
            .plan
            .steps
            .get(self.selected)
            .is_some_and(|step| step.action == RebaseAction::Reword);
        let count = self.plan.steps.len();
        let oldest = self
            .plan
            .steps
            .first()
            .map(|step| short_commit(&step.id))
            .unwrap_or_default();
        let shell = surface(&theme)
            .id("rebase-view")
            .debug_selector(|| "rebase-view".to_owned())
            .key_context("Rebase")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)));
        let contents = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .p(px(POPUP_PADDING))
            .gap(px(SPACE_M))
            .child(
                div().flex().flex_col().flex_none().gap(px(SPACE_XS)).child(
                    div()
                        .text_color(theme.text_secondary)
                        .child(format!("{count} commit(s) from {oldest} to HEAD")),
                ),
            )
            .child(
                scrollable(
                    "rebase-commits",
                    &self.scroll,
                    div()
                        .id("rebase-commits")
                        .flex()
                        .flex_col()
                        .px(px(SPACE_M))
                        .overflow_y_scroll()
                        .track_scroll(&self.scroll)
                        .children(rows),
                    &theme,
                    window,
                    cx,
                )
                .flex_1()
                .min_h_0(),
            )
            .children(editing_message.then(|| self.message_editor(&theme, cx)))
            .child(
                DialogButtons::new()
                    .extra(
                        dialog_button(
                            "rebase-reset",
                            "rebase-reset".to_owned(),
                            "Reset",
                            false,
                            true,
                            &theme,
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.reset_plan(cx))),
                    )
                    .primary(
                        dialog_button(
                            "rebase-start",
                            "rebase-start".to_owned(),
                            "Start Rebasing",
                            true,
                            true,
                            &theme,
                        )
                        .on_click(cx.listener(|this, _, window, cx| this.start(window, cx))),
                    )
                    .other(
                        dialog_button(
                            "rebase-cancel",
                            "rebase-cancel".to_owned(),
                            "Cancel",
                            false,
                            true,
                            &theme,
                        )
                        .on_click(cx.listener(|this, _, window, cx| this.close(window, cx))),
                    )
                    .into_row(),
            );
        let close = cx.listener(|this, _: &ClickEvent, window, cx| this.close(window, cx));
        dialog_window(
            DialogSpec {
                key: "rebase",
                title: "Interactive Rebase".into(),
                extra: None,
                place: DialogPlace {
                    left: f32::from(left),
                    top: f32::from(top),
                    width: f32::from(width),
                    height: Some(f32::from(height)),
                },
                min: (480., 320.),
            },
            shell,
            window,
            cx,
            close,
            contents,
        )
    }
}

#[cfg(test)]
impl RebaseView {
    /// The subjects of the plan, oldest first.
    pub fn subjects(&self) -> Vec<String> {
        self.plan
            .steps
            .iter()
            .map(|step| step.subject.clone())
            .collect()
    }

    /// The action of every row, oldest first.
    pub fn actions(&self) -> Vec<RebaseAction> {
        self.plan.steps.iter().map(|step| step.action).collect()
    }
}

/// Show History of one file: opens the tool and loads the log of `path`.
pub fn show_file(
    path: PathBuf,
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(panel) = cx.try_global::<ActiveGitLog>().map(|panel| panel.0.clone()) else {
        return;
    };
    workspace.show_tool(LOG_TOOL, window, cx);
    panel.update(cx, |panel, cx| panel.show_path(path, cx));
}

impl Render for GitLogPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(test)]
        workspace::note_view_render(cx, "git-log");
        let theme = *cx.global::<Theme>();
        if self.tab == GitTab::Log
            && self.more
            && !self.loading
            && self.scroll_commits.max_offset().y > px(0.)
            && self.scroll_commits.offset().y + px(ROW_HEIGHT * 4.)
                >= self.scroll_commits.max_offset().y
        {
            let panel = cx.weak_entity();
            cx.defer(move |cx| {
                panel.update(cx, |panel, cx| panel.load_more(cx)).ok();
            });
        }
        let title = "Git";
        let busy = self.loading || self.files_loading;
        let tabs = self.tab_strip(&theme, cx);
        let workspace = self.workspace.clone();
        let hide = move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
            workspace
                .update(cx, |workspace, cx| {
                    workspace.toggle_tool(LOG_TOOL, window, cx)
                })
                .ok();
        };
        let options =
            crate::tool_menu::options_menu(self.workspace.clone(), LOG_TOOL, |_| Vec::new());
        let head = div()
            .flex()
            .items_center()
            .gap(px(SPACE_S))
            .min_w_0()
            .child(
                div()
                    .font_weight(FontWeight::MEDIUM)
                    .debug_selector(|| "log-title".to_owned())
                    .child(title),
            )
            .child(tabs);
        let actions = div()
            .flex()
            .items_center()
            .gap(px(SPACE_S))
            .when(busy, |this| {
                this.child(div().text_color(theme.text_secondary).child("Loading..."))
            });
        let header = tool_header(head, actions, "left-panel", "log", options, hide, &theme)
            .debug_selector(|| "log-header".to_owned());
        let body: AnyElement = if self.tab == GitTab::Console {
            self.console_panel(&theme, window, cx)
        } else if self.tab == GitTab::LocalChanges {
            self.commit.clone().map_or_else(
                || {
                    div()
                        .p(px(SPACE_M))
                        .text_color(theme.text_secondary)
                        .child("No commit window")
                        .into_any_element()
                },
                |commit| {
                    div()
                        .flex()
                        .flex_1()
                        .min_h_0()
                        .child(commit)
                        .into_any_element()
                },
            )
        } else {
            let left = self
                .left_visible
                .then(|| self.branches_panel(&theme, window, cx));
            let show_left = (!self.left_visible).then(|| {
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .flex_none()
                    .w(px(CONTROL_HEIGHT))
                    .h_full()
                    .py(px(SPACE_XS))
                    .border_r_1()
                    .border_color(theme.toolbar_border)
                    .child(action_button(
                        "log-action-panel",
                        "general/chevronRight",
                        "Show Branches",
                        &theme,
                        cx,
                        |this, _, cx| {
                            this.left_visible = true;
                            cx.notify();
                        },
                    ))
            });
            let files = self.files_panel(&theme, window, cx);
            let details = self.details_panel(&theme, window, cx);
            let filters = self.filter_bar(&theme, cx);
            let commits = self.commit_rows(&theme, cx);
            let empty = commits.is_empty();
            let center = div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .child(filters)
                .child(
                    scrollable(
                        "log-commits",
                        &self.scroll_commits,
                        div()
                            .id("log-commits")
                            .debug_selector(|| "log-commits".to_owned())
                            .flex()
                            .flex_col()
                            .px(px(SPACE_M))
                            .overflow_y_scroll()
                            .track_scroll(&self.scroll_commits)
                            .children(commits)
                            .when(empty, |this| {
                                this.items_center()
                                    .justify_center()
                                    .p_1()
                                    .text_color(theme.text_secondary)
                                    .child("No commits")
                            }),
                        &theme,
                        window,
                        cx,
                    )
                    .flex_1()
                    .min_w_0()
                    .min_h_0(),
                );
            let right = div()
                .flex()
                .flex_col()
                .flex_none()
                .w(px(self.right_width))
                .h_full()
                .min_h_0()
                .child(files)
                .child(self.splitter(SplitKind::Files, &theme, cx))
                .child(details);
            let mut panes = div().flex().flex_row().flex_1().min_h_0();
            if let Some(toggle) = show_left {
                panes = panes.child(toggle);
            }
            if let Some(left) = left {
                panes = panes
                    .child(left)
                    .child(self.splitter(SplitKind::Left, &theme, cx));
            }
            panes
                .child(center)
                .child(self.splitter(SplitKind::Right, &theme, cx))
                .child(right)
                .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, window, cx| {
                    this.drag_split(event, window, cx)
                }))
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        this.split = None;
                        cx.notify();
                    }),
                )
                .on_mouse_up_out(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        this.split = None;
                        cx.notify();
                    }),
                )
                .into_any_element()
        };
        div()
            .id("git-log")
            .group("left-panel")
            .debug_selector(|| "git-log".to_owned())
            .key_context("GitLog ToolWindow")
            .track_focus(&self.focus)
            .flex()
            .flex_col()
            .size_full()
            .bg(theme.island_bg)
            .text_color(theme.text)
            .on_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)))
            .on_action(
                cx.listener(|this, _: &RewordCommit, window, cx| this.ask_reword(window, cx)),
            )
            .on_action(cx.listener(|this, _: &BranchAtCommit, window, cx| {
                this.ask_branch_from_commit(window, cx)
            }))
            .on_action(cx.listener(|this, _: &ShowAllAffected, _, cx| this.show_all_affected(cx)))
            .child(header)
            .child(body)
    }
}

/// The lines of one stream of a git command, the output of stderr in red.
fn output_lines(text: &str, error: bool) -> Vec<crate::ansi::Line> {
    let mut parser = AnsiParser::default();
    let mut lines = Vec::new();
    parser.push(text, error, &mut lines);
    parser.finish(&mut lines);
    lines
}

/// Show History of one commit: opens the log and selects `id` with its files, as a click
/// on a blame label asks.
pub fn show_commit(
    id: &str,
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(panel) = cx.try_global::<ActiveGitLog>().map(|panel| panel.0.clone()) else {
        return;
    };
    workspace.show_tool(LOG_TOOL, window, cx);
    let id = id.to_owned();
    panel.update(cx, |panel, cx| {
        panel.path = None;
        panel.root = None;
        panel.branch_filter = None;
        panel.filter.text.clear();
        panel.clear_log();
        panel.want = Some(id);
        panel.reload(cx);
        cx.notify();
    });
}

#[cfg(test)]
mod tests {
    use super::date_of;

    #[test]
    fn a_commit_date_is_a_utc_day() {
        assert_eq!(date_of(0), "1970-01-01");
        assert_eq!(date_of(1_700_000_000), "2023-11-14");
        assert_eq!(date_of(1_709_164_800), "2024-02-29");
    }

    #[test]
    fn the_time_of_day_does_not_change_the_day() {
        assert_eq!(date_of(1_700_000_000 + 3600), "2023-11-14");
        assert_ne!(date_of(1_700_000_000 + 86_400), "2023-11-14");
    }
}
