//! The Gradle daemon of an import stops on its own shortly after the import.
//!
//! `cargo test -p java-lang --test gradle_daemon -- --ignored --test-threads=1` needs
//! `IDE_CORPUS`, a filled `MAVEN_USER_HOME` (the Gradle cache lives next to it) and
//! PowerShell. The corpus stays untouched: the project is a copy in the target folder.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use java_lang::{JavaSession, JdtlsConfig, Runtime, Trust};

/// The timeout the import passes to the daemon.
const IDLE_MS: u64 = 30_000;
/// The daemon has to be gone this long after the import.
const LIMIT: Duration = Duration::from_secs(90);
/// The import itself gets this long.
const READY_LIMIT: Duration = Duration::from_secs(600);

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

/// A project without dependencies whose wrapper distribution is already in the
/// shared Gradle home, so the import needs no network.
fn write_project(root: &Path) {
    let wrapper_source = corpus().join("src/petclinic/gradle/wrapper");
    let wrapper = root.join("gradle/wrapper");
    fs::create_dir_all(&wrapper).unwrap();
    for name in ["gradle-wrapper.jar", "gradle-wrapper.properties"] {
        fs::copy(wrapper_source.join(name), wrapper.join(name)).unwrap();
    }
    fs::write(
        root.join("settings.gradle"),
        "rootProject.name = 'daemon-probe'\n",
    )
    .unwrap();
    fs::write(root.join("build.gradle"), "plugins {\n    id 'java'\n}\n").unwrap();
    fs::create_dir_all(root.join("src/main/java/probe")).unwrap();
    fs::write(
        root.join("src/main/java/probe/App.java"),
        "package probe;\n\npublic class App {\n}\n",
    )
    .unwrap();
}

/// The process ids of the Gradle daemons that run now.
fn daemons() -> HashSet<u32> {
    let script = "Get-CimInstance Win32_Process -Filter \"Name='java.exe'\" | \
                  Where-Object { $_.CommandLine -and $_.CommandLine.Contains('GradleDaemon') } | \
                  ForEach-Object { $_.ProcessId }";
    let Ok(output) = Command::new("powershell")
        .args(["-NoProfile", "-Command", script])
        .output()
    else {
        return HashSet::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .filter_map(|pid| pid.parse().ok())
        .collect()
}

fn running(process: u32) -> bool {
    daemons().contains(&process)
}

#[test]
#[ignore = "needs IDE_CORPUS, the Gradle cache and a Gradle project import"]
fn the_daemon_of_the_import_stops_after_its_idle_timeout() {
    let temp = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    let root = temp.join("gradle-daemon-probe");
    let state = temp.join("gradle-daemon-probe-state");
    for folder in [&root, &state] {
        let _ = fs::remove_dir_all(folder);
    }
    fs::create_dir_all(&root).unwrap();
    write_project(&root);
    let before = daemons();

    let config = JdtlsConfig {
        jdtls_home: corpus().join("tools/jdtls-1.63.0"),
        debug_plugin: corpus().join(
            "tools/vscode-java-debug-0.60.0/extension/server/com.microsoft.java.debug.plugin-0.53.2.jar",
        ),
        java_home: jdk("25"),
        project_root: root.clone(),
        state_dir: state.join("server"),
        runtimes: vec![
            Runtime {
                name: "JavaSE-17".to_owned(),
                home: jdk("17"),
                default: false,
            },
            Runtime {
                name: "JavaSE-25".to_owned(),
                home: jdk("25"),
                default: true,
            },
        ],
        maven_settings: None,
        gradle_home: None,
        gradle_user_home: gradle_home(),
        gradle_java_home: Some(jdk("17")),
        offline: false,
        environment: std::collections::HashMap::from([(
            "IDE_GRADLE_IDLE_TIMEOUT_MS".to_owned(),
            IDLE_MS.to_string(),
        )]),
    };
    let session = JavaSession::open(config, Trust::Trusted).expect("the server starts");
    let (sender, statuses) = channel();
    session.watch_status(move |text| {
        sender.send(text).ok();
    });

    let mut started = HashSet::new();
    let began = Instant::now();
    let mut last_scan = Instant::now() - Duration::from_secs(10);
    loop {
        if last_scan.elapsed() >= Duration::from_secs(2) {
            started.extend(daemons().difference(&before).copied());
            last_scan = Instant::now();
        }
        match statuses.recv_timeout(Duration::from_millis(250)) {
            Ok(text) if text.ends_with("ready") => break,
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
    started.extend(daemons().difference(&before).copied());
    assert!(
        !started.is_empty(),
        "the import started no Gradle daemon, so the test cannot follow its end"
    );

    let imported = Instant::now();
    while started.iter().any(|process| running(*process)) {
        assert!(
            imported.elapsed() < LIMIT,
            "a Gradle daemon of the import is still alive {:?} after it, processes {started:?}",
            imported.elapsed()
        );
        std::thread::sleep(Duration::from_secs(2));
    }
    println!(
        "the daemon of the import stopped {} s after the import (limit {} s)",
        imported.elapsed().as_secs_f64(),
        LIMIT.as_secs()
    );
}
