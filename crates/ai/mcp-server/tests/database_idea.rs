//! The Database Tools tools of IDEA's server, over plain HTTP: the names and parameters of
//! the list, a statement with its result set, and an address whose password never reaches
//! an answer or the journal.

mod support;

use std::sync::Mutex;

use serde_json::{Value, json};
use support::Fixture;
use tool_gateway::{
    DatabaseAnswer, DatabaseConnection, DatabaseDescription, DatabaseIntrospection, DatabaseObject,
    DatabaseObjectKind, DatabaseQuery, DatabaseRecentQuery, DatabaseRows, DatabaseSchema,
    DatabaseTestResult, DescriptionColumn, DescriptionIndex, DescriptionKey, SessionScope,
    ToolBackend, ToolError,
};

/// A Database window with one connection, one schema and three rows.
#[derive(Default)]
struct Fake {
    /// What the tools asked, for a test that checks the query text.
    asked: Mutex<Vec<DatabaseQuery>>,
}

fn connection(name: &str, url: &str) -> DatabaseConnection {
    DatabaseConnection {
        id: "owners".to_string(),
        name: name.to_string(),
        dbms: "PostgreSQL".to_string(),
        driver: Some("org.postgresql.Driver".to_string()),
        url: url.to_string(),
        notice: None,
    }
}

fn rows() -> DatabaseRows {
    DatabaseRows {
        columns: vec!["id".to_string(), "name".to_string()],
        rows: vec![
            vec!["1".to_string(), "Ann, the first".to_string()],
            vec!["2".to_string(), "Bob".to_string()],
            vec!["3".to_string(), "Cid".to_string()],
        ],
        elapsed_ms: 4,
        ..DatabaseRows::default()
    }
}

impl ToolBackend for Fake {
    fn editor_context(&self, _: &SessionScope) -> Result<tool_gateway::EditorContext, ToolError> {
        Ok(tool_gateway::EditorContext::default())
    }

    fn find_symbol(
        &self,
        _: &SessionScope,
        _: &tool_gateway::SymbolQuery,
    ) -> Result<tool_gateway::SymbolPage, ToolError> {
        Err(ToolError::Failed("not here".to_string()))
    }

    fn symbol_info(
        &self,
        _: &SessionScope,
        _: &tool_gateway::Position,
    ) -> Result<tool_gateway::SymbolInfo, ToolError> {
        Err(ToolError::Failed("not here".to_string()))
    }

    fn diagnostics(
        &self,
        _: &SessionScope,
        _: &tool_gateway::DiagnosticsQuery,
    ) -> Result<tool_gateway::DiagnosticsPage, ToolError> {
        Err(ToolError::Failed("not here".to_string()))
    }

