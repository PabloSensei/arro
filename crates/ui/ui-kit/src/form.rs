//! The rows of a form. A form has one label column, as in IntelliJ IDEA on Windows: the labels
//! stand at the left edge, the fields start on one vertical line after the column and reach the
//! right edge, and every field is `CONTROL_HEIGHT` high. The width of the column is the width
//! of the widest label of the form, measured in the font of the labels, and the helpers take it
//! from [`form_label_width`]. A row without a label starts where the fields start.

use gpui_kit::prelude::*;
use gpui_kit::*;

use crate::Theme;
use crate::metrics::*;

/// The narrowest label column of a form, in logical pixels.
pub const FORM_LABEL_MIN: f32 = 96.;
/// The widest label column of a form. A label wider than this is cut at the edge of the column.
pub const FORM_LABEL_MAX: f32 = 200.;

/// The gap between the label column and the fields.
const FORM_GAP: f32 = SPACE_M;

/// The width of a label column that holds labels of the given widths: the widest of them, kept
/// between [`FORM_LABEL_MIN`] and [`FORM_LABEL_MAX`].
pub fn label_column_width(widths: &[f32]) -> f32 {
    widths
        .iter()
        .copied()
        .fold(FORM_LABEL_MIN, f32::max)
        .min(FORM_LABEL_MAX)
}

/// The width of the label column of a form whose labels are `labels`. The labels are measured
/// in the font and the size they are drawn with, so the column fits the widest of them.
pub fn form_label_width(window: &Window, labels: &[&str]) -> f32 {
    let widths: Vec<f32> = labels
        .iter()
        .map(|label| label_text_width(window, label))
        .collect();
    label_column_width(&widths)
}

/// The width of `text` in the font and the size of the labels.
fn label_text_width(window: &Window, text: &str) -> f32 {
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
        px(UI_SIZE),
        std::slice::from_ref(&run),
        None,
    );
    f32::from(line.width())
}

/// A label in a label column of `width` in `color`: on the height of a field, cut at the edge of
/// the column.
fn label_in(width: f32, text: impl IntoElement, color: Hsla) -> Div {
    div()
        .w(px(width))
        .flex_none()
        .h(px(CONTROL_HEIGHT))
        .flex()
        .items_center()
        .overflow_hidden()
        .whitespace_nowrap()
        .font_family(UI_FONT)
        .text_size(px(UI_SIZE))
        .text_color(color)
        .child(text)
}

/// A label in a label column of `width`: muted, on the height of a field, cut at the edge of the
/// column.
pub fn form_label(width: f32, text: &str, theme: &Theme) -> Div {
    label_in(width, text.to_owned(), theme.text_muted)
}

/// A label and a field on one row. The label stands in the column of `width`, and the field
/// starts after it and fills the rest of the row.
pub fn form_row(width: f32, label: &str, field: impl IntoElement, theme: &Theme) -> Div {
    form_row_colored(width, label, theme.text_muted, field)
}

/// [`form_row`] with the label in `color`, for the pages whose labels read as their text.
pub fn form_row_colored(width: f32, label: &str, color: Hsla, field: impl IntoElement) -> Div {
    form_row_labelled(width, label.to_owned(), color, field)
}

/// [`form_row_colored`] with a label of its own, such as one that marks the words of a search.
pub fn form_row_labelled(
    width: f32,
    label: impl IntoElement,
    color: Hsla,
    field: impl IntoElement,
) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(FORM_GAP))
        .child(label_in(width, label, color))
        .child(div().flex_1().min_w(px(0.)).child(field))
}

/// Content without a label that starts where the fields start: a checkbox, a list of choices,
/// or a row of buttons under a form.
pub fn form_aligned(width: f32, content: impl IntoElement) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(FORM_GAP))
        .child(div().w(px(width)).flex_none())
        .child(div().flex_1().min_w(px(0.)).child(content))
}

/// A hint under a field: small grey text that starts where the fields start.
pub fn form_hint(width: f32, text: &str, theme: &Theme) -> Div {
    form_aligned(
        width,
        div()
            .pt(px(SPACE_XS))
            .text_size(px(TEXT_SMALL))
            .text_color(theme.text_muted)
            .child(text.to_owned()),
    )
}

#[cfg(test)]
mod tests {
    use super::{FORM_LABEL_MAX, FORM_LABEL_MIN, label_column_width};

    #[test]
    fn the_column_is_as_wide_as_the_longest_label_within_the_bounds() {
        assert_eq!(label_column_width(&[40., 120., 80.]), 120.);
        assert_eq!(
            label_column_width(&[40.]),
            FORM_LABEL_MIN,
            "a short form keeps the narrowest column"
        );
        assert_eq!(
            label_column_width(&[260., 90.]),
            FORM_LABEL_MAX,
            "a long label is capped at the widest column"
        );
        assert_eq!(
            label_column_width(&[]),
            FORM_LABEL_MIN,
            "a form without labels"
        );
    }
}
