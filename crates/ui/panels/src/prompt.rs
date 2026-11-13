//! A one-line question in a popup, and the plain text field the git windows use.

use std::rc::Rc;

use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::Workspace;

pub use ui_kit::TextField;

type Submit = Rc<dyn Fn(String, &mut App)>;

pub struct Prompt {
    title: SharedString,
    hint: SharedString,
    /// A question to confirm with Enter instead of a field.
    confirm: bool,
    field: TextField,
    focus: FocusHandle,
    workspace: WeakEntity<Workspace>,
    submit: Submit,
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
        Self::open(title, hint, "", false, submit, workspace, window, cx);
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
        Self::open(title, "", text, false, submit, workspace, window, cx);
    }

    /// A question that Enter answers yes to, and Escape or a click elsewhere no.
    pub fn ask(
        title: &str,
        question: &str,
        submit: impl Fn(&mut App) + 'static,
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        let submit = move |_: String, cx: &mut App| submit(cx);
        Self::open(title, question, "", true, submit, workspace, window, cx);
    }

    #[allow(clippy::too_many_arguments)]
    fn open(
        title: &str,
        hint: &str,
        text: &str,
        confirm: bool,
        submit: impl Fn(String, &mut App) + 'static,
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        let weak = cx.weak_entity();
        let (title, hint) = (title.to_string(), hint.to_string());
        let text = text.to_string();
        let view = cx.new(|cx| Self {
            title: title.into(),
            hint: hint.into(),
            confirm,
            field: TextField::from_text(text),
            focus: cx.focus_handle(),
            workspace: weak,
            submit: Rc::new(submit),
        });
        let focus = view.read(cx).focus.clone();
        workspace.show_overlay(view.into(), focus, window, cx);
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key == "enter" {
            let text = self.field.text.trim().to_string();
            if self.confirm || !text.is_empty() {
                (self.submit)(text, cx);
            }
            self.close(window, cx);
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
        let left = (window.viewport_size().width - width) / 2.;
        let top = window.viewport_size().height * 0.25;
        let empty = self.field.text.is_empty();
        let color = if empty {
            theme.text_secondary
        } else {
            theme.text
        };
        let body = surface(&theme)
            .id("prompt")
            .debug_selector(|| "prompt".to_owned())
            .track_focus(&self.focus)
            .occlude()
            .w(width)
            .p(px(DIALOG_PADDING))
            .gap(px(DIALOG_GAP))
            .on_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)))
            .on_mouse_down_out(cx.listener(|this, _, window, cx| this.close(window, cx)))
            .child(
                div()
                    .text_size(px(TEXT_TITLE))
                    .font_weight(FontWeight::MEDIUM)
                    .child(self.title.clone()),
            )
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
            });
        div()
            .absolute()
            .left(left)
            .top(top)
            .child(enter("prompt-enter", body))
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
