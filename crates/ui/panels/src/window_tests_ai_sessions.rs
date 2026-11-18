//! The AI sessions window without a real window: the New Session menu and the hints of its rows,
//! the AGENTS.md button and its result under it, the list of changes that the button refreshes,
//! and the Changes tab that scrolls. The mock session and the files the test writes stand for a
//! real agent, as in the other AI window tests.

use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use agent_core::{Harness, Installed};
use change_tracker::TrackerState;
use ui_kit::gpui::{
    self, Entity, Modifiers, ScrollDelta, ScrollEvent, TestAppContext, VisualTestContext,
    WindowHandle,
};
use ui_kit::{TOOLTIP_DELAY, clear_shown_tooltips, point, shown_tooltips};
use workspace::{AI_TOOL, Workspace};

use crate::agents::{ActiveAgents, AgentKind, AgentsPanel, Tab};
use crate::ai_review::AGENTS_MD_NOTE_TIME;
use crate::window_tests::{
    bounds_of, click, draw, open_path, open_window, sample_project, type_text,
    wait_for_settled_bounds, wait_for_signal,
};
use crate::window_tests_cached_views::show_tool;

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

/// Shows the AI tool window and waits until it has slid into its dock, so the places of its
/// buttons are the settled ones that a hover or a click meets.
fn show_ai_window(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    show_tool(cx, window, AI_TOOL);
    wait_for_settled_bounds(cx, window, "agents-md");
}

fn start_mock(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    let agents = panel(cx);
    window
        .update(cx, |_, window, cx| {
            agents.update(cx, |panel, cx| panel.request(AgentKind::Mock, window, cx))
        })
        .unwrap();
    draw(cx, window);
    wait_for_signal(cx, "the mock session", |cx| {
        agents.read_with(cx, |panel, _| panel.entries.len() == 1)
    });
}

/// Waits for the tracker of the window to take its first snapshot.
fn wait_for_tracker(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    let agents = panel(cx);
    draw(cx, window);
    wait_for_signal(cx, "the first snapshot", |cx| {
        agents.read_with(cx, |panel, _| panel.changes.state == TrackerState::Ready)
    });
}

/// A file the session made, written by the test and taken into the list of changes.
fn make_file(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    root: &Path,
    name: &str,
    text: &str,
) {
    fs::write(root.join(name), text).unwrap();
    let agents = panel(cx);
    let tracker = agents
        .read_with(cx, |panel, _| panel.tracker().cloned())
        .unwrap();
    tracker.refresh();
    wait_for_signal(cx, "the new file in the changes", |cx| {
        draw(cx, window);
        agents.read_with(cx, |panel, _| {
            panel
                .changes
                .session
                .iter()
                .any(|change| change.path == name)
        })
    });
}

/// Opens the Changes tab, where the first change is selected, and waits for its review.
fn show_changes(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    let agents = panel(cx);
    cx.update(|cx| agents.update(cx, |panel, cx| panel.show_tab(Tab::Changes, cx)));
    draw(cx, window);
    wait_for_signal(cx, "the review", |cx| {
        agents.read_with(cx, |panel, _| panel.review.file.is_some())
    });
}

/// Presses the AGENTS.md button of the sessions tab and draws the frame after it.
fn press_agents_md(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    draw(cx, window);
    click(cx, window, "agents-md");
    draw(cx, window);
}

/// The result that the AGENTS.md button shows: its text, and whether it failed.
fn agents_md_note(cx: &TestAppContext) -> Option<(String, bool)> {
    panel(cx).read_with(cx, |panel, _| {
        panel
            .agents_md
            .as_ref()
            .map(|note| (note.text.to_string(), note.failed))
    })
}

/// Hovers the middle of an element and returns the text of the tooltip it shows.
fn hover(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &'static str,
) -> String {
    wait_for_settled_bounds(cx, window, selector);
    clear_shown_tooltips();
    let bounds = bounds_of(cx, window, selector);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_move(bounds.center(), None, Modifiers::default());
    drop(visual);
    cx.executor()
        .advance_clock(TOOLTIP_DELAY + Duration::from_millis(100));
    cx.run_until_parked();
    draw(cx, window);
    shown_tooltips()
        .into_iter()
        .last()
        .map(|(text, _)| text)
        .unwrap_or_else(|| panic!("{selector} shows no tooltip"))
}

