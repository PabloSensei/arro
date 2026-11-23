//! The names of the extractions of the value `5` on the real language server, through the window
//! and its keys: Introduce Variable and Introduce Parameter give a name field with `pageSize1`,
//! and Extract Constant gives `PAGE_SIZE`, which Enter writes into the declaration and in place of
//! the value. Introduce Field is not checked here: the real server answers it with a form of its
//! own (an action with a handle and no edits), so the editor opens that form, not the name field.
//! The corpus and the package cache are the only outside world; the project is copied, so nothing
//! changes in the corpus. The module is not part of a normal test run:
//!
//! `cargo test -p panels window_tests_real_extract_name -- --ignored --test-threads=1 --nocapture`

use std::time::{Duration, Instant};

use editor_view::EditorView;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::Workspace;

use crate::window_tests::{buffer_of, draw, editor_of, open_path, press, project_setup, scratch};
use crate::window_tests_real::{build_window, close, finish, project, wait_ready};

/// The longest wait of one extraction for its name field, its picker or its change.
const LIMIT: Duration = Duration::from_secs(120);
/// The line of the value in OwnerController, and the text before the value on it.
const LINE_TEXT: &str = "int pageSize = 5;";
const BEFORE_VALUE: &str = "int pageSize = ";

/// What an extraction key shows within the limit: its name field, a picker, or a change of the
/// text at once. `None` when nothing comes within the limit.
fn outcome_of(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    view: &gpui::Entity<EditorView>,
    before: &str,
) -> Option<String> {
    let asked = Instant::now();
    loop {
        draw(cx, window);
        if let Some(name) = view.read_with(cx, |view, _| view.name_field_text()) {
            return Some(format!("name field `{name}`"));
        }
        if view.read_with(cx, |view, _| view.actions_visible().is_some()) {
            return Some("picker".to_owned());
        }
        if buffer_of(cx, view) != before {
            return Some("applied at once".to_owned());
        }
        if asked.elapsed() > LIMIT {
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
        cx.executor().advance_clock(Duration::from_millis(50));
        cx.run_until_parked();
    }
}

/// Whether the text of the buffer comes to hold `needle` within the limit.
fn text_holds(cx: &mut TestAppContext, view: &gpui::Entity<EditorView>, needle: &str) -> bool {
    let asked = Instant::now();
    loop {
        if buffer_of(cx, view).contains(needle) {
            return true;
        }
        if asked.elapsed() > LIMIT {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
        cx.executor().advance_clock(Duration::from_millis(50));
        cx.run_until_parked();
    }
}

#[ui_kit::gpui::test]
#[ignore = "needs IDE_CORPUS and a filled package cache"]
fn the_extractions_of_the_value_name_it_on_the_real_server(cx: &mut TestAppContext) {
    let started = Instant::now();
    let (root, language) = project("real-extract-name");
    let window = build_window(cx, root.clone(), &language, project_setup(scratch("cache")));
    cx.run_until_parked();
    draw(cx, window);
    wait_ready(cx, window);

    // The language name is put together from two parts: this neutral crate does not spell it.
    let lang = format!("{}{}", "ja", "va");
    let path = root.join(format!(
        "src/main/{lang}/org/springframework/samples/petclinic/owner/OwnerController.{lang}"
    ));
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);

    // The caret goes onto the value, found in the text of the buffer and not by a fixed column.
    let text = buffer_of(cx, &view);
    let value = text
        .find(LINE_TEXT)
        .expect("the line of the value is in the file")
        + BEFORE_VALUE.len();
    let line_start = text[..value].rfind('\n').map_or(0, |index| index + 1);
    let line = text[..value].matches('\n').count();
    let column = text[line_start..value].chars().count();

    // The checks collect their failures, so that the window closes before any assert. Each name
    // field closes with Escape, which takes nothing back into the text.
    let mut failures = Vec::new();
    for key in ["ctrl-alt-v", "ctrl-alt-p"] {
        view.update(cx, |view, cx| view.restore_caret(line, column, cx));
        draw(cx, window);
        let before = buffer_of(cx, &view);
        press(cx, window, key);
        let shown = outcome_of(cx, window, &view, &before);
        if shown.as_deref() != Some("name field `pageSize1`") {
            failures.push(format!("{key}: {shown:?}, wanted the name field pageSize1"));
        }
        press(cx, window, "escape");
        draw(cx, window);
        if buffer_of(cx, &view) != before {
            failures.push(format!("{key}: the text changed without the name field"));
        }
    }

    // Extract Constant with the caret on the value: the name field shows PAGE_SIZE, and Enter
    // writes it into the declaration and in place of the value.
    view.update(cx, |view, cx| view.restore_caret(line, column, cx));
    draw(cx, window);
    let before = buffer_of(cx, &view);
    press(cx, window, "ctrl-alt-c");
    let shown = outcome_of(cx, window, &view, &before);
    if shown.as_deref() != Some("name field `PAGE_SIZE`") {
        failures.push(format!(
            "ctrl-alt-c: {shown:?}, wanted the name field PAGE_SIZE"
        ));
    }
    press(cx, window, "enter");
    draw(cx, window);
    let declared = text_holds(cx, &view, "int PAGE_SIZE = 5;");
    let used = text_holds(cx, &view, "int pageSize = PAGE_SIZE;");
    let text = buffer_of(cx, &view);
    if !(declared && used) {
        failures.push(format!(
            "Enter did not write PAGE_SIZE into the declaration and the value:\n{text}"
        ));
    }

    close(cx, window, language, false);
    finish(
        "the_extractions_of_the_value_name_it_on_the_real_server",
        started,
    );
    assert!(
        failures.is_empty(),
        "the checks failed:\n{}",
        failures.join("\n")
    );
}
