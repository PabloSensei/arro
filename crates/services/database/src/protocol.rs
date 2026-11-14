//! The lines between the IDE and the helper, in both directions.
//!
//! One request or answer is one JSON object on one line. A request names itself with an
//! `id`, and every answer about it carries the same one, so a cancel may arrive while a
//! result is still being read.

use std::time::Duration;

use serde_json::{Map, Value as Json, json};

use crate::schema::{Column, ColumnKind, ForeignKey, Index, Schema, Table, TableKind};
use crate::sources::Secret;

/// The version of the protocol both sides speak.
pub const PROTOCOL: u32 = 1;

/// A request of the IDE. The password of a connection never leaves this message.
#[derive(Clone, Debug)]
pub enum Command {
    Connect {
        id: u64,
        url: String,
        user: String,
        password: Secret,
        /// Extra properties for the driver, such as `ssl=true`.
        properties: Vec<(String, String)>,
    },
    Query {
        id: u64,
        sql: String,
        /// `None` leaves the statement to the user, who may cancel it.
        timeout: Option<Duration>,
        /// The most rows the driver returns for the statement. `None` leaves it
        /// unlimited, so the address itself carries no row clause.
        max_rows: Option<usize>,
    },
    /// Asks for the next batch of the result of `id`.
    Fetch {
        id: u64,
    },
    /// Asks the helper to stop `id`; it answers with `done` or `error` marked cancelled.
    Cancel {
        id: u64,
    },
    /// Reads the schemas, tables, columns, indexes and keys of the connection.
    Schema {
        id: u64,
    },
    AutoCommit {
        id: u64,
        on: bool,
    },
    Commit {
        id: u64,
    },
    Rollback {
        id: u64,
    },
    Close {
        id: u64,
    },
}

impl Command {
    /// The line the helper reads. The caller writes the newline.
    pub fn encode(&self) -> String {
        let mut object = Map::new();
        match self {
            Self::Connect {
                id,
                url,
                user,
                password,
                properties,
            } => {
                object.insert("type".into(), json!("connect"));
                object.insert("id".into(), json!(id));
                object.insert("url".into(), json!(url));
                object.insert("user".into(), json!(user));
                object.insert("password".into(), json!(password.expose()));
                let properties: Map<String, Json> = properties
                    .iter()
                    .map(|(name, value)| (name.clone(), json!(value)))
                    .collect();
                object.insert("properties".into(), Json::Object(properties));
            }
            Self::Query {
                id,
                sql,
                timeout,
                max_rows,
            } => {
                object.insert("type".into(), json!("query"));
                object.insert("id".into(), json!(id));
                object.insert("sql".into(), json!(sql));
                if let Some(timeout) = timeout {
                    object.insert("timeoutMs".into(), json!(timeout.as_millis() as u64));
                }
                if let Some(max_rows) = max_rows {
                    object.insert("maxRows".into(), json!(max_rows));
                }
            }
            Self::Fetch { id } => {
                object.insert("type".into(), json!("fetch"));
                object.insert("id".into(), json!(id));
            }
            Self::Cancel { id } => {
                object.insert("type".into(), json!("cancel"));
                object.insert("id".into(), json!(id));
            }
            Self::Schema { id } => {
                object.insert("type".into(), json!("schema"));
                object.insert("id".into(), json!(id));
            }
            Self::AutoCommit { id, on } => {
                object.insert("type".into(), json!("autocommit"));
                object.insert("id".into(), json!(id));
                object.insert("on".into(), json!(on));
            }
            Self::Commit { id } => {
                object.insert("type".into(), json!("commit"));
                object.insert("id".into(), json!(id));
            }
            Self::Rollback { id } => {
                object.insert("type".into(), json!("rollback"));
                object.insert("id".into(), json!(id));
            }
            Self::Close { id } => {
                object.insert("type".into(), json!("close"));
                object.insert("id".into(), json!(id));
            }
        }
        Json::Object(object).to_string()
    }
}

