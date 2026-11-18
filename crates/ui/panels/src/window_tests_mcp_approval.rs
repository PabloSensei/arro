//! The answer buttons of the question an `Execute` tool asks: the commands behind them
//! run by id but stay out of Find Action and Search Everywhere, and a command that runs
//! inside the update of the window answers the waiting call without updating it again.

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]

use std::thread;

use keymap::workspace as keys;
use tool_gateway::{Approval, ApprovalAnswer, ApprovalRequest};
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::Workspace;

use crate::agents::ActiveAgents;
use crate::palette::Palette;
use crate::window_tests::{
    draw, open_palette, open_sample_window, press, type_text, wait_for_signal,
};

fn palette(cx: &TestAppContext, window: WindowHandle<Workspace>) -> gpui::Entity<Palette> {
    let view = window
        .read_with(cx, |workspace, _| workspace.overlay_view())
        .unwrap()
        .expect("a popup is open");
    view.downcast::<Palette>()
        .expect("the popup is the palette")
}

fn rows(cx: &TestAppContext, window: WindowHandle<Workspace>) -> Vec<(String, String)> {
    palette(cx, window).read_with(cx, |palette, _| palette.rows_text())
}

/// The buttons of a question are not actions of their own: neither Find Action nor
/// Search Everywhere lists them, and the ids still run from the notification.
#[ui_kit::gpui::test]
fn the_answer_buttons_stay_out_of_the_search_lists(cx: &mut TestAppContext) {
    let (_root, window) = open_sample_window(cx, "mcp-approval-lists");

    open_palette(cx, window, keys::FIND_ACTION);
    type_text(cx, window, "Allow Agent Tool");
    draw(cx, window);
    let found = rows(cx, window);
    assert!(
        !found
            .iter()
            .any(|(row, _)| row.contains("Allow Agent Tool")),
        "Find Action lists a button of a notification: {found:?}"
    );
    press(cx, window, "escape");

    // Search Everywhere: `notes` finds the file of the sample, and no Actions group.
    open_palette(cx, window, keys::SEARCH_EVERYWHERE);
    type_text(cx, window, "notes");
    draw(cx, window);
    let found = rows(cx, window);
    assert!(
        !found.iter().any(|(row, _)| row == "Actions"),
        "Search Everywhere lists an empty Actions group: {found:?}"
    );
    assert!(
        found.iter().any(|(row, _)| row == "notes.txt"),
        "the file of the query is still there: {found:?}"
    );
    press(cx, window, "escape");

    let ids = window
        .read_with(cx, |workspace, _| {
            workspace
                .commands()
                .all()
                .filter(|command| command.hidden)
                .map(|command| command.id)
                .collect::<Vec<_>>()
        })
        .unwrap();
    for id in ["ai.mcp.allow", "ai.mcp.allow_session", "ai.mcp.deny"] {
        assert!(ids.contains(&id), "{id} is a command of the window");
    }
}

/// A command runs inside the update of the window. The answer it carries updates the
/// window back, so the answer waits for the end of that update, as the button does.
#[ui_kit::gpui::test]
fn an_answer_command_reaches_the_waiting_call(cx: &mut TestAppContext) {
    let (_root, window) = open_sample_window(cx, "mcp-approval-answer");
    let agents = cx.read(|cx| cx.global::<ActiveAgents>().0.clone());
    let approvals = agents.read_with(cx, |panel, _| panel.approvals.clone());

    let waiting = approvals.clone();
    let request = ApprovalRequest {
        session: "mcp".to_string(),
        client: Some("Fixture Client".to_string()),
        tool: "run_tests".to_string(),
    };
    let call = thread::spawn(move || waiting.ask_with(&request, "demo.Vitest"));
    wait_for_signal(cx, "the question of the tool", |_| {
        approvals.pending().is_some()
    });
    draw(cx, window);

    let (title, text) = window
        .read_with(cx, |workspace, _| workspace.last_notification())
        .unwrap()
        .expect("the question is a notification");
    assert!(title.contains("Allow an agent tool"), "{title}");
    assert!(
        text.contains("Fixture Client wants to run tests: demo.Vitest"),
        "{text}"
    );
    let serial = window
        .read_with(cx, |workspace, _| workspace.notification_serial())
        .unwrap();

    let ran = window
        .update(cx, |workspace, window, cx| {
            workspace.run_command("ai.mcp.allow", window, cx)
        })
        .unwrap();
    assert!(ran, "the button of the notification runs by id");
    wait_for_signal(cx, "the answer of the user", |_| call.is_finished());
    assert_eq!(call.join().expect("the call"), ApprovalAnswer::Once);
    let closed = window
        .read_with(cx, |workspace, _| workspace.notification_serial())
        .unwrap();
    assert!(closed > serial, "the notification closes with the answer");
}

/// Without a question on screen the answer commands do nothing: no notification opens or
/// closes, and the window keeps its state.
#[ui_kit::gpui::test]
fn the_answer_commands_do_nothing_without_a_question(cx: &mut TestAppContext) {
    let (_root, window) = open_sample_window(cx, "mcp-approval-idle");
    window
        .update(cx, |workspace, _, cx| {
            workspace.notify_user("Keep", "This notification stays", cx)
        })
        .unwrap();
    draw(cx, window);
    let serial = window
        .read_with(cx, |workspace, _| workspace.notification_serial())
        .unwrap();

    for id in ["ai.mcp.allow", "ai.mcp.allow_session", "ai.mcp.deny"] {
        let ran = window
            .update(cx, |workspace, window, cx| {
                workspace.run_command(id, window, cx)
            })
            .unwrap();
        assert!(ran, "{id} runs by id");
        cx.run_until_parked();
        draw(cx, window);
    }

    let after = window
        .read_with(cx, |workspace, _| workspace.notification_serial())
        .unwrap();
    assert_eq!(after, serial, "the commands changed no notification");
    let (title, text) = window
        .read_with(cx, |workspace, _| workspace.last_notification())
        .unwrap()
        .expect("the notification of the window itself is still there");
    assert_eq!(title.to_string(), "Keep");
    assert_eq!(text.to_string(), "This notification stays");
}
