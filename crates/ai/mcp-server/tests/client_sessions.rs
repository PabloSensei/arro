//! The permissions of the clients outside the window. Each connection of such a client has a
//! session of its own, over Streamable HTTP and over SSE alike, so an "Allow for this session"
//! that the user gives one connection does not reach another. The agents of the window keep
//! the names of their own tokens.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mcp_server::McpServer;
use serde_json::{Value, json};
use tool_gateway::{
    Approval, ApprovalAnswer, ApprovalRequest, AuditLog, SessionScope, ToolGateway, Unavailable,
};

const PROTOCOL: &str = "2025-06-18";

/// The user answers the questions in turn; every question is kept for the test to read.
struct Script {
    answers: Mutex<VecDeque<ApprovalAnswer>>,
    asked: Mutex<Vec<String>>,
}

impl Approval for Script {
    fn ask(&self, request: &ApprovalRequest) -> ApprovalAnswer {
        self.asked.lock().unwrap().push(request.session.clone());
        self.answers
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(ApprovalAnswer::NoAnswer)
    }
}

struct Fixture {
    server: McpServer,
    gateway: Arc<ToolGateway>,
    script: Arc<Script>,
    token: String,
}

fn fixture(answers: impl IntoIterator<Item = ApprovalAnswer>) -> Fixture {
    let script = Arc::new(Script {
        answers: Mutex::new(answers.into_iter().collect()),
        asked: Mutex::new(Vec::new()),
    });
    let gateway = Arc::new(ToolGateway::with_approval(
        Arc::new(Unavailable),
        AuditLog::default(),
        script.clone(),
    ));
    let server = McpServer::start(gateway.clone()).expect("the server starts");
    let token = server
        .issue_client_token(SessionScope {
            session: "mcp".to_string(),
            project: "project-1".into(),
            cwd: "project-1".into(),
        })
        .expect("a token");
    Fixture {
        server,
        gateway,
        script,
        token,
    }
}

/// The text of the answer of a tool and whether it was an error. The answer comes plain or in
/// the event-stream form, so both are read.
fn result_of(reply: &str) -> (String, bool) {
    let value = reply
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter(|text| !text.trim().is_empty())
        .find_map(|text| serde_json::from_str::<Value>(text.trim()).ok())
        .or_else(|| serde_json::from_str::<Value>(reply).ok())
        .unwrap_or(Value::Null);
    let result = value.get("result").cloned().unwrap_or(Value::Null);
    let text = result
        .get("content")
        .and_then(Value::as_array)
        .and_then(|content| content.first())
        .and_then(|first| first.get("text"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let error = result
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    (text, error)
}

/// The user said no: the call ended in the refusal, not in the backend.
fn refused(answer: &(String, bool)) -> bool {
    answer.1 && answer.0.contains("the user refused")
}

fn build_project() -> Value {
    json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": "build_project", "arguments": {}},
    })
}

impl Fixture {
    /// The clients of the Agents tab that are connected now.
    fn connected_rows(&self) -> usize {
        self.gateway
            .clients()
            .clients()
            .iter()
            .filter(|row| row.connected)
            .count()
    }

    fn address(&self) -> String {
        self.server
            .url()
            .strip_prefix("http://")
            .and_then(|rest| rest.strip_suffix("/mcp"))
            .expect("a local url")
            .to_string()
    }

