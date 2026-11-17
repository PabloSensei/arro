//! Turns the Windows keymap of the keymap crate into the macOS one, with the rules of
//! `keymap::macos`. Keystrokes keep the crate's syntax: `cmd-shift-f`, `double-shift`, a
//! chord as two keystrokes with a space.

use std::fs;
use std::path::Path;

use serde_json::{Map, Value, json};

use crate::Result;

pub fn run(windows: &Path, macos: &Path) -> Result {
    let text = fs::read_to_string(windows)?;
    let shortcuts = keymap::idea_shortcuts(&text)?;
    let table = keymap::macos_table(&shortcuts)?;
    let mut actions = Map::new();
    for (id, keys) in table {
        let keys: Vec<Value> = keys.iter().map(|key| json!(key.to_string())).collect();
        actions.insert(id, Value::Array(keys));
    }
    let mut text = serde_json::to_string_pretty(&json!({ "actions": Value::Object(actions) }))?;
    text.push('\n');
    fs::write(macos, text)?;
    println!("{}", macos.display());
    Ok(())
}
