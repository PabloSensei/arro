//! The quick documentation popup: Ctrl+Q over a name, the same popup on hover, the
//! links inside it and the setting that turns the hover popup off.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use editor_view::{ExternalLinks, Languages};
use lang_api::{
    Answer, Confidence, DiagnosticEntry, DiagnosticsProvider, DiagnosticsQuery, DocumentSpot,
    HoverProvider, HoverText, LanguageFn, LanguageId, LanguageSupport, Provider, Queries, Severity,
    SourceId, SyntaxProvider, TextPoint, TextSpan,
};
use platform::CanonPath;
use ui_kit::gpui;
use ui_kit::gpui::{Modifiers, TestAppContext, VisualTestContext, WindowHandle};
use ui_kit::{Theme, point, px};
use workspace::{FileIcons, Launch, Layout, Settings, Workspace};

use crate::index::SearchSetup;
use crate::window_tests::{
    active_path, bounds_of, draw, editor_of, line_height, open_path, press, project_setup, scratch,
    wait_for_signal,
};
use crate::window_tests_lang::project;
use crate::{AgentOptions, LanguageServer, connect, install};

const SOURCE: SourceId = SourceId::new("hover-server");

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
struct HoverLanguage {
    syntax: NoSyntax,
    hover: Mutex<String>,
    diagnostics: Mutex<Vec<DiagnosticEntry>>,
}

impl HoverLanguage {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            syntax: NoSyntax(Queries::default()),
            hover: Mutex::new(String::new()),
            diagnostics: Mutex::new(Vec::new()),
        })
    }

    fn set_hover(&self, text: &str) {
        *self.hover.lock().unwrap() = text.to_owned();
    }

    fn set_diagnostics(&self, diagnostics: Vec<DiagnosticEntry>) {
        *self.diagnostics.lock().unwrap() = diagnostics;
    }

    fn answer<T>(&self, value: T) -> Answer<T> {
        Answer::new(value, Confidence::Certain, SOURCE)
    }
}

impl Provider<DocumentSpot, HoverText> for HoverLanguage {
    fn start(&self, _: DocumentSpot, _: base::Cancel, mut reply: lang_api::Reply<HoverText>) {
        let text = self.hover.lock().unwrap().clone();
        let _ = reply.send(self.answer(HoverText(text)));
    }
}

impl Provider<DiagnosticsQuery, Vec<DiagnosticEntry>> for HoverLanguage {
    fn start(
        &self,
        _: DiagnosticsQuery,
        _: base::Cancel,
        mut reply: lang_api::Reply<Vec<DiagnosticEntry>>,
    ) {
        let entries = self.diagnostics.lock().unwrap().clone();
        let _ = reply.send(self.answer(entries));
    }
}

