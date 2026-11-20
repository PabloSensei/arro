//! The second step of the AI panel in a drawn window: the review buttons of the Changes
//! tab, a real editor selection as `@selection`, the queue after a stopped turn, and Ask AI
//! from the editor menu and its key. The mock agent and files the test writes stand for a
//! real session; there is no network and no process.

use std::path::{Path, PathBuf};

use change_tracker::TrackerState;
use ui_kit::gpui;
use ui_kit::gpui::{Entity, MouseButton, TestAppContext, VisualTestContext, WindowHandle};
use ui_kit::{Modifiers, px, size};
use workspace::{AI_TOOL, Workspace};

use crate::agents::{ASK_AI, ActiveAgents, AgentKind, AgentsPanel, Tab};
use crate::transcript::Item;
use crate::window_tests::{
    bounds_of, buffer_of, click, draw, editor_of, line_height, open_path, open_window, press,
    sample_project, type_text, wait_for_signal,
};

const BASE: &str = "alpha\nbeta\ngamma\ndelta\nepsilon\n";
const CHANGED: &str = "ALPHA\nbeta\ngamma\ndelta\nEPSILON\n";
/// What the first hunk leaves when it goes back to the base.
const FIRST_REVERTED: &str = "alpha\nbeta\ngamma\ndelta\nEPSILON\n";

fn panel(cx: &TestAppContext) -> Entity<AgentsPanel> {
    cx.read(|cx| cx.global::<ActiveAgents>().0.clone())
}

/// A window of the sample project that trusts the folder, so a session starts at once.
fn open(cx: &mut TestAppContext, name: &str) -> (WindowHandle<Workspace>, PathBuf) {
    let root = sample_project(name);
    let window = open_window(cx, root.clone());
    draw(cx, window);
    let state_dir = window
        .read_with(cx, |workspace, _| workspace.state_dir().to_path_buf())
        .unwrap();
    store::TrustList::new(&state_dir).trust(&root).unwrap();
    (window, root)
}

fn start_mock(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    let panel = panel(cx);
    window
        .update(cx, |_, window, cx| {
            panel.update(cx, |panel, cx| panel.request(AgentKind::Mock, window, cx))
        })
        .unwrap();
    draw(cx, window);
    wait_for_signal(cx, "the mock session", |cx| {
        panel.read_with(cx, |panel, _| panel.entries.len() == 1)
    });
}

fn send(cx: &mut TestAppContext, text: &str) {
    let panel = panel(cx);
    cx.update(|cx| {
        panel.update(cx, |panel, cx| {
            panel.composer.text = text.to_string();
            panel.send(cx);
        })
    });
}

fn items(cx: &TestAppContext) -> Vec<Item> {
    let panel = panel(cx);
    panel.read_with(cx, |panel, _| {
        panel
            .active_entry()
            .map(|entry| entry.transcript.items.clone())
            .unwrap_or_default()
    })
}

fn permission_pending(cx: &TestAppContext) -> bool {
    let panel = panel(cx);
    panel.read_with(cx, |panel, _| {
        panel
            .active_entry()
            .is_some_and(|entry| entry.transcript.pending_permission().is_some())
    })
}

fn running(cx: &TestAppContext) -> bool {
    let panel = panel(cx);
    panel.read_with(cx, |panel, _| {
        panel
            .active_entry()
            .is_some_and(|entry| entry.transcript.running)
    })
}

fn queued(cx: &TestAppContext) -> usize {
    let panel = panel(cx);
    panel.read_with(cx, |panel, _| {
        panel
            .active_entry()
            .map(|entry| entry.queued())
            .unwrap_or(0)
    })
}

fn stop(cx: &mut TestAppContext) {
    let panel = panel(cx);
    cx.update(|cx| panel.update(cx, |panel, cx| panel.stop(cx)));
}

/// Waits for the tracker of the window to take its first snapshot.
fn wait_for_tracker(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    let panel = panel(cx);
    draw(cx, window);
    wait_for_signal(cx, "the first snapshot", |cx| {
        panel.read_with(cx, |panel, _| panel.changes.state == TrackerState::Ready)
    });
}

/// A file the agent changed, written by the test and taken into the review.
fn change(cx: &mut TestAppContext, window: WindowHandle<Workspace>, root: &Path, text: &str) {
    std::fs::write(root.join("src").join("main.txt"), text).unwrap();
    let panel = panel(cx);
    let tracker = panel
        .read_with(cx, |panel, _| panel.tracker().cloned())
        .unwrap();
    tracker.refresh();
    draw(cx, window);
    wait_for_signal(cx, "the changed file", |cx| {
        panel.read_with(cx, |panel, _| {
            panel
                .changes
                .session
                .iter()
                .any(|file| file.path == "src/main.txt")
        })
    });
}

