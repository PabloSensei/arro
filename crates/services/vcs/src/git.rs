use std::ffi::{OsStr, OsString};
use std::io::{self, Write};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use base::perf;
use platform::NO_WINDOW;

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
        let mut candidates = std::env::split_paths(path).map(|directory| directory.join("git.exe"));
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
        command
            .current_dir(cwd)
            .creation_flags(NO_WINDOW)
            .args(GLOBAL_ARGS)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .stdin(Stdio::null());
        for (name, value) in &self.env {
            command.env(name, value);
        }
        command
    }

    /// Stdout of a command that has to succeed.
    pub fn checked(mut command: Command) -> io::Result<Output> {
        let output = command.output()?;
        if output.status.success() {
            return Ok(output);
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(io::Error::other(format!("git: {}", stderr.trim())))
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
        let mut attempt = 0;
        let started = Instant::now();
        loop {
            let mut command = self.command(cwd);
            command.args(args.clone());
            match run_once(command, input) {
                Err(error) if is_lock_error(&error) && attempt < LOCK_RETRIES => {
                    attempt += 1;
                    sleep(LOCK_PAUSE);
                }
                result => {
                    perf::observe("git.command", started.elapsed());
                    return result;
                }
            }
        }
    }
}

fn run_once(mut command: Command, input: Option<&[u8]>) -> io::Result<Vec<u8>> {
    let Some(input) = input else {
        return GitRunner::checked(command).map(|output| output.stdout);
    };
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(input)?;
    }
    let output = child.wait_with_output()?;
    if output.status.success() {
        return Ok(output.stdout);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(io::Error::other(format!("git: {}", stderr.trim())))
}

/// `Unable to create '.../index.lock': File exists.` and the same for refs.
fn is_lock_error(error: &io::Error) -> bool {
    let text = error.to_string();
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

    #[test]
    fn git_is_found_on_the_path() {
        let git = GitRunner::from_env().expect("git in PATH");

        let version = git.run(Path::new("."), ["--version"]).unwrap();

        assert!(String::from_utf8_lossy(&version).starts_with("git version"));
    }
}
