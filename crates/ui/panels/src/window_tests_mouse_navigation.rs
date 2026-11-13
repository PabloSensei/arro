//! The side buttons of the mouse: X1 is Back and X2 is Forward in every part of the
//! window, and they walk the history of the window the way the keys do.

use ui_kit::Modifiers;
use ui_kit::gpui;
use ui_kit::gpui::{
    MouseButton, NavigationDirection, TestAppContext, VisualTestContext, WindowHandle,
};
use workspace::Workspace;

use crate::documentation::ActiveDocumentation;
use crate::window_tests::{
    active_path, bounds_of, draw, editor_of, open_path, press, wait_for_project_tree,
    wait_for_signal,
};
use crate::window_tests_doc_window::{
    DocsLanguage, click_editor, open_window_with as open_documentation_window,
};
use crate::window_tests_lang::{FakeLanguage, open_window_with, project};

/// Presses a side button of the mouse over `position`, picks it up again and lets the
/// window settle. A press is one step; the release does nothing.
fn side_button(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    position: gpui::Point<gpui::Pixels>,
    direction: NavigationDirection,
) {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_down(
        position,
        MouseButton::Navigate(direction),
        Modifiers::default(),
    );
    visual.simulate_mouse_up(
        position,
        MouseButton::Navigate(direction),
        Modifiers::default(),
    );
    cx.run_until_parked();
    draw(cx, window);
}

fn caret_at(
    cx: &TestAppContext,
    window: WindowHandle<Workspace>,
    path: &std::path::Path,
) -> (usize, usize) {
    editor_of(cx, window, path).read_with(cx, |view, _| view.caret())
}

#[ui_kit::gpui::test]
fn the_side_buttons_walk_the_history_the_way_the_keys_do(cx: &mut TestAppContext) {
    let root = project(
        "mouse-navigation",
        &[
            ("src/Main.txt", "greet\n"),
            ("src/Other.txt", "other()\ngreet\n"),
        ],
    );
    let main = root.join("src").join("Main.txt");
    let other = root.join("src").join("Other.txt");
    let language = FakeLanguage::new();
    language.set_definition(&other, 1, 0);
    let window = open_window_with(cx, root, language);
    cx.run_until_parked();
    draw(cx, window);

    open_path(cx, window, &main);
    press(cx, window, "right right");
    assert_eq!(caret_at(cx, window, &main), (0, 2));

    press(cx, window, "ctrl-b");
    draw(cx, window);
    assert_eq!(active_path(cx, window), Some(other.clone()));

    let position = bounds_of(cx, window, "editor-body-0").center();
    side_button(cx, window, position, NavigationDirection::Back);
    assert_eq!(
        active_path(cx, window),
        Some(main.clone()),
        "the side Back button returns to the file the jump started from"
    );
    assert_eq!(
        caret_at(cx, window, &main),
        (0, 2),
        "Back returns to the place of the caret the jump left"
    );

    side_button(cx, window, position, NavigationDirection::Forward);
    assert_eq!(
        active_path(cx, window),
        Some(other.clone()),
        "the side Forward button returns to the place Back left"
    );
    assert_eq!(
        caret_at(cx, window, &other),
        (1, 0),
        "Forward returns to the place of the caret Back left"
    );
}

#[ui_kit::gpui::test]
fn a_side_button_without_history_changes_nothing(cx: &mut TestAppContext) {
    let root = project("mouse-navigation-empty", &[("src/Main.txt", "greet\n")]);
    let main = root.join("src").join("Main.txt");
    let window = open_window_with(cx, root, FakeLanguage::new());
    cx.run_until_parked();
    draw(cx, window);

    open_path(cx, window, &main);
    press(cx, window, "right");

    let position = bounds_of(cx, window, "editor-body-0").center();
    side_button(cx, window, position, NavigationDirection::Back);
    side_button(cx, window, position, NavigationDirection::Forward);
    assert_eq!(
        active_path(cx, window),
        Some(main.clone()),
        "there is nowhere to go and the window stays on the file"
    );
    assert_eq!(
        caret_at(cx, window, &main),
        (0, 1),
        "the caret stays where it was"
    );
}

