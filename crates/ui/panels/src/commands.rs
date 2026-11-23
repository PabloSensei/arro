//! The commands of the window: what the keymap and Find Action run by id.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use editor_view::{EditorView, Semantic};
use keymap::{editor as editor_keys, workspace as keys};
use lang_api::DebugProcess;
use search::Tab;
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::{
    DATABASE_TOOL, DEBUG_TOOL, DOCUMENTATION_TOOL, DockSide, RUN_TOOL, TERMINAL_TOOL, Workspace,
};

use crate::agents::ASK_AI;
use crate::choice::{ChoiceDialog, DialogChoice};
use crate::database_panel::ActiveDatabase;
use crate::debug::{ActiveDebug, DebugPanel};
use crate::diff_view::DiffView;
use crate::documentation::ActiveDocumentation;
use crate::finder::{ActiveFinder, Finder};
use crate::git::ActiveGit;
use crate::git_log::{self, ActiveGitLog};
use crate::history_view::HistoryView;
use crate::index::{ActiveIndex, ProjectIndex};
use crate::marks;
use crate::navigation;
use crate::palette::{Mode, Palette};
use crate::places;
use crate::problems::ActiveProblems;
use crate::project::ActiveProjectPanel;
use crate::project_structure::ProjectStructureDialog;
use crate::prompt::Prompt;
use crate::run::RunPanel;
use crate::run_configs::RunConfigDialog;
use crate::script::{ActiveCommitPanel, ActiveRunPanel};
use crate::stack_trace_dialog::StackTraceDialog;
use crate::structural::StructuralSearch;
use crate::terminal_panel::ActiveTerminal;
use crate::todo::ActiveTodo;
use crate::usages::{self, ActiveUsages};

/// One rule of the grouping or the filters that changes the settings.
type SettingChange = fn(&mut workspace::Settings);

