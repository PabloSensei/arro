//! T-0251: the data tab of the Database window without a window. Double click on a table
//! opens its rows, the pages move, a cell takes a new text, rows are added and deleted,
//! Submit runs one transaction and asks the helper to roll it back when a statement
//! fails, and closing a tab with edits asks first.
//!
//! `cargo test -p panels window_tests_database_data`

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use database::{
    Column, ColumnKind, DriverKind, ResultSet, Source, SourceId, Table, TableKind, Value,
};
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use workspace::{DATABASE_TOOL, Workspace};

use crate::database_data::Question;
use crate::database_panel::{ActiveDatabase, DatabasePanel, ScriptedCommand, ScriptedReply};
use crate::window_tests::{draw, open_window, press, sample_project, type_text, wait_for_signal};

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

fn columns() -> Vec<Column> {
    vec![
        Column {
            name: "ID".into(),
            type_name: "INTEGER".into(),
            kind: ColumnKind::Integer,
            nullable: false,
            primary_key: true,
        },
        Column {
            name: "NAME".into(),
            type_name: "VARCHAR".into(),
            kind: ColumnKind::Text,
            nullable: true,
            primary_key: false,
        },
        Column {
            name: "AGE".into(),
            type_name: "INTEGER".into(),
            kind: ColumnKind::Integer,
            nullable: true,
            primary_key: false,
        },
    ]
}

fn table() -> Table {
    Table {
        schema: "PUBLIC".into(),
        name: "OWNERS".into(),
        kind: TableKind::Table,
        columns: columns(),
        indexes: Vec::new(),
        foreign_keys: Vec::new(),
        partial: false,
    }
}

fn view() -> Table {
    Table {
        kind: TableKind::View,
        ..table()
    }
}

fn page(rows: Vec<Vec<Value>>) -> ScriptedReply {
    ScriptedReply {
        result: ResultSet {
            columns: columns(),
            rows,
            update_count: None,
            truncated: false,
        },
        error: None,
    }
}

fn owners() -> Vec<Vec<Value>> {
    vec![
        vec![
            Value::Integer(1),
            Value::Text("Ann".into()),
            Value::Integer(30),
        ],
        vec![
            Value::Integer(2),
            Value::Text("Bob".into()),
            Value::Integer(40),
        ],
    ]
}

fn count(rows: i64) -> ScriptedReply {
    ScriptedReply {
        result: ResultSet {
            columns: vec![Column {
                name: "COUNT(*)".into(),
                type_name: "BIGINT".into(),
                kind: ColumnKind::Integer,
                nullable: false,
                primary_key: false,
            }],
            rows: vec![vec![Value::Integer(rows)]],
            update_count: None,
            truncated: false,
        },
        error: None,
    }
}

fn update() -> ScriptedReply {
    ScriptedReply {
        result: ResultSet {
            update_count: Some(1),
            ..ResultSet::default()
        },
        error: None,
    }
}

fn panel(cx: &TestAppContext) -> gpui::Entity<DatabasePanel> {
    cx.update(|cx| cx.global::<ActiveDatabase>().0.clone())
}

/// Opens the window, shows Database, gives the panel a scripted connection and opens the
/// rows of `table` through the menu action of the tree.
fn data_tab(
    cx: &mut TestAppContext,
    name: &str,
    table: Table,
    replies: Vec<ScriptedReply>,
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
            vec![table.clone()],
            ResultSet::default(),
            None,
            Arc::new(AtomicBool::new(false)),
            cx,
        );
        panel.scripted_replies_for_test(replies);
    });
    let opened = table.clone();
    let source = SourceId::new(SOURCE);
    // The panel's own listener opens the tab, so the window is not borrowed while the
    // workspace is read.
    crate::window_tests::with_window(cx, window, |window, cx| {
        panel.update(cx, |panel, cx| {
            panel.open_data(source.clone(), opened.clone(), window, cx)
        });
    });
    draw(cx, window);
    (window, panel)
}

fn data_view(
    cx: &TestAppContext,
    panel: &gpui::Entity<DatabasePanel>,
    table: &Table,
) -> Option<gpui::Entity<crate::database_data::DatabaseDataView>> {
    cx.read(|app| {
        panel
            .read(app)
            .data_view_for_test(&SourceId::new(SOURCE), table, app)
    })
}

fn wait_for_rows(
    cx: &mut TestAppContext,
    panel: &gpui::Entity<DatabasePanel>,
    table: &Table,
    rows: usize,
) -> gpui::Entity<crate::database_data::DatabaseDataView> {
    wait_for_signal(cx, "the rows of the data tab", |cx| {
        data_view(cx, panel, table).is_some_and(|view| {
            cx.read(|app| {
                view.read(app).model_for_test().row_count() == rows
                    && view.read(app).total_for_test().is_some()
            })
        })
    });
    data_view(cx, panel, table).expect("the data tab is open")
}

