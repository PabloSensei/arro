//! The Bookmarks window: the tree of groups, files and lines, the jump of Enter, Delete
//! of a mark, the description and the groups that come back with the saved layout.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ui_kit::gpui;
use ui_kit::gpui::{Modifiers, MouseButton, TestAppContext, VisualTestContext, WindowHandle};
use ui_kit::{Focusable, Theme, px};
use workspace::{
    BOOKMARKS_TOOL, FileIcons, Launch, Layout, Settings, Workspace, bind_keys, load_layout,
};

use crate::bookmarks::Row;
use crate::window_tests::{
    active_path, click, draw, editor_of, open_path, open_sample_window, press, project_setup,
    scratch, type_text,
};

/// The bookmark rows the tests reach by their index, written out because the debug
/// selector of a row has to be a static string. A row of another kind has another name.
const ROW_SELECTORS: [&str; 32] = [
    "bookmark-item-row-0",
    "bookmark-item-row-1",
    "bookmark-item-row-2",
    "bookmark-item-row-3",
    "bookmark-item-row-4",
    "bookmark-item-row-5",
    "bookmark-item-row-6",
    "bookmark-item-row-7",
    "bookmark-item-row-8",
    "bookmark-item-row-9",
    "bookmark-item-row-10",
    "bookmark-item-row-11",
    "bookmark-item-row-12",
    "bookmark-item-row-13",
    "bookmark-item-row-14",
    "bookmark-item-row-15",
    "bookmark-item-row-16",
    "bookmark-item-row-17",
    "bookmark-item-row-18",
    "bookmark-item-row-19",
    "bookmark-item-row-20",
    "bookmark-item-row-21",
    "bookmark-item-row-22",
    "bookmark-item-row-23",
    "bookmark-item-row-24",
    "bookmark-item-row-25",
    "bookmark-item-row-26",
    "bookmark-item-row-27",
    "bookmark-item-row-28",
    "bookmark-item-row-29",
    "bookmark-item-row-30",
    "bookmark-item-row-31",
];

fn panel(cx: &TestAppContext) -> gpui::Entity<crate::bookmarks::BookmarksPanel> {
    cx.read(|cx| cx.global::<crate::bookmarks::ActiveBookmarks>().0.clone())
}

fn rows(cx: &TestAppContext) -> Vec<Row> {
    panel(cx).read_with(cx, |panel, _| panel.rows().to_vec())
}

fn row_index(cx: &TestAppContext, path: &Path, line: u32) -> usize {
    rows(cx)
        .iter()
        .position(|row| {
            matches!(
                row,
                Row::Bookmark {
                    path: marked,
                    line: marked_line,
                    ..
                } if marked == path && *marked_line == line
            )
        })
        .expect("the bookmark is in the tree")
}

fn bounds_named(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &'static str,
) -> gpui::Bounds<gpui::Pixels> {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("no element {selector} on screen"))
}

fn click_row(cx: &mut TestAppContext, window: WindowHandle<Workspace>, index: usize) {
    let bounds = bounds_named(cx, window, ROW_SELECTORS[index]);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_click(bounds.center(), Modifiers::default());
    cx.run_until_parked();
}

fn right_click_row(cx: &mut TestAppContext, window: WindowHandle<Workspace>, index: usize) {
    let bounds = bounds_named(cx, window, ROW_SELECTORS[index]);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_down(bounds.center(), MouseButton::Right, Modifiers::default());
    visual.simulate_mouse_up(bounds.center(), MouseButton::Right, Modifiers::default());
    cx.run_until_parked();
}

fn open_bookmarks(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    click(cx, window, "stripe-s-bookmarks");
    draw(cx, window);
    let open = window
        .read_with(cx, |workspace, _| workspace.tool_open(BOOKMARKS_TOOL))
        .unwrap();
    assert!(open, "the stripe button opens the Bookmarks window");
}

fn bookmark(cx: &mut TestAppContext, window: WindowHandle<Workspace>, path: &Path, line: usize) {
    open_path(cx, window, path);
    let view = editor_of(cx, window, path);
    view.update(cx, |view, cx| view.restore_caret(line, 0, cx));
    press(cx, window, "f11");
}

