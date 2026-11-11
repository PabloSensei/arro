//! Contrast of the accent, selection and link tokens of both themes: WCAG AA wants
//! 4.5:1 for text. The values may only move along their own hue, so a failing pair is
//! fixed here and in `src/lib.rs` together.

use theme::{Color, Tokens};

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
fn accent_selection_and_links_keep_their_contrast() {
    for theme in [Tokens::dark(), Tokens::light()] {
        let checks = [
            ("over_accent on accent", theme.over_accent, theme.accent),
            ("text on selection", theme.text, theme.selection),
            ("link on editor_bg", theme.link, theme.editor_bg),
            ("match_text on match_bg", theme.match_text, theme.match_bg),
        ];
        for (name, color, background) in checks {
            let ratio = contrast(color, background);
            assert!(ratio >= 4.5, "{name} {ratio:.2} dark={}", theme.dark);
        }
    }
}
