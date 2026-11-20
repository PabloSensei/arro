//! T-0359: the Data Sources window of the Database tool, a modal dialog over the workspace.
//! `+` and F4 open it, OK saves and closes, Cancel and the cross close it without saving, the
//! Host, Port and Database fields build the address of a product and an address fills them
//! back, and Test Connection shows the answer of its helper or the reason it stopped.
//!
//! No test reaches a database: the helper is a stand-in, which the probe answers from a script
//! or which stops before it starts. The test on a real PostgreSQL is ignored; it needs a server
//! and its driver in the repository.
//!
//! `cargo test -p panels window_tests_database_sources`

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use database::{
    Driver, DriverKind, Event, Launch, Source, SourceId, Suggestion, Support, VendorTemplate,
};
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use workspace::{DATABASE_TOOL, Workspace};

use crate::database_panel::{ActiveDatabase, DatabasePanel};
use crate::database_sources::{DataSourcesDialog, Field};
use crate::window_tests::{
    click, draw, open_window, press, sample_project, type_text, wait_for_settled_bounds,
    wait_for_signal,
};

const SOURCE: &str = "owners";
const SERVER: &str = "the stand-in helper stops here";

/// The language side of the window in the tests: the address templates of two products, a
/// PostgreSQL driver whose jar the project has, and a helper that stops before it starts.
struct StandIn {
    /// Keeps the folder of the jar until the support goes.
    _folder: test_support::TempDir,
    jar: PathBuf,
}

impl StandIn {
    fn support(name: &str) -> Arc<dyn Support> {
        let folder = test_support::TempDir::new(name);
        let jar = folder.path().join("postgresql-42.7.4.jar");
        fs::write(&jar, b"the stand-in jar").expect("the stand-in jar is written");
        Arc::new(Self {
            _folder: folder,
            jar,
        })
    }
}

impl Support for StandIn {
    fn vendors(&self) -> Vec<VendorTemplate> {
        vec![
            VendorTemplate {
                kind: DriverKind::H2,
                title: "H2".into(),
                url_template: "jdbc:h2:{file}".into(),
                host: String::new(),
                port: None,
            },
            VendorTemplate {
                kind: DriverKind::PostgreSql,
                title: "PostgreSQL".into(),
                url_template: "jdbc:postgresql://{host}:{port}/{database}".into(),
                host: "localhost".into(),
                port: Some(5432),
            },
        ]
    }

    fn drivers(&self) -> Vec<Driver> {
        vec![Driver {
            kind: DriverKind::PostgreSql,
            label: "PostgreSQL driver".into(),
            jars: vec![self.jar.clone()],
            module: Some("app".into()),
        }]
    }

    fn suggestions(&self) -> Vec<Suggestion> {
        Vec::new()
    }

    fn helper(&self, _jars: &[PathBuf]) -> Result<Launch, String> {
        Err(SERVER.into())
    }
}

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

/// Opens the window, shows the Database tool, and gives it the language side and the sources.
fn open(
    cx: &mut TestAppContext,
    name: &str,
    support: Arc<dyn Support>,
    sources: Vec<Source>,
) -> (WindowHandle<Workspace>, gpui::Entity<DatabasePanel>) {
    let root = sample_project(name);
    let window = open_window(cx, root);
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(DATABASE_TOOL, window, cx)
        })
        .unwrap();
    draw(cx, window);
    let panel = cx.update(|cx| cx.global::<ActiveDatabase>().0.clone());
    // The window reads the saved sources in the background when it is built; that read
    // must land before the test puts its own sources in place.
    cx.run_until_parked();
    panel.update(cx, |panel, cx| {
        panel.set_support_for_test(support);
        panel.sources_for_test(sources, cx);
    });
    let focus = panel.read_with(cx, |panel, _| panel.focus());
    crate::window_tests::with_window(cx, window, |window, cx| window.focus(&focus, cx));
    for _ in 0..6 {
        press(cx, window, "ctrl-shift-up");
    }
    draw(cx, window);
    wait_for_settled_bounds(cx, window, "database-panel");
    draw(cx, window);
    (window, panel)
}

/// Whether the element with a debug selector is on screen.
fn on_screen(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &'static str,
) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

/// The Data Sources window, when it is open.
pub(crate) fn data_sources_of(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
) -> Option<gpui::Entity<DataSourcesDialog>> {
    window
        .read_with(cx, |workspace, _| workspace.overlay_view())
        .ok()
        .flatten()
        .and_then(|view| view.downcast::<DataSourcesDialog>().ok())
}

