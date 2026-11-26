//! A new start of the server clears the import that the old one was running.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use jdtls_adapter::{Jdtls, JdtlsConfig};
use serde_json::json;
use sidecar::ServerSpec;

fn server(name: &str) -> (Jdtls, PathBuf) {
    let root = test_support::scratch(name);
    let plugins = root.join("home").join("plugins");
    fs::create_dir_all(&plugins).unwrap();
    fs::write(plugins.join("org.eclipse.equinox.launcher_1.8.0.jar"), b"").unwrap();
    let project = root.join("project");
    fs::create_dir_all(&project).unwrap();
    let config = JdtlsConfig {
        jdtls_home: root.join("home"),
        debug_plugin: root.join("debug.jar"),
        java_home: root.join("jdk"),
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
fn a_start_clears_an_import_that_the_old_server_did_not_finish() {
    let (adapter, _) = server("import-reset");
    let importing = adapter.watch_importing();
    assert!(!*importing.borrow());

    adapter.state_of(
        "$/progress",
        &json!({
            "token": "1",
            "value": {"kind": "begin", "title": "Importing Gradle project(s)"}
        }),
    );
    assert!(*importing.borrow(), "the Gradle import began");

    adapter.starting();
    assert!(
        !*importing.borrow(),
        "a process that starts now imports the project again"
    );
}
