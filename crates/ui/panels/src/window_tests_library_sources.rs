//! The line above a tab of a class of a library, with a stand-in language and a local
//! server that plays the repository. Nothing leaves the machine.

use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ::base::Cancel;
use editor_view::Languages;
use lang_api::{
    Answer, Confidence, LanguageFn, LanguageId, LanguageSupport, LibraryClass,
    LibraryClassProvider, LibraryClassQuery, NavigationProvider, NavigationQuery, Provider,
    Queries, Reply, SourceId, SourceSpan, SourceTextProvider, SourceTextQuery, SourcesEvent,
    SourcesKind, SourcesProvider, SourcesRequest, SyntaxProvider, TextPoint, TextSpan,
};
use ui_kit::Theme;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use workspace::{FileIcons, Launch, Layout, Settings, Workspace};

use crate::index::SearchSetup;
use crate::run::ProjectSetup;
use crate::window_tests::{
    active_path, buffer_of, click, draw, editor_of, open_path, press, project_setup, scratch,
};
use crate::window_tests_lang::project;
use crate::window_tests_real::wait_for;
use crate::{AgentOptions, LanguageServer, connect, install};

const SOURCE: SourceId = SourceId::new("stand-in");
const URI: &str = "jdt://contents/demo-1.0.jar/com/example/Demo.class";
const SIGNATURE: &str = "public class Demo {\n  public int answer();\n}\n";
const SOURCES: &str = "package com.example;\n\n/** SOURCE-MARKER */\npublic class Demo {\n  public int answer() { return 42; }\n}\n";

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

/// A repository on the loopback: it serves the sources of `demo-1.0` until the test
/// asks it to answer 404.
struct Repository {
    address: String,
    missing: Arc<AtomicBool>,
    requests: Arc<AtomicUsize>,
}

impl Repository {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let port = listener.local_addr().expect("the port").port();
        let missing = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(AtomicUsize::new(0));
        let (missing_flag, seen) = (missing.clone(), requests.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else {
                    continue;
                };
                let mut request = [0u8; 2048];
                let read = stream.read(&mut request).unwrap_or(0);
                let request = String::from_utf8_lossy(&request[..read]).into_owned();
                seen.fetch_add(1, Ordering::SeqCst);
                let found = request.contains("demo-1.0-sources.jar")
                    && !missing_flag.load(Ordering::SeqCst);
                let (status, body) = if found {
                    ("200 OK", SOURCES.as_bytes())
                } else {
                    ("404 Not Found", b"not here".as_slice())
                };
                let head = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(body);
            }
        });
        Self {
            address: format!("http://127.0.0.1:{port}"),
            missing,
            requests,
        }
    }
}

/// The language of the window tests here: one class of a library, its text and the
/// sources a repository would give.
struct FakeLibrary {
    syntax: NoSyntax,
    definition: Mutex<Option<SourceSpan>>,
    repository: String,
    destination: PathBuf,
    attached: Arc<AtomicBool>,
    requests: AtomicUsize,
    chosen: Mutex<Option<PathBuf>>,
}

impl FakeLibrary {
    fn new(repository: String, destination: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            syntax: NoSyntax(Queries::default()),
            definition: Mutex::new(None),
            repository,
            destination,
            attached: Arc::new(AtomicBool::new(false)),
            requests: AtomicUsize::new(0),
            chosen: Mutex::new(None),
        })
    }

    fn set_definition(&self, line: u32, character: u32) {
        *self.definition.lock().unwrap() = Some(SourceSpan {
            uri: URI.to_owned(),
            span: TextSpan {
                start: TextPoint { line, character },
                end: TextPoint { line, character },
            },
        });
    }

    fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }

    fn chosen(&self) -> Option<PathBuf> {
        self.chosen.lock().unwrap().clone()
    }
}

impl Provider<NavigationQuery, Vec<SourceSpan>> for FakeLibrary {
    fn start(&self, _: NavigationQuery, _: Cancel, mut reply: Reply<Vec<SourceSpan>>) {
        let definition = self.definition.lock().unwrap().clone();
        let answer = definition.map_or_else(
            || Answer::new(Vec::new(), Confidence::Unknown, SOURCE),
            |span| Answer::new(vec![span], Confidence::Certain, SOURCE),
        );
        let _ = reply.send(answer);
    }
}

