//! What a cached view rebuilds and what it keeps: a keystroke in the editor does not
//! reach the tool windows or the title bar, while their own changes, the theme and the
//! size of a dock still do.
//!
//! The frames here are drawn without `Window::refresh`, which would bypass the cache of
//! every view, so the counters show what the cache actually rebuilt.

#[cfg(windows)]
use std::sync::Arc;
use std::time::Duration;

use ui_kit::Theme;
use ui_kit::gpui;
use ui_kit::gpui::{AppContext, TestAppContext, WindowHandle};
use workspace::{AI_TOOL, PROJECT_TOOL, RUN_TOOL, TASKS_TOOL, Workspace, view_render_count};

use crate::project::ActiveProjectPanel;
use crate::window_tests::{
    bounds_of, click, draw, open_path, open_sample_window, press, wait_for_project_tree,
};
#[cfg(windows)]
use crate::window_tests::{open_window_with, sample_project};
#[cfg(windows)]
use crate::window_tests_run::{EchoRun, run_panel, setup, wait_for};

/// The number of draws of one heavy view.
fn renders(cx: &TestAppContext, view: &str) -> usize {
    cx.read(|cx| view_render_count(cx, view))
}

/// One frame that honors the cache of the views, unlike the `draw` helper of the window
/// tests, which refreshes every view on purpose.
fn frame(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    cx.update_window(window.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
    })
    .unwrap();
}

/// Lets the work of the window's own threads reach the views and draws the result.
fn settle(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    for _ in 0..4 {
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(20));
        frame(cx, window);
    }
}

fn show_tool(cx: &mut TestAppContext, window: WindowHandle<Workspace>, tool: &'static str) {
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(tool, window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
}

fn tree_has_row(cx: &TestAppContext, root: &std::path::Path, name: &str) -> bool {
    let path = root.join("src").join(name);
    cx.read(|cx| {
        cx.global::<ActiveProjectPanel>()
            .0
            .read(cx)
            .tree()
            .index_of(&path)
            .is_some()
    })
}

#[ui_kit::gpui::test]
fn keystrokes_and_caret_moves_do_not_rebuild_the_project_the_run_panel_or_the_title(
    cx: &mut TestAppContext,
) {
    let (root, window) = open_sample_window(cx, "cached-keystrokes");
    show_tool(cx, window, PROJECT_TOOL);
    show_tool(cx, window, RUN_TOOL);
    let path = root.join("src").join("main.txt");
    open_path(cx, window, &path);
    // The first keystroke may move the focus into the editor, which refreshes the whole
    // window: take it before the count that the keystrokes must not change.
    press(cx, window, "left");
    frame(cx, window);
    settle(cx, window);

    let before = (
        renders(cx, "project"),
        renders(cx, "run"),
        renders(cx, "title"),
    );
    assert!(before.0 > 0, "the project tree drew itself");

    for _ in 0..10 {
        press(cx, window, "a");
    }
    frame(cx, window);
    assert_eq!(
        (
            renders(cx, "project"),
            renders(cx, "run"),
            renders(cx, "title")
        ),
        before,
        "ten letters do not rebuild the tool windows or the title bar"
    );

    for _ in 0..10 {
        press(cx, window, "left");
    }
    frame(cx, window);
    assert_eq!(
        (
            renders(cx, "project"),
            renders(cx, "run"),
            renders(cx, "title")
        ),
        before,
        "ten caret moves do not rebuild the tool windows or the title bar"
    );
}

#[ui_kit::gpui::test]
fn a_click_and_an_expanded_folder_rebuild_the_project_tree(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "cached-tree");
    wait_for_project_tree(cx);
    settle(cx, window);

    let before = renders(cx, "project");
    click(cx, window, "tree-row-src");
    frame(cx, window);
    assert!(
        renders(cx, "project") > before,
        "expanding a folder rebuilds the project tree"
    );
    assert!(
        tree_has_row(cx, &root, "main.txt"),
        "the opened folder shows its files"
    );

    let file = root.join("src").join("main.txt");
    let before = renders(cx, "project");
    click(cx, window, "tree-row-main.txt");
    frame(cx, window);
    assert!(
        renders(cx, "project") > before,
        "a click on a row rebuilds the project tree"
    );
    let selected = cx.read(|cx| {
        cx.global::<ActiveProjectPanel>()
            .0
            .read(cx)
            .selected_path()
            .map(std::path::Path::to_path_buf)
    });
    assert_eq!(selected, Some(file), "the clicked row is selected");
}

