//! The buttons that switch a markdown tab between the editor, the editor and the preview,
//! and the preview show an icon in both icon sets and in both looks. A name that the
//! IntelliJ set has no file for takes the file of `arro`, so no button is left empty.

use std::fs;
use std::time::Duration;

use ui_kit::gpui;
use ui_kit::gpui::{AssetSource as _, TestAppContext, VisualTestContext, WindowHandle};
use ui_kit::metrics::ICON;
use ui_kit::{ARRO, Assets, INTELLIJ, Theme, icon_file};
use workspace::{SystemAppearance, Workspace};

use crate::window_tests::{bounds_of, draw, open_path, open_sample_window};

/// The buttons of the mode of a markdown tab, each with the icon it names.
const MODE_BUTTONS: [(&str, &str); 3] = [
    ("markdown-mode-editor", "general/editorOnly"),
    ("markdown-mode-split", "general/editorAndPreview"),
    ("markdown-mode-preview", "general/previewOnly"),
];

/// Whether the binary has the file of the icon, with some data in it.
fn in_binary(path: &str) -> bool {
    Assets
        .load(path)
        .ok()
        .flatten()
        .is_some_and(|data| !data.is_empty())
}

/// The debug selector of an icon is its file, so the file is looked up by it.
fn selector(path: String) -> &'static str {
    Box::leak(path.into_boxed_str())
}

/// The icon of a button lies inside the button, and the icon is drawn at all.
fn icon_is_in_button(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    button: &'static str,
    path: &'static str,
) {
    let button = bounds_of(cx, window, button);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    let icon = visual
        .debug_bounds(path)
        .unwrap_or_else(|| panic!("the button {button:?} shows no {path}"));
    assert!(
        button.contains(&icon.center()),
        "the icon {path} is outside the button {button:?}: {icon:?}"
    );
}

/// The system changes its look, and the window follows it, as `SystemAppearance` does in
/// a real window.
fn set_system_theme(cx: &mut TestAppContext, window: WindowHandle<Workspace>, dark: bool) {
    cx.update(|cx| cx.set_global(SystemAppearance { dark }));
    window
        .update(cx, |workspace, _, cx| {
            workspace.on_system_appearance_changed(cx)
        })
        .unwrap();
    cx.run_until_parked();
    for _ in 0..100 {
        draw(cx, window);
        if cx.read(|cx| cx.global::<Theme>().dark) == dark {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
        cx.run_until_parked();
    }
    panic!(
        "the window did not become {}",
        if dark { "dark" } else { "light" }
    );
}

fn set_icon_set(cx: &mut TestAppContext, window: WindowHandle<Workspace>, set: &str) {
    window
        .update(cx, |workspace, _, cx| workspace.assign_icon_set(set, cx))
        .unwrap();
}

#[ui_kit::gpui::test]
fn the_mode_buttons_of_a_markdown_tab_show_an_icon_in_both_sets_and_looks(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "markdown-mode-icons");
    let path = root.join("guide.md");
    fs::write(&path, "# Guide\n\nSome words.\n").unwrap();
    open_path(cx, window, &path);
    draw(cx, window);

    for set in [ARRO, INTELLIJ] {
        set_icon_set(cx, window, set);
        for dark in [false, true] {
            set_system_theme(cx, window, dark);
            for (button, name) in MODE_BUTTONS {
                let file = icon_file(set, name, dark, ICON).to_string();
                assert!(
                    in_binary(&file),
                    "{name} of the {set} set has no file {file} in the binary"
                );
                icon_is_in_button(cx, window, button, selector(file));
            }
        }
    }
}
