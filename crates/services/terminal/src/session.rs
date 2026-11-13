//! One program in a pseudoconsole. Four threads serve it: the reader drains the pipe, the engine
//! owns `Term`, the writer owns the blocking input pipe, the waiter ends the stream when the child
//! exits. No call here waits for the child, so the UI thread may use all of them.

use std::fs::File;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use base::Snapshot;
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TrySendError, bounded, select};

use crate::conpty::{Backend, Pty};
use crate::engine::Engine;
use crate::keys::{KeyEvent, ShiftEnter, encode, encode_text};
use crate::screen::{Modes, Screen};

const READ_BUFFER: usize = 64 * 1024;
/// Ceilings of the queues. Output backpressure reaches the child through the pipe.
const OUTPUT_CHUNKS: usize = 16;
const INPUT_MESSAGES: usize = 1024;
const REPLIES: usize = 256;
const EVENTS: usize = 1024;
const CONTROLS: usize = 64;
/// One write to the pipe. The boundaries fall on chars, and replies pass between chunks.
const WRITE_CHUNK: usize = 16 * 1024;
/// The shortest gap between two snapshots of the process table while output flows.
const ADOPT_INTERVAL: Duration = Duration::from_millis(250);

/// What to start. `env` is the whole environment of the child, see `allowlisted`.
#[derive(Clone, Debug)]
pub struct SpawnSpec {
    pub command_line: String,
    pub cwd: Option<PathBuf>,
    pub env: Vec<(String, String)>,
    pub cols: u16,
    pub rows: u16,
    pub shift_enter: ShiftEnter,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalEvent {
    /// Bytes arrived, a new snapshot follows.
    Output,
    Title(String),
    Bell,
    Osc {
        code: String,
        payload: String,
    },
    /// The stream ended. `None` when the exit code never became known.
    Exited(Option<u32>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputError {
    /// The program is not reading fast enough, and the queue is full.
    Busy,
    Closed,
}

impl std::fmt::Display for InputError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(match self {
            Self::Busy => "the terminal input queue is full",
            Self::Closed => "the terminal is closed",
        })
    }
}

impl std::error::Error for InputError {}

pub(crate) enum Control {
    Resize,
    Scroll(i32),
    ScrollToBottom,
}

/// What the threads of a session have in common. All of it is lock-free for readers.
pub(crate) struct Shared {
    pub(crate) screen: Snapshot<Screen>,
    pub(crate) modes: AtomicU32,
    pub(crate) bytes: AtomicU64,
    pub(crate) exit: OnceLock<u32>,
    size: AtomicU32,
    pub(crate) pty: Arc<Pty>,
}

impl Shared {
    pub(crate) fn requested_size(&self) -> (u16, u16) {
        let size = self.size.load(Ordering::SeqCst);
        ((size >> 16) as u16, size as u16)
    }

