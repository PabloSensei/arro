//! Ctrl+Shift+E and Shift+F11: the places where the user was working and the bookmarks, as
//! one list with the line each place points at.

use std::path::{Path, PathBuf};

use editor_view::{EditorView, Spot};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::Workspace;

pub use workspace::Location;
#[cfg(test)]
pub use workspace::LocationLog;

use crate::navigation;
use crate::outline::OpenEditors;

const WIDTH: f32 = 640.;
const HEIGHT: f32 = 420.;
/// The smallest size the window resizes to.
const MIN_WIDTH: f32 = 360.;
const MIN_HEIGHT: f32 = 200.;
/// Characters of the line shown for a place.
const SNIPPET: usize = 90;

/// Remembers where the caret of the editor is.
pub fn remember(view: &Entity<EditorView>, workspace: &mut Workspace, cx: &mut Context<Workspace>) {
    let place = {
        let view = view.read(cx);
        let line = view.caret().0;
        view.line_text(line).map(|text| Location {
            path: view.path().to_path_buf(),
            line,
            text,
        })
    };
    if let Some(place) = place {
        workspace.remember_place(place);
    }
}

/// The place of the caret of the editor of `path`, when that editor is open.
fn caret_place(workspace: &Workspace, path: &Path, cx: &App) -> Option<Location> {
    let view = workspace.view_of(path)?;
    let editor = view.downcast::<EditorView>().ok()?;
    let view = editor.read(cx);
    let line = view.caret().0;
    view.line_text(line).map(|text| Location {
        path: view.path().to_path_buf(),
        line,
        text,
    })
}

/// Remembers where the caret of the editor of `path` is, when that editor is open.
pub fn remember_path(workspace: &mut Workspace, path: &Path, cx: &mut Context<Workspace>) {
    if let Some(place) = caret_place(workspace, path, cx) {
        workspace.remember_place(place);
    }
}

/// One row of the popup.
struct Row {
    /// The bookmark name, empty for a place.
    badge: String,
    path: PathBuf,
    line: usize,
    text: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Recent,
    Bookmarks,
}

pub struct PlacesPopup {
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    kind: Kind,
    rows: Vec<Row>,
    selected: usize,
    scroll: ScrollHandle,
}

fn line_now(path: &Path, line: usize, cx: &App) -> String {
    if let Some(editor) = OpenEditors::get(cx, path)
        && let Some(text) = editor.read(cx).line_text(line)
    {
        return text;
    }
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| text.lines().nth(line).map(str::to_owned))
        .unwrap_or_default()
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map_or(String::new(), |name| name.to_string_lossy().into_owned())
}

pub fn show_recent(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    // The caret of the open file is a place of its own, so the list of a window with a
    // file open is never empty. A place that is already in the list keeps its position.
    if let Some(path) = workspace.active_path() {
        let known = caret_place(workspace, &path, cx).is_some_and(|place| {
            workspace
                .recent_places()
                .iter()
                .any(|known| known.path == place.path && known.line == place.line)
        });
        if !known {
            remember_path(workspace, &path, cx);
        }
    }
    // Places of files that are gone cannot be opened, and the next save drops them.
    if workspace.prune_recent_places() {
        workspace.tab_layout_changed(cx);
    }
    let rows = workspace
        .recent_places()
        .iter()
        .map(|place| Row {
            badge: String::new(),
            path: place.path.clone(),
            line: place.line,
            // The file may have changed since the place was remembered.
            text: line_now(&place.path, place.line, cx),
        })
        .collect();
    show(Kind::Recent, rows, workspace, window, cx);
}

pub fn show_bookmarks(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let rows = editor_view::bookmarks(cx)
        .into_iter()
        .map(|entry| {
            let line = entry.line as usize;
            Row {
                badge: entry.mnemonic.map(String::from).unwrap_or_default(),
                text: line_now(&entry.path, line, cx),
                path: entry.path,
                line,
            }
        })
        .collect();
    show(Kind::Bookmarks, rows, workspace, window, cx);
}

fn show(
    kind: Kind,
    rows: Vec<Row>,
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    // Recent Locations and Bookmarks both have a popup of their own, with an empty state
    // of their own: no notification takes the place of an empty list.
    let weak = cx.weak_entity();
    let view = cx.new(|cx| PlacesPopup {
        workspace: weak,
        focus: cx.focus_handle(),
        kind,
        rows,
        selected: 0,
        scroll: ScrollHandle::new(),
    });
    let focus = view.read(cx).focus.clone();
    let tag = match kind {
        Kind::Recent => workspace::RECENT_PLACES_POPUP,
        Kind::Bookmarks => workspace::BOOKMARKS_POPUP,
    };
    workspace.show_overlay_tagged(view.into(), focus, tag, window, cx);
}

impl PlacesPopup {
    fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace
            .update(cx, |workspace, cx| workspace.dismiss(window, cx))
            .ok();
    }

    fn pick(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.rows.get(index) else {
            return;
        };
        let (path, line) = (row.path.clone(), row.line);
        let spot = Spot::Line { line, column: 0 };
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.dismiss(window, cx);
                navigation::go_to(workspace, path, Some(spot), true, window, cx)
            })
            .ok();
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let last = self.rows.len().saturating_sub(1);
        match event.keystroke.key.as_str() {
            "up" => self.selected = self.selected.saturating_sub(1),
            "down" => self.selected = (self.selected + 1).min(last),
            "enter" => return self.pick(self.selected, window, cx),
            "escape" => return self.dismiss(window, cx),
            // A bookmark is picked by its name, a place by its number.
            key if key.chars().count() == 1 => {
                let typed = key.chars().next().unwrap_or(' ').to_ascii_uppercase();
                let found = match self.kind {
                    Kind::Bookmarks => self
                        .rows
                        .iter()
                        .position(|row| row.badge.to_ascii_uppercase() == typed.to_string()),
                    Kind::Recent => typed
                        .to_digit(10)
                        .filter(|number| *number >= 1)
                        .map(|number| number as usize - 1),
                };
                match found {
                    Some(index) => return self.pick(index, window, cx),
                    None => return,
                }
            }
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }
}

