//! The small things of the editor, checked on the window without a window: the panel of
//! sticky lines hides the row under it without cutting it in half, the steps of a script
//! reach the text of a tab that is still loading, and the caret and the selection stay
//! clear of the rows a language server adds above declarations.

use std::time::Duration;

use editor_view::{EditorView, OutlineItem, OutlineKind, StripeKind};
use lang_api::{DiagnosticEntry, InlayHintKind, Severity, TextPoint, TextSpan};
use ui_kit::gpui::{
    self, MouseButton, Pixels, Point, ScrollDelta, ScrollEvent, TestAppContext, VisualTestContext,
    WindowHandle,
};
use ui_kit::{Modifiers, point, px};
use workspace::Workspace;

use crate::window_tests::{
    bounds_of, buffer_of, draw, editor_of, line_height, open_path, open_window, press,
    wait_for_signal,
};
use crate::window_tests_lang::{FakeLanguage, open_window_with, project};

/// The answer of the fake language and the delayed question of the editor both need a turn
/// of the clock.
fn settle(cx: &mut TestAppContext) {
    cx.executor().advance_clock(Duration::from_millis(300));
    cx.run_until_parked();
}

/// Runs the steps of a script and lets the window work them off.
fn run_script(cx: &mut TestAppContext, window: WindowHandle<Workspace>, script: &str) {
    window
        .update(cx, |_, window, cx| crate::script::start(script, window, cx))
        .unwrap();
    for _ in 0..12 {
        cx.executor().advance_clock(Duration::from_millis(50));
        cx.run_until_parked();
    }
    draw(cx, window);
}

/// The rows of the last frame, from the top of the editor.
fn rows(cx: &TestAppContext, view: &gpui::Entity<EditorView>) -> Vec<editor_view::RowPaint> {
    view.read_with(cx, |view, _| view.row_paint())
}

/// The row that shows the text of a buffer line, without a block row of the same line.
fn text_row(rows: &[editor_view::RowPaint], line: usize) -> editor_view::RowPaint {
    *rows
        .iter()
        .find(|row| !row.block && row.line == line)
        .unwrap_or_else(|| panic!("no text row for line {line}: {rows:?}"))
}

/// A point of the editor inside the row of a buffer line, `column` characters in.
fn point_in_line(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    view: &gpui::Entity<EditorView>,
    line: usize,
    column: f32,
) -> Point<Pixels> {
    let bounds = bounds_of(cx, window, "editor-body-0");
    let height = line_height(cx);
    let row = text_row(&rows(cx, view), line);
    point(
        bounds.left() + px(67. + column * 8.),
        bounds.top() + px(row.top + height / 2.),
    )
}

/// A point of the editor inside a block row above a buffer line.
fn point_in_block(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    view: &gpui::Entity<EditorView>,
    line: usize,
) -> Point<Pixels> {
    let bounds = bounds_of(cx, window, "editor-body-0");
    let height = line_height(cx);
    let row = *rows(cx, view)
        .iter()
        .find(|row| row.block && row.line == line)
        .unwrap_or_else(|| panic!("no block row above line {line}"));
    point(
        bounds.right() - px(40.),
        bounds.top() + px(row.top + height / 2.),
    )
}

fn click_at(cx: &mut TestAppContext, window: WindowHandle<Workspace>, position: Point<Pixels>) {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_click(position, Modifiers::default());
    cx.run_until_parked();
    draw(cx, window);
}

fn drag(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    from: Point<Pixels>,
    to: Point<Pixels>,
) {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_move(from, None, Modifiers::default());
    visual.simulate_mouse_down(from, MouseButton::Left, Modifiers::default());
    for step in 1..=8 {
        let fraction = step as f32 / 8.;
        visual.simulate_mouse_move(
            point(
                from.x + (to.x - from.x) * fraction,
                from.y + (to.y - from.y) * fraction,
            ),
            Some(MouseButton::Left),
            Modifiers::default(),
        );
    }
    visual.simulate_mouse_up(to, MouseButton::Left, Modifiers::default());
    cx.run_until_parked();
    draw(cx, window);
}

