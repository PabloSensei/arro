//! One ACP session: `initialize`, `session/new`, then one `session/prompt` per turn. Prompts sent
//! during a turn wait in the inbox, which is the message queue of the panel.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use agent_core::{
    AgentError, AgentEvent, AgentRuntime, AgentSession, PermissionReply, PromptPart, Role,
    RuntimeCaps, SessionSpec, SessionStatus, Surface, SurfaceKind, ToolStatus,
};
use crossbeam_channel::{Receiver, Sender, bounded, never, select, unbounded};
use proc_sup::{Process, ProcessSpec};
use serde_json::{Value, json};

use crate::connection::{Connection, Incoming, Response};
use crate::preset::AcpCommand;

/// A cold `npx` downloads the adapter first, S7 saw 18 s.
const HANDSHAKE_WAIT: Duration = Duration::from_secs(300);
const STDERR_LINES: usize = 20;
const QUEUE: usize = 64;
/// ACP v1, the only version adapters speak today.
const PROTOCOL_VERSION: u64 = 1;
const MCP_SERVER_NAME: &str = "javaide";

pub struct AcpRuntime {
    name: String,
    command: AcpCommand,
}

impl AcpRuntime {
    pub fn new(name: impl Into<String>, command: AcpCommand) -> Self {
        Self {
            name: name.into(),
            command,
        }
    }

    fn process(&self, spec: &SessionSpec) -> std::io::Result<Process> {
        let command = &self.command;
        let mut process = ProcessSpec::new(&command.program)
            .args(&command.args)
            .cwd(&spec.cwd);
        if let Some(env) = &command.env {
            // Exactly this environment: nothing of the parent leaks into a prepared one.
            for (key, _) in std::env::vars_os() {
                process = process.env_remove(key);
            }
            for (key, value) in env {
                process = process.env(key, value);
            }
        }
        process.start()
    }
}

impl AgentRuntime for AcpRuntime {
    fn name(&self) -> &str {
        &self.name
    }

    fn caps(&self) -> RuntimeCaps {
        RuntimeCaps {
            streaming: true,
            permissions: true,
            resume: false,
            modes: false,
            surface: SurfaceKind::Transcript,
            prompts: true,
        }
    }

    fn start(&self, spec: SessionSpec) -> Result<Box<dyn AgentSession>, AgentError> {
        let mut process = self.process(&spec)?;
        let stdin = process.take_stdin().ok_or(AgentError::Closed)?;
        let stdout = process.take_stdout().ok_or(AgentError::Closed)?;
        let stderr = process.take_stderr();
        let (connection, incoming) = Connection::start(stdin, stdout)?;
        let errors = Arc::new(Mutex::new(Vec::new()));
        if let Some(stderr) = stderr {
            let lines = errors.clone();
            thread::Builder::new()
                .name("acp-stderr".to_string())
                .spawn(move || keep_tail(stderr, &lines))?;
        }
        let process = Arc::new(Mutex::new(process));
        let (events, stream) = unbounded();
        let (prompts, inbox) = bounded(QUEUE);
        let (cancels, cancel_inbox) = unbounded();
        let shared = Arc::new(Shared {
            connection,
            permissions: Mutex::new(HashMap::new()),
            session_id: Mutex::new(None),
        });
        let worker = Worker {
            shared: shared.clone(),
            spec,
            events,
            process: process.clone(),
            errors,
            embedded_context: false,
        };
        thread::Builder::new()
            .name("acp-session".to_string())
            .spawn(move || worker.run(incoming, inbox, cancel_inbox))?;
        Ok(Box::new(AcpSession {
            shared,
            stream,
            prompts: Mutex::new(Some(prompts)),
            cancels,
            process,
        }))
    }
}

fn keep_tail(stderr: impl std::io::Read, lines: &Mutex<Vec<String>>) {
    for line in BufReader::new(stderr).lines().map_while(Result::ok) {
        let mut lines = lines.lock().unwrap();
        lines.push(line);
        if lines.len() > STDERR_LINES {
            lines.remove(0);
        }
    }
}

/// A permission question the agent waits on: its JSON-RPC id and the options it offered.
struct Pending {
    id: Value,
    options: Vec<(String, String)>,
}

/// What the UI thread and the worker both touch.
struct Shared {
    connection: Arc<Connection>,
    permissions: Mutex<HashMap<String, Pending>>,
    session_id: Mutex<Option<String>>,
}

