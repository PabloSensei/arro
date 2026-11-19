//! The neutral types of the database tools that IDEA's MCP server names after its
//! Database Tools plugin.
//!
//! The rows of a query travel as comma-separated text with a header, as the console of
//! IDEA copies them. A result stays in memory under a `resultSetId`, so an agent reads a
//! long answer in pages; the store is small and the oldest result goes first. A password
//! of a connection address never reaches an answer.

use std::collections::VecDeque;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::backend::ToolError;

/// The results one gateway keeps. A result is read by `fetch_query_result` after the
/// answer of `execute_sql_query`; older ones go first.
pub const RESULT_LIMIT: usize = 16;
/// The rows of a result that stay after the answer, so `fetch_query_result` can page.
pub const STORED_ROWS: usize = 10_000;
/// The rows one page of `fetch_query_result` carries.
pub const PAGE_ROWS: usize = 500;

/// One connection as the Database window lists it. The address has no password.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseConnection {
    pub id: String,
    pub name: String,
    /// The product, such as `PostgreSQL`.
    pub dbms: String,
    /// The driver the project resolves for it, when there is one.
    pub driver: Option<String>,
    /// The address with every password hidden.
    pub url: String,
}

/// What a test of a connection found.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseTestResult {
    /// Whether the connection has a problem.
    pub problem: bool,
    pub dbms: String,
    pub version: Option<String>,
    pub driver: Option<String>,
    /// What the attempt said, in the words of the driver.
    pub result: String,
}

/// One schema of a connection.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseSchema {
    pub name: String,
    pub database: Option<String>,
}

/// How far the window has read the schema of a connection.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseIntrospection {
    pub connection_id: String,
    pub database_name: Option<String>,
    pub schema_name: Option<String>,
    /// `started`, `ready` or `failed`.
    pub status: String,
    pub tables: usize,
    /// Some keys or indexes could not be described by the driver.
    pub partial: bool,
}

/// A kind of object of a schema, with the code the other tools pass.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseObjectKind {
    pub code: String,
    pub name: String,
}

/// One table or view of a connection.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseObject {
    pub name: String,
    /// `TABLE` or `VIEW`.
    pub kind: String,
    pub schema: Option<String>,
    pub database: Option<String>,
}

/// One column of an object.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DescriptionColumn {
    pub name: String,
    #[serde(rename = "type")]
    pub type_name: String,
    pub nullable: bool,
    pub primary_key: bool,
}

/// One index of an object.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DescriptionIndex {
    pub name: String,
    pub unique: bool,
    pub columns: Vec<String>,
}

/// One foreign key of an object.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DescriptionKey {
    pub name: String,
    pub columns: Vec<String>,
    pub referenced_table: String,
    pub referenced_columns: Vec<String>,
}

/// What the Database window knows about one object.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseDescription {
    pub name: String,
    /// `TABLE` or `VIEW`.
    pub kind: String,
    pub schema: Option<String>,
    pub database: Option<String>,
    pub columns: Vec<DescriptionColumn>,
    pub indexes: Vec<DescriptionIndex>,
    pub foreign_keys: Vec<DescriptionKey>,
    /// Keys or indexes the driver refused to describe.
    pub partial: bool,
}

/// The rows of one statement, already read into text.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DatabaseRows {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
    /// Set for INSERT, UPDATE and DELETE.
    pub update_count: Option<u64>,
    pub truncated: bool,
    pub elapsed_ms: u64,
    pub error: Option<String>,
}

impl DatabaseRows {
    /// The rows from `offset` on, as comma-separated text with a header.
    pub fn csv(&self, offset: usize) -> (String, usize) {
        let rows = self.rows.get(offset..).unwrap_or_default();
        let rows = &rows[..rows.len().min(PAGE_ROWS)];
        (csv(&self.columns, rows), rows.len())
    }

    /// Whether rows exist after the page that starts at `offset`.
    pub fn has_more(&self, offset: usize) -> bool {
        self.rows.len() > offset + PAGE_ROWS
    }
}

