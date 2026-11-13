//! A tool window that lists places in files: Problems and TODO. A click, or Enter, opens
//! the place in the editor.

use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::Workspace;

actions!(items_panel, [PreviousItem, NextItem, OpenItem]);

const CONTEXT: &str = "ItemsPanel";
const GROUP: &str = "bottom-panel";
const ROW_MARGIN: f32 = 12.;
/// A list this long is already more than anybody reads, and every row costs a frame.
const MAX_ITEMS: usize = 2_000;
/// A pause this long starts a new speed search rather than adding to the old one.
const SEARCH_TIMEOUT: Duration = Duration::from_millis(1000);

macro_rules! items_keys {
    ($(($keys:literal, $action:ident, $idea:expr, $mac:expr)),* $(,)?) => {
        pub fn bind_keys(cx: &mut App) {
            bind_keys_for(cx, keymap::platform());
        }

        /// Binds the keys of one platform, so that a test reads the macOS table on every
        /// runner.
        fn bind_keys_for(cx: &mut App, platform: keymap::Platform) {
            $(
                let row = workspace::KeyRow {
                    keys: $keys,
                    mac: $mac,
                    action: concat!("items_panel::", stringify!($action)),
                    context: Some(CONTEXT),
                    idea: $idea,
                };
                for key in row.keys_for(platform) {
                    cx.bind_keys([KeyBinding::new(key, $action, Some(CONTEXT))]);
                }
            )*
        }

        #[cfg(test)]
        pub(crate) const KEYS: &[workspace::KeyRow] = &[
            $(workspace::KeyRow {
                keys: $keys,
                mac: $mac,
                action: concat!("items_panel::", stringify!($action)),
                context: Some(CONTEXT),
                idea: $idea,
            }),*
        ];
    };
}

items_keys![
    ("up", PreviousItem, None, Some("up")),
    ("down", NextItem, None, Some("down")),
    ("enter", OpenItem, None, Some("enter")),
    ("f4", OpenItem, None, Some("f4")),
];

/// One place in a file. Line and column count from zero.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub path: PathBuf,
    pub line: usize,
    pub column: usize,
    pub label: String,
    /// Where it is, shown after the label.
    pub hint: String,
    pub icon: &'static str,
}

/// The extra rows a window puts in the menu of an item. The row of a file with a path is
/// added by the panel itself.
pub type RowMenu = Rc<dyn Fn(&Item, Point<Pixels>, &mut App) -> Vec<MenuEntry>>;

/// What a button of the panel header runs.
pub type HeaderAction = Rc<dyn Fn(&mut Window, &mut App)>;

/// Told when Delete or Insert changes an item.
pub type ToggleHandler = Rc<dyn Fn(usize, bool, &mut App)>;

/// One row of the list: a group header or one item of the panel. `depth` indents a row
/// under the groups above it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PanelRow {
    Group {
        label: String,
        depth: usize,
        icon: &'static str,
    },
    Item {
        index: usize,
        depth: usize,
        excluded: bool,
    },
}

/// A button of the panel header, such as a grouping rule of Usages.
#[derive(Clone)]
pub struct HeaderButton {
    pub id: &'static str,
    pub label: String,
    pub active: bool,
    /// The icon of the button; a button without one shows its label.
    pub icon: Option<&'static str>,
    /// The name of the button, shown in a tooltip with its key.
    pub tooltip: Option<String>,
    /// The key of the button, shown after its name in the tooltip.
    pub shortcut: Option<String>,
    /// What the button runs.
    pub run: HeaderAction,
}

/// Opens `path` with the caret at a place. The caret is set once the file is read.
pub fn open_at(
    workspace: &mut Workspace,
    path: PathBuf,
    line: usize,
    column: usize,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    workspace.open_file(path, window, cx);
    let view = workspace
        .active_editor()
        .and_then(|(_, view)| view.downcast::<editor_view::EditorView>().ok());
    if let Some(view) = view {
        view.update(cx, |view, cx| view.restore_caret(line, column, cx));
    }
}

pub struct ItemsPanel {
    title: SharedString,
    tool: &'static str,
    empty: SharedString,
    items: Vec<Item>,
    selected: Option<usize>,
    focus: FocusHandle,
    scroll: ScrollHandle,
    workspace: WeakEntity<Workspace>,
    row_menu: Option<RowMenu>,
    /// The letters typed for the speed search.
    search: String,
    search_at: Option<Instant>,
    /// The rows of the list, in the order they are drawn.
    rows: Vec<PanelRow>,
    /// The buttons of the header, in the order they are drawn.
    header_buttons: Vec<HeaderButton>,
    /// The items Delete took out of the result, by item index.
    excluded: Vec<bool>,
    /// The title with the number of the items that are not excluded, when set.
    counted_title: Option<String>,
    /// Told when Delete or Insert changes an item.
    on_toggle: Option<ToggleHandler>,
}

