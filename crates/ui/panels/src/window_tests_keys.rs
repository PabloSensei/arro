//! Every editor key of the IDEA keymap, pressed in an open editor without a window.
//! The groups follow the kinds of the actions: moves and selections, edits, folds and
//! bookmarks, search, the language of a server, and the save.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use editor_view::{EditorView, FoldStyle, LanguageConfig, Languages, Semantic, StatementRules};
use lang_api::{
    ActionEntry, ActionKind, Answer, Confidence, DocumentEdit, DocumentSpot, HierarchyProvider,
    HierarchyQuery, HoverProvider, HoverText, LanguageId, LanguageSupport, Provider,
    SignatureProvider, SignatureQuery, SignatureText, SourceId, SourceSpan, SymbolEntry,
    SyntaxProvider, TextPoint, TextSpan,
};
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use ui_kit::{Theme, px};
use workspace::{Settings, USAGES_TOOL, Workspace};

use crate::window_tests::{
    active_path, bounds_of, buffer_of, draw, editor_of, open_path, open_window, press,
    sample_project, type_text,
};
use crate::window_tests_lang::{FakeLanguage, open_window_with, project};
use crate::window_tests_lang::{edit_in, uri_of};

const LIMIT: Duration = Duration::from_secs(20);

fn wait_for(
    cx: &mut TestAppContext,
    what: &str,
    mut check: impl FnMut(&mut TestAppContext) -> bool,
) {
    let started = Instant::now();
    loop {
        if check(cx) {
            return;
        }
        assert!(started.elapsed() < LIMIT, "waited for {what} in vain");
        std::thread::sleep(Duration::from_millis(10));
        cx.executor().advance_clock(Duration::from_millis(50));
        cx.run_until_parked();
    }
}

/// The folds, the statements and the comments of the plain text files of these tests.
static FOLD_STYLES: &[FoldStyle] = &[FoldStyle {
    capture: "fold",
    placeholder: "\u{2026}",
    keep: 0,
    joined: false,
    collapsed_on_open: false,
}];

static STATEMENTS: StatementRules = StatementRules {
    terminator: ";",
    block_words: &["if", "for", "while", "class"],
};

fn sample_language(path: &Path) -> Option<LanguageConfig> {
    let extension = path.extension()?.to_str()?;
    if !extension.eq_ignore_ascii_case("txt") {
        return None;
    }
    let mut config = dockerfile_syntax::config();
    config.name = "Sample";
    config.folds = "(source_file) @fold";
    config.fold_styles = FOLD_STYLES;
    config.statements = Some(STATEMENTS);
    config.line_comment = Some("#");
    config.block_comment = Some(("/*", "*/"));
    config.indent_unit = "    ";
    Some(config)
}

fn use_sample_language(cx: &mut TestAppContext) {
    cx.update(|cx| {
        cx.set_global(Languages {
            resolve: sample_language,
            templates: |_| None,
        });
    });
}

/// A window and an open text file of its own.
fn open_text(
    cx: &mut TestAppContext,
    name: &str,
    text: &str,
) -> (PathBuf, WindowHandle<Workspace>, gpui::Entity<EditorView>) {
    let root = sample_project(name);
    let path = root.join("src").join("main.txt");
    fs::write(&path, text).unwrap();
    let window = open_window(cx, root);
    use_sample_language(cx);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    (path, window, view)
}

/// Replaces the whole text and puts the caret, so one window serves a group of actions.
fn reset(
    cx: &mut TestAppContext,
    view: &gpui::Entity<EditorView>,
    text: &str,
    line: usize,
    column: usize,
) {
    let length = buffer_of(cx, view).len();
    view.update(cx, |view, cx| {
        view.replace_text(0..length, text, cx);
        view.restore_caret(line, column, cx);
    });
}

fn selection(cx: &TestAppContext, view: &gpui::Entity<EditorView>) -> std::ops::Range<usize> {
    view.read_with(cx, |view, _| view.selection())
        .expect("the file is read")
        .0
}

