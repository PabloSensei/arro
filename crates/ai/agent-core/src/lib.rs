//! What the UI knows about agents. A runtime starts sessions, a session takes prompts and streams
//! events, and nothing here says which protocol or harness sits behind them.

mod harness;
mod mock;
mod runtime;
mod store;

pub use harness::{Harness, Installed, detect};
pub use mock::{MockRuntime, MockStep, ToolInvoker};
pub use runtime::{
    AgentError, AgentEvent, AgentRuntime, AgentSession, PermissionReply, Role, RuntimeCaps,
    SessionSpec, SessionStatus, Surface, SurfaceKind, ToolEndpoint,
};
pub use store::{SessionRecord, SessionStore};