    pub(crate) fn exit_code(&self) -> Option<u32> {
        self.exit.get().copied()
    }
}

fn pack_size(cols: u16, rows: u16) -> u32 {
    (cols as u32) << 16 | rows as u32
}

fn spawn_thread(name: &str, work: impl FnOnce() + Send + 'static) -> io::Result<()> {
    thread::Builder::new()
        .name(name.to_string())
        .spawn(work)
        .map(drop)
}

pub struct TerminalSession {
    shared: Arc<Shared>,
    input: Sender<Vec<Vec<u8>>>,
    control: Sender<Control>,
    events: Receiver<TerminalEvent>,
    shift_enter: ShiftEnter,
}

impl TerminalSession {
    pub fn spawn(spec: &SpawnSpec) -> io::Result<Self> {
        let (pty, input_pipe, output_pipe) = Pty::spawn(spec)?;
        let pty = Arc::new(pty);
        let shared = Arc::new(Shared {
            screen: Snapshot::new(Screen::default()),
            modes: AtomicU32::new(0),
            bytes: AtomicU64::new(0),
            exit: OnceLock::new(),
            size: AtomicU32::new(pack_size(spec.cols, spec.rows)),
            pty: pty.clone(),
        });

        let (input, input_queue) = bounded(INPUT_MESSAGES);
        let (replies, reply_queue) = bounded(REPLIES);
        let (output, output_queue) = bounded(OUTPUT_CHUNKS);
        let (control, control_queue) = bounded(CONTROLS);
        let (event_sink, events) = bounded(EVENTS);
        let (output_seen, adopt_queue) = bounded(1);

        spawn_thread("pty-writer", move || {
            write_loop(input_pipe, reply_queue, input_queue);
        })?;
        spawn_thread("pty-reader", move || {
            read_loop(output_pipe, output, output_seen);
        })?;
        let adopt_pty = pty.clone();
        spawn_thread("pty-adopt", move || adopt_loop(adopt_queue, adopt_pty))?;
        let (cols, rows) = (spec.cols, spec.rows);
        let engine_shared = shared.clone();
        // `Term` is built on its own thread: its listener is not `Send`.
        spawn_thread("pty-engine", move || {
            let engine = Engine::new(cols, rows, replies, event_sink, engine_shared);
            engine.run(output_queue, control_queue);
        })?;
        let waiter_shared = shared.clone();
        spawn_thread("pty-waiter", move || {
            let code = pty.wait();
            let _ = waiter_shared.exit.set(code);
            // Programs outside the job do not go with it; the rounds end them by their
            // parent id, which the snapshot keeps after the shell is gone.
            pty.kill_tree();
            // The console goes last: a program still attached would keep the close waiting.
            pty.close_console();
            // Whatever the cleanup rounds missed still dies with the job.
            pty.terminate();
            pty.kill_tree();
        })?;

        Ok(Self {
            shared,
            input,
            control,
            events,
            shift_enter: spec.shift_enter,
        })
    }

    pub fn pid(&self) -> u32 {
        self.shared.pty.pid
    }

    pub fn backend(&self) -> Backend {
        self.shared.pty.backend()
    }

    /// The latest picture. Taking it never waits.
    pub fn screen(&self) -> Arc<Screen> {
        self.shared.screen.get()
    }

    /// The modes in force now, which a snapshot may lag behind.
    pub fn modes(&self) -> Modes {
        Modes::from_bits(self.shared.modes.load(Ordering::SeqCst))
    }

    /// There is one receiver, and whoever owns the session reads it.
    pub fn events(&self) -> Receiver<TerminalEvent> {
        self.events.clone()
    }

    pub fn output_bytes(&self) -> u64 {
        self.shared.bytes.load(Ordering::Relaxed)
    }

    pub fn exit_code(&self) -> Option<u32> {
        self.shared.exit_code()
    }

    /// True once the stream ended and the last picture is out. The flag lives in the
    /// snapshot itself, so a reader of the final version cannot miss the end.
    pub fn is_closed(&self) -> bool {
        self.shared.screen.get().closed
    }

    fn enqueue(&self, segments: Vec<Vec<u8>>) -> Result<(), InputError> {
        self.input.try_send(segments).map_err(|error| match error {
            TrySendError::Full(_) => InputError::Busy,
            TrySendError::Disconnected(_) => InputError::Closed,
        })
    }

    /// Bytes as they are, for what a host encoded itself.
    pub fn send(&self, bytes: impl Into<Vec<u8>>) -> Result<(), InputError> {
        self.enqueue(vec![bytes.into()])
    }

    pub fn key(&self, event: KeyEvent) -> Result<(), InputError> {
        self.send(encode(event, self.modes(), self.shift_enter))
    }

    /// One queue entry however long the text is, the writer feeds it as the program reads.
    pub fn paste(&self, text: &str) -> Result<(), InputError> {
        let modes = self.modes();
        let normalized = sanitize(text).replace('\n', "\r");
        let encoded = encode_text(&normalized, modes);
        let segments = if modes.bracketed_paste {
            let (open, close) = (b"\x1b[200~".to_vec(), b"\x1b[201~".to_vec());
            vec![open, encoded, close]
        } else {
            vec![encoded]
        };
        self.enqueue(segments)
    }

