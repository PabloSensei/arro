//! Find Action finds Data Sources... by its title and by the name of its window, Data Sources and
//! Drivers. Enter opens the Data Sources window on the first connection, or on a new one when
//! there is none. New Project is found by its title, and Enter opens the wizard in a window of
//! its own.
//!
//! cargo test -p panels window_tests_find_action_data

use database::{DriverKind, Source, SourceId};
use keymap::workspace as keys;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::Workspace;

use crate::database_panel::ActiveDatabase;
use crate::database_sources::Field;
use crate::palette::Palette;
use crate::start::StartScreen;
use crate::window_tests::{draw, open_palette, open_sample_window, press, type_text};
use crate::window_tests_database_sources::{data_sources_of, field_text};

type Row = (String, Option<String>, &'static str);

/// The popup that Find Action opened.
fn palette(cx: &TestAppContext, window: WindowHandle<Workspace>) -> gpui::Entity<Palette> {
    let view = window
        .read_with(cx, |workspace, _| workspace.overlay_view())
        .unwrap()
        .expect("a popup is open");
    view.downcast::<Palette>()
        .expect("the popup is the palette")
}

/// Opens Find Action and types `query` into it.
fn search(cx: &mut TestAppContext, window: WindowHandle<Workspace>, query: &str) {
    open_palette(cx, window, keys::FIND_ACTION);
    type_text(cx, window, query);
    draw(cx, window);
}

/// The rows on screen: the title, the key at the right edge and the icon at the left.
fn rows(cx: &TestAppContext, window: WindowHandle<Workspace>) -> Vec<Row> {
    palette(cx, window).read_with(cx, |palette, _| palette.action_rows_text())
}

/// The title of the first row.
fn first_title(rows: &[Row]) -> Option<&str> {
    rows.first().map(|(title, _, _)| title.as_str())
}

/// The icon of the row with this title.
fn icon_of(rows: &[Row], title: &str) -> &'static str {
    rows.iter()
        .find(|(row, _, _)| row == title)
        .unwrap_or_else(|| panic!("no {title} in Find Action: {rows:?}"))
        .2
}

/// A saved connection of the test, under its id and its name.
fn connection(id: &str, name: &str) -> Source {
    Source {
        id: SourceId::new(id),
        name: name.into(),
        kind: DriverKind::H2,
        url: format!("mem:{id}"),
        user: "sa".into(),
        module: None,
        driver_jar: None,
    }
}

#[ui_kit::gpui::test]
fn data_sources_is_found_by_its_title_and_enter_opens_the_window(cx: &mut TestAppContext) {
    let (_root, window) = open_sample_window(cx, "find-action-data-sources");
    search(cx, window, "Data Sources");
    let shown = rows(cx, window);
    assert_eq!(first_title(&shown), Some("Data Sources…"), "{shown:?}");
    assert_eq!(
        icon_of(&shown, "Data Sources…"),
        "toolwindows/database",
        "the command has the icon of the Database window: {shown:?}"
    );

    press(cx, window, "enter");
    draw(cx, window);
    assert!(
        data_sources_of(cx, window).is_some(),
        "Enter opens the Data Sources window"
    );
}

#[ui_kit::gpui::test]
fn data_sources_is_found_by_the_name_of_its_window(cx: &mut TestAppContext) {
    let (_root, window) = open_sample_window(cx, "find-action-data-alias");
    search(cx, window, "Data Sources and Drivers");
    let shown = rows(cx, window);
    assert_eq!(first_title(&shown), Some("Data Sources…"), "{shown:?}");
}

#[ui_kit::gpui::test]
fn data_sources_from_find_action_opens_on_the_first_connection(cx: &mut TestAppContext) {
    let (_root, window) = open_sample_window(cx, "find-action-data-first");
    let panel = cx.update(|cx| cx.global::<ActiveDatabase>().0.clone());
    cx.run_until_parked();
    panel.update(cx, |panel, cx| {
        panel.sources_for_test(
            vec![
                connection("pet-clinic", "PetClinic"),
                connection("owners", "Owners"),
            ],
            cx,
        );
    });
    search(cx, window, "Data Sources");
    press(cx, window, "enter");
    draw(cx, window);

    assert_eq!(
        field_text(cx, window, Field::Name).as_deref(),
        Some("PetClinic"),
        "the window opens on the first connection"
    );
}

#[ui_kit::gpui::test]
fn data_sources_from_find_action_opens_a_new_connection_without_sources(cx: &mut TestAppContext) {
    let (_root, window) = open_sample_window(cx, "find-action-data-new");
    search(cx, window, "Data Sources");
    press(cx, window, "enter");
    draw(cx, window);

    assert!(
        data_sources_of(cx, window).is_some(),
        "Enter opens the Data Sources window"
    );
    assert_eq!(
        field_text(cx, window, Field::Name).as_deref(),
        Some(""),
        "the window opens on a new connection, which has no name yet"
    );
}

#[ui_kit::gpui::test]
fn new_project_is_found_by_its_title_and_enter_opens_the_wizard(cx: &mut TestAppContext) {
    let (_root, window) = open_sample_window(cx, "find-action-new-project");
    let before = cx.update(|cx| cx.windows().len());
    search(cx, window, "New Project");
    let shown = rows(cx, window);
    assert_eq!(first_title(&shown), Some("New Project..."), "{shown:?}");

    press(cx, window, "enter");
    cx.run_until_parked();
    assert_eq!(
        cx.update(|cx| cx.windows().len()),
        before + 1,
        "the wizard opens in a window of its own"
    );
    let wizard = cx.update(|cx| {
        cx.windows()
            .into_iter()
            .any(|handle| handle.downcast::<StartScreen>().is_some())
    });
    assert!(wizard, "one of the windows is the New Project wizard");
}
