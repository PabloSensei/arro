//! The popups of the editor and the gutter with a language behind them: one window at
//! a time, Escape on the topmost, Parameter Info clear of the completion list, and the
//! click on a line number that places or removes a breakpoint.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use editor_view::Languages;
use lang_api::{
    ActionEntry, ActionKind, ActionProvider, ActionQuery, Answer, CallParameter, CallSignature,
    CompletionEntry, CompletionList, CompletionProvider, Confidence, DocumentEdit, DocumentSpot,
    HoverProvider, HoverText, LanguageFn, LanguageId, LanguageSupport, Provider, Queries,
    SemanticServer, SignatureHelp, SignatureInfoProvider, SignatureQuery, SourceEdit, SourceId,
    SyntaxProvider, TextPoint, TextSpan,
};
use platform::CanonPath;
use ui_kit::gpui;
use ui_kit::gpui::{Modifiers, TestAppContext, VisualTestContext, WindowHandle};
use ui_kit::{Theme, point, px};
use workspace::{FileIcons, Launch, Layout, Settings, Workspace};

use crate::index::SearchSetup;
use crate::window_tests::{
    bounds_of, draw, editor_of, line_height, open_path, press, project_setup, scratch, type_text,
};
use crate::window_tests_lang::project;
use crate::{AgentOptions, LanguageServer, connect, install};

const SOURCE: SourceId = SourceId::new("popup-server");

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

fn uri_of(path: &Path) -> String {
    CanonPath::new(path).unwrap().to_uri()
}

fn edit_in(text: &str, start: usize, end: usize, new_text: &str) -> DocumentEdit {
    DocumentEdit {
        span: TextSpan {
            start: point_in(text, start),
            end: point_in(text, end),
        },
        text: new_text.to_owned(),
    }
}

/// The language of these tests: the entries, the signatures, the documentation and the
/// actions come from the test itself, no process runs.
pub(crate) struct PopupLanguage {
    syntax: NoSyntax,
    documents: Mutex<HashMap<String, Arc<str>>>,
    actions: Mutex<Vec<ActionEntry>>,
    hover: Mutex<String>,
}

impl PopupLanguage {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            syntax: NoSyntax(Queries::default()),
            documents: Mutex::new(HashMap::new()),
            actions: Mutex::new(Vec::new()),
            hover: Mutex::new(String::new()),
        })
    }

    pub(crate) fn set_actions(&self, entries: Vec<ActionEntry>) {
        *self.actions.lock().unwrap() = entries;
    }

    fn set_hover(&self, text: &str) {
        *self.hover.lock().unwrap() = text.to_owned();
    }

    fn answer<T>(&self, value: T) -> Answer<T> {
        Answer::new(value, Confidence::Certain, SOURCE)
    }
}

