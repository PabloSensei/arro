//! The server as a client sees it, in plain HTTP.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;

use mcp_server::McpServer;
use serde_json::{Value, json};
use tool_gateway::{
    AuditLog, Confidence, DiagnosticsPage, DiagnosticsQuery, EditorContext, Position, SessionScope,
    SymbolHit, SymbolInfo, SymbolPage, SymbolQuery, ToolBackend, ToolError, ToolGateway,
};

struct Fake;

impl ToolBackend for Fake {
    fn editor_context(&self, _scope: &SessionScope) -> Result<EditorContext, ToolError> {
        Ok(EditorContext::default())
    }

    fn find_symbol(
        &self,
        scope: &SessionScope,
        query: &SymbolQuery,
    ) -> Result<SymbolPage, ToolError> {
        Ok(SymbolPage {
            hits: vec![SymbolHit {
                name: query.query.clone(),
                kind: "class".to_string(),
                path: scope.project.join("Foo.txt"),
                line: 1,
                content: format!("class {}", query.query),
                before: Vec::new(),
                after: Vec::new(),
            }],
            source: "fake".to_string(),
            confidence: Confidence::Certain,
            next: None,
        })
    }

    fn symbol_info(&self, _: &SessionScope, _: &Position) -> Result<SymbolInfo, ToolError> {
        Err(ToolError::Failed("not here".to_string()))
    }

    fn diagnostics(
        &self,
        _: &SessionScope,
        _: &DiagnosticsQuery,
    ) -> Result<DiagnosticsPage, ToolError> {
        Err(ToolError::Failed("not here".to_string()))
    }
}

struct Fixture {
    server: McpServer,
    gateway: Arc<ToolGateway>,
    token: String,
}

fn fixture() -> Fixture {
    let gateway = Arc::new(ToolGateway::new(Arc::new(Fake), AuditLog::default()));
    let server = McpServer::start(gateway.clone()).expect("start");
    let token = server
        .issue_token(SessionScope {
            session: "session-1".to_string(),
            project: "project-1".into(),
            cwd: "project-1".into(),
        })
        .expect("token");
    Fixture {
        server,
        gateway,
        token,
    }
}

struct Reply {
    status: u16,
    session: Option<String>,
    body: String,
}

fn post(fixture: &Fixture, headers: &[String], body: &Value) -> Reply {
    let url = fixture.server.url();
    let address = url
        .strip_prefix("http://")
        .and_then(|rest| rest.strip_suffix("/mcp"))
        .expect("a local url");
    let mut stream = TcpStream::connect(address).expect("connect");
    let body = body.to_string();
    let mut request = format!(
        "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\
         Content-Type: application/json\r\nAccept: application/json, text/event-stream\r\n\
         Content-Length: {}\r\n",
        body.len()
    );
    for header in headers {
        request.push_str(header);
        request.push_str("\r\n");
    }
    request.push_str("\r\n");
    request.push_str(&body);
    stream.write_all(request.as_bytes()).expect("write");
    let mut response = String::new();
    stream.read_to_string(&mut response).expect("read");
    let (head, body) = response.split_once("\r\n\r\n").unwrap_or((&response, ""));
    let status = head.split_whitespace().nth(1).expect("status");
    let session = head.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("mcp-session-id")
            .then(|| value.trim().to_string())
    });
    Reply {
        status: status.parse().expect("status code"),
        session,
        body: body.to_string(),
    }
}

fn bearer(fixture: &Fixture) -> String {
    format!("Authorization: Bearer {}", fixture.token)
}

fn initialize_body() -> Value {
    json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "test", "version": "0"},
        },
    })
}

/// Opens an MCP session and returns the headers of further calls.
fn connect(fixture: &Fixture) -> Vec<String> {
    let reply = post(fixture, &[bearer(fixture)], &initialize_body());
    assert_eq!(reply.status, 200, "{}", reply.body);
    let session = reply.session.expect("a session id");
    let headers = vec![
        bearer(fixture),
        format!("Mcp-Session-Id: {session}"),
        "MCP-Protocol-Version: 2025-06-18".to_string(),
    ];
    let done = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
    assert_eq!(post(fixture, &headers, &done).status, 202);
    headers
}

#[test]
fn a_foreign_origin_gets_403_even_with_a_good_token() {
    let fixture = fixture();
    for origin in ["https://evil.example", "http://localhost:3000", "null"] {
        let headers = [bearer(&fixture), format!("Origin: {origin}")];

        let reply = post(&fixture, &headers, &initialize_body());

        assert_eq!(reply.status, 403, "{origin}");
    }
}

#[test]
fn a_foreign_origin_gets_403_without_a_token_too() {
    let fixture = fixture();
    let headers = ["Origin: https://evil.example".to_string()];

    assert_eq!(post(&fixture, &headers, &initialize_body()).status, 403);
}

#[test]
fn no_token_or_a_wrong_one_gets_401() {
    let fixture = fixture();
    let wrong = ["Authorization: Bearer nope".to_string()];

    assert_eq!(post(&fixture, &[], &initialize_body()).status, 401);
    assert_eq!(post(&fixture, &wrong, &initialize_body()).status, 401);
}

#[test]
fn a_revoked_token_stops_working() {
    let fixture = fixture();

    fixture.server.revoke(&fixture.token);

    let reply = post(&fixture, &[bearer(&fixture)], &initialize_body());
    assert_eq!(reply.status, 401);
}

#[test]
fn both_supported_protocol_epochs_initialize() {
    let fixture = fixture();
    for version in ["2025-06-18", "2025-11-25"] {
        let mut body = initialize_body();
        body["params"]["protocolVersion"] = json!(version);

        let reply = post(&fixture, &[bearer(&fixture)], &body);

        assert_eq!(reply.status, 200, "{version}: {}", reply.body);
        assert!(reply.body.contains(version), "{}", reply.body);
    }
}

#[test]
fn tools_are_listed_with_their_schema() {
    let fixture = fixture();
    let headers = connect(&fixture);
    let list = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"});

    let reply = post(&fixture, &headers, &list);

    assert!(reply.body.contains("find_symbol"), "{}", reply.body);
    assert!(reply.body.contains("get_editor_context"), "{}", reply.body);
    assert!(reply.body.contains("get_symbol_info"), "{}", reply.body);
    assert!(reply.body.contains("get_diagnostics"), "{}", reply.body);
    assert!(
        reply.body.contains("\"readOnlyHint\":true"),
        "{}",
        reply.body
    );
}

#[test]
fn a_call_runs_in_the_session_of_its_token() {
    let fixture = fixture();
    let headers = connect(&fixture);
    let call = json!({
        "jsonrpc": "2.0", "id": 3, "method": "tools/call",
        "params": {"name": "find_symbol", "arguments": {"query": "Widget"}},
    });

    let reply = post(&fixture, &headers, &call);

    assert!(reply.body.contains("class Widget"), "{}", reply.body);
    assert!(reply.body.contains("project-1"), "{}", reply.body);
    let audit = fixture.gateway.audit().recent();
    assert_eq!(audit[0].session, "session-1");
}

#[test]
fn a_failed_tool_is_a_tool_error_and_not_a_protocol_error() {
    let fixture = fixture();
    let headers = connect(&fixture);
    let call = json!({
        "jsonrpc": "2.0", "id": 4, "method": "tools/call",
        "params": {"name": "find_symbol", "arguments": {"query": " "}},
    });

    let reply = post(&fixture, &headers, &call);

    assert!(reply.body.contains("\"isError\":true"), "{}", reply.body);
    assert!(reply.body.contains("the query is empty"), "{}", reply.body);
}
