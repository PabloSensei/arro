//! `ide-mcp` between a stdio client and the real MCP server of the IDE.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::Arc;

use agent_testkit::FakeTools;
use mcp_server::McpServer;
use serde_json::{Value, json};
use tool_gateway::{AuditLog, SessionScope, ToolGateway};

fn initialize(version: &str) -> Value {
    json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {
            "protocolVersion": version,
            "capabilities": {},
            "clientInfo": {"name": "shim-test", "version": "0"},
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
    let answers = stdout.lines().map_while(Result::ok);
    let answers = answers
        .filter_map(|line| serde_json::from_str(&line).ok())
        .collect();
    assert!(shim.wait().expect("exit").success());
    answers
}

fn server() -> (McpServer, String) {
    let gateway = Arc::new(ToolGateway::new(Arc::new(FakeTools), AuditLog::default()));
    let server = McpServer::start(gateway).expect("server");
    let token = server
        .issue_token(SessionScope {
            session: "shim".to_string(),
            project: "project".into(),
            cwd: "project".into(),
        })
        .expect("token");
    (server, token)
}

#[test]
fn a_stdio_client_lists_and_calls_tools() {
    let (server, token) = server();
    let messages = [
        initialize("2025-06-18"),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "find_symbol", "arguments": {"query": "Widget"}}}),
    ];

    let answers = talk(&token, &server.url(), &messages);

    assert_eq!(answers.len(), 3, "{answers:?}");
    assert_eq!(answers[0]["result"]["protocolVersion"], json!("2025-06-18"));
    assert!(answers[1].to_string().contains("get_symbol_info"));
    assert!(
        answers[2].to_string().contains("class Widget"),
        "{}",
        answers[2]
    );
}

#[test]
fn every_supported_epoch_initializes_through_the_shim() {
    let (server, token) = server();
    // rmcp 3.8.0 answers the newest epoch with the newest one it knows, as the protocol allows.
    let epochs = [
        ("2025-06-18", "2025-06-18"),
        ("2025-11-25", "2025-11-25"),
        ("2026-07-28", "2025-11-25"),
    ];
    for (asked, agreed) in epochs {
        let answers = talk(&token, &server.url(), &[initialize(asked)]);

        assert_eq!(answers[0]["result"]["protocolVersion"], json!(agreed));
    }
}

#[test]
fn a_bad_token_becomes_a_json_rpc_error() {
    let (server, _) = server();

    let answers = talk("wrong", &server.url(), &[initialize("2025-06-18")]);

    assert_eq!(answers[0]["id"], json!(1));
    let message = answers[0]["error"]["message"].as_str().unwrap_or_default();
    assert!(message.contains("401"), "{message}");
}
