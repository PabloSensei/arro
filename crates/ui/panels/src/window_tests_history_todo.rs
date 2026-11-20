//! Local History snapshots of unsaved work and of a refactoring, labels in the history of a
//! file, the search of its window, and the patterns and filters of the TODO tool window.

use std::fs;
use std::path::Path;
use std::time::Duration;

use local_history::Cause;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::{Settings, TODO_TOOL, Workspace};

use crate::window_tests::{
    click, draw, editor_of, open_path, open_window, press, sample_project, scratch, type_text,
    wait_for_signal,
};
use crate::window_tests_lang::{FakeLanguage, open_window_with, project};

fn cause_of(cx: &mut TestAppContext, path: &Path, cause: Cause) -> bool {
    cx.update(|cx| {
        crate::history::get(cx).is_some_and(|history| {
            history
                .file_history(path)
                .iter()
                .any(|revision| revision.cause == cause)
        })
    })
}

fn run_command(cx: &mut TestAppContext, window: WindowHandle<Workspace>, id: &str) {
    let ran = window
        .update(cx, |workspace, window, cx| {
            workspace.run_command(id, window, cx)
        })
        .unwrap();
    assert!(ran, "{id} is not a registered command");
    cx.run_until_parked();
    draw(cx, window);
}

fn version_lines(cx: &mut TestAppContext) -> Vec<String> {
    cx.update(|cx| crate::history_view::report(cx))
        .into_iter()
        .filter(|line| line.starts_with("history-version"))
        .collect()
}

#[gpui::test]
fn window_tests_history_auto_snapshot_records_an_unsaved_change(cx: &mut TestAppContext) {
    // The interval of the window is a short one, so the test reads the snapshot it makes
    // without waiting for minutes of the real clock.
    cx.update(|cx| {
        cx.set_global(crate::history::AutoSnapshotInterval(Duration::from_millis(
            10,
        )))
    });
    let root = sample_project("history-auto");
    let path = root.join("src").join("main.txt");
    let window = open_window(cx, root.clone());
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    view.update(cx, |view, cx| view.replace_text(0..5, "changed", cx));

    wait_for_signal(cx, "the automatic snapshot", |cx| {
        cause_of(cx, &path, Cause::Auto)
    });

    let snapshots = cx.update(|cx| {
        crate::history::get(cx)
            .map(|history| history.file_history(&path))
            .unwrap_or_default()
    });
    let auto = snapshots
        .iter()
        .find(|revision| revision.cause == Cause::Auto)
        .expect("the unsaved text is recorded");
    assert_eq!(auto.cause.title(), "Auto");
    assert!(
        snapshots
            .iter()
            .filter(|revision| revision.cause == Cause::Auto)
            .count()
            == 1,
        "a text that did not change adds no second snapshot"
    );
}

#[gpui::test]
fn window_tests_history_refactoring_leaves_a_snapshot_with_its_name(cx: &mut TestAppContext) {
    let root = project(
        "history-refactoring",
        &[
            ("src/Open.txt", "alpha beta\n"),
            ("src/Closed.txt", "alpha gamma\n"),
        ],
    );
    let open = root.join("src").join("Open.txt");
    let closed = root.join("src").join("Closed.txt");
    let language = FakeLanguage::new();
    language.set_closed(closed.clone());
    let window = open_window_with(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &open);
    let view = editor_of(cx, window, &open);

    press(cx, window, "shift-f6");
    draw(cx, window);
    press(
        cx,
        window,
        "backspace backspace backspace backspace backspace",
    );
    type_text(cx, window, "delta");
    press(cx, window, "enter");
    draw(cx, window);
    press(cx, window, "enter");
    cx.run_until_parked();
    draw(cx, window);
    assert_eq!(crate::window_tests::buffer_of(cx, &view), "delta beta\n");

    wait_for_signal(cx, "the refactoring snapshot", |cx| {
        cause_of(cx, &closed, Cause::BeforeRefactoring)
    });
    assert!(
        cause_of(cx, &closed, Cause::BeforeRefactoring),
        "the closed file keeps the text the refactoring replaced"
    );

    open_path(cx, window, &closed);
    run_command(cx, window, "local_history.show");
    let lines = version_lines(cx);
    assert!(
        lines.iter().any(|line| line.contains("Refactoring")),
        "the history names the refactoring: {lines:?}"
    );
}

