//! Running tests: a request, a live stream of events and the tree they build. A provider
//! turns the request into a plan whose test processes connect to [`TestListen`] and send
//! one JSON event per line, after a line with the token.

use std::collections::HashMap;
use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::io::{BufRead, BufReader, ErrorKind};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use base::{Cancel, Result};
use lang_api::CoverageReport;
use project_model::ProjectModel;
use serde::Deserialize;

use crate::coverage::{CoveragePlan, CoverageStart, StartKind};
use crate::{Console, RunPlan};

const ACCEPT_POLL: Duration = Duration::from_millis(20);
/// How long a test JVM may take to announce that its debug port listens. The same as the
/// wait of an attach to a JVM that somebody else started.
const DEBUG_READY: Duration = Duration::from_secs(300);
/// The error of a start that Stop ended before the debugger could attach.
pub const STOPPED_BY_USER: &str = "stopped by the user";

/// What to run. Nothing set runs every test of the project, a module alone all of its tests.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TestRequest {
    pub module: Option<String>,
    /// A class or file of tests, in the terms of the provider.
    pub suite: Option<String>,
    /// One test of the suite.
    pub case: Option<String>,
    /// Tests of several suites as `(suite, case)`, for running the failed ones again. When
    /// not empty it replaces `suite` and `case`.
    pub cases: Vec<(String, String)>,
}

/// A test or a class of tests found in a source, for the gutter and for running the test
/// under the caret. `suite` and `case` are in the terms of the provider, as in
/// [`TestRequest`] and [`TestNode`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestSite {
    pub suite: String,
    /// `None` for the class itself.
    pub case: Option<String>,
    /// One-based.
    pub line: u32,
}

/// Finds the tests in the text of one source file. A language brings its own.
pub type FindTests = fn(&str, &std::path::Path) -> Vec<TestSite>;

/// Where the test processes report to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestListen {
    pub port: u16,
    pub token: String,
}

/// Where a debugger may attach to the test JVM. The port is free and on loopback.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestDebug {
    pub host: String,
    pub port: u16,
}

pub trait TestProvider: Send + Sync {
    fn plan(
        &self,
        model: &ProjectModel,
        request: &TestRequest,
        listen: &TestListen,
    ) -> Result<RunPlan>;

    /// The same tests with a debug agent in their JVM. A provider that cannot do that
    /// keeps the plain plan.
    fn debug_plan(
        &self,
        model: &ProjectModel,
        request: &TestRequest,
        listen: &TestListen,
        _debug: &TestDebug,
    ) -> Result<RunPlan> {
        self.plan(model, request, listen)
    }

    /// The same tests with the coverage agent in their JVM, and the file the agent
    /// writes. A provider without coverage keeps the error of this default.
    fn coverage_plan(
        &self,
        _model: &ProjectModel,
        _request: &TestRequest,
        _listen: &TestListen,
    ) -> Result<CoveragePlan> {
        Err(format!("{} cannot collect coverage", std::any::type_name::<Self>()).into())
    }

    /// Reads the data of the last run of [`TestProvider::coverage_plan`].
    fn coverage_report(
        &self,
        _exec: &std::path::Path,
        _model: &ProjectModel,
    ) -> Result<CoverageReport> {
        Err("this test provider cannot read coverage".into())
    }

