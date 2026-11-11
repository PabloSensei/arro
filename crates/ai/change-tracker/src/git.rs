use std::ffi::OsStr;
use std::io;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Runs the `git` of the machine. A git repository is plain data here, not a build tool of ours.
#[derive(Clone, Debug)]
pub struct GitRunner {
    program: PathBuf,
}

impl GitRunner {
    pub fn new(program: PathBuf) -> Self {
        Self { program }
    }

    /// `git.exe` on the given PATH value.
    pub fn find(path: &OsStr) -> Option<Self> {
        let candidates = std::env::split_paths(path).map(|directory| directory.join("git.exe"));
        let program = candidates.into_iter().find(|file| file.is_file())?;
        Some(Self::new(program))
    }

    /// Paths past 260 characters fail in git for Windows unless it is told otherwise.
    pub fn command(&self, cwd: &Path) -> Command {
        let mut command = Command::new(&self.program);
        command
            .current_dir(cwd)
            .creation_flags(CREATE_NO_WINDOW)
            .args(["-c", "core.longpaths=true"]);
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
}
