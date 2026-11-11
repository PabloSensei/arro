//! Named latency histograms and memory gauges, cheap on the hot path.
//!
//! A sample is one atomic store into a fixed ring, so recording never waits for a lock.
//! The time of the sample is packed next to its length, which lets a reader keep only
//! the last N minutes without a second buffer. Each metric keeps the total count of
//! events since the process started, apart from the window of samples.
//!
//! The registry only takes a lock when a metric is seen for the first time on a thread,
//! and when a reader takes a snapshot. Memory gauges are set by their owners and read
//! with the same snapshot.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Samples one metric keeps. A window with more events keeps its newest ones.
const CAPACITY: usize = 4096;
/// Durations above this many microseconds, about 71 minutes, are recorded as this value.
/// Microseconds rather than nanoseconds, so a scan or a slow server answer of more
/// than four seconds keeps its length.
const MAX_MICROS: u64 = u32::MAX as u64;

/// One latency metric as the panel reads it. Times are for the events of the window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetricSnapshot {
    pub name: &'static str,
    /// Events since the process started, not only those in the window.
    pub count: u64,
    pub p50: Option<Duration>,
    pub p95: Option<Duration>,
    pub p99: Option<Duration>,
}

/// A memory value its owner records, such as the bytes of the symbol index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemorySnapshot {
    pub name: &'static str,
    pub bytes: u64,
}

struct Histogram {
    name: &'static str,
    total: AtomicU64,
    cursor: AtomicU64,
    /// `(seconds since the origin) << 32 | microseconds`, zero while untouched.
    samples: Box<[AtomicU64]>,
}

impl Histogram {
    fn new(name: &'static str) -> Self {
        let samples = (0..CAPACITY).map(|_| AtomicU64::new(0)).collect();
        Self {
            name,
            total: AtomicU64::new(0),
            cursor: AtomicU64::new(0),
            samples,
        }
    }

    fn record(&self, elapsed: Duration) {
        self.record_at(now_seconds(), elapsed);
    }

    fn record_at(&self, seconds: u64, elapsed: Duration) {
        let micros = elapsed.as_micros().min(MAX_MICROS as u128) as u64;
        let packed = (seconds.min(u32::MAX as u64) << 32) | micros;
        let index = self.cursor.fetch_add(1, Ordering::Relaxed);
        self.samples[index as usize % CAPACITY].store(packed, Ordering::Relaxed);
        self.total.fetch_add(1, Ordering::Relaxed);
    }

    fn snapshot(&self, window: Duration) -> MetricSnapshot {
        self.snapshot_at(now_seconds(), window)
    }

    fn snapshot_at(&self, now: u64, window: Duration) -> MetricSnapshot {
        let oldest = now.saturating_sub(window.as_secs());
        let mut durations = Vec::with_capacity(CAPACITY);
        for sample in self.samples.iter() {
            let packed = sample.load(Ordering::Relaxed);
            // A zero slot is untouched, or an event at the very start of the process.
            if packed == 0 {
                continue;
            }
            if packed >> 32 < oldest {
                continue;
            }
            durations.push((packed & MAX_MICROS) as u32);
        }
        durations.sort_unstable();
        MetricSnapshot {
            name: self.name,
            count: self.total.load(Ordering::Relaxed),
            p50: percentile(&durations, 50.),
            p95: percentile(&durations, 95.),
            p99: percentile(&durations, 99.),
        }
    }
}

/// The nearest-rank percentile, so one slow event is never hidden by an average.
fn percentile(sorted: &[u32], percent: f64) -> Option<Duration> {
    if sorted.is_empty() {
        return None;
    }
    let rank = (percent / 100. * sorted.len() as f64).ceil() as usize;
    let index = rank.saturating_sub(1).min(sorted.len() - 1);
    Some(Duration::from_micros(sorted[index] as u64))
}

#[derive(Default)]
struct Registry {
    histograms: Vec<&'static Histogram>,
    by_name: HashMap<&'static str, usize>,
    memory: Vec<MemorySnapshot>,
}

static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
static ORIGIN: OnceLock<Instant> = OnceLock::new();

thread_local! {
    /// Every thread finds its metrics once, then reads its own pointers.
    static CACHE: RefCell<Vec<(&'static str, &'static Histogram)>> = const { RefCell::new(Vec::new()) };
}

fn registry() -> &'static Mutex<Registry> {
    REGISTRY.get_or_init(|| Mutex::new(Registry::default()))
}

fn now_seconds() -> u64 {
    ORIGIN.get_or_init(Instant::now).elapsed().as_secs()
}