/// Turns the wheel over the editor by `pixels` of a line height at a time.
fn wheel(cx: &mut TestAppContext, window: WindowHandle<Workspace>, pixels: f32) {
    let bounds = bounds_of(cx, window, "editor-body-0");
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_event(ScrollEvent {
        position: bounds.center(),
        delta: ScrollDelta::Pixels(point(px(0.), px(-pixels))),
        modifiers: Modifiers::default(),
        ..ScrollEvent::default()
    });
    cx.run_until_parked();
    draw(cx, window);
}

/// Every drawn row is either wholly under the panel of sticky lines or wholly below it.
fn assert_no_cut_rows(cx: &TestAppContext, view: &gpui::Entity<EditorView>, height: f32) -> f32 {
    let panel = view.read_with(cx, |view, _| view.sticky_panel_height());
    assert!(panel > 0., "the sticky panel is on");
    for row in rows(cx, view) {
        let bottom = row.top + height;
        assert!(
            bottom <= panel + 0.5 || row.top >= panel - 0.5,
            "row {row:?} is cut by the panel of height {panel}"
        );
    }
    panel
}

/// The offset of each line of `text`.
fn line_starts(text: &str) -> Vec<usize> {
    let mut starts = vec![0];
    for (at, byte) in text.bytes().enumerate() {
        if byte == b'\n' && at + 1 < text.len() {
            starts.push(at + 1);
        }
    }
    starts
}

/// A declaration of the whole text, for the sticky lines of the editor.
fn outline_item(
    text: &str,
    starts: &[usize],
    line: usize,
    end: usize,
    kind: OutlineKind,
    name: &str,
) -> OutlineItem {
    let at = starts[line] + text[starts[line]..].find(name).unwrap();
    OutlineItem {
        name: name.to_owned(),
        signature: None,
        kind,
        depth: 0,
        range: starts[line]..starts[end],
        name_range: at..at + name.len(),
    }
}

fn diagnostic(line: u32) -> DiagnosticEntry {
    DiagnosticEntry {
        span: TextSpan {
            start: TextPoint { line, character: 0 },
            end: TextPoint { line, character: 4 },
        },
        severity: Severity::Error,
        message: "the error".to_owned(),
        unnecessary: false,
    }
}

#[ui_kit::gpui::test]
fn a_script_goes_to_a_line_then_end_then_types_the_end_of_it(cx: &mut TestAppContext) {
    let root = project(
        "script-end",
        &[("src/Main.txt", "first\nsecond\nthird\nfourth\n")],
    );
    let path = root.join("src").join("Main.txt");
    let language = FakeLanguage::new();
    let window = open_window_with(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);

    // The tab shows its text after the first steps: End and the typed letter must still
    // land at the end of the third line.
    run_script(cx, window, "open:src/Main.txt;goto:3;key:end;type:X");

    let view = editor_of(cx, window, &path);
    assert_eq!(
        buffer_of(cx, &view),
        "first\nsecond\nthirdX\nfourth\n",
        "End puts the caret after the text of the third line"
    );
}

#[ui_kit::gpui::test]
fn wait_ready_holds_the_script_until_the_language_is_ready(cx: &mut TestAppContext) {
    let root = project("script-ready", &[("src/Main.txt", "value\n")]);
    let path = root.join("src").join("Main.txt");
    let language = FakeLanguage::new();
    let window = open_window_with(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);

    window
        .update(cx, |workspace, _, cx| {
            workspace.set_language_status(Some("Language server importing".into()), cx)
        })
        .unwrap();
    window
        .update(cx, |_, window, cx| {
            crate::script::start("wait-ready;type:Y", window, cx)
        })
        .unwrap();
    cx.executor().advance_clock(Duration::from_millis(200));
    cx.run_until_parked();
    assert_eq!(
        buffer_of(cx, &view),
        "value\n",
        "nothing is typed while the server imports"
    );

    window
        .update(cx, |workspace, _, cx| {
            workspace.set_language_status(Some("Language server ready".into()), cx)
        })
        .unwrap();
    for _ in 0..12 {
        cx.executor().advance_clock(Duration::from_millis(50));
        cx.run_until_parked();
    }
    draw(cx, window);
    assert_eq!(
        buffer_of(cx, &view),
        "Yvalue\n",
        "the letter comes once the server is ready"
    );
}

