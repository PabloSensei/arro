//! Actions on Save with the real language server: Ctrl+S formats a source file and takes
//! its unused import away before the text reaches the disk.
//!
//! The module is not part of a normal test run:
//!
//! `cargo test -p panels window_tests_real_actions_on_save -- --ignored --test-threads=1`

use std::time::{Duration, Instant};

use ui_kit::gpui;
use ui_kit::gpui::TestAppContext;
use workspace::Settings;

use crate::window_tests::{buffer_of, draw, editor_of, open_path, press, project_setup, scratch};
use crate::window_tests_real::{build_window, close, finish, project, wait_for, wait_ready};

const LIMIT: Duration = Duration::from_secs(120);

#[gpui::test]
#[ignore = "needs IDE_CORPUS and a filled package cache"]
fn save_formats_the_file_and_organizes_the_imports(cx: &mut TestAppContext) {
    let started = Instant::now();
    let (root, language) = project("real-save-actions");
    let (sample, _) = window_language::write_refactor_samples(&root);
    let window = build_window(cx, root, &language, project_setup(scratch("cache")));
    cx.run_until_parked();
    draw(cx, window);
    wait_ready(cx, window);

    cx.update(|cx| {
        let mut settings = cx.global::<Settings>().clone();
        settings.actions_on_save_reformat = true;
        settings.actions_on_save_optimize_imports = true;
        cx.set_global(settings);
    });
    open_path(cx, window, &sample);
    let view = editor_of(cx, window, &sample);
    assert!(
        window_language::has_unused_import(&buffer_of(cx, &view)),
        "the sample starts with the unused import"
    );

    // A dirty file is what Ctrl+S saves; the extra space is what the formatter takes away.
    let text = buffer_of(cx, &view);
    let from = text.find("int sum = a + b;").expect("the sample has a sum");
    view.update(cx, |view, cx| {
        view.replace_text(
            from..from + "int sum = a + b;".len(),
            "int  sum = a + b;",
            cx,
        )
    });
    press(cx, window, "ctrl-s");
    wait_for(cx, "the actions on save", LIMIT, |cx| {
        let text = buffer_of(cx, &view);
        window_language::unused_import_is_gone(&text)
            && !text.contains("int  sum")
            && std::fs::read_to_string(&sample).is_ok_and(|disk| disk == text)
    });

    close(cx, window, language, false);
    finish("save_formats_the_file_and_organizes_the_imports", started);
}
