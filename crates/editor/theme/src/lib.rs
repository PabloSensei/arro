//! Design tokens: our own palette on warm greys with an orange accent, dark and light,
//! and a theme as data: a JSON file with the tokens it changes and a parent theme for the
//! rest. Text and code keep a WCAG contrast of 4.5:1 on their background, muted text 3:1,
//! see the tests. This crate knows nothing about the UI framework.

mod builtin;
pub mod format;
mod icls;
mod library;

pub use format::{ThemeError, ThemeFile};
pub use icls::from_icls;
pub use library::{ThemeDefinition, ThemeLibrary, slug, themes_folder};

/// A colour in sRGB, each channel in 0..=1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
    /// A colour no theme writes; a token left at this value is missing.
    pub const TRANSPARENT: Self = Self {
        r: 0.,
        g: 0.,
        b: 0.,
        a: 0.,
    };

    /// `hex` is 0xRRGGBBAA.
    pub fn from_hex(hex: u32) -> Self {
        let channel = |shift: u32| ((hex >> shift) & 0xff) as f32 / 255.;
        Self {
            r: channel(24),
            g: channel(16),
            b: channel(8),
            a: channel(0),
        }
    }

    /// A colour of a theme file: `#rrggbb`, `#rrggbbaa` or the same without the number
    /// sign. A theme of the other IDE writes its colours without it.
    pub fn from_text(text: &str) -> Option<Self> {
        let text = text.trim().trim_start_matches('#');
        if !text.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        let value = u32::from_str_radix(text, 16).ok()?;
        let hex = match text.len() {
            6 => (value << 8) | 0xff,
            8 => value,
            _ => return None,
        };
        Some(Self::from_hex(hex))
    }

    /// The colour as a theme file writes it, `#rrggbb` or `#rrggbbaa`.
    pub fn to_text(self) -> String {
        let channel = |value: f32| (value.clamp(0., 1.) * 255.).round() as u32;
        let (r, g, b, a) = (
            channel(self.r),
            channel(self.g),
            channel(self.b),
            channel(self.a),
        );
        if a == 0xff {
            format!("#{r:02x}{g:02x}{b:02x}")
        } else {
            format!("#{r:02x}{g:02x}{b:02x}{a:02x}")
        }
    }
}

macro_rules! tokens {
    ($($field:ident;)*) => {
        /// One value per token. `C` is [`Color`] here and the colour type of the
        /// UI framework in `ui-kit`.
        #[derive(Clone, Copy, Debug)]
        pub struct Tokens<C> {
            pub dark: bool,
            $(pub $field: C,)*
        }

        impl Tokens<Color> {
            /// The built-in dark theme.
            pub fn dark() -> Self {
                builtin::tokens("dark")
            }

            /// The built-in light theme.
            pub fn light() -> Self {
                builtin::tokens("light")
            }

            /// Every token transparent: a theme file fills the ones it names, and a token
            /// left transparent went missing.
            fn blank(dark: bool) -> Self {
                Self { dark, $($field: Color::TRANSPARENT,)* }
            }
        }

        impl<C: Copy> Tokens<C> {
            pub fn map<D>(&self, f: impl Fn(C) -> D) -> Tokens<D> {
                Tokens { dark: self.dark, $($field: f(self.$field),)* }
            }

            /// Combines two themes token by token and keeps `dark` of `self`.
            pub fn zip<D>(&self, other: &Self, f: impl Fn(C, C) -> D) -> Tokens<D> {
                Tokens { dark: self.dark, $($field: f(self.$field, other.$field),)* }
            }

            /// Sets the token called `name`, the way a theme file spells it. False when
            /// there is no such token.
            pub fn set(&mut self, name: &str, value: C) -> bool {
                match name {
                    $(stringify!($field) => self.$field = value,)*
                    _ => return false,
                }
                true
            }

            /// The token called `name`, the way a theme file spells it.
            pub fn get(&self, name: &str) -> Option<C> {
                Some(match name {
                    $(stringify!($field) => self.$field,)*
                    _ => return None,
                })
            }
        }

        /// Every token name, as a theme file spells it.
        pub const TOKEN_NAMES: &[&str] = &[$(stringify!($field),)*];
    };
}

