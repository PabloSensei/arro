//! The low-memory signal of the whole process. Windows notifications alone fire on the
//! commit charge of the machine, which can be low in absolute terms while plenty of memory
//! is still available; the watcher compares both the available physical memory and the
//! commit room against the thresholds of the settings.

#[cfg(test)]
use std::io;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use tokio::sync::watch;
#[cfg(test)]
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
#[cfg(test)]
use windows_sys::Win32::System::Memory::{
    CreateMemoryResourceNotification, MEMORY_RESOURCE_NOTIFICATION_TYPE,
};
use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

/// How often the free memory is looked at while the process runs.
const POLL: Duration = Duration::from_secs(2);

/// Memory must free beyond this share of the threshold before a low state clears. A
/// machine that hovers at the threshold does not stop and start its servers in a loop.
const HYSTERESIS: f64 = 1.25;

/// When memory counts as low: below both the share of the total and the absolute floor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MemoryThresholds {
    /// Percent of the total memory, for example 5.
    pub low_percent: f32,
    /// The most free memory that still counts as low, for example 1 GiB.
    pub low_bytes: u64,
}

impl Default for MemoryThresholds {
    fn default() -> Self {
        Self {
            low_percent: 5.,
            low_bytes: 1024 * 1024 * 1024,
        }
    }
}

impl MemoryThresholds {
    /// Clamps what the settings file may hold, so a broken value cannot stop everything.
    pub fn new(low_percent: f32, low_megabytes: u32) -> Self {
        let low_percent = if low_percent.is_finite() {
            low_percent.clamp(0., 100.)
        } else {
            Self::default().low_percent
        };
        Self {
            low_percent,
            low_bytes: u64::from(low_megabytes).saturating_mul(1024 * 1024),
        }
    }
}

/// What Windows reports about the memory of the machine at one moment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemoryState {
    /// Physical memory that programs can still ask for.
    pub available_physical: u64,
    pub total_physical: u64,
    /// Commit room that is not used yet: the commit limit counts the page file too.
    pub available_commit: u64,
    pub commit_limit: u64,
}

impl MemoryState {
    /// Low only when both the available memory and the commit room are nearly gone. A
    /// large machine with a busy commit charge but free physical memory keeps its servers.
    pub fn is_low(&self, thresholds: MemoryThresholds) -> bool {
        self.low_state(thresholds, false)
    }

    /// The state after a reading, with hysteresis on the way up: a low state clears only
    /// above [`HYSTERESIS`] times the thresholds, so a machine at the edge stays stable.
    pub fn low_state(&self, thresholds: MemoryThresholds, was_low: bool) -> bool {
        let available = self.available_physical.min(self.available_commit);
        let total = self.total_physical.min(self.commit_limit);
        if total == 0 {
            return false;
        }
        let margin = if was_low { HYSTERESIS } else { 1.0 };
        let floor = thresholds.low_bytes as f64 * margin;
        let share = f64::from(thresholds.low_percent) * margin;
        (available as f64) < floor && (available as f64) * 100.0 < (total as f64) * share
    }
}

/// Total physical memory of the machine, or 0 when Windows cannot answer. A server
/// that sizes its heap for the machine reads this once at start.
pub fn physical_memory() -> u64 {
    state().map_or(0, |state| state.total_physical)
}

/// Reads the current numbers, or `None` when Windows cannot answer.
fn state() -> Option<MemoryState> {
    let mut status = MEMORYSTATUSEX {
        dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
        ..unsafe { std::mem::zeroed() }
    };
    if unsafe { GlobalMemoryStatusEx(&mut status) } == 0 {
        return None;
    }
    Some(MemoryState {
        available_physical: status.ullAvailPhys,
        total_physical: status.ullTotalPhys,
        available_commit: status.ullAvailPageFile,
        commit_limit: status.ullTotalPageFile,
    })
}

/// One memory notification. It stays set while its condition holds, so a wait returns
/// at once until the condition clears.
#[cfg(test)]
struct Notification(HANDLE);

// The handle is only waited on or closed, both of which are safe from any thread.
#[cfg(test)]
unsafe impl Send for Notification {}

#[cfg(test)]
impl Notification {
    fn create(kind: MEMORY_RESOURCE_NOTIFICATION_TYPE) -> io::Result<Self> {
        let handle = unsafe { CreateMemoryResourceNotification(kind) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(handle))
    }
}

#[cfg(test)]
impl Drop for Notification {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

/// The low-memory state of the whole process. One thread watches the numbers; every
/// server subscribes to its latest value.
pub struct MemoryPressure {
    low: watch::Receiver<bool>,
    thresholds: Arc<Mutex<MemoryThresholds>>,
}

impl MemoryPressure {
    /// Starts the watcher on the first call and shares it afterwards.
    pub fn get() -> &'static Self {
        static PRESSURE: OnceLock<MemoryPressure> = OnceLock::new();
        PRESSURE.get_or_init(MemoryPressure::start)
    }

