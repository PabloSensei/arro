//! The keyboard and the mouse inside the tool windows without a real window: the tree of
//! the project, Structure, Problems, Usages, TODO, the test tree of Run, Git Log, Commit,
//! the Conflicts window, the Performance Monitor and the Local History dialog.

use std::io::Write;
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use proc_sup::ProcessSpec;
use runner::{RunPlan, TestListen, TestProvider, TestRequest, TestSite};
use ui_kit::Focusable;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use workspace::{
    COMMIT_TOOL, DEBUG_TOOL, LOG_TOOL, PERFORMANCE_TOOL, PROBLEMS_TOOL, PROJECT_TOOL, RUN_TOOL,
    SERVICES_TOOL, STRUCTURE_TOOL, TERMINAL_TOOL, TODO_TOOL, USAGES_TOOL, Workspace,
};

use crate::history_view;
use crate::performance::ActivePerformance;
use crate::problems::{ActiveProblems, Problem};
use crate::project::ActiveProjectPanel;
use crate::script::ActiveRunPanel;
use crate::structure::ActiveStructure;
use crate::todo::ActiveTodo;
use crate::usages::ActiveUsages;
use crate::window_tests::{
    active_path, click, draw, editor_of, open_path, open_sample_window, open_window,
    open_window_with, press, sample_project, scratch, type_text, usage, wait_for_file_index,
    wait_for_process,
};
use crate::window_tests_git::{
    conflicting_repository, repository_with_origin, start_conflicting_merge,
};
use crate::window_tests_run::setup;
use crate::window_tests_ui::{navigation_project, open_test_window, wait_for, wait_for_files};

/// A panel of the window is on screen by its debug selector.
fn has_selector(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &'static str,
) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear(cx);
    });
    visual.debug_bounds(selector).is_some()
}

fn clipboard(cx: &TestAppContext) -> Option<String> {
    cx.read(|cx| cx.read_from_clipboard().and_then(|item| item.text()))
}

fn show_tool(cx: &mut TestAppContext, window: WindowHandle<Workspace>, tool: &'static str) {
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(tool, window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
}

fn focused_is_editor(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    path: &Path,
) -> bool {
    let editor = editor_of(cx, window, path);
    let focus = editor.read_with(cx, |view, cx| view.focus_handle(cx));
    let focused = window
        .update(cx, |_, window, cx| window.focused(cx))
        .unwrap();
    focused == Some(focus)
}

#[ui_kit::gpui::test]
fn escape_in_every_tool_window_gives_the_focus_back_to_the_editor(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "tools-escape");
    let path = root.join("src").join("main.txt");
    open_path(cx, window, &path);

    let tools = [
        PROJECT_TOOL,
        STRUCTURE_TOOL,
        PROBLEMS_TOOL,
        USAGES_TOOL,
        RUN_TOOL,
        DEBUG_TOOL,
        SERVICES_TOOL,
        COMMIT_TOOL,
        LOG_TOOL,
        TODO_TOOL,
        PERFORMANCE_TOOL,
    ];
    for tool in tools {
        show_tool(cx, window, tool);
        assert!(
            !focused_is_editor(cx, window, &path),
            "{tool} takes the focus when it opens"
        );

        press(cx, window, "escape");
        let open = window
            .read_with(cx, |workspace, _| workspace.tool_open(tool))
            .unwrap();
        assert!(open, "{tool} stays open after Escape");
        assert!(
            focused_is_editor(cx, window, &path),
            "Escape in {tool} puts the focus back into the editor"
        );
    }

    // In the terminal Escape goes to the program inside, a harness stops its turn on it.
    show_tool(cx, window, TERMINAL_TOOL);
    press(cx, window, "escape");
    assert!(
        !focused_is_editor(cx, window, &path),
        "Escape in the terminal stays in the terminal"
    );
}

