//! The Git window of the window without a window: the tabs of Local Changes and Log, the
//! branch tree, the paging of a long history, the filters, the files and the details of a
//! commit, the dividers of the three parts of the Log and the button of the left stripe.
//! Every repository lives in a scratch folder with its own identity.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ui_kit::gpui;
use ui_kit::gpui::{Modifiers, MouseButton, TestAppContext, VisualTestContext, WindowHandle};
use ui_kit::{point, px};
use vcs::GitRunner;
use workspace::{LOG_TOOL, Workspace};

use crate::git_log::{ActiveGitLog, GitTab};
use crate::script::ActiveCommitPanel;
use crate::window_tests::{
    bounds_of, click, draw, open_window, press, scratch, type_text, wait_for_settled_bounds,
};
use crate::window_tests_git::{git, git_state, has_selector, wait_for, wait_for_repository};

fn repository(name: &str) -> (PathBuf, GitRunner) {
    let root = scratch(name);
    let git = git();
    git.run(&root, ["init"]).unwrap();
    git.run(&root, ["symbolic-ref", "HEAD", "refs/heads/main"])
        .unwrap();
    git.run(&root, ["config", "user.email", "window-test@example.com"])
        .unwrap();
    git.run(&root, ["config", "user.name", "Window Test"])
        .unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::create_dir_all(root.join("docs")).unwrap();
    fs::write(root.join("src/main.txt"), "base\n").unwrap();
    fs::write(root.join("README.txt"), "read me\n").unwrap();
    git.run(&root, ["add", "."]).unwrap();
    git.run(&root, ["commit", "-m", "base"]).unwrap();
    git.run(&root, ["tag", "v1.0"]).unwrap();
    git.run(&root, ["tag", "release/1.0"]).unwrap();

    git.run(&root, ["switch", "-c", "feature/login"]).unwrap();
    fs::write(root.join("src/login.txt"), "login\n").unwrap();
    git.run(&root, ["add", "."]).unwrap();
    git.run(&root, ["commit", "-m", "login form"]).unwrap();
    let login = head(&git, &root);

    git.run(&root, ["switch", "main"]).unwrap();
    git.run(&root, ["switch", "-c", "feature/signup"]).unwrap();
    fs::write(root.join("src/signup.txt"), "sign up\n").unwrap();
    git.run(&root, ["add", "."]).unwrap();
    git.run(&root, ["commit", "-m", "sign up"]).unwrap();

    git.run(&root, ["switch", "main"]).unwrap();
    fs::write(root.join("src/main.txt"), "base\nmore\n").unwrap();
    fs::write(root.join("docs/guide.txt"), "guide\n").unwrap();
    git.run(&root, ["add", "."]).unwrap();
    git.run(
        &root,
        [
            "commit",
            "--author",
            "Other <other@example.com>",
            "-m",
            "other change",
        ],
    )
    .unwrap();
    let main = head(&git, &root);
    git.run(&root, ["update-ref", "refs/remotes/origin/main", &main])
        .unwrap();
    git.run(
        &root,
        ["update-ref", "refs/remotes/origin/feature/login", &login],
    )
    .unwrap();
    (root, git)
}

fn head(git: &GitRunner, root: &Path) -> String {
    String::from_utf8_lossy(&git.run(root, ["rev-parse", "HEAD"]).unwrap())
        .trim()
        .to_owned()
}

pub(crate) fn open_log(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    press(cx, window, "alt-9");
    wait_for(cx, "the log", |cx| has_selector(cx, window, "log-commit-0"));
    cx.executor().advance_clock(Duration::from_millis(5000));
    cx.run_until_parked();
    draw(cx, window);
}

pub(crate) fn log<T>(
    cx: &TestAppContext,
    read: impl FnOnce(&crate::git_log::GitLogPanel) -> T,
) -> T {
    cx.read(|cx| read(cx.global::<ActiveGitLog>().0.read(cx)))
}

