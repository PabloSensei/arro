//! Tool windows and popups on top of the workspace.

mod agents;
mod ansi;
mod branches;
mod build;
mod call_hierarchy;
mod change_signature;
mod changelists;
mod changes;
mod clipboard;
mod commands;
mod commit;
mod compare;
mod conflict;
mod console_input;
mod copy_class;
mod debug;
mod diff_view;
mod docker;
mod documentation;
mod editor_menu;
mod editor_tab;
mod file_tree;
mod finder;
mod folder_compare;
mod git;
mod git_log;
mod history;
mod history_view;
mod index;
mod inspect;
mod items;
mod marks;
mod method_hierarchy;
mod navigation;
mod outline;
mod palette;
mod performance;
mod places;
mod plugins;
mod problems;
mod project;
mod project_state;
mod project_structure;
mod prompt;
mod reformat;
mod run;
mod run_configs;
mod script;
mod settings;
mod settings_catalog;
mod settings_keymap;
mod settings_templates;
mod shelf;
mod start;
mod structure;
mod term_grid;
mod terminal_panel;
mod todo;
mod tree_ops;
mod trust;
mod usages;
mod window_features;
mod window_popups;

#[cfg(test)]
mod window_tests;
#[cfg(test)]
mod window_tests_actions;
#[cfg(test)]
mod window_tests_complete;
#[cfg(test)]
mod window_tests_completion;
#[cfg(test)]
mod window_tests_debug_actions;
#[cfg(test)]
mod window_tests_edit;
#[cfg(test)]
mod window_tests_editing;
#[cfg(test)]
mod window_tests_git;
#[cfg(test)]
mod window_tests_hierarchy;
#[cfg(test)]
mod window_tests_hotswap;
#[cfg(test)]
mod window_tests_import;
#[cfg(test)]
mod window_tests_keys;
#[cfg(test)]
mod window_tests_lang;
#[cfg(test)]
mod window_tests_layout;
#[cfg(test)]
mod window_tests_live;
#[cfg(test)]
mod window_tests_lookup;
#[cfg(test)]
mod window_tests_m11;
#[cfg(test)]
mod window_tests_menu_main;
#[cfg(test)]
mod window_tests_menus;
#[cfg(test)]
mod window_tests_perf;
#[cfg(test)]
mod window_tests_real;
#[cfg(test)]
mod window_tests_real_drag;
#[cfg(test)]
mod window_tests_run;
#[cfg(test)]
mod window_tests_safe_delete;
#[cfg(test)]
mod window_tests_signature;
#[cfg(test)]
mod window_tests_snippets;
#[cfg(test)]
mod window_tests_tools;
#[cfg(test)]
mod window_tests_ui;
#[cfg(test)]
mod window_tests_usages;
#[cfg(test)]
mod window_tests_vcs_keys;
#[cfg(test)]
mod window_tests_virtual;
#[cfg(test)]
mod window_tests_window;

#[cfg(test)]
mod window_tests_doc_window;
#[cfg(test)]
mod window_tests_folding;
#[cfg(test)]
mod window_tests_grouping;

#[cfg(test)]
mod window_tests_audit;

#[cfg(test)]
mod window_tests_shelf;

#[cfg(test)]
mod window_tests_stable;

#[cfg(test)]
mod window_tests_inspect;

#[cfg(test)]
mod window_tests_vcs_polish;

#[cfg(test)]
mod window_tests_indent;

#[cfg(test)]
mod window_tests_copy_class;

#[cfg(test)]
mod window_tests_inspect2;

#[cfg(test)]
mod window_tests_usages_tabs;

#[cfg(test)]
mod window_tests_about;
#[cfg(test)]
mod window_tests_menu_bar;

#[cfg(test)]
mod window_tests_idle;

#[cfg(test)]
mod window_tests_language_failure;

#[cfg(test)]
mod window_tests_chrome_layout;

#[cfg(test)]
mod window_tests_popup_motion;

#[cfg(test)]
mod window_tests_reentrancy;

#[cfg(test)]
mod window_tests_dead_buttons;

#[cfg(test)]
mod window_tests_server_setup;

#[cfg(test)]
mod window_tests_build_tool;
#[cfg(test)]
mod window_tests_run_configs;
#[cfg(test)]
mod window_tests_spacing_popups;
#[cfg(test)]
mod window_tests_spacing_tools;
#[cfg(test)]
mod window_tests_toolchain_hint;
#[cfg(test)]
mod window_tests_tooltips;

