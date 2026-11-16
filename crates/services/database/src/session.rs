//! One helper process and the events it sends.
//!
//! The helper reads a request per line from its standard input and writes an answer per
//! line to its standard output. The process is owned: it ends with the session, with the
//! window and with the IDE, even when it no longer answers.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::{self, BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::ChildStdin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use proc_sup::{Process, ProcessSpec};

use crate::protocol::{Command, Event};

/// The requests a window may have written but not read yet. A helper that floods the
/// channel waits instead of growing without a limit.
const QUEUE: usize = 1024;

/// The lines of the error stream kept as the reason a helper ended: a JVM writes its
/// failure there and the last lines carry it.
const TAIL_LINES: usize = 40;
/// How long the reader of the answers waits for the error stream to end after the last
/// answer, so that the reason of a failed start is in place when the window asks.
const TAIL_WAIT: Duration = Duration::from_millis(200);

/// How the helper is started. It carries driver files and interpreter flags, never a
/// password: the password goes to the helper in [`Command::Connect`] and nowhere else.
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
    events: Receiver<Event>,
    input: Arc<Mutex<Option<ChildStdin>>>,
    process: Arc<Mutex<Option<Process>>>,
    process_id: Option<u32>,
    /// The last lines of the error stream, the reason a failed helper gives.
    tail: Arc<Mutex<VecDeque<String>>>,
}

impl Session {
    /// Starts the helper. The call returns when the process exists, not when the database
    /// answers: the events carry that.
    pub fn start(launch: Launch) -> io::Result<Self> {
        let mut spec = ProcessSpec::new(&launch.program)
            .args(launch.args)
            .input()
            .label("Database helper");
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
        let tail = Arc::new(Mutex::new(VecDeque::new()));
        let errors_done = Arc::new(AtomicBool::new(false));
        if let Some(stderr) = process.take_stderr() {
            // A driver that complains on its error stream must not fill the pipe and stop:
            // the text is not part of the protocol. The last lines are kept, because a
            // helper that dies says why there and the window shows that reason.
            let tail = tail.clone();
            let done = errors_done.clone();
            thread::Builder::new()
                .name("database-helper-errors".into())
                .spawn(move || {
                    read_tail(stderr, &tail);
                    done.store(true, Ordering::Release);
                })?;
        } else {
            errors_done.store(true, Ordering::Release);
        }
        let (sender, events) = sync_channel(QUEUE);
        if let Some(stdout) = stdout {
            thread::Builder::new()
                .name("database-helper".into())
                .spawn(move || read_events(stdout, sender, &errors_done))?;
        } else {
            let _ = sender.try_send(Event::Exited { code: None });
        }
        Ok(Self {
            events,
            input: Arc::new(Mutex::new(stdin)),
            process: Arc::new(Mutex::new(Some(process))),
            process_id,
            tail,
        })
    }

    pub fn process_id(&self) -> Option<u32> {
        self.process_id
    }

