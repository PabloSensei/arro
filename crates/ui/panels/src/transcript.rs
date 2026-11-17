//! What the chat of a session shows, built from its events. No GPUI here, so the logic is
//! tested on `MockRuntime` alone.

use agent_core::{AgentEvent, PermissionReply, Role, ToolStatus};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Item {
    User(String),
    Agent(String),
    Tool {
        id: String,
        title: String,
        input: String,
        status: ToolStatus,
        output: Option<String>,
    },
    Permission {
        id: String,
        tool: String,
        description: String,
        answer: Option<PermissionReply>,
    },
    Notice(String),
    Error(String),
}

#[derive(Default)]
pub struct Transcript {
    pub items: Vec<Item>,
    pub title: Option<String>,
    pub running: bool,
    /// Text deltas go into the last agent message until something else happens.
    streaming: bool,
}

/// A short form of tool input for the collapsed row.
fn summary(input: &Value) -> String {
    let text = match input {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        other => other.to_string(),
    };
    const LIMIT: usize = 120;
    match text.char_indices().nth(LIMIT) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text,
    }
}

impl Transcript {
    fn tool_mut(&mut self, wanted: &str) -> Option<&mut Item> {
        let mut tools = self.items.iter_mut().rev();
        tools.find(|item| matches!(item, Item::Tool { id, .. } if id == wanted))
    }

    /// Takes one event; false when the event changes nothing on screen.
    pub fn apply(&mut self, event: &AgentEvent) -> bool {
        match event {
            AgentEvent::Message {
                role: Role::User,
                text,
            } => {
                self.streaming = false;
                self.items.push(Item::User(text.clone()));
            }
            AgentEvent::TextDelta(text) => match self.items.last_mut() {
                Some(Item::Agent(message)) if self.streaming => message.push_str(text),
                _ => {
                    self.streaming = true;
                    self.items.push(Item::Agent(text.clone()));
                }
            },
            AgentEvent::Message {
                role: Role::Assistant,
                text,
            } => {
                // The deltas already showed it.
                if !self.streaming {
                    self.items.push(Item::Agent(text.clone()));
                }
                self.streaming = false;
            }
            AgentEvent::ToolCall { id, name, input } => {
                self.streaming = false;
                self.items.push(Item::Tool {
                    id: id.clone(),
                    title: name.clone(),
                    input: summary(input),
                    status: ToolStatus::Pending,
                    output: None,
                });
            }
            AgentEvent::ToolResult { id, output } => {
                self.streaming = false;
                if let Some(Item::Tool {
                    status,
                    output: shown,
                    ..
                }) = self.tool_mut(id)
                {
                    *status = ToolStatus::Done;
                    *shown = Some(output.clone());
                }
            }
            AgentEvent::ToolStatus { id, status, output } => {
                self.streaming = false;
                match self.tool_mut(id) {
                    Some(Item::Tool {
                        status: shown_status,
                        output: shown,
                        ..
                    }) => {
                        *shown_status = *status;
                        if output.is_some() {
                            shown.clone_from(output);
                        }
                    }
                    _ => self.items.push(Item::Tool {
                        id: id.clone(),
                        title: id.clone(),
                        input: String::new(),
                        status: *status,
                        output: output.clone(),
                    }),
                }
            }
            AgentEvent::PermissionRequest {
                id,
                tool,
                description,
            } => {
                self.streaming = false;
                self.items.push(Item::Permission {
                    id: id.clone(),
                    tool: tool.clone(),
                    description: description.clone(),
                    answer: None,
                });
            }
            AgentEvent::TurnStarted => self.running = true,
            AgentEvent::TurnEnded => {
                self.running = false;
                self.streaming = false;
                self.clear_pending();
            }
            AgentEvent::Title(title) => self.title = Some(title.clone()),
            AgentEvent::Notification(text) => self.items.push(Item::Notice(text.clone())),
            AgentEvent::Error(text) => self.items.push(Item::Error(text.clone())),
            AgentEvent::Exited(_) => {
                self.running = false;
                self.items
                    .push(Item::Notice("The agent has exited".to_string()));
            }
            AgentEvent::Status(_) | AgentEvent::Idle => return false,
        }
        true
    }

    /// The question the agent waits on, if any.
    pub fn pending_permission(&self) -> Option<&str> {
        self.items.iter().rev().find_map(|item| match item {
            Item::Permission {
                id, answer: None, ..
            } => Some(id.as_str()),
            _ => None,
        })
    }

    pub fn answer(&mut self, wanted: &str, reply: PermissionReply) {
        for item in &mut self.items {
            if let Item::Permission { id, answer, .. } = item
                && id == wanted
            {
                *answer = Some(reply);
            }
        }
    }

