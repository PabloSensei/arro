//! The context menus of the editor and Problems, and the drag and drop of the project
//! tree. Every scenario checks the buffer, the clipboard, the disk or the tabs; no window
//! of the machine is opened and nothing goes to the Recycle Bin.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use editor_view::EditorView;
use lang_api::{
    ActionEntry, ActionKind, DiagnosticEntry, MoveDestination, Severity, SourceSpan, TextPoint,
    TextSpan,
};
use ui_kit::gpui;
use ui_kit::gpui::{MouseButton, TestAppContext, VisualTestContext, WindowHandle};
use ui_kit::{Modifiers, point, px};
use workspace::{PROBLEMS_TOOL, Workspace};

use crate::problems::ActiveProblems;
use crate::project::ActiveProjectPanel;
use crate::usages::ActiveUsages;
use crate::window_tests::{
    active_path, bounds_of, buffer_of, click, draw, editor_of, line_height, open_path, press,
    sample_project, type_text, with_window,
};
use crate::window_tests_lang::{
    FakeLanguage, open_window_with as open_language_window, project as language_project, span_in,
    uri_of,
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

fn right_click_text(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    line: usize,
    column: usize,
) {
    let position = text_point(cx, window, line, column);
    right_click_at(cx, window, position);
}

fn drag_to(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    from: gpui::Point<gpui::Pixels>,
    to: gpui::Point<gpui::Pixels>,
) {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_move(from, None, Modifiers::default());
    visual.simulate_mouse_down(from, MouseButton::Left, Modifiers::default());
    for step in 1..=8 {
        let fraction = step as f32 / 8.;
        visual.simulate_mouse_move(
            point(
                from.x + (to.x - from.x) * fraction,
                from.y + (to.y - from.y) * fraction,
            ),
            Some(MouseButton::Left),
            Modifiers::default(),
        );
    }
    visual.simulate_mouse_up(to, MouseButton::Left, Modifiers::default());
    cx.run_until_parked();
    draw(cx, window);
}

fn clipboard(cx: &TestAppContext) -> Option<String> {
    cx.read(|cx| cx.read_from_clipboard().and_then(|item| item.text()))
}

pub(crate) fn show_project(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(workspace::PROJECT_TOOL, window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
}

/// Opens a bottom tool window and lets its slide finish: the pointer can only reach a
/// row that is not clipped by a half-open dock.
fn show_problems(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(PROBLEMS_TOOL, window, cx)
        })
        .unwrap();
    std::thread::sleep(Duration::from_millis(300));
    cx.run_until_parked();
    draw(cx, window);
}

pub(crate) fn reveal(cx: &mut TestAppContext, window: WindowHandle<Workspace>, path: &Path) {
    let panel = cx.update(|cx| cx.global::<ActiveProjectPanel>().0.clone());
    with_window(cx, window, |_, cx| {
        panel.update(cx, |panel, cx| panel.select_path(path, cx))
    });
    wait_for(cx, "the tree row", |cx| {
        panel.read_with(cx, |panel, _| panel.tree().index_of(path).is_some())
    });
    draw(cx, window);
}

pub(crate) fn drag_row(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    from: &'static str,
    to: &'static str,
) {
    let from = bounds_of(cx, window, from).center();
    let to = bounds_of(cx, window, to).center();
    drag_to(cx, window, from, to);
}

