//! History for Selection: the commits that changed the selected lines of the file, with
//! the diff of each of them, and History for Method of the declaration under the caret.
//! `git log -L` follows the lines through the commits and the renames of the file.

use std::path::{Path, PathBuf};

use text_core::decode;
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use vcs::{Hunk, LineHistory};
use workspace::{SELECTION_HISTORY_TOOL, Workspace};

use crate::editor_tab::style_of;
use crate::git::{GitState, relative_of, short_commit};
use crate::git_log;

const LIST_WIDTH: f32 = 460.;
const HISTORY_CONTEXT: &str = "SelectionHistory";

/// One row of the window: the uncommitted changes of the lines, or a commit.
enum Entry {
    Local {
        base: String,
        current: String,
        hunks: Vec<Hunk>,
    },
    Commit(LineHistory),
}

impl Entry {
    fn subject(&self) -> String {
        match self {
            Self::Local { .. } => "Local Changes".to_owned(),
            Self::Commit(entry) => entry.commit.subject.clone(),
        }
    }

    fn author(&self) -> String {
        match self {
            Self::Local { .. } => String::new(),
            Self::Commit(entry) => entry.commit.author.clone(),
        }
    }

    fn time(&self) -> i64 {
        match self {
            Self::Local { .. } => i64::MAX,
            Self::Commit(entry) => entry.commit.time,
        }
    }

    fn id(&self) -> Option<&str> {
        match self {
            Self::Local { .. } => None,
            Self::Commit(entry) => Some(&entry.commit.id),
        }
    }

    fn path(&self) -> Option<&str> {
        match self {
            Self::Local { .. } => None,
            Self::Commit(entry) => Some(&entry.path),
        }
    }
}

pub struct SelectionHistoryPanel {
    git: Entity<GitState>,
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    entries: Vec<Entry>,
    selected: Option<usize>,
    loading: bool,
    /// The file and the lines the window shows, for the title.
    path: Option<PathBuf>,
    lines: (u32, u32),
    scroll_list: ScrollHandle,
    scroll_diff: ScrollHandle,
    /// A result of a load that a newer one replaced is dropped.
    serial: u64,
}

pub struct ActiveSelectionHistory(pub Entity<SelectionHistoryPanel>);

impl Global for ActiveSelectionHistory {}

impl SelectionHistoryPanel {
    pub fn create(
        git: Entity<GitState>,
        workspace: WeakEntity<Workspace>,
        cx: &mut App,
    ) -> Entity<Self> {
        cx.new(|cx| Self {
            git,
            workspace,
            focus: cx.focus_handle(),
            entries: Vec::new(),
            selected: None,
            loading: false,
            path: None,
            lines: (1, 1),
            scroll_list: ScrollHandle::new(),
            scroll_diff: ScrollHandle::new(),
            serial: 0,
        })
    }

    pub fn focus(&self) -> FocusHandle {
        self.focus.clone()
    }

