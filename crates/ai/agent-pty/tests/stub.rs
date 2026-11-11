//! The session worker over a stand-in TUI. No harness runs.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use agent_core::{
    AgentEvent, AgentRuntime, AgentSession, Harness, Installed, SessionSpec, SessionStatus,
};
use agent_pty::{Launch, PtyRuntime};
use agent_testkit::Sandbox;
use crossbeam_channel::Receiver;

const WAIT: Duration = Duration::from_secs(30);

const CLAUDE_BOX: &str = "\
Welcome
────────────────────────────────
❯
────────────────────────────────
";
const LOGIN: &str = "\
Select login method:
❯ 1. Claude account with subscription
  2. Anthropic Console account
";

fn stub(sandbox: &Sandbox, screen: &str) -> PathBuf {
    let file = sandbox.home.join("screen.txt");
    std::fs::write(&file, screen).expect("screen");
    file
}

fn runtime(sandbox: &Sandbox, screen: &Path) -> PtyRuntime {
    let installed = Installed {
        harness: Harness::Claude,
        program: env!("CARGO_BIN_EXE_tui-stub").into(),
        prefix: vec![screen.display().to_string()],
        through_shell: false,
    };
    let launch = Launch::Prepared {
        env: sandbox.env().expect("env"),
        marker: None,
    };
    PtyRuntime::new(installed, launch).with_first_prompt_wait(Duration::from_secs(4))
}

fn start(sandbox: &Sandbox, screen: &Path) -> Box<dyn AgentSession> {
    let spec = SessionSpec {
        project: sandbox.project.clone(),
        cwd: sandbox.project.clone(),
        cols: 100,
        rows: 30,
        tools: None,
    };
    runtime(sandbox, screen).start(spec).expect("start")
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

#[test]
fn a_prompt_goes_into_the_box_and_the_turn_ends_on_silence() {
    let sandbox = Sandbox::new("box").expect("sandbox");
    let screen = stub(&sandbox, CLAUDE_BOX);
    let session = start(&sandbox, &screen);
    let stream = session.events();

    session.prompt("hello").expect("prompt");

    let done = AgentEvent::Status(SessionStatus::Done);
    let events = wait_for(&stream, |event| *event == done);
    assert!(events.contains(&AgentEvent::TurnStarted));
    assert!(events.contains(&AgentEvent::Status(SessionStatus::Running)));
    assert!(events.contains(&AgentEvent::TurnEnded));
    session.close();
}

#[test]
fn a_login_screen_gets_no_keys() {
    let sandbox = Sandbox::new("login").expect("sandbox");
    let screen = stub(&sandbox, LOGIN);
    let session = start(&sandbox, &screen);
    let stream = session.events();
    let bytes_before = terminal_bytes(session.as_ref());

    session.prompt("must not arrive").expect("queue");

    let refused = |event: &AgentEvent| matches!(event, AgentEvent::Error(_));
    let events = wait_for(&stream, refused);
    assert!(!events.contains(&AgentEvent::TurnStarted));
    let shown = screen_text(session.as_ref());
    assert!(!shown.contains("must not arrive"), "{shown}");
    assert!(terminal_bytes(session.as_ref()) >= bytes_before);
    session.close();
}

#[test]
fn closing_ends_the_session() {
    let sandbox = Sandbox::new("close").expect("sandbox");
    let screen = stub(&sandbox, CLAUDE_BOX);
    let session = start(&sandbox, &screen);
    let stream = session.events();

    session.close();

    wait_for(&stream, |event| matches!(event, AgentEvent::Exited(_)));
}

fn with_terminal<T>(
    session: &dyn AgentSession,
    read: impl Fn(&terminal::TerminalSession) -> T,
) -> T {
    match session.surface() {
        agent_core::Surface::Terminal(terminal) => read(&terminal),
        agent_core::Surface::Transcript => panic!("a TUI session has a terminal"),
    }
}

fn terminal_bytes(session: &dyn AgentSession) -> u64 {
    with_terminal(session, |terminal| terminal.output_bytes())
}

fn screen_text(session: &dyn AgentSession) -> String {
    with_terminal(session, |terminal| terminal.screen().lines().join("\n"))
}
