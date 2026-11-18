//! Database connections, their schema and the requests of a console.
//!
//! The database itself is reached by a helper that runs next to the project: one helper
//! process per source, line-delimited JSON over its standard streams. This crate owns the
//! protocol, the process and the files of the sources; it has no UI and does no work on
//! the caller's thread. The helper and the search for a driver belong to the language
//! layer, which hands this crate a [`Launch`].

mod complete;
mod console;
mod data;
mod explain;
mod export;
mod format;
mod passwords;
mod protocol;
mod schema;
mod session;
mod sources;

pub use complete::{
    SqlContext, SqlProblem, SqlSuggestion, SuggestionKind, TableUse, complete, context,
    countable_statement, problems, single_table,
};
pub use console::{
    Parameter, asked_parameters, bind_parameters, parameters, split_statements, statement_at,
};
pub use data::{
    Cell, DataModel, DataRow, Dialect, PageQuery, Plan, RowKind, Statement, TableRef, csv,
    insert_literal, tsv,
};
pub use explain::{ExplainNode, ExplainPlan, explain_statement, plan_of};
pub use export::{ExportFormat, Exporter};
pub use format::{FormatOptions, KeywordCase, format_sql, format_sql_range};
pub use passwords::{PasswordStore, SystemPasswords, service_of};
pub use protocol::{Command, Event, PROTOCOL, Param, Value};
pub use schema::{Column, ColumnKind, ForeignKey, Index, ResultSet, Schema, Table, TableKind};
pub use session::{Launch, Session, explain_output};
pub use sources::{
    DownloadProgress, Driver, DriverKind, DriverPackage, Secret, Source, SourceId, SourceStore,
    Suggestion, Support, VendorTemplate, console_path, ensure_console, source_of_console,
    state_folder,
};

/// How many rows one message carries before the console asks for the next one.
pub const BATCH_ROWS: usize = 500;
/// The most text one message carries besides the row count.
pub const BATCH_BYTES: usize = 256 * 1024;
/// The rows one result keeps; a longer result is shown up to here and marked truncated.
pub const MAX_RESULT_ROWS: usize = 10_000;
/// The bytes one result keeps, across all its messages.
pub const MAX_RESULT_BYTES: usize = 8 * 1024 * 1024;
/// How long a connection may take before the helper is ended and the source says why.
pub const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
/// How long a cancelled statement may take before the helper is ended.
pub const CANCEL_GRACE: std::time::Duration = std::time::Duration::from_secs(5);
/// How long a value of a binary column may be before the rest is left out.
pub const MAX_BINARY_BYTES: usize = 4 * 1024;
/// How long a large text value may be before the rest is left out.
pub const MAX_TEXT_BYTES: usize = 64 * 1024;
