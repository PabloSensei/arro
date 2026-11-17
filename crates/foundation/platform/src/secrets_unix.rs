//! The Secret Service of Linux through `secret-tool` of `libsecret-tools`. The secret
//! travels on the standard input of the program and never on its command line or in the
//! environment, where any other process of the session could read it.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use super::{DeleteAnswer, Secret, delete_is_done, secret_from_lookup};

/// The program of `libsecret-tools`.
const TOOL: &str = "secret-tool";
/// A path of the program of the caller, which the tests point at a stand-in. A path set
/// here is trusted as the whole answer: the session check is for a machine's own tool.
const TOOL_PATH: &str = "ARRO_SECRET_TOOL";
/// How long one call of the tool may take before it is killed.
const TIMEOUT: Duration = Duration::from_secs(10);
/// How often a waiting call checks whether the tool has finished.
const TICK: Duration = Duration::from_millis(10);

fn program() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os(TOOL_PATH) {
        return Some(PathBuf::from(path));
    }
    let path = std::env::var_os("PATH")?;
    for folder in std::env::split_paths(&path) {
        let candidate = folder.join(TOOL);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn has_session() -> bool {
    std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some_and(|address| !address.is_empty())
}

fn missing(message: &str) -> String {
    format!("{message} Install the `libsecret-tools` package for the Secret Service.")
}

pub fn available() -> Result<(), String> {
    let Some(program) = program() else {
        return Err(missing(
            "The password storage needs the `secret-tool` program.",
        ));
    };
    if std::env::var_os(TOOL_PATH).is_none() && !has_session() {
        return Err(missing(
            "The password storage needs a D-Bus session, and this session has none.",
        ));
    }
    if !program.is_file() {
        return Err(missing("The `secret-tool` program is not a file."));
    }
    Ok(())
}

pub fn store(service: &str, account: &str, secret: &Secret) -> Result<(), String> {
    let label = format!("{service} ({account})");
    let arguments = [
        "store".to_owned(),
        format!("--label={label}"),
        "service".to_owned(),
        service.to_owned(),
        "account".to_owned(),
        account.to_owned(),
    ];
    run(&arguments, Some(secret.expose().as_bytes()))?;
    Ok(())
}

pub fn load(service: &str, account: &str) -> Option<Secret> {
    let arguments = [
        "lookup".to_owned(),
        "service".to_owned(),
        service.to_owned(),
        "account".to_owned(),
        account.to_owned(),
    ];
    let answer = run(&arguments, None).ok()?;
    secret_from_lookup(&answer)
}

pub fn delete(service: &str, account: &str) -> Result<(), String> {
    let arguments = [
        "clear".to_owned(),
        "service".to_owned(),
        service.to_owned(),
        "account".to_owned(),
        account.to_owned(),
    ];
    let ended = execute(&arguments, None)?;
    let answer = DeleteAnswer::Program {
        code: ended.status.code(),
        stderr: String::from_utf8_lossy(&ended.stderr).into_owned(),
    };
    if delete_is_done(answer) {
        Ok(())
    } else {
        Err(ended.failure())
    }
}

/// A call of the tool that ended, with what it wrote: the caller decides what a failure
/// means, because `delete` treats one of them as done and no other call does.
struct Ended {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

impl Ended {
    /// The text of a call that failed: the code and the words of the tool, which name the
    /// reason for the user. `secret-tool` never writes a secret to its error stream.
    fn failure(&self) -> String {
        let code = self.status.code().unwrap_or(-1);
        let stderr = String::from_utf8_lossy(&self.stderr);
        let stderr = stderr.trim();
        if stderr.is_empty() {
            format!("The `secret-tool` program ended with code {code}.")
        } else {
            format!("The `secret-tool` program ended with code {code}: {stderr}")
        }
    }
}

/// Starts the tool, writes the secret to its standard input, and waits for it with a
/// timeout. The output and the error of the tool are read on their own threads, so a
/// program that writes more than a pipe holds cannot block this call.
fn execute(arguments: &[String], input: Option<&[u8]>) -> Result<Ended, String> {
    let program = program().ok_or_else(|| missing("The `secret-tool` program is missing."))?;
    let mut child = Command::new(&program)
        .args(arguments)
        // The caller tells a missing entry from a failure by the words of the error
        // stream, so the tool is asked for the untranslated words of the C locale.
        .env("LC_ALL", "C")
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            missing(&format!(
                "The `secret-tool` program did not start: {error}."
            ))
        })?;
    let stdout = child
        .stdout
        .take()
        .map(|mut stream| thread_reader(move || read_all(&mut stream)));
    let stderr = child
        .stderr
        .take()
        .map(|mut stream| thread_reader(move || read_all(&mut stream)));
    let writer = input.map(|bytes| {
        let bytes = bytes.to_vec();
        child.stdin.take().map(|mut stream| {
            thread_reader(move || {
                // The tool reads the secret up to the end of the stream, then stores it.
                let _ = stream.write_all(&bytes);
            })
        })
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(missing(&format!(
                    "The `secret-tool` program could not be awaited: {error}."
                )));
            }
        }
        if started.elapsed() >= TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            return Err(missing("The `secret-tool` program did not answer in time."));
        }
        std::thread::sleep(TICK);
    };
    if let Some(writer) = writer.flatten() {
        let _ = writer.join();
    }
    let stdout = stdout
        .and_then(|reader| reader.join().ok())
        .unwrap_or_default();
    let stderr = stderr
        .and_then(|reader| reader.join().ok())
        .unwrap_or_default();
    Ok(Ended {
        status,
        stdout,
        stderr,
    })
}

