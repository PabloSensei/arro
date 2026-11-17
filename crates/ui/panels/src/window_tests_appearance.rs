//! View | Appearance without a window: the modes of the window, the parts it shows, the
//! modes of the editor, the zoom of the interface and the menu that names them.

use std::time::Duration;

use ui_kit::MenuEntry;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use workspace::{
    SETTINGS_FILE, Settings, TOGGLE_BREADCRUMBS, TOGGLE_DISTRACTION_FREE, TOGGLE_FULL_SCREEN,
    TOGGLE_GUTTER_ICONS, TOGGLE_INDENT_GUIDES, TOGGLE_LINE_NUMBERS, TOGGLE_NAVIGATION_BAR,
    TOGGLE_PRESENTATION_MODE, TOGGLE_STATUS_BAR, TOGGLE_STICKY_LINES, TOGGLE_TOOL_WINDOW_BARS,
    TOGGLE_TOOLBAR, TOGGLE_ZEN_MODE, WindowMode, Workspace,
};

use crate::window_tests::{
    bounds_of, draw, editor_of, open_path, open_sample_window, press, type_text,
};

fn has(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

fn settings(cx: &TestAppContext) -> Settings {
    cx.read(|cx| cx.global::<Settings>().clone())
}

fn saved(cx: &TestAppContext, window: WindowHandle<Workspace>) -> Settings {
    let file = window
        .read_with(cx, |workspace, _| {
            workspace.settings_dir().join(SETTINGS_FILE)
        })
        .unwrap();
    Settings::load(&file)
}

fn run(cx: &mut TestAppContext, window: WindowHandle<Workspace>, id: &'static str) -> bool {
    let ran = window
        .update(cx, |workspace, window, cx| {
            workspace.run_command(id, window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
    ran
}

fn mode(cx: &TestAppContext, window: WindowHandle<Workspace>) -> WindowMode {
    window
        .read_with(cx, |workspace, _| workspace.window_mode())
        .unwrap()
}

fn editor_width(cx: &mut TestAppContext, window: WindowHandle<Workspace>) -> f32 {
    f32::from(bounds_of(cx, window, "editor-body-0").size.width)
}

/// The rows of View | Appearance, by their labels.
fn appearance_labels(cx: &mut TestAppContext, window: WindowHandle<Workspace>) -> Vec<String> {
    let sections: Vec<MenuEntry> = window
        .update(cx, |workspace, _, cx| workspace.main_menu(cx))
        .unwrap();
    let view = sections
        .iter()
        .find(|section| section.label.as_ref() == "View")
        .expect("the main menu has View");
    let appearance = view
        .submenu
        .iter()
        .find(|section| section.label.as_ref() == "Appearance")
        .expect("View has Appearance");
    appearance
        .submenu
        .iter()
        .map(|entry| entry.label.to_string())
        .collect()
}

#[ui_kit::gpui::test]
fn distraction_free_hides_the_parts_of_the_window_and_brings_them_back(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "appearance-free");
    let main = root.join("src").join("main.txt");
    open_path(cx, window, &main);

    // A tool window opens before the mode, so the exit has something to bring back.
    press(cx, window, "alt-f12");
    draw(cx, window);
    let terminal = window
        .read_with(cx, |workspace, _| {
            workspace.tool_open(workspace::TERMINAL_TOOL)
        })
        .unwrap();
    assert!(terminal, "Alt+F12 opens the terminal");
    assert!(has(cx, window, "t-project"), "the toolbar is on screen");
    assert!(
        has(cx, window, "stripe-s-project"),
        "the tool window bar is on screen"
    );
    assert!(has(cx, window, "st-caret"), "the status bar is on screen");
    assert!(has(cx, window, "tabs-0"), "the editor tabs are on screen");
    let windowed = editor_width(cx, window);

    assert!(run(cx, window, TOGGLE_DISTRACTION_FREE));
    assert_eq!(mode(cx, window), WindowMode::DistractionFree);
    assert!(
        !has(cx, window, "stripe-s-project"),
        "the tool window bars go away"
    );
    assert!(!has(cx, window, "st-caret"), "the status bar goes away");
    assert!(!has(cx, window, "tabs-0"), "the editor tabs go away");
    assert!(
        editor_width(cx, window) > windowed,
        "the editor takes the room of the docks"
    );
    let view = editor_of(cx, window, &main);
    let (numbers, icons, crumbs) = view.read_with(cx, |view, _| {
        let options = view.options();
        (
            options.line_numbers,
            options.gutter_icons,
            options.breadcrumbs,
        )
    });
    assert!(!numbers, "the line numbers go away");
    assert!(!icons, "the gutter icons go away");
    assert!(!crumbs, "the breadcrumbs go away");

    // Escape leaves a popup; it does not leave the mode, as in IDEA.
    press(cx, window, "escape");
    assert_eq!(mode(cx, window), WindowMode::DistractionFree);
    type_text(cx, window, "z");
    assert_eq!(
        view.read_with(cx, |view, _| view.text()).unwrap(),
        "zalpha\nbeta\n",
        "the editor keeps the keys in the mode"
    );

    assert!(run(cx, window, TOGGLE_DISTRACTION_FREE));
    assert_eq!(mode(cx, window), WindowMode::Normal);
    assert!(
        has(cx, window, "stripe-s-project"),
        "the tool window bars come back"
    );
    assert!(has(cx, window, "st-caret"), "the status bar comes back");
    assert!(has(cx, window, "tabs-0"), "the editor tabs come back");
    assert_eq!(
        editor_width(cx, window),
        windowed,
        "the editor gives the room of the docks back"
    );
    let terminal = window
        .read_with(cx, |workspace, _| {
            workspace.tool_open(workspace::TERMINAL_TOOL)
        })
        .unwrap();
    assert!(terminal, "the tool window that was open comes back");
    assert!(
        view.read_with(cx, |view, _| view.options().line_numbers),
        "the line numbers come back"
    );
}

#[ui_kit::gpui::test]
fn presentation_mode_grows_the_editor_font_without_touching_the_settings(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "appearance-present");
    let main = root.join("src").join("main.txt");
    open_path(cx, window, &main);
    let view = editor_of(cx, window, &main);
    let regular = view.read_with(cx, |view, _| view.font_size());
    assert_eq!(regular, 13.);
    assert_eq!(settings(cx).presentation_font_size, 24.);

    assert!(run(cx, window, TOGGLE_PRESENTATION_MODE));
    assert_eq!(mode(cx, window), WindowMode::Presentation);
    assert_eq!(
        view.read_with(cx, |view, _| view.font_size()),
        24.,
        "Presentation Mode shows the text larger"
    );
    assert_eq!(
        settings(cx).font_size,
        regular,
        "the font of the regular mode is not changed"
    );
    let file = saved(cx, window);
    assert_eq!(file.font_size, regular, "the settings file keeps the size");
    assert_eq!(file.presentation_font_size, 24.);

    // The setting of the mode reaches the open editor at once.
    window
        .update(cx, |workspace, _, cx| {
            workspace.update_settings(|settings| settings.presentation_font_size = 30., cx)
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
    assert_eq!(view.read_with(cx, |view, _| view.font_size()), 30.);

    assert!(run(cx, window, TOGGLE_PRESENTATION_MODE));
    assert_eq!(mode(cx, window), WindowMode::Normal);
    assert_eq!(
        view.read_with(cx, |view, _| view.font_size()),
        regular,
        "the regular font comes back"
    );
    assert_eq!(saved(cx, window).font_size, regular);
}

#[ui_kit::gpui::test]
fn the_switches_of_the_window_and_the_editor_apply_at_once_and_are_saved(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "appearance-switches");
    let main = root.join("src").join("main.txt");
    open_path(cx, window, &main);
    let view = editor_of(cx, window, &main);

    assert!(run(cx, window, TOGGLE_STATUS_BAR));
    assert!(!has(cx, window, "st-caret"), "the status bar goes away");
    assert!(!saved(cx, window).status_bar_visible);
    assert!(run(cx, window, TOGGLE_TOOL_WINDOW_BARS));
    assert!(
        !has(cx, window, "stripe-s-project"),
        "the tool window bars go away"
    );
    assert!(!saved(cx, window).tool_window_bars_visible);
    assert!(run(cx, window, TOGGLE_TOOLBAR));
    assert!(
        !has(cx, window, "t-project"),
        "the toolbar controls go away"
    );
    assert!(
        has(cx, window, "menu-bar-0"),
        "the menu bar and the window buttons stay reachable"
    );
    assert!(!saved(cx, window).toolbar_visible);

    assert!(run(cx, window, TOGGLE_LINE_NUMBERS));
    assert!(!view.read_with(cx, |view, _| view.options().line_numbers));
    assert!(!saved(cx, window).show_line_numbers);
    assert!(run(cx, window, TOGGLE_GUTTER_ICONS));
    assert!(!view.read_with(cx, |view, _| view.options().gutter_icons));
    assert!(!saved(cx, window).show_gutter_icons);
    assert!(run(cx, window, TOGGLE_INDENT_GUIDES));
    assert!(!view.read_with(cx, |view, _| view.options().indent_guides));
    assert!(!saved(cx, window).show_indent_guides);
    assert!(run(cx, window, TOGGLE_BREADCRUMBS));
    assert!(!view.read_with(cx, |view, _| view.options().breadcrumbs));
    assert!(!saved(cx, window).show_breadcrumbs);
    assert!(run(cx, window, TOGGLE_STICKY_LINES));
    assert!(!view.read_with(cx, |view, _| view.options().sticky_lines));
    assert!(!saved(cx, window).show_sticky_lines);
    assert!(run(cx, window, TOGGLE_NAVIGATION_BAR));
    assert!(!saved(cx, window).navigation_bar_visible);
    assert!(
        !view.read_with(cx, |view, _| view.options().breadcrumbs),
        "the navigation bar switch hides the breadcrumbs of the editor"
    );

    // Every switch comes back the same way.
    for id in [
        TOGGLE_STATUS_BAR,
        TOGGLE_TOOL_WINDOW_BARS,
        TOGGLE_TOOLBAR,
        TOGGLE_LINE_NUMBERS,
        TOGGLE_GUTTER_ICONS,
        TOGGLE_INDENT_GUIDES,
        TOGGLE_BREADCRUMBS,
        TOGGLE_STICKY_LINES,
        TOGGLE_NAVIGATION_BAR,
    ] {
        assert!(run(cx, window, id), "{id} runs");
    }
    assert!(has(cx, window, "st-caret"));
    assert!(has(cx, window, "stripe-s-project"));
    assert!(has(cx, window, "t-project"));
    assert!(view.read_with(cx, |view, _| view.options().line_numbers));
    assert!(saved(cx, window).status_bar_visible);
}

#[ui_kit::gpui::test]
fn zoom_steps_show_a_notice_and_stop_at_the_limits(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "appearance-zoom");
    press(cx, window, "ctrl-alt-shift-=");
    let scale = window
        .read_with(cx, |workspace, cx| workspace.ui_scale(cx))
        .unwrap();
    assert!((scale - 1.1).abs() < 0.001, "Zoom In raises the scale");
    assert!(
        has(cx, window, "st-zoom"),
        "the status bar shows the zoom for a second"
    );
    let notice = window
        .read_with(cx, |workspace, _| {
            workspace.zoom_notice().map(|text| text.to_string())
        })
        .unwrap();
    assert_eq!(notice.as_deref(), Some("Zoom: 110%"));

    for _ in 0..20 {
        press(cx, window, "ctrl-alt-shift-=");
    }
    let top = window
        .read_with(cx, |workspace, cx| workspace.ui_scale(cx))
        .unwrap();
    assert!((top - 2.).abs() < 0.001, "the scale stops at its top");

    for _ in 0..40 {
        press(cx, window, "ctrl-alt-shift--");
    }
    let bottom = window
        .read_with(cx, |workspace, cx| workspace.ui_scale(cx))
        .unwrap();
    assert!(
        (bottom - 0.8).abs() < 0.001,
        "the scale stops at its bottom"
    );

    press(cx, window, "ctrl-alt-shift-0");
    let reset = window
        .read_with(cx, |workspace, cx| workspace.ui_scale(cx))
        .unwrap();
    assert!((reset - 1.).abs() < 0.001, "Reset Zoom goes back to 100%");
    assert_eq!(saved_scale(cx, window), 1.);

    // The notice goes away on its own.
    cx.executor().advance_clock(Duration::from_secs(2));
    cx.run_until_parked();
    draw(cx, window);
    let notice = window
        .read_with(cx, |workspace, _| {
            workspace.zoom_notice().map(|text| text.to_string())
        })
        .unwrap();
    assert_eq!(notice, None, "the notice leaves the status bar");
}

fn saved_scale(cx: &TestAppContext, window: WindowHandle<Workspace>) -> f32 {
    saved(cx, window).ui_scale
}

#[ui_kit::gpui::test]
fn the_appearance_menu_names_the_modes_and_follows_their_state(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "appearance-menu");
    let labels = appearance_labels(cx, window);
    for label in [
        "Enter Presentation Mode",
        "Enter Distraction Free Mode",
        "Enter Full Screen",
        "Enter Zen Mode",
        "✓ Toolbar",
        "✓ Tool Window Bars",
        "✓ Status Bar",
        "✓ Navigation Bar",
        "✓ Line Numbers",
        "✓ Gutter Icons",
        "✓ Indent Guides",
        "✓ Breadcrumbs",
        "✓ Sticky Lines",
    ] {
        assert!(
            labels.iter().any(|row| row == label),
            "the Appearance menu has {label}: {labels:?}"
        );
    }

    assert!(run(cx, window, TOGGLE_DISTRACTION_FREE));
    let labels = appearance_labels(cx, window);
    assert!(
        labels.iter().any(|row| row == "Exit Distraction Free Mode"),
        "the row names the way out: {labels:?}"
    );
    assert!(run(cx, window, TOGGLE_DISTRACTION_FREE));

    // Full Screen reaches the window itself; the test platform keeps its own flag.
    assert!(run(cx, window, TOGGLE_FULL_SCREEN));
    let full = window
        .update(cx, |_, window, _| window.is_fullscreen())
        .unwrap();
    assert!(full, "Enter Full Screen fills the screen");
    let labels = appearance_labels(cx, window);
    assert!(labels.iter().any(|row| row == "Exit Full Screen"));
    assert!(run(cx, window, TOGGLE_FULL_SCREEN));
    let full = window
        .update(cx, |_, window, _| window.is_fullscreen())
        .unwrap();
    assert!(!full, "Exit Full Screen comes back");

    // Zen Mode is Distraction Free in a full screen window; Escape does not leave it.
    assert!(run(cx, window, TOGGLE_ZEN_MODE));
    assert_eq!(mode(cx, window), WindowMode::Zen);
    let full = window
        .update(cx, |_, window, _| window.is_fullscreen())
        .unwrap();
    assert!(full, "Zen Mode fills the screen");
    assert!(!has(cx, window, "st-caret"), "Zen hides the status bar");
    press(cx, window, "escape");
    assert_eq!(mode(cx, window), WindowMode::Zen);
    assert!(run(cx, window, TOGGLE_ZEN_MODE));
    assert_eq!(mode(cx, window), WindowMode::Normal);
    let full = window
        .update(cx, |_, window, _| window.is_fullscreen())
        .unwrap();
    assert!(!full, "leaving Zen Mode leaves the full screen");

    // A switch shows its check in the menu.
    assert!(run(cx, window, TOGGLE_STATUS_BAR));
    let labels = appearance_labels(cx, window);
    assert!(labels.iter().any(|row| row == "Status Bar"), "{labels:?}");
}

#[ui_kit::gpui::test]
fn a_switch_and_a_command_of_the_mode_are_in_find_action(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "appearance-find");
    for (id, title) in [
        (
            "window.toggle_distraction_free",
            "Enter Distraction Free Mode",
        ),
        ("window.toggle_zen_mode", "Enter Zen Mode"),
        ("window.toggle_presentation_mode", "Enter Presentation Mode"),
        ("window.toggle_full_screen", "Enter Full Screen"),
        ("window.toggle_status_bar", "Status Bar"),
        ("window.toggle_line_numbers", "Line Numbers"),
        ("workspace.zoom_in_ide", "Zoom In"),
        ("workspace.zoom_out_ide", "Zoom Out"),
        ("workspace.reset_ide_scale", "Reset Zoom"),
    ] {
        let found = window
            .read_with(cx, |workspace, _| {
                workspace
                    .commands()
                    .get(id)
                    .map(|command| command.title.clone())
            })
            .unwrap();
        assert_eq!(found.as_deref(), Some(title), "{id} is in Find Action");
    }
}
