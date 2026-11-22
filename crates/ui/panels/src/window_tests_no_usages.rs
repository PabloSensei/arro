//! The count of no usages is not shown above a declaration that a framework or a test
//! runner calls. Every other count stays as it was.

use std::time::Duration;

use ui_kit::gpui;
use ui_kit::gpui::TestAppContext;

use crate::window_tests::{draw, editor_of, open_path};
use crate::window_tests_lang::{FakeLanguage, open_window_with, project};

/// The editor asks after a pause, as it does for the hints in the text.
fn settle(cx: &mut TestAppContext) {
    cx.executor().advance_clock(Duration::from_millis(300));
    cx.run_until_parked();
}

#[ui_kit::gpui::test]
fn no_usages_is_hidden_only_above_a_declaration_a_framework_calls(cx: &mut TestAppContext) {
    // Lines: 0 the annotation of a test, 1 the test, 2 a plain method, 3 an annotation,
    // 4 a method that a framework calls and that has two usages.
    let text = "@Test\nvoid checks() {}\nvoid unused() {}\n@Test\nvoid used() {}\n";
    let root = project("no-usages-implicit", &[("src/Main.txt", text)]);
    let main = root.join("src").join("Main.txt");
    let language = FakeLanguage::new();
    language.set_code_lens(vec![
        (
            text.find("void checks").unwrap(),
            "0 usages".to_owned(),
            "show.references".to_owned(),
        ),
        (
            text.find("void unused").unwrap(),
            "0 usages".to_owned(),
            "show.references".to_owned(),
        ),
        (
            text.find("void used").unwrap(),
            "2 usages".to_owned(),
            "show.references".to_owned(),
        ),
    ]);
    // The test runs from its annotation on line 0 to its name on line 1, and so does the
    // method on lines 3 and 4.
    language.set_implicit_spans(vec![0..2, 3..5]);
    let window = open_window_with(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &main);
    let view = editor_of(cx, window, &main);
    settle(cx);

    let labels = view.read_with(cx, |view, _| view.code_vision_texts());
    assert_eq!(
        labels,
        [(2, "no usages".to_owned()), (4, "2 usages".to_owned())],
        "a test has no label of zero usages, a plain method keeps it, a count stays"
    );
}
