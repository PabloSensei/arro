//! Snippets of the completion list: their fields, the keys that walk them, and the
//! postfix keys of the real server. The real checks need `IDE_CORPUS` and a filled package
//! cache:
//!
//! `cargo test -p panels window_tests_snippets -- --ignored --test-threads=1`

use std::fs;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use editor_view::{EditorView, Spot};
use futures_util::StreamExt;
use lang_api::{CompletionEntry, DocumentSpot, TextPoint, ask};
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use workspace::Workspace;

use crate::window_tests::{draw, editor_of, open_path, press, project_setup, scratch, type_text};
use crate::window_tests_complete::{
    CompleteLanguage, open_file as open_complete_file, open_window as open_complete_window,
};
use crate::window_tests_real::{build_window, project, wait_for, wait_ready};

fn snippet(label: &str, insert: &str) -> CompletionEntry {
    CompletionEntry {
        label: label.to_owned(),
        insert_text: Some(insert.to_owned()),
        snippet: true,
        ..CompletionEntry::default()
    }
}

fn point_in(text: &str, offset: usize) -> TextPoint {
    let before = &text[..offset];
    let line = before.bytes().filter(|byte| *byte == b'\n').count() as u32;
    let line_start = before.rfind('\n').map_or(0, |index| index + 1);
    let character = text[line_start..offset].encode_utf16().count() as u32;
    TextPoint { line, character }
}

fn completion_popup(cx: &mut TestAppContext, window: WindowHandle<Workspace>) -> bool {
    draw(cx, window);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds("completion-popup").is_some()
}

fn text_of(cx: &TestAppContext, view: &gpui::Entity<EditorView>) -> String {
    view.read_with(cx, |view, _| view.text().unwrap_or_default())
}

fn field(cx: &TestAppContext, view: &gpui::Entity<EditorView>) -> std::ops::Range<usize> {
    let (range, _) = view
        .read_with(cx, |view, _| view.selection())
        .expect("the editor has a selection");
    range
}

#[ui_kit::gpui::test]
fn snippet_fields_walk_with_tab_and_shift_tab_and_copies_follow(cx: &mut TestAppContext) {
    let root = crate::window_tests_lang::project("snippet-fields", &[("src/Loop.txt", "lo\n")]);
    let language = CompleteLanguage::new();
    language.set_entries(vec![snippet(
        "loop",
        "for (${1:int} ${2:i} = ${3:0}; $2 < ${4:args}.length; $2++) {\n\t$0\n}",
    )]);
    let window = open_complete_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    let (_, view) = open_complete_file(cx, window, &root, "src/Loop.txt", "lo\n", 0, 2);

    press(cx, window, "ctrl-space");
    draw(cx, window);
    press(cx, window, "enter");

    assert_eq!(
        text_of(cx, &view),
        "for (int i = 0; i < args.length; i++) {\n    \n}\n"
    );
    assert_eq!(field(cx, &view), 5..8, "the first field is selected");
    assert_eq!(&text_of(cx, &view)[field(cx, &view)], "int");

    // Typing replaces the selected field; Tab goes to the second one.
    type_text(cx, window, "var");
    press(cx, window, "tab");
    assert_eq!(&text_of(cx, &view)[field(cx, &view)], "i");

    // The copies of field 2 change together.
    type_text(cx, window, "index");
    assert_eq!(
        text_of(cx, &view),
        "for (var index = 0; index < args.length; index++) {\n    \n}\n"
    );

    // Shift+Tab goes back to the first field.
    press(cx, window, "shift-tab");
    assert_eq!(&text_of(cx, &view)[field(cx, &view)], "var");
    press(cx, window, "tab");

    press(cx, window, "tab");
    assert_eq!(&text_of(cx, &view)[field(cx, &view)], "0");
    type_text(cx, window, "10");
    press(cx, window, "tab");
    assert_eq!(&text_of(cx, &view)[field(cx, &view)], "args");
    type_text(cx, window, "items");

    // The last Tab ends the session at `$0`, inside the loop body.
    press(cx, window, "tab");
    type_text(cx, window, "body");
    assert_eq!(
        text_of(cx, &view),
        "for (var index = 10; index < items.length; index++) {\n    body\n}\n"
    );
}

