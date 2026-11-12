//! The low-memory signal of the whole process. Linux reports the numbers in
//! `/proc/meminfo`; the watcher compares both the available physical memory and the
//! commit room against the thresholds of the settings.

use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use tokio::sync::watch;

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

/// What the system reports about the memory of the machine at one moment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemoryState {
    /// Physical memory that programs can still ask for.
    pub available_physical: u64,
    pub total_physical: u64,
    /// Commit room that is not used yet. Linux counts the swap area in the limit.
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

/// Total physical memory of the machine, or 0 when the system cannot answer.
pub fn physical_memory() -> u64 {
    state().map_or(0, |state| state.total_physical)
}

/// Reads the current numbers, or `None` when `/proc/meminfo` cannot be read.
fn state() -> Option<MemoryState> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    parse_meminfo(&text)
}

/// The fields of `/proc/meminfo`, in bytes. A missing commit limit means the kernel does
/// not account it, so the physical numbers alone decide.
fn parse_meminfo(text: &str) -> Option<MemoryState> {
    let mut total = None;
    let mut available = None;
    let mut commit_limit = None;
    let mut committed = None;
    for line in text.lines() {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let mut fields = value.split_whitespace();
        let Some(number) = fields.next().and_then(|number| number.parse::<u64>().ok()) else {
            continue;
        };
        let bytes = match fields.next() {
            Some("kB") => number.saturating_mul(1024),
            Some(_) => continue,
            None => number,
        };
        match name {
            "MemTotal" => total = Some(bytes),
            "MemAvailable" => available = Some(bytes),
            "CommitLimit" => commit_limit = Some(bytes),
            "Committed_AS" => committed = Some(bytes),
            _ => {}
        }
    }
    let total_physical = total?;
    let commit_limit = commit_limit.unwrap_or(total_physical);
    let available_commit = committed.map_or(commit_limit, |used| commit_limit.saturating_sub(used));
    Some(MemoryState {
        available_physical: available.unwrap_or(0),
        total_physical,
        available_commit,
        commit_limit,
    })
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
    use super::{MemoryPressure, MemoryState, MemoryThresholds, parse_meminfo};

    fn gigabytes(count: u64) -> u64 {
        count * 1024 * 1024 * 1024
    }

    #[test]
    fn meminfo_is_read_in_bytes() {
        let text = "MemTotal:       16384000 kB\n\
                    MemFree:         1000000 kB\n\
                    MemAvailable:    8192000 kB\n\
                    CommitLimit:    32768000 kB\n\
                    Committed_AS:    4096000 kB\n";
        let state = parse_meminfo(text).unwrap();

        assert_eq!(state.total_physical, 16384000 * 1024);
        assert_eq!(state.available_physical, 8192000 * 1024);
        assert_eq!(state.commit_limit, 32768000 * 1024);
        assert_eq!(state.available_commit, (32768000 - 4096000) * 1024);
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

        assert!(!machine(gigabytes(1) + gigabytes(2) / 10).is_low(thresholds));
        assert!(machine(gigabytes(1) - gigabytes(2) / 10).is_low(thresholds));
        let commit = MemoryState {
            available_physical: gigabytes(8),
            total_physical: gigabytes(32),
            available_commit: gigabytes(1) / 2,
            commit_limit: gigabytes(16),
        };
        assert!(commit.is_low(thresholds));
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

        assert!(machine(megabytes(400)).low_state(thresholds, false));
        assert!(machine(megabytes(600)).low_state(thresholds, true));
        assert!(!machine(megabytes(650)).low_state(thresholds, true));
        assert!(!machine(gigabytes(1) + gigabytes(2) / 10).low_state(thresholds, false));
    }
}
