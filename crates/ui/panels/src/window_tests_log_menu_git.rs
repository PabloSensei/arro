//! The Git menu of the window and the log of several commits: the actions the menu lists,
//! Cherry-Pick of a selection, Compare Revisions and the Paths filter of one folder. Every
//! repository lives in a scratch directory and its identity is set there, so the machine
//! is left alone.

use std::fs;
use std::path::Path;
use std::time::Duration;

use ui_kit::MenuEntry;
use ui_kit::Modifiers;
use ui_kit::gpui;
use ui_kit::gpui::{
    MouseButton, MouseDownEvent, MouseUpEvent, TestAppContext, VisualTestContext, WindowHandle,
};
use workspace::Workspace;

use crate::compare::CompareView;
use crate::git_log::ActiveGitLog;
use crate::window_tests::{
    bounds_of, click, draw, open_path, open_window, press, scratch, type_text,
};
use crate::window_tests_git::{
    git_state, has_selector, repository_with_origin, wait_for, wait_for_repository,
};

const GIT_ROWS: [&str; 7] = [
    "Annotate with Git Blame",
    "Rebase...",
    "Cherry-pick",
    "Revert Commit",
    "Reset HEAD...",
    "Shelve Changes...",
    "Unshelve...",
];

fn read_text(path: &Path) -> String {
    fs::read_to_string(path).unwrap().replace("\r\n", "\n")
}

fn commit_change(git: &vcs::GitRunner, root: &Path, file: &str, text: &str, message: &str) {
    if let Some(parent) = root.join(file).parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(root.join(file), text).unwrap();
    git.run(root, ["add", "."]).unwrap();
    git.run(root, ["commit", "-m", message]).unwrap();
}

fn git_menu(cx: &mut TestAppContext, window: WindowHandle<Workspace>) -> Vec<MenuEntry> {
    window
        .update(cx, |workspace, _, cx| workspace.main_menu(cx))
        .unwrap()
        .into_iter()
        .find(|entry| entry.label == "Git")
        .expect("the window has a Git menu")
        .submenu
}

fn right_click(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) {
    let position = bounds_of(cx, window, selector).center();
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_down(position, MouseButton::Right, Modifiers::default());
    visual.simulate_mouse_up(position, MouseButton::Right, Modifiers::default());
    cx.run_until_parked();
    draw(cx, window);
}

fn click_with(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &'static str,
    modifiers: Modifiers,
) {
    let position = bounds_of(cx, window, selector).center();
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_click(position, modifiers);
    cx.run_until_parked();
    draw(cx, window);
}

fn open_log(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    press(cx, window, "alt-9");
    wait_for(cx, "the log", |cx| has_selector(cx, window, "log-commit-0"));
    cx.executor().advance_clock(Duration::from_millis(5000));
    cx.run_until_parked();
    draw(cx, window);
}

fn log_subjects(cx: &mut TestAppContext) -> Vec<String> {
    cx.read(|cx| {
        cx.global::<ActiveGitLog>()
            .0
            .read(cx)
            .commits()
            .iter()
            .map(|commit| commit.subject.clone())
            .collect()
    })
}

#[ui_kit::gpui::test]
fn the_git_menu_lists_the_new_rows_with_tips_and_greys_them_without_a_repository(
    cx: &mut TestAppContext,
) {
    let root = scratch("log-menu-no-repository");
    let window = open_window(cx, root);
    cx.run_until_parked();
    draw(cx, window);

    let rows = git_menu(cx, window);
    for label in GIT_ROWS {
        let row = rows
            .iter()
            .find(|entry| entry.label == label)
            .unwrap_or_else(|| panic!("{label} is missing from the Git menu"));
        assert!(row.tip.is_some(), "{label} has no tip");
        assert!(
            !row.is_enabled(),
            "{label} is not grey without a repository"
        );
    }
}

