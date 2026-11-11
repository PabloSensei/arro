//! Git in the window without a window: stash, fetch, Update Project with merge and
//! conflicts, abort and the log. Every remote is a bare folder in a scratch directory and
//! every identity is set in the repository, so the machine is left alone.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ui_kit::Modifiers;
use ui_kit::gpui;
use ui_kit::gpui::{
    MouseButton, MouseDownEvent, MouseUpEvent, TestAppContext, VisualTestContext, WindowHandle,
};

use crate::git_log::ActiveGitLog;
use crate::window_tests::{
    bounds_of, buffer_of, click, draw, editor_of, open_path, open_window, press, scratch,
    type_text, with_window,
};
use workspace::Workspace;

const LIMIT: Duration = Duration::from_secs(30);

pub(crate) fn wait_for(
    cx: &mut TestAppContext,
    what: &str,
    mut check: impl FnMut(&mut TestAppContext) -> bool,
) {
    let started = Instant::now();
    loop {
        if check(cx) {
            return;
        }
        assert!(
            started.elapsed() < LIMIT,
            "the test waited for {what} in vain"
        );
        std::thread::sleep(Duration::from_millis(10));
        cx.executor().advance_clock(Duration::from_millis(50));
        cx.run_until_parked();
    }
}

pub(crate) fn git() -> vcs::GitRunner {
    vcs::GitRunner::from_env().expect("git is on the PATH")
}

pub(crate) fn repository_with_origin(name: &str) -> (PathBuf, PathBuf, vcs::GitRunner) {
    let git = git();
    let root = scratch(name);
    let bare = scratch(&format!("{name}-origin"));
    git.run(&bare, ["init", "--bare"]).unwrap();
    git.run(&root, ["init"]).unwrap();
    git.run(&root, ["config", "user.email", "window-test@example.com"])
        .unwrap();
    git.run(&root, ["config", "user.name", "Window Test"])
        .unwrap();
    fs::write(root.join("main.txt"), "alpha\nbeta\n").unwrap();
    git.run(&root, ["add", "main.txt"]).unwrap();
    git.run(&root, ["commit", "-m", "base"]).unwrap();
    git.run(
        &root,
        [
            "remote",
            "add",
            "origin",
            bare.to_str().expect("a UTF-8 scratch path"),
        ],
    )
    .unwrap();
    git.run(&root, ["push", "-u", "origin", "HEAD"]).unwrap();
    (root, bare, git)
}

fn clone(git: &vcs::GitRunner, bare: &Path, into: &Path) {
    git.run(
        &scratch("git-clone-cwd"),
        ["clone", bare.to_str().unwrap(), into.to_str().unwrap()],
    )
    .unwrap();
    git.run(into, ["config", "user.email", "other@example.com"])
        .unwrap();
    git.run(into, ["config", "user.name", "Other"]).unwrap();
}

fn read_text(path: &Path) -> String {
    fs::read_to_string(path).unwrap().replace("\r\n", "\n")
}

pub(crate) fn git_state(cx: &TestAppContext) -> gpui::Entity<crate::git::GitState> {
    cx.read(|cx| cx.global::<crate::git::ActiveGit>().0.clone())
}

pub(crate) fn wait_for_repository(
    cx: &mut TestAppContext,
    state: &gpui::Entity<crate::git::GitState>,
) {
    wait_for(cx, "the repository of the window", |cx| {
        state.read_with(cx, |state, _| state.repo().is_some())
    });
}

fn run_command(cx: &mut TestAppContext, window: WindowHandle<Workspace>, command: &'static str) {
    window
        .update(cx, |workspace, window, cx| {
            workspace.run_command(command, window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
}

fn right_click(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) {
    let position = bounds_of(cx, window, selector).center();
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_down(position, MouseButton::Right, Modifiers::default());
    visual.simulate_mouse_up(position, MouseButton::Right, Modifiers::default());
    cx.run_until_parked();
    draw(cx, window);
}

fn click_count(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    position: gpui::Point<gpui::Pixels>,
    count: usize,
    modifiers: Modifiers,
) {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    for clicks in 1..=count {
        visual.simulate_event(MouseDownEvent {
            button: MouseButton::Left,
            position,
            modifiers,
            click_count: clicks,
            first_mouse: false,
        });
        visual.simulate_event(MouseUpEvent {
            button: MouseButton::Left,
            position,
            modifiers,
            click_count: clicks,
        });
    }
    cx.run_until_parked();
}

pub(crate) fn has_selector(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &'static str,
) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear(cx);
    });
    visual.debug_bounds(selector).is_some()
}