#[cfg(test)]
mod window_tests_find_usages;
#[cfg(test)]
mod window_tests_keymap_edit;
#[cfg(test)]
mod window_tests_run_main;

#[cfg(test)]
mod window_tests_editor_settings;

#[cfg(test)]
mod window_tests_project_structure;

#[cfg(test)]
mod window_tests_branches;
#[cfg(test)]
mod window_tests_idle_events;
#[cfg(test)]
mod window_tests_inlay_hints;
#[cfg(test)]
mod window_tests_menu_gaps;
#[cfg(test)]
mod window_tests_scrollbars;
#[cfg(test)]
mod window_tests_semantic_tokens;

#[cfg(test)]
mod window_tests_settings_tools;

#[cfg(test)]
mod window_tests_editor_scrollbar;
#[cfg(test)]
mod window_tests_quick_doc;

#[cfg(test)]
mod window_tests_code_vision;
#[cfg(test)]
mod window_tests_editor_polish;
#[cfg(test)]
mod window_tests_rendered_doc;
#[cfg(test)]
mod window_tests_template_engine;
#[cfg(test)]
mod window_tests_templates_settings;
#[cfg(test)]
mod window_tests_tool_resize;

// The program of these tests is powershell.exe.
#[cfg(all(test, windows))]
mod window_tests_console_input;
#[cfg(test)]
mod window_tests_debug_input;
#[cfg(test)]
mod window_tests_diff_side;
#[cfg(test)]
mod window_tests_folder_compare;
#[cfg(test)]
mod window_tests_navigation_polish;

#[cfg(test)]
mod window_tests_branches_tree;
#[cfg(test)]
mod window_tests_diff_editor;
#[cfg(test)]
mod window_tests_diff_own_view;
#[cfg(test)]
mod window_tests_keystroke_repaint;
#[cfg(test)]
mod window_tests_run_output;

#[cfg(test)]
mod window_tests_cached_views;

#[cfg(test)]
mod window_tests_keys_macos;
#[cfg(test)]
mod window_tests_mask_hint;

#[cfg(test)]
mod window_tests_release;

#[cfg(test)]
mod window_tests_cached_tails;
#[cfg(test)]
mod window_tests_close_terminal;
#[cfg(test)]
mod window_tests_diff_shared;

#[cfg(test)]
mod window_tests_stripe_notifications;

#[cfg(test)]
mod window_tests_diff_views;

#[cfg(test)]
mod window_tests_macos_menu;

mod choice;
mod invalidate;

use std::rc::Rc;
use std::sync::Arc;

use ::platform::CanonPath;
use editor_view::{EditorView, Languages, Semantic};
use fs_watch::FsChange;
use futures_channel::mpsc::UnboundedReceiver;
use futures_util::StreamExt;
use lang_api::{FileChange, LanguageSupport, ProjectProblemsQuery, ask};
use tool_gateway::LanguageTools;
use ui_kit::*;
use workspace::{
    AI_TOOL, COMMIT_TOOL, DEBUG_TOOL, DOCUMENTATION_TOOL, DockSide, INSPECTION_TOOL, LOG_TOOL,
    Launch, PERFORMANCE_TOOL, PROBLEMS_TOOL, PROJECT_TOOL, RUN_TOOL, SERVICES_TOOL, STRUCTURE_TOOL,
    TASKS_TOOL, TERMINAL_TOOL, TODO_TOOL, USAGES_TOOL, Workspace,
};

use crate::index::{IndexEvent, ProjectIndex};
use crate::navigation::Navigator;

pub use agents::{AgentLanguages, AgentOptions};
pub use debug::{ActiveDebug, BreakpointsChanged, DebugPanel};
pub use index::SearchSetup;
pub use inspect::{ActiveInspect, Finding, InspectHub};
pub use invalidate::CacheLocations;
pub use problems::{ActiveProblems, Problem, ProblemsHub, ProblemsState};
pub use run::ProjectSetup;
pub use script::parse_script;
pub use settings_templates::TemplatesSetup;
pub use start::{ProjectTemplate, run as run_start};
pub use tree_ops::{Creates, FileTemplate};
pub use usages::{ActiveUsages, UsagesHub};

/// Names the build tools of the language for the pages and the search of Settings.
pub fn set_build_tool_names(first: impl Into<String>, second: impl Into<String>) {
    settings_catalog::set_build_tool_names(first, second);
}

/// The language server of the open project, started by the application.
pub struct LanguageServer {
    /// `None` when it did not start; the status says why.
    pub support: Option<Arc<dyn LanguageSupport>>,
    /// Lines for the status bar. The first line of a text is the line itself; the
    /// lines after it explain the state in a balloon when the line is clicked.
    pub status: UnboundedReceiver<String>,
}

