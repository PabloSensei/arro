//! T-0292: the SQL console of the Database window without a window. Completion of the
//! loaded schema offers the tables and the columns of a statement, an unknown table is
//! underlined, every statement keeps its own result tab with its time and rows, and Stop
//! answers a statement that runs while the window keeps drawing.
//!
//! `cargo test -p panels window_tests_database_console`

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]

use std::fs;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use database::{
    Column, ColumnKind, DriverKind, ResultSet, Source, SourceId, Table, TableKind, Value,
};
use lang_api::Severity;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use workspace::{DATABASE_TOOL, Workspace};

use crate::database_panel::{ActiveDatabase, DatabasePanel, ScriptedCommand};
use crate::problems::ActiveProblems;
use crate::window_tests::{
    bounds_of, buffer_of, click, draw, editor_of, open_path, open_window, press, sample_project,
    type_text, wait_for_signal,
};

const SOURCE: &str = "owners";

fn source() -> Source {
    Source {
        id: SourceId::new(SOURCE),
        name: "PetClinic".into(),
        kind: DriverKind::H2,
        url: "mem:petclinic".into(),
        user: "sa".into(),
        module: None,
        driver_jar: None,
    }
}

fn column(name: &str, type_name: &str) -> Column {
    Column {
        name: name.into(),
        type_name: type_name.into(),
        kind: ColumnKind::Other,
        nullable: true,
        primary_key: false,
    }
}

fn tables() -> Vec<Table> {
    vec![
        Table {
            schema: "PUBLIC".into(),
            name: "OWNERS".into(),
            kind: TableKind::Table,
            columns: vec![column("ID", "INTEGER"), column("NAME", "VARCHAR")],
            indexes: Vec::new(),
            foreign_keys: Vec::new(),
            partial: false,
        },
        Table {
            schema: "PUBLIC".into(),
            name: "TYPES".into(),
            kind: TableKind::Table,
            columns: vec![column("ID", "INTEGER"), column("LABEL", "VARCHAR")],
            indexes: Vec::new(),
            foreign_keys: Vec::new(),
            partial: false,
        },
    ]
}

fn rows() -> ResultSet {
    ResultSet {
        columns: vec![column("ID", "INTEGER")],
        rows: vec![vec![Value::Integer(1)]],
        update_count: None,
        truncated: false,
    }
}

/// A result of `count` rows: a limited statement answers with a whole portion.
fn many_rows(count: usize) -> ResultSet {
    ResultSet {
        columns: vec![column("ID", "INTEGER")],
        rows: (0..count)
            .map(|index| vec![Value::Integer(index as i64)])
            .collect(),
        update_count: None,
        truncated: false,
    }
}

fn panel(cx: &TestAppContext) -> gpui::Entity<DatabasePanel> {
    cx.update(|cx| cx.global::<ActiveDatabase>().0.clone())
}

/// Opens the window, shows Database and hands the source a scripted connection with the
/// tables of the test.
fn source_shown(
    cx: &mut TestAppContext,
    name: &str,
) -> (WindowHandle<Workspace>, gpui::Entity<DatabasePanel>) {
    source_shown_rows(cx, name, rows())
}

/// Opens the window as [`source_shown`] does, with the rows the statement answers.
fn source_shown_rows(
    cx: &mut TestAppContext,
    name: &str,
    rows: ResultSet,
) -> (WindowHandle<Workspace>, gpui::Entity<DatabasePanel>) {
    let root = sample_project(name);
    let window = open_window(cx, root);
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(DATABASE_TOOL, window, cx)
        })
        .unwrap();
    draw(cx, window);
    let panel = panel(cx);
    panel.update(cx, |panel, cx| {
        panel.connect_for_test(
            source(),
            tables(),
            rows,
            None,
            Arc::new(AtomicBool::new(false)),
            cx,
        );
    });
    draw(cx, window);
    (window, panel)
}

fn console_of(cx: &TestAppContext, panel: &gpui::Entity<DatabasePanel>) -> std::path::PathBuf {
    panel
        .read_with(cx, |panel, _| panel.console_path(&SourceId::new(SOURCE)))
        .expect("the source has a console")
}

