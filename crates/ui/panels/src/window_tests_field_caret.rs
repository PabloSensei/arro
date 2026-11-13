//! The caret of every field: the windows that draw a `TextField` show its caret when the
//! field has the keys, and Shift+Left draws the selected text. The check `check-fields`
//! keeps new fields on the same renderer of `ui-kit`; these tests look at the frames of
//! each window the bug of the commit message reported.

use std::fs;

use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use workspace::{COMMIT_TOOL, TASKS_TOOL, Workspace};

use crate::script::ActiveCommitPanel;
use crate::start::{ProjectTemplate, StartScreen};
use crate::window_tests::{
    bounds_of, click, draw, open_sample_window, open_window, sample_project, scratch,
};

fn has<T: ui_kit::Render + 'static>(
    cx: &mut TestAppContext,
    window: WindowHandle<T>,
    selector: &'static str,
) -> bool {
    draw(cx, window);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

fn press<T: ui_kit::Render + 'static>(
    cx: &mut TestAppContext,
    window: WindowHandle<T>,
    keys: &str,
) {
    cx.simulate_keystrokes(window.into(), keys);
    cx.run_until_parked();
}

fn type_text<T: ui_kit::Render + 'static>(
    cx: &mut TestAppContext,
    window: WindowHandle<T>,
    text: &str,
) {
    cx.simulate_input(window.into(), text);
    cx.run_until_parked();
}

fn focus<T: 'static>(cx: &mut TestAppContext, window: WindowHandle<T>, handle: gpui::FocusHandle) {
    window
        .update(cx, |_, window, cx| window.focus(&handle, cx))
        .unwrap();
    cx.run_until_parked();
}

/// The caret of a field with the keys, then the selection that Shift+Left leaves.
fn caret_and_selection<T: ui_kit::Render + 'static>(
    cx: &mut TestAppContext,
    window: WindowHandle<T>,
    caret: &'static str,
    selection: &'static str,
) {
    assert!(has(cx, window, caret), "the caret of {caret} is on screen");
    press(cx, window, "shift-left");
    assert!(has(cx, window, selection), "Shift+Left draws {selection}");
}

fn run_command(cx: &mut TestAppContext, window: WindowHandle<Workspace>, command: &'static str) {
    window
        .update(cx, |workspace, window, cx| {
            workspace.run_command(command, window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
}

#[ui_kit::gpui::test]
fn the_commit_message_has_a_caret_and_a_selection_on_its_own_line(cx: &mut TestAppContext) {
    let (root, _bare, _git) = crate::window_tests_git::repository_with_origin("field-commit");
    fs::write(root.join("main.txt"), "alpha\nbeta\ngamma\n").unwrap();
    let window = open_window(cx, root);
    cx.run_until_parked();
    let state = crate::window_tests_git::git_state(cx);
    crate::window_tests_git::wait_for_repository(cx, &state);

    let panel = cx.read(|cx| cx.global::<ActiveCommitPanel>().0.clone());
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(COMMIT_TOOL, window, cx)
        })
        .unwrap();
    draw(cx, window);
    let handle = panel.read_with(cx, |panel, _| panel.focus());
    focus(cx, window, handle);

    type_text(cx, window, "Fix the build");
    draw(cx, window);
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.message().to_owned()),
        "Fix the build"
    );
    let first = bounds_of(cx, window, "commit-message-caret").top();

    // Enter starts a second line, and the caret goes there with the text.
    press(cx, window, "enter");
    type_text(cx, window, "and the test");
    draw(cx, window);
    let second = bounds_of(cx, window, "commit-message-caret").top();
    assert!(
        second > first,
        "the caret of the second line is below the first one: {first:?} and {second:?}"
    );
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.message().to_owned()),
        "Fix the build\nand the test"
    );

    press(cx, window, "shift-left");
    assert!(
        has(cx, window, "commit-message-selection"),
        "Shift+Left marks the text before the caret"
    );
}

