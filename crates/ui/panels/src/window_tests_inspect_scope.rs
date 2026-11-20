//! Inspect Code of a directory selected in the project tree, and the export of its
//! results, on the memory language of the reformat scope tests.

use std::fs;

use lang_api::Severity;
use ui_kit::gpui;
use ui_kit::gpui::TestAppContext;
use workspace::INSPECTION_TOOL;

use crate::inspect::{ActiveInspect, Finding, InspectHub, Mode, Scope};
use crate::window_tests::{
    click, draw, press, project_setup, scratch, wait_for_file_index, wait_for_signal,
};
use crate::window_tests_reformat_scope::{
    ScopeLanguage, focus_project, note, open_window_with, project, project_panel, show_inspection,
    visible,
};

fn hub(cx: &TestAppContext) -> gpui::Entity<InspectHub> {
    cx.read(|cx| cx.global::<ActiveInspect>().0.clone())
}

fn findings(cx: &TestAppContext) -> Vec<Finding> {
    hub(cx).read_with(cx, |hub, _| hub.findings().to_vec())
}

#[ui_kit::gpui::test]
fn inspect_code_of_a_directory_checks_only_the_files_of_that_directory(cx: &mut TestAppContext) {
    let root = project(
        "inspect-directory",
        &[
            ("src/One.txt", "one\n"),
            ("src/Two.txt", "two\n"),
            ("other/Three.txt", "three\n"),
        ],
    );
    let src = root.join("src");
    let one = src.join("One.txt");
    let three = root.join("other").join("Three.txt");
    let language = ScopeLanguage::new();
    language.set_note(&one, note(0, Severity::Warning, "The name is never used"));
    language.set_note(&three, note(0, Severity::Error, "The value is never used"));
    let window = open_window_with(
        cx,
        root.clone(),
        language,
        project_setup(scratch("inspect-directory-cache")),
    );
    cx.run_until_parked();
    draw(cx, window);
    wait_for_file_index(cx);

    let panel = project_panel(cx);
    panel.update(cx, |panel, cx| panel.select_path(&src, cx));
    focus_project(cx, window, &panel);

    press(cx, window, "alt-shift-i");
    draw(cx, window);
    assert!(
        visible(cx, window, "inspect-scope-popup"),
        "Alt+Shift+I asks for the scope"
    );
    assert!(
        visible(cx, window, "inspect-scope-run"),
        "the dialog has its Inspect button"
    );
    assert!(visible(cx, window, "inspect-scope-cancel"));
    press(cx, window, "down down down");
    draw(cx, window);
    click(cx, window, "inspect-scope-run");
    wait_for_signal(cx, "the inspection of the folder", |cx| {
        hub(cx).read_with(cx, |hub, _| hub.can_rerun())
    });

    let all = findings(cx);
    assert_eq!(all.len(), 1, "{all:?}");
    assert_eq!(
        all[0].path, one,
        "only the file of the directory is checked"
    );
    assert!(
        window
            .read_with(cx, |workspace, _| workspace.tool_open(INSPECTION_TOOL))
            .unwrap(),
        "the results open in their window"
    );
}

#[ui_kit::gpui::test]
fn export_writes_the_findings_of_two_files(cx: &mut TestAppContext) {
    let root = project(
        "inspect-export",
        &[("src/One.txt", "one\n"), ("src/Two.txt", "two\n")],
    );
    let one = root.join("src").join("One.txt");
    let two = root.join("src").join("Two.txt");
    let language = ScopeLanguage::new();
    language.set_note(&one, note(0, Severity::Warning, "The name is never used"));
    language.set_note(&two, note(1, Severity::Error, "The value is never used"));
    let window = open_window_with(
        cx,
        root.clone(),
        language,
        project_setup(scratch("inspect-export-cache")),
    );
    cx.run_until_parked();
    draw(cx, window);
    show_inspection(cx, window);
    hub(cx).update(cx, |hub, cx| {
        hub.start(
            Mode::Inspect(None),
            Scope::Files(vec![one.clone(), two.clone()]),
            cx,
        )
    });
    wait_for_signal(cx, "both findings", |cx| findings(cx).len() == 2);
    draw(cx, window);

    click(cx, window, "inspection-export");
    draw(cx, window);
    assert!(
        visible(cx, window, "menu-0"),
        "Export offers the two formats"
    );
    let target = root.join("inspection-report.txt");
    click(cx, window, "menu-1");
    cx.simulate_new_path_selection({
        let target = target.clone();
        move |_| Some(target.clone())
    });
    wait_for_signal(cx, "the exported report", |_| target.is_file());

    let text = fs::read_to_string(&target).unwrap();
    assert!(text.contains("2 problems"), "{text}");
    assert!(text.contains("src/One.txt"), "{text}");
    assert!(text.contains("src/Two.txt"), "{text}");
    assert!(
        text.contains("line 1: Warning: The name is never used"),
        "{text}"
    );
    assert!(
        text.contains("line 2: Error: The value is never used"),
        "{text}"
    );
}
