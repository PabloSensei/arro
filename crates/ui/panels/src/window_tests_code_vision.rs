//! The counts a language server puts above declarations, as IDEA shows them: their
//! rows in the text, their lists on a click and the Settings that hide them.

use std::time::Duration;

use ui_kit::gpui::{self, TestAppContext, VisualTestContext, WindowHandle};
use ui_kit::{Modifiers, point, px};
use workspace::{Settings, Workspace};

use crate::usages::ActiveShowUsages;
use crate::window_tests::{bounds_of, buffer_of, draw, editor_of, line_height, open_path, press};
use crate::window_tests_lang::{FakeLanguage, open_window_with, project, span_in, uri_of};

/// The editor asks after a pause, as it does for the hints in the text.
fn settle(cx: &mut TestAppContext) {
    cx.executor().advance_clock(Duration::from_millis(300));
    cx.run_until_parked();
}

fn labels(
    cx: &TestAppContext,
    view: &gpui::Entity<editor_view::EditorView>,
) -> Vec<(usize, String)> {
    view.read_with(cx, |view, _| view.code_vision_texts())
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

fn usages_popup(cx: &TestAppContext) -> usize {
    cx.update(|cx| {
        cx.try_global::<ActiveShowUsages>()
            .and_then(|active| active.0.clone())
            .map(|popup| popup.read_with(cx, |popup, _| popup.visible_rows()))
            .unwrap_or(0)
    })
}

/// A click on the label of the row `row`, a few pixels into its text.
fn click_label(cx: &mut TestAppContext, window: WindowHandle<Workspace>, row: usize, column: f32) {
    let height = line_height(cx);
    let bounds = bounds_of(cx, window, "editor-body-0");
    let x = bounds.left() + px(67. + column);
    let y = bounds.top() + px(height * row as f32 + height / 2.);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_click(point(x, y), Modifiers::default());
    cx.run_until_parked();
}

#[ui_kit::gpui::test]
fn a_label_of_the_server_stands_above_the_method_and_a_click_opens_the_usages(
    cx: &mut TestAppContext,
) {
    let root = project(
        "code-vision-usages",
        &[("src/Main.txt", "one\nvoid m() {}\n")],
    );
    let main = root.join("src").join("Main.txt");
    let language = FakeLanguage::new();
    language.set_code_lens(vec![
        (4, "2 usages".to_owned(), "show.references".to_owned()),
        (
            4,
            "1 implementation".to_owned(),
            "show.implementations".to_owned(),
        ),
    ]);
    let language_trait = language.clone();
    // The two places the list shows, both in the file under the label.
    language_trait.set_references(vec![
        lang_api::SourceSpan {
            uri: uri_of(&main),
            span: span_in("one\nvoid m() {}\n", 0, 3),
        },
        lang_api::SourceSpan {
            uri: uri_of(&main),
            span: span_in("one\nvoid m() {}\n", 4, 8),
        },
    ]);
    let window = open_window_with(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &main);
    let view = editor_of(cx, window, &main);
    settle(cx);

    assert_eq!(
        labels(cx, &view),
        [(1, "2 usages  1 implementation".to_owned())],
        "the row stands above the declaration and holds both counts"
    );

    click_label(cx, window, 1, 8.);
    settle(cx);
    assert_eq!(usages_popup(cx), 2, "a click on the count lists the usages");
}

#[ui_kit::gpui::test]
fn settings_take_the_rows_away_and_bring_them_back(cx: &mut TestAppContext) {
    let root = project(
        "code-vision-settings",
        &[("src/Main.txt", "one\nvoid m() {}\n")],
    );
    let main = root.join("src").join("Main.txt");
    let language = FakeLanguage::new();
    language.set_code_lens(vec![
        (4, "2 usages".to_owned(), "show.references".to_owned()),
        (
            4,
            "1 implementation".to_owned(),
            "show.implementations".to_owned(),
        ),
    ]);
    let window = open_window_with(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &main);
    let view = editor_of(cx, window, &main);
    settle(cx);
    assert_eq!(
        labels(cx, &view),
        [(1, "2 usages  1 implementation".to_owned())]
    );

    apply(cx, window, |settings| settings.code_vision_usages = false);
    settle(cx);
    assert_eq!(
        labels(cx, &view),
        [(1, "1 implementation".to_owned())],
        "the usage count leaves the row at once"
    );

    apply(cx, window, |settings| {
        settings.code_vision_implementations = false;
    });
    settle(cx);
    assert!(labels(cx, &view).is_empty(), "no label is left");

    apply(cx, window, |settings| {
        settings.code_vision_usages = true;
    });
    settle(cx);
    assert_eq!(
        labels(cx, &view),
        [(1, "2 usages".to_owned())],
        "the count comes back"
    );
}

#[ui_kit::gpui::test]
fn an_edit_above_the_declaration_moves_the_row(cx: &mut TestAppContext) {
    let root = project(
        "code-vision-edit",
        &[("src/Main.txt", "one\nvoid m() {}\n")],
    );
    let main = root.join("src").join("Main.txt");
    let language = FakeLanguage::new();
    language.set_code_lens(vec![(
        4,
        "2 usages".to_owned(),
        "show.references".to_owned(),
    )]);
    let window = open_window_with(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &main);
    let view = editor_of(cx, window, &main);
    settle(cx);
    assert_eq!(labels(cx, &view), [(1, "2 usages".to_owned())]);

    view.update(cx, |view, cx| view.restore_caret(0, 0, cx));
    press(cx, window, "enter");
    settle(cx);

    assert_eq!(buffer_of(cx, &view), "\none\nvoid m() {}\n");
    assert_eq!(
        labels(cx, &view),
        [(2, "2 usages".to_owned())],
        "the row follows its declaration down"
    );
}

#[ignore = "needs IDE_CORPUS and a filled package cache"]
#[ui_kit::gpui::test]
fn a_real_server_counts_the_usages_of_a_method_and_the_implementations_of_a_type(
    cx: &mut TestAppContext,
) {
    use crate::window_tests::project_setup;
    use crate::window_tests::scratch;
    use crate::window_tests_real::{build_window, project as real_project, wait_for, wait_ready};

    let (root, language) = real_project("real-code-vision");
    let (class, interface) = window_language::write_code_vision_sample(&root);
    let window = build_window(
        cx,
        root.clone(),
        &language,
        project_setup(scratch("code-vision-cache")),
    );
    cx.run_until_parked();
    draw(cx, window);
    wait_ready(cx, window);

    open_path(cx, window, &interface);
    let view = editor_of(cx, window, &interface);
    wait_for(
        cx,
        "the implementation count of the interface",
        Duration::from_secs(120),
        |cx| {
            labels(cx, &view)
                .iter()
                .any(|(_, text)| text.contains("1 implementation") || text.contains("1 inheritor"))
        },
    );

    open_path(cx, window, &class);
    let view = editor_of(cx, window, &class);
    wait_for(
        cx,
        "the usage count of the method",
        Duration::from_secs(120),
        |cx| {
            labels(cx, &view)
                .iter()
                .any(|(_, text)| text.contains("2 usages"))
        },
    );
}
