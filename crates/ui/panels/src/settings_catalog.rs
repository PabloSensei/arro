//! The settings of the window in one list: the name, the page and the kind of each.
//! The Settings window and Search Everywhere build their rows from here, so a new
//! setting shows up in both places at once.

use std::sync::OnceLock;

use project_model::ToolHome;
use ui_kit::{ARRO, INTELLIJ, IconCatalog, Theme};
use workspace::{MAX_COMPLETION_DELAY_MS, MAX_INDENT_SIZE, Settings};

/// One page of the Settings window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsPage {
    Appearance,
    Editor,
    EditorCodeStyle,
    EditorCompletion,
    EditorInlayHints,
    EditorInspections,
    EditorSpelling,
    Project,
    Plugins,
    Keymap,
    BuildTools,
    BuildToolsFirst,
    BuildToolsSecond,
    VersionControl,
    Git,
    Tools,
    ActionsOnSave,
    Terminal,
    HttpClient,
    EditorLiveTemplates,
    EditorPostfix,
    EditorFileTemplates,
    Debugger,
    Kubernetes,
    Database,
    /// The group above the MCP server page, under Tools.
    ToolsAi,
    Mcp,
    /// The patterns and the case of the TODO tool window.
    EditorTodo,
    /// The ignored file and folder masks of the project tree and the file index.
    EditorFileTypes,
    /// The typeface, the size and the line height of the editor text.
    EditorFont,
    /// The group of the settings of the program itself.
    SystemSettings,
    /// The automatic check for a new version.
    Updates,
    /// The free memory that stops a language server, and the heap it takes.
    Memory,
}

/// The names the language gives its two build tools. They are no part of this crate:
/// the application fills them before the Settings window opens.
static BUILD_TOOL_NAMES: OnceLock<(String, String)> = OnceLock::new();

/// Names the build tools for their pages and for the search. The first call wins.
pub fn set_build_tool_names(first: impl Into<String>, second: impl Into<String>) {
    let _ = BUILD_TOOL_NAMES.set((first.into(), second.into()));
}

fn build_tool_name(index: usize) -> &'static str {
    match BUILD_TOOL_NAMES.get() {
        Some((first, second)) => {
            if index == 1 {
                second.as_str()
            } else {
                first.as_str()
            }
        }
        None if index == 1 => "Second build tool",
        None => "First build tool",
    }
}

impl SettingsPage {
    /// The pages in the order the window shows them.
    pub const ALL: [SettingsPage; 33] = [
        SettingsPage::Appearance,
        SettingsPage::Keymap,
        SettingsPage::Editor,
        SettingsPage::EditorCodeStyle,
        SettingsPage::EditorCompletion,
        SettingsPage::EditorInlayHints,
        SettingsPage::EditorInspections,
        SettingsPage::EditorSpelling,
        SettingsPage::EditorLiveTemplates,
        SettingsPage::EditorPostfix,
        SettingsPage::EditorFileTemplates,
        SettingsPage::EditorTodo,
        SettingsPage::EditorFileTypes,
        SettingsPage::EditorFont,
        SettingsPage::Debugger,
        SettingsPage::Project,
        SettingsPage::BuildTools,
        SettingsPage::BuildToolsFirst,
        SettingsPage::BuildToolsSecond,
        SettingsPage::VersionControl,
        SettingsPage::Git,
        SettingsPage::Tools,
        SettingsPage::ActionsOnSave,
        SettingsPage::Terminal,
        SettingsPage::Kubernetes,
        SettingsPage::Database,
        SettingsPage::ToolsAi,
        SettingsPage::Mcp,
        SettingsPage::HttpClient,
        SettingsPage::Plugins,
        SettingsPage::SystemSettings,
        SettingsPage::Updates,
        SettingsPage::Memory,
    ];

    pub fn title(self) -> &'static str {
        match self {
            SettingsPage::Appearance => "Appearance",
            SettingsPage::Editor => "Editor",
            SettingsPage::EditorCodeStyle => "Code Style",
            SettingsPage::EditorCompletion => "Code Completion",
            SettingsPage::EditorInlayHints => "Inlay Hints",
            SettingsPage::EditorInspections => "Inspections",
            SettingsPage::EditorSpelling => "Spelling",
            SettingsPage::Project => "Project",
            SettingsPage::Plugins => "Plugins",
            SettingsPage::Keymap => "Keymap",
            SettingsPage::BuildTools => "Build Tools",
            SettingsPage::BuildToolsFirst => build_tool_name(0),
            SettingsPage::BuildToolsSecond => build_tool_name(1),
            SettingsPage::VersionControl => "Version Control",
            SettingsPage::Git => "Git",
            SettingsPage::Tools => "Tools",
            SettingsPage::ActionsOnSave => "Actions on Save",
            SettingsPage::Terminal => "Terminal",
            SettingsPage::HttpClient => "HTTP Client",
            SettingsPage::EditorLiveTemplates => "Live Templates",
            SettingsPage::EditorPostfix => "Postfix Completion",
            SettingsPage::EditorFileTemplates => "File and Code Templates",
            SettingsPage::Debugger => "Debugger",
            SettingsPage::Kubernetes => "Kubernetes",
            SettingsPage::Database => "Database",
            SettingsPage::ToolsAi => "AI",
            SettingsPage::Mcp => "MCP Server",
            SettingsPage::EditorTodo => "TODO",
            SettingsPage::EditorFileTypes => "File Types",
            SettingsPage::EditorFont => "Font",
            SettingsPage::SystemSettings => "System Settings",
            SettingsPage::Updates => "Updates",
            SettingsPage::Memory => "Memory",
        }
    }

    /// A part of Editor sits one step right of the page it belongs to.
    pub fn depth(self) -> usize {
        match self {
            SettingsPage::EditorCodeStyle
            | SettingsPage::EditorCompletion
            | SettingsPage::EditorInlayHints
            | SettingsPage::EditorInspections
            | SettingsPage::EditorSpelling
            | SettingsPage::BuildToolsFirst
            | SettingsPage::BuildToolsSecond
            | SettingsPage::Git
            | SettingsPage::ActionsOnSave
            | SettingsPage::Terminal
            | SettingsPage::Kubernetes
            | SettingsPage::Database
            | SettingsPage::EditorLiveTemplates
            | SettingsPage::EditorFileTemplates
            | SettingsPage::EditorTodo => 1,
            SettingsPage::EditorPostfix => 2,
            SettingsPage::ToolsAi => 1,
            SettingsPage::Mcp => 2,
            SettingsPage::EditorFileTypes => 1,
            SettingsPage::EditorFont => 1,
            SettingsPage::Updates => 1,
            SettingsPage::Memory => 1,
            _ => 0,
        }
    }

    /// The page a click on this row opens: a group opens its first part.
    pub fn target(self) -> SettingsPage {
        match self {
            SettingsPage::BuildTools => SettingsPage::BuildToolsFirst,
            SettingsPage::VersionControl => SettingsPage::Git,
            SettingsPage::Tools => SettingsPage::Terminal,
            SettingsPage::ToolsAi => SettingsPage::Mcp,
            SettingsPage::SystemSettings => SettingsPage::Updates,
            page => page,
        }
    }

    /// The group row above this page in the tree, when it has one.
    pub fn parent(self) -> Option<SettingsPage> {
        match self {
            SettingsPage::EditorCodeStyle
            | SettingsPage::EditorCompletion
            | SettingsPage::EditorInlayHints
            | SettingsPage::EditorInspections
            | SettingsPage::EditorSpelling
            | SettingsPage::EditorLiveTemplates
            | SettingsPage::EditorFileTemplates
            | SettingsPage::EditorTodo => Some(SettingsPage::Editor),
            SettingsPage::EditorPostfix => Some(SettingsPage::EditorLiveTemplates),
            SettingsPage::EditorFileTypes => Some(SettingsPage::Editor),
            SettingsPage::EditorFont => Some(SettingsPage::Editor),
            SettingsPage::Updates => Some(SettingsPage::SystemSettings),
            SettingsPage::Memory => Some(SettingsPage::SystemSettings),
            SettingsPage::BuildToolsFirst | SettingsPage::BuildToolsSecond => {
                Some(SettingsPage::BuildTools)
            }
            SettingsPage::Git => Some(SettingsPage::VersionControl),
            SettingsPage::ActionsOnSave
            | SettingsPage::Terminal
            | SettingsPage::Kubernetes
            | SettingsPage::Database => Some(SettingsPage::Tools),
            SettingsPage::ToolsAi => Some(SettingsPage::Tools),
            SettingsPage::Mcp => Some(SettingsPage::ToolsAi),
            _ => None,
        }
    }
}

/// What the control of a setting does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingKind {
    /// A switch: on or off. Find Action flips it in place.
    Toggle,
    /// A number with a step up and a step down.
    Number,
    /// A line of text.
    Text,
    /// One of two or more named choices.
    Choice,
}

/// The value of a setting in [`Settings`], for the ones that live in the file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingField {
    FontSize,
    UiScale,
    EditorFontFamily,
    LineSpacing,
    SoftWrap,
    ShowWhitespace,
    ShowLineNumbers,
    HighlightCurrentLine,
    ShowIndentGuides,
    CaretBlink,
    TrimTrailingWhitespace,
    EnsureFinalNewline,
    ActionsOnSaveReformat,
    ActionsOnSaveScope,
    ActionsOnSaveFiles,
    ActionsOnSaveOptimizeImports,
    ActionsOnSaveCleanup,
    ZoomWithWheel,
    TabSize,
    IndentSize,
    UseTabs,
    CompletionCase,
    CompletionAutoPopup,
    CompletionDelay,
    Autosave,
    OpenReadmeOnFirstOpen,
    UpdatesCheckAutomatically,
    SemanticHighlighting,
    InlayParameterNames,
    InlayTypeHints,
    QuickDocOnHover,
    CodeVisionUsages,
    CodeVisionImplementations,
    BuildToolFirstHome,
    BuildToolFirstSettingsFile,
    BuildToolFirstRepository,
    BuildToolFirstOffline,
    BuildToolFirstVmOptions,
    BuildToolFirstUpdateSnapshots,
    DownloadSourcesAutomatically,
    CheckDependencyUpdates,
    /// The source of the distribution of the second build tool: the wrapper of the
    /// project, a local installation or the copy the IDE downloaded.
    BuildToolSecondHome,
    BuildToolSecondOffline,
    BuildToolSecondUserHome,
    BuildToolSecondRuntimeHome,
    /// The folder of a local installation of the second build tool.
    BuildToolSecondHomePath,
    /// Whether the IDE downloads the distribution the wrapper of the project names.
    BuildToolSecondWrapperDownload,
    GitPath,
    GitUpdateMethod,
    GitAutoUpdateOnReject,
    GitUpdateSubmodules,
    GitWarnCrlf,
    GitAnalyzeBeforeCommit,
    TerminalShell,
    TerminalStartDirectory,
    TerminalEnvironment,
    TerminalConfirmMultilinePaste,
    RenderDocComments,
    MarkdownImagesFromInternet,
    DiffSideBySide,
    GitGroupByDirectory,
    HttpTimeout,
    DebugShowValuesInline,
    DebugHideLibraryFrames,
    DebugTypeRenderers,
    PresentationFontSize,
    RightMargin,
    ToolbarVisible,
    ToolWindowBarsVisible,
    StatusBarVisible,
    NavigationBarVisible,
    ShowGutterIcons,
    ShowBreadcrumbs,
    ShowStickyLines,
    HeapSnapshotFolder,
    KubectlPath,
    KubeconfigPath,
    DatabasePageRows,
    ThemeSyncWithOs,
    ThemeLight,
    ThemeDark,
    IconSet,
    ProjectAlwaysSelectOpenedFile,
    /// Where the home of the first build tool comes from.
    BuildToolFirstHomeKind,
    /// Whether the terminals put the downloaded first build tool on their `PATH`.
    BuildToolFirstTerminalPath,
    /// Whether the window shares its MCP server with the agents outside it.
    McpEnabled,
    /// The program a client outside the window starts.
    McpBinary,
    /// Whether a tool of the MCP server may build, test and run without a question.
    McpExecutePermission,
    /// Whether the terminals put the downloaded second build tool on their `PATH`.
    BuildToolSecondTerminalPath,
    /// The case Reformat Code gives the keywords of SQL.
    SqlKeywordCase,
    /// Whether Run SQL Script stops at the first statement that fails.
    SqlScriptStopOnError,
    /// Whether Ask AI, Fix with AI and Explain with AI are offered.
    AiEnabled,
    /// Whether the project tree shows chains of folders with one subfolder each as one row.
    ProjectCompactMiddlePackages,
    /// Whether `execute_tool` is the only tool a client sees.
    McpRouterOnlyMode,
    /// The clients the router-only mode covers.
    McpRouterOnlyScope,
    /// Whether a terminal suggests the setup of a client that starts in it.
    McpTerminalSuggestions,
    /// The patterns the TODO window looks for.
    TodoTemplates,
    /// Whether the TODO window matches the patterns with their case.
    TodoCaseSensitive,
    /// The file and folder masks the project tree and the file index leave out.
    IgnoredFiles,
    /// Whether Escape in the terminal moves the focus to the editor.
    TerminalEscapeFocus,
    /// Whether the memory check stops a language server at all.
    MemoryLowCheck,
    /// The free memory in megabytes below which the check stops a language server.
    MemoryLowMegabytes,
    /// The share of the total memory below which the check stops a language server.
    MemoryLowPercent,
    /// The most heap of the language server in megabytes; empty lets it choose.
    LanguageMaxHeap,
}

