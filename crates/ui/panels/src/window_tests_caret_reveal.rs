//! The caret at the bottom edge of the editor: its line stays above the breadcrumbs, under
//! the find bar and the panel of pinned declarations, and the rename popup of the caret
//! opens above the line when the room under it runs out.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ui_kit::gpui;
use ui_kit::gpui::{Bounds, Pixels, TestAppContext, VisualTestContext, WindowHandle};
use ui_kit::px;
use workspace::Workspace;

use crate::window_tests::{draw, editor_of, open_path, press, sample_project, type_text};
use crate::window_tests_generate_list as generate;
use crate::window_tests_lang::{FakeLanguage, open_window_with, project};
use crate::window_tests_ui::{open_test_window, wait_for_outline};

fn open(cx: &mut TestAppContext, root: PathBuf) -> WindowHandle<Workspace> {
    let window = open_test_window(cx, root);
    cx.run_until_parked();
    draw(cx, window);
    window
}

fn write_file(root: &Path, name: &str, text: &str) -> PathBuf {
    let path = root.join("src").join(name);
    fs::write(&path, text).unwrap();
    path
}

/// A class with one method and `body` lines in it: the breadcrumbs name the method.
fn class_with_body(root: &Path, body: usize) -> PathBuf {
    let mut text = String::from("class Long\n    method walk\n");
    for line in 0..body {
        text.push_str(&format!("        body {line}\n"));
    }
    write_file(root, "Long.txt", &text)
}

/// Sixty plain lines, then a class with a method of 120 body lines: the top of the view stays
/// outside every declaration while the caret is at the bottom.
fn plain_then_class(root: &Path) -> PathBuf {
    let mut text = String::new();
    for line in 0..60 {
        text.push_str(&format!("note {line}\n"));
    }
    text.push_str("class Long\n    method walk\n");
    for line in 0..120 {
        text.push_str(&format!("        body {line}\n"));
    }
    write_file(root, "Long.txt", &text)
}

/// The arrow key down, with a frame after each key as the window draws one.
fn walk_down(cx: &mut TestAppContext, window: WindowHandle<Workspace>, steps: usize) {
    for _ in 0..steps {
        press(cx, window, "down");
        draw(cx, window);
    }
}

/// The rectangle of the element with the debug selector, when it is on screen.
fn shown_rect(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &'static str,
) -> Option<Bounds<Pixels>> {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector)
}

/// The line of the caret as the last frame drew it.
fn caret_line(cx: &TestAppContext, view: &gpui::Entity<editor_view::EditorView>) -> Bounds<Pixels> {
    view.read_with(cx, |view, _| view.caret_line_bounds())
        .expect("the frame drew the row of the caret")
}

/// The area of the text as the last frame laid it out.
fn text_area(cx: &TestAppContext, view: &gpui::Entity<editor_view::EditorView>) -> Bounds<Pixels> {
    view.read_with(cx, |view, _| view.editor_bounds())
        .expect("the frame laid out the text")
}

#[ui_kit::gpui::test]
fn the_caret_row_at_the_bottom_edge_is_fully_above_the_breadcrumbs(cx: &mut TestAppContext) {
    let root = sample_project("caret-reveal-crumbs");
    let path = plain_then_class(&root);
    let window = open(cx, root.clone());
    open_path(cx, window, &path);
    wait_for_outline(cx, window, &path);
    let view = editor_of(cx, window, &path);
    walk_down(cx, window, 75);
    let line = caret_line(cx, &view);
    let text = text_area(cx, &view);
    let crumbs = shown_rect(cx, window, "editor-crumbs").expect("the caret is in a declaration");
    assert!(
        text.bottom() <= crumbs.top(),
        "the text {text:?} ends above the breadcrumbs {crumbs:?}"
    );
    assert!(
        line.bottom() <= text.bottom(),
        "the caret row {line:?} is above the edge {text:?}"
    );
}

#[ui_kit::gpui::test]
fn the_caret_row_stays_above_the_breadcrumbs_with_the_find_bar_open(cx: &mut TestAppContext) {
    let root = sample_project("caret-reveal-find");
    let path = plain_then_class(&root);
    let window = open(cx, root.clone());
    open_path(cx, window, &path);
    wait_for_outline(cx, window, &path);
    let view = editor_of(cx, window, &path);
    press(cx, window, "ctrl-f");
    draw(cx, window);
    view.update(cx, |view, cx| view.restore_caret(75, 0, cx));
    draw(cx, window);
    let line = caret_line(cx, &view);
    let text = text_area(cx, &view);
    let crumbs = shown_rect(cx, window, "editor-crumbs").expect("the caret is in a declaration");
    assert!(
        line.top() >= text.top() && line.bottom() <= text.bottom(),
        "the caret row {line:?} is inside the text {text:?} under the find bar"
    );
    assert!(
        text.bottom() <= crumbs.top(),
        "the text {text:?} ends above the breadcrumbs {crumbs:?}"
    );
}

#[ui_kit::gpui::test]
fn a_caret_under_pinned_declarations_is_shown_above_the_breadcrumbs(cx: &mut TestAppContext) {
    let root = sample_project("caret-reveal-pinned");
    let path = class_with_body(&root, 120);
    let window = open(cx, root.clone());
    open_path(cx, window, &path);
    wait_for_outline(cx, window, &path);
    let view = editor_of(cx, window, &path);
    walk_down(cx, window, 75);
    let panel = view.read_with(cx, |view, _| view.sticky_panel_height());
    assert!(panel > 0., "the class and the method are pinned at the top");
    let line = caret_line(cx, &view);
    let text = text_area(cx, &view);
    let crumbs = shown_rect(cx, window, "editor-crumbs").expect("the caret is in a declaration");
    assert!(
        line.top() >= text.top() + px(panel),
        "the caret row {line:?} is under the pinned panel of {panel}px"
    );
    assert!(
        line.bottom() <= text.bottom(),
        "the caret row {line:?} is above the edge {text:?}"
    );
    assert!(
        text.bottom() <= crumbs.top(),
        "the text {text:?} ends above the breadcrumbs {crumbs:?}"
    );
}

