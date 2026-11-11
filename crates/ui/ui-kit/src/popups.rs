//! Popups inside the main window: surface, entry motion, menu, notification.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::prelude::*;
use gpui_kit::*;

use crate::metrics::*;
use crate::{Theme, icon, icon_button, icon_sized, motion, popup_frame};

/// The one entry motion of a popup or a dialog: a short fade, no movement.
pub fn enter(
    id: impl Into<ElementId>,
    element: impl IntoElement + Styled + 'static,
) -> impl IntoElement {
    let duration = Duration::from_millis(motion::POPUP_FADE);
    element.with_animation(
        id,
        Animation::new(duration).with_easing(ease_out_quint()),
        |this, delta| this.opacity(delta),
    )
}

/// A popup panel: the shared frame, a column of rows, and no overflow.
pub fn surface(theme: &Theme) -> Div {
    popup_frame(theme).flex().flex_col().overflow_hidden()
}

/// What a menu row runs.
pub type MenuAction = Rc<dyn Fn(&mut Window, &mut App)>;
/// The row the pointer is over, its path from the top of the menu, and whether it entered.
pub type MenuHover = Rc<dyn Fn(&[usize], bool, &mut Window, &mut App)>;
/// A row with a submenu was clicked: the path of the row.
pub type MenuOpen = Rc<dyn Fn(&[usize], &mut Window, &mut App)>;

/// Callbacks a menu reports to its owner.
#[derive(Clone)]
pub struct MenuCallbacks {
    /// Runs before the picked row and closes the popup.
    pub on_pick: MenuAction,
    pub on_hover: MenuHover,
    pub on_open: MenuOpen,
}

/// A menu row, or a separator when `label` is empty. A row with `submenu` opens it.
#[derive(Clone)]
pub struct MenuEntry {
    pub label: SharedString,
    pub shortcut: Option<SharedString>,
    pub icon: Option<&'static str>,
    pub on_click: Option<MenuAction>,
    pub submenu: Vec<MenuEntry>,
    /// The command or action this row runs, for checks. It is not shown.
    pub id: Option<SharedString>,
    /// A less important row, such as a temporary run configuration.
    pub muted: bool,
}

impl MenuEntry {
    pub fn new(
        label: impl Into<SharedString>,
        on_click: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            label: label.into(),
            shortcut: None,
            icon: None,
            on_click: Some(Rc::new(on_click)),
            submenu: Vec::new(),
            id: None,
            muted: false,
        }
    }

    /// A row that opens `submenu`.
    pub fn submenu(label: impl Into<SharedString>, submenu: Vec<MenuEntry>) -> Self {
        Self {
            label: label.into(),
            submenu,
            ..Self::separator()
        }
    }

    pub fn shortcut(mut self, shortcut: impl Into<SharedString>) -> Self {
        self.shortcut = Some(shortcut.into());
        self
    }

    pub fn icon(mut self, icon: &'static str) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn id(mut self, id: impl Into<SharedString>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// Draws the row in the muted colour: a configuration that is not in the file.
    pub fn muted(mut self) -> Self {
        self.muted = true;
        self
    }

    pub fn separator() -> Self {
        Self {
            label: SharedString::default(),
            shortcut: None,
            icon: None,
            on_click: None,
            submenu: Vec::new(),
            id: None,
            muted: false,
        }
    }

    /// A row that is shown, but does nothing.
    pub fn disabled(label: impl Into<SharedString>) -> Self {
        Self {
            label: label.into(),
            ..Self::separator()
        }
    }

    pub fn has_submenu(&self) -> bool {
        !self.submenu.is_empty()
    }

    /// A row the arrows stop on: it is not a separator and it is not disabled.
    pub fn selectable(&self) -> bool {
        !self.label.is_empty() && (self.on_click.is_some() || self.has_submenu())
    }
}

/// The highlighted row of a menu and the submenus that are open, from the top row down.
/// Every method keeps the parts consistent, so the renderer can trust them.
#[derive(Clone, Debug, Default)]
pub struct MenuState {
    /// The indexes of the rows whose submenus are open, from the top menu down.
    open: Vec<usize>,
    /// The index of the highlighted row at each open level, the last one included.
    highlight: Vec<usize>,
}

impl MenuState {
    pub fn clear(&mut self) {
        self.open.clear();
        self.highlight.clear();
    }