#[ui_kit::gpui::test]
fn a_double_clicked_table_shows_its_rows_and_count(cx: &mut TestAppContext) {
    let (_, panel) = data_tab(
        cx,
        "database-data-rows",
        table(),
        vec![page(owners()), count(2)],
    );
    let view = wait_for_rows(cx, &panel, &table(), 2);

    let queries = panel.read_with(cx, |panel, _| panel.queries_for_test());
    assert_eq!(
        queries.first().map(|(sql, _)| sql.as_str()),
        Some("SELECT * FROM \"PUBLIC\".\"OWNERS\" LIMIT 500 OFFSET 0"),
        "{queries:?}"
    );
    assert_eq!(queries[0].1, Some(500));
    assert_eq!(
        queries.get(1).map(|(sql, _)| sql.as_str()),
        Some("SELECT COUNT(*) FROM \"PUBLIC\".\"OWNERS\"")
    );
    assert_eq!(view.read_with(cx, |view, _| view.total_for_test()), Some(2));
    assert!(!view.read_with(cx, |view, _| view.is_read_only()));
}

#[ui_kit::gpui::test]
fn the_pages_move_forward_and_back(cx: &mut TestAppContext) {
    let (window, panel) = data_tab(
        cx,
        "database-data-pages",
        table(),
        vec![
            page(owners()),
            count(3),
            page(vec![vec![
                Value::Integer(3),
                Value::Text("Cid".into()),
                Value::Null,
            ]]),
            count(3),
            page(owners()),
            count(3),
        ],
    );
    let view = wait_for_rows(cx, &panel, &table(), 2);

    crate::window_tests::click(cx, window, "database-data-next");
    wait_for_signal(cx, "the second page", |cx| {
        view.read_with(cx, |view, _| view.page_for_test() == 1)
            && view.read_with(cx, |view, _| view.model_for_test().row_count() == 1)
    });
    let queries = panel.read_with(cx, |panel, _| panel.queries_for_test());
    assert!(
        queries
            .iter()
            .any(|(sql, _)| sql.contains("LIMIT 500 OFFSET 500")),
        "{queries:?}"
    );

    crate::window_tests::click(cx, window, "database-data-previous");
    wait_for_signal(cx, "the first page again", |cx| {
        view.read_with(cx, |view, _| view.page_for_test() == 0)
            && view.read_with(cx, |view, _| view.model_for_test().row_count() == 2)
    });
}

#[ui_kit::gpui::test]
fn a_cell_takes_a_new_text(cx: &mut TestAppContext) {
    let (window, panel) = data_tab(
        cx,
        "database-data-edit",
        table(),
        vec![page(owners()), count(2)],
    );
    let view = wait_for_rows(cx, &panel, &table(), 2);

    press(cx, window, "down");
    press(cx, window, "right");
    press(cx, window, "enter");
    type_text(cx, window, " Lee");
    press(cx, window, "enter");

    let text = view.read_with(cx, |view, _| {
        view.model_for_test()
            .row(0)
            .and_then(|row| row.cell(1).cloned())
    });
    assert_eq!(text, Some(Value::Text("Ann Lee".into())));
    assert_eq!(
        view.read_with(cx, |view, _| view.selected_for_test()),
        Some((0, 1)),
        "the edited cell keeps the selection"
    );
    assert!(view.read_with(cx, |view, _| view.model_for_test().is_dirty()));
}

#[ui_kit::gpui::test]
fn the_cell_field_draws_a_caret_and_takes_the_keys_of_a_field(cx: &mut TestAppContext) {
    let (window, panel) = data_tab(
        cx,
        "database-data-field",
        table(),
        vec![page(owners()), count(2)],
    );
    let view = wait_for_rows(cx, &panel, &table(), 2);

    press(cx, window, "down");
    press(cx, window, "right");
    press(cx, window, "enter");
    draw(cx, window);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    assert!(
        visual
            .debug_bounds("database-data-cell-field-caret")
            .is_some(),
        "the field of a cell draws its caret"
    );
    drop(visual);

    press(cx, window, "ctrl-a");
    draw(cx, window);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    assert!(
        visual
            .debug_bounds("database-data-cell-field-selection")
            .is_some(),
        "Ctrl+A selects the text of the field"
    );
    drop(visual);

    cx.update(|cx| cx.write_to_clipboard(ui_kit::ClipboardItem::new_string("Zed".to_owned())));
    press(cx, window, "ctrl-v");
    press(cx, window, "enter");
    assert_eq!(
        view.read_with(cx, |view, _| {
            view.model_for_test()
                .row(0)
                .and_then(|row| row.cell(1).cloned())
        }),
        Some(Value::Text("Zed".into())),
        "Ctrl+V replaces the selection of the field"
    );
}

