//! T-0224: the Database and JShell windows read the answers of their helpers only while
//! a connection or a console is alive, and the tab of a console hears that it closed
//! instead of a list of the open tabs being compared on every answer.
//!
//! `cargo test -p panels window_tests_idle_polling`

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]

use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use database::{DriverKind, ResultSet, Source, SourceId, Table};
use repl::{Command, CompletionItem, CompletionSource, ConsoleSession, Event};
use ui_kit::gpui;
use ui_kit::gpui::TestAppContext;

use crate::database_panel::{ActiveDatabase, DatabasePanel};
use crate::jshell_panel::{ActiveJShell, JShellPanel};
use crate::window_tests::{draw, open_path, open_window, sample_project, wait_for_signal};

const MODULE: &str = "app";
/// Enough frames for a timer of 50 ms to tick many times.
const FRAMES: usize = 20;

/// A helper that never answers and never ends, for a console that keeps a session.
struct IdleSession {
    killed: Arc<AtomicBool>,
}

impl ConsoleSession for IdleSession {
    fn send(&self, _: Command) -> io::Result<()> {
        Ok(())
    }

    fn poll(&self) -> Vec<Event> {
        Vec::new()
    }

    fn next_id(&self) -> u64 {
        1
    }

    fn kill(&self) {
        self.killed.store(true, Ordering::Relaxed);
    }

    fn completion_source(&self) -> Arc<dyn CompletionSource> {
        Arc::new(NoCompletion)
    }
}

struct NoCompletion;

impl CompletionSource for NoCompletion {
    fn complete(&self, _: &str, _: usize, _: Duration) -> Result<Vec<CompletionItem>, String> {
        Ok(Vec::new())
    }
}

fn database(cx: &TestAppContext) -> gpui::Entity<DatabasePanel> {
    cx.update(|cx| cx.global::<ActiveDatabase>().0.clone())
}

fn jshell(cx: &TestAppContext) -> gpui::Entity<JShellPanel> {
    cx.update(|cx| cx.global::<ActiveJShell>().0.clone())
}

/// Lets the timers of the window run: a task of 50 ms ticks on every frame of this loop.
fn frames(cx: &mut TestAppContext, count: usize) {
    for _ in 0..count {
        cx.executor().advance_clock(Duration::from_millis(50));
        cx.run_until_parked();
    }
}

#[ui_kit::gpui::test]
fn a_database_window_without_a_connection_does_not_poll(cx: &mut TestAppContext) {
    let root = sample_project("idle-database");
    let _window = open_window(cx, root);
    let panel = database(cx);
    frames(cx, FRAMES);
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.poll_ticks_for_test()),
        0,
        "a window without a connection holds no timer"
    );

    // A source the window knows is not a live session either.
    panel.update(cx, |panel, cx| {
        panel.connect_for_test(
            Source {
                id: SourceId::new("owners"),
                name: "PetClinic".into(),
                kind: DriverKind::H2,
                url: "mem:petclinic".into(),
                user: "sa".into(),
                module: None,
                driver_jar: None,
            },
            Vec::<Table>::new(),
            ResultSet::default(),
            None,
            Arc::new(AtomicBool::new(false)),
            cx,
        );
    });
    frames(cx, FRAMES);
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.poll_ticks_for_test()),
        0,
        "a source without a connection keeps no timer"
    );
}

#[ui_kit::gpui::test]
fn a_jshell_window_without_a_console_does_not_poll(cx: &mut TestAppContext) {
    let root = sample_project("idle-jshell");
    let _window = open_window(cx, root);
    let panel = jshell(cx);
    frames(cx, FRAMES);
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.poll_ticks_for_test()),
        0,
        "a window without a console holds no timer"
    );
}

#[ui_kit::gpui::test]
fn closing_the_console_tab_ends_its_session(cx: &mut TestAppContext) {
    let root = sample_project("idle-jshell-tab");
    let window = open_window(cx, root);
    let panel = jshell(cx);
    let path: PathBuf = panel.read_with(cx, |panel, _| panel.console_path(MODULE));
    if let Some(directory) = path.parent() {
        std::fs::create_dir_all(directory).unwrap();
    }
    std::fs::write(&path, "x * 21\n").unwrap();
    let killed = Arc::new(AtomicBool::new(false));
    panel.update(cx, |panel, cx| {
        panel.connect_for_test(
            MODULE,
            Box::new(IdleSession {
                killed: killed.clone(),
            }),
            cx,
        );
    });
    open_path(cx, window, &path);
    draw(cx, window);

    // A live console keeps the task that reads its answers.
    wait_for_signal(cx, "the answers of the console", |cx| {
        panel.read_with(cx, |panel, _| panel.poll_ticks_for_test()) > 0
    });

    window
        .update(cx, |workspace, window, cx| {
            workspace.close_active_tab(window, cx)
        })
        .unwrap();
    wait_for_signal(cx, "the session of the closed tab", |_| {
        killed.load(Ordering::Relaxed)
    });
    assert!(
        window
            .read_with(cx, |workspace, _| workspace
                .open_tabs()
                .iter()
                .flatten()
                .all(|open| open != &path))
            .unwrap(),
        "the tab of the console is closed"
    );
}
