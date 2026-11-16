//! The AI panel without a model: the scripted `MockRuntime` drives the chat, and the
//! files the test writes on disk stand for the edits of an agent in the review tab.

use std::path::{Path, PathBuf};

use agent_core::PermissionReply;
use change_tracker::TrackerState;
use ui_kit::gpui;
use ui_kit::gpui::{Entity, TestAppContext, WindowHandle};
use workspace::Workspace;

use crate::agents::{ActiveAgents, AgentKind, AgentsPanel, Tab};
use crate::ai_review::Action;
use crate::transcript::Item;
use crate::window_tests::{
    buffer_of, draw, editor_of, open_path, open_window, press, sample_project, type_text,
    wait_for_signal,
};

fn panel(cx: &TestAppContext) -> Entity<AgentsPanel> {
    cx.read(|cx| cx.global::<ActiveAgents>().0.clone())
}

/// A window of the sample project that trusts the folder, so a session starts at once.
fn open(cx: &mut TestAppContext, name: &str) -> (WindowHandle<Workspace>, PathBuf) {
    let root = sample_project(name);
    let window = open_window(cx, root.clone());
    draw(cx, window);
    let state_dir = window
        .read_with(cx, |workspace, _| workspace.state_dir().to_path_buf())
        .unwrap();
    store::TrustList::new(&state_dir).trust(&root).unwrap();
    (window, root)
}

fn start_mock(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    let panel = panel(cx);
    window
        .update(cx, |_, window, cx| {
            panel.update(cx, |panel, cx| panel.request(AgentKind::Mock, window, cx))
        })
        .unwrap();
    draw(cx, window);
    wait_for_signal(cx, "the mock session", |cx| {
        panel.read_with(cx, |panel, _| panel.entries.len() == 1)
    });
}