    pub fn open_path(&self) -> &[usize] {
        &self.open
    }

    pub fn highlight(&self) -> &[usize] {
        &self.highlight
    }

    /// The highlighted row at `depth`, zero being the rows of the menu itself.
    pub fn highlighted_at(&self, depth: usize) -> Option<usize> {
        self.highlight.get(depth).copied()
    }

    /// Whether the submenu of the row at `path` is open.
    pub fn is_submenu_open(&self, path: &[usize]) -> bool {
        !path.is_empty() && self.open.len() >= path.len() && self.open[..path.len()] == *path
    }

    fn level<'a>(&self, entries: &'a [MenuEntry]) -> &'a [MenuEntry] {
        let mut level = entries;
        for index in &self.open {
            let Some(entry) = level.get(*index) else {
                return &[];
            };
            level = &entry.submenu;
        }
        level
    }

    /// The row at `path`, whether or not its level is open.
    pub fn entry<'a>(&self, entries: &'a [MenuEntry], path: &[usize]) -> Option<&'a MenuEntry> {
        let mut level = entries;
        let mut found = None;
        for index in path {
            let entry = level.get(*index)?;
            found = Some(entry);
            level = &entry.submenu;
        }
        found
    }

    fn step(&mut self, entries: &[MenuEntry], forward: bool) {
        let depth = self.open.len();
        let level = self.level(entries);
        let current = self.highlighted_at(depth);
        let count = level.len();
        if count == 0 {
            return;
        }
        let next = match current {
            Some(current) => {
                let mut index = current;
                for _ in 0..count {
                    index = if forward {
                        (index + 1) % count
                    } else {
                        (index + count - 1) % count
                    };
                    if level[index].selectable() {
                        break;
                    }
                }
                level[index].selectable().then_some(index)
            }
            None if forward => (0..count).find(|index| level[*index].selectable()),
            None => (0..count).rev().find(|index| level[*index].selectable()),
        };
        if let Some(index) = next {
            self.highlight.truncate(depth);
            self.highlight.push(index);
        }
    }

    pub fn move_down(&mut self, entries: &[MenuEntry]) {
        self.step(entries, true);
    }

    pub fn move_up(&mut self, entries: &[MenuEntry]) {
        self.step(entries, false);
    }

    /// Highlights the row at `path`; submenus below its level close. A row whose own
    /// submenu is already open keeps it: the pointer goes from the row into the submenu.
    pub fn hover(&mut self, path: &[usize]) {
        let Some((&index, parents)) = path.split_last() else {
            return;
        };
        if self.is_submenu_open(path) {
            let depth = path.len() - 1;
            if self.highlight.len() > depth {
                self.highlight[depth] = index;
            }
            return;
        }
        self.open.clear();
        self.open.extend_from_slice(parents);
        self.highlight.clear();
        self.highlight.extend_from_slice(parents);
        self.highlight.push(index);
    }

    pub fn is_highlighted(&self, path: &[usize]) -> bool {
        self.highlight.len() >= path.len() && self.highlight[..path.len()] == *path
    }

    /// Opens the submenu of the row at `path` and highlights its first row.
    pub fn open_row(&mut self, entries: &[MenuEntry], path: &[usize]) -> bool {
        let Some((&index, parents)) = path.split_last() else {
            return false;
        };
        let Some(entry) = self.entry(entries, path) else {
            return false;
        };
        if entry.submenu.is_empty() {
            return false;
        }
        // A submenu with nothing to run, such as an empty recent list, still opens.
        let first = entry
            .submenu
            .iter()
            .position(MenuEntry::selectable)
            .unwrap_or(0);
        self.open.clear();
        self.open.extend_from_slice(parents);
        self.open.push(index);
        self.highlight.clear();
        self.highlight.extend_from_slice(&self.open);
        self.highlight.push(first);
        true
    }

    /// Opens the submenu of the highlighted row, as Right or Enter does.
    pub fn open_submenu(&mut self, entries: &[MenuEntry]) -> bool {
        let depth = self.open.len();
        let Some(index) = self.highlighted_at(depth) else {
            return false;
        };
        let mut path = self.open.clone();
        path.push(index);
        self.open_row(entries, &path)
    }

    /// Closes the deepest open submenu. False when the menu itself is the only level.
    pub fn close_submenu(&mut self) -> bool {
        if self.open.pop().is_none() {
            return false;
        }
        self.highlight.truncate(self.open.len() + 1);
        true
    }

    /// The highlighted row at the deepest open level.
    pub fn current<'a>(&self, entries: &'a [MenuEntry]) -> Option<&'a MenuEntry> {
        let depth = self.open.len();
        let index = self.highlighted_at(depth)?;
        self.level(entries).get(index)
    }
}

