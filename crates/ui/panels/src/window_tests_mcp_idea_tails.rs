//! The tails of the IDEA MCP tools in the window: the overrides of one run, the file of
//! the whole output, the ignored files of `git_status`, a delete with Undo, and the
//! search that a time limit stops. The run providers and the language are substitutes; no
//! real client and no real project are started.

#![cfg(windows)]

use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use proc_sup::ProcessSpec;
use project_model::ProjectModel;
use runner::{ConfigField, RunConfig, RunConfigProvider, RunPlan};
use serde_json::{Value, json};
use tool_gateway::{
    Approval, ApprovalAnswer, ApprovalRequest, AuditLog, SessionScope, ToolGateway,
};
use ui_kit::Modifiers as ClickModifiers;
use ui_kit::gpui;
use ui_kit::gpui::{Entity, TestAppContext, VisualTestContext, WindowHandle};
use workspace::Workspace;

use crate::agents::{ActiveAgents, AgentLanguages};
use crate::window_tests::wait_for_signal;
use crate::window_tests::{buffer_of, draw, editor_of, open_path, sample_project, scratch};
use crate::window_tests_lang::{FakeLanguage, open_window_with, project};
use crate::window_tests_run::{open_with, run_panel, setup, wait_for};

/// The tools of the tests run for the user: every question is answered with yes.
struct Allow;

impl Approval for Allow {
    fn ask(&self, _: &ApprovalRequest) -> ApprovalAnswer {
        ApprovalAnswer::Once
    }
}

fn scope(root: &Path) -> SessionScope {
    SessionScope {
        session: "mcp-idea-tails".to_string(),
        project: root.to_path_buf(),
        cwd: root.to_path_buf(),
    }
}

/// A call of a tool that waits on the window, answered by its own thread.
fn call_in_background<T: Send + 'static>(
    cx: &mut TestAppContext,
    what: &str,
    call: impl FnOnce() -> T + Send + 'static,
) -> T {
    let answer = thread::spawn(call);
    wait_for(cx, what, |_| answer.is_finished());
    answer.join().expect("the call of the tool")
}

fn languages_gateway(cx: &TestAppContext) -> ToolGateway {
    let tools = cx.read(|cx| cx.global::<AgentLanguages>().0.clone());
    ToolGateway::with_approval(tools, AuditLog::default(), Arc::new(Allow))
}

fn window_tools_gateway(cx: &TestAppContext) -> ToolGateway {
    let tools = cx.read(|cx| cx.global::<crate::AgentTools>().0.clone());
    ToolGateway::with_approval(tools, AuditLog::default(), Arc::new(Allow))
}

/// A run configuration whose provider reads the arguments, the folder and the variables
/// of its settings, so an override of one run is visible in the output.
struct ArgumentsRun;

fn arguments_config() -> RunConfig {
    let mut config = RunConfig {
        name: "Arguments".to_string(),
        provider: "fixture".to_string(),
        assemble_first: false,
        settings: Default::default(),
        ..RunConfig::default()
    };
    config
        .settings
        .insert("program_args".to_string(), String::new().into());
    config
        .settings
        .insert("env".to_string(), "BASE=base".to_string().into());
    config
        .settings
        .insert("working_dir".to_string(), String::new().into());
    config
}

impl RunConfigProvider for ArgumentsRun {
    fn id(&self) -> &'static str {
        "fixture"
    }

    fn discover(&self, _: &ProjectModel) -> base::Result<Vec<RunConfig>> {
        Ok(vec![arguments_config()])
    }

    fn plan(&self, config: &RunConfig, _: &ProjectModel) -> base::Result<RunPlan> {
        let text = |key: &str| {
            config
                .settings
                .get(key)
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .to_string()
        };
        let arguments = text("program_args");
        let env = text("env");
        let directory = text("working_dir");
        let mut spec = ProcessSpec::new("cmd").args(["/d", "/c"]).arg(format!(
            "echo args={arguments} & echo BASE=%BASE% EXTRA=%EXTRA% & echo dir=%CD%"
        ));
        if !directory.is_empty() {
            spec = spec.cwd(directory);
        }
        for pair in env.split(';').filter(|pair| pair.contains('=')) {
            if let Some((name, value)) = pair.split_once('=') {
                spec = spec.env(name, value);
            }
        }
        Ok(RunPlan {
            before: None,
            program: spec,
            grace: Duration::from_secs(1),
        })
    }

    fn fields(&self, _config: &RunConfig) -> Vec<ConfigField> {
        vec![
            ConfigField::arguments("program_args", "Program arguments"),
            ConfigField::env("env", "Environment variables"),
            ConfigField::path("working_dir", "Working directory"),
        ]
    }

    fn read_field(&self, config: &RunConfig, key: &str) -> String {
        config
            .settings
            .get(key)
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string()
    }

    fn write_field(&self, config: &mut RunConfig, key: &str, value: &str) {
        config
            .settings
            .insert(key.to_string(), value.to_string().into());
    }
}

