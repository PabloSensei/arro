//! The theme in colours of GPUI.

use std::ops::Deref;

use gpui_kit::*;
use theme::{Color, Tokens};

fn hsla_of(color: Color) -> Hsla {
    let Color { r, g, b, a } = color;
    Rgba { r, g, b, a }.into()
}

fn mix(from: Hsla, to: Hsla, t: f32) -> Hsla {
    let (a, b) = (from.to_rgb(), to.to_rgb());
    let lerp = |x: f32, y: f32| x + (y - x) * t;
    Rgba {
        r: lerp(a.r, b.r),
        g: lerp(a.g, b.g),
        b: lerp(a.b, b.b),
        a: lerp(a.a, b.a),
    }
    .into()
}

#[derive(Clone, Copy)]
pub struct Theme(Tokens<Hsla>);

impl Deref for Theme {
    type Target = Tokens<Hsla>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Global for Theme {}

impl Theme {
    pub fn dark() -> Self {
        Self(Tokens::dark().map(hsla_of))
    }

    pub fn light() -> Self {
        Self(Tokens::light().map(hsla_of))
    }

    /// A built-in theme with some tokens replaced, colours as 0xRRGGBBAA. Returns the
    /// names that are no token.
    pub fn with_colors<'a>(
        dark: bool,
        colors: impl IntoIterator<Item = (&'a str, u32)>,
    ) -> (Self, Vec<String>) {
        let mut tokens = if dark {
            Tokens::dark()
        } else {
            Tokens::light()
        };
        let mut unknown = Vec::new();
        for (name, hex) in colors {
            if !tokens.set(name, Color::from_hex(hex)) {
                unknown.push(name.to_owned());
            }
        }
        (Self(tokens.map(hsla_of)), unknown)
    }

    /// Cross-fade used by the theme switch animation.
    pub fn mix(from: &Self, to: &Self, t: f32) -> Self {
        let mut tokens = from.0.zip(&to.0, |a, b| mix(a, b, t));
        tokens.dark = if t < 0.5 { from.dark } else { to.dark };
        Self(tokens)
    }
}
