//! The tests in the Run tool window: a live tree of results, the failure of the selected
//! test with a link on every frame that is in the project, and starting and stopping them.

use std::collections::HashSet;
use std::ops::Range;
use std::sync::mpsc::TryRecvError;
use std::time::Instant;

use editor_view::Spot;
use runner::{TestRequest, TestRun, TestState, TestTree, TreeNode};

use super::*;
use crate::navigation;

/// How long the events of a test process may still arrive after it exited when its
/// stream does not close, as for a socket a grandchild keeps open.
const EVENT_DRAIN: Duration = Duration::from_secs(5);
const TREE_WIDTH: f32 = 360.;
const CHIP_HEIGHT: f32 = 22.;

/// Everything about the last test run that the window shows.
pub(super) struct TestView {
    /// What the tree is for; `None` before the first test run.
    pub request: Option<TestRequest>,
    pub run: Option<TestRun>,
    /// The test process ended, events may still be on their way.
    pub exited: Option<Instant>,
    pub finished: bool,
    pub tree: TestTree,
    pub selected: Option<String>,
    pub only_failed: bool,
    collapsed: HashSet<String>,
    scroll: UniformListScrollHandle,
    detail: Vec<Frame>,
    detail_scroll: UniformListScrollHandle,
}

/// A line of a failure, with where it points when that is a file of the project.
struct Frame {
    text: String,
    at: Option<(PathBuf, u32)>,
}

struct Row {
    id: String,
    depth: usize,
    name: String,
    state: TestState,
    duration_ms: u64,
    open: Option<bool>,
}

impl TestView {
    pub fn new() -> Self {
        Self {
            request: None,
            run: None,
            exited: None,
            finished: false,
            tree: TestTree::default(),
            selected: None,
            only_failed: false,
            collapsed: HashSet::new(),
            scroll: UniformListScrollHandle::new(),
            detail: Vec::new(),
            detail_scroll: UniformListScrollHandle::new(),
        }
    }

    pub fn active(&self) -> bool {
        self.request.is_some()
    }

    /// The test process is still going.
    pub fn running(&self) -> bool {
        self.run.is_some() && self.exited.is_none()
    }

    /// Starts over for a request, the process comes later.
    pub fn begin(&mut self, request: TestRequest) {
        *self = Self {
            request: Some(request),
            only_failed: self.only_failed,
            ..Self::new()
        };
    }

    /// Whether the tree was ever shown; a plain run clears it.
    pub fn clear(&mut self) {
        *self = Self::new();
    }

    /// Applies what the tests reported. True when the tree changed.
    pub fn poll(&mut self, files: &[PathBuf]) -> bool {
        let Some(run) = &self.run else {
            return false;
        };
        let mut changed = false;
        let mut closed = false;
        for _ in 0..EVENTS_PER_POLL {
            match run.events().try_recv() {
                Ok(event) => {
                    self.tree.apply(&event);
                    changed = true;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    // Every connection is done and the queue is empty: no event is late.
                    closed = true;
                    break;
                }
            }
        }
        if self.exited.is_some() {
            // The process is over, so the listener may stop taking connections. The
            // stream closes after the last one delivered its last line.
            run.end();
        }
        if closed || self.exited.is_some_and(|at| at.elapsed() >= EVENT_DRAIN) {
            self.run = None;
            self.finished = true;
            changed = true;
            if self.selected.is_none() {
                let failed = self.failed_ids().into_iter().next();
                if let Some(id) = failed {
                    self.select(&id, files);
                }
            }
        }
        changed
    }

    fn failed_ids(&self) -> Vec<String> {
        let failed = self.tree.cases().filter(|node| is_failed(node));
        failed.map(|node| node.node.id.clone()).collect()
    }

    /// The `(suite, case)` of every failed test, for running them again.
    pub fn failed_cases(&self) -> Vec<(String, String)> {
        let cases = self.tree.cases().filter(|node| is_failed(node));
        let named = cases.filter_map(|node| {
            let node = &node.node;
            Some((node.suite.clone()?, node.case.clone()?))
        });
        named.collect()
    }

    pub fn select(&mut self, id: &str, files: &[PathBuf]) {
        self.selected = Some(id.to_owned());
        self.detail = self
            .tree
            .get(id)
            .map(|node| failure_lines(node, files))
            .unwrap_or_default();
    }

