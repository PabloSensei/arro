//! Folding, pages, the insert mode, element moves and Context Info of the editor,
//! driven by keys in a window without a real window.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use editor_view::{EditorView, FoldStyle, LanguageConfig, Languages};
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use workspace::Workspace;

use crate::window_tests::{
    bounds_of, draw, editor_of, open_path, open_window, press, sample_project, type_text,
};

const LIMIT: Duration = Duration::from_secs(20);

fn wait_for(
    cx: &mut TestAppContext,
    what: &str,
    mut check: impl FnMut(&mut TestAppContext) -> bool,
) {
    let started = Instant::now();
    loop {
        if check(cx) {
            return;
        }
        assert!(started.elapsed() < LIMIT, "waited for {what} in vain");
        std::thread::sleep(Duration::from_millis(10));
        cx.executor().advance_clock(Duration::from_millis(50));
        cx.run_until_parked();
    }
}

static FOLD_STYLES: &[FoldStyle] = &[FoldStyle {
    capture: "fold",
    placeholder: "\u{2026}",
    keep: 0,
    joined: false,
    collapsed_on_open: false,
}];

/// A text file with the regions and comments the actions read: the grammar only gives
/// the editor a language, the folds of the test come from the region markers.
fn sample_language(path: &Path) -> Option<LanguageConfig> {
    let extension = path.extension()?.to_str()?;
    if !extension.eq_ignore_ascii_case("txt") {
        return None;
    }
    let mut config = dockerfile_syntax::config();
    config.name = "Sample";
    config.folds = "";
    config.fold_styles = FOLD_STYLES;
    config.line_comment = Some("//");
    config.indent_unit = "    ";
    Some(config)
}

fn open_text(
    cx: &mut TestAppContext,
    name: &str,
    text: &str,
) -> (PathBuf, WindowHandle<Workspace>, gpui::Entity<EditorView>) {
    let root = sample_project(name);
    let path = root.join("src").join("main.txt");
    fs::write(&path, text).unwrap();
    let window = open_window(cx, root);
    cx.update(|cx| {
        cx.set_global(Languages {
            resolve: sample_language,
            templates: |_| None,
        });
    });
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    cx.run_until_parked();
    draw(cx, window);
    (path, window, view)
}

fn reset(
    cx: &mut TestAppContext,
    view: &gpui::Entity<EditorView>,
    text: &str,
    line: usize,
    column: usize,
) {
    let length = view
        .read_with(cx, |view, _| view.text())
        .unwrap_or_default()
        .len();
    view.update(cx, |view, cx| {
        view.replace_text(0..length, text, cx);
        view.restore_caret(line, column, cx);
    });
    cx.run_until_parked();
}

fn selection(cx: &TestAppContext, view: &gpui::Entity<EditorView>) -> std::ops::Range<usize> {
    view.read_with(cx, |view, _| view.selection())
        .expect("the file is read")
        .0
}

fn has_popup(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &'static str,
) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

#[ui_kit::gpui::test]
fn a_block_folds_at_the_caret_and_opens_again(cx: &mut TestAppContext) {
    let text = "class A {\n    void f() {\n        x();\n        y();\n    }\n}\n";
    let (_path, window, view) = open_text(cx, "collapse-block", text);
    let rows = view.read_with(cx, |view, _| view.display_rows());
    view.update(cx, |view, cx| view.restore_caret(2, 8, cx));
    cx.run_until_parked();

    press(cx, window, "ctrl-shift-.");
    let folded = view.read_with(cx, |view, _| view.display_rows());
    assert!(folded < rows, "Ctrl+Shift+. folds the method body");

    press(cx, window, "ctrl-=");
    assert_eq!(
        view.read_with(cx, |view, _| view.display_rows()),
        rows,
        "Ctrl+= opens the fold"
    );
}

