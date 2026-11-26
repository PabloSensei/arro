//! What the user can change in Settings. One JSON file in the settings folder; a missing
//! or damaged file gives the defaults, a new key an old file does not know gets its default.

use std::collections::BTreeMap;
use std::path::Path;

use project_model::BuildTools;
use serde::{Deserialize, Serialize};
use ui_kit::*;

use crate::layout::DockState;
use crate::settings_file;

pub const SETTINGS_FILE: &str = "settings.json";
pub const MIN_FONT_SIZE: f32 = 8.;
pub const MAX_FONT_SIZE: f32 = 32.;
const DEFAULT_FONT_SIZE: f32 = 13.;
const DEFAULT_MEMORY_LOW_PERCENT: f32 = 5.;
const DEFAULT_MEMORY_LOW_MEGABYTES: u32 = 512;
/// The step of the memory floor in the Settings window, in megabytes.
pub const MEMORY_LOW_STEP_MB: u32 = 64;
/// The largest memory floor the settings may hold, in megabytes.
pub const MAX_MEMORY_LOW_MEGABYTES: u32 = 1024 * 1024;
/// The smallest heap the language server takes when a heap is set; a smaller one stops
/// the server from starting.
pub const MIN_LANGUAGE_MAX_HEAP_MB: u32 = 256;
/// The largest heap the language server takes when a heap is set, in megabytes.
pub const MAX_LANGUAGE_MAX_HEAP_MB: u32 = 65_536;
/// The reach of Alt+Shift+= and Alt+Shift+-.
pub const MIN_UI_SCALE: f32 = 0.8;
pub const MAX_UI_SCALE: f32 = 2.;
pub const UI_SCALE_STEP: f32 = 0.1;
const DEFAULT_UI_SCALE: f32 = 1.;
/// The distance between two lines of the editor, as a share of the font size.
pub const MIN_LINE_SPACING: f32 = 1.;
pub const MAX_LINE_SPACING: f32 = 2.;
pub const LINE_SPACING_STEP: f32 = 0.1;
const DEFAULT_LINE_SPACING: f32 = 1.;
/// The widest tab or indentation step the Code Style page takes.
pub const MAX_INDENT_SIZE: f32 = 16.;
/// The longest wait before the completion list opens on its own, in milliseconds.
pub const MAX_COMPLETION_DELAY_MS: u32 = 5000;
/// The most unchanged lines the diff keeps around a change.
pub const MAX_DIFF_CONTEXT_LINES: u32 = 100;
/// The column of the right margin: the width of the centred text of Distraction Free
/// Mode, in characters.
pub const DEFAULT_RIGHT_MARGIN: u32 = 120;
pub const MIN_RIGHT_MARGIN: u32 = 20;
pub const MAX_RIGHT_MARGIN: u32 = 500;
pub const RIGHT_MARGIN_STEP: u32 = 10;
/// How long an HTTP request may take before it is stopped.
pub const DEFAULT_HTTP_TIMEOUT_SECONDS: u32 = 60;
/// The longest timeout the Settings window allows.
pub const MAX_HTTP_TIMEOUT_SECONDS: u32 = 600;
pub const COMPLETION_DELAY_STEP_MS: u32 = 100;
/// The rows one page of a data tab reads when the user has not changed the setting.
pub const DEFAULT_DATABASE_PAGE_ROWS: u32 = 500;
/// The most rows one page of a data tab reads.
pub const MAX_DATABASE_PAGE_ROWS: u32 = 10_000;
/// The masks a new installation leaves out of the project tree and the file index.
pub const DEFAULT_IGNORED_FILES: &[&str] = &[".git", "node_modules", "*.class"];

/// What a save does with the spaces and tabs at the end of a line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrimTrailingWhitespace {
    /// Keep every line as it is.
    #[default]
    None,
    /// Only the lines changed since the file was read or saved.
    Modified,
    /// Every line of the file.
    All,
}

impl TrimTrailingWhitespace {
    pub fn label(self) -> &'static str {
        match self {
            TrimTrailingWhitespace::None => "None",
            TrimTrailingWhitespace::Modified => "Modified lines",
            TrimTrailingWhitespace::All => "All",
        }
    }

    pub fn next(self) -> Self {
        match self {
            TrimTrailingWhitespace::None => TrimTrailingWhitespace::Modified,
            TrimTrailingWhitespace::Modified => TrimTrailingWhitespace::All,
            TrimTrailingWhitespace::All => TrimTrailingWhitespace::None,
        }
    }
}

/// What the diff leaves out of the comparison of two lines.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffWhitespace {
    /// Every character counts.
    #[default]
    DoNotIgnore,
    /// The spaces at both ends of a line do not count.
    Trim,
    /// No whitespace inside a line counts either.
    Ignore,
    /// No whitespace counts, and lines that hold nothing but whitespace do not count.
    IgnoreAndEmptyLines,
}

impl DiffWhitespace {
    pub fn label(self) -> &'static str {
        match self {
            DiffWhitespace::DoNotIgnore => "Do not ignore",
            DiffWhitespace::Trim => "Trim whitespaces",
            DiffWhitespace::Ignore => "Ignore whitespaces",
            DiffWhitespace::IgnoreAndEmptyLines => "Ignore whitespaces and empty lines",
        }
    }

    pub fn next(self) -> Self {
        match self {
            DiffWhitespace::DoNotIgnore => DiffWhitespace::Trim,
            DiffWhitespace::Trim => DiffWhitespace::Ignore,
            DiffWhitespace::Ignore => DiffWhitespace::IgnoreAndEmptyLines,
            DiffWhitespace::IgnoreAndEmptyLines => DiffWhitespace::DoNotIgnore,
        }
    }
}

/// What a diff marks inside the lines it shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffHighlighting {
    /// Only whole lines.
    ByLine,
    /// The words that differ inside a changed line.
    #[default]
    ByWord,
    /// The words that differ, with a changed line split into its removed and added parts.
    SplitChanges,
    /// The characters that differ.
    ByCharacter,
    /// Nothing.
    None,
}