#[ui_kit::gpui::test]
fn the_editor_menu_cuts_copies_pastes_and_puts_the_caret_at_the_click(cx: &mut TestAppContext) {
    let (_root, window, view) = open_editor(cx, "menu-edit", "alpha\nbeta\n");

    // Cut the line at the caret: the menu runs the same action as Ctrl+X.
    right_click_text(cx, window, 0, 0);
    click(cx, window, "menu-0");
    assert_eq!(buffer_of(cx, &view), "beta\n");
    assert_eq!(clipboard(cx).as_deref(), Some("alpha\n"));

    // Paste it back from the menu.
    right_click_text(cx, window, 0, 0);
    click(cx, window, "menu-2");
    assert_eq!(buffer_of(cx, &view), "alpha\nbeta\n");

    // A right click inside a selection keeps it; Copy takes the selection.
    press(cx, window, "ctrl-a");
    right_click_text(cx, window, 0, 1);
    click(cx, window, "menu-1");
    assert_eq!(clipboard(cx).as_deref(), Some("alpha\nbeta\n"));
    assert_eq!(buffer_of(cx, &view), "alpha\nbeta\n");

    // A right click outside a selection puts the caret there.
    press(cx, window, "ctrl-home");
    right_click_text(cx, window, 1, 2);
    press(cx, window, "escape");
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (1, 2));
    assert_eq!(
        view.read_with(cx, |view, _| view.selection().unwrap().0),
        8..8,
        "the right click left one caret"
    );
}

#[ui_kit::gpui::test]
fn the_editor_menu_finds_usages_and_renames_with_the_language(cx: &mut TestAppContext) {
    let root = language_project(
        "menu-lang",
        &[
            ("src/Open.txt", "alpha beta\n"),
            ("src/Other.txt", "alpha\n"),
        ],
    );
    let open = root.join("src").join("Open.txt");
    let other = root.join("src").join("Other.txt");
    let language = FakeLanguage::new();
    language.set_references(vec![
        SourceSpan {
            uri: uri_of(&open),
            span: span_in("alpha beta\n", 0, 5),
        },
        SourceSpan {
            uri: uri_of(&other),
            span: span_in("alpha\n", 0, 5),
        },
    ]);
    let window = open_language_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &open);
    let view = editor_of(cx, window, &open);

    right_click_text(cx, window, 0, 0);
    click(cx, window, "menu-10");
    wait_for(cx, "the usages of the symbol", |cx| {
        let usages = cx.update(|cx| cx.global::<ActiveUsages>().0.clone());
        usages.read_with(cx, |hub, cx| hub.panel().read(cx).items().len() == 2)
    });

    right_click_text(cx, window, 0, 0);
    click(cx, window, "menu-12");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "rename-popup").size.height > px(0.),
        "the menu opens the rename field"
    );
    press(
        cx,
        window,
        "backspace backspace backspace backspace backspace",
    );
    type_text(cx, window, "delta");
    press(cx, window, "enter");
    press(cx, window, "enter");
    draw(cx, window);
    assert_eq!(buffer_of(cx, &view), "delta beta\n");
}