/// What the helper says. [`Event::Rows`] and [`Event::Schema`] arrive in batches.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Ready {
        protocol: u32,
        vendor: String,
        version: String,
    },
    Connected {
        id: u64,
        product: String,
        auto_commit: bool,
        /// The string the driver wraps an identifier with; empty when it quotes none.
        quote: String,
    },
    /// The names and types of a result, before its rows.
    Columns {
        id: u64,
        columns: Vec<Column>,
    },
    Rows {
        id: u64,
        rows: Vec<Vec<Value>>,
    },
    /// The statement is over. `update_count` is set for INSERT, UPDATE and DELETE.
    Done {
        id: u64,
        update_count: Option<u64>,
        truncated: bool,
        cancelled: bool,
        elapsed: Duration,
    },
    Error {
        id: u64,
        sql_state: Option<String>,
        code: Option<i32>,
        message: String,
        cancelled: bool,
    },
    /// One schema and its tables, or one table of a large schema.
    Schema {
        id: u64,
        schema: Schema,
    },
    SchemaDone {
        id: u64,
        truncated: bool,
        /// Some of the keys or indexes could not be described by the driver.
        partial: bool,
    },
    /// The helper ended: the connection is over, and the source has to be opened again.
    Exited {
        code: Option<i32>,
    },
}

impl Event {
    /// A line that came from the helper, when it is one of the known answers.
    pub fn decode(line: &str) -> Option<Self> {
        let value: Json = serde_json::from_str(line).ok()?;
        let object = value.as_object()?;
        let kind = object.get("type")?.as_str()?;
        let id = object.get("id").and_then(Json::as_u64).unwrap_or(0);
        Some(match kind {
            "ready" => Self::Ready {
                protocol: object.get("protocol").and_then(Json::as_u64).unwrap_or(0) as u32,
                vendor: text(object, "vendor"),
                version: text(object, "version"),
            },
            "connected" => Self::Connected {
                id,
                product: text(object, "product"),
                auto_commit: object
                    .get("autoCommit")
                    .and_then(Json::as_bool)
                    .unwrap_or(true),
                quote: text(object, "quote"),
            },
            "columns" => Self::Columns {
                id,
                columns: object
                    .get("columns")
                    .and_then(Json::as_array)
                    .map(|columns| columns.iter().filter_map(column).collect())
                    .unwrap_or_default(),
            },
            "rows" => Self::Rows {
                id,
                rows: object
                    .get("rows")
                    .and_then(Json::as_array)
                    .map(|rows| {
                        rows.iter()
                            .filter_map(Json::as_array)
                            .map(|row| row.iter().map(cell_value).collect())
                            .collect()
                    })
                    .unwrap_or_default(),
            },
            "done" => Self::Done {
                id,
                update_count: object.get("updateCount").and_then(Json::as_u64),
                truncated: object
                    .get("truncated")
                    .and_then(Json::as_bool)
                    .unwrap_or(false),
                cancelled: object
                    .get("cancelled")
                    .and_then(Json::as_bool)
                    .unwrap_or(false),
                elapsed: Duration::from_millis(
                    object.get("elapsedMs").and_then(Json::as_u64).unwrap_or(0),
                ),
            },
            "error" => Self::Error {
                id,
                sql_state: object
                    .get("sqlState")
                    .and_then(Json::as_str)
                    .map(str::to_owned),
                code: object
                    .get("code")
                    .and_then(Json::as_i64)
                    .map(|code| code as i32),
                message: text(object, "message"),
                cancelled: object
                    .get("cancelled")
                    .and_then(Json::as_bool)
                    .unwrap_or(false),
            },
            "schema" => Self::Schema {
                id,
                schema: object.get("schema").and_then(schema)?,
            },
            "schema-done" => Self::SchemaDone {
                id,
                truncated: object
                    .get("truncated")
                    .and_then(Json::as_bool)
                    .unwrap_or(false),
                partial: object
                    .get("partial")
                    .and_then(Json::as_bool)
                    .unwrap_or(false),
            },
            "exited" => Self::Exited {
                code: object
                    .get("code")
                    .and_then(Json::as_i64)
                    .map(|code| code as i32),
            },
            _ => return None,
        })
    }
}