#[ui_kit::gpui::test]
fn stash_takes_saved_changes_and_unstash_applies_pops_and_drops_them(cx: &mut TestAppContext) {
    let (root, _bare, _git) = repository_with_origin("git-stash");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    let path = root.join("main.txt");
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    view.update(cx, |view, cx| view.replace_text(0..5, "ALPHA", cx));
    assert_eq!(buffer_of(cx, &view), "ALPHA\nbeta\n");

    // Stash writes the buffer first, then takes the change away and reloads the buffer.
    run_command(cx, window, "git.stash");
    type_text(cx, window, "my work");
    press(cx, window, "enter");
    wait_for(cx, "the stash", |cx| {
        let repo = state.read_with(cx, |state, _| state.repo()).unwrap();
        state.read_with(cx, |state, _| state.busy().is_none())
            && !repo.stash_list().unwrap().is_empty()
    });
    assert_eq!(read_text(&path), "alpha\nbeta\n");
    wait_for(cx, "the buffer to read the disk", |cx| {
        buffer_of(cx, &view) == "alpha\nbeta\n"
    });

    // The first stash is Apply, the second is Pop, the third is Drop after a question.
    run_command(cx, window, "git.unstash");
    wait_for(cx, "the stash menu", |cx| {
        has_selector(cx, window, "menu-0")
    });
    click(cx, window, "menu-1");
    wait_for(cx, "the applied text", |cx| {
        buffer_of(cx, &view) == "ALPHA\nbeta\n"
            && state.read_with(cx, |state, _| state.busy().is_none())
    });
    let stashes = state
        .read_with(cx, |state, _| state.repo())
        .and_then(|repo| repo.stash_list().ok())
        .unwrap_or_default();
    assert_eq!(stashes.len(), 1, "apply keeps the stash");

    // Drop takes the stash away after the question of the menu.
    run_command(cx, window, "git.unstash");
    wait_for(cx, "the stash menu", |cx| {
        has_selector(cx, window, "menu-0")
    });
    click(cx, window, "menu-3");
    wait_for(cx, "the drop question", |cx| {
        has_selector(cx, window, "prompt")
    });
    press(cx, window, "enter");
    wait_for(cx, "the dropped stash", |cx| {
        state.read_with(cx, |state, _| state.busy().is_none())
            && state
                .read_with(cx, |state, _| state.repo())
                .is_some_and(|repo| repo.stash_list().unwrap().is_empty())
    });

    // A new stash of the same change is then popped: it restores and drops it.
    run_command(cx, window, "git.stash");
    type_text(cx, window, "second");
    press(cx, window, "enter");
    wait_for(cx, "the second stash", |cx| {
        buffer_of(cx, &view) == "alpha\nbeta\n"
            && state.read_with(cx, |state, _| state.busy().is_none())
            && state
                .read_with(cx, |state, _| state.repo())
                .is_some_and(|repo| repo.stash_list().unwrap().len() == 1)
    });
    run_command(cx, window, "git.unstash");
    wait_for(cx, "the stash menu", |cx| {
        has_selector(cx, window, "menu-0")
    });
    click(cx, window, "menu-2");
    wait_for(cx, "the popped text", |cx| {
        buffer_of(cx, &view) == "ALPHA\nbeta\n"
            && state.read_with(cx, |state, _| state.busy().is_none())
    });
    let stashes = state
        .read_with(cx, |state, _| state.repo())
        .and_then(|repo| repo.stash_list().ok())
        .unwrap_or_default();
    assert!(stashes.is_empty(), "pop drops the stash");
}