#[ui_kit::gpui::test]
fn the_problems_menu_jumps_shows_fixes_copies_and_survives_an_empty_window(
    cx: &mut TestAppContext,
) {
    // An empty Problems window: a right click does nothing and does not panic.
    let (_root, empty) = crate::window_tests::open_sample_window(cx, "problems-empty");
    show_problems(cx, empty);
    let position = bounds_of(cx, empty, "items-problems").center();
    right_click_at(cx, empty, position);
    let mut visual = VisualTestContext::from_window(empty.into(), cx);
    assert!(
        visual.debug_bounds("menu-0").is_none(),
        "an empty window has no row menu"
    );

    let root = language_project("problems-menu", &[("src/Main.txt", "one\ntwo\nthree\n")]);
    let path = root.join("src").join("Main.txt");
    let language = FakeLanguage::new();
    language.set_diagnostics(vec![DiagnosticEntry {
        span: TextSpan {
            start: TextPoint {
                line: 1,
                character: 0,
            },
            end: TextPoint {
                line: 1,
                character: 3,
            },
        },
        severity: Severity::Error,
        message: "first error".into(),
        unnecessary: false,
    }]);
    language.set_actions(vec![ActionEntry {
        title: "Fix the problem".into(),
        kind: Some(ActionKind::QuickFix),
        code: Some("quickfix.fix".into()),
        preferred: true,
        // An action without edits does nothing, so Alt+Enter leaves it out.
        edits: vec![lang_api::SourceEdit {
            uri: uri_of(&path),
            edits: vec![lang_api::DocumentEdit {
                span: TextSpan {
                    start: TextPoint {
                        line: 1,
                        character: 0,
                    },
                    end: TextPoint {
                        line: 1,
                        character: 0,
                    },
                },
                text: "// fixed
"
                .into(),
            }],
        }],
        renames: Vec::new(),
        handle: None,
    }]);
    let window = open_language_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    let problems = cx.update(|cx| cx.global::<ActiveProblems>().0.clone());
    show_problems(cx, window);
    wait_for(cx, "the problem of the file", |cx| {
        problems.read_with(cx, |hub, cx| hub.panel().read(cx).items().len() == 1)
    });
    draw(cx, window);

    right_click(cx, window, "item-row-0");
    click(cx, window, "menu-2");
    assert_eq!(clipboard(cx).as_deref(), Some("first error"));

    right_click(cx, window, "item-row-0");
    click(cx, window, "menu-4");
    assert_eq!(
        clipboard(cx).map(PathBuf::from),
        Some(path.clone()),
        "Copy Path puts the file of the problem on the clipboard"
    );

    right_click(cx, window, "item-row-0");
    click(cx, window, "menu-0");
    draw(cx, window);
    assert_eq!(active_path(cx, window), Some(path.clone()));
    let view = editor_of(cx, window, &path);
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (1, 0));

    right_click(cx, window, "item-row-0");
    click(cx, window, "menu-1");
    wait_for(cx, "the quick fixes of the problem", |cx| {
        let mut visual = VisualTestContext::from_window(window.into(), cx);
        visual.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
        visual
            .debug_bounds("actions-popup")
            .is_some_and(|bounds| bounds.size.height > px(0.))
    });
}

fn right_click(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) {
    let position = bounds_of(cx, window, selector).center();
    right_click_at(cx, window, position);
}

#[ui_kit::gpui::test]
fn a_file_and_a_folder_move_by_drag_and_a_taken_name_is_refused(cx: &mut TestAppContext) {
    let root = sample_project("drag-tree");
    let src = root.join("src");
    let dest = src.join("dest");
    fs::create_dir_all(&dest).unwrap();
    fs::create_dir_all(src.join("move_me")).unwrap();
    fs::write(src.join("move_me").join("inner.txt"), "inner\n").unwrap();
    fs::create_dir_all(src.join("outer").join("inner")).unwrap();
    fs::write(src.join("collide.txt"), "source\n").unwrap();
    fs::write(dest.join("collide.txt"), "taken\n").unwrap();

    let window = crate::window_tests::open_window(cx, root.clone());
    cx.run_until_parked();
    draw(cx, window);
    let main = src.join("main.txt");
    open_path(cx, window, &main);
    show_project(cx, window);
    reveal(cx, window, &dest);
    reveal(cx, window, &src.join("move_me"));
    reveal(cx, window, &src.join("outer").join("inner"));

    // A file goes into another folder and its tab follows it.
    drag_row(cx, window, "tree-row-main.txt", "tree-row-dest");
    let moved = dest.join("main.txt");
    wait_for(cx, "the file on disk", |_| {
        moved.is_file() && !main.exists()
    });
    wait_for(cx, "the tab of the file", |cx| {
        active_path(cx, window) == Some(moved.clone())
    });
    assert_eq!(fs::read_to_string(&moved).unwrap(), "alpha\nbeta\n");

    // Dropping a file on its own folder does nothing.
    reveal(cx, window, &moved);
    drag_row(cx, window, "tree-row-main.txt", "tree-row-dest");
    assert!(moved.is_file());

    // A name that is taken is a message, not an overwrite.
    drag_row(cx, window, "tree-row-collide.txt", "tree-row-dest");
    assert_eq!(
        fs::read_to_string(dest.join("collide.txt")).unwrap(),
        "taken\n"
    );
    assert_eq!(
        fs::read_to_string(src.join("collide.txt")).unwrap(),
        "source\n"
    );
    let (_, text) = window
        .read_with(cx, |workspace, _| workspace.last_notification())
        .unwrap()
        .expect("the window says why the drop was refused");
    assert!(
        text.to_lowercase().contains("already exists") || text.contains("exists"),
        "{text}"
    );

    // A folder cannot go into its own folder.
    let outer = src.join("outer");
    let inner = outer.join("inner");
    drag_row(cx, window, "tree-row-outer", "tree-row-inner");
    assert!(outer.is_dir() && inner.is_dir());
    assert!(!inner.join("outer").exists());

    // A folder goes into another folder with everything in it.
    drag_row(cx, window, "tree-row-move_me", "tree-row-dest");
    let folder = dest.join("move_me");
    wait_for(cx, "the folder on disk", |_| {
        folder.join("inner.txt").is_file() && !src.join("move_me").exists()
    });
}

