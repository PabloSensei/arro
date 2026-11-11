// Modified for Arro by Paweł Januszko, 2026: frame demand, so the vsync thread sleeps while nothing changes.
// See NOTICE-ARRO.md in the root of this crate.
use std::{
    sync::{Condvar, LazyLock, Mutex},
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use gpui::FrameRequestSource;
use gpui_util::ResultExt;
use windows::Win32::{
    Foundation::HWND,
    Graphics::DirectComposition::DCompositionWaitForCompositorClock,
    Graphics::Dwm::{DWM_TIMING_INFO, DwmGetCompositionTimingInfo},
    System::Performance::QueryPerformanceFrequency,
};

static QPC_TICKS_PER_SECOND: LazyLock<u64> = LazyLock::new(|| {
    let mut frequency = 0;
    // On systems that run Windows XP or later, the function will always succeed and
    // will thus never return zero.
    unsafe { QueryPerformanceFrequency(&mut frequency).unwrap() };
    frequency as u64
});

const VSYNC_INTERVAL_THRESHOLD: Duration = Duration::from_millis(1);
const DEFAULT_VSYNC_INTERVAL: Duration = Duration::from_micros(16_666); // ~60Hz

/// How often the sleeping vsync thread still checks for a lost GPU device.
pub(crate) const DEVICE_CHECK_INTERVAL: Duration = Duration::from_millis(250);
/// Vblanks the vsync thread keeps ticking after the last request, so that a run of
/// frames does not put it to sleep and wake it again between every two of them.
pub(crate) const FRAME_LINGER_TICKS: u32 = 10;

/// Set by anything that needs a frame. The vsync thread sleeps while nobody does.
#[derive(Default)]
pub(crate) struct FrameDemand {
    requested: Mutex<bool>,
    changed: Condvar,
}

impl FrameDemand {
    pub(crate) fn request(&self) {
        let mut requested = self.requested.lock().unwrap();
        if !*requested {
            *requested = true;
            self.changed.notify_one();
        }
    }

    /// Clears the request and says whether there was one.
    pub(crate) fn take(&self) -> bool {
        std::mem::take(&mut *self.requested.lock().unwrap())
    }

    /// Like `take`, but waits up to `timeout` for a request to arrive.
    pub(crate) fn wait(&self, timeout: Duration) -> bool {
        let requested = self.requested.lock().unwrap();
        let (mut requested, _) = self
            .changed
            .wait_timeout_while(requested, timeout, |requested| !*requested)
            .unwrap();
        std::mem::take(&mut *requested)
    }
}

pub(crate) struct VSyncProvider {
    interval: Duration,
    f: Box<dyn Fn() -> bool>,
}

impl VSyncProvider {
    pub(crate) fn new() -> Self {
        let interval = get_dwm_interval()
            .context("Failed to get DWM interval")
            .log_err()
            .unwrap_or(DEFAULT_VSYNC_INTERVAL);
        // DwmFlush can block for more than a second. Two periods bound this wait,
        // also when the compositor has nothing to do.
        let timeout_ms = (interval.as_millis() as u32 * 2).max(4);
        let f: Box<dyn Fn() -> bool> = Box::new(move || unsafe {
            DCompositionWaitForCompositorClock(None, timeout_ms);
            true
        });
        Self { interval, f }
    }

    pub(crate) fn wait_for_vsync(&self) -> FrameRequestSource {
        let vsync_start = Instant::now();
        let wait_succeeded = (self.f)();
        let elapsed = vsync_start.elapsed();
        // DwmFlush and DCompositionWaitForCompositorClock returns very early
        // instead of waiting until vblank when the monitor goes to sleep or is
        // unplugged (nothing to present due to desktop occlusion). We use 1ms as
        // a threshold for the duration of the wait functions and fallback to
        // Sleep() if it returns before that. This could happen during normal
        // operation for the first call after the vsync thread becomes non-idle,
        // but it shouldn't happen often.
        if !wait_succeeded || elapsed < VSYNC_INTERVAL_THRESHOLD {
            log::trace!("VSyncProvider::wait_for_vsync() took less time than expected");
            std::thread::sleep(self.interval);
            FrameRequestSource::LocalSchedule
        } else {
            FrameRequestSource::NativeCallback
        }
    }
}

fn get_dwm_interval() -> Result<Duration> {
    let mut timing_info = DWM_TIMING_INFO {
        cbSize: std::mem::size_of::<DWM_TIMING_INFO>() as u32,
        ..Default::default()
    };
    unsafe { DwmGetCompositionTimingInfo(HWND::default(), &mut timing_info) }?;
    let interval = retrieve_duration(timing_info.qpcRefreshPeriod, *QPC_TICKS_PER_SECOND);
    // Check for interval values that are impossibly low. A 29 microsecond
    // interval was seen (from a qpcRefreshPeriod of 60).
    if interval < VSYNC_INTERVAL_THRESHOLD {
        Ok(retrieve_duration(
            timing_info.rateRefresh.uiDenominator as u64,
            timing_info.rateRefresh.uiNumerator as u64,
        ))
    } else {
        Ok(interval)
    }
}

#[inline]
fn retrieve_duration(counts: u64, ticks_per_second: u64) -> Duration {
    let ticks_per_microsecond = ticks_per_second / 1_000_000;
    Duration::from_micros(counts / ticks_per_microsecond)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_made_before_the_wait_is_not_lost() {
        let demand = FrameDemand::default();
        demand.request();
        assert!(demand.wait(Duration::ZERO));
        assert!(!demand.wait(Duration::ZERO));
    }

    #[test]
    fn take_consumes_the_request() {
        let demand = FrameDemand::default();
        demand.request();
        assert!(demand.take());
        assert!(!demand.take());
    }

    #[test]
    fn successful_compositor_wait_reports_native_callback() {
        let provider = VSyncProvider {
            interval: Duration::ZERO,
            f: Box::new(|| {
                std::thread::sleep(VSYNC_INTERVAL_THRESHOLD * 2);
                true
            }),
        };

        assert_eq!(
            provider.wait_for_vsync(),
            FrameRequestSource::NativeCallback
        );
    }

    #[test]
    fn failed_compositor_wait_reports_local_schedule_even_after_threshold() {
        let provider = VSyncProvider {
            interval: Duration::ZERO,
            f: Box::new(|| {
                std::thread::sleep(VSYNC_INTERVAL_THRESHOLD * 2);
                false
            }),
        };

        assert_eq!(provider.wait_for_vsync(), FrameRequestSource::LocalSchedule);
    }
}
