//! Installed harnesses against the local mock, in an empty home. Off by default: they need the
//! binaries and take a while. `cargo test -p agent-pty --test real_harness -- --ignored`

use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::thread::sleep;
use std::time::{Duration, Instant};

use agent_core::{
    AgentEvent, AgentRuntime, AgentSession, Harness, Installed, SessionSpec, Surface, ToolEndpoint,
    detect,
};
use agent_pty::{Launch, PtyRuntime};
use agent_testkit::{FakeTools, MOCK_MODEL, MockApi, Sandbox, claude_env, codex_env};
use change_tracker::{ChangeTracker, GitRunner, TrackerState};
use crossbeam_channel::Receiver;
use mcp_server::McpServer;
use tool_gateway::{AuditLog, SessionScope, ToolGateway};

const WAIT: Duration = Duration::from_secs(60);
const PROMPT: &str = "hello from the session test";
const FIND_SYMBOL: &str = r#"MOCK_TOOL mcp__arro__find_symbol {"query":"Widget"}"#;

fn installed(harness: Harness) -> Installed {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut found = detect(&path).into_iter();
    found
        .find(|install| install.harness == harness)
        .expect("the harness is on PATH")
}

fn spec(sandbox: &Sandbox) -> SessionSpec {
    SessionSpec {
        project: sandbox.project.clone(),
        cwd: sandbox.project.clone(),
        cols: 120,
        rows: 40,
        tools: None,
    }
}

fn claude(sandbox: &Sandbox, mock: &MockApi) -> PtyRuntime {
    let launch = Launch::Prepared {
        env: claude_env(sandbox, mock).expect("env"),
        marker: None,
    };
    PtyRuntime::new(installed(Harness::Claude), launch)
}

fn codex(sandbox: &Sandbox, mock: &MockApi) -> PtyRuntime {
    let launch = Launch::Prepared {
        env: codex_env(sandbox, mock).expect("env"),
        marker: Some(MOCK_MODEL.to_string()),
    };
    PtyRuntime::new(installed(Harness::Codex), launch)
}

fn wait_for(
    stream: &Receiver<AgentEvent>,
    wanted: impl Fn(&AgentEvent) -> bool,
) -> Vec<AgentEvent> {
    let deadline = Instant::now() + WAIT;
    let mut seen = Vec::new();
    while Instant::now() < deadline {
        if let Ok(event) = stream.recv_timeout(Duration::from_millis(100)) {
            let stop = wanted(&event);
            seen.push(event);
            if stop {
                return seen;
            }
        }
    }
    panic!("no such event in {seen:?}");
}

fn wait_until(what: &str, check: impl Fn() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !check() {
        assert!(Instant::now() < deadline, "{what}");
        sleep(Duration::from_millis(50));
    }
}

fn turn_ended(event: &AgentEvent) -> bool {
    *event == AgentEvent::TurnEnded
}

fn exited(event: &AgentEvent) -> bool {
    matches!(event, AgentEvent::Exited(_))
}

fn screen(session: &dyn AgentSession) -> String {
    match session.surface() {
        Surface::Terminal(terminal) => terminal.screen().lines().join("\n"),
        Surface::Transcript => String::new(),
    }
}

struct Tools {
    server: McpServer,
    gateway: Arc<ToolGateway>,
}

fn tools() -> Tools {
    let gateway = Arc::new(ToolGateway::new(Arc::new(FakeTools), AuditLog::default()));
    let server = McpServer::start(gateway.clone()).expect("mcp server");
    Tools { server, gateway }
}

fn connected(sandbox: &Sandbox, tools: &Tools) -> SessionSpec {
    let token = tools.server.issue_token(SessionScope {
        session: "s1".to_string(),
        project: sandbox.project.clone(),
        cwd: sandbox.project.clone(),
    });
    SessionSpec {
        tools: Some(ToolEndpoint {
            url: tools.server.url(),
            token: token.expect("token"),
        }),
        ..spec(sandbox)
    }
}

