use std::collections::HashMap;

use keymap::{ActionRegistry, Keymap, Shortcut, workspace};
use workspace_core::CommandRegistry;

use crate::PluginHost;

/// Puts the commands of the enabled plugins into the registry, so that Find Action shows
/// them, and binds their keys on top of the default keymap. A plugin binding of a key the
/// IDE uses wins. Returns what could not be bound, for the plugin list.
///
/// Ids are leaked to get the `&'static str` the registries keep: plugins load once per run
/// of the IDE. Unloading a plugin without a restart would need owned ids there.
pub fn install(
    host: &PluginHost,
    commands: &mut CommandRegistry,
    actions: &mut ActionRegistry,
    keymap: &mut Keymap,
) -> Vec<String> {
    let mut ids: HashMap<String, &'static str> = HashMap::new();
    let mut leak = |id: &str| {
        *ids.entry(id.to_owned())
            .or_insert_with(|| id.to_owned().leak())
    };
    let mut problems = Vec::new();
    for command in host.commands() {
        let id = leak(&command.id);
        commands.register(id, &command.title, workspace::CONTEXT);
        actions.register(&[id]);
        if let Some(key) = &command.key
            && let Err(error) = bind(actions, keymap, workspace::CONTEXT, key, id)
        {
            problems.push(format!("{}: {key}: {error}", command.id));
        }
    }
    for binding in host.keybindings() {
        let context = binding.context.as_deref().unwrap_or(workspace::CONTEXT);
        let id = leak(&binding.command);
        if let Err(error) = bind(actions, keymap, context, &binding.key, id) {
            problems.push(format!("{}: {}: {error}", binding.command, binding.key));
        }
    }
    problems
}

fn bind(
    actions: &ActionRegistry,
    keymap: &mut Keymap,
    context: &str,
    key: &str,
    action: &'static str,
) -> Result<(), keymap::KeymapError> {
    keymap.bind(actions, context, &Shortcut::parse(key)?, action)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use keymap::{Keystroke, Lookup, default_keymap};

    use super::*;
    use crate::{PluginSettings, Timeouts};

    #[test]
    fn plugin_commands_reach_the_registry_and_the_keymap() {
        let dir = std::env::temp_dir().join(format!("plugin-install-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("hello")).unwrap();
        let manifest = r#"
id = "hello"
name = "Hello"
version = "1"
api = 1
command = ["node", "main.js"]

[[contributes.commands]]
id = "upper-case"
title = "Upper Case Selection"
key = "ctrl-alt-shift-u"

[[contributes.keybindings]]
key = "ctrl-alt-shift-l"
command = "hello.upper-case"
context = "editor"

[[contributes.keybindings]]
key = "ctrl-alt-shift-x"
command = "no.such-command"
"#;
        fs::write(dir.join("hello/plugin.toml"), manifest).unwrap();
        let (host, _requests) =
            PluginHost::load(&dir, &PluginSettings::default(), Timeouts::default());
        let mut commands = CommandRegistry::new();
        let (mut actions, mut keymap) = default_keymap();

        let problems = install(&host, &mut commands, &mut actions, &mut keymap);

        let command = commands.get("hello.upper-case").unwrap();
        assert_eq!(command.title, "Upper Case Selection");
        let stroke = Keystroke::parse("ctrl-alt-shift-u").unwrap();
        assert_eq!(
            keymap.lookup(&[stroke], &[workspace::CONTEXT]),
            Lookup::Action("hello.upper-case")
        );
        let stroke = Keystroke::parse("ctrl-alt-shift-l").unwrap();
        assert_eq!(
            keymap.lookup(&[stroke], &["editor", workspace::CONTEXT]),
            Lookup::Action("hello.upper-case")
        );
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("unknown action"), "{problems:?}");
        let _ = fs::remove_dir_all(&dir);
    }
}
