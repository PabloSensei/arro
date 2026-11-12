//! The right pane of a diff as a view of its own over the buffer of the tab, the rows
//! that folds take away, and the recently edited files of Recent Files, without a window.

use editor_view::{EditorView, FoldStyle, LanguageConfig, Languages};
use keymap::editor as editor_keys;
use keymap::workspace as keys;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use ui_kit::{Focusable, Theme, px};
use workspace::{Launch, Layout, Settings, Workspace};

use crate::diff_view::DiffView;
use crate::index::SearchSetup;
use crate::palette::Palette;
use crate::run::ProjectSetup;
use crate::window_tests::{
    bounds_of, draw, editor_of, open_palette, open_path, open_window, press, sample_project,
    scratch, type_text, with_window,
};
use crate::window_tests_git::{git_state, repository_with_origin, wait_for};
use crate::{AgentOptions, LanguageServer, connect, install};

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

fn scroll(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) {
    let position = bounds_of(cx, window, selector).center();
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_event(gpui::ScrollEvent {
        position,
        delta: gpui::ScrollDelta::Lines(gpui::point(0., -6.)),
        modifiers: gpui::Modifiers::default(),
        ..Default::default()
    });
    cx.run_until_parked();
    draw(cx, window);
}

fn top_of(cx: &TestAppContext, editor: &gpui::Entity<EditorView>, line: usize) -> f32 {
    editor
        .read_with(cx, |view, _| {
            view.row_paint()
                .into_iter()
                .find(|row| !row.block && row.line == line)
                .map(|row| row.top)
        })
        .unwrap_or_else(|| panic!("the editor has a row for line {line}"))
}

fn buffer_of(cx: &TestAppContext, editor: &gpui::Entity<EditorView>) -> String {
    editor
        .read_with(cx, |view, _| view.text())
        .unwrap_or_default()
}

fn commit(root: &Path, text: &str) {
    let git = crate::window_tests_git::git();
    fs::write(root.join("main.txt"), text).unwrap();
    git.run(root, ["add", "main.txt"]).unwrap();
    git.run(root, ["commit", "-m", "base"]).unwrap();
}

#[ui_kit::gpui::test]
fn the_right_pane_has_its_own_scroll_and_caret_over_the_buffer_of_the_tab(cx: &mut TestAppContext) {
    let (root, _bare, _git) = repository_with_origin("diff-own-view");
    let mut base = String::new();
    for line in 0..40 {
        base.push_str(&format!("line {line}\n"));
    }
    commit(&root, &base);
    let current = base.replace("line 20\n", "changed 20\n");
    fs::write(root.join("main.txt"), &current).unwrap();
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let view = open_diff(cx, window, &root);
    wait_for(cx, "the changed line", |cx| {
        view.read_with(cx, |view, _| view.hunk_count()) == 1
    });
    let tab = editor_of(cx, window, &root.join("main.txt"));
    let right = view.read_with(cx, |view, _| view.right_editor());
    assert_ne!(
        right, tab,
        "the right pane is a view of its own, not the tab"
    );
    assert_eq!(
        buffer_of(cx, &right),
        buffer_of(cx, &tab),
        "the two views show the same buffer"
    );

    // The wheel in the diff scrolls the diff alone.
    scroll(cx, window, "diff-pane-right");
    let right_top = right.read_with(cx, |view, _| view.scroll_top());
    assert!(right_top > 0., "the wheel scrolled the right pane");
    assert_eq!(
        tab.read_with(cx, |view, _| view.scroll_top()),
        0.,
        "the tab under the diff did not move"
    );

    // The other way round: the tab scrolls without the diff following.
    tab.update(cx, |tab, cx| tab.scroll_to_top(120., cx));
    cx.run_until_parked();
    draw(cx, window);
    assert_eq!(
        right.read_with(cx, |view, _| view.scroll_top()),
        right_top,
        "the diff kept its own scroll"
    );

    // A caret put in the diff stays in the diff.
    right.update(cx, |right, cx| right.restore_caret(2, 1, cx));
    cx.run_until_parked();
    assert_eq!(
        tab.read_with(cx, |view, _| view.caret()),
        (0, 0),
        "the caret of the diff did not land in the tab"
    );
    tab.update(cx, |tab, cx| tab.restore_caret(5, 2, cx));
    cx.run_until_parked();
    assert_eq!(
        right.read_with(cx, |view, _| view.caret()),
        (2, 1),
        "the caret of the tab did not land in the diff"
    );

    // An edit made in the diff is in the tab at once, and Ctrl+Z in the tab takes it back.
    right.update(cx, |right, cx| right.restore_caret(0, 0, cx));
    cx.run_until_parked();
    type_text(cx, window, "Z");
    assert!(
        buffer_of(cx, &right).starts_with('Z'),
        "the diff took the typed text"
    );
    assert!(
        buffer_of(cx, &tab).starts_with('Z'),
        "the tab shows the edit of the diff at once"
    );
    tab.update(cx, |tab, cx| tab.run(editor_keys::UNDO, cx));
    cx.run_until_parked();
    draw(cx, window);
    assert_eq!(
        buffer_of(cx, &tab),
        current,
        "Ctrl+Z in the tab takes back the edit of the diff"
    );
    assert_eq!(
        buffer_of(cx, &right),
        current,
        "the diff follows the undo of the tab"
    );
}