impl DiffHighlighting {
    pub fn label(self) -> &'static str {
        match self {
            DiffHighlighting::ByLine => "By line",
            DiffHighlighting::ByWord => "By word",
            DiffHighlighting::SplitChanges => "Split changes",
            DiffHighlighting::ByCharacter => "By character",
            DiffHighlighting::None => "Do not highlight",
        }
    }

    pub fn next(self) -> Self {
        match self {
            DiffHighlighting::ByLine => DiffHighlighting::ByWord,
            DiffHighlighting::ByWord => DiffHighlighting::SplitChanges,
            DiffHighlighting::SplitChanges => DiffHighlighting::ByCharacter,
            DiffHighlighting::ByCharacter => DiffHighlighting::None,
            DiffHighlighting::None => DiffHighlighting::ByLine,
        }
    }
}

/// The part of a file Reformat Code changes on save.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReformatScope {
    /// Every line of the file.
    #[default]
    WholeFile,
    /// Only the lines that differ from the last commit, as the gutter marks them.
    ChangedLines,
}

impl ReformatScope {
    pub fn label(self) -> &'static str {
        match self {
            ReformatScope::WholeFile => "Whole file",
            ReformatScope::ChangedLines => "Changed lines",
        }
    }

    pub fn next(self) -> Self {
        match self {
            ReformatScope::WholeFile => ReformatScope::ChangedLines,
            ReformatScope::ChangedLines => ReformatScope::WholeFile,
        }
    }
}

/// How the completion list compares the typed prefix with its entries.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionCase {
    /// The case of the letters does not matter.
    #[default]
    None,
    /// Only the first letter must match in case.
    FirstLetter,
    /// Every letter must match in case.
    All,
}

impl CompletionCase {
    pub fn label(self) -> &'static str {
        match self {
            CompletionCase::None => "None",
            CompletionCase::FirstLetter => "First letter",
            CompletionCase::All => "All",
        }
    }

    pub fn next(self) -> Self {
        match self {
            CompletionCase::None => CompletionCase::FirstLetter,
            CompletionCase::FirstLetter => CompletionCase::All,
            CompletionCase::All => CompletionCase::None,
        }
    }
}

/// The case Reformat Code gives the keywords of SQL.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SqlKeywordCase {
    /// `SELECT`, `FROM`, `WHERE`.
    #[default]
    Upper,
    /// `select`, `from`, `where`.
    Lower,
    /// The case the statement was typed in.
    Keep,
}

impl SqlKeywordCase {
    pub fn label(self) -> &'static str {
        match self {
            SqlKeywordCase::Upper => "Upper case",
            SqlKeywordCase::Lower => "Lower case",
            SqlKeywordCase::Keep => "As is",
        }
    }

    pub fn next(self) -> Self {
        match self {
            SqlKeywordCase::Upper => SqlKeywordCase::Lower,
            SqlKeywordCase::Lower => SqlKeywordCase::Keep,
            SqlKeywordCase::Keep => SqlKeywordCase::Upper,
        }
    }
}

/// The tool windows of the layout the user stores as their default: which are open and
/// how wide they are.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct DefaultLayout {
    pub left: DockState,
    pub right: DockState,
    pub bottom: DockState,
}

/// Which parameter names the editor shows as hints: none, the ones before a literal or
/// another argument whose meaning the name explains, or every one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InlayParameterNames {
    None,
    #[default]
    Literals,
    All,
}

impl InlayParameterNames {
    pub fn label(self) -> &'static str {
        match self {
            InlayParameterNames::None => "None",
            InlayParameterNames::Literals => "Literals",
            InlayParameterNames::All => "All",
        }
    }

    pub fn next(self) -> Self {
        match self {
            InlayParameterNames::None => InlayParameterNames::Literals,
            InlayParameterNames::Literals => InlayParameterNames::All,
            InlayParameterNames::All => InlayParameterNames::None,
        }
    }
}

/// How Update Project brings the upstream of the current branch in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitUpdateMethod {
    /// A merge commit joins the two histories.
    #[default]
    Merge,
    /// The local commits move on top of the upstream.
    Rebase,
}

/// Whether a tool of the MCP server may build, test and run the project without a
/// question first.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpExecutePermission {
    /// Every call of the class `Execute` asks the user in the window.
    #[default]
    AlwaysAsk,
    /// A project the user trusted runs without a question; another one still asks.
    TrustedProjects,
    /// Every call of the class `Execute` runs without a question, as brave mode of IDEA.
    WithoutConfirmation,
}

impl McpExecutePermission {
    pub fn label(self) -> &'static str {
        match self {
            Self::AlwaysAsk => "Always ask",
            Self::TrustedProjects => "Allow for trusted projects",
            Self::WithoutConfirmation => "Run without confirmation",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::AlwaysAsk => Self::TrustedProjects,
            Self::TrustedProjects => Self::WithoutConfirmation,
            Self::WithoutConfirmation => Self::AlwaysAsk,
        }
    }
}

/// The clients the router-only mode leaves with `execute_tool` alone.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpRouterScope {
    /// Every agent and every client outside the window.
    #[default]
    AllAgents,
    /// Only the sessions of the ACP agents.
    AcpOnly,
}

impl McpRouterScope {
    pub fn label(self) -> &'static str {
        match self {
            Self::AllAgents => "All agents",
            Self::AcpOnly => "ACP only",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::AllAgents => Self::AcpOnly,
            Self::AcpOnly => Self::AllAgents,
        }
    }
}

/// The two switches of one row of the Exposed Tools table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpToolChoice {
    pub enabled: bool,
    pub router_only: bool,
}

impl GitUpdateMethod {
    pub fn label(self) -> &'static str {
        match self {
            GitUpdateMethod::Merge => "Merge",
            GitUpdateMethod::Rebase => "Rebase",
        }
    }

    pub fn next(self) -> Self {
        match self {
            GitUpdateMethod::Merge => GitUpdateMethod::Rebase,
            GitUpdateMethod::Rebase => GitUpdateMethod::Merge,
        }
    }
}

/// How the blame column names the author of a line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnnotateNames {
    /// The family name alone, the default of IDEA.
    #[default]
    LastName,
    FirstName,
    FullName,
    /// The initials of the first and the last name.
    Initials,
    Email,
}

