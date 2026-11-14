//! The Git Log tool window: the commits of the branch or of one file, the files a commit
//! changed, the diff of one of them and the actions a right click on a commit opens.

use std::path::PathBuf;

use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use vcs::graph;
use vcs::{Commit, RefKind, RefLabel, ResetMode};
use workspace::{LOG_TOOL, Workspace};

use crate::diff_view::DiffView;
use crate::git::{GitState, relative_of, short_commit};
use crate::prompt::{Prompt, TextField};

const LIMIT: usize = 200;
const FILES_WIDTH: f32 = 260.;
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
/// The height of a label of a branch or a tag before the subject of a commit, and the
/// width of the longest of them: a longer name is cut.
const LABEL_HEIGHT: f32 = 16.;
const LABEL_WIDTH: f32 = 180.;

actions!(
    git_log_panel,
    [RewordCommit, BranchAtCommit, ShowAllAffected]
);

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
    /// `None` for the branch, `Some` for Show History of one file.
    path: Option<PathBuf>,
    commits: Vec<Commit>,
    /// The graph rows of `commits`, laid out once when the log is loaded.
    graph: Vec<graph::Row>,
    /// The branches and tags of the repository, in the order `for-each-ref` gives them.
    labels: Vec<RefLabel>,
    /// The lanes the shown rows use, for the width of the graph column.
    lanes: usize,
    /// The indices of `commits` that the filter keeps.
    shown: Vec<usize>,
    files: Vec<String>,
    selected: Option<usize>,
    filter: TextField,
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
    scroll_commits: ScrollHandle,
    scroll_files: ScrollHandle,
    _git: Subscription,
}

pub struct ActiveGitLog(pub Entity<GitLogPanel>);

impl Global for ActiveGitLog {}