/// `editor.move_line_up` reads `Move line up`.
fn title_of(id: &str) -> String {
    let words = id.rsplit('.').next().unwrap_or(id).replace('_', " ");
    let mut chars = words.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// The panel of the Run tool window, when its console waits for a line and has the
/// focus: the console keys act on what the user types, nowhere else.
fn console_panel(cx: &App, window: &Window) -> Option<Entity<RunPanel>> {
    let panel = cx.try_global::<ActiveRunPanel>()?.0.clone();
    let ready = {
        let panel = panel.read(cx);
        panel.takes_input() && panel.focus().contains_focused(window, cx)
    };
    ready.then_some(panel)
}

pub fn register(
    workspace: &mut Workspace,
    project: &ProjectIndex,
    files: &Entity<Arc<Vec<PathBuf>>>,
) {
    let palettes = [
        (keys::GOTO_FILE, "Go to File", Mode::Everywhere(Tab::Files)),
        (
            keys::GOTO_CLASS,
            "Go to Class",
            Mode::Everywhere(Tab::Classes),
        ),
        (
            keys::GOTO_SYMBOL,
            "Go to Symbol",
            Mode::Everywhere(Tab::Symbols),
        ),
        (
            keys::SEARCH_EVERYWHERE,
            "Search Everywhere",
            Mode::Everywhere(Tab::All),
        ),
        (
            keys::FIND_ACTION,
            "Find Action",
            Mode::Everywhere(Tab::Actions),
        ),
        (keys::RUN_ANYTHING, "Run Anything", Mode::RunAnything),
        (keys::RECENT_FILES, "Recent Files", Mode::Recent),
        (
            keys::RECENT_CHANGED_FILES,
            "Recently Changed Files",
            Mode::RecentChanged,
        ),
        (keys::SWITCHER, "Switcher", Mode::Switcher),
        (keys::FILE_STRUCTURE, "File Structure", Mode::Structure),
    ];
    for (id, title, mode) in palettes {
        let (project, files) = (project.clone(), files.clone());
        workspace.register_command(id, title, move |workspace, window, cx| {
            Palette::show(mode, project.clone(), files.clone(), workspace, window, cx)
        });
    }

    for (id, title, replace) in [
        (keys::FIND_IN_FILES, "Find in Files", false),
        (keys::REPLACE_IN_FILES, "Replace in Files", true),
    ] {
        let (project, files) = (project.clone(), files.clone());
        workspace.register_command(id, title, move |workspace, window, cx| {
            let root = project.root.clone();
            Finder::show(replace, files.clone(), root, workspace, window, cx)
        });
    }

    for (id, title, replace) in [
        (
            "workspace.search_structurally",
            "Search Structurally",
            false,
        ),
        (
            "workspace.replace_structurally",
            "Replace Structurally",
            true,
        ),
    ] {
        let (project, files) = (project.clone(), files.clone());
        workspace.register_command(id, title, move |workspace, window, cx| {
            let root = project.root.clone();
            StructuralSearch::show(replace, files.clone(), root, workspace, window, cx)
        });
    }

    workspace.register_command(keys::RUN_CLASS, "Run Test at Caret", marks::run_at_caret);
    workspace.register_command(
        keys::RUN_IN_TERMINAL,
        "Run Selection in Terminal",
        run_selection_in_terminal,
    );
    for (id, title) in [
        (keys::CONSOLE_EXECUTE, "Execute Console Command"),
        (keys::CONSOLE_EXECUTE_MULTILINE, "Execute Console Command"),
    ] {
        workspace.register_command(id, title, |_, window, cx| {
            if let Some(panel) = console_panel(cx, window) {
                panel.update(cx, |panel, cx| panel.execute_input(cx));
            }
        });
    }
    workspace.register_command(keys::SEND_EOF, "Send EOF", |_, window, cx| {
        if let Some(panel) = console_panel(cx, window) {
            panel.update(cx, |panel, cx| panel.send_eof(cx));
        }
    });
    workspace.register_command(
        keys::CONSOLE_HISTORY_BROWSE,
        "Console History",
        |_, window, cx| {
            if let Some(panel) = console_panel(cx, window) {
                panel.update(cx, |panel, cx| panel.browse_history(true, cx));
            }
        },
    );
    workspace.register_command(
        keys::CONSOLE_OPEN,
        "Open Console",
        |workspace, window, cx| workspace.show_tool(RUN_TOOL, window, cx),
    );
    workspace.register_command(
        keys::BUILD_PROJECT,
        "Build Project",
        |workspace, window, cx| workspace.build_project(window, cx),
    );
    workspace.register_command(
        "workspace.build_module",
        "Build Module",
        |workspace, window, cx| {
            let path = current_path(workspace, window, cx);
            workspace.build_module(path, window, cx);
        },
    );
    workspace.register_command(
        "workspace.rebuild_project",
        "Rebuild Project",
        |workspace, window, cx| workspace.rebuild_project(window, cx),
    );
    workspace.register_command(
        "workspace.generate_sources",
        "Generate Sources and Update Folders",
        |workspace, window, cx| workspace.generate_sources(window, cx),
    );
    workspace.register_command(
        "workspace.profile",
        "Profile with Profiler",
        |workspace, window, cx| workspace.run_with_profiler(window, cx),
    );
    workspace.register_command(
        "workspace.analyze_stack_trace",
        "Analyze Stack Trace",
        StackTraceDialog::show,
    );
    workspace.register_command(
        "workspace.compare_with_clipboard",
        "Compare with Clipboard",
        compare_with_clipboard,
    );
    workspace.register_command("http.copy_curl", "Copy as cURL", |workspace, window, cx| {
        crate::http::copy_as_curl_active(::http_client::Shell::Bash, workspace, window, cx)
    });
    workspace.register_command(
        "http.copy_curl_cmd",
        "Copy as cURL (cmd)",
        |workspace, window, cx| {
            crate::http::copy_as_curl_active(::http_client::Shell::Cmd, workspace, window, cx)
        },
    );
    workspace.register_command("workspace.compile", "Recompile", |workspace, window, cx| {
        let path = current_path(workspace, window, cx);
        workspace.compile_path(path, window, cx);
    });
    workspace.register_command("debug.attach_jvm", "Attach to JVM…", attach_to_jvm);
    workspace.register_command("debug.mute_breakpoints", "Mute Breakpoints", |_, _, cx| {
        if let Some(panel) = cx.try_global::<ActiveDebug>() {
            let panel = panel.0.clone();
            panel.update(cx, |panel, cx| panel.toggle_mute(cx));
        }
    });
    // The link of a failed build: the Debug window opens at its Console.
    workspace.register_command(
        "debug.show_console",
        "Show Debug Console",
        |workspace, window, cx| {
            workspace.show_tool(DEBUG_TOOL, window, cx);
            if let Some(panel) = cx.try_global::<ActiveDebug>() {
                let panel = panel.0.clone();
                panel.update(cx, |panel, cx| panel.show_console(cx));
            }
        },
    );
    workspace.register_command("window.database", "Database", |workspace, window, cx| {
        workspace.toggle_tool(DATABASE_TOOL, window, cx)
    });
    workspace.register_command(
        "database.data_sources",
        "Data Sources…",
        |_, window, cx| {
            let Some(panel) = cx
                .try_global::<ActiveDatabase>()
                .map(|panel| panel.0.clone())
            else {
                return;
            };
            panel.update(cx, |panel, cx| panel.open_data_sources_from(window, cx));
        },
    );
    workspace.register_command(
        "run.edit_configurations",
        "Edit Configurations…",
        |workspace, window, cx| {
            let panel = cx.global::<ActiveRunPanel>().0.clone();
            RunConfigDialog::show(panel, workspace, window, cx);
        },
    );
    workspace.register_command(
        "project.structure",
        "Project Structure...",
        ProjectStructureDialog::show,
    );
    workspace.register_command(
        "window.rerun_failed_tests",
        "Rerun Failed Tests",
        marks::rerun_failed,
    );
    workspace.register_command(
        keys::RECENT_LOCATIONS,
        "Recent Locations",
        places::show_recent,
    );
    workspace.register_command(
        keys::RECENT_CHANGES,
        "Recent Changes",
        crate::recent_changes::show,
    );
    workspace.register_command(
        "window.show_bookmarks",
        "Bookmarks",
        crate::bookmarks::show_window,
    );
    workspace.register_command(keys::BACK, "Back", navigation::back);
    workspace.register_command(keys::FORWARD, "Forward", navigation::forward);
    workspace.register_command(keys::SHOW_CONTENT, "Show Content", show_content);

    for action in editor_keys::IDEA_IDS
        .iter()
        .chain(editor_keys::HANDLED_IDS)
        .map(|(_, action)| *action)
    {
        workspace.register_command(action, title_of(action), move |workspace, _, cx| {
            let view = workspace.active_view();
            if let Some(view) = view.and_then(|view| view.downcast::<EditorView>().ok()) {
                view.update(cx, |view, cx| view.run(action, cx));
            }
        });
    }

    // The refactorings of the members have no key in `idea-default.json`, so their
    // rows name the command and the run of the editor action directly.
    for (action, title) in [
        (editor_keys::PULL_MEMBERS_UP, "Pull Members Up..."),
        (editor_keys::PUSH_MEMBERS_DOWN, "Push Members Down..."),
        (editor_keys::EXTRACT_SUPERCLASS, "Extract Superclass..."),
        (
            editor_keys::INTRODUCE_PARAMETER_OBJECT,
            "Introduce Parameter Object...",
        ),
    ] {
        workspace.register_command(action, title, move |workspace, _, cx| {
            let view = workspace.active_view();
            if let Some(view) = view.and_then(|view| view.downcast::<EditorView>().ok()) {
                view.update(cx, |view, cx| view.run(action, cx));
            }
        });
    }

    for (action, title) in [
        (editor_keys::TOGGLE_SOFT_WRAP, "Soft-Wrap"),
        (editor_keys::TOGGLE_WHITESPACE, "Show Whitespaces"),
    ] {
        workspace.register_command(action, title, move |workspace, _, cx| {
            workspace.update_settings(
                |settings| match action {
                    editor_keys::TOGGLE_SOFT_WRAP => settings.soft_wrap = !settings.soft_wrap,
                    _ => settings.show_whitespace = !settings.show_whitespace,
                },
                cx,
            );
        });
    }
    // View | Active Editor: the switch of this view alone, until it closes.
    for (id, title, action) in [
        (
            "editor.soft_wrap_in_editor",
            "Soft-Wrap in This Editor",
            editor_keys::TOGGLE_SOFT_WRAP,
        ),
        (
            "editor.show_whitespace_in_editor",
            "Show Whitespaces in This Editor",
            editor_keys::TOGGLE_WHITESPACE,
        ),
    ] {
        workspace.register_command(id, title, move |workspace, _, cx| {
            let view = workspace.active_view();
            if let Some(view) = view.and_then(|view| view.downcast::<EditorView>().ok()) {
                view.update(cx, |view, cx| view.run(action, cx));
            }
        });
    }
    workspace.register_command(
        "local_history.show",
        "Local History: Show History",
        |workspace, window, cx| {
            let path = workspace.active_editor().map(|(path, _)| path);
            let path = path.unwrap_or_else(|| workspace.root().to_path_buf());
            HistoryView::show(path, workspace, window, cx)
        },
    );
    workspace.register_command(
        "local_history.show_project",
        "Local History: Show Project History",
        |workspace, window, cx| {
            let root = workspace.root().to_path_buf();
            HistoryView::show(root, workspace, window, cx)
        },
    );
    workspace.register_command(
        "local_history.put_label",
        "Local History: Put Label",
        |workspace, window, cx| {
            let root = workspace.root().to_path_buf();
            let submit = move |label: String, cx: &mut App| {
                let label = label.trim().to_string();
                if !label.is_empty() {
                    crate::history::put_label(&root, label, cx);
                }
            };
            Prompt::show("Put Label", "Label name", submit, workspace, window, cx)
        },
    );

    register_git_commands(workspace);

    workspace.register_command("window.toggle_theme", "Toggle Theme", |workspace, _, cx| {
        workspace.toggle_theme(cx)
    });
    let panels = [
        (
            "window.toggle_project",
            "Project Tool Window",
            DockSide::Left,
        ),
        (
            "window.toggle_terminal",
            "Terminal Tool Window",
            DockSide::Bottom,
        ),
        (
            "window.toggle_assistant",
            "AI Assistant Tool Window",
            DockSide::Right,
        ),
    ];
    for (id, title, side) in panels {
        workspace.register_command(id, title, move |workspace, window, cx| {
            workspace.toggle_panel(side, window, cx)
        });
    }
    // Ask AI about the selection: a key of its own, clear of the IDEA keymap.
    workspace.register_command(ASK_AI, "Ask AI...", crate::agents::ask_ai);
    if let Err(error) = workspace.bind_command("ctrl-alt-shift-a", ASK_AI) {
        eprintln!("keymap: {ASK_AI}: {error}");
    }
    // Explain Plan of the SQL console: Ctrl+Shift+E is Recent Locations in the keymap.
    workspace.register_command(
        crate::database_panel::EXPLAIN_PLAN,
        "Explain Plan",
        crate::database_panel::explain_plan,
    );
    workspace.register_command(
        crate::database_panel::RUN_SQL_SCRIPT,
        "Run SQL Script",
        crate::database_panel::run_script,
    );
    if let Err(error) = workspace.bind_command(
        crate::database_panel::EXPLAIN_KEY,
        crate::database_panel::EXPLAIN_PLAN,
    ) {
        eprintln!("keymap: {}: {error}", crate::database_panel::EXPLAIN_PLAN);
    }
    workspace.register_command(
        "window.show_performance",
        "Show Performance Monitor",
        |workspace, window, cx| workspace.show_tool(workspace::PERFORMANCE_TOOL, window, cx),
    );
    workspace.register_command(
        "window.show_documentation",
        "Documentation",
        |workspace, window, cx| {
            workspace.show_tool(DOCUMENTATION_TOOL, window, cx);
            let editor = workspace
                .active_view()
                .and_then(|view| view.downcast::<EditorView>().ok());
            if let Some(editor) = editor
                && let Some(panel) = cx.try_global::<ActiveDocumentation>()
            {
                let panel = panel.0.clone();
                panel.update(cx, |panel, cx| panel.show_editor(editor, cx));
            }
        },
    );
    workspace.register_command("window.close_tab", "Close Tab", |workspace, window, cx| {
        workspace.close_active_tab(window, cx)
    });
    workspace.register_command(
        "window.reopen_closed_tab",
        "Reopen Closed Tab",
        |workspace, window, cx| workspace.reopen_closed_tab(window, cx),
    );
    workspace.register_command(
        "window.close_other_tabs",
        "Close Other Tabs",
        |workspace, window, cx| workspace.close_other_tabs_in_active_group(window, cx),
    );
    workspace.register_command(
        "window.close_all_tabs",
        "Close All Tabs",
        |workspace, window, cx| workspace.close_all_tabs_in_active_group(window, cx),
    );
    workspace.register_command(
        "window.close_unmodified_tabs",
        "Close Unmodified Tabs",
        |workspace, window, cx| workspace.close_unmodified_tabs(window, cx),
    );
    workspace.register_command(
        "window.close_tabs_to_left",
        "Close Tabs to the Left",
        |workspace, window, cx| workspace.close_tabs_to_left(window, cx),
    );
    workspace.register_command(
        "window.close_tabs_to_right",
        "Close Tabs to the Right",
        |workspace, window, cx| workspace.close_tabs_to_right(window, cx),
    );
    workspace.register_command(
        "workspace.store_default_layout",
        "Store Current Layout as Default",
        |workspace, _, cx| workspace.store_default_layout(cx),
    );
    workspace.register_command("file.open", "Open...", open_paths);
    workspace.register_command(
        "file.new_project",
        "New Project...",
        |workspace, _window, cx| {
            crate::start::open_window(workspace.state_dir().to_path_buf(), cx);
        },
    );
    workspace.register_command(
        "file.reload_all",
        "Reload All from Disk",
        reload_all_from_disk,
    );
    workspace.register_command(
        "file.invalidate_caches",
        "Invalidate Caches...",
        crate::invalidate::InvalidateDialog::show,
    );
    workspace.register_command("app.exit", "Exit", exit);
    for (id, title) in [
        (editor_keys::SORT_LINES, "Sort Lines"),
        (editor_keys::REVERSE_LINES, "Reverse Lines"),
        (
            editor_keys::CONVERT_INDENTS_TO_SPACES,
            "Convert Indents to Spaces",
        ),
        (
            editor_keys::CONVERT_INDENTS_TO_TABS,
            "Convert Indents to Tabs",
        ),
        (editor_keys::SORT_MEMBERS, "Sort Members"),
        (editor_keys::ADD_FINAL_MODIFIERS, "Add final Modifiers"),
    ] {
        workspace.register_command(id, title, move |workspace, _, cx| {
            if let Some(view) = workspace
                .active_view()
                .and_then(|view| view.downcast::<EditorView>().ok())
            {
                view.update(cx, |view, cx| view.run(id, cx));
            }
        });
    }
    workspace.register_command(
        "editor.toggle_read_only",
        "Toggle Read-Only Attribute",
        toggle_read_only,
    );
    workspace.register_command("window.next_tab", "Next Tab", |workspace, window, cx| {
        workspace.next_tab(window, cx)
    });
    workspace.register_command(
        "window.split_right",
        "Split Right",
        |workspace, window, cx| workspace.split_right(window, cx),
    );
    workspace.register_command("window.unsplit", "Unsplit", |workspace, window, cx| {
        workspace.unsplit(window, cx)
    });

    let test_files = files.clone();
    workspace.register_command(
        keys::GOTO_TEST,
        "Go to Test",
        move |workspace, window, cx| {
            goto_test(workspace, &test_files, window, cx);
        },
    );
    let related_files = files.clone();
    workspace.register_command(
        "workspace.goto_related",
        "Go to Related",
        move |workspace, window, cx| {
            goto_related(workspace, &related_files, window, cx);
        },
    );
    workspace.register_command(
        "workspace.new_element_same_place",
        "New in Same Place",
        new_element_same_place,
    );
    workspace.register_command("workspace.copy_element", "Copy Element", copy_element);
    workspace.register_command(keys::COPY_PATHS, "Copy Path", |workspace, window, cx| {
        copy_paths(workspace, window, cx);
    });
    workspace.register_command(
        keys::ROLLBACK_CHANGED_LINES,
        "Rollback Lines",
        |_, _, cx| {
            let Some(git) = cx.try_global::<ActiveGit>().map(|git| git.0.clone()) else {
                return;
            };
            // The key runs inside the update of the window; the repository reads it after.
            cx.defer(move |cx| git.update(cx, |git, cx| git.revert_at_caret(cx)));
        },
    );
    workspace.register_command(keys::NEXT_CHANGE, "Show Next Change", |_, _, cx| {
        let Some(git) = cx.try_global::<ActiveGit>().map(|git| git.0.clone()) else {
            return;
        };
        cx.defer(move |cx| git.update(cx, |git, cx| git.show_next_change(cx)));
    });
    workspace.register_command(keys::PREVIOUS_CHANGE, "Show Previous Change", |_, _, cx| {
        let Some(git) = cx.try_global::<ActiveGit>().map(|git| git.0.clone()) else {
            return;
        };
        cx.defer(move |cx| git.update(cx, |git, cx| git.show_previous_change(cx)));
    });
    workspace.register_command(
        "workspace.synchronize",
        "Synchronize",
        |workspace, window, cx| {
            reload_all_now(workspace, window, cx);
        },
    );
    workspace.register_command(
        "git.quick_list",
        "VCS Operations...",
        |workspace, window, cx| {
            let Some(git) = cx.try_global::<ActiveGit>().map(|git| git.0.clone()) else {
                return;
            };
            let position = popup_anchor(workspace, window, cx);
            cx.defer(move |cx| git.update(cx, |git, cx| git.quick_list(position, cx)));
        },
    );
    workspace.register_command("app.about", "About Arro", |workspace, window, cx| {
        workspace.show_about(window, cx)
    });
    workspace.register_command(
        "app.check_updates",
        "Check for Updates",
        |workspace, _, cx| workspace.check_updates(cx),
    );
    workspace.register_command("app.support", "Support Arro", |_, _, _| {
        ::platform::open_url("https://github.com/sponsors/PabloSensei");
    });
    workspace.register_command("app.report_problem", "Report a Problem", |_, _, _| {
        ::platform::open_url("https://github.com/PabloSensei/arro/issues/new");
    });
    register_tool_commands(workspace);
}

fn set_usage_type(settings: &mut workspace::Settings) {
    settings.usage_group_usage_type = !settings.usage_group_usage_type;
}
fn set_usage_module(settings: &mut workspace::Settings) {
    settings.usage_group_module = !settings.usage_group_module;
}
fn set_usage_directory(settings: &mut workspace::Settings) {
    settings.usage_group_directory = !settings.usage_group_directory;
}
fn set_usage_directory_structure(settings: &mut workspace::Settings) {
    settings.usage_group_directory_structure = !settings.usage_group_directory_structure;
}
fn set_usage_file_structure(settings: &mut workspace::Settings) {
    settings.usage_group_file_structure = !settings.usage_group_file_structure;
}
fn set_usage_flatten_modules(settings: &mut workspace::Settings) {
    settings.usage_flatten_modules = !settings.usage_flatten_modules;
}
fn set_usage_read(settings: &mut workspace::Settings) {
    settings.usage_show_read = !settings.usage_show_read;
}
fn set_usage_write(settings: &mut workspace::Settings) {
    settings.usage_show_write = !settings.usage_show_write;
}
fn set_usage_imports(settings: &mut workspace::Settings) {
    settings.usage_show_imports = !settings.usage_show_imports;
}
fn set_todo_modules(settings: &mut workspace::Settings) {
    settings.todo_group_modules = !settings.todo_group_modules;
}
fn set_todo_packages(settings: &mut workspace::Settings) {
    settings.todo_group_packages = !settings.todo_group_packages;
}
fn set_todo_flatten(settings: &mut workspace::Settings) {
    settings.todo_flatten_packages = !settings.todo_flatten_packages;
}

/// Alt+Down: the tabs of the tool window in front, as a popup. The tool windows with
/// tabs are Run, Usages and Commit.
fn show_content(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let tools = [RUN_TOOL, workspace::USAGES_TOOL, workspace::COMMIT_TOOL];
    let tool = workspace
        .focused_tool_id(window, cx)
        .filter(|id| tools.contains(id))
        .or_else(|| tools.into_iter().find(|id| workspace.tool_open(id)));
    let Some(tool) = tool else {
        return;
    };
    let entries = match tool {
        RUN_TOOL => run_content(cx),
        workspace::USAGES_TOOL => usages_content(cx),
        workspace::COMMIT_TOOL => commit_content(cx),
        _ => return,
    };
    if entries.is_empty() {
        return;
    }
    workspace.show_menu(popup_anchor(workspace, window, cx), entries, cx);
}

fn run_content(cx: &App) -> Vec<MenuEntry> {
    let Some(panel) = cx
        .try_global::<ActiveRunPanel>()
        .map(|active| active.0.clone())
    else {
        return Vec::new();
    };
    panel
        .read(cx)
        .content_tabs()
        .into_iter()
        .enumerate()
        .map(|(index, title)| {
            let panel = panel.clone();
            MenuEntry::new(title, move |_, cx| {
                panel.update(cx, |panel, cx| panel.select_content(index, cx));
            })
        })
        .collect()
}

fn usages_content(cx: &App) -> Vec<MenuEntry> {
    let Some(hub) = cx
        .try_global::<ActiveUsages>()
        .map(|active| active.0.clone())
    else {
        return Vec::new();
    };
    hub.read(cx)
        .tab_names()
        .into_iter()
        .enumerate()
        .map(|(index, title)| {
            let hub = hub.clone();
            MenuEntry::new(title, move |_, cx| {
                hub.update(cx, |hub, cx| hub.select_tab(index, cx));
            })
        })
        .collect()
}

fn commit_content(cx: &App) -> Vec<MenuEntry> {
    let Some(panel) = cx
        .try_global::<ActiveCommitPanel>()
        .map(|active| active.0.clone())
    else {
        return Vec::new();
    };
    [crate::commit::Tab::Commit, crate::commit::Tab::Shelf]
        .into_iter()
        .map(|tab| {
            let panel = panel.clone();
            let label = match tab {
                crate::commit::Tab::Commit => "Commit",
                crate::commit::Tab::Shelf => "Shelf",
            };
            MenuEntry::new(label, move |_, cx| {
                panel.update(cx, |panel, cx| panel.set_tab(tab, cx));
            })
        })
        .collect()
}

fn with_usages(cx: &mut Context<Workspace>, change: fn(&mut workspace::Settings)) {
    let Some(hub) = cx.try_global::<ActiveUsages>().map(|hub| hub.0.clone()) else {
        return;
    };
    cx.spawn(async move |_, cx| {
        hub.update(cx, |hub, cx| hub.change_setting(change, cx));
    })
    .detach();
}

/// The grouping and filtering rules of Usages and TODO, Find in Files and the export.
fn register_tool_commands(workspace: &mut Workspace) {
    let usage_rules: [(&'static str, SettingChange); 9] = [
        (keys::USAGE_GROUP_USAGE_TYPE, set_usage_type),
        (keys::USAGE_GROUP_MODULE, set_usage_module),
        (keys::USAGE_GROUP_DIRECTORY, set_usage_directory),
        (
            keys::USAGE_GROUP_DIRECTORY_STRUCTURE,
            set_usage_directory_structure,
        ),
        (keys::USAGE_GROUP_FILE_STRUCTURE, set_usage_file_structure),
        (keys::USAGE_GROUP_FLATTEN_MODULES, set_usage_flatten_modules),
        (keys::USAGE_FILTER_READ, set_usage_read),
        (keys::USAGE_FILTER_WRITE, set_usage_write),
        (keys::USAGE_FILTER_IMPORTS, set_usage_imports),
    ];
    for (id, change) in usage_rules {
        workspace.register_command(id, title_of(id), move |workspace, _, cx| {
            let usages_focused = workspace.active_tool_id() == Some(workspace::USAGES_TOOL);
            if id == keys::USAGE_GROUP_MODULE && !usages_focused {
                workspace.update_settings(set_todo_modules, cx);
                return;
            }
            if id == keys::USAGE_GROUP_DIRECTORY && !usages_focused {
                workspace.update_settings(set_todo_packages, cx);
                return;
            }
            with_usages(cx, change);
        });
    }
    workspace.register_command(
        keys::SHOW_SETTINGS_AND_FIND_USAGES,
        "Find Usages...",
        usages::FindUsagesSettings::show,
    );
    let todo_rules: [(&'static str, SettingChange); 3] = [
        (keys::TODO_GROUP_MODULES, set_todo_modules),
        (keys::TODO_GROUP_PACKAGES, set_todo_packages),
        (keys::TODO_FLATTEN_PACKAGES, set_todo_flatten),
    ];
    for (id, change) in todo_rules {
        workspace.register_command(id, title_of(id), move |workspace, _, cx| {
            let usages_focused = workspace.active_tool_id() == Some(workspace::USAGES_TOOL);
            if usages_focused && id == keys::TODO_GROUP_MODULES {
                return with_usages(cx, set_usage_module);
            }
            if usages_focused && id == keys::TODO_GROUP_PACKAGES {
                return with_usages(cx, set_usage_directory);
            }
            workspace.update_settings(change, cx);
        });
    }
    workspace.register_command(
        keys::FIND_USAGE_FILTER_POPUP,
        "File Mask",
        |workspace, _, cx| {
            if let Some(finder) = cx
                .try_global::<ActiveFinder>()
                .and_then(|finder| finder.0.clone())
            {
                finder.update(cx, |finder, cx| finder.show_mask_popup(cx));
                return;
            }
            if workspace.active_tool_id() == Some(workspace::USAGES_TOOL) {
                with_usages(cx, set_usage_file_structure);
            }
        },
    );
    workspace.register_command(keys::SHOW_SEARCH_HISTORY, "Search History", |_, _, cx| {
        let Some(finder) = cx
            .try_global::<ActiveFinder>()
            .and_then(|finder| finder.0.clone())
        else {
            return;
        };
        finder.update(cx, |finder, cx| finder.show_history(cx));
    });
    workspace.register_command(
        keys::TOGGLE_FIND_IN_SELECTION,
        "Find in Selection",
        |_, _, cx| {
            let Some(finder) = cx
                .try_global::<ActiveFinder>()
                .and_then(|finder| finder.0.clone())
            else {
                return;
            };
            finder.update(cx, |finder, cx| finder.toggle_selection(cx));
        },
    );
    workspace.register_command(
        keys::EXPORT_TO_TEXT_FILE,
        "Export to Text File...",
        export_to_text,
    );
}

/// Alt+O: the tree of the focused tool window goes to a file the user picks.
fn export_to_text(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let usages = cx.try_global::<ActiveUsages>().map(|hub| hub.0.clone());
    let todo = cx.try_global::<ActiveTodo>().map(|todo| todo.0.clone());
    let problems = cx
        .try_global::<ActiveProblems>()
        .map(|problems| problems.0.read(cx).panel().clone());
    let finder = cx
        .try_global::<ActiveFinder>()
        .and_then(|finder| finder.0.clone());
    let focused =
        |panel: &Entity<crate::items::ItemsPanel>| panel.read(cx).focus().is_focused(window);
    let (text, name) = if let Some(finder) = finder {
        (finder.read(cx).export_text(), "find.txt")
    } else if let Some(panel) = usages
        .map(|hub| hub.read(cx).panel().clone())
        .filter(focused)
    {
        (panel.read(cx).export_text(), "usages.txt")
    } else if let Some(panel) = todo.filter(focused) {
        (panel.read(cx).export_text(), "todo.txt")
    } else if let Some(panel) = problems.filter(focused) {
        (panel.read(cx).export_text(), "problems.txt")
    } else {
        return;
    };
    if text.is_empty() {
        workspace.notify_user("Export", "There is nothing to export", cx);
        return;
    }
    let directory = workspace.root().to_path_buf();
    let chosen = cx.prompt_for_new_path(&directory, Some(name));
    cx.spawn_in(window, async move |_, cx| {
        let Ok(Ok(Some(path))) = chosen.await else {
            return;
        };
        cx.background_executor()
            .spawn(async move { std::fs::write(path, text) })
            .await
            .ok();
    })
    .detach();
}

/// The names a test of the file with `stem` may have, and the name without the test
/// suffix when the file is a test itself.
fn related_names(stem: &str) -> Vec<String> {
    let base = stem
        .strip_suffix("Tests")
        .or_else(|| stem.strip_suffix("Test"))
        .or_else(|| stem.strip_prefix("Test"))
        .unwrap_or(stem);
    let mut names = vec![
        format!("{base}Test"),
        format!("{base}Tests"),
        format!("Test{base}"),
    ];
    if stem != base {
        names.push(base.to_owned());
    }
    names
}

/// The tests of the class of the active file, by the names of the files of the project.
fn goto_test(
    workspace: &mut Workspace,
    files: &Entity<Arc<Vec<PathBuf>>>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(current) = workspace.active_path() else {
        return;
    };
    let (Some(stem), extension) = (
        current
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned()),
        current.extension().map(|extension| extension.to_owned()),
    ) else {
        return;
    };
    let names = related_names(&stem);
    let mut found: Vec<PathBuf> = files
        .read(cx)
        .iter()
        .filter(|path| **path != current)
        .filter(|path| path.extension() == extension.as_deref())
        .filter(|path| {
            path.file_stem()
                .and_then(|stem| stem.to_str())
                .is_some_and(|stem| names.iter().any(|name| name == stem))
        })
        .cloned()
        .collect();
    found.sort();
    found.dedup();
    let spot = create_test_spot(workspace, &current, cx);
    if found.is_empty() {
        match spot {
            Some(spot) => {
                let entries = vec![create_test_entry(spot)];
                let title = test_menu_title(&stem, 0);
                workspace.show_menu_titled(popup_anchor(workspace, window, cx), title, entries, cx);
                workspace.highlight_first_menu_row(cx);
            }
            None => workspace.notify_user("Go to Test", "No test found", cx),
        }
        return;
    }
    if found.len() == 1 && spot.is_none() {
        workspace.open_file(found.remove(0), window, cx);
        return;
    }
    let title = test_menu_title(&stem, found.len());
    let entries = test_menu(workspace, found, spot, cx.weak_entity());
    workspace.show_menu_titled(popup_anchor(workspace, window, cx), title, entries, cx);
    workspace.highlight_first_menu_row(cx);
}

/// The heading of Go to Test: the class of the active file and how many tests it has.
fn test_menu_title(stem: &str, found: usize) -> String {
    format!("Choose Test for {stem} ({found} found)")
}

/// The place Create Test works from, when the language of `path` makes test classes for
/// the class of the current editor.
fn create_test_spot(
    workspace: &Workspace,
    path: &Path,
    cx: &App,
) -> Option<lang_api::DocumentSpot> {
    let support = cx.try_global::<Semantic>()?;
    if !support.0.matches(path) || support.0.test_form().is_none() {
        return None;
    }
    let view = workspace.active_view()?.downcast::<EditorView>().ok()?;
    view.read(cx).spot().map(|(spot, _)| spot)
}

/// The rows of Go to Test: the tests that exist, and Create New Test last after a
/// separator.
fn test_menu(
    workspace: &Workspace,
    found: Vec<PathBuf>,
    spot: Option<lang_api::DocumentSpot>,
    weak: WeakEntity<Workspace>,
) -> Vec<MenuEntry> {
    let root = workspace.root().to_path_buf();
    let icons = workspace.icons().clone();
    let mut entries: Vec<MenuEntry> = found
        .into_iter()
        .map(|path| {
            let label = path.file_stem().map_or_else(
                || path.display().to_string(),
                |stem| stem.to_string_lossy().into_owned(),
            );
            let icon = icons.icon_for(&path);
            let folder = test_folder(&root, &path);
            let weak = weak.clone();
            let row = MenuEntry::new(label, move |window, cx| {
                weak.update(cx, |this, cx| this.open_file(path.clone(), window, cx))
                    .ok();
            })
            .icon(icon);
            match folder {
                Some(folder) => row.detail(folder),
                None => row,
            }
        })
        .collect();
    if let Some(spot) = spot {
        if !entries.is_empty() {
            entries.push(MenuEntry::separator());
        }
        entries.push(create_test_entry(spot));
    }
    entries
}

/// The folder of a test from the root of the project, as the row shows it: a path with
/// `/` on every system.
fn test_folder(root: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(root).unwrap_or(path);
    let parent = relative.parent()?;
    let folder = parent.to_string_lossy().replace('\\', "/");
    (!folder.is_empty()).then_some(folder)
}

/// "Create New Test..." of Go to Test: the Create Test window of the language.
fn create_test_entry(spot: lang_api::DocumentSpot) -> MenuEntry {
    MenuEntry::new("Create New Test...", move |window, cx| {
        let Some(handle) = window.window_handle().downcast::<Workspace>() else {
            return;
        };
        let spot = spot.clone();
        cx.defer(move |cx| {
            handle
                .update(cx, |workspace, window, cx| {
                    crate::create_test::CreateTestDialog::show(spot.clone(), workspace, window, cx)
                })
                .ok();
        });
    })
}

/// Ctrl+Alt+Home: the files tied to the active one, from the language and from the test
/// names of the project.
fn goto_related(
    workspace: &mut Workspace,
    files: &Entity<Arc<Vec<PathBuf>>>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(current) = workspace.active_path() else {
        return;
    };
    let mut found: Vec<PathBuf> = Vec::new();
    if let Some(semantic) = cx.try_global::<Semantic>() {
        found.extend(
            semantic
                .0
                .related_files(&current)
                .into_iter()
                .filter(|path| path.exists()),
        );
    }
    if let (Some(stem), Some(extension)) = (
        current.file_stem().and_then(|stem| stem.to_str()),
        current.extension(),
    ) {
        let names = related_names(stem);
        found.extend(
            files
                .read(cx)
                .iter()
                .filter(|path| **path != current)
                .filter(|path| path.extension() == Some(extension))
                .filter(|path| {
                    path.file_stem()
                        .and_then(|stem| stem.to_str())
                        .is_some_and(|stem| names.iter().any(|name| name == stem))
                })
                .cloned(),
        );
    }
    found.retain(|path| *path != current);
    found.sort();
    found.dedup();
    if found.is_empty() {
        workspace.notify_user("Go to Related", "No related files found", cx);
        return;
    }
    show_related_menu(workspace, found, window, cx);
}

fn show_related_menu(
    workspace: &mut Workspace,
    found: Vec<PathBuf>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let weak = cx.weak_entity();
    let entries = found
        .into_iter()
        .map(|path| {
            let weak = weak.clone();
            let label = path
                .strip_prefix(workspace.root())
                .unwrap_or(&path)
                .to_string_lossy()
                .into_owned();
            let open = path.clone();
            MenuEntry::new(label, move |window, cx| {
                weak.update(cx, |this, cx| this.open_file(open.clone(), window, cx))
                    .ok();
            })
        })
        .collect();
    workspace.show_menu(popup_anchor(workspace, window, cx), entries, cx);
}

/// Where a popup invoked from the keyboard goes: under the selected row of the project
/// tree, under the caret of the editor, or at the pointer when neither has the focus.
pub(crate) fn popup_anchor(workspace: &Workspace, window: &Window, cx: &App) -> Point<Pixels> {
    if let Some(panel) = cx.try_global::<ActiveProjectPanel>() {
        let panel = panel.0.read(cx);
        if panel.focus().is_focused(window) {
            return panel.selected_row_position();
        }
    }
    if let Some(view) = workspace
        .active_view()
        .and_then(|view| view.downcast::<EditorView>().ok())
    {
        let view = view.read(cx);
        if view.focus_handle(cx).is_focused(window) {
            return view.context_menu_position(cx);
        }
    }
    window.mouse_position()
}

/// Run Selection in Terminal: the selection of the editor, or the line of the caret
/// when nothing is selected, goes to the active tab of the terminal. A closed terminal
/// opens with a new tab in the root of the project.
fn run_selection_in_terminal(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(view) = workspace
        .active_view()
        .and_then(|view| view.downcast::<EditorView>().ok())
    else {
        return;
    };
    let text = {
        let view = view.read(cx);
        view.selected_text()
            .or_else(|| view.line_text(view.caret().0))
    };
    let Some(text) = text else {
        return;
    };
    let lines: Vec<String> = text.lines().map(ToString::to_string).collect();
    if lines.iter().all(|line| line.trim().is_empty()) {
        return;
    }
    let Some(terminal) = cx
        .try_global::<ActiveTerminal>()
        .map(|terminal| terminal.0.clone())
    else {
        return;
    };
    let opened = !workspace.tool_open(TERMINAL_TOOL);
    if opened {
        workspace.show_tool(TERMINAL_TOOL, window, cx);
    }
    terminal.update(cx, |panel, cx| panel.run_lines(lines, opened, window, cx));
}

/// The file the active tab or the selected row of the project tree points at.
fn current_path(workspace: &Workspace, window: &Window, cx: &App) -> Option<PathBuf> {
    if let Some(panel) = cx.try_global::<ActiveProjectPanel>() {
        let panel = panel.0.read(cx);
        if panel.focus().is_focused(window)
            && let Some(path) = panel.selected_path()
        {
            return Some(path.to_path_buf());
        }
    }
    workspace.active_path()
}

/// Ctrl+Alt+Insert: the New list of the folder of the current file.
fn new_element_same_place(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let folder = current_path(workspace, window, cx)
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| workspace.root().to_path_buf());
    let mut templates = cx
        .try_global::<crate::project::FileTemplates>()
        .map(|templates| templates.0.clone())
        .unwrap_or_default();
    let Some(panel) = cx
        .try_global::<ActiveProjectPanel>()
        .map(|panel| panel.0.clone())
    else {
        workspace.notify_user("New", "The project tree is not open", cx);
        return;
    };
    if templates.is_empty() {
        workspace.notify_user("New", "No file templates", cx);
        return;
    }
    // The request file of the HTTP client goes last, so the rows the key offered
    // before keep their places.
    templates.push(crate::tree_ops::http_request());
    let entries = templates
        .into_iter()
        .map(|template| {
            let panel = panel.clone();
            let folder = folder.clone();
            MenuEntry::new(template.label, move |window, cx| {
                panel.update(cx, |panel, cx| {
                    panel.ask_new(template.clone(), folder.clone(), window, cx)
                });
            })
        })
        .collect();
    workspace.show_menu(popup_anchor(workspace, window, cx), entries, cx);
}

/// F5: asks for the name of the copy of the current file and opens it. A file whose
/// language names a declaration goes through the Copy Class window, with its folder.
fn copy_element(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let Some(path) = current_path(workspace, window, cx) else {
        workspace.notify_user("Copy Element", "No file is selected", cx);
        return;
    };
    let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
        workspace.notify_user("Copy Element", "The file has no name", cx);
        return;
    };
    let folder = path
        .parent()
        .map_or_else(|| workspace.root().to_path_buf(), Path::to_path_buf);
    let extension = path.extension();
    let support = cx.try_global::<Semantic>();
    let text = support.and_then(|_| crate::copy_class::source_text(workspace, &path, cx));
    if let Some(text) = text
        && crate::copy_class::knows_declaration(support, &path, &text, stem)
    {
        let default_name = crate::copy_class::default_name(&folder, stem, extension);
        let Some(handle) = window.window_handle().downcast::<Workspace>() else {
            return;
        };
        let source = path.clone();
        let submit = move |name: String, folder: PathBuf, cx: &mut App| {
            let (path, handle) = (source.clone(), handle);
            // The key that confirms the window runs while it is being updated.
            cx.defer(move |cx| {
                cx.defer(move |cx| {
                    handle
                        .update(cx, move |workspace, window, cx| {
                            copy_element_now(workspace, &path, &name, &folder, window, cx)
                        })
                        .ok();
                });
            });
        };
        crate::copy_class::CopyClassDialog::show(
            &path,
            &folder,
            &default_name,
            false,
            submit,
            workspace,
            window,
            cx,
        );
        return;
    }
    let name = format!("{stem}Copy");
    let Some(handle) = window.window_handle().downcast::<Workspace>() else {
        return;
    };
    let submit = move |name: String, cx: &mut App| {
        let (path, folder, handle) = (path.clone(), folder.clone(), handle);
        // The key that confirms the prompt runs while the window is updated.
        cx.defer(move |cx| {
            cx.defer(move |cx| {
                handle
                    .update(cx, move |workspace, window, cx| {
                        copy_element_now(workspace, &path, &name, &folder, window, cx)
                    })
                    .ok();
            });
        });
    };
    Prompt::show_with("Copy Element", &name, submit, workspace, window, cx);
}

