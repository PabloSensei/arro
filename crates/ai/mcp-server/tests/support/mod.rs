//! A running MCP server over plain HTTP for the tests of the tools of IDEA's server: a
//! fake backend, a token and the requests of one client.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;

use mcp_server::McpServer;
use serde_json::{Value, json};
use tool_gateway::{
    Approval, ApprovalAnswer, ApprovalRequest, AuditLog, SessionScope, ToolBackend, ToolGateway,
};

/// The user says yes to every question, so a test reaches the backend.
pub struct Allow;

impl Approval for Allow {
    fn ask(&self, _: &ApprovalRequest) -> ApprovalAnswer {
        ApprovalAnswer::Once
    }
}

pub struct Fixture {
    pub server: McpServer,
    pub gateway: Arc<ToolGateway>,
    pub token: String,
    pub headers: Vec<String>,
}

impl Fixture {
    pub fn start(backend: Arc<dyn ToolBackend>) -> Self {
        let gateway = Arc::new(ToolGateway::with_approval(
            backend,
            AuditLog::default(),
            Arc::new(Allow),
        ));
        // The debugger tools start router-only, as in IDEA (the tests of that live with the
        // exposure); these tests call them directly.
        let mut exposure = gateway.exposure();
        for name in tool_gateway::TOOL_NAMES {
            if tool_gateway::is_debug_tool(name) {
                exposure.set_state(
                    name,
                    tool_gateway::ToolState {
                        enabled: true,
                        router_only: false,
                    },
                );
            }
        }
        gateway.set_exposure(exposure);
        let server = McpServer::start(gateway.clone()).expect("the server starts");
        let token = server
            .issue_token(SessionScope {
                session: "session-1".to_string(),
                project: "project".into(),
                cwd: "project".into(),
            })
            .expect("a token");
        let mut fixture = Self {
            server,
            gateway,
            token,
            headers: Vec::new(),
        };
        fixture.headers = fixture.connect();
        fixture
    }

    fn post(&self, headers: &[String], body: &Value) -> (u16, String, String) {
        let address = self
            .server
            .url()
            .strip_prefix("http://")
            .and_then(|rest| rest.strip_suffix("/mcp"))
            .expect("a local url")
            .to_string();
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
        let status = head
            .split_whitespace()
            .nth(1)
            .and_then(|status| status.parse().ok())
            .unwrap_or(0);
        (status, head.to_string(), body.to_string())
    }

    fn bearer(&self) -> String {
        format!("Authorization: Bearer {}", self.token)
    }

    fn connect(&self) -> Vec<String> {
        let body = json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "idea-test", "version": "0"},
            },
        });
        let (status, head, reply) = self.post(&[self.bearer()], &body);
        assert_eq!(status, 200, "{reply}");
        let session = head
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("mcp-session-id")
                    .then(|| value.trim().to_string())
            })
            .expect("a session id");
        let headers = vec![
            self.bearer(),
            format!("Mcp-Session-Id: {session}"),
            "MCP-Protocol-Version: 2025-06-18".to_string(),
        ];
        let done = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
        let _ = self.post(&headers, &done);
        headers
    }

    /// The list of the tools with their schemas.
    pub fn list(&self) -> String {
        let body = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"});
        let (status, _, reply) = self.post(&self.headers, &body);
        assert_eq!(status, 200, "{reply}");
        reply
    }

    /// A call of a tool; the answer is the body of the JSON-RPC reply.
    pub fn call(&self, name: &str, arguments: Value) -> String {
        let body = json!({
            "jsonrpc": "2.0", "id": 7, "method": "tools/call",
            "params": {"name": name, "arguments": arguments},
        });
        let (status, _, reply) = self.post(&self.headers, &body);
        assert_eq!(status, 200, "{name}: {reply}");
        reply
    }

    /// The text of the answer of a tool, and whether it was an error.
    pub fn answer(&self, name: &str, arguments: Value) -> (String, bool) {
        let reply = self.call(name, arguments);
        // The transport of the server answers in the event-stream form as well.
        let value = reply
            .lines()
            .filter_map(|line| line.strip_prefix("data: "))
            .filter(|text| !text.trim().is_empty())
            .find_map(|text| serde_json::from_str::<Value>(text.trim()).ok())
            .or_else(|| serde_json::from_str::<Value>(&reply).ok())
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
        assert!(!text.is_empty(), "{name}: {reply}");
        (text, error)
    }
}
