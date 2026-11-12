//! Everything that talks to the operating system, behind safe types.

#[cfg(windows)]
mod crash;
#[cfg(unix)]
#[path = "crash_unix.rs"]
mod crash;
#[cfg(windows)]
mod job;
#[cfg(unix)]
#[path = "job_unix.rs"]
mod job;
#[cfg(windows)]
mod memory;
#[cfg(unix)]
#[path = "memory_unix.rs"]
mod memory;
#[cfg(windows)]
mod os;
#[cfg(unix)]
#[path = "os_unix.rs"]
mod os;
#[cfg(windows)]
mod path;
#[cfg(unix)]
#[path = "path_unix.rs"]
mod path;
#[cfg(windows)]
mod process;
#[cfg(unix)]
#[path = "process_unix.rs"]
mod process;
#[cfg(windows)]
mod process_stats;
#[cfg(unix)]
#[path = "process_stats_unix.rs"]
mod process_stats;
#[cfg(windows)]
mod shell;
#[cfg(unix)]
#[path = "shell_unix.rs"]
mod shell;
mod trash;
#[cfg(windows)]
mod waiter;
#[cfg(unix)]
#[path = "waiter_unix.rs"]
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
#[cfg(all(test, windows))]
pub(crate) static WIN32_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
