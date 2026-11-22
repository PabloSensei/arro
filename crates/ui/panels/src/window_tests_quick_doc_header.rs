//! The header of the quick documentation popup: a long declaration wraps inside the popup
//! and keeps off the buttons, and the place of a declaration is a line of its own under it.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use editor_view::Languages;
use lang_api::{
    Answer, Confidence, DocumentSpot, HoverPlace, HoverProvider, HoverText, LanguageFn, LanguageId,
    LanguageSupport, Provider, Queries, SourceId, SyntaxProvider,
};
use ui_kit::gpui;
use ui_kit::gpui::{Bounds, Modifiers, Pixels, TestAppContext, VisualTestContext, WindowHandle};
use ui_kit::{Theme, point, px};
use workspace::{FileIcons, Launch, Layout, Settings, Workspace};

use crate::index::SearchSetup;
use crate::window_tests::{
    bounds_of, draw, editor_of, line_height, open_path, press, project_setup, scratch,
};
use crate::window_tests_lang::project;
use crate::{AgentOptions, LanguageServer, connect, install};

const SOURCE: SourceId = SourceId::new("header-server");

/// A declaration too long for the popup. Its first part has no space in it, only the
/// separators of the names and the parameters.
const LONG: &str = "org.springframework.samples.petclinic.owner.OwnerController.processFindForm(int, Owner, BindingResult, Model)";

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

/// A language whose hover answer is set by the test. Its place is the method named after a
/// dash, with the class in front of the method name.
struct HeaderLanguage {
    syntax: NoSyntax,
    hover: Mutex<String>,
}