/// Copies `source` to a file named `name` in `folder`, giving the declaration the new
/// name and the text the package of the place when the language knows how.
fn copy_element_now(
    workspace: &mut Workspace,
    source: &Path,
    name: &str,
    folder: &Path,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let target = match crate::copy_class::copy_file(workspace, source, name, folder, cx) {
        Ok(target) => target,
        Err(error) => {
            workspace.notify_error("Copy Element", error, cx);
            return;
        }
    };
    // The tree reports the selected row to the window, which the window cannot take
    // while it is being updated here.
    if let Some(panel) = cx.try_global::<ActiveProjectPanel>() {
        let panel = panel.0.clone();
        let selected = target.clone();
        cx.defer(move |cx| {
            panel.update(cx, |panel, cx| {
                panel.select_path(&selected, cx);
                panel.reload(cx);
            });
        });
    }
    workspace.open_file(target, window, cx);
}

/// Ctrl+Shift+C: the file of the current tab, or the file selected in the project tree.
fn copy_paths(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let panel = cx
        .try_global::<crate::project::ActiveProjectPanel>()
        .map(|panel| panel.0.clone());
    let from_tree = panel.as_ref().is_some_and(|panel| {
        let panel = panel.read(cx);
        panel.focus().is_focused(window)
    });
    let paths = if from_tree {
        panel
            .map(|panel| panel.read(cx).selected_paths())
            .unwrap_or_default()
    } else {
        workspace.active_path().into_iter().collect()
    };
    if !paths.is_empty() {
        let text = paths
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join("\n");
        cx.write_to_clipboard(ClipboardItem::new_string(text));
    }
}