/// The value of a build tool field inside [`workspace::Settings`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ToolProperty {
    Home,
    SettingsFile,
    Repository,
    Offline,
    VmOptions,
    UpdateSnapshots,
    UserHome,
    RuntimeHome,
}

/// The tool and the value of a build tool field; `None` for every other setting.
fn tool_property(field: SettingField) -> Option<(usize, ToolProperty)> {
    use SettingField as F;
    use ToolProperty as P;
    Some(match field {
        F::BuildToolFirstHome => (0, P::Home),
        F::BuildToolFirstSettingsFile => (0, P::SettingsFile),
        F::BuildToolFirstRepository => (0, P::Repository),
        F::BuildToolFirstOffline => (0, P::Offline),
        F::BuildToolFirstVmOptions => (0, P::VmOptions),
        F::BuildToolFirstUpdateSnapshots => (0, P::UpdateSnapshots),
        F::BuildToolSecondHomePath => (1, P::Home),
        F::BuildToolSecondOffline => (1, P::Offline),
        F::BuildToolSecondUserHome => (1, P::UserHome),
        F::BuildToolSecondRuntimeHome => (1, P::RuntimeHome),
        _ => return None,
    })
}

/// A setting as the window and the search list it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Setting {
    /// The token the rows carry and look the setting up by.
    pub id: &'static str,
    pub title: &'static str,
    pub page: SettingsPage,
    pub kind: SettingKind,
    pub field: SettingField,
    /// More words the search finds this setting by.
    pub keywords: &'static str,
}

