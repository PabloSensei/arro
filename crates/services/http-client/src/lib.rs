//! The request files of the HTTP client: the text format of a `.http` or `.rest` file,
//! the environments of `http-client.env.json`, the request itself and its response.
//! There is no UI here and no work on the caller's thread: the caller runs every
//! request away from the window.

use std::path::Path;

mod env;
mod execute;
mod highlight;
mod history;
mod json;
mod parse;
mod variables;

#[cfg(test)]
mod crate_tests;
#[cfg(test)]
mod execute_tests;
#[cfg(test)]
mod test_server;

pub use env::{Environments, environment_files, load_environments};
pub use execute::{Cancel, DEFAULT_TIMEOUT, Error, Response, execute};
pub use highlight::{Span, TokenKind, highlight, highlight_json};
pub use history::{HistoryEntry, history_dir, load_history, read_body, save_response};
pub use json::pretty_json;
pub use parse::{Body, Method, ParsedFile, Problem, Request, parse};
pub use variables::{PreparedRequest, Variables, prepare, substitute};

/// Whether `path` is a request file the HTTP client shows.
pub fn is_request_file(path: &Path) -> bool {
    path.extension().is_some_and(|extension| {
        extension.eq_ignore_ascii_case("http") || extension.eq_ignore_ascii_case("rest")
    })
}

/// The default of the timeout setting, in seconds.
pub const DEFAULT_TIMEOUT_SECONDS: u32 = 60;

/// The limits the panel and the tests share.
pub mod limits {
    /// A body longer than this is shown from its beginning until the reader asks for
    /// all of it.
    pub const SHOWN_BODY_BYTES: usize = 1024 * 1024;
    /// Responses kept in the state folder.
    pub const HISTORY: usize = 50;
}
