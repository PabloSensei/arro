//! The details of the editor that IDEA gets right: the gutter places a breakpoint on a
//! line that carries a mark, the change mark answers across its whole width, the fold
//! arrow alone folds, Parameter Info keeps its place over the call, the documentation
//! scrolls and outlives an arrow key, a preview comes from `codeAction/resolve`, and the
//! icons of the build files and the Makefile know their language.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use editor_view::{EndpointMark, PlaceMark, PlaceTarget, RunMark, RunMarkState};
use lang_api::{
    ActionEntry, ActionKind, ActionProvider, ActionQuery, ActionResolveProvider,
    ActionResolveQuery, Answer, Confidence, DocumentEdit, LanguageFn, LanguageId, LanguageSupport,
    Provider, Queries, Reply, SemanticServer, SourceEdit, SourceId, SyntaxProvider, TextPoint,
    TextSpan,
};
use ui_kit::gpui;
use ui_kit::gpui::{
    Modifiers, ScrollDelta, ScrollEvent, TestAppContext, VisualTestContext, WindowHandle,
};
use ui_kit::{Theme, point, px};
use workspace::{FileIcons, Launch, Layout, Settings, Workspace};

use crate::index::SearchSetup;
use crate::window_tests::{
    bounds_of, draw, editor_of, line_height, open_path, open_window, press, project_setup, scratch,
};
use crate::window_tests_doc_window::{
    DocsLanguage, click_editor, open_window_with as open_docs_window,
};
use crate::window_tests_editor_popups::{PopupLanguage, open_window_with as open_popup_window};
use crate::window_tests_folding::open_text;
use crate::window_tests_lang::project;
use crate::{AgentOptions, LanguageServer, connect, install};

const SOURCE: SourceId = SourceId::new("resolve-server");

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

fn point_in(text: &str, offset: usize) -> TextPoint {
    let before = &text[..offset];
    let line = before.bytes().filter(|byte| *byte == b'\n').count() as u32;
    let line_start = before.rfind('\n').map_or(0, |index| index + 1);
    let character = text[line_start..offset].encode_utf16().count() as u32;
    TextPoint { line, character }
}

/// A language whose actions carry no edit until the editor resolves them. The test keeps
/// the reply of every resolve and answers when it wants, so the popup can wait.
pub(crate) struct ResolveLanguage {
    syntax: NoSyntax,
    documents: Mutex<HashMap<String, Arc<str>>>,
    actions: Mutex<Vec<ActionEntry>>,
    pending: Mutex<Vec<(String, Reply<Option<ActionEntry>>)>>,
}

impl ResolveLanguage {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            syntax: NoSyntax(Queries::default()),
            documents: Mutex::new(HashMap::new()),
            actions: Mutex::new(Vec::new()),
            pending: Mutex::new(Vec::new()),
        })
    }

    pub(crate) fn set_actions(&self, entries: Vec<ActionEntry>) {
        *self.actions.lock().unwrap() = entries;
    }

    /// Whether the editor asked for the edits of `token` and still waits for them.
    pub(crate) fn asked(&self, token: &str) -> bool {
        self.pending
            .lock()
            .unwrap()
            .iter()
            .any(|(asked, _)| asked == token)
    }

    /// Answers the resolve of `token` with an edit, or nothing when the row is gone.
    pub(crate) fn answer(&self, token: &str, text: &str) {
        let mut pending = self.pending.lock().unwrap();
        let Some(index) = pending.iter().position(|(asked, _)| asked == token) else {
            return;
        };
        let (_, mut reply) = pending.remove(index);
        let uri = self
            .documents
            .lock()
            .unwrap()
            .keys()
            .next()
            .cloned()
            .unwrap_or_default();
        let mut entry = self
            .actions
            .lock()
            .unwrap()
            .iter()
            .find(|entry| entry.resolve.as_deref() == Some(token))
            .cloned()
            .unwrap_or_else(|| ActionEntry {
                title: token.to_owned(),
                kind: Some(ActionKind::QuickFix),
                code: Some(format!("quickfix.{token}")),
                preferred: true,
                edits: Vec::new(),
                renames: Vec::new(),
                handle: None,
                resolve: Some(token.to_owned()),
            });
        entry.edits = vec![SourceEdit {
            uri,
            edits: vec![DocumentEdit {
                span: TextSpan {
                    start: TextPoint::default(),
                    end: point_in(text, text.find('\n').unwrap_or(text.len())),
                },
                text: token.to_owned(),
            }],
        }];
        entry.resolve = None;
        let _ = reply.send(Answer::new(Some(entry), Confidence::Certain, SOURCE));
    }

    fn answer_of<T>(&self, value: T) -> Answer<T> {
        Answer::new(value, Confidence::Certain, SOURCE)
    }
}

