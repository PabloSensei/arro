//! The database helper on a real in-memory H2: creating, inserting, selecting, a driver
//! error, a cancelled long query and the metadata of the schema.
//!
//! The helper jar comes from `cargo run -q -p xtask -- build-helpers --out target/helpers`
//! and `MAVEN_USER_HOME` must hold `com/h2database/h2`. `IDE_CORPUS` names the JDKs when
//! `JAVA_HOME` is not set.
//!
//! `cargo test -p java-project --test database_helper -- --ignored --test-threads=1`

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use database::{ColumnKind, Command, Event, Launch, ResultSet, Secret, Session, Table, Value};

fn java() -> PathBuf {
    if let Some(home) = env::var_os("JAVA_HOME") {
        let home = PathBuf::from(home);
        let java = home
            .join("bin")
            .join(if cfg!(windows) { "java.exe" } else { "java" });
        if java.is_file() {
            return java;
        }
    }
    let corpus = PathBuf::from(env::var("IDE_CORPUS").expect("set JAVA_HOME or IDE_CORPUS"));
    let java = corpus
        .join("jdk/17/bin")
        .join(if cfg!(windows) { "java.exe" } else { "java" });
    assert!(java.is_file(), "no java at {}", java.display());
    java
}

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

fn newest_jar(directory: &Path) -> Option<PathBuf> {
    let mut jars: Vec<PathBuf> = fs::read_dir(directory)
        .ok()?
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
    jars.pop()
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
    versions
        .into_iter()
        .rev()
        .find_map(|version| newest_jar(&version))
        .unwrap_or_else(|| panic!("no H2 jar under {}", root.display()))
}

fn helper_jar() -> PathBuf {
    if let Some(path) = env::var_os("IDE_DB_HELPER") {
        return PathBuf::from(path);
    }
    let target = env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../target"));
    let path = target.join("helpers").join("ide-db-helper.jar");
    assert!(
        path.is_file(),
        "no helper at {}: run cargo run -q -p xtask -- build-helpers --out target/helpers",
        path.display()
    );
    path
}