/// The narrowest a menu goes.
pub const MENU_MIN_WIDTH: f32 = 336.;
/// The height a separator takes with its margins.
pub const MENU_SEPARATOR_HEIGHT: f32 = 1. + SPACE_S * 2.;
const MENU_ICON_WIDTH: f32 = 16.;
/// The space between the icon of a row and its label.
const MENU_ICON_GAP: f32 = SPACE_S;
/// The space between a row's label and its shortcut or the arrow of its submenu.
const MENU_ROW_GAP: f32 = 32.;
/// The arrow of a row with a submenu.
const MENU_CHEVRON: f32 = 12.;
/// The hairline frame of the popup on both sides.
const MENU_FRAME: f32 = 2.;

/// The width of `text` at `font_size`, in the font of the interface.
fn menu_text_width(window: &Window, text: &str, font_size: Pixels) -> f32 {
    if text.is_empty() {
        return 0.;
    }
    let run = TextRun {
        len: text.len(),
        font: font(UI_FONT),
        color: hsla(0., 0., 0., 1.),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let line = window.text_system().shape_line(
        text.to_owned().into(),
        font_size,
        std::slice::from_ref(&run),
        None,
    );
    f32::from(line.width())
}

/// The size a menu of `entries` takes, from the metrics the menu draws with. It lets a
/// menu that a button opens know where it goes before its first frame.
pub fn menu_size(window: &Window, entries: &[MenuEntry], font_size: Pixels) -> Size<Pixels> {
    let mut width = MENU_MIN_WIDTH;
    let mut height = POPUP_PADDING * 2. + MENU_FRAME;
    for entry in entries {
        if entry.label.is_empty() {
            height += MENU_SEPARATOR_HEIGHT;
            continue;
        }
        height += ROW_HEIGHT;
        let mut row = ROW_PADDING_X * 2.
            + MENU_ICON_WIDTH
            + MENU_ICON_GAP
            + menu_text_width(window, &entry.label, font_size);
        if let Some(shortcut) = entry.shortcut.as_ref() {
            row += MENU_ROW_GAP + menu_text_width(window, shortcut, font_size);
        } else if entry.has_submenu() {
            row += MENU_ROW_GAP + MENU_CHEVRON;
        }
        width = width.max(row + MENU_FRAME);
    }
    size(px(width.ceil()), px(height))
}

/// Where a menu that a button opens goes: its left edge on the left edge of the button,
/// its top edge a hair under it. At the right edge of the window the right edges match;
/// when it does not fit below, it opens above the button. It always stays inside the
/// window with `SPACE_M` to spare.
pub fn menu_position(
    anchor: Bounds<Pixels>,
    size: Size<Pixels>,
    viewport: Size<Pixels>,
) -> Point<Pixels> {
    let margin = px(SPACE_M);
    let max_x = (viewport.width - size.width - margin).max(margin);
    let mut x = anchor.left();
    if x + size.width + margin > viewport.width {
        x = anchor.right() - size.width;
    }
    let x = x.clamp(margin, max_x);

    let below = anchor.bottom() + px(SPACE_XS);
    let max_y = (viewport.height - size.height - margin).max(margin);
    let y = if below + size.height + margin > viewport.height {
        anchor.top() - px(SPACE_XS) - size.height
    } else {
        below
    };
    point(x, y.clamp(margin, max_y))
}

/// The area of a control that opens a menu. A menu opened by a button stands under that
/// button instead of at the point of the click, so the button records its area while it
/// is drawn and the click reads it back.
#[derive(Clone, Default)]
pub struct MenuAnchor {
    bounds: Rc<RefCell<Option<Bounds<Pixels>>>>,
}

impl MenuAnchor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn bounds(&self) -> Option<Bounds<Pixels>> {
        *self.bounds.borrow()
    }

    pub fn record(&self, bounds: Bounds<Pixels>) {
        *self.bounds.borrow_mut() = Some(bounds);
    }

    /// Fills the control it is put into and records that area on every draw.
    pub fn canvas(&self) -> impl IntoElement {
        let anchor = self.clone();
        canvas(move |bounds, _, _| anchor.record(bounds), |_, _, _, _| {})
            .absolute()
            .top_0()
            .left_0()
            .size_full()
    }
}

