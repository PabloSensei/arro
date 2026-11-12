//! The shell of a new terminal when the user names none: the newest PowerShell this
//! machine has, else the one of Windows, else the Git Bash, else the command prompt.

use std::path::{Path, PathBuf};

/// The command line of the shell found, quoted for the console of Windows.
pub fn default_shell() -> Option<String> {
    let mut found = find_in_path("pwsh.exe");
    if found.is_none() {
        found = find_in_path("powershell.exe");
    }
    if found.is_none() {
        found = git_bash();
    }
    if found.is_none() {
        found = std::env::var_os("ComSpec")
            .map(PathBuf::from)
            .filter(|path| path.is_file());
    }
    found.map(|path| command_line(&path))
}

fn command_line(shell: &Path) -> String {
    let quoted = format!("\"{}\"", shell.display());
    let name = shell
        .file_stem()
        .map(|name| name.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if name == "bash" {
        format!("{quoted} --login -i")
    } else {
        quoted
    }
}

fn find_in_path(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|directory| directory.join(program))
        .find(|file| file.is_file())
}

/// The `bash.exe` of a Git for Windows installed in its usual places, which is often
/// not on `PATH`.
fn git_bash() -> Option<PathBuf> {
    let folders = [
        std::env::var_os("ProgramFiles").map(PathBuf::from),
        std::env::var_os("ProgramFiles(x86)").map(PathBuf::from),
        std::env::var_os("LOCALAPPDATA").map(|folder| PathBuf::from(folder).join("Programs")),
    ];
    folders.into_iter().flatten().find_map(|folder| {
        let bash = folder.join("Git").join("bin").join("bash.exe");
        bash.is_file().then_some(bash)
    })
}

#[cfg(test)]
mod tests {
    use super::command_line;
    use std::path::Path;

    #[test]
    fn a_shell_path_is_quoted_and_bash_gets_its_login_flags() {
        assert_eq!(
            command_line(Path::new(r"C:\Program Files\PowerShell\7\pwsh.exe")),
            r#""C:\Program Files\PowerShell\7\pwsh.exe""#
        );
        assert!(
            command_line(Path::new(r"C:\Program Files\Git\bin\bash.exe")).ends_with("--login -i")
        );
    }
}
