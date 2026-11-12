//! Documentation comments shown as rendered text in the editor: the key, the gutter
//! icon, the setting, the rows the caret passes and the find that brings the text back.

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use editor_view::{EditorView, Languages};
use lang_api::{
    DocumentationBlock, DocumentationComment, DocumentationCommentProvider, DocumentationInline,
    LanguageFn, LanguageId, LanguageSupport, Queries, SyntaxProvider,
};
use ui_kit::gpui;
use ui_kit::gpui::{Modifiers, TestAppContext, VisualTestContext, WindowHandle};
use ui_kit::{Theme, point, px};
use workspace::{FileIcons, Launch, Layout, Settings, Workspace};

use crate::index::SearchSetup;
use crate::window_tests::{
    bounds_of, buffer_of, draw, editor_of, line_height, open_path, press, project_setup, scratch,
    type_text,
};
use crate::window_tests_lang::project;
use crate::{AgentOptions, LanguageServer, connect, install};

unsafe extern "C" fn no_grammar() -> *const () {
    std::ptr::null()
}

struct NoSyntax(Queries);

impl SyntaxProvider for NoSyntax {
    fn grammar(&self) -> LanguageFn {
        unsafe { LanguageFn::from_raw(no_grammar) }
    }

    fn queries(&self) -> &Queries {
        &self.0
    }
}

/// A language that answers with the documentation comments the test gives it.
struct DocLanguage {
    syntax: NoSyntax,
    comments: Mutex<Vec<DocumentationComment>>,
}

impl DocLanguage {
    fn new(comments: Vec<DocumentationComment>) -> Arc<Self> {
        Arc::new(Self {
            syntax: NoSyntax(Queries::default()),
            comments: Mutex::new(comments),
        })
    }
}

impl DocumentationCommentProvider for DocLanguage {
    fn documentation_comments(&self, text: &str) -> Vec<DocumentationComment> {
        let known = self.comments.lock().unwrap().clone();
        let mut comments = Vec::new();
        let mut from = 0;
        while let Some(open) = text[from..].find("/**") {
            let start = from + open;
            let Some(close) = text[start..].find("*/") else {
                break;
            };
            let end = start + close + 2;
            comments.push(DocumentationComment {
                range: start..end,
                blocks: known
                    .get(comments.len())
                    .map(|comment| comment.blocks.clone())
                    .unwrap_or_default(),
            });
            from = end;
        }
        comments
    }
}

impl LanguageSupport for DocLanguage {
    fn id(&self) -> LanguageId {
        LanguageId("documented")
    }

    fn matches(&self, path: &Path) -> bool {
        path.extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("txt"))
    }

    fn syntax(&self) -> &dyn SyntaxProvider {
        &self.syntax
    }

    fn documentation_comments(&self) -> Option<&dyn DocumentationCommentProvider> {
        Some(self)
    }
}

fn open_window_with(
    cx: &mut TestAppContext,
    root: PathBuf,
    language: Arc<DocLanguage>,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let state_dir = scratch("rendered-state");
    let store = store::Store::open(&state_dir.join("state.sqlite"))
        .ok()
        .map(Arc::new);
    cx.update(|cx| {
        cx.set_global(Theme::dark());
        cx.set_global(Settings::default());
        cx.set_global(Languages {
            resolve: |path| dockerfile_syntax::is_dockerfile(path).then(dockerfile_syntax::config),
            templates: |_| None,
        });
        workspace::bind_keys(cx);
    });
    let launch = Launch {
        root,
        open: None,
        state_dir: state_dir.clone(),
        settings_dir: state_dir,
        icons: FileIcons {
            by_name: Vec::new(),
            fallback: "fileTypes/text",
        },
        background: true,
        info: Default::default(),
    };
    let setup = project_setup(scratch("rendered-cache"));
    let agents = AgentOptions {
        state_dir: launch.state_dir.clone(),
        mock: false,
    };
    let (status_sender, status) = futures_channel::mpsc::unbounded();
    status_sender
        .unbounded_send("Language server ready".to_string())
        .ok();
    let server = LanguageServer {
        support: Some(language as Arc<dyn LanguageSupport>),
        status,
    };
    cx.add_window(move |window, cx| {
        let mut workspace =
            Workspace::new_for_test(launch, Layout::default(), store, Vec::new(), window, cx);
        install(
            &mut workspace,
            SearchSetup::default(),
            setup,
            agents,
            window,
            cx,
        );
        connect(&mut workspace, server, cx);
        workspace
    })
}

fn settle(cx: &mut TestAppContext) {
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(20));
    cx.run_until_parked();
}

