//! One helper process and the events it sends.
//!
//! The helper reads a request per line from its standard input and writes an answer per
//! line to its standard output. The process is owned: it ends with the session, with the
//! window and with the IDE, even when it no longer answers.

use std::collections::HashMap;
use std::ffi::OsString;
use std::io::{self, BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::ChildStdin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, channel, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use proc_sup::{Process, ProcessSpec};

use crate::protocol::{Command, CompletionItem, Event};

/// The requests a window may have written but not read yet. A helper that floods the
/// channel waits instead of growing without a limit.
const QUEUE: usize = 1024;

/// How long a completion waits for its answer before the window gives up.
pub const COMPLETION_TIMEOUT: Duration = Duration::from_secs(5);

/// A request to a console helper, and the answer of a completion that another thread
/// waits for while the window keeps polling the helper.
type Waiter = std::sync::mpsc::Sender<Result<Vec<CompletionItem>, String>>;

/// A source of completion that answers from a console helper. The editor asks it off the
/// UI thread; an answer that arrives after the window is gone is dropped.
pub trait CompletionSource: Send + Sync {
    /// `cursor` is a UTF-16 offset in `text`, as the console counts characters.
    fn complete(
        &self,
        text: &str,
        cursor: usize,
        timeout: Duration,
    ) -> Result<Vec<CompletionItem>, String>;
}

/// How the helper is started: a program, its arguments and the folder it runs in.
#[derive(Clone, Debug)]
pub struct Launch {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub env: Vec<(OsString, OsString)>,
    pub working_dir: Option<PathBuf>,
}

impl Launch {
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            env: Vec::new(),
            working_dir: None,
        }
    }

    pub fn arg(mut self, arg: impl Into<OsString>) -> Self {
        self.args.push(arg.into());
        self
    }

    pub fn args(mut self, args: impl IntoIterator<Item = impl Into<OsString>>) -> Self {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    pub fn env(mut self, name: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.env.push((name.into(), value.into()));
        self
    }

    pub fn working_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.working_dir = Some(dir.into());
        self
    }
}

/// A started helper, its requests and the answers it has already sent.
pub struct Session {
    events: Mutex<Receiver<Event>>,
    input: Arc<Mutex<Option<ChildStdin>>>,
    process: Arc<Mutex<Option<Process>>>,
    waiters: Arc<Mutex<HashMap<u64, Waiter>>>,
    ids: Arc<AtomicU64>,
    process_id: Option<u32>,
}

impl Session {
    /// Starts the helper. The call returns when the process exists, not when the console
    /// answers: the events carry that.
    pub fn start(launch: Launch) -> io::Result<Self> {
        let mut spec = ProcessSpec::new(&launch.program)
            .args(launch.args)
            .input()
            .label("Console helper");
        for (name, value) in launch.env {
            spec = spec.env(name, value);
        }
        if let Some(dir) = launch.working_dir {
            spec = spec.cwd(dir);
        }
        let mut process = spec.start()?;
        let process_id = Some(process.pid());
        let stdin = process.take_stdin();
        let stdout = process.take_stdout();
        if let Some(stderr) = process.take_stderr() {
            // A runtime that complains on its error stream must not fill the pipe and
            // stop: the text is not part of the protocol, the events say what happened.
            thread::spawn(move || {
                let mut stderr = BufReader::new(stderr);
                let mut line = String::new();
                while stderr.read_line(&mut line).unwrap_or(0) > 0 {
                    line.clear();
                }
            });
        }
        let (sender, events) = sync_channel(QUEUE);
        let waiters = Arc::new(Mutex::new(HashMap::new()));
        if let Some(stdout) = stdout {
            let shared = waiters.clone();
            thread::Builder::new()
                .name("console-helper".into())
                .spawn(move || read_events(stdout, sender, shared))?;
        } else {
            let _ = sender.try_send(Event::Exited { code: None });
        }
        Ok(Self {
            events: Mutex::new(events),
            input: Arc::new(Mutex::new(stdin)),
            process: Arc::new(Mutex::new(Some(process))),
            waiters,
            ids: Arc::new(AtomicU64::new(1)),
            process_id,
        })
    }

    pub fn process_id(&self) -> Option<u32> {
        self.process_id
    }

    /// The identifier of the next request. A completion and an eval never share one.
    pub fn next_id(&self) -> u64 {
        self.ids.fetch_add(1, Ordering::Relaxed)
    }

    /// The source the editor asks for completion of the console text.
    pub fn completion_source(&self) -> Arc<dyn CompletionSource> {
        Arc::new(Completer {
            input: self.input.clone(),
            waiters: self.waiters.clone(),
            ids: self.ids.clone(),
        })
    }

    /// Writes one request. A broken pipe means the helper is gone; the events say why.
    pub fn send(&self, command: Command) -> io::Result<()> {
        send(&self.input, &command)
    }

    /// One answer, if the helper has already sent it.
    pub fn try_recv(&self) -> Option<Event> {
        let events = self.events.lock().ok()?;
        events.try_recv().ok()
    }