/// What the balloon behind a status line offers besides the README when the server
/// did not start: a page with the missing tool and a second search for it.
pub struct LanguageSetup {
    /// The label of the button and the page it opens.
    pub download: Option<(SharedString, String)>,
    /// Searches for the missing tool again.
    pub check: Option<Box<dyn SetupCheck>>,
}

/// A second search for a tool that a language server needs, run by the application.
pub trait SetupCheck {
    /// Sends `Some` when the server started and `None` when a tool is still missing.
    fn check(&self, started: futures_channel::mpsc::UnboundedSender<Option<StartedServer>>);
}

/// A language server that a second search started.
pub struct StartedServer {
    pub support: Arc<dyn LanguageSupport>,
    pub status: UnboundedReceiver<String>,
    /// The debugger of the language, when it has one.
    pub debug: Option<Arc<dyn dap_client::DebugService>>,
}

/// Runs the application. `languages` says what a file is, `search` what the project
/// search needs to know about the language, `project` how it builds and runs, `server` answers
/// about the code, `script`
/// drives the window without real input, see `script.rs`.
pub fn run(
    launch: Launch,
    languages: Languages,
    search: SearchSetup,
    project: ProjectSetup,
    server: Option<LanguageServer>,
    mock_agents: bool,
    script: Option<String>,
) {
    run_with_setup(
        launch,
        languages,
        search,
        project,
        server,
        None,
        CacheLocations::default(),
        TemplatesSetup::default(),
        mock_agents,
        script,
    )
}

/// [`run`] with the hint of a tool that was not found: the balloon offers its page and
/// a second search that starts the server without restarting Arro.
#[allow(clippy::too_many_arguments)]
pub fn run_with_setup(
    launch: Launch,
    languages: Languages,
    search: SearchSetup,
    project: ProjectSetup,
    server: Option<LanguageServer>,
    setup: Option<LanguageSetup>,
    cache: CacheLocations,
    templates: TemplatesSetup,
    mock_agents: bool,
    script: Option<String>,
) {
    let agents = AgentOptions {
        state_dir: launch.state_dir.clone(),
        mock: mock_agents,
    };
    workspace::run(launch, move |workspace, window, cx| {
        cx.set_global(languages);
        cx.set_global(cache);
        cx.set_global(templates.initial());
        install(workspace, search, project, agents, window, cx);
        settings_templates::load(workspace, cx);
        if let Some(server) = server {
            connect(workspace, server, cx);
        }
        if let Some(setup) = setup {
            apply_setup(workspace, setup, cx);
        }
        if let Some(script) = script {
            script::start(&script, window, cx);
        }
    });
}

fn connect(workspace: &mut Workspace, server: LanguageServer, cx: &mut Context<Workspace>) {
    match server.support {
        Some(support) => connect_server(
            workspace,
            StartedServer {
                support,
                status: server.status,
                debug: None,
            },
            cx,
        ),
        None => watch_status(workspace, server.status, cx),
    }
}

/// Connects a server that just started, at the beginning or after a second search.
pub fn connect_server(
    workspace: &mut Workspace,
    started: StartedServer,
    cx: &mut Context<Workspace>,
) {
    let StartedServer {
        support,
        status,
        debug,
    } = started;
    cx.global::<AgentLanguages>().0.register(support.clone());
    if let Some(debug) = debug
        && let Some(panel) = cx
            .try_global::<script::ActiveRunPanel>()
            .map(|panel| panel.0.clone())
    {
        panel.update(cx, |panel, _| panel.set_debug_service(debug));
    }
    let restart = support.clone();
    cx.set_global(Semantic(support.clone()));
    workspace.register_command(
        "language.restart_server",
        "Restart Language Server",
        move |workspace, _, cx| {
            let text = if restart.restart_server() {
                "Starting the language server again"
            } else {
                "The language server is not stopped"
            };
            workspace.notify_user("Language Server", text, cx);
        },
    );
    connect_project_problems(support, cx);
    watch_status(workspace, status, cx);
}

