//! The Commit window with shelves, change lists, grouping and commit options, and the
//! actions of the Git log, without a window. Every repository is a scratch folder.

use std::fs;
use std::path::Path;
use std::time::Duration;

use ui_kit::Modifiers;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use workspace::{COMMIT_TOOL, Workspace};

use crate::git_log::ActiveGitLog;
use crate::script::ActiveCommitPanel;
use crate::window_tests::{
    buffer_of, click, draw, editor_of, open_path, open_window, press, type_text,
};
use crate::window_tests_git::{
    git, git_state, has_selector, repository_with_origin, wait_for, wait_for_repository,
};

fn commit_panel(cx: &TestAppContext) -> gpui::Entity<crate::commit::CommitPanel> {
    cx.read(|cx| cx.global::<ActiveCommitPanel>().0.clone())
}

fn focus<T: 'static>(cx: &mut TestAppContext, window: WindowHandle<T>, handle: gpui::FocusHandle) {
    window
        .update(cx, |_, window, cx| window.focus(&handle, cx))
        .unwrap();
    cx.run_until_parked();
}

fn show_commit(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(COMMIT_TOOL, window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
}

/// Clicks an element by a selector built while the test runs.
fn click_selector(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &str) {
    draw(cx, window);
    let selector: &'static str = Box::leak(selector.to_string().into_boxed_str());
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    let bounds = visual
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("no element {selector} on screen"));
    visual.simulate_click(bounds.center(), Modifiers::default());
    cx.run_until_parked();
}

fn menu_index(cx: &TestAppContext, window: WindowHandle<Workspace>, label: &str) -> usize {
    window
        .read_with(cx, |workspace, _| {
            workspace.menu_entries().and_then(|entries| {
                entries
                    .iter()
                    .position(|entry| entry.label.as_ref() == label)
            })
        })
        .unwrap()
        .unwrap_or_else(|| panic!("no menu row {label}"))
}

fn click_menu(cx: &mut TestAppContext, window: WindowHandle<Workspace>, label: &str) {
    wait_for(cx, "the menu row", |cx| {
        window
            .read_with(cx, |workspace, _| {
                workspace.menu_entries().is_some_and(|entries| {
                    entries.iter().any(|entry| entry.label.as_ref() == label)
                })
            })
            .unwrap()
    });
    let index = menu_index(cx, window, label);
    click_selector(cx, window, &format!("menu-{index}"));
}

fn focus_panel(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    panel: &gpui::Entity<crate::commit::CommitPanel>,
) {
    let handle = panel.read_with(cx, |panel, _| panel.focus());
    focus(cx, window, handle);
}

fn read_text(path: &Path) -> String {
    fs::read_to_string(path).unwrap().replace("\r\n", "\n")
}

/// Lets the first load of the log finish, so that a click selects and no later reload
/// takes the selection away.
fn settle_log(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    cx.executor().advance_clock(Duration::from_millis(5000));
    cx.run_until_parked();
    // The slide of the dock runs on the real clock, not the simulated one: the frame
    // that the clicks hit test against waits until the log is in its place.
    std::thread::sleep(Duration::from_millis(200));
    draw(cx, window);
}

fn commit_file(root: &Path, path: &str, text: &str, message: &str) {
    fs::write(root.join(path), text).unwrap();
    let git = git();
    git.run(root, ["add", path]).unwrap();
    git.run(root, ["commit", "-m", message]).unwrap();
}

