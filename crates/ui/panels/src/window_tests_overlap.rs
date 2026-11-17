//! T-0200: no two controls of a tool window or of a dialog lie on each other.
//!
//! GPUI keeps the bounds of the `debug_selector` elements of a frame to itself: a test
//! asks about one name and gets one box. The button, tab, row and menu builders of
//! `ui-kit` record the boxes they draw, and the names of the constant selectors come
//! from the sources of the UI crates, so a new panel is picked up without a list here.
//! The frame then answers where each of them was.
//!
//! A pair is a bug when the boxes cross by more than a pixel and neither box contains
//! the other: a row inside a list, or a label inside a button, is not a crossing. The
//! label of a control with a text label must also fit into the box of the control.
//!
//! A text that a panel puts into a fixed box by hand has no box of its own in a frame,
//! so only the labels that a builder draws are measured: a label added to a square
//! icon button behind the back of `button_frame` is past these checks.
//!
//! The states that need a language server, Docker, a build tool or a debugger are not
//! here: an empty window of the panel is what these checks draw. The real sessions live
//! in the ignored tests of the other files.

#![cfg(windows)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use editor_view::Languages;
use ui_kit::gpui;
use ui_kit::gpui::{Bounds, Pixels, TestAppContext, VisualTestContext, WindowHandle, size};
use ui_kit::metrics::{RIGHT_STRIPE_WIDTH, STATUS_HEIGHT, STRIPE_WIDTH, TOOLBAR_HEIGHT};
use ui_kit::{Theme, motion, point, px};
use workspace::{DockSide, FileIcons, Launch, Layout, Settings, Workspace};

use crate::window_tests::{draw, press, project_setup, sample_project, scratch, type_text};
use crate::window_tests_git::{
    conflicting_repository, git_state, repository_with_origin, wait_for_repository,
};
use crate::window_tests_ui::{double_shift, wait_for};
use crate::{AgentOptions, LanguageServer, SearchSetup, connect, install};

/// The width of the window the checks draw in.
const WIDE: f32 = 1280.;
/// The height of a wide window.
const TALL: f32 = 800.;
/// A laptop window, where a dialog has less room.
const NARROW: f32 = 760.;
const LOW: f32 = 620.;

/// One box a test named, as the last frame drew it.
struct Element {
    name: String,
    bounds: Bounds<Pixels>,
    area: Area,
    /// The label the control draws, for a control with a visible text label.
    label: Option<String>,
    label_width: Pixels,
}

/// The part of the window a box belongs to, by the middle of the box.
///
/// Two parts never lie on each other: a tool window over the status bar is a layout
/// bug, but a box that its part clips reports its whole size in a test, so a check of
/// one part compares only the boxes of that part. A dialog covers the parts under it,
/// and its check compares the controls that the frame did not have before it opened.
#[derive(Clone, Copy, PartialEq)]
enum Area {
    Toolbar,
    Status,
    Stripe,
    Left,
    Right,
    Bottom,
    Editor,
}

/// The names of the constant selectors of the UI crates, oldest first.
///
/// A row named by a number is not in this list: its builder in `ui-kit` records the box
/// and the name as it draws. A selector that a panel builds with `format!` outside
/// `ui-kit` is out of reach of both, and a check of the panel does not see it.
fn selector_names() -> &'static [&'static str] {
    static NAMES: OnceLock<Vec<&'static str>> = OnceLock::new();
    NAMES.get_or_init(|| {
        let ui = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("the panels crate lives under crates/ui")
            .to_path_buf();
        let mut names = Vec::new();
        for crate_dir in [
            "panels/src",
            "workspace/src",
            "ui-kit/src",
            "editor-view/src",
        ] {
            let mut stack = vec![ui.join(crate_dir)];
            while let Some(dir) = stack.pop() {
                let Ok(entries) = fs::read_dir(dir) else {
                    continue;
                };
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        stack.push(path);
                    } else if path.extension().is_some_and(|ext| ext == "rs") {
                        names.extend(literals(&fs::read_to_string(path).unwrap_or_default()));
                    }
                }
            }
        }
        names.sort();
        names.dedup();
        names
            .into_iter()
            .map(|name| &*Box::leak(name.into_boxed_str()))
            .collect()
    })
}

