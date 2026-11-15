//! The tabs of the terminal without a window: a second tab, its name, its split, the
//! search over the scrollback, the link of a printed path and the selection in the
//! clipboard. A real shell runs behind every tab, as in the other tests of the terminal.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use terminal::TerminalSession;
use ui_kit::gpui;
use ui_kit::gpui::{Modifiers, TestAppContext, VisualTestContext, WindowHandle};
use ui_kit::{MouseButton, point, px};
use workspace::{TERMINAL_TOOL, Workspace};

use crate::term_tabs::{Split, TabsState};
use crate::terminal_panel::{ActiveTerminal, TerminalPanel};
use crate::window_tests::{bounds_of, draw, open_sample_window, press, type_text, wait_for_signal};

fn terminal_of(cx: &mut TestAppContext) -> gpui::Entity<TerminalPanel> {
    cx.read(|cx| cx.global::<ActiveTerminal>().0.clone())
}

/// Opens a window with the terminal in its dock and waits until the shell is up.
fn open_terminal(
    cx: &mut TestAppContext,
    name: &str,
) -> (
    PathBuf,
    WindowHandle<Workspace>,
    gpui::Entity<TerminalPanel>,
) {
    let (root, window) = open_sample_window(cx, name);
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(TERMINAL_TOOL, window, cx)
        })
        .unwrap();
    let terminal = terminal_of(cx);
    wait_for_signal(cx, "the shell", |cx| {
        terminal.read_with(cx, |panel, _| panel.session().is_some())
    });
    draw(cx, window);
    (root, window, terminal)
}

fn session_of(
    cx: &mut TestAppContext,
    terminal: &gpui::Entity<TerminalPanel>,
) -> Arc<TerminalSession> {
    terminal
        .read_with(cx, |panel, _| panel.session().cloned())
        .expect("the terminal has a session")
}

/// Types a command line that prints `line` and waits until the screen shows it. The
/// command is the same in every shell of the tests: `echo`.
fn print_line(
    cx: &mut TestAppContext,
    terminal: &gpui::Entity<TerminalPanel>,
    line: &str,
) -> Arc<TerminalSession> {
    let session = session_of(cx, terminal);
    session
        .send(format!("echo {line}\r").into_bytes())
        .expect("input");
    let expected = line.to_string();
    // The shell echoes the command with its prompt: only a row of its own is the output.
    wait_for_signal(cx, "the line of the test", |_| {
        session
            .screen()
            .lines()
            .iter()
            .any(|shown| shown.trim() == expected)
    });
    session
}

/// One turn of the simulated clock.
fn tick(cx: &mut TestAppContext) {
    cx.executor().advance_clock(Duration::from_millis(50));
    cx.run_until_parked();
}

/// Where a cell of the grid sits on the screen. The cell width is not known before the
/// frame, so a click aims inside the first columns of the row.
fn cell_position(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    row: usize,
) -> gpui::Point<gpui::Pixels> {
    let bounds = bounds_of(cx, window, "terminal-grid");
    point(
        bounds.left() + px(9.),
        bounds.top() + px(6. + 18. * row as f32 + 9.),
    )
}

/// The row of the screen that shows `needle`, the latest one.
fn row_of(session: &TerminalSession, needle: &str) -> usize {
    let lines = session.screen().lines();
    lines
        .iter()
        .rposition(|line| line.trim() == needle)
        .unwrap_or_else(|| panic!("{needle} is not on the screen: {lines:?}"))
}

#[ui_kit::gpui::test]
fn a_second_tab_is_opened_named_switched_and_closed(cx: &mut TestAppContext) {
    let (root, window, terminal) = open_terminal(cx, "terminal-tabs");
    assert_eq!(
        terminal.read_with(cx, |panel, _| panel.tab_names()),
        ["Local"]
    );

    // The key of the terminal of the IDE, which the global keymap would take for a test.
    press(cx, window, "ctrl-shift-t");
    assert_eq!(
        terminal.read_with(cx, |panel, _| panel.tab_names()),
        ["Local", "Local (2)"]
    );
    assert_eq!(terminal.read_with(cx, |panel, _| panel.active_tab()), 1);

    press(cx, window, "alt-left");
    assert_eq!(terminal.read_with(cx, |panel, _| panel.active_tab()), 0);
    press(cx, window, "alt-right");
    assert_eq!(terminal.read_with(cx, |panel, _| panel.active_tab()), 1);

    // Rename the second tab the way the menu does, then type the new name.
    window
        .update(cx, |_, window, cx| {
            terminal.update(cx, |panel, cx| panel.begin_rename(1, window, cx))
        })
        .unwrap();
    type_text(cx, window, "Build");
    press(cx, window, "enter");
    assert_eq!(
        terminal.read_with(cx, |panel, _| panel.tab_names()),
        ["Local", "Build"]
    );

    press(cx, window, "ctrl-f4");
    assert_eq!(
        terminal.read_with(cx, |panel, _| panel.tab_names()),
        ["Local"]
    );
    assert_eq!(terminal.read_with(cx, |panel, _| panel.active_tab()), 0);

    // The tree of the project opens a session in the folder of its row.
    let folder = root.join("src");
    window
        .update(cx, |_, window, cx| {
            terminal.update(cx, |panel, cx| {
                panel.open_tab_in(folder.clone(), window, cx)
            })
        })
        .unwrap();
    assert_eq!(
        terminal.read_with(cx, |panel, _| panel.tab_directories()),
        [root.clone(), root.join("src")]
    );
}