    /// Loads the history of `lines` of `path`; `text` is the text the editor shows, so
    /// that the uncommitted changes of the lines come first.
    pub fn show(&mut self, path: PathBuf, text: String, lines: (u32, u32), cx: &mut Context<Self>) {
        let Some(repo) = self.git.read(cx).repo() else {
            return;
        };
        let top = relative_of(repo.root(), &path);
        let Some(repository) = self.git.read(cx).repo_of(&top) else {
            return;
        };
        self.serial = self.serial.wrapping_add(1);
        let serial = self.serial;
        self.path = Some(path.clone());
        self.lines = lines;
        self.loading = true;
        self.entries.clear();
        self.selected = None;
        cx.notify();
        let job = cx.background_executor().spawn(async move {
            let relative = relative_of(repository.root(), &path);
            let local = repository
                .blobs()
                .ok()
                .and_then(|mut blobs| repository.file_diff(&mut blobs, &relative, &text).ok())
                .and_then(|diff| {
                    let start = lines.0.saturating_sub(1);
                    let end = lines.1;
                    let touched: Vec<Hunk> = diff
                        .hunks
                        .into_iter()
                        .filter(|hunk| {
                            if hunk.after.is_empty() {
                                (start..=end).contains(&hunk.after.start)
                            } else {
                                hunk.after.start < end && hunk.after.end > start
                            }
                        })
                        .collect();
                    (!touched.is_empty()).then_some((diff.base, diff.current, touched))
                });
            // An untracked file has no commits yet: the local changes are the history.
            let history = match repository.line_history(&relative, lines.0, lines.1) {
                Ok(history) => history,
                Err(_) if local.is_some() => Vec::new(),
                Err(error) => return Err(error),
            };
            Ok::<_, std::io::Error>((local, history))
        });
        cx.spawn(async move |this, cx| {
            let result = job.await;
            this.update(cx, |this, cx| {
                if serial != this.serial {
                    return;
                }
                this.loading = false;
                match result {
                    Ok((local, history)) => {
                        if let Some((base, current, hunks)) = local {
                            this.entries.push(Entry::Local {
                                base,
                                current,
                                hunks,
                            });
                        }
                        this.entries.extend(history.into_iter().map(Entry::Commit));
                        this.selected = (!this.entries.is_empty()).then_some(0);
                    }
                    Err(message) => this.git.update(cx, |git, cx| {
                        git.report("History for Selection", message.to_string(), cx)
                    }),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn select(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.entries.len() {
            self.selected = Some(index);
            cx.notify();
        }
    }

    fn selected_entry(&self) -> Option<&Entry> {
        self.selected.and_then(|index| self.entries.get(index))
    }

    /// Opens the file as the selected commit has it, in a read-only tab of its own.
    fn open_revision(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.selected_entry() else {
            return;
        };
        let (Some(id), Some(path)) = (entry.id(), entry.path()) else {
            return;
        };
        let (id, path) = (id.to_owned(), path.to_owned());
        let window = window.window_handle();
        let Some(absolute) = self.path.clone() else {
            return;
        };
        let Some(repo) = self.git.read(cx).repo() else {
            return;
        };
        let top = relative_of(repo.root(), &absolute);
        let Some(repository) = self.git.read(cx).repo_of(&top) else {
            return;
        };
        let job_id = id.clone();
        let job = cx.background_executor().spawn(async move {
            let mut blobs = repository.blobs().ok()?;
            let text = blobs.read(&format!("{job_id}:{path}")).ok()??;
            Some(decode(&text).text)
        });
        let workspace = self.workspace.clone();
        cx.spawn(async move |this, cx| {
            let Some(text) = job.await else {
                return;
            };
            this.update(cx, |_, cx| {
                window
                    .update(cx, |_, window, cx| {
                        let style = style_of(cx);
                        let short = short_commit(&id);
                        let title = format!(
                            "{} ({short})",
                            absolute
                                .file_name()
                                .map(|name| name.to_string_lossy().into_owned())
                                .unwrap_or_default()
                        );
                        let tab_path = absolute.with_file_name(format!(
                            ".{short}-{}",
                            absolute
                                .file_name()
                                .map(|name| name.to_string_lossy().into_owned())
                                .unwrap_or_default()
                        ));
                        let view = cx.new(|cx| {
                            editor_view::EditorView::open_text(
                                absolute.clone(),
                                text,
                                true,
                                style,
                                cx,
                            )
                        });
                        let focus = view.read(cx).focus_handle(cx);
                        workspace
                            .update(cx, |workspace, cx| {
                                workspace.open_view_tab(
                                    tab_path,
                                    title,
                                    "general/vcs",
                                    view.into(),
                                    focus,
                                    window,
                                    cx,
                                );
                            })
                            .ok();
                    })
                    .ok();
            })
            .ok();
        })
        .detach();
    }

    /// The commits of the selected commit in the Git Log window, as IDEA's Show Log does.
    fn open_in_log(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.selected_entry().and_then(|entry| entry.id()) else {
            return;
        };
        let id = id.to_owned();
        self.workspace
            .update(cx, |workspace, cx| {
                git_log::show_commit(&id, workspace, window, cx)
            })
            .ok();
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "up" => {
                let selected = self.selected.map_or(0, |index| index.saturating_sub(1));
                self.select(selected, cx);
                cx.stop_propagation();
            }
            "down" => {
                let last = self.entries.len().saturating_sub(1);
                let selected = self.selected.map_or(0, |index| (index + 1).min(last));
                self.select(selected, cx);
                cx.stop_propagation();
            }
            "enter" | "f4" => {
                self.open_revision(window, cx);
                cx.stop_propagation();
            }
            _ => {}
        }
    }

    fn row(&self, index: usize, theme: &Theme, cx: &mut Context<Self>) -> Stateful<Div> {
        let entry = &self.entries[index];
        let selected = self.selected == Some(index);
        let hover = theme.selection_hover;
        let short = entry.id().map_or_else(|| "local".to_owned(), short_commit);
        let subject = entry.subject();
        let author = entry.author();
        let time = entry.time();
        let date = if time == i64::MAX {
            String::new()
        } else {
            git_log::date_of(time)
        };
        div()
            .id(("selection-history-row", index))
            .debug_selector(move || format!("selection-history-row-{index}"))
            .flex()
            .items_center()
            .gap(px(SPACE_S))
            .flex_none()
            .h(px(ROW_HEIGHT))
            .px(px(ROW_PADDING_X))
            .rounded(px(ROW_RADIUS))
            .when(selected, |this| this.bg(theme.selection_inactive))
            .when(!selected, |this| this.hover(move |style| style.bg(hover)))
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                this.select(index, cx);
                if event.click_count() >= 2 {
                    this.open_revision(window, cx);
                }
            }))
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
                    .w(px(110.))
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
    }

    fn diff(&self, theme: &Theme) -> AnyElement {
        let Some(entry) = self.selected_entry() else {
            return div()
                .p(px(SPACE_M))
                .text_color(theme.text_secondary)
                .child("Choose a commit")
                .into_any_element();
        };
        let rows: Vec<AnyElement> = match entry {
            Entry::Local {
                base,
                current,
                hunks,
            } => hunks
                .iter()
                .map(|hunk| {
                    crate::diff_view::hunk_lines(base, current, hunk, theme).into_any_element()
                })
                .collect(),
            Entry::Commit(entry) => {
                let hunks = vcs::line_hunks(&entry.before, &entry.after);
                hunks
                    .iter()
                    .map(|hunk| {
                        crate::diff_view::hunk_lines(&entry.before, &entry.after, hunk, theme)
                            .into_any_element()
                    })
                    .collect()
            }
        };
        if rows.is_empty() {
            return div()
                .p(px(SPACE_M))
                .text_color(theme.text_secondary)
                .child("No changes in the selection")
                .into_any_element();
        }
        div()
            .id("selection-history-diff-body")
            .flex()
            .flex_col()
            .p(px(SPACE_S))
            .children(rows)
            .into_any_element()
    }
}

impl Render for SelectionHistoryPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let rows: Vec<AnyElement> = (0..self.entries.len())
            .map(|index| self.row(index, &theme, cx).into_any_element())
            .collect();
        let empty = rows.is_empty();
        let diff = self.diff(&theme);
        let title: SharedString = match &self.path {
            Some(path) => {
                let name = path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default();
                format!(
                    "History for Selection: {name}:{}-{}",
                    self.lines.0, self.lines.1
                )
                .into()
            }
            None => "History for Selection".into(),
        };
        let can_open_log = self.selected_entry().and_then(Entry::id).is_some();
        div()
            .id("selection-history")
            .debug_selector(|| "selection-history".to_owned())
            .key_context(HISTORY_CONTEXT)
            .track_focus(&self.focus)
            .flex()
            .flex_col()
            .size_full()
            .bg(theme.island_bg)
            .text_color(theme.text)
            .on_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .flex_none()
                    .h(px(CONTROL_HEIGHT))
                    .pl(px(SPACE_M))
                    .pr(px(SPACE_S))
                    .border_b_1()
                    .border_color(theme.toolbar_border)
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .debug_selector(|| "selection-history-title".to_owned())
                            .child(title),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(SPACE_S))
                            .when(self.loading, |this| {
                                this.child(
                                    div().text_color(theme.text_secondary).child("Loading..."),
                                )
                            })
                            .when(can_open_log, |this| {
                                this.child(
                                    div()
                                        .id("selection-history-open-log")
                                        .debug_selector(|| "selection-history-open-log".to_owned())
                                        .px(px(SPACE_M))
                                        .rounded(px(CONTROL_RADIUS))
                                        .cursor_pointer()
                                        .hover({
                                            let hover = theme.hover;
                                            move |style| style.bg(hover)
                                        })
                                        .child("Open in Git Log")
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.open_in_log(window, cx)
                                        })),
                                )
                            }),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(
                        scrollable(
                            "selection-history-list",
                            &self.scroll_list,
                            div()
                                .id("selection-history-list")
                                .flex()
                                .flex_col()
                                .px(px(SPACE_M))
                                .overflow_y_scroll()
                                .track_scroll(&self.scroll_list)
                                .children(rows)
                                .when(empty, |this| {
                                    this.p_1()
                                        .text_color(theme.text_secondary)
                                        .child("No commits")
                                }),
                            &theme,
                            window,
                            cx,
                        )
                        .w(px(LIST_WIDTH))
                        .flex_none()
                        .min_h_0(),
                    )
                    .child(
                        scrollable(
                            "selection-history-diff",
                            &self.scroll_diff,
                            div()
                                .id("selection-history-diff")
                                .flex()
                                .flex_col()
                                .overflow_y_scroll()
                                .track_scroll(&self.scroll_diff)
                                .child(diff),
                            &theme,
                            window,
                            cx,
                        )
                        .flex_1()
                        .min_w_0()
                        .min_h_0(),
                    ),
            )
    }
}