impl ItemsPanel {
    pub fn create(
        title: &str,
        tool: &'static str,
        empty: &str,
        workspace: WeakEntity<Workspace>,
        cx: &mut App,
    ) -> Entity<Self> {
        let (title, empty) = (title.to_string(), empty.to_string());
        cx.new(|cx| Self {
            title: title.into(),
            tool,
            empty: empty.into(),
            items: Vec::new(),
            selected: None,
            focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            workspace,
            row_menu: None,
            search: String::new(),
            search_at: None,
            rows: Vec::new(),
            header_buttons: Vec::new(),
            excluded: Vec::new(),
            counted_title: None,
            on_toggle: None,
        })
    }

    /// Adds rows to the menu of every item. The panel adds Copy Path after them.
    pub fn set_row_menu(&mut self, menu: RowMenu) {
        self.row_menu = Some(menu);
    }

    pub fn focus(&self) -> FocusHandle {
        self.focus.clone()
    }

    pub fn workspace(&self) -> WeakEntity<Workspace> {
        self.workspace.clone()
    }

    pub fn items(&self) -> &[Item] {
        &self.items
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn set_title(&mut self, title: String, cx: &mut Context<Self>) {
        self.title = title.into();
        cx.notify();
    }

    pub fn set_items(&mut self, items: Vec<Item>, cx: &mut Context<Self>) {
        let rows = (0..items.len())
            .map(|index| PanelRow::Item {
                index,
                depth: 0,
                excluded: false,
            })
            .collect();
        self.set_view(items, rows, cx);
    }

    /// Replaces the list with rows that may group the items.
    pub fn set_view(
        &mut self,
        mut items: Vec<Item>,
        mut rows: Vec<PanelRow>,
        cx: &mut Context<Self>,
    ) {
        items.truncate(MAX_ITEMS);
        rows.retain(|row| match row {
            PanelRow::Item { index, .. } => *index < items.len(),
            PanelRow::Group { .. } => true,
        });
        self.selected = self.selected.filter(|index| *index < items.len());
        self.excluded = vec![false; items.len()];
        for row in &rows {
            if let PanelRow::Item {
                index,
                excluded: true,
                ..
            } = row
                && let Some(excluded) = self.excluded.get_mut(*index)
            {
                *excluded = true;
            }
        }
        self.items = items;
        self.rows = rows;
        self.update_counted_title();
        cx.notify();
    }

    /// The title gets the number of the items that are not excluded.
    pub fn set_counted_title(&mut self, prefix: &str, cx: &mut Context<Self>) {
        self.counted_title = Some(prefix.to_owned());
        self.update_counted_title();
        cx.notify();
    }

    fn update_counted_title(&mut self) {
        let Some(prefix) = &self.counted_title else {
            return;
        };
        let shown = self.excluded.iter().filter(|excluded| !**excluded).count();
        self.title = format!("{prefix} ({shown})").into();
    }

    /// Told when Delete or Insert changes an item.
    pub fn set_toggle_handler(&mut self, handler: ToggleHandler) {
        self.on_toggle = Some(handler);
    }

    /// The rows of the list as they are drawn.
    pub fn rows(&self) -> &[PanelRow] {
        &self.rows
    }

    /// The selected item.
    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    /// The buttons on the right of the title.
    pub fn set_header_buttons(&mut self, buttons: Vec<HeaderButton>, cx: &mut Context<Self>) {
        self.header_buttons = buttons;
        cx.notify();
    }

    /// The buttons of the header, for the tests.
    #[cfg(test)]
    pub(crate) fn header_buttons(&self) -> &[HeaderButton] {
        &self.header_buttons
    }

    /// The tree of the list as plain text, for Export to Text File.
    pub fn export_text(&self) -> String {
        let mut lines = Vec::new();
        for row in &self.rows {
            match row {
                PanelRow::Group { label, depth, .. } => {
                    lines.push(format!("{}{}", "  ".repeat(*depth), label));
                }
                PanelRow::Item {
                    index,
                    depth,
                    excluded,
                } => {
                    if let Some(item) = self.items.get(*index) {
                        let mark = if *excluded { "- " } else { "" };
                        lines.push(format!(
                            "{}{mark}{}\t{}",
                            "  ".repeat(*depth),
                            item.label,
                            item.hint
                        ));
                    }
                }
            }
        }
        lines.join("\n")
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.items.is_empty() {
            return;
        }
        let last = self.items.len() as isize - 1;
        let next = self.selected.map_or(0, |index| index as isize + delta);
        self.selected = Some(next.clamp(0, last) as usize);
        cx.notify();
    }

    fn open(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = self.items.get(index).cloned() else {
            return;
        };
        self.selected = Some(index);
        let go = |workspace: &mut Workspace, cx: &mut Context<Workspace>| {
            open_at(workspace, item.path, item.line, item.column, window, cx)
        };
        self.workspace.update(cx, go).ok();
        cx.notify();
    }

    /// Ctrl+C puts the selected row on the clipboard, as the list of IDEA does.
    fn copy_selected(&mut self, cx: &mut Context<Self>) {
        let Some(item) = self.selected.and_then(|index| self.items.get(index)) else {
            return;
        };
        let text = format!("{}\t{}", item.label, item.hint);
        cx.write_to_clipboard(ClipboardItem::new_string(text));
    }

    /// Typed letters pick the next row whose label starts with them.
    fn search_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        let key = &event.keystroke;
        if key.modifiers.control || key.modifiers.alt || key.modifiers.platform {
            return false;
        }
        let now = Instant::now();
        let expired = self
            .search_at
            .is_none_or(|at| now.duration_since(at) > SEARCH_TIMEOUT);
        if key.key == "backspace" {
            if self.search.is_empty() {
                return false;
            }
            self.search.pop();
        } else {
            let Some(text) = &key.key_char else {
                return false;
            };
            if text.chars().count() != 1 || text.chars().any(char::is_control) {
                return false;
            }
            if expired {
                self.search.clear();
            }
            self.search.push_str(text);
        }
        self.search_at = Some(now);
        if self.search.is_empty() || self.items.is_empty() {
            return true;
        }
        let needle = self.search.to_lowercase();
        let count = self.items.len();
        let start = if expired {
            0
        } else {
            self.selected.map_or(0, |index| index + 1)
        };
        let found = (0..count)
            .map(|step| (start + step) % count)
            .find(|index| self.items[*index].label.to_lowercase().starts_with(&needle));
        if let Some(index) = found {
            self.selected = Some(index);
            cx.notify();
        }
        true
    }

