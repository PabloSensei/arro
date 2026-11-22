//! The list of frames of the Debugger: a frame of the project is bright, a frame of a
//! class of a library is grey, its whole place is in the tooltip, and the eye folds the
//! library frames of a `jdt://` buffer as it folds the other ones.
//!
//! `cargo test -p panels window_tests_debug_frames`

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use dap_client::{DebugState, Frame, Snapshot, Thread};
use ui_kit::Theme;
use ui_kit::gpui::{self, TestAppContext};
use workspace::{DEBUG_TOOL, Workspace};

use crate::debug::{clear_frame_colors, frame_text_color};
use crate::window_tests::{active_path, click, draw, open_sample_window};
use crate::window_tests_debug_actions::{debug_panel, shown};
use crate::window_tests_tooltips::hover;

/// The source of a class of a library as the server names it: a module and a package in
/// the path, percent escapes in both, and no file of the project.
const CLASS_SOURCE: &str = "jdt://contents/runtime.base/runtime.lang.reflect/Method.sample\
                           ?=runtime.base%2Fruntime.lang.reflect%2FMethod.sample";

fn stack(root: &Path) -> Snapshot {
    let project = root.join("src").join("Controller.sample");
    Snapshot {
        state: DebugState::Stopped {
            reason: "Breakpoint".to_owned(),
        },
        threads: Arc::new(vec![Thread {
            id: 1,
            name: "main".to_owned(),
        }]),
        current_thread: Some(1),
        frames: vec![
            Frame {
                id: 1,
                name: "demo.Controller.processFindForm(int,demo.Model,demo.Result)".to_owned(),
                source: Some(project),
                line: 107,
                column: 0,
            },
            Frame {
                id: 2,
                name: "Method.invoke(Object,Object[])".to_owned(),
                source: Some(PathBuf::from(CLASS_SOURCE)),
                line: 569,
                column: 0,
            },
        ],
        selected_frame: Some(1),
        stops: 1,
        ..Snapshot::default()
    }
}

fn show_debug(cx: &mut TestAppContext, window: gpui::WindowHandle<Workspace>) {
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(DEBUG_TOOL, window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    // The bottom dock slides in: the click of a test lands on the settled frame.
    cx.executor().advance_clock(Duration::from_millis(300));
    cx.run_until_parked();
    draw(cx, window);
}

#[ui_kit::gpui::test]
fn a_project_frame_stays_bright_and_a_class_frame_is_grey_with_its_full_place(
    cx: &mut TestAppContext,
) {
    let (root, window) = open_sample_window(cx, "debug-frames-colors");
    let debug = debug_panel(cx);
    debug.update(cx, |panel, cx| {
        panel.show_snapshot_for_test(stack(&root), cx)
    });
    show_debug(cx, window);

    // The eye folds the frames of the buffer as it folds the other library frames.
    assert!(
        shown(cx, window, "frames-group-2"),
        "the frame of the buffer is in the folded group"
    );
    assert!(
        !shown(cx, window, "frame-2"),
        "the frame of the buffer is hidden until the group opens"
    );
    assert!(
        shown(cx, window, "frame-1"),
        "the frame of the project stays in the list"
    );

    click(cx, window, "debug-hide-libraries");
    draw(cx, window);
    assert!(
        shown(cx, window, "frame-2"),
        "the eye shows the frame of the buffer"
    );

    clear_frame_colors();
    draw(cx, window);
    let (text, secondary) = cx.read(|cx| {
        let theme = cx.global::<Theme>();
        (theme.text, theme.text_secondary)
    });
    assert_eq!(
        frame_text_color(1),
        Some(text),
        "the frame of the project is drawn in the text colour"
    );
    assert_eq!(
        frame_text_color(2),
        Some(secondary),
        "the frame of a library is drawn grey"
    );

    let (hint, key) = hover(cx, window, "frame-2");
    assert_eq!(hint, "invoke:569, Method (runtime.lang.reflect)");
    assert_eq!(key, None, "a frame place has no key");

    let before = active_path(cx, window);
    click(cx, window, "frame-2");
    cx.run_until_parked();
    assert_eq!(
        active_path(cx, window),
        before,
        "a class frame without a language opens nothing and says nothing"
    );
}
