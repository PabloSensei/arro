//! A dialog that asks a question with a few answers, for closing, reloading and other
//! steps that must not happen without the user.

use std::rc::Rc;

use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::Workspace;

use crate::project_structure::dialog_button;

const WIDTH: f32 = 460.;

/// What a button of the dialog does after the dialog closes.
pub(crate) type ChoiceAction = Rc<dyn Fn(&mut Workspace, &mut Window, &mut Context<Workspace>)>;

/// One answer of the dialog.
pub(crate) struct DialogChoice {
    pub label: &'static str,
    pub primary: bool,
    pub action: ChoiceAction,
}

impl DialogChoice {
    pub(crate) fn new(
        label: &'static str,
        primary: bool,
        action: impl Fn(&mut Workspace, &mut Window, &mut Context<Workspace>) + 'static,
    ) -> Self {
        Self {
            label,
            primary,
            action: Rc::new(action),
        }
    }
}

/// A question over the window with the answers the caller gives.
pub(crate) struct ChoiceDialog {
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    title: SharedString,
    text: SharedString,
    choices: Vec<DialogChoice>,
}

impl ChoiceDialog {
    pub(crate) fn show(
        title: impl Into<SharedString>,
        text: impl Into<SharedString>,
        choices: Vec<DialogChoice>,
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        let weak = cx.weak_entity();
        let view = cx.new(|cx| Self {
            workspace: weak,
            focus: cx.focus_handle(),
            title: title.into(),
            text: text.into(),
            choices,
        });
        let focus = view.read(cx).focus.clone();
        workspace.show_overlay(view.into(), focus, window, cx);
    }

    fn choose(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(choice) = self.choices.get(index) else {
            return;
        };
        let action = choice.action.clone();
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.dismiss(window, cx);
                action(workspace, window, cx);
            })
            .ok();
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace
            .update(cx, |workspace, cx| workspace.dismiss(window, cx))
            .ok();
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "escape" => self.close(window, cx),
            "enter" => {
                let index = self
                    .choices
                    .iter()
                    .position(|choice| choice.primary)
                    .unwrap_or(0);
                self.choose(index, window, cx);
            }
            _ => {}
        }
    }
}

impl Render for ChoiceDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let viewport = window.viewport_size();
        let width = px(WIDTH);
        let left = (viewport.width - width) / 2.;
        let top = viewport.height * 0.3;

        let buttons = self
            .choices
            .iter()
            .enumerate()
            .map(|(index, choice)| {
                let selector = format!("choice-{index}");
                dialog_button(
                    ("choice-button", index),
                    selector.clone(),
                    choice.label,
                    choice.primary,
                    true,
                    &theme,
                )
                .on_click(cx.listener(move |this, _, window, cx| this.choose(index, window, cx)))
            })
            .collect::<Vec<_>>();

        div().absolute().left(left).top(top).child(
            surface(&theme)
                .id("choice-dialog")
                .debug_selector(|| "choice-dialog".to_owned())
                .key_context("ChoiceDialog")
                .track_focus(&self.focus)
                .occlude()
                .w(width)
                .p(px(DIALOG_PADDING))
                .gap(px(DIALOG_GAP))
                .text_size(px(UI_SIZE))
                .text_color(theme.text)
                .on_key_down(
                    cx.listener(|this, event: &KeyDownEvent, window, cx| {
                        this.key(event, window, cx)
                    }),
                )
                .on_mouse_down_out(cx.listener(|this, _, window, cx| this.close(window, cx)))
                .child(
                    div()
                        .text_size(px(TEXT_TITLE))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(self.title.clone()),
                )
                .child(
                    div()
                        .text_color(theme.text_secondary)
                        .child(self.text.clone()),
                )
                .child(
                    div()
                        .flex()
                        .justify_end()
                        .gap(px(SPACE_M))
                        .children(buttons),
                ),
        )
    }
}
