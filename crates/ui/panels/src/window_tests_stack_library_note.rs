//! T-0307: Analyze Stack Trace answers at once with the frames of the project and fills
//! the frames of the libraries in when the server answers. A server that is not ready
//! leaves those frames as they are and says so under the lines.
//!
//! `cargo test -p panels window_tests_stack_library_note`

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base::Cancel;
use editor_view::Semantic;
use lang_api::{
    Answer, Confidence, Frame, FrameOrigin, LanguageFn, LanguageId, LanguageSupport, LineKind,
    Provider, Queries, Reply, Report, SourceId, SourceSpan, StackLine, SyntaxProvider, TextPoint,
    TextSpan, TraceProvider, TraceQuery,
};
use platform::CanonPath;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::{STACK_TRACE_TOOL, Workspace};

use crate::stack_traces::{ActiveStackTrace, StackTracePanel};
use crate::window_tests::{bounds_of, draw, scratch, wait_for_signal};
use crate::window_tests_lang::{FakeLanguage, open_window_with};

const SOURCE: SourceId = SourceId::new("slow-stacks");
/// The line the language adds when the server that knows the libraries is not there.
const NOTE: &str = "Library frames are not resolved: the language server is not ready";
/// A project with one file of the test: the places below point into it.
const TEXT: &str = "public class Main {\n    static void boom() {\n    }\n}\n";

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

/// The second answer of the stand-in server.
enum Second {
    /// The places of the library frames, as a server that answered them.
    Places(Report),
    /// The same frames and the remark of a server that is not ready.
    Note(Report),
}

/// A language whose stack report comes in two answers: the first one at once, the second
/// when the test lets it through, so the window has time to show what it has.
struct SlowStacks {
    syntax: NoSyntax,
    first: Report,
    second: Second,
    release: Mutex<Option<Receiver<()>>>,
}

impl SlowStacks {
    fn new(first: Report, second: Second) -> (Arc<Self>, mpsc::Sender<()>) {
        let (sender, release) = mpsc::channel();
        let language = Arc::new(Self {
            syntax: NoSyntax(Queries::default()),
            first,
            second,
            release: Mutex::new(Some(release)),
        });
        (language, sender)
    }
}

impl Provider<TraceQuery, Report> for SlowStacks {
    fn start(&self, _: TraceQuery, _: Cancel, mut reply: Reply<Report>) {
        let first = self.first.clone();
        let second = match &self.second {
            Second::Places(report) => Answer::new(report.clone(), Confidence::Certain, SOURCE),
            Second::Note(report) => {
                Answer::new(report.clone(), Confidence::Partial, SOURCE).with_note(NOTE)
            }
        };
        let release = self
            .release
            .lock()
            .unwrap()
            .take()
            .expect("the test asks the language once");
        std::thread::spawn(move || {
            let _ = reply.send(Answer::new(first, Confidence::Partial, SOURCE));
            match release.recv_timeout(Duration::from_secs(10)) {
                Ok(()) | Err(RecvTimeoutError::Disconnected) => {}
                Err(RecvTimeoutError::Timeout) => return,
            }
            let _ = reply.send(second);
        });
    }
}

impl LanguageSupport for SlowStacks {
    fn id(&self) -> LanguageId {
        LanguageId("slow-stacks")
    }

    fn matches(&self, _: &Path) -> bool {
        false
    }

    fn syntax(&self) -> &dyn SyntaxProvider {
        &self.syntax
    }

    fn stack_traces(&self) -> Option<&TraceProvider> {
        Some(self)
    }
}

fn project(name: &str) -> PathBuf {
    let root = scratch(name);
    let folder = root.join("src/com/example");
    fs::create_dir_all(&folder).unwrap();
    fs::write(folder.join("Main.txt"), TEXT).unwrap();
    root
}

fn place(path: &Path, line: usize) -> SourceSpan {
    let point = TextPoint {
        line: line as u32,
        character: 0,
    };
    SourceSpan {
        uri: CanonPath::new(path).unwrap().to_uri(),
        span: TextSpan {
            start: point,
            end: point,
        },
    }
}

fn frame_line(
    class: &str,
    method: &str,
    line: usize,
    place: Option<SourceSpan>,
    origin: FrameOrigin,
) -> StackLine {
    StackLine {
        text: format!("\tat {class}.{method}(Main.txt:{})", line + 1),
        kind: LineKind::Frame,
        frame: Some(Frame {
            class: class.to_owned(),
            method: method.to_owned(),
            file: Some("Main.txt".to_owned()),
            line: Some(line),
            place,
            origin,
        }),
    }
}

