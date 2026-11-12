//! The diff as two editors without a window: the text of the file in the right pane is
//! the buffer of its tab, the left pane is read only, the rows of a hunk line up with
//! empty rows, the two panes scroll together and the Local History window shows the same
//! pair of editors.

use std::fs;
use std::path::Path;

use editor_view::EditorView;
use ui_kit::Focusable;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use workspace::Workspace;

use crate::diff_view::DiffView;
use crate::window_tests::{
    bounds_of, draw, editor_of, line_height, open_path, open_window, press, type_text, with_window,
};
use crate::window_tests_git::{git, git_state, repository_with_origin, wait_for};

fn diff_view(cx: &mut TestAppContext, window: WindowHandle<Workspace>) -> gpui::Entity<DiffView> {
    window
        .read_with(cx, |workspace, _| {
            workspace
                .overlay_view()
                .and_then(|view| view.downcast::<DiffView>().ok())
        })
        .unwrap()
        .expect("the diff popup is open")
}

fn replace(
    cx: &mut TestAppContext,
    editor: &gpui::Entity<EditorView>,
    range: std::ops::Range<usize>,
    text: &str,
) {
    editor.update(cx, |editor, cx| {
        editor.replace_text(range, text, cx);
        assert!(editor.save_blocking(cx).is_none());
    });
}

/// Opens `main.txt`, waits for the repository and shows the diff of the file.
fn open_diff(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    root: &Path,
) -> gpui::Entity<DiffView> {
    let state = git_state(cx);
    wait_for(cx, "the repository", |cx| {
        state.read_with(cx, |state, _| state.repo().is_some())
    });
    let path = root.join("main.txt");
    open_path(cx, window, &path);
    let weak = window.root(cx).unwrap().downgrade();
    let shown = path.clone();
    with_window(cx, window, move |window, cx| {
        DiffView::open(state.clone(), weak, shown, window, cx);
    });
    cx.run_until_parked();
    draw(cx, window);
    diff_view(cx, window)
}

/// The point of a text column and a buffer line inside one of the two diff panes.
fn pane_point(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &'static str,
    line: usize,
    column: usize,
) -> gpui::Point<gpui::Pixels> {
    let height = line_height(cx);
    let bounds = bounds_of(cx, window, selector);
    gpui::point(
        bounds.left() + gpui::px(67. + column as f32 * 8.),
        bounds.top() + gpui::px(height * line as f32 + height / 2.),
    )
}

fn drag_in(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &'static str,
    from: (usize, usize),
    to: (usize, usize),
) {
    let from = pane_point(cx, window, selector, from.0, from.1);
    let to = pane_point(cx, window, selector, to.0, to.1);
    let modifiers = gpui::Modifiers::default();
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_move(from, None, modifiers);
    visual.simulate_mouse_down(from, gpui::MouseButton::Left, modifiers);
    for step in 1..=8 {
        let fraction = step as f32 / 8.;
        visual.simulate_mouse_move(
            gpui::point(
                from.x + (to.x - from.x) * fraction,
                from.y + (to.y - from.y) * fraction,
            ),
            Some(gpui::MouseButton::Left),
            modifiers,
        );
    }
    visual.simulate_mouse_up(to, gpui::MouseButton::Left, modifiers);
    cx.run_until_parked();
    draw(cx, window);
}

fn selected_text(cx: &TestAppContext, view: &gpui::Entity<EditorView>) -> String {
    let text = view
        .read_with(cx, |view, _| view.text())
        .unwrap_or_default();
    let (range, _) = view
        .read_with(cx, |view, _| view.selection())
        .expect("the file is read");
    text[range].to_string()
}

#[ui_kit::gpui::test]
fn the_keys_walk_the_hunks_when_the_file_has_no_tab(cx: &mut TestAppContext) {
    let (root, _bare, _git) = repository_with_origin("diff-editor-keys");
    fs::write(root.join("main.txt"), "ALPHA\nbeta\nGAMMA\n").unwrap();
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for(cx, "the repository", |cx| {
        state.read_with(cx, |state, _| state.repo().is_some())
    });
    let path = root.join("main.txt");
    let weak = window.root(cx).unwrap().downgrade();
    let shown = path.clone();
    with_window(cx, window, move |window, cx| {
        DiffView::open(state.clone(), weak, shown, window, cx);
    });
    cx.run_until_parked();
    draw(cx, window);
    let view = diff_view(cx, window);
    assert_eq!(view.read_with(cx, |view, _| view.hunk_count()), 2);
    let right = view.read_with(cx, |view, _| view.right_editor());
    let focused = window
        .update(cx, |_, window, cx| window.focused(cx))
        .unwrap();
    assert_eq!(
        focused,
        Some(right.read_with(cx, |view, cx| view.focus_handle(cx))),
        "the right pane of a file without a tab holds the keys"
    );
    press(cx, window, "f7");
    assert_eq!(view.read_with(cx, |view, _| view.hunk_index()), 1);
}

