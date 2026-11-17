//! Hot swap on the real pair: jdtls with `java-debug`, a Maven and a Gradle project
//! built by the tool itself, and a program that keeps calling a method of a class with a
//! lambda.
//!
//! `cargo test -p java-lang --test hot_swap -- --ignored --test-threads=1` needs
//! `IDE_CORPUS` and `MAVEN_USER_HOME`; the projects are copies in a folder of the target
//! directory, so the corpus stays untouched. The Gradle daemon is allowed to outlive the
//! test; nothing else is.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::mpsc::{RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use base::Cancel;
use dap_client::{Command as DebugCommand, DebugService, DebugState, Debugger, Snapshot};
use java_exec::JvmFlags;
use java_lang::{JavaSession, JdtlsConfig, Runtime, State};
use java_project::{BuildEnv, Jdk};
use platform::CanonPath;
use proc_sup::ProcessSpec;

const READY: Duration = Duration::from_secs(600);
const LIMIT: Duration = Duration::from_secs(120);

fn corpus() -> PathBuf {
    PathBuf::from(std::env::var("IDE_CORPUS").expect("set IDE_CORPUS"))
}

fn jdk(version: &str) -> PathBuf {
    let home =
        fs::canonicalize(corpus().join("jdk").join(version)).expect("the corpus has the JDK");
    // A batch file passes `JAVA_HOME` to cmd.exe, which does not understand the
    // extended-length prefix canonicalize adds on Windows.
    PathBuf::from(home.display().to_string().replace(r"\\?\", ""))
}

/// A Maven cache outside the default place, and the Gradle home next to it.
fn maven_home() -> PathBuf {
    PathBuf::from(std::env::var("MAVEN_USER_HOME").expect("set MAVEN_USER_HOME"))
}

fn gradle_home() -> Option<PathBuf> {
    if let Ok(home) = std::env::var("GRADLE_USER_HOME") {
        return Some(home.into());
    }
    let maven = maven_home();
    let gradle = maven.parent()?.join("gradle");
    gradle.is_dir().then_some(gradle)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tool {
    Maven,
    Gradle,
}

impl Tool {
    fn name(self) -> &'static str {
        match self {
            Self::Maven => "maven",
            Self::Gradle => "gradle",
        }
    }

    fn outputs(self, root: &Path) -> Vec<PathBuf> {
        match self {
            Self::Maven => vec![root.join("target/classes")],
            Self::Gradle => vec![root.join("build/classes/java/main")],
        }
    }
}

/// A floating-point source whose method body changes and then grows a method.
const SOURCE: &str = r#"package demo;

import java.util.function.Supplier;

public class App {

    static int value() {
        return 1;
    }

    public static void main(String[] args) throws Exception {
        Supplier<String> marker = () -> "probe";
        while (true) {
            System.out.println("value=" + value() + " " + marker.get());
            Thread.sleep(50);
        }
    }
}
"#;

const SOURCE_RELOADED: &str = r#"package demo;

import java.util.function.Supplier;

public class App {

    static int value() {
        return 2;
    }

    public static void main(String[] args) throws Exception {
        Supplier<String> marker = () -> "probe";
        while (true) {
            System.out.println("value=" + value() + " " + marker.get());
            Thread.sleep(50);
        }
    }
}
"#;

const SOURCE_WITH_METHOD: &str = r#"package demo;

import java.util.function.Supplier;

public class App {

    static int value() {
        return 3;
    }

    static int extra() {
        return 3;
    }

    public static void main(String[] args) throws Exception {
        Supplier<String> marker = () -> "probe";
        while (true) {
            System.out.println("value=" + value() + " " + marker.get());
            Thread.sleep(50);
        }
    }
}
"#;

/// Writes a small project of the tool into a folder of its own and returns its root.
fn write_project(tool: Tool) -> PathBuf {
    let root = test_support::scratch(&format!("hot-swap-{}", tool.name()));
    let sources = root.join("src/main/java/demo");
    fs::create_dir_all(&sources).unwrap();
    fs::write(sources.join("App.java"), SOURCE).unwrap();
    fs::write(root.join(".java-version"), "21\n").unwrap();
    match tool {
        Tool::Maven => {
            let wrapper_source = corpus().join("src/petclinic");
            for name in ["mvnw", "mvnw.cmd"] {
                fs::copy(wrapper_source.join(name), root.join(name))
                    .unwrap_or_else(|error| panic!("the corpus has the Maven wrapper: {error}"));
            }
            let wrapper = root.join(".mvn/wrapper");
            fs::create_dir_all(&wrapper).unwrap();
            fs::copy(
                wrapper_source.join(".mvn/wrapper/maven-wrapper.properties"),
                wrapper.join("maven-wrapper.properties"),
            )
            .unwrap_or_else(|error| panic!("the corpus has the Maven wrapper properties: {error}"));
            let m2 = maven_home();
            let repository = m2.join("repository").display().to_string();
            fs::write(
                root.join("pom.xml"),
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
                 <project xmlns=\"http://maven.apache.org/POM/4.0.0\">\n\
                 <modelVersion>4.0.0</modelVersion>\n\
                 <groupId>com.example</groupId><artifactId>hot-swap</artifactId>\n\
                 <version>1.0-SNAPSHOT</version>\n\
                 <properties><maven.compiler.release>21</maven.compiler.release>\n\
                 <project.build.sourceEncoding>UTF-8</project.build.sourceEncoding></properties>\n\
                 </project>\n",
            )
            .unwrap();
            let settings = root.join("settings.xml");
            fs::write(
                &settings,
                format!(
                    "<settings><localRepository>{repository}</localRepository><offline>true</offline></settings>"
                ),
            )
            .unwrap();
            let config = root.join(".mvn");
            fs::create_dir_all(&config).unwrap();
            fs::write(
                config.join("maven.config"),
                format!(
                    "-o\n-s\n{}\n-Dmaven.repo.local={repository}\n",
                    settings.display()
                ),
            )
            .unwrap();
        }
        Tool::Gradle => {
            let wrapper_source = corpus().join("src/petclinic/gradle/wrapper");
            let wrapper = root.join("gradle/wrapper");
            fs::create_dir_all(&wrapper).unwrap();
            for name in ["gradle-wrapper.jar", "gradle-wrapper.properties"] {
                fs::copy(wrapper_source.join(name), wrapper.join(name))
                    .unwrap_or_else(|error| panic!("the corpus has the Gradle wrapper: {error}"));
            }
            fs::write(
                root.join("settings.gradle"),
                "rootProject.name = 'hot-swap'\n",
            )
            .unwrap();
            fs::write(
                root.join("build.gradle"),
                "plugins {\n    id 'java'\n}\n\n\
                 java {\n    sourceCompatibility = JavaVersion.VERSION_21\n    targetCompatibility = JavaVersion.VERSION_21\n}\n",
            )
            .unwrap();
        }
    }
    root
}

struct Fixture {
    root: PathBuf,
    tool: Arc<dyn project_model::BuildSystem>,
    outputs: Vec<PathBuf>,
    /// A copy of the tool output the debuggee runs from, so that a later build of the
    /// language server never changes the bytes already loaded.
    run_classes: PathBuf,
    state_dir: PathBuf,
    session: JavaSession,
}

fn fixture(tool: Tool) -> Fixture {
    let root = write_project(tool);
    let jdk21 = Jdk::at(&jdk("21")).expect("the corpus JDK 21");
    if tool == Tool::Gradle && gradle_home().is_none() {
        panic!("set GRADLE_USER_HOME or keep the Gradle home next to MAVEN_USER_HOME");
    }
    let mut vars = Vec::new();
    if let Some(home) = gradle_home() {
        vars.push((OsString::from("GRADLE_USER_HOME"), OsString::from(home)));
    }
    let env = BuildEnv {
        jdk: Some(jdk21.clone()),
        vars,
        cache_root: Some(root.join(".ide-cache")),
        ..BuildEnv::default()
    };
    let tool_system = java_project::detect_build_system(&root, &env).expect("a build tool");
    // One clean build before the debugger starts: the same compiler and the same bytes.
    run(&tool_system, &root, false);

    let state_dir = root.with_file_name(format!(
        "{}-state",
        root.file_name().unwrap().to_string_lossy()
    ));
    let settings = root.join("settings.xml");
    let runtime = |name: &str, version: &str, default| Runtime {
        name: name.to_owned(),
        home: jdk(version),
        default,
    };
    let config = JdtlsConfig {
        jdtls_home: corpus().join("tools/jdtls-1.63.0"),
        debug_plugin: corpus().join(
            "tools/vscode-java-debug-0.60.0/extension/server/com.microsoft.java.debug.plugin-0.53.2.jar",
        ),
        java_home: jdk("25"),
        project_root: root.clone(),
        state_dir: state_dir.clone(),
        runtimes: vec![
            runtime("JavaSE-17", "17", false),
            runtime("JavaSE-21", "21", true),
        ],
        maven_settings: (tool == Tool::Maven).then_some(settings),
        gradle_home: None,
        gradle_user_home: gradle_home(),
        gradle_java_home: Some(jdk21.home.clone()),
        offline: tool == Tool::Maven,
        environment: std::collections::HashMap::new(),
    };
    let session = JavaSession::open(config, sidecar::Trust::Trusted).expect("jdtls starts");
    wait_ready(&session);
    let entry = root.join("src/main/java/demo/App.java");
    let uri = CanonPath::new(&entry)
        .ok()
        .and_then(|path| lsp_client::uri::file_uri(&path))
        .expect("the source has a file URI");
    let started = Instant::now();
    loop {
        let resolved = session.handle().block_on(async {
            let extension = java_lang::DebugExtension::new(session.project());
            extension.resolve_main_class(&uri).await
        });
        if matches!(&resolved, Ok(Some(main)) if main.main_class == "demo.App") {
            break;
        }
        assert!(
            started.elapsed() < READY,
            "the server did not import the project: {resolved:?}"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
    // The import may compile in the background; this waits for that build, then the
    // tool writes again, and the debuggee runs from a copy of the tool output.
    build_workspace_for(&session);
    touch_path(&root.join("src/main/java/demo/App.java"));
    run(&tool_system, &root, false);
    let outputs = tool.outputs(&root);
    let run_classes = test_support::scratch(&format!("hot-swap-{}-run", tool.name()));
    copy_tree(&outputs[0], &run_classes);
    Fixture {
        outputs,
        root,
        tool: tool_system,
        run_classes,
        state_dir,
        session,
    }
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&path, &to.join(entry.file_name()));
        } else {
            fs::copy(&path, to.join(entry.file_name())).unwrap();
        }
    }
}

fn wait_ready(session: &JavaSession) {
    let mut status = session.status();
    let ready = session.handle().block_on(async {
        tokio::time::timeout(
            READY,
            status.wait_for(|status| status.state == State::Ready),
        )
        .await
    });
    ready.expect("jdtls did not get ready in time").unwrap();
}

/// Build Project as the window runs it: the command of the tool, waited to the end.
fn run(tool: &Arc<dyn project_model::BuildSystem>, root: &Path, tests: bool) {
    let spec = tool.build(root, tests).expect("the build command");
    let captured = proc_sup::capture(&spec, &Cancel::new()).expect("the build starts");
    assert!(
        captured.status.success(),
        "the build failed:\n{}\n{}",
        captured.stdout,
        captured.stderr
    );
}

fn touch_path(path: &Path) {
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(std::time::SystemTime::now())
        .unwrap();
}

/// Waits for the build jobs of the server, including the one of a fresh import.
fn build_workspace_for(session: &JavaSession) {
    let reply = session.handle().block_on(async {
        let router = session.project().router().clone();
        router
            .extension("java/buildWorkspace", serde_json::json!(false))
            .await
    });
    assert!(reply.is_ok(), "java/buildWorkspace failed: {reply:?}");
}

fn start(fixture: &Fixture) -> Debugger {
    let jdk = jdk("21");
    let flags = JvmFlags::for_jdk(21);
    let entry = fixture.root.join("src/main/java/demo/App.java");
    let program = ProcessSpec::new(jdk.join("bin/java.exe"))
        .cwd(&fixture.root)
        .arg(flags.debug_agent("127.0.0.1", 0, false))
        .arg("-cp")
        .arg(fixture.run_classes.clone())
        .arg("demo.App");
    let service =
        java_lang::JavaDebugger::new(fixture.session.project().clone(), fixture.session.handle());
    service
        .start(
            program,
            "HotSwap".into(),
            Some(entry),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
        .expect("the debug session starts")
}

fn wait_for(
    debugger: &Debugger,
    what: &str,
    mut done: impl FnMut(&Snapshot) -> bool,
) -> Arc<Snapshot> {
    let started = Instant::now();
    let mut last = None;
    loop {
        if let Some(snapshot) = debugger.poll() {
            if done(&snapshot) {
                return snapshot;
            }
            last = Some(snapshot);
        }
        assert!(
            started.elapsed() < LIMIT,
            "the test waited for {what} in vain; last state: {:?}; console: {:?}",
            last.as_ref().map(|snapshot| &snapshot.state),
            last.as_ref().map(|snapshot| &snapshot.console)
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn output_count(snapshot: &Snapshot, text: &str) -> usize {
    snapshot
        .console
        .iter()
        .filter(|line| line.text.contains(text))
        .count()
}

/// Tells jdtls about the change the way the editor does on save: the first time the
/// document opens with the old text, then the full new text arrives and is saved.
fn notify_source(fixture: &Fixture, open: bool, version: i32, text: &str) {
    let path = fixture.root.join("src/main/java/demo/App.java");
    let uri = CanonPath::new(&path)
        .ok()
        .and_then(|path| lsp_client::uri::file_uri(&path))
        .expect("the source has a file URI");
    let router = fixture.session.project().router();
    if open {
        router.open(uri.clone(), "java", 1, Arc::from(SOURCE));
    }
    router.change(
        &uri,
        version,
        Arc::from(text),
        vec![lsp_client::lsp_types::TextDocumentContentChangeEvent {
            range: None,
            range_length: None,
            text: text.to_owned(),
        }],
    );
    router.save(&uri);
    router.file_changed(uri.as_str());
}

fn reload(
    service: &dyn DebugService,
    debugger: &Debugger,
    outputs: &[PathBuf],
) -> dap_client::ReloadOutcome {
    let (sender, answer) = channel();
    assert!(
        service.reload_classes(debugger.clone(), outputs.to_vec(), sender),
        "the language reloads classes"
    );
    match answer.recv_timeout(LIMIT) {
        Ok(outcome) => outcome,
        Err(RecvTimeoutError::Timeout) => panic!("the hot swap did not answer"),
        Err(RecvTimeoutError::Disconnected) => panic!("the hot swap went away"),
    }
}

/// The java processes whose command line contains `marker`.
fn processes_with(marker: &str) -> Vec<u32> {
    let script = format!(
        "Get-CimInstance Win32_Process -Filter \"Name='java.exe'\" | Where-Object {{ \
         $_.CommandLine -and $_.CommandLine.Contains('{marker}') }} | \
         ForEach-Object {{ $_.ProcessId }}"
    );
    let output = Command::new("powershell")
        .args(["-NoProfile", "-Command", &script])
        .output()
        .expect("powershell runs");
    String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .filter_map(|pid| pid.parse().ok())
        .collect()
}

fn wait_no_processes(marker: &str, what: &str) {
    let started = Instant::now();
    loop {
        let left = processes_with(marker);
        if left.is_empty() {
            return;
        }
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "{what} outlived the test: {left:?}"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn hot_swap_on(tool: Tool) {
    let fixture = fixture(tool);
    let service =
        java_lang::JavaDebugger::new(fixture.session.project().clone(), fixture.session.handle());
    // Every session of a program this process launches is counted, and the server keeps
    // compilation on save off until the last one ends.
    let session_token = service
        .begin_session()
        .expect("the language counts debug sessions");
    let debugger = start(&fixture);
    let seen = wait_for(&debugger, "the first value", |snapshot| {
        output_count(snapshot, "value=1") > 0
    });
    let process_id = seen.process_id.expect("the adapter reports the debuggee");

    // The body of a method changes; the same compiler writes the new bytes.
    fs::write(
        fixture.root.join("src/main/java/demo/App.java"),
        SOURCE_RELOADED,
    )
    .unwrap();
    notify_source(&fixture, true, 2, SOURCE_RELOADED);
    run(&fixture.tool, &fixture.root, false);
    let outcome = reload(&service, &debugger, &fixture.outputs);
    assert_eq!(outcome.error, None, "a changed body is reloaded");
    assert!(
        outcome.reloaded.iter().any(|class| class.ends_with("App")),
        "the adapter names the class: {:?}",
        outcome.reloaded
    );
    wait_for(&debugger, "the reloaded value", |snapshot| {
        output_count(snapshot, "value=2") > 0
    });

    // A new method cannot be added to a loaded class, and the program keeps running.
    let before = wait_for(&debugger, "the old value", |_| true);
    fs::write(
        fixture.root.join("src/main/java/demo/App.java"),
        SOURCE_WITH_METHOD,
    )
    .unwrap();
    notify_source(&fixture, false, 3, SOURCE_WITH_METHOD);
    run(&fixture.tool, &fixture.root, false);
    let refused = reload(&service, &debugger, &fixture.outputs);
    assert!(
        refused.error.is_some(),
        "a new method is not reloadable: {refused:?}"
    );
    assert!(
        refused
            .error
            .as_deref()
            .is_some_and(|error| error.contains("method not implemented")),
        "the adapter says why: {:?}",
        refused.error
    );
    let old_lines = output_count(&before, "value=2");
    wait_for(&debugger, "the program to keep running", |snapshot| {
        output_count(snapshot, "value=2") > old_lines
    });
    assert!(
        !matches!(
            wait_for(&debugger, "the session to stay", |_| true).state,
            DebugState::Terminated
        ),
        "the session survives a refused hot swap"
    );

    assert!(debugger.send(DebugCommand::Disconnect));
    wait_for(&debugger, "the end of the session", |snapshot| {
        snapshot.state == DebugState::Terminated
    });
    drop(session_token);
    drop(fixture.session);
    assert!(!is_running(process_id), "the debuggee outlived the session");

    let root_marker = fixture.root.display().to_string();
    wait_no_processes(&root_marker, "the debuggee");
    let state_marker = fixture.state_dir.display().to_string();
    wait_no_processes(&state_marker, "jdtls");
}

fn is_running(process_id: u32) -> bool {
    let output = Command::new("tasklist")
        .args(["/FI", &format!("PID eq {process_id}"), "/NH"])
        .output()
        .expect("tasklist runs");
    String::from_utf8_lossy(&output.stdout).contains(&process_id.to_string())
}

#[test]
#[ignore = "needs IDE_CORPUS, MAVEN_USER_HOME and a filled package cache"]
fn maven_reloads_a_changed_body_and_refuses_a_new_method() {
    hot_swap_on(Tool::Maven);
}

#[test]
#[ignore = "needs IDE_CORPUS, MAVEN_USER_HOME and a filled package cache"]
fn gradle_reloads_a_changed_body_and_refuses_a_new_method() {
    hot_swap_on(Tool::Gradle);
}