fn open(
    cx: &mut TestAppContext,
    root: PathBuf,
    language: Arc<SlowStacks>,
) -> WindowHandle<Workspace> {
    let window = open_window_with(cx, root, FakeLanguage::new());
    let support: Arc<dyn LanguageSupport> = language;
    cx.update(|cx| cx.set_global(Semantic(support)));
    window
}

fn panel(cx: &TestAppContext) -> gpui::Entity<StackTracePanel> {
    cx.read(|cx| {
        cx.try_global::<ActiveStackTrace>()
            .map(|panel| panel.0.clone())
    })
    .expect("the Stack Trace tool window")
}

/// Lets the slide of the dock and the work of the window finish before the window is
/// asked what it shows.
fn settle(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    for _ in 0..8 {
        cx.executor().advance_clock(Duration::from_millis(50));
        cx.run_until_parked();
        draw(cx, window);
    }
}

/// The report of a trace whose second frame is a class of a library.
fn reports(root: &Path) -> (Report, Report) {
    let path = root.join("src/com/example/Main.txt");
    let first = Report::Trace {
        lines: vec![
            StackLine::text("boom.Error: boom"),
            frame_line(
                "example.Main",
                "boom",
                1,
                Some(place(&path, 1)),
                FrameOrigin::Project,
            ),
            frame_line("core.Thread", "run", 3, None, FrameOrigin::Library),
        ],
    };
    let mut second = first.clone();
    let Report::Trace { lines } = &mut second else {
        panic!("a trace");
    };
    lines[2] = frame_line(
        "core.Thread",
        "run",
        3,
        Some(place(&path, 3)),
        FrameOrigin::Library,
    );
    (first, second)
}

#[ui_kit::gpui::test]
fn the_library_frames_arrive_after_the_frames_of_the_project(cx: &mut TestAppContext) {
    let root = project("stack-library-places");
    let (first, second) = reports(&root);
    let (language, release) = SlowStacks::new(first, Second::Places(second));
    let _window = open(cx, root, language);
    let panel = panel(cx);
    panel.update(cx, |panel, cx| {
        panel.analyze(
            "boom.Error: boom\n\tat example.Main.boom(Main.txt:2)".to_owned(),
            cx,
        )
    });
    wait_for_signal(cx, "the first answer of the stack", |cx| {
        panel.read_with(cx, |panel, _| panel.shown_lines().len() == 3)
    });

    let marks = panel.read_with(cx, |panel, _| panel.line_marks());
    assert_eq!(marks[1], (FrameOrigin::Project, true), "the project frame");
    assert_eq!(
        marks[2],
        (FrameOrigin::Library, false),
        "the library frame has no place before the answer of the server"
    );
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.note().map(str::to_owned)),
        None,
        "the report has no remark"
    );

    release.send(()).unwrap();
    wait_for_signal(cx, "the place of the library frame", |cx| {
        panel.read_with(cx, |panel, _| {
            panel.line_marks()[2] == (FrameOrigin::Library, true)
        })
    });
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.shown_lines())[1],
        "\tat example.Main.boom(Main.txt:2)",
        "the lines keep their text"
    );
}

#[ui_kit::gpui::test]
fn a_server_that_is_not_ready_keeps_the_project_frames_and_says_why(cx: &mut TestAppContext) {
    let root = project("stack-library-note");
    let (first, _) = reports(&root);
    let (language, release) = SlowStacks::new(first.clone(), Second::Note(first));
    let window = open(cx, root, language);
    let panel = panel(cx);
    panel.update(cx, |panel, cx| {
        panel.analyze(
            "boom.Error: boom\n\tat example.Main.boom(Main.txt:2)".to_owned(),
            cx,
        )
    });
    wait_for_signal(cx, "the first answer of the stack", |cx| {
        panel.read_with(cx, |panel, _| panel.shown_lines().len() == 3)
    });

    release.send(()).unwrap();
    wait_for_signal(cx, "the remark about the library frames", |cx| {
        panel.read_with(cx, |panel, _| panel.note() == Some(NOTE))
    });

    let marks = panel.read_with(cx, |panel, _| panel.line_marks());
    assert_eq!(
        marks[1],
        (FrameOrigin::Project, true),
        "the frames of the project keep their links"
    );
    assert_eq!(
        marks[2],
        (FrameOrigin::Library, false),
        "the library frame stays without a place"
    );

    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(STACK_TRACE_TOOL, window, cx)
        })
        .unwrap();
    settle(cx, window);
    draw(cx, window);
    bounds_of(cx, window, "stack-note");
}
