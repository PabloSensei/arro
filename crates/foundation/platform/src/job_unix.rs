//! A process group that dies together: every child starts in a group of its own and the
//! group is signalled as a whole, so a grandchild cannot outlive its owner.

use std::io;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long [`Job::terminate`] waits for the last process of the group to die.
const GONE_TIMEOUT: Duration = Duration::from_secs(2);
/// The pause between two looks at the group.
const GONE_POLL: Duration = Duration::from_millis(10);

/// A process group started for one child. Dropping the job ends the group, which is what
/// closing the job object does on Windows.
pub struct Job {
    group: Mutex<Option<i32>>,
}

impl Job {
    pub fn new() -> io::Result<Self> {
        Ok(Self {
            group: Mutex::new(None),
        })
    }

    /// Records the group of a child started with [`std::os::unix::process::CommandExt::process_group`].
    pub fn assign(&self, process_id: u32) -> io::Result<()> {
        *self.group() = Some(process_id as i32);
        Ok(())
    }

    /// Ends every process in the group. The signal is delivered asynchronously, so the
    /// call waits until nothing alive is left: a caller that looks at [`Job::process_ids`]
    /// right after it sees an empty group, as it does after a job object is ended.
    pub fn terminate(&self, _exit_code: u32) -> io::Result<()> {
        let group = *self.group();
        let Some(group) = group else {
            return Ok(());
        };
        // SAFETY: a negative pid names the process group, a plain value.
        let sent = unsafe { libc::kill(-group, libc::SIGKILL) };
        if sent != 0 {
            let error = io::Error::last_os_error();
            // A group that is gone already is what was asked for.
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error);
            }
        }
        let deadline = Instant::now() + GONE_TIMEOUT;
        while !self.process_ids()?.is_empty() && Instant::now() < deadline {
            std::thread::sleep(GONE_POLL);
        }
        Ok(())
    }

    /// Ids of the processes alive in the group.
    pub fn process_ids(&self) -> io::Result<Vec<u32>> {
        let group = *self.group();
        let Some(group) = group else {
            return Ok(Vec::new());
        };
        let mut ids = Vec::new();
        let entries = std::fs::read_dir("/proc")?;
        for entry in entries.flatten() {
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<u32>().ok())
            else {
                continue;
            };
            let Some(process_group) = crate::process_stats::process_group(pid) else {
                continue;
            };
            if process_group == group {
                ids.push(pid);
            }
        }
        Ok(ids)
    }

    fn group(&self) -> std::sync::MutexGuard<'_, Option<i32>> {
        self.group
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        let _ = self.terminate(1);
    }
}
