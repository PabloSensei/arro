//! The buttons of the stripes: each one shows its own popup or tool window, whatever was
//! open before it. The bookmarks popup opens empty with a state of its own, the same
//! button closes it, and Notifications shows the notifications rather than the bookmarks.

use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};

use workspace::{
    AI_TOOL, BOOKMARKS_POPUP, COMMIT_TOOL, DEBUG_TOOL, LOG_TOOL, PROBLEMS_TOOL, PROJECT_TOOL,
    RECENT_PLACES_POPUP, RUN_TOOL, SERVICES_TOOL, STRUCTURE_TOOL, TASKS_TOOL, TERMINAL_TOOL,
    TODO_TOOL, USAGES_TOOL, Workspace,
};

use crate::window_tests::{
    click, draw, editor_of, open_path, open_sample_window, press, type_text,
};

fn has(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

fn tag(cx: &TestAppContext, window: WindowHandle<Workspace>) -> Option<&'static str> {
    window
        .read_with(cx, |workspace, _| workspace.overlay_tag())
        .unwrap()
}

fn tool_open(cx: &TestAppContext, window: WindowHandle<Workspace>, tool: &str) -> bool {
    window
        .read_with(cx, |workspace, _| workspace.tool_open(tool))
        .unwrap()
}

fn notification_serial(cx: &TestAppContext, window: WindowHandle<Workspace>) -> usize {
    window
        .read_with(cx, |workspace, _| workspace.notification_serial())
        .unwrap()
}

/// What a button of the right stripe shows.
#[derive(Clone, Copy)]
enum Shows {
    Balloon,
    Tool(&'static str),
    Bookmarks,
}

fn shows(cx: &mut TestAppContext, window: WindowHandle<Workspace>, item: Shows) -> bool {
    match item {
        Shows::Balloon => has(cx, window, "notification"),
        Shows::Tool(tool) => tool_open(cx, window, tool),
        Shows::Bookmarks => tag(cx, window) == Some(BOOKMARKS_POPUP),
    }
}

/// The buttons of the right stripe, top to bottom.
const RIGHT: [(&str, Shows); 4] = [
    ("stripe-s-notifications", Shows::Balloon),
    ("stripe-s-ai", Shows::Tool(AI_TOOL)),
    ("stripe-s-tasks", Shows::Tool(TASKS_TOOL)),
    ("stripe-s-bookmarks", Shows::Bookmarks),
];

/// The buttons of the left stripe, the top group and the bottom one.
const LEFT: [(&str, &str); 11] = [
    ("stripe-s-project", PROJECT_TOOL),
    ("stripe-s-commit", COMMIT_TOOL),
    ("stripe-s-log", LOG_TOOL),
    ("stripe-s-structure", STRUCTURE_TOOL),
    ("stripe-s-run", RUN_TOOL),
    ("stripe-s-debug", DEBUG_TOOL),
    ("stripe-s-terminal", TERMINAL_TOOL),
    ("stripe-s-services", SERVICES_TOOL),
    ("stripe-s-todo", TODO_TOOL),
    ("stripe-s-problems", PROBLEMS_TOOL),
    ("stripe-s-usages", USAGES_TOOL),
];

/// Closes what the previous pair of a walk left open: a popup, a tool window or the
/// balloon, so that every pair starts from the same window.
fn reset(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    window
        .update(cx, |workspace, window, cx| {
            if workspace.has_overlay() {
                workspace.dismiss(window, cx);
            }
            for (_, item) in RIGHT {
                if let Shows::Tool(tool) = item
                    && workspace.tool_open(tool)
                {
                    workspace.toggle_tool(tool, window, cx);
                }
            }
            for (_, tool) in LEFT {
                if workspace.tool_open(tool) {
                    workspace.toggle_tool(tool, window, cx);
                }
            }
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
    if has(cx, window, "notification") {
        click(cx, window, "stripe-s-notifications");
        draw(cx, window);
    }
}

#[ui_kit::gpui::test]
fn the_empty_bookmarks_open_a_popup_and_notifications_show_their_balloon(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "stripe-bookmarks");
    assert!(
        cx.read(|cx| editor_view::bookmarks(cx).is_empty()),
        "the sample project has no bookmarks"
    );
    let serial = notification_serial(cx, window);

    click(cx, window, "stripe-s-bookmarks");
    draw(cx, window);
    assert_eq!(
        notification_serial(cx, window),
        serial,
        "the empty list of bookmarks is not a notification"
    );
    assert_eq!(
        tag(cx, window),
        Some(BOOKMARKS_POPUP),
        "the bookmarks popup is open, and the button is drawn as chosen while it is"
    );
    assert!(
        has(cx, window, "places-popup"),
        "the bookmarks popup is on screen"
    );
    assert!(
        has(cx, window, "places-empty"),
        "the popup itself says the list is empty"
    );

    click(cx, window, "stripe-s-notifications");
    draw(cx, window);
    assert!(
        has(cx, window, "notification"),
        "Notifications shows the balloon"
    );
    assert!(
        !has(cx, window, "places-popup"),
        "the bookmarks popup is closed"
    );
    let shown = window
        .read_with(cx, |workspace, _| workspace.last_notification())
        .unwrap();
    assert!(
        shown.as_ref().is_none_or(|(title, _)| title != "Bookmarks"),
        "the balloon is not the bookmark notification: {shown:?}"
    );
}

#[ui_kit::gpui::test]
fn the_bookmarks_button_closes_its_own_popup(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "stripe-bookmarks-toggle");
    draw(cx, window);

    click(cx, window, "stripe-s-bookmarks");
    draw(cx, window);
    assert!(has(cx, window, "places-popup"), "the first press opens it");

    // The press that dismisses a popup outside it is this same press: the button must
    // not open the popup again behind it.
    click(cx, window, "stripe-s-bookmarks");
    draw(cx, window);
    assert!(
        !has(cx, window, "places-popup"),
        "the second press closes the popup"
    );
    assert_eq!(tag(cx, window), None, "no popup is open");

    click(cx, window, "stripe-s-bookmarks");
    draw(cx, window);
    assert!(
        has(cx, window, "places-popup"),
        "the third press opens the popup again"
    );
}

#[ui_kit::gpui::test]
fn the_recent_places_still_open_their_own_popup(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "stripe-recent");
    let path = root.join("src").join("main.txt");
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    view.update(cx, |view, cx| view.restore_caret(1, 0, cx));
    type_text(cx, window, "z");
    view.update(cx, |view, cx| view.restore_caret(0, 0, cx));

