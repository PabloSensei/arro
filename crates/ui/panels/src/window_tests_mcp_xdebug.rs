//! The session tools of the debugger that IDEA's server names `xdebug_*`: a point an
//! agent sets is marked and painted apart, and the session of the window answers a wait
//! for a pause with the values of the frame. The mock adapter of the fixture language
//! plays the debuggee; no window and no real project are needed.

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]

use std::path::Path;
use std::sync::Arc;
use std::thread;

use serde_json::{Value, json};
use tool_gateway::{AuditLog, BreakpointOwner, SessionScope, ToolGateway};
use ui_kit::gpui;
use ui_kit::gpui::TestAppContext;

use crate::agents::AgentLanguages;
use crate::mcp_approval::Reply;
use crate::window_tests::{draw, editor_of, open_path};
use crate::window_tests_debug_actions::setup;
use crate::window_tests_run::wait_for;

fn scope(root: &Path) -> SessionScope {
    SessionScope {
        session: "mcp-xdebug".to_string(),
        project: root.to_path_buf(),
        cwd: root.to_path_buf(),
    }
}

fn gateway(cx: &mut TestAppContext) -> (Arc<ToolGateway>, Arc<crate::mcp_approval::McpApprovals>) {
    let agents = cx.read(|cx| cx.global::<crate::agents::ActiveAgents>().0.clone());
    let approvals = agents.read_with(cx, |panel, _| panel.approvals.clone());
    let tools = cx.read(|cx| cx.global::<AgentLanguages>().0.clone());
    let gateway = Arc::new(ToolGateway::with_approval(
        tools,
        AuditLog::default(),
        approvals.clone(),
    ));
    gateway.set_exposure(crate::window_tests_mcp_debug::direct_exposure());
    (gateway, approvals)
}

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

fn call_allowed(
    cx: &mut TestAppContext,
    gateway: &Arc<ToolGateway>,
    approvals: &Arc<crate::mcp_approval::McpApprovals>,
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
    wait_for(cx, "the question of the tool", |_| {
        approvals.pending().is_some()
    });
    let agents = cx.read(|cx| cx.global::<crate::agents::ActiveAgents>().0.clone());
    agents.update(cx, |panel, cx| panel.answer_mcp_question(Reply::Once, cx));
    wait_for(cx, "the answer of the tool", |_| asked.is_finished());
    asked.join().expect("the thread of the call")
}

#[cfg(windows)]
#[ui_kit::gpui::test]
fn an_agent_point_is_marked_and_taken_away_by_its_owner(cx: &mut TestAppContext) {
    let setup = setup(cx, "mcp-xdebug-owner");
    let path = setup.root.join("src").join("main.txt");
    open_path(cx, setup.window, &path);
    draw(cx, setup.window);
    let (gateway, approvals) = gateway(cx);

    let set = call(
        cx,
        &gateway,
        &setup.root,
        "xdebug_set_breakpoint",
        json!({"filePath": "src/main.txt", "line": 2, "condition": "x > 1"}),
    )
    .expect("the point is set");
    assert!(approvals.pending().is_none(), "a point asks no question");
    assert!(set.contains("\"owner\": \"agent\""), "{set}");
    assert!(set.contains("\"line\": 2"), "{set}");
    let debug = crate::window_tests_debug_actions::debug_panel(cx);
    assert_eq!(
        debug.read_with(cx, |panel, _| panel.breakpoint_owner(&path, 2)),
        Some(BreakpointOwner::Agent)
    );
    let view = editor_of(cx, setup.window, &path);
    draw(cx, setup.window);
    assert_eq!(
        view.read_with(cx, |view, _| view.agent_breakpoints().to_vec()),
        [1],
        "the gutter paints the point of the agent apart"
    );

    let listed = call(
        cx,
        &gateway,
        &setup.root,
        "xdebug_list_breakpoints",
        json!({}),
    )
    .expect("the points are listed");
    assert!(listed.contains("\"owner\": \"agent\""), "{listed}");
    assert!(listed.contains("main.txt:2"), "{listed}");

    let removed = call(
        cx,
        &gateway,
        &setup.root,
        "xdebug_remove_breakpoint",
        json!({"owner": "agent"}),
    )
    .expect("the points of the agent go away");
    assert!(removed.contains("\"removed\": 1"), "{removed}");
    assert!(
        debug
            .read_with(cx, |panel, _| panel.breakpoint_lines(&path))
            .is_empty(),
        "the window no longer holds the point"
    );
}

#[cfg(windows)]
#[ui_kit::gpui::test]
fn a_wait_for_pause_answers_the_frame_values_of_the_stop(cx: &mut TestAppContext) {
    let setup = setup(cx, "mcp-xdebug-pause");
    let path = setup.root.join("src").join("main.txt");
    open_path(cx, setup.window, &path);
    draw(cx, setup.window);
    let (gateway, approvals) = gateway(cx);

    call(
        cx,
        &gateway,
        &setup.root,
        "xdebug_set_breakpoint",
        json!({"filePath": "src/main.txt", "line": 2}),
    )
    .expect("the point is set");
    let started = call_allowed(
        cx,
        &gateway,
        &approvals,
        &setup.root,
        "xdebug_start_debugger_session",
        json!({"configurationName": "Alpha", "timeout": 20000, "graceWaitMs": 20000}),
    )
    .expect("the session starts");
    assert!(started.contains("\"sessionId\""), "{started}");
    assert!(started.contains("\"status\":"), "{started}");

    let status = call(
        cx,
        &gateway,
        &setup.root,
        "xdebug_get_debugger_status",
        json!({}),
    )
    .expect("the status of the debugger");
    assert!(status.contains("\"paused\""), "{status}");

    let paused = call_allowed(
        cx,
        &gateway,
        &approvals,
        &setup.root,
        "xdebug_control_session",
        json!({"action": "WAIT_FOR_PAUSE", "timeout": 20000}),
    )
    .expect("the wait answers");
    assert!(paused.contains("\"paused\": true"), "{paused}");
    assert!(paused.contains("\"status\": \"paused\""), "{paused}");
    assert!(paused.contains("\"frameValues\""), "{paused}");
    assert!(paused.contains("\"name\": \"items\""), "{paused}");

    let threads = call(
        cx,
        &gateway,
        &setup.root,
        "xdebug_get_threads",
        json!({"limit": 10}),
    )
    .expect("the threads of the session");
    assert!(threads.contains("\"name\": \"main\""), "{threads}");
    assert!(threads.contains("\"isCurrent\": true"), "{threads}");

    let stack = call(
        cx,
        &gateway,
        &setup.root,
        "xdebug_get_stack",
        json!({"threadId": "main", "limit": 10}),
    )
    .expect("the stack of the thread");
    assert!(stack.contains("\"presentation\": \"run\""), "{stack}");

    let value = call(
        cx,
        &gateway,
        &setup.root,
        "xdebug_get_value_by_path",
        json!({"path": ["items", "[0]"]}),
    )
    .expect("the value of the path");
    assert!(value.contains("\"name\": \"0\""), "{value}");
    assert!(value.contains("\"value\": \"\\\"a\\\"\""), "{value}");

    // STOP only cleans up: on a loaded machine the stand-in program may have ended already.
    let stopped = call_allowed(
        cx,
        &gateway,
        &approvals,
        &setup.root,
        "xdebug_control_session",
        json!({"action": "STOP"}),
    );
    if let Err(error) = &stopped {
        assert!(
            error.contains("debugging is not running"),
            "the session ends: {error}"
        );
    }
}