#[ui_kit::gpui::test]
fn the_git_menu_enables_the_actions_of_the_active_file_and_the_log(cx: &mut TestAppContext) {
    let (root, _bare, _git) = repository_with_origin("log-menu-repository");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    open_path(cx, window, &root.join("main.txt"));
    draw(cx, window);

    let rows = git_menu(cx, window);
    let row = |label: &str| rows.iter().find(|entry| entry.label == label).unwrap();
    assert!(row("Annotate with Git Blame").is_enabled());
    assert!(!row("Cherry-pick").is_enabled(), "no commit is picked");

    open_log(cx, window);
    click(cx, window, "log-commit-0");
    draw(cx, window);
    let rows = git_menu(cx, window);
    let row = |label: &str| rows.iter().find(|entry| entry.label == label).unwrap();
    assert!(row("Cherry-pick").is_enabled());
    assert!(row("Revert Commit").is_enabled());
    assert!(row("Reset HEAD...").is_enabled());
    assert!(row("Rebase...").is_enabled());
}

#[ui_kit::gpui::test]
fn cherry_pick_of_two_picked_commits_replays_them_from_the_oldest(cx: &mut TestAppContext) {
    let (root, _bare, git) = repository_with_origin("log-multi-cherry-pick");
    let main = String::from_utf8_lossy(
        &git.run(&root, ["rev-parse", "--abbrev-ref", "HEAD"])
            .unwrap(),
    )
    .trim()
    .to_owned();
    git.run(&root, ["switch", "-c", "side"]).unwrap();
    commit_change(&git, &root, "a.txt", "one\n", "first");
    commit_change(&git, &root, "b.txt", "two\n", "second");
    git.run(&root, ["switch", &main]).unwrap();
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    open_log(cx, window);

    // The log of the side branch lists the two commits that the base branch has not.
    let branch = cx.read(|cx| cx.global::<ActiveGitLog>().0.read(cx).branch_row("side"));
    let branch_row: &'static str =
        Box::leak(format!("log-branch-{}", branch.unwrap()).into_boxed_str());
    click(cx, window, branch_row);
    wait_for(cx, "the side log", |cx| {
        log_subjects(cx).first().map(String::as_str) == Some("second")
    });
    assert_eq!(log_subjects(cx), ["second", "first", "base"]);

    // The two commits of the log are picked, the newest first as the rows are.
    click_with(cx, window, "log-commit-0", Modifiers::default());
    click_with(
        cx,
        window,
        "log-commit-1",
        Modifiers {
            control: true,
            ..Modifiers::default()
        },
    );
    right_click(cx, window, "log-commit-1");
    wait_for(cx, "the commit menu", |cx| {
        has_selector(cx, window, "menu-0")
    });
    click(cx, window, "menu-5");

    wait_for(cx, "the two cherry-picked commits", |cx| {
        let subjects = log_subjects(cx);
        subjects.first().map(String::as_str) == Some("second")
            && subjects.get(1).map(String::as_str) == Some("first")
            && state.read_with(cx, |state, _| state.busy().is_none())
    });
    assert_eq!(read_text(&root.join("a.txt")), "one\n");
    assert_eq!(read_text(&root.join("b.txt")), "two\n");
}

