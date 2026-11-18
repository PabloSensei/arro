//! The MCP tools that build, test and run the project: the question they ask the user,
//! the answer of the Run window and the output of a run. The build tool and the tests are
//! substitutes that answer at once; no window and no real project are needed.

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]

use std::io::Write;
use std::net::TcpStream;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use base::Cancel;
use proc_sup::ProcessSpec;
use project_model::{BuildSystem, ProjectModel};
use runner::{RunPlan, TestListen, TestProvider, TestRequest};
use serde_json::json;
use tool_gateway::{AuditLog, SessionScope, ToolGateway};
use ui_kit::gpui;
use ui_kit::gpui::TestAppContext;

use crate::agents::{ActiveAgents, AgentLanguages};
use crate::mcp_approval::Reply;
use crate::window_tests::sample_project;
use crate::window_tests_run::{EchoRun, open_with, run_panel, setup, wait_for};

/// The events of one test class: a passing case and a failing one. The same shape as the
/// listener of the Tests window takes.
const TEST_EVENTS: &[&str] = &[
    r#"{"event":"plan"}"#,
    r#"{"event":"started","id":"e","name":"Engine","kind":"suite"}"#,
    r#"{"event":"started","id":"e/c","parent":"e","name":"Vitest","kind":"suite","suite":"demo.Vitest"}"#,
    r#"{"event":"started","id":"e/c/passes","parent":"e/c","name":"passes()","kind":"case","suite":"demo.Vitest","case":"passes"}"#,
    r#"{"event":"finished","id":"e/c/passes","outcome":"passed","durationMs":2}"#,
    r#"{"event":"started","id":"e/c/fails","parent":"e/c","name":"fails()","kind":"case","suite":"demo.Vitest","case":"fails"}"#,
    r#"{"event":"finished","id":"e/c/fails","outcome":"failed","message":"expected 1, got 2","trace":"AssertionFailedError: expected\n\tat demo.Vitest.fails(Vitest.code:2)","durationMs":3}"#,
    r#"{"event":"planFinished"}"#,
];

/// A build tool of the scratch project whose build prints a compile error and fails.
struct FailingTool(ProjectModel);

impl BuildSystem for FailingTool {
    fn detect(&self, _: &Path) -> bool {
        true
    }

    fn scan(&self, _: &Path) -> base::Result<ProjectModel> {
        Ok(self.0.clone())
    }

    fn sync(&self, _: &Path, _: &Cancel) -> base::Result<ProjectModel> {
        Ok(self.0.clone())
    }

    fn assemble(&self, _: &Path) -> base::Result<ProcessSpec> {
        Ok(ProcessSpec::new("cmd").args([
            "/d",
            "/c",
            "echo src/Foo.txt:3: cannot find symbol & exit 1",
        ]))
    }
}

fn failing_tool(root: &Path) -> Arc<dyn BuildSystem> {
    Arc::new(FailingTool(ProjectModel {
        root: root.to_path_buf(),
        tool: "fixture".into(),
        resolved: true,
        ..ProjectModel::default()
    }))
}

/// A test provider that plays the listener from a thread, as the build tool would.
struct FixtureTests {
    requests: Arc<Mutex<Vec<TestRequest>>>,
}

impl TestProvider for FixtureTests {
    fn plan(
        &self,
        _: &ProjectModel,
        request: &TestRequest,
        listen: &TestListen,
    ) -> base::Result<RunPlan> {
        self.requests
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(request.clone());
        let listen = listen.clone();
        let Ok(mut stream) = TcpStream::connect(("127.0.0.1", listen.port)) else {
            return Ok(RunPlan {
                before: None,
                program: ProcessSpec::new("cmd").args(["/d", "/c", "exit", "0"]),
                grace: Duration::from_secs(1),
            });
        };
        let _ = writeln!(stream, "{}", listen.token);
        thread::spawn(move || {
            for line in TEST_EVENTS {
                if writeln!(stream, "{line}").is_err() {
                    return;
                }
            }
        });
        Ok(RunPlan {
            before: None,
            program: ProcessSpec::new("cmd").args(["/d", "/c", "exit", "0"]),
            grace: Duration::from_secs(1),
        })
    }
}

fn scope(root: &Path) -> SessionScope {
    SessionScope {
        session: "mcp-runs".to_string(),
        project: root.to_path_buf(),
        cwd: root.to_path_buf(),
    }
}

