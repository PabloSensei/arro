//! Console signals, creation flags and Ctrl+C inheritance. Unix has real signals, so a
//! child is asked directly and the helper of Windows is not needed.

use std::io;

/// There is no window to hide: every child starts without one.
pub const NO_WINDOW: u32 = 0;

/// Unix has no suspended start that a job needs.
pub const SUSPENDED_NO_WINDOW: u32 = 0;

/// A child inherits the signal dispositions of its parent; nothing has to be undone.
pub fn allow_ctrl_c_in_children() -> io::Result<()> {
    Ok(())
}

/// Windows resumes a child created suspended; Unix starts it running.
pub fn resume_process(_process_id: u32) -> io::Result<()> {
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConsoleSignal {
    CtrlC,
    /// A JVM answers with a thread dump on its stdout and keeps running.
    CtrlBreak,
}

/// Sends `Ctrl+C` or `Ctrl+Break` to the process group of `process_id`. This takes over
/// the process group of the caller on Windows; on Unix it only sends a signal.
pub fn signal_console_of(process_id: u32, signal: ConsoleSignal) -> io::Result<()> {
    let signal = match signal {
        ConsoleSignal::CtrlC => libc::SIGINT,
        ConsoleSignal::CtrlBreak => libc::SIGQUIT,
    };
    // SAFETY: a negative pid names the process group, plain values.
    let sent = unsafe { libc::kill(-(process_id as i32), signal) };
    if sent == 0 {
        return Ok(());
    }
    // SAFETY: the same call without the group, for a process that leads none.
    let sent = unsafe { libc::kill(process_id as i32, signal) };
    if sent == 0 {
        return Ok(());
    }
    Err(io::Error::last_os_error())
}