    /// The suite and the case of the selected test, for Jump to Source.
    pub fn selected_test(&self) -> Option<(String, Option<String>)> {
        let node = self.tree.get(self.selected.as_deref()?)?;
        Some((node.node.suite.clone()?, node.node.case.clone()))
    }

    /// Moves the selection through the rows as they show, opening no source.
    pub fn select_relative(&mut self, delta: isize, files: &[PathBuf]) {
        let rows = self.rows();
        if rows.is_empty() {
            return;
        }
        let last = rows.len() as isize - 1;
        let current = self
            .selected
            .as_deref()
            .and_then(|id| rows.iter().position(|row| row.id == id));
        let next = match current {
            Some(index) => (index as isize + delta).clamp(0, last),
            None if delta < 0 => last,
            None => 0,
        };
        let id = rows[next as usize].id.clone();
        self.select(&id, files);
    }

    /// The text of the selected row, for Ctrl+C.
    pub fn selected_text(&self) -> Option<String> {
        let id = self.selected.as_deref()?;
        self.rows()
            .into_iter()
            .find(|row| row.id == id)
            .map(|row| row.name)
    }

    /// The lines of the failure of the selected test, for scripts.
    pub fn detail_text(&self) -> Vec<String> {
        self.detail.iter().map(|frame| frame.text.clone()).collect()
    }

    fn rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        for root in self.tree.roots() {
            self.push_rows(root, 0, &mut rows);
        }
        rows
    }

    fn push_rows(&self, node: &TreeNode, depth: usize, rows: &mut Vec<Row>) {
        if self.only_failed && !failed_within(&self.tree, node) {
            return;
        }
        let has_children = !node.children.is_empty();
        let open = has_children && !self.collapsed.contains(&node.node.id);
        rows.push(Row {
            id: node.node.id.clone(),
            depth,
            name: node.node.name.clone(),
            state: node.state.clone(),
            duration_ms: node.duration_ms,
            open: has_children.then_some(open),
        });
        if open {
            for child in self.tree.children(node) {
                self.push_rows(child, depth + 1, rows);
            }
        }
    }

    /// Text lines of the tree as it shows, for scripts.
    pub fn report(&self) -> Vec<String> {
        let counts = self.tree.counts();
        let mut lines = vec![format!(
            "tests passed {} failed {} aborted {} skipped {}",
            counts.passed, counts.failed, counts.aborted, counts.skipped
        )];
        for row in self.rows() {
            let state = match row.state {
                TestState::Running if self.finished => "interrupted",
                TestState::Running => "running",
                TestState::Passed => "passed",
                TestState::Failed { .. } => "failed",
                TestState::Aborted { .. } => "aborted",
                TestState::Skipped { .. } => "skipped",
            };
            let indent = "  ".repeat(row.depth);
            lines.push(format!("test {indent}{state} {}", row.name));
        }
        lines
    }
}

fn is_failed(node: &TreeNode) -> bool {
    matches!(node.state, TestState::Failed { .. })
}

fn failed_within(tree: &TestTree, node: &TreeNode) -> bool {
    is_failed(node) || tree.children(node).any(|child| failed_within(tree, child))
}

/// The message and the stack of a failed test, one entry per line.
fn failure_lines(node: &TreeNode, files: &[PathBuf]) -> Vec<Frame> {
    let (message, trace) = match &node.state {
        TestState::Failed { message, trace } => (message.as_deref(), trace.as_deref()),
        TestState::Aborted { message } => (message.as_deref(), None),
        TestState::Skipped { reason } => (reason.as_deref(), None),
        _ => return Vec::new(),
    };
    let plain = |text: &str| Frame {
        text: text.replace('\t', "    "),
        at: None,
    };
    let mut lines = vec![plain(&node.node.name)];
    for line in message.unwrap_or_default().lines() {
        lines.push(plain(line));
    }
    if let Some(trace) = trace {
        lines.push(plain(""));
        for line in trace.lines() {
            lines.push(Frame {
                text: line.replace('\t', "    "),
                at: frame_target(line, files),
            });
        }
    }
    lines
}