#[ui_kit::gpui::test]
fn one_run_takes_the_arguments_and_keeps_the_configuration_on_disk(cx: &mut TestAppContext) {
    let root = sample_project("mcp-run-overrides");
    let mut setup = setup(&root);
    setup.providers = vec![Arc::new(ArgumentsRun)];
    let window = open_with(cx, &root, setup);
    let panel = run_panel(cx);
    wait_for(cx, "the configuration of the provider", |cx| {
        !panel.read_with(cx, |panel, _| panel.state().configs.is_empty())
    });
    draw(cx, window);
    let config_file = root.join(".arro").join("run.toml");
    let before = fs::read(&config_file).ok();

    let gateway = languages_gateway(cx);
    let project = scope(&root);
    let listed = call_in_background(cx, "the list of the configurations", move || {
        gateway
            .call(&project, "get_run_configurations", &json!({}))
            .map_err(|error| error.to_string())
    })
    .expect("the list of the configurations");
    let listed: Value = serde_json::from_str(&listed).expect("the list is JSON");
    assert_eq!(
        listed["configurations"][0]["supportsDynamicLaunchOverrides"], true,
        "{listed}"
    );

    let gateway = languages_gateway(cx);
    let project = scope(&root);
    let directory = root.to_string_lossy().to_string();
    let answer = call_in_background(cx, "the run of the tool", move || {
        gateway
            .call(
                &project,
                "execute_run_configuration",
                &json!({
                    "configurationName": "Arguments",
                    "programArguments": "alpha beta",
                    "workingDirectory": directory,
                    "envs": ["EXTRA=one"],
                    "waitForExit": true,
                    "timeout": 20000,
                }),
            )
            .map_err(|error| error.to_string())
    })
    .expect("the run of the tool");
    let answer: Value = serde_json::from_str(&answer).expect("the run answer is JSON");
    assert_eq!(answer["exitCode"], 0, "{answer}");
    let output = answer["output"].as_str().unwrap_or_default();
    assert!(output.contains("args=alpha beta"), "{answer}");
    assert!(
        output.contains("BASE=base"),
        "the variable of the configuration stays: {answer}"
    );
    assert!(
        output.contains("EXTRA=one"),
        "the variable of the call arrives: {answer}"
    );
    assert!(
        output
            .to_lowercase()
            .contains(&root.to_string_lossy().to_lowercase()),
        "the folder of the call is the folder of the program: {answer}"
    );

    let path = answer["fullOutputPath"]
        .as_str()
        .expect("the answer names the whole log");
    let log = fs::read_to_string(path).expect("the file of the whole log");
    assert!(log.contains("args=alpha beta"), "{log}");
    assert!(log.contains("EXTRA=one"), "{log}");

    assert_eq!(
        fs::read(&config_file).ok(),
        before,
        "the configuration on disk did not change"
    );
}

fn repository(name: &str) -> std::path::PathBuf {
    let root = scratch(name);
    let git = vcs::GitRunner::from_env().expect("git is on the PATH");
    git.run(&root, ["init"]).unwrap();
    git.run(&root, ["config", "user.email", "tails@example.com"])
        .unwrap();
    git.run(&root, ["config", "user.name", "Tails Test"])
        .unwrap();
    fs::write(root.join(".gitignore"), "hidden.txt\n").unwrap();
    fs::write(root.join("shown.txt"), "shown\n").unwrap();
    git.run(&root, ["add", ".gitignore", "shown.txt"]).unwrap();
    git.run(&root, ["commit", "-m", "base"]).unwrap();
    fs::write(root.join("shown.txt"), "changed\n").unwrap();
    fs::write(root.join("hidden.txt"), "hidden\n").unwrap();
    root
}