    /// The last lines of the error stream of the helper, trimmed, oldest first.
    pub fn output_tail(&self) -> Vec<String> {
        self.tail
            .lock()
            .map(|tail| tail.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Writes one request. A broken pipe means the helper is gone; the events say why.
    pub fn send(&self, command: Command) -> io::Result<()> {
        let mut guard = match self.input.lock() {
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

    /// The answers of the helper. The channel is bounded, so a flood of rows waits for
    /// the window instead of growing without a limit.
    pub fn events(&self) -> &Receiver<Event> {
        &self.events
    }

    /// Asks the helper to end and stops it when it does not.
    pub fn close(self) {
        drop(self);
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.send(Command::Close { id: 0 });
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

fn read_events(stdout: impl io::Read, sender: SyncSender<Event>, errors_done: &AtomicBool) {
    let mut reader = BufReader::new(stdout);
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                if let Some(event) = Event::decode(line.trim_end()) {
                    match sender.try_send(event) {
                        Ok(()) => {}
                        Err(TrySendError::Full(event)) => {
                            // The window is behind. Waiting is what keeps the memory of
                            // the process bounded; the window reads the queue in its poll.
                            if sender.send(event).is_err() {
                                break;
                            }
                        }
                        Err(TrySendError::Disconnected(_)) => break,
                    }
                }
            }
            Err(_) => break,
        }
    }
    // The error text usually arrives with the end of the process; a short bounded wait
    // gives the reader of that stream time to keep it, without holding the window.
    let started = Instant::now();
    while !errors_done.load(Ordering::Acquire) && started.elapsed() < TAIL_WAIT {
        thread::sleep(Duration::from_millis(5));
    }
    let _ = sender.try_send(Event::Exited { code: None });
}

/// Keeps the last lines of the error stream of the helper.
fn read_tail(stderr: impl io::Read, tail: &Mutex<VecDeque<String>>) {
    let mut reader = BufReader::new(stderr);
    let mut line = String::new();
    while reader.read_line(&mut line).unwrap_or(0) > 0 {
        let text = line.trim_end().to_owned();
        if !text.is_empty()
            && let Ok(mut tail) = tail.lock()
        {
            if tail.len() == TAIL_LINES {
                tail.pop_front();
            }
            tail.push_back(text);
        }
        line.clear();
    }
}

/// The lines that name a cause of a helper that ended.
const DRIVER_MISSING: &[&str] = &[
    "classnotfoundexception",
    "no suitable driver",
    "could not find or load main class",
];
const PASSWORD_REFUSED: &[&str] = &[
    "password authentication failed",
    "access denied for user",
    "invalid username/password",
    "login failed",
    "28p01",
    "28000",
];
const SERVER_ABSENT: &[&str] = &[
    "connection refused",
    "connect timed out",
    "connection timed out",
    "unknownhost",
    "no route to host",
    "the connection attempt failed",
];
const ACCESS_REFUSED: &[&str] = &[
    "accessdenied",
    "access is denied",
    "permission denied",
    "read-only file system",
];

/// The reason a helper ended, from the last lines of its error stream: a missing driver
/// class, a refused password, a server that did not answer or a file it could not open.
/// `None` when the stream said nothing.
pub fn explain_output(lines: &[String]) -> Option<String> {
    let last = lines.iter().rev().find(|line| !line.trim().is_empty())?;
    let causes = [
        (DRIVER_MISSING, "a class the helper needs was not found"),
        (
            PASSWORD_REFUSED,
            "the server refused the user or the password",
        ),
        (SERVER_ABSENT, "the database server did not answer"),
        (ACCESS_REFUSED, "the helper could not open its files"),
    ];
    for (markers, cause) in causes {
        if let Some(line) = lines.iter().find(|line| contains(line, markers)) {
            return Some(format!("The helper ended: {cause}: {}", shorten(line)));
        }
    }
    Some(format!("The helper ended: {}", shorten(last)))
}

fn contains(line: &str, markers: &[&str]) -> bool {
    let lower = line.to_ascii_lowercase();
    markers.iter().any(|marker| lower.contains(marker))
}

/// A line of an error stream cut to the part that belongs in the window.
fn shorten(line: &str) -> String {
    let line = line.trim();
    if line.chars().count() <= 240 {
        return line.to_owned();
    }
    let cut: String = line.chars().take(240).collect();
    format!("{cut}…")
}

#[cfg(test)]
mod tests {
    use super::explain_output;

    fn lines(text: &str) -> Vec<String> {
        text.lines().map(str::to_owned).collect()
    }

    #[test]
    fn a_missing_driver_class_is_the_reason() {
        let reason = explain_output(&lines(
            "ClassNotFoundException: org.postgresql.Driver\n\
             \tat jdk.internal.loader.BuiltinClassLoader.loadClass(BuiltinClassLoader)",
        ));
        let reason = reason.expect("the stream names a reason");
        assert!(
            reason.contains("a class the helper needs was not found"),
            "{reason}"
        );
        assert!(reason.contains("org.postgresql.Driver"), "{reason}");
    }

    #[test]
    fn a_refused_password_is_the_reason() {
        let reason = explain_output(&lines(
            "org.postgresql.util.PSQLException: FATAL: password authentication failed for user \"pet\"",
        ))
        .expect("the stream names a reason");
        assert!(
            reason.contains("the server refused the user or the password"),
            "{reason}"
        );
    }

    #[test]
    fn a_server_that_does_not_answer_is_the_reason() {
        let reason = explain_output(&lines(
            "ConnectException: Connection refused (Connection refused)",
        ))
        .expect("the stream names a reason");
        assert!(
            reason.contains("the database server did not answer"),
            "{reason}"
        );
    }

    #[test]
    fn an_unrelated_line_is_shown_as_it_is() {
        let reason = explain_output(&lines("first\nError: could not create the virtual machine"))
            .expect("the stream names a reason");
        assert!(
            reason.contains("could not create the virtual machine"),
            "{reason}"
        );
        assert!(explain_output(&[]).is_none());
    }
}
