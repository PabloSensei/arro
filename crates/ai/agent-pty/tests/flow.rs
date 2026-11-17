//! The whole path without a process: a scripted agent calls a tool through the gateway, changes a
//! file, and the tracker sees it when the turn ends.

use std::fs;
use std::sync::Arc;
use std::thread::sleep;
use std::time::{Duration, Instant};

use agent_core::{
    AgentEvent, AgentRuntime, MockRuntime, MockStep, SessionSpec, SessionStatus, ToolInvoker,
};
use agent_testkit::{FakeTools, Sandbox};
use change_tracker::{ChangeKind, ChangeTracker, GitRunner, TrackerState};
use serde_json::json;
use tool_gateway::{AuditLog, SessionScope, ToolGateway};

const WAIT: Duration = Duration::from_secs(20);

#[test]
fn an_agent_finds_a_symbol_and_its_edit_shows_up() {
    let sandbox = Sandbox::new("flow").expect("sandbox");
    let path = std::env::var_os("PATH").unwrap_or_default();
    let git = GitRunner::find(&path).expect("git on PATH");
    let state = sandbox.home.join("state");
    let tracker = ChangeTracker::open(git, &state, &sandbox.project).expect("tracker");

    let gateway = ToolGateway::new(Arc::new(FakeTools), AuditLog::default());
    let scope = SessionScope {
        session: "s1".to_string(),
        project: sandbox.project.clone(),
        cwd: sandbox.project.clone(),
    };
    let project = sandbox.project.clone();
    let tools: ToolInvoker = Arc::new(move |name, input| {
        if name == "write_file" {
            fs::write(project.join("Edited.txt"), "edit").map_err(|error| error.to_string())?;
            return Ok("written".to_string());
        }
        gateway
            .call(&scope, name, input)
            .map_err(|error| error.to_string())
    });
    let turn = vec![
        MockStep::Tool {
            name: "find_symbol".to_string(),
            input: json!({"query": "Widget"}),
        },
        MockStep::Tool {
            name: "write_file".to_string(),
            input: json!({}),
        },
        MockStep::Reply("done".to_string()),
    ];
    let runtime = MockRuntime::new(vec![turn]).with_tools(tools);
    let session = runtime
        .start(SessionSpec {
            project: sandbox.project.clone(),
            cwd: sandbox.project.clone(),
            cols: 80,
            rows: 24,
            tools: None,
        })
        .expect("start");
    let stream = session.events();
    let ready = Instant::now() + WAIT;
    while tracker.changes().state != TrackerState::Ready {
        assert!(Instant::now() < ready);
        sleep(Duration::from_millis(20));
    }

    session.prompt("find it and edit it").expect("prompt");

    let mut output = None;
    let deadline = Instant::now() + WAIT;
    while Instant::now() < deadline {
        let Ok(event) = stream.recv_timeout(Duration::from_millis(100)) else {
            continue;
        };
        tracker.observe(&event);
        match event {
            AgentEvent::ToolResult { output: text, .. } if output.is_none() => output = Some(text),
            AgentEvent::Status(SessionStatus::Done) => break,
            _ => {}
        }
    }
    let output = output.expect("a tool result");
    assert!(output.contains("class Widget"), "{output}");
    assert!(output.contains("confidence"), "{output}");
    let seen = Instant::now() + WAIT;
    while tracker.changes().session.is_empty() {
        assert!(Instant::now() < seen, "{:?}", tracker.changes());
        sleep(Duration::from_millis(20));
    }
    let changes = tracker.changes();
    assert_eq!(changes.session.len(), 1);
    assert_eq!(changes.session[0].path, "Edited.txt");
    assert_eq!(changes.session[0].kind, ChangeKind::Added);
}