impl HeaderLanguage {
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

impl Provider<DocumentSpot, HoverText> for HeaderLanguage {
    fn start(&self, _: DocumentSpot, _: base::Cancel, mut reply: lang_api::Reply<HoverText>) {
        let text = self.hover.lock().unwrap().clone();
        let _ = reply.send(Answer::new(HoverText(text), Confidence::Certain, SOURCE));
    }
}

impl LanguageSupport for HeaderLanguage {
    fn id(&self) -> LanguageId {
        LanguageId("header")
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

    fn hover_place(&self, line: &str) -> Option<HoverPlace> {
        let (declaration, tail) = line.split_once(" - ")?;
        let mut names = tail.split('(').next()?.rsplit('.');
        let method = names.next()?;
        let class = names.next()?;
        Some(HoverPlace {
            declaration: declaration.to_owned(),
            place: format!("{class}.{method}()"),
        })
    }
}

/// The window with the fake language behind it.
fn open_window_with(
    cx: &mut TestAppContext,
    root: PathBuf,
    language: Arc<HeaderLanguage>,
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

/// The selectors of the chunks of the header, in order. The header is the first code block of
/// the popup and starts at the first character of the answer, so its chunks are numbered
/// from zero after `quick-doc-chunk-0-`.
const HEADER_CHUNKS: [&str; 24] = [
    "quick-doc-chunk-0-0",
    "quick-doc-chunk-0-1",
    "quick-doc-chunk-0-2",
    "quick-doc-chunk-0-3",
    "quick-doc-chunk-0-4",
    "quick-doc-chunk-0-5",
    "quick-doc-chunk-0-6",
    "quick-doc-chunk-0-7",
    "quick-doc-chunk-0-8",
    "quick-doc-chunk-0-9",
    "quick-doc-chunk-0-10",
    "quick-doc-chunk-0-11",
    "quick-doc-chunk-0-12",
    "quick-doc-chunk-0-13",
    "quick-doc-chunk-0-14",
    "quick-doc-chunk-0-15",
    "quick-doc-chunk-0-16",
    "quick-doc-chunk-0-17",
    "quick-doc-chunk-0-18",
    "quick-doc-chunk-0-19",
    "quick-doc-chunk-0-20",
    "quick-doc-chunk-0-21",
    "quick-doc-chunk-0-22",
    "quick-doc-chunk-0-23",
];

/// The bounds of the chunks of the header, up to the first one that is not on the screen.
fn header_chunks(cx: &mut TestAppContext, window: WindowHandle<Workspace>) -> Vec<Bounds<Pixels>> {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    HEADER_CHUNKS
        .iter()
        .copied()
        .map_while(|selector| visual.debug_bounds(selector))
        .collect()
}

/// Opens the popup on the first character of the file, with Ctrl+Q.
fn open_popup(cx: &mut TestAppContext, window: WindowHandle<Workspace>, main: &Path) {
    open_path(cx, window, main);
    click_editor(cx, window, 0, 1);
    press(cx, window, "ctrl-q");
    draw(cx, window);
    assert!(shown(cx, window, "quick-doc"), "Ctrl+Q opens the popup");
}

#[ui_kit::gpui::test]
fn a_long_declaration_wraps_in_the_popup_and_keeps_off_the_buttons(cx: &mut TestAppContext) {
    let root = project("header-wrap", &[("src/Main.txt", "alpha beta\n")]);
    let main = root.join("src").join("Main.txt");
    let language = HeaderLanguage::new();
    language.set_hover(&format!("```text\n{LONG}\n```\n\nThe words after it.\n"));
    let window = open_window_with(cx, root, language);
    cx.run_until_parked();
    draw(cx, window);

    open_popup(cx, window, &main);
    let frame = bounds_of(cx, window, "quick-doc");
    let buttons = bounds_of(cx, window, "quick-doc-buttons");

    let chunks = header_chunks(cx, window);
    assert!(
        chunks.len() > 2,
        "the long declaration is cut into chunks: {chunks:?}"
    );
    for chunk in &chunks {
        assert!(
            chunk.right() <= buttons.left(),
            "a chunk stays left of the buttons: {chunk:?} against {buttons:?}"
        );
        assert!(
            chunk.right() <= frame.right(),
            "a chunk stays inside the popup: {chunk:?} against {frame:?}"
        );
    }
    let mut rows: Vec<f32> = chunks.iter().map(|chunk| f32::from(chunk.top())).collect();
    rows.dedup();
    assert!(
        rows.len() >= 2,
        "the declaration wraps onto a second row: {rows:?}"
    );
}

#[ui_kit::gpui::test]
fn a_short_declaration_stays_on_one_row(cx: &mut TestAppContext) {
    let root = project("header-short", &[("src/Main.txt", "alpha beta\n")]);
    let main = root.join("src").join("Main.txt");
    let language = HeaderLanguage::new();
    language.set_hover("```text\nPage<Owner> ownersResults\n```\n");
    let window = open_window_with(cx, root, language);
    cx.run_until_parked();
    draw(cx, window);

    open_popup(cx, window, &main);
    let chunks = header_chunks(cx, window);
    assert!(chunks.len() > 1, "the declaration has words: {chunks:?}");
    let top = chunks[0].top();
    assert!(
        chunks.iter().all(|chunk| chunk.top() == top),
        "every word is on the first row: {chunks:?}"
    );
}

#[ui_kit::gpui::test]
fn the_place_of_a_declaration_is_a_line_of_its_own_under_it(cx: &mut TestAppContext) {
    let root = project("header-place", &[("src/Main.txt", "alpha beta\n")]);
    let main = root.join("src").join("Main.txt");
    let language = HeaderLanguage::new();
    language.set_hover(&format!(
        "```text\nPage<Owner> ownersResults - {LONG}\n```\n\nThe words after it.\n"
    ));
    let window = open_window_with(cx, root, language);
    cx.run_until_parked();
    draw(cx, window);

    open_popup(cx, window, &main);
    let view = editor_of(cx, window, &main);
    let text = view
        .read_with(cx, |view, _| view.quick_doc_text())
        .expect("the popup has the text of the answer");
    assert!(
        text.starts_with("Page<Owner> ownersResults\nin OwnerController.processFindForm()\n"),
        "the declaration is the header and the place is the line under it: {text}"
    );
    assert!(
        !text.contains("springframework"),
        "the package of the place is not shown: {text}"
    );
}