#[ui_kit::gpui::test]
fn fetch_updates_the_counts_without_merging(cx: &mut TestAppContext) {
    let (root, bare, git) = repository_with_origin("git-fetch");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);

    let other = scratch("git-fetch-other");
    clone(&git, &bare, &other);
    fs::write(other.join("remote.txt"), "remote\n").unwrap();
    git.run(&other, ["add", "remote.txt"]).unwrap();
    git.run(&other, ["commit", "-m", "remote change"]).unwrap();
    git.run(&other, ["push"]).unwrap();

    run_command(cx, window, "git.fetch");
    wait_for(cx, "the fetched branch", |cx| {
        state.read_with(cx, |state, _| state.status().behind == 1)
    });
    assert!(
        !root.join("remote.txt").exists(),
        "Fetch does not touch the working tree"
    );
}

#[ui_kit::gpui::test]
fn ctrl_t_at_a_diverged_branch_asks_merge_or_rebase_and_merges(cx: &mut TestAppContext) {
    let (root, bare, git) = repository_with_origin("git-update-merge");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);

    let other = scratch("git-update-merge-other");
    clone(&git, &bare, &other);
    fs::write(other.join("remote.txt"), "remote\n").unwrap();
    git.run(&other, ["add", "remote.txt"]).unwrap();
    git.run(&other, ["commit", "-m", "remote change"]).unwrap();
    git.run(&other, ["push"]).unwrap();
    fs::write(root.join("local.txt"), "local\n").unwrap();
    git.run(&root, ["add", "local.txt"]).unwrap();
    git.run(&root, ["commit", "-m", "local change"]).unwrap();

    press(cx, window, "ctrl-t");
    wait_for(cx, "the merge or rebase choice", |cx| {
        has_selector(cx, window, "menu-0")
    });
    click(cx, window, "menu-0");

    wait_for(cx, "the merge commit", |cx| {
        state.read_with(cx, |state, _| {
            state
                .repo()
                .is_some_and(|repo| repo.log(1, 0).unwrap()[0].parents.len() == 2)
        })
    });
    assert_eq!(read_text(&root.join("remote.txt")), "remote\n");
    assert_eq!(read_text(&root.join("local.txt")), "local\n");
}

/// Two files conflict between the local and the remote commit.
pub(crate) fn conflicting_repository(name: &str) -> (PathBuf, PathBuf, vcs::GitRunner) {
    let git = git();
    let root = scratch(name);
    let bare = scratch(&format!("{name}-origin"));
    git.run(&bare, ["init", "--bare"]).unwrap();
    git.run(&root, ["init"]).unwrap();
    git.run(&root, ["config", "user.email", "window-test@example.com"])
        .unwrap();
    git.run(&root, ["config", "user.name", "Window Test"])
        .unwrap();
    for (file, text) in [("main.txt", "main\n"), ("second.txt", "second\n")] {
        fs::write(root.join(file), text).unwrap();
    }
    git.run(&root, ["add", "."]).unwrap();
    git.run(&root, ["commit", "-m", "base"]).unwrap();
    git.run(
        &root,
        [
            "remote",
            "add",
            "origin",
            bare.to_str().expect("a UTF-8 scratch path"),
        ],
    )
    .unwrap();
    git.run(&root, ["push", "-u", "origin", "HEAD"]).unwrap();

    let other = scratch(&format!("{name}-other"));
    clone(&git, &bare, &other);
    fs::write(other.join("main.txt"), "remote main\n").unwrap();
    fs::write(other.join("second.txt"), "remote second\n").unwrap();
    git.run(&other, ["add", "."]).unwrap();
    git.run(&other, ["commit", "-m", "remote change"]).unwrap();
    git.run(&other, ["push"]).unwrap();

    fs::write(root.join("main.txt"), "local main\n").unwrap();
    fs::write(root.join("second.txt"), "local second\n").unwrap();
    git.run(&root, ["add", "."]).unwrap();
    git.run(&root, ["commit", "-m", "local change"]).unwrap();
    (root, bare, git)
}

pub(crate) fn start_conflicting_merge(
    cx: &mut TestAppContext,
    root: PathBuf,
) -> (WindowHandle<Workspace>, gpui::Entity<crate::git::GitState>) {
    let window = open_window(cx, root);
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    press(cx, window, "ctrl-t");
    wait_for(cx, "the update choice", |cx| {
        has_selector(cx, window, "menu-0")
    });
    click(cx, window, "menu-0");
    wait_for(cx, "the conflicts window", |cx| {
        has_selector(cx, window, "conflict-row-0")
            && state.read_with(cx, |state, _| state.operation() == vcs::Operation::Merge)
    });
    (window, state)
}

