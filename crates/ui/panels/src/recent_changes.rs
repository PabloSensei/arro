//! Alt+Shift+C: the latest versions of the project from Local History, as one popup. A
//! row names the cause and the file or folder; the runs of automatic and saved versions of
//! one file are one row. Enter opens the history window at the version of the row.

use std::path::Path;

use local_history::{Cause, Revision};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::Workspace;

use crate::history_view::{self, HistoryView};

/// The versions the popup reads from the history of the project root.
const MAX_VERSIONS: usize = 50;
const WIDTH: f32 = 560.;
const HEIGHT: f32 = 420.;
/// The smallest size the window resizes to.
const MIN_WIDTH: f32 = 320.;
const MIN_HEIGHT: f32 = 160.;

/// The window that is open, for scripts.
#[cfg(test)]
struct ActiveRecentChanges(WeakEntity<RecentChangesPopup>);

#[cfg(test)]
impl Global for ActiveRecentChanges {}

/// A row of the popup: a version, or the newest version of a run of saved ones.
#[derive(Clone)]
struct Change {
    revision: Revision,
    /// The cause, or the text of a label.
    title: String,
    /// The name of the file or the folder.
    name: String,
    /// Whether a following linked save of the same file joins this row.
    groupable: bool,
}

/// The name of the file or the folder a version belongs to.
fn name_of(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.to_string_lossy().into_owned(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// The versions as rows, newest first. Runs of automatic and saved versions of one file
/// are one row; a label is a row of its own with its text.
fn changes(revisions: Vec<Revision>) -> Vec<Change> {
    let mut changes: Vec<Change> = Vec::new();
    for revision in revisions.into_iter().take(MAX_VERSIONS) {
        let groupable =
            !revision.is_label() && matches!(revision.cause, Cause::Auto | Cause::Saved);
        if groupable
            && let Some(last) = changes.last()
            && last.groupable
            && ::platform::path_key(&last.revision.path) == ::platform::path_key(&revision.path)
        {
            continue;
        }
        let title = revision
            .label
            .clone()
            .unwrap_or_else(|| revision.cause.title().to_string());
        let name = name_of(&revision.path);
        changes.push(Change {
            revision,
            title,
            name,
            groupable,
        });
    }
    changes
}

pub fn show(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let Some(history) = crate::history::get(cx) else {
        workspace.notify_user("Recent Changes", "The history is not open yet", cx);
        return;
    };
    let root = workspace.root().to_path_buf();
    let rows = changes(history.folder_history(&root));
    let weak = cx.weak_entity();
    let view = cx.new(|cx| RecentChangesPopup {
        workspace: weak,
        focus: cx.focus_handle(),
        rows,
        filter: String::new(),
        selected: 0,
        scroll: ScrollHandle::new(),
    });
    #[cfg(test)]
    cx.set_global(ActiveRecentChanges(view.downgrade()));
    let focus = view.read(cx).focus.clone();
    workspace.show_overlay(view.into(), focus, window, cx);
}

pub struct RecentChangesPopup {
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    rows: Vec<Change>,
    /// The letters typed so far; they filter by the cause and the name.
    filter: String,
    selected: usize,
    scroll: ScrollHandle,
}

impl RecentChangesPopup {
    /// The rows the filter keeps, in the order of the list.
    fn visible(&self) -> Vec<usize> {
        let query = self.filter.to_lowercase();
        self.rows
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                query.is_empty()
                    || format!("{} {}", row.title, row.name)
                        .to_lowercase()
                        .contains(&query)
            })
            .map(|(index, _)| index)
            .collect()
    }

    /// A change of the filter starts again at the first row it keeps.
    fn refilter(&mut self, cx: &mut Context<Self>) {
        self.selected = 0;
        cx.notify();
    }

    fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace
            .update(cx, |workspace, cx| workspace.dismiss(window, cx))
            .ok();
    }

    /// Opens the history window at the version of the row, or at the history of the
    /// folder when the row is a label.
    fn pick(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(change) = self
            .visible()
            .get(index)
            .and_then(|row| self.rows.get(*row))
            .cloned()
        else {
            return;
        };
        let (path, revision) = (change.revision.path.clone(), change.revision.clone());
        let workspace = self.workspace.clone();
        workspace
            .update(cx, |workspace, cx| {
                workspace.dismiss(window, cx);
                HistoryView::show_at(path, &revision, workspace, window, cx);
            })
            .ok();
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = &event.keystroke;
        match key.key.as_str() {
            "escape" => return self.dismiss(window, cx),
            "enter" => return self.pick(self.selected, window, cx),
            "up" => {
                self.selected = self.selected.saturating_sub(1);
                cx.stop_propagation();
                return cx.notify();
            }
            "down" => {
                let last = self.visible().len().saturating_sub(1);
                self.selected = (self.selected + 1).min(last);
                cx.stop_propagation();
                return cx.notify();
            }
            "backspace" => {
                if self.filter.pop().is_some() {
                    self.refilter(cx);
                }
                cx.stop_propagation();
                return;
            }
            _ => {}
        }
        if key.modifiers.control || key.modifiers.alt || key.modifiers.platform {
            return;
        }
        let Some(text) = &key.key_char else {
            return;
        };
        if text.chars().count() != 1 || text.chars().any(char::is_control) {
            return;
        }
        self.filter.push_str(text);
        self.refilter(cx);
        cx.stop_propagation();
    }
}

