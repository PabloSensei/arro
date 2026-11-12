//! The documentation popup and the Documentation tool window: a drag selects a part of
//! the answer, a markdown table is a grid, the second Ctrl+Q moves the answer to the
//! window, the caret keeps it current and Back returns to the answer seen before.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use editor_view::Languages;
use lang_api::{
    Answer, Confidence, DocumentSpot, HoverProvider, HoverText, LanguageFn, LanguageId,
    LanguageSupport, Provider, Queries, SourceId, SyntaxProvider,
};
use ui_kit::gpui;
use ui_kit::gpui::{Modifiers, TestAppContext, VisualTestContext, WindowHandle};
use ui_kit::{Theme, point, px};
use workspace::{DOCUMENTATION_TOOL, FileIcons, Launch, Layout, Settings, Workspace};

use crate::documentation::ActiveDocumentation;
use crate::index::SearchSetup;
use crate::window_tests::{
    bounds_of, click, draw, editor_of, line_height, open_path, press, project_setup, scratch,
    wait_for_signal,
};
use crate::window_tests_lang::project;
use crate::{AgentOptions, LanguageServer, connect, install};

const SOURCE: SourceId = SourceId::new("docs-server");

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

/// A language whose documentation answer is set by the test.
struct DocsLanguage {
    syntax: NoSyntax,
    hover: Mutex<String>,
}

impl DocsLanguage {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            syntax: NoSyntax(Queries::default()),
            hover: Mutex::new(String::new()),
        })
    }

    fn set_hover(&self, text: &str) {
        *self.hover.lock().unwrap() = text.to_owned();
    }
}

impl Provider<DocumentSpot, HoverText> for DocsLanguage {
    fn start(&self, _: DocumentSpot, _: base::Cancel, mut reply: lang_api::Reply<HoverText>) {
        let text = self.hover.lock().unwrap().clone();
        let _ = reply.send(Answer::new(HoverText(text), Confidence::Certain, SOURCE));
    }
}

impl LanguageSupport for DocsLanguage {
    fn id(&self) -> LanguageId {
        LanguageId("docs")
    }

    fn matches(&self, path: &Path) -> bool {
        path.extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("txt"))
    }

    fn syntax(&self) -> &dyn SyntaxProvider {
        &self.syntax
    }

    fn hover(&self) -> Option<&HoverProvider> {
        Some(self)
    }
}

