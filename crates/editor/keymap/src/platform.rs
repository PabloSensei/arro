//! Which system's keyboard the window follows, and how a shortcut is written for it.
//!
//! The table of IntelliJ IDEA is a table per system: macOS moves most keys from Control to
//! Command, keeps Control for the ones IDEA keeps, and the hints write the modifiers as
//! glyphs. The value is process-wide because a window, its menus and its tooltips must all
//! agree; it starts as [`Platform::Windows`] so that a test presses the Windows keys on
//! every runner, and the application sets [`Platform::current`] once before the first
//! window opens.

use std::sync::atomic::{AtomicU8, Ordering};

/// The system whose keys a window follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Platform {
    Windows,
    Linux,
    MacOs,
}

impl Platform {
    /// The system this process runs on.
    pub fn current() -> Self {
        #[cfg(windows)]
        {
            Self::Windows
        }
        #[cfg(target_os = "macos")]
        {
            Self::MacOs
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            Self::Linux
        }
    }

    /// Whether Command is the primary modifier: the one that holds a key with a letter.
    pub fn uses_command(self) -> bool {
        self == Self::MacOs
    }
}

const WINDOWS: u8 = 0;
const LINUX: u8 = 1;
const MACOS: u8 = 2;

/// The platform of this process, [`Platform::Windows`] until something is chosen.
static CHOSEN: AtomicU8 = AtomicU8::new(WINDOWS);

/// The platform the keymap and the hints of this process use.
pub fn platform() -> Platform {
    match CHOSEN.load(Ordering::Relaxed) {
        LINUX => Platform::Linux,
        MACOS => Platform::MacOs,
        _ => Platform::Windows,
    }
}

/// Chooses the platform of this process. [`Platform::current`] before the first window.
pub fn set_platform(platform: Platform) {
    let value = match platform {
        Platform::Windows => WINDOWS,
        Platform::Linux => LINUX,
        Platform::MacOs => MACOS,
    };
    CHOSEN.store(value, Ordering::Relaxed);
}

/// A shortcut as a menu or a hint writes it: `Ctrl+Shift+F` on Windows and Linux, `⇧⌘F` on
/// macOS. A chord is two strokes, in IDEA's spelling with a comma between them.
pub fn shortcut_label(text: &str, platform: Platform) -> String {
    if let Some(modifier) = text.strip_prefix("double-") {
        return format!("Double {}", title(modifier));
    }
    text.split(' ')
        .map(|stroke| stroke_label(stroke, platform))
        .collect::<Vec<_>>()
        .join(", ")
}

/// One keystroke of a shortcut.
fn stroke_label(stroke: &str, platform: Platform) -> String {
    let mut ctrl = false;
    let mut alt = false;
    let mut shift = false;
    let mut cmd = false;
    let mut key = stroke;
    while let Some((part, rest)) = key.split_once('-') {
        // An empty tail is the minus key itself: `ctrl--` ends here.
        if rest.is_empty() {
            break;
        }
        match part {
            "ctrl" => ctrl = true,
            "alt" => alt = true,
            "shift" => shift = true,
            "cmd" => cmd = true,
            _ => break,
        }
        key = rest;
    }
    // The modifiers come in the order of the system's menus, whatever order the binding
    // was written in.
    let mut modifiers = String::new();
    for (on, windows, mac) in [
        (ctrl, "Ctrl+", "⌃"),
        (alt, "Alt+", "⌥"),
        (shift, "Shift+", "⇧"),
        (cmd, "Cmd+", "⌘"),
    ] {
        if on {
            modifiers.push_str(if platform.uses_command() {
                mac
            } else {
                windows
            });
        }
    }
    format!("{modifiers}{}", key_label(key, platform))
}

/// The name of the key itself, in the spelling of the system's menus.
fn key_label(key: &str, platform: Platform) -> String {
    if let Some(label) = numpad_label(key) {
        return label;
    }
    if !platform.uses_command() {
        return title(key);
    }
    match key {
        "backspace" => "⌫".to_owned(),
        "delete" => "⌦".to_owned(),
        "escape" => "⎋".to_owned(),
        "tab" => "⇥".to_owned(),
        "enter" => "↩".to_owned(),
        "up" => "↑".to_owned(),
        "down" => "↓".to_owned(),
        "left" => "←".to_owned(),
        "right" => "→".to_owned(),
        "pageup" => "⇞".to_owned(),
        "pagedown" => "⇟".to_owned(),
        "home" => "↖".to_owned(),
        "end" => "↘".to_owned(),
        other => title(other),
    }
}

/// The keys of the numeric keypad, in the spelling of the menus of IDEA: `NumPad -`,
/// `NumPad /`, `NumPad *`, `NumPad +` and `NumPad 0` to `NumPad 9`.
fn numpad_label(key: &str) -> Option<String> {
    let rest = key.strip_prefix("numpad")?;
    let rest = rest.strip_prefix('_').unwrap_or(rest);
    Some(match rest {
        "add" => "NumPad +".to_owned(),
        "subtract" => "NumPad -".to_owned(),
        "multiply" => "NumPad *".to_owned(),
        "divide" => "NumPad /".to_owned(),
        "decimal" => "NumPad .".to_owned(),
        digits if !digits.is_empty() && digits.chars().all(|digit| digit.is_ascii_digit()) => {
            format!("NumPad {digits}")
        }
        _ => return None,
    })
}

/// The first letter in capitals, as a menu writes a key name.
fn title(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{Platform, shortcut_label};

    #[test]
    fn a_numpad_key_is_written_as_idea_writes_it() {
        assert_eq!(
            shortcut_label("numpad_subtract", Platform::Windows),
            "NumPad -"
        );
        assert_eq!(
            shortcut_label("ctrl-numpad_divide", Platform::Windows),
            "Ctrl+NumPad /"
        );
        assert_eq!(
            shortcut_label("ctrl-numpad_multiply", Platform::Windows),
            "Ctrl+NumPad *"
        );
        assert_eq!(shortcut_label("numpad_add", Platform::Windows), "NumPad +");
        assert_eq!(shortcut_label("numpad0", Platform::Windows), "NumPad 0");
        assert_eq!(
            shortcut_label("ctrl-alt-numpad_subtract", Platform::Windows),
            "Ctrl+Alt+NumPad -"
        );
        assert_eq!(
            shortcut_label("ctrl-numpad_multiply numpad3", Platform::Windows),
            "Ctrl+NumPad *, NumPad 3"
        );
    }
}