#[ui_kit::gpui::test]
fn dragging_a_class_moves_it_with_the_language(cx: &mut TestAppContext) {
    let root = language_project(
        "drag-move",
        &[("src/Main.txt", "package old;\nclass Main\n")],
    );
    let src = root.join("src");
    let newpkg = src.join("newpkg");
    fs::create_dir_all(&newpkg).unwrap();
    let from = src.join("Main.txt");
    let to = newpkg.join("Main.txt");
    let language = FakeLanguage::new();
    language.set_move(
        to.clone(),
        vec![MoveDestination {
            name: "newpkg".into(),
            detail: Some("project  /project/src/main/source/newpkg".into()),
            folder: Some(newpkg.clone()),
            handle: "pkg".into(),
        }],
    );
    let window = open_language_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    show_project(cx, window);
    reveal(cx, window, &from);
    reveal(cx, window, &newpkg);

    drag_row(cx, window, "tree-row-Main.txt", "tree-row-newpkg");
    wait_for(cx, "the class move", |_| to.is_file() && !from.exists());
    assert_eq!(
        fs::read_to_string(&to).unwrap(),
        "package newpkg;\nclass Main\n",
        "the language changed the package and moved the file"
    );
    wait_for(cx, "the tab of the moved class", |cx| {
        active_path(cx, window) == Some(to.clone())
    });
    let panel = cx.update(|cx| cx.global::<ActiveProjectPanel>().0.clone());
    wait_for(cx, "the row of the moved class", |cx| {
        panel.read_with(cx, |panel, _| panel.tree().index_of(&to).is_some())
    });
}

#[ui_kit::gpui::test]
fn a_class_dropped_on_a_folder_that_is_not_a_package_asks_first(cx: &mut TestAppContext) {
    let root = language_project(
        "drag-plain",
        &[("src/Main.txt", "package old;\nclass Main\n")],
    );
    let src = root.join("src");
    let docs = root.join("docs");
    fs::create_dir_all(&docs).unwrap();
    let from = src.join("Main.txt");
    let to = docs.join("Main.txt");
    let language = FakeLanguage::new();
    language.set_move(
        to.clone(),
        vec![MoveDestination {
            name: "old".into(),
            detail: Some("project  /project/src/main/source/old".into()),
            folder: Some(src.join("old")),
            handle: "pkg".into(),
        }],
    );
    let window = open_language_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    show_project(cx, window);
    reveal(cx, window, &from);
    reveal(cx, window, &docs);

    drag_row(cx, window, "tree-row-Main.txt", "tree-row-docs");
    wait_for(cx, "the question", |cx| {
        let mut visual = VisualTestContext::from_window(window.into(), cx);
        visual.debug_bounds("prompt").is_some()
    });
    assert!(
        from.is_file() && !to.exists(),
        "nothing moves before the answer"
    );

    press(cx, window, "enter");
    wait_for(cx, "the plain move", |_| to.is_file() && !from.exists());
    assert_eq!(
        fs::read_to_string(&to).unwrap(),
        "package old;\nclass Main\n",
        "the plain move keeps the class and its references as they are"
    );
}
