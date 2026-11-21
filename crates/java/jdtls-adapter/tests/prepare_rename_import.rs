//! What the real server answers to `textDocument/prepareRename` while it imports the project,
//! and what it answers once the import is done. The probe prints every answer and the state
//! of the server around it; it asserts only that the questions got an answer.
//!
//! cargo test -p jdtls-adapter --test prepare_rename_import -- --ignored --test-threads=1 --nocapture
//!
//! The real server needs `IDE_CORPUS` and `MAVEN_USER_HOME`. The project is temporary, the
//! repositories stay untouched, and the import is offline.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use jdtls_adapter::{Jdtls, JdtlsConfig, Runtime};
use lsp_client::utf16::position_at;
use lsp_client::{Client, ServerEvents};
use platform::CanonPath;
use serde_json::{Value, json};
use sidecar::{Limits, ServerSpec, Sidecar, State, Status, SupervisedLauncher, Trust};
use tokio::sync::watch;
use tokio::time::{sleep, timeout};

const READY: Duration = Duration::from_secs(600);
const ANSWER: Duration = Duration::from_secs(120);

const SOURCE: &str = r#"package demo;

public class Person {

    private String name;

    public String described() {
        return name;
    }
}
"#;

/// The messages of the import the server sends: its status and its progress.
#[derive(Default)]
struct Timeline {
    started: Option<Instant>,
    lines: Mutex<Vec<String>>,
}

impl Timeline {
    fn note(&self, line: String) {
        let at = self
            .started
            .map(|start| start.elapsed())
            .unwrap_or_default();
        self.lines
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .push(format!("[{at:>9.2?}] {line}"));
    }
}

impl ServerEvents for Timeline {
    fn notification(&self, method: &str, params: Value) {
        if method == "language/status" || method == "$/progress" {
            let text: String = params.to_string().chars().take(240).collect();
            self.note(format!("{method} {text}"));
        }
    }
}

fn corpus() -> PathBuf {
    PathBuf::from(std::env::var("IDE_CORPUS").expect("set IDE_CORPUS"))
}

fn maven_home() -> PathBuf {
    PathBuf::from(std::env::var("MAVEN_USER_HOME").expect("set MAVEN_USER_HOME"))
}

fn write_project(root: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let project = root.join("project");
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
  <artifactId>rename-probe</artifactId>
  <version>1.0</version>
  <properties>
    <maven.compiler.release>17</maven.compiler.release>
    <project.build.sourceEncoding>UTF-8</project.build.sourceEncoding>
  </properties>
</project>
"#;
    fs::write(project.join("pom.xml"), pom).unwrap();
    let source = project.join("src/main/java/demo/Person.java");
    fs::write(&source, SOURCE).unwrap();
    (project, settings, source)
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
    }
}

fn state_of(status: &watch::Receiver<Status>) -> String {
    format!("{:?}", status.borrow().state)
}

/// What the server answered to one question.
#[derive(Debug, PartialEq)]
enum Outcome {
    /// A range, or a null answer.
    Answered(Value),
    /// An error of the server, with its code and its text.
    Refused(i64, String),
    /// No answer arrived in time, or the connection failed.
    Silent(String),
}

/// One question about the position `offset`, printed with the server state around it.
async fn ask(
    client: &Client,
    uri: &str,
    text: &str,
    status: &watch::Receiver<Status>,
    what: &str,
    offset: usize,
) -> Outcome {
    let position = position_at(text, offset);
    let before = state_of(status);
    let started = Instant::now();
    let answer = timeout(
        ANSWER,
        client.raw_request(
            "textDocument/prepareRename",
            json!({
                "textDocument": {"uri": uri},
                "position": {"line": position.line, "character": position.character},
            }),
        ),
    )
    .await;
    let after = state_of(status);
    let took = started.elapsed();
    let outcome = match answer {
        Err(_) => Outcome::Silent(format!("no answer in {ANSWER:?}")),
        Ok(Ok(value)) => Outcome::Answered(value),
        Ok(Err(lsp_client::Error::Server(response))) => {
            Outcome::Refused(response.code, response.message)
        }
        Ok(Err(error)) => Outcome::Silent(error.to_string()),
    };
    eprintln!("[probe] {what}: {outcome:?} (state {before} -> {after}, after {took:.2?})");
    outcome
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs IDE_CORPUS and MAVEN_USER_HOME; starts the real server"]
async fn the_answers_to_prepare_rename_while_the_server_imports() {
    let temp = test_support::TempDir::new("prepare-rename-import");
    let root = temp.path().to_path_buf();
    let (project, settings, source) = write_project(&root);
    let jdtls = Arc::new(Jdtls::new(config_for(project, settings, root.join("state"))).unwrap());
    let timeline = Arc::new(Timeline {
        started: Some(Instant::now()),
        ..Timeline::default()
    });
    let spec: Arc<dyn ServerSpec> = jdtls.clone();
    let sidecar = Sidecar::start(
        spec,
        Arc::new(SupervisedLauncher),
        timeline.clone(),
        Limits::default(),
        Trust::Trusted,
    )
    .unwrap();
    let mut status = sidecar.status();
    timeout(READY, status.wait_for(|status| status.client.is_some()))
        .await
        .expect("the server did not connect in time")
        .unwrap();
    let client = sidecar.client().expect("the server has a connection");

    let file = lsp_client::uri::file_uri(&CanonPath::new(&source).unwrap()).unwrap();
    let text = SOURCE.to_owned();
    client
        .open(file.clone(), "java", text.clone())
        .expect("the document opens");
    let target = file.as_str().to_owned();

    // The first question goes out while the server still imports: the name of a field. Then
    // the server gets ready, and the same name, a method name and a keyword that no rename
    // covers are asked again.
    let name = text.find("return name").unwrap() + "return ".len();
    let method = text.find("described()").unwrap();
    let keyword = text.find("public class").unwrap();
    let during = ask(&client, &target, &text, &status, "during: name", name).await;

    timeout(
        READY,
        status.wait_for(|status| status.state == State::Ready),
    )
    .await
    .expect("the server did not get ready in time")
    .unwrap();
    sleep(Duration::from_secs(2)).await;
    let ready_name = ask(&client, &target, &text, &status, "ready: name", name).await;
    let ready_method = ask(&client, &target, &text, &status, "ready: method", method).await;
    let ready_keyword = ask(&client, &target, &text, &status, "ready: keyword", keyword).await;

    for line in timeline
        .lines
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .iter()
    {
        eprintln!("[timeline] {line}");
    }

    client.shutdown().await.ok();
    let _ = sidecar.stop().await;

    // What the real server did: a name is answered with its range, during the import as
    // well, and a keyword gets the refusal that says the element cannot be renamed.
    assert!(
        matches!(during, Outcome::Answered(_)),
        "a name asked during the import: {during:?}"
    );
    assert!(
        matches!(ready_name, Outcome::Answered(_)),
        "a name once ready: {ready_name:?}"
    );
    assert!(
        matches!(ready_method, Outcome::Answered(_)),
        "a method name once ready: {ready_method:?}"
    );
    match ready_keyword {
        Outcome::Refused(code, message) => {
            assert_eq!(code, -32600, "{message}");
            assert!(message.contains("not supported"), "{message}");
        }
        other => panic!("a keyword is refused once ready: {other:?}"),
    }
}