pub const SETTINGS: &[Setting] = &[
    Setting {
        id: "theme_sync_with_os",
        title: "Sync with OS",
        page: SettingsPage::Appearance,
        kind: SettingKind::Toggle,
        field: SettingField::ThemeSyncWithOs,
        keywords: "system appearance auto schedule automatic",
    },
    Setting {
        id: "theme_light",
        title: "Theme for the light system appearance",
        page: SettingsPage::Appearance,
        kind: SettingKind::Choice,
        field: SettingField::ThemeLight,
        keywords: "sync with os light theme colors appearance",
    },
    Setting {
        id: "theme_dark",
        title: "Theme for the dark system appearance",
        page: SettingsPage::Appearance,
        kind: SettingKind::Choice,
        field: SettingField::ThemeDark,
        keywords: "sync with os dark theme colors appearance",
    },
    Setting {
        id: "soft_wrap",
        title: "Soft wrap",
        page: SettingsPage::Editor,
        kind: SettingKind::Toggle,
        field: SettingField::SoftWrap,
        keywords: "wrap long lines editor",
    },
    Setting {
        id: "show_whitespace",
        title: "Show whitespace",
        page: SettingsPage::Editor,
        kind: SettingKind::Toggle,
        field: SettingField::ShowWhitespace,
        keywords: "spaces tabs marks editor",
    },
    Setting {
        id: "autosave",
        title: "Save when the window or the tab loses the focus",
        page: SettingsPage::Editor,
        kind: SettingKind::Toggle,
        field: SettingField::Autosave,
        keywords: "autosave save focus editor",
    },
    Setting {
        id: "open_readme_on_first_open",
        title: "Open README when a project is opened for the first time",
        page: SettingsPage::Project,
        kind: SettingKind::Toggle,
        field: SettingField::OpenReadmeOnFirstOpen,
        keywords: "readme markdown first open project start",
    },
    Setting {
        id: "updates_check_automatically",
        title: "Check for updates automatically",
        page: SettingsPage::Updates,
        kind: SettingKind::Toggle,
        field: SettingField::UpdatesCheckAutomatically,
        keywords: "updates system settings network automatic",
    },
    Setting {
        id: "ui_scale",
        title: "Interface scale",
        page: SettingsPage::Appearance,
        kind: SettingKind::Number,
        field: SettingField::UiScale,
        keywords: "zoom scale interface size appearance",
    },
    Setting {
        id: "editor_font_family",
        title: "Font",
        page: SettingsPage::EditorFont,
        kind: SettingKind::Choice,
        field: SettingField::EditorFontFamily,
        keywords: "typeface family monospace text editor font",
    },
    Setting {
        id: "font_size",
        title: "Size",
        page: SettingsPage::EditorFont,
        kind: SettingKind::Number,
        field: SettingField::FontSize,
        keywords: "font size text editor",
    },
    Setting {
        id: "line_spacing",
        title: "Line height",
        page: SettingsPage::EditorFont,
        kind: SettingKind::Number,
        field: SettingField::LineSpacing,
        keywords: "line height spacing text editor font",
    },
    Setting {
        id: "show_line_numbers",
        title: "Line numbers",
        page: SettingsPage::Editor,
        kind: SettingKind::Toggle,
        field: SettingField::ShowLineNumbers,
        keywords: "gutter numbers editor general",
    },
    Setting {
        id: "highlight_current_line",
        title: "Highlight current line",
        page: SettingsPage::Editor,
        kind: SettingKind::Toggle,
        field: SettingField::HighlightCurrentLine,
        keywords: "caret line background editor general",
    },
    Setting {
        id: "show_indent_guides",
        title: "Show indent guides",
        page: SettingsPage::Editor,
        kind: SettingKind::Toggle,
        field: SettingField::ShowIndentGuides,
        keywords: "indentation guides lines editor general",
    },
    Setting {
        id: "caret_blink",
        title: "Caret blinking",
        page: SettingsPage::Editor,
        kind: SettingKind::Toggle,
        field: SettingField::CaretBlink,
        keywords: "cursor blink editor general",
    },
    Setting {
        id: "trim_trailing_whitespace",
        title: "Remove trailing whitespace",
        page: SettingsPage::ActionsOnSave,
        kind: SettingKind::Choice,
        field: SettingField::TrimTrailingWhitespace,
        keywords: "spaces tabs trailing save modified lines actions on save editor general",
    },
    Setting {
        id: "ensure_final_newline",
        title: "Ensure an empty line at the end of file",
        page: SettingsPage::ActionsOnSave,
        kind: SettingKind::Toggle,
        field: SettingField::EnsureFinalNewline,
        keywords: "newline final empty line save line feed actions on save",
    },
    Setting {
        id: "actions_on_save_reformat",
        title: "Reformat code",
        page: SettingsPage::ActionsOnSave,
        kind: SettingKind::Toggle,
        field: SettingField::ActionsOnSaveReformat,
        keywords: "format reformat code save actions",
    },
    Setting {
        id: "actions_on_save_reformat_scope",
        title: "Reformat code: scope",
        page: SettingsPage::ActionsOnSave,
        kind: SettingKind::Choice,
        field: SettingField::ActionsOnSaveScope,
        keywords: "format reformat whole file changed lines vcs save actions",
    },
    Setting {
        id: "actions_on_save_reformat_files",
        title: "Reformat code: files",
        page: SettingsPage::ActionsOnSave,
        kind: SettingKind::Text,
        field: SettingField::ActionsOnSaveFiles,
        keywords: "format mask files patterns save actions",
    },
    Setting {
        id: "actions_on_save_optimize_imports",
        title: "Optimize imports",
        page: SettingsPage::ActionsOnSave,
        kind: SettingKind::Toggle,
        field: SettingField::ActionsOnSaveOptimizeImports,
        keywords: "imports organize save actions",
    },
    Setting {
        id: "actions_on_save_cleanup",
        title: "Run code cleanup",
        page: SettingsPage::ActionsOnSave,
        kind: SettingKind::Toggle,
        field: SettingField::ActionsOnSaveCleanup,
        keywords: "cleanup safe fixes inspections save actions",
    },
    Setting {
        id: "zoom_with_wheel",
        title: "Zoom with Ctrl+Mouse Wheel",
        page: SettingsPage::Editor,
        kind: SettingKind::Toggle,
        field: SettingField::ZoomWithWheel,
        keywords: "scale zoom wheel ctrl editor general",
    },
    Setting {
        id: "tab_size",
        title: "Tab size",
        page: SettingsPage::EditorCodeStyle,
        kind: SettingKind::Number,
        field: SettingField::TabSize,
        keywords: "tab width spaces code style",
    },
    Setting {
        id: "indent_size",
        title: "Indent size",
        page: SettingsPage::EditorCodeStyle,
        kind: SettingKind::Number,
        field: SettingField::IndentSize,
        keywords: "indent step spaces code style",
    },
    Setting {
        id: "use_tabs",
        title: "Indent with",
        page: SettingsPage::EditorCodeStyle,
        kind: SettingKind::Choice,
        field: SettingField::UseTabs,
        keywords: "tabs spaces indentation code style",
    },
    Setting {
        id: "completion_case",
        title: "Case sensitive completion",
        page: SettingsPage::EditorCompletion,
        kind: SettingKind::Choice,
        field: SettingField::CompletionCase,
        keywords: "case completion match code completion",
    },
    Setting {
        id: "completion_auto_popup",
        title: "Show the completion list automatically",
        page: SettingsPage::EditorCompletion,
        kind: SettingKind::Toggle,
        field: SettingField::CompletionAutoPopup,
        keywords: "completion popup automatic code completion",
    },
    Setting {
        id: "completion_delay_ms",
        title: "Completion delay",
        page: SettingsPage::EditorCompletion,
        kind: SettingKind::Number,
        field: SettingField::CompletionDelay,
        keywords: "completion delay wait milliseconds code completion",
    },
    Setting {
        id: "semantic_highlighting",
        title: "Semantic highlighting",
        page: SettingsPage::Editor,
        kind: SettingKind::Toggle,
        field: SettingField::SemanticHighlighting,
        keywords: "semantic tokens fields parameters static deprecated editor colours",
    },
    Setting {
        id: "inlay_parameter_names",
        title: "Parameter names",
        page: SettingsPage::EditorInlayHints,
        kind: SettingKind::Choice,
        field: SettingField::InlayParameterNames,
        keywords: "inlay hints parameter names arguments literals inlay hints",
    },
    Setting {
        id: "inlay_type_hints",
        title: "Show type hints",
        page: SettingsPage::EditorInlayHints,
        kind: SettingKind::Toggle,
        field: SettingField::InlayTypeHints,
        keywords: "inlay hints type variable return inlay hints",
    },
    Setting {
        id: "quick_doc_on_hover",
        title: "Show quick documentation on hover",
        page: SettingsPage::Editor,
        kind: SettingKind::Toggle,
        field: SettingField::QuickDocOnHover,
        keywords: "quick documentation hover popup delay editor",
    },
    Setting {
        id: "code_vision_usages",
        title: "Usages",
        page: SettingsPage::EditorInlayHints,
        kind: SettingKind::Toggle,
        field: SettingField::CodeVisionUsages,
        keywords: "code vision usages references usages count inlay hints",
    },
    Setting {
        id: "code_vision_implementations",
        title: "Implementations / Inheritors",
        page: SettingsPage::EditorInlayHints,
        kind: SettingKind::Toggle,
        field: SettingField::CodeVisionImplementations,
        keywords: "code vision implementations inheritors subtypes inlay hints",
    },
    Setting {
        id: "build_tool_first_home",
        title: "Home",
        page: SettingsPage::BuildToolsFirst,
        kind: SettingKind::Choice,
        field: SettingField::BuildToolFirstHomeKind,
        keywords: "home distribution wrapper local installation downloaded location build tools",
    },
    Setting {
        id: "build_tool_first_settings_file",
        title: "User settings file",
        page: SettingsPage::BuildToolsFirst,
        kind: SettingKind::Text,
        field: SettingField::BuildToolFirstSettingsFile,
        keywords: "user settings file build tools configuration",
    },
    Setting {
        id: "build_tool_first_repository",
        title: "Local repository",
        page: SettingsPage::BuildToolsFirst,
        kind: SettingKind::Text,
        field: SettingField::BuildToolFirstRepository,
        keywords: "local repository cache artifacts build tools",
    },
    Setting {
        id: "build_tool_first_offline",
        title: "Work offline",
        page: SettingsPage::BuildToolsFirst,
        kind: SettingKind::Toggle,
        field: SettingField::BuildToolFirstOffline,
        keywords: "offline network build tools",
    },
    Setting {
        id: "build_tool_first_vm_options",
        title: "VM options for import and runs",
        page: SettingsPage::BuildToolsFirst,
        kind: SettingKind::Text,
        field: SettingField::BuildToolFirstVmOptions,
        keywords: "vm options memory arguments build tools",
    },
    Setting {
        id: "build_tool_first_update_snapshots",
        title: "Always update snapshots",
        page: SettingsPage::BuildToolsFirst,
        kind: SettingKind::Toggle,
        field: SettingField::BuildToolFirstUpdateSnapshots,
        keywords: "update snapshots refresh dependencies build tools",
    },
    Setting {
        id: "download_sources_automatically",
        title: "Download sources automatically",
        page: SettingsPage::BuildToolsFirst,
        kind: SettingKind::Toggle,
        field: SettingField::DownloadSourcesAutomatically,
        keywords: "sources download library decompiled class build tools",
    },
    Setting {
        id: "check_dependency_updates",
        title: "Check for dependency updates",
        page: SettingsPage::BuildToolsFirst,
        kind: SettingKind::Toggle,
        field: SettingField::CheckDependencyUpdates,
        keywords: "dependency updates versions build files repositories build tools",
    },
    Setting {
        id: "build_tool_second_home",
        title: "Distribution",
        page: SettingsPage::BuildToolsSecond,
        kind: SettingKind::Choice,
        field: SettingField::BuildToolSecondHome,
        keywords: "distribution wrapper local installation downloaded location build tools",
    },
    Setting {
        id: "build_tool_second_user_home",
        title: "User home",
        page: SettingsPage::BuildToolsSecond,
        kind: SettingKind::Text,
        field: SettingField::BuildToolSecondUserHome,
        keywords: "user home cache data build tools",
    },
    Setting {
        id: "build_tool_second_runtime_home",
        title: "JVM",
        page: SettingsPage::BuildToolsSecond,
        kind: SettingKind::Text,
        field: SettingField::BuildToolSecondRuntimeHome,
        keywords: "jvm runtime sdk jdk build tools",
    },
    Setting {
        id: "build_tool_second_offline",
        title: "Work offline",
        page: SettingsPage::BuildToolsSecond,
        kind: SettingKind::Toggle,
        field: SettingField::BuildToolSecondOffline,
        keywords: "offline network build tools",
    },
    Setting {
        id: "git_path",
        title: "Path to Git executable",
        page: SettingsPage::Git,
        kind: SettingKind::Text,
        field: SettingField::GitPath,
        keywords: "git executable path version control",
    },
    Setting {
        id: "git_update_method",
        title: "Update method",
        page: SettingsPage::Git,
        kind: SettingKind::Choice,
        field: SettingField::GitUpdateMethod,
        keywords: "update project merge rebase version control",
    },
    Setting {
        id: "git_auto_update_on_reject",
        title: "Auto-update if push of the current branch was rejected",
        page: SettingsPage::Git,
        kind: SettingKind::Toggle,
        field: SettingField::GitAutoUpdateOnReject,
        keywords: "push rejected auto update merge rebase version control",
    },
    Setting {
        id: "git_update_submodules",
        title: "Update submodules on project update",
        page: SettingsPage::Git,
        kind: SettingKind::Toggle,
        field: SettingField::GitUpdateSubmodules,
        keywords: "submodule update project git version control",
    },
    Setting {
        id: "git_warn_crlf",
        title: "Warn if CRLF line separators are about to be committed",
        page: SettingsPage::Git,
        kind: SettingKind::Toggle,
        field: SettingField::GitWarnCrlf,
        keywords: "crlf line separators commit warning version control",
    },
    Setting {
        id: "git_analyze_before_commit",
        title: "Analyze code before commit",
        page: SettingsPage::Git,
        kind: SettingKind::Toggle,
        field: SettingField::GitAnalyzeBeforeCommit,
        keywords: "analyze inspection commit version control",
    },
    Setting {
        id: "terminal_shell",
        title: "Shell path",
        page: SettingsPage::Terminal,
        kind: SettingKind::Text,
        field: SettingField::TerminalShell,
        keywords: "shell path terminal command",
    },
    Setting {
        id: "terminal_start_directory",
        title: "Start directory",
        page: SettingsPage::Terminal,
        kind: SettingKind::Text,
        field: SettingField::TerminalStartDirectory,
        keywords: "start directory folder terminal working",
    },
    Setting {
        id: "terminal_environment",
        title: "Environment variables",
        page: SettingsPage::Terminal,
        kind: SettingKind::Text,
        field: SettingField::TerminalEnvironment,
        keywords: "environment variables terminal",
    },
    Setting {
        id: "terminal_confirm_multiline_paste",
        title: "Confirm pasting multi-line text",
        page: SettingsPage::Terminal,
        kind: SettingKind::Toggle,
        field: SettingField::TerminalConfirmMultilinePaste,
        keywords: "paste multiline confirmation terminal",
    },
    Setting {
        id: "render_doc_comments",
        title: "Render documentation comments",
        page: SettingsPage::Editor,
        kind: SettingKind::Toggle,
        field: SettingField::RenderDocComments,
        keywords: "documentation comments doc rendered javadoc editor appearance",
    },
    Setting {
        id: "markdown_images_from_internet",
        title: "Load images from the internet",
        page: SettingsPage::Editor,
        kind: SettingKind::Toggle,
        field: SettingField::MarkdownImagesFromInternet,
        keywords: "markdown preview images internet network picture editor",
    },
    Setting {
        id: "diff_side_by_side",
        title: "Show diffs side by side",
        page: SettingsPage::Editor,
        kind: SettingKind::Toggle,
        field: SettingField::DiffSideBySide,
        keywords: "diff side by side unified compare version control editor",
    },
    Setting {
        id: "git_group_by_directory",
        title: "Group by Directory",
        page: SettingsPage::Git,
        kind: SettingKind::Toggle,
        field: SettingField::GitGroupByDirectory,
        keywords: "branch popup folders group directory remote version control",
    },
    Setting {
        id: "http_timeout",
        title: "HTTP request timeout",
        page: SettingsPage::HttpClient,
        kind: SettingKind::Number,
        field: SettingField::HttpTimeout,
        keywords: "http rest request timeout seconds network client",
    },
    Setting {
        id: "debug_show_values_inline",
        title: "Show values inline",
        page: SettingsPage::Debugger,
        kind: SettingKind::Toggle,
        field: SettingField::DebugShowValuesInline,
        keywords: "debugger values variables inline editor",
    },
    Setting {
        id: "debug_hide_library_frames",
        title: "Hide Frames from Libraries",
        page: SettingsPage::Debugger,
        kind: SettingKind::Toggle,
        field: SettingField::DebugHideLibraryFrames,
        keywords: "debugger stack frames library filter hide fold",
    },
    Setting {
        id: "debug_type_renderers",
        title: "Type Renderers",
        page: SettingsPage::Debugger,
        kind: SettingKind::Text,
        field: SettingField::DebugTypeRenderers,
        keywords: "debugger data views type renderer display expression values",
    },
    Setting {
        id: "presentation_font_size",
        title: "Presentation Mode font size",
        page: SettingsPage::Appearance,
        kind: SettingKind::Number,
        field: SettingField::PresentationFontSize,
        keywords: "presentation mode font size text appearance",
    },
    Setting {
        id: "toolbar_visible",
        title: "Toolbar",
        page: SettingsPage::Appearance,
        kind: SettingKind::Toggle,
        field: SettingField::ToolbarVisible,
        keywords: "toolbar main window part appearance",
    },
    Setting {
        id: "tool_window_bars_visible",
        title: "Tool Window Bars",
        page: SettingsPage::Appearance,
        kind: SettingKind::Toggle,
        field: SettingField::ToolWindowBarsVisible,
        keywords: "tool window bars stripes buttons appearance",
    },
    Setting {
        id: "status_bar_visible",
        title: "Status Bar",
        page: SettingsPage::Appearance,
        kind: SettingKind::Toggle,
        field: SettingField::StatusBarVisible,
        keywords: "status bar window part appearance",
    },
    Setting {
        id: "navigation_bar_visible",
        title: "Navigation Bar",
        page: SettingsPage::Appearance,
        kind: SettingKind::Toggle,
        field: SettingField::NavigationBarVisible,
        keywords: "navigation bar breadcrumbs window part appearance",
    },
    Setting {
        id: "right_margin",
        title: "Right margin",
        page: SettingsPage::Editor,
        kind: SettingKind::Number,
        field: SettingField::RightMargin,
        keywords: "right margin column wrap distract editor",
    },
    Setting {
        id: "show_gutter_icons",
        title: "Gutter Icons",
        page: SettingsPage::Editor,
        kind: SettingKind::Toggle,
        field: SettingField::ShowGutterIcons,
        keywords: "gutter icons breakpoints bookmarks folds editor general",
    },
    Setting {
        id: "show_breadcrumbs",
        title: "Breadcrumbs",
        page: SettingsPage::Editor,
        kind: SettingKind::Toggle,
        field: SettingField::ShowBreadcrumbs,
        keywords: "breadcrumbs navigation declarations editor general",
    },
    Setting {
        id: "show_sticky_lines",
        title: "Sticky Lines",
        page: SettingsPage::Editor,
        kind: SettingKind::Toggle,
        field: SettingField::ShowStickyLines,
        keywords: "sticky lines header declarations editor general",
    },
    Setting {
        id: "kubectl_path",
        title: "Path to kubectl",
        page: SettingsPage::Kubernetes,
        kind: SettingKind::Text,
        field: SettingField::KubectlPath,
        keywords: "kubectl kubernetes executable path cluster",
    },
    Setting {
        id: "kubeconfig_path",
        title: "Path to kubeconfig",
        page: SettingsPage::Kubernetes,
        kind: SettingKind::Text,
        field: SettingField::KubeconfigPath,
        keywords: "kubeconfig kubernetes configuration contexts cluster",
    },
    Setting {
        id: "heap_snapshot_folder",
        title: "Memory snapshot folder",
        page: SettingsPage::Debugger,
        kind: SettingKind::Text,
        field: SettingField::HeapSnapshotFolder,
        keywords: "heap dump memory snapshot hprof profiler capture folder directory",
    },
    Setting {
        id: "database_page_rows",
        title: "Rows per page",
        page: SettingsPage::Database,
        kind: SettingKind::Number,
        field: SettingField::DatabasePageRows,
        keywords: "database table rows page grid data editor",
    },
    Setting {
        id: "icon_set",
        title: "Icons",
        page: SettingsPage::Appearance,
        kind: SettingKind::Choice,
        field: SettingField::IconSet,
        keywords: "icons icon set appearance arro intellij",
    },
    Setting {
        id: "project_always_select_opened_file",
        title: "Always Select Opened File",
        page: SettingsPage::Project,
        kind: SettingKind::Toggle,
        field: SettingField::ProjectAlwaysSelectOpenedFile,
        keywords: "project tree select opened file autoscroll source follow editor tab",
    },
    Setting {
        id: "build_tool_first_terminal_path",
        title: "Add the downloaded tool to the PATH of terminals",
        page: SettingsPage::BuildToolsFirst,
        kind: SettingKind::Toggle,
        field: SettingField::BuildToolFirstTerminalPath,
        keywords: "terminal path environment downloaded bundled build tools",
    },
    Setting {
        id: "build_tool_second_terminal_path",
        title: "Add the downloaded tool to the PATH of terminals",
        page: SettingsPage::BuildToolsSecond,
        kind: SettingKind::Toggle,
        field: SettingField::BuildToolSecondTerminalPath,
        keywords: "terminal path environment downloaded bundled build tools",
    },
    Setting {
        id: "mcp_enabled",
        title: "Enable MCP Server",
        page: SettingsPage::Mcp,
        kind: SettingKind::Toggle,
        field: SettingField::McpEnabled,
        keywords: "mcp server enable ai agents claude codex cursor integrate tools",
    },
    Setting {
        id: "mcp_binary",
        title: "Path to arro-mcp",
        page: SettingsPage::Mcp,
        kind: SettingKind::Text,
        field: SettingField::McpBinary,
        keywords: "mcp arro-mcp binary path ai agents claude codex",
    },
    Setting {
        id: "mcp_execute_permission",
        title: "Run shell commands or run configurations without confirmation (brave mode)",
        page: SettingsPage::Mcp,
        kind: SettingKind::Choice,
        field: SettingField::McpExecutePermission,
        keywords: "mcp brave mode permission confirmation allow ask trusted project build test run terminal agent",
    },
    Setting {
        id: "sql_keyword_case",
        title: "Keyword case",
        page: SettingsPage::Database,
        kind: SettingKind::Choice,
        field: SettingField::SqlKeywordCase,
        keywords: "sql keyword case upper lower reformat console script database",
    },
    Setting {
        id: "sql_script_stop_on_error",
        title: "Stop Run SQL Script at the first error",
        page: SettingsPage::Database,
        kind: SettingKind::Toggle,
        field: SettingField::SqlScriptStopOnError,
        keywords: "sql script run error stop continue statement database",
    },
    Setting {
        id: "ai_enabled",
        title: "Enable AI features",
        page: SettingsPage::ToolsAi,
        kind: SettingKind::Toggle,
        field: SettingField::AiEnabled,
        keywords: "ai assistant ask fix explain agent features",
    },
    Setting {
        id: "project_compact_middle_packages",
        title: "Compact Middle Packages",
        page: SettingsPage::Project,
        kind: SettingKind::Toggle,
        field: SettingField::ProjectCompactMiddlePackages,
        keywords: "project tree compact middle packages folders chain one subfolder package name dots",
    },
    Setting {
        id: "mcp_router_only_mode",
        title: "Enable router-only mode",
        page: SettingsPage::Mcp,
        kind: SettingKind::Toggle,
        field: SettingField::McpRouterOnlyMode,
        keywords: "mcp router only mode execute_tool exposed tools agents acp",
    },
    Setting {
        id: "mcp_router_only_scope",
        title: "Router-only mode for",
        page: SettingsPage::Mcp,
        kind: SettingKind::Choice,
        field: SettingField::McpRouterOnlyScope,
        keywords: "mcp router only mode scope agents acp all",
    },
    Setting {
        id: "mcp_terminal_suggestions",
        title: "Show setup suggestions for Codex and Claude terminal sessions",
        page: SettingsPage::Mcp,
        kind: SettingKind::Toggle,
        field: SettingField::McpTerminalSuggestions,
        keywords: "mcp terminal codex claude setup suggestion configure auto",
    },
    Setting {
        id: "todo_templates",
        title: "TODO patterns",
        page: SettingsPage::EditorTodo,
        kind: SettingKind::Text,
        field: SettingField::TodoTemplates,
        keywords: "todo fixme hack patterns keywords markers comments editor",
    },
    Setting {
        id: "todo_case_sensitive",
        title: "Case sensitive",
        page: SettingsPage::EditorTodo,
        kind: SettingKind::Toggle,
        field: SettingField::TodoCaseSensitive,
        keywords: "todo patterns case sensitive editor",
    },
    Setting {
        id: "ignored_files",
        title: "Ignored files and folders",
        page: SettingsPage::EditorFileTypes,
        kind: SettingKind::Text,
        field: SettingField::IgnoredFiles,
        keywords: "file types masks exclude hide tree project class node_modules",
    },
    Setting {
        id: "terminal_escape_focus",
        title: "Move focus to the editor with Escape",
        page: SettingsPage::Terminal,
        kind: SettingKind::Toggle,
        field: SettingField::TerminalEscapeFocus,
        keywords: "escape focus editor terminal move shell vim less",
    },
    Setting {
        id: "memory_low_check",
        title: "Stop the language server when memory is low",
        page: SettingsPage::Memory,
        kind: SettingKind::Toggle,
        field: SettingField::MemoryLowCheck,
        keywords: "memory ram swap commit stop language server low check off",
    },
    Setting {
        id: "memory_low_megabytes",
        title: "Stop the language server when free memory is below",
        page: SettingsPage::Memory,
        kind: SettingKind::Number,
        field: SettingField::MemoryLowMegabytes,
        keywords: "memory ram swap commit stop language server megabytes floor threshold",
    },
    Setting {
        id: "memory_low_percent",
        title: "and below this percentage of the total memory",
        page: SettingsPage::Memory,
        kind: SettingKind::Number,
        field: SettingField::MemoryLowPercent,
        keywords: "memory ram swap commit stop language server percent share total threshold",
    },
    Setting {
        id: "language_max_heap_megabytes",
        title: "Maximum heap of the language server",
        page: SettingsPage::Memory,
        kind: SettingKind::Text,
        field: SettingField::LanguageMaxHeap,
        keywords: "memory heap xmx megabytes limit language server automatic choose",
    },
    Setting {
        id: "build_tool_second_home_path",
        title: "Local installation",
        page: SettingsPage::BuildToolsSecond,
        kind: SettingKind::Text,
        field: SettingField::BuildToolSecondHomePath,
        keywords: "local installation folder path distribution wrapper build tools",
    },
    Setting {
        id: "build_tool_second_wrapper_download",
        title: "Download the wrapper distribution automatically",
        page: SettingsPage::BuildToolsSecond,
        kind: SettingKind::Toggle,
        field: SettingField::BuildToolSecondWrapperDownload,
        keywords: "download wrapper distribution automatic network build tools",
    },
    Setting {
        id: "build_tool_first_home_path",
        title: "Local installation",
        page: SettingsPage::BuildToolsFirst,
        kind: SettingKind::Text,
        field: SettingField::BuildToolFirstHome,
        keywords: "local installation folder path distribution wrapper build tools",
    },
];