    /// One POST to `/mcp`: the status, the session of the answer and the body.
    fn post(&self, session: Option<&str>, body: &Value) -> (u16, Option<String>, String) {
        let body = body.to_string();
        let mut request = format!(
            "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\
             Content-Type: application/json\r\nAccept: application/json, text/event-stream\r\n\
             Authorization: Bearer {}\r\nContent-Length: {}\r\n",
            self.token,
            body.len()
        );
        if let Some(session) = session {
            request.push_str(&format!(
                "Mcp-Session-Id: {session}\r\nMCP-Protocol-Version: {PROTOCOL}\r\n"
            ));
        }
        request.push_str("\r\n");
        request.push_str(&body);
        let mut stream = TcpStream::connect(self.address()).expect("connect");
        stream.write_all(request.as_bytes()).expect("write");
        let mut response = String::new();
        stream.read_to_string(&mut response).expect("read");
        let (head, body) = response.split_once("\r\n\r\n").unwrap_or((&response, ""));
        let status = head
            .split_whitespace()
            .nth(1)
            .and_then(|status| status.parse().ok())
            .unwrap_or(0);
        let session = head.lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("mcp-session-id")
                .then(|| value.trim().to_string())
        });
        (status, session, body.to_string())
    }

    /// A client of Streamable HTTP: its `initialize` opens a connection, which the answer names.
    fn connect_http(&self, name: &str) -> String {
        let initialize = json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": PROTOCOL,
                "capabilities": {},
                "clientInfo": {"name": name, "version": "1"},
            },
        });
        let (status, session, reply) = self.post(None, &initialize);
        assert_eq!(status, 200, "{reply}");
        let session = session.expect("the answer names the session of the connection");
        let done = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
        let (status, _, _) = self.post(Some(&session), &done);
        assert_eq!(status, 202, "the client initialized");
        session
    }

    /// The SSE transport: the stream of events of one connection and its endpoint.
    fn connect_sse(&self) -> Sse {
        let mut stream = TcpStream::connect(self.address()).expect("connect");
        let request = format!(
            "GET /sse HTTP/1.1\r\nHost: 127.0.0.1\r\nAccept: text/event-stream\r\n\
             Authorization: Bearer {}\r\n\r\n",
            self.token
        );
        stream.write_all(request.as_bytes()).expect("write");
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).expect("status");
        assert!(line.contains("200"), "{line}");
        let endpoint = loop {
            line.clear();
            let read = reader.read_line(&mut line).expect("an event");
            assert!(read > 0, "the stream ended before its endpoint");
            if let Some(id) = line.trim_end().strip_prefix("data: /message?sessionId=") {
                break id.to_string();
            }
        };
        Sse { reader, endpoint }
    }
}

/// One connection of the SSE transport.
struct Sse {
    reader: BufReader<TcpStream>,
    endpoint: String,
}

impl Sse {
    /// Posts a request to the endpoint and reads its answer from the stream.
    fn request(&mut self, fixture: &Fixture, body: &Value) -> String {
        let body = body.to_string();
        let mut post = TcpStream::connect(fixture.address()).expect("connect");
        let request = format!(
            "POST /message?sessionId={} HTTP/1.1\r\nHost: 127.0.0.1\r\n\
             Connection: close\r\nContent-Type: application/json\r\n\
             Authorization: Bearer {}\r\nContent-Length: {}\r\n\r\n{}",
            self.endpoint,
            fixture.token,
            body.len(),
            body
        );
        post.write_all(request.as_bytes()).expect("write");
        let mut reply = String::new();
        post.read_to_string(&mut reply).expect("read");
        assert!(reply.starts_with("HTTP/1.1 202"), "{reply}");
        loop {
            let mut line = String::new();
            let read = self.reader.read_line(&mut line).expect("an event");
            assert!(read > 0, "the stream ended before the answer");
            if let Some(data) = line.trim_end().strip_prefix("data: ") {
                return data.to_string();
            }
        }
    }

    /// `initialize` of this connection, which names it for the rest of its calls.
    fn initialize(&mut self, fixture: &Fixture, name: &str) {
        let body = json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": PROTOCOL,
                "capabilities": {},
                "clientInfo": {"name": name, "version": "1"},
            },
        });
        let answer: Value = serde_json::from_str(&self.request(fixture, &body)).expect("json");
        assert!(answer["result"]["serverInfo"].is_object(), "{answer}");
    }

    /// A call of `build_project` on this connection.
    fn build(&mut self, fixture: &Fixture) -> (String, bool) {
        let answer = self.request(fixture, &build_project());
        result_of(&answer)
    }
}

