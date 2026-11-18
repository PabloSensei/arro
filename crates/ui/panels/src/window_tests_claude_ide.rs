//! A harness of the IDE protocol in a window without a window: a client finds the
//! server by the lock file of a temporary home, reads the problems and proposes a file
//! that only the user decides. There is no real harness and no network.

use std::fs;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use editor_view::EditorView;
use lang_api::{DiagnosticEntry, Severity, TextPoint, TextSpan};
use serde_json::{Value, json};
use ui_kit::gpui;
use ui_kit::gpui::{Entity, Modifiers, TestAppContext, VisualTestContext, WindowHandle};
use workspace::Workspace;

use crate::claude_ide::ActiveClaude;
use crate::problems::ActiveProblems;
use crate::window_tests::{
    active_path, click, draw, editor_of, open_path, open_window, press, sample_project,
    wait_for_signal,
};

const BASE: &str = "alpha\nbeta\ngamma\ndelta\nepsilon\n";
const CHANGED: &str = "ALPHA\nbeta\ngamma\ndelta\nEPSILON\n";
/// The base with only the first hunk of `CHANGED` taken in.
const FIRST_ACCEPTED: &str = "ALPHA\nbeta\ngamma\ndelta\nepsilon\n";

/// A client of the protocol, as a harness is: one WebSocket, masked frames, JSON-RPC.
struct Client {
    stream: TcpStream,
    next: u64,
}

impl Client {
    fn connect(lock: &Path) -> Result<Self, String> {
        let text = fs::read_to_string(lock).map_err(|error| error.to_string())?;
        let parsed: Value = serde_json::from_str(&text).map_err(|error| error.to_string())?;
        let token = parsed["authToken"].as_str().unwrap_or_default();
        // The port is the name of the lock file, as the protocol says.
        let port = port_of(lock.file_name().and_then(|name| name.to_str()))?;
        let mut stream =
            TcpStream::connect(("127.0.0.1", port)).map_err(|error| error.to_string())?;
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .map_err(|error| error.to_string())?;
        let request = format!(
            "GET / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nUpgrade: websocket\r\n\
             Connection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
             Sec-WebSocket-Version: 13\r\nSec-WebSocket-Protocol: mcp\r\n\
             X-Claude-Code-Ide-Authorization: {token}\r\n\r\n"
        );
        stream
            .write_all(request.as_bytes())
            .map_err(|error| error.to_string())?;
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            stream
                .read_exact(&mut byte)
                .map_err(|error| error.to_string())?;
            head.push(byte[0]);
        }
        let head = String::from_utf8_lossy(&head).into_owned();
        if !head.starts_with("HTTP/1.1 101") {
            return Err(head);
        }
        Ok(Self { stream, next: 1 })
    }

    fn send(&mut self, message: &Value) -> Result<(), String> {
        let text = message.to_string();
        let mask = [0x11u8, 0x22, 0x33, 0x44];
        let mut frame = vec![0x81u8];
        match text.len() {
            length if length < 126 => frame.push(0x80 | length as u8),
            length => {
                frame.push(0x80 | 126);
                frame.extend_from_slice(&(length as u16).to_be_bytes());
            }
        }
        frame.extend_from_slice(&mask);
        for (index, byte) in text.bytes().enumerate() {
            frame.push(byte ^ mask[index % 4]);
        }
        self.stream
            .write_all(&frame)
            .map_err(|error| error.to_string())
    }

    fn pong(&mut self, payload: &[u8]) -> Result<(), String> {
        let mut frame = vec![0x8au8, payload.len() as u8];
        frame.extend_from_slice(payload);
        self.stream
            .write_all(&frame)
            .map_err(|error| error.to_string())
    }

    fn read(&mut self) -> Result<Value, String> {
        loop {
            let mut head = [0u8; 2];
            self.stream
                .read_exact(&mut head)
                .map_err(|error| error.to_string())?;
            let opcode = head[0] & 0x0f;
            let masked = head[1] & 0x80 != 0;
            let length = match head[1] & 0x7f {
                126 => {
                    let mut raw = [0u8; 2];
                    self.stream
                        .read_exact(&mut raw)
                        .map_err(|error| error.to_string())?;
                    u16::from_be_bytes(raw) as usize
                }
                127 => {
                    let mut raw = [0u8; 8];
                    self.stream
                        .read_exact(&mut raw)
                        .map_err(|error| error.to_string())?;
                    u64::from_be_bytes(raw) as usize
                }
                other => other as usize,
            };
            let mask = if masked {
                let mut mask = [0u8; 4];
                self.stream
                    .read_exact(&mut mask)
                    .map_err(|error| error.to_string())?;
                Some(mask)
            } else {
                None
            };
            let mut payload = vec![0u8; length];
            self.stream
                .read_exact(&mut payload)
                .map_err(|error| error.to_string())?;
            if let Some(mask) = mask {
                for (index, byte) in payload.iter_mut().enumerate() {
                    *byte ^= mask[index % 4];
                }
            }
            match opcode {
                0x1 => {
                    let text = String::from_utf8(payload).map_err(|error| error.to_string())?;
                    return serde_json::from_str(&text).map_err(|error| error.to_string());
                }
                0x8 => return Err("the IDE closed the connection".to_string()),
                0x9 => self.pong(&payload)?,
                _ => {}
            }
        }
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next;
        self.next += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
        loop {
            let answer = self.read()?;
            if answer["id"] == json!(id) {
                return Ok(answer);
            }
        }
    }

    fn initialize(&mut self) -> Result<(), String> {
        let answer = self.request(
            "initialize",
            json!({
                "protocolVersion": "2026-07-28",
                "capabilities": {},
                "clientInfo": {"name": "window-test", "version": "0"},
            }),
        )?;
        if answer.get("result").is_none() {
            return Err(answer.to_string());
        }
        self.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
    }

    fn call(&mut self, tool: &str, arguments: Value) -> Result<Vec<Value>, String> {
        let answer = self.request("tools/call", json!({"name": tool, "arguments": arguments}))?;
        if let Some(error) = answer.get("error") {
            return Err(error.to_string());
        }
        answer["result"]["content"]
            .as_array()
            .cloned()
            .ok_or_else(|| answer.to_string())
    }
}

