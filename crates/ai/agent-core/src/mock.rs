//! A scripted agent for UI and flow tests: no network, no process, no money.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, bounded, unbounded};
use serde_json::Value;

use crate::runtime::{
    AgentError, AgentEvent, AgentRuntime, AgentSession, PermissionReply, Role, RuntimeCaps,
    SessionSpec, SessionStatus, Surface, SurfaceKind,
};

/// Calls a tool the way the MCP server would.
pub type ToolInvoker = Arc<dyn Fn(&str, &Value) -> Result<String, String> + Send + Sync>;

/// One thing the agent does in a turn. A turn ends with the script of its prompt.
#[derive(Clone, Debug)]
pub enum MockStep {
    Reply(String),
    Tool {
        name: String,
        input: Value,
    },
    /// Stops until `respond`. A denial ends the turn.
    Permission {
        tool: String,
        description: String,
    },
    /// Runs until `cancel`.
    Hang,
    Fail(String),
}

/// `turns` are consumed one per prompt, a prompt past the end gets a short reply.
#[derive(Default)]
pub struct MockRuntime {
    turns: Mutex<VecDeque<Vec<MockStep>>>,
    tools: Option<ToolInvoker>,
}

impl MockRuntime {
    pub fn new(turns: Vec<Vec<MockStep>>) -> Self {
        Self {
            turns: Mutex::new(turns.into()),
            tools: None,
        }
    }

    pub fn with_tools(mut self, tools: ToolInvoker) -> Self {
        self.tools = Some(tools);
        self
    }
}

impl AgentRuntime for MockRuntime {
    fn name(&self) -> &str {
        "mock"
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

    fn start(&self, _spec: SessionSpec) -> Result<Box<dyn AgentSession>, AgentError> {
        let turns = std::mem::take(&mut *self.turns.lock().unwrap());
        let (events, stream) = unbounded();
        let (prompts, inbox) = bounded(64);
        let (replies, answers) = bounded(8);
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker = Worker {
            turns,
            tools: self.tools.clone(),
            events,
            answers,
            cancelled: cancelled.clone(),
        };
        thread::Builder::new()
            .name("mock-agent".to_string())
            .spawn(move || worker.run(inbox))?;
        Ok(Box::new(MockSession {
            stream,
            prompts: Mutex::new(Some(prompts)),
            replies,
            cancelled,
        }))
    }
}

struct MockSession {
    stream: Receiver<AgentEvent>,
    prompts: Mutex<Option<Sender<String>>>,
    replies: Sender<PermissionReply>,
    cancelled: Arc<AtomicBool>,
}

impl AgentSession for MockSession {
    fn surface(&self) -> Surface {
        Surface::Transcript
    }

    fn events(&self) -> Receiver<AgentEvent> {
        self.stream.clone()
    }

    fn prompt(&self, text: &str) -> Result<(), AgentError> {
        let prompts = self.prompts.lock().unwrap();
        let prompts = prompts.as_ref().ok_or(AgentError::Closed)?;
        prompts
            .try_send(text.to_string())
            .map_err(|_| AgentError::Busy)
    }

    fn cancel(&self) -> Result<(), AgentError> {
        self.cancelled.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn respond(&self, _request: &str, reply: PermissionReply) -> Result<(), AgentError> {
        self.replies.try_send(reply).map_err(|_| AgentError::Busy)
    }

    fn close(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.prompts.lock().unwrap().take();
    }
}

struct Worker {
    turns: VecDeque<Vec<MockStep>>,
    tools: Option<ToolInvoker>,
    events: Sender<AgentEvent>,
    answers: Receiver<PermissionReply>,
    cancelled: Arc<AtomicBool>,
}

const POLL: Duration = Duration::from_millis(5);

impl Worker {
    fn emit(&self, event: AgentEvent) {
        let _ = self.events.send(event);
    }

    fn run(mut self, inbox: Receiver<String>) {
        self.emit(AgentEvent::Status(SessionStatus::WaitingForUser));
        for prompt in inbox {
            self.cancelled.store(false, Ordering::SeqCst);
            self.emit(AgentEvent::Message {
                role: Role::User,
                text: prompt,
            });
            self.emit(AgentEvent::TurnStarted);
            self.emit(AgentEvent::Status(SessionStatus::Running));
            let default = vec![MockStep::Reply("mock reply".to_string())];
            let steps = self.turns.pop_front().unwrap_or(default);
            let finished = self.play(steps);
            self.emit(AgentEvent::TurnEnded);
            self.emit(AgentEvent::Status(finished));
        }
        self.emit(AgentEvent::Exited(Some(0)));
    }

    fn play(&self, steps: Vec<MockStep>) -> SessionStatus {
        for (index, step) in steps.into_iter().enumerate() {
            if self.cancelled.load(Ordering::SeqCst) {
                return SessionStatus::Done;
            }
            match step {
                MockStep::Reply(text) => {
                    for word in text.split_inclusive(' ') {
                        self.emit(AgentEvent::TextDelta(word.to_string()));
                    }
                    self.emit(AgentEvent::Message {
                        role: Role::Assistant,
                        text,
                    });
                }
                MockStep::Tool { name, input } => self.tool(index, name, input),
                MockStep::Permission { tool, description } => {
                    if !self.ask(index, tool, description) {
                        return SessionStatus::Done;
                    }
                }
                MockStep::Hang => {
                    while !self.cancelled.load(Ordering::SeqCst) {
                        thread::sleep(POLL);
                    }
                }
                MockStep::Fail(message) => {
                    self.emit(AgentEvent::Error(message));
                    return SessionStatus::Error;
                }
            }
        }
        SessionStatus::Done
    }

