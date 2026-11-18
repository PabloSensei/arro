//! T-0298: the second step of the SQL console without a window. A console in Manual keeps
//! its changes until Commit or Rollback and asks before it closes; an export reads every
//! row of a result into a file; Explain Plan opens the plan of the statement at the caret;
//! a statement with placeholders asks for their values and sends them to the driver.
//!
//! `cargo test -p panels window_tests_database_step2`

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use database::{
    Column, ColumnKind, DriverKind, Param, ResultSet, Source, SourceId, Table, TableKind, Value,
};
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use workspace::{DATABASE_TOOL, Workspace};

use crate::database_panel::{ActiveDatabase, DatabasePanel, ScriptedCommand};
use crate::window_tests::{
    bounds_of, click, draw, editor_of, open_path, open_window, press, sample_project, type_text,
    wait_for_signal,
};

const SOURCE: &str = "owners";

fn source(kind: DriverKind) -> Source {
    Source {
        id: SourceId::new(SOURCE),
        name: "PetClinic".into(),
        kind,
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
    vec![Table {
        schema: "PUBLIC".into(),
        name: "OWNERS".into(),
        kind: TableKind::Table,
        columns: vec![column("ID", "INTEGER"), column("NAME", "VARCHAR")],
        indexes: Vec::new(),
        foreign_keys: Vec::new(),
        partial: false,
    }]
}

/// The rows of a `SELECT NAME FROM OWNERS`.
fn names(names: &[&str]) -> ResultSet {
    ResultSet {
        columns: vec![column("NAME", "VARCHAR")],
        rows: names
            .iter()
            .map(|name| vec![Value::Text((*name).to_owned())])
            .collect(),
        update_count: None,
        truncated: false,
    }
}

/// The answer of an `INSERT` or a `DELETE`.
fn changed(count: u64) -> ResultSet {
    ResultSet {
        update_count: Some(count),
        ..ResultSet::default()
    }
}

fn panel(cx: &TestAppContext) -> gpui::Entity<DatabasePanel> {
    cx.update(|cx| cx.global::<ActiveDatabase>().0.clone())
}

/// Opens the window, shows Database and hands the source a scripted connection.
fn source_shown(
    cx: &mut TestAppContext,
    name: &str,
    kind: DriverKind,
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
            source(kind),
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

fn console_of(cx: &TestAppContext, panel: &gpui::Entity<DatabasePanel>) -> PathBuf {
    panel
        .read_with(cx, |panel, _| panel.console_path(&SourceId::new(SOURCE)))
        .expect("the source has a console")
}

fn shown(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

/// Waits until the tab of the console is open: the panel opens it in the background.
fn wait_for_console(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    console: &Path,
) -> PathBuf {
    let path = console.to_path_buf();
    let wanted = path.clone();
    wait_for_signal(cx, "the tab of the console", move |cx| {
        draw(cx, window);
        window
            .read_with(cx, |workspace, _| workspace.view_of(&wanted).is_some())
            .unwrap_or(false)
    });
    path
}

/// Runs the statement at the caret of the open console.
fn run_statement(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    press(cx, window, "ctrl-enter");
    draw(cx, window);
}

fn summaries(cx: &TestAppContext, panel: &gpui::Entity<DatabasePanel>) -> Vec<String> {
    panel.read_with(cx, |panel, _| {
        panel
            .results_for_test()
            .into_iter()
            .map(|(_, summary, _)| summary)
            .collect()
    })
}

#[ui_kit::gpui::test]
fn a_manual_transaction_keeps_its_rows_until_commit_and_loses_them_on_rollback(
    cx: &mut TestAppContext,
) {
    let (window, panel) = source_shown(cx, "sql-transaction", DriverKind::H2, names(&["Ann"]));
    panel.update(cx, |panel, _| {
        panel.scripted_console_replies_for_test(vec![
            changed(1),
            names(&[]),
            changed(1),
            names(&["Ann"]),
        ]);
    });
    let console = console_of(cx, &panel);
    fs::write(
        &console,
        "INSERT INTO OWNERS (NAME) VALUES ('Ann');\nSELECT NAME FROM OWNERS;\n",
    )
    .unwrap();
    open_path(cx, window, &console);

    click(cx, window, "database-console-manual");
    draw(cx, window);
    assert!(
        shown(cx, window, "database-console-commit"),
        "Manual offers Commit"
    );
    // The click left the focus in the panel; the keys belong to the console again.
    open_path(cx, window, &console);

    press(cx, window, "ctrl-home");
    run_statement(cx, window);
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.pending_count(&SourceId::new(SOURCE))),
        1,
        "the inserted row waits for its transaction"
    );
    draw(cx, window);
    assert!(
        shown(cx, window, "database-console-pending"),
        "the console says how many statements wait"
    );

    press(cx, window, "down");
    run_statement(cx, window);
    let rows = summaries(cx, &panel);
    assert!(
        rows.last()
            .is_some_and(|summary| summary.contains("0 rows")),
        "the row of the rolled back transaction is not there: {rows:?}"
    );

    click(cx, window, "database-console-rollback");
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.pending_count(&SourceId::new(SOURCE))),
        0,
        "Rollback ends the pending changes"
    );
    assert!(
        panel
            .read_with(cx, |panel, _| panel.scripted_commands_for_test())
            .contains(&ScriptedCommand::Rollback),
        "the driver was asked to roll back"
    );

    // The same statement in a committed transaction leaves its row.
    open_path(cx, window, &console);
    press(cx, window, "ctrl-home");
    run_statement(cx, window);
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.pending_count(&SourceId::new(SOURCE))),
        1
    );
    click(cx, window, "database-console-commit");
    assert!(
        panel
            .read_with(cx, |panel, _| panel.scripted_commands_for_test())
            .contains(&ScriptedCommand::Commit),
        "the driver was asked to commit"
    );
    open_path(cx, window, &console);
    press(cx, window, "down");
    run_statement(cx, window);
    let rows = summaries(cx, &panel);
    assert!(
        rows.last().is_some_and(|summary| summary.contains("1 row")),
        "the committed row is there: {rows:?}"
    );
}