#[ui_kit::gpui::test]
fn the_project_tree_walks_expands_searches_and_collapses(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "tools-project");
    let main = root.join("src").join("main.txt");
    let readme = root.join("README.txt");
    let panel = cx.update(|cx| cx.global::<ActiveProjectPanel>().0.clone());
    show_tool(cx, window, PROJECT_TOOL);

    // Down walks the rows, Right opens the folder under the selection.
    press(cx, window, "down down right");
    wait_for(cx, "the rows of the source folder", |cx| {
        panel.read_with(cx, |panel, _| panel.tree().index_of(&main).is_some())
    });

    // Down reaches the file and Enter opens it.
    press(cx, window, "down enter");
    assert_eq!(active_path(cx, window), Some(main.clone()));

    // Typed letters run the speed search; F4 opens the row it picked.
    show_tool(cx, window, PROJECT_TOOL);
    type_text(cx, window, "readme");
    press(cx, window, "f4");
    assert_eq!(active_path(cx, window), Some(readme));

    // Ctrl+Minus closes every folder but the root.
    show_tool(cx, window, PROJECT_TOOL);
    press(cx, window, "ctrl--");
    assert!(
        panel.read_with(cx, |panel, _| panel.tree().index_of(&main).is_none()),
        "the rows of the closed folders are gone"
    );
}

#[ui_kit::gpui::test]
fn the_structure_window_moves_and_opens_a_declaration_by_the_keyboard(cx: &mut TestAppContext) {
    let root = navigation_project("tools-structure");
    let window = open_test_window(cx, root.clone());
    cx.run_until_parked();
    draw(cx, window);
    let sample = root.join("src").join("Sample.txt");
    open_path(cx, window, &sample);
    let structure = cx.update(|cx| cx.global::<ActiveStructure>().0.clone());
    wait_for(cx, "the outline of the file", |cx| {
        !structure
            .read_with(cx, |panel, cx| panel.lines(cx))
            .is_empty()
    });

    press(cx, window, "alt-7");
    draw(cx, window);
    press(cx, window, "down down");
    press(cx, window, "ctrl-c");
    assert_eq!(
        clipboard(cx).as_deref(),
        Some("getOwner"),
        "Ctrl+C copies the selected declaration"
    );
    press(cx, window, "enter");
    let view = editor_of(cx, window, &sample);
    assert_eq!(
        view.read_with(cx, |view, _| view.caret()),
        (1, 11),
        "Enter opens the declaration at its name"
    );
}

#[ui_kit::gpui::test]
fn problems_todo_and_usages_copy_the_selected_row(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "tools-lists");
    let main = root.join("src").join("main.txt");
    let notes = root.join("src").join("notes.txt");

    // Problems: Down picks the only row, Ctrl+C copies it.
    let problems = cx.update(|cx| cx.global::<ActiveProblems>().0.clone());
    show_tool(cx, window, PROBLEMS_TOOL);
    problems.update(cx, |hub, cx| {
        hub.set_file_problems(
            main.clone(),
            vec![Problem {
                line: 1,
                column: 4,
                severity: lang_api::Severity::Error,
                message: "cannot resolve the name".into(),
            }],
            cx,
        );
    });
    cx.run_until_parked();
    draw(cx, window);
    press(cx, window, "down");
    press(cx, window, "ctrl-c");
    assert!(
        clipboard(cx)
            .as_deref()
            .is_some_and(|text| text.contains("cannot resolve the name")),
        "Ctrl+C copies the problem"
    );

    // TODO: the typed letters pick the mark, Ctrl+C copies the row.
    let todo = cx.update(|cx| cx.global::<ActiveTodo>().0.clone());
    wait_for(cx, "the TODO scan", |cx| {
        !todo.read_with(cx, |panel, _| panel.items().is_empty())
    });
    show_tool(cx, window, TODO_TOOL);
    type_text(cx, window, "todo");
    press(cx, window, "ctrl-c");
    assert!(
        clipboard(cx)
            .as_deref()
            .is_some_and(|text| text.starts_with("TODO read me")),
        "the speed search picked the TODO mark and Ctrl+C copied it"
    );

    // Usages: Down picks the first place, Ctrl+C copies it.
    let usages = cx.update(|cx| cx.global::<ActiveUsages>().0.clone());
    usages.update(cx, |hub, cx| {
        hub.set_usages(
            vec![usage(&notes, 0, 0, "later"), usage(&main, 1, 0, "earlier")],
            cx,
        );
    });
    cx.run_until_parked();
    show_tool(cx, window, USAGES_TOOL);
    press(cx, window, "down");
    press(cx, window, "ctrl-c");
    assert!(
        clipboard(cx)
            .as_deref()
            .is_some_and(|text| text.starts_with("earlier")),
        "Ctrl+C copies the usage"
    );
}

