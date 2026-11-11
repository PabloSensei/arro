use std::collections::VecDeque;
use std::io::Read;
use std::sync::mpsc::{
    Receiver, RecvError, RecvTimeoutError, SyncSender, TryRecvError, TrySendError, sync_channel,
};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, sleep};
use std::time::{Duration, Instant};

use base::Cancel;
use proc_sup::{Process, ProcessSpec};

use crate::RunPlan;

/// Pieces of output on their way to the reader. The program waits when this is full, so a
/// program that prints without end cannot fill memory.
const QUEUE: usize = 256;
const POLL: Duration = Duration::from_millis(20);
/// How long output may still arrive after the program exited while something it started
/// still runs.
const DRAIN: Duration = Duration::from_millis(200);
/// The same when nothing of the program runs any more: the pipes are closed, so this only
/// bounds a handle some other process inherited by chance.
const TAIL: Duration = Duration::from_secs(5);
/// How much of the output is kept for a marker search. A build prints much more.
const WATCH_TAIL: usize = 16 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stream {
    Stdout,
    Stderr,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConsoleEvent {
    /// A step of the plan started.
    Started {
        pid: u32,
        step: Step,
    },
    Output {
        stream: Stream,
        text: String,
    },
    /// Could not start a step.
    Failed {
        message: String,
    },
    /// The process of the plan ended. Output that a child or a server still writes may
    /// come after it, until [`ConsoleEvent::Closed`].
    Exited {
        code: Option<i32>,
        step: Step,
    },
    /// The last event: the pipes are closed and no more output will come.
    Closed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Before,
    Program,
}

/// A running plan and its output. Dropping it ends what is running.
pub struct Console {
    events: Receiver<ConsoleEvent>,
    backlog: Mutex<VecDeque<ConsoleEvent>>,
    stop: Cancel,
    keeper: Option<thread::JoinHandle<()>>,
    watch: Arc<OutputWatch>,
}

/// The events of a console, with what [`Console::wait_for_output`] took aside while it
/// waited. The window sees them in order.
pub struct ConsoleEvents<'a> {
    events: &'a Receiver<ConsoleEvent>,
    backlog: &'a Mutex<VecDeque<ConsoleEvent>>,
}

impl ConsoleEvents<'_> {
    pub fn try_recv(&self) -> Result<ConsoleEvent, TryRecvError> {
        if let Some(event) = self.take_backlog() {
            return Ok(event);
        }
        self.events.try_recv()
    }

    pub fn recv_timeout(&self, limit: Duration) -> Result<ConsoleEvent, RecvTimeoutError> {
        if let Some(event) = self.take_backlog() {
            return Ok(event);
        }
        self.events.recv_timeout(limit)
    }

    pub fn recv(&self) -> Result<ConsoleEvent, RecvError> {
        if let Some(event) = self.take_backlog() {
            return Ok(event);
        }
        self.events.recv()
    }

    fn take_backlog(&self) -> Option<ConsoleEvent> {
        self.backlog
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .pop_front()
    }
}

/// The tail of the output and whether the plan is over, for a caller that waits for a
/// marker without taking events away from the window.
#[derive(Default)]
struct OutputWatch {
    state: Mutex<WatchState>,
    changed: Condvar,
}

#[derive(Default)]
struct WatchState {
    tail: String,
    ended: bool,
}

impl OutputWatch {
    fn observe(&self, text: &str) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.tail.push_str(text);
        if state.tail.len() > WATCH_TAIL {
            let mut cut = state.tail.len() - WATCH_TAIL;
            while cut < state.tail.len() && !state.tail.is_char_boundary(cut) {
                cut += 1;
            }
            state.tail.drain(..cut);
        }
        drop(state);
        self.changed.notify_all();
    }

    fn end(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.ended = true;
        drop(state);
        self.changed.notify_all();
    }

    fn wait_for(&self, marker: &str, limit: Duration) -> bool {
        let deadline = Instant::now() + limit;
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        loop {
            if state.tail.contains(marker) {
                return true;
            }
            if state.ended {
                return false;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            let (next, _) = self
                .changed
                .wait_timeout(state, left)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state = next;
        }
    }

    fn contains(&self, marker: &str) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .tail
            .contains(marker)
    }

    fn ended(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .ended
    }
}

