//! The editing side of the window without a window: the mouse in the text, the
//! multi-caret commands, the clipboard keys, the tabs and the project tree by pointer,
//! the context menus and a repository with a remote. Every scenario checks the buffer,
//! the disk or the repository.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use editor_view::EditorView;
use ui_kit::gpui;
use ui_kit::gpui::{
    ClipboardItem, MouseButton, MouseDownEvent, MouseUpEvent, ScrollDelta, ScrollEvent,
    TestAppContext, VisualTestContext, WindowHandle,
};
use ui_kit::{Modifiers, point, px};

use crate::project::ActiveProjectPanel;
use crate::window_tests::{
    active_path, bounds_of, buffer_of, click, draw, editor_of, line_height, open_path, press,
    sample_project, scratch, type_text, with_window,
};
use workspace::Workspace;

/// How long a test waits for an external process or a panel timer.
const LIMIT: Duration = Duration::from_secs(20);

/// Runs `check` until it holds, letting the real threads and the panel timers work.
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
        assert!(
            started.elapsed() < LIMIT,
            "the test waited for {what} in vain"
        );
        std::thread::sleep(Duration::from_millis(10));
        cx.executor().advance_clock(Duration::from_millis(50));
        cx.run_until_parked();
    }
}

fn open_editor(
    cx: &mut TestAppContext,
    name: &str,
    text: &str,
) -> (PathBuf, WindowHandle<Workspace>, gpui::Entity<EditorView>) {
    let root = sample_project(name);
    let path = root.join("src").join("main.txt");
    fs::write(&path, text).unwrap();
    let window = crate::window_tests::open_window(cx, root.clone());
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    (root, window, view)
}

/// The point of a text column and a buffer line, as the window tests find the text.
/// The advance of the test font is eight pixels, as in `window_tests_lang`.
fn text_point(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    line: usize,
    column: usize,
) -> gpui::Point<gpui::Pixels> {
    let height = line_height(cx);
    let bounds = bounds_of(cx, window, "editor-body-0");
    point(
        bounds.left() + px(67. + column as f32 * 8.),
        bounds.top() + px(height * line as f32 + height / 2.),
    )
}

fn click_at(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    position: gpui::Point<gpui::Pixels>,
    modifiers: Modifiers,
) {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_click(position, modifiers);
    cx.run_until_parked();
    draw(cx, window);
}

fn click_text(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    line: usize,
    column: usize,
    modifiers: Modifiers,
) {
    let position = text_point(cx, window, line, column);
    click_at(cx, window, position, modifiers);
}

fn click_text_count(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    line: usize,
    column: usize,
    count: usize,
    modifiers: Modifiers,
) {
    let position = text_point(cx, window, line, column);
    click_count(cx, window, position, count, modifiers);
}

fn drag_text(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    from: (usize, usize),
    to: (usize, usize),
    modifiers: Modifiers,
) {
    let from = text_point(cx, window, from.0, from.1);
    let to = text_point(cx, window, to.0, to.1);
    drag_to(cx, window, from, to, modifiers);
}

fn click_count(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    position: gpui::Point<gpui::Pixels>,
    count: usize,
    modifiers: Modifiers,
) {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    for clicks in 1..=count {
        visual.simulate_event(MouseDownEvent {
            button: MouseButton::Left,
            position,
            modifiers,
            click_count: clicks,
            first_mouse: false,
        });
        visual.simulate_event(MouseUpEvent {
            button: MouseButton::Left,
            position,
            modifiers,
            click_count: clicks,
        });
    }
    cx.run_until_parked();
    draw(cx, window);
}

fn drag_to(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    from: gpui::Point<gpui::Pixels>,
    to: gpui::Point<gpui::Pixels>,
    modifiers: Modifiers,
) {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_move(from, None, modifiers);
    visual.simulate_mouse_down(from, MouseButton::Left, modifiers);
    for step in 1..=8 {
        let fraction = step as f32 / 8.;
        visual.simulate_mouse_move(
            point(
                from.x + (to.x - from.x) * fraction,
                from.y + (to.y - from.y) * fraction,
            ),
            Some(MouseButton::Left),
            modifiers,
        );
    }
    visual.simulate_mouse_up(to, MouseButton::Left, modifiers);
    cx.run_until_parked();
    draw(cx, window);
}

