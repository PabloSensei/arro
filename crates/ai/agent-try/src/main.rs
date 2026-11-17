//! Tries the whole agent stack in a console until the IDE has a window: a harness in our terminal,
//! the MCP server with made-up tool answers, the change list at the end.
//!
//! `agent-try <claude|codex|opencode> [--project <dir>] [--inherit <variable>]... [--mock]`
//!
//! Without `--mock` the harness runs in the real home of the user, with their login and their
//! models. Only a person may start it that way. `--mock` is an empty home and a local model API.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use agent_core::{AgentEvent, AgentRuntime, Harness, SessionSpec, Surface, ToolEndpoint, detect};
use agent_pty::{Launch, PtyRuntime};
use agent_testkit::{FakeTools, MOCK_MODEL, MockApi, Sandbox, claude_env, codex_env};
use change_tracker::{ChangeTracker, GitRunner};
use mcp_server::McpServer;
use terminal::{attach_console, console_size};
use tool_gateway::{AuditLog, SessionScope, ToolGateway};

const USAGE: &str =
    "usage: agent-try <claude|codex|opencode> [--project <dir>] [--inherit <variable>]... [--mock]";
const LAST_SNAPSHOT_WAIT: Duration = Duration::from_secs(30);

type Error = Box<dyn std::error::Error>;

struct Options {
    harness: Harness,
    project: Option<PathBuf>,
    inherit: Vec<String>,
    mock: bool,
}

fn parse() -> Option<Options> {
    let mut arguments = std::env::args().skip(1);
    let name = arguments.next()?;
    let harness = *Harness::ALL.iter().find(|harness| harness.stem() == name)?;
    let (mut project, mut inherit, mut mock) = (None, Vec::new(), false);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--mock" => mock = true,
            "--project" => project = Some(PathBuf::from(arguments.next()?)),
            "--inherit" => inherit.push(arguments.next()?),
            _ => return None,
        }
    }
    Some(Options {
        harness,
        project,
        inherit,
        mock,
    })
}

/// Everything that has to live as long as the session in a mock run.
struct Mock {
    sandbox: Sandbox,
    _api: MockApi,
    launch: Launch,
}

fn mock_launch(harness: Harness) -> Result<Mock, Error> {
    let sandbox = Sandbox::new("try")?;
    let api = MockApi::start()?;
    let launch = match harness {
        Harness::Claude => Launch::Prepared {
            env: claude_env(&sandbox, &api)?,
            marker: None,
        },
        Harness::Codex => Launch::Prepared {
            env: codex_env(&sandbox, &api)?,
            marker: Some(MOCK_MODEL.to_string()),
        },
        Harness::OpenCode => return Err("--mock has no setup for opencode".into()),
    };
    Ok(Mock {
        sandbox,
        _api: api,
        launch,
    })
}

fn run(options: Options) -> Result<(), Error> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let installed = detect(&path)
        .into_iter()
        .find(|install| install.harness == options.harness)
        .ok_or("the harness is not on PATH")?;
    let mock = options
        .mock
        .then(|| mock_launch(options.harness))
        .transpose()?;
    let project = match (&mock, options.project) {
        (Some(mock), _) => mock.sandbox.project.clone(),
        (None, Some(project)) => project,
        (None, None) => std::env::current_dir()?,
    };
    let launch = match &mock {
        Some(mock) => mock.launch.clone(),
        None => Launch::UserHome {
            inherit: options.inherit,
            set: Vec::new(),
        },
    };

    let gateway = Arc::new(ToolGateway::new(Arc::new(FakeTools), AuditLog::default()));
    let server = McpServer::start(gateway.clone())?;
    let token = server.issue_token(SessionScope {
        session: "agent-try".to_string(),
        project: project.clone(),
        cwd: project.clone(),
    })?;

    let git = GitRunner::find(&path).ok_or("git is not on PATH")?;
    let state = std::env::temp_dir().join(format!("ide-try-{}", std::process::id()));
    let tracker = Arc::new(ChangeTracker::open(git, &state, &project)?);

    let (cols, rows) = console_size().ok_or("this needs a console")?;
    let runtime = PtyRuntime::new(installed, launch);
    let session = runtime.start(SessionSpec {
        project: project.clone(),
        cwd: project,
        cols,
        rows,
        tools: Some(ToolEndpoint {
            url: server.url(),
            token,
        }),
    })?;

    let observer = tracker.clone();
    let stream = session.events();
    let events = thread::spawn(move || {
        let mut errors = Vec::new();
        for event in stream {
            observer.observe(&event);
            match event {
                AgentEvent::Error(message) => errors.push(message),
                AgentEvent::Exited(_) => break,
                _ => {}
            }
        }
        errors
    });

    let version = tracker.changes().version;
    let Surface::Terminal(terminal) = session.surface() else {
        return Err("this harness has no terminal".into());
    };
    attach_console(terminal.clone())?;
    let errors = events.join().unwrap_or_default();

    let deadline = Instant::now() + LAST_SNAPSHOT_WAIT;
    while tracker.changes().version == version && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(50));
    }
    println!("exit code: {:?}", terminal.exit_code());
    let changes = tracker.changes();
    println!("changed files: {}", changes.session.len());
    for change in &changes.session {
        println!("  {:?} {}", change.kind, change.path);
    }
    let calls = gateway.audit().recent();
    println!("tool calls: {}", calls.len());
    for call in calls {
        println!("  {} ok={}", call.tool, call.ok);
    }
    for error in errors {
        println!("error: {error}");
    }
    let _ = std::fs::remove_dir_all(state);
    Ok(())
}

fn main() -> ExitCode {
    let Some(options) = parse() else {
        eprintln!("{USAGE}");
        return ExitCode::FAILURE;
    };
    match run(options) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("agent-try: {error}");
            ExitCode::FAILURE
        }
    }
}
