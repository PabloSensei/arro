//! Analyze Stack Trace, the thread dump of a running program and Compare with Clipboard,
//! with a stand-in language that answers from memory. Nothing leaves the machine.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use base::Cancel;
use editor_view::Semantic;
use lang_api::{
    Answer, Confidence, DumpAnswer, DumpProvider, DumpQuery, Frame, FrameOrigin, LanguageFn,
    LanguageId, LanguageSupport, LineKind, Provider, Queries, Reply, Report, SourceId, SourceSpan,
    StackLine, SyntaxProvider, TextPoint, TextSpan, ThreadStack, TraceProvider, TraceQuery,
};
use platform::CanonPath;
use ui_kit::ClipboardItem;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::{RUN_TOOL, STACK_TRACE_TOOL, Workspace};

use crate::script::ActiveRunPanel;
use crate::stack_traces::{ActiveStackTrace, StackTracePanel};
use crate::window_tests::{
    active_path, click, draw, editor_of, open_palette, scratch, type_text, wait_for_signal,
};
use crate::window_tests_lang::{FakeLanguage, open_window_with};

const SOURCE: SourceId = SourceId::new("fake-stacks");
const TEXT: &str = "public class Main {\n    public static void main(String[] args) {\n        boom();\n    }\n\n    static void boom() {\n        throw new IllegalStateException(\"boom\");\n    }\n}\n";

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

/// The language of the tests here: one report for Analyze Stack Trace, one for Dump
/// Threads, and the process id the button asked about.
struct FakeStacks {
    syntax: NoSyntax,
    trace: Report,
    dump: Report,
    dumped: Mutex<Option<u32>>,
}

impl FakeStacks {
    fn new(trace: Report, dump: Report) -> Arc<Self> {
        Arc::new(Self {
            syntax: NoSyntax(Queries::default()),
            trace,
            dump,
            dumped: Mutex::new(None),
        })
    }

    fn dumped(&self) -> Option<u32> {
        *self.dumped.lock().unwrap()
    }
}

impl Provider<TraceQuery, Report> for FakeStacks {
    fn start(&self, _: TraceQuery, _: Cancel, mut reply: Reply<Report>) {
        let _ = reply.send(Answer::new(self.trace.clone(), Confidence::Certain, SOURCE));
    }
}

impl Provider<DumpQuery, DumpAnswer> for FakeStacks {
    fn start(&self, query: DumpQuery, _: Cancel, mut reply: Reply<DumpAnswer>) {
        *self.dumped.lock().unwrap() = Some(query.0);
        let answer = DumpAnswer {
            report: Some(self.dump.clone()),
            error: None,
        };
        let _ = reply.send(Answer::new(answer, Confidence::Certain, SOURCE));
    }
}

