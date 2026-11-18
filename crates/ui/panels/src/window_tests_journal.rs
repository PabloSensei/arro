//! T-0293: the journal of a stand-in CLI. The Services window asks the watch, an action
//! and a live log of one CLI at the same time, so several copies of its script run
//! together. Windows lets one writer open a file at a time, and an `echo` that meets
//! another writer loses its line without a word and without a change of `%errorlevel%`,
//! so a test that waits for that line could never get it back. Every call therefore takes
//! a folder of its own: `mkdir` either makes it or fails, so no two calls write one file.
//!
//! cargo test -p panels window_tests_journal

#![cfg(all(test, windows))]

use std::fs;
use std::path::Path;

/// Makes the folder of the journal and answers the lines that a stand-in `.cmd` script
/// runs before its own work. The lines are separated by `\n`: a script that wants `\r\n`
/// replaces them together with the rest of its text.
///
/// The first copy in one tick of the clock takes `call-<time>`, the next ones add an `x`
/// until `mkdir` makes a folder of their own, so a call never waits for another one and
/// never loses its line. The `echo` has no retry: its `%errorlevel%` stays zero whether it
/// wrote the line or not, and the folder of the call holds no other writer anyway.
pub(crate) fn journal_writer(folder: &Path) -> String {
    fs::create_dir_all(folder).unwrap();
    format!(
        r#"set "journal={folder}"
set "stamp=%TIME%"
set "stamp=%stamp::=%"
set "stamp=%stamp:,=%"
set "stamp=%stamp:.=%"
set "stamp=%stamp: =0%"
:journal_folder
mkdir "%journal%\call-%stamp%" 2>nul
if not errorlevel 1 goto journal_record
set "stamp=%stamp%x"
goto journal_folder
:journal_record
echo %*>"%journal%\call-%stamp%\call.txt""#,
        folder = folder.display(),
    )
}

/// The journal of a stand-in CLI, read after the call of the test has ended: every copy of
/// the script keeps its line in a file of its own, and a copy that runs at this moment may
/// have made its folder but not its file yet.
pub(crate) fn journal_of(folder: &Path) -> String {
    let entries = match fs::read_dir(folder) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return String::new(),
        Err(error) => panic!("the journal {}: {error}", folder.display()),
    };
    let mut journal = String::new();
    for entry in entries {
        let file = entry
            .expect("a call of the stand-in")
            .path()
            .join("call.txt");
        match fs::read_to_string(&file) {
            Ok(text) => journal.push_str(&text),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => panic!("the journal {}: {error}", file.display()),
        }
    }
    journal
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::process::{Child, Command, Stdio};

    use super::{journal_of, journal_writer};
    use crate::window_tests::scratch;

    /// Starts the stand-in with the arguments of one call.
    fn call(script: &Path, args: &[&str]) -> Child {
        Command::new(script)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("the stand-in script starts")
    }

    #[test]
    fn twelve_calls_that_write_the_journal_together_all_reach_it() {
        let folder = scratch("journal-together");
        let journal = folder.join("calls");
        let lines = journal_writer(&journal);
        let script = folder.join("stand-in.cmd");
        let text = format!(
            "@echo off\r\nsetlocal EnableExtensions\r\n{}\r\nexit /b 0\r\n",
            lines.replace('\n', "\r\n")
        );
        fs::write(&script, text).unwrap();

        let writers = 12;
        let mut children = Vec::new();
        for index in 0..writers {
            let name = format!("web-{index}");
            children.push(call(&script, &["delete", "pods", &name]));
        }
        for mut child in children {
            assert!(
                child.wait().expect("the stand-in ends").success(),
                "the stand-in answers"
            );
        }

        let journal = journal_of(&journal);
        for index in 0..writers {
            let line = format!("delete pods web-{index}");
            assert!(
                journal.lines().any(|row| row == line),
                "`{line}` is in the journal: {journal:?}"
            );
        }
    }
}
