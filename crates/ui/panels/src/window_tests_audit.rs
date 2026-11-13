//! The IDEA keymap of M11: every action of `idea-default.json` is either bound, a
//! deliberate deviation, or knowingly out of MVP-1, and the small actions of the last
//! batch work without a real window.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use editor_view::EditorView;
use keymap::{editor as editor_keys, workspace as keys};
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::{Workspace, window_step};

use crate::window_tests::{
    active_path, bounds_of, draw, editor_of, open_path, open_window, press, sample_project,
    type_text,
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

fn idea_keys_for(platform: keymap::Platform) -> HashMap<String, Vec<String>> {
    let file = if platform.uses_command() {
        keymap::IDEA_DEFAULT_MACOS
    } else {
        keymap::IDEA_DEFAULT
    };
    keymap::idea_shortcuts(file)
        .expect("the IDEA keymap parses")
        .into_iter()
        .map(|(id, shortcuts)| {
            (
                id,
                shortcuts
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>(),
            )
        })
        .collect()
}

/// Every table of keys the window and the panels bind, for the checks below.
fn tables() -> Vec<&'static [workspace::KeyRow]> {
    vec![
        workspace::WINDOW_KEYS,
        crate::project::PANEL_KEYS,
        crate::diff_view::KEYS,
        crate::commit::KEYS,
        crate::commit::SHELF_RENAME,
        crate::git_log::KEYS,
        crate::debug::KEYS,
        crate::palette::KEYS,
        crate::folder_compare::KEYS,
        crate::branches::KEYS,
        crate::items::KEYS,
        crate::plugins::KEYS,
    ]
}

/// The IDEA actions the keymaps of the window know, without the ones out of MVP-1.
fn implemented_ids() -> HashSet<&'static str> {
    let mut ids = HashSet::new();
    ids.extend(editor_keys::IDEA_IDS.iter().map(|(id, _)| *id));
    ids.extend(editor_keys::HANDLED_IDS.iter().map(|(id, _)| *id));
    ids.extend(keys::IDEA_IDS.iter().map(|(id, _)| *id));
    ids.extend(keys::EXTRA_IDEA_IDS.iter().map(|(id, _)| *id));
    ids.extend(
        keymap::DEVIATIONS
            .iter()
            .filter_map(|deviation| deviation.idea_action),
    );
    for table in tables() {
        ids.extend(table.iter().filter_map(|row| row.idea));
    }
    ids
}

fn platform_name(platform: keymap::Platform) -> &'static str {
    match platform {
        keymap::Platform::Windows => "Windows",
        keymap::Platform::Linux => "Linux",
        keymap::Platform::MacOs => "macOS",
    }
}

#[test]
fn every_idea_action_is_bound_a_deviation_or_out_of_mvp() {
    let idea = idea_keys_for(keymap::Platform::Windows);
    let implemented = implemented_ids();
    let out: HashSet<&str> = keymap::NOT_IN_MVP.iter().map(|(id, _)| *id).collect();
    let mut accounted: HashSet<&str> = implemented.clone();
    accounted.extend(out.iter().copied());

    for id in idea.keys() {
        assert!(
            accounted.contains(id.as_str()),
            "{id} is neither bound, a deviation, nor in NOT_IN_MVP"
        );
    }
    for id in &accounted {
        assert!(
            idea.contains_key(*id),
            "{id} is listed but not in the IDEA file"
        );
    }
    for id in &implemented {
        assert!(
            !out.contains(id),
            "{id} is implemented and also out of MVP-1"
        );
    }
    for (id, reason) in keymap::NOT_IN_MVP {
        assert!(!reason.is_empty(), "{id} has no reason");
    }
}

#[test]
fn every_key_of_a_table_belongs_to_its_idea_action_on_both_platforms() {
    for platform in [
        keymap::Platform::Windows,
        keymap::Platform::Linux,
        keymap::Platform::MacOs,
    ] {
        let idea = idea_keys_for(platform);
        for table in tables() {
            for row in table {
                let Some(id) = row.idea else { continue };
                let expected = idea
                    .get(id)
                    .unwrap_or_else(|| panic!("{id} of {} is in the IDEA file", row.action));
                for keys in row.keys_for(platform) {
                    assert!(
                        expected.iter().any(|expected| expected == keys),
                        "{}: {keys} is not a key of IDEA {id} on {}",
                        row.action,
                        platform_name(platform)
                    );
                }
            }
        }
    }
}

