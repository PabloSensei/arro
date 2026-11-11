//! Rows and small strips: tabs, tree rows, status bar pieces.

use gpui_kit::prelude::*;
use gpui_kit::*;

use crate::metrics::*;
use crate::{Theme, icon, icon_sized, tooltip, tooltip_show_delay};

/// One editor tab. `index` makes element ids unique inside a pane.
pub struct TabStyle {
    pub id: ElementId,
    pub close_id: ElementId,
    pub group: SharedString,
    pub label: SharedString,
    pub icon: &'static str,
    pub selected: bool,
    pub dirty: bool,
    /// A pinned tab shows a dot instead of the close button; the dot unpins it.
    pub pinned: bool,
}

pub fn tab(
    style: TabStyle,
    on_close: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    theme: &Theme,
) -> Stateful<Div> {
    let TabStyle {
        id,
        close_id,
        group,
        label,
        icon: icon_name,
        selected,
        dirty,
        pinned,
    } = style;
    let (hover, pressed) = (theme.hover, theme.pressed);
    let label = if dirty {
        format!("*{label}").into()
    } else {
        label
    };
    div()
        .id(id)
        .group(group.clone())
        .flex()
        .flex_none()
        .items_center()
        .gap(px(SPACE_S))
        .h(px(CONTROL_HEIGHT))
        .pl(px(SPACE_M))
        .pr(px(SPACE_S))
        .rounded(px(TAB_RADIUS))
        .border_1()
        .when(selected, |this| {
            this.bg(theme.tab_selected)
                .border_color(theme.tab_selected_border)
        })
        .when(!selected, |this| {
            this.border_color(transparent_black())
                .hover(move |style| style.bg(hover))
        })
        .active(move |style| style.bg(pressed))
        .child(icon(icon_name, theme))
        .child(
            div()
                .text_color(if selected {
                    theme.text
                } else {
                    theme.text_muted
                })
                .child(label),
        )
        .child(
            div()
                .id(close_id)
                .ml(px(-SPACE_XS))
                .size(px(16.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(4.))
                .occlude()
                .when(!selected && !pinned, |this| {
                    this.invisible().group_hover(group, |style| style.visible())
                })
                .hover(move |style| style.bg(hover))
                .tooltip(move |_, cx| {
                    tooltip(
                        if pinned { "Unpin Tab" } else { "Close Tab" },
                        (!pinned).then(|| "Ctrl+F4".into()),
                        cx,
                    )
                })
                .tooltip_show_delay(tooltip_show_delay())
                .on_click(on_close)
                .child(if pinned {
                    div()
                        .size(px(6.))
                        .rounded_full()
                        .bg(theme.accent)
                        .into_any_element()
                } else {
                    icon_sized("general/closeSmall", 14., theme).into_any_element()
                }),
        )
}

/// What a tree row shows. The list around it sets the side margins.
pub struct TreeRow {
    pub id: ElementId,
    pub depth: usize,
    pub label: SharedString,
    pub hint: Option<SharedString>,
    pub icon: &'static str,
    pub expandable: bool,
    pub open: bool,
    pub selected: bool,
}

pub fn tree_row(row: TreeRow, theme: &Theme) -> Stateful<Div> {
    let hover = theme.selection_hover;
    div()
        .id(row.id)
        .flex()
        .items_center()
        .flex_none()
        .w_full()
        .h(px(ROW_HEIGHT))
        .pl(px(row.depth as f32 * 19. + ROW_PADDING_X))
        .pr(px(ROW_PADDING_X))
        .rounded(px(ROW_RADIUS))
        .when(row.selected, |this| this.bg(theme.selection_inactive))
        .when(!row.selected, |this| {
            this.hover(move |style| style.bg(hover))
        })
        .child(
            div()
                .size(px(16.))
                .flex_none()
                .when(row.expandable, |this| {
                    let name = if row.open {
                        "general/chevronDown"
                    } else {
                        "general/chevronRight"
                    };
                    this.child(icon(name, theme))
                }),
        )
        .child(icon(row.icon, theme).ml(px(SPACE_S)))
        .child(div().ml(px(SPACE_S)).flex_none().child(row.label))
        .children(row.hint.map(|hint| {
            div()
                .ml(px(SPACE_M))
                .text_color(theme.text_secondary)
                .flex_none()
                .child(hint)
        }))
}

/// A breadcrumb in the status bar.
pub fn crumb(
    id: impl Into<ElementId>,
    leading: Option<&'static str>,
    label: impl Into<SharedString>,
    theme: &Theme,
) -> Stateful<Div> {
    let hover = theme.hover;
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .gap(px(SPACE_XS))
        .h(px(CONTROL_HEIGHT))
        .px(px(SPACE_S))
        .rounded(px(CONTROL_RADIUS))
        .text_size(px(TEXT_SMALL))
        .hover(move |style| style.bg(hover))
        .children(leading.map(|name| icon_sized(name, ICON, theme)))
        .child(label.into())
}

pub fn crumb_separator(theme: &Theme) -> Img {
    icon_sized("general/chevronRight", TEXT_SMALL, theme).mx(px(SPACE_XS))
}

/// A text widget on the right of the status bar.
pub fn status_widget(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    theme: &Theme,
) -> Stateful<Div> {
    let hover = theme.hover;
    div()
        .id(id)
        .flex()
        .items_center()
        .h(px(CONTROL_HEIGHT))
        .px(px(SPACE_S))
        .rounded(px(CONTROL_RADIUS))
        .text_size(px(TEXT_SMALL))
        .hover(move |style| style.bg(hover))
        .child(label.into())
}
