//! The indentation of an open file: the `.editorconfig` files around it, the indentation
//! of its own lines, the language unit, and what the status bar and a save do with it.

use std::fs;
use std::path::PathBuf;

use ui_kit::gpui;
use ui_kit::gpui::TestAppContext;
use workspace::IndentChoice;

use crate::window_tests::{
    buffer_of, click, draw, editor_of, open_path, open_window, press, project_setup, scratch,
    type_text, wait_for_signal,
};

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

fn status_indent(
    cx: &TestAppContext,
    window: ui_kit::gpui::WindowHandle<workspace::Workspace>,
) -> String {
    window
        .read_with(cx, |workspace, cx| workspace.editor_status(cx))
        .unwrap()
        .unwrap()
        .indent
}

#[gpui::test]
fn editorconfig_gives_tabs_to_text_and_two_spaces_to_markdown(cx: &mut TestAppContext) {
    let root = project(
        "indent-editorconfig",
        &[
            (
                ".editorconfig",
                "root = true\n\
                 [*]\n\
                 charset = utf-8\n\
                 end_of_line = lf\n\
                 insert_final_newline = true\n\
                 indent_style = space\n\
                 \n\
                 [*.txt]\n\
                 indent_style = tab\n\
                 indent_size = 4\n\
                 tab_width = 4\n\
                 \n\
                 [*.md]\n\
                 indent_style = space\n\
                 indent_size = 2\n",
            ),
            ("src/main.txt", "alpha {\n"),
            ("src/notes.md", "alpha\n"),
        ],
    );
    let window = open_window(cx, root.clone());
    let text = root.join("src").join("main.txt");
    open_path(cx, window, &text);
    let view = editor_of(cx, window, &text);
    assert_eq!(status_indent(cx, window), "Tab (EditorConfig)");

    press(cx, window, "end");
    press(cx, window, "enter");
    assert_eq!(
        buffer_of(cx, &view),
        "alpha {\n\t\n",
        "Enter adds the tab of the file"
    );

    press(cx, window, "home");
    press(cx, window, "tab");
    assert_eq!(buffer_of(cx, &view), "alpha {\n\t\t\n", "Tab adds a tab");

    press(cx, window, "shift-tab");
    assert_eq!(
        buffer_of(cx, &view),
        "alpha {\n\t\n",
        "Shift+Tab takes one tab away"
    );

    let markdown = root.join("src").join("notes.md");
    open_path(cx, window, &markdown);
    let view = editor_of(cx, window, &markdown);
    assert_eq!(status_indent(cx, window), "2 spaces (EditorConfig)");
    press(cx, window, "tab");
    assert_eq!(
        buffer_of(cx, &view),
        "  alpha\n",
        "Tab adds two spaces to Markdown"
    );
}

#[gpui::test]
fn without_editorconfig_the_lines_of_the_file_name_their_indent(cx: &mut TestAppContext) {
    let root = project(
        "indent-content",
        &[
            ("src/tabbed.txt", "if (a) {\n\tfirst\n\tsecond\n}\n"),
            ("src/spaced.txt", "if (a) {\n  first\n  second\n}\n"),
        ],
    );
    let window = open_window(cx, root.clone());

    let tabbed = root.join("src").join("tabbed.txt");
    open_path(cx, window, &tabbed);
    let view = editor_of(cx, window, &tabbed);
    assert_eq!(status_indent(cx, window), "Tab");
    press(cx, window, "home");
    press(cx, window, "tab");
    assert_eq!(buffer_of(cx, &view), "\tif (a) {\n\tfirst\n\tsecond\n}\n");

    let spaced = root.join("src").join("spaced.txt");
    open_path(cx, window, &spaced);
    let view = editor_of(cx, window, &spaced);
    assert_eq!(status_indent(cx, window), "2 spaces");
    view.update(cx, |view, cx| view.restore_caret(0, 8, cx));
    press(cx, window, "enter");
    assert_eq!(
        buffer_of(cx, &view),
        "if (a) {\n  \n  first\n  second\n}\n",
        "the new line takes two spaces"
    );
}

#[gpui::test]
fn a_pasted_block_takes_the_indent_of_the_line_and_keeps_its_shape(cx: &mut TestAppContext) {
    let root = project(
        "indent-paste",
        &[
            (
                ".editorconfig",
                "root = true\n[*.txt]\nindent_style = space\nindent_size = 2\n",
            ),
            ("src/main.txt", "  alpha\n"),
        ],
    );
    let window = open_window(cx, root.clone());
    let path = root.join("src").join("main.txt");
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    view.update(cx, |view, cx| view.restore_caret(0, 7, cx));
    cx.update(|cx| {
        cx.write_to_clipboard(ui_kit::ClipboardItem::new_string("one\n  two\n".to_owned()));
    });
    press(cx, window, "ctrl-v");
    assert_eq!(
        buffer_of(cx, &view),
        "  alphaone\n    two\n\n",
        "the second line keeps its own two spaces above the two of the line"
    );
}

