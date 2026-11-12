//! The shell of a new terminal when the user names none: the shell of the account, else
//! the one every Unix has.

use std::path::PathBuf;

/// The command line of the shell found.
pub fn default_shell() -> Option<String> {
    let shell = std::env::var_os("SHELL")
        .map(PathBuf::from)
        .filter(|path| path.is_file())
        .unwrap_or_else(|| PathBuf::from("/bin/bash"));
    shell.is_file().then(|| format!("\"{}\"", shell.display()))
}

#[cfg(test)]
mod tests {
    use super::default_shell;

    #[test]
    fn a_shell_is_quoted_for_the_command_line() {
        let shell = default_shell().expect("a Unix has a shell");
        assert!(shell.starts_with('"'), "{shell}");
        assert!(shell.ends_with('"'), "{shell}");
    }
}
