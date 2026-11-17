//! Debugging without a window: the mock adapter of `lang-fixture` and the real debug
//! plugin of jdtls on a copy of PetClinic.
//!
//! `cargo test -p java-lang --test debug` runs the mock scenario. The real ones need
//! `IDE_CORPUS`, `MAVEN_USER_HOME` and the helpers
//! (`cargo run -p xtask -- build-helpers --out <target>/debug/helpers`):
//! `cargo test -p java-lang --test debug -- --ignored --test-threads=1`.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};

use dap_client::{BreakpointSpec, Command as DebugCommand, DebugService, DebugState, Snapshot};
use java_exec::{JavaTestProvider, JvmFlags};
use java_lang::{DebugExtension, JavaDebugger, JavaProject, JdtlsConfig, Runtime, State};
use java_project::{BuildEnv, Jdk};
use platform::CanonPath;
use proc_sup::ProcessSpec;
use project_model::ProjectModel;
use runner::{ConsoleEvent, TestRequest, TestRun};
use sidecar::Trust;
use tokio::runtime::Handle;
use tokio::time::{sleep, timeout};

const READY: Duration = Duration::from_secs(600);
const LIMIT: Duration = Duration::from_secs(300);

const PROBE: &str = r#"package probe;

public class DebugProbe {

    public static void main(String[] args) {
        System.out.println("probe-stdout");
        System.err.println("probe-stderr");
        int total = 0;
        for (int i = 0; i < 3; i++) {
            total = add(total, i);
        }
        System.out.println("total=" + total);
        try {
            Thread.sleep(20000);
        } catch (InterruptedException ignored) {
        }
    }

    static int add(int a, int b) {
        int sum = a + b;
        return sum;
    }
}
"#;
/// The line of `int sum = a + b;` in [`PROBE`], from one.
const BREAK_LINE: u32 = 20;

const EXCEPTION_PROBE: &str = r#"package probe;

public class ExceptionProbe {

    public static void main(String[] args) {
        throw new IllegalStateException("probe-exception");
    }
}
"#;

const SKIPPED: &[&str] = &[".git", "target", "build", "bin", ".gradle", "node_modules"];

fn corpus() -> PathBuf {
    PathBuf::from(std::env::var("IDE_CORPUS").expect("set IDE_CORPUS"))
}

fn copy_tree(from: &Path, to: &Path, inside_sources: bool) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        let text = name.to_string_lossy();
        if !inside_sources && SKIPPED.contains(&text.as_ref()) {
            continue;
        }
        if entry.file_type().unwrap().is_dir() {
            copy_tree(
                &entry.path(),
                &to.join(&name),
                inside_sources || text == "src",
            );
        } else {
            fs::copy(entry.path(), to.join(&name)).unwrap();
        }
    }
}

