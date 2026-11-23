//! Go to Class and Go to File without a real window: a name that begins with the query
//! comes before the beginnings of words, a substring, and the scattered letters of longer
//! names. The sources of the project come before the tests, and the letters the query
//! matched are in the row data for the accent colour.
//!
//! cargo test -p panels window_tests_palette_ranking

use std::fs;
use std::path::Path;

use keymap::workspace as keys;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::Workspace;

use crate::palette::Palette;
use crate::window_tests::{
    draw, open_palette, press, scratch, type_text, wait_for_file_index, wait_for_project_index,
};
use crate::window_tests_ui::{open_test_window, wait_for};

/// Writes the files of a test project, each path relative to the root.
fn write_files(root: &Path, files: &[(&str, &str)]) {
    for (path, text) in files {
        let path = root.join(path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, text).unwrap();
    }
}

/// The popup of the palette that the command opened.
fn palette(cx: &TestAppContext, window: WindowHandle<Workspace>) -> gpui::Entity<Palette> {
    let view = window
        .read_with(cx, |workspace, _| workspace.overlay_view())
        .unwrap()
        .expect("a popup is open");
    view.downcast::<Palette>()
        .expect("the popup is the palette")
}

/// The rows of the popup on screen.
fn rows(cx: &TestAppContext, window: WindowHandle<Workspace>) -> Vec<(String, String)> {
    palette(cx, window).read_with(cx, |palette, _| palette.rows_text())
}

/// The names of the rows on screen.
fn titles(cx: &TestAppContext, window: WindowHandle<Workspace>) -> Vec<String> {
    rows(cx, window)
        .into_iter()
        .map(|(title, _)| title)
        .collect()
}

/// Runs the command and types the query, then waits until the rows of the index arrive.
fn search(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    command: &'static str,
    query: &str,
    expected: impl Fn(&[(String, String)]) -> bool,
) {
    open_palette(cx, window, command);
    type_text(cx, window, query);
    draw(cx, window);
    wait_for(cx, "the rows of the palette", |cx| {
        expected(&rows(cx, window))
    });
}

#[ui_kit::gpui::test]
fn a_name_that_begins_with_the_query_comes_before_the_tests(cx: &mut TestAppContext) {
    let root = scratch("palette-own");
    write_files(
        &root,
        &[
            ("src/main/Owner.txt", "class Owner\n"),
            ("src/main/OwnerController.txt", "class OwnerController\n"),
            ("src/main/OwnerRepository.txt", "class OwnerRepository\n"),
            ("src/test/OwnerTests.txt", "class OwnerTests\n"),
            (
                "src/test/OwnerControllerTests.txt",
                "class OwnerControllerTests\n",
            ),
        ],
    );
    let window = open_test_window(cx, root);
    cx.run_until_parked();
    draw(cx, window);
    wait_for_project_index(cx);

    search(cx, window, keys::GOTO_CLASS, "Own", |rows| rows.len() >= 5);

    assert_eq!(
        titles(cx, window),
        [
            "Owner",
            "OwnerController",
            "OwnerRepository",
            "OwnerTests",
            "OwnerControllerTests",
        ],
        "the sources come before the tests, the shorter names first"
    );
}

#[ui_kit::gpui::test]
fn files_hide_scattered_letters_while_a_substring_matches(cx: &mut TestAppContext) {
    let root = scratch("palette-app-files");
    write_files(
        &root,
        &[
            ("src/application.properties", "name=app\n"),
            ("src/Application.conf", "app=1\n"),
            ("src/wrapper-library.jar", ""),
            ("src/build-wrapper.properties", "name=app\n"),
            ("src/messages_pt.properties", "name=app\n"),
            ("src/createOrUpdatePetForm.html", "<form></form>\n"),
        ],
    );
    let window = open_test_window(cx, root);
    cx.run_until_parked();
    draw(cx, window);
    wait_for_file_index(cx);

    search(cx, window, keys::GOTO_FILE, "app", |rows| {
        rows.iter()
            .any(|(title, _)| title == "application.properties")
    });

    let titles = titles(cx, window);
    assert!(
        titles.iter().any(|title| title == "application.properties"),
        "the prefix match is shown: {titles:?}"
    );
    assert!(
        titles.iter().any(|title| title == "wrapper-library.jar"),
        "a substring match stays under the prefix matches: {titles:?}"
    );
    for scattered in ["messages_pt.properties", "createOrUpdatePetForm.html"] {
        assert!(
            !titles.iter().any(|title| title == scattered),
            "{scattered} matches only by its scattered letters: {titles:?}"
        );
    }
}

