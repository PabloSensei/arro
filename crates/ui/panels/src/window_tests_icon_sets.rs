//! The sets of icons in the window: the switch of the Appearance page changes the
//! icons of the stripes, the project tree, the tabs and the completion list without a
//! restart, the dark look takes the `_dark` files, and a folder of the settings becomes
//! a set of its own.

use std::fs;

use lang_api::{CompletionEntry, CompletionKind};
use ui_kit::gpui;
use ui_kit::gpui::TestAppContext;
use ui_kit::gpui::{VisualTestContext, WindowHandle};
use workspace::{SystemAppearance, Workspace};

use crate::window_tests::{draw, open_sample_window, press, scratch};
use crate::window_tests_complete::{CompleteLanguage, open_file, open_window};

const ARRO_PROJECT: &str = "icons/toolwindows/project_dark.svg";
const INTELLIJ_PROJECT: &str = "icon-sets/intellij/toolwindows/project_dark.svg";
const ARRO_FOLDER: &str = "icons/nodes/folder_dark.svg";
const INTELLIJ_FOLDER: &str = "icon-sets/intellij/nodes/folder_dark.svg";
const INTELLIJ_FOLDER_LIGHT: &str = "icon-sets/intellij/nodes/folder.svg";
const ARRO_TEXT_FILE: &str = "icons/fileTypes/text_dark.svg";
const INTELLIJ_TEXT_FILE: &str = "icon-sets/intellij/fileTypes/text_dark.svg";
const INTELLIJ_CLASS: &str = "icon-sets/intellij/nodes/class_dark.svg";
const CUSTOM_FOLDER: &str = "icon-sets/mine/nodes/folder_dark.svg";

fn has(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

#[ui_kit::gpui::test]
fn the_appearance_page_offers_the_icon_sets(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "icon-list");
    press(cx, window, "ctrl-alt-s");
    draw(cx, window);
    assert!(has(cx, window, "settings-icon-set-arro"), "Arro is there");
    assert!(
        has(cx, window, "settings-icon-set-intellij"),
        "IntelliJ New UI is there"
    );
}

#[ui_kit::gpui::test]
fn switching_the_icon_set_changes_the_icons_without_a_restart(cx: &mut TestAppContext) {
    cx.update(|cx| cx.set_global(SystemAppearance { dark: true }));
    let root = scratch("icon-switch");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/A.txt"), "al\n").unwrap();
    let language = CompleteLanguage::new();
    let window = open_window(cx, root.clone(), language.clone());
    cx.run_until_parked();
    draw(cx, window);
    open_file(cx, window, &root, "src/A.txt", "al\n", 0, 2);

    language.set_entries(vec![CompletionEntry {
        label: "alClass".to_owned(),
        insert_text: Some("alClass".to_owned()),
        kind: CompletionKind::Class,
        ..CompletionEntry::default()
    }]);
    press(cx, window, "ctrl-space");
    draw(cx, window);
    assert!(has(cx, window, "completion-popup"), "the list opens");
    assert!(
        has(cx, window, "completion-row-0"),
        "the entry is in the list"
    );

    assert!(has(cx, window, ARRO_PROJECT), "the stripes start with arro");
    assert!(has(cx, window, ARRO_FOLDER), "the tree starts with arro");
    assert!(has(cx, window, ARRO_TEXT_FILE), "the tab starts with arro");
    assert!(!has(cx, window, INTELLIJ_PROJECT));

    window
        .update(cx, |workspace, _, cx| {
            workspace.assign_icon_set("intellij", cx)
        })
        .unwrap();
    draw(cx, window);

    assert!(
        has(cx, window, INTELLIJ_PROJECT),
        "the stripe takes the icon of the new set"
    );
    assert!(
        has(cx, window, INTELLIJ_FOLDER),
        "the tree takes the icon of the new set"
    );
    assert!(
        has(cx, window, INTELLIJ_TEXT_FILE),
        "the tab takes the icon of the new set"
    );
    assert!(
        has(cx, window, INTELLIJ_CLASS),
        "the completion list takes the icon of the new set"
    );
    assert!(
        !has(cx, window, ARRO_PROJECT),
        "the old set leaves the stripe"
    );
}

#[ui_kit::gpui::test]
fn the_icon_set_choice_is_saved_in_the_settings_file(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "icon-save");
    let file = window
        .read_with(cx, |workspace, _| {
            workspace.settings_dir().join("settings.json")
        })
        .unwrap();
    window
        .update(cx, |workspace, _, cx| {
            workspace.assign_icon_set("intellij", cx)
        })
        .unwrap();
    assert_eq!(
        cx.read(|cx| cx.global::<workspace::Settings>().icon_set.clone()),
        "intellij",
        "the setting is in force at once"
    );
    crate::window_tests::wait_for_signal(cx, "the settings file", |_| {
        fs::read_to_string(&file).is_ok_and(|text| text.contains(r#""icon_set": "intellij""#))
    });
}

#[ui_kit::gpui::test]
fn a_dark_theme_takes_the_dark_files_of_the_set(cx: &mut TestAppContext) {
    cx.update(|cx| cx.set_global(SystemAppearance { dark: true }));
    let (_, window) = open_sample_window(cx, "icon-dark");
    window
        .update(cx, |workspace, _, cx| {
            workspace.assign_icon_set("intellij", cx)
        })
        .unwrap();
    draw(cx, window);
    assert!(
        has(cx, window, INTELLIJ_FOLDER),
        "the dark theme takes the _dark file"
    );

    window
        .update(cx, |workspace, _, cx| workspace.preview_theme("light", cx))
        .unwrap();
    draw(cx, window);
    assert!(
        has(cx, window, INTELLIJ_FOLDER_LIGHT),
        "the light theme takes the light file"
    );
}

#[ui_kit::gpui::test]
fn a_custom_icon_set_of_the_settings_folder_joins_the_list(cx: &mut TestAppContext) {
    cx.update(|cx| cx.set_global(SystemAppearance { dark: true }));
    let (_, window) = open_sample_window(cx, "icon-user");
    let settings_dir = window
        .read_with(cx, |workspace, _| workspace.settings_dir().to_path_buf())
        .unwrap();
    let set = settings_dir.join("icons").join("mine");
    fs::create_dir_all(set.join("nodes")).unwrap();
    let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16"></svg>"#;
    fs::write(set.join("nodes/folder.svg"), svg).unwrap();
    fs::write(set.join("nodes/folder_dark.svg"), svg).unwrap();
    window
        .update(cx, |workspace, _, cx| workspace.reload_icon_sets(cx))
        .unwrap();
    window
        .update(cx, |workspace, _, cx| workspace.assign_icon_set("mine", cx))
        .unwrap();
    draw(cx, window);
    assert!(
        has(cx, window, CUSTOM_FOLDER),
        "a set of the settings folder draws its own icon"
    );

    press(cx, window, "ctrl-alt-s");
    draw(cx, window);
    assert!(
        has(cx, window, "settings-icon-set-mine"),
        "the set of the settings folder is in the list"
    );
}
