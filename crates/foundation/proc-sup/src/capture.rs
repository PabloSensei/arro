use std::io::{self, Read};
use std::process::ExitStatus;
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, sleep};
use std::time::{Duration, Instant};

use base::{Cancel, Cancelled};

use crate::ProcessSpec;

/// How much output a captured run keeps: the end is what explains a failure.
const KEPT_BYTES: usize = 1 << 20;
const POLL: Duration = Duration::from_millis(20);
/// How long output may still arrive after the process exited. A server it left behind can
/// hold the pipes open for hours, so the end of a pipe is not worth waiting for.
const DRAIN: Duration = Duration::from_millis(200);
const GRACE: Duration = Duration::from_secs(5);
/// The shortest gap between two looks at the process table while output flows. A quiet
/// run takes none: output is the only event that can bring a new program.
const ADOPT_INTERVAL: Duration = Duration::from_millis(250);

pub struct Captured {
    pub status: ExitStatus,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug)]
pub enum CaptureError {
    Cancelled,
    Io(io::Error),
}

impl std::fmt::Display for CaptureError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Self::Cancelled => Cancelled.fmt(f),
            Self::Io(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for CaptureError {}

impl From<io::Error> for CaptureError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Runs to the end and hands back the output. Blocks, so this belongs on a background
/// thread. On cancellation the process gets Ctrl+C, then force after a grace period.
pub fn capture(spec: &ProcessSpec, cancel: &Cancel) -> Result<Captured, CaptureError> {
    let mut process = spec.start()?;
    drop(process.take_stdin());
    let stdout = keep_tail(process.take_stdout());
    let stderr = keep_tail(process.take_stderr());

    let mut adopted: Option<Instant> = None;
    let status = loop {
        if cancel.is_cancelled() {
            process.shut_down(GRACE)?;
            return Err(CaptureError::Cancelled);
        }
        // Output is the only sign of life a program gives, and the moment a program it
        // started outside the job is worth pulling in.
        if (stdout.take_arrived() | stderr.take_arrived())
            && adopted.is_none_or(|last| last.elapsed() >= ADOPT_INTERVAL)
        {
            process.adopt();
            adopted = Some(Instant::now());
        }
        if let Some(status) = process.try_wait()? {
            break status;
        }
        sleep(POLL);
    };
    let start = Instant::now();
    while start.elapsed() < DRAIN && !(stdout.is_done() && stderr.is_done()) {
        sleep(POLL);
    }
    Ok(Captured {
        status,
        stdout: stdout.text(),
        stderr: stderr.text(),
    })
}

#[derive(Default)]
struct Tail {
    bytes: Vec<u8>,
    done: bool,
    /// Set by the reader when new bytes arrived, taken by the caller of the run.
    arrived: bool,
}

struct TailHandle(Arc<Mutex<Tail>>);

impl TailHandle {
    fn lock(&self) -> std::sync::MutexGuard<'_, Tail> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn is_done(&self) -> bool {
        self.lock().done
    }

    /// Whether new bytes arrived since the last call, which then clears the mark.
    fn take_arrived(&self) -> bool {
        std::mem::take(&mut self.lock().arrived)
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.lock().bytes).into_owned()
    }
}

/// The reader is detached: it ends with the pipe, which may outlive the whole run.
fn keep_tail(pipe: Option<impl Read + Send + 'static>) -> TailHandle {
    let tail = Arc::new(Mutex::new(Tail::default()));
    let handle = TailHandle(tail.clone());
    let Some(mut pipe) = pipe else {
        handle.lock().done = true;
        return handle;
    };
    thread::spawn(move || {
        let writer = TailHandle(tail);
        let mut chunk = [0; 8192];
        while let Ok(count @ 1..) = pipe.read(&mut chunk) {
            let mut tail = writer.lock();
            tail.arrived = true;
            tail.bytes.extend_from_slice(&chunk[..count]);
            let extra = tail.bytes.len().saturating_sub(KEPT_BYTES);
            tail.bytes.drain(..extra);
        }
        writer.lock().done = true;
    });
    handle
}