fn commit(root: &Path, text: &str) {
    fs::write(root.join("main.txt"), text).unwrap();
    let git = git();
    git.run(root, ["add", "main.txt"]).unwrap();
    git.run(root, ["commit", "-m", "base"]).unwrap();
}

#[ui_kit::gpui::test]
fn empty_rows_put_the_lines_of_a_hunk_face_to_face(cx: &mut TestAppContext) {
    let (root, _bare, _git) = repository_with_origin("diff-editor-fillers");
    commit(&root, "a\nb\nc\nd\ne");
    fs::write(root.join("main.txt"), "a\ne").unwrap();
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for(cx, "the repository", |cx| {
        state.read_with(cx, |state, _| state.repo().is_some())
    });
    let path = root.join("main.txt");
    let weak = window.root(cx).unwrap().downgrade();
    let shown = path.clone();
    with_window(cx, window, move |window, cx| {
        DiffView::open(state.clone(), weak, shown, window, cx);
    });
    cx.run_until_parked();
    draw(cx, window);
    let view = diff_view(cx, window);
    wait_for(cx, "the deleted lines", |cx| {
        view.read_with(cx, |view, _| view.hunk_count()) == 1
    });

    let right = view.read_with(cx, |view, _| view.right_editor());
    let side_rows = view.read_with(cx, |view, _| view.side_rows_text());
    let right_missing = side_rows
        .iter()
        .filter(|(_, right)| right.is_none())
        .count();
    let left_missing = side_rows.iter().filter(|(left, _)| left.is_none()).count();
    assert!(right_missing > 1, "the change takes more than one line");
    assert_eq!(left_missing, 0);
    let rows: Vec<(usize, bool)> = right.read_with(cx, |view, _| {
        view.row_paint()
            .into_iter()
            .map(|row| (row.line, row.block))
            .collect()
    });
    assert_eq!(
        rows.iter().filter(|(_, block)| *block).count(),
        right_missing,
        "the right pane has an empty row for every line it has none of: {rows:?}"
    );
    let left = view.read_with(cx, |view, _| view.left_editor());
    assert!(
        left.read_with(cx, |view, _| view.row_paint())
            .iter()
            .all(|row| !row.block),
        "the left pane has a line of its own for every row"
    );
}

#[ui_kit::gpui::test]
fn the_two_panes_scroll_together(cx: &mut TestAppContext) {
    let (root, _bare, _git) = repository_with_origin("diff-editor-scroll");
    let mut base = String::new();
    for line in 0..60 {
        base.push_str(&format!("line {line}\n"));
    }
    commit(&root, &base);
    let mut current = base.clone();
    current = current.replace("line 50\n", "changed 50\n");
    fs::write(root.join("main.txt"), current).unwrap();
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for(cx, "the repository", |cx| {
        state.read_with(cx, |state, _| state.repo().is_some())
    });
    let path = root.join("main.txt");
    let weak = window.root(cx).unwrap().downgrade();
    let shown = path.clone();
    with_window(cx, window, move |window, cx| {
        DiffView::open(state.clone(), weak, shown, window, cx);
    });
    cx.run_until_parked();
    draw(cx, window);
    let view = diff_view(cx, window);
    wait_for(cx, "the changed line", |cx| {
        view.read_with(cx, |view, _| view.hunk_count()) == 1
    });
    let right = view.read_with(cx, |view, _| view.right_editor());
    let left = view.read_with(cx, |view, _| view.left_editor());
    let position = bounds_of(cx, window, "diff-pane-right").center();
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_event(gpui::ScrollEvent {
        position,
        delta: gpui::ScrollDelta::Lines(gpui::point(0., -6.)),
        modifiers: gpui::Modifiers::default(),
        ..Default::default()
    });
    cx.run_until_parked();
    draw(cx, window);
    let (right_top, left_top) = (
        right.read_with(cx, |view, _| view.scroll_top()),
        left.read_with(cx, |view, _| view.scroll_top()),
    );
    assert!(right_top > 0., "the wheel scrolled the right pane");
    assert!(
        (right_top - left_top).abs() < 0.5,
        "the left pane followed: {left_top} against {right_top}"
    );
}