impl AnnotateNames {
    pub fn label(self) -> &'static str {
        match self {
            AnnotateNames::LastName => "Last Name",
            AnnotateNames::FirstName => "First Name",
            AnnotateNames::FullName => "Full Name",
            AnnotateNames::Initials => "Initials",
            AnnotateNames::Email => "Email",
        }
    }

    /// The name of the author as the column writes it.
    pub fn format(self, author: &str, email: &str) -> String {
        let parts: Vec<&str> = author.split_whitespace().collect();
        let first = parts.first().copied().unwrap_or_default();
        let last = parts.last().copied().unwrap_or_default();
        match self {
            AnnotateNames::LastName => last.to_owned(),
            AnnotateNames::FirstName => first.to_owned(),
            AnnotateNames::FullName => parts.join(" "),
            AnnotateNames::Initials if first == last => {
                format!("{}.", first.chars().next().unwrap_or_default())
            }
            AnnotateNames::Initials => format!(
                "{}.{}.",
                first.chars().next().unwrap_or_default(),
                last.chars().next().unwrap_or_default()
            ),
            AnnotateNames::Email => email.to_owned(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// The theme follows the appearance of the system.
    pub theme_sync_with_os: bool,
    /// The theme the window uses while the system is light; the id of a theme of the
    /// catalog.
    pub theme_light: String,
    /// The theme the window uses while the system is dark; the id of a theme of the
    /// catalog.
    pub theme_dark: String,
    /// The theme of a window that does not follow the system; the id of any theme of the
    /// catalog. Empty in a file written before themes became files: the window then takes
    /// the base theme of the look.
    pub theme: String,
    /// The set of icons of the window: `arro`, `intellij`, or the name of a folder of the
    /// `icons` folder of the settings.
    pub icon_set: String,
    /// Size of the text in the editor, in points.
    pub font_size: f32,
    /// Size of the text of the editors in Presentation Mode, in points.
    pub presentation_font_size: f32,
    /// Scale of the whole interface, 1 for the regular size.
    pub ui_scale: f32,
    /// Family of the editor text; `JetBrains Mono` by default.
    pub editor_font_family: String,
    /// Distance between two editor lines, as a share of the font size, from 1 to 2.
    pub line_spacing: f32,
    /// Long lines wrap at the edge of the editor, in every editor of the window.
    pub soft_wrap: bool,
    /// Spaces and tabs show as marks, in every editor of the window.
    pub show_whitespace: bool,
    /// The line numbers in the gutter of the editor.
    pub show_line_numbers: bool,
    /// The icons of the gutter: breakpoints, run marks, bookmarks and the fold arrows.
    pub show_gutter_icons: bool,
    /// The background of the line with the caret.
    pub highlight_current_line: bool,
    /// The values of the variables of the stopped frame after the lines that name them.
    pub debug_show_values_inline: bool,
    /// The frames of libraries are folded into one row of the stack.
    pub debug_hide_library_frames: bool,
    /// How values of some types are shown, one `type = display | children` rule per
    /// semicolon.
    pub debug_type_renderers: String,
    /// The thin vertical lines at the steps of the indentation.
    pub show_indent_guides: bool,
    /// The declarations around the caret above the editor text.
    pub show_breadcrumbs: bool,
    /// The declarations that the top of the editor holds while the caret is in their body.
    pub show_sticky_lines: bool,
    /// The caret blinks while the editor has the focus.
    pub caret_blink: bool,
    /// What a save does with the spaces at the end of a line.
    pub trim_trailing_whitespace: TrimTrailingWhitespace,
    /// A save puts a line break after the last line when the file lacks one.
    pub ensure_final_newline: bool,
    /// A save formats the file through the language.
    pub actions_on_save_reformat: bool,
    /// What that formatting changes: the whole file or the lines of the last commit.
    pub actions_on_save_reformat_scope: ReformatScope,
    /// The files actions on save touch, as masks; `*` is every file.
    pub actions_on_save_reformat_files: String,
    /// A save organizes the imports through the language.
    pub actions_on_save_optimize_imports: bool,
    /// A save runs the safe fixes of the language over the file.
    pub actions_on_save_cleanup: bool,
    /// Ctrl+wheel changes the font size of the editor.
    pub zoom_with_wheel: bool,
    /// Columns a tab takes on screen; also the default step of the indentation.
    pub tab_size: u32,
    /// Columns of one indentation step in a new file.
    pub indent_size: u32,
    /// A new file indents with tabs rather than spaces.
    pub use_tabs: bool,
    /// How the completion list compares the typed prefix with its entries.
    pub completion_case: CompletionCase,
    /// The completion list opens while the user types.
    pub completion_auto_popup: bool,
    /// The wait before that list opens, in milliseconds.
    pub completion_delay_ms: u32,
    /// Which parameter names the language shows in the text.
    pub inlay_parameter_names: InlayParameterNames,
    /// The type hints of the language in the text.
    pub inlay_type_hints: bool,
    /// The number of usages of a declaration above it.
    pub code_vision_usages: bool,
    /// The implementations or inheritors of a declaration above it.
    pub code_vision_implementations: bool,
    /// Documentation comments show as rendered text in the editor.
    pub render_doc_comments: bool,
    /// The preview of a markdown file takes the pictures of an address of the internet.
    pub markdown_images_from_internet: bool,
    /// Unsaved files are written when the window loses the focus or the tab changes.
    pub autosave: bool,
    /// The first open of a project shows its README, rendered when it is markdown.
    pub open_readme_on_first_open: bool,
    /// The home of the runtime that runs the project; empty means the one found by the
    /// environment.
    pub runtime_home: String,
    /// Where Capture Memory Snapshot writes its files; empty means `.arro/snapshots` of
    /// the project. A relative folder is relative to the project root.
    pub heap_snapshot_folder: String,
    /// Memory below this share of the total and below `memory_low_megabytes` stops a
    /// language server so that the machine does not run out. Zero turns the stop off.
    pub memory_low_percent: f32,
    /// The absolute floor of the same check, in megabytes.
    pub memory_low_megabytes: u32,
    /// Whether the memory check stops a language server. Unset, the check is on, except on
    /// a virtual machine, whose host adds memory on demand; a choice of the user always wins.
    pub memory_low_check: Option<bool>,
    /// The most heap the language server takes, in megabytes, from its next start. Zero
    /// lets the language server choose it from the size of the machine.
    pub language_max_heap_megabytes: u32,
    /// Reformat File dialog: organize the imports as well.
    pub reformat_optimize_imports: bool,
    /// Reformat File dialog: only the lines changed since the last commit.
    pub reformat_only_changed: bool,
    /// Reformat File dialog: the selection rather than the whole file.
    pub reformat_selection: bool,
    /// The grouping rules of the Usages window.
    pub usage_group_usage_type: bool,
    pub usage_group_module: bool,
    pub usage_group_directory: bool,
    pub usage_group_directory_structure: bool,
    pub usage_group_file_structure: bool,
    /// Modules are one group instead of a tree of their parts.
    pub usage_flatten_modules: bool,
    /// The usage filters of the Usages window.
    pub usage_show_read: bool,
    pub usage_show_write: bool,
    pub usage_show_imports: bool,
    /// The grouping rules of the TODO window.
    pub todo_group_packages: bool,
    pub todo_group_modules: bool,
    pub todo_flatten_packages: bool,
    /// The patterns the TODO window looks for, in the order its filter offers them.
    pub todo_templates: Vec<String>,
    /// The TODO window matches the patterns with their case.
    pub todo_case_sensitive: bool,
    /// Looks for a newer release once a day after the window opens.
    pub updates_check_automatically: bool,
    /// The tool window layout Restore Default Layout goes back to; `None` is the layout
    /// of a new window.
    pub default_layout: Option<DefaultLayout>,
    /// The column of the right margin, which also limits the width of the text of
    /// Distraction Free Mode.
    pub right_margin: u32,
    /// The parts of the window that View | Appearance shows.
    pub toolbar_visible: bool,
    pub tool_window_bars_visible: bool,
    pub status_bar_visible: bool,
    pub navigation_bar_visible: bool,
    /// The language server's semantic tokens colour the names over the syntax colours.
    pub semantic_highlighting: bool,
    /// The documentation popup opens under the pointer after a delay.
    pub quick_doc_on_hover: bool,
    /// How the build tools of the project start, in the order of their pages.
    pub build_tools: BuildTools,
    /// The sources of a class of a library are fetched as soon as the class opens.
    pub download_sources_automatically: bool,
    /// A build file shows the newer versions of its dependencies, and offers to update
    /// them, when the repositories of the project can be asked.
    pub check_dependency_updates: bool,
    /// The program that runs Git; empty means the one on `PATH`.
    pub git_path: String,
    /// How Update Project brings the upstream in.
    pub git_update_method: GitUpdateMethod,
    /// A push the remote rejected fetches, updates and pushes again.
    pub git_auto_update_on_reject: bool,
    /// Update Project initializes and updates the submodules of the project.
    pub git_update_submodules: bool,
    /// A commit whose files carry CRLF line separators warns first.
    pub git_warn_crlf: bool,
    /// The commit window starts with code analysis.
    pub git_analyze_before_commit: bool,
    /// The blame column shows the author of a line.
    pub annotate_author: bool,
    /// The blame column shows the date of the commit of a line.
    pub annotate_date: bool,
    /// The blame column shows the short hash of the commit of a line.
    pub annotate_commit: bool,
    /// How the blame column names the author.
    pub annotate_names: AnnotateNames,
    /// The shell of a new terminal; empty means the one found on this machine.
    pub terminal_shell: String,
    /// Where a new terminal starts; empty means the root of the project.
    pub terminal_start_directory: String,
    /// Variables of a new terminal, one `NAME=value` per line.
    pub terminal_environment: String,
    /// A paste of more than one line asks before it reaches the shell.
    pub terminal_confirm_multiline_paste: bool,
    /// The diff shows the two versions side by side rather than as one list.
    pub diff_side_by_side: bool,
    /// What the diff leaves out of the comparison of two lines.
    pub diff_whitespace: DiffWhitespace,
    /// What the diff marks inside the lines it shows.
    pub diff_highlighting: DiffHighlighting,
    /// A scroll of one pane of the diff moves the other one.
    pub diff_synchronize_scrolling: bool,
    /// The diff shows the numbers of the lines in the gutter.
    pub diff_show_line_numbers: bool,
    /// Long lines of the diff wrap at the edge of the pane.
    pub diff_soft_wrap: bool,
    /// Unchanged lines around a change the diff keeps in sight, on each side.
    pub diff_context_lines: u32,
    /// The diff folds the unchanged lines away.
    pub diff_collapse_unchanged: bool,
    /// The branch popup groups local and remote branches by the folders of their names.
    pub git_group_by_directory: bool,
    /// How long an HTTP request may take before it is stopped, in seconds.
    pub http_timeout_seconds: u32,
    /// The program that talks to Kubernetes; empty means the one on `PATH`.
    pub kubectl_path: String,
    /// The kubeconfig the Kubernetes tool window reads; empty means `KUBECONFIG` or the
    /// usual file of the user.
    pub kubeconfig_path: String,
    /// How many rows one page of a data tab reads from the table.
    pub database_page_rows: u32,
    /// The case Reformat Code gives the keywords of SQL.
    pub sql_keyword_case: SqlKeywordCase,
    /// Run SQL Script stops at the first statement that fails.
    pub sql_script_stop_on_error: bool,
    /// The project tree selects the file of every tab that becomes active, as the Always
    /// Select Opened File switch of its view options says.
    pub project_always_select_opened_file: bool,
    /// The project tree shows a chain of folders that have one subfolder each as one row,
    /// `com.example.demo` below a source root and `a/b/c` elsewhere, as the Compact Middle
    /// Packages switch of its view options says.
    pub project_compact_middle_packages: bool,
    /// The file and folder masks the project tree and the file index leave out, such as
    /// `*.class`, `.git` and `node_modules`.
    pub ignored_files: Vec<String>,
    /// The AI features of the window are on: Ask AI, Fix with AI and Explain with AI
    /// offer the problem at hand to the agent. The AI Sessions window itself stays.
    pub ai_enabled: bool,
    /// The window shares its MCP server with the agents outside it while the project is
    /// open. The sessions of AI Sessions work either way.
    pub mcp_enabled: bool,
    /// The program a client outside the window starts; empty means `arro-mcp` next to the
    /// program of the window.
    pub mcp_binary: String,
    /// Whether a tool that builds, tests or runs the project asks first.
    pub mcp_execute_permission: McpExecutePermission,
    /// The Exposed Tools table: the tools the user changed from the state they start with.
    pub mcp_tool_settings: BTreeMap<String, McpToolChoice>,
    /// `execute_tool` is the only tool a client sees, reached by name through it.
    pub mcp_router_only_mode: bool,
    /// The clients the router-only mode covers.
    pub mcp_router_only_scope: McpRouterScope,
    /// A terminal of the window suggests the setup of the MCP client when `claude` or
    /// `codex` starts in it without an entry of Arro.
    pub mcp_terminal_suggestions: bool,
    /// Escape in the terminal moves the focus to the editor and keeps the terminal open. Off,
    /// the shell gets Escape, which vim and less need.
    pub terminal_escape_focus: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme_sync_with_os: true,
            theme_light: "light".to_owned(),
            theme_dark: "dark".to_owned(),
            theme: String::new(),
            icon_set: "arro".to_owned(),
            font_size: DEFAULT_FONT_SIZE,
            presentation_font_size: crate::appearance::DEFAULT_PRESENTATION_FONT_SIZE,
            ui_scale: DEFAULT_UI_SCALE,
            editor_font_family: "JetBrains Mono".to_owned(),
            line_spacing: DEFAULT_LINE_SPACING,
            soft_wrap: false,
            show_whitespace: false,
            show_line_numbers: true,
            show_gutter_icons: true,
            highlight_current_line: true,
            debug_show_values_inline: true,
            debug_hide_library_frames: true,
            debug_type_renderers: String::new(),
            show_indent_guides: true,
            show_breadcrumbs: true,
            show_sticky_lines: true,
            caret_blink: true,
            trim_trailing_whitespace: TrimTrailingWhitespace::None,
            ensure_final_newline: false,
            actions_on_save_reformat: false,
            actions_on_save_reformat_scope: ReformatScope::WholeFile,
            actions_on_save_reformat_files: "*".to_owned(),
            actions_on_save_optimize_imports: false,
            actions_on_save_cleanup: false,
            zoom_with_wheel: true,
            tab_size: 4,
            indent_size: 4,
            use_tabs: false,
            completion_case: CompletionCase::None,
            completion_auto_popup: true,
            completion_delay_ms: 0,
            inlay_parameter_names: InlayParameterNames::Literals,
            inlay_type_hints: false,
            code_vision_usages: true,
            code_vision_implementations: true,
            render_doc_comments: false,
            markdown_images_from_internet: true,
            autosave: true,
            open_readme_on_first_open: true,
            runtime_home: String::new(),
            heap_snapshot_folder: String::new(),
            memory_low_percent: DEFAULT_MEMORY_LOW_PERCENT,
            memory_low_megabytes: DEFAULT_MEMORY_LOW_MEGABYTES,
            memory_low_check: None,
            language_max_heap_megabytes: 0,
            reformat_optimize_imports: false,
            reformat_only_changed: false,
            reformat_selection: false,
            usage_group_usage_type: false,
            usage_group_module: false,
            usage_group_directory: false,
            usage_group_directory_structure: false,
            usage_group_file_structure: true,
            usage_flatten_modules: false,
            usage_show_read: true,
            usage_show_write: true,
            usage_show_imports: true,
            todo_group_packages: false,
            todo_group_modules: false,
            todo_flatten_packages: false,
            todo_templates: vec!["TODO".to_owned(), "FIXME".to_owned()],
            todo_case_sensitive: true,
            updates_check_automatically: true,
            default_layout: None,
            right_margin: DEFAULT_RIGHT_MARGIN,
            toolbar_visible: true,
            tool_window_bars_visible: true,
            status_bar_visible: true,
            navigation_bar_visible: true,
            semantic_highlighting: true,
            quick_doc_on_hover: true,
            build_tools: BuildTools::default(),
            download_sources_automatically: false,
            check_dependency_updates: true,
            git_path: String::new(),
            git_update_method: GitUpdateMethod::Merge,
            git_auto_update_on_reject: false,
            git_update_submodules: true,
            git_warn_crlf: true,
            git_analyze_before_commit: false,
            annotate_author: true,
            annotate_date: true,
            annotate_commit: false,
            annotate_names: AnnotateNames::LastName,
            terminal_shell: String::new(),
            terminal_start_directory: String::new(),
            terminal_environment: String::new(),
            terminal_confirm_multiline_paste: true,
            diff_side_by_side: true,
            diff_whitespace: DiffWhitespace::DoNotIgnore,
            diff_highlighting: DiffHighlighting::ByWord,
            diff_synchronize_scrolling: true,
            diff_show_line_numbers: true,
            diff_soft_wrap: false,
            diff_context_lines: 3,
            diff_collapse_unchanged: false,
            git_group_by_directory: true,
            http_timeout_seconds: DEFAULT_HTTP_TIMEOUT_SECONDS,
            kubectl_path: String::new(),
            kubeconfig_path: String::new(),
            database_page_rows: DEFAULT_DATABASE_PAGE_ROWS,
            sql_keyword_case: SqlKeywordCase::Upper,
            sql_script_stop_on_error: true,
            project_always_select_opened_file: false,
            project_compact_middle_packages: true,
            ignored_files: DEFAULT_IGNORED_FILES
                .iter()
                .map(|pattern| (*pattern).to_owned())
                .collect(),
            ai_enabled: true,
            mcp_enabled: true,
            mcp_binary: String::new(),
            mcp_execute_permission: McpExecutePermission::AlwaysAsk,
            mcp_tool_settings: BTreeMap::new(),
            mcp_router_only_mode: false,
            mcp_router_only_scope: McpRouterScope::AllAgents,
            mcp_terminal_suggestions: true,
            terminal_escape_focus: true,
        }
    }
}

