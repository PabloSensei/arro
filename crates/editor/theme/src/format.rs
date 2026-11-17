//! The file of a theme: its name, the look it belongs to, the tokens it changes and the
//! theme the rest comes from. Everything lives in one JSON file that a user can copy,
//! edit and put into the `themes` folder of the settings.

use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{Color, TOKEN_NAMES, Tokens};

/// The file of a theme.
///
/// `ui` holds the tokens of the window, `syntax` the colours of the code. Both are the
/// same token set and only the grouping makes a file readable; a token of either group
/// wins over the parent theme.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ThemeFile {
    /// The label the settings show.
    pub name: String,
    /// Whether the theme is meant for a dark look.
    pub dark: bool,
    /// The theme the tokens this file leaves out come from. A file without it has to name
    /// every token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    #[serde(default)]
    pub ui: BTreeMap<String, String>,
    #[serde(default)]
    pub syntax: BTreeMap<String, String>,
}

/// A theme file that cannot be used, or a token of it that this version does not know.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThemeError {
    /// The file, when the error belongs to one of the folder.
    pub path: Option<PathBuf>,
    pub message: String,
}

impl ThemeError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            path: None,
            message: message.into(),
        }
    }

    pub fn at(path: impl Into<PathBuf>, message: impl Into<String>) -> Self {
        Self {
            path: Some(path.into()),
            message: message.into(),
        }
    }
}

impl fmt::Display for ThemeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.path {
            Some(path) => write!(f, "{}: {}", path.display(), self.message),
            None => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for ThemeError {}

impl ThemeFile {
    /// Reads the text of a file. The colours are checked here, the names of the tokens
    /// when the file is resolved against its parent.
    pub fn parse(text: &str) -> Result<Self, ThemeError> {
        let file: Self = serde_json::from_str(text)
            .map_err(|error| ThemeError::new(format!("not a theme file: {error}")))?;
        file.validate()?;
        Ok(file)
    }

    /// Whether the name and the colours of the file make sense.
    fn validate(&self) -> Result<(), ThemeError> {
        if self.name.trim().is_empty() {
            return Err(ThemeError::new("the theme has no name"));
        }
        if self
            .parent
            .as_deref()
            .is_some_and(|parent| parent.trim().is_empty())
        {
            return Err(ThemeError::new("the theme has an empty parent"));
        }
        for (name, value) in self.colors() {
            if Color::from_text(value).is_none() {
                return Err(ThemeError::new(format!("{name} is no colour: {value:?}")));
            }
        }
        Ok(())
    }

    /// Every token the file names, with the colour as it is written there.
    fn colors(&self) -> impl Iterator<Item = (&str, &str)> {
        self.ui
            .iter()
            .chain(self.syntax.iter())
            .map(|(name, value)| (name.as_str(), value.as_str()))
    }

    /// The tokens of the theme over those of `parent`. A file without a parent has to
    /// name every token, because nothing else can fill it. Returns the names this version
    /// does not know.
    pub fn resolve(
        &self,
        parent: Option<Tokens<Color>>,
    ) -> Result<(Tokens<Color>, Vec<String>), ThemeError> {
        let mut tokens = parent.unwrap_or_else(|| Tokens::blank(self.dark));
        tokens.dark = self.dark;
        let mut seen = HashSet::new();
        let mut warnings = Vec::new();
        for (name, value) in self.colors() {
            let Some(color) = Color::from_text(value) else {
                return Err(ThemeError::new(format!("{name} is no colour: {value:?}")));
            };
            if !seen.insert(name) {
                warnings.push(format!("{name} is written twice, the last one wins"));
            }
            if !tokens.set(name, color) {
                warnings.push(format!("{name} is no token of this version"));
            }
        }
        if parent.is_none() {
            let missing: Vec<&str> = TOKEN_NAMES
                .iter()
                .copied()
                .filter(|name| !seen.contains(name))
                .collect();
            if !missing.is_empty() {
                let shown = missing.iter().take(8).copied().collect::<Vec<_>>();
                let more = missing.len().saturating_sub(shown.len());
                let mut list = shown.join(", ");
                if more > 0 {
                    list.push_str(&format!(" and {more} more"));
                }
                return Err(ThemeError::new(format!(
                    "a theme without a parent has to name every token, missing: {list}"
                )));
            }
        }
        Ok((tokens, warnings))
    }

    /// The file as it is written into the settings folder.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_short_file_takes_the_missing_tokens_from_its_parent() {
        let text = r##"{
            "name": "Test",
            "dark": true,
            "parent": "dark",
            "ui": {"editor_bg": "#101010", "text": "#f0f0f0"},
            "syntax": {"keyword": "#ff8800"}
        }"##;
        let file = ThemeFile::parse(text).unwrap();
        assert_eq!(file.parent.as_deref(), Some("dark"));
        let (tokens, warnings) = file.resolve(Some(Tokens::dark())).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(tokens.editor_bg, Color::from_text("#101010").unwrap());
        assert_eq!(tokens.keyword, Color::from_text("#ff8800").unwrap());
        assert_eq!(
            tokens.string,
            Tokens::dark().string,
            "the parent keeps the rest"
        );
        assert!(tokens.dark);
    }

    #[test]
    fn a_file_without_a_parent_names_every_token() {
        let text = r##"{"name": "Short", "dark": true, "ui": {"editor_bg": "#101010"}}"##;
        let file = ThemeFile::parse(text).unwrap();
        let error = file.resolve(None).unwrap_err();
        assert!(
            error.message.contains("missing") && error.message.contains("main_bg"),
            "{error}"
        );
    }

    #[test]
    fn a_broken_file_says_what_is_wrong() {
        assert!(ThemeFile::parse("{").is_err());
        assert!(ThemeFile::parse(r#"{"name": "", "dark": true}"#).is_err());
        assert!(
            ThemeFile::parse(r##"{"name": "A", "dark": true, "ui": {"text": "#xy"}}"##).is_err()
        );
        let unknown = ThemeFile::parse(
            r##"{"name": "A", "dark": true, "parent": "dark", "ui": {"nonsense": "#ffffff"}}"##,
        )
        .unwrap();
        let (_, warnings) = unknown.resolve(Some(Tokens::dark())).unwrap();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("nonsense"));
    }
}