    press(cx, window, "ctrl-shift-e");
    draw(cx, window);
    assert_eq!(
        tag(cx, window),
        Some(RECENT_PLACES_POPUP),
        "Recent Locations keeps its popup"
    );

    // The bookmarks replace a popup that is already open: the press that closes the
    // places must not take the bookmarks with it.
    click(cx, window, "stripe-s-bookmarks");
    draw(cx, window);
    assert_eq!(
        tag(cx, window),
        Some(BOOKMARKS_POPUP),
        "the bookmarks replace the places of the same popup class"
    );
    assert!(has(cx, window, "places-empty"));
}

#[ui_kit::gpui::test]
fn each_pair_of_the_right_stripe_leaves_the_second_button_in_front(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "stripe-pairs");
    draw(cx, window);

    for (first_index, (first, _)) in RIGHT.iter().enumerate() {
        for (second_index, (second, item)) in RIGHT.iter().enumerate() {
            if first_index == second_index {
                continue;
            }
            reset(cx, window);
            click(cx, window, first);
            draw(cx, window);
            assert!(
                shows(cx, window, RIGHT[first_index].1),
                "{first} opens its own window"
            );

            click(cx, window, second);
            draw(cx, window);
            assert!(
                shows(cx, window, *item),
                "{first} then {second}: the second button shows its own window"
            );
        }
    }
}

#[ui_kit::gpui::test]
fn each_button_of_the_left_stripe_shows_its_own_window_after_another_one(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "stripe-left-pairs");
    draw(cx, window);

    for (index, (second, tool)) in LEFT.iter().enumerate() {
        let (first, _) = LEFT[(index + LEFT.len() - 1) % LEFT.len()];
        reset(cx, window);
        click(cx, window, first);
        draw(cx, window);

        click(cx, window, second);
        draw(cx, window);
        assert!(
            tool_open(cx, window, tool),
            "{first} then {second}: the second button shows its own tool window"
        );
        assert!(
            !has(cx, window, "places-popup"),
            "{first} then {second}: no popup stays over the window"
        );
    }
}
