use std::collections::HashMap;
use std::fs::File;
use std::future::Future;
use std::io::{self, Error, ErrorKind, Read, Write};
use std::path::PathBuf;
use std::pin::Pin;
use std::thread;
use std::time::Duration;

use proc_sup::{Process, ProcessSpec};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, DuplexStream, duplex};
use tokio::runtime::Handle;
use tokio::time::sleep;

/// Holds a burst between the blocking pipes and the async client.
const BUFFER: usize = 64 * 1024;

const EXIT_POLL: Duration = Duration::from_millis(100);

/// The command line of a server. The server's own stderr goes to `log`.
#[derive(Clone, Debug, Default)]
pub struct Plan {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    pub directory: Option<PathBuf>,
    pub log: Option<PathBuf>,
    /// A role for the memory table, such as `Language server`.
    pub label: Option<&'static str>,
}

pub type WaitFuture<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

/// A running server. Dropping the handle must end the process.
pub trait ProcessHandle: Send {
    fn id(&self) -> Option<u32>;

    /// Resolves when the process has ended.
    fn wait(&mut self) -> WaitFuture<'_>;

    /// The exit code once the process has ended, when the platform tells it.
    fn exit_code(&mut self) -> Option<i32> {
        None
    }

    /// Ends the process now. Harmless if it has ended already.
    fn kill(&mut self);
}

pub struct ServerProcess {
    pub stdin: Box<dyn AsyncWrite + Send + Unpin>,
    pub stdout: Box<dyn AsyncRead + Send + Unpin>,
    pub handle: Box<dyn ProcessHandle>,
}

pub trait Launcher: Send + Sync {
    fn launch(&self, plan: &Plan) -> io::Result<ServerProcess>;
}

/// Starts the server in a Job Object, so that it cannot outlive the IDE even
/// when the IDE crashes. Must be called on a tokio runtime.
pub struct SupervisedLauncher;

struct Supervised(Process);

impl ProcessHandle for Supervised {
    fn id(&self) -> Option<u32> {
        Some(self.0.pid())
    }

    fn wait(&mut self) -> WaitFuture<'_> {
        Box::pin(async {
            // An error here means there is no process to wait for.
            while matches!(self.0.try_wait(), Ok(None)) {
                sleep(EXIT_POLL).await;
            }
        })
    }

    fn kill(&mut self) {
        // Fails only when the process is gone already.
        let _ = self.0.stop();
    }

    fn exit_code(&mut self) -> Option<i32> {
        self.0.try_wait().ok().flatten()?.code()
    }
}

fn pump(name: &str, work: impl FnOnce() + Send + 'static) -> io::Result<()> {
    thread::Builder::new()
        .name(name.to_owned())
        .spawn(work)
        .map(drop)
}

/// Process pipes block, so each has a thread. Each ends with its pipe.
fn read_pipe(runtime: Handle, mut pipe: impl Read, mut stream: DuplexStream) {
    let mut buffer = vec![0; BUFFER];
    while let Ok(count @ 1..) = pipe.read(&mut buffer) {
        if runtime
            .block_on(stream.write_all(&buffer[..count]))
            .is_err()
        {
            return;
        }
    }
}

fn write_pipe(runtime: Handle, mut stream: DuplexStream, mut pipe: impl Write) {
    let mut buffer = vec![0; BUFFER];
    while let Ok(count @ 1..) = runtime.block_on(stream.read(&mut buffer)) {
        if pipe
            .write_all(&buffer[..count])
            .and_then(|()| pipe.flush())
            .is_err()
        {
            return;
        }
    }
}

impl Launcher for SupervisedLauncher {
    fn launch(&self, plan: &Plan) -> io::Result<ServerProcess> {
        let runtime = Handle::try_current().map_err(Error::other)?;
        let log = plan.log.as_ref().map(File::create).transpose()?;

        let mut spec = ProcessSpec::new(&plan.program).args(&plan.args);
        if let Some(label) = plan.label {
            spec = spec.label(label);
        }
        for (name, value) in &plan.env {
            spec = spec.env(name, value);
        }
        if let Some(directory) = &plan.directory {
            spec = spec.cwd(directory);
        }
        let mut process = spec.start()?;

        let missing = || Error::new(ErrorKind::BrokenPipe, "server pipes are missing");
        let child_stdin = process.take_stdin().ok_or_else(missing)?;
        let child_stdout = process.take_stdout().ok_or_else(missing)?;
        let mut child_stderr = process.take_stderr().ok_or_else(missing)?;

        let (stdin, stdin_end) = duplex(BUFFER);
        let (stdout, stdout_end) = duplex(BUFFER);
        let writer = runtime.clone();
        pump("server-stdin", move || {
            write_pipe(writer, stdin_end, child_stdin)
        })?;
        pump("server-stdout", move || {
            read_pipe(runtime, child_stdout, stdout_end)
        })?;
        // Unread stderr would stop the server once the pipe is full.
        pump("server-stderr", move || match log {
            Some(mut log) => drop(io::copy(&mut child_stderr, &mut log)),
            None => drop(io::copy(&mut child_stderr, &mut io::sink())),
        })?;

        Ok(ServerProcess {
            stdin: Box::new(stdin),
            stdout: Box::new(stdout),
            handle: Box::new(Supervised(process)),
        })
    }
}
