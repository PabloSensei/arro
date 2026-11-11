//! The actions of the whole window: tabs of the tool windows, the saved layout, the
//! scale of the interface, the file path, the navigation bar and scratch files.

use keymap::workspace as keys;
use ui_kit::*;
use workspace::{UI_SCALE_STEP, Workspace};

use crate::call_hierarchy::ActiveCallHierarchy;
use crate::method_hierarchy::ActiveMethodHierarchy;
use crate::project::ActiveProjectPanel;
use crate::window_popups;

/// Registers the commands of the window. They run from the keymap, Find Action and the
/// menus once the panels are in place.
pub fn register(workspace: &mut Workspace, _window: &mut Window, cx: &mut Context<Workspace>) {
    // The Scratches folder of the tree stays empty until its first file.
    let state_dir = workspace.state_dir().to_path_buf();
    cx.background_executor()
        .spawn(async move { std::fs::create_dir_all(state_dir.join("scratches")) })
        .detach();

    workspace.register_command(
        keys::CLOSE_ACTIVE_TAB,
        "Close Active Tab",
        |workspace, window, cx| workspace.close_active_tool_tab(window, cx),
    );
    workspace.register_command(
        keys::NEXT_EDITOR_TAB,
        "Next Editor Tab",
        |workspace, window, cx| workspace.next_tab(window, cx),
    );
    workspace.register_command(
        keys::PREVIOUS_EDITOR_TAB,
        "Previous Editor Tab",
        |workspace, window, cx| workspace.previous_tab(window, cx),
    );
    workspace.register_command(
        keys::OPEN_IN_RIGHT_SPLIT,
        "Open in Right Split",
        |workspace, window, cx| {
            if let Some(path) = workspace.active_path() {
                workspace.open_file_in_split(path, false, window, cx);
            }
        },
    );
    workspace.register_command(keys::SAVE_AS, "Save As...", |workspace, window, cx| {
        save_as(workspace, window, cx)
    });
    workspace.register_command(
        keys::HIDE_ALL_WINDOWS,
        "Hide All Windows",
        |workspace, window, cx| workspace.toggle_all_windows(window, cx),
    );
    workspace.register_command(
        keys::MAXIMIZE_TOOL_WINDOW,
        "Maximize Tool Window",
        |workspace, window, cx| workspace.toggle_maximize_tool_window(window, cx),
    );
    workspace.register_command(
        keys::JUMP_TO_LAST_WINDOW,
        "Jump to Last Window",
        |workspace, window, cx| workspace.jump_to_last_window(window, cx),
    );
    workspace.register_command(
        keys::RESTORE_DEFAULT_LAYOUT,
        "Restore Default Layout",
        |workspace, window, cx| workspace.restore_default_layout(window, cx),
    );
    workspace.register_command(
        keys::FOCUS_EDITOR,
        "Focus Editor",
        |workspace, window, cx| workspace.focus_editor(window, cx),
    );
    workspace.register_command(keys::ZOOM_IN_IDE, "Zoom In Arro", |workspace, _, cx| {
        workspace.zoom_ide(UI_SCALE_STEP, cx)
    });
    workspace.register_command(keys::ZOOM_OUT_IDE, "Zoom Out Arro", |workspace, _, cx| {
        workspace.zoom_ide(-UI_SCALE_STEP, cx)
    });
    workspace.register_command(
        keys::RESET_IDE_SCALE,
        "Reset Arro Scale",
        |workspace, _, cx| workspace.reset_ide_scale(cx),
    );
    workspace.register_command(
        keys::EDITOR_FONT_INCREASE,
        "Increase Editor Font Size",
        |workspace, _, cx| workspace.change_editor_font(1., cx),
    );
    workspace.register_command(
        keys::EDITOR_FONT_DECREASE,
        "Decrease Editor Font Size",
        |workspace, _, cx| workspace.change_editor_font(-1., cx),
    );
    workspace.register_command(
        keys::SHOW_FILE_PATH,
        "Show File Path",
        window_popups::show_file_path,
    );
    workspace.register_command(
        keys::SHOW_NAV_BAR,
        "Show Navigation Bar",
        window_popups::show_nav_bar,
    );
    workspace.register_command(keys::SHOW_POPUP_MENU, "Show Context Menu", show_popup_menu);
    workspace.register_command(
        keys::QUICK_CHANGE_SCHEME,
        "Quick Change Scheme",
        window_popups::show_change_scheme,
    );
    workspace.register_command(
        keys::NEW_SCRATCH_FILE,
        "New Scratch File",
        window_popups::show_scratch_languages,
    );
    for (id, title, node) in [
        (keys::EXPAND_ALL, "Expand All", false),
        (keys::COLLAPSE_ALL, "Collapse All", false),
        (keys::EXPAND_TREE_NODE, "Expand Tree Node", true),
        (keys::COLLAPSE_TREE_NODE, "Collapse Tree Node", true),
    ] {
        workspace.register_command(id, title, move |workspace, window, cx| {
            tree_command(workspace, window, cx, id, node)
        });
    }
}

