//! The IntelliJ IDEA keymap as data. See `keymaps/NOTICE.md` for where it comes from.

use std::collections::HashMap;

use serde_json::Value;

use crate::{KeymapError, Shortcut};

pub const IDEA_DEFAULT: &str = include_str!("../keymaps/idea-default.json");

/// The same table for macOS: Command where Windows has Control, and the keys IDEA keeps on
/// Control. Generated from [`IDEA_DEFAULT`] by `cargo xtask macos-keymap`.
pub const IDEA_DEFAULT_MACOS: &str = include_str!("../keymaps/idea-default-macos.json");

/// One deliberate difference from IDEA's default Windows keymap.
#[derive(Clone, Copy, Debug)]
pub struct Deviation {
    /// The keymap context: `editor`, `workspace`, `global`, or a panel context.
    pub context: &'static str,
    /// Our keys, `""` when the IDEA action has no key here at all.
    pub keys: &'static str,
    /// The IDEA action whose slot we take, when there is one.
    pub idea_action: Option<&'static str>,
    /// Why the difference is intentional.
    pub reason: &'static str,
}

/// The differences from IDEA that we keep on purpose. Everything else must match
/// `idea-default.json`; the keymap and window tests read this list.
pub const DEVIATIONS: &[Deviation] = &[
    Deviation {
        context: "global",
        keys: "alt-2",
        idea_action: Some("ActivateBookmarksToolWindow"),
        reason: "Bookmarks live in the editor (F11); the AI assistant uses the tool window slot.",
    },
    Deviation {
        context: "global",
        keys: "ctrl-alt-shift-left",
        idea_action: Some("MoveElementLeft"),
        reason: "Moves the selected element left in the editor; outside the editor it moves focus to the previous split group. Tool windows are resized by dragging.",
    },
    Deviation {
        context: "global",
        keys: "ctrl-alt-shift-right",
        idea_action: Some("MoveElementRight"),
        reason: "Moves the selected element right in the editor; outside the editor it moves focus to the next split group. Tool windows are resized by dragging.",
    },
    Deviation {
        context: "global",
        keys: "escape",
        idea_action: Some("EditorEscape"),
        reason: "Escape closes a popup first and otherwise propagates to the focused view.",
    },
];

/// The deviation for a key, if the difference is intentional.
pub fn deviation_for(
    context: &str,
    keys: &str,
    idea_action: Option<&str>,
) -> Option<&'static Deviation> {
    DEVIATIONS.iter().find(|deviation| {
        deviation.context == context
            && deviation.keys == keys
            && deviation.idea_action == idea_action
    })
}