fn show_changes(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    let panel = panel(cx);
    cx.update(|cx| panel.update(cx, |panel, cx| panel.show_tab(Tab::Changes, cx)));
    draw(cx, window);
    wait_for_signal(cx, "the review", |cx| {
        panel.read_with(cx, |panel, _| panel.review.file.is_some())
    });
}

/// Opens the AI tool window, so that its buttons are laid out and can be clicked.
fn show_panel(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    crate::window_tests_cached_views::show_tool(cx, window, AI_TOOL);
}

/// The number of hunks the review shows, or `None` when no file is in it.
fn hunks(cx: &TestAppContext) -> Option<usize> {
    let panel = panel(cx);
    panel.read_with(cx, |panel, _| {
        panel.review.file.as_ref().map(|file| file.hunks.len())
    })
}

fn changed_paths(cx: &TestAppContext) -> Vec<String> {
    let panel = panel(cx);
    panel.read_with(cx, |panel, _| {
        panel
            .changes
            .session
            .iter()
            .map(|change| change.path.clone())
            .collect()
    })
}

/// A window tall enough for the whole editor menu, whose last row is Ask AI.
fn tall_window(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.update(|window, cx| {
        window.resize(size(px(1100.), px(1100.)));
        window.bounds_changed(cx);
    });
    drop(visual);
    cx.run_until_parked();
    draw(cx, window);
}

fn right_click_at(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    position: gpui::Point<gpui::Pixels>,
) {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_down(position, MouseButton::Right, Modifiers::default());
    visual.simulate_mouse_up(position, MouseButton::Right, Modifiers::default());
    drop(visual);
    cx.run_until_parked();
    draw(cx, window);
}

/// Clicks the row of the open menu whose label is `label`.
fn click_menu_row(cx: &mut TestAppContext, window: WindowHandle<Workspace>, label: &str) {
    let index = window
        .read_with(cx, |workspace, _| {
            workspace.menu_entries().and_then(|entries| {
                entries
                    .iter()
                    .position(|entry| entry.label.as_ref() == label)
            })
        })
        .unwrap()
        .unwrap_or_else(|| panic!("no menu row {label}"));
    let selector: &'static str = Box::leak(format!("menu-{index}").into_boxed_str());
    click(cx, window, selector);
    draw(cx, window);
}

fn menu_row_enabled(
    cx: &TestAppContext,
    window: WindowHandle<Workspace>,
    label: &str,
) -> Option<bool> {
    window
        .read_with(cx, |workspace, _| {
            workspace.menu_entries().and_then(|entries| {
                entries
                    .iter()
                    .find(|entry| entry.label.as_ref() == label)
                    .map(|entry| entry.on_click.is_some())
            })
        })
        .unwrap()
}

fn menu_row_shortcut(
    cx: &TestAppContext,
    window: WindowHandle<Workspace>,
    label: &str,
) -> Option<String> {
    window
        .read_with(cx, |workspace, _| {
            workspace.menu_entries().and_then(|entries| {
                entries
                    .iter()
                    .find(|entry| entry.label.as_ref() == label)
                    .and_then(|entry| entry.shortcut.as_ref().map(ToString::to_string))
            })
        })
        .unwrap()
}

fn panel_focused(cx: &mut TestAppContext, window: WindowHandle<Workspace>) -> bool {
    let focus = panel(cx).read_with(cx, |panel, _| panel.focus.clone());
    window
        .update(cx, |_, window, cx| window.focused(cx))
        .unwrap()
        == Some(focus)
}

fn composer_text(cx: &TestAppContext) -> String {
    panel(cx).read_with(cx, |panel, _| panel.composer.text.clone())
}

/// The project with a file of two hunks and a session that tracks it.
fn two_hunks(cx: &mut TestAppContext, name: &str) -> (WindowHandle<Workspace>, PathBuf) {
    let (window, root) = open(cx, name);
    let file = root.join("src").join("main.txt");
    std::fs::write(&file, BASE).unwrap();
    start_mock(cx, window);
    wait_for_tracker(cx, window);
    open_path(cx, window, &file);
    show_panel(cx, window);
    change(cx, window, &root, CHANGED);
    show_changes(cx, window);
    wait_for_signal(cx, "two hunks", |cx| hunks(cx) == Some(2));
    (window, root)
}

