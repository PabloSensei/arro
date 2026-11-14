//! Conflicts merged in three panes: Yours, the editable Result and Theirs. These tests
//! describe T-0186 and fail until the Merge button, the window and the three-way merge
//! exist: today a conflicted row takes a whole side and nothing opens from it.

use std::fs;
use std::path::{Path, PathBuf};

use text_core::{Eol, EolStyle};
use ui_kit::Focusable;
use ui_kit::Modifiers;
use ui_kit::gpui;
use ui_kit::gpui::{
    MouseButton, MouseDownEvent, MouseUpEvent, TestAppContext, VisualTestContext, WindowHandle,
};
use workspace::Workspace;

use crate::merge_view::{MergeView, chunk_recomputes, reset_chunk_recomputes};
use crate::window_tests::{bounds_of, click, draw, scratch};
use crate::window_tests_git::{
    conflicting_repository, git, has_selector, start_conflicting_merge, wait_for,
};

fn read_text(path: &Path) -> String {
    fs::read_to_string(path).unwrap().replace("\r\n", "\n")
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

#[ui_kit::gpui::test]
fn the_merge_button_opens_yours_theirs_and_the_result(cx: &mut TestAppContext) {
    let (root, _bare, _git) = conflicting_repository("merge-panes");
    let (window, _state) = start_conflicting_merge(cx, root.clone());

    assert!(
        has_selector(cx, window, "conflict-merge-0"),
        "a conflicted row offers Merge..."
    );
    click(cx, window, "conflict-merge-0");
    draw(cx, window);

    assert!(
        has_selector(cx, window, "merge-yours"),
        "the left pane shows our version"
    );
    assert!(
        has_selector(cx, window, "merge-result"),
        "the middle pane holds the editable result"
    );
    assert!(
        has_selector(cx, window, "merge-theirs"),
        "the right pane shows their version"
    );
    assert!(
        has_selector(cx, window, "merge-accept-yours-0"),
        "the chunk of our side takes our lines"
    );
    assert!(
        has_selector(cx, window, "merge-accept-theirs-0"),
        "the chunk of their side takes their lines"
    );
    assert!(
        has_selector(cx, window, "merge-apply"),
        "Apply writes the result"
    );
    assert!(
        has_selector(cx, window, "merge-cancel"),
        "Cancel closes without a write"
    );
}

#[ui_kit::gpui::test]
fn accepting_yours_and_apply_write_the_file_and_resolve_the_conflict(cx: &mut TestAppContext) {
    let (root, _bare, git) = conflicting_repository("merge-apply");
    let (window, state) = start_conflicting_merge(cx, root.clone());

    click(cx, window, "conflict-merge-0");
    draw(cx, window);
    click(cx, window, "merge-accept-yours-0");
    draw(cx, window);
    click(cx, window, "merge-apply");

    wait_for(cx, "the merged file", |_| {
        read_text(&root.join("main.txt")) == "local main\n"
    });
    wait_for(cx, "the conflict of main.txt to go", |cx| {
        state.read_with(cx, |state, _| {
            !state.conflicts().iter().any(|path| path == "main.txt")
        })
    });
    wait_for(cx, "the Merge window to close", |cx| {
        !has_selector(cx, window, "merge-result")
    });

    let status = String::from_utf8(git.run(&root, ["status", "--porcelain=v2"]).unwrap()).unwrap();
    assert!(
        !status.contains("main.txt"),
        "git leaves no entry for the resolved file: {status}"
    );
}

#[ui_kit::gpui::test]
fn cancel_leaves_the_file_and_the_index_as_they_were(cx: &mut TestAppContext) {
    let (root, _bare, git) = conflicting_repository("merge-cancel");
    let (window, state) = start_conflicting_merge(cx, root.clone());
    let before = fs::read(root.join("main.txt")).unwrap();
    let before_status = git.run(&root, ["status", "--porcelain=v2"]).unwrap();

    click(cx, window, "conflict-merge-0");
    draw(cx, window);
    click(cx, window, "merge-accept-yours-0");
    draw(cx, window);
    click(cx, window, "merge-cancel");
    draw(cx, window);
    if has_selector(cx, window, "merge-discard") {
        click(cx, window, "merge-discard");
        draw(cx, window);
    }

    assert_eq!(
        fs::read(root.join("main.txt")).unwrap(),
        before,
        "Cancel leaves the file with its markers"
    );
    assert_eq!(
        git.run(&root, ["status", "--porcelain=v2"]).unwrap(),
        before_status,
        "Cancel leaves the index as it was"
    );
    assert!(
        state.read_with(cx, |state, _| {
            state.conflicts().iter().any(|path| path == "main.txt")
        }),
        "the file is still a conflict"
    );
    assert!(
        !has_selector(cx, window, "merge-result"),
        "Cancel closes the Merge window"
    );
}

#[ui_kit::gpui::test]
fn a_double_click_on_the_conflict_row_opens_the_merge_window(cx: &mut TestAppContext) {
    let (root, _bare, _git) = conflicting_repository("merge-double-click");
    let (window, _state) = start_conflicting_merge(cx, root.clone());

    let row = bounds_of(cx, window, "conflict-row-0").center();
    click_count(cx, window, row, 2, Modifiers::default());
    draw(cx, window);

    assert!(
        has_selector(cx, window, "merge-result"),
        "a double click opens the Merge window"
    );
}

/// A repository whose one file both sides changed differently, so git leaves it in
/// conflict. `base`, `ours` and `theirs` are the bytes of the three versions.
fn conflict_repository(
    name: &str,
    file: &str,
    base: &[u8],
    ours: &[u8],
    theirs: &[u8],
) -> (PathBuf, PathBuf, vcs::GitRunner) {
    let git = git();
    let root = scratch(name);
    let bare = scratch(&format!("{name}-origin"));
    git.run(&bare, ["init", "--bare"]).unwrap();
    git.run(&root, ["init"]).unwrap();
    git.run(&root, ["config", "user.email", "window-test@example.com"])
        .unwrap();
    git.run(&root, ["config", "user.name", "Window Test"])
        .unwrap();
    fs::write(root.join(file), base).unwrap();
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
    git.run(
        &scratch("merge-clone-cwd"),
        ["clone", bare.to_str().unwrap(), other.to_str().unwrap()],
    )
    .unwrap();
    git.run(&other, ["config", "user.email", "other@example.com"])
        .unwrap();
    git.run(&other, ["config", "user.name", "Other"]).unwrap();
    fs::write(other.join(file), theirs).unwrap();
    git.run(&other, ["add", "."]).unwrap();
    git.run(&other, ["commit", "-m", "remote change"]).unwrap();
    git.run(&other, ["push"]).unwrap();

    fs::write(root.join(file), ours).unwrap();
    git.run(&root, ["add", "."]).unwrap();
    git.run(&root, ["commit", "-m", "local change"]).unwrap();
    (root, bare, git)
}

/// A repository where our side deletes the file and theirs changes it.
fn delete_modify_repository(name: &str) -> (PathBuf, PathBuf, vcs::GitRunner) {
    let git = git();
    let root = scratch(name);
    let bare = scratch(&format!("{name}-origin"));
    git.run(&bare, ["init", "--bare"]).unwrap();
    git.run(&root, ["init"]).unwrap();
    git.run(&root, ["config", "user.email", "window-test@example.com"])
        .unwrap();
    git.run(&root, ["config", "user.name", "Window Test"])
        .unwrap();
    fs::write(root.join("main.txt"), "main\n").unwrap();
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
    git.run(
        &scratch("merge-delete-clone-cwd"),
        ["clone", bare.to_str().unwrap(), other.to_str().unwrap()],
    )
    .unwrap();
    git.run(&other, ["config", "user.email", "other@example.com"])
        .unwrap();
    git.run(&other, ["config", "user.name", "Other"]).unwrap();
    fs::write(other.join("main.txt"), "remote main\n").unwrap();
    git.run(&other, ["add", "."]).unwrap();
    git.run(&other, ["commit", "-m", "remote change"]).unwrap();
    git.run(&other, ["push"]).unwrap();

    fs::remove_file(root.join("main.txt")).unwrap();
    git.run(&root, ["add", "-A"]).unwrap();
    git.run(&root, ["commit", "-m", "local delete"]).unwrap();
    (root, bare, git)
}

/// The Merge window of the test's window.
fn merge_view(cx: &mut TestAppContext, window: WindowHandle<Workspace>) -> gpui::Entity<MergeView> {
    window
        .read_with(cx, |workspace, _| workspace.overlay_view())
        .unwrap()
        .and_then(|view| view.downcast::<MergeView>().ok())
        .expect("the Merge window is open")
}

/// Whether the Merge window is open and no write is on its way.
fn settled(cx: &mut TestAppContext, window: WindowHandle<Workspace>) -> bool {
    window
        .read_with(cx, |workspace, _| workspace.overlay_view())
        .ok()
        .flatten()
        .and_then(|view| view.downcast::<MergeView>().ok())
        .is_some_and(|view| !view.read_with(cx, |view, _| view.busy()))
}

/// The bytes of a text in windows-1251, with CRLF line ends.
fn cp1251_crlf(text: &str) -> Vec<u8> {
    let mut bytes = Vec::new();
    for character in text.chars() {
        match character {
            '\n' => bytes.extend_from_slice(b"\r\n"),
            'А'..='я' => bytes.push(character as u32 as u8 - 0x10 + 0xC0),
            'ё' => bytes.push(0xB8),
            'Ё' => bytes.push(0xA8),
            other => bytes.push(other as u32 as u8),
        }
    }
    bytes
}

#[ui_kit::gpui::test]
fn a_binary_conflict_offers_no_merge_button(cx: &mut TestAppContext) {
    let (root, _bare, _git) = conflict_repository(
        "merge-binary",
        "main.bin",
        b"\x00\x01base\n",
        b"\x00\x02ours\n",
        b"\x00\x03theirs\n",
    );
    let (window, _state) = start_conflicting_merge(cx, root);

    assert!(
        has_selector(cx, window, "conflict-yours-0"),
        "the sides of a binary conflict are still taken whole"
    );
    assert!(
        !has_selector(cx, window, "conflict-merge-0"),
        "a binary file has no three texts to merge"
    );

    let row = bounds_of(cx, window, "conflict-row-0").center();
    click_count(cx, window, row, 2, Modifiers::default());
    draw(cx, window);
    assert!(
        !has_selector(cx, window, "merge-result"),
        "a double click opens nothing for a binary file"
    );
}

#[ui_kit::gpui::test]
fn apply_with_unresolved_conflicts_asks_first(cx: &mut TestAppContext) {
    let (root, _bare, git) = conflicting_repository("merge-apply-anyway");
    let (window, state) = start_conflicting_merge(cx, root.clone());
    let before = fs::read(root.join("main.txt")).unwrap();

    click(cx, window, "conflict-merge-0");
    draw(cx, window);
    click(cx, window, "merge-apply");
    wait_for(cx, "the question about the conflict", |cx| {
        has_selector(cx, window, "merge-apply-anyway")
    });
    assert_eq!(
        fs::read(root.join("main.txt")).unwrap(),
        before,
        "nothing is written before the question is answered"
    );

    click(cx, window, "merge-apply-back");
    draw(cx, window);
    assert!(
        !has_selector(cx, window, "merge-apply-anyway"),
        "Back closes the question"
    );
    assert!(
        state.read_with(cx, |state, _| state
            .conflicts()
            .iter()
            .any(|path| path == "main.txt")),
        "the file is still in conflict"
    );

    click(cx, window, "merge-apply");
    wait_for(cx, "the question again", |cx| {
        has_selector(cx, window, "merge-apply-anyway")
    });
    click(cx, window, "merge-apply-anyway");
    wait_for(cx, "the base text to be applied", |_| {
        read_text(&root.join("main.txt")) == "main\n"
    });
    wait_for(cx, "the conflict to go", |cx| {
        state.read_with(cx, |state, _| {
            !state.conflicts().iter().any(|path| path == "main.txt")
        })
    });
    let status = String::from_utf8(
        git.run(&root, ["status", "--porcelain=v2"])
            .unwrap_or_default(),
    )
    .unwrap_or_default();
    for line in status.lines().filter(|line| line.contains("main.txt")) {
        assert!(
            !line.contains("UU"),
            "the file is staged as resolved: {line}"
        );
    }
}

#[ui_kit::gpui::test]
fn a_delete_modify_conflict_takes_their_version(cx: &mut TestAppContext) {
    let (root, _bare, _git) = delete_modify_repository("merge-delete-modify");
    let (window, state) = start_conflicting_merge(cx, root.clone());

    click(cx, window, "conflict-merge-0");
    draw(cx, window);
    click(cx, window, "merge-accept-theirs-0");
    draw(cx, window);
    click(cx, window, "merge-apply");

    wait_for(cx, "their text in the file", |_| {
        read_text(&root.join("main.txt")) == "remote main\n"
    });
    wait_for(cx, "the conflict to go", |cx| {
        state.read_with(cx, |state, _| {
            !state.conflicts().iter().any(|path| path == "main.txt")
        })
    });
}

#[ui_kit::gpui::test]
fn a_conflict_in_windows_1251_keeps_its_bytes_and_crlf(cx: &mut TestAppContext) {
    let base = "первая строка\nвторая строка\nтретья строка\n";
    let theirs = "первая строка\nвторая строка от них\nтретья строка\n";
    let ours = "первая строка\nвторая строка от нас\nтретья строка\n";
    let (root, _bare, _git) = conflict_repository(
        "merge-1251",
        "main.txt",
        &cp1251_crlf(base),
        &cp1251_crlf(ours),
        &cp1251_crlf(theirs),
    );
    let (window, _state) = start_conflicting_merge(cx, root.clone());

    click(cx, window, "conflict-merge-0");
    draw(cx, window);
    click(cx, window, "merge-accept-theirs-0");
    draw(cx, window);
    click(cx, window, "merge-apply");

    wait_for(cx, "the bytes of their version", |_| {
        fs::read(root.join("main.txt")).unwrap() == cp1251_crlf(theirs)
    });
    let decoded = text_core::read_file(&root.join("main.txt")).unwrap();
    assert_eq!(decoded.text, theirs, "the text stays the one of their side");
    assert_eq!(
        decoded.format.eol,
        EolStyle::Uniform(Eol::CrLf),
        "the line ends stay CRLF"
    );
}

#[ui_kit::gpui::test]
fn a_file_changed_on_disk_is_not_overwritten(cx: &mut TestAppContext) {
    let (root, _bare, _git) = conflicting_repository("merge-changed-on-disk");
    let (window, state) = start_conflicting_merge(cx, root.clone());

    click(cx, window, "conflict-merge-0");
    draw(cx, window);
    click(cx, window, "merge-accept-yours-0");
    draw(cx, window);
    fs::write(root.join("main.txt"), "edited behind the window\n").unwrap();
    click(cx, window, "merge-apply");
    wait_for(cx, "the write to end", |cx| settled(cx, window));

    assert_eq!(
        read_text(&root.join("main.txt")),
        "edited behind the window\n",
        "the file on disk wins"
    );
    assert!(
        has_selector(cx, window, "merge-result"),
        "the window stays open to tell what happened"
    );
    assert!(
        state.read_with(cx, |state, _| state
            .conflicts()
            .iter()
            .any(|path| path == "main.txt")),
        "the conflict is still there"
    );
}

#[ui_kit::gpui::test]
fn typing_in_a_big_result_does_not_reread_the_chunks(cx: &mut TestAppContext) {
    let base: String = (0..5000).map(|line| format!("line {line}\n")).collect();
    let ours =
        base.replacen("line 0\n", "ours at the top\n", 1)
            .replacen("line 10\n", "ours at ten\n", 1);
    let theirs = base
        .replacen("line 0\n", "theirs at the top\n", 1)
        .replacen("line 4999\n", "theirs at the end\n", 1);
    let (root, _bare, _git) = conflict_repository(
        "merge-big",
        "main.txt",
        base.as_bytes(),
        ours.as_bytes(),
        theirs.as_bytes(),
    );
    let (window, _state) = start_conflicting_merge(cx, root);

    click(cx, window, "conflict-merge-0");
    draw(cx, window);
    assert!(has_selector(cx, window, "merge-result"));
    let editor = merge_view(cx, window).read_with(cx, |view, _| view.result_editor());
    crate::window_tests::with_window(cx, window, |window, cx| {
        window.focus(&editor.read(cx).focus_handle(cx), cx);
    });
    reset_chunk_recomputes();

    for _ in 0..200 {
        crate::window_tests::press(cx, window, "a");
    }
    draw(cx, window);
    assert!(
        chunk_recomputes() <= 2,
        "the chunks were read {} times for 200 keystrokes",
        chunk_recomputes()
    );
}

#[ui_kit::gpui::test]
fn f7_walks_the_conflicts_of_the_merge_window(cx: &mut TestAppContext) {
    let (root, _bare, _git) = conflict_repository(
        "merge-conflict-keys",
        "main.txt",
        b"a\nb\nc\nd\ne\n",
        b"a\nours one\nc\nours two\ne\n",
        b"a\ntheirs one\nc\ntheirs two\ne\n",
    );
    let (window, _state) = start_conflicting_merge(cx, root);

    click(cx, window, "conflict-merge-0");
    draw(cx, window);
    let view = merge_view(cx, window);
    assert_eq!(view.read_with(cx, |view, _| view.selected()), 0);

    crate::window_tests::press(cx, window, "f7");
    assert_eq!(
        view.read_with(cx, |view, _| view.selected()),
        1,
        "F7 goes to the next conflict"
    );
    crate::window_tests::press(cx, window, "shift-f7");
    assert_eq!(
        view.read_with(cx, |view, _| view.selected()),
        0,
        "Shift+F7 goes to the previous conflict"
    );
}
