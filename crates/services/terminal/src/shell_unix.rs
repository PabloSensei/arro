//! The shell of a new terminal when the user names none: the shell of the account, else
//! the one every Unix has. The `+` menu lists the shells of the account and of PATH.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::shells::{Shell, find_in, push_unique};

/// The command line of the shell found.
pub fn default_shell() -> Option<String> {
    available_shells()
        .into_iter()
        .next()
        .map(|shell| shell.command_line)
}

/// Every shell this machine has, in the order the menu shows them.
pub fn available_shells() -> Vec<Shell> {
    let account = std::env::var_os("SHELL");
    let path = std::env::var_os("PATH").unwrap_or_default();
    shells_in(account.as_deref(), &path)
}

/// The shells of `account` and of `path`. A shell and a PATH of their own make this
/// testable.
pub fn shells_in(account: Option<&OsStr>, path: &OsStr) -> Vec<Shell> {
    let mut shells = Vec::new();
    if let Some(shell) = account.map(PathBuf::from).filter(|shell| shell.is_file()) {
        let name = shell_name(&shell);
        push_unique(&mut shells, Shell::new(&name, &shell));
    }
    for program in ["bash", "zsh", "fish", "sh"] {
        if let Some(found) = find_in(path, program) {
            let name = shell_name(&found);
            push_unique(&mut shells, Shell::new(&name, &found));
        }
    }
    // The shell of every Unix is there even when PATH does not name it.
    let sh = PathBuf::from("/bin/sh");
    if sh.is_file() {
        push_unique(&mut shells, Shell::new("sh", &sh));
    }
    shells
}

fn shell_name(shell: &Path) -> String {
    shell
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{default_shell, shells_in};
    use std::ffi::OsString;

    #[test]
    fn a_shell_is_quoted_for_the_command_line() {
        let shell = default_shell().expect("a Unix has a shell");
        assert!(shell.starts_with('"'), "{shell}");
        assert!(shell.ends_with('"'), "{shell}");
    }

    /// A PATH of its own, with the files `names` in the first folder.
    fn path_with(name: &str, names: &[&str]) -> OsString {
        let folder = test_support::scratch(name);
        for name in names {
            let file = folder.join(name);
            std::fs::write(&file, "#!/bin/sh\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut permissions = std::fs::metadata(&file).unwrap().permissions();
                permissions.set_mode(0o755);
                std::fs::set_permissions(&file, permissions).unwrap();
            }
        }
        folder.as_os_str().to_os_string()
    }

    #[test]
    fn the_shells_of_the_account_and_of_a_path_keep_the_order_of_the_menu() {
        let path = path_with("shells-unix-order", &["bash", "fish"]);
        let shells = shells_in(None, &path);
        let names: Vec<&str> = shells.iter().map(|shell| shell.name.as_str()).collect();
        assert_eq!(names[..2], ["bash", "fish"]);
        assert!(names.contains(&"sh"));
    }

    #[test]
    fn the_shell_of_the_account_comes_first() {
        let path = path_with("shells-unix-account", &["bash"]);
        let folder = path_with("shells-unix-zsh", &["zsh"]);
        // The account names the file of the shell, not the folder it lies in.
        let account = std::path::Path::new(&folder).join("zsh");
        let shells = shells_in(Some(account.as_os_str()), &path);
        assert_eq!(shells[0].name, "zsh");
        assert_eq!(shells[0].command_line, format!("\"{}\"", account.display()));
    }
}
