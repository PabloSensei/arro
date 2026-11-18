//! The text of a result as a file: comma-separated values, JSON or the `INSERT`
//! statements of its rows.
//!
//! An export reads the whole statement again, so its text may be long. [`Exporter`]
//! builds it one batch at a time and the window writes it once: the rows are never kept
//! twice.

use serde_json::Value as Json;

use crate::data::{Dialect, TableRef, insert_literal};
use crate::protocol::Value;
use crate::schema::Column;

/// What an export writes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ExportFormat {
    #[default]
    Csv,
    Json,
    SqlInsert,
}

impl ExportFormat {
    /// The name of the format, as the dialog offers it.
    pub fn title(self) -> &'static str {
        match self {
            Self::Csv => "CSV",
            Self::Json => "JSON",
            Self::SqlInsert => "SQL INSERT",
        }
    }

    /// The suffix of the file of the format.
    pub fn extension(self) -> &'static str {
        match self {
            Self::Csv => "csv",
            Self::Json => "json",
            Self::SqlInsert => "sql",
        }
    }

    /// The formats of the dialog, in the order it shows them.
    pub fn all() -> [Self; 3] {
        [Self::Csv, Self::Json, Self::SqlInsert]
    }
}

/// The text of an export, built from the columns and then from each batch of rows. A
/// missing value is an empty field of a CSV, `null` of the JSON and `NULL` of a statement.
pub struct Exporter {
    format: ExportFormat,
    dialect: Dialect,
    /// The table the statements insert into, when the result is of one table.
    table: TableRef,
    columns: Vec<Column>,
    text: String,
    rows: usize,
}

impl Exporter {
    pub fn new(format: ExportFormat, dialect: Dialect, table: TableRef) -> Self {
        Self {
            format,
            dialect,
            table,
            columns: Vec::new(),
            text: String::new(),
            rows: 0,
        }
    }

    /// The columns of the result. A format with a header writes it here.
    pub fn start(&mut self, columns: &[Column]) {
        self.columns = columns.to_vec();
        if columns.is_empty() {
            return;
        }
        match self.format {
            ExportFormat::Csv => {
                let names: Vec<String> = columns
                    .iter()
                    .map(|column| csv_field(&column.name))
                    .collect();
                self.text.push_str(&names.join(","));
                self.text.push('\n');
            }
            ExportFormat::Json => self.text.push_str("[\n"),
            ExportFormat::SqlInsert => {}
        }
    }

    /// Adds one row of the result.
    pub fn push(&mut self, row: &[Value]) {
        if self.columns.is_empty() {
            return;
        }
        match self.format {
            ExportFormat::Csv => {
                let fields: Vec<String> = row
                    .iter()
                    .map(|value| match value {
                        Value::Null => String::new(),
                        other => csv_field(&other.text()),
                    })
                    .collect();
                self.text.push_str(&fields.join(","));
                self.text.push('\n');
            }
            ExportFormat::Json => {
                if self.rows > 0 {
                    self.text.push_str(",\n");
                }
                // The order of the columns is kept, so the file reads like the grid.
                self.text.push('{');
                for (index, (column, value)) in self.columns.iter().zip(row).enumerate() {
                    if index > 0 {
                        self.text.push_str(", ");
                    }
                    self.text
                        .push_str(&Json::from(column.name.clone()).to_string());
                    self.text.push_str(": ");
                    self.text.push_str(&json_value(value).to_string());
                }
                self.text.push('}');
            }
            ExportFormat::SqlInsert => {
                self.text.push_str(&insert_literal(
                    self.dialect,
                    &self.table,
                    &self.columns,
                    row,
                ));
                self.text.push_str(";\n");
            }
        }
        self.rows += 1;
    }

    /// How many rows the text holds so far.
    pub fn rows(&self) -> usize {
        self.rows
    }

    /// The whole text. A JSON array of no rows is empty.
    pub fn finish(mut self) -> String {
        if self.format == ExportFormat::Json {
            match self.rows {
                0 => return "[]\n".to_owned(),
                _ => self.text.push_str("\n]\n"),
            }
        }
        self.text
    }
}

