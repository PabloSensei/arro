//! The diff window as IDEA draws it, without a window: the toolbar, the set of files,
//! Jump to Source, the whitespace and highlighting choices of the gear, the unchanged
//! gaps, the lock of a read-only side and the editor tab.

use std::fs;
use std::path::{Path, PathBuf};

use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use workspace::{DiffHighlighting, DiffWhitespace, Settings, Workspace};

use crate::diff_view::DiffView;
use crate::window_tests::{
    bounds_of, click, draw, editor_of, open_path, open_window, press, with_window,
};
use crate::window_tests_git::{git, git_state, repository_with_origin, wait_for};

fn diff_view(cx: &mut TestAppContext, window: WindowHandle<Workspace>) -> gpui::Entity<DiffView> {
    window
        .read_with(cx, |workspace, _| {
            workspace
                .overlay_view()
                .and_then(|view| view.downcast::<DiffView>().ok())
        })
        .unwrap()
        .expect("the diff popup is open")
}

fn has(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) -> bool {
    VisualTestContext::from_window(window.into(), cx)
        .debug_bounds(selector)
        .is_some()
}

/// Clicks an element whose name is made at run time.
fn click_selector(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &str) {
    draw(cx, window);
    let selector: &'static str = Box::leak(selector.to_string().into_boxed_str());
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    let bounds = visual
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("no element {selector} on screen"));
    visual.simulate_click(bounds.center(), ui_kit::Modifiers::default());
    cx.run_until_parked();
}

/// Commits `text` to `path` in the scratch repository.
fn commit_file(root: &Path, path: &str, text: &str, message: &str) {
    fs::write(root.join(path), text).unwrap();
    let git = git();
    git.run(root, ["add", path]).unwrap();
    git.run(root, ["commit", "-m", message]).unwrap();
}

/// Opens the diff of the file set `files` at `index`, as Local Changes does.
fn open_diff(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    files: Vec<PathBuf>,
    index: usize,
) -> gpui::Entity<DiffView> {
    let state = git_state(cx);
    wait_for(cx, "the repository", |cx| {
        state.read_with(cx, |state, _| state.repo().is_some())
    });
    let weak = window.root(cx).unwrap().downgrade();
    with_window(cx, window, move |window, cx| {
        DiffView::open_list(state.clone(), weak, files, index, window, cx);
    });
    cx.run_until_parked();
    draw(cx, window);
    diff_view(cx, window)
}

fn dismiss(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    window
        .update(cx, |workspace, window, cx| workspace.dismiss(window, cx))
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
}

#[ui_kit::gpui::test]
fn the_toolbar_has_every_button_and_walks_the_differences_and_the_files(cx: &mut TestAppContext) {
    let (root, _bare, _git) = repository_with_origin("diff-idea-toolbar");
    fs::write(root.join("main.txt"), "ALPHA\nbeta\nGAMMA\n").unwrap();
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let files = vec![root.join("main.txt")];
    open_path(cx, window, &root.join("main.txt"));
    let view = open_diff(cx, window, files, 0);
    wait_for(cx, "the two differences", |cx| {
        view.read_with(cx, |view, _| view.hunk_count()) == 2
    });

    for selector in [
        "diff-toolbar",
        "diff-prev",
        "diff-next",
        "diff-jump",
        "diff-prev-file",
        "diff-next-file",
        "diff-files",
        "diff-collapse",
        "diff-count",
        "diff-layout",
        "diff-layout-unified",
        "diff-settings",
        "diff-title",
    ] {
        assert!(
            bounds_of(cx, window, selector).size.width > gpui::px(0.),
            "{selector} is on screen"
        );
    }
    assert_eq!(
        view.read_with(cx, |view, cx| view.title_text(cx)),
        "Repository Diff: main.txt"
    );
    assert_eq!(
        view.read_with(cx, |view, _| view.counter_text()),
        "2 differences"
    );
    assert_eq!(
        view.read_with(cx, |view, _| view.files_label()),
        "1/1 files"
    );
    assert!(
        view.read_with(cx, |view, _| view.attached()),
        "the right pane is a view of the open tab"
    );

    click(cx, window, "diff-next");
    assert_eq!(view.read_with(cx, |view, _| view.hunk_index()), 1);
    click(cx, window, "diff-prev");
    assert_eq!(view.read_with(cx, |view, _| view.hunk_index()), 0);

    // The gear opens the menu of the settings of the diff.
    click(cx, window, "diff-settings");
    draw(cx, window);
    let labels = view.read_with(cx, |view, _| view.menu_labels());
    for wanted in [
        "Ignore whitespaces",
        "Do not ignore",
        "Highlighting",
        "Synchronize scrolling",
        "Show line numbers",
        "Use soft wraps",
        "Context lines",
        "Open in Editor Tab",
    ] {
        assert!(
            labels.iter().any(|label| label == wanted),
            "{wanted}: {labels:?}"
        );
    }
    click(cx, window, "diff-settings");
    draw(cx, window);
    assert!(!view.read_with(cx, |view, _| view.menu_labels().is_empty()));
}

