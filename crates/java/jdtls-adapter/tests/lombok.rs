//! Lombok and jdtls: the agent is a JVM setting, and a project with Lombok gets its
//! generated members.
//!
//! cargo test -p jdtls-adapter --test lombok -- --ignored --test-threads=1
//!
//! The real server needs `IDE_CORPUS` and `MAVEN_USER_HOME` with
//! `org/projectlombok/lombok/1.18.38` and `org/slf4j/slf4j-api/2.0.17`. The project is
//! temporary, the repositories stay untouched, and the import is offline.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use jdtls_adapter::{Jdtls, JdtlsConfig, Lombok, Runtime};
use lsp_client::ServerEvents;
use lsp_client::utf16::position_at;
use platform::CanonPath;
use serde_json::{Value, json};
use sidecar::{Limits, ServerSpec, Sidecar, State, Status, SupervisedLauncher, Trust};
use tokio::sync::watch;
use tokio::time::{sleep, timeout};

const READY: Duration = Duration::from_secs(600);
const ANSWER: Duration = Duration::from_secs(120);

fn scratch(name: &str) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("lombok")
        .join(name);
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).unwrap();
    path
}

fn jdtls_home(root: &Path) -> PathBuf {
    let home = root.join("home");
    let plugins = home.join("plugins");
    fs::create_dir_all(&plugins).unwrap();
    fs::write(plugins.join("org.eclipse.equinox.launcher_1.8.0.jar"), b"").unwrap();
    home
}

fn config(root: &Path) -> JdtlsConfig {
    let project = root.join("project");
    fs::create_dir_all(&project).unwrap();
    JdtlsConfig {
        jdtls_home: jdtls_home(root),
        debug_plugin: root.join("debug.jar"),
        java_home: root.join("jdk"),
        project_root: project,
        state_dir: root.join("state"),
        runtimes: Vec::new(),
        maven_settings: None,
        gradle_user_home: None,
        gradle_java_home: None,
        offline: true,
        environment: HashMap::new(),
    }
}

fn agent_jar(root: &Path, name: &str) -> PathBuf {
    let path = root.join("agent files").join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, b"agent").unwrap();
    path
}

#[test]
fn the_agent_stays_before_the_launcher_jar_and_keeps_the_spaces_of_its_path() {
    let root = scratch("plan with spaces");
    let jdtls = Jdtls::new(config(&root)).unwrap();
    let source = agent_jar(&root, "lombok-1.18.38.jar");

    assert!(
        jdtls
            .set_lombok(Some(Lombok {
                version: "1.18.38".into(),
                jar: source,
            }))
            .unwrap()
    );

    let plan = jdtls.plan();
    let agent = plan
        .args
        .iter()
        .position(|argument| argument.starts_with("-javaagent:"))
        .expect("the plan has the agent");
    let launcher = plan
        .args
        .iter()
        .position(|argument| argument == "-jar")
        .expect("the plan has the launcher");
    assert!(agent < launcher, "the agent is a JVM setting");
    let path = plan.args[agent]
        .strip_prefix("-javaagent:")
        .expect("the option keeps its path");
    assert!(path.contains(' '), "{path}");
    let stored = Path::new(path);
    assert!(stored.is_file(), "the agent was copied");
    assert!(
        stored.starts_with(root.join("state")),
        "the copy lives in the state folder: {path}"
    );
    assert_eq!(jdtls.lombok_version().as_deref(), Some("1.18.38"));

    assert!(
        !jdtls
            .set_lombok(Some(Lombok {
                version: "1.18.38".into(),
                jar: root.join("another.jar"),
            }))
            .unwrap(),
        "the same version needs no restart"
    );

    assert!(
        jdtls
            .set_lombok(Some(Lombok {
                version: "1.18.40".into(),
                jar: agent_jar(&root, "lombok-1.18.40.jar"),
            }))
            .unwrap()
    );
    let plan = jdtls.plan();
    let agent = plan
        .args
        .iter()
        .find(|argument| argument.starts_with("-javaagent:"))
        .expect("the plan has the agent");
    assert!(agent.ends_with("lombok-1.18.40.jar"), "{agent}");

    assert!(jdtls.set_lombok(None).unwrap());
    assert!(
        !jdtls
            .plan()
            .args
            .iter()
            .any(|argument| argument.starts_with("-javaagent:")),
        "without Lombok the agent is gone"
    );
    let _ = fs::remove_dir_all(root);
}

#[derive(Default)]
struct Recorder {
    diagnostics: Mutex<Vec<Value>>,
}

