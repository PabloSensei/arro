//! The window without a real window: the test platform of GPUI opens a workspace, puts
//! the tool windows in its docks and drives the tab the way the keyboard does.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use editor_view::{EditorView, Languages, Usage};
use keymap::{editor as editor_keys, workspace as keys};
use lang_api::{Severity, SourceSpan, TextPoint, TextSpan};
use platform::CanonPath;
use ui_kit::gpui;
use ui_kit::gpui::{
    AppContext, AsKeystroke, KeyBindingContextPredicate, OperatingSystem, TestAppContext,
    VisualTestContext, WindowHandle,
};
use ui_kit::prelude::*;
use ui_kit::{Modifiers, Theme, point, px};
use workspace::{
    COMMIT_TOOL, DEBUG_TOOL, DockSide, Launch, Layout, PROBLEMS_TOOL, PROJECT_TOOL, RUN_TOOL,
    SERVICES_TOOL, SETTINGS_FILE, STRUCTURE_TOOL, Settings, TERMINAL_TOOL, TODO_TOOL, USAGES_TOOL,
    Workspace,
};

use crate::debug::ActiveDebug;
use crate::git::ActiveGit;
use crate::index::SearchSetup;
use crate::problems::{ActiveProblems, Problem};
use crate::project::ActiveProjectPanel;
use crate::run::ProjectSetup;
use crate::start::{ProjectTemplate, StartScreen};
use crate::usages::ActiveUsages;
use crate::{AgentOptions, LanguageServer, connect, install, tree_ops::FileTemplate};

/// A folder of its own for one part of one test, removed when the test ends. The project
/// source of the test lives here too, so a run without `IDE_CORPUS` works the same.
pub(crate) fn scratch(name: &str) -> PathBuf {
    test_support::scratch(&format!("window-{name}"))
}

pub(crate) fn sample_project(name: &str) -> PathBuf {
    let root = scratch(name);
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("README.txt"), "A sample project.\n").unwrap();
    fs::write(root.join("src/main.txt"), "alpha\nbeta\n").unwrap();
    fs::write(root.join("src/notes.txt"), "TODO: read me\n").unwrap();
    fs::write(root.join("Dockerfile"), "FROM scratch\n").unwrap();
    root
}

pub(crate) fn project_setup(cache_root: PathBuf) -> ProjectSetup {
    ProjectSetup {
        tool: None,
        providers: Vec::new(),
        tests: None,
        test_sites: |_, _| Vec::new(),
        cache_root,
        templates: Vec::<FileTemplate>::new(),
        skipped_folders: Vec::new(),
        environment: None,
        debug: None,
    }
}

/// The same order as `panels::run`: the window is built, `install` fills it, `connect`
/// waits for the status of the language server. There is no server in the test.
pub(crate) fn open_window(cx: &mut TestAppContext, root: PathBuf) -> WindowHandle<Workspace> {
    open_window_with(cx, root, project_setup(scratch("cache")))
}