/// Adds the buttons of a missing tool to the balloon behind the status line and the
/// command that searches again.
pub fn apply_setup(workspace: &mut Workspace, setup: LanguageSetup, cx: &mut Context<Workspace>) {
    let check = setup.check.map(Rc::new);
    let has_check = check.is_some();
    workspace.set_language_setup(setup.download, has_check, cx);
    let Some(check) = check else {
        return;
    };
    workspace.register_command(
        workspace::CHECK_LANGUAGE_SETUP,
        "Check Again",
        move |_workspace, _window, cx| {
            let (sender, receiver) = futures_channel::mpsc::unbounded();
            check.check(sender);
            cx.spawn(async move |workspace, cx| {
                let mut receiver = receiver;
                while let Some(started) = receiver.next().await {
                    let Some(started) = started else {
                        let updated = workspace.update(cx, |workspace, cx| {
                            workspace.notify_user(
                                "Language Server",
                                "The tool was not found. Install it, then press Check again.",
                                cx,
                            );
                        });
                        if updated.is_err() {
                            break;
                        }
                        continue;
                    };
                    let updated = workspace.update(cx, |workspace, cx| {
                        workspace.set_language_setup(None, false, cx);
                        connect_server(workspace, started, cx);
                    });
                    if updated.is_err() {
                        break;
                    }
                }
            })
            .detach();
        },
    );
}

fn watch_status(
    _workspace: &mut Workspace,
    mut status: UnboundedReceiver<String>,
    cx: &mut Context<Workspace>,
) {
    cx.spawn(async move |workspace, cx| {
        // A server that gave up says so once per run, not on every crash.
        let mut failure_shown = false;
        while let Some(text) = status.next().await {
            let reason = if failure_shown {
                None
            } else {
                failed_reason(&text).map(str::to_owned)
            };
            let shown = workspace.update(cx, |workspace, cx| {
                if let Some(reason) = reason.clone() {
                    workspace.notify_user_with_command(
                        "Language Server",
                        reason,
                        "Retry",
                        "language.restart_server",
                        cx,
                    );
                }
                workspace.set_language_status(Some(text.into()), cx)
            });
            if shown.is_err() {
                break;
            }
            if reason.is_some() {
                failure_shown = true;
            }
        }
    })
    .detach();
}

/// The reason in a status line of the form `<language>: failed: <reason>`. A server
/// that gave up asks for one notification per run with a Retry button.
fn failed_reason(text: &str) -> Option<&str> {
    let (_, reason) = text.split_once(": failed: ")?;
    let reason = reason.trim();
    (!reason.is_empty()).then_some(reason)
}

/// Takes the problems the server reports about the project itself. A build script
/// that could not be imported lands in Problems, in a notification and, through the
/// status line of the language, in the status bar.
fn connect_project_problems(support: Arc<dyn LanguageSupport>, cx: &mut Context<Workspace>) {
    cx.spawn(async move |workspace, cx| {
        let Some(provider) = support.project_problems() else {
            return;
        };
        let mut replies = ask(provider, ProjectProblemsQuery);
        let mut notified = String::new();
        while let Some(answer) = replies.next().await {
            let problems = answer.value;
            match problems.first() {
                Some(problem) if problem.summary != notified => {
                    notified = problem.summary.clone();
                    let text = format!("import failed: {}", problem.summary);
                    let updated = workspace.update(cx, |workspace, cx| match &problem.log {
                        Some(log) => workspace.notify_user_with_action(
                            "Language Server",
                            text,
                            "Show Log",
                            log.clone(),
                            cx,
                        ),
                        None => workspace.notify_user("Language Server", text, cx),
                    });
                    if updated.is_err() {
                        break;
                    }
                }
                Some(_) => {}
                None => notified.clear(),
            }
            let updated = workspace.update(cx, |_, cx| {
                if let Some(active) = cx.try_global::<ActiveProblems>() {
                    let hub = active.0.clone();
                    hub.update(cx, |hub, cx| hub.set_project_problems(&problems, cx));
                }
            });
            if updated.is_err() {
                break;
            }
        }
    })
    .detach();
}

