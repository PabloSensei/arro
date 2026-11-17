//! The documentation under the completion list, as IDEA shows it: every label of a
//! Javadoc comment stands on a line of its own, and a long line wraps inside the box of
//! the list instead of running past its edge.

use lang_api::CompletionEntry;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use workspace::Workspace;

use crate::window_tests::{bounds_of, draw, line_height, open_path, sample_project, type_text};
use crate::window_tests_speed::{SpeedLanguage, open_window};

/// What a server answers about `String.toString()`: the comment carries its labels on
/// lines of their own, and the last line is longer than the box of the list.
fn documentation() -> String {
    let long = "A string that represents this object and keeps saying so until the end, ".repeat(4);
    format!(
        "Returns a string representation of this object.\nSpecified by:\ntoString in class Object\n\n{long}"
    )
}

fn has(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

#[ui_kit::gpui::test]
fn the_labels_of_the_documentation_stand_on_their_own_lines_and_a_long_one_wraps(
    cx: &mut TestAppContext,
) {
    let root = sample_project("completion-doc");
    let language = SpeedLanguage::new();
    language.set_entries(vec![CompletionEntry {
        label: "alpha".to_owned(),
        insert_text: Some("alpha".to_owned()),
        documentation: Some(documentation()),
        ..CompletionEntry::default()
    }]);
    let window = open_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    let path = root.join("src/main.txt");
    open_path(cx, window, &path);
    window
        .update(cx, |workspace, window, cx| {
            workspace.focus_editor(window, cx)
        })
        .unwrap();
    type_text(cx, window, "a");
    draw(cx, window);

    for selector in [
        "completion-doc-row-0",
        "completion-doc-row-1",
        "completion-doc-row-2",
        "completion-doc-row-3",
    ] {
        assert!(
            has(cx, window, selector),
            "{selector} stands under the list"
        );
    }
    assert!(
        !has(cx, window, "completion-doc-row-4"),
        "each line of the comment is a row of its own, and the labels are not beside the text"
    );

    let block = bounds_of(cx, window, "completion-doc");
    assert!(
        f32::from(block.size.width) <= 640.,
        "the documentation stays in the width of the list: {}",
        block.size.width
    );
    let long = bounds_of(cx, window, "completion-doc-row-3");
    assert!(
        f32::from(long.size.height) > line_height(cx) * 1.5,
        "the long line wraps instead of running past the edge: {}",
        long.size.height
    );
    assert!(
        f32::from(long.right() - block.right()) <= 1.,
        "the long line stops at the edge of the box: {} > {}",
        long.right(),
        block.right()
    );
}
