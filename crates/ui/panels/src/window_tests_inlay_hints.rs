//! The parameter and type hints of a language server in the text of the window.
//!
//! The fake language answers from memory; the last test runs against a real language
//! server on a copy of the corpus.

use std::time::Duration;

use lang_api::InlayHintKind;
use ui_kit::gpui::{self, TestAppContext, WindowHandle};
use workspace::{InlayParameterNames, Settings, Workspace};

use crate::window_tests::{buffer_of, draw, editor_of, open_path, type_text};
use crate::window_tests_lang::{FakeLanguage, open_window_with, project};

/// One answer of the fake language and one delayed question of the editor.
fn settle(cx: &mut TestAppContext) {
    cx.executor().advance_clock(Duration::from_millis(300));
    cx.run_until_parked();
}

fn hints(
    cx: &TestAppContext,
    view: &gpui::Entity<editor_view::EditorView>,
) -> Vec<(usize, String)> {
    view.read_with(cx, |view, _| view.inlay_texts())
}

fn apply(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    change: impl FnOnce(&mut Settings),
) {
    window
        .update(cx, |workspace, _, cx| workspace.update_settings(change, cx))
        .unwrap();
    cx.run_until_parked();
}

#[gpui::test]
fn a_parameter_hint_stands_before_its_argument(cx: &mut TestAppContext) {
    let root = project("inlay-show", &[("src/Call.txt", "call(true, 5);\n")]);
    let language = FakeLanguage::new();
    language.set_inlays(vec![
        (5, "flag:".to_owned(), InlayHintKind::Parameter),
        (11, "count:".to_owned(), InlayHintKind::Parameter),
    ]);
    let window = open_window_with(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);

    let path = root.join("src").join("Call.txt");
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    settle(cx);

    assert_eq!(
        hints(cx, &view),
        [(5, "flag: ".to_owned()), (11, "count: ".to_owned())],
        "the names stand right before the arguments"
    );
}

#[gpui::test]
fn no_parameter_hints_at_none_and_type_hints_only_when_asked(cx: &mut TestAppContext) {
    let root = project("inlay-settings", &[("src/Call.txt", "call(true, 5);\n")]);
    let language = FakeLanguage::new();
    language.set_inlays(vec![
        (5, "flag:".to_owned(), InlayHintKind::Parameter),
        (11, "Integer".to_owned(), InlayHintKind::Type),
    ]);
    let window = open_window_with(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    let path = root.join("src").join("Call.txt");
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    settle(cx);
    assert_eq!(
        hints(cx, &view),
        [(5, "flag: ".to_owned())],
        "the type hint is off by default"
    );

    apply(cx, window, |settings| {
        settings.inlay_parameter_names = InlayParameterNames::None;
    });
    settle(cx);
    assert!(hints(cx, &view).is_empty(), "None leaves no hint");

    apply(cx, window, |settings| settings.inlay_type_hints = true);
    settle(cx);
    assert_eq!(
        hints(cx, &view),
        [(11, "Integer ".to_owned())],
        "only the type hint stays"
    );

    apply(cx, window, |settings| {
        settings.inlay_type_hints = false;
        settings.inlay_parameter_names = InlayParameterNames::All;
    });
    settle(cx);
    assert_eq!(
        hints(cx, &view),
        [(5, "flag: ".to_owned())],
        "the parameter hints come back"
    );
}

#[gpui::test]
fn an_edit_before_a_hint_moves_it(cx: &mut TestAppContext) {
    let root = project("inlay-edit", &[("src/Call.txt", "call(true, 5);\n")]);
    let language = FakeLanguage::new();
    language.set_inlays(vec![(5, "flag:".to_owned(), InlayHintKind::Parameter)]);
    let window = open_window_with(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    let path = root.join("src").join("Call.txt");
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    settle(cx);
    assert_eq!(hints(cx, &view), [(5, "flag: ".to_owned())]);

    view.update(cx, |view, cx| view.restore_caret(0, 0, cx));
    type_text(cx, window, "x");
    settle(cx);

    assert_eq!(
        buffer_of(cx, &view),
        "xcall(true, 5);\n",
        "the hint is not part of the text"
    );
    assert_eq!(
        hints(cx, &view),
        [(6, "flag: ".to_owned())],
        "the hint follows the text it belongs to"
    );
}

#[ignore = "needs IDE_CORPUS and a filled package cache"]
#[ui_kit::gpui::test]
fn a_real_language_server_names_parameters_of_literals_at_its_places(cx: &mut TestAppContext) {
    use crate::window_tests::project_setup;
    use crate::window_tests::scratch;
    use crate::window_tests_real::{build_window, project as real_project, wait_for, wait_ready};

    let started = std::time::Instant::now();
    let (root, language) = real_project("real-inlay-hints");
    let source = window_language::write_inlay_hints_sample(&root);

    let window = build_window(
        cx,
        root.clone(),
        &language,
        project_setup(scratch("inlay-hints-cache")),
    );
    cx.run_until_parked();
    draw(cx, window);
    wait_ready(cx, window);
    open_path(cx, window, &source);
    let view = editor_of(cx, window, &source);
    wait_for(
        cx,
        "the parameter hints of the call",
        Duration::from_secs(120),
        |cx| !hints(cx, &view).is_empty(),
    );

    let text = buffer_of(cx, &view);
    let flag = text.find("true").expect("the literal of the call");
    let count = text.find('5').expect("the number of the call");
    let shown = hints(cx, &view);
    assert!(
        shown
            .iter()
            .any(|(offset, label)| *offset == flag && label.trim() == "flag:"),
        "the first name stands before its literal: {shown:?}"
    );
    assert!(
        shown
            .iter()
            .any(|(offset, label)| *offset == count && label.trim() == "count:"),
        "the second name stands before its literal: {shown:?}"
    );
    let value = text.find("call(value").expect("the call of a variable") + "call(".len();
    assert!(
        !shown.iter().any(|(offset, _)| *offset == value),
        "with Literals a variable argument gets no name: {shown:?}"
    );

    // All is a setting of the server: it applies without a restart.
    apply(cx, window, |settings| {
        settings.inlay_parameter_names = InlayParameterNames::All;
    });
    wait_for(
        cx,
        "the name before a variable argument",
        Duration::from_secs(60),
        |cx| {
            hints(cx, &view)
                .iter()
                .any(|(offset, label)| *offset == value && label.trim() == "flag:")
        },
    );

    println!(
        "real server inlay hints: {:.1} s",
        started.elapsed().as_secs_f64()
    );
}
