//! Commands by id, with what Find Action and menus show. The keymap binds keys to the
//! same ids, the window and agents run them by id.

use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandInfo {
    pub id: &'static str,
    pub title: String,
    /// Where it runs: the keymap context of its key bindings.
    pub context: &'static str,
    /// Runs by id but is not listed in Find Action and Search Everywhere, such as the
    /// buttons of a notification.
    pub hidden: bool,
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
        self.insert(id, title, context, false);
    }

    /// A command that runs by id but stays out of the lists of Find Action and Search
    /// Everywhere, for the buttons of a notification.
    pub fn register_hidden(
        &mut self,
        id: &'static str,
        title: impl Into<String>,
        context: &'static str,
    ) {
        self.insert(id, title, context, true);
    }

    fn insert(
        &mut self,
        id: &'static str,
        title: impl Into<String>,
        context: &'static str,
        hidden: bool,
    ) {
        let title = title.into();
        self.commands.insert(
            id,
            CommandInfo {
                id,
                title,
                context,
                hidden,
            },
        );
    }

    pub fn get(&self, id: &str) -> Option<&CommandInfo> {
        self.commands.get(id)
    }

    /// In id order.
    pub fn all(&self) -> impl Iterator<Item = &CommandInfo> {
        self.commands.values()
    }

    /// The commands Find Action and Search Everywhere list, in id order.
    pub fn visible(&self) -> impl Iterator<Item = &CommandInfo> {
        self.commands.values().filter(|command| !command.hidden)
    }

    pub fn len(&self) -> usize {
        self.commands.len()
    }

    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::CommandRegistry;

    #[test]
    fn a_hidden_command_runs_but_the_palette_does_not_list_it() {
        let mut commands = CommandRegistry::new();
        commands.register("test.visible", "Visible", "workspace");
        commands.register_hidden("test.hidden", "Hidden", "workspace");

        assert!(commands.get("test.hidden").is_some(), "the id runs");
        assert_eq!(
            commands
                .visible()
                .map(|command| command.id)
                .collect::<Vec<_>>(),
            ["test.visible"],
            "Find Action lists only the visible one"
        );
        assert_eq!(commands.all().count(), 2, "both are commands of the window");
    }
}