fn shown(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

/// Waits until the list of completion is drawn, as an answer of the language arrives on
/// its own thread.
fn wait_for_completion(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    wait_for_signal(cx, "the list of completion", |cx| {
        draw(cx, window);
        shown(cx, window, "completion-popup")
    });
}

#[ui_kit::gpui::test]
fn a_from_prefix_offers_the_tables_of_the_source_and_inserts_one(cx: &mut TestAppContext) {
    let (window, panel) = source_shown(cx, "sql-console-from");
    let console = console_of(cx, &panel);
    fs::write(&console, "SELECT * FROM o\n").unwrap();
    open_path(cx, window, &console);
    let view = editor_of(cx, window, &console);

    press(cx, window, "end");
    press(cx, window, "ctrl-space");
    wait_for_completion(cx, window);
    press(cx, window, "enter");

    assert_eq!(
        buffer_of(cx, &view),
        "SELECT * FROM OWNERS\n",
        "the chosen table is inserted at the caret"
    );
}

#[ui_kit::gpui::test]
fn an_alias_of_the_from_offers_the_columns_of_its_table(cx: &mut TestAppContext) {
    let (window, panel) = source_shown(cx, "sql-console-alias");
    let console = console_of(cx, &panel);
    fs::write(&console, "SELECT * FROM OWNERS o WHERE o.\n").unwrap();
    open_path(cx, window, &console);
    let view = editor_of(cx, window, &console);

    press(cx, window, "end");
    press(cx, window, "ctrl-space");
    wait_for_completion(cx, window);
    press(cx, window, "enter");

    assert_eq!(
        buffer_of(cx, &view),
        "SELECT * FROM OWNERS o WHERE o.ID\n",
        "the column of the table the alias names is inserted"
    );
}

#[ui_kit::gpui::test]
fn an_unknown_table_is_underlined_with_its_message(cx: &mut TestAppContext) {
    let (window, panel) = source_shown(cx, "sql-console-unknown");
    let console = console_of(cx, &panel);
    fs::write(&console, "SELECT * FROM NOPE\n").unwrap();
    open_path(cx, window, &console);
    let view = editor_of(cx, window, &console);

    wait_for_signal(cx, "the mark of the unknown table", |cx| {
        !view
            .read_with(cx, |view, _| view.diagnostic_marks())
            .is_empty()
    });
    let marks = view.read_with(cx, |view, _| view.diagnostic_marks());
    assert!(
        marks
            .iter()
            .any(|(severity, range)| *severity == Severity::Error
                && range.start == 14
                && range.end == 18),
        "the mark covers the name NOPE: {marks:?}"
    );

    let problems = cx.update(|cx| cx.global::<ActiveProblems>().0.clone());
    press(cx, window, "alt-6");
    draw(cx, window);
    let items = problems.read_with(cx, |hub, cx| hub.panel().read(cx).items().to_vec());
    let labels: Vec<&str> = items.iter().map(|item| item.label.as_str()).collect();
    assert!(
        labels.contains(&"Unable to resolve table 'NOPE'"),
        "{labels:?}"
    );
}

#[ui_kit::gpui::test]
fn two_statements_give_two_result_tabs(cx: &mut TestAppContext) {
    let (window, panel) = source_shown(cx, "sql-console-tabs");
    let console = console_of(cx, &panel);
    fs::write(&console, "SELECT 1;\nSELECT 2;\n").unwrap();
    open_path(cx, window, &console);

    press(cx, window, "ctrl-a");
    press(cx, window, "ctrl-enter");

    wait_for_signal(cx, "the results of both statements", |cx| {
        panel.read_with(cx, |panel, _| panel.results_for_test().len() == 2)
    });
    let history = panel.read_with(cx, |panel, _| panel.console_history_for_test());
    assert_eq!(
        history,
        ["SELECT 2".to_owned(), "SELECT 1".to_owned()],
        "the toolbar keeps the statements the user ran, newest first"
    );
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "database-result-tab-1").size.width > ui_kit::px(0.),
        "each statement has its own tab"
    );
}

#[ui_kit::gpui::test]
fn the_result_shows_its_time_and_its_rows(cx: &mut TestAppContext) {
    let (window, panel) = source_shown(cx, "sql-console-summary");
    let console = console_of(cx, &panel);
    fs::write(&console, "SELECT ID FROM OWNERS;\n").unwrap();
    open_path(cx, window, &console);

    press(cx, window, "ctrl-enter");

    wait_for_signal(cx, "the result of the statement", |cx| {
        panel.read_with(cx, |panel, _| !panel.results_for_test().is_empty())
    });
    let results = panel.read_with(cx, |panel, _| panel.results_for_test());
    let summary = &results[0].1;
    assert!(summary.contains("ms"), "the time is shown: {summary}");
    assert!(
        summary.contains("1 row"),
        "the number of rows is shown: {summary}"
    );
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "database-result-summary").size.width > ui_kit::px(0.),
        "the summary is on screen"
    );
}