fn send(cx: &mut TestAppContext, text: &str) {
    let panel = panel(cx);
    cx.update(|cx| {
        panel.update(cx, |panel, cx| {
            panel.composer.text = text.to_string();
            panel.send(cx);
        })
    });
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

fn permission_pending(cx: &TestAppContext) -> bool {
    let panel = panel(cx);
    panel.read_with(cx, |panel, _| {
        panel
            .active_entry()
            .is_some_and(|entry| entry.transcript.pending_permission().is_some())
    })
}

fn running(cx: &TestAppContext) -> bool {
    let panel = panel(cx);
    panel.read_with(cx, |panel, _| {
        panel
            .active_entry()
            .is_some_and(|entry| entry.transcript.running)
    })
}

fn queued(cx: &TestAppContext) -> usize {
    let panel = panel(cx);
    panel.read_with(cx, |panel, _| {
        panel
            .active_entry()
            .map(|entry| entry.queued())
            .unwrap_or(0)
    })
}

fn answer(cx: &mut TestAppContext, reply: PermissionReply) {
    let panel = panel(cx);
    cx.update(|cx| panel.update(cx, |panel, cx| panel.answer(reply, cx)));
}

fn stop(cx: &mut TestAppContext) {
    let panel = panel(cx);
    cx.update(|cx| panel.update(cx, |panel, cx| panel.stop(cx)));
}

/// Waits for the tracker of the window to take its first snapshot.
fn wait_for_tracker(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    let panel = panel(cx);
    draw(cx, window);
    wait_for_signal(cx, "the first snapshot", |cx| {
        panel.read_with(cx, |panel, _| panel.changes.state == TrackerState::Ready)
    });
}

/// A file the agent changed, written by the test and taken into the review.
fn change(cx: &mut TestAppContext, window: WindowHandle<Workspace>, root: &Path, text: &str) {
    std::fs::write(root.join("src").join("main.txt"), text).unwrap();
    let panel = panel(cx);
    let tracker = panel
        .read_with(cx, |panel, _| panel.tracker().cloned())
        .unwrap();
    tracker.refresh();
    draw(cx, window);
    wait_for_signal(cx, "the changed file", |cx| {
        panel.read_with(cx, |panel, _| {
            panel
                .changes
                .session
                .iter()
                .any(|file| file.path == "src/main.txt")
        })
    });
}

fn show_changes(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    let panel = panel(cx);
    cx.update(|cx| panel.update(cx, |panel, cx| panel.show_tab(Tab::Changes, cx)));
    draw(cx, window);
    wait_for_signal(cx, "the review", |cx| {
        panel.read_with(cx, |panel, _| panel.review.file.is_some())
    });
}

fn act(cx: &mut TestAppContext, window: WindowHandle<Workspace>, action: Action) {
    let panel = panel(cx);
    cx.update(|cx| panel.update(cx, |panel, cx| panel.act(action, cx)));
    draw(cx, window);
}

#[gpui::test]
fn the_mock_chat_streams_a_reply_a_tool_call_and_a_permission_question(cx: &mut TestAppContext) {
    let (window, _root) = open(cx, "ai-chat");
    start_mock(cx, window);
    send(cx, "where is Main");
    draw(cx, window);

    wait_for_signal(cx, "the permission question", |cx| permission_pending(cx));
    let shown = items(cx);
    assert_eq!(
        shown.first(),
        Some(&Item::User("where is Main".to_string())),
        "{shown:?}"
    );
    assert!(
        shown.iter().any(|item| matches!(
            item,
            Item::Agent(text) if text == "Let me look for it first."
        )),
        "{shown:?}"
    );
    assert!(
        shown
            .iter()
            .any(|item| matches!(item, Item::Tool { title, .. } if title == "find_symbol")),
        "{shown:?}"
    );

    // A message typed while the agent waits waits for its turn.
    send(cx, "and again");
    draw(cx, window);
    wait_for_signal(cx, "the queued message", |cx| queued(cx) == 1);
}

#[gpui::test]
fn a_denied_permission_ends_the_turn_before_the_rest_of_the_script(cx: &mut TestAppContext) {
    let (window, _root) = open(cx, "ai-deny");
    start_mock(cx, window);
    send(cx, "edit it");
    draw(cx, window);
    wait_for_signal(cx, "the permission question", |cx| permission_pending(cx));

    answer(cx, PermissionReply::Deny);
    draw(cx, window);
    wait_for_signal(cx, "the end of the turn", |cx| {
        !running(cx) && !permission_pending(cx)
    });

    let shown = items(cx);
    assert!(
        shown.iter().any(|item| matches!(
            item,
            Item::Permission {
                answer: Some(PermissionReply::Deny),
                ..
            }
        )),
        "{shown:?}"
    );
    assert!(
        !shown
            .iter()
            .any(|item| matches!(item, Item::Agent(text) if text.contains("Done"))),
        "{shown:?}"
    );
}

#[gpui::test]
fn a_cancel_while_the_agent_asks_stops_the_turn(cx: &mut TestAppContext) {
    let (window, _root) = open(cx, "ai-cancel");
    start_mock(cx, window);
    send(cx, "edit it");
    draw(cx, window);
    wait_for_signal(cx, "the permission question", |cx| permission_pending(cx));

    stop(cx);
    draw(cx, window);
    wait_for_signal(cx, "the cancelled turn", |cx| !running(cx));
    assert!(
        !items(cx)
            .iter()
            .any(|item| matches!(item, Item::Agent(text) if text.contains("Done"))),
        "{:?}",
        items(cx)
    );
}

#[gpui::test]
fn a_reverted_hunk_comes_back_with_ctrl_z(cx: &mut TestAppContext) {
    let (window, root) = open(cx, "ai-revert");
    let file = root.join("src").join("main.txt");
    start_mock(cx, window);
    wait_for_tracker(cx, window);
    open_path(cx, window, &file);
    change(cx, window, &root, "ALPHA\nbeta\n");
    show_changes(cx, window);

    act(cx, window, Action::RevertHunk(0));
    wait_for_signal(cx, "the reverted file", |_| {
        std::fs::read_to_string(&file).unwrap() == "alpha\nbeta\n"
    });
    let view = editor_of(cx, window, &file);
    wait_for_signal(cx, "the reloaded editor", |cx| {
        buffer_of(cx, &view) == "alpha\nbeta\n"
    });

    press(cx, window, "ctrl-z");
    draw(cx, window);
    assert_eq!(buffer_of(cx, &view), "ALPHA\nbeta\n");
}

#[gpui::test]
fn a_file_and_then_the_whole_session_revert_to_the_base(cx: &mut TestAppContext) {
    let (window, root) = open(cx, "ai-revert-file");
    let file = root.join("src").join("main.txt");
    start_mock(cx, window);
    wait_for_tracker(cx, window);
    change(cx, window, &root, "ALPHA\nbeta\n");
    show_changes(cx, window);

    act(cx, window, Action::RevertFile);
    wait_for_signal(cx, "the reverted file", |_| {
        std::fs::read_to_string(&file).unwrap() == "alpha\nbeta\n"
    });

    change(cx, window, &root, "ALPHA\nBETA\n");
    show_changes(cx, window);
    act(cx, window, Action::RevertAll);
    wait_for_signal(cx, "the reverted session", |_| {
        std::fs::read_to_string(&file).unwrap() == "alpha\nbeta\n"
    });
}

#[gpui::test]
fn a_revert_all_leaves_a_file_with_unsaved_edits_alone(cx: &mut TestAppContext) {
    let (window, root) = open(cx, "ai-dirty");
    let file = root.join("src").join("main.txt");
    start_mock(cx, window);
    wait_for_tracker(cx, window);
    open_path(cx, window, &file);
    change(cx, window, &root, "ALPHA\nbeta\n");
    show_changes(cx, window);

    type_text(cx, window, "!");
    act(cx, window, Action::RevertAll);
    let panel = panel(cx);
    wait_for_signal(cx, "the notice about unsaved edits", |cx| {
        panel.read_with(cx, |panel, _| {
            panel
                .review
                .notice
                .as_deref()
                .is_some_and(|notice| notice.contains("unsaved"))
        })
    });
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "ALPHA\nbeta\n");
    let view = editor_of(cx, window, &file);
    assert!(
        buffer_of(cx, &view).starts_with('!'),
        "{}",
        buffer_of(cx, &view)
    );
}