#[ui_kit::gpui::test]
fn a_code_lens_that_arrives_above_the_caret_keeps_the_caret_shown(cx: &mut TestAppContext) {
    let mut text = String::new();
    for line in 0..100 {
        if line == 80 {
            text.push_str("void m() {}\n");
        } else {
            text.push_str(&format!("body {line}\n"));
        }
    }
    let root = project("caret-reveal-lens", &[("src/Main.txt", text.as_str())]);
    let main = root.join("src").join("Main.txt");
    let language = FakeLanguage::new();
    let window = open_window_with(cx, root.clone(), language.clone());
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &main);
    let view = editor_of(cx, window, &main);
    walk_down(cx, window, 80);
    // An edit at the caret asks for the lenses after a pause, so the answer comes after the
    // reveal and puts a row above the caret.
    type_text(cx, window, "x");
    press(cx, window, "backspace");
    draw(cx, window);
    language.set_code_lens(vec![(
        80,
        "2 usages".to_owned(),
        "show.references".to_owned(),
    )]);
    cx.executor().advance_clock(Duration::from_millis(1000));
    cx.run_until_parked();
    draw(cx, window);
    let labels = view.read_with(cx, |view, _| view.code_vision_texts());
    assert!(!labels.is_empty(), "the lens of the declaration arrived");
    let line = caret_line(cx, &view);
    let area = text_area(cx, &view);
    assert!(
        line.bottom() <= area.bottom(),
        "the caret row {line:?} is above the edge {area:?}"
    );
}

#[ui_kit::gpui::test]
fn the_rename_popup_opens_above_the_caret_line_at_the_bottom_edge(cx: &mut TestAppContext) {
    let mut text = String::new();
    for line in 0..120 {
        text.push_str(&format!("body {line}\n"));
    }
    let root = project("caret-reveal-rename", &[("src/Main.txt", text.as_str())]);
    let main = root.join("src").join("Main.txt");
    let window = open_window_with(cx, root.clone(), FakeLanguage::new());
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &main);
    let view = editor_of(cx, window, &main);

    view.update(cx, |view, cx| view.restore_caret(75, 0, cx));
    draw(cx, window);
    press(cx, window, "shift-f6");
    draw(cx, window);
    let popup = shown_rect(cx, window, "rename-popup").expect("Shift+F6 asks for the new name");
    let line = caret_line(cx, &view);
    assert!(
        popup.bottom() <= line.top(),
        "the popup {popup:?} stands above the caret row {line:?}"
    );

    press(cx, window, "escape");
    view.update(cx, |view, cx| view.restore_caret(20, 0, cx));
    draw(cx, window);
    press(cx, window, "shift-f6");
    draw(cx, window);
    let popup = shown_rect(cx, window, "rename-popup").expect("Shift+F6 asks for the new name");
    let line = caret_line(cx, &view);
    assert!(
        popup.top() >= line.bottom(),
        "the popup {popup:?} stands under the caret row {line:?} with room below"
    );
}

#[ui_kit::gpui::test]
fn the_completion_list_opens_above_the_caret_line_at_the_bottom_edge(cx: &mut TestAppContext) {
    let mut text = String::new();
    for line in 0..120 {
        text.push_str(&format!("body {line}\n"));
    }
    let root = project(
        "caret-reveal-completion",
        &[("src/Main.txt", text.as_str())],
    );
    let main = root.join("src").join("Main.txt");
    let window = open_window_with(cx, root.clone(), FakeLanguage::new());
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &main);
    let view = editor_of(cx, window, &main);

    view.update(cx, |view, cx| view.restore_caret(75, 0, cx));
    draw(cx, window);
    press(cx, window, "ctrl-space");
    draw(cx, window);
    let list = shown_rect(cx, window, "completion-popup").expect("Ctrl+Space lists the entries");
    let line = caret_line(cx, &view);
    assert!(
        list.bottom() <= line.top(),
        "the list {list:?} stands above the caret row {line:?}"
    );

    press(cx, window, "escape");
    view.update(cx, |view, cx| view.restore_caret(20, 0, cx));
    draw(cx, window);
    press(cx, window, "ctrl-space");
    draw(cx, window);
    let list = shown_rect(cx, window, "completion-popup").expect("Ctrl+Space lists the entries");
    let line = caret_line(cx, &view);
    assert!(
        list.top() >= line.bottom(),
        "the list {list:?} stands under the caret row {line:?} with room below"
    );
}

#[ui_kit::gpui::test]
fn the_generate_list_has_its_heading_above_the_rows(cx: &mut TestAppContext) {
    let (root, path) = generate::project("caret-reveal-generate", "class A {\n}\n");
    let window = generate::open_window(cx, root, generate::GenerateLanguage::new());
    cx.run_until_parked();
    draw(cx, window);
    let rows = generate::alt_insert(cx, window, &path, 0, 9).expect("the list opens");
    draw(cx, window);
    assert!(!rows.is_empty(), "the class offers rows");
    let heading =
        shown_rect(cx, window, "actions-title").expect("the Generate list has its heading");
    let first = shown_rect(cx, window, "actions-intention-0").expect("the first row is on screen");
    assert!(
        heading.bottom() <= first.top(),
        "the heading {heading:?} stands above the first row {first:?}"
    );
}
