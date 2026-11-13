//! The tab and the right pane of a diff as two views of one document, without a window
//! where that is enough: a view that missed a change catches up before its own command,
//! a tab that closes leaves the pane the text of the file, a tab opened under a pane with
//! edits takes them only over the text they were made on, and a view that missed more
//! changes than the document keeps drops what points into the old text.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use editor_view::{EditorState, EditorView, OutlineItem, OutlineKind};
use keymap::editor as editor_keys;
use lang_api::{DiagnosticEntry, Severity, TextPoint, TextSpan};
use text_core::{Edit, FileFormat};
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::Workspace;

use crate::diff_view::DiffView;
use crate::window_tests::{buffer_of, click, draw, editor_of, open_path, open_window, with_window};
use crate::window_tests_git::{git_state, repository_with_origin, wait_for};
use crate::window_tests_lang::{FakeLanguage, open_window_with};

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

fn right_editor(
    cx: &mut TestAppContext,
    view: &gpui::Entity<DiffView>,
) -> gpui::Entity<EditorView> {
    view.read_with(cx, |view, _| view.right_editor())
}

/// A repository whose working text of `main.txt` differs from the commit.
fn changed_repository(name: &str, base: &str, current: &str) -> PathBuf {
    let (root, _bare, _git) = repository_with_origin(name);
    let git = crate::window_tests_git::git();
    fs::write(root.join("main.txt"), base).unwrap();
    git.run(&root, ["add", "main.txt"]).unwrap();
    git.run(&root, ["commit", "--allow-empty", "-m", "base"])
        .unwrap();
    fs::write(root.join("main.txt"), current).unwrap();
    root
}