/// Makes a change in the editor, keeps the buffer unsaved and shelves it by the button.
fn shelve_the_change(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    root: &Path,
    message: &str,
) {
    let path = root.join("main.txt");
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    view.update(cx, |view, cx| {
        view.replace_text(0..5, "ALPHA", cx);
        assert!(view.save_blocking(cx).is_none());
    });
    let state = crate::window_tests_git::git_state(cx);
    state.update(cx, |state, cx| state.refresh(cx));
    wait_for(cx, "the changed file", |cx| {
        state.read_with(cx, |state, _| !state.status().files.is_empty())
    });
    show_commit(cx, window);
    let panel = commit_panel(cx);
    let handle = panel.read_with(cx, |panel, _| panel.focus());
    focus(cx, window, handle);
    type_text(cx, window, message);
    click(cx, window, "commit-shelve");
    wait_for(cx, "the shelf", |cx| {
        panel.read_with(cx, |panel, _| panel.shelves().len() == 1)
            && read_text(&path) == "alpha\nbeta\n"
    });
    wait_for(cx, "the buffer to read the disk", |cx| {
        buffer_of(cx, &view) == "alpha\nbeta\n"
    });
}

#[ui_kit::gpui::test]
fn shelve_takes_a_file_and_unshelve_with_delete_puts_it_back(cx: &mut TestAppContext) {
    let (root, _bare, _git) = repository_with_origin("shelve-round-trip");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);

    shelve_the_change(cx, window, &root, "my work");
    let panel = commit_panel(cx);
    let shelves = panel.read_with(cx, |panel, _| panel.shelves().to_vec());
    assert_eq!(shelves[0].name, "my work");
    assert_eq!(shelves[0].files, ["main.txt"]);

    // Ctrl+Shift+U opens the dialog, Delete shelf after apply is ticked, then Unshelve.
    let handle = panel.read_with(cx, |panel, _| panel.focus());
    focus(cx, window, handle);
    press(cx, window, "ctrl-shift-u");
    wait_for(cx, "the Unshelve dialog", |cx| {
        has_selector(cx, window, "unshelve-dialog")
    });
    click(cx, window, "unshelve-delete");
    click(cx, window, "unshelve-apply");
    let path = root.join("main.txt");
    wait_for(cx, "the unshelved text and the deleted shelf", |cx| {
        read_text(&path) == "ALPHA\nbeta\n"
            && panel.read_with(cx, |panel, _| panel.shelves().is_empty())
    });
    let view = editor_of(cx, window, &path);
    wait_for(cx, "the buffer to follow", |cx| {
        buffer_of(cx, &view) == "ALPHA\nbeta\n"
    });
}

#[ui_kit::gpui::test]
fn a_conflict_of_unshelve_names_the_files_and_keeps_the_shelf(cx: &mut TestAppContext) {
    let (root, _bare, _git) = repository_with_origin("shelve-conflict");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);

    shelve_the_change(cx, window, &root, "conflicting");
    let path = root.join("main.txt");
    // The disk moves on behind the clean editor, so the patch no longer fits.
    fs::write(&path, "something else\n").unwrap();
    let panel = commit_panel(cx);
    let handle = panel.read_with(cx, |panel, _| panel.focus());
    focus(cx, window, handle);
    press(cx, window, "ctrl-shift-u");
    wait_for(cx, "the Unshelve dialog", |cx| {
        has_selector(cx, window, "unshelve-dialog")
    });
    click(cx, window, "unshelve-apply");

    wait_for(cx, "the conflict report", |cx| {
        window
            .read_with(cx, |workspace, _| workspace.last_notification())
            .unwrap()
            .is_some_and(|(_, text)| text.contains("main.txt") && text.contains("conflict"))
    });
    assert_eq!(read_text(&path), "something else\n", "nothing is applied");
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.shelves().len()),
        1,
        "the shelf stays"
    );
}