/// The gateway of the window, over the language tools the window filled in.
fn gateway(cx: &mut TestAppContext) -> (Arc<ToolGateway>, Arc<crate::mcp_approval::McpApprovals>) {
    let agents = cx.read(|cx| cx.global::<ActiveAgents>().0.clone());
    let approvals = agents.read_with(cx, |panel, _| panel.approvals.clone());
    let tools = cx.read(|cx| cx.global::<AgentLanguages>().0.clone());
    let gateway = Arc::new(ToolGateway::with_approval(
        tools,
        AuditLog::default(),
        approvals.clone(),
    ));
    (gateway, approvals)
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

fn wait_for_call<T: Send + 'static>(
    cx: &mut TestAppContext,
    what: &str,
    join: thread::JoinHandle<T>,
) -> T {
    wait_for(cx, what, |_| join.is_finished());
    join.join().expect("the call")
}

fn output_id(answer: &str) -> String {
    answer
        .lines()
        .find_map(|line| line.strip_prefix("output: "))
        .unwrap_or_else(|| panic!("the answer names the output: {answer}"))
        .to_string()
}

/// The MCP page of the settings offers "Always ask" and "Allow for trusted projects".
#[test]
fn the_mcp_page_offers_both_permissions() {
    use crate::settings_catalog::{self, SettingKind, SettingsPage};
    use workspace::McpExecutePermission;

    let setting = settings_catalog::find("mcp_execute_permission").expect("the setting");
    assert_eq!(setting.page, SettingsPage::Mcp);
    assert_eq!(setting.kind, SettingKind::Choice);

    let mut settings = workspace::Settings::default();
    assert_eq!(
        settings.mcp_execute_permission,
        McpExecutePermission::AlwaysAsk
    );
    assert_eq!(
        settings_catalog::choice_label(&settings, "mcp_execute_permission").as_deref(),
        Some("Always ask")
    );
    assert!(settings_catalog::next_choice(
        &mut settings,
        "mcp_execute_permission"
    ));
    assert_eq!(
        settings_catalog::choice_label(&settings, "mcp_execute_permission").as_deref(),
        Some("Allow for trusted projects")
    );
}

/// Allow once: the tests run in the Tests window, and the answer has the one that passed
/// and the one that failed, with the message and the stack of the failure.
#[cfg(windows)]
#[ui_kit::gpui::test]
fn run_tests_after_allow_once_answers_both_cases(cx: &mut TestAppContext) {
    let root = sample_project("mcp-runs-tests");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let mut setup = setup(&root);
    setup.tests = Some(Arc::new(FixtureTests {
        requests: requests.clone(),
    }));
    open_with(cx, &root, setup);
    let panel = run_panel(cx);
    let (gateway, approvals) = gateway(cx);

    let calling = gateway.clone();
    let project = scope(&root);
    let call = thread::spawn(move || {
        calling
            .call(&project, "run_tests", &json!({"suite": "demo.Vitest"}))
            .map_err(|error| error.to_string())
    });
    wait_for_question(cx, &approvals);
    answer(cx, Reply::Once);
    let answer = wait_for_call(cx, "the answer of the tests", call).expect("the tests answer");

    assert!(answer.contains("1 passed, 1 failed, 0 skipped"), "{answer}");
    assert!(answer.contains("demo.Vitest.fails"), "{answer}");
    assert!(answer.contains("expected 1, got 2"), "{answer}");
    assert!(answer.contains("at demo.Vitest.fails"), "{answer}");
    assert_eq!(
        requests.lock().unwrap().len(),
        1,
        "the request reached the test provider"
    );
    let report = panel.read_with(cx, |panel, _| panel.test_report());
    assert!(
        report
            .iter()
            .any(|line| line.contains("passed") && line.contains("passes")),
        "the Tests window shows the passed case: {report:?}"
    );
    assert!(
        report
            .iter()
            .any(|line| line.contains("failed") && line.contains("fails")),
        "the Tests window shows the failed case: {report:?}"
    );

    let id = output_id(&answer);
    let calling = gateway.clone();
    let project = scope(&root);
    let asked = id.clone();
    let output = wait_for_call(
        cx,
        "the output of the tests",
        thread::spawn(move || {
            calling
                .call(&project, "get_run_output", &json!({"id": asked}))
                .map_err(|error| error.to_string())
        }),
    )
    .expect("the output of the run");
    assert!(output.contains(&id), "{output}");
}