fn scroll_at(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    position: gpui::Point<gpui::Pixels>,
    delta: ScrollDelta,
    modifiers: Modifiers,
) {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_event(ScrollEvent {
        position,
        delta,
        modifiers,
        ..Default::default()
    });
    cx.run_until_parked();
    draw(cx, window);
}

fn selected_text(cx: &TestAppContext, view: &gpui::Entity<EditorView>) -> String {
    let text = buffer_of(cx, view);
    let (range, _) = view
        .read_with(cx, |view, _| view.selection())
        .expect("the file is read");
    text[range].to_string()
}

fn clipboard(cx: &TestAppContext) -> Option<String> {
    cx.read(|cx| cx.read_from_clipboard().and_then(|item| item.text()))
}

#[ui_kit::gpui::test]
fn the_mouse_clicks_drags_selects_and_scrolls_the_text(cx: &mut TestAppContext) {
    let (_root, window, view) = open_editor(cx, "mouse", "alpha beta\ngamma delta\nthird line\n");

    click_text(cx, window, 0, 2, Modifiers::default());
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (0, 2));

    drag_text(cx, window, (0, 0), (1, 5), Modifiers::default());
    assert_eq!(selected_text(cx, &view), "alpha beta\ngamma");
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (1, 5));

    click_text_count(cx, window, 0, 7, 2, Modifiers::default());
    assert_eq!(
        selected_text(cx, &view),
        "beta",
        "a double click takes the word"
    );

    click_text_count(cx, window, 1, 3, 3, Modifiers::default());
    assert_eq!(
        selected_text(cx, &view),
        "gamma delta\n",
        "a triple click takes the line"
    );

    click_text(cx, window, 0, 1, Modifiers::default());
    let shift = Modifiers {
        shift: true,
        ..Modifiers::default()
    };
    click_text(cx, window, 0, 4, shift);
    assert_eq!(
        selected_text(cx, &view),
        "lph",
        "Shift+click extends the selection"
    );

    // A click on the line number is a gutter click: it puts a breakpoint on the line.
    crate::window_tests::click_gutter(cx, window, 0, 1);
    draw(cx, window);
    let debug = cx.update(|cx| cx.global::<crate::debug::ActiveDebug>().0.clone());
    assert_eq!(
        debug.read_with(cx, |panel, _| panel
            .breakpoint_lines(&_root.join("src").join("main.txt"))),
        vec![2],
        "the line number puts a breakpoint"
    );

    // A click in the fold column does nothing while the language has no folds.
    let bounds = bounds_of(cx, window, "editor-body-0");
    let height = line_height(cx);
    let fold = point(
        bounds.left() + px(50.),
        bounds.top() + px(height + height / 2.),
    );
    click_at(cx, window, fold, Modifiers::default());
    assert_eq!(
        debug.read_with(cx, |panel, _| panel
            .breakpoint_lines(&_root.join("src").join("main.txt"))),
        vec![2],
        "the fold column does not put a breakpoint"
    );

    // The wheel moves the text: a click at the top row lands on a later line.
    let body = bounds_of(cx, window, "editor-body-0");
    scroll_at(
        cx,
        window,
        body.center(),
        ScrollDelta::Lines(point(0., -1.)),
        Modifiers::default(),
    );
    click_at(
        cx,
        window,
        point(body.left() + px(80.), body.top() + px(height / 2.)),
        Modifiers::default(),
    );
    let line = view.read_with(cx, |view, _| view.caret().0);
    assert!(line >= 1, "the wheel scrolled down: the line is {line}");

    // Shift+wheel scrolls sideways: a click at the left edge lands in a later column.
    scroll_at(
        cx,
        window,
        body.center(),
        ScrollDelta::Lines(point(0., -2.)),
        Modifiers {
            shift: true,
            ..Modifiers::default()
        },
    );
    click_at(
        cx,
        window,
        point(body.left() + px(67.), body.top() + px(height / 2.)),
        Modifiers::default(),
    );
    let column = view.read_with(cx, |view, _| view.caret().1);
    assert!(
        column > 0,
        "Shift+wheel scrolled right: the column is {column}"
    );
}