impl SelectionHistoryPanel {
    /// The lines the window shows, for a test.
    #[cfg(test)]
    pub fn lines(&self) -> (u32, u32) {
        self.lines
    }

    /// The subjects of the rows, for a test.
    #[cfg(test)]
    pub fn subjects(&self) -> Vec<String> {
        self.entries.iter().map(Entry::subject).collect()
    }

    /// The diff of the selected row as text, for a test.
    #[cfg(test)]
    pub fn diff_text(&self) -> String {
        match self.selected_entry() {
            Some(Entry::Local {
                base,
                current,
                hunks,
            }) => {
                let before: Vec<&str> = base.lines().collect();
                let after: Vec<&str> = current.lines().collect();
                hunks
                    .iter()
                    .map(|hunk| {
                        let old = before
                            .get(hunk.before.start as usize..hunk.before.end as usize)
                            .unwrap_or_default()
                            .join("\n");
                        let new = after
                            .get(hunk.after.start as usize..hunk.after.end as usize)
                            .unwrap_or_default()
                            .join("\n");
                        format!("{old}\n{new}")
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            }
            Some(Entry::Commit(entry)) => {
                format!("{}\n{}", entry.before, entry.after)
            }
            None => String::new(),
        }
    }
}

/// History for Selection of the caret or the selected lines.
pub fn show_selection(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    show_at(workspace, window, cx, false);
}

/// History for Method of the declaration under the caret.
pub fn show_method(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    show_at(workspace, window, cx, true);
}

fn show_at(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
    method: bool,
) {
    let Some((path, view)) = workspace.active_editor() else {
        return;
    };
    let Ok(view) = view.downcast::<editor_view::EditorView>() else {
        return;
    };
    let Some(text) = view.read(cx).text() else {
        return;
    };
    let Some((range, head)) = view.read(cx).selection() else {
        return;
    };
    let lines = if method {
        declaration_lines(cx, &path, &text, head)
    } else {
        let start = line_of(&text, range.start.min(range.end));
        let end_offset = if range.end > range.start && text[..range.end].ends_with('\n') {
            range.end - 1
        } else {
            range.end.max(range.start)
        };
        let end = line_of(&text, end_offset);
        Some((start + 1, end.max(start) + 1))
    };
    let Some(lines) = lines else {
        if let Some(git) = cx
            .try_global::<crate::git::ActiveGit>()
            .map(|git| git.0.clone())
        {
            // The command runs while the window is being updated.
            cx.defer(move |cx| {
                git.update(cx, |git, cx| {
                    git.report(
                        "History for Method",
                        "Put the caret on a declaration".to_string(),
                        cx,
                    )
                });
            });
        }
        return;
    };
    let Some(panel) = cx
        .try_global::<ActiveSelectionHistory>()
        .map(|panel| panel.0.clone())
    else {
        return;
    };
    workspace.show_tool(SELECTION_HISTORY_TOOL, window, cx);
    panel.update(cx, |panel, cx| panel.show(path, text, lines, cx));
}

/// The lines of the declaration that holds `offset`: the innermost symbol of the file.
fn declaration_lines(cx: &App, path: &Path, text: &str, offset: usize) -> Option<(u32, u32)> {
    let support = cx.try_global::<editor_view::Semantic>()?.0.clone();
    if !support.matches(path) {
        return None;
    }
    let provider = support.symbols()?;
    let symbols = provider.file_symbols(path, text);
    let symbol = symbols
        .iter()
        .filter(|symbol| symbol.range.start <= offset && offset < symbol.range.end)
        .min_by_key(|symbol| symbol.range.end - symbol.range.start)?;
    let start = line_of(text, symbol.range.start);
    let end = line_of(text, symbol.range.end.saturating_sub(1));
    Some((start + 1, end.max(start) + 1))
}

/// The line of `offset`, counted from zero.
fn line_of(text: &str, offset: usize) -> u32 {
    text[..offset.min(text.len())]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count() as u32
}
