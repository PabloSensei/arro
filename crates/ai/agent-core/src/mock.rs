//! A scripted agent for UI and flow tests: no network, no process, no money.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, bounded, unbounded};
use serde_json::{Value, json};

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
    /// Writes a file of the project as a harness does, so the change tracker and the review
    /// see the edit. The chat shows the call and its answer.
    Edit {
        path: PathBuf,
        old_text: String,
        new_text: String,
    },
    /// What the user typed. A turn that opens with it shows this text as the message of the
    /// user instead of the prompt that started the turn.
    User(String),
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

    fn start(&self, spec: SessionSpec) -> Result<Box<dyn AgentSession>, AgentError> {
        let turns = std::mem::take(&mut *self.turns.lock().unwrap());
        let (events, stream) = unbounded();
        let (prompts, inbox) = bounded(64);
        let (replies, answers) = bounded(8);
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker = Worker {
            root: spec.cwd,
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
    /// The project folder the edit steps write in.
    root: PathBuf,
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
            let default = vec![MockStep::Reply("mock reply".to_string())];
            let mut steps = self.turns.pop_front().unwrap_or(default);
            // A turn may name the words of the user, so a demo shows what a person would
            // have typed instead of the prompt that started the turn.
            let typed = match steps.first().cloned() {
                Some(MockStep::User(text)) => {
                    steps.remove(0);
                    text
                }
                _ => prompt,
            };
            self.emit(AgentEvent::Message {
                role: Role::User,
                text: typed,
            });
            self.emit(AgentEvent::TurnStarted);
            self.emit(AgentEvent::Status(SessionStatus::Running));
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
                MockStep::Edit {
                    path,
                    old_text,
                    new_text,
                } => self.edit(index, path, old_text, new_text),
                MockStep::User(text) => self.emit(AgentEvent::Message {
                    role: Role::User,
                    text,
                }),
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

    /// Writes the file the way a harness does: straight to disk, so the change tracker and
    /// the review of the window see it. The chat shows the call and its answer as for any
    /// other tool.
    fn edit(&self, index: usize, path: PathBuf, old_text: String, new_text: String) {
        let id = format!("edit-{index}");
        let output = match edit_file(&self.root, &path, &old_text, &new_text) {
            Ok(()) => format!("Edited {}", path.display()),
            Err(error) => format!("error: {error}"),
        };
        self.emit(AgentEvent::ToolCall {
            id: id.clone(),
            name: "Edit".to_string(),
            input: json!({"path": path}),
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

/// Replaces the first `old_text` of a file of the project. A path that leaves the project,
/// a file that is not there and text the file does not hold are refused, so a transcript
/// can neither overwrite a file outside the project nor guess at its text.
fn edit_file(root: &Path, path: &Path, old_text: &str, new_text: &str) -> Result<(), String> {
    let absolute = root.join(path);
    let root = root
        .canonicalize()
        .map_err(|error| format!("the project folder is not read: {error}"))?;
    let file = absolute
        .canonicalize()
        .map_err(|error| format!("{}: {error}", path.display()))?;
    if !file.starts_with(&root) {
        return Err(format!("{} is outside the project", path.display()));
    }
    let text = std::fs::read_to_string(&file).map_err(|error| error.to_string())?;
    let Some(start) = text.find(old_text) else {
        return Err(format!("{} does not hold that text", path.display()));
    };
    let mut changed = String::with_capacity(text.len() + new_text.len());
    changed.push_str(&text[..start]);
    changed.push_str(new_text);
    changed.push_str(&text[start + old_text.len()..]);
    std::fs::write(&file, changed).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use std::path::Path;
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

    fn spec_in(root: &Path) -> SessionSpec {
        SessionSpec {
            project: root.to_path_buf(),
            cwd: root.to_path_buf(),
            ..spec()
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

    #[test]
    fn a_user_step_shows_instead_of_the_prompt() {
        let steps = vec![
            MockStep::User("what a person would type".to_string()),
            MockStep::Reply("hello".to_string()),
        ];
        let runtime = MockRuntime::new(vec![steps]);
        let session = runtime.start(spec()).expect("start");

        session.prompt("the prompt of the script").expect("prompt");

        let events = collect(&session.events(), turn_ended);
        let typed = |text: &str| AgentEvent::Message {
            role: Role::User,
            text: text.to_string(),
        };
        assert!(events.contains(&typed("what a person would type")));
        assert!(!events.contains(&typed("the prompt of the script")));
    }

    #[test]
    fn an_edit_writes_a_file_of_the_project() {
        let root = test_support::scratch("mock-edit");
        std::fs::create_dir_all(root.join("src")).unwrap();
        let file = root.join("src").join("main.txt");
        std::fs::write(&file, "alpha\nbeta\n").unwrap();
        let steps = vec![MockStep::Edit {
            path: "src/main.txt".into(),
            old_text: "alpha".to_string(),
            new_text: "ALPHA".to_string(),
        }];
        let runtime = MockRuntime::new(vec![steps]);
        let session = runtime.start(spec_in(&root)).expect("start");

        session.prompt("edit it").expect("prompt");

        let events = collect(&session.events(), turn_ended);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "ALPHA\nbeta\n");
        let result = AgentEvent::ToolResult {
            id: "edit-0".to_string(),
            output: "Edited src/main.txt".to_string(),
        };
        assert!(
            events.contains(&result),
            "the chat shows the edit: {events:?}"
        );
    }

    #[test]
    fn an_edit_outside_the_project_is_refused() {
        let root = test_support::scratch("mock-edit-root");
        let outside = test_support::scratch("mock-edit-outside").join("keep.txt");
        std::fs::write(&outside, "keep\n").unwrap();
        let steps = vec![MockStep::Edit {
            path: outside.clone(),
            old_text: "keep".to_string(),
            new_text: "gone".to_string(),
        }];
        let runtime = MockRuntime::new(vec![steps]);
        let session = runtime.start(spec_in(&root)).expect("start");

        session.prompt("edit it").expect("prompt");

        let events = collect(&session.events(), turn_ended);
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "keep\n");
        let refused = events.iter().any(|event| {
            matches!(
                event,
                AgentEvent::ToolResult { output, .. }
                    if output.starts_with("error: ") && output.contains("outside the project")
            )
        });
        assert!(refused, "the edit is refused: {events:?}");
    }
}
