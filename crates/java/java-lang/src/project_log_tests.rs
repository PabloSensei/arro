//! Show Log opens the messages of the server, not the warnings of the JVM.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use jdtls_adapter::{ImportFailure, Jdtls, JdtlsConfig};
use serde_json::json;
use sidecar::ServerSpec;
use tokio::sync::watch;

use super::ProjectProblems;

fn server(name: &str) -> (Jdtls, PathBuf) {
    let root = test_support::scratch(name);
    let plugins = root.join("home").join("plugins");
    fs::create_dir_all(&plugins).unwrap();
    fs::write(plugins.join("org.eclipse.equinox.launcher_1.8.0.jar"), b"").unwrap();
    let java_home = root.join("jdk");
    fs::create_dir_all(java_home.join("bin")).unwrap();
    fs::write(java_home.join("release"), "JAVA_VERSION=\"25\"\n").unwrap();
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

#[test]
fn a_failed_import_opens_the_messages_without_the_jvm_warnings() {
    let (spec, root) = server("project-log");
    spec.starting();
    spec.state_of(
        "window/logMessage",
        &json!({
            "type": 1,
            "message": "Cannot import Gradle project\nCaused by: Task with name 'postRelease' not found"
        }),
    );
    fs::write(
        root.join("state").join("jdtls-stderr.log"),
        "WARNING: Using incubator modules: jdk.incubator.vector\n\
         Exception in thread \"main\" java.lang.RuntimeException: the import ended\n",
    )
    .unwrap();
    let failure = ImportFailure {
        file: root.join("build.gradle"),
        line: 3,
        message: "Cannot import Gradle project\nCaused by: Task with name 'postRelease' not found"
            .to_owned(),
        summary: "Task with name 'postRelease' not found".to_owned(),
        jdk: None,
        tool: Some(jdtls_adapter::BuildTool::Gradle),
        retry: false,
    };
    let (_sender, failures) = watch::channel(Some(failure));

    let problems = ProjectProblems::list(&failures, &spec);
    let problem = problems.first().expect("the failed import is a problem");
    let log = problem.log.as_ref().expect("the problem names a log");
    assert_eq!(
        log.file_name().and_then(|name| name.to_str()),
        Some("jdtls-messages.log")
    );

    let text = fs::read_to_string(log).unwrap();
    assert!(
        text.contains("Caused by: Task with name 'postRelease' not found"),
        "{text}"
    );
    assert!(text.contains("--- JVM output ---"), "{text}");
    assert!(
        text.contains("Exception in thread \"main\" java.lang.RuntimeException: the import ended"),
        "{text}"
    );
    assert!(
        !text.lines().any(|line| line.starts_with("WARNING:")),
        "{text}"
    );
}
