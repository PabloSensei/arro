//! Rows and small strips: tabs, tree rows, status bar pieces.

use gpui_kit::prelude::*;
use gpui_kit::*;

use crate::metrics::*;
use crate::{TOOLTIP_CREATE_DELAY, Theme, icon, icon_sized, tooltip};

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
    #[cfg(any(test, feature = "test-support"))]
    let (name, shown_label) = (id.to_string(), Some(label.clone()));
    let tab = div()
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
                .tooltip_show_delay(TOOLTIP_CREATE_DELAY)
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
        );
    #[cfg(any(test, feature = "test-support"))]
    let tab = tab.child(crate::bounds_probe(name, shown_label));
    tab
}

/// The gap between the icon of a tree row and the name after it. The project tree and the
/// change trees of the Commit and Push windows keep it.
pub const TREE_ICON_GAP: f32 = SPACE_S;

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

/// Where the grey hint of a tree row stands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum HintPlacement {
    /// Right after the label, as a group shows its count.
    #[default]
    AfterLabel,
    /// At the right edge of the row, as IDEA lists the line of a place.
    End,
}

pub fn tree_row(row: TreeRow, theme: &Theme) -> Stateful<Div> {
    tree_row_with_badge(row, "", theme)
}

/// A tree row whose hint is pinned to the right edge of the row. The label takes the rest
/// of the width and ends with an ellipsis when it does not fit; the hint keeps its text
/// until it would take more than a third of the row.
pub fn tree_row_hint_at_end(row: TreeRow, theme: &Theme) -> Stateful<Div> {
    tree_row_with(row, "", HintPlacement::End, theme)
}

/// A tree row with a small icon right of its icon, such as the mark of a visibility; `""`
/// shows none.
pub fn tree_row_with_badge(row: TreeRow, badge: &'static str, theme: &Theme) -> Stateful<Div> {
    tree_row_with(row, badge, HintPlacement::AfterLabel, theme)
}

fn tree_row_with(
    row: TreeRow,
    badge: &'static str,
    placement: HintPlacement,
    theme: &Theme,
) -> Stateful<Div> {
    let hover = theme.selection_hover;
    #[cfg(any(test, feature = "test-support"))]
    let (name, label) = (row.id.to_string(), Some(row.label.clone()));
    // The icon and the name carry selectors of their own, so that a test measures the gap
    // between them. They are not probes: the overlap check reads the probes of a frame.
    let (icon_id, name_id, badge_id) = (row.id.clone(), row.id.clone(), row.id.clone());
    let hint_id = row.id.clone();
    let icon_part = div()
        .ml(px(SPACE_S))
        .flex_none()
        .debug_selector(move || format!("{icon_id}-icon"))
        .child(icon(row.icon, theme));
    let name_part = div()
        .ml(px(TREE_ICON_GAP))
        .flex_none()
        .debug_selector(move || format!("{name_id}-label"))
        .child(row.label);
    let name_part = match placement {
        HintPlacement::AfterLabel => name_part,
        // The label yields the room to the hint and is cut rather than pushing it away.
        HintPlacement::End => name_part.flex_1().min_w_0().truncate(),
    };
    let hint_part = row.hint.map(|hint| {
        let part = div()
            .ml(px(SPACE_M))
            .text_color(theme.text_secondary)
            .flex_none()
            .debug_selector(move || format!("{hint_id}-hint"));
        let part = match placement {
            HintPlacement::AfterLabel => part,
            HintPlacement::End => part.max_w(relative(1. / 3.)).min_w_0().truncate(),
        };
        part.child(hint)
    });
    let row = div()
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
        .child(icon_part)
        .when(!badge.is_empty(), |this| {
            this.child(
                div()
                    .ml(px(SPACE_XS))
                    .flex_none()
                    .debug_selector(move || format!("{badge_id}-badge"))
                    .child(icon(badge, theme)),
            )
        })
        .child(name_part)
        .children(hint_part);
    #[cfg(any(test, feature = "test-support"))]
    let row = row.child(crate::bounds_probe(name, label));
    row
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
