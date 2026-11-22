//! T-0410: while the language server searches, the Usages window already has the tab of
//! the symbol and says what it searches; "No usages found." comes only after an answer
//! without results. A usage row marks the name of the symbol, even when the server names
//! the whole invocation.
//!
//! `cargo test -p panels window_tests_usages_searching`

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use editor_view::Languages;
use lang_api::{
    Answer, Confidence, DocumentSpot, LanguageId, LanguageSupport, Provider, Reply, SourceId,
    SyntaxProvider, UsageEntry, UsageKind,
};
use ui_kit::Theme;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::{FileIcons, Launch, Layout, Settings, USAGES_TOOL, Workspace};

use crate::index::SearchSetup;
use crate::usages::ActiveUsages;
use crate::window_tests::{draw, editor_of, open_path, press, project_setup, scratch};
use crate::window_tests_lang::project;
use crate::window_tests_ui::wait_for;
use crate::window_tests_usages_tabs::{UsageLanguage, hub, usage};
use crate::{AgentOptions, LanguageServer, connect, install};

const SOURCE: SourceId = SourceId::new("usages-searching-server");
/// The line of the Usages window when an answer brought no use.
const NO_USAGES: &str = "No usages found.";

/// A language whose usages answer waits until the test sends it.
struct PendingUsages {
    language: Arc<UsageLanguage>,
    replies: Mutex<Vec<Reply<Vec<UsageEntry>>>>,
}

impl PendingUsages {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            language: UsageLanguage::new(),
            replies: Mutex::new(Vec::new()),
        })
    }

    fn answer(&self, entries: Vec<UsageEntry>) {
        let reply = self.replies.lock().unwrap().pop();
        if let Some(mut reply) = reply {
            let _ = reply.send(Answer::new(entries, Confidence::Certain, SOURCE));
        }
    }

    fn pending(&self) -> bool {
        !self.replies.lock().unwrap().is_empty()
    }
}

impl Provider<DocumentSpot, Vec<UsageEntry>> for PendingUsages {
    fn start(&self, _: DocumentSpot, _: base::Cancel, reply: Reply<Vec<UsageEntry>>) {
        self.replies.lock().unwrap().push(reply);
    }
}

impl LanguageSupport for PendingUsages {
    fn id(&self) -> LanguageId {
        LanguageId("usages-searching")
    }

    fn matches(&self, path: &Path) -> bool {
        self.language.matches(path)
    }

    fn syntax(&self) -> &dyn SyntaxProvider {
        self.language.syntax()
    }

    fn usages(&self) -> Option<&lang_api::UsageProvider> {
        Some(self)
    }
}

/// A language that reports its server as still importing, over the pending language.
struct NotReady {
    inner: Arc<PendingUsages>,
}

impl LanguageSupport for NotReady {
    fn id(&self) -> LanguageId {
        self.inner.id()
    }

    fn matches(&self, path: &Path) -> bool {
        self.inner.matches(path)
    }

    fn syntax(&self) -> &dyn SyntaxProvider {
        self.inner.syntax()
    }

    fn usages(&self) -> Option<&lang_api::UsageProvider> {
        self.inner.usages()
    }

    fn server_ready(&self) -> bool {
        false
    }
}

/// A window with the language of the test.
fn open_window_with(
    cx: &mut TestAppContext,
    root: PathBuf,
    language: Arc<dyn LanguageSupport>,
    status_line: &str,
    name: &str,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let state_dir = scratch(name);
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
    let agents = AgentOptions {
        state_dir: launch.state_dir.clone(),
        mock: false,
    };
    let (status_sender, status) = futures_channel::mpsc::unbounded();
    status_sender.unbounded_send(status_line.to_string()).ok();
    let server = LanguageServer {
        support: Some(language),
        status,
    };
    cx.add_window(move |window, cx| {
        let mut workspace =
            Workspace::new_for_test(launch, Layout::default(), store, Vec::new(), window, cx);
        install(
            &mut workspace,
            SearchSetup::default(),
            project_setup(scratch("cache")),
            agents,
            window,
            cx,
        );
        connect(&mut workspace, server, cx);
        workspace
    })
}

