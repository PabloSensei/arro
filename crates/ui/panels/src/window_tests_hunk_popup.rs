//! The window of a change and the marks of Git on the scroll bar, without a window: the
//! old text of a hunk, its buttons, the keys between the hunks and the rollback.

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use editor_view::{EditorView, StripeKind};
use ui_kit::gpui;
use ui_kit::gpui::{Modifiers, TestAppContext, VisualTestContext, WindowHandle};
use ui_kit::{TOOLTIP_DELAY, point, px};
use workspace::Workspace;

use crate::hunk_popup::HunkPopup;
use crate::window_tests::{
    buffer_of, draw, editor_of, git_repository, open_path, open_window, press, sample_project,
};
use crate::window_tests_git::wait_for;

const LINES: usize = 200;

/// A repository with a file long enough for the bar of the editor, committed.
fn long_repository(name: &str) -> (PathBuf, PathBuf) {
    let root = git_repository(name);
    let path = root.join("main.txt");
    let mut text = String::from("alpha\n");
    for line in 1..LINES {
        text.push_str(&format!("line {line}\n"));
    }
    fs::write(&path, &text).unwrap();
    let git = crate::window_tests_git::git();
    git.run(&root, ["add", "main.txt"]).unwrap();
    git.run(&root, ["commit", "-m", "long"]).unwrap();
    (root, path)
}

fn setup(
    cx: &mut TestAppContext,
    name: &str,
) -> (
    PathBuf,
    WindowHandle<Workspace>,
    gpui::Entity<EditorView>,
    PathBuf,
) {
    let (root, path) = long_repository(name);
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    let git = cx.update(|cx| cx.global::<crate::git::ActiveGit>().0.clone());
    wait_for(cx, "the repository", |cx| {
        git.read_with(cx, |git, _| git.repo().is_some())
    });
    (root, window, view, path)
}

/// The byte range of a line of the text, without its line break.
fn line_range(text: &str, line: usize) -> std::ops::Range<usize> {
    let starts: Vec<usize> = std::iter::once(0)
        .chain(text.match_indices('\n').map(|(index, _)| index + 1))
        .collect();
    let start = starts[line];
    let end = starts
        .get(line + 1)
        .map_or(text.len(), |next| next - 1)
        .max(start);
    start..end
}

/// The byte range of a line of the text together with its line break.
fn line_with_break(text: &str, line: usize) -> std::ops::Range<usize> {
    let starts: Vec<usize> = std::iter::once(0)
        .chain(text.match_indices('\n').map(|(index, _)| index + 1))
        .collect();
    let start = starts[line];
    let end = starts.get(line + 1).copied().unwrap_or(text.len());
    start..end
}

fn popup_of(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
) -> Option<gpui::Entity<HunkPopup>> {
    window
        .read_with(cx, |workspace, _| workspace.overlay_view())
        .unwrap()
        .and_then(|view| view.downcast::<HunkPopup>().ok())
}

fn mark_center(
    cx: &TestAppContext,
    view: &gpui::Entity<EditorView>,
    line: usize,
) -> ui_kit::Point<ui_kit::Pixels> {
    view.read_with(cx, |view, _| view.change_mark_bounds(line))
        .unwrap_or_else(|| panic!("no change mark on line {line}"))
        .center()
}

/// Changes `line` to `text` of the same length, so that the offsets of other lines stay.
fn edit_line(cx: &mut TestAppContext, view: &gpui::Entity<EditorView>, line: usize, text: &str) {
    let source = buffer_of(cx, view);
    let range = line_range(&source, line);
    assert_eq!(range.len(), text.len(), "the edit keeps the line length");
    view.update(cx, |view, cx| view.replace_text(range, text, cx));
}

/// Waits until the gutter has a mark of the change of `line`.
fn wait_for_mark(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    view: &gpui::Entity<EditorView>,
    line: usize,
) {
    wait_for(cx, "the change marks", |cx| {
        draw(cx, window);
        view.read_with(cx, |view, _| {
            view.gutter_marks()
                .iter()
                .any(|mark| mark.lines.contains(&(line as u32)) || mark.lines.start == line as u32)
        })
    });
}

