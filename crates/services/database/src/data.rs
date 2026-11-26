//! The rows of a table as an editor shows and writes them.
//!
//! A page of a table is read by [`PageQuery`], and the unsaved edits of its rows become
//! the statements of [`Plan`]. Both are plain functions of the schema and the dialect, so
//! the window decides when to read or write and this module decides what the text is.
//!
//! The statements of Submit carry no parameter: every value becomes a literal that the
//! dialect can read back. Only the placeholders of a console statement keep their values
//! as parameters, and the driver binds them.

use crate::export::csv_field;
use crate::protocol::Value;
use crate::schema::{Column, ColumnKind, Table};
use crate::sources::DriverKind;

/// How a product quotes a name, writes a boolean and pages a result.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Dialect {
    #[default]
    H2,
    PostgreSql,
    MySql,
    MariaDb,
    Sqlite,
    Oracle,
    SqlServer,
}

impl Dialect {
    /// The dialect of the kind of source the user picked. The kind is what the project
    /// keeps, so the text does not wait for a connected driver.
    pub fn of(kind: DriverKind) -> Self {
        match kind {
            DriverKind::H2 => Self::H2,
            DriverKind::PostgreSql => Self::PostgreSql,
            DriverKind::MySql => Self::MySql,
            DriverKind::MariaDb => Self::MariaDb,
            DriverKind::Sqlite => Self::Sqlite,
            DriverKind::Oracle => Self::Oracle,
            DriverKind::SqlServer => Self::SqlServer,
        }
    }

    /// The character the product wraps a name with.
    pub fn quote(self) -> char {
        match self {
            Self::MySql | Self::MariaDb => '`',
            Self::H2 | Self::PostgreSql | Self::Sqlite | Self::Oracle | Self::SqlServer => '"',
        }
    }

    /// Whether the product refuses `OFFSET … FETCH NEXT` without an `ORDER BY` before it.
    fn needs_order_for_paging(self) -> bool {
        matches!(self, Self::SqlServer)
    }

    /// Wraps a name in the quotes of the product; a quote inside it is doubled.
    pub fn quote_name(self, name: &str) -> String {
        let quote = self.quote();
        let mut text = String::with_capacity(name.len() + 2);
        text.push(quote);
        for character in name.chars() {
            if character == quote {
                text.push(quote);
            }
            text.push(character);
        }
        text.push(quote);
        text
    }

    /// The text of a string literal, with the escapes the product needs.
    fn quote_text(self, text: &str) -> String {
        let mut quoted = String::with_capacity(text.len() + 2);
        quoted.push('\'');
        for character in text.chars() {
            match character {
                '\'' => quoted.push_str("''"),
                // These products read a backslash as an escape by default.
                '\\' if self.escapes_backslash() => quoted.push_str("\\\\"),
                other => quoted.push(other),
            }
        }
        quoted.push('\'');
        quoted
    }

    fn escapes_backslash(self) -> bool {
        matches!(self, Self::MySql | Self::MariaDb)
    }

    /// How the product writes a boolean literal.
    fn boolean(self, value: bool) -> &'static str {
        match self {
            Self::H2 | Self::PostgreSql => {
                if value {
                    "TRUE"
                } else {
                    "FALSE"
                }
            }
            Self::MySql | Self::MariaDb | Self::Sqlite | Self::Oracle | Self::SqlServer => {
                if value { "1" } else { "0" }
            }
        }
    }

    /// How the product pages a result: the offset first, then the row count.
    fn paging(self, limit: usize, offset: usize) -> String {
        match self {
            Self::Oracle | Self::SqlServer => {
                format!("OFFSET {offset} ROWS FETCH NEXT {limit} ROWS ONLY")
            }
            Self::H2 | Self::PostgreSql | Self::MySql | Self::MariaDb | Self::Sqlite => {
                format!("LIMIT {limit} OFFSET {offset}")
            }
        }
    }

    /// A date, a time or a timestamp of a column of this kind of the product.
    fn temporal(self, kind: ColumnKind, text: &str) -> String {
        let escaped = self.quote_text(text);
        match (self, kind) {
            (Self::Oracle, ColumnKind::Time)
            | (Self::Sqlite, ColumnKind::Date | ColumnKind::Time | ColumnKind::Timestamp)
            | (Self::SqlServer, ColumnKind::Date | ColumnKind::Time) => escaped,
            (Self::SqlServer, ColumnKind::Timestamp) => {
                format!("CAST({escaped} AS DATETIME2)")
            }
            (_, ColumnKind::Date) => format!("DATE {escaped}"),
            (_, ColumnKind::Time) => format!("TIME {escaped}"),
            (_, ColumnKind::Timestamp) => format!("TIMESTAMP {escaped}"),
            (_, _) => escaped,
        }
    }
}

/// The schema and name of the table a data tab shows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TableRef {
    pub schema: String,
    pub name: String,
}

impl TableRef {
    pub fn new(schema: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            schema: schema.into(),
            name: name.into(),
        }
    }

    /// The name as the statements write it.
    pub fn qualified(&self, dialect: Dialect) -> String {
        if self.schema.is_empty() {
            dialect.quote_name(&self.name)
        } else {
            format!(
                "{}.{}",
                dialect.quote_name(&self.schema),
                dialect.quote_name(&self.name)
            )
        }
    }
}

/// One page of a table: the filter of the user, the sort of the grid and the rows to read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PageQuery {
    pub table: TableRef,
    /// The text of the user after `WHERE`, without the keyword itself.
    pub filter: String,
    /// The name of the sorted column; `None` keeps the order of the table.
    pub sort: Option<String>,
    pub ascending: bool,
    /// The columns that tell a row apart, for a product that pages only in an order.
    pub key: Vec<String>,
    pub offset: usize,
    pub limit: usize,
}

impl PageQuery {
    /// The expression of the filter without a leading `WHERE`, when the user wrote one.
    pub fn filter_expression(&self) -> Option<&str> {
        filter_expression(&self.filter)
    }