/// The same window with a setup of the caller: a build tool, run configurations, tests or
/// a debug service that do not need the real project.
pub(crate) fn open_window_with(
    cx: &mut TestAppContext,
    root: PathBuf,
    setup: ProjectSetup,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let state_dir = scratch("state");
    let store = store::Store::open(&state_dir.join("state.sqlite"))
        .ok()
        .map(Arc::new);
    cx.update(|cx| {
        cx.set_global(Theme::dark());
        cx.set_global(Settings::default());
        cx.set_global(Languages {
            // A grammar without a server: the editor keys of a Dockerfile need comments.
            resolve: |path| dockerfile_syntax::is_dockerfile(path).then(dockerfile_syntax::config),
            templates: |_| None,
        });
        // The application binds the keys of the whole window before the window opens.
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
    cx.add_window(move |window, cx| {
        let mut workspace =
            Workspace::new_for_test(launch, Layout::default(), store, Vec::new(), window, cx);
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

/// The guard against a hang while a test waits for a signal of the window's own
/// threads. The signal ends the wait, not this clock, so it is far longer than a wait
/// for a frame or for a real program would be.
const SIGNAL_LIMIT: Duration = Duration::from_secs(120);
/// One turn of the simulated clock while a signal is awaited.
const SIGNAL_TICK: Duration = Duration::from_millis(50);

/// Runs `check` until it holds, letting the real threads of the window work. The
/// condition reads a counter or a flag that the work itself raises.
pub(crate) fn wait_for_signal(
    cx: &mut TestAppContext,
    what: &str,
    mut check: impl FnMut(&mut TestAppContext) -> bool,
) {
    let started = std::time::Instant::now();
    loop {
        if check(cx) {
            return;
        }
        assert!(
            started.elapsed() < SIGNAL_LIMIT,
            "the test waited for {what} in vain"
        );
        std::thread::sleep(Duration::from_millis(10));
        cx.executor().advance_clock(SIGNAL_TICK);
        cx.run_until_parked();
    }
}

/// Waits until the symbols of the project are in the index.
pub(crate) fn wait_for_project_index(cx: &mut TestAppContext) {
    wait_for_signal(cx, "the project index", |cx| {
        cx.read(|cx| {
            cx.try_global::<crate::index::ActiveIndex>()
                .is_some_and(|index| index.0.symbols_generation() > 0)
        })
    });
}

/// Waits until the list of files of the project is in the index.
pub(crate) fn wait_for_file_index(cx: &mut TestAppContext) {
    wait_for_signal(cx, "the file index", |cx| {
        cx.read(|cx| {
            cx.try_global::<crate::index::ActiveIndex>()
                .is_some_and(|index| index.0.files_generation() > 0)
        })
    });
}

/// Waits until at least one folder listing has reached the project tree.
pub(crate) fn wait_for_project_tree(cx: &mut TestAppContext) {
    wait_for_signal(cx, "the project tree", |cx| {
        cx.read(|cx| {
            cx.try_global::<ActiveProjectPanel>()
                .is_some_and(|panel| panel.0.read(cx).listing_generation() > 0)
        })
    });
}

/// Waits until the language has answered about the declarations of the file.
pub(crate) fn wait_for_outline_signal(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    path: &Path,
) {
    let view = editor_of(cx, window, path);
    wait_for_signal(cx, "the outline", |cx| {
        view.read_with(cx, |view, _| view.outline_generation() > 0)
    });
}

/// Waits until the parser has handed the first syntax tree of the file to the view.
pub(crate) fn wait_for_parse_signal(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    path: &Path,
) {
    let view = editor_of(cx, window, path);
    wait_for_signal(cx, "the parse", |cx| {
        view.read_with(cx, |view, _| view.syntax_generation() > 0)
    });
}

/// Waits for a step of a real program or test process. Only this wait needs a limit of
/// its own: the signal it looks for comes from outside the window.
pub(crate) fn wait_for_process(
    cx: &mut TestAppContext,
    what: &str,
    check: impl FnMut(&mut TestAppContext) -> bool,
) {
    wait_for_signal(cx, what, check);
}

/// Draws one frame, so that the render of the workspace and of the open tool window runs.
pub(crate) fn draw<T: Render>(cx: &mut TestAppContext, window: WindowHandle<T>) {
    cx.update_window(window.into(), |_, window, cx| {
        window.refresh();
        window.draw(cx).clear(cx);
    })
    .unwrap();
}

/// Opens a file the way a click in the project tree does, and lets the window settle.
pub(crate) fn open_path(cx: &mut TestAppContext, window: WindowHandle<Workspace>, path: &Path) {
    window
        .update(cx, |workspace, window, cx| {
            workspace.open_file(path.to_path_buf(), window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
}

pub(crate) fn editor_of(
    cx: &TestAppContext,
    window: WindowHandle<Workspace>,
    path: &Path,
) -> gpui::Entity<EditorView> {
    window
        .read_with(cx, |workspace, _| workspace.view_of(path))
        .unwrap()
        .and_then(|view| view.downcast::<EditorView>().ok())
        .expect("the opened tab has an editor")
}

pub(crate) fn buffer_of(cx: &TestAppContext, view: &gpui::Entity<EditorView>) -> String {
    view.read_with(cx, |view, _| view.text())
        .expect("the file is read")
}

pub(crate) fn press(cx: &mut TestAppContext, window: WindowHandle<Workspace>, keys: &str) {
    cx.simulate_keystrokes(window.into(), keys);
    cx.run_until_parked();
}

pub(crate) fn type_text(cx: &mut TestAppContext, window: WindowHandle<Workspace>, text: &str) {
    cx.simulate_input(window.into(), text);
    cx.run_until_parked();
}

pub(crate) fn active_path(cx: &TestAppContext, window: WindowHandle<Workspace>) -> Option<PathBuf> {
    window
        .read_with(cx, |workspace, _| workspace.active_path())
        .unwrap()
}

/// The height of a line in the editor, as `editor_tab::style_of` computes it.
pub(crate) fn line_height(cx: &TestAppContext) -> f32 {
    cx.read(|cx| {
        let style = editor_view::EditorStyle::from_theme(cx.global::<Theme>());
        let ratio = style.line_height / style.font_size;
        let size = cx
            .try_global::<Settings>()
            .map_or(13., |settings| settings.font_size);
        (size * ratio).round()
    })
}

pub(crate) fn bounds_of<T: 'static>(
    cx: &mut TestAppContext,
    window: WindowHandle<T>,
    selector: &'static str,
) -> gpui::Bounds<gpui::Pixels> {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("no element {selector} on screen"))
}

/// Clicks the middle of the element with a debug selector.
pub(crate) fn click<T: 'static>(
    cx: &mut TestAppContext,
    window: WindowHandle<T>,
    selector: &'static str,
) {
    let bounds = bounds_of(cx, window, selector);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_click(bounds.center(), Modifiers::default());
    cx.run_until_parked();
}

/// Runs code with the window but without the borrow of the root view, as a popup callback
/// does after the window is free again.
pub(crate) fn with_window<T: 'static>(
    cx: &mut TestAppContext,
    window: WindowHandle<T>,
    run: impl FnOnce(&mut gpui::Window, &mut gpui::App),
) {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.update(run);
    cx.run_until_parked();
}

/// Clicks the gutter of the editor `pane` at a buffer line counted from zero.
pub(crate) fn click_gutter(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    pane: usize,
    line: usize,
) {
    let height = line_height(cx);
    let selector = if pane == 0 {
        "editor-body-0"
    } else {
        "editor-body-1"
    };
    let bounds = bounds_of(cx, window, selector);
    let x = bounds.left() + px(30.);
    let y = bounds.top() + px(height * line as f32 + height / 2.);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_click(point(x, y), Modifiers::default());
    cx.run_until_parked();
}

pub(crate) fn open_sample_window(
    cx: &mut TestAppContext,
    name: &str,
) -> (PathBuf, WindowHandle<Workspace>) {
    let root = sample_project(name);
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    draw(cx, window);
    (root, window)
}

/// A search popup on an empty query, the way its shortcut shows it.
pub(crate) fn open_palette(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    command: &'static str,
) {
    window
        .update(cx, |workspace, window, cx| {
            workspace.run_command(command, window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
}

#[ui_kit::gpui::test]
fn opens_a_project_without_a_window(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "open");
    let workspace = window.root(cx).unwrap();
    let (seen, project_dock_open, project_shown) = workspace.read_with(cx, |workspace, _| {
        (
            workspace.root().to_path_buf(),
            workspace.dock_open(DockSide::Left),
            workspace.tool_open(PROJECT_TOOL),
        )
    });
    assert_eq!(seen, root);
    assert!(project_dock_open, "the left dock starts open");
    assert!(project_shown, "the project tool is in the left dock");
}

#[ui_kit::gpui::test]
fn every_tool_window_opens_and_closes_by_its_action(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "tools");
    let ids = [
        PROJECT_TOOL,
        COMMIT_TOOL,
        STRUCTURE_TOOL,
        RUN_TOOL,
        DEBUG_TOOL,
        USAGES_TOOL,
        PROBLEMS_TOOL,
        TERMINAL_TOOL,
        SERVICES_TOOL,
        TODO_TOOL,
    ];
    for id in ids {
        // The project tool starts in the open left dock: close it first, as a click does.
        window
            .update(cx, |workspace, window, cx| {
                if workspace.tool_open(id) {
                    workspace.toggle_tool(id, window, cx);
                }
            })
            .unwrap();
        let opened = window
            .update(cx, |workspace, window, cx| {
                workspace.toggle_tool(id, window, cx);
                workspace.tool_open(id)
            })
            .unwrap();
        assert!(opened, "{id} did not open");
        cx.run_until_parked();
        draw(cx, window);
        let closed = window
            .update(cx, |workspace, window, cx| {
                workspace.toggle_tool(id, window, cx);
                workspace.tool_open(id)
            })
            .unwrap();
        assert!(!closed, "{id} did not close");
        cx.run_until_parked();
        draw(cx, window);
    }
}

#[ui_kit::gpui::test]
fn find_action_opens_a_setting_without_a_window(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "setting-open");
    open_palette(cx, window, keys::FIND_ACTION);
    cx.simulate_keystrokes(window.into(), "f o n t");
    draw(cx, window);
    cx.simulate_keystrokes(window.into(), "enter");
    cx.run_until_parked();
    draw(cx, window);
}

#[ui_kit::gpui::test]
fn search_everywhere_opens_a_setting_without_a_window(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "setting-everywhere");
    open_palette(cx, window, keys::SEARCH_EVERYWHERE);
    cx.simulate_keystrokes(window.into(), "f o n t");
    draw(cx, window);
    cx.simulate_keystrokes(window.into(), "enter");
    cx.run_until_parked();
    draw(cx, window);
}

#[ui_kit::gpui::test]
fn find_action_toggles_a_boolean_setting_and_saves_it(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "setting-toggle");
    assert!(!cx.read_global::<Settings, _>(|settings, _| settings.show_whitespace));
    open_palette(cx, window, keys::FIND_ACTION);
    cx.simulate_keystrokes(window.into(), "w h i t e s p a c e");
    draw(cx, window);
    cx.simulate_keystrokes(window.into(), "right");
    cx.run_until_parked();
    draw(cx, window);

    assert!(
        cx.read_global::<Settings, _>(|settings, _| settings.show_whitespace),
        "the switch changes at once"
    );
    let file = window
        .update(cx, |workspace, _, _| {
            workspace.settings_dir().join(SETTINGS_FILE)
        })
        .unwrap();
    assert!(
        Settings::load(&file).show_whitespace,
        "the switch is written to the settings file"
    );
}

#[ui_kit::gpui::test]
fn opens_edits_saves_and_closes_a_tab(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "tab");
    let path = root.join("src").join("main.txt");
    window
        .update(cx, |workspace, window, cx| {
            workspace.open_file(path.clone(), window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    let tabs = window
        .update(cx, |workspace, _, _| workspace.open_tabs())
        .unwrap();
    assert_eq!(tabs, vec![vec![path.clone()]]);

    let view = window
        .update(cx, |workspace, _, _| workspace.view_of(&path))
        .unwrap()
        .and_then(|view| view.downcast::<EditorView>().ok())
        .expect("the opened tab has an editor");
    let before = view
        .read_with(cx, |view, _| view.text())
        .expect("the file is read");
    assert_eq!(before, "alpha\nbeta\n");
    view.update(cx, |view, cx| view.replace_text(0..0, "edited ", cx));
    assert!(view.read_with(cx, |view, _| view.is_dirty()));

    let error = view.update(cx, |view, cx| view.save_blocking(cx));
    assert!(error.is_none(), "{error:?}");
    assert_eq!(fs::read_to_string(&path).unwrap(), "edited alpha\nbeta\n");

    window
        .update(cx, |workspace, window, cx| {
            workspace.close_active_tab(window, cx)
        })
        .unwrap();
    let (tabs, closed) = window
        .update(cx, |workspace, _, _| {
            (workspace.open_tabs(), workspace.closed_tabs().to_vec())
        })
        .unwrap();
    assert!(tabs.iter().all(Vec::is_empty), "{tabs:?}");
    assert_eq!(closed, vec![path]);
}

/// Every command Find Action lists here runs without a language server and without a
/// process of its own. The editor commands come last, before a split takes the tab away.
#[ui_kit::gpui::test]
fn find_action_commands_run_without_a_server(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "commands");
    window
        .update(cx, |workspace, window, cx| {
            workspace.open_file(root.join("src").join("main.txt"), window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    let ids = [
        keys::GOTO_FILE,
        keys::GOTO_CLASS,
        keys::GOTO_SYMBOL,
        keys::SEARCH_EVERYWHERE,
        keys::FIND_ACTION,
        keys::RECENT_FILES,
        keys::SWITCHER,
        keys::FILE_STRUCTURE,
        keys::FIND_IN_FILES,
        keys::REPLACE_IN_FILES,
        keys::RECENT_LOCATIONS,
        keys::BACK,
        keys::FORWARD,
        "window.toggle_theme",
        "window.toggle_project",
        "window.toggle_terminal",
        "window.toggle_assistant",
        "window.next_tab",
        editor_keys::SELECT_ALL,
        editor_keys::MOVE_LINE_DOWN,
        editor_keys::DUPLICATE,
        editor_keys::TOGGLE_COMMENT,
        editor_keys::UNDO,
        editor_keys::REDO,
        editor_keys::FIND,
        editor_keys::GOTO_LINE,
        editor_keys::MATCH_BRACKET,
        editor_keys::FOLD,
        editor_keys::UNFOLD,
        editor_keys::TOGGLE_SOFT_WRAP,
        editor_keys::TOGGLE_WHITESPACE,
        "window.split_right",
    ];
    for id in ids {
        let ran = window
            .update(cx, |workspace, window, cx| {
                workspace.run_command(id, window, cx)
            })
            .unwrap();
        assert!(ran, "{id} is not registered in Find Action");
        cx.run_until_parked();
        draw(cx, window);
    }
}

#[ui_kit::gpui::test]
fn typing_and_the_editing_keys_change_the_buffer(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "edit-keys");
    let path = root.join("src").join("main.txt");
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    assert_eq!(buffer_of(cx, &view), "alpha\nbeta\n");

    type_text(cx, window, "X");
    assert_eq!(buffer_of(cx, &view), "Xalpha\nbeta\n");

    press(cx, window, "ctrl-d");
    let duplicated = buffer_of(cx, &view);
    assert_eq!(duplicated, "Xalpha\nXalpha\nbeta\n");

    press(cx, window, "ctrl-z");
    let undone = buffer_of(cx, &view);
    assert_ne!(undone, duplicated, "Ctrl+Z takes the duplicate back");
    press(cx, window, "ctrl-shift-z");
    assert_eq!(
        buffer_of(cx, &view),
        duplicated,
        "Ctrl+Shift+Z brings it again"
    );

    // Ctrl+W grows the selection to the word at the caret.
    press(cx, window, "ctrl-w");
    let selected = view.read_with(cx, |view, _| view.selected_chars());
    assert!(selected > 0, "Ctrl+W selects the word under the caret");

    // Alt+J adds the next occurrence; a typed character edits every caret at once.
    press(cx, window, "escape");
    press(cx, window, "ctrl-home");
    press(cx, window, "alt-j alt-j");
    type_text(cx, window, "q");
    assert_eq!(buffer_of(cx, &view), "q\nq\nbeta\n");
}

#[ui_kit::gpui::test]
fn ctrl_slash_comments_and_uncomments_a_line_with_a_language(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "comment");
    let path = root.join("Dockerfile");
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    press(cx, window, "ctrl-home");
    press(cx, window, "ctrl-/");
    assert_eq!(buffer_of(cx, &view), "#FROM scratch\n");
    press(cx, window, "ctrl-/");
    assert_eq!(buffer_of(cx, &view), "FROM scratch\n");
}

#[ui_kit::gpui::test]
fn find_replace_and_goto_move_the_caret_and_edit(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "find");
    let path = root.join("src").join("main.txt");
    fs::write(&path, "alpha\nbeta\ngamma alpha\n").unwrap();
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);

    press(cx, window, "ctrl-f");
    type_text(cx, window, "alpha");
    let first = view
        .read_with(cx, |view, _| view.selection())
        .expect("the file is read");
    assert_eq!(first.0, 0..5, "the first match is selected");
    press(cx, window, "enter");
    let second = view
        .read_with(cx, |view, _| view.selection())
        .expect("the file is read");
    assert_eq!(second.0, 17..22, "Enter goes to the next match");
    press(cx, window, "escape");

    press(cx, window, "ctrl-r");
    // The query stays from the last search; Tab goes to the replacement field.
    press(cx, window, "tab");
    type_text(cx, window, "ALPHA");
    press(cx, window, "enter");
    assert_eq!(
        buffer_of(cx, &view),
        "alpha\nbeta\ngamma ALPHA\n",
        "Enter replaces the current match"
    );
    press(cx, window, "enter");
    assert_eq!(
        buffer_of(cx, &view),
        "ALPHA\nbeta\ngamma ALPHA\n",
        "the next match is replaced too"
    );
    press(cx, window, "escape");

    press(cx, window, "ctrl-g");
    type_text(cx, window, "2");
    press(cx, window, "enter");
    assert_eq!(
        view.read_with(cx, |view, _| view.caret()),
        (1, 0),
        "Ctrl+G moves the caret to the line"
    );
}

#[ui_kit::gpui::test]
fn ctrl_tab_switches_tabs_and_the_tab_actions_keep_the_rest(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "tabs");
    let main = root.join("src").join("main.txt");
    let notes = root.join("src").join("notes.txt");
    let readme = root.join("README.txt");
    open_path(cx, window, &main);
    open_path(cx, window, &notes);
    open_path(cx, window, &readme);
    assert_eq!(active_path(cx, window), Some(readme));

    // Ctrl+Tab opens the Switcher on the file used before this one.
    press(cx, window, "ctrl-tab");
    draw(cx, window);
    press(cx, window, "enter");
    assert_eq!(active_path(cx, window), Some(notes.clone()));

    let pinned = window
        .update(cx, |workspace, window, cx| {
            workspace.pin_active_tab(window, cx);
            workspace.pinned_tabs()
        })
        .unwrap();
    assert_eq!(pinned, vec![notes.clone()]);

    let ran = window
        .update(cx, |workspace, window, cx| {
            workspace.run_command("window.close_other_tabs", window, cx)
        })
        .unwrap();
    assert!(ran, "Close Other Tabs is a command");
    cx.run_until_parked();
    draw(cx, window);
    let (tabs, active) = window
        .update(cx, |workspace, _, _| {
            (workspace.open_tabs(), workspace.active_path())
        })
        .unwrap();
    assert_eq!(tabs, vec![vec![notes.clone()]]);
    assert_eq!(active, Some(notes));
}

#[ui_kit::gpui::test]
fn splits_go_right_and_down_and_a_tab_moves_between_groups(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "splits");
    let main = root.join("src").join("main.txt");
    let notes = root.join("src").join("notes.txt");
    open_path(cx, window, &main);
    open_path(cx, window, &notes);

    let (tabs, group, down) = window
        .update(cx, |workspace, window, cx| {
            workspace.split_right(window, cx);
            (
                workspace.open_tabs(),
                workspace.active_group(),
                workspace.split_is_down(),
            )
        })
        .unwrap();
    assert_eq!(
        tabs,
        vec![vec![main.clone()], vec![notes.clone()]],
        "Split Right takes the selected tab to a new group"
    );
    assert_eq!(group, 1);
    assert!(!down);

    let (tabs, group) = window
        .update(cx, |workspace, window, cx| {
            workspace.move_to_other_group(window, cx);
            (workspace.open_tabs(), workspace.active_group())
        })
        .unwrap();
    assert_eq!(
        tabs,
        vec![vec![main.clone(), notes.clone()]],
        "the tab comes back to the first group"
    );
    assert_eq!(group, 0, "the group with the moved tab gets the focus");

    let (tabs, group, down) = window
        .update(cx, |workspace, window, cx| {
            workspace.split_down(window, cx);
            (
                workspace.open_tabs(),
                workspace.active_group(),
                workspace.split_is_down(),
            )
        })
        .unwrap();
    assert_eq!(tabs, vec![vec![main], vec![notes]]);
    assert_eq!(group, 1);
    assert!(down, "the first split sets the direction of the groups");
}

