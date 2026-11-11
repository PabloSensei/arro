//! Keeps one language server alive for one project.
//!
//! Base form of M5a: start for a trusted project, stop, restart after a crash.
//! What the server is comes from [`ServerSpec`], how it is spawned from [`Launcher`].

mod process;
mod supervisor;

pub use process::{Launcher, Plan, ProcessHandle, ServerProcess, SupervisedLauncher, WaitFuture};
pub use supervisor::{Limits, NotTrusted, ReloadHandle, ServerSpec, Sidecar, State, Status, Trust};