/// The literal names of the `debug_selector` calls of one source file.
fn literals(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut found = Vec::new();
    let mut at = 0;
    while let Some(offset) = text[at..].find("debug_selector") {
        let mut index = at + offset + "debug_selector".len();
        at = index;
        let skip_space = |index: &mut usize| {
            while *index < bytes.len() && bytes[*index].is_ascii_whitespace() {
                *index += 1;
            }
        };
        skip_space(&mut index);
        if bytes.get(index) == Some(&b'(') {
            index += 1;
        }
        skip_space(&mut index);
        if text[index..].starts_with("move") {
            index += "move".len();
        }
        skip_space(&mut index);
        if !text[index..].starts_with("||") {
            continue;
        }
        index += 2;
        skip_space(&mut index);
        if bytes.get(index) != Some(&b'"') {
            continue;
        }
        index += 1;
        let mut literal = String::new();
        while index < bytes.len() && bytes[index] != b'"' {
            if bytes[index] == b'\\' && index + 1 < bytes.len() {
                literal.push(bytes[index + 1] as char);
                index += 2;
                continue;
            }
            literal.push(bytes[index] as char);
            index += 1;
        }
        if !literal.is_empty() {
            found.push(literal);
        }
    }
    found
}

/// The id of every tool window the window knows, from the constants of `workspace`.
///
/// A constant is found before the panel of a parallel task is merged: the check opens
/// what the window has and moves on, so a new tool window needs no line here. A tool
/// the window does not have stays closed, and the check covers what is on screen.
fn tool_ids() -> &'static [&'static str] {
    static IDS: OnceLock<Vec<&'static str>> = OnceLock::new();
    IDS.get_or_init(|| {
        let file = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("the panels crate lives under crates/ui")
            .join("workspace/src/window.rs");
        let text = fs::read_to_string(file).unwrap_or_default();
        let mut ids = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            let Some(rest) = line.strip_prefix("pub const ") else {
                continue;
            };
            let Some((name, value)) = rest.split_once(": &str = \"") else {
                continue;
            };
            if !name.ends_with("_TOOL") {
                continue;
            }
            let Some(value) = value.split('"').next() else {
                continue;
            };
            ids.push(&*Box::leak(value.to_owned().into_boxed_str()));
        }
        ids.sort_unstable();
        ids.dedup();
        ids
    })
}

