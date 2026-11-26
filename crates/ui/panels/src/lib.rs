//! Tool windows and popups on top of the workspace.

mod agent_clients;
mod agent_hunk;
mod agent_tools;
mod agents;
mod ai_review;
mod annotate;
mod ansi;
mod beans;
mod branches;
mod build;
mod build_output;
mod bundle_editor;
mod call_hierarchy;
mod change_signature;
mod change_tree;
mod changelists;
mod changes;
mod chat;
mod claude_ide;
mod clipboard;
mod commands;
mod commit;
mod compare;
mod composer;
mod conflict;
mod console_follow;
mod console_input;
mod console_links;
mod console_search;
mod copy_class;
mod coverage;
mod create_test;
mod database_console;
pub mod database_data;
pub mod database_panel;
mod debug;
mod diagram;
mod diagram_canvas;
mod diagram_layout;
mod diagram_svg;
mod diff_view;
mod docker;
mod documentation;
mod editor_menu;
mod editor_tab;
mod endpoints;
mod file_tree;
mod finder;
mod fix_with_ai;
mod folder_compare;
mod git;
mod git_history;
mod git_log;
mod gutter_menu;
pub mod heap_dump;
mod hierarchy_scope;
mod history;
mod history_view;
mod http;
mod hunk_popup;
mod idea_import;
mod index;
mod inspect;
mod inspection;
mod items;
pub mod jshell_panel;
mod kubernetes;
mod library_sources;
mod live_panel;
mod marks;
mod mcp_approval;
mod mcp_database;
mod mcp_debug;
mod mcp_edits;
mod mcp_runs;
mod member_form;
mod memory_stop;
mod merge_view;
mod method_hierarchy;
mod move_members;
mod move_targets;
mod navigation;
mod new_project;
mod outline;
mod palette;
mod performance;
mod places;
mod plugins;
mod preview;
mod problems;
mod profiler;
mod project;
mod project_state;
mod project_structure;
mod prompt;
mod proposal;
mod push;
mod recent_changes;
mod refactor_form;
mod refactor_undo;
mod reformat;
mod run;
mod run_configs;
mod script;
mod select_in;
mod sequence_diagram;
mod sequence_export;
mod sequence_layout;
mod settings;
mod settings_archive;
mod settings_catalog;
mod settings_file_types;
mod settings_keymap;
mod settings_templates;
mod shelf;
mod spelling;
mod spring_data;
mod stack_trace_dialog;
mod stack_traces;
mod start;
mod structural;
mod structure;
mod term_grid;
mod term_links;
mod term_tabs;
mod terminal_panel;
mod theme_import;
mod todo;
mod transcript;
mod tree_ops;
mod trust;
mod type_hierarchy;
mod usages;
mod window_features;
mod window_popups;

#[cfg(feature = "mock-agents")]
mod mock_transcript;

#[cfg(test)]
mod test_alloc;
#[cfg(test)]
mod window_tests;
#[cfg(test)]
mod window_tests_actions;
#[cfg(test)]
mod window_tests_actions_on_save;
#[cfg(test)]
mod window_tests_actuator;
#[cfg(test)]
mod window_tests_ai;
#[cfg(test)]
mod window_tests_ai_review;
#[cfg(test)]
mod window_tests_ai_step2;
#[cfg(test)]
mod window_tests_annotate;
#[cfg(test)]
mod window_tests_complete;
#[cfg(test)]
mod window_tests_completion;
#[cfg(test)]
mod window_tests_debug_actions;
#[cfg(test)]
mod window_tests_diagram;
#[cfg(test)]
mod window_tests_edit;
#[cfg(test)]
mod window_tests_editing;
#[cfg(test)]
mod window_tests_file_syntax;
#[cfg(test)]
mod window_tests_first_open;
#[cfg(test)]
mod window_tests_git;
#[cfg(test)]
mod window_tests_git_history_selection;
#[cfg(test)]
mod window_tests_heap_dump;
#[cfg(test)]
mod window_tests_hierarchy;
#[cfg(test)]
mod window_tests_hotswap;
#[cfg(test)]
mod window_tests_http_client;
#[cfg(test)]
mod window_tests_http_curl;
#[cfg(test)]
mod window_tests_import;
#[cfg(test)]
mod window_tests_keys;
#[cfg(test)]
mod window_tests_lang;
#[cfg(test)]
mod window_tests_layout;
#[cfg(test)]
mod window_tests_library_sources;
#[cfg(test)]
mod window_tests_live;
#[cfg(test)]
mod window_tests_log_graph;
#[cfg(test)]
mod window_tests_member;
#[cfg(test)]
mod window_tests_type_hierarchy;

#[cfg(test)]
mod window_tests_appearance;
#[cfg(test)]
mod window_tests_lookup;
#[cfg(test)]
mod window_tests_m11;
#[cfg(test)]
mod window_tests_markdown_images;
#[cfg(test)]
mod window_tests_menu_main;
#[cfg(test)]
mod window_tests_menus;
#[cfg(test)]
mod window_tests_nested_splits;
#[cfg(test)]
mod window_tests_perf;
#[cfg(test)]
mod window_tests_real;
#[cfg(test)]
mod window_tests_real_actions_on_save;
#[cfg(test)]
mod window_tests_real_drag;
#[cfg(test)]
mod window_tests_refactor_copy;
#[cfg(test)]
mod window_tests_refactor_extract;
#[cfg(test)]
mod window_tests_refactor_this;
#[cfg(test)]
mod window_tests_run;
#[cfg(test)]
mod window_tests_safe_delete;
#[cfg(test)]
mod window_tests_signature;
#[cfg(test)]
mod window_tests_snippets;
#[cfg(test)]
mod window_tests_speed;
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
mod window_tests_database;
#[cfg(test)]
mod window_tests_database_console;
#[cfg(test)]
mod window_tests_database_data;
#[cfg(test)]
mod window_tests_database_driver;
#[cfg(test)]
mod window_tests_database_passwords;
#[cfg(test)]
mod window_tests_database_step2;

#[cfg(test)]
mod window_tests_jshell;

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
mod window_tests_structural_search;

#[cfg(test)]
mod window_tests_stable;

#[cfg(test)]
mod window_tests_index_isolation;

#[cfg(test)]
mod window_tests_inspect;

#[cfg(test)]
mod window_tests_sequence_diagram;

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
mod window_tests_icon_sets;
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
mod window_tests_dependencies;

#[cfg(test)]
mod window_tests_dependency_updates;

#[cfg(test)]
mod window_tests_server_setup;

#[cfg(test)]
mod window_tests_build_tool;
#[cfg(test)]
mod window_tests_run_configs;
#[cfg(test)]
mod window_tests_run_idea;
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
mod window_tests_project_structure_modules;

#[cfg(test)]
mod window_tests_branches;
#[cfg(test)]
mod window_tests_debug_depth;
#[cfg(test)]
mod window_tests_idle_events;