fn double_click(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) {
    let position = bounds_of(cx, window, selector).center();
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    for click_count in 1..=2 {
        visual.simulate_event(gpui::MouseDownEvent {
            button: MouseButton::Left,
            position,
            modifiers: Modifiers::default(),
            click_count,
            first_mouse: false,
        });
        visual.simulate_event(gpui::MouseUpEvent {
            button: MouseButton::Left,
            position,
            modifiers: Modifiers::default(),
            click_count,
        });
    }
    cx.run_until_parked();
    draw(cx, window);
}

fn drag(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &'static str,
    delta: (f32, f32),
) {
    let start = bounds_of(cx, window, selector).center();
    let end = point(start.x + px(delta.0), start.y + px(delta.1));
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
    visual.simulate_mouse_move(end, MouseButton::Left, Modifiers::default());
    visual.simulate_mouse_up(end, MouseButton::Left, Modifiers::default());
    cx.run_until_parked();
    draw(cx, window);
}

#[ui_kit::gpui::test]
fn the_three_parts_of_the_log_are_visible_and_their_dividers_move(cx: &mut TestAppContext) {
    let (root, _git) = repository("git-window-parts");
    let window = open_window(cx, root);
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    open_log(cx, window);

    for selector in [
        "log-branches",
        "log-branch-search",
        "log-commits",
        "log-files",
        "log-details",
        "log-split-left",
        "log-split-right",
        "log-split-files",
    ] {
        assert!(
            has_selector(cx, window, selector),
            "the Log shows {selector}"
        );
    }
    wait_for_settled_bounds(cx, window, "log-split-left");
    let (left, right, files) = log(cx, |panel| panel.panes());
    drag(cx, window, "log-split-left", (40., 0.));
    let (after_left, _, _) = log(cx, |panel| panel.panes());
    assert!(
        after_left > left,
        "the left divider moves the branch tree: {left} -> {after_left}"
    );
    drag(cx, window, "log-split-right", (-40., 0.));
    let (_, after_right, _) = log(cx, |panel| panel.panes());
    assert!(
        after_right > right,
        "the right divider moves the files and details: {right} -> {after_right}"
    );
    drag(cx, window, "log-split-files", (0., 30.));
    let (_, _, after_files) = log(cx, |panel| panel.panes());
    assert!(
        after_files > files,
        "the lower divider moves the details: {files} -> {after_files}"
    );
}

#[ui_kit::gpui::test]
fn the_branch_search_filters_the_tree(cx: &mut TestAppContext) {
    let (root, _git) = repository("git-window-branch-search");
    let window = open_window(cx, root);
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    open_log(cx, window);
    wait_for(cx, "the branches of the tree", |cx| {
        log(cx, |panel| !panel.visible_branches().is_empty())
    });

    let panel = cx.read(|cx| cx.global::<ActiveGitLog>().0.clone());
    panel.update(cx, |panel, cx| panel.set_branch_query("login", cx));
    cx.run_until_parked();
    draw(cx, window);

    let visible = log(cx, |panel| panel.visible_branches());
    assert!(
        visible.iter().any(|name| name == "feature/login"),
        "the search keeps feature/login: {visible:?}"
    );
    let tree = log(cx, |panel| panel.branch_tree());
    assert!(
        tree.iter()
            .any(|(depth, label)| *depth == 2 && label == "login"),
        "the matching branch keeps its folder: {tree:?}"
    );
    assert!(
        !visible.iter().any(|name| name == "feature/signup"),
        "the search hides feature/signup: {visible:?}"
    );
    assert!(
        !visible.iter().any(|name| name == "main"),
        "the search hides main: {visible:?}"
    );
}