#[ui_kit::gpui::test]
fn a_query_that_matches_only_scattered_letters_still_shows_its_rows(cx: &mut TestAppContext) {
    let root = scratch("palette-scattered");
    write_files(
        &root,
        &[(
            "src/main/XylophoneZooQuartz.txt",
            "class XylophoneZooQuartz\n",
        )],
    );
    let window = open_test_window(cx, root);
    cx.run_until_parked();
    draw(cx, window);
    wait_for_project_index(cx);

    search(cx, window, keys::GOTO_CLASS, "xyzq", |rows| {
        rows.iter().any(|(title, _)| title == "XylophoneZooQuartz")
    });

    assert!(
        titles(cx, window)
            .iter()
            .any(|title| title == "XylophoneZooQuartz"),
        "a scattered match shows when no better kind exists"
    );
}

#[ui_kit::gpui::test]
fn the_matched_letters_of_a_name_are_in_the_row(cx: &mut TestAppContext) {
    let root = scratch("palette-marks");
    write_files(&root, &[("src/main/Owner.txt", "class Owner\n")]);
    let window = open_test_window(cx, root);
    cx.run_until_parked();
    draw(cx, window);
    wait_for_project_index(cx);

    search(cx, window, keys::GOTO_CLASS, "Own", |rows| {
        rows.iter().any(|(title, _)| title == "Owner")
    });

    let matched: Vec<(String, Vec<usize>)> =
        palette(cx, window).read_with(cx, |palette, _| palette.rows_matched());
    let owner = matched
        .iter()
        .find(|(title, _)| title == "Owner")
        .expect("the Owner row");
    assert_eq!(owner.1, [0, 1, 2], "the first three letters are marked");
}

#[ui_kit::gpui::test]
fn a_class_in_the_file_of_its_name_shows_no_file_name(cx: &mut TestAppContext) {
    let root = scratch("palette-own-file");
    write_files(
        &root,
        &[
            ("src/main/Owner.txt", "class Owner\n"),
            ("src/main/Other.txt", "class Helper\n"),
        ],
    );
    let window = open_test_window(cx, root);
    cx.run_until_parked();
    draw(cx, window);
    wait_for_project_index(cx);

    search(cx, window, keys::GOTO_CLASS, "Owner", |rows| {
        rows.iter().any(|(title, _)| title == "Owner")
    });
    let detail = rows(cx, window)
        .into_iter()
        .find(|(title, _)| title == "Owner")
        .map(|(_, detail)| detail)
        .expect("the Owner row");
    assert!(
        !detail.contains("Owner.txt"),
        "the file of the class does not repeat its name: {detail:?}"
    );

    press(cx, window, "escape");
    search(cx, window, keys::GOTO_CLASS, "Helper", |rows| {
        rows.iter().any(|(title, _)| title == "Helper")
    });
    let detail = rows(cx, window)
        .into_iter()
        .find(|(title, _)| title == "Helper")
        .map(|(_, detail)| detail)
        .expect("the Helper row");
    assert_eq!(
        detail, "Other.txt",
        "a class in a file of another name shows that file"
    );
}

/// Open tabs do not lift a test or a longer name above `Owner`: the open order only breaks
/// a tie of names of one length.
#[ui_kit::gpui::test]
fn open_tabs_keep_the_shorter_source_name_first(cx: &mut TestAppContext) {
    let root = scratch("palette-own-open");
    write_files(
        &root,
        &[
            ("src/main/Owner.txt", "class Owner\n"),
            ("src/main/OwnerController.txt", "class OwnerController\n"),
            ("src/test/OwnerTests.txt", "class OwnerTests\n"),
        ],
    );
    let window = open_test_window(cx, root.clone());
    cx.run_until_parked();
    draw(cx, window);
    crate::window_tests::open_path(cx, window, &root.join("src/test/OwnerTests.txt"));
    crate::window_tests::open_path(cx, window, &root.join("src/main/OwnerController.txt"));
    wait_for_project_index(cx);

    search(cx, window, keys::GOTO_CLASS, "Own", |rows| rows.len() >= 3);

    assert_eq!(
        titles(cx, window),
        ["Owner", "OwnerController", "OwnerTests"],
        "the shorter source name comes first, the test last, open or not"
    );
}
