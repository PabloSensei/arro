//! The bottom row of the modal dialogs, one rule for every window: the primary button stands
//! first from the left, then the others in their order, then Apply; the buttons share one
//! height and one gap, and the row keeps the padding of the dialog from its right and bottom
//! edges. Enter presses the primary button and Escape closes the dialog.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use ui_kit::gpui;
use ui_kit::gpui::{Bounds, Pixels, TestAppContext, VisualTestContext, WindowHandle};
use ui_kit::metrics::{CONTROL_HEIGHT, DIALOG_PADDING, SPACE_M};
use ui_kit::px;
use workspace::Workspace;

use lang_api::{ActionChoice, ActionField, ActionParameters};

use crate::choice::{ChoiceDialog, DialogChoice};
use crate::window_tests::{
    bounds_of, click, draw, open_palette, open_path, open_sample_window, press,
};
use crate::window_tests_push_cancel::open_push_window;
use crate::window_tests_refactor_this::{
    RefactorLanguage, action, open_window as open_refactor_window, project, select_refactoring,
};
use crate::window_tests_run_configs::{open_dialog as open_run_dialog, open_run};

/// The border of a dialog frame, outside the padding its contents keep from it.
const FRAME: f32 = 1.;

/// Whether the element with `selector` is on the screen.
fn on_screen(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &'static str,
) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