#[ui_kit::gpui::test]
fn a_shelf_is_renamed_by_f2_and_deleted_after_a_question(cx: &mut TestAppContext) {
    let (root, _bare, _git) = repository_with_origin("shelve-rename");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);

    shelve_the_change(cx, window, &root, "first");
    let panel = commit_panel(cx);
    click(cx, window, "shelf-tab");
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.tab()),
        crate::commit::Tab::Shelf
    );
    click(cx, window, "shelf-row-0");
    let handle = panel.read_with(cx, |panel, _| panel.focus());
    focus(cx, window, handle);
    press(cx, window, "f2");
    wait_for(cx, "the rename question", |cx| {
        has_selector(cx, window, "prompt")
    });
    press(cx, window, "ctrl-a");
    type_text(cx, window, "renamed");
    press(cx, window, "enter");
    wait_for(cx, "the new shelf name", |cx| {
        panel.read_with(cx, |panel, _| {
            panel
                .shelves()
                .first()
                .is_some_and(|shelf| shelf.name == "renamed")
        })
    });

    click(cx, window, "shelf-row-0");
    let handle = panel.read_with(cx, |panel, _| panel.focus());
    focus(cx, window, handle);
    click(cx, window, "shelf-delete");
    wait_for(cx, "the delete question", |cx| {
        has_selector(cx, window, "prompt")
    });
    press(cx, window, "enter");
    wait_for(cx, "the deleted shelf", |cx| {
        panel.read_with(cx, |panel, _| panel.shelves().is_empty())
    });
}

#[ui_kit::gpui::test]
fn change_lists_move_set_active_rename_delete_and_group(cx: &mut TestAppContext) {
    let (root, _bare, _git) = repository_with_origin("change-lists");
    commit_file(&root, "second.txt", "one\n", "second");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    fs::write(root.join("main.txt"), "ALPHA\nbeta\n").unwrap();
    fs::write(root.join("second.txt"), "ONE\n").unwrap();
    state.update(cx, |state, cx| state.refresh(cx));
    show_commit(cx, window);
    let panel = commit_panel(cx);
    wait_for(cx, "the changed files", |cx| {
        panel.read_with(cx, |panel, _| panel.active_changelist() == "Changes")
            && has_selector(cx, window, "commit-row-1")
    });

    // A new list becomes active.
    click(cx, window, "changelist-active");
    click_menu(cx, window, "New Changelist...");
    wait_for(cx, "the name question", |cx| {
        has_selector(cx, window, "prompt")
    });
    type_text(cx, window, "Feature");
    press(cx, window, "enter");
    wait_for(cx, "the new active list", |cx| {
        panel.read_with(cx, |panel, _| panel.active_changelist() == "Feature")
    });
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.changelist_names().to_vec()),
        ["Changes", "Feature"]
    );

    // Move the file under the cursor to the other list by Alt+Shift+M.
    click(cx, window, "commit-row-0");
    let handle = panel.read_with(cx, |panel, _| panel.focus());
    focus(cx, window, handle);
    press(cx, window, "alt-shift-m");
    click_menu(cx, window, "Changes");

    // Ctrl+Space makes Changes active again.
    focus_panel(cx, window, &panel);
    press(cx, window, "ctrl-space");
    click_menu(cx, window, "Changes");
    wait_for(cx, "Changes as the active list", |cx| {
        panel.read_with(cx, |panel, _| panel.active_changelist() == "Changes")
    });

    // F2 renames it and Delete takes it away.
    focus_panel(cx, window, &panel);
    press(cx, window, "f2");
    wait_for(cx, "the rename question", |cx| {
        has_selector(cx, window, "prompt")
    });
    press(cx, window, "ctrl-a");
    type_text(cx, window, "Topic");
    press(cx, window, "enter");
    wait_for(cx, "the renamed list", |cx| {
        panel.read_with(cx, |panel, _| panel.active_changelist() == "Topic")
    });
    focus_panel(cx, window, &panel);
    click(cx, window, "changelist-active");
    click_menu(cx, window, "Delete");
    wait_for(cx, "the deleted list", |cx| {
        panel.read_with(cx, |panel, _| {
            panel.active_changelist() == "Feature" && panel.changelist_names() == ["Feature"]
        })
    });

    // Ctrl+Alt+P and Ctrl+Alt+M choose the grouping, and it is kept.
    focus_panel(cx, window, &panel);
    press(cx, window, "ctrl-alt-m");
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.grouping()),
        crate::changelists::Grouping::Module
    );
    press(cx, window, "ctrl-alt-p");
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.grouping()),
        crate::changelists::Grouping::Directory
    );
}