#[ui_kit::gpui::test]
fn the_project_tree_creates_renames_and_deletes_in_a_scratch_folder(cx: &mut TestAppContext) {
    // Delete must not put the scratch files into the Recycle Bin of the machine.
    crate::tree_ops::use_temporary_delete();
    let (root, window) = open_sample_window(cx, "tree");
    let panel = cx.update(|cx| cx.global::<ActiveProjectPanel>().0.clone());

    // Alt+Insert on the src folder, the first entry of the New menu: a file.
    let src = root.join("src");
    with_window(cx, window, |_, cx| {
        panel.update(cx, |panel, cx| panel.select_path(&src, cx))
    });
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(PROJECT_TOOL, window, cx)
        })
        .unwrap();
    draw(cx, window);
    press(cx, window, "alt-insert");
    draw(cx, window);
    click(cx, window, "menu-0");
    draw(cx, window);
    type_text(cx, window, "made.txt");
    press(cx, window, "enter");
    draw(cx, window);
    let made = root.join("src").join("made.txt");
    assert_eq!(fs::read_to_string(&made).unwrap(), "");
    assert_eq!(active_path(cx, window), Some(made.clone()));
    let found = panel.read_with(cx, |panel, _| panel.tree().index_of(&made));
    assert!(found.is_some(), "the new file is in the tree");

    // The second entry: a folder.
    with_window(cx, window, |_, cx| {
        panel.update(cx, |panel, cx| panel.select_path(&src, cx))
    });
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(PROJECT_TOOL, window, cx)
        })
        .unwrap();
    draw(cx, window);
    press(cx, window, "alt-insert");
    draw(cx, window);
    click(cx, window, "menu-1");
    draw(cx, window);
    type_text(cx, window, "made_dir");
    press(cx, window, "enter");
    draw(cx, window);
    let folder = root.join("src").join("made_dir");
    assert!(folder.is_dir());
    let found = panel.read_with(cx, |panel, _| panel.tree().index_of(&folder));
    assert!(found.is_some(), "the new folder is in the tree");

    // Shift+F6 in the tree asks for the new name, as a click on Rename does.
    with_window(cx, window, |_, cx| {
        panel.update(cx, |panel, cx| panel.select_path(&made, cx))
    });
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(PROJECT_TOOL, window, cx)
        })
        .unwrap();
    draw(cx, window);
    press(cx, window, "shift-f6");
    draw(cx, window);
    press(
        cx,
        window,
        "backspace backspace backspace backspace backspace backspace backspace backspace",
    );
    type_text(cx, window, "renamed.txt");
    press(cx, window, "enter");
    draw(cx, window);
    let renamed = root.join("src").join("renamed.txt");
    assert!(!made.exists() && renamed.is_file());
    assert_eq!(
        active_path(cx, window),
        Some(renamed.clone()),
        "the tab follows the renamed file"
    );

    // Delete in the tree asks once and then moves the file to the Recycle Bin.
    with_window(cx, window, |_, cx| {
        panel.update(cx, |panel, cx| panel.select_path(&renamed, cx))
    });
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(PROJECT_TOOL, window, cx)
        })
        .unwrap();
    draw(cx, window);
    press(cx, window, "delete");
    draw(cx, window);
    press(cx, window, "enter");
    draw(cx, window);
    assert!(!renamed.exists(), "the file left the scratch folder");
    let tabs = window
        .update(cx, |workspace, _, _| workspace.open_tabs())
        .unwrap();
    assert!(
        tabs.iter().flatten().all(|open| open != &renamed),
        "the tab of a deleted file is gone"
    );

    with_window(cx, window, |_, cx| {
        panel.update(cx, |panel, cx| panel.select_path(&folder, cx))
    });
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(PROJECT_TOOL, window, cx)
        })
        .unwrap();
    draw(cx, window);
    press(cx, window, "delete");
    draw(cx, window);
    press(cx, window, "enter");
    draw(cx, window);
    assert!(!folder.exists(), "the folder left the scratch folder");
}