impl LanguageSupport for HoverLanguage {
    fn id(&self) -> LanguageId {
        LanguageId("hover")
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

    fn diagnostics(&self) -> Option<&DiagnosticsProvider> {
        Some(self)
    }
}

/// The window with the fake language behind it.
fn open_window_with(
    cx: &mut TestAppContext,
    root: PathBuf,
    language: Arc<HoverLanguage>,
    settings: Settings,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let state_dir = scratch("state");
    let store = store::Store::open(&state_dir.join("state.sqlite"))
        .ok()
        .map(Arc::new);
    cx.update(|cx| {
        cx.set_global(Theme::dark());
        cx.set_global(settings);
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

/// Puts the pointer over a character of a line and lets `time` pass.
fn hover_editor(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    line: usize,
    column: usize,
    time: Duration,
) {
    let height = line_height(cx);
    let bounds = bounds_of(cx, window, "editor-body-0");
    let x = bounds.left() + px(67. + column as f32 * 8.);
    let y = bounds.top() + px(height * line as f32 + height / 2.);
    move_pointer(cx, window, point(x, y));
    cx.executor().advance_clock(time);
    cx.run_until_parked();
    draw(cx, window);
}

fn move_pointer(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    position: gpui::Point<gpui::Pixels>,
) {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_move(position, None, Modifiers::default());
    drop(visual);
    cx.run_until_parked();
}

fn file_uri(path: &Path) -> String {
    CanonPath::new(path).unwrap().to_uri()
}

fn documentation(link: &str) -> String {
    format!(
        "```text\npublic static String format(String format, Object... args)\n```\n\n\
         Returns a **formatted** string with the `format` string.\n\n\
         **Parameters:**\n`format` - the pattern\n`args` - the values\n\n\
         **Returns:**\nthe text\n\n\
         **Throws:**\n`IllegalState` - a wrong pattern\n\n\
         See [the other file]({link}) for more.\n\n\
         - one\n- two\n"
    )
}

#[ui_kit::gpui::test]
fn ctrl_q_shows_the_formatted_answer_and_a_second_press_closes_it(cx: &mut TestAppContext) {
    let root = project("quick-doc", &[("src/Main.txt", "format call\n")]);
    let main = root.join("src").join("Main.txt");
    let language = HoverLanguage::new();
    language.set_hover(&documentation("https://example.com/docs"));
    let window = open_window_with(cx, root, language, Settings::default());
    cx.run_until_parked();
    draw(cx, window);

    open_path(cx, window, &main);
    click_editor(cx, window, 0, 1);
    press(cx, window, "ctrl-q");
    draw(cx, window);

    assert!(shown(cx, window, "quick-doc"), "Ctrl+Q opens the popup");
    let view = editor_of(cx, window, &main);
    let text = view
        .read_with(cx, |view, _| view.quick_doc_text())
        .expect("the popup has the text of the answer");
    assert!(text.contains("public static String format"), "{text}");
    assert!(text.contains("Parameters"), "{text}");
    assert!(!text.contains("**"), "the bold markers are gone: {text}");
    assert!(!text.contains("]("), "the link markers are gone: {text}");
    assert!(!text.contains("```"), "the code fences are gone: {text}");

    press(cx, window, "ctrl-q");
    draw(cx, window);
    assert!(
        !shown(cx, window, "quick-doc"),
        "the second Ctrl+Q closes the popup"
    );

    press(cx, window, "ctrl-q");
    draw(cx, window);
    assert!(shown(cx, window, "quick-doc"));
    press(cx, window, "escape");
    draw(cx, window);
    assert!(!shown(cx, window, "quick-doc"), "Escape closes it");
}

#[ui_kit::gpui::test]
fn a_file_link_opens_the_file_at_its_line(cx: &mut TestAppContext) {
    let root = project(
        "quick-doc-link",
        &[
            ("src/Main.txt", "format call\n"),
            ("src/Other.txt", "one\ntwo\nthree\nfour\nfive\nsix\n"),
        ],
    );
    let main = root.join("src").join("Main.txt");
    let other = root.join("src").join("Other.txt");
    let language = HoverLanguage::new();
    language.set_hover(&documentation(&format!("{}#L5", file_uri(&other))));
    let window = open_window_with(cx, root, language, Settings::default());
    cx.run_until_parked();
    draw(cx, window);

    open_path(cx, window, &main);
    click_editor(cx, window, 0, 1);
    press(cx, window, "ctrl-q");
    draw(cx, window);

    let link = bounds_of(cx, window, "quick-doc-link-0-0");
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_click(link.center(), Modifiers::default());
    cx.run_until_parked();
    draw(cx, window);

    assert_eq!(
        active_path(cx, window),
        Some(other.clone()),
        "the link opens the file"
    );
    let view = editor_of(cx, window, &other);
    assert_eq!(
        view.read_with(cx, |view, _| view.caret()),
        (4, 0),
        "the caret is on the fifth line of the link"
    );
}

#[ui_kit::gpui::test]
fn a_web_link_opens_in_the_browser(cx: &mut TestAppContext) {
    let root = project("quick-doc-web", &[("src/Main.txt", "format call\n")]);
    let main = root.join("src").join("Main.txt");
    let language = HoverLanguage::new();
    language.set_hover(&documentation("https://example.com/docs"));
    let opened = Arc::new(Mutex::new(Vec::new()));
    let seen = opened.clone();
    cx.update(|cx| {
        cx.set_global(ExternalLinks(Some(Arc::new(move |url: &str| {
            seen.lock().unwrap().push(url.to_owned());
        }))));
    });
    let window = open_window_with(cx, root, language, Settings::default());
    cx.run_until_parked();
    draw(cx, window);

    open_path(cx, window, &main);
    click_editor(cx, window, 0, 1);
    press(cx, window, "ctrl-q");
    draw(cx, window);

    let link = bounds_of(cx, window, "quick-doc-link-0-0");
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_click(link.center(), Modifiers::default());
    cx.run_until_parked();
    assert_eq!(
        opened.lock().unwrap().as_slice(),
        ["https://example.com/docs"],
        "a web link goes to the browser"
    );
}

#[ui_kit::gpui::test]
fn hover_shows_the_popup_after_the_delay_and_leaving_closes_it(cx: &mut TestAppContext) {
    let root = project("quick-doc-hover", &[("src/Main.txt", "alpha beta\n")]);
    let main = root.join("src").join("Main.txt");
    let language = HoverLanguage::new();
    language.set_hover(&documentation("https://example.com/docs"));
    let window = open_window_with(cx, root, language, Settings::default());
    cx.run_until_parked();
    draw(cx, window);

    open_path(cx, window, &main);
    hover_editor(cx, window, 0, 1, Duration::from_millis(300));
    assert!(
        !shown(cx, window, "quick-doc"),
        "the popup waits longer than a moment"
    );

    hover_editor(cx, window, 0, 1, Duration::from_millis(400));
    assert!(
        shown(cx, window, "quick-doc"),
        "the popup comes after the delay"
    );

    let popup = bounds_of(cx, window, "quick-doc");
    move_pointer(cx, window, popup.center());
    cx.executor().advance_clock(Duration::from_millis(400));
    cx.run_until_parked();
    draw(cx, window);
    assert!(
        shown(cx, window, "quick-doc"),
        "the popup stays while the pointer is over it"
    );

    let bounds = bounds_of(cx, window, "editor-body-0");
    move_pointer(
        cx,
        window,
        point(bounds.left() + px(4.), bounds.top() - px(40.)),
    );
    cx.executor().advance_clock(Duration::from_millis(400));
    cx.run_until_parked();
    draw(cx, window);
    assert!(
        !shown(cx, window, "quick-doc"),
        "leaving the name and the popup closes it"
    );
}

#[ui_kit::gpui::test]
fn a_problem_under_the_pointer_comes_before_the_documentation(cx: &mut TestAppContext) {
    let root = project("quick-doc-problem", &[("src/Main.txt", "alpha beta\n")]);
    let main = root.join("src").join("Main.txt");
    let language = HoverLanguage::new();
    language.set_hover(&documentation("https://example.com/docs"));
    language.set_diagnostics(vec![DiagnosticEntry {
        span: TextSpan {
            start: TextPoint {
                line: 0,
                character: 0,
            },
            end: TextPoint {
                line: 0,
                character: 5,
            },
        },
        severity: Severity::Error,
        message: "alpha is broken".to_owned(),
        unnecessary: false,
    }]);
    let window = open_window_with(cx, root, language, Settings::default());
    cx.run_until_parked();
    draw(cx, window);

    open_path(cx, window, &main);
    let view = editor_of(cx, window, &main);
    wait_for_signal(cx, "the problem", |cx| {
        !view
            .read_with(cx, |view, _| view.diagnostic_marks())
            .is_empty()
    });
    click_editor(cx, window, 0, 1);
    press(cx, window, "ctrl-q");
    draw(cx, window);

    let text = view
        .read_with(cx, |view, _| view.quick_doc_text())
        .expect("the popup shows");
    let problem = text.find("alpha is broken").expect("the problem is shown");
    let signature = text
        .find("public static String format")
        .expect("the signature is shown");
    assert!(problem < signature, "the problem comes first: {text}");
}

#[ui_kit::gpui::test]
fn the_setting_off_keeps_the_hover_popup_away(cx: &mut TestAppContext) {
    let root = project("quick-doc-off", &[("src/Main.txt", "alpha beta\n")]);
    let main = root.join("src").join("Main.txt");
    let language = HoverLanguage::new();
    language.set_hover(&documentation("https://example.com/docs"));
    let settings = Settings {
        quick_doc_on_hover: false,
        ..Settings::default()
    };
    let window = open_window_with(cx, root, language, settings);
    cx.run_until_parked();
    draw(cx, window);

    open_path(cx, window, &main);
    hover_editor(cx, window, 0, 1, Duration::from_millis(800));
    assert!(
        !shown(cx, window, "quick-doc"),
        "the setting turns the hover popup off"
    );
}

#[ui_kit::gpui::test]
#[ignore = "needs IDE_CORPUS and a filled package cache"]
fn a_real_server_documents_a_platform_method_on_ctrl_q(cx: &mut TestAppContext) {
    let started = std::time::Instant::now();
    let (root, language) = crate::window_tests_real::project("quick-doc-real");
    let sample = window_language::write_documentation_sample(&root);
    let window = crate::window_tests_real::build_window(
        cx,
        root.clone(),
        &language,
        project_setup(scratch("cache")),
    );
    cx.run_until_parked();
    draw(cx, window);
    crate::window_tests_real::wait_ready(cx, window);

    open_path(cx, window, &sample);
    let view = editor_of(cx, window, &sample);
    let (line, column) = view.read_with(cx, |view, _| {
        (0..12)
            .find_map(|line| {
                let text = view.line_text(line)?;
                let column = text.find("format")? + 1;
                Some((line, column))
            })
            .expect("the sample calls the formatter")
    });
    view.update(cx, |view, cx| view.restore_caret(line, column, cx));
    press(cx, window, "ctrl-q");
    crate::window_tests_real::wait_for(
        cx,
        "the documentation of the method",
        Duration::from_secs(60),
        |cx| view.read_with(cx, |view, _| view.quick_doc_text().is_some()),
    );
    let text = view
        .read_with(cx, |view, _| view.quick_doc_text())
        .expect("the popup has the answer");
    assert!(text.contains("format"), "{text}");
    assert!(
        text.to_lowercase().contains("parameter"),
        "the parameters section is there: {text}"
    );
    crate::window_tests_real::finish("quick-doc-real", started);
}
