//! Editing and navigation of the editor without a language server, through the keys of
//! the IDEA keymap: members and statements, code blocks, lines and scrolling, the word
//! under the caret, highlighted usages, paths, tests and the changes against Git.

use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use editor_view::{EditorView, OutlineItem, OutlineKind};
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::Workspace;

use crate::window_tests::{
    active_path, buffer_of, draw, editor_of, git_repository, line_height, open_path, open_window,
    press, sample_project, scratch,
};

const LIMIT: Duration = Duration::from_secs(20);

fn wait_for(
    cx: &mut TestAppContext,
    what: &str,
    mut check: impl FnMut(&mut TestAppContext) -> bool,
) {
    let started = Instant::now();
    loop {
        if check(cx) {
            return;
        }
        assert!(started.elapsed() < LIMIT, "waited for {what} in vain");
        std::thread::sleep(Duration::from_millis(10));
        cx.executor().advance_clock(Duration::from_millis(50));
        cx.run_until_parked();
    }
}

fn open_text(
    cx: &mut TestAppContext,
    name: &str,
    text: &str,
) -> (PathBuf, WindowHandle<Workspace>, gpui::Entity<EditorView>) {
    let root = sample_project(name);
    let path = root.join("src").join("main.txt");
    fs::write(&path, text).unwrap();
    let window = open_window(cx, root);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    (path, window, view)
}

fn reset(
    cx: &mut TestAppContext,
    view: &gpui::Entity<EditorView>,
    text: &str,
    line: usize,
    column: usize,
) {
    let length = buffer_of(cx, view).len();
    view.update(cx, |view, cx| {
        view.replace_text(0..length, text, cx);
        view.restore_caret(line, column, cx);
    });
}

fn selection(cx: &TestAppContext, view: &gpui::Entity<EditorView>) -> std::ops::Range<usize> {
    view.read_with(cx, |view, _| view.selection())
        .expect("the file is read")
        .0
}

fn outline_item(
    name: &str,
    kind: OutlineKind,
    depth: u32,
    range: std::ops::Range<usize>,
) -> OutlineItem {
    let start = range.start;
    OutlineItem {
        name: name.to_owned(),
        signature: None,
        kind,
        depth,
        range,
        name_range: start..start + name.len(),
    }
}

#[ui_kit::gpui::test]
fn method_keys_visit_the_members_with_and_without_the_outline(cx: &mut TestAppContext) {
    const TEXT: &str = "class A {\n    int one;\n\n    void two() {\n        int local;\n    }\n\n    class Three {}\n}\n";
    let (_path, window, view) = open_text(cx, "members", TEXT);

    // Without an outline the brace levels say where the members begin.
    reset(cx, &view, TEXT, 1, 0);
    press(cx, window, "alt-down");
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (3, 0));
    press(cx, window, "alt-down");
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (7, 0));
    press(cx, window, "alt-up");
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (3, 0));

    // The outline is used when the language gives one.
    let one = TEXT.find("int one").unwrap();
    let two = TEXT.find("void two").unwrap();
    let three = TEXT.find("class Three").unwrap();
    reset(cx, &view, TEXT, 1, 0);
    view.update(cx, |view, cx| {
        view.set_outline(
            vec![
                outline_item("A", OutlineKind::Class, 0, 0..TEXT.len()),
                outline_item("one", OutlineKind::Field, 1, one..one + 8),
                outline_item("two", OutlineKind::Method, 1, two..three - 6),
                outline_item("Three", OutlineKind::Class, 1, three..TEXT.len() - 2),
            ],
            cx,
        );
    });
    press(cx, window, "alt-down");
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (3, 0));
    press(cx, window, "alt-down");
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (7, 0));
}

#[ui_kit::gpui::test]
fn move_statement_keys_move_statements_members_and_whole_blocks(cx: &mut TestAppContext) {
    const TEXT: &str = "class A {\n    void a() {\n        int first = 1;\n        int second = 2;\n    }\n\n    void b() {}\n}\n";
    let (_path, window, view) = open_text(cx, "move-statement", TEXT);

    // A statement swaps with its neighbour above, then comes back.
    reset(cx, &view, TEXT, 3, 0);
    press(cx, window, "ctrl-shift-up");
    assert_eq!(
        buffer_of(cx, &view),
        "class A {\n    void a() {\n        int second = 2;\n        int first = 1;\n    }\n\n    void b() {}\n}\n"
    );
    press(cx, window, "ctrl-shift-down");
    assert_eq!(buffer_of(cx, &view), TEXT);

    // The first statement does not leave its method.
    reset(cx, &view, TEXT, 2, 0);
    press(cx, window, "ctrl-shift-up");
    assert_eq!(buffer_of(cx, &view), TEXT);

    // A member of the class moves with its whole body.
    reset(cx, &view, TEXT, 1, 0);
    press(cx, window, "ctrl-shift-down");
    assert_eq!(
        buffer_of(cx, &view),
        "class A {\n    void b() {}\n\n    void a() {\n        int first = 1;\n        int second = 2;\n    }\n}\n"
    );
    press(cx, window, "ctrl-shift-up");
    assert_eq!(buffer_of(cx, &view), TEXT);
}

