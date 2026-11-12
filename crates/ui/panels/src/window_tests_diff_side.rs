//! The side-by-side diff without a window: the two panes, the strip between them, the
//! words inside changed lines, editing the right pane and the Unified switch.

use std::fs;

use editor_view::EditorView;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use workspace::{Settings, Workspace};

use crate::diff_view::DiffView;
use crate::window_tests::{
    click, draw, editor_of, open_path, open_window, press, type_text, with_window,
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

fn has(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) -> bool {
    VisualTestContext::from_window(window.into(), cx)
        .debug_bounds(selector)
        .is_some()
}

/// Opens `main.txt`, lets the window find the repository and shows its diff.
fn open_diff(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    root: &std::path::Path,
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

fn write_committed(root: &std::path::Path, text: &str) {
    fs::write(root.join("main.txt"), text).unwrap();
    let git = git();
    git.run(root, ["add", "main.txt"]).unwrap();
    git.run(root, ["commit", "-m", "base"]).unwrap();
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

#[ui_kit::gpui::test]
fn the_diff_shows_two_panes_and_marks_the_words_inside_a_changed_line(cx: &mut TestAppContext) {
    let (root, _bare, _git) = repository_with_origin("diff-side-panes");
    write_committed(&root, "alpha\ncolour = red\nbeta\n");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    wait_for(cx, "the repository", |cx| {
        git_state(cx).read_with(cx, |state, _| state.repo().is_some())
    });
    let path = root.join("main.txt");
    open_path(cx, window, &path);
    let editor = editor_of(cx, window, &path);
    replace(cx, &editor, 15..18, "blue");
    let view = open_diff(cx, window, &root);
    wait_for(cx, "the changed line", |cx| {
        view.read_with(cx, |view, _| view.hunk_count()) == 1
    });

    assert!(view.read_with(cx, |view, _| view.side_by_side()));
    let rows = view.read_with(cx, |view, _| view.side_rows_text());
    let at = rows
        .iter()
        .position(|(left, _)| left.as_deref() == Some("colour = red"))
        .expect("the left pane has the line before the change");
    assert_eq!(
        rows[at].1.as_deref(),
        Some("colour = blue"),
        "the changed line faces the line it came from"
    );

    let marks = view.read_with(cx, |view, _| view.word_marks());
    let (left, right) = marks
        .iter()
        .find(|(_, right)| {
            right
                .as_ref()
                .is_some_and(|(text, _)| text == "colour = blue")
        })
        .expect("the changed line has word marks");
    assert_eq!(
        left.as_ref().map(|(_, words)| words.clone()),
        Some(vec!["red".to_owned()]),
        "the word before the change is marked"
    );
    assert_eq!(
        right.as_ref().map(|(_, words)| words.clone()),
        Some(vec!["blue".to_owned()]),
        "the word after the change is marked"
    );
}

#[ui_kit::gpui::test]
fn the_right_arrow_of_the_strip_takes_the_hunk_back(cx: &mut TestAppContext) {
    let (root, _bare, _git) = repository_with_origin("diff-side-apply");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let path = root.join("main.txt");
    open_path(cx, window, &path);
    let editor = editor_of(cx, window, &path);
    replace(cx, &editor, 0..5, "ALPHA");
    let view = open_diff(cx, window, &root);
    wait_for(cx, "the changed line", |cx| {
        view.read_with(cx, |view, _| view.hunk_count()) == 1
    });

    click(cx, window, "diff-apply-left-0");

    wait_for(cx, "the hunk to go back", |cx| {
        fs::read_to_string(&path).unwrap().replace("\r\n", "\n") == "alpha\nbeta\n"
            && view.read_with(cx, |view, _| view.hunk_count()) == 0
    });
}

#[ui_kit::gpui::test]
fn typing_in_the_right_pane_edits_the_open_buffer_and_reads_the_diff_again(
    cx: &mut TestAppContext,
) {
    let (root, _bare, _git) = repository_with_origin("diff-side-edit");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let path = root.join("main.txt");
    open_path(cx, window, &path);
    let editor = editor_of(cx, window, &path);
    let view = open_diff(cx, window, &root);
    assert_eq!(view.read_with(cx, |view, _| view.hunk_count()), 0);
    assert_eq!(
        view.read_with(cx, |view, _| view.pane()),
        "right",
        "the working file opens with the right pane focused"
    );

    editor.update(cx, |editor, cx| editor.restore_caret(0, 0, cx));
    type_text(cx, window, "Z");

    wait_for(cx, "the edited buffer", |cx| {
        editor.read_with(cx, |editor, _| {
            editor.text().is_some_and(|text| text.starts_with("Zalpha"))
        })
    });
    wait_for(cx, "the diff of the edited buffer", |cx| {
        view.read_with(cx, |view, _| {
            view.hunk_count() == 1 && view.current().starts_with("Zalpha")
        })
    });
}

#[ui_kit::gpui::test]
fn tab_moves_the_keys_between_the_panes(cx: &mut TestAppContext) {
    let (root, _bare, _git) = repository_with_origin("diff-side-tab");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let path = root.join("main.txt");
    open_path(cx, window, &path);
    let editor = editor_of(cx, window, &path);
    replace(cx, &editor, 0..5, "ALPHA");
    let view = open_diff(cx, window, &root);

    assert_eq!(view.read_with(cx, |view, _| view.pane()), "right");
    press(cx, window, "ctrl-shift-tab");
    assert_eq!(view.read_with(cx, |view, _| view.pane()), "left");
    press(cx, window, "ctrl-shift-tab");
    assert_eq!(view.read_with(cx, |view, _| view.pane()), "right");
}

#[ui_kit::gpui::test]
fn the_layout_switch_returns_the_unified_list_and_keeps_the_choice(cx: &mut TestAppContext) {
    let (root, _bare, _git) = repository_with_origin("diff-side-layout");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let path = root.join("main.txt");
    open_path(cx, window, &path);
    let editor = editor_of(cx, window, &path);
    replace(cx, &editor, 0..5, "ALPHA");
    let view = open_diff(cx, window, &root);
    wait_for(cx, "the changed line", |cx| {
        view.read_with(cx, |view, _| view.hunk_count()) == 1
    });
    assert!(has(cx, window, "diff-side-by-side"));

    click(cx, window, "diff-layout");
    draw(cx, window);

    assert!(!view.read_with(cx, |view, _| view.side_by_side()));
    assert!(!cx.read(|cx| cx.global::<Settings>().diff_side_by_side));
    assert!(has(cx, window, "diff-unified"));
    assert!(!has(cx, window, "diff-side-by-side"));
    assert!(
        has(cx, window, "diff-revert-0"),
        "the unified list keeps its hunk buttons"
    );
}
