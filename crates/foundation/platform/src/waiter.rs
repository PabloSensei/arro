//! Waits for a process to end without trusting its id to stay its own.

use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::time::Duration;

use windows_sys::Win32::Foundation::{
    ERROR_INVALID_PARAMETER, GetLastError, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
};

/// A process opened for waiting. The handle names the process itself, so a new
/// process that takes the same id does not make the wait return early or late.
pub struct ProcessWaiter {
    id: u32,
    handle: OwnedHandle,
}

impl ProcessWaiter {
    /// Opens a process for waiting. `None` when it has already ended.
    pub fn open(process_id: u32) -> io::Result<Option<Self>> {
        // SAFETY: the parameters are plain values, the result is checked.
        let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, process_id) };
        if handle.is_null() {
            // SAFETY: a thread-local read right after the call.
            return if unsafe { GetLastError() } == ERROR_INVALID_PARAMETER {
                Ok(None)
            } else {
                Err(io::Error::last_os_error())
            };
        }
        // SAFETY: a non-null result is ours to own.
        let handle = unsafe { OwnedHandle::from_raw_handle(handle) };
        Ok(Some(Self {
            id: process_id,
            handle,
        }))
    }

    pub fn id(&self) -> u32 {
        self.id
    }

    /// Whether the process ended within `timeout`. A process that has already ended
    /// answers at once.
    pub fn wait(&self, timeout: Duration) -> io::Result<bool> {
        let millis = timeout.as_millis().min(u32::MAX as u128) as u32;
        // SAFETY: the handle is valid for the lifetime of `self`.
        match unsafe { WaitForSingleObject(self.handle.as_raw_handle(), millis) } {
            WAIT_OBJECT_0 => Ok(true),
            WAIT_TIMEOUT => Ok(false),
            _ => Err(io::Error::last_os_error()),
        }
    }
}