#[ui_kit::gpui::test]
fn commit_takes_the_files_of_the_active_list(cx: &mut TestAppContext) {
    let (root, _bare, _git) = repository_with_origin("commit-active-list");
    commit_file(&root, "second.txt", "one\n", "second");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    fs::write(root.join("main.txt"), "ALPHA\nbeta\n").unwrap();
    fs::write(root.join("second.txt"), "ONE\n").unwrap();
    state.update(cx, |state, cx| state.refresh(cx));
    show_commit(cx, window);
    let panel = commit_panel(cx);
    wait_for(cx, "the changed files", |cx| {
        has_selector(cx, window, "commit-row-1")
    });

    // Feature takes one file, the other one goes to Changes.
    click(cx, window, "changelist-active");
    click_menu(cx, window, "New Changelist...");
    type_text(cx, window, "Feature");
    press(cx, window, "enter");
    wait_for(cx, "the new list", |cx| {
        panel.read_with(cx, |panel, _| panel.active_changelist() == "Feature")
    });
    click(cx, window, "commit-row-0");
    let handle = panel.read_with(cx, |panel, _| panel.focus());
    focus(cx, window, handle);
    press(cx, window, "alt-shift-m");
    click_menu(cx, window, "Changes");

    focus_panel(cx, window, &panel);
    type_text(cx, window, "only-the-active-list");
    press(cx, window, "ctrl-enter");
    wait_for(cx, "the commit of the active list", |cx| {
        state.read_with(cx, |state, _| state.busy().is_none())
            && state.read_with(cx, |state, _| state.status().files.len()) == 1
    });
    let repo = state.read_with(cx, |state, _| state.repo()).unwrap();
    let log = repo.log(1, 0).unwrap();
    assert_eq!(log[0].subject, "only-the-active-list");
    let files = repo.commit_files(&log[0].id).unwrap();
    assert_eq!(files.len(), 1, "{files:?}");
    let remaining = state.read_with(cx, |state, _| state.status().files[0].path.clone());
    assert_ne!(files[0], remaining, "the other file stays uncommitted");
}