impl LanguageSupport for FakeStacks {
    fn id(&self) -> LanguageId {
        LanguageId("fake-stacks")
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

    fn thread_dump(&self) -> Option<&DumpProvider> {
        Some(self)
    }
}

fn project(name: &str) -> (PathBuf, PathBuf) {
    let root = scratch(name);
    let folder = root.join("src/com/example");
    fs::create_dir_all(&folder).unwrap();
    let path = folder.join("Main.txt");
    fs::write(&path, TEXT).unwrap();
    (root, path)
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

fn text_line(text: &str) -> StackLine {
    StackLine {
        text: text.to_owned(),
        kind: LineKind::Text,
        frame: None,
    }
}

fn frame_line(
    class: &str,
    method: &str,
    file: Option<&str>,
    line: Option<usize>,
    place: Option<SourceSpan>,
    origin: FrameOrigin,
) -> StackLine {
    StackLine {
        text: format!(
            "\tat {class}.{method}({})",
            file.unwrap_or("Unknown Source")
        ),
        kind: LineKind::Frame,
        frame: Some(Frame {
            class: class.to_owned(),
            method: method.to_owned(),
            file: file.map(str::to_owned),
            line,
            place,
            origin,
        }),
    }
}

fn trace(path: &Path) -> Report {
    Report::Trace {
        lines: vec![
            text_line("boom.Error: boom"),
            frame_line(
                "example.Main",
                "boom",
                Some("Main.txt"),
                Some(2),
                Some(place(path, 2)),
                FrameOrigin::Project,
            ),
            frame_line(
                "core.Thread",
                "run",
                Some("Thread.txt"),
                Some(5),
                None,
                FrameOrigin::Library,
            ),
            StackLine {
                text: "Caused by: io.Error: bad".to_owned(),
                kind: LineKind::Structure,
                frame: None,
            },
            StackLine {
                text: "\t... 3 more".to_owned(),
                kind: LineKind::Structure,
                frame: None,
            },
        ],
    }
}

fn dump(path: &Path) -> Report {
    Report::Dump {
        threads: vec![
            ThreadStack {
                name: "main".to_owned(),
                state: "RUNNABLE".to_owned(),
                lines: vec![frame_line(
                    "example.Main",
                    "main",
                    Some("Main.txt"),
                    Some(1),
                    Some(place(path, 1)),
                    FrameOrigin::Project,
                )],
                deadlocked: false,
                waiting_for_lock: false,
            },
            ThreadStack {
                name: "worker".to_owned(),
                state: "BLOCKED".to_owned(),
                lines: vec![
                    frame_line(
                        "example.Main",
                        "work",
                        Some("Main.txt"),
                        Some(5),
                        Some(place(path, 5)),
                        FrameOrigin::Project,
                    ),
                    StackLine {
                        text: "- waiting to lock <0x00000000aaaa> (an object)".to_owned(),
                        kind: LineKind::Lock,
                        frame: None,
                    },
                ],
                deadlocked: true,
                waiting_for_lock: true,
            },
        ],
        deadlock: true,
    }
}

/// Opens the window of `project` with `language` as the language of the stacks.
fn open(
    cx: &mut TestAppContext,
    root: PathBuf,
    language: Arc<FakeStacks>,
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

/// Lets the slide of the dock and the work of the window finish, so a click lands on the
/// place the last frame put on screen.
fn settle(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    for _ in 0..8 {
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(50));
        cx.run_until_parked();
        draw(cx, window);
    }
}

/// Runs a command that opens a dialog and confirms it with its OK button.
fn confirm(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    command: &'static str,
    text: &str,
) {
    cx.update(|cx| cx.write_to_clipboard(ClipboardItem::new_string(text.to_owned())));
    open_palette(cx, window, command);
    draw(cx, window);
    click(cx, window, "stack-trace-ok");
    cx.run_until_parked();
    settle(cx, window);
}

#[ui_kit::gpui::test]
fn analyze_stack_trace_links_a_project_frame_and_greys_the_rest(cx: &mut TestAppContext) {
    let (root, path) = project("stack-analyze");
    let language = FakeStacks::new(trace(&path), dump(&path));
    let window = open(cx, root, language);
    confirm(
        cx,
        window,
        "workspace.analyze_stack_trace",
        "boom.Error: boom\n\tat example.Main.boom(Main.txt:3)",
    );
    wait_for_signal(cx, "the report in the panel", |cx| {
        panel(cx).read_with(cx, |panel, _| panel.shown_lines().len() == 5)
    });

    let marks = panel(cx).read_with(cx, |panel, _| panel.line_marks());
    assert_eq!(marks[1], (FrameOrigin::Project, true));
    assert_eq!(
        marks[2],
        (FrameOrigin::Library, false),
        "a JDK frame is grey and has no link"
    );

    draw(cx, window);
    click(cx, window, "stack-line-1");
    wait_for_signal(cx, "the file of the frame", |cx| {
        active_path(cx, window).as_deref() == Some(path.as_path())
    });
    let view = editor_of(cx, window, &path);
    assert_eq!(
        view.read_with(cx, |view, _| view.caret_line()),
        Some(2),
        "the caret is on the line of the frame"
    );
}

#[ui_kit::gpui::test]
fn a_thread_dump_lists_its_threads_filters_them_and_shows_the_selected_stack(
    cx: &mut TestAppContext,
) {
    let (root, path) = project("stack-dump");
    let language = FakeStacks::new(dump(&path), dump(&path));
    let window = open(cx, root, language);
    confirm(cx, window, "workspace.analyze_stack_trace", "a thread dump");
    wait_for_signal(cx, "the threads in the panel", |cx| {
        panel(cx).read_with(cx, |panel, _| !panel.visible_threads().is_empty())
    });

    let visible = panel(cx).read_with(cx, |panel, _| panel.visible_threads());
    let names: Vec<&str> = visible.iter().map(|(_, name)| name.as_str()).collect();
    assert_eq!(names, ["worker", "main"], "blocked threads come first");
    assert!(
        panel(cx).read_with(cx, |panel, _| panel
            .shown_lines()
            .iter()
            .any(|line| line.contains("Main.work"))),
        "the stack of the selected thread is on the right"
    );

    // A click on the other thread shows its stack. The blocked worker is first.
    draw(cx, window);
    click(cx, window, "stack-thread-1");
    assert!(
        panel(cx).read_with(cx, |panel, _| panel
            .shown_lines()
            .iter()
            .any(|line| line.contains("Main.main"))),
        "the chosen thread is on the right"
    );
    cx.run_until_parked();

    // The filter keeps the threads whose name has the typed letters.
    draw(cx, window);
    click(cx, window, "stack-thread-filter");
    type_text(cx, window, "mai");
    let filtered = panel(cx).read_with(cx, |panel, _| panel.visible_threads());
    let names: Vec<&str> = filtered.iter().map(|(_, name)| name.as_str()).collect();
    assert_eq!(names, ["main"]);
    assert_eq!(
        panel(cx).read_with(cx, |panel, _| panel.filter_text().to_owned()),
        "mai"
    );
}

#[ui_kit::gpui::test]
fn the_dump_threads_button_asks_the_language_and_shows_the_dump(cx: &mut TestAppContext) {
    let (root, path) = project("stack-run");
    let language = FakeStacks::new(trace(&path), dump(&path));
    let window = open(cx, root, language.clone());
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(RUN_TOOL, window, cx)
        })
        .unwrap();
    let runs = cx.read(|cx| cx.global::<ActiveRunPanel>().0.clone());
    runs.update(cx, |panel, _| panel.set_program_pid(Some(4242)));
    cx.run_until_parked();
    settle(cx, window);
    click(cx, window, "run-dump-threads");
    cx.run_until_parked();