impl Render for RecentChangesPopup {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let viewport = window.viewport_size();
        let place = DialogPlace {
            left: (f32::from(viewport.width) - WIDTH) / 2.,
            top: f32::from(viewport.height) * 0.205,
            width: WIDTH,
            height: None,
        };
        let visible = self.visible();
        let rows: Vec<_> = visible
            .iter()
            .enumerate()
            .filter_map(|(position, index)| {
                let row = self.rows.get(*index)?;
                let chosen = position == self.selected;
                let hover = theme.selection_hover;
                Some(
                    div()
                        .id(("recent-changes-row", *index))
                        .debug_selector({
                            let index = *index;
                            move || format!("recent-changes-row-{index}")
                        })
                        .flex()
                        .items_center()
                        .gap(px(SPACE_M))
                        .flex_none()
                        .h(px(ROW_HEIGHT))
                        .px(px(ROW_PADDING_X))
                        .rounded(px(ROW_RADIUS))
                        .when(chosen, |this| this.bg(theme.selection))
                        .when(!chosen, |this| this.hover(move |style| style.bg(hover)))
                        .child(
                            div()
                                .flex_none()
                                .text_color(theme.text)
                                .child(SharedString::from(row.title.clone())),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_color(theme.text)
                                .child(SharedString::from(row.name.clone())),
                        )
                        .child(
                            div()
                                .flex_none()
                                .text_color(theme.text_secondary)
                                .child(SharedString::from(history_view::age(row.revision.at))),
                        )
                        .on_click(
                            cx.listener(move |this, _, window, cx| this.pick(position, window, cx)),
                        )
                        .into_any_element(),
                )
            })
            .collect();
        let header = popup_drag_header(
            cx,
            "recent-changes",
            (MIN_WIDTH, MIN_HEIGHT),
            div()
                .px(px(ROW_PADDING_X))
                .h(px(CONTROL_HEIGHT))
                .flex_none()
                .flex()
                .items_center()
                .text_color(theme.text_secondary)
                .debug_selector(|| "recent-changes-title".to_owned())
                .child("Recent Changes"),
        );
        let filter = (!self.rows.is_empty()).then(|| {
            let empty = self.filter.is_empty();
            div()
                .id("recent-changes-filter")
                .debug_selector(|| "recent-changes-filter".to_owned())
                .flex()
                .items_center()
                .flex_none()
                .h(px(CONTROL_HEIGHT))
                .px(px(ROW_PADDING_X))
                .text_color(if empty {
                    theme.text_secondary
                } else {
                    theme.text
                })
                .child(if empty {
                    SharedString::from("Type to filter")
                } else {
                    SharedString::from(self.filter.clone())
                })
        });
        let body = surface(&theme)
            .id("recent-changes-popup")
            .debug_selector(|| "recent-changes-popup".to_owned())
            .relative()
            .track_focus(&self.focus)
            .occlude()
            .p(px(POPUP_PADDING))
            .flex()
            .flex_col()
            .on_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)))
            .on_mouse_down_out(cx.listener(|this, _, window, cx| this.dismiss(window, cx)))
            .child(header)
            .children(filter)
            .child(if self.rows.is_empty() {
                div()
                    .id("recent-changes-empty")
                    .debug_selector(|| "recent-changes-empty".to_owned())
                    .flex_none()
                    .px(px(ROW_PADDING_X))
                    .py(px(SPACE_M))
                    .text_color(theme.text_secondary)
                    .child("No recent changes")
                    .into_any_element()
            } else if rows.is_empty() {
                div()
                    .id("recent-changes-no-match")
                    .debug_selector(|| "recent-changes-no-match".to_owned())
                    .flex_none()
                    .px(px(ROW_PADDING_X))
                    .py(px(SPACE_M))
                    .text_color(theme.text_secondary)
                    .child("No version matches the search")
                    .into_any_element()
            } else {
                scrollable(
                    "recent-changes-rows",
                    &self.scroll,
                    div()
                        .id("recent-changes-rows")
                        .flex()
                        .flex_col()
                        .overflow_y_scroll()
                        .track_scroll(&self.scroll)
                        .children(rows),
                    &theme,
                    window,
                    cx,
                )
                .min_h_0()
                .into_any_element()
            });
        popup_window(
            PopupSpec {
                key: "recent-changes",
                place,
                min: (MIN_WIDTH, MIN_HEIGHT),
                content_height: true,
                max_height: Some(HEIGHT),
            },
            body,
            window,
            cx,
        )
    }
}

