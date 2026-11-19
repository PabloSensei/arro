//! Refactor | Copy runs the copy of the editor: a class of a language goes through the
//! Copy Class window, a plain file through a name prompt, and either copy opens in the
//! editor. Refactor This lists the same row. The last test runs the row against the real
//! server on a copy of the corpus.

use std::fs;
use std::path::PathBuf;

use ui_kit::gpui;
use ui_kit::gpui::TestAppContext;
use workspace::Workspace;

use crate::window_tests::{active_path, draw, open_path, press, type_text, wait_for_signal};
use crate::window_tests_copy_class::{TestLanguage, open_language_window, project, shown};

const THING: &str = "package a;

class Thing {
    Thing() {}
    Thing next;
    static Thing of() { return new Thing(); }
}
";

/// The file a copy of `source` gets in `folder`.
fn copy_of(source: &str, folder: &std::path::Path) -> PathBuf {
    folder.join(
        PathBuf::from(source)
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .replacen("Thing", "Thing2", 1),
    )
}

fn leak(text: String) -> &'static str {
    Box::leak(text.into_boxed_str())
}

/// Clicks Refactor | Copy in the main menu. Returns the labels of the rows, for a test
/// that wants to name another one.
fn click_refactor_copy(cx: &mut TestAppContext, window: gpui::WindowHandle<Workspace>) {
    let entries = window
        .update(cx, |workspace, _, cx| workspace.main_menu(cx))
        .unwrap();
    let section = entries
        .iter()
        .position(|entry| entry.label.as_ref() == "Refactor")
        .expect("the main menu has Refactor");
    let row = entries[section]
        .submenu
        .iter()
        .position(|entry| entry.label.as_ref() == "Copy...")
        .expect("Refactor has Copy...");
    let bar = leak(format!("menu-bar-{section}"));
    // The rows of a submenu are named by their place in it, without the section.
    let item = leak(format!("menu-{row}"));
    crate::window_tests::click(cx, window, bar);
    draw(cx, window);
    crate::window_tests::click(cx, window, item);
    draw(cx, window);
}

#[ui_kit::gpui::test]
fn copy_in_the_refactor_menu_renames_the_class_and_its_package_and_opens_the_copy(
    cx: &mut TestAppContext,
) {
    let source = "src/main/source/a/Thing.cls";
    let root = project(
        "refactor-copy-class",
        &[(source, THING), ("src/main/source/b/.keep", "")],
    );
    let window = open_language_window(cx, root.clone(), TestLanguage::new());
    cx.run_until_parked();
    draw(cx, window);

    let from = root.join(source);
    open_path(cx, window, &from);
    click_refactor_copy(cx, window);
    assert!(
        shown(cx, window, "copy-class"),
        "Refactor | Copy opens the Copy Class window"
    );

    // The same folder already has the class, so the name offers the next number. The
    // destination field takes the other package.
    crate::window_tests::click(cx, window, "copy-class-destination");
    draw(cx, window);
    press(cx, window, "ctrl-a");
    type_text(cx, window, "src/main/source/b");
    press(cx, window, "enter");
    let copy = copy_of(source, &root.join("src/main/source/b"));
    wait_for_signal(cx, "the copy", |_| copy.is_file());
    let text = fs::read_to_string(&copy).unwrap();
    assert!(text.contains("class Thing2 {"), "{text}");
    assert!(text.contains("Thing2() {}"), "{text}");
    assert!(text.contains("package b;"), "{text}");
    assert_eq!(
        active_path(cx, window),
        Some(copy),
        "the copy opens in the editor"
    );
}

