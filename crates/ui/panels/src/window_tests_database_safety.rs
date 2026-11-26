//! T-0472: the safety of the Database window without a window. A row that no statement can
//! tell apart is not written, a count other than one rolls the whole submit back, text keeps
//! its spaces, Submit asks before it takes the transaction of the console and leaves the
//! console in its own mode, a moved address forgets the stored password, an address that
//! runs code is refused, the address of the dialog leaves its password in the store, and a
//! copy as INSERT takes only the cells of the selection.
//!
//! `cargo test -p panels window_tests_database_safety`

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]

use std::collections::HashMap;
use std::fs;
use std::sync::{Arc, Mutex};

use database::{
    Column, DriverKind, PasswordStore, ResultSet, Secret, Source, SourceId, SourceStore, Table,
    TableKind,
};
use ui_kit::gpui;
use ui_kit::gpui::{
    Modifiers, MouseButton, Pixels, Point, TestAppContext, VisualTestContext, WindowHandle,
};
use workspace::Workspace;

use crate::database_data::Question;
use crate::database_panel::{ActivePasswords, DatabasePanel, ScriptedCommand, ScriptedReply};
use crate::window_tests::{
    bounds_of, click, draw, open_path, press, type_text, wait_for_signal, with_window,
};
use crate::window_tests_database_data::{count, data_tab, owners, page, table, wait_for_rows};
use crate::window_tests_database_sources::{
    StandIn, data_sources_of, field_text, open as open_sources, retype,
};

const SOURCE: &str = "owners";

fn source_id() -> SourceId {
    SourceId::new(SOURCE)
}

fn source() -> Source {
    Source {
        id: source_id(),
        name: "PetClinic".into(),
        kind: DriverKind::H2,
        url: "mem:petclinic".into(),
        user: "sa".into(),
        module: None,
        driver_jar: None,
    }
}

/// The answer of a statement that reported `count` changed rows.
fn changed(count: u64) -> ScriptedReply {
    ScriptedReply {
        result: ResultSet {
            update_count: Some(count),
            ..ResultSet::default()
        },
        error: None,
    }
}

fn updated() -> ScriptedReply {
    changed(1)
}

/// A table whose columns name no key: neither a primary key nor a unique index.
fn no_key_table() -> Table {
    Table {
        schema: "PUBLIC".into(),
        name: "OWNERS".into(),
        kind: TableKind::Table,
        columns: table()
            .columns
            .into_iter()
            .map(|column| Column {
                primary_key: false,
                ..column
            })
            .collect(),
        indexes: Vec::new(),
        foreign_keys: Vec::new(),
        partial: false,
    }
}

fn no_key_page() -> ScriptedReply {
    ScriptedReply {
        result: ResultSet {
            columns: no_key_table().columns,
            rows: owners(),
            update_count: None,
            truncated: false,
        },
        error: None,
    }
}

fn update_name(cx: &mut TestAppContext, window: WindowHandle<Workspace>, text: &str) {
    press(cx, window, "down");
    press(cx, window, "right");
    press(cx, window, "enter");
    press(cx, window, "ctrl-a");
    type_text(cx, window, text);
    press(cx, window, "enter");
}

fn row_error(
    cx: &TestAppContext,
    view: &gpui::Entity<crate::database_data::DatabaseDataView>,
    row: usize,
) -> Option<String> {
    view.read_with(cx, |view, _| view.row_error_for_test(row))
}

#[ui_kit::gpui::test]
fn an_update_that_matches_two_rows_rolls_the_submit_back(cx: &mut TestAppContext) {
    let (window, panel) = data_tab(
        cx,
        "database-safety-two-rows",
        table(),
        vec![page(owners()), count(2), changed(2)],
    );
    let view = wait_for_rows(cx, &panel, &table(), 2);

    update_name(cx, window, "Ann Lee");
    press(cx, window, "ctrl-enter");

    wait_for_signal(cx, "the rollback of the two rows", |cx| {
        panel
            .read_with(cx, |panel, _| panel.scripted_commands_for_test())
            .iter()
            .any(|command| matches!(command, ScriptedCommand::Rollback))
    });
    let expected = "2 rows matched; nothing was saved.".to_owned();
    assert_eq!(
        view.read_with(cx, |view, _| view.error_for_test()),
        Some(expected.clone())
    );
    assert_eq!(row_error(cx, &view, 0), Some(expected));
    assert!(
        view.read_with(cx, |view, _| view.model_for_test().is_dirty()),
        "the edit stays for a second try"
    );
    assert!(
        !panel
            .read_with(cx, |panel, _| panel.scripted_commands_for_test())
            .iter()
            .any(|command| matches!(command, ScriptedCommand::Commit)),
        "the rest of the submit is not committed"
    );
}