/// One statement the console ran or still runs.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseRecentQuery {
    pub sql: String,
    /// `running`, `failed` or `finished`.
    pub state: String,
    pub elapsed_ms: u64,
    /// The connection it ran on.
    pub connection_id: String,
}

/// What a tool of the database asks of the window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DatabaseQuery {
    ListConnections,
    TestConnection {
        id: String,
    },
    CreateConnection {
        name: String,
        dbms: String,
        url: String,
        need_to_check: bool,
    },
    EditConnection {
        connection_id: String,
        dbms: Option<String>,
        url: String,
        need_to_check: bool,
    },
    ListSchemas {
        connection_id: String,
        selected_only: bool,
    },
    Introspect {
        connection_id: String,
        database_name: Option<String>,
        schema_name: Option<String>,
    },
    ObjectKinds {
        connection_id: String,
    },
    ListObjects {
        connection_id: String,
        schema_name: Option<String>,
        database_name: Option<String>,
        kind: Option<String>,
    },
    Describe {
        connection_id: String,
        database_name: Option<String>,
        schema_name: Option<String>,
        kind: Option<String>,
        object_name: String,
    },
    Execute {
        connection_id: String,
        query_text: String,
    },
    PreviewTable {
        connection_id: String,
        schema_name: Option<String>,
        database_name: Option<String>,
        table_name: String,
        max_row_count: usize,
    },
    RecentQueries {
        connection_id: String,
    },
    /// Stops the statement that runs on the connection. The id is the connection id,
    /// the session of a statement in the terms of this window.
    Cancel {
        session_id: String,
    },
}

/// What the Database window answers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DatabaseAnswer {
    Connections(Vec<DatabaseConnection>),
    Tested(DatabaseTestResult),
    /// The connection a create or an edit left in place.
    Saved(DatabaseConnection, Option<DatabaseTestResult>),
    Schemas(Vec<DatabaseSchema>),
    Introspected(DatabaseIntrospection),
    Kinds(Vec<DatabaseObjectKind>),
    Objects(Vec<DatabaseObject>),
    Described(DatabaseDescription),
    Rows(DatabaseRows),
    Recent(Vec<DatabaseRecentQuery>),
    Cancelled(bool),
}

/// One stored result and the lines it keeps.
#[derive(Clone, Debug, PartialEq, Eq)]
struct StoredResult {
    id: String,
    rows: DatabaseRows,
}

/// The results of the queries of a gateway, in the order they arrived.
#[derive(Default)]
pub struct QueryResults {
    state: Mutex<VecDeque<StoredResult>>,
    next: std::sync::atomic::AtomicU64,
}

impl QueryResults {
    /// Keeps a result and answers the id `fetch_query_result` reads it by.
    pub fn store(&self, mut rows: DatabaseRows) -> String {
        let serial = self.next.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        let id = format!("result-{serial}");
        rows.rows.truncate(STORED_ROWS);
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if state.len() == RESULT_LIMIT {
            state.pop_front();
        }
        state.push_back(StoredResult {
            id: id.clone(),
            rows,
        });
        id
    }

    /// The rows of a stored result from `offset`, or `None` for an unknown id.
    pub fn page(&self, id: &str, offset: usize) -> Option<(String, usize, bool)> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let stored = state.iter().find(|stored| stored.id == id)?;
        let (text, count) = stored.rows.csv(offset);
        Some((text, count, stored.rows.has_more(offset)))
    }
}

/// The name of a product as a tool writes it, in the words the window shows.
pub fn dbms_name(kind: &str) -> Option<&'static str> {
    match kind.trim().to_ascii_lowercase().as_str() {
        "h2" => Some("H2"),
        "postgresql" | "postgres" => Some("PostgreSQL"),
        "mysql" => Some("MySQL"),
        "mariadb" => Some("MariaDB"),
        "sqlite" => Some("SQLite"),
        "oracle" => Some("Oracle"),
        "sqlserver" | "sql server" | "mssql" => Some("SQL Server"),
        _ => None,
    }
}