#[ui_kit::gpui::test]
fn moves_go_by_character_word_line_file_and_page(cx: &mut TestAppContext) {
    let (_path, window, view) = open_text(cx, "moves", "one two\n  three four\nfive six\n");

    press(cx, window, "right");
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (0, 1));
    press(cx, window, "left");
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (0, 0));

    // Ctrl+Right ends the current word, then moves to the end of the next one.
    press(cx, window, "ctrl-right");
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (0, 3));
    press(cx, window, "ctrl-right");
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (0, 7));
    press(cx, window, "ctrl-left");
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (0, 4));

    // End is the line end; Home goes to the first non-blank, then to column zero.
    press(cx, window, "end");
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (0, 7));
    press(cx, window, "home");
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (0, 0));
    press(cx, window, "down");
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (1, 0));
    press(cx, window, "home");
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (1, 2));
    press(cx, window, "home");
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (1, 0));

    // Up and Down keep the goal column.
    press(cx, window, "right");
    press(cx, window, "right");
    press(cx, window, "down");
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (2, 2));
    press(cx, window, "up");
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (1, 2));

    // Ctrl+Home and Ctrl+End are the file bounds.
    press(cx, window, "ctrl-end");
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (3, 0));
    press(cx, window, "ctrl-home");
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (0, 0));

    // A page is a screenful; with a short text it reaches the last line and back.
    press(cx, window, "pagedown");
    assert!(view.read_with(cx, |view, _| view.caret().0) > 0);
    press(cx, window, "pageup");
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (0, 0));
}

#[ui_kit::gpui::test]
fn selections_go_by_character_word_line_and_file(cx: &mut TestAppContext) {
    let (_path, window, view) = open_text(cx, "selections", "one two\n  three four\nfive six\n");
    let length = buffer_of(cx, &view).len();

    press(cx, window, "shift-right");
    assert_eq!(selection(cx, &view), 0..1);
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (0, 1));

    reset(cx, &view, "one two\n  three four\nfive six\n", 0, 0);
    press(cx, window, "shift-down");
    assert_eq!(selection(cx, &view), 0..8);
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (1, 0));

    reset(cx, &view, "one two\n  three four\nfive six\n", 0, 0);
    press(cx, window, "ctrl-shift-right");
    assert_eq!(selection(cx, &view), 0..3);
    press(cx, window, "ctrl-shift-right");
    assert_eq!(selection(cx, &view), 0..7);
    reset(cx, &view, "one two\n  three four\nfive six\n", 0, 7);
    press(cx, window, "ctrl-shift-left");
    assert_eq!(selection(cx, &view), 4..7);

    reset(cx, &view, "one two\n  three four\nfive six\n", 0, 0);
    press(cx, window, "shift-end");
    assert_eq!(selection(cx, &view), 0..7);
    // Shift+Up on the first line takes the selection back to the file start.
    press(cx, window, "shift-up");
    assert_eq!(selection(cx, &view), 0..0);

    // Shift+Home from the line end takes the line back.
    reset(cx, &view, "one two\n  three four\nfive six\n", 0, 7);
    press(cx, window, "shift-home");
    assert_eq!(selection(cx, &view), 0..7);
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (0, 0));

    // Shift+Ctrl+End and Shift+Ctrl+Home are the whole file from either end.
    reset(cx, &view, "one two\n  three four\nfive six\n", 0, 0);
    press(cx, window, "ctrl-shift-end");
    assert_eq!(selection(cx, &view), 0..length);
    reset(cx, &view, "one two\n  three four\nfive six\n", 3, 0);
    press(cx, window, "ctrl-shift-home");
    assert_eq!(selection(cx, &view), 0..length);

    reset(cx, &view, "one two\n  three four\nfive six\n", 1, 2);
    press(cx, window, "ctrl-a");
    assert_eq!(selection(cx, &view), 0..length);
}

