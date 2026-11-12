//! The actions of the window without a real window: the search and navigation keys,
//! Find in Files, Run Anything, Local History, the tool windows, Find Action itself and
//! the Git windows it opens. The language is the sample one of `window_tests_ui`, the
//! debuggee is the mock adapter of the fixture language.

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]
use std::fs;
use std::io::Write;
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use dap_client::{BreakpointSpec, DebugService, Debugger};
use keymap::workspace as keys;
use proc_sup::ProcessSpec;
use runner::{RunPlan, TestListen, TestProvider, TestRequest, TestSite};
use ui_kit::gpui;
use ui_kit::gpui::{ModifiersChangedEvent, TestAppContext, VisualTestContext, WindowHandle};
use ui_kit::{Modifiers, px};
use workspace::{
    AI_TOOL, COMMIT_TOOL, DEBUG_TOOL, LOG_TOOL, PERFORMANCE_TOOL, PROBLEMS_TOOL, PROJECT_TOOL,
    RUN_TOOL, SERVICES_TOOL, STRUCTURE_TOOL, TERMINAL_TOOL, TODO_TOOL, USAGES_TOOL, Workspace,
};

use crate::debug::ActiveDebug;
use crate::git_log::ActiveGitLog;
use crate::script::ActiveRunPanel;
use crate::window_tests::{
    active_path, bounds_of, buffer_of, click, draw, editor_of, git_repository, open_palette,
    open_path, open_sample_window, open_window, open_window_with, press, sample_project, scratch,
    type_text, wait_for_outline_signal, wait_for_process, wait_for_project_index,
};
#[cfg(windows)]
use crate::window_tests_run::EchoRun;
use crate::window_tests_run::setup;
use crate::window_tests_ui::{
    double_shift, navigation_project, open_test_window, wait_for, wait_for_files, wait_for_outline,
    wait_for_symbols,
};

/// A row of Find Action runs by its id, as the row of the list does.
fn run_command(cx: &mut TestAppContext, window: WindowHandle<Workspace>, id: &'static str) {
    let ran = window
        .update(cx, |workspace, window, cx| {
            workspace.run_command(id, window, cx)
        })
        .unwrap();
    assert!(ran, "{id} is not a registered command");
    cx.run_until_parked();
    draw(cx, window);
}

/// Types `text` in Find Action and takes the first row.
fn find_action(cx: &mut TestAppContext, window: WindowHandle<Workspace>, text: &str) {
    open_palette(cx, window, keys::FIND_ACTION);
    type_text(cx, window, text);
    draw(cx, window);
    press(cx, window, "enter");
    draw(cx, window);
}

fn has_selector(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &'static str,
) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

fn open_test_root(cx: &mut TestAppContext, root: PathBuf) -> WindowHandle<Workspace> {
    let window = open_test_window(cx, root);
    cx.run_until_parked();
    draw(cx, window);
    window
}

/// Double Shift and Double Ctrl, the way the platform reports the held modifiers.
#[cfg(windows)]
fn double_modifier(cx: &mut TestAppContext, window: WindowHandle<Workspace>, held: Modifiers) {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    let change = |modifiers| ModifiersChangedEvent {
        modifiers,
        ..ModifiersChangedEvent::default()
    };
    visual.simulate_event(change(held));
    visual.simulate_event(change(Modifiers::default()));
    visual.simulate_event(change(held));
    visual.simulate_event(change(Modifiers::default()));
    cx.run_until_parked();
}

