//! The Problems window as IDEA shows it: the summary in the header counts every kind of
//! problem, and the Current File tab names the file once and its problems without a path.

use lang_api::Severity;
use ui_kit::gpui::{self, TestAppContext, WindowHandle};
use workspace::{PROBLEMS_TOOL, Workspace};

use crate::items::PanelRow;
use crate::problems::{ActiveProblems, Problem, ProblemsHub};
use crate::window_tests::{editor_of, open_path, open_sample_window, press};

fn show_tool(cx: &mut TestAppContext, window: WindowHandle<Workspace>, tool: &'static str) {
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(tool, window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    crate::window_tests::draw(cx, window);
}

fn problem(line: usize, column: usize, severity: Severity, message: &str) -> Problem {
    Problem {
        line,
        column,
        severity,
        message: message.to_owned(),
    }
}

/// A misspelled word, as the editor makes it for the spelling check.
fn typo(line: usize, word: &str) -> Problem {
    problem(line, 4, Severity::Hint, &format!("Typo: In word '{word}'"))
}

fn title(cx: &TestAppContext, problems: &gpui::Entity<ProblemsHub>) -> String {
    problems.read_with(cx, |hub, cx| hub.panel().read(cx).title().to_owned())
}

#[ui_kit::gpui::test]
fn the_summary_names_typos_and_does_not_count_them_as_no_errors_and_warnings(
    cx: &mut TestAppContext,
) {
    let (root, window) = open_sample_window(cx, "problems-typos");
    let main = root.join("src").join("main.txt");
    open_path(cx, window, &main);
    let problems = cx.update(|cx| cx.global::<ActiveProblems>().0.clone());
    problems.update(cx, |hub, cx| {
        let found = (0..10)
            .map(|line| typo(line, &format!("word{line}")))
            .collect();
        hub.set_file_problems(main.clone(), found, cx);
    });
    show_tool(cx, window, PROBLEMS_TOOL);

    let title = title(cx, &problems);
    assert_eq!(title, "Problems (10 typos)");
    assert!(
        !title.contains("0 errors") && !title.contains("0 warnings"),
        "the kinds with no problems are left out: {title}"
    );
}

#[ui_kit::gpui::test]
fn the_summary_counts_every_kind_and_leaves_out_the_zero_parts(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "problems-kinds");
    let main = root.join("src").join("main.txt");
    open_path(cx, window, &main);
    let problems = cx.update(|cx| cx.global::<ActiveProblems>().0.clone());
    problems.update(cx, |hub, cx| {
        hub.set_file_problems(
            main.clone(),
            vec![
                problem(0, 0, Severity::Error, "cannot resolve"),
                problem(1, 0, Severity::Warning, "unused"),
                problem(2, 0, Severity::Hint, "style"),
                problem(3, 0, Severity::Hint, "name"),
                problem(4, 0, Severity::Information, "note"),
                typo(5, "Hoeller"),
            ],
            cx,
        );
    });
    show_tool(cx, window, PROBLEMS_TOOL);

    assert_eq!(
        title(cx, &problems),
        "Problems (1 error, 1 warning, 2 weak warnings, 1 info, 1 typo)"
    );
}

#[ui_kit::gpui::test]
fn an_empty_list_says_no_problems(cx: &mut TestAppContext) {
    let (_root, window) = open_sample_window(cx, "problems-empty");
    show_tool(cx, window, PROBLEMS_TOOL);
    let problems = cx.update(|cx| cx.global::<ActiveProblems>().0.clone());

    assert_eq!(title(cx, &problems), "No problems");
}

#[ui_kit::gpui::test]
fn the_current_file_tab_names_the_file_once_and_its_problems_without_a_path(
    cx: &mut TestAppContext,
) {
    let (root, window) = open_sample_window(cx, "problems-file-node");
    let main = root.join("src").join("main.txt");
    open_path(cx, window, &main);
    let problems = cx.update(|cx| cx.global::<ActiveProblems>().0.clone());
    problems.update(cx, |hub, cx| {
        hub.set_file_problems(
            main.clone(),
            vec![
                problem(1, 0, Severity::Error, "cannot resolve"),
                problem(4, 0, Severity::Warning, "unused"),
            ],
            cx,
        );
    });
    show_tool(cx, window, PROBLEMS_TOOL);

    let (rows, items) = problems.read_with(cx, |hub, cx| {
        let panel = hub.panel().read(cx);
        (panel.rows().to_vec(), panel.items().to_vec())
    });
    assert_eq!(rows.len(), 3, "one row of the file and two problems");
    let PanelRow::Group {
        label,
        depth,
        icon,
        hint,
    } = &rows[0]
    else {
        panic!("the first row is the file");
    };
    assert_eq!(label, "main.txt");
    assert_eq!(*depth, 0);
    assert_eq!(
        *icon, "fileTypes/text",
        "the row carries the icon of the file"
    );
    let hint = hint.as_deref().unwrap_or_default();
    assert_eq!(
        hint, "src (2)",
        "the row shows the folder and the number of problems"
    );
    for (at, row) in rows[1..].iter().enumerate() {
        assert_eq!(
            *row,
            PanelRow::Item {
                index: at,
                depth: 1,
                excluded: false,
            }
        );
    }

    let hints: Vec<&str> = items.iter().map(|item| item.hint.as_str()).collect();
    assert_eq!(hints, [":2", ":5"], "a problem row shows only its line");
    for item in &items {
        assert_eq!(item.path, main, "an item still knows where it opens");
        assert!(
            !item.label.contains("src"),
            "the message of a problem has no path: {}",
            item.label
        );
    }
}

#[ui_kit::gpui::test]
fn enter_on_a_problem_row_opens_the_place(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "problems-enter");
    let main = root.join("src").join("main.txt");
    open_path(cx, window, &main);
    let problems = cx.update(|cx| cx.global::<ActiveProblems>().0.clone());
    problems.update(cx, |hub, cx| {
        hub.set_file_problems(
            main.clone(),
            vec![problem(1, 4, Severity::Error, "cannot resolve")],
            cx,
        );
    });
    show_tool(cx, window, PROBLEMS_TOOL);

    press(cx, window, "down enter");
    let view = editor_of(cx, window, &main);
    assert_eq!(
        view.read_with(cx, |view, _| view.caret()),
        (1, 4),
        "Enter opens the selected problem at its place"
    );
}
