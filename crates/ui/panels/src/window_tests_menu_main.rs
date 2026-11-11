//! The main menu of the window without a window: IDEA sections, their submenus, the
//! pointer and the keys, and every row running the command or action it names.

use std::fs;

use ui_kit::gpui;
use ui_kit::gpui::{
    Modifiers, ModifiersChangedEvent, TestAppContext, VisualTestContext, WindowHandle,
};
use ui_kit::{MenuAction, MenuEntry};
use workspace::{TERMINAL_TOOL, Workspace};

use crate::window_tests::{buffer_of, click, draw, editor_of, open_path, press, sample_project};

fn has_selector(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &'static str,
) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

/// `menu-1-3`: the debug selector of a row at a path.
fn selector(path: &[usize]) -> &'static str {
    let parts: Vec<String> = path.iter().map(|index| index.to_string()).collect();
    Box::leak(format!("menu-{}", parts.join("-")).into_boxed_str())
}

fn sections(cx: &mut TestAppContext, window: WindowHandle<Workspace>) -> Vec<MenuEntry> {
    window
        .update(cx, |workspace, _, cx| workspace.main_menu(cx))
        .unwrap()
}

/// The debug selector of a section of the menu bar.
fn bar_selector(index: usize) -> &'static str {
    Box::leak(format!("menu-bar-{index}").into_boxed_str())
}

/// Opens the submenu of a section of the menu bar by its item, as a click does.
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

/// The indexes of the rows named by `labels`, from the top section down.
fn path_to(entries: &[MenuEntry], labels: &[&str]) -> Vec<usize> {
    let mut path = Vec::new();
    let mut level = entries;
    for label in labels {
        let index = level
            .iter()
            .position(|entry| entry.label.as_ref() == *label)
            .unwrap_or_else(|| panic!("the main menu has no {label}"));
        path.push(index);
        level = &level[index].submenu;
    }
    path
}

fn open_parents(cx: &mut TestAppContext, window: WindowHandle<Workspace>, path: &[usize]) {
    for depth in 1..path.len() {
        click(cx, window, selector(&path[..depth]));
        draw(cx, window);
    }
}

fn click_row(cx: &mut TestAppContext, window: WindowHandle<Workspace>, path: &[usize]) {
    click(cx, window, selector(path));
    draw(cx, window);
}

#[ui_kit::gpui::test]
fn the_main_menu_has_the_idea_sections_and_opens_a_submenu_by_mouse(cx: &mut TestAppContext) {
    let (_, window) = crate::window_tests::open_sample_window(cx, "main-menu-sections");
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

    for (index, entry) in entries.iter().enumerate() {
        assert!(
            has_selector(cx, window, bar_selector(index)),
            "the {} section is missing from the menu bar",
            entry.label
        );
    }

    open_section(cx, window, 0);
    assert!(
        has_selector(cx, window, selector(&[0])),
        "the File submenu did not open under its item"
    );
    assert_eq!(
        window
            .read_with(cx, |workspace, _| workspace.menu_bar_section())
            .unwrap(),
        Some(0)
    );

    press(cx, window, "escape");
    draw(cx, window);
    assert!(
        window
            .read_with(cx, |workspace, _| workspace.menu_entries().is_none())
            .unwrap(),
        "Escape leaves the submenu open"
    );

    open_section(cx, window, 0);
    open_section(cx, window, 0);
    assert!(
        window
            .read_with(cx, |workspace, _| workspace.menu_entries().is_none())
            .unwrap(),
        "a second click on the open item must close the submenu"
    );
}

#[ui_kit::gpui::test]
fn the_arrows_walk_the_sections_and_open_or_close_a_submenu(cx: &mut TestAppContext) {
    let (_, window) = crate::window_tests::open_sample_window(cx, "main-menu-keys");

    // Alt alone picks the first section without opening it.
    alt_tap(cx, window);
    assert_eq!(
        window
            .read_with(cx, |workspace, _| workspace.menu_bar_focus())
            .unwrap(),
        Some(0)
    );
    press(cx, window, "right");
    assert_eq!(
        window
            .read_with(cx, |workspace, _| workspace.menu_bar_focus())
            .unwrap(),
        Some(1),
        "Right moves the highlight along the menu bar"
    );
    press(cx, window, "down");
    draw(cx, window);
    assert_eq!(
        window
            .read_with(cx, |workspace, _| workspace.menu_bar_section())
            .unwrap(),
        Some(1),
        "Down opens the submenu of the highlighted section"
    );
    assert!(has_selector(cx, window, selector(&[0])));

    // With a submenu open the horizontal arrows move to the neighbouring section.
    press(cx, window, "right");
    draw(cx, window);
    assert_eq!(
        window
            .read_with(cx, |workspace, _| workspace.menu_bar_section())
            .unwrap(),
        Some(2)
    );
    press(cx, window, "left");
    draw(cx, window);
    assert_eq!(
        window
            .read_with(cx, |workspace, _| workspace.menu_bar_section())
            .unwrap(),
        Some(1)
    );

    press(cx, window, "down");
    assert_eq!(
        window
            .read_with(cx, |workspace, _| workspace.menu_highlight())
            .unwrap(),
        Some(vec![0])
    );
    press(cx, window, "down");
    assert_eq!(
        window
            .read_with(cx, |workspace, _| workspace.menu_highlight())
            .unwrap(),
        Some(vec![1])
    );

    press(cx, window, "escape");
    draw(cx, window);
    assert!(
        window
            .read_with(cx, |workspace, _| workspace.menu_entries().is_none())
            .unwrap(),
        "Escape closes the submenu"
    );
}

