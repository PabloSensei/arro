//! The stable waits of the window tests of M11, Save As with the format of the buffer,
//! a copy pasted beside its source and Expand All in the hierarchy windows. Every wait
//! here ends on a signal of the work itself, not on a clock.

use std::fs;

use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::Workspace;

use crate::call_hierarchy::ActiveCallHierarchy;
use crate::index::ActiveIndex;
use crate::method_hierarchy::ActiveMethodHierarchy;
use crate::project::ActiveProjectPanel;
use crate::window_tests::{
    active_path, draw, editor_of, open_path, open_sample_window, open_window, press,
    wait_for_outline_signal, wait_for_parse_signal, wait_for_project_index, wait_for_project_tree,
    wait_for_signal,
};
use crate::window_tests_hierarchy::{
    HierarchyLanguage, open_window as open_hierarchy_window, project,
};
use crate::window_tests_safe_delete::{
    FakeEditLanguage, open_window as open_safe_delete_window, project as safe_delete_project,
    span_in, uri_of,
};
use crate::window_tests_ui::{navigation_project, open_test_window, wait_for_symbols};

fn project_panel(cx: &TestAppContext) -> gpui::Entity<crate::project::ProjectPanel> {
    cx.read(|cx| cx.global::<ActiveProjectPanel>().0.clone())
}