/// What the keeper of a console runs.
enum Work {
    Plan(RunPlan),
    /// One step only, for a caller that starts the program itself after it succeeds.
    Single {
        spec: ProcessSpec,
        step: Step,
        grace: Duration,
    },
}

impl Console {
    pub fn start(plan: RunPlan) -> Self {
        Self::spawn(Work::Plan(plan))
    }

    /// Runs one build step and stops there. `ConsoleEvent::Exited` with [`Step::Program`]
    /// never comes.
    pub fn start_step(spec: ProcessSpec, step: Step, grace: Duration) -> Self {
        Self::spawn(Work::Single { spec, step, grace })
    }

    fn spawn(work: Work) -> Self {
        let (sender, events) = sync_channel(QUEUE);
        let stop = Cancel::new();
        let cancel = stop.clone();
        let watch = Arc::new(OutputWatch::default());
        let keeper_watch = watch.clone();
        let keeper = thread::spawn(move || {
            keep(work, &cancel, &sender, &keeper_watch);
            keeper_watch.end();
        });
        Self {
            events,
            backlog: Mutex::new(VecDeque::new()),
            stop,
            keeper: Some(keeper),
            watch,
        }
    }

    pub fn events(&self) -> ConsoleEvents<'_> {
        ConsoleEvents {
            events: &self.events,
            backlog: &self.backlog,
        }
    }

    /// Waits until the output contains `marker`. Events that arrive meanwhile are kept
    /// for [`Console::events`], so the reader of the program is not held up by the wait.
    /// False when the plan ends, `stop` is cancelled, or the limit passes first.
    pub fn wait_for_output(&self, marker: &str, limit: Duration, stop: &Cancel) -> bool {
        let deadline = Instant::now() + limit;
        loop {
            self.drain_events();
            if stop.is_cancelled() {
                return false;
            }
            if self.watch.contains(marker) {
                self.drain_events();
                return true;
            }
            if self.watch.ended() {
                self.drain_events();
                return self.watch.contains(marker);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                self.drain_events();
                return false;
            }
            self.watch
                .wait_for(marker, left.min(Duration::from_millis(20)));
        }
    }

    fn drain_events(&self) {
        let mut backlog = self
            .backlog
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        while let Ok(event) = self.events.try_recv() {
            backlog.push_back(event);
        }
    }

    /// Asks the plan to stop and returns at once. [`ConsoleEvent::Exited`] follows.
    pub fn stop(&self) {
        self.stop.cancel();
    }
}

impl Drop for Console {
    fn drop(&mut self) {
        self.stop.cancel();
        // Reading no more: the queue must not keep the keeper from ending the program.
        let (_, closed) = sync_channel(0);
        self.events = closed;
        if let Some(keeper) = self.keeper.take() {
            let _ = keeper.join();
        }
    }
}

fn keep(work: Work, stop: &Cancel, events: &SyncSender<ConsoleEvent>, watch: &Arc<OutputWatch>) {
    match work {
        Work::Plan(plan) => keep_plan(plan, stop, events, watch),
        Work::Single { spec, step, grace } => {
            let end = run_step(&spec, step, grace, stop, events, watch);
            let _ = events.send(ConsoleEvent::Exited {
                code: end.code,
                step,
            });
            // The exit code is known; the output is read until the pipes close, so a
            // program that writes through a child after it ended is not cut off.
            end.drain(stop, None);
            let _ = events.send(ConsoleEvent::Closed);
        }
    }
}

