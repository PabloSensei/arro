//! Requests of the server that are not the job of the adapter or of a specification.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use jdtls_adapter::{Jdtls, JdtlsConfig};
use lsp_client::ResponseError;
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
fn the_refresh_requests_go_on_to_the_consumer_of_the_sidecar() {
    let (adapter, _) = server("server-requests-refresh");
    for method in ["workspace/codeLens/refresh", "workspace/inlayHint/refresh"] {
        match adapter.request(method, json!({})) {
            Err(error) => assert_eq!(error.code, ResponseError::METHOD_NOT_FOUND, "{method}"),
            Ok(value) => panic!("{method} answered {value}"),
        }
    }
}

#[test]
fn the_other_requests_keep_their_answers() {
    let (adapter, _) = server("server-requests-other");
    let sections = json!({"items": [{"section": "java.completion"}, {}]});
    let values = adapter
        .request("workspace/configuration", sections)
        .unwrap();
    let values = values.as_array().expect("a list for the asked sections");
    assert_eq!(values.len(), 2);
    assert!(values[0].is_object(), "the completion settings");
    assert!(values[1].is_object(), "the whole settings");
    assert_eq!(
        adapter.request("workspace/applyEdit", json!({})).unwrap(),
        json!({"applied": false})
    );
    assert_eq!(
        adapter
            .request("window/workDoneProgress/create", json!({}))
            .unwrap(),
        json!(null)
    );
}