/// Ctrl+D in the editor: the selected text, or the whole file, against the clipboard.
fn compare_with_clipboard(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(view) = workspace
        .active_view()
        .and_then(|view| view.downcast::<EditorView>().ok())
    else {
        workspace.notify_user("Compare with Clipboard", "No file is open", cx);
        return;
    };
    let Some(clipboard) = cx.read_from_clipboard().and_then(|item| item.text()) else {
        workspace.notify_user("Compare with Clipboard", "The clipboard is empty", cx);
        return;
    };
    let Some(git) = cx.try_global::<ActiveGit>().map(|git| git.0.clone()) else {
        return;
    };
    let (path, text) = {
        let view = view.read(cx);
        let text = view
            .selected_text()
            .or_else(|| view.text())
            .unwrap_or_default();
        (view.path().to_path_buf(), text)
    };
    let weak = cx.weak_entity();
    DiffView::compare_text_clipboard(git, weak, path, text, clipboard, window, cx);
}

/// File > Open: a file opens in a tab of this window, a folder as a project.
fn open_paths(_workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let paths = cx.prompt_for_paths(PathPromptOptions {
        files: true,
        directories: true,
        multiple: false,
        prompt: Some("Open".into()),
    });
    cx.spawn_in(window, async move |workspace, cx| {
        let Ok(Ok(Some(paths))) = paths.await else {
            return;
        };
        let Some(path) = paths.into_iter().next() else {
            return;
        };
        workspace
            .update_in(cx, |workspace, window, cx| {
                if path.is_dir() {
                    workspace.open_project(path, cx);
                } else {
                    let heap = crate::heap_dump::open_file(workspace, path.clone(), window, cx);
                    if !heap {
                        workspace.open_file(path, window, cx);
                    }
                }
            })
            .ok();
    })
    .detach();
}