    /// A line of the test output that says the test JVM listens for the debugger. A
    /// provider whose debug JVM names its port provides it, and the run waits for it
    /// before the window attaches.
    fn debug_ready(&self) -> Option<&'static str> {
        None
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TestKind {
    Suite,
    Case,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TestOutcome {
    Passed,
    Failed,
    /// An assumption did not hold, the test neither passed nor failed.
    Aborted,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct TestNode {
    pub id: String,
    #[serde(default)]
    pub parent: Option<String>,
    pub name: String,
    pub kind: TestKind,
    /// Where it is in the sources, for the gutter and the jump to it.
    #[serde(default)]
    pub suite: Option<String>,
    #[serde(default)]
    pub case: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(tag = "event", rename_all = "camelCase")]
pub enum TestEvent {
    /// A test process began its run. A build may start several.
    Plan,
    PlanFinished,
    /// Found while running, before it starts.
    Registered(TestNode),
    Started(TestNode),
    #[serde(rename_all = "camelCase")]
    Finished {
        id: String,
        outcome: TestOutcome,
        #[serde(default)]
        message: Option<String>,
        #[serde(default)]
        error_type: Option<String>,
        #[serde(default)]
        trace: Option<String>,
        #[serde(default)]
        duration_ms: u64,
    },
    Skipped {
        #[serde(flatten)]
        node: TestNode,
        #[serde(default)]
        reason: Option<String>,
    },
}

/// A running test plan. Its console has what the build printed, the events what the
/// tests did. Dropping it stops everything.
pub struct TestRun {
    events: Receiver<TestEvent>,
    stop: Arc<AtomicBool>,
    console: Console,
}

impl TestRun {
    pub fn start(
        provider: &dyn TestProvider,
        model: &ProjectModel,
        request: &TestRequest,
    ) -> Result<Self> {
        Self::start_with(provider, model, request, StartKind::Plain).map(|(run, _)| run)
    }

    /// Starts the tests under a debugger and says where it may attach. The test JVM
    /// waits for the debugger before the first test runs.
    pub fn debug_start(
        provider: &dyn TestProvider,
        model: &ProjectModel,
        request: &TestRequest,
    ) -> Result<(Self, TestDebug)> {
        Self::debug_start_with_stop(provider, model, request, &Cancel::new())
    }

    /// The same, with a token that Stop sets. Cancelling it ends the wait for the debug
    /// line at once: the build is stopped, as by the Stop of a plain test run, and the
    /// start returns [`STOPPED_BY_USER`] instead of waiting out the limit.
    pub fn debug_start_with_stop(
        provider: &dyn TestProvider,
        model: &ProjectModel,
        request: &TestRequest,
        stop: &Cancel,
    ) -> Result<(Self, TestDebug)> {
        if stop.is_cancelled() {
            return Err(STOPPED_BY_USER.into());
        }
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let port = listener.local_addr()?.port();
        drop(listener);
        let debug = TestDebug {
            host: Ipv4Addr::LOCALHOST.to_string(),
            port,
        };
        let ready = provider.debug_ready();
        let (run, _) = Self::start_with(provider, model, request, StartKind::Debug(&debug))?;
        let ready = match ready {
            Some(marker) => {
                !stop.is_cancelled() && run.console().wait_for_output(marker, DEBUG_READY, stop)
            }
            None => !stop.is_cancelled(),
        };
        if !ready {
            run.stop();
            if stop.is_cancelled() {
                return Err(STOPPED_BY_USER.into());
            }
            return Err("the test JVM did not announce a debug port".into());
        }
        Ok((run, debug))
    }

    pub(crate) fn start_with(
        provider: &dyn TestProvider,
        model: &ProjectModel,
        request: &TestRequest,
        kind: StartKind<'_>,
    ) -> Result<(Self, Option<CoverageStart>)> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        let listen = TestListen {
            port: listener.local_addr()?.port(),
            token: new_token(),
        };
        let (sender, events) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let token = listen.token.clone();
        // The listener takes connections before the plan starts a test process, so a
        // process that connects at once is never missed, however loaded the machine is.
        thread::Builder::new()
            .name("test-events".into())
            .spawn(move || accept(&listener, &token, &sender, &stopped))?;
        let plan = match kind {
            StartKind::Debug(debug) => provider
                .debug_plan(model, request, &listen, debug)
                .map(|plan| (plan, None)),
            StartKind::Plain => provider
                .plan(model, request, &listen)
                .map(|plan| (plan, None)),
            StartKind::Coverage => {
                provider
                    .coverage_plan(model, request, &listen)
                    .map(|coverage| {
                        let CoveragePlan { plan, exec, note } = coverage;
                        (plan, Some(CoverageStart { exec, note }))
                    })
            }
        };
        let (plan, coverage) = match plan {
            Ok(plan) => plan,
            Err(error) => {
                stop.store(true, Ordering::Relaxed);
                return Err(error);
            }
        };
        Ok((
            Self {
                events,
                stop,
                console: Console::start(plan),
            },
            coverage,
        ))
    }

    pub fn events(&self) -> &Receiver<TestEvent> {
        &self.events
    }

    pub fn console(&self) -> &Console {
        &self.console
    }

    /// The test process is over: no new connection is taken after the ones already
    /// pending. The stream closes when those connections delivered their last event.
    pub fn end(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    /// The same stop as for a run: Ctrl+C, then the whole tree after the grace.
    pub fn stop(&self) {
        self.console.stop();
    }
}

impl Drop for TestRun {
    fn drop(&mut self) {
        self.end();
    }
}

/// Only for telling our processes from others on the same machine, not a secret.
fn new_token() -> String {
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u32(std::process::id());
    let first = hasher.finish();
    hasher.write_u64(first);
    format!("{first:016x}{:016x}", hasher.finish())
}

fn accept(listener: &TcpListener, token: &str, events: &Sender<TestEvent>, stop: &AtomicBool) {
    loop {
        if stop.load(Ordering::Relaxed) {
            // The test process is over: take the connections that are already waiting
            // and let the readers deliver their last lines. The channel closes when the
            // accept loop and every reader have dropped their sender.
            while let Ok((stream, _)) = listener.accept() {
                read_in_thread(stream, token, events);
            }
            return;
        }
        match listener.accept() {
            Ok((stream, _)) => read_in_thread(stream, token, events),
            Err(error) if error.kind() == ErrorKind::WouldBlock => thread::sleep(ACCEPT_POLL),
            Err(_) => thread::sleep(ACCEPT_POLL),
        }
    }
}

fn read_in_thread(stream: TcpStream, token: &str, events: &Sender<TestEvent>) {
    let (token, events) = (token.to_owned(), events.clone());
    let _ = thread::Builder::new()
        .name("test-events-in".into())
        .spawn(move || read_events(stream, &token, &events));
}

fn read_events(stream: TcpStream, token: &str, events: &Sender<TestEvent>) {
    // The listener socket is non-blocking, an accepted one need not be.
    if stream.set_nonblocking(false).is_err() {
        return;
    }
    let mut lines = BufReader::new(stream).lines();
    match lines.next() {
        Some(Ok(first)) if first.trim() == token => {}
        _ => return,
    }
    for line in lines {
        let Ok(line) = line else { return };
        // An event of a later listener version is skipped, not fatal.
        if let Ok(event) = serde_json::from_str::<TestEvent>(&line)
            && events.send(event).is_err()
        {
            return;
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TestState {
    Running,
    Passed,
    Failed {
        message: Option<String>,
        trace: Option<String>,
    },
    Aborted {
        message: Option<String>,
    },
    Skipped {
        reason: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeNode {
    pub node: TestNode,
    pub state: TestState,
    pub duration_ms: u64,
    pub children: Vec<usize>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TestCounts {
    pub passed: usize,
    pub failed: usize,
    pub aborted: usize,
    pub skipped: usize,
}

/// The results so far, built from the events. Several test processes may report the same
/// container, such as the engine: it stays one node.
#[derive(Clone, Debug, Default)]
pub struct TestTree {
    nodes: Vec<TreeNode>,
    index: HashMap<String, usize>,
    roots: Vec<usize>,
}

impl TestTree {
    pub fn apply(&mut self, event: &TestEvent) {
        match event {
            TestEvent::Plan | TestEvent::PlanFinished => {}
            TestEvent::Registered(node) | TestEvent::Started(node) => {
                self.node(node, TestState::Running);
            }
            TestEvent::Skipped { node, reason } => {
                let reason = reason.clone();
                self.node(node, TestState::Skipped { reason });
            }
            TestEvent::Finished {
                id,
                outcome,
                message,
                trace,
                duration_ms,
                ..
            } => {
                let Some(&at) = self.index.get(id) else {
                    return;
                };
                let message = message.clone();
                self.nodes[at].state = match outcome {
                    TestOutcome::Passed => TestState::Passed,
                    TestOutcome::Failed => TestState::Failed {
                        message,
                        trace: trace.clone(),
                    },
                    TestOutcome::Aborted => TestState::Aborted { message },
                };
                self.nodes[at].duration_ms = *duration_ms;
            }
        }
    }

    fn node(&mut self, node: &TestNode, state: TestState) {
        if let Some(&at) = self.index.get(&node.id) {
            self.nodes[at].state = state;
            return;
        }
        let at = self.nodes.len();
        self.nodes.push(TreeNode {
            node: node.clone(),
            state,
            duration_ms: 0,
            children: Vec::new(),
        });
        self.index.insert(node.id.clone(), at);
        let parent = node.parent.as_ref().and_then(|id| self.index.get(id));
        match parent {
            Some(&parent) => self.nodes[parent].children.push(at),
            None => self.roots.push(at),
        }
    }

    pub fn roots(&self) -> impl Iterator<Item = &TreeNode> {
        self.roots.iter().map(|&at| &self.nodes[at])
    }

    pub fn children<'a>(&'a self, node: &'a TreeNode) -> impl Iterator<Item = &'a TreeNode> {
        node.children.iter().map(|&at| &self.nodes[at])
    }

    pub fn get(&self, id: &str) -> Option<&TreeNode> {
        self.index.get(id).map(|&at| &self.nodes[at])
    }

    /// The first case with this name in this suite.
    pub fn case(&self, suite: &str, case: &str) -> Option<&TreeNode> {
        self.nodes.iter().find(|node| {
            node.node.kind == TestKind::Case
                && node.node.suite.as_deref() == Some(suite)
                && node.node.case.as_deref() == Some(case)
        })
    }

    /// The class or file of tests with this name.
    pub fn suite(&self, suite: &str) -> Option<&TreeNode> {
        self.nodes.iter().find(|node| {
            node.node.kind == TestKind::Suite && node.node.suite.as_deref() == Some(suite)
        })
    }

    /// Every case so far, in the order they were found.
    pub fn cases(&self) -> impl Iterator<Item = &TreeNode> {
        let cases = self.nodes.iter();
        cases.filter(|node| node.node.kind == TestKind::Case)
    }

    /// Over cases only.
    pub fn counts(&self) -> TestCounts {
        let mut counts = TestCounts::default();
        for node in &self.nodes {
            if node.node.kind != TestKind::Case {
                continue;
            }
            match node.state {
                TestState::Passed => counts.passed += 1,
                TestState::Failed { .. } => counts.failed += 1,
                TestState::Aborted { .. } => counts.aborted += 1,
                TestState::Skipped { .. } => counts.skipped += 1,
                TestState::Running => {}
            }
        }
        counts
    }
}

#[cfg(test)]
mod tests {
    #[cfg(windows)]
    use std::fs;
    use std::io::Write;
    #[cfg(windows)]
    use std::path::{Path, PathBuf};
    #[cfg(windows)]
    use std::process::Command;
    use std::time::Instant;

    use proc_sup::ProcessSpec;

    use super::*;

    /// What the listener sent in the S8 run of one test class, the last case failing.
    const EVENTS: &[&str] = &[
        r#"{"event":"plan"}"#,
        r#"{"event":"started","id":"e","name":"Engine","kind":"suite"}"#,
        r#"{"event":"started","id":"e/c","parent":"e","name":"VetTests","kind":"suite","suite":"demo.VetTests"}"#,
        r#"{"event":"started","id":"e/c/a","parent":"e/c","name":"a()","kind":"case","suite":"demo.VetTests","case":"a"}"#,
        r#"{"event":"finished","id":"e/c/a","outcome":"passed","durationMs":12}"#,
        r#"{"event":"skipped","id":"e/c/s","parent":"e/c","name":"s()","kind":"case","suite":"demo.VetTests","case":"s","reason":"off"}"#,
        r#"{"event":"started","id":"e/c/b","parent":"e/c","name":"b()","kind":"case","suite":"demo.VetTests","case":"b"}"#,
        r#"{"event":"finished","id":"e/c/b","outcome":"failed","message":"expected: <1> but was: <2>","errorType":"org.opentest4j.AssertionFailedError","trace":"at demo.VetTests.b(VetTests.src:9)","durationMs":3}"#,
        r#"{"event":"some future event"}"#,
        r#"{"event":"finished","id":"e/c","outcome":"passed"}"#,
        r#"{"event":"planFinished"}"#,
    ];

    #[test]
    fn events_build_a_tree_with_counts() {
        let mut tree = TestTree::default();
        for line in EVENTS {
            if let Ok(event) = serde_json::from_str::<TestEvent>(line) {
                tree.apply(&event);
            }
        }

        let root = tree.roots().next().unwrap();
        let suite = tree.children(root).next().unwrap();
        assert_eq!(suite.node.name, "VetTests");
        assert_eq!(tree.children(suite).count(), 3);
        let counts = tree.counts();
        assert_eq!((counts.passed, counts.failed, counts.skipped), (1, 1, 1));
        let TestState::Failed { message, trace } = &tree.case("demo.VetTests", "b").unwrap().state
        else {
            panic!("b did not fail");
        };
        assert_eq!(message.as_deref(), Some("expected: <1> but was: <2>"));
        assert!(trace.as_deref().unwrap().contains("VetTests.src:9"));
    }

    /// A provider whose test process is a thread that plays the listener.
    struct Replay;

    impl TestProvider for Replay {
        fn plan(
            &self,
            _model: &ProjectModel,
            _request: &TestRequest,
            listen: &TestListen,
        ) -> Result<RunPlan> {
            let listen = listen.clone();
            thread::spawn(move || {
                // A stranger without the token is ignored.
                let mut stranger = TcpStream::connect((Ipv4Addr::LOCALHOST, listen.port)).unwrap();
                writeln!(stranger, "wrong\n{}", EVENTS[1]).unwrap();
                let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, listen.port)).unwrap();
                writeln!(stream, "{}", listen.token).unwrap();
                for line in EVENTS {
                    writeln!(stream, "{line}").unwrap();
                }
            });
            Ok(RunPlan {
                before: None,
                program: ProcessSpec::new("cmd").args(["/c", "exit", "0"]),
                grace: Duration::from_secs(1),
            })
        }
    }

    #[test]
    fn a_run_hears_the_processes_that_know_the_token() {
        let run =
            TestRun::start(&Replay, &ProjectModel::default(), &TestRequest::default()).unwrap();

        let mut tree = TestTree::default();
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            match run.events().recv_timeout(Duration::from_millis(100)) {
                Ok(TestEvent::PlanFinished) => break,
                Ok(event) => tree.apply(&event),
                Err(_) => {}
            }
        }

        assert_eq!(tree.counts().failed, 1);
        assert_eq!(tree.roots().count(), 1);
    }

    /// A provider whose test JVM starts and never announces its debug port.
    #[cfg(windows)]
    struct SilentDebug {
        program: ProcessSpec,
    }

    #[cfg(windows)]
    impl TestProvider for SilentDebug {
        fn plan(
            &self,
            _model: &ProjectModel,
            _request: &TestRequest,
            _listen: &TestListen,
        ) -> Result<RunPlan> {
            Ok(RunPlan {
                before: None,
                program: self.program.clone(),
                grace: Duration::ZERO,
            })
        }

        fn debug_plan(
            &self,
            model: &ProjectModel,
            request: &TestRequest,
            listen: &TestListen,
            _debug: &TestDebug,
        ) -> Result<RunPlan> {
            self.plan(model, request, listen)
        }

        fn debug_ready(&self) -> Option<&'static str> {
            Some("Listening for transport dt_socket at address:")
        }
    }

    /// A copy of a program that runs far longer than the test, under a name of its own:
    /// other tests start the same program, and only this image tells the process apart.
    #[cfg(windows)]
    fn silent_program(name: &str) -> (ProcessSpec, PathBuf) {
        let system_root = std::env::var_os("SystemRoot").expect("a Windows machine");
        let source = PathBuf::from(system_root).join("System32").join("ping.exe");
        let image = test_support::scratch(&format!("runner-{name}")).join("program.exe");
        fs::copy(&source, &image).expect("the program is copied");
        let spec = ProcessSpec::new(&image)
            .args(["-n", "120", "127.0.0.1"])
            // No helper in this test binary, so Stop goes straight to force.
            .ctrl_helper("no-such-helper.exe");
        (spec, image)
    }

    #[cfg(windows)]
    fn is_running(image: &Path) -> bool {
        let name = image.file_name().unwrap().to_string_lossy().into_owned();
        let list = Command::new("tasklist")
            .args(["/fi", &format!("imagename eq {name}"), "/fo", "csv", "/nh"])
            .output()
            .expect("tasklist runs");
        String::from_utf8_lossy(&list.stdout).contains(&name)
    }

    #[cfg(windows)]
    fn wait_until(mut condition: impl FnMut() -> bool) {
        let start = Instant::now();
        while !condition() {
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "condition not met in time"
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    #[cfg(windows)]
    #[test]
    fn stop_during_the_wait_for_the_debug_line_ends_the_run_at_once() {
        let (program, image) = silent_program("debug-stop");
        let provider = SilentDebug { program };
        let request = TestRequest::default();
        let cancel = Cancel::new();
        let worker = {
            let cancel = cancel.clone();
            thread::spawn(move || {
                let result = TestRun::debug_start_with_stop(
                    &provider,
                    &ProjectModel::default(),
                    &request,
                    &cancel,
                );
                // The failing start already dropped the run and with it the process.
                result.map(|(run, _)| drop(run)).map_err(|e| e.to_string())
            })
        };
        wait_until(|| is_running(&image));

        let start = Instant::now();
        cancel.cancel();
        let outcome = worker.join().unwrap();
        let elapsed = start.elapsed();

        assert!(
            elapsed < Duration::from_secs(1),
            "the wait took {elapsed:?}"
        );
        assert_eq!(outcome, Err(STOPPED_BY_USER.to_owned()));
        assert!(!is_running(&image), "the stopped process still runs");
    }
}