    /// For programs that never see a paste as one event: line ends go out as Shift+Enter, which
    /// can only mean a new line, so a pause in the stream cannot turn one into a submit.
    pub fn paste_lines(&self, text: &str) -> Result<(), InputError> {
        let modes = self.modes();
        let newline = encode(
            KeyEvent::new(crate::Key::Enter, crate::Mods::SHIFT),
            modes,
            self.shift_enter,
        );
        let sanitized = sanitize(text);
        let mut segments = Vec::new();
        for (index, line) in sanitized.split('\n').enumerate() {
            if index > 0 {
                segments.push(newline.clone());
            }
            segments.push(encode_text(line, modes));
        }
        self.enqueue(segments)
    }

    /// Tells a program that asked for it whether the terminal has focus.
    pub fn focus(&self, gained: bool) -> Result<(), InputError> {
        if !self.modes().focus_events {
            return Ok(());
        }
        self.send(if gained {
            &b"\x1b[I"[..]
        } else {
            &b"\x1b[O"[..]
        })
    }

    /// The size asked for last, which the screen may still lag behind.
    pub fn size(&self) -> (u16, u16) {
        self.shared.requested_size()
    }

    /// The last size wins: a full queue means a resize is already on its way.
    pub fn resize(&self, cols: u16, rows: u16) {
        self.shared
            .size
            .store(pack_size(cols, rows), Ordering::SeqCst);
        drop(self.control.try_send(Control::Resize));
    }

    /// Positive goes back into history.
    pub fn scroll(&self, lines: i32) {
        drop(self.control.try_send(Control::Scroll(lines)));
    }

    pub fn scroll_to_bottom(&self) {
        drop(self.control.try_send(Control::ScrollToBottom));
    }

    /// Kills the program and everything it started.
    pub fn terminate(&self) {
        self.shared.pty.terminate();
    }

