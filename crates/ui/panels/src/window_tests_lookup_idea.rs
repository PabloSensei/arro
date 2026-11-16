//! The completion list and the jump as IDEA gives them: a row under the pointer, a list
//! longer than its box that scrolls, and Ctrl+B that moves inside the tab of the file
//! that is open already instead of opening another one. The language here is a fake
//! server of the tests; no process runs.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use editor_view::Spot;
use lang_api::{CompletionEntry, SourceSpan, TextPoint, TextSpan};
use ui_kit::gpui::{self, MouseButton, MouseDownEvent, MouseUpEvent, ScrollDelta, ScrollEvent};
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use ui_kit::{Modifiers, point};
use workspace::Workspace;

use crate::window_tests::{
    active_path, bounds_of, buffer_of, click, draw, editor_of, open_path, press,
};
use crate::window_tests_lang::{project, uri_of};
use crate::window_tests_speed::{SpeedLanguage, open_window};

/// The rows of the list on screen: the number the editor shows at once.
const SHOWN: usize = 12;

/// A list of `count` entries, each one a plain name in the order the server sends them.
fn entries(count: usize) -> Vec<CompletionEntry> {
    (0..count)
        .map(|index| {
            let label = format!("item{index:02}");
            CompletionEntry {
                label: label.clone(),
                insert_text: Some(label),
                ..CompletionEntry::default()
            }
        })
        .collect()
}

fn span_at(path: &Path, line: u32, character: u32) -> SourceSpan {
    SourceSpan {
        uri: uri_of(path),
        span: TextSpan {
            start: TextPoint { line, character },
            end: TextPoint { line, character },
        },
    }
}

/// Opens the editor of `name` and puts the caret on `line`, `column`.
fn open_file(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    root: &Path,
    name: &str,
    line: usize,
    column: usize,
) -> gpui::Entity<editor_view::EditorView> {
    let path = root.join(name);
    open_path(cx, window, &path);
    focus_editor(cx, window);
    let view = editor_of(cx, window, &path);
    view.update(cx, |view, cx| view.go_to(Spot::Line { line, column }, cx));
    cx.run_until_parked();
    draw(cx, window);
    view
}

/// Puts the keyboard back into the editor of the active tab.
fn focus_editor(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    window
        .update(cx, |workspace, window, cx| {
            workspace.focus_editor(window, cx)
        })
        .unwrap();
    cx.run_until_parked();
}

/// The rows of the open list on screen and the label of the selected row.
fn visible(
    cx: &TestAppContext,
    view: &gpui::Entity<editor_view::EditorView>,
    step: &str,
) -> (Vec<String>, Option<String>) {
    view.read_with(cx, |view, _| view.completion_visible())
        .unwrap_or_else(|| panic!("the list is open: {step}"))
}

/// The tabs of the panes of the window, as the file tabs list them.
fn open_tabs(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
) -> Vec<Vec<std::path::PathBuf>> {
    window
        .read_with(cx, |workspace, _| workspace.open_tabs())
        .unwrap()
}

fn labels(rows: &[String]) -> Vec<&str> {
    rows.iter().map(String::as_str).collect()
}

/// Two clicks with the counts the platform gives a double click.
fn double_click(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) {
    let position = bounds_of(cx, window, selector).center();
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    for clicks in 1..=2 {
        visual.simulate_event(MouseDownEvent {
            button: MouseButton::Left,
            position,
            modifiers: Modifiers::default(),
            click_count: clicks,
            first_mouse: false,
        });
        visual.simulate_event(MouseUpEvent {
            button: MouseButton::Left,
            position,
            modifiers: Modifiers::default(),
            click_count: clicks,
        });
    }
    cx.run_until_parked();
    draw(cx, window);
}