#[ui_kit::gpui::test]
fn the_diff_works_after_its_tab_is_closed(cx: &mut TestAppContext) {
    let (root, _bare, _git) = repository_with_origin("diff-own-view-closed-tab");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let view = open_diff(cx, window, &root);
    wait_for(cx, "the changed text", |cx| {
        view.read_with(cx, |view, _| view.hunk_count()) == 0
    });
    window
        .update(cx, |workspace, window, cx| {
            workspace.close_active_tab(window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);

    let right = view.read_with(cx, |view, _| view.right_editor());
    let focus = right.read_with(cx, |view, cx| view.focus_handle(cx));
    window
        .update(cx, |_, window, cx| window.focus(&focus, cx))
        .unwrap();
    right.update(cx, |right, cx| right.restore_caret(0, 0, cx));
    cx.run_until_parked();
    type_text(cx, window, "Z");
    assert!(
        buffer_of(cx, &right).starts_with('Z'),
        "the right pane still takes text after the tab is gone"
    );
    right.update(cx, |right, cx| {
        assert!(
            right.save_blocking(cx).is_none(),
            "the right pane still writes the file"
        );
    });
    assert_eq!(
        fs::read_to_string(root.join("main.txt"))
            .unwrap()
            .replace("\r\n", "\n"),
        "Zalpha\nbeta\n",
        "the edit of the right pane reached the disk"
    );
}

static FOLD_STYLES: &[FoldStyle] = &[FoldStyle {
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
fn a_fold_in_the_right_pane_keeps_the_hunks_face_to_face(cx: &mut TestAppContext) {
    let (root, _bare, _git) = repository_with_origin("diff-own-view-fold");
    let base = "a0\n//region folded\na2\na3\na4\n//endregion\na6\n";
    commit(&root, base);
    let current = "A0\n//region folded\na2\na3\na4\n//endregion\nA6\n";
    fs::write(root.join("main.txt"), current).unwrap();
    let window = open_window(cx, root.clone());
    cx.update(|cx| {
        cx.set_global(Languages {
            resolve: sample_language,
            templates: |_| None,
        });
    });
    cx.run_until_parked();
    let view = open_diff(cx, window, &root);
    wait_for(cx, "the two hunks", |cx| {
        view.read_with(cx, |view, _| view.hunk_count()) == 2
    });
    let right = view.read_with(cx, |view, _| view.right_editor());
    let left = view.read_with(cx, |view, _| view.left_editor());
    let before = right.read_with(cx, |view, _| view.display_rows());
    right.update(cx, |right, cx| right.restore_caret(1, 0, cx));
    wait_for(cx, "the syntax tree", |cx| {
        right.read_with(cx, |view, _| view.has_syntax_tree())
    });
    let focus = right.read_with(cx, |view, cx| view.focus_handle(cx));
    window
        .update(cx, |_, window, cx| window.focus(&focus, cx))
        .unwrap();
    cx.run_until_parked();
    press(cx, window, "ctrl--");
    cx.run_until_parked();
    draw(cx, window);
    assert!(
        right.read_with(cx, |view, _| view.display_rows()) < before,
        "the region folded in the right pane"
    );

    let starts = view.read_with(cx, |view, _| view.hunk_starts());
    assert_eq!(starts.len(), 2);
    for (index, (left_line, right_line)) in starts.into_iter().enumerate() {
        let (Some(left_line), Some(right_line)) = (left_line, right_line) else {
            panic!("hunk {index} has a line on both sides");
        };
        let (left_top, right_top) = (top_of(cx, &left, left_line), top_of(cx, &right, right_line));
        assert!(
            (left_top - right_top).abs() < 0.5,
            "the first row of hunk {index} is at {left_top} on the left and {right_top} on the right"
        );
    }
}

/// Opens the window of `root` with `state_dir`, and the layout saved there.
fn open_at(cx: &mut TestAppContext, root: PathBuf, state_dir: PathBuf) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let store = store::Store::open(&state_dir.join("state.sqlite"))
        .ok()
        .map(Arc::new);
    let layout = store
        .as_ref()
        .and_then(|store| {
            store
                .get(&format!("layout:{}", root.display()))
                .ok()
                .flatten()
        })
        .and_then(|json| serde_json::from_str::<Layout>(&json).ok())
        .unwrap_or_default();
    cx.update(|cx| {
        cx.set_global(Theme::dark());
        cx.set_global(Settings::default());
        cx.set_global(Languages {
            resolve: |path| dockerfile_syntax::is_dockerfile(path).then(dockerfile_syntax::config),
            templates: |_| None,
        });
        workspace::bind_keys(cx);
    });
    let launch = Launch {
        root,
        open: None,
        state_dir: state_dir.clone(),
        settings_dir: state_dir,
        icons: workspace::FileIcons {
            by_name: Vec::new(),
            fallback: "fileTypes/text",
        },
        background: true,
        info: Default::default(),
    };
    let agents = AgentOptions {
        state_dir: launch.state_dir.clone(),
        mock: false,
    };
    let (status_sender, status) = futures_channel::mpsc::unbounded();
    status_sender
        .unbounded_send("No language server in the test".to_string())
        .ok();
    let server = LanguageServer {
        support: None,
        status,
    };
    let setup = ProjectSetup {
        tool: None,
        providers: Vec::new(),
        tests: None,
        test_sites: |_, _| Vec::new(),
        cache_root: scratch("diff-own-view-cache"),
        templates: Vec::new(),
        skipped_folders: Vec::new(),
        environment: None,
        debug: None,
    };
    cx.add_window(move |window, cx| {
        let mut workspace = Workspace::new_for_test(launch, layout, store, Vec::new(), window, cx);
        install(
            &mut workspace,
            SearchSetup::default(),
            setup,
            agents,
            window,
            cx,
        );
        connect(&mut workspace, server, cx);
        workspace
    })
}

fn palette(cx: &TestAppContext, window: WindowHandle<Workspace>) -> gpui::Entity<Palette> {
    window
        .read_with(cx, |workspace, _| workspace.overlay_view())
        .unwrap()
        .expect("a popup is open")
        .downcast::<Palette>()
        .expect("the popup is the palette")
}

fn row_titles(cx: &TestAppContext, window: WindowHandle<Workspace>) -> Vec<String> {
    palette(cx, window)
        .read_with(cx, |palette, _| palette.rows_text())
        .into_iter()
        .map(|(title, _)| title)
        .collect()
}

#[ui_kit::gpui::test]
fn recent_files_shows_the_files_that_were_edited_and_saved(cx: &mut TestAppContext) {
    let root = sample_project("diff-own-view-edited");
    let state_dir = scratch("diff-own-view-edited-state");
    let window = open_at(cx, root.clone(), state_dir.clone());
    cx.run_until_parked();
    draw(cx, window);
    let main = root.join("src").join("main.txt");
    let notes = root.join("src").join("notes.txt");
    open_path(cx, window, &main);
    let view = editor_of(cx, window, &main);
    view.update(cx, |view, cx| {
        view.replace_text(0..5, "one", cx);
        assert!(view.save_blocking(cx).is_none());
    });
    cx.run_until_parked();
    open_path(cx, window, &notes);
    cx.run_until_parked();

    press(cx, window, "ctrl-e");
    press(cx, window, "ctrl-e");
    draw(cx, window);
    let rows = row_titles(cx, window);
    assert!(
        rows.iter().any(|row| row == "main.txt"),
        "the edited and saved file is in the list: {rows:?}"
    );
    assert!(
        rows.iter().all(|row| row != "notes.txt"),
        "a file that was only opened is not: {rows:?}"
    );
    press(cx, window, "escape");

    // The list is part of the state of the project: a new window on the same folder
    // reads it back.
    window
        .update(cx, |workspace, window, cx| {
            workspace.close_window(window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    let window = open_at(cx, root.clone(), state_dir);
    cx.run_until_parked();
    draw(cx, window);
    let edited: Vec<PathBuf> = window
        .read_with(cx, |workspace, _| workspace.recent_edited().to_vec())
        .unwrap();
    assert_eq!(edited, vec![main.clone()], "the list survived the restart");
    press(cx, window, "ctrl-e");
    press(cx, window, "ctrl-e");
    draw(cx, window);
    let rows = row_titles(cx, window);
    assert!(
        rows.iter().any(|row| row == "main.txt"),
        "Show edited only reads the saved list: {rows:?}"
    );
    press(cx, window, "escape");

    // The action of IDEA opens the same list with the checkbox already on.
    open_palette(cx, window, keys::RECENT_CHANGED_FILES);
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "palette-edited-only").size.height > px(0.),
        "Recently Changed Files shows the checkbox"
    );
    let rows = row_titles(cx, window);
    assert!(
        rows.iter().any(|row| row == "main.txt"),
        "Recently Changed Files lists the edited file: {rows:?}"
    );
    press(cx, window, "escape");

    // The checkbox of the Switcher reads the same list.
    open_palette(cx, window, keys::SWITCHER);
    press(cx, window, "ctrl-e");
    draw(cx, window);
    let rows = row_titles(cx, window);
    assert!(
        rows.iter().any(|row| row == "main.txt"),
        "the Switcher checkbox lists the edited file: {rows:?}"
    );
    press(cx, window, "escape");
}