#[ui_kit::gpui::test]
fn move_statement_takes_a_selection_as_one_unit(cx: &mut TestAppContext) {
    const TEXT: &str =
        "class A {\n    void a() {\n        one();\n        two();\n        three();\n    }\n}\n";
    let (_path, window, view) = open_text(cx, "move-selection", TEXT);

    reset(cx, &view, TEXT, 2, 0);
    press(cx, window, "shift-down");
    press(cx, window, "shift-down");
    press(cx, window, "ctrl-shift-down");
    assert_eq!(
        buffer_of(cx, &view),
        "class A {\n    void a() {\n        three();\n        one();\n        two();\n    }\n}\n"
    );
}

#[ui_kit::gpui::test]
fn code_block_keys_go_to_the_braces_and_extend_the_selection(cx: &mut TestAppContext) {
    const TEXT: &str = "void m() {\n    if (x) {\n        body();\n    }\n}\n";
    let (_path, window, view) = open_text(cx, "code-block", TEXT);
    let if_open = TEXT.find("if (x) {").unwrap() + "if (x) ".len();
    let if_close = TEXT.find("\n    }").unwrap() + 5;
    let body = TEXT.find("        body();").unwrap() + 8;

    reset(cx, &view, TEXT, 2, 8);
    press(cx, window, "ctrl-[");
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (1, 11));
    press(cx, window, "ctrl-]");
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (3, 4));

    reset(cx, &view, TEXT, 2, 8);
    press(cx, window, "ctrl-shift-]");
    assert_eq!(selection(cx, &view), body..if_close);
    reset(cx, &view, TEXT, 2, 8);
    press(cx, window, "ctrl-shift-[");
    assert_eq!(selection(cx, &view).start, if_open);
}

#[ui_kit::gpui::test]
fn split_line_keeps_the_caret_and_scrolling_keeps_the_caret_in_view(cx: &mut TestAppContext) {
    let (_path, window, view) = open_text(cx, "lines", "abc\ndef\n");

    reset(cx, &view, "abc\ndef\n", 0, 1);
    press(cx, window, "ctrl-enter");
    assert_eq!(buffer_of(cx, &view), "a\nbc\ndef\n");
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (0, 1));

    let lines: String = (0..100).map(|line| format!("line {line}\n")).collect();
    reset(cx, &view, &lines, 10, 0);
    let before = view.read_with(cx, |view, _| view.scroll_top());
    press(cx, window, "ctrl-down");
    let after = view.read_with(cx, |view, _| view.scroll_top());
    assert!(after > before, "Ctrl+Down scrolls one line");
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (10, 0));
    assert_eq!(after - before, line_height(cx));

    press(cx, window, "ctrl-up");
    assert_eq!(view.read_with(cx, |view, _| view.scroll_top()), before);

    // A caret at the edge of the view moves with the text.
    reset(cx, &view, &lines, 0, 0);
    press(cx, window, "ctrl-up");
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (0, 0));
    press(cx, window, "ctrl-down");
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (1, 0));
}

#[ui_kit::gpui::test]
fn find_word_keys_search_the_word_under_the_caret(cx: &mut TestAppContext) {
    const TEXT: &str = "item item item\n";
    let (_path, window, view) = open_text(cx, "find-word", TEXT);

    reset(cx, &view, TEXT, 0, 0);
    press(cx, window, "ctrl-f3");
    assert_eq!(selection(cx, &view), 5..9);
    press(cx, window, "f3");
    assert_eq!(selection(cx, &view), 10..14);
    press(cx, window, "shift-f3");
    assert_eq!(selection(cx, &view), 5..9);
    press(cx, window, "ctrl-shift-f3");
    assert_eq!(selection(cx, &view), 0..4);
}

#[ui_kit::gpui::test]
fn highlight_usages_marks_the_whole_words_and_f3_walks_them(cx: &mut TestAppContext) {
    const TEXT: &str = "count counter count\ncount\n";
    let (_path, window, view) = open_text(cx, "usages", TEXT);

    reset(cx, &view, TEXT, 0, 0);
    press(cx, window, "ctrl-shift-f7");
    let ranges = view
        .read_with(cx, |view, _| view.highlighted_usages().map(<[_]>::to_vec))
        .expect("the usages are on");
    assert_eq!(ranges, [0..5, 14..19, 20..25]);

    press(cx, window, "f3");
    assert_eq!(selection(cx, &view), 14..19);
    press(cx, window, "f3");
    assert_eq!(selection(cx, &view), 20..25);
    press(cx, window, "shift-f3");
    assert_eq!(selection(cx, &view), 14..19);

    press(cx, window, "escape");
    assert!(
        view.read_with(cx, |view, _| view.highlighted_usages().is_none()),
        "Escape clears the highlight"
    );
}