    fn key(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let key = &event.keystroke;
        if key.modifiers.control && !key.modifiers.alt && key.key == "c" {
            cx.stop_propagation();
            self.copy_selected(cx);
            return;
        }
        if matches!(key.key.as_str(), "delete" | "insert")
            && let Some(index) = self.selected
            && let Some(excluded) = self.excluded.get_mut(index)
        {
            cx.stop_propagation();
            *excluded = key.key == "delete";
            let now = *excluded;
            for row in &mut self.rows {
                if let PanelRow::Item {
                    index: at,
                    excluded: out,
                    ..
                } = row
                    && *at == index
                {
                    *out = now;
                }
            }
            self.update_counted_title();
            if let Some(handler) = self.on_toggle.clone() {
                handler(index, now, cx);
            }
            cx.notify();
            return;
        }
        if self.search_key(event, cx) {
            cx.stop_propagation();
        }
    }
}

impl Render for ItemsPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let (workspace, tool) = (self.workspace.clone(), self.tool);
        let hide = move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
            workspace
                .update(cx, |workspace, cx| workspace.toggle_tool(tool, window, cx))
                .ok();
        };
        let rows = self.rows.iter().enumerate().map(|(at, row)| match row {
            PanelRow::Group { label, depth, icon } => {
                let style = TreeRow {
                    id: ("group", at).into(),
                    depth: *depth,
                    label: label.clone().into(),
                    hint: None,
                    icon,
                    expandable: false,
                    open: true,
                    selected: false,
                };
                div().w_full().px(px(ROW_MARGIN)).child(
                    tree_row(style, &theme).debug_selector(move || format!("item-group-{at}")),
                )
            }
            PanelRow::Item {
                index,
                depth,
                excluded,
            } => {
                let index = *index;
                let depth = *depth;
                let excluded = *excluded;
                let Some(item) = self.items.get(index) else {
                    return div();
                };
                let style = TreeRow {
                    id: ("item", index).into(),
                    depth,
                    label: item.label.clone().into(),
                    hint: Some(item.hint.clone().into()),
                    icon: item.icon,
                    expandable: false,
                    open: false,
                    selected: self.selected == Some(index),
                };
                let menu = self.row_menu.clone();
                let row = tree_row(style, &theme)
                    .debug_selector(move || format!("item-row-{index}"))
                    .when(excluded, |this| this.text_color(theme.text_muted))
                    .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                        window.focus(&this.focus, cx);
                        this.selected = Some(index);
                        if event.click_count() >= 2 {
                            this.open(index, window, cx);
                        }
                        cx.notify();
                    }))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                            let Some(menu) = &menu else {
                                return;
                            };
                            let Some(item) = this.items.get(index).cloned() else {
                                return;
                            };
                            let mut entries = menu(&item, event.position, cx);
                            if entries.is_empty() {
                                return;
                            }
                            entries.push(MenuEntry::separator());
                            let text = item.path.display().to_string();
                            entries.push(MenuEntry::new("Copy Path", move |_, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(text.clone()))
                            }));
                            this.workspace
                                .update(cx, |workspace, cx| {
                                    workspace.show_menu(event.position, entries, cx)
                                })
                                .ok();
                        }),
                    );
                let row = div().relative().w_full().child(row);
                let row = if excluded {
                    row.child(
                        div()
                            .absolute()
                            .left(px(depth as f32 * 19. + 40.))
                            .right(px(8.))
                            .top(px(10.))
                            .h(px(1.))
                            .bg(theme.text_muted),
                    )
                } else {
                    row
                };
                div().w_full().px(px(ROW_MARGIN)).child(row)
            }
        });
        let body = if self.items.is_empty() {
            div()
                .p(px(12.))
                .text_color(theme.text_secondary)
                .child(self.empty.clone())
                .into_any_element()
        } else {
            scrollable(
                "items-list",
                &self.scroll,
                div()
                    .id("items-list")
                    .flex()
                    .flex_col()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .children(rows),
                &theme,
                window,
                cx,
            )
            .size_full()
            .into_any_element()
        };
        let buttons: Vec<_> = self
            .header_buttons
            .iter()
            .map(|button| {
                let active = button.active;
                let id = button.id;
                let run = button.run.clone();
                let icon_name = button.icon;
                let label = button.label.clone();
                let tooltip_text = button
                    .tooltip
                    .clone()
                    .unwrap_or_else(|| button.label.clone());
                let shortcut = button.shortcut.clone();
                let frame = div()
                    .id(id)
                    .debug_selector(move || format!("item-button-{id}"))
                    .flex()
                    .items_center()
                    .h(px(20.))
                    .px(px(SPACE_S))
                    .rounded(px(CONTROL_RADIUS))
                    .text_color(if active {
                        theme.accent
                    } else {
                        theme.text_secondary
                    })
                    .when(active, |this| this.bg(theme.tab_selected))
                    .hover(move |style| style.bg(theme.hover))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |_, _: &MouseDownEvent, window, cx| run(window, cx)),
                    );
                let frame = match icon_name {
                    Some(name) => frame.child(icon(name, &theme)),
                    None => frame.child(label),
                };
                frame
                    .tooltip(move |_, cx| {
                        tooltip(tooltip_text.clone(), shortcut.clone().map(Into::into), cx)
                    })
                    .tooltip_show_delay(ui_kit::tooltip_show_delay())
            })
            .collect();
        let header = div()
            .flex()
            .items_center()
            .justify_between()
            .flex_none()
            .h(px(HEADER_HEIGHT))
            .pl(px(SPACE_M))
            .pr(px(SPACE_S))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .min_w_0()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(4.))
                            .font_weight(FontWeight::MEDIUM)
                            .child(self.title.clone()),
                    )
                    .child(div().flex().items_center().gap(px(2.)).children(buttons)),
            )
            .child(panel_tools(GROUP, hide, &theme));
        island(&theme)
            .id(self.tool)
            .debug_selector(move || format!("items-{tool}"))
            .key_context("ItemsPanel ToolWindow")
            .track_focus(&self.focus)
            .group(GROUP)
            .h_full()
            .on_action(cx.listener(|this, _: &PreviousItem, _, cx| this.move_selection(-1, cx)))
            .on_action(cx.listener(|this, _: &NextItem, _, cx| this.move_selection(1, cx)))
            .on_action(cx.listener(|this, _: &OpenItem, window, cx| {
                if let Some(index) = this.selected {
                    this.open(index, window, cx);
                }
            }))
            .on_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)))
            .child(header)
            .child(div().flex_1().min_h_0().pt(px(SPACE_S)).child(body))
    }
}
