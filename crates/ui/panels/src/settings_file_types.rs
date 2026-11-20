//! Settings -> Editor -> File Types: the masks of the files and folders the project tree
//! and the file index leave out. The window has no registry of file types of its own yet,
//! so the page keeps the ignored names only.

use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;

use crate::prompt::TextField;
use crate::settings::{SettingsView, note};

/// The row that names the text field of the page in the event of a key.
pub(crate) const IGNORED_FIELD: &str = "ignored_files";
/// The hint of the empty field.
pub(crate) const IGNORED_HINT: &str = "*.class, .git, node_modules";

#[derive(Default)]
pub(crate) struct FileTypesPage {
    pub(crate) pattern: TextField,
}

impl FileTypesPage {
    /// The rows of the page: the field that takes one more mask and the masks in force.
    pub(crate) fn render(
        &self,
        patterns: &[String],
        active: bool,
        theme: &Theme,
        cx: &mut Context<SettingsView>,
    ) -> Div {
        let input = pattern_field(&self.pattern, IGNORED_HINT, active, theme)
            .debug_selector(|| "settings-ignore-pattern".to_owned())
            .on_click(cx.listener(|this, _, _, cx| {
                this.start_ignored_pattern(cx);
            }));
        let add = div()
            .id("settings-ignore-add")
            .debug_selector(|| "settings-ignore-add".to_owned())
            .flex()
            .items_center()
            .justify_center()
            .h(px(CONTROL_HEIGHT))
            .px(px(SPACE_L))
            .rounded(px(CONTROL_RADIUS))
            .bg(theme.accent)
            .text_color(theme.over_accent)
            .child("Add")
            .on_click(cx.listener(|this, _, _, cx| this.add_ignored_pattern(cx)));
        let rows: Vec<Stateful<Div>> = patterns
            .iter()
            .enumerate()
            .map(|(index, pattern)| {
                div()
                    .id(("settings-ignore-row", index))
                    .debug_selector(move || format!("settings-ignore-row-{index}"))
                    .flex()
                    .items_center()
                    .justify_between()
                    .h(px(ROW_HEIGHT))
                    .px(px(ROW_PADDING_X))
                    .rounded(px(ROW_RADIUS))
                    .hover(|style| style.bg(theme.selection_hover))
                    .child(pattern.clone())
                    .child(
                        div()
                            .id(("settings-ignore-remove", index))
                            .debug_selector(move || format!("settings-ignore-remove-{index}"))
                            .px(px(SPACE_M))
                            .h(px(CONTROL_HEIGHT))
                            .flex()
                            .items_center()
                            .rounded(px(CONTROL_RADIUS))
                            .hover(|style| style.bg(theme.hover))
                            .child("Remove")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.remove_ignored_pattern(index, cx);
                            })),
                    )
            })
            .collect();
        div()
            .flex()
            .flex_col()
            .gap(px(SPACE_S))
            .child(note(
                "A mask is a file or folder name: * is any run of characters and ? is one character.",
                theme,
            ))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(SPACE_M))
                    .child(input)
                    .child(add),
            )
            .child(div().flex().flex_col().children(rows))
    }
}

/// The field that takes one more mask. Its caret and its selection are named for the
/// tests of the window, as every text field of the settings is.
fn pattern_field(
    value: &TextField,
    hint: &'static str,
    active: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let empty = value.text.is_empty();
    let color = if empty {
        theme.text_secondary
    } else {
        theme.text
    };
    div()
        .id("settings-ignore-pattern")
        .flex()
        .items_center()
        .flex_1()
        .h(px(CONTROL_HEIGHT))
        .px(px(SPACE_M))
        .rounded(px(CONTROL_RADIUS))
        .border_2()
        .border_color(if active { theme.accent } else { theme.border })
        .text_color(color)
        .children(text_field_children_named(
            "settings-field",
            value,
            theme.text,
            theme.text,
            theme.selection,
            theme.caret,
            16.,
            active,
        ))
        .when(empty, |this| this.child(hint))
}
