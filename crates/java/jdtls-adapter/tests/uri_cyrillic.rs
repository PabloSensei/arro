//! What URI the real server publishes diagnostics for when the project path has Cyrillic
//! characters and parentheses.
//!
//! cargo test -p jdtls-adapter --test uri_cyrillic -- --ignored --test-threads=1 --nocapture
//!
//! The real server needs `IDE_CORPUS` and `MAVEN_USER_HOME`. The project is temporary and
//! the import is offline.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use jdtls_adapter::{Jdtls, JdtlsConfig, Runtime};
use lsp_client::ServerEvents;
use platform::CanonPath;
use serde_json::Value;
use sidecar::{Limits, ServerSpec, Sidecar, State, SupervisedLauncher, Trust};
use tokio::time::{sleep, timeout};

const READY: Duration = Duration::from_secs(600);

const SOURCE: &str = r#"package demo;

public class Broken {
    int answer() {
        return "not an int";
    }
}
"#;

fn corpus() -> PathBuf {
    PathBuf::from(std::env::var("IDE_CORPUS").expect("set IDE_CORPUS"))
}

fn maven_home() -> PathBuf {
    PathBuf::from(std::env::var("MAVEN_USER_HOME").expect("set MAVEN_USER_HOME"))
}

/// Every publication of diagnostics, as the server wrote the URI.
#[derive(Default)]
struct Publications {
    /// The count of diagnostics last published for each URI text.
    counts: Mutex<HashMap<String, usize>>,
    /// Every URI the server published diagnostics for.
    uris: Mutex<Vec<String>>,
    publications: AtomicUsize,
}

impl Publications {
    fn uri_for(&self, source: &Path) -> Option<(String, usize)> {
        let key = platform::path_key(source);
        let counts = self
            .counts
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        counts
            .iter()
            .find(|(uri, count)| {
                **count > 0
                    && platform::path_from_uri(uri)
                        .is_some_and(|path| platform::path_key(&path) == key)
            })
            .map(|(uri, count)| (uri.clone(), *count))
    }
}

impl ServerEvents for Publications {
    fn notification(&self, method: &str, params: Value) {
        if method != "textDocument/publishDiagnostics" {
            return;
        }
        let Some(uri) = params.get("uri").and_then(Value::as_str) else {
            return;
        };
        let count = params
            .get("diagnostics")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0);
        self.publications.fetch_add(1, Ordering::SeqCst);
        self.uris
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .push(uri.to_owned());
        self.counts
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .insert(uri.to_owned(), count);
    }
}

fn write_project(root: &Path) -> (PathBuf, PathBuf) {
    // The folder the bug is about: Cyrillic letters and parentheses in the project path.
    let project = root.join("\u{041f}\u{0440}\u{043e}\u{0435}\u{043a}\u{0442} (1)");
    fs::create_dir_all(project.join("src/main/java/demo")).unwrap();
    let settings = root.join("maven-settings.xml");
    let repository = maven_home()
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
    let pom = r#"<?xml version="1.0" encoding="UTF-8"?>
<project xmlns="http://maven.apache.org/POM/4.0.0">
  <modelVersion>4.0.0</modelVersion>
  <groupId>demo</groupId>
  <artifactId>uri-probe</artifactId>
  <version>1.0</version>
  <properties>
    <maven.compiler.release>17</maven.compiler.release>
    <project.build.sourceEncoding>UTF-8</project.build.sourceEncoding>
  </properties>
</project>
"#;
    fs::write(project.join("pom.xml"), pom).unwrap();
    let source = project.join("src/main/java/demo/Broken.java");
    fs::write(&source, SOURCE).unwrap();
    (project, settings)
}

fn config_for(project: PathBuf, settings: PathBuf, state: PathBuf) -> JdtlsConfig {
    let tools = corpus().join("tools");
    let runtime = |name: &str, version: &str, default| Runtime {
        name: name.to_owned(),
        home: fs::canonicalize(corpus().join("jdk").join(version)).unwrap(),
        default,
    };
    JdtlsConfig {
        jdtls_home: tools.join("jdtls-1.63.0"),
        debug_plugin: tools.join(
            "vscode-java-debug-0.60.0/extension/server/com.microsoft.java.debug.plugin-0.53.2.jar",
        ),
        java_home: fs::canonicalize(corpus().join("jdk").join("25")).unwrap(),
        project_root: project,
        state_dir: state,
        runtimes: vec![
            runtime("JavaSE-17", "17", false),
            runtime("JavaSE-25", "25", true),
        ],
        maven_settings: Some(settings),
        gradle_home: None,
        gradle_user_home: None,
        gradle_java_home: None,
        offline: true,
        environment: HashMap::new(),
        max_heap_mb: None,
    }
}

fn escaped(text: &str) -> String {
    text.chars()
        .map(|character| {
            if character.is_ascii() {
                character.to_string()
            } else {
                format!("\\u{{{:04X}}}", character as u32)
            }
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs IDE_CORPUS and MAVEN_USER_HOME; starts the real server"]
async fn diagnostics_arrive_for_a_project_in_a_folder_with_cyrillic_and_parentheses() {
    let temp = test_support::TempDir::new("jdtls-uri-cyrillic");
    let root = temp.path().to_path_buf();
    let (project, settings) = write_project(&root);
    let jdtls =
        Arc::new(Jdtls::new(config_for(project.clone(), settings, root.join("state"))).unwrap());
    let publications = Arc::new(Publications::default());
    let spec: Arc<dyn ServerSpec> = jdtls.clone();
    let sidecar = Sidecar::start(
        spec,
        Arc::new(SupervisedLauncher),
        publications.clone(),
        Limits::default(),
        Trust::Trusted,
    )
    .unwrap();
    let mut status = sidecar.status();
    timeout(READY, status.wait_for(|status| status.client.is_some()))
        .await
        .expect("the server did not connect in time")
        .unwrap();
    timeout(
        READY,
        status.wait_for(|status| status.state == State::Ready),
    )
    .await
    .expect("the server did not import the project in time")
    .unwrap();

    let source = project.join("src/main/java/demo/Broken.java");
    let uri = lsp_client::uri::file_uri(&CanonPath::new(&source).unwrap()).unwrap();
    let client = sidecar.client().unwrap();
    client.open(uri, "java", SOURCE.to_owned()).unwrap();

    timeout(READY, async {
        loop {
            if publications.uri_for(&source).is_some() {
                return;
            }
            sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("no diagnostics arrived for the file");

    let (raw, count) = publications.uri_for(&source).unwrap();
    assert!(count > 0);
    eprintln!("[probe] diagnostics of {count} arrived with URI {raw}");
    eprintln!("[probe] the same URI escaped: {}", escaped(&raw));
    let parsed = lsp_client::uri::server_uri(&raw).expect("the URI of the server is not readable");
    eprintln!("[probe] Arro reads it as {}", parsed.as_str());
    assert_eq!(
        platform::path_key(&platform::path_from_uri(&raw).unwrap()),
        platform::path_key(&source),
        "the URI names the source file"
    );
    assert_eq!(
        status.borrow().state,
        State::Ready,
        "the server imported the project"
    );

    sidecar.stop().await;
}
