//! The modes and switches of View | Appearance: full screen, Distraction Free, Zen and
//! Presentation, the parts of the window and the parts of the editor.

use ui_kit::*;

use crate::settings::Settings;

/// What the system tells the windows to look like. A test platform has no appearance of
/// its own, so a test sets this global and the windows take it as the look of the system.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SystemAppearance {
    pub dark: bool,
}

impl SystemAppearance {
    /// The appearance the window shows to the system.
    pub fn of(window: &Window) -> Self {
        Self {
            dark: matches!(
                window.appearance(),
                WindowAppearance::Dark | WindowAppearance::VibrantDark
            ),
        }
    }
}

impl Global for SystemAppearance {}

/// The appearance the window reported last. It is what a settings change follows when it
/// happens without a window of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LastSystemAppearance(SystemAppearance);

impl Global for LastSystemAppearance {}

/// The appearance the windows follow: the one a test put in place, or the one of the
/// window itself.
pub fn system_appearance(window: &Window, cx: &App) -> SystemAppearance {
    current_appearance(cx).unwrap_or_else(|| SystemAppearance::of(window))
}

/// The appearance put in place by a test, or the one the windows reported last.
fn current_appearance(cx: &App) -> Option<SystemAppearance> {
    cx.try_global::<SystemAppearance>()
        .copied()
        .or_else(|| cx.try_global::<LastSystemAppearance>().map(|last| last.0))
}

fn remember_appearance(appearance: SystemAppearance, cx: &mut App) {
    if cx.try_global::<LastSystemAppearance>() != Some(&LastSystemAppearance(appearance)) {
        cx.set_global(LastSystemAppearance(appearance));
    }
}

/// The window reports a new appearance of the system.
pub(crate) fn system_appearance_changed(window: &Window, cx: &mut App) {
    remember_appearance(SystemAppearance::of(window), cx);
}

/// Remembers the appearance of the window and takes the theme of the settings for it
/// without a fade, as the first frame of a window has none.
pub fn follow_system_theme(window: &Window, cx: &mut App) {
    let appearance = system_appearance(window, cx);
    remember_appearance(appearance, cx);
    let Some(settings) = cx.try_global::<Settings>() else {
        return;
    };
    if !settings.theme_sync_with_os {
        return;
    }
    let theme = theme_of(settings, appearance.dark, cx);
    if cx
        .try_global::<Theme>()
        .is_some_and(|current| current.id() == theme.id())
    {
        return;
    }
    cx.set_global(theme);
}

/// The theme of the settings for the look of the system, whether Sync with OS is on or the
/// theme was chosen by hand. The first frame of a window shows it without a fade.
pub fn apply_saved_theme(window: &Window, cx: &mut App) {
    let appearance = system_appearance(window, cx);
    remember_appearance(appearance, cx);
    let Some(settings) = cx.try_global::<Settings>() else {
        return;
    };
    let theme = theme_of(settings, appearance.dark, cx);
    if cx
        .try_global::<Theme>()
        .is_some_and(|current| current.id() == theme.id())
    {
        return;
    }
    cx.set_global(theme);
}

/// The theme the settings ask for: the one of the system look while Sync with OS is on,
/// and the chosen one otherwise. A name this build does not know falls back to the dark
/// theme of the catalog.
pub fn theme_of(settings: &Settings, system_dark: bool, cx: &App) -> Theme {
    let library = ThemeCatalog::library_of(cx);
    let id = if settings.theme_sync_with_os {
        settings.theme_for(system_dark)
    } else if settings.theme.is_empty() {
        if system_dark { "dark" } else { "light" }
    } else {
        settings.theme.as_str()
    };
    Theme::from_library(library, id)
}

/// The appearance a settings change follows: the one the window reported last, or the one
/// a test put in place.
pub(crate) fn last_system_appearance(cx: &App) -> Option<SystemAppearance> {
    current_appearance(cx)
}

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