/// The open editors this crate knows how to reload.
fn open_views(workspace: &Workspace) -> Vec<(PathBuf, Entity<EditorView>)> {
    workspace
        .open_editors()
        .into_iter()
        .filter_map(|(path, view)| view.downcast::<EditorView>().ok().map(|view| (path, view)))
        .collect()
}

/// Reads every open file from disk again; unsaved text goes too.
fn reload_all_now(workspace: &mut Workspace, _window: &mut Window, cx: &mut Context<Workspace>) {
    for (_, view) in open_views(workspace) {
        view.update(cx, |view, cx| view.reload_from_disk(cx));
    }
    if let Some(panel) = cx.try_global::<ActiveProjectPanel>() {
        let panel = panel.0.clone();
        panel.update(cx, |panel, cx| panel.reload(cx));
    }
    // The walk catches whatever the watcher missed: the file list of the search, the
    // folders of the tree, and the build scripts the language server is told about.
    if let Some(index) = cx.try_global::<ActiveIndex>() {
        index.0.rescan();
    }
    // The working tree may be different from what the status line and the diff show.
    if let Some(git) = cx.try_global::<ActiveGit>() {
        let git = git.0.clone();
        git.update(cx, |git, cx| git.repository_changed(cx));
    }
}

/// Ctrl+Alt+Y, File > Reload All from Disk: a file with unsaved text asks first.
fn reload_all_from_disk(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let dirty: Vec<PathBuf> = open_views(workspace)
        .into_iter()
        .filter(|(_, view)| view.read(cx).is_dirty())
        .map(|(path, _)| path)
        .collect();
    if dirty.is_empty() {
        reload_all_now(workspace, window, cx);
        return;
    }
    let names = dirty
        .iter()
        .map(|path| {
            path.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        })
        .collect::<Vec<_>>()
        .join(", ");
    let text = format!(
        "{} file(s) have unsaved changes. Reloading reads the text on disk and drops them: {names}",
        dirty.len()
    );
    ChoiceDialog::show(
        "Reload All from Disk",
        text,
        vec![
            DialogChoice::new("Reload All", true, reload_all_now),
            DialogChoice::new("Cancel", false, |_, _, _| {}),
        ],
        workspace,
        window,
        cx,
    );
}