/// Where a stack line like `at demo.owner.OwnerTests.find(OwnerTests.code:42)` points,
/// when that file is in the project.
fn frame_target(line: &str, files: &[PathBuf]) -> Option<(PathBuf, u32)> {
    let rest = line.trim().strip_prefix("at ")?;
    let (call, location) = rest.split_once('(')?;
    let (file, number) = location[..location.rfind(')')?].rsplit_once(':')?;
    let number: u32 = number.trim().parse().ok().filter(|&number| number > 0)?;
    // `code.base/code.util.List.get` carries the module before the slash.
    let call = call.rsplit('/').next()?;
    let (class, _method) = call.rsplit_once('.')?;
    let package = class.rsplit_once('.').map_or("", |(package, _)| package);
    let mut tail = PathBuf::new();
    for part in package.split('.').filter(|part| !part.is_empty()) {
        tail.push(part);
    }
    tail.push(file);
    let found = files.iter().find(|path| path.ends_with(&tail))?;
    Some((found.clone(), number))
}

fn state_icon(state: &TestState, finished: bool) -> &'static str {
    match state {
        TestState::Running if finished => "status/info",
        TestState::Running => "run/run",
        TestState::Passed => "status/success",
        TestState::Failed { .. } => "status/error",
        TestState::Aborted { .. } => "status/warning",
        TestState::Skipped { .. } => "status/info",
    }
}

fn duration_text(milliseconds: u64) -> String {
    if milliseconds >= 1000 {
        format!("{:.1} s", milliseconds as f64 / 1000.)
    } else {
        format!("{milliseconds} ms")
    }
}

impl EventEmitter<TestsChanged> for RunPanel {}

/// The tree or the marks of the tests changed.
pub struct TestsChanged;

impl RunPanel {
    /// Nothing runs, plans or waits, and the last events are in.
    pub fn idle(&self) -> bool {
        !(self.busy()
            || self.starting
            || self.waiting
            || self.waiting_tests.is_some()
            || self.tests.run.is_some())
    }

    /// The tree and the failure as text, for scripts.
    pub fn test_report(&self) -> Vec<String> {
        if !self.tests.active() {
            return Vec::new();
        }
        let mut lines = self.tests.report();
        for text in self.tests.detail_text() {
            lines.push(format!("detail {text}"));
        }
        lines
    }

    /// The project files, to find the sources named in a stack.
    pub fn set_files(&mut self, files: Entity<Arc<Vec<PathBuf>>>) {
        self.files = Some(files);
    }

    fn file_list(&self, cx: &App) -> Arc<Vec<PathBuf>> {
        self.files
            .as_ref()
            .map_or_else(Default::default, |files| files.read(cx).clone())
    }

    /// Whether the build tool of the project runs tests at all.
    pub fn can_test(&self) -> bool {
        self.test_provider.is_some()
    }

    /// Finds the sites of tests in a source, for the marks and for Run at caret.
    pub fn test_sites(&self) -> FindTests {
        self.find_tests
    }

    /// What the last test run says about a test or a class, for its mark in the gutter.
    pub fn test_state(&self, suite: &str, case: Option<&str>) -> Option<TestState> {
        let node = match case {
            Some(case) => self.tests.tree.case(suite, case),
            None => self.tests.tree.suite(suite),
        }?;
        match node.state {
            TestState::Running if self.tests.finished => None,
            _ => Some(node.state.clone()),
        }
    }