fn keep_plan(
    plan: RunPlan,
    stop: &Cancel,
    events: &SyncSender<ConsoleEvent>,
    watch: &Arc<OutputWatch>,
) {
    if let Some(before) = &plan.before {
        let end = run_step(before, Step::Before, plan.grace, stop, events, watch);
        if end.code != Some(0) || stop.is_cancelled() {
            let step = Step::Before;
            let _ = events.send(ConsoleEvent::Exited {
                code: end.code,
                step,
            });
            end.drain(stop, Some(DRAIN));
            let _ = events.send(ConsoleEvent::Closed);
            return;
        }
        // A build that left a daemon behind must not hold the program back.
        end.drain(stop, Some(DRAIN));
    }
    let end = run_step(
        &plan.program,
        Step::Program,
        plan.grace,
        stop,
        events,
        watch,
    );
    let step = Step::Program;
    let _ = events.send(ConsoleEvent::Exited {
        code: end.code,
        step,
    });
    end.drain(stop, None);
    let _ = events.send(ConsoleEvent::Closed);
}

/// A step whose process ended and whose readers may still be seeing output.
struct StepEnd {
    code: Option<i32>,
    /// Kept alive while the readers wait: dropping it closes the job of the program.
    _process: Option<Process>,
    readers: Vec<thread::JoinHandle<()>>,
}

impl StepEnd {
    /// Waits for the readers to see the end of their pipes. `limit` bounds a step that must
    /// not hold the next one back; the program itself waits as long as the pipes live, and
    /// Stop ends the wait.
    fn drain(self, stop: &Cancel, limit: Option<Duration>) {
        let start = Instant::now();
        while !self.readers.iter().all(thread::JoinHandle::is_finished) {
            if stop.is_cancelled() {
                return;
            }
            if limit.is_some_and(|limit| start.elapsed() >= limit) {
                return;
            }
            sleep(POLL);
        }
    }
}

fn run_step(
    spec: &ProcessSpec,
    step: Step,
    grace: Duration,
    stop: &Cancel,
    events: &SyncSender<ConsoleEvent>,
    watch: &Arc<OutputWatch>,
) -> StepEnd {
    let mut process = match spec.start() {
        Ok(process) => process,
        Err(error) => {
            let message = error.to_string();
            let _ = events.send(ConsoleEvent::Failed { message });
            return StepEnd {
                code: None,
                _process: None,
                readers: Vec::new(),
            };
        }
    };
    let pid = process.pid();
    let _ = events.send(ConsoleEvent::Started { pid, step });
    drop(process.take_stdin());
    let readers = vec![
        read_pipe(
            process.take_stdout(),
            Stream::Stdout,
            events.clone(),
            watch.clone(),
        ),
        read_pipe(
            process.take_stderr(),
            Stream::Stderr,
            events.clone(),
            watch.clone(),
        ),
    ];
    let result = wait(&mut process, grace, stop);
    // With the whole tree gone the pipes are closed and the readers reach their end, however
    // late a busy machine runs them. A server the program left behind may hold the pipes
    // open, so then the readers only get a moment. The tree is asked again on every poll:
    // the console host of a windowless program stays in the job for a moment after the
    // program, and one look at that moment would cut the readers short.
    let start = Instant::now();
    while !readers.iter().all(thread::JoinHandle::is_finished) {
        let tree_gone = process.tree().is_ok_and(|tree| tree.is_empty());
        let patience = if tree_gone { TAIL } else { DRAIN };
        if start.elapsed() >= patience {
            break;
        }
        sleep(POLL);
    }
    let code = match result {
        Ok(code) => code,
        Err(message) => {
            let _ = events.send(ConsoleEvent::Failed { message });
            None
        }
    };
    StepEnd {
        code,
        _process: Some(process),
        readers,
    }
}

fn wait(process: &mut Process, grace: Duration, stop: &Cancel) -> Result<Option<i32>, String> {
    loop {
        if stop.is_cancelled() {
            let status = process
                .shut_down(grace)
                .map_err(|error| error.to_string())?;
            return Ok(status.code());
        }
        match process.try_wait() {
            Ok(Some(status)) => return Ok(status.code()),
            Ok(None) => sleep(POLL),
            Err(error) => return Err(error.to_string()),
        }
    }
}

