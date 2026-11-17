//! The menu bar of the window without a window: the sections in the title bar, their
//! submenus under the pointer, the Alt mnemonics and the overflow row of a narrow window.

use ui_kit::gpui;
use ui_kit::gpui::{
    Modifiers, ModifiersChangedEvent, TestAppContext, VisualTestContext, WindowHandle,
};
use ui_kit::{MenuEntry, px, size};
use workspace::Workspace;

use crate::window_tests::{bounds_of, click, draw, open_sample_window, press};

/// The debug selector of a section of the menu bar.
fn bar_selector(index: usize) -> &'static str {
    Box::leak(format!("menu-bar-{index}").into_boxed_str())
}

fn has_selector(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &'static str,
) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

fn sections(cx: &mut TestAppContext, window: WindowHandle<Workspace>) -> Vec<MenuEntry> {
    window
        .update(cx, |workspace, _, cx| workspace.main_menu(cx))
        .unwrap()
}

fn open_section(cx: &mut TestAppContext, window: WindowHandle<Workspace>, index: usize) {
    click(cx, window, bar_selector(index));
    draw(cx, window);
}

/// An Alt tap: the press and the release, without another key between them.
fn alt_tap(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    let change = |modifiers| ModifiersChangedEvent {
        modifiers,
        ..ModifiersChangedEvent::default()
    };
    visual.simulate_event(change(Modifiers {
        alt: true,
        ..Modifiers::default()
    }));
    visual.simulate_event(change(Modifiers::default()));
    cx.run_until_parked();
}

/// Moves the pointer onto a section of the menu bar.
fn hover_bar(cx: &mut TestAppContext, window: WindowHandle<Workspace>, index: usize) {
    let position = bounds_of(cx, window, bar_selector(index)).center();
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_move(position, None, Modifiers::default());
    cx.run_until_parked();
    draw(cx, window);
}

fn resize(cx: &mut TestAppContext, window: WindowHandle<Workspace>, width: f32) {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.update(|window, cx| {
        window.resize(size(px(width), px(700.)));
        window.bounds_changed(cx);
    });
    cx.run_until_parked();
    draw(cx, window);
}

fn open_labels(cx: &mut TestAppContext, window: WindowHandle<Workspace>) -> Option<Vec<String>> {
    window
        .read_with(cx, |workspace, _| {
            workspace.menu_entries().map(|entries| {
                entries
                    .iter()
                    .map(|entry| entry.label.to_string())
                    .collect()
            })
        })
        .unwrap()
}

#[ui_kit::gpui::test]
fn the_menu_bar_lists_the_sections_in_order(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "menu-bar-order");
    let entries = sections(cx, window);
    let labels: Vec<String> = entries
        .iter()
        .map(|entry| entry.label.to_string())
        .collect();
    assert_eq!(
        labels,
        [
            "File", "Edit", "View", "Navigate", "Code", "Refactor", "Build", "Run", "Git",
            "Window", "Help"
        ]
    );
    for (index, label) in labels.iter().enumerate() {
        assert!(
            has_selector(cx, window, bar_selector(index)),
            "the menu bar has no {label} item"
        );
    }
}

#[ui_kit::gpui::test]
fn a_click_opens_a_submenu_and_a_second_click_closes_it(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "menu-bar-click");
    open_section(cx, window, 0);

    assert_eq!(
        window
            .read_with(cx, |workspace, _| workspace.menu_bar_section())
            .unwrap(),
        Some(0)
    );
    assert_eq!(
        open_labels(cx, window).and_then(|labels| labels.first().cloned()),
        Some("Open Folder...".to_string())
    );

    open_section(cx, window, 0);
    assert!(
        window
            .read_with(cx, |workspace, _| workspace.menu_entries().is_none())
            .unwrap(),
        "a second click on the open item must close the submenu"
    );
}

