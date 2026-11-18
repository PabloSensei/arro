#![cfg(all(windows, feature = "mock-agents"))]
//! A mock session that plays a transcript from a file: the chat shows the words of the
//! user, the reply, a real tool call and the edit, and the edit reaches the review and the
//! gutter of the editor.

use std::path::PathBuf;

use agent_core::ToolStatus;
use change_tracker::TrackerState;
use ui_kit::gpui;
use ui_kit::gpui::{Entity, TestAppContext, WindowHandle};
use workspace::Workspace;

use crate::agents::{ActiveAgents, AgentKind, AgentsPanel};
use crate::transcript::Item;
use crate::window_tests::{
    buffer_of, draw, editor_of, open_path, open_window, sample_project, wait_for_signal,
};

/// One turn: the words of the user, a reply, a read through the gateway, and an edit the
/// session writes itself.
const TRANSCRIPT: &str = r#"[
    [
        {"user": "Owners with no pets should not be listed on the search page"},
        {"reply": "Let me read the file first."},
        {"tool": "read_file", "input": {"path": "src/main.txt"}},
        {"edit": {"path": "src/main.txt", "old_text": "alpha", "new_text": "ALPHA"}}
    ]
]"#;

const PROMPT: &str = "Owners with no pets should not be listed on the search page";

fn panel(cx: &TestAppContext) -> Entity<AgentsPanel> {
    cx.read(|cx| cx.global::<ActiveAgents>().0.clone())
}

fn items(cx: &TestAppContext) -> Vec<Item> {
    let panel = panel(cx);
    panel.read_with(cx, |panel, _| {
        panel
            .active_entry()
            .map(|entry| entry.transcript.items.clone())
            .unwrap_or_default()
    })
}

fn transcript_file(name: &str, text: &str) -> PathBuf {
    let folder = test_support::scratch(name);
    let path = folder.join("agent.json");
    std::fs::write(&path, text).unwrap();
    path
}

/// A window of the sample project that trusts the folder, so a session starts at once.
fn open(cx: &mut TestAppContext, name: &str) -> (PathBuf, WindowHandle<Workspace>) {
    let root = sample_project(name);
    let window = open_window(cx, root.clone());
    draw(cx, window);
    let state_dir = window
        .read_with(cx, |workspace, _| workspace.state_dir().to_path_buf())
        .unwrap();
    store::TrustList::new(&state_dir).trust(&root).unwrap();
    (root, window)
}

#[gpui::test]
fn a_transcript_shows_its_messages_a_tool_and_the_edit(cx: &mut TestAppContext) {
    let (root, window) = open(cx, "mock-transcript");
    let file = root.join("src").join("main.txt");
    let panel = panel(cx);
    let path = transcript_file("mock-transcript-file", TRANSCRIPT);
    cx.update(|cx| panel.update(cx, |panel, _| panel.set_mock_transcript(path)));

    window
        .update(cx, |_, window, cx| {
            panel.update(cx, |panel, cx| panel.request(AgentKind::Mock, window, cx))
        })
        .unwrap();
    draw(cx, window);
    wait_for_signal(cx, "the mock session", |cx| {
        panel.read_with(cx, |panel, _| panel.entries.len() == 1)
    });
    // The tracker takes its first snapshot when the session starts; an edit before it
    // would belong to the base of the session and never reach the review.
    wait_for_signal(cx, "the first snapshot", |cx| {
        panel.read_with(cx, |panel, _| panel.changes.state == TrackerState::Ready)
    });
    open_path(cx, window, &file);

    cx.update(|cx| {
        panel.update(cx, |panel, cx| {
            assert_eq!(panel.prompt(PROMPT, cx), Ok(true), "the prompt is sent");
        })
    });
    draw(cx, window);

    wait_for_signal(cx, "the whole turn", |cx| {
        let shown = items(cx);
        let typed = shown
            .iter()
            .any(|item| item == &Item::User(PROMPT.to_string()));
        let replied = shown
            .iter()
            .any(|item| matches!(item, Item::Agent(text) if text == "Let me read the file first."));
        let read = shown.iter().any(|item| {
            matches!(
                item,
                Item::Tool { title, status: ToolStatus::Done, output: Some(text), .. }
                    if title == "read_file" && text.contains("alpha")
            )
        });
        let edited = shown.iter().any(|item| {
            matches!(
                item,
                Item::Tool { title, status: ToolStatus::Done, .. } if title == "Edit"
            )
        });
        typed && replied && read && edited
    });

    wait_for_signal(cx, "the edit on disk", |_| {
        std::fs::read_to_string(&file).unwrap() == "ALPHA\nbeta\n"
    });
    let view = editor_of(cx, window, &file);
    draw(cx, window);
    wait_for_signal(cx, "the mark of the changed line", |cx| {
        view.read_with(cx, |view, _| {
            view.agent_marks()
                .iter()
                .any(|mark| mark.lines.contains(&0))
        })
    });
    assert_eq!(buffer_of(cx, &view), "ALPHA\nbeta\n");
}

#[test]
fn parses_the_agent_transcript_and_prompt_steps() {
    let steps = crate::script::parse_script(
        "agent-transcript:tools/demo/petclinic-agent.json;agent:mock;\
         agent-prompt:Owners with no pets should not be listed",
    )
    .expect("the steps parse");
    assert_eq!(
        steps[0],
        crate::script::Step::AgentTranscript("tools/demo/petclinic-agent.json".into())
    );
    assert_eq!(steps[1], crate::script::Step::Agent(AgentKind::Mock));
    assert_eq!(
        steps[2],
        crate::script::Step::AgentPrompt("Owners with no pets should not be listed".into())
    );
    assert!(crate::script::parse_script("agent-transcript:").is_err());
}