#[ui_kit::gpui::test]
fn an_update_that_matches_no_row_rolls_the_submit_back(cx: &mut TestAppContext) {
    let (window, panel) = data_tab(
        cx,
        "database-safety-no-row",
        table(),
        vec![page(owners()), count(2), changed(0)],
    );
    let view = wait_for_rows(cx, &panel, &table(), 2);

    update_name(cx, window, "Ann Lee");
    press(cx, window, "ctrl-enter");

    wait_for_signal(cx, "the rollback of the missing row", |cx| {
        panel
            .read_with(cx, |panel, _| panel.scripted_commands_for_test())
            .iter()
            .any(|command| matches!(command, ScriptedCommand::Rollback))
    });
    let expected = "The row changed or was deleted; nothing was saved.".to_owned();
    assert_eq!(
        view.read_with(cx, |view, _| view.error_for_test()),
        Some(expected.clone())
    );
    assert_eq!(row_error(cx, &view, 0), Some(expected));
    assert!(
        view.read_with(cx, |view, _| view.model_for_test().is_dirty()),
        "the edit stays for a second try"
    );
}

#[ui_kit::gpui::test]
fn a_text_keeps_the_spaces_around_it_in_the_statement(cx: &mut TestAppContext) {
    let (window, panel) = data_tab(
        cx,
        "database-safety-spaces",
        table(),
        vec![
            page(owners()),
            count(2),
            updated(),
            page(owners()),
            count(2),
        ],
    );
    let view = wait_for_rows(cx, &panel, &table(), 2);

    update_name(cx, window, "  Ann Lee  ");
    press(cx, window, "ctrl-enter");

    wait_for_signal(cx, "the commit of the row", |cx| {
        view.read_with(cx, |view, _| !view.model_for_test().is_dirty())
    });
    let queries = panel.read_with(cx, |panel, _| panel.queries_for_test());
    assert!(
        queries
            .iter()
            .any(|(sql, _)| sql.contains("SET \"NAME\" = '  Ann Lee  '")),
        "the spaces are part of the value: {queries:?}"
    );
}

#[ui_kit::gpui::test]
fn a_table_without_a_key_takes_no_edit_and_no_deletion_but_takes_a_new_row(
    cx: &mut TestAppContext,
) {
    let table = no_key_table();
    let (window, panel) = data_tab(
        cx,
        "database-safety-no-key",
        table.clone(),
        vec![no_key_page(), count(2)],
    );
    let view = wait_for_rows(cx, &panel, &table, 2);
    click(cx, window, "database-data-cell-0-1");
    draw(cx, window);

    press(cx, window, "enter");
    assert!(
        !view.read_with(cx, |view, _| view.editing_for_test()),
        "the cell of a table without a key takes no edit"
    );
    press(cx, window, "ctrl-delete");
    assert_eq!(
        view.read_with(cx, |view, _| view
            .model_for_test()
            .row(0)
            .map(|row| row.kind())),
        Some(database::RowKind::Existing),
        "the row of a table without a key is not marked for deletion"
    );

    press(cx, window, "alt-insert");
    wait_for_signal(cx, "the new row", |cx| {
        view.read_with(cx, |view, _| view.model_for_test().row_count() == 3)
    });
    press(cx, window, "enter");
    type_text(cx, window, "9");
    press(cx, window, "enter");
    assert_eq!(
        view.read_with(cx, |view, _| view
            .model_for_test()
            .row(2)
            .and_then(|row| row.cell(0).cloned())),
        Some(database::Value::Integer(9)),
        "a new row of a table without a key takes its values"
    );

    draw(cx, window);
    assert_eq!(
        view.read_with(cx, |view, _| view.hint_for_test()),
        Some("The table has no key, so a row cannot be told apart"),
        "the grid says why a row cannot be changed"
    );
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    assert!(
        visual.debug_bounds("database-data-nokey").is_some(),
        "the hint is on screen"
    );
}