#[ui_kit::gpui::test]
fn snippet_choices_escapes_and_variables_parse_and_one_undo_takes_it_back(cx: &mut TestAppContext) {
    let root = crate::window_tests_lang::project("snippet-grammar", &[("src/Text.txt", "g\n")]);
    let language = CompleteLanguage::new();
    let insert = concat!(
        r"${1|one,two|};${2:def};\$5;\};",
        "\\\\",
        r";$TM_FILENAME;${TM_USER:x}$0"
    );
    language.set_entries(vec![snippet("grammar", insert)]);
    let window = open_complete_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    let (_, view) = open_complete_file(cx, window, &root, "src/Text.txt", "g\n", 0, 1);

    press(cx, window, "ctrl-space");
    draw(cx, window);
    press(cx, window, "enter");

    assert_eq!(
        text_of(cx, &view),
        "one;def;$5;};\\;;\n",
        "the choice, the escapes and the empty variables"
    );
    assert_eq!(&text_of(cx, &view)[field(cx, &view)], "one");

    press(cx, window, "ctrl-z");
    assert_eq!(text_of(cx, &view), "g\n", "one undo takes the snippet back");
}

#[ui_kit::gpui::test]
fn escape_ends_the_snippet_session(cx: &mut TestAppContext) {
    let root = crate::window_tests_lang::project("snippet-escape", &[("src/Two.txt", "tw\n")]);
    let language = CompleteLanguage::new();
    language.set_entries(vec![snippet("two", "${1:first} ${2:second}$0")]);
    let window = open_complete_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    let (_, view) = open_complete_file(cx, window, &root, "src/Two.txt", "tw\n", 0, 2);

    press(cx, window, "ctrl-space");
    draw(cx, window);
    press(cx, window, "enter");
    assert_eq!(text_of(cx, &view), "first second\n");
    assert_eq!(&text_of(cx, &view)[field(cx, &view)], "first");

    press(cx, window, "escape");
    assert_eq!(
        &text_of(cx, &view)[field(cx, &view)],
        "first",
        "Escape keeps the field text and the caret"
    );

    // The session is over: Enter inserts a line break instead of walking the fields.
    press(cx, window, "enter");
    assert!(
        text_of(cx, &view).contains("\n second\n"),
        "Enter edits the text again: {:?}",
        text_of(cx, &view)
    );
}

#[ui_kit::gpui::test]
fn a_multiline_snippet_takes_the_indent_of_its_line(cx: &mut TestAppContext) {
    let source = "    \n";
    let root = crate::window_tests_lang::project("snippet-indent", &[("src/Block.txt", "    \n")]);
    let language = CompleteLanguage::new();
    language.set_entries(vec![snippet("block", "if (${1:true}) {\n\t$0\n}")]);
    let window = open_complete_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    let (_, view) = open_complete_file(cx, window, &root, "src/Block.txt", source, 0, 4);

    press(cx, window, "ctrl-space");
    draw(cx, window);
    press(cx, window, "enter");

    assert_eq!(text_of(cx, &view), "    if (true) {\n        \n    }\n");
    assert_eq!(&text_of(cx, &view)[field(cx, &view)], "true");

    press(cx, window, "tab");
    type_text(cx, window, "body");
    assert_eq!(text_of(cx, &view), "    if (true) {\n        body\n    }\n");
}

