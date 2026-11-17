//! A window program the user started from the terminal outlives the session, while a
//! console program started beside it goes with the session.
#![cfg(windows)]

use std::path::Path;
use std::time::{Duration, Instant};

use platform::{
    ProcessWaiter, is_console_process, process_created, process_table, terminate_process,
};
use terminal::{SpawnSpec, TerminalSession, allowlisted};

/// The window program of this crate: no window, only a long sleep.
const GUI: &str = env!("CARGO_BIN_EXE_gui-sleeper");
const IMAGE: &str = "gui-sleeper.exe";
const WAIT: Duration = Duration::from_secs(20);

/// Opens a terminal of the default shell in a folder of the test.
fn session(folder: &Path) -> TerminalSession {
    TerminalSession::spawn(&SpawnSpec {
        command_line: terminal::default_shell().expect("a shell"),
        cwd: Some(folder.to_path_buf()),
        env: allowlisted(),
        cols: 80,
        rows: 12,
        shift_enter: Default::default(),
    })
    .expect("spawn")
}

/// The line that starts `program` in the shell of the terminal. PowerShell needs `&` for
/// a quoted path; the command prompt and bash run the path as it is.
fn start_line(program: &Path) -> String {
    let shell = terminal::default_shell()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if shell.contains("pwsh") || shell.contains("powershell") {
        format!("& '{}'", program.display())
    } else if shell.contains("bash") {
        format!("'{}' &", program.display())
    } else {
        format!("\"{}\"", program.display())
    }
}

/// The live child of `shell` with this image, if the shell has started it.
fn child(shell: u32, image: &str) -> Option<u32> {
    process_table()
        .into_iter()
        .find(|process| process.parent == Some(shell) && process.name.eq_ignore_ascii_case(image))
        .map(|process| process.pid)
}

fn wait_for_child(shell: u32, image: &str) -> u32 {
    let started = Instant::now();
    loop {
        if let Some(pid) = child(shell, image) {
            return pid;
        }
        assert!(started.elapsed() < WAIT, "the shell never started {image}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn ended(waiter: &ProcessWaiter) -> bool {
    // An unreadable answer leaves the process counted as running, which fails the test.
    matches!(waiter.wait(Duration::ZERO), Ok(true))
}

fn wait_until_gone(waiter: &ProcessWaiter, what: &str) {
    let started = Instant::now();
    while !ended(waiter) {
        assert!(started.elapsed() < WAIT, "{what} still runs");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Ends the window program even when the test fails before its last line. The creation
/// time keeps an id that another process took later out of it.
struct EndOnDrop {
    pid: u32,
    created: u64,
}

impl Drop for EndOnDrop {
    fn drop(&mut self) {
        let _ = terminate_process(self.pid, self.created);
    }
}

#[test]
fn a_window_program_outlives_the_session_while_a_console_program_does_not() {
    let folder = test_support::TempDir::new("terminal-gui");
    let session = session(&folder);
    let shell = session.pid();

    // The window program comes first: the console program takes the shell for itself.
    session
        .send(format!("{}\r", start_line(Path::new(GUI))).into_bytes())
        .expect("input");
    let window = wait_for_child(shell, IMAGE);

    session
        .send(b"ping -t 127.0.0.1\r".to_vec())
        .expect("input");
    let console = wait_for_child(shell, "PING.EXE");

    // The two programs differ only in the subsystem of their image.
    assert_eq!(is_console_process(console), Some(true));
    assert_eq!(is_console_process(window), Some(false));
    let created = process_created(window).expect("the creation time of the window program");
    let _end = EndOnDrop {
        pid: window,
        created,
    };

    let console_waiter = ProcessWaiter::open(console)
        .expect("open the console program")
        .expect("the console program runs");
    let window_waiter = ProcessWaiter::open(window)
        .expect("open the window program")
        .expect("the window program runs");

    session.terminate();
    drop(session);

    // The console program dies with the session; the cleanup rounds that find it by its
    // parent id have to skip the window program.
    wait_until_gone(&console_waiter, "the console program of the terminal");
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        !ended(&window_waiter),
        "the session ended the window program it did not start for itself"
    );

    // The window program is not the session's to end, so the test ends it here.
    terminate_process(window, created).expect("end the window program");
    wait_until_gone(&window_waiter, "the window program of the test");
}
