//! The importance of a notification: an informational one goes away on its own, a
//! warning and an error stay until the user closes them, and the icon of the balloon
//! follows the importance.

use std::time::Duration;

use ui_kit::NotificationLevel;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use workspace::{NOTIFICATION_TIME, Workspace};

use crate::window_tests::{click, draw, open_sample_window};

fn has(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

fn last_level(cx: &TestAppContext, window: WindowHandle<Workspace>) -> Option<NotificationLevel> {
    window
        .read_with(cx, |workspace, _| workspace.last_notification_level())
        .unwrap()
}

/// Lets the timer of the balloon pass and draws the window again.
fn wait_out_the_notification(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    cx.executor()
        .advance_clock(NOTIFICATION_TIME + Duration::from_millis(100));
    cx.run_until_parked();
    draw(cx, window);
}

fn show(cx: &mut TestAppContext, window: WindowHandle<Workspace>, level: NotificationLevel) {
    window
        .update(cx, |workspace, _, cx| match level {
            NotificationLevel::Info => workspace.notify_user("Title", "Text", cx),
            NotificationLevel::Warning => workspace.notify_warning("Title", "Text", cx),
            NotificationLevel::Error => workspace.notify_error("Title", "Text", cx),
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
}

#[ui_kit::gpui::test]
fn an_info_notification_goes_away_after_its_time(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "notification-info");
    show(cx, window, NotificationLevel::Info);
    assert!(
        has(cx, window, "notification"),
        "the notification is on screen"
    );
    assert_eq!(last_level(cx, window), Some(NotificationLevel::Info));

    wait_out_the_notification(cx, window);
    assert!(
        !has(cx, window, "notification"),
        "an info notification closes itself after NOTIFICATION_TIME"
    );
}

#[ui_kit::gpui::test]
fn an_error_notification_stays_after_the_timer_and_closes_with_the_cross(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "notification-error");
    show(cx, window, NotificationLevel::Error);
    assert!(
        has(cx, window, "notification"),
        "the notification is on screen"
    );
    assert_eq!(last_level(cx, window), Some(NotificationLevel::Error));

    wait_out_the_notification(cx, window);
    assert!(
        has(cx, window, "notification"),
        "an error notification stays after the time an info one goes away"
    );

    click(cx, window, "notification-close");
    draw(cx, window);
    assert!(
        !has(cx, window, "notification"),
        "the cross of the balloon closes the error notification"
    );
}

#[ui_kit::gpui::test]
fn the_error_balloon_shows_another_icon_than_an_info_one(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "notification-icon");
    show(cx, window, NotificationLevel::Info);
    assert!(
        has(cx, window, NotificationLevel::Info.selector()),
        "an info notification shows the info icon"
    );
    assert!(
        !has(cx, window, NotificationLevel::Error.selector()),
        "an info notification shows no error icon"
    );

    show(cx, window, NotificationLevel::Error);
    assert!(
        has(cx, window, NotificationLevel::Error.selector()),
        "an error notification shows the error icon"
    );
    assert!(
        !has(cx, window, NotificationLevel::Info.selector()),
        "an error notification shows no info icon"
    );
}

#[ui_kit::gpui::test]
fn a_warning_notification_stays_after_the_timer(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "notification-warning");
    show(cx, window, NotificationLevel::Warning);
    assert_eq!(last_level(cx, window), Some(NotificationLevel::Warning));

    wait_out_the_notification(cx, window);
    assert!(
        has(cx, window, "notification"),
        "a warning notification stays after the time an info one goes away"
    );
}