fn read_pipe(
    pipe: Option<impl Read + Send + 'static>,
    stream: Stream,
    events: SyncSender<ConsoleEvent>,
    watch: Arc<OutputWatch>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let Some(mut pipe) = pipe else { return };
        let mut chunk = [0; 8192];
        let mut decoder = Utf8Decoder::default();
        while let Ok(count @ 1..) = pipe.read(&mut chunk) {
            let text = decoder.push(&chunk[..count]);
            if !text.is_empty() {
                watch.observe(&text);
                if !deliver(&events, stream, text) {
                    return;
                }
            }
        }
        let text = decoder.finish();
        if !text.is_empty() {
            watch.observe(&text);
            deliver(&events, stream, text);
        }
    })
}

/// False when nobody reads any more.
fn deliver(events: &SyncSender<ConsoleEvent>, stream: Stream, text: String) -> bool {
    let event = ConsoleEvent::Output { stream, text };
    match events.try_send(event) {
        Ok(()) => true,
        Err(TrySendError::Full(event)) => events.send(event).is_ok(),
        Err(TrySendError::Disconnected(_)) => false,
    }
}

/// A read may end in the middle of a character, which must not turn into garbage.
#[derive(Default)]
struct Utf8Decoder {
    pending: Vec<u8>,
}

impl Utf8Decoder {
    fn push(&mut self, bytes: &[u8]) -> String {
        self.pending.extend_from_slice(bytes);
        let complete = match std::str::from_utf8(&self.pending) {
            Ok(_) => self.pending.len(),
            Err(error) if error.error_len().is_none() => error.valid_up_to(),
            Err(_) => self.pending.len(),
        };
        let text = String::from_utf8_lossy(&self.pending[..complete]).into_owned();
        self.pending.drain(..complete);
        text
    }

