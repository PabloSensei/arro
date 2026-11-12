//! What every crate shares: typed ids, cancellation, snapshots and the error type.

pub mod app;
mod cancel;
mod error;
mod id;
pub mod perf;
pub mod program;
mod snapshot;

pub use cancel::{Cancel, Cancelled};
pub use error::{Error, Result};
pub use id::{Id, IdSource};
pub use snapshot::Snapshot;