#[ui_kit::gpui::test]
fn a_conflict_takes_theirs_for_one_file_and_a_hand_edit_for_the_other(cx: &mut TestAppContext) {
    let (root, _bare, _git) = conflicting_repository("git-conflict");
    let (window, state) = start_conflicting_merge(cx, root.clone());

    click(cx, window, "conflict-theirs-0");
    wait_for(cx, "the first file resolved", |cx| {
        state.read_with(cx, |state, _| state.conflicts() == ["second.txt"])
    });
    assert_eq!(read_text(&root.join("main.txt")), "remote main\n");

    // The other file is edited by hand and then marked resolved.
    let second = root.join("second.txt");
    open_path(cx, window, &second);
    let view = editor_of(cx, window, &second);
    let length = buffer_of(cx, &view).len();
    view.update(cx, |view, cx| {
        view.replace_text(0..length, "resolved\n", cx)
    });
    assert!(view.update(cx, |view, cx| view.save_blocking(cx)).is_none());
    click(cx, window, "conflict-resolve-0");
    wait_for(cx, "every conflict resolved", |cx| {
        state.read_with(cx, |state, _| state.conflicts().is_empty())
    });

    // With no conflicts the window asks for the message of the merge commit.
    wait_for(cx, "the merge commit button", |cx| {
        has_selector(cx, window, "conflict-commit")
    });
    click(cx, window, "conflict-commit");
    wait_for(cx, "the finished merge", |cx| {
        state.read_with(cx, |state, _| state.operation() == vcs::Operation::None)
    });
    assert_eq!(read_text(&root.join("main.txt")), "remote main\n");
    assert_eq!(read_text(&root.join("second.txt")), "resolved\n");
    let log = state
        .read_with(cx, |state, _| state.repo())
        .and_then(|repo| repo.log(1, 0).ok())
        .unwrap_or_default();
    assert_eq!(log[0].parents.len(), 2, "the merge commit has two parents");
}

#[ui_kit::gpui::test]
fn ctrl_t_a_conflict_and_commit_merge_put_the_uncommitted_change_back(cx: &mut TestAppContext) {
    let (root, _bare, git) = conflicting_repository("git-autostash");
    let keep = root.join("keep.txt");
    fs::write(&keep, "keep\n").unwrap();
    git.run(&root, ["add", "keep.txt"]).unwrap();
    git.run(&root, ["commit", "-m", "keep"]).unwrap();

    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);

    // The editor holds a change that the merge does not touch.
    open_path(cx, window, &keep);
    let view = editor_of(cx, window, &keep);
    view.update(cx, |view, cx| view.replace_text(0..4, "edited", cx));
    assert_eq!(buffer_of(cx, &view), "edited\n");

    press(cx, window, "ctrl-t");
    wait_for(cx, "the update choice", |cx| {
        has_selector(cx, window, "menu-0")
    });
    click(cx, window, "menu-0");
    wait_for(cx, "the conflicts window", |cx| {
        has_selector(cx, window, "conflict-theirs-0")
            && state.read_with(cx, |state, _| state.operation() == vcs::Operation::Merge)
    });
    wait_for(cx, "the note about the local changes", |cx| {
        has_selector(cx, window, "conflict-autostash")
    });

    click(cx, window, "conflict-theirs-0");
    wait_for(cx, "the first conflict resolved", |cx| {
        state.read_with(cx, |state, _| state.conflicts().len() == 1)
    });
    click(cx, window, "conflict-theirs-0");
    wait_for(cx, "every conflict resolved", |cx| {
        state.read_with(cx, |state, _| state.conflicts().is_empty())
    });
    wait_for(cx, "the merge commit button", |cx| {
        has_selector(cx, window, "conflict-commit")
    });
    click(cx, window, "conflict-commit");
    wait_for(cx, "the finished merge", |cx| {
        state.read_with(cx, |state, _| state.operation() == vcs::Operation::None)
    });

    wait_for(cx, "the returned change in the buffer", |cx| {
        buffer_of(cx, &view) == "edited\n"
    });
    assert_eq!(read_text(&keep), "edited\n");
    let stashes = state
        .read_with(cx, |state, _| state.repo())
        .and_then(|repo| repo.stash_list().ok())
        .unwrap_or_default();
    assert!(
        stashes.is_empty(),
        "the autostash applied and left no stash"
    );
}