/// Rows as comma-separated text with the names of the columns in the first line.
pub fn csv(columns: &[String], rows: &[Vec<String>]) -> String {
    let mut text = String::new();
    let names: Vec<String> = columns.iter().map(|name| field(name)).collect();
    text.push_str(&names.join(","));
    text.push('\n');
    for row in rows {
        let fields: Vec<String> = row.iter().map(|value| field(value)).collect();
        text.push_str(&fields.join(","));
        text.push('\n');
    }
    text
}

/// One field of a comma-separated answer: quoted when it holds a separator, a quote or
/// a line break.
fn field(text: &str) -> String {
    if text.contains([',', '"', '\n', '\r']) {
        return format!("\"{}\"", text.replace('"', "\"\""));
    }
    text.to_string()
}

/// The answer of `execute_sql_query`: the status, the rows as CSV and the id of the
/// result that `fetch_query_result` pages.
pub fn executed(answer: &DatabaseRows, result_set: Option<&str>) -> String {
    let (rows, _) = answer.csv(0);
    let status = match (&answer.error, answer.update_count) {
        (Some(error), _) => error.clone(),
        (None, Some(count)) => format!("{count} rows updated"),
        (None, None) => "ok".to_string(),
    };
    json!({
        "status": status,
        "resultSetId": result_set,
        "rowCount": answer.rows.len(),
        "columnCount": answer.columns.len(),
        "truncated": answer.truncated,
        "updateCount": answer.update_count,
        "elapsedMs": answer.elapsed_ms,
        "rows": rows,
    })
    .to_string()
        + "\n"
}

/// The answer of `fetch_query_result`.
pub fn fetched(id: &str, offset: usize, csv: &str, count: usize, more: bool) -> String {
    json!({
        "resultSetId": id,
        "offset": offset,
        "rowCount": count,
        "hasMore": more,
        "rows": csv,
    })
    .to_string()
        + "\n"
}

/// The answer of `preview_table_data`.
pub fn preview(answer: &DatabaseRows) -> String {
    let (rows, _) = answer.csv(0);
    json!({
        "rowCount": answer.rows.len(),
        "truncated": answer.truncated,
        "elapsedMs": answer.elapsed_ms,
        "rows": rows,
    })
    .to_string()
        + "\n"
}

/// Every answer of the database tools, as the JSON an agent reads.
pub fn json_answer(answer: DatabaseAnswer) -> String {
    let value = match answer {
        DatabaseAnswer::Connections(items) => json!({ "connections": items }),
        DatabaseAnswer::Tested(result) => json!(result),
        DatabaseAnswer::Saved(connection, check) => {
            json!({ "connection": connection, "diagnostics": check })
        }
        DatabaseAnswer::Schemas(items) => json!({ "schemas": items }),
        DatabaseAnswer::Introspected(status) => json!(status),
        DatabaseAnswer::Kinds(items) => json!({ "kinds": items }),
        DatabaseAnswer::Objects(items) => json!({ "objects": items }),
        DatabaseAnswer::Described(object) => json!(object),
        DatabaseAnswer::Rows(rows) => return executed(&rows, None),
        DatabaseAnswer::Recent(items) => json!({ "queries": items }),
        DatabaseAnswer::Cancelled(cancelled) => json!({ "cancelled": cancelled }),
    };
    value.to_string() + "\n"
}

/// A path of the project, for the tools that name a file.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PreviewInput {
    #[serde(rename = "connectionId")]
    pub connection_id: String,
    #[serde(rename = "schemaName", default)]
    pub schema_name: Option<String>,
    #[serde(rename = "databaseName", default)]
    pub database_name: Option<String>,
    #[serde(rename = "tableName")]
    pub table_name: String,
    #[serde(rename = "maxRowCount", default)]
    pub max_row_count: Option<usize>,
}

/// The columns a tool names an object by.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ObjectInput {
    #[serde(rename = "connectionId")]
    pub connection_id: String,
    #[serde(rename = "databaseName", default)]
    pub database_name: Option<String>,
    #[serde(rename = "schemaName", default)]
    pub schema_name: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
}

