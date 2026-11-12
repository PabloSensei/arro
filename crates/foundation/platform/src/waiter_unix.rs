//! Waits for a process to end without trusting its id to stay its own. Linux opens a
//! pidfd, which names the process itself; other systems poll `kill(pid, 0)`.

use std::io;
use std::time::{Duration, Instant};

/// A process opened for waiting.
pub struct ProcessWaiter {
    id: u32,
    #[cfg(target_os = "linux")]
    pidfd: Option<std::os::fd::OwnedFd>,
}

impl ProcessWaiter {
    /// Opens a process for waiting. `None` when it has already ended.
    pub fn open(process_id: u32) -> io::Result<Option<Self>> {
        // SAFETY: signal 0 checks the process without touching it.
        let alive = unsafe { libc::kill(process_id as i32, 0) } == 0;
        if !alive {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ESRCH) {
                return Ok(None);
            }
            // A process of another user answers EPERM and is very much alive.
            if error.raw_os_error() != Some(libc::EPERM) {
                return Err(error);
            }
        }
        Ok(Some(Self {
            id: process_id,
            #[cfg(target_os = "linux")]
            pidfd: open_pidfd(process_id),
        }))
    }

    pub fn id(&self) -> u32 {
        self.id
    }

    /// Whether the process ended within `timeout`. A process that has already ended
    /// answers at once.
    pub fn wait(&self, timeout: Duration) -> io::Result<bool> {
        #[cfg(target_os = "linux")]
        if let Some(pidfd) = &self.pidfd {
            return wait_pidfd(pidfd, timeout);
        }
        let deadline = Instant::now() + timeout;
        loop {
            // SAFETY: signal 0 checks the process without touching it.
            if unsafe { libc::kill(self.id as i32, 0) } != 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::ESRCH) {
                    return Ok(true);
                }
                return Err(error);
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(false);
            }
            std::thread::sleep((deadline - now).min(Duration::from_millis(10)));
        }
    }
}

#[cfg(target_os = "linux")]
fn open_pidfd(process_id: u32) -> Option<std::os::fd::OwnedFd> {
    // SAFETY: a plain syscall with plain values; a non-negative result is an owned descriptor.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, process_id as libc::c_long, 0) };
    if fd < 0 {
        return None;
    }
    // SAFETY: the descriptor is new and nothing else owns it.
    Some(unsafe { std::os::fd::FromRawFd::from_raw_fd(fd as i32) })
}

#[cfg(target_os = "linux")]
fn wait_pidfd(pidfd: &std::os::fd::OwnedFd, timeout: Duration) -> io::Result<bool> {
    use std::os::fd::AsRawFd;

    let millis = timeout.as_millis().min(i32::MAX as u128) as i32;
    let mut descriptor = libc::pollfd {
        fd: pidfd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one descriptor with its events, valid for the call.
    let ready = unsafe { libc::poll(&mut descriptor, 1, millis) };
    match ready {
        0 => Ok(false),
        1.. => Ok(true),
        _ => {
            let error = io::Error::last_os_error();
            // An interrupted sleep is not an answer.
            if error.raw_os_error() == Some(libc::EINTR) {
                return Ok(false);
            }
            Err(error)
        }
    }
}