/// A row without an IDEA action spells its macOS key itself: the file cannot give it.
#[test]
fn a_row_without_an_idea_action_names_its_macos_key() {
    for table in tables() {
        for row in table {
            if row.idea.is_none() {
                assert!(
                    row.mac.is_some(),
                    "{} has no macOS key of its own",
                    row.action
                );
            }
        }
    }
}

/// One key of one context runs one action on each platform: a stray row of the macOS
/// table would otherwise take a key of the Windows one without a test noticing.
#[test]
fn one_key_in_a_context_runs_one_action_on_each_platform() {
    for platform in [
        keymap::Platform::Windows,
        keymap::Platform::Linux,
        keymap::Platform::MacOs,
    ] {
        let mut seen: HashMap<(Option<&'static str>, &'static str), &'static str> = HashMap::new();
        for table in tables() {
            for row in table {
                for keys in row.keys_for(platform) {
                    let key = (row.context, keys);
                    if let Some(previous) = seen.insert(key, row.action) {
                        assert_eq!(
                            previous,
                            row.action,
                            "{keys} in {context:?} runs {previous} and {action} on {name}",
                            context = row.context,
                            action = row.action,
                            name = platform_name(platform)
                        );
                    }
                }
            }
        }
    }
}

/// A window and an open text file of its own.
fn open_text(
    cx: &mut TestAppContext,
    name: &str,
    text: &str,
) -> (
    PathBuf,
    WindowHandle<Workspace>,
    ui_kit::gpui::Entity<EditorView>,
) {
    let root = sample_project(name);
    let path = root.join("src").join("main.txt");
    fs::write(&path, text).unwrap();
    let window = open_window(cx, root);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    (path, window, view)
}

fn selection(
    cx: &TestAppContext,
    view: &ui_kit::gpui::Entity<EditorView>,
) -> std::ops::Range<usize> {
    view.read_with(cx, |view, _| view.selection())
        .expect("the file is read")
        .0
}

#[ui_kit::gpui::test]
fn ctrl_f7_marks_the_usages_in_the_file_and_f3_walks_them(cx: &mut TestAppContext) {
    let (_path, window, view) = open_text(cx, "usages-in-file", "alpha beta alpha\n");
    view.update(cx, |view, cx| view.restore_caret(0, 0, cx));
    press(cx, window, "ctrl-f7");
    let usages = view
        .read_with(cx, |view, _| view.highlighted_usages().map(<[_]>::to_vec))
        .expect("the occurrences of the word are marked");
    assert_eq!(usages, [0..5, 11..16]);

    press(cx, window, "f3");
    assert_eq!(selection(cx, &view), 11..16);
    press(cx, window, "shift-f3");
    assert_eq!(selection(cx, &view), 0..5);
}

#[ui_kit::gpui::test]
fn ctrl_alt_down_and_up_walk_the_matches_of_the_find_bar(cx: &mut TestAppContext) {
    let (_path, window, view) = open_text(cx, "occurrence-step", "one two one two one\n");
    press(cx, window, "ctrl-f");
    type_text(cx, window, "one");
    press(cx, window, "escape");
    assert_eq!(selection(cx, &view), 0..3);

    press(cx, window, "ctrl-alt-down");
    assert_eq!(selection(cx, &view), 8..11);
    press(cx, window, "ctrl-alt-down");
    assert_eq!(selection(cx, &view), 16..19);
    press(cx, window, "ctrl-alt-up");
    assert_eq!(selection(cx, &view), 8..11);
}

#[ui_kit::gpui::test]
fn ctrl_alt_insert_creates_a_file_next_to_the_open_one(cx: &mut TestAppContext) {
    let (path, window, _view) = open_text(cx, "new-same-place", "alpha\n");
    press(cx, window, "ctrl-alt-insert");
    draw(cx, window);
    crate::window_tests::click(cx, window, "menu-0");
    draw(cx, window);
    type_text(cx, window, "made");
    press(cx, window, "enter");
    draw(cx, window);
    let made = path.parent().unwrap().join("made");
    assert!(
        made.is_file(),
        "the new file is in the folder of the open one"
    );
}