#[ui_kit::gpui::test]
fn multi_carets_edit_every_place_and_one_undo_takes_them_back(cx: &mut TestAppContext) {
    let (_root, window, view) = open_editor(cx, "carets", "one two\nthree four\n");

    click_text(cx, window, 0, 0, Modifiers::default());
    let alt = Modifiers {
        alt: true,
        ..Modifiers::default()
    };
    click_text(cx, window, 1, 0, alt);
    type_text(cx, window, ">");
    assert_eq!(
        buffer_of(cx, &view),
        ">one two\n>three four\n",
        "the two carets type at once"
    );

    press(cx, window, "ctrl-z");
    assert_eq!(
        buffer_of(cx, &view),
        "one two\nthree four\n",
        "one undo takes the typing at both carets"
    );
    press(cx, window, "ctrl-shift-z");
    assert_eq!(buffer_of(cx, &view), ">one two\n>three four\n");
    press(cx, window, "backspace");
    assert_eq!(
        buffer_of(cx, &view),
        "one two\nthree four\n",
        "one backspace takes the character at both carets"
    );
    press(cx, window, "escape");
    assert_eq!(
        view.read_with(cx, |view, _| view.caret()),
        (1, 0),
        "Escape leaves the caret of the last selection"
    );

    // Alt+click also adds a caret; Alt+Shift+click adds one without a selection.
    click_text(cx, window, 1, 1, Modifiers::default());
    click_text(cx, window, 0, 1, alt);
    assert_eq!(
        view.read_with(cx, |view, _| view.selection().unwrap().0),
        1..1,
        "the second Alt+click adds a caret"
    );
    let alt_shift = Modifiers {
        alt: true,
        shift: true,
        ..Modifiers::default()
    };
    click_text(cx, window, 1, 3, alt_shift);
    let (range, caret) = view.read_with(cx, |view, _| {
        let (range, _) = view.selection().unwrap();
        (range, view.caret())
    });
    assert_eq!(range.start, range.end, "Alt+Shift+click adds a caret");
    assert_eq!(caret.0, 1, "the caret is on the clicked line");
    press(cx, window, "escape");

    // Alt+J takes the next occurrence, Shift+Alt+J gives the last one back.
    let (_root, window, view) = open_editor(cx, "occurrences", "alpha beta alpha beta\n");
    press(cx, window, "ctrl-home");
    press(cx, window, "alt-j alt-j");
    type_text(cx, window, "q");
    assert_eq!(
        buffer_of(cx, &view),
        "q beta q beta\n",
        "both occurrences are typed in"
    );
    press(cx, window, "ctrl-z");
    press(cx, window, "alt-j alt-j alt-shift-j");
    type_text(cx, window, "q");
    assert_eq!(
        buffer_of(cx, &view),
        "q beta alpha beta\n",
        "Shift+Alt+J takes the last occurrence away"
    );
    press(cx, window, "ctrl-z");

    // Ctrl+Alt+Shift+J takes every occurrence, Escape leaves one caret.
    press(cx, window, "ctrl-alt-shift-j");
    type_text(cx, window, "q");
    assert_eq!(buffer_of(cx, &view), "q beta q beta\n");
    press(cx, window, "ctrl-z");
    press(cx, window, "escape");
    type_text(cx, window, "Q");
    assert_eq!(
        buffer_of(cx, &view),
        "alphaQ beta alpha beta\n",
        "Escape leaves one caret for the next typing"
    );

    // Column mode: a drag takes the same columns on every line, as a block.
    let (_root, window, view) = open_editor(cx, "block", "abc\ndef\nghi\n");
    press(cx, window, "alt-shift-insert");
    drag_text(cx, window, (0, 1), (2, 3), Modifiers::default());
    type_text(cx, window, "x");
    assert_eq!(
        buffer_of(cx, &view),
        "ax\ndx\ngx\n",
        "typing replaces the block on every line"
    );

    // Alt+drag takes the same block without the column mode.
    let (_root, window, view) = open_editor(cx, "alt-block", "abc\ndef\n");
    let alt = Modifiers {
        alt: true,
        ..Modifiers::default()
    };
    drag_text(cx, window, (0, 1), (1, 3), alt);
    type_text(cx, window, "x");
    assert_eq!(
        buffer_of(cx, &view),
        "xax\ndx\n",
        "Alt+drag takes the block on both lines and keeps the caret it found"
    );
}

