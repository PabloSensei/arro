//! Analyze Stack Trace: a dialog with the text of the clipboard in a field. OK fills the
//! Stack Trace tool window, which reads the frames with the language.

use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::{STACK_TRACE_TOOL, Workspace};

use crate::project_structure::dialog_button;
use crate::prompt::TextField;
use crate::stack_traces::ActiveStackTrace;

const WIDTH: f32 = 760.;
const HEIGHT: f32 = 480.;
/// The height of the field above the buttons.
const FIELD_HEIGHT: f32 = 320.;

pub(crate) struct StackTraceDialog {
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    field: TextField,
    scroll: ScrollHandle,
}

impl StackTraceDialog {
    pub(crate) fn show(
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        let text = cx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .unwrap_or_default();
        let weak = cx.weak_entity();
        let view = cx.new(|cx| Self {
            workspace: weak,
            focus: cx.focus_handle(),
            field: TextField::from_text(text),
            scroll: ScrollHandle::new(),
        });
        let focus = view.read(cx).focus.clone();
        workspace.show_overlay(view.into(), focus, window, cx);
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace
            .update(cx, |workspace, cx| workspace.dismiss(window, cx))
            .ok();
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.field.text.clone();
        if text.trim().is_empty() {
            return;
        }
        let Some(panel) = cx
            .try_global::<ActiveStackTrace>()
            .map(|panel| panel.0.clone())
        else {
            return;
        };
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.dismiss(window, cx);
                workspace.show_tool(STACK_TRACE_TOOL, window, cx);
            })
            .ok();
        panel.update(cx, |panel, cx| panel.analyze(text, cx));
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let stroke = &event.keystroke;
        if stroke.key == "escape" {
            self.close(window, cx);
            return;
        }
        if stroke.key == "enter" && (stroke.modifiers.control || stroke.modifiers.platform) {
            self.submit(window, cx);
            return;
        }
        if self.field.key_with_clipboard(event, true, cx) {
            cx.stop_propagation();
            cx.notify();
        }
    }
}

impl Render for StackTraceDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let viewport = window.viewport_size();
        let width = px(WIDTH).min(viewport.width - px(40.));
        let height = px(HEIGHT).min(viewport.height - px(40.));
        let left = (viewport.width - width) / 2.;
        let top = (viewport.height - height) / 2.;
        let empty = self.field.text.trim().is_empty();
        let field = div()
            .id("stack-trace-field")
            .debug_selector(|| "stack-trace-field".to_owned())
            .flex()
            .flex_col()
            .h(px(FIELD_HEIGHT))
            .rounded(px(CONTROL_RADIUS))
            .border_1()
            .border_color(theme.control_border)
            .overflow_hidden()
            .child(
                scrollable(
                    "stack-trace-input",
                    &self.scroll,
                    div()
                        .id("stack-trace-input")
                        .flex()
                        .flex_col()
                        .p(px(SPACE_S))
                        .font_family(MONO_FONT)
                        .text_size(px(CODE_SIZE))
                        .overflow_y_scroll()
                        .track_scroll(&self.scroll)
                        .children(text_field_lines_named(
                            "stack-trace-field",
                            &self.field,
                            theme.text,
                            theme.text,
                            theme.selection,
                            theme.caret,
                            18.,
                            true,
                        ))
                        .when(empty, |this| {
                            this.child(
                                div()
                                    .px(px(SPACE_S))
                                    .text_color(theme.text_secondary)
                                    .child("Paste a stack trace or a thread dump"),
                            )
                        }),
                    &theme,
                    window,
                    cx,
                )
                .size_full(),
            );
        let shell = surface(&theme)
            .id("stack-trace-dialog")
            .debug_selector(|| "stack-trace-dialog".to_owned())
            .key_context("StackTraceDialog")
            .track_focus(&self.focus)
            .text_size(px(UI_SIZE))
            .text_color(theme.text)
            .on_key_down(
                cx.listener(|this, event: &KeyDownEvent, window, cx| this.key(event, window, cx)),
            );
        let contents = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .p(px(DIALOG_PADDING))
            .gap(px(DIALOG_GAP))
            .child(
                div()
                    .text_color(theme.text_secondary)
                    .child("Frames of the project open their source; the frames of libraries and of the runtime stay grey."),
            )
            .child(field)
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(SPACE_M))
                    .child(
                        dialog_button(
                            "stack-trace-cancel",
                            "stack-trace-cancel".to_owned(),
                            "Cancel",
                            false,
                            true,
                            &theme,
                        )
                        .on_click(cx.listener(|this, _, window, cx| this.close(window, cx))),
                    )
                    .child(
                        dialog_button(
                            "stack-trace-ok",
                            "stack-trace-ok".to_owned(),
                            "OK",
                            true,
                            !empty,
                            &theme,
                        )
                        .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
                    ),
            );
        let close = cx.listener(|this, _: &ClickEvent, window, cx| this.close(window, cx));
        dialog_window(
            DialogSpec {
                key: "stack-trace",
                title: "Analyze Stack Trace".into(),
                extra: None,
                place: DialogPlace {
                    left: f32::from(left),
                    top: f32::from(top),
                    width: f32::from(width),
                    height: Some(f32::from(height)),
                },
                min: (480., 320.),
            },
            shell,
            window,
            cx,
            close,
            contents,
        )
    }
}