/// The port of a lock file name, `<port>.lock`, for the client of the tests.
fn port_of(name: Option<&str>) -> Result<u16, String> {
    name.and_then(|name| name.strip_suffix(".lock"))
        .and_then(|port| port.parse().ok())
        .ok_or_else(|| "the lock file names no port".to_string())
}

fn open(cx: &mut TestAppContext, name: &str) -> (WindowHandle<Workspace>, PathBuf) {
    let root = sample_project(name);
    let window = open_window(cx, root.clone());
    draw(cx, window);
    (window, root)
}

fn bridge(cx: &TestAppContext) -> std::sync::Arc<claude_bridge::Bridge> {
    cx.read(|cx| {
        cx.global::<ActiveClaude>()
            .bridge
            .clone()
            .expect("the window runs the IDE protocol")
    })
}

fn run<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Receiver<Result<T, String>> {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(work());
    });
    receiver
}

fn wait_for_answer<T>(cx: &mut TestAppContext, receiver: &Receiver<Result<T, String>>) -> T {
    let mut answer = None;
    wait_for_signal(cx, "the answer of the client", |_| {
        if let Ok(value) = receiver.try_recv() {
            answer = Some(value);
        }
        answer.is_some()
    });
    answer.expect("the client answered").expect("the client")
}

#[gpui::test]
fn a_client_by_the_lock_file_reads_a_problem_of_a_closed_file(cx: &mut TestAppContext) {
    let root = crate::window_tests_lang::project(
        "claude-diagnostics",
        &[("src/Main.txt", "one\ntwo\nthree\n")],
    );
    let file = root.join("src").join("Main.txt");
    let language = crate::window_tests_lang::FakeLanguage::new();
    language.set_diagnostics(vec![DiagnosticEntry {
        span: TextSpan {
            start: TextPoint {
                line: 1,
                character: 0,
            },
            end: TextPoint {
                line: 1,
                character: 3,
            },
        },
        severity: Severity::Error,
        message: "cannot resolve the name".to_string(),
        unnecessary: false,
        code: None,
    }]);
    let window = crate::window_tests_lang::open_window_with(cx, root, language);
    draw(cx, window);
    let problems = cx.update(|cx| cx.global::<ActiveProblems>().0.clone());
    open_path(cx, window, &file);
    wait_for_signal(cx, "the problem of the substitute language", |cx| {
        draw(cx, window);
        problems.read_with(cx, |hub, cx| !hub.panel().read(cx).items().is_empty())
    });
    // The tab is closed: the answer comes from the Problems list of the window, not from
    // an open document of the editor.
    press(cx, window, "ctrl-f4");
    draw(cx, window);
    assert!(active_path(cx, window).is_none(), "the file is closed");
    let bridge = bridge(cx);
    let lock = bridge.lock_path().to_path_buf();
    let uri = claude_bridge::file_uri(&file);
    let asked = uri.clone();

    let answers = run(move || {
        let mut client = Client::connect(&lock)?;
        client.initialize()?;
        let content = client.call("getDiagnostics", json!({"uri": asked}))?;
        let text = content
            .first()
            .and_then(|block| block["text"].as_str())
            .ok_or("the answer has no text")?;
        serde_json::from_str(text).map_err(|error| error.to_string())
    });
    let files: Value = wait_for_answer(cx, &answers);

    assert_eq!(files[0]["uri"], json!(uri));
    let problem = &files[0]["diagnostics"][0];
    assert_eq!(problem["severity"], json!("Error"));
    assert_eq!(problem["message"], json!("cannot resolve the name"));
    assert_eq!(problem["range"]["start"]["line"], json!(1));
    assert_eq!(problem["range"]["start"]["character"], json!(0));
}