#[ui_kit::gpui::test]
fn search_everywhere_tabs_open_classes_files_symbols_and_actions(cx: &mut TestAppContext) {
    let root = navigation_project("actions-tabs");
    let window = open_test_root(cx, root.clone());
    let sample = root.join("src").join("Sample.txt");
    wait_for_project_index(cx);
    wait_for_symbols(cx, window, &sample);

    // Double Shift opens Search Everywhere on All.
    double_shift(cx, window);
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "palette").size.height > px(0.),
        "Double Shift opens Search Everywhere"
    );
    assert!(has_selector(cx, window, "palette-tab-All"));

    // The chips switch the tab; Classes finds the class of the index.
    click(cx, window, "palette-chip-Files");
    draw(cx, window);
    assert!(has_selector(cx, window, "palette-tab-Files"));
    click(cx, window, "palette-chip-Classes");
    draw(cx, window);
    assert!(has_selector(cx, window, "palette-tab-Classes"));
    type_text(cx, window, "Owner");
    draw(cx, window);
    press(cx, window, "enter");
    assert_eq!(active_path(cx, window), Some(sample.clone()));
    let view = editor_of(cx, window, &sample);
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (0, 6));

    // Symbols finds a method of the file.
    wait_for(cx, "the Symbols tab to find the method", |cx| {
        double_shift(cx, window);
        draw(cx, window);
        if !has_selector(cx, window, "palette") {
            return false;
        }
        click(cx, window, "palette-chip-Symbols");
        draw(cx, window);
        type_text(cx, window, "feed");
        draw(cx, window);
        press(cx, window, "enter");
        let at = editor_of(cx, window, &sample).read_with(cx, |view, _| view.caret());
        if at != (5, 11) {
            press(cx, window, "escape");
        }
        at == (5, 11)
    });
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (5, 11));

    // Actions runs a command of the window.
    double_shift(cx, window);
    draw(cx, window);
    click(cx, window, "palette-chip-Actions");
    draw(cx, window);
    type_text(cx, window, "performance monitor");
    draw(cx, window);
    press(cx, window, "enter");
    let opened = window
        .read_with(cx, |workspace, _| workspace.tool_open(PERFORMANCE_TOOL))
        .unwrap();
    assert!(opened, "the Actions tab runs Show Performance Monitor");

    // Tab and Shift+Tab walk the same chips.
    double_shift(cx, window);
    draw(cx, window);
    for name in ["Classes", "Files", "Symbols", "Actions", "All"] {
        press(cx, window, "tab");
        draw(cx, window);
        assert!(
            has_selector(
                cx,
                window,
                match name {
                    "All" => "palette-tab-All",
                    "Classes" => "palette-tab-Classes",
                    "Files" => "palette-tab-Files",
                    "Symbols" => "palette-tab-Symbols",
                    _ => "palette-tab-Actions",
                }
            ),
            "Tab reaches the {name} tab"
        );
    }
    press(cx, window, "shift-tab");
    draw(cx, window);
    assert!(has_selector(cx, window, "palette-tab-Actions"));
    press(cx, window, "escape");
    cx.run_until_parked();
}

#[ui_kit::gpui::test]
fn goto_keys_recent_windows_structure_and_alt_left_right(cx: &mut TestAppContext) {
    let root = navigation_project("actions-navigation");
    let window = open_test_root(cx, root.clone());
    let sample = root.join("src").join("Sample.txt");
    let notes = root.join("src").join("notes.txt");
    let readme = root.join("README.txt");
    wait_for_project_index(cx);
    wait_for_symbols(cx, window, &sample);

    // Ctrl+Shift+N, Ctrl+N and Ctrl+Alt+Shift+N reach the index.
    press(cx, window, "ctrl-shift-n");
    type_text(cx, window, "Sample");
    draw(cx, window);
    press(cx, window, "enter");
    assert_eq!(active_path(cx, window), Some(sample.clone()));

    // The class list fills in the background: retry until Pet is there.
    wait_for(cx, "Go to Class to find Pet", |cx| {
        press(cx, window, "ctrl-n");
        draw(cx, window);
        if !has_selector(cx, window, "palette") {
            return false;
        }
        type_text(cx, window, "Pet");
        draw(cx, window);
        press(cx, window, "enter");
        let at = editor_of(cx, window, &sample).read_with(cx, |view, _| view.caret());
        if at != (4, 6) {
            press(cx, window, "escape");
        }
        at == (4, 6)
    });
    let view = editor_of(cx, window, &sample);
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (4, 6));

    wait_for(cx, "Go to Symbol to find the method", |cx| {
        press(cx, window, "ctrl-alt-shift-n");
        draw(cx, window);
        if !has_selector(cx, window, "palette") {
            return false;
        }
        type_text(cx, window, "feed");
        draw(cx, window);
        press(cx, window, "enter");
        let at = editor_of(cx, window, &sample).read_with(cx, |view, _| view.caret());
        if at != (5, 11) {
            press(cx, window, "escape");
        }
        at == (5, 11)
    });
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (5, 11));

    // Ctrl+F12 shows the structure of the open file.
    wait_for_outline_signal(cx, window, &sample);
    wait_for_outline(cx, window, &sample);
    press(cx, window, "ctrl-f12");
    draw(cx, window);
    assert!(bounds_of(cx, window, "palette").size.height > px(0.));
    press(cx, window, "enter");
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (0, 6));

    // A place of an edit is in Recent Locations.
    view.update(cx, |view, cx| view.restore_caret(2, 0, cx));
    type_text(cx, window, "z");
    view.update(cx, |view, cx| view.restore_caret(0, 0, cx));
    press(cx, window, "ctrl-shift-e");
    draw(cx, window);
    assert!(bounds_of(cx, window, "places-popup").size.height > px(0.));
    press(cx, window, "enter");
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (2, 0));

    // Ctrl+E and Ctrl+Tab go to the file used before the current one.
    open_path(cx, window, &notes);
    open_path(cx, window, &readme);
    press(cx, window, "ctrl-e");
    draw(cx, window);
    press(cx, window, "enter");
    assert_eq!(active_path(cx, window), Some(notes.clone()));

    press(cx, window, "ctrl-tab");
    draw(cx, window);
    press(cx, window, "enter");
    assert_eq!(active_path(cx, window), Some(readme.clone()));

    // Alt+Left and Alt+Right walk the tabs of the group.
    press(cx, window, "alt-left");
    assert_eq!(active_path(cx, window), Some(notes.clone()));
    press(cx, window, "alt-right");
    assert_eq!(active_path(cx, window), Some(readme.clone()));

    // Alt+F1 selects the open file in the project tree.
    press(cx, window, "alt-f1");
    let project_open = window
        .read_with(cx, |workspace, _| workspace.tool_open(PROJECT_TOOL))
        .unwrap();
    assert!(project_open, "Alt+F1 shows the project tree");

    // Ctrl+Alt+Shift+Left and Right move the focus between the split groups.
    run_command(cx, window, "window.split_right");
    press(cx, window, "ctrl-alt-shift-left");
    let left = window
        .read_with(cx, |workspace, _| workspace.active_group())
        .unwrap();
    press(cx, window, "ctrl-alt-shift-right");
    let right = window
        .read_with(cx, |workspace, _| workspace.active_group())
        .unwrap();
    assert_eq!((left, right), (0, 1));
}