impl SemanticServer for PopupLanguage {
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

impl Provider<DocumentSpot, CompletionList> for PopupLanguage {
    fn start(&self, _: DocumentSpot, _: base::Cancel, mut reply: lang_api::Reply<CompletionList>) {
        let entry = |label: &str| CompletionEntry {
            label: label.to_owned(),
            insert_text: Some(label.to_owned()),
            ..CompletionEntry::default()
        };
        let _ = reply.send(self.answer(CompletionList {
            entries: vec![entry("greet"), entry("greeting")],
            complete: true,
        }));
    }
}

impl Provider<SignatureQuery, SignatureHelp> for PopupLanguage {
    fn start(&self, _: SignatureQuery, _: base::Cancel, mut reply: lang_api::Reply<SignatureHelp>) {
        let _ = reply.send(self.answer(SignatureHelp {
            signatures: vec![
                CallSignature {
                    label: "void greet(String name)".to_owned(),
                    parameters: vec![CallParameter {
                        label: "String name".to_owned(),
                    }],
                },
                CallSignature {
                    label: "void greet()".to_owned(),
                    parameters: Vec::new(),
                },
            ],
            active_signature: 0,
            active_parameter: 0,
        }));
    }
}

impl Provider<DocumentSpot, HoverText> for PopupLanguage {
    fn start(&self, _: DocumentSpot, _: base::Cancel, mut reply: lang_api::Reply<HoverText>) {
        let text = self.hover.lock().unwrap().clone();
        let _ = reply.send(self.answer(HoverText(text)));
    }
}

impl Provider<ActionQuery, Vec<ActionEntry>> for PopupLanguage {
    fn start(
        &self,
        query: ActionQuery,
        _: base::Cancel,
        mut reply: lang_api::Reply<Vec<ActionEntry>>,
    ) {
        let entries: Vec<ActionEntry> = self
            .actions
            .lock()
            .unwrap()
            .iter()
            .filter(|entry| query.only.is_none() || entry.kind == query.only)
            .cloned()
            .collect();
        let _ = reply.send(self.answer(entries));
    }
}

impl LanguageSupport for PopupLanguage {
    fn id(&self) -> LanguageId {
        LanguageId("popup")
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

    fn completion(&self) -> Option<&CompletionProvider> {
        Some(self)
    }

    fn signature_info(&self) -> Option<&SignatureInfoProvider> {
        Some(self)
    }

    fn hover(&self) -> Option<&HoverProvider> {
        Some(self)
    }

    fn actions(&self) -> Option<&ActionProvider> {
        Some(self)
    }
}

/// The window with the fake language behind it.
pub(crate) fn open_window_with(
    cx: &mut TestAppContext,
    root: PathBuf,
    language: Arc<PopupLanguage>,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let state_dir = scratch("state");
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
    let setup = project_setup(scratch("cache"));
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

/// Whether the popup with the debug selector is on screen.
fn shown(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

/// How many of the popups of the editor are on screen.
fn shown_popups(cx: &mut TestAppContext, window: WindowHandle<Workspace>) -> usize {
    [
        "completion-popup",
        "actions-popup",
        "signature-popup",
        "quick-doc",
        "lsp-note",
    ]
    .into_iter()
    .filter(|selector| shown(cx, window, selector))
    .count()
}

fn popup_bounds(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &'static str,
) -> gpui::Bounds<gpui::Pixels> {
    bounds_of(cx, window, selector)
}

/// Whether two rectangles of the window overlap by at least a pixel.
fn overlaps(left: gpui::Bounds<gpui::Pixels>, right: gpui::Bounds<gpui::Pixels>) -> bool {
    left.right() > right.left()
        && right.right() > left.left()
        && left.bottom() > right.top()
        && right.bottom() > left.top()
}

/// The point of a gutter column of a line: the number area starts left of the fold
/// column and the debug marks take the leftmost room.
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

fn click_gutter_at(cx: &mut TestAppContext, window: WindowHandle<Workspace>, line: usize, x: f32) {
    let at = gutter_point(cx, window, line, x);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_click(at, Modifiers::default());
    cx.run_until_parked();
}

fn drag_gutter(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    from: (usize, f32),
    to: (usize, f32),
) {
    let start = gutter_point(cx, window, from.0, from.1);
    let end = gutter_point(cx, window, to.0, to.1);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_move(start, None, Modifiers::default());
    visual.simulate_mouse_down(start, gpui::MouseButton::Left, Modifiers::default());
    for step in 1..=8 {
        let fraction = step as f32 / 8.;
        visual.simulate_mouse_move(
            point(
                start.x + (end.x - start.x) * fraction,
                start.y + (end.y - start.y) * fraction,
            ),
            Some(gpui::MouseButton::Left),
            Modifiers::default(),
        );
    }
    visual.simulate_mouse_up(end, gpui::MouseButton::Left, Modifiers::default());
    cx.run_until_parked();
    draw(cx, window);
}

fn breakpoint_lines(cx: &mut TestAppContext, path: &Path) -> Vec<u32> {
    let debug = cx.update(|cx| cx.global::<crate::debug::ActiveDebug>().0.clone());
    debug.read_with(cx, |panel, _| panel.breakpoint_lines(path))
}

/// The window with one file that has a quick fix of the test.
fn setup(cx: &mut TestAppContext, name: &str, text: &str) -> (PathBuf, WindowHandle<Workspace>) {
    let root = project(name, &[("src/Main.txt", text)]);
    let language = PopupLanguage::new();
    let path = root.join("src").join("Main.txt");
    let uri = uri_of(&path);
    language.set_actions(vec![ActionEntry {
        title: "Fix the problem".to_owned(),
        kind: Some(ActionKind::QuickFix),
        code: Some("quickfix.fix".to_owned()),
        preferred: true,
        edits: vec![SourceEdit {
            uri,
            edits: vec![edit_in(text, 0, 1, "f")],
        }],
        renames: Vec::new(),
        handle: None,
    }]);
    let window = open_window_with(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    (path, window)
}

#[ui_kit::gpui::test]
fn alt_enter_closes_the_completion_list(cx: &mut TestAppContext) {
    let (_path, window) = setup(cx, "popup-alt-enter", "problem\n");

    press(cx, window, "ctrl-space");
    draw(cx, window);
    assert!(
        shown(cx, window, "completion-popup"),
        "Ctrl+Space opens the completion list"
    );

    press(cx, window, "alt-enter");
    draw(cx, window);
    assert!(
        !shown(cx, window, "completion-popup"),
        "Alt+Enter closes the completion list"
    );
    assert!(
        shown(cx, window, "actions-popup"),
        "Alt+Enter shows the actions of the place"
    );
    assert_eq!(
        shown_popups(cx, window),
        1,
        "one window of the editor at a time"
    );
}

#[ui_kit::gpui::test]
fn typing_closes_alt_enter_and_opens_the_completion(cx: &mut TestAppContext) {
    let (path, window) = setup(cx, "popup-typing", "problem\n");
    let view = editor_of(cx, window, &path);

    press(cx, window, "alt-enter");
    draw(cx, window);
    assert!(shown(cx, window, "actions-popup"));

    type_text(cx, window, "g");
    draw(cx, window);
    assert!(
        !shown(cx, window, "actions-popup"),
        "a typed letter closes Alt+Enter"
    );
    assert!(
        shown(cx, window, "completion-popup"),
        "the typed letter opens the completion list"
    );
    assert!(
        view.read_with(cx, |view, _| view.text())
            .is_some_and(|text| text.starts_with('g')),
        "the letter reaches the text"
    );
}

#[ui_kit::gpui::test]
fn escape_closes_the_topmost_popup(cx: &mut TestAppContext) {
    let (_path, window) = setup(cx, "popup-escape", "problem\n");

    press(cx, window, "ctrl-space");
    draw(cx, window);
    press(cx, window, "ctrl-p");
    draw(cx, window);
    assert!(shown(cx, window, "completion-popup"));
    assert!(
        shown(cx, window, "signature-popup"),
        "Ctrl+P shows Parameter Info beside the list"
    );

    press(cx, window, "escape");
    draw(cx, window);
    assert!(
        !shown(cx, window, "signature-popup"),
        "Escape closes Parameter Info first"
    );
    assert!(
        shown(cx, window, "completion-popup"),
        "the list waits for its own Escape"
    );

    press(cx, window, "escape");
    draw(cx, window);
    assert!(
        !shown(cx, window, "completion-popup"),
        "the second Escape closes the list"
    );
    assert_eq!(
        shown_popups(cx, window),
        0,
        "no window of the editor stays open"
    );
}

#[ui_kit::gpui::test]
fn hover_does_not_open_the_documentation_over_the_completion(cx: &mut TestAppContext) {
    let root = project("popup-hover", &[("src/Main.txt", "problem call\n")]);
    let language = PopupLanguage::new();
    language.set_hover("```text\nvoid greet(String name)\n```\n\nThe documentation.");
    let path = root.join("src").join("Main.txt");
    let window = open_window_with(cx, root, language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);

    press(cx, window, "ctrl-space");
    draw(cx, window);
    assert!(shown(cx, window, "completion-popup"));

    let height = line_height(cx);
    let bounds = bounds_of(cx, window, "editor-body-0");
    let at = point(bounds.left() + px(67.), bounds.top() + px(height / 2.));
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_move(at, None, Modifiers::default());
    cx.executor()
        .advance_clock(std::time::Duration::from_millis(900));
    cx.run_until_parked();
    draw(cx, window);

    assert!(
        !shown(cx, window, "quick-doc"),
        "the pointer stays quiet while the completion list is open"
    );
    assert!(
        shown(cx, window, "completion-popup"),
        "the completion list keeps the screen"
    );
}

#[ui_kit::gpui::test]
fn parameter_info_does_not_cover_the_completion_list(cx: &mut TestAppContext) {
    let (_path, window) = setup(cx, "popup-parameter-info", "problem\n");

    press(cx, window, "ctrl-space");
    draw(cx, window);
    press(cx, window, "ctrl-p");
    draw(cx, window);

    let list = popup_bounds(cx, window, "completion-popup");
    let info = popup_bounds(cx, window, "signature-popup");
    assert!(
        list.size.height > px(0.) && info.size.height > px(0.),
        "both windows are on screen"
    );
    assert!(
        !overlaps(list, info),
        "Parameter Info and the completion list do not cover each other: {list:?} and {info:?}"
    );
}

#[ui_kit::gpui::test]
fn a_tab_switch_and_a_window_without_the_focus_close_alt_enter(cx: &mut TestAppContext) {
    let root = project(
        "popup-tab",
        &[("src/A.txt", "problem\n"), ("src/B.txt", "other\n")],
    );
    let first = root.join("src").join("A.txt");
    let second = root.join("src").join("B.txt");
    let language = PopupLanguage::new();
    language.set_actions(vec![ActionEntry {
        title: "Fix the problem".to_owned(),
        kind: Some(ActionKind::QuickFix),
        code: Some("quickfix.fix".to_owned()),
        preferred: true,
        edits: vec![SourceEdit {
            uri: uri_of(&first),
            edits: vec![edit_in("problem\n", 0, 1, "f")],
        }],
        renames: Vec::new(),
        handle: None,
    }]);
    let window = open_window_with(cx, root, language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &first);

    press(cx, window, "alt-enter");
    draw(cx, window);
    assert!(shown(cx, window, "actions-popup"));
    open_path(cx, window, &second);
    open_path(cx, window, &first);
    draw(cx, window);
    assert!(
        !shown(cx, window, "actions-popup"),
        "a tab that left the screen dropped its menu"
    );

    press(cx, window, "alt-enter");
    draw(cx, window);
    assert!(shown(cx, window, "actions-popup"));
    // The focus moves to another view of the window, as a click into the project tree
    // does; the menu belongs to the editor and goes with the focus.
    let panel = cx.update(|cx| cx.global::<crate::project::ActiveProjectPanel>().0.clone());
    window
        .update(cx, |_, window, cx| {
            panel.update(cx, |panel, cx| window.focus(&panel.focus(), cx))
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
    assert!(
        !shown(cx, window, "actions-popup"),
        "the editor without the focus dropped its menu"
    );
}

#[ui_kit::gpui::test]
fn a_click_on_a_line_number_toggles_a_breakpoint(cx: &mut TestAppContext) {
    let (path, window) = setup(cx, "gutter-number", "one\ntwo\nthree\n");

    // The number of the first line, right of the debug column.
    click_gutter_at(cx, window, 0, 38.);
    assert_eq!(
        breakpoint_lines(cx, &path),
        vec![1],
        "a click on the number places the breakpoint of its line"
    );

    click_gutter_at(cx, window, 0, 38.);
    assert!(
        breakpoint_lines(cx, &path).is_empty(),
        "the second click removes the breakpoint"
    );
}

#[ui_kit::gpui::test]
fn a_drag_over_line_numbers_selects_and_places_no_breakpoint(cx: &mut TestAppContext) {
    let (path, window) = setup(cx, "gutter-drag", "one\ntwo\nthree\n");
    let view = editor_of(cx, window, &path);

    drag_gutter(cx, window, (0, 38.), (2, 38.));

    assert!(
        breakpoint_lines(cx, &path).is_empty(),
        "a press that moved over the numbers places no breakpoint"
    );
    let selected = view
        .read_with(cx, |view, _| view.selected_text())
        .unwrap_or_default();
    assert!(
        selected.starts_with("one\ntwo\nthree"),
        "the drag selects the lines it crosses, not {selected:?}"
    );
}
