//! The MCP tools of the debugger: a breakpoint the window and the editor show, a session
//! started under the debugger after the question of the user, the stack and the values of
//! the stop, a step and the end of the session. The mock adapter of the fixture language
//! plays the debuggee; no window and no real project are needed.

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use dap_client::{BreakpointSpec, DebugService, DebugState, Debugger, Launch};
use proc_sup::ProcessSpec;
use project_model::ProjectModel;
use runner::{RunPlan, TestListen, TestProvider, TestRequest};
use serde_json::{Value, json};
use tool_gateway::{AuditLog, SessionScope, ToolExposure, ToolGateway};
use ui_kit::gpui;
use ui_kit::gpui::TestAppContext;

use crate::agents::{ActiveAgents, AgentLanguages};
use crate::mcp_approval::Reply;
use crate::window_tests::{draw, editor_of, open_path};
use crate::window_tests_debug_actions::{debug_panel, last_breakpoint_lines, setup};
use crate::window_tests_run::{open_with, run_panel, setup as run_setup, wait_for};

fn scope(root: &Path) -> SessionScope {
    SessionScope {
        session: "mcp-debug".to_string(),
        project: root.to_path_buf(),
        cwd: root.to_path_buf(),
    }
}

/// The gateway of the window over the language tools it filled in; its questions go to
/// the notification of the window.
fn gateway(cx: &mut TestAppContext) -> (Arc<ToolGateway>, Arc<crate::mcp_approval::McpApprovals>) {
    let agents = cx.read(|cx| cx.global::<ActiveAgents>().0.clone());
    let approvals = agents.read_with(cx, |panel, _| panel.approvals.clone());
    let tools = cx.read(|cx| cx.global::<AgentLanguages>().0.clone());
    let gateway = Arc::new(ToolGateway::with_approval(
        tools,
        AuditLog::default(),
        approvals.clone(),
    ));
    // The debugger rows start router-only; these tests call the debugger by name, so the
    // table of the page is not between them and the call.
    gateway.set_exposure(direct_exposure());
    (gateway, approvals)
}

/// Every tool on and callable by name: the debugger rows start router-only, and the tests
/// that call the debugger by name keep the table of the page out of their way.
pub(crate) fn direct_exposure() -> ToolExposure {
    let mut exposure = ToolExposure::default();
    for name in tool_gateway::TOOL_NAMES {
        exposure.set_state(
            name,
            tool_gateway::ToolState {
                enabled: true,
                router_only: false,
            },
        );
    }
    exposure
}

/// Calls a tool on its own thread, as the server does, and waits for the answer.
fn call(
    cx: &mut TestAppContext,
    gateway: &Arc<ToolGateway>,
    root: &Path,
    tool: &str,
    input: Value,
) -> Result<String, String> {
    let gateway = gateway.clone();
    let project = scope(root);
    let tool_name = tool.to_string();
    let asked = thread::spawn(move || {
        gateway
            .call(&project, &tool_name, &input)
            .map_err(|error| error.to_string())
    });
    wait_for(cx, "the answer of the tool", |_| asked.is_finished());
    asked.join().expect("the thread of the call")
}

fn wait_for_question(cx: &mut TestAppContext, approvals: &crate::mcp_approval::McpApprovals) {
    wait_for(cx, "the question of the tool", |_| {
        approvals.pending().is_some()
    });
}

fn answer(cx: &mut TestAppContext, reply: Reply) {
    let agents = cx.read(|cx| cx.global::<ActiveAgents>().0.clone());
    agents.update(cx, |panel, cx| panel.answer_mcp_question(reply, cx));
}

/// A call of the class `Execute`: it asks the user, and only then runs.
fn call_allowed(
    cx: &mut TestAppContext,
    gateway: &Arc<ToolGateway>,
    approvals: &Arc<crate::mcp_approval::McpApprovals>,
    root: &Path,
    tool: &str,
    input: Value,
    client: Option<&str>,
) -> Result<String, String> {
    let gateway = gateway.clone();
    let project = scope(root);
    let tool_name = tool.to_string();
    let client = client.map(str::to_string);
    let asked = thread::spawn(move || {
        gateway
            .call_from(client.as_deref(), &project, &tool_name, &input)
            .map_err(|error| error.to_string())
    });
    wait_for_question(cx, approvals);
    answer(cx, Reply::Once);
    wait_for(cx, "the answer of the tool", |_| asked.is_finished());
    asked.join().expect("the thread of the call")
}