#[ui_kit::gpui::test]
fn find_in_files_and_replace_in_files_with_undo(cx: &mut TestAppContext) {
    let root = scratch("actions-find");
    let path = root.join("a.txt");
    fs::write(&path, "alpha\nbeta\nalpha\n").unwrap();
    let window = open_test_root(cx, root.clone());
    wait_for_files(cx, window, &path);

    // Ctrl+Shift+F finds a hit and opens it.
    press(cx, window, "ctrl-shift-f");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "finder").size.height > px(0.),
        "Ctrl+Shift+F opens Find in Files"
    );
    type_text(cx, window, "alpha");
    draw(cx, window);
    press(cx, window, "enter");
    assert_eq!(active_path(cx, window), Some(path.clone()));
    let view = editor_of(cx, window, &path);
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (0, 0));

    // Ctrl+Shift+R replaces every hit of the open file, and Ctrl+Z takes it back.
    press(cx, window, "ctrl-shift-r");
    draw(cx, window);
    type_text(cx, window, "alpha");
    press(cx, window, "tab");
    type_text(cx, window, "omega");
    press(cx, window, "ctrl-enter");
    wait_for(cx, "the replaced text", |cx| {
        buffer_of(cx, &view) == "omega\nbeta\nomega\n"
    });
    assert_eq!(fs::read_to_string(&path).unwrap(), "omega\nbeta\nomega\n");

    // The finder still has the focus: a click in the editor gives it back.
    press(cx, window, "escape");
    cx.run_until_parked();
    click(cx, window, "editor-body-0");
    press(cx, window, "ctrl-z");
    assert_eq!(
        buffer_of(cx, &view),
        "alpha\nbeta\nalpha\n",
        "Ctrl+Z undoes the replacement of the open file"
    );
    assert!(view.update(cx, |view, cx| view.save_blocking(cx)).is_none());
    assert_eq!(fs::read_to_string(&path).unwrap(), "alpha\nbeta\nalpha\n");
}

