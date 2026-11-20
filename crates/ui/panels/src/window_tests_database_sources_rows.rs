//! T-0375: the Data Sources window picks the jar of a driver with the same file dialog as
//! the file of the database, and a double click on a connection of the Database tree or
//! Properties in its menu opens the window on that connection.
//!
//! `cargo test -p panels window_tests_database_sources`

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]

use std::path::PathBuf;

use database::{DriverKind, Source, SourceId};
use ui_kit::gpui;
use ui_kit::gpui::{
    Modifiers, MouseButton, MouseDownEvent, MouseUpEvent, TestAppContext, VisualTestContext,
    WindowHandle,
};
use workspace::Workspace;

use crate::database_sources::Field;
use crate::window_tests::{bounds_of, click, draw};
use crate::window_tests_database_sources::{StandIn, data_sources_of, field_text, on_screen, open};

fn source() -> Source {
    Source {
        id: SourceId::new("owners"),
        name: "PetClinic".into(),
        kind: DriverKind::H2,
        url: "mem:petclinic".into(),
        user: "sa".into(),
        module: None,
        driver_jar: None,
    }
}

/// A left click with its count, as the platform reports the two clicks of a double click.
fn left_click(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &'static str,
    clicks: usize,
) {
    let position = bounds_of(cx, window, selector).center();
    let mut visual = VisualTestContext::from_window(window.into(), cx);
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
    cx.run_until_parked();
    draw(cx, window);
}

/// The right button, as the menu of a row opens on it.
fn right_click(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) {
    let position = bounds_of(cx, window, selector).center();
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_event(MouseDownEvent {
        button: MouseButton::Right,
        position,
        modifiers: Modifiers::default(),
        click_count: 1,
        first_mouse: false,
    });
    visual.simulate_event(MouseUpEvent {
        button: MouseButton::Right,
        position,
        modifiers: Modifiers::default(),
        click_count: 1,
    });
    cx.run_until_parked();
    draw(cx, window);
}

#[ui_kit::gpui::test]
fn window_tests_database_sources_driver_jar_browses_for_a_jar(cx: &mut TestAppContext) {
    let (window, _panel) = open(
        cx,
        "database-sources-driver-browse",
        StandIn::support("database-sources-driver-browse"),
        Vec::new(),
    );
    click(cx, window, "database-add");
    draw(cx, window);
    assert!(
        on_screen(cx, window, "data-sources-browse-driver"),
        "the Driver jar field has the button of the file dialog"
    );

    click(cx, window, "data-sources-browse-driver");
    assert!(
        cx.did_prompt_for_paths(),
        "the button of Driver jar asks for a file"
    );
    let jar = PathBuf::from("drivers").join("postgresql-42.7.4.jar");
    cx.simulate_path_prompt_response(|_| Some(vec![jar.clone()]));
    cx.run_until_parked();
    draw(cx, window);

    let expected = jar.display().to_string();
    assert_eq!(
        field_text(cx, window, Field::Driver).as_deref(),
        Some(expected.as_str()),
        "the jar the user picked is in the Driver jar field"
    );
}

#[ui_kit::gpui::test]
fn window_tests_database_sources_double_click_opens_the_connection(cx: &mut TestAppContext) {
    let (window, _panel) = open(
        cx,
        "database-sources-double-click",
        StandIn::support("database-sources-double-click"),
        vec![source()],
    );

    left_click(cx, window, "database-source-owners", 1);
    assert!(
        data_sources_of(cx, window).is_none(),
        "one click selects the connection alone"
    );
    left_click(cx, window, "database-source-owners", 2);
    draw(cx, window);

    assert!(
        data_sources_of(cx, window).is_some(),
        "a double click opens the Data Sources window"
    );
    assert_eq!(
        field_text(cx, window, Field::Name).as_deref(),
        Some("PetClinic"),
        "the window opens on the connection of the row"
    );
}

#[ui_kit::gpui::test]
fn window_tests_database_sources_properties_opens_the_connection(cx: &mut TestAppContext) {
    let (window, _panel) = open(
        cx,
        "database-sources-properties",
        StandIn::support("database-sources-properties"),
        vec![source()],
    );

    right_click(cx, window, "database-source-owners");
    click(cx, window, "menu-1");
    draw(cx, window);

    assert!(
        data_sources_of(cx, window).is_some(),
        "Properties opens the Data Sources window"
    );
    assert_eq!(
        field_text(cx, window, Field::Name).as_deref(),
        Some("PetClinic"),
        "Properties opens the window on the connection of the row"
    );
}