impl GitLogPanel {
    pub fn create(
        git: Entity<GitState>,
        workspace: WeakEntity<Workspace>,
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
            });
            Self {
                git,
                workspace,
                focus: cx.focus_handle(),
                path: None,
                commits: Vec::new(),
                graph: Vec::new(),
                labels: Vec::new(),
                lanes: 0,
                shown: Vec::new(),
                files: Vec::new(),
                selected: None,
                filter: TextField::default(),
                loading: false,
                files_loading: false,
                head,
                revision,
                want: None,
                reload_pending: false,
                scroll_commits: ScrollHandle::new(),
                scroll_files: ScrollHandle::new(),
                _git: observe,
            }
        });
        let loaded = panel.clone();
        cx.defer(move |cx| loaded.update(cx, |panel, cx| panel.reload(cx)));
        panel
    }

    /// The whole branch again, and no filter.
    pub fn show_root(&mut self, cx: &mut Context<Self>) {
        self.path = None;
        self.filter.text.clear();
        self.commits.clear();
        self.graph.clear();
        self.labels.clear();
        self.lanes = 0;
        self.shown.clear();
        self.files.clear();
        self.selected = None;
        self.want = None;
        self.reload(cx);
        cx.notify();
    }

    /// The log of one file, newest first.
    pub fn show_path(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.path = Some(path);
        self.filter.text.clear();
        self.commits.clear();
        self.graph.clear();
        self.labels.clear();
        self.lanes = 0;
        self.shown.clear();
        self.files.clear();
        self.selected = None;
        self.want = None;
        self.reload(cx);
        cx.notify();
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
        let path = self.path.clone();
        let job = cx.background_executor().spawn(async move {
            let result = match path {
                Some(path) => {
                    let relative = relative_of(repo.root(), &path);
                    repo.file_log(&relative, LIMIT, 0)
                }
                None => repo.log(LIMIT, 0),
            };
            // The layout of a long log and the labels are the slow part of a load, so
            // they are read here and never while the window draws. A log without labels
            // is still a log.
            result
                .map(|commits| {
                    let graph = graph::layout(&commits);
                    let labels = repo.labels().unwrap_or_default();
                    (commits, graph, labels)
                })
                .map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = job.await;
            this.update(cx, |this, cx| {
                this.loading = false;
                match result {
                    Ok((commits, graph, labels)) => {
                        this.commits = commits;
                        this.graph = graph;
                        this.labels = labels;
                        this.selected = None;
                        this.files.clear();
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

    fn apply_filter(&mut self) {
        let needle = self.filter.text.trim().to_lowercase();
        self.shown = self
            .commits
            .iter()
            .enumerate()
            .filter(|(_, commit)| {
                needle.is_empty()
                    || commit.id.to_lowercase().contains(&needle)
                    || commit.subject.to_lowercase().contains(&needle)
                    || commit.author.to_lowercase().contains(&needle)
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
            self.files.clear();
        }
    }

    fn select(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(commit) = self
            .shown
            .get(index)
            .and_then(|index| self.commits.get(*index))
        else {
            return;
        };
        let id = commit.id.clone();
        self.selected = Some(index);
        self.files.clear();
        self.files_loading = true;
        let Some(repo) = self.git.read(cx).repo() else {
            return;
        };
        let job = cx
            .background_executor()
            .spawn(async move { repo.commit_files(&id).map_err(|error| error.to_string()) });
        cx.spawn(async move |this, cx| {
            let result = job.await;
            this.update(cx, |this, cx| {
                this.files_loading = false;
                match result {
                    Ok(files) => this.files = files,
                    Err(message) => this
                        .git
                        .update(cx, |git, cx| git.report("Git Log", message, cx)),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
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
        let Some(path) = self.files.get(index) else {
            return;
        };
        let Some(root) = self
            .git
            .read(cx)
            .repo()
            .map(|repo| repo.root().to_path_buf())
        else {
            return;
        };
        DiffView::open_commit(
            self.git.clone(),
            self.workspace.clone(),
            root.join(path),
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
        if stroke.modifiers.control && !stroke.modifiers.alt && stroke.key == "c" {
            self.copy_selected(cx);
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
                    self.open_diff(0, window, cx);
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
            self.apply_filter();
            cx.notify();
        }
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

    /// Ctrl+C puts the selected commit on the clipboard, as the log of IDEA does.
    fn copy_selected(&mut self, cx: &mut Context<Self>) {
        let Some(commit) = self
            .selected
            .and_then(|selected| self.shown.get(selected))
            .and_then(|index| self.commits.get(*index))
        else {
            return;
        };
        let text = format!(
            "{} {} {} {}",
            commit.id,
            commit.subject,
            commit.author,
            date_of(commit.time)
        );
        cx.write_to_clipboard(ClipboardItem::new_string(text));
    }

    /// The menu of a commit, as a right click of the log of IDEA opens it.
    fn commit_menu(&mut self, index: usize, position: Point<Pixels>, cx: &mut Context<Self>) {
        self.select(index, cx);
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

        let picked = id.clone();
        let state = git.clone();
        entries.push(
            MenuEntry::new("Cherry-Pick", move |_, cx| {
                let commit = picked.clone();
                state.update(cx, |git, cx| git.cherry_pick(commit, cx));
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

        let compared = short_commit(&id);
        let state = git;
        entries.push(
            MenuEntry::new("Compare with Local", move |_, cx| {
                let commit = compared.clone();
                state.update(cx, |git, cx| git.show_working_tree_diff(commit, cx));
            })
            .icon("general/vcs"),
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

    fn commit_rows(&self, theme: &Theme, cx: &mut Context<Self>) -> Vec<Stateful<Div>> {
        let colors = graph_colors(theme);
        let lanes = self.lanes;
        self.shown
            .iter()
            .enumerate()
            .map(|(index, source)| {
                let commit = &self.commits[*source];
                let selected = self.selected == Some(index);
                let short = &commit.id[..commit.id.len().min(7)];
                let hover = theme.selection_hover;
                let subject = commit.subject.clone();
                let author = commit.author.clone();
                let date = date_of(commit.time);
                let row = self.graph.get(*source).cloned().unwrap_or_default();
                let labels = self.commit_labels(commit);
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
                        this.select(index, cx);
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
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(SPACE_S))
                            .flex_1()
                            .min_w_0()
                            .children(
                                labels.iter().enumerate().map(|(position, label)| {
                                    label_pill(index, position, label, theme)
                                }),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .child(subject),
                            ),
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
                    .child(
                        div()
                            .w(px(90.))
                            .flex_none()
                            .text_color(theme.text_secondary)
                            .child(date),
                    )
            })
            .collect()
    }

    fn file_rows(&self, theme: &Theme, cx: &mut Context<Self>) -> Vec<Stateful<Div>> {
        self.files
            .iter()
            .enumerate()
            .map(|(index, path)| {
                let label = path.clone();
                div()
                    .id(("log-file", index))
                    .debug_selector(move || format!("log-file-{index}"))
                    .flex()
                    .items_center()
                    .flex_none()
                    .h(px(ROW_HEIGHT))
                    .px(px(ROW_PADDING_X))
                    .rounded(px(ROW_RADIUS))
                    .hover({
                        let hover = theme.hover;
                        move |style| style.bg(hover)
                    })
                    .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                        if event.click_count() >= 2 {
                            this.open_diff(index, window, cx);
                        }
                    }))
                    .child(div().child(label))
            })
            .collect()
    }
}

/// `YYYY-MM-DD` in UTC.
fn date_of(seconds: i64) -> String {
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

/// A branch or a tag before the subject of a commit. The branch HEAD points at takes
/// the accent colour, a remote branch and a tag stay quieter than a local one.
fn label_pill(index: usize, position: usize, label: &RefLabel, theme: &Theme) -> impl IntoElement {
    let (background, color) = match (label.kind, label.current) {
        (_, true) => (theme.accent, theme.over_accent),
        (RefKind::Local, false) => (theme.selection, theme.text),
        (RefKind::Remote, false) => (theme.selection_muted, theme.text_secondary),
        (RefKind::Tag, false) => (theme.fold_bg, theme.fold_text),
    };
    let name = div().child(label.name.clone());
    let name = if label.current {
        name.debug_selector(move || format!("log-ref-head-{index}"))
    } else {
        name
    };
    div()
        .debug_selector(move || format!("log-ref-{index}-{position}"))
        .flex()
        .items_center()
        .flex_none()
        .h(px(LABEL_HEIGHT))
        .max_w(px(LABEL_WIDTH))
        .overflow_hidden()
        .whitespace_nowrap()
        .px(px(SPACE_S))
        .rounded(px(CONTROL_RADIUS))
        .bg(background)
        .text_color(color)
        .text_size(px(TEXT_SMALL))
        .child(name)
}

/// Reset Current Branch to Here: the four modes of git, each with what it keeps, as the
/// dialog of IDEA explains them. Hard asks again when the local changes would be lost.
struct ResetView {
    git: Entity<GitState>,
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    commit: String,
}

impl ResetView {
    fn open(
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
        let body = surface(&theme)
            .id("reset-view")
            .debug_selector(|| "reset-view".to_owned())
            .key_context("ResetCurrentBranch")
            .track_focus(&self.focus)
            .occlude()
            .w(width)
            .p(px(POPUP_PADDING))
            .flex()
            .flex_col()
            .gap(px(SPACE_S))
            .on_mouse_down_out(cx.listener(|this, _, window, cx| this.close(window, cx)))
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    this.close(window, cx);
                    cx.stop_propagation();
                }
            }))
            .child(
                div()
                    .text_size(px(TEXT_TITLE))
                    .font_weight(FontWeight::MEDIUM)
                    .child(format!("Reset Current Branch to {short}")),
            )
            .child(
                div()
                    .text_color(theme.text_secondary)
                    .child("The current branch and HEAD move to the chosen commit."),
            )
            .children(rows);
        div()
            .absolute()
            .left(left)
            .top(top)
            .child(enter("reset-enter", body))
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
        let commits = self.commit_rows(&theme, cx);
        let files = self.file_rows(&theme, cx);
        let branch = self.git.read(cx).status().branch.clone();
        let title: SharedString = match &self.path {
            Some(path) => {
                let root = self
                    .git
                    .read(cx)
                    .repo()
                    .map(|repo| repo.root().to_path_buf())
                    .unwrap_or_default();
                format!("Git Log: {}", relative_of(&root, path)).into()
            }
            None => match branch {
                Some(branch) => format!("Git Log: {branch}").into(),
                None => "Git Log".into(),
            },
        };
        let filter_empty = self.filter.text.is_empty();
        let filter_focused = self.focus.is_focused(window);
        let filter_color = if filter_empty {
            theme.text_secondary
        } else {
            theme.text
        };
        let busy = self.loading || self.files_loading;
        let empty = commits.is_empty();
        let no_files = files.is_empty();
        div()
            .id("git-log")
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
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .flex_none()
                    .h(px(CONTROL_HEIGHT))
                    .pl(px(SPACE_M))
                    .pr(px(SPACE_S))
                    .debug_selector(|| "log-header".to_owned())
                    .border_b_1()
                    .border_color(theme.toolbar_border)
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .debug_selector(|| "log-title".to_owned())
                            .child(title),
                    )
                    .child(if busy {
                        div()
                            .text_color(theme.text_secondary)
                            .child("Loading...")
                            .into_any_element()
                    } else {
                        div()
                            .id("log-refresh")
                            .px(px(SPACE_M))
                            .rounded(px(CONTROL_RADIUS))
                            .hover({
                                let hover = theme.hover;
                                move |style| style.bg(hover)
                            })
                            .child("Refresh")
                            .on_click(cx.listener(|this, _, _, cx| this.reload(cx)))
                            .into_any_element()
                    }),
            )
            .child(
                div()
                    .id("log-filter")
                    .debug_selector(|| "log-filter".to_owned())
                    .flex()
                    .items_center()
                    .flex_none()
                    .h(px(CONTROL_HEIGHT))
                    .mx(px(SPACE_M))
                    .my(px(SPACE_S))
                    .px(px(SPACE_M))
                    .rounded(px(CONTROL_RADIUS))
                    .border_1()
                    .border_color(theme.popup_border)
                    .text_color(filter_color)
                    .children(text_field_children_named(
                        "git-log-filter",
                        &self.filter,
                        theme.text,
                        theme.text,
                        theme.selection,
                        theme.caret,
                        16.,
                        filter_focused,
                    ))
                    .when(filter_empty, |this| this.child("Filter commits")),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(
                        scrollable(
                            "log-commits",
                            &self.scroll_commits,
                            div()
                                .id("log-commits")
                                .flex()
                                .flex_col()
                                .px(px(SPACE_M))
                                .overflow_y_scroll()
                                .track_scroll(&self.scroll_commits)
                                .children(commits)
                                .when(empty, |this| {
                                    this.items_center()
                                        .justify_center()
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
                    )
                    .child(
                        div()
                            .id("log-files")
                            .flex()
                            .flex_col()
                            .flex_none()
                            .w(px(FILES_WIDTH))
                            .h_full()
                            .border_l_1()
                            .border_color(theme.toolbar_border)
                            .child(
                                div()
                                    .flex_none()
                                    .h(px(ROW_HEIGHT))
                                    .px(px(SPACE_M))
                                    .flex()
                                    .items_center()
                                    .text_color(theme.text_secondary)
                                    .child("Changed files"),
                            )
                            .child(
                                scrollable(
                                    "log-file-list",
                                    &self.scroll_files,
                                    div()
                                        .id("log-file-list")
                                        .flex()
                                        .flex_col()
                                        .px(px(SPACE_M))
                                        .overflow_y_scroll()
                                        .track_scroll(&self.scroll_files)
                                        .children(files)
                                        .when(no_files, |this| {
                                            this.p_1().text_color(theme.text_secondary).child(
                                                if self.selected.is_some() {
                                                    "No files"
                                                } else {
                                                    "Choose a commit"
                                                },
                                            )
                                        }),
                                    &theme,
                                    window,
                                    cx,
                                )
                                .flex_1()
                                .min_h_0(),
                            ),
                    ),
            )
    }
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
        panel.filter.text.clear();
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