impl Provider<SourceTextQuery, String> for FakeLibrary {
    fn start(&self, _: SourceTextQuery, _: Cancel, mut reply: Reply<String>) {
        let text = if self.attached.load(Ordering::SeqCst) {
            match self.chosen.lock().unwrap().clone() {
                Some(path) => fs::read_to_string(path).unwrap_or_else(|_| SOURCES.to_owned()),
                None => SOURCES.to_owned(),
            }
        } else {
            SIGNATURE.to_owned()
        };
        let _ = reply.send(Answer::new(text, Confidence::Certain, SOURCE));
    }
}

impl Provider<LibraryClassQuery, Option<LibraryClass>> for FakeLibrary {
    fn start(&self, _: LibraryClassQuery, _: Cancel, mut reply: Reply<Option<LibraryClass>>) {
        let class = LibraryClass {
            artifact: "demo-1.0".to_owned(),
            detail: None,
            has_source: self.attached.load(Ordering::SeqCst),
        };
        let _ = reply.send(Answer::new(Some(class), Confidence::Certain, SOURCE));
    }
}

impl Provider<SourcesRequest, SourcesEvent> for FakeLibrary {
    fn start(&self, request: SourcesRequest, _: Cancel, mut reply: Reply<SourcesEvent>) {
        match request.kind {
            SourcesKind::Class(_) => {
                let url = format!(
                    "{}/com/example/demo/1.0/demo-1.0-sources.jar",
                    self.repository
                );
                self.requests.fetch_add(1, Ordering::SeqCst);
                let attached = self.attached.clone();
                let destination = self.destination.clone();
                std::thread::spawn(move || {
                    let _ = reply.send(Answer::new(
                        SourcesEvent::Started { total: 1 },
                        Confidence::Certain,
                        SOURCE,
                    ));
                    let prepared = http_client::PreparedRequest {
                        name: None,
                        method: http_client::Method::Get,
                        url,
                        headers: Vec::new(),
                        body: None,
                        line: 0,
                    };
                    let answer = http_client::execute(
                        &prepared,
                        Duration::from_secs(10),
                        &http_client::Cancel::new(),
                    );
                    let outcome = match answer {
                        Ok(response) if response.status == 200 => {
                            if let Some(parent) = destination.parent() {
                                let _ = fs::create_dir_all(parent);
                            }
                            fs::write(&destination, &response.body).map(|()| true)
                        }
                        Ok(response) if response.status == 404 => Ok(false),
                        _ => Err(std::io::Error::other("the server did not answer")),
                    };
                    match outcome {
                        Ok(true) => {
                            attached.store(true, Ordering::SeqCst);
                            let _ = reply.send(Answer::new(
                                SourcesEvent::Progress {
                                    done: 1,
                                    total: 1,
                                    name: "demo-1.0".to_owned(),
                                },
                                Confidence::Certain,
                                SOURCE,
                            ));
                            let _ = reply.send(Answer::new(
                                SourcesEvent::Attached {
                                    uri: URI.to_owned(),
                                },
                                Confidence::Certain,
                                SOURCE,
                            ));
                        }
                        Ok(false) => {
                            let _ = reply.send(Answer::new(
                                SourcesEvent::Missing {
                                    name: "demo-1.0".to_owned(),
                                },
                                Confidence::Certain,
                                SOURCE,
                            ));
                        }
                        Err(error) => {
                            let _ = reply.send(Answer::new(
                                SourcesEvent::Failed {
                                    message: error.to_string(),
                                },
                                Confidence::Certain,
                                SOURCE,
                            ));
                        }
                    }
                });
            }
            SourcesKind::Attach { path, .. } => {
                *self.chosen.lock().unwrap() = Some(path);
                self.attached.store(true, Ordering::SeqCst);
                let _ = reply.send(Answer::new(
                    SourcesEvent::Attached {
                        uri: URI.to_owned(),
                    },
                    Confidence::Certain,
                    SOURCE,
                ));
            }
            SourcesKind::All => {
                let _ = reply.send(Answer::new(
                    SourcesEvent::Summary {
                        fetched: 0,
                        skipped: 0,
                        failed: 0,
                    },
                    Confidence::Certain,
                    SOURCE,
                ));
            }
        }
    }
}

impl LanguageSupport for FakeLibrary {
    fn id(&self) -> LanguageId {
        LanguageId("fake-library")
    }