    fn database(
        &self,
        _scope: &SessionScope,
        query: &DatabaseQuery,
    ) -> Result<DatabaseAnswer, ToolError> {
        self.asked.lock().unwrap().push(query.clone());
        match query {
            DatabaseQuery::ListConnections => Ok(DatabaseAnswer::Connections(vec![connection(
                "Owners",
                "jdbc:postgresql://db/owners?user=app&password=s3cret",
            )])),
            DatabaseQuery::TestConnection { id } => {
                assert_eq!(id, "owners");
                Ok(DatabaseAnswer::Tested(DatabaseTestResult {
                    problem: false,
                    dbms: "PostgreSQL".to_string(),
                    version: Some("16.2".to_string()),
                    driver: Some("org.postgresql.Driver".to_string()),
                    result: "Connected".to_string(),
                }))
            }
            DatabaseQuery::CreateConnection {
                name, dbms, url, ..
            } => Ok(DatabaseAnswer::Saved(
                DatabaseConnection {
                    id: "created".to_string(),
                    name: name.clone(),
                    dbms: dbms.clone(),
                    driver: None,
                    url: url.clone(),
                    notice: None,
                },
                None,
            )),
            DatabaseQuery::EditConnection {
                connection_id, url, ..
            } => Ok(DatabaseAnswer::Saved(
                DatabaseConnection {
                    id: connection_id.clone(),
                    name: "Owners".to_string(),
                    dbms: "PostgreSQL".to_string(),
                    driver: None,
                    url: url.clone(),
                    notice: None,
                },
                None,
            )),
            DatabaseQuery::ListSchemas { .. } => {
                Ok(DatabaseAnswer::Schemas(vec![DatabaseSchema {
                    name: "public".to_string(),
                    database: Some("owners".to_string()),
                }]))
            }
            DatabaseQuery::Introspect {
                connection_id,
                database_name,
                schema_name,
            } => Ok(DatabaseAnswer::Introspected(DatabaseIntrospection {
                connection_id: connection_id.clone(),
                database_name: database_name.clone(),
                schema_name: schema_name.clone(),
                status: "started".to_string(),
                tables: 1,
                partial: false,
            })),
            DatabaseQuery::ObjectKinds { .. } => Ok(DatabaseAnswer::Kinds(vec![
                DatabaseObjectKind {
                    code: "TABLE".to_string(),
                    name: "Tables".to_string(),
                },
                DatabaseObjectKind {
                    code: "VIEW".to_string(),
                    name: "Views".to_string(),
                },
            ])),
            DatabaseQuery::ListObjects { kind, .. } => {
                Ok(DatabaseAnswer::Objects(vec![DatabaseObject {
                    name: "owners".to_string(),
                    kind: kind.clone().unwrap_or_else(|| "TABLE".to_string()),
                    schema: Some("public".to_string()),
                    database: Some("owners".to_string()),
                }]))
            }
            DatabaseQuery::Describe { object_name, .. } => {
                Ok(DatabaseAnswer::Described(DatabaseDescription {
                    name: object_name.clone(),
                    kind: "TABLE".to_string(),
                    schema: Some("public".to_string()),
                    database: Some("owners".to_string()),
                    columns: vec![DescriptionColumn {
                        name: "id".to_string(),
                        type_name: "INTEGER".to_string(),
                        nullable: false,
                        primary_key: true,
                    }],
                    indexes: vec![DescriptionIndex {
                        name: "owners_pkey".to_string(),
                        unique: true,
                        columns: vec!["id".to_string()],
                    }],
                    foreign_keys: vec![DescriptionKey {
                        name: "owners_type".to_string(),
                        columns: vec!["type_id".to_string()],
                        referenced_table: "types".to_string(),
                        referenced_columns: vec!["id".to_string()],
                    }],
                    partial: false,
                }))
            }
            DatabaseQuery::Execute { .. } | DatabaseQuery::PreviewTable { .. } => {
                Ok(DatabaseAnswer::Rows(rows()))
            }
            DatabaseQuery::RecentQueries { connection_id } => {
                Ok(DatabaseAnswer::Recent(vec![DatabaseRecentQuery {
                    sql: "SELECT * FROM owners".to_string(),
                    state: "finished".to_string(),
                    elapsed_ms: 4,
                    connection_id: connection_id.clone(),
                }]))
            }
            DatabaseQuery::Cancel { .. } => Ok(DatabaseAnswer::Cancelled(true)),
        }
    }
}

fn fixture() -> Fixture {
    Fixture::start(std::sync::Arc::new(Fake::default()))
}

#[test]
fn the_list_names_every_database_tool_with_its_parameters() {
    let fixture = fixture();

    let list = fixture.list();

    for (name, parameter) in [
        ("list_database_connections", "connections"),
        ("test_database_connection", "\"id\""),
        ("create_database_connection", "needToCheckDs"),
        ("edit_database_connection", "connectionId"),
        ("list_database_schemas", "selectedOnly"),
        ("introspect_schema", "databaseName"),
        ("list_schema_object_kinds", "connectionId"),
        ("list_schema_objects", "schemaName"),
        ("get_database_object_description", "objectName"),
        ("execute_sql_query", "queryText"),
        ("fetch_query_result", "resultSetId"),
        ("preview_table_data", "maxRowCount"),
        ("list_recent_sql_queries", "connectionId"),
        ("cancel_sql_query", "sessionId"),
    ] {
        assert!(list.contains(name), "{name} is not listed");
        let at = list.find(name).expect(name);
        assert!(
            list[at..].contains(parameter),
            "{name} does not name {parameter}"
        );
    }
}