#[ui_kit::gpui::test]
fn submit_asks_before_it_takes_the_transaction_of_the_console(cx: &mut TestAppContext) {
    let (window, panel) = data_tab(
        cx,
        "database-safety-console",
        table(),
        vec![
            page(owners()),
            count(2),
            updated(),
            page(owners()),
            count(2),
        ],
    );
    let view = wait_for_rows(cx, &panel, &table(), 2);
    let console = panel
        .read_with(cx, |panel, _| panel.console_path(&source_id()))
        .expect("the source has a console");
    fs::write(&console, "INSERT INTO OWNERS (NAME) VALUES ('Ann');\n").unwrap();
    panel.update(cx, |panel, _| {
        panel.scripted_console_replies_for_test(vec![ResultSet {
            update_count: Some(1),
            ..ResultSet::default()
        }])
    });
    open_path(cx, window, &console);
    click(cx, window, "database-console-manual");
    open_path(cx, window, &console);
    press(cx, window, "ctrl-enter");
    wait_for_signal(cx, "the pending statement of the console", |cx| {
        panel.read_with(cx, |panel, _| panel.pending_count(&source_id())) == 1
    });
    assert!(
        panel.read_with(cx, |panel, _| panel.manual_for_test(&source_id())),
        "the console is in Manual"
    );

    // The data tab comes back to the front; its edits wait for the answer about the console.
    with_window(cx, window, |window, cx| {
        panel.update(cx, |panel, cx| {
            panel.open_data(source_id(), table(), window, cx)
        });
    });
    draw(cx, window);
    update_name(cx, window, "Ann Lee");
    press(cx, window, "ctrl-enter");
    draw(cx, window);
    assert!(
        matches!(
            view.read_with(cx, |view, _| view.question_for_test()),
            Some(Question::Console { .. })
        ),
        "Submit asks about the transaction of the console"
    );
    assert!(
        !panel
            .read_with(cx, |panel, _| panel.scripted_commands_for_test())
            .iter()
            .any(|command| matches!(command, ScriptedCommand::Commit)),
        "nothing is committed before the answer"
    );

    click(cx, window, "database-data-question-commit");
    wait_for_signal(cx, "the commit of the table and the console", |cx| {
        panel
            .read_with(cx, |panel, _| panel.scripted_commands_for_test())
            .iter()
            .any(|command| matches!(command, ScriptedCommand::Commit))
    });
    wait_for_signal(cx, "the page after the commit", |cx| {
        view.read_with(cx, |view, _| {
            !view.model_for_test().is_dirty() && view.total_for_test().is_some()
        })
    });
    assert!(
        panel.read_with(cx, |panel, _| panel.manual_for_test(&source_id())),
        "the console stays in Manual after the commit"
    );
    let commands = panel.read_with(cx, |panel, _| panel.scripted_commands_for_test());
    let last = commands.iter().rev().find_map(|command| match command {
        ScriptedCommand::AutoCommit(on) => Some(*on),
        _ => None,
    });
    assert_eq!(
        last,
        Some(false),
        "auto-commit does not come back on under a console in Manual: {commands:?}"
    );
}

#[ui_kit::gpui::test]
fn a_rollback_returns_auto_commit_to_a_console_that_had_it(cx: &mut TestAppContext) {
    let (window, panel) = data_tab(
        cx,
        "database-safety-rollback-mode",
        table(),
        vec![page(owners()), count(2), changed(0)],
    );
    let _view = wait_for_rows(cx, &panel, &table(), 2);

    update_name(cx, window, "Ann Lee");
    press(cx, window, "ctrl-enter");
    wait_for_signal(cx, "the rollback", |cx| {
        panel
            .read_with(cx, |panel, _| panel.scripted_commands_for_test())
            .iter()
            .any(|command| matches!(command, ScriptedCommand::Rollback))
    });
    let commands = panel.read_with(cx, |panel, _| panel.scripted_commands_for_test());
    let last = commands.iter().rev().find_map(|command| match command {
        ScriptedCommand::AutoCommit(on) => Some(*on),
        _ => None,
    });
    assert_eq!(
        last,
        Some(true),
        "auto-commit comes back on after the rollback: {commands:?}"
    );
    assert!(!panel.read_with(cx, |panel, _| panel.manual_for_test(&source_id())));
}

/// The store of the tests: the entries it holds in memory. No test writes a password of a
/// user.
struct FakePasswords {
    entries: Mutex<HashMap<(String, String), String>>,
}