fn install(
    workspace: &mut Workspace,
    search: SearchSetup,
    project: ProjectSetup,
    agents: AgentOptions,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    history::open(&agents.state_dir, cx);
    // The open editors take a keymap file of the user as soon as it is applied.
    workspace.set_keymap_listener(editor_view::install_keymap);
    project::bind_keys(cx);
    folder_compare::bind_keys(cx);
    items::bind_keys(cx);
    palette::bind_keys(cx);
    finder::bind_keys(cx);
    plugins::bind_keys(cx);
    usages::bind_keys(cx);

    let mut templates = tree_ops::basic_templates();
    templates.extend(project.templates.iter().cloned());
    cx.set_global(project::FileTemplates(templates));

    cx.set_global(outline::SymbolsFor(search.symbols_for));
    let weak = cx.weak_entity();
    workspace.set_editor_factory(editor_tab::factory(weak.clone()));
    let git = git::GitState::create(workspace.root().to_path_buf(), weak.clone(), cx);
    cx.set_global(git::ActiveGit(git.clone()));
    git::bind_actions(git.clone(), window.window_handle(), cx);
    let state = git.clone();
    // A window in the background does not ask Git: the change is read when it comes back.
    // The observer runs while the workspace is being updated, and reading the status
    // reads the workspace, so the walk to Git waits until that update is over.
    cx.observe_window_activation(window, move |_, window, cx| {
        let active = window.is_window_active();
        let state = state.clone();
        cx.defer(move |cx| {
            state.update(cx, |state, cx| state.window_activation(active, cx));
        });
    })
    .detach();
    settings::bind_actions(weak.clone(), window.window_handle(), cx);
    project_structure::bind_actions(weak.clone(), window.window_handle(), cx);
    cx.set_global(project_structure::ProjectEnvironmentGlobal(
        project.environment.clone(),
    ));
    let panel = project::ProjectPanel::create(workspace, weak.clone(), window, cx);
    let focus = panel.read(cx).focus();
    workspace.set_dock(
        DockSide::Left,
        PROJECT_TOOL,
        panel.clone().into(),
        Some(focus),
        cx,
    );
    project::bind_actions(panel.clone(), weak.clone(), window.window_handle(), cx);
    cx.set_global(project::ActiveProjectPanel(panel));
    let log = git_log::GitLogPanel::create(git.clone(), weak.clone(), cx);
    let focus = log.read(cx).focus();
    workspace.set_dock(
        DockSide::Bottom,
        LOG_TOOL,
        log.clone().into(),
        Some(focus),
        cx,
    );
    cx.set_global(git_log::ActiveGitLog(log));
    let root = workspace.root().to_path_buf();
    let commit = commit::CommitPanel::create(
        git,
        weak.clone(),
        root,
        workspace.state_dir().to_path_buf(),
        workspace.store(),
        window,
        cx,
    );
    let focus = commit.read(cx).focus();
    workspace.set_dock(
        DockSide::Left,
        COMMIT_TOOL,
        commit.clone().into(),
        Some(focus),
        cx,
    );
    cx.set_global(script::ActiveCommitPanel(commit));
    let structure = structure::StructurePanel::create(weak.clone(), cx);
    let focus = structure.read(cx).focus();
    workspace.set_dock(
        DockSide::Left,
        STRUCTURE_TOOL,
        structure.clone().into(),
        Some(focus),
        cx,
    );
    cx.set_global(structure::ActiveStructure(structure));

    let terminal = terminal_panel::TerminalPanel::create(
        workspace.root().to_path_buf(),
        project.environment.clone(),
        cx,
    );
    let focus = terminal.read(cx).focus();
    workspace.set_dock(
        DockSide::Bottom,
        TERMINAL_TOOL,
        terminal.clone().into(),
        Some(focus),
        cx,
    );
    cx.set_global(terminal_panel::ActiveTerminal(terminal));

    let root = workspace.root().to_path_buf();
    let services = docker::DockerPanel::create(root, weak.clone(), window.window_handle(), cx);
    let focus = services.read(cx).focus();
    workspace.set_dock(
        DockSide::Bottom,
        SERVICES_TOOL,
        services.clone().into(),
        Some(focus),
        cx,
    );
    cx.set_global(docker::ActiveDocker(services));

    let root = workspace.root().to_path_buf();
    let runs = run::RunPanel::create(root, project.clone(), weak.clone(), cx);
    let focus = runs.read(cx).focus();
    workspace.set_dock(
        DockSide::Bottom,
        RUN_TOOL,
        runs.clone().into(),
        Some(focus),
        cx,
    );
    workspace.set_run_controls(run::RunPanel::controls(&runs));
    cx.set_global(script::ActiveRunPanel(runs.clone()));

    let debug = debug::DebugPanel::create(
        workspace.root().to_path_buf(),
        workspace.store(),
        weak.clone(),
        cx,
    );
    let focus = debug.read(cx).focus();
    workspace.set_dock(
        DockSide::Bottom,
        DEBUG_TOOL,
        debug.clone().into(),
        Some(focus),
        cx,
    );
    workspace.set_debug_controls(debug::controls(&debug, &runs));
    // The Stop button of the title bar follows the debug session, and the title bar is a
    // cached view that redraws with the window: the window redraws when the session
    // starts, stops or ends.
    let mut shown = debug.read(cx).state();
    cx.observe(&debug, move |_, debug, cx| {
        let state = debug.read(cx).state();
        if state != shown {
            shown = state;
            cx.notify();
        }
    })
    .detach();
    cx.set_global(debug::ActiveDebug(debug));

    let hub = Arc::new(debug::DebugHub::default());
    let languages = Arc::new(LanguageTools::new());
    languages.register_debug(workspace.root().to_path_buf(), hub.clone());
    cx.set_global(debug::ActiveDebugHub(hub));
    cx.set_global(AgentLanguages(languages));
    let agent_panel = agents::AgentsPanel::create(
        workspace,
        weak.clone(),
        agents,
        project.environment.clone(),
        cx,
    );
    let focus = agent_panel.read(cx).focus();
    workspace.set_dock(
        DockSide::Right,
        AI_TOOL,
        agent_panel.clone().into(),
        Some(focus),
        cx,
    );
    cx.set_global(agents::ActiveAgents(agent_panel));

    let tasks = build::BuildPanel::create(runs, weak.clone(), cx);
    let focus = tasks.read(cx).focus();
    workspace.set_dock(
        DockSide::Right,
        TASKS_TOOL,
        tasks.clone().into(),
        Some(focus),
        cx,
    );
    cx.set_global(script::ActiveBuildPanel(tasks));

    let window_handle = window.window_handle();
    let user_plugins = plugins::Plugins::create(workspace, window_handle, cx);
    cx.set_global(plugins::ActivePlugins(user_plugins));

    let root = workspace.root().to_path_buf();
    cx.set_global(todo::SkippedFolders(project.skipped_folders.clone()));
    let todo_panel = todo::create(root.clone(), weak.clone(), cx);
    let focus = todo_panel.read(cx).focus();
    workspace.set_dock(
        DockSide::Bottom,
        TODO_TOOL,
        todo_panel.clone().into(),
        Some(focus),
        cx,
    );
    cx.set_global(todo::ActiveTodo(todo_panel));

    let problems = problems::ProblemsHub::create(root.clone(), weak.clone(), cx);
    let problems_panel = problems.read(cx).panel().clone();
    let focus = problems_panel.read(cx).focus();
    workspace.set_dock(
        DockSide::Bottom,
        PROBLEMS_TOOL,
        problems_panel.into(),
        Some(focus),
        cx,
    );
    problems::bind_actions(problems.clone(), weak.clone(), window.window_handle(), cx);
    cx.set_global(problems::ActiveProblems(problems));

    let usages = usages::UsagesHub::create(root.clone(), weak.clone(), cx);
    let usages_panel = usages.read(cx).panel().clone();
    let focus = usages_panel.read(cx).focus();
    workspace.set_dock(
        DockSide::Bottom,
        USAGES_TOOL,
        usages.clone().into(),
        Some(focus),
        cx,
    );
    cx.set_global(usages::ActiveUsages(usages));

    let performance = performance::PerformancePanel::create(weak.clone(), cx);
    let focus = performance.read(cx).focus();
    workspace.set_dock(
        DockSide::Bottom,
        PERFORMANCE_TOOL,
        performance.clone().into(),
        Some(focus),
        cx,
    );
    cx.set_global(performance::ActivePerformance(performance));

    let documentation = documentation::DocumentationPanel::create(weak.clone(), cx);
    let focus = documentation.read(cx).focus();
    workspace.set_dock(
        DockSide::Right,
        DOCUMENTATION_TOOL,
        documentation.clone().into(),
        Some(focus),
        cx,
    );
    cx.set_global(documentation::ActiveDocumentation(documentation));

    let call_hierarchy = call_hierarchy::CallHierarchyPanel::create(weak.clone(), cx);
    let focus = call_hierarchy.read(cx).focus();
    workspace.set_dock(
        DockSide::Bottom,
        workspace::CALL_HIERARCHY_TOOL,
        call_hierarchy.clone().into(),
        Some(focus),
        cx,
    );
    cx.set_global(call_hierarchy::ActiveCallHierarchy(call_hierarchy));

    let method_hierarchy = method_hierarchy::MethodHierarchyPanel::create(weak.clone(), cx);
    let focus = method_hierarchy.read(cx).panel().read(cx).focus();
    workspace.set_dock(
        DockSide::Bottom,
        workspace::METHOD_HIERARCHY_TOOL,
        method_hierarchy.read(cx).panel().clone().into(),
        Some(focus),
        cx,
    );
    cx.set_global(method_hierarchy::ActiveMethodHierarchy(method_hierarchy));

    let keymap_file = workspace.keymap_path().to_path_buf();
    let templates_file = workspace.settings_dir().join(templates::TEMPLATES_FILE);
    let (index, events) =
        ProjectIndex::start(root.clone(), vec![keymap_file, templates_file], search);
    cx.set_global(crate::index::ActiveIndex(index.clone()));
    let files = cx.new(|_| Arc::new(Vec::new()));
    let runs = cx.global::<script::ActiveRunPanel>().0.clone();
    runs.update(cx, |runs, _| runs.set_files(files.clone()));
    commands::register(workspace, &index, &files);
    window_features::register(workspace, window, cx);
    let inspect = inspect::InspectHub::create(
        root,
        files.downgrade(),
        weak.clone(),
        window.window_handle(),
        cx,
    );
    let panel = inspect.read(cx).panel().clone();
    let focus = panel.read(cx).focus();
    workspace.set_dock(
        DockSide::Bottom,
        INSPECTION_TOOL,
        panel.into(),
        Some(focus),
        cx,
    );
    cx.set_global(inspect::ActiveInspect(inspect.clone()));
    inspect::bind_commands(inspect, workspace, cx);
    reformat::bind_commands(workspace, cx);
    workspace.set_search({
        let (index, files) = (index.clone(), files.clone());
        move |workspace, window, cx| {
            let mode = palette::Mode::Everywhere(search::Tab::All);
            palette::Palette::show(mode, index.clone(), files.clone(), workspace, window, cx)
        }
    });
    workspace.set_config_choose({
        let (index, files) = (index.clone(), files.clone());
        move |debug, workspace, window, cx| {
            let mode = palette::Mode::ChooseRun { debug };
            palette::Palette::show(mode, index.clone(), files.clone(), workspace, window, cx)
        }
    });
    workspace.set_tab_listener(|path, cx| {
        cx.default_global::<Navigator>().activated(path);
        let panel = cx.global::<structure::ActiveStructure>().0.clone();
        panel.update(cx, |panel, cx| panel.show(path.to_path_buf(), cx));
        let documentation = cx.global::<documentation::ActiveDocumentation>().0.clone();
        documentation.update(cx, |panel, cx| panel.show_path(path.to_path_buf(), cx));
    });
    // What the watcher missed shows up when the window comes back to the front.
    let rescan = index.clone();
    cx.observe_window_activation(window, move |_, window, _| {
        if window.is_window_active() {
            rescan.rescan();
        }
    })
    .detach();
    watch_project(events, files, cx);
    release_on_close(window, cx);
}