/// IDEA actions that MVP-1 does not have, with the reason each one is out. Every other
/// action of `idea-default.json` is one of our bindings or a deliberate deviation.
pub const NOT_IN_MVP: &[(&str, &str)] = &[
    (
        "ActivateNuGetToolWindow",
        "NuGet is not a Java tool window.",
    ),
    (
        "ActivateUnitTestsToolWindow",
        "Tests run in the Run tool window; MVP-1 has no separate unit test window.",
    ),
    (
        "ChangeTypeSignature",
        "Changing a type signature is not in MVP-1; Change Signature is.",
    ),
    ("CloseGotItTooltip", "MVP-1 has no tooltips of this kind."),
    (
        "CodeFloatingToolbar.GotoNextMenu",
        "The floating code toolbar is not in MVP-1.",
    ),
    (
        "CodeFloatingToolbar.GotoPrevMenu",
        "The floating code toolbar is not in MVP-1.",
    ),
    (
        "CollapseExpandableComponent",
        "A generic expandable component key is not in MVP-1; trees have their own keys.",
    ),
    ("ContextHelp", "MVP-1 has no in-product help."),
    (
        "Diff.ShowSettingsPopup",
        "The diff shows context lines always; settings popup is not in MVP-1.",
    ),
    (
        "DumpLookupElementWeights",
        "A developer tool of IDEA, not a product action.",
    ),
    (
        "DuplicatesForm.SendToLeft",
        "The duplicates form is not in MVP-1.",
    ),
    (
        "DuplicatesForm.SendToRight",
        "The duplicates form is not in MVP-1.",
    ),
    (
        "EditSourceInNewWindow",
        "MVP-1 opens a file in a tab or a split, not in a new window.",
    ),
    (
        "EditorFocusGutter",
        "The gutter needs no focus of its own in MVP-1.",
    ),
    (
        "EditorShowGutterIconTooltip",
        "MVP-1 has no gutter icon tooltips.",
    ),
    (
        "ExpandExpandableComponent",
        "A generic expandable component key is not in MVP-1; trees have their own keys.",
    ),
    (
        "FileChooser.GoToParent",
        "The folder chooser is the native Windows dialog.",
    ),
    (
        "FileChooser.GoToRoot",
        "The folder chooser is the native Windows dialog.",
    ),
    (
        "FileChooser.GotoDesktop",
        "The folder chooser is the native Windows dialog.",
    ),
    (
        "FileChooser.GotoHome",
        "The folder chooser is the native Windows dialog.",
    ),
    (
        "FileChooser.GotoModule",
        "The folder chooser is the native Windows dialog.",
    ),
    (
        "FileChooser.GotoProject",
        "The folder chooser is the native Windows dialog.",
    ),
    (
        "FileChooser.NewFolder",
        "The folder chooser is the native Windows dialog.",
    ),
    (
        "FileChooser.TogglePathBar",
        "The folder chooser is the native Windows dialog.",
    ),
    (
        "MainMenuButton.ShowMenu",
        "The main menu opens from the window toolbar.",
    ),
    (
        "MoveToolWindowTabToEditorAction",
        "Tool windows have no tabs of their own in MVP-1.",
    ),
    (
        "PopupHector",
        "The HECTOR lookup is a developer tool, not a product action.",
    ),
    (
        "QuickActionPopup",
        "Ctrl+Alt+Enter starts a line; no quick action popup is in MVP-1.",
    ),
    (
        "SelectVirtualTemplateElement",
        "Virtual template elements are not in MVP-1.",
    ),
    (
        "ServiceView.GroupByContributor",
        "The Services window has one flat list.",
    ),
    (
        "ServiceView.ShowServices",
        "The Services window lists what it has; no filter action.",
    ),
    ("SwitchCoverage", "Coverage is not in MVP-1."),
    ("SwitchHeaderSource", "Java has no header and source pair."),
    ("TextSearchAction", "Search Everywhere covers text search."),
    (
        "Vcs.MoveChangedLinesToChangelist",
        "Changelists are not in MVP-1.",
    ),
    ("ViewSource", "The navigation popups open source directly."),
    (
        "XDebugger.JumpToTypeSource",
        "The debugger opens a type from the variables tree; no key.",
    ),
];

/// IDEA action id to its shortcuts, from a file in the `idea-default.json` format.
pub fn idea_shortcuts(json: &str) -> Result<HashMap<String, Vec<Shortcut>>, KeymapError> {
    let bad = |message: &str| KeymapError::BadKeymapFile(message.to_owned());
    let document: Value = serde_json::from_str(json).map_err(|error| bad(&error.to_string()))?;
    let actions = document["actions"]
        .as_object()
        .ok_or_else(|| bad("no actions"))?;
    let mut shortcuts = HashMap::with_capacity(actions.len());
    for (id, keys) in actions {
        let keys = keys.as_array().ok_or_else(|| bad(id))?;
        let parsed = keys
            .iter()
            .map(|key| Shortcut::parse(key.as_str().ok_or_else(|| bad(id))?))
            .collect::<Result<Vec<_>, _>>()?;
        shortcuts.insert(id.clone(), parsed);
    }
    Ok(shortcuts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_shortcut_of_the_idea_file_parses() {
        let shortcuts = idea_shortcuts(IDEA_DEFAULT).unwrap();
        assert!(shortcuts.len() > 300);
        assert!(shortcuts.values().all(|keys| !keys.is_empty()));
    }

    #[test]
    fn the_idea_ids_of_the_tables_exist_in_the_file() {
        let shortcuts = idea_shortcuts(IDEA_DEFAULT).unwrap();
        let tables = crate::editor::IDEA_IDS
            .iter()
            .chain(crate::editor::HANDLED_IDS)
            .chain(crate::workspace::IDEA_IDS)
            .chain(crate::workspace::EXTRA_IDEA_IDS);
        for (id, _) in tables {
            assert!(shortcuts.contains_key(*id), "{id}");
        }
        for (id, reason) in NOT_IN_MVP {
            assert!(shortcuts.contains_key(*id), "{id}");
            assert!(!reason.is_empty(), "{id}");
            assert!(
                !crate::editor::IDEA_IDS.iter().any(|(known, _)| known == id),
                "{id} is implemented and missed"
            );
            assert!(
                !crate::workspace::IDEA_IDS
                    .iter()
                    .any(|(known, _)| known == id),
                "{id} is implemented and missed"
            );
        }
    }

    #[test]
    fn not_in_mvp_actions_are_known_only_once() {
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for (id, _) in NOT_IN_MVP {
            assert!(seen.insert(id), "{id} is listed twice");
        }
    }
}