impl Global for Settings {}

impl Settings {
    /// Whether the memory check stops a language server on a machine that is or is not a
    /// virtual one: the choice of the user when there is one, otherwise the default of the
    /// machine.
    pub fn memory_check_on(&self, virtual_machine: bool) -> bool {
        self.memory_low_check.unwrap_or(!virtual_machine)
    }

    /// The thresholds of the memory check in force on this computer: zero when the check is
    /// off, so that no server stops for memory.
    pub fn memory_thresholds(&self) -> ::platform::MemoryThresholds {
        self.memory_thresholds_on(::platform::is_virtual_machine())
    }

    /// The thresholds of the memory check on a machine that is or is not a virtual one.
    pub fn memory_thresholds_on(&self, virtual_machine: bool) -> ::platform::MemoryThresholds {
        if !self.memory_check_on(virtual_machine) {
            return ::platform::MemoryThresholds::new(0., 0);
        }
        self.memory_numbers()
    }

    /// The numbers of the memory check, whether the check stops or not: a virtual machine
    /// that leaves the check off still warns from them.
    pub fn memory_numbers(&self) -> ::platform::MemoryThresholds {
        ::platform::MemoryThresholds::new(self.memory_low_percent, self.memory_low_megabytes)
    }

    /// The heap the language server takes at its next start, or `None` when it chooses.
    pub fn language_max_heap(&self) -> Option<u32> {
        (self.language_max_heap_megabytes > 0).then_some(self.language_max_heap_megabytes)
    }