// The program of the run is cmd.exe, as in the other Run window tests.
#[cfg(windows)]
#[ui_kit::gpui::test]
fn run_output_rebuilds_the_run_panel(cx: &mut TestAppContext) {
    let root = sample_project("cached-run");
    let mut setup = setup(&root);
    setup.providers = vec![Arc::new(EchoRun)];
    let window = open_window_with(cx, root, setup);
    show_tool(cx, window, RUN_TOOL);
    let panel = run_panel(cx);
    wait_for(cx, "a run configuration", |cx| {
        !panel.read_with(cx, |panel, _| panel.state().configs.is_empty())
    });
    settle(cx, window);

    let before = renders(cx, "run");
    press(cx, window, "shift-f10");
    wait_for(cx, "the output of the run", |cx| {
        panel
            .read_with(cx, |panel, _| panel.text())
            .iter()
            .any(|line| line.contains("hello from run"))
    });
    frame(cx, window);
    assert!(
        renders(cx, "run") > before,
        "the output rebuilds the Run panel"
    );
    let text = panel.read_with(cx, |panel, _| panel.text());
    assert!(
        text.iter().any(|line| line.contains("hello from run")),
        "the panel shows the output: {text:?}"
    );
}

#[ui_kit::gpui::test]
fn another_theme_rebuilds_the_project_tree_and_the_title(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "cached-theme");
    show_tool(cx, window, PROJECT_TOOL);
    settle(cx, window);

    let before = (renders(cx, "project"), renders(cx, "title"));
    cx.update(|cx| cx.set_global(Theme::light()));
    cx.run_until_parked();
    frame(cx, window);
    assert!(
        renders(cx, "project") > before.0,
        "a theme change rebuilds the project tree"
    );
    assert!(
        renders(cx, "title") > before.1,
        "a theme change rebuilds the title bar"
    );
}

#[ui_kit::gpui::test]
fn a_wider_dock_rebuilds_the_project_tree(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "cached-resize");
    show_tool(cx, window, PROJECT_TOOL);
    settle(cx, window);

    let before = renders(cx, "project");
    let size = window
        .read_with(cx, |workspace, _| {
            workspace.dock_size(workspace::DockSide::Left)
        })
        .unwrap();
    press(cx, window, "ctrl-alt-shift-right");
    let wider = window
        .read_with(cx, |workspace, _| {
            workspace.dock_size(workspace::DockSide::Left)
        })
        .unwrap();
    assert!(wider > size, "the key widened the left dock: {wider}");
    frame(cx, window);
    assert!(
        renders(cx, "project") > before,
        "a wider dock rebuilds the project tree"
    );
}

/// The stripes are cached views of their own: the bottom group of the left stripe still
/// sits at the bottom of the window, below the middle of the window.
#[ui_kit::gpui::test]
fn the_bottom_group_of_the_left_stripe_stays_at_the_bottom(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "cached-stripe");
    settle(cx, window);
    let project = bounds_of(cx, window, "stripe-s-project");
    let terminal = bounds_of(cx, window, "stripe-s-terminal");
    let height = window
        .update(cx, |_, window, _| window.viewport_size().height)
        .unwrap();
    assert!(
        terminal.origin.y > height / 2.,
        "the terminal button is in the bottom half: {terminal:?} of {height:?}"
    );
    assert!(
        terminal.origin.y > project.origin.y,
        "the bottom group is below the top group"
    );
}

/// The AI button shows the AI sessions even after another tool window of the right dock,
/// and hides them on the second click.
#[ui_kit::gpui::test]
fn the_ai_button_shows_the_ai_sessions_after_another_tool_of_the_right_dock(
    cx: &mut TestAppContext,
) {
    let (_, window) = open_sample_window(cx, "cached-ai-button");
    show_tool(cx, window, TASKS_TOOL);
    settle(cx, window);
    let ai_open = |cx: &mut TestAppContext| {
        window
            .read_with(cx, |workspace, _| workspace.tool_open(AI_TOOL))
            .unwrap()
    };
    assert!(!ai_open(cx), "Tasks is the tool of the right dock");

    click(cx, window, "stripe-s-ai");
    settle(cx, window);
    assert!(ai_open(cx), "the AI button shows the AI sessions");

    click(cx, window, "stripe-s-ai");
    settle(cx, window);
    assert!(!ai_open(cx), "the second click hides them");
}
