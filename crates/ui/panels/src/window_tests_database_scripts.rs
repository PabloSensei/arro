//! T-0302: the third step of the SQL console without a window. Ctrl+Alt+L reformats a
//! statement, Tab expands the `sel` template, and Run SQL Script of a `.sql` file of the
//! project runs every statement against the source picked in the bar above the file and
//! stops at the first error.
//!
//! `cargo test -p panels window_tests_database_scripts`

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use database::{
    Column, ColumnKind, DriverKind, ResultSet, Source, SourceId, Table, TableKind, Value,
};
use ui_kit::Modifiers;
use ui_kit::gpui;
use ui_kit::gpui::{MouseButton, TestAppContext, VisualTestContext, WindowHandle};
use workspace::{DATABASE_TOOL, Workspace};

use crate::database_panel::{ActiveDatabase, DatabasePanel};
use crate::window_tests::{
    bounds_of, buffer_of, click, draw, editor_of, open_path, open_window, press, sample_project,
    type_text, wait_for_signal,
};
use crate::window_tests_menus::click_menu_row;

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

fn rows() -> ResultSet {
    ResultSet {
        columns: vec![column("ID", "INTEGER")],
        rows: vec![vec![Value::Integer(1)]],
        update_count: None,
        truncated: false,
    }
}

fn panel(cx: &TestAppContext) -> gpui::Entity<DatabasePanel> {
    cx.update(|cx| cx.global::<ActiveDatabase>().0.clone())
}

/// Opens the window, shows Database and hands the source a scripted connection.
fn source_shown(
    cx: &mut TestAppContext,
    name: &str,
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
            rows(),
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

/// A right click inside the text of the open editor.
fn right_click_editor(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    let position = bounds_of(cx, window, "editor-body-0").center();
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_down(position, MouseButton::Right, Modifiers::default());
    visual.simulate_mouse_up(position, MouseButton::Right, Modifiers::default());
    drop(visual);
    cx.run_until_parked();
    draw(cx, window);
}

#[ui_kit::gpui::test]
fn ctrl_alt_l_formats_the_statement_of_the_console(cx: &mut TestAppContext) {
    let (window, panel) = source_shown(cx, "sql-format");
    let console = console_of(cx, &panel);
    fs::write(&console, "select id,name from owners where id=1\n").unwrap();
    open_path(cx, window, &console);
    let view = editor_of(cx, window, &console);

    press(cx, window, "ctrl-alt-l");
    wait_for_signal(cx, "the reformatted statement", |cx| {
        buffer_of(cx, &view).contains("SELECT\n    id,")
    });

    assert_eq!(
        buffer_of(cx, &view),
        "SELECT\n    id,\n    name\nFROM owners\nWHERE id = 1\n"
    );
}

#[ui_kit::gpui::test]
fn sel_and_tab_expand_the_select_template(cx: &mut TestAppContext) {
    let (window, panel) = source_shown(cx, "sql-template");
    let console = console_of(cx, &panel);
    fs::write(&console, "").unwrap();
    open_path(cx, window, &console);
    let view = editor_of(cx, window, &console);

    type_text(cx, window, "sel");
    press(cx, window, "tab");

    assert_eq!(buffer_of(cx, &view), "SELECT * FROM ;");
    assert_eq!(
        view.read_with(cx, |view, _| view.caret()),
        (0, "SELECT * FROM ".len()),
        "the caret waits where the table goes"
    );
}

#[ui_kit::gpui::test]
fn a_project_sql_script_runs_until_the_first_error(cx: &mut TestAppContext) {
    let (window, panel) = source_shown(cx, "sql-script");
    panel.update(cx, |panel, _| {
        panel.scripted_console_answers_for_test(vec![Ok(rows()), Err("boom".into())]);
    });
    let root = window
        .read_with(cx, |workspace, _| workspace.root().to_path_buf())
        .unwrap();
    let script = root.join("scripts").join("report.sql");
    fs::create_dir_all(script.parent().unwrap()).unwrap();
    fs::write(&script, "SELECT 1;\nSELECT broken;\nSELECT 3;\n").unwrap();
    open_path(cx, window, &script);
    draw(cx, window);

    // The bar above the file picks the source its statements run against.
    click(cx, window, "database-sql-source");
    draw(cx, window);
    click_menu_row(cx, window, "PetClinic");
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.file_source(&script)),
        Some(SourceId::new(SOURCE)),
        "the bar keeps the source of the script"
    );

    // The menu of the file runs the whole script.
    right_click_editor(cx, window);
    click_menu_row(cx, window, "Run SQL Script");
    wait_for_signal(cx, "the results of the script", |cx| {
        panel.read_with(cx, |panel, _| panel.results_for_test().len() == 2)
    });

    let results = panel.read_with(cx, |panel, _| panel.results_for_test());
    assert_eq!(results.len(), 2, "the failing statement has its tab");
    assert!(
        results[1]
            .2
            .as_deref()
            .is_some_and(|error| error.contains("boom")),
        "the second statement failed: {results:?}"
    );
    let queries = panel.read_with(cx, |panel, _| panel.queries_for_test());
    assert_eq!(
        queries.len(),
        2,
        "the script stopped before the third statement: {queries:?}"
    );
    assert!(
        queries.iter().all(|(sql, _)| !sql.contains('3')),
        "the third statement never ran: {queries:?}"
    );

    // The picked source also answers the names of the file: `o` offers the table.
    open_path(cx, window, &script);
    press(cx, window, "ctrl-end");
    type_text(cx, window, "\nSELECT * FROM o");
    press(cx, window, "ctrl-space");
    wait_for_signal(cx, "the completion of the project script", |cx| {
        draw(cx, window);
        shown(cx, window, "completion-popup")
    });
    press(cx, window, "enter");
    assert!(
        buffer_of(cx, &editor_of(cx, window, &script)).ends_with("SELECT * FROM OWNERS"),
        "the table of the source completes in the file"
    );
}

/// Whether an element is drawn, for a test that does not need its place.
fn shown(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}
