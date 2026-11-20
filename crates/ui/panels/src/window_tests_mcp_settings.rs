//! The MCP Server page as IDEA has it: the question before the server opens to other
//! programs, the auto-configuration of a client in a home folder of the test, and brave
//! mode. No real profile of the user is read or written: the rows take the home folder of
//! the test.

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use serde_json::json;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use workspace::{McpExecutePermission, Workspace};

use crate::settings::SettingsView;
use crate::settings::TestMcpHome;
use crate::window_tests::{bounds_of, click, draw, open_sample_window, press, type_text};

fn settings_view(
    cx: &TestAppContext,
    window: WindowHandle<Workspace>,
) -> gpui::Entity<SettingsView> {
    window
        .read_with(cx, |workspace, _| workspace.overlay_view())
        .expect("the window answers")
        .expect("the Settings window is open")
        .downcast::<SettingsView>()
        .unwrap_or_else(|_| panic!("the overlay is the Settings window"))
}

/// The page under the search field, with the client rows reading `home`.
fn open_mcp_page(
    cx: &mut TestAppContext,
    name: &str,
    home: &Path,
) -> (PathBuf, WindowHandle<Workspace>) {
    let (root, window) = open_sample_window(cx, name);
    cx.update(|cx| cx.set_global(TestMcpHome(home.to_path_buf())));
    press(cx, window, "ctrl-alt-s");
    draw(cx, window);
    bounds_of(cx, window, "settings");
    type_text(cx, window, "mcp");
    draw(cx, window);
    click(cx, window, "settings-page-mcp-server");
    draw(cx, window);
    (root, window)
}

fn wait_for_selector(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &'static str,
) {
    crate::window_tests_run::wait_for(cx, selector, |cx| {
        let mut visual = VisualTestContext::from_window(window.into(), cx);
        visual.debug_bounds(selector).is_some()
    });
}

#[ui_kit::gpui::test]
fn the_enable_switch_asks_before_the_server_opens(cx: &mut TestAppContext) {
    let home = test_support::scratch("mcp-settings-enable");
    let (_root, window) = open_mcp_page(cx, "mcp-settings-enable", &home);

    bound_and_click(cx, window, "settings-mcp-enabled");
    assert!(
        !settings_view(cx, window).read_with(cx, |view, _| view.draft_settings().mcp_enabled),
        "the first click turns the server off"
    );

    bound_and_click(cx, window, "settings-mcp-enabled");
    bounds_of(cx, window, "settings-mcp-enable-confirm");
    assert!(
        !settings_view(cx, window).read_with(cx, |view, _| view.draft_settings().mcp_enabled),
        "the server stays off until the user answers"
    );

    bound_and_click(cx, window, "settings-mcp-enable-yes");
    assert!(
        settings_view(cx, window).read_with(cx, |view, _| view.draft_settings().mcp_enabled),
        "Enable turns the server on after the question"
    );
}

fn bound_and_click(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &'static str,
) {
    bounds_of(cx, window, selector);
    click(cx, window, selector);
    draw(cx, window);
}

#[ui_kit::gpui::test]
fn auto_configure_writes_the_claude_entry_in_the_home_of_the_test(cx: &mut TestAppContext) {
    let home = test_support::TempDir::new("mcp-settings-home");
    let file = home.path().join(".claude.json");
    fs::write(&file, r#"{"theme":"dark"}"#).unwrap();
    let (_root, window) = open_mcp_page(cx, "mcp-settings-auto", home.path());

    wait_for_selector(cx, window, "settings-mcp-client-claude-auto");
    bound_and_click(cx, window, "settings-mcp-client-claude-auto");

    crate::window_tests_run::wait_for(cx, "the entry of Claude Code", |_| {
        fs::read_to_string(&file).is_ok_and(|text| text.contains("\"arro\""))
    });
    let text = fs::read_to_string(&file).unwrap();
    let root: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(root["theme"], "dark", "the other keys stay: {text}");
    assert!(root["mcpServers"]["arro"]["command"].is_string(), "{text}");
    assert!(
        PathBuf::from(format!("{}.arro-backup", file.display())).is_file(),
        "a copy of the old file lies next to it"
    );
}

#[ui_kit::gpui::test]
fn router_only_mode_and_the_table_stay_in_the_settings(cx: &mut TestAppContext) {
    let home = test_support::scratch("mcp-settings-tools");
    let (_root, window) = open_mcp_page(cx, "mcp-settings-tools", &home);

    // The table is below the page, so the switches are reached through its view rather
    // than through a click that the scroll may put outside the window.
    let view = settings_view(cx, window);
    view.update(cx, |view, cx| {
        view.set_tool_switch("find_symbol", false, false, cx);
        view.set_router_mode(true, cx);
    });

    let settings = view.read_with(cx, |view, _| view.draft_settings().clone());
    assert!(
        !settings
            .mcp_tool_settings
            .get("find_symbol")
            .expect("the tool row")
            .enabled,
        "the switch of the tool reaches the draft"
    );
    assert!(
        settings.mcp_router_only_mode,
        "the router-only mode is kept"
    );
}

#[cfg(windows)]
#[ui_kit::gpui::test]
fn brave_mode_takes_away_the_question_of_execute_terminal_command(cx: &mut TestAppContext) {
    let home = test_support::scratch("mcp-settings-brave");
    let (root, window) = open_mcp_page(cx, "mcp-settings-brave", &home);
    let _ = window;

    window
        .update(cx, |workspace, _, cx| {
            workspace.update_settings(
                |settings| {
                    settings.mcp_execute_permission = McpExecutePermission::WithoutConfirmation;
                },
                cx,
            )
        })
        .unwrap();
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(50));
    cx.run_until_parked();

    let agents = cx.read(|cx| cx.global::<crate::agents::ActiveAgents>().0.clone());
    let (gateway, approvals) = agents.read_with(cx, |panel, _| {
        let mcp = panel.mcp.clone().expect("the MCP server of the window");
        (mcp.gateway.clone(), panel.approvals.clone())
    });
    let scope = tool_gateway::SessionScope {
        session: "brave".to_string(),
        project: root.clone(),
        cwd: root,
    };
    let call = thread::spawn(move || {
        gateway.call(
            &scope,
            "execute_terminal_command",
            &json!({"command": "cmd /d /c echo brave mode", "timeout": 5000}),
        )
    });

    crate::window_tests_run::wait_for(cx, "the terminal command", |_| call.is_finished());
    let answer = call.join().expect("the call").expect("the command runs");
    assert!(answer.contains("brave mode"), "{answer}");
    assert!(
        approvals.pending().is_none(),
        "brave mode put no question on screen"
    );
    let _ = Arc::strong_count(&approvals);
}