#[ui_kit::gpui::test]
fn the_sticky_panel_covers_whole_rows_and_never_cuts_one(cx: &mut TestAppContext) {
    let mut text = String::from("class Long\n    method walk\n");
    for line in 0..120 {
        text.push_str(&format!("        body {line}\n"));
    }
    let root = project("sticky-panel", &[("src/Long.txt", &text)]);
    let path = root.join("src").join("Long.txt");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    let starts = line_starts(&text);
    let last = starts.len() - 1;
    let class = outline_item(&text, &starts, 0, last, OutlineKind::Class, "Long");
    let method = outline_item(&text, &starts, 1, last, OutlineKind::Method, "walk");
    view.update(cx, |view, cx| view.set_outline(vec![class, method], cx));

    let height = line_height(cx);
    view.update(cx, |view, cx| view.restore_caret(80, 0, cx));
    draw(cx, window);
    assert_eq!(
        view.read_with(cx, |view, _| view.sticky_lines().to_vec()),
        [0, 1],
        "the class and the method stay on top"
    );
    let panel = assert_no_cut_rows(cx, &view, height);
    assert!(
        panel > 2. * height,
        "the panel has the two rows and the gap under them: {panel}"
    );
    let number = text_row(&rows(cx, &view), 81).number;
    assert_eq!(number, Some(82), "the gutter numbers the text rows");

    // A wheel turn in a fraction of a line leaves no half row under the panel.
    wheel(cx, window, height * 1.5);
    assert_no_cut_rows(cx, &view, height);

    // Dragging the thumb of the vertical bar does the same.
    let thumb = view
        .read_with(cx, |view, _| view.vertical_thumb())
        .expect("the file is longer than the editor");
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_down(thumb.center(), MouseButton::Left, Modifiers::default());
    visual.simulate_mouse_move(
        point(thumb.center().x, thumb.center().y + px(160.)),
        Some(MouseButton::Left),
        Modifiers::default(),
    );
    visual.simulate_mouse_up(
        point(thumb.center().x, thumb.center().y + px(160.)),
        MouseButton::Left,
        Modifiers::default(),
    );
    cx.run_until_parked();
    draw(cx, window);
    assert_no_cut_rows(cx, &view, height);

    // A jump inside the same declaration lands on a whole row too.
    view.update(cx, |view, cx| view.go_to_line(100, cx));
    draw(cx, window);
    assert_no_cut_rows(cx, &view, height);
}