impl FakePasswords {
    fn with_password(service: &str, account: &str, password: &str) -> Arc<Self> {
        let store = Arc::new(Self {
            entries: Mutex::new(HashMap::new()),
        });
        store
            .entries
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(
                (service.to_owned(), account.to_owned()),
                password.to_owned(),
            );
        store
    }

    fn password(&self, service: &str, account: &str) -> Option<String> {
        self.entries
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&(service.to_owned(), account.to_owned()))
            .cloned()
    }

    fn entries(&self) -> usize {
        self.entries
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .len()
    }
}

impl PasswordStore for FakePasswords {
    fn available(&self) -> Result<(), String> {
        Ok(())
    }

    fn store(&self, service: &str, account: &str, password: &Secret) -> Result<(), String> {
        self.entries
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(
                (service.to_owned(), account.to_owned()),
                password.expose().to_owned(),
            );
        Ok(())
    }

    fn load(&self, service: &str, account: &str) -> Option<Secret> {
        self.password(service, account).map(Secret::new)
    }

    fn delete(&self, service: &str, account: &str) -> Result<(), String> {
        self.entries
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&(service.to_owned(), account.to_owned()));
        Ok(())
    }
}

/// Opens the Database window with a stand-in store of passwords and the sources of the test.
fn open_with_store(
    cx: &mut TestAppContext,
    name: &str,
    store: Arc<FakePasswords>,
    sources: Vec<Source>,
) -> (WindowHandle<Workspace>, gpui::Entity<DatabasePanel>) {
    let dynamic: Arc<dyn PasswordStore> = store;
    cx.update(|cx| cx.set_global(ActivePasswords(dynamic)));
    let (window, panel) = open_sources(cx, name, StandIn::support(name), sources);
    wait_for_signal(cx, "the store of passwords", |cx| {
        panel.read_with(cx, |panel, _| panel.password_storage_ready_for_test())
    });
    (window, panel)
}

fn open_dialog_on_source(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    panel: &gpui::Entity<DatabasePanel>,
) {
    click(cx, window, "database-source-owners");
    with_window(cx, window, |window, cx| {
        let focus = panel.read(cx).focus();
        window.focus(&focus, cx);
    });
    press(cx, window, "f4");
    draw(cx, window);
}

#[ui_kit::gpui::test]
fn a_moved_address_forgets_the_stored_password(cx: &mut TestAppContext) {
    let service = database::service_of(&source_id());
    let store = FakePasswords::with_password(&service, "sa", "hunter2");
    let (window, _panel) =
        open_with_store(cx, "database-safety-moved", store.clone(), vec![source()]);

    open_dialog_on_source(cx, window, &_panel);
    assert_eq!(
        field_text(cx, window, crate::database_sources::Field::Url).as_deref(),
        Some("mem:petclinic")
    );
    retype(cx, window, "database-field-1", "jdbc:h2:mem:neighbours");
    click(cx, window, "database-dialog-add");
    draw(cx, window);

    wait_for_signal(cx, "the forgotten password", |_| store.entries() == 0);
    assert!(
        data_sources_of(cx, window).is_none(),
        "the window saved and closed"
    );
}

#[ui_kit::gpui::test]
fn an_address_that_runs_code_is_refused_by_the_dialog(cx: &mut TestAppContext) {
    let (window, panel) = open_with_store(
        cx,
        "database-safety-running",
        FakePasswords::with_password("Arro/database/none", "sa", "unused"),
        Vec::new(),
    );

    click(cx, window, "database-add");
    draw(cx, window);
    retype(
        cx,
        window,
        "database-field-1",
        "jdbc:h2:mem:x;INIT=RUNSCRIPT FROM 'http://a'",
    );
    click(cx, window, "database-dialog-add");
    draw(cx, window);

    let dialog = data_sources_of(cx, window).expect("the window stays open");
    let error = dialog.read_with(cx, |dialog, _| dialog.error_for_test());
    assert!(
        error.as_deref().is_some_and(|error| error.contains("INIT")),
        "the address is refused with the name of the parameter: {error:?}"
    );
    assert!(
        panel.read_with(cx, |panel, _| panel.sources().is_empty()),
        "nothing is saved"
    );
}

