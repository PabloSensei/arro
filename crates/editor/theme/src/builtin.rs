//! The themes of the binary: the same JSON files a user writes, built in so that a start
//! has them without a file system.
//!
//! The two root themes name every token. A new theme with a parent only names what makes
//! it itself, so a later release may add a token without breaking it.

use crate::format::ThemeError;
use crate::library::{ThemeDefinition, resolve_builtin};
use crate::{Color, Tokens};

/// The id and the text of every built-in theme file.
pub(crate) const FILES: &[(&str, &str)] = &[
    ("dark", include_str!("../themes/dark.theme.json")),
    ("light", include_str!("../themes/light.theme.json")),
    (
        "high-contrast",
        include_str!("../themes/high-contrast.theme.json"),
    ),
    ("retro", include_str!("../themes/retro.theme.json")),
    ("nord", include_str!("../themes/nord.theme.json")),
    ("mocha", include_str!("../themes/mocha.theme.json")),
    ("paper", include_str!("../themes/paper.theme.json")),
    ("nightfall", include_str!("../themes/nightfall.theme.json")),
];

/// The tokens of a built-in theme. The tests check every file, so a broken one stops the
/// build rather than a start without a theme.
pub(crate) fn tokens(id: &str) -> Tokens<Color> {
    crate::ThemeLibrary::builtin()
        .get(id)
        .unwrap_or_else(|| panic!("the built-in theme {id} is part of the binary"))
        .tokens
}

pub(crate) fn resolve_all() -> (Vec<ThemeDefinition>, Vec<ThemeError>) {
    resolve_builtin(FILES)
}