#[ui_kit::gpui::test]
fn commit_options_offer_sign_off_and_the_author(cx: &mut TestAppContext) {
    let (root, _bare, _git) = repository_with_origin("commit-options");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    show_commit(cx, window);
    let panel = commit_panel(cx);
    let handle = panel.read_with(cx, |panel, _| panel.focus());
    focus(cx, window, handle);

    press(cx, window, "ctrl-o");
    wait_for(cx, "the commit options", |cx| {
        has_selector(cx, window, "menu-0")
    });
    let labels = window
        .read_with(cx, |workspace, _| {
            workspace
                .menu_entries()
                .map(|entries| {
                    entries
                        .iter()
                        .map(|entry| entry.label.to_string())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        })
        .unwrap();
    for wanted in ["Amend", "Sign-off", "Author..."] {
        assert!(
            labels.iter().any(|label| label == wanted),
            "{wanted}: {labels:?}"
        );
    }
    click_menu(cx, window, "Sign-off");
    assert!(panel.read_with(cx, |panel, _| panel.signs_off()));
}

fn log_panel(cx: &TestAppContext) -> gpui::Entity<crate::git_log::GitLogPanel> {
    cx.read(|cx| cx.global::<ActiveGitLog>().0.clone())
}

fn subjects(cx: &TestAppContext) -> Vec<String> {
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
fn the_log_reworks_the_message_and_refuses_a_pushed_commit(cx: &mut TestAppContext) {
    let (root, _bare, _git) = repository_with_origin("log-reword");
    commit_file(&root, "second.txt", "one\n", "second");
    commit_file(&root, "third.txt", "one\n", "third");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    press(cx, window, "alt-9");
    wait_for(cx, "the log", |cx| has_selector(cx, window, "log-commit-0"));
    settle_log(cx, window);

    // F2 on HEAD gives it another message with an amend.
    click(cx, window, "log-commit-0");
    press(cx, window, "f2");
    wait_for(cx, "the reword question", |cx| {
        has_selector(cx, window, "prompt")
    });
    press(cx, window, "ctrl-a");
    type_text(cx, window, "third again");
    press(cx, window, "enter");
    wait_for(cx, "the amended message", |cx| {
        subjects(cx)
            .first()
            .is_some_and(|subject| subject == "third again")
    });

    // An older commit is reworded by a rebase.
    click(cx, window, "log-commit-1");
    press(cx, window, "f2");
    wait_for(cx, "the reword question", |cx| {
        has_selector(cx, window, "prompt")
    });
    press(cx, window, "ctrl-a");
    type_text(cx, window, "second again");
    press(cx, window, "enter");
    wait_for(cx, "the reworded older commit", |cx| {
        subjects(cx)
            .get(1)
            .is_some_and(|subject| subject == "second again")
    });
    let repo = state.read_with(cx, |state, _| state.repo()).unwrap();
    assert_eq!(repo.log(1, 0).unwrap()[0].subject, "third again");

    // The base commit is on the remote already: its message stays.
    let base = repo.log(3, 0).unwrap()[2].subject.clone();
    click(cx, window, "log-commit-2");
    press(cx, window, "f2");
    wait_for(cx, "the reword question", |cx| {
        has_selector(cx, window, "prompt")
    });
    press(cx, window, "ctrl-a");
    type_text(cx, window, "should not happen");
    press(cx, window, "enter");
    wait_for(cx, "the refusal", |cx| {
        window
            .read_with(cx, |workspace, _| workspace.last_notification())
            .unwrap()
            .is_some_and(|(_, text)| text.contains("pushed"))
    });
    assert_eq!(repo.log(3, 0).unwrap()[2].subject, base);
}

#[ui_kit::gpui::test]
fn the_log_creates_a_branch_at_a_commit_and_shows_all_affected(cx: &mut TestAppContext) {
    let (root, _bare, _git) = repository_with_origin("log-branch");
    commit_file(&root, "second.txt", "one\n", "second");
    commit_file(&root, "third.txt", "one\n", "third");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    press(cx, window, "alt-9");
    wait_for(cx, "the log", |cx| has_selector(cx, window, "log-commit-0"));
    settle_log(cx, window);
    let repo = state.read_with(cx, |state, _| state.repo()).unwrap();
    let target = repo.log(3, 0).unwrap()[1].id.clone();

    click(cx, window, "log-commit-1");
    press(cx, window, "ctrl-alt-n");
    wait_for(cx, "the branch question", |cx| {
        has_selector(cx, window, "prompt")
    });
    type_text(cx, window, "from-older");
    press(cx, window, "enter");
    wait_for(cx, "the new branch", |cx| {
        let repo = state.read_with(cx, |state, _| state.repo()).unwrap();
        repo.branches()
            .unwrap()
            .into_iter()
            .find(|branch| branch.current)
            .is_some_and(|branch| branch.name == "from-older" && branch.head == target)
    });

    // Alt+Shift+A in the history of a file goes back to the whole commit.
    let readme = root.join("second.txt");
    let log = log_panel(cx);
    log.update(cx, |log, cx| log.show_path(readme, cx));
    wait_for(cx, "the history of the file", |cx| {
        log.read_with(cx, |log, _| log.path().is_some()) && has_selector(cx, window, "log-commit-0")
    });
    click(cx, window, "log-commit-0");
    press(cx, window, "alt-shift-a");
    wait_for(cx, "the whole commit", |cx| {
        log.read_with(cx, |log, _| log.path().is_none())
    });
}
