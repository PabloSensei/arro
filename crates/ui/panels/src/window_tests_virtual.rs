//! Threads in the thousands and the read-only debug tool of MCP.
//!
//! `cargo test -p panels window_tests_virtual` runs the mock adapter scenario. The real
//! adapter needs `IDE_CORPUS` and a filled package cache:
//!
//! `cargo test -p panels window_tests_virtual -- --ignored --test-threads=1`

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

use dap_client::{
    BreakpointSpec, Command, DebugState, Debugger, Launch, Snapshot, is_virtual_thread, thread_list,
};
use serde_json::json;
use tool_gateway::{AuditLog, LanguageTools, SessionScope, ToolGateway};
use window_language::RealLanguage;

use crate::debug::DebugHub;

const LIMIT: Duration = Duration::from_secs(30);
const READY_LIMIT: Duration = Duration::from_secs(600);

fn wait_for(
    debugger: &Debugger,
    what: &str,
    mut done: impl FnMut(&Snapshot) -> bool,
) -> Arc<Snapshot> {
    let started = Instant::now();
    loop {
        if let Some(snapshot) = debugger.poll()
            && done(&snapshot)
        {
            return snapshot;
        }
        assert!(
            started.elapsed() < LIMIT,
            "the session did not get to {what} in time"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn scope(project: &std::path::Path) -> SessionScope {
    SessionScope {
        session: "window-tests-virtual".to_string(),
        project: project.to_path_buf(),
        cwd: project.to_path_buf(),
    }
}

fn mock_gateway(project: &std::path::Path, snapshot: Arc<Snapshot>) -> ToolGateway {
    let hub = Arc::new(DebugHub::default());
    hub.set(snapshot);
    let tools = Arc::new(LanguageTools::new());
    tools.register_debug(project.to_path_buf(), hub);
    ToolGateway::new(tools, AuditLog::default())
}

/// Ten thousand virtual threads in the thread list do not become ten thousand requests:
/// the list is grouped, and only the selected thread is asked for a stack.
#[test]
fn a_large_thread_list_is_grouped_and_only_the_selected_thread_has_a_stack() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("the runtime starts");
    runtime.block_on(async {
        let server = lang_fixture::MockDap::start_with_virtual_threads(10_000).unwrap();
        let (commands, receiver) = tokio::sync::mpsc::unbounded_channel();
        let (snapshots, incoming) = channel();
        let launch = Launch {
            address: server.address().to_owned(),
            adapter_id: "fixture".into(),
            name: "fixture".into(),
            breakpoints: vec![BreakpointSpec {
                file: PathBuf::from("C:/fixture/demo.sample"),
                line: 42,
                ..BreakpointSpec::default()
            }],
            ..Launch::default()
        };
        let running = tokio::spawn(dap_client::run(launch, receiver, snapshots));
        let debugger = Debugger::new(commands.clone(), incoming);

        let stopped = wait_for(&debugger, "the stop", |snapshot| {
            matches!(snapshot.state, DebugState::Stopped { .. })
        });
        assert_eq!(stopped.current_thread, Some(1));
        assert_eq!(stopped.threads.len(), 10_002);
        let list = thread_list(&stopped.threads, stopped.current_thread, 0);
        assert_eq!(list.stopped.as_ref().map(|thread| thread.id), Some(1));
        assert_eq!(list.platform.len(), 1);
        assert_eq!(list.virtual_threads.total, 10_000);
        assert_eq!(list.virtual_threads.threads.len(), dap_client::VIRTUAL_PAGE);

        commands.send(Command::Expand(101)).unwrap();
        let expanded = wait_for(&debugger, "the children", |snapshot| {
            snapshot
                .variables
                .get(&101)
                .is_some_and(|items| items.len() == 2)
        });
        commands.send(Command::Disconnect).unwrap();
        let ended = wait_for(&debugger, "the end", |snapshot| {
            snapshot.state == DebugState::Terminated
        });
        assert_eq!(ended.state, DebugState::Terminated);
        running.await.unwrap().unwrap();
        let requests = server.requests();
        server.join();

        let stacks = requests
            .iter()
            .filter(|message| message["command"] == "stackTrace")
            .count();
        assert_eq!(
            stacks, 1,
            "a stack was asked for more than the selected thread"
        );
        let threads = requests
            .iter()
            .filter(|message| message["command"] == "threads")
            .count();
        assert_eq!(threads, 1, "the thread list was asked for more than once");

        let project = PathBuf::from("C:/work/project");
        let gateway = mock_gateway(&project, expanded);
        let started = Instant::now();
        let answer = gateway
            .call(
                &scope(&project),
                "debug_get_state",
                &json!({"frames": 5, "variables": 10, "depth": 2, "length": 40}),
            )
            .expect("debug_get_state answers");
        let elapsed = started.elapsed();
        println!("debug_get_state took {elapsed:?}");
        assert!(
            answer.contains("debugging: stopped (breakpoint)"),
            "{answer}"
        );
        assert!(answer.contains("10000 virtual"), "{answer}");
        assert!(
            answer.contains("C:/fixture/demo.sample:42:start"),
            "{answer}"
        );
        assert!(answer.contains("Locals:"), "{answer}");
        assert!(answer.contains("text = \"hello\" (String)"), "{answer}");
        assert!(answer.contains("0 = \"a\" (String)"), "{answer}");
        assert!(
            answer.contains("C:/fixture/demo.sample:42 verified"),
            "{answer}"
        );
        let audit = gateway.audit().recent();
        assert_eq!(audit.len(), 1);
        assert_eq!(audit[0].tool, "debug_get_state");
        assert_eq!(audit[0].decision, "allow");
        assert!(audit[0].ok);
    });
}

/// The real adapter on a probe that starts ten thousand virtual threads and stops on a
/// breakpoint in `main`. The adapter of the corpus does not report virtual threads of a
/// stopped program, so the test checks what it does report and that the answer arrives.
#[test]
#[ignore = "needs IDE_CORPUS and a filled package cache"]
fn ten_thousand_virtual_threads_stop_without_hanging_the_answer() {
    let started = Instant::now();
    let corpus = window_language::corpus_dir();
    let root = std::env::temp_dir().join(format!("ide-ds-t0068-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    window_language::prepare_project(&corpus.join("src/petclinic"), &root);
    let state = root.join("state");
    let probe = window_language::install_virtual_threads_probe(&root, &state, 10_000)
        .expect("the probe compiles");
    let language = RealLanguage::open(root.clone(), state).expect("the server starts");

    let (status, updates) = channel();
    language.watch_status(move |text| {
        let _ = status.send(text);
    });
    let ready = Instant::now();
    loop {
        let text = updates
            .recv_timeout(Duration::from_secs(1))
            .unwrap_or_default();
        if window_language::status_is_ready(&text) {
            break;
        }
        assert!(
            ready.elapsed() < READY_LIMIT,
            "the server did not get ready in time"
        );
    }

    let service = language.debugger();
    language
        .open_document(&probe.path)
        .expect("the probe opens");
    let resolved = language
        .resolve_main(&probe.path)
        .expect("the main class resolves");
    assert_eq!(
        resolved.map(|main| main.main_class),
        Some("probe.DebugProbe".to_string())
    );
    let debugger = service
        .start(
            language.debug_process_with(&probe, &RealLanguage::jdk_home("21")),
            "probe.DebugProbe".into(),
            Some(probe.path.clone()),
            vec![BreakpointSpec {
                file: probe.path.clone(),
                line: probe.line,
                ..BreakpointSpec::default()
            }],
            Vec::new(),
            Vec::new(),
        )
        .expect("the session starts");

    let stopped = wait_for(&debugger, "the stop", |snapshot| {
        matches!(snapshot.state, DebugState::Stopped { .. }) && !snapshot.frames.is_empty()
    });
    let stopped_after = started.elapsed();
    let list = thread_list(&stopped.threads, stopped.current_thread, 0);
    println!(
        "stopped after {stopped_after:?}: {} platform threads, {} virtual threads, {} frames",
        list.platform.len(),
        list.virtual_threads.total,
        stopped.frames.len()
    );
    assert!(
        stopped
            .frames
            .iter()
            .any(|frame| frame.source.as_deref() == Some(probe.path.as_path())),
        "the stack has a frame of the probe: {:?}",
        stopped.frames
    );
    assert_eq!(
        list.stopped.as_ref().map(|thread| thread.id),
        stopped.current_thread
    );
    let reported = stopped
        .threads
        .iter()
        .filter(|thread| is_virtual_thread(&thread.name))
        .count();
    assert_eq!(list.virtual_threads.total, reported);

    let project = root.clone();
    let gateway = mock_gateway(&project, stopped.clone());
    let called = Instant::now();
    let answer = gateway
        .call(
            &scope(&project),
            "debug_get_state",
            &json!({"frames": 5, "variables": 10, "depth": 1, "length": 80}),
        )
        .expect("debug_get_state answers");
    let elapsed = called.elapsed();
    println!("debug_get_state took {elapsed:?}");
    assert!(answer.contains("debugging: stopped"), "{answer}");
    assert!(
        answer.contains(&format!("{} virtual", reported)),
        "{answer}"
    );

    let process_id = stopped.process_id.expect("the debuggee process id");
    let server_marker = language.process_marker();
    debugger.send(Command::Disconnect);
    let _ = wait_for(&debugger, "the end", |snapshot| {
        snapshot.state == DebugState::Terminated
    });
    let marker = probe.classes().display().to_string();
    let finished = Instant::now();
    loop {
        let left = window_language::processes_with(&marker);
        if !window_language::is_process_running(process_id) && left.is_empty() {
            break;
        }
        assert!(
            finished.elapsed() < Duration::from_secs(60),
            "the debuggee is still running: {left:?}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    drop(debugger);
    drop(language);
    let finished = Instant::now();
    loop {
        let left = window_language::processes_with(&server_marker);
        if left.is_empty() {
            break;
        }
        assert!(
            finished.elapsed() < Duration::from_secs(60),
            "the language server is still running: {left:?}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = std::fs::remove_dir_all(&root);
}