impl ServerEvents for Recorder {
    fn notification(&self, method: &str, params: Value) {
        if method == "textDocument/publishDiagnostics" {
            self.diagnostics
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .push(params);
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
  <artifactId>lombok-demo</artifactId>
  <version>1.0</version>
  <properties>
    <maven.compiler.release>17</maven.compiler.release>
    <project.build.sourceEncoding>UTF-8</project.build.sourceEncoding>
  </properties>
  <dependencies>
    <dependency>
      <groupId>org.projectlombok</groupId>
      <artifactId>lombok</artifactId>
      <version>1.18.38</version>
      <scope>provided</scope>
    </dependency>
    <dependency>
      <groupId>org.slf4j</groupId>
      <artifactId>slf4j-api</artifactId>
      <version>2.0.17</version>
    </dependency>
  </dependencies>
</project>
"#;
    fs::write(project.join("pom.xml"), pom).unwrap();
    let source = project.join("src/main/java/demo/Person.java");
    let text = r#"package demo;

import lombok.Getter;
import lombok.Setter;
import lombok.extern.slf4j.Slf4j;

@Slf4j
public class Person {

    @Getter
    @Setter
    private String name;

    public String described() {
        Person other = new Person();
        other.setName("probe");
        log.info("{}", other.getName());
        return other.getName();
    }
}
"#;
    fs::write(&source, text).unwrap();
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
        gradle_user_home: None,
        gradle_java_home: None,
        offline: true,
        environment: HashMap::new(),
    }
}

fn json_items(response: &Value) -> Vec<Value> {
    if let Some(items) = response.as_array() {
        return items.clone();
    }
    response
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn first_location(response: &Value) -> Option<&Value> {
    if response.get("uri").is_some() {
        return Some(response);
    }
    response.as_array()?.first()
}

async fn until_ready(status: &mut watch::Receiver<Status>) -> Status {
    timeout(
        READY,
        status.wait_for(|status| status.state == State::Ready),
    )
    .await
    .expect("the server did not get ready in time")
    .unwrap()
    .clone()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs IDE_CORPUS and MAVEN_USER_HOME with Lombok 1.18.38"]
async fn a_project_with_lombok_has_no_false_errors_and_navigates_to_the_field() {
    let root = scratch("real");
    let (project, settings, source) = write_project(&root);
    let state = root.join("state");
    let jdtls = Arc::new(Jdtls::new(config_for(project, settings, state.clone())).unwrap());
    let jar = maven_home().join("repository/org/projectlombok/lombok/1.18.38/lombok-1.18.38.jar");
    assert!(jar.is_file(), "the package cache has {}", jar.display());
    assert!(
        jdtls
            .set_lombok(Some(Lombok {
                version: "1.18.38".into(),
                jar,
            }))
            .unwrap()
    );

    let recorder = Arc::new(Recorder::default());
    let spec: Arc<dyn ServerSpec> = jdtls.clone();
    let sidecar = Sidecar::start(
        spec,
        Arc::new(SupervisedLauncher),
        recorder.clone(),
        Limits::default(),
        Trust::Trusted,
    )
    .unwrap();
    let mut status = sidecar.status();
    until_ready(&mut status).await;
    let client = sidecar.client().expect("the server has a connection");

    let uri = lsp_client::uri::file_uri(&CanonPath::new(&source).unwrap()).unwrap();
    let text = fs::read_to_string(&source).unwrap();
    client.open(uri.clone(), "java", text.clone()).unwrap();

    let dot = text.find("other.getName").unwrap() + "other.".len();
    let position = position_at(&text, dot);
    let response = timeout(
        ANSWER,
        client.raw_request(
            "textDocument/completion",
            json!({
                "textDocument": {"uri": uri.as_str()},
                "position": {"line": position.line, "character": position.character},
            }),
        ),
    )
    .await
    .expect("the completion did not arrive")
    .expect("the completion request failed");
    let items = json_items(&response);
    assert!(
        items.iter().any(|item| {
            item.get("label")
                .and_then(Value::as_str)
                .is_some_and(|label| label.starts_with("getName"))
        }),
        "no getName among {} entries",
        items.len()
    );

    let name_at = text.find("other.getName").unwrap() + "other.".len();
    let position = position_at(&text, name_at);
    let response = timeout(
        ANSWER,
        client.raw_request(
            "textDocument/definition",
            json!({
                "textDocument": {"uri": uri.as_str()},
                "position": {"line": position.line, "character": position.character},
            }),
        ),
    )
    .await
    .expect("the definition did not arrive")
    .expect("the definition request failed");
    let location = first_location(&response).expect("getName has a definition");
    assert_eq!(location["uri"].as_str(), Some(uri.as_str()));
    let line = text
        .lines()
        .position(|line| line.contains("private String name"))
        .expect("the field is in the source");
    assert_eq!(
        location["range"]["start"]["line"].as_u64(),
        Some(line as u64),
        "the getter leads to its field: {location}"
    );

    // The server compiles after the document opens; an answer without errors is the
    // one the editor shows.
    let target = uri.as_str().to_owned();
    let wait = async {
        loop {
            let lists = recorder
                .diagnostics
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .clone();
            if lists
                .iter()
                .any(|list| list.get("uri").and_then(Value::as_str) == Some(target.as_str()))
            {
                return;
            }
            sleep(Duration::from_millis(250)).await;
        }
    };
    timeout(ANSWER, wait)
        .await
        .expect("the diagnostics did not arrive");
    // A later answer of the same file replaces the first one.
    sleep(Duration::from_secs(3)).await;
    let lists = recorder
        .diagnostics
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .clone();
    let latest = lists
        .into_iter()
        .rev()
        .find(|list| list.get("uri").and_then(Value::as_str) == Some(target.as_str()))
        .expect("diagnostics for the file");
    let errors: Vec<&Value> = latest["diagnostics"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .filter(|entry| entry["severity"].as_u64() == Some(1))
                .collect()
        })
        .unwrap_or_default();
    assert!(errors.is_empty(), "false errors: {errors:?}");

    client.shutdown().await.ok();
    let _ = sidecar.stop().await;
    let _ = fs::remove_dir_all(root);
}
