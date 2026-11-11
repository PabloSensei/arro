use std::fmt;

use lsp_types::Uri;
use serde_json::Value;

/// JSON-RPC error object, either received from the server or sent to it.
#[derive(Clone, Debug, PartialEq)]
pub struct ResponseError {
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
}

impl ResponseError {
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const REQUEST_CANCELLED: i64 = -32800;

    pub fn method_not_found(method: &str) -> Self {
        Self {
            code: Self::METHOD_NOT_FOUND,
            message: format!("unhandled method {method}"),
            data: None,
        }
    }
}

#[derive(Debug)]
pub enum Error {
    /// The connection ended, this request will never be answered.
    Closed,
    /// The server confirmed a `$/cancelRequest`.
    Cancelled,
    /// The server answered with an error.
    Server(ResponseError),
    /// A message that does not fit the protocol or the expected type.
    Decode(serde_json::Error),
    /// A document operation on a document that is not open.
    NotOpen(Uri),
    /// `didOpen` of a document that is already open.
    AlreadyOpen(Uri),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Closed => f.write_str("connection closed"),
            Self::Cancelled => f.write_str("request cancelled"),
            Self::Server(error) => write!(f, "server error {}: {}", error.code, error.message),
            Self::Decode(error) => write!(f, "bad message: {error}"),
            Self::NotOpen(uri) => write!(f, "document not open: {}", uri.as_str()),
            Self::AlreadyOpen(uri) => write!(f, "document already open: {}", uri.as_str()),
        }
    }
}

impl std::error::Error for Error {}

impl From<serde_json::Error> for Error {
    fn from(error: serde_json::Error) -> Self {
        Self::Decode(error)
    }
}
