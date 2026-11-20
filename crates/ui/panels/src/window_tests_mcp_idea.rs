//! The question of a command of the terminal: without brave mode the window asks the user,
//! and a refusal reaches the agent as an error of the tool.

#![cfg(windows)]

use std::sync::Arc;
use std::thread;

use tool_gateway::{AuditLog, SessionScope, ToolGateway};
use ui_kit::gpui;
use ui_kit::gpui::TestAppContext;

use crate::agents::ActiveAgents;
use crate::mcp_approval::Reply;
use crate::window_tests::{open_sample_window, wait_for_signal};

fn gateway(cx: &TestAppContext) -> (Arc<ToolGateway>, Arc<crate::mcp_approval::McpApprovals>) {
    let agents = cx.read(|cx| cx.global::<ActiveAgents>().0.clone());
    let approvals = agents.read_with(cx, |panel, _| panel.approvals.clone());
    let tools = cx.read(|cx| cx.global::<crate::AgentTools>().0.clone());
    let gateway = Arc::new(ToolGateway::with_approval(
        tools,
        AuditLog::default(),
        approvals.clone(),
    ));
    (gateway, approvals)
}

fn scope(root: &std::path::Path) -> SessionScope {
    SessionScope {
        session: "window-tests-mcp-idea".to_string(),
        project: root.to_path_buf(),
        cwd: root.to_path_buf(),
    }
}

#[ui_kit::gpui::test]
fn a_terminal_command_asks_and_a_refusal_is_an_error(cx: &mut TestAppContext) {
    let (root, _window) = open_sample_window(cx, "mcp-idea-terminal");
    let (gateway, approvals) = gateway(cx);

    let calling = gateway.clone();
    let project = root.clone();
    let call = thread::spawn(move || {
        calling.call(
            &scope(&project),
            "execute_terminal_command",
            &serde_json::json!({"command": "echo hello", "timeout": 5000}),
        )
    });
    wait_for_signal(cx, "the question of the terminal tool", |_| {
        approvals.pending().is_some()
    });
    let shown = approvals.pending().expect("a question");
    assert_eq!(shown.tool, "execute_terminal_command");
    assert_eq!(approvals.pending_detail().as_deref(), Some("echo hello"));

    let agents = cx.read(|cx| cx.global::<ActiveAgents>().0.clone());
    agents.update(cx, |panel, cx| panel.answer_mcp_question(Reply::Deny, cx));
    wait_for_signal(cx, "the answer of the user", |_| call.is_finished());
    let answer = call.join().expect("the call");
    let error = answer.expect_err("a refusal is an error");
    assert!(error.to_string().contains("refused"), "{error}");
}