fn start() -> Session {
    let classpath =
        env::join_paths([helper_jar(), h2_jar()]).expect("the helper and H2 on one classpath");
    let launch = Launch::new(java())
        .arg("-cp")
        .arg(classpath)
        .arg("ide.db.Helper");
    let session = Session::start(launch).expect("the helper starts");
    wait(&session, |event| matches!(event, Event::Ready { .. }));
    session
        .send(Command::Connect {
            id: 1,
            url: "jdbc:h2:mem:t0206;DB_CLOSE_DELAY=-1".into(),
            user: "sa".into(),
            password: Secret::new(""),
            properties: Vec::new(),
        })
        .expect("the connect request is written");
    wait(&session, |event| {
        matches!(event, Event::Connected { id: 1, .. })
    });
    session
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

fn execute(session: &Session, id: u64, sql: &str) -> Result<ResultSet, String> {
    session
        .send(Command::Query {
            id,
            sql: sql.to_owned(),
            timeout: None,
            max_rows: None,
            params: Vec::new(),
        })
        .expect("the query request is written");
    let mut columns = Vec::new();
    let mut rows = Vec::new();
    loop {
        match wait(session, |_| true) {
            Event::Columns {
                id: got,
                columns: found,
            } if got == id => columns = found,
            Event::Rows {
                id: got,
                rows: found,
            } if got == id => rows.extend(found),
            Event::Done {
                id: got,
                update_count,
                truncated,
                ..
            } if got == id => {
                return Ok(ResultSet {
                    columns,
                    rows,
                    update_count,
                    truncated,
                });
            }
            Event::Error {
                id: got, message, ..
            } if got == id => return Err(message),
            _ => {}
        }
    }
}

fn metadata(session: &Session, id: u64) -> Vec<Table> {
    session
        .send(Command::Schema { id })
        .expect("the schema request is written");
    let mut tables = Vec::new();
    loop {
        match wait(session, |_| true) {
            Event::Schema { id: got, schema } if got == id => tables.extend(schema.tables),
            Event::SchemaDone { id: got, .. } if got == id => return tables,
            Event::Error {
                id: got, message, ..
            } if got == id => panic!("the schema failed: {message}"),
            _ => {}
        }
    }
}

#[test]
#[ignore = "T-0206: needs the helper jar and H2 from MAVEN_USER_HOME"]
fn h2_create_insert_select_reads_rows_and_nulls() {
    let session = start();
    let created = execute(
        &session,
        2,
        "CREATE TABLE OWNERS (ID INT PRIMARY KEY, NAME VARCHAR(30), CITY VARCHAR(30))",
    )
    .expect("the table is created");
    assert_eq!(created.update_count, Some(0));
    execute(
        &session,
        3,
        "INSERT INTO OWNERS VALUES (1, 'Ann', 'Kyiv'), (2, 'Bob', NULL)",
    )
    .expect("the rows are inserted");

    let result = execute(&session, 4, "SELECT ID, NAME, CITY FROM OWNERS ORDER BY ID")
        .expect("the rows are read");

    assert_eq!(
        result
            .columns
            .iter()
            .map(|column| column.name.as_str())
            .collect::<Vec<_>>(),
        ["ID", "NAME", "CITY"]
    );
    assert_eq!(result.columns[0].kind, ColumnKind::Integer);
    assert_eq!(result.rows.len(), 2);
    assert_eq!(result.rows[0][0], Value::Integer(1));
    assert_eq!(result.rows[0][1], Value::Text("Ann".into()));
    assert_eq!(result.rows[1][2], Value::Null);
    assert_eq!(result.update_count, None);
}

#[test]
#[ignore = "T-0206: needs the helper jar and H2 from MAVEN_USER_HOME"]
fn h2_metadata_lists_tables_columns_indexes_and_keys() {
    let session = start();
    execute(
        &session,
        2,
        "CREATE TABLE TYPES (ID INT PRIMARY KEY, NAME VARCHAR(30) NOT NULL)",
    )
    .expect("the referenced table is created");
    execute(
        &session,
        3,
        "CREATE TABLE OWNERS (ID INT PRIMARY KEY, NAME VARCHAR(30) NOT NULL, \
         TYPE_ID INT, CONSTRAINT FK_TYPE FOREIGN KEY (TYPE_ID) REFERENCES TYPES (ID))",
    )
    .expect("the table is created");
    execute(&session, 4, "CREATE INDEX IDX_OWNERS_NAME ON OWNERS (NAME)")
        .expect("the index is created");

    let tables = metadata(&session, 5);
    let owners = tables
        .iter()
        .find(|table| table.name.eq_ignore_ascii_case("OWNERS"))
        .expect("OWNERS is in the schema");

    let id = owners
        .columns
        .iter()
        .find(|column| column.name.eq_ignore_ascii_case("ID"))
        .expect("ID is a column");
    assert!(id.primary_key);
    assert!(id.kind == ColumnKind::Integer);
    let name = owners
        .columns
        .iter()
        .find(|column| column.name.eq_ignore_ascii_case("NAME"))
        .expect("NAME is a column");
    assert!(!name.nullable);
    assert!(
        owners
            .indexes
            .iter()
            .any(|index| index.columns.iter().any(|column| column == "NAME")),
        "the index is described: {:?}",
        owners.indexes
    );
    assert!(
        owners
            .foreign_keys
            .iter()
            .any(|key| key.referenced_table.eq_ignore_ascii_case("TYPES")),
        "the foreign key is described: {:?}",
        owners.foreign_keys
    );
}

#[test]
#[ignore = "T-0206: needs the helper jar and H2 from MAVEN_USER_HOME"]
fn h2_reports_the_error_of_bad_sql() {
    let session = start();
    let error = execute(&session, 2, "SELECT * FROM NOPE").expect_err("the statement fails");
    assert!(
        error.contains("NOPE"),
        "the driver names the table: {error}"
    );
}

#[test]
#[ignore = "T-0206: needs the helper jar and H2 from MAVEN_USER_HOME"]
fn h2_cancel_stops_a_long_query() {
    let session = start();
    session
        .send(Command::Query {
            id: 2,
            sql: "SELECT SUM(X) FROM SYSTEM_RANGE(1, 1000000000)".into(),
            timeout: None,
            max_rows: None,
            params: Vec::new(),
        })
        .expect("the query request is written");
    let running = wait(&session, |event| {
        matches!(
            event,
            Event::Columns { id: 2, .. } | Event::Done { id: 2, .. } | Event::Error { id: 2, .. }
        )
    });
    assert!(
        matches!(running, Event::Columns { .. }),
        "the query still runs: {running:?}"
    );

    session
        .send(Command::Cancel { id: 2 })
        .expect("the cancel request is written");

    let stopped = wait(&session, |event| {
        matches!(
            event,
            Event::Done { id: 2, .. } | Event::Error { id: 2, .. }
        )
    });
    match stopped {
        Event::Done { cancelled, .. } => assert!(cancelled, "the statement says it was cancelled"),
        Event::Error { cancelled, .. } => assert!(cancelled, "the error says it was cancelled"),
        other => panic!("the statement ended as {other:?}"),
    }
}