/// Deny: the answer is a refusal and the test provider never sees the request.
#[cfg(windows)]
#[ui_kit::gpui::test]
fn deny_refuses_the_tests_and_starts_nothing(cx: &mut TestAppContext) {
    let root = sample_project("mcp-runs-deny");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let mut setup = setup(&root);
    setup.tests = Some(Arc::new(FixtureTests {
        requests: requests.clone(),
    }));
    open_with(cx, &root, setup);
    let panel = run_panel(cx);
    let (gateway, approvals) = gateway(cx);

    let project = scope(&root);
    let call = thread::spawn(move || {
        gateway
            .call(&project, "run_tests", &json!({}))
            .map_err(|error| error.to_string())
    });
    wait_for_question(cx, &approvals);
    answer(cx, Reply::Deny);
    let answer = wait_for_call(cx, "the refusal", call).expect_err("the refusal");

    assert!(answer.starts_with("denied:"), "{answer}");
    assert!(
        requests.lock().unwrap().is_empty(),
        "nothing reached the test provider"
    );
    assert!(
        panel.read_with(cx, |panel, _| panel.idle()),
        "the window started nothing"
    );
}

/// A build with an error answers the verdict, the error and the output, and the Run
/// window shows the console of the build.
#[cfg(windows)]
#[ui_kit::gpui::test]
fn build_project_answers_the_compile_error(cx: &mut TestAppContext) {
    let root = sample_project("mcp-runs-build");
    let mut setup = setup(&root);
    setup.tool = Some(failing_tool(&root));
    open_with(cx, &root, setup);
    let (gateway, approvals) = gateway(cx);

    let project = scope(&root);
    let call = thread::spawn(move || {
        gateway
            .call(&project, "build_project", &json!({}))
            .map_err(|error| error.to_string())
    });
    wait_for_question(cx, &approvals);
    answer(cx, Reply::Once);
    let answer = wait_for_call(cx, "the answer of the build", call).expect("the build answer");

    assert!(answer.contains("build of the project failed"), "{answer}");
    assert!(
        answer.contains("src/Foo.txt:3: cannot find symbol"),
        "{answer}"
    );
    assert!(answer.contains("output: "), "{answer}");
}

/// A run configuration of the window starts from the tool, prints its line, and
/// `get_run_output` reads it with the exit code.
#[cfg(windows)]
#[ui_kit::gpui::test]
fn run_configuration_starts_and_its_output_is_readable(cx: &mut TestAppContext) {
    let root = sample_project("mcp-runs-output");
    let mut setup = setup(&root);
    setup.providers = vec![Arc::new(EchoRun)];
    open_with(cx, &root, setup);
    let panel = run_panel(cx);
    wait_for(cx, "a run configuration", |cx| {
        !panel.read_with(cx, |panel, _| panel.state().configs.is_empty())
    });
    let (gateway, approvals) = gateway(cx);

    let calling = gateway.clone();
    let project = scope(&root);
    let call = thread::spawn(move || {
        calling
            .call_from(
                Some("Fixture Client"),
                &project,
                "run_configuration",
                &json!({"name": "Echo"}),
            )
            .map_err(|error| error.to_string())
    });
    wait_for_question(cx, &approvals);
    answer(cx, Reply::Once);
    let answer = wait_for_call(cx, "the start of the run", call).expect("the run starts");
    let id = answer
        .strip_prefix("started Echo: ")
        .expect("the answer names the run")
        .trim()
        .to_string();
    assert!(
        panel
            .read_with(cx, |panel, _| panel.text())
            .iter()
            .any(|line| line.contains("Started by Fixture Client")),
        "the window marks the run of the client"
    );

    wait_for(cx, "the program to finish", |cx| {
        !panel.read_with(cx, |panel, _| panel.state().running)
            && panel
                .read_with(cx, |panel, _| panel.text())
                .iter()
                .any(|line| line.contains("exit code"))
    });
    let output = wait_for_call(cx, "the output of the run", {
        let calling = gateway.clone();
        let project = scope(&root);
        thread::spawn(move || {
            calling
                .call(&project, "get_run_output", &json!({"id": id}))
                .map_err(|error| error.to_string())
        })
    })
    .expect("the output of the run");

    assert!(output.contains("hello from run"), "{output}");
    assert!(output.contains("exit code 0"), "{output}");
}
