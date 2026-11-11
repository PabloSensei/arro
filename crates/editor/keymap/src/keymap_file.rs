//! The keymap file of the user: what a changed shortcut adds to or removes from the
//! default layout. One JSON file, versioned, with an entry per action. A broken file is
//! reported and left alone: overwriting it would lose what the user wrote.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{Map, Value};

use crate::{ActionRegistry, Keymap, KeymapError, Shortcut};

/// The file in the settings folder.
pub const KEYMAP_FILE: &str = "keymap.json";
/// The format of the file this build reads and writes.
pub const KEYMAP_VERSION: u64 = 1;

/// What one action adds to or removes from the default layout.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KeymapAction {
    /// Shortcuts that work in addition to the defaults.
    pub add: Vec<String>,
    /// Default shortcuts that no longer work.
    pub remove: Vec<String>,
}

impl KeymapAction {
    fn is_empty(&self) -> bool {
        self.add.is_empty() && self.remove.is_empty()
    }
}

/// The shortcuts of the user, over the default layout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeymapFile {
    pub version: u64,
    pub actions: BTreeMap<String, KeymapAction>,
}

impl Default for KeymapFile {
    fn default() -> Self {
        Self {
            version: KEYMAP_VERSION,
            actions: BTreeMap::new(),
        }
    }
}

/// The parsed file and what the reader skipped.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LoadedKeymap {
    pub file: KeymapFile,
    pub warnings: Vec<String>,
}

/// Whether an edit takes a shortcut away or gives the action one more.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeymapChange {
    Add,
    Remove,
}

