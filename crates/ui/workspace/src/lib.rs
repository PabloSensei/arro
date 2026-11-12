//! The main window: toolbar merged with the title bar, tool window stripes, docks,
//! editor tabs and splits, the status bar and the saved layout.

mod chrome;
mod editor;
mod invalidate;
mod launch;
mod layout;
mod pane;
mod run;
mod settings;
mod window;

mod about;

pub use about::AboutView;
pub use editor::{EditorFactory, EditorStatus, EditorTab, FileIcons, IndentChoice, IndentSetter};
pub use invalidate::{apply_pending_invalidation, pending_invalidation, schedule_invalidation};
pub use launch::{AppInfo, Launch, StartContext, run, spawn_project, start_context};
pub use layout::{DockState, Layout, PaneState, WindowPlacement};
pub use run::{DebugControls, DebugRunState, RunControls, RunState};
pub use settings::{
    COMPLETION_DELAY_STEP_MS, CompletionCase, DefaultLayout, GitUpdateMethod, InlayParameterNames,
    LINE_SPACING_STEP, MAX_COMPLETION_DELAY_MS, MAX_FONT_SIZE, MAX_INDENT_SIZE, MAX_LINE_SPACING,
    MAX_UI_SCALE, MIN_FONT_SIZE, MIN_LINE_SPACING, MIN_UI_SCALE, SETTINGS_FILE, Settings,
    TrimTrailingWhitespace, UI_SCALE_STEP,
};
pub use window::{
    AI_TOOL, CALL_HIERARCHY_TOOL, CHECK_LANGUAGE_SETUP, COMMIT_TOOL, CloseTab, CommandFn,
    DEBUG_TOOL, DOCUMENTATION_TOOL, Debug, Dismiss, DockSide, Evaluate, INSPECTION_TOOL, LOG_TOOL,
    METHOD_HIERARCHY_TOOL, MenuShortcut, NextProblem, NextTab, PERFORMANCE_TOOL, PROBLEMS_TOOL,
    PROJECT_TOOL, PreviousProblem, ProjectOpener, ProjectStructure, PushChanges, RUN_TOOL,
    RerunInspection, Resume, RevertChange, Run, SERVICES_TOOL, STRUCTURE_TOOL, SelectInProject,
    ShowBranches, ShowCommit, ShowSearch, ShowSettings, SplitRight, StepInto, StepOut, StepOver,
    Stop, TASKS_TOOL, TERMINAL_TOOL, TODO_TOOL, TabListener, ToggleAi, ToggleBreakpoint,
    ToggleCommit, ToggleDebug, ToggleLog, ToggleProject, ToggleRun, ToggleServices,
    ToggleStructure, ToggleTerminal, ToggleTheme, ToggleUsages, USAGES_TOOL, UpdateProject,
    VcsQuickList, ViewBreakpoints, WINDOW_KEYS, WindowKey, Workspace, bind_keys, build_date,
    build_version, window_step,
};
