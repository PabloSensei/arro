//! Turns the IntelliJ IDEA keymap `$default.xml` into `idea-default.json` of the keymap
//! crate. Keystrokes come out in the crate's syntax: `ctrl-shift-n`, a chord as two
//! keystrokes with a space, `double-shift` for a modifier pressed twice. Mouse shortcuts
//! are left out.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde_json::{Map, Value, json};

use crate::Result;

const SOURCE: &str = "https://github.com/JetBrains/intellij-community/blob/master/platform/platform-resources/src/keymaps/$default.xml";

pub fn run(xml: &Path, json: &Path) -> Result {
    let text = fs::read_to_string(xml)?;
    let mut actions: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut skipped = Vec::new();
    let mut current: Option<String> = None;
    for tag in tags(&text) {
        match tag.name {
            "action" => {
                let id = attribute(tag.body, "id").ok_or("an action without an id")?;
                current = Some(id.to_owned());
            }
            "keyboard-shortcut" | "keyboard-gesture-shortcut" => {
                let id = current.as_ref().ok_or("a shortcut outside an action")?;
                match shortcut(tag.name, tag.body) {
                    Some(shortcut) => actions.entry(id.clone()).or_default().push(shortcut),
                    None => skipped.push(format!("{id}: <{} {}>", tag.name, tag.body.trim())),
                }
            }
            _ => {}
        }
    }
    for line in &skipped {
        eprintln!("skipped {line}");
    }
    let actions: Map<String, Value> = actions
        .into_iter()
        .map(|(id, keys)| (id, json!(keys)))
        .collect();
    let document = json!({
        "source": SOURCE,
        "license": "Apache-2.0",
        "actions": actions,
    });
    let mut output = serde_json::to_string_pretty(&document)?;
    output.push('\n');
    fs::write(json, output)?;
    println!(
        "{} actions written to {}",
        actions_len(&document),
        json.display()
    );
    Ok(())
}

fn actions_len(document: &Value) -> usize {
    document["actions"].as_object().map_or(0, Map::len)
}

struct Tag<'a> {
    name: &'a str,
    body: &'a str,
}

/// Opening tags of a simple XML file. Enough for a keymap: no CDATA, no `>` in values.
fn tags(text: &str) -> impl Iterator<Item = Tag<'_>> {
    text.split('<').skip(1).filter_map(|part| {
        let inner = part.split('>').next()?.trim_end_matches('/');
        if inner.starts_with(['/', '?', '!']) {
            return None;
        }
        let (name, body) = inner.split_once(char::is_whitespace).unwrap_or((inner, ""));
        Some(Tag { name, body })
    })
}

/// The value of `name="..."`, spaces around `=` allowed.
fn attribute<'a>(body: &'a str, name: &str) -> Option<&'a str> {
    let mut rest = body;
    while let Some(at) = rest.find(name) {
        let before = rest[..at].chars().next_back();
        let after = rest[at + name.len()..].trim_start();
        rest = &rest[at + name.len()..];
        if before.is_some_and(|c| !c.is_whitespace()) {
            continue;
        }
        let Some(value) = after.strip_prefix('=') else {
            continue;
        };
        let value = value.trim_start().strip_prefix('"')?;
        return Some(&value[..value.find('"')?]);
    }
    None
}

fn shortcut(tag: &str, body: &str) -> Option<String> {
    if tag == "keyboard-gesture-shortcut" {
        if attribute(body, "modifier")? != "dblClick" {
            return None;
        }
        let key = attribute(body, "keystroke")?.split_whitespace().last()?;
        let modifier = modifier(key)?;
        return Some(format!("double-{modifier}"));
    }
    let first = keystroke(attribute(body, "first-keystroke")?)?;
    match attribute(body, "second-keystroke") {
        Some(second) => Some(format!("{first} {}", keystroke(second)?)),
        None => Some(first),
    }
}

fn modifier(word: &str) -> Option<&'static str> {
    match word.to_lowercase().as_str() {
        "ctrl" | "control" => Some("ctrl"),
        "alt" => Some("alt"),
        "shift" => Some("shift"),
        "meta" => Some("meta"),
        _ => None,
    }
}