/// The rows of the open popup, for scripts.
#[cfg(test)]
pub(crate) fn report(cx: &App) -> Vec<String> {
    let Some(view) = cx
        .try_global::<ActiveRecentChanges>()
        .and_then(|active| active.0.upgrade())
    else {
        return Vec::new();
    };
    let view = view.read(cx);
    if view.rows.is_empty() {
        return vec!["No recent changes".to_owned()];
    }
    let visible = view.visible();
    if visible.is_empty() {
        return vec!["No version matches the search".to_owned()];
    }
    visible
        .into_iter()
        .filter_map(|index| view.rows.get(index))
        .map(|row| format!("recent-change {} · {}", row.title, row.name))
        .collect()
}

/// The version the keyboard would open, for scripts.
#[cfg(test)]
pub(crate) fn selected(cx: &App) -> Option<(u64, String)> {
    let view = cx
        .try_global::<ActiveRecentChanges>()
        .and_then(|active| active.0.upgrade())?;
    let view = view.read(cx);
    let index = view.visible().get(view.selected).copied()?;
    let row = view.rows.get(index)?;
    Some((row.revision.id, row.title.clone()))
}

#[cfg(test)]
mod checks {
    use std::path::PathBuf;
    use std::time::SystemTime;

    use local_history::{Cause, Revision};

    use super::{MAX_VERSIONS, changes};

    fn revision(id: u64, path: &str, cause: Cause) -> Revision {
        Revision {
            id,
            at: SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(id),
            path: PathBuf::from(path),
            cause,
            content: Some(String::new()),
            label: None,
        }
    }

    #[test]
    fn a_run_of_saved_versions_of_one_file_is_one_row() {
        let rows = changes(vec![
            revision(3, "src/Owner.txt", Cause::Saved),
            revision(2, "src/Owner.txt", Cause::Auto),
            revision(1, "src/Owner.txt", Cause::Saved),
        ]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].title, "Saved");
    }

    #[test]
    fn another_file_or_cause_breaks_the_run() {
        let rows = changes(vec![
            revision(4, "src/Owner.txt", Cause::BeforeRename),
            revision(3, "src/Owner.txt", Cause::Saved),
            revision(2, "src/Other.txt", Cause::Saved),
            revision(1, "src/Owner.txt", Cause::Saved),
        ]);
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0].title, "Rename");
    }

    #[test]
    fn the_popup_reads_the_latest_versions_only() {
        let rows = changes(
            (0..MAX_VERSIONS as u64 + 5)
                .map(|id| revision(id, &format!("src/File{id}.txt"), Cause::Saved))
                .collect(),
        );
        assert_eq!(rows.len(), MAX_VERSIONS);
    }
}