#[ui_kit::gpui::test]
fn a_click_on_a_branch_filters_the_commits(cx: &mut TestAppContext) {
    let (root, _git) = repository("git-window-branch-click");
    let window = open_window(cx, root);
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    open_log(cx, window);
    wait_for(cx, "the branch of the tree", |cx| {
        log(cx, |panel| panel.branch_row("feature/login").is_some())
    });
    wait_for_settled_bounds(
        cx,
        window,
        Box::leak(
            format!(
                "log-branch-{}",
                log(cx, |panel| panel.branch_row("feature/login").unwrap())
            )
            .into_boxed_str(),
        ),
    );

    let row = log(cx, |panel| panel.branch_row("feature/login").unwrap());
    let selector: &'static str = Box::leak(format!("log-branch-{row}").into_boxed_str());
    click(cx, window, selector);

    wait_for(cx, "the log of the branch", |cx| {
        log(cx, |panel| {
            panel.branch_filter() == Some("feature/login")
                && panel
                    .commits()
                    .first()
                    .map(|commit| commit.subject.as_str())
                    == Some("login form")
        })
    });
    let subjects: Vec<String> = log(cx, |panel| {
        panel
            .commits()
            .into_iter()
            .map(|commit| commit.subject.clone())
            .collect()
    });
    assert!(
        !subjects.iter().any(|subject| subject == "other change"),
        "the branch does not show the commit of main: {subjects:?}"
    );
}

#[ui_kit::gpui::test]
fn the_user_and_text_filters_keep_their_commits(cx: &mut TestAppContext) {
    let (root, _git) = repository("git-window-filters");
    let window = open_window(cx, root);
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    open_log(cx, window);
    wait_for(cx, "the whole log", |cx| {
        log(cx, |panel| panel.commits().len()) >= 2
    });

    let panel = cx.read(|cx| cx.global::<ActiveGitLog>().0.clone());
    panel.update(cx, |panel, cx| panel.set_filter("other", cx));
    let subjects: Vec<String> = log(cx, |panel| {
        panel
            .commits()
            .into_iter()
            .map(|commit| commit.subject.clone())
            .collect()
    });
    assert_eq!(
        subjects,
        ["other change"],
        "Text keeps the commit that matches"
    );

    panel.update(cx, |panel, cx| {
        panel.set_regex(true, false, cx);
        panel.set_filter("oth.r chan.e", cx)
    });
    let subjects: Vec<String> = log(cx, |panel| {
        panel
            .commits()
            .into_iter()
            .map(|commit| commit.subject.clone())
            .collect()
    });
    assert_eq!(
        subjects,
        ["other change"],
        "the regex switch reads the text as a regex"
    );

    panel.update(cx, |panel, cx| panel.set_filter("", cx));
    panel.update(cx, |panel, cx| {
        panel.set_user_filter(Some("Other".to_owned()), cx)
    });
    let authors: Vec<String> = log(cx, |panel| {
        panel
            .commits()
            .into_iter()
            .map(|commit| commit.author.clone())
            .collect()
    });
    assert_eq!(authors, ["Other"], "User keeps the commits of one author");
}

#[ui_kit::gpui::test]
fn the_selected_commit_shows_its_files_as_a_tree_and_its_details(cx: &mut TestAppContext) {
    let (root, _git) = repository("git-window-details");
    let window = open_window(cx, root);
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    open_log(cx, window);
    wait_for_settled_bounds(cx, window, "log-commit-0");
    click(cx, window, "log-commit-0");

    wait_for(cx, "the files and the details of the commit", |cx| {
        log(cx, |panel| {
            panel.files().len() == 2 && panel.details().is_some()
        })
    });
    assert!(
        has_selector(cx, window, "log-folder-0"),
        "the files are a tree of the folders of the commit"
    );
    assert!(
        has_selector(cx, window, "log-file-0") && has_selector(cx, window, "log-file-1"),
        "both files of the commit are rows of the tree"
    );
    assert!(
        has_selector(cx, window, "log-details-hash"),
        "the details show the hash"
    );
    assert!(
        has_selector(cx, window, "log-details-branches"),
        "the details name the branches of the commit"
    );
    let branches = log(cx, |panel| {
        panel.details().map(|details| details.branches.clone())
    })
    .unwrap_or_default();
    assert!(
        branches.len() >= 2,
        "the commit is on main and its remote: {branches:?}"
    );
}

