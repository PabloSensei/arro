//! What the console shows: the tree of a source and the answer to one statement.

use crate::protocol::Value;

/// One catalog of a source, with the schemas under it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Schema {
    pub catalog: Option<String>,
    pub schema: String,
    pub tables: Vec<Table>,
}

/// Whether a table is a table or a view.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TableKind {
    #[default]
    Table,
    View,
}

/// One table or view with what the driver knows about it. A driver that cannot answer
/// some of it sends the rest and says that the part is missing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Table {
    pub schema: String,
    pub name: String,
    pub kind: TableKind,
    pub columns: Vec<Column>,
    pub indexes: Vec<Index>,
    pub foreign_keys: Vec<ForeignKey>,
    /// Keys or indexes the driver refused to describe.
    pub partial: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Column {
    pub name: String,
    /// The type name of the driver, such as `VARCHAR` or `NUMERIC(10,2)`.
    pub type_name: String,
    pub kind: ColumnKind,
    pub nullable: bool,
    pub primary_key: bool,
}

/// How the console reads a value, not the exact type of the driver.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ColumnKind {
    Integer,
    Decimal,
    Number,
    Boolean,
    Text,
    Date,
    Time,
    Timestamp,
    Binary,
    #[default]
    Other,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Index {
    pub name: String,
    pub unique: bool,
    pub columns: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ForeignKey {
    pub name: String,
    pub columns: Vec<String>,
    pub referenced_table: String,
    pub referenced_columns: Vec<String>,
}

/// The answer to a statement: a grid, an update count, or both when a driver gives both.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ResultSet {
    pub columns: Vec<Column>,
    pub rows: Vec<Vec<Value>>,
    pub update_count: Option<u64>,
    /// The result was longer than [`crate::MAX_RESULT_ROWS`] or
    /// [`crate::MAX_RESULT_BYTES`], so the rest was not read.
    pub truncated: bool,
}