impl KeymapFile {
    /// Reads the file of the user. A missing file is an empty keymap; a broken or a
    /// newer one is an error, so that the caller leaves it alone. Actions the registry
    /// does not know and shortcuts that do not parse are skipped with a warning.
    pub fn load(path: &Path, registry: &ActionRegistry) -> Result<LoadedKeymap, KeymapError> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(LoadedKeymap::default());
            }
            Err(error) => {
                return Err(KeymapError::BadKeymapFile(format!(
                    "{}: {error}",
                    path.display()
                )));
            }
        };
        Self::parse(&text, registry)
    }

    /// Parses the text of a keymap file.
    pub fn parse(text: &str, registry: &ActionRegistry) -> Result<LoadedKeymap, KeymapError> {
        let bad = |message: String| KeymapError::BadKeymapFile(message);
        let value: Value = serde_json::from_str(text).map_err(|error| bad(error.to_string()))?;
        let object = value
            .as_object()
            .ok_or_else(|| bad("the file is not an object".to_owned()))?;
        let version = match object.get("version") {
            None => KEYMAP_VERSION,
            Some(Value::Number(number)) => number.as_u64().unwrap_or_default(),
            Some(_) => return Err(bad("version is not a number".to_owned())),
        };
        if version != KEYMAP_VERSION {
            return Err(bad(format!(
                "version {version} is not the version {KEYMAP_VERSION} of this build"
            )));
        }
        let mut file = Self {
            version,
            actions: BTreeMap::new(),
        };
        let mut warnings = Vec::new();
        let Some(actions) = object.get("actions") else {
            return Ok(LoadedKeymap { file, warnings });
        };
        let actions = actions
            .as_object()
            .ok_or_else(|| bad("actions is not an object".to_owned()))?;
        for (action, spec) in actions {
            if !registry.contains(action) {
                warnings.push(format!("unknown action {action} is skipped"));
                continue;
            }
            let Some(spec) = spec.as_object() else {
                warnings.push(format!("{action}: the entry is not an object"));
                continue;
            };
            let mut edit = KeymapAction::default();
            for (field, list) in [("add", &mut edit.add), ("remove", &mut edit.remove)] {
                let Some(values) = spec.get(field) else {
                    continue;
                };
                let Some(values) = values.as_array() else {
                    warnings.push(format!("{action}: {field} is not a list"));
                    continue;
                };
                for value in values {
                    let Some(text) = value.as_str() else {
                        warnings.push(format!("{action}: {field} holds a value that is not text"));
                        continue;
                    };
                    match Shortcut::parse(text) {
                        Ok(shortcut) => list.push(shortcut.to_string()),
                        Err(error) => warnings.push(format!("{action}: {text}: {error}")),
                    }
                }
            }
            if !edit.is_empty() {
                file.actions.insert(action.clone(), edit);
            }
        }
        Ok(LoadedKeymap { file, warnings })
    }

    /// Writes the file, creating its folder. Blocks: not for the UI thread.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, self.to_text())
    }

    /// The JSON of the file, as it is written.
    pub fn to_text(&self) -> String {
        let mut actions = Map::new();
        for (action, edit) in &self.actions {
            if edit.is_empty() {
                continue;
            }
            let mut spec = Map::new();
            if !edit.add.is_empty() {
                spec.insert(
                    "add".to_owned(),
                    Value::Array(edit.add.iter().cloned().map(Value::String).collect()),
                );
            }
            if !edit.remove.is_empty() {
                spec.insert(
                    "remove".to_owned(),
                    Value::Array(edit.remove.iter().cloned().map(Value::String).collect()),
                );
            }
            actions.insert(action.clone(), Value::Object(spec));
        }
        let root = Value::Object(Map::from_iter([
            ("version".to_owned(), Value::from(self.version)),
            ("actions".to_owned(), Value::Object(actions)),
        ]));
        serde_json::to_string_pretty(&root).expect("a keymap file is JSON")
    }

    /// Whether the action has a shortcut of its own in this file.
    pub fn is_customized(&self, action: &str) -> bool {
        self.actions.contains_key(action)
    }

    /// Gives the action one more shortcut. A shortcut it already has in the default
    /// layout only stops being removed.
    pub fn add_shortcut(&mut self, action: &str, shortcut: &Shortcut) {
        let text = shortcut.to_string();
        let edit = self.actions.entry(action.to_owned()).or_default();
        edit.remove.retain(|removed| *removed != text);
        if !edit.add.contains(&text) {
            edit.add.push(text);
        }
        self.drop_empty(action);
    }

    /// Takes a shortcut away from the action. One that the file added is dropped from
    /// the additions; a default one is listed as removed.
    pub fn remove_shortcut(&mut self, action: &str, shortcut: &Shortcut) {
        let text = shortcut.to_string();
        let edit = self.actions.entry(action.to_owned()).or_default();
        if edit.add.contains(&text) {
            edit.add.retain(|added| *added != text);
        } else if !edit.remove.contains(&text) {
            edit.remove.push(text);
        }
        self.drop_empty(action);
    }

    /// Drops every change of one action: its defaults come back.
    pub fn reset_action(&mut self, action: &str) -> bool {
        self.actions.remove(action).is_some()
    }

    /// Drops every change: the whole default layout comes back.
    pub fn reset_all(&mut self) -> bool {
        let changed = !self.actions.is_empty();
        self.actions.clear();
        changed
    }

    /// The default layout with the changes of this file applied.
    pub fn apply(&self, registry: &ActionRegistry, base: &Keymap) -> Result<Keymap, KeymapError> {
        let mut keymap = base.clone();
        for (action, edit) in &self.actions {
            let Some(context) = registry.context(action) else {
                continue;
            };
            let Some(name) = registry.name(action) else {
                continue;
            };
            for text in &edit.remove {
                let shortcut = Shortcut::parse(text)?;
                keymap.remove_shortcut(context, &shortcut, name);
            }
        }
        for (action, edit) in &self.actions {
            let Some(context) = registry.context(action) else {
                continue;
            };
            let Some(name) = registry.name(action) else {
                continue;
            };
            for text in &edit.add {
                let shortcut = Shortcut::parse(text)?;
                keymap.bind(registry, context, &shortcut, name)?;
            }
        }
        Ok(keymap)
    }

    fn drop_empty(&mut self, action: &str) {
        if self.actions.get(action).is_some_and(KeymapAction::is_empty) {
            self.actions.remove(action);
        }
    }
}
