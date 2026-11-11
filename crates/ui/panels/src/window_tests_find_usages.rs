//! Find Usages: the dialog and the header of the Usages window, driven by a language
//! of the test, and two searches on the real language server.

use std::time::Duration;

use lang_api::{TextPoint, UsageKind};
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::{USAGES_TOOL, Workspace};

use crate::window_tests::{click, draw, editor_of, open_path, press, project_setup, scratch};
use crate::window_tests_lang::project;
use crate::window_tests_ui::wait_for;
use crate::window_tests_usages_tabs::{
    UsageLanguage, class, counts, field, hub, labels, open_language_window, tabs, title, usage,
    visible,
};

fn usages_open(cx: &TestAppContext, window: WindowHandle<Workspace>) -> bool {
    window
        .read_with(cx, |workspace, _| workspace.tool_open(USAGES_TOOL))
        .unwrap()
}

#[ui_kit::gpui::test]
fn find_enter_and_cancel_of_the_dialog(cx: &mut TestAppContext) {
    let text = "class Owner {\n}\n";
    let root = project(
        "find-usages-dialog-keys",
        &[("src/Main.txt", text), ("src/Other.txt", "Owner\n")],
    );
    let main = root.join("src/Main.txt");
    let other = root.join("src/Other.txt");
    let language = UsageLanguage::new();
    language.set_symbols(vec![class("Owner", text, Vec::new())]);
    language.set_usages_at(
        TextPoint {
            line: 0,
            character: 7,
        },
        vec![usage(&other, "Owner\n", 0, 5, UsageKind::Read)],
    );
    let window = open_language_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &main);
    let view = editor_of(cx, window, &main);
    view.update(cx, |view, cx| view.restore_caret(0, 7, cx));
    draw(cx, window);

    press(cx, window, "ctrl-alt-shift-f7");
    draw(cx, window);
    wait_for(cx, "the classes of the dialog", |cx| {
        visible(cx, window, "find-usages-check-usages")
    });
    press(cx, window, "escape");
    draw(cx, window);
    assert!(
        !visible(cx, window, "find-usages-settings"),
        "Escape closes the dialog"
    );
    assert!(!usages_open(cx, window), "Escape does not start the search");

    press(cx, window, "ctrl-alt-shift-f7");
    draw(cx, window);
    wait_for(cx, "the dialog again", |cx| {
        visible(cx, window, "find-usages-cancel")
    });
    click(cx, window, "find-usages-cancel");
    draw(cx, window);
    assert!(
        !visible(cx, window, "find-usages-settings"),
        "Cancel closes the dialog"
    );
    assert!(!usages_open(cx, window), "Cancel does not start the search");

    press(cx, window, "ctrl-alt-shift-f7");
    draw(cx, window);
    wait_for(cx, "the dialog before Enter", |cx| {
        visible(cx, window, "find-usages-find")
    });
    press(cx, window, "enter");
    wait_for(cx, "the search of Enter", |cx| {
        usages_open(cx, window) && counts(cx) == 1
    });
    draw(cx, window);
    assert!(!visible(cx, window, "find-usages-settings"));
    assert_eq!(labels(cx), vec!["Owner".to_owned()]);
}

