//! A one-line question in a popup, and the plain text field the git windows use.

use std::rc::Rc;

use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::Workspace;

pub use ui_kit::TextField;

/// What Enter of a prompt without an option sends back.
type Submit = Rc<dyn Fn(String, &mut App)>;

/// What Enter of a prompt with an option sends back.
type SubmitOption = Rc<dyn Fn(String, bool, &mut App)>;

/// What Enter of a prompt with a list of kinds sends back.
type SubmitKind = Rc<dyn Fn(String, usize, &mut App)>;

/// What Enter of the prompt sends back: the text, the tick of the option row when the
/// prompt has one, and the kind of the list when it has one.
enum Answer {
    Text(Submit),
    TextAndOption(SubmitOption),
    TextAndKind(SubmitKind),
}

/// One row of the list under the field of a prompt, such as the kinds of a new source file.
#[derive(Clone)]
pub struct PromptKind {
    pub label: SharedString,
    pub icon: &'static str,
}

/// Where a prompt stands: in the middle of the window, or over the part of the window
/// that asked, with the top center of the prompt at the point.
#[derive(Clone, Copy, Debug)]
pub enum Place {
    Center,
    At(Point<Pixels>),
}

pub struct Prompt {
    title: SharedString,
    hint: SharedString,
    /// A question to confirm with Enter instead of a field.
    confirm: bool,
    field: TextField,
    /// The tick under the field and whether it is on. New Branch asks for Checkout.
    option: Option<(SharedString, bool)>,
    /// The kinds the field can create, and the one the keys are on.
    kinds: Vec<PromptKind>,
    kind: usize,
    /// Where the prompt stands on screen.
    place: Place,
    focus: FocusHandle,
    workspace: WeakEntity<Workspace>,
    submit: Answer,
}

impl Prompt {
    pub fn show(
        title: &str,
        hint: &str,
        submit: impl Fn(String, &mut App) + 'static,
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        Self::open(
            title,
            hint,
            "",
            false,
            None,
            Vec::new(),
            Answer::Text(Rc::new(submit)),
            workspace,
            window,
            cx,
        );
    }

    /// Like [`Prompt::show`] with `text` already in the field, for a rename.
    pub fn show_with(
        title: &str,
        text: &str,
        submit: impl Fn(String, &mut App) + 'static,
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        Self::open(
            title,
            "",
            text,
            false,
            None,
            Vec::new(),
            Answer::Text(Rc::new(submit)),
            workspace,
            window,
            cx,
        );
    }

    /// Like [`Prompt::show`] with a tick under the field, on by `checked`, whose state
    /// reaches `submit` with the text: New Branch of the log asks whether to check out.
    #[allow(clippy::too_many_arguments)]
    pub fn show_with_option(
        title: &str,
        hint: &str,
        option: &str,
        checked: bool,
        submit: impl Fn(String, bool, &mut App) + 'static,
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        Self::open(
            title,
            hint,
            "",
            false,
            Some((option.to_string().into(), checked)),
            Vec::new(),
            Answer::TextAndOption(Rc::new(submit)),
            workspace,
            window,
            cx,
        );
    }