/// The events of a test class with a passing and a failing case.
const TEST_EVENTS: &[&str] = &[
    r#"{"event":"plan"}"#,
    r#"{"event":"started","id":"e","name":"Engine","kind":"suite"}"#,
    r#"{"event":"started","id":"e/c","parent":"e","name":"Vitest","kind":"suite","suite":"demo.Vitest"}"#,
    r#"{"event":"started","id":"e/c/passes","parent":"e/c","name":"passes()","kind":"case","suite":"demo.Vitest","case":"passes"}"#,
    r#"{"event":"finished","id":"e/c/passes","outcome":"passed","durationMs":2}"#,
    r#"{"event":"started","id":"e/c/fails","parent":"e/c","name":"fails()","kind":"case","suite":"demo.Vitest","case":"fails"}"#,
    r#"{"event":"finished","id":"e/c/fails","outcome":"failed","message":"expected 1, got 2","trace":"AssertionFailedError: expected\n\tat demo.Vitest.fails(Vitest.code:2)","durationMs":3}"#,
    r#"{"event":"planFinished"}"#,
];

/// A test provider that plays the listener from a thread.
struct EventTests;

impl TestProvider for EventTests {
    fn plan(
        &self,
        _: &project_model::ProjectModel,
        _: &TestRequest,
        listen: &TestListen,
    ) -> base::Result<RunPlan> {
        let listen = listen.clone();
        std::thread::spawn(move || {
            let Ok(mut stream) = TcpStream::connect(("127.0.0.1", listen.port)) else {
                return;
            };
            let _ = writeln!(stream, "{}", listen.token);
            for line in TEST_EVENTS {
                if writeln!(stream, "{line}").is_err() {
                    return;
                }
            }
        });
        Ok(RunPlan {
            before: None,
            program: ProcessSpec::new("cmd").args(["/d", "/c", "exit", "0"]),
            grace: Duration::from_secs(1),
        })
    }
}

fn find_tests(text: &str, _: &Path) -> Vec<TestSite> {
    text.lines()
        .position(|line| line.contains("void fails"))
        .map(|index| {
            vec![TestSite {
                suite: "demo.Vitest".into(),
                case: Some("fails".into()),
                line: index as u32 + 1,
            }]
        })
        .unwrap_or_default()
}

fn test_source(root: &Path) -> PathBuf {
    let directory = root.join("src").join("demo");
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("Vitest.code");
    std::fs::write(
        &path,
        "class Vitest {\n    void fails() {\n        check();\n    }\n}\n",
    )
    .unwrap();
    path
}