tokens! {
    // Window chrome. Comments name the role of a group of tokens; Islands key names
    // stand for the same role.
    main_bg;
    island_bg;
    editor_bg;
    border;
    // Text: default, muted, secondary, disabled, link, over the accent.
    text;
    text_muted;
    text_secondary;
    text_disabled;
    link;
    over_accent;
    // Selection in lists and trees.
    selection;
    selection_muted;
    selection_inactive;
    selection_hover;
    // Toolbar buttons: hovered, pressed, border; the accent.
    hover;
    pressed;
    toolbar_border;
    accent;
    // The selected editor tab.
    tab_selected;
    tab_selected_border;
    // Popups, notifications, controls.
    popup_bg;
    popup_border;
    feedback_bg;
    feedback_border;
    control_border;
    control_small;
    // A search match and the text over it. The blue stays apart from the orange
    // selection, and the light text keeps its contrast on the dark blue.
    match_bg;
    match_text;
    // The close button hover of Windows 11.
    close_hover;
    // Code.
    code;
    keyword;
    string;
    number;
    comment;
    doc;
    doc_tag;
    function;
    annotation;
    field;
    unused;
    // Semantic tokens: what a name is, painted over the syntax colours.
    semantic_field;
    semantic_parameter;
    semantic_local;
    semantic_method;
    semantic_type;
    semantic_type_parameter;
    semantic_constant;
    semantic_annotation;
    caret_row;
    // The line the stopped program is on: the tint of its row and the arrow in the gutter.
    execution_line;
    caret;
    line_number;
    line_number_current;
    indent_guide;
    // Inlay hints and their background.
    inlay;
    inlay_bg;
    // Scrollbars: the thumb at rest and under the pointer, and the track behind it.
    scrollbar_thumb;
    scrollbar_thumb_hover;
    scrollbar_track;
    // A warning on the scrollbar.
    warning_mark;
    // A misspelled word under its green wave, apart from the warnings of a language.
    spell_mark;
    // Folded text.
    fold_bg;
    fold_text;
    // The marks on the editor scrollbar: errors, warnings, find matches and the caret
    // line. A hunk an agent changed and the user has not accepted keeps its own colour.
    stripe_error;
    stripe_warning;
    stripe_find;
    stripe_caret;
    stripe_agent;
    // What the user typed into a console, as IDEA colours its input line.
    console_input;
    // The states of the rest of the window: a test that passed, a change of the version
    // control, a failure. They read on any background a theme puts them over.
    success;
    info;
    error;
}

/// Metrics in logical pixels.
pub mod metrics {
    /// The height of the window title bar. The toolbar and the run and settings entry
    /// points keep this height.
    pub const TOOLBAR_HEIGHT: f32 = 42.;
    /// The height of the status bar under the editor area.
    pub const STATUS_HEIGHT: f32 = 30.;
    /// The width of the docked tool window stripes on the left and on the right.
    pub const STRIPE_WIDTH: f32 = 40.;
    pub const RIGHT_STRIPE_WIDTH: f32 = 38.;

    /// The spacing steps of the window chrome. Inside a group of buttons the step is
    /// `SPACE_XS`, between groups `SPACE_L`, and the window edge keeps `SPACE_M`.
    /// `SPACE_S` is the horizontal padding of a status bar item, `SPACE_XL` the length
    /// of the divider between groups in the title bar and in the tool window stripes.
    pub const SPACE_XS: f32 = 2.;
    pub const SPACE_S: f32 = 4.;
    pub const SPACE_M: f32 = 8.;
    pub const SPACE_L: f32 = 12.;
    pub const SPACE_XL: f32 = 16.;