#[ui_kit::gpui::test]
fn alt_right_walks_the_file_set_and_the_counter_says_where_it_is(cx: &mut TestAppContext) {
    let (root, _bare, _git) = repository_with_origin("diff-idea-files");
    for (name, base) in [
        ("one.txt", "alpha\n"),
        ("two.txt", "bravo\n"),
        ("three.txt", "charlie\n"),
    ] {
        commit_file(&root, name, base, name);
        fs::write(root.join(name), base.to_uppercase()).unwrap();
    }
    fs::write(root.join("main.txt"), "ALPHA\nbeta\n").unwrap();
    let files: Vec<PathBuf> = ["one.txt", "two.txt", "three.txt", "main.txt"]
        .iter()
        .map(|name| root.join(name))
        .collect();
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    open_path(cx, window, &root.join("one.txt"));
    let view = open_diff(cx, window, files, 0);
    wait_for(cx, "the first difference", |cx| {
        view.read_with(cx, |view, _| view.hunk_count()) == 1
    });
    assert_eq!(
        view.read_with(cx, |view, _| view.files_label()),
        "1/4 files"
    );

    press(cx, window, "alt-right");
    wait_for(cx, "the second file", |cx| {
        view.read_with(cx, |view, _| view.files_label()) == "2/4 files"
    });
    press(cx, window, "alt-left");
    wait_for(cx, "the first file again", |cx| {
        view.read_with(cx, |view, _| view.files_label()) == "1/4 files"
    });

    // The counter lists every file of the set, and a row shows its diff.
    click(cx, window, "diff-files");
    draw(cx, window);
    let labels = view.read_with(cx, |view, _| view.menu_labels());
    assert_eq!(labels.len(), 4, "{labels:?}");
    click_selector(cx, window, "diff-files-row-3");
    wait_for(cx, "the fourth file", |cx| {
        view.read_with(cx, |view, _| view.path().to_path_buf()) == root.join("main.txt")
    });
    assert_eq!(
        view.read_with(cx, |view, _| view.files_label()),
        "4/4 files"
    );
}

#[ui_kit::gpui::test]
fn f7_on_the_last_difference_goes_to_the_next_file(cx: &mut TestAppContext) {
    let (root, _bare, _git) = repository_with_origin("diff-idea-f7");
    commit_file(&root, "second.txt", "one\ntwo\n", "second");
    fs::write(root.join("main.txt"), "ALPHA\nbeta\n").unwrap();
    fs::write(root.join("second.txt"), "ONE\ntwo\n").unwrap();
    let files = vec![root.join("main.txt"), root.join("second.txt")];
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    open_path(cx, window, &root.join("main.txt"));
    let view = open_diff(cx, window, files, 0);
    wait_for(cx, "the difference", |cx| {
        view.read_with(cx, |view, _| view.hunk_count()) == 1
    });

    press(cx, window, "f7");
    wait_for(cx, "the next file", |cx| {
        view.read_with(cx, |view, _| view.path().to_path_buf()) == root.join("second.txt")
    });
    assert_eq!(view.read_with(cx, |view, _| view.hunk_index()), 0);
}

