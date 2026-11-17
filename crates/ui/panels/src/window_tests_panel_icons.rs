//! The icons of the tool window stripes and of the Usages tabs, and the Hide button of
//! a tool window header: every window of a stripe shows an icon of the chosen set and
//! look, the icons stay grey, and a press on Hide under the pointer closes the window.

use ui_kit::gpui;
use ui_kit::gpui::{
    AssetSource as _, Bounds, Modifiers, Pixels, TestAppContext, VisualTestContext, WindowHandle,
};
use ui_kit::metrics::ICON;
use ui_kit::{ARRO, INTELLIJ, Theme, icon_file};
use workspace::{PROJECT_TOOL, SystemAppearance, TERMINAL_TOOL, USAGES_TOOL, Workspace};

use crate::window_tests::{bounds_of, draw, open_sample_window, press};
use crate::window_tests_lang::project;
use crate::window_tests_ui::wait_for;
use crate::window_tests_usages_tabs::{UsageLanguage, open_language_window, tabs};

/// The icons of the left stripe, top to bottom, and of the right one.
const LEFT: [(&str, &str); 12] = [
    ("stripe-s-project", "toolwindows/project"),
    ("stripe-s-commit", "toolwindows/commit"),
    ("stripe-s-structure", "toolwindows/structure"),
    ("stripe-s-run", "toolwindows/run"),
    ("stripe-s-debug", "toolwindows/debug"),
    ("stripe-s-terminal", "toolwindows/terminal"),
    ("stripe-s-services", "toolwindows/services"),
    ("stripe-s-database", "toolwindows/database"),
    ("stripe-s-todo", "toolwindows/todo"),
    ("stripe-s-log", "general/vcs"),
    ("stripe-s-problems", "toolwindows/problems"),
    ("stripe-s-usages", "codeInsight/search"),
];

const RIGHT: [(&str, &str); 4] = [
    ("stripe-s-notifications", "toolwindows/notifications"),
    ("stripe-s-ai", "own/aiSessions"),
    ("stripe-s-tasks", "toolwindows/tasks"),
    ("stripe-s-bookmarks", "toolwindows/bookmarks"),
];

fn has<T: 'static>(
    cx: &mut TestAppContext,
    window: WindowHandle<T>,
    selector: &'static str,
) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

/// A path of an icon as a debug selector. A test lives as long as the process, so the
/// leaked name of an icon is no leak at all.
fn selector(path: String) -> &'static str {
    Box::leak(path.into_boxed_str())
}

/// Whether the binary has the file of the icon.
fn in_binary(path: &str) -> bool {
    ui_kit::Assets
        .load(path)
        .ok()
        .flatten()
        .is_some_and(|data| !data.is_empty())
}

