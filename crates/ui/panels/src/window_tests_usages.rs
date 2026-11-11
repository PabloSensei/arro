//! Ctrl+Alt+F7: the Show Usages popup at the caret, driven by the fake language of
//! `window_tests_lang`.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use editor_view::Languages;
use lang_api::{
    Answer, Confidence, DocumentSpot, LanguageId, LanguageSupport, Provider, Reply, SourceId,
    SourceSpan, SyntaxProvider, TextSpan,
};
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use ui_kit::{Theme, px};
use workspace::{FileIcons, Launch, Layout, Settings, USAGES_TOOL, Workspace};

use crate::index::SearchSetup;
use crate::usages::{ActiveShowUsages, ActiveUsages};
use crate::window_tests::{
    active_path, bounds_of, click, draw, editor_of, open_path, press, project_setup, scratch,
    type_text,
};
use crate::window_tests_lang::{FakeLanguage, open_window_with, project, span_in, uri_of};
use crate::window_tests_ui::wait_for;
use crate::{AgentOptions, LanguageServer, connect, install};

const SLOW_SOURCE: SourceId = SourceId::new("slow-references");

/// The fake language with references that arrive after a delay, so a test can look at
/// the popup while the search is still running.
struct SlowLanguage {
    inner: Arc<FakeLanguage>,
    spans: Vec<SourceSpan>,
    delay: Duration,
}

impl Provider<DocumentSpot, Vec<SourceSpan>> for SlowLanguage {
    fn start(&self, _: DocumentSpot, _: base::Cancel, mut reply: Reply<Vec<SourceSpan>>) {
        let spans = self.spans.clone();
        let delay = self.delay;
        std::thread::spawn(move || {
            std::thread::sleep(delay);
            let _ = reply.send(Answer::new(spans, Confidence::Certain, SLOW_SOURCE));
        });
    }
}

impl LanguageSupport for SlowLanguage {
    fn id(&self) -> LanguageId {
        self.inner.id()
    }

    fn matches(&self, path: &Path) -> bool {
        self.inner.matches(path)
    }

    fn syntax(&self) -> &dyn SyntaxProvider {
        self.inner.syntax()
    }

    fn server(&self) -> Option<&dyn lang_api::SemanticServer> {
        self.inner.server()
    }

    fn references(&self) -> Option<&lang_api::ReferencesProvider> {
        Some(self)
    }
}

/// The same window as `open_window_with`, for a language of the test.
fn open_slow_window(
    cx: &mut TestAppContext,
    root: std::path::PathBuf,
    language: Arc<SlowLanguage>,
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
            resolve: |_| None,
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

fn popup(cx: &TestAppContext) -> Option<gpui::Entity<crate::usages::ShowUsagesPopup>> {
    cx.update(|cx| {
        cx.try_global::<ActiveShowUsages>()
            .and_then(|active| active.0.clone())
    })
}

fn visible_rows(cx: &TestAppContext) -> usize {
    popup(cx)
        .map(|popup| popup.read_with(cx, |popup, _| popup.visible_rows()))
        .unwrap_or(0)
}

fn done(cx: &TestAppContext) -> bool {
    popup(cx)
        .map(|popup| popup.read_with(cx, |popup, _| popup.is_done()))
        .unwrap_or(false)
}

/// A place in a file, at the start of its only line.
fn at(path: &Path, text: &str, start: usize, end: usize) -> SourceSpan {
    SourceSpan {
        uri: uri_of(path),
        span: span_in(text, start, end),
    }
}

#[ui_kit::gpui::test]
fn the_popup_lists_the_places_and_enter_opens_the_selected_one(cx: &mut TestAppContext) {
    let root = project(
        "show-usages",
        &[
            ("src/Main.txt", "greet\n"),
            ("src/Other.txt", "greet\n"),
            ("src/Third.txt", "greet\n"),
        ],
    );
    let main = root.join("src").join("Main.txt");
    let other = root.join("src").join("Other.txt");
    let third = root.join("src").join("Third.txt");
    let language = FakeLanguage::new();
    language.set_references(vec![
        at(&other, "greet\n", 0, 5),
        at(&main, "greet\n", 0, 5),
        at(&third, "greet\n", 0, 5),
    ]);
    let window = open_window_with(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &main);

    press(cx, window, "ctrl-alt-f7");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "usages-popup").size.height > px(0.),
        "Ctrl+Alt+F7 shows the popup"
    );
    assert_eq!(visible_rows(cx), 3, "the popup lists every place");
    assert!(
        !window
            .read_with(cx, |workspace, _| workspace.tool_open(USAGES_TOOL))
            .unwrap(),
        "the popup does not open the Usages window"
    );

    press(cx, window, "down enter");
    assert_eq!(active_path(cx, window), Some(other.clone()));
    let view = editor_of(cx, window, &other);
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (0, 0));
    assert!(popup(cx).is_none(), "the popup closes after the jump");
}