/// A call of the tool that must succeed for the answer to matter.
fn run(arguments: &[String], input: Option<&[u8]>) -> Result<Vec<u8>, String> {
    let ended = execute(arguments, input)?;
    if ended.status.success() {
        Ok(ended.stdout)
    } else {
        Err(ended.failure())
    }
}

/// A thread whose result the caller waits for, and never the answer of a panic.
fn thread_reader<T: Send + 'static>(
    read: impl FnOnce() -> T + Send + 'static,
) -> std::thread::JoinHandle<T> {
    std::thread::spawn(read)
}

fn read_all(stream: &mut impl Read) -> Vec<u8> {
    let mut bytes = Vec::new();
    let _ = stream.read_to_end(&mut bytes);
    bytes
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::{Mutex, MutexGuard};

    use super::*;

    /// The variable of the test program is global to the process, so the tests of the
    /// stand-in run one at a time.
    static LOCK: Mutex<()> = Mutex::new(());

    fn lock() -> MutexGuard<'static, ()> {
        LOCK.lock().unwrap_or_else(|error| error.into_inner())
    }

    /// Points the module at a stand-in for the time of one test, and puts the variable
    /// back even when an assertion fails.
    struct ToolPath;

    impl ToolPath {
        fn set(path: &std::path::Path) -> Self {
            // SAFETY: the lock of this module keeps every test of the variable apart.
            unsafe { std::env::set_var(TOOL_PATH, path) };
            Self
        }
    }

    impl Drop for ToolPath {
        fn drop(&mut self) {
            // SAFETY: the lock of this module keeps every test of the variable apart.
            unsafe { std::env::remove_var(TOOL_PATH) };
        }
    }

    /// A `secret-tool` that writes its arguments and its standard input into a folder
    /// of the test, and answers `lookup` with a text of the test.
    fn stand_in(folder: &std::path::Path, answer: &str) -> PathBuf {
        let script = folder.join("secret-tool");
        let args = folder.join("args");
        let input = folder.join("input");
        fs::write(
            &script,
            format!(
                "#!/bin/sh\n\
                 printf '%s\\n' \"$@\" > '{}'\n\
                 cat > '{}'\n\
                 if [ \"$1\" = lookup ]; then printf '%s\\n' '{}'; fi\n\
                 exit 0\n",
                args.display(),
                input.display(),
                answer
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(&script).unwrap().permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&script, permissions).unwrap();
        }
        script
    }

    #[test]
    fn store_sends_the_arguments_and_the_secret_on_the_standard_input() {
        let _guard = lock();
        let folder = test_support::TempDir::new("secret-tool-store");
        let script = stand_in(folder.path(), "");
        let _tool = ToolPath::set(&script);
        let secret = Secret::new("hunter2");

        store("Arro/database/owners", "sa", &secret).unwrap();

        let args = fs::read_to_string(folder.path().join("args")).unwrap();
        let lines: Vec<&str> = args.lines().collect();
        assert_eq!(lines[0], "store");
        // One argument of `--label=`, as `secret-tool store --label='Label' attribute value`
        // takes it, so the spaces of the label never split it.
        assert_eq!(lines[1], "--label=Arro/database/owners (sa)");
        assert_eq!(
            &lines[2..],
            ["service", "Arro/database/owners", "account", "sa"]
        );
        assert!(
            !args.contains("hunter2"),
            "the password is not an argument: {args}"
        );
        let input = fs::read_to_string(folder.path().join("input")).unwrap();
        assert_eq!(input, "hunter2");
    }

    #[test]
    fn lookup_reads_the_secret_and_clear_asks_for_the_same_entry() {
        let _guard = lock();
        let folder = test_support::TempDir::new("secret-tool-load");
        let script = stand_in(folder.path(), "from-the-store");
        let _tool = ToolPath::set(&script);

        let loaded = load("Arro/database/owners", "sa").expect("the stand-in answers");
        assert_eq!(loaded.expose(), "from-the-store");
        let args = fs::read_to_string(folder.path().join("args")).unwrap();
        assert_eq!(
            args.lines().collect::<Vec<_>>(),
            ["lookup", "service", "Arro/database/owners", "account", "sa"]
        );

        delete("Arro/database/owners", "sa").unwrap();
        let args = fs::read_to_string(folder.path().join("args")).unwrap();
        assert_eq!(
            args.lines().collect::<Vec<_>>(),
            ["clear", "service", "Arro/database/owners", "account", "sa"]
        );
    }

    #[test]
    fn a_missing_tool_names_the_package_to_install() {
        let _guard = lock();
        let folder = test_support::TempDir::new("secret-tool-missing");
        let _tool = ToolPath::set(&folder.path().join("no-such-secret-tool"));
        let error = available().unwrap_err();
        assert!(error.contains("libsecret-tools"), "{error}");
    }

    /// Puts a folder first in `PATH` and clears the direct path of the tool, so `program`
    /// has to find the stand-in through the search path. Both variables go back even when
    /// an assertion fails.
    #[cfg(target_os = "linux")]
    struct SearchPath {
        path: Option<std::ffi::OsString>,
        tool: Option<std::ffi::OsString>,
    }

    #[cfg(target_os = "linux")]
    impl SearchPath {
        fn set(folder: &std::path::Path) -> Self {
            let path = std::env::var_os("PATH");
            let tool = std::env::var_os(TOOL_PATH);
            let mut folders = vec![folder.to_path_buf()];
            if let Some(value) = &path {
                folders.extend(std::env::split_paths(value));
            }
            let search = std::env::join_paths(folders).expect("a search path of the test");
            // SAFETY: the lock of this module keeps every test of the variables apart.
            unsafe {
                std::env::set_var("PATH", search);
                std::env::remove_var(TOOL_PATH);
            }
            Self { path, tool }
        }
    }

    #[cfg(target_os = "linux")]
    impl Drop for SearchPath {
        fn drop(&mut self) {
            // SAFETY: the lock of this module keeps every test of the variables apart.
            unsafe {
                match self.path.take() {
                    Some(path) => std::env::set_var("PATH", path),
                    None => std::env::remove_var("PATH"),
                }
                if let Some(tool) = self.tool.take() {
                    std::env::set_var(TOOL_PATH, tool);
                }
            }
        }
    }

    /// A `secret-tool` that writes the locale of the call into a folder of the test and
    /// ends with `code`, saying `stderr`. The script uses the builtins of the shell alone,
    /// so the search path of the test may stay the one of the machine.
    #[cfg(target_os = "linux")]
    fn failing_stand_in(folder: &std::path::Path, code: i32, stderr: &str) -> PathBuf {
        let script = folder.join(TOOL);
        let locale = folder.join("locale");
        fs::write(
            &script,
            format!(
                "#!/bin/sh\n\
                 printf '%s' \"$LC_ALL\" > '{}'\n\
                 printf '%s\\n' '{stderr}' >&2\n\
                 exit {code}\n",
                locale.display()
            ),
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(&script).unwrap().permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&script, permissions).unwrap();
        }
        script
    }

    /// `secret-tool clear` may end with a failure when the store has nothing to remove,
    /// and the module promises that an absent entry is not an error.
    #[cfg(target_os = "linux")]
    #[test]
    fn clear_of_an_absent_entry_is_not_an_error() {
        let _guard = lock();
        let folder = test_support::TempDir::new("secret-tool-clear-absent");
        failing_stand_in(
            folder.path(),
            1,
            "clear: No such secret item at path /org/freedesktop/secrets/collection/login/7",
        );
        let _path = SearchPath::set(folder.path());

        delete("Arro/database/owners", "sa").unwrap();

        let locale = fs::read_to_string(folder.path().join("locale")).unwrap();
        assert_eq!(locale, "C", "the words of the tool are read untranslated");
    }

    /// Every other failure of the tool stays an error and carries its own words.
    #[cfg(target_os = "linux")]
    #[test]
    fn clear_that_failed_for_another_reason_is_an_error() {
        let _guard = lock();
        let folder = test_support::TempDir::new("secret-tool-clear-failed");
        failing_stand_in(
            folder.path(),
            1,
            "clear: Cannot autolaunch D-Bus without X11 $DISPLAY",
        );
        let _path = SearchPath::set(folder.path());

        let error = delete("Arro/database/owners", "sa").unwrap_err();
        assert!(error.contains("code 1"), "{error}");
        assert!(error.contains("Cannot autolaunch D-Bus"), "{error}");
    }
}