#[ui_kit::gpui::test]
fn closing_a_console_with_pending_changes_asks_commit_rollback_or_cancel(cx: &mut TestAppContext) {
    let (window, panel) = source_shown(cx, "sql-transaction-close", DriverKind::H2, names(&[]));
    panel.update(cx, |panel, _| {
        panel.scripted_console_replies_for_test(vec![changed(1)]);
    });
    let console = console_of(cx, &panel);
    fs::write(&console, "INSERT INTO OWNERS (NAME) VALUES ('Ann');\n").unwrap();

    click(cx, window, "database-console");
    wait_for_console(cx, window, &console);

    click(cx, window, "database-console-manual");
    open_path(cx, window, &console);
    run_statement(cx, window);
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.pending_count(&SourceId::new(SOURCE))),
        1
    );

    press(cx, window, "ctrl-f4");
    draw(cx, window);
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.transaction_question_for_test()),
        Some(SOURCE.to_owned()),
        "the close asks before it drops the transaction"
    );
    assert!(
        shown(cx, window, "database-transaction-question"),
        "the question has its three answers"
    );
    assert_eq!(
        editor_of(cx, window, &console).read_with(cx, |view, _| view.text()),
        Some("INSERT INTO OWNERS (NAME) VALUES ('Ann');\n".to_owned()),
        "the console is still open"
    );

    click(cx, window, "database-transaction-cancel");
    draw(cx, window);
    assert!(
        panel
            .read_with(cx, |panel, _| panel.transaction_question_for_test())
            .is_none(),
        "Cancel keeps the transaction"
    );

    press(cx, window, "ctrl-f4");
    draw(cx, window);
    click(cx, window, "database-transaction-rollback");
    wait_for_signal(cx, "the close of the console", move |cx| {
        draw(cx, window);
        window
            .read_with(cx, |workspace, _| workspace.view_of(&console).is_none())
            .unwrap_or(false)
    });
    assert!(
        panel
            .read_with(cx, |panel, _| panel.scripted_commands_for_test())
            .contains(&ScriptedCommand::Rollback),
        "Rollback came before the tab went"
    );
}

