//! The MCP page of Settings that points another program at the MCP server of the window:
//! the switch of the server, the manual strings and their copy buttons. No client, no
//! network and no real harness: only the strings and the clipboard.

use ui_kit::gpui;
use ui_kit::gpui::TestAppContext;
use workspace::Settings;

use crate::settings::TestMcpHome;
use crate::settings_catalog::{self, SettingsPage};
use crate::window_tests::{bounds_of, click, draw, open_sample_window, press, type_text};

#[test]
fn the_stdio_config_names_the_program_of_the_settings() {
    let mut settings = Settings::default();
    assert!(
        settings.mcp_enabled,
        "the switch enables the server by default"
    );
    assert!(settings_catalog::toggle(&mut settings, "mcp_enabled"));
    assert!(!settings.mcp_enabled, "the switch turns the server off");
    assert!(settings_catalog::set_text(
        &mut settings,
        "mcp_binary",
        r"C:\Tools\arro-mcp.exe"
    ));

    let stdio =
        settings_catalog::mcp_config(&settings, mcp_discovery::Transport::Stdio, None, None);

    assert!(stdio.contains("mcpServers"), "{stdio}");
    assert!(stdio.contains(r"C:\\Tools\\arro-mcp.exe"), "{stdio}");
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
fn the_page_shows_the_enable_row_and_copies_the_stdio_config(cx: &mut TestAppContext) {
    let (_root, window) = open_sample_window(cx, "mcp-settings-page");
    let home = test_support::scratch("mcp-settings-home");
    cx.update(|cx| cx.set_global(TestMcpHome(home)));
    press(cx, window, "ctrl-alt-s");
    draw(cx, window);
    bounds_of(cx, window, "settings");
    type_text(cx, window, "mcp");
    draw(cx, window);
    click(cx, window, "settings-page-mcp-server");
    draw(cx, window);

    bounds_of(cx, window, "settings-mcp-enabled");
    bounds_of(cx, window, "settings-mcp-client-claude");
    bounds_of(cx, window, "settings-mcp-project-client-claude");
    bounds_of(cx, window, "settings-mcp-copy-stdio");

    click(cx, window, "settings-mcp-copy-stdio");

    let copied = cx
        .read(|cx| cx.read_from_clipboard().and_then(|item| item.text()))
        .expect("Copy puts the config on the clipboard");
    assert!(copied.contains("arro-mcp"), "{copied}");
}