#[ui_kit::gpui::test]
fn a_limited_result_counts_only_when_the_user_asks(cx: &mut TestAppContext) {
    let (window, panel) = source_shown_rows(cx, "sql-console-count", many_rows(500));
    panel.update(cx, |panel, _| panel.scripted_count_for_test(12_840));
    let console = console_of(cx, &panel);
    fs::write(&console, "SELECT ID FROM OWNERS;\n").unwrap();
    open_path(cx, window, &console);

    press(cx, window, "ctrl-enter");
    wait_for_signal(cx, "the limited result", |cx| {
        panel.read_with(cx, |panel, _| !panel.results_for_test().is_empty())
    });
    draw(cx, window);
    let results = panel.read_with(cx, |panel, _| panel.results_for_test());
    assert!(
        results[0].1.contains("500+ rows"),
        "the portion says that there is more: {}",
        results[0].1
    );
    assert_eq!(
        panel
            .read_with(cx, |panel, _| panel.queries_for_test())
            .len(),
        1,
        "the statement ran once: no count follows it on its own"
    );
    assert!(
        shown(cx, window, "database-result-count"),
        "the count is offered on the portion"
    );
    assert!(!shown(cx, window, "database-result-counting"));

    click(cx, window, "database-result-count");
    wait_for_signal(cx, "the count of the result", |cx| {
        panel.read_with(cx, |panel, _| {
            panel.results_for_test()[0].1.contains(" of ")
        })
    });
    let results = panel.read_with(cx, |panel, _| panel.results_for_test());
    assert!(
        results[0].1.contains("500 rows of 12\u{a0}840"),
        "the click counts the rows of the whole statement: {}",
        results[0].1
    );
    let queries = panel.read_with(cx, |panel, _| panel.queries_for_test());
    assert_eq!(
        queries.len(),
        2,
        "the count is the second statement: {queries:?}"
    );
    assert!(
        queries[1].0.contains("COUNT(*)"),
        "the count asks the database for it: {queries:?}"
    );
    draw(cx, window);
    assert!(
        !shown(cx, window, "database-result-count"),
        "a counted result offers no second count"
    );
}

#[ui_kit::gpui::test]
fn a_count_shows_its_indicator_and_stop_cancels_it(cx: &mut TestAppContext) {
    let (window, panel) = source_shown_rows(cx, "sql-console-count-stop", many_rows(500));
    panel.update(cx, |panel, _| panel.scripted_count_for_test(12_840));
    let console = console_of(cx, &panel);
    fs::write(&console, "SELECT ID FROM OWNERS;\n").unwrap();
    open_path(cx, window, &console);

    press(cx, window, "ctrl-enter");
    wait_for_signal(cx, "the limited result", |cx| {
        panel.read_with(cx, |panel, _| !panel.results_for_test().is_empty())
    });
    // The next statement waits for the driver, as a count of a big table does.
    panel.update(cx, |panel, _| panel.scripted_hangs_for_test(true));
    click(cx, window, "database-result-count");
    draw(cx, window);
    assert!(
        shown(cx, window, "database-result-counting"),
        "the count shows that it runs"
    );
    assert!(
        panel.read_with(cx, |panel, _| panel.running_for_test()),
        "the console knows that the count runs"
    );

    click(cx, window, "database-console-stop");
    wait_for_signal(cx, "the stopped count", |cx| {
        !panel.read_with(cx, |panel, _| panel.running_for_test())
    });
    draw(cx, window);
    assert!(
        !shown(cx, window, "database-result-counting"),
        "the indicator is gone after the stop"
    );
    assert!(
        shown(cx, window, "database-result-count"),
        "the count can be asked for again"
    );
    let results = panel.read_with(cx, |panel, _| panel.results_for_test());
    assert!(
        results[0].1.contains("500+ rows"),
        "the stopped count leaves the portion alone: {}",
        results[0].1
    );
    assert!(
        panel
            .read_with(cx, |panel, _| panel.scripted_commands_for_test())
            .contains(&ScriptedCommand::Cancel),
        "the driver was asked to stop the count"
    );
}

#[ui_kit::gpui::test]
fn a_long_statement_stops_while_the_window_answers(cx: &mut TestAppContext) {
    let (window, panel) = source_shown(cx, "sql-console-stop");
    panel.update(cx, |panel, _| panel.scripted_hangs_for_test(true));
    let console = console_of(cx, &panel);
    fs::write(&console, "SELECT * FROM OWNERS;\n").unwrap();
    open_path(cx, window, &console);
    let view = editor_of(cx, window, &console);

    press(cx, window, "ctrl-enter");
    wait_for_signal(cx, "the running statement", |cx| {
        panel.read_with(cx, |panel, _| panel.running_for_test())
    });
    draw(cx, window);
    assert!(
        shown(cx, window, "database-console-running"),
        "the window shows that the statement runs"
    );
    assert!(
        shown(cx, window, "database-console-stop"),
        "the Stop button is on screen"
    );

    press(cx, window, "ctrl-f2");
    wait_for_signal(cx, "the stopped statement", |cx| {
        !panel.read_with(cx, |panel, _| panel.running_for_test())
    });
    let results = panel.read_with(cx, |panel, _| panel.results_for_test());
    assert_eq!(results.len(), 1);
    assert!(
        results[0]
            .2
            .as_deref()
            .is_some_and(|error| error.contains("cancelled")),
        "the tab says the statement was stopped: {results:?}"
    );
    assert!(
        panel
            .read_with(cx, |panel, _| panel.scripted_commands_for_test())
            .contains(&ScriptedCommand::Cancel),
        "the driver was asked to stop the statement"
    );

    // The window kept drawing and the console still takes what the user types.
    type_text(cx, window, "-- after the stop\n");
    assert!(
        buffer_of(cx, &view).contains("-- after the stop"),
        "the editor of the console is alive"
    );
}
