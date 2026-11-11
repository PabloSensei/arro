use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// The protocol version this host speaks.
pub const API_VERSION: u32 = 1;
pub const MANIFEST_FILE: &str = "plugin.toml";

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub api: u32,
    /// The process and its arguments, started in the plugin folder. A plugin with only a
    /// theme or key bindings has none.
    #[serde(default)]
    pub command: Option<Vec<String>>,
    #[serde(default)]
    pub contributes: Contributes,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Contributes {
    #[serde(default)]
    pub commands: Vec<CommandContribution>,
    #[serde(default)]
    pub themes: Vec<ThemeContribution>,
    #[serde(default)]
    pub keybindings: Vec<KeyBinding>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CommandContribution {
    /// Unique within the plugin. The IDE knows it as `<plugin id>.<id>`.
    pub id: String,
    pub title: String,
    /// A default key binding, `ctrl-alt-u` in keymap syntax.
    #[serde(default)]
    pub key: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ThemeContribution {
    pub name: String,
    /// Relative to the plugin folder.
    pub file: PathBuf,
}

/// Binds a key to any command, the IDE's own or a plugin's, on top of the default keymap.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct KeyBinding {
    pub key: String,
    pub command: String,
    /// The keymap context, the whole window when absent.
    #[serde(default)]
    pub context: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManifestError(pub String);

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ManifestError {}

impl Manifest {
    /// Reads `plugin.toml` of a plugin folder. The folder must be named after the id, so
    /// that two folders never claim one plugin.
    pub fn read(dir: &Path) -> Result<Self, ManifestError> {
        let path = dir.join(MANIFEST_FILE);
        let text = std::fs::read_to_string(&path)
            .map_err(|error| ManifestError(format!("{}: {error}", path.display())))?;
        let manifest = Self::parse(&text)?;
        let folder = dir.file_name().map(|name| name.to_string_lossy());
        if folder.as_deref() != Some(manifest.id.as_str()) {
            return Err(ManifestError(format!(
                "the folder {} must be named after the id \"{}\"",
                dir.display(),
                manifest.id
            )));
        }
        for theme in &manifest.contributes.themes {
            if !is_inside(&theme.file) {
                return Err(ManifestError(format!(
                    "theme \"{}\": the file must lie inside the plugin folder",
                    theme.name
                )));
            }
        }
        Ok(manifest)
    }

    pub fn parse(text: &str) -> Result<Self, ManifestError> {
        let manifest: Self = toml::from_str(text)
            .map_err(|error| ManifestError(format!("{MANIFEST_FILE}: {}", error.message())))?;
        manifest.validate()?;
        Ok(manifest)
    }

    fn validate(&self) -> Result<(), ManifestError> {
        let fail = |message: String| Err(ManifestError(message));
        if !is_name(&self.id) {
            return fail(format!(
                "id \"{}\": use lowercase letters, digits and '-'",
                self.id
            ));
        }
        if self.api != API_VERSION {
            return fail(format!(
                "api {} is not supported, this Arro build speaks {API_VERSION}",
                self.api
            ));
        }
        if matches!(&self.command, Some(command) if command.is_empty()) {
            return fail("command is empty".into());
        }
        let commands = &self.contributes.commands;
        if self.command.is_none() && !commands.is_empty() {
            return fail("commands need a process: set command".into());
        }
        for (index, command) in commands.iter().enumerate() {
            if !is_name(&command.id) {
                return fail(format!(
                    "command id \"{}\": use lowercase letters, digits and '-'",
                    command.id
                ));
            }
            if commands[..index].iter().any(|other| other.id == command.id) {
                return fail(format!("command id \"{}\" is there twice", command.id));
            }
        }
        Ok(())
    }

    /// The id the IDE registers a command of this plugin under.
    pub fn command_id(&self, command: &str) -> String {
        format!("{}.{command}", self.id)
    }
}

fn is_name(text: &str) -> bool {
    let mut chars = text.chars();
    chars.next().is_some_and(|first| first.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn is_inside(path: &Path) -> bool {
    use std::path::Component;
    path.components()
        .all(|component| matches!(component, Component::Normal(_) | Component::CurDir))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = r#"
id = "hello"
name = "Hello"
version = "0.1.0"
api = 1
command = ["node", "main.js"]

[[contributes.commands]]
id = "upper-case"
title = "Upper Case Selection"
key = "ctrl-alt-u"

[[contributes.themes]]
name = "Hello Dark"
file = "themes/dark.toml"

[[contributes.keybindings]]
key = "ctrl-alt-l"
command = "hello.upper-case"
context = "editor"
"#;

    #[test]
    fn a_full_manifest_is_read() {
        let manifest = Manifest::parse(FULL).unwrap();

        assert_eq!(manifest.command.as_deref().unwrap(), ["node", "main.js"]);
        assert_eq!(
            manifest.contributes.commands[0].key.as_deref(),
            Some("ctrl-alt-u")
        );
        assert_eq!(
            manifest.contributes.themes[0].file,
            Path::new("themes/dark.toml")
        );
        assert_eq!(
            manifest.contributes.keybindings[0].context.as_deref(),
            Some("editor")
        );
        assert_eq!(manifest.command_id("upper-case"), "hello.upper-case");
    }

    #[test]
    fn mistakes_are_told_plainly() {
        let cases = [
            (FULL.replace("api = 1", "api = 2"), "api 2 is not supported"),
            (
                FULL.replace("id = \"hello\"", "id = \"Hello World\""),
                "id \"Hello World\"",
            ),
            (
                FULL.replace("command = [\"node\", \"main.js\"]", ""),
                "commands need a process",
            ),
            (
                FULL.replace("version = \"0.1.0\"", ""),
                "missing field `version`",
            ),
            (
                FULL.replace("api = 1", "api = 1\nextra = true"),
                "unknown field `extra`",
            ),
        ];
        for (text, expected) in cases {
            let error = Manifest::parse(&text).unwrap_err();

            assert!(error.0.contains(expected), "{error}");
        }
    }

    #[test]
    fn a_theme_file_outside_the_folder_is_refused() {
        assert!(is_inside(Path::new("themes/dark.toml")));
        assert!(!is_inside(Path::new("../other/dark.toml")));
        assert!(!is_inside(Path::new("C:/Windows/dark.toml")));
    }
}