#[ui_kit::gpui::test]
fn local_history_lists_a_saved_version_and_reverts_to_it(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "history");
    let path = root.join("src").join("main.txt");
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);

    view.update(cx, |view, cx| {
        view.replace_text(0..5, "one", cx);
        let error = view.save_blocking(cx);
        assert!(error.is_none(), "{error:?}");
    });
    cx.run_until_parked();
    view.update(cx, |view, cx| view.replace_text(0..3, "two", cx));
    assert_eq!(buffer_of(cx, &view), "two\nbeta\n");

    window
        .update(cx, |workspace, window, cx| {
            crate::history_view::HistoryView::show(path.clone(), workspace, window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);

    let report = cx.update(|cx| crate::history_view::report(cx));
    assert!(
        report.iter().any(|line| line == "history-version Saved"),
        "the saved version is listed: {report:?}"
    );
    assert!(
        report.iter().any(|line| line == "history-diff hunks 1"),
        "the window compares the version with the file now: {report:?}"
    );

    cx.update(crate::history_view::revert_shown);
    cx.run_until_parked();
    assert_eq!(buffer_of(cx, &view), "one\nbeta\n");
    assert_eq!(fs::read_to_string(&path).unwrap(), "one\nbeta\n");
}

pub(crate) fn git_repository(name: &str) -> PathBuf {
    let root = scratch(name);
    let git = vcs::GitRunner::from_env().expect("git is on the PATH");
    git.run(&root, ["init"]).unwrap();
    git.run(&root, ["config", "user.email", "test@example.com"])
        .unwrap();
    git.run(&root, ["config", "user.name", "Window Test"])
        .unwrap();
    fs::write(root.join("main.txt"), "alpha\nbeta\n").unwrap();
    git.run(&root, ["add", "main.txt"]).unwrap();
    git.run(&root, ["commit", "-m", "base"]).unwrap();
    root
}

#[ui_kit::gpui::test]
fn git_marks_the_gutter_commits_and_reverts_a_hunk(cx: &mut TestAppContext) {
    let root = git_repository("git");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    draw(cx, window);
    let git = cx.update(|cx| cx.global::<ActiveGit>().0.clone());
    assert!(
        git.read_with(cx, |git, _| git.repo().is_some()),
        "the window found the repository"
    );

    let path = root.join("main.txt");
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    view.update(cx, |view, cx| {
        view.replace_text(0..5, "ALPHA", cx);
        let error = view.save_blocking(cx);
        assert!(error.is_none(), "{error:?}");
    });
    git.update(cx, |git, cx| git.refresh(cx));
    cx.run_until_parked();
    assert_eq!(
        git.read_with(cx, |git, _| git.kind_of(&path)),
        Some(crate::git::Kind::Modified)
    );
    let marks = view.read_with(cx, |view, _| view.gutter_marks().len());
    assert!(marks > 0, "the changed line has a mark in the gutter");

    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(COMMIT_TOOL, window, cx)
        })
        .unwrap();
    draw(cx, window);
    type_text(cx, window, "first change");
    press(cx, window, "ctrl-enter");
    cx.run_until_parked();
    let status = git.read_with(cx, |git, _| git.status().clone());
    assert!(
        status.files.is_empty(),
        "the commit took the change: {status:?}"
    );
    let log = git
        .read_with(cx, |git, _| git.repo())
        .and_then(|repo| repo.log(1, 0).ok())
        .unwrap_or_default();
    assert_eq!(log[0].subject, "first change");

    // A second change against the commit: the diff window and the rollback of a hunk.
    view.update(cx, |view, cx| {
        view.replace_text(0..5, "BETA", cx);
        let error = view.save_blocking(cx);
        assert!(error.is_none(), "{error:?}");
    });
    git.update(cx, |git, cx| git.refresh(cx));
    cx.run_until_parked();
    let weak = window.root(cx).unwrap().downgrade();
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.update(|window, cx| {
        crate::diff_view::DiffView::open(git.clone(), weak, path.clone(), window, cx)
    });
    cx.run_until_parked();
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "diff-revert-0").size.height > px(0.),
        "the diff lists a hunk"
    );
    click(cx, window, "diff-revert-0");
    cx.run_until_parked();
    assert_eq!(fs::read_to_string(&path).unwrap(), "ALPHA\nbeta\n");
    assert_eq!(buffer_of(cx, &view), "ALPHA\nbeta\n");

    // Ctrl+Alt+Z takes the change under the caret back to the last commit.
    view.update(cx, |view, cx| {
        view.replace_text(0..5, "GAMMA", cx);
        let error = view.save_blocking(cx);
        assert!(error.is_none(), "{error:?}");
        view.restore_caret(0, 0, cx);
    });
    git.update(cx, |git, cx| git.refresh(cx));
    cx.run_until_parked();
    press(cx, window, "ctrl-alt-z");
    cx.run_until_parked();
    assert_eq!(fs::read_to_string(&path).unwrap(), "ALPHA\nbeta\n");
    assert_eq!(buffer_of(cx, &view), "ALPHA\nbeta\n");
}