/// A breakpoint of the tool is a breakpoint of the user: the Debug window lists it and
/// the gutter of the open editor shows it.
#[cfg(windows)]
#[ui_kit::gpui::test]
fn set_breakpoint_is_visible_in_the_editor_and_the_window(cx: &mut TestAppContext) {
    let setup = setup(cx, "mcp-debug-breakpoints");
    let path = setup.root.join("src").join("main.txt");
    open_path(cx, setup.window, &path);
    draw(cx, setup.window);
    let (gateway, approvals) = gateway(cx);

    let answer = call(
        cx,
        &gateway,
        &setup.root,
        "set_breakpoint",
        json!({"path": "src/main.txt", "line": 2, "condition": "x > 1"}),
    )
    .expect("the breakpoint is set");
    assert!(
        approvals.pending().is_none(),
        "a breakpoint asks no question"
    );
    assert!(answer.starts_with("breakpoint set: "), "{answer}");
    assert!(answer.contains("main.txt:2 on"), "{answer}");
    assert!(answer.contains("condition: x > 1"), "{answer}");

    let debug = debug_panel(cx);
    assert_eq!(
        debug.read_with(cx, |panel, _| panel.breakpoint_lines(&path)),
        vec![2],
        "the Debug window holds the breakpoint of the tool"
    );
    assert_eq!(
        debug.read_with(cx, |panel, _| panel.condition(&path, 2).map(str::to_owned)),
        Some("x > 1".to_string())
    );
    let view = editor_of(cx, setup.window, &path);
    draw(cx, setup.window);
    assert_eq!(
        view.read_with(cx, |view, _| view.breakpoints().to_vec()),
        [1],
        "the gutter of the editor shows the line of the breakpoint"
    );

    let listed = call(cx, &gateway, &setup.root, "list_breakpoints", json!({}))
        .expect("the breakpoints are listed");
    assert!(listed.starts_with("1 breakpoints:"), "{listed}");
    assert!(listed.contains("main.txt:2"), "{listed}");
    assert!(listed.contains("condition: x > 1"), "{listed}");

    let removed = call(
        cx,
        &gateway,
        &setup.root,
        "remove_breakpoint",
        json!({"path": "src/main.txt", "line": 2}),
    )
    .expect("the breakpoint goes away");
    assert!(removed.contains("breakpoint removed: "), "{removed}");
    assert!(
        debug
            .read_with(cx, |panel, _| panel.breakpoint_lines(&path))
            .is_empty(),
        "the window no longer holds it"
    );
}

/// The whole session of a tool: the question, the first stop on the breakpoint, the
/// stack and the values of the stop, an expression, a step and the end.
#[cfg(windows)]
#[ui_kit::gpui::test]
fn debug_run_after_allow_once_stops_on_the_breakpoint(cx: &mut TestAppContext) {
    let setup = setup(cx, "mcp-debug-run");
    let path = setup.root.join("src").join("main.txt");
    open_path(cx, setup.window, &path);
    draw(cx, setup.window);
    let (gateway, approvals) = gateway(cx);

    call(
        cx,
        &gateway,
        &setup.root,
        "set_breakpoint",
        json!({"path": "src/main.txt", "line": 2}),
    )
    .expect("the breakpoint is set");

    let run = call_allowed(
        cx,
        &gateway,
        &approvals,
        &setup.root,
        "debug_run",
        json!({"configuration": "Alpha", "wait_seconds": 20}),
        Some("Fixture Client"),
    )
    .expect("the session starts");
    assert!(run.starts_with("debugging Alpha: session "), "{run}");
    assert!(run.contains("debugging: stopped (breakpoint)"), "{run}");
    assert!(run.contains("thread: main #1"), "{run}");
    assert!(run.contains("#0 run"), "{run}");

    let requests = setup.service.requests();
    assert_eq!(
        last_breakpoint_lines(&requests, &path),
        Some(vec![2]),
        "the adapter got the breakpoint of the tool with the program"
    );
    let debug = debug_panel(cx);
    assert!(
        matches!(
            debug.read_with(cx, |panel, _| panel.snapshot().state.clone()),
            DebugState::Stopped { .. }
        ),
        "the Debug window shows the stop of the session"
    );
    assert!(
        debug.read_with(cx, |panel, _| panel
            .console_lines()
            .iter()
            .any(|line| line.contains("Started by"))),
        "the console marks the client that started the session"
    );

    let stack = call(
        cx,
        &gateway,
        &setup.root,
        "debug_stack",
        json!({"frames": 10}),
    )
    .expect("the stack of the stop");
    assert!(stack.starts_with("stack of main #1: 2 frames\n"), "{stack}");
    assert!(stack.contains("#0 run\n"), "{stack}");
    assert!(stack.contains("#1 start at "), "{stack}");

    let variables = call(
        cx,
        &gateway,
        &setup.root,
        "debug_variables",
        json!({"depth": 1, "length": 100}),
    )
    .expect("the variables of the stop");
    assert!(
        variables.starts_with("variables of frame #0\n"),
        "{variables}"
    );
    assert!(variables.contains("text = "), "{variables}");
    assert!(variables.contains("items = ArrayList (2)"), "{variables}");
    assert!(variables.contains("0 = "), "{variables}");

    let value = call_allowed(
        cx,
        &gateway,
        &approvals,
        &setup.root,
        "debug_evaluate",
        json!({"expression": "items.size()"}),
        None,
    )
    .expect("the expression is evaluated");
    assert_eq!(value, "items.size() = 2 (int)\n");

    let stops = debug.read_with(cx, |panel, _| panel.snapshot().stops);
    let step = call_allowed(
        cx,
        &gateway,
        &approvals,
        &setup.root,
        "debug_step",
        json!({"action": "over", "seconds": 20}),
        None,
    )
    .expect("the step reaches the next stop");
    assert!(step.contains("debugging: stopped (step)"), "{step}");
    assert!(
        debug.read_with(cx, |panel, _| panel.snapshot().stops) > stops,
        "the program stopped again after the step"
    );

    call_allowed(
        cx,
        &gateway,
        &approvals,
        &setup.root,
        "debug_stop",
        json!({}),
        None,
    )
    .expect("the session ends");
    wait_for(cx, "the end of the session", |cx| {
        debug.read_with(cx, |panel, _| panel.snapshot().state.clone()) == DebugState::Terminated
    });

    let state = call(cx, &gateway, &setup.root, "debug_get_state", json!({}))
        .expect("the state of the debugger");
    assert_eq!(state, "debugging is not running\n");
}

