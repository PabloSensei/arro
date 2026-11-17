//! A drag of a class in the project tree with the real language server on a copy of
//! PetClinic. The move goes through the same path as F6: the class gets the package of
//! the folder it lands in and the files that use it get the import.
//!
//! `cargo test -p panels window_tests_real_drag -- --ignored --test-threads=1`
//!
//! Needs `IDE_CORPUS` and a filled package cache. The corpus is copied, never changed.

use std::fs;
use std::time::{Duration, Instant};

use ui_kit::gpui;
use ui_kit::gpui::TestAppContext;

use crate::window_tests::{draw, editor_of, open_path, project_setup, scratch};
use crate::window_tests_menus::{drag_row, reveal, show_project};
use crate::window_tests_real::{
    build_window, close, errors, finish, project, wait_for, wait_ready,
};

/// One answer of the server after the drop.
const LIMIT: Duration = Duration::from_secs(120);

#[ui_kit::gpui::test]
#[ignore = "needs IDE_CORPUS and a filled package cache"]
fn dragging_a_class_updates_its_package_and_the_imports_of_its_users(cx: &mut TestAppContext) {
    let started = Instant::now();
    let (root, language) = project("real-drag");
    let package = "org.springframework.samples.petclinic";
    let from = window_language::main_class(&root, &format!("{package}.owner"), "PetType");
    let to = window_language::main_class(&root, &format!("{package}.vet"), "PetType");
    let user = window_language::main_class(&root, &format!("{package}.owner"), "PetTypeFormatter");
    let vet = to
        .parent()
        .expect("the target package folder")
        .to_path_buf();
    assert!(from.is_file(), "the corpus has the class to drag");
    assert!(user.is_file(), "the corpus has the class that uses it");

    let window = build_window(cx, root.clone(), &language, project_setup(scratch("cache")));
    cx.run_until_parked();
    draw(cx, window);
    wait_ready(cx, window);

    show_project(cx, window);
    reveal(cx, window, &from);
    reveal(cx, window, &vet);
    let name = from.file_name().unwrap_or_default().to_string_lossy();
    let row: &'static str = Box::leak(format!("tree-row-{name}").into_boxed_str());
    drag_row(cx, window, row, "tree-row-vet");
    wait_for(cx, "the moved class", LIMIT, |_| {
        to.is_file() && !from.exists()
    });

    let moved = fs::read_to_string(&to).unwrap();
    assert!(
        moved.contains("package org.springframework.samples.petclinic.vet;"),
        "{moved}"
    );
    let user_text = fs::read_to_string(&user).unwrap();
    assert!(
        user_text.contains("import org.springframework.samples.petclinic.vet.PetType;"),
        "{user_text}"
    );

    // The moved class and the file that uses it have no errors after the move.
    open_path(cx, window, &to);
    let moved_view = editor_of(cx, window, &to);
    open_path(cx, window, &user);
    let user_view = editor_of(cx, window, &user);
    draw(cx, window);
    wait_for(cx, "the moved class to have no errors", LIMIT, |cx| {
        errors(cx, &moved_view) == 0
    });
    wait_for(cx, "the user to have no errors", LIMIT, |cx| {
        errors(cx, &user_view) == 0
    });

    close(cx, window, language, false);
    finish(
        "dragging_a_class_updates_its_package_and_the_imports_of_its_users",
        started,
    );
}
