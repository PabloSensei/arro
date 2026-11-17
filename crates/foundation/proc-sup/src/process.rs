use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::{Arc, OnceLock};
use std::thread::sleep;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::process::CommandExt;
#[cfg(windows)]
use std::os::windows::process::CommandExt;

use base::perf;
use platform::{Job, ProcessTree, allow_ctrl_c_in_children};
#[cfg(windows)]
use platform::{NO_WINDOW, SUSPENDED_NO_WINDOW, resume_process};

/// Exit code of a tree ended by [`Process::stop`], the one `taskkill` uses.
#[cfg(windows)]
const STOPPED: u32 = 1;
/// How often [`Process::shut_down`] looks whether the process has gone.
const POLL: Duration = Duration::from_millis(20);
/// The helper that delivers console signals, next to the IDE executable.
#[cfg(windows)]
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
        let mut command = Command::new(&self.program);
        command
            .args(&self.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            let flags = match self.class {
                ProcessClass::Owned => SUSPENDED_NO_WINDOW,
                ProcessClass::Client => NO_WINDOW,
            };
            command.creation_flags(flags);
        }
        #[cfg(unix)]
        if self.class == ProcessClass::Owned {
            // A group of its own is what [`Job`] signals and what keeps a shell and its
            // children from surviving their owner.
            command.process_group(0);
        }
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
    /// Shared with the reaper thread, which keeps the job alive until its last round: the
    /// programs in it must not go before the cleanup of the ones outside it is over.
    job: Option<Arc<Job>>,
    /// For an owned process: the programs outside the job, which the job alone would leave
    /// running.
    tree: Option<ProcessTree>,
    /// Whether the cleanup of the programs outside the job has run already.
    settled: bool,
    #[cfg_attr(unix, allow(dead_code))]
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
            ProcessClass::Owned => Some(Arc::new(Job::new()?)),
            ProcessClass::Client => None,
        };
        let started = Instant::now();
        let mut child = spec.command().spawn()?;
        // Read before the first instruction of the child: a later process that takes its id
        // is not the one the tree belongs to.
        let tree = job.as_ref().map(|_| ProcessTree::new(child.id()));
        if let Some(job) = &job {
            // In the job before its first instruction, so that no grandchild can slip out.
            #[cfg(windows)]
            let resumed = job.assign(&child).and_then(|()| resume_process(&child));
            #[cfg(unix)]
            let resumed = job.assign(child.id());
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
            tree,
            settled: false,
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
    ///
    /// A process that has just exited takes the programs it started outside the job with
    /// it: they are found by the parent id of the root, which the snapshot keeps.
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        let status = self.child.try_wait()?;
        if status.is_some() {
            self.end_escaped();
        }
        Ok(status)
    }

    /// Blocks until the process exits, so this belongs on a dedicated thread. Take stdout
    /// and stderr first and read them there: a full pipe stops the child forever. Wait for
    /// this and not for the end of those pipes: a server the child left behind may hold
    /// them open for hours. The programs the process started outside the job end here, but
    /// the ones in the job stay until the job goes.
    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        let status = self.child.wait()?;
        self.end_escaped();
        Ok(status)
    }

    /// Finds the programs the process started outside the job and pulls them into it, so
    /// that the job takes them along later. A shell such as PowerShell 7 starts native
    /// commands outside the job. Window programs are left alone, and so is everything they
    /// start. Returns how many programs were added.
    pub(crate) fn adopt(&self) -> usize {
        match (&self.tree, &self.job) {
            (Some(tree), Some(job)) => tree.adopt(job),
            _ => 0,
        }
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
        #[cfg(unix)]
        {
            let signal = match signal {
                "--ctrl-break" => platform::ConsoleSignal::CtrlBreak,
                _ => platform::ConsoleSignal::CtrlC,
            };
            platform::signal_console_of(self.pid(), signal)
        }
        #[cfg(windows)]
        {
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
        let Some(job) = &self.job else {
            // Already gone is what was asked for.
            return self.child.kill().or(Ok(()));
        };
        // The job takes the process and what it started inside it at once; the programs
        // it left outside are found by its parent id, which the snapshot keeps.
        #[cfg(windows)]
        let ended = job.terminate(STOPPED);
        #[cfg(unix)]
        let ended = job.terminate(1);
        // A process that has exited already had its escaped programs cleaned by `wait` or
        // `try_wait`: the job is what is left to end here.
        if !self.settled {
            self.settled = true;
            if let Some(tree) = &self.tree {
                tree.kill(job);
            }
        }
        ended
    }

    /// Ends the programs the process started outside the job, once and only after the
    /// process has exited. The job keeps the ones inside it until it goes: whoever waits
    /// for the process may still want them.
    fn end_escaped(&mut self) {
        if self.settled {
            return;
        }
        self.settled = true;
        if let (Some(tree), Some(job)) = (&self.tree, &self.job) {
            tree.kill_escaped(job);
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
        // An owned tree ends with its job, but not everything the process started is in
        // the job: the programs outside it are found by their parent id and ended here.
        // A client has nothing else that would end it.
        if self.job.is_some() {
            let _ = self.end_tree();
        } else {
            let _ = self.child.kill();
        }
    }
}

/// A process whose child is outside its job. On Windows a program keeps the job it was born
/// with, and a child of a job member starts outside the job only through a broker of the
/// system, so the tests make the state that matters: the program starts in the moment
/// before the job holds the root, and the job takes the root only afterwards.
#[cfg(all(test, windows))]
mod tests {
    use super::{Process, ProcessTree};
    use platform::{
        Job, NO_WINDOW, ProcessWaiter, process_created, process_table, terminate_process,
    };
    use std::io::Write;
    use std::os::windows::process::CommandExt;
    use std::path::PathBuf;
    use std::process::{Command, Stdio};
    use std::sync::Arc;
    use std::thread::sleep;
    use std::time::{Duration, Instant};

    const PATIENCE: Duration = Duration::from_secs(20);
    /// Long enough that a caller that slept one round is seen at once, instead of a
    /// threshold that depends on the machine.
    const TEST_PAUSE: Duration = Duration::from_secs(30);
    /// How long a call may take and still count as not sleeping a cleanup round.
    const QUICK: Duration = Duration::from_secs(5);

    /// The window program of this crate. Built next to the test binary, which has no way to
    /// name a binary of its own package at run time.
    fn window_program() -> PathBuf {
        let test = std::env::current_exe().expect("the test binary");
        let directory = test
            .parent()
            .and_then(|deps| deps.parent())
            .expect("the directory of the test binary");
        directory.join("gui-probe.exe")
    }

    /// A shell that has started `image` with `command`, and only then put in a job of its
    /// own: the program stays outside the job, and the tree has to find it by the parent id.
    /// Returns the process and the id of the program.
    fn owned_with_escaped(command: &str, image: &str) -> (Process, u32) {
        let mut child = Command::new("cmd.exe")
            .args(["/d", "/q"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(NO_WINDOW)
            .spawn()
            .expect("start the shell");
        let pid = child.id();
        let stdin = child.stdin.as_mut().expect("the input of the shell");
        stdin
            .write_all(format!("{command}\r\n").as_bytes())
            .expect("write to the shell");
        stdin.flush().expect("flush the input of the shell");
        // The program is born before the job takes the shell: a later child inherits it.
        let program = child_of(pid, image);
        let job = Job::new().expect("a job");
        job.assign(&child).expect("assign the shell");
        let tree = ProcessTree::new(pid).with_pause(TEST_PAUSE);
        let process = Process {
            child,
            job: Some(Arc::new(job)),
            tree: Some(tree),
            settled: false,
            ctrl_helper: None,
        };
        (process, program)
    }

    /// The live child of `root` with this image: the id of a dead parent stays in the
    /// snapshot, so a program that outlived its shell is still found.
    fn child_of(root: u32, image: &str) -> u32 {
        let started = Instant::now();
        loop {
            let child = process_table()
                .into_iter()
                .find(|process| {
                    process.parent == Some(root) && process.name.eq_ignore_ascii_case(image)
                })
                .map(|process| process.pid);
            if let Some(child) = child {
                return child;
            }
            assert!(started.elapsed() < PATIENCE, "the shell started no {image}");
            sleep(Duration::from_millis(10));
        }
    }

    fn waiter_of(pid: u32) -> ProcessWaiter {
        ProcessWaiter::open(pid)
            .expect("open the program")
            .expect("the program runs")
    }

    fn ended(waiter: &ProcessWaiter) -> bool {
        // An unreadable answer leaves the process counted as running, which fails the test.
        matches!(waiter.wait(Duration::ZERO), Ok(true))
    }

    /// Ends a program of a test even when the test fails before its last line.
    struct EndOnDrop {
        pid: u32,
        created: Option<u64>,
    }

    impl Drop for EndOnDrop {
        fn drop(&mut self) {
            if let Some(created) = self.created {
                let _ = terminate_process(self.pid, created);
            }
        }
    }

    #[test]
    fn a_console_program_outside_the_job_is_pulled_into_it() {
        let (process, program) = owned_with_escaped("start /b ping -t 127.0.0.1", "PING.EXE");
        let _end = EndOnDrop {
            pid: program,
            created: process_created(program),
        };
        assert!(
            !process.tree().expect("the job").contains(&program),
            "the program is inside the job, so it tests nothing"
        );

        assert_eq!(process.adopt(), 1, "the program outside the job is found");
        assert!(
            process.tree().expect("the job").contains(&program),
            "the program was not pulled into the job"
        );
    }

    #[test]
    fn a_window_program_outside_the_job_is_left_alone() {
        let window = window_program();
        let (process, program) = owned_with_escaped(
            &format!("start \"\" /b \"{}\"", window.display()),
            "gui-probe.exe",
        );
        let _end = EndOnDrop {
            pid: program,
            created: process_created(program),
        };

        assert_eq!(
            process.adopt(),
            0,
            "a window program is not a program of the job"
        );
        drop(process);

        // Its window and everything below it stay alive, and so does this program: nothing
        // of someone else's is ended on a guess.
        sleep(Duration::from_millis(500));
        let waiter = waiter_of(program);
        assert!(
            !ended(&waiter),
            "the owner ended the window program it did not start for itself"
        );
    }

    #[test]
    fn a_console_program_outside_the_job_dies_with_its_owner() {
        let (process, program) = owned_with_escaped("start /b ping -t 127.0.0.1", "PING.EXE");
        let _end = EndOnDrop {
            pid: program,
            created: process_created(program),
        };
        let waiter = waiter_of(program);

        drop(process);

        assert!(
            waiter.wait(PATIENCE).expect("the state of the program"),
            "the program outside the job outlived its owner"
        );
    }

    #[test]
    fn a_console_program_outside_the_job_dies_when_its_owner_exits() {
        let (mut process, program) = owned_with_escaped("start /b ping -t 127.0.0.1", "PING.EXE");
        let _end = EndOnDrop {
            pid: program,
            created: process_created(program),
        };
        let waiter = waiter_of(program);

        // The shell exits while the program it started outside the job keeps running.
        let mut stdin = process.take_stdin().expect("the input of the shell");
        stdin.write_all(b"exit\r\n").expect("write to the shell");
        drop(stdin);
        process.wait().expect("the shell exits");

        assert!(
            waiter.wait(PATIENCE).expect("the state of the program"),
            "the program outside the job outlived its owner"
        );
    }

    #[test]
    fn dropping_an_owner_does_not_wait_for_the_cleanup_rounds() {
        let (process, program) = owned_with_escaped("start /b ping -t 127.0.0.1", "PING.EXE");
        let _end = EndOnDrop {
            pid: program,
            created: process_created(program),
        };
        let waiter = waiter_of(program);

        // One round runs here; the rounds the escaped program may still need belong to the
        // reaper thread, whose pause is far longer than any drop may take.
        let started = Instant::now();
        drop(process);
        let elapsed = started.elapsed();
        assert!(
            elapsed < QUICK,
            "the owner waited for the cleanup rounds: {elapsed:?}"
        );
        assert!(
            waiter.wait(PATIENCE).expect("the state of the program"),
            "the program outside the job outlived its owner"
        );
    }

    #[test]
    fn try_wait_does_not_wait_for_the_cleanup_rounds() {
        let (mut process, program) = owned_with_escaped("start /b ping -t 127.0.0.1", "PING.EXE");
        let _end = EndOnDrop {
            pid: program,
            created: process_created(program),
        };
        let waiter = waiter_of(program);

        let mut stdin = process.take_stdin().expect("the input of the shell");
        stdin.write_all(b"exit\r\n").expect("write to the shell");
        drop(stdin);
        let started = Instant::now();
        let elapsed;
        loop {
            let call = Instant::now();
            if process
                .try_wait()
                .expect("the state of the shell")
                .is_some()
            {
                elapsed = call.elapsed();
                break;
            }
            assert!(
                started.elapsed() < PATIENCE,
                "the shell did not exit in time"
            );
            sleep(Duration::from_millis(10));
        }
        assert!(
            elapsed < QUICK,
            "the exit waited for the cleanup rounds: {elapsed:?}"
        );
        assert!(
            waiter.wait(PATIENCE).expect("the state of the program"),
            "the program outside the job outlived its owner"
        );
    }
}