#[ui_kit::gpui::test]
fn a_click_on_a_block_row_belongs_to_the_line_under_it(cx: &mut TestAppContext) {
    let text = "one\ntwo\nthree\nfour\n";
    let root = project("block-click", &[("src/Main.txt", text)]);
    let path = root.join("src").join("Main.txt");
    let language = FakeLanguage::new();
    language.set_code_lens(vec![(
        8,
        "2 usages".to_owned(),
        "show.references".to_owned(),
    )]);
    let window = open_window_with(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    settle(cx);
    draw(cx, window);
    assert_eq!(
        view.read_with(cx, |view, _| view.code_vision_texts()),
        [(2, "2 usages".to_owned())]
    );

    view.update(cx, |view, cx| view.restore_caret(0, 0, cx));
    draw(cx, window);
    let position = point_in_block(cx, window, &view, 2);
    click_at(cx, window, position);
    assert_eq!(
        view.read_with(cx, |view, _| view.caret()),
        (2, 0),
        "an empty place of the block row puts the caret on the line under it"
    );
}

#[ui_kit::gpui::test]
fn keys_pass_a_block_row_and_keep_the_column(cx: &mut TestAppContext) {
    let mut text = String::from("one\ntwo\nthree\n");
    for line in 3..80 {
        text.push_str(&format!("line {line}\n"));
    }
    let root = project("block-keys", &[("src/Main.txt", &text)]);
    let path = root.join("src").join("Main.txt");
    let language = FakeLanguage::new();
    language.set_code_lens(vec![(
        8,
        "2 usages".to_owned(),
        "show.references".to_owned(),
    )]);
    let window = open_window_with(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    settle(cx);
    draw(cx, window);

    // Shift+Down from the line above the block reaches the line under it.
    view.update(cx, |view, cx| view.restore_caret(0, 1, cx));
    press(cx, window, "shift-down");
    assert_eq!(
        view.read_with(cx, |view, _| view.caret()),
        (1, 1),
        "the block above the third line changes nothing on the way to the second"
    );
    press(cx, window, "shift-down");
    assert_eq!(
        view.read_with(cx, |view, _| view.caret()),
        (2, 1),
        "one step down from the line above the block reaches the line below it"
    );
    assert!(
        view.read_with(cx, |view, _| view.selected_chars()) > 0,
        "the selection grows over both lines"
    );
    press(cx, window, "shift-up");
    press(cx, window, "shift-up");
    assert_eq!(
        view.read_with(cx, |view, _| view.caret()),
        (0, 1),
        "the same steps up pass the block the same way"
    );

    // Home and End stay on the line of the caret, not on the block above it.
    view.update(cx, |view, cx| view.restore_caret(2, 1, cx));
    press(cx, window, "end");
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (2, 5));
    press(cx, window, "home");
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (2, 0));

    // A page moves by text rows: the block row takes no step of its own.
    let bounds = bounds_of(cx, window, "editor-body-0");
    let page = (f32::from(bounds.size.height) / line_height(cx)).floor() as usize;
    view.update(cx, |view, cx| view.restore_caret(0, 0, cx));
    press(cx, window, "pagedown");
    assert_eq!(
        view.read_with(cx, |view, _| view.caret()),
        (page, 0),
        "PageDown moves by the rows of the text"
    );
    press(cx, window, "pageup");
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (0, 0));
}