/// Ctrl+Shift+S: writes the text of the active tab to a file the user picks.
fn save_as(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let Some(path) = workspace.active_path() else {
        return;
    };
    let Some(view) = workspace
        .active_view()
        .and_then(|view| view.downcast::<editor_view::EditorView>().ok())
    else {
        return;
    };
    let Some(snapshot) = view.read(cx).snapshot_for_save() else {
        return;
    };
    let directory = path.parent().map_or_else(
        || workspace.root().to_path_buf(),
        std::path::Path::to_path_buf,
    );
    let name = path
        .file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
    let chosen = cx.prompt_for_new_path(&directory, Some(&name));
    let root = workspace.root().to_path_buf();
    let project = cx
        .try_global::<ActiveProjectPanel>()
        .map(|panel| panel.0.clone());
    cx.spawn_in(window, async move |workspace, cx| {
        let Ok(Ok(Some(path))) = chosen.await else {
            return;
        };
        let target = path.clone();
        let write = cx.background_executor().spawn(async move {
            editor_view::EditorState::save_snapshot(&target, snapshot).result
        });
        match write.await {
            Ok(()) => {
                workspace
                    .update_in(cx, |workspace, window, cx| {
                        workspace.open_file_keeping_edits(path.clone(), window, cx)
                    })
                    .ok();
                // The file may live inside the project: the tree shows it. The window
                // is free again, so the tree can update its own workspace state.
                if path.starts_with(&root)
                    && let Some(panel) = project
                {
                    panel.update(cx, |panel, cx| panel.select_path(&path, cx));
                }
            }
            Err(error) => {
                let text = format!("{}: {error}", path.display());
                workspace
                    .update(cx, |workspace, cx| {
                        workspace.notify_user("Cannot save", text, cx)
                    })
                    .ok();
            }
        }
    })
    .detach();
}

/// The Menu key: the context menu of the focused tree, or of the editor at the caret.
fn show_popup_menu(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let panel = cx
        .try_global::<ActiveProjectPanel>()
        .map(|panel| panel.0.clone());
    if let Some(panel) = panel
        && panel.read(cx).focus().is_focused(window)
    {
        let (position, entries) = panel.update(cx, |panel, cx| panel.context_menu_entries(cx));
        workspace.show_menu(position, entries, cx);
        return;
    }
    window_popups::show_editor_menu(workspace, window, cx);
}

/// Expand and Collapse go to the project tree or to the hierarchy window that has the
/// focus; the project tree is the fallback.
fn tree_command(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
    id: &'static str,
    node: bool,
) {
    let project = cx
        .try_global::<ActiveProjectPanel>()
        .map(|panel| panel.0.clone());
    let hierarchy = cx
        .try_global::<ActiveCallHierarchy>()
        .map(|panel| panel.0.clone());
    let project_focused = project
        .as_ref()
        .is_some_and(|panel| panel.read(cx).focus().is_focused(window));
    let hierarchy_focused = hierarchy
        .as_ref()
        .is_some_and(|panel| panel.read(cx).focus().is_focused(window));
    if let Some(panel) = hierarchy.filter(|_| hierarchy_focused) {
        panel.update(cx, |panel, cx| match id {
            keys::EXPAND_ALL => panel.expand_all(cx),
            keys::COLLAPSE_ALL => panel.collapse_all(cx),
            _ => {}
        });
        return;
    }
    let method_hierarchy = cx
        .try_global::<ActiveMethodHierarchy>()
        .map(|panel| panel.0.clone());
    let method_focused = method_hierarchy
        .as_ref()
        .is_some_and(|panel| panel.read(cx).panel().read(cx).focus().is_focused(window));
    if let Some(panel) = method_hierarchy.filter(|_| method_focused) {
        panel.update(cx, |panel, cx| match id {
            keys::EXPAND_ALL => panel.expand_all(cx),
            keys::COLLAPSE_ALL => panel.collapse_all(cx),
            _ => {}
        });
        return;
    }
    let Some(panel) = project else {
        return;
    };
    if !project_focused && !matches!(id, keys::EXPAND_ALL | keys::COLLAPSE_ALL) {
        return;
    }
    let state = panel.update(cx, |panel, cx| {
        match (id, node) {
            (keys::EXPAND_ALL, _) => panel.expand_all_quiet(cx),
            (keys::COLLAPSE_ALL, _) => panel.collapse_all_quiet(cx),
            (keys::EXPAND_TREE_NODE, _) => panel.expand_node_quiet(cx),
            _ => panel.collapse_node_quiet(cx),
        }
        panel.tree_state()
    });
    let (expanded, selected) = state;
    workspace.set_tree_state(expanded, selected, cx);
}