#[ui_kit::gpui::test]
fn an_edit_row_duplicates_the_line_as_its_key_does(cx: &mut TestAppContext) {
    let root = sample_project("main-menu-duplicate");
    let path = root.join("src").join("main.txt");
    fs::write(&path, "alpha\nbeta\n").unwrap();
    let window = crate::window_tests::open_window(cx, root);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);

    let entries = sections(cx, window);
    let row = path_to(&entries, &["Edit", "Duplicate Line"]);
    open_section(cx, window, row[0]);
    let row = &row[1..];
    open_parents(cx, window, row);
    click_row(cx, window, row);

    assert_eq!(buffer_of(cx, &view), "alpha\nalpha\nbeta\n");
    assert!(
        window
            .read_with(cx, |workspace, _| workspace.menu_entries().is_none())
            .unwrap(),
        "a picked row leaves the menu open"
    );
}

#[ui_kit::gpui::test]
fn a_view_row_opens_the_terminal_tool_window(cx: &mut TestAppContext) {
    let (_, window) = crate::window_tests::open_sample_window(cx, "main-menu-terminal");
    let entries = sections(cx, window);
    let row = path_to(&entries, &["View", "Tool Windows", "Terminal"]);
    open_section(cx, window, row[0]);
    let row = &row[1..];
    open_parents(cx, window, row);
    click_row(cx, window, row);

    assert!(
        window
            .read_with(cx, |workspace, _| workspace.tool_open(TERMINAL_TOOL))
            .unwrap(),
        "View > Tool Windows > Terminal did not open the tool window"
    );
}

#[ui_kit::gpui::test]
fn recent_projects_and_about_open_from_the_main_menu(cx: &mut TestAppContext) {
    let (_, window) = crate::window_tests::open_sample_window(cx, "main-menu-about");
    let entries = sections(cx, window);
    let recent = path_to(&entries, &["File", "Open Recent"]);
    open_section(cx, window, recent[0]);
    let recent = &recent[1..];
    open_parents(cx, window, recent);
    click_row(cx, window, recent);
    assert!(
        has_selector(cx, window, selector(&[recent[0], 0])),
        "the empty recent list has no row"
    );

    let about = path_to(&entries, &["Help", "About Arro"]);
    open_section(cx, window, about[0]);
    let about = &about[1..];
    open_parents(cx, window, about);
    click_row(cx, window, about);

    let opened = window
        .read_with(cx, |workspace, _| {
            workspace
                .overlay_view()
                .and_then(|view| view.downcast::<workspace::AboutView>().ok())
                .is_some()
        })
        .unwrap();
    assert!(opened, "Help > About Arro did not open the About window");
}

struct Row {
    shortcut: Option<String>,
    id: Option<String>,
    action: MenuAction,
    label: String,
}

fn collect(entries: &[MenuEntry], prefix: &mut Vec<usize>, rows: &mut Vec<Row>) {
    for (index, entry) in entries.iter().enumerate() {
        prefix.push(index);
        if entry.has_submenu() {
            collect(&entry.submenu, prefix, rows);
        } else if let Some(action) = entry.on_click.clone() {
            rows.push(Row {
                shortcut: entry.shortcut.as_ref().map(ToString::to_string),
                id: entry.id.as_ref().map(ToString::to_string),
                action,
                label: entry.label.to_string(),
            });
        }
        prefix.pop();
    }
}

#[ui_kit::gpui::test]
fn every_row_runs_and_its_key_matches_the_keymap(cx: &mut TestAppContext) {
    let (_, window) = crate::window_tests::open_sample_window(cx, "main-menu-every-row");
    let mut rows = Vec::new();
    collect(&sections(cx, window), &mut Vec::new(), &mut rows);

    for row in &rows {
        let Some(id) = row.id.as_deref() else {
            continue;
        };
        let expected = window
            .read_with(cx, |workspace, _| workspace.shortcut_of_key(id))
            .unwrap();
        assert_eq!(
            row.shortcut.as_deref(),
            expected.as_deref(),
            "{} has a key that is not the one of {id}",
            row.label
        );
    }

    let closing = rows
        .iter()
        .position(|row| row.label == "Close Project")
        .expect("File has Close Project");
    // File > Exit closes the window the test runs in.
    let leaving = rows
        .iter()
        .position(|row| row.label == "Exit")
        .expect("File has Exit");
    let mut order: Vec<usize> = (0..rows.len())
        .filter(|index| *index != closing && *index != leaving)
        .collect();
    order.push(closing);

    for index in order {
        let action = rows[index].action.clone();
        let mut visual = VisualTestContext::from_window(window.into(), cx);
        visual.update(|window, cx| action(window, cx));
        cx.run_until_parked();
        window
            .update(cx, |workspace, window, cx| {
                if workspace.has_overlay() {
                    workspace.dismiss(window, cx);
                }
            })
            .ok();
        cx.run_until_parked();
        if rows[index].label == "Close Project" {
            break;
        }
        draw(cx, window);
    }
}
