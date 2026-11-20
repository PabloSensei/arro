//! Ctrl+Enter in Find in Files moves the places of the search to the Find window as a tab
//! with the query, and its Rerun button repeats the search; in Replace the same key
//! replaces everything and no Find window opens.

use std::fs;
use std::path::PathBuf;

use keymap::workspace as keys;
use ui_kit::Modifiers;
use ui_kit::gpui;
use ui_kit::gpui::{
    MouseButton, MouseDownEvent, MouseUpEvent, TestAppContext, VisualTestContext, WindowHandle,
};
use workspace::{USAGES_TOOL, Workspace};

use crate::finder::ActiveFinder;
use crate::usages::ActiveUsages;
use crate::window_tests::{
    bounds_of, click, draw, editor_of, open_palette, open_window, press, scratch,
    wait_for_file_index, wait_for_settled_bounds, wait_for_signal,
};
use crate::window_tests_finder_scope::{finder, wait_for_finder};

/// Whether a Find in Files window is on screen.
fn finder_open(cx: &mut TestAppContext) -> bool {
    cx.read(|cx| {
        cx.try_global::<ActiveFinder>()
            .is_some_and(|global| global.0.is_some())
    })
}

/// A double click on the element with the selector: a press and a release, twice, the
/// second pair counting as a double click.
fn double_click(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) {
    let position = bounds_of(cx, window, selector).center();
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    for clicks in 1..=2 {
        visual.simulate_event(MouseDownEvent {
            button: MouseButton::Left,
            position,
            modifiers: Modifiers::default(),
            click_count: clicks,
            first_mouse: false,
        });
        visual.simulate_event(MouseUpEvent {
            button: MouseButton::Left,
            position,
            modifiers: Modifiers::default(),
            click_count: clicks,
        });
    }
    cx.run_until_parked();
    draw(cx, window);
}

/// The project with the files, the Find in Files window open on it, and a search for
/// `needle` that has answered.
fn search_for_needle(
    cx: &mut TestAppContext,
    name: &str,
    files: &[(&str, &str)],
) -> (WindowHandle<Workspace>, PathBuf) {
    let root = scratch(name);
    for (file, text) in files {
        fs::write(root.join(file), text).unwrap();
    }
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    draw(cx, window);
    wait_for_file_index(cx);
    let finder = wait_for_finder(cx, window);
    wait_for_signal(cx, "the file list of the dialog", |cx| {
        finder.read_with(cx, |finder, cx| {
            finder.files().read(cx).len() >= files.len()
        })
    });
    finder.update(cx, |finder, cx| finder.search("needle", cx));
    wait_for_signal(cx, "the answer of the search", |cx| {
        !finder.read_with(cx, |finder, _| finder.is_searching())
    });
    (window, root)
}

#[gpui::test]
fn ctrl_enter_moves_the_places_to_the_find_window_as_a_tree(cx: &mut TestAppContext) {
    let (window, root) = search_for_needle(
        cx,
        "find-window-tree",
        &[
            ("a.txt", "needle one\nnothing\n"),
            ("b.txt", "first\nneedle two\n"),
        ],
    );

    press(cx, window, "ctrl-enter");
    assert!(!finder_open(cx), "the Find in Files window closes");
    assert!(
        window
            .read_with(cx, |workspace, _| workspace.tool_open(USAGES_TOOL))
            .unwrap(),
        "the Find window opens"
    );
    draw(cx, window);
    wait_for_settled_bounds(cx, window, "item-row-1");

    let hub = cx.update(|cx| cx.global::<ActiveUsages>().0.clone());
    assert_eq!(
        hub.read_with(cx, |hub, _| hub.tab_names()),
        vec!["needle".to_owned()],
        "the query is the name of the tab"
    );
    assert_eq!(
        hub.read_with(cx, |hub, cx| hub.group_labels(cx)),
        vec!["a.txt  (1)".to_owned(), "b.txt  (1)".to_owned()],
        "each file is a group with its count of matching lines"
    );

    // A double click on the match of b.txt puts the caret on its line.
    double_click(cx, window, "item-row-1");
    let path = root.join("b.txt");
    wait_for_signal(cx, "the opened file", |cx| {
        window
            .read_with(cx, |workspace, _| workspace.view_of(&path).is_some())
            .unwrap()
    });
    let view = editor_of(cx, window, &path);
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (1, 0));
}

#[gpui::test]
fn ctrl_enter_in_replace_replaces_all_and_opens_no_find_window(cx: &mut TestAppContext) {
    let root = scratch("find-window-replace");
    fs::write(root.join("a.txt"), "needle one\n").unwrap();
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    draw(cx, window);
    wait_for_file_index(cx);
    open_palette(cx, window, keys::REPLACE_IN_FILES);
    let finder = finder(cx);
    wait_for_signal(cx, "the file list of the dialog", |cx| {
        finder.read_with(cx, |finder, cx| !finder.files().read(cx).is_empty())
    });
    finder.update(cx, |finder, cx| finder.search("needle", cx));
    wait_for_signal(cx, "the answer of the search", |cx| {
        !finder.read_with(cx, |finder, _| finder.is_searching())
    });

    press(cx, window, "ctrl-enter");
    let path = root.join("a.txt");
    wait_for_signal(cx, "the replacement on disk", |_| {
        fs::read_to_string(&path).is_ok_and(|text| !text.contains("needle"))
    });
    assert!(
        !window
            .read_with(cx, |workspace, _| workspace.tool_open(USAGES_TOOL))
            .unwrap(),
        "Replace All opens no Find window"
    );
}

#[gpui::test]
fn the_rerun_button_of_the_find_window_repeats_the_search(cx: &mut TestAppContext) {
    let (window, root) = search_for_needle(cx, "find-window-rerun", &[("a.txt", "needle one\n")]);

    press(cx, window, "ctrl-enter");
    draw(cx, window);
    wait_for_settled_bounds(cx, window, "item-button-find-rerun");

    // The file gains a match on disk; the rerun reads the file again.
    fs::write(root.join("a.txt"), "needle one\nneedle again\n").unwrap();
    click(cx, window, "item-button-find-rerun");
    let hub = cx.update(|cx| cx.global::<ActiveUsages>().0.clone());
    wait_for_signal(cx, "the places of the repeated search", |cx| {
        hub.read_with(cx, |hub, cx| hub.panel().read(cx).items().len() == 2)
    });
    let labels: Vec<String> = hub.read_with(cx, |hub, cx| {
        hub.panel()
            .read(cx)
            .items()
            .iter()
            .map(|item| item.label.clone())
            .collect()
    });
    assert_eq!(
        labels,
        vec!["needle one".to_owned(), "needle again".to_owned()]
    );
}