/// File > Exit: every window closes, and unsaved text asks first.
fn exit(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let dirty = workspace.unsaved_files(cx);
    if dirty.is_empty() {
        ChoiceDialog::show(
            "Exit Arro",
            "Close every project window?",
            vec![
                DialogChoice::new("Exit", true, |workspace, window, cx| {
                    workspace.close_all_windows(true, window, cx)
                }),
                DialogChoice::new("Cancel", false, |_, _, _| {}),
            ],
            workspace,
            window,
            cx,
        );
        return;
    }
    let names = dirty
        .iter()
        .map(|path| {
            path.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        })
        .collect::<Vec<_>>()
        .join(", ");
    let text = format!(
        "{} file(s) have unsaved changes. Save them before Arro exits? {names}",
        dirty.len()
    );
    ChoiceDialog::show(
        "Exit Arro",
        text,
        vec![
            DialogChoice::new("Save All and Exit", true, |workspace, window, cx| {
                let mut failure = None;
                for path in workspace.unsaved_files(cx) {
                    if let Err(error) = workspace.save_editor_now(&path, cx) {
                        failure = Some(error);
                        break;
                    }
                }
                if let Some(text) = failure {
                    workspace.notify_error("Cannot save", text, cx);
                    return;
                }
                workspace.close_all_windows(true, window, cx);
            }),
            DialogChoice::new("Exit Without Saving", false, |workspace, window, cx| {
                workspace.close_all_windows(false, window, cx)
            }),
            DialogChoice::new("Cancel", false, |_, _, _| {}),
        ],
        workspace,
        window,
        cx,
    );
}

/// Edit > Toggle Read-Only Attribute: flips the attribute of the file on disk and makes
/// the open editor follow it.
#[allow(clippy::permissions_set_readonly_false)]
fn toggle_read_only(workspace: &mut Workspace, _window: &mut Window, cx: &mut Context<Workspace>) {
    let Some(path) = workspace.active_path() else {
        return;
    };
    let Some(view) = workspace
        .active_view()
        .and_then(|view| view.downcast::<EditorView>().ok())
    else {
        return;
    };
    let Ok(metadata) = std::fs::metadata(&path) else {
        workspace.notify_error(
            "Read-Only",
            format!("{} is not on disk", path.display()),
            cx,
        );
        return;
    };
    let read_only = !metadata.permissions().readonly();
    let mut permissions = metadata.permissions();
    permissions.set_readonly(read_only);
    if let Err(error) = std::fs::set_permissions(&path, permissions) {
        workspace.notify_error("Read-Only", format!("{}: {error}", path.display()), cx);
        return;
    }
    view.update(cx, |view, cx| view.set_read_only(read_only, cx));
    cx.notify();
}

