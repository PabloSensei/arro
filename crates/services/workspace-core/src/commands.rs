//! Commands by id, with what Find Action and menus show. The keymap binds keys to the
//! same ids, the window and agents run them by id.

use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandInfo {
    pub id: &'static str,
    pub title: String,
    /// Where it runs: the keymap context of its key bindings.
    pub context: &'static str,
}

#[derive(Clone, Debug, Default)]
pub struct CommandRegistry {
    commands: BTreeMap<&'static str, CommandInfo>,
}

impl CommandRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// A later registration of the same id replaces the earlier one.
    pub fn register(&mut self, id: &'static str, title: impl Into<String>, context: &'static str) {
        let title = title.into();
        self.commands.insert(id, CommandInfo { id, title, context });
    }

    pub fn get(&self, id: &str) -> Option<&CommandInfo> {
        self.commands.get(id)
    }

    /// In id order.
    pub fn all(&self) -> impl Iterator<Item = &CommandInfo> {
        self.commands.values()
    }

    pub fn len(&self) -> usize {
        self.commands.len()
    }

    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }
}
