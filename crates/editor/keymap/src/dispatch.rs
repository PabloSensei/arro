//! From key events to actions: chords, Double Shift and Double Ctrl. The window feeds
//! every key down and every change of the held modifiers.

use std::time::Instant;

use crate::{GestureRecognizer, Keymap, Keystroke, Lookup, Modifiers};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dispatch {
    Action(&'static str),
    /// The first keystroke of a chord: swallow it and wait for the second.
    Pending,
    Unhandled,
}

#[derive(Clone, Debug, Default)]
pub struct KeyDispatcher {
    pending: Option<Keystroke>,
    gestures: GestureRecognizer,
}

impl KeyDispatcher {
    pub fn new() -> Self {
        Self::default()
    }

    /// `contexts` from the focused element outwards.
    pub fn key_down(&mut self, keymap: &Keymap, stroke: Keystroke, contexts: &[&str]) -> Dispatch {
        self.gestures.key_down();
        if let Some(first) = self.pending.take() {
            // A chord that does not complete swallows its second key, as in IDEA.
            return match keymap.lookup(&[first, stroke], contexts) {
                Lookup::Action(action) => Dispatch::Action(action),
                _ => Dispatch::Unhandled,
            };
        }
        match keymap.lookup(std::slice::from_ref(&stroke), contexts) {
            Lookup::Action(action) => Dispatch::Action(action),
            Lookup::Prefix => {
                self.pending = Some(stroke);
                Dispatch::Pending
            }
            Lookup::None => Dispatch::Unhandled,
        }
    }

    pub fn modifiers_changed(&mut self, keymap: &Keymap, held: Modifiers, at: Instant) -> Dispatch {
        match self.gestures.modifiers_changed(held, at) {
            Some(modifier) => keymap
                .double(modifier)
                .map_or(Dispatch::Unhandled, Dispatch::Action),
            None => Dispatch::Unhandled,
        }
    }

    pub fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// Any other key breaks a double press, also when an editor or a popup consumed it
    /// before this dispatcher saw the key down.
    pub fn key_seen(&mut self) {
        self.gestures.key_down();
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::{ActionRegistry, Modifier, Shortcut};

    fn keymap() -> Keymap {
        let mut registry = ActionRegistry::default();
        registry.register(&["comment", "kill", "search"]);
        let mut keymap = Keymap::default();
        keymap
            .add(
                &registry,
                "editor",
                &[("ctrl-k ctrl-c", "comment"), ("ctrl-x", "kill")],
            )
            .unwrap();
        let double = Shortcut::Double(Modifier::Shift);
        keymap
            .bind(&registry, "workspace", &double, "search")
            .unwrap();
        keymap
    }

    fn stroke(text: &str) -> Keystroke {
        Keystroke::parse(text).unwrap()
    }

    #[test]
    fn a_chord_waits_for_its_second_keystroke() {
        let keymap = keymap();
        let mut keys = KeyDispatcher::new();
        let contexts = ["editor"];
        assert_eq!(
            keys.key_down(&keymap, stroke("ctrl-k"), &contexts),
            Dispatch::Pending
        );
        assert!(keys.is_pending());
        assert_eq!(
            keys.key_down(&keymap, stroke("ctrl-c"), &contexts),
            Dispatch::Action("comment")
        );

        keys.key_down(&keymap, stroke("ctrl-k"), &contexts);
        assert_eq!(
            keys.key_down(&keymap, stroke("ctrl-x"), &contexts),
            Dispatch::Unhandled
        );
        assert_eq!(
            keys.key_down(&keymap, stroke("ctrl-x"), &contexts),
            Dispatch::Action("kill")
        );
    }

    #[test]
    fn double_shift_runs_its_action() {
        let keymap = keymap();
        let mut keys = KeyDispatcher::new();
        let start = Instant::now();
        let shift = Modifiers {
            shift: true,
            ..Modifiers::default()
        };
        let mut last = Dispatch::Unhandled;
        for (step, held) in [shift, Modifiers::default(), shift, Modifiers::default()]
            .into_iter()
            .enumerate()
        {
            let at = start + Duration::from_millis(60 * (step as u64 + 1));
            last = keys.modifiers_changed(&keymap, held, at);
        }
        assert_eq!(last, Dispatch::Action("search"));
    }
}