/// A test provider whose debug start announces a port at once, so the window attaches to
/// the mock adapter without a real test JVM.
struct DebuggableTests;

impl TestProvider for DebuggableTests {
    fn plan(&self, _: &ProjectModel, _: &TestRequest, _: &TestListen) -> base::Result<RunPlan> {
        Ok(RunPlan {
            before: None,
            program: ProcessSpec::new("cmd").args(["/d", "/c", "exit", "0"]),
            grace: Duration::from_secs(1),
        })
    }

    fn debug_plan(
        &self,
        _: &ProjectModel,
        _: &TestRequest,
        _: &TestListen,
        _: &runner::TestDebug,
    ) -> base::Result<RunPlan> {
        Ok(RunPlan {
            before: None,
            program: ProcessSpec::new("cmd").args([
                "/d",
                "/c",
                "echo Listening for transport dt_socket at address: 127.0.0.1:5005",
            ]),
            grace: Duration::from_secs(1),
        })
    }

    fn debug_ready(&self) -> Option<&'static str> {
        Some("Listening for transport dt_socket at address:")
    }
}

/// A debug service whose mock adapter moves the reported line one down at every step,
/// as a real debuggee moves between two statements.
struct MovingLineService {
    runtime: tokio::runtime::Runtime,
    server: Mutex<Option<lang_fixture::MockDap>>,
    source: PathBuf,
}

impl MovingLineService {
    fn new(source: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            runtime: tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("the test runtime starts"),
            server: Mutex::new(None),
            source,
        })
    }

    fn session(
        &self,
        name: String,
        breakpoints: Vec<BreakpointSpec>,
        watch: Vec<String>,
        exception_filters: Vec<String>,
    ) -> Result<Debugger, String> {
        let server = lang_fixture::MockDap::start_with_moving_line(self.source.to_string_lossy())
            .map_err(|error| error.to_string())?;
        let address = server.address().to_owned();
        *self
            .server
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(server);
        let (commands, receiver) = tokio::sync::mpsc::unbounded_channel();
        let (snapshots, incoming) = std::sync::mpsc::channel();
        let launch = Launch {
            address,
            adapter_id: "fixture".into(),
            name,
            breakpoints,
            exception_filters,
            watch,
            ..Launch::default()
        };
        self.runtime.spawn(async move {
            let _ = dap_client::run(launch, receiver, snapshots).await;
        });
        Ok(Debugger::new(commands, incoming))
    }
}

