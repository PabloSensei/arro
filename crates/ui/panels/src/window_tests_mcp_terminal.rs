//! The bar above the terminal that offers to configure an MCP client. A shell of the
//! tests runs a stand-in `claude` put in a folder of the test; the real client is never
//! started, and the home of the test takes the entry of Arro.

#![cfg(windows)]

use std::fs;
use std::path::PathBuf;

use mcp_discovery::Client;
use ui_kit::gpui;
use ui_kit::gpui::{Modifiers, TestAppContext, VisualTestContext, WindowHandle};
use workspace::Workspace;

use crate::settings::TestMcpHome;
use crate::window_tests::{draw, press, scratch, wait_for_signal};
use crate::window_tests_terminal_tabs::open_terminal;

/// A folder with a `claude.cmd` that prints a line instead of starting the real client.
fn stand_in_claude(name: &str) -> PathBuf {
    let folder = scratch(name).join("bin");
    fs::create_dir_all(&folder).unwrap();
    fs::write(
        folder.join("claude.cmd"),
        "@echo off\r\necho stand-in claude\r\n",
    )
    .unwrap();
    folder
}

fn click(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    let bounds = visual
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("no element {selector} on screen"));
    visual.simulate_click(bounds.center(), Modifiers::default());
    drop(visual);
    cx.run_until_parked();
}

#[ui_kit::gpui::test]
fn a_claude_without_the_entry_gets_a_bar_and_auto_configure_writes_the_test_home(
    cx: &mut TestAppContext,
) {
    let home = test_support::TempDir::new("mcp-terminal-home");
    cx.update(|cx| cx.set_global(TestMcpHome(home.path().to_path_buf())));
    let (_, window, terminal) = open_terminal(cx, "mcp-terminal-bar");
    let session = terminal
        .read_with(cx, |panel, _| panel.session().cloned())
        .expect("the shell of the terminal");
    let bin = stand_in_claude("mcp-terminal-bin");
    session
        .send(format!("set \"PATH={};%PATH%\"\r", bin.display()).into_bytes())
        .expect("input");
    // The marker proves the shell read the line before the command of the client.
    session.send(b"echo ready\r".to_vec()).expect("input");
    wait_for_signal(cx, "the folder of the stand-in client", |_| {
        session
            .screen()
            .lines()
            .iter()
            .any(|line| line.trim() == "ready")
    });

    window
        .update(cx, |_, window, cx| {
            window.focus(&terminal.read(cx).focus(), cx)
        })
        .unwrap();
    for key in ["c", "l", "a", "u", "d", "e"] {
        press(cx, window, key);
    }
    // The caret of the drawn prompt is the signal that the shell finished rendering the
    // line: pressing `enter` in the middle of a redraw would read a half-drawn prompt.
    wait_for_signal(cx, "the typed command line", |_| {
        let screen = session.screen();
        screen.cursor.is_some() && screen.contains("claude")
    });
    press(cx, window, "enter");

    wait_for_signal(cx, "the bar of the MCP client", |cx| {
        draw(cx, window);
        let mut visual = VisualTestContext::from_window(window.into(), cx);
        visual.debug_bounds("terminal-mcp-suggestion").is_some()
    });
    assert_eq!(
        terminal.read_with(cx, |panel, _| panel.suggestion_client()),
        Some(Client::ClaudeCode),
        "the bar names the client of the command"
    );

    click(cx, window, "terminal-mcp-suggestion-configure");
    wait_for_signal(cx, "the entry in the home of the test", |_| {
        fs::read_to_string(home.path().join(".claude.json"))
            .is_ok_and(|text| text.contains("\"arro\""))
    });
}