#[ui_kit::gpui::test]
fn the_side_buttons_work_over_a_tool_window(cx: &mut TestAppContext) {
    let root = project(
        "mouse-navigation-tools",
        &[
            ("README.txt", "read me\n"),
            ("src/Main.txt", "greet\n"),
            ("src/Other.txt", "other()\ngreet\n"),
        ],
    );
    let main = root.join("src").join("Main.txt");
    let other = root.join("src").join("Other.txt");
    let language = FakeLanguage::new();
    language.set_definition(&other, 1, 0);
    let window = open_window_with(cx, root, language);
    cx.run_until_parked();
    draw(cx, window);
    wait_for_project_tree(cx);
    draw(cx, window);

    open_path(cx, window, &main);
    press(cx, window, "right right");
    press(cx, window, "ctrl-b");
    draw(cx, window);
    assert_eq!(active_path(cx, window), Some(other.clone()));

    let position = bounds_of(cx, window, "tree-row-README.txt").center();
    side_button(cx, window, position, NavigationDirection::Back);
    assert_eq!(
        active_path(cx, window),
        Some(main.clone()),
        "a side button over the project tree navigates as the keys do"
    );
    assert_eq!(caret_at(cx, window, &main), (0, 2));

    side_button(cx, window, position, NavigationDirection::Forward);
    assert_eq!(active_path(cx, window), Some(other));
}

/// The answer on screen of the Documentation window.
fn documentation_text(cx: &TestAppContext) -> Option<String> {
    cx.read(|cx| {
        cx.try_global::<ActiveDocumentation>()
            .and_then(|panel| panel.0.read(cx).documentation_text())
    })
}

#[ui_kit::gpui::test]
fn the_side_button_pages_the_history_of_the_documentation_window(cx: &mut TestAppContext) {
    let root = project(
        "mouse-navigation-documentation",
        &[("src/Main.txt", "alpha beta\n")],
    );
    let main = root.join("src").join("Main.txt");
    let language = DocsLanguage::new();
    language.set_hover("Alpha documentation");
    let window = open_documentation_window(cx, root, language.clone());
    cx.run_until_parked();
    draw(cx, window);

    open_path(cx, window, &main);
    click_editor(cx, window, 0, 1);
    press(cx, window, "ctrl-q");
    draw(cx, window);
    // The second Ctrl+Q moves the answer to the Documentation window.
    press(cx, window, "ctrl-q");
    cx.run_until_parked();
    draw(cx, window);
    wait_for_signal(cx, "the first answer", |cx| {
        documentation_text(cx).is_some_and(|text| text.contains("Alpha documentation"))
    });

    language.set_hover("Beta documentation");
    click_editor(cx, window, 0, 7);
    wait_for_signal(cx, "the answer of the other symbol", |cx| {
        documentation_text(cx).is_some_and(|text| text.contains("Beta documentation"))
    });

    let position = bounds_of(cx, window, "documentation-window").center();
    side_button(cx, window, position, NavigationDirection::Back);
    assert!(
        documentation_text(cx).is_some_and(|text| text.contains("Alpha documentation")),
        "the side Back button pages the answers of the window: {:?}",
        documentation_text(cx)
    );

    side_button(cx, window, position, NavigationDirection::Forward);
    assert!(
        documentation_text(cx).is_some_and(|text| text.contains("Beta documentation")),
        "the side Forward button pages them the other way: {:?}",
        documentation_text(cx)
    );
}

/// The press changes the history and nothing else: the keyboard still goes to the editor,
/// not to the window that the side button passed over.
#[ui_kit::gpui::test]
fn a_side_button_leaves_the_focus_where_it_was(cx: &mut TestAppContext) {
    let root = project(
        "mouse-navigation-focus",
        &[
            ("src/Main.txt", "greet\n"),
            ("src/Other.txt", "other()\ngreet\n"),
        ],
    );
    let main = root.join("src").join("Main.txt");
    let other = root.join("src").join("Other.txt");
    let language = FakeLanguage::new();
    language.set_definition(&other, 1, 0);
    let window = open_window_with(cx, root, language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &main);
    press(cx, window, "right right");
    press(cx, window, "ctrl-b");
    draw(cx, window);
    let position = bounds_of(cx, window, "editor-body-0").center();
    side_button(cx, window, position, NavigationDirection::Back);
    assert_eq!(active_path(cx, window), Some(main.clone()));
    crate::window_tests::type_text(cx, window, "x");
    let view = editor_of(cx, window, &main);
    assert_eq!(
        view.read_with(cx, |view, _| view.text()).as_deref(),
        Some("grxeet\n"),
        "the caret of the editor still takes the typed text"
    );
}