/// The window of a project is the only one of its process: nothing may keep its panels,
/// its git state or the watchers of its index for the rest of the run after it closes.
fn release_on_close(window: &Window, cx: &mut Context<Workspace>) {
    let closed_window = window.window_handle().window_id();
    cx.on_window_closed(move |cx, closed| {
        if closed == closed_window {
            release_globals(cx);
        }
    })
    .detach();
}

/// Forgets the globals that hold the entities of a closed window.
fn release_globals(cx: &mut App) {
    macro_rules! forget {
        ($($global:ty),* $(,)?) => {
            $(
                if cx.try_global::<$global>().is_some() {
                    cx.remove_global::<$global>();
                }
            )*
        };
    }
    forget![
        agents::ActiveAgents,
        call_hierarchy::ActiveCallHierarchy,
        debug::ActiveDebug,
        debug::ActiveDebugHub,
        docker::ActiveDocker,
        documentation::ActiveDocumentation,
        finder::ActiveFinder,
        git::ActiveGit,
        git_log::ActiveGitLog,
        index::ActiveIndex,
        inspect::ActiveInspect,
        method_hierarchy::ActiveMethodHierarchy,
        performance::ActivePerformance,
        plugins::ActivePlugins,
        problems::ActiveProblems,
        project::ActiveProjectPanel,
        script::ActiveBuildPanel,
        script::ActiveCommitPanel,
        script::ActiveRunPanel,
        structure::ActiveStructure,
        terminal_panel::ActiveTerminal,
        todo::ActiveTodo,
        usages::ActiveFindUsagesSettings,
        usages::ActiveShowUsages,
        usages::ActiveUsages,
    ];
}

