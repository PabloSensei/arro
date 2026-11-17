//! T-0224: Analyze Stack Trace answers twice. The text and the frames of the project come
//! first, the places of the libraries after the server has looked them up; the second
//! answer fills the links in without moving the chosen thread.
//!
//! `cargo test -p panels window_tests_stack_refinement`

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base::Cancel;
use editor_view::Semantic;
use lang_api::{
    Answer, Confidence, Frame, FrameOrigin, LanguageFn, LanguageId, LanguageSupport, LineKind,
    Provider, Queries, Reply, Report, SourceId, SourceSpan, StackLine, SyntaxProvider, TextPoint,
    TextSpan, ThreadStack, TraceProvider, TraceQuery,
};
use platform::CanonPath;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::{STACK_TRACE_TOOL, Workspace};

use crate::stack_traces::{ActiveStackTrace, StackTracePanel};
use crate::window_tests::{click, draw, scratch, wait_for_signal};
use crate::window_tests_lang::{FakeLanguage, open_window_with};

const SOURCE: SourceId = SourceId::new("slow-stacks");
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

/// A language whose stack report comes in two answers: the first one at once, the second
/// when the test lets it through, so the window has time to show what it has.
struct SlowStacks {
    syntax: NoSyntax,
    first: Report,
    second: Report,
    release: Mutex<Option<Receiver<()>>>,
    second_sent: Arc<AtomicBool>,
}

impl SlowStacks {
    fn new(first: Report, second: Report) -> (Arc<Self>, mpsc::Sender<()>) {
        let (sender, release) = mpsc::channel();
        let language = Arc::new(Self {
            syntax: NoSyntax(Queries::default()),
            first,
            second,
            release: Mutex::new(Some(release)),
            second_sent: Arc::new(AtomicBool::new(false)),
        });
        (language, sender)
    }

    fn second_sent(&self) -> bool {
        self.second_sent.load(Ordering::Relaxed)
    }
}

impl Provider<TraceQuery, Report> for SlowStacks {
    fn start(&self, _: TraceQuery, _: Cancel, mut reply: Reply<Report>) {
        let first = self.first.clone();
        let second = self.second.clone();
        let sent = self.second_sent.clone();
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
            if reply
                .send(Answer::new(second, Confidence::Certain, SOURCE))
                .is_ok()
            {
                sent.store(true, Ordering::Relaxed);
            }
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

/// One thread of a dump.
fn thread(name: &str, state: &str, lines: Vec<StackLine>) -> ThreadStack {
    ThreadStack {
        name: name.to_owned(),
        state: state.to_owned(),
        lines,
        deadlocked: false,
        waiting_for_lock: false,
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

/// Lets the slide of the dock and the work of the window finish before a click.
fn settle(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    for _ in 0..8 {
        cx.executor().advance_clock(Duration::from_millis(50));
        cx.run_until_parked();
        draw(cx, window);
    }
}

#[ui_kit::gpui::test]
fn the_text_and_the_project_links_come_before_the_second_answer(cx: &mut TestAppContext) {
    let root = project("stack-refine-trace");
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
    let second = Report::Trace {
        lines: vec![
            StackLine::text("boom.Error: boom"),
            frame_line(
                "example.Main",
                "boom",
                1,
                Some(place(&path, 1)),
                FrameOrigin::Project,
            ),
            frame_line(
                "core.Thread",
                "run",
                3,
                Some(place(&path, 3)),
                FrameOrigin::Library,
            ),
        ],
    };
    let (language, release) = SlowStacks::new(first, second);
    let _window = open(cx, root, language.clone());
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
        "the library frame has no place before the second answer"
    );
    assert!(
        !language.second_sent(),
        "the test saw the first answer before the second one"
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
fn the_second_answer_keeps_the_chosen_thread(cx: &mut TestAppContext) {
    let root = project("stack-refine-dump");
    let path = root.join("src/com/example/Main.txt");
    let project_frame = frame_line(
        "example.Main",
        "work",
        1,
        Some(place(&path, 1)),
        FrameOrigin::Project,
    );
    let library_frame = |place| frame_line("core.Thread", "run", 3, place, FrameOrigin::Library);
    let first = Report::Dump {
        threads: vec![
            thread("worker", "BLOCKED", vec![project_frame.clone()]),
            thread("main", "RUNNABLE", vec![library_frame(None)]),
        ],
        deadlock: false,
    };
    let second = Report::Dump {
        threads: vec![
            thread("worker", "BLOCKED", vec![project_frame]),
            thread(
                "main",
                "RUNNABLE",
                vec![library_frame(Some(place(&path, 3)))],
            ),
        ],
        deadlock: false,
    };
    let (language, release) = SlowStacks::new(first, second);
    let window = open(cx, root, language.clone());
    let panel = panel(cx);
    panel.update(cx, |panel, cx| {
        panel.analyze("a thread dump".to_owned(), cx)
    });
    wait_for_signal(cx, "the threads of the dump", |cx| {
        panel.read_with(cx, |panel, _| panel.visible_threads().len() == 2)
    });
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(STACK_TRACE_TOOL, window, cx)
        })
        .unwrap();
    settle(cx, window);

    draw(cx, window);
    click(cx, window, "stack-thread-1");
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.selected_thread()),
        Some("main".to_owned()),
        "the test chose the other thread"
    );

    release.send(()).unwrap();
    wait_for_signal(cx, "the place of the library frame", |cx| {
        panel.read_with(cx, |panel, _| {
            panel.line_marks().contains(&(FrameOrigin::Library, true))
        })
    });
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.selected_thread()),
        Some("main".to_owned()),
        "the second answer keeps the chosen thread"
    );
    let names: Vec<String> = panel
        .read_with(cx, |panel, _| panel.visible_threads())
        .into_iter()
        .map(|(_, name)| name)
        .collect();
    assert_eq!(names, ["worker", "main"], "the order of the list is kept");
}