    /// Changes the thresholds of the settings for the whole process.
    pub fn configure(thresholds: MemoryThresholds) {
        let pressure = Self::get();
        *pressure
            .thresholds
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = thresholds;
    }

    /// A subscription that starts with the current state and changes with it.
    pub fn watch(&self) -> watch::Receiver<bool> {
        self.low.clone()
    }

    fn start() -> Self {
        let thresholds = Arc::new(Mutex::new(MemoryThresholds::default()));
        let initial = state()
            .map(|state| state.is_low(MemoryThresholds::default()))
            .unwrap_or(false);
        let (sender, receiver) = watch::channel(initial);
        let watched = thresholds.clone();
        let _ = thread::Builder::new()
            .name("memory-pressure".to_owned())
            .spawn(move || observe(watched, initial, sender));
        Self {
            low: receiver,
            thresholds,
        }
    }
}

/// Looks at the numbers often enough to stop a server before the machine starts paging.
fn observe(thresholds: Arc<Mutex<MemoryThresholds>>, mut low: bool, sender: watch::Sender<bool>) {
    loop {
        thread::sleep(POLL);
        let limits = *thresholds
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let next = state()
            .map(|state| state.low_state(limits, low))
            .unwrap_or(low);
        if next != low {
            low = next;
            // A subscriber that is gone must not end the watcher for the next one.
            let _ = sender.send(low);
        }
    }
}

#[cfg(test)]
mod tests {
    use windows_sys::Win32::System::Memory::{
        HighMemoryResourceNotification, LowMemoryResourceNotification,
    };

    use super::{MemoryPressure, MemoryState, MemoryThresholds, Notification};

    fn gigabytes(count: u64) -> u64 {
        count * 1024 * 1024 * 1024
    }

    #[test]
    fn the_memory_notifications_open() {
        assert!(Notification::create(LowMemoryResourceNotification).is_ok());
        assert!(Notification::create(HighMemoryResourceNotification).is_ok());
    }

    #[test]
    fn every_subscriber_sees_the_same_state() {
        let first = MemoryPressure::get().watch();
        let second = MemoryPressure::get().watch();
        assert_eq!(*first.borrow(), *second.borrow());
    }

    #[test]
    fn memory_is_low_only_when_both_the_share_and_the_floor_are_gone() {
        let thresholds = MemoryThresholds::default();
        let machine = |available: u64| MemoryState {
            available_physical: available,
            total_physical: gigabytes(32),
            available_commit: gigabytes(64),
            commit_limit: gigabytes(128),
        };

        // 1.2 GB of 32 GB is below 5 percent, but above the 1 GB floor: not a threat.
        assert!(!machine(gigabytes(1) + gigabytes(2) / 10).is_low(thresholds));
        // 0.8 GB is below both.
        assert!(machine(gigabytes(1) - gigabytes(2) / 10).is_low(thresholds));
        // Plenty of physical memory with the commit nearly gone is still a threat.
        let commit = MemoryState {
            available_physical: gigabytes(8),
            total_physical: gigabytes(32),
            available_commit: gigabytes(1) / 2,
            commit_limit: gigabytes(16),
        };
        assert!(commit.is_low(thresholds));
        // Above the share, little is at stake even below the floor.
        let plenty = MemoryState {
            available_physical: gigabytes(4),
            total_physical: gigabytes(32),
            available_commit: gigabytes(8),
            commit_limit: gigabytes(16),
        };
        assert!(!plenty.is_low(thresholds));
    }

    #[test]
    fn a_low_state_clears_only_above_the_hysteresis_band() {
        let thresholds = MemoryThresholds::new(5., 512);
        let machine = |available: u64| MemoryState {
            available_physical: available,
            total_physical: gigabytes(32),
            available_commit: gigabytes(64),
            commit_limit: gigabytes(128),
        };
        let megabytes = |count: u64| count * 1024 * 1024;

        // Below the 512 MB floor and below 5 percent: enters the low state.
        assert!(machine(megabytes(400)).low_state(thresholds, false));
        // The same reading would clear without hysteresis, but the band keeps it low.
        assert!(machine(megabytes(600)).low_state(thresholds, true));
        // Above the band the state clears again.
        assert!(!machine(megabytes(650)).low_state(thresholds, true));
        // 1.2 GB free of 32 GB is not a threat.
        assert!(!machine(gigabytes(1) + gigabytes(2) / 10).low_state(thresholds, false));
    }
}
