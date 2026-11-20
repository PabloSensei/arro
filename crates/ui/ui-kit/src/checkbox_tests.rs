//! The empty box of a checkbox on every built-in theme: its border has to stand out from the
//! dialogs and the tool windows, or the box looks like no box at all.

use gpui_kit::Hsla;
use theme::ThemeLibrary;

use crate::{Theme, checkbox_border, checkbox_border_hover};

/// WCAG 2 relative luminance of a colour.
fn luminance(color: Hsla) -> f32 {
    let rgb = color.to_rgb();
    let linear = |c: f32| {
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(rgb.r) + 0.7152 * linear(rgb.g) + 0.0722 * linear(rgb.b)
}

/// WCAG 2 contrast ratio of two colours, the way the theme tests measure it.
fn contrast(a: Hsla, b: Hsla) -> f32 {
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// The border of an empty checkbox reaches 3:1 on the dialog and the tool windows of every
/// theme, as WCAG asks of the parts a control needs to be seen, and 7:1 on High Contrast.
/// `panel_bg` is no token of a theme: the tool windows draw on `island_bg`, and `main_bg` is the
/// window behind them.
#[test]
fn an_empty_checkbox_stands_out_on_every_theme() {
    for definition in ThemeLibrary::builtin().themes() {
        let theme = Theme::from_definition(definition);
        let required = if definition.id == "high-contrast" {
            7.
        } else {
            3.
        };
        let border = checkbox_border(&theme);
        for (name, background) in [
            ("popup_bg", theme.popup_bg),
            ("island_bg", theme.island_bg),
            ("main_bg", theme.main_bg),
        ] {
            let ratio = contrast(border, background);
            assert!(
                ratio >= required,
                "{}: the border of an empty checkbox on {name} is {ratio:.2}, below {required}",
                definition.id
            );
        }
    }
}

/// The pointer brightens the border of an empty checkbox: the hover colour is the brightest
/// text of the theme, so it stands out more than the border at rest on every theme.
#[test]
fn the_border_of_an_empty_checkbox_brightens_under_the_pointer() {
    for definition in ThemeLibrary::builtin().themes() {
        let theme = Theme::from_definition(definition);
        let (rest, hover) = (checkbox_border(&theme), checkbox_border_hover(&theme));
        assert!(
            contrast(hover, theme.popup_bg) > contrast(rest, theme.popup_bg),
            "{}: the hover border does not stand out more than the border at rest",
            definition.id
        );
    }
}