    pub fn sql(&self, dialect: Dialect) -> String {
        let mut text = format!("SELECT * FROM {}", self.table.qualified(dialect));
        if let Some(filter) = self.filter_expression() {
            text.push_str(" WHERE (");
            text.push_str(filter);
            text.push(')');
        }
        text.push_str(&self.order_clause(dialect));
        text.push(' ');
        text.push_str(&dialect.paging(self.limit, self.offset));
        text
    }

    /// The sort of the grid, or the order that a product needs to page a result. SQL Server
    /// refuses `OFFSET … FETCH NEXT` without one, so a table without a sort is ordered by
    /// its key, and a table without a key by a constant that keeps the order of the table.
    fn order_clause(&self, dialect: Dialect) -> String {
        if let Some(column) = &self.sort {
            return format!(
                " ORDER BY {} {}",
                dialect.quote_name(column),
                if self.ascending { "ASC" } else { "DESC" }
            );
        }
        if !dialect.needs_order_for_paging() {
            return String::new();
        }
        if self.key.is_empty() {
            return " ORDER BY (SELECT NULL)".to_owned();
        }
        let columns: Vec<String> = self
            .key
            .iter()
            .map(|column| format!("{} ASC", dialect.quote_name(column)))
            .collect();
        format!(" ORDER BY {}", columns.join(", "))
    }

    pub fn count_sql(&self, dialect: Dialect) -> String {
        let mut text = format!("SELECT COUNT(*) FROM {}", self.table.qualified(dialect));
        if let Some(filter) = self.filter_expression() {
            text.push_str(" WHERE (");
            text.push_str(filter);
            text.push(')');
        }
        text
    }
}

/// The expression of a filter field, or nothing when the field holds only `WHERE`.
fn filter_expression(text: &str) -> Option<&str> {
    let text = text.trim();
    let rest = match text.get(..5) {
        Some(head) if head.eq_ignore_ascii_case("where") => text[5..].trim(),
        _ => text,
    };
    (!rest.is_empty()).then_some(rest)
}

/// The columns that tell a row of the table apart, in the order of the table: its primary
/// key, or a unique index over columns that cannot be NULL when it has no primary key. A
/// unique index with a nullable column does not tell two rows with NULL apart, and a value
/// without a primary key cannot be matched safely.
pub fn identifying_columns(table: &Table) -> Vec<usize> {
    let primary: Vec<usize> = table
        .columns
        .iter()
        .enumerate()
        .filter(|(_, column)| column.primary_key)
        .map(|(index, _)| index)
        .collect();
    if !primary.is_empty() {
        return primary;
    }
    for index in &table.indexes {
        if !index.unique || index.columns.is_empty() {
            continue;
        }
        let columns: Option<Vec<usize>> = index
            .columns
            .iter()
            .map(|name| {
                let index = column_of(table, name)?;
                (!table.columns[index].nullable).then_some(index)
            })
            .collect();
        if let Some(columns) = columns {
            return columns;
        }
    }
    Vec::new()
}

/// The position of a column of the table by its name, as the driver wrote it.
fn column_of(table: &Table, name: &str) -> Option<usize> {
    table
        .columns
        .iter()
        .position(|column| column.name == name)
        .or_else(|| {
            table
                .columns
                .iter()
                .position(|column| column.name.eq_ignore_ascii_case(name))
        })
}

/// One cell edit of a row: `None` means the column is left out of the statement, which
/// lets the database use its default.
#[derive(Clone, Debug, PartialEq)]
pub enum Cell {
    Unset,
    Value(Value),
}

/// What a row of the grid is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RowKind {
    #[default]
    Existing,
    New,
    Deleted,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DataRow {
    /// The values the row had when the page was read; `None` for a row the user added.
    original: Option<Vec<Value>>,
    values: Vec<Cell>,
    deleted: bool,
    /// The text of a failed statement of this row.
    error: Option<String>,
}

impl DataRow {
    fn existing(values: Vec<Value>) -> Self {
        Self {
            original: Some(values.clone()),
            values: values.into_iter().map(Cell::Value).collect(),
            deleted: false,
            error: None,
        }
    }

    fn new(columns: usize) -> Self {
        Self {
            original: None,
            values: vec![Cell::Unset; columns],
            deleted: false,
            error: None,
        }
    }

    pub fn kind(&self) -> RowKind {
        if self.deleted {
            RowKind::Deleted
        } else if self.original.is_none() {
            RowKind::New
        } else {
            RowKind::Existing
        }
    }

    pub fn cell(&self, column: usize) -> Option<&Value> {
        match self.values.get(column) {
            Some(Cell::Value(value)) => Some(value),
            _ => None,
        }
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    fn set(&mut self, column: usize, value: Cell) {
        if let Some(slot) = self.values.get_mut(column) {
            *slot = value;
        }
    }

    /// The original value of a column of a row that is already in the table.
    fn original(&self, column: usize) -> Option<&Value> {
        self.original.as_ref()?.get(column)
    }

    /// Whether the cell differs from the table or was set by the user of a new row.
    pub fn is_edited(&self, column: usize) -> bool {
        match self.original.as_ref().and_then(|row| row.get(column)) {
            Some(original) => match self.values.get(column) {
                Some(Cell::Value(value)) => !same_value(value, original),
                _ => true,
            },
            None => !matches!(self.values.get(column), None | Some(Cell::Unset)),
        }
    }

    fn edited(&self) -> bool {
        (0..self.values.len()).any(|column| self.is_edited(column))
    }
}

/// Whether two values are the same cell. A result may carry `NaN`, which never equals
/// itself, so a row of it must not look edited before the user touches it.
fn same_value(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Real(left), Value::Real(right)) => {
            left == right || (left.is_nan() && right.is_nan())
        }
        _ => left == right,
    }
}