fn close_tab(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    window
        .update(cx, |workspace, window, cx| {
            workspace.close_active_tab(window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
}

/// Sets or clears the read-only attribute of a test file on every system.
fn set_read_only(path: &Path, read_only: bool) {
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_readonly(read_only);
    fs::set_permissions(path, permissions).unwrap();
}

fn bookmark_lines(cx: &TestAppContext, path: &Path) -> Vec<u32> {
    cx.update(|cx| {
        editor_view::bookmarks(cx)
            .into_iter()
            .filter(|entry| entry.path == path)
            .map(|entry| entry.line)
            .collect()
    })
}

#[test]
fn a_view_that_did_not_follow_a_change_catches_up_before_its_own_command() {
    let text = "a\n//region\nb\nc\n//endregion\nd\n";
    let mut first = EditorState::new(text, FileFormat::default(), None, false);
    first.set_caret(5, 0);
    first.toggle_bookmark(None);
    assert_eq!(first.bookmarks(), vec![(5, None)]);
    let mut second = first.fork();
    second.set_caret(1, 0);
    second.perform(editor_keys::FOLD, 10);
    let folded = second.map().folds.as_slice()[0].range.clone();
    assert!(!folded.is_empty(), "the region folded in the second view");
    // The caret of the second view rests on `d` before the first view inserts above it.
    second.set_caret(5, 0);

    // The first view inserts two lines. Nothing tells the second view about it.
    first.apply_edits(vec![Edit::insert(0, "x\ny\n")]);

    // A command of the second view counts the text as it is now: the typing lands on
    // `d`, where its caret was, and the fold and the bookmark moved with the lines.
    second.type_text("Z");
    assert_eq!(
        second.text(),
        "x\ny\na\n//region\nb\nc\n//endregion\nZd\n",
        "the typing landed at the caret the change of the first view moved"
    );
    assert_eq!(
        second.bookmarks(),
        vec![(7, None)],
        "the bookmark moved with the text"
    );
    let shifted = &second.map().folds.as_slice()[0].range;
    assert_eq!(
        shifted,
        &(folded.start + 4..folded.end + 4),
        "the fold moved with the text"
    );
}

#[ui_kit::gpui::test]
fn a_tab_closed_under_the_diff_leaves_the_pane_the_text_of_the_file(cx: &mut TestAppContext) {
    let root = changed_repository("diff-views-close", "alpha\nbeta\n", "ALPHA\nbeta\n");
    let path = root.join("main.txt");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let view = open_diff(cx, window, &root);
    wait_for(cx, "the changed line", |cx| {
        view.read_with(cx, |view, _| view.hunk_count()) == 1
    });
    let tab = editor_of(cx, window, &path);

    // A dirty tab is written when it closes: the pane keeps the saved text.
    tab.update(cx, |tab, cx| tab.replace_text(0..5, "SAVED", cx));
    cx.run_until_parked();
    close_tab(cx, window);
    wait_for(cx, "the file of the closed tab", |_| {
        fs::read_to_string(&path).unwrap() == "SAVED\nbeta\n"
    });
    let right = right_editor(cx, &view);
    assert_eq!(
        buffer_of(cx, &right),
        "SAVED\nbeta\n",
        "the pane kept the text the closed tab wrote"
    );
    assert!(
        right.read_with(cx, |right, _| !right.is_dirty()),
        "the pane is a clean document of its own"
    );

    // A tab dropped without a save leaves the text of the disk: the pane shows that, and
    // the tab opened again is clean with the same text.
    open_path(cx, window, &path);
    cx.run_until_parked();
    let tab = editor_of(cx, window, &path);
    tab.update(cx, |tab, cx| tab.replace_text(0..5, "LOST", cx));
    cx.run_until_parked();
    window
        .update(cx, |workspace, window, cx| {
            workspace.discard_tabs_under(&path, window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
    let right = right_editor(cx, &view);
    assert_eq!(
        buffer_of(cx, &right),
        "SAVED\nbeta\n",
        "the pane dropped the edits of the tab closed without a save"
    );
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "SAVED\nbeta\n",
        "the discarded tab wrote nothing"
    );

    open_path(cx, window, &path);
    cx.run_until_parked();
    let tab = editor_of(cx, window, &path);
    assert_eq!(buffer_of(cx, &tab), "SAVED\nbeta\n");
    assert!(
        !tab.read_with(cx, |tab, _| tab.is_dirty()),
        "the tab opened again is clean"
    );
}

#[ui_kit::gpui::test]
fn a_pane_with_edits_keeps_them_when_the_file_changed_on_disk(cx: &mut TestAppContext) {
    let root = changed_repository("diff-views-diverged", "alpha\nbeta\n", "ALPHA\nbeta\n");
    let path = root.join("main.txt");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let view = open_diff(cx, window, &root);
    wait_for(cx, "the changed line", |cx| {
        view.read_with(cx, |view, _| view.hunk_count()) == 1
    });
    // The tab is clean, so the pane takes the disk text when it goes.
    close_tab(cx, window);
    let right = right_editor(cx, &view);
    assert_eq!(buffer_of(cx, &right), "ALPHA\nbeta\n");
    right.update(cx, |right, cx| right.replace_text(0..5, "PANEL", cx));
    cx.run_until_parked();

    // The file changes on disk while no tab is open.
    fs::write(&path, "DISK\nbeta\n").unwrap();
    open_path(cx, window, &path);
    cx.run_until_parked();
    draw(cx, window);
    let tab = editor_of(cx, window, &path);
    assert_eq!(
        buffer_of(cx, &tab),
        "DISK\nbeta\n",
        "the tab opened again read the file"
    );
    let right = right_editor(cx, &view);
    assert_eq!(
        buffer_of(cx, &right),
        "PANEL\nbeta\n",
        "the edit of the pane is not lost silently"
    );
    assert!(
        !right.read_with(cx, |right, cx| right.shares_document(tab.read(cx))),
        "the pane stays a document of its own while the text differs"
    );
    let (title, text) = window
        .read_with(cx, |workspace, _| workspace.last_notification())
        .unwrap()
        .expect("the window tells why the pane did not take the document of the tab");
    assert_eq!(title, "Diff");
    assert!(
        text.contains("changed on disk"),
        "the notification names the reason: {text}"
    );
    // The hunks describe the text the pane shows, not the text of the tab.
    wait_for(cx, "the diff of the pane", |cx| {
        view.read_with(cx, |view, _| view.current() == "PANEL\nbeta\n")
    });

    // Revert changes the document of the pane and writes it.
    click(cx, window, "diff-revert-0");
    cx.run_until_parked();
    wait_for(cx, "the reverted file", |_| {
        fs::read_to_string(&path).unwrap() == "alpha\nbeta\n"
    });
    assert_eq!(buffer_of(cx, &right), "alpha\nbeta\n");
}

#[ui_kit::gpui::test]
fn a_read_only_tab_leaves_the_pane_its_own_document(cx: &mut TestAppContext) {
    let root = changed_repository("diff-views-read-only", "alpha\nbeta\n", "ALPHA\nbeta\n");
    let path = root.join("main.txt");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let view = open_diff(cx, window, &root);
    wait_for(cx, "the changed line", |cx| {
        view.read_with(cx, |view, _| view.hunk_count()) == 1
    });
    close_tab(cx, window);
    let right = right_editor(cx, &view);
    right.update(cx, |right, cx| right.replace_text(0..5, "PANEL", cx));
    cx.run_until_parked();

    set_read_only(&path, true);
    open_path(cx, window, &path);
    cx.run_until_parked();
    draw(cx, window);
    let tab = editor_of(cx, window, &path);
    assert!(
        tab.read_with(cx, |tab, _| tab.is_read_only()),
        "the tab of a read-only file is read-only"
    );
    let right = right_editor(cx, &view);
    assert_eq!(buffer_of(cx, &right), "PANEL\nbeta\n");
    assert_eq!(buffer_of(cx, &tab), "ALPHA\nbeta\n");
    assert!(
        !right.read_with(cx, |right, cx| right.shares_document(tab.read(cx))),
        "the pane keeps its edits while the tab cannot take them"
    );
    let (title, text) = window
        .read_with(cx, |workspace, _| workspace.last_notification())
        .unwrap()
        .expect("the window tells why the pane did not take the document of the tab");
    assert_eq!(title, "Diff");
    assert!(
        text.contains("read-only"),
        "the notification names the reason: {text}"
    );

    // The hunks and Revert work on the text of the pane, the one it shows.
    wait_for(cx, "the diff of the pane", |cx| {
        view.read_with(cx, |view, _| view.current() == "PANEL\nbeta\n")
    });
    click(cx, window, "diff-revert-0");
    cx.run_until_parked();
    draw(cx, window);
    assert_eq!(
        buffer_of(cx, &right),
        "alpha\nbeta\n",
        "Revert changed the text of the pane"
    );
    assert_eq!(
        buffer_of(cx, &tab),
        "ALPHA\nbeta\n",
        "Revert left the read-only tab alone"
    );
    set_read_only(&path, false);
}

fn error(line: u32, start: u32, end: u32, message: &str) -> DiagnosticEntry {
    DiagnosticEntry {
        span: TextSpan {
            start: TextPoint {
                line,
                character: start,
            },
            end: TextPoint {
                line,
                character: end,
            },
        },
        severity: Severity::Error,
        message: message.to_owned(),
        unnecessary: false,
    }
}

/// A repository with `main.txt` of `lines` lines and a diagnostic on the last one.
fn long_repository(name: &str, lines: usize) -> (PathBuf, Arc<FakeLanguage>) {
    let mut base = String::new();
    for line in 0..lines {
        base.push_str(&format!("line {line}\n"));
    }
    let (root, _bare, _git) = repository_with_origin(name);
    let git = crate::window_tests_git::git();
    fs::write(root.join("main.txt"), &base).unwrap();
    git.run(&root, ["add", "main.txt"]).unwrap();
    git.run(&root, ["commit", "--allow-empty", "-m", "base"])
        .unwrap();
    fs::write(root.join("main.txt"), &base).unwrap();
    let language = FakeLanguage::new();
    language.set_diagnostics(vec![error(
        lines as u32 - 1,
        0,
        4,
        "the last line is wrong",
    )]);
    (root, language)
}

/// Deletes the first `count` lines of the tab in one answer, which lands in fewer than
/// the changes a view keeps.
fn delete_first_lines(cx: &mut TestAppContext, tab: &gpui::Entity<EditorView>, count: usize) {
    let mut probe = buffer_of(cx, tab);
    let mut batches = Vec::new();
    for _ in 0..count {
        let first = probe.split('\n').next().unwrap().len();
        let edit = crate::window_tests_lang::edit_in(&probe, 0, first + 1, "");
        batches.push(vec![edit]);
        probe.replace_range(0..first + 1, "");
    }
    tab.update(cx, |tab, cx| tab.apply_document_batches(batches, cx));
    cx.run_until_parked();
}

#[ui_kit::gpui::test]
fn a_pane_that_missed_changes_drops_the_marks_of_the_old_text(cx: &mut TestAppContext) {
    let (root, language) = long_repository("diff-views-missed-marks", 120);
    let window = open_window_with(cx, root.clone(), language);
    cx.run_until_parked();
    let view = open_diff(cx, window, &root);
    wait_for(cx, "the changed line", |cx| {
        view.read_with(cx, |view, _| view.hunk_count()) == 0
    });
    let tab = editor_of(cx, window, &root.join("main.txt"));
    let right = right_editor(cx, &view);
    wait_for(cx, "the marks of the pane", |cx| {
        !right.read_with(cx, |right, _| right.diagnostic_marks().is_empty())
    });
    draw(cx, window);

    // The answer deletes 80 lines, more than the document keeps for a view that has not
    // followed: the pane drops its ranges and the frame still draws.
    delete_first_lines(cx, &tab, 80);
    draw(cx, window);
    assert!(
        right.read_with(cx, |right, _| right.diagnostic_marks().is_empty()),
        "the marks of the old text are gone from the pane"
    );
    assert_eq!(
        buffer_of(cx, &right),
        buffer_of(cx, &tab),
        "the pane shows the text of the tab"
    );
}

#[ui_kit::gpui::test]
fn a_pane_that_missed_changes_keeps_the_bookmarks_of_the_file(cx: &mut TestAppContext) {
    let (root, language) = long_repository("diff-views-missed-bookmarks", 120);
    let path = root.join("main.txt");
    let window = open_window_with(cx, root.clone(), language);
    cx.run_until_parked();
    let view = open_diff(cx, window, &root);
    wait_for(cx, "the changed line", |cx| {
        view.read_with(cx, |view, _| view.hunk_count()) == 0
    });
    let tab = editor_of(cx, window, &path);
    tab.update(cx, |tab, cx| tab.restore_caret(110, 0, cx));
    tab.update(cx, |tab, cx| tab.run(editor_keys::TOGGLE_BOOKMARK, cx));
    cx.run_until_parked();
    assert!(
        !bookmark_lines(cx, &path).is_empty(),
        "the bookmark of the tab is in the window list"
    );

    let right = right_editor(cx, &view);
    delete_first_lines(cx, &tab, 80);
    draw(cx, window);
    let moved = bookmark_lines(cx, &path);
    assert_eq!(
        moved,
        vec![30],
        "the bookmark of the file moved with the lines the tab deleted"
    );
    // An edit of the pane after the changes it missed neither erases the list of the
    // file nor writes its own empty one.
    right.update(cx, |right, cx| right.replace_text(0..0, "Z", cx));
    cx.run_until_parked();
    draw(cx, window);
    assert_eq!(
        bookmark_lines(cx, &path),
        moved,
        "the bookmark of the file survived the missed changes of the pane"
    );
}

/// A repository whose `main.txt` is raw bytes: the commit and the working file. Line
/// ends stay what the bytes hold, so a test can prove the pane writes the same ones.
fn byte_repository(name: &str, base: &[u8], current: &[u8]) -> PathBuf {
    let (root, _bare, git) = repository_with_origin(name);
    git.run(&root, ["config", "core.autocrlf", "false"])
        .unwrap();
    fs::write(root.join("main.txt"), base).unwrap();
    git.run(&root, ["add", "main.txt"]).unwrap();
    git.run(&root, ["commit", "--allow-empty", "-m", "base"])
        .unwrap();
    fs::write(root.join("main.txt"), current).unwrap();
    root
}

/// Waits for the pane to become a document of its own: the save of the closed tab is
/// gone or its result has landed.
fn wait_detached(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    view: &gpui::Entity<DiffView>,
) {
    let view = view.clone();
    wait_for(cx, "the pane to become its own document", |cx| {
        !view.read_with(cx, |view, _| view.attached())
    });
    draw(cx, window);
}

/// Sends the notification the window sends after a tab opens, closes or saves.
fn notify_workspace(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    window
        .update(cx, |_workspace, _window, cx| cx.notify())
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
}

#[ui_kit::gpui::test]
fn a_detached_pane_keeps_the_crlf_of_the_file(cx: &mut TestAppContext) {
    let root = byte_repository(
        "diff-views-crlf",
        b"one\r\ntwo\r\nthree\r\n",
        b"ONE\r\ntwo\r\nTHREE\r\n",
    );
    let path = root.join("main.txt");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let view = open_diff(cx, window, &root);
    wait_for(cx, "the two hunks", |cx| {
        view.read_with(cx, |view, _| view.hunk_count()) == 2
    });
    close_tab(cx, window);
    wait_detached(cx, window, &view);

    wait_for(cx, "the diff of the pane", |cx| {
        view.read_with(cx, |view, _| view.current() == "ONE\ntwo\nTHREE\n")
    });
    let right = right_editor(cx, &view);
    assert_eq!(
        right.read_with(cx, |right, _| right.line_ending()),
        "CRLF",
        "the pane writes the line ends of the file"
    );
    click(cx, window, "diff-revert-0");
    cx.run_until_parked();
    let expected = b"one\r\ntwo\r\nTHREE\r\n";
    wait_for(cx, "the reverted file", |_| {
        fs::read(&path).unwrap() == expected
    });
    assert_eq!(
        fs::read(&path).unwrap(),
        expected,
        "only the reverted hunk changed; the other one and every line end stayed"
    );
}

#[ui_kit::gpui::test]
fn a_detached_pane_keeps_the_utf8_bom_of_the_file(cx: &mut TestAppContext) {
    let base = b"\xef\xbb\xbfone\r\ntwo\r\nthree\r\n".to_vec();
    let current = b"\xef\xbb\xbfONE\r\ntwo\r\nTHREE\r\n".to_vec();
    let root = byte_repository("diff-views-bom", &base, &current);
    let path = root.join("main.txt");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let view = open_diff(cx, window, &root);
    wait_for(cx, "the two hunks", |cx| {
        view.read_with(cx, |view, _| view.hunk_count()) == 2
    });
    close_tab(cx, window);
    wait_detached(cx, window, &view);
    let right = right_editor(cx, &view);
    assert_eq!(
        right.read_with(cx, |right, _| right.encoding()),
        "UTF-8 BOM",
        "the pane writes the encoding and BOM of the file"
    );
    click(cx, window, "diff-revert-0");
    cx.run_until_parked();
    let expected = b"\xef\xbb\xbfone\r\ntwo\r\nTHREE\r\n";
    wait_for(cx, "the reverted file", |_| {
        fs::read(&path).unwrap() == expected
    });
    assert_eq!(
        fs::read(&path).unwrap(),
        expected,
        "the BOM and the untouched hunk stayed byte for byte"
    );
}

#[ui_kit::gpui::test]
fn a_detached_pane_keeps_the_windows_1251_of_the_file(cx: &mut TestAppContext) {
    // `Привет`, `мир`, an untouched `world` and `ещё`, and the same file with ASCII
    // changes on two lines, all in windows-1251 with CRLF.
    let base = b"\xcf\xf0\xe8\xe2\xe5\xf2\r\n\xec\xe8\xf0\r\nworld\r\n\xe5\xf9\xb8\r\n";
    let current = b"HELLO\r\n\xec\xe8\xf0\r\nMIR\r\n\xe5\xf9\xb8\r\n";
    let root = byte_repository("diff-views-cp1251", base, current);
    let path = root.join("main.txt");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let view = open_diff(cx, window, &root);
    wait_for(cx, "the two hunks", |cx| {
        view.read_with(cx, |view, _| view.hunk_count()) == 2
    });
    let right = right_editor(cx, &view);
    assert_eq!(
        buffer_of(cx, &right),
        "HELLO\nмир\nMIR\nещё\n",
        "the pane of a file that is not UTF-8 is not empty"
    );
    close_tab(cx, window);
    wait_detached(cx, window, &view);
    let right = right_editor(cx, &view);
    assert_eq!(
        right.read_with(cx, |right, _| right.encoding()),
        "windows-1251",
        "the pane writes the encoding of the file"
    );
    click(cx, window, "diff-revert-0");
    cx.run_until_parked();
    let expected = b"\xcf\xf0\xe8\xe2\xe5\xf2\r\n\xec\xe8\xf0\r\nMIR\r\n\xe5\xf9\xb8\r\n";
    wait_for(cx, "the reverted file", |_| {
        fs::read(&path).unwrap() == expected
    });
    assert_eq!(
        fs::read(&path).unwrap(),
        expected,
        "the untouched encoded text and the line ends stayed"
    );
}

#[ui_kit::gpui::test]
fn a_tab_dropped_without_a_save_leaves_the_pane_the_text_of_a_file_that_is_not_utf8(
    cx: &mut TestAppContext,
) {
    let base = b"\xcf\xf0\xe8\xe2\xe5\xf2\r\n\xec\xe8\xf0 \xe8 \xe5\xf9\xb8 \xec\xe8\xf0\r\n";
    let current = b"HELLO\r\n\xec\xe8\xf0 \xe8 \xe5\xf9\xb8 \xec\xe8\xf0\r\n";
    let root = byte_repository("diff-views-cp1251-dropped", base, current);
    let path = root.join("main.txt");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let view = open_diff(cx, window, &root);
    wait_for(cx, "the changed line", |cx| {
        view.read_with(cx, |view, _| view.hunk_count()) == 1
    });
    let tab = editor_of(cx, window, &path);
    tab.update(cx, |tab, cx| tab.replace_text(0..5, "LOST", cx));
    cx.run_until_parked();
    window
        .update(cx, |workspace, window, cx| {
            workspace.discard_tabs_under(&path, window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
    wait_detached(cx, window, &view);
    let right = right_editor(cx, &view);
    assert_eq!(
        buffer_of(cx, &right),
        "HELLO\nмир и ещё мир\n",
        "the pane took the decoded text of the disk, not an empty string"
    );
    // The pane writes the file in the encoding the disk had, so Revert gives the bytes
    // of the commit back.
    click(cx, window, "diff-revert-0");
    cx.run_until_parked();
    wait_for(cx, "the reverted file", |_| {
        fs::read(&path).unwrap() == base.to_vec()
    });
    assert_eq!(
        fs::read(&path).unwrap(),
        base.to_vec(),
        "the reverted file is back in windows-1251 without a rewrite of the other lines"
    );
}

#[ui_kit::gpui::test]
fn a_detached_pane_keeps_the_bookmarks_of_the_file(cx: &mut TestAppContext) {
    let root = changed_repository(
        "diff-views-bookmarks-pane",
        "alpha\nbeta\n",
        "ALPHA\nbeta\n",
    );
    let path = root.join("main.txt");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let view = open_diff(cx, window, &root);
    wait_for(cx, "the changed line", |cx| {
        view.read_with(cx, |view, _| view.hunk_count()) == 1
    });
    let tab = editor_of(cx, window, &path);
    tab.update(cx, |tab, cx| tab.restore_caret(1, 0, cx));
    tab.update(cx, |tab, cx| tab.run(editor_keys::TOGGLE_BOOKMARK, cx));
    cx.run_until_parked();
    assert_eq!(
        bookmark_lines(cx, &path),
        vec![1],
        "the bookmark of the tab is in the window list"
    );

    close_tab(cx, window);
    wait_detached(cx, window, &view);
    click(cx, window, "diff-revert-0");
    cx.run_until_parked();
    assert_eq!(
        bookmark_lines(cx, &path),
        vec![1],
        "the pane of its own neither erased nor moved the bookmark of the file"
    );
}

#[ui_kit::gpui::test]
fn a_failed_save_of_the_closed_tab_leaves_the_pane_the_edits(cx: &mut TestAppContext) {
    let root = changed_repository("diff-views-save-failed", "alpha\nbeta\n", "ALPHA\nbeta\n");
    let path = root.join("main.txt");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let view = open_diff(cx, window, &root);
    wait_for(cx, "the changed line", |cx| {
        view.read_with(cx, |view, _| view.hunk_count()) == 1
    });
    let tab = editor_of(cx, window, &path);
    tab.update(cx, |tab, cx| tab.replace_text(0..5, "KEPT", cx));
    cx.run_until_parked();
    // The attribute changes on the disk; the open view still believes it may write.
    set_read_only(&path, true);
    close_tab(cx, window);
    wait_detached(cx, window, &view);
    let right = right_editor(cx, &view);
    assert!(
        right.read_with(cx, |right, _| right.is_dirty()),
        "the pane keeps the text of the edits the failed save did not write"
    );
    assert_eq!(
        buffer_of(cx, &right),
        "KEPT\nbeta\n",
        "the pane shows the edits of the closed tab"
    );
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "ALPHA\nbeta\n",
        "the file kept its bytes"
    );
    set_read_only(&path, false);
}

#[ui_kit::gpui::test]
fn a_pane_binds_again_after_it_is_saved(cx: &mut TestAppContext) {
    let root = changed_repository("diff-views-bind-saved", "alpha\nbeta\n", "ALPHA\nbeta\n");
    let path = root.join("main.txt");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let view = open_diff(cx, window, &root);
    wait_for(cx, "the changed line", |cx| {
        view.read_with(cx, |view, _| view.hunk_count()) == 1
    });
    close_tab(cx, window);
    wait_detached(cx, window, &view);
    let right = right_editor(cx, &view);
    right.update(cx, |right, cx| right.replace_text(0..5, "PANEL", cx));
    cx.run_until_parked();
    fs::write(&path, "DISK\nbeta\n").unwrap();

    // A tab opened again over another text keeps the pane apart and says why.
    open_path(cx, window, &path);
    cx.run_until_parked();
    draw(cx, window);
    wait_for(cx, "the refusal", |cx| {
        window
            .read_with(cx, |workspace, _| workspace.last_notification())
            .unwrap()
            .is_some_and(|(_, text)| text.contains("changed on disk"))
    });
    assert!(!view.read_with(cx, |view, _| view.attached()));

    // Saving the pane makes its text the text of the file; the tab reads it again and
    // the pane becomes a view of the document of the tab.
    right.update(cx, |right, cx| {
        assert!(right.save_blocking(cx).is_none());
    });
    cx.run_until_parked();
    let tab = editor_of(cx, window, &path);
    tab.update(cx, |tab, cx| tab.reload(cx));
    cx.run_until_parked();
    notify_workspace(cx, window);
    wait_for(cx, "the pane bound again", |cx| {
        view.read_with(cx, |view, _| view.attached())
    });
    let right = right_editor(cx, &view);
    assert!(
        right.read_with(cx, |right, cx| right.shares_document(tab.read(cx))),
        "the pane is again a view of the document of the tab"
    );
    assert_eq!(buffer_of(cx, &right), "PANEL\nbeta\n");
    assert_eq!(buffer_of(cx, &tab), "PANEL\nbeta\n");
}

#[ui_kit::gpui::test]
fn a_pane_binds_again_when_the_file_becomes_writable(cx: &mut TestAppContext) {
    let root = changed_repository("diff-views-bind-writable", "alpha\nbeta\n", "ALPHA\nbeta\n");
    let path = root.join("main.txt");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let view = open_diff(cx, window, &root);
    wait_for(cx, "the changed line", |cx| {
        view.read_with(cx, |view, _| view.hunk_count()) == 1
    });
    close_tab(cx, window);
    wait_detached(cx, window, &view);
    let right = right_editor(cx, &view);
    right.update(cx, |right, cx| right.replace_text(0..5, "PANEL", cx));
    cx.run_until_parked();

    set_read_only(&path, true);
    open_path(cx, window, &path);
    cx.run_until_parked();
    draw(cx, window);
    let tab = editor_of(cx, window, &path);
    wait_for(cx, "the refusal", |cx| {
        window
            .read_with(cx, |workspace, _| workspace.last_notification())
            .unwrap()
            .is_some_and(|(_, text)| text.contains("read-only"))
    });
    assert!(!view.read_with(cx, |view, _| view.attached()));

    // The attribute goes away: the tab can take the edits of the pane now.
    set_read_only(&path, false);
    tab.update(cx, |tab, cx| tab.set_read_only(false, cx));
    cx.run_until_parked();
    notify_workspace(cx, window);
    wait_for(cx, "the pane bound again", |cx| {
        view.read_with(cx, |view, _| view.attached())
    });
    let right = right_editor(cx, &view);
    assert_eq!(buffer_of(cx, &right), "PANEL\nbeta\n");
    assert_eq!(buffer_of(cx, &tab), "PANEL\nbeta\n");
}

#[ui_kit::gpui::test]
fn a_lone_cr_file_does_not_make_the_pane_a_stranger(cx: &mut TestAppContext) {
    let root = byte_repository("diff-views-lone-cr", b"alpha\rbeta\r", b"ALPHA\rbeta\r");
    let path = root.join("main.txt");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let view = open_diff(cx, window, &root);
    wait_for(cx, "the changed line", |cx| {
        view.read_with(cx, |view, _| view.hunk_count()) >= 1
    });
    let tab = editor_of(cx, window, &path);
    assert_eq!(
        buffer_of(cx, &tab),
        "ALPHA\nbeta\n",
        "the tab shows the file with `\\n` line ends"
    );
    tab.update(cx, |tab, cx| tab.replace_text(0..5, "LOST", cx));
    cx.run_until_parked();
    window
        .update(cx, |workspace, window, cx| {
            workspace.discard_tabs_under(&path, window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
    wait_detached(cx, window, &view);
    let right = right_editor(cx, &view);
    assert_eq!(
        buffer_of(cx, &right),
        "ALPHA\nbeta\n",
        "the pane took the disk text of a file with single `\\r` line ends"
    );
    right.update(cx, |right, cx| right.replace_text(0..5, "PANEL", cx));
    cx.run_until_parked();

    // The text the pane started from is the text the tab reads: the vain "changed on
    // disk" of the old comparison is gone and the pane takes the tab over.
    open_path(cx, window, &path);
    cx.run_until_parked();
    draw(cx, window);
    wait_for(cx, "the pane bound again", |cx| {
        view.read_with(cx, |view, _| view.attached())
    });
    let tab = editor_of(cx, window, &path);
    let right = right_editor(cx, &view);
    assert_eq!(buffer_of(cx, &right), "PANEL\nbeta\n");
    assert_eq!(buffer_of(cx, &tab), "PANEL\nbeta\n");
}

#[ui_kit::gpui::test]
fn an_answer_of_the_server_moves_the_ranges_the_view_missed(cx: &mut TestAppContext) {
    let root = changed_repository("diff-views-catch-up", "alpha\nbeta\n", "alpha\nbeta\n");
    let path = root.join("main.txt");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    open_path(cx, window, &path);
    cx.run_until_parked();
    let tab = editor_of(cx, window, &path);

    // A view of the same document that nothing subscribes to: a change of the tab never
    // reaches it.
    let mut created = None;
    with_window(cx, window, |_window, cx| {
        created = EditorView::open_shared(&tab, cx);
    });
    let second = created.expect("the tab has a document to share");
    second.update(cx, |second, cx| {
        second.set_outline(
            vec![OutlineItem {
                name: "alpha".to_owned(),
                signature: None,
                kind: OutlineKind::Function,
                depth: 0,
                range: 0..5,
                name_range: 0..5,
            }],
            cx,
        );
    });

    // The tab inserts four bytes before the declaration; no event tells the second view.
    tab.update(cx, |tab, cx| tab.replace_text(0..0, "////", cx));
    cx.run_until_parked();
    let text = buffer_of(cx, &tab);
    assert_eq!(text, "////alpha\nbeta\n");
    let at = text.len();
    let edit = crate::window_tests_lang::edit_in(&text, at, at, "Z");
    // The answer comes to the view that missed the change: it follows first, so its
    // own ranges move by the four bytes of the tab.
    second.update(cx, |second, cx| second.apply_document_edits(vec![edit], cx));
    cx.run_until_parked();
    assert_eq!(
        second.read_with(cx, |second, _| second.outline()[0].range.clone()),
        4..9,
        "the outline moved with the change the view did not see"
    );
}

/// A clean tab hands the pane the text it has, not the snapshot of its last save: a
/// reload from the disk after that save left newer text, and the old snapshot would
/// write the pulled change away.
#[ui_kit::gpui::test]
fn a_clean_tab_reloaded_after_a_save_leaves_the_pane_the_reloaded_text(cx: &mut TestAppContext) {
    let root = changed_repository("diff-views-reloaded", "alpha\nbeta\n", "ALPHA\nbeta\n");
    let path = root.join("main.txt");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let view = open_diff(cx, window, &root);
    wait_for(cx, "the changed line", |cx| {
        view.read_with(cx, |view, _| view.hunk_count()) == 1
    });
    let tab = editor_of(cx, window, &path);
    tab.update(cx, |tab, cx| {
        tab.replace_text(0..5, "SAVED", cx);
        tab.save_blocking(cx);
    });
    cx.run_until_parked();
    assert_eq!(fs::read_to_string(&path).unwrap(), "SAVED\nbeta\n");

    // Another program changes the file, and the clean tab takes the new text.
    fs::write(&path, "PULLED\nbeta\n").unwrap();
    tab.update(cx, |tab, cx| tab.reload_from_disk(cx));
    wait_for(cx, "the reloaded tab", |cx| {
        tab.read_with(cx, |tab, _| tab.text().as_deref() == Some("PULLED\nbeta\n"))
    });
    assert!(tab.read_with(cx, |tab, _| !tab.is_dirty()));

    close_tab(cx, window);
    wait_detached(cx, window, &view);
    let right = right_editor(cx, &view);
    assert_eq!(
        buffer_of(cx, &right),
        "PULLED\nbeta\n",
        "the pane shows what the file has, not the text of the earlier save"
    );
    assert!(right.read_with(cx, |right, _| !right.is_dirty()));
    assert_eq!(fs::read_to_string(&path).unwrap(), "PULLED\nbeta\n");
}