    /// The height of a toolbar control (button or dropdown) and of a panel header.
    pub const CONTROL_HEIGHT: f32 = 28.;
    /// The square of an icon button in the toolbar and in the tool window stripes.
    pub const ICON_BUTTON: f32 = 28.;
    /// The size of an icon inside a toolbar or stripe button.
    pub const ICON: f32 = 16.;
    /// The corner radius of a toolbar control and of a stripe button.
    pub const CONTROL_RADIUS: f32 = 6.;
    /// The text of the status bar and the size of a dropdown arrow.
    pub const TEXT_SMALL: f32 = 12.;
    /// The flat accent square with the letter of the project in the title bar.
    pub const PROJECT_BADGE: f32 = 20.;
    pub const PROJECT_BADGE_RADIUS: f32 = 4.;
    /// The vertical divider between toolbar groups.
    pub const DIVIDER: f32 = SPACE_XL;
    pub const DIVIDER_WIDTH: f32 = 1.;

    /// Rows of lists, trees and menus: horizontal padding inside a row and the corner
    /// radius of a selected or hovered row. A row is `ROW_HEIGHT` high and centres its
    /// text, so it needs no vertical padding.
    pub const ROW_PADDING_X: f32 = SPACE_M;
    pub const ROW_RADIUS: f32 = 4.;
    /// The inset between the frame of a popup or menu and its rows.
    pub const POPUP_PADDING: f32 = SPACE_S;
    /// Dialogs: padding inside the frame, the gap between sections and the title size.
    pub const DIALOG_PADDING: f32 = SPACE_XL;
    pub const DIALOG_GAP: f32 = SPACE_L;
    pub const TEXT_TITLE: f32 = 15.;

    pub const ISLAND_GAP: f32 = 4.;
    pub const ISLAND_RADIUS: f32 = 10.;
    /// The height of the editor tab bar and of panel headers that are not tool windows.
    pub const HEADER_HEIGHT: f32 = 41.;
    pub const ROW_HEIGHT: f32 = 24.;

    /// Scrollbars: the hit area at the edge, the thumb at rest and under the pointer,
    /// the inset from the edge, the shortest thumb and its corner radius.
    pub const SCROLLBAR_HIT: f32 = SPACE_L;
    pub const SCROLLBAR_THICKNESS: f32 = SPACE_S;
    pub const SCROLLBAR_HOVER_THICKNESS: f32 = SPACE_M;
    pub const SCROLLBAR_INSET: f32 = SPACE_XS;
    pub const SCROLLBAR_MIN_LENGTH: f32 = 24.;
    pub const SCROLLBAR_RADIUS: f32 = SPACE_S;
    pub const TAB_RADIUS: f32 = 8.;
    pub const WINDOW_BUTTON_WIDTH: f32 = 47.;
    pub const LEFT_PANEL_WIDTH: f32 = 456.;
    pub const RIGHT_PANEL_WIDTH: f32 = 380.;
    pub const BOTTOM_PANEL_HEIGHT: f32 = 260.;
    pub const UI_FONT: &str = "Inter";
    pub const MONO_FONT: &str = "JetBrains Mono";
    pub const UI_SIZE: f32 = 13.;
    pub const CODE_SIZE: f32 = 14.;
    pub const CODE_LINE_HEIGHT: f32 = 22.;
}

/// Editor size whose monospace advance (0.6 em) is a whole number of device
/// pixels, so that every glyph lands on the same subpixel offset.
pub fn snapped_code_size(size: f32, scale_factor: f32) -> f32 {
    (size * 0.6 * scale_factor).round() / 0.6 / scale_factor
}

/// Durations of short interface animations, in milliseconds.
pub mod motion {
    /// The fade of every popup and dialog: opacity only, no movement.
    pub const POPUP_FADE: u64 = 80;
    pub const PANEL: u64 = 160;
    pub const THEME: u64 = 180;
    /// The fade of the editor body when the active tab changes.
    pub const TAB: u64 = 120;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_hex_channels_in_order() {
        let color = Color::from_hex(0x336699cc);
        let expected = Color {
            r: 0.2,
            g: 0.4,
            b: 0.6,
            a: 0.8,
        };
        assert_eq!(color, expected);
    }