#[ui_kit::gpui::test]
fn regions_fold_recursively_by_their_headers(cx: &mut TestAppContext) {
    let text =
        "//region Outer\none();\n//region Inner\ntwo();\n//endregion\nthree();\n//endregion\n";
    let (_path, window, view) = open_text(cx, "regions-recursive", text);
    let rows = view.read_with(cx, |view, _| view.display_rows());

    // The fold candidates come from the syntax thread.
    wait_for(cx, "the region folds", |cx| {
        press(cx, window, "ctrl-shift--");
        view.read_with(cx, |view, _| view.display_rows()) < rows
    });
    press(cx, window, "ctrl-shift-=");
    assert_eq!(view.read_with(cx, |view, _| view.display_rows()), rows);

    view.update(cx, |view, cx| view.restore_caret(3, 0, cx));
    cx.run_until_parked();
    press(cx, window, "ctrl-alt--");
    let folded = view.read_with(cx, |view, _| view.display_rows());
    assert!(folded < rows, "Ctrl+Alt+- folds the region recursively");
    press(cx, window, "ctrl-alt-=");
    assert_eq!(
        view.read_with(cx, |view, _| view.display_rows()),
        rows,
        "Ctrl+Alt+= opens the regions again"
    );
}

#[ui_kit::gpui::test]
fn a_level_key_collapses_and_opens_the_regions_down_to_that_level(cx: &mut TestAppContext) {
    let text =
        "//region Outer\none();\n//region Inner\ntwo();\n//endregion\nthree();\n//endregion\n";
    let (_path, window, view) = open_text(cx, "regions-levels", text);
    let rows = view.read_with(cx, |view, _| view.display_rows());

    // The folds of the markers are candidates after the parse.
    wait_for(cx, "the region folds", |cx| {
        press(cx, window, "ctrl-shift--");
        view.read_with(cx, |view, _| view.display_rows()) < rows
    });
    press(cx, window, "ctrl-shift-=");

    view.update(cx, |view, cx| view.restore_caret(1, 0, cx));
    cx.run_until_parked();
    press(cx, window, "ctrl-numpad_multiply 1");
    let level_one = view.read_with(cx, |view, _| view.display_rows());
    assert!(level_one < rows, "level 1 folds the inner region");

    press(cx, window, "ctrl-numpad_multiply 2");
    assert_eq!(
        view.read_with(cx, |view, _| view.display_rows()),
        rows,
        "level 2 opens the inner region"
    );

    press(cx, window, "ctrl-shift-numpad_multiply 1");
    assert!(
        view.read_with(cx, |view, _| view.display_rows()) < rows,
        "Expand All to Level 1 folds the inner region"
    );
    press(cx, window, "ctrl-shift-numpad_multiply 3");
    assert_eq!(
        view.read_with(cx, |view, _| view.display_rows()),
        rows,
        "Expand All to Level 3 opens every region"
    );
}

#[ui_kit::gpui::test]
fn custom_regions_show_their_name_and_goto_lists_them(cx: &mut TestAppContext) {
    let text = "//region First\none();\ntwo();\n//endregion\n\
// <editor-fold desc=\"Second\">\nthree();\nfour();\n// </editor-fold>\n";
    let (_path, window, view) = open_text(cx, "regions-goto", text);
    let rows = view.read_with(cx, |view, _| view.display_rows());

    view.update(cx, |view, cx| view.restore_caret(0, 3, cx));
    cx.run_until_parked();
    press(cx, window, "ctrl--");
    assert!(view.read_with(cx, |view, _| view.display_rows()) < rows);
    press(cx, window, "ctrl-=");

    press(cx, window, "ctrl-alt-.");
    draw(cx, window);
    assert!(
        has_popup(cx, window, "goto-region-popup"),
        "Ctrl+Alt+. lists the regions"
    );
    type_text(cx, window, "Second");
    draw(cx, window);
    press(cx, window, "enter");
    assert_eq!(
        view.read_with(cx, |view, _| view.caret().0),
        4,
        "Enter goes to the header of the picked region"
    );

    press(cx, window, "ctrl-alt-.");
    draw(cx, window);
    assert!(has_popup(cx, window, "goto-region-popup"));
    press(cx, window, "escape");
    draw(cx, window);
    assert!(
        !has_popup(cx, window, "goto-region-popup"),
        "Escape closes the list"
    );
}