#[ui_kit::gpui::test]
fn a_variable_has_readers_and_writers_instead_of_usages(cx: &mut TestAppContext) {
    let text = "class Thing {\n  int value;\n}\n";
    let root = project(
        "find-usages-variable",
        &[
            ("src/Main.txt", text),
            ("src/Read.txt", "value\n"),
            ("src/Write.txt", "value\n"),
        ],
    );
    let main = root.join("src/Main.txt");
    let reader = root.join("src/Read.txt");
    let writer = root.join("src/Write.txt");
    let language = UsageLanguage::new();
    language.set_symbols(vec![class(
        "Thing",
        text,
        vec![field(
            "value",
            text,
            text.find("value").unwrap(),
            text.find("value").unwrap() + 5,
        )],
    )]);
    language.set_usages(vec![
        usage(&reader, "value\n", 0, 5, UsageKind::Read),
        usage(&writer, "value\n", 0, 5, UsageKind::Write),
    ]);
    let window = open_language_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &main);
    let view = editor_of(cx, window, &main);
    view.update(cx, |view, cx| view.restore_caret(1, 6, cx));
    draw(cx, window);

    press(cx, window, "ctrl-alt-shift-f7");
    draw(cx, window);
    wait_for(cx, "the readers of the field", |cx| {
        visible(cx, window, "find-usages-check-readers")
    });
    assert!(
        !visible(cx, window, "find-usages-usages"),
        "a field has no Usages flag"
    );
    assert!(visible(cx, window, "find-usages-check-writers"));

    click(cx, window, "find-usages-readers");
    click(cx, window, "find-usages-find");
    wait_for(cx, "the Usages window", |cx| usages_open(cx, window));
    draw(cx, window);
    assert_eq!(counts(cx), 1, "only the writers are left");
    let items = hub(cx).read_with(cx, |hub, cx| hub.panel().read(cx).items().to_vec());
    assert_eq!(items[0].path, writer, "the readers are filtered out");
    assert!(title(cx).contains("fields"), "{}", title(cx));
}

#[ui_kit::gpui::test]
fn the_scope_of_the_dialog_narrows_the_result(cx: &mut TestAppContext) {
    let text = "alpha\n";
    let root = project(
        "find-usages-scope",
        &[("src/Main.txt", text), ("src/Other.txt", "alpha\n")],
    );
    let main = root.join("src/Main.txt");
    let other = root.join("src/Other.txt");
    let language = UsageLanguage::new();
    language.set_usages(vec![
        usage(&main, text, 0, 5, UsageKind::Read),
        usage(&other, "alpha\n", 0, 5, UsageKind::Read),
    ]);
    let window = open_language_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &main);
    let view = editor_of(cx, window, &main);
    view.update(cx, |view, cx| view.restore_caret(0, 0, cx));
    draw(cx, window);

    press(cx, window, "ctrl-alt-shift-f7");
    draw(cx, window);
    wait_for(cx, "the scopes of the dialog", |cx| {
        visible(cx, window, "find-usages-scope-3")
    });
    click(cx, window, "find-usages-scope-3");
    click(cx, window, "find-usages-find");
    wait_for(cx, "the search of the current file", |cx| {
        usages_open(cx, window) && counts(cx) == 1
    });
    draw(cx, window);
    let items = hub(cx).read_with(cx, |hub, cx| hub.panel().read(cx).items().to_vec());
    assert_eq!(items[0].path, main, "only the current file is left");
}

#[ui_kit::gpui::test]
fn open_in_new_tab_adds_a_tab_to_the_usages_window(cx: &mut TestAppContext) {
    let text = "alpha beta\n";
    let root = project(
        "find-usages-new-tab",
        &[("src/Main.txt", text), ("src/Other.txt", "alpha\n")],
    );
    let main = root.join("src/Main.txt");
    let other = root.join("src/Other.txt");
    let language = UsageLanguage::new();
    language.set_usages_at(
        TextPoint {
            line: 0,
            character: 0,
        },
        vec![usage(&other, "alpha\n", 0, 5, UsageKind::Read)],
    );
    language.set_usages_at(
        TextPoint {
            line: 0,
            character: 6,
        },
        vec![usage(&other, "alpha\n", 0, 5, UsageKind::Read)],
    );
    let window = open_language_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &main);
    let view = editor_of(cx, window, &main);

    view.update(cx, |view, cx| view.restore_caret(0, 0, cx));
    draw(cx, window);
    press(cx, window, "alt-f7");
    wait_for(cx, "the first tab", |cx| tabs(cx).len() == 1);

    view.update(cx, |view, cx| view.restore_caret(0, 6, cx));
    window
        .update(cx, |workspace, window, cx| {
            workspace.focus_editor(window, cx)
        })
        .unwrap();
    draw(cx, window);
    press(cx, window, "ctrl-alt-shift-f7");
    draw(cx, window);
    wait_for(cx, "the new tab flag", |cx| {
        visible(cx, window, "find-usages-new-tab")
    });
    click(cx, window, "find-usages-new-tab");
    click(cx, window, "find-usages-find");
    wait_for(cx, "the second tab", |cx| tabs(cx).len() == 2);
    assert_eq!(tabs(cx).len(), 2, "the search opened another tab");
}

