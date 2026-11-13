//! The tab and the right pane of a diff as two views of one document: a change through
//! either one is the text of both, the carets and the folds stay their own, and the tab
//! opened again under an open diff takes its document with it.

use std::fs;
use std::path::{Path, PathBuf};

use editor_view::{EditorView, LanguageConfig, Languages};
use keymap::editor as editor_keys;
use ui_kit::Focusable;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::Workspace;

use crate::diff_view::DiffView;
use crate::window_tests::{
    buffer_of, click, draw, editor_of, open_path, open_window, press, with_window,
};
use crate::window_tests_git::{git_state, repository_with_origin, wait_for};
use crate::window_tests_lang::edit_in;

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

/// Opens `main.txt`, waits for the repository and shows the diff of the file.
fn open_diff(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    root: &Path,
) -> gpui::Entity<DiffView> {
    let state = git_state(cx);
    wait_for(cx, "the repository", |cx| {
        state.read_with(cx, |state, _| state.repo().is_some())
    });
    let path = root.join("main.txt");
    open_path(cx, window, &path);
    let weak = window.root(cx).unwrap().downgrade();
    let shown = path.clone();
    with_window(cx, window, move |window, cx| {
        DiffView::open(state.clone(), weak, shown, window, cx);
    });
    cx.run_until_parked();
    draw(cx, window);
    diff_view(cx, window)
}

/// Shows the diff of the file without opening its tab, as the window of a commit does.
fn open_diff_without_tab(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    root: &Path,
) -> gpui::Entity<DiffView> {
    let state = git_state(cx);
    wait_for(cx, "the repository", |cx| {
        state.read_with(cx, |state, _| state.repo().is_some())
    });
    let path = root.join("main.txt");
    let weak = window.root(cx).unwrap().downgrade();
    let shown = path.clone();
    with_window(cx, window, move |window, cx| {
        DiffView::open(state.clone(), weak, shown, window, cx);
    });
    cx.run_until_parked();
    draw(cx, window);
    diff_view(cx, window)
}

/// A repository whose working text of `main.txt` differs from the commit.
fn changed_repository(name: &str, base: &str, current: &str) -> PathBuf {
    let (root, _bare, _git) = repository_with_origin(name);
    fs::write(root.join("main.txt"), base).unwrap();
    let git = crate::window_tests_git::git();
    git.run(&root, ["add", "main.txt"]).unwrap();
    git.run(&root, ["commit", "--allow-empty", "-m", "base"])
        .unwrap();
    fs::write(root.join("main.txt"), current).unwrap();
    root
}

fn right_editor(
    cx: &mut TestAppContext,
    view: &gpui::Entity<DiffView>,
) -> gpui::Entity<EditorView> {
    view.read_with(cx, |view, _| view.right_editor())
}

#[ui_kit::gpui::test]
fn an_lsp_edit_of_the_tab_is_in_the_right_pane_at_once(cx: &mut TestAppContext) {
    let root = changed_repository("diff-shared-lsp", "alpha\nbeta\n", "ALPHA\nbeta\n");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let view = open_diff(cx, window, &root);
    wait_for(cx, "the changed line", |cx| {
        view.read_with(cx, |view, _| view.hunk_count()) == 1
    });
    let tab = editor_of(cx, window, &root.join("main.txt"));
    let right = right_editor(cx, &view);
    assert_ne!(right, tab, "the right pane is a view of its own");
    assert_eq!(buffer_of(cx, &right), "ALPHA\nbeta\n");
    right.update(cx, |right, cx| right.restore_caret(0, 5, cx));
    cx.run_until_parked();

    // A `TextDocumentEdit` of a refactoring goes to the buffer of the tab.
    let text = buffer_of(cx, &tab);
    tab.update(cx, |tab, cx| {
        tab.apply_document_edits(vec![edit_in(&text, 0, 5, "MOVEDLINE")], cx)
    });
    cx.run_until_parked();
    draw(cx, window);
    assert_eq!(
        buffer_of(cx, &tab),
        "MOVEDLINE\nbeta\n",
        "the tab took the edit of the server"
    );
    assert_eq!(
        buffer_of(cx, &right),
        "MOVEDLINE\nbeta\n",
        "the right pane shows the edit of the server at once"
    );
    assert_eq!(
        right.read_with(cx, |view, _| view.caret()),
        (0, 9),
        "the caret of the right pane moved with the text"
    );
}