    fn finish(&mut self) -> String {
        let text = String::from_utf8_lossy(&self.pending).into_owned();
        self.pending.clear();
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PATIENCE: Duration = Duration::from_secs(10);

    fn shell(line: &str) -> ProcessSpec {
        ProcessSpec::new("cmd.exe").args(["/d", "/c", line])
    }

    fn long_program() -> ProcessSpec {
        ProcessSpec::new("ping.exe")
            .args(["-n", "120", "127.0.0.1"])
            .ctrl_helper("no-such-helper.exe")
    }

    fn plan(before: Option<ProcessSpec>, program: ProcessSpec) -> RunPlan {
        RunPlan {
            before,
            program,
            grace: Duration::ZERO,
        }
    }

    fn to_end(console: &Console) -> (String, Option<i32>, Step) {
        let mut text = String::new();
        loop {
            match console.events().recv_timeout(PATIENCE).unwrap() {
                ConsoleEvent::Output { text: part, .. } => text.push_str(&part),
                ConsoleEvent::Exited { code, step } => return (text, code, step),
                ConsoleEvent::Started { .. } | ConsoleEvent::Failed { .. } => {}
                ConsoleEvent::Closed => {}
            }
        }
    }

    #[test]
    fn output_and_exit_code_arrive_in_order() {
        let console = Console::start(plan(None, shell("echo hello& exit /b 3")));

        let (text, code, step) = to_end(&console);

        assert_eq!(text.trim(), "hello");
        assert_eq!((code, step), (Some(3), Step::Program));
    }

    #[test]
    fn the_program_follows_a_step_that_succeeded() {
        let console = Console::start(plan(Some(shell("echo first")), shell("echo second")));

        let (text, code, _) = to_end(&console);

        assert_eq!(
            text.split_whitespace().collect::<Vec<_>>(),
            ["first", "second"]
        );
        assert_eq!(code, Some(0));
    }

    #[test]
    fn a_step_that_failed_keeps_the_program_from_starting() {
        let console = Console::start(plan(Some(shell("exit /b 7")), shell("echo second")));

        let (text, code, step) = to_end(&console);

        assert_eq!(text, "");
        assert_eq!((code, step), (Some(7), Step::Before));
    }

    #[test]
    fn a_program_that_cannot_start_is_reported() {
        let console = Console::start(plan(None, ProcessSpec::new("no-such-program.exe")));

        let first = console.events().recv_timeout(PATIENCE).unwrap();

        assert!(matches!(first, ConsoleEvent::Failed { .. }), "{first:?}");
        let (_, code, _) = to_end(&console);
        assert_eq!(code, None);
    }

    #[test]
    fn stop_ends_the_program_and_reports_it() {
        let console = Console::start(plan(None, long_program()));

        console.stop();

        let (_, code, _) = to_end(&console);
        assert_eq!(code, Some(1));
    }

    #[test]
    fn stop_during_the_first_step_skips_the_program() {
        let console = Console::start(plan(Some(long_program()), shell("echo second")));

        console.stop();

        let (text, _, step) = to_end(&console);
        assert_eq!(step, Step::Before);
        assert!(!text.contains("second"), "{text}");
    }

    #[test]
    fn dropping_the_console_ends_the_program() {
        let console = Console::start(plan(None, long_program()));
        let start = Instant::now();

        drop(console);

        assert!(start.elapsed() < PATIENCE);
    }

    #[test]
    fn a_marker_in_the_output_is_seen_without_taking_events() {
        let console = Console::start(plan(None, shell("echo ready-marker")));

        assert!(console.wait_for_output("ready-marker", PATIENCE, &Cancel::new()));

        let (text, code, _) = to_end(&console);
        assert!(text.contains("ready-marker"), "{text}");
        assert_eq!(code, Some(0));
    }

    #[test]
    fn a_marker_that_never_comes_ends_with_the_plan() {
        let console = Console::start(plan(None, shell("echo other")));

        assert!(!console.wait_for_output("ready-marker", PATIENCE, &Cancel::new()));
    }

    /// The parent exits after starting a child that keeps its output pipe and writes
    /// later, as a server started by a program does.
    fn late_writer() -> ProcessSpec {
        ProcessSpec::new("powershell.exe").args([
            "-NoProfile",
            "-Command",
            "Start-Process -FilePath cmd.exe -ArgumentList '/d','/c',\
             'ping -n 2 127.0.0.1 >nul & echo late-output' -NoNewWindow",
        ])
    }

    #[test]
    fn output_written_after_the_program_ended_is_still_read() {
        let console = Console::start(plan(None, late_writer()));

        let mut text = String::new();
        let mut code = None;
        loop {
            match console.events().recv_timeout(PATIENCE).unwrap() {
                ConsoleEvent::Output { text: part, .. } => text.push_str(&part),
                ConsoleEvent::Exited { code: exit, .. } => code = exit,
                ConsoleEvent::Closed => break,
                ConsoleEvent::Started { .. } | ConsoleEvent::Failed { .. } => {}
            }
        }

        assert_eq!(code, Some(0));
        assert!(
            text.contains("late-output"),
            "the output after the exit was lost: {text:?}"
        );
    }

    #[test]
    fn stop_ends_the_wait_for_a_pipe_a_child_keeps_open() {
        let console = Console::start(plan(None, late_writer()));
        std::thread::sleep(Duration::from_millis(300));

        console.stop();
        let start = Instant::now();
        loop {
            if console.events().recv_timeout(PATIENCE).unwrap() == ConsoleEvent::Closed {
                break;
            }
        }

        assert!(start.elapsed() < PATIENCE);
    }

    #[test]
    fn a_character_split_by_a_read_stays_whole() {
        let mut decoder = Utf8Decoder::default();
        let bytes = "я".as_bytes();

        assert_eq!(decoder.push(&bytes[..1]), "");
        assert_eq!(decoder.push(&bytes[1..]), "я");
    }

    #[test]
    fn bytes_that_are_not_text_do_not_stop_the_output() {
        let mut decoder = Utf8Decoder::default();

        assert_eq!(decoder.push(b"a\xffb"), "a\u{fffd}b");
        assert_eq!(decoder.push(b"\xd1"), "");
        assert_eq!(decoder.finish(), "\u{fffd}");
    }
}