fn usages_open(cx: &TestAppContext, window: WindowHandle<Workspace>) -> bool {
    window
        .read_with(cx, |workspace, _| workspace.tool_open(USAGES_TOOL))
        .unwrap()
}

fn empty_line(cx: &TestAppContext) -> String {
    hub(cx).read_with(cx, |hub, cx| hub.panel().read(cx).empty_text())
}

fn tabs(cx: &TestAppContext) -> Vec<String> {
    hub(cx).read_with(cx, |hub, _| hub.tab_names())
}

fn progress_shown(cx: &TestAppContext) -> bool {
    hub(cx).read_with(cx, |hub, cx| hub.panel().read(cx).progress_shown())
}

fn code_lines(cx: &TestAppContext) -> Vec<crate::items::CodeLine> {
    cx.read(|cx| {
        let Some(active) = cx.try_global::<ActiveUsages>() else {
            return Vec::new();
        };
        let panel = active.0.read(cx).panel().clone();
        let panel = panel.read(cx);
        (0..panel.items().len())
            .filter_map(|index| panel.code_line(index).cloned())
            .collect()
    })
}

/// Whether an element with the selector is on screen.
fn visible(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &'static str,
) -> bool {
    let mut visual = ui_kit::gpui::VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

/// The line and the column of a byte offset in a text.
fn place(text: &str, at: usize) -> (usize, usize) {
    let line = text[..at].matches('\n').count();
    let column = text[..at]
        .rfind('\n')
        .map_or(at, |newline| at - newline - 1);
    (line, column)
}

#[ui_kit::gpui::test]
fn a_search_that_has_not_answered_says_what_it_searches(cx: &mut TestAppContext) {
    let name = "findPaginatedForOwnersLastName";
    let text = format!("class Owner {{\n    void {name}(int page) {{}}\n}}\n");
    let root = project("usages-searching-start", &[("src/Main.txt", &text)]);
    let main = root.join("src").join("Main.txt");
    let language = PendingUsages::new();
    let window = open_window_with(
        cx,
        root.clone(),
        language.clone(),
        "Language server ready",
        "usages-searching-start",
    );
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &main);
    let view = editor_of(cx, window, &main);
    let (line, column) = place(&text, text.find(name).unwrap());
    view.update(cx, |view, cx| view.restore_caret(line, column, cx));
    draw(cx, window);

    press(cx, window, "alt-f7");
    wait_for(cx, "the search that waits for its answer", |cx| {
        usages_open(cx, window) && language.pending()
    });
    draw(cx, window);

    assert_eq!(
        tabs(cx),
        vec![name.to_owned()],
        "the tab of the symbol is already there"
    );
    assert_eq!(
        empty_line(cx),
        format!("Searching for usages of {name}…"),
        "the window says what it searches"
    );
    assert_ne!(empty_line(cx), NO_USAGES, "the search has not ended");
    assert!(
        progress_shown(cx),
        "the window marks the search in progress"
    );
    assert!(
        visible(cx, window, "items-progress"),
        "the progress line is on screen"
    );
}