    /// Runs tests, stopping what runs now first. Planning can run a tool, so it happens in
    /// the background.
    pub fn run_tests(&mut self, request: TestRequest, cx: &mut Context<Self>) {
        if self.starting {
            return;
        }
        if self.busy() {
            self.queued_tests = Some(request);
            self.stop_running();
            return;
        }
        let Some(provider) = self.test_provider.clone() else {
            self.tell("Cannot run tests", "The project has no build tool", cx);
            return;
        };
        let model = self.session.as_ref().and_then(ProjectSession::model);
        let Some(model) = model.filter(|model| model.resolved || !self.loading) else {
            self.waiting_tests = Some(request);
            self.lines.clear();
            self.note("Waiting for the project to load", false);
            self.changed(cx);
            return;
        };
        self.waiting_tests = None;
        self.queued_tests = None;
        self.rerun = false;
        self.archive_console();
        self.title = title_of(&request).into();
        self.lines.clear();
        self.parser = AnsiParser::default();
        self.tests.begin(request.clone());
        self.starting = true;
        let plan = cx.background_executor().spawn(async move {
            TestRun::start(&*provider, &model, &request).map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| {
            let started = plan.await;
            this.update(cx, |this, cx| {
                this.starting = false;
                match started {
                    Ok(run) => this.tests.run = Some(run),
                    Err(message) => {
                        this.note(&message, true);
                        this.tell("Cannot run tests", message, cx);
                        this.tests.finished = true;
                    }
                }
                cx.emit(TestsChanged);
                this.changed(cx);
            })
            .ok();
        })
        .detach();
        cx.emit(TestsChanged);
        self.changed(cx);
    }

    /// Debugs the test selected in the tree, or its class when the class is selected.
    pub(crate) fn debug_selected_test(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.tests.selected.clone() else {
            self.tell("Cannot debug tests", "Select a test in the tree first", cx);
            return;
        };
        let Some(node) = self.tests.tree.get(&id) else {
            return;
        };
        let suite = node.node.suite.clone().or_else(|| {
            self.tests
                .request
                .as_ref()
                .and_then(|request| request.suite.clone())
        });
        let case = node.node.case.clone();
        let module = self
            .tests
            .request
            .as_ref()
            .and_then(|request| request.module.clone());
        let Some(suite) = suite else {
            self.tell("Cannot debug tests", "The selected node is not a test", cx);
            return;
        };
        let request = TestRequest {
            module,
            suite: Some(suite),
            case,
            ..TestRequest::default()
        };
        self.debug_tests(request, window, cx);
    }

    /// Runs the failed tests of the last run again.
    pub fn rerun_failed(&mut self, cx: &mut Context<Self>) {
        let cases = self.tests.failed_cases();
        if cases.is_empty() {
            self.tell("Nothing to rerun", "No test failed in the last run", cx);
            return;
        }
        let request = TestRequest {
            cases,
            ..self.tests.request.clone().unwrap_or_default()
        };
        self.run_tests(request, cx);
    }

    /// The Rerun button: the tests when they ran last, else the selected configuration.
    pub fn rerun_last(&mut self, cx: &mut Context<Self>) {
        match self.tests.request.clone() {
            Some(request) => self.run_tests(request, cx),
            None => self.run(cx),
        }
    }

    /// Called every poll with the console already read.
    pub(super) fn poll_tests(&mut self, cx: &mut Context<Self>) -> bool {
        let files = self.file_list(cx);
        let changed = self.tests.poll(&files);
        if changed {
            cx.emit(TestsChanged);
        }
        changed
    }

    fn click_test(&mut self, id: &str, cx: &mut Context<Self>) {
        let has_children = self
            .tests
            .tree
            .get(id)
            .is_some_and(|n| !n.children.is_empty());
        if has_children && !self.tests.collapsed.remove(id) {
            self.tests.collapsed.insert(id.to_owned());
        }
        let files = self.file_list(cx);
        self.tests.select(id, &files);
        cx.notify();
    }

    /// Selects a test by its name, for scripts.
    pub fn select_test(&mut self, name: &str, cx: &mut Context<Self>) -> bool {
        let found = self.tests.tree.cases().find(|node| {
            node.node.case.as_deref() == Some(name) || node.node.name.trim_end_matches("()") == name
        });
        let Some(id) = found.map(|node| node.node.id.clone()) else {
            return false;
        };
        let files = self.file_list(cx);
        self.tests.select(&id, &files);
        cx.notify();
        true
    }

    /// Opens the source of the test selected in the tree, as F4 does in IDEA.
    pub fn open_selected_test(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((suite, case)) = self.tests.selected_test() else {
            self.tell(
                "Cannot open the test",
                "Select a test in the tree first",
                cx,
            );
            return;
        };
        let files = self.file_list(cx);
        let find = self.find_tests;
        // The file of a suite carries its name: reading every source of a big project on
        // a key press would freeze the window.
        let name = suite.rsplit('.').next().unwrap_or(&suite);
        let found = files
            .iter()
            .filter(|path| {
                path.file_stem()
                    .is_some_and(|stem| stem.to_string_lossy() == name)
            })
            .find_map(|path| {
                let text = std::fs::read_to_string(path).ok()?;
                let site = find(&text, path).into_iter().find(|site| {
                    site.suite == suite
                        && case
                            .as_deref()
                            .is_none_or(|case| site.case.as_deref() == Some(case))
                })?;
                Some((path.clone(), site.line))
            });
        let Some((path, line)) = found else {
            self.tell(
                "Cannot open the test",
                "The source of the test was not found",
                cx,
            );
            return;
        };
        let workspace = self.workspace.clone();
        let spot = Spot::Line {
            line: line.saturating_sub(1) as usize,
            column: 0,
        };
        window.defer(cx, move |window, cx| {
            workspace
                .update(cx, |workspace, cx| {
                    navigation::go_to(workspace, path, Some(spot), true, window, cx)
                })
                .ok();
        });
    }

    /// The keys of the test tree: arrows walk it, Enter and F4 open the source, Ctrl+C
    /// copies the row.
    pub(super) fn test_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.tests.active() {
            return;
        }
        let key = &event.keystroke;
        let files = self.file_list(cx);
        if key.modifiers.control && !key.modifiers.alt && key.key == "c" {
            if let Some(text) = self.tests.selected_text() {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
            }
            cx.stop_propagation();
            return;
        }
        match key.key.as_str() {
            "up" => {
                self.tests.select_relative(-1, &files);
                cx.stop_propagation();
                cx.notify();
            }
            "down" => {
                self.tests.select_relative(1, &files);
                cx.stop_propagation();
                cx.notify();
            }
            "enter" | "f4" => {
                cx.stop_propagation();
                self.open_selected_test(window, cx);
            }
            _ => {}
        }
    }