impl DebugService for MovingLineService {
    fn start(
        &self,
        _program: ProcessSpec,
        name: String,
        _entry: Option<PathBuf>,
        breakpoints: Vec<BreakpointSpec>,
        watch: Vec<String>,
        exception_filters: Vec<String>,
    ) -> Result<Debugger, String> {
        self.session(name, breakpoints, watch, exception_filters)
    }

    fn attach(
        &self,
        _host: String,
        _port: u16,
        name: String,
        _project: Option<String>,
        breakpoints: Vec<BreakpointSpec>,
        watch: Vec<String>,
        exception_filters: Vec<String>,
    ) -> Result<Debugger, String> {
        self.session(name, breakpoints, watch, exception_filters)
    }
}

/// A test of the project runs under the debugger the way Ctrl+Shift+F10 does, the tool
/// reads the stop of its session, and `over` moves the line of the stack.
#[cfg(windows)]
#[ui_kit::gpui::test]
fn debug_run_of_a_test_starts_the_test_under_the_debugger(cx: &mut TestAppContext) {
    let root = crate::window_tests::sample_project("mcp-debug-test");
    let mut project = run_setup(&root);
    project.tests = Some(Arc::new(DebuggableTests));
    project.debug = Some(MovingLineService::new(root.join("src").join("main.txt")));
    open_with(cx, &root, project);
    let runs = run_panel(cx);
    wait_for(cx, "the project to load", |cx| {
        runs.read_with(cx, |panel, _| panel.state().hint.is_none())
    });
    let (gateway, approvals) = gateway(cx);

    let run = call_allowed(
        cx,
        &gateway,
        &approvals,
        &root,
        "debug_run",
        json!({"suite": "demo.Vitest", "case": "fails", "wait_seconds": 20}),
        None,
    )
    .expect("the test starts under the debugger");
    assert!(
        run.starts_with("debugging demo.Vitest.fails: session "),
        "{run}"
    );
    assert!(run.contains("debugging: stopped (breakpoint)"), "{run}");

    let debug = debug_panel(cx);
    assert!(
        matches!(
            debug.read_with(cx, |panel, _| panel.snapshot().state.clone()),
            DebugState::Stopped { .. }
        ),
        "the Debug window shows the stop of the test session"
    );
    let line_of = |cx: &mut TestAppContext| {
        debug.read_with(cx, |panel, _| {
            panel
                .snapshot()
                .frames
                .iter()
                .find(|frame| frame.source.is_some())
                .map(|frame| frame.line)
        })
    };
    let before = line_of(cx).expect("a frame with a source");
    call_allowed(
        cx,
        &gateway,
        &approvals,
        &root,
        "debug_step",
        json!({"action": "over", "seconds": 20}),
        None,
    )
    .expect("the step reaches the next stop");
    let after = line_of(cx).expect("a frame with a source");
    assert_eq!(after, before + 1, "the step moved the line of the stack");

    call_allowed(
        cx,
        &gateway,
        &approvals,
        &root,
        "debug_stop",
        json!({}),
        None,
    )
    .expect("the session ends");
}

/// A question nobody answers starts nothing: the window has no session and the adapter
/// is never reached.
#[cfg(windows)]
#[ui_kit::gpui::test]
fn debug_run_without_an_answer_starts_nothing(cx: &mut TestAppContext) {
    let setup = setup(cx, "mcp-debug-deny");
    let (gateway, approvals) = gateway(cx);

    let gateway_call = gateway.clone();
    let project = scope(&setup.root);
    let asked = thread::spawn(move || {
        gateway_call
            .call(
                &project,
                "debug_run",
                &json!({"configuration": "Alpha", "wait_seconds": 1}),
            )
            .map_err(|error| error.to_string())
    });
    wait_for_question(cx, &approvals);
    answer(cx, Reply::Deny);
    wait_for(cx, "the refusal", |_| asked.is_finished());
    let refused = asked.join().expect("the thread of the call");

    assert!(
        matches!(&refused, Err(message) if message.starts_with("denied:")),
        "{refused:?}"
    );
    assert!(
        setup.service.requests().is_empty(),
        "the adapter never started"
    );
    let debug = debug_panel(cx);
    assert_eq!(
        debug.read_with(cx, |panel, _| panel.snapshot().state.clone()),
        DebugState::Inactive,
        "the window shows no session"
    );
    // The question of the call does not hold the window: a read tool still answers.
    let listed = call(cx, &gateway, &setup.root, "list_breakpoints", json!({}))
        .expect("the tools still answer");
    assert_eq!(listed, "no breakpoints\n");
}