/// Turns the wheel over `selector` by `rows`, positive ones showing the rows below.
fn wheel(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &'static str,
    rows: f32,
) {
    let position = bounds_of(cx, window, selector).center();
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_event(ScrollEvent {
        position,
        delta: ScrollDelta::Lines(point(0., -rows)),
        modifiers: Modifiers::default(),
        ..ScrollEvent::default()
    });
    cx.run_until_parked();
    draw(cx, window);
}

#[ui_kit::gpui::test]
fn a_click_selects_a_row_and_a_double_click_inserts_it(cx: &mut TestAppContext) {
    let root = project("lookup-idea-mouse", &[("src/A.txt", "\n")]);
    let language = SpeedLanguage::new();
    language.set_entries(entries(5));
    let window = open_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    let view = open_file(cx, window, &root, "src/A.txt", 0, 0);

    press(cx, window, "ctrl-space");
    draw(cx, window);
    let (rows, selected) = visible(cx, &view, "open");
    assert_eq!(
        labels(&rows),
        ["item00", "item01", "item02", "item03", "item04"]
    );
    assert_eq!(
        selected.as_deref(),
        Some("item00"),
        "the first row is picked"
    );

    click(cx, window, "completion-row-2");
    draw(cx, window);
    let (_, selected) = visible(cx, &view, "first click");
    assert_eq!(
        selected.as_deref(),
        Some("item02"),
        "a click picks the row under the pointer"
    );
    assert_eq!(buffer_of(cx, &view), "\n", "a click alone inserts nothing");

    double_click(cx, window, "completion-row-3");
    draw(cx, window);
    assert_eq!(
        buffer_of(cx, &view),
        "item03\n",
        "a double click puts the entry in"
    );
}

#[ui_kit::gpui::test]
fn the_list_scrolls_with_the_wheel_and_with_the_page_keys(cx: &mut TestAppContext) {
    let root = project("lookup-idea-scroll", &[("src/A.txt", "\n")]);
    let language = SpeedLanguage::new();
    language.set_entries(entries(30));
    let window = open_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    let view = open_file(cx, window, root.as_path(), "src/A.txt", 0, 0);

    press(cx, window, "ctrl-space");
    draw(cx, window);
    let (rows, selected) = visible(cx, &view, "first rows");
    assert_eq!(
        labels(&rows),
        [
            "item00", "item01", "item02", "item03", "item04", "item05", "item06", "item07",
            "item08", "item09", "item10", "item11"
        ],
        "the list shows its first rows and no more than a box holds"
    );
    assert_eq!(selected.as_deref(), Some("item00"));

    // The wheel moves the rows under the selection, which stays where it was.
    wheel(cx, window, "completion-popup", 3.);
    let (rows, selected) = visible(cx, &view, "wheel");
    assert_eq!(
        rows.first().map(String::as_str),
        Some("item03"),
        "the wheel shows the rows below"
    );
    assert_eq!(rows.len(), SHOWN);
    assert_eq!(
        selected.as_deref(),
        Some("item00"),
        "a scroll keeps the selected row"
    );

    press(cx, window, "pagedown");
    draw(cx, window);
    let (rows, selected) = visible(cx, &view, "page down");
    assert_eq!(
        selected.as_deref(),
        Some("item12"),
        "PgDn moves the selection a page down"
    );
    assert!(
        rows.iter().any(|row| row == "item12"),
        "the selected row is on screen: {rows:?}"
    );

    press(cx, window, "pageup");
    draw(cx, window);
    let (_, selected) = visible(cx, &view, "page up");
    assert_eq!(selected.as_deref(), Some("item00"));

    press(cx, window, "end");
    draw(cx, window);
    let (rows, selected) = visible(cx, &view, "end");
    assert_eq!(
        selected.as_deref(),
        Some("item29"),
        "End goes to the last row"
    );
    assert_eq!(
        rows.last().map(String::as_str),
        Some("item29"),
        "the last row is on screen"
    );

    press(cx, window, "home");
    draw(cx, window);
    let (rows, selected) = visible(cx, &view, "home");
    assert_eq!(
        selected.as_deref(),
        Some("item00"),
        "Home goes to the first row"
    );
    assert_eq!(rows.first().map(String::as_str), Some("item00"));
}