fn snippet(text: &str) -> String {
    let text = text.trim();
    let mut shown: String = text.chars().take(SNIPPET).collect();
    if text.chars().count() > SNIPPET {
        shown.push('\u{2026}');
    }
    shown
}

impl Render for PlacesPopup {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let scripted = cx.has_global::<ScriptSpots>();
        let viewport = window.viewport_size();
        let first = DialogPlace {
            left: (f32::from(viewport.width) - WIDTH) / 2.,
            top: f32::from(viewport.height) * 0.205,
            width: WIDTH,
            height: None,
        };
        let key = match self.kind {
            Kind::Recent => "recent-places",
            Kind::Bookmarks => "bookmarks-popup",
        };
        let rows: Vec<_> = self
            .rows
            .iter()
            .enumerate()
            .map(|(index, row)| {
                let chosen = index == self.selected;
                let badge = match self.kind {
                    Kind::Bookmarks if row.badge.is_empty() => "\u{2022}".to_owned(),
                    Kind::Bookmarks => row.badge.clone(),
                    Kind::Recent if index < 9 => (index + 1).to_string(),
                    Kind::Recent => String::new(),
                };
                div()
                    .id(("places-row", index))
                    .debug_selector(move || format!("places-row-{index}"))
                    .flex()
                    .items_center()
                    .flex_none()
                    .h(px(ROW_HEIGHT))
                    .px(px(ROW_PADDING_X))
                    .gap(px(SPACE_M))
                    .rounded(px(ROW_RADIUS))
                    .when(chosen, |this| this.bg(theme.selection))
                    .child(
                        div()
                            .w(px(16.))
                            .text_color(theme.accent)
                            .child(SharedString::from(badge)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(snippet(&row.text)),
                    )
                    .child(div().flex_none().text_color(theme.text_secondary).child(
                        SharedString::from(format!("{}:{}", file_name(&row.path), row.line + 1)),
                    ))
                    .on_click(cx.listener(move |this, _, window, cx| this.pick(index, window, cx)))
            })
            .collect();
        let title = match self.kind {
            Kind::Recent => "Recent Locations",
            Kind::Bookmarks => "Bookmarks",
        };
        let header = popup_drag_header(
            cx,
            key,
            (MIN_WIDTH, MIN_HEIGHT),
            div()
                .px(px(ROW_PADDING_X))
                .h(px(CONTROL_HEIGHT))
                .flex_none()
                .flex()
                .items_center()
                .text_color(theme.text_secondary)
                .debug_selector(|| "places-title".to_owned())
                .child(title),
        );
        let body = surface(&theme)
            .id("places-popup")
            .debug_selector(|| "places-popup".to_owned())
            .relative()
            .child(script_spot(scripted, "places-popup"))
            .track_focus(&self.focus)
            .occlude()
            .p(px(POPUP_PADDING))
            .flex()
            .flex_col()
            .on_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)))
            .on_mouse_down_out(cx.listener(|this, _, window, cx| this.dismiss(window, cx)))
            .child(header)
            .child(if rows.is_empty() {
                // Both lists have a popup of their own even with nothing in it: the list
                // says so in place of its rows, without a notification.
                div()
                    .id("places-empty")
                    .debug_selector(|| "places-empty".to_owned())
                    .relative()
                    .child(script_spot(scripted, "places-empty"))
                    .flex_none()
                    .px(px(ROW_PADDING_X))
                    .py(px(SPACE_M))
                    .text_color(theme.text_secondary)
                    .child(match self.kind {
                        Kind::Recent => "No recent locations",
                        Kind::Bookmarks => "No bookmarks yet",
                    })
                    .into_any_element()
            } else {
                scrollable(
                    "places-rows",
                    &self.scroll,
                    div()
                        .id("places-rows")
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
                key,
                place: first,
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

#[cfg(test)]
mod checks {
    use std::path::PathBuf;

    use super::{Location, LocationLog, snippet};

    fn place(path: &str, line: usize) -> Location {
        Location {
            path: PathBuf::from(path),
            line,
            text: format!("line {line}"),
        }
    }

    #[test]
    fn the_latest_place_comes_first_and_a_line_is_listed_once() {
        let mut log = LocationLog::default();
        log.push(place("a", 1));
        log.push(place("b", 2));
        log.push(place("a", 1));
        let lines: Vec<_> = log
            .places()
            .iter()
            .map(|place| (place.path.to_str().unwrap(), place.line))
            .collect();
        assert_eq!(lines, [("a", 1), ("b", 2)]);
    }

    #[test]
    fn the_log_keeps_the_latest_fifty() {
        let mut log = LocationLog::default();
        for line in 0..80 {
            log.push(place("a", line));
        }
        assert_eq!(log.places().len(), 50);
        assert_eq!(log.places()[0].line, 79);
    }

    #[test]
    fn a_long_line_is_cut() {
        assert_eq!(snippet("   short  "), "short");
        assert_eq!(snippet(&"x".repeat(200)).chars().count(), 91);
    }
}
