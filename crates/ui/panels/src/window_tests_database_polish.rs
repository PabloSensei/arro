//! T-0212/T-0251: a double click on a table opens its data tab, whose page asks for the
//! rows through the limit and the offset of the source kind, so the text carries no
//! dialect clause.
//!
//! `cargo test -p panels window_tests_database_polish`

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use database::{Column, ColumnKind, DriverKind, ResultSet, Source, SourceId, Table, TableKind};
use ui_kit::Modifiers;
use ui_kit::gpui;
use ui_kit::gpui::{
    MouseButton, MouseDownEvent, MouseUpEvent, TestAppContext, VisualTestContext, WindowHandle,
};
use workspace::{DATABASE_TOOL, Workspace};

use crate::database_panel::{ActiveDatabase, DatabasePanel};
use crate::window_tests::{bounds_of, draw, open_window, press, sample_project};

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

fn panel(cx: &TestAppContext) -> gpui::Entity<DatabasePanel> {
    cx.update(|cx| cx.global::<ActiveDatabase>().0.clone())
}

fn double_click(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) {
    let position = bounds_of(cx, window, selector).center();
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    for clicks in 1..=2 {
        visual.simulate_event(MouseDownEvent {
            button: MouseButton::Left,
            position,
            modifiers: Modifiers::default(),
            click_count: clicks,
            first_mouse: false,
        });
        visual.simulate_event(MouseUpEvent {
            button: MouseButton::Left,
            position,
            modifiers: Modifiers::default(),
            click_count: clicks,
        });
    }
    cx.run_until_parked();
    draw(cx, window);
}

#[ui_kit::gpui::test]
fn a_double_clicked_table_carries_its_page_and_the_quotes_of_the_dialect(cx: &mut TestAppContext) {
    let root = sample_project("database-polish");
    let window = open_window(cx, root);
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(DATABASE_TOOL, window, cx)
        })
        .unwrap();
    draw(cx, window);
    let panel = panel(cx);
    // The tree needs a bottom dock that shows it whole, as a user would drag it.
    let focus = panel.read_with(cx, |panel, _| panel.focus());
    crate::window_tests::with_window(cx, window, |window, cx| window.focus(&focus, cx));
    for _ in 0..6 {
        press(cx, window, "ctrl-shift-up");
    }
    draw(cx, window);
    panel.update(cx, |panel, cx| {
        panel.connect_for_test(
            source(),
            tables(),
            ResultSet::default(),
            None,
            Arc::new(AtomicBool::new(false)),
            cx,
        );
    });
    draw(cx, window);

    // The quote comes from the kind of the source, not from the `connected` answer.
    panel.update(cx, |panel, cx| {
        panel.connected_for_test(SourceId::new(SOURCE), "H2 2.4.240", "`", cx);
    });

    double_click(cx, window, "database-table-OWNERS");

    let queries = panel.read_with(cx, |panel, _| panel.queries_for_test());
    let (sql, max_rows) = queries
        .iter()
        .find(|(sql, _)| sql.starts_with("SELECT * FROM"))
        .expect("the table asks for its page");
    assert_eq!(
        sql, "SELECT * FROM \"PUBLIC\".\"OWNERS\" LIMIT 500 OFFSET 0",
        "{queries:?}"
    );
    assert_eq!(*max_rows, Some(500));
    assert!(
        queries
            .iter()
            .any(|(sql, _)| sql == "SELECT COUNT(*) FROM \"PUBLIC\".\"OWNERS\""),
        "{queries:?}"
    );
}
