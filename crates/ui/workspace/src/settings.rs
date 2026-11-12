//! What the user can change in Settings. One JSON file in the settings folder; a missing
//! or damaged file gives the defaults, a new key an old file does not know gets its default.

use std::path::Path;

use project_model::BuildTools;
use serde::{Deserialize, Serialize};
use ui_kit::*;

use crate::layout::DockState;

pub const SETTINGS_FILE: &str = "settings.json";
pub const MIN_FONT_SIZE: f32 = 8.;
pub const MAX_FONT_SIZE: f32 = 32.;
const DEFAULT_FONT_SIZE: f32 = 13.;
const DEFAULT_MEMORY_LOW_PERCENT: f32 = 5.;
const DEFAULT_MEMORY_LOW_MEGABYTES: u32 = 512;
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
pub const COMPLETION_DELAY_STEP_MS: u32 = 100;

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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Size of the text in the editor, in points.
    pub font_size: f32,
    /// Scale of the whole interface, 1 for the regular size.
    pub ui_scale: f32,
    /// Family of the editor text; `JetBrains Mono` by default.
    pub editor_font_family: String,
    /// Distance between two editor lines, as a share of the font size, from 1 to 2.
    pub line_spacing: f32,
    /// Long lines wrap at the edge of the editor. Kept for the editor; not drawn yet.
    pub soft_wrap: bool,
    /// Show spaces and tabs as marks. Kept for the editor; not drawn yet.
    pub show_whitespace: bool,
    /// The line numbers in the gutter of the editor.
    pub show_line_numbers: bool,
    /// The background of the line with the caret.
    pub highlight_current_line: bool,
    /// The thin vertical lines at the steps of the indentation.
    pub show_indent_guides: bool,
    /// The caret blinks while the editor has the focus.
    pub caret_blink: bool,
    /// What a save does with the spaces at the end of a line.
    pub trim_trailing_whitespace: TrimTrailingWhitespace,
    /// A save puts a line break after the last line when the file lacks one.
    pub ensure_final_newline: bool,
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
    /// Unsaved files are written when the window loses the focus or the tab changes.
    pub autosave: bool,
    /// The home of the runtime that runs the project; empty means the one found by the
    /// environment.
    pub runtime_home: String,
    /// Memory below this share of the total and below `memory_low_megabytes` stops a
    /// language server so that the machine does not run out. Zero turns the stop off.
    pub memory_low_percent: f32,
    /// The absolute floor of the same check, in megabytes.
    pub memory_low_megabytes: u32,
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
    /// Looks for a newer release once a day after the window opens.
    pub updates_check_automatically: bool,
    /// The tool window layout Restore Default Layout goes back to; `None` is the layout
    /// of a new window.
    pub default_layout: Option<DefaultLayout>,
    /// The language server's semantic tokens colour the names over the syntax colours.
    pub semantic_highlighting: bool,
    /// The documentation popup opens under the pointer after a delay.
    pub quick_doc_on_hover: bool,
    /// How the build tools of the project start, in the order of their pages.
    pub build_tools: BuildTools,
    /// The program that runs Git; empty means the one on `PATH`.
    pub git_path: String,
    /// How Update Project brings the upstream in.
    pub git_update_method: GitUpdateMethod,
    /// A push the remote rejected fetches, updates and pushes again.
    pub git_auto_update_on_reject: bool,
    /// A commit whose files carry CRLF line separators warns first.
    pub git_warn_crlf: bool,
    /// The commit window starts with code analysis.
    pub git_analyze_before_commit: bool,
    /// The shell of a new terminal; empty means the one found on this machine.
    pub terminal_shell: String,
    /// Where a new terminal starts; empty means the root of the project.
    pub terminal_start_directory: String,
    /// Variables of a new terminal, one `NAME=value` per line.
    pub terminal_environment: String,
    /// The diff shows the two versions side by side rather than as one list.
    pub diff_side_by_side: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            font_size: DEFAULT_FONT_SIZE,
            ui_scale: DEFAULT_UI_SCALE,
            editor_font_family: "JetBrains Mono".to_owned(),
            line_spacing: DEFAULT_LINE_SPACING,
            soft_wrap: false,
            show_whitespace: false,
            show_line_numbers: true,
            highlight_current_line: true,
            show_indent_guides: true,
            caret_blink: true,
            trim_trailing_whitespace: TrimTrailingWhitespace::None,
            ensure_final_newline: false,
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
            autosave: true,
            runtime_home: String::new(),
            memory_low_percent: DEFAULT_MEMORY_LOW_PERCENT,
            memory_low_megabytes: DEFAULT_MEMORY_LOW_MEGABYTES,
            reformat_optimize_imports: false,
            reformat_only_changed: false,
            reformat_selection: false,
            usage_group_usage_type: false,
            usage_group_module: false,
            usage_group_directory: false,
            usage_group_directory_structure: false,
            usage_group_file_structure: false,
            usage_flatten_modules: false,
            usage_show_read: true,
            usage_show_write: true,
            usage_show_imports: true,
            todo_group_packages: false,
            todo_group_modules: false,
            todo_flatten_packages: false,
            updates_check_automatically: true,
            default_layout: None,
            semantic_highlighting: true,
            quick_doc_on_hover: true,
            build_tools: BuildTools::default(),
            git_path: String::new(),
            git_update_method: GitUpdateMethod::Merge,
            git_auto_update_on_reject: false,
            git_warn_crlf: true,
            git_analyze_before_commit: false,
            terminal_shell: String::new(),
            terminal_start_directory: String::new(),
            terminal_environment: String::new(),
            diff_side_by_side: true,
        }
    }
}