/// The rows of one page together with the schema that types them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DataModel {
    columns: Vec<Column>,
    /// The columns that identify a row of the table; empty when no such column exists.
    key: Vec<usize>,
    rows: Vec<DataRow>,
}

impl DataModel {
    /// The kinds and keys of the columns come from the schema of the table, not from the
    /// metadata of the result, which names no key.
    pub fn new(columns: Vec<Column>) -> Self {
        let key = columns
            .iter()
            .enumerate()
            .filter(|(_, column)| column.primary_key)
            .map(|(index, _)| index)
            .collect();
        Self::with_key(columns, key)
    }

    /// A model whose rows are told apart by `key`, which the schema of the table names; a
    /// unique index may take the place of a primary key.
    pub fn with_key(columns: Vec<Column>, key: Vec<usize>) -> Self {
        let key = key
            .into_iter()
            .filter(|index| *index < columns.len())
            .collect();
        Self {
            columns,
            key,
            rows: Vec::new(),
        }
    }

    pub fn columns(&self) -> &[Column] {
        &self.columns
    }

    pub fn rows(&self) -> &[DataRow] {
        &self.rows
    }

    pub fn row(&self, index: usize) -> Option<&DataRow> {
        self.rows.get(index)
    }

    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    /// The columns that identify a row on the table; empty when the table has no key.
    pub fn key(&self) -> Vec<usize> {
        self.key.clone()
    }

    pub fn has_key(&self) -> bool {
        !self.key.is_empty()
    }

    /// Replaces the page. A row carries one value per column of the schema.
    pub fn set_rows(&mut self, rows: Vec<Vec<Value>>) {
        let columns = self.columns.len();
        self.rows = rows
            .into_iter()
            .map(|mut row| {
                row.resize(columns, Value::Null);
                row.truncate(columns);
                DataRow::existing(row)
            })
            .collect();
    }

    /// Drops every edit and returns the rows to the page that was read.
    pub fn revert(&mut self) {
        self.rows.retain(|row| row.original.is_some());
        for row in &mut self.rows {
            let Some(original) = row.original.clone() else {
                continue;
            };
            row.values = original.into_iter().map(Cell::Value).collect();
            row.deleted = false;
            row.error = None;
        }
    }

    pub fn is_dirty(&self) -> bool {
        self.rows
            .iter()
            .any(|row| row.original.is_none() || row.deleted || row.error.is_some() || row.edited())
    }

    /// Adds an empty row at the end and returns its index. The values of the columns stay
    /// unset until the user types them, so the database writes its defaults.
    pub fn add_row(&mut self) -> usize {
        self.rows.push(DataRow::new(self.columns.len()));
        self.rows.len() - 1
    }

    /// Marks a row of the table for deletion; a row the user added goes at once.
    pub fn delete_row(&mut self, index: usize) {
        let Some(row) = self.rows.get_mut(index) else {
            return;
        };
        if row.original.is_none() {
            self.rows.remove(index);
        } else {
            row.deleted = true;
        }
    }

    pub fn set_error(&mut self, index: usize, message: Option<String>) {
        if let Some(row) = self.rows.get_mut(index) {
            row.error = message;
        }
    }

    /// Puts the text of an editor into a cell. The text is read as the kind of the
    /// column says; text that does not fit is refused with a message of the user.
    pub fn set_text(&mut self, row: usize, column: usize, text: &str) -> Result<(), String> {
        let Some(kind) = self.columns.get(column).map(|column| column.kind) else {
            return Err("There is no such column.".to_owned());
        };
        let value = parse_text(kind, text)?;
        let Some(found) = self.rows.get_mut(row) else {
            return Err("There is no such row.".to_owned());
        };
        found.error = None;
        found.set(column, Cell::Value(value));
        Ok(())
    }

    /// Puts `NULL` into a cell, which only a nullable column takes.
    pub fn set_null(&mut self, row: usize, column: usize) -> Result<(), String> {
        let Some(found) = self.columns.get(column) else {
            return Err("There is no such column.".to_owned());
        };
        if !found.nullable {
            return Err(format!("{} does not allow NULL.", found.name));
        }
        let Some(found) = self.rows.get_mut(row) else {
            return Err("There is no such row.".to_owned());
        };
        found.error = None;
        found.set(column, Cell::Value(Value::Null));
        Ok(())
    }

    /// Whether the column of a cell may be edited at all: binary values have no literal
    /// that every product reads back the same way.
    pub fn is_editable_column(&self, column: usize) -> bool {
        self.columns
            .get(column)
            .is_some_and(|column| column.kind != ColumnKind::Binary)
    }

    /// The unsaved rows as the statements of one transaction.
    pub fn plan(&self, dialect: Dialect, table: &TableRef) -> Plan {
        let mut plan = Plan::default();
        let key = self.key();
        for (index, row) in self.rows.iter().enumerate() {
            let outcome = match row.kind() {
                RowKind::New => self.insert(dialect, table, row),
                RowKind::Deleted => self.delete(dialect, table, row, &key),
                RowKind::Existing if row.edited() => self.update(dialect, table, row, &key),
                RowKind::Existing => Outcome::Unchanged,
            };
            match outcome {
                Outcome::Unchanged => {}
                Outcome::Statement { sql, one_row } => plan.statements.push(Statement {
                    sql,
                    row: Some(index),
                    one_row,
                }),
                Outcome::Failed(text) => {
                    plan.error = Some(format!("Row {}: {text}", index + 1));
                    return plan;
                }
            }
        }
        plan
    }

    fn update(&self, dialect: Dialect, table: &TableRef, row: &DataRow, key: &[usize]) -> Outcome {
        let mut set = Vec::new();
        for (index, column) in self.columns.iter().enumerate() {
            if !row.is_edited(index) {
                continue;
            }
            let Some(value) = row.cell(index) else {
                set.push(format!("{} = DEFAULT", dialect.quote_name(&column.name)));
                continue;
            };
            match literal(dialect, column, value) {
                Some(text) => set.push(format!("{} = {text}", dialect.quote_name(&column.name))),
                None => return Outcome::failed(&column.name),
            }
        }
        if set.is_empty() {
            return Outcome::Unchanged;
        }
        let condition = match self.condition(dialect, row, key) {
            Ok(condition) => condition,
            Err(text) => return Outcome::Failed(text),
        };
        Outcome::statement(
            format!(
                "UPDATE {} SET {} WHERE {condition}",
                table.qualified(dialect),
                set.join(", ")
            ),
            true,
        )
    }