#[ui_kit::gpui::test]
fn an_edit_through_either_view_is_the_text_of_both(cx: &mut TestAppContext) {
    let root = changed_repository("diff-shared-edit", "alpha\nbeta\n", "ALPHA\nbeta\n");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let view = open_diff(cx, window, &root);
    wait_for(cx, "the changed line", |cx| {
        view.read_with(cx, |view, _| view.hunk_count()) == 1
    });
    let tab = editor_of(cx, window, &root.join("main.txt"));
    let right = right_editor(cx, &view);

    right.update(cx, |right, cx| right.replace_text(0..0, "R", cx));
    cx.run_until_parked();
    assert_eq!(
        buffer_of(cx, &tab),
        "RALPHA\nbeta\n",
        "an edit of the right pane is in the tab at once"
    );

    tab.update(cx, |tab, cx| tab.replace_text(0..1, "T", cx));
    cx.run_until_parked();
    draw(cx, window);
    assert_eq!(
        buffer_of(cx, &right),
        "TALPHA\nbeta\n",
        "an edit of the tab is in the right pane at once"
    );
}

#[ui_kit::gpui::test]
fn undo_takes_back_the_edit_of_the_other_view(cx: &mut TestAppContext) {
    let root = changed_repository("diff-shared-undo", "alpha\nbeta\n", "ALPHA\nbeta\n");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let view = open_diff(cx, window, &root);
    wait_for(cx, "the changed line", |cx| {
        view.read_with(cx, |view, _| view.hunk_count()) == 1
    });
    let tab = editor_of(cx, window, &root.join("main.txt"));
    let right = right_editor(cx, &view);

    // An edit in the tab: Ctrl+Z in the right pane takes it back, and the tab follows.
    tab.update(cx, |tab, cx| tab.replace_text(0..0, "T", cx));
    cx.run_until_parked();
    assert_eq!(buffer_of(cx, &right), "TALPHA\nbeta\n");
    right.update(cx, |right, cx| right.run(editor_keys::UNDO, cx));
    cx.run_until_parked();
    draw(cx, window);
    assert_eq!(
        buffer_of(cx, &right),
        "ALPHA\nbeta\n",
        "Ctrl+Z in the right pane takes back the edit of the tab"
    );
    assert_eq!(
        buffer_of(cx, &tab),
        "ALPHA\nbeta\n",
        "the tab follows the undo of the right pane"
    );

    // An edit in the right pane: Ctrl+Z in the tab takes it back.
    right.update(cx, |right, cx| right.replace_text(0..0, "R", cx));
    cx.run_until_parked();
    assert_eq!(buffer_of(cx, &tab), "RALPHA\nbeta\n");
    tab.update(cx, |tab, cx| tab.run(editor_keys::UNDO, cx));
    cx.run_until_parked();
    draw(cx, window);
    assert_eq!(
        buffer_of(cx, &tab),
        "ALPHA\nbeta\n",
        "Ctrl+Z in the tab takes back the edit of the right pane"
    );
    assert_eq!(
        buffer_of(cx, &right),
        "ALPHA\nbeta\n",
        "the right pane follows the undo of the tab"
    );
}