#[ui_kit::gpui::test]
fn alt_insert_adds_a_row_and_ctrl_delete_marks_one(cx: &mut TestAppContext) {
    let (window, panel) = data_tab(
        cx,
        "database-data-rows-new",
        table(),
        vec![page(owners()), count(2)],
    );
    let view = wait_for_rows(cx, &panel, &table(), 2);

    press(cx, window, "alt-insert");
    wait_for_signal(cx, "the new row", |cx| {
        view.read_with(cx, |view, _| view.model_for_test().row_count() == 3)
    });
    assert_eq!(
        view.read_with(cx, |view, _| view
            .model_for_test()
            .row(2)
            .map(|row| row.kind())),
        Some(database::RowKind::New)
    );

    press(cx, window, "up");
    press(cx, window, "up");
    press(cx, window, "ctrl-delete");
    assert_eq!(
        view.read_with(cx, |view, _| view
            .model_for_test()
            .row(0)
            .map(|row| row.kind())),
        Some(database::RowKind::Deleted)
    );
}

#[ui_kit::gpui::test]
fn submit_sends_every_statement_in_one_transaction(cx: &mut TestAppContext) {
    let (window, panel) = data_tab(
        cx,
        "database-data-submit",
        table(),
        vec![
            page(owners()),
            count(2),
            update(),
            update(),
            update(),
            page(owners()),
            count(2),
        ],
    );
    let view = wait_for_rows(cx, &panel, &table(), 2);

    // Row 0 changes its name, a new row goes in, row 1 is deleted.
    press(cx, window, "down");
    press(cx, window, "right");
    press(cx, window, "enter");
    type_text(cx, window, " Lee");
    press(cx, window, "enter");
    press(cx, window, "alt-insert");
    press(cx, window, "enter");
    type_text(cx, window, "9");
    press(cx, window, "enter");
    press(cx, window, "right");
    press(cx, window, "enter");
    type_text(cx, window, "Cid");
    press(cx, window, "enter");
    press(cx, window, "up");
    press(cx, window, "ctrl-delete");
    press(cx, window, "ctrl-enter");

    wait_for_signal(cx, "the commit of the transaction", |cx| {
        let commands = panel.read_with(cx, |panel, _| panel.scripted_commands_for_test());
        commands
            .iter()
            .any(|command| matches!(command, ScriptedCommand::Commit))
    });

    let commands = panel.read_with(cx, |panel, _| panel.scripted_commands_for_test());
    let texts: Vec<String> = commands
        .iter()
        .filter_map(|command| match command {
            ScriptedCommand::Query { sql, .. } => Some(sql.clone()),
            _ => None,
        })
        .collect();
    let updates = texts.iter().filter(|sql| sql.starts_with("UPDATE")).count();
    assert_eq!(updates, 1, "{texts:?}");
    assert!(
        texts.iter().any(|sql| sql.starts_with("INSERT INTO")),
        "{texts:?}"
    );
    assert!(
        texts.iter().any(|sql| sql.starts_with("DELETE FROM")),
        "{texts:?}"
    );
    // The statement texts do not follow the SELECT of the page in the same list, so the
    // transaction is read from the commands themselves.
    let order = |wanted: &dyn Fn(&ScriptedCommand) -> bool| {
        commands
            .iter()
            .position(wanted)
            .expect("the command is sent")
    };
    let auto_commit_off = order(&|command| matches!(command, ScriptedCommand::AutoCommit(false)));
    let commit = order(&|command| matches!(command, ScriptedCommand::Commit));
    let update_at = order(
        &|command| matches!(command, ScriptedCommand::Query { sql, .. } if sql.starts_with("UPDATE")),
    );
    let insert_at = order(
        &|command| matches!(command, ScriptedCommand::Query { sql, .. } if sql.starts_with("INSERT")),
    );
    let delete_at = order(
        &|command| matches!(command, ScriptedCommand::Query { sql, .. } if sql.starts_with("DELETE")),
    );
    assert!(auto_commit_off < update_at, "{commands:?}");
    assert!(update_at < delete_at, "{commands:?}");
    assert!(delete_at < insert_at, "{commands:?}");
    assert!(insert_at < commit, "{commands:?}");

    // The page is read again once the transaction is committed, so the rows lose their
    // marks and the grid shows what the table holds now.
    wait_for_signal(cx, "the page after the commit", |cx| {
        cx.read(|app| {
            let view = view.read(app);
            !view.model_for_test().is_dirty() && view.total_for_test().is_some()
        })
    });
    assert!(
        view.read_with(cx, |view, _| view.error_for_test().is_none()),
        "the transaction commits"
    );
}