#[ui_kit::gpui::test]
fn pages_move_the_caret_and_the_center_key_scrolls(cx: &mut TestAppContext) {
    let text: String = (0..60).map(|line| format!("line{line}\n")).collect();
    let (_path, window, view) = open_text(cx, "pages", &text);
    view.update(cx, |view, cx| view.restore_caret(30, 0, cx));
    cx.run_until_parked();
    draw(cx, window);

    let rows = view.read_with(cx, |view, _| view.display_rows());
    // Scroll far away first, so the center key has work to do.
    view.update(cx, |view, cx| view.restore_caret(30, 0, cx));
    cx.run_until_parked();
    press(cx, window, "ctrl-m");
    assert_eq!(
        view.read_with(cx, |view, _| view.caret().0),
        30,
        "the caret stays"
    );
    assert!(
        view.read_with(cx, |view, _| view.scroll_top()) > 0.,
        "Ctrl+M moves the caret line to the middle"
    );

    press(cx, window, "ctrl-pageup");
    let top = view.read_with(cx, |view, _| view.caret().0);
    assert!(
        top < 30,
        "Ctrl+PageUp goes to the first visible line, got {top}"
    );
    press(cx, window, "ctrl-pagedown");
    let bottom = view.read_with(cx, |view, _| view.caret().0);
    assert!(bottom > top, "Ctrl+PageDown goes to the last visible line");

    press(cx, window, "ctrl-pageup");
    let before = view.read_with(cx, |view, _| view.caret().0);
    press(cx, window, "shift-pageup");
    assert!(
        !selection(cx, &view).is_empty(),
        "Shift+PageUp selects a page"
    );
    assert!(
        view.read_with(cx, |view, _| view.caret().0) < before,
        "the caret moves up with the selection"
    );

    press(cx, window, "ctrl-shift-pagedown");
    assert!(
        !selection(cx, &view).is_empty(),
        "Ctrl+Shift+PageDown selects to the last visible line"
    );
    assert!(rows > 1);
}

#[ui_kit::gpui::test]
fn insert_mode_carets_and_simple_paste(cx: &mut TestAppContext) {
    let (_path, window, view) = open_text(cx, "insert-carets", "one\ntwo\nthree\n");
    view.update(cx, |view, cx| view.restore_caret(0, 0, cx));
    press(cx, window, "shift-down shift-down");
    press(cx, window, "alt-shift-g");
    type_text(cx, window, "!");
    assert_eq!(
        view.read_with(cx, |view, _| view.text())
            .unwrap_or_default(),
        "one!\ntwo!\nthree\n",
        "Alt+Shift+G puts a caret at the end of every selected line"
    );

    reset(cx, &view, "abc\n", 0, 1);
    press(cx, window, "insert");
    let status = window
        .read_with(cx, |workspace, cx| workspace.editor_status(cx))
        .unwrap()
        .map(|status| status.insert);
    assert_eq!(
        status.as_deref(),
        Some("Overwrite"),
        "Insert toggles the mode"
    );
    type_text(cx, window, "X");
    assert_eq!(
        view.read_with(cx, |view, _| view.text())
            .unwrap_or_default(),
        "aXc\n",
        "typing replaces the character under the caret"
    );
    press(cx, window, "insert");
    let status = window
        .read_with(cx, |workspace, cx| workspace.editor_status(cx))
        .unwrap()
        .map(|status| status.insert);
    assert_eq!(status.as_deref(), Some("Insert"));
    type_text(cx, window, "Y");
    assert_eq!(
        view.read_with(cx, |view, _| view.text())
            .unwrap_or_default(),
        "aXYc\n"
    );

    reset(cx, &view, "abc\n", 0, 0);
    cx.update(|cx| {
        cx.write_to_clipboard(ui_kit::ClipboardItem::new_string("ZZ".to_owned()));
    });
    press(cx, window, "ctrl-alt-shift-v");
    assert_eq!(
        view.read_with(cx, |view, _| view.text())
            .unwrap_or_default(),
        "ZZabc\n",
        "Ctrl+Alt+Shift+V pastes the text as it is"
    );

    reset(cx, &view, "a\nb\n", 0, 0);
    press(cx, window, "shift-down shift-down");
    press(cx, window, "tab");
    assert_eq!(
        view.read_with(cx, |view, _| view.text())
            .unwrap_or_default(),
        "    a\n    b\n",
        "Tab with a selection indents its lines"
    );
}

