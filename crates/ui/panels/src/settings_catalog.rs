//! The settings of the window in one list: the name, the page and the kind of each.
//! The Settings window and Search Everywhere build their rows from here, so a new
//! setting shows up in both places at once.

use std::sync::OnceLock;

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
    pub const ALL: [SettingsPage; 25] = [
        SettingsPage::Appearance,
        SettingsPage::Editor,
        SettingsPage::EditorCodeStyle,
        SettingsPage::EditorCompletion,
        SettingsPage::EditorInlayHints,
        SettingsPage::EditorInspections,
        SettingsPage::EditorSpelling,
        SettingsPage::EditorLiveTemplates,
        SettingsPage::EditorPostfix,
        SettingsPage::EditorFileTemplates,
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
        SettingsPage::HttpClient,
        SettingsPage::Plugins,
        SettingsPage::Keymap,
        SettingsPage::Database,
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
            | SettingsPage::EditorFileTemplates => 1,
            SettingsPage::EditorPostfix => 2,
            _ => 0,
        }
    }

    /// The page a click on this row opens: a group opens its first part.
    pub fn target(self) -> SettingsPage {
        match self {
            SettingsPage::BuildTools => SettingsPage::BuildToolsFirst,
            SettingsPage::VersionControl => SettingsPage::Git,
            SettingsPage::Tools => SettingsPage::Terminal,
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
            | SettingsPage::EditorPostfix
            | SettingsPage::EditorFileTemplates => Some(SettingsPage::Editor),
            SettingsPage::BuildToolsFirst | SettingsPage::BuildToolsSecond => {
                Some(SettingsPage::BuildTools)
            }
            SettingsPage::Git => Some(SettingsPage::VersionControl),
            SettingsPage::ActionsOnSave
            | SettingsPage::Terminal
            | SettingsPage::Kubernetes
            | SettingsPage::Database => Some(SettingsPage::Tools),
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
    Theme,
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
    BuildToolSecondHome,
    BuildToolSecondOffline,
    BuildToolSecondUserHome,
    BuildToolSecondRuntimeHome,
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
        F::BuildToolSecondHome => (1, P::Home),
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
        id: "theme",
        title: "Theme",
        page: SettingsPage::Appearance,
        kind: SettingKind::Choice,
        field: SettingField::Theme,
        keywords: "dark light colors appearance",
    },
    Setting {
        id: "font_size",
        title: "Editor font size",
        page: SettingsPage::Appearance,
        kind: SettingKind::Number,
        field: SettingField::FontSize,
        keywords: "text size zoom appearance",
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
        page: SettingsPage::Appearance,
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
        title: "Editor typeface",
        page: SettingsPage::Appearance,
        kind: SettingKind::Text,
        field: SettingField::EditorFontFamily,
        keywords: "typeface family monospace text appearance",
    },
    Setting {
        id: "line_spacing",
        title: "Line spacing",
        page: SettingsPage::Appearance,
        kind: SettingKind::Number,
        field: SettingField::LineSpacing,
        keywords: "line height spacing text appearance",
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
        kind: SettingKind::Text,
        field: SettingField::BuildToolFirstHome,
        keywords: "home install location build tools wrapper",
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
        kind: SettingKind::Text,
        field: SettingField::BuildToolSecondHome,
        keywords: "distribution wrapper install location build tools",
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

impl Setting {
    /// Every word of `query` is in the name, the page or the extra words, case ignored.
    pub fn matches(&self, query: &str) -> bool {
        let text = format!("{} {} {}", self.title, self.page.title(), self.keywords).to_lowercase();
        query
            .split_whitespace()
            .all(|word| text.contains(&word.to_lowercase()))
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

/// The switch of a boolean setting; `None` for a setting that is not a switch.
pub fn value(settings: &Settings, id: &str) -> Option<bool> {
    match find(id)?.field {
        SettingField::SoftWrap => Some(settings.soft_wrap),
        SettingField::ShowWhitespace => Some(settings.show_whitespace),
        SettingField::ShowLineNumbers => Some(settings.show_line_numbers),
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
        SettingField::BuildToolFirstUpdateSnapshots => {
            Some(settings.build_tools.first.update_snapshots)
        }
        SettingField::BuildToolSecondOffline => Some(settings.build_tools.second.offline),
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
        SettingField::Theme
        | SettingField::FontSize
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
        | SettingField::BuildToolFirstSettingsFile
        | SettingField::BuildToolFirstRepository
        | SettingField::BuildToolFirstVmOptions
        | SettingField::BuildToolSecondHome
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
        | SettingField::DatabasePageRows => None,
    }
}

/// Flips a boolean setting. False when the id is not a switch.
pub fn toggle(settings: &mut Settings, id: &str) -> bool {
    match find(id).map(|setting| setting.field) {
        Some(SettingField::SoftWrap) => settings.soft_wrap = !settings.soft_wrap,
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
        Some(SettingField::BuildToolSecondOffline) => {
            settings.build_tools.second.offline = !settings.build_tools.second.offline;
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
            SettingField::Theme
            | SettingField::FontSize
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
            | SettingField::BuildToolFirstSettingsFile
            | SettingField::BuildToolFirstRepository
            | SettingField::BuildToolFirstVmOptions
            | SettingField::BuildToolSecondHome
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
            | SettingField::DatabasePageRows,
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
        _ => return None,
    })
}

/// The chosen name of a setting with named choices, for the window to show.
pub fn choice_label(settings: &Settings, id: &str) -> Option<&'static str> {
    match find(id)?.field {
        SettingField::TrimTrailingWhitespace => Some(settings.trim_trailing_whitespace.label()),
        SettingField::ActionsOnSaveScope => Some(settings.actions_on_save_reformat_scope.label()),
        SettingField::UseTabs => Some(if settings.use_tabs { "Tabs" } else { "Spaces" }),
        SettingField::CompletionCase => Some(settings.completion_case.label()),
        SettingField::InlayParameterNames => Some(settings.inlay_parameter_names.label()),
        SettingField::GitUpdateMethod => Some(settings.git_update_method.label()),
        _ => None,
    }
}

/// The next choice of a setting with named choices. False when it has none.
pub fn next_choice(settings: &mut Settings, id: &str) -> bool {
    match find(id).map(|setting| setting.field) {
        Some(SettingField::TrimTrailingWhitespace) => {
            settings.trim_trailing_whitespace = settings.trim_trailing_whitespace.next();
        }
        Some(SettingField::ActionsOnSaveScope) => {
            settings.actions_on_save_reformat_scope =
                settings.actions_on_save_reformat_scope.next();
        }
        Some(SettingField::UseTabs) => settings.use_tabs = !settings.use_tabs,
        Some(SettingField::CompletionCase) => {
            settings.completion_case = settings.completion_case.next();
        }
        Some(SettingField::InlayParameterNames) => {
            settings.inlay_parameter_names = settings.inlay_parameter_names.next();
        }
        Some(SettingField::GitUpdateMethod) => {
            settings.git_update_method = settings.git_update_method.next();
        }
        _ => return false,
    }
    true
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
        assert_eq!(search("", 100).len(), super::all().len());
    }

    #[test]
    fn the_search_keeps_the_pages_of_the_matching_settings_and_their_groups() {
        let pages = super::pages_matching("font");
        assert!(pages.contains(&super::SettingsPage::Appearance));
        assert!(
            !pages.contains(&super::SettingsPage::Project),
            "a page without a matching setting leaves the tree"
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
}
