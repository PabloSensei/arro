//! Project Structure on a real jdtls: another SDK of the project changes what the server
//! sees, without a restart of the window. The project is written here, so the corpus is
//! only read.
//!
//! cargo test -p java-lang --test environment -- --ignored --test-threads=1

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use futures_util::StreamExt;
use java_lang::{JavaProject, JdtlsConfig, Runtime, State};
use java_project::Jdk;
use lang_api::{DiagnosticsQuery, LanguageSupport, Severity, ask};
use lsp_client::uri::file_uri;
use platform::CanonPath;
use sidecar::{Status, Trust};
use tokio::sync::watch;
use tokio::time::{sleep, timeout};

const READY: Duration = Duration::from_secs(600);
const ANSWER: Duration = Duration::from_secs(120);

/// `List.getFirst` came with Java 21, so a project with level 21 that resolves against a
/// 17 SDK has an error there and a project on a 21 SDK does not.
const PROBE: &str = r#"package probe;

import java.util.List;

public class Probe {

    public String first(List<String> names) {
        return names.getFirst();
    }
}
"#;

fn corpus() -> PathBuf {
    PathBuf::from(std::env::var("IDE_CORPUS").expect("set IDE_CORPUS"))
}

fn jdk(version: &str) -> Jdk {
    let home = fs::canonicalize(corpus().join("jdk").join(version)).unwrap();
    Jdk::at(&home).expect("the corpus has this SDK")
}

fn runtime(name: &str, version: &str, default: bool) -> Runtime {
    Runtime {
        name: name.to_owned(),
        home: fs::canonicalize(corpus().join("jdk").join(version)).unwrap(),
        default,
    }
}

fn scratch(name: &str) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("environment")
        .join(name);
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).unwrap();
    path
}

fn project(name: &str) -> (PathBuf, PathBuf, JdtlsConfig) {
    let root = scratch(name).join("project");
    // No build file: jdtls makes an invisible project and takes the SDK that Project
    // Structure chose, which is what the server sees.
    let source = root.join("probe/Probe.java");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    fs::write(&source, PROBE).unwrap();
    let state_dir = scratch(&format!("{name}-state"));
    let m2 = PathBuf::from(std::env::var("MAVEN_USER_HOME").expect("set MAVEN_USER_HOME"));
    let settings = state_dir.join("maven-settings.xml");
    let repository = m2
        .join("repository")
        .display()
        .to_string()
        .replace('\\', "/");
    fs::write(
        &settings,
        format!(
            "<settings><localRepository>{repository}</localRepository>\
             <offline>true</offline></settings>"
        ),
    )
    .unwrap();
    let tools = corpus().join("tools");
    let config = JdtlsConfig {
        jdtls_home: tools.join("jdtls-1.63.0"),
        debug_plugin: tools.join(
            "vscode-java-debug-0.60.0/extension/server/com.microsoft.java.debug.plugin-0.53.2.jar",
        ),
        java_home: fs::canonicalize(corpus().join("jdk/25")).unwrap(),
        project_root: root.clone(),
        state_dir: state_dir.join("server"),
        runtimes: vec![runtime("JavaSE-17", "17", true)],
        maven_settings: Some(settings),
        gradle_user_home: None,
        gradle_java_home: None,
        offline: true,
        environment: HashMap::new(),
    };
    (root, source, config)
}

async fn until(status: &mut watch::Receiver<Status>, test: impl Fn(&Status) -> bool) -> Status {
    timeout(READY, status.wait_for(test))
        .await
        .expect("jdtls did not get there in time")
        .unwrap()
        .clone()
}

async fn wait_error(project: &JavaProject, uri: &str, wanted: bool) {
    let diagnostics = project.diagnostics().unwrap();
    let mut lists = ask(diagnostics, DiagnosticsQuery(uri.to_owned()));
    timeout(ANSWER, async {
        while let Some(list) = lists.next().await {
            let has_error = list
                .value
                .iter()
                .any(|entry| entry.severity == Severity::Error);
            if has_error == wanted {
                return;
            }
        }
        panic!("diagnostics ended");
    })
    .await
    .expect("diagnostics did not arrive");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the corpus with jdtls and its SDKs"]
async fn another_project_sdk_changes_what_the_server_sees() {
    let (_, source, config) = project("sdk-change");
    let project = JavaProject::open(config, Trust::Trusted).unwrap();
    let mut status = project.status();
    let ready = until(&mut status, |status| status.state == State::Ready).await;
    assert_eq!(ready.generation, 1);

    let uri = file_uri(&CanonPath::new(&source).unwrap())
        .unwrap()
        .as_str()
        .to_owned();
    let text = fs::read_to_string(&source).unwrap();
    project
        .server()
        .unwrap()
        .open(&uri, 1, text.as_str().into());
    wait_error(&project, &uri, true).await;

    let environment = java_project::ResolvedEnvironment {
        runtimes: vec![(jdk("17"), false), (jdk("21"), false)],
        selected: Some(jdk("21")),
        ..Default::default()
    };
    assert!(project.set_project_jdk(&environment));
    let ready = until(&mut status, |status| {
        status.state == State::Ready && status.generation > 1
    })
    .await;
    assert!(ready.generation > 1, "the server did not start again");

    // The editor replays the open documents on the next request; this is that request.
    project
        .server()
        .unwrap()
        .open(&uri, 2, text.as_str().into());
    sleep(Duration::from_millis(500)).await;
    wait_error(&project, &uri, false).await;
}