#[ui_kit::gpui::test]
fn git_status_lists_an_ignored_file_only_when_asked(cx: &mut TestAppContext) {
    let root = repository("mcp-git-ignored");
    open_with(cx, &root, setup(&root));

    let gateway = window_tools_gateway(cx);
    let project = scope(&root);
    let without = call_in_background(cx, "the status without the ignored files", move || {
        gateway
            .call(&project, "git_status", &json!({"includeIgnored": false}))
            .map_err(|error| error.to_string())
    })
    .expect("the status without the ignored files");
    let gateway = window_tools_gateway(cx);
    let project = scope(&root);
    let with = call_in_background(cx, "the status with the ignored files", move || {
        gateway
            .call(&project, "git_status", &json!({"includeIgnored": true}))
            .map_err(|error| error.to_string())
    })
    .expect("the status with the ignored files");

    assert!(
        without.contains("shown.txt"),
        "the tracked file is there: {without}"
    );
    assert!(
        !without.contains("hidden.txt"),
        "the ignored file waits for the flag: {without}"
    );
    assert!(
        with.contains("hidden.txt"),
        "the ignored file is there: {with}"
    );
}

/// The gateway of the window over its own tools, with the questions answered by the test.
fn edit_gateway(cx: &TestAppContext) -> ToolGateway {
    let tools = cx.read(|cx| cx.global::<crate::AgentTools>().0.clone());
    ToolGateway::with_approval(tools, AuditLog::default(), Arc::new(Allow))
}

fn sync(cx: &mut TestAppContext) {
    let panel = cx.update(|cx| cx.global::<ActiveAgents>().0.clone());
    panel.update(cx, |panel, cx| panel.sync_tools(cx));
}

fn click_hunk_mark(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    view: &Entity<editor_view::EditorView>,
    line: usize,
) {
    wait_for_signal(cx, "the mark of the deletion", |cx| {
        draw(cx, window);
        view.read_with(cx, |view, _| view.agent_mark_bounds(line).is_some())
    });
    let bounds = view
        .read_with(cx, |view, _| view.agent_mark_bounds(line))
        .expect("the mark is on screen");
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_click(bounds.center(), ClickModifiers::default());
    drop(visual);
    cx.run_until_parked();
    draw(cx, window);
    wait_for_signal(cx, "the window of the hunk", |cx| {
        draw(cx, window);
        let mut visual = VisualTestContext::from_window(window.into(), cx);
        visual.debug_bounds("agent-hunk-popup").is_some()
    });
}

type Answer = Arc<Mutex<Option<Result<String, String>>>>;

fn ask_in_background(
    gateway: ToolGateway,
    scope: SessionScope,
    tool: &'static str,
    input: Value,
) -> Answer {
    let answer: Answer = Arc::new(Mutex::new(None));
    let slot = answer.clone();
    thread::spawn(move || {
        let result = gateway
            .call(&scope, tool, &input)
            .map_err(|error| error.to_string());
        *slot.lock().unwrap_or_else(|poison| poison.into_inner()) = Some(result);
    });
    answer
}

fn take_answer(answer: &Answer) -> Result<String, String> {
    answer
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .clone()
        .expect("the tool has answered")
}

fn answered(answer: &Answer) -> bool {
    answer
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .is_some()
}

#[ui_kit::gpui::test]
fn a_patch_delete_goes_through_the_review_and_undo_returns_the_file(cx: &mut TestAppContext) {
    let text = "only line\n";
    let root = project("mcp-delete-undo", &[("src/Main.txt", text)]);
    let main = root.join("src").join("Main.txt");
    let language = FakeLanguage::new();
    let window = open_window_with(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &main);
    let view = editor_of(cx, window, &main);
    sync(cx);

    let answer = ask_in_background(
        edit_gateway(cx),
        scope(&root),
        "apply_patch",
        json!({"input": "*** Begin Patch\n*** Delete File: src/Main.txt\n*** End Patch\n"}),
    );
    click_hunk_mark(cx, window, &view, 0);
    crate::window_tests::click(cx, window, "agent-hunk-accept");
    draw(cx, window);
    wait_for(cx, "the answer of the delete", |cx| {
        draw(cx, window);
        answered(&answer)
    });
    let answer = take_answer(&answer).expect("the delete is accepted");
    assert!(answer.contains("accepted"), "{answer}");
    wait_for(cx, "the file to go away", |_| !main.exists());
    assert_eq!(
        buffer_of(cx, &view),
        text,
        "the editor keeps the text it had"
    );

    wait_for(cx, "the Undo of the delete", |cx| {
        draw(cx, window);
        window
            .read_with(cx, |workspace, _| workspace.last_notification())
            .ok()
            .flatten()
            .is_some_and(|(title, _)| title.as_ref() == "Delete")
    });
    crate::window_tests::click(cx, window, "notification-action");
    wait_for(cx, "the file the Undo brings back", |_| {
        fs::read_to_string(&main).is_ok_and(|restored| restored == text)
    });
}