impl Shared {
    fn answer(&self, key: &str, reply: Option<PermissionReply>) -> Result<(), AgentError> {
        let pending = self.permissions.lock().unwrap().remove(key);
        let Some(pending) = pending else {
            return Err(AgentError::Io(format!("no question {key} is open")));
        };
        let wanted: &[&str] = match reply {
            Some(PermissionReply::AllowOnce) => &["allow_once", "allow_always"],
            Some(PermissionReply::AllowAlways) => &["allow_always", "allow_once"],
            Some(PermissionReply::Deny) => &["reject_once", "reject_always"],
            None => &[],
        };
        let option = wanted.iter().find_map(|kind| {
            let found = pending.options.iter().find(|(_, known)| known == kind);
            found.map(|(id, _)| id.clone())
        });
        let outcome = match option {
            Some(option) => json!({"outcome": "selected", "optionId": option}),
            None => json!({"outcome": "cancelled"}),
        };
        self.connection
            .reply(&pending.id, json!({"outcome": outcome}));
        Ok(())
    }

    fn cancel_questions(&self) {
        let keys: Vec<String> = self.permissions.lock().unwrap().keys().cloned().collect();
        for key in keys {
            let _ = self.answer(&key, None);
        }
    }
}

struct AcpSession {
    shared: Arc<Shared>,
    stream: Receiver<AgentEvent>,
    prompts: Mutex<Option<Sender<Vec<PromptPart>>>>,
    cancels: Sender<()>,
    process: Arc<Mutex<Process>>,
}

impl AgentSession for AcpSession {
    fn surface(&self) -> Surface {
        Surface::Transcript
    }

    fn events(&self) -> Receiver<AgentEvent> {
        self.stream.clone()
    }

    fn prompt(&self, text: &str) -> Result<(), AgentError> {
        self.prompt_parts(&[PromptPart::Text(text.to_string())])
    }

    fn prompt_parts(&self, parts: &[PromptPart]) -> Result<(), AgentError> {
        let prompts = self.prompts.lock().unwrap();
        let prompts = prompts.as_ref().ok_or(AgentError::Closed)?;
        prompts
            .try_send(parts.to_vec())
            .map_err(|_| AgentError::Busy)
    }

    fn cancel(&self) -> Result<(), AgentError> {
        self.shared.cancel_questions();
        self.cancels.send(()).map_err(|_| AgentError::Closed)
    }

    fn respond(&self, request: &str, reply: PermissionReply) -> Result<(), AgentError> {
        self.shared.answer(request, Some(reply))
    }

    fn close(&self) {
        self.prompts.lock().unwrap().take();
        let _ = self.process.lock().unwrap().stop();
    }
}

struct Worker {
    shared: Arc<Shared>,
    spec: SessionSpec,
    events: Sender<AgentEvent>,
    process: Arc<Mutex<Process>>,
    errors: Arc<Mutex<Vec<String>>>,
    /// The agent takes file contents inside a prompt.
    embedded_context: bool,
}

fn text_of(content: &Value) -> Option<String> {
    match content["type"].as_str()? {
        "text" => content["text"].as_str().map(str::to_string),
        "content" => text_of(&content["content"]),
        "diff" => {
            let path = content["path"].as_str().unwrap_or_default();
            Some(format!("edited {path}"))
        }
        _ => None,
    }
}

/// The text of a tool result, from `content` blocks or else the raw output.
fn output_of(update: &Value) -> Option<String> {
    let blocks = update["content"].as_array();
    let texts: Vec<String> = blocks.into_iter().flatten().filter_map(text_of).collect();
    if !texts.is_empty() {
        return Some(texts.join("\n"));
    }
    let raw = update.get("rawOutput")?;
    Some(match raw {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    })
}

fn tool_status(status: &str) -> Option<ToolStatus> {
    Some(match status {
        "pending" => ToolStatus::Pending,
        "in_progress" => ToolStatus::Running,
        "completed" => ToolStatus::Done,
        "failed" => ToolStatus::Failed,
        _ => return None,
    })
}

/// What a permission card shows: the command when there is one, so the user sees what runs.
fn description_of(call: &Value) -> String {
    let input = &call["rawInput"];
    let command = ["cmd", "command"]
        .iter()
        .find_map(|key| match &input[*key] {
            Value::String(text) => Some(text.clone()),
            Value::Array(parts) => Some(
                parts
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" "),
            ),
            _ => None,
        });
    command
        .or_else(|| call["title"].as_str().map(str::to_string))
        .unwrap_or_else(|| input.to_string())
}

impl Worker {
    fn emit(&self, event: AgentEvent) {
        let _ = self.events.send(event);
    }

    fn fail(&self, message: String) {
        let tail = self.errors.lock().unwrap().join("\n");
        let text = if tail.is_empty() {
            message
        } else {
            format!("{message}\n{tail}")
        };
        self.emit(AgentEvent::Error(text));
        self.emit(AgentEvent::Status(SessionStatus::Error));
    }

