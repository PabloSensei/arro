//! Nested splits of the editor: a split inside a group splits that group only, every
//! pair has its own divider, the sizes go to the saved layout with the tabs, and a group
//! whose last tab closes takes its node away.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ui_kit::Theme;
use ui_kit::gpui;
use ui_kit::gpui::{
    Modifiers, MouseButton, Pixels, Point, TestAppContext, VisualTestContext, WindowHandle,
};
use ui_kit::{point, px};
use workspace::{
    FileIcons, Launch, Layout, PaneState, Settings, SplitLayout, Workspace, bind_keys, load_layout,
};

use crate::window_tests::{bounds_of, draw, open_path, open_sample_window, scratch};

fn split(cx: &mut TestAppContext, window: WindowHandle<Workspace>, down: bool) {
    window
        .update(cx, |workspace, window, cx| {
            if down {
                workspace.split_down(window, cx)
            } else {
                workspace.split_right(window, cx)
            }
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
}

fn close_active(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    window
        .update(cx, |workspace, window, cx| {
            workspace.close_active_tab(window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
}

fn ratio(cx: &TestAppContext, window: WindowHandle<Workspace>, path: &[usize]) -> Option<f32> {
    window
        .read_with(cx, |workspace, _| workspace.split_ratio(path))
        .unwrap()
}

fn drag_to(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    from: Point<Pixels>,
    to: Point<Pixels>,
) {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_move(from, None, Modifiers::default());
    visual.simulate_mouse_down(from, MouseButton::Left, Modifiers::default());
    for step in 1..=8 {
        let fraction = step as f32 / 8.;
        visual.simulate_mouse_move(
            point(
                from.x + (to.x - from.x) * fraction,
                from.y + (to.y - from.y) * fraction,
            ),
            Some(MouseButton::Left),
            Modifiers::default(),
        );
    }
    visual.simulate_mouse_up(to, MouseButton::Left, Modifiers::default());
    drop(visual);
    cx.run_until_parked();
    draw(cx, window);
}

fn open_paths(cx: &TestAppContext, window: WindowHandle<Workspace>) -> Vec<Vec<PathBuf>> {
    window
        .read_with(cx, |workspace, _| workspace.open_tabs())
        .unwrap()
}

/// A window with three groups: `main` on the left, `notes` and `readme` one under
/// another on the right.
fn three_groups(
    cx: &mut TestAppContext,
    name: &str,
) -> (PathBuf, WindowHandle<Workspace>, PathBuf, PathBuf, PathBuf) {
    let (root, window) = open_sample_window(cx, name);
    let main = root.join("src").join("main.txt");
    let notes = root.join("src").join("notes.txt");
    let readme = root.join("README.txt");
    open_path(cx, window, &main);
    split(cx, window, false);
    open_path(cx, window, &notes);
    split(cx, window, true);
    open_path(cx, window, &readme);
    (root, window, main, notes, readme)
}

/// A launch of a window of `root`, for a second window of the same project.
fn launch_of(root: &Path, state_dir: &Path) -> Launch {
    Launch {
        root: root.to_path_buf(),
        open: None,
        state_dir: state_dir.to_path_buf(),
        settings_dir: state_dir.to_path_buf(),
        icons: FileIcons {
            by_name: Vec::new(),
            fallback: "fileTypes/text",
        },
        background: true,
        info: Default::default(),
    }
}

fn globals(cx: &mut TestAppContext) {
    cx.update(|cx| {
        cx.set_global(Theme::dark());
        cx.set_global(Settings::default());
        bind_keys(cx);
    });
}

/// A window built the way the application builds it, from a layout and a store that came
/// from an earlier run.
fn open_saved_window(
    cx: &mut TestAppContext,
    root: &Path,
    state_dir: &Path,
    layout: Layout,
    store: Option<Arc<store::Store>>,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let launch = launch_of(root, state_dir);
    let setup = crate::window_tests::project_setup(scratch("nested-splits-cache"));
    let agents = crate::AgentOptions {
        state_dir: state_dir.to_path_buf(),
        mock: false,
    };
    let (status_sender, status) = futures_channel::mpsc::unbounded();
    status_sender
        .unbounded_send("No language server in the test".to_string())
        .ok();
    let server = crate::LanguageServer {
        support: None,
        status,
    };
    let window = cx.add_window(move |window, cx| {
        let mut workspace = Workspace::new_for_test(launch, layout, store, Vec::new(), window, cx);
        crate::install(
            &mut workspace,
            crate::index::SearchSetup::default(),
            setup,
            agents,
            window,
            cx,
        );
        crate::connect(&mut workspace, server, cx);
        workspace
    });
    window
        .update(cx, |workspace, window, cx| {
            workspace.restore_tabs(window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    window
}

#[ui_kit::gpui::test]
fn split_down_in_the_right_group_makes_a_column_beside_the_left_one(cx: &mut TestAppContext) {
    let (_root, window, main, notes, readme) = three_groups(cx, "nested-splits-tree");
    assert_eq!(
        open_paths(cx, window),
        vec![vec![main], vec![notes], vec![readme]],
        "the second split divides the right group, not the whole window"
    );

    let left = bounds_of(cx, window, "editor-body-0");
    let upper = bounds_of(cx, window, "editor-body-1");
    let lower = bounds_of(cx, window, "editor-body-2");
    assert!(
        (left.top() - upper.top()).abs() < px(1.),
        "the first group starts at the top with the upper right group"
    );
    assert!(
        left.size.height > upper.size.height * 1.5,
        "the first group takes the whole height: {} and {}",
        left.size.height,
        upper.size.height
    );
    assert!(
        (upper.left() - lower.left()).abs() < px(1.),
        "the two right groups start at the same edge"
    );
    assert!(
        lower.top() > upper.top(),
        "the third group sits under the second one"
    );
}

#[ui_kit::gpui::test]
fn dragging_a_divider_changes_only_the_pair_it_belongs_to(cx: &mut TestAppContext) {
    let (_root, window, _, _, _) = three_groups(cx, "nested-splits-drag");
    assert_eq!(ratio(cx, window, &[]), Some(0.5));
    assert_eq!(ratio(cx, window, &[1]), Some(0.5));

    // The divider of the left group and the column on the right.
    let divider = bounds_of(cx, window, "split-divider-0").center();
    drag_to(cx, window, divider, point(divider.x + px(80.), divider.y));
    let moved = ratio(cx, window, &[]).unwrap();
    assert!(moved > 0.55, "the left group grew: {moved}");
    assert_eq!(
        ratio(cx, window, &[1]),
        Some(0.5),
        "the nested pair keeps its size"
    );

    // The divider between the two right groups moves only those two.
    let nested = bounds_of(cx, window, "split-divider-1").center();
    drag_to(cx, window, nested, point(nested.x, nested.y + px(80.)));
    assert!(ratio(cx, window, &[1]).unwrap() > 0.55);
    assert_eq!(
        ratio(cx, window, &[]),
        Some(moved),
        "the pair on the left keeps its size"
    );
}

#[ui_kit::gpui::test]
fn the_saved_layout_keeps_the_nested_tree_the_sizes_and_the_tabs(cx: &mut TestAppContext) {
    let (root, window, main, notes, readme) = three_groups(cx, "nested-splits-save");
    let divider = bounds_of(cx, window, "split-divider-0").center();
    drag_to(cx, window, divider, point(divider.x + px(60.), divider.y));
    let saved_ratio = ratio(cx, window, &[]).unwrap();
    let state_dir = window
        .read_with(cx, |workspace, _| workspace.state_dir().to_path_buf())
        .unwrap();
    let store = window
        .read_with(cx, |workspace, _| workspace.store())
        .unwrap()
        .expect("the test window has a store");

    // Closing the window writes the layout, as a quit does.
    window
        .update(cx, |workspace, window, cx| {
            workspace.close_window(window, cx)
        })
        .unwrap();
    cx.run_until_parked();

    // The application reads the layout of the store before it builds the window.
    let layout = load_layout(&store, &root);
    let second = open_saved_window(cx, &root, &state_dir, layout, Some(store));
    draw(cx, second);

    assert_eq!(
        open_paths(cx, second),
        vec![vec![main], vec![notes], vec![readme]],
        "the groups come back in the order of the tree"
    );
    let restored = ratio(cx, second, &[]).unwrap();
    assert!(
        (restored - saved_ratio).abs() < 0.001,
        "the saved sizes come back: {restored} and {saved_ratio}"
    );
    assert!(
        ratio(cx, second, &[1]).is_some(),
        "the nested split comes back"
    );
}

#[ui_kit::gpui::test]
fn a_saved_layout_without_a_tree_opens_as_one_column(cx: &mut TestAppContext) {
    globals(cx);
    let root = scratch("nested-splits-old");
    fs::create_dir_all(root.join("src")).unwrap();
    let main = root.join("src").join("main.txt");
    let notes = root.join("src").join("notes.txt");
    fs::write(&main, "alpha\n").unwrap();
    fs::write(&notes, "beta\n").unwrap();

    let layout = Layout {
        split_down: true,
        active_pane: 1,
        panes: vec![
            PaneState {
                tabs: vec![main.clone()],
                active: 0,
                carets: Vec::new(),
                pinned: Vec::new(),
            },
            PaneState {
                tabs: vec![notes.clone()],
                active: 0,
                carets: Vec::new(),
                pinned: Vec::new(),
            },
        ],
        ..Layout::default()
    };
    let state_dir = scratch("nested-splits-old-state");
    let store = store::Store::open(&state_dir.join("state.sqlite"))
        .ok()
        .map(Arc::new);
    let window = open_saved_window(cx, &root, &state_dir, layout, store);
    draw(cx, window);

    assert_eq!(open_paths(cx, window), vec![vec![main], vec![notes]]);
    let layout = window
        .read_with(cx, |workspace, cx| workspace.current_layout(cx))
        .unwrap();
    assert!(
        matches!(layout.splits, Some(SplitLayout::Split { down: true, .. })),
        "the two groups stand one under another"
    );
    let upper = bounds_of(cx, window, "editor-body-0");
    let lower = bounds_of(cx, window, "editor-body-1");
    assert!(lower.top() > upper.top());
    assert!((lower.left() - upper.left()).abs() < px(1.));
}

#[ui_kit::gpui::test]
fn unsplit_joins_a_group_with_the_one_on_the_other_side_of_its_split(cx: &mut TestAppContext) {
    let (_root, window, main, notes, readme) = three_groups(cx, "nested-splits-unsplit");
    let ran = window
        .update(cx, |workspace, window, cx| {
            workspace.run_command("window.unsplit", window, cx)
        })
        .unwrap();
    assert!(ran, "Unsplit is a command");
    cx.run_until_parked();
    draw(cx, window);
    assert_eq!(
        open_paths(cx, window),
        vec![vec![main.clone()], vec![notes, readme]],
        "the lower right group joined the upper one"
    );
    assert_eq!(
        ratio(cx, window, &[1]),
        None,
        "the split between the two is gone"
    );

    // The right group joins the left one, and the window keeps a single group.
    window
        .update(cx, |workspace, window, cx| workspace.unsplit(window, cx))
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
    assert_eq!(open_paths(cx, window).len(), 1);
    assert_eq!(ratio(cx, window, &[]), None);
}

#[ui_kit::gpui::test]
fn closing_the_last_tab_of_a_group_takes_its_node_away(cx: &mut TestAppContext) {
    let (_root, window, main, notes, _readme) = three_groups(cx, "nested-splits-close");
    assert_eq!(open_paths(cx, window).len(), 3);

    // The lower right group goes; the split above it joins the upper group.
    close_active(cx, window);
    assert_eq!(
        open_paths(cx, window),
        vec![vec![main.clone()], vec![notes.clone()]]
    );
    assert_eq!(
        ratio(cx, window, &[1]),
        None,
        "the split of the closed group is gone"
    );
    assert!(ratio(cx, window, &[]).is_some());

    // The last tab of the right group leaves the first one alone in the window.
    close_active(cx, window);
    assert_eq!(open_paths(cx, window), vec![vec![main]]);
    assert_eq!(ratio(cx, window, &[]), None, "no split is left");
}