    /// Opens the source behind a line of the failure.
    pub fn open_frame(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some((path, line)) = self.tests.detail.get(index).and_then(|f| f.at.clone()) else {
            return;
        };
        let workspace = self.workspace.clone();
        window.defer(cx, move |window, cx| {
            let spot = Spot::Line {
                line: line as usize - 1,
                column: 0,
            };
            workspace
                .update(cx, |workspace, cx| {
                    navigation::go_to(workspace, path, Some(spot), true, window, cx)
                })
                .ok();
        });
    }

    /// The first line of the failure that points into the project, for scripts.
    pub fn first_frame(&self) -> Option<usize> {
        self.tests
            .detail
            .iter()
            .position(|frame| frame.at.is_some())
    }

    /// The tree of results with the failure or the console next to it.
    pub(super) fn tests_body(
        &mut self,
        console: AnyElement,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *theme;
        let counts = self.tests.tree.counts();
        let status = if self.tests.finished {
            "Done"
        } else if self.tests.running() || self.starting {
            "Running"
        } else {
            "Stopping"
        };
        let summary = format!(
            "{status}: {} passed, {} failed, {} skipped",
            counts.passed,
            counts.failed,
            counts.skipped + counts.aborted
        );
        let header = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(4.))
            .h(px(ROW_HEIGHT + 6.))
            .px(px(8.))
            .child(div().flex_1().min_w_0().truncate().child(summary))
            .child(chip(
                "tests-only-failed",
                "Failed",
                self.tests.only_failed,
                &theme,
                cx.listener(|this, _, _, cx| {
                    this.tests.only_failed = !this.tests.only_failed;
                    cx.notify();
                }),
            ))
            .child(chip(
                "tests-rerun-failed",
                "Rerun failed",
                false,
                &theme,
                cx.listener(|this, _, _, cx| this.rerun_failed(cx)),
            ))
            .child(chip(
                "tests-debug-selected",
                "Debug",
                false,
                &theme,
                cx.listener(|this, _, window, cx| this.debug_selected_test(window, cx)),
            ));
        let rows = self.tests.rows();
        let empty = rows.is_empty();
        let hint = if !empty {
            None
        } else if self.tests.finished {
            Some("No tests ran, see the console")
        } else {
            Some("Starting the tests")
        };
        let left = div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(TREE_WIDTH))
            .h_full()
            .border_r_1()
            .border_color(theme.border)
            .child(header)
            .children(hint.map(|hint| {
                div()
                    .px(px(12.))
                    .text_color(theme.text_secondary)
                    .child(hint)
            }))
            .child(
                scrollable(
                    "test-tree",
                    &self.tests.scroll,
                    div().size_full().child(
                        uniform_list(
                            "test-tree",
                            rows.len(),
                            cx.processor(move |this, range: Range<usize>, _, cx| {
                                let rows = this.tests.rows();
                                let rows = range.filter_map(|index| rows.get(index));
                                rows.map(|row| this.test_row(row, &theme, cx))
                                    .collect::<Vec<_>>()
                            }),
                        )
                        .track_scroll(&self.tests.scroll)
                        .px(px(4.))
                        .size_full(),
                    ),
                    &theme,
                    window,
                    cx,
                )
                .flex_1()
                .min_h_0(),
            );
        let right = if self.tests.detail.is_empty() {
            console
        } else {
            scrollable(
                "test-detail",
                &self.tests.detail_scroll,
                div()
                    .size_full()
                    .font_family(MONO_FONT)
                    .text_size(px(CODE_SIZE))
                    .text_color(theme.code)
                    .child(
                        uniform_list(
                            "test-detail",
                            self.tests.detail.len(),
                            cx.processor(move |this, range: Range<usize>, _, cx| {
                                range
                                    .map(|index| this.detail_line(index, &theme, cx))
                                    .collect::<Vec<_>>()
                            }),
                        )
                        .track_scroll(&self.tests.detail_scroll)
                        .size_full(),
                    ),
                &theme,
                window,
                cx,
            )
            .size_full()
            .into_any_element()
        };
        div()
            .flex()
            .size_full()
            .child(left)
            .child(div().flex_1().min_w_0().h_full().child(right))
            .into_any_element()
    }

    fn test_row(&self, row: &Row, theme: &Theme, cx: &mut Context<Self>) -> Stateful<Div> {
        let hint =
            (row.duration_ms > 0 && row.open.is_none()).then(|| duration_text(row.duration_ms));
        let id = row.id.clone();
        let shown = TreeRow {
            id: SharedString::from(row.id.clone()).into(),
            depth: row.depth,
            label: row.name.clone().into(),
            hint: hint.map(Into::into),
            icon: state_icon(&row.state, self.tests.finished),
            expandable: row.open.is_some(),
            open: row.open == Some(true),
            selected: self.tests.selected.as_deref() == Some(row.id.as_str()),
        };
        let row_id = id.clone();
        tree_row(shown, theme)
            .debug_selector(move || format!("test-row-{row_id}"))
            .on_click(cx.listener(move |this, _, _, cx| this.click_test(&id, cx)))
    }

    fn detail_line(&self, index: usize, theme: &Theme, cx: &mut Context<Self>) -> Stateful<Div> {
        let frame = &self.tests.detail[index];
        let line = div()
            .id(("test-frame", index))
            .debug_selector(move || format!("test-frame-{index}"))
            .h(px(CODE_LINE_HEIGHT))
            .px(px(LINE_PAD))
            .whitespace_nowrap()
            .child(frame.text.clone());
        match &frame.at {
            Some(_) => line
                .text_color(theme.link)
                .cursor_pointer()
                .hover(|style| style.underline())
                .on_click(
                    cx.listener(move |this, _, window, cx| this.open_frame(index, window, cx)),
                ),
            None if index == 0 => line.text_color(theme.text),
            None => line,
        }
    }
}