/// One field of a comma-separated line: a comma, a quote or a line break puts the field in
/// quotes and a quote inside it is doubled.
pub(crate) fn csv_field(text: &str) -> String {
    if text.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", text.replace('"', "\"\""))
    } else {
        text.to_owned()
    }
}

/// One value as JSON: a number stays a number, a missing value is `null` and a date, a
/// time or a binary value is its text.
fn json_value(value: &Value) -> Json {
    match value {
        Value::Null => Json::Null,
        Value::Integer(integer) => Json::from(*integer),
        Value::Decimal(decimal) => decimal
            .trim()
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map_or_else(|| Json::from(decimal.clone()), Json::Number),
        Value::Real(real) => serde_json::Number::from_f64(*real)
            .map_or_else(|| Json::from(real.to_string()), Json::Number),
        Value::Boolean(boolean) => Json::from(*boolean),
        other => Json::from(other.text()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::ColumnKind;

    fn column(name: &str) -> Column {
        Column {
            name: name.into(),
            type_name: "VARCHAR".into(),
            kind: ColumnKind::Text,
            nullable: true,
            primary_key: false,
        }
    }

    fn columns() -> Vec<Column> {
        vec![column("ID"), column("NAME"), column("NOTE")]
    }

    fn rows() -> Vec<Vec<Value>> {
        vec![
            vec![
                Value::Integer(1),
                Value::Text("Ann, \"A\"".into()),
                Value::Null,
            ],
            vec![
                Value::Integer(2),
                Value::Text("Bob\nline".into()),
                Value::Text("it's".into()),
            ],
        ]
    }

    fn export(format: ExportFormat) -> String {
        let mut exporter = Exporter::new(format, Dialect::H2, TableRef::new("PUBLIC", "OWNERS"));
        exporter.start(&columns());
        for row in rows() {
            exporter.push(&row);
        }
        exporter.finish()
    }

    #[test]
    fn a_csv_quotes_its_commas_quotes_and_line_breaks_and_leaves_null_empty() {
        assert_eq!(
            export(ExportFormat::Csv),
            "ID,NAME,NOTE\n1,\"Ann, \"\"A\"\"\",\n2,\"Bob\nline\",it's\n"
        );
    }

    #[test]
    fn a_json_keeps_the_kinds_and_writes_null() {
        let text = export(ExportFormat::Json);
        let value: Json = serde_json::from_str(&text).expect("the export is JSON");

        assert_eq!(
            value,
            serde_json::json!([
                {"ID": 1, "NAME": "Ann, \"A\"", "NOTE": null},
                {"ID": 2, "NAME": "Bob\nline", "NOTE": "it's"}
            ])
        );
        assert_eq!(
            Exporter::new(ExportFormat::Json, Dialect::H2, TableRef::default()).finish(),
            "[]\n"
        );
    }

    #[test]
    fn insert_statements_escape_the_text_and_name_the_table() {
        let text = export(ExportFormat::SqlInsert);
        assert!(text.contains(
            "INSERT INTO \"PUBLIC\".\"OWNERS\" (\"ID\", \"NAME\", \"NOTE\") \
             VALUES (1, 'Ann, \"A\"', NULL);"
        ));
        assert!(text.contains("VALUES (2, 'Bob\nline', 'it''s');"));
        assert!(text.ends_with(";\n"));
    }

    #[test]
    fn a_my_sql_statements_double_a_backslash() {
        let mut exporter = Exporter::new(
            ExportFormat::SqlInsert,
            Dialect::MySql,
            TableRef::new("", "OWNERS"),
        );
        exporter.start(&columns());
        exporter.push(&[Value::Integer(1), Value::Text("a\\b".into()), Value::Null]);

        assert_eq!(
            exporter.finish(),
            "INSERT INTO `OWNERS` (`ID`, `NAME`, `NOTE`) VALUES (1, 'a\\\\b', NULL);\n"
        );
    }
}