    /// The turn is over: no question of it waits for an answer any more. The agent runs no
    /// step of it, so a question left on screen is shown as denied rather than kept.
    pub fn clear_pending(&mut self) {
        for item in &mut self.items {
            if let Item::Permission { answer, .. } = item
                && answer.is_none()
            {
                *answer = Some(PermissionReply::Deny);
            }
        }
    }

    /// One line per item, for scripts.
    pub fn report(&self) -> Vec<String> {
        let first_line = |text: &str| text.lines().next().unwrap_or_default().to_string();
        self.items
            .iter()
            .map(|item| match item {
                Item::User(text) => format!("chat user {}", first_line(text)),
                Item::Agent(text) => format!("chat agent {}", first_line(text)),
                Item::Tool { title, status, .. } => format!("chat tool {title} {status:?}"),
                Item::Permission {
                    tool,
                    description,
                    answer,
                    ..
                } => format!("chat permission {tool}: {description} -> {answer:?}"),
                Item::Notice(text) => format!("chat notice {}", first_line(text)),
                Item::Error(text) => format!("chat error {}", first_line(text)),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use agent_core::{AgentRuntime, MockRuntime, MockStep, SessionSpec};
    use serde_json::json;

    use super::{Item, PermissionReply, ToolStatus, Transcript};

    fn spec() -> SessionSpec {
        SessionSpec {
            project: "project".into(),
            cwd: "project".into(),
            cols: 80,
            rows: 24,
            tools: None,
        }
    }

    /// Feeds events into the transcript until `done` holds.
    fn feed(
        transcript: &mut Transcript,
        events: &crossbeam_channel::Receiver<agent_core::AgentEvent>,
        done: impl Fn(&Transcript) -> bool,
    ) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done(transcript) {
            assert!(Instant::now() < deadline, "{:?}", transcript.items);
            if let Ok(event) = events.recv_timeout(Duration::from_millis(20)) {
                transcript.apply(&event);
            }
        }
    }

    #[test]
    fn a_turn_shows_the_prompt_the_streamed_reply_and_the_tool() {
        let steps = vec![
            MockStep::Reply("looking it up".to_string()),
            MockStep::Tool {
                name: "find_symbol".to_string(),
                input: json!({"query": "Foo"}),
            },
        ];
        let tools: agent_core::ToolInvoker = std::sync::Arc::new(|_, _| Ok("found".to_string()));
        let runtime = MockRuntime::new(vec![steps]).with_tools(tools);
        let session = runtime.start(spec()).expect("start");
        let events = session.events();
        let mut transcript = Transcript::default();

        session.prompt("where is Foo").expect("prompt");
        feed(&mut transcript, &events, |transcript| {
            transcript.items.len() == 3 && !transcript.running
        });

        assert_eq!(transcript.items[0], Item::User("where is Foo".to_string()));
        assert_eq!(
            transcript.items[1],
            Item::Agent("looking it up".to_string())
        );
        let Item::Tool { status, output, .. } = &transcript.items[2] else {
            panic!("{:?}", transcript.items);
        };
        assert_eq!(
            (*status, output.as_deref()),
            (ToolStatus::Done, Some("found"))
        );
    }

    #[test]
    fn a_denied_permission_is_shown_answered_and_stops_the_turn() {
        let steps = vec![
            MockStep::Permission {
                tool: "Write".to_string(),
                description: "write a.txt".to_string(),
            },
            MockStep::Reply("must not appear".to_string()),
        ];
        let runtime = MockRuntime::new(vec![steps]);
        let session = runtime.start(spec()).expect("start");
        let events = session.events();
        let mut transcript = Transcript::default();

        session.prompt("edit").expect("prompt");
        feed(&mut transcript, &events, |transcript| {
            transcript.pending_permission().is_some()
        });
        let id = transcript.pending_permission().unwrap().to_string();
        session
            .respond(&id, PermissionReply::Deny)
            .expect("respond");
        transcript.answer(&id, PermissionReply::Deny);
        feed(&mut transcript, &events, |transcript| !transcript.running);

        assert!(transcript.pending_permission().is_none());
        let shown = transcript.report().join("\n");
        assert!(
            shown.contains("chat permission Write: write a.txt -> Some(Deny)"),
            "{shown}"
        );
        assert!(!shown.contains("must not appear"), "{shown}");
    }

    #[test]
    fn a_tool_status_without_a_call_still_shows_up() {
        let mut transcript = Transcript::default();

        transcript.apply(&agent_core::AgentEvent::ToolStatus {
            id: "x".to_string(),
            status: ToolStatus::Failed,
            output: Some("denied by review".to_string()),
        });

        assert_eq!(transcript.report(), ["chat tool x Failed"]);
    }
}