/// Takes what the index thread reports: the file list for the search, and changes on
/// disk for the tree and the open tabs.
fn watch_project(
    mut events: futures_channel::mpsc::UnboundedReceiver<IndexEvent>,
    files: Entity<Arc<Vec<std::path::PathBuf>>>,
    cx: &mut Context<Workspace>,
) {
    let weak_files = files.downgrade();
    cx.spawn(async move |this, cx| {
        while let Some(event) = events.next().await {
            match event {
                IndexEvent::Files(list) => {
                    let updated = weak_files.update(cx, |files, cx| {
                        *files = list;
                        cx.notify();
                    });
                    if updated.is_err() {
                        break;
                    }
                }
                IndexEvent::Disk(changes) => {
                    let applied =
                        this.update(cx, |workspace, cx| disk_changed(workspace, &changes, cx));
                    if applied.is_err() {
                        break;
                    }
                }
                IndexEvent::Watched(changes) => {
                    let applied = this.update(cx, |workspace, cx| {
                        watched_changed(workspace, &changes, cx);
                    });
                    if applied.is_err() {
                        break;
                    }
                }
                IndexEvent::Settings => {
                    // A file of the settings folder changed on disk: the same read as
                    // an edit of the Settings page, without a clock.
                    let applied = this.update(cx, |workspace, cx| {
                        workspace.reload_keymap(cx);
                        settings_templates::load(workspace, cx);
                    });
                    if applied.is_err() {
                        break;
                    }
                }
                IndexEvent::Repository => {
                    // The status is read in the background and only when the window shows it.
                    let applied = this.update(cx, |_, cx| {
                        let Some(git) = cx.try_global::<crate::git::ActiveGit>() else {
                            return;
                        };
                        let git = git.0.clone();
                        git.update(cx, |git, cx| git.repository_changed(cx));
                    });
                    if applied.is_err() {
                        break;
                    }
                }
            }
        }
    })
    .detach();
}