    /// Like [`Prompt::show`] with the list of kinds under the field: the arrows move the
    /// chosen kind, and Enter sends it with the text. The dialog of a new class asks this
    /// way.
    #[allow(clippy::too_many_arguments)]
    pub fn show_with_kinds(
        title: &str,
        hint: &str,
        kinds: Vec<PromptKind>,
        chosen: usize,
        submit: impl Fn(String, usize, &mut App) + 'static,
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        Self::open_kinds(
            title,
            hint,
            kinds,
            chosen,
            Answer::TextAndKind(Rc::new(submit)),
            workspace,
            window,
            cx,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn open_kinds(
        title: &str,
        hint: &str,
        kinds: Vec<PromptKind>,
        chosen: usize,
        submit: Answer,
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        Self::open_at(
            title,
            hint,
            "",
            false,
            None,
            kinds,
            chosen,
            submit,
            Place::Center,
            workspace,
            window,
            cx,
        );
    }

    /// A question that Enter answers yes to, and Escape, Cancel or a click elsewhere no.
    pub fn ask(
        title: &str,
        question: &str,
        submit: impl Fn(&mut App) + 'static,
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        Self::ask_at(
            title,
            question,
            Place::Center,
            submit,
            workspace,
            window,
            cx,
        );
    }

    /// [`Prompt::ask`] over the part of the window that asked, such as the terminal a
    /// paste goes to: the top center of the prompt stands at the point.
    pub fn ask_at(
        title: &str,
        question: &str,
        place: Place,
        submit: impl Fn(&mut App) + 'static,
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        let submit = move |_: String, cx: &mut App| submit(cx);
        Self::open_at(
            title,
            question,
            "",
            true,
            None,
            Vec::new(),
            0,
            Answer::Text(Rc::new(submit)),
            place,
            workspace,
            window,
            cx,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn open(
        title: &str,
        hint: &str,
        text: &str,
        confirm: bool,
        option: Option<(SharedString, bool)>,
        kinds: Vec<PromptKind>,
        submit: Answer,
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        Self::open_at(
            title,
            hint,
            text,
            confirm,
            option,
            kinds,
            0,
            submit,
            Place::Center,
            workspace,
            window,
            cx,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn open_at(
        title: &str,
        hint: &str,
        text: &str,
        confirm: bool,
        option: Option<(SharedString, bool)>,
        kinds: Vec<PromptKind>,
        chosen: usize,
        submit: Answer,
        place: Place,
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        let weak = cx.weak_entity();
        let (title, hint) = (title.to_string(), hint.to_string());
        let text = text.to_string();
        let chosen = chosen.min(kinds.len().saturating_sub(1));
        let view = cx.new(move |cx| Self {
            title: title.into(),
            hint: hint.into(),
            confirm,
            field: TextField::from_text(text),
            option,
            kinds,
            kind: chosen,
            place,
            focus: cx.focus_handle(),
            workspace: weak,
            submit,
        });
        let focus = view.read(cx).focus.clone();
        workspace.show_overlay(view.into(), focus, window, cx);
    }

    /// Pressing Enter, Paste or another answer of a confirmation: the question is
    /// answered once, and the prompt goes.
    fn confirm_answer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.field.text.trim().to_string();
        if !self.confirm && text.is_empty() {
            return;
        }
        match &self.submit {
            Answer::Text(submit) => submit(text, cx),
            Answer::TextAndOption(submit) => {
                let checked = self.option.as_ref().is_some_and(|(_, on)| *on);
                submit(text, checked, cx);
            }
            Answer::TextAndKind(submit) => submit(text, self.kind, cx),
        }
        self.close(window, cx);
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key == "enter" {
            self.confirm_answer(window, cx);
        } else if event.keystroke.key == "escape" {
            self.close(window, cx);
        } else if !self.kinds.is_empty() && event.keystroke.key == "down" {
            self.kind = (self.kind + 1).min(self.kinds.len() - 1);
            cx.stop_propagation();
            cx.notify();
        } else if !self.kinds.is_empty() && event.keystroke.key == "up" {
            self.kind = self.kind.saturating_sub(1);
            cx.stop_propagation();
            cx.notify();
        } else if !self.confirm && self.field.key_with_clipboard(event, false, cx) {
            cx.stop_propagation();
            cx.notify();
        }
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace
            .update(cx, |workspace, cx| workspace.dismiss(window, cx))
            .ok();
    }
}

impl Render for Prompt {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let width = px(420.);
        let viewport = window.viewport_size();
        let (left, top) = match self.place {
            Place::Center => ((viewport.width - width) / 2., viewport.height * 0.25),
            Place::At(at) => (
                (at.x - width / 2.).clamp(
                    px(SPACE_M),
                    (viewport.width - width - px(SPACE_M)).max(px(SPACE_M)),
                ),
                at.y.clamp(px(SPACE_M), (viewport.height - px(120.)).max(px(SPACE_M))),
            ),
        };
        let empty = self.field.text.is_empty();
        let color = if empty {
            theme.text_secondary
        } else {
            theme.text
        };
        let option = self.option.clone().map(|(label, on)| {
            let hover = theme.hover;
            div()
                .id("prompt-option")
                .debug_selector(|| "prompt-option".to_owned())
                .flex()
                .items_center()
                .gap(px(SPACE_S))
                .flex_none()
                .h(px(CONTROL_HEIGHT))
                .px(px(SPACE_XS))
                .rounded(px(ROW_RADIUS))
                .cursor_pointer()
                .hover(move |style| style.bg(hover))
                .child(checkbox(on, &theme))
                .child(label)
                .on_click(cx.listener(|this, _, _, cx| {
                    if let Some((_, on)) = &mut this.option {
                        *on = !*on;
                    }
                    cx.notify();
                }))
        });
        let kinds = (!self.kinds.is_empty()).then(|| {
            let rows = self.kinds.iter().enumerate().map(|(index, kind)| {
                let chosen = index == self.kind;
                let label = kind.label.clone();
                let row_icon = kind.icon;
                div()
                    .id(("prompt-kind", index))
                    .debug_selector(move || format!("prompt-kind-{index}"))
                    .flex()
                    .items_center()
                    .gap(px(SPACE_S))
                    .h(px(CONTROL_HEIGHT))
                    .px(px(SPACE_M))
                    .rounded(px(ROW_RADIUS))
                    .cursor_pointer()
                    .text_color(theme.text)
                    .when(chosen, |this| this.bg(theme.selection))
                    .when(!chosen, |this| {
                        let hover = theme.hover;
                        this.hover(move |style| style.bg(hover))
                    })
                    .child(icon(row_icon, &theme))
                    .child(label)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.kind = index;
                        cx.notify();
                    }))
            });
            div()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(
                    div()
                        .text_size(px(TEXT_SMALL))
                        .text_color(theme.text_muted)
                        .child("Kind"),
                )
                .children(rows)
                .into_any_element()
        });
        let title = self.title.clone();
        let shell = surface(&theme)
            .id("prompt")
            .debug_selector(|| "prompt".to_owned())
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)));
        let contents = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .p(px(DIALOG_PADDING))
            .gap(px(DIALOG_GAP))
            .child(if self.confirm {
                div()
                    .text_color(theme.text_secondary)
                    .child(self.hint.clone())
                    .into_any_element()
            } else {
                div()
                    .flex()
                    .items_center()
                    .h(px(33.))
                    .px(px(SPACE_M))
                    .rounded(px(CONTROL_RADIUS))
                    .border_2()
                    .border_color(theme.accent)
                    .text_color(color)
                    .children(text_field_children_named(
                        FieldMouse::new(cx, |this| Some(&mut this.field)),
                        "prompt-field",
                        &self.field,
                        theme.text,
                        theme.text,
                        theme.selection,
                        theme.caret,
                        18.,
                        true,
                    ))
                    .when(empty, |this| this.child(self.hint.clone()))
                    .into_any_element()
            })
            .children(option)
            .children(kinds);
        let buttons = self.confirm.then(|| {
            let label = self.title.to_string();
            DialogButtons::new()
                .primary(
                    dialog_button(
                        ("prompt-confirm", 0usize),
                        "prompt-confirm".to_owned(),
                        &label,
                        true,
                        true,
                        &theme,
                    )
                    .on_click(cx.listener(|this, _, window, cx| this.confirm_answer(window, cx))),
                )
                .other(
                    dialog_button(
                        ("prompt-cancel", 0usize),
                        "prompt-cancel".to_owned(),
                        "Cancel",
                        false,
                        true,
                        &theme,
                    )
                    .on_click(cx.listener(|this, _, window, cx| this.close(window, cx))),
                )
                .into_row()
        });
        let contents = contents.children(buttons);
        let close = cx.listener(|this, _: &ClickEvent, window, cx| this.close(window, cx));
        dialog_window(
            DialogSpec {
                key: "prompt",
                title,
                extra: None,
                place: DialogPlace {
                    left: f32::from(left),
                    top: f32::from(top),
                    width: f32::from(width),
                    height: None,
                },
                min: (320., 120.),
            },
            shell,
            window,
            cx,
            close,
            contents,
        )
    }
}