#[ui_kit::gpui::test]
fn debug_breakpoints_come_from_the_gutter_and_the_keys(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "debug");
    let path = root.join("src").join("main.txt");
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    let debug = cx.update(|cx| cx.global::<ActiveDebug>().0.clone());

    click_gutter(cx, window, 0, 1);
    cx.run_until_parked();
    draw(cx, window);
    assert_eq!(
        debug.read_with(cx, |panel, _| panel.breakpoint_lines(&path)),
        vec![2],
        "a click in the gutter puts a breakpoint on line 2"
    );
    assert_eq!(
        view.read_with(cx, |view, _| view.breakpoints().to_vec()),
        [1],
        "the gutter of the open editor shows it"
    );

    // Ctrl+F8 on another line adds a second one.
    view.update(cx, |view, cx| view.restore_caret(3, 0, cx));
    draw(cx, window);
    press(cx, window, "ctrl-f8");
    assert_eq!(
        debug.read_with(cx, |panel, _| panel.breakpoint_lines(&path)),
        vec![2, 3]
    );
    // The same key takes it away again; one breakpoint keeps the condition row unique.
    press(cx, window, "ctrl-f8");
    assert_eq!(
        debug.read_with(cx, |panel, _| panel.breakpoint_lines(&path)),
        vec![2]
    );

    // Ctrl+Shift+F8 lists them; the condition of the first one is edited in a prompt.
    press(cx, window, "ctrl-shift-f8");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "breakpoints-dialog").size.height > px(0.),
        "the list of breakpoints is open"
    );
    click(cx, window, "breakpoint-dialog-condition");
    draw(cx, window);
    type_text(cx, window, "x>1");
    press(cx, window, "enter");
    cx.run_until_parked();
    assert_eq!(
        debug.read_with(cx, |panel, _| panel.condition(&path, 2).map(str::to_owned)),
        Some("x>1".to_string())
    );
}

