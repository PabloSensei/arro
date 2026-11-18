//! The main window: toolbar merged with the title bar, tool window stripes, docks,
//! editor tabs and splits, the status bar and the saved layout.

mod appearance;
mod chrome;
mod editor;
mod external;
mod git_settings;
mod inspections;
mod invalidate;
mod keys;
mod launch;
mod layout;
mod native_menu;
mod pane;
mod run;
mod settings;
mod window;

mod about;

pub use about::AboutView;
pub use appearance::{
    DEFAULT_PRESENTATION_FONT_SIZE, EditorAppearance, MAX_PRESENTATION_FONT_SIZE,
    MIN_PRESENTATION_FONT_SIZE, SystemAppearance, TOGGLE_BREADCRUMBS, TOGGLE_DISTRACTION_FREE,
    TOGGLE_FULL_SCREEN, TOGGLE_GUTTER_ICONS, TOGGLE_INDENT_GUIDES, TOGGLE_LINE_NUMBERS,
    TOGGLE_NAVIGATION_BAR, TOGGLE_PRESENTATION_MODE, TOGGLE_STATUS_BAR, TOGGLE_STICKY_LINES,
    TOGGLE_TOOL_WINDOW_BARS, TOGGLE_TOOLBAR, TOGGLE_ZEN_MODE, WindowMode, apply_saved_theme,
    checked_label, follow_system_theme, theme_of,
};
pub use editor::{
    BundleLayout, BundleReader, BundleSetter, BundleTab, EditorFactory, EditorStatus, EditorTab,
    FileIcons, HiddenGuard, IndentChoice, IndentSetter, PreviewLayout, PreviewReader,
    PreviewSetter, PreviewTab,
};
pub use external::{OpenRequests, OpenTarget, Request, open_target};
pub use git_settings::{
    GitProjectSettings, git_settings_file, load_git_settings, save_git_settings,
};
pub use inspections::{inspections_file, load_inspections, save_inspections};
pub use invalidate::{apply_pending_invalidation, pending_invalidation, schedule_invalidation};
pub use keys::KeyRow;
pub use launch::{
    AppInfo, Launch, StartContext, choose_platform_keymap, run, spawn_path, spawn_path_taking_over,
    spawn_project, start_context,
};
pub use layout::{DockState, Layout, PaneState, WindowPlacement};
pub use native_menu::ScreenMenus;
pub use run::{DebugControls, DebugRunState, RunControls, RunState};
pub use settings::{
    COMPLETION_DELAY_STEP_MS, CompletionCase, DEFAULT_DATABASE_PAGE_ROWS,
    DEFAULT_HTTP_TIMEOUT_SECONDS, DEFAULT_RIGHT_MARGIN, DefaultLayout, GitUpdateMethod,
    InlayParameterNames, LINE_SPACING_STEP, MAX_COMPLETION_DELAY_MS, MAX_DATABASE_PAGE_ROWS,
    MAX_FONT_SIZE, MAX_HTTP_TIMEOUT_SECONDS, MAX_INDENT_SIZE, MAX_LINE_SPACING, MAX_RIGHT_MARGIN,
    MAX_UI_SCALE, MIN_FONT_SIZE, MIN_LINE_SPACING, MIN_RIGHT_MARGIN, MIN_UI_SCALE,
    McpExecutePermission, RIGHT_MARGIN_STEP, ReformatScope, SETTINGS_FILE, Settings,
    TrimTrailingWhitespace, UI_SCALE_STEP,
};
pub use window::{
    AI_TOOL, BEANS_TOOL, BOOKMARKS_POPUP, CALL_HIERARCHY_TOOL, CHECK_LANGUAGE_SETUP, COMMIT_TOOL,
    COVERAGE_TOOL, CloseTab, CommandFn, DATABASE_TOOL, DEBUG_TOOL, DOCUMENTATION_TOOL, Debug,
    Dismiss, DockSide, ENDPOINTS_TOOL, Evaluate, HTTP_TOOL, INSPECTION_TOOL, JSHELL_TOOL, LOG_TOOL,
    METHOD_HIERARCHY_TOOL, MIN_DOCK_SIZE, MIN_EDITOR_SIZE, MenuRow, MenuShortcut, NextProblem,
    NextTab, PERFORMANCE_TOOL, PROBLEMS_TOOL, PROFILER_TOOL, PROJECT_TOOL, PreviousProblem,
    ProjectOpener, ProjectStructure, PushChanges, RECENT_PLACES_POPUP, RUN_TOOL, RerunInspection,
    Resume, RevertChange, Run, SELECTION_HISTORY_TOOL, SERVICES_TOOL, STACK_TRACE_TOOL,
    STRUCTURE_TOOL, SelectInProject, ShowBranches, ShowCommit, ShowSearch, ShowSettings,
    SplitRight, StepInto, StepOut, StepOver, Stop, TASKS_TOOL, TERMINAL_TOOL, TODO_TOOL,
    TabListener, ToggleAi, ToggleBreakpoint, ToggleCommit, ToggleDatabase, ToggleDebug,
    ToggleEndpoints, ToggleFullScreen, ToggleHttp, ToggleLog, ToggleProject, ToggleRun,
    ToggleServices, ToggleStructure, ToggleTerminal, ToggleTheme, ToggleUsages, USAGES_TOOL,
    UpdateProject, VcsQuickList, ViewBreakpoints, WINDOW_KEYS, WindowKey, Workspace, bind_keys,
    bind_keys_for, build_date, build_version, window_key, window_step,
};

#[cfg(any(test, feature = "test-support"))]
pub use window::{note_view_render, view_render_count, window_render_count, window_zoom_requests};