#[ui_kit::gpui::test]
fn a_selection_started_above_a_block_row_keeps_the_text_of_the_lines(cx: &mut TestAppContext) {
    let text = "one\ntwo\nthree\nfour\n";
    let root = project("block-drag", &[("src/Main.txt", text)]);
    let path = root.join("src").join("Main.txt");
    let language = FakeLanguage::new();
    language.set_code_lens(vec![(
        8,
        "2 usages".to_owned(),
        "show.references".to_owned(),
    )]);
    let window = open_window_with(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    settle(cx);
    draw(cx, window);

    let from = point_in_line(cx, window, &view, 0, 1.);
    let to = point_in_line(cx, window, &view, 3, 2.);
    drag(cx, window, from, to);
    let (range, _) = view
        .read_with(cx, |view, _| view.selection())
        .expect("the file is read");
    assert_eq!(
        &text[range], "ne\ntwo\nthree\nfo",
        "the drag crosses the block row but selects only the text"
    );
}

#[ui_kit::gpui::test]
fn a_block_row_keeps_the_gutter_numbers_and_the_bar_marks_on_their_lines(cx: &mut TestAppContext) {
    let mut text = String::from("one\ntwo\nthree\n");
    for line in 3..200 {
        text.push_str(&format!("line {line}\n"));
    }
    let root = project("block-gutter", &[("src/Main.txt", &text)]);
    let path = root.join("src").join("Main.txt");
    let language = FakeLanguage::new();
    language.set_code_lens(vec![(
        8,
        "2 usages".to_owned(),
        "show.references".to_owned(),
    )]);
    language.set_diagnostics(vec![diagnostic(3)]);
    let window = open_window_with(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    settle(cx);
    wait_for_signal(cx, "the diagnostics", |cx| {
        !view
            .read_with(cx, |view, _| view.diagnostic_marks())
            .is_empty()
    });
    draw(cx, window);

    let height = line_height(cx);
    let drawn = rows(cx, &view);
    let block = drawn
        .iter()
        .find(|row| row.block && row.line == 2)
        .copied()
        .expect("the label of the server has a row");
    assert_eq!(block.number, None, "a block row shows no number");
    let line = text_row(&drawn, 2);
    assert_eq!(line.number, Some(3), "the number stays on the line itself");
    assert!(
        (line.top - (block.top + height)).abs() < 0.01,
        "the block sits one row above its line"
    );

    // The mark of the error stands on the row of its line, not on the block above.
    let stripe = view
        .read_with(cx, |view, _| view.stripe_bounds(StripeKind::Error, 3))
        .expect("the error has a mark on the bar");
    let bounds = bounds_of(cx, window, "editor-body-0");
    let total = view.read_with(cx, |view, _| view.display_rows()) as f32 * height;
    let row = text_row(&drawn, 3);
    let center = row.top + height / 2.;
    let expected = bounds.top() + px(center / total * f32::from(bounds.size.height));
    assert!(
        (f32::from(stripe.top()) - f32::from(expected)).abs() < 2.5,
        "the mark at {} stands where the row begins {expected}",
        stripe.top()
    );
}

#[ui_kit::gpui::test]
fn soft_wrap_puts_the_block_above_the_first_row_of_its_line(cx: &mut TestAppContext) {
    let long = "word ".repeat(80);
    let text = format!("one\n{long}\nthree\n");
    let starts = line_starts(&text);
    let root = project("block-wrap", &[("src/Main.txt", &text)]);
    let path = root.join("src").join("Main.txt");
    let language = FakeLanguage::new();
    language.set_code_lens(vec![(
        starts[1],
        "2 usages".to_owned(),
        "show.references".to_owned(),
    )]);
    let window = open_window_with(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    settle(cx);
    window
        .update(cx, |workspace, window, cx| {
            workspace.run_command(keymap::editor::TOGGLE_SOFT_WRAP, window, cx)
        })
        .unwrap();
    draw(cx, window);

    let drawn = rows(cx, &view);
    let at = drawn
        .iter()
        .position(|row| row.block && row.line == 1)
        .expect("the block of the second line has a row");
    assert!(
        !drawn[at + 1].block && drawn[at + 1].line == 1,
        "the text of the line follows the block: {drawn:?}"
    );
    assert!(
        (drawn[at + 1].top - (drawn[at].top + line_height(cx))).abs() < 0.01,
        "the block takes one row"
    );

    view.update(cx, |view, cx| view.restore_caret(1, 0, cx));
    press(cx, window, "end");
    let caret = view.read_with(cx, |view, _| view.caret());
    assert_eq!(caret.0, 1, "End stays on the wrapped line");
    assert!(
        caret.1 > 0,
        "End reaches the end of the wrapped row: {caret:?}"
    );
}

#[ui_kit::gpui::test]
fn a_hint_sits_on_the_baseline_of_the_code_and_inside_its_columns(cx: &mut TestAppContext) {
    let root = project("inlay-geometry", &[("src/Call.txt", "call(true, 5);\n")]);
    let path = root.join("src").join("Call.txt");
    let language = FakeLanguage::new();
    language.set_inlays(vec![(5, "flag:".to_owned(), InlayHintKind::Parameter)]);
    let window = open_window_with(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    settle(cx);
    draw(cx, window);

    let hints = view.read_with(cx, |view, _| view.inlay_paint().to_vec());
    let hint = hints.first().expect("the hint is in the text");
    assert!(
        (hint.baseline - hint.code_baseline).abs() < 0.1,
        "the smaller label sits on the baseline of the code: {hint:?}"
    );
    assert!(
        hint.pill.left() >= hint.columns_left - px(0.5)
            && hint.pill.right() <= hint.columns_right + px(0.5),
        "the pill stays inside the columns of the hint: {hint:?}"
    );
}