    fn wait(&self, answer: &Receiver<Response>, incoming: &Receiver<Incoming>) -> Response {
        let deadline = crossbeam_channel::after(HANDSHAKE_WAIT);
        loop {
            select! {
                recv(answer) -> response => {
                    return response.unwrap_or_else(|_| Err("the agent exited".to_string()));
                }
                recv(incoming) -> message => match message {
                    Ok(Incoming::Closed) | Err(_) => return Err("the agent exited".to_string()),
                    Ok(message) => self.handle(message),
                },
                recv(deadline) -> _ => return Err("the agent did not answer in time".to_string()),
            }
        }
    }

    fn handshake(&mut self, incoming: &Receiver<Incoming>) -> Result<String, String> {
        let capabilities = json!({
            "fs": {"readTextFile": false, "writeTextFile": false},
            "terminal": false,
        });
        let params = json!({
            "protocolVersion": PROTOCOL_VERSION,
            "clientCapabilities": capabilities,
            "clientInfo": {"name": "javaide", "version": env!("CARGO_PKG_VERSION")},
        });
        let answer = self.shared.connection.request("initialize", params);
        let init = self.wait(&answer, incoming)?;
        let prompts = &init["agentCapabilities"]["promptCapabilities"];
        self.embedded_context = prompts["embeddedContext"] == json!(true);
        let servers = match &self.spec.tools {
            Some(tools) => json!([{
                "type": "http",
                "name": MCP_SERVER_NAME,
                "url": tools.url,
                "headers": [{"name": "Authorization", "value": format!("Bearer {}", tools.token)}],
            }]),
            None => json!([]),
        };
        let params = json!({"cwd": self.spec.cwd, "mcpServers": servers});
        let answer = self.shared.connection.request("session/new", params);
        let session = self.wait(&answer, incoming)?;
        let id = session["sessionId"]
            .as_str()
            .ok_or("session/new gave no id")?;
        Ok(id.to_string())
    }

    fn run(
        mut self,
        incoming: Receiver<Incoming>,
        inbox: Receiver<Vec<PromptPart>>,
        cancels: Receiver<()>,
    ) {
        self.emit(AgentEvent::Status(SessionStatus::Starting));
        let session = match self.handshake(&incoming) {
            Ok(session) => session,
            Err(error) => {
                self.fail(format!("The agent did not start: {error}"));
                self.finish();
                return;
            }
        };
        *self.shared.session_id.lock().unwrap() = Some(session.clone());
        self.emit(AgentEvent::Status(SessionStatus::WaitingForUser));
        let mut turn: Option<Receiver<Response>> = None;
        loop {
            let idle_inbox = if turn.is_none() {
                inbox.clone()
            } else {
                never()
            };
            let turn_answer = turn.clone().unwrap_or_else(never);
            select! {
                recv(idle_inbox) -> parts => match parts {
                    Ok(parts) => turn = Some(self.start_turn(&session, &parts)),
                    Err(_) => break,
                },
                recv(turn_answer) -> response => {
                    turn = None;
                    self.end_turn(response.unwrap_or_else(|_| Err("the agent exited".to_string())));
                }
                recv(cancels) -> _ => {
                    if turn.is_some() {
                        self.shared.connection.notify("session/cancel", json!({"sessionId": session}));
                    }
                }
                recv(incoming) -> message => match message {
                    Ok(Incoming::Closed) | Err(_) => break,
                    Ok(message) => self.handle(message),
                },
            }
        }
        self.finish();
    }

    fn content(&self, parts: &[PromptPart]) -> Vec<Value> {
        let uri = |path: &std::path::Path| {
            let text = path.to_string_lossy().replace('\\', "/");
            format!("file:///{}", text.trim_start_matches('/'))
        };
        let name = |path: &std::path::Path| {
            let name = path.file_name().unwrap_or_default();
            name.to_string_lossy().into_owned()
        };
        parts
            .iter()
            .map(|part| match part {
                PromptPart::Text(text) => json!({"type": "text", "text": text}),
                PromptPart::File(path) => json!({
                    "type": "resource_link", "uri": uri(path), "name": name(path),
                }),
                PromptPart::Snippet { path, .. } if self.embedded_context => {
                    let text = part.as_text();
                    json!({"type": "resource", "resource": {
                        "uri": uri(path), "text": text, "mimeType": "text/plain",
                    }})
                }
                PromptPart::Snippet { .. } => json!({"type": "text", "text": part.as_text()}),
            })
            .collect()
    }

    fn start_turn(&self, session: &str, parts: &[PromptPart]) -> Receiver<Response> {
        let text: String = parts.iter().map(PromptPart::as_text).collect();
        self.emit(AgentEvent::Message {
            role: Role::User,
            text,
        });
        self.emit(AgentEvent::TurnStarted);
        self.emit(AgentEvent::Status(SessionStatus::Running));
        let params = json!({"sessionId": session, "prompt": self.content(parts)});
        self.shared.connection.request("session/prompt", params)
    }