/// `shift control N` becomes `ctrl-shift-n`.
fn keystroke(text: &str) -> Option<String> {
    let words: Vec<&str> = text.split_whitespace().collect();
    let (key, modifiers) = words.split_last()?;
    let mut flags = [false; 4];
    for word in modifiers {
        let index = ["ctrl", "alt", "shift", "meta"]
            .iter()
            .position(|name| Some(*name) == modifier(word))?;
        flags[index] = true;
    }
    let mut stroke = String::new();
    for (on, name) in flags.iter().zip(["ctrl-", "alt-", "shift-", "meta-"]) {
        if *on {
            stroke.push_str(name);
        }
    }
    stroke.push_str(&key_name(key)?);
    Some(stroke)
}

/// Java `KeyEvent` names to the names of the keymap crate.
fn key_name(key: &str) -> Option<String> {
    let upper = key.to_uppercase();
    let named = match upper.as_str() {
        "BACK_SPACE" => "backspace",
        "PAGE_UP" => "pageup",
        "PAGE_DOWN" => "pagedown",
        "CONTEXT_MENU" => "menu",
        "MINUS" => "-",
        "EQUALS" => "=",
        "PERIOD" => ".",
        "COMMA" => ",",
        "SLASH" => "/",
        "BACK_SLASH" => "\\",
        "SEMICOLON" => ";",
        "QUOTE" => "'",
        "BACK_QUOTE" => "`",
        "OPEN_BRACKET" => "[",
        "CLOSE_BRACKET" => "]",
        "MULTIPLY" => "numpad_multiply",
        "ADD" => "numpad_add",
        "SUBTRACT" => "numpad_subtract",
        "DIVIDE" => "numpad_divide",
        "DECIMAL" => "numpad_decimal",
        "TAB" | "ENTER" | "ESCAPE" | "SPACE" | "INSERT" | "DELETE" | "HOME" | "END" | "UP"
        | "DOWN" | "LEFT" | "RIGHT" => return Some(upper.to_lowercase()),
        _ => {
            if let Some(digit) = upper.strip_prefix("NUMPAD") {
                return Some(format!("numpad{digit}"));
            }
            let simple = upper.len() == 1 && upper.chars().all(|c| c.is_ascii_alphanumeric());
            let function = upper.starts_with('F') && upper[1..].parse::<u8>().is_ok();
            if simple || function {
                return Some(upper.to_lowercase());
            }
            return None;
        }
    };
    Some(named.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keystrokes_take_the_crate_syntax() {
        assert_eq!(keystroke("shift control N").unwrap(), "ctrl-shift-n");
        assert_eq!(keystroke("control alt LEFT").unwrap(), "ctrl-alt-left");
        assert_eq!(
            keystroke("control SUBTRACT").unwrap(),
            "ctrl-numpad_subtract"
        );
        assert_eq!(keystroke("BACK_SPACE").unwrap(), "backspace");
        assert_eq!(keystroke("control BACK_SLASH").unwrap(), "ctrl-\\");
        assert_eq!(keystroke("alt Enter").unwrap(), "alt-enter");
        assert_eq!(keystroke("control button1"), None);
    }

    #[test]
    fn attributes_allow_spaces() {
        assert_eq!(attribute(r#"id ="A.B""#, "id"), Some("A.B"));
        assert_eq!(attribute(r#" first-keystroke="X" id="Y""#, "id"), Some("Y"));
        assert_eq!(attribute(r#"keystroke="X""#, "stroke"), None);
    }

    #[test]
    fn chords_and_gestures_are_read() {
        let chord = r#"first-keystroke="control K" second-keystroke="control C""#;
        assert_eq!(
            shortcut("keyboard-shortcut", chord).unwrap(),
            "ctrl-k ctrl-c"
        );
        let gesture = r#"keystroke="shift SHIFT" modifier="dblClick""#;
        assert_eq!(
            shortcut("keyboard-gesture-shortcut", gesture).unwrap(),
            "double-shift"
        );
    }
}