    fn matches(&self, path: &Path) -> bool {
        path.extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("txt"))
    }

    fn syntax(&self) -> &dyn SyntaxProvider {
        &self.syntax
    }

    fn navigation(&self) -> Option<&NavigationProvider> {
        Some(self)
    }

    fn source_text(&self) -> Option<&SourceTextProvider> {
        Some(self)
    }

    fn library_class(&self) -> Option<&LibraryClassProvider> {
        Some(self)
    }

    fn library_sources(&self) -> Option<&SourcesProvider> {
        Some(self)
    }
}

fn open_window(
    cx: &mut TestAppContext,
    root: PathBuf,
    language: Arc<FakeLibrary>,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let state_dir = scratch("library-state");
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
    let setup: ProjectSetup = project_setup(scratch("library-cache"));
    let agents = AgentOptions {
        state_dir: launch.state_dir.clone(),
        mock: false,
    };
    let (status_sender, status) = futures_channel::mpsc::unbounded();
    status_sender.unbounded_send("Ready".to_string()).ok();
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
/// Opens the sample, jumps into the class of the library and returns its tab path.
fn library_tab(
    cx: &mut TestAppContext,
    name: &str,
    language: &Arc<FakeLibrary>,
) -> (WindowHandle<Workspace>, PathBuf, PathBuf) {
    let root = project(name, &[("src/Main.txt", "demo();\n")]);
    let main = root.join("src/Main.txt");
    let window = open_window(cx, root.clone(), language.clone());
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &main);
    press(cx, window, "ctrl-b");
    draw(cx, window);
    let tab = active_path(cx, window).expect("the class opens in a tab");
    assert_ne!(tab, main, "another tab shows the class");
    (window, tab, root)
}

#[ui_kit::gpui::test]
fn a_class_without_sources_shows_the_line_and_download_fills_the_tab(cx: &mut TestAppContext) {
    let repository = Repository::start();
    let destination = scratch("library-download").join("demo-1.0-sources.jar");
    let language = FakeLibrary::new(repository.address.clone(), destination.clone());
    language.set_definition(1, 14);
    let (window, tab, _root) = library_tab(cx, "library-download", &language);

    assert!(
        shown(cx, window, "library-sources-bar"),
        "the line is there"
    );
    assert!(
        shown(cx, window, "library-sources-download"),
        "the line offers Download Sources"
    );
    assert!(
        shown(cx, window, "library-sources-choose"),
        "the line offers Choose Sources"
    );
    assert_eq!(
        cx.update(|cx| crate::library_sources::bar_text_at(&tab, cx)),
        Some("Sources not found for demo-1.0".to_owned())
    );
    let view = editor_of(cx, window, &tab);
    assert_eq!(buffer_of(cx, &view), SIGNATURE);
    assert_eq!(
        view.read_with(cx, |view, _| view.caret()),
        (1, 14),
        "the jump put the caret on the symbol of the class"
    );

    click(cx, window, "library-sources-download");
    wait_for(cx, "the sources to arrive", Duration::from_secs(20), |cx| {
        cx.update(|cx| crate::library_sources::bar_text_at(&tab, cx))
            .is_none()
    });
    draw(cx, window);

    assert_eq!(language.requests(), 1, "the repository was asked once");
    assert_eq!(
        repository.requests.load(Ordering::SeqCst),
        1,
        "the local server saw one request"
    );
    assert!(destination.is_file(), "the file is on disk");
    assert!(
        !shown(cx, window, "library-sources-bar"),
        "the line goes away"
    );
    assert!(
        buffer_of(cx, &view).contains("SOURCE-MARKER"),
        "the tab shows the text of the sources"
    );
    assert_eq!(
        view.read_with(cx, |view, _| view.caret()),
        (4, 13),
        "the caret stays on the same symbol of the class"
    );
}

#[ui_kit::gpui::test]
fn a_missing_file_says_so_in_the_line(cx: &mut TestAppContext) {
    let repository = Repository::start();
    repository.missing.store(true, Ordering::SeqCst);
    let destination = scratch("library-missing").join("demo-1.0-sources.jar");
    let language = FakeLibrary::new(repository.address.clone(), destination);
    language.set_definition(0, 0);
    let (window, tab, _root) = library_tab(cx, "library-missing", &language);

    click(cx, window, "library-sources-download");
    wait_for(
        cx,
        "the 404 to reach the line",
        Duration::from_secs(20),
        |cx| {
            cx.update(|cx| crate::library_sources::bar_text_at(&tab, cx))
                .is_some_and(|text| text.contains("404"))
        },
    );
    draw(cx, window);
    assert!(shown(cx, window, "library-sources-bar"));
    assert!(
        shown(cx, window, "library-sources-download"),
        "the line offers the download again"
    );
    assert_eq!(language.requests(), 1, "the repository was asked once");
    let view = editor_of(cx, window, &tab);
    assert_eq!(buffer_of(cx, &view), SIGNATURE, "the text did not change");
}