#[ui_kit::gpui::test]
fn a_tab_split_right_holds_two_shells_until_one_closes(cx: &mut TestAppContext) {
    let (_, window, terminal) = open_terminal(cx, "terminal-split");
    terminal.update(cx, |panel, cx| panel.split(Split::Right, cx));
    assert_eq!(terminal.read_with(cx, |panel, _| panel.panes_len()), 2);
    assert_eq!(
        terminal.read_with(cx, |panel, _| panel.split_way()),
        Some(Split::Right)
    );

    wait_for_signal(cx, "the shell of the second pane", |cx| {
        let first = terminal.read_with(cx, |panel, _| panel.pane_session(0, 0));
        let second = terminal.read_with(cx, |panel, _| panel.pane_session(0, 1));
        first.is_some() && second.is_some()
    });
    let first = terminal
        .read_with(cx, |panel, _| panel.pane_session(0, 0))
        .expect("the first pane");
    let second = terminal
        .read_with(cx, |panel, _| panel.pane_session(0, 1))
        .expect("the second pane");
    assert_ne!(first.pid(), second.pid(), "two shells, not one");
    let second_pid = second.pid();
    drop(second);

    terminal.update(cx, |panel, cx| panel.close_pane(cx));
    assert_eq!(terminal.read_with(cx, |panel, _| panel.panes_len()), 1);
    // The program of the closed pane ends with it; the one of the other keeps running.
    wait_for_signal(cx, "the program of the closed pane", |_| {
        !platform::process_tree()
            .iter()
            .any(|process| process.pid == second_pid)
    });
    assert!(
        platform::process_tree()
            .iter()
            .any(|process| process.pid == first.pid()),
        "the first pane still runs"
    );
    let _ = window;
}

#[ui_kit::gpui::test]
fn ctrl_f_finds_a_line_of_the_scrollback_and_scrolls_to_it(cx: &mut TestAppContext) {
    let (_, window, terminal) = open_terminal(cx, "terminal-search");
    let session = session_of(cx, &terminal);
    for index in 1..=60 {
        session
            .send(format!("echo row-{index}\r").into_bytes())
            .expect("input");
    }
    wait_for_signal(cx, "the last printed line", |_| {
        session
            .screen()
            .lines()
            .iter()
            .any(|shown| shown.trim() == "row-60")
    });

    press(cx, window, "ctrl-f");
    assert!(terminal.read_with(cx, |panel, _| panel.search_bar_open()));
    type_text(cx, window, "row-42");
    wait_for_signal(cx, "the matches of the search", |cx| {
        !terminal
            .read_with(cx, |panel, _| panel.search_matches())
            .is_empty()
    });
    // Enter moves to the next match of the search.
    press(cx, window, "enter");

    wait_for_signal(cx, "the screen at the found line", |_| {
        let screen = session.screen();
        screen.display_offset > 0 && screen.lines().iter().any(|line| line.contains("row-42"))
    });
    // The match lies above the live screen, so it is a line of the scrollback.
    let matches = terminal.read_with(cx, |panel, _| panel.search_matches());
    assert!(matches.iter().any(|found| found.line < 0), "{matches:?}");
    assert!(
        terminal
            .read_with(cx, |panel, _| panel.current_match())
            .is_some()
    );
    tick(cx);
}

#[ui_kit::gpui::test]
fn control_click_on_a_printed_path_opens_it_at_the_line(cx: &mut TestAppContext) {
    let (root, window, terminal) = open_terminal(cx, "terminal-link");
    let path = root.join("src/main.txt");
    let session = print_line(cx, &terminal, "src/main.txt:2");
    let row = row_of(&session, "src/main.txt:2");
    let at = cell_position(cx, window, row);

    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_down(
        at,
        MouseButton::Left,
        Modifiers {
            control: true,
            ..Modifiers::default()
        },
    );
    visual.simulate_mouse_up(
        at,
        MouseButton::Left,
        Modifiers {
            control: true,
            ..Modifiers::default()
        },
    );
    drop(visual);
    cx.run_until_parked();

    wait_for_signal(cx, "the file of the link", |cx| {
        crate::window_tests::active_path(cx, window).as_deref() == Some(path.as_path())
    });
    let view = crate::window_tests::editor_of(cx, window, &path);
    let caret = view
        .read_with(cx, |view, _| view.head_offset())
        .unwrap_or(0);
    assert_eq!(caret, 6, "the second line of the file starts after `alpha`");
}

