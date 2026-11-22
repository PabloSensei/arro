//! Find in Files under its list: the preview fills the box under the list, a long list reaches
//! the divider and keeps its selected row whole, a file row names its folder with `/`, the
//! arrow keys move the preview to the next match, and the divider keeps the height it was
//! dragged to.

use std::fs;
use std::path::{Path, PathBuf};

use keymap::workspace as keys;
use ui_kit::gpui;
use ui_kit::gpui::{
    Bounds, Entity, MouseButton, Pixels, TestAppContext, VisualTestContext, WindowHandle,
};
use ui_kit::{Modifiers, px};
use workspace::Workspace;

use crate::finder::Finder;
use crate::preview::FilePreview;
use crate::window_tests::{
    bounds_of, draw, open_palette, open_window, press, scratch, type_text, wait_for_file_index,
    wait_for_signal,
};
use crate::window_tests_finder_scope::finder;

/// Opens a project with the files and the window of Find in Files, or of Replace in Files
/// when `replace` is set, and returns the workspace window with the folder of the project.
fn open_finder_window(
    cx: &mut TestAppContext,
    name: &str,
    files: &[(&str, &str)],
    replace: bool,
) -> (WindowHandle<Workspace>, PathBuf) {
    let root = scratch(name);
    for (file, text) in files {
        let path = root.join(file);
        if let Some(folder) = path.parent() {
            fs::create_dir_all(folder).unwrap();
        }
        fs::write(&path, text).unwrap();
    }
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    draw(cx, window);
    wait_for_file_index(cx);
    let command = if replace {
        keys::REPLACE_IN_FILES
    } else {
        keys::FIND_IN_FILES
    };
    open_palette(cx, window, command);
    let finder = finder(cx);
    wait_for_signal(cx, "the file list of the dialog", |cx| {
        finder.read_with(cx, |finder, cx| {
            finder.files().read(cx).len() >= files.len()
        })
    });
    draw(cx, window);
    (window, root)
}

/// Types `text` into the window and waits until its search has answered.
fn search_for(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    text: &str,
) -> Entity<Finder> {
    let finder = finder(cx);
    type_text(cx, window, text);
    wait_for_signal(cx, "the answer of the search", |cx| {
        !finder.read_with(cx, |finder, _| finder.is_searching())
    });
    draw(cx, window);
    finder
}

/// The text the line under the fields shows in the last frame.
fn status_text() -> String {
    ui_kit::test_bounds()
        .into_iter()
        .find(|(name, _)| name == "finder-status-text")
        .and_then(|(_, bounds)| bounds.label)
        .map_or_else(String::new, |label| label.to_string())
}

/// The label a probe of the last frame recorded under `name`, if the frame drew it.
fn label_of(name: &str) -> Option<String> {
    ui_kit::test_bounds()
        .into_iter()
        .find(|(shown, _)| shown == name)
        .and_then(|(_, bounds)| bounds.label)
        .map(|label| label.to_string())
}

/// Waits until the preview shows `path` with `text` highlighted.
fn wait_for_preview(
    cx: &mut TestAppContext,
    preview: &Entity<FilePreview>,
    path: &Path,
    text: &'static str,
) {
    wait_for_signal(cx, "the preview of the selected row", |cx| {
        preview.read_with(cx, |preview, _| {
            preview.is_ready()
                && preview.path() == Some(path)
                && preview.highlight_text().as_deref() == Some(text)
        })
    });
}

/// The editor of the preview, which must exist once the text is on screen.
fn editor_of_preview(
    cx: &mut TestAppContext,
    preview: &Entity<FilePreview>,
) -> Entity<editor_view::EditorView> {
    preview
        .read_with(cx, |preview, _| preview.editor().cloned())
        .expect("the preview has an editor")
}

fn top(bounds: Bounds<Pixels>) -> f32 {
    f32::from(bounds.top())
}

fn bottom(bounds: Bounds<Pixels>) -> f32 {
    f32::from(bounds.bottom())
}

fn height(bounds: Bounds<Pixels>) -> f32 {
    f32::from(bounds.size.height)
}

/// Whether the line lies inside the box, top and bottom, within a pixel.
fn inside_box(line: Bounds<Pixels>, shown: Bounds<Pixels>) -> bool {
    top(line) >= top(shown) - 1. && bottom(line) <= bottom(shown) + 1.
}

/// Drags from `from` by (`dx`, `dy`) in eight steps, with the left button held, the way a
/// pointer moves. The steps leave the thin divider at once, so the drag must reach the
/// window and not the divider alone.
fn drag_by(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    from: gpui::Point<Pixels>,
    dx: f32,
    dy: f32,
) {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_move(from, None, Modifiers::default());
    visual.simulate_mouse_down(from, MouseButton::Left, Modifiers::default());
    drop(visual);
    // The frame after the press draws the layer that follows the drag, as a window does
    // before the next move of the pointer.
    cx.run_until_parked();
    draw(cx, window);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    for step in 1..=8 {
        let fraction = step as f32 / 8.;
        visual.simulate_mouse_move(
            gpui::point(from.x + px(dx * fraction), from.y + px(dy * fraction)),
            Some(MouseButton::Left),
            Modifiers::default(),
        );
    }
    visual.simulate_mouse_up(
        gpui::point(from.x + px(dx), from.y + px(dy)),
        MouseButton::Left,
        Modifiers::default(),
    );
    drop(visual);
    cx.run_until_parked();
    draw(cx, window);
}