/// The text of a field of the selected connection in the window.
fn field_text(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    field: Field,
) -> Option<String> {
    let dialog = data_sources_of(cx, window)?;
    dialog.read_with(cx, |dialog, _| dialog.text_for_test(field))
}

/// The answer of the last Test Connection in the window.
fn test_answer(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
) -> Option<Result<String, String>> {
    let dialog = data_sources_of(cx, window)?;
    dialog.read_with(cx, |dialog, _| dialog.test_for_test())
}

/// Chooses a product in the kind list of the form: `row` is the product's row of the list.
fn choose_kind(cx: &mut TestAppContext, window: WindowHandle<Workspace>, row: &'static str) {
    click(cx, window, "data-sources-kind");
    draw(cx, window);
    click(cx, window, row);
    draw(cx, window);
}

/// Types into a field of the form: the field takes the focus on click, and the old text goes.
fn retype(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &'static str,
    text: &str,
) {
    click(cx, window, selector);
    press(cx, window, "ctrl-a");
    type_text(cx, window, text);
    draw(cx, window);
}

#[ui_kit::gpui::test]
fn plus_opens_the_window_and_ok_adds_the_connection_to_the_tree(cx: &mut TestAppContext) {
    let (window, panel) = open(
        cx,
        "database-sources-add",
        StandIn::support("database-sources-add"),
        Vec::new(),
    );

    click(cx, window, "database-add");
    draw(cx, window);
    assert!(
        on_screen(cx, window, "data-sources-title"),
        "+ opens the window"
    );
    assert!(
        on_screen(cx, window, "data-sources-kind"),
        "the window chooses a product"
    );
    assert!(
        on_screen(cx, window, "database-field-0"),
        "the window has the name field"
    );
    assert!(
        on_screen(cx, window, "database-field-1"),
        "the window has the address field"
    );

    retype(cx, window, "database-field-0", "Owners");
    click(cx, window, "database-dialog-add");
    draw(cx, window);

    assert!(
        data_sources_of(cx, window).is_none(),
        "OK closes the window"
    );
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.sources().len()),
        1,
        "OK adds the connection"
    );
    assert!(
        on_screen(cx, window, "database-source-owners"),
        "the connection is in the tree"
    );
}

#[ui_kit::gpui::test]
fn cancel_and_the_cross_add_nothing(cx: &mut TestAppContext) {
    let (window, panel) = open(
        cx,
        "database-sources-cancel",
        StandIn::support("database-sources-cancel"),
        Vec::new(),
    );

    click(cx, window, "database-add");
    draw(cx, window);
    retype(cx, window, "database-field-0", "Owners");
    click(cx, window, "database-dialog-cancel");
    draw(cx, window);
    assert!(
        data_sources_of(cx, window).is_none(),
        "Cancel closes the window"
    );

    click(cx, window, "database-add");
    draw(cx, window);
    retype(cx, window, "database-field-0", "Owners");
    click(cx, window, "data-sources-close");
    draw(cx, window);
    assert!(
        data_sources_of(cx, window).is_none(),
        "the cross closes the window"
    );

    assert!(
        panel.read_with(cx, |panel, _| panel.sources().is_empty()),
        "neither Cancel nor the cross keeps a connection"
    );
}

#[ui_kit::gpui::test]
fn host_port_and_database_build_the_address_and_an_address_fills_them_back(
    cx: &mut TestAppContext,
) {
    let (window, _panel) = open(
        cx,
        "database-sources-address",
        StandIn::support("database-sources-address"),
        Vec::new(),
    );

    click(cx, window, "database-add");
    draw(cx, window);
    choose_kind(cx, window, "data-sources-kind-1");
    assert_eq!(
        field_text(cx, window, Field::Url).as_deref(),
        Some("jdbc:postgresql://localhost:5432/"),
        "the product fills the address with its host and port"
    );

    retype(cx, window, "database-field-8", "pet");
    assert_eq!(
        field_text(cx, window, Field::Url).as_deref(),
        Some("jdbc:postgresql://localhost:5432/pet"),
        "the database writes the address"
    );

    retype(
        cx,
        window,
        "database-field-1",
        "jdbc:postgresql://db.local:6543/owners",
    );
    assert_eq!(
        field_text(cx, window, Field::Host).as_deref(),
        Some("db.local")
    );
    assert_eq!(field_text(cx, window, Field::Port).as_deref(), Some("6543"));
    assert_eq!(
        field_text(cx, window, Field::Database).as_deref(),
        Some("owners")
    );
}