#[cfg(test)]
mod window_tests_inlay_hints;
#[cfg(test)]
mod window_tests_inline_values;
#[cfg(test)]
mod window_tests_lookup_idea;
#[cfg(test)]
mod window_tests_menu_gaps;
#[cfg(test)]
mod window_tests_mouse_navigation;
#[cfg(test)]
mod window_tests_scrollbars;
#[cfg(test)]
mod window_tests_semantic_tokens;

#[cfg(test)]
mod window_tests_resource_bundles;

#[cfg(test)]
mod window_tests_bundle_buffers;
#[cfg(test)]
mod window_tests_bundle_editor;
#[cfg(test)]
mod window_tests_bundle_save;
#[cfg(test)]
mod window_tests_exit_buffers;

#[cfg(test)]
mod window_tests_spring_config;
#[cfg(test)]
mod window_tests_spring_config_nav;

#[cfg(test)]
mod window_tests_kubernetes;

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
mod window_tests_debug_layout;
#[cfg(test)]
mod window_tests_diff_side;
#[cfg(test)]
mod window_tests_folder_compare;
#[cfg(test)]
mod window_tests_navigation_polish;
#[cfg(test)]
mod window_tests_new_project;

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
mod window_tests_markdown_inline;
#[cfg(test)]
mod window_tests_markdown_preview;
#[cfg(test)]
mod window_tests_mask_hint;
#[cfg(test)]
mod window_tests_mcp;
#[cfg(test)]
mod window_tests_mcp_approval;
#[cfg(test)]
mod window_tests_mcp_database;
#[cfg(test)]
mod window_tests_mcp_debug;
#[cfg(test)]
mod window_tests_mcp_idea;
#[cfg(test)]
mod window_tests_mcp_runs;
#[cfg(test)]
mod window_tests_mcp_settings;

#[cfg(test)]
mod window_tests_mcp_xdebug;

#[cfg(test)]
mod window_tests_release;

#[cfg(test)]
mod window_tests_cached_tails;
#[cfg(test)]
mod window_tests_close_terminal;
#[cfg(test)]
mod window_tests_create_test;
#[cfg(test)]
mod window_tests_diff_shared;
#[cfg(test)]
mod window_tests_terminal_actions;
#[cfg(test)]
mod window_tests_terminal_leak;
#[cfg(test)]
mod window_tests_terminal_tabs;

#[cfg(test)]
mod window_tests_stripe_notifications;

#[cfg(test)]
mod window_tests_script_smoke;

#[cfg(test)]
mod window_tests_settings_window;

#[cfg(test)]
mod window_tests_inspection_settings;

#[cfg(test)]
mod window_tests_diff_views;

#[cfg(test)]
mod window_tests_merge;

#[cfg(test)]
mod window_tests_merge_growth;

#[cfg(test)]
mod window_tests_macos_menu;

#[cfg(test)]
mod window_tests_breakpoint_kinds;
#[cfg(test)]
mod window_tests_field_caret;

#[cfg(test)]
mod window_tests_log_actions;

#[cfg(test)]
mod window_tests_interactive_rebase;
#[cfg(test)]
mod window_tests_overlap;
#[cfg(test)]
mod window_tests_services;
#[cfg(test)]
mod window_tests_services_runs;

#[cfg(test)]
mod window_tests_coverage;
#[cfg(test)]
mod window_tests_endpoints;
#[cfg(test)]
mod window_tests_idle_polling;
#[cfg(test)]
mod window_tests_logpoints;
#[cfg(test)]
mod window_tests_profiler;
#[cfg(test)]
mod window_tests_profiler_allocations;
#[cfg(test)]
mod window_tests_spelling;
#[cfg(test)]
mod window_tests_spring_beans;
#[cfg(test)]
mod window_tests_spring_data;
#[cfg(test)]
mod window_tests_stack_library_note;
#[cfg(test)]
mod window_tests_stack_refinement;
#[cfg(test)]
mod window_tests_stack_traces;

#[cfg(test)]
mod window_tests_database_polish;
#[cfg(test)]
mod window_tests_idea_import;
#[cfg(test)]
mod window_tests_idea_settings;
#[cfg(test)]
mod window_tests_run_configurations;
#[cfg(test)]
mod window_tests_switcher_icons;

#[cfg(test)]
mod window_tests_close_cross;
#[cfg(test)]
mod window_tests_editor_popups;
#[cfg(test)]
mod window_tests_gutter_width;
#[cfg(test)]
mod window_tests_hunk_popup;
#[cfg(test)]
mod window_tests_jakarta;
#[cfg(test)]
mod window_tests_migrations;
#[cfg(test)]
mod window_tests_platform_menu;
#[cfg(test)]
mod window_tests_platform_open;
#[cfg(test)]
mod window_tests_theme_sync;
#[cfg(test)]
mod window_tests_themes;
#[cfg(test)]
mod window_tests_ui_scale;
#[cfg(test)]
mod window_tests_window_frame;

#[cfg(test)]
mod window_tests_branches_idea;
#[cfg(test)]
mod window_tests_branches_place;
#[cfg(test)]
mod window_tests_completion_doc;
#[cfg(test)]
mod window_tests_editor_idea;
#[cfg(test)]
mod window_tests_editor_idea_details;
#[cfg(test)]
mod window_tests_git_window;
#[cfg(test)]
mod window_tests_live_fixes;
#[cfg(test)]
mod window_tests_markdown_readme;
#[cfg(test)]
mod window_tests_project_toolbar;
#[cfg(test)]
mod window_tests_source_languages;
#[cfg(test)]
mod window_tests_tab_move;
#[cfg(test)]
mod window_tests_theme_choice;

#[cfg(test)]
mod window_tests_markdown_blocks;
#[cfg(test)]
mod window_tests_panel_icons;

#[cfg(test)]
mod window_tests_ai_tools;
#[cfg(test)]
mod window_tests_build_tool_missing;
#[cfg(test)]
mod window_tests_claude_ide;
#[cfg(test)]
mod window_tests_git_console;
#[cfg(test)]
mod window_tests_git_log_labels;
#[cfg(test)]
mod window_tests_git_partial;
#[cfg(test)]
mod window_tests_gradle_missing;
#[cfg(test)]
mod window_tests_kubernetes_journal;
#[cfg(test)]
mod window_tests_mcp_edits;
#[cfg(test)]
mod window_tests_postfix;

#[cfg(test)]
mod window_tests_database_scripts;
#[cfg(test)]
mod window_tests_journal;
#[cfg(test)]
mod window_tests_menu_scroll;

#[cfg(test)]
mod window_tests_ai_tools_text;

#[cfg(test)]
mod window_tests_mock_transcript;

#[cfg(test)]
mod window_tests_mcp_clients;

#[cfg(test)]
mod window_tests_ai_sessions;

// The stand-in harness of these tests is a PowerShell script, so they run on Windows only.
#[cfg(all(test, windows))]
mod window_tests_ai_terminal_select;

#[cfg(test)]
mod window_tests_run_starting;