#[ui_kit::gpui::test]
fn delete_word_join_lines_case_and_start_line_edit_the_text(cx: &mut TestAppContext) {
    let (_path, window, view) = open_text(cx, "delete-words", "alpha beta\n");

    reset(cx, &view, "alpha beta\n", 0, 5);
    press(cx, window, "ctrl-backspace");
    assert_eq!(buffer_of(cx, &view), " beta\n");
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (0, 0));

    reset(cx, &view, "alpha beta\n", 0, 0);
    press(cx, window, "ctrl-delete");
    assert_eq!(buffer_of(cx, &view), " beta\n");

    reset(cx, &view, "alpha beta\n    gamma\n", 0, 0);
    press(cx, window, "ctrl-shift-j");
    assert_eq!(buffer_of(cx, &view), "alpha beta gamma\n");

    reset(cx, &view, "alpha beta\n", 0, 1);
    press(cx, window, "ctrl-shift-u");
    assert_eq!(buffer_of(cx, &view), "ALPHA beta\n");
    press(cx, window, "ctrl-shift-u");
    assert_eq!(buffer_of(cx, &view), "alpha beta\n");

    reset(cx, &view, "abc\n", 0, 1);
    press(cx, window, "shift-enter");
    assert_eq!(buffer_of(cx, &view), "abc\n\n");
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (1, 0));

    reset(cx, &view, "abc\n", 0, 1);
    press(cx, window, "ctrl-alt-enter");
    assert_eq!(buffer_of(cx, &view), "\nabc\n");
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (0, 0));
}

#[ui_kit::gpui::test]
fn indent_and_block_comment_keys_edit_the_text(cx: &mut TestAppContext) {
    let (_path, window, view) = open_text(cx, "indent", "value\n");

    reset(cx, &view, "    abc\n", 0, 4);
    press(cx, window, "shift-tab");
    assert_eq!(buffer_of(cx, &view), "abc\n");

    reset(cx, &view, "  one\n        two\n", 1, 8);
    press(cx, window, "ctrl-alt-i");
    assert_eq!(buffer_of(cx, &view), "  one\n  two\n");

    reset(cx, &view, "value\n", 0, 0);
    press(cx, window, "ctrl-shift-/");
    assert_eq!(buffer_of(cx, &view), "/*value*/\n");
    press(cx, window, "ctrl-shift-/");
    assert_eq!(buffer_of(cx, &view), "value\n");

    // The line comment key takes the whole line, as the Dockerfile test does.
    reset(cx, &view, "value\n", 0, 0);
    press(cx, window, "ctrl-/");
    assert_eq!(buffer_of(cx, &view), "#value\n");
}

#[ui_kit::gpui::test]
fn plain_enter_tab_backspace_and_delete_edit_the_text(cx: &mut TestAppContext) {
    let (_path, window, view) = open_text(cx, "plain-keys", "abc");

    reset(cx, &view, "abc", 0, 1);
    press(cx, window, "enter");
    assert_eq!(buffer_of(cx, &view), "a\nbc");
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (1, 0));

    reset(cx, &view, "abc", 0, 0);
    press(cx, window, "tab");
    assert_eq!(buffer_of(cx, &view), "    abc");
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (0, 4));

    reset(cx, &view, "abc", 0, 3);
    press(cx, window, "backspace");
    assert_eq!(buffer_of(cx, &view), "ab");

    reset(cx, &view, "abc", 0, 0);
    press(cx, window, "delete");
    assert_eq!(buffer_of(cx, &view), "bc");
}

#[ui_kit::gpui::test]
fn match_bracket_and_last_edit_move_the_caret(cx: &mut TestAppContext) {
    let (_path, window, view) = open_text(cx, "bracket", "if (a) {\n}\n");

    view.update(cx, |view, cx| view.restore_caret(0, 3, cx));
    press(cx, window, "ctrl-shift-m");
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (0, 6));
    press(cx, window, "ctrl-shift-m");
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (0, 3));

    // Ctrl+Shift+Backspace returns to the place of the latest edit.
    reset(cx, &view, "abc\n", 0, 0);
    type_text(cx, window, "X");
    press(cx, window, "ctrl-end");
    press(cx, window, "ctrl-shift-backspace");
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (0, 1));
}

fn action(
    title: &str,
    kind: ActionKind,
    code: &str,
    uri: &str,
    edits: Vec<DocumentEdit>,
) -> ActionEntry {
    ActionEntry {
        title: title.to_owned(),
        kind: Some(kind),
        code: Some(code.to_owned()),
        preferred: true,
        edits: vec![lang_api::SourceEdit {
            uri: uri.to_owned(),
            edits,
        }],
        renames: Vec::new(),
        handle: None,
    }
}