/// The fields of one object.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DescribeInput {
    #[serde(rename = "connectionId")]
    pub connection_id: String,
    #[serde(rename = "databaseName", default)]
    pub database_name: Option<String>,
    #[serde(rename = "schemaName", default)]
    pub schema_name: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(rename = "objectName")]
    pub object_name: String,
}

/// The connection a create writes.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateInput {
    pub name: String,
    pub dbms: String,
    pub url: String,
    #[serde(rename = "needToCheckDs", default)]
    pub need_to_check: Option<bool>,
}

/// The connection an edit changes.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EditInput {
    #[serde(rename = "connectionId")]
    pub connection_id: String,
    #[serde(default)]
    pub dbms: Option<String>,
    pub url: String,
    #[serde(rename = "needToCheckDs", default)]
    pub need_to_check: Option<bool>,
}

/// A connection named for the tools that take only its id. The reference calls the
/// parameter `id` in one tool and `connectionId` in the others.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DatabaseIdInput {
    #[serde(rename = "connectionId", alias = "id", default)]
    pub connection_id: Option<String>,
    #[serde(rename = "selectedOnly", default)]
    pub selected_only: bool,
}

impl DatabaseIdInput {
    pub(crate) fn id(&self) -> Result<String, ToolError> {
        match self.connection_id.as_deref().map(str::trim) {
            Some(id) if !id.is_empty() => Ok(id.to_string()),
            _ => Err(ToolError::BadInput(
                "pass the id of a connection".to_string(),
            )),
        }
    }
}

/// The database and schema of an introspection.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct IntrospectInput {
    #[serde(rename = "connectionId")]
    pub connection_id: String,
    #[serde(rename = "databaseName", default)]
    pub database_name: Option<String>,
    #[serde(rename = "schemaName", default)]
    pub schema_name: Option<String>,
}

/// A statement of the console.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExecuteInput {
    #[serde(rename = "connectionId")]
    pub connection_id: String,
    #[serde(rename = "queryText")]
    pub query_text: String,
}

/// The result a page is read from.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FetchInput {
    #[serde(rename = "resultSetId")]
    pub result_set_id: String,
    #[serde(default)]
    pub offset: usize,
}

/// The session of a statement to stop.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CancelInput {
    #[serde(rename = "sessionId")]
    pub session_id: String,
}

/// The error a product the window does not know gives.
pub fn unknown_dbms(name: &str) -> ToolError {
    ToolError::BadInput(format!(
        "unknown DBMS {name}: pass H2, PostgreSQL, MySQL, MariaDB, SQLite, Oracle or SQL Server"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_result_is_paged_and_its_csv_quotes_a_separator() {
        let results = QueryResults::default();
        let rows = DatabaseRows {
            columns: vec!["id".to_string(), "name".to_string()],
            rows: vec![
                vec!["1".to_string(), "Ann, the first".to_string()],
                vec!["2".to_string(), "say \"hi\"".to_string()],
            ],
            ..DatabaseRows::default()
        };
        let id = results.store(rows);

        let (page, count, more) = results.page(&id, 0).expect("the stored result");

        assert_eq!(count, 2);
        assert!(!more);
        assert!(page.starts_with("id,name\n"), "{page}");
        assert!(page.contains("\"Ann, the first\""), "{page}");
        assert!(page.contains("\"say \"\"hi\"\"\""), "{page}");
        assert!(results.page("result-999", 0).is_none());
    }

    #[test]
    fn an_offset_cuts_the_rows_of_a_page() {
        let results = QueryResults::default();
        let rows = DatabaseRows {
            columns: vec!["n".to_string()],
            rows: (0..3).map(|n| vec![n.to_string()]).collect(),
            ..DatabaseRows::default()
        };
        let id = results.store(rows);

        let (page, count, _) = results.page(&id, 1).expect("the stored result");

        assert_eq!(count, 2);
        assert_eq!(page, "n\n1\n2\n");
    }

    #[test]
    fn a_product_is_read_in_the_words_the_window_shows() {
        assert_eq!(dbms_name("postgresql"), Some("PostgreSQL"));
        assert_eq!(dbms_name(" SQL Server "), Some("SQL Server"));
        assert_eq!(dbms_name("db2"), None);
    }
}