    assert_eq!(
        language.dumped(),
        Some(4242),
        "the jcmd of the program asked"
    );
    wait_for_signal(cx, "the dump in the Stack Trace window", |cx| {
        panel(cx).read_with(cx, |panel, _| !panel.visible_threads().is_empty())
    });
    let open = window
        .read_with(cx, |workspace, _| workspace.tool_open(STACK_TRACE_TOOL))
        .unwrap();
    assert!(open, "the Stack Trace tool window is shown");
}

#[ui_kit::gpui::test]
fn compare_with_clipboard_opens_the_file_against_the_clipboard(cx: &mut TestAppContext) {
    let (root, path) = project("stack-compare");
    let window = open(cx, root, FakeStacks::new(trace(&path), dump(&path)));
    draw(cx, window);
    crate::window_tests::open_path(cx, window, &path);
    cx.update(|cx| {
        cx.write_to_clipboard(ClipboardItem::new_string(
            "public class Main {\n}".to_owned(),
        ))
    });

    window
        .update(cx, |workspace, window, cx| {
            workspace.run_command("workspace.compare_with_clipboard", window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);

    let diff = window
        .read_with(cx, |workspace, _| {
            workspace
                .overlay_view()
                .and_then(|view| view.downcast::<crate::diff_view::DiffView>().ok())
        })
        .unwrap()
        .expect("the compare window is open");
    let left = diff.read_with(cx, |diff, _| {
        diff.left_editor().read_with(cx, |view, _| view.text())
    });
    assert_eq!(left.as_deref(), Some("public class Main {\n}"));
    assert!(
        diff.read_with(cx, |diff, _| diff.current().contains("boom")),
        "the whole file is the right side"
    );
}