/// Every setting the window and the search know.
pub fn all() -> &'static [Setting] {
    SETTINGS
}

/// The setting with this id, when there is one.
pub fn find(id: &str) -> Option<&'static Setting> {
    SETTINGS.iter().find(|setting| setting.id == id)
}

/// The group of settings a row belongs to on its page, for the pages that divide their
/// rows into named groups.
pub fn section_of(id: &str) -> Option<&'static str> {
    match id {
        "render_doc_comments" => Some("Appearance"),
        "markdown_images_from_internet" => Some("Markdown"),
        "code_vision_usages" | "code_vision_implementations" => Some("Code Vision"),
        "debug_hide_library_frames" | "debug_type_renderers" => Some("Data Views"),
        "ui_scale" => Some("UI"),
        "presentation_font_size"
        | "toolbar_visible"
        | "tool_window_bars_visible"
        | "status_bar_visible"
        | "navigation_bar_visible" => Some("Window"),
        "right_margin" | "show_gutter_icons" | "show_breadcrumbs" | "show_sticky_lines" => {
            Some("Appearance")
        }
        "heap_snapshot_folder" => Some("Data Views"),
        _ => None,
    }
}

/// The grey text of an empty text field: the value applied when the field stays empty.
pub fn text_hint(field: SettingField) -> &'static str {
    match field {
        SettingField::EditorFontFamily => "JetBrains Mono",
        SettingField::BuildToolFirstHome => "C:/tools/build-tool",
        SettingField::BuildToolFirstSettingsFile => "~/.m2/settings.xml",
        SettingField::BuildToolFirstVmOptions => "-Xmx2g",
        SettingField::BuildToolSecondHomePath => "C:/tools/build-tool",
        SettingField::BuildToolSecondUserHome => "Default",
        SettingField::GitPath => "Found on PATH",
        SettingField::TerminalShell => "Detected automatically",
        SettingField::TerminalStartDirectory => "Project root",
        SettingField::TerminalEnvironment => "NAME=value per line",
        SettingField::KubectlPath => "Found on PATH",
        SettingField::KubeconfigPath => "~/.kube/config",
        SettingField::McpBinary => "Next to Arro",
        SettingField::LanguageMaxHeap => "Automatic",
        _ => "",
    }
}

/// Every word of `query` is in `text`, case ignored.
fn words_in(text: &str, query: &str) -> bool {
    let text = text.to_lowercase();
    query
        .split_whitespace()
        .all(|word| text.contains(&word.to_lowercase()))
}

impl Setting {
    /// Every word of `query` is in the name, the page, the extra words or the hint of the
    /// field, case ignored.
    pub fn matches(&self, query: &str) -> bool {
        let text = format!(
            "{} {} {} {}",
            self.title,
            self.page.title(),
            self.keywords,
            text_hint(self.field)
        );
        words_in(&text, query)
    }
}

/// Settings in the order of the list that match `query`, at most `limit` of them.
pub fn search(query: &str, limit: usize) -> Vec<&'static Setting> {
    SETTINGS
        .iter()
        .filter(|setting| setting.matches(query))
        .take(limit)
        .collect()
}