pub(crate) fn usage(path: &Path, line: usize, column: usize, name: &str) -> Usage {
    let uri = CanonPath::new(path).unwrap().to_uri();
    Usage {
        name: name.to_string(),
        target: SourceSpan {
            uri,
            span: TextSpan {
                start: TextPoint {
                    line: line as u32,
                    character: column as u32,
                },
                end: TextPoint {
                    line: line as u32,
                    character: column as u32 + 1,
                },
            },
        },
    }
}

#[ui_kit::gpui::test]
fn problems_and_usages_start_empty_and_open_a_place(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "lists");
    let main = root.join("src").join("main.txt");
    let notes = root.join("src").join("notes.txt");

    let problems = cx.update(|cx| cx.global::<ActiveProblems>().0.clone());
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(PROBLEMS_TOOL, window, cx)
        })
        .unwrap();
    draw(cx, window);
    assert!(
        problems.read_with(cx, |hub, cx| hub.panel().read(cx).items().is_empty()),
        "a project with no server has no problems"
    );

    problems.update(cx, |hub, cx| {
        hub.set_file_problems(
            main.clone(),
            vec![Problem {
                line: 1,
                column: 4,
                severity: Severity::Error,
                message: "cannot resolve the name".into(),
            }],
            cx,
        );
    });
    cx.run_until_parked();
    draw(cx, window);
    let items = problems.read_with(cx, |hub, cx| hub.panel().read(cx).items().to_vec());
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].label, "cannot resolve the name");
    press(cx, window, "down enter");
    draw(cx, window);
    assert_eq!(active_path(cx, window), Some(main.clone()));
    let view = editor_of(cx, window, &main);
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (1, 4));

    let usages = cx.update(|cx| cx.global::<ActiveUsages>().0.clone());
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(USAGES_TOOL, window, cx)
        })
        .unwrap();
    draw(cx, window);
    assert!(
        usages.read_with(cx, |hub, cx| hub.panel().read(cx).items().is_empty()),
        "no symbol was asked about yet"
    );

    usages.update(cx, |hub, cx| {
        hub.set_usages(
            vec![usage(&notes, 0, 0, "later"), usage(&main, 1, 0, "earlier")],
            cx,
        );
    });
    cx.run_until_parked();
    draw(cx, window);
    let items = usages.read_with(cx, |hub, cx| hub.panel().read(cx).items().to_vec());
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].path, main, "the list is sorted by file");
    assert_eq!(items[0].label, "earlier");
    press(cx, window, "down enter");
    assert_eq!(active_path(cx, window), Some(main.clone()));
    let view = editor_of(cx, window, &main);
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (1, 0));
}