#[cfg(windows)]
#[ui_kit::gpui::test]
fn run_anything_by_double_ctrl_runs_the_configuration(cx: &mut TestAppContext) {
    let root = sample_project("actions-run-anything");
    let mut setup = setup(&root);
    setup.providers = vec![Arc::new(EchoRun)];
    let window = open_window_with(cx, root, setup);
    cx.run_until_parked();
    draw(cx, window);
    let panel = cx.update(|cx| cx.global::<ActiveRunPanel>().0.clone());
    wait_for(cx, "a run configuration", |cx| {
        !panel.read_with(cx, |panel, _| panel.state().configs.is_empty())
    });

    double_modifier(
        cx,
        window,
        Modifiers {
            control: true,
            ..Modifiers::default()
        },
    );
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "palette").size.height > px(0.),
        "Double Ctrl opens Run Anything"
    );
    type_text(cx, window, "Echo");
    draw(cx, window);
    press(cx, window, "enter");

    wait_for(cx, "the run", |cx| {
        panel
            .read_with(cx, |panel, _| panel.text())
            .iter()
            .any(|line| line.contains("hello from run"))
    });
    let opened = window
        .read_with(cx, |workspace, _| workspace.tool_open(RUN_TOOL))
        .unwrap();
    assert!(opened, "Run Anything shows the Run tool window");
}

#[ui_kit::gpui::test]
fn local_history_show_label_project_and_revert(cx: &mut TestAppContext) {
    let root = scratch("actions-history");
    let path = root.join("Saved.txt");
    fs::write(&path, "one\n").unwrap();
    let window = open_test_root(cx, root.clone());
    wait_for(cx, "the local history", |cx| {
        cx.update(|cx| crate::history::get(cx).is_some())
    });
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    view.update(cx, |view, cx| {
        view.replace_text(0..3, "two", cx);
        assert!(view.save_blocking(cx).is_none());
    });
    wait_for(cx, "the saved version", |cx| {
        cx.update(|cx| {
            crate::history::get(cx).is_some_and(|history| !history.file_history(&path).is_empty())
        })
    });
    view.update(cx, |view, cx| view.replace_text(0..3, "three", cx));

    // Local History: Put Label asks for the name and records it on the folder.
    find_action(cx, window, "put label");
    assert!(
        bounds_of(cx, window, "prompt").size.height > px(0.),
        "Put Label asks for a name"
    );
    type_text(cx, window, "milestone");
    press(cx, window, "enter");
    wait_for(cx, "the label", |cx| {
        cx.update(|cx| {
            crate::history::get(cx).is_some_and(|history| {
                history
                    .folder_history(&root)
                    .iter()
                    .any(|revision| revision.is_label())
            })
        })
    });

    // Local History: Show History lists the version and reverts the file to it.
    run_command(cx, window, "local_history.show");
    wait_for(cx, "the history window", |cx| {
        cx.update(|cx| crate::history_view::report(cx))
            .iter()
            .any(|line| line.starts_with("history-version"))
    });
    assert_eq!(buffer_of(cx, &view), "three\n");
    cx.update(crate::history_view::revert_shown);
    cx.run_until_parked();
    assert_eq!(buffer_of(cx, &view), "two\n");
    assert_eq!(fs::read_to_string(&path).unwrap(), "two\n");
    press(cx, window, "escape");

    // Local History: Show Project History lists the versions of the folder.
    run_command(cx, window, "local_history.show_project");
    let report = cx.update(|cx| crate::history_view::report(cx));
    assert!(
        report.iter().any(|line| line.contains("Saved.txt")),
        "the project history lists the saved file: {report:?}"
    );
    press(cx, window, "escape");
}

