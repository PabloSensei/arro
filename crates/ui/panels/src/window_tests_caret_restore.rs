//! Where a jump or a restored session leaves the caret of a file. The status line shows the
//! caret at once, a command that acts on the caret sees the place it went to, and a command
//! pressed while the file is still read waits for the text and acts on the place the file
//! opens at.
//!
//! cargo test -p panels window_tests_caret_restore

use editor_view::Spot;
use lang_api::{ActionEntry, ActionKind, SourceEdit};
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::Workspace;

use crate::navigation;
use crate::window_tests::{active_path, buffer_of, draw, editor_of, open_path, press};
use crate::window_tests_cached_views::frame;
use crate::window_tests_copy_class::shown;
use crate::window_tests_editor_popups::{PopupLanguage, open_window_with};
use crate::window_tests_lang::{edit_in, uri_of};
use crate::window_tests_rename_in_place::{
    RenameLanguage, open_window as open_rename_window, project,
};

/// Line 98 of a file indented with tabs, as the walk saw it. Column 20 is the space before
/// `owner`, column 30 is the place between `get` and `LastName`, and columns 10 to 17 are
/// `lastName`.
const OWNER_LINE: &str = "\t\tString lastName = owner.getLastName();\n";

/// A file whose line 98 is the owner line, so the caret lines of the walk exist in it.
fn owner_file() -> String {
    let mut text = "\n".repeat(97);
    text.push_str(OWNER_LINE);
    text
}

/// The caret the status line drew at its last frame.
fn status_caret(cx: &TestAppContext, window: WindowHandle<Workspace>) -> Option<String> {
    window
        .read_with(cx, |workspace, cx| workspace.status_caret_on_screen(cx))
        .unwrap()
}

/// A caret put into a loaded file is the place Shift+F6 acts on, and the status line shows
/// it after one frame. Column 12 is inside `lastName`, so the status line says 98:12.
#[ui_kit::gpui::test]
fn shift_f6_right_after_a_restored_caret_renames_the_name_there(cx: &mut TestAppContext) {
    let text = owner_file();
    let root = project("caret-restore-rename", &[("src/Owner.txt", text.as_str())]);
    let file = root.join("src").join("Owner.txt");
    let window = open_rename_window(cx, root.clone(), RenameLanguage::new());
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &file);
    let view = editor_of(cx, window, &file);

    view.update(cx, |view, cx| view.restore_caret(97, 11, cx));
    press(cx, window, "shift-f6");
    frame(cx, window);
    assert!(
        shown(cx, window, "rename-in-place-hint"),
        "Shift+F6 starts changing the name in place"
    );
    assert_eq!(
        status_caret(cx, window).as_deref(),
        Some("98:12"),
        "the status line shows the caret that was put"
    );
    assert_eq!(
        buffer_of(cx, &view),
        text,
        "the text stays as it is until a letter is typed"
    );
}

/// Two carets put one after the other: after one frame the status line shows the last of
/// them, not the first one that it kept from before.
#[ui_kit::gpui::test]
fn the_status_line_shows_the_last_restored_caret_after_one_frame(cx: &mut TestAppContext) {
    let text = owner_file();
    let root = project("caret-restore-status", &[("src/Owner.txt", text.as_str())]);
    let file = root.join("src").join("Owner.txt");
    let window = open_rename_window(cx, root.clone(), RenameLanguage::new());
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &file);
    let view = editor_of(cx, window, &file);

    // Column 20 is the space before `owner`; column 30 is inside `getLastName`.
    view.update(cx, |view, cx| view.restore_caret(97, 19, cx));
    view.update(cx, |view, cx| view.restore_caret(97, 29, cx));
    frame(cx, window);
    assert_eq!(status_caret(cx, window).as_deref(), Some("98:30"));
}

