use std::ffi::{OsStr, OsString};
use std::io;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::OnceLock;
use std::thread::sleep;
use std::time::{Duration, Instant};

use base::perf;
use platform::{Job, NO_WINDOW, SUSPENDED_NO_WINDOW, allow_ctrl_c_in_children, resume_process};

/// Exit code of a tree ended by [`Process::stop`], the one `taskkill` uses.
const STOPPED: u32 = 1;
/// How often [`Process::shut_down`] looks whether the process has gone.
const POLL: Duration = Duration::from_millis(20);
/// The helper that delivers console signals, next to the IDE executable.
const CTRL_HELPER: &str = "ide-ctrl.exe";

/// How a process relates to its owner.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ProcessClass {
    /// In a job: the whole tree ends with the owner, even if the owner crashes.
    #[default]
    Owned,
    /// Starts a long-lived server that must outlive it, and a job would take the server
    /// along. No job, so only this process ends with the owner. Start the real client
    /// here and not a script: the child of a script stays behind when the script ends.
    Client,
}

/// What to run. The child starts with piped stdin, stdout and stderr and no console window.
#[derive(Clone, Debug)]
pub struct ProcessSpec {
    program: OsString,
    args: Vec<OsString>,
    cwd: Option<PathBuf>,
    env: Vec<(OsString, Option<OsString>)>,
    env_clear: bool,
    class: ProcessClass,
    ctrl_helper: Option<PathBuf>,
    label: Option<&'static str>,
    reads_input: bool,
}

impl ProcessSpec {
    pub fn new(program: impl AsRef<OsStr>) -> Self {
        Self {
            program: program.as_ref().to_owned(),
            args: Vec::new(),
            cwd: None,
            env: Vec::new(),
            env_clear: false,
            class: ProcessClass::default(),
            ctrl_helper: None,
            label: None,
            reads_input: false,
        }
    }

    pub fn arg(mut self, arg: impl AsRef<OsStr>) -> Self {
        self.args.push(arg.as_ref().to_owned());
        self
    }

    pub fn args(mut self, args: impl IntoIterator<Item = impl AsRef<OsStr>>) -> Self {
        let args = args.into_iter().map(|arg| arg.as_ref().to_owned());
        self.args.extend(args);
        self
    }

    pub fn cwd(mut self, cwd: impl AsRef<Path>) -> Self {
        self.cwd = Some(cwd.as_ref().to_owned());
        self
    }