#[gpui::test]
fn the_accept_buttons_of_a_hunk_and_of_a_file_leave_the_agent_text_in_place(
    cx: &mut TestAppContext,
) {
    let (window, root) = two_hunks(cx, "ai-accept");
    let file = root.join("src").join("main.txt");
    let view = editor_of(cx, window, &file);
    assert_eq!(buffer_of(cx, &view), CHANGED);

    click(cx, window, "accept-hunk-0");
    wait_for_signal(cx, "the first hunk accepted", |cx| hunks(cx) == Some(1));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), CHANGED);
    assert_eq!(buffer_of(cx, &view), CHANGED);

    click(cx, window, "accept-file");
    wait_for_signal(cx, "the file accepted", |cx| changed_paths(cx).is_empty());
    assert_eq!(std::fs::read_to_string(&file).unwrap(), CHANGED);
    assert_eq!(buffer_of(cx, &view), CHANGED);
}

#[gpui::test]
fn the_revert_buttons_of_a_hunk_and_of_a_file_take_the_file_back_to_the_base(
    cx: &mut TestAppContext,
) {
    let (window, root) = two_hunks(cx, "ai-revert-buttons");
    let file = root.join("src").join("main.txt");
    let view = editor_of(cx, window, &file);
    assert_eq!(buffer_of(cx, &view), CHANGED);

    click(cx, window, "revert-hunk-0");
    wait_for_signal(cx, "the first hunk reverted", |cx| hunks(cx) == Some(1));
    wait_for_signal(cx, "the reverted file on disk", |_| {
        std::fs::read_to_string(&file).unwrap() == FIRST_REVERTED
    });
    wait_for_signal(cx, "the reverted buffer", |cx| {
        buffer_of(cx, &view) == FIRST_REVERTED
    });

    click(cx, window, "revert-file");
    wait_for_signal(cx, "the file reverted", |cx| changed_paths(cx).is_empty());
    wait_for_signal(cx, "the base on disk", |_| {
        std::fs::read_to_string(&file).unwrap() == BASE
    });
    wait_for_signal(cx, "the base in the buffer", |cx| {
        buffer_of(cx, &view) == BASE
    });
}

#[gpui::test]
fn the_selection_of_the_editor_goes_to_the_agent_with_its_file_and_lines(cx: &mut TestAppContext) {
    let (window, root) = open(cx, "ai-selection");
    start_mock(cx, window);
    let file = root.join("src").join("main.txt");
    open_path(cx, window, &file);
    press(cx, window, "ctrl-a");
    draw(cx, window);

    send(cx, "look at @selection");
    wait_for_signal(cx, "the message of the user", |cx| {
        items(cx).iter().any(|item| matches!(item, Item::User(_)))
    });

    let shown = items(cx);
    let Some(Item::User(text)) = shown.first() else {
        panic!("{shown:?}");
    };
    assert!(text.contains("main.txt:1-2"), "{text}");
    assert!(text.contains("alpha\nbeta"), "{text}");
}

#[gpui::test]
fn a_message_queued_during_a_turn_leaves_after_the_turn_is_stopped(cx: &mut TestAppContext) {
    let (window, _root) = open(cx, "ai-queue-after-cancel");
    start_mock(cx, window);
    send(cx, "first");
    draw(cx, window);
    wait_for_signal(cx, "the question of the first turn", |cx| {
        permission_pending(cx)
    });

    send(cx, "second");
    wait_for_signal(cx, "the queued message", |cx| queued(cx) == 1);
    stop(cx);
    draw(cx, window);
    wait_for_signal(cx, "the second turn after the stop", |cx| {
        let users = items(cx)
            .into_iter()
            .filter(|item| matches!(item, Item::User(_)))
            .count();
        !running(cx) && queued(cx) == 0 && users == 2 && !permission_pending(cx)
    });

    let shown = items(cx);
    let users: Vec<&String> = shown
        .iter()
        .filter_map(|item| match item {
            Item::User(text) => Some(text),
            _ => None,
        })
        .collect();
    assert_eq!(users.len(), 2, "{shown:?}");
    assert!(users[0].starts_with("first"), "{shown:?}");
    assert!(users[1].starts_with("second"), "{shown:?}");
}

