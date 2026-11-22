//! The File Structure keeps each name against its signature, and the gap of its row stands only
//! between the icon and the name. The Reformat File dialog names the file in its title, without
//! the path, and offers Selected text only while the editor has a selection.
//!
//! `cargo test -p panels window_tests_signature_gap_reformat`

use std::fs;
use std::path::PathBuf;

use keymap::workspace as keys;
use ui_kit::gpui;
use ui_kit::gpui::{Entity, TestAppContext, WindowHandle};
use ui_kit::metrics::SPACE_S;
use workspace::{Settings, Workspace};

use crate::reformat::ReformatDialog;
use crate::window_tests::{
    bounds_of, click, draw, editor_of, open_palette, open_path, open_window, press, scratch,
};
use crate::window_tests_commit_tree::selector;
use crate::window_tests_file_structure_signatures::{owner_window, palette};
use crate::window_tests_git::has_selector;

/// The File Structure lists `getPet(String)` as its name with the signature after it. The name
/// ends where the signature begins, and the gap of the row stands between the icon and the name.
#[ui_kit::gpui::test]
fn the_name_of_a_declaration_ends_where_its_signature_begins(cx: &mut TestAppContext) {
    let (_, window) = owner_window(cx, "signature-gap-structure");
    open_palette(cx, window, keys::FILE_STRUCTURE);
    let rows = palette(cx, window).read_with(cx, |palette, _| palette.rows_text());
    let row = ("getPet".to_owned(), "(String): Pet".to_owned());
    let index = rows
        .iter()
        .position(|found| *found == row)
        .unwrap_or_else(|| panic!("{row:?} is among the rows {rows:?}"));
    let icon = bounds_of(cx, window, selector(format!("palette-icon-{index}")));
    let name = bounds_of(cx, window, selector(format!("palette-title-{index}")));
    let signature = bounds_of(cx, window, selector(format!("palette-signature-{index}")));
    let joined = f32::from(signature.left() - name.right());
    assert!(
        joined.abs() <= 1.,
        "the signature starts {joined} px after the name"
    );
    let gap = f32::from(name.left() - icon.right());
    assert!(
        gap >= SPACE_S - 0.01,
        "the name starts {gap} px after its icon, and the row gives {SPACE_S} px"
    );
}

/// A window with a text file open in its editor, and the path of that file.
fn text_window(cx: &mut TestAppContext, name: &str) -> (PathBuf, WindowHandle<Workspace>) {
    let root = scratch(name);
    let file = root.join("src").join("Notes.txt");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, "alpha\nbeta\n").unwrap();
    let window = open_window(cx, root);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &file);
    (file, window)
}

/// The Reformat File dialog that is open in the window.
fn reformat_dialog(cx: &TestAppContext, window: WindowHandle<Workspace>) -> Entity<ReformatDialog> {
    window
        .read_with(cx, |workspace, _| workspace.overlay_view())
        .unwrap()
        .expect("a dialog is open")
        .downcast::<ReformatDialog>()
        .expect("the dialog is Reformat File")
}

/// The title of the dialog is the name of the file, and no path is drawn under it.
#[ui_kit::gpui::test]
fn reformat_file_titles_the_dialog_with_the_file_name_and_no_path(cx: &mut TestAppContext) {
    let (file, window) = text_window(cx, "reformat-title-name");
    press(cx, window, "ctrl-alt-shift-l");
    draw(cx, window);
    let dialog = reformat_dialog(cx, window);
    let title = dialog.read_with(cx, |dialog, _| dialog.title());
    assert_eq!(title, "Reformat File: Notes.txt");
    let folder = file.parent().unwrap().display().to_string();
    assert!(
        !title.contains(&folder),
        "the title names no folder: {title}"
    );
    assert!(
        !has_selector(cx, window, "reformat-title"),
        "no path is drawn under the title"
    );
}

/// Without a selection in the editor, Selected text is unavailable: the whole file is chosen,
/// and a click on the radio changes nothing.
#[ui_kit::gpui::test]
fn selected_text_is_unavailable_without_a_selection(cx: &mut TestAppContext) {
    let (_, window) = text_window(cx, "reformat-scope-none");
    press(cx, window, "ctrl-alt-shift-l");
    draw(cx, window);
    let dialog = reformat_dialog(cx, window);
    assert_eq!(
        dialog.read_with(cx, |dialog, _| dialog.scope()),
        (false, false),
        "Selected text is not offered and the whole file is chosen"
    );

    click(cx, window, "reformat-selected");
    draw(cx, window);
    assert_eq!(
        dialog.read_with(cx, |dialog, _| dialog.scope()),
        (false, false),
        "a click does not choose the unavailable Selected text"
    );
    press(cx, window, "escape");
}

/// With a selection in the editor, the dialog starts on Selected text, and either scope can be
/// chosen from there.
#[ui_kit::gpui::test]
fn selected_text_is_the_default_with_a_selection(cx: &mut TestAppContext) {
    let (file, window) = text_window(cx, "reformat-scope-selection");
    press(cx, window, "ctrl-a");
    let view = editor_of(cx, window, &file);
    let selected = view.read_with(cx, |view, _| view.selection().map(|(range, _)| range));
    assert!(
        selected.is_some_and(|range| !range.is_empty()),
        "the text is selected"
    );

    press(cx, window, "ctrl-alt-shift-l");
    draw(cx, window);
    let dialog = reformat_dialog(cx, window);
    assert_eq!(
        dialog.read_with(cx, |dialog, _| dialog.scope()),
        (true, true),
        "Selected text is offered and chosen"
    );

    click(cx, window, "reformat-whole-file");
    draw(cx, window);
    assert_eq!(
        dialog.read_with(cx, |dialog, _| dialog.scope()),
        (true, false),
        "the whole file can be chosen"
    );
    click(cx, window, "reformat-selected");
    draw(cx, window);
    assert_eq!(
        dialog.read_with(cx, |dialog, _| dialog.scope()),
        (true, true),
        "the selection can be chosen again"
    );
    press(cx, window, "escape");
}

/// A run from a file without a selection does not reset the scope that Ctrl+Alt+L remembers.
#[ui_kit::gpui::test]
fn running_without_a_selection_keeps_the_remembered_scope(cx: &mut TestAppContext) {
    let (_, window) = text_window(cx, "reformat-scope-remembered");
    window
        .update(cx, |workspace, _, cx| {
            workspace.update_settings(|settings| settings.reformat_selection = true, cx)
        })
        .unwrap();
    press(cx, window, "ctrl-alt-shift-l");
    draw(cx, window);
    click(cx, window, "reformat-run");
    cx.run_until_parked();
    assert!(
        cx.read_global::<Settings, _>(|settings, _| settings.reformat_selection),
        "the remembered scope stays the selection"
    );
}