#[ui_kit::gpui::test]
fn a_double_click_on_a_file_of_the_commit_opens_its_diff(cx: &mut TestAppContext) {
    let (root, _git) = repository("git-window-file-diff");
    let window = open_window(cx, root);
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    open_log(cx, window);
    wait_for_settled_bounds(cx, window, "log-commit-0");
    click(cx, window, "log-commit-0");
    wait_for(cx, "the file of the commit", |cx| {
        has_selector(cx, window, "log-file-0")
    });

    double_click(cx, window, "log-file-0");

    wait_for(cx, "the diff of the file", |cx| {
        has_selector(cx, window, "diff-title")
    });
}

#[ui_kit::gpui::test]
fn the_local_changes_and_log_tabs_switch(cx: &mut TestAppContext) {
    let (root, _git) = repository("git-window-tabs");
    let window = open_window(cx, root);
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    open_log(cx, window);
    assert_eq!(log(cx, |panel| panel.tab()), GitTab::Log);

    click(cx, window, "git-tab-local");
    wait_for(cx, "the Local Changes tab", |cx| {
        log(cx, |panel| panel.tab()) == GitTab::LocalChanges
    });
    assert!(
        has_selector(cx, window, "commit-panel-local"),
        "Local Changes shows the changed files of the Commit window"
    );

    click(cx, window, "git-tab-log");
    wait_for(cx, "the Log tab", |cx| {
        log(cx, |panel| panel.tab()) == GitTab::Log
    });
    assert!(has_selector(cx, window, "log-commit-0"));
}

#[ui_kit::gpui::test]
fn alt_9_opens_the_log_tab(cx: &mut TestAppContext) {
    let (root, _git) = repository("git-window-alt-9");
    let window = open_window(cx, root);
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    open_log(cx, window);

    let panel = cx.read(|cx| cx.global::<ActiveGitLog>().0.clone());
    panel.update(cx, |panel, cx| panel.set_tab(GitTab::LocalChanges, cx));
    cx.run_until_parked();
    draw(cx, window);
    assert_eq!(log(cx, |panel| panel.tab()), GitTab::LocalChanges);

    press(cx, window, "alt-9");
    wait_for(cx, "the Log tab of the Git window", |cx| {
        log(cx, |panel| panel.tab()) == GitTab::Log
    });
}

#[ui_kit::gpui::test]
fn the_git_button_stands_in_the_lower_group_of_the_left_stripe(cx: &mut TestAppContext) {
    let (root, _git) = repository("git-window-stripe");
    let window = open_window(cx, root);
    cx.run_until_parked();
    draw(cx, window);
    let commit = bounds_of(cx, window, "stripe-s-commit");
    let terminal = bounds_of(cx, window, "stripe-s-terminal");
    let log = bounds_of(cx, window, "stripe-s-log");
    let problems = bounds_of(cx, window, "stripe-s-problems");

    assert!(
        log.top() > commit.bottom(),
        "the Git button left the upper group of the stripe"
    );
    assert!(
        log.top() > terminal.bottom(),
        "the Git button stands below Terminal"
    );
    assert!(
        log.bottom() < problems.top(),
        "the Git button stands above Problems"
    );
    assert!(
        !window
            .read_with(cx, |workspace, _| workspace.tool_open(LOG_TOOL))
            .unwrap_or(false),
        "the tool window itself stays closed until it is asked for"
    );
}