#[ui_kit::gpui::test]
fn every_tool_window_opens_by_its_key_closes_by_shift_escape_and_toggles(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "actions-tools");
    // The project tool starts open: the shortcut must close it first.
    let projected = window
        .read_with(cx, |workspace, _| workspace.tool_open(PROJECT_TOOL))
        .unwrap();
    if projected {
        press(cx, window, "alt-1");
    }

    let keys_and_tools = [
        ("alt-1", PROJECT_TOOL),
        ("alt-2", AI_TOOL),
        ("alt-3", USAGES_TOOL),
        ("alt-4", RUN_TOOL),
        ("alt-5", DEBUG_TOOL),
        ("alt-6", PROBLEMS_TOOL),
        ("alt-7", STRUCTURE_TOOL),
        ("alt-8", SERVICES_TOOL),
        ("alt-9", LOG_TOOL),
        ("alt-0", COMMIT_TOOL),
        ("alt-f12", TERMINAL_TOOL),
    ];
    for (key, tool) in keys_and_tools {
        press(cx, window, key);
        let opened = window
            .read_with(cx, |workspace, _| workspace.tool_open(tool))
            .unwrap();
        assert!(opened, "{key} opens {tool}");

        press(cx, window, "shift-escape");
        let closed = window
            .read_with(cx, |workspace, _| workspace.tool_open(tool))
            .unwrap();
        assert!(!closed, "Shift+Esc closes {tool}");

        press(cx, window, key);
        let opened = window
            .read_with(cx, |workspace, _| workspace.tool_open(tool))
            .unwrap();
        assert!(opened, "{key} opens {tool} again");

        press(cx, window, key);
        let hidden = window
            .read_with(cx, |workspace, _| workspace.tool_open(tool))
            .unwrap();
        assert!(!hidden, "{key} hides {tool} a second time");
    }

    // The tools without an IDEA key open and close the same way from the window.
    for tool in [TODO_TOOL, workspace::TASKS_TOOL] {
        window
            .update(cx, |workspace, window, cx| {
                workspace.show_tool(tool, window, cx)
            })
            .unwrap();
        cx.run_until_parked();
        draw(cx, window);
        press(cx, window, "escape");
        press(cx, window, "shift-escape");
        let closed = window
            .read_with(cx, |workspace, _| workspace.tool_open(tool))
            .unwrap();
        assert!(!closed, "Shift+Esc closes {tool}");
    }
}

#[ui_kit::gpui::test]
fn close_tab_and_reopen_it(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "actions-close");
    let main = root.join("src").join("main.txt");
    let notes = root.join("src").join("notes.txt");
    open_path(cx, window, &main);
    let view = editor_of(cx, window, &main);
    view.update(cx, |view, cx| view.replace_text(0..5, "saved", cx));
    press(cx, window, "ctrl-s");
    wait_for(cx, "the saved file", |_| {
        fs::read_to_string(&main).is_ok_and(|text| text == "saved\nbeta\n")
    });

    open_path(cx, window, &notes);
    assert_eq!(active_path(cx, window), Some(notes.clone()));

    press(cx, window, "ctrl-f4");
    let (tabs, closed) = window
        .read_with(cx, |workspace, _| {
            (workspace.open_tabs(), workspace.closed_tabs().to_vec())
        })
        .unwrap();
    assert_eq!(tabs, vec![vec![main.clone()]]);
    assert_eq!(closed, vec![notes.clone()]);
    assert_eq!(active_path(cx, window), Some(main.clone()));

    run_command(cx, window, "window.reopen_closed_tab");
    let tabs = window
        .read_with(cx, |workspace, _| workspace.open_tabs())
        .unwrap();
    assert_eq!(tabs, vec![vec![main.clone(), notes.clone()]]);
    assert_eq!(active_path(cx, window), Some(notes.clone()));

    run_command(cx, window, "window.next_tab");
    assert_eq!(active_path(cx, window), Some(main.clone()));
    run_command(cx, window, "window.next_tab");
    assert_eq!(active_path(cx, window), Some(notes.clone()));

    // The Find Action row closes the selected tab, as the key does.
    run_command(cx, window, "window.close_tab");
    let tabs = window
        .read_with(cx, |workspace, _| workspace.open_tabs())
        .unwrap();
    assert_eq!(tabs, vec![vec![main]]);
}

#[ui_kit::gpui::test]
fn show_performance_monitor_from_find_action(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "actions-performance");
    find_action(cx, window, "performance monitor");
    let opened = window
        .read_with(cx, |workspace, _| workspace.tool_open(PERFORMANCE_TOOL))
        .unwrap();
    assert!(opened, "Show Performance Monitor opens the tool window");
    press(cx, window, "shift-escape");
    let closed = window
        .read_with(cx, |workspace, _| workspace.tool_open(PERFORMANCE_TOOL))
        .unwrap();
    assert!(!closed);
}