fn groups(cx: &TestAppContext) -> Vec<editor_view::BookmarkGroup> {
    cx.read(|cx| editor_view::bookmark_groups(cx).to_vec())
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
    cx.update(|cx| {
        cx.set_global(Theme::dark());
        cx.set_global(Settings::default());
        bind_keys(cx);
    });
    let launch = launch_of(root, state_dir);
    let setup = project_setup(scratch("bookmarks-cache"));
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
fn two_bookmarks_of_two_files_are_in_the_tree(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "bookmarks-tree");
    let main = root.join("src").join("main.txt");
    let notes = root.join("src").join("notes.txt");
    bookmark(cx, window, &main, 0);
    bookmark(cx, window, &notes, 1);
    open_bookmarks(cx, window);

    let rows = rows(cx);
    assert!(
        matches!(rows.first(), Some(Row::Group { .. })),
        "the tree starts with its group"
    );
    let files: Vec<PathBuf> = rows
        .iter()
        .filter_map(|row| match row {
            Row::File { path, .. } => Some(path.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        files,
        [main.clone(), notes.clone()],
        "both files are under the group"
    );
    let marks: Vec<(PathBuf, u32)> = rows
        .iter()
        .filter_map(|row| match row {
            Row::Bookmark { path, line, .. } => Some((path.clone(), *line)),
            _ => None,
        })
        .collect();
    assert_eq!(marks, [(main.clone(), 0), (notes.clone(), 1)]);

    let main_row = row_index(cx, &main, 0);
    let notes_row = row_index(cx, &notes, 1);
    for index in [main_row, notes_row] {
        assert!(
            bounds_named(cx, window, ROW_SELECTORS[index]).size.height > px(0.),
            "the bookmark row {index} is drawn"
        );
    }
}

#[ui_kit::gpui::test]
fn enter_opens_the_bookmark_at_its_line(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "bookmarks-enter");
    let main = root.join("src").join("main.txt");
    let notes = root.join("src").join("notes.txt");
    bookmark(cx, window, &main, 1);
    bookmark(cx, window, &notes, 0);
    open_bookmarks(cx, window);
    assert_eq!(active_path(cx, window), Some(notes.clone()));

    let index = row_index(cx, &main, 1);
    click_row(cx, window, index);
    press(cx, window, "enter");
    draw(cx, window);

    assert_eq!(
        active_path(cx, window),
        Some(main.clone()),
        "Enter opens the file of the bookmark"
    );
    let view = editor_of(cx, window, &main);
    assert_eq!(
        view.read_with(cx, |view, _| view.caret().0),
        1,
        "the caret stands on the line of the bookmark"
    );
}

#[ui_kit::gpui::test]
fn delete_takes_the_bookmark_away_and_the_gutter_stops_drawing_it(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "bookmarks-delete");
    let main = root.join("src").join("main.txt");
    bookmark(cx, window, &main, 0);
    open_bookmarks(cx, window);

    let index = row_index(cx, &main, 0);
    click_row(cx, window, index);
    press(cx, window, "delete");
    draw(cx, window);

    assert!(
        cx.read(|cx| editor_view::bookmarks(cx).is_empty()),
        "the window list lost the bookmark"
    );
    let view = editor_of(cx, window, &main);
    assert_eq!(
        view.read_with(cx, |view, _| view.bookmark_at_line(0)),
        None,
        "the gutter has no mark on the line"
    );
    assert!(
        !rows(cx)
            .iter()
            .any(|row| matches!(row, Row::Bookmark { .. })),
        "the tree has no bookmark row left"
    );
}

#[ui_kit::gpui::test]
fn an_edit_moves_the_line_and_the_window_follows(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "bookmarks-live");
    let main = root.join("src").join("main.txt");
    bookmark(cx, window, &main, 1);
    open_bookmarks(cx, window);

    let index = row_index(cx, &main, 1);
    let label = panel(cx)
        .update(cx, |panel, cx| panel.row_label(index, cx))
        .expect("the bookmark row has a label");
    assert!(
        label.contains("2: beta"),
        "the tree shows the line: {label}"
    );

    // An empty line above moves the mark down; the window follows the text.
    let view = editor_of(cx, window, &main);
    window
        .update(cx, |_, window, cx| {
            window.focus(&view.read(cx).focus_handle(cx), cx);
        })
        .unwrap();
    view.update(cx, |view, cx| view.restore_caret(0, 5, cx));
    press(cx, window, "enter");
    draw(cx, window);

    assert_eq!(
        groups(cx)[0].bookmarks[0].line,
        2,
        "the bookmark moved with its line"
    );
    let index = row_index(cx, &main, 2);
    let label = panel(cx)
        .update(cx, |panel, cx| panel.row_label(index, cx))
        .expect("the moved bookmark row has a label");
    assert!(
        label.contains("3: beta"),
        "the tree shows the new number and text: {label}"
    );
}

#[ui_kit::gpui::test]
fn typed_letters_pick_the_bookmark_that_starts_with_them(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "bookmarks-search");
    let main = root.join("src").join("main.txt");
    bookmark(cx, window, &main, 1);
    open_bookmarks(cx, window);

    type_text(cx, window, "beta");
    let index = row_index(cx, &main, 1);
    assert_eq!(
        panel(cx).read_with(cx, |panel, _| panel.selected()),
        Some(index),
        "the speed search picked the bookmark of the beta line"
    );
}