/// Ctrl+B into another file that is still read, and Alt+Enter right after it: the list of
/// actions opens at the place of the jump, not at the start of the file.
#[ui_kit::gpui::test]
fn alt_enter_right_after_a_jump_into_a_file_still_read_opens_at_the_jump(cx: &mut TestAppContext) {
    let owner = owner_file();
    let root = project(
        "caret-restore-jump",
        &[
            ("src/Source.txt", "go\n"),
            ("src/Owner.txt", owner.as_str()),
        ],
    );
    let source = root.join("src").join("Source.txt");
    let target = root.join("src").join("Owner.txt");
    let language = PopupLanguage::new();
    language.set_actions(vec![ActionEntry {
        title: "Organize the owner".to_owned(),
        kind: Some(ActionKind::QuickFix),
        code: Some("quickfix.organize".to_owned()),
        preferred: true,
        // The server offers an edit that changes nothing: the list keeps only applicable rows.
        edits: vec![SourceEdit {
            uri: uri_of(&target),
            edits: vec![edit_in(&owner, 0, 1, "\n")],
        }],
        renames: Vec::new(),
        handle: None,
        resolve: None,
        rename: None,
    }]);
    let window = open_window_with(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &source);

    // What Ctrl+B does once the declaration is known: the other file opens at line 98.
    window
        .update(cx, |workspace, window, cx| {
            navigation::go_to(
                workspace,
                target.clone(),
                Some(Spot::Line {
                    line: 97,
                    column: 11,
                }),
                true,
                window,
                cx,
            )
        })
        .unwrap();
    let view = editor_of(cx, window, &target);
    assert!(
        !view.read_with(cx, |view, _| view.is_loaded()),
        "the file is still read when Alt+Enter comes"
    );

    press(cx, window, "alt-enter");
    draw(cx, window);
    assert_eq!(active_path(cx, window), Some(target.clone()));
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (97, 11));
    assert!(
        view.read_with(cx, |view, _| view.actions_visible().is_some()),
        "the list of actions is asked for the place of the jump"
    );
}

/// A script that puts the caret on `delta` and presses Shift+F6 starts the rename of it.
#[ui_kit::gpui::test]
fn the_caret_and_shift_f6_steps_of_a_script_rename_the_name(cx: &mut TestAppContext) {
    let root = project(
        "caret-restore-script",
        &[("src/Names.txt", "alpha\nbeta\ngamma delta\n")],
    );
    let file = root.join("src").join("Names.txt");
    let window = open_rename_window(cx, root.clone(), RenameLanguage::new());
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &file);

    window
        .update(cx, |_, window, cx| {
            crate::script::start("caret:3:7;key:shift-f6", window, cx)
        })
        .unwrap();
    for _ in 0..5 {
        cx.run_until_parked();
        draw(cx, window);
    }
    assert!(
        shown(cx, window, "rename-in-place-hint"),
        "the script starts changing the name in place"
    );
}

/// Shift+F6 pressed while the file is still read waits for the text. Then the rename starts
/// at the caret that was put before it, not silently at the start of the file.
#[ui_kit::gpui::test]
fn shift_f6_while_the_file_is_read_starts_at_the_restored_caret(cx: &mut TestAppContext) {
    let text = owner_file();
    let root = project("caret-restore-loading", &[("src/Owner.txt", text.as_str())]);
    let file = root.join("src").join("Owner.txt");
    let window = open_rename_window(cx, root.clone(), RenameLanguage::new());
    cx.run_until_parked();
    draw(cx, window);

    window
        .update(cx, |workspace, window, cx| {
            workspace.open_file(file.clone(), window, cx)
        })
        .unwrap();
    let view = editor_of(cx, window, &file);
    assert!(
        !view.read_with(cx, |view, _| view.is_loaded()),
        "the file is still read"
    );
    view.update(cx, |view, cx| view.restore_caret(97, 11, cx));
    press(cx, window, "shift-f6");
    draw(cx, window);
    assert!(
        shown(cx, window, "rename-in-place-hint"),
        "the rename starts once the text is read"
    );
    // The rename selects the name the caret was put in; the caret itself sits at its end.
    assert_eq!(
        view.read_with(cx, |view, _| view.selected_text()),
        Some("lastName".to_owned()),
        "the rename selects the name the caret was put in"
    );
}
