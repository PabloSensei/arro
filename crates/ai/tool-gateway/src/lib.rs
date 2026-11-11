//! The one door from agents into the IDE. Whatever the transport, a call passes the schema, the
//! policy and the audit log here, and only then reaches a service.

mod audit;
mod backend;
mod gateway;
mod language;
mod render;
mod tools;

pub use audit::{AuditEntry, AuditLog};
pub use backend::{
    Confidence, DebugSnapshotSource, DebugStateQuery, Diagnostic, DiagnosticsPage,
    DiagnosticsQuery, EditorContext, Location, Position, SessionScope, Severity, SymbolHit,
    SymbolInfo, SymbolPage, SymbolQuery, ToolBackend, ToolError, Unavailable,
};
pub use gateway::{Decision, Policy, ToolGateway};
pub use language::LanguageTools;
pub use tools::{TOOL_NAMES, ToolClass, ToolSpec};