#[test]
fn every_database_tool_answers() {
    let fixture = fixture();
    let calls = [
        ("list_database_connections", json!({})),
        ("test_database_connection", json!({"id": "owners"})),
        (
            "create_database_connection",
            json!({"name": "Owners 2", "dbms": "PostgreSQL", "url": "jdbc:postgresql://db/two"}),
        ),
        (
            "edit_database_connection",
            json!({"connectionId": "owners", "url": "jdbc:postgresql://db/three"}),
        ),
        (
            "list_database_schemas",
            json!({"connectionId": "owners", "selectedOnly": false}),
        ),
        (
            "introspect_schema",
            json!({"connectionId": "owners", "databaseName": "owners", "schemaName": "public"}),
        ),
        (
            "list_schema_object_kinds",
            json!({"connectionId": "owners"}),
        ),
        (
            "list_schema_objects",
            json!({"connectionId": "owners", "schemaName": "public", "kind": "TABLE"}),
        ),
        (
            "get_database_object_description",
            json!({"connectionId": "owners", "objectName": "owners"}),
        ),
        (
            "execute_sql_query",
            json!({"connectionId": "owners", "queryText": "SELECT * FROM owners"}),
        ),
        (
            "preview_table_data",
            json!({"connectionId": "owners", "tableName": "owners", "maxRowCount": 10}),
        ),
        ("list_recent_sql_queries", json!({"connectionId": "owners"})),
        ("cancel_sql_query", json!({"sessionId": "owners"})),
    ];
    for (name, arguments) in calls {
        let (answer, error) = fixture.answer(name, arguments);
        assert!(!error, "{name}: {answer}");
    }
    // The result of a statement is read by the id it answered.
    let (executed, error) = fixture.answer(
        "execute_sql_query",
        json!({"connectionId": "owners", "queryText": "SELECT * FROM owners"}),
    );
    assert!(!error, "{executed}");
    let value: Value = serde_json::from_str(&executed).expect("the answer is JSON");
    let id = value["resultSetId"].as_str().expect("a result set id");
    let (fetched, error) = fixture.answer(
        "fetch_query_result",
        json!({"resultSetId": id, "offset": 0}),
    );
    assert!(!error, "{fetched}");
}

#[test]
fn a_statement_answers_csv_and_fetch_reads_the_next_page() {
    let fixture = fixture();

    let (executed, error) = fixture.answer(
        "execute_sql_query",
        json!({"connectionId": "owners", "queryText": "SELECT * FROM owners"}),
    );
    assert!(!error, "{executed}");
    let value: Value = serde_json::from_str(&executed).expect("the answer is JSON");
    assert_eq!(value["rowCount"], 3);
    let rows = value["rows"].as_str().expect("the rows as CSV");
    assert!(rows.starts_with("id,name\n"), "{rows}");
    assert!(rows.contains("\"Ann, the first\""), "{rows}");
    let id = value["resultSetId"].as_str().expect("a result set id");

    let (page, error) = fixture.answer(
        "fetch_query_result",
        json!({"resultSetId": id, "offset": 2}),
    );

    assert!(!error, "{page}");
    let page: Value = serde_json::from_str(&page).expect("the answer is JSON");
    assert_eq!(page["rowCount"], 1, "{page}");
    assert_eq!(page["offset"], 2);
    assert_eq!(page["hasMore"], false);
    assert_eq!(page["rows"], "id,name\n3,Cid\n");
}

#[test]
fn a_password_of_a_jdbc_address_is_hidden_in_the_answer_and_the_journal() {
    let fixture = fixture();
    let url = "jdbc:postgresql://db/owners?user=app&password=s3cret";

    let (created, error) = fixture.answer(
        "create_database_connection",
        json!({"name": "Owners", "dbms": "PostgreSQL", "url": url}),
    );
    assert!(!error, "{created}");
    assert!(!created.contains("s3cret"), "{created}");
    assert!(created.contains("***"), "{created}");

    let (listed, error) = fixture.answer("list_database_connections", json!({}));
    assert!(!error, "{listed}");
    assert!(!listed.contains("s3cret"), "{listed}");

    let journal = fixture.gateway.clients().all_calls();
    for call in journal {
        assert!(!call.arguments.contains("s3cret"), "{}", call.arguments);
        assert!(
            call.arguments.contains("***") || !call.arguments.contains("password"),
            "{}",
            call.arguments
        );
    }
    assert!(
        !format!("{:?}", fixture.gateway.audit().recent()).contains("s3cret"),
        "the audit log keeps no arguments"
    );
}