impl SemanticServer for ResolveLanguage {
    fn open(&self, uri: &str, _: i32, text: Arc<str>) {
        self.documents.lock().unwrap().insert(uri.to_owned(), text);
    }

    fn change(&self, uri: &str, _: i32, text: Arc<str>, _: Vec<DocumentEdit>) {
        self.documents.lock().unwrap().insert(uri.to_owned(), text);
    }

    fn save(&self, _: &str) {}

    fn close(&self, uri: &str) {
        self.documents.lock().unwrap().remove(uri);
    }

    fn version(&self, uri: &str) -> Option<i32> {
        self.documents
            .lock()
            .unwrap()
            .contains_key(uri)
            .then_some(1)
    }
}

impl Provider<ActionQuery, Vec<ActionEntry>> for ResolveLanguage {
    fn start(&self, query: ActionQuery, _: base::Cancel, mut reply: Reply<Vec<ActionEntry>>) {
        let entries: Vec<ActionEntry> = self
            .actions
            .lock()
            .unwrap()
            .iter()
            .filter(|entry| query.only.is_none() || entry.kind == query.only)
            .cloned()
            .collect();
        let _ = reply.send(self.answer_of(entries));
    }
}

impl Provider<ActionResolveQuery, Option<ActionEntry>> for ResolveLanguage {
    fn start(&self, query: ActionResolveQuery, _: base::Cancel, reply: Reply<Option<ActionEntry>>) {
        self.pending.lock().unwrap().push((query.resolve, reply));
    }
}

impl LanguageSupport for ResolveLanguage {
    fn id(&self) -> LanguageId {
        LanguageId("resolve")
    }

    fn matches(&self, path: &Path) -> bool {
        path.extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("txt"))
    }

    fn syntax(&self) -> &dyn SyntaxProvider {
        &self.syntax
    }

    fn server(&self) -> Option<&dyn SemanticServer> {
        Some(self)
    }

    fn actions(&self) -> Option<&ActionProvider> {
        Some(self)
    }

    fn action_resolve(&self) -> Option<&ActionResolveProvider> {
        Some(self)
    }
}