#[cfg(test)]
mod window_tests_fix_with_ai;

#[cfg(test)]
mod window_tests_idea_editor;

#[cfg(test)]
mod window_tests_find_idea;

#[cfg(test)]
mod window_tests_column;

#[cfg(test)]
mod window_tests_smart_enter;

#[cfg(test)]
mod window_tests_preview_crumbs;

#[cfg(test)]
mod window_tests_icon_sets_buttons;

#[cfg(test)]
mod window_tests_diff_idea;
#[cfg(test)]
mod window_tests_settings_scroll;

#[cfg(test)]
mod window_tests_completion_idea;

#[cfg(test)]
mod window_tests_project_compact;

#[cfg(test)]
mod window_tests_daily;

#[cfg(test)]
mod window_tests_problems;

#[cfg(test)]
mod window_tests_annotate_idea;

#[cfg(test)]
mod window_tests_git_log_icons;

mod bookmarks;
#[cfg(test)]
mod window_tests_build_profiles;
#[cfg(test)]
mod window_tests_gradle_wrapper;
#[cfg(test)]
mod window_tests_scratch_languages;

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
    AI_TOOL, BEANS_TOOL, BOOKMARKS_TOOL, BUILD_TOOL, COMMIT_TOOL, COVERAGE_TOOL, DATABASE_TOOL,
    DEBUG_TOOL, DOCUMENTATION_TOOL, DockSide, ENDPOINTS_TOOL, INSPECTION_TOOL, LOG_TOOL, Launch,
    OpenRequests, PERFORMANCE_TOOL, PROBLEMS_TOOL, PROFILER_TOOL, PROJECT_TOOL, RUN_TOOL,
    SELECTION_HISTORY_TOOL, SERVICES_TOOL, STACK_TRACE_TOOL, STRUCTURE_TOOL, TASKS_TOOL,
    TERMINAL_TOOL, TODO_TOOL, USAGES_TOOL, Workspace,
};

use crate::index::{IndexEvent, ProjectIndex};
use crate::navigation::Navigator;

pub use agent_tools::AgentTools;
pub use agents::{AgentLanguages, AgentOptions};
pub use coverage::{ActiveCoverage, CoverageColumn};
pub use database_data::DatabaseDataView;
pub use database_panel::{
    ActiveDatabase, ActiveMigrations, ActivePasswords, DatabasePanel, migration_support,
    set_database_support, set_migration_support,
};
pub use debug::{ActiveDebug, BreakpointsChanged, DebugPanel};
pub use index::SearchSetup;
pub use inspect::{ActiveInspect, Finding, InspectHub};
pub use invalidate::CacheLocations;
pub use jshell_panel::{ActiveJShell, JShellPanel, set_jshell_support};
pub use problems::{ActiveProblems, Problem, ProblemsHub, ProblemsState};
pub use profiler::{ActiveProfiler, ProfileColumn, ProfilerTab};
pub use run::ProjectSetup;
pub use script::parse_script;
pub use settings_templates::TemplatesSetup;
pub use start::{
    Generators, ProjectTemplate, run as run_start, set_generators as set_project_generators,
};
pub use tree_ops::{Creates, FileTemplate};
pub use trust::{LanguageStart, ProjectTrust, ServerTrust};
pub use usages::{ActiveUsages, UsagesHub};
pub use window_popups::{ScratchFileType, ScratchFileTypes};

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
#[allow(clippy::too_many_arguments)]
pub fn run(
    launch: Launch,
    languages: Languages,
    scratch: ScratchFileTypes,
    search: SearchSetup,
    project: ProjectSetup,
    server: Option<LanguageServer>,
    mock_agents: bool,
    script: Option<String>,
) {
    run_with_setup(
        launch,
        OpenRequests::default(),
        languages,
        scratch,
        search,
        project,
        server,
        None,
        ProjectTrust::trusted(),
        CacheLocations::default(),
        TemplatesSetup::default(),
        |_| None,
        mock_agents,
        script,
    )
}