/// A repository that is in the middle of a conflicted merge.
pub(crate) fn conflicting_repository(name: &str) -> PathBuf {
    let root = scratch(name);
    let git = vcs::GitRunner::from_env().expect("git is on the PATH");
    git.run(&root, ["init"]).unwrap();
    git.run(&root, ["config", "user.email", "test@example.com"])
        .unwrap();
    git.run(&root, ["config", "user.name", "Window Test"])
        .unwrap();
    fs::write(root.join("main.txt"), "base\n").unwrap();
    git.run(&root, ["add", "main.txt"]).unwrap();
    git.run(&root, ["commit", "-m", "base"]).unwrap();
    let branch = String::from_utf8_lossy(
        &git.run(&root, ["rev-parse", "--abbrev-ref", "HEAD"])
            .unwrap(),
    )
    .trim()
    .to_string();
    git.run(&root, ["checkout", "-b", "other"]).unwrap();
    fs::write(root.join("main.txt"), "other\n").unwrap();
    git.run(&root, ["add", "main.txt"]).unwrap();
    git.run(&root, ["commit", "-m", "other"]).unwrap();
    git.run(&root, ["checkout", &branch]).unwrap();
    fs::write(root.join("main.txt"), "local\n").unwrap();
    git.run(&root, ["add", "main.txt"]).unwrap();
    git.run(&root, ["commit", "-m", "local"]).unwrap();
    let output = git
        .command(&root)
        .args(["merge", "other"])
        .output()
        .expect("git merge runs");
    assert!(!output.status.success(), "the merge leaves a conflict");
    root
}

#[ui_kit::gpui::test]
fn git_log_and_conflicts_open_from_find_action(cx: &mut TestAppContext) {
    // Conflicts... lists the files of the merge in progress.
    let root = conflicting_repository("actions-conflicts");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    draw(cx, window);
    wait_for(cx, "the repository", |cx| {
        cx.read(|cx| {
            cx.global::<crate::git::ActiveGit>()
                .0
                .read(cx)
                .repo()
                .is_some()
        })
    });
    find_action(cx, window, "conflicts");
    wait_for(cx, "the conflicts window", |cx| {
        has_selector(cx, window, "conflict-row-0")
    });
    assert!(has_selector(cx, window, "conflicts"));
    press(cx, window, "escape");
    cx.run_until_parked();

    // Git Log shows the commits of another repository.
    let root = git_repository("actions-log");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    draw(cx, window);
    wait_for(cx, "the repository", |cx| {
        cx.read(|cx| {
            cx.global::<crate::git::ActiveGit>()
                .0
                .read(cx)
                .repo()
                .is_some()
        })
    });
    press(cx, window, "ctrl-k");
    let commit_open = window
        .read_with(cx, |workspace, _| workspace.tool_open(COMMIT_TOOL))
        .unwrap();
    assert!(commit_open, "Ctrl+K opens the Commit tool window");
    press(cx, window, "escape");
    find_action(cx, window, "git log");
    wait_for(cx, "the log", |cx| {
        cx.read(|cx| !cx.global::<ActiveGitLog>().0.read(cx).commits().is_empty())
    });
    let opened = window
        .read_with(cx, |workspace, _| workspace.tool_open(LOG_TOOL))
        .unwrap();
    assert!(opened, "Git Log shows the Log tool window");
    let subjects: Vec<String> = cx.read(|cx| {
        cx.global::<ActiveGitLog>()
            .0
            .read(cx)
            .commits()
            .iter()
            .map(|commit| commit.subject.clone())
            .collect()
    });
    assert_eq!(subjects, ["base"]);
}

/// The events of one test class with a passing and a failing case.
const TEST_EVENTS: &[&str] = &[
    r#"{"event":"plan"}"#,
    r#"{"event":"started","id":"e","name":"Engine","kind":"suite"}"#,
    r#"{"event":"started","id":"e/c","parent":"e","name":"Vitest","kind":"suite","suite":"demo.Vitest"}"#,
    r#"{"event":"started","id":"e/c/passes","parent":"e/c","name":"passes()","kind":"case","suite":"demo.Vitest","case":"passes"}"#,
    r#"{"event":"finished","id":"e/c/passes","outcome":"passed","durationMs":2}"#,
    r#"{"event":"started","id":"e/c/fails","parent":"e/c","name":"fails()","kind":"case","suite":"demo.Vitest","case":"fails"}"#,
    r#"{"event":"finished","id":"e/c/fails","outcome":"failed","message":"expected 1, got 2","trace":"AssertionFailedError: expected\n\tat demo.Vitest.fails(Vitest.code:2)","durationMs":3}"#,
    r#"{"event":"planFinished"}"#,
];

/// A test provider that plays the listener from a thread and keeps every request.
struct EventTests {
    requests: Arc<Mutex<Vec<TestRequest>>>,
}

