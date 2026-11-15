//! The shells a new terminal can start: the name the menu shows and the command line
//! that starts it. Which shells exist is the business of the platform files.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// One shell of the machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shell {
    /// The name the user sees, such as `PowerShell`.
    pub name: String,
    /// The command line of the program, quoted and with its flags.
    pub command_line: String,
}

impl Shell {
    pub(crate) fn new(name: &str, program: &Path) -> Self {
        Self {
            name: name.to_string(),
            command_line: quote(program),
        }
    }
}

/// `"<path>"`, the form every shell accepts on the command line of its console.
pub(crate) fn quote(program: &Path) -> String {
    format!("\"{}\"", program.display())
}

/// The first file named `program` in a folder of `path`.
pub(crate) fn find_in(path: &OsStr, program: &str) -> Option<PathBuf> {
    std::env::split_paths(path)
        .map(|directory| directory.join(program))
        .find(|file| file.is_file())
}

/// Adds `shell` unless the same command line is in the list already.
pub(crate) fn push_unique(shells: &mut Vec<Shell>, shell: Shell) {
    if shells
        .iter()
        .any(|known| known.command_line == shell.command_line)
    {
        return;
    }
    shells.push(shell);
}