#[ui_kit::gpui::test]
fn jump_to_source_opens_the_file_at_the_line_of_the_caret(cx: &mut TestAppContext) {
    let (root, _bare, _git) = repository_with_origin("diff-idea-jump");
    let path = root.join("main.txt");
    fs::write(&path, "ALPHA\nbeta\n").unwrap();
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    open_path(cx, window, &path);
    let view = open_diff(cx, window, vec![path.clone()], 0);
    wait_for(cx, "the difference", |cx| {
        view.read_with(cx, |view, _| view.hunk_count()) == 1
    });
    let right = view.read_with(cx, |view, _| view.right_editor());
    right.update(cx, |right, cx| right.restore_caret(1, 1, cx));
    cx.run_until_parked();

    press(cx, window, "f4");
    wait_for(cx, "the popup to close", |cx| {
        !window
            .read_with(cx, |workspace, _| workspace.has_overlay())
            .unwrap()
    });
    assert_eq!(
        window
            .read_with(cx, |workspace, _| workspace.active_path())
            .unwrap(),
        Some(path.clone())
    );
    let editor = editor_of(cx, window, &path);
    assert_eq!(
        editor.read_with(cx, |editor, _| editor.caret()),
        (1, 1),
        "the caret of the pane is the caret of the opened file"
    );
}

#[ui_kit::gpui::test]
fn ignore_whitespaces_hides_a_whitespace_hunk_and_changes_the_counter(cx: &mut TestAppContext) {
    let (root, _bare, _git) = repository_with_origin("diff-idea-whitespace");
    let path = root.join("main.txt");
    fs::write(&path, "alpha\n   beta\n").unwrap();
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    open_path(cx, window, &path);
    let view = open_diff(cx, window, vec![path.clone()], 0);
    wait_for(cx, "the whitespace difference", |cx| {
        view.read_with(cx, |view, _| view.hunk_count()) == 1
    });
    assert_eq!(
        view.read_with(cx, |view, _| view.counter_text()),
        "1 difference"
    );

    click(cx, window, "diff-settings");
    draw(cx, window);
    click(cx, window, "diff-settings-whitespace-ignore");
    wait_for(cx, "the hunk to go away", |cx| {
        view.read_with(cx, |view, _| view.hunk_count()) == 0
    });
    assert_eq!(
        view.read_with(cx, |view, _| view.whitespace_mode()),
        DiffWhitespace::Ignore
    );
    assert_eq!(
        view.read_with(cx, |view, _| view.counter_text()),
        "Only whitespace differences"
    );
}

#[ui_kit::gpui::test]
fn collapse_unchanged_shows_the_count_and_a_click_opens_the_lines(cx: &mut TestAppContext) {
    let (root, _bare, _git) = repository_with_origin("diff-idea-collapse");
    let mut base = String::new();
    for line in 0..30 {
        base.push_str(&format!("line {line}\n"));
    }
    let path = root.join("main.txt");
    fs::write(&path, &base).unwrap();
    let git = git();
    git.run(&root, ["add", "main.txt"]).unwrap();
    git.run(&root, ["commit", "-m", "long"]).unwrap();
    let current = base
        .replace("line 2\n", "changed 2\n")
        .replace("line 27\n", "changed 27\n");
    fs::write(&path, current).unwrap();
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    open_path(cx, window, &path);
    let view = open_diff(cx, window, vec![path.clone()], 0);
    wait_for(cx, "the two differences", |cx| {
        view.read_with(cx, |view, _| view.hunk_count()) == 2
    });
    click(cx, window, "diff-layout");
    draw(cx, window);
    assert!(has(cx, window, "diff-unified"));

    click(cx, window, "diff-collapse");
    draw(cx, window);
    assert!(view.read_with(cx, |view, _| view.collapses()));
    let labels = view.read_with(cx, |view, _| view.unchanged_labels());
    assert!(
        labels
            .iter()
            .all(|label| label.ends_with("unchanged lines")),
        "{labels:?}"
    );
    assert!(!labels.is_empty(), "the unchanged lines fold away");
    let gap = view
        .read_with(cx, |view, _| view.gap_ids())
        .into_iter()
        .next()
        .expect("a gap");
    let selector = format!("diff-unchanged-{gap}");
    let selector: &'static str = Box::leak(selector.into_boxed_str());
    assert!(
        VisualTestContext::from_window(window.into(), cx)
            .debug_bounds(selector)
            .is_some(),
        "the gap shows its count"
    );

    click_selector(cx, window, &format!("diff-unchanged-{gap}"));
    draw(cx, window);
    assert_eq!(
        view.read_with(cx, |view, _| view.gap_count()),
        0,
        "the click opens the only gap and every line is in sight"
    );
}