fn middle_click_at(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    position: gpui::Point<gpui::Pixels>,
) {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_down(position, MouseButton::Middle, Modifiers::default());
    visual.simulate_mouse_up(position, MouseButton::Middle, Modifiers::default());
    cx.run_until_parked();
    draw(cx, window);
}

fn right_click_at(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    position: gpui::Point<gpui::Pixels>,
) {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_down(position, MouseButton::Right, Modifiers::default());
    visual.simulate_mouse_up(position, MouseButton::Right, Modifiers::default());
    cx.run_until_parked();
    draw(cx, window);
}

fn right_click(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) {
    let position = bounds_of(cx, window, selector).center();
    right_click_at(cx, window, position);
}

fn middle_click(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) {
    let position = bounds_of(cx, window, selector).center();
    middle_click_at(cx, window, position);
}

fn show_project(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(workspace::PROJECT_TOOL, window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
}

#[ui_kit::gpui::test]
fn a_right_click_on_the_editor_opens_the_menu_and_escape_closes_it(cx: &mut TestAppContext) {
    let (_root, window, _view) = open_editor(cx, "editor-menu", "alpha\nbeta\n");
    let body = bounds_of(cx, window, "editor-body-0").center();
    right_click_at(cx, window, body);
    assert!(
        bounds_of(cx, window, "menu-0").size.height > px(0.),
        "the editor right click opens a menu"
    );
    press(cx, window, "escape");
    draw(cx, window);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    assert!(
        visual.debug_bounds("menu-0").is_none(),
        "Escape closes the menu"
    );
}

#[ui_kit::gpui::test]
fn tabs_move_by_drag_and_the_middle_button_closes_one(cx: &mut TestAppContext) {
    let (root, window) = crate::window_tests::open_sample_window(cx, "tab-mouse");
    let main = root.join("src").join("main.txt");
    let notes = root.join("src").join("notes.txt");
    let readme = root.join("README.txt");
    open_path(cx, window, &main);
    open_path(cx, window, &notes);
    open_path(cx, window, &readme);

    middle_click(cx, window, "tab-0-1");
    let tabs = window
        .update(cx, |workspace, _, _| workspace.open_tabs())
        .unwrap();
    assert_eq!(tabs, vec![vec![main.clone(), readme.clone()]]);

    // Drag the last tab onto the first one: it takes that place.
    let from = bounds_of(cx, window, "tab-0-1").center();
    let to = bounds_of(cx, window, "tab-0-0").center();
    drag_to(cx, window, from, to, Modifiers::default());
    let tabs = window
        .update(cx, |workspace, _, _| workspace.open_tabs())
        .unwrap();
    assert_eq!(tabs, vec![vec![readme.clone(), main.clone()]]);

    // A tab goes to the group on the right when it is dropped on its editor.
    window
        .update(cx, |workspace, window, cx| {
            workspace.split_right(window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
    let tabs = window
        .update(cx, |workspace, _, _| workspace.open_tabs())
        .unwrap();
    assert_eq!(tabs, vec![vec![main.clone()], vec![readme.clone()]]);

    let from = bounds_of(cx, window, "tab-0-0").center();
    let to = bounds_of(cx, window, "editor-body-1").center();
    drag_to(cx, window, from, to, Modifiers::default());
    let (tabs, group) = window
        .update(cx, |workspace, _, _| {
            (workspace.open_tabs(), workspace.active_group())
        })
        .unwrap();
    assert_eq!(
        tabs,
        vec![vec![readme, main]],
        "the tab of the left group moved to the right one"
    );
    assert_eq!(group, 0, "the group that is left keeps the focus");
}

#[ui_kit::gpui::test]
fn the_tab_menu_closes_tabs_and_copies_the_path(cx: &mut TestAppContext) {
    let (root, window) = crate::window_tests::open_sample_window(cx, "tab-menu");
    let main = root.join("src").join("main.txt");
    let notes = root.join("src").join("notes.txt");
    let readme = root.join("README.txt");
    open_path(cx, window, &main);
    open_path(cx, window, &notes);
    open_path(cx, window, &readme);

    right_click(cx, window, "tab-0-1");
    click(cx, window, "menu-11");
    assert_eq!(
        clipboard(cx).as_deref(),
        Some(notes.to_string_lossy().as_ref()),
        "Copy Path puts the path of the tab on the clipboard"
    );

    right_click(cx, window, "tab-0-1");
    click(cx, window, "menu-1");
    let tabs = window
        .update(cx, |workspace, _, _| workspace.open_tabs())
        .unwrap();
    assert_eq!(
        tabs,
        vec![vec![notes.clone()]],
        "Close Other Tabs keeps one"
    );

    open_path(cx, window, &main);
    open_path(cx, window, &readme);
    right_click(cx, window, "tab-0-0");
    click(cx, window, "menu-2");
    let tabs = window
        .update(cx, |worksace, _, _| worksace.open_tabs())
        .unwrap();
    assert!(
        tabs.iter().flatten().next().is_none(),
        "Close All Tabs closes every tab: {tabs:?}"
    );
}

#[ui_kit::gpui::test]
fn the_project_tree_menu_creates_renames_deletes_and_copies(cx: &mut TestAppContext) {
    // Delete must not put the scratch files into the Recycle Bin of the machine.
    crate::tree_ops::use_temporary_delete();
    let (root, window) = crate::window_tests::open_sample_window(cx, "tree-menu");
    let panel = cx.update(|cx| cx.global::<ActiveProjectPanel>().0.clone());
    let src = root.join("src");
    with_window(cx, window, |_, cx| {
        panel.update(cx, |panel, cx| panel.select_path(&src, cx))
    });
    show_project(cx, window);

    assert!(
        panel
            .read_with(cx, |panel, _| panel.tree().index_of(&src))
            .is_some(),
        "the src folder is in the tree"
    );
    right_click(cx, window, "tree-row-src");
    click(cx, window, "menu-0");
    draw(cx, window);
    type_text(cx, window, "menu-made.txt");
    press(cx, window, "enter");
    cx.run_until_parked();
    draw(cx, window);
    let made = src.join("menu-made.txt");
    assert_eq!(fs::read_to_string(&made).unwrap(), "");

    // Rename from the menu of the file row.
    with_window(cx, window, |_, cx| {
        panel.update(cx, |panel, cx| panel.select_path(&made, cx))
    });
    show_project(cx, window);
    assert!(
        panel
            .read_with(cx, |panel, _| panel.tree().index_of(&made))
            .is_some(),
        "the new file is in the tree"
    );
    right_click(cx, window, "tree-row-menu-made.txt");
    click(cx, window, "menu-4");
    draw(cx, window);
    press(
        cx,
        window,
        "backspace backspace backspace backspace backspace backspace backspace backspace backspace \
         backspace backspace backspace backspace",
    );
    type_text(cx, window, "menu-renamed.txt");
    press(cx, window, "enter");
    cx.run_until_parked();
    draw(cx, window);
    let renamed = src.join("menu-renamed.txt");
    assert!(!made.exists() && renamed.is_file());
    assert_eq!(active_path(cx, window), Some(renamed.clone()));

    // Copy Path and Reveal in Explorer from the same menu.
    with_window(cx, window, |_, cx| {
        panel.update(cx, |panel, cx| panel.select_path(&renamed, cx))
    });
    show_project(cx, window);
    right_click(cx, window, "tree-row-menu-renamed.txt");
    click(cx, window, "menu-9");
    assert_eq!(
        clipboard(cx).map(PathBuf::from),
        Some(renamed.clone()),
        "Copy Path puts the selected file on the clipboard"
    );
    right_click(cx, window, "tree-row-menu-renamed.txt");
    // Reveal in Explorer goes through the platform of the toolkit; the test platform
    // records the call and starts nothing.
    ui_kit::clear_revealed_paths();
    click(cx, window, "menu-10");
    draw(cx, window);
    assert_eq!(ui_kit::revealed_paths(), vec![renamed.clone()]);

    // Delete asks once and then takes the file away without the Recycle Bin.
    right_click(cx, window, "tree-row-menu-renamed.txt");
    click(cx, window, "menu-5");
    draw(cx, window);
    press(cx, window, "enter");
    cx.run_until_parked();
    draw(cx, window);
    assert!(!renamed.exists(), "the file left the scratch folder");
}

/// A repository with one commit and an empty bare repository as its `origin`. The user
/// is set in the repository, so the global git configuration is neither read nor written.
fn repository_with_origin(name: &str) -> (PathBuf, PathBuf, vcs::GitRunner) {
    let git = vcs::GitRunner::from_env().expect("git is on the PATH");
    let root = scratch(name);
    let bare = scratch(&format!("{name}-origin"));
    git.run(&bare, ["init", "--bare"]).unwrap();
    git.run(&root, ["init"]).unwrap();
    git.run(&root, ["config", "user.email", "window-test@example.com"])
        .unwrap();
    git.run(&root, ["config", "user.name", "Window Test"])
        .unwrap();
    fs::write(root.join("main.txt"), "alpha\nbeta\n").unwrap();
    git.run(&root, ["add", "main.txt"]).unwrap();
    git.run(&root, ["commit", "-m", "base"]).unwrap();
    git.run(
        &root,
        [
            "remote",
            "add",
            "origin",
            bare.to_str().expect("a UTF-8 scratch path"),
        ],
    )
    .unwrap();
    git.run(&root, ["push", "-u", "origin", "HEAD"]).unwrap();
    (root, bare, git)
}

fn read_text(path: &Path) -> String {
    fs::read_to_string(path).unwrap().replace("\r\n", "\n")
}

fn bare_subject(git: &vcs::GitRunner, bare: &Path) -> String {
    let output = git.run(bare, ["log", "-1", "--format=%s"]).unwrap();
    String::from_utf8_lossy(&output).trim().to_string()
}

fn commit_in(git: &vcs::GitRunner, root: &Path, text: &str, subject: &str) {
    fs::write(root.join("main.txt"), text).unwrap();
    git.run(root, ["add", "main.txt"]).unwrap();
    git.run(root, ["commit", "-m", subject]).unwrap();
}

fn git_state(cx: &TestAppContext) -> gpui::Entity<crate::git::GitState> {
    cx.read(|cx| cx.global::<crate::git::ActiveGit>().0.clone())
}

#[ui_kit::gpui::test]
fn a_commit_from_the_window_and_ctrl_shift_k_push_it_to_the_origin(cx: &mut TestAppContext) {
    let (root, bare, git) = repository_with_origin("git-push");
    let window = crate::window_tests::open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for(cx, "the repository of the window", |cx| {
        state.read_with(cx, |state, _| state.repo().is_some())
    });

    let path = root.join("main.txt");
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    view.update(cx, |view, cx| {
        view.replace_text(0..5, "ALPHA", cx);
        assert!(view.save_blocking(cx).is_none());
    });
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(workspace::COMMIT_TOOL, window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
    wait_for(cx, "the changed file in the commit window", |cx| {
        state.read_with(cx, |state, _| !state.status().files.is_empty())
    });

    type_text(cx, window, "from the window");
    press(cx, window, "ctrl-enter");
    wait_for(cx, "the commit", |cx| {
        state.read_with(cx, |state, _| {
            state.busy().is_none() && state.status().files.is_empty()
        })
    });
    assert_eq!(bare_subject(&git, &bare), "base");

    press(cx, window, "ctrl-shift-k");
    wait_for(cx, "the push to the origin", |cx| {
        let _ = cx;
        bare_subject(&git, &bare) == "from the window"
    });
    assert_eq!(
        fs::read_to_string(root.join("main.txt"))
            .unwrap()
            .lines()
            .next(),
        Some("ALPHA"),
        "the origin has the text of the window"
    );
}

#[ui_kit::gpui::test]
fn ctrl_t_brings_a_remote_commit_and_asks_for_a_diverged_one(cx: &mut TestAppContext) {
    let (root, bare, git) = repository_with_origin("git-update");
    let window = crate::window_tests::open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for(cx, "the repository of the window", |cx| {
        state.read_with(cx, |state, _| state.repo().is_some())
    });

    // Another clone writes and pushes: Ctrl+T fetches it and moves forward.
    let other = scratch("git-update-other");
    git.run(
        &scratch("git-clone-cwd"),
        ["clone", bare.to_str().unwrap(), other.to_str().unwrap()],
    )
    .unwrap();
    git.run(&other, ["config", "user.email", "other@example.com"])
        .unwrap();
    git.run(&other, ["config", "user.name", "Other"]).unwrap();
    commit_in(&git, &other, "remote\ntext\n", "remote change");
    git.run(&other, ["push"]).unwrap();

    press(cx, window, "ctrl-t");
    wait_for(cx, "the fetched text", |cx| {
        let _ = cx;
        read_text(&root.join("main.txt")) == "remote\ntext\n"
    });

    // A local commit and a newer remote commit: the update asks Merge or Rebase, the
    // merge conflicts on the same file and Abort puts the local branch back.
    commit_in(&git, &root, "local\n", "local change");
    commit_in(&git, &other, "other\n", "other change");
    git.run(&other, ["push"]).unwrap();

    press(cx, window, "ctrl-t");
    wait_for(cx, "the update choice", |cx| {
        let mut visual = VisualTestContext::from_window(window.into(), cx);
        visual.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
        visual.debug_bounds("menu-0").is_some()
    });
    click(cx, window, "menu-0");
    wait_for(cx, "the merge conflict", |cx| {
        state.read_with(cx, |state, _| state.operation() == vcs::Operation::Merge)
    });
    click(cx, window, "conflict-abort");
    wait_for(cx, "the aborted merge", |cx| {
        state.read_with(cx, |state, _| state.operation() == vcs::Operation::None)
    });
    let log = state
        .read_with(cx, |state, _| state.repo())
        .and_then(|repo| repo.log(1, 0).ok())
        .unwrap_or_default();
    assert_eq!(log[0].subject, "local change", "no merge commit was made");
    assert_eq!(
        read_text(&root.join("main.txt")),
        "local\n",
        "the local text stays as it was"
    );
}

#[ui_kit::gpui::test]
fn a_double_click_in_the_commit_window_shows_the_diff_of_the_file(cx: &mut TestAppContext) {
    let root = crate::window_tests::sample_project("git-diff");
    let git = vcs::GitRunner::from_env().expect("git is on the PATH");
    git.run(&root, ["init"]).unwrap();
    git.run(&root, ["config", "user.email", "window-test@example.com"])
        .unwrap();
    git.run(&root, ["config", "user.name", "Window Test"])
        .unwrap();
    git.run(&root, ["add", "."]).unwrap();
    git.run(&root, ["commit", "-m", "base"]).unwrap();

    let window = crate::window_tests::open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for(cx, "the repository of the window", |cx| {
        state.read_with(cx, |state, _| state.repo().is_some())
    });
    let path = root.join("src").join("main.txt");
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    view.update(cx, |view, cx| {
        view.replace_text(0..5, "ALPHA", cx);
        assert!(view.save_blocking(cx).is_none());
    });
    state.update(cx, |state, cx| state.refresh(cx));
    wait_for(cx, "the change in the commit window", |cx| {
        state.read_with(cx, |state, _| !state.status().files.is_empty())
    });
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(workspace::COMMIT_TOOL, window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);

    let row = bounds_of(cx, window, "commit-row-0").center();
    click_count(cx, window, row, 2, Modifiers::default());
    wait_for(cx, "the diff of the file", |cx| {
        let mut visual = VisualTestContext::from_window(window.into(), cx);
        visual.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
        visual.debug_bounds("diff-revert-0").is_some()
    });
    click(cx, window, "diff-revert-0");
    cx.run_until_parked();
    assert_eq!(fs::read_to_string(&path).unwrap(), "alpha\nbeta\n");
    assert_eq!(buffer_of(cx, &view), "alpha\nbeta\n");
}

#[ui_kit::gpui::test]
fn the_tree_menu_opens_the_local_history_of_a_file(cx: &mut TestAppContext) {
    let (root, window) = crate::window_tests::open_sample_window(cx, "tree-history");
    let path = root.join("README.txt");
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    view.update(cx, |view, cx| {
        view.replace_text(0..0, "saved ", cx);
        assert!(view.save_blocking(cx).is_none());
    });
    cx.run_until_parked();
    let panel = cx.update(|cx| cx.global::<ActiveProjectPanel>().0.clone());
    with_window(cx, window, |_, cx| {
        panel.update(cx, |panel, cx| panel.select_path(&path, cx))
    });
    show_project(cx, window);

    right_click(cx, window, "tree-row-README.txt");
    click(cx, window, "menu-7");
    cx.run_until_parked();
    draw(cx, window);
    let report = cx.update(|cx| crate::history_view::report(cx));
    assert!(
        report
            .iter()
            .any(|line| line.starts_with("history-version")),
        "Local History lists the saved version: {report:?}"
    );
}

#[ui_kit::gpui::test]
fn the_clipboard_keys_copy_cut_paste_and_move_lines(cx: &mut TestAppContext) {
    let (_root, window, view) = open_editor(cx, "clipboard", "alpha\nbeta\ngamma\n");
    press(cx, window, "ctrl-home");

    // Ctrl+C without a selection takes the whole line.
    press(cx, window, "ctrl-c");
    assert_eq!(clipboard(cx).as_deref(), Some("alpha\n"));
    assert_eq!(buffer_of(cx, &view), "alpha\nbeta\ngamma\n");

    // Ctrl+X takes that line out and puts it on the clipboard.
    press(cx, window, "ctrl-x");
    assert_eq!(buffer_of(cx, &view), "beta\ngamma\n");
    assert_eq!(clipboard(cx).as_deref(), Some("alpha\n"));
    press(cx, window, "ctrl-v");
    assert_eq!(buffer_of(cx, &view), "alpha\nbeta\ngamma\n");

    // Ctrl+D duplicates the line, Ctrl+Y deletes it.
    press(cx, window, "ctrl-home");
    press(cx, window, "ctrl-d");
    assert_eq!(buffer_of(cx, &view), "alpha\nalpha\nbeta\ngamma\n");
    press(cx, window, "ctrl-y");
    assert_eq!(buffer_of(cx, &view), "alpha\nbeta\ngamma\n");

    // Ctrl+Shift+V offers what was copied before; the second entry is placed.
    press(cx, window, "ctrl-home");
    press(cx, window, "ctrl-c");
    press(cx, window, "down ctrl-c");
    cx.update(|cx| cx.write_to_clipboard(ClipboardItem::new_string("clip".to_string())));
    press(cx, window, "escape");
    press(cx, window, "ctrl-shift-v");
    draw(cx, window);
    press(cx, window, "down enter");
    assert_eq!(
        buffer_of(cx, &view),
        "alpha\nbeta\nbeta\ngamma\n",
        "the older entry is pasted"
    );

    // Alt+Shift+Down and Up move the line; Ctrl+W and Ctrl+Shift+W grow and shrink.
    press(cx, window, "ctrl-home");
    press(cx, window, "alt-shift-down");
    assert_eq!(buffer_of(cx, &view), "beta\nalpha\nbeta\ngamma\n");
    press(cx, window, "alt-shift-up");
    assert_eq!(buffer_of(cx, &view), "alpha\nbeta\nbeta\ngamma\n");
    press(cx, window, "ctrl-z");
    press(cx, window, "ctrl-z");
    assert_eq!(buffer_of(cx, &view), "alpha\nbeta\nbeta\ngamma\n");

    press(cx, window, "ctrl-w");
    let grown = view.read_with(cx, |view, _| view.selected_chars());
    assert!(grown > 0, "Ctrl+W selects the word at the caret");
    press(cx, window, "ctrl-shift-w");
    let shrunk = view.read_with(cx, |view, _| view.selected_chars());
    assert!(shrunk < grown, "Ctrl+Shift+W takes the selection back");
}