/// How a page answers `query`; the smaller the rank, the earlier the search opens it. The
/// name of the page comes before the name of a setting on it, and both come before the
/// extra words and the hint of a field.
fn page_rank(page: SettingsPage, query: &str) -> Option<u8> {
    let title = page.title().to_lowercase();
    let wanted = query.to_lowercase();
    if title == wanted {
        return Some(0);
    }
    if title.starts_with(&wanted) {
        return Some(1);
    }
    // A query that names the page and then adds a word: `font size` opens Font before it
    // looks for the words in the settings of every page.
    if wanted.starts_with(&title) && wanted[title.len()..].starts_with(char::is_whitespace) {
        return Some(2);
    }
    let mut title_match = false;
    let mut word_match = false;
    for setting in SETTINGS.iter().filter(|setting| setting.page == page) {
        title_match |= words_in(setting.title, query);
        word_match |= setting.matches(query);
    }
    if title_match {
        Some(3)
    } else if word_match {
        Some(4)
    } else {
        None
    }
}

/// The page the search opens for `query`: the page with the best match of its name, the
/// name of a setting on it, or the words and hints of a setting; the first page of the tree
/// wins a tie. None for an empty query, which keeps the page in force.
pub fn page_matching(query: &str) -> Option<SettingsPage> {
    let query = query.trim();
    if query.is_empty() {
        return None;
    }
    let mut best: Option<(u8, SettingsPage)> = None;
    for page in SettingsPage::ALL {
        let Some(rank) = page_rank(page, query) else {
            continue;
        };
        if best.is_none_or(|(best_rank, _)| rank < best_rank) {
            best = Some((rank, page));
        }
    }
    best.map(|(_, page)| page.target())
}

/// The pages the search keeps, in the order of the tree: the pages of the settings that
/// match and the groups above them. The Keymap and template pages have a search of their
/// own and stay when the query is empty only.
pub fn pages_matching(query: &str) -> Vec<SettingsPage> {
    if query.trim().is_empty() {
        return SettingsPage::ALL.to_vec();
    }
    let mut kept = Vec::new();
    for setting in SETTINGS.iter().filter(|setting| setting.matches(query)) {
        let mut page = Some(setting.page);
        while let Some(current) = page {
            if !kept.contains(&current) {
                kept.push(current);
            }
            page = current.parent();
        }
    }
    SettingsPage::ALL
        .iter()
        .copied()
        .filter(|page| kept.contains(page))
        .collect()
}

/// The page a row of the tree opens when the tree keeps `kept`. A row whose own page the tree
/// keeps opens that page; a group opens its first part the tree keeps, or else its own page.
pub fn opened_by(row: SettingsPage, kept: &[SettingsPage]) -> SettingsPage {
    let target = row.target();
    if kept.contains(&target) {
        return target;
    }
    kept.iter()
        .copied()
        .find(|page| page.parent() == Some(row))
        .unwrap_or(row)
}

/// The pages of the tree `kept` that show their own content, in the order of the tree. A group
/// row is left out: it opens a part that the tree lists in its own place.
pub fn shown_pages(kept: &[SettingsPage]) -> Vec<SettingsPage> {
    kept.iter()
        .copied()
        .filter(|page| opened_by(*page, kept) == *page)
        .collect()
}

/// The pages with a setting that matches `query`, in the order of the tree. A page that is only
/// the group of a matching page is left out, so a search opens the page that holds the match.
pub fn matching_pages(query: &str) -> Vec<SettingsPage> {
    SettingsPage::ALL
        .iter()
        .copied()
        .filter(|page| {
            SETTINGS
                .iter()
                .any(|setting| setting.page == *page && setting.matches(query))
        })
        .collect()
}

/// The switch of a boolean setting; `None` for a setting that is not a switch.
pub fn value(settings: &Settings, id: &str) -> Option<bool> {
    match find(id)?.field {
        SettingField::ThemeSyncWithOs => Some(settings.theme_sync_with_os),
        SettingField::SoftWrap => Some(settings.soft_wrap),
        SettingField::ShowWhitespace => Some(settings.show_whitespace),
        SettingField::ShowLineNumbers => Some(settings.show_line_numbers),
        SettingField::ProjectAlwaysSelectOpenedFile => {
            Some(settings.project_always_select_opened_file)
        }
        SettingField::HighlightCurrentLine => Some(settings.highlight_current_line),
        SettingField::ShowIndentGuides => Some(settings.show_indent_guides),
        SettingField::CaretBlink => Some(settings.caret_blink),
        SettingField::EnsureFinalNewline => Some(settings.ensure_final_newline),
        SettingField::ActionsOnSaveReformat => Some(settings.actions_on_save_reformat),
        SettingField::ActionsOnSaveOptimizeImports => {
            Some(settings.actions_on_save_optimize_imports)
        }
        SettingField::ActionsOnSaveCleanup => Some(settings.actions_on_save_cleanup),
        SettingField::ZoomWithWheel => Some(settings.zoom_with_wheel),
        SettingField::CompletionAutoPopup => Some(settings.completion_auto_popup),
        SettingField::InlayTypeHints => Some(settings.inlay_type_hints),
        SettingField::CodeVisionUsages => Some(settings.code_vision_usages),
        SettingField::CodeVisionImplementations => Some(settings.code_vision_implementations),
        SettingField::RenderDocComments => Some(settings.render_doc_comments),
        SettingField::MarkdownImagesFromInternet => Some(settings.markdown_images_from_internet),
        SettingField::Autosave => Some(settings.autosave),
        SettingField::OpenReadmeOnFirstOpen => Some(settings.open_readme_on_first_open),
        SettingField::UpdatesCheckAutomatically => Some(settings.updates_check_automatically),
        SettingField::SemanticHighlighting => Some(settings.semantic_highlighting),
        SettingField::QuickDocOnHover => Some(settings.quick_doc_on_hover),
        SettingField::DiffSideBySide => Some(settings.diff_side_by_side),
        SettingField::GitGroupByDirectory => Some(settings.git_group_by_directory),
        SettingField::BuildToolFirstOffline => Some(settings.build_tools.first.offline),
        SettingField::BuildToolFirstTerminalPath => Some(settings.build_tools.first.terminal_path),
        SettingField::McpEnabled => Some(settings.mcp_enabled),
        SettingField::McpRouterOnlyMode => Some(settings.mcp_router_only_mode),
        SettingField::McpTerminalSuggestions => Some(settings.mcp_terminal_suggestions),
        SettingField::TerminalEscapeFocus => Some(settings.terminal_escape_focus),
        SettingField::MemoryLowCheck => {
            Some(settings.memory_check_on(::platform::is_virtual_machine()))
        }
        SettingField::TodoCaseSensitive => Some(settings.todo_case_sensitive),
        SettingField::AiEnabled => Some(settings.ai_enabled),
        SettingField::ProjectCompactMiddlePackages => {
            Some(settings.project_compact_middle_packages)
        }
        SettingField::BuildToolSecondTerminalPath => {
            Some(settings.build_tools.second.terminal_path)
        }
        SettingField::BuildToolFirstUpdateSnapshots => {
            Some(settings.build_tools.first.update_snapshots)
        }
        SettingField::BuildToolSecondOffline => Some(settings.build_tools.second.offline),
        SettingField::BuildToolSecondWrapperDownload => {
            Some(settings.build_tools.second.download_wrapper)
        }
        SettingField::SqlScriptStopOnError => Some(settings.sql_script_stop_on_error),
        SettingField::DownloadSourcesAutomatically => Some(settings.download_sources_automatically),
        SettingField::CheckDependencyUpdates => Some(settings.check_dependency_updates),
        SettingField::GitAutoUpdateOnReject => Some(settings.git_auto_update_on_reject),
        SettingField::GitUpdateSubmodules => Some(settings.git_update_submodules),
        SettingField::GitWarnCrlf => Some(settings.git_warn_crlf),
        SettingField::GitAnalyzeBeforeCommit => Some(settings.git_analyze_before_commit),
        SettingField::DebugShowValuesInline => Some(settings.debug_show_values_inline),
        SettingField::TerminalConfirmMultilinePaste => {
            Some(settings.terminal_confirm_multiline_paste)
        }
        SettingField::DebugHideLibraryFrames => Some(settings.debug_hide_library_frames),
        SettingField::ToolbarVisible => Some(settings.toolbar_visible),
        SettingField::ToolWindowBarsVisible => Some(settings.tool_window_bars_visible),
        SettingField::StatusBarVisible => Some(settings.status_bar_visible),
        SettingField::NavigationBarVisible => Some(settings.navigation_bar_visible),
        SettingField::ShowGutterIcons => Some(settings.show_gutter_icons),
        SettingField::ShowBreadcrumbs => Some(settings.show_breadcrumbs),
        SettingField::ShowStickyLines => Some(settings.show_sticky_lines),
        SettingField::FontSize
        | SettingField::UiScale
        | SettingField::EditorFontFamily
        | SettingField::LineSpacing
        | SettingField::TrimTrailingWhitespace
        | SettingField::ActionsOnSaveScope
        | SettingField::ActionsOnSaveFiles
        | SettingField::TabSize
        | SettingField::IndentSize
        | SettingField::UseTabs
        | SettingField::CompletionCase
        | SettingField::CompletionDelay
        | SettingField::InlayParameterNames
        | SettingField::BuildToolFirstHome
        | SettingField::BuildToolFirstHomeKind
        | SettingField::BuildToolFirstSettingsFile
        | SettingField::BuildToolFirstRepository
        | SettingField::BuildToolFirstVmOptions
        | SettingField::BuildToolSecondHome
        | SettingField::BuildToolSecondHomePath
        | SettingField::BuildToolSecondUserHome
        | SettingField::BuildToolSecondRuntimeHome
        | SettingField::GitPath
        | SettingField::GitUpdateMethod
        | SettingField::TerminalShell
        | SettingField::TerminalStartDirectory
        | SettingField::TerminalEnvironment
        | SettingField::HttpTimeout
        | SettingField::DebugTypeRenderers
        | SettingField::PresentationFontSize
        | SettingField::RightMargin
        | SettingField::HeapSnapshotFolder
        | SettingField::KubectlPath
        | SettingField::KubeconfigPath
        | SettingField::DatabasePageRows
        | SettingField::ThemeLight
        | SettingField::ThemeDark
        | SettingField::IconSet
        | SettingField::McpBinary
        | SettingField::McpExecutePermission
        | SettingField::McpRouterOnlyScope
        | SettingField::SqlKeywordCase
        | SettingField::TodoTemplates
        | SettingField::IgnoredFiles
        | SettingField::MemoryLowMegabytes
        | SettingField::MemoryLowPercent
        | SettingField::LanguageMaxHeap => None,
    }
}