#[cfg(test)]
mod tests {
    use ui_kit::{KeyDownEvent, Keystroke, Modifiers};

    use super::TextField;

    fn press(key: &str, key_char: Option<&str>, modifiers: Modifiers) -> KeyDownEvent {
        KeyDownEvent {
            keystroke: Keystroke {
                modifiers,
                key: key.into(),
                key_char: key_char.map(Into::into),
            },
            is_held: false,
            prefer_character_input: false,
        }
    }

    #[test]
    fn text_goes_in_and_backspace_takes_it_out() {
        let mut field = TextField::default();
        assert!(field.key(&press("a", Some("a"), Modifiers::default()), false));
        assert!(field.key(&press("b", Some("b"), Modifiers::default()), false));
        assert!(field.key(&press("backspace", None, Modifiers::default()), false));
        assert_eq!(field.text, "a");
    }

    #[test]
    fn shortcuts_are_not_text_and_enter_needs_a_multiline_field() {
        let mut field = TextField::default();
        let control = Modifiers {
            control: true,
            ..Modifiers::default()
        };
        assert!(!field.key(&press("s", Some("s"), control), false));
        assert!(!field.key(&press("enter", Some("\n"), Modifiers::default()), false));
        assert!(field.key(&press("enter", Some("\n"), Modifiers::default()), true));
        assert_eq!(field.text, "\n");
    }
}