fn focus_project(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    crate::window_tests_menus::show_project(cx, window);
    let panel = project_panel(cx);
    window
        .update(cx, |_, window, cx| {
            window.focus(&panel.read(cx).focus(), cx);
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
}

#[ui_kit::gpui::test]
fn the_index_raises_its_generation_before_the_palette_searches(cx: &mut TestAppContext) {
    let root = navigation_project("stable-index");
    let window = open_test_window(cx, root.clone());
    cx.run_until_parked();
    draw(cx, window);

    wait_for_project_index(cx);

    let (files, symbols) = cx.read(|cx| {
        let index = &cx.global::<ActiveIndex>().0;
        (index.files_generation(), index.symbols_generation())
    });
    assert!(files >= 1, "the list of files reached the index");
    assert!(symbols >= 1, "the symbols reached the index");

    // The palette finds the class that the signal promised.
    let sample = root.join("src").join("Sample.txt");
    wait_for_symbols(cx, window, &sample);
    assert_eq!(active_path(cx, window), Some(sample));
}

#[ui_kit::gpui::test]
fn the_tree_and_the_parser_signal_their_progress(cx: &mut TestAppContext) {
    let root = navigation_project("stable-signals");
    let sample = root.join("src").join("Sample.txt");
    let window = open_test_window(cx, root);
    cx.run_until_parked();
    draw(cx, window);

    wait_for_project_tree(cx);
    let rows = cx.read(|cx| {
        cx.global::<ActiveProjectPanel>()
            .0
            .read(cx)
            .tree()
            .rows()
            .len()
    });
    assert!(rows > 0, "the root of the project has its rows");

    open_path(cx, window, &sample);
    wait_for_parse_signal(cx, window, &sample);
    wait_for_outline_signal(cx, window, &sample);

    let view = editor_of(cx, window, &sample);
    assert!(
        view.read_with(cx, |view, _| view.syntax_generation()) > 0,
        "the parser handed its tree to the view"
    );
    assert!(
        !view.read_with(cx, |view, _| view.outline().is_empty()),
        "the outline of the file is there"
    );
}

#[ui_kit::gpui::test]
fn save_as_writes_the_format_of_the_buffer_and_reports_a_failure(cx: &mut TestAppContext) {
    let root = crate::window_tests::scratch("stable-save-as");
    let source = root.join("source.txt");
    let text = b"one\r\ntwo\r\n";
    fs::write(&source, text).unwrap();
    let window = open_window(cx, root.clone());
    draw(cx, window);
    open_path(cx, window, &source);
    let view = editor_of(cx, window, &source);
    wait_for_signal(cx, "the loaded tab", |cx| {
        view.read_with(cx, |view, _| view.is_loaded())
    });

    // Save As keeps the line ends of the buffer, not the ones of the platform.
    let target = root.join("copy.txt");
    press(cx, window, "ctrl-shift-s");
    cx.simulate_new_path_selection({
        let target = target.clone();
        move |_| Some(target.clone())
    });
    wait_for_signal(cx, "the saved file", |_| target.is_file());
    assert_eq!(fs::read(&target).unwrap(), text, "the line ends are kept");
    wait_for_signal(cx, "the new tab", |cx| {
        active_path(cx, window).as_deref() == Some(target.as_path())
    });

    // A path that cannot be written shows a notification and no tab.
    let notifications = window
        .read_with(cx, |workspace, _| workspace.notification_serial())
        .unwrap();
    let missing = root.join("missing").join("copy.txt");
    press(cx, window, "ctrl-shift-s");
    cx.simulate_new_path_selection({
        let missing = missing.clone();
        move |_| Some(missing.clone())
    });
    wait_for_signal(cx, "the failed save", |cx| {
        window
            .read_with(cx, |workspace, _| workspace.notification_serial())
            .unwrap()
            > notifications
    });
    assert!(!missing.exists(), "the failed save writes nothing");
}

#[ui_kit::gpui::test]
fn a_copy_pasted_beside_its_source_gets_a_numbered_name(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "stable-paste");
    let main = root.join("src").join("main.txt");
    let panel = project_panel(cx);

    panel.update(cx, |panel, cx| panel.select_path(&main, cx));
    focus_project(cx, window);
    press(cx, window, "ctrl-c");
    press(cx, window, "ctrl-v");
    wait_for_signal(cx, "the first copy", |_| {
        root.join("src").join("main_1.txt").is_file()
    });

    press(cx, window, "ctrl-v");
    wait_for_signal(cx, "the second copy", |_| {
        root.join("src").join("main_2.txt").is_file()
    });

    // A cut into the same folder does nothing; the next copy takes `_3`, so no copy
    // of the cut made a name in between.
    panel.update(cx, |panel, cx| panel.select_path(&main, cx));
    focus_project(cx, window);
    press(cx, window, "ctrl-x");
    press(cx, window, "ctrl-v");
    cx.run_until_parked();
    assert!(main.is_file(), "the cut source stays in place");
    assert!(
        !root.join("src").join("main_3.txt").exists(),
        "the cut into the same folder makes no copy"
    );

    press(cx, window, "ctrl-c");
    press(cx, window, "ctrl-v");
    wait_for_signal(cx, "the third copy", |_| {
        root.join("src").join("main_3.txt").is_file()
    });
}

fn call_item(name: &str, path: &std::path::Path, text: &str) -> lang_api::CallHierarchyItem {
    lang_api::CallHierarchyItem {
        name: name.to_owned(),
        detail: None,
        place: Some(lang_api::SourceSpan {
            uri: crate::window_tests_lang::uri_of(path),
            span: crate::window_tests_lang::span_in(text, 0, text.len()),
        }),
        handle: name.to_owned(),
    }
}

#[ui_kit::gpui::test]
fn expand_all_loads_the_call_hierarchy_down_to_five_levels(cx: &mut TestAppContext) {
    let text = "callee\n";
    let root = project("stable-call-deep", &[("src/Calls.txt", text)]);
    let path = root.join("src").join("Calls.txt");
    let language = HierarchyLanguage::new();
    *language.call_roots.lock().unwrap() = vec![call_item("root", &path, text)];
    *language.calls.lock().unwrap() = vec![call_item("caller", &path, text)];
    let window = open_hierarchy_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    press(cx, window, "ctrl-alt-h");
    draw(cx, window);

    let panel = cx.update(|cx| cx.global::<ActiveCallHierarchy>().0.clone());
    wait_for_signal(cx, "the roots of the call hierarchy", |cx| {
        panel.read_with(cx, |panel, _| panel.lines().len()) >= 2
    });

    panel.update(cx, |panel, cx| panel.expand_all(cx));

    wait_for_signal(cx, "the fifth level of the call hierarchy", |cx| {
        panel
            .read_with(cx, |panel, _| {
                panel
                    .lines()
                    .iter()
                    .map(|line| line.chars().take_while(|c| *c == ' ').count())
                    .max()
            })
            .is_some_and(|depth| depth >= 10)
    });
    let lines = panel.read_with(cx, |panel, _| panel.lines());
    assert_eq!(
        lines.len(),
        6,
        "the root and five levels below it: {lines:?}"
    );
}

#[ui_kit::gpui::test]
fn expand_all_asks_the_language_for_the_whole_method_hierarchy(cx: &mut TestAppContext) {
    let text = "interface Shape {\n    double area();\n}\n";
    let root = safe_delete_project("stable-method-deep", &[("src/Shape.txt", text)]);
    let path = root.join("src").join("Shape.txt");
    let language = FakeEditLanguage::new();
    *language.hierarchy.lock().unwrap() = vec![lang_api::MethodHierarchyEntry {
        type_name: "Shape".to_owned(),
        method: "area".to_owned(),
        relation: lang_api::MethodRelation::Defines,
        place: Some(lang_api::SourceSpan {
            uri: uri_of(&path),
            span: span_in(
                text,
                text.find("Shape").unwrap(),
                text.find("Shape").unwrap() + 5,
            ),
        }),
    }];
    let window = open_safe_delete_window(cx, root.clone(), language.clone());
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    view.update(cx, |view, cx| view.restore_caret(1, 10, cx));
    press(cx, window, "ctrl-shift-h");
    draw(cx, window);

    let panel = cx.update(|cx| cx.global::<ActiveMethodHierarchy>().0.clone());
    wait_for_signal(cx, "the method hierarchy", |cx| {
        !panel.read_with(cx, |panel, cx| panel.panel().read(cx).items().is_empty())
    });
    let before = *language.hierarchy_requests.lock().unwrap();

    panel.update(cx, |panel, cx| panel.expand_all(cx));

    wait_for_signal(cx, "the reloaded method hierarchy", |cx| {
        *language.hierarchy_requests.lock().unwrap() > before
            && !panel.read_with(cx, |panel, cx| panel.panel().read(cx).items().is_empty())
    });
}
