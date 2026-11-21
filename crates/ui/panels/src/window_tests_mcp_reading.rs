//! The MCP Server page reads the settings files of the clients in the home folder of the
//! test. The page is reached through the search field, as typing "mcp" does, and the rows
//! leave "Reading..." for what the files say.

use std::fs;

use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use workspace::Workspace;

use crate::settings::TestMcpHome;
use crate::window_tests::{bounds_of, draw, open_sample_window, press, type_text};

/// Opens the Settings window and reaches the MCP Server page through the search field and
/// the arrow keys, as a user who types the name of the page does.
fn open_mcp_page(
    cx: &mut TestAppContext,
    name: &str,
    home: &std::path::Path,
) -> WindowHandle<Workspace> {
    let (_root, window) = open_sample_window(cx, name);
    cx.update(|cx| cx.set_global(TestMcpHome(home.to_path_buf())));
    press(cx, window, "ctrl-alt-s");
    draw(cx, window);
    bounds_of(cx, window, "settings");
    type_text(cx, window, "mcp");
    press(cx, window, "down");
    draw(cx, window);
    bounds_of(cx, window, "settings-mcp-enabled");
    cx.run_until_parked();
    draw(cx, window);
    window
}

/// The status of one client row as the frame drew it.
fn status(project: bool, client: &str) -> Option<String> {
    let prefix = if project {
        "settings-mcp-project-client"
    } else {
        "settings-mcp-client"
    };
    let name = format!("{prefix}-{client}-status");
    ui_kit::test_bounds()
        .into_iter()
        .find(|(drawn, _)| *drawn == name)
        .and_then(|(_, probe)| probe.label)
        .map(|label| label.to_string())
}

fn shown(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

#[gpui::test]
fn the_rows_leave_reading_on_a_home_without_client_files(cx: &mut TestAppContext) {
    let home = test_support::TempDir::new("mcp-reading-empty");
    let window = open_mcp_page(cx, "mcp-reading-empty", home.path());

    for client in ["claude", "codex", "junie", "vscode", "copilot"] {
        assert_eq!(
            status(false, client).as_deref(),
            Some("Not configured"),
            "the row of {client} says what its missing file means"
        );
        assert_eq!(
            status(true, client).as_deref(),
            Some("Not configured"),
            "the project row of {client} says the same"
        );
    }
    bounds_of(cx, window, "settings-mcp-client-claude-auto");
}

#[gpui::test]
fn a_file_with_another_entry_and_a_file_with_ours_read_apart(cx: &mut TestAppContext) {
    let home = test_support::TempDir::new("mcp-reading-files");
    fs::write(
        home.path().join(".claude.json"),
        r#"{"mcpServers":{"other":{"command":"other"}}}"#,
    )
    .unwrap();
    fs::create_dir_all(home.path().join(".codex")).unwrap();
    fs::write(
        home.path().join(".codex").join("config.toml"),
        "[mcp_servers.arro]\ncommand = \"arro-mcp\"\n",
    )
    .unwrap();
    let window = open_mcp_page(cx, "mcp-reading-files", home.path());

    assert_eq!(
        status(false, "claude").as_deref(),
        Some("Different entry"),
        "the file of Claude Code names another server"
    );
    assert_eq!(
        status(false, "codex").as_deref(),
        Some("Configured"),
        "the file of Codex names Arro"
    );
    assert_eq!(
        status(false, "junie").as_deref(),
        Some("Not configured"),
        "Junie has no file here"
    );
    assert!(
        shown(cx, window, "settings-mcp-client-claude-auto"),
        "another entry can be configured"
    );
    assert!(
        !shown(cx, window, "settings-mcp-client-codex-auto"),
        "a configured client needs no Configure button"
    );
}