#[ui_kit::gpui::test]
fn abort_leaves_the_local_branch_as_it_was(cx: &mut TestAppContext) {
    let (root, _bare, _git) = conflicting_repository("git-abort");
    let (window, state) = start_conflicting_merge(cx, root.clone());

    click(cx, window, "conflict-abort");
    wait_for(cx, "the aborted merge", |cx| {
        state.read_with(cx, |state, _| state.operation() == vcs::Operation::None)
    });
    assert_eq!(read_text(&root.join("main.txt")), "local main\n");
    assert_eq!(read_text(&root.join("second.txt")), "local second\n");
    let log = state
        .read_with(cx, |state, _| state.repo())
        .and_then(|repo| repo.log(1, 0).ok())
        .unwrap_or_default();
    assert_eq!(log[0].subject, "local change");
    assert_eq!(log[0].parents.len(), 1);
}

#[ui_kit::gpui::test]
fn the_log_lists_commits_and_double_click_on_a_file_opens_its_diff(cx: &mut TestAppContext) {
    let root = crate::window_tests::sample_project("git-log");
    let git = git();
    git.run(&root, ["init"]).unwrap();
    git.run(&root, ["config", "user.email", "window-test@example.com"])
        .unwrap();
    git.run(&root, ["config", "user.name", "Window Test"])
        .unwrap();
    git.run(&root, ["add", "."]).unwrap();
    git.run(&root, ["commit", "-m", "base"]).unwrap();
    fs::write(root.join("README.txt"), "A sample project.\nSecond line.\n").unwrap();
    git.run(&root, ["add", "README.txt"]).unwrap();
    git.run(&root, ["commit", "-m", "extend the readme"])
        .unwrap();

    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);

    press(cx, window, "alt-9");
    wait_for(cx, "the log", |cx| has_selector(cx, window, "log-commit-0"));
    // The filter keeps the commits that contain the text.
    type_text(cx, window, "extend");
    wait_for(cx, "the filtered log", |cx| {
        cx.read(|cx| cx.global::<ActiveGitLog>().0.read(cx).commits().len() == 1)
    });
    press(
        cx,
        window,
        "backspace backspace backspace backspace backspace backspace",
    );
    wait_for(cx, "the whole log", |cx| {
        cx.read(|cx| cx.global::<ActiveGitLog>().0.read(cx).commits().len() == 2)
    });
    cx.executor().advance_clock(Duration::from_millis(5000));
    cx.run_until_parked();
    draw(cx, window);
    click(cx, window, "log-commit-0");
    wait_for(cx, "the changed files", |cx| {
        has_selector(cx, window, "log-file-0")
    });
    let position = bounds_of(cx, window, "log-file-0").center();
    click_count(cx, window, position, 2, Modifiers::default());
    wait_for(cx, "the diff from the log", |cx| {
        has_selector(cx, window, "diff-title")
    });

    // The tree menu shows the log of exactly that file.
    press(cx, window, "escape");
    let panel = cx.update(|cx| cx.global::<crate::project::ActiveProjectPanel>().0.clone());
    let readme = root.join("README.txt");
    with_window(cx, window, |_, cx| {
        panel.update(cx, |panel, cx| panel.select_path(&readme, cx))
    });
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(workspace::PROJECT_TOOL, window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
    right_click(cx, window, "tree-row-README.txt");
    click(cx, window, "menu-11");
    wait_for(cx, "the log of the file", |cx| {
        cx.read(|cx| {
            let log = cx.global::<ActiveGitLog>().0.clone();
            log.read(cx)
                .path()
                .is_some_and(|path| path.ends_with("README.txt"))
                && !log.read(cx).commits().is_empty()
        })
    });
    let subjects: Vec<String> = cx.read(|cx| {
        cx.global::<ActiveGitLog>()
            .0
            .read(cx)
            .commits()
            .iter()
            .map(|commit| commit.subject.clone())
            .collect()
    });
    assert_eq!(subjects, ["extend the readme", "base"]);
}
