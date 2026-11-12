//! The context menu of the editor: the actions of the keymap, with their keys.

use std::collections::HashMap;

use editor_view::{EditorView, Semantic};
use keymap::editor as actions;
use keymap::workspace as workspace_keys;
use ui_kit::*;
use workspace::Workspace;

/// `ctrl-shift-n` as the menus write it: `Ctrl+Shift+N`, or `⇧⌘N` on macOS.
fn shortcut_label(text: &str) -> String {
    keymap::shortcut_label(text, keymap::platform())
}

/// What the menu reads from the window before it is built, so that building it never
/// borrows the workspace that is already being updated.
pub(crate) struct MenuModel {
    can_run: bool,
    can_debug: bool,
    shortcuts: HashMap<&'static str, Option<SharedString>>,
}

impl MenuModel {
    pub(crate) fn of(workspace: &Workspace) -> Self {
        let ids = [
            actions::CUT,
            actions::COPY,
            actions::PASTE,
            actions::PASTE_HISTORY,
            actions::GOTO_DECLARATION,
            actions::GOTO_IMPLEMENTATION,
            actions::GOTO_SUPER,
            actions::FIND_USAGES,
            actions::RENAME,
            actions::CHANGE_SIGNATURE,
            actions::MOVE,
            actions::EXTRACT_METHOD,
            actions::EXTRACT_VARIABLE,
            actions::EXTRACT_CONSTANT,
            actions::EXTRACT_FIELD,
            actions::INTRODUCE_PARAMETER,
            actions::INLINE,
            actions::GENERATE,
            actions::REFORMAT,
            actions::OPTIMIZE_IMPORTS,
            actions::TOGGLE_COMMENT,
            workspace_keys::ROLLBACK_CHANGED_LINES,
            "workspace::Run",
            "workspace::Debug",
            "workspace::ShowCommit",
            "workspace::UpdateProject",
            "workspace::PushChanges",
            "workspace::ShowBranches",
        ];
        let shortcuts = ids
            .into_iter()
            .map(|id| {
                let text = workspace.shortcut_of(id).map(|text| shortcut_label(&text));
                (id, text.map(Into::into))
            })
            .collect();
        Self {
            can_run: workspace.can_run(),
            can_debug: workspace.can_debug(),
            shortcuts,
        }
    }

    fn key(&self, action: &str) -> Option<SharedString> {
        self.shortcuts.get(action).cloned().flatten()
    }
}

fn action_entry(
    label: &'static str,
    action: &'static str,
    enabled: bool,
    view: &Entity<EditorView>,
    shortcut: Option<SharedString>,
) -> MenuEntry {
    let entry = if enabled {
        let view = view.clone();
        MenuEntry::new(label, move |_, cx| {
            view.update(cx, |view, cx| view.run(action, cx));
        })
    } else {
        MenuEntry::disabled(label)
    };
    match shortcut {
        Some(shortcut) => entry.shortcut(shortcut),
        None => entry,
    }
}

fn dispatch_entry(
    label: &'static str,
    shortcut: Option<SharedString>,
    make: fn() -> Box<dyn ui_kit::gpui::Action>,
) -> MenuEntry {
    let entry = MenuEntry::new(label, move |window, cx| window.dispatch_action(make(), cx));
    match shortcut {
        Some(shortcut) => entry.shortcut(shortcut),
        None => entry,
    }
}

/// The menu of the editor at `position`, for the right click in the text.
pub(crate) fn show(
    view: Entity<EditorView>,
    workspace: WeakEntity<Workspace>,
    model: MenuModel,
    position: Point<Pixels>,
    cx: &mut App,
) {
    let entries = build(view.clone(), workspace.clone(), model, cx);
    workspace
        .update(cx, |workspace, cx| {
            workspace.show_menu(position, entries, cx)
        })
        .ok();
}

/// The same menu from a command of the window, where the workspace is at hand and is
/// already being updated.
pub(crate) fn show_now(
    workspace: &mut Workspace,
    view: Entity<EditorView>,
    weak: WeakEntity<Workspace>,
    model: MenuModel,
    position: Point<Pixels>,
    cx: &mut Context<Workspace>,
) {
    let entries = build(view, weak, model, cx);
    workspace.show_menu(position, entries, cx);
}

