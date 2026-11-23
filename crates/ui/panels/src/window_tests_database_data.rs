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
use ui_kit::gpui::{
    Modifiers, MouseButton, Pixels, Point, TestAppContext, VisualTestContext, WindowHandle, point,
    px,
};
use workspace::{DATABASE_TOOL, Workspace};

use crate::database_data::Question;
use crate::database_panel::{ActiveDatabase, DatabasePanel, ScriptedCommand, ScriptedReply};
use crate::window_tests::{
    bounds_of, click, draw, open_window, press, sample_project, type_text, wait_for_signal,
};

const SOURCE: &str = "owners";

pub(crate) fn source() -> Source {
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

pub(crate) fn table() -> Table {
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

pub(crate) fn page(rows: Vec<Vec<Value>>) -> ScriptedReply {
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

pub(crate) fn owners() -> Vec<Vec<Value>> {
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

pub(crate) fn count(rows: i64) -> ScriptedReply {
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
pub(crate) fn data_tab(
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

pub(crate) fn wait_for_rows(
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

#[ui_kit::gpui::test]
fn a_click_on_a_cell_selects_it(cx: &mut TestAppContext) {
    let (window, panel) = data_tab(
        cx,
        "database-data-click",
        table(),
        vec![page(owners()), count(2)],
    );
    let view = wait_for_rows(cx, &panel, &table(), 2);

    click(cx, window, "database-data-cell-1-2");
    draw(cx, window);
    assert_eq!(
        view.read_with(cx, |view, _| view.selected_for_test()),
        Some((1, 2))
    );
}

/// Three owners: the third has no age, so its cell is NULL.
fn three_owners() -> Vec<Vec<Value>> {
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
        vec![Value::Integer(3), Value::Text("Cid".into()), Value::Null],
    ]
}

/// A debug selector that lives as long as the test, for the cells of the grid.
fn selector(text: String) -> &'static str {
    Box::leak(text.into_boxed_str())
}

/// The centre of a cell of the grid on the screen.
fn cell_centre(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    row: usize,
    column: usize,
) -> Point<Pixels> {
    bounds_of(
        cx,
        window,
        selector(format!("database-data-cell-{row}-{column}")),
    )
    .center()
}

/// The left edge of the element with a debug selector, in pixels.
fn left_of(cx: &mut TestAppContext, window: WindowHandle<Workspace>, name: String) -> f32 {
    f32::from(bounds_of(cx, window, selector(name)).left())
}

/// The text on the clipboard now.
fn clipboard(cx: &mut TestAppContext) -> Option<String> {
    cx.read(|cx| cx.read_from_clipboard().and_then(|item| item.text()))
}

/// Puts a text on the clipboard that a copy must replace.
fn clear_clipboard(cx: &mut TestAppContext) {
    cx.update(|cx| cx.write_to_clipboard(ui_kit::ClipboardItem::new_string("before".to_owned())));
}

/// Presses the left button at `from`, moves to `to` and releases it there.
fn drag(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    from: Point<Pixels>,
    to: Point<Pixels>,
    modifiers: Modifiers,
) {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_down(from, MouseButton::Left, modifiers);
    visual.simulate_mouse_move(to, MouseButton::Left, modifiers);
    visual.simulate_mouse_up(to, MouseButton::Left, modifiers);
    cx.run_until_parked();
}

/// A click at `at` with the keys of `modifiers` held.
fn click_at(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    at: Point<Pixels>,
    modifiers: Modifiers,
) {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_click(at, modifiers);
    cx.run_until_parked();
}

/// Moves the pointer to `at` with no button held.
fn hover(cx: &mut TestAppContext, window: WindowHandle<Workspace>, at: Point<Pixels>) {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_move(at, None::<MouseButton>, Modifiers::default());
    cx.run_until_parked();
}

/// A press and a release of the right button at `at`, which opens the menu of the place.
fn right_click(cx: &mut TestAppContext, window: WindowHandle<Workspace>, at: Point<Pixels>) {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_down(at, MouseButton::Right, Modifiers::default());
    visual.simulate_mouse_up(at, MouseButton::Right, Modifiers::default());
    cx.run_until_parked();
}

fn ctrl() -> Modifiers {
    Modifiers {
        control: true,
        ..Modifiers::default()
    }
}

fn shift() -> Modifiers {
    Modifiers {
        shift: true,
        ..Modifiers::default()
    }
}

#[ui_kit::gpui::test]
fn a_drag_selects_a_rectangle_and_ctrl_c_copies_it(cx: &mut TestAppContext) {
    let (window, panel) = data_tab(
        cx,
        "database-data-drag",
        table(),
        vec![page(three_owners()), count(3)],
    );
    let _view = wait_for_rows(cx, &panel, &table(), 3);
    draw(cx, window);

    let from = cell_centre(cx, window, 0, 0);
    let to = cell_centre(cx, window, 2, 1);
    drag(cx, window, from, to, Modifiers::default());
    draw(cx, window);
    clear_clipboard(cx);
    press(cx, window, "ctrl-c");
    assert_eq!(
        clipboard(cx).as_deref(),
        Some("1\tAnn\n2\tBob\n3\tCid"),
        "three rows of two cells"
    );
}

#[ui_kit::gpui::test]
fn shift_click_draws_the_rectangle_from_the_anchor(cx: &mut TestAppContext) {
    let (window, panel) = data_tab(
        cx,
        "database-data-shift",
        table(),
        vec![page(owners()), count(2)],
    );
    let _view = wait_for_rows(cx, &panel, &table(), 2);
    click(cx, window, "database-data-cell-0-0");
    draw(cx, window);
    let to = cell_centre(cx, window, 1, 1);
    click_at(cx, window, to, shift());
    draw(cx, window);
    clear_clipboard(cx);
    press(cx, window, "ctrl-c");
    assert_eq!(clipboard(cx).as_deref(), Some("1\tAnn\n2\tBob"));
}

#[ui_kit::gpui::test]
fn ctrl_click_takes_two_cells_of_a_row_and_the_cell_between_copies_empty(cx: &mut TestAppContext) {
    let (window, panel) = data_tab(
        cx,
        "database-data-ctrl",
        table(),
        vec![page(owners()), count(2)],
    );
    let _view = wait_for_rows(cx, &panel, &table(), 2);
    click(cx, window, "database-data-cell-0-0");
    draw(cx, window);
    let at = cell_centre(cx, window, 0, 2);
    click_at(cx, window, at, ctrl());
    draw(cx, window);
    clear_clipboard(cx);
    press(cx, window, "ctrl-c");
    assert_eq!(
        clipboard(cx).as_deref(),
        Some("1\t\t30"),
        "the cell between the two is copied empty"
    );

    // Ctrl+click on a selected cell takes it out of the selection.
    let first = cell_centre(cx, window, 0, 0);
    click_at(cx, window, first, ctrl());
    draw(cx, window);
    clear_clipboard(cx);
    press(cx, window, "ctrl-c");
    assert_eq!(clipboard(cx).as_deref(), Some("30"));
}

#[ui_kit::gpui::test]
fn ctrl_drag_adds_a_rectangle_of_its_own(cx: &mut TestAppContext) {
    let (window, panel) = data_tab(
        cx,
        "database-data-ctrl-drag",
        table(),
        vec![page(three_owners()), count(3)],
    );
    let _view = wait_for_rows(cx, &panel, &table(), 3);
    click(cx, window, "database-data-cell-0-0");
    draw(cx, window);

    let from = cell_centre(cx, window, 1, 2);
    let to = cell_centre(cx, window, 2, 2);
    drag(cx, window, from, to, ctrl());
    draw(cx, window);
    clear_clipboard(cx);
    press(cx, window, "ctrl-c");
    assert_eq!(
        clipboard(cx).as_deref(),
        Some("1\t\t\n\t\t40\n\t\tNULL"),
        "the first cell and the dragged rectangle share one grid"
    );
}

#[ui_kit::gpui::test]
fn shift_click_on_headers_selects_the_columns_between(cx: &mut TestAppContext) {
    let (window, panel) = data_tab(
        cx,
        "database-data-shift-head",
        table(),
        vec![page(owners()), count(2)],
    );
    let _view = wait_for_rows(cx, &panel, &table(), 2);
    click(cx, window, "database-data-head-0");
    draw(cx, window);
    let last = bounds_of(cx, window, "database-data-head-2").center();
    click_at(cx, window, last, shift());
    draw(cx, window);
    clear_clipboard(cx);
    press(cx, window, "ctrl-c");
    assert_eq!(
        clipboard(cx).as_deref(),
        Some("ID\tNAME\tAGE\n1\tAnn\t30\n2\tBob\t40"),
        "whole columns carry their names"
    );

    // Ctrl+click on a header takes its column out; the columns that stay are whole and keep the
    // names, and the removed column stays in the box as empty cells.
    let middle = bounds_of(cx, window, "database-data-head-1").center();
    click_at(cx, window, middle, ctrl());
    draw(cx, window);
    clear_clipboard(cx);
    press(cx, window, "ctrl-c");
    assert_eq!(
        clipboard(cx).as_deref(),
        Some("ID\tNAME\tAGE\n1\t\t30\n2\t\t40"),
        "the removed column is empty, the others keep their names"
    );
}

#[ui_kit::gpui::test]
fn a_drag_past_the_edge_scrolls_the_rows_until_the_release(cx: &mut TestAppContext) {
    let rows: Vec<Vec<Value>> = (0..60)
        .map(|index| {
            vec![
                Value::Integer(index + 1),
                Value::Text(format!("n{index}")),
                Value::Integer(index),
            ]
        })
        .collect();
    let (window, panel) = data_tab(
        cx,
        "database-data-autoscroll",
        table(),
        vec![page(rows), count(60)],
    );
    let view = wait_for_rows(cx, &panel, &table(), 60);
    draw(cx, window);

    // The press in the first cell, and a move to a point below the rows, which the frames scroll.
    let from = cell_centre(cx, window, 0, 0);
    let list = bounds_of(cx, window, "database-data-rows");
    let below = point(from.x, list.bottom() + px(40.));
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_down(from, MouseButton::Left, Modifiers::default());
    visual.simulate_mouse_move(below, MouseButton::Left, Modifiers::default());
    cx.run_until_parked();
    for _ in 0..5 {
        draw(cx, window);
    }
    let reached = view.read_with(cx, |view, _| view.selected_for_test());
    assert!(
        reached.is_some_and(|(row, _)| row >= 5),
        "the rows scrolled under the pointer: {reached:?}"
    );

    // The release ends the drag, so the next frames leave the selection where it is.
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_up(below, MouseButton::Left, Modifiers::default());
    cx.run_until_parked();
    draw(cx, window);
    let released = view.read_with(cx, |view, _| view.selected_for_test());
    draw(cx, window);
    draw(cx, window);
    assert_eq!(
        view.read_with(cx, |view, _| view.selected_for_test()),
        released,
        "no scroll after the release"
    );
}

#[ui_kit::gpui::test]
fn a_click_on_a_header_selects_the_column_and_copy_gives_its_name(cx: &mut TestAppContext) {
    let (window, panel) = data_tab(
        cx,
        "database-data-column",
        table(),
        vec![page(owners()), count(2)],
    );
    let _view = wait_for_rows(cx, &panel, &table(), 2);
    click(cx, window, "database-data-head-1");
    draw(cx, window);
    clear_clipboard(cx);
    press(cx, window, "ctrl-c");
    assert_eq!(clipboard(cx).as_deref(), Some("NAME\nAnn\nBob"));
}

#[ui_kit::gpui::test]
fn a_click_on_a_header_keeps_the_sort_and_the_icon_sorts(cx: &mut TestAppContext) {
    let replies = vec![
        page(owners()),
        count(2),
        page(owners()),
        count(2),
        page(owners()),
        count(2),
        page(owners()),
        count(2),
    ];
    let (window, panel) = data_tab(cx, "database-data-sort", table(), replies);
    let view = wait_for_rows(cx, &panel, &table(), 2);
    let reads = panel.read_with(cx, |panel, _| panel.queries_for_test().len());

    click(cx, window, "database-data-head-1");
    draw(cx, window);
    assert_eq!(
        view.read_with(cx, |view, _| view.sort_for_test()),
        None,
        "a click on the name sorts nothing"
    );
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.queries_for_test().len()),
        reads,
        "and reads nothing"
    );

    // The icon shows while the pointer is over the header: ascending, then descending, then none.
    let head = bounds_of(cx, window, "database-data-head-1").center();
    hover(cx, window, head);
    draw(cx, window);
    click(cx, window, "database-data-sort-1");
    wait_for_signal(cx, "the ascending sort", |cx| {
        view.read_with(cx, |view, _| {
            view.sort_for_test() == Some(("NAME".to_owned(), true))
        })
    });
    let queries = panel.read_with(cx, |panel, _| panel.queries_for_test());
    assert!(
        queries
            .iter()
            .any(|(sql, _)| sql.contains("ORDER BY \"NAME\" ASC")),
        "{queries:?}"
    );

    draw(cx, window);
    click(cx, window, "database-data-sort-1");
    wait_for_signal(cx, "the descending sort", |cx| {
        view.read_with(cx, |view, _| {
            view.sort_for_test() == Some(("NAME".to_owned(), false))
        })
    });
    draw(cx, window);
    click(cx, window, "database-data-sort-1");
    wait_for_signal(cx, "no sort", |cx| {
        view.read_with(cx, |view, _| view.sort_for_test().is_none())
    });
}

#[ui_kit::gpui::test]
fn the_row_numbers_count_the_rows_and_a_click_selects_the_whole_row(cx: &mut TestAppContext) {
    let (window, panel) = data_tab(
        cx,
        "database-data-numbers",
        table(),
        vec![page(owners()), count(2)],
    );
    let view = wait_for_rows(cx, &panel, &table(), 2);
    assert_eq!(
        view.read_with(cx, |view, _| (
            view.number_for_test(0),
            view.number_for_test(1)
        )),
        (1, 2)
    );

    draw(cx, window);
    click(cx, window, "database-data-number-1");
    draw(cx, window);
    clear_clipboard(cx);
    press(cx, window, "ctrl-c");
    assert_eq!(
        clipboard(cx).as_deref(),
        Some("2\tBob\t40"),
        "the whole row is selected"
    );
}

#[ui_kit::gpui::test]
fn the_rows_of_a_second_page_go_on_from_the_page_size(cx: &mut TestAppContext) {
    let (window, panel) = data_tab(
        cx,
        "database-data-numbers-page",
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
        ],
    );
    let view = wait_for_rows(cx, &panel, &table(), 2);

    click(cx, window, "database-data-next");
    wait_for_signal(cx, "the second page", |cx| {
        view.read_with(cx, |view, _| {
            view.page_for_test() == 1 && view.model_for_test().row_count() == 1
        })
    });
    assert_eq!(
        view.read_with(cx, |view, _| view.number_for_test(0)),
        501,
        "the first row of the second page is the 501st"
    );
}

#[ui_kit::gpui::test]
fn ctrl_a_and_the_corner_select_every_loaded_row(cx: &mut TestAppContext) {
    let (window, panel) = data_tab(
        cx,
        "database-data-select-all",
        table(),
        vec![page(owners()), count(2)],
    );
    let _view = wait_for_rows(cx, &panel, &table(), 2);
    click(cx, window, "database-data-cell-0-0");
    draw(cx, window);
    press(cx, window, "ctrl-a");
    draw(cx, window);
    clear_clipboard(cx);
    press(cx, window, "ctrl-c");
    assert_eq!(
        clipboard(cx).as_deref(),
        Some("1\tAnn\t30\n2\tBob\t40"),
        "every loaded row, and no names: the rows are selected, not the columns"
    );

    press(cx, window, "down");
    draw(cx, window);
    clear_clipboard(cx);
    press(cx, window, "ctrl-c");
    assert_eq!(
        clipboard(cx).as_deref(),
        Some("2"),
        "an arrow without Shift leaves one cell"
    );

    click(cx, window, "database-data-cell-0-1");
    draw(cx, window);
    click(cx, window, "database-data-corner");
    draw(cx, window);
    clear_clipboard(cx);
    press(cx, window, "ctrl-c");
    assert_eq!(clipboard(cx).as_deref(), Some("1\tAnn\t30\n2\tBob\t40"));
}

#[ui_kit::gpui::test]
fn a_right_click_inside_the_selection_keeps_it_for_the_menu(cx: &mut TestAppContext) {
    let (window, panel) = data_tab(
        cx,
        "database-data-menu-select",
        table(),
        vec![page(three_owners()), count(3)],
    );
    let _view = wait_for_rows(cx, &panel, &table(), 3);
    draw(cx, window);
    let from = cell_centre(cx, window, 0, 0);
    let to = cell_centre(cx, window, 1, 1);
    drag(cx, window, from, to, Modifiers::default());
    draw(cx, window);

    // A right click inside the rectangle keeps it, so Copy of the menu copies all of it.
    let inside = cell_centre(cx, window, 1, 0);
    right_click(cx, window, inside);
    draw(cx, window);
    clear_clipboard(cx);
    click(cx, window, "menu-0");
    assert_eq!(clipboard(cx).as_deref(), Some("1\tAnn\n2\tBob"));

    // A right click outside the rectangle selects the cell under it, and Copy takes that cell.
    let outside = cell_centre(cx, window, 1, 2);
    right_click(cx, window, outside);
    draw(cx, window);
    clear_clipboard(cx);
    click(cx, window, "menu-0");
    assert_eq!(clipboard(cx).as_deref(), Some("40"));
}

#[ui_kit::gpui::test]
fn the_menu_of_a_header_sorts_the_rows(cx: &mut TestAppContext) {
    let (window, panel) = data_tab(
        cx,
        "database-data-header-menu",
        table(),
        vec![page(owners()), count(2), page(owners()), count(2)],
    );
    let view = wait_for_rows(cx, &panel, &table(), 2);
    draw(cx, window);
    let head = bounds_of(cx, window, "database-data-head-1").center();
    right_click(cx, window, head);
    draw(cx, window);
    click(cx, window, "menu-0");
    wait_for_signal(cx, "the ascending sort from the menu", |cx| {
        view.read_with(cx, |view, _| {
            view.sort_for_test() == Some(("NAME".to_owned(), true))
        })
    });
}

#[ui_kit::gpui::test]
fn the_numbers_stay_in_place_when_the_grid_scrolls_sideways(cx: &mut TestAppContext) {
    let wide = wide_table();
    let (window, panel) = data_tab(
        cx,
        "database-data-scroll",
        wide.clone(),
        vec![wide_page(), count(1)],
    );
    let view = wait_for_rows(cx, &panel, &wide, 1);
    draw(cx, window);
    let number = left_of(cx, window, "database-data-number-0".to_owned());
    let head = left_of(cx, window, "database-data-head-2".to_owned());
    let cell = left_of(cx, window, "database-data-cell-0-2".to_owned());
    assert!(
        (head - cell).abs() < 0.5,
        "the header lines up with the rows"
    );

    view.read_with(cx, |view, _| view.scroll_sideways_for_test(-120.));
    draw(cx, window);
    assert!(
        view.read_with(cx, |view, _| view.scroll_x_for_test()) < 0.,
        "the rows scrolled to the right"
    );
    let number_after = left_of(cx, window, "database-data-number-0".to_owned());
    let head_after = left_of(cx, window, "database-data-head-2".to_owned());
    let cell_after = left_of(cx, window, "database-data-cell-0-2".to_owned());
    assert!(
        (number_after - number).abs() < 0.5,
        "the numbers stay where they were"
    );
    assert!(cell_after < cell, "the cells moved to the left");
    assert!(
        (head_after - cell_after).abs() < 0.5,
        "the header follows the rows"
    );
}

/// Twelve text columns, wider than the grid, so that the grid scrolls sideways.
fn wide_columns() -> Vec<Column> {
    (0..12)
        .map(|index| Column {
            name: format!("C{index}"),
            type_name: "VARCHAR".into(),
            kind: ColumnKind::Text,
            nullable: true,
            primary_key: index == 0,
        })
        .collect()
}

/// The table of the wide columns.
fn wide_table() -> Table {
    Table {
        name: "WIDE".into(),
        columns: wide_columns(),
        ..table()
    }
}

/// One row of the wide table, as the driver answers it.
fn wide_page() -> ScriptedReply {
    let row: Vec<Value> = (0..12)
        .map(|index| Value::Text(format!("v{index}")))
        .collect();
    ScriptedReply {
        result: ResultSet {
            columns: wide_columns(),
            rows: vec![row],
            update_count: None,
            truncated: false,
        },
        error: None,
    }
}

#[ui_kit::gpui::test]
fn a_drag_past_the_right_edge_scrolls_the_columns(cx: &mut TestAppContext) {
    let wide = wide_table();
    let (window, panel) = data_tab(
        cx,
        "database-data-scroll-edge",
        wide.clone(),
        vec![wide_page(), count(1)],
    );
    let view = wait_for_rows(cx, &panel, &wide, 1);
    draw(cx, window);

    // The press in the first cell, and a move to the right of the grid, which the frames scroll.
    let from = cell_centre(cx, window, 0, 0);
    let list = bounds_of(cx, window, "database-data-rows");
    let beyond = point(list.right() + px(40.), from.y);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_down(from, MouseButton::Left, Modifiers::default());
    visual.simulate_mouse_move(beyond, MouseButton::Left, Modifiers::default());
    cx.run_until_parked();
    for _ in 0..4 {
        draw(cx, window);
    }
    assert!(
        view.read_with(cx, |view, _| view.scroll_x_for_test()) < 0.,
        "the columns scrolled to the right"
    );
    let reached = view.read_with(cx, |view, _| view.selected_for_test());
    assert!(
        reached.is_some_and(|(_, column)| column >= 3),
        "the drag reached the columns on the right: {reached:?}"
    );

    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_up(beyond, MouseButton::Left, Modifiers::default());
    cx.run_until_parked();
}

#[ui_kit::gpui::test]
fn shift_arrows_extend_the_rectangle_from_the_anchor(cx: &mut TestAppContext) {
    let (window, panel) = data_tab(
        cx,
        "database-data-shift-arrows",
        table(),
        vec![page(owners()), count(2)],
    );
    let _view = wait_for_rows(cx, &panel, &table(), 2);
    click(cx, window, "database-data-cell-0-0");
    draw(cx, window);
    press(cx, window, "shift-down");
    draw(cx, window);
    clear_clipboard(cx);
    press(cx, window, "ctrl-c");
    assert_eq!(
        clipboard(cx).as_deref(),
        Some("1\n2"),
        "the rectangle reaches the row below"
    );
}

#[ui_kit::gpui::test]
fn a_drag_across_row_numbers_selects_rows_and_across_headers_selects_columns(
    cx: &mut TestAppContext,
) {
    let (window, panel) = data_tab(
        cx,
        "database-data-drag-axes",
        table(),
        vec![page(owners()), count(2)],
    );
    let _view = wait_for_rows(cx, &panel, &table(), 2);
    draw(cx, window);

    let from = bounds_of(cx, window, "database-data-number-0").center();
    let to = bounds_of(cx, window, "database-data-number-1").center();
    drag(cx, window, from, to, Modifiers::default());
    draw(cx, window);
    clear_clipboard(cx);
    press(cx, window, "ctrl-c");
    assert_eq!(
        clipboard(cx).as_deref(),
        Some("1\tAnn\t30\n2\tBob\t40"),
        "two whole rows"
    );

    let from = bounds_of(cx, window, "database-data-head-0").center();
    let to = bounds_of(cx, window, "database-data-head-1").center();
    drag(cx, window, from, to, Modifiers::default());
    draw(cx, window);
    clear_clipboard(cx);
    press(cx, window, "ctrl-c");
    assert_eq!(
        clipboard(cx).as_deref(),
        Some("ID\tNAME\n1\tAnn\n2\tBob"),
        "two whole columns carry their names"
    );
}
