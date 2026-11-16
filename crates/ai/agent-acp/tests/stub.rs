//! The ACP runtime against `acp-stub`, a real process on real pipes.

use std::time::{Duration, Instant};

use agent_acp::{AcpCommand, AcpRuntime};
use agent_core::{
    AgentEvent, AgentRuntime, AgentSession, PermissionReply, PromptPart, SessionSpec,
    SessionStatus, ToolStatus,
};
use crossbeam_channel::Receiver;

const WAIT: Duration = Duration::from_secs(20);

struct Fixture {
    project: test_support::TempDir,
    session: Box<dyn AgentSession>,
    events: Receiver<AgentEvent>,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let project = test_support::TempDir::new(&format!("acp-{name}"));
        let stub = AcpCommand::new(env!("CARGO_BIN_EXE_acp-stub"), Vec::new());
        let runtime = AcpRuntime::new("stub", stub);
        let session = runtime
            .start(SessionSpec {
                project: project.path().to_path_buf(),
                cwd: project.path().to_path_buf(),
                cols: 80,
                rows: 24,
                tools: None,
            })
            .expect("start");
        let events = session.events();
        Self {
            project,
            session,
            events,
        }
    }

    /// Reads until `done`, returns everything read.
    fn until(&self, done: impl Fn(&AgentEvent) -> bool) -> Vec<AgentEvent> {
        let deadline = Instant::now() + WAIT;
        let mut seen = Vec::new();
        while Instant::now() < deadline {
            let Ok(event) = self.events.recv_timeout(Duration::from_millis(50)) else {
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

    fn turn(&self) -> Vec<AgentEvent> {
        self.until(|event| *event == AgentEvent::TurnEnded)
    }

    fn text(events: &[AgentEvent]) -> String {
        let deltas = events.iter().filter_map(|event| match event {
            AgentEvent::TextDelta(text) => Some(text.as_str()),
            _ => None,
        });
        deltas.collect()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.session.close();
    }
}

#[test]
fn a_turn_streams_text_and_ends() {
    let fixture = Fixture::new("turn");
    fixture.until(|event| *event == AgentEvent::Status(SessionStatus::WaitingForUser));

    fixture.session.prompt("hello").expect("prompt");
    let events = fixture.turn();

    assert_eq!(Fixture::text(&events), "echo: hello");
    assert!(events.contains(&AgentEvent::TurnStarted));
}

#[test]
fn files_and_snippets_reach_the_agent_as_links_and_resources() {
    let fixture = Fixture::new("mentions");
    let parts = [
        PromptPart::Text("look".to_string()),
        PromptPart::File(fixture.project.join("Main.txt")),
        PromptPart::Snippet {
            path: fixture.project.join("Main.txt"),
            lines: (1, 2),
            text: "a\nb".to_string(),
        },
    ];

    fixture.session.prompt_parts(&parts).expect("prompt");
    let events = fixture.turn();

    assert_eq!(
        Fixture::text(&events),
        "echo: look links: Main.txt,resource"
    );
}

#[test]
fn an_allowed_edit_writes_and_a_denied_one_does_not() {
    let fixture = Fixture::new("permission");

    fixture
        .session
        .prompt("ASK\nWRITE a.txt one")
        .expect("prompt");
    let asked = fixture.until(|event| matches!(event, AgentEvent::PermissionRequest { .. }));
    let Some(AgentEvent::PermissionRequest {
        id, description, ..
    }) = asked.last()
    else {
        unreachable!();
    };
    assert_eq!(description, "write a.txt");
    fixture
        .session
        .respond(id, PermissionReply::AllowOnce)
        .expect("respond");
    let events = fixture.turn();
    let done = AgentEvent::ToolStatus {
        id: "call-write".to_string(),
        status: ToolStatus::Done,
        output: Some("written".to_string()),
    };
    assert!(events.contains(&done), "{events:?}");
    assert!(fixture.project.join("a.txt").exists());

    fixture
        .session
        .prompt("ASK\nWRITE b.txt two")
        .expect("prompt");
    let asked = fixture.until(|event| matches!(event, AgentEvent::PermissionRequest { .. }));
    let Some(AgentEvent::PermissionRequest { id, .. }) = asked.last() else {
        unreachable!();
    };
    fixture
        .session
        .respond(id, PermissionReply::Deny)
        .expect("respond");
    let events = fixture.turn();
    let failed = events.iter().any(|event| {
        matches!(
            event,
            AgentEvent::ToolStatus {
                status: ToolStatus::Failed,
                ..
            }
        )
    });
    assert!(failed, "{events:?}");
    assert!(!fixture.project.join("b.txt").exists());
}

#[test]
fn cancel_ends_the_turn_and_the_queue_goes_on() {
    let fixture = Fixture::new("cancel");

    fixture.session.prompt("HANG").expect("prompt");
    fixture.session.prompt("after").expect("queued");
    fixture.until(|event| *event == AgentEvent::TurnStarted);
    fixture.session.cancel().expect("cancel");
    let first = fixture.turn();
    let second = fixture.turn();

    let cancelled = AgentEvent::Notification("The turn ended: cancelled".to_string());
    assert!(first.contains(&cancelled), "{first:?}");
    assert_eq!(Fixture::text(&second), "echo: after");
}

#[test]
fn close_ends_the_agent() {
    let fixture = Fixture::new("close");
    fixture.until(|event| *event == AgentEvent::Status(SessionStatus::WaitingForUser));

    fixture.session.close();

    fixture.until(|event| matches!(event, AgentEvent::Exited(_)));
}

#[test]
fn a_missing_agent_reports_an_error() {
    let project = test_support::TempDir::new("acp-missing");
    let command = AcpCommand::shell("no-such-agent-here".to_string());
    let runtime = AcpRuntime::new("missing", command);
    let session = runtime
        .start(SessionSpec {
            project: project.path().to_path_buf(),
            cwd: project.path().to_path_buf(),
            cols: 80,
            rows: 24,
            tools: None,
        })
        .expect("cmd starts");
    let events = session.events();
    let deadline = Instant::now() + WAIT;
    let mut error = None;
    while error.is_none() && Instant::now() < deadline {
        if let Ok(AgentEvent::Error(text)) = events.recv_timeout(Duration::from_millis(50)) {
            error = Some(text);
        }
    }
    let error = error.expect("an error");
    assert!(error.contains("did not start"), "{error}");
}
