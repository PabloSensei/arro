//! Generic LSP client: framing, requests, cancellation, document versions, UTF-16.
//!
//! Knows nothing about the server behind it or how that server is started.

mod client;
mod error;
mod framing;
pub mod hints;
mod tokens;
pub mod uri;
pub mod utf16;

pub use client::{Client, ServerEvents};
pub use error::{Error, ResponseError};
pub use lsp_types;
pub use tokens::{SemanticTokensCapability, SemanticTokensSetup};
