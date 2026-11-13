//! Ctrl+Shift+E and Shift+F11: the places where the user was working and the bookmarks, as
//! one list with the line each place points at.

use std::path::{Path, PathBuf};

use editor_view::{EditorView, Spot};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::Workspace;

use crate::navigation;
use crate::outline::OpenEditors;

const WIDTH: f32 = 640.;
const HEIGHT: f32 = 420.;
/// Places the log keeps.
const LOG_MAX: usize = 50;
/// Characters of the line shown for a place.
const SNIPPET: usize = 90;

/// A line of a file the user was at, with the text it had then.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Location {
    pub path: PathBuf,
    pub line: usize,
    pub text: String,
}

/// The places of edits and of jumps, the latest first.
#[derive(Default)]
pub struct LocationLog(Vec<Location>);

impl Global for LocationLog {}

impl LocationLog {
    /// A place on a line that is already in the log moves to the front.
    pub fn push(&mut self, place: Location) {
        self.0
            .retain(|known| !(known.path == place.path && known.line == place.line));
        self.0.insert(0, place);
        self.0.truncate(LOG_MAX);
    }

    pub fn places(&self) -> &[Location] {
        &self.0
    }
}

/// Remembers where the caret of the editor is.
pub fn remember(view: &Entity<EditorView>, cx: &mut App) {
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
        cx.default_global::<LocationLog>().push(place);
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

fn line_of(path: &Path, line: usize, cx: &App) -> String {
    OpenEditors::get(cx, path)
        .and_then(|editor| editor.read(cx).line_text(line))
        .unwrap_or_default()
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map_or(String::new(), |name| name.to_string_lossy().into_owned())
}

pub fn show_recent(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let rows = cx
        .default_global::<LocationLog>()
        .places()
        .iter()
        .map(|place| Row {
            badge: String::new(),
            path: place.path.clone(),
            line: place.line,
            text: place.text.clone(),
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
                text: line_of(&entry.path, line, cx),
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
    // Recent Locations has no popup that could show an empty list, so it keeps its
    // notification. Bookmarks have a popup of their own, and its empty state is part of it.
    if rows.is_empty() && kind == Kind::Recent {
        workspace.notify_user("Recent Locations", "No places yet", cx);
        return;
    }
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
        let left = (viewport.width - px(WIDTH)) / 2.;
        let top = viewport.height * 0.205;
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
        let body = surface(&theme)
            .id("places-popup")
            .debug_selector(|| "places-popup".to_owned())
            .relative()
            .child(script_spot(scripted, "places-popup"))
            .track_focus(&self.focus)
            .occlude()
            .w(px(WIDTH))
            .max_h(px(HEIGHT))
            .p(px(POPUP_PADDING))
            .flex()
            .flex_col()
            .on_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)))
            .on_mouse_down_out(cx.listener(|this, _, window, cx| this.dismiss(window, cx)))
            .child(
                div()
                    .px(px(ROW_PADDING_X))
                    .h(px(CONTROL_HEIGHT))
                    .flex()
                    .items_center()
                    .text_color(theme.text_secondary)
                    .child(title),
            )
            .child(if rows.is_empty() {
                // The bookmarks have a popup of their own even with nothing in it: the
                // list says so in place of its rows, without a notification.
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
                        Kind::Recent => "No places yet",
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
        div()
            .absolute()
            .left(left)
            .top(top)
            .child(enter("places-enter", body))
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