/// Flips a boolean setting. False when the id is not a switch.
pub fn toggle(settings: &mut Settings, id: &str) -> bool {
    match find(id).map(|setting| setting.field) {
        Some(SettingField::IgnoredFiles) => return false,
        Some(SettingField::ThemeSyncWithOs) => {
            settings.theme_sync_with_os = !settings.theme_sync_with_os;
        }
        Some(SettingField::SoftWrap) => settings.soft_wrap = !settings.soft_wrap,
        Some(SettingField::ProjectAlwaysSelectOpenedFile) => {
            settings.project_always_select_opened_file =
                !settings.project_always_select_opened_file;
        }
        Some(SettingField::ShowWhitespace) => {
            settings.show_whitespace = !settings.show_whitespace;
        }
        Some(SettingField::ShowLineNumbers) => {
            settings.show_line_numbers = !settings.show_line_numbers;
        }
        Some(SettingField::HighlightCurrentLine) => {
            settings.highlight_current_line = !settings.highlight_current_line;
        }
        Some(SettingField::ShowIndentGuides) => {
            settings.show_indent_guides = !settings.show_indent_guides;
        }
        Some(SettingField::CaretBlink) => settings.caret_blink = !settings.caret_blink,
        Some(SettingField::EnsureFinalNewline) => {
            settings.ensure_final_newline = !settings.ensure_final_newline;
        }
        Some(SettingField::ActionsOnSaveReformat) => {
            settings.actions_on_save_reformat = !settings.actions_on_save_reformat;
        }
        Some(SettingField::ActionsOnSaveOptimizeImports) => {
            settings.actions_on_save_optimize_imports = !settings.actions_on_save_optimize_imports;
        }
        Some(SettingField::ActionsOnSaveCleanup) => {
            settings.actions_on_save_cleanup = !settings.actions_on_save_cleanup;
        }
        Some(SettingField::ZoomWithWheel) => settings.zoom_with_wheel = !settings.zoom_with_wheel,
        Some(SettingField::CompletionAutoPopup) => {
            settings.completion_auto_popup = !settings.completion_auto_popup;
        }
        Some(SettingField::InlayTypeHints) => {
            settings.inlay_type_hints = !settings.inlay_type_hints;
        }
        Some(SettingField::CodeVisionUsages) => {
            settings.code_vision_usages = !settings.code_vision_usages;
        }
        Some(SettingField::CodeVisionImplementations) => {
            settings.code_vision_implementations = !settings.code_vision_implementations;
        }
        Some(SettingField::RenderDocComments) => {
            settings.render_doc_comments = !settings.render_doc_comments;
        }
        Some(SettingField::MarkdownImagesFromInternet) => {
            settings.markdown_images_from_internet = !settings.markdown_images_from_internet;
        }
        Some(SettingField::Autosave) => settings.autosave = !settings.autosave,
        Some(SettingField::OpenReadmeOnFirstOpen) => {
            settings.open_readme_on_first_open = !settings.open_readme_on_first_open;
        }
        Some(SettingField::UpdatesCheckAutomatically) => {
            settings.updates_check_automatically = !settings.updates_check_automatically;
        }
        Some(SettingField::SemanticHighlighting) => {
            settings.semantic_highlighting = !settings.semantic_highlighting;
        }
        Some(SettingField::QuickDocOnHover) => {
            settings.quick_doc_on_hover = !settings.quick_doc_on_hover;
        }
        Some(SettingField::DiffSideBySide) => {
            settings.diff_side_by_side = !settings.diff_side_by_side;
        }
        Some(SettingField::GitGroupByDirectory) => {
            settings.git_group_by_directory = !settings.git_group_by_directory;
        }
        Some(SettingField::BuildToolFirstOffline) => {
            settings.build_tools.first.offline = !settings.build_tools.first.offline;
        }
        Some(SettingField::BuildToolFirstUpdateSnapshots) => {
            settings.build_tools.first.update_snapshots =
                !settings.build_tools.first.update_snapshots;
        }
        Some(SettingField::BuildToolFirstTerminalPath) => {
            settings.build_tools.first.terminal_path = !settings.build_tools.first.terminal_path;
        }
        Some(SettingField::McpEnabled) => settings.mcp_enabled = !settings.mcp_enabled,
        Some(SettingField::McpRouterOnlyMode) => {
            settings.mcp_router_only_mode = !settings.mcp_router_only_mode;
        }
        Some(SettingField::McpTerminalSuggestions) => {
            settings.mcp_terminal_suggestions = !settings.mcp_terminal_suggestions;
        }
        Some(SettingField::TerminalEscapeFocus) => {
            settings.terminal_escape_focus = !settings.terminal_escape_focus;
        }
        Some(SettingField::MemoryLowCheck) => {
            // The first flip makes the choice explicit: the switch shows the flipped value.
            let on = settings.memory_check_on(::platform::is_virtual_machine());
            settings.memory_low_check = Some(!on);
        }
        Some(SettingField::TodoCaseSensitive) => {
            settings.todo_case_sensitive = !settings.todo_case_sensitive;
        }
        Some(SettingField::SqlScriptStopOnError) => {
            settings.sql_script_stop_on_error = !settings.sql_script_stop_on_error;
        }
        Some(SettingField::AiEnabled) => settings.ai_enabled = !settings.ai_enabled,
        Some(SettingField::ProjectCompactMiddlePackages) => {
            settings.project_compact_middle_packages = !settings.project_compact_middle_packages;
        }
        Some(SettingField::BuildToolSecondTerminalPath) => {
            settings.build_tools.second.terminal_path = !settings.build_tools.second.terminal_path;
        }
        Some(SettingField::BuildToolSecondOffline) => {
            settings.build_tools.second.offline = !settings.build_tools.second.offline;
        }
        Some(SettingField::BuildToolSecondWrapperDownload) => {
            settings.build_tools.second.download_wrapper =
                !settings.build_tools.second.download_wrapper;
        }
        Some(SettingField::DownloadSourcesAutomatically) => {
            settings.download_sources_automatically = !settings.download_sources_automatically;
        }
        Some(SettingField::CheckDependencyUpdates) => {
            settings.check_dependency_updates = !settings.check_dependency_updates;
        }
        Some(SettingField::GitAutoUpdateOnReject) => {
            settings.git_auto_update_on_reject = !settings.git_auto_update_on_reject;
        }
        Some(SettingField::GitUpdateSubmodules) => {
            settings.git_update_submodules = !settings.git_update_submodules;
        }
        Some(SettingField::GitWarnCrlf) => settings.git_warn_crlf = !settings.git_warn_crlf,
        Some(SettingField::GitAnalyzeBeforeCommit) => {
            settings.git_analyze_before_commit = !settings.git_analyze_before_commit;
        }
        Some(SettingField::DebugShowValuesInline) => {
            settings.debug_show_values_inline = !settings.debug_show_values_inline;
        }
        Some(SettingField::TerminalConfirmMultilinePaste) => {
            settings.terminal_confirm_multiline_paste = !settings.terminal_confirm_multiline_paste;
        }
        Some(SettingField::DebugHideLibraryFrames) => {
            settings.debug_hide_library_frames = !settings.debug_hide_library_frames;
        }
        Some(SettingField::ToolbarVisible) => {
            settings.toolbar_visible = !settings.toolbar_visible;
        }
        Some(SettingField::ToolWindowBarsVisible) => {
            settings.tool_window_bars_visible = !settings.tool_window_bars_visible;
        }
        Some(SettingField::StatusBarVisible) => {
            settings.status_bar_visible = !settings.status_bar_visible;
        }
        Some(SettingField::NavigationBarVisible) => {
            settings.navigation_bar_visible = !settings.navigation_bar_visible;
        }
        Some(SettingField::ShowGutterIcons) => {
            settings.show_gutter_icons = !settings.show_gutter_icons;
        }
        Some(SettingField::ShowBreadcrumbs) => {
            settings.show_breadcrumbs = !settings.show_breadcrumbs;
        }
        Some(SettingField::ShowStickyLines) => {
            settings.show_sticky_lines = !settings.show_sticky_lines;
        }
        Some(
            SettingField::FontSize
            | SettingField::UiScale
            | SettingField::EditorFontFamily
            | SettingField::LineSpacing
            | SettingField::TrimTrailingWhitespace
            | SettingField::ActionsOnSaveScope
            | SettingField::ActionsOnSaveFiles
            | SettingField::TabSize
            | SettingField::IndentSize
            | SettingField::UseTabs
            | SettingField::CompletionCase
            | SettingField::CompletionDelay
            | SettingField::InlayParameterNames
            | SettingField::BuildToolFirstHome
            | SettingField::BuildToolFirstHomeKind
            | SettingField::BuildToolFirstSettingsFile
            | SettingField::BuildToolFirstRepository
            | SettingField::BuildToolFirstVmOptions
            | SettingField::BuildToolSecondHome
            | SettingField::BuildToolSecondHomePath
            | SettingField::BuildToolSecondUserHome
            | SettingField::BuildToolSecondRuntimeHome
            | SettingField::GitPath
            | SettingField::GitUpdateMethod
            | SettingField::TerminalShell
            | SettingField::TerminalStartDirectory
            | SettingField::TerminalEnvironment
            | SettingField::HttpTimeout
            | SettingField::DebugTypeRenderers
            | SettingField::PresentationFontSize
            | SettingField::RightMargin
            | SettingField::HeapSnapshotFolder
            | SettingField::KubectlPath
            | SettingField::KubeconfigPath
            | SettingField::DatabasePageRows
            | SettingField::ThemeLight
            | SettingField::ThemeDark
            | SettingField::IconSet
            | SettingField::McpBinary
            | SettingField::McpExecutePermission
            | SettingField::McpRouterOnlyScope
            | SettingField::SqlKeywordCase
            | SettingField::TodoTemplates
            | SettingField::MemoryLowMegabytes
            | SettingField::MemoryLowPercent
            | SettingField::LanguageMaxHeap,
        )
        | None => {
            return false;
        }
    }
    true
}

/// The number of a numeric setting, for the window to show.
pub fn number(settings: &Settings, id: &str) -> Option<f32> {
    match find(id)?.field {
        SettingField::FontSize => Some(settings.font_size),
        SettingField::PresentationFontSize => Some(settings.presentation_font_size),
        SettingField::UiScale => Some(settings.ui_scale),
        SettingField::LineSpacing => Some(settings.line_spacing),
        SettingField::RightMargin => Some(settings.right_margin as f32),
        SettingField::TabSize => Some(settings.tab_size as f32),
        SettingField::IndentSize => Some(settings.indent_size as f32),
        SettingField::CompletionDelay => Some(settings.completion_delay_ms as f32),
        SettingField::HttpTimeout => Some(settings.http_timeout_seconds as f32),
        SettingField::DatabasePageRows => Some(settings.database_page_rows as f32),
        SettingField::MemoryLowMegabytes => Some(settings.memory_low_megabytes as f32),
        SettingField::MemoryLowPercent => Some(settings.memory_low_percent),
        _ => None,
    }
}

/// The number of a numeric setting as the window writes it.
pub fn number_label(settings: &Settings, id: &str) -> Option<String> {
    let setting = find(id)?;
    let value = number(settings, id)?;
    Some(match setting.field {
        SettingField::FontSize | SettingField::TabSize | SettingField::IndentSize => {
            format!("{value}")
        }
        SettingField::PresentationFontSize => format!("{value}"),
        SettingField::RightMargin => format!("{value}"),
        SettingField::UiScale | SettingField::LineSpacing => format!("{value:.1}"),
        SettingField::CompletionDelay => format!("{value} ms"),
        SettingField::HttpTimeout => format!("{value} s"),
        SettingField::DatabasePageRows => format!("{value} rows"),
        SettingField::MemoryLowMegabytes => format!("{value} MB"),
        SettingField::MemoryLowPercent => format!("{value} %"),
        _ => format!("{value}"),
    })
}