/// The Git actions Find Action and the menus run. What needs the working tree waits for the
/// end of the update of the window before it reads it.
fn register_git_commands(workspace: &mut Workspace) {
    workspace.register_command("git.stash", "Stash Changes...", |workspace, window, cx| {
        let Some(git) = cx.try_global::<ActiveGit>().map(|git| git.0.clone()) else {
            return;
        };
        Prompt::show(
            "Stash Changes",
            "Stash message",
            move |message, cx| {
                git.update(cx, |git, cx| git.stash(message, cx));
            },
            workspace,
            window,
            cx,
        );
    });
    workspace.register_command(
        "git.unstash",
        "Unstash Changes...",
        |workspace, window, cx| {
            let Some(git) = cx.try_global::<ActiveGit>().map(|git| git.0.clone()) else {
                return;
            };
            let position = popup_anchor(workspace, window, cx);
            git.update(cx, |git, cx| git.show_stashes(position, cx));
        },
    );
    workspace.register_command("git.fetch", "Fetch", |_, _, cx| {
        let Some(git) = cx.try_global::<ActiveGit>().map(|git| git.0.clone()) else {
            return;
        };
        git.update(cx, |git, cx| git.fetch(cx));
    });
    workspace.register_command("git.update", "Update Project", |workspace, window, cx| {
        let Some(git) = cx.try_global::<ActiveGit>().map(|git| git.0.clone()) else {
            return;
        };
        let position = popup_anchor(workspace, window, cx);
        cx.defer(move |cx| {
            git.update(cx, |git, cx| git.update_project(position, cx));
        });
    });
    workspace.register_command("git.conflicts", "Conflicts...", |_, _, cx| {
        let Some(git) = cx.try_global::<ActiveGit>().map(|git| git.0.clone()) else {
            return;
        };
        git.update(cx, |git, cx| git.open_conflicts(cx));
    });
    workspace.register_command("git.abort", "Abort Merge or Rebase", |_, _, cx| {
        let Some(git) = cx.try_global::<ActiveGit>().map(|git| git.0.clone()) else {
            return;
        };
        cx.defer(move |cx| {
            git.update(cx, |git, cx| git.abort_operation(cx));
        });
    });
    workspace.register_command(
        crate::git::CONTINUE_REBASE_COMMAND,
        "Continue Rebase",
        |_, _, cx| {
            let Some(git) = cx.try_global::<ActiveGit>().map(|git| git.0.clone()) else {
                return;
            };
            cx.defer(move |cx| {
                git.update(cx, |git, cx| git.finish_operation(String::new(), cx));
            });
        },
    );
    workspace.register_command("git.show_log", "Git Log", |workspace, window, cx| {
        let Some(panel) = cx.try_global::<ActiveGitLog>().map(|panel| panel.0.clone()) else {
            return;
        };
        workspace.show_tool(workspace::LOG_TOOL, window, cx);
        panel.update(cx, |panel, cx| panel.show_root(cx));
    });
    workspace.register_command(
        crate::git::SHOW_CONSOLE_COMMAND,
        "Show in Console",
        |workspace, window, cx| {
            let Some(panel) = cx.try_global::<ActiveGitLog>().map(|panel| panel.0.clone()) else {
                return;
            };
            workspace.show_tool(workspace::LOG_TOOL, window, cx);
            panel.update(cx, |panel, cx| panel.show_console(cx));
        },
    );
    workspace.register_command(
        "git.show_file_history",
        "Show History",
        |workspace, window, cx| {
            let path = workspace
                .active_path()
                .unwrap_or_else(|| workspace.root().to_path_buf());
            git_log::show_file(path, workspace, window, cx);
        },
    );
    workspace.register_command(
        "git.show_selection_history",
        "Show History for Selection",
        crate::git_history::show_selection,
    );
    workspace.register_command(
        "git.show_method_history",
        "Show History for Method",
        crate::git_history::show_method,
    );
    workspace.register_command("git.show_branches", "Branches", |workspace, window, cx| {
        let Some(git) = cx.try_global::<ActiveGit>().map(|git| git.0.clone()) else {
            return;
        };
        let position = popup_anchor(workspace, window, cx);
        cx.defer(move |cx| {
            git.update(cx, |git, cx| git.show_branches(position, cx));
        });
    });
    workspace.register_command(
        crate::git::RESTORE_BRANCH_COMMAND,
        "Restore Deleted Branch",
        |_, _, cx| {
            let Some(git) = cx.try_global::<ActiveGit>().map(|git| git.0.clone()) else {
                return;
            };
            git.update(cx, |git, cx| git.restore_deleted_branch(cx));
        },
    );
    workspace.register_command(
        crate::git::ANNOTATE_COMMAND,
        "Annotate with Git Blame",
        |_, _, cx| {
            let Some(git) = cx.try_global::<ActiveGit>().map(|git| git.0.clone()) else {
                return;
            };
            // The blame saves the open files and reads the repository: both wait for the
            // end of the update that ran the command.
            cx.defer(move |cx| git.update(cx, |git, cx| git.annotate(cx)));
        },
    );
    workspace.register_command(
        crate::git::CLOSE_ANNOTATIONS_COMMAND,
        "Close Annotations",
        |_, _, cx| {
            let Some(git) = cx.try_global::<ActiveGit>().map(|git| git.0.clone()) else {
                return;
            };
            cx.defer(move |cx| git.update(cx, |git, cx| git.close_annotations(cx)));
        },
    );
    workspace.register_command("git.rebase", "Rebase...", |_, window, cx| {
        let Some(panel) = cx.try_global::<ActiveGitLog>().map(|panel| panel.0.clone()) else {
            return;
        };
        let Some(commit) = panel.read(cx).focused_commit_id() else {
            return;
        };
        let handle = window.window_handle();
        panel.update(cx, |panel, cx| {
            panel.open_rebase(commit, handle, cx);
        });
    });
    workspace.register_command("git.cherry_pick", "Cherry-pick", |_, _, cx| {
        let Some(panel) = cx.try_global::<ActiveGitLog>().map(|panel| panel.0.clone()) else {
            return;
        };
        panel.update(cx, |panel, cx| panel.cherry_pick_picked(cx));
    });
    workspace.register_command("git.revert_commit", "Revert Commit", |_, _, cx| {
        let Some(panel) = cx.try_global::<ActiveGitLog>().map(|panel| panel.0.clone()) else {
            return;
        };
        let Some(commit) = panel.read(cx).focused_commit_id() else {
            return;
        };
        let Some(git) = cx.try_global::<ActiveGit>().map(|git| git.0.clone()) else {
            return;
        };
        git.update(cx, |git, cx| git.revert_commit(commit, cx));
    });
    workspace.register_command("git.reset_head", "Reset HEAD...", |_, window, cx| {
        let Some(panel) = cx.try_global::<ActiveGitLog>().map(|panel| panel.0.clone()) else {
            return;
        };
        let Some(commit) = panel.read(cx).focused_commit_id() else {
            return;
        };
        let Some(git) = cx.try_global::<ActiveGit>().map(|git| git.0.clone()) else {
            return;
        };
        let workspace = cx.weak_entity();
        let handle = window.window_handle();
        cx.defer(move |cx| {
            handle
                .update(cx, |_, window, cx| {
                    workspace
                        .update(cx, |workspace, cx| {
                            git_log::ResetView::open(git, commit, workspace, window, cx)
                        })
                        .ok();
                })
                .ok();
        });
    });
    workspace.register_command("git.shelve", "Shelve Changes...", |_, window, cx| {
        let Some(panel) = cx
            .try_global::<ActiveCommitPanel>()
            .map(|panel| panel.0.clone())
        else {
            return;
        };
        let workspace = cx.weak_entity();
        let handle = window.window_handle();
        cx.defer(move |cx| {
            handle
                .update(cx, |_, window, cx| {
                    workspace
                        .update(cx, |workspace, cx| {
                            workspace.show_tool(workspace::COMMIT_TOOL, window, cx)
                        })
                        .ok();
                    panel.update(cx, |panel, cx| panel.shelve(cx));
                })
                .ok();
        });
    });
    workspace.register_command("git.unshelve", "Unshelve...", |_, window, cx| {
        let Some(panel) = cx
            .try_global::<ActiveCommitPanel>()
            .map(|panel| panel.0.clone())
        else {
            return;
        };
        let workspace = cx.weak_entity();
        let handle = window.window_handle();
        cx.defer(move |cx| {
            handle
                .update(cx, |_, window, cx| {
                    workspace
                        .update(cx, |workspace, cx| {
                            workspace.show_tool(workspace::COMMIT_TOOL, window, cx)
                        })
                        .ok();
                    panel.update(cx, |panel, cx| panel.ask_unshelve(window, cx));
                })
                .ok();
        });
    });

    let repository = |cx: &App| {
        cx.try_global::<ActiveGit>()
            .is_some_and(|git| git.0.read(cx).repo().is_some())
    };
    let picked = |cx: &App| {
        cx.try_global::<ActiveGitLog>()
            .is_some_and(|panel| !panel.0.read(cx).picked_ids().is_empty())
    };
    workspace.set_command_check(crate::git::ANNOTATE_COMMAND, move |workspace, cx| {
        repository(cx) && workspace.active_path().is_some()
    });
    workspace.set_command_check("git.show_selection_history", move |workspace, cx| {
        repository(cx) && workspace.active_path().is_some()
    });
    workspace.set_command_check("git.rebase", move |_, cx| repository(cx) && picked(cx));
    workspace.set_command_check("git.cherry_pick", move |_, cx| repository(cx) && picked(cx));
    workspace.set_command_check("git.revert_commit", move |_, cx| {
        repository(cx) && picked(cx)
    });
    workspace.set_command_check("git.reset_head", move |_, cx| repository(cx) && picked(cx));
    workspace.set_command_check("git.shelve", move |_, cx| repository(cx));
    workspace.set_command_check("git.unshelve", move |_, cx| repository(cx));
}

/// `host:port`, the form the Attach to JVM prompt takes. Only the user names the host.
fn parse_endpoint(text: &str) -> Result<(String, u16), String> {
    let text = text.trim();
    let (host, port) = text.rsplit_once(':').ok_or("write host:port")?;
    let host = host.trim().trim_start_matches('[').trim_end_matches(']');
    if host.is_empty() {
        return Err("the host is empty".to_owned());
    }
    let port: u16 = port
        .trim()
        .parse()
        .map_err(|_| format!("{port}: not a port"))?;
    if port == 0 {
        return Err("the port is zero".to_owned());
    }
    Ok((host.to_owned(), port))
}

/// The processes a debugger can attach to, for a language that lists its JVMs. The last
/// row asks for a `host:port` by hand.
struct AttachProcessView {
    debug: Entity<DebugPanel>,
    runs: Entity<RunPanel>,
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    scroll: ScrollHandle,
    /// The answer of the language, `None` while the list is being built.
    processes: Option<Vec<DebugProcess>>,
    selected: usize,
}