#[gpui::test]
fn a_long_list_reaches_the_divider_and_the_preview_fills_its_box(cx: &mut TestAppContext) {
    let text: String = (1..=100).map(|line| format!("needle {line}\n")).collect();
    let (window, root) =
        open_finder_window(cx, "find-preview-layout", &[("many.txt", &text)], true);
    let finder = search_for(cx, window, "needle");
    assert_eq!(
        status_text(),
        "100 matches in 1 files",
        "every line of the file is a row of the list"
    );
    let preview = finder.read_with(cx, |finder, _| finder.preview().clone());
    wait_for_preview(cx, &preview, &root.join("many.txt"), "needle");
    draw(cx, window);

    let list = bounds_of(cx, window, "scroll-area-find-list");
    let divider = bounds_of(cx, window, "finder-splitter");
    assert!(
        (bottom(list) - top(divider)).abs() <= 1.,
        "the list ends where the divider starts: {} and {}",
        bottom(list),
        top(divider)
    );

    let shown = bounds_of(cx, window, "finder-preview");
    let filled = bounds_of(cx, window, "preview");
    assert!(
        height(filled) >= height(shown) - 1.,
        "the preview fills the box under the list: {} of {}",
        height(filled),
        height(shown)
    );
    let editor = editor_of_preview(cx, &preview);
    let line = editor
        .read_with(cx, |editor, _| editor.caret_line_bounds())
        .expect("the frame drew the line of the match");
    assert!(
        inside_box(line, shown),
        "the line of the match is on screen in the preview"
    );

    // The last match of the file is selected: its row is whole inside the list, and the
    // preview moves to its line.
    let downs = vec!["down"; 99].join(" ");
    press(cx, window, &downs);
    draw(cx, window);
    let list = bounds_of(cx, window, "scroll-area-find-list");
    let last = bounds_of(cx, window, "find-hit-100");
    assert!(
        top(last) >= top(list) - 1. && bottom(last) <= bottom(list) + 1.,
        "the selected row is whole inside the list: {:?} in {:?}",
        last,
        list
    );
    assert_eq!(
        editor.read_with(cx, |editor, _| editor.caret()),
        (99, 0),
        "the preview shows the line of the selected row"
    );
    let line = editor
        .read_with(cx, |editor, _| editor.caret_line_bounds())
        .expect("the frame drew the line of the match");
    assert!(
        inside_box(line, shown),
        "the line of the last match is on screen in the preview"
    );
}

#[gpui::test]
fn a_file_row_names_its_folder_with_slashes(cx: &mut TestAppContext) {
    let (window, _root) = open_finder_window(
        cx,
        "find-preview-paths",
        &[
            ("src/main/model/demo/Owner.txt", "class Owner {}\nowners\n"),
            ("LICENSE.txt", "owners\n"),
        ],
        false,
    );
    search_for(cx, window, "owners");
    assert_eq!(
        label_of("find-file-label-0").as_deref(),
        Some("LICENSE.txt  (1)"),
        "a file at the root of the project has no folder"
    );
    assert_eq!(
        label_of("find-file-label-2").as_deref(),
        Some("Owner.txt  src/main/model/demo  (1)"),
        "the folder comes after the name, relative to the project, with slashes"
    );
}

#[gpui::test]
fn down_moves_the_preview_to_the_next_file_and_shows_its_line(cx: &mut TestAppContext) {
    let (window, root) = open_finder_window(
        cx,
        "find-preview-arrow",
        &[
            ("a.txt", "first line\nalpha one\n"),
            ("b.txt", "beta alpha\nlast line\n"),
        ],
        false,
    );
    let finder = search_for(cx, window, "alpha");
    let preview = finder.read_with(cx, |finder, _| finder.preview().clone());
    wait_for_preview(cx, &preview, &root.join("a.txt"), "alpha");

    press(cx, window, "down");
    wait_for_preview(cx, &preview, &root.join("b.txt"), "alpha");
    draw(cx, window);
    let editor = editor_of_preview(cx, &preview);
    assert_eq!(
        editor.read_with(cx, |editor, _| editor.caret()),
        (0, 5),
        "the caret stands on the match of the second file"
    );
    let shown = bounds_of(cx, window, "finder-preview");
    let line = editor
        .read_with(cx, |editor, _| editor.caret_line_bounds())
        .expect("the frame drew the line of the match");
    assert!(inside_box(line, shown), "the matching line is on screen");
}

#[gpui::test]
fn a_dragged_divider_keeps_its_height_when_the_window_opens_again(cx: &mut TestAppContext) {
    let (window, _root) =
        open_finder_window(cx, "find-preview-divider", &[("a.txt", "alpha\n")], false);
    let before = height(bounds_of(cx, window, "finder-preview"));
    let divider = bounds_of(cx, window, "finder-splitter");
    drag_by(cx, window, divider.center(), 0., -60.);
    let dragged = height(bounds_of(cx, window, "finder-preview"));
    assert!(
        (dragged - before - 60.).abs() <= 4.,
        "the preview grows by the drag: {before} became {dragged}"
    );

    press(cx, window, "escape");
    draw(cx, window);
    open_palette(cx, window, keys::FIND_IN_FILES);
    let again = height(bounds_of(cx, window, "finder-preview"));
    assert!(
        (again - dragged).abs() <= 1.,
        "Find in Files opens with the height the divider was dragged to: {again}"
    );
}
