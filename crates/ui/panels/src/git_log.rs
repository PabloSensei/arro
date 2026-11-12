//! The Git Log tool window: the commits of the branch or of one file, the files a commit
//! changed and the diff of one of them.

use std::path::PathBuf;

use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use vcs::Commit;
use workspace::{LOG_TOOL, Workspace};

use crate::diff_view::DiffView;
use crate::git::{GitState, relative_of};
use crate::prompt::{Prompt, TextField};

const LIMIT: usize = 200;
const FILES_WIDTH: f32 = 260.;
const CONTEXT: &str = "GitLog";

actions!(
    git_log_panel,
    [RewordCommit, BranchAtCommit, ShowAllAffected]
);

/// One key of the log and the IDEA action it implements.
#[cfg(test)]
pub(crate) struct LogKey {
    pub(crate) keys: &'static str,
    pub(crate) action: &'static str,
    pub(crate) idea: Option<&'static str>,
}

macro_rules! log_keys {
    ($(($keys:literal, $action:ident, $idea:expr)),* $(,)?) => {
        /// F2, Shift+F6, Ctrl+Alt+N and Alt+Shift+A of the log.
        fn bind_keys(cx: &mut App) {
            cx.bind_keys([$(KeyBinding::new($keys, $action, Some(CONTEXT))),*]);
        }

        #[cfg(test)]
        pub(crate) const KEYS: &[LogKey] = &[
            $(LogKey {
                keys: $keys,
                action: concat!("git_log_panel::", stringify!($action)),
                idea: $idea,
            }),*
        ];
    };
}

log_keys![
    ("f2", RewordCommit, Some("Git.Reword.Commit")),
    ("shift-f6", RewordCommit, Some("Git.Reword.Commit")),
    ("ctrl-alt-n", BranchAtCommit, Some("Git.New.Branch.In.Log")),
    (
        "alt-shift-a",
        ShowAllAffected,
        Some("VcsHistory.ShowAllAffected")
    ),
];

pub struct GitLogPanel {
    git: Entity<GitState>,
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    /// `None` for the branch, `Some` for Show History of one file.
    path: Option<PathBuf>,
    commits: Vec<Commit>,
    /// The indices of `commits` that the filter keeps.
    shown: Vec<usize>,
    files: Vec<String>,
    selected: Option<usize>,
    filter: TextField,
    loading: bool,
    files_loading: bool,
    head: Option<String>,
    /// The commit that is selected again after the next load, as Show All Affected does.
    want: Option<String>,
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
        let panel = cx.new(|cx| {
            let observe = cx.observe(&git, |this: &mut Self, git, cx| {
                let head = git.read(cx).status().head.clone();
                if head != this.head {
                    this.head = head;
                    this.reload(cx);
                }
            });
            Self {
                git,
                workspace,
                focus: cx.focus_handle(),
                path: None,
                commits: Vec::new(),
                shown: Vec::new(),
                files: Vec::new(),
                selected: None,
                filter: TextField::default(),
                loading: false,
                files_loading: false,
                head,
                want: None,
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
            result.map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = job.await;
            this.update(cx, |this, cx| {
                this.loading = false;
                match result {
                    Ok(commits) => {
                        this.commits = commits;
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

    fn commit_rows(&self, theme: &Theme, cx: &mut Context<Self>) -> Vec<Stateful<Div>> {
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
                div()
                    .id(("log-commit", index))
                    .debug_selector(move || format!("log-commit-{index}"))
                    .flex()
                    .items_center()
                    .gap(px(8.))
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
        let shown: SharedString = if self.filter.text.is_empty() {
            "Filter commits".into()
        } else {
            self.filter.text.clone().into()
        };
        let filter_color = if self.filter.text.is_empty() {
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
                    .child(shown),
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