/// Clicks the mark on `line` and waits for the window of its hunk.
fn open_hunk(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    view: &Entity<EditorView>,
    line: usize,
) {
    wait_for_signal(cx, "the mark on the line", |cx| {
        draw(cx, window);
        view.read_with(cx, |view, _| view.agent_mark_bounds(line).is_some())
    });
    let bounds = view
        .read_with(cx, |view, _| view.agent_mark_bounds(line))
        .unwrap();
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_click(bounds.center(), Modifiers::default());
    drop(visual);
    cx.run_until_parked();
    draw(cx, window);
    wait_for_signal(cx, "the window of the hunk", |cx| {
        draw(cx, window);
        has_selector(cx, window, "agent-hunk-popup")
    });
}

fn has_selector(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &'static str,
) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

#[gpui::test]
fn accept_all_writes_the_proposed_file_and_the_terminal_names_the_client(cx: &mut TestAppContext) {
    let (window, root) = open(cx, "claude-accept-all");
    let file = root.join("src").join("main.txt");
    fs::write(&file, BASE).unwrap();
    open_path(cx, window, &file);
    let view = editor_of(cx, window, &file);
    let bridge = bridge(cx);
    let lock = bridge.lock_path().to_path_buf();
    let path = file.clone();

    let answers = run(move || {
        let mut client = Client::connect(&lock)?;
        client.initialize()?;
        client.call(
            "openDiff",
            json!({
                "old_file_path": path,
                "new_file_path": path,
                "new_file_contents": CHANGED,
                "tab_name": "Claude Code test",
            }),
        )
    });
    wait_for_signal(cx, "the marks of the proposal", |cx| {
        draw(cx, window);
        view.read_with(cx, |view, _| view.agent_marks().len() == 2)
    });
    crate::window_tests_cached_views::show_tool(cx, window, workspace::TERMINAL_TOOL);
    draw(cx, window);
    wait_for_signal(cx, "the status of the terminal", |cx| {
        draw(cx, window);
        has_selector(cx, window, "terminal-ide-status")
    });

    click(cx, window, "agent-bar-accept-all");
    draw(cx, window);
    let content = wait_for_answer(cx, &answers);
    assert_eq!(content[0]["text"], json!("FILE_SAVED"));
    assert_eq!(content[1]["text"], json!(CHANGED));
    wait_for_signal(cx, "the proposed text", |_| {
        fs::read_to_string(&file).unwrap() == CHANGED
    });
    wait_for_signal(cx, "no more marks", |cx| {
        draw(cx, window);
        view.read_with(cx, |view, _| view.agent_marks().is_empty())
    });
}

#[gpui::test]
fn a_proposed_file_waits_for_the_user_and_keeps_what_was_accepted(cx: &mut TestAppContext) {
    let (window, root) = open(cx, "claude-open-diff");
    let file = root.join("src").join("main.txt");
    fs::write(&file, BASE).unwrap();
    open_path(cx, window, &file);
    let view = editor_of(cx, window, &file);
    let bridge = bridge(cx);
    let lock = bridge.lock_path().to_path_buf();
    let path = file.clone();

    let answers = run(move || {
        let mut client = Client::connect(&lock)?;
        client.initialize()?;
        client.call(
            "openDiff",
            json!({
                "old_file_path": path,
                "new_file_path": path,
                "new_file_contents": CHANGED,
                "tab_name": "Claude Code test",
            }),
        )
    });
    wait_for_signal(cx, "the marks of the proposal", |cx| {
        draw(cx, window);
        view.read_with(cx, |view, _| view.agent_marks().len() == 2)
    });
    assert!(
        bridge.connected(),
        "the terminal status says the harness is here"
    );

    open_hunk(cx, window, &view, 0);
    click(cx, window, "agent-hunk-accept");
    draw(cx, window);
    wait_for_signal(cx, "one hunk left", |cx| {
        draw(cx, window);
        view.read_with(cx, |view, _| view.agent_marks().len() == 1)
    });
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        BASE,
        "nothing is written while a hunk still waits"
    );

    open_hunk(cx, window, &view, 4);
    click(cx, window, "agent-hunk-reject");
    draw(cx, window);
    let content = wait_for_answer(cx, &answers);
    assert_eq!(content[0]["text"], json!("FILE_SAVED"));
    assert_eq!(content[1]["text"], json!(FIRST_ACCEPTED));
    wait_for_signal(cx, "the text the user kept", |_| {
        fs::read_to_string(&file).unwrap() == FIRST_ACCEPTED
    });
    wait_for_signal(cx, "the editor to read the file again", |cx| {
        draw(cx, window);
        crate::window_tests::buffer_of(cx, &view) == FIRST_ACCEPTED
    });
    wait_for_signal(cx, "no more marks", |cx| {
        draw(cx, window);
        view.read_with(cx, |view, _| view.agent_marks().is_empty())
    });
}
