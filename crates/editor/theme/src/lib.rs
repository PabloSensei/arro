//! Design tokens: our own palette on warm greys with an orange accent, dark and light.
//! Text and code keep a WCAG contrast of 4.5:1 on their background, muted text 3:1,
//! see the tests. This crate knows nothing about the UI framework.

/// A colour in sRGB, each channel in 0..=1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
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
}

macro_rules! tokens {
    ($($field:ident: $dark:expr, $light:expr;)*) => {
        /// One value per token. `C` is [`Color`] here and the colour type of the
        /// UI framework in `ui-kit`.
        #[derive(Clone, Copy, Debug)]
        pub struct Tokens<C> {
            pub dark: bool,
            $(pub $field: C,)*
        }

        impl Tokens<Color> {
            pub fn dark() -> Self {
                Self { dark: true, $($field: Color::from_hex($dark),)* }
            }

            pub fn light() -> Self {
                Self { dark: false, $($field: Color::from_hex($light),)* }
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
        }
    };
}

tokens! {
    // Window chrome. Comments name the role of a group of tokens; Islands key names
    // stand for the same role.
    main_bg: 0x2a2826ff, 0xebe8e3ff;
    island_bg: 0x1c1b1aff, 0xfdfcfaff;
    editor_bg: 0x1c1b1aff, 0xfdfcfaff;
    border: 0x2a2826ff, 0xebe8e3ff;
    // Text: default, muted, secondary, disabled, link, over the accent.
    text: 0xdedad3ff, 0x1c1b1aff;
    text_muted: 0xa8a39aff, 0x5e5a54ff;
    text_secondary: 0x928c83ff, 0x6f6a63ff;
    text_disabled: 0x56524cff, 0xa8a39aff;
    link: 0xfdba74ff, 0x9a3412ff;
    over_accent: 0x1c1b1aff, 0xffffffff;
    // Selection in lists and trees.
    selection: 0x3d2a1cff, 0xffedd5ff;
    selection_muted: 0x30241bff, 0xfff7edff;
    selection_inactive: 0x363330ff, 0xe6e2dcff;
    selection_hover: 0xfff4e612, 0x2a201008;
    // Toolbar buttons: hovered, pressed, border; the accent.
    hover: 0xfff4e619, 0x2a201012;
    pressed: 0xfff4e62b, 0x2a201020;
    toolbar_border: 0xfff4e622, 0x2a201020;
    accent: 0xf97316ff, 0xc2410cff;
    // The selected editor tab.
    tab_selected: 0x3d2a1cff, 0xffedd5ff;
    tab_selected_border: 0xf97316ff, 0xfdba74ff;
    // Popups, notifications, controls.
    popup_bg: 0x2a2826ff, 0xfdfcfaff;
    popup_border: 0x383532ff, 0xe6e2dcff;
    feedback_bg: 0x383532ff, 0xfdfcfaff;
    feedback_border: 0x383532ff, 0xd6d1c9ff;
    control_border: 0x46423eff, 0xd6d1c9ff;
    control_small: 0x46423eff, 0xdedad3ff;
    // A search match and the text over it. The blue stays apart from the orange
    // selection, and the light text keeps its contrast on the dark blue.
    match_bg: 0x1e4a6bff, 0xbfdbfeff;
    match_text: 0xfff7edff, 0x1c1b1aff;
    // The close button hover of Windows 11.
    close_hover: 0xc42b1cff, 0xc42b1cff;
    // Code.
    code: 0xd4d0c8ff, 0x1f1d1bff;
    keyword: 0xe8899cff, 0xb02a4fff;
    string: 0xd8b46eff, 0x8a5a00ff;
    number: 0xb8a2f4ff, 0x5b3fc4ff;
    comment: 0x8f8a81ff, 0x75706aff;
    doc: 0x9d9ab5ff, 0x6a6593ff;
    doc_tag: 0xbcb6e0ff, 0x564f8cff;
    function: 0x7db6f5ff, 0x1d5fa8ff;
    annotation: 0x5ccfbfff, 0x0b7363ff;
    field: 0xa9d18aff, 0x356a19ff;
    unused: 0x77726aff, 0x878279ff;
    // Semantic tokens: what a name is, painted over the syntax colours.
    semantic_field: 0xc4a1f5ff, 0x7b3fb0ff;
    semantic_parameter: 0x8ec7d8ff, 0x1b6f86ff;
    semantic_local: 0xa9bdd4ff, 0x35536eff;
    semantic_method: 0x86b9f5ff, 0x1d5fa8ff;
    semantic_type: 0x8fd3a2ff, 0x2c7a45ff;
    semantic_type_parameter: 0xc5dfa0ff, 0x4f6f1fff;
    semantic_constant: 0xd9a7e8ff, 0x8a3f9eff;
    semantic_annotation: 0x5ccfbfff, 0x0b7363ff;
    caret_row: 0x242220ff, 0xf4f1ecff;
    caret: 0xe8e3daff, 0x1c1b1aff;
    line_number: 0x7a756dff, 0x8c877fff;
    line_number_current: 0xb3ada3ff, 0x5e5a54ff;
    indent_guide: 0x312e2bff, 0xece8e2ff;
    // Inlay hints and their background.
    inlay: 0x948f86ff, 0x77726bff;
    inlay_bg: 0x3a37338c, 0xebe7e18c;
    // Scrollbars: the thumb at rest and under the pointer, and the track behind it.
    scrollbar_thumb: 0x8a847bcc, 0x8c877fcc;
    scrollbar_thumb_hover: 0xb8b2a8ff, 0x6f6a63ff;
    scrollbar_track: 0xffffff0d, 0x0000000d;
    // A warning on the scrollbar.
    warning_mark: 0xd9a441ff, 0xe0a530ff;
    // Folded text.
    fold_bg: 0x3a3733ff, 0xece6f7ff;
    fold_text: 0xb0aa9fff, 0x4f4a63ff;
    // The marks on the editor scrollbar: errors, warnings, find matches and the caret
    // line.
    stripe_error: 0xf75464ff, 0xc7222dff;
    stripe_warning: 0xd9a441ff, 0xe0a530ff;
    stripe_find: 0x6aa9ffff, 0x2c6fbbff;
    stripe_caret: 0xe8e3daff, 0x1c1b1aff;
    // What the user typed into a console, as IDEA colours its input line.
    console_input: 0x62b543ff, 0x067d17ff;
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