#[ui_kit::gpui::test]
fn typing_filters_the_rows(cx: &mut TestAppContext) {
    let root = project(
        "show-usages-filter",
        &[
            ("src/Main.txt", "greet alpha\n"),
            ("src/Other.txt", "greet beta\n"),
            ("src/Third.txt", "greet gamma\n"),
        ],
    );
    let main = root.join("src").join("Main.txt");
    let other = root.join("src").join("Other.txt");
    let third = root.join("src").join("Third.txt");
    let language = FakeLanguage::new();
    language.set_references(vec![
        at(&main, "greet alpha\n", 0, 5),
        at(&other, "greet beta\n", 0, 5),
        at(&third, "greet gamma\n", 0, 5),
    ]);
    let window = open_window_with(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &main);

    press(cx, window, "ctrl-alt-f7");
    draw(cx, window);
    assert_eq!(visible_rows(cx), 3);

    type_text(cx, window, "beta");
    draw(cx, window);
    assert_eq!(visible_rows(cx), 1, "typing keeps the matching line");
    assert!(
        bounds_of(cx, window, "usages-search").size.height > px(0.),
        "the popup shows what was typed"
    );

    press(cx, window, "enter");
    assert_eq!(active_path(cx, window), Some(other.clone()));
    assert_eq!(
        editor_of(cx, window, &other).read_with(cx, |view, _| view.caret()),
        (0, 0)
    );
}

#[ui_kit::gpui::test]
fn escape_closes_the_popup_without_the_window(cx: &mut TestAppContext) {
    let root = project("show-usages-escape", &[("src/Main.txt", "greet\n")]);
    let main = root.join("src").join("Main.txt");
    let language = FakeLanguage::new();
    language.set_references(vec![at(&main, "greet\n", 0, 5), at(&main, "greet\n", 0, 5)]);
    let window = open_window_with(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &main);

    press(cx, window, "ctrl-alt-f7");
    draw(cx, window);
    assert!(popup(cx).is_some());

    press(cx, window, "escape");
    assert!(popup(cx).is_none(), "Escape closes the popup");
    assert!(
        !window
            .read_with(cx, |workspace, _| workspace.tool_open(USAGES_TOOL))
            .unwrap(),
        "Escape does not open the Usages window"
    );
}

#[ui_kit::gpui::test]
fn one_place_jumps_at_once_without_the_popup(cx: &mut TestAppContext) {
    let root = project(
        "show-usages-one",
        &[("src/Main.txt", "greet\n"), ("src/Other.txt", "greet\n")],
    );
    let main = root.join("src").join("Main.txt");
    let other = root.join("src").join("Other.txt");
    let language = FakeLanguage::new();
    language.set_references(vec![at(&other, "greet\n", 0, 5)]);
    let window = open_window_with(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &main);

    press(cx, window, "ctrl-alt-f7");
    draw(cx, window);
    assert_eq!(active_path(cx, window), Some(other.clone()));
    assert_eq!(
        editor_of(cx, window, &other).read_with(cx, |view, _| view.caret()),
        (0, 0)
    );
    assert!(popup(cx).is_none(), "the only place needs no popup");
}

#[ui_kit::gpui::test]
fn no_places_show_the_hint_at_the_caret(cx: &mut TestAppContext) {
    let root = project("show-usages-none", &[("src/Main.txt", "greet\n")]);
    let main = root.join("src").join("Main.txt");
    let language = FakeLanguage::new();
    language.set_references(Vec::new());
    let window = open_window_with(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &main);

    press(cx, window, "ctrl-alt-f7");
    draw(cx, window);
    assert!(popup(cx).is_some(), "the search still opens the popup");
    assert_eq!(visible_rows(cx), 0);
    assert!(done(cx));
    assert!(
        bounds_of(cx, window, "usages-empty").size.height > px(0.),
        "the popup says that there are no usages"
    );
}

#[ui_kit::gpui::test]
fn open_in_find_window_shows_the_same_places_in_the_tool_window(cx: &mut TestAppContext) {
    let root = project(
        "show-usages-window",
        &[("src/Main.txt", "greet\n"), ("src/Other.txt", "greet\n")],
    );
    let main = root.join("src").join("Main.txt");
    let other = root.join("src").join("Other.txt");
    let language = FakeLanguage::new();
    language.set_references(vec![
        at(&main, "greet\n", 0, 5),
        at(&other, "greet\n", 0, 5),
    ]);
    let window = open_window_with(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &main);

    press(cx, window, "ctrl-alt-f7");
    draw(cx, window);
    click(cx, window, "usages-open-window");

    assert!(
        window
            .read_with(cx, |workspace, _| workspace.tool_open(USAGES_TOOL))
            .unwrap(),
        "Open in Find Window shows the Usages window"
    );
    let hub = cx.update(|cx| cx.global::<ActiveUsages>().0.clone());
    assert_eq!(
        hub.read_with(cx, |hub, cx| hub.panel().read(cx).items().len()),
        2
    );
    assert!(popup(cx).is_none(), "the popup gives way to the window");
}

#[ui_kit::gpui::test]
fn a_slow_search_fills_the_popup_after_it_is_shown(cx: &mut TestAppContext) {
    let root = project(
        "show-usages-slow",
        &[("src/Main.txt", "greet\n"), ("src/Other.txt", "greet\n")],
    );
    let main = root.join("src").join("Main.txt");
    let other = root.join("src").join("Other.txt");
    let language = Arc::new(SlowLanguage {
        inner: FakeLanguage::new(),
        spans: vec![
            SourceSpan {
                uri: uri_of(&main),
                span: TextSpan::default(),
            },
            SourceSpan {
                uri: uri_of(&other),
                span: TextSpan::default(),
            },
        ],
        delay: Duration::from_millis(300),
    });
    let window = open_slow_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &main);

    press(cx, window, "ctrl-alt-f7");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "usages-popup").size.height > px(0.),
        "the popup is on screen before the answer"
    );
    assert_eq!(visible_rows(cx), 0, "no places have arrived yet");
    assert!(!done(cx));

    wait_for(cx, "the places of the slow search", |cx| done(cx));
    draw(cx, window);
    assert_eq!(visible_rows(cx), 2, "the rows arrive in the open popup");
}
