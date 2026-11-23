//! A configuration folder of the server that is gone takes the model of the workspace
//! with it, so the next start imports the project again.
//!
//! The session of 2026-11-21 11:54 of a project that was last opened days before began
//! with a reset `config` folder and the model of the old release: the Gradle import of
//! the new start refused the JVM of that model, and the editor kept the class path of
//! the previous import, which had no test dependencies.
//!
//! `cargo test -p java-lang --test config_reset -- --ignored --test-threads=1` needs
//! `IDE_CORPUS`, a filled `MAVEN_USER_HOME` (the Gradle cache lives next to it). The
//! corpus stays untouched: the project is a copy in the temporary folder of the test.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use java_lang::{JavaSession, JdtlsConfig, Runtime, Trust};

/// The import itself gets this long.
const READY_LIMIT: Duration = Duration::from_secs(600);
/// The log of the import gets this long to reach its end after the ready status.
const LOG_LIMIT: Duration = Duration::from_secs(120);
/// Windows may still hold the files of the server it just ended.
const REMOVE_LIMIT: Duration = Duration::from_secs(30);

fn corpus() -> PathBuf {
    PathBuf::from(std::env::var("IDE_CORPUS").expect("set IDE_CORPUS"))
}

fn jdk(version: &str) -> PathBuf {
    fs::canonicalize(corpus().join("jdk").join(version)).expect("the corpus has the JDK")
}

fn gradle_home() -> Option<PathBuf> {
    if let Ok(home) = std::env::var("GRADLE_USER_HOME") {
        return Some(home.into());
    }
    let maven = PathBuf::from(std::env::var("MAVEN_USER_HOME").ok()?);
    let gradle = maven.parent()?.join("gradle");
    gradle.is_dir().then_some(gradle)
}

/// PetClinic as a copy, without the folders a build and a repository leave behind.
fn copy_project(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        let lower = name.to_string_lossy().to_lowercase();
        if matches!(
            lower.as_str(),
            ".git" | "build" | ".gradle" | "target" | "node_modules"
        ) {
            continue;
        }
        if entry.file_type().unwrap().is_dir() {
            copy_project(&entry.path(), &to.join(&name));
        } else {
            fs::copy(entry.path(), to.join(&name)).unwrap();
        }
    }
}

fn config(root: &Path, state: &Path) -> JdtlsConfig {
    JdtlsConfig {
        jdtls_home: corpus().join("tools/jdtls-1.63.0"),
        debug_plugin: corpus().join(
            "tools/vscode-java-debug-0.60.0/extension/server/com.microsoft.java.debug.plugin-0.53.2.jar",
        ),
        java_home: jdk("25"),
        project_root: root.to_path_buf(),
        state_dir: state.to_path_buf(),
        runtimes: vec![
            Runtime {
                name: "JavaSE-17".to_owned(),
                home: jdk("17"),
                default: true,
            },
            Runtime {
                name: "JavaSE-25".to_owned(),
                home: jdk("25"),
                default: false,
            },
        ],
        maven_settings: None,
        gradle_home: None,
        gradle_user_home: gradle_home(),
        gradle_java_home: Some(jdk("17")),
        offline: false,
        environment: HashMap::new(),
        max_heap_mb: None,
    }
}

/// Waits until the server imports the project, and fails on an import failure.
fn wait_ready(session: &JavaSession) -> String {
    let (sender, statuses) = channel();
    session.watch_status(move |text| {
        sender.send(text).ok();
    });
    let began = Instant::now();
    loop {
        match statuses.recv_timeout(Duration::from_millis(250)) {
            Ok(text) if text.ends_with("ready") => return text,
            Ok(text) if text.contains("failed") => panic!("the import failed: {text}"),
            Ok(_) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => panic!("the status stream ended"),
        }
        assert!(
            began.elapsed() < READY_LIMIT,
            "the import did not finish in {:?}",
            READY_LIMIT
        );
    }
}

/// The whole log of the server, or nothing before it writes one.
fn log_text(log: &Path) -> String {
    String::from_utf8_lossy(&fs::read(log).unwrap_or_default()).into_owned()
}

/// Waits until the log of the server names the end of the import.
fn wait_for_log(log: &Path, line: &str) -> String {
    let began = Instant::now();
    loop {
        let text = log_text(log);
        if text.contains(line) {
            return text;
        }
        assert!(
            began.elapsed() < LOG_LIMIT,
            "the log did not reach {line:?} in {LOG_LIMIT:?}:\n{text}"
        );
        std::thread::sleep(Duration::from_secs(1));
    }
}

/// The folders of the projects in the model of the workspace.
fn project_models(workspace: &Path) -> PathBuf {
    workspace
        .join(".metadata")
        .join(".plugins")
        .join("org.eclipse.core.resources")
        .join(".projects")
}

fn remove_until_gone(path: &Path) {
    let began = Instant::now();
    while path.exists() {
        match fs::remove_dir_all(path) {
            Ok(()) => return,
            Err(_) if began.elapsed() < REMOVE_LIMIT => {
                std::thread::sleep(Duration::from_millis(500));
            }
            Err(error) => panic!("{path:?} is still there: {error}"),
        }
    }
}

#[test]
#[ignore = "needs IDE_CORPUS, the Gradle cache and two imports of a project"]
fn a_configuration_removed_between_starts_reimports_the_project() {
    let folder = test_support::TempDir::new("config-reset");
    let root = folder.path().join("petclinic");
    copy_project(&corpus().join("src/petclinic"), &root);
    let state = folder.path().join("state");
    let workspace = state.join("workspace");
    let log = workspace.join(".metadata").join(".log");

    let first =
        JavaSession::open(config(&root, &state), Trust::Trusted).expect("the server starts");
    wait_ready(&first);
    let models = project_models(&workspace);
    let stale = fs::read_dir(&models)
        .expect("the import leaves the model of the project")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .next()
        .expect("the import leaves the model of the project");
    // The model of this import, which a start before an update would keep.
    fs::write(stale.join("stale-model"), b"model of the previous start").unwrap();
    drop(first);

    // The reset of `config` that T-0379 does when the release of the server changes,
    // while the workspace stays as it was.
    remove_until_gone(&state.join("config"));
    assert!(stale.is_dir(), "the test keeps the model it marks");
    // The new start writes a log of its own, so the whole file belongs to it.
    let _ = fs::remove_file(&log);

    let second =
        JavaSession::open(config(&root, &state), Trust::Trusted).expect("the server starts again");
    wait_ready(&second);
    let text = wait_for_log(&log, ">> build jobs finished");

    assert!(
        !stale.join("stale-model").exists(),
        "the model of the previous start went with the configuration folder"
    );
    assert!(
        fs::read_dir(project_models(&workspace)).is_ok_and(|mut models| models.next().is_some()),
        "the new start writes the model of the project again"
    );
    assert!(
        text.contains("must be synchronized"),
        "the new start imports the project:\n{text}"
    );
    assert!(
        !text.contains("cannot be resolved"),
        "the new start has the dependencies of the project:\n{text}"
    );
}