#[ui_kit::gpui::test]
fn f4_opens_the_window_on_the_connection_and_ok_renames_it(cx: &mut TestAppContext) {
    let (window, panel) = open(
        cx,
        "database-sources-f4",
        StandIn::support("database-sources-f4"),
        vec![source()],
    );

    click(cx, window, "database-source-owners");
    crate::window_tests::with_window(cx, window, |window, cx| {
        let focus = panel.read(cx).focus();
        window.focus(&focus, cx);
    });
    press(cx, window, "f4");
    draw(cx, window);

    assert_eq!(
        field_text(cx, window, Field::Name).as_deref(),
        Some("PetClinic"),
        "F4 shows the connection the tree has selected"
    );
    retype(cx, window, "database-field-0", "Renamed");
    click(cx, window, "database-dialog-add");
    draw(cx, window);

    assert!(
        data_sources_of(cx, window).is_none(),
        "OK closes the window"
    );
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.sources()[0].name.clone()),
        "Renamed",
        "OK renames the connection in the list"
    );
    assert!(
        on_screen(cx, window, "database-source-owners"),
        "the connection keeps its id"
    );
}

#[ui_kit::gpui::test]
fn remove_takes_the_connection_away_when_ok_is_pressed(cx: &mut TestAppContext) {
    let (window, panel) = open(
        cx,
        "database-sources-remove",
        StandIn::support("database-sources-remove"),
        vec![source()],
    );

    click(cx, window, "database-source-owners");
    crate::window_tests::with_window(cx, window, |window, cx| {
        let focus = panel.read(cx).focus();
        window.focus(&focus, cx);
    });
    press(cx, window, "f4");
    draw(cx, window);
    click(cx, window, "data-sources-remove");
    draw(cx, window);
    click(cx, window, "database-dialog-add");
    draw(cx, window);

    assert!(
        panel.read_with(cx, |panel, _| panel.sources().is_empty()),
        "OK removes the connection the window removed"
    );
}

#[ui_kit::gpui::test]
fn ok_without_an_address_keeps_the_window_open_with_the_reason(cx: &mut TestAppContext) {
    let (window, panel) = open(
        cx,
        "database-sources-address-missing",
        StandIn::support("database-sources-address-missing"),
        Vec::new(),
    );

    click(cx, window, "database-add");
    draw(cx, window);
    click(cx, window, "database-field-1");
    press(cx, window, "ctrl-a");
    press(cx, window, "backspace");
    draw(cx, window);
    click(cx, window, "database-dialog-add");
    draw(cx, window);

    assert!(
        data_sources_of(cx, window).is_some(),
        "the window stays open"
    );
    let reason = data_sources_of(cx, window)
        .and_then(|dialog| dialog.read_with(cx, |dialog, _| dialog.error_for_test()));
    assert_eq!(
        reason.as_deref(),
        Some("The address of a source is required.")
    );
    assert!(
        panel.read_with(cx, |panel, _| panel.sources().is_empty()),
        "nothing is saved without an address"
    );
}

#[ui_kit::gpui::test]
fn test_connection_shows_the_product_of_the_server(cx: &mut TestAppContext) {
    let (window, panel) = open(
        cx,
        "database-sources-test-ok",
        StandIn::support("database-sources-test-ok"),
        Vec::new(),
    );
    panel.update(cx, |panel, _| {
        panel.set_probe_script_for_test(vec![Event::Connected {
            id: 1,
            product: "PostgreSQL 16.2".into(),
            auto_commit: true,
            quote: "\"".into(),
        }]);
    });

    click(cx, window, "database-add");
    draw(cx, window);
    choose_kind(cx, window, "data-sources-kind-1");
    click(cx, window, "data-sources-test");
    wait_for_signal(cx, "the answer of the helper", |cx| {
        test_answer(cx, window).is_some()
    });

    assert_eq!(
        test_answer(cx, window),
        Some(Ok("PostgreSQL 16.2".to_owned())),
        "the answer of the server is the product it reports"
    );
    assert!(
        on_screen(cx, window, "data-sources-test-result"),
        "the answer is on screen"
    );
    assert!(
        panel.read_with(cx, |panel, _| panel.sources().is_empty()),
        "a test saves nothing"
    );
}

#[ui_kit::gpui::test]
fn test_connection_shows_the_reason_the_helper_stopped(cx: &mut TestAppContext) {
    let (window, _panel) = open(
        cx,
        "database-sources-test-error",
        StandIn::support("database-sources-test-error"),
        Vec::new(),
    );

    click(cx, window, "database-add");
    draw(cx, window);
    choose_kind(cx, window, "data-sources-kind-1");
    click(cx, window, "data-sources-test");
    wait_for_signal(cx, "the reason of the helper", |cx| {
        test_answer(cx, window).is_some()
    });

    assert_eq!(
        test_answer(cx, window),
        Some(Err(SERVER.to_owned())),
        "the stand-in helper that stops gives its reason"
    );
    assert!(
        on_screen(cx, window, "data-sources-test-result"),
        "the reason is on screen"
    );
}

