//! The dialog that asks whether to trust a project. An agent runs commands in the project
//! folder, so the first session of a project asks first; `store::TrustList` keeps the answer.

use std::path::PathBuf;

use store::TrustList;
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::Workspace;

const WIDTH: f32 = 460.;

type OnTrust = Box<dyn FnOnce(&mut Window, &mut App)>;

pub struct TrustDialog {
    project: PathBuf,
    store: TrustList,
    focus: FocusHandle,
    workspace: WeakEntity<Workspace>,
    on_trust: Option<OnTrust>,
}

impl TrustDialog {
    /// Shows the dialog; `on_trust` runs once the user trusts the project.
    pub fn show(
        project: PathBuf,
        store: TrustList,
        workspace: &mut Workspace,
        on_trust: impl FnOnce(&mut Window, &mut App) + 'static,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        let weak = cx.weak_entity();
        let view = cx.new(|cx| Self {
            project,
            store,
            focus: cx.focus_handle(),
            workspace: weak,
            on_trust: Some(Box::new(on_trust)),
        });
        let focus = view.read(cx).focus.clone();
        workspace.show_overlay(view.into(), focus, window, cx);
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace
            .update(cx, |workspace, cx| workspace.dismiss(window, cx))
            .ok();
    }

    fn trust(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(error) = self.store.trust(&self.project) {
            let text = format!("{}: {error}", self.project.display());
            self.workspace
                .update(cx, |workspace, cx| {
                    workspace.notify_user("Trust is not saved", text, cx)
                })
                .ok();
        }
        self.close(window, cx);
        if let Some(on_trust) = self.on_trust.take() {
            on_trust(window, cx);
        }
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "enter" => self.trust(window, cx),
            "escape" => self.close(window, cx),
            _ => return,
        }
        cx.stop_propagation();
    }
}

impl Render for TrustDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let viewport = window.viewport_size();
        let width = px(WIDTH);
        let left = (viewport.width - width) / 2.;
        let top = viewport.height * 0.25;
        let folder = self.project.display().to_string();
        let shell = surface(&theme)
            .id("trust-project")
            .key_context("TrustDialog")
            .track_focus(&self.focus)
            .text_size(px(UI_SIZE))
            .text_color(theme.text)
            .on_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)));
        let contents = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .p(px(DIALOG_PADDING))
            .gap(px(DIALOG_GAP))
            .child(div().text_color(theme.text_muted).child(folder))
            .child(div().text_color(theme.text_secondary).child(
                "An AI agent runs commands and edits files in this folder. \
                 Start agents only in projects you trust.",
            ))
            .child(
                DialogButtons::new()
                    .primary(
                        dialog_button(
                            "trust-accept",
                            "trust-accept".to_owned(),
                            "Trust Project",
                            true,
                            true,
                            &theme,
                        )
                        .on_click(cx.listener(|this, _, window, cx| this.trust(window, cx))),
                    )
                    .other(
                        dialog_button(
                            "trust-cancel",
                            "trust-cancel".to_owned(),
                            "Don't Trust",
                            false,
                            true,
                            &theme,
                        )
                        .on_click(cx.listener(|this, _, window, cx| this.close(window, cx))),
                    )
                    .into_row(),
            );
        let close = cx.listener(|this, _: &ClickEvent, window, cx| this.close(window, cx));
        dialog_window(
            DialogSpec {
                key: "trust",
                title: "Trust this project?".into(),
                extra: None,
                place: DialogPlace {
                    left: f32::from(left),
                    top: f32::from(top),
                    width: f32::from(width),
                    height: None,
                },
                min: (360., 200.),
            },
            shell,
            window,
            cx,
            close,
            contents,
        )
    }
}
