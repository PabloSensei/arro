//! T-0212: the way the language layer starts the database helper, on a real in-memory
//! H2: the JDK of the project, the pathing jar of `helper()` and a create and a select
//! through the `Session` it returns.
//!
//! The helper jar comes from `cargo run -q -p xtask -- build-helpers --out target/helpers`
//! and `MAVEN_USER_HOME` must hold `com/h2database/h2`. `IDE_HELPERS_DIR` names the
//! folder of the helper jar, as `BuildEnv` reads it, and `JAVA_HOME` or `IDE_CORPUS`
//! names the JDK.
//!
//! `cargo test -p java-lang --test database_helper -- --ignored --test-threads=1`

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use database::{Command, DriverKind, Event, Secret, Session, Support, Value};
use java_lang::DatabaseSupport;
use project_model::{Module, PathEntry, PathKind, ProjectModel, Unit};

fn repository() -> PathBuf {
    env::var_os("MAVEN_USER_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            env::var_os("USERPROFILE")
                .or_else(|| env::var_os("HOME"))
                .map(|home| PathBuf::from(home).join(".m2"))
        })
        .map(|home| home.join("repository"))
        .expect("set MAVEN_USER_HOME")
}

fn h2_jar() -> PathBuf {
    let root = repository().join("com/h2database/h2");
    let mut versions: Vec<PathBuf> = fs::read_dir(&root)
        .unwrap_or_else(|error| panic!("no H2 under {}: {error}", root.display()))
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    versions.sort();
    for version in versions.into_iter().rev() {
        let mut jars: Vec<PathBuf> = fs::read_dir(&version)
            .ok()
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.is_file()
                    && path.extension().is_some_and(|extension| extension == "jar")
                    && !path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .ends_with("-sources.jar")
            })
            .collect();
        jars.sort();
        if let Some(jar) = jars.pop() {
            return jar;
        }
    }
    panic!("no H2 jar under {}", root.display());
}

fn helpers_dir() -> PathBuf {
    if let Some(dir) = env::var_os("IDE_HELPERS_DIR") {
        return PathBuf::from(dir);
    }
    env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../target"))
        .join("helpers")
}

/// The H2 driver as the runtime classpath of a module, the way a build tool reports it.
fn model(root: &Path, jar: PathBuf) -> ProjectModel {
    ProjectModel {
        root: root.to_path_buf(),
        tool: "maven".into(),
        modules: vec![Module {
            id: "app".into(),
            name: "app".into(),
            dir: root.to_path_buf(),
            units: vec![Unit {
                name: "main".into(),
                runtime_paths: vec![PathEntry {
                    path: jar,
                    kind: PathKind::Library {
                        coordinates: "com.h2database:h2".into(),
                    },
                }],
                ..Unit::default()
            }],
        }],
        resolved: true,
        ..ProjectModel::default()
    }
}

fn wait(session: &Session, found: impl Fn(&Event) -> bool) -> Event {
    let started = Instant::now();
    loop {
        let event = session
            .events()
            .recv_timeout(Duration::from_secs(120))
            .expect("the helper answers");
        if found(&event) {
            return event;
        }
        assert!(
            started.elapsed() < Duration::from_secs(120),
            "the answer did not come: {event:?}"
        );
    }
}

fn execute(
    session: &Session,
    id: u64,
    sql: &str,
    max_rows: Option<usize>,
) -> Result<Vec<Vec<Value>>, String> {
    session
        .send(Command::Query {
            id,
            sql: sql.to_owned(),
            timeout: None,
            max_rows,
        })
        .expect("the query request is written");
    let mut rows = Vec::new();
    loop {
        match wait(session, |_| true) {
            Event::Rows {
                id: got,
                rows: found,
            } if got == id => rows.extend(found),
            Event::Done { id: got, .. } if got == id => return Ok(rows),
            Event::Error {
                id: got, message, ..
            } if got == id => return Err(message),
            _ => {}
        }
    }
}

#[test]
#[ignore = "T-0212: needs the helper jar, MAVEN_USER_HOME and a JDK"]
fn the_support_starts_the_helper_with_the_driver_of_the_model() {
    let folder = test_support::TempDir::new("java-lang-db-helper");
    let root = folder.path().to_path_buf();
    let model = model(&root, h2_jar());
    let driver = java_project::drivers(&model)
        .into_iter()
        .find(|driver| driver.kind == DriverKind::H2)
        .expect("the model carries the H2 driver");

    let support = DatabaseSupport::new(root.clone(), None, root.join("cache"));
    let launch = support
        .helper(&driver.jars)
        .unwrap_or_else(|error| panic!("no helper: {error}; IDE_HELPERS_DIR={:?}", helpers_dir()));
    let session = Session::start(launch).expect("the helper starts");
    wait(&session, |event| matches!(event, Event::Ready { .. }));

    session
        .send(Command::Connect {
            id: 1,
            url: "jdbc:h2:mem:t0212;DB_CLOSE_DELAY=-1".into(),
            user: "sa".into(),
            password: Secret::new(""),
            properties: Vec::new(),
        })
        .expect("the connect request is written");
    let connected = wait(&session, |event| {
        matches!(event, Event::Connected { id: 1, .. })
    });
    let Event::Connected { quote, .. } = connected else {
        panic!("not a connected answer");
    };
    assert_eq!(quote, "\"", "H2 quotes an identifier with a double quote");

    execute(
        &session,
        2,
        "CREATE TABLE OWNERS (ID INT PRIMARY KEY, NAME VARCHAR(30))",
        None,
    )
    .expect("the table is created");
    execute(
        &session,
        3,
        "INSERT INTO OWNERS VALUES (1, 'Ann'), (2, 'Bob'), (3, 'Cid')",
        None,
    )
    .expect("the rows are inserted");

    let rows = execute(&session, 4, "SELECT ID, NAME FROM OWNERS ORDER BY ID", None)
        .expect("the rows are read");
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0][0], Value::Integer(1));
    assert_eq!(rows[0][1], Value::Text("Ann".into()));

    // The address has no clause of its own: the limit is the one of the statement.
    let limited = execute(&session, 5, "SELECT ID FROM OWNERS ORDER BY ID", Some(2))
        .expect("the limited select runs");
    assert_eq!(limited.len(), 2, "maxRows stops the driver at two rows");
}