/// One step up or down of a numeric setting. False when the id is not a number.
pub fn adjust_number(settings: &mut Settings, id: &str, delta: f32) -> bool {
    let Some(setting) = find(id) else {
        return false;
    };
    let update = |value: f32, low: f32, high: f32| {
        if !value.is_finite() {
            return low;
        }
        // The steps of the numbers with a fraction do not drift; the whole ones do not.
        (value + delta).clamp(low, high)
    };
    match setting.field {
        SettingField::FontSize => *settings = settings.with_font_step(delta),
        SettingField::PresentationFontSize => {
            *settings = settings.with_presentation_font_step(delta)
        }
        SettingField::UiScale => *settings = settings.with_ui_scale_step(delta),
        SettingField::LineSpacing => *settings = settings.with_line_spacing_step(delta),
        SettingField::RightMargin => {
            let value = update(
                settings.right_margin as f32,
                workspace::MIN_RIGHT_MARGIN as f32,
                workspace::MAX_RIGHT_MARGIN as f32,
            ) as u32;
            settings.right_margin = value;
        }
        SettingField::TabSize => {
            let value = update(settings.tab_size as f32, 1., MAX_INDENT_SIZE) as u32;
            // A tab size that still matches the step moves both, as the Code Style page
            // does until the user picks a step of its own.
            let follows = settings.tab_size == settings.indent_size;
            settings.tab_size = value;
            if follows {
                settings.indent_size = value;
            }
        }
        SettingField::IndentSize => {
            let value = update(settings.indent_size as f32, 1., MAX_INDENT_SIZE) as u32;
            settings.indent_size = value;
        }
        SettingField::CompletionDelay => {
            let value = update(
                settings.completion_delay_ms as f32,
                0.,
                MAX_COMPLETION_DELAY_MS as f32,
            ) as u32;
            settings.completion_delay_ms = value;
        }
        SettingField::HttpTimeout => {
            let value = update(
                settings.http_timeout_seconds as f32,
                1.,
                workspace::MAX_HTTP_TIMEOUT_SECONDS as f32,
            ) as u32;
            settings.http_timeout_seconds = value;
        }
        SettingField::DatabasePageRows => {
            let value = update(
                settings.database_page_rows as f32,
                1.,
                workspace::MAX_DATABASE_PAGE_ROWS as f32,
            ) as u32;
            settings.database_page_rows = value;
        }
        SettingField::MemoryLowMegabytes => {
            let value = update(
                settings.memory_low_megabytes as f32,
                0.,
                workspace::MAX_MEMORY_LOW_MEGABYTES as f32,
            ) as u32;
            settings.memory_low_megabytes = value;
        }
        SettingField::MemoryLowPercent => {
            settings.memory_low_percent = update(settings.memory_low_percent, 0., 100.);
        }
        _ => return false,
    }
    true
}

/// Puts a name into a setting. False when the id does not take text.
pub fn set_text(settings: &mut Settings, id: &str, text: &str) -> bool {
    let Some(setting) = find(id) else {
        return false;
    };
    if let Some((tool, property)) = tool_property(setting.field) {
        let tool = settings.build_tools.tool_mut(tool);
        match property {
            ToolProperty::Home => tool.home = text.to_owned(),
            ToolProperty::SettingsFile => tool.settings_file = text.to_owned(),
            ToolProperty::Repository => tool.repository = text.to_owned(),
            ToolProperty::VmOptions => tool.vm_options = text.to_owned(),
            ToolProperty::UserHome => tool.user_home = text.to_owned(),
            ToolProperty::RuntimeHome => tool.runtime_home = text.to_owned(),
            ToolProperty::Offline | ToolProperty::UpdateSnapshots => return false,
        }
        return true;
    }
    match setting.field {
        SettingField::EditorFontFamily => settings.editor_font_family = text.to_owned(),
        SettingField::ActionsOnSaveFiles => {
            settings.actions_on_save_reformat_files = text.to_owned()
        }
        SettingField::GitPath => settings.git_path = text.to_owned(),
        SettingField::TerminalShell => settings.terminal_shell = text.to_owned(),
        SettingField::TerminalStartDirectory => settings.terminal_start_directory = text.to_owned(),
        SettingField::TerminalEnvironment => settings.terminal_environment = text.to_owned(),
        SettingField::DebugTypeRenderers => settings.debug_type_renderers = text.to_owned(),
        SettingField::HeapSnapshotFolder => settings.heap_snapshot_folder = text.to_owned(),
        SettingField::KubectlPath => settings.kubectl_path = text.to_owned(),
        SettingField::KubeconfigPath => settings.kubeconfig_path = text.to_owned(),
        SettingField::McpBinary => settings.mcp_binary = text.to_owned(),
        SettingField::TodoTemplates => {
            let mut seen = std::collections::HashSet::new();
            settings.todo_templates = text
                .split([',', ';', '\n'])
                .map(str::trim)
                .filter(|template| !template.is_empty())
                .filter(|template| seen.insert(*template))
                .map(ToOwned::to_owned)
                .collect();
        }
        SettingField::IgnoredFiles => {
            settings.ignored_files = text
                .split([';', '\n'])
                .map(str::trim)
                .filter(|mask| !mask.is_empty())
                .map(str::to_owned)
                .collect();
        }
        SettingField::LanguageMaxHeap => {
            // Empty or zero leaves the choice to the language server; anything else is a
            // number of megabytes, kept within the limits of the settings.
            let text = text.trim();
            let megabytes = if text.is_empty() {
                0
            } else {
                match text.parse::<u32>() {
                    Ok(megabytes) => megabytes,
                    Err(_) => return false,
                }
            };
            settings.language_max_heap_megabytes = match megabytes {
                0 => 0,
                megabytes => megabytes.clamp(
                    workspace::MIN_LANGUAGE_MAX_HEAP_MB,
                    workspace::MAX_LANGUAGE_MAX_HEAP_MB,
                ),
            };
        }
        _ => return false,
    }
    true
}

/// The text of a setting that holds one, for the window to show.
pub fn text(settings: &Settings, id: &str) -> Option<String> {
    let setting = find(id)?;
    if let Some((tool, property)) = tool_property(setting.field) {
        let tool = settings.build_tools.tool(tool);
        let value = match property {
            ToolProperty::Home => &tool.home,
            ToolProperty::SettingsFile => &tool.settings_file,
            ToolProperty::Repository => &tool.repository,
            ToolProperty::VmOptions => &tool.vm_options,
            ToolProperty::UserHome => &tool.user_home,
            ToolProperty::RuntimeHome => &tool.runtime_home,
            ToolProperty::Offline | ToolProperty::UpdateSnapshots => return None,
        };
        return Some(value.clone());
    }
    Some(match setting.field {
        SettingField::EditorFontFamily => settings.editor_font_family.clone(),
        SettingField::ActionsOnSaveFiles => settings.actions_on_save_reformat_files.clone(),
        SettingField::GitPath => settings.git_path.clone(),
        SettingField::TerminalShell => settings.terminal_shell.clone(),
        SettingField::TerminalStartDirectory => settings.terminal_start_directory.clone(),
        SettingField::TerminalEnvironment => settings.terminal_environment.clone(),
        SettingField::DebugTypeRenderers => settings.debug_type_renderers.clone(),
        SettingField::HeapSnapshotFolder => settings.heap_snapshot_folder.clone(),
        SettingField::KubectlPath => settings.kubectl_path.clone(),
        SettingField::KubeconfigPath => settings.kubeconfig_path.clone(),
        SettingField::McpBinary => settings.mcp_binary.clone(),
        SettingField::TodoTemplates => settings.todo_templates.join(", "),
        SettingField::IgnoredFiles => settings.ignored_files.join("; "),
        SettingField::LanguageMaxHeap if settings.language_max_heap_megabytes == 0 => String::new(),
        SettingField::LanguageMaxHeap => settings.language_max_heap_megabytes.to_string(),
        _ => return None,
    })
}

/// The id of the theme the Appearance page marks as chosen: the theme of the window while
/// Sync with OS is off, and the theme of the system look while it is on. A settings file
/// that names no theme takes `on_screen`, the theme the window shows.
pub fn chosen_theme<'a>(settings: &'a Settings, dark: bool, on_screen: &'a str) -> &'a str {
    if settings.theme_sync_with_os {
        if dark {
            &settings.theme_dark
        } else {
            &settings.theme_light
        }
    } else if settings.theme.is_empty() {
        on_screen
    } else {
        &settings.theme
    }
}

/// The chosen name of a setting with named choices, for the window to show.
pub fn choice_label(settings: &Settings, id: &str) -> Option<String> {
    let field = find(id)?.field;
    let label = match field {
        SettingField::TrimTrailingWhitespace => {
            settings.trim_trailing_whitespace.label().to_owned()
        }
        SettingField::ActionsOnSaveScope => {
            settings.actions_on_save_reformat_scope.label().to_owned()
        }
        SettingField::UseTabs => (if settings.use_tabs { "Tabs" } else { "Spaces" }).to_owned(),
        SettingField::CompletionCase => settings.completion_case.label().to_owned(),
        SettingField::InlayParameterNames => settings.inlay_parameter_names.label().to_owned(),
        SettingField::GitUpdateMethod => settings.git_update_method.label().to_owned(),
        SettingField::ThemeLight => Theme::label(&settings.theme_light),
        SettingField::ThemeDark => Theme::label(&settings.theme_dark),
        SettingField::IconSet => IconCatalog::label(&settings.icon_set),
        SettingField::BuildToolFirstHomeKind | SettingField::BuildToolSecondHome => {
            distribution_label(chosen_home(settings, field)?).to_owned()
        }
        SettingField::McpExecutePermission => settings.mcp_execute_permission.label().to_owned(),
        SettingField::McpRouterOnlyScope => settings.mcp_router_only_scope.label().to_owned(),
        SettingField::SqlKeywordCase => settings.sql_keyword_case.label().to_owned(),
        _ => return None,
    };
    Some(label)
}

/// The name of a way to find the home of a build tool. The list has three items; the
/// automatic order of an old file shows the wrapper it starts with. An old file whose
/// kind is `PATH` shows the wrapper too, and keeps its kind until the list is used.
fn distribution_label(kind: ToolHome) -> &'static str {
    match kind {
        ToolHome::Bundled => "Downloaded by Arro",
        ToolHome::Custom => "Local installation",
        ToolHome::Auto | ToolHome::Wrapper | ToolHome::Path => "Wrapper",
    }
}

/// The kinds a home list offers, in its order.
const HOME_KINDS: [ToolHome; 3] = [ToolHome::Wrapper, ToolHome::Custom, ToolHome::Bundled];

/// The tool of a home list and the row of the folder of a local installation.
fn home_choice(field: SettingField) -> Option<(usize, &'static str)> {
    match field {
        SettingField::BuildToolFirstHomeKind => Some((0, "build_tool_first_home_path")),
        SettingField::BuildToolSecondHome => Some((1, "build_tool_second_home_path")),
        _ => None,
    }
}

/// The source of the home a list names.
pub fn chosen_home(settings: &Settings, field: SettingField) -> Option<ToolHome> {
    let (tool, _) = home_choice(field)?;
    Some(settings.build_tools.tool(tool).chosen_home_kind())
}

/// The id of the row of the folder of a home list: the row that belongs to the choice.
pub fn home_path_id(field: SettingField) -> Option<&'static str> {
    home_choice(field).map(|(_, id)| id)
}

/// The next choice of a setting with named choices. False when it has none.
pub fn next_choice(settings: &mut Settings, id: &str) -> bool {
    let Some(field) = find(id).map(|setting| setting.field) else {
        return false;
    };
    if let Some((tool, _)) = home_choice(field) {
        let current = settings.build_tools.tool(tool).chosen_home_kind();
        let index = HOME_KINDS
            .iter()
            .position(|kind| *kind == current)
            .unwrap_or(0);
        settings.build_tools.tool_mut(tool).home_kind = HOME_KINDS[(index + 1) % HOME_KINDS.len()];
        return true;
    }
    match field {
        SettingField::TrimTrailingWhitespace => {
            settings.trim_trailing_whitespace = settings.trim_trailing_whitespace.next();
        }
        SettingField::ActionsOnSaveScope => {
            settings.actions_on_save_reformat_scope =
                settings.actions_on_save_reformat_scope.next();
        }
        SettingField::UseTabs => settings.use_tabs = !settings.use_tabs,
        SettingField::CompletionCase => {
            settings.completion_case = settings.completion_case.next();
        }
        SettingField::InlayParameterNames => {
            settings.inlay_parameter_names = settings.inlay_parameter_names.next();
        }
        SettingField::GitUpdateMethod => {
            settings.git_update_method = settings.git_update_method.next();
        }
        SettingField::ThemeLight => settings.theme_light = next_theme(&settings.theme_light),
        SettingField::ThemeDark => settings.theme_dark = next_theme(&settings.theme_dark),
        SettingField::IconSet => {
            settings.icon_set = if settings.icon_set == INTELLIJ {
                ARRO.to_owned()
            } else {
                INTELLIJ.to_owned()
            };
        }
        SettingField::McpExecutePermission => {
            settings.mcp_execute_permission = settings.mcp_execute_permission.next();
        }
        SettingField::McpRouterOnlyScope => {
            settings.mcp_router_only_scope = settings.mcp_router_only_scope.next();
        }
        SettingField::SqlKeywordCase => {
            settings.sql_keyword_case = settings.sql_keyword_case.next();
        }
        _ => return false,
    }
    true
}

