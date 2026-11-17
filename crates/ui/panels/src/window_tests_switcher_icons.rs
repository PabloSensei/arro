//! T-0212: the Switcher lists the Profiler, and the Database, HTTP Response, Endpoints
//! and Profiler rows carry four different icons instead of the Services one.
//!
//! `cargo test -p panels window_tests_switcher_icons`

use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::Workspace;

use crate::palette::Palette;
use crate::window_tests::{draw, open_sample_window, press};

fn palette(cx: &TestAppContext, window: WindowHandle<Workspace>) -> gpui::Entity<Palette> {
    let view = window
        .read_with(cx, |workspace, _| workspace.overlay_view())
        .unwrap()
        .expect("a popup is open");
    view.downcast::<Palette>()
        .expect("the popup is the palette")
}

#[ui_kit::gpui::test]
fn the_switcher_gives_each_tool_window_its_own_icon(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "switcher-icons");
    press(cx, window, "ctrl-tab");
    draw(cx, window);

    let icons = palette(cx, window).read_with(cx, |palette, _| palette.tool_icons());
    let icon_of = |title: &str| {
        icons
            .iter()
            .find(|(row, _)| row == title)
            .map(|(_, icon)| *icon)
            .unwrap_or_else(|| panic!("no {title} row in {icons:?}"))
    };

    assert_eq!(icon_of("Database"), "toolwindows/database");
    assert_eq!(icon_of("HTTP Response"), "toolwindows/http");
    assert_eq!(icon_of("Endpoints"), "toolwindows/endpoints");
    assert_eq!(icon_of("Profiler"), "run/profiler");

    let own = [
        icon_of("Database"),
        icon_of("HTTP Response"),
        icon_of("Endpoints"),
        icon_of("Profiler"),
    ];
    for (index, icon) in own.iter().enumerate() {
        assert!(
            own[index + 1..].iter().all(|other| other != icon),
            "the icons are different: {own:?}"
        );
    }
}