#[ui_kit::gpui::test]
fn a_password_in_the_address_of_the_dialog_never_reaches_the_sources_file(cx: &mut TestAppContext) {
    let store = FakePasswords::with_password("Arro/database/none", "sa", "unused");
    let (window, panel) = open_with_store(
        cx,
        "database-safety-password-url",
        store.clone(),
        Vec::new(),
    );

    click(cx, window, "database-add");
    draw(cx, window);
    retype(cx, window, "database-field-0", "Owners");
    retype(cx, window, "database-field-2", "sa");
    retype(
        cx,
        window,
        "database-field-1",
        "jdbc:h2:mem:owners;password=s3cret",
    );
    let dialog = data_sources_of(cx, window).expect("the window is open");
    let edit = dialog
        .read_with(cx, |dialog, _| dialog.edit_for_test())
        .expect("a connection is selected");
    assert_eq!(
        edit.password
            .as_ref()
            .map(|password| password.expose().to_owned())
            .as_deref(),
        Some("s3cret"),
        "the password of the address is taken out of it"
    );
    assert!(!edit.source.url.contains("password"), "{}", edit.source.url);
    assert_eq!(
        dialog.read_with(cx, |dialog, _| dialog.save_password_for_test()),
        Some(true),
        "the store is ready, so the Save password flag is on"
    );
    assert_eq!(
        field_text(cx, window, crate::database_sources::Field::Url).as_deref(),
        Some("jdbc:h2:mem:owners;password=s3cret"),
        "the address is in its field"
    );
    click(cx, window, "database-dialog-add");
    draw(cx, window);
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.sources().len()),
        1,
        "OK saves the connection"
    );
    assert_eq!(
        panel
            .read_with(cx, |panel, _| panel.cached_password_for_test(&source_id()))
            .as_deref(),
        Some("s3cret"),
        "the window holds the password of the address"
    );
    assert!(
        panel.read_with(cx, |panel, _| panel.password_storage_ready_for_test()),
        "the store is ready when the connection is saved"
    );

    let service = database::service_of(&source_id());
    wait_for_signal(cx, "the stored password", |_| {
        store.password(&service, "sa").is_some()
    });
    assert_eq!(store.password(&service, "sa").as_deref(), Some("s3cret"));

    let state_dir = panel.read_with(cx, |panel, _| panel.state_dir_for_test());
    wait_for_signal(cx, "the sources file", |_| {
        SourceStore::load(&state_dir).sources.len() == 1
    });
    let saved = SourceStore::load(&state_dir);
    assert!(
        !saved.sources[0].url.contains("s3cret") && !saved.sources[0].url.contains("password"),
        "the file keeps the address without its password: {}",
        saved.sources[0].url
    );
    let text = fs::read_to_string(database::state_folder(&state_dir).join("sources.json")).unwrap();
    assert!(
        !text.contains("s3cret"),
        "the password is nowhere in the file"
    );
}

/// The left edge of the element with a debug selector, in pixels.
fn selector(text: String) -> &'static str {
    Box::leak(text.into_boxed_str())
}

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

/// A press and a release of the right button at `at`, which opens the menu of the place.
fn right_click(cx: &mut TestAppContext, window: WindowHandle<Workspace>, at: Point<Pixels>) {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_down(at, MouseButton::Right, Modifiers::default());
    visual.simulate_mouse_up(at, MouseButton::Right, Modifiers::default());
    cx.run_until_parked();
}

fn control() -> Modifiers {
    Modifiers {
        control: true,
        ..Modifiers::default()
    }
}

#[ui_kit::gpui::test]
fn copy_as_insert_takes_only_the_selected_cells(cx: &mut TestAppContext) {
    let (window, panel) = data_tab(
        cx,
        "database-safety-insert-copy",
        table(),
        vec![page(owners()), count(2)],
    );
    let _view = wait_for_rows(cx, &panel, &table(), 2);
    click(cx, window, "database-data-cell-0-0");
    draw(cx, window);
    let at = cell_centre(cx, window, 0, 2);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_click(at, control());
    cx.run_until_parked();
    draw(cx, window);

    let inside = cell_centre(cx, window, 0, 0);
    right_click(cx, window, inside);

    draw(cx, window);
    click(cx, window, "menu-1");

    let text = cx.read(|cx| cx.read_from_clipboard().and_then(|item| item.text()));
    assert_eq!(
        text.as_deref(),
        Some("INSERT INTO \"PUBLIC\".\"OWNERS\" (\"ID\", \"AGE\") VALUES (1, 30)"),
        "only the two selected cells are written, and the cell between them is left out"
    );
}