#[ui_kit::gpui::test]
fn refactoring_keys_apply_field_constant_parameter_and_inline(cx: &mut TestAppContext) {
    let root = project("keys-refactor", &[("src/Refactor.txt", "value\n")]);
    let path = root.join("src").join("Refactor.txt");
    let uri = uri_of(&path);
    let language = FakeLanguage::new();
    language.set_actions(vec![
        action(
            "Extract field",
            ActionKind::Refactor,
            "refactor.extract.field",
            &uri,
            vec![edit_in("value\n", 0, 5, "field")],
        ),
        action(
            "Extract constant",
            ActionKind::Refactor,
            "refactor.extract.constant",
            &uri,
            vec![edit_in("value\n", 0, 5, "CONST")],
        ),
        action(
            "Introduce parameter",
            ActionKind::Refactor,
            "refactor.introduce.parameter",
            &uri,
            vec![edit_in("value\n", 0, 5, "param")],
        ),
        action(
            "Inline",
            ActionKind::Refactor,
            "refactor.inline",
            &uri,
            vec![edit_in("value\n", 0, 5, "inlined")],
        ),
    ]);
    let window = open_window_with(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);

    for (keys, expected) in [
        ("ctrl-alt-f", "field\n"),
        ("ctrl-alt-c", "CONST\n"),
        ("ctrl-alt-p", "param\n"),
        ("ctrl-alt-n", "inlined\n"),
    ] {
        reset(cx, &view, "value\n", 0, 0);
        press(cx, window, keys);
        assert_eq!(buffer_of(cx, &view), expected, "{keys}");
    }
}

#[ui_kit::gpui::test]
fn usage_keys_open_the_usages_window(cx: &mut TestAppContext) {
    let root = project(
        "keys-usages",
        &[("src/One.txt", "greet\n"), ("src/Two.txt", "greet\n")],
    );
    let one = root.join("src").join("One.txt");
    let two = root.join("src").join("Two.txt");
    let language = FakeLanguage::new();
    language.set_references(vec![
        SourceSpan {
            uri: uri_of(&one),
            span: TextSpan::default(),
        },
        SourceSpan {
            uri: uri_of(&two),
            span: TextSpan::default(),
        },
    ]);
    let window = open_window_with(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &one);

    press(cx, window, "alt-f7");
    assert!(
        window
            .read_with(cx, |workspace, _| workspace.tool_open(USAGES_TOOL))
            .unwrap(),
        "Alt+F7 shows the usages"
    );
}

#[ui_kit::gpui::test]
fn generate_lists_the_source_actions_and_applies_one(cx: &mut TestAppContext) {
    let root = project("keys-generate", &[("src/Type.txt", "value\n")]);
    let path = root.join("src").join("Type.txt");
    let uri = uri_of(&path);
    let language = FakeLanguage::new();
    language.set_actions(vec![action(
        "Generate getter",
        ActionKind::Source,
        "source.generate.getter",
        &uri,
        vec![edit_in("value\n", 0, 5, "getter()")],
    )]);
    let window = open_window_with(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);

    press(cx, window, "alt-insert");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "actions-popup").size.height > px(0.),
        "Alt+Insert lists what can be generated"
    );
    press(cx, window, "enter");
    assert_eq!(buffer_of(cx, &view), "getter()\n");
}

#[ui_kit::gpui::test]
fn goto_implementation_and_super_method_open_the_definition(cx: &mut TestAppContext) {
    let root = project(
        "keys-navigation",
        &[("src/Main.txt", "call\n"), ("src/Other.txt", "place\n")],
    );
    let main = root.join("src").join("Main.txt");
    let other = root.join("src").join("Other.txt");
    let language = FakeLanguage::new();
    language.set_definition(&other, 0, 2);
    let window = open_window_with(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &main);

    press(cx, window, "ctrl-alt-b");
    draw(cx, window);
    assert_eq!(active_path(cx, window), Some(other.clone()));
    assert_eq!(
        editor_of(cx, window, &other).read_with(cx, |view, _| view.caret()),
        (0, 2)
    );

    open_path(cx, window, &main);
    press(cx, window, "ctrl-u");
    draw(cx, window);
    assert_eq!(active_path(cx, window), Some(other));
}

