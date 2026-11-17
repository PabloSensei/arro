//! Spelling: the green wave under a misspelled word of a comment, the corrections of
//! Alt+Enter, saving the word and turning the check off in the settings.

use std::fs;
use std::path::Path;

use editor_view::EditorView;
use lang_api::{InspectionInfo, InspectionLevel, SPELLING_INSPECTION, SpellPlace};
use ui_kit::Modifiers;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use ui_kit::{point, px};
use workspace::Workspace;

use crate::settings::SettingsView;
use crate::window_tests::{
    bounds_of, buffer_of, click, draw, editor_of, line_height, open_path, press, wait_for_signal,
};
use crate::window_tests_lang::{FakeLanguage, open_window_with, project};

fn check(id: &str, title: &str) -> InspectionInfo {
    InspectionInfo {
        id: id.to_owned(),
        title: title.to_owned(),
        group: "Spelling".to_owned(),
        default_severity: InspectionLevel::WeakWarning,
        configurable: true,
    }
}

/// The words the check knows during a test, in place of the dictionary of the system.
fn dictionary(cx: &mut TestAppContext, words: &[&str]) {
    cx.update(|cx| crate::spelling::use_words(cx, words));
}

fn marks(
    cx: &TestAppContext,
    window: WindowHandle<Workspace>,
    path: &Path,
) -> Vec<(lang_api::Severity, std::ops::Range<usize>)> {
    editor_of(cx, window, path).read_with(cx, |view: &EditorView, _| view.diagnostic_marks())
}

fn click_editor(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    line: usize,
    column: usize,
) {
    let height = line_height(cx);
    let bounds = bounds_of(cx, window, "editor-body-0");
    let x = bounds.left() + px(67. + column as f32 * 8.);
    let y = bounds.top() + px(height * line as f32 + height / 2.);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_click(point(x, y), Modifiers::default());
    cx.run_until_parked();
}

#[gpui::test]
fn a_typo_in_a_comment_is_waved_and_replaced(cx: &mut TestAppContext) {
    let root = project(
        "spelling-comment",
        &[("src/Notes.txt", "// calcualte the total\n")],
    );
    let path = root.join("src").join("Notes.txt");
    let language = FakeLanguage::new();
    let window = open_window_with(cx, root.clone(), language);
    cx.run_until_parked();
    dictionary(cx, &["calculate", "the", "total"]);
    draw(cx, window);
    open_path(cx, window, &path);
    draw(cx, window);

    wait_for_signal(cx, "the misspelled word", |cx| {
        marks(cx, window, &path)
            .iter()
            .any(|(severity, range)| *severity == lang_api::Severity::Hint && range.start == 3)
    });

    click_editor(cx, window, 0, 5);
    press(cx, window, "alt-enter");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "actions-popup").size.height > px(0.),
        "Alt+Enter lists the corrections of the word"
    );
    press(cx, window, "enter");
    draw(cx, window);

    wait_for_signal(cx, "the corrected text", |cx| {
        buffer_of(cx, &editor_of(cx, window, &path)).contains("calculate")
    });
    assert_eq!(
        buffer_of(cx, &editor_of(cx, window, &path)),
        "// calculate the total\n"
    );
}

#[gpui::test]
fn saving_a_word_to_the_project_dictionary_takes_the_wave_away(cx: &mut TestAppContext) {
    let root = project("spelling-save", &[("src/Notes.txt", "// calcualte\n")]);
    let path = root.join("src").join("Notes.txt");
    let language = FakeLanguage::new();
    let window = open_window_with(cx, root.clone(), language);
    cx.run_until_parked();
    dictionary(cx, &["calculate"]);
    draw(cx, window);
    open_path(cx, window, &path);
    draw(cx, window);
    wait_for_signal(cx, "the misspelled word", |cx| {
        !marks(cx, window, &path).is_empty()
    });

    click_editor(cx, window, 0, 5);
    press(cx, window, "alt-enter");
    draw(cx, window);
    // The first row is the correction; the one after it saves the word to the project.
    press(cx, window, "down enter");
    cx.run_until_parked();

    wait_for_signal(cx, "the project dictionary", |_| {
        fs::read_to_string(root.join(".arro").join("dictionary.txt"))
            .is_ok_and(|text| text.lines().any(|line| line.trim() == "calcualte"))
    });
    wait_for_signal(cx, "the wave to go away", |cx| {
        marks(cx, window, &path).is_empty()
    });
}

#[gpui::test]
fn the_check_turned_off_in_the_settings_leaves_the_text(cx: &mut TestAppContext) {
    let root = project("spelling-off", &[("src/Notes.txt", "// calcualte\n")]);
    let path = root.join("src").join("Notes.txt");
    let language = FakeLanguage::new();
    language.set_inspections(vec![check(SPELLING_INSPECTION, "Typo")]);
    let window = open_window_with(cx, root.clone(), language);
    cx.run_until_parked();
    dictionary(cx, &["calculate"]);
    draw(cx, window);
    open_path(cx, window, &path);
    draw(cx, window);
    wait_for_signal(cx, "the misspelled word", |cx| {
        !marks(cx, window, &path).is_empty()
    });

    window
        .update(cx, |workspace, window, cx| {
            SettingsView::show_inspection(workspace, window, cx, SPELLING_INSPECTION);
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
    click(cx, window, "settings-inspection-toggle-0");
    draw(cx, window);
    click(cx, window, "settings-ok");
    cx.run_until_parked();
    draw(cx, window);

    wait_for_signal(cx, "the wave to go away", |cx| {
        marks(cx, window, &path).is_empty()
    });
    assert!(
        fs::read_to_string(workspace::inspections_file(&root)).is_ok_and(|text| {
            text.contains(SPELLING_INSPECTION) && text.contains("enabled = false")
        }),
        "the check is off in the project settings"
    );
}

#[gpui::test]
fn a_typo_in_a_declaration_name_renames_the_symbol_and_its_uses(cx: &mut TestAppContext) {
    let root = project(
        "spelling-rename",
        &[(
            "src/Main.txt",
            "void calcualteTotal() {\n    calcualteTotal();\n}\n",
        )],
    );
    let path = root.join("src").join("Main.txt");
    let language = FakeLanguage::new();
    let text = "void calcualteTotal() {\n    calcualteTotal();\n}\n";
    let start = text.find("calcualteTotal").unwrap();
    language.set_spelling_places(vec![SpellPlace {
        start,
        end: start + "calcualteTotal".len(),
        declaration: true,
    }]);
    let window = open_window_with(cx, root.clone(), language);
    cx.run_until_parked();
    dictionary(cx, &["calculate", "total"]);
    draw(cx, window);
    open_path(cx, window, &path);
    draw(cx, window);
    wait_for_signal(cx, "the misspelled name", |cx| {
        marks(cx, window, &path)
            .iter()
            .any(|(severity, range)| *severity == lang_api::Severity::Hint && range.start == start)
    });

    click_editor(cx, window, 0, 8);
    press(cx, window, "alt-enter");
    draw(cx, window);
    press(cx, window, "enter");
    cx.run_until_parked();

    wait_for_signal(cx, "the renamed symbol", |cx| {
        buffer_of(cx, &editor_of(cx, window, &path)).contains("calculateTotal")
    });
    assert_eq!(
        buffer_of(cx, &editor_of(cx, window, &path)),
        "void calculateTotal() {\n    calculateTotal();\n}\n",
        "the declaration and its use are renamed"
    );
}