/// The row bounds and the submenu sizes the menu measured while drawing. A submenu is
/// drawn hidden until its size is measured, then placed from it.
#[derive(Clone, Default)]
pub struct MenuGeometry {
    rows: Rc<RefCell<HashMap<Vec<usize>, Bounds<Pixels>>>>,
    sizes: Rc<RefCell<HashMap<Vec<usize>, Size<Pixels>>>>,
}

impl MenuGeometry {
    pub fn row(&self, path: &[usize]) -> Option<Bounds<Pixels>> {
        self.rows.borrow().get(path).copied()
    }

    pub fn size(&self, path: &[usize]) -> Option<Size<Pixels>> {
        self.sizes.borrow().get(path).copied()
    }

    fn record_row(&self, path: &[usize], bounds: Bounds<Pixels>) {
        self.rows.borrow_mut().insert(path.to_vec(), bounds);
    }

    fn record_size(&self, path: &[usize], size: Size<Pixels>) {
        self.sizes.borrow_mut().insert(path.to_vec(), size);
    }
}

/// Space a submenu keeps between itself and the window edge.
const SUBMENU_MARGIN: Pixels = px(SPACE_M);

/// Where a submenu opens from its row: to the right while it fits, otherwise to the
/// left of the row, and shifted up when it would leave the window.
pub fn submenu_position(
    row: Bounds<Pixels>,
    size: Size<Pixels>,
    viewport: Size<Pixels>,
) -> Point<Pixels> {
    let mut x = row.right();
    if x + size.width + SUBMENU_MARGIN > viewport.width {
        x = row.left() - size.width;
    }
    let max_x = (viewport.width - size.width - SUBMENU_MARGIN).max(SUBMENU_MARGIN);
    let x = x.clamp(SUBMENU_MARGIN, max_x);

    let mut y = row.top();
    if y + size.height + SUBMENU_MARGIN > viewport.height {
        y = viewport.height - size.height - SUBMENU_MARGIN;
    }
    let max_y = (viewport.height - size.height - SUBMENU_MARGIN).max(SUBMENU_MARGIN);
    let y = y.clamp(SUBMENU_MARGIN, max_y);
    point(x, y)
}

struct MenuRender<'a> {
    state: &'a MenuState,
    geometry: &'a MenuGeometry,
    callbacks: &'a MenuCallbacks,
    viewport: Size<Pixels>,
    theme: &'a Theme,
}