#[ui_kit::gpui::test]
fn the_tab_opened_again_under_the_diff_is_the_same_document(cx: &mut TestAppContext) {
    let root = changed_repository("diff-shared-reopen", "alpha\nbeta\n", "ALPHA\nbeta\n");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let view = open_diff(cx, window, &root);
    wait_for(cx, "the changed line", |cx| {
        view.read_with(cx, |view, _| view.hunk_count()) == 1
    });
    let path = root.join("main.txt");

    window
        .update(cx, |workspace, window, cx| {
            workspace.close_active_tab(window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    let tab = editor_of(cx, window, &path);
    let right = right_editor(cx, &view);
    assert_ne!(right, tab, "the right pane is still a view of its own");
    assert_eq!(
        buffer_of(cx, &right),
        buffer_of(cx, &tab),
        "the tab opened again and the right pane are one text"
    );

    // An edit of the right pane reaches the tab opened again.
    right.update(cx, |right, cx| right.replace_text(0..0, "Z", cx));
    cx.run_until_parked();
    assert_eq!(
        buffer_of(cx, &tab),
        "ZALPHA\nbeta\n",
        "the tab opened again took the edit of the right pane"
    );

    // And an edit of the tab reaches the right pane. A command is an undo step of its
    // own, so the undo below takes back this edit alone.
    tab.update(cx, |tab, cx| {
        tab.apply_edits(vec![text_core::Edit::insert(0, "Y")], cx)
    });
    cx.run_until_parked();
    draw(cx, window);
    assert_eq!(
        buffer_of(cx, &right),
        "YZALPHA\nbeta\n",
        "the right pane took the edit of the tab opened again"
    );

    // The undo history survived the tab as well.
    tab.update(cx, |tab, cx| tab.run(editor_keys::UNDO, cx));
    cx.run_until_parked();
    assert_eq!(
        buffer_of(cx, &right),
        "ZALPHA\nbeta\n",
        "Ctrl+Z in the tab opened again takes back its edit in both views"
    );
}

static FOLD_STYLES: &[editor_view::FoldStyle] = &[editor_view::FoldStyle {
    capture: "fold",
    placeholder: "\u{2026}",
    keep: 0,
    joined: false,
    collapsed_on_open: false,
}];

/// A text file with the region markers the fold actions read.
fn sample_language(path: &Path) -> Option<LanguageConfig> {
    let extension = path.extension()?.to_str()?;
    if !extension.eq_ignore_ascii_case("txt") {
        return None;
    }
    let mut config = dockerfile_syntax::config();
    config.name = "Sample";
    config.folds = "";
    config.fold_styles = FOLD_STYLES;
    config.line_comment = Some("//");
    config.indent_unit = "    ";
    Some(config)
}

#[ui_kit::gpui::test]
fn a_fold_in_one_view_does_not_fold_the_other(cx: &mut TestAppContext) {
    let text = "a0\n//region folded\na2\na3\na4\n//endregion\na6\n";
    let root = changed_repository("diff-shared-fold", text, text);
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    cx.update(|cx| {
        cx.set_global(Languages {
            resolve: sample_language,
            templates: |_| None,
        });
    });
    let view = open_diff(cx, window, &root);
    wait_for(cx, "the diff of the file", |cx| {
        view.read_with(cx, |view, _| view.hunk_count()) == 0
    });
    let tab = editor_of(cx, window, &root.join("main.txt"));
    let right = right_editor(cx, &view);
    right.update(cx, |right, cx| right.restore_caret(1, 0, cx));
    wait_for(cx, "the syntax tree of the right pane", |cx| {
        right.read_with(cx, |view, _| view.has_syntax_tree())
    });
    let focus = right.read_with(cx, |view, cx| view.focus_handle(cx));
    window
        .update(cx, |_, window, cx| window.focus(&focus, cx))
        .unwrap();
    cx.run_until_parked();
    let before_right = right.read_with(cx, |view, _| view.display_rows());
    let before_tab = tab.read_with(cx, |view, _| view.display_rows());

    press(cx, window, "ctrl--");
    cx.run_until_parked();
    draw(cx, window);

    assert!(
        right.read_with(cx, |view, _| view.display_rows()) < before_right,
        "the region folded in the right pane"
    );
    assert_eq!(
        tab.read_with(cx, |view, _| view.display_rows()),
        before_tab,
        "the fold of the right pane did not fold the tab"
    );
}

/// Sets or clears the read-only attribute of a test file on every system.
fn set_read_only(path: &Path, read_only: bool) {
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_readonly(read_only);
    fs::set_permissions(path, permissions).unwrap();
}

#[ui_kit::gpui::test]
fn a_tab_opened_under_a_diff_without_one_brings_its_document(cx: &mut TestAppContext) {
    let root = changed_repository("diff-shared-late-tab", "alpha\nbeta\n", "ALPHA\nbeta\n");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let view = open_diff_without_tab(cx, window, &root);
    wait_for(cx, "the changed line", |cx| {
        view.read_with(cx, |view, _| view.hunk_count()) == 1
    });
    let path = root.join("main.txt");

    // F4 on the diff file opens the tab, which becomes the document of the pane.
    open_path(cx, window, &path);
    let tab = editor_of(cx, window, &path);
    let right = right_editor(cx, &view);
    assert_ne!(right, tab, "the right pane stays a view of its own");
    assert!(
        right.read_with(cx, |right, cx| right.shares_document(tab.read(cx))),
        "the pane is a view of the document of the tab opened later"
    );

    // An edit of the tab is the text of the pane, and the diff was read again.
    tab.update(cx, |tab, cx| tab.replace_text(0..5, "X", cx));
    cx.run_until_parked();
    draw(cx, window);
    assert_eq!(buffer_of(cx, &right), "X\nbeta\n");
    wait_for(cx, "the diff of the edited tab", |cx| {
        view.read_with(cx, |view, _| {
            view.current() == "X\nbeta\n" && view.hunk_count() == 1
        })
    });
    draw(cx, window);

    // Revert counts the range in the text of the tab, because that is the document the
    // edit goes to.
    click(cx, window, "diff-revert-0");
    cx.run_until_parked();
    wait_for(cx, "the reverted file", |_| {
        fs::read_to_string(&path).unwrap() == "alpha\nbeta\n"
    });
    assert_eq!(buffer_of(cx, &tab), "alpha\nbeta\n");
    assert_eq!(buffer_of(cx, &right), "alpha\nbeta\n");
}

#[ui_kit::gpui::test]
fn the_edits_of_the_pane_while_the_tab_is_closed_survive_its_reopening(cx: &mut TestAppContext) {
    let root = changed_repository("diff-shared-closed-edits", "alpha\nbeta\n", "ALPHA\nbeta\n");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let view = open_diff(cx, window, &root);
    wait_for(cx, "the changed line", |cx| {
        view.read_with(cx, |view, _| view.hunk_count()) == 1
    });
    let path = root.join("main.txt");
    window
        .update(cx, |workspace, window, cx| {
            workspace.close_active_tab(window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
    let right = right_editor(cx, &view);

    // No tab is open: the pane keeps the working text and takes the edit.
    right.update(cx, |right, cx| right.replace_text(0..0, "Q", cx));
    cx.run_until_parked();
    assert_eq!(buffer_of(cx, &right), "QALPHA\nbeta\n");

    // The tab opened for the file takes the edits of the pane with it.
    open_path(cx, window, &path);
    let tab = editor_of(cx, window, &path);
    let right = right_editor(cx, &view);
    assert_eq!(
        buffer_of(cx, &tab),
        "QALPHA\nbeta\n",
        "the tab opened again kept the edit of the pane"
    );
    assert_eq!(buffer_of(cx, &right), "QALPHA\nbeta\n");
    assert!(
        right.read_with(cx, |right, cx| right.shares_document(tab.read(cx))),
        "the pane and the tab opened again are one document"
    );
}

#[ui_kit::gpui::test]
fn a_read_only_view_follows_a_reload_into_a_shorter_text(cx: &mut TestAppContext) {
    let text = "//region folded\na1\na2\na3\na4\n//endregion\na6\n";
    let current = format!("{text}changed\n");
    let root = changed_repository("diff-shared-read-only", text, &current);
    let path = root.join("main.txt");
    set_read_only(&path, true);
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    cx.update(|cx| {
        cx.set_global(Languages {
            resolve: sample_language,
            templates: |_| None,
        });
    });
    let view = open_diff(cx, window, &root);
    wait_for(cx, "the changed line", |cx| {
        view.read_with(cx, |view, _| view.hunk_count()) == 1
    });
    let tab = editor_of(cx, window, &path);
    let right = right_editor(cx, &view);
    assert!(
        right.read_with(cx, |right, _| right.is_read_only()),
        "the pane is read-only with the file"
    );
    right.update(cx, |right, cx| right.restore_caret(0, 0, cx));
    wait_for(cx, "the syntax tree of the right pane", |cx| {
        right.read_with(cx, |view, _| view.has_syntax_tree())
    });
    right.update(cx, |right, cx| right.run(editor_keys::FOLD, cx));
    cx.run_until_parked();
    draw(cx, window);
    assert!(
        right.read_with(cx, |right, _| right.shown_lines().len()) < 8,
        "the region folded in the read-only pane"
    );

    // The file shrinks on disk and the tab takes the new text: the read-only view of the
    // same document follows it, so its old fold no longer points past the text.
    set_read_only(&path, false);
    fs::write(&path, "a\n").unwrap();
    tab.update(cx, |tab, cx| tab.reload_from_disk(cx));
    cx.run_until_parked();
    draw(cx, window);
    assert_eq!(buffer_of(cx, &tab), "a\n");
    assert_eq!(buffer_of(cx, &right), "a\n");
    assert!(
        right.read_with(cx, |right, _| right.shown_lines().len()) <= 2,
        "the fold of the view went away with the text"
    );
}

#[ui_kit::gpui::test]
fn a_view_that_missed_more_than_the_kept_changes_drops_its_folds(cx: &mut TestAppContext) {
    let mut base = String::from("//region folded\n");
    for line in 1..100 {
        base.push_str(&format!("line {line}\n"));
    }
    base.push_str("//endregion\n");
    let current = base.replace("line 50\n", "changed 50\n");
    let root = changed_repository("diff-shared-many-edits", &base, &current);
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    cx.update(|cx| {
        cx.set_global(Languages {
            resolve: sample_language,
            templates: |_| None,
        });
    });
    let view = open_diff(cx, window, &root);
    wait_for(cx, "the changed line", |cx| {
        view.read_with(cx, |view, _| view.hunk_count()) == 1
    });
    let tab = editor_of(cx, window, &root.join("main.txt"));
    let right = right_editor(cx, &view);
    right.update(cx, |right, cx| right.restore_caret(0, 0, cx));
    wait_for(cx, "the syntax tree of the right pane", |cx| {
        right.read_with(cx, |view, _| view.has_syntax_tree())
    });
    right.update(cx, |right, cx| right.run(editor_keys::FOLD, cx));
    cx.run_until_parked();
    draw(cx, window);
    let folded = right.read_with(cx, |right, _| right.shown_lines().len());
    assert!(folded < 101, "the region folded in the right pane");

    // An answer of the server deletes the first line 70 times as one undo step. The list
    // of changes keeps 64, so the pane cannot replay them and has to drop its view state.
    let mut probe = buffer_of(cx, &tab);
    let mut batches = Vec::new();
    for _ in 0..70 {
        let first = probe.split('\n').next().unwrap().len();
        batches.push(vec![edit_in(&probe, 0, first + 1, "")]);
        probe.replace_range(0..first + 1, "");
    }
    tab.update(cx, |tab, cx| tab.apply_document_batches(batches, cx));
    cx.run_until_parked();
    draw(cx, window);
    assert_eq!(buffer_of(cx, &tab), probe);
    assert_eq!(
        buffer_of(cx, &right),
        probe,
        "the pane has the text of the tab after the changes it missed"
    );
    assert!(
        right.read_with(cx, |right, _| right.shown_lines().len()) <= probe.split('\n').count(),
        "the fold of the view did not outlive the text it covered"
    );
}