fn histogram(name: &'static str) -> &'static Histogram {
    if let Some(found) = CACHE.with(|cache| {
        cache
            .borrow()
            .iter()
            .find(|(known, _)| *known == name)
            .map(|(_, histogram)| *histogram)
    }) {
        return found;
    }
    let mut registry = registry()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let index = match registry.by_name.get(name) {
        Some(index) => *index,
        None => {
            let histogram = &*Box::leak(Box::new(Histogram::new(name)));
            let index = registry.histograms.len();
            registry.histograms.push(histogram);
            registry.by_name.insert(name, index);
            index
        }
    };
    let histogram = registry.histograms[index];
    drop(registry);
    CACHE.with(|cache| cache.borrow_mut().push((name, histogram)));
    histogram
}

/// Records one event of `name`, such as `editor.keystroke`.
pub fn observe(name: &'static str, elapsed: Duration) {
    histogram(name).record(elapsed);
}

/// Sets the current size of an in-memory store. One value per name; a later call replaces it.
pub fn set_memory(name: &'static str, bytes: u64) {
    let mut registry = registry()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    match registry.memory.iter_mut().find(|entry| entry.name == name) {
        Some(entry) => entry.bytes = bytes,
        None => registry.memory.push(MemorySnapshot { name, bytes }),
    }
}

/// Every memory gauge, in the order the owners first set them.
pub fn memory() -> Vec<MemorySnapshot> {
    registry()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .memory
        .clone()
}

/// One row per metric, by name, with the events of the last `window`.
pub fn snapshot(window: Duration) -> Vec<MetricSnapshot> {
    let histograms: Vec<&'static Histogram> = registry()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .histograms
        .clone();
    let mut rows: Vec<MetricSnapshot> = histograms
        .into_iter()
        .map(|histogram| histogram.snapshot(window))
        .collect();
    rows.sort_by_key(|row| row.name);
    rows
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{Histogram, MAX_MICROS, now_seconds, percentile};

    #[test]
    fn percentiles_use_the_nearest_rank() {
        let sorted: Vec<u32> = (1..=100).map(|value| value * 1_000).collect();
        assert_eq!(percentile(&sorted, 50.), Some(Duration::from_millis(50)));
        assert_eq!(percentile(&sorted, 95.), Some(Duration::from_millis(95)));
        assert_eq!(percentile(&sorted, 99.), Some(Duration::from_millis(99)));
        assert_eq!(percentile(&[7], 99.), Some(Duration::from_micros(7)));
        assert_eq!(percentile(&[], 50.), None);
    }

    #[test]
    fn the_window_keeps_only_recent_events() {
        let histogram = Histogram::new("test.window");
        let now = 10_000;
        histogram.record_at(now - 600, Duration::from_millis(9));
        histogram.record_at(now - 10, Duration::from_millis(2));
        histogram.record_at(now, Duration::from_millis(4));

        let recent = histogram.snapshot_at(now, Duration::from_secs(300));
        assert_eq!(
            recent.count, 3,
            "the counter is the whole life of the metric"
        );
        assert_eq!(recent.p50, Some(Duration::from_millis(2)));
        assert_eq!(recent.p99, Some(Duration::from_millis(4)));

        let all = histogram.snapshot_at(now, Duration::from_secs(3600));
        assert_eq!(all.p99, Some(Duration::from_millis(9)));
    }

    #[test]
    fn a_long_event_keeps_its_length() {
        let histogram = Histogram::new("test.long");
        histogram.record_at(now_seconds(), Duration::from_secs(12));
        let snapshot = histogram.snapshot(Duration::from_secs(300));
        assert_eq!(snapshot.p99, Some(Duration::from_secs(12)));
    }

    #[test]
    fn the_ring_drops_the_oldest_sample() {
        let histogram = Histogram::new("test.ring");
        histogram.record_at(now_seconds(), Duration::from_micros(1));
        for _ in 0..super::CAPACITY {
            histogram.record_at(now_seconds(), Duration::from_micros(MAX_MICROS));
        }
        let snapshot = histogram.snapshot(Duration::from_secs(3600));
        assert_eq!(snapshot.count, super::CAPACITY as u64 + 1);
        assert_eq!(snapshot.p99, Some(Duration::from_micros(MAX_MICROS)));
    }

    #[test]
    fn threads_write_without_waiting_for_each_other() {
        let histogram = std::sync::Arc::new(Histogram::new("test.threads"));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let histogram = histogram.clone();
                std::thread::spawn(move || {
                    for _ in 0..500 {
                        histogram.record(Duration::from_micros(50));
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        let snapshot = histogram.snapshot(Duration::from_secs(3600));
        assert_eq!(snapshot.count, 8 * 500);
        assert_eq!(snapshot.p50, Some(Duration::from_micros(50)));
        assert_eq!(snapshot.p95, Some(Duration::from_micros(50)));
        assert_eq!(snapshot.p99, Some(Duration::from_micros(50)));
    }

    #[test]
    fn a_gauge_is_replaced_not_added() {
        super::set_memory("test.gauge", 10);
        super::set_memory("test.gauge", 20);
        let found = super::memory()
            .into_iter()
            .find(|entry| entry.name == "test.gauge");
        assert_eq!(found.map(|entry| entry.bytes), Some(20));
    }

    #[test]
    fn an_event_is_counted() {
        super::observe("test.events", Duration::from_millis(3));
        let found = super::snapshot(Duration::from_secs(60))
            .into_iter()
            .find(|row| row.name == "test.events");
        let row = found.expect("the metric is registered");
        assert!(row.count > 0);
        assert_eq!(row.p50, Some(Duration::from_millis(3)));
    }
}
