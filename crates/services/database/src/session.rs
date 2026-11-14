//! One helper process and the events it sends.
//!
//! The helper reads a request per line from its standard input and writes an answer per
//! line to its standard output. The process is owned: it ends with the session, with the
//! window and with the IDE, even when it no longer answers.

use std::ffi::OsString;
use std::io::{self, BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::ChildStdin;
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread;

use proc_sup::{Process, ProcessSpec};

use crate::protocol::{Command, Event};

/// The requests a window may have written but not read yet. A helper that floods the
/// channel waits instead of growing without a limit.
const QUEUE: usize = 1024;

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
        if let Some(stderr) = process.take_stderr() {
            // A driver that complains on its error stream must not fill the pipe and stop:
            // the text is not part of the protocol, the events say what happened.
            thread::spawn(move || {
                let mut stderr = BufReader::new(stderr);
                let mut line = String::new();
                while stderr.read_line(&mut line).unwrap_or(0) > 0 {
                    line.clear();
                }
            });
        }
        let (sender, events) = sync_channel(QUEUE);
        if let Some(stdout) = stdout {
            thread::Builder::new()
                .name("database-helper".into())
                .spawn(move || read_events(stdout, sender))?;
        } else {
            let _ = sender.try_send(Event::Exited { code: None });
        }
        Ok(Self {
            events,
            input: Arc::new(Mutex::new(stdin)),
            process: Arc::new(Mutex::new(Some(process))),
            process_id,
        })
    }

    pub fn process_id(&self) -> Option<u32> {
        self.process_id
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

fn read_events(stdout: impl io::Read, sender: SyncSender<Event>) {
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
    let _ = sender.try_send(Event::Exited { code: None });
}
