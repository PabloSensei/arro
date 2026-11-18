//! The Agents tab of the AI sessions window and the agents indicator of the status bar,
//! with a real MCP client that finds the window by its entry point and calls its tools.

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::thread;

use mcp_discovery::EntryPoint;
use serde_json::{Value, json};
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use workspace::Workspace;

use crate::agents::{ActiveAgents, AgentsPanel, Tab};
use crate::window_tests::{
    bounds_of, click, draw, open_sample_window, wait_for_settled_bounds, wait_for_signal,
};

const PROTOCOL: &str = "2025-06-18";

struct Client {
    address: String,
    token: String,
    session: Option<String>,
}

struct Reply {
    status: u16,
    session: Option<String>,
    body: String,
}

impl Client {
    /// An MCP client that found the window the way `arro-mcp` does: by the entry point of
    /// the project in the state folder.
    fn connect(entry: &EntryPoint, name: &str, version: &str) -> Self {
        let address = entry
            .url
            .strip_prefix("http://")
            .and_then(|rest| rest.strip_suffix("/mcp"))
            .expect("a loopback endpoint")
            .to_string();
        let mut client = Self {
            address,
            token: entry.token.clone(),
            session: None,
        };
        let initialize = json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": PROTOCOL,
                "capabilities": {},
                "clientInfo": {"name": name, "version": version},
            },
        });
        let reply = client.post(&initialize);
        assert_eq!(reply.status, 200, "{}", reply.body);
        client.session = reply.session;
        assert!(client.session.is_some(), "the server named a session");
        let done = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
        assert_eq!(client.post(&done).status, 202, "the client initialized");
        client
    }

    fn call(&mut self, tool: &str, arguments: Value) -> Reply {
        let body = json!({
            "jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": {"name": tool, "arguments": arguments},
        });
        self.post(&body)
    }

    fn post(&mut self, body: &Value) -> Reply {
        let body = body.to_string();
        let mut request = format!(
            "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\
             Content-Type: application/json\r\nAccept: application/json, text/event-stream\r\n\
             Authorization: Bearer {}\r\nContent-Length: {}\r\n",
            self.token,
            body.len()
        );
        if let Some(session) = &self.session {
            request.push_str(&format!("Mcp-Session-Id: {session}\r\n"));
            request.push_str(&format!("MCP-Protocol-Version: {PROTOCOL}\r\n"));
        }
        request.push_str("\r\n");
        request.push_str(&body);
        let Ok(mut stream) = TcpStream::connect(&self.address) else {
            return Reply {
                status: 0,
                session: None,
                body: String::new(),
            };
        };
        stream.write_all(request.as_bytes()).expect("write");
        let mut response = String::new();
        stream.read_to_string(&mut response).expect("read");
        let (head, body) = response.split_once("\r\n\r\n").unwrap_or((&response, ""));
        let status = head
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse().ok())
            .unwrap_or(0);
        let session = head.lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("mcp-session-id")
                .then(|| value.trim().to_string())
        });
        Reply {
            status,
            session,
            body: body.to_string(),
        }
    }
}

fn panel(cx: &TestAppContext) -> gpui::Entity<AgentsPanel> {
    cx.read(|cx| cx.global::<ActiveAgents>().0.clone())
}

fn has<T: 'static>(
    cx: &mut TestAppContext,
    window: WindowHandle<T>,
    selector: &'static str,
) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

fn show_agents(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    click(cx, window, "stripe-s-ai");
    wait_for_settled_bounds(cx, window, "tab-agents");
    click(cx, window, "tab-agents");
    draw(cx, window);
}

fn wait_for_client(cx: &mut TestAppContext, panel: &gpui::Entity<AgentsPanel>, calls: usize) {
    wait_for_signal(cx, "the MCP client of the test", |cx| {
        panel.read_with(cx, |panel, _| {
            panel
                .clients()
                .first()
                .is_some_and(|client| client.connected && client.calls >= calls)
        })
    });
}

fn wait_for_question(cx: &mut TestAppContext, panel: &gpui::Entity<AgentsPanel>) {
    wait_for_signal(cx, "the question of an Execute tool", |cx| {
        panel.read_with(cx, |panel, _| panel.approvals.pending().is_some())
    });
}