/// Opens a window of `WIDE` by `height` with the docks of `layout` on `root`.
fn open_window_at(
    cx: &mut TestAppContext,
    name: &str,
    root: PathBuf,
    layout: Layout,
    height: f32,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let state_dir = scratch(name);
    let store = store::Store::open(&state_dir.join("state.sqlite"))
        .ok()
        .map(Arc::new);
    cx.update(|cx| {
        cx.set_global(Theme::dark());
        cx.set_global(Settings::default());
        cx.set_global(Languages {
            resolve: |path| dockerfile_syntax::is_dockerfile(path).then(dockerfile_syntax::config),
            templates: |_| None,
        });
        workspace::bind_keys(cx);
    });
    let tree = layout.left.open;
    let launch = Launch {
        root,
        open: None,
        state_dir: state_dir.clone(),
        settings_dir: state_dir,
        icons: FileIcons {
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
    let window = cx.add_window(move |window, cx| {
        let mut workspace = Workspace::new_for_test(launch, layout, store, Vec::new(), window, cx);
        install(
            &mut workspace,
            SearchSetup::default(),
            project_setup(scratch(name)),
            agents,
            window,
            cx,
        );
        connect(&mut workspace, server, cx);
        workspace
    });
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.update(|window, cx| {
        window.resize(size(px(WIDE), px(height)));
        window.bounds_changed(cx);
    });
    cx.run_until_parked();
    // The tree of the project arrives from the index thread: its rows are the controls
    // a check of the Project window is about.
    if tree {
        crate::window_tests::wait_for_project_tree(cx);
    }
    window
}

/// Draws the window, so that the table of the recorded controls holds this frame only.
fn redraw(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    ui_kit::clear_test_bounds();
    draw(cx, window);
}

/// Resizes the window and draws the frame it settles in.
fn resize(cx: &mut TestAppContext, window: WindowHandle<Workspace>, width: f32, height: f32) {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.update(|window, cx| {
        window.resize(size(px(width), px(height)));
        window.bounds_changed(cx);
    });
    cx.run_until_parked();
    redraw(cx, window);
}

/// The boxes of the frame with the given selector, for the names that are on screen.
///
/// A tool window of a closed dock keeps drawing outside the window, and a row of a
/// long list is cut by the edge of its part: neither is on screen as a whole, and the
/// boxes a check can compare are the ones fully inside the window.
fn elements(cx: &mut TestAppContext, window: WindowHandle<Workspace>) -> Vec<Element> {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    let viewport = visual.update(|window, _| window.viewport_size());
    let sides = window
        .read_with(cx, |workspace, _| {
            [DockSide::Left, DockSide::Right, DockSide::Bottom].map(|side| {
                if workspace.dock_open(side) {
                    workspace.dock_size(side)
                } else {
                    0.
                }
            })
        })
        .unwrap();
    let screen = Bounds::new(point(px(0.), px(0.)), viewport);
    let area_of = |bounds: Bounds<Pixels>| area(bounds, viewport, sides);
    let shown = |bounds: Bounds<Pixels>| {
        bounds.size.width > px(0.)
            && bounds.size.height > px(0.)
            && bounds.left() >= screen.left() - px(0.5)
            && bounds.right() <= screen.right() + px(0.5)
            && bounds.top() >= screen.top() - px(0.5)
            && bounds.bottom() <= screen.bottom() + px(0.5)
    };
    let mut elements: Vec<Element> = Vec::new();
    for (name, record) in ui_kit::test_bounds() {
        if !shown(record.bounds) {
            continue;
        }
        elements.push(Element {
            name,
            bounds: record.bounds,
            area: area_of(record.bounds),
            label: record.label.map(|label| label.to_string()),
            label_width: record.label_width,
        });
    }
    for name in selector_names() {
        if elements.iter().any(|element| element.name == *name) {
            continue;
        }
        let Some(bounds) = visual.debug_bounds(name) else {
            continue;
        };
        if !shown(bounds) {
            continue;
        }
        elements.push(Element {
            name: (*name).to_owned(),
            bounds,
            area: area_of(bounds),
            label: None,
            label_width: px(0.),
        });
    }
    elements
}

/// The part of the window the middle of `bounds` lies in.
fn area(
    bounds: Bounds<Pixels>,
    viewport: gpui::Size<Pixels>,
    [left, right, bottom]: [f32; 3],
) -> Area {
    let middle = bounds.center();
    let (x, y) = (f32::from(middle.x), f32::from(middle.y));
    let (width, height) = (f32::from(viewport.width), f32::from(viewport.height));
    if y < TOOLBAR_HEIGHT {
        return Area::Toolbar;
    }
    if y >= height - STATUS_HEIGHT {
        return Area::Status;
    }
    if x < STRIPE_WIDTH {
        return Area::Stripe;
    }
    if x < STRIPE_WIDTH + left {
        return Area::Left;
    }
    if x >= width - RIGHT_STRIPE_WIDTH - right {
        return Area::Right;
    }
    if y >= height - STATUS_HEIGHT - bottom {
        return Area::Bottom;
    }
    Area::Editor
}

/// Whether the boxes cross by more than one pixel and neither contains the other.
fn crossing(first: &Bounds<Pixels>, second: &Bounds<Pixels>) -> bool {
    let crossing = first.intersect(second);
    if crossing.size.width <= px(1.) || crossing.size.height <= px(1.) {
        return false;
    }
    let inside = |inner: &Bounds<Pixels>, outer: &Bounds<Pixels>| {
        inner.left() >= outer.left() - px(1.)
            && inner.right() <= outer.right() + px(1.)
            && inner.top() >= outer.top() - px(1.)
            && inner.bottom() <= outer.bottom() + px(1.)
    };
    !inside(first, second) && !inside(second, first)
}

/// The crossings of the boxes on screen and the labels that do not fit, as lines of a
/// failure.
fn failures(elements: &[Element], by_area: bool) -> Vec<String> {
    let mut found = Vec::new();
    for (index, element) in elements.iter().enumerate() {
        for other in &elements[index + 1..] {
            if (!by_area || element.area == other.area) && crossing(&element.bounds, &other.bounds)
            {
                found.push(format!(
                    "{} {:?} crosses {} {:?}",
                    element.name, element.bounds, other.name, other.bounds
                ));
            }
        }
    }
    for element in elements {
        let Some(label) = &element.label else {
            continue;
        };
        if element.label_width > element.bounds.size.width + px(0.5) {
            found.push(format!(
                "the label {label:?} of {} takes {:?} in a box of {:?}",
                element.name, element.label_width, element.bounds.size.width
            ));
        }
    }
    found
}

/// Fails with every crossing the frame of the window holds.
fn assert_clear(cx: &mut TestAppContext, window: WindowHandle<Workspace>, what: &str) {
    let found = failures(&elements(cx, window), true);
    assert!(found.is_empty(), "{what}:\n{}", found.join("\n"));
}

/// Shows `tool` in the window and draws it.
fn show_tool(cx: &mut TestAppContext, window: WindowHandle<Workspace>, tool: &str) {
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(tool, window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    // A dock slides in with the real clock of the motion: the boxes are read from the
    // frame it settles in, where the panel lies in its dock and not under the stripe.
    std::thread::sleep(Duration::from_millis(motion::PANEL + 20));
    cx.run_until_parked();
    redraw(cx, window);
}

/// Shows every tool window of a window of the docks of `layout`.
///
/// The project is a repository with one changed file and one open tab, so the windows of
/// Git, of the tree and of the tabs have rows and not only an empty state. A local
/// repository is enough: these windows need no server.
fn check_tools(cx: &mut TestAppContext, name: &str, layout: Layout) {
    let (root, _bare, _git) = repository_with_origin(name);
    let path = root.join("main.txt");
    fs::write(&path, "alpha\nbeta\n").unwrap();
    let window = open_window_at(cx, name, root, layout, TALL);
    let git = git_state(cx);
    wait_for_repository(cx, &git);
    git.update(cx, |git, cx| git.refresh(cx));
    crate::window_tests::open_path(cx, window, &path);
    redraw(cx, window);
    for tool in tool_ids() {
        show_tool(cx, window, tool);
        // A constant of a window that this build does not register opens nothing: the
        // panel of a parallel task is checked on the run after its merge.
        let open = window
            .read_with(cx, |workspace, _| workspace.tool_open(tool))
            .unwrap();
        if !open {
            continue;
        }
        assert_clear(cx, window, &format!("{tool} at {WIDE}×{TALL}"));
    }
}

#[ui_kit::gpui::test]
fn tool_windows_have_no_crossing_controls(cx: &mut TestAppContext) {
    check_tools(cx, "overlap-tools", Layout::default());
}

#[ui_kit::gpui::test]
fn narrow_tool_windows_have_no_crossing_controls(cx: &mut TestAppContext) {
    let mut layout = Layout::default();
    layout.left.size = 360.;
    layout.right.size = 360.;
    layout.bottom.size = 200.;
    check_tools(cx, "overlap-tools-narrow", layout);
}

/// The names of the controls a frame shows.
fn names(elements: &[Element]) -> std::collections::HashSet<String> {
    elements
        .iter()
        .map(|element| element.name.clone())
        .collect()
}

/// Fails with every crossing among the controls that `open` adds to the window, at the
/// wide window and at a laptop one.
///
/// The controls a frame showed before the dialog are the parts behind it: a dialog lies
/// over the dock and the stripe by design, and a pair of a control of the dialog and a
/// control of a part behind it says nothing about the layout of the dialog.
fn dialog_background(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
) -> std::collections::HashSet<String> {
    redraw(cx, window);
    names(&elements(cx, window))
}

/// Fails with every crossing of the controls of the dialog that is now open.
fn check_dialog(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    background: &std::collections::HashSet<String>,
    what: &str,
) {
    redraw(cx, window);
    assert_dialog_clear(cx, window, background, &format!("{what} at {WIDE}×{TALL}"));
    resize(cx, window, NARROW, LOW);
    assert_dialog_clear(cx, window, background, &format!("{what} at {NARROW}×{LOW}"));
}

/// Fails with every crossing of the controls that are not in `background`.
fn assert_dialog_clear(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    background: &std::collections::HashSet<String>,
    what: &str,
) {
    let foreground: Vec<Element> = elements(cx, window)
        .into_iter()
        .filter(|element| !background.contains(&element.name))
        .collect();
    let found = failures(&foreground, false);
    assert!(
        found.is_empty(),
        "{what}:
{}",
        found.join(
            "
"
        )
    );
}

#[ui_kit::gpui::test]
fn settings_and_find_in_files_have_no_crossing_controls(cx: &mut TestAppContext) {
    let window = open_window_at(
        cx,
        "overlap-dialogs-search",
        sample_project("overlap-dialogs-search"),
        Layout::default(),
        TALL,
    );
    let background = dialog_background(cx, window);
    press(cx, window, "ctrl-shift-f");
    check_dialog(cx, window, &background, "Find in Files");
    press(cx, window, "escape");
    press(cx, window, "ctrl-alt-s");
    check_dialog(cx, window, &background, "Settings");
}

#[ui_kit::gpui::test]
fn search_everywhere_has_no_crossing_controls(cx: &mut TestAppContext) {
    let window = open_window_at(
        cx,
        "overlap-dialogs-everywhere",
        sample_project("overlap-dialogs-everywhere"),
        Layout::default(),
        TALL,
    );
    let background = dialog_background(cx, window);
    double_shift(cx, window);
    type_text(cx, window, "main");
    check_dialog(cx, window, &background, "Search Everywhere");
}

#[ui_kit::gpui::test]
fn change_signature_has_no_crossing_controls(cx: &mut TestAppContext) {
    let window = open_window_at(
        cx,
        "overlap-dialogs-signature",
        sample_project("overlap-dialogs-signature"),
        Layout::default(),
        TALL,
    );
    let spot = lang_api::DocumentSpot {
        uri: "file:///signature".to_owned(),
        version: 0,
        point: lang_api::TextPoint {
            line: 0,
            character: 0,
        },
    };
    let background = dialog_background(cx, window);
    window
        .update(cx, |workspace, window, cx| {
            crate::change_signature::ChangeSignatureDialog::show(spot, workspace, window, cx);
        })
        .unwrap();
    check_dialog(cx, window, &background, "Change Signature");
}

#[ui_kit::gpui::test]
fn rename_has_no_crossing_controls(cx: &mut TestAppContext) {
    use crate::window_tests_lang::{FakeLanguage, open_window_with};

    let root = scratch("overlap-dialogs-rename");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("src/Open.txt"),
        "alpha beta
",
    )
    .unwrap();
    let window = open_window_with(cx, root.clone(), FakeLanguage::new());
    cx.run_until_parked();
    draw(cx, window);
    let path = root.join("src").join("Open.txt");
    crate::window_tests::open_path(cx, window, &path);
    let background = dialog_background(cx, window);
    press(cx, window, "shift-f6");
    check_dialog(cx, window, &background, "Rename");
}

#[ui_kit::gpui::test]
fn view_breakpoints_has_no_crossing_controls(cx: &mut TestAppContext) {
    let root = sample_project("overlap-dialogs-breakpoints");
    let window = open_window_at(
        cx,
        "overlap-dialogs-breakpoints",
        root.clone(),
        Layout::default(),
        TALL,
    );
    let path = root.join("src").join("main.txt");
    crate::window_tests::open_path(cx, window, &path);
    crate::window_tests::click_gutter(cx, window, 0, 1);
    let view = crate::window_tests::editor_of(cx, window, &path);
    view.update(cx, |view, cx| view.restore_caret(3, 0, cx));
    draw(cx, window);
    let background = dialog_background(cx, window);
    press(cx, window, "ctrl-shift-f8");
    wait_for(cx, "the breakpoints dialog", |cx| {
        has_selector(cx, window, "breakpoints-dialog")
    });
    check_dialog(cx, window, &background, "View Breakpoints");
}

#[ui_kit::gpui::test]
fn the_branches_popup_has_no_crossing_controls(cx: &mut TestAppContext) {
    let (root, _bare, _git) = repository_with_origin("overlap-dialogs-branches");
    let window = open_window_at(
        cx,
        "overlap-dialogs-branches",
        root,
        Layout::default(),
        TALL,
    );
    let git = git_state(cx);
    wait_for_repository(cx, &git);
    let background = dialog_background(cx, window);
    git.update(cx, |git, cx| {
        git.show_branches(point(px(200.), px(200.)), cx)
    });
    wait_for(cx, "the branch menu", |cx| {
        has_selector(cx, window, "menu-2")
    });
    check_dialog(cx, window, &background, "Branches");
}

#[ui_kit::gpui::test]
fn the_conflicts_window_has_no_crossing_controls(cx: &mut TestAppContext) {
    let (root, _bare, _git) = conflicting_repository("overlap-dialogs-conflicts");
    let window = open_window_at(
        cx,
        "overlap-dialogs-conflicts",
        root,
        Layout::default(),
        TALL,
    );
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    let background = dialog_background(cx, window);
    press(cx, window, "ctrl-t");
    wait_for(cx, "the update choice", |cx| {
        has_selector(cx, window, "menu-0")
    });
    crate::window_tests::click(cx, window, "menu-0");
    wait_for(cx, "the conflicts window", |cx| {
        has_selector(cx, window, "conflict-row-0")
    });
    check_dialog(cx, window, &background, "Conflicts");
}

#[ui_kit::gpui::test]
fn the_rebase_dialog_has_no_crossing_controls(cx: &mut TestAppContext) {
    let (root, _bare, git) = repository_with_origin("overlap-dialogs-rebase");
    for (name, message) in [("one.txt", "one"), ("two.txt", "two")] {
        fs::write(
            root.join(name),
            format!(
                "{message}
"
            ),
        )
        .unwrap();
        git.run(&root, ["add", name]).unwrap();
        git.run(&root, ["commit", "-m", message]).unwrap();
    }
    let window = open_window_at(cx, "overlap-dialogs-rebase", root, Layout::default(), TALL);
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    press(cx, window, "alt-9");
    wait_for(cx, "the log", |cx| has_selector(cx, window, "log-commit-0"));
    // The menu and the dialog read the log a moment after it opens.
    std::thread::sleep(Duration::from_millis(300));
    cx.run_until_parked();
    redraw(cx, window);
    let background = dialog_background(cx, window);
    right_click(cx, window, "log-commit-1");
    wait_for(cx, "the commit menu", |cx| {
        has_selector(cx, window, "menu-0")
    });
    crate::window_tests::click(cx, window, "menu-9");
    wait_for(cx, "the rebase dialog", |cx| {
        has_selector(cx, window, "rebase-row-1")
    });
    check_dialog(cx, window, &background, "Rebase");
}

/// The right button of the mouse on the middle of the element with a selector.
fn right_click(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) {
    let position = crate::window_tests::bounds_of(cx, window, selector).center();
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_down(
        position,
        gpui::MouseButton::Right,
        gpui::Modifiers::default(),
    );
    visual.simulate_mouse_up(
        position,
        gpui::MouseButton::Right,
        gpui::Modifiers::default(),
    );
    cx.run_until_parked();
    redraw(cx, window);
}

/// Whether the element with a selector was drawn.
fn has_selector(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &'static str,
) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear(cx);
    });
    visual.debug_bounds(selector).is_some()
}

#[test]
fn the_sources_name_their_selectors() {
    let names = selector_names();
    assert!(names.len() > 100, "only {} names", names.len());
    assert!(names.contains(&"debug-toolbar"));
    assert!(names.contains(&"docker-title"));
    assert!(tool_ids().contains(&"debug"));
    assert!(tool_ids().contains(&"services"));
}

#[test]
fn a_pair_of_boxes_crosses_only_outside_each_other() {
    let box_at = |x: f32, y: f32, width: f32, height: f32| {
        Bounds::new(point(px(x), px(y)), size(px(width), px(height)))
    };
    assert!(crossing(
        &box_at(0., 0., 24., 24.),
        &box_at(12., 0., 24., 24.)
    ));
    assert!(!crossing(
        &box_at(0., 0., 24., 24.),
        &box_at(24., 0., 24., 24.)
    ));
    assert!(!crossing(
        &box_at(0., 0., 24., 24.),
        &box_at(0.5, 0., 12., 12.)
    ));
    assert!(!crossing(
        &box_at(0., 0., 24., 24.),
        &box_at(23., 0., 24., 24.)
    ));
}
