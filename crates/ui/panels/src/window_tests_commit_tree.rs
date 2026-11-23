//! The Commit window as a tree, without a window of the user's own: the changes in two groups
//! with their counts, a chain of folders folded into one row, the icon of each file by its
//! type, the ticks of the groups and the folders with the partial tick, the keys that fold a
//! folder, and the icons above the tree.

use std::fs;
use std::path::{Path, PathBuf};

use ui_kit::gpui;
use ui_kit::gpui::{Entity, TestAppContext, VisualTestContext, WindowHandle};
use ui_kit::metrics::ICON;
use ui_kit::{Theme, icon_file};
use workspace::{COMMIT_TOOL, Workspace};

use crate::commit::CommitPanel;
use crate::git::GitState;
use crate::script::ActiveCommitPanel;
use crate::window_tests::{bounds_of, click, draw, press, scratch};
use crate::window_tests_git::{git, git_state, has_selector, wait_for, wait_for_repository};
use crate::window_tests_git_log_icons::open_window_with_icons;

/// A repository with three committed files, all of them changed after the commit, and a file
/// that Git does not know yet in a folder of its own. The class file has the icon of its type
/// in the test window; the XML file has the icon of its type too.
fn repository(name: &str) -> PathBuf {
    let root = scratch(name);
    let git = git();
    git.run(&root, ["init"]).unwrap();
    git.run(&root, ["symbolic-ref", "HEAD", "refs/heads/main"])
        .unwrap();
    git.run(&root, ["config", "user.email", "window-test@example.com"])
        .unwrap();
    git.run(&root, ["config", "user.name", "Window Test"])
        .unwrap();
    fs::create_dir_all(root.join("src/main/acme/owner")).unwrap();
    fs::write(
        root.join("src/main/acme/owner/Owner.kls"),
        "class Owner {}\n",
    )
    .unwrap();
    fs::write(root.join("src/main/acme/owner/Pet.xml"), "<pet/>\n").unwrap();
    fs::write(root.join("main.txt"), "alpha\n").unwrap();
    git.run(&root, ["add", "-A"]).unwrap();
    git.run(&root, ["commit", "-m", "base"]).unwrap();

    fs::write(
        root.join("src/main/acme/owner/Owner.kls"),
        "class Owner { int a; }\n",
    )
    .unwrap();
    fs::write(
        root.join("src/main/acme/owner/Pet.xml"),
        "<pet id=\"1\"/>\n",
    )
    .unwrap();
    fs::write(root.join("main.txt"), "beta\n").unwrap();
    fs::create_dir_all(root.join("scratch")).unwrap();
    fs::write(root.join("scratch/notes.txt"), "notes\n").unwrap();
    root
}

/// Puts the focus on the Commit window, as Alt+0 and the tool button do.
fn focus_commit(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    let panel = commit_panel(cx);
    let focus = panel.read_with(cx, |panel, _| panel.focus());
    window
        .update(cx, |_, window, cx| window.focus(&focus, cx))
        .unwrap();
    cx.run_until_parked();
}

/// Shows the Commit window with the focus in it and draws it once.
fn show_commit(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(COMMIT_TOOL, window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    focus_commit(cx, window);
    draw(cx, window);
}

pub(crate) fn commit_panel(cx: &TestAppContext) -> Entity<CommitPanel> {
    cx.read(|cx| cx.global::<ActiveCommitPanel>().0.clone())
}

/// The text of each row of the tree, in the order the window draws them.
fn summary(cx: &mut TestAppContext) -> Vec<String> {
    commit_panel(cx).read_with(cx, |panel, cx| panel.tree_summary(cx))
}

/// Whether the window ticks the file at `path`.
fn ticked(cx: &mut TestAppContext, path: &str) -> bool {
    commit_panel(cx).read_with(cx, |panel, _| panel.is_checked(path))
}

/// A selector built at run time. A test lives as long as the process, so a name leaked here
/// costs nothing that matters.
pub(crate) fn selector(name: String) -> &'static str {
    Box::leak(name.into_boxed_str())
}

/// The selector of the row of the changed file whose path ends with `name`.
pub(crate) fn row_of(cx: &TestAppContext, state: &Entity<GitState>, name: &str) -> &'static str {
    let index = state.read_with(cx, |state, _| {
        state
            .status()
            .files
            .iter()
            .position(|file| file.path.ends_with(name))
    });
    selector(format!(
        "commit-row-{}",
        index.unwrap_or_else(|| panic!("no changed file ends with {name}"))
    ))
}