fn text(object: &Map<String, Json>, key: &str) -> String {
    object
        .get(key)
        .and_then(Json::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn column(value: &Json) -> Option<Column> {
    let object = value.as_object()?;
    Some(Column {
        name: text(object, "name"),
        type_name: text(object, "type"),
        kind: column_kind(object.get("kind").and_then(Json::as_str)),
        nullable: object
            .get("nullable")
            .and_then(Json::as_bool)
            .unwrap_or(true),
        primary_key: object
            .get("primaryKey")
            .and_then(Json::as_bool)
            .unwrap_or(false),
    })
}

fn column_kind(kind: Option<&str>) -> ColumnKind {
    match kind {
        Some("integer") => ColumnKind::Integer,
        Some("decimal") => ColumnKind::Decimal,
        Some("number") => ColumnKind::Number,
        Some("boolean") => ColumnKind::Boolean,
        Some("date") => ColumnKind::Date,
        Some("time") => ColumnKind::Time,
        Some("timestamp") => ColumnKind::Timestamp,
        Some("binary") => ColumnKind::Binary,
        Some("text") => ColumnKind::Text,
        _ => ColumnKind::Other,
    }
}

fn schema(value: &Json) -> Option<Schema> {
    let object = value.as_object()?;
    Some(Schema {
        catalog: object
            .get("catalog")
            .and_then(Json::as_str)
            .map(str::to_owned),
        schema: text(object, "schema"),
        tables: object
            .get("tables")
            .and_then(Json::as_array)
            .map(|tables| tables.iter().filter_map(table).collect())
            .unwrap_or_default(),
    })
}

fn table(value: &Json) -> Option<Table> {
    let object = value.as_object()?;
    Some(Table {
        schema: text(object, "schema"),
        name: text(object, "name"),
        kind: match object.get("kind").and_then(Json::as_str) {
            Some("view") => TableKind::View,
            _ => TableKind::Table,
        },
        columns: object
            .get("columns")
            .and_then(Json::as_array)
            .map(|columns| columns.iter().filter_map(column).collect())
            .unwrap_or_default(),
        indexes: object
            .get("indexes")
            .and_then(Json::as_array)
            .map(|indexes| indexes.iter().filter_map(index).collect())
            .unwrap_or_default(),
        foreign_keys: object
            .get("foreignKeys")
            .and_then(Json::as_array)
            .map(|keys| keys.iter().filter_map(foreign_key).collect())
            .unwrap_or_default(),
        partial: object
            .get("partial")
            .and_then(Json::as_bool)
            .unwrap_or(false),
    })
}

fn index(value: &Json) -> Option<Index> {
    let object = value.as_object()?;
    Some(Index {
        name: text(object, "name"),
        unique: object
            .get("unique")
            .and_then(Json::as_bool)
            .unwrap_or(false),
        columns: strings(object, "columns"),
    })
}

fn foreign_key(value: &Json) -> Option<ForeignKey> {
    let object = value.as_object()?;
    Some(ForeignKey {
        name: text(object, "name"),
        columns: strings(object, "columns"),
        referenced_table: text(object, "referencedTable"),
        referenced_columns: strings(object, "referencedColumns"),
    })
}

fn strings(object: &Map<String, Json>, key: &str) -> Vec<String> {
    object
        .get(key)
        .and_then(Json::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Json::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// One cell, read without the type of its column.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Integer(i64),
    /// A decimal or a very large integer, kept exact as text.
    Decimal(String),
    Real(f64),
    Boolean(bool),
    Text(String),
    /// A date or a time as the driver wrote it, without a time zone conversion.
    Date(String),
    Time(String),
    Timestamp(String),
    Binary {
        base64: String,
        bytes: u64,
        /// Only the first [`crate::MAX_BINARY_BYTES`] were read.
        truncated: bool,
    },
}

fn cell_value(value: &Json) -> Value {
    match value {
        Json::Null => Value::Null,
        Json::Bool(boolean) => Value::Boolean(*boolean),
        Json::Number(number) => match number.as_i64() {
            Some(integer) => Value::Integer(integer),
            None => Value::Real(number.as_f64().unwrap_or_default()),
        },
        Json::String(text) => Value::Text(text.clone()),
        Json::Object(object) => {
            if let Some(text) = object.get("decimal").and_then(Json::as_str) {
                Value::Decimal(text.to_owned())
            } else if let Some(text) = object.get("date").and_then(Json::as_str) {
                Value::Date(text.to_owned())
            } else if let Some(text) = object.get("time").and_then(Json::as_str) {
                Value::Time(text.to_owned())
            } else if let Some(text) = object.get("timestamp").and_then(Json::as_str) {
                Value::Timestamp(text.to_owned())
            } else if let Some(base64) = object.get("binary").and_then(Json::as_str) {
                Value::Binary {
                    base64: base64.to_owned(),
                    bytes: object.get("bytes").and_then(Json::as_u64).unwrap_or(0),
                    truncated: object
                        .get("truncated")
                        .and_then(Json::as_bool)
                        .unwrap_or(false),
                }
            } else {
                Value::Text(Json::Object(object.clone()).to_string())
            }
        }
        Json::Array(_) => Value::Text(value.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_connect_line_carries_the_password_and_the_properties() {
        let line = Command::Connect {
            id: 7,
            url: "jdbc:h2:mem:test".into(),
            user: "sa".into(),
            password: Secret::new("secret"),
            properties: vec![("ssl".into(), "true".into())],
        }
        .encode();
        let value: Json = serde_json::from_str(&line).unwrap();

        assert_eq!(value["type"], "connect");
        assert_eq!(value["password"], "secret");
        assert_eq!(value["properties"]["ssl"], "true");
        assert!(!format!("{:?}", Secret::new("secret")).contains("secret"));
    }

    #[test]
    fn a_query_carries_the_row_limit_of_a_table_sample() {
        let line = Command::Query {
            id: 4,
            sql: "SELECT * FROM \"PUBLIC\".\"OWNERS\"".into(),
            timeout: None,
            max_rows: Some(500),
        }
        .encode();
        let value: Json = serde_json::from_str(&line).unwrap();

        assert_eq!(value["type"], "query");
        assert_eq!(value["sql"], "SELECT * FROM \"PUBLIC\".\"OWNERS\"");
        assert_eq!(value["maxRows"], 500);
        assert!(value.get("timeoutMs").is_none());

        // A console statement has no limit of its own: the address holds it, if any.
        let line = Command::Query {
            id: 5,
            sql: "SELECT ID FROM OWNERS".into(),
            timeout: None,
            max_rows: None,
        }
        .encode();
        let value: Json = serde_json::from_str(&line).unwrap();
        assert!(value.get("maxRows").is_none());
    }

    #[test]
    fn a_connected_answer_names_the_quote_of_the_driver() {
        let event = Event::decode(
            r#"{"type":"connected","id":1,"product":"H2 2.4.240","autoCommit":true,"quote":"\""}"#,
        )
        .expect("connected");

        assert_eq!(
            event,
            Event::Connected {
                id: 1,
                product: "H2 2.4.240".into(),
                auto_commit: true,
                quote: "\"".into(),
            }
        );
    }

    #[test]
    fn the_answers_of_the_helper_are_read_back() {
        let ready = Event::decode(r#"{"type":"ready","protocol":1,"vendor":"H2","version":"2"}"#)
            .expect("ready");
        assert_eq!(
            ready,
            Event::Ready {
                protocol: 1,
                vendor: "H2".into(),
                version: "2".into()
            }
        );

        let rows = Event::decode(
            r#"{"type":"rows","id":2,"rows":[[1,"Ann",null,{"decimal":"1.50"},{"date":"2024-01-02"}]]}"#,
        )
        .expect("rows");
        assert_eq!(
            rows,
            Event::Rows {
                id: 2,
                rows: vec![vec![
                    Value::Integer(1),
                    Value::Text("Ann".into()),
                    Value::Null,
                    Value::Decimal("1.50".into()),
                    Value::Date("2024-01-02".into()),
                ]]
            }
        );
    }

    #[test]
    fn a_schema_answer_keeps_the_keys_and_the_indexes() {
        let event = Event::decode(
            r#"{"type":"schema","id":3,"schema":{"catalog":"C","schema":"PUBLIC","tables":[
                {"schema":"PUBLIC","name":"OWNERS","kind":"table",
                 "columns":[{"name":"ID","type":"INTEGER","kind":"integer","nullable":false,"primaryKey":true}],
                 "indexes":[{"name":"IDX","unique":true,"columns":["ID"]}],
                 "foreignKeys":[{"name":"FK","columns":["TYPE_ID"],"referencedTable":"TYPES","referencedColumns":["ID"]}]}]}}"#,
        )
        .expect("schema");
        let Event::Schema { schema, .. } = event else {
            panic!("not a schema");
        };
        assert_eq!(schema.schema, "PUBLIC");
        assert_eq!(schema.tables[0].name, "OWNERS");
        assert!(schema.tables[0].columns[0].primary_key);
        assert!(schema.tables[0].indexes[0].unique);
        assert_eq!(schema.tables[0].foreign_keys[0].referenced_table, "TYPES");
    }
}
