//! The edit tools an MCP client asks the window for: a proposed file the user decides hunk
//! by hunk, a rename the user confirms, and Organize Imports that runs at once. The
//! stand-in language and the window without a window need no server and no process.

use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};

use lang_api::{ActionEntry, ActionKind, SourceEdit};
use serde_json::{Value, json};
use tool_gateway::{
    Approval, ApprovalAnswer, ApprovalRequest, AuditLog, SessionScope, ToolGateway,
};
use ui_kit::Modifiers as ClickModifiers;
use ui_kit::gpui;
use ui_kit::gpui::{Entity, TestAppContext, VisualTestContext, WindowHandle};
use workspace::Workspace;

use crate::window_tests::{buffer_of, click, draw, editor_of, open_path, wait_for_signal};
use crate::window_tests_lang::{FakeLanguage, edit_in, open_window_with, project, uri_of};
use crate::window_tests_ui::wait_for;

const BASE: &str = "alpha\nbeta\ngamma\ndelta\nepsilon\n";
const CHANGED: &str = "ALPHA\nbeta\ngamma\ndelta\nEPSILON\n";
/// The base with only the first hunk of `CHANGED` taken in.
const FIRST_ACCEPTED: &str = "ALPHA\nbeta\ngamma\ndelta\nepsilon\n";

/// The permission of an `Execute` tool: these tests are the user, so the answer is yes.
struct Allow;

impl Approval for Allow {
    fn ask(&self, _: &ApprovalRequest) -> ApprovalAnswer {
        ApprovalAnswer::Once
    }
}

fn gateway(cx: &TestAppContext) -> ToolGateway {
    let tools = cx.read(|cx| cx.global::<crate::AgentTools>().0.clone());
    ToolGateway::with_approval(tools, AuditLog::default(), Arc::new(Allow))
}

/// The gateway the window builds for a session: its questions reach the notification.
fn window_gateway(cx: &TestAppContext) -> ToolGateway {
    let tools = cx.read(|cx| cx.global::<crate::AgentTools>().0.clone());
    let panel = cx.read(|cx| cx.global::<crate::agents::ActiveAgents>().0.clone());
    let approvals = panel.read_with(cx, |panel, _| panel.approvals.clone());
    ToolGateway::with_approval(tools, AuditLog::default(), approvals)
}

fn scope(root: &Path) -> SessionScope {
    SessionScope {
        session: "window-tests-mcp-edits".to_string(),
        project: root.to_path_buf(),
        cwd: root.to_path_buf(),
    }
}

/// The window writes what the tools read: called every frame and by the tests.
fn sync(cx: &mut TestAppContext) {
    let panel = cx.update(|cx| cx.global::<crate::agents::ActiveAgents>().0.clone());
    panel.update(cx, |panel, cx| panel.sync_tools(cx));
}

/// The answer of a tool that waits on the UI thread, filled by its own thread.
type Answer = Arc<Mutex<Option<Result<String, String>>>>;

