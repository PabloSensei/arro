//! `ide-mcp` without `IDE_MCP_URL` and `IDE_MCP_TOKEN`: it takes the window that has the
//! current folder open, from the entry point in the state folder of Arro.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

use agent_testkit::FakeTools;
use mcp_discovery::EntryPoint;
use mcp_server::McpServer;
use serde_json::{Value, json};
use test_support::TempDir;
use tool_gateway::{AuditLog, SessionScope, ToolGateway};

fn initialize() -> Value {
    json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "discovery-test", "version": "0"},
        },
    })
}

/// The state folder of Arro under a profile folder, as `base` resolves it on this system.
fn state_folder(profile: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        profile
            .join("Library")
            .join("Application Support")
            .join("Arro")
    }
    #[cfg(not(target_os = "macos"))]
    {
        profile.join("Arro")
    }
}

/// Runs the shim in `folder` with no `IDE_MCP_*` variables, as a client starts it, and
/// takes the answers of the messages.
fn talk(profile: &Path, folder: &Path, messages: &[Value]) -> Vec<Value> {
    let mut shim = Command::new(env!("CARGO_BIN_EXE_ide-mcp"))
        .current_dir(folder)
        .env_remove("IDE_MCP_URL")
        .env_remove("IDE_MCP_TOKEN")
        .env("LOCALAPPDATA", profile)
        .env("XDG_DATA_HOME", profile)
        .env("HOME", profile)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("shim");
    let mut stdin = shim.stdin.take().expect("stdin");
    for message in messages {
        writeln!(stdin, "{message}").expect("write");
    }
    drop(stdin);
    let stdout = BufReader::new(shim.stdout.take().expect("stdout"));
    let answers = stdout.lines().map_while(Result::ok);
    let answers = answers
        .filter_map(|line| serde_json::from_str(&line).ok())
        .collect();
    assert!(shim.wait().expect("exit").success());
    answers
}

#[test]
fn a_client_in_a_subfolder_finds_the_project_window() {
    let temp = TempDir::new("ide-mcp-discovery");
    let project = temp.path().join("shop");
    let folder = project.join("src").join("main");
    std::fs::create_dir_all(&folder).unwrap();
    let state = state_folder(temp.path());
    let gateway = Arc::new(ToolGateway::new(Arc::new(FakeTools), AuditLog::default()));
    let server = McpServer::start(gateway.clone()).expect("server");
    let token = server
        .issue_token(SessionScope {
            session: "mcp".to_string(),
            project: project.clone(),
            cwd: project.clone(),
        })
        .expect("token");
    let entry = EntryPoint {
        root: project.clone(),
        url: server.url(),
        token,
        pid: std::process::id(),
    };
    let _file = entry.write(&state).expect("entry point");
    let messages = [
        initialize(),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
        json!({
            "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": {"name": "find_symbol", "arguments": {"query": "Widget"}},
        }),
    ];

    let answers = talk(temp.path(), &folder, &messages);

    assert_eq!(answers.len(), 3, "{answers:?}");
    assert_eq!(answers[0]["result"]["protocolVersion"], json!("2025-06-18"));
    assert!(answers[1].to_string().contains("get_symbol_info"));
    assert!(
        answers[2].to_string().contains("class Widget"),
        "{}",
        answers[2]
    );
    let audit = gateway.audit().recent();
    assert_eq!(audit.len(), 1, "{audit:?}");
    assert_eq!(audit[0].client.as_deref(), Some("discovery-test"));
}

#[test]
fn without_a_window_the_answer_names_the_folder() {
    let temp = TempDir::new("ide-mcp-no-window");
    let folder = temp.path().join("shop");
    std::fs::create_dir_all(&folder).unwrap();

    let answers = talk(temp.path(), &folder, &[initialize()]);

    assert_eq!(answers.len(), 1, "{answers:?}");
    assert_eq!(answers[0]["id"], json!(1));
    let message = answers[0]["error"]["message"].as_str().unwrap_or_default();
    assert_eq!(
        message,
        format!("Arro is not running for {}", folder.display())
    );
}