pub(super) fn title_of(request: &TestRequest) -> String {
    let short = |suite: &str| suite.rsplit('.').next().unwrap_or(suite).to_owned();
    match (&request.suite, &request.case) {
        _ if !request.cases.is_empty() => "Tests: failed".to_owned(),
        (Some(suite), Some(case)) => format!("Tests: {}.{case}", short(suite)),
        (Some(suite), None) => format!("Tests: {}", short(suite)),
        _ => "Tests".to_owned(),
    }
}

fn chip(
    id: &'static str,
    label: &'static str,
    on: bool,
    theme: &Theme,
    handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let (hover, selected) = (theme.hover, theme.selection_inactive);
    div()
        .id(id)
        .debug_selector(move || id.to_owned())
        .flex_none()
        .flex()
        .items_center()
        .h(px(CHIP_HEIGHT))
        .px(px(8.))
        .rounded(px(CONTROL_RADIUS))
        .cursor_pointer()
        .when(on, |this| this.bg(selected))
        .when(!on, |this| this.hover(move |style| style.bg(hover)))
        .child(label)
        .on_click(handler)
}

#[cfg(test)]
mod checks {
    use std::path::PathBuf;

    use runner::{TestEvent, TestKind, TestNode, TestOutcome};

    use super::{TestView, duration_text, frame_target};

    fn files() -> Vec<PathBuf> {
        vec![
            PathBuf::from("C:/p/src/main/code/demo/owner/Owner.code"),
            PathBuf::from("C:/p/src/test/code/demo/owner/OwnerTests.code"),
        ]
    }