#[ui_kit::gpui::test]
fn the_header_buttons_have_icons_and_tooltips(cx: &mut TestAppContext) {
    let text = "alpha\n";
    let root = project(
        "find-usages-header",
        &[("src/Main.txt", text), ("src/Other.txt", "alpha\n")],
    );
    let main = root.join("src/Main.txt");
    let other = root.join("src/Other.txt");
    let language = UsageLanguage::new();
    language.set_usages(vec![usage(&other, "alpha\n", 0, 5, UsageKind::Read)]);
    let window = open_language_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &main);
    let view = editor_of(cx, window, &main);
    view.update(cx, |view, cx| view.restore_caret(0, 0, cx));
    draw(cx, window);

    press(cx, window, "alt-f7");
    wait_for(cx, "the header of the Usages window", |cx| {
        usages_open(cx, window) && counts(cx) > 0
    });
    draw(cx, window);
    let buttons = hub(cx).read_with(cx, |hub, cx| hub.panel().read(cx).header_buttons().to_vec());
    assert_eq!(buttons.len(), 9);
    for button in &buttons {
        assert!(button.icon.is_some(), "{} has an icon", button.id);
        assert!(
            button
                .tooltip
                .as_deref()
                .is_some_and(|tooltip| !tooltip.is_empty()),
            "{} has a tooltip",
            button.id
        );
        assert!(
            button.label.chars().count() > 1,
            "{} is not a single letter: {}",
            button.id,
            button.label
        );
    }
    let tooltips: Vec<&str> = buttons
        .iter()
        .filter_map(|button| button.tooltip.as_deref())
        .collect();
    for expected in [
        "Group by Type",
        "Group by Module",
        "Group by Package",
        "Group by Directory Structure",
        "Group by File Structure",
        "Flatten Modules",
        "Show Read Access",
        "Show Write Access",
        "Show Imports",
    ] {
        assert!(
            tooltips.contains(&expected),
            "the tooltip {expected} is there: {tooltips:?}"
        );
    }
}

/// The line of the probe that has `needle`.
fn probe_line(
    cx: &TestAppContext,
    view: &gpui::Entity<editor_view::EditorView>,
    needle: &str,
) -> usize {
    crate::window_tests::buffer_of(cx, view)
        .lines()
        .position(|line| line.contains(needle))
        .expect("the probe has the line")
}

/// The column of `name` in the line of the probe that has `needle`.
fn probe_column(
    cx: &TestAppContext,
    view: &gpui::Entity<editor_view::EditorView>,
    line: usize,
    name: &str,
) -> usize {
    crate::window_tests::buffer_of(cx, view)
        .lines()
        .nth(line)
        .and_then(|line| line.find(name))
        .expect("the line has the name")
}