/// What still runs from a home, the copy of the Codex daemon for one.
fn processes_under(home: &Path) -> String {
    let filter = format!(
        "Get-CimInstance Win32_Process | Where-Object {{ $_.ExecutablePath -like '{}*' }} \
         | ForEach-Object {{ $_.ExecutablePath }}",
        home.display()
    );
    let output = Command::new("powershell")
        .args(["-NoProfile", "-Command", &filter])
        .output()
        .expect("powershell");
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn prompt_reaches_the_model(runtime: &PtyRuntime, sandbox: &Sandbox, mock: &MockApi) {
    let session = runtime.start(spec(sandbox)).expect("start");
    let stream = session.events();

    session.prompt(PROMPT).expect("prompt");
    wait_for(&stream, turn_ended);

    let texts = mock.user_texts();
    let seen = texts.iter().any(|text| text.contains(PROMPT));
    assert!(seen, "{texts:?}\n{}", screen(session.as_ref()));
    session.close();
    wait_for(&stream, exited);
}

#[test]
#[ignore = "needs an installed claude"]
fn claude_takes_a_prompt() {
    let sandbox = Sandbox::new("cl").expect("sandbox");
    let mock = MockApi::start().expect("mock");

    prompt_reaches_the_model(&claude(&sandbox, &mock), &sandbox, &mock);
}

#[test]
#[ignore = "needs an installed codex"]
fn codex_takes_a_prompt() {
    let sandbox = Sandbox::new("cx").expect("sandbox");
    let mock = MockApi::start().expect("mock");

    prompt_reaches_the_model(&codex(&sandbox, &mock), &sandbox, &mock);
}

#[test]
#[ignore = "needs an installed claude"]
fn claude_calls_a_tool_and_its_edit_shows_up() {
    let sandbox = Sandbox::new("clt").expect("sandbox");
    let mock = MockApi::start().expect("mock");
    let tools = tools();
    let path = std::env::var_os("PATH").unwrap_or_default();
    let git = GitRunner::find(&path).expect("git on PATH");
    let tracker = ChangeTracker::open(git, &sandbox.home.join("state"), &sandbox.project);
    let tracker = tracker.expect("tracker");
    wait_until("the first snapshot", || {
        tracker.changes().state == TrackerState::Ready
    });
    let arguments = ["--permission-mode", "acceptEdits"].map(String::from);
    let runtime = claude(&sandbox, &mock).with_arguments(arguments.to_vec());
    let session = runtime.start(connected(&sandbox, &tools)).expect("start");
    let stream = session.events();
    // The server connects in the background, the box is live before its tools are known.
    sleep(Duration::from_secs(6));

    session.prompt(FIND_SYMBOL).expect("prompt");
    wait_for(&stream, turn_ended);
    wait_until(&screen(session.as_ref()), || {
        let results = mock.tool_results();
        results.iter().any(|text| text.contains("class Widget"))
    });
    assert_eq!(tools.gateway.audit().recent()[0].tool, "find_symbol");

    let file = sandbox.project.join("Edited.txt");
    let file = file.to_string_lossy().replace('\\', "/");
    let write = format!(r#"MOCK_TOOL Write {{"file_path":"{file}","content":"edit"}}"#);
    tracker.turn_started();
    session.prompt(&write).expect("prompt");
    wait_for(&stream, turn_ended);
    tracker.turn_ended();
    wait_until(&screen(session.as_ref()), || {
        let changes = tracker.changes();
        changes
            .session
            .iter()
            .any(|change| change.path == "Edited.txt")
    });

    session.close();
    wait_for(&stream, exited);
}

#[test]
#[ignore = "needs an installed codex"]
fn codex_calls_a_tool() {
    let sandbox = Sandbox::new("cxt").expect("sandbox");
    let mock = MockApi::start().expect("mock");
    let tools = tools();
    let runtime = codex(&sandbox, &mock);
    let session = runtime.start(connected(&sandbox, &tools)).expect("start");
    let stream = session.events();

    session.prompt(FIND_SYMBOL).expect("prompt");
    wait_for(&stream, turn_ended);
    wait_until(&screen(session.as_ref()), || {
        let results = mock.tool_results();
        results.iter().any(|text| text.contains("class Widget"))
    });

    session.close();
    wait_for(&stream, exited);
    wait_until("no process left in the home", || {
        processes_under(&sandbox.home).is_empty()
    });
}

#[test]
#[ignore = "needs an installed codex"]
fn closing_one_codex_session_leaves_the_other_working() {
    let sandbox = Sandbox::new("cx2").expect("sandbox");
    let mock = MockApi::start().expect("mock");
    let runtime = codex(&sandbox, &mock);
    let first = runtime.start(spec(&sandbox)).expect("first");
    let first_stream = first.events();
    first.prompt("from the first session").expect("prompt");
    wait_for(&first_stream, turn_ended);
    let second = runtime.start(spec(&sandbox)).expect("second");
    let second_stream = second.events();
    second.prompt("from the second session").expect("prompt");
    let events = wait_for(&second_stream, |event| turn_ended(event) || exited(event));
    let alive = !events.iter().any(exited);
    assert!(alive, "{events:?}\n{}", screen(second.as_ref()));

    first.close();
    wait_for(&first_stream, exited);
    second.prompt("after the first one closed").expect("prompt");
    wait_for(&second_stream, turn_ended);

    wait_until("the prompt reached the model", || {
        let texts = mock.user_texts();
        texts
            .iter()
            .any(|text| text.contains("after the first one closed"))
    });
    second.close();
    wait_for(&second_stream, exited);
    wait_until("no process left in the home", || {
        processes_under(&sandbox.home).is_empty()
    });
}
