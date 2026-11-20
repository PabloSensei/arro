//! The stdio shim passes the IDEA names through to the server of the IDE.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::Arc;

use agent_testkit::FakeTools;
use mcp_server::McpServer;
use serde_json::{Value, json};
use tool_gateway::{AuditLog, SessionScope, ToolGateway};

fn initialize() -> Value {
    json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "idea-shim", "version": "0"},
        },
    })
}

fn talk(token: &str, url: &str, messages: &[Value]) -> Vec<Value> {
    let mut shim = Command::new(env!("CARGO_BIN_EXE_ide-mcp"))
        .env("IDE_MCP_URL", url)
        .env("IDE_MCP_TOKEN", token)
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
    let answers = stdout
        .lines()
        .filter_map(|line| line.ok().and_then(|line| serde_json::from_str(&line).ok()));
    let answers = answers.collect();
    assert!(shim.wait().expect("exit").success());
    answers
}

#[test]
fn the_shim_lists_and_calls_an_idea_tool() {
    let gateway = Arc::new(ToolGateway::new(Arc::new(FakeTools), AuditLog::default()));
    let server = McpServer::start(gateway).expect("server");
    let token = server
        .issue_token(SessionScope {
            session: "shim-idea".to_string(),
            project: "project".into(),
            cwd: "project".into(),
        })
        .expect("token");
    let messages = [
        initialize(),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
        json!({
            "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": {
                "name": "execute_tool",
                "arguments": {"command": "get_file_problems {\"filePath\": \"Found.txt\"}"},
            },
        }),
    ];

    let answers = talk(&token, &server.url(), &messages);

    assert_eq!(answers.len(), 3, "{answers:?}");
    for name in ["search_file", "git_status", "read_file", "execute_tool"] {
        assert!(
            answers[1].to_string().contains(name),
            "{name}: {}",
            answers[1]
        );
    }
    assert!(
        answers[2].to_string().contains("made-up error"),
        "{}",
        answers[2]
    );
}