#[ui_kit::gpui::test]
#[ignore = "needs IDE_CORPUS and a filled package cache"]
fn a_local_variable_has_all_its_reads_and_writes(cx: &mut TestAppContext) {
    use crate::window_tests_real::{
        build_window, close, project as real_project, wait_for as wait_long, wait_ready,
    };

    let (root, language) = real_project("find-usages-local");
    let source = "package probe;\npublic class LocalProbe {\n    int sum(int start) {\n        int total = start;\n        total = total + 1;\n        return total;\n    }\n}\n";
    let probe = window_language::write_probe(&root, "probe", "LocalProbe", source);
    let window = build_window(cx, root.clone(), &language, project_setup(scratch("cache")));
    cx.run_until_parked();
    draw(cx, window);
    wait_ready(cx, window);

    open_path(cx, window, &probe);
    let view = editor_of(cx, window, &probe);
    let line = probe_line(cx, &view, "int total = start;");
    let name = probe_column(cx, &view, line, "total");
    view.update(cx, |view, cx| view.restore_caret(line, name, cx));
    window
        .update(cx, |workspace, window, cx| {
            workspace.focus_editor(window, cx)
        })
        .unwrap();
    draw(cx, window);

    press(cx, window, "ctrl-alt-shift-f7");
    draw(cx, window);
    wait_long(
        cx,
        "the readers of the local variable",
        Duration::from_secs(120),
        |cx| {
            visible(cx, window, "find-usages-check-readers")
                && !visible(cx, window, "find-usages-usages")
        },
    );
    press(cx, window, "escape");
    draw(cx, window);

    press(cx, window, "alt-f7");
    wait_long(
        cx,
        "the usages of the local variable",
        Duration::from_secs(120),
        |cx| usages_open(cx, window) && counts(cx) >= 2,
    );
    draw(cx, window);
    click(cx, window, "item-button-usage-group-type");
    draw(cx, window);
    let groups = hub(cx).read_with(cx, |hub, cx| hub.group_labels(cx));
    assert!(groups.iter().any(|label| label == "Read"), "{groups:?}");
    assert!(groups.iter().any(|label| label == "Write"), "{groups:?}");

    close(cx, window, language, true);
}

#[ui_kit::gpui::test]
#[ignore = "needs IDE_CORPUS and a filled package cache"]
fn a_call_in_a_closed_file_is_a_call_not_other(cx: &mut TestAppContext) {
    use crate::window_tests_real::{
        build_window, close, project as real_project, wait_for as wait_long, wait_ready,
    };

    let (root, language) = real_project("find-usages-closed-call");
    let source = "package probe;\npublic class ClosedProbe {\n    int sum(int left, int right) {\n        return left + right;\n    }\n}\n";
    let caller = "package probe;\npublic class ClosedCaller {\n    int call(ClosedProbe probe) {\n        return probe.sum(1, 2);\n    }\n}\n";
    let probe = window_language::write_probe(&root, "probe", "ClosedProbe", source);
    let called = window_language::write_probe(&root, "probe", "ClosedCaller", caller);
    let window = build_window(cx, root.clone(), &language, project_setup(scratch("cache")));
    cx.run_until_parked();
    draw(cx, window);
    wait_ready(cx, window);

    open_path(cx, window, &probe);
    let view = editor_of(cx, window, &probe);
    let line = probe_line(cx, &view, "int sum(int left");
    let name = probe_column(cx, &view, line, "sum");
    view.update(cx, |view, cx| view.restore_caret(line, name, cx));
    window
        .update(cx, |workspace, window, cx| {
            workspace.focus_editor(window, cx)
        })
        .unwrap();
    draw(cx, window);

    press(cx, window, "alt-f7");
    wait_long(
        cx,
        "the usages of the method",
        Duration::from_secs(120),
        |cx| usages_open(cx, window) && counts(cx) > 0,
    );
    draw(cx, window);
    click(cx, window, "item-button-usage-group-type");
    draw(cx, window);
    let groups = hub(cx).read_with(cx, |hub, cx| hub.group_labels(cx));
    let paths: Vec<std::path::PathBuf> = hub(cx)
        .read_with(cx, |hub, cx| hub.panel().read(cx).items().to_vec())
        .into_iter()
        .map(|item| item.path)
        .collect();
    assert!(groups.iter().any(|label| label == "Call"), "{groups:?}");
    assert!(
        !groups.iter().any(|label| label == "Other"),
        "the call in the closed file has a kind: {groups:?}"
    );
    assert!(
        paths.iter().any(|path| path == &called),
        "the closed file is in the list: {paths:?}"
    );
    assert!(
        paths.iter().all(|path| path != &probe),
        "the declaration is left out: {paths:?}"
    );

    close(cx, window, language, true);
}