fn menu_level(
    entries: &[MenuEntry],
    depth: usize,
    parent_path: &[usize],
    interactive: bool,
    render: &MenuRender,
) -> Stateful<Div> {
    let MenuRender {
        state,
        geometry,
        callbacks,
        viewport,
        theme,
    } = render;
    let hover = theme.selection;
    let muted = theme.text_secondary;
    let popup_id: SharedString = if depth == 0 {
        "menu-popup".into()
    } else {
        format!("menu-popup-{depth}").into()
    };
    // A submenu is placed by its real size. Until its first layout has measured
    // that size the frame is drawn hidden, so nothing is shown at a guess and then
    // moved on the next frame.
    let measured = (depth > 0).then(|| geometry.size(parent_path)).flatten();
    let visible = depth == 0 || measured.is_some();
    let measured_selector = if visible {
        popup_id.to_string()
    } else {
        format!("{popup_id}-pending")
    };
    let surface = popup_frame(theme)
        .id(popup_id.clone())
        .debug_selector(move || measured_selector.clone())
        .flex()
        .flex_col()
        .when(visible, |this| this.occlude())
        .when(!visible, |this| this.invisible().opacity(0.))
        .min_w(px(MENU_MIN_WIDTH))
        .py(px(POPUP_PADDING));

    let mut items = Vec::with_capacity(entries.len());
    for (index, entry) in entries.iter().enumerate() {
        let mut path = parent_path.to_vec();
        path.push(index);
        if entry.label.is_empty() {
            items.push(
                div()
                    .h(px(1.))
                    .mx(px(SPACE_M))
                    .my(px(SPACE_S))
                    .bg(theme.popup_border)
                    .into_any_element(),
            );
            continue;
        }

        let disabled = entry.on_click.is_none() && !entry.has_submenu();
        let selected = state.highlighted_at(depth) == Some(index);
        let open = state.is_submenu_open(&path);

        let measure = {
            let geometry = (*geometry).clone();
            let path = path.clone();
            canvas(
                move |bounds, _, _| geometry.record_row(&path, bounds),
                |_, _, _, _| {},
            )
            .absolute()
            .top_0()
            .left_0()
            .size_full()
        };

        let row_hover = callbacks.on_hover.clone();
        let row_open = callbacks.on_open.clone();
        let row_pick = callbacks.on_pick.clone();
        let row_action = entry.on_click.clone();
        let opens_submenu = entry.has_submenu();
        let hover_path = path.clone();
        let click_path = path.clone();

        let mut row = div()
            .id(ElementId::named_usize(format!("menu-{depth}"), index))
            .flex()
            .items_center()
            .justify_between()
            .gap(px(MENU_ROW_GAP))
            .h(px(ROW_HEIGHT))
            .px(px(ROW_PADDING_X))
            .rounded(px(ROW_RADIUS))
            .when(disabled, |this| this.text_color(theme.text_disabled))
            .when(entry.muted, |this| this.opacity(0.55))
            .when(selected, |this| this.bg(hover))
            .child(measure);

        if interactive {
            row = row
                .debug_selector({
                    let path = path.clone();
                    move || {
                        let parts: Vec<String> =
                            path.iter().map(|index| index.to_string()).collect();
                        format!("menu-{}", parts.join("-"))
                    }
                })
                .on_hover(move |hovered, window, cx| {
                    row_hover(&hover_path, *hovered, window, cx);
                });
            if !disabled {
                row = row.hover(move |style| if selected { style } else { style.bg(hover) });
                row = if opens_submenu {
                    let open = row_open.clone();
                    row.on_click(move |_, window, cx| {
                        open(&click_path, window, cx);
                    })
                } else {
                    let close = row_pick.clone();
                    let action = row_action.expect("an enabled row has an action");
                    row.on_click(move |_, window, cx| {
                        close(window, cx);
                        action(window, cx);
                    })
                };
            }
        }

        row = row.child(
            div()
                .flex()
                .items_center()
                .gap(px(MENU_ICON_GAP))
                .child(
                    div()
                        .size(px(MENU_ICON_WIDTH))
                        .flex_none()
                        .children(entry.icon.map(|name| icon(name, theme).grayscale(true))),
                )
                .child(entry.label.clone()),
        );

        if let Some(shortcut) = entry.shortcut.clone() {
            row = row.child(
                div()
                    .text_color(if disabled { theme.text_disabled } else { muted })
                    .child(shortcut),
            );
        } else if entry.has_submenu() {
            row = row.child(icon_sized("general/chevronRight", MENU_CHEVRON, theme));
        }

        if open {
            let size = geometry.size(&path);
            let submenu = menu_level(&entry.submenu, depth + 1, &path, size.is_some(), render);
            let row_bounds = geometry.row(&path);
            let position = match (row_bounds, size) {
                (Some(row), Some(size)) => submenu_position(row, size, *viewport),
                (Some(row), None) => point(row.right(), row.top()),
                _ => point(px(0.), px(0.)),
            };
            let record = {
                let geometry = (*geometry).clone();
                let path = path.clone();
                let unmeasured = size.is_none();
                canvas(
                    move |bounds, window, _| {
                        geometry.record_size(&path, bounds.size);
                        if unmeasured {
                            window.request_animation_frame();
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full()
            };
            let body = div().relative().child(record).child(submenu);
            row = row.child(
                deferred(
                    anchored()
                        .position(position)
                        .snap_to_window_with_margin(SUBMENU_MARGIN)
                        .child(body),
                )
                .with_priority(2 + depth),
            );
        }

        items.push(row.into_any_element());
    }

    surface.children(items)
}

/// The menu body with its submenus. `callbacks` closes the popup, tracks the pointer and
/// opens a row a click picked.
pub fn menu(
    entries: &[MenuEntry],
    state: &MenuState,
    geometry: &MenuGeometry,
    callbacks: &MenuCallbacks,
    viewport: Size<Pixels>,
    theme: &Theme,
) -> Stateful<Div> {
    let render = MenuRender {
        state,
        geometry,
        callbacks,
        viewport,
        theme,
    };
    menu_level(entries, 0, &[], true, &render)
}

/// What a button of a notification does when it is pressed.
pub type NotificationClick = Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// One button that the user can press from a notification.
pub struct NotificationAction {
    pub id: &'static str,
    pub label: SharedString,
    pub on_click: NotificationClick,
}

/// The balloon at the bottom right.
pub fn notification(
    title: SharedString,
    text: SharedString,
    actions: Vec<NotificationAction>,
    on_close: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    theme: &Theme,
) -> Stateful<Div> {
    let hover = theme.hover;
    // The actions get a row of their own: several buttons next to the text squeeze it
    // into one character per line, and the balloon grows taller than the window.
    let mut buttons = Vec::new();
    for (index, action) in actions.into_iter().enumerate() {
        let selector = if index == 0 {
            "notification-action".to_owned()
        } else {
            format!("notification-action-{}", action.id)
        };
        buttons.push(
            div()
                .id(("notification-action", index))
                .debug_selector(move || selector.clone())
                .flex_none()
                .h(px(24.))
                .px(px(SPACE_M))
                .flex()
                .items_center()
                .rounded(px(ROW_RADIUS))
                .text_color(theme.accent)
                .hover(move |style| style.bg(hover))
                .on_click(action.on_click)
                .child(action.label),
        );
    }
    popup_frame(theme)
        .id("notification")
        .debug_selector(|| "notification".to_owned())
        .occlude()
        .flex()
        .flex_col()
        .gap(px(SPACE_M))
        .w(px(380.))
        .p(px(DIALOG_PADDING))
        .child(
            div()
                .flex()
                .gap(px(SPACE_M))
                .child(icon("status/info", theme))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .gap(px(SPACE_S))
                        .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
                        .child(div().text_color(theme.text_muted).child(text)),
                )
                .child(
                    icon_button(
                        "notification-close",
                        "general/closeSmall",
                        "Close Notification",
                        None,
                        theme,
                    )
                    .size(px(20.))
                    .on_click(on_close),
                ),
        )
        .child(div().flex().flex_wrap().gap(px(SPACE_S)).children(buttons))
}

#[cfg(test)]
mod menu_tests {
    use super::{MenuEntry, MenuState, SUBMENU_MARGIN, menu_position, submenu_position};
    use crate::gpui::{Bounds, point, px, size};

    fn action(label: &'static str) -> MenuEntry {
        MenuEntry::new(label, |_, _| {})
    }

    fn sample() -> Vec<MenuEntry> {
        vec![
            MenuEntry::submenu(
                "File",
                vec![
                    action("Open"),
                    MenuEntry::separator(),
                    MenuEntry::disabled("Nothing"),
                    MenuEntry::submenu("Recent", vec![action("one"), action("two")]),
                ],
            ),
            MenuEntry::separator(),
            MenuEntry::submenu("Edit", vec![action("Undo"), action("Redo")]),
            action("Help"),
        ]
    }

    #[test]
    fn the_arrows_skip_separators_and_disabled_rows() {
        let entries = sample();
        let mut state = MenuState::default();
        state.move_down(&entries);
        assert_eq!(state.highlighted_at(0), Some(0), "the first section");
        state.move_down(&entries);
        assert_eq!(state.highlighted_at(0), Some(2), "the separator is skipped");
        state.move_down(&entries);
        assert_eq!(state.highlighted_at(0), Some(3));
        state.move_down(&entries);
        assert_eq!(state.highlighted_at(0), Some(0), "the list wraps");
        state.move_up(&entries);
        assert_eq!(state.highlighted_at(0), Some(3));
    }

    #[test]
    fn right_opens_a_submenu_and_left_closes_it() {
        let entries = sample();
        let mut state = MenuState::default();
        state.move_down(&entries);
        assert!(state.open_submenu(&entries));
        assert!(state.is_submenu_open(&[0]));
        assert_eq!(
            state.highlighted_at(1),
            Some(0),
            "the first row of the submenu"
        );
        state.move_down(&entries);
        assert_eq!(
            state.highlighted_at(1),
            Some(3),
            "the separator and the disabled row are skipped"
        );
        state.move_down(&entries);
        assert_eq!(state.highlighted_at(1), Some(0), "the submenu wraps");
        assert!(state.close_submenu());
        assert!(!state.is_submenu_open(&[0]));
        assert_eq!(state.highlighted_at(0), Some(0));
        assert!(!state.close_submenu(), "the top level is not a submenu");
    }

    #[test]
    fn a_nested_submenu_opens_from_its_row() {
        let entries = sample();
        let mut state = MenuState::default();
        state.hover(&[0, 3]);
        assert!(state.open_row(&entries, &[0, 3]));
        assert!(state.is_submenu_open(&[0, 3]));
        assert_eq!(state.highlighted_at(2), Some(0));
        assert!(state.close_submenu());
        assert!(state.is_submenu_open(&[0]));
        assert!(!state.is_submenu_open(&[0, 3]));
    }

    #[test]
    fn a_submenu_opens_to_the_right_while_it_fits() {
        let row = Bounds::new(point(px(100.), px(80.)), size(px(200.), px(26.)));
        let at = submenu_position(row, size(px(180.), px(200.)), size(px(800.), px(600.)));
        assert_eq!(at, point(px(300.), px(80.)));
    }

    #[test]
    fn a_submenu_opens_to_the_left_at_the_right_edge() {
        let row = Bounds::new(point(px(500.), px(80.)), size(px(200.), px(26.)));
        let at = submenu_position(row, size(px(180.), px(200.)), size(px(800.), px(600.)));
        assert_eq!(at, point(px(320.), px(80.)));
        assert!(at.x < row.left());
    }

    #[test]
    fn a_tall_submenu_is_shifted_up_from_the_bottom_edge() {
        let row = Bounds::new(point(px(100.), px(550.)), size(px(200.), px(26.)));
        let at = submenu_position(row, size(px(180.), px(300.)), size(px(800.), px(600.)));
        assert_eq!(at.y, px(292.));
        assert!(at.y + px(300.) <= px(600.) - SUBMENU_MARGIN);
    }

    #[test]
    fn a_menu_wider_than_the_window_stays_at_the_margin() {
        let row = Bounds::new(point(px(10.), px(80.)), size(px(20.), px(26.)));
        let at = submenu_position(row, size(px(900.), px(100.)), size(px(800.), px(600.)));
        assert_eq!(at.x, SUBMENU_MARGIN);
    }

    #[test]
    fn a_button_menu_stands_under_the_button() {
        let button = Bounds::new(point(px(100.), px(40.)), size(px(120.), px(26.)));
        let at = menu_position(button, size(px(336.), px(200.)), size(px(800.), px(700.)));
        assert_eq!(
            at,
            point(px(100.), px(68.)),
            "left edge under, one hair below"
        );
    }

    #[test]
    fn a_button_menu_at_the_right_edge_matches_the_right_edges() {
        let button = Bounds::new(point(px(600.), px(40.)), size(px(160.), px(26.)));
        let at = menu_position(button, size(px(336.), px(200.)), size(px(800.), px(700.)));
        assert_eq!(at.x + px(336.), button.right());
        assert!(at.x >= SUBMENU_MARGIN);
    }

    #[test]
    fn a_button_menu_that_does_not_fit_below_opens_above_it() {
        let button = Bounds::new(point(px(100.), px(500.)), size(px(120.), px(26.)));
        let at = menu_position(button, size(px(336.), px(240.)), size(px(800.), px(600.)));
        assert_eq!(at.y, button.top() - px(2.) - px(240.));
        assert!(at.y >= SUBMENU_MARGIN);
    }

    #[test]
    fn a_button_menu_stays_inside_a_small_window() {
        let button = Bounds::new(point(px(280.), px(300.)), size(px(120.), px(26.)));
        let at = menu_position(button, size(px(336.), px(240.)), size(px(400.), px(500.)));
        assert!(at.x >= SUBMENU_MARGIN);
        assert!(at.x + px(336.) <= px(400.) - SUBMENU_MARGIN);
        assert!(at.y >= SUBMENU_MARGIN);
        assert!(at.y + px(240.) <= px(500.) - SUBMENU_MARGIN);
    }
}