#[ui_kit::gpui::test]
fn the_question_of_a_prompt_has_a_caret_and_a_selection(cx: &mut TestAppContext) {
    let (root, _bare, _git) = crate::window_tests_git::repository_with_origin("field-prompt");
    fs::write(root.join("main.txt"), "alpha\nbeta\ngamma\n").unwrap();
    let window = open_window(cx, root);
    cx.run_until_parked();
    let state = crate::window_tests_git::git_state(cx);
    crate::window_tests_git::wait_for_repository(cx, &state);

    run_command(cx, window, "git.stash");
    crate::window_tests_git::wait_for(cx, "the stash question", |cx| has(cx, window, "prompt"));
    type_text(cx, window, "work in progress");
    caret_and_selection(cx, window, "prompt-field-caret", "prompt-field-selection");
    press(cx, window, "escape");
}

#[ui_kit::gpui::test]
fn the_merge_message_has_a_caret_and_a_selection(cx: &mut TestAppContext) {
    let (root, _bare, _git) = crate::window_tests_git::conflicting_repository("field-conflict");
    let (window, state) = crate::window_tests_git::start_conflicting_merge(cx, root);

    click(cx, window, "conflict-theirs-0");
    crate::window_tests_git::wait_for(cx, "the first side", |cx| {
        state.read_with(cx, |state, _| state.conflicts().len() == 1)
    });
    click(cx, window, "conflict-theirs-0");
    crate::window_tests_git::wait_for(cx, "every conflict resolved", |cx| {
        state.read_with(cx, |state, _| state.conflicts().is_empty())
            && has(cx, window, "conflict-commit")
    });

    type_text(cx, window, "merge the two");
    caret_and_selection(
        cx,
        window,
        "conflict-message-field-caret",
        "conflict-message-field-selection",
    );
}

#[ui_kit::gpui::test]
fn the_log_filter_has_a_caret_and_a_selection(cx: &mut TestAppContext) {
    let (root, _bare, _git) = crate::window_tests_git::repository_with_origin("field-log");
    let window = open_window(cx, root);
    cx.run_until_parked();
    let state = crate::window_tests_git::git_state(cx);
    crate::window_tests_git::wait_for_repository(cx, &state);

    press(cx, window, "alt-9");
    crate::window_tests_git::wait_for(cx, "the log", |cx| has(cx, window, "log-filter"));
    type_text(cx, window, "base");
    caret_and_selection(
        cx,
        window,
        "git-log-filter-caret",
        "git-log-filter-selection",
    );
}

#[ui_kit::gpui::test]
fn the_command_field_of_the_build_window_has_a_caret_and_a_selection(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "field-build");
    let panel = cx.read(|cx| cx.global::<crate::script::ActiveBuildPanel>().0.clone());
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(TASKS_TOOL, window, cx)
        })
        .unwrap();
    window
        .update(cx, |_, window, cx| {
            panel.update(cx, |panel, cx| panel.open_execute(window, cx))
        })
        .unwrap();
    draw(cx, window);
    assert!(
        panel.read_with(cx, |panel, _| panel.execute_open()),
        "the command popup is open"
    );
    type_text(cx, window, "clean install");
    caret_and_selection(
        cx,
        window,
        "build-execute-field-caret",
        "build-execute-field-selection",
    );
}

#[ui_kit::gpui::test]
fn the_name_of_the_change_signature_form_has_a_caret_and_a_selection(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "field-signature");
    let spot = lang_api::DocumentSpot {
        uri: "file:///signature".to_owned(),
        version: 0,
        point: lang_api::TextPoint {
            line: 0,
            character: 0,
        },
    };
    window
        .update(cx, |workspace, window, cx| {
            crate::change_signature::ChangeSignatureDialog::show(spot, workspace, window, cx);
        })
        .unwrap();
    draw(cx, window);
    assert!(
        crate::window_tests::bounds_of(cx, window, "change-signature")
            .size
            .height
            > ui_kit::px(0.),
        "the form is on screen"
    );

    type_text(cx, window, "renamed");
    caret_and_selection(
        cx,
        window,
        "change-signature-field-caret",
        "change-signature-field-selection",
    );
}

#[ui_kit::gpui::test]
fn the_name_of_the_copy_class_window_has_a_caret_and_a_selection(cx: &mut TestAppContext) {
    let root = sample_project("field-copy-class");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let source = root.join("src").join("main.txt");
    let folder = root.join("src");
    window
        .update(cx, |workspace, window, cx| {
            crate::copy_class::CopyClassDialog::show(
                &source,
                &folder,
                "Thing2",
                false,
                |_, _, _| {},
                workspace,
                window,
                cx,
            );
        })
        .unwrap();
    draw(cx, window);
    type_text(cx, window, "Thing3");
    caret_and_selection(
        cx,
        window,
        "copy-class-field-caret",
        "copy-class-field-selection",
    );
}