#[ui_kit::gpui::test]
fn a_change_marks_the_scroll_bar_before_the_save(cx: &mut TestAppContext) {
    let (_, window, view, _) = setup(cx, "hunk-marks");
    edit_line(cx, &view, 0, "ALPHA");

    wait_for(cx, "the marks of the bar", |cx| {
        draw(cx, window);
        view.read_with(cx, |view, _| {
            view.stripe_kinds()
                .iter()
                .any(|(kind, _)| *kind == StripeKind::Vcs)
        })
    });
    assert!(
        view.read_with(cx, |view, _| view.is_dirty()),
        "the marks come from the buffer, not from the disk"
    );
}

#[ui_kit::gpui::test]
fn hovering_a_change_mark_shows_the_old_text(cx: &mut TestAppContext) {
    let (_, window, view, _) = setup(cx, "hunk-hover");
    edit_line(cx, &view, 0, "ALPHA");
    wait_for_mark(cx, window, &view, 0);

    let center = mark_center(cx, &view, 0);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_move(center, None, Modifiers::default());
    drop(visual);
    cx.executor()
        .advance_clock(TOOLTIP_DELAY + Duration::from_millis(100));
    cx.run_until_parked();
    draw(cx, window);

    let popup = popup_of(cx, window).expect("the hover opens the window of the change");
    let old = popup.read_with(cx, |popup, _| popup.old_text().to_owned());
    assert_eq!(old, "alpha");

    // Leaving the mark closes the window after the grace for the move into it.
    let bounds = view
        .read_with(cx, |view, _| view.editor_bounds())
        .expect("the editor has its place");
    let away = point(bounds.origin.x + px(300.), bounds.origin.y + px(300.));
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_move(away, None, Modifiers::default());
    drop(visual);
    cx.executor().advance_clock(Duration::from_millis(400));
    cx.run_until_parked();
    draw(cx, window);
    assert!(
        popup_of(cx, window).is_none(),
        "the window leaves with the pointer"
    );

    let center = mark_center(cx, &view, 0);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_move(center, None, Modifiers::default());
    drop(visual);
    cx.executor()
        .advance_clock(TOOLTIP_DELAY + Duration::from_millis(100));
    cx.run_until_parked();
    draw(cx, window);
    assert!(popup_of(cx, window).is_some(), "the hover opens it again");

    press(cx, window, "escape");
    assert!(
        popup_of(cx, window).is_none(),
        "Escape closes the window of the change"
    );
}

#[ui_kit::gpui::test]
fn a_click_on_a_change_mark_opens_the_window_and_the_rollback_goes_back(cx: &mut TestAppContext) {
    let (_, window, view, _) = setup(cx, "hunk-rollback");
    edit_line(cx, &view, 0, "ALPHA");
    wait_for_mark(cx, window, &view, 0);

    let center = mark_center(cx, &view, 0);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_click(center, Modifiers::default());
    drop(visual);
    cx.run_until_parked();
    draw(cx, window);
    let popup = popup_of(cx, window).expect("the click opens the window of the change");
    assert_eq!(
        popup.read_with(cx, |popup, _| popup.old_text().to_owned()),
        "alpha"
    );

    crate::window_tests::click(cx, window, "hunk-popup-rollback");
    cx.run_until_parked();
    draw(cx, window);
    assert_eq!(
        view.read_with(cx, |view, _| view.line_text(0)),
        Some("alpha".to_owned()),
        "Rollback Lines puts the committed text back into the buffer"
    );
    assert!(
        popup_of(cx, window).is_none(),
        "the last change closes the window"
    );

    press(cx, window, "ctrl-z");
    assert_eq!(
        view.read_with(cx, |view, _| view.line_text(0)),
        Some("ALPHA".to_owned()),
        "the rollback is one undo step"
    );
}

