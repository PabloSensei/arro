//! T-0429: the hint of a row of a list of places stands at the right edge, so that the
//! lines of one list make a column, and the label of a row is cut before it reaches the
//! hint. A group keeps its hint right after the label, as it names the detail of a node.
//!
//! `cargo test -p panels window_tests_items_hint`

use lang_api::Severity;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use ui_kit::metrics::{ROW_PADDING_X, SPACE_M};
use ui_kit::px;
use workspace::{PROBLEMS_TOOL, Workspace};

use crate::problems::{ActiveProblems, Problem};
use crate::window_tests::{bounds_of, draw, open_path, open_sample_window};

fn show_problems(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(PROBLEMS_TOOL, window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
}

fn problem(line: usize, message: &str) -> Problem {
    Problem {
        line,
        column: 0,
        severity: Severity::Hint,
        message: message.to_owned(),
    }
}

/// The right edge of a hint stands on the right edge of the row, less its padding, and the
/// two rows of different labels keep one column.
#[ui_kit::gpui::test]
fn the_hint_of_every_row_ends_on_the_right_edge_of_the_row(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "items-hint-column");
    let main = root.join("src").join("main.txt");
    open_path(cx, window, &main);
    let problems = cx.update(|cx| cx.global::<ActiveProblems>().0.clone());
    problems.update(cx, |hub, cx| {
        hub.set_file_problems(
            main.clone(),
            vec![problem(1, "Typo: In word 'Hoeller'"), problem(4, "unused")],
            cx,
        );
    });
    show_problems(cx, window);

    let short_row = bounds_of(cx, window, "item-row-0");
    let long_row = bounds_of(cx, window, "item-row-1");
    let short_hint = bounds_of(cx, window, "item-0-hint");
    let long_hint = bounds_of(cx, window, "item-1-hint");

    let padding = f32::from(short_row.right() - short_hint.right());
    assert!(
        (padding - ROW_PADDING_X).abs() < 0.01,
        "the hint of a row ends {padding} px before the row, and the row keeps {ROW_PADDING_X} px of padding"
    );
    assert_eq!(
        short_hint.right(),
        long_hint.right(),
        "the hints of two rows make one column at the right edge"
    );
    assert_eq!(
        long_hint.size.width, short_hint.size.width,
        "the hint is short enough to keep its whole text"
    );
    assert!(
        short_hint.right() <= short_row.right() && long_hint.right() <= long_row.right(),
        "no hint leaves its row: {short_hint:?} in {short_row:?} and {long_hint:?} in {long_row:?}"
    );
}

/// A label longer than the room left by the hint is cut, and the hint keeps its whole text
/// and its column.
#[ui_kit::gpui::test]
fn a_long_label_is_cut_before_the_hint(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "items-hint-cut");
    let main = root.join("src").join("main.txt");
    open_path(cx, window, &main);
    let long = "A problem message that is much longer than the room left in the row of the list of places. ".repeat(4);
    let problems = cx.update(|cx| cx.global::<ActiveProblems>().0.clone());
    problems.update(cx, |hub, cx| {
        hub.set_file_problems(
            main.clone(),
            vec![problem(1, "unused"), problem(4, &long)],
            cx,
        );
    });
    show_problems(cx, window);

    let row = bounds_of(cx, window, "item-row-1");
    let short_hint = bounds_of(cx, window, "item-0-hint");
    let hint = bounds_of(cx, window, "item-1-hint");
    let label = bounds_of(cx, window, "item-1-label");

    assert!(
        label.right() <= hint.left(),
        "the label stops before the hint: {label:?} and {hint:?}"
    );
    assert!(
        (f32::from(hint.left() - label.right()) - SPACE_M).abs() < 0.01,
        "the hint starts one gap after the label"
    );
    assert!(
        label.size.width < row.size.width,
        "the label of the long message does not take the whole row: {label:?} in {row:?}"
    );
    assert_eq!(
        hint.size.width, short_hint.size.width,
        "the hint of the long row keeps its whole text"
    );
    assert_eq!(
        hint.right(),
        row.right() - px(ROW_PADDING_X),
        "the hint of the long row still ends on the right edge"
    );
}

/// A group keeps its hint right after its label: it names the detail of the node rather
/// than a column of the list.
#[ui_kit::gpui::test]
fn a_group_keeps_its_hint_right_after_its_label(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "items-hint-group");
    let main = root.join("src").join("main.txt");
    open_path(cx, window, &main);
    let problems = cx.update(|cx| cx.global::<ActiveProblems>().0.clone());
    problems.update(cx, |hub, cx| {
        hub.set_file_problems(
            main.clone(),
            vec![problem(1, "unused"), problem(4, "style")],
            cx,
        );
    });
    show_problems(cx, window);

    let label = bounds_of(cx, window, "group-0-label");
    let hint = bounds_of(cx, window, "group-0-hint");
    let gap = f32::from(hint.left() - label.right());
    assert!(
        (gap - SPACE_M).abs() < 0.01,
        "the hint of a group starts {gap} px after its label, and the row gives {SPACE_M} px"
    );
    assert!(
        hint.left() < label.right() + px(SPACE_M * 4.),
        "the hint of a group follows its label rather than standing in the right column: {hint:?}"
    );
}
