//! Local stand-in for the Anthropic and OpenAI model APIs. The prompt scripts the reply:
//! `MOCK_TOOL <name> <json>` asks for a tool call, `MOCK_WRITE <path> <text>` asks the harness to
//! write a file, anything else gets a short text.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use serde_json::{Value, json};

pub const FAKE_TOKEN: &str = "mock-token-not-a-credential";

/// Model name in the throwaway Codex config. Seeing it on screen proves the config is in effect.
pub const MOCK_MODEL: &str = "mock-model";

#[derive(Default)]
struct MockState {
    tool_results: Mutex<Vec<String>>,
    user_texts: Mutex<Vec<String>>,
    counter: AtomicU64,
}

pub struct MockApi {
    port: u16,
    state: Arc<MockState>,
}

impl MockApi {
    pub fn start() -> io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        let state = Arc::new(MockState::default());
        let shared = state.clone();
        thread::Builder::new()
            .name("mock-api".to_string())
            .spawn(move || {
                for stream in listener.incoming().flatten() {
                    let state = shared.clone();
                    thread::spawn(move || drop(serve(stream, &state)));
                }
            })?;
        Ok(Self { port, state })
    }

    pub fn anthropic_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    pub fn openai_url(&self) -> String {
        format!("http://127.0.0.1:{}/v1", self.port)
    }

    /// Text of every user turn the model would have seen.
    pub fn user_texts(&self) -> Vec<String> {
        self.state.user_texts.lock().unwrap().clone()
    }

    /// Tool outputs the model would have seen.
    pub fn tool_results(&self) -> Vec<String> {
        self.state.tool_results.lock().unwrap().clone()
    }
}

struct Request {
    method: String,
    path: String,
    body: Vec<u8>,
}

fn read_request(reader: &mut BufReader<TcpStream>) -> io::Result<Option<Request>> {
    let mut first = String::new();
    if reader.read_line(&mut first)? == 0 {
        return Ok(None);
    }
    let mut parts = first.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let path = parts.next().unwrap_or_default().to_string();
    let (mut length, mut chunked) = (0usize, false);
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 || line == "\r\n" {
            break;
        }
        let lower = line.to_ascii_lowercase();
        if let Some(value) = lower.strip_prefix("content-length:") {
            length = value.trim().parse().unwrap_or(0);
        }
        if lower.starts_with("transfer-encoding:") && lower.contains("chunked") {
            chunked = true;
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    if chunked {
        loop {
            let mut size = String::new();
            reader.read_line(&mut size)?;
            let size = usize::from_str_radix(size.trim(), 16).unwrap_or(0);
            let mut chunk = vec![0; size + 2];
            reader.read_exact(&mut chunk)?;
            if size == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..size]);
        }
    }
    Ok(Some(Request { method, path, body }))
}

fn serve(stream: TcpStream, state: &MockState) -> io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut stream = stream;
    while let Some(request) = read_request(&mut reader)? {
        let body: Value = serde_json::from_slice(&request.body).unwrap_or(Value::Null);
        let route = request.path.split('?').next().unwrap_or_default();
        match (request.method.as_str(), route) {
            ("POST", "/v1/messages") => anthropic(&mut stream, state, &body)?,
            ("POST", "/v1/messages/count_tokens") => {
                respond(&mut stream, "application/json", r#"{"input_tokens":10}"#)?
            }
            ("POST", "/v1/responses") => openai(&mut stream, state, &body)?,
            ("GET", "/v1/models") => respond(
                &mut stream,
                "application/json",
                r#"{"object":"list","data":[],"models":[]}"#,
            )?,
            _ => write!(
                stream,
                "HTTP/1.1 404 Not Found\r\nContent-Length: 2\r\n\
                 Content-Type: application/json\r\n\r\n{{}}"
            )?,
        }
        stream.flush()?;
    }
    Ok(())
}

fn respond(stream: &mut TcpStream, content_type: &str, body: &str) -> io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
}

fn sse(stream: &mut TcpStream, events: &[(&str, Value)]) -> io::Result<()> {
    let mut body = String::new();
    for (name, data) in events {
        body.push_str(&format!("event: {name}\ndata: {data}\n\n"));
    }
    respond(stream, "text/event-stream", &body)
}

enum Reply {
    Text(String),
    Tool { name: String, input: Value },
}

/// The scripted command must be the last one: earlier history may quote old commands.
fn parse_script(text: &str) -> Reply {
    if let Some(start) = text.rfind("MOCK_TOOL ") {
        let rest = &text[start + "MOCK_TOOL ".len()..];
        let name_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let name = rest[..name_end].to_string();
        let mut values =
            serde_json::Deserializer::from_str(rest[name_end..].trim_start()).into_iter::<Value>();
        let input = values.next().and_then(Result::ok).unwrap_or(json!({}));
        return Reply::Tool { name, input };
    }
    Reply::Text("MOCK_REPLY lorem ipsum".to_string())
}

fn collect_text(value: &Value, out: &mut String) {
    match value {
        Value::String(text) => {
            out.push_str(text);
            out.push('\n');
        }
        Value::Array(items) => items.iter().for_each(|item| collect_text(item, out)),
        Value::Object(map) => {
            for key in ["text", "content"] {
                if let Some(item) = map.get(key) {
                    collect_text(item, out);
                }
            }
        }
        _ => {}
    }
}

