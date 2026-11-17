use std::io;
use std::os::windows::io::{AsHandle, AsRawHandle, RawHandle};
use std::thread;
use std::time::Duration;

use windows_sys::Win32::System::Console::{
    AttachConsole, CTRL_BREAK_EVENT, CTRL_C_EVENT, FreeConsole, GenerateConsoleCtrlEvent,
    SetConsoleCtrlHandler,
};
use windows_sys::Win32::System::Threading::{CREATE_NO_WINDOW, CREATE_SUSPENDED};

/// Creation flags for a child with a console of its own that nobody sees. The console is
/// what lets [`signal_console_of`] reach it and nobody else.
pub const NO_WINDOW: u32 = CREATE_NO_WINDOW;

/// [`NO_WINDOW`], and the child waits for [`resume_process`].
pub const SUSPENDED_NO_WINDOW: u32 = CREATE_SUSPENDED | CREATE_NO_WINDOW;

/// How long the helper stays attached, so that the event reaches the console first.
const DELIVERY: Duration = Duration::from_millis(200);

#[link(name = "ntdll")]
unsafe extern "system" {
    /// Not documented, but stable since Windows XP. `std` does not hand out the main
    /// thread of a child, and walking all threads of the system costs milliseconds.
    fn NtResumeProcess(process: RawHandle) -> i32;
}

/// Starts a child created with [`SUSPENDED_NO_WINDOW`].
pub fn resume_process(process: impl AsHandle) -> io::Result<()> {
    // SAFETY: the handle is valid for the call.
    let status = unsafe { NtResumeProcess(process.as_handle().as_raw_handle()) };
    if status < 0 {
        return Err(io::Error::other(format!("NtResumeProcess: {status:#x}")));
    }
    Ok(())
}

/// A child inherits ignoring Ctrl+C from its parent, and a JVM then never stops on it.
pub fn allow_ctrl_c_in_children() -> io::Result<()> {
    // SAFETY: a null handler is allowed and touches only this process's console state.
    if unsafe { SetConsoleCtrlHandler(None, 0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConsoleSignal {
    CtrlC,
    /// A JVM answers with a thread dump on its stdout and keeps running.
    CtrlBreak,
}

unsafe extern "system" fn survive(_event: u32) -> i32 {
    1
}

/// Sends a signal to everything on the console of `process_id`. This takes over the console
/// of the calling process, so only `ide-ctrl`, which lives for this one call, may use it.
pub fn signal_console_of(process_id: u32, signal: ConsoleSignal) -> io::Result<()> {
    let event = match signal {
        ConsoleSignal::CtrlC => CTRL_C_EVENT,
        ConsoleSignal::CtrlBreak => CTRL_BREAK_EVENT,
    };
    // SAFETY: plain calls, the handler is a function that lives as long as the process.
    unsafe {
        FreeConsole();
        if AttachConsole(process_id) == 0 {
            return Err(io::Error::last_os_error());
        }
        // The event reaches this process too. A null handler would cover only Ctrl+C.
        SetConsoleCtrlHandler(Some(survive), 1);
        let sent = GenerateConsoleCtrlEvent(event, 0);
        let error = io::Error::last_os_error();
        thread::sleep(DELIVERY);
        FreeConsole();
        if sent == 0 {
            return Err(error);
        }
    }
    Ok(())
}
