//! The messages of the server reach the log that Show Log opens, and the JVM flag for
//! the `sun.misc.Unsafe` warning follows the version of the JDK that runs the server.
//!
//! cargo test -p jdtls-adapter --test messages

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use jdtls_adapter::{Jdtls, JdtlsConfig};
use serde_json::json;
use sidecar::ServerSpec;

/// A server over a fake release, with its own state folder and a JDK whose `release`
/// names `version`, or none.
fn server(name: &str, version: Option<&str>) -> (Jdtls, PathBuf) {
    let root = test_support::scratch(name);
    let plugins = root.join("home").join("plugins");
    fs::create_dir_all(&plugins).unwrap();
    fs::write(plugins.join("org.eclipse.equinox.launcher_1.8.0.jar"), b"").unwrap();
    let java_home = root.join("jdk");
    fs::create_dir_all(java_home.join("bin")).unwrap();
    if let Some(version) = version {
        fs::write(
            java_home.join("release"),
            format!("JAVA_VERSION=\"{version}\"\n"),
        )
        .unwrap();
    }
    let project = root.join("project");
    fs::create_dir_all(&project).unwrap();
    let config = JdtlsConfig {
        jdtls_home: root.join("home"),
        debug_plugin: root.join("debug.jar"),
        java_home,
        project_root: project,
        state_dir: root.join("state"),
        runtimes: Vec::new(),
        maven_settings: None,
        gradle_home: None,
        gradle_user_home: None,
        gradle_java_home: None,
        offline: true,
        environment: HashMap::new(),
        max_heap_mb: None,
    };
    (Jdtls::new(config).unwrap(), root)
}

fn before_jar(args: &[String], wanted: &str) -> bool {
    let jar = args.iter().position(|arg| arg == "-jar").unwrap();
    args[..jar].iter().any(|arg| arg == wanted)
}

#[test]
fn a_jdk_that_knows_the_unsafe_flag_gets_it_before_the_jar() {
    let (new, _) = server("messages-flag-new", Some("25"));
    assert!(before_jar(
        &new.plan().args,
        "--sun-misc-unsafe-memory-access=allow"
    ));

    let (old, _) = server("messages-flag-old", Some("21"));
    assert!(
        !old.plan()
            .args
            .iter()
            .any(|arg| arg == "--sun-misc-unsafe-memory-access=allow")
    );

    let (unknown, _) = server("messages-flag-unknown", None);
    assert!(
        !unknown
            .plan()
            .args
            .iter()
            .any(|arg| arg == "--sun-misc-unsafe-memory-access=allow")
    );
}

#[test]
fn a_log_message_and_a_balloon_reach_the_log_with_their_levels() {
    let (jdtls, root) = server("messages-levels", Some("25"));
    jdtls.starting();
    jdtls.state_of(
        "window/logMessage",
        &json!({
            "type": 1,
            "message": "Cannot import Gradle project\nCaused by: Task with name 'postRelease' not found"
        }),
    );
    jdtls.state_of(
        "window/showMessage",
        &json!({"type": 2, "message": "Gradle import failed"}),
    );
    jdtls.flush_log();

    let log = jdtls.log_file();
    assert_eq!(
        log.file_name().and_then(|name| name.to_str()),
        Some("jdtls-messages.log")
    );
    let text = fs::read_to_string(&log).unwrap();
    assert!(
        text.contains(
            "Error: Cannot import Gradle project\nCaused by: Task with name 'postRelease' not found"
        ),
        "{text}"
    );
    assert!(text.contains("Warning: Gradle import failed"), "{text}");
    assert!(text.contains("jdtls 1.63"), "{text}");
    let java = root.join("jdk").display().to_string();
    assert!(text.contains(&java), "{text}");
}

#[test]
fn the_jvm_output_joins_the_log_without_the_warnings() {
    let (jdtls, root) = server("messages-jvm", Some("25"));
    jdtls.starting();
    fs::write(
        root.join("state").join("jdtls-stderr.log"),
        "WARNING: Using incubator modules: jdk.incubator.vector\n\
         Exception in thread \"main\" java.lang.RuntimeException: no build tool\n\
         \tat org.example.Main.main(Main.java:1)\n\
         WARNING: A terminally deprecated method in sun.misc.Unsafe has been called\n",
    )
    .unwrap();
    jdtls.append_jvm_output();

    let text = fs::read_to_string(jdtls.log_file()).unwrap();
    assert!(text.contains("--- JVM output ---"), "{text}");
    assert!(
        text.contains("Exception in thread \"main\" java.lang.RuntimeException: no build tool"),
        "{text}"
    );
    assert!(
        text.contains("\tat org.example.Main.main(Main.java:1)"),
        "{text}"
    );
    assert!(!text.contains("WARNING: Using incubator modules"), "{text}");
    assert!(
        !text.contains("WARNING: A terminally deprecated method"),
        "{text}"
    );
}

#[test]
fn the_log_moves_to_its_previous_name_past_five_mebibytes() {
    let (jdtls, _) = server("messages-rotation", Some("25"));
    jdtls.starting();
    let big = "x".repeat(5 * 1024 * 1024 + 100);
    jdtls.state_of("window/logMessage", &json!({"type": 3, "message": big}));
    jdtls.state_of(
        "window/logMessage",
        &json!({"type": 3, "message": "the next line"}),
    );
    jdtls.flush_log();

    let log = jdtls.log_file();
    let rotated = log.with_file_name("jdtls-messages.1.log");
    assert!(rotated.is_file(), "{}", rotated.display());
    let previous = fs::metadata(&rotated).unwrap().len();
    let current = fs::metadata(&log).unwrap().len();
    assert!(previous > 5 * 1024 * 1024, "{previous}");
    assert!(current < 5 * 1024 * 1024, "{current}");
    let text = fs::read_to_string(&log).unwrap();
    assert!(text.contains("the next line"), "{text}");
}
