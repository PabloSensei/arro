//! A program of the terminal dies with what started it: the window, the session, or the
//! test process itself. The check is by process id, not by the parent tree, because a
//! program that outlives its shell is no longer a child of anything a walk from this
//! process can reach.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use platform::{ProcessWaiter, process_tree};
#[cfg(windows)]
use platform::{process_created, process_table};
use terminal::{SpawnSpec, TerminalSession};
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use workspace::{TERMINAL_TOOL, Workspace};

use crate::terminal_panel::ActiveTerminal;
use crate::window_tests::{open_sample_window, scratch, wait_for_signal};

/// The line that starts a program which never ends by itself.
fn endless_program() -> &'static str {
    if cfg!(windows) {
        "ping -t 127.0.0.1"
    } else {
        "sleep 600"
    }
}

/// The image name of the program `endless_program` starts.
fn endless_image() -> &'static str {
    if cfg!(windows) { "PING.EXE" } else { "sleep" }
}

/// How long a program of the terminal may take to go away before the test calls it a leak.
const LEAK_LIMIT: Duration = Duration::from_secs(60);

/// Opens the terminal of a window and waits until the shell is up.
fn running_terminal(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
) -> Arc<TerminalSession> {
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(TERMINAL_TOOL, window, cx)
        })
        .unwrap();
    let terminal = cx.update(|cx| cx.global::<ActiveTerminal>().0.clone());
    wait_for_signal(cx, "the shell", |cx| {
        terminal.read_with(cx, |panel, _| panel.session().is_some())
    });
    terminal
        .read_with(cx, |panel, _| panel.session().cloned())
        .expect("the terminal has a session")
}

/// Types the endless program into the shell and returns the id of the program it started.
fn run_endless_program(cx: &mut TestAppContext, session: &TerminalSession) -> u32 {
    let shell = session.pid();
    session
        .send(format!("{}\r", endless_program()).into_bytes())
        .expect("input");
    let mut program = None;
    wait_for_signal(cx, "the program of the terminal", |_| {
        program = child_of(shell);
        program.is_some()
    });
    program.expect("the program of the terminal has a process id")
}

/// The program of `endless_program`, while its session still runs.
///
/// The search walks the whole tree of the shell, not its direct children only. On Linux
/// the session starts `/bin/sh` (dash), which runs the real shell as its child instead of
/// replacing itself, so the program of the shell is a grandchild of the pid the session
/// reports. Windows starts the shell itself, and macOS `/bin/sh` is bash, which replaces
/// itself, so there the walk finds the program one level lower.
fn child_of(shell: u32) -> Option<u32> {
    let tree = process_tree();
    let mut below: HashMap<u32, Vec<u32>> = HashMap::new();
    for process in &tree {
        if let Some(parent) = process.parent {
            below.entry(parent).or_default().push(process.pid);
        }
    }
    let mut seen = HashSet::new();
    let mut pending = vec![shell];
    while let Some(parent) = pending.pop() {
        let Some(children) = below.get(&parent) else {
            continue;
        };
        for &pid in children {
            if !seen.insert(pid) {
                continue;
            }
            let ours = tree.iter().any(|process| {
                process.pid == pid && process.name.eq_ignore_ascii_case(endless_image())
            });
            if ours {
                return Some(pid);
            }
            pending.push(pid);
        }
    }
    None
}

/// The live children of `shell` that were created with the session or later. The id of a
/// dead shell stays in the process table as the parent of its programs, and the creation
/// time keeps a process that reused the id out of the list.
#[cfg(windows)]
fn children_of(shell: u32, since: Option<u64>) -> Vec<u32> {
    process_table()
        .into_iter()
        .filter(|process| process.parent == Some(shell))
        .filter(|process| match (since, process_created(process.pid)) {
            (Some(since), Some(created)) => created >= since,
            _ => true,
        })
        .map(|process| process.pid)
        .collect()
}

/// Waits until the shell has no children left. A test that watched the program it knew
/// before the close still misses one that was started during the close; this catches it
/// by the parent link.
#[cfg(windows)]
fn wait_until_no_children(shell: u32, since: Option<u64>, what: &str, mut tick: impl FnMut()) {
    let started = Instant::now();
    loop {
        let left = children_of(shell, since);
        if left.is_empty() {
            return;
        }
        assert!(
            started.elapsed() < LEAK_LIMIT,
            "{what}: the terminal still runs {left:?} after {} s",
            LEAK_LIMIT.as_secs()
        );
        std::thread::sleep(Duration::from_millis(10));
        tick();
    }
}