#[ui_kit::gpui::test]
fn the_start_screen_shows_recent_projects_and_opens_the_new_project_mode(cx: &mut TestAppContext) {
    let root = sample_project("start");
    let templates = vec![ProjectTemplate {
        label: "Plain",
        files: |_| Vec::new(),
    }];
    let screen = cx.add_window(|window, cx| {
        cx.set_global(Theme::dark());
        StartScreen::new_for_test(
            vec![root.clone()],
            scratch("start-state"),
            templates.clone(),
            window,
            cx,
        )
    });
    cx.run_until_parked();
    draw(cx, screen);
    assert_eq!(
        screen
            .read_with(cx, |screen, _| screen.mode_name())
            .unwrap(),
        "home"
    );

    click(cx, screen, "start-new");
    draw(cx, screen);
    assert_eq!(
        screen
            .read_with(cx, |screen, _| screen.mode_name())
            .unwrap(),
        "new"
    );
}

#[ui_kit::gpui::test]
fn settings_open_change_at_once_and_reach_the_file(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "settings");
    let settings_file = window
        .read_with(cx, |workspace, _| {
            workspace.settings_dir().join(workspace::SETTINGS_FILE)
        })
        .unwrap();
    press(cx, window, "ctrl-alt-s");
    draw(cx, window);

    let before = cx.update(|cx| cx.global::<Settings>().font_size);
    click(cx, window, "settings-font-up");
    let after = cx.update(|cx| cx.global::<Settings>().font_size);
    assert_eq!(after, before + 1., "the font size changes at once");

    draw(cx, window);
    click(cx, window, "settings-page-editor");
    draw(cx, window);
    click(cx, window, "settings-soft-wrap");
    assert!(
        cx.update(|cx| cx.global::<Settings>().soft_wrap),
        "the switch changes the setting"
    );

    cx.run_until_parked();
    let saved: Settings = serde_json::from_str(&fs::read_to_string(&settings_file).unwrap())
        .expect("the settings file is written");
    assert_eq!(saved.font_size, after);
    assert!(saved.soft_wrap);
}