fn anthropic(stream: &mut TcpStream, state: &MockState, body: &Value) -> io::Result<()> {
    // Claude Code appends a `system` role message after the prompt or tool result.
    let messages = body["messages"].as_array().cloned().unwrap_or_default();
    let tail: Vec<&Value> = messages
        .iter()
        .rev()
        .take_while(|item| item["role"] != "assistant")
        .collect();
    let results: Vec<&Value> = tail
        .iter()
        .filter_map(|message| message["content"].as_array())
        .flatten()
        .filter(|item| item["type"] == "tool_result")
        .collect();
    let reply = if results.is_empty() {
        let mut text = String::new();
        tail.iter()
            .rev()
            .for_each(|message| collect_text(&message["content"], &mut text));
        state.user_texts.lock().unwrap().push(text.clone());
        parse_script(&text)
    } else {
        let mut text = String::new();
        results
            .iter()
            .for_each(|item| collect_text(&item["content"], &mut text));
        state.tool_results.lock().unwrap().push(text.clone());
        Reply::Text(format!("MOCK_DONE {}", text.trim()))
    };

    let id = state.counter.fetch_add(1, Ordering::Relaxed);
    let usage = json!({"input_tokens": 10, "output_tokens": 5});
    let tool_id = format!("toolu_mock_{id}");
    let (block, delta, stop) = match &reply {
        Reply::Text(text) => (
            json!({"type": "text", "text": ""}),
            json!({"type": "text_delta", "text": text}),
            "end_turn",
        ),
        Reply::Tool { name, input } => (
            json!({"type": "tool_use", "id": tool_id, "name": name, "input": {}}),
            json!({"type": "input_json_delta", "partial_json": input.to_string()}),
            "tool_use",
        ),
    };
    if body["stream"] != json!(true) {
        let content = match &reply {
            Reply::Text(text) => json!([{"type": "text", "text": text}]),
            Reply::Tool { name, input } => json!([{
                "type": "tool_use", "id": tool_id, "name": name, "input": input
            }]),
        };
        let message = json!({
            "id": format!("msg_mock_{id}"), "type": "message", "role": "assistant",
            "model": "claude-mock", "content": content, "stop_reason": stop,
            "stop_sequence": null, "usage": usage,
        });
        return respond(stream, "application/json", &message.to_string());
    }
    let message_start = json!({"type": "message_start", "message": {
        "id": format!("msg_mock_{id}"), "type": "message", "role": "assistant",
        "model": "claude-mock", "content": [], "stop_reason": null,
        "stop_sequence": null, "usage": usage,
    }});
    let block_start = json!({"type": "content_block_start", "index": 0, "content_block": block});
    let block_delta = json!({"type": "content_block_delta", "index": 0, "delta": delta});
    let message_delta = json!({
        "type": "message_delta",
        "delta": {"stop_reason": stop, "stop_sequence": null},
        "usage": {"output_tokens": 5},
    });
    sse(
        stream,
        &[
            ("message_start", message_start),
            ("content_block_start", block_start),
            ("content_block_delta", block_delta),
            (
                "content_block_stop",
                json!({"type": "content_block_stop", "index": 0}),
            ),
            ("message_delta", message_delta),
            ("message_stop", json!({"type": "message_stop"})),
        ],
    )
}

fn openai(stream: &mut TcpStream, state: &MockState, body: &Value) -> io::Result<()> {
    let input = body["input"].as_array().cloned().unwrap_or_default();
    let last = input.last().cloned().unwrap_or(Value::Null);
    // Codex asks a reviewer model before MCP calls, and what it sends quotes the script.
    let reply = if body["model"] == "codex-auto-review" {
        let verdict = json!({"risk_level": "low", "outcome": "allow", "rationale": "mock"});
        Reply::Text(verdict.to_string())
    } else if last["type"] == "function_call_output" || last["type"] == "mcp_tool_call_output" {
        let text = last["output"].to_string();
        state.tool_results.lock().unwrap().push(text.clone());
        Reply::Text(format!("MOCK_DONE {text}"))
    } else {
        let mut text = String::new();
        collect_text(&last["content"], &mut text);
        state.user_texts.lock().unwrap().push(text.clone());
        parse_script(&text)
    };

    let id = state.counter.fetch_add(1, Ordering::Relaxed);
    let response_id = format!("resp_mock_{id}");
    let item = match &reply {
        Reply::Text(text) => json!({
            "type": "message", "id": format!("msg_mock_{id}"), "role": "assistant",
            "status": "completed",
            "content": [{"type": "output_text", "text": text, "annotations": []}],
        }),
        Reply::Tool { name, input } => {
            let mut call = json!({
                "type": "function_call", "id": format!("fc_mock_{id}"),
                "call_id": format!("call_mock_{id}"),
                "name": name, "arguments": input.to_string(), "status": "completed",
            });
            // Codex groups MCP tools: `mcp__ide__echo` is `echo` in namespace `mcp__ide`.
            let split = name.rsplit_once("__");
            if let Some((namespace, tool)) = split.filter(|(left, _)| left.starts_with("mcp__")) {
                call["namespace"] = json!(namespace);
                call["name"] = json!(tool);
            }
            call
        }
    };
    let usage = json!({
        "input_tokens": 10, "input_tokens_details": {"cached_tokens": 0},
        "output_tokens": 5, "output_tokens_details": {"reasoning_tokens": 0},
        "total_tokens": 15,
    });
    sse(
        stream,
        &[
            (
                "response.created",
                json!({"type": "response.created", "response": {"id": response_id}}),
            ),
            (
                "response.output_item.done",
                json!({"type": "response.output_item.done", "output_index": 0, "item": item}),
            ),
            (
                "response.completed",
                json!({
                    "type": "response.completed",
                    "response": {"id": response_id, "status": "completed", "usage": usage},
                }),
            ),
        ],
    )
}