#[ui_kit::gpui::test]
fn tab_leaves_a_pair_the_editor_inserted(cx: &mut TestAppContext) {
    let (_path, window, view) = open_text(cx, "brace-out", "");
    type_text(cx, window, "f(");
    assert_eq!(
        view.read_with(cx, |view, _| view.text())
            .unwrap_or_default(),
        "f()"
    );
    press(cx, window, "tab");
    assert_eq!(
        view.read_with(cx, |view, _| view.caret()),
        (0, 3),
        "Tab moves past the closing bracket"
    );
    assert_eq!(
        view.read_with(cx, |view, _| view.text())
            .unwrap_or_default(),
        "f()"
    );

    reset(cx, &view, "x\n", 0, 0);
    press(cx, window, "tab");
    assert_eq!(
        view.read_with(cx, |view, _| view.text())
            .unwrap_or_default(),
        "    x\n",
        "away from a pair Tab keeps inserting the indent"
    );
}

#[ui_kit::gpui::test]
fn an_element_moves_one_step_with_its_caret(cx: &mut TestAppContext) {
    let (_path, window, view) = open_text(cx, "move-element", "g(a, b, c)\n");
    view.update(cx, |view, cx| view.restore_caret(0, 2, cx));
    cx.run_until_parked();

    press(cx, window, "ctrl-alt-shift-right");
    assert_eq!(
        view.read_with(cx, |view, _| view.text())
            .unwrap_or_default(),
        "g(b, a, c)\n"
    );
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (0, 5));

    press(cx, window, "ctrl-alt-shift-left");
    assert_eq!(
        view.read_with(cx, |view, _| view.text())
            .unwrap_or_default(),
        "g(a, b, c)\n"
    );
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (0, 2));

    press(cx, window, "ctrl-alt-shift-right");
    press(cx, window, "ctrl-z");
    assert_eq!(
        view.read_with(cx, |view, _| view.text())
            .unwrap_or_default(),
        "g(a, b, c)\n",
        "one step of undo takes the move back"
    );

    reset(cx, &view, "class A implements B, C {\n}\n", 0, 19);
    press(cx, window, "ctrl-alt-shift-right");
    assert_eq!(
        view.read_with(cx, |view, _| view.text())
            .unwrap_or_default(),
        "class A implements C, B {\n}\n"
    );
}

#[ui_kit::gpui::test]
fn context_info_shows_the_header_above_the_first_visible_line(cx: &mut TestAppContext) {
    let mut text = String::from("void run() {\n");
    for line in 0..200 {
        text.push_str(&format!("    call{line}();\n"));
    }
    text.push_str("}\n");
    let (_path, window, view) = open_text(cx, "context-info", &text);
    view.update(cx, |view, cx| view.restore_caret(150, 4, cx));
    cx.run_until_parked();
    draw(cx, window);

    press(cx, window, "ctrl-m");
    draw(cx, window);
    assert!(
        view.read_with(cx, |view, _| view.scroll_top()) > 0.,
        "the header is above the viewport now"
    );

    press(cx, window, "alt-q");
    draw(cx, window);
    assert!(
        has_popup(cx, window, "context-info-popup"),
        "Alt+Q shows the header"
    );
    let popup = bounds_of(cx, window, "context-info-popup");
    let body = bounds_of(cx, window, "editor-body-0");
    assert!(
        popup.top() <= body.top() + ui_kit::px(64.),
        "the header shows over the top of the editor"
    );

    press(cx, window, "escape");
    draw(cx, window);
    assert!(
        !has_popup(cx, window, "context-info-popup"),
        "Escape closes Context Info"
    );

    press(cx, window, "alt-q");
    draw(cx, window);
    assert!(has_popup(cx, window, "context-info-popup"));
    press(cx, window, "down");
    draw(cx, window);
    assert!(
        !has_popup(cx, window, "context-info-popup"),
        "a move of the caret closes Context Info"
    );
}