    /// Polls snapshots. For tests and background workers, never for the UI thread.
    pub fn wait_for(&self, timeout: Duration, check: impl Fn(&Screen) -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if check(&self.screen()) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    /// Polls until the stream has ended. For tests and background workers.
    pub fn wait_closed(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while !self.is_closed() {
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(10));
        }
        true
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        // The threads see the end of the stream and leave on their own.
        self.terminate();
    }
}

/// An escape in pasted text could end the bracket and turn the rest into keystrokes.
fn sanitize(text: &str) -> String {
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('\x1b', "")
}

fn read_loop(mut pipe: File, output: Sender<Vec<u8>>, seen: Sender<()>) {
    let mut buffer = vec![0; READ_BUFFER];
    loop {
        match pipe.read(&mut buffer) {
            Ok(0) | Err(_) => return,
            Ok(count) => {
                if output.send(buffer[..count].to_vec()).is_err() {
                    return;
                }
                // One queued signal is enough: a snapshot covers every chunk that came
                // before the adopt thread wakes up.
                let _ = seen.try_send(());
            }
        }
    }
}

/// Takes a snapshot of the process table shortly after the shell prints something. A
/// quiet terminal leaves this thread blocked on the channel: no timer and no snapshot.
/// Output that keeps coming costs at most one snapshot per interval, and the one delayed
/// call after a burst covers a program that printed nothing but its prompt.
fn adopt_loop(signals: Receiver<()>, pty: Arc<Pty>) {
    let mut last: Option<Instant> = None;
    loop {
        if signals.recv().is_err() {
            return;
        }
        if let Some(previous) = last {
            let rest = ADOPT_INTERVAL.saturating_sub(previous.elapsed());
            if !rest.is_zero() {
                thread::sleep(rest);
                while signals.try_recv().is_ok() {}
            }
        }
        pty.adopt();
        last = Some(Instant::now());
        match signals.recv_timeout(ADOPT_INTERVAL) {
            Ok(()) => {}
            Err(RecvTimeoutError::Timeout) => {
                // The output stopped: one last look at the tree it may have started.
                pty.adopt();
                last = Some(Instant::now());
            }
            Err(RecvTimeoutError::Disconnected) => {
                pty.adopt();
                return;
            }
        }
    }
}

/// Longest prefix of at most `WRITE_CHUNK` bytes that ends on a char boundary.
fn chunk_end(bytes: &[u8]) -> usize {
    let mut end = bytes.len().min(WRITE_CHUNK);
    while end > 0 && end < bytes.len() && bytes[end] & 0xC0 == 0x80 {
        end -= 1;
    }
    if end == 0 { bytes.len() } else { end }
}

/// Replies go first: a program waiting for one may stop reading input until it gets it.
fn write_loop(mut pipe: File, replies: Receiver<Vec<u8>>, input: Receiver<Vec<Vec<u8>>>) {
    let write_replies = |pipe: &mut File| -> io::Result<()> {
        while let Ok(reply) = replies.try_recv() {
            pipe.write_all(&reply)?;
        }
        Ok(())
    };
    loop {
        if write_replies(&mut pipe).is_err() {
            return;
        }
        select! {
            recv(replies) -> reply => {
                let Ok(reply) = reply else { return };
                if pipe.write_all(&reply).is_err() {
                    return;
                }
            }
            recv(input) -> segments => {
                let Ok(segments) = segments else { return };
                for segment in segments {
                    let mut rest = segment.as_slice();
                    while !rest.is_empty() {
                        let (chunk, tail) = rest.split_at(chunk_end(rest));
                        rest = tail;
                        if pipe.write_all(chunk).is_err() || write_replies(&mut pipe).is_err() {
                            return;
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_end_on_char_boundaries() {
        let text = "я".repeat(WRITE_CHUNK);
        let bytes = text.as_bytes();

        let end = chunk_end(bytes);

        assert!(end <= WRITE_CHUNK);
        assert!(text.is_char_boundary(end));
    }

    #[test]
    fn a_paste_cannot_close_its_own_bracket() {
        assert_eq!(sanitize("a\x1b[201~b\r\nc\rd"), "a[201~b\nc\nd");
    }

    /// A program that prints one line, starts a long child and then stays quiet.
    #[cfg(windows)]
    fn quiet_session() -> TerminalSession {
        let line = crate::command_line(
            std::path::Path::new("cmd.exe"),
            &[
                "/c".to_string(),
                "echo ready & ping -n 30 127.0.0.1 >NUL".to_string(),
            ],
        );
        let session = TerminalSession::spawn(&SpawnSpec {
            command_line: line,
            cwd: None,
            env: crate::allowlisted(),
            cols: 80,
            rows: 12,
            shift_enter: ShiftEnter::default(),
        })
        .expect("spawn");
        assert!(session.wait_for(Duration::from_secs(15), |screen| screen.contains("ready")));
        session
    }

    #[cfg(windows)]
    #[test]
    fn a_quiet_terminal_takes_no_process_snapshots() {
        let session = quiet_session();
        // The burst that printed the line and its delayed snapshot pass first.
        thread::sleep(Duration::from_millis(600));
        let before = session.shared.pty.adopt_calls();
        thread::sleep(Duration::from_millis(1200));
        let after = session.shared.pty.adopt_calls();
        assert_eq!(before, after, "a quiet terminal took process snapshots");
    }

    #[cfg(windows)]
    #[test]
    fn the_job_of_a_session_holds_the_program_of_its_shell() {
        let session = quiet_session();
        let shell = session.pid();
        let started = Instant::now();
        let program = loop {
            let program = platform::process_table()
                .into_iter()
                .find(|process| {
                    process.parent == Some(shell) && process.name.eq_ignore_ascii_case("PING.EXE")
                })
                .map(|process| process.pid);
            if let Some(program) = program {
                break program;
            }
            assert!(
                started.elapsed() < Duration::from_secs(15),
                "the shell started no program"
            );
            thread::sleep(Duration::from_millis(20));
        };
        let started = Instant::now();
        while !session.shared.pty.job_ids().contains(&program) {
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "the job does not hold the program {program}"
            );
            thread::sleep(Duration::from_millis(20));
        }
    }
}