#[ui_kit::gpui::test]
fn completion_of_the_language_works_in_the_right_pane(cx: &mut TestAppContext) {
    use crate::window_tests_lang::{FakeLanguage, open_window_with};

    let (root, _bare, _git) = repository_with_origin("diff-editor-completion");
    let language = FakeLanguage::new();
    let window = open_window_with(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    let path = root.join("main.txt");
    open_path(cx, window, &path);
    let view = open_diff(cx, window, &root);
    let right = view.read_with(cx, |view, _| view.right_editor());

    press(cx, window, "ctrl-end");
    type_text(cx, window, "g");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "completion-popup").size.height > gpui::px(0.),
        "typing in the right pane shows the list of the language"
    );
    press(cx, window, "enter");
    assert!(
        right
            .read_with(cx, |view, _| view.text())
            .is_some_and(|text| text.ends_with("greet")),
        "the entry of the list lands in the buffer of the right pane"
    );
}

#[ui_kit::gpui::test]
fn the_local_history_shows_the_version_and_the_file_side_by_side(cx: &mut TestAppContext) {
    let (root, window) = crate::window_tests::open_sample_window(cx, "history-diff-editor");
    let path = root.join("src").join("main.txt");
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    view.update(cx, |view, cx| {
        view.replace_text(0..5, "one", cx);
        assert!(view.save_blocking(cx).is_none());
    });
    cx.run_until_parked();
    view.update(cx, |view, cx| view.replace_text(0..3, "two", cx));
    window
        .update(cx, |workspace, window, cx| {
            crate::history_view::HistoryView::show(path.clone(), workspace, window, cx)
        })
        .unwrap();
    wait_for(cx, "the diff of the version", |cx| {
        cx.update(|cx| crate::history_view::report(cx))
            .iter()
            .any(|line| line == "history-diff hunks 1")
    });
    draw(cx, window);
    assert!(
        cx.update(|cx| crate::history_view::side_by_side(cx)),
        "the history window shows two editors"
    );
    assert!(bounds_of(cx, window, "history-pane-left").size.width > gpui::px(0.));
    assert!(bounds_of(cx, window, "history-pane-right").size.width > gpui::px(0.));
}

#[ui_kit::gpui::test]
fn the_right_pane_holds_the_keys_and_the_left_pane_is_read_only(cx: &mut TestAppContext) {
    let (root, _bare, _git) = repository_with_origin("diff-editor-focus");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let path = root.join("main.txt");
    open_path(cx, window, &path);
    let tab = editor_of(cx, window, &path);
    replace(cx, &tab, 0..5, "ALPHA");
    assert_eq!(
        tab.read_with(cx, |view, _| view.text()),
        Some("ALPHA\nbeta\n".to_owned())
    );
    let view = open_diff(cx, window, &root);
    wait_for(cx, "the changed line", |cx| {
        view.read_with(cx, |view, _| view.hunk_count()) == 1
    });

    assert_eq!(
        tab.read_with(cx, |view, _| view.text()),
        Some("ALPHA\nbeta\n".to_owned()),
        "the tab keeps the saved text"
    );
    let right = view.read_with(cx, |view, _| view.right_editor());
    assert_eq!(right, tab, "the right pane is the tab of the file");
    assert_eq!(
        right.read_with(cx, |view, _| view.text()),
        Some("ALPHA\nbeta\n".to_owned()),
        "the right pane shows the buffer of the tab"
    );
    let focused = window
        .update(cx, |_, window, cx| window.focused(cx))
        .unwrap();
    assert_eq!(
        focused,
        Some(right.read_with(cx, |view, cx| view.focus_handle(cx))),
        "the right pane holds the keys when the diff opens"
    );

    // A drag in the right pane selects text, and typing over it edits the buffer.
    drag_in(cx, window, "diff-pane-right", (0, 0), (1, 2));
    assert_eq!(selected_text(cx, &right), "ALPHA\nbe");
    type_text(cx, window, "x");
    assert_eq!(
        right.read_with(cx, |view, _| view.text()),
        Some("xta\n".to_owned())
    );

    // Ctrl+Z takes the typed text back, so the buffer is the text before it again.
    press(cx, window, "ctrl-z");
    assert_eq!(
        right.read_with(cx, |view, _| view.text()),
        Some("ALPHA\nbeta\n".to_owned())
    );

    // The left pane takes no input.
    press(cx, window, "ctrl-shift-tab");
    assert_eq!(view.read_with(cx, |view, _| view.pane()), "left");
    type_text(cx, window, "x");
    let left = view.read_with(cx, |view, _| view.left_editor());
    assert_eq!(
        left.read_with(cx, |view, _| view.text()),
        Some("alpha\nbeta\n".to_owned()),
        "the left pane keeps the version before"
    );
}
