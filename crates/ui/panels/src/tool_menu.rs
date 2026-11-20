//! The Options button of the header of a tool window. It opens the menu of the window: the
//! view options the window has, then Hide. Arro has no `Move to` for a tool window yet.

use ui_kit::MenuEntry;
use ui_kit::gpui::{App, ClickEvent, WeakEntity, Window};
use workspace::Workspace;

/// The press handler of the Options button of the tool window `tool`. `view` returns the
/// view options of the window as rows of the menu; the menu ends with Hide.
pub(crate) fn options_menu(
    workspace: WeakEntity<Workspace>,
    tool: &'static str,
    view: impl Fn(&mut App) -> Vec<MenuEntry> + 'static,
) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
    move |event, _, cx| {
        let mut entries = view(cx);
        if !entries.is_empty() {
            entries.push(MenuEntry::separator());
        }
        let hiding = workspace.clone();
        entries.push(MenuEntry::new("Hide", move |window, cx| {
            hiding
                .update(cx, |workspace, cx| workspace.toggle_tool(tool, window, cx))
                .ok();
        }));
        let position = event.position();
        workspace
            .update(cx, |workspace, cx| {
                workspace.show_menu(position, entries, cx)
            })
            .ok();
    }
}