/// A client of the entry point, its calls and the tab: the row and the newest call are on
/// screen, and the number of the status bar opens the same tab.
#[cfg(windows)]
#[ui_kit::gpui::test]
fn the_agents_tab_lists_the_client_its_calls_and_the_status_bar_count(cx: &mut TestAppContext) {
    let (_root, window) = open_sample_window(cx, "mcp-clients-list");
    let panel = panel(cx);
    let entry = panel
        .read_with(cx, |panel, _| panel.entry_point())
        .expect("the window published its entry point");
    let mut client = Client::connect(&entry, "Fixture Client", "1.2");
    let answer = client.call("get_editor_context", json!({}));
    assert_eq!(answer.status, 200, "{}", answer.body);

    wait_for_client(cx, &panel, 1);
    wait_for_signal(cx, "the count of the status bar", |cx| {
        window
            .read_with(cx, |workspace, _| workspace.agent_clients().0)
            .unwrap()
            == 1
    });
    draw(cx, window);
    assert!(
        has(cx, window, "st-agents"),
        "the status bar names the client"
    );

    show_agents(cx, window);
    let (name, version, connected, calls) = panel.read_with(cx, |panel, _| {
        let clients = panel.clients();
        let client = clients.first().expect("the client is listed");
        (
            client.name.clone(),
            client.version.clone(),
            client.connected,
            client.calls,
        )
    });
    assert_eq!(name, "Fixture Client");
    assert_eq!(version, "1.2");
    assert!(connected, "the client is on the list as connected");
    assert_eq!(calls, 1);
    bounds_of(cx, window, "agent-client-first");
    bounds_of(cx, window, "agent-call-first");

    // The status bar opens the tab by itself.
    panel.update(cx, |panel, cx| panel.show_tab(Tab::Session, cx));
    draw(cx, window);
    click(cx, window, "st-agents");
    draw(cx, window);
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.tab),
        Tab::Agents,
        "the indicator opens the Agents tab"
    );
}

/// Allow for this session: the badge of the permission is on the tab, Revoke takes it
/// back, and the next Execute call asks again. While the call waits, the status bar says
/// that the agent works.
#[cfg(windows)]
#[ui_kit::gpui::test]
fn revoking_the_session_permission_makes_the_next_call_ask_again(cx: &mut TestAppContext) {
    let (_root, window) = open_sample_window(cx, "mcp-clients-revoke");
    let panel = panel(cx);
    let entry = panel
        .read_with(cx, |panel, _| panel.entry_point())
        .expect("the window published its entry point");
    let mut client = Client::connect(&entry, "Fixture Client", "1.2");
    assert_eq!(client.call("get_editor_context", json!({})).status, 200);
    wait_for_client(cx, &panel, 1);
    show_agents(cx, window);

    let first = thread::spawn(move || {
        let mut client = client;
        client.call("build_project", json!({})).status
    });
    wait_for_question(cx, &panel);
    wait_for_signal(cx, "the working mark", |cx| {
        window
            .read_with(cx, |workspace, _| workspace.agent_clients().1)
            .unwrap()
    });
    draw(cx, window);
    assert!(
        has(cx, window, "st-agents-busy"),
        "the status bar shows that an agent works"
    );
    window
        .update(cx, |workspace, window, cx| {
            workspace.run_command("ai.mcp.allow_session", window, cx)
        })
        .unwrap();
    wait_for_signal(cx, "the end of the first call", |_| first.is_finished());
    let _ = first.join().expect("the call of the client");
    draw(cx, window);
    bounds_of(cx, window, "agent-revoke");

    click(cx, window, "agent-revoke");
    draw(cx, window);
    assert!(
        !has(cx, window, "agent-revoke"),
        "the permission is taken back"
    );

    let second = thread::spawn(move || {
        let mut client = Client::connect(&entry, "Fixture Client", "1.2");
        client.call("build_project", json!({})).status
    });
    wait_for_question(cx, &panel);
    window
        .update(cx, |workspace, window, cx| {
            workspace.run_command("ai.mcp.deny", window, cx)
        })
        .unwrap();
    wait_for_signal(cx, "the end of the second call", |_| second.is_finished());
    let _ = second.join().expect("the call of the client");
}

/// Disconnect closes the transport session on the server: the client is no longer
/// connected, and its old session id is refused.
#[cfg(windows)]
#[ui_kit::gpui::test]
fn disconnect_closes_the_session_of_the_client(cx: &mut TestAppContext) {
    let (_root, window) = open_sample_window(cx, "mcp-clients-disconnect");
    let panel = panel(cx);
    let entry = panel
        .read_with(cx, |panel, _| panel.entry_point())
        .expect("the window published its entry point");
    let mut client = Client::connect(&entry, "Fixture Client", "1.2");
    assert_eq!(client.call("get_editor_context", json!({})).status, 200);
    wait_for_client(cx, &panel, 1);
    show_agents(cx, window);

    click(cx, window, "agent-disconnect");
    wait_for_signal(cx, "the disconnected client", |cx| {
        panel.read_with(cx, |panel, _| {
            panel
                .clients()
                .first()
                .is_some_and(|client| !client.connected)
        })
    });
    let mut refused = false;
    wait_for_signal(cx, "the closed session", |_| {
        refused = client.call("get_editor_context", json!({})).status != 200;
        refused
    });
    assert!(refused, "the server closed the session of the client");
    draw(cx, window);
    assert!(
        !has(cx, window, "agent-disconnect"),
        "a disconnected client offers no second Disconnect"
    );
}