#[ui_kit::gpui::test]
fn an_export_reads_every_row_of_the_statement_into_a_file(cx: &mut TestAppContext) {
    let (window, panel) = source_shown(
        cx,
        "sql-export",
        DriverKind::H2,
        names(&["Ann", "Bob", "Cid"]),
    );
    // The console shows three rows; the export of the whole statement reads five hundred.
    let many: Vec<String> = (0..500).map(|index| format!("Owner {index}")).collect();
    let many: Vec<&str> = many.iter().map(String::as_str).collect();
    panel.update(cx, |panel, _| {
        panel.scripted_console_replies_for_test(vec![names(&["Ann", "Bob", "Cid"]), names(&many)]);
    });
    let console = console_of(cx, &panel);
    fs::write(&console, "SELECT NAME FROM OWNERS;\n").unwrap();
    open_path(cx, window, &console);

    run_statement(cx, window);
    draw(cx, window);
    assert!(
        shown(cx, window, "database-result-export"),
        "a statement that only reads may be exported"
    );

    let target = console.parent().unwrap().join("owners.csv");
    click(cx, window, "database-result-export");
    draw(cx, window);
    assert!(shown(cx, window, "database-export"));
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.export_dialog_for_test()),
        Some(database::ExportFormat::Csv),
        "the form offers CSV first"
    );
    click(cx, window, "database-export-save");
    cx.simulate_new_path_selection({
        let target = target.clone();
        move |_| Some(target.clone())
    });
    wait_for_signal(cx, "the exported file", |_| target.is_file());

    let text = fs::read_to_string(&target).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.first(), Some(&"NAME"), "{text:?}");
    assert_eq!(lines.len(), 501, "the header and every row: {text:?}");
    assert_eq!(lines.last(), Some(&"Owner 499"), "{text:?}");
    let queries = panel.read_with(cx, |panel, _| panel.queries_for_test());
    assert_eq!(
        queries
            .last()
            .map(|(sql, max_rows)| (sql.as_str(), *max_rows)),
        Some(("SELECT NAME FROM OWNERS", None)),
        "the export runs the statement again without a row limit: {queries:?}"
    );
}

#[ui_kit::gpui::test]
fn explain_plan_opens_the_plan_of_the_statement_as_a_tree(cx: &mut TestAppContext) {
    let (window, panel) = source_shown(cx, "sql-explain", DriverKind::Sqlite, names(&[]));
    let plan = ResultSet {
        columns: vec![
            column("id", "INTEGER"),
            column("parent", "INTEGER"),
            column("notused", "INTEGER"),
            column("detail", "TEXT"),
        ],
        rows: vec![
            vec![
                Value::Integer(2),
                Value::Integer(0),
                Value::Integer(0),
                Value::Text("SCAN OWNERS".into()),
            ],
            vec![
                Value::Integer(3),
                Value::Integer(2),
                Value::Integer(0),
                Value::Text("SEARCH USING INDEX ID".into()),
            ],
        ],
        update_count: None,
        truncated: false,
    };
    panel.update(cx, |panel, _| {
        panel.scripted_console_replies_for_test(vec![plan]);
    });
    let console = console_of(cx, &panel);
    fs::write(&console, "SELECT * FROM OWNERS WHERE ID = 1;\n").unwrap();
    open_path(cx, window, &console);

    press(cx, window, "alt-shift-e");
    wait_for_signal(cx, "the plan of the statement", |cx| {
        !panel
            .read_with(cx, |panel, _| panel.plans_for_test())
            .is_empty()
    });
    let plans = panel.read_with(cx, |panel, _| panel.plans_for_test());
    let (statement, plan) = &plans[0];
    assert_eq!(
        statement, "EXPLAIN QUERY PLAN SELECT * FROM OWNERS WHERE ID = 1",
        "SQLite answers a query plan as steps"
    );
    let plan = plan.as_ref().expect("the plan is read");
    assert_eq!(plan.nodes.len(), 1);
    assert_eq!(plan.nodes[0].label, "SCAN OWNERS");
    assert_eq!(plan.nodes[0].children[0].label, "SEARCH USING INDEX ID");
    draw(cx, window);
    let row = bounds_of(cx, window, "database-result-plan-row");
    assert!(row.size.width > ui_kit::px(0.), "the plan is drawn");
    assert!(
        panel.read_with(cx, |panel, _| panel.results_for_test())[0]
            .0
            .starts_with("Plan:"),
        "the plan has a tab of its own"
    );
}