#[ui_kit::gpui::test]
fn a_failed_statement_rolls_back_and_names_its_row(cx: &mut TestAppContext) {
    let error = "Value too long for column NAME";
    let (window, panel) = data_tab(
        cx,
        "database-data-rollback",
        table(),
        vec![
            page(owners()),
            count(2),
            ScriptedReply {
                result: ResultSet::default(),
                error: Some(error.to_owned()),
            },
        ],
    );
    let view = wait_for_rows(cx, &panel, &table(), 2);

    press(cx, window, "down");
    press(cx, window, "right");
    press(cx, window, "enter");
    type_text(cx, window, " Lee");
    press(cx, window, "enter");
    press(cx, window, "ctrl-enter");

    wait_for_signal(cx, "the rollback", |cx| {
        let commands = panel.read_with(cx, |panel, _| panel.scripted_commands_for_test());
        commands
            .iter()
            .any(|command| matches!(command, ScriptedCommand::Rollback))
    });
    assert_eq!(
        view.read_with(cx, |view, _| view.error_for_test()),
        Some(error.to_owned())
    );
    assert_eq!(
        view.read_with(cx, |view, _| view.row_error_for_test(0)),
        Some(error.to_owned())
    );
    assert!(
        view.read_with(cx, |view, _| view.model_for_test().is_dirty()),
        "the edits stay for a second try"
    );
}

#[ui_kit::gpui::test]
fn revert_drops_every_edit(cx: &mut TestAppContext) {
    let (window, panel) = data_tab(
        cx,
        "database-data-revert",
        table(),
        vec![page(owners()), count(2)],
    );
    let view = wait_for_rows(cx, &panel, &table(), 2);

    press(cx, window, "down");
    press(cx, window, "right");
    press(cx, window, "enter");
    type_text(cx, window, " Lee");
    press(cx, window, "enter");
    assert!(view.read_with(cx, |view, _| view.model_for_test().is_dirty()));

    crate::window_tests::click(cx, window, "database-data-revert");
    assert!(!view.read_with(cx, |view, _| view.model_for_test().is_dirty()));
    assert_eq!(
        view.read_with(cx, |view, _| {
            view.model_for_test()
                .row(0)
                .and_then(|row| row.cell(1).cloned())
        }),
        Some(Value::Text("Ann".into()))
    );
}

#[ui_kit::gpui::test]
fn a_close_with_edits_asks_before_it_drops_them(cx: &mut TestAppContext) {
    let (window, panel) = data_tab(
        cx,
        "database-data-close",
        table(),
        vec![page(owners()), count(2)],
    );
    let view = wait_for_rows(cx, &panel, &table(), 2);

    press(cx, window, "down");
    press(cx, window, "right");
    press(cx, window, "enter");
    type_text(cx, window, " Lee");
    press(cx, window, "enter");

    window
        .update(cx, |workspace, window, cx| {
            workspace.close_active_tab(window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    assert_eq!(
        view.read_with(cx, |view, _| view.question_for_test()),
        Some(Question::Unsaved),
        "the tab stays and asks"
    );
    assert!(
        data_view(cx, &panel, &table()).is_some(),
        "the tab is still open"
    );

    crate::window_tests::click(cx, window, "database-data-question-discard");
    wait_for_signal(cx, "the closed tab", |cx| {
        data_view(cx, &panel, &table()).is_none()
    });
}

#[ui_kit::gpui::test]
fn a_closing_window_keeps_its_data_tab_with_edits(cx: &mut TestAppContext) {
    let (window, panel) = data_tab(
        cx,
        "database-data-window-close",
        table(),
        vec![page(owners()), count(2)],
    );
    let view = wait_for_rows(cx, &panel, &table(), 2);

    press(cx, window, "down");
    press(cx, window, "right");
    press(cx, window, "enter");
    type_text(cx, window, " Lee");
    press(cx, window, "enter");

    let mut visual = VisualTestContext::from_window(window.into(), cx);
    assert!(
        !visual.simulate_close(),
        "the window stays while the tab has edits"
    );
    drop(visual);
    cx.run_until_parked();
    assert_eq!(
        view.read_with(cx, |view, _| view.question_for_test()),
        Some(Question::Unsaved)
    );
}

#[ui_kit::gpui::test]
fn a_view_is_read_only_and_says_why(cx: &mut TestAppContext) {
    let (window, panel) = data_tab(
        cx,
        "database-data-view",
        view(),
        vec![page(owners()), count(2)],
    );
    let view = wait_for_rows(cx, &panel, &view(), 2);

    assert!(view.read_with(cx, |view, _| view.is_read_only()));
    assert!(
        view.read_with(cx, |view, _| view.read_only_reason())
            .is_some_and(|reason| reason.contains("view"))
    );
    press(cx, window, "alt-insert");
    assert_eq!(
        view.read_with(cx, |view, _| view.model_for_test().row_count()),
        2,
        "a view takes no new row"
    );
}
