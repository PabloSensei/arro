//! The text of an open file as an agent reads it. The window publishes the document
//! itself and no copy of its text, so a tool takes the text when it asks, and an edit
//! that came after the last frame is in the answer.
//!
//! The stand-in language needs no server.

use serde_json::json;
use tool_gateway::{AuditLog, SessionScope, ToolGateway};
use ui_kit::gpui;
use ui_kit::gpui::TestAppContext;

use crate::window_tests::{buffer_of, draw, editor_of, open_path};
use crate::window_tests_lang::{FakeLanguage, open_window_with, project};

/// The gateway of the window, as the MCP server builds it for a session.
fn gateway(cx: &TestAppContext) -> ToolGateway {
    let tools = cx.read(|cx| cx.global::<crate::AgentTools>().0.clone());
    ToolGateway::new(tools, AuditLog::default())
}

fn scope(root: &std::path::Path) -> SessionScope {
    SessionScope {
        session: "window-tests-ai-tools-text".to_string(),
        project: root.to_path_buf(),
        cwd: root.to_path_buf(),
    }
}

#[ui_kit::gpui::test]
fn read_file_takes_the_text_when_the_tool_asks(cx: &mut TestAppContext) {
    let root = project("ai-tools-lazy-text", &[("src/Main.txt", "one\ntwo\n")]);
    let main = root.join("src").join("Main.txt");
    let language = FakeLanguage::new();
    let window = open_window_with(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &main);
    let view = editor_of(cx, window, &main);

    // The frame publishes the document, and the edit follows with no frame in between.
    let panel = cx.update(|cx| cx.global::<crate::agents::ActiveAgents>().0.clone());
    panel.update(cx, |panel, cx| panel.sync_tools(cx));
    view.update(cx, |view, cx| view.replace_text(0..3, "ONE", cx));
    assert_eq!(buffer_of(cx, &view), "ONE\ntwo\n");

    let answer = gateway(cx)
        .call(
            &scope(&root),
            "read_file",
            &json!({"path": "src/Main.txt", "start_line": 1, "end_line": 1}),
        )
        .expect("read_file answers");

    assert!(
        answer.contains("1|ONE"),
        "the text is read when the tool asks: {answer}"
    );
    assert!(
        answer.contains("lines 1-1 of 2, unsaved changes"),
        "the unsaved state comes from the document too: {answer}"
    );
}