#[ui_kit::gpui::test]
fn ctrl_b_inside_the_file_moves_the_caret_and_keeps_the_tab(cx: &mut TestAppContext) {
    let root = project(
        "lookup-idea-jump-inside",
        &[("src/A.txt", "shape\nshape\n")],
    );
    let a = root.join("src/A.txt");
    let language = SpeedLanguage::new();
    language.set_navigation(vec![span_at(&a, 1, 0)]);
    let window = open_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    let view = open_file(cx, window, &root, "src/A.txt", 0, 0);
    let tabs = open_tabs(cx, window);

    press(cx, window, "ctrl-b");
    draw(cx, window);
    assert_eq!(active_path(cx, window), Some(a.clone()));
    assert_eq!(
        view.read_with(cx, |view, _| view.caret()),
        (1, 0),
        "the caret goes to the declaration in the same text"
    );
    assert_eq!(
        open_tabs(cx, window),
        tabs,
        "a place of the same file opens no tab"
    );

    press(cx, window, "ctrl-alt-left");
    draw(cx, window);
    assert_eq!(
        view.read_with(cx, |view, _| view.caret()),
        (0, 0),
        "Back returns to the place the jump left"
    );
}

#[ui_kit::gpui::test]
fn ctrl_b_to_an_open_file_shows_its_tab_and_back_returns(cx: &mut TestAppContext) {
    let root = project(
        "lookup-idea-jump-open",
        &[
            ("src/Main.txt", "greet\n"),
            ("src/Other.txt", "other()\ngreet\n"),
        ],
    );
    let main = root.join("src/Main.txt");
    let other = root.join("src/Other.txt");
    let language = SpeedLanguage::new();
    language.set_navigation(vec![span_at(&other, 1, 0)]);
    let window = open_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &main);
    open_path(cx, window, &other);
    open_path(cx, window, &main);
    let view = editor_of(cx, window, &main);
    view.update(cx, |view, cx| {
        view.go_to(Spot::Line { line: 0, column: 2 }, cx)
    });
    cx.run_until_parked();
    draw(cx, window);
    let tabs = open_tabs(cx, window);

    press(cx, window, "ctrl-b");
    draw(cx, window);
    assert_eq!(active_path(cx, window), Some(other.clone()));
    assert_eq!(
        open_tabs(cx, window),
        tabs,
        "a file that is open already keeps its own tab"
    );
    let target = editor_of(cx, window, &other);
    assert_eq!(target.read_with(cx, |view, _| view.caret()), (1, 0));

    press(cx, window, "ctrl-alt-left");
    draw(cx, window);
    assert_eq!(active_path(cx, window), Some(main.clone()));
    assert_eq!(
        view.read_with(cx, |view, _| view.caret()),
        (0, 2),
        "Back returns to the caret the jump left"
    );
}

#[ui_kit::gpui::test]
fn a_jump_without_a_place_shows_a_message_that_goes_away_by_itself(cx: &mut TestAppContext) {
    let root = project("lookup-idea-note", &[("src/A.txt", "shape\n")]);
    let language = SpeedLanguage::new();
    language.set_ready(false);
    let window = open_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    let view = open_file(cx, window, &root, "src/A.txt", 0, 0);
    cx.update(|cx| {
        cx.set_global(editor_view::SymbolLookup(Some(Arc::new(|_: &str| None))));
    });

    press(cx, window, "ctrl-b");
    draw(cx, window);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    assert!(
        visual.debug_bounds("lsp-note").is_some(),
        "a jump without a place says so under the caret"
    );

    cx.executor().advance_clock(Duration::from_millis(3000));
    cx.run_until_parked();
    draw(cx, window);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    assert!(
        visual.debug_bounds("lsp-note").is_none(),
        "the message goes away without a key"
    );
    assert_eq!(buffer_of(cx, &view), "shape\n", "the jump changed no text");
}