#[ui_kit::gpui::test]
fn f5_copies_the_current_file_and_opens_the_copy(cx: &mut TestAppContext) {
    let (path, window, _view) = open_text(cx, "copy-element", "alpha\nbeta\n");
    press(cx, window, "f5");
    draw(cx, window);
    press(cx, window, "ctrl-a");
    type_text(cx, window, "mainCopy");
    press(cx, window, "enter");
    wait_for(cx, "the copy", |_| {
        path.parent().unwrap().join("mainCopy.txt").is_file()
    });
    let copy = path.parent().unwrap().join("mainCopy.txt");
    assert_eq!(fs::read_to_string(&copy).unwrap(), "alpha\nbeta\n");
    assert_eq!(active_path(cx, window), Some(copy));

    // The same key acts on the row selected in the project tree.
    let notes = path.parent().unwrap().join("notes.txt");
    let panel = cx.update(|cx| cx.global::<crate::project::ActiveProjectPanel>().0.clone());
    panel.update(cx, |panel, cx| panel.select_path(&notes, cx));
    window
        .update(cx, |_, window, cx| {
            panel.update(cx, |panel, cx| window.focus(&panel.focus(), cx));
        })
        .unwrap();
    press(cx, window, "f5");
    draw(cx, window);
    press(cx, window, "ctrl-a");
    type_text(cx, window, "notesCopy");
    press(cx, window, "enter");
    wait_for(cx, "the copy of the tree row", |_| {
        path.parent().unwrap().join("notesCopy.txt").is_file()
    });
}

#[ui_kit::gpui::test]
fn ctrl_alt_home_lists_the_related_test(cx: &mut TestAppContext) {
    let root = sample_project("related");
    let class = root.join("src").join("Foo.txt");
    let test = root.join("src").join("FooTest.txt");
    fs::write(&class, "class Foo {}\n").unwrap();
    fs::write(&test, "class FooTest {}\n").unwrap();
    let window = open_window(cx, root);
    open_path(cx, window, &class);

    wait_for(cx, "the project index", |cx| {
        press(cx, window, "ctrl-alt-home");
        window
            .read_with(cx, |workspace, _| workspace.menu_entries().is_some())
            .unwrap()
    });
    let labels: Vec<String> = window
        .read_with(cx, |workspace, _| {
            workspace
                .menu_entries()
                .unwrap_or_default()
                .iter()
                .map(|entry| entry.label.to_string())
                .collect::<Vec<_>>()
        })
        .unwrap();
    assert!(
        labels.iter().any(|label| label.ends_with("FooTest.txt")),
        "{labels:?}"
    );
    crate::window_tests::click(cx, window, "menu-0");
    assert_eq!(active_path(cx, window), Some(test));
}

#[ui_kit::gpui::test]
fn ctrl_shift_f11_lists_the_bookmarks_with_their_mnemonics(cx: &mut TestAppContext) {
    let (_path, window, view) = open_text(cx, "type-bookmarks", "alpha\nbeta\ngamma\n");
    view.update(cx, |view, cx| view.restore_caret(1, 0, cx));
    press(cx, window, "ctrl-f11");
    press(cx, window, "m");
    press(cx, window, "ctrl-shift-f11");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "places-popup").size.height > ui_kit::px(0.),
        "Ctrl+Shift+F11 shows the bookmarks popup"
    );
    assert!(
        cx.update(|cx| editor_view::bookmarks(cx))
            .iter()
            .any(|entry| entry.mnemonic == Some('M')),
        "the popup has the mnemonic"
    );
    press(cx, window, "escape");
}

#[ui_kit::gpui::test]
fn project_window_keys_step_between_two_windows(cx: &mut TestAppContext) {
    assert_eq!(window_step(3, 0, 1), Some(1));
    assert_eq!(window_step(3, 2, 1), Some(0));
    assert_eq!(window_step(3, 0, -1), Some(2));
    assert_eq!(window_step(1, 0, 1), None);
    assert_eq!(window_step(2, 2, 1), None);

    let (_path, window, _view) = open_text(cx, "project-windows", "alpha\n");
    press(cx, window, "ctrl-alt-]");
    wait_for(cx, "the notice about one window", |cx| {
        window
            .read_with(cx, |workspace, _| {
                workspace
                    .last_notification()
                    .is_some_and(|(_, text)| text.contains("Only one project window"))
            })
            .unwrap()
    });
}