#[ui_kit::gpui::test]
fn copy_paths_copies_the_tab_and_the_project_tree(cx: &mut TestAppContext) {
    let root = sample_project("copy-paths");
    let main = root.join("src").join("main.txt");
    let notes = root.join("src").join("notes.txt");
    let window = open_window(cx, root.clone());
    open_path(cx, window, &main);

    press(cx, window, "ctrl-shift-c");
    assert_eq!(
        clip(cx),
        Some(main.display().to_string()),
        "Ctrl+Shift+C copies the path of the current tab"
    );

    let panel = cx.update(|cx| cx.global::<crate::project::ActiveProjectPanel>().0.clone());
    cx.update(|cx| panel.update(cx, |panel, cx| panel.select_path(&notes, cx)));
    window
        .update(cx, |_, window, cx| {
            panel.update(cx, |panel, cx| window.focus(&panel.focus(), cx));
        })
        .unwrap();
    press(cx, window, "ctrl-shift-c");
    assert_eq!(clip(cx), Some(notes.display().to_string()));
}

fn clip(cx: &mut TestAppContext) -> Option<String> {
    cx.read(|cx| cx.read_from_clipboard().and_then(|item| item.text()))
}

#[ui_kit::gpui::test]
fn goto_test_opens_the_test_and_finds_its_way_back(cx: &mut TestAppContext) {
    let root = scratch("goto-test");
    fs::create_dir_all(root.join("src")).unwrap();
    let class = root.join("src").join("Foo.txt");
    let test = root.join("src").join("FooTest.txt");
    fs::write(&class, "class Foo {}\n").unwrap();
    fs::write(&test, "class FooTest {}\n").unwrap();
    let window = open_window(cx, root.clone());
    open_path(cx, window, &class);

    wait_for(cx, "the project index", |cx| {
        press(cx, window, "ctrl-shift-t");
        active_path(cx, window).as_deref() == Some(test.as_path())
    });
    assert_eq!(active_path(cx, window), Some(test.clone()));

    press(cx, window, "ctrl-shift-t");
    assert_eq!(active_path(cx, window), Some(class.clone()));
}

#[ui_kit::gpui::test]
fn goto_test_lists_several_tests_and_says_when_there_are_none(cx: &mut TestAppContext) {
    let root = scratch("goto-test-many");
    fs::create_dir_all(root.join("src")).unwrap();
    let class = root.join("src").join("Foo.txt");
    fs::write(&class, "class Foo {}\n").unwrap();
    fs::write(root.join("src").join("FooTest.txt"), "one\n").unwrap();
    fs::write(root.join("src").join("TestFoo.txt"), "two\n").unwrap();
    let window = open_window(cx, root.clone());
    open_path(cx, window, &class);

    wait_for(cx, "the list of tests", |cx| {
        press(cx, window, "ctrl-shift-t");
        window
            .read_with(cx, |workspace, _| workspace.has_overlay())
            .unwrap()
    });
    window
        .update(cx, |workspace, window, cx| workspace.close_menu(window, cx))
        .unwrap();

    let alone = scratch("goto-test-none");
    fs::create_dir_all(alone.join("src")).unwrap();
    fs::write(alone.join("src").join("Bar.txt"), "class Bar {}\n").unwrap();
    let window = open_window(cx, alone.clone());
    let bar = alone.join("src").join("Bar.txt");
    open_path(cx, window, &bar);
    wait_for(cx, "the message", |cx| {
        press(cx, window, "ctrl-shift-t");
        window
            .read_with(cx, |workspace, _| {
                workspace
                    .last_notification()
                    .is_some_and(|(_, text)| text.contains("No test found"))
            })
            .unwrap()
    });
}

#[ui_kit::gpui::test]
fn change_markers_walk_the_gutter_and_rollback_selected_lines(cx: &mut TestAppContext) {
    let root = git_repository("editing-git");
    let path = root.join("main.txt");
    fs::write(&path, "ALPHA\nbeta\nGAMMA\n").unwrap();
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    let git = cx.update(|cx| cx.global::<crate::git::ActiveGit>().0.clone());
    git.update(cx, |git, cx| git.refresh(cx));
    cx.run_until_parked();
    assert!(
        !view.read_with(cx, |view, _| view.gutter_marks().is_empty()),
        "the changed lines have marks"
    );

    reset(cx, &view, "ALPHA\nbeta\nGAMMA\n", 0, 0);
    press(cx, window, "ctrl-alt-shift-down");
    assert_eq!(view.read_with(cx, |view, _| view.caret().0), 2);
    press(cx, window, "ctrl-alt-shift-up");
    assert_eq!(view.read_with(cx, |view, _| view.caret().0), 0);

    // A selection over both changes goes back to the committed text in one step.
    press(cx, window, "shift-down");
    press(cx, window, "shift-down");
    press(cx, window, "shift-down");
    press(cx, window, "ctrl-alt-z");
    cx.run_until_parked();
    assert_eq!(buffer_of(cx, &view), "alpha\nbeta\n");
    assert_eq!(fs::read_to_string(&path).unwrap(), "alpha\nbeta\n");
}