    fn insert(&self, dialect: Dialect, table: &TableRef, row: &DataRow) -> Outcome {
        let mut names = Vec::new();
        let mut values = Vec::new();
        for (index, column) in self.columns.iter().enumerate() {
            let Some(value) = row.cell(index) else {
                continue;
            };
            let Some(text) = literal(dialect, column, value) else {
                return Outcome::failed(&column.name);
            };
            names.push(dialect.quote_name(&column.name));
            values.push(text);
        }
        if names.is_empty() {
            return Outcome::Failed("the new row has no value".to_owned());
        }
        Outcome::statement(
            format!(
                "INSERT INTO {} ({}) VALUES ({})",
                table.qualified(dialect),
                names.join(", "),
                values.join(", ")
            ),
            false,
        )
    }

    fn delete(&self, dialect: Dialect, table: &TableRef, row: &DataRow, key: &[usize]) -> Outcome {
        let condition = match self.condition(dialect, row, key) {
            Ok(condition) => condition,
            Err(text) => return Outcome::Failed(text),
        };
        Outcome::statement(
            format!("DELETE FROM {} WHERE {condition}", table.qualified(dialect)),
            true,
        )
    }

    /// The key of the row. A table without a key of its own cannot name one row among equal
    /// ones, so its rows are not written at all.
    fn condition(&self, dialect: Dialect, row: &DataRow, key: &[usize]) -> Result<String, String> {
        if key.is_empty() {
            return Err(NO_KEY.to_owned());
        }
        let mut parts = Vec::new();
        for &index in key {
            let column = self
                .columns
                .get(index)
                .ok_or_else(|| "the row has no such column".to_owned())?;
            let value = row
                .original(index)
                .ok_or_else(|| "the row has no original value".to_owned())?;
            match value {
                Value::Null => parts.push(format!("{} IS NULL", dialect.quote_name(&column.name))),
                other => {
                    let text = literal(dialect, column, other).ok_or_else(|| {
                        format!("{} has no literal to find the row by", column.name)
                    })?;
                    parts.push(format!("{} = {text}", dialect.quote_name(&column.name)));
                }
            }
        }
        Ok(parts.join(" AND "))
    }
}

/// What a table without a key cannot do, in the words the window shows.
pub const NO_KEY: &str = "the table has no key, so a row cannot be told apart";

/// What one row contributes to a plan, so that a row that cannot be written stops the
/// whole plan instead of hiding among the warnings.
enum Outcome {
    Unchanged,
    Statement { sql: String, one_row: bool },
    Failed(String),
}

impl Outcome {
    fn statement(sql: String, one_row: bool) -> Self {
        Outcome::Statement { sql, one_row }
    }

    fn failed(what: &str) -> Self {
        Outcome::Failed(format!(
            "the value of {what} cannot be written as a literal"
        ))
    }
}

/// One statement of Submit and the row of the grid it writes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Statement {
    pub sql: String,
    /// The index of the row in the model, for a message at that row.
    pub row: Option<usize>,
    /// The statement names one row by its key, so the product must report exactly one
    /// changed row; any other count means it wrote other rows or none at all.
    pub one_row: bool,
}

/// What Submit sends and what the user should know before it does.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Plan {
    pub statements: Vec<Statement>,
    pub warnings: Vec<String>,
    /// A row cannot be written safely: nothing is sent at all.
    pub error: Option<String>,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.statements.is_empty() && self.error.is_none()
    }
}

/// Reads the text of a cell as the kind of its column says. Text keeps every space the user
/// typed: a column of text may hold them, and the grid shows them again after a read.
fn parse_text(kind: ColumnKind, text: &str) -> Result<Value, String> {
    let trimmed = text.trim();
    match kind {
        ColumnKind::Integer => trimmed
            .parse::<i64>()
            .map(Value::Integer)
            .map_err(|_| format!("{trimmed} is not a whole number.")),
        ColumnKind::Decimal | ColumnKind::Number => {
            if is_number(trimmed) {
                Ok(Value::Decimal(trimmed.to_owned()))
            } else {
                Err(format!("{trimmed} is not a number."))
            }
        }
        ColumnKind::Boolean => match trimmed.to_ascii_lowercase().as_str() {
            "true" | "t" | "1" | "yes" => Ok(Value::Boolean(true)),
            "false" | "f" | "0" | "no" => Ok(Value::Boolean(false)),
            _ => Err(format!("{trimmed} is not true or false.")),
        },
        ColumnKind::Date => Ok(Value::Date(trimmed.to_owned())),
        ColumnKind::Time => Ok(Value::Time(trimmed.to_owned())),
        ColumnKind::Timestamp => Ok(Value::Timestamp(trimmed.to_owned())),
        ColumnKind::Binary => Err("A binary value is read-only.".to_owned()),
        ColumnKind::Text | ColumnKind::Other => Ok(Value::Text(text.to_owned())),
    }
}

/// Whether a text is a number a product reads as one. A sign, digits, one point and an
/// exponent are enough for every dialect here.
fn is_number(text: &str) -> bool {
    let text = text.trim();
    if text.is_empty() {
        return false;
    }
    let mut digits = 0;
    let mut point = false;
    let mut exponent = false;
    let mut previous = ' ';
    for (index, character) in text.chars().enumerate() {
        match character {
            '+' | '-' => {
                let first = index == 0;
                let after_exponent = exponent && matches!(previous, 'e' | 'E');
                if !first && !after_exponent {
                    return false;
                }
            }
            '.' => {
                if point || exponent {
                    return false;
                }
                point = true;
            }
            'e' | 'E' => {
                if exponent || digits == 0 {
                    return false;
                }
                exponent = true;
            }
            character if character.is_ascii_digit() => digits += 1,
            _ => return false,
        }
        previous = character;
    }
    digits > 0
}