impl TestProvider for EventTests {
    fn plan(
        &self,
        _: &project_model::ProjectModel,
        request: &TestRequest,
        listen: &TestListen,
    ) -> base::Result<RunPlan> {
        self.requests
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(request.clone());
        let listen = listen.clone();
        let Ok(mut stream) = TcpStream::connect(("127.0.0.1", listen.port)) else {
            return Ok(RunPlan {
                before: None,
                program: ProcessSpec::new("cmd").args(["/d", "/c", "exit", "0"]),
                grace: std::time::Duration::from_secs(1),
            });
        };
        let _ = writeln!(stream, "{}", listen.token);
        std::thread::spawn(move || {
            for line in TEST_EVENTS {
                if writeln!(stream, "{line}").is_err() {
                    return;
                }
            }
        });
        Ok(RunPlan {
            before: None,
            program: ProcessSpec::new("cmd").args(["/d", "/c", "exit", "0"]),
            grace: std::time::Duration::from_secs(1),
        })
    }
}

fn find_tests(text: &str, _: &Path) -> Vec<TestSite> {
    text.lines()
        .position(|line| line.contains("void fails"))
        .map(|index| {
            vec![TestSite {
                suite: "demo.Vitest".into(),
                case: Some("fails".into()),
                line: index as u32 + 1,
            }]
        })
        .unwrap_or_default()
}

fn test_source(root: &Path) -> PathBuf {
    let directory = root.join("src").join("demo");
    fs::create_dir_all(&directory).unwrap();
    let path = directory.join("Vitest.code");
    fs::write(
        &path,
        "class Vitest {\n    void fails() {\n        check();\n    }\n}\n",
    )
    .unwrap();
    path
}

#[ui_kit::gpui::test]
fn rerun_failed_tests_from_find_action_runs_only_the_failures(cx: &mut TestAppContext) {
    crate::tree_ops::use_temporary_delete();
    let root = sample_project("actions-rerun");
    let source = test_source(&root);
    let requests = Arc::new(Mutex::new(Vec::new()));
    let mut setup = setup(&root);
    setup.tests = Some(Arc::new(EventTests {
        requests: requests.clone(),
    }));
    setup.test_sites = find_tests;
    let window = open_window_with(cx, root.clone(), setup);
    cx.run_until_parked();
    draw(cx, window);
    let panel = cx.update(|cx| cx.global::<ActiveRunPanel>().0.clone());

    open_path(cx, window, &source);
    let view = editor_of(cx, window, &source);
    view.update(cx, |view, cx| view.restore_caret(1, 0, cx));
    draw(cx, window);
    press(cx, window, "ctrl-shift-f10");
    wait_for_process(cx, "the first run", |cx| {
        requests
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .len()
            == 1
            && panel.read_with(cx, |panel, _| panel.idle())
    });
    let first = requests.lock().unwrap_or_else(|error| error.into_inner())[0].clone();
    assert_eq!(first.case.as_deref(), Some("fails"));

    find_action(cx, window, "rerun failed tests");
    wait_for_process(cx, "the rerun", |cx| {
        requests
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .len()
            == 2
            && panel.read_with(cx, |panel, _| panel.idle())
    });
    let second = requests.lock().unwrap_or_else(|error| error.into_inner())[1].clone();
    assert_eq!(
        second.cases,
        vec![("demo.Vitest".to_owned(), "fails".to_owned())],
        "Rerun Failed Tests runs only the failed case"
    );
    let opened = window
        .read_with(cx, |workspace, _| workspace.tool_open(RUN_TOOL))
        .unwrap();
    assert!(opened, "Rerun Failed Tests shows the Run tool window");
}

/// A trial debug adapter: it starts the mock adapter of the fixture language and keeps
/// the endpoint Attach to JVM asked it for.
struct TrialAdapter {
    runtime: tokio::runtime::Runtime,
    attached: Mutex<Vec<(String, u16)>>,
    server: Mutex<Option<lang_fixture::MockDap>>,
}

impl TrialAdapter {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            runtime: tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("the test runtime starts"),
            attached: Mutex::new(Vec::new()),
            server: Mutex::new(None),
        })
    }

    fn endpoints(&self) -> Vec<(String, u16)> {
        self.attached
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    fn join(&self) {
        if let Some(server) = self
            .server
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
        {
            server.join();
        }
    }
}

impl DebugService for TrialAdapter {
    fn start(
        &self,
        _program: ProcessSpec,
        _name: String,
        _entry: Option<PathBuf>,
        _breakpoints: Vec<BreakpointSpec>,
        _watch: Vec<String>,
        _exception_filters: Vec<String>,
    ) -> Result<Debugger, String> {
        Err("this adapter only attaches".to_owned())
    }