fn apply(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    change: impl FnOnce(&mut Settings),
) {
    window
        .update(cx, |workspace, _, cx| workspace.update_settings(change, cx))
        .unwrap();
    settle(cx);
}

/// The range of the lines that hold a comment from `open` to `close`.
fn comment_range(text: &str, open: &str, close: &str) -> Range<usize> {
    let open_at = text.find(open).expect("the comment opens");
    let start = text[..open_at].rfind('\n').map_or(0, |at| at + 1);
    let close_at = text[open_at..].find(close).expect("the comment closes") + open_at;
    let end = text[close_at..]
        .find('\n')
        .map_or(text.len(), |at| close_at + at);
    start..end
}

fn blocks(sentence: &str) -> Vec<DocumentationBlock> {
    vec![
        DocumentationBlock::Paragraph(vec![DocumentationInline::Text(sentence.to_owned())]),
        DocumentationBlock::Tag {
            name: "param".to_owned(),
            subject: Some("a".to_owned()),
            description: vec![DocumentationInline::Text("the first".to_owned())],
        },
        DocumentationBlock::Tag {
            name: "return".to_owned(),
            subject: None,
            description: vec![DocumentationInline::Text("the sum".to_owned())],
        },
    ]
}

const TEXT: &str = "class A {\n    /**\n     * Adds two numbers.\n     * @param a the first\n     * @return the sum\n     */\n    int add(int a) { return a; }\n\n    /**\n     * Another comment.\n     */\n    int value() { return 1; }\n}\n";

fn documented(root: &Path) -> (PathBuf, Vec<DocumentationComment>) {
    let path = root.join("src").join("Main.txt");
    let comments = vec![
        DocumentationComment {
            range: comment_range(TEXT, "    /**\n     * Adds", "*/"),
            blocks: blocks("Adds two numbers."),
        },
        DocumentationComment {
            range: comment_range(TEXT, "    /**\n     * Another", "*/"),
            blocks: blocks("Another comment."),
        },
    ];
    (path, comments)
}

fn click_line(cx: &mut TestAppContext, window: WindowHandle<Workspace>, row: f32, column: f32) {
    let height = line_height(cx);
    let bounds = bounds_of(cx, window, "editor-body-0");
    let at = point(
        bounds.left() + px(column),
        bounds.top() + px(height * row + height / 2.),
    );
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_click(at, Modifiers::default());
    cx.run_until_parked();
}

fn caret_line(cx: &TestAppContext, view: &gpui::Entity<EditorView>) -> Option<usize> {
    view.read_with(cx, |view, _| view.caret_line())
}

#[ui_kit::gpui::test]
fn the_key_renders_the_comment_under_the_caret_and_keeps_the_text(cx: &mut TestAppContext) {
    let root = project("rendered-doc-key", &[("src/Main.txt", TEXT)]);
    let (path, comments) = documented(&root);
    let window = open_window_with(cx, root.clone(), DocLanguage::new(comments));
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    settle(cx);

    assert!(
        view.read_with(cx, |view, _| view.rendered_doc_rows())
            .is_empty(),
        "the setting is off at first"
    );
    assert_eq!(
        view.read_with(cx, |view, _| view.rendered_doc_icons()),
        [(1, false), (8, false)]
    );

    view.update(cx, |view, cx| view.restore_caret(2, 8, cx));
    press(cx, window, "ctrl-alt-q");
    let rows = view.read_with(cx, |view, _| view.rendered_doc_rows());
    assert!(
        rows.iter()
            .any(|(line, text)| *line == 1 && text.contains("Adds two numbers.")),
        "the comment shows its text: {rows:?}"
    );
    assert_eq!(
        buffer_of(cx, &view),
        TEXT,
        "the file keeps the comment source"
    );
    assert!(
        view.read_with(cx, |view, _| view.rendered_doc_icons())
            .contains(&(1, true))
    );

    press(cx, window, "ctrl-alt-q");
    assert!(
        view.read_with(cx, |view, _| view.rendered_doc_rows())
            .is_empty(),
        "the second key brings the source back"
    );
}