#[ui_kit::gpui::test]
#[ignore = "needs IDE_CORPUS and a filled package cache"]
fn the_real_server_offers_for_and_var_postfix_snippets(cx: &mut TestAppContext) {
    let (root, language) = project("snippets-postfix");
    let path = window_language::main_class(&root, "probe", "Postfix");
    let source = r#"package probe;

public class Postfix {

    void show(String[] args) {
        int name = 1;
        args.
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

    let offset = source.find("args.").unwrap() + "args.".len();
    let point = point_in(source, offset);
    let uri = platform::CanonPath::new(&path).unwrap().to_uri();
    let support = language.support();
    let provider = support.completion().unwrap();
    let mut replies = ask(
        provider,
        DocumentSpot {
            uri,
            version: 1,
            point,
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
        "the completion of the postfix keys",
        Duration::from_secs(120),
        |_| entries.lock().unwrap().is_some(),
    );
    let entries = entries.lock().unwrap().clone().unwrap();
    let find = |key: &str| {
        entries
            .iter()
            .find(|entry| entry.filter_text.as_deref() == Some(key) || entry.label == key)
            .cloned()
            .unwrap_or_else(|| panic!("no postfix {key} in {:?}", entries.len()))
    };
    let for_entry = find("for");
    assert!(for_entry.snippet, "the postfix key comes as a snippet");
    assert!(
        for_entry
            .insert_text
            .as_deref()
            .is_some_and(|text| text.contains("for (")),
        "the snippet is a loop: {:?}",
        for_entry.insert_text
    );
    let var_entry = find("var");
    assert!(var_entry.snippet, "the postfix key comes as a snippet");
    assert!(
        var_entry
            .insert_text
            .as_deref()
            .is_some_and(|text| text.contains('=')),
        "the snippet is a declaration: {:?}",
        var_entry.insert_text
    );

    // Enter on `args.for` puts the loop in with its field selected.
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    view.update(cx, |view, cx| {
        view.go_to(
            Spot::Line {
                line: point.line as usize,
                column: point.character as usize,
            },
            cx,
        )
    });
    cx.run_until_parked();
    press(cx, window, "ctrl-space");
    wait_for(
        cx,
        "the completion list of the postfix keys",
        Duration::from_secs(120),
        |cx| completion_popup(cx, window),
    );
    type_text(cx, window, "for");
    draw(cx, window);
    press(cx, window, "enter");
    let text = text_of(cx, &view);
    assert!(
        text.contains("for (String ") && text.contains(" : args) {"),
        "the postfix key expands to a loop: {text:?}"
    );
    assert!(!field(cx, &view).is_empty(), "the loop starts on its field");
    type_text(cx, window, "item");
    press(cx, window, "tab");
    type_text(cx, window, "body");
    let text = text_of(cx, &view);
    assert!(
        text.contains("for (String item : args) {") && text.contains("body\n"),
        "the field and the end of the session: {text:?}"
    );

    // Enter on `name.var` declares a variable and selects its name.
    let var_path = window_language::main_class(&root, "probe", "PostfixVar");
    let var_source = r#"package probe;

public class PostfixVar {

    void show() {
        int name = 1;
        name.
    }
}
"#;
    fs::write(&var_path, var_source).unwrap();
    let var_offset = var_source.find("name.").unwrap() + "name.".len();
    let var_point = point_in(var_source, var_offset);
    open_path(cx, window, &var_path);
    let var_view = editor_of(cx, window, &var_path);
    var_view.update(cx, |view, cx| {
        view.go_to(
            Spot::Line {
                line: var_point.line as usize,
                column: var_point.character as usize,
            },
            cx,
        )
    });
    cx.run_until_parked();
    press(cx, window, "ctrl-space");
    wait_for(
        cx,
        "the completion list of the variable",
        Duration::from_secs(120),
        |cx| completion_popup(cx, window),
    );
    type_text(cx, window, "var");
    draw(cx, window);
    press(cx, window, "enter");
    assert!(
        !field(cx, &var_view).is_empty(),
        "the declaration starts on its field"
    );
    type_text(cx, window, "count");
    press(cx, window, "tab");
    let text = text_of(cx, &var_view);
    assert!(
        text.contains("int count = name;"),
        "the postfix key declares the variable: {text:?}"
    );

    crate::window_tests_real::close(cx, window, language, false);
}
