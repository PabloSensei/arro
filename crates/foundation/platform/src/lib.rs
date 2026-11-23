//! Everything that talks to the operating system, behind safe types.

#[cfg(windows)]
mod crash;
#[cfg(unix)]
#[path = "crash_unix.rs"]
mod crash;
/// One program per state folder: a second start hands its path to the first one.
#[cfg(any(unix, windows))]
pub mod instance;
#[cfg(windows)]
mod job;
#[cfg(unix)]
#[path = "job_unix.rs"]
mod job;
mod machine;
#[cfg(windows)]
mod memory;
#[cfg(unix)]
#[path = "memory_unix.rs"]
mod memory;
mod mode;
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
mod process_tree;
#[cfg(unix)]
#[path = "process_tree_unix.rs"]
mod process_tree;
mod resources;
/// Passwords kept by the system: Credential Manager, Keychain or Secret Service.
pub mod secrets;
#[cfg(windows)]
mod shell;
#[cfg(unix)]
#[path = "shell_unix.rs"]
mod shell;
#[cfg(windows)]
mod spelling;
#[cfg(target_os = "macos")]
#[path = "spelling_macos.rs"]
mod spelling;
#[cfg(all(unix, not(target_os = "macos")))]
#[path = "spelling_unix.rs"]
mod spelling;
/// The thread of the system spell checker, which the platforms that have one share.
/// A system without a checker keeps it for the tests of the queue alone.
#[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
mod spelling_service;
mod trash;
mod url;
#[cfg(windows)]
mod waiter;
#[cfg(unix)]
#[path = "waiter_unix.rs"]
mod waiter;

pub use crash::{install as install_crash_handler, module_of};
pub use job::Job;
pub use machine::{bios_names_a_virtual_machine, is_virtual_machine};
pub use memory::{LOW_MEMORY_HOLD, MemoryPressure, MemoryState, MemoryThresholds, physical_memory};
pub use mode::{make_executable, mode, private_file, set_mode};
pub use os::version as os_version;
pub use path::{CanonPath, file_url_path, path_from_uri, path_key};
pub use process::{
    ConsoleSignal, NO_WINDOW, SUSPENDED_NO_WINDOW, allow_ctrl_c_in_children, resume_process,
    signal_console_of,
};
#[cfg(windows)]
pub use process_stats::{
    ProcessEntry, is_console_process, process_created, process_table, terminate_process,
};
pub use process_stats::{ProcessMemory, label_process, parent_process_id, process_tree};
pub use process_tree::ProcessTree;
pub use resources::{resources_dir, resources_dir_of};
pub use shell::{NO_BROWSER_ENV, open_url, recorded_urls};
pub use spelling::{spelling_check, spelling_source, spelling_suggest, spelling_supported};
pub use trash::move_to_trash;
pub use waiter::ProcessWaiter;

/// Tests that use DbgHelp or the shell run one at a time: `MiniDumpWriteDump` of this
/// process hangs when a thread is inside `SHFileOperationW`, so a dump and the Recycle
/// Bin test must not overlap.
#[cfg(all(test, windows))]
pub(crate) static WIN32_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
