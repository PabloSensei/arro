//! Paths the system opens: a file of the project becomes a tab of its window, a path
//! outside the project a project of its own. The choice of the window is the same on
//! every system.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use ui_kit::gpui;
use ui_kit::gpui::TestAppContext;

use workspace::{OpenRequests, Request};

use crate::window_tests::{draw, open_sample_window, sample_project, wait_for_signal};

#[ui_kit::gpui::test]
fn a_file_of_the_project_opens_as_a_tab_of_its_window(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "platform-open-inside");
    let file = root.join("src").join("notes.txt");

    window
        .update(cx, |workspace, window, cx| {
            workspace.accept_request(Request::Open(file.clone()), window, cx);
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);

    let active = window
        .update(cx, |workspace, _, _| workspace.active_path())
        .unwrap();
    assert_eq!(active, Some(file));
}

#[ui_kit::gpui::test]
fn a_file_outside_the_project_opens_a_project_of_its_own(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "platform-open-outside");
    let other = sample_project("platform-open-other");
    let file = other.join("src").join("main.txt");
    let opened: Rc<RefCell<Vec<PathBuf>>> = Rc::new(RefCell::new(Vec::new()));
    let watched = Rc::clone(&opened);
    window
        .update(cx, |workspace, _, _| {
            workspace.set_project_opener(Rc::new(move |path, _| {
                watched.borrow_mut().push(path.to_path_buf());
            }));
        })
        .unwrap();

    window
        .update(cx, |workspace, window, cx| {
            workspace.accept_request(Request::Open(file.clone()), window, cx);
        })
        .unwrap();
    cx.run_until_parked();

    // Without a window that owns the file, Arro opens the folder of the file as a
    // project and the file itself on top of it.
    assert_eq!(*opened.borrow(), [file.parent().unwrap().to_path_buf()]);
}

#[ui_kit::gpui::test]
fn the_folder_of_the_window_does_not_open_a_second_one(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "platform-open-folder");
    let opened: Rc<RefCell<Vec<PathBuf>>> = Rc::new(RefCell::new(Vec::new()));
    let watched = Rc::clone(&opened);
    window
        .update(cx, |workspace, _, _| {
            workspace.set_project_opener(Rc::new(move |path, _| {
                watched.borrow_mut().push(path.to_path_buf());
            }));
        })
        .unwrap();

    window
        .update(cx, |workspace, window, cx| {
            workspace.accept_request(Request::Open(root), window, cx);
        })
        .unwrap();
    cx.run_until_parked();

    assert!(opened.borrow().is_empty(), "the window is already open");
}

/// The queue wakes the window only for a request: an empty one costs no timer and no
/// frame, so a window left open asks nothing until the system hands it a path.
#[ui_kit::gpui::test]
fn an_empty_queue_does_not_wake_the_window(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "platform-open-idle");
    let requests = OpenRequests::default();
    window
        .update(cx, |workspace, window, cx| {
            workspace.watch_open_requests(requests.clone(), window, cx);
        })
        .unwrap();
    cx.run_until_parked();
    let before = window
        .read_with(cx, |workspace, _| workspace.open_request_wakes_for_test())
        .unwrap();
    assert_eq!(before, 0, "the wait itself is not a wake");

    cx.executor().advance_clock(Duration::from_secs(2));
    cx.run_until_parked();
    let after = window
        .read_with(cx, |workspace, _| workspace.open_request_wakes_for_test())
        .unwrap();
    assert_eq!(after, before, "an empty queue woke the window");
}

/// A path that the system pushes after the window has settled is taken by the waiting
/// task: the listener of the platform wakes it, and the file opens as a tab.
#[ui_kit::gpui::test]
fn a_queued_path_reaches_the_window(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "platform-open-queued");
    let requests = OpenRequests::default();
    window
        .update(cx, |workspace, window, cx| {
            workspace.watch_open_requests(requests.clone(), window, cx);
        })
        .unwrap();
    cx.run_until_parked();

    let file = root.join("src").join("notes.txt");
    requests.push(Request::Open(file.clone()));
    wait_for_signal(cx, "the queued file", |cx| {
        cx.run_until_parked();
        window
            .read_with(cx, |workspace, _| workspace.active_path())
            .unwrap()
            .as_ref()
            == Some(&file)
    });
    let wakes = window
        .read_with(cx, |workspace, _| workspace.open_request_wakes_for_test())
        .unwrap();
    assert!(wakes >= 1, "the queue woke the window");
}

#[ui_kit::gpui::test]
fn a_focus_request_leaves_the_tabs_alone(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "platform-open-focus");
    let before = window
        .update(cx, |workspace, _, _| workspace.active_path())
        .unwrap();

    window
        .update(cx, |workspace, window, cx| {
            workspace.accept_request(Request::Focus, window, cx);
        })
        .unwrap();
    cx.run_until_parked();

    let after = window
        .update(cx, |workspace, _, _| workspace.active_path())
        .unwrap();
    assert_eq!(before, after);
}
