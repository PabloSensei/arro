//! The SSE transport: one stream of events and the POST endpoint that feeds it, with the
//! same token and the same Exposed Tools as Streamable HTTP.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::sync::Arc;

use mcp_server::McpServer;
use serde_json::{Value, json};
use tool_gateway::{AuditLog, SessionScope, ToolGateway, Unavailable};

struct Fixture {
    server: McpServer,
    token: String,
}

fn fixture() -> Fixture {
    let gateway = Arc::new(ToolGateway::new(Arc::new(Unavailable), AuditLog::default()));
    let server = McpServer::start(gateway).expect("start");
    let token = server
        .issue_token(SessionScope {
            session: "sse".to_string(),
            project: "project-1".into(),
            cwd: "project-1".into(),
        })
        .expect("token");
    Fixture { server, token }
}

fn address(fixture: &Fixture) -> String {
    fixture
        .server
        .url()
        .strip_prefix("http://")
        .and_then(|rest| rest.strip_suffix("/mcp"))
        .expect("a local url")
        .to_string()
}

/// The session id of the endpoint event of the stream.
fn endpoint(line: &str) -> Option<String> {
    let rest = line.strip_prefix("data: /message?sessionId=")?;
    Some(rest.trim().to_string())
}

#[test]
fn the_sse_stream_lists_tools_through_a_post() {
    let fixture = fixture();
    let address = address(&fixture);
    let mut stream = TcpStream::connect(&address).expect("connect");
    let request = format!(
        "GET /sse HTTP/1.1\r\nHost: 127.0.0.1\r\nAccept: text/event-stream\r\n\
         Authorization: Bearer {}\r\n\r\n",
        fixture.token
    );
    stream.write_all(request.as_bytes()).expect("write");
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));
    let mut line = String::new();
    reader.read_line(&mut line).expect("status");
    assert!(line.contains("200"), "{line}");
    let mut session = None;
    line.clear();
    while reader.read_line(&mut line).expect("event") > 0 {
        if let Some(id) = endpoint(line.trim_end()) {
            session = Some(id);
            break;
        }
        line.clear();
    }
    let session = session.expect("the endpoint event of the stream");

    let body = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"});
    let body = body.to_string();
    let mut post = TcpStream::connect(&address).expect("connect");
    let request = format!(
        "POST /message?sessionId={session} HTTP/1.1\r\nHost: 127.0.0.1\r\n\
         Connection: close\r\nContent-Type: application/json\r\n\
         Authorization: Bearer {}\r\nContent-Length: {}\r\n\r\n{}",
        fixture.token,
        body.len(),
        body
    );
    post.write_all(request.as_bytes()).expect("write");
    let mut reply = String::new();
    post.read_to_string(&mut reply).expect("read");
    assert!(reply.starts_with("HTTP/1.1 202"), "{reply}");

    let mut event = String::new();
    let mut answer = None;
    line.clear();
    while reader.read_line(&mut line).expect("event") > 0 {
        if let Some(data) = line.trim_end().strip_prefix("data: ") {
            answer = Some(data.to_string());
            break;
        }
        line.clear();
    }
    event.push_str(&line);
    let answer: Value =
        serde_json::from_str(&answer.unwrap_or_else(|| panic!("no event: {event}"))).expect("json");
    let tools = answer["result"]["tools"]
        .as_array()
        .expect("the list of tools");
    assert!(!tools.is_empty(), "{answer}");
    let names: Vec<&str> = tools
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    assert!(names.contains(&"execute_tool"), "{names:?}");
    assert!(
        !names.contains(&"debug_stack"),
        "the debugger tools start router-only: {names:?}"
    );
}
