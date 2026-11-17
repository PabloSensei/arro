//! The marks of an agent's changes in an open editor, the window of one hunk and the bar
//! above the file, without a real window. The mock session and files the test writes stand
//! for a real agent: there is no network and no process.

use std::fs;
use std::path::{Path, PathBuf};

use change_tracker::TrackerState;
use editor_view::{EditorView, StripeKind};
use ui_kit::gpui;
use ui_kit::gpui::{Entity, Modifiers, TestAppContext, VisualTestContext, WindowHandle};
use workspace::{AI_TOOL, Workspace};

use crate::agent_hunk::AgentHunkPopup;
use crate::agents::{ActiveAgents, AgentKind, AgentsPanel, Tab};
use crate::window_tests::{
    buffer_of, click, draw, editor_of, open_path, open_window, press, sample_project,
    wait_for_signal,
};

const BASE: &str = "alpha\nbeta\ngamma\ndelta\nepsilon\n";
const CHANGED: &str = "ALPHA\nbeta\ngamma\ndelta\nEPSILON\n";
/// What the first hunk leaves when it goes back to the base.
const FIRST_REVERTED: &str = "alpha\nbeta\ngamma\ndelta\nEPSILON\n";

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
    fs::write(root.join("src").join("main.txt"), text).unwrap();
    let tracker = panel(cx)
        .read_with(cx, |panel, _| panel.tracker().cloned())
        .unwrap();
    tracker.refresh();
    draw(cx, window);
    wait_for_signal(cx, "the changed file", |cx| {
        panel(cx).read_with(cx, |panel, _| {
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

fn hunks(cx: &TestAppContext) -> Option<usize> {
    panel(cx).read_with(cx, |panel, _| {
        panel.review.file.as_ref().map(|file| file.hunks.len())
    })
}

fn changed_paths(cx: &TestAppContext) -> Vec<String> {
    panel(cx).read_with(cx, |panel, _| {
        panel
            .changes
            .session
            .iter()
            .map(|change| change.path.clone())
            .collect()
    })
}

fn has_selector(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &'static str,
) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

fn popup_of(
    cx: &TestAppContext,
    window: WindowHandle<Workspace>,
) -> Option<Entity<AgentHunkPopup>> {
    window
        .read_with(cx, |workspace, _| workspace.overlay_view())
        .unwrap()
        .and_then(|view| view.downcast::<AgentHunkPopup>().ok())
}

/// The project with a file of two hunks, a mock session and the AI tool window open.
fn two_hunks(
    cx: &mut TestAppContext,
    name: &str,
) -> (WindowHandle<Workspace>, PathBuf, Entity<EditorView>) {
    let (window, root) = open(cx, name);
    let file = root.join("src").join("main.txt");
    fs::write(&file, BASE).unwrap();
    start_mock(cx, window);
    wait_for_tracker(cx, window);
    open_path(cx, window, &file);
    crate::window_tests_cached_views::show_tool(cx, window, AI_TOOL);
    change(cx, window, &root, CHANGED);
    let view = editor_of(cx, window, &file);
    (window, root, view)
}

fn wait_for_marks(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    view: &Entity<EditorView>,
    count: usize,
) {
    wait_for_signal(cx, "the marks of the agent", |cx| {
        draw(cx, window);
        view.read_with(cx, |view, _| view.agent_marks().len() == count)
    });
}

/// Clicks the mark of `line` and waits for the window of its hunk.
fn open_hunk(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    view: &Entity<EditorView>,
    line: usize,
) -> Entity<AgentHunkPopup> {
    wait_for_signal(cx, "the mark on the line", |cx| {
        draw(cx, window);
        view.read_with(cx, |view, _| view.agent_mark_bounds(line).is_some())
    });
    let bounds = view
        .read_with(cx, |view, _| view.agent_mark_bounds(line))
        .unwrap();
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_click(bounds.center(), Modifiers::default());
    drop(visual);
    cx.run_until_parked();
    draw(cx, window);
    wait_for_signal(cx, "the window of the hunk", |cx| {
        popup_of(cx, window).is_some()
    });
    popup_of(cx, window).unwrap()
}

/// A file long enough for the vertical bar of the editor, changed at the top and the end.
fn long_base() -> String {
    let mut text = String::from("alpha\nbeta\ngamma\ndelta\n");
    for line in 4..120 {
        text.push_str(&format!("line {line}\n"));
    }
    text.push_str("epsilon\n");
    text
}

fn long_changed() -> String {
    let mut text = String::from("ALPHA\nbeta\ngamma\ndelta\n");
    for line in 4..120 {
        text.push_str(&format!("line {line}\n"));
    }
    text.push_str("EPSILON\n");
    text
}

#[gpui::test]
fn an_agent_change_marks_the_file_and_shows_the_bar(cx: &mut TestAppContext) {
    let (window, root) = open(cx, "ai-marks");
    let file = root.join("src").join("main.txt");
    fs::write(&file, long_base()).unwrap();
    start_mock(cx, window);
    wait_for_tracker(cx, window);
    open_path(cx, window, &file);
    crate::window_tests_cached_views::show_tool(cx, window, AI_TOOL);
    change(cx, window, &root, &long_changed());
    let view = editor_of(cx, window, &file);
    wait_for_marks(cx, window, &view, 2);

    assert!(
        view.read_with(cx, |view, _| view.gutter_marks().is_empty()),
        "the file is outside Git, so every mark in the gutter belongs to the agent"
    );
    wait_for_signal(cx, "the stripes of the agent", |cx| {
        draw(cx, window);
        view.read_with(cx, |view, _| {
            view.stripe_kinds()
                .iter()
                .any(|(kind, _)| *kind == StripeKind::Agent)
        })
    });

    wait_for_signal(cx, "the bar above the editor", |cx| {
        draw(cx, window);
        has_selector(cx, window, "agent-change-bar")
    });
    for selector in [
        "agent-bar-accept-all",
        "agent-bar-reject-all",
        "agent-bar-review",
    ] {
        assert!(has_selector(cx, window, selector), "the bar has {selector}");
    }

    click(cx, window, "agent-bar-review");
    wait_for_signal(cx, "the review of the file", |cx| {
        panel(cx).read_with(cx, |panel, _| {
            panel.tab == Tab::Changes && panel.review.selected.as_deref() == Some("src/main.txt")
        })
    });
    let open = window
        .read_with(cx, |workspace, _| workspace.tool_open(AI_TOOL))
        .unwrap();
    assert!(open, "Review opens the AI tool window");
}

#[gpui::test]
fn accepting_a_mark_leaves_the_agent_text_in_place(cx: &mut TestAppContext) {
    let (window, root, view) = two_hunks(cx, "ai-mark-accept");
    wait_for_marks(cx, window, &view, 2);

    open_hunk(cx, window, &view, 0);
    click(cx, window, "agent-hunk-accept");
    draw(cx, window);
    wait_for_marks(cx, window, &view, 1);

    assert_eq!(buffer_of(cx, &view), CHANGED);
    assert_eq!(
        fs::read_to_string(root.join("src").join("main.txt")).unwrap(),
        CHANGED
    );
}

#[gpui::test]
fn rejecting_a_mark_returns_the_old_text_and_ctrl_z_returns_the_edit(cx: &mut TestAppContext) {
    let (window, root, view) = two_hunks(cx, "ai-mark-reject");
    wait_for_marks(cx, window, &view, 2);

    open_hunk(cx, window, &view, 0);
    click(cx, window, "agent-hunk-reject");
    draw(cx, window);
    wait_for_signal(cx, "the reverted text", |cx| {
        buffer_of(cx, &view) == FIRST_REVERTED
    });
    assert_eq!(
        fs::read_to_string(root.join("src").join("main.txt")).unwrap(),
        FIRST_REVERTED
    );
    wait_for_marks(cx, window, &view, 1);

    press(cx, window, "ctrl-z");
    draw(cx, window);
    assert_eq!(
        buffer_of(cx, &view),
        CHANGED,
        "the revert is one undo step of the editor"
    );
}

#[gpui::test]
fn accept_all_of_the_bar_keeps_the_text_and_clears_the_marks(cx: &mut TestAppContext) {
    let (window, _root, view) = two_hunks(cx, "ai-bar-accept");
    wait_for_marks(cx, window, &view, 2);

    click(cx, window, "agent-bar-accept-all");
    draw(cx, window);
    wait_for_marks(cx, window, &view, 0);
    wait_for_signal(cx, "the bar to leave", |cx| {
        draw(cx, window);
        !has_selector(cx, window, "agent-change-bar")
    });
    assert_eq!(buffer_of(cx, &view), CHANGED);
}

#[gpui::test]
fn reject_all_of_the_bar_returns_the_base(cx: &mut TestAppContext) {
    let (window, root, view) = two_hunks(cx, "ai-bar-reject");
    wait_for_marks(cx, window, &view, 2);

    click(cx, window, "agent-bar-reject-all");
    draw(cx, window);
    wait_for_signal(cx, "the base on disk", |_| {
        fs::read_to_string(root.join("src").join("main.txt")).unwrap() == BASE
    });
    wait_for_signal(cx, "the base in the editor", |cx| {
        buffer_of(cx, &view) == BASE
    });
    wait_for_marks(cx, window, &view, 0);
}

#[gpui::test]
fn the_panel_and_the_editor_share_the_state_of_a_hunk(cx: &mut TestAppContext) {
    let (window, _root, view) = two_hunks(cx, "ai-shared-state");
    show_changes(cx, window);
    wait_for_signal(cx, "two hunks in the panel", |cx| hunks(cx) == Some(2));

    // The panel takes the first hunk in: the mark in the editor follows.
    click(cx, window, "accept-hunk-0");
    draw(cx, window);
    wait_for_signal(cx, "the panel to drop the first hunk", |cx| {
        hunks(cx) == Some(1)
    });
    wait_for_marks(cx, window, &view, 1);

    // The editor takes the second hunk in: the panel follows, and the list is empty.
    open_hunk(cx, window, &view, 4);
    click(cx, window, "agent-hunk-accept");
    draw(cx, window);
    wait_for_signal(cx, "the panel to have no hunks left", |cx| {
        hunks(cx).is_none_or(|hunks| hunks == 0)
    });
    wait_for_marks(cx, window, &view, 0);
    wait_for_signal(cx, "the empty list of changes", |cx| {
        changed_paths(cx).is_empty()
    });
}

const BINARY_BEFORE: [u8; 3] = [0xff, 0x00, 0x01];
const BINARY_AFTER: [u8; 3] = [0xff, 0x00, 0x02];

/// The project with a binary file the agent changed, its review on screen.
fn binary_change(cx: &mut TestAppContext, name: &str) -> (WindowHandle<Workspace>, PathBuf) {
    let (window, root) = open(cx, name);
    let file = root.join("src").join("image.bin");
    fs::write(&file, BINARY_BEFORE).unwrap();
    start_mock(cx, window);
    wait_for_tracker(cx, window);

    fs::write(&file, BINARY_AFTER).unwrap();
    let tracker = panel(cx)
        .read_with(cx, |panel, _| panel.tracker().cloned())
        .unwrap();
    tracker.refresh();
    draw(cx, window);
    wait_for_signal(cx, "the binary file in the list", |cx| {
        panel(cx).read_with(cx, |panel, _| {
            panel
                .changes
                .session
                .iter()
                .any(|change| change.path == "src/image.bin")
        })
    });

    crate::window_tests_cached_views::show_tool(cx, window, AI_TOOL);
    show_changes(cx, window);
    wait_for_signal(cx, "the review of the binary file", |cx| {
        panel(cx).read_with(cx, |panel, _| {
            panel.review.file.as_ref().is_some_and(|file| file.binary)
        })
    });
    (window, root)
}

#[gpui::test]
fn a_binary_change_reverts_to_the_snapshot_from_before_the_agent(cx: &mut TestAppContext) {
    let (window, root) = binary_change(cx, "ai-binary-revert");
    let file = root.join("src").join("image.bin");

    click(cx, window, "revert-file");
    draw(cx, window);
    wait_for_signal(cx, "the snapshot on disk", |_| {
        fs::read(&file).unwrap() == BINARY_BEFORE
    });
    wait_for_signal(cx, "the empty list of changes", |cx| {
        changed_paths(cx).is_empty()
    });
}

#[gpui::test]
fn a_binary_change_is_accepted_as_its_bytes(cx: &mut TestAppContext) {
    let (window, root) = binary_change(cx, "ai-binary-accept");
    let file = root.join("src").join("image.bin");

    click(cx, window, "accept-file");
    draw(cx, window);
    wait_for_signal(cx, "the empty list of changes", |cx| {
        changed_paths(cx).is_empty()
    });
    assert_eq!(fs::read(&file).unwrap(), BINARY_AFTER);
}
