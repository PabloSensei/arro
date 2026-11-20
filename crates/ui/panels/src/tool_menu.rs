//! The Options button of the header of a tool window. It opens the menu of the window: the
//! view options the window has, Move to for the other docks, then Hide.

use std::rc::Rc;

use ui_kit::MenuEntry;
use ui_kit::gpui::{App, ClickEvent, WeakEntity, Window};
use workspace::{DockSide, Workspace};

/// The press handler of the Options button of the tool window `tool`. `view` returns the
/// view options of the window as rows of the menu; the menu ends with Move to and Hide.
pub(crate) fn options_menu(
    workspace: WeakEntity<Workspace>,
    tool: &'static str,
    view: impl Fn(&mut App) -> Vec<MenuEntry> + 'static,
) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
    let hiding = workspace.clone();
    options_menu_with_hide(workspace, tool, view, move |window, cx| {
        hiding
            .update(cx, |workspace, cx| workspace.toggle_tool(tool, window, cx))
            .ok();
    })
}

/// The same menu for a window whose Hide does more than close the dock, such as Coverage,
/// which also takes its stripes off the editors.
pub(crate) fn options_menu_with_hide(
    workspace: WeakEntity<Workspace>,
    tool: &'static str,
    view: impl Fn(&mut App) -> Vec<MenuEntry> + 'static,
    on_hide: impl Fn(&mut Window, &mut App) + 'static,
) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
    let hiding = Rc::new(on_hide);
    move |event, _, cx| {
        let mut entries = view(cx);
        if !entries.is_empty() {
            entries.push(MenuEntry::separator());
        }
        entries.extend(move_to_entries(&workspace, tool, cx));
        let hiding = hiding.clone();
        entries.push(MenuEntry::new("Hide", move |window, cx| hiding(window, cx)));
        let position = event.position();
        workspace
            .update(cx, |workspace, cx| {
                workspace.show_menu(position, entries, cx)
            })
            .ok();
    }
}

/// The Move to section of the menu of `tool`: Left, Right and Bottom, with the dock the
/// window is in now greyed out.
pub(crate) fn move_to_entries(
    workspace: &WeakEntity<Workspace>,
    tool: &'static str,
    cx: &App,
) -> Vec<MenuEntry> {
    let current = workspace
        .upgrade()
        .and_then(|workspace| workspace.read(cx).tool_side(tool));
    let mut target = Vec::with_capacity(3);
    for (label, side) in [
        ("Left", DockSide::Left),
        ("Right", DockSide::Right),
        ("Bottom", DockSide::Bottom),
    ] {
        let moving = workspace.clone();
        let row = MenuEntry::new(label, move |window, cx| {
            moving
                .update(cx, |workspace, cx| {
                    workspace.move_tool(tool, side, window, cx)
                })
                .ok();
        })
        .enabled(current != Some(side));
        target.push(row);
    }
    vec![MenuEntry::submenu("Move to", target)]
}