#[test]
fn each_connection_of_a_client_has_a_permission_of_its_own() {
    let fixture = fixture([
        ApprovalAnswer::Session,
        ApprovalAnswer::Deny,
        ApprovalAnswer::Session,
    ]);
    // Two connections of one client over Streamable HTTP, the second a new `initialize`.
    let first = fixture.connect_http("Fixture Client");
    let second = fixture.connect_http("Fixture Client");

    let allowed = result_of(&fixture.post(Some(&first), &build_project()).2);
    assert!(
        !refused(&allowed),
        "the session allows the call: {allowed:?}"
    );
    let again = result_of(&fixture.post(Some(&first), &build_project()).2);
    assert!(!refused(&again), "{again:?}");
    assert_eq!(
        fixture.script.asked.lock().unwrap().len(),
        1,
        "the session asks no more"
    );

    let other = result_of(&fixture.post(Some(&second), &build_project()).2);
    assert!(
        refused(&other),
        "the other connection asks, and is refused: {other:?}"
    );
    assert_eq!(fixture.script.asked.lock().unwrap().len(), 2);

    // A client over SSE is a connection of its own as well.
    let mut sse = fixture.connect_sse();
    sse.initialize(&fixture, "Fixture Client");
    let allowed = sse.build(&fixture);
    assert!(
        !refused(&allowed),
        "the SSE connection asks and is allowed: {allowed:?}"
    );
    let again = sse.build(&fixture);
    assert!(!refused(&again), "{again:?}");

    let asked = fixture.script.asked.lock().unwrap().clone();
    assert_eq!(asked.len(), 3, "only the new connections asked: {asked:?}");
    assert!(
        asked
            .iter()
            .all(|session| session.starts_with("mcp-FixtureClient-")),
        "{asked:?}"
    );
    assert_ne!(asked[0], asked[1], "{asked:?}");
    assert_ne!(asked[1], asked[2], "{asked:?}");
    assert_ne!(asked[0], asked[2], "{asked:?}");
}

#[test]
fn taking_back_the_client_permissions_asks_again_on_every_connection() {
    let fixture = fixture([ApprovalAnswer::Session, ApprovalAnswer::Session]);
    let session = fixture.connect_http("Fixture Client");
    assert!(!refused(&result_of(
        &fixture.post(Some(&session), &build_project()).2
    )));

    assert_eq!(fixture.gateway.revoke_client_sessions(), 1);
    let asked_again = result_of(&fixture.post(Some(&session), &build_project()).2);
    assert!(
        !refused(&asked_again),
        "the second answer allows it: {asked_again:?}"
    );
    assert_eq!(
        fixture.script.asked.lock().unwrap().len(),
        2,
        "the client asked again"
    );
}

#[test]
fn a_client_whose_session_ends_leaves_the_list_as_disconnected() {
    let fixture = fixture(std::iter::empty());
    let session = fixture.connect_http("Fixture Client");
    assert_eq!(fixture.connected_rows(), 1, "the client is connected");

    fixture.server.disconnect(&session);
    // The session closes on the runtime of the server, so the test waits for the row to change.
    let deadline = Instant::now() + Duration::from_secs(10);
    while fixture.connected_rows() != 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        fixture.connected_rows(),
        0,
        "the client left the list as disconnected"
    );
    assert_eq!(
        fixture.gateway.clients().clients().len(),
        1,
        "its row stays"
    );
}

#[test]
fn a_token_of_the_window_keeps_the_session_it_names() {
    let fixture = fixture([ApprovalAnswer::Deny]);
    let token = fixture
        .server
        .issue_token(SessionScope {
            session: "claude-1".to_string(),
            project: "project-1".into(),
            cwd: "project-1".into(),
        })
        .expect("a token");
    let mut fixture = fixture;
    fixture.token = token;

    let session = fixture.connect_http("Claude Code");
    let refusal = result_of(&fixture.post(Some(&session), &build_project()).2);
    assert!(refused(&refusal), "{refusal:?}");
    assert_eq!(
        fixture.script.asked.lock().unwrap().clone(),
        ["claude-1".to_string()],
        "the agent of the window asks under its own name"
    );
}