#[ui_kit::gpui::test]
fn next_and_previous_change_walk_the_hunks(cx: &mut TestAppContext) {
    let (_, window, view, _) = setup(cx, "hunk-walk");
    edit_line(cx, &view, 0, "ALPHA");
    edit_line(cx, &view, 100, "LINE 100");
    wait_for_mark(cx, window, &view, 0);
    wait_for_mark(cx, window, &view, 100);
    // The caret followed the last edit: the first hunk comes back into sight.
    view.update(cx, |view, cx| view.go_to_line(0, cx));
    draw(cx, window);

    let center = mark_center(cx, &view, 0);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_click(center, Modifiers::default());
    drop(visual);
    cx.run_until_parked();
    draw(cx, window);
    let popup = popup_of(cx, window).expect("the click opens the window of the change");
    assert_eq!(popup.read_with(cx, |popup, _| popup.hunk_count()), 2);
    assert_eq!(popup.read_with(cx, |popup, _| popup.hunk_index()), 0);

    crate::window_tests::click(cx, window, "hunk-popup-next");
    draw(cx, window);
    assert_eq!(popup.read_with(cx, |popup, _| popup.hunk_index()), 1);
    assert_eq!(
        popup.read_with(cx, |popup, _| popup.old_text().to_owned()),
        "line 100"
    );

    // The key of Previous Change reaches the open window, as it does in IDEA.
    press(cx, window, "ctrl-alt-shift-up");
    draw(cx, window);
    assert_eq!(popup.read_with(cx, |popup, _| popup.hunk_index()), 0);
    assert_eq!(
        popup.read_with(cx, |popup, _| popup.old_text().to_owned()),
        "alpha"
    );

    crate::window_tests::click(cx, window, "hunk-popup-next");
    draw(cx, window);
    assert_eq!(popup.read_with(cx, |popup, _| popup.hunk_index()), 1);
    crate::window_tests::click(cx, window, "hunk-popup-previous");
    draw(cx, window);
    assert_eq!(popup.read_with(cx, |popup, _| popup.hunk_index()), 0);

    // The key of Rollback Lines takes the hunk of the open window back.
    press(cx, window, "ctrl-alt-z");
    cx.run_until_parked();
    draw(cx, window);
    assert_eq!(
        view.read_with(cx, |view, _| view.line_text(0)),
        Some("alpha".to_owned())
    );
    assert!(
        popup_of(cx, window).is_none(),
        "the window leaves when its last hunk goes away"
    );
}

#[ui_kit::gpui::test]
fn a_deleted_line_shows_the_text_it_had(cx: &mut TestAppContext) {
    let (_, window, view, _) = setup(cx, "hunk-deleted");
    let source = buffer_of(cx, &view);
    let range = line_with_break(&source, LINES - 1);
    view.update(cx, |view, cx| view.replace_text(range, "", cx));
    wait_for_mark(cx, window, &view, LINES - 1);
    assert_eq!(
        view.read_with(cx, |view, _| view
            .gutter_marks()
            .iter()
            .find(|mark| mark.lines.start == (LINES - 1) as u32)
            .map(|mark| mark.kind)),
        Some(editor_view::GutterKind::Deleted)
    );

    let center = mark_center(cx, &view, LINES - 1);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_click(center, Modifiers::default());
    drop(visual);
    cx.run_until_parked();
    draw(cx, window);
    let popup = popup_of(cx, window).expect("the click opens the window of the change");
    let old = popup.read_with(cx, |popup, _| popup.old_text().to_owned());
    assert_eq!(old, "line 199", "the removed line keeps its text");
}

#[ui_kit::gpui::test]
fn a_file_outside_git_has_no_marks_and_no_window(cx: &mut TestAppContext) {
    let root = sample_project("hunk-no-git");
    let path = root.join("src").join("main.txt");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);

    edit_line(cx, &view, 0, "ALPHA");
    cx.executor()
        .advance_clock(TOOLTIP_DELAY + Duration::from_millis(100));
    cx.run_until_parked();
    draw(cx, window);
    assert!(
        view.read_with(cx, |view, _| view.gutter_marks().is_empty()),
        "a file outside Git has no change marks"
    );
    assert!(
        !view
            .read_with(cx, |view, _| view.stripe_kinds())
            .iter()
            .any(|(kind, _)| *kind == StripeKind::Vcs),
        "and no marks on the bar"
    );

    // The room of the gutter where a change mark would stand.
    let (bounds, row) = view.read_with(cx, |view, _| {
        (
            view.editor_bounds(),
            view.row_paint().first().map(|row| row.top),
        )
    });
    let Some(bounds) = bounds else {
        panic!("the editor has its place");
    };
    let Some(row) = row else {
        panic!("the editor has a row");
    };
    let position = point(
        bounds.origin.x + px(60.),
        bounds.origin.y + px(row) + px(8.),
    );
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_move(position, None, Modifiers::default());
    visual.simulate_click(position, Modifiers::default());
    drop(visual);
    cx.executor()
        .advance_clock(TOOLTIP_DELAY + Duration::from_millis(100));
    cx.run_until_parked();
    draw(cx, window);
    assert!(
        popup_of(cx, window).is_none(),
        "no window opens without a repository"
    );
}
