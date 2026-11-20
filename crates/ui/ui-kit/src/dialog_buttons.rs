//! The buttons at the bottom of a modal dialog: one button, and the row that places them.
//!
//! One rule for every dialog, as in IntelliJ IDEA on Windows and Linux. The extra buttons
//! (Preview, Test Connection, Help) stand at the left edge. The right group stands in the
//! order OK, Cancel, Apply from the left, and OK is the primary button: filled with the
//! accent while it is enabled, grey while it is not. All buttons share one height and one
//! gap. macOS mirrors the right group, so Cancel stands left of OK there.

use gpui_kit::prelude::*;
use gpui_kit::*;

use crate::Theme;
use crate::metrics::*;

/// The gap between two buttons of a dialog.
const GAP: f32 = SPACE_M;

/// One button of a dialog. The primary one is filled with the accent while it is enabled and
/// grey while it is not. A disabled button only looks disabled: the action behind it must
/// refuse too, because Enter reaches the same action.
pub fn dialog_button(
    id: impl Into<ElementId>,
    selector: String,
    label: &str,
    primary: bool,
    enabled: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let (hover, pressed) = (theme.hover, theme.pressed);
    div()
        .id(id)
        .debug_selector(move || selector)
        .flex()
        .items_center()
        .h(px(CONTROL_HEIGHT))
        .px(px(SPACE_L))
        .rounded(px(CONTROL_RADIUS))
        .when(primary && enabled, |this| {
            this.bg(theme.accent).text_color(theme.over_accent)
        })
        .when(primary && !enabled, |this| {
            this.text_color(theme.text_disabled)
                .border_1()
                .border_color(theme.control_border)
        })
        .when(!primary && enabled, |this| {
            this.border_1()
                .border_color(theme.control_border)
                .text_color(theme.text)
                .hover(move |style| style.bg(hover))
                .active(move |style| style.bg(pressed))
        })
        .when(!primary && !enabled, |this| {
            this.text_color(theme.text_disabled)
                .border_1()
                .border_color(theme.control_border)
        })
        .child(label.to_owned())
}

/// The buttons of a dialog's bottom row, by their place in it. The caller says what a button
/// is; the row decides where it stands.
#[derive(Default)]
pub struct DialogButtons {
    extra: Vec<AnyElement>,
    primary: Vec<AnyElement>,
    other: Vec<AnyElement>,
    apply: Option<AnyElement>,
}

impl DialogButtons {
    pub fn new() -> Self {
        Self::default()
    }

    /// A button at the left edge: Preview, Test Connection, Help.
    pub fn extra(mut self, button: impl IntoElement) -> Self {
        self.extra.push(button.into_any_element());
        self
    }

    /// A primary button, the one Enter presses: OK, Push, Commit, Run, Create. A dialog with
    /// a question may offer more than one; they stand in the order they were given.
    pub fn primary(mut self, button: impl IntoElement) -> Self {
        self.primary.push(button.into_any_element());
        self
    }

    /// A button of the right group after the primary one: Cancel, Close.
    pub fn other(mut self, button: impl IntoElement) -> Self {
        self.other.push(button.into_any_element());
        self
    }

    /// Apply: the last button of the right group.
    pub fn apply(mut self, button: impl IntoElement) -> Self {
        self.apply = Some(button.into_any_element());
        self
    }

    /// The row. It adds no padding: the content of the dialog gives the inset from the edges.
    pub fn into_row(self) -> Div {
        let right = in_order(self.primary, self.other, self.apply, mirrored());
        div()
            .flex()
            .flex_none()
            .items_center()
            .justify_between()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(GAP))
                    .children(self.extra),
            )
            .child(div().flex().items_center().gap(px(GAP)).children(right))
    }
}

/// The right group from the left edge: the primary buttons, the others, then Apply. A mirrored
/// group runs the other way, so Cancel stands left of OK.
fn in_order<T>(primary: Vec<T>, other: Vec<T>, apply: Option<T>, mirror: bool) -> Vec<T> {
    let mut row: Vec<T> = primary.into_iter().chain(other).chain(apply).collect();
    if mirror {
        row.reverse();
    }
    row
}

/// Whether the right group is mirrored: macOS puts the default button at the right edge.
/// The tests keep the Windows order on every system.
fn mirrored() -> bool {
    cfg!(target_os = "macos") && !cfg!(any(test, feature = "test-support"))
}

#[cfg(test)]
mod tests {
    use super::in_order;

    #[test]
    fn the_right_group_stands_ok_cancel_apply_from_the_left() {
        assert_eq!(
            in_order(vec!["OK"], vec!["Cancel"], Some("Apply"), false),
            ["OK", "Cancel", "Apply"]
        );
    }

    #[test]
    fn a_dialog_without_apply_keeps_the_pair_in_the_same_order() {
        assert_eq!(
            in_order(vec!["OK"], vec!["Cancel"], None, false),
            ["OK", "Cancel"]
        );
    }

    #[test]
    fn a_mirrored_group_puts_cancel_left_of_ok() {
        assert_eq!(
            in_order(vec!["OK"], vec!["Cancel"], None, true),
            ["Cancel", "OK"]
        );
        assert_eq!(
            in_order(vec!["OK"], vec!["Cancel"], Some("Apply"), true),
            ["Apply", "Cancel", "OK"]
        );
    }

    #[test]
    fn a_question_with_two_primaries_keeps_them_in_the_order_given() {
        assert_eq!(
            in_order(vec!["Reload", "Retry"], vec!["Cancel"], None, false),
            ["Reload", "Retry", "Cancel"]
        );
    }
}