#[ui_kit::gpui::test]
fn error_description_shows_the_message_of_the_mark(cx: &mut TestAppContext) {
    let root = project("keys-error", &[("src/Problem.txt", "one\ntwo\n")]);
    let path = root.join("src").join("Problem.txt");
    let language = FakeLanguage::new();
    language.set_diagnostics(vec![lang_api::DiagnosticEntry {
        span: TextSpan {
            start: TextPoint {
                line: 1,
                character: 0,
            },
            end: TextPoint {
                line: 1,
                character: 3,
            },
        },
        severity: lang_api::Severity::Error,
        message: "first error".to_owned(),
        unnecessary: false,
    }]);
    let window = open_window_with(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    draw(cx, window);
    let view = editor_of(cx, window, &path);

    view.update(cx, |view, cx| view.restore_caret(1, 1, cx));
    press(cx, window, "ctrl-f1");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "lsp-note").size.height > px(0.),
        "Ctrl+F1 shows the message of the error at the caret"
    );
}

/// Hover, signatures and the hierarchy on top of the fake language, so that the three
/// notes of the editor can be checked without a server.
struct Described {
    inner: Arc<FakeLanguage>,
}

const SOURCE: SourceId = SourceId::new("fake-server");

fn answer<T>(value: T) -> Answer<T> {
    Answer::new(value, Confidence::Certain, SOURCE)
}

impl Provider<DocumentSpot, HoverText> for Described {
    fn start(&self, _: DocumentSpot, _: base::Cancel, mut reply: lang_api::Reply<HoverText>) {
        let _ = reply.send(answer(HoverText(
            "String value\nthe documentation".to_owned(),
        )));
    }
}

impl Provider<SignatureQuery, SignatureText> for Described {
    fn start(&self, _: SignatureQuery, _: base::Cancel, mut reply: lang_api::Reply<SignatureText>) {
        let _ = reply.send(answer(SignatureText("(int a, int b)".to_owned())));
    }
}

impl Provider<HierarchyQuery, Vec<SymbolEntry>> for Described {
    fn start(
        &self,
        _: HierarchyQuery,
        _: base::Cancel,
        mut reply: lang_api::Reply<Vec<SymbolEntry>>,
    ) {
        let place = SourceSpan {
            uri: "file:///hierarchy.txt".to_owned(),
            span: TextSpan::default(),
        };
        let entry = |name: &str| SymbolEntry {
            name: name.to_owned(),
            container: None,
            place: Some(place.clone()),
        };
        let _ = reply.send(answer(vec![entry("Base"), entry("Child")]));
    }
}

impl LanguageSupport for Described {
    fn id(&self) -> LanguageId {
        self.inner.id()
    }

    fn matches(&self, path: &Path) -> bool {
        self.inner.matches(path)
    }

    fn syntax(&self) -> &dyn SyntaxProvider {
        self.inner.syntax()
    }

    fn hover(&self) -> Option<&HoverProvider> {
        Some(self)
    }

    fn signatures(&self) -> Option<&SignatureProvider> {
        Some(self)
    }

    fn hierarchy(&self) -> Option<&HierarchyProvider> {
        Some(self)
    }
}

#[ui_kit::gpui::test]
fn doc_signature_type_and_hierarchy_keys_show_notes_and_lists(cx: &mut TestAppContext) {
    let root = sample_project("keys-described");
    let path = root.join("src").join("main.txt");
    fs::write(&path, "value\n").unwrap();
    let window = open_window(cx, root);
    let support: Arc<dyn LanguageSupport> = Arc::new(Described {
        inner: FakeLanguage::new(),
    });
    cx.update(|cx| {
        cx.set_global(Theme::dark());
        cx.set_global(Settings::default());
        cx.set_global(Semantic(support));
    });
    open_path(cx, window, &path);

    press(cx, window, "ctrl-q");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "quick-doc").size.height > px(0.),
        "Ctrl+Q shows the documentation"
    );

    press(cx, window, "ctrl-p");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "signature-popup").size.height > px(0.),
        "Ctrl+P shows the signature"
    );

    press(cx, window, "ctrl-shift-p");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "lsp-note").size.height > px(0.),
        "Ctrl+Shift+P shows the type"
    );

    press(cx, window, "ctrl-h");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "actions-popup").size.height > px(0.),
        "Ctrl+H lists the type hierarchy"
    );
    press(cx, window, "escape");
}

