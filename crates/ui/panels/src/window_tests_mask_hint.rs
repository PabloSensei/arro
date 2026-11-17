//! The mask hint of Find in Files follows the list of files of the project: the list
//! may arrive after the dialog is open. Every test waits for the list or for the hint
//! itself, never for a clock.

use std::fs;
use std::path::{Path, PathBuf};

use ui_kit::gpui;
use ui_kit::gpui::{Entity, TestAppContext, WindowHandle};
use workspace::Workspace;

use crate::finder::{ActiveFinder, Finder};
use crate::window_tests::{draw, open_window, press, scratch, type_text, wait_for_signal};

/// The language of the test knows the `*.txt` files and offers a mask of its own.
fn language_of_txt(cx: &mut TestAppContext) {
    cx.update(|cx| {
        cx.set_global(editor_view::Languages {
            resolve: |path| {
                path.extension()
                    .is_some_and(|extension| extension == "txt")
                    .then(|| {
                        let mut config = dockerfile_syntax::config();
                        config.file_mask = "*.kt !*Test.kt";
                        config
                    })
            },
            templates: |_| None,
        });
    });
}

fn finder(cx: &TestAppContext) -> Entity<Finder> {
    cx.read(|cx| {
        cx.global::<ActiveFinder>()
            .0
            .clone()
            .expect("the finder is open")
    })
}

fn hint(cx: &TestAppContext, finder: &Entity<Finder>) -> String {
    finder.read_with(cx, |finder, _| finder.mask_hint().to_owned())
}

/// A project of one file the language does not know: the hint starts on its extension.
fn start_project(name: &str) -> PathBuf {
    let root = scratch(name);
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("notes.md"), "one note\n").unwrap();
    root
}

/// Ctrl+Shift+F, then the finder that is on screen.
fn open_finder(cx: &mut TestAppContext, window: WindowHandle<Workspace>) -> Entity<Finder> {
    press(cx, window, "ctrl-shift-f");
    wait_for_signal(cx, "the finder", |cx| {
        cx.read(|cx| {
            cx.try_global::<ActiveFinder>()
                .is_some_and(|global| global.0.is_some())
        })
    });
    draw(cx, window);
    finder(cx)
}

/// The files the language of the project owns, written after the dialog is open.
fn add_language_files(root: &Path) {
    for name in ["Main.txt", "Owner.txt", "Third.txt"] {
        fs::write(root.join("src").join(name), "one\n").unwrap();
    }
}

#[ui_kit::gpui::test]
fn the_mask_hint_follows_the_file_list_that_arrives_after_the_dialog(cx: &mut TestAppContext) {
    let root = start_project("mask-hint-late");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    // The window sets the languages of the test up; the language of these files comes after.
    language_of_txt(cx);
    let finder = open_finder(cx, window);

    // The dialog is open while the project has only the file the language does not know.
    wait_for_signal(cx, "the first list of files", |cx| {
        finder.read_with(cx, |finder, cx| finder.files().read(cx).len() == 1)
    });
    wait_for_signal(cx, "the hint of the first list", |cx| {
        hint(cx, &finder) == "*.md"
    });

    // The files of the language reach the list after the dialog: the hint follows them.
    add_language_files(&root);
    wait_for_signal(cx, "the hint of the language of the project", |cx| {
        hint(cx, &finder) == "*.kt !*Test.kt"
    });
}

#[ui_kit::gpui::test]
fn a_mask_typed_by_the_user_survives_the_file_list(cx: &mut TestAppContext) {
    let root = start_project("mask-hint-typed");
    fs::write(root.join("src").join("Main.txt"), "one\n").unwrap();
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    language_of_txt(cx);
    let finder = open_finder(cx, window);
    wait_for_signal(cx, "the first list of files", |cx| {
        finder.read_with(cx, |finder, cx| finder.files().read(cx).len() == 2)
    });

    // Both files match the query, and the mask of the user keeps the search to one of them.
    type_text(cx, window, "one");
    wait_for_signal(cx, "the search before the mask", |cx| {
        finder.read_with(cx, |finder, _| finder.export_text().contains("Main.txt"))
    });
    press(cx, window, "ctrl-alt-f");
    draw(cx, window);
    assert!(
        finder.read_with(cx, |finder, _| finder.mask_open()),
        "Ctrl+Alt+F opens the mask"
    );
    type_text(cx, window, "*.md");
    wait_for_signal(cx, "the search on the typed mask", |cx| {
        finder.read_with(cx, |finder, _| {
            let text = finder.export_text();
            finder.mask_text() == "*.md" && text.contains("notes.md") && !text.contains("Main.txt")
        })
    });

    // The list of files of the language arrives, and the mask stays as typed.
    add_language_files(&root);
    wait_for_signal(cx, "the new list of files", |cx| {
        finder.read_with(cx, |finder, cx| finder.files().read(cx).len() >= 4)
    });
    assert_eq!(
        finder.read_with(cx, |finder, _| finder.mask_text().to_owned()),
        "*.md",
        "the mask of the user is not overwritten by the list"
    );
    assert!(
        !finder
            .read_with(cx, |finder, _| finder.export_text())
            .contains(".txt"),
        "the search still keeps to the mask of the user"
    );
}