/// The system changes its look, and the window follows it, as `SystemAppearance` does in
/// a real window.
fn set_system_theme(cx: &mut TestAppContext, window: WindowHandle<Workspace>, dark: bool) {
    cx.update(|cx| cx.set_global(SystemAppearance { dark }));
    window
        .update(cx, |workspace, _, cx| {
            workspace.on_system_appearance_changed(cx)
        })
        .unwrap();
    cx.run_until_parked();
    for _ in 0..100 {
        draw(cx, window);
        if cx.read(|cx| cx.global::<Theme>().dark) == dark {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
        cx.run_until_parked();
    }
    panic!(
        "the window did not become {}",
        if dark { "dark" } else { "light" }
    );
}

fn set_icon_set(cx: &mut TestAppContext, window: WindowHandle<Workspace>, set: &str) {
    window
        .update(cx, |workspace, _, cx| workspace.assign_icon_set(set, cx))
        .unwrap();
}

/// Moves the pointer over the middle of the control and draws the frame that shows the
/// style of the hover.
fn hover(cx: &mut TestAppContext, window: WindowHandle<Workspace>, name: &'static str) {
    let bounds = bounds_of(cx, window, name);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_move(bounds.center(), None, Modifiers::default());
    cx.run_until_parked();
    draw(cx, window);
}

/// The box of a control the frame recorded, whether or not the control was drawn: the
/// tools of a panel header are hidden until the pointer reaches the panel.
fn control_bounds(name: &str) -> Bounds<Pixels> {
    ui_kit::test_bounds()
        .into_iter()
        .find(|(control, _)| control == name)
        .map(|(_, bounds)| bounds.bounds)
        .unwrap_or_else(|| panic!("no control {name} in the frame"))
}

fn click_control(cx: &mut TestAppContext, window: WindowHandle<Workspace>, name: &str) {
    let at = control_bounds(name).center();
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_click(at, Modifiers::default());
    cx.run_until_parked();
}

fn tool_open(cx: &TestAppContext, window: WindowHandle<Workspace>, tool: &str) -> bool {
    window
        .read_with(cx, |workspace, _| workspace.tool_open(tool))
        .unwrap()
}

/// The icon of a control lies inside the control, not somewhere else on the screen.
fn icon_is_in_button(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    button: &'static str,
    path: &'static str,
) {
    let button = bounds_of(cx, window, button);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    let icon = visual
        .debug_bounds(path)
        .unwrap_or_else(|| panic!("the button {button:?} shows no {path}"));
    assert!(
        button.contains(&icon.center()),
        "the icon {path} is outside the button {button:?}: {icon:?}"
    );
}

#[ui_kit::gpui::test]
fn every_icon_of_the_stripes_is_a_file_of_both_sets_and_looks(_cx: &mut TestAppContext) {
    for (id, name) in LEFT.into_iter().chain(RIGHT) {
        for set in [ARRO, INTELLIJ] {
            for dark in [false, true] {
                let path = icon_file(set, name, dark, ICON).to_string();
                assert!(in_binary(&path), "{id} of the {set} set has no file {path}");
            }
        }
    }
}

#[ui_kit::gpui::test]
fn the_icons_of_the_stripes_are_grey(_cx: &mut TestAppContext) {
    // A coloured file is a marker of a state, not a window: the stripe shows every
    // window in the grey of the set.
    for (id, name) in LEFT.into_iter().chain(RIGHT) {
        for dark in [false, true] {
            let path = icon_file(ARRO, name, dark, ICON).to_string();
            let data = ui_kit::Assets.load(&path).unwrap().unwrap();
            let text = String::from_utf8_lossy(&data);
            for line in text.lines() {
                for colour in ["#2f6fbf", "#4682fa", "#c2410c", "#f97316"] {
                    assert!(
                        !line.contains(colour),
                        "{id} has a coloured icon {path}: {line}"
                    );
                }
            }
        }
    }
}

#[ui_kit::gpui::test]
fn every_button_of_the_stripes_shows_its_icon_in_both_sets_and_looks(cx: &mut TestAppContext) {
    cx.update(|cx| cx.set_global(SystemAppearance { dark: true }));
    let (_, window) = open_sample_window(cx, "stripe-icon-sets");

    for dark in [true, false] {
        set_system_theme(cx, window, dark);
        for set in [ARRO, INTELLIJ] {
            set_icon_set(cx, window, set);
            draw(cx, window);
            for (id, name) in LEFT.into_iter().chain(RIGHT) {
                let path = selector(icon_file(set, name, dark, ICON).to_string());
                icon_is_in_button(cx, window, id, path);
            }
        }
    }
}

/// Opens the language window of the Usages test with the editor on `alpha`, without a
/// search: the Usages window with its tabs opens at `alt-f7`.
fn open_usages_window(cx: &mut TestAppContext, name: &str) -> WindowHandle<Workspace> {
    let root = project(
        name,
        &[
            ("src/Main.txt", "alpha beta\n"),
            ("src/Other.txt", "alpha\n"),
        ],
    );
    let main = root.join("src/Main.txt");
    let language = UsageLanguage::new();
    // The search finds nothing: the rows of the list carry the same icon as the tab,
    // and the empty list leaves the tab the one icon under the tab.
    language.set_usages_at(
        lang_api::TextPoint {
            line: 0,
            character: 0,
        },
        Vec::new(),
    );
    let window = open_language_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    crate::window_tests::open_path(cx, window, &main);
    let view = crate::window_tests::editor_of(cx, window, &main);
    view.update(cx, |view, cx| view.restore_caret(0, 0, cx));
    draw(cx, window);
    window
}

#[ui_kit::gpui::test]
fn the_usages_window_has_its_icon_on_the_stripe_and_on_its_tab(cx: &mut TestAppContext) {
    let window = open_usages_window(cx, "usages-icon");

    // The stripe is checked before the search opens a tab with the same icon.
    for dark in [true, false] {
        set_system_theme(cx, window, dark);
        for set in [ARRO, INTELLIJ] {
            set_icon_set(cx, window, set);
            draw(cx, window);
            let path = selector(icon_file(set, "codeInsight/search", dark, ICON).to_string());
            assert!(in_binary(path), "the Usages icon has no file {path}");
            icon_is_in_button(cx, window, "stripe-s-usages", path);
        }
    }

    press(cx, window, "alt-f7");
    wait_for(cx, "the tab of the Usages window", |cx| {
        !tabs(cx).is_empty()
    });
    draw(cx, window);
    assert!(has(cx, window, "usage-tabs"), "the Usages window has tabs");

    for dark in [true, false] {
        set_system_theme(cx, window, dark);
        for set in [ARRO, INTELLIJ] {
            set_icon_set(cx, window, set);
            draw(cx, window);
            let path = selector(icon_file(set, "codeInsight/search", dark, ICON).to_string());
            icon_is_in_button(cx, window, "usage-tab-0", path);
        }
    }
}

#[ui_kit::gpui::test]
fn the_hide_button_shows_its_icon_under_the_pointer_and_hides_the_window(cx: &mut TestAppContext) {
    cx.update(|cx| cx.set_global(SystemAppearance { dark: true }));
    let (_, window) = open_sample_window(cx, "hide-button");
    let hide = selector(icon_file(ARRO, "general/hide", true, ICON).to_string());
    assert!(
        tool_open(cx, window, PROJECT_TOOL),
        "the Project window starts open"
    );
    assert!(
        !has(cx, window, hide),
        "the tools of a panel show only under the pointer"
    );

    hover(cx, window, "left-panel-tools");
    assert!(
        has(cx, window, hide),
        "the Hide icon of Project stays visible under the pointer"
    );
    click_control(cx, window, "left-panel-hide");
    draw(cx, window);
    assert!(
        !tool_open(cx, window, PROJECT_TOOL),
        "a press on Hide closes Project, as Shift+Escape does"
    );

    for tool in [USAGES_TOOL, TERMINAL_TOOL] {
        window
            .update(cx, |workspace, window, cx| {
                workspace.show_tool(tool, window, cx)
            })
            .unwrap();
        cx.run_until_parked();
        draw(cx, window);
        assert!(tool_open(cx, window, tool), "{tool} opens");

        hover(cx, window, "bottom-panel-tools");
        assert!(
            has(cx, window, hide),
            "the Hide icon of {tool} stays visible under the pointer"
        );
        click_control(cx, window, "bottom-panel-hide");
        draw(cx, window);
        assert!(
            !tool_open(cx, window, tool),
            "a press on Hide closes {tool}"
        );
    }
}
