//! A keystroke redraws the editor and the parts that show its caret or text, not the
//! window itself. The tests count the draws that show a change of the window's own state
//! and read what the status bar and the tab strips drew.

use ui_kit::gpui::{self, TestAppContext, WindowHandle};
use workspace::{PROBLEMS_TOOL, Workspace, window_render_count};

use crate::window_tests::{click, draw, open_path, open_sample_window, press, type_text};

/// The draws that showed a change of the window's own state. GPUI redraws the root view
/// when any view under it repaints, so the plain number of calls of `Workspace::render`
/// also counts the frames of the editor, which every keystroke makes.
fn renders(cx: &TestAppContext) -> usize {
    cx.read(window_render_count)
}

fn status_caret(cx: &TestAppContext, window: WindowHandle<Workspace>) -> Option<String> {
    window
        .read_with(cx, |workspace, cx| workspace.status_caret_on_screen(cx))
        .unwrap()
}

fn tab_dirty(cx: &TestAppContext, window: WindowHandle<Workspace>) -> Vec<Vec<bool>> {
    window
        .read_with(cx, |workspace, cx| workspace.tab_dirty_on_screen(cx))
        .unwrap()
}

#[ui_kit::gpui::test]
fn keystrokes_and_caret_moves_do_not_redraw_the_window(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "keystroke-repaint");
    let path = root.join("src").join("main.txt");
    open_path(cx, window, &path);
    let before = renders(cx);

    for _ in 0..10 {
        press(cx, window, "a");
    }
    draw(cx, window);
    assert_eq!(
        renders(cx),
        before,
        "ten letters do not change what the window itself shows"
    );

    for _ in 0..10 {
        press(cx, window, "left");
    }
    draw(cx, window);
    assert_eq!(
        renders(cx),
        before,
        "ten caret moves do not change what the window itself shows"
    );
}

#[ui_kit::gpui::test]
fn the_status_bar_shows_every_caret_move(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "caret-status");
    let path = root.join("src").join("main.txt");
    open_path(cx, window, &path);
    assert_eq!(status_caret(cx, window).as_deref(), Some("1:1"));

    let moves = [
        ("right", "1:2"),
        ("right", "1:3"),
        ("down", "2:3"),
        ("end", "2:5"),
        ("left", "2:4"),
    ];
    for (key, expected) in moves {
        press(cx, window, key);
        draw(cx, window);
        assert_eq!(
            status_caret(cx, window).as_deref(),
            Some(expected),
            "the status bar after {key}"
        );
    }
}

#[ui_kit::gpui::test]
fn the_star_of_the_tab_follows_edits_undo_and_save(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "dirty-star");
    let path = root.join("src").join("main.txt");
    open_path(cx, window, &path);
    assert_eq!(tab_dirty(cx, window), vec![vec![false]]);

    type_text(cx, window, "X");
    draw(cx, window);
    assert_eq!(
        tab_dirty(cx, window),
        vec![vec![true]],
        "an edit puts the star on the tab"
    );

    press(cx, window, "ctrl-z");
    draw(cx, window);
    assert_eq!(
        tab_dirty(cx, window),
        vec![vec![false]],
        "undo to the saved text takes the star away"
    );

    type_text(cx, window, "Y");
    draw(cx, window);
    assert_eq!(tab_dirty(cx, window), vec![vec![true]]);

    press(cx, window, "ctrl-s");
    draw(cx, window);
    assert_eq!(
        tab_dirty(cx, window),
        vec![vec![false]],
        "Ctrl+S takes the star away"
    );
}

#[ui_kit::gpui::test]
fn another_tab_a_tool_window_and_a_closed_file_still_redraw_the_window(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "window-repaint");
    let main = root.join("src").join("main.txt");
    let notes = root.join("src").join("notes.txt");
    open_path(cx, window, &main);
    open_path(cx, window, &notes);

    let before = renders(cx);
    click(cx, window, "tab-0-0");
    draw(cx, window);
    assert!(
        renders(cx) > before,
        "another tab changes what the window shows"
    );

    let before = renders(cx);
    window
        .update(cx, |workspace, window, cx| {
            workspace.toggle_tool(PROBLEMS_TOOL, window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
    assert!(
        renders(cx) > before,
        "a tool window changes what the window shows"
    );

    let before = renders(cx);
    window
        .update(cx, |workspace, window, cx| {
            workspace.close_active_tab(window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
    assert!(
        renders(cx) > before,
        "a closed file changes what the window shows"
    );
}