fn ask_in_background(
    gateway: ToolGateway,
    scope: SessionScope,
    tool: &'static str,
    input: Value,
) -> Answer {
    let answer: Answer = Arc::new(Mutex::new(None));
    let slot = answer.clone();
    std::thread::spawn(move || {
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

/// Clicks the mark on `line` and waits for the window of its hunk.
fn open_hunk(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    view: &Entity<editor_view::EditorView>,
    line: usize,
) {
    wait_for_signal(cx, "the mark on the line", |cx| {
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

#[ui_kit::gpui::test]
fn a_proposed_file_waits_and_keeps_what_was_accepted(cx: &mut TestAppContext) {
    let root = project("mcp-edits-propose", &[("src/Main.txt", BASE)]);
    let main = root.join("src").join("Main.txt");
    let language = FakeLanguage::new();
    let window = open_window_with(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &main);
    let view = editor_of(cx, window, &main);
    sync(cx);

    let answer = ask_in_background(
        gateway(cx),
        scope(&root),
        "propose_edit",
        json!({"path": "src/Main.txt", "new_text": CHANGED}),
    );
    wait_for_signal(cx, "the marks of the proposal", |cx| {
        draw(cx, window);
        view.read_with(cx, |view, _| view.agent_marks().len() == 2)
    });
    assert_eq!(
        fs::read_to_string(&main).unwrap(),
        BASE,
        "nothing is written while a hunk waits"
    );

    open_hunk(cx, window, &view, 0);
    click(cx, window, "agent-hunk-accept");
    draw(cx, window);
    wait_for_signal(cx, "one hunk left", |cx| {
        draw(cx, window);
        view.read_with(cx, |view, _| view.agent_marks().len() == 1)
    });
    assert_eq!(
        fs::read_to_string(&main).unwrap(),
        BASE,
        "nothing is written while a hunk still waits"
    );

    open_hunk(cx, window, &view, 4);
    click(cx, window, "agent-hunk-reject");
    draw(cx, window);
    wait_for(cx, "the answer of propose_edit", |cx| {
        draw(cx, window);
        answered(&answer)
    });
    let text = take_answer(&answer).expect("propose_edit answers");

    assert!(
        text.contains("Main.txt: partial, 1 of 2 hunks kept"),
        "{text}"
    );
    assert!(
        text.contains(FIRST_ACCEPTED),
        "the kept text is in the answer: {text}"
    );
    wait_for(cx, "the file the user kept", |cx| {
        draw(cx, window);
        fs::read_to_string(&main).unwrap() == FIRST_ACCEPTED
    });
    wait_for_signal(cx, "the editor to read the file again", |cx| {
        draw(cx, window);
        buffer_of(cx, &view) == FIRST_ACCEPTED
    });
}

#[ui_kit::gpui::test]
fn a_rename_changes_two_files_after_the_user_confirms(cx: &mut TestAppContext) {
    let root = project(
        "mcp-edits-rename",
        &[("src/Main.txt", "alpha\n"), ("src/Other.txt", "alpha\n")],
    );
    let main = root.join("src").join("Main.txt");
    let other = root.join("src").join("Other.txt");
    let language = FakeLanguage::new();
    language.set_closed(other.clone());
    let window = open_window_with(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &main);
    let view = editor_of(cx, window, &main);
    sync(cx);

    let answer = ask_in_background(
        gateway(cx),
        scope(&root),
        "rename_symbol",
        json!({"path": "src/Main.txt", "line": 1, "column": 1, "new_name": "beta"}),
    );
    wait_for(cx, "the preview of the rename", |cx| {
        draw(cx, window);
        window
            .read_with(cx, |workspace, _| workspace.last_notification())
            .ok()
            .flatten()
            .is_some_and(|(title, _)| title.as_ref() == "Rename symbol")
    });
    assert_eq!(
        fs::read_to_string(&other).unwrap(),
        "alpha\n",
        "nothing is written before the user confirms"
    );

    click(cx, window, "notification-action");
    draw(cx, window);
    wait_for(cx, "the answer of rename_symbol", |cx| {
        draw(cx, window);
        answered(&answer)
    });
    let text = take_answer(&answer).expect("the user confirmed the rename");

    assert!(text.contains("rename: 2 changes in 2 files"), "{text}");
    assert!(text.contains("src/Main.txt"), "{text}");
    assert!(text.contains("src/Other.txt"), "{text}");
    wait_for(cx, "the open file the rename reached", |cx| {
        draw(cx, window);
        buffer_of(cx, &view) == "beta\n"
    });
    assert_eq!(
        fs::read_to_string(&other).unwrap(),
        "beta\n",
        "the closed file changed on disk"
    );
}

#[ui_kit::gpui::test]
fn organize_imports_goes_through_the_action_of_the_language(cx: &mut TestAppContext) {
    let text = "import b;\nimport a;\n";
    let root = project("mcp-edits-organize", &[("src/Main.txt", text)]);
    let main = root.join("src").join("Main.txt");
    let language = FakeLanguage::new();
    language.set_actions(vec![ActionEntry {
        title: "Organize imports".to_string(),
        kind: Some(ActionKind::OrganizeImports),
        code: None,
        preferred: true,
        edits: vec![SourceEdit {
            uri: uri_of(&main),
            edits: vec![edit_in(text, 0, text.len(), "import a;\nimport b;\n")],
        }],
        renames: Vec::new(),
        handle: None,
        resolve: None,
    }]);
    let window = open_window_with(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &main);
    let view = editor_of(cx, window, &main);
    sync(cx);

    let answer = ask_in_background(
        window_gateway(cx),
        scope(&root),
        "organize_imports",
        json!({"path": "src/Main.txt"}),
    );
    wait_for(cx, "the question of the Execute tool", |cx| {
        draw(cx, window);
        window
            .read_with(cx, |workspace, _| workspace.last_notification())
            .ok()
            .flatten()
            .is_some_and(|(title, text)| {
                title.as_ref() == "Allow an agent tool?"
                    && text.as_ref().contains("organize_imports")
            })
    });
    click(cx, window, "notification-action");
    wait_for(cx, "the answer of organize_imports", |cx| {
        draw(cx, window);
        answered(&answer)
    });

    let answer = take_answer(&answer).expect("organize_imports answers");
    assert!(answer.contains("changed, 1 change"), "{answer}");
    assert_eq!(buffer_of(cx, &view), "import a;\nimport b;\n");
    assert_eq!(
        fs::read_to_string(&main).unwrap(),
        text,
        "the disk waits for Save"
    );
}
