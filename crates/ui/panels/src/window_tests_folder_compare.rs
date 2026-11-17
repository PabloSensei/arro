//! The folder compare of the window without a window: the four kinds of rows, the
//! filter of the identical files, the diff of one pair, synchronization in both
//! directions with a confirmation before a delete, and a large folder that stays off the
//! window thread.

use std::fs;
use std::path::{Path, PathBuf};

use ui_kit::Modifiers;
use ui_kit::gpui;
use ui_kit::gpui::{
    Entity, MouseButton, MouseDownEvent, MouseUpEvent, TestAppContext, VisualTestContext,
    WindowHandle,
};
use workspace::Workspace;

use crate::diff_view::DiffView;
use crate::folder_compare::{Direction, FolderCompareView, RowKind, hold_scan};
use crate::project::ActiveProjectPanel;
use crate::window_tests::{bounds_of, click, draw, open_window, press, scratch, wait_for_signal};

fn write_file(root: &Path, side: &str, relative: &str, text: &str) {
    let path = root.join(side).join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, text).unwrap();
}

fn folders(name: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = scratch(name);
    let left = root.join("left");
    let right = root.join("right");
    fs::create_dir_all(&left).unwrap();
    fs::create_dir_all(&right).unwrap();
    (root, left, right)
}

fn focus<T: 'static>(cx: &mut TestAppContext, window: WindowHandle<T>, handle: gpui::FocusHandle) {
    window
        .update(cx, |_, window, cx| window.focus(&handle, cx))
        .unwrap();
}

fn compare_view(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
) -> Entity<FolderCompareView> {
    let view = window
        .read_with(cx, |workspace, _| workspace.overlay_view())
        .expect("the window is open")
        .expect("the folder compare is open");
    view.downcast::<FolderCompareView>()
        .map_err(|_| ())
        .expect("the open popup is the folder compare")
}

/// Opens the compare of two folders by Ctrl+D on their rows of the project tree.
fn open_from_tree(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    left: &Path,
    right: &Path,
) {
    let panel = cx.update(|cx| cx.global::<ActiveProjectPanel>().0.clone());
    wait_for_signal(cx, "the folder rows", |cx| {
        panel.read_with(cx, |panel, _| {
            panel.tree().index_of(left).is_some() && panel.tree().index_of(right).is_some()
        })
    });
    panel.update(cx, |panel, cx| panel.select_path(left, cx));
    let index = panel
        .read_with(cx, |panel, _| panel.tree().index_of(right))
        .expect("the right folder has a row");
    panel.update(cx, |panel, cx| panel.toggle_selection(index, cx));
    let handle = panel.read_with(cx, |panel, _| panel.focus());
    focus(cx, window, handle);
    press(cx, window, "ctrl-d");
    wait_for_signal(cx, "the folder compare", |cx| {
        window
            .read_with(cx, |workspace, _| workspace.overlay_view().is_some())
            .unwrap()
    });
    draw(cx, window);
}