    fn tool(&self, index: usize, name: String, input: Value) {
        let id = format!("call-{index}");
        let output = match &self.tools {
            Some(tools) => tools(&name, &input).unwrap_or_else(|error| format!("error: {error}")),
            None => "error: no tools".to_string(),
        };
        self.emit(AgentEvent::ToolCall {
            id: id.clone(),
            name,
            input,
        });
        self.emit(AgentEvent::ToolResult { id, output });
    }

    fn ask(&self, index: usize, tool: String, description: String) -> bool {
        self.emit(AgentEvent::Status(SessionStatus::WaitingForUser));
        self.emit(AgentEvent::PermissionRequest {
            id: format!("permission-{index}"),
            tool,
            description,
        });
        loop {
            if let Ok(reply) = self.answers.recv_timeout(POLL) {
                self.emit(AgentEvent::Status(SessionStatus::Running));
                return reply != PermissionReply::Deny;
            }
            if self.cancelled.load(Ordering::SeqCst) {
                return false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use serde_json::json;

    use super::*;

    const WAIT: Duration = Duration::from_secs(5);

    fn spec() -> SessionSpec {
        SessionSpec {
            project: "project".into(),
            cwd: "project".into(),
            cols: 80,
            rows: 24,
            tools: None,
        }
    }

    /// Reads until `done` says stop, and returns everything read.
    fn collect(
        stream: &Receiver<AgentEvent>,
        done: impl Fn(&AgentEvent) -> bool,
    ) -> Vec<AgentEvent> {
        let deadline = Instant::now() + WAIT;
        let mut seen = Vec::new();
        while Instant::now() < deadline {
            let Ok(event) = stream.recv_timeout(Duration::from_millis(50)) else {
                continue;
            };
            let stop = done(&event);
            seen.push(event);
            if stop {
                return seen;
            }
        }
        panic!("no end in {seen:?}");
    }

    fn turn_ended(event: &AgentEvent) -> bool {
        *event == AgentEvent::TurnEnded
    }

    #[test]
    fn a_prompt_streams_a_reply_and_ends_the_turn() {
        let runtime = MockRuntime::new(vec![vec![MockStep::Reply("hello there".to_string())]]);
        let session = runtime.start(spec()).expect("start");

        session.prompt("hi").expect("prompt");

        let events = collect(&session.events(), turn_ended);
        assert!(events.contains(&AgentEvent::TextDelta("hello ".to_string())));
        assert!(events.contains(&AgentEvent::Status(SessionStatus::Running)));
        assert_eq!(session.surface().kind(), SurfaceKind::Transcript);
    }

    #[test]
    fn tool_calls_go_through_the_invoker() {
        let tools: ToolInvoker = Arc::new(|name, _| Ok(format!("called {name}")));
        let step = MockStep::Tool {
            name: "find_symbol".to_string(),
            input: json!({"query": "Foo"}),
        };
        let runtime = MockRuntime::new(vec![vec![step]]).with_tools(tools);
        let session = runtime.start(spec()).expect("start");

        session.prompt("find").expect("prompt");

        let events = collect(&session.events(), turn_ended);
        let result = AgentEvent::ToolResult {
            id: "call-0".to_string(),
            output: "called find_symbol".to_string(),
        };
        assert!(events.contains(&result));
    }

    #[test]
    fn a_denied_permission_ends_the_turn_early() {
        let steps = vec![
            MockStep::Permission {
                tool: "rm".to_string(),
                description: "remove a file".to_string(),
            },
            MockStep::Reply("must not appear".to_string()),
        ];
        let runtime = MockRuntime::new(vec![steps]);
        let session = runtime.start(spec()).expect("start");
        let stream = session.events();

        session.prompt("go").expect("prompt");
        collect(&stream, |event| {
            matches!(event, AgentEvent::PermissionRequest { .. })
        });
        session
            .respond("permission-0", PermissionReply::Deny)
            .expect("respond");

        let rest = collect(&stream, turn_ended);
        let replied = rest
            .iter()
            .any(|event| matches!(event, AgentEvent::TextDelta(_)));
        assert!(!replied);
    }

    #[test]
    fn cancel_stops_a_hanging_turn() {
        let runtime = MockRuntime::new(vec![vec![MockStep::Hang]]);
        let session = runtime.start(spec()).expect("start");
        let stream = session.events();

        session.prompt("go").expect("prompt");
        collect(&stream, |event| *event == AgentEvent::TurnStarted);
        session.cancel().expect("cancel");

        collect(&stream, turn_ended);
    }

    #[test]
    fn closing_ends_the_session() {
        let runtime = MockRuntime::default();
        let session = runtime.start(spec()).expect("start");

        session.close();

        collect(&session.events(), |event| {
            matches!(event, AgentEvent::Exited(_))
        });
        assert_eq!(session.prompt("late"), Err(AgentError::Closed));
    }

    #[test]
    fn queued_prompts_run_in_order() {
        let first = vec![MockStep::Reply("one".to_string())];
        let second = vec![MockStep::Reply("two".to_string())];
        let runtime = MockRuntime::new(vec![first, second]);
        let session = runtime.start(spec()).expect("start");
        let stream = session.events();

        session.prompt("a").expect("prompt");
        session.prompt("b").expect("prompt");

        collect(&stream, turn_ended);
        let events = collect(&stream, turn_ended);
        assert!(events.contains(&AgentEvent::TextDelta("two".to_string())));
    }
}