struct Fixture {
    root: PathBuf,
    probe: PathBuf,
    classes: PathBuf,
    settings: PathBuf,
    config: JdtlsConfig,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Build {
    Maven,
    Gradle,
}

fn fixture() -> Fixture {
    fixture_for(Build::Maven)
}

fn gradle_fixture() -> Fixture {
    fixture_for(Build::Gradle)
}

fn fixture_for(build: Build) -> Fixture {
    let corpus = corpus();
    let scratch = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("debug");
    let name = match build {
        Build::Maven => "petclinic",
        Build::Gradle => "petclinic-gradle",
    };
    let root = scratch.join(name);
    let probe = root.join("src/main/java/probe/DebugProbe.java");
    let exception = root.join("src/main/java/probe/ExceptionProbe.java");
    fs::create_dir_all(probe.parent().unwrap()).unwrap();
    let marker = match build {
        Build::Maven => "pom.xml",
        Build::Gradle => "build.gradle",
    };
    if !root.join(marker).exists() {
        copy_tree(&corpus.join("src/petclinic"), &root, false);
    }
    // One build system only.
    match build {
        Build::Maven => {
            for name in ["build.gradle", "settings.gradle", "gradlew", "gradlew.bat"] {
                let _ = fs::remove_file(root.join(name));
            }
            let _ = fs::remove_dir_all(root.join("gradle"));
        }
        Build::Gradle => {
            for name in ["pom.xml", "mvnw", "mvnw.cmd"] {
                let _ = fs::remove_file(root.join(name));
            }
            let _ = fs::remove_dir_all(root.join(".mvn"));
        }
    }
    fs::write(&probe, PROBE).unwrap();
    fs::write(&exception, EXCEPTION_PROBE).unwrap();
    let state_dir = scratch.join("state");
    let _ = fs::remove_dir_all(&state_dir);
    fs::create_dir_all(&state_dir).unwrap();

    let m2 = PathBuf::from(std::env::var("MAVEN_USER_HOME").expect("set MAVEN_USER_HOME"));
    let settings = scratch.join("maven-settings.xml");
    let repository = m2
        .join("repository")
        .display()
        .to_string()
        .replace('\\', "/");
    fs::write(
        &settings,
        format!(
            "<settings><localRepository>{repository}</localRepository><offline>true</offline></settings>"
        ),
    )
    .unwrap();

    let jdk = fs::canonicalize(corpus.join("jdk/25")).unwrap();
    let tools = corpus.join("tools");
    let runtime = |name: &str, version: &str, default| Runtime {
        name: name.to_owned(),
        home: fs::canonicalize(corpus.join("jdk").join(version)).unwrap(),
        default,
    };
    let config = JdtlsConfig {
        jdtls_home: tools.join("jdtls-1.63.0"),
        debug_plugin: tools.join(
            "vscode-java-debug-0.60.0/extension/server/com.microsoft.java.debug.plugin-0.53.2.jar",
        ),
        java_home: jdk.clone(),
        project_root: root.clone(),
        state_dir,
        runtimes: vec![
            runtime("JavaSE-17", "17", false),
            runtime("JavaSE-25", "25", true),
        ],
        maven_settings: Some(settings.clone()),
        gradle_home: None,
        gradle_user_home: None,
        gradle_java_home: None,
        offline: true,
        environment: HashMap::new(),
    };

    let classes = scratch.join("classes");
    let _ = fs::remove_dir_all(&classes);
    fs::create_dir_all(&classes).unwrap();
    let compiled = Command::new(jdk.join("bin/javac.exe"))
        .arg("-g")
        .arg("--release")
        .arg("8")
        .arg("-d")
        .arg(&classes)
        .arg(&probe)
        .arg(&exception)
        .output()
        .expect("javac runs");
    assert!(
        compiled.status.success(),
        "javac failed: {}",
        String::from_utf8_lossy(&compiled.stderr)
    );

    Fixture {
        root,
        probe,
        classes,
        settings,
        config,
    }
}

fn probe_process(fixture: &Fixture, jdk: &Path, feature: u32, class: &str) -> ProcessSpec {
    let flags = JvmFlags::for_jdk(feature);
    ProcessSpec::new(jdk.join("bin/java.exe"))
        .cwd(&fixture.root)
        .arg(flags.debug_agent("127.0.0.1", 0, true))
        .arg("-cp")
        .arg(&fixture.classes)
        .arg(class)
}

fn helpers_dir() -> PathBuf {
    std::env::var_os("IDE_HELPERS_DIR").map_or_else(
        || {
            std::env::current_exe()
                .unwrap()
                .parent()
                .unwrap()
                .with_file_name("helpers")
        },
        PathBuf::from,
    )
}

/// Test JVMs of Surefire and Gradle and the build clients that started them.
fn test_jvms() -> Vec<u32> {
    let script = "Get-CimInstance Win32_Process -Filter \"Name='java.exe'\" |         Where-Object { $_.CommandLine -like '*surefirebooter*' -or             $_.CommandLine -like '*plexus-classworlds*' -or             $_.CommandLine -like '*GradleWrapperMain*' -or             $_.CommandLine -like '*GradleWorkerMain*' } |         ForEach-Object { $_.ProcessId }";
    let output = Command::new("powershell")
        .args(["-NoProfile", "-Command", script])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    text.split_whitespace()
        .filter_map(|pid| pid.parse().ok())
        .collect()
}

fn assert_no_test_jvms() {
    let start = Instant::now();
    loop {
        let left = test_jvms();
        if left.is_empty() {
            return;
        }
        assert!(
            start.elapsed() < Duration::from_secs(60),
            "test JVMs left: {left:?}"
        );
        std::thread::sleep(Duration::from_millis(500));
    }
}

async fn ready(project: &JavaProject) {
    let mut status = project.status();
    let waited = timeout(
        READY,
        status.wait_for(|status| status.state == State::Ready),
    )
    .await;
    waited.expect("jdtls did not get ready in time").unwrap();
}

fn wait_for(
    debugger: &dap_client::Debugger,
    mut done: impl FnMut(&Snapshot) -> bool,
) -> Arc<Snapshot> {
    let started = Instant::now();
    let mut last: Option<Arc<Snapshot>> = None;
    loop {
        if let Some(snapshot) = debugger.poll() {
            if done(&snapshot) {
                return snapshot;
            }
            last = Some(snapshot);
        }
        if started.elapsed() > LIMIT {
            panic!(
                "the debug session did not get there in time; last: {:?}",
                last.as_ref().map(|snapshot| (
                    &snapshot.state,
                    &snapshot.console,
                    snapshot.frames.len(),
                    snapshot.breakpoints.len()
                ))
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn is_running(process_id: u32) -> bool {
    let output = Command::new("tasklist")
        .args(["/FI", &format!("PID eq {process_id}"), "/NH"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stdout).contains(&process_id.to_string())
}

fn has_line(snapshot: &Snapshot, text: &str, error: bool) -> bool {
    snapshot
        .console
        .iter()
        .any(|line| line.text == text && line.error == error)
}

/// Everything the test process printed, until it ends.
fn console_output(run: &TestRun) -> String {
    let started = Instant::now();
    let mut text = String::new();
    loop {
        match run
            .console()
            .events()
            .recv_timeout(Duration::from_millis(200))
        {
            Ok(ConsoleEvent::Output { text: part, .. }) => text.push_str(&part),
            Ok(ConsoleEvent::Exited { .. }) => break,
            Ok(ConsoleEvent::Started { .. } | ConsoleEvent::Failed { .. }) => {}
            Ok(ConsoleEvent::Closed) => {}
            Err(RecvTimeoutError::Timeout) => {
                assert!(
                    started.elapsed() < Duration::from_secs(60),
                    "the test process did not end:\n{text}"
                );
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    text
}

/// The mock adapter of `lang-fixture`: framing, a stop, frames and variables, steps and
/// the end, with a frame that has no source and line zero.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_mock_adapter_drives_a_session() {
    let server = lang_fixture::MockDap::start().unwrap();
    let (commands, receiver) = tokio::sync::mpsc::unbounded_channel();
    let (snapshots, incoming) = std::sync::mpsc::channel();
    let launch = dap_client::Launch {
        address: server.address().to_owned(),
        adapter_id: "fixture".into(),
        name: "fixture".into(),
        breakpoints: vec![BreakpointSpec {
            file: PathBuf::from("C:/fixture/demo.sample"),
            line: 42,
            ..BreakpointSpec::default()
        }],
        watch: vec!["1+1".into()],
        ..dap_client::Launch::default()
    };
    let running = tokio::spawn(dap_client::run(launch, receiver, snapshots));

    let stopped = wait_for(
        &dap_client::Debugger::new(commands.clone(), incoming),
        |snapshot| matches!(snapshot.state, DebugState::Stopped { .. }),
    );
    assert_eq!(stopped.frames[0].line, 0);
    assert_eq!(stopped.frames[0].source, None);
    assert_eq!(stopped.frames[1].line, 42);
    assert!(
        stopped
            .variables
            .values()
            .flatten()
            .any(|v| v.name == "text")
    );

    commands.send(DebugCommand::Disconnect).unwrap();
    running.await.unwrap().unwrap();
    server.join();
}

/// The mock adapter covers what the window sends and shows: program output, a condition
/// the adapter cannot compile, a hit count, and an exception filter from its capabilities.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_mock_adapter_covers_output_conditions_hits_and_exceptions() {
    let server = lang_fixture::MockDap::start().unwrap();
    let (commands, receiver) = tokio::sync::mpsc::unbounded_channel();
    let (snapshots, incoming) = std::sync::mpsc::channel();
    let launch = dap_client::Launch {
        address: server.address().to_owned(),
        adapter_id: "fixture".into(),
        name: "fixture".into(),
        breakpoints: vec![
            BreakpointSpec {
                file: PathBuf::from("C:/fixture/demo.sample"),
                line: 42,
                condition: Some("broken".into()),
                ..BreakpointSpec::default()
            },
            BreakpointSpec {
                file: PathBuf::from("C:/fixture/demo.sample"),
                line: 43,
                hit_condition: Some("2".into()),
                ..BreakpointSpec::default()
            },
            BreakpointSpec {
                file: PathBuf::from("C:/fixture/demo.sample"),
                line: 44,
                enabled: false,
                ..BreakpointSpec::default()
            },
        ],
        exception_filters: vec!["uncaught".into()],
        ..dap_client::Launch::default()
    };
    let running = tokio::spawn(dap_client::run(launch, receiver, snapshots));
    let debugger = dap_client::Debugger::new(commands.clone(), incoming);

    let stopped = wait_for(&debugger, |snapshot| {
        matches!(snapshot.state, DebugState::Stopped { .. })
            && snapshot
                .console
                .iter()
                .any(|line| line.text == "hello from fixture")
    });
    assert!(
        stopped
            .console
            .iter()
            .any(|line| line.text == "hello from fixture" && !line.error)
    );
    assert!(
        stopped
            .console
            .iter()
            .any(|line| line.text == "oops from fixture" && line.error)
    );
    assert_eq!(stopped.exception_filters.len(), 2);
    assert_eq!(stopped.exception_filters[0].filter, "caught");
    assert!(stopped.exception_filters[1].default);
    assert_eq!(stopped.breakpoints.len(), 2, "{:?}", stopped.breakpoints);
    assert!(
        stopped.breakpoints.iter().any(|breakpoint| {
            breakpoint.line == 42
                && !breakpoint.verified
                && breakpoint
                    .message
                    .as_deref()
                    .is_some_and(|message| message.contains("broken"))
        }),
        "{:?}",
        stopped.breakpoints
    );
    assert!(
        stopped
            .console
            .iter()
            .any(|line| line.error && line.text.contains("broken"))
    );

    let requests = server.requests();
    let set = requests
        .iter()
        .find(|message| message["command"] == "setBreakpoints")
        .expect("the breakpoints were sent");
    let sent = set["arguments"]["breakpoints"].as_array().unwrap();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0]["condition"], "broken");
    assert_eq!(sent[1]["hitCondition"], "2");
    let filters = requests
        .iter()
        .find(|message| message["command"] == "setExceptionBreakpoints")
        .expect("the exception filters were sent");
    assert_eq!(
        filters["arguments"]["filters"],
        serde_json::json!(["uncaught"])
    );

    debugger.send(DebugCommand::Continue);
    let exception = wait_for(
        &debugger,
        |snapshot| matches!(&snapshot.state, DebugState::Stopped { reason } if reason == "exception"),
    );
    assert_eq!(
        exception.state,
        DebugState::Stopped {
            reason: "exception".into()
        }
    );

    debugger.send(DebugCommand::Disconnect);
    running.await.unwrap().unwrap();
    server.join();
}

/// The real plugin: resolve the main class, start the adapter, launch the JVM, stop on
/// a breakpoint, read the stack and variables, step, continue and stop without a process
/// left behind.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs IDE_CORPUS and MAVEN_USER_HOME with the debug plugin"]
async fn breakpoints_and_steps_on_petclinic() {
    let fixture = fixture();
    let project = Arc::new(
        JavaProject::open(fixture.config.clone(), Trust::Trusted).expect("the project opens"),
    );
    ready(&project).await;

    let extension = DebugExtension::new(&project);
    let uri = lsp_client::uri::file_uri(&CanonPath::new(&fixture.probe).unwrap()).unwrap();
    let resolved = extension.resolve_main_class(&uri).await.unwrap();
    assert_eq!(
        resolved.as_ref().map(|main| main.main_class.as_str()),
        Some("probe.DebugProbe")
    );

    let jdk = &fixture.config.java_home;
    let spec = ProcessSpec::new(jdk.join("bin/java.exe"))
        .cwd(&fixture.root)
        .arg(java_exec::DEBUG_AGENT)
        .arg("-cp")
        .arg(&fixture.classes)
        .arg("probe.DebugProbe");
    let service = JavaDebugger::new(project.clone(), Handle::current());
    let debugger = service
        .start(
            spec,
            "DebugProbe".into(),
            Some(fixture.probe.clone()),
            vec![BreakpointSpec {
                file: fixture.probe.clone(),
                line: BREAK_LINE,
                ..BreakpointSpec::default()
            }],
            vec!["a".into(), "sum".into()],
            Vec::new(),
        )
        .expect("the session starts");

    let stopped = wait_for(&debugger, |snapshot| {
        matches!(snapshot.state, DebugState::Stopped { .. })
            && !snapshot.frames.is_empty()
            && has_line(snapshot, "probe-stdout", false)
            && has_line(snapshot, "probe-stderr", true)
    });
    assert!(
        stopped
            .console
            .iter()
            .any(|line| line.text == "probe-stdout" && !line.error),
        "stdout is missing: {:?}",
        stopped.console
    );
    assert!(
        stopped
            .console
            .iter()
            .any(|line| line.text == "probe-stderr" && line.error),
        "stderr is not marked: {:?}",
        stopped.console
    );
    let frame = stopped
        .frames
        .iter()
        .find(|frame| frame.source.as_deref() == Some(fixture.probe.as_path()))
        .expect("a frame in the probe");
    assert!(frame.name.contains("add"), "{}", frame.name);
    assert_eq!(frame.line, BREAK_LINE);
    let variables: Vec<&dap_client::Variable> = stopped.variables.values().flatten().collect();
    assert!(
        variables.iter().any(|variable| variable.name == "a"),
        "no `a` in {:?}",
        variables.iter().map(|v| &v.name).collect::<Vec<_>>()
    );
    assert!(
        stopped
            .watch
            .iter()
            .any(|watch| watch.expression == "a" && !watch.value.is_empty()),
        "watch did not evaluate: {:?}",
        stopped.watch
    );

    debugger.send(DebugCommand::Next);
    let stepped = wait_for(&debugger, |snapshot| {
        matches!(&snapshot.state, DebugState::Stopped { reason } if reason == "step")
            && snapshot.frames.iter().any(|frame| {
                frame.source.as_deref() == Some(fixture.probe.as_path()) && frame.line > BREAK_LINE
            })
    });
    assert!(stepped.selected_frame.is_some());

    debugger.send(DebugCommand::Continue);
    let resumed = wait_for(&debugger, |snapshot| {
        matches!(&snapshot.state, DebugState::Stopped { reason } if reason == "breakpoint")
            || snapshot.state == DebugState::Terminated
    });
    let process_id = resumed
        .process_id
        .expect("the adapter reported the process id of the debuggee");

    debugger.send(DebugCommand::Disconnect);
    let ended = wait_for(&debugger, |snapshot| {
        snapshot.state == DebugState::Terminated
    });
    assert_eq!(ended.state, DebugState::Terminated);

    timeout(Duration::from_secs(30), async {
        while is_running(process_id) {
            sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .expect("the debuggee outlived the session");
}

/// A condition and a hit count on the real adapter. Both stop at the second call of
/// `add`, where `a` is one.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs IDE_CORPUS and MAVEN_USER_HOME with the debug plugin"]
async fn conditional_and_hit_count_breakpoints_on_petclinic() {
    let fixture = fixture();
    let project = Arc::new(
        JavaProject::open(fixture.config.clone(), Trust::Trusted).expect("the project opens"),
    );
    ready(&project).await;
    let service = JavaDebugger::new(project.clone(), Handle::current());
    let jdk = fixture.config.java_home.clone();

    for (condition, hit, expected) in [
        (Some("a == 1".to_owned()), None, "1"),
        (None, Some("2".to_owned()), "0"),
    ] {
        let spec = probe_process(&fixture, &jdk, 25, "probe.DebugProbe");
        let debugger = service
            .start(
                spec,
                "DebugProbe".into(),
                None,
                vec![BreakpointSpec {
                    file: fixture.probe.clone(),
                    line: BREAK_LINE,
                    condition,
                    hit_condition: hit,
                    log_message: None,
                    enabled: true,
                }],
                vec!["a".into()],
                Vec::new(),
            )
            .expect("the session starts");
        let stopped = wait_for(&debugger, |snapshot| {
            matches!(snapshot.state, DebugState::Stopped { .. })
                && snapshot
                    .frames
                    .iter()
                    .any(|frame| frame.source.as_deref() == Some(fixture.probe.as_path()))
        });
        let variables: Vec<&dap_client::Variable> = stopped.variables.values().flatten().collect();
        assert!(
            variables
                .iter()
                .any(|variable| variable.name == "a" && variable.value == expected),
            "the breakpoint did not stop at the second call: {:?}",
            variables
                .iter()
                .map(|variable| (&variable.name, &variable.value))
                .collect::<Vec<_>>()
        );

        debugger.send(DebugCommand::Continue);
        let process_id = stopped.process_id.expect("the process id");
        debugger.send(DebugCommand::Disconnect);
        wait_for(&debugger, |snapshot| {
            snapshot.state == DebugState::Terminated
        });
        timeout(Duration::from_secs(30), async {
            while is_running(process_id) {
                sleep(Duration::from_millis(200)).await;
            }
        })
        .await
        .expect("the debuggee outlived the session");
    }
}

/// A method breakpoint set before the launch stops at the entry of the method, and Drop
/// Frame sends the selected frame back to its first line.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs IDE_CORPUS and MAVEN_USER_HOME with the debug plugin"]
async fn a_method_breakpoint_and_drop_frame_on_petclinic() {
    let fixture = fixture();
    let project = Arc::new(
        JavaProject::open(fixture.config.clone(), Trust::Trusted).expect("the project opens"),
    );
    ready(&project).await;
    let service = JavaDebugger::new(project.clone(), Handle::current());
    let jdk = fixture.config.java_home.clone();
    let spec = probe_process(&fixture, &jdk, 25, "probe.DebugProbe");

    let debugger = service
        .start_with_breakpoints(
            spec,
            "DebugProbe".into(),
            None,
            Vec::new(),
            vec![dap_client::FunctionBreakpointSpec {
                name: "probe.DebugProbe#add".into(),
                condition: None,
                hit_condition: None,
                enabled: true,
            }],
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
        .expect("the session starts");

    let stopped = wait_for(&debugger, |snapshot| {
        matches!(&snapshot.state, DebugState::Stopped { reason } if reason == "function breakpoint")
            && snapshot
                .frames
                .iter()
                .any(|frame| frame.name.contains("add"))
    });
    let frame = stopped
        .frames
        .iter()
        .find(|frame| frame.name.contains("add"))
        .expect("a frame in add");
    assert_eq!(
        frame.line, BREAK_LINE,
        "the breakpoint stopped at the entry"
    );
    assert_eq!(
        stopped
            .function_breakpoints
            .first()
            .map(|answer| answer.verified),
        Some(true),
        "the adapter accepted the method breakpoint: {:?}",
        stopped.function_breakpoints
    );

    // One step away from the entry, so the restarted frame has somewhere to return from.
    debugger.send(DebugCommand::Next);
    let stepped = wait_for(&debugger, |snapshot| {
        matches!(&snapshot.state, DebugState::Stopped { reason } if reason == "step")
            && snapshot
                .frames
                .iter()
                .any(|frame| frame.name.contains("add") && frame.line > BREAK_LINE)
    });
    let frame_id = stepped.selected_frame.expect("the stepped frame");

    debugger.send(DebugCommand::RestartFrame(frame_id));
    let restarted = wait_for(&debugger, |snapshot| {
        matches!(&snapshot.state, DebugState::Stopped { reason } if reason == "restartframe")
            && snapshot
                .frames
                .iter()
                .any(|frame| frame.name.contains("add") && frame.line == BREAK_LINE)
    });
    let process_id = restarted.process_id.expect("the process id");

    debugger.send(DebugCommand::Disconnect);
    wait_for(&debugger, |snapshot| {
        snapshot.state == DebugState::Terminated
    });
    timeout(Duration::from_secs(30), async {
        while is_running(process_id) {
            sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .expect("the debuggee outlived the session");
}

/// An exception breakpoint from the adapter capabilities stops the debuggee on a throw.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs IDE_CORPUS and MAVEN_USER_HOME with the debug plugin"]
async fn an_exception_breakpoint_stops_on_petclinic() {
    let fixture = fixture();
    let project = Arc::new(
        JavaProject::open(fixture.config.clone(), Trust::Trusted).expect("the project opens"),
    );
    ready(&project).await;
    let service = JavaDebugger::new(project.clone(), Handle::current());
    let jdk = fixture.config.java_home.clone();
    let spec = probe_process(&fixture, &jdk, 25, "probe.ExceptionProbe");
    let debugger = service
        .start(
            spec,
            "ExceptionProbe".into(),
            None,
            Vec::new(),
            Vec::new(),
            vec!["uncaught".into()],
        )
        .expect("the session starts");

    let stopped = wait_for(
        &debugger,
        |snapshot| matches!(&snapshot.state, DebugState::Stopped { reason } if reason == "exception"),
    );
    assert!(
        stopped.frames.iter().any(|frame| frame
            .source
            .as_deref()
            .is_some_and(|path| path.ends_with("ExceptionProbe.java"))),
        "no frame of the probe: {:?}",
        stopped.frames
    );
    let process_id = stopped.process_id.expect("the process id");
    debugger.send(DebugCommand::Disconnect);
    wait_for(&debugger, |snapshot| {
        snapshot.state == DebugState::Terminated
    });
    timeout(Duration::from_secs(30), async {
        while is_running(process_id) {
            sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .expect("the debuggee outlived the session");
}

/// The debug agent of every JDK in the corpus starts the probe, and its streams reach
/// the console.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs IDE_CORPUS and MAVEN_USER_HOME with the debug plugin"]
async fn the_debuggee_jvm_starts_on_every_corpus_jdk() {
    let fixture = fixture();
    let project = Arc::new(
        JavaProject::open(fixture.config.clone(), Trust::Trusted).expect("the project opens"),
    );
    ready(&project).await;
    let service = JavaDebugger::new(project.clone(), Handle::current());
    let corpus = corpus();

    for (version, feature) in [("8", 8), ("17", 17), ("21", 21), ("25", 25)] {
        let jdk = fs::canonicalize(corpus.join("jdk").join(version)).unwrap();
        let spec = probe_process(&fixture, &jdk, feature, "probe.DebugProbe");
        let debugger = service
            .start(
                spec,
                format!("DebugProbe-{version}"),
                None,
                Vec::new(),
                Vec::new(),
                Vec::new(),
            )
            .expect("the session starts");
        let running = wait_for(&debugger, |snapshot| {
            snapshot.state == DebugState::Terminated
                || (has_line(snapshot, "probe-stdout", false)
                    && has_line(snapshot, "probe-stderr", true))
        });
        assert!(
            running
                .console
                .iter()
                .any(|line| line.text == "probe-stderr" && line.error),
            "JDK {version}: stderr is not marked: {:?}",
            running.console
        );
        assert!(
            running
                .console
                .iter()
                .any(|line| line.text == "probe-stdout" && !line.error),
            "JDK {version}: stdout is missing"
        );
        let process_id = running.process_id.expect("the process id");
        if running.state != DebugState::Terminated {
            debugger.send(DebugCommand::Disconnect);
            wait_for(&debugger, |snapshot| {
                snapshot.state == DebugState::Terminated
            });
        }
        timeout(Duration::from_secs(30), async {
            while is_running(process_id) {
                sleep(Duration::from_millis(200)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("JDK {version}: the debuggee outlived the session"));
    }
}

/// One test of PetClinic runs through Surefire under the debugger, and stopping the
/// session leaves neither Maven nor its test JVM.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs IDE_CORPUS, MAVEN_USER_HOME, the helpers and a Maven cache"]
async fn a_maven_test_runs_under_the_debugger() {
    let fixture = fixture();
    let project = Arc::new(
        JavaProject::open(fixture.config.clone(), Trust::Trusted).expect("the project opens"),
    );
    ready(&project).await;
    let service = JavaDebugger::new(project.clone(), Handle::current());
    // No extended-length prefix: a batch file passes JAVA_HOME to cmd.exe, which does not
    // take one.
    let jdk = corpus().join("jdk/25");
    let cache = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("debug-test-cache");
    let env = BuildEnv {
        jdk: Some(Jdk {
            home: jdk.clone(),
            version: "25".into(),
            feature: 25,
        }),
        maven_args: vec![
            "-o".into(),
            "-s".into(),
            fixture.settings.display().to_string(),
            "-Dspring-javaformat.skip=true".into(),
            "-Dcheckstyle.skip=true".into(),
        ],
        cache_root: Some(cache),
        helpers: Some(helpers_dir()),
        ..BuildEnv::default()
    };
    let provider = JavaTestProvider::new(env);
    let model = ProjectModel {
        root: fixture.root.clone(),
        tool: "maven".into(),
        resolved: true,
        ..ProjectModel::default()
    };
    let suite = "org.springframework.samples.petclinic.model.ValidatorTests";
    let request = TestRequest {
        suite: Some(suite.into()),
        case: Some("shouldNotValidateWhenFirstNameEmpty".into()),
        ..TestRequest::default()
    };
    let test_file = fixture
        .root
        .join("src/test/java/org/springframework/samples/petclinic/model/ValidatorTests.java");

    let (run, debug) = TestRun::debug_start(&provider, &model, &request).expect("the run starts");
    let debugger = service
        .attach_ready(
            debug.host,
            debug.port,
            "ValidatorTests".into(),
            None,
            vec![BreakpointSpec {
                file: test_file.clone(),
                line: 50,
                ..BreakpointSpec::default()
            }],
            Vec::new(),
            Vec::new(),
        )
        .expect("the session starts");

    let stopped = wait_for(&debugger, |snapshot| {
        matches!(snapshot.state, DebugState::Stopped { .. })
            && snapshot
                .frames
                .iter()
                .any(|frame| frame.source.as_deref() == Some(test_file.as_path()))
    });
    let frame = stopped
        .frames
        .iter()
        .find(|frame| frame.source.as_deref() == Some(test_file.as_path()))
        .expect("a frame in the test");
    assert!(frame.name.contains("shouldNotValidateWhenFirstNameEmpty"));

    debugger.send(DebugCommand::Disconnect);
    wait_for(&debugger, |snapshot| {
        snapshot.state == DebugState::Terminated
    });
    run.stop();
    let output = console_output(&run);
    assert!(
        output.contains("Listening for transport dt_socket at address:"),
        "the test JVM did not announce its port:\n{output}"
    );
    assert!(
        !output.contains("handshake failed"),
        "the test JVM saw a broken handshake:\n{output}"
    );
    assert_no_test_jvms();
}

/// The same test through Gradle: the init script puts the debug agent into the test JVM,
/// and stopping the session leaves no test worker behind.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs IDE_CORPUS, a Gradle cache and the helpers"]
async fn a_gradle_test_runs_under_the_debugger() {
    let fixture = gradle_fixture();
    let project = Arc::new(
        JavaProject::open(fixture.config.clone(), Trust::Trusted).expect("the project opens"),
    );
    ready(&project).await;
    let service = JavaDebugger::new(project.clone(), Handle::current());
    let jdk = corpus().join("jdk/17");
    let cache = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("debug-test-cache-gradle");
    let env = BuildEnv {
        jdk: Some(Jdk {
            home: jdk,
            version: "17".into(),
            feature: 17,
        }),
        cache_root: Some(cache),
        helpers: Some(helpers_dir()),
        ..BuildEnv::default()
    };
    let provider = JavaTestProvider::new(env);
    let model = ProjectModel {
        root: fixture.root.clone(),
        tool: "gradle".into(),
        resolved: true,
        ..ProjectModel::default()
    };
    let suite = "org.springframework.samples.petclinic.model.ValidatorTests";
    let request = TestRequest {
        suite: Some(suite.into()),
        case: Some("shouldNotValidateWhenFirstNameEmpty".into()),
        ..TestRequest::default()
    };
    let test_file = fixture
        .root
        .join("src/test/java/org/springframework/samples/petclinic/model/ValidatorTests.java");

    let (run, debug) = TestRun::debug_start(&provider, &model, &request).expect("the run starts");
    let debugger = service
        .attach_ready(
            debug.host,
            debug.port,
            "ValidatorTests".into(),
            None,
            vec![BreakpointSpec {
                file: test_file.clone(),
                line: 50,
                ..BreakpointSpec::default()
            }],
            Vec::new(),
            Vec::new(),
        )
        .expect("the session starts");

    let stopped = wait_for(&debugger, |snapshot| {
        matches!(snapshot.state, DebugState::Stopped { .. })
            && snapshot
                .frames
                .iter()
                .any(|frame| frame.source.as_deref() == Some(test_file.as_path()))
    });
    let frame = stopped
        .frames
        .iter()
        .find(|frame| frame.source.as_deref() == Some(test_file.as_path()))
        .expect("a frame in the test");
    assert!(frame.name.contains("shouldNotValidateWhenFirstNameEmpty"));

    debugger.send(DebugCommand::Disconnect);
    wait_for(&debugger, |snapshot| {
        snapshot.state == DebugState::Terminated
    });
    run.stop();
    assert_no_test_jvms();
}