/// A value as a literal of the product, when the kind of the column has one.
fn literal(dialect: Dialect, column: &Column, value: &Value) -> Option<String> {
    match (column.kind, value) {
        (_, Value::Null) => Some("NULL".to_owned()),
        (ColumnKind::Binary, _) | (_, Value::Binary { .. }) => None,
        (ColumnKind::Integer, Value::Integer(integer)) => Some(integer.to_string()),
        (ColumnKind::Integer, Value::Decimal(text)) if is_number(text) => {
            Some(text.trim().to_owned())
        }
        (ColumnKind::Decimal | ColumnKind::Number, Value::Integer(integer)) => {
            Some(integer.to_string())
        }
        (ColumnKind::Decimal | ColumnKind::Number, Value::Decimal(text)) if is_number(text) => {
            Some(text.trim().to_owned())
        }
        (ColumnKind::Decimal | ColumnKind::Number, Value::Real(real)) => Some(real_text(*real)),
        (ColumnKind::Boolean, Value::Boolean(boolean)) => {
            Some(dialect.boolean(*boolean).to_owned())
        }
        (ColumnKind::Date | ColumnKind::Time | ColumnKind::Timestamp, value) => {
            let text = text_of(value)?;
            Some(dialect.temporal(column.kind, text))
        }
        (_, Value::Text(text)) => Some(dialect.quote_text(text)),
        (_, Value::Integer(integer)) => Some(integer.to_string()),
        (_, Value::Decimal(text)) if is_number(text) => Some(text.trim().to_owned()),
        (_, Value::Real(real)) => Some(real_text(*real)),
        (_, Value::Boolean(boolean)) => Some(dialect.boolean(*boolean).to_owned()),
        (_, value) => text_of(value).map(|text| dialect.quote_text(text)),
    }
}

fn text_of(value: &Value) -> Option<&str> {
    match value {
        Value::Text(text) | Value::Date(text) | Value::Time(text) | Value::Timestamp(text) => {
            Some(text)
        }
        _ => None,
    }
}

fn real_text(real: f64) -> String {
    if real.is_nan() {
        "NaN".to_owned()
    } else if real == f64::INFINITY {
        "Infinity".to_owned()
    } else if real == f64::NEG_INFINITY {
        "-Infinity".to_owned()
    } else {
        real.to_string()
    }
}

/// One row of a copy as an `INSERT` of the product.
pub fn insert_literal(
    dialect: Dialect,
    table: &TableRef,
    columns: &[Column],
    row: &[Value],
) -> String {
    let names: Vec<String> = columns
        .iter()
        .map(|column| dialect.quote_name(&column.name))
        .collect();
    let values: Vec<String> = columns
        .iter()
        .zip(row)
        .map(|(column, value)| literal(dialect, column, value).unwrap_or_else(|| "NULL".to_owned()))
        .collect();
    format!(
        "INSERT INTO {} ({}) VALUES ({})",
        table.qualified(dialect),
        names.join(", "),
        values.join(", ")
    )
}

/// Rows of a copy as comma-separated text, with the header of the columns.
pub fn csv(columns: &[Column], rows: &[Vec<Value>]) -> String {
    let mut text = String::new();
    let names: Vec<String> = columns
        .iter()
        .map(|column| csv_field(&column.name))
        .collect();
    text.push_str(&names.join(","));
    text.push('\n');
    for row in rows {
        let fields: Vec<String> = row.iter().map(|value| csv_field(&value.text())).collect();
        text.push_str(&fields.join(","));
        text.push('\n');
    }
    text
}