    /// A damaged or missing file is not an error: the defaults come back.
    pub fn load(path: &Path) -> Self {
        Self::load_with_theme_choice(path, false)
    }

    /// Reads the file. A file written before Sync with OS existed keeps the theme of an
    /// installation that already chose one: the sync stays off there. A file without an
    /// explicit value in an installation without a chosen theme follows the system, as a
    /// new one does.
    pub fn load_with_theme_choice(path: &Path, theme_chosen: bool) -> Self {
        settings_file::read(path, theme_chosen).settings
    }

    /// The saved name of the theme the system appearance asks for.
    pub fn theme_for(&self, system_dark: bool) -> &str {
        if system_dark {
            &self.theme_dark
        } else {
            &self.theme_light
        }
    }

    /// Writes every key of these settings into the file and keeps the keys the file holds
    /// that these settings do not name. The window writes only the keys it changed, see
    /// `settings_file`. Blocks: not for the UI thread.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        settings_file::write(path, &settings_file::all_keys(self), &[])
            .map_err(|error| std::io::Error::other(error.to_string()))
    }

    /// Takes over the leaves of `draft` that differ from `saved`. A field that another
    /// part of the window changed while the dialog was open keeps its newer value, even
    /// inside a nested object.
    pub fn merge_draft(&mut self, saved: &Settings, draft: &Settings) {
        let (Ok(saved), Ok(draft), Ok(mut current)) = (
            serde_json::to_value(saved),
            serde_json::to_value(draft),
            serde_json::to_value(&*self),
        ) else {
            return;
        };
        merge_changed(&mut current, &saved, &draft);
        if let Ok(merged) = serde_json::from_value(current) {
            *self = merged;
        }
    }

    pub(crate) fn clamped(mut self) -> Self {
        if !self.font_size.is_finite() {
            self.font_size = DEFAULT_FONT_SIZE;
        }
        self.font_size = self.font_size.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE);
        if !self.presentation_font_size.is_finite() {
            self.presentation_font_size = crate::appearance::DEFAULT_PRESENTATION_FONT_SIZE;
        }
        self.presentation_font_size = self.presentation_font_size.clamp(
            crate::appearance::MIN_PRESENTATION_FONT_SIZE,
            crate::appearance::MAX_PRESENTATION_FONT_SIZE,
        );
        if !self.memory_low_percent.is_finite() {
            self.memory_low_percent = DEFAULT_MEMORY_LOW_PERCENT;
        }
        self.memory_low_percent = self.memory_low_percent.clamp(0., 100.);
        self.memory_low_megabytes = self.memory_low_megabytes.min(MAX_MEMORY_LOW_MEGABYTES);
        if self.language_max_heap_megabytes != 0 {
            self.language_max_heap_megabytes = self
                .language_max_heap_megabytes
                .clamp(MIN_LANGUAGE_MAX_HEAP_MB, MAX_LANGUAGE_MAX_HEAP_MB);
        }
        if !self.ui_scale.is_finite() {
            self.ui_scale = DEFAULT_UI_SCALE;
        }
        self.ui_scale = self.ui_scale.clamp(MIN_UI_SCALE, MAX_UI_SCALE);
        if self.editor_font_family.trim().is_empty() {
            self.editor_font_family = "JetBrains Mono".to_owned();
        }
        if self.icon_set.trim().is_empty() {
            self.icon_set = "arro".to_owned();
        }
        if !self.line_spacing.is_finite() {
            self.line_spacing = DEFAULT_LINE_SPACING;
        }
        self.line_spacing = self.line_spacing.clamp(MIN_LINE_SPACING, MAX_LINE_SPACING);
        self.tab_size = self.tab_size.clamp(1, MAX_INDENT_SIZE as u32);
        self.indent_size = self.indent_size.clamp(1, MAX_INDENT_SIZE as u32);
        self.right_margin = self.right_margin.clamp(MIN_RIGHT_MARGIN, MAX_RIGHT_MARGIN);
        self.completion_delay_ms = self.completion_delay_ms.min(MAX_COMPLETION_DELAY_MS);
        self.ignored_files = self
            .ignored_files
            .iter()
            .map(|pattern| pattern.trim())
            .filter(|pattern| !pattern.is_empty())
            .fold(Vec::new(), |mut kept, pattern| {
                if !kept.iter().any(|known| known == pattern) {
                    kept.push(pattern.to_owned());
                }
                kept
            });
        self.diff_context_lines = self.diff_context_lines.min(MAX_DIFF_CONTEXT_LINES);
        self.http_timeout_seconds = self.http_timeout_seconds.clamp(1, MAX_HTTP_TIMEOUT_SECONDS);
        self
    }

    /// One step bigger or smaller, inside the limits.
    pub fn with_font_step(&self, step: f32) -> Self {
        Self {
            font_size: self.font_size + step,
            ..self.clone()
        }
        .clamped()
    }

    /// One step of the font of Presentation Mode, inside the limits.
    pub fn with_presentation_font_step(&self, step: f32) -> Self {
        Self {
            presentation_font_size: self.presentation_font_size + step,
            ..self.clone()
        }
        .clamped()
    }

    /// One step of the interface scale, rounded so that the steps do not drift.
    pub fn with_ui_scale_step(&self, step: f32) -> Self {
        let scale = ((self.ui_scale + step) * 100.).round() / 100.;
        Self {
            ui_scale: scale,
            ..self.clone()
        }
        .clamped()
    }

    /// The regular scale of the interface.
    pub fn with_ui_scale(&self, scale: f32) -> Self {
        Self {
            ui_scale: scale,
            ..self.clone()
        }
        .clamped()
    }

    /// One step of the editor line spacing, rounded so that the steps do not drift.
    pub fn with_line_spacing_step(&self, step: f32) -> Self {
        let spacing = ((self.line_spacing + step) * 100.).round() / 100.;
        Self {
            line_spacing: spacing,
            ..self.clone()
        }
        .clamped()
    }
}

