//! The window in idle: nothing changes, so nothing asks Git or Docker. The status follows
//! the file system, not a clock, and the Services tool window starts its watch only while
//! it is open.

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use ui_kit::gpui;
use ui_kit::gpui::TestAppContext;
use workspace::SERVICES_TOOL;

use crate::docker::ActiveDocker;
use crate::git::ActiveGit;
use crate::window_tests::{draw, open_window, scratch, wait_for_signal};
use crate::window_tests_ui::ENV;

/// How long the status may take to arrive after a change on disk.
const LIMIT: Duration = Duration::from_secs(2);
/// One step of the simulated clock while a change on disk is awaited.
const STEP: Duration = Duration::from_millis(50);

/// A project with a repository and one committed file, as a user opens it.
pub(crate) fn repository(name: &str) -> (PathBuf, vcs::GitRunner) {
    let git = vcs::GitRunner::from_env().expect("git is on the PATH");
    let root = scratch(name);
    git.run(&root, ["init"]).unwrap();
    git.run(&root, ["config", "user.email", "idle-test@example.com"])
        .unwrap();
    git.run(&root, ["config", "user.name", "Idle Test"])
        .unwrap();
    fs::write(root.join("main.txt"), "alpha\nbeta\n").unwrap();
    git.run(&root, ["add", "main.txt"]).unwrap();
    git.run(&root, ["commit", "-m", "base"]).unwrap();
    (root, git)
}

pub(crate) fn git_state(cx: &TestAppContext) -> gpui::Entity<crate::git::GitState> {
    cx.read(|cx| cx.global::<ActiveGit>().0.clone())
}

pub(crate) fn status_reads(cx: &TestAppContext, state: &gpui::Entity<crate::git::GitState>) -> u64 {
    state.read_with(cx, |state, _| {
        state
            .repo()
            .map_or(0, |repository| repository.status_reads())
    })
}

pub(crate) fn wait_for_repository(
    cx: &mut TestAppContext,
    state: &gpui::Entity<crate::git::GitState>,
) {
    wait_for_signal(cx, "the repository of the window", |cx| {
        state.read_with(cx, |state, _| state.repo().is_some())
    });
}

/// Advances the simulated clock until `done` holds, or fails at `LIMIT`.
pub(crate) fn wait_for_status(
    cx: &mut TestAppContext,
    state: &gpui::Entity<crate::git::GitState>,
    what: &str,
    mut done: impl FnMut(&vcs::Status) -> bool,
) {
    let mut waited = Duration::ZERO;
    loop {
        if state.read_with(cx, |state, _| done(state.status())) {
            return;
        }
        assert!(waited < LIMIT, "the status did not show {what} in time");
        std::thread::sleep(Duration::from_millis(10));
        cx.executor().advance_clock(STEP);
        cx.run_until_parked();
        waited += STEP;
    }
}

#[ui_kit::gpui::test]
fn an_idle_window_asks_neither_git_nor_docker(cx: &mut TestAppContext) {
    let _guard = ENV.lock().unwrap_or_else(|poison| poison.into_inner());
    let (root, _) = repository("idle");
    // A CLI that does not exist: a watch that started would say so at once.
    let missing = scratch("idle-no-docker").join("docker.exe");
    // SAFETY: the test holds `ENV`, the window reads the variable at creation.
    unsafe { std::env::set_var("IDE_DOCKER_CLI", &missing) };
    let window = open_window(cx, root);
    // SAFETY: the window has read the variable already.
    unsafe { std::env::remove_var("IDE_DOCKER_CLI") };

    let git = git_state(cx);
    wait_for_repository(cx, &git);
    wait_for_signal(cx, "the first read of the status", |cx| {
        status_reads(cx, &git) > 0 && !git.read_with(cx, |state, _| state.refreshing())
    });
    let docker = cx.read(|cx| cx.global::<ActiveDocker>().0.clone());

    // Let the start of the window settle, then count ten seconds of its clock.
    cx.executor().advance_clock(Duration::from_secs(1));
    cx.run_until_parked();
    let reads = status_reads(cx, &git);
    assert!(
        !docker.read_with(cx, |panel, _| panel.watching()),
        "a closed Services window holds no watch of Docker"
    );
    let calls = docker.read_with(cx, |panel, _| panel.cli_calls());
    for _ in 0..20 {
        std::thread::sleep(Duration::from_millis(2));
        cx.executor().advance_clock(Duration::from_millis(500));
        cx.run_until_parked();
    }
    assert_eq!(
        status_reads(cx, &git),
        reads,
        "the status is not read while nothing changes"
    );
    assert_eq!(
        docker.read_with(cx, |panel, _| panel.cli_calls()),
        calls,
        "Docker is not asked while the panel is closed"
    );
    assert!(!docker.read_with(cx, |panel, _| panel.watching()));
    draw(cx, window);
}

#[ui_kit::gpui::test]
fn a_file_changed_on_disk_and_a_commit_outside_update_the_status(cx: &mut TestAppContext) {
    let (root, git) = repository("idle-events");
    let window = open_window(cx, root.clone());
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    wait_for_signal(cx, "the first read of the status", |cx| {
        status_reads(cx, &state) > 0 && !state.read_with(cx, |state, _| state.refreshing())
    });

    fs::write(root.join("later.txt"), "gamma\n").unwrap();
    wait_for_status(cx, &state, "the new file", |status| {
        status.files.iter().any(|file| file.path == "later.txt")
    });

    // A commit of another program changes only the metadata under `.git`.
    git.run(&root, ["add", "later.txt"]).unwrap();
    git.run(&root, ["commit", "-m", "later"]).unwrap();
    wait_for_status(cx, &state, "the commit", |status| status.files.is_empty());
    draw(cx, window);
}

#[ui_kit::gpui::test]
fn the_services_window_watches_docker_only_while_it_is_open(cx: &mut TestAppContext) {
    let _guard = ENV.lock().unwrap_or_else(|poison| poison.into_inner());
    let root = scratch("idle-services");
    let missing = scratch("idle-services-no-docker").join("docker.exe");
    // SAFETY: the test holds `ENV`, the window reads the variable at creation.
    unsafe { std::env::set_var("IDE_DOCKER_CLI", &missing) };
    let window = open_window(cx, root);
    // SAFETY: the window has read the variable already.
    unsafe { std::env::remove_var("IDE_DOCKER_CLI") };

    let docker = cx.read(|cx| cx.global::<ActiveDocker>().0.clone());
    assert!(
        !docker.read_with(cx, |panel, _| panel.watching()),
        "the watch waits for the tool window"
    );

    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(SERVICES_TOOL, window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
    wait_for_signal(cx, "the Docker watch", |cx| {
        docker.read_with(cx, |panel, _| panel.watching())
    });

    window
        .update(cx, |workspace, window, cx| {
            workspace.toggle_tool(SERVICES_TOOL, window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    wait_for_signal(cx, "the watch to end", |cx| {
        !docker.read_with(cx, |panel, _| panel.watching())
    });
}