#[ui_kit::gpui::test]
fn the_settings_field_has_a_caret_and_a_selection(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "field-settings");
    press(cx, window, "ctrl-alt-s");
    draw(cx, window);
    click(cx, window, "settings-page-appearance");
    draw(cx, window);
    click(cx, window, "settings-editor-font-family");
    draw(cx, window);
    type_text(cx, window, "Mono");
    caret_and_selection(
        cx,
        window,
        "settings-field-caret",
        "settings-field-selection",
    );
}

#[ui_kit::gpui::test]
fn the_keymap_search_has_a_caret_and_a_selection(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "field-keymap");
    press(cx, window, "ctrl-alt-s");
    draw(cx, window);
    click(cx, window, "settings-page-keymap");
    draw(cx, window);
    type_text(cx, window, "file");
    caret_and_selection(
        cx,
        window,
        "keymap-search-field-caret",
        "keymap-search-field-selection",
    );
}

#[ui_kit::gpui::test]
fn the_template_fields_have_a_caret_and_a_selection(cx: &mut TestAppContext) {
    let root = sample_project("field-templates");
    let window = crate::window_tests_templates_settings::open_templates_window(cx, root);
    crate::window_tests_templates_settings::open_page(cx, window, "settings-page-live-templates");
    click(cx, window, "template-add");
    draw(cx, window);
    click(cx, window, "template-abbreviation");
    draw(cx, window);
    type_text(cx, window, "mine");
    caret_and_selection(
        cx,
        window,
        "template-field-caret",
        "template-field-selection",
    );
}

#[ui_kit::gpui::test]
fn the_inspection_filter_has_a_caret_and_a_selection(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "field-inspect");
    press(cx, window, "ctrl-alt-shift-i");
    draw(cx, window);
    assert!(
        crate::window_tests::bounds_of(cx, window, "run-inspection")
            .size
            .height
            > ui_kit::px(0.),
        "Run Inspection by Name shows its list"
    );
    type_text(cx, window, "unused");
    caret_and_selection(
        cx,
        window,
        "run-inspection-field-caret",
        "run-inspection-field-selection",
    );
}

#[ui_kit::gpui::test]
fn the_region_filter_has_a_caret_and_a_selection(cx: &mut TestAppContext) {
    let (_path, window, _view) = crate::window_tests_folding::open_text(
        cx,
        "field-regions",
        "//region First\none();\n//endregion\n",
    );
    press(cx, window, "ctrl-alt-.");
    draw(cx, window);
    crate::window_tests_git::wait_for(cx, "the region list", |cx| {
        has(cx, window, "goto-region-popup")
    });
    type_text(cx, window, "First");
    caret_and_selection(
        cx,
        window,
        "region-query-field-caret",
        "region-query-field-selection",
    );
}

#[ui_kit::gpui::test]
fn the_start_screen_fields_have_a_caret_and_a_selection(cx: &mut TestAppContext) {
    let root = sample_project("field-start");
    let templates = vec![ProjectTemplate {
        label: "Plain",
        files: |_| Vec::new(),
    }];
    let screen = cx.add_window(|window, cx| {
        cx.set_global(ui_kit::Theme::dark());
        StartScreen::new_for_test(
            vec![root.clone()],
            scratch("field-start-state"),
            templates,
            window,
            cx,
        )
    });
    cx.run_until_parked();
    draw(cx, screen);
    click(cx, screen, "start-new");
    draw(cx, screen);
    type_text(cx, screen, "demo");
    caret_and_selection(cx, screen, "start-field-caret", "start-field-selection");
}

// The program of this test is powershell.exe, as in `window_tests_console_input`.
#[cfg(windows)]
#[ui_kit::gpui::test]
fn the_console_input_has_a_caret_and_a_selection(cx: &mut TestAppContext) {
    let root = sample_project("field-console");
    let window = crate::window_tests_console_input::open(cx, &root);
    let panel = crate::window_tests_run::run_panel(cx);
    crate::window_tests_console_input::started(cx, window, &panel);
    type_text(cx, window, "hello");
    caret_and_selection(
        cx,
        window,
        "console-input-field-caret",
        "console-input-field-selection",
    );
}