/// Copies the changed leaves of `draft` over `target`, one key of an object at a time.
fn merge_changed(
    target: &mut serde_json::Value,
    saved: &serde_json::Value,
    draft: &serde_json::Value,
) {
    match (saved, draft) {
        (serde_json::Value::Object(saved), serde_json::Value::Object(draft)) => {
            let serde_json::Value::Object(target) = target else {
                return;
            };
            for (key, draft_value) in draft {
                let saved_value = saved.get(key).unwrap_or(&serde_json::Value::Null);
                if saved_value == draft_value {
                    continue;
                }
                match target.get_mut(key) {
                    Some(target_value) => merge_changed(target_value, saved_value, draft_value),
                    None => {
                        target.insert(key.clone(), draft_value.clone());
                    }
                }
            }
        }
        _ if saved != draft => *target = draft.clone(),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CompletionCase, MAX_COMPLETION_DELAY_MS, MAX_INDENT_SIZE, MIN_LINE_SPACING, Settings,
        TrimTrailingWhitespace,
    };

    #[test]
    fn an_old_settings_file_keeps_the_theme_the_installation_chose() {
        let dir = test_support::scratch("settings-theme");
        let file = dir.join("settings.json");
        std::fs::write(&file, r#"{"font_size": 15.0}"#).unwrap();

        let old = Settings::load_with_theme_choice(&file, true);
        assert!(
            !old.theme_sync_with_os,
            "a theme chosen before Sync with OS existed keeps the sync off"
        );
        assert_eq!(old.font_size, 15.);
        assert_eq!(old.theme_light, "light");
        assert_eq!(old.theme_dark, "dark");
        assert!(
            Settings::load_with_theme_choice(&file, false).theme_sync_with_os,
            "an installation without a chosen theme follows the system"
        );

        std::fs::write(&file, r#"{"theme_sync_with_os": true}"#).unwrap();
        assert!(
            Settings::load_with_theme_choice(&file, true).theme_sync_with_os,
            "a file that names the switch keeps its value"
        );
        assert!(
            !Settings::load_with_theme_choice(&dir.join("missing.json"), true).theme_sync_with_os,
            "a start without a settings file keeps the chosen theme too"
        );
    }

    #[test]
    fn the_compact_middle_packages_switch_is_on_by_default_and_saved_when_off() {
        let dir = test_support::scratch("settings-compact-packages");
        let file = dir.join("settings.json");
        std::fs::write(&file, "{}").unwrap();
        assert!(Settings::load(&file).project_compact_middle_packages);
        let off = Settings {
            project_compact_middle_packages: false,
            ..Settings::default()
        };
        off.save(&file).unwrap();
        assert!(!Settings::load(&file).project_compact_middle_packages);
    }

    #[test]
    fn the_project_always_select_switch_is_saved_and_read_back() {
        let dir = test_support::scratch("settings-always-select");
        let file = dir.join("settings.json");
        let on = Settings {
            project_always_select_opened_file: true,
            ..Settings::default()
        };
        on.save(&file).unwrap();
        assert!(Settings::load(&file).project_always_select_opened_file);
        assert!(!Settings::load(&dir.join("missing.json")).project_always_select_opened_file);
    }

    #[test]
    fn a_missing_or_damaged_file_gives_the_defaults() {
        let dir = test_support::scratch("settings");
        let file = dir.join("settings.json");
        assert_eq!(Settings::load(&file), Settings::default());
        std::fs::write(&file, "{broken").unwrap();
        assert_eq!(Settings::load(&file), Settings::default());
    }

    #[test]
    fn settings_round_trip_and_a_partial_file_keeps_the_other_defaults() {
        let dir = test_support::scratch("settings-rt");
        let file = dir.join("ide").join("settings.json");
        let settings = Settings {
            font_size: 16.,
            autosave: false,
            runtime_home: "C:/runtime".into(),
            ..Settings::default()
        };
        settings.save(&file).unwrap();
        assert_eq!(Settings::load(&file), settings);

        std::fs::write(&file, r#"{"soft_wrap": true, "future": 1}"#).unwrap();
        let partial = Settings::load(&file);
        assert!(partial.soft_wrap);
        assert!(partial.autosave, "the default stays");
    }

    #[test]
    fn the_font_size_stays_in_limits() {
        let settings = Settings::default();
        assert_eq!(settings.with_font_step(1.).font_size, 14.);
        assert_eq!(settings.with_font_step(100.).font_size, 32.);
        assert_eq!(settings.with_font_step(-100.).font_size, 8.);
        let wild = Settings {
            font_size: f32::NAN,
            ..Settings::default()
        };
        assert_eq!(wild.with_font_step(0.).font_size, 13.);
    }

    #[test]
    fn the_editor_settings_round_trip_and_an_old_file_gets_the_defaults() {
        let dir = test_support::scratch("settings-editor");
        let file = dir.join("settings.json");
        let settings = Settings {
            editor_font_family: "Consolas".into(),
            line_spacing: 1.4,
            show_line_numbers: false,
            highlight_current_line: false,
            show_indent_guides: false,
            caret_blink: false,
            trim_trailing_whitespace: TrimTrailingWhitespace::Modified,
            ensure_final_newline: true,
            zoom_with_wheel: false,
            tab_size: 2,
            indent_size: 2,
            use_tabs: true,
            completion_case: CompletionCase::All,
            completion_auto_popup: false,
            completion_delay_ms: 300,
            ..Settings::default()
        };
        settings.save(&file).unwrap();
        assert_eq!(Settings::load(&file), settings);

        std::fs::write(&file, r#"{"soft_wrap": true}"#).unwrap();
        let old = Settings::load(&file);
        assert!(old.soft_wrap);
        assert_eq!(old.editor_font_family, "JetBrains Mono");
        assert_eq!(old.line_spacing, 1.);
        assert!(old.show_line_numbers);
        assert!(old.highlight_current_line);
        assert!(old.show_indent_guides);
        assert!(old.caret_blink);
        assert_eq!(old.trim_trailing_whitespace, TrimTrailingWhitespace::None);
        assert!(!old.ensure_final_newline);
        assert!(old.zoom_with_wheel);
        assert_eq!(old.tab_size, 4);
        assert_eq!(old.indent_size, 4);
        assert!(!old.use_tabs);
        assert_eq!(old.completion_case, CompletionCase::None);
        assert!(old.completion_auto_popup);
        assert_eq!(old.completion_delay_ms, 0);
    }

    #[test]
    fn the_new_numbers_stay_in_their_limits() {
        let dir = test_support::scratch("settings-limits");
        let file = dir.join("settings.json");
        let settings = Settings {
            line_spacing: 0.2,
            tab_size: 0,
            indent_size: 99,
            completion_delay_ms: 99_999,
            editor_font_family: "  ".into(),
            ..Settings::default()
        };
        settings.save(&file).unwrap();
        let clamped = Settings::load(&file);
        assert_eq!(clamped.line_spacing, MIN_LINE_SPACING);
        assert_eq!(clamped.tab_size, 1);
        assert_eq!(clamped.indent_size, MAX_INDENT_SIZE as u32);
        assert_eq!(clamped.completion_delay_ms, MAX_COMPLETION_DELAY_MS);
        assert_eq!(clamped.editor_font_family, "JetBrains Mono");
    }

    #[test]
    fn a_nested_draft_field_keeps_the_neighbour_changed_outside() {
        let saved = {
            let mut settings = Settings::default();
            settings.build_tools.first.home = "old home".into();
            settings.build_tools.second.user_home = "old user home".into();
            settings.git_path = "old git".into();
            settings
        };
        let mut draft = saved.clone();
        draft.build_tools.first.home = "new home".into();

        let mut current = saved.clone();
        current.build_tools.second.user_home = "changed outside".into();
        current.git_path = "new git".into();
        current.merge_draft(&saved, &draft);

        assert_eq!(
            current.build_tools.first.home, "new home",
            "the field of the draft wins"
        );
        assert_eq!(
            current.build_tools.second.user_home, "changed outside",
            "a neighbour inside the same object keeps its newer value"
        );
        assert_eq!(
            current.git_path, "new git",
            "a field the draft did not touch keeps its newer value"
        );
    }

    #[test]
    fn the_save_leaves_no_temporary_file_behind() {
        let dir = test_support::scratch("settings-atomic");
        let file = dir.join("settings.json");
        let settings = Settings {
            font_size: 17.,
            ..Settings::default()
        };
        settings.save(&file).unwrap();
        settings.save(&file).unwrap();
        assert_eq!(Settings::load(&file), settings);

        let names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["settings.json"], "only the settings file stays");
    }

    #[test]
    fn the_memory_and_heap_settings_round_trip() {
        let dir = test_support::scratch("settings-memory-heap");
        let file = dir.join("settings.json");
        let settings = Settings {
            memory_low_megabytes: 1024,
            memory_low_percent: 10.,
            language_max_heap_megabytes: 3072,
            ..Settings::default()
        };
        settings.save(&file).unwrap();

        let loaded = Settings::load(&file);
        assert_eq!(loaded.memory_low_megabytes, 1024);
        assert_eq!(loaded.memory_low_percent, 10.);
        assert_eq!(loaded.language_max_heap(), Some(3072));
        assert_eq!(
            loaded.memory_low_check, None,
            "no choice is written by default"
        );
        assert_eq!(
            loaded.memory_thresholds_on(false),
            ::platform::MemoryThresholds::new(10., 1024)
        );

        let off = Settings {
            memory_low_check: Some(false),
            ..loaded.clone()
        };
        off.save(&file).unwrap();
        let reloaded = Settings::load(&file);
        assert_eq!(
            reloaded.memory_low_check,
            Some(false),
            "a choice of the user is saved"
        );
        assert_eq!(
            reloaded.memory_thresholds_on(false),
            ::platform::MemoryThresholds::new(0., 0),
            "a check that is off takes no memory as low"
        );
    }

    #[test]
    fn the_memory_check_defaults_to_on_except_on_a_virtual_machine() {
        let settings = Settings::default();
        assert!(
            settings.memory_check_on(false),
            "an ordinary computer stops for memory"
        );
        assert!(
            !settings.memory_check_on(true),
            "a virtual machine does not stop for memory by default"
        );
        assert_eq!(
            settings.memory_thresholds_on(true),
            ::platform::MemoryThresholds::new(0., 0)
        );
        assert_eq!(
            settings.memory_thresholds_on(false),
            ::platform::MemoryThresholds::new(5., 512)
        );
        assert_eq!(
            settings.memory_numbers(),
            ::platform::MemoryThresholds::new(5., 512),
            "the numbers stay for the warning of a virtual machine"
        );
    }

    #[test]
    fn an_explicit_choice_wins_over_the_machine_both_ways() {
        let on = Settings {
            memory_low_check: Some(true),
            ..Settings::default()
        };
        assert!(
            on.memory_check_on(true),
            "a virtual machine stops when the user turned it on"
        );
        assert_eq!(
            on.memory_thresholds_on(true),
            ::platform::MemoryThresholds::new(5., 512)
        );
        let off = Settings {
            memory_low_check: Some(false),
            ..Settings::default()
        };
        assert!(
            !off.memory_check_on(false),
            "an ordinary computer stops nothing when the user turned it off"
        );
    }

    #[test]
    fn a_heap_below_the_floor_is_raised_and_zero_lets_the_server_choose() {
        let dir = test_support::scratch("settings-heap-limits");
        let file = dir.join("settings.json");
        std::fs::write(&file, r#"{"language_max_heap_megabytes": 64}"#).unwrap();
        assert_eq!(
            Settings::load(&file).language_max_heap(),
            Some(super::MIN_LANGUAGE_MAX_HEAP_MB)
        );
        std::fs::write(&file, r#"{"language_max_heap_megabytes": 0}"#).unwrap();
        assert_eq!(Settings::load(&file).language_max_heap(), None);
    }
}