impl Global for Settings {}

impl Settings {
    /// A damaged or missing file is not an error: the defaults come back.
    pub fn load(path: &Path) -> Self {
        let text = std::fs::read_to_string(path).ok();
        let parsed: Option<Self> = text.and_then(|text| serde_json::from_str(&text).ok());
        parsed.map_or_else(Self::default, Self::clamped)
    }

    /// Writes the file, creating its folder. Blocks: not for the UI thread.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(path, text)
    }

    fn clamped(mut self) -> Self {
        if !self.font_size.is_finite() {
            self.font_size = DEFAULT_FONT_SIZE;
        }
        self.font_size = self.font_size.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE);
        if !self.memory_low_percent.is_finite() {
            self.memory_low_percent = DEFAULT_MEMORY_LOW_PERCENT;
        }
        self.memory_low_percent = self.memory_low_percent.clamp(0., 100.);
        self.memory_low_megabytes = self.memory_low_megabytes.min(1024 * 1024);
        if !self.ui_scale.is_finite() {
            self.ui_scale = DEFAULT_UI_SCALE;
        }
        self.ui_scale = self.ui_scale.clamp(MIN_UI_SCALE, MAX_UI_SCALE);
        if self.editor_font_family.trim().is_empty() {
            self.editor_font_family = "JetBrains Mono".to_owned();
        }
        if !self.line_spacing.is_finite() {
            self.line_spacing = DEFAULT_LINE_SPACING;
        }
        self.line_spacing = self.line_spacing.clamp(MIN_LINE_SPACING, MAX_LINE_SPACING);
        self.tab_size = self.tab_size.clamp(1, MAX_INDENT_SIZE as u32);
        self.indent_size = self.indent_size.clamp(1, MAX_INDENT_SIZE as u32);
        self.completion_delay_ms = self.completion_delay_ms.min(MAX_COMPLETION_DELAY_MS);
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

#[cfg(test)]
mod tests {
    use super::{
        CompletionCase, MAX_COMPLETION_DELAY_MS, MAX_INDENT_SIZE, MIN_LINE_SPACING, Settings,
        TrimTrailingWhitespace,
    };

    #[test]
    fn a_missing_or_damaged_file_gives_the_defaults() {
        let dir = std::env::temp_dir().join(format!("settings-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("settings.json");
        assert_eq!(Settings::load(&file), Settings::default());
        std::fs::write(&file, "{broken").unwrap();
        assert_eq!(Settings::load(&file), Settings::default());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn settings_round_trip_and_a_partial_file_keeps_the_other_defaults() {
        let dir = std::env::temp_dir().join(format!("settings-rt-{}", std::process::id()));
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
        std::fs::remove_dir_all(&dir).unwrap();
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
        let dir = std::env::temp_dir().join(format!("settings-editor-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
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
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_new_numbers_stay_in_their_limits() {
        let dir = std::env::temp_dir().join(format!("settings-limits-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
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
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(clamped.line_spacing, MIN_LINE_SPACING);
        assert_eq!(clamped.tab_size, 1);
        assert_eq!(clamped.indent_size, MAX_INDENT_SIZE as u32);
        assert_eq!(clamped.completion_delay_ms, MAX_COMPLETION_DELAY_MS);
        assert_eq!(clamped.editor_font_family, "JetBrains Mono");
    }
}