/// [`run`] with the hint of a tool that was not found: the balloon offers its page and
/// a second search that starts the server without restarting Arro. `requests` brings the
/// paths that the system opens and the requests of a second start.
#[allow(clippy::too_many_arguments)]
pub fn run_with_setup(
    launch: Launch,
    requests: OpenRequests,
    languages: Languages,
    scratch: ScratchFileTypes,
    search: SearchSetup,
    project: ProjectSetup,
    server: Option<LanguageServer>,
    setup: Option<LanguageSetup>,
    trust: ProjectTrust,
    cache: CacheLocations,
    templates: TemplatesSetup,
    code_style_language: fn(&std::path::Path) -> Option<&'static str>,
    mock_agents: bool,
    script: Option<String>,
) {
    let agents = AgentOptions {
        state_dir: launch.state_dir.clone(),
        mock: mock_agents,
    };
    // The harness of the IDE protocol looks in the configuration folder of the user, and
    // a scripted run has no user in front of it and leaves that folder alone.
    let claude = claude_ide::config_for(
        script.is_some(),
        std::env::var_os(claude_bridge::CONFIG_VARIABLE).map(std::path::PathBuf::from),
        ::base::program::home_dir(),
    );
    workspace::run(launch, requests, move |workspace, window, cx| {
        cx.set_global(trust);
        cx.set_global(claude);
        cx.set_global(languages);
        cx.set_global(scratch);
        cx.set_global(editor_view::CodeStyleLanguages(code_style_language));
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
    if let Some(settings) = cx.try_global::<workspace::Settings>() {
        support.set_dependency_updates(settings.check_dependency_updates);
    }
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
    let inspections = workspace::load_inspections(workspace.root());
    support.set_inspection_settings(inspections.clone());
    cx.set_global(editor_view::InspectionProfile(inspections));
    spring_data::follow(support.clone(), cx);
    if let Some(panel) = cx.try_global::<endpoints::ActiveEndpoints>() {
        let panel = panel.0.clone();
        panel.update(cx, |panel, cx| panel.language_ready(cx));
    }
    if let Some(panel) = cx.try_global::<beans::ActiveBeans>() {
        let panel = panel.0.clone();
        let support = support.clone();
        panel.update(cx, |panel, cx| panel.language_ready(support, cx));
    }
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
    workspace.register_command(
        "language.reimport_project",
        "Reload Project",
        |_workspace, _window, cx| {
            if let Some(semantic) = cx.try_global::<Semantic>()
                && let Some(server) = semantic.0.server()
            {
                server.reimport();
            }
            if let Some(panel) = cx.try_global::<script::ActiveRunPanel>() {
                let panel = panel.0.clone();
                panel.update(cx, |panel, cx| panel.reload_project(cx));
            }
        },
    );
    connect_project_problems(workspace, support, cx);
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
                            workspace.notify_warning(
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
    workspace: &mut Workspace,
    mut status: UnboundedReceiver<String>,
    cx: &mut Context<Workspace>,
) {
    memory_stop::register(workspace);
    memory_stop::watch_warning(cx);
    cx.spawn(async move |workspace, cx| {
        // A server that gave up says so once per run, not on every crash.
        let mut failure_shown = false;
        // Whether the line before was a stop for low memory: one stop shows its notice once.
        let mut stopped = false;
        while let Some(text) = status.next().await {
            let reason = if failure_shown {
                None
            } else {
                failed_reason(&text).map(str::to_owned)
            };
            let stop = memory_stop::stop_of(&text).map(|stop| (stop.name.to_owned(), stop.manual));
            let was_stopped = std::mem::replace(&mut stopped, stop.is_some());
            let shown = workspace.update(cx, |workspace, cx| {
                if let Some(reason) = reason.clone() {
                    workspace.notify_error_with_command(
                        "Language Server",
                        reason,
                        "Retry",
                        "language.restart_server",
                        cx,
                    );
                }
                if let Some((name, manual)) = &stop
                    && !was_stopped
                {
                    memory_stop::notify(name, *manual, workspace, cx);
                }
                let failed_import = import_failed(&text);
                let action = if failed_import {
                    Some("language.show_import_failure")
                } else if stop.is_some() {
                    Some(memory_stop::SHOW_COMMAND)
                } else {
                    None
                };
                workspace.set_language_status(Some(text.into()), cx);
                workspace.set_language_action(action, cx)
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

/// Whether the line says that the build tool could not import the project. A click on
/// such a line opens the notification of the failure again.
fn import_failed(text: &str) -> bool {
    text.contains(": import failed")
}

/// Takes the problems the server reports about the project itself. A build script
/// that could not be imported lands in Problems and in a notification with the ways
/// out: the Settings page of that build tool, another import and the server log. The
/// status line of the language carries the same reason.
fn connect_project_problems(
    workspace: &mut Workspace,
    support: Arc<dyn LanguageSupport>,
    cx: &mut Context<Workspace>,
) {
    // The notification may be closed or replaced by another one. A click on the status
    // line asks for the last failure, so the window keeps it.
    let last: Arc<std::sync::Mutex<Option<lang_api::ProjectProblem>>> = Arc::default();
    let shown = last.clone();
    workspace.register_command(
        "language.show_import_failure",
        "Show Import Failure",
        move |workspace, _window, cx| {
            let problem = shown
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .clone();
            if let Some(problem) = problem {
                notify_project_problem(workspace, &problem, cx);
            }
        },
    );
    cx.spawn(async move |workspace, cx| {
        let Some(provider) = support.project_problems() else {
            return;
        };
        let mut replies = ask(provider, ProjectProblemsQuery);
        let mut notified = String::new();
        // One repeat per session of the server. A failure that the language says another
        // import alone clears is imported again instead of shown, and the same failure
        // after that is a real error.
        let mut retried = false;
        while let Some(answer) = replies.next().await {
            let mut problems = answer.value;
            let retry = problems.first().is_some_and(|problem| problem.retry);
            if retry && !retried {
                // The request behind Reload Project, which the user does not have to press.
                if let Some(server) = support.server() {
                    server.reimport();
                    retried = true;
                }
            }
            if retry && retried {
                // It is on its way out: neither a notification nor a row in Problems.
                problems.retain(|problem| !problem.retry);
            }
            let first = problems.first().cloned();
            *last.lock().unwrap_or_else(|poison| poison.into_inner()) = first.clone();
            match &first {
                Some(problem) if problem.summary != notified => {
                    notified = problem.summary.clone();
                    let problem = problem.clone();
                    let updated = workspace.update(cx, |workspace, cx| {
                        notify_project_problem(workspace, &problem, cx)
                    });
                    if updated.is_err() {
                        break;
                    }
                }
                Some(_) => {}
                None => {
                    // The import ended well, so the notification of the failure this
                    // window showed closes itself. One that the user closed or another
                    // one that replaced it stays as it is.
                    let text = std::mem::take(&mut notified);
                    if !text.is_empty() {
                        let expected = format!("import failed: {text}");
                        let updated = workspace.update(cx, |workspace, cx| {
                            let ours =
                                workspace.last_notification().is_some_and(|(title, shown)| {
                                    title == "Language Server" && shown == expected
                                });
                            if ours {
                                workspace.close_notification(cx);
                            }
                        });
                        if updated.is_err() {
                            break;
                        }
                    }
                }
            }
            // The hub reads the workspace for the icons of its rows, so it is updated while
            // the workspace is free: a lease of the workspace here would panic in that read.
            let Ok(hub) = workspace.read_with(cx, |_, app| {
                app.try_global::<ActiveProblems>()
                    .map(|active| active.0.clone())
            }) else {
                break;
            };
            if let Some(hub) = hub {
                hub.update(cx, |hub, cx| hub.set_project_problems(&problems, cx));
            }
        }
    })
    .detach();
}

/// The notification of a failed import: the reason, the page of its build tool, another
/// try and the log of the server.
fn notify_project_problem(
    workspace: &mut Workspace,
    problem: &lang_api::ProjectProblem,
    cx: &mut Context<Workspace>,
) {
    let text = format!("import failed: {}", problem.summary);
    let settings = if problem.tool == Some(1) {
        "settings.build_tool_second"
    } else {
        "settings.build_tools"
    };
    let buttons = vec![
        ("settings", "Open Settings".into(), settings),
        (
            "reload",
            "Reload Project".into(),
            "language.reimport_project",
        ),
    ];
    match &problem.log {
        Some(log) => workspace.notify_error_with_action_and_commands(
            "Language Server",
            text,
            "Show Log",
            log.clone(),
            buttons,
            cx,
        ),
        None => workspace.notify_error_with_commands("Language Server", text, buttons, cx),
    }
}

fn install(
    workspace: &mut Workspace,
    search: SearchSetup,
    project: ProjectSetup,
    agents: AgentOptions,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let trust = cx
        .try_global::<trust::ProjectTrust>()
        .cloned()
        .unwrap_or_default();
    workspace.set_safe_mode(!trust.trusted, cx);
    history::open(&agents.state_dir, project.skipped_folders.clone(), cx);
    clipboard::open(&agents.state_dir, cx);
    editor_view::install_find_history(&agents.state_dir, cx);
    editor_view::install_completion_usage(&agents.state_dir, cx);
    finder::install_history(&agents.state_dir, cx);
    // The editor draws the icon of a file it shows outside its own text, such as the
    // header of Quick Definition, and knows no file types of its own.
    let icons = std::sync::Arc::new(workspace.icons().clone());
    cx.set_global(editor_view::FileIcon(std::sync::Arc::new(
        move |path: &std::path::Path| icons.icon_for(path),
    )));
    // The bookmarks of the project open before the first editor reads them.
    let project_name = workspace.root().file_name().map_or_else(
        || "Bookmarks".to_owned(),
        |name| name.to_string_lossy().into_owned(),
    );
    let saved = workspace.layout().bookmarks.clone();
    let (groups, default) = bookmarks::groups_from_layout(&saved, &project_name);
    editor_view::install_bookmarks(groups, default, &project_name, cx);
    // The open editors take a keymap file of the user as soon as it is applied.
    workspace.set_keymap_listener(editor_view::install_keymap);
    project::bind_keys(cx);
    folder_compare::bind_keys(cx);
    items::bind_keys(cx);
    palette::bind_keys(cx);
    finder::bind_keys(cx);
    plugins::bind_keys(cx);
    usages::bind_keys(cx);
    terminal_panel::bind_keys(cx);
    bookmarks::bind_keys(cx);

    let mut templates = tree_ops::basic_templates();
    templates.extend(project.templates.iter().cloned());
    cx.set_global(project::FileTemplates(templates));

    // The spelling check reads its dictionaries once, when the window opens.
    spelling::install(workspace, cx);

    cx.set_global(outline::SymbolsFor(search.symbols_for));
    let weak = cx.weak_entity();
    workspace.set_editor_factory(editor_tab::factory(
        weak.clone(),
        workspace.state_dir().to_path_buf(),
    ));
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
    // The migrations reach the project tree and the Database window when they are built,
    // and a test that puts them in place before the window keeps them here.
    if cx
        .try_global::<database_panel::ActiveMigrations>()
        .is_none()
    {
        cx.set_global(database_panel::ActiveMigrations(
            database_panel::migration_support(),
        ));
    }
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
    select_in::bind_actions(weak.clone(), window.window_handle(), cx);
    cx.set_global(project::ActiveProjectPanel(panel));
    let root = workspace.root().to_path_buf();
    let draft = commit::CommitDraft::create(&root, workspace.store(), cx);
    let commit = commit::CommitPanel::create(
        git.clone(),
        weak.clone(),
        root,
        workspace.state_dir().to_path_buf(),
        draft.clone(),
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
    cx.set_global(script::ActiveCommitPanel(commit.clone()));
    let embedded = commit::CommitPanel::create(
        git.clone(),
        weak.clone(),
        workspace.root().to_path_buf(),
        workspace.state_dir().to_path_buf(),
        draft,
        window,
        cx,
    );
    embedded.update(cx, |commit, _| commit.set_embedded(true));
    let log = git_log::GitLogPanel::create(git.clone(), weak.clone(), Some(embedded), cx);
    let focus = log.read(cx).focus();
    workspace.set_dock(
        DockSide::Bottom,
        LOG_TOOL,
        log.clone().into(),
        Some(focus),
        cx,
    );
    cx.set_global(git_log::ActiveGitLog(log.clone()));
    let log_tab = log.clone();
    workspace.set_tool_activator(Rc::new(move |id, cx| {
        if id == LOG_TOOL {
            log_tab.update(cx, |log, cx| log.show_log(cx))
        } else {
            false
        }
    }));
    let history = git_history::SelectionHistoryPanel::create(git.clone(), weak.clone(), cx);
    let focus = history.read(cx).focus();
    workspace.set_dock(
        DockSide::Bottom,
        SELECTION_HISTORY_TOOL,
        history.clone().into(),
        Some(focus),
        cx,
    );
    cx.set_global(git_history::ActiveSelectionHistory(history));
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

    // The terminal starts under the IDE protocol of the window, so a harness run from it
    // finds the diagnostics and the diff view of the project. A window no application
    // built, such as a test, keeps the lock file in its own state folder.
    let claude = cx
        .try_global::<claude_ide::ClaudeConfig>()
        .cloned()
        .unwrap_or_else(|| {
            claude_ide::ClaudeConfig::Folder(Some(workspace.state_dir().join("claude-home")))
        });
    claude_ide::start(workspace.root(), claude, cx);
    let terminal = terminal_panel::TerminalPanel::create(
        weak.clone(),
        workspace.root().to_path_buf(),
        project.environment.clone(),
        workspace.store(),
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
    let services = docker::DockerPanel::create(
        root,
        workspace.state_dir().to_path_buf(),
        workspace,
        weak.clone(),
        window.window_handle(),
        cx,
    );
    let focus = services.read(cx).focus();
    workspace.set_dock(
        DockSide::Bottom,
        SERVICES_TOOL,
        services.clone().into(),
        Some(focus),
        cx,
    );
    cx.set_global(docker::ActiveDocker(services.clone()));

    let state_dir = workspace.state_dir().to_path_buf();
    let http = http::HttpPanel::create(weak.clone(), state_dir, cx);
    let focus = http.read(cx).focus();
    workspace.set_dock(
        DockSide::Bottom,
        workspace::HTTP_TOOL,
        http.clone().into(),
        Some(focus),
        cx,
    );
    cx.set_global(http::ActiveHttp(http));
    http::install(cx);
    library_sources::install(cx);
    library_sources::register(workspace, cx);
    bundle_editor::install(weak.clone(), cx);

    let state_dir = workspace.state_dir().to_path_buf();
    let database = database_panel::DatabasePanel::create(weak.clone(), state_dir.clone(), cx);
    let focus = database.read(cx).focus();
    workspace.set_dock(
        DockSide::Bottom,
        DATABASE_TOOL,
        database.clone().into(),
        Some(focus),
        cx,
    );
    cx.set_global(database_panel::ActiveDatabase(database));
    database_panel::install(state_dir, cx);
    jshell_panel::install(workspace, weak.clone(), cx);

    let root = workspace.root().to_path_buf();
    let store = workspace.store();
    let runs = run::RunPanel::create(
        root,
        project.clone(),
        workspace.state_dir().to_path_buf(),
        store,
        weak.clone(),
        trust.trusted,
        cx,
    );
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
    run::bind_actions(workspace);
    let build_output = build_output::BuildOutputPanel::create(runs.clone(), weak.clone(), cx);
    let focus = build_output.read(cx).focus();
    workspace.set_dock(
        DockSide::Bottom,
        BUILD_TOOL,
        build_output.clone().into(),
        Some(focus),
        cx,
    );
    #[cfg(test)]
    cx.set_global(build_output::ActiveBuildOutput(build_output));
    services.update(cx, |services, cx| services.set_runs(runs.clone(), cx));
    let coverage_source = runs.clone();
    let profiler_source = runs.clone();

    let debug = debug::DebugPanel::create(
        workspace.root().to_path_buf(),
        workspace.store(),
        workspace.state_dir().to_path_buf(),
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
    let debug_tools = crate::mcp_debug::DebugBridge::new();
    debug.update(cx, |panel, _| panel.set_tool_debug(debug_tools.clone()));
    debug.update(cx, |panel, _| panel.bind_window(window.window_handle()));
    cx.set_global(debug::ActiveDebug(debug));

    let hub = Arc::new(debug::DebugHub::default());
    let languages = Arc::new(LanguageTools::new());
    let project_root = workspace.root().to_path_buf();
    languages.register_debug(project_root.clone(), hub.clone());
    languages.register_debug_control(project_root.clone(), debug_tools);
    let database_tools = crate::mcp_database::DatabaseBridge::new();
    if let Some(database) = cx.try_global::<database_panel::ActiveDatabase>() {
        let database = database.0.clone();
        database.update(cx, |panel, cx| {
            panel.set_tool_database(database_tools.clone(), cx)
        });
    }
    languages.register_database_control(project_root.clone(), database_tools);
    let run_tools = crate::mcp_runs::RunBridge::new();
    runs.update(cx, |panel, _| panel.set_tool_runs(run_tools.clone()));
    languages.register_project(project_root, run_tools);
    cx.set_global(debug::ActiveDebugHub(hub));
    let edits = mcp_edits::EditRequests::new();
    let tools = Arc::new(agent_tools::WindowTools::new(
        languages.clone(),
        workspace.root().to_path_buf(),
        search.excludes.clone(),
        project.environment.clone(),
        edits,
    ));
    cx.set_global(agent_tools::AgentTools(tools));
    cx.set_global(AgentLanguages(languages));
    let files = cx.new(|_| Arc::new(Vec::new()));
    workspace.set_project_files(files.clone());
    let close_state_dir = agents.state_dir.clone();
    let close_root = workspace.root().to_path_buf();
    let agent_panel = agents::AgentsPanel::create(
        workspace,
        weak.clone(),
        agents,
        files.clone(),
        project.environment.clone(),
        window.window_handle(),
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
    agents::install(cx);
    mcp_approval::bind_actions(workspace);
    mcp_edits::bind_actions(workspace);
    agent_clients::bind_actions(workspace);
    // The entry point of the project goes away with the window, as the server does.
    let entry_state = close_state_dir;
    let entry_root = close_root;
    workspace.on_close(move || mcp_discovery::EntryPoint::remove(&entry_state, &entry_root));

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

    let coverage = coverage::CoveragePanel::create(weak.clone(), coverage_source, cx);
    let focus = coverage.read(cx).focus();
    workspace.set_dock(
        DockSide::Bottom,
        COVERAGE_TOOL,
        coverage.clone().into(),
        Some(focus),
        cx,
    );
    cx.set_global(coverage::ActiveCoverage(coverage));

    let profiler = profiler::ProfilerPanel::create(weak.clone(), profiler_source, cx);
    let focus = profiler.read(cx).focus();
    workspace.set_dock(
        DockSide::Bottom,
        PROFILER_TOOL,
        profiler.clone().into(),
        Some(focus),
        cx,
    );
    cx.set_global(profiler::ActiveProfiler(profiler));

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

    let type_hierarchy = type_hierarchy::TypeHierarchyPanel::create(weak.clone(), cx);
    let focus = type_hierarchy.read(cx).focus();
    workspace.set_dock(
        DockSide::Bottom,
        workspace::TYPE_HIERARCHY_TOOL,
        type_hierarchy.clone().into(),
        Some(focus),
        cx,
    );
    cx.set_global(type_hierarchy::ActiveTypeHierarchy(type_hierarchy));

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

    let stack_traces = stack_traces::StackTracePanel::create(weak.clone(), cx);
    let focus = stack_traces.read(cx).focus();
    workspace.set_dock(
        DockSide::Bottom,
        STACK_TRACE_TOOL,
        stack_traces.clone().into(),
        Some(focus),
        cx,
    );
    cx.set_global(stack_traces::ActiveStackTrace(stack_traces));

    let keymap_file = workspace.keymap_path().to_path_buf();
    let templates_file = workspace.settings_dir().join(templates::TEMPLATES_FILE);
    let ignored = cx
        .try_global::<workspace::Settings>()
        .map(|settings| settings.ignored_files.clone())
        .unwrap_or_default();
    let (index, events) = ProjectIndex::start_with_ignored(
        root.clone(),
        vec![keymap_file, templates_file],
        search,
        ignored,
    );
    cx.set_global(crate::index::ActiveIndex(index.clone()));
    let lookup = index.clone();
    cx.set_global(editor_view::SymbolLookup(Some(Arc::new(
        move |name: &str| lookup.locate(name),
    ))));
    let runs = cx.global::<script::ActiveRunPanel>().0.clone();
    runs.update(cx, |runs, _| runs.set_files(files.clone()));
    if let Some(debug) = cx.try_global::<debug::ActiveDebug>() {
        let debug = debug.0.clone();
        debug.update(cx, |debug, _| debug.set_files(files.clone()));
    }
    commands::register(workspace, &index, &files);
    let endpoints = endpoints::EndpointsPanel::create(files.clone(), weak.clone(), cx);
    let focus = endpoints.read(cx).focus();
    workspace.set_dock(
        DockSide::Bottom,
        ENDPOINTS_TOOL,
        endpoints.clone().into(),
        Some(focus),
        cx,
    );
    cx.set_global(endpoints::ActiveEndpoints(endpoints.clone()));
    endpoints.update(cx, |panel, cx| panel.language_ready(cx));
    workspace.register_command(
        "window.show_endpoints",
        "Endpoints",
        |workspace, window, cx| workspace.show_tool(ENDPOINTS_TOOL, window, cx),
    );
    let beans_hub = beans::BeanHub::create(cx);
    cx.set_global(beans::ActiveBeanHub(beans_hub.clone()));
    let beans_panel = beans::BeansPanel::create(beans_hub, weak.clone(), cx);
    let focus = beans_panel.read(cx).focus();
    workspace.set_dock(
        DockSide::Right,
        BEANS_TOOL,
        beans_panel.clone().into(),
        Some(focus),
        cx,
    );
    cx.set_global(beans::ActiveBeans(beans_panel));
    workspace.register_command("window.show_beans", "Beans", |workspace, window, cx| {
        workspace.show_tool(BEANS_TOOL, window, cx)
    });
    workspace.register_command(
        "window.show_diagram",
        "Show Diagram",
        crate::diagram::show_current,
    );
    workspace.register_command(
        "window.show_sequence_diagram",
        "Generate Sequence Diagram",
        crate::sequence_diagram::show_current,
    );
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
    workspace.set_tab_listener(|workspace, path, cx| {
        places::remember_path(workspace, path, cx);
        cx.default_global::<Navigator>().activated(path);
        project::follow_active_file(path, cx);
        if let Some(problems) = cx.try_global::<problems::ActiveProblems>() {
            let hub = problems.0.clone();
            let path = path.to_path_buf();
            // A tab may change while its list is being updated, as a click on a row
            // opens a file: the hub rebuilds the list once that update ends.
            cx.defer(move |cx| {
                hub.update(cx, |hub, cx| hub.set_active_file(Some(path), cx));
            });
        }
        let panel = cx.global::<structure::ActiveStructure>().0.clone();
        panel.update(cx, |panel, cx| panel.show(path.to_path_buf(), cx));
        let documentation = cx.global::<documentation::ActiveDocumentation>().0.clone();
        documentation.update(cx, |panel, cx| panel.show_path(path.to_path_buf(), cx));
        if let Some(git) = cx.try_global::<git::ActiveGit>().map(|git| git.0.clone()) {
            git.update(cx, |git, cx| git.active_editor_changed(path, cx));
        }
    });
    navigation::bind_recent(workspace, cx);
    // What the watcher missed shows up when the window comes back to the front.
    let rescan = index.clone();
    cx.observe_window_activation(window, move |_, window, _| {
        if window.is_window_active() {
            rescan.rescan();
        }
    })
    .detach();
    watch_project(events, files, cx);
    let bookmarks = bookmarks::BookmarksPanel::create(weak.clone(), cx);
    let focus = bookmarks.read(cx).focus();
    workspace.set_dock(
        DockSide::Right,
        BOOKMARKS_TOOL,
        bookmarks.clone().into(),
        Some(focus),
        cx,
    );
    cx.set_global(bookmarks::ActiveBookmarks(bookmarks));
    settings_archive::bind_commands(workspace, cx);
    idea_import::bind_commands(workspace, cx);
    // The offer of the run configurations of another IDE and the answer Not Now reach
    // the Run window through these commands, as the buttons of its notification.
    workspace.register_command(
        "run.import_idea_configs",
        "Import Run Configurations from IntelliJ IDEA...",
        |_workspace, _window, cx| {
            if let Some(panel) = cx.try_global::<script::ActiveRunPanel>() {
                let panel = panel.0.clone();
                panel.update(cx, |panel, cx| panel.import_foreign_configs(cx));
            }
        },
    );
    workspace.register_command(
        "run.dismiss_idea_configs",
        "Not Now",
        |_workspace, _window, cx| {
            if let Some(panel) = cx.try_global::<script::ActiveRunPanel>() {
                let panel = panel.0.clone();
                panel.update(cx, |panel, cx| panel.dismiss_foreign_configs(cx));
            }
        },
    );
    workspace.register_command(
        workspace::TRUST_PROJECT_COMMAND,
        "Trust Project...",
        trust::ProjectTrustDialog::show,
    );
    workspace.register_command("file.forget_trust", "Forget Trust", |workspace, _, cx| {
        forget_trust(workspace, cx);
    });
    if !trust.trusted {
        // The window asks the language server of an untrusted project only to say that
        // nothing starts; a server that refuses the untrusted project sends `None`.
        if let Some(start) = &trust.start {
            let (sender, _receiver) = futures_channel::mpsc::unbounded();
            start.start(trust::ServerTrust::Untrusted, sender);
        }
        trust::ProjectTrustDialog::show(workspace, window, cx);
    }
    release_on_close(window, cx);
}

/// File > Forget Trust: the project asks again the next time it opens, and nothing of it
/// runs in this window from now on.
fn forget_trust(workspace: &mut Workspace, cx: &mut Context<Workspace>) {
    let list = store::TrustList::new(workspace.state_dir());
    if let Err(error) = list.untrust(workspace.root()) {
        let text = format!("{}: {error}", workspace.root().display());
        workspace.notify_error("Trust is not forgotten", text, cx);
    }
    workspace.set_safe_mode(true, cx);
    workspace.set_language_status(None, cx);
    if let Some(semantic) = cx.try_global::<Semantic>() {
        semantic.0.stop_server();
    }
    if let Some(panel) = cx.try_global::<script::ActiveRunPanel>() {
        let panel = panel.0.clone();
        panel.update(cx, |panel, cx| panel.forget_trust(cx));
    }
    workspace.notify_user(
        "Trust",
        "The project is no longer trusted: the build stays off until you trust it again.",
        cx,
    );
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
        beans::ActiveBeanHub,
        beans::ActiveBeans,
        bookmarks::ActiveBookmarks,
        call_hierarchy::ActiveCallHierarchy,
        debug::ActiveDebug,
        debug::ActiveDebugHub,
        docker::ActiveDocker,
        documentation::ActiveDocumentation,
        editor_view::SymbolLookup,
        endpoints::ActiveEndpoints,
        finder::ActiveFinder,
        git::ActiveGit,
        git_history::ActiveSelectionHistory,
        git_log::ActiveGitLog,
        http::ActiveHttp,
        index::ActiveIndex,
        inspect::ActiveInspect,
        method_hierarchy::ActiveMethodHierarchy,
        performance::ActivePerformance,
        plugins::ActivePlugins,
        problems::ActiveProblems,
        project::ActiveProjectPanel,
        run::DebugOutput,
        script::ActiveBuildPanel,
        script::ActiveCommitPanel,
        script::ActiveRunPanel,
        spring_data::Followed,
        stack_traces::ActiveStackTrace,
        structure::ActiveStructure,
        terminal_panel::ActiveTerminal,
        todo::ActiveTodo,
        type_hierarchy::ActiveTypeHierarchy,
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
    let properties = changes.iter().any(|change| match change {
        FsChange::Created(path) | FsChange::Changed(path) | FsChange::Removed(path) => path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("properties")),
        FsChange::Rescan => false,
    });
    if properties {
        crate::bundle_editor::files_changed(cx);
    }
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
    if let Some(panel) = cx.try_global::<endpoints::ActiveEndpoints>() {
        let panel = panel.0.clone();
        panel.update(cx, |panel, cx| panel.disk_changed(changes, cx));
    }
    if let Some(semantic) = cx.try_global::<Semantic>() {
        let support = semantic.0.clone();
        let touched = changes.iter().any(|change| match change {
            FsChange::Created(path) | FsChange::Changed(path) | FsChange::Removed(path) => {
                support.matches(path)
            }
            FsChange::Rescan => true,
        });
        if touched && let Some(beans) = support.beans() {
            beans.sources_changed();
        }
    }
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

#[cfg(test)]
mod window_tests_build_output;
#[cfg(test)]
mod window_tests_mcp_idea_tails;
#[cfg(test)]
mod window_tests_mcp_terminal;
#[cfg(test)]
mod window_tests_run_console;

#[cfg(test)]
mod window_tests_beans_menu;
#[cfg(test)]
mod window_tests_generate_list;
#[cfg(test)]
mod window_tests_import_failure;

#[cfg(test)]
mod window_tests_bookmarks;

#[cfg(test)]
mod window_tests_history_todo;
#[cfg(test)]
mod window_tests_move_members;

#[cfg(test)]
mod window_tests_move_targets;

#[cfg(test)]
mod window_tests_type_hierarchy_scope;

#[cfg(test)]
mod window_tests_macos_keys;

#[cfg(test)]
mod window_tests_log_menu_git;

#[cfg(test)]
mod window_tests_dialog_frame;
#[cfg(test)]
mod window_tests_inspect_scope;
#[cfg(test)]
mod window_tests_reformat_scope;

#[cfg(test)]
mod window_tests_push_cancel;

#[cfg(test)]
mod window_tests_push_tree;

#[cfg(test)]
mod window_tests_commit_tree;

#[cfg(test)]
mod window_tests_finder_polish;
#[cfg(test)]
mod window_tests_icons;
#[cfg(test)]
mod window_tests_settings_polish;

mod database_sources;

#[cfg(test)]
mod window_tests_database_sources;
#[cfg(test)]
mod window_tests_dialog_buttons;
#[cfg(test)]
mod window_tests_mcp_runs_unseen;
#[cfg(test)]
mod window_tests_new_file;
#[cfg(test)]
mod window_tests_popup_frame;
#[cfg(test)]
mod window_tests_run_configs_tree;

mod tool_menu;
#[cfg(test)]
mod window_tests_tool_headers;

#[cfg(test)]
mod window_tests_database_sources_rows;
#[cfg(test)]
mod window_tests_field_mouse;
#[cfg(test)]
mod window_tests_find_action;
#[cfg(test)]
mod window_tests_form_columns;
#[cfg(test)]
mod window_tests_panel_icons_rows;

// The stand-in program of these tests is a PowerShell script, so they run on Windows only.
#[cfg(test)]
mod window_tests_finder_scope;
#[cfg(all(test, windows))]
mod window_tests_terminal_wheel;

#[cfg(test)]
mod window_tests_project_structure_select;

#[cfg(test)]
mod window_tests_database_blank;

#[cfg(test)]
mod window_tests_breakpoints_dialog;
#[cfg(test)]
mod window_tests_caret_reveal;
#[cfg(test)]
mod window_tests_change_tree_gap;
#[cfg(test)]
mod window_tests_dialog_windows;
#[cfg(test)]
mod window_tests_file_structure_signatures;
#[cfg(test)]
mod window_tests_find_window;
#[cfg(test)]
mod window_tests_new_project_form;

#[cfg(test)]
mod window_tests_keymap_tree;
#[cfg(test)]
mod window_tests_mcp_reading;
#[cfg(test)]
mod window_tests_rename_in_place;
#[cfg(test)]
mod window_tests_settings_checks;

#[cfg(test)]
mod window_tests_debug_idle;
#[cfg(test)]
mod window_tests_find_action_tools;
#[cfg(test)]
mod window_tests_generate_status;
#[cfg(test)]
mod window_tests_run_idle;

#[cfg(test)]
mod window_tests_database_copy;
#[cfg(test)]
mod window_tests_database_safety;
#[cfg(test)]
mod window_tests_editor_importing;
#[cfg(test)]
mod window_tests_find_action_data;
#[cfg(test)]
mod window_tests_generate_server;
#[cfg(test)]
mod window_tests_import_retry;

#[cfg(test)]
mod window_tests_completion_members;

#[cfg(test)]
mod window_tests_import_retry_attempt;

#[cfg(test)]
mod window_tests_intentions_after_import;

#[cfg(test)]
mod window_tests_rename_in_place_errors;

#[cfg(test)]
mod window_tests_no_usages;

#[cfg(test)]
mod window_tests_test_marks_menu;

#[cfg(test)]
mod window_tests_debug_console_ansi;
#[cfg(test)]
mod window_tests_debug_console_follow;
#[cfg(test)]
mod window_tests_test_enter;

#[cfg(test)]
mod window_tests_structure_window;

#[cfg(test)]
mod window_tests_debug_stop_view;

#[cfg(test)]
mod window_tests_find_in_files_preview;

#[cfg(test)]
mod window_tests_surround_list;

#[cfg(test)]
mod window_tests_usages_rows;

#[cfg(test)]
mod window_tests_debug_live_shape;

#[cfg(test)]
mod window_tests_debug_build_front;

#[cfg(test)]
mod window_tests_debug_frames;

#[cfg(test)]
mod window_tests_find_action_new;

#[cfg(test)]
mod window_tests_usages_searching;

#[cfg(test)]
mod window_tests_recent_places;

#[cfg(test)]
mod window_tests_goto;

#[cfg(test)]
mod window_tests_switcher_tabs;

#[cfg(test)]
mod window_tests_commit_untracked;

#[cfg(test)]
mod window_tests_quick_doc_header;

#[cfg(test)]
mod window_tests_settings_appearance;

#[cfg(test)]
mod window_tests_run_config_form;

#[cfg(test)]
mod window_tests_settings_search;

#[cfg(test)]
mod window_tests_find_action_ranking;

#[cfg(test)]
mod window_tests_tool_polish;

#[cfg(test)]
mod window_tests_tool_field_keys;

#[cfg(test)]
mod window_tests_dialog_escape;

#[cfg(test)]
mod window_tests_services_offline;

#[cfg(test)]
mod window_tests_items_hint;

#[cfg(test)]
mod window_tests_commit_amend;

#[cfg(test)]
mod window_tests_notification_levels;

#[cfg(test)]
mod window_tests_switcher_columns;

#[cfg(test)]
mod window_tests_terminal_focus;

#[cfg(test)]
mod window_tests_find_bar;

#[cfg(test)]
mod window_tests_call_hierarchy_scope;

#[cfg(test)]
mod window_tests_palette_ranking;

#[cfg(test)]
mod window_tests_amend_message;

#[cfg(test)]
mod window_tests_amend_push;

#[cfg(test)]
mod window_tests_alt_enter_kinds;

#[cfg(test)]
mod window_tests_definition;

#[cfg(test)]
mod window_tests_caret_restore;

#[cfg(test)]
mod window_tests_signature_gap_reformat;

#[cfg(test)]
mod window_tests_git_log_details;

#[cfg(test)]
mod window_tests_introduce_variable;

#[cfg(test)]
mod window_tests_services_contexts;

#[cfg(test)]
mod window_tests_definition_layout;

#[cfg(test)]
mod window_tests_source_inline;

#[cfg(test)]
mod window_tests_signature_visibility;

#[cfg(test)]
mod window_tests_introduce_wait;

#[cfg(test)]
mod window_tests_real_expressions;

#[cfg(test)]
mod window_tests_select_in;

#[cfg(test)]
mod window_tests_find_in_files_highlight;

#[cfg(test)]
mod window_tests_extract_name;

#[cfg(test)]
mod window_tests_refactor_popups;

#[cfg(test)]
mod window_tests_goto_menu;

#[cfg(test)]
mod window_tests_type_info;

#[cfg(test)]
mod window_tests_insert_template;

#[cfg(test)]
mod window_tests_recent_changes;

#[cfg(test)]
mod window_tests_find_in_files_preview_match;

#[cfg(test)]
mod window_tests_recent_changes_time;

#[cfg(test)]
mod window_tests_note_place;

#[cfg(test)]
mod window_tests_memory_stop;

#[cfg(test)]
mod window_tests_settings_memory;

#[cfg(test)]
mod window_tests_extract_wide;

#[cfg(test)]
mod window_tests_real_extract_name;
#[cfg(test)]
mod window_tests_replace_files;
#[cfg(test)]
mod window_tests_settings_file;
#[cfg(test)]
mod window_tests_trust;

#[cfg(test)]
mod window_tests_git_safety;

#[cfg(test)]
mod window_tests_cut_paste;

#[cfg(test)]
mod window_tests_run_config_look;
#[cfg(test)]
mod window_tests_run_list;