#[ui_kit::gpui::test]
fn evaluate_without_a_session_opens_a_prompt(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "evaluate");
    let path = root.join("src").join("main.txt");
    open_path(cx, window, &path);
    press(cx, window, "alt-f8");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "prompt").size.height > px(0.),
        "Alt+F8 asks for an expression without a session"
    );
    press(cx, window, "escape");
    cx.run_until_parked();
    draw(cx, window);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    assert!(
        visual.debug_bounds("prompt").is_none(),
        "Escape closes the prompt"
    );
}

/// One key binding of the application, as the keymap check sees it.
struct Bound {
    keys: String,
    action: &'static str,
    context: Option<String>,
}

/// The IDEA action id of one of our actions, if it has one.
fn idea_action_of(action: &str) -> Option<&'static str> {
    editor_keys::IDEA_IDS
        .iter()
        .chain(keys::IDEA_IDS)
        .find(|(_, our)| *our == action)
        .map(|(id, _)| *id)
}

fn idea_keys() -> HashMap<String, Vec<String>> {
    keymap::idea_shortcuts(keymap::IDEA_DEFAULT)
        .expect("idea-default.json parses")
        .into_iter()
        .map(|(id, shortcuts)| {
            (
                id,
                shortcuts
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>(),
            )
        })
        .collect()
}

/// Every binding the GPUI keymap holds, after the window and the panels added theirs.
fn app_bindings(cx: &mut TestAppContext) -> Vec<Bound> {
    cx.update(|cx| {
        let keymap = cx.key_bindings();
        let keymap = keymap.borrow();
        keymap
            .bindings()
            .map(|binding| Bound {
                keys: binding
                    .keystrokes()
                    .iter()
                    .map(|stroke| stroke.as_keystroke().unparse(OperatingSystem::Windows))
                    .collect::<Vec<_>>()
                    .join(" "),
                action: binding.action().name(),
                context: binding
                    .predicate()
                    .and_then(|predicate| match predicate.as_ref() {
                        KeyBindingContextPredicate::Identifier(name) => Some(name.to_string()),
                        _ => None,
                    }),
            })
            .collect()
    })
}

/// The context a deviation key lives in; `global` is a key with no predicate.
fn deviation_context(deviation: &keymap::Deviation) -> Option<&'static str> {
    match deviation.context {
        "global" => None,
        other => Some(other),
    }
}

/// The keys of our binding rows must be IDEA keys of the action they name, the
/// intentional differences have a reason and a real binding, and one key in one
/// context runs one action. A global key must not shadow an editor or workspace
/// action unless the deviation list says so.
#[ui_kit::gpui::test]
fn the_window_keymap_matches_idea_without_conflicts(cx: &mut TestAppContext) {
    let (_, _window) = open_sample_window(cx, "keymap");
    cx.update(workspace::bind_keys);
    let bindings = app_bindings(cx);
    let idea = idea_keys();

    let rows = workspace::WINDOW_KEYS
        .iter()
        .map(|row| (row.keys, row.action, row.idea))
        .chain(
            crate::project::PANEL_KEYS
                .iter()
                .map(|row| (row.keys, row.action, row.idea)),
        );
    for (keys, action, row_idea) in rows {
        let Some(id) = row_idea else { continue };
        let expected = idea
            .get(id)
            .unwrap_or_else(|| panic!("{id} is in the IDEA file"));
        assert!(
            expected.iter().any(|expected| expected == keys),
            "{action}: {keys} is not a key of IDEA {id}"
        );
    }

    for deviation in keymap::DEVIATIONS {
        assert!(!deviation.reason.is_empty(), "{deviation:?}");
        if deviation.keys.is_empty() {
            continue;
        }
        let found = bindings.iter().any(|binding| {
            binding.keys == deviation.keys
                && binding.context.as_deref() == deviation_context(deviation)
        });
        assert!(found, "the deviation {deviation:?} has no binding");
        if let Some(id) = deviation.idea_action {
            let expected = idea
                .get(id)
                .unwrap_or_else(|| panic!("{id} is in the IDEA file"));
            assert!(
                expected.iter().any(|keys| keys == deviation.keys),
                "{} is not a key of IDEA {id}",
                deviation.keys
            );
        }
    }

    let mut seen: HashMap<(Option<String>, String), &'static str> = HashMap::new();
    for binding in &bindings {
        let key = (binding.context.clone(), binding.keys.clone());
        if let Some(previous) = seen.insert(key.clone(), binding.action) {
            assert_eq!(
                previous, binding.action,
                "{key:?} runs {previous} and {}",
                binding.action
            );
        }
    }

    let (_, keyboard) = keymap::default_keymap();
    for binding in &bindings {
        if binding.context.is_some() {
            continue;
        }
        let Ok(stroke) = keymap::Keystroke::parse(&binding.keys) else {
            continue;
        };
        for (context, label) in [
            (editor_keys::CONTEXT, "the editor"),
            (keys::CONTEXT, "the workspace"),
        ] {
            let Some(action) = keyboard.resolve(&stroke, context) else {
                continue;
            };
            if keymap::deviation_for("global", &binding.keys, idea_action_of(action)).is_some() {
                continue;
            }
            panic!(
                "the global {} runs {} but {label} binds it to {action}",
                binding.keys, binding.action
            );
        }
    }
}