    pub fn env(mut self, name: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> Self {
        let value = Some(value.as_ref().to_owned());
        self.env.push((name.as_ref().to_owned(), value));
        self
    }

    pub fn env_remove(mut self, name: impl AsRef<OsStr>) -> Self {
        self.env.push((name.as_ref().to_owned(), None));
        self
    }

    /// Starts from an empty environment: only what [`Self::env`] sets reaches the child.
    pub fn env_clear(mut self) -> Self {
        self.env_clear = true;
        self
    }

    pub fn class(mut self, class: ProcessClass) -> Self {
        self.class = class;
        self
    }

    /// Where `ide-ctrl` lies, when it is not next to the running executable.
    pub fn ctrl_helper(mut self, path: impl AsRef<Path>) -> Self {
        self.ctrl_helper = Some(path.as_ref().to_owned());
        self
    }

    /// A role for the memory table of the IDE, such as `Terminal` or `Build tool`.
    pub fn label(mut self, label: &'static str) -> Self {
        self.label = Some(label);
        self
    }

    /// The program reads its standard input while it runs, so the owner keeps the pipe
    /// open and may write to it. Without this the owner closes the pipe at once and the
    /// program sees the end of its input.
    pub fn input(mut self) -> Self {
        self.reads_input = true;
        self
    }

    /// Whether the owner writes to the standard input of the program.
    pub fn reads_input(&self) -> bool {
        self.reads_input
    }

    /// The arguments after the program, for a check of a plan before it starts.
    pub fn arguments(&self) -> &[OsString] {
        &self.args
    }

    /// Where the program starts, when the plan sets it.
    pub fn working_dir(&self) -> Option<&Path> {
        self.cwd.as_deref()
    }

    /// The environment the plan sets, in the order it was added.
    pub fn environment(&self) -> &[(OsString, Option<OsString>)] {
        &self.env
    }

    pub fn start(&self) -> io::Result<Process> {
        Process::start(self)
    }

    fn command(&self) -> Command {
        let flags = match self.class {
            ProcessClass::Owned => SUSPENDED_NO_WINDOW,
            ProcessClass::Client => NO_WINDOW,
        };
        let mut command = Command::new(&self.program);
        command
            .args(&self.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .creation_flags(flags);
        if let Some(cwd) = &self.cwd {
            command.current_dir(cwd);
        }
        if self.env_clear {
            command.env_clear();
        }
        for (name, value) in &self.env {
            match value {
                Some(value) => command.env(name, value),
                None => command.env_remove(name),
            };
        }
        command
    }
}

/// A running process and, for [`ProcessClass::Owned`], everything it started. Dropping this
/// ends them, and so does a crash of the IDE: the job goes with the last handle to it.
pub struct Process {
    child: Child,
    job: Option<Job>,
    ctrl_helper: Option<PathBuf>,
}

impl Process {
    fn start(spec: &ProcessSpec) -> io::Result<Self> {
        static CTRL_C: OnceLock<Result<(), String>> = OnceLock::new();
        let allowed =
            CTRL_C.get_or_init(|| allow_ctrl_c_in_children().map_err(|error| error.to_string()));
        if let Err(error) = allowed {
            return Err(io::Error::other(error.clone()));
        }

        let job = match spec.class {
            ProcessClass::Owned => Some(Job::new()?),
            ProcessClass::Client => None,
        };
        let started = Instant::now();
        let mut child = spec.command().spawn()?;
        if let Some(job) = &job {
            // In the job before its first instruction, so that no grandchild can slip out.
            let resumed = job.assign(&child).and_then(|()| resume_process(&child));
            if let Err(error) = resumed {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        }
        if let Some(label) = spec.label {
            platform::label_process(child.id(), label);
        }
        perf::observe("process.spawn", started.elapsed());
        let ctrl_helper = spec.ctrl_helper.clone();
        Ok(Self {
            child,
            job,
            ctrl_helper,
        })
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    pub fn take_stdin(&mut self) -> Option<ChildStdin> {
        self.child.stdin.take()
    }

    pub fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.child.stdout.take()
    }

    pub fn take_stderr(&mut self) -> Option<ChildStderr> {
        self.child.stderr.take()
    }

    /// Exit status of the process itself, if it has exited. Its children may live on.
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }

    /// Blocks until the process exits, so this belongs on a dedicated thread. Take stdout
    /// and stderr first and read them there: a full pipe stops the child forever. Wait for
    /// this and not for the end of those pipes: a server the child left behind may hold
    /// them open for hours.
    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        self.child.wait()
    }

    /// Asks everything on the console to stop. Returns at once, the process may ignore it.
    pub fn interrupt(&self) -> io::Result<()> {
        self.signal("--ctrl-c")
    }

    /// A JVM prints a thread dump to its stdout and keeps running.
    pub fn thread_dump(&self) -> io::Result<()> {
        self.signal("--ctrl-break")
    }

    fn signal(&self, signal: &str) -> io::Result<()> {
        let helper = match &self.ctrl_helper {
            Some(helper) => helper.clone(),
            None => std::env::current_exe()?.with_file_name(CTRL_HELPER),
        };
        let output = Command::new(helper)
            .arg(signal)
            .arg(self.pid().to_string())
            .creation_flags(NO_WINDOW)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()?;
        if !output.status.success() {
            let message = String::from_utf8_lossy(&output.stderr);
            return Err(io::Error::other(message.trim().to_owned()));
        }
        Ok(())
    }

    /// Asks the process to stop, and ends it by force after `grace`. Blocks for that long,
    /// so this belongs on a dedicated thread.
    pub fn shut_down(&mut self, grace: Duration) -> io::Result<ExitStatus> {
        // A process that cannot be asked is simply out of its grace.
        if self.interrupt().is_ok() {
            let start = Instant::now();
            while start.elapsed() < grace {
                if let Some(status) = self.try_wait()? {
                    self.end_tree()?;
                    return Ok(status);
                }
                sleep(POLL);
            }
        }
        self.stop()
    }

    /// Ends everything at once and waits for the process itself.
    pub fn stop(&mut self) -> io::Result<ExitStatus> {
        self.end_tree()?;
        self.child.wait()
    }

    fn end_tree(&mut self) -> io::Result<()> {
        match &self.job {
            Some(job) => job.terminate(STOPPED),
            // Already gone is what was asked for.
            None => self.child.kill().or(Ok(())),
        }
    }

    /// Ids of the processes alive in the tree. A client knows only itself.
    pub fn tree(&self) -> io::Result<Vec<u32>> {
        match &self.job {
            Some(job) => job.process_ids(),
            None => Ok(vec![self.pid()]),
        }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        // An owned tree ends with its job, a client has nothing else that would end it.
        if self.job.is_none() {
            let _ = self.child.kill();
        }
    }
}