/// Checks the bottom row of the dialog `dialog`: `buttons` stand in this order from the left,
/// one height and one `SPACE_M` apart, and the last one keeps `DIALOG_PADDING` from the right
/// and the bottom edges of the frame.
fn assert_bottom_row(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    dialog: &'static str,
    buttons: &[&'static str],
) {
    draw(cx, window);
    let frame = bounds_of(cx, window, dialog);
    let shown: Vec<Bounds<Pixels>> = buttons
        .iter()
        .map(|selector| bounds_of(cx, window, selector))
        .collect();
    for (selector, button) in buttons.iter().zip(&shown) {
        assert_eq!(
            button.size.height,
            px(CONTROL_HEIGHT),
            "{selector} is one height"
        );
    }
    for (names, pair) in buttons.windows(2).zip(shown.windows(2)) {
        assert!(
            pair[0].left() < pair[1].left(),
            "{} stands left of {}",
            names[0],
            names[1]
        );
        assert_eq!(
            pair[1].left() - pair[0].right(),
            px(SPACE_M),
            "{} and {} keep one gap",
            names[0],
            names[1]
        );
    }
    let last = shown.last().expect("a row has a button");
    let last_name = buttons.last().expect("a row has a button");
    assert_eq!(
        frame.right() - last.right(),
        px(DIALOG_PADDING + FRAME),
        "{last_name} keeps the padding from the right edge"
    );
    assert_eq!(
        frame.bottom() - last.bottom(),
        px(DIALOG_PADDING + FRAME),
        "{last_name} keeps the padding from the bottom edge"
    );
}

/// Shows a question with `choices` on the workspace window.
fn open_question(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    choices: Vec<DialogChoice>,
) {
    window
        .update(cx, |workspace, window, cx| {
            ChoiceDialog::show(
                "Reload the project?",
                "The files changed on disk.",
                choices,
                workspace,
                window,
                cx,
            );
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
}

#[gpui::test]
fn a_question_stands_its_primary_answer_first_and_enter_presses_it(cx: &mut TestAppContext) {
    let (_root, window) = open_sample_window(cx, "dialog-buttons-question");
    let reloaded = Arc::new(AtomicUsize::new(0));
    let counter = reloaded.clone();
    open_question(
        cx,
        window,
        vec![
            DialogChoice::new("Discard", false, |_, _, _| {}),
            DialogChoice::new("Cancel", false, |_, _, _| {}),
            DialogChoice::new("Reload", true, move |_, _, _| {
                counter.fetch_add(1, Ordering::SeqCst);
            }),
        ],
    );
    assert_bottom_row(
        cx,
        window,
        "choice-dialog",
        &["choice-2", "choice-0", "choice-1"],
    );

    press(cx, window, "enter");
    draw(cx, window);
    assert_eq!(
        reloaded.load(Ordering::SeqCst),
        1,
        "Enter presses the primary answer"
    );
    assert!(
        !on_screen(cx, window, "choice-dialog"),
        "the answer closes the question"
    );
}

#[gpui::test]
fn a_question_closes_on_escape_without_an_answer(cx: &mut TestAppContext) {
    let (_root, window) = open_sample_window(cx, "dialog-buttons-escape");
    let answered = Arc::new(AtomicUsize::new(0));
    let counter = answered.clone();
    open_question(
        cx,
        window,
        vec![
            DialogChoice::new("Reload", true, move |_, _, _| {
                counter.fetch_add(1, Ordering::SeqCst);
            }),
            DialogChoice::new("Cancel", false, |_, _, _| {}),
        ],
    );
    assert_bottom_row(cx, window, "choice-dialog", &["choice-0", "choice-1"]);

    press(cx, window, "escape");
    draw(cx, window);
    assert!(
        !on_screen(cx, window, "choice-dialog"),
        "Escape closes the question"
    );
    assert_eq!(answered.load(Ordering::SeqCst), 0, "Escape chooses nothing");
}

/// Opens Project Structure with its shortcut.
fn open_structure(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    press(cx, window, "ctrl-alt-shift-s");
    draw(cx, window);
}

#[gpui::test]
fn project_structure_stands_ok_cancel_apply_in_the_rule_order(cx: &mut TestAppContext) {
    let (_root, window) = open_sample_window(cx, "dialog-buttons-structure");
    open_structure(cx, window);
    assert_bottom_row(
        cx,
        window,
        "project-structure",
        &[
            "project-structure-ok",
            "project-structure-cancel",
            "project-structure-apply",
        ],
    );
    click(cx, window, "project-structure-cancel");
    draw(cx, window);
    assert!(
        !on_screen(cx, window, "project-structure"),
        "Cancel closes Project Structure"
    );
}

#[gpui::test]
fn run_configurations_keep_ok_cancel_apply_in_the_rule_order(cx: &mut TestAppContext) {
    let (_root, _planned, run, _panel) = open_run(cx, "dialog-buttons-run");
    open_run_dialog(cx, run);
    assert_bottom_row(
        cx,
        run,
        "run-configs",
        &["run-config-ok", "run-config-cancel", "run-config-apply"],
    );
    press(cx, run, "escape");
    draw(cx, run);
    assert!(
        !on_screen(cx, run, "run-configs"),
        "Escape closes the configurations"
    );
}

#[gpui::test]
fn settings_keep_ok_cancel_apply_in_the_rule_order(cx: &mut TestAppContext) {
    let (_root, window) = open_sample_window(cx, "dialog-buttons-settings");
    click(cx, window, "t-settings");
    draw(cx, window);
    assert_bottom_row(
        cx,
        window,
        "settings",
        &["settings-ok", "settings-cancel", "settings-apply"],
    );
    press(cx, window, "escape");
    draw(cx, window);
    assert!(!on_screen(cx, window, "settings"), "Escape closes Settings");
}

#[gpui::test]
fn reformat_file_keeps_run_and_cancel_in_the_rule_order(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "dialog-buttons-reformat");
    open_path(cx, window, &root.join("src").join("main.txt"));
    press(cx, window, "ctrl-alt-shift-l");
    draw(cx, window);
    assert_bottom_row(
        cx,
        window,
        "reformat-dialog",
        &["reformat-run", "reformat-cancel"],
    );
    press(cx, window, "escape");
    draw(cx, window);
    assert!(
        !on_screen(cx, window, "reformat-dialog"),
        "Escape closes Reformat File"
    );
}

#[gpui::test]
fn the_fields_of_project_structure_start_on_one_vertical_line(cx: &mut TestAppContext) {
    let (_root, window) = open_sample_window(cx, "dialog-buttons-structure-fields");
    open_structure(cx, window);
    let sdk = bounds_of(cx, window, "project-structure-sdk");
    let language = bounds_of(cx, window, "project-structure-language");
    assert_eq!(
        sdk.left(),
        language.left(),
        "the fields of the Project page start on one vertical line"
    );
    assert_eq!(
        sdk.size.height, language.size.height,
        "the fields of the Project page have one height"
    );
}

/// A server action with two fields: the form keeps its bottom row, and its fields start on one
/// vertical line and have one height.
#[gpui::test]
fn the_refactor_form_keeps_its_row_and_its_fields_share_one_edge(cx: &mut TestAppContext) {
    let root = project("dialog-buttons-refactor", &[("src/Shape.txt", "shape\n")]);
    let shape = root.join("src").join("Shape.txt");
    let language = RefactorLanguage::new();
    language.set_actions(vec![action(
        "Extract interface...",
        "refactor.extract.interface",
        Some("handle"),
    )]);
    language.set_form(ActionParameters {
        title: "Extract interface".to_owned(),
        members: Vec::new(),
        members_label: "Members".to_owned(),
        fields: vec![
            ActionField {
                key: "name".to_owned(),
                label: "Interface name".to_owned(),
                value: "ShapeView".to_owned(),
            },
            ActionField {
                key: "package".to_owned(),
                label: "Package".to_owned(),
                value: "app".to_owned(),
            },
        ],
        choices: vec![ActionChoice {
            name: "probe".to_owned(),
            detail: Some("/src".to_owned()),
            value: "{\"uri\":\"probe\"}".to_owned(),
        }],
        choices_label: "Destination".to_owned(),
    });
    let window = open_refactor_window(cx, root.clone(), language.clone());
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &shape);
    press(cx, window, "ctrl-alt-shift-t");
    draw(cx, window);
    select_refactoring(cx, window, &shape, "Extract Interface...");
    press(cx, window, "enter");
    draw(cx, window);

    assert_bottom_row(
        cx,
        window,
        "refactor-form",
        &["refactor-form-apply", "refactor-form-cancel"],
    );
    let first = bounds_of(cx, window, "refactor-form-field-0");
    let second = bounds_of(cx, window, "refactor-form-field-1");
    assert_eq!(
        first.left(),
        second.left(),
        "the fields start on one vertical line"
    );
    assert_eq!(
        first.size.height, second.size.height,
        "the fields have one height"
    );
    press(cx, window, "escape");
    draw(cx, window);
    assert!(
        !on_screen(cx, window, "refactor-form"),
        "Escape closes the form"
    );
}

/// A dump without text cannot be analysed: a click on its grey OK and Ctrl+Enter both leave the
/// window open.
#[gpui::test]
fn a_stack_trace_without_text_keeps_its_window_open(cx: &mut TestAppContext) {
    let (_root, window) = open_sample_window(cx, "dialog-buttons-stack");
    open_palette(cx, window, "workspace.analyze_stack_trace");
    draw(cx, window);
    assert!(
        on_screen(cx, window, "stack-trace-ok"),
        "the stack trace window is open"
    );
    click(cx, window, "stack-trace-ok");
    draw(cx, window);
    assert!(
        on_screen(cx, window, "stack-trace-ok"),
        "a click on the grey OK does nothing"
    );
    press(cx, window, "ctrl-enter");
    draw(cx, window);
    assert!(
        on_screen(cx, window, "stack-trace-ok"),
        "Ctrl+Enter submits no empty dump"
    );
}

#[gpui::test]
fn push_keeps_push_and_cancel_in_the_rule_order(cx: &mut TestAppContext) {
    let (window, _root, _bare, _git) = open_push_window(cx, "dialog-buttons-push");
    draw(cx, window);
    assert_bottom_row(cx, window, "push-dialog", &["push-go", "push-cancel"]);
    press(cx, window, "escape");
    draw(cx, window);
    assert!(
        !on_screen(cx, window, "push-dialog"),
        "Escape closes the Push window"
    );
}