#[ui_kit::gpui::test]
fn the_settings_of_the_gear_stay_after_the_diff_opens_again(cx: &mut TestAppContext) {
    let (root, _bare, _git) = repository_with_origin("diff-idea-settings");
    let path = root.join("main.txt");
    fs::write(&path, "ALPHA\nbeta\n").unwrap();
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    open_path(cx, window, &path);
    let view = open_diff(cx, window, vec![path.clone()], 0);
    wait_for(cx, "the difference", |cx| {
        view.read_with(cx, |view, _| view.hunk_count()) == 1
    });

    click(cx, window, "diff-settings");
    draw(cx, window);
    click(cx, window, "diff-settings-whitespace-ignore");
    click(cx, window, "diff-settings");
    draw(cx, window);
    click(cx, window, "diff-settings-highlighting-by-line");
    click(cx, window, "diff-settings");
    draw(cx, window);
    click(cx, window, "diff-settings-context-5");
    click(cx, window, "diff-settings");
    draw(cx, window);
    click(cx, window, "diff-settings-sync");
    click(cx, window, "diff-settings");
    draw(cx, window);
    click(cx, window, "diff-settings-line-numbers");
    click(cx, window, "diff-settings");
    draw(cx, window);
    click(cx, window, "diff-settings-soft-wrap");
    assert_eq!(
        view.read_with(cx, |view, _| view.highlighting_mode()),
        DiffHighlighting::ByLine
    );
    assert_eq!(view.read_with(cx, |view, _| view.context()), 5);
    assert!(!view.read_with(cx, |view, _| view.synchronizes()));
    assert!(!view.read_with(cx, |view, _| view.line_numbers()));
    assert!(view.read_with(cx, |view, _| view.wraps()));

    let file = window
        .read_with(cx, |workspace, _| {
            workspace.settings_dir().join(workspace::SETTINGS_FILE)
        })
        .unwrap();
    wait_for(cx, "the settings file", |_| {
        let settings = Settings::load(&file);
        settings.diff_whitespace == DiffWhitespace::Ignore
            && settings.diff_highlighting == DiffHighlighting::ByLine
            && settings.diff_context_lines == 5
            && !settings.diff_synchronize_scrolling
            && !settings.diff_show_line_numbers
            && settings.diff_soft_wrap
    });

    // A new window reads the file: the diff opens with the same choices.
    let saved = Settings::load(&file);
    dismiss(cx, window);
    cx.update(|cx| cx.set_global(saved));
    let view = open_diff(cx, window, vec![path.clone()], 0);
    wait_for(cx, "the difference", |cx| {
        view.read_with(cx, |view, _| view.hunk_count()) == 1
    });
    assert_eq!(
        view.read_with(cx, |view, _| view.whitespace_mode()),
        DiffWhitespace::Ignore
    );
    assert_eq!(
        view.read_with(cx, |view, _| view.highlighting_mode()),
        DiffHighlighting::ByLine
    );
    assert_eq!(view.read_with(cx, |view, _| view.context()), 5);
    assert!(!view.read_with(cx, |view, _| view.synchronizes()));
    assert!(!view.read_with(cx, |view, _| view.line_numbers()));
    assert!(view.read_with(cx, |view, _| view.wraps()));
}