/// The selector of the icon `name` in the theme of the window.
pub(crate) fn icon_selector(set: &str, name: &str, dark: bool) -> &'static str {
    selector(icon_file(set, name, dark, ICON).to_string())
}

/// The icon is drawn inside the row or the button.
fn icon_in_row(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    row: &'static str,
    icon: &'static str,
) {
    let bounds = bounds_of(cx, window, row);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    let drawn = visual
        .debug_bounds(icon)
        .unwrap_or_else(|| panic!("the window draws no {icon}"));
    assert!(
        bounds.contains(&drawn.center()),
        "the icon {icon} is outside {row}: {drawn:?} and {bounds:?}"
    );
}

pub(crate) fn theme_of(cx: &mut TestAppContext) -> (&'static str, bool) {
    cx.read(|cx| {
        let theme = cx.global::<Theme>();
        (theme.icon_set(), theme.dark)
    })
}

/// Opens the window of the repository, waits for its four changed files and shows the Commit
/// window with them.
pub(crate) fn open_commit(
    cx: &mut TestAppContext,
    name: &str,
) -> (PathBuf, WindowHandle<Workspace>, Entity<GitState>) {
    let root = repository(name);
    let window = open_window_with_icons(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    wait_for(cx, "the changed files", |cx| {
        state.read_with(cx, |state, _| state.status().files.len() == 4)
    });
    show_commit(cx, window);
    (root, window, state)
}

#[ui_kit::gpui::test]
fn the_changes_form_a_tree_under_their_groups_with_their_counts(cx: &mut TestAppContext) {
    let (_root, _window, _state) = open_commit(cx, "commit-tree-summary");
    let rows = summary(cx);
    let expected = [
        "Changes 3 files",
        "src/main/acme/owner",
        "Owner.kls",
        "Pet.xml",
        "main.txt",
        "Unversioned Files 1 file",
        "scratch",
        "notes.txt",
    ];
    assert_eq!(
        rows.iter().map(String::as_str).collect::<Vec<_>>(),
        expected,
        "the chain of folders with one folder inside each is one row, and each group has its count"
    );
}

#[ui_kit::gpui::test]
fn a_class_file_has_the_icon_of_its_type_in_the_tree(cx: &mut TestAppContext) {
    let (_root, window, state) = open_commit(cx, "commit-tree-icons");
    let owner = "src/main/acme/owner/Owner.kls";
    let icon = window
        .read_with(cx, |workspace, _| {
            workspace.icons().icon_for(Path::new(owner))
        })
        .unwrap();
    assert_eq!(
        icon, "nodes/class",
        "the class file has the icon the application gives its extension"
    );
    assert_ne!(
        icon, "fileTypes/text",
        "the class file does not get the icon of an unknown file"
    );
    let (set, dark) = theme_of(cx);
    let row = row_of(cx, &state, "Owner.kls");
    icon_in_row(cx, window, row, icon_selector(set, icon, dark));
}

#[ui_kit::gpui::test]
fn a_folder_tick_takes_all_its_files_and_shows_a_partial_tick(cx: &mut TestAppContext) {
    let (_root, window, _state) = open_commit(cx, "commit-tree-folder-tick");
    let owner = "src/main/acme/owner/Owner.kls";
    let pet = "src/main/acme/owner/Pet.xml";
    let tick = "commit-folder-check-changes-src/main/acme/owner";
    let partial = "commit-folder-partial-changes-src/main/acme/owner";
    wait_for(cx, "the tick of the folder", |cx| {
        has_selector(cx, window, tick)
    });

    click(cx, window, tick);
    assert!(
        !ticked(cx, owner) && !ticked(cx, pet),
        "the tick of the folder takes all its files off"
    );
    assert!(
        summary(cx).iter().any(|row| row == "Owner.kls"),
        "the click on the tick leaves the folder open"
    );

    click(cx, window, tick);
    assert!(
        ticked(cx, owner) && ticked(cx, pet),
        "the tick of the folder puts all its files back"
    );

    commit_panel(cx).update(cx, |panel, cx| panel.set_checked(pet, false, cx));
    wait_for(cx, "the partial tick of the folder", |cx| {
        has_selector(cx, window, partial)
    });

    click(cx, window, tick);
    assert!(
        ticked(cx, owner) && ticked(cx, pet),
        "a partial tick of a folder ticks every file"
    );
    wait_for(cx, "the partial tick to go", |cx| {
        !has_selector(cx, window, partial)
    });
}

#[ui_kit::gpui::test]
fn a_group_tick_takes_its_files_and_leaves_the_other_group_as_it_was(cx: &mut TestAppContext) {
    let (_root, window, _state) = open_commit(cx, "commit-tree-group-tick");
    let tick = "commit-group-check-changes";
    wait_for(cx, "the tick of the group", |cx| {
        has_selector(cx, window, tick)
    });

    click(cx, window, tick);
    assert!(
        !ticked(cx, "main.txt")
            && !ticked(cx, "src/main/acme/owner/Owner.kls")
            && !ticked(cx, "src/main/acme/owner/Pet.xml"),
        "the tick of Changes takes its files off"
    );
    assert!(
        !ticked(cx, "scratch/notes.txt"),
        "the tick of Changes leaves Unversioned Files as it was, and an untracked file starts without a tick"
    );

    click(cx, window, tick);
    assert!(
        ticked(cx, "main.txt") && ticked(cx, "src/main/acme/owner/Owner.kls"),
        "the tick of Changes puts its files back"
    );
}

#[ui_kit::gpui::test]
fn left_and_right_fold_a_folder_and_collapse_all_survives_a_refresh(cx: &mut TestAppContext) {
    let (root, window, state) = open_commit(cx, "commit-tree-keys");
    let owner_row = row_of(cx, &state, "Owner.kls");

    // The click closes the folder it selects; Right opens it and Left closes it again.
    click(cx, window, "commit-folder-changes-src/main/acme/owner");
    wait_for(cx, "the folder closed by its row", |cx| {
        !has_selector(cx, window, owner_row)
    });
    press(cx, window, "right");
    wait_for(cx, "the folder opened by Right", |cx| {
        has_selector(cx, window, owner_row)
    });
    press(cx, window, "left");
    wait_for(cx, "the folder closed by Left", |cx| {
        !has_selector(cx, window, owner_row)
    });
    press(cx, window, "right");
    wait_for(cx, "the folder opened again", |cx| {
        has_selector(cx, window, owner_row)
    });

    // Collapse All closes every heading and folder, and a new file keeps them closed.
    click(cx, window, "commit-tree-collapse");
    wait_for(cx, "every row folded", |cx| {
        !has_selector(cx, window, owner_row)
    });
    fs::write(root.join("scratch/extra.txt"), "extra\n").unwrap();
    state.update(cx, |state, cx| state.refresh(cx));
    wait_for(cx, "the new file in the status", |cx| {
        state.read_with(cx, |state, _| state.status().files.len() == 5)
    });
    draw(cx, window);
    assert_eq!(
        summary(cx),
        vec!["Changes 3 files", "Unversioned Files 2 files"],
        "the headings stay folded after the refresh and show their new counts"
    );

    click(cx, window, "commit-tree-expand");
    wait_for(cx, "every row opened", |cx| {
        has_selector(cx, window, owner_row)
    });
    assert!(
        summary(cx).iter().any(|row| row == "extra.txt"),
        "Expand All opens the folder with the new file"
    );
}

#[ui_kit::gpui::test]
fn the_tree_toolbar_shows_an_icon_inside_each_button(cx: &mut TestAppContext) {
    let (_root, window, _state) = open_commit(cx, "commit-tree-toolbar");
    for button in [
        "commit-tree-refresh",
        "commit-tree-rollback",
        "commit-tree-diff",
        "commit-tree-expand",
        "commit-tree-collapse",
    ] {
        wait_for(cx, button, |cx| has_selector(cx, window, button));
        // The icon has a box of its own: the same icon drawn by another tool window elsewhere
        // on the screen does not count.
        let icon = selector(format!("{button}-icon"));
        let frame = bounds_of(cx, window, button);
        let drawn = bounds_of(cx, window, icon);
        assert!(
            frame.contains(&drawn.center()),
            "the icon of {button} is outside the button: {drawn:?} and {frame:?}"
        );
    }
    let refresh = bounds_of(cx, window, "commit-tree-refresh");
    let first_row = bounds_of(cx, window, "commit-row-0");
    assert!(
        refresh.top() < first_row.top(),
        "the toolbar sits above the tree: {refresh:?} and {first_row:?}"
    );
}