#[ui_kit::gpui::test]
fn the_waiting_line_names_the_server_of_the_language(cx: &mut TestAppContext) {
    let name = "findPaginatedForOwnersLastName";
    let text = format!("class Owner {{\n    void {name}(int page) {{}}\n}}\n");
    let root = project("usages-searching-wait", &[("src/Main.txt", &text)]);
    let main = root.join("src").join("Main.txt");
    let pending = PendingUsages::new();
    let language = Arc::new(NotReady { inner: pending });
    let window = open_window_with(
        cx,
        root.clone(),
        language,
        "Fixture: importing",
        "usages-searching-wait",
    );
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &main);
    let view = editor_of(cx, window, &main);
    let (line, column) = place(&text, text.find(name).unwrap());
    view.update(cx, |view, cx| view.restore_caret(line, column, cx));
    draw(cx, window);

    press(cx, window, "alt-f7");
    wait_for(cx, "the search that waits for the server", |cx| {
        usages_open(cx, window) && empty_line(cx) == "Waiting for the Fixture server…"
    });
    draw(cx, window);

    assert_eq!(
        empty_line(cx),
        "Waiting for the Fixture server…",
        "the line names the server of the language"
    );
    assert!(progress_shown(cx), "the window marks the wait");
    assert_eq!(
        tabs(cx),
        vec![name.to_owned()],
        "the tab of the symbol is already there"
    );
}

#[ui_kit::gpui::test]
fn an_answer_without_usages_says_no_usages_found(cx: &mut TestAppContext) {
    let name = "findPaginatedForOwnersLastName";
    let text = format!("class Owner {{\n    void {name}(int page) {{}}\n}}\n");
    let root = project("usages-searching-none", &[("src/Main.txt", &text)]);
    let main = root.join("src").join("Main.txt");
    let language = PendingUsages::new();
    let window = open_window_with(
        cx,
        root.clone(),
        language.clone(),
        "Language server ready",
        "usages-searching-none",
    );
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &main);
    let view = editor_of(cx, window, &main);
    let (line, column) = place(&text, text.find(name).unwrap());
    view.update(cx, |view, cx| view.restore_caret(line, column, cx));
    draw(cx, window);

    press(cx, window, "alt-f7");
    wait_for(cx, "the search that waits for its answer", |cx| {
        usages_open(cx, window) && language.pending()
    });
    assert_ne!(empty_line(cx), NO_USAGES, "the search has not ended");

    language.answer(Vec::new());
    wait_for(cx, "the answer without usages", |cx| {
        empty_line(cx) == NO_USAGES
    });
    draw(cx, window);

    assert!(!progress_shown(cx), "the ended search has no progress line");
    assert!(
        !visible(cx, window, "items-progress"),
        "the progress line is gone"
    );
    assert_eq!(
        tabs(cx),
        vec![name.to_owned()],
        "the tab keeps the name of the symbol"
    );
}

#[ui_kit::gpui::test]
fn a_range_wider_than_the_name_marks_only_the_name(cx: &mut TestAppContext) {
    let name = "findPaginatedForOwnersLastName";
    let line = "    Page<Owner> ownersResults = findPaginatedForOwnersLastName(page, lastName);\n";
    let text = format!("class OwnerController {{\n{line}}}\n");
    let root = project("usages-searching-range", &[("src/Main.txt", &text)]);
    let main = root.join("src").join("Main.txt");
    let language = PendingUsages::new();
    let window = open_window_with(
        cx,
        root.clone(),
        language.clone(),
        "Language server ready",
        "usages-searching-range",
    );
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &main);
    let view = editor_of(cx, window, &main);
    let (caret_line, column) = place(&text, text.find(name).unwrap());
    view.update(cx, |view, cx| view.restore_caret(caret_line, column, cx));
    draw(cx, window);

    press(cx, window, "alt-f7");
    wait_for(cx, "the search that waits for its answer", |cx| {
        usages_open(cx, window) && language.pending()
    });
    // The server names the whole invocation, arguments included.
    let start = text.find(name).unwrap();
    let end = text.find("lastName);").unwrap() + "lastName);".len();
    language.answer(vec![usage(&main, &text, start, end, UsageKind::Call)]);
    wait_for(cx, "the row of the usage", |cx| !code_lines(cx).is_empty());
    draw(cx, window);

    let code = code_lines(cx).remove(0);
    let trimmed = line.trim();
    let found = trimmed.find(name).expect("the name is in the line");
    assert_eq!(code.text, trimmed);
    assert_eq!(
        code.highlight,
        found..found + name.len(),
        "the match is the name, not the invocation"
    );
}
