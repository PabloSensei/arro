use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::{self, Write};
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use base::perf;
use base::program::executable;
#[cfg(windows)]
use platform::NO_WINDOW;

use crate::console::{CommandRecord, command_console};

/// Before the subcommand on every call. Reads must not take `index.lock`, or a harness
/// that runs `git add` at the same moment fails. Paths come unquoted and in UTF-8.
const GLOBAL_ARGS: &[&str] = &[
    "--no-optional-locks",
    "-c",
    "core.longpaths=true",
    "-c",
    "core.quotepath=false",
    "-c",
    "color.ui=false",
];
/// Another git holds the lock for the length of one command, a short wait is enough.
const LOCK_RETRIES: u32 = 40;
const LOCK_PAUSE: Duration = Duration::from_millis(50);

/// Runs the `git` of the machine. Never asks in a terminal: a question without an answer
/// would hang the call, so prompts go to [`crate::AskpassServer`] or fail.
#[derive(Clone, Debug)]
pub struct GitRunner {
    program: PathBuf,
    env: Vec<(OsString, OsString)>,
}

impl GitRunner {
    pub fn new(program: PathBuf) -> Self {
        Self {
            program,
            env: Vec::new(),
        }
    }

    /// `git.exe` on the given PATH value.
    pub fn find(path: &OsStr) -> Option<Self> {
        let name = executable("git");
        let mut candidates = std::env::split_paths(path).map(|directory| directory.join(&name));
        let program = candidates.find(|file| file.is_file())?;
        Some(Self::new(program))
    }

    /// `git.exe` on the PATH of this process.
    pub fn from_env() -> Option<Self> {
        Self::find(&std::env::var_os("PATH")?)
    }

    /// Added to the environment of every call, like the askpass variables.
    pub fn with_env(mut self, vars: impl IntoIterator<Item = (OsString, OsString)>) -> Self {
        self.env.extend(vars);
        self
    }

    pub fn command(&self, cwd: &Path) -> Command {
        // A `.cmd` path of the user also starts here: the standard library runs it
        // through the command interpreter and refuses arguments it cannot quote safely.
        let mut command = Command::new(&self.program);
        command.current_dir(cwd).args(GLOBAL_ARGS);
        #[cfg(windows)]
        command.creation_flags(NO_WINDOW);
        command
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .stdin(Stdio::null());
        for (name, value) in &self.env {
            command.env(name, value);
        }
        command
    }

    /// Stdout of a command that has to succeed.
    pub fn checked(command: Command) -> io::Result<Output> {
        Self::checked_with_input(command, None)
    }

    /// Stdout of a command that has to succeed, with `input` on its stdin.
    pub fn checked_with_input(command: Command, input: Option<&[u8]>) -> io::Result<Output> {
        let started = Instant::now();
        let directory = command
            .get_current_dir()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        let args: Vec<OsString> = command.get_args().map(OsStr::to_owned).collect();
        let output = run_once(command, input);
        let duration = started.elapsed();
        let id = command_console().record(match &output {
            Ok(output) => CommandRecord::new(
                &directory,
                without_globals(&args),
                output.status.code(),
                duration,
                &output.stdout,
                &output.stderr,
            ),
            Err(error) => CommandRecord::new(
                &directory,
                without_globals(&args),
                None,
                duration,
                b"",
                error.to_string().as_bytes(),
            ),
        });
        let output = output?;
        if output.status.success() {
            return Ok(output);
        }
        Err(command_failed(&output, id))
    }

    /// Stdout of `git args` in `cwd`. A lock held by another git is waited out.
    pub fn run<I, S>(&self, cwd: &Path, args: I) -> io::Result<Vec<u8>>
    where
        I: IntoIterator<Item = S> + Clone,
        S: AsRef<OsStr>,
    {
        self.run_with_input(cwd, args, None)
    }

    pub fn run_with_input<I, S>(
        &self,
        cwd: &Path,
        args: I,
        input: Option<&[u8]>,
    ) -> io::Result<Vec<u8>>
    where
        I: IntoIterator<Item = S> + Clone,
        S: AsRef<OsStr>,
    {
        let args: Vec<OsString> = args
            .into_iter()
            .map(|arg| arg.as_ref().to_owned())
            .collect();
        let mut attempt = 0;
        let started = Instant::now();
        loop {
            let mut command = self.command(cwd);
            command.args(&args);
            match run_once(command, input) {
                Err(error) if is_lock_error(&error) && attempt < LOCK_RETRIES => {
                    attempt += 1;
                    sleep(LOCK_PAUSE);
                }
                Ok(output) if output_locked(&output) && attempt < LOCK_RETRIES => {
                    attempt += 1;
                    sleep(LOCK_PAUSE);
                }
                result => {
                    let duration = started.elapsed();
                    perf::observe("git.command", duration);
                    return journal(cwd, &args, duration, result);
                }
            }
        }
    }
}