/// The raw batch of the watcher: a folder the table leaves out, such as the metadata
/// folder under `.gitignore`, still has files the window keeps in step.
fn watched_changed(workspace: &Workspace, changes: &[FsChange], cx: &mut Context<Workspace>) {
    let file = runner::configs_file(workspace.root());
    let touched = changes.iter().any(|change| match change {
        FsChange::Created(path) | FsChange::Changed(path) | FsChange::Removed(path) => {
            path == &file
        }
        // A rescan may have missed the file the table leaves out.
        FsChange::Rescan => true,
    });
    if !touched {
        return;
    }
    let panel = cx.global::<script::ActiveRunPanel>().0.clone();
    panel.update(cx, |panel, cx| panel.store_changed(cx));
}

fn disk_changed(workspace: &Workspace, changes: &[FsChange], cx: &mut Context<Workspace>) {
    // The language server registered the files it watches, such as build scripts. A
    // save of one comes through the watcher even when no editor is open on it.
    let mut model_changed = false;
    if let Some(semantic) = cx.try_global::<Semantic>() {
        let support = semantic.0.clone();
        if let Some(server) = support.server() {
            for change in changes {
                let (path, kind) = match change {
                    FsChange::Created(path) => (path, FileChange::Created),
                    FsChange::Changed(path) => (path, FileChange::Changed),
                    FsChange::Removed(path) => (path, FileChange::Removed),
                    FsChange::Rescan => continue,
                };
                if support.watches_file(path)
                    && let Ok(path) = CanonPath::new(path)
                {
                    server.file_changed(&path.to_uri(), kind);
                    model_changed = true;
                }
            }
        }
    }
    // A new, removed or changed build script can name other modules: the window reads
    // the model again as it does when the build tool window asks for a reload.
    if model_changed {
        let panel = cx.global::<script::ActiveRunPanel>().0.clone();
        panel.update(cx, |panel, cx| panel.model_file_changed(cx));
    }
    let mut folders = Vec::new();
    for change in changes {
        match change {
            FsChange::Changed(path) => {
                let view = workspace.view_of(path);
                if let Some(view) = view.and_then(|view| view.downcast::<EditorView>().ok()) {
                    // A clean buffer that really takes other text says so with an
                    // `ExternalChange` event, which keeps the text it showed.
                    view.update(cx, |view, cx| view.reload(cx));
                }
            }
            FsChange::Created(path) | FsChange::Removed(path) => {
                if let Some(parent) = path.parent() {
                    folders.push(parent.to_path_buf());
                }
            }
            FsChange::Rescan => {}
        }
    }
    folders.sort();
    folders.dedup();
    if folders.is_empty() {
        return;
    }
    let panel = cx.global::<project::ActiveProjectPanel>().0.clone();
    panel.update(cx, |panel, cx| {
        for folder in folders {
            panel.refresh_folder(&folder, cx);
        }
    });
}