#[ui_kit::gpui::test]
fn a_group_and_a_description_come_back_after_a_restart(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "bookmarks-groups");
    let main = root.join("src").join("main.txt");
    bookmark(cx, window, &main, 0);
    open_bookmarks(cx, window);

    let index = row_index(cx, &main, 0);
    click_row(cx, window, index);
    press(cx, window, "f2");
    draw(cx, window);
    assert!(
        bounds_named(cx, window, "prompt").size.height > px(0.),
        "F2 asks for the description"
    );
    type_text(cx, window, "Worth a look");
    press(cx, window, "enter");
    draw(cx, window);
    assert_eq!(
        groups(cx)[0].bookmarks[0].description.as_deref(),
        Some("Worth a look")
    );

    // The menu of the bookmark makes a group, and the bookmark moves to it.
    right_click_row(cx, window, row_index(cx, &main, 0));
    draw(cx, window);
    click(cx, window, "menu-3");
    draw(cx, window);
    assert!(
        bounds_named(cx, window, "prompt").size.height > px(0.),
        "New Group asks for the name"
    );
    type_text(cx, window, "Reading");
    press(cx, window, "enter");
    draw(cx, window);
    assert_eq!(groups(cx).len(), 2);
    assert_eq!(groups(cx)[1].name, "Reading");

    right_click_row(cx, window, row_index(cx, &main, 0));
    draw(cx, window);
    click(cx, window, "menu-3");
    draw(cx, window);
    click(cx, window, "menu-3-0");
    draw(cx, window);
    let saved = groups(cx);
    assert!(
        saved[0].bookmarks.is_empty(),
        "the bookmark left the project group"
    );
    assert_eq!(saved[1].bookmarks[0].line, 0);
    assert_eq!(
        saved[1].bookmarks[0].description.as_deref(),
        Some("Worth a look")
    );

    let state_dir = window
        .read_with(cx, |workspace, _| workspace.state_dir().to_path_buf())
        .unwrap();
    let store = window
        .read_with(cx, |workspace, _| workspace.store())
        .unwrap()
        .expect("the test window has a store");
    window
        .update(cx, |workspace, window, cx| {
            workspace.close_window(window, cx)
        })
        .unwrap();
    cx.run_until_parked();

    let layout = load_layout(&store, &root);
    let second = open_saved_window(cx, &root, &state_dir, layout, Some(store));
    draw(cx, second);
    let saved = groups(cx);
    assert_eq!(saved.len(), 2, "both groups are back");
    assert_eq!(saved[1].name, "Reading");
    assert_eq!(
        saved[1].bookmarks[0].description.as_deref(),
        Some("Worth a look"),
        "the description came back with the group"
    );
    open_bookmarks(cx, second);
    let index = row_index(cx, &main, 0);
    assert!(
        bounds_named(cx, second, ROW_SELECTORS[index]).size.height > px(0.),
        "the bookmark is drawn under its group"
    );
}

#[test]
fn a_layout_without_groups_opens_in_the_default_group() {
    let saved = r#"{"bookmarks":[
        {"path":"C:/project/src/A.txt","line":2,"mnemonic":"M","description":"note"},
        {"path":"C:/project/src/B.txt","line":5,"mnemonic":null,"description":null}
    ]}"#;
    let layout: Layout = serde_json::from_str(saved).expect("the old layout reads");
    let (groups, default) = crate::bookmarks::groups_from_layout(&layout.bookmarks, "project");
    assert_eq!(default, 0);
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].name, "project");
    assert_eq!(groups[0].bookmarks.len(), 2);
    assert_eq!(groups[0].bookmarks[0].line, 2);
    assert_eq!(groups[0].bookmarks[0].mnemonic, Some('M'));
    assert_eq!(groups[0].bookmarks[0].description.as_deref(), Some("note"));
    assert_eq!(groups[0].bookmarks[1].line, 5);
}

#[test]
fn the_map_of_the_old_store_opens_in_the_default_group() {
    let saved = r#"{"bookmarks":{
        "C:/project/src/A.txt":[[2,"M"],[7,null]],
        "C:/project/src/B.txt":[[5,"B"]]
    }}"#;
    let layout: Layout = serde_json::from_str(saved).expect("the old store reads");
    let (groups, default) = crate::bookmarks::groups_from_layout(&layout.bookmarks, "project");
    assert_eq!(default, 0);
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].name, "project");
    assert_eq!(groups[0].bookmarks.len(), 3);
    assert!(
        groups[0]
            .bookmarks
            .iter()
            .all(|bookmark| bookmark.description.is_none()),
        "the old store had no descriptions"
    );
    assert!(groups[0].bookmarks.iter().any(|bookmark| {
        bookmark.path.ends_with("A.txt") && bookmark.line == 2 && bookmark.mnemonic == Some('M')
    }));
}
