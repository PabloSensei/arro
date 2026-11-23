//! Ctrl+J in a source file: the templates that fit the place of the caret, the abbreviation
//! that leads each row, and the letters typed into the open list. The language is the real
//! one of the window language, so the places come from its syntax tree.

use editor_view::{EditorView, Languages, Spot};
use ui_kit::gpui::{self, Entity, TestAppContext, VisualTestContext, WindowHandle};
use workspace::Workspace;

use crate::window_tests::{
    bounds_of, buffer_of, draw, editor_of, open_path, open_window, press, sample_project,
    type_text, wait_for_parse_signal,
};

/// A method with an empty line at its start and a call chain in its last statement.
const SOURCE: &str = "class Owner {\n    void run(Owner owner) {\n\n        String lastName = owner.getLastName();\n    }\n}\n";
/// The empty line inside the method.
const EMPTY_LINE: usize = 2;
/// The line with the call chain.
const CALL_LINE: usize = 3;

/// A source file of a test project, parsed by the real grammar, with its editor open.
fn open_owner(
    cx: &mut TestAppContext,
    name: &str,
) -> (WindowHandle<Workspace>, Entity<EditorView>) {
    let root = sample_project(name);
    let path = window_language::write_test_source(&root, "Owner", SOURCE);
    let window = open_window(cx, root);
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
    (window, view)
}

fn go_to(cx: &mut TestAppContext, view: &Entity<EditorView>, line: usize, column: usize) {
    view.update(cx, |view, cx| view.go_to(Spot::Line { line, column }, cx));
}

/// Whether an element with `selector` is on screen after the last frame.
fn shown(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) -> bool {
    draw(cx, window);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

#[ui_kit::gpui::test]
fn ctrl_j_on_an_empty_line_of_a_method_lists_sout_with_its_key_first(cx: &mut TestAppContext) {
    let (window, view) = open_owner(cx, "insert-template-empty-line");
    go_to(cx, &view, EMPTY_LINE, 0);
    press(cx, window, "ctrl-j");
    draw(cx, window);
    assert!(
        shown(cx, window, "template-item-sout"),
        "Ctrl+J on an empty line of a method lists sout"
    );

    // The abbreviation is the first column of its row and the description follows it.
    let popup = bounds_of(cx, window, "template-popup");
    let key = bounds_of(cx, window, "template-key-sout");
    let description = bounds_of(cx, window, "template-description-sout");
    assert!(
        key.origin.x < description.origin.x,
        "the key {key:?} is left of the description {description:?}"
    );
    assert!(
        key.origin.x + key.size.width <= description.origin.x,
        "the description starts after the key"
    );
    assert!(
        description.origin.x + description.size.width <= popup.origin.x + popup.size.width,
        "the description stays inside the list"
    );
}

#[ui_kit::gpui::test]
fn ctrl_j_inside_a_call_chain_lists_no_statement_template(cx: &mut TestAppContext) {
    let (window, view) = open_owner(cx, "insert-template-call-chain");
    // Right after `getLa` in `owner.getLastName()`.
    let column = "        String lastName = owner.getLa".chars().count();
    go_to(cx, &view, CALL_LINE, column);
    press(cx, window, "ctrl-j");
    draw(cx, window);
    assert!(
        !shown(cx, window, "template-item-sout"),
        "a statement does not go into the middle of a call chain"
    );
    assert!(
        !shown(cx, window, "template-item-fori"),
        "a loop does not go into the middle of a call chain"
    );
    assert!(
        shown(cx, window, "template-item-St"),
        "the templates of an expression are listed"
    );
    assert_eq!(
        buffer_of(cx, &view),
        SOURCE,
        "Ctrl+J changes nothing in the text"
    );
}

#[ui_kit::gpui::test]
fn typed_letters_filter_the_open_list_and_enter_takes_the_first_row(cx: &mut TestAppContext) {
    let (window, view) = open_owner(cx, "insert-template-filter");
    go_to(cx, &view, EMPTY_LINE, 0);
    press(cx, window, "ctrl-j");
    draw(cx, window);
    assert!(shown(cx, window, "template-item-sout"));

    // `fo` is in the abbreviation of fori, and the word `formatted` of the description of
    // souf starts with it.
    type_text(cx, window, "fo");
    assert!(
        shown(cx, window, "template-item-fori"),
        "fori has fo in its abbreviation"
    );
    assert!(
        !shown(cx, window, "template-item-sout"),
        "sout has no fo in it"
    );
    assert!(
        shown(cx, window, "template-item-souf"),
        "a word of the description of souf starts with fo"
    );
    assert!(
        !buffer_of(cx, &view).contains("fo"),
        "the typed letters go to the list, not to the text"
    );

    // Backspace takes the letters back, and the list grows again.
    press(cx, window, "backspace");
    press(cx, window, "backspace");
    assert!(
        shown(cx, window, "template-item-sout"),
        "without a filter sout is listed again"
    );

    // Enter takes the first row of the filter: fori, with its fields filled in.
    type_text(cx, window, "fo");
    press(cx, window, "enter");
    let text = buffer_of(cx, &view);
    assert!(text.contains("for (int i = 0; i < ; i++) {"), "{text:?}");
}