#[ui_kit::gpui::test]
fn a_log_of_three_thousand_commits_opens_in_pages_and_scrolls(cx: &mut TestAppContext) {
    let root = scratch("git-window-long-log");
    let git = git();
    git.run(&root, ["init"]).unwrap();
    git.run(&root, ["symbolic-ref", "HEAD", "refs/heads/main"])
        .unwrap();
    let mut stream = String::new();
    stream.push_str("blob\nmark :1\ndata 2\nx\n");
    for index in 0..3000 {
        let message = format!("commit {index}");
        stream.push_str(&format!(
            "commit refs/heads/main\nmark :{}\nauthor Window Test <window-test@example.com> {} +0000\ncommitter Window Test <window-test@example.com> {} +0000\ndata {}\n{}\nM 100644 :1 src.txt\n\n",
            index + 2,
            1_700_000_000 + index,
            1_700_000_000 + index,
            message.len(),
            message
        ));
    }
    git.run_with_input(&root, ["fast-import"], Some(stream.as_bytes()))
        .unwrap();

    let window = open_window(cx, root);
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    open_log(cx, window);
    wait_for(cx, "the first page of a long log", |cx| {
        log(cx, |panel| panel.loaded() == 500)
    });
    assert!(log(cx, |panel| panel.more()), "the log has more pages");

    let panel = cx.read(|cx| cx.global::<ActiveGitLog>().0.clone());
    panel.update(cx, |panel, cx| panel.load_next(cx));
    wait_for(cx, "the second page of a long log", |cx| {
        log(cx, |panel| panel.loaded() == 1000)
    });
    cx.executor().advance_clock(Duration::from_millis(5000));
    cx.run_until_parked();
    draw(cx, window);
    assert!(
        has_selector(cx, window, "log-commit-0"),
        "the log of three thousand commits draws its first row"
    );
}

fn focus<T: 'static>(cx: &mut TestAppContext, window: WindowHandle<T>, handle: gpui::FocusHandle) {
    window
        .update(cx, |_, window, cx| window.focus(&handle, cx))
        .unwrap();
    cx.run_until_parked();
}

/// The Commit window and the Local Changes tab draw one draft: the message typed and the
/// tick taken off in one of them are the message and the tick of the other, both ways.
#[ui_kit::gpui::test]
fn the_commit_window_and_the_local_changes_tab_share_the_draft(cx: &mut TestAppContext) {
    let (root, _git) = repository("git-window-shared-draft");
    fs::write(root.join("src/main.txt"), "base\nchanged\n").unwrap();
    let window = open_window(cx, root);
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    wait_for(cx, "the changed file", |cx| {
        state.read_with(cx, |state, _| {
            state
                .status()
                .files
                .iter()
                .any(|file| file.path == "src/main.txt")
        })
    });

    press(cx, window, "alt-0");
    let commit = cx.read(|cx| cx.global::<ActiveCommitPanel>().0.clone());
    let handle = commit.read_with(cx, |panel, _| panel.focus());
    focus(cx, window, handle);
    type_text(cx, window, "shared message");
    commit.update(cx, |panel, cx| panel.set_checked("src/main.txt", false, cx));
    cx.run_until_parked();
    assert_eq!(
        commit.read_with(cx, |panel, _| panel.message().to_owned()),
        "shared message",
        "the Commit window keeps the typed message"
    );

    open_log(cx, window);
    click(cx, window, "git-tab-local");
    wait_for(cx, "the Local Changes tab", |cx| {
        log(cx, |panel| panel.tab()) == GitTab::LocalChanges
    });
    let local = log(cx, |panel| {
        panel
            .commit_panel()
            .expect("Local Changes shows the Commit window")
    });
    assert!(
        has_selector(cx, window, "commit-panel-local"),
        "the tab draws its own view of the commit"
    );
    assert!(
        has_selector(cx, window, "commit-panel"),
        "the Commit window keeps its own element id, so a window with both never repeats one"
    );
    let (message, checked) = local.read_with(cx, |panel, _| {
        (panel.message().to_owned(), panel.is_checked("src/main.txt"))
    });
    assert_eq!(
        message, "shared message",
        "Local Changes shows the message typed in the Commit window"
    );
    assert!(
        !checked,
        "Local Changes shows the tick taken off in the Commit window"
    );

    // And back: what the tab changes is what the Commit window shows.
    let handle = local.read_with(cx, |panel, _| panel.focus());
    focus(cx, window, handle);
    type_text(cx, window, " from the tab");
    local.update(cx, |panel, cx| panel.set_checked("src/main.txt", true, cx));
    cx.run_until_parked();
    let (message, checked) = commit.read_with(cx, |panel, _| {
        (panel.message().to_owned(), panel.is_checked("src/main.txt"))
    });
    assert_eq!(
        message, "shared message from the tab",
        "the Commit window shows the message typed in Local Changes"
    );
    assert!(
        checked,
        "the Commit window shows the tick set in Local Changes"
    );
}