#[ui_kit::gpui::test]
fn choose_sources_attaches_the_picked_file(cx: &mut TestAppContext) {
    let repository = Repository::start();
    let destination = scratch("library-choose").join("demo-1.0-sources.jar");
    let language = FakeLibrary::new(repository.address.clone(), destination);
    let chosen = scratch("library-chosen").join("Demo.txt");
    fs::create_dir_all(chosen.parent().unwrap()).unwrap();
    fs::write(&chosen, "public class Demo { /* CHOSEN-MARKER */ }\n").unwrap();
    language.set_definition(0, 0);
    let (window, tab, _root) = library_tab(cx, "library-choose", &language);

    click(cx, window, "library-sources-choose");
    assert!(cx.did_prompt_for_paths(), "Choose asks for a path");
    let picked = chosen.clone();
    cx.simulate_path_prompt_response(move |_| Some(vec![picked.clone()]));
    wait_for(
        cx,
        "the chosen sources to arrive",
        Duration::from_secs(20),
        |cx| {
            cx.update(|cx| crate::library_sources::bar_text_at(&tab, cx))
                .is_none()
        },
    );
    draw(cx, window);

    assert_eq!(language.chosen(), Some(chosen), "the file is remembered");
    let view = editor_of(cx, window, &tab);
    assert!(
        buffer_of(cx, &view).contains("CHOSEN-MARKER"),
        "the tab shows the chosen sources"
    );
}

/// The real server on a project whose dependency has no sources in the local repository.
/// The sources come from a loopback repository; the body of the method must appear in
/// the tab.
#[cfg(windows)]
#[ui_kit::gpui::test]
#[ignore = "needs IDE_CORPUS and a filled package cache"]
fn real_server_fetches_the_sources_of_a_dependency(cx: &mut TestAppContext) {
    use std::time::Instant;

    use window_language::{RealLanguage, SourceFixture};

    use crate::window_tests_real::{build_window, close, finish, wait_ready};

    let started = Instant::now();
    let fixture = SourceFixture::new("real-sources");
    let main = fixture.main.clone();
    let root = fixture.root.clone();
    let language = RealLanguage::open(root.clone(), scratch("real-sources-state"))
        .expect("the language server starts");
    let window = build_window(cx, root, &language, project_setup(scratch("cache")));
    cx.run_until_parked();
    draw(cx, window);
    wait_ready(cx, window);

    open_path(cx, window, &main);
    let view = editor_of(cx, window, &main);
    view.update(cx, |view, cx| view.restore_caret(4, 19, cx));
    draw(cx, window);
    press(cx, window, "ctrl-b");
    wait_for(
        cx,
        "the class of the library",
        Duration::from_secs(120),
        |cx| active_path(cx, window) != Some(main.clone()),
    );
    let tab = active_path(cx, window).expect("the class opens");
    wait_for(
        cx,
        "the line above the class",
        Duration::from_secs(120),
        |cx| {
            cx.update(|cx| crate::library_sources::bar_text_at(&tab, cx))
                .is_some_and(|text| text.contains("Sources not found for lib-1.0"))
        },
    );
    draw(cx, window);
    click(cx, window, "library-sources-download");
    wait_for(
        cx,
        "the sources in the tab",
        Duration::from_secs(120),
        |cx| {
            let view = editor_of(cx, window, &tab);
            buffer_of(cx, &view).contains("SOURCE-MARKER")
        },
    );
    let view = editor_of(cx, window, &tab);
    assert!(
        buffer_of(cx, &view).contains("return 42;"),
        "the tab shows the body of the method"
    );
    assert!(
        cx.update(|cx| crate::library_sources::bar_text_at(&tab, cx))
            .is_none(),
        "the line goes away"
    );
    assert!(
        fixture
            .repository_dir()
            .join("com/example/lib/1.0/lib-1.0-sources.jar")
            .is_file(),
        "the sources went next to the jar of the copy"
    );

    close(cx, window, language, false);
    drop(fixture);
    finish("real_server_fetches_the_sources_of_a_dependency", started);
}