    #[test]
    fn a_frame_points_to_the_file_of_its_package() {
        let line = "\tat demo.owner.OwnerTests$Nested.find(OwnerTests.code:42)";

        let target = frame_target(line, &files()).unwrap();

        assert_eq!(target.1, 42);
        assert!(target.0.ends_with("test/code/demo/owner/OwnerTests.code"));
    }

    #[test]
    fn frames_outside_the_project_point_nowhere() {
        let list = files();
        assert_eq!(
            frame_target("at code.base/code.util.List.get(List.code:5)", &list),
            None
        );
        assert_eq!(
            frame_target("at demo.owner.Owner.name(Owner.code)", &list),
            None
        );
        assert_eq!(frame_target("Caused by: boom", &list), None);
        assert_eq!(
            frame_target("at other.Owner.name(Owner.code:3)", &list),
            None
        );
    }

    #[test]
    fn durations_read_short() {
        assert_eq!(duration_text(12), "12 ms");
        assert_eq!(duration_text(1500), "1.5 s");
    }

    fn node(id: &str, parent: Option<&str>, name: &str, case: Option<&str>) -> TestNode {
        TestNode {
            id: id.into(),
            parent: parent.map(Into::into),
            name: name.into(),
            kind: if case.is_some() {
                TestKind::Case
            } else {
                TestKind::Suite
            },
            suite: (id != "e").then(|| "demo.VetTests".into()),
            case: case.map(Into::into),
        }
    }

    fn finished(id: &str, outcome: TestOutcome) -> TestEvent {
        TestEvent::Finished {
            id: id.into(),
            outcome,
            message: Some("expected: <1> but was: <2>".into()),
            error_type: None,
            trace: Some(
                "AssertionFailedError: expected
	at demo.VetTests.b(VetTests.code:9)
	at code.base/code.lang.Thread.run(Thread.code:1)"
                    .into(),
            ),
            duration_ms: 3,
        }
    }

    fn view_with_results() -> TestView {
        let mut view = TestView::new();
        let events = [
            TestEvent::Started(node("e", None, "Engine", None)),
            TestEvent::Started(node("e/c", Some("e"), "VetTests", None)),
            TestEvent::Started(node("e/c/a", Some("e/c"), "a()", Some("a"))),
            finished("e/c/a", TestOutcome::Passed),
            TestEvent::Started(node("e/c/b", Some("e/c"), "b()", Some("b"))),
            finished("e/c/b", TestOutcome::Failed),
        ];
        for event in &events {
            view.tree.apply(event);
        }
        view
    }

    #[test]
    fn the_tree_lists_containers_before_their_tests() {
        let view = view_with_results();

        let names: Vec<_> = view
            .rows()
            .iter()
            .map(|row| (row.depth, row.name.clone()))
            .collect();

        assert_eq!(
            names,
            [
                (0, "Engine".into()),
                (1, "VetTests".into()),
                (2, "a()".into()),
                (2, "b()".into())
            ]
        );
    }

    #[test]
    fn only_failed_keeps_the_failures_and_the_way_to_them() {
        let mut view = view_with_results();
        view.only_failed = true;

        let names: Vec<_> = view.rows().into_iter().map(|row| row.name).collect();

        assert_eq!(names, ["Engine", "VetTests", "b()"]);
    }

    #[test]
    fn a_collapsed_container_hides_its_children() {
        let mut view = view_with_results();
        view.collapsed.insert("e/c".into());

        let rows = view.rows();

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].open, Some(false));
    }

    #[test]
    fn failed_tests_are_named_for_a_rerun() {
        let view = view_with_results();

        assert_eq!(
            view.failed_cases(),
            [("demo.VetTests".to_owned(), "b".to_owned())]
        );
    }

    #[test]
    fn the_failure_shows_the_message_and_links_the_frames_of_the_project() {
        let mut view = view_with_results();
        let files = [PathBuf::from("C:/p/src/test/code/demo/VetTests.code")];

        view.select("e/c/b", &files);

        let text = view.detail_text();
        assert_eq!(text[0], "b()");
        assert_eq!(text[1], "expected: <1> but was: <2>");
        let linked: Vec<_> = view
            .detail
            .iter()
            .filter(|frame| frame.at.is_some())
            .collect();
        assert_eq!(linked.len(), 1);
        assert_eq!(linked[0].at.as_ref().unwrap().1, 9);
    }
}
