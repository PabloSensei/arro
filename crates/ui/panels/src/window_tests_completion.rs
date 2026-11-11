//! The completion list: where it opens, how a row is split and in what order entries
//! come. The real server check needs `IDE_CORPUS` and a filled package cache:
//!
//! `cargo test -p panels window_tests_completion -- --ignored --test-threads=1`

use std::fs;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::StreamExt;
use lang_api::{CompletionEntry, DocumentSpot, TextPoint, ask};
use ui_kit::gpui;
use ui_kit::gpui::TestAppContext;

use crate::window_tests::{draw, project_setup, scratch};
use crate::window_tests_real::{build_window, close, project, wait_for, wait_ready};

#[ui_kit::gpui::test]
#[ignore = "needs IDE_CORPUS and a filled package cache"]
fn the_real_server_puts_string_first_for_a_short_prefix(cx: &mut TestAppContext) {
    let (root, language) = project("completion-order");
    let path = window_language::main_class(&root, "probe", "CompletionOrder");
    let source = r#"package probe;

public class CompletionOrder {

    void show() {
        Str
    }
}
"#;
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, source).unwrap();
    let window = build_window(cx, root.clone(), &language, project_setup(scratch("cache")));
    cx.run_until_parked();
    draw(cx, window);
    wait_ready(cx, window);
    language.open_document(&path).unwrap();

    let offset = source.find("Str").unwrap() + "Str".len();
    let before = &source[..offset];
    let line = before.bytes().filter(|byte| *byte == b'\n').count() as u32;
    let character = before.rsplit('\n').next().unwrap().chars().count() as u32;
    let uri = platform::CanonPath::new(&path).unwrap().to_uri();
    let support = language.support();
    let provider = support.completion().unwrap();
    let mut replies = ask(
        provider,
        DocumentSpot {
            uri,
            version: 1,
            point: TextPoint { line, character },
        },
    );
    let entries: Arc<Mutex<Option<Vec<CompletionEntry>>>> = Arc::new(Mutex::new(None));
    let slot = entries.clone();
    cx.spawn(async move |_| {
        if let Some(answer) = replies.next().await {
            *slot.lock().unwrap() = Some(answer.value.entries);
        }
    })
    .detach();
    wait_for(
        cx,
        "the completion of the types",
        Duration::from_secs(120),
        |_| entries.lock().unwrap().is_some(),
    );
    let entries = entries.lock().unwrap().clone().unwrap();
    let order = editor_view::completion_order(&entries, "Str");
    let first = entries[order[0]].label.clone();
    assert_eq!(
        first.split(" - ").next(),
        Some("String"),
        "String is the first entry, not {first}"
    );

    close(cx, window, language, false);
}