#[ui_kit::gpui::test]
fn the_lock_marks_only_the_read_only_side(cx: &mut TestAppContext) {
    let (root, _bare, _git) = repository_with_origin("diff-idea-lock");
    let path = root.join("main.txt");
    fs::write(&path, "ALPHA\nbeta\n").unwrap();
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    open_path(cx, window, &path);
    let view = open_diff(cx, window, vec![path.clone()], 0);
    wait_for(cx, "the difference", |cx| {
        view.read_with(cx, |view, _| view.hunk_count()) == 1
    });
    assert!(view.read_with(cx, |view, _| view.locked(true)));
    assert!(
        !view.read_with(cx, |view, _| view.locked(false)),
        "the working copy of the right pane is written"
    );
    assert!(has(cx, window, "diff-lock-left"));
    assert!(!has(cx, window, "diff-lock-right"));
    let (left, right) = view.read_with(cx, |view, _| view.revisions());
    assert_eq!(left, "HEAD");
    assert_eq!(right, "Working copy");

    // Both sides of a commit are read only, and the right one names its short hash.
    let commit = {
        let state = git_state(cx);
        state
            .read_with(cx, |state, _| state.repo())
            .unwrap()
            .log(1, 0)
            .unwrap()
            .remove(0)
            .id
    };
    dismiss(cx, window);
    let state = git_state(cx);
    let weak = window.root(cx).unwrap().downgrade();
    let shown = path.clone();
    let id = commit.clone();
    with_window(cx, window, move |window, cx| {
        DiffView::open_commit(state.clone(), weak, vec![shown], 0, id, window, cx);
    });
    cx.run_until_parked();
    draw(cx, window);
    let view = diff_view(cx, window);
    wait_for(cx, "the commit difference", |cx| {
        view.read_with(cx, |view, _| view.hunk_count()) == 1
    });
    assert!(view.read_with(cx, |view, _| view.locked(true)));
    assert!(view.read_with(cx, |view, _| view.locked(false)));
    assert!(has(cx, window, "diff-lock-left"));
    assert!(has(cx, window, "diff-lock-right"));
    let (_, right) = view.read_with(cx, |view, _| view.revisions());
    assert_eq!(right, &commit[..7.min(commit.len())]);
}

#[ui_kit::gpui::test]
fn open_in_editor_tab_shows_the_same_diff_in_a_tab(cx: &mut TestAppContext) {
    let (root, _bare, _git) = repository_with_origin("diff-idea-tab");
    let path = root.join("main.txt");
    fs::write(&path, "ALPHA\nbeta\n").unwrap();
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    open_path(cx, window, &path);
    let view = open_diff(cx, window, vec![path.clone()], 0);
    wait_for(cx, "the difference", |cx| {
        view.read_with(cx, |view, _| view.hunk_count()) == 1
    });

    click(cx, window, "diff-settings");
    draw(cx, window);
    click(cx, window, "diff-settings-open-in-tab");
    wait_for(cx, "the diff tab", |cx| {
        window
            .read_with(cx, |workspace, _| {
                workspace
                    .active_path()
                    .is_some_and(|path| path.to_string_lossy().contains(".diff"))
            })
            .unwrap()
    });
    assert!(
        !window
            .read_with(cx, |workspace, _| workspace.has_overlay())
            .unwrap(),
        "the popup is gone"
    );
    draw(cx, window);
    let tab = window
        .read_with(cx, |workspace, _| {
            let path = workspace.active_path()?;
            workspace.view_of(&path)
        })
        .unwrap()
        .and_then(|view| view.downcast::<DiffView>().ok())
        .expect("the tab shows the diff");
    assert!(tab.read_with(cx, |tab, _| tab.in_tab()));
    assert_eq!(tab.read_with(cx, |tab, _| tab.files_label()), "1/1 files");
    assert!(bounds_of(cx, window, "diff-toolbar").size.width > gpui::px(0.));
}