fn open_window_with(
    cx: &mut TestAppContext,
    root: PathBuf,
    language: Arc<DocsLanguage>,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let state_dir = scratch("doc-state");
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
    let setup = project_setup(scratch("doc-cache"));
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

fn shown(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

/// Clicks a character of a line, as the editor tests do.
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

fn drag(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    from: gpui::Point<gpui::Pixels>,
    to: gpui::Point<gpui::Pixels>,
) {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_move(from, None, Modifiers::default());
    visual.simulate_mouse_down(from, gpui::MouseButton::Left, Modifiers::default());
    for step in 1..=8 {
        let fraction = step as f32 / 8.;
        visual.simulate_mouse_move(
            point(
                from.x + (to.x - from.x) * fraction,
                from.y + (to.y - from.y) * fraction,
            ),
            Some(gpui::MouseButton::Left),
            Modifiers::default(),
        );
    }
    visual.simulate_mouse_up(to, gpui::MouseButton::Left, Modifiers::default());
    cx.run_until_parked();
    draw(cx, window);
}

fn copied(cx: &TestAppContext) -> String {
    cx.read(|cx| cx.read_from_clipboard().and_then(|item| item.text()))
        .unwrap_or_default()
}

#[ui_kit::gpui::test]
fn a_drag_selects_a_part_of_the_answer_and_ctrl_c_copies_it_without_markers(
    cx: &mut TestAppContext,
) {
    let root = project("doc-select", &[("src/Main.txt", "alpha beta\n")]);
    let main = root.join("src").join("Main.txt");
    let language = DocsLanguage::new();
    language.set_hover("Alpha **Beta** Gamma Delta");
    let window = open_window_with(cx, root, language);
    cx.run_until_parked();
    draw(cx, window);

    open_path(cx, window, &main);
    click_editor(cx, window, 0, 1);
    press(cx, window, "ctrl-q");
    draw(cx, window);
    assert!(shown(cx, window, "quick-doc"), "Ctrl+Q opens the popup");

    let start = bounds_of(cx, window, "quick-doc-word-0").center();
    let end = bounds_of(cx, window, "quick-doc-word-3").center();
    drag(cx, window, start, end);

    let view = editor_of(cx, window, &main);
    assert!(
        view.read_with(cx, |view, _| view.quick_doc_selection())
            .is_some(),
        "the drag leaves a selection"
    );
    press(cx, window, "ctrl-c");
    let text = copied(cx);
    assert!(
        text.contains("Beta"),
        "the copied part has the word: {text:?}"
    );
    assert!(!text.contains("**"), "the markers stay out: {text:?}");
    let whole = view
        .read_with(cx, |view, _| view.quick_doc_text())
        .expect("the popup shows");
    assert!(
        text.len() < whole.len(),
        "a part is copied, not the whole answer"
    );
}

#[ui_kit::gpui::test]
fn a_markdown_table_is_a_grid_with_cells(cx: &mut TestAppContext) {
    let root = project("doc-table", &[("src/Main.txt", "alpha beta\n")]);
    let main = root.join("src").join("Main.txt");
    let language = DocsLanguage::new();
    language.set_hover("| Name | Value |\n| --- | ---: |\n| one | 1 |\n");
    let window = open_window_with(cx, root, language);
    cx.run_until_parked();
    draw(cx, window);

    open_path(cx, window, &main);
    click_editor(cx, window, 0, 1);
    press(cx, window, "ctrl-q");
    draw(cx, window);

    assert!(
        shown(cx, window, "quick-doc-table-0"),
        "the answer draws a table"
    );
    assert!(
        bounds_of(cx, window, "quick-doc-table-0-cell-0-0")
            .size
            .width
            > px(0.),
        "the header cell is there"
    );
    assert!(
        bounds_of(cx, window, "quick-doc-table-0-cell-1-1")
            .size
            .width
            > px(0.),
        "the body cell is there"
    );
    let text = editor_of(cx, window, &main)
        .read_with(cx, |view, _| view.quick_doc_text())
        .expect("the popup shows");
    assert!(!text.contains("| ---"), "the separator row is gone: {text}");
}

/// The answer on screen of the Documentation window.
fn doc_window_text(cx: &mut TestAppContext) -> Option<String> {
    cx.read(|cx| {
        cx.try_global::<ActiveDocumentation>()
            .and_then(|panel| panel.0.read(cx).documentation_text())
    })
}

#[ui_kit::gpui::test]
fn the_second_ctrl_q_opens_the_documentation_window_and_the_caret_updates_it(
    cx: &mut TestAppContext,
) {
    let root = project("doc-window", &[("src/Main.txt", "alpha beta\n")]);
    let main = root.join("src").join("Main.txt");
    let language = DocsLanguage::new();
    language.set_hover("Alpha documentation");
    let window = open_window_with(cx, root, language.clone());
    cx.run_until_parked();
    draw(cx, window);

    open_path(cx, window, &main);
    click_editor(cx, window, 0, 1);
    press(cx, window, "ctrl-q");
    draw(cx, window);
    press(cx, window, "ctrl-q");
    cx.run_until_parked();
    draw(cx, window);

    assert!(
        shown(cx, window, "documentation-window"),
        "the second Ctrl+Q opens the Documentation window"
    );
    assert!(
        window
            .read_with(cx, |workspace, _| workspace.tool_open(DOCUMENTATION_TOOL))
            .unwrap(),
        "the window is the open tool"
    );
    wait_for_signal(cx, "the first answer", |cx| {
        doc_window_text(cx).is_some_and(|text| text.contains("Alpha documentation"))
    });

    language.set_hover("Beta documentation");
    click_editor(cx, window, 0, 7);
    wait_for_signal(cx, "the answer of the other symbol", |cx| {
        doc_window_text(cx).is_some_and(|text| text.contains("Beta documentation"))
    });

    click(cx, window, "doc-back");
    cx.run_until_parked();
    draw(cx, window);
    assert!(
        doc_window_text(cx).is_some_and(|text| text.contains("Alpha documentation")),
        "Back returns to the answer seen before: {:?}",
        doc_window_text(cx)
    );
}
