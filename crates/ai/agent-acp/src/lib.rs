//! Agents over the Agent Client Protocol: a process that speaks JSON-RPC on stdio, and its
//! session as a transcript. The client is our own, the v1 subset a turn needs; see
//! `docs/ARCHITECTURE.md` section 7 and spike S7.

mod connection;
mod preset;
mod session;

pub use preset::{AcpCommand, Preset};
pub use session::AcpRuntime;
