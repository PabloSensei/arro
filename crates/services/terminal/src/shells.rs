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
    /// The shells of Windows name their flags in the platform file.
    #[cfg(windows)]
    pub(crate) fn new(name: &str, program: &Path) -> Self {
        Self::with_arguments(name, program, &[])
    }

    /// A shell of Unix, started as a login shell when its program knows the flag: `zsh`,
    /// `bash` and `fish` then read `/etc/zprofile` and `path_helper` on macOS, and the
    /// profile of the account on Linux, so that the `PATH` of the user reaches the
    /// terminal.
    #[cfg(unix)]
    pub(crate) fn login(name: &str, program: &Path) -> Self {
        Self::with_arguments(name, program, login_arguments(program))
    }

    fn with_arguments(name: &str, program: &Path, arguments: &[&str]) -> Self {
        let mut command_line = quote(program);
        for argument in arguments {
            command_line.push(' ');
            command_line.push_str(argument);
        }
        Self {
            name: name.to_string(),
            command_line,
        }
    }
}

/// The arguments that make a shell of Unix a login shell. A shell the IDE does not know
/// gets none: its flags are not ours to guess.
#[cfg(any(unix, test))]
pub(crate) fn login_arguments(program: &Path) -> &'static [&'static str] {
    match program
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
    {
        "zsh" | "bash" | "fish" => &["-l"],
        _ => &[],
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

#[cfg(test)]
mod tests {
    use super::login_arguments;
    use std::path::Path;

    /// zsh, bash and fish read the login files of the account; a shell the IDE does not
    /// know keeps the command line it has. The paths are Unix ones on every runner.
    #[test]
    fn a_known_shell_of_unix_starts_as_a_login_shell() {
        for shell in ["/bin/zsh", "/bin/bash", "/usr/local/bin/fish"] {
            assert_eq!(
                login_arguments(Path::new(shell)),
                ["-l"],
                "the flags of {shell}"
            );
        }
        for shell in ["/bin/sh", "/bin/tcsh", "/bin/csh", "/usr/local/bin/pwsh"] {
            assert!(
                login_arguments(Path::new(shell)).is_empty(),
                "{shell} has no flag of the IDE"
            );
        }
    }
}
