//! The editor settings of the Settings window: every switch reaches the open editor at
//! once, and the save rules change what Ctrl+S writes. No real window and no server.

use std::fs;
use std::path::PathBuf;

use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use workspace::{CompletionCase, Settings, Workspace};

use crate::settings_catalog;
use crate::window_tests::{
    buffer_of, draw, editor_of, open_path, open_window, press, scratch, type_text, wait_for_signal,
};
use crate::window_tests_lang::{FakeLanguage, open_window_with, project as lang_project};

fn project(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let root = scratch(name);
    for (path, text) in files {
        let path = root.join(path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, text).unwrap();
    }
    root
}

fn apply(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    change: impl FnOnce(&mut Settings),
) {
    window
        .update(cx, |workspace, _, cx| workspace.update_settings(change, cx))
        .unwrap();
    cx.run_until_parked();
}

fn has(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

#[gpui::test]
fn line_numbers_leave_the_gutter_of_the_open_editor(cx: &mut TestAppContext) {
    let root = project(
        "editor-settings-numbers",
        &[("src/main.txt", "alpha\nbeta\n")],
    );
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    draw(cx, window);
    let path = root.join("src").join("main.txt");
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    assert!(
        view.read_with(cx, |view, _| view.options().line_numbers),
        "the numbers are on by default"
    );

    apply(cx, window, |settings| settings.show_line_numbers = false);
    draw(cx, window);
    assert!(
        !view.read_with(cx, |view, _| view.options().line_numbers),
        "the line numbers go away at once"
    );

    apply(cx, window, |settings| settings.show_line_numbers = true);
    assert!(
        view.read_with(cx, |view, _| view.options().line_numbers),
        "and come back"
    );
}

#[gpui::test]
fn a_tab_size_of_two_changes_the_tab_of_a_new_file(cx: &mut TestAppContext) {
    let root = project("editor-settings-tab", &[("src/main.txt", "alpha\n")]);
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    draw(cx, window);
    let path = root.join("src").join("main.txt");
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);

    apply(cx, window, |settings| {
        settings_catalog::adjust_number(settings, "tab_size", 1.);
        settings_catalog::adjust_number(settings, "tab_size", -1.);
        settings_catalog::adjust_number(settings, "tab_size", -1.);
        settings_catalog::adjust_number(settings, "tab_size", -1.);
    });
    assert_eq!(
        cx.update(|cx| cx.global::<Settings>().tab_size),
        2,
        "Tab size is two"
    );

    press(cx, window, "tab");
    assert_eq!(
        buffer_of(cx, &view),
        "  alpha\n",
        "Tab adds the two spaces of the setting"
    );
}

#[gpui::test]
fn modified_lines_lose_their_trailing_spaces_and_the_others_keep_them(cx: &mut TestAppContext) {
    let root = project(
        "editor-settings-trim",
        &[("src/main.txt", "one\ntwo \nthree\n")],
    );
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    draw(cx, window);
    let path = root.join("src").join("main.txt");
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);

    view.update(cx, |view, cx| view.restore_caret(0, 3, cx));
    type_text(cx, window, "  ");
    assert_eq!(buffer_of(cx, &view), "one  \ntwo \nthree\n");
    view.update(cx, |view, cx| view.restore_caret(2, 5, cx));
    apply(cx, window, |settings| {
        settings.trim_trailing_whitespace = workspace::TrimTrailingWhitespace::Modified;
    });

    press(cx, window, "ctrl-s");
    wait_for_signal(cx, "the trimmed save", |_| {
        fs::read_to_string(&path).is_ok_and(|text| text == "one\ntwo \nthree\n")
    });
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "one\ntwo \nthree\n",
        "the changed line is trimmed and the unchanged line keeps its space"
    );
}

#[gpui::test]
fn a_save_adds_the_final_line_feed(cx: &mut TestAppContext) {
    let root = project("editor-settings-newline", &[("src/main.txt", "alpha")]);
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    draw(cx, window);
    let path = root.join("src").join("main.txt");
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    apply(cx, window, |settings| settings.ensure_final_newline = true);
    assert!(
        view.read_with(cx, |view, _| view.options().save.ensure_final_newline),
        "the save rule reaches the editor"
    );
    view.update(cx, |view, cx| view.restore_caret(0, 5, cx));
    type_text(cx, window, "!");

    press(cx, window, "ctrl-s");
    cx.run_until_parked();
    assert_eq!(
        buffer_of(cx, &view),
        "alpha!\n",
        "the buffer of the save ends with a line feed"
    );
    wait_for_signal(cx, "the final newline", |_| {
        fs::read_to_string(&path).is_ok_and(|text| text == "alpha!\n")
    });
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "alpha!\n",
        "the save puts a line feed after the last line"
    );
}

#[gpui::test]
fn the_case_of_the_completion_prefix_changes_the_list(cx: &mut TestAppContext) {
    let root = lang_project("editor-settings-case", &[("src/main.txt", "\n")]);
    let language = FakeLanguage::new();
    let window = open_window_with(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    let path = root.join("src").join("main.txt");
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);

    type_text(cx, window, "G");
    draw(cx, window);
    assert!(
        has(cx, window, "completion-popup"),
        "without case matching an uppercase prefix finds the lowercase entries"
    );

    apply(cx, window, |settings| {
        settings.completion_case = CompletionCase::All;
    });
    press(cx, window, "escape");
    press(cx, window, "ctrl-space");
    draw(cx, window);
    assert!(
        !has(cx, window, "completion-popup"),
        "with every letter matched by case no entry starts with G"
    );

    apply(cx, window, |settings| {
        settings.completion_case = CompletionCase::None;
    });
    press(cx, window, "ctrl-space");
    draw(cx, window);
    assert!(
        has(cx, window, "completion-popup"),
        "ignoring case brings the entries back"
    );
    press(cx, window, "enter");
    assert_eq!(buffer_of(cx, &view), "greet\n");
}

#[test]
fn the_settings_search_finds_the_new_editor_settings() {
    assert!(
        settings_catalog::search("tab size", 20)
            .iter()
            .any(|setting| setting.id == "tab_size")
    );
    assert!(
        settings_catalog::search("line numbers", 20)
            .iter()
            .any(|setting| setting.id == "show_line_numbers")
    );
    assert!(
        settings_catalog::search("indent guides", 20)
            .iter()
            .any(|setting| setting.id == "show_indent_guides")
    );
    assert!(
        settings_catalog::search("completion delay", 20)
            .iter()
            .any(|setting| setting.id == "completion_delay_ms")
    );
}