    fn end_turn(&self, response: Response) {
        self.shared.cancel_questions();
        match response {
            Ok(result) => {
                let reason = result["stopReason"].as_str().unwrap_or("end_turn");
                if reason != "end_turn" {
                    self.emit(AgentEvent::Notification(format!(
                        "The turn ended: {reason}"
                    )));
                }
                self.emit(AgentEvent::TurnEnded);
                self.emit(AgentEvent::Status(SessionStatus::WaitingForUser));
            }
            Err(error) => {
                self.emit(AgentEvent::TurnEnded);
                self.fail(format!("The turn failed: {error}"));
            }
        }
    }

    fn handle(&self, message: Incoming) {
        match message {
            Incoming::Notification { method, params } if method == "session/update" => {
                self.update(&params["update"]);
            }
            Incoming::Notification { .. } | Incoming::Closed => {}
            Incoming::Request { id, method, params } => match method.as_str() {
                "session/request_permission" => self.ask(id, &params),
                // No fs or terminal capability was offered: the agent works on disk itself.
                _ => self.shared.connection.reply_error(
                    &id,
                    -32601,
                    "the IDE does not offer this method",
                ),
            },
        }
    }

    fn update(&self, update: &Value) {
        let id = || {
            update["toolCallId"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        };
        match update["sessionUpdate"].as_str().unwrap_or_default() {
            "agent_message_chunk" => {
                if let Some(text) = text_of(&update["content"]) {
                    self.emit(AgentEvent::TextDelta(text));
                }
            }
            "tool_call" => {
                let name = update["title"].as_str().unwrap_or("tool").to_string();
                let input = update.get("rawInput").cloned().unwrap_or(Value::Null);
                self.emit(AgentEvent::ToolCall {
                    id: id(),
                    name,
                    input,
                });
                if let Some(status) = update["status"].as_str().and_then(tool_status) {
                    let output = output_of(update);
                    self.emit(AgentEvent::ToolStatus {
                        id: id(),
                        status,
                        output,
                    });
                }
            }
            "tool_call_update" => {
                let status = update["status"].as_str().and_then(tool_status);
                let output = output_of(update);
                if status.is_some() || output.is_some() {
                    self.emit(AgentEvent::ToolStatus {
                        id: id(),
                        status: status.unwrap_or(ToolStatus::Running),
                        output,
                    });
                }
            }
            "session_info_update" => {
                if let Some(title) = update["title"].as_str() {
                    self.emit(AgentEvent::Title(title.to_string()));
                }
            }
            _ => {}
        }
    }

    fn ask(&self, id: Value, params: &Value) {
        let call = &params["toolCall"];
        let key = match call["toolCallId"].as_str() {
            Some(call) => format!("permission-{call}"),
            None => format!("permission-{id}"),
        };
        let options = params["options"].as_array().into_iter().flatten();
        let options = options
            .filter_map(|option| {
                let id = option["optionId"].as_str()?;
                Some((id.to_string(), option["kind"].as_str()?.to_string()))
            })
            .collect();
        self.shared
            .permissions
            .lock()
            .unwrap()
            .insert(key.clone(), Pending { id, options });
        self.emit(AgentEvent::Status(SessionStatus::WaitingForUser));
        self.emit(AgentEvent::PermissionRequest {
            id: key,
            tool: call["title"].as_str().unwrap_or("tool").to_string(),
            description: description_of(call),
        });
    }

    fn finish(&self) {
        self.shared.cancel_questions();
        let mut process = self.process.lock().unwrap();
        let code = process
            .try_wait()
            .ok()
            .flatten()
            .and_then(|status| status.code())
            .map(|code| code as u32);
        let _ = process.stop();
        drop(process);
        self.emit(AgentEvent::Exited(code));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_permission_card_shows_the_command() {
        let call = json!({"title": "Run", "rawInput": {"command": ["git", "push"]}});
        assert_eq!(description_of(&call), "git push");
        let call = json!({"title": "Edit a.txt", "rawInput": {"path": "a.txt"}});
        assert_eq!(description_of(&call), "Edit a.txt");
    }

    #[test]
    fn tool_output_comes_from_content_or_raw_output() {
        let update =
            json!({"content": [{"type": "content", "content": {"type": "text", "text": "ok"}}]});
        assert_eq!(output_of(&update).as_deref(), Some("ok"));
        let update = json!({"rawOutput": {"exit": 0}});
        assert_eq!(output_of(&update).as_deref(), Some("{\"exit\":0}"));
        assert_eq!(output_of(&json!({})), None);
    }
}
