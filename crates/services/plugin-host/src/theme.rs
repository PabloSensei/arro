use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;

/// A theme file of a plugin: token names of the IDE theme with `#rrggbb` or `#rrggbbaa`.
/// Tokens it leaves out come from the built-in theme named by `dark`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginTheme {
    pub name: String,
    pub dark: bool,
    /// Token name to 0xRRGGBBAA.
    pub colors: BTreeMap<String, u32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ThemeFile {
    dark: bool,
    #[serde(default)]
    colors: BTreeMap<String, String>,
}

impl PluginTheme {
    pub fn read(name: &str, path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        Self::parse(name, &text).map_err(|error| format!("{}: {error}", path.display()))
    }

    pub fn parse(name: &str, text: &str) -> Result<Self, String> {
        let file: ThemeFile = toml::from_str(text).map_err(|error| error.message().to_owned())?;
        let mut colors = BTreeMap::new();
        for (token, value) in file.colors {
            let color =
                parse_color(&value).ok_or_else(|| format!("{token}: \"{value}\" is no #rrggbb"))?;
            colors.insert(token, color);
        }
        Ok(Self {
            name: name.to_owned(),
            dark: file.dark,
            colors,
        })
    }
}

fn parse_color(text: &str) -> Option<u32> {
    let hex = text.strip_prefix('#')?;
    let value = u32::from_str_radix(hex, 16).ok()?;
    match hex.len() {
        6 => Some(value << 8 | 0xff),
        8 => Some(value),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_with_and_without_alpha_are_read() {
        let theme = PluginTheme::parse(
            "T",
            "dark = true\n[colors]\neditor_bg = \"#102030\"\nselection = \"#11223344\"\n",
        )
        .unwrap();

        assert!(theme.dark);
        assert_eq!(theme.colors["editor_bg"], 0x102030ff);
        assert_eq!(theme.colors["selection"], 0x11223344);
    }

    #[test]
    fn a_bad_colour_names_its_token() {
        let error =
            PluginTheme::parse("T", "dark = false\n[colors]\ntext = \"red\"\n").unwrap_err();

        assert!(error.starts_with("text:"), "{error}");
    }
}