fn build(
    view: Entity<EditorView>,
    workspace: WeakEntity<Workspace>,
    model: MenuModel,
    cx: &mut App,
) -> Vec<MenuEntry> {
    let path = view.read(cx).path().to_path_buf();
    let support = cx
        .try_global::<Semantic>()
        .map(|semantic| semantic.0.clone());
    let matches = support
        .as_ref()
        .is_some_and(|support| support.matches(&path));
    let actions_available = matches && support.as_ref().is_some_and(|s| s.actions().is_some());
    let navigation = matches && support.as_ref().is_some_and(|s| s.navigation().is_some());
    let references = matches && support.as_ref().is_some_and(|s| s.references().is_some());
    let rename = matches && support.as_ref().is_some_and(|s| s.rename().is_some());
    let format = matches && support.as_ref().is_some_and(|s| s.format().is_some());
    let moves = matches
        && support
            .as_ref()
            .is_some_and(|s| s.move_destinations().is_some() && s.move_file().is_some());
    let paste = cx
        .read_from_clipboard()
        .and_then(|item| item.text())
        .is_some();
    let history = !editor_view::clipboard_history(cx).is_empty();
    let git = cx
        .try_global::<crate::git::ActiveGit>()
        .is_some_and(|git| git.0.read(cx).repo().is_some());

    let key = |action: &str| model.key(action);
    let entry = |label, action, enabled| action_entry(label, action, enabled, &view, key(action));
    let mut entries = vec![
        entry("Cut", actions::CUT, true),
        entry("Copy", actions::COPY, true),
        entry("Paste", actions::PASTE, paste),
        entry("Paste from History...", actions::PASTE_HISTORY, history),
        MenuEntry::separator(),
    ];

    let text = path.display().to_string();
    entries.push(MenuEntry::new("Copy Path", move |_, cx| {
        cx.write_to_clipboard(ClipboardItem::new_string(text.clone()))
    }));
    entries.push(MenuEntry::separator());

    entries.push(entry(
        "Go to Declaration",
        actions::GOTO_DECLARATION,
        navigation,
    ));
    entries.push(entry(
        "Go to Implementation",
        actions::GOTO_IMPLEMENTATION,
        navigation,
    ));
    entries.push(entry("Go to Super Method", actions::GOTO_SUPER, navigation));
    entries.push(entry("Find Usages", actions::FIND_USAGES, references));
    entries.push(MenuEntry::separator());

    entries.push(entry("Rename...", actions::RENAME, rename));
    entries.push(entry(
        "Change Signature...",
        actions::CHANGE_SIGNATURE,
        actions_available,
    ));
    entries.push(entry("Move...", actions::MOVE, moves));
    entries.push(entry(
        "Extract Method...",
        actions::EXTRACT_METHOD,
        actions_available,
    ));
    entries.push(entry(
        "Extract Variable...",
        actions::EXTRACT_VARIABLE,
        actions_available,
    ));
    entries.push(entry(
        "Extract Constant...",
        actions::EXTRACT_CONSTANT,
        actions_available,
    ));
    entries.push(entry(
        "Extract Field...",
        actions::EXTRACT_FIELD,
        actions_available,
    ));
    entries.push(entry(
        "Introduce Parameter...",
        actions::INTRODUCE_PARAMETER,
        actions_available,
    ));
    entries.push(entry("Inline...", actions::INLINE, actions_available));
    entries.push(MenuEntry::separator());

    entries.push(entry("Generate...", actions::GENERATE, actions_available));
    entries.push(MenuEntry::separator());
    entries.push(entry("Reformat Code", actions::REFORMAT, format));
    entries.push(entry(
        "Optimize Imports",
        actions::OPTIMIZE_IMPORTS,
        actions_available,
    ));
    entries.push(MenuEntry::separator());
    entries.push(entry("Comment Line", actions::TOGGLE_COMMENT, true));

    let (can_run, can_debug) = (model.can_run, model.can_debug);
    if can_run || can_debug {
        entries.push(MenuEntry::separator());
        if can_run {
            entries.push(dispatch_entry("Run", key("workspace::Run"), || {
                Box::new(workspace::Run)
            }));
        }
        if can_debug {
            entries.push(dispatch_entry("Debug", key("workspace::Debug"), || {
                Box::new(workspace::Debug)
            }));
        }
    }

    entries.push(MenuEntry::separator());
    let history_workspace = workspace.clone();
    entries.push(MenuEntry::new("Local History", move |window, cx| {
        history_workspace
            .update(cx, |workspace, cx| {
                workspace.run_command("local_history.show", window, cx);
            })
            .ok();
    }));

    if git {
        entries.push(MenuEntry::separator());
        entries.push(dispatch_entry(
            "Commit...",
            key("workspace::ShowCommit"),
            || Box::new(workspace::ShowCommit),
        ));
        entries.push(dispatch_entry(
            "Update Project",
            key("workspace::UpdateProject"),
            || Box::new(workspace::UpdateProject),
        ));
        entries.push(dispatch_entry(
            "Push...",
            key("workspace::PushChanges"),
            || Box::new(workspace::PushChanges),
        ));
        let revert = keymap::workspace::ROLLBACK_CHANGED_LINES;
        let runner = workspace.clone();
        let entry = MenuEntry::new("Revert Change", move |window, cx| {
            runner
                .update(cx, |workspace, cx| {
                    workspace.run_command(revert, window, cx)
                })
                .ok();
        });
        entries.push(match key(revert) {
            Some(shortcut) => entry.shortcut(shortcut),
            None => entry,
        });
        entries.push(dispatch_entry(
            "Branches...",
            key("workspace::ShowBranches"),
            || Box::new(workspace::ShowBranches),
        ));
    }

    entries
}