    fn attach(
        &self,
        host: String,
        port: u16,
        name: String,
        _project: Option<String>,
        breakpoints: Vec<BreakpointSpec>,
        watch: Vec<String>,
        exception_filters: Vec<String>,
    ) -> Result<Debugger, String> {
        let server = lang_fixture::MockDap::start().map_err(|error| error.to_string())?;
        let address = server.address().to_owned();
        *self
            .server
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(server);
        self.attached
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push((host.clone(), port));
        let (commands, receiver) = tokio::sync::mpsc::unbounded_channel();
        let (snapshots, incoming) = std::sync::mpsc::channel();
        let launch = dap_client::Launch {
            address,
            adapter_id: "fixture".into(),
            name,
            attach_host: Some(host),
            attach_port: Some(port),
            breakpoints,
            exception_filters,
            watch,
            ..dap_client::Launch::default()
        };
        self.runtime.spawn(async move {
            let _ = dap_client::run(launch, receiver, snapshots).await;
        });
        Ok(Debugger::new(commands, incoming))
    }
}

#[ui_kit::gpui::test]
fn attach_to_jvm_asks_for_the_endpoint_and_starts_a_trial_adapter(cx: &mut TestAppContext) {
    let root = sample_project("actions-attach");
    let adapter = TrialAdapter::new();
    let mut setup = setup(&root);
    setup.debug = Some(adapter.clone());
    let window = open_window_with(cx, root, setup);
    cx.run_until_parked();
    draw(cx, window);

    find_action(cx, window, "attach to jvm");
    assert!(
        bounds_of(cx, window, "prompt").size.height > px(0.),
        "Attach to JVM asks for host:port"
    );
    // The prompt starts with the example endpoint: clear it before typing.
    press(
        cx,
        window,
        "backspace backspace backspace backspace backspace backspace backspace backspace backspace backspace backspace backspace backspace backspace",
    );
    type_text(cx, window, "127.0.0.1:5005");
    press(cx, window, "enter");

    wait_for(cx, "the attachment", |_| !adapter.endpoints().is_empty());
    assert_eq!(
        adapter.endpoints(),
        vec![("127.0.0.1".to_owned(), 5005)],
        "the prompt endpoint reaches the adapter"
    );
    let opened = window
        .read_with(cx, |workspace, _| workspace.tool_open(DEBUG_TOOL))
        .unwrap();
    assert!(opened, "Attach to JVM shows the Debug tool window");
    wait_for(cx, "the stopped session", |cx| {
        let panel = cx.read(|cx| cx.global::<ActiveDebug>().0.clone());
        matches!(
            panel.read_with(cx, |panel, _| panel.snapshot().state.clone()),
            dap_client::DebugState::Stopped { .. }
        )
    });
    press(cx, window, "ctrl-f2");
    wait_for(cx, "the end of the session", |cx| {
        let panel = cx.read(|cx| cx.global::<ActiveDebug>().0.clone());
        panel.read_with(cx, |panel, _| panel.snapshot().state.clone())
            == dap_client::DebugState::Terminated
    });
    adapter.join();
}

/// Every command Find Action lists runs, and every setting it lists opens.
#[ui_kit::gpui::test]
fn every_find_action_item_opens_without_a_panic(cx: &mut TestAppContext) {
    let root = navigation_project("actions-all");
    let window = open_test_root(cx, root.clone());
    let sample = root.join("src").join("Sample.txt");
    open_path(cx, window, &sample);

    let ids: Vec<&'static str> = window
        .read_with(cx, |workspace, _| {
            workspace
                .commands()
                .all()
                .map(|command| command.id)
                .collect()
        })
        .unwrap();
    assert!(ids.len() > 80, "the window has its commands: {}", ids.len());

    for id in ids {
        let ran = window
            .update(cx, |workspace, window, cx| {
                if workspace.active_view().is_none() {
                    workspace.open_file(sample.clone(), window, cx);
                }
                let ran = workspace.run_command(id, window, cx);
                workspace.dismiss(window, cx);
                ran
            })
            .unwrap();
        assert!(ran, "{id} runs");
        cx.run_until_parked();
        draw(cx, window);
    }

    for setting in crate::settings_catalog::all() {
        find_action(cx, window, setting.title);
        window
            .update(cx, |workspace, window, cx| workspace.dismiss(window, cx))
            .unwrap();
        cx.run_until_parked();
        draw(cx, window);
    }
}