fn show(cx: &mut TestAppContext, window: WindowHandle<Workspace>, left: PathBuf, right: PathBuf) {
    window
        .update(cx, |workspace, window, cx| {
            FolderCompareView::open(left, right, workspace, window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
}

fn wait_for_scan(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    view: &Entity<FolderCompareView>,
) {
    wait_for_signal(cx, "the folder scan", |cx| {
        !view.read_with(cx, |view, _| view.is_scanning())
    });
    draw(cx, window);
}

fn kinds(cx: &TestAppContext, view: &Entity<FolderCompareView>) -> Vec<(String, RowKind)> {
    view.read_with(cx, |view, _| {
        view.rows()
            .iter()
            .map(|row| (row.relative.clone(), row.kind))
            .collect()
    })
}

fn click_count(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    position: gpui::Point<gpui::Pixels>,
    count: usize,
) {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    for clicks in 1..=count {
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

#[ui_kit::gpui::test]
fn the_four_kinds_of_rows_and_the_filter_of_identical_files(cx: &mut TestAppContext) {
    let (root, left, right) = folders("folder-compare-kinds");
    write_file(&root, "left", "only-left.txt", "left\n");
    write_file(&root, "right", "only-right.txt", "right\n");
    write_file(&root, "left", "src/differ.txt", "one\n");
    write_file(&root, "right", "src/differ.txt", "two\n");
    write_file(&root, "left", "src/same.txt", "same\n");
    write_file(&root, "right", "src/same.txt", "same\n");
    let window = open_window(cx, root);
    open_from_tree(cx, window, &left, &right);
    let view = compare_view(cx, window);
    wait_for_scan(cx, window, &view);

    assert_eq!(
        kinds(cx, &view),
        [
            ("only-left.txt".to_owned(), RowKind::OnlyLeft),
            ("only-right.txt".to_owned(), RowKind::OnlyRight),
            ("src/differ.txt".to_owned(), RowKind::Differ),
            ("src/same.txt".to_owned(), RowKind::Same),
        ]
    );

    // The identical file is in the scan but not on the screen until the filter asks for it.
    let visible = view.read_with(cx, |view, _| view.visible_rows());
    assert!(!view.read_with(cx, |view, _| view.shows_same()));
    assert_eq!(visible.len(), 3);
    assert!(visible.iter().all(|row| row.kind != RowKind::Same));
    click(cx, window, "folder-compare-show-same");
    assert!(view.read_with(cx, |view, _| view.shows_same()));
    assert_eq!(view.read_with(cx, |view, _| view.visible_rows()).len(), 4);

    // F7 and Shift+F7 walk the differences only.
    assert_eq!(
        view.read_with(cx, |view, _| view.selected_relative().map(str::to_owned)),
        Some("only-left.txt".to_owned())
    );
    press(cx, window, "f7");
    assert_eq!(
        view.read_with(cx, |view, _| view.selected_relative().map(str::to_owned)),
        Some("only-right.txt".to_owned())
    );
    press(cx, window, "shift-f7");
    assert_eq!(
        view.read_with(cx, |view, _| view.selected_relative().map(str::to_owned)),
        Some("only-left.txt".to_owned())
    );
}

#[ui_kit::gpui::test]
fn a_double_click_opens_the_pair_side_by_side(cx: &mut TestAppContext) {
    let (root, left, right) = folders("folder-compare-diff");
    write_file(&root, "left", "main.txt", "alpha\nbeta\n");
    write_file(&root, "right", "main.txt", "ALPHA\nbeta\n");
    let window = open_window(cx, root);
    show(cx, window, left, right.clone());
    let view = compare_view(cx, window);
    wait_for_scan(cx, window, &view);

    let position = bounds_of(cx, window, "folder-compare-row-0").center();
    click_count(cx, window, position, 2);
    wait_for_signal(cx, "the diff of the pair", |cx| {
        window
            .read_with(cx, |workspace, _| {
                workspace
                    .overlay_view()
                    .and_then(|view| view.downcast::<DiffView>().ok())
                    .is_some()
            })
            .unwrap()
    });

    let diff = window
        .read_with(cx, |workspace, _| workspace.overlay_view())
        .unwrap()
        .expect("the diff is open")
        .downcast::<DiffView>()
        .map_err(|_| ())
        .expect("the open popup is the diff");
    assert_eq!(
        diff.read_with(cx, |diff, _| diff.path().to_path_buf()),
        right.join("main.txt")
    );
    assert_eq!(diff.read_with(cx, |diff, _| diff.hunk_count()), 1);
    assert!(
        diff.read_with(cx, |diff, _| diff.side_by_side()),
        "the pair opens side by side"
    );
}

#[ui_kit::gpui::test]
fn synchronization_copies_both_ways_and_deletes_after_a_confirmation(cx: &mut TestAppContext) {
    crate::tree_ops::use_temporary_delete();
    let (root, left, right) = folders("folder-compare-sync");
    write_file(&root, "left", "to-right.txt", "right\n");
    write_file(&root, "left", "shared.txt", "left\n");
    write_file(&root, "right", "shared.txt", "right\n");
    write_file(&root, "right", "to-left.txt", "left\n");
    write_file(&root, "left", "extra-left.txt", "extra\n");
    write_file(&root, "left", "extra-cancel.txt", "extra\n");
    let window = open_window(cx, root);
    open_from_tree(cx, window, &left, &right);
    let view = compare_view(cx, window);
    wait_for_scan(cx, window, &view);

    // Enter copies the selected file from the left to the right.
    view.update(cx, |view, _| assert!(view.select("to-right.txt")));
    press(cx, window, "enter");
    wait_for_signal(cx, "the copy on the right", |_| {
        right.join("to-right.txt").is_file()
    });
    assert_eq!(
        fs::read_to_string(right.join("to-right.txt")).unwrap(),
        "right\n"
    );

    // A changed file is overwritten by the side the direction points from.
    view.update(cx, |view, _| assert!(view.select("shared.txt")));
    press(cx, window, "enter");
    wait_for_signal(cx, "the changed file on the right", |_| {
        fs::read_to_string(right.join("shared.txt")).unwrap() == "left\n"
    });

    // The direction button turns the copy around.
    click(cx, window, "folder-compare-direction");
    assert_eq!(
        view.read_with(cx, |view, _| view.direction()),
        Direction::RightToLeft
    );
    view.update(cx, |view, _| assert!(view.select("to-left.txt")));
    press(cx, window, "enter");
    wait_for_signal(cx, "the copy on the left", |_| {
        left.join("to-left.txt").is_file()
    });
    assert_eq!(
        fs::read_to_string(left.join("to-left.txt")).unwrap(),
        "left\n"
    );

    // A file that is missing on the source side is the extra one: it goes after a
    // confirmation and only then.
    view.update(cx, |view, _| assert!(view.select("extra-left.txt")));
    press(cx, window, "enter");
    draw(cx, window);
    assert!(view.read_with(cx, |view, _| view.is_confirming()));
    assert!(bounds_of(cx, window, "folder-compare-confirm").size.height > ui_kit::px(0.));
    assert!(
        left.join("extra-left.txt").is_file(),
        "nothing goes before the answer"
    );
    press(cx, window, "enter");
    wait_for_signal(cx, "the extra file to go", |_| {
        !left.join("extra-left.txt").exists()
    });
    assert!(!view.read_with(cx, |view, _| view.is_confirming()));

    // Escape keeps the file where it is.
    view.update(cx, |view, _| assert!(view.select("extra-cancel.txt")));
    press(cx, window, "enter");
    assert!(view.read_with(cx, |view, _| view.is_confirming()));
    press(cx, window, "escape");
    assert!(!view.read_with(cx, |view, _| view.is_confirming()));
    assert!(left.join("extra-cancel.txt").is_file());
}

#[ui_kit::gpui::test]
fn a_large_folder_does_not_hold_the_window_thread(cx: &mut TestAppContext) {
    let (root, left, right) = folders("folder-compare-large");
    for index in 0..1000 {
        let name = format!("same-{index}.txt");
        write_file(
            &root, "left", &name, "same
",
        );
        write_file(
            &root, "right", &name, "same
",
        );
    }
    for index in 0..500 {
        write_file(
            &root,
            "left",
            &format!("left-{index}.txt"),
            "left
",
        );
        write_file(
            &root,
            "right",
            &format!("right-{index}.txt"),
            "right
",
        );
    }
    // The scan of this folder waits until the test lets it go, so the window can be
    // checked while the background thread is still reading the disk.
    let hold = hold_scan(&left);
    let window = open_window(cx, root);
    window
        .update(cx, |workspace, window, cx| {
            FolderCompareView::open(left.clone(), right.clone(), workspace, window, cx)
        })
        .unwrap();
    let view = compare_view(cx, window);
    assert!(
        view.read_with(cx, |view, _| view.is_scanning()),
        "the scan has not run yet"
    );
    assert!(view.read_with(cx, |view, _| view.visible_rows().is_empty()));
    draw(cx, window);
    assert!(
        view.read_with(cx, |view, _| view.is_scanning()),
        "the window drew while the scan was still in the gate"
    );
    drop(hold);

    wait_for_scan(cx, window, &view);
    let rows = kinds(cx, &view);
    assert_eq!(rows.len(), 2000);
    assert_eq!(
        rows.iter()
            .filter(|(_, kind)| *kind == RowKind::OnlyLeft)
            .count(),
        500
    );
    assert_eq!(
        rows.iter()
            .filter(|(_, kind)| *kind == RowKind::OnlyRight)
            .count(),
        500
    );
    assert_eq!(
        rows.iter()
            .filter(|(_, kind)| *kind == RowKind::Same)
            .count(),
        1000
    );
}