#[ui_kit::gpui::test]
fn refactor_this_lists_copy_and_runs_it(cx: &mut TestAppContext) {
    use crate::window_tests_refactor_this::{RefactorLanguage, open_window};

    let root = project("refactor-copy-own", &[("src/Own.txt", "alpha beta\n")]);
    let own = root.join("src").join("Own.txt");
    let window = open_window(cx, root.clone(), RefactorLanguage::new());
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &own);

    // Rename, Change Signature and Move come first, then Copy, which asks for the name
    // of the plain file.
    press(cx, window, "ctrl-alt-shift-t");
    draw(cx, window);
    assert!(
        shown(cx, window, "actions-popup"),
        "Refactor This lists the rows of the editor"
    );
    for _ in 0..3 {
        press(cx, window, "down");
    }
    press(cx, window, "enter");
    draw(cx, window);
    assert!(
        shown(cx, window, "prompt"),
        "Refactor This runs Copy from its row"
    );
    press(cx, window, "escape");
}

#[ui_kit::gpui::test]
fn copy_of_a_plain_file_writes_the_new_name_and_opens_it(cx: &mut TestAppContext) {
    let root = project("refactor-copy-text", &[("src/notes.txt", "notes\n")]);
    let window = open_language_window(cx, root.clone(), TestLanguage::new());
    cx.run_until_parked();
    draw(cx, window);

    let from = root.join("src").join("notes.txt");
    open_path(cx, window, &from);
    click_refactor_copy(cx, window);
    assert!(
        shown(cx, window, "prompt"),
        "a plain file asks for the name of its copy"
    );
    press(cx, window, "enter");
    let copy = root.join("src").join("notesCopy.txt");
    wait_for_signal(cx, "the copy", |_| copy.is_file());
    assert_eq!(fs::read_to_string(&copy).unwrap(), "notes\n");
    assert_eq!(
        active_path(cx, window),
        Some(copy),
        "the copy of a plain file opens in the editor"
    );
}

// The same row on the real server and a copy of PetClinic.
#[ui_kit::gpui::test]
#[ignore = "needs IDE_CORPUS and a filled package cache"]
fn owner_copied_into_another_package_by_the_refactor_menu_opens_in_the_editor(
    cx: &mut TestAppContext,
) {
    use std::time::Duration;

    use crate::window_tests_real::{
        build_window, close, errors, finish, project, wait_for, wait_ready,
    };

    const LIMIT: Duration = Duration::from_secs(120);
    let started = std::time::Instant::now();
    let (root, language) = project("real-refactor-copy");
    let package = "org.springframework.samples.petclinic";
    let from = window_language::main_class(&root, &format!("{package}.owner"), "Owner");
    let to = window_language::main_class(&root, &format!("{package}.vet"), "OwnerCopy");
    let vet = to
        .parent()
        .expect("the target package folder")
        .to_path_buf();
    assert!(from.is_file(), "the corpus has the class to copy");

    let window = build_window(
        cx,
        root.clone(),
        &language,
        crate::window_tests::project_setup(crate::window_tests::scratch("cache")),
    );
    cx.run_until_parked();
    draw(cx, window);
    wait_ready(cx, window);

    open_path(cx, window, &from);
    click_refactor_copy(cx, window);
    assert!(
        shown(cx, window, "copy-class"),
        "the refactor row opens the Copy Class window"
    );
    crate::window_tests::click(cx, window, "copy-class-destination");
    draw(cx, window);
    press(cx, window, "ctrl-a");
    type_text(cx, window, &vet.display().to_string());
    press(cx, window, "enter");
    wait_for(cx, "the copy", LIMIT, |_| to.is_file());
    let text = fs::read_to_string(&to).unwrap();
    assert!(text.contains("public class OwnerCopy"), "{text}");
    assert!(
        text.contains("package org.springframework.samples.petclinic.vet;"),
        "{text}"
    );
    wait_for(cx, "the copy to be the active tab", LIMIT, |cx| {
        active_path(cx, window).as_deref() == Some(to.as_path())
    });
    let view = crate::window_tests::editor_of(cx, window, &to);
    draw(cx, window);
    wait_for(cx, "the copy to have no errors", LIMIT, |cx| {
        errors(cx, &view) == 0
    });

    close(cx, window, language, false);
    finish(
        "owner_copied_into_another_package_by_the_refactor_menu_opens_in_the_editor",
        started,
    );
}