#[ui_kit::gpui::test]
fn the_gutter_icon_switches_the_comment_and_arrows_pass_the_rows(cx: &mut TestAppContext) {
    let root = project("rendered-doc-gutter", &[("src/Main.txt", TEXT)]);
    let (path, comments) = documented(&root);
    let window = open_window_with(cx, root.clone(), DocLanguage::new(comments));
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    settle(cx);

    // The icon stands in the gutter of the line where the comment begins.
    click_line(cx, window, 1., 51.);
    assert!(
        view.read_with(cx, |view, _| view.rendered_doc_icons())
            .contains(&(1, true)),
        "a click on the icon renders the comment"
    );

    view.update(cx, |view, cx| view.restore_caret(0, 0, cx));
    press(cx, window, "down");
    assert_eq!(
        caret_line(cx, &view),
        Some(6),
        "one step down from the line above reaches the code under the comment"
    );

    // A click on a rendered row, away from its links, brings the text back.
    click_line(cx, window, 1., 220.);
    assert!(
        view.read_with(cx, |view, _| view.rendered_doc_rows())
            .is_empty(),
        "a click on the block returns the source"
    );
    assert_eq!(caret_line(cx, &view), Some(1));
}

#[ui_kit::gpui::test]
fn the_setting_renders_every_comment_and_its_off_returns_the_text(cx: &mut TestAppContext) {
    let root = project("rendered-doc-setting", &[("src/Main.txt", TEXT)]);
    let (path, comments) = documented(&root);
    let window = open_window_with(cx, root.clone(), DocLanguage::new(comments));
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    settle(cx);

    apply(cx, window, |settings| settings.render_doc_comments = true);
    let icons = view.read_with(cx, |view, _| view.rendered_doc_icons());
    assert_eq!(icons, [(1, true), (8, true)], "the setting covers them all");
    let rows = view.read_with(cx, |view, _| view.rendered_doc_rows());
    assert!(rows.iter().any(|(line, _)| *line == 1));
    assert!(rows.iter().any(|(line, _)| *line == 8));
    assert_eq!(buffer_of(cx, &view), TEXT);

    apply(cx, window, |settings| settings.render_doc_comments = false);
    assert!(
        view.read_with(cx, |view, _| view.rendered_doc_rows())
            .is_empty(),
        "the setting off gives every comment back"
    );
}

#[ui_kit::gpui::test]
fn find_inside_a_rendered_comment_brings_that_comment_back(cx: &mut TestAppContext) {
    let root = project("rendered-doc-find", &[("src/Main.txt", TEXT)]);
    let (path, comments) = documented(&root);
    let window = open_window_with(cx, root.clone(), DocLanguage::new(comments));
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    settle(cx);

    apply(cx, window, |settings| settings.render_doc_comments = true);
    assert_eq!(
        view.read_with(cx, |view, _| view.rendered_doc_icons()),
        [(1, true), (8, true)]
    );

    view.update(cx, |view, cx| view.restore_caret(0, 0, cx));
    press(cx, window, "ctrl-f");
    type_text(cx, window, "Another");
    settle(cx);

    assert_eq!(
        view.read_with(cx, |view, _| view.rendered_doc_icons()),
        [(1, true), (8, false)],
        "only the comment with the match goes back to text"
    );
    assert_eq!(caret_line(cx, &view), Some(9));
}

#[ui_kit::gpui::test]
fn an_edit_inside_a_rendered_comment_returns_its_text(cx: &mut TestAppContext) {
    let root = project("rendered-doc-typing", &[("src/Main.txt", TEXT)]);
    let (path, comments) = documented(&root);
    let window = open_window_with(cx, root.clone(), DocLanguage::new(comments));
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    settle(cx);

    apply(cx, window, |settings| settings.render_doc_comments = true);
    view.update(cx, |view, cx| view.restore_caret(2, 8, cx));
    type_text(cx, window, "x");
    settle(cx);

    assert_eq!(
        view.read_with(cx, |view, _| view.rendered_doc_icons()),
        [(1, false), (8, true)],
        "the edited comment shows its source, the other stays rendered"
    );
    assert_ne!(
        buffer_of(cx, &view),
        TEXT,
        "the typed letter landed in the comment"
    );
}

#[ui_kit::gpui::test]
fn an_edit_next_to_a_rendered_comment_keeps_it_rendered(cx: &mut TestAppContext) {
    let root = project("rendered-doc-edit", &[("src/Main.txt", TEXT)]);
    let (path, comments) = documented(&root);
    let window = open_window_with(cx, root.clone(), DocLanguage::new(comments));
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    settle(cx);

    apply(cx, window, |settings| settings.render_doc_comments = true);
    assert!(
        !view
            .read_with(cx, |view, _| view.rendered_doc_rows())
            .is_empty()
    );

    view.update(cx, |view, cx| view.restore_caret(0, 0, cx));
    press(cx, window, "enter");
    settle(cx);
    assert!(
        !view
            .read_with(cx, |view, _| view.rendered_doc_rows())
            .is_empty(),
        "an edit above the comments leaves them rendered"
    );
}