/// The arguments without the options the runner puts before every subcommand: the Console
/// shows the command the user asked for.
fn without_globals(args: &[OsString]) -> &[OsString] {
    let globals = GLOBAL_ARGS.len();
    let prefixed = args.len() > globals
        && args
            .iter()
            .take(globals)
            .enumerate()
            .all(|(index, arg)| arg.as_os_str() == OsStr::new(GLOBAL_ARGS[index]));
    if prefixed { &args[globals..] } else { args }
}

/// Writes the command into the journal of the process and answers with the stdout of a
/// command that succeeded, or with the error git wrote.
fn journal(
    cwd: &Path,
    args: &[OsString],
    duration: Duration,
    result: io::Result<Output>,
) -> io::Result<Vec<u8>> {
    let id = match &result {
        Ok(output) => command_console().record(CommandRecord::new(
            cwd,
            args,
            output.status.code(),
            duration,
            &output.stdout,
            &output.stderr,
        )),
        Err(error) => {
            let message = error.to_string();
            command_console().record(CommandRecord::new(
                cwd,
                args,
                None,
                duration,
                b"",
                message.as_bytes(),
            ))
        }
    };
    match result {
        Ok(output) if output.status.success() => Ok(output.stdout),
        Ok(output) => Err(command_failed(&output, id)),
        Err(error) => Err(error),
    }
}

fn run_once(mut command: Command, input: Option<&[u8]>) -> io::Result<Output> {
    let Some(input) = input else {
        return command.output();
    };
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(input)?;
    }
    child.wait_with_output()
}

/// The error of a failed command, with the number the journal gave it, so the Console
/// opens on that command and not on a command of the ide that failed at the same time.
#[derive(Debug)]
struct CommandFailure {
    message: String,
    id: u64,
}

impl fmt::Display for CommandFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CommandFailure {}

/// The number the journal gave the command that failed, when `error` is what a git
/// command of this runner left.
pub fn command_id(error: &io::Error) -> Option<u64> {
    let inner: &(dyn std::error::Error + 'static) = error.get_ref()?;
    inner
        .downcast_ref::<CommandFailure>()
        .map(|failure| failure.id)
}

/// The error of a command git ran and failed, as the window reads it: `git: <stderr>`.
fn command_failed(output: &Output, id: u64) -> io::Error {
    let stderr = String::from_utf8_lossy(&output.stderr);
    io::Error::other(CommandFailure {
        message: format!("git: {}", stderr.trim()),
        id,
    })
}

/// `Unable to create '.../index.lock': File exists.` and the same for refs.
fn is_lock_error(error: &io::Error) -> bool {
    is_lock_text(&error.to_string())
}

/// A failed attempt on a lock another git holds: git answered with a non-zero code and the
/// text of [`is_lock_error`] on stderr. The runner waits such a lock out and tries again.
fn locked_answer(success: bool, stderr: &[u8]) -> bool {
    !success && is_lock_text(&String::from_utf8_lossy(stderr))
}

fn output_locked(output: &Output) -> bool {
    locked_answer(output.status.success(), &output.stderr)
}

fn is_lock_text(text: &str) -> bool {
    text.contains(".lock") && text.contains("File exists")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lock_held_by_another_git_is_recognized() {
        let text = "git: fatal: Unable to create 'C:/r/.git/index.lock': File exists.";

        assert!(is_lock_error(&io::Error::other(text)));
        assert!(!is_lock_error(&io::Error::other(
            "git: fatal: not a git repository"
        )));
    }

    /// `git` answers a non-zero code without an `io::Error`, so the runner decides the
    /// retry on the answer itself: the code and stderr, without running a command.
    fn answer(success: bool, stderr: &str) -> bool {
        locked_answer(success, stderr.as_bytes())
    }

    #[test]
    fn a_locked_attempt_is_retried_by_the_answer_of_git() {
        assert!(
            answer(
                false,
                "fatal: Unable to create 'C:/r/.git/index.lock': File exists."
            ),
            "another git holds the lock"
        );

        assert!(
            !answer(false, "fatal: not a git repository"),
            "a failure about something else is not waited out"
        );
        assert!(
            !answer(true, "fatal: index.lock: File exists"),
            "a command that succeeded is no lock failure"
        );
    }

    #[test]
    fn a_failed_command_names_its_entry_in_the_journal() {
        let git = GitRunner::from_env().expect("git in PATH");

        let error = git
            .run(Path::new("."), ["definitely-not-a-command"])
            .expect_err("git refuses the command");

        assert!(
            error.to_string().starts_with("git: "),
            "the error still reads as git wrote it: {error}"
        );
        let id = command_id(&error).expect("the failure names the command of the journal");
        let entry = crate::command_console()
            .entries()
            .into_iter()
            .find(|entry| entry.id == id)
            .expect("the journal keeps the failed command");
        assert_eq!(entry.code, Some(1));
        assert_eq!(entry.args, ["definitely-not-a-command"]);
    }

    #[test]
    fn git_is_found_on_the_path() {
        let git = GitRunner::from_env().expect("git in PATH");

        let version = git.run(Path::new("."), ["--version"]).unwrap();

        assert!(String::from_utf8_lossy(&version).starts_with("git version"));
    }
}
