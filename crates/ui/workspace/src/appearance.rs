//! The modes and switches of View | Appearance: full screen, Distraction Free, Zen and
//! Presentation, the parts of the window and the parts of the editor.

use ui_kit::*;

/// The mode of the window. Every mode but `Normal` hides the tool windows, the stripes,
/// the status bar, the tabs of the editors and the navigation bar of the editor.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WindowMode {
    /// Everything the settings allow.
    #[default]
    Normal,
    /// The text alone, without the parts of the window around it.
    DistractionFree,
    /// Distraction Free with a larger font in the editors.
    Presentation,
    /// Distraction Free in a window that fills the screen.
    Zen,
}

impl WindowMode {
    /// Whether the mode leaves the editor alone and hides the rest of the window.
    pub fn hides_tools(self) -> bool {
        !matches!(self, WindowMode::Normal)
    }

    /// Whether the editors of the window show themselves in this mode.
    pub fn distraction_free(self) -> bool {
        self.hides_tools()
    }

    /// Whether the editors use the font of Presentation Mode.
    pub fn presentation(self) -> bool {
        matches!(self, WindowMode::Presentation)
    }
}

/// The default font of Presentation Mode, in points.
pub const DEFAULT_PRESENTATION_FONT_SIZE: f32 = 24.;
/// The smallest and the largest font of Presentation Mode.
pub const MIN_PRESENTATION_FONT_SIZE: f32 = 12.;
pub const MAX_PRESENTATION_FONT_SIZE: f32 = 72.;

/// What the open editors show in the mode of the window. It is not a setting: leaving the
/// mode brings the settings of the editor back untouched.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct EditorAppearance {
    /// No line numbers, no gutter icons and no breadcrumbs; the text is centred.
    pub distraction_free: bool,
    /// The font of Presentation Mode; `None` for the size of Settings.
    pub presentation_font: Option<f32>,
}

impl EditorAppearance {
    pub fn of(mode: WindowMode, presentation_font: f32) -> Self {
        Self {
            distraction_free: mode.distraction_free(),
            presentation_font: mode.presentation().then_some(presentation_font),
        }
    }
}

impl Global for EditorAppearance {}

/// Full screen: the row of View | Appearance and the key of macOS.
pub const TOGGLE_FULL_SCREEN: &str = "window.toggle_full_screen";
/// The modes of View | Appearance.
pub const TOGGLE_DISTRACTION_FREE: &str = "window.toggle_distraction_free";
pub const TOGGLE_ZEN_MODE: &str = "window.toggle_zen_mode";
pub const TOGGLE_PRESENTATION_MODE: &str = "window.toggle_presentation_mode";
/// The parts of the window.
pub const TOGGLE_TOOLBAR: &str = "window.toggle_toolbar";
pub const TOGGLE_TOOL_WINDOW_BARS: &str = "window.toggle_tool_window_bars";
pub const TOGGLE_STATUS_BAR: &str = "window.toggle_status_bar";
pub const TOGGLE_NAVIGATION_BAR: &str = "window.toggle_navigation_bar";
/// The parts of the editor.
pub const TOGGLE_LINE_NUMBERS: &str = "window.toggle_line_numbers";
pub const TOGGLE_GUTTER_ICONS: &str = "window.toggle_gutter_icons";
pub const TOGGLE_INDENT_GUIDES: &str = "window.toggle_indent_guides";
pub const TOGGLE_BREADCRUMBS: &str = "window.toggle_breadcrumbs";
pub const TOGGLE_STICKY_LINES: &str = "window.toggle_sticky_lines";

/// `✓ Name` for a switch that is on, as the menus of the window write one.
pub fn checked_label(name: &str, on: bool) -> String {
    if on {
        format!("✓ {name}")
    } else {
        name.to_owned()
    }
}
