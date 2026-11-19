//! Processes of the machine that already listen for a debugger, for the window that
//! offers them by name rather than by a typed address.

use crate::Provider;

/// One process a debugger may attach to. The language names it; the address is where
/// the debug agent listens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DebugProcess {
    pub pid: u32,
    /// What the process runs, as the language names it: a main class for one.
    pub name: String,
    pub host: String,
    pub port: u16,
}

/// The processes that may be attached to. A language that cannot tell keeps the
/// capability out, and the window asks for an address instead.
pub type DebugProcessProvider = dyn Provider<(), Vec<DebugProcess>>;