#[gpui::test]
fn ask_ai_on_the_key_puts_the_selection_in_the_message_box_and_focuses_it(cx: &mut TestAppContext) {
    let (window, root) = open(cx, "ai-ask-key");
    start_mock(cx, window);
    let file = root.join("src").join("main.txt");
    open_path(cx, window, &file);
    press(cx, window, "ctrl-a");
    press(cx, window, "ctrl-alt-shift-a");
    draw(cx, window);

    let open = window
        .read_with(cx, |workspace, _| workspace.tool_open(AI_TOOL))
        .unwrap();
    assert!(open, "Ask AI opens the AI Sessions tool window");
    assert_eq!(composer_text(cx), "@selection ");
    assert!(panel_focused(cx, window), "the message box takes the focus");

    // The field takes what the user types, so the message is finished, not sent.
    type_text(cx, window, "why?");
    draw(cx, window);
    assert_eq!(composer_text(cx), "@selection why?");

    press(cx, window, "enter");
    wait_for_signal(cx, "the message of the user", |cx| {
        items(cx).iter().any(|item| matches!(item, Item::User(_)))
    });
    let shown = items(cx);
    let Some(Item::User(text)) = shown.first() else {
        panic!("{shown:?}");
    };
    assert!(text.contains("why?"), "{text}");
    assert!(text.contains("main.txt:1-2"), "{text}");
    let registered = window
        .read_with(cx, |workspace, _| {
            workspace.commands().get(ASK_AI).is_some()
        })
        .unwrap();
    assert!(
        registered,
        "Ask AI is in the common registry, so Find Action lists it"
    );
}

#[gpui::test]
fn ask_ai_in_the_editor_menu_runs_the_registered_command(cx: &mut TestAppContext) {
    let (window, root) = open(cx, "ai-ask-menu");
    start_mock(cx, window);
    tall_window(cx, window);
    let file = root.join("src").join("main.txt");
    open_path(cx, window, &file);
    press(cx, window, "ctrl-a");
    draw(cx, window);

    let body = bounds_of(cx, window, "editor-body-0");
    let first_line = body.top() + px(line_height(cx) / 2.);
    right_click_at(cx, window, gpui::point(body.left() + px(300.), first_line));

    assert_eq!(
        menu_row_enabled(cx, window, "Ask AI..."),
        Some(true),
        "the row of a selection is enabled"
    );
    assert_eq!(
        menu_row_shortcut(cx, window, "Ask AI...").as_deref(),
        Some(keymap::shortcut_label("ctrl-alt-shift-a", keymap::platform()).as_str())
    );
    assert_eq!(
        window
            .read_with(cx, |workspace, _| workspace.shortcut_of(ASK_AI))
            .unwrap()
            .as_deref(),
        Some("ctrl-alt-shift-a"),
        "the key of Ask AI is the one the menu shows"
    );

    click_menu_row(cx, window, "Ask AI...");
    let open = window
        .read_with(cx, |workspace, _| workspace.tool_open(AI_TOOL))
        .unwrap();
    assert!(open, "the row opens the AI Sessions tool window");
    assert_eq!(composer_text(cx), "@selection ");
    assert!(panel_focused(cx, window));
}

#[gpui::test]
fn ask_ai_is_disabled_without_a_selection(cx: &mut TestAppContext) {
    let (window, root) = open(cx, "ai-ask-empty");
    let file = root.join("src").join("main.txt");
    open_path(cx, window, &file);
    draw(cx, window);
    assert!(composer_text(cx).is_empty());

    let body = bounds_of(cx, window, "editor-body-0");
    let first_line = body.top() + px(line_height(cx) / 2.);
    right_click_at(cx, window, gpui::point(body.left() + px(300.), first_line));
    assert_eq!(
        menu_row_enabled(cx, window, "Ask AI..."),
        Some(false),
        "a caret alone is not a selection"
    );
    press(cx, window, "escape");
    draw(cx, window);

    // The key does nothing either: there is nothing to ask about.
    press(cx, window, "ctrl-alt-shift-a");
    draw(cx, window);
    let open = window
        .read_with(cx, |workspace, _| workspace.tool_open(AI_TOOL))
        .unwrap();
    assert!(!open, "Ask AI without a selection does not open the panel");
    assert!(composer_text(cx).is_empty());
}

#[test]
fn the_key_of_ask_ai_is_free_in_the_idea_keymap() {
    for table in [keymap::IDEA_DEFAULT, keymap::IDEA_DEFAULT_MACOS] {
        let shortcuts = keymap::idea_shortcuts(table).unwrap();
        assert!(
            shortcuts
                .values()
                .flatten()
                .all(|shortcut| shortcut.to_string() != "ctrl-alt-shift-a"),
            "the IDEA keymap gives ctrl-alt-shift-a to another action"
        );
    }
}