#[gpui::test]
fn window_tests_history_label_shows_in_a_file_and_the_search_keeps_it(cx: &mut TestAppContext) {
    let root = sample_project("history-label");
    let path = root.join("src").join("main.txt");
    let window = open_window(cx, root.clone());
    wait_for_signal(cx, "the local history", |cx| {
        cx.update(|cx| crate::history::get(cx).is_some())
    });
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    view.update(cx, |view, cx| {
        view.replace_text(0..5, "alpha", cx);
        assert!(view.save_blocking(cx).is_none());
    });

    // Local History: Put Label records the label on the project.
    run_command(cx, window, "local_history.put_label");
    type_text(cx, window, "milestone");
    press(cx, window, "enter");
    wait_for_signal(cx, "the label", |cx| {
        cx.update(|cx| {
            crate::history::get(cx).is_some_and(|history| {
                history
                    .file_history(&path)
                    .iter()
                    .any(|revision| revision.label.as_deref() == Some("milestone"))
            })
        })
    });

    run_command(cx, window, "local_history.show");
    let lines = version_lines(cx);
    assert!(
        lines.iter().any(|line| line.contains("Label: milestone")),
        "the label stands in the history of the file: {lines:?}"
    );

    // The switch keeps the labels alone.
    click(cx, window, "history-only-labels");
    draw(cx, window);
    let lines = version_lines(cx);
    assert!(
        !lines.is_empty() && lines.iter().all(|line| line.contains("Label:")),
        "the labels alone are left: {lines:?}"
    );
    click(cx, window, "history-only-labels");
    draw(cx, window);

    // The search by the name of the label keeps its row and drops the versions.
    click(cx, window, "history-filter");
    type_text(cx, window, "milestone");
    draw(cx, window);
    let lines = version_lines(cx);
    assert!(
        lines.iter().any(|line| line.contains("Label: milestone")),
        "the search keeps the label: {lines:?}"
    );
    assert!(
        lines.iter().all(|line| line.contains("milestone")),
        "the search drops the other versions: {lines:?}"
    );
}

#[gpui::test]
fn window_tests_todo_custom_pattern_from_the_settings_is_found(cx: &mut TestAppContext) {
    let root = scratch("todo-settings");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src").join("A.txt"), "// HACK: first\n").unwrap();
    let window = open_window(cx, root.clone());

    // Settings -> Editor -> TODO: a pattern of the user.
    press(cx, window, "ctrl-alt-s");
    draw(cx, window);
    click(cx, window, "settings-search");
    type_text(cx, window, "TODO patterns");
    draw(cx, window);
    click(cx, window, "settings-page-todo");
    draw(cx, window);
    click(cx, window, "settings-todo-new");
    type_text(cx, window, "HACK");
    draw(cx, window);
    click(cx, window, "settings-todo-add");
    draw(cx, window);
    click(cx, window, "settings-ok");
    draw(cx, window);

    wait_for_signal(cx, "the HACK pattern", |cx| {
        cx.update(|cx| {
            cx.global::<Settings>()
                .todo_templates
                .iter()
                .any(|template| template == "HACK")
        })
    });
    let todo = cx.update(|cx| cx.global::<crate::todo::ActiveTodo>().0.clone());
    wait_for_signal(cx, "the TODO scan of HACK", |cx| {
        todo.read_with(cx, |panel, _| {
            panel.items().iter().any(|item| item.label == "HACK first")
        })
    });
}

#[gpui::test]
fn window_tests_todo_filters_by_pattern_and_current_file(cx: &mut TestAppContext) {
    let root = scratch("todo-filters");
    fs::create_dir_all(root.join("src")).unwrap();
    let first = root.join("src").join("A.txt");
    let second = root.join("src").join("B.txt");
    fs::write(&first, "// HACK: first\n// TODO: second\n").unwrap();
    fs::write(&second, "// HACK: third\n").unwrap();
    let window = open_window(cx, root.clone());
    wait_for_signal(cx, "the local history", |cx| {
        cx.update(|cx| crate::history::get(cx).is_some())
    });
    cx.update(|cx| {
        let mut settings = cx.global::<Settings>().clone();
        settings.todo_templates = vec!["TODO".to_owned(), "FIXME".to_owned(), "HACK".to_owned()];
        cx.set_global(settings);
    });
    let todo = cx.update(|cx| cx.global::<crate::todo::ActiveTodo>().0.clone());
    wait_for_signal(cx, "the TODO scan of HACK", |cx| {
        todo.read_with(cx, |panel, _| {
            panel.items().iter().any(|item| item.label == "HACK first")
        })
    });
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(TODO_TOOL, window, cx)
        })
        .unwrap();
    draw(cx, window);

    // All -> TODO -> FIXME -> HACK.
    for _ in 0..3 {
        click(cx, window, "item-button-todo-filter-template");
        draw(cx, window);
    }
    let items = todo.read_with(cx, |panel, _| panel.items().to_vec());
    assert_eq!(items.len(), 2, "{items:?}");
    assert!(
        items.iter().all(|item| item.label.starts_with("HACK")),
        "the filter keeps one pattern: {items:?}"
    );

    // Back to All, then the scope of the open file.
    click(cx, window, "item-button-todo-filter-template");
    draw(cx, window);
    open_path(cx, window, &first);
    click(cx, window, "item-tab-todo-scope-current-file");
    draw(cx, window);
    let items = todo.read_with(cx, |panel, _| panel.items().to_vec());
    assert_eq!(items.len(), 2, "{items:?}");
    assert!(
        items.iter().all(|item| item.path == first),
        "the current file scope keeps the marks of the open file: {items:?}"
    );
}