#[ui_kit::gpui::test]
fn a_dragged_selection_goes_to_the_clipboard(cx: &mut TestAppContext) {
    let (_, window, terminal) = open_terminal(cx, "terminal-selection");
    let session = print_line(cx, &terminal, "copy-me-123");
    let row = row_of(&session, "copy-me-123");
    let start = cell_position(cx, window, row);
    let bounds = bounds_of(cx, window, "terminal-grid");
    let end = point(
        bounds.right() - px(12.),
        bounds.top() + px(6. + 18. * row as f32 + 9.),
    );

    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
    visual.simulate_mouse_move(end, Some(MouseButton::Left), Modifiers::default());
    visual.simulate_mouse_up(end, MouseButton::Left, Modifiers::default());
    drop(visual);
    cx.run_until_parked();

    let selected = terminal
        .read_with(cx, |panel, _| panel.selection_text())
        .expect("the drag selects the line");
    assert_eq!(selected, "copy-me-123");

    // Plain Ctrl+C copies while a selection exists, as the setting of the IDE with
    // "Copy on selection" off does; without one the shell would get its interrupt.
    press(cx, window, "ctrl-c");
    let copied = cx.read(|cx| cx.read_from_clipboard().and_then(|item| item.text()));
    assert_eq!(copied.as_deref(), Some("copy-me-123"));
}

#[ui_kit::gpui::test]
fn a_double_click_selects_a_word(cx: &mut TestAppContext) {
    let (_, window, terminal) = open_terminal(cx, "terminal-word");
    let session = print_line(cx, &terminal, "one-two-three");
    let row = row_of(&session, "one-two-three");
    let start = cell_position(cx, window, row);
    let at = point(start.x + px(8. * 5.), start.y);

    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_event(ui_kit::MouseDownEvent {
        position: at,
        modifiers: Modifiers::default(),
        button: MouseButton::Left,
        click_count: 2,
        first_mouse: false,
    });
    visual.simulate_event(ui_kit::MouseUpEvent {
        position: at,
        modifiers: Modifiers::default(),
        button: MouseButton::Left,
        click_count: 2,
    });
    drop(visual);
    cx.run_until_parked();

    let selected = terminal
        .read_with(cx, |panel, _| panel.selection_text())
        .expect("the double click selects the word");
    assert_eq!(selected, "one-two-three");
}

#[ui_kit::gpui::test]
fn a_multi_line_paste_asks_before_it_reaches_the_shell(cx: &mut TestAppContext) {
    let (_, window, terminal) = open_terminal(cx, "terminal-paste");
    let text = "echo paste-one\necho paste-two";
    cx.update(|cx| cx.write_to_clipboard(ui_kit::ClipboardItem::new_string(text.to_string())));

    press(cx, window, "ctrl-v");
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    assert!(
        visual.debug_bounds("prompt").is_some(),
        "the paste of two lines asks first"
    );
    drop(visual);

    let session = session_of(cx, &terminal);
    press(cx, window, "enter");
    wait_for_signal(cx, "the pasted lines", |_| {
        let screen = session.screen();
        screen.contains("paste-one") && screen.contains("paste-two")
    });
}

#[ui_kit::gpui::test]
fn the_tabs_of_a_closed_window_open_again_empty_in_their_folder(cx: &mut TestAppContext) {
    let (root, window, terminal) = open_terminal(cx, "terminal-restore");
    press(cx, window, "ctrl-shift-t");
    window
        .update(cx, |_, window, cx| {
            terminal.update(cx, |panel, cx| panel.begin_rename(1, window, cx))
        })
        .unwrap();
    type_text(cx, window, "Build");
    press(cx, window, "enter");

    let state = terminal.read_with(cx, |panel, _| panel.saved_state());
    assert_eq!(state.tabs.len(), 2);
    assert_eq!(state.tabs[1].name, "Build");
    assert_eq!(state.tabs[0].directory, root);

    // The store of the window keeps those tabs for the next window of the project.
    let store = window
        .read_with(cx, |workspace, _| workspace.store())
        .unwrap()
        .expect("the window has a store");
    wait_for_signal(cx, "the saved tabs", |_| {
        let saved = store
            .get(&format!("terminal:{}", root.display()))
            .ok()
            .flatten();
        saved
            .and_then(|json| serde_json::from_str::<TabsState>(&json).ok())
            .is_some_and(|state| state.tabs.len() == 2)
    });

    let workspace = window.root(cx).unwrap();
    let restored = cx.update(|cx| {
        TerminalPanel::create(workspace.downgrade(), root.clone(), None, Some(store), cx)
    });
    assert_eq!(
        restored.read_with(cx, |panel, _| panel.tab_names()),
        ["Local", "Build"]
    );
    assert_eq!(
        restored.read_with(cx, |panel, _| panel.tab_directories()),
        [root.clone(), root]
    );
    assert_eq!(restored.read_with(cx, |panel, _| panel.tabs_len()), 2);
}