    /// Waits for one answer. The tests use it; the window polls [`Self::try_recv`].
    pub fn recv_timeout(
        &self,
        timeout: Duration,
    ) -> Result<Event, std::sync::mpsc::RecvTimeoutError> {
        let events = match self.events.lock() {
            Ok(events) => events,
            Err(poison) => poison.into_inner(),
        };
        events.recv_timeout(timeout)
    }

    /// Asks the helper to end and stops it when it does not.
    pub fn close(self) {
        drop(self);
    }

    /// Stops the helper now, without asking it first. The second click of Stop uses it
    /// when a fragment does not answer.
    pub fn kill(&self) {
        let process = match self.process.lock() {
            Ok(mut guard) => guard.take(),
            Err(poison) => poison.into_inner().take(),
        };
        if let Some(mut process) = process {
            let _ = process.stop();
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.send(Command::Close);
        if let Ok(mut guard) = self.input.lock() {
            guard.take();
        }
        let process = match self.process.lock() {
            Ok(mut guard) => guard.take(),
            Err(poison) => poison.into_inner().take(),
        };
        if let Some(mut process) = process {
            let _ = process.stop();
        }
    }
}

/// The window's side of one console, so that a test can give it answers without a
/// process. A helper session is the only implementation the application uses.
pub trait ConsoleSession: Send {
    fn send(&self, command: Command) -> io::Result<()>;
    /// Every answer the helper has sent so far.
    fn poll(&self) -> Vec<Event>;
    /// The identifier of the next request, shared with the completion source.
    fn next_id(&self) -> u64;
    /// Stops the console now, without asking it first.
    fn kill(&self);
    fn completion_source(&self) -> Arc<dyn CompletionSource>;
}

impl ConsoleSession for Session {
    fn send(&self, command: Command) -> io::Result<()> {
        Session::send(self, command)
    }

    fn poll(&self) -> Vec<Event> {
        let mut events = Vec::new();
        while let Some(event) = self.try_recv() {
            events.push(event);
        }
        events
    }

    fn next_id(&self) -> u64 {
        Session::next_id(self)
    }

    fn kill(&self) {
        Session::kill(self);
    }

    fn completion_source(&self) -> Arc<dyn CompletionSource> {
        Session::completion_source(self)
    }
}

fn send(input: &Mutex<Option<ChildStdin>>, command: &Command) -> io::Result<()> {
    let mut guard = match input.lock() {
        Ok(guard) => guard,
        Err(poison) => poison.into_inner(),
    };
    let Some(input) = guard.as_mut() else {
        return Err(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "the helper is closed",
        ));
    };
    input.write_all(command.encode().as_bytes())?;
    input.write_all(b"\n")?;
    input.flush()
}

struct Completer {
    input: Arc<Mutex<Option<ChildStdin>>>,
    waiters: Arc<Mutex<HashMap<u64, Waiter>>>,
    ids: Arc<AtomicU64>,
}

impl CompletionSource for Completer {
    fn complete(
        &self,
        text: &str,
        cursor: usize,
        timeout: Duration,
    ) -> Result<Vec<CompletionItem>, String> {
        let id = self.ids.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = channel();
        if let Ok(mut waiters) = self.waiters.lock() {
            waiters.insert(id, sender);
        }
        let command = Command::Complete {
            id,
            text: text.to_owned(),
            cursor,
        };
        if let Err(error) = send(&self.input, &command) {
            if let Ok(mut waiters) = self.waiters.lock() {
                waiters.remove(&id);
            }
            return Err(error.to_string());
        }
        let answer = receiver.recv_timeout(timeout);
        if let Ok(mut waiters) = self.waiters.lock() {
            waiters.remove(&id);
        }
        match answer {
            Ok(answer) => answer,
            Err(_) => Err("the console did not answer".to_owned()),
        }
    }
}

fn read_events(
    stdout: impl io::Read,
    sender: SyncSender<Event>,
    waiters: Arc<Mutex<HashMap<u64, Waiter>>>,
) {
    let mut reader = BufReader::new(stdout);
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                let Some(event) = Event::decode(line.trim_end()) else {
                    continue;
                };
                // A completion wakes the thread that asked for it before the window sees
                // the same event: the window ignores completions of its own.
                match &event {
                    Event::Completion { id, items } => {
                        if let Ok(mut waiters) = waiters.lock()
                            && let Some(waiter) = waiters.remove(id)
                        {
                            let _ = waiter.send(Ok(items.clone()));
                        }
                    }
                    Event::Error { id, message } => {
                        if let Ok(mut waiters) = waiters.lock()
                            && let Some(waiter) = waiters.remove(id)
                        {
                            let _ = waiter.send(Err(message.clone()));
                        }
                    }
                    _ => {}
                }
                match sender.try_send(event) {
                    Ok(()) => {}
                    Err(TrySendError::Full(event)) => {
                        // The window is behind. Waiting is what keeps the memory of the
                        // process bounded; the window reads the queue in its poll.
                        if sender.send(event).is_err() {
                            break;
                        }
                    }
                    Err(TrySendError::Disconnected(_)) => break,
                }
            }
            Err(_) => break,
        }
    }
    if let Ok(mut waiters) = waiters.lock() {
        for (_, waiter) in waiters.drain() {
            let _ = waiter.send(Err("the helper ended".to_owned()));
        }
    }
    let _ = sender.try_send(Event::Exited { code: None });
}