impl AttachProcessView {
    fn new(
        debug: Entity<DebugPanel>,
        runs: Entity<RunPanel>,
        workspace: WeakEntity<Workspace>,
        replies: lang_api::Replies<Vec<DebugProcess>>,
        cx: &mut Context<Self>,
    ) -> Self {
        let view = Self {
            debug,
            runs,
            workspace,
            focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            processes: None,
            selected: 0,
        };
        cx.spawn(async move |this, cx| {
            use futures_util::StreamExt;
            let mut replies = replies;
            while let Some(answer) = replies.next().await {
                let processes = answer.value;
                this.update(cx, |this, cx| {
                    this.processes = Some(processes);
                    this.selected = 0;
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
        view
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace
            .update(cx, |workspace, cx| workspace.dismiss(window, cx))
            .ok();
    }

    fn choose(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let processes = self.processes.clone().unwrap_or_default();
        if let Some(process) = processes.get(index) {
            let name = if process.name.is_empty() {
                format!("process {}", process.pid)
            } else {
                process.name.clone()
            };
            self.close(window, cx);
            begin_attach(
                self.workspace.clone(),
                window.window_handle(),
                self.debug.clone(),
                self.runs.clone(),
                process.host.clone(),
                process.port,
                name,
                cx,
            );
        } else if index == processes.len() {
            let (debug, runs) = (self.debug.clone(), self.runs.clone());
            self.workspace
                .update(cx, |workspace, cx| {
                    workspace.dismiss(window, cx);
                    ask_endpoint(workspace, window, debug, runs, cx);
                })
                .ok();
        }
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.processes.as_ref().map_or(1, Vec::len) + 1;
        match event.keystroke.key.as_str() {
            "up" => {
                self.selected = self.selected.saturating_sub(1);
                cx.notify();
            }
            "down" => {
                if self.selected + 1 < count {
                    self.selected += 1;
                }
                cx.notify();
            }
            "enter" => self.choose(self.selected, window, cx),
            "escape" => self.close(window, cx),
            _ => return,
        }
        cx.stop_propagation();
    }
}

impl Render for AttachProcessView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let mut rows: Vec<AnyElement> = Vec::new();
        match &self.processes {
            None => rows.push(
                div()
                    .px(px(ROW_PADDING_X))
                    .py(px(SPACE_S))
                    .text_color(theme.text_secondary)
                    .child("Looking for processes…")
                    .into_any_element(),
            ),
            Some(processes) if processes.is_empty() => rows.push(
                div()
                    .px(px(ROW_PADDING_X))
                    .py(px(SPACE_S))
                    .text_color(theme.text_secondary)
                    .child("No process with a debug agent is running.")
                    .into_any_element(),
            ),
            Some(processes) => {
                for (index, process) in processes.iter().enumerate() {
                    let selected = index == self.selected;
                    let name = if process.name.is_empty() {
                        format!("process {}", process.pid)
                    } else {
                        process.name.clone()
                    };
                    rows.push(
                        div()
                            .id(SharedString::from(format!("attach-process-{index}")))
                            .debug_selector(move || format!("attach-process-{index}"))
                            .flex()
                            .items_center()
                            .gap(px(SPACE_S))
                            .px(px(ROW_PADDING_X))
                            .py(px(SPACE_S))
                            .cursor_pointer()
                            .text_color(theme.text)
                            .when(selected, |row| row.bg(theme.selection))
                            .when(!selected, |row| row.hover(|row| row.bg(theme.hover)))
                            .child(div().flex_1().min_w(px(0.)).truncate().child(name))
                            .child(div().flex_none().text_color(theme.text_secondary).child(
                                format!("{} · {}:{}", process.pid, process.host, process.port),
                            ))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.choose(index, window, cx)
                            }))
                            .into_any_element(),
                    );
                }
            }
        }
        let manual = self.processes.as_ref().map_or(0, Vec::len);
        let selected = manual == self.selected;
        rows.push(
            div()
                .id("attach-process-manual")
                .debug_selector(|| "attach-process-manual".to_owned())
                .flex()
                .items_center()
                .px(px(ROW_PADDING_X))
                .py(px(SPACE_S))
                .cursor_pointer()
                .text_color(theme.text_secondary)
                .when(selected, |row| row.bg(theme.selection))
                .when(!selected, |row| row.hover(|row| row.bg(theme.hover)))
                .child("Enter host:port…")
                .on_click(cx.listener(move |this, _, window, cx| this.choose(manual, window, cx)))
                .into_any_element(),
        );
        let shell = surface(&theme)
            .id("attach-processes")
            .debug_selector(|| "attach-processes".to_owned())
            .track_focus(&self.focus)
            .text_size(px(UI_SIZE))
            .on_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)));
        let contents = scrollable(
            "attach-processes",
            &self.scroll,
            div()
                .id("attach-processes-list")
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .gap(px(2.))
                .p(px(DIALOG_PADDING))
                .overflow_y_scroll()
                .track_scroll(&self.scroll)
                .children(rows),
            &theme,
            window,
            cx,
        );
        let viewport = window.viewport_size();
        let width = px(460.);
        let left = (viewport.width - width) / 2.;
        let top = viewport.height * 0.2;
        let close = cx.listener(|this, _: &ClickEvent, window, cx| this.close(window, cx));
        dialog_window(
            DialogSpec {
                key: "attach-process",
                title: "Attach to Process".into(),
                extra: None,
                place: DialogPlace {
                    left: f32::from(left),
                    top: f32::from(top),
                    width: f32::from(width),
                    height: None,
                },
                min: (360., 240.),
            },
            shell,
            window,
            cx,
            close,
            contents,
        )
    }
}

fn attach_to_jvm(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let Some(debug) = cx.try_global::<ActiveDebug>().map(|panel| panel.0.clone()) else {
        return;
    };
    let Some(runs) = cx
        .try_global::<ActiveRunPanel>()
        .map(|panel| panel.0.clone())
    else {
        return;
    };
    // A language that lists the JVMs with a debug agent offers them by name; the manual
    // address stays the last row of that list.
    if let Some(provider) = cx
        .try_global::<Semantic>()
        .and_then(|semantic| semantic.0.debug_processes())
    {
        let replies = lang_api::ask(provider, ());
        let workspace_weak = cx.weak_entity();
        let view = cx.new(|cx| AttachProcessView::new(debug, runs, workspace_weak, replies, cx));
        let focus = view.read(cx).focus.clone();
        workspace.show_overlay(view.into(), focus, window, cx);
        return;
    }
    ask_endpoint(workspace, window, debug, runs, cx);
}

/// The prompt of a `host:port` the user types, for a language without a process list.
fn ask_endpoint(
    workspace: &mut Workspace,
    window: &mut Window,
    debug: Entity<DebugPanel>,
    runs: Entity<RunPanel>,
    cx: &mut Context<Workspace>,
) {
    let workspace_weak = cx.weak_entity();
    let handle = window.window_handle();
    let submit = move |text: String, cx: &mut App| {
        let (host, port) = match parse_endpoint(&text) {
            Ok(endpoint) => endpoint,
            Err(message) => {
                workspace_weak
                    .update(cx, |workspace, cx| {
                        workspace.notify_warning("Attach to JVM", message, cx)
                    })
                    .ok();
                return;
            }
        };
        let name = format!("{host}:{port}");
        begin_attach(
            workspace_weak.clone(),
            handle,
            debug.clone(),
            runs.clone(),
            host,
            port,
            name,
            cx,
        );
    };
    Prompt::show_with(
        "Attach to JVM",
        "localhost:5005",
        submit,
        workspace,
        window,
        cx,
    );
}

/// Starts the session of an attachment and opens the Debug window once it is up.
#[allow(clippy::too_many_arguments)]
fn begin_attach(
    workspace: WeakEntity<Workspace>,
    handle: AnyWindowHandle,
    debug: Entity<DebugPanel>,
    runs: Entity<RunPanel>,
    host: String,
    port: u16,
    name: String,
    cx: &mut App,
) {
    let Some(service) = runs.read(cx).debug_service() else {
        workspace
            .update(cx, |workspace, cx| {
                workspace.notify_warning("Attach to JVM", "The language server is not running", cx)
            })
            .ok();
        return;
    };
    let started = debug.update(cx, |panel, cx| {
        panel.start_attach(service, host, port, name, None, cx)
    });
    match started {
        Ok(()) => {
            // The caller may still be in its own update: show the window after it ends.
            cx.defer(move |cx| {
                handle
                    .update(cx, |_, window, cx| {
                        workspace
                            .update(cx, |workspace, cx| {
                                workspace.show_tool(DEBUG_TOOL, window, cx)
                            })
                            .ok();
                    })
                    .ok();
            });
        }
        Err(error) => {
            workspace
                .update(cx, |workspace, cx| {
                    workspace.notify_error("Attach to JVM", error, cx)
                })
                .ok();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_endpoint, title_of};

    #[test]
    fn titles_come_from_ids() {
        assert_eq!(title_of("editor.move_line_up"), "Move line up");
        assert_eq!(title_of("window.toggle_theme"), "Toggle theme");
        assert_eq!(title_of(""), "");
    }

    #[test]
    fn an_endpoint_is_a_host_and_a_port() {
        assert_eq!(
            parse_endpoint("localhost:5005"),
            Ok(("localhost".into(), 5005))
        );
        assert_eq!(
            parse_endpoint("  127.0.0.1:5005 "),
            Ok(("127.0.0.1".into(), 5005))
        );
        assert_eq!(parse_endpoint("[::1]:5005"), Ok(("::1".into(), 5005)));
        assert!(parse_endpoint("localhost").is_err());
        assert!(parse_endpoint(":5005").is_err());
        assert!(parse_endpoint("localhost:0").is_err());
    }
}
