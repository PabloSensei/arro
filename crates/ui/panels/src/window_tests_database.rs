//! T-0206: the Database window without a window. A source is added, its tables appear,
//! Ctrl+Enter in its console shows the rows or the error of the driver, and closing the
//! window closes the connection to the helper.
//!
//! `cargo test -p panels window_tests_database`

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]

use std::fs;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use database::{
    Column, ColumnKind, DriverKind, ResultSet, Source, SourceId, Table, TableKind, Value,
};
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use workspace::{DATABASE_TOOL, Workspace};

use crate::database_panel::{ActiveDatabase, DatabasePanel};
use crate::window_tests::{draw, open_path, open_window, press, sample_project, wait_for_signal};

const SOURCE: &str = "owners";

fn source() -> Source {
    Source {
        id: SourceId::new(SOURCE),
        name: "PetClinic".into(),
        kind: DriverKind::H2,
        // The window keeps the address as the driver spells it; the test does not read it.
        url: "mem:petclinic".into(),
        user: "sa".into(),
        module: None,
        driver_jar: None,
    }
}

fn tables() -> Vec<Table> {
    vec![Table {
        schema: "PUBLIC".into(),
        name: "OWNERS".into(),
        kind: TableKind::Table,
        columns: vec![Column {
            name: "ID".into(),
            type_name: "INTEGER".into(),
            kind: ColumnKind::Integer,
            nullable: false,
            primary_key: true,
        }],
        indexes: Vec::new(),
        foreign_keys: Vec::new(),
        partial: false,
    }]
}

fn rows() -> ResultSet {
    ResultSet {
        columns: vec![Column {
            name: "ID".into(),
            type_name: "INTEGER".into(),
            kind: ColumnKind::Integer,
            nullable: false,
            primary_key: true,
        }],
        rows: vec![vec![Value::Integer(1)]],
        update_count: None,
        truncated: false,
    }
}

fn panel(cx: &TestAppContext) -> gpui::Entity<DatabasePanel> {
    cx.update(|cx| cx.global::<ActiveDatabase>().0.clone())
}

/// Opens the window, shows Database and hands the source to the panel with a scripted
/// connection: the helper is not started, its answers are known in advance.
fn source_shown(
    cx: &mut TestAppContext,
    name: &str,
    error: Option<String>,
    rows: ResultSet,
) -> (
    WindowHandle<Workspace>,
    gpui::Entity<DatabasePanel>,
    Arc<AtomicBool>,
) {
    let root = sample_project(name);
    let window = open_window(cx, root);
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(DATABASE_TOOL, window, cx)
        })
        .unwrap();
    draw(cx, window);
    let panel = panel(cx);
    let closed = Arc::new(AtomicBool::new(false));
    let flag = closed.clone();
    panel.update(cx, |panel, cx| {
        panel.connect_for_test(source(), tables(), rows, error, flag, cx);
    });
    (window, panel, closed)
}

#[ui_kit::gpui::test]
fn a_source_shows_its_tables(cx: &mut TestAppContext) {
    let (_, panel, _) = source_shown(cx, "database-tables", None, rows());

    wait_for_signal(cx, "the tables of the source", |cx| {
        panel.read_with(cx, |panel, _| {
            !panel.tables(&SourceId::new(SOURCE)).is_empty()
        })
    });

    let tables = panel.read_with(cx, |panel, _| panel.tables(&SourceId::new(SOURCE)));
    assert_eq!(tables.len(), 1);
    assert_eq!(tables[0].name, "OWNERS");
    assert_eq!(tables[0].columns[0].name, "ID");
    assert!(tables[0].columns[0].primary_key);
}

#[ui_kit::gpui::test]
fn ctrl_enter_in_the_console_shows_rows(cx: &mut TestAppContext) {
    let (window, panel, _) = source_shown(cx, "database-console", None, rows());
    let console = panel
        .read_with(cx, |panel, _| panel.console_path(&SourceId::new(SOURCE)))
        .expect("the source has a console");
    fs::write(&console, "SELECT ID FROM OWNERS;\n").unwrap();
    open_path(cx, window, &console);

    press(cx, window, "ctrl-enter");

    wait_for_signal(cx, "the rows of the statement", |cx| {
        panel.read_with(cx, |panel, _| {
            panel.result().is_some_and(|result| result.rows.len() == 1)
        })
    });
    let result = panel
        .read_with(cx, |panel, _| panel.result())
        .expect("the statement has a result");
    assert_eq!(result.columns[0].name, "ID");
    assert_eq!(result.rows[0][0], Value::Integer(1));
    assert_eq!(panel.read_with(cx, |panel, _| panel.last_error()), None);
}

#[ui_kit::gpui::test]
fn a_bad_statement_shows_the_error_of_the_driver(cx: &mut TestAppContext) {
    let error = "Table \"NOPE\" not found; SQL statement:\nSELECT * FROM NOPE [42102-224]";
    let (window, panel, _) = source_shown(
        cx,
        "database-error",
        Some(error.to_owned()),
        ResultSet::default(),
    );
    let console = panel
        .read_with(cx, |panel, _| panel.console_path(&SourceId::new(SOURCE)))
        .expect("the source has a console");
    fs::write(&console, "SELECT * FROM NOPE;\n").unwrap();
    open_path(cx, window, &console);

    press(cx, window, "ctrl-enter");

    wait_for_signal(cx, "the error of the driver", |cx| {
        panel.read_with(cx, |panel, _| panel.last_error().is_some())
    });
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.last_error()),
        Some(error.to_owned())
    );
}

#[ui_kit::gpui::test]
fn closing_the_window_closes_the_connection(cx: &mut TestAppContext) {
    let (window, _, closed) = source_shown(cx, "database-close", None, rows());
    assert!(!closed.load(Ordering::Relaxed), "the connection is open");

    let mut visual = VisualTestContext::from_window(window.into(), cx);
    assert!(
        visual.simulate_close(),
        "the cross asks to close the window"
    );
    drop(visual);
    window
        .update(cx, |_, window, _| window.remove_window())
        .unwrap();
    cx.run_until_parked();

    wait_for_signal(cx, "the closed connection", |_| {
        closed.load(Ordering::Relaxed)
    });
}
