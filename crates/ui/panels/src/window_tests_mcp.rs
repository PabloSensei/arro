//! The AI page of Settings that points another program at the MCP server of the window:
//! the three ready strings, their copy buttons and the switch that shares the server.
//! No client, no network and no real harness: only the strings and the clipboard.

use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use workspace::{Settings, Workspace};

use crate::settings_catalog::{self, SettingsPage};
use crate::window_tests::{bounds_of, click, draw, open_sample_window, press, type_text};

fn has(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

#[test]
fn the_three_lines_name_the_program_of_the_settings() {
    let mut settings = Settings::default();
    assert!(
        settings.mcp_enabled,
        "the switch shares the server by default"
    );
    assert!(settings_catalog::toggle(&mut settings, "mcp_enabled"));
    assert!(!settings.mcp_enabled, "the switch turns the sharing off");
    assert!(settings_catalog::set_text(
        &mut settings,
        "mcp_binary",
        r"C:\Tools\arro-mcp.exe"
    ));

    let lines = settings_catalog::mcp_lines(&settings);

    assert_eq!(
        lines[0],
        r#"claude mcp add arro -- "C:\Tools\arro-mcp.exe""#
    );
    assert_eq!(
        lines[1],
        "[mcp_servers.arro]\ncommand = \"C:\\\\Tools\\\\arro-mcp.exe\""
    );
    assert_eq!(
        lines[2],
        r#"{"mcpServers": {"arro": {"command": "C:\\Tools\\arro-mcp.exe"}}}"#
    );
}

#[test]
fn the_page_sits_under_tools_and_ai_and_the_search_finds_it() {
    assert_eq!(SettingsPage::ToolsAi.parent(), Some(SettingsPage::Tools));
    assert_eq!(SettingsPage::Mcp.parent(), Some(SettingsPage::ToolsAi));
    assert_eq!(SettingsPage::ToolsAi.target(), SettingsPage::Mcp);
    assert_eq!(SettingsPage::Mcp.depth(), 2);

    let pages = settings_catalog::pages_matching("mcp");
    assert!(pages.contains(&SettingsPage::Mcp), "{pages:?}");
    assert!(
        pages.contains(&SettingsPage::ToolsAi) && pages.contains(&SettingsPage::Tools),
        "the group rows of the page stay: {pages:?}"
    );
}

#[ui_kit::gpui::test]
fn the_page_shows_three_lines_and_copies_the_claude_one(cx: &mut TestAppContext) {
    let (_root, window) = open_sample_window(cx, "mcp-settings-page");
    press(cx, window, "ctrl-alt-s");
    draw(cx, window);
    bounds_of(cx, window, "settings");
    type_text(cx, window, "mcp");
    draw(cx, window);
    click(cx, window, "settings-page-mcp-server");
    draw(cx, window);

    bounds_of(cx, window, "settings-mcp-line-claude");
    bounds_of(cx, window, "settings-mcp-line-codex");
    bounds_of(cx, window, "settings-mcp-line-json");
    assert!(
        has(cx, window, "settings-mcp-enabled"),
        "the switch of the page is on screen"
    );

    click(cx, window, "settings-mcp-copy-claude");

    let copied = cx
        .read(|cx| cx.read_from_clipboard().and_then(|item| item.text()))
        .expect("Copy puts the line of Claude Code on the clipboard");
    assert!(copied.starts_with("claude mcp add arro -- \""), "{copied}");
    assert!(copied.contains("arro-mcp"), "{copied}");
}