/// Turns the wheel over the Changes tab by `rows` lines: positive scrolls down.
fn scroll_changes(cx: &mut TestAppContext, window: WindowHandle<Workspace>, rows: f32) {
    let bounds = bounds_of(cx, window, "scroll-area-changes");
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_event(ScrollEvent {
        position: bounds.center(),
        delta: ScrollDelta::Lines(point(0., -rows)),
        modifiers: Modifiers::default(),
        ..Default::default()
    });
    drop(visual);
    cx.run_until_parked();
    draw(cx, window);
}

#[gpui::test]
fn the_new_session_button_and_agents_md_do_not_overlap(cx: &mut TestAppContext) {
    let (window, _root) = open(cx, "ai-sessions-layout");
    show_ai_window(cx, window);

    // One button starts the agents and AGENTS.md follows it: on a wide panel on its right in
    // the same row, and on a narrow one under it, where the row wraps.
    let session = bounds_of(cx, window, "agent-new-session");
    let agents_md = bounds_of(cx, window, "agents-md");
    let beside = (f32::from(session.center().y) - f32::from(agents_md.center().y)).abs() < 2.0
        && f32::from(agents_md.left()) >= f32::from(session.right());
    let under = f32::from(agents_md.top()) >= f32::from(session.bottom()) - 0.5;
    assert!(
        beside || under,
        "AGENTS.md neither beside nor under the New Session button: {session:?} {agents_md:?}"
    );
}

#[gpui::test]
fn the_menu_hints_name_the_key_and_the_login(cx: &mut TestAppContext) {
    let (window, _root) = open(cx, "ai-sessions-tooltips");
    show_ai_window(cx, window);
    // A stand-in replaces the harnesses on PATH, so the rows do not depend on this machine.
    // The test only hovers the rows: nothing starts.
    let agents = panel(cx);
    cx.update(|cx| {
        agents.update(cx, |panel, _| {
            panel.use_installs_for_test(vec![Installed {
                harness: Harness::OpenCode,
                program: "opencode".into(),
                prefix: Vec::new(),
                through_shell: false,
            }])
        })
    });
    click(cx, window, "agent-new-session");
    draw(cx, window);

    // The rows: the harness, a separator, then the two ACP agents.
    let claude = hover(cx, window, "menu-3");
    assert!(claude.contains("ANTHROPIC_API_KEY"), "{claude}");
    assert!(claude.contains("Agent Client Protocol"), "{claude}");

    let codex = hover(cx, window, "menu-2");
    assert!(codex.contains("Signs in through Codex itself"), "{codex}");
}

#[gpui::test]
fn the_agents_md_result_shows_under_its_button_and_writes_the_section(cx: &mut TestAppContext) {
    let (window, root) = open(cx, "ai-sessions-agents-md");
    show_ai_window(cx, window);
    press_agents_md(cx, window);

    let expected = tool_gateway::with_agents_md_section("", agent_pty::MCP_SERVER_NAME);
    assert_eq!(
        fs::read_to_string(root.join("AGENTS.md")).unwrap(),
        expected
    );
    assert_eq!(
        agents_md_note(cx),
        Some(("AGENTS.md lists the IDE tools now".to_string(), false))
    );

    // The result sits in a row of its own under the buttons, so that its text stays readable.
    let button = bounds_of(cx, window, "agents-md");
    let result = bounds_of(cx, window, "agents-md-result");
    assert!(
        f32::from(result.top()) >= f32::from(button.bottom()) - 0.5,
        "the result is under the button"
    );

    // The Changes tab keeps its own notice for the other actions.
    assert!(panel(cx).read_with(cx, |panel, _| panel.review.notice.is_none()));
}

#[gpui::test]
fn agents_md_again_says_it_lists_the_tools_and_keeps_the_file(cx: &mut TestAppContext) {
    let (window, root) = open(cx, "ai-sessions-agents-md-again");
    show_ai_window(cx, window);
    press_agents_md(cx, window);

    // An old time, so that a write would show as a new one.
    let file = root.join("AGENTS.md");
    let old = UNIX_EPOCH + Duration::from_secs(1_000_000_000);
    OpenOptions::new()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(old)
        .unwrap();

    press_agents_md(cx, window);
    assert_eq!(
        agents_md_note(cx),
        Some(("AGENTS.md already lists the IDE tools".to_string(), false))
    );
    assert_eq!(fs::metadata(&file).unwrap().modified().unwrap(), old);
}

