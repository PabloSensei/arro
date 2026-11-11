//! Everything that talks to Win32, behind safe types.

mod crash;
mod job;
mod memory;
mod os;
mod path;
mod process;
mod process_stats;
mod shell;
mod trash;
mod waiter;

pub use crash::{install as install_crash_handler, module_of};
pub use job::Job;
pub use memory::{MemoryPressure, MemoryState, MemoryThresholds, physical_memory};
pub use os::version as os_version;
pub use path::CanonPath;
pub use process::{
    ConsoleSignal, NO_WINDOW, SUSPENDED_NO_WINDOW, allow_ctrl_c_in_children, resume_process,
    signal_console_of,
};
pub use process_stats::{ProcessMemory, label_process, process_tree};
pub use shell::{NO_BROWSER_ENV, open_url, recorded_urls};
pub use trash::move_to_trash;
pub use waiter::ProcessWaiter;

/// Tests that use DbgHelp or the shell run one at a time: `MiniDumpWriteDump` of this
/// process hangs when a thread is inside `SHFileOperationW`, so a dump and the Recycle
/// Bin test must not overlap.
#[cfg(test)]
pub(crate) static WIN32_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