#[ui_kit::gpui::test]
fn find_next_and_previous_step_through_the_matches(cx: &mut TestAppContext) {
    let (_path, window, view) = open_text(cx, "find-next", "one two one two one\n");

    press(cx, window, "ctrl-f");
    type_text(cx, window, "one");
    press(cx, window, "escape");
    assert_eq!(selection(cx, &view), 0..3);

    press(cx, window, "f3");
    assert_eq!(selection(cx, &view), 8..11);
    press(cx, window, "f3");
    assert_eq!(selection(cx, &view), 16..19);
    press(cx, window, "shift-f3");
    assert_eq!(selection(cx, &view), 8..11);
    press(cx, window, "shift-f3");
    assert_eq!(selection(cx, &view), 0..3);

    // The other keys of the same actions step in the same way.
    press(cx, window, "ctrl-l");
    assert_eq!(selection(cx, &view), 8..11);
    press(cx, window, "ctrl-shift-l");
    assert_eq!(selection(cx, &view), 0..3);
}

#[ui_kit::gpui::test]
fn folds_all_selection_and_numbered_bookmarks(cx: &mut TestAppContext) {
    let text: String = (0..12).map(|line| format!("line{line}\n")).collect();
    let (_path, window, view) = open_text(cx, "folds-bookmarks", &text);
    let rows = view.read_with(cx, |view, _| view.display_rows());

    // Ctrl+. folds the selected lines; Ctrl+Shift+= opens every fold again.
    view.update(cx, |view, cx| view.restore_caret(0, 0, cx));
    press(cx, window, "shift-down shift-down");
    press(cx, window, "ctrl-.");
    assert!(
        view.read_with(cx, |view, _| view.display_rows()) < rows,
        "the selected lines fold"
    );
    press(cx, window, "ctrl-shift-=");
    assert_eq!(view.read_with(cx, |view, _| view.display_rows()), rows);

    // Ctrl+Shift+- folds every region of the syntax; the parse arrives in the background.
    wait_for(cx, "the syntax of the folds", |cx| {
        press(cx, window, "ctrl-shift--");
        view.read_with(cx, |view, _| view.display_rows()) < rows
    });
    press(cx, window, "ctrl-shift-=");
    assert_eq!(view.read_with(cx, |view, _| view.display_rows()), rows);

    // Ctrl+Shift+digit marks a line and Ctrl+digit goes back to it.
    for digit in 0..=9u32 {
        let line = 2 + digit as usize;
        view.update(cx, |view, cx| view.restore_caret(line, 0, cx));
        press(cx, window, &format!("ctrl-shift-{digit}"));
        view.update(cx, |view, cx| view.restore_caret(11, 0, cx));
        press(cx, window, &format!("ctrl-{digit}"));
        assert_eq!(
            view.read_with(cx, |view, _| view.caret().0),
            line,
            "bookmark {digit}"
        );
        press(cx, window, &format!("ctrl-shift-{digit}"));
    }

    // Ctrl+F11 takes the next typed letter as the name of the bookmark.
    view.update(cx, |view, cx| view.restore_caret(1, 0, cx));
    press(cx, window, "ctrl-f11");
    press(cx, window, "m");
    let marks = cx.update(|cx| editor_view::bookmarks(cx));
    assert!(
        marks
            .iter()
            .any(|entry| entry.line == 1 && entry.mnemonic == Some('M')),
        "{marks:?}"
    );
    press(cx, window, "ctrl-f11");
    press(cx, window, "m");
    assert!(
        cx.update(|cx| editor_view::bookmarks(cx))
            .iter()
            .all(|entry| entry.line != 1),
        "the second mnemonic key takes the mark away"
    );
}

#[ui_kit::gpui::test]
fn ctrl_s_writes_the_file_and_clears_the_dirty_mark(cx: &mut TestAppContext) {
    let (path, window, view) = open_text(cx, "save", "alpha\n");
    type_text(cx, window, "X");
    assert!(view.read_with(cx, |view, _| view.is_dirty()));

    press(cx, window, "ctrl-s");
    wait_for(cx, "the save", |_| {
        fs::read_to_string(&path).is_ok_and(|text| text == "Xalpha\n")
    });
    assert!(!view.read_with(cx, |view, _| view.is_dirty()));
}