#[gpui::test]
fn agents_md_with_unsaved_edits_in_the_editor_is_not_written(cx: &mut TestAppContext) {
    let (window, root) = open(cx, "ai-sessions-agents-md-dirty");
    let file = root.join("AGENTS.md");
    fs::write(&file, "# Notes\n").unwrap();
    show_ai_window(cx, window);
    open_path(cx, window, &file);
    type_text(cx, window, "!");

    press_agents_md(cx, window);
    assert_eq!(
        agents_md_note(cx),
        Some((
            "AGENTS.md has unsaved edits in the editor, save them first".to_string(),
            true
        ))
    );
    assert_eq!(fs::read_to_string(&file).unwrap(), "# Notes\n");
}

#[gpui::test]
fn the_agents_md_result_goes_at_the_next_action_or_after_a_while(cx: &mut TestAppContext) {
    let (window, _root) = open(cx, "ai-sessions-agents-md-fade");
    show_ai_window(cx, window);
    press_agents_md(cx, window);
    assert!(agents_md_note(cx).is_some());

    // Another tab is the next action, so the result goes at once.
    let agents = panel(cx);
    cx.update(|cx| agents.update(cx, |panel, cx| panel.show_tab(Tab::Changes, cx)));
    draw(cx, window);
    assert_eq!(agents_md_note(cx), None);

    // Without an action it goes after AGENTS_MD_NOTE_TIME.
    cx.update(|cx| agents.update(cx, |panel, cx| panel.show_tab(Tab::Session, cx)));
    press_agents_md(cx, window);
    assert!(agents_md_note(cx).is_some());
    cx.executor()
        .advance_clock(AGENTS_MD_NOTE_TIME + Duration::from_millis(100));
    cx.run_until_parked();
    draw(cx, window);
    assert_eq!(agents_md_note(cx), None);
}

#[gpui::test]
fn agents_md_joins_the_changes_in_the_next_frames(cx: &mut TestAppContext) {
    let (window, root) = open(cx, "ai-sessions-agents-md-changes");
    start_mock(cx, window);
    wait_for_tracker(cx, window);
    show_ai_window(cx, window);

    press_agents_md(cx, window);

    // No other action: the frames that follow bring the file into the list of changes.
    let agents = panel(cx);
    wait_for_signal(cx, "AGENTS.md in the changes", |cx| {
        draw(cx, window);
        agents.read_with(cx, |panel, _| {
            panel
                .changes
                .session
                .iter()
                .any(|change| change.path == "AGENTS.md")
        })
    });
    assert!(root.join("AGENTS.md").is_file());
}

#[gpui::test]
fn a_long_added_file_scrolls_in_the_changes_tab(cx: &mut TestAppContext) {
    let (window, root) = open(cx, "ai-sessions-scroll");
    start_mock(cx, window);
    wait_for_tracker(cx, window);
    show_ai_window(cx, window);

    // A new file of sixty lines: one hunk of sixty added lines, taller than the view.
    let text: String = (1..=60).map(|line| format!("line {line}\n")).collect();
    make_file(cx, window, &root, "CLAUDE.md", &text);
    show_changes(cx, window);

    let agents = panel(cx);
    let taller = agents.read_with(cx, |panel, _| f32::from(panel.review_scroll.max_offset().y));
    assert!(taller > 0.0, "the hunk is taller than the view: {taller}");

    // Before the wheel, the last line of the hunk is below the view.
    let area = bounds_of(cx, window, "scroll-area-changes");
    let last = bounds_of(cx, window, "review-last-line-0");
    assert!(
        f32::from(last.bottom()) > f32::from(area.bottom()),
        "the last line starts below the view"
    );

    // The wheel moves the content, and at its end the last line is in the view.
    scroll_changes(cx, window, 3.0);
    let moved = -agents.read_with(cx, |panel, _| f32::from(panel.review_scroll.offset().y));
    assert!(moved > 0.0, "the wheel moves the changes: {moved}");

    scroll_changes(cx, window, 500.0);
    let area = bounds_of(cx, window, "scroll-area-changes");
    let last = bounds_of(cx, window, "review-last-line-0");
    assert!(
        f32::from(last.bottom()) <= f32::from(area.bottom()) + 0.5,
        "the last line is above the bottom of the view"
    );
    assert!(
        f32::from(last.top()) >= f32::from(area.top()) - 0.5,
        "the last line is below the top of the view"
    );
}