#[ui_kit::gpui::test]
fn a_statement_with_a_placeholder_asks_for_its_value_and_finds_the_row(cx: &mut TestAppContext) {
    let (window, panel) = source_shown(cx, "sql-parameters", DriverKind::H2, names(&["Ann"]));
    panel.update(cx, |panel, _| {
        panel.scripted_console_replies_for_test(vec![names(&["Ann"])]);
    });
    let console = console_of(cx, &panel);
    fs::write(&console, "SELECT NAME FROM OWNERS WHERE ID = :id;\n").unwrap();
    open_path(cx, window, &console);

    run_statement(cx, window);
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.asked_parameters_for_test()),
        ["id"],
        "the console asks for the value of :id"
    );
    draw(cx, window);
    assert!(
        shown(cx, window, "database-parameter-0"),
        "the value has its field"
    );

    click(cx, window, "database-parameter-0");
    type_text(cx, window, "1");
    click(cx, window, "database-parameters-run");
    wait_for_signal(cx, "the answered statement", |cx| {
        !panel
            .read_with(cx, |panel, _| panel.parameter_queries_for_test())
            .is_empty()
    });

    let sent = panel.read_with(cx, |panel, _| panel.parameter_queries_for_test());
    assert_eq!(
        sent.last().map(|(_, params)| params.clone()),
        Some(vec![Param::Integer(1)]),
        "the value goes to the driver as a parameter: {sent:?}"
    );
    let summaries = summaries(cx, &panel);
    assert!(
        summaries
            .last()
            .is_some_and(|summary| summary.contains("1 row")),
        "the statement found its row: {summaries:?}"
    );
    assert!(
        panel
            .read_with(cx, |panel, _| panel.asked_parameters_for_test())
            .is_empty(),
        "the question is over"
    );
}

#[ui_kit::gpui::test]
fn a_statement_that_writes_offers_no_export(cx: &mut TestAppContext) {
    let (window, panel) = source_shown(cx, "sql-export-write", DriverKind::H2, names(&[]));
    panel.update(cx, |panel, _| {
        panel.scripted_console_replies_for_test(vec![changed(1)]);
    });
    let console = console_of(cx, &panel);
    fs::write(&console, "DELETE FROM OWNERS WHERE ID = 1;\n").unwrap();
    open_path(cx, window, &console);

    run_statement(cx, window);
    draw(cx, window);
    assert!(
        results_visible(cx, window, "database-result-affected"),
        "the statement changed its rows"
    );
    assert!(
        !shown(cx, window, "database-result-export"),
        "running a write again would write twice, so Export is not offered"
    );
}

/// Whether an element is drawn, for a test that does not need its place.
fn results_visible(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &'static str,
) -> bool {
    shown(cx, window, selector)
}