/// The window with the language that resolves its actions on demand.
fn open_resolve_window(
    cx: &mut TestAppContext,
    root: PathBuf,
    language: Arc<ResolveLanguage>,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let state_dir = scratch("resolve-state");
    let store = store::Store::open(&state_dir.join("state.sqlite"))
        .ok()
        .map(Arc::new);
    cx.update(|cx| {
        cx.set_global(Theme::dark());
        cx.set_global(Settings::default());
        cx.set_global(editor_view::Languages {
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
    let setup = project_setup(scratch("resolve-cache"));
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

/// A window whose file icons know the languages of the editor and of the application.
fn open_icon_window(cx: &mut TestAppContext, root: PathBuf) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let state_dir = scratch("icon-state");
    let store = store::Store::open(&state_dir.join("state.sqlite"))
        .ok()
        .map(Arc::new);
    cx.update(|cx| {
        cx.set_global(Theme::dark());
        cx.set_global(Settings::default());
        cx.set_global(editor_view::Languages {
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
            by_name: window_language::file_icons().to_vec(),
            fallback: "fileTypes/text",
        }
        .with_languages(),
        background: true,
        info: Default::default(),
    };
    let setup = project_setup(scratch("icon-cache"));
    let agents = AgentOptions {
        state_dir: launch.state_dir.clone(),
        mock: false,
    };
    let (status_sender, status) = futures_channel::mpsc::unbounded();
    status_sender
        .unbounded_send("No language server in the test".to_string())
        .ok();
    let server = LanguageServer {
        support: None,
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

fn shown(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

/// The point of a gutter column of a line, as the number row of the editor paints it.
fn gutter_point(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    line: usize,
    x: f32,
) -> gpui::Point<gpui::Pixels> {
    let height = line_height(cx);
    let bounds = bounds_of(cx, window, "editor-body-0");
    point(
        bounds.left() + px(x),
        bounds.top() + px(height * line as f32 + height / 2.),
    )
}

fn click_at(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    at: gpui::Point<gpui::Pixels>,
) {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_click(at, Modifiers::default());
    cx.run_until_parked();
    draw(cx, window);
}

fn breakpoint_lines(cx: &mut TestAppContext, path: &Path) -> Vec<u32> {
    let debug = cx.update(|cx| cx.global::<crate::debug::ActiveDebug>().0.clone());
    debug.read_with(cx, |panel, _| panel.breakpoint_lines(path))
}

fn run_mark(line: u32) -> RunMark {
    RunMark {
        line,
        state: RunMarkState::Ready,
        color: Theme::dark().accent,
    }
}

#[ui_kit::gpui::test]
fn a_number_places_a_breakpoint_on_a_line_with_a_run_mark_or_a_bookmark(cx: &mut TestAppContext) {
    let root = project("gutter-marks", &[("src/Main.txt", "one\ntwo\nthree\n")]);
    let path = root.join("src").join("Main.txt");
    let window = open_window(cx, root);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);

    view.update(cx, |view, cx| {
        view.set_run_marks(vec![run_mark(0)], cx);
        view.restore_caret(1, 0, cx);
        view.toggle_bookmark(None, cx);
        view.set_place_marks(
            vec![PlaceMark {
                line: 2,
                icon: "nodes/class".into(),
                color: Theme::dark().accent,
                hint: "A place".into(),
                list: false,
                places: vec![PlaceTarget {
                    label: "Main".to_owned(),
                    file: PathBuf::from("src/Main.txt"),
                    offset: 0,
                }],
            }],
            cx,
        );
        view.set_endpoint_marks(
            vec![EndpointMark {
                line: 2,
                color: Theme::dark().accent,
            }],
            cx,
        );
    });
    draw(cx, window);

    for line in 0..3 {
        let at = gutter_point(cx, window, line, 38.);
        click_at(cx, window, at);
    }
    assert_eq!(
        breakpoint_lines(cx, &path),
        vec![1, 2, 3],
        "a click on the number places a breakpoint on a line with a mark"
    );
}

#[ui_kit::gpui::test]
fn a_click_in_the_middle_of_a_change_mark_opens_the_change(cx: &mut TestAppContext) {
    let root = crate::window_tests::git_repository("mark-middle");
    let path = root.join("main.txt");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    let git = cx.update(|cx| cx.global::<crate::git::ActiveGit>().0.clone());
    crate::window_tests_git::wait_for(cx, "the repository", |cx| {
        git.read_with(cx, |git, _| git.repo().is_some())
    });

    view.update(cx, |view, cx| view.replace_text(0..5, "ALPHA", cx));
    crate::window_tests_git::wait_for(cx, "the change marks", |cx| {
        draw(cx, window);
        view.read_with(cx, |view, _| !view.gutter_marks().is_empty())
    });
    let mark = view
        .read_with(cx, |view, _| view.change_mark_bounds(0))
        .expect("the change mark has its place");
    let middle = mark.center();
    click_at(cx, window, middle);
    assert!(
        shown(cx, window, "hunk-popup"),
        "a click in the middle of the change mark opens the window"
    );
}

#[ui_kit::gpui::test]
fn a_click_in_the_fold_column_away_from_the_arrow_does_not_fold(cx: &mut TestAppContext) {
    let text = "//region Outer\none();\ntwo();\n//endregion\n";
    let (_path, window, view) = open_text(cx, "fold-arrow", text);
    let rows = view.read_with(cx, |view, _| view.display_rows());
    let gutter = view.read_with(cx, |view, _| view.gutter_width());
    let arrow = gutter - 4. - 16. + 8.;

    let arrow_at = gutter_point(cx, window, 0, arrow);
    click_at(cx, window, arrow_at);
    let folded = view.read_with(cx, |view, _| view.display_rows());
    assert!(folded < rows, "a click on the arrow folds the region");

    let edge = gutter_point(cx, window, 0, gutter - 1.);
    click_at(cx, window, edge);
    assert_eq!(
        view.read_with(cx, |view, _| view.display_rows()),
        folded,
        "a click at the edge of the fold column, away from the arrow, keeps the fold"
    );
}

#[ui_kit::gpui::test]
fn parameter_info_stands_over_the_call_and_clear_of_the_completion_list(cx: &mut TestAppContext) {
    let root = project(
        "parameter-place",
        &[("src/Main.txt", "void greet() {}\n\n\n\n\n\ngreet();\n")],
    );
    let path = root.join("src").join("Main.txt");
    let language = PopupLanguage::new();
    let window = open_popup_window(cx, root, language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    view.update(cx, |view, cx| view.restore_caret(6, 7, cx));
    draw(cx, window);

    press(cx, window, "ctrl-space");
    press(cx, window, "ctrl-p");
    draw(cx, window);
    assert!(shown(cx, window, "completion-popup"));
    assert!(shown(cx, window, "signature-popup"));

    let height = line_height(cx);
    let editor = bounds_of(cx, window, "editor-body-0");
    let line_top = editor.top() + px(height * 6.);
    let info = bounds_of(cx, window, "signature-popup");
    let list = bounds_of(cx, window, "completion-popup");
    assert!(
        info.bottom() <= line_top + px(2.),
        "Parameter Info stands over the line of the call: {info:?}, line top {line_top:?}"
    );
    let overlaps = info.right() > list.left()
        && list.right() > info.left()
        && info.bottom() > list.top()
        && list.bottom() > info.top();
    assert!(
        !overlaps,
        "Parameter Info does not cover the completion list: {info:?} and {list:?}"
    );
}

/// A hover answer long enough to need scrolling.
fn long_documentation() -> String {
    let mut text = String::from("```text\nvoid long()\n```\n\n");
    for line in 0..120 {
        text.push_str(&format!("Line {line} of the documentation.\n\n"));
    }
    text
}

#[ui_kit::gpui::test]
fn the_documentation_scrolls_by_the_wheel_and_the_keys_and_outlives_an_arrow(
    cx: &mut TestAppContext,
) {
    let root = project("doc-scroll", &[("src/Main.txt", "format call\n")]);
    let main = root.join("src").join("Main.txt");
    let language = DocsLanguage::new();
    language.set_hover(&long_documentation());
    let window = open_docs_window(cx, root, language);
    cx.run_until_parked();
    draw(cx, window);

    open_path(cx, window, &main);
    click_editor(cx, window, 0, 1);
    press(cx, window, "ctrl-q");
    draw(cx, window);
    assert!(shown(cx, window, "quick-doc"), "Ctrl+Q opens the popup");
    let view = editor_of(cx, window, &main);
    assert_eq!(
        view.read_with(cx, |view, _| view.quick_doc_scroll()),
        Some(0.),
        "the popup starts at the top"
    );

    let doc = bounds_of(cx, window, "quick-doc");
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_event(ScrollEvent {
        position: doc.center(),
        delta: ScrollDelta::Pixels(point(px(0.), px(-120.))),
        modifiers: Modifiers::default(),
        ..ScrollEvent::default()
    });
    cx.run_until_parked();
    draw(cx, window);
    let wheel = view
        .read_with(cx, |view, _| view.quick_doc_scroll())
        .expect("the popup is open");
    assert!(wheel > 0., "the wheel scrolls the documentation: {wheel}");

    press(cx, window, "down");
    draw(cx, window);
    let key = view
        .read_with(cx, |view, _| view.quick_doc_scroll())
        .expect("the popup is open");
    assert!(key > wheel, "Down scrolls the documentation further: {key}");
    assert!(
        shown(cx, window, "quick-doc"),
        "an arrow key does not close the documentation"
    );

    press(cx, window, "escape");
    draw(cx, window);
    assert!(
        !shown(cx, window, "quick-doc"),
        "Escape closes the documentation"
    );
}

fn resolved_actions() -> Vec<ActionEntry> {
    let entry = |title: &str, token: &str| ActionEntry {
        title: title.to_owned(),
        kind: Some(ActionKind::QuickFix),
        code: Some(format!("quickfix.{token}")),
        preferred: true,
        edits: Vec::new(),
        renames: Vec::new(),
        handle: None,
        resolve: Some(token.to_owned()),
    };
    vec![entry("First", "first"), entry("Second", "second")]
}

#[ui_kit::gpui::test]
fn a_preview_of_an_action_without_edits_comes_from_resolve(cx: &mut TestAppContext) {
    let text = "problem\n";
    let root = project("resolve-preview", &[("src/Main.txt", text)]);
    let path = root.join("src").join("Main.txt");
    let language = ResolveLanguage::new();
    language.set_actions(resolved_actions());
    let window = open_resolve_window(cx, root, language.clone());
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);

    press(cx, window, "alt-enter");
    draw(cx, window);
    assert!(
        shown(cx, window, "actions-popup"),
        "Alt+Enter shows the list"
    );
    let visible = view
        .read_with(cx, |view, _| view.actions_visible())
        .expect("the list is open");
    assert_eq!(
        visible.rows.len(),
        2,
        "the list does not wait for the resolve"
    );
    assert_eq!(visible.preview, None, "no preview before the answer");
    assert!(language.asked("first"), "the editor asked for the edit");

    language.answer("first", text);
    cx.run_until_parked();
    draw(cx, window);
    let visible = view
        .read_with(cx, |view, _| view.actions_visible())
        .expect("the list is open");
    assert_eq!(
        visible.preview,
        Some(vec!["- problem".to_owned(), "+ first".to_owned()]),
        "the preview appears when the resolve answers"
    );
    assert!(
        shown(cx, window, "actions-preview"),
        "the preview stands beside the list"
    );
}

#[ui_kit::gpui::test]
fn a_preview_of_the_picked_over_row_is_dropped_when_the_selection_moves(cx: &mut TestAppContext) {
    let text = "problem\n";
    let root = project("resolve-cancel", &[("src/Main.txt", text)]);
    let path = root.join("src").join("Main.txt");
    let language = ResolveLanguage::new();
    language.set_actions(resolved_actions());
    let window = open_resolve_window(cx, root, language.clone());
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);

    press(cx, window, "alt-enter");
    draw(cx, window);
    assert!(language.asked("first"), "the first row asks for its edit");
    press(cx, window, "down");
    draw(cx, window);
    assert!(language.asked("second"), "the second row asks for its edit");

    // The first answer arrives after the selection left its row: it must not show.
    language.answer("first", text);
    cx.run_until_parked();
    draw(cx, window);
    let visible = view
        .read_with(cx, |view, _| view.actions_visible())
        .expect("the list is open");
    assert_eq!(
        visible.preview, None,
        "the answer of the row left behind is dropped"
    );

    language.answer("second", text);
    cx.run_until_parked();
    draw(cx, window);
    let visible = view
        .read_with(cx, |view, _| view.actions_visible())
        .expect("the list is open");
    assert_eq!(
        visible.preview,
        Some(vec!["- problem".to_owned(), "+ second".to_owned()]),
        "the selected row gets its preview"
    );
}

#[ui_kit::gpui::test]
fn build_files_and_the_makefile_carry_their_language_icons_in_the_tree(cx: &mut TestAppContext) {
    let root = scratch("idea-file-icons");
    let gradle = root.join("build.gradle");
    let kotlin = root.join("build.gradle.kts");
    let makefile = root.join("Makefile");
    let ini = root.join("app.ini");
    for path in [&gradle, &kotlin, &makefile, &ini] {
        fs::write(path, "\n").unwrap();
    }
    let window = open_icon_window(cx, root.clone());
    cx.run_until_parked();
    draw(cx, window);

    let (gradle_icon, kotlin_icon, make_icon, ini_icon) = window
        .read_with(cx, |workspace, _| {
            let icons = workspace.icons();
            (
                icons.icon_for(&gradle),
                icons.icon_for(&kotlin),
                icons.icon_for(&makefile),
                icons.icon_for(&ini),
            )
        })
        .unwrap();
    assert_eq!(
        gradle_icon, "fileTypes/groovy",
        "a Gradle build script carries the Groovy icon"
    );
    assert_eq!(
        kotlin_icon, "fileTypes/kotlin",
        "a Gradle Kotlin script carries the Kotlin icon"
    );
    assert_eq!(
        make_icon, "fileTypes/makefile",
        "a Makefile carries its own icon"
    );
    assert_eq!(
        ini_icon, "fileTypes/ini",
        "an INI file carries its own icon"
    );
}
