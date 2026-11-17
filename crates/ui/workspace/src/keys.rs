//! The keys of the window and of the panels, one row per binding.
//!
//! The Windows and Linux columns are the keys IDEA gives an action in its Windows
//! keymap. macOS takes its keys from `idea-default-macos.json` by the IDEA action id of
//! the row; a row without one writes the macOS key beside its Windows key.

use keymap::Platform;

/// One key of a table of the window or of a panel and the IDEA action it implements.
#[derive(Clone, Copy)]
pub struct KeyRow {
    /// The key on Windows and Linux.
    pub keys: &'static str,
    /// The key on macOS when the row implements no IDEA action, `None` to take the keys
    /// of `idea-default-macos.json`.
    pub mac: Option<&'static str>,
    /// Our action or command, `workspace::Run` for a window action.
    pub action: &'static str,
    /// The keymap context, `None` for a window key without a predicate.
    pub context: Option<&'static str>,
    /// The IDEA action of the row, when it has one.
    pub idea: Option<&'static str>,
}

impl KeyRow {
    /// The keys of this row on one platform. A row of an IDEA action takes every key the
    /// macOS file gives it; a row without the action uses the key written for macOS.
    pub fn keys_for(&self, platform: Platform) -> Vec<&'static str> {
        if !platform.uses_command() {
            return vec![self.keys];
        }
        if let Some(mac) = self.mac {
            return vec![mac];
        }
        match self.idea {
            Some(id) => keymap::macos_keys(id)
                .map(<[_]>::to_vec)
                .unwrap_or_default(),
            // A row of Windows-only keys keeps them; the check tells a missing macOS key.
            None => vec![self.keys],
        }
    }
}