/// The values of a setting with named choices, in the order the window lists them. The
/// themes, their two system slots and the editor font are not here: the window knows
/// the themes and the families it can offer.
pub fn choices(id: &str) -> Vec<String> {
    let labels: &[&str] = match find(id).map(|setting| setting.field) {
        Some(SettingField::TrimTrailingWhitespace) => &["None", "Modified lines", "All"],
        Some(SettingField::ActionsOnSaveScope) => &["Whole file", "Changed lines"],
        Some(SettingField::UseTabs) => &["Spaces", "Tabs"],
        Some(SettingField::CompletionCase) => &["None", "First letter", "All"],
        Some(SettingField::InlayParameterNames) => &["None", "Literals", "All"],
        Some(SettingField::GitUpdateMethod) => &["Merge", "Rebase"],
        Some(SettingField::BuildToolFirstHomeKind | SettingField::BuildToolSecondHome) => {
            &["Wrapper", "Local installation", "Downloaded by Arro"]
        }
        Some(SettingField::McpExecutePermission) => &[
            "Always ask",
            "Allow for trusted projects",
            "Run without confirmation",
        ],
        Some(SettingField::McpRouterOnlyScope) => &["All agents", "ACP only"],
        Some(SettingField::SqlKeywordCase) => &["Upper case", "Lower case", "As is"],
        _ => return Vec::new(),
    };
    labels.iter().map(|label| (*label).to_owned()).collect()
}

/// The place of the value in force among [`choices`], zero for a setting without them.
pub fn chosen_choice(settings: &Settings, id: &str) -> usize {
    let Some(label) = choice_label(settings, id) else {
        return 0;
    };
    choices(id)
        .iter()
        .position(|value| *value == label)
        .unwrap_or(0)
}

/// Picks the value at `index` of [`choices`]; false when the setting has none.
pub fn set_choice(settings: &mut Settings, id: &str, index: usize) -> bool {
    let count = choices(id).len();
    if count == 0 {
        return false;
    }
    // A home list names its kind directly: an old file whose label already reads Wrapper
    // still has to become the explicit source the user picked.
    if let Some((tool, _)) = find(id).map(|setting| setting.field).and_then(home_choice) {
        let Some(kind) = HOME_KINDS.get(index).copied() else {
            return false;
        };
        settings.build_tools.tool_mut(tool).home_kind = kind;
        return true;
    }
    let steps = (index + count - chosen_choice(settings, id)) % count;
    for _ in 0..steps {
        if !next_choice(settings, id) {
            return false;
        }
    }
    true
}

/// The theme name after `name` in the list the settings offer.
fn next_theme(name: &str) -> String {
    let names = Theme::NAMES;
    let index = names.iter().position(|known| *known == name).unwrap_or(0);
    names[(index + 1) % names.len()].to_owned()
}

/// The program that gives an agent outside the window the tools of Arro: the path of the
/// settings, or `arro-mcp` next to the program of the window, which on macOS is the
/// `Resources` folder of the bundle.
pub fn mcp_binary(settings: &Settings) -> String {
    let configured = settings.mcp_binary.trim();
    if !configured.is_empty() {
        return configured.to_owned();
    }
    let program = std::env::current_exe().unwrap_or_default();
    let name = if cfg!(windows) {
        "arro-mcp.exe"
    } else {
        "arro-mcp"
    };
    platform::resources_dir(&program)
        .join(name)
        .display()
        .to_string()
}

/// The three ways to point an MCP client at the window, ready to paste into its own
/// settings. Arro itself runs none of them.
pub fn mcp_config(
    settings: &Settings,
    transport: mcp_discovery::Transport,
    url: Option<&str>,
    token: Option<&str>,
) -> String {
    let server = mcp_discovery::Server {
        command: mcp_binary(settings),
        url: url.map(str::to_owned),
        token: token.map(str::to_owned),
    };
    mcp_discovery::snippet(mcp_discovery::Client::ClaudeCode, transport, &server)
        .unwrap_or_else(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::{SettingKind, find, search, toggle, value};
    use workspace::Settings;

    #[test]
    fn a_part_of_a_name_finds_the_setting() {
        let font = search("font", 10);
        assert!(
            font.iter().any(|setting| setting.id == "font_size"),
            "the font size of the editor is one of the settings of the word: {font:?}"
        );
        assert!(
            search("wrap", 10)
                .iter()
                .any(|setting| setting.id == "soft_wrap")
        );
        assert!(
            search("spaces", 10)
                .iter()
                .any(|setting| setting.id == "show_whitespace"),
            "the extra words count too"
        );
        assert!(search("zzz", 10).is_empty());
        let every = super::all().len();
        assert_eq!(search("", every).len(), every);
    }

    #[test]
    fn the_search_keeps_the_pages_of_the_matching_settings_and_their_groups() {
        let pages = super::pages_matching("font");
        assert!(pages.contains(&super::SettingsPage::EditorFont));
        assert!(
            pages.contains(&super::SettingsPage::Editor),
            "the group above the page stays, so the tree keeps its shape"
        );
        assert!(
            !pages.contains(&super::SettingsPage::Project),
            "a page without a matching setting leaves the tree"
        );
        assert_eq!(
            super::page_matching("font size"),
            Some(super::SettingsPage::EditorFont),
            "the search opens the page of the setting, not the group above it"
        );
        let pages = super::pages_matching("tab size");
        assert!(pages.contains(&super::SettingsPage::EditorCodeStyle));
        assert!(
            pages.contains(&super::SettingsPage::Editor),
            "the group above the page stays, so the tree keeps its shape"
        );
        assert_eq!(
            super::pages_matching("").len(),
            super::SettingsPage::ALL.len()
        );
    }

    /// The tree draws pages in this order with the indent of their depth, so a page must
    /// follow its parent with only deeper pages between them, or it looks nested elsewhere.
    #[test]
    fn every_page_sits_inside_the_block_of_its_parent() {
        let all = super::SettingsPage::ALL;
        for (index, page) in all.iter().enumerate() {
            let Some(parent) = page.parent() else {
                assert_eq!(page.depth(), 0, "{} has no parent", page.title());
                continue;
            };
            assert_eq!(page.depth(), parent.depth() + 1, "{}", page.title());
            let start = all
                .iter()
                .position(|other| *other == parent)
                .expect("the parent is in the list");
            assert!(
                start < index,
                "{} comes after {}",
                page.title(),
                parent.title()
            );
            for between in &all[start + 1..index] {
                assert!(
                    between.depth() > parent.depth(),
                    "{} is drawn under {} instead of {}",
                    page.title(),
                    between.title(),
                    parent.title()
                );
            }
        }
    }

    #[test]
    fn the_switch_settings_are_the_boolean_ones() {
        let mut settings = Settings::default();
        for setting in super::all() {
            let before = value(&settings, setting.id);
            assert_eq!(
                before.is_some(),
                setting.kind == SettingKind::Toggle,
                "{}",
                setting.id
            );
            if let Some(before) = before {
                assert!(toggle(&mut settings, setting.id));
                assert_eq!(value(&settings, setting.id), Some(!before));
            } else {
                assert!(!toggle(&mut settings, setting.id));
            }
        }
        assert_eq!(
            find("font_size").map(|setting| setting.kind),
            Some(SettingKind::Number)
        );
    }

    #[test]
    fn the_distribution_choice_reads_the_old_folder() {
        let mut settings = Settings::default();
        assert_eq!(
            super::choice_label(&settings, "build_tool_second_home").as_deref(),
            Some("Wrapper"),
            "an empty value of an old file is the wrapper"
        );
        assert_eq!(super::choices("build_tool_second_home").len(), 3);

        settings.build_tools.second.home = "C:/tool-dist".into();
        assert_eq!(
            super::choice_label(&settings, "build_tool_second_home").as_deref(),
            Some("Local installation"),
            "a folder of an old file is a local installation"
        );

        settings.build_tools.second.home.clear();
        settings.build_tools.second.home_kind = project_model::ToolHome::Bundled;
        assert_eq!(
            super::choice_label(&settings, "build_tool_second_home").as_deref(),
            Some("Downloaded by Arro")
        );

        // A file with only a folder shows Local installation; picking the Wrapper of the
        // list has to store that choice even though the row already showed it.
        settings.build_tools.second.home = "C:/tool-dist".into();
        settings.build_tools.second.home_kind = project_model::ToolHome::Auto;
        assert!(super::set_choice(
            &mut settings,
            "build_tool_second_home",
            0
        ));
        assert_eq!(
            settings.build_tools.second.home_kind,
            project_model::ToolHome::Wrapper
        );
        assert!(
            !settings.build_tools.second.home.is_empty(),
            "the folder stays for the next local installation"
        );
    }

    #[test]
    fn the_wrapper_download_switch_is_on_by_default() {
        let mut settings = Settings::default();
        assert_eq!(
            value(&settings, "build_tool_second_wrapper_download"),
            Some(true)
        );
        assert!(toggle(&mut settings, "build_tool_second_wrapper_download"));
        assert_eq!(
            value(&settings, "build_tool_second_wrapper_download"),
            Some(false)
        );
        assert!(!settings.build_tools.second.download_wrapper);
    }

    #[test]
    fn the_appearance_page_marks_the_theme_the_window_shows() {
        let mut settings = Settings {
            theme_sync_with_os: false,
            theme: "mocha".to_owned(),
            theme_light: "paper".to_owned(),
            theme_dark: "nord".to_owned(),
            ..Settings::default()
        };
        assert_eq!(super::chosen_theme(&settings, true, "dark"), "mocha");
        assert_eq!(super::chosen_theme(&settings, false, "dark"), "mocha");

        // A file written before themes became files names no theme: the look of the
        // window is the one the page marks.
        settings.theme.clear();
        assert_eq!(super::chosen_theme(&settings, true, "dark"), "dark");
        assert_eq!(super::chosen_theme(&settings, false, "light"), "light");

        // Sync with OS marks the theme of each system look instead.
        settings.theme = "mocha".to_owned();
        settings.theme_sync_with_os = true;
        assert_eq!(super::chosen_theme(&settings, true, "mocha"), "nord");
        assert_eq!(super::chosen_theme(&settings, false, "mocha"), "paper");
    }

    #[test]
    fn the_memory_check_is_a_switch_and_an_off_check_takes_no_memory_as_low() {
        // Without a choice the check follows the machine: on, except on a virtual machine.
        let machine = ::platform::is_virtual_machine();
        let mut settings = Settings::default();
        assert_eq!(value(&settings, "memory_low_check"), Some(!machine));
        assert!(toggle(&mut settings, "memory_low_check"));
        assert_eq!(value(&settings, "memory_low_check"), Some(machine));
        assert_eq!(
            settings.memory_low_check,
            Some(machine),
            "the switch keeps the explicit choice"
        );
        let thresholds = if machine {
            platform::MemoryThresholds::new(5., 512)
        } else {
            platform::MemoryThresholds::new(0., 0)
        };
        assert_eq!(
            settings.memory_thresholds(),
            thresholds,
            "a switch off takes no memory as low, so no server stops for memory"
        );
        assert_eq!(
            super::number_label(&settings, "memory_low_megabytes").as_deref(),
            Some("512 MB")
        );
    }
}