#[ui_kit::gpui::test]
fn hovering_a_neighbour_switches_the_open_submenu(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "menu-bar-hover");
    open_section(cx, window, 0);
    hover_bar(cx, window, 1);

    assert_eq!(
        window
            .read_with(cx, |workspace, _| workspace.menu_bar_section())
            .unwrap(),
        Some(1),
        "the hover must switch the open submenu to Edit"
    );
    assert_eq!(
        open_labels(cx, window).and_then(|labels| labels.first().cloned()),
        Some("Undo".to_string())
    );
}

#[ui_kit::gpui::test]
fn escape_closes_the_open_submenu(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "menu-bar-escape");
    open_section(cx, window, 0);
    press(cx, window, "escape");
    draw(cx, window);

    assert!(
        window
            .read_with(cx, |workspace, _| workspace.menu_entries().is_none())
            .unwrap(),
        "Escape leaves the submenu open"
    );
}

#[ui_kit::gpui::test]
fn alt_alone_highlights_the_first_section(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "menu-bar-alt");
    alt_tap(cx, window);
    assert_eq!(
        window
            .read_with(cx, |workspace, _| workspace.menu_bar_focus())
            .unwrap(),
        Some(0),
        "Alt alone must pick File"
    );

    press(cx, window, "down");
    draw(cx, window);
    assert_eq!(
        window
            .read_with(cx, |workspace, _| workspace.menu_bar_section())
            .unwrap(),
        Some(0),
        "Down after Alt must open File"
    );
}

#[ui_kit::gpui::test]
fn alt_f_opens_the_file_submenu(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "menu-bar-alt-f");
    cx.simulate_keystrokes(window.into(), "alt-f");
    cx.run_until_parked();
    draw(cx, window);

    assert_eq!(
        window
            .read_with(cx, |workspace, _| workspace.menu_bar_section())
            .unwrap(),
        Some(0)
    );
    assert_eq!(
        open_labels(cx, window).and_then(|labels| labels.first().cloned()),
        Some("Open Folder...".to_string())
    );
}

#[ui_kit::gpui::test]
fn a_narrow_window_moves_the_sections_to_the_overflow_row(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "menu-bar-narrow");
    let entries = sections(cx, window);
    resize(cx, window, 720.);

    let visible = window
        .read_with(cx, |workspace, _| workspace.menu_bar_visible())
        .unwrap();
    assert!(
        visible < entries.len(),
        "a narrow window keeps every section on the bar"
    );
    assert!(
        has_selector(cx, window, "menu-bar-overflow"),
        "the overflow row is missing"
    );

    click(cx, window, "menu-bar-overflow");
    draw(cx, window);
    let labels = open_labels(cx, window).expect("the overflow row opens a menu");
    let expected: Vec<String> = entries
        .iter()
        .skip(visible)
        .map(|entry| entry.label.to_string())
        .collect();
    assert_eq!(
        labels, expected,
        "the overflow row lists the hidden sections"
    );
}

#[ui_kit::gpui::test]
fn an_unknown_command_is_a_disabled_row(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "menu-bar-unknown");
    let entries = sections(cx, window);
    let help = entries
        .iter()
        .find(|entry| entry.label.as_ref() == "Help")
        .expect("the menu bar has Help");
    let rows: Vec<&MenuEntry> = help
        .submenu
        .iter()
        .filter(|entry| entry.id.as_deref().is_some_and(|id| id.starts_with("app.")))
        .collect();
    assert_eq!(rows.len(), 4, "Help has the four Arro rows");

    for row in rows {
        let id = row.id.as_deref().expect("the row names its command");
        let registered = window
            .read_with(cx, |workspace, _| workspace.commands().get(id).is_some())
            .unwrap();
        assert_eq!(
            row.on_click.is_some(),
            registered,
            "{id} is not shown the way its registration says"
        );
        if !registered {
            assert!(
                row.on_click.is_none() && !row.has_submenu(),
                "{id} must be inactive and must not panic"
            );
        }
    }
}
