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

fn dialog_button(
    id: &'static str,
    label: &'static str,
    primary: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let (hover, pressed) = (theme.hover, theme.pressed);
    div()
        .id(id)
        .flex()
        .items_center()
        .h(px(CONTROL_HEIGHT))
        .px(px(SPACE_L))
        .rounded(px(CONTROL_RADIUS))
        .when(primary, |this| {
            this.bg(theme.accent).text_color(theme.over_accent)
        })
        .when(!primary, |this| {
            this.border_1()
                .border_color(theme.control_border)
                .hover(move |style| style.bg(hover))
                .active(move |style| style.bg(pressed))
        })
        .child(label)
}

impl Render for TrustDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let viewport = window.viewport_size();
        let width = px(WIDTH);
        let left = (viewport.width - width) / 2.;
        let top = viewport.height * 0.25;
        let folder = self.project.display().to_string();
        let body = surface(&theme)
            .id("trust-project")
            .key_context("TrustDialog")
            .track_focus(&self.focus)
            .occlude()
            .w(width)
            .p(px(DIALOG_PADDING))
            .gap(px(DIALOG_GAP))
            .text_size(px(UI_SIZE))
            .text_color(theme.text)
            .on_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)))
            .child(
                div()
                    .text_size(px(TEXT_TITLE))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Trust this project?"),
            )
            .child(div().text_color(theme.text_muted).child(folder))
            .child(div().text_color(theme.text_secondary).child(
                "An AI agent runs commands and edits files in this folder. \
                 Start agents only in projects you trust.",
            ))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(SPACE_M))
                    .child(
                        dialog_button("trust-cancel", "Don't Trust", false, &theme)
                            .on_click(cx.listener(|this, _, window, cx| this.close(window, cx))),
                    )
                    .child(
                        dialog_button("trust-accept", "Trust Project", true, &theme)
                            .on_click(cx.listener(|this, _, window, cx| this.trust(window, cx))),
                    ),
            );
        div()
            .absolute()
            .left(left)
            .top(top)
            .child(enter("trust-enter", body))
    }
}
