//! Column selection from the keyboard and the move keys of more than one caret: a
//! rectangle takes the same columns on every line of a short text, and every caret moves
//! its own statement or line. The window is a test one: the buffers and the disk only.

use std::fs;
use std::path::PathBuf;

use editor_view::{EditorView, Languages};
use ui_kit::gpui;
use ui_kit::gpui::{Modifiers, TestAppContext, VisualTestContext, WindowHandle};
use workspace::Workspace;

use crate::window_tests::{
    buffer_of, draw, editor_of, open_path, open_window, press, sample_project, type_text,
    wait_for_parse_signal,
};
use crate::window_tests_edit::text_point;

fn open_text(
    cx: &mut TestAppContext,
    name: &str,
    text: &str,
) -> (PathBuf, WindowHandle<Workspace>, gpui::Entity<EditorView>) {
    let root = sample_project(name);
    let path = root.join("src").join("main.txt");
    fs::write(&path, text).unwrap();
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    (root, window, view)
}

/// A source file of the sample project, parsed by the real grammar of the language.
fn open_source(
    cx: &mut TestAppContext,
    name: &str,
    text: &str,
) -> (PathBuf, WindowHandle<Workspace>, gpui::Entity<EditorView>) {
    let root = sample_project(name);
    let path = window_language::write_test_source(&root, "Main", text);
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    cx.update(|cx| {
        cx.set_global(Languages {
            resolve: window_language::language_for,
            templates: window_language::templates_for,
        });
    });
    draw(cx, window);
    open_path(cx, window, &path);
    wait_for_parse_signal(cx, window, &path);
    let view = editor_of(cx, window, &path);
    (root, window, view)
}

/// A click in the text at a buffer line and a column in characters.
fn click_text(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    line: usize,
    column: usize,
    modifiers: Modifiers,
) {
    let position = text_point(cx, window, line, column);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_click(position, modifiers);
    cx.run_until_parked();
    draw(cx, window);
}

#[ui_kit::gpui::test]
fn column_mode_types_and_deletes_on_every_line_of_the_block(cx: &mut TestAppContext) {
    let (_root, window, view) = open_text(cx, "column-type", "abcd\nb\ncdef\n");

    click_text(cx, window, 0, 2, Modifiers::default());
    press(cx, window, "alt-shift-insert");
    press(cx, window, "shift-down");
    press(cx, window, "shift-down");
    type_text(cx, window, "x");
    assert_eq!(
        buffer_of(cx, &view),
        "abxcd\nb x\ncdxef\n",
        "the text lands on every line, and the short line gets the spaces up to the column"
    );

    press(cx, window, "backspace");
    assert_eq!(
        buffer_of(cx, &view),
        "abcd\nb \ncdef\n",
        "Backspace takes the typed character back on every line"
    );
}

#[ui_kit::gpui::test]
fn escape_leaves_one_caret_of_the_block_for_the_next_typing(cx: &mut TestAppContext) {
    let (_root, window, view) = open_text(cx, "column-escape", "abc\ndef\nghi\n");

    click_text(cx, window, 0, 1, Modifiers::default());
    press(cx, window, "alt-shift-insert");
    press(cx, window, "shift-down");
    press(cx, window, "escape");
    type_text(cx, window, "z");
    assert_eq!(
        buffer_of(cx, &view),
        "abc\ndzef\nghi\n",
        "Escape drops the block and leaves the last line of it"
    );
}

#[ui_kit::gpui::test]
fn move_statement_moves_the_statement_of_every_caret_and_one_undo_takes_them_back(
    cx: &mut TestAppContext,
) {
    const TEXT: &str = "class A {\n    void one() {\n        int a = 1;\n        int b = 2;\n    }\n\n    void two() {\n        int c = 3;\n        int d = 4;\n    }\n}\n";
    let (_root, window, view) = open_source(cx, "column-statements", TEXT);

    click_text(cx, window, 2, 8, Modifiers::default());
    let alt = Modifiers {
        alt: true,
        ..Modifiers::default()
    };
    click_text(cx, window, 7, 8, alt);
    press(cx, window, "ctrl-shift-down");
    assert_eq!(
        buffer_of(cx, &view),
        "class A {\n    void one() {\n        int b = 2;\n        int a = 1;\n    }\n\n    void two() {\n        int d = 4;\n        int c = 3;\n    }\n}\n",
        "both statements move down in their own methods"
    );
    press(cx, window, "ctrl-z");
    assert_eq!(
        buffer_of(cx, &view),
        TEXT,
        "one undo returns both statements"
    );
}

#[ui_kit::gpui::test]
fn move_line_moves_the_line_of_every_caret_once(cx: &mut TestAppContext) {
    let (_root, window, view) = open_text(cx, "column-lines", "one\ntwo\nthree\nfour\n");

    click_text(cx, window, 0, 0, Modifiers::default());
    let alt = Modifiers {
        alt: true,
        ..Modifiers::default()
    };
    click_text(cx, window, 0, 2, alt);
    click_text(cx, window, 2, 0, alt);
    press(cx, window, "alt-shift-down");
    assert_eq!(
        buffer_of(cx, &view),
        "two\none\nfour\nthree\n",
        "the two carets of the first line move it once, the third line moves too"
    );
    press(cx, window, "ctrl-z");
    assert_eq!(buffer_of(cx, &view), "one\ntwo\nthree\nfour\n");
}

#[ui_kit::gpui::test]
fn move_statement_moves_an_argument_through_the_comma_of_its_call(cx: &mut TestAppContext) {
    const TEXT: &str = "class A {\n    void a() {\n        f(first, second);\n    }\n}\n";
    let (_root, window, view) = open_source(cx, "column-argument", TEXT);

    click_text(cx, window, 2, 11, Modifiers::default());
    press(cx, window, "ctrl-shift-down");
    assert_eq!(
        buffer_of(cx, &view),
        "class A {\n    void a() {\n        f(second, first);\n    }\n}\n",
        "the argument changes places with the one after the comma"
    );
    press(cx, window, "ctrl-z");
    assert_eq!(buffer_of(cx, &view), TEXT);
}