#[ui_kit::gpui::test]
fn the_test_tree_of_the_run_window_walks_and_opens_the_source(cx: &mut TestAppContext) {
    let root = sample_project("tools-run-tree");
    let source = test_source(&root);
    let mut setup = setup(&root);
    setup.tests = Some(Arc::new(EventTests));
    setup.test_sites = find_tests;
    let window = open_window_with(cx, root.clone(), setup);
    cx.run_until_parked();
    draw(cx, window);
    let panel = cx.update(|cx| cx.global::<ActiveRunPanel>().0.clone());

    open_path(cx, window, &source);
    wait_for_file_index(cx);
    wait_for_files(cx, window, &source);
    let view = editor_of(cx, window, &source);
    view.update(cx, |view, cx| view.restore_caret(1, 0, cx));
    draw(cx, window);
    press(cx, window, "ctrl-shift-f10");
    wait_for_process(cx, "the finished tests", |cx| {
        panel.read_with(cx, |panel, _| {
            panel.idle()
                && panel
                    .test_report()
                    .iter()
                    .any(|line| line.contains("failed") && line.contains("fails"))
        })
    });

    show_tool(cx, window, RUN_TOOL);
    press(cx, window, "up down");
    press(cx, window, "ctrl-c");
    assert_eq!(
        clipboard(cx).as_deref(),
        Some("fails()"),
        "Ctrl+C copies the row of the test"
    );

    press(cx, window, "enter");
    assert_eq!(active_path(cx, window), Some(source.clone()));
    let view = editor_of(cx, window, &source);
    assert_eq!(
        view.read_with(cx, |view, _| view.caret()),
        (1, 0),
        "Enter opens the source of the test"
    );
}

#[ui_kit::gpui::test]
fn the_git_log_walks_copies_a_commit_and_opens_its_diff(cx: &mut TestAppContext) {
    let (root, _bare, git) = repository_with_origin("tools-log");
    std::fs::write(root.join("main.txt"), "alpha\nbeta\ngamma\n").unwrap();
    git.run(&root, ["add", "main.txt"]).unwrap();
    git.run(&root, ["commit", "-m", "second"]).unwrap();

    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    draw(cx, window);
    let state = cx.update(|cx| cx.global::<crate::git::ActiveGit>().0.clone());
    wait_for(cx, "the repository", |cx| {
        state.read_with(cx, |state, _| state.repo().is_some())
    });
    press(cx, window, "alt-9");
    wait_for(cx, "the commits", |cx| {
        has_selector(cx, window, "log-commit-0")
    });

    // Down walks the commits newest first; Ctrl+C copies the selected one.
    press(cx, window, "down down");
    press(cx, window, "ctrl-c");
    assert!(
        clipboard(cx)
            .as_deref()
            .is_some_and(|text| text.contains("base")),
        "Ctrl+C copies the older commit"
    );

    // Up goes back to the newest commit and Enter opens its diff.
    press(cx, window, "up");
    wait_for(cx, "the changed files of the commit", |cx| {
        has_selector(cx, window, "log-file-0")
    });
    press(cx, window, "enter");
    wait_for(cx, "the diff of the commit", |cx| {
        has_selector(cx, window, "diff-title")
    });
}

#[ui_kit::gpui::test]
fn space_marks_a_file_of_the_commit_window_and_ctrl_enter_commits(cx: &mut TestAppContext) {
    let (root, _bare, _git) = repository_with_origin("tools-commit");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    draw(cx, window);
    let state = cx.update(|cx| cx.global::<crate::git::ActiveGit>().0.clone());
    wait_for(cx, "the repository", |cx| {
        state.read_with(cx, |state, _| state.repo().is_some())
    });

    let path = root.join("main.txt");
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    view.update(cx, |view, cx| {
        view.replace_text(0..5, "ALPHA", cx);
        assert!(view.save_blocking(cx).is_none());
    });
    state.update(cx, |state, cx| state.refresh(cx));
    wait_for(cx, "the changed file", |cx| {
        state.read_with(cx, |state, _| !state.status().files.is_empty())
    });

    show_tool(cx, window, COMMIT_TOOL);
    wait_for(cx, "the row of the file", |cx| {
        has_selector(cx, window, "commit-row-0")
    });
    // A click gives the list the keys; Space takes the tick off the file.
    click(cx, window, "commit-row-0");
    press(cx, window, "space");
    type_text(cx, window, "unchecked");
    press(cx, window, "ctrl-enter");
    wait_for(cx, "the refusal to commit", |cx| {
        window
            .read_with(cx, |workspace, _| workspace.last_notification())
            .unwrap()
            .is_some_and(|(_, text)| text.contains("Tick at least one file"))
    });
    assert!(
        state.read_with(cx, |state, _| !state.status().files.is_empty()),
        "nothing went to git"
    );

    // Space ticks the file again and Ctrl+Enter commits the message.
    press(cx, window, "space");
    press(cx, window, "ctrl-enter");
    wait_for(cx, "the commit", |cx| {
        state.read_with(cx, |state, _| {
            state.busy().is_none() && state.status().files.is_empty()
        })
    });
    let log = state
        .read_with(cx, |state, _| state.repo())
        .and_then(|repo| repo.log(1, 0).ok())
        .unwrap_or_default();
    assert_eq!(log[0].subject, "unchecked");
}