    #[test]
    fn map_and_zip_keep_every_token() {
        let dark = Tokens::dark();
        let light = Tokens::light();
        assert!(dark.dark && !light.dark);
        assert_eq!(dark.map(|color| color).caret, dark.caret);
        let mixed = dark.zip(&light, |_, to| to);
        assert_eq!(mixed.main_bg, light.main_bg);
        assert!(mixed.dark, "zip keeps the flag of self");
    }

    /// WCAG 2 relative luminance of an opaque colour.
    fn luminance(color: Color) -> f32 {
        let linear = |c: f32| {
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * linear(color.r) + 0.7152 * linear(color.g) + 0.0722 * linear(color.b)
    }

    fn contrast(a: Color, b: Color) -> f32 {
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    #[test]
    fn text_and_code_keep_their_contrast() {
        for theme in [Tokens::dark(), Tokens::light()] {
            let code_bgs = [theme.editor_bg, theme.caret_row];
            let full = [
                ("text", theme.text, [theme.island_bg, theme.main_bg]),
                ("link", theme.link, [theme.island_bg, theme.main_bg]),
                (
                    "over_accent",
                    theme.over_accent,
                    [theme.accent, theme.accent],
                ),
                (
                    "match_text",
                    theme.match_text,
                    [theme.match_bg, theme.match_bg],
                ),
                ("fold_text", theme.fold_text, [theme.fold_bg, theme.fold_bg]),
                ("code", theme.code, code_bgs),
                ("keyword", theme.keyword, code_bgs),
                ("string", theme.string, code_bgs),
                ("number", theme.number, code_bgs),
                ("doc_tag", theme.doc_tag, code_bgs),
                ("function", theme.function, code_bgs),
                ("annotation", theme.annotation, code_bgs),
                ("field", theme.field, code_bgs),
            ];
            let muted = [
                (
                    "text_muted",
                    theme.text_muted,
                    [theme.island_bg, theme.main_bg],
                ),
                (
                    "text_secondary",
                    theme.text_secondary,
                    [theme.island_bg, theme.main_bg],
                ),
                ("comment", theme.comment, code_bgs),
                ("doc", theme.doc, code_bgs),
                ("unused", theme.unused, code_bgs),
                ("line_number", theme.line_number, code_bgs),
                ("inlay", theme.inlay, code_bgs),
            ];
            let checks = full
                .iter()
                .map(|check| (check, 4.5))
                .chain(muted.iter().map(|check| (check, 3.)));
            for ((name, color, backgrounds), least) in checks {
                for background in backgrounds {
                    let ratio = contrast(*color, *background);
                    assert!(ratio >= least, "{name} {ratio:.2} dark={}", theme.dark);
                }
            }
            for name in ["code", "keyword", "string", "number", "function", "field"] {
                let color = full.iter().find(|check| check.0 == name).unwrap().1;
                let ratio = contrast(color, theme.selection);
                assert!(ratio >= 4.5, "{name} on selection {ratio:.2}");
            }
        }
    }

    #[test]
    fn a_token_is_set_by_the_name_a_theme_file_uses() {
        let mut theme = Tokens::dark();

        assert!(theme.set("editor_bg", Color::from_hex(0x102030ff)));
        assert!(!theme.set("no_such_token", Color::from_hex(0)));
        assert_eq!(theme.editor_bg, Color::from_hex(0x102030ff));
    }

    #[test]
    fn snapped_size_gives_a_whole_pixel_advance() {
        for scale in [1., 1.25, 1.5, 2.] {
            let advance = snapped_code_size(14., scale) * 0.6 * scale;
            assert!((advance - advance.round()).abs() < 1e-3, "{scale}");
        }
    }
}
