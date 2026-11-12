//! The names programs have on this system: the extension of an executable and the
//! wrapper a build tool installs.

use std::path::PathBuf;

/// `.exe` on Windows, empty everywhere else.
pub const EXE_SUFFIX: &str = if cfg!(windows) { ".exe" } else { "" };

/// A program name with the extension of an executable: `tool` becomes `tool.exe` on
/// Windows and stays `tool` on Unix.
pub fn executable(name: &str) -> String {
    format!("{name}{EXE_SUFFIX}")
}

/// A command script: `tool` becomes `tool.cmd` on Windows and stays `tool` on Unix.
pub fn cmd_script(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.cmd")
    } else {
        name.to_owned()
    }
}

/// A batch script: `tool` becomes `tool.bat` on Windows and stays `tool` on Unix.
pub fn bat_script(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.bat")
    } else {
        name.to_owned()
    }
}

/// The home folder of the user: `HOME` on Unix, `USERPROFILE` on Windows. The other
/// variable is still read when the first is missing: a shell on Windows sets `HOME` too.
pub fn home_dir() -> Option<PathBuf> {
    let variable = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(variable)
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::{bat_script, cmd_script, executable};

    #[test]
    fn the_names_carry_the_suffix_of_the_system() {
        let suffix = if cfg!(windows) { ".exe" } else { "" };
        assert_eq!(executable("tool"), format!("tool{suffix}"));
        let command = if cfg!(windows) { "tool.cmd" } else { "tool" };
        assert_eq!(cmd_script("tool"), command);
        let batch = if cfg!(windows) { "tool.bat" } else { "tool" };
        assert_eq!(bat_script("tool"), batch);
    }
}