#[ui_kit::gpui::test]
fn ctrl_enter_finishes_a_resolved_conflict(cx: &mut TestAppContext) {
    let (root, _bare, _git) = conflicting_repository("tools-conflict");
    let (window, state) = start_conflicting_merge(cx, root);

    click(cx, window, "conflict-theirs-0");
    wait_for(cx, "the first conflict resolved", |cx| {
        state.read_with(cx, |state, _| state.conflicts().len() == 1)
    });
    click(cx, window, "conflict-theirs-0");
    wait_for(cx, "the commit button", |cx| {
        has_selector(cx, window, "conflict-commit")
    });

    press(cx, window, "ctrl-enter");
    wait_for(cx, "the finished merge", |cx| {
        state.read_with(cx, |state, _| state.operation() == vcs::Operation::None)
    });
}

#[ui_kit::gpui::test]
fn the_performance_monitor_copies_its_table_with_ctrl_c(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "tools-performance");
    show_tool(cx, window, PERFORMANCE_TOOL);
    let panel = cx.update(|cx| cx.global::<ActivePerformance>().0.clone());
    assert!(
        panel.read_with(cx, |panel, _| !panel.report().is_empty()),
        "the monitor has its table"
    );
    press(cx, window, "ctrl-c");
    assert!(
        clipboard(cx)
            .as_deref()
            .is_some_and(|text| text.contains("Performance Monitor")),
        "Ctrl+C copies the table"
    );
}

#[ui_kit::gpui::test]
fn the_local_history_walks_its_versions_by_the_keyboard(cx: &mut TestAppContext) {
    let root = scratch("tools-history");
    let path = root.join("Saved.txt");
    std::fs::write(&path, "one\n").unwrap();
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    draw(cx, window);
    wait_for(cx, "the local history", |cx| {
        cx.update(|cx| crate::history::get(cx).is_some())
    });
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    for (index, text) in ["two\n", "three\n"].into_iter().enumerate() {
        view.update(cx, |view, cx| {
            let length = view.text().map_or(0, |text| text.len());
            view.replace_text(0..length, text, cx);
            assert!(view.save_blocking(cx).is_none(), "the file is written");
        });
        let expected = index + 1;
        wait_for(cx, "the saved version", |cx| {
            cx.update(|cx| {
                crate::history::get(cx)
                    .is_some_and(|history| history.file_history(&path).len() >= expected)
            })
        });
    }

    window
        .update(cx, |workspace, window, cx| {
            history_view::HistoryView::show(path.clone(), workspace, window, cx)
        })
        .unwrap();
    wait_for(cx, "the versions of the history", |cx| {
        cx.update(|cx| {
            history_view::report(cx)
                .iter()
                .filter(|line| line.starts_with("history-version"))
                .count()
                >= 2
        })
    });
    assert_eq!(cx.update(|cx| history_view::selected_index(cx)), Some(0));

    press(cx, window, "down");
    assert_eq!(
        cx.update(|cx| history_view::selected_index(cx)),
        Some(1),
        "Down moves to the older version"
    );
    press(cx, window, "up");
    assert_eq!(cx.update(|cx| history_view::selected_index(cx)), Some(0));
    press(cx, window, "escape");
    cx.run_until_parked();
    assert!(
        !has_selector(cx, window, "history"),
        "Escape closes the Local History dialog"
    );
}