#[ui_kit::gpui::test]
fn compare_revisions_lists_the_files_and_a_double_click_opens_their_diff(cx: &mut TestAppContext) {
    let (root, _bare, git) = repository_with_origin("log-compare-revisions");
    commit_change(&git, &root, "a.txt", "one\n", "one");
    commit_change(&git, &root, "main.txt", "alpha\nTWO\n", "two");
    let base = String::from_utf8_lossy(&git.run(&root, ["rev-parse", "HEAD~2"]).unwrap())
        .trim()
        .to_owned();
    let head = String::from_utf8_lossy(&git.run(&root, ["rev-parse", "HEAD"]).unwrap())
        .trim()
        .to_owned();
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    open_log(cx, window);
    assert_eq!(log_subjects(cx), ["two", "one", "base"]);

    // The newest commit and the base one are picked.
    click_with(cx, window, "log-commit-0", Modifiers::default());
    click_with(
        cx,
        window,
        "log-commit-2",
        Modifiers {
            control: true,
            ..Modifiers::default()
        },
    );
    right_click(cx, window, "log-commit-0");
    wait_for(cx, "the commit menu", |cx| {
        has_selector(cx, window, "menu-0")
    });
    click(cx, window, "menu-11");
    wait_for(cx, "the compare window", |cx| {
        has_selector(cx, window, "compare")
    });

    let view = window
        .read_with(cx, |workspace, _| workspace.overlay_view())
        .unwrap()
        .expect("the compare window is open");
    let view = view
        .downcast::<CompareView>()
        .map_err(|_| ())
        .expect("the open window compares the revisions");
    let (left, right) = view
        .read_with(cx, |view, _| {
            view.revisions()
                .map(|(left, right)| (left.to_owned(), right.to_owned()))
        })
        .expect("the window compares two revisions");
    assert_eq!(left, base);
    assert_eq!(right, head);
    let files = view.read_with(cx, |view, _| view.files().to_vec());
    assert!(files.contains(&"a.txt".to_owned()), "{files:?}");
    assert!(files.contains(&"main.txt".to_owned()), "{files:?}");

    // The first file of the list opens the diff of the two revisions.
    let position = bounds_of(cx, window, "compare-file-0").center();
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    for clicks in 1..=2 {
        visual.simulate_event(MouseDownEvent {
            button: MouseButton::Left,
            position,
            modifiers: Modifiers::default(),
            click_count: clicks,
            first_mouse: false,
        });
        visual.simulate_event(MouseUpEvent {
            button: MouseButton::Left,
            position,
            modifiers: Modifiers::default(),
            click_count: clicks,
        });
    }
    cx.run_until_parked();
    draw(cx, window);
    wait_for(cx, "the diff of the revisions", |cx| {
        has_selector(cx, window, "diff-title")
    });
}

#[ui_kit::gpui::test]
fn the_paths_field_keeps_the_commits_of_one_folder(cx: &mut TestAppContext) {
    let (root, _bare, git) = repository_with_origin("log-paths-folder");
    commit_change(&git, &root, "src/a.txt", "a\n", "the source");
    commit_change(&git, &root, "main.txt", "alpha\nROOT\n", "the root");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    open_log(cx, window);
    assert_eq!(log_subjects(cx), ["the root", "the source", "base"]);

    click(cx, window, "log-filter-path");
    type_text(cx, window, "src");
    press(cx, window, "enter");
    wait_for(cx, "the log of the folder", |cx| {
        log_subjects(cx) == ["the source"]
    });
    assert_eq!(
        cx.read(|cx| cx
            .global::<ActiveGitLog>()
            .0
            .read(cx)
            .path_filter()
            .map(Path::to_path_buf)),
        Some(root.join("src"))
    );
}

#[ui_kit::gpui::test]
fn the_git_menu_of_a_folder_shows_its_history(cx: &mut TestAppContext) {
    let (root, _bare, git) = repository_with_origin("log-tree-history");
    commit_change(&git, &root, "src/a.txt", "a\n", "the source");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    let panel = cx.update(|cx| cx.global::<crate::project::ActiveProjectPanel>().0.clone());
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(workspace::PROJECT_TOOL, window, cx)
        })
        .unwrap();
    crate::window_tests::with_window(cx, window, |_, cx| {
        panel.update(cx, |panel, cx| panel.select_path(&root.join("src"), cx))
    });
    draw(cx, window);
    right_click(cx, window, "tree-row-src");
    wait_for(cx, "the row menu", |cx| has_selector(cx, window, "menu-0"));
    let entries = window
        .read_with(cx, |workspace, _| {
            workspace.menu_entries().map(|entries| entries.to_vec())
        })
        .unwrap()
        .unwrap();
    let index = entries
        .iter()
        .position(|entry| entry.label == "Git")
        .expect("the menu of a folder has Git");
    let git_row: &'static str = Box::leak(format!("menu-{index}").into_boxed_str());
    click(cx, window, git_row);
    let show_row: &'static str = Box::leak(format!("menu-{index}-0").into_boxed_str());
    click(cx, window, show_row);
    wait_for(cx, "the history of the folder", |cx| {
        cx.read(|cx| {
            let log = cx.global::<ActiveGitLog>().0.read(cx);
            log.path().is_some_and(|path| path.ends_with("src")) && !log.commits().is_empty()
        })
    });
}