/// Waits until the shell has started the program, without a window to turn the test clock.
fn wait_for_child(shell: u32) -> u32 {
    let started = Instant::now();
    loop {
        if let Some(program) = child_of(shell) {
            return program;
        }
        assert!(
            started.elapsed() < LEAK_LIMIT,
            "the shell never started {}",
            endless_image()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Opens a watcher for every id that still names a running process. The watcher holds the
/// process itself, so an id reused later cannot answer for it.
fn watch(ids: &[u32]) -> Vec<(u32, ProcessWaiter)> {
    ids.iter()
        .filter_map(|&id| {
            ProcessWaiter::open(id)
                .ok()
                .flatten()
                .map(|waiter| (id, waiter))
        })
        .collect()
}

/// Whether the process behind the watcher has ended.
fn ended(waiter: &ProcessWaiter) -> bool {
    // An unreadable answer leaves the process counted as running, which fails the test.
    matches!(waiter.wait(Duration::ZERO), Ok(true))
}

/// Waits until every watched process has ended and names the ones that did not. `tick`
/// gives the window of a test the chance to work while the wait goes on.
fn wait_until_gone(watched: &[(u32, ProcessWaiter)], what: &str, mut tick: impl FnMut()) {
    let started = Instant::now();
    loop {
        let left: Vec<u32> = watched
            .iter()
            .filter(|(_, waiter)| !ended(waiter))
            .map(|(id, _)| *id)
            .collect();
        if left.is_empty() {
            return;
        }
        assert!(
            started.elapsed() < LEAK_LIMIT,
            "{what}: the terminal still runs {left:?} after {} s",
            LEAK_LIMIT.as_secs()
        );
        std::thread::sleep(Duration::from_millis(10));
        tick();
    }
}

/// Closes the window the way the cross does: the platform asks the workspace whether it
/// may close, and the window goes away as the application removes it.
fn close_by_the_cross(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    let closed = visual.simulate_close();
    assert!(closed, "the cross asks to close the window");
    drop(visual);
    window
        .update(cx, |_, window, _| window.remove_window())
        .unwrap();
    cx.run_until_parked();
    assert!(
        cx.windows().is_empty(),
        "the window of the test is still open"
    );
}

/// One turn of the simulated clock of a window test.
fn tick(cx: &mut TestAppContext) {
    cx.executor().advance_clock(Duration::from_millis(50));
    cx.run_until_parked();
}

#[ui_kit::gpui::test]
fn closing_the_window_leaves_no_program_of_its_terminal(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "terminal-leak-cross");
    let session = running_terminal(cx, window);
    let shell = session.pid();
    #[cfg(windows)]
    let shell_created = process_created(shell);
    let program = run_endless_program(cx, &session);
    let watched = watch(&[shell, program]);
    assert_eq!(watched.len(), 2, "both the shell and its program run");
    drop(session);

    close_by_the_cross(cx, window);

    wait_until_gone(&watched, "the window closed", || tick(cx));
    #[cfg(windows)]
    wait_until_no_children(shell, shell_created, "the window closed", || tick(cx));
}

#[ui_kit::gpui::test]
fn a_program_ends_with_the_session_that_started_it(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "terminal-leak-session");
    let session = running_terminal(cx, window);
    let program = run_endless_program(cx, &session);
    let watched = watch(&[session.pid(), program]);
    assert_eq!(watched.len(), 2, "both the shell and its program run");

    // The panel still holds its own copy of the session, so stopping it alone has to end
    // the program, without the panel or the window going away first.
    let panel_session = session;
    panel_session.terminate();

    wait_until_gone(&watched, "the session was stopped", || tick(cx));
    drop(panel_session);
    close_by_the_cross(cx, window);
}

#[test]
fn a_dropped_session_takes_its_shell_and_program_with_it() {
    let session = session_in(scratch("terminal-leak-drop"));
    let shell = session.pid();
    session
        .send(format!("{}\r", endless_program()).into_bytes())
        .expect("input");
    let program = wait_for_child(shell);
    let watched = watch(&[shell, program]);
    assert_eq!(watched.len(), 2, "both the shell and its program run");

    drop(session);

    wait_until_gone(&watched, "the session was dropped", || {});
}

/// A session of the default shell in `directory`, without a window.
fn session_in(directory: PathBuf) -> TerminalSession {
    let spec = SpawnSpec {
        command_line: terminal::default_shell().expect("a shell"),
        cwd: Some(directory),
        env: terminal::allowlisted(),
        cols: 80,
        rows: 12,
        shift_enter: Default::default(),
    };
    TerminalSession::spawn(&spec).expect("spawn")
}

/// The line that starts the endless program in a console of its own: the close of the
/// pseudoconsole never reaches that console, so only the job of the session can end it.
#[cfg(windows)]
fn detached_program() -> &'static str {
    "Start-Process -WindowStyle Hidden ping -ArgumentList '-t','127.0.0.1'"
}

/// The reproduction of T-0174: a program started from the terminal outlived the session
/// and the test process. Measured: PowerShell 7 from the Store starts native commands
/// outside every job, and the job of a session held the shell only, so the program was
/// left to the close of the pseudoconsole, which a console of its own never sees. The
/// session now pulls the program of the shell into its job and ends the tree in rounds.
#[cfg(windows)]
#[test]
fn a_program_in_a_console_of_its_own_dies_with_the_session() {
    let session = session_in(scratch("terminal-leak-console"));
    let shell = session.pid();
    session
        .send(format!("{}\r", detached_program()).into_bytes())
        .expect("input");
    let program = wait_for_child(shell);
    let watched = watch(&[program]);
    assert_eq!(watched.len(), 1, "the program runs");

    session.terminate();
    drop(session);

    wait_until_gone(&watched, "the session was stopped", || {});
}