#[ui_kit::gpui::test]
fn browse_asks_for_the_file_and_the_file_picked_goes_into_the_address(cx: &mut TestAppContext) {
    let (window, _panel) = open(
        cx,
        "database-sources-browse",
        StandIn::support("database-sources-browse"),
        Vec::new(),
    );
    click(cx, window, "database-add");
    draw(cx, window);
    assert!(
        on_screen(cx, window, "data-sources-browse"),
        "a connection to a file has the Browse button"
    );

    click(cx, window, "data-sources-browse");
    assert!(cx.did_prompt_for_paths(), "Browse asks for a file");

    let dialog = data_sources_of(cx, window).expect("the window is open");
    dialog.update(cx, |dialog, cx| {
        dialog.file_picked(PathBuf::from("owners.h2.db"), cx)
    });
    let (file, url) = dialog.read_with(cx, |dialog, _| {
        (
            dialog.text_for_test(Field::File),
            dialog.text_for_test(Field::Url),
        )
    });
    assert_eq!(
        file.as_deref(),
        Some("owners.h2.db"),
        "the file the user picked is in the field"
    );
    assert!(
        url.is_some_and(|url| url.contains("owners.h2.db")),
        "the address is built from the picked file"
    );
}

#[ui_kit::gpui::test]
fn apply_saves_the_connection_and_ok_after_it_adds_no_second_one(cx: &mut TestAppContext) {
    let (window, panel) = open(
        cx,
        "database-sources-apply",
        StandIn::support("database-sources-apply"),
        Vec::new(),
    );
    click(cx, window, "database-add");
    draw(cx, window);
    retype(cx, window, "database-field-0", "Owners");

    click(cx, window, "database-dialog-apply");
    draw(cx, window);
    assert!(
        data_sources_of(cx, window).is_some(),
        "Apply keeps the window open"
    );
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.sources().len()),
        1,
        "Apply saves the connection"
    );

    click(cx, window, "database-dialog-add");
    draw(cx, window);
    assert!(
        data_sources_of(cx, window).is_none(),
        "OK closes the window"
    );
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.sources().len()),
        1,
        "OK after Apply saves the same connection, not a second one"
    );
}

/// The PostgreSQL server of the environment, by the variables the database helper test reads.
/// The driver is the newest one that the language finds in the repository of the environment.
#[ignore = "needs a PostgreSQL server: PGHOST, PGPORT, PGUSER, PGPASSWORD and PGDATABASE, and its driver in the repository of the environment"]
#[ui_kit::gpui::test]
fn test_connection_reaches_a_real_postgresql(cx: &mut TestAppContext) {
    let variable =
        |name: &str, default: &str| std::env::var(name).unwrap_or_else(|_| default.to_owned());
    let host = variable("PGHOST", "localhost");
    let port = variable("PGPORT", "5432");
    let user = variable("PGUSER", "arro");
    let database = variable("PGDATABASE", "petshop");
    let password = std::env::var("PGPASSWORD").expect("set PGPASSWORD of the test server");
    let jar = window_language::postgresql_driver_jar()
        .expect("a PostgreSQL driver in the repository of the environment");
    let root = sample_project("database-sources-real");
    let support: Arc<dyn Support> = window_language::database_support(
        &root,
        &crate::window_tests::scratch("database-sources-real"),
    );
    let server = Source {
        id: SourceId::new("pet"),
        name: "Pet".into(),
        kind: DriverKind::PostgreSql,
        url: format!("jdbc:postgresql://{host}:{port}/{database}"),
        user,
        module: None,
        driver_jar: Some(jar),
    };
    let (window, panel) = open(cx, "database-sources-real-window", support, vec![server]);
    click(cx, window, "database-source-pet");
    crate::window_tests::with_window(cx, window, |window, cx| {
        let focus = panel.read(cx).focus();
        window.focus(&focus, cx);
    });
    press(cx, window, "f4");
    draw(cx, window);
    retype(cx, window, "database-field-3", &password);
    click(cx, window, "data-sources-test");
    wait_for_signal(cx, "the answer of the server", |cx| {
        test_answer(cx, window).is_some()
    });

    let answer = test_answer(cx, window);
    assert!(
        matches!(&answer, Some(Ok(product)) if product.contains("PostgreSQL")),
        "the server answers with its product: {answer:?}"
    );
}