/// Selected cells of a grid as tab-separated text, one line per row.
pub fn tsv(rows: &[Vec<Value>]) -> String {
    rows.iter()
        .map(|row| row.iter().map(Value::text).collect::<Vec<_>>().join("\t"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::Index;

    fn column(name: &str, kind: ColumnKind, nullable: bool, primary_key: bool) -> Column {
        Column {
            name: name.into(),
            type_name: "T".into(),
            kind,
            nullable,
            primary_key,
        }
    }

    fn owners() -> Vec<Column> {
        vec![
            column("ID", ColumnKind::Integer, false, true),
            column("NAME", ColumnKind::Text, true, false),
            column("AGE", ColumnKind::Integer, true, false),
        ]
    }

    fn table() -> TableRef {
        TableRef::new("PUBLIC", "OWNERS")
    }

    fn model() -> DataModel {
        let mut model = DataModel::new(owners());
        model.set_rows(vec![
            vec![
                Value::Integer(1),
                Value::Text("Ann".into()),
                Value::Integer(30),
            ],
            vec![Value::Integer(2), Value::Text("O'Neil".into()), Value::Null],
        ]);
        model
    }

    #[test]
    fn a_page_query_carries_the_filter_the_sort_and_the_rows() {
        let query = PageQuery {
            table: table(),
            filter: "AGE > 18".into(),
            sort: Some("NAME".into()),
            ascending: false,
            key: vec!["ID".into()],
            offset: 500,
            limit: 500,
        };

        assert_eq!(
            query.sql(Dialect::H2),
            "SELECT * FROM \"PUBLIC\".\"OWNERS\" WHERE (AGE > 18) ORDER BY \"NAME\" DESC \
             LIMIT 500 OFFSET 500"
        );
        assert_eq!(
            query.sql(Dialect::Oracle),
            "SELECT * FROM \"PUBLIC\".\"OWNERS\" WHERE (AGE > 18) ORDER BY \"NAME\" DESC \
             OFFSET 500 ROWS FETCH NEXT 500 ROWS ONLY"
        );
        assert_eq!(
            query.count_sql(Dialect::MySql),
            "SELECT COUNT(*) FROM `PUBLIC`.`OWNERS` WHERE (AGE > 18)"
        );
    }

    #[test]
    fn a_filter_field_takes_the_word_where_or_leaves_it_out() {
        let mut query = PageQuery {
            table: table(),
            limit: 10,
            ..PageQuery::default()
        };
        assert_eq!(
            query.sql(Dialect::H2),
            "SELECT * FROM \"PUBLIC\".\"OWNERS\" LIMIT 10 OFFSET 0"
        );

        query.filter = "  where age > 1 ".into();
        assert_eq!(query.filter_expression(), Some("age > 1"));
        assert!(query.sql(Dialect::H2).contains("WHERE (age > 1)"));

        query.filter = "WHERE".into();
        assert!(query.filter_expression().is_none());
        assert!(!query.sql(Dialect::H2).contains("WHERE"));
    }

    #[test]
    fn every_product_wraps_names_in_its_own_quotes() {
        let names = [
            (Dialect::H2, "\"A\"\"B\""),
            (Dialect::PostgreSql, "\"A\"\"B\""),
            (Dialect::MySql, "`A\"B`"),
            (Dialect::MariaDb, "`A\"B`"),
            (Dialect::Sqlite, "\"A\"\"B\""),
            (Dialect::Oracle, "\"A\"\"B\""),
            (Dialect::SqlServer, "\"A\"\"B\""),
        ];
        for (dialect, expected) in names {
            let name = "A\"B";
            assert_eq!(dialect.quote_name(name), expected, "{dialect:?}");
        }
        assert_eq!(Dialect::MySql.quote_name("A`B"), "`A``B`");
    }

    #[test]
    fn an_update_finds_a_row_by_its_key_and_writes_the_changed_cells() {
        let mut model = model();
        model.set_text(0, 1, "Ann Lee").unwrap();
        model.set_null(0, 2).unwrap();

        let plan = model.plan(Dialect::H2, &table());
        assert_eq!(plan.error, None);
        assert_eq!(
            plan.statements,
            vec![Statement {
                sql: "UPDATE \"PUBLIC\".\"OWNERS\" SET \"NAME\" = 'Ann Lee', \"AGE\" = NULL \
                      WHERE \"ID\" = 1"
                    .into(),
                row: Some(0),
                one_row: true,
            }]
        );
        assert!(plan.warnings.is_empty());
    }

    #[test]
    fn an_update_of_a_table_without_a_key_is_refused() {
        let mut columns = owners();
        columns[0].primary_key = false;
        let mut model = DataModel::new(columns);
        model.set_rows(vec![vec![
            Value::Integer(1),
            Value::Text("Ann".into()),
            Value::Null,
        ]]);
        model.set_text(0, 1, "Ann Lee").unwrap();

        let plan = model.plan(Dialect::PostgreSql, &table());
        assert!(plan.statements.is_empty());
        assert_eq!(plan.error, Some(format!("Row 1: {NO_KEY}")));
    }

    #[test]
    fn a_new_row_becomes_an_insert_of_the_columns_it_sets() {
        let mut model = model();
        let row = model.add_row();
        model.set_text(row, 0, "3").unwrap();
        model.set_text(row, 1, "Bob O'Neil").unwrap();

        let plan = model.plan(Dialect::MySql, &table());
        assert_eq!(
            plan.statements[0].sql,
            "INSERT INTO `PUBLIC`.`OWNERS` (`ID`, `NAME`) VALUES (3, 'Bob O''Neil')"
        );

        // A row with no value at all cannot be written: every product spells it its own way.
        let empty = model.add_row();
        let plan = model.plan(Dialect::MySql, &table());
        assert_eq!(
            plan.error,
            Some(format!("Row {}: the new row has no value", empty + 1))
        );
    }

    #[test]
    fn a_deleted_row_becomes_a_delete_of_its_key() {
        let mut model = model();
        model.delete_row(1);

        let plan = model.plan(Dialect::H2, &table());
        assert_eq!(
            plan.statements,
            vec![Statement {
                sql: "DELETE FROM \"PUBLIC\".\"OWNERS\" WHERE \"ID\" = 2".into(),
                row: Some(1),
                one_row: true,
            }]
        );
        assert_eq!(model.row(1).map(DataRow::kind), Some(RowKind::Deleted));
    }

    #[test]
    fn every_product_writes_an_update_an_insert_and_a_delete() {
        let mut model = model();
        model.set_text(0, 1, "Ann O'Neil").unwrap();
        let row = model.add_row();
        model.set_text(row, 0, "3").unwrap();
        model.set_text(row, 1, "Bob").unwrap();
        model.delete_row(1);

        for dialect in [
            Dialect::H2,
            Dialect::PostgreSql,
            Dialect::MySql,
            Dialect::MariaDb,
            Dialect::Sqlite,
            Dialect::Oracle,
            Dialect::SqlServer,
        ] {
            let qualified = table().qualified(dialect);
            let id = dialect.quote_name("ID");
            let name = dialect.quote_name("NAME");
            let plan = model.plan(dialect, &table());
            assert_eq!(plan.error, None, "{dialect:?}");
            assert_eq!(
                plan.statements,
                vec![
                    Statement {
                        sql: format!(
                            "UPDATE {qualified} SET {name} = 'Ann O''Neil' WHERE {id} = 1"
                        ),
                        row: Some(0),
                        one_row: true,
                    },
                    Statement {
                        sql: format!("DELETE FROM {qualified} WHERE {id} = 2"),
                        row: Some(1),
                        one_row: true,
                    },
                    Statement {
                        sql: format!("INSERT INTO {qualified} ({id}, {name}) VALUES (3, 'Bob')"),
                        row: Some(2),
                        one_row: false,
                    },
                ],
                "{dialect:?}"
            );
        }
    }

    #[test]
    fn a_new_row_that_is_deleted_goes_without_a_statement() {
        let mut model = model();
        let row = model.add_row();
        model.delete_row(row);

        assert_eq!(model.row_count(), 2);
        assert!(!model.is_dirty());
        assert!(model.plan(Dialect::H2, &table()).is_empty());
    }

    #[test]
    fn a_text_is_read_as_the_kind_of_its_column() {
        let mut model = DataModel::new(vec![
            column("ID", ColumnKind::Integer, false, true),
            column("AMOUNT", ColumnKind::Decimal, true, false),
            column("OK", ColumnKind::Boolean, true, false),
            column("AT", ColumnKind::Date, true, false),
            column("AT2", ColumnKind::Timestamp, true, false),
            column("NOTE", ColumnKind::Text, true, false),
        ]);
        model.set_rows(vec![vec![
            Value::Integer(1),
            Value::Decimal("1.50".into()),
            Value::Boolean(true),
            Value::Date("2024-01-02".into()),
            Value::Timestamp("2024-01-02 03:04:05".into()),
            Value::Text("a".into()),
        ]]);
        model.set_text(0, 1, "2.25").unwrap();
        model.set_text(0, 2, "false").unwrap();
        model.set_text(0, 3, "2025-03-04").unwrap();
        model.set_text(0, 5, "it's").unwrap();

        assert_eq!(
            model.set_text(0, 0, "x"),
            Err("x is not a whole number.".into())
        );
        assert_eq!(model.set_text(0, 1, "x"), Err("x is not a number.".into()));
        assert_eq!(
            model.set_text(0, 2, "x"),
            Err("x is not true or false.".into())
        );

        let plan = model.plan(Dialect::H2, &table());
        assert_eq!(
            plan.statements[0].sql,
            "UPDATE \"PUBLIC\".\"OWNERS\" SET \"AMOUNT\" = 2.25, \"OK\" = FALSE, \
             \"AT\" = DATE '2025-03-04', \"NOTE\" = 'it''s' WHERE \"ID\" = 1"
        );
    }

    #[test]
    fn dates_and_booleans_take_the_form_of_the_product() {
        let mut columns = vec![
            column("ID", ColumnKind::Integer, false, true),
            column("OK", ColumnKind::Boolean, true, false),
            column("AT", ColumnKind::Date, true, false),
            column("AT2", ColumnKind::Timestamp, true, false),
        ];
        columns.insert(2, column("AT0", ColumnKind::Time, true, false));
        let mut model = DataModel::new(columns);
        model.set_rows(vec![vec![
            Value::Integer(1),
            Value::Boolean(true),
            Value::Time("03:04:05".into()),
            Value::Date("2024-01-02".into()),
            Value::Timestamp("2024-01-02 03:04:05".into()),
        ]]);
        model.set_text(0, 1, "false").unwrap();
        model.set_text(0, 2, "04:05:06").unwrap();
        model.set_text(0, 3, "2024-05-06").unwrap();
        model.set_text(0, 4, "2024-05-06 07:08:09").unwrap();

        let h2 = &model.plan(Dialect::H2, &table()).statements[0].sql;
        assert!(h2.contains("\"OK\" = FALSE"), "{h2}");
        assert!(h2.contains("\"AT0\" = TIME '04:05:06'"), "{h2}");
        assert!(h2.contains("\"AT\" = DATE '2024-05-06'"), "{h2}");
        assert!(
            h2.contains("\"AT2\" = TIMESTAMP '2024-05-06 07:08:09'"),
            "{h2}"
        );

        let mysql = &model.plan(Dialect::MySql, &table()).statements[0].sql;
        assert!(mysql.contains("`OK` = 0"), "{mysql}");

        let oracle = &model.plan(Dialect::Oracle, &table()).statements[0].sql;
        assert!(oracle.contains("\"OK\" = 0"), "{oracle}");
        assert!(oracle.contains("\"AT0\" = '04:05:06'"), "{oracle}");

        let sql_server = &model.plan(Dialect::SqlServer, &table()).statements[0].sql;
        assert!(sql_server.contains("\"AT\" = '2024-05-06'"), "{sql_server}");
        assert!(
            sql_server.contains("\"AT2\" = CAST('2024-05-06 07:08:09' AS DATETIME2)"),
            "{sql_server}"
        );

        let sqlite = &model.plan(Dialect::Sqlite, &table()).statements[0].sql;
        assert!(sqlite.contains("\"AT\" = '2024-05-06'"), "{sqlite}");
    }

    #[test]
    fn a_backslash_is_escaped_for_the_products_that_read_it() {
        let mut model = model();
        model.set_text(0, 1, "a\\b").unwrap();

        assert!(
            model.plan(Dialect::MySql, &table()).statements[0]
                .sql
                .contains("'a\\\\b'")
        );
        assert!(
            model.plan(Dialect::H2, &table()).statements[0]
                .sql
                .contains("'a\\b'")
        );
    }

    #[test]
    fn null_is_refused_by_a_column_that_does_not_allow_it() {
        let mut model = model();

        assert_eq!(model.set_null(0, 0), Err("ID does not allow NULL.".into()));
        assert_eq!(model.set_null(0, 1), Ok(()));
        assert_eq!(model.row(0).and_then(|row| row.cell(1)), Some(&Value::Null));
    }

    #[test]
    fn a_row_with_a_nan_is_not_edited_before_the_user_touches_it() {
        let mut model = DataModel::new(vec![
            column("ID", ColumnKind::Integer, false, true),
            column("SCORE", ColumnKind::Number, true, false),
        ]);
        model.set_rows(vec![vec![Value::Integer(1), Value::Real(f64::NAN)]]);

        assert!(!model.is_dirty());
        assert!(!model.row(0).expect("the row is read").is_edited(1));
    }

    #[test]
    fn revert_drops_the_edits_the_new_rows_and_the_deletions() {
        let mut model = model();
        model.set_text(0, 1, "Ann Lee").unwrap();
        model.delete_row(1);
        model.add_row();

        assert!(model.is_dirty());
        model.revert();

        assert!(!model.is_dirty());
        assert_eq!(model.row_count(), 2);
        assert_eq!(
            model.row(0).and_then(|row| row.cell(1)),
            Some(&Value::Text("Ann".into()))
        );
        assert_eq!(model.row(1).map(DataRow::kind), Some(RowKind::Existing));
        assert!(model.plan(Dialect::H2, &table()).is_empty());
    }

    #[test]
    fn only_the_changed_cells_of_a_row_are_written() {
        let mut model = model();
        model.set_text(0, 2, "31").unwrap();

        assert_eq!(
            model.plan(Dialect::H2, &table()).statements[0].sql,
            "UPDATE \"PUBLIC\".\"OWNERS\" SET \"AGE\" = 31 WHERE \"ID\" = 1"
        );
    }

    #[test]
    fn a_binary_value_has_no_literal_and_stops_the_plan() {
        let mut columns = owners();
        columns[0].primary_key = false;
        columns.push(column("PHOTO", ColumnKind::Binary, true, true));
        // The key of the table is the binary column, and a binary value has no literal: the
        // statement cannot name the row it changes.
        let mut model = DataModel::new(columns);
        model.set_rows(vec![vec![
            Value::Integer(1),
            Value::Text("Ann".into()),
            Value::Null,
            Value::Binary {
                base64: "AA==".into(),
                bytes: 1,
                truncated: false,
            },
        ]]);
        model.set_text(0, 1, "Ann Lee").unwrap();

        let plan = model.plan(Dialect::H2, &table());
        assert_eq!(
            plan.error,
            Some("Row 1: PHOTO has no literal to find the row by".into())
        );
        assert!(plan.statements.is_empty());
        assert!(!model.is_editable_column(3));
    }

    #[test]
    fn a_copy_of_rows_is_an_insert_and_a_csv() {
        let row = vec![
            Value::Integer(1),
            Value::Text("Ann, \"A\"".into()),
            Value::Null,
        ];
        assert_eq!(
            insert_literal(Dialect::H2, &table(), &owners(), &row),
            "INSERT INTO \"PUBLIC\".\"OWNERS\" (\"ID\", \"NAME\", \"AGE\") \
             VALUES (1, 'Ann, \"A\"', NULL)"
        );
        assert_eq!(
            csv(&owners(), std::slice::from_ref(&row)),
            "ID,NAME,AGE\n1,\"Ann, \"\"A\"\"\",NULL\n"
        );
        assert_eq!(tsv(&[row]), "1\tAnn, \"A\"\tNULL");
    }

    #[test]
    fn sql_server_pages_only_in_an_order() {
        let mut query = PageQuery {
            table: table(),
            key: vec!["ID".into()],
            limit: 10,
            ..PageQuery::default()
        };
        assert_eq!(
            query.sql(Dialect::SqlServer),
            "SELECT * FROM \"PUBLIC\".\"OWNERS\" ORDER BY \"ID\" ASC \
             OFFSET 0 ROWS FETCH NEXT 10 ROWS ONLY"
        );

        query.key.clear();
        assert_eq!(
            query.sql(Dialect::SqlServer),
            "SELECT * FROM \"PUBLIC\".\"OWNERS\" ORDER BY (SELECT NULL) \
             OFFSET 0 ROWS FETCH NEXT 10 ROWS ONLY"
        );

        // A chosen sort wins over the key, and the other products need no order at all.
        query.sort = Some("NAME".into());
        query.ascending = true;
        assert_eq!(
            query.sql(Dialect::SqlServer),
            "SELECT * FROM \"PUBLIC\".\"OWNERS\" ORDER BY \"NAME\" ASC \
             OFFSET 0 ROWS FETCH NEXT 10 ROWS ONLY"
        );
        let plain = PageQuery {
            table: table(),
            limit: 10,
            ..PageQuery::default()
        };
        assert!(!plain.sql(Dialect::PostgreSql).contains("ORDER BY"));
    }

    #[test]
    fn a_text_keeps_the_spaces_around_it() {
        let mut model = model();
        model.set_text(0, 1, "  Ann  ").unwrap();

        assert_eq!(
            model.row(0).and_then(|row| row.cell(1)),
            Some(&Value::Text("  Ann  ".into()))
        );
        assert_eq!(
            model.plan(Dialect::H2, &table()).statements[0].sql,
            "UPDATE \"PUBLIC\".\"OWNERS\" SET \"NAME\" = '  Ann  ' WHERE \"ID\" = 1"
        );
    }

    #[test]
    fn a_unique_index_over_columns_that_cannot_be_null_is_a_key() {
        let mut table = schema_table();
        table.indexes = vec![Index {
            name: "UQ_EMAIL".into(),
            unique: true,
            columns: vec!["EMAIL".into()],
        }];
        table.columns[1].nullable = false;
        assert_eq!(identifying_columns(&table), vec![1]);
        assert_eq!(
            identifier_names(&table, identifying_columns(&table)),
            vec!["EMAIL".to_owned()]
        );

        // A unique index over a column that takes NULL does not tell two rows apart.
        table.columns[1].nullable = true;
        assert!(identifying_columns(&table).is_empty());

        // A primary key wins over every index.
        table.columns[0].primary_key = true;
        assert_eq!(identifying_columns(&table), vec![0]);
    }

    fn schema_table() -> Table {
        Table {
            schema: "PUBLIC".into(),
            name: "OWNERS".into(),
            kind: crate::schema::TableKind::Table,
            columns: vec![
                column("ID", ColumnKind::Integer, false, false),
                column("EMAIL", ColumnKind::Text, false, false),
            ],
            indexes: Vec::new(),
            foreign_keys: Vec::new(),
            partial: false,
        }
    }

    fn identifier_names(table: &Table, key: Vec<usize>) -> Vec<String> {
        key.into_iter()
            .map(|index| table.columns[index].name.clone())
            .collect()
    }
}