#[gpui::test]
fn a_save_adds_the_final_newline_and_trims_trailing_spaces(cx: &mut TestAppContext) {
    let root = project(
        "indent-save",
        &[
            (
                ".editorconfig",
                "root = true\n\
                 [*]\n\
                 charset = utf-8\n\
                 insert_final_newline = true\n\
                 trim_trailing_whitespace = true\n",
            ),
            ("src/main.txt", "alpha   \nbeta   "),
        ],
    );
    let window = open_window(cx, root.clone());
    let path = root.join("src").join("main.txt");
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    view.update(cx, |view, cx| view.restore_caret(1, 7, cx));
    type_text(cx, window, "!");
    press(cx, window, "ctrl-s");
    wait_for_signal(cx, "the save of the file", |_| {
        fs::read_to_string(&path).is_ok_and(|text| text == "alpha\nbeta   !\n")
    });
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "alpha\nbeta   !\n",
        "the trailing spaces of the first line are gone and the final newline appears"
    );
}

#[gpui::test]
fn the_status_menu_changes_the_indent_of_the_open_file(cx: &mut TestAppContext) {
    let root = project(
        "indent-menu",
        &[
            (
                ".editorconfig",
                "root = true\n[*.txt]\nindent_style = space\nindent_size = 4\n",
            ),
            ("src/main.txt", "alpha\n"),
        ],
    );
    let window = open_window(cx, root.clone());
    let path = root.join("src").join("main.txt");
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    assert_eq!(status_indent(cx, window), "4 spaces (EditorConfig)");

    window
        .update(cx, |workspace, _, cx| {
            workspace.set_active_indent(IndentChoice::Size(2), cx)
        })
        .unwrap();
    assert_eq!(status_indent(cx, window), "2 spaces");
    press(cx, window, "tab");
    assert_eq!(buffer_of(cx, &view), "  alpha\n");

    click(cx, window, "st-indent");
    draw(cx, window);
    press(cx, window, "escape");
}

// The test below needs `IDE_CORPUS` and a filled package cache:
//
// `cargo test -p panels window_tests_indent -- --ignored --test-threads=1`

#[gpui::test]
#[ignore = "needs IDE_CORPUS and a filled package cache"]
fn reformat_file_of_petclinic_uses_the_tabs_of_its_editorconfig(cx: &mut TestAppContext) {
    use std::time::Duration;

    let source = window_language::corpus_dir().join("src/petclinic");
    let root = scratch("indent-real").join("petclinic");
    window_language::prepare_project(&source, &root);
    assert!(
        root.join(".editorconfig").is_file(),
        "the copy keeps the editorconfig of PetClinic"
    );
    let sample = window_language::write_vcs_format_sample(&root);
    let language = window_language::RealLanguage::open(root.clone(), scratch("indent-real-state"))
        .expect("the language server starts");
    let window = crate::window_tests_real::build_window(
        cx,
        root.clone(),
        &language,
        project_setup(scratch("indent-real-cache")),
    );
    cx.run_until_parked();
    draw(cx, window);
    crate::window_tests_real::wait_ready(cx, window);
    open_path(cx, window, &sample);
    let view = editor_of(cx, window, &sample);
    crate::window_tests_real::wait_for(cx, "the index of the file", Duration::from_secs(120), {
        let view = view.clone();
        move |cx| view.read_with(cx, |view, _| view.has_syntax_tree())
    });
    let before = buffer_of(cx, &view);
    assert!(
        before.contains("\n    public int first()"),
        "the sample starts with spaces: {before}"
    );

    press(cx, window, "ctrl-alt-l");
    crate::window_tests_real::wait_for(cx, "the formatting", Duration::from_secs(120), |cx| {
        buffer_of(cx, &view) != before
    });
    let after = buffer_of(cx, &view);
    assert!(
        after.contains("\n\tpublic int first()"),
        "Reformat Code takes the tabs of .editorconfig: {after}"
    );
    assert!(
        !after.contains("\n    public int first()"),
        "the spaces of the sample are gone: {after}"
    );

    crate::window_tests_real::close(cx, window, language, false);
}
