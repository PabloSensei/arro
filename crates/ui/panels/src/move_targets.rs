//! Moves one symbol into a place around it: the form opens at once and asks the
//! language for the places it can go to. The first place is chosen.

use futures_util::StreamExt;
use lang_api::{
    Confidence, DocumentSpot, MoveDestination, MoveKind, MoveRequest, MoveTargetsForm,
    MoveTargetsQuery, ask,
};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::Workspace;

const WIDTH: f32 = 520.;
const ROW_HEIGHT: f32 = 26.;

const NO_ANSWER: &str = "The language server did not answer";
const NO_SUPPORT: &str = "This language does not move methods";
const NO_TARGET: &str = "The language named no place to move the method to";
const NO_TARGETS: &str = "The method has no parameter or field of a project type";
const NO_EDITS: &str = "The server returned no edits";

/// The form of a move to a parameter or a field of the type of a symbol. It opens over
/// the workspace, so Escape and the buttons work without editing the file under it.
pub struct MoveTargetsDialog {
    spot: DocumentSpot,
    title: String,
    targets: Vec<MoveDestination>,
    active: usize,
    loading: bool,
    busy: bool,
    status: Option<String>,
    error: Option<String>,
    focus: FocusHandle,
    window: AnyWindowHandle,
    workspace: WeakEntity<Workspace>,
    /// The editor the move started in, whose Ctrl+Z takes the whole change back.
    origin: Option<Entity<editor_view::EditorView>>,
    /// The requests of the form: dropping one drops its replies, which cancels it.
    request: Option<Task<()>>,
}

impl MoveTargetsDialog {
    pub(crate) fn show(
        spot: DocumentSpot,
        origin: Option<Entity<editor_view::EditorView>>,
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        let weak = cx.weak_entity();
        let window_handle = window.window_handle();
        let view = cx.new(|cx| Self {
            spot,
            title: "Move Instance Method".to_owned(),
            targets: Vec::new(),
            active: 0,
            loading: true,
            busy: false,
            status: None,
            error: None,
            focus: cx.focus_handle(),
            window: window_handle,
            workspace: weak,
            origin,
            request: None,
        });
        let focus = view.read(cx).focus.clone();
        workspace.show_overlay(view.clone().into(), focus, window, cx);
        view.update(cx, |this, cx| this.load(cx));
    }

    /// Asks the language for the places the symbol can go to. The window is on screen
    /// before the answer arrives, and closing it drops the request.
    fn load(&mut self, cx: &mut Context<Self>) {
        let Some(provider) = cx
            .try_global::<editor_view::Semantic>()
            .and_then(|semantic| semantic.0.move_targets_form())
        else {
            self.loading = false;
            self.error = Some(NO_SUPPORT.to_owned());
            cx.notify();
            return;
        };
        let query = MoveTargetsQuery {
            spot: self.spot.clone(),
            kind: MoveKind::InstanceMethod,
        };
        let mut replies = ask(provider, query);
        let window = self.window;
        let workspace = self.workspace.clone();
        let origin = self.origin.clone();
        let point = self.spot.point;
        let task = cx.spawn(async move |this, cx| {
            let answer = replies.next().await;
            let nothing = this
                .update(cx, |this, cx| {
                    this.loading = false;
                    let mut nothing = false;
                    match answer {
                        Some(answer) if answer.confidence == Confidence::Unknown => {
                            this.error = Some(NO_ANSWER.to_owned());
                        }
                        Some(answer) => match answer.value {
                            Some(form) if !form.targets.is_empty() => this.form_arrived(form, cx),
                            _ => nothing = true,
                        },
                        None => this.error = Some(NO_ANSWER.to_owned()),
                    }
                    cx.notify();
                    nothing
                })
                .unwrap_or(false);
            if !nothing {
                return;
            }
            // No place takes the method: the form goes away and the window says why at
            // the caret, where the move started.
            window
                .update(cx, |_, window, cx| {
                    workspace
                        .update(cx, |workspace, cx| {
                            workspace.dismiss(window, cx);
                            if let Some(origin) = origin {
                                origin.update(cx, |view, cx| {
                                    view.note_at(point, NO_TARGETS.to_owned(), cx)
                                });
                            }
                        })
                        .ok();
                })
                .ok();
        });
        self.request = Some(task);
    }

    fn form_arrived(&mut self, form: MoveTargetsForm, cx: &mut Context<Self>) {
        self.title = form.title;
        self.targets = form.targets;
        self.active = 0;
        cx.notify();
    }

    /// Keys of the form: the arrows move between the places, Enter applies.
    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        let modifiers = event.keystroke.modifiers;
        if key == "escape" {
            self.close(window, cx);
            cx.stop_propagation();
            return;
        }
        if key == "enter" && !self.busy && !self.loading {
            self.refactor(window, cx);
            cx.stop_propagation();
            return;
        }
        if modifiers.alt || modifiers.control || modifiers.platform || self.targets.is_empty() {
            return;
        }
        let step = match key {
            "up" => -1,
            "down" | "tab" => {
                if modifiers.shift {
                    -1
                } else {
                    1
                }
            }
            _ => return,
        };
        let len = self.targets.len() as i32;
        self.active = (self.active as i32 + step).rem_euclid(len) as usize;
        self.status = None;
        self.error = None;
        cx.notify();
        cx.stop_propagation();
    }

    fn refactor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.loading {
            return;
        }
        let Some(destination) = self.targets.get(self.active).cloned() else {
            self.error = Some(NO_TARGET.to_owned());
            self.status = None;
            cx.notify();
            return;
        };
        let Some(provider) = cx
            .try_global::<editor_view::Semantic>()
            .and_then(|semantic| semantic.0.move_file())
        else {
            self.error = Some(NO_SUPPORT.to_owned());
            cx.notify();
            return;
        };
        let query = MoveRequest {
            spot: self.spot.clone(),
            kind: MoveKind::InstanceMethod,
            destination,
            update_references: true,
        };
        let mut replies = ask(provider, query);
        self.busy = true;
        self.error = None;
        self.status = Some("Asking the language server".to_owned());
        cx.notify();

        let handle = window.window_handle();
        let workspace = self.workspace.clone();
        let origin = self.origin.clone();
        let task = cx.spawn(async move |this, cx| {
            let answer = replies.next().await;
            let Some(answer) = answer else {
                this.update(cx, |this, cx| {
                    this.busy = false;
                    this.status = None;
                    this.error = Some(NO_ANSWER.to_owned());
                    cx.notify();
                })
                .ok();
                return;
            };
            let (outcome, confidence) = (answer.value, answer.confidence);
            if let Some(error) = outcome.error {
                this.update(cx, |this, cx| {
                    this.busy = false;
                    this.status = None;
                    this.error = Some(error);
                    cx.notify();
                })
                .ok();
                return;
            }
            if confidence == Confidence::Unknown {
                this.update(cx, |this, cx| {
                    this.busy = false;
                    this.status = None;
                    this.error = Some(NO_ANSWER.to_owned());
                    cx.notify();
                })
                .ok();
                return;
            }
            let change = outcome.change;
            if change.is_empty() {
                this.update(cx, |this, cx| {
                    this.busy = false;
                    this.status = None;
                    this.error = Some(NO_EDITS.to_owned());
                    cx.notify();
                })
                .ok();
                return;
            }
            // Escape may have closed the form while the server was answering.
            let open = this
                .update(cx, |this, cx| {
                    this.busy = false;
                    cx.notify();
                })
                .is_ok();
            if !open {
                return;
            }
            handle
                .update(cx, |_, window, cx| {
                    workspace
                        .update(cx, |workspace, cx| {
                            workspace.dismiss(window, cx);
                            crate::changes::apply_from(
                                workspace,
                                change,
                                origin,
                                Some("Move Instance Method".to_owned()),
                                window,
                                cx,
                            );
                        })
                        .ok();
                })
                .ok();
        });
        self.request = Some(task);
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.request = None;
        self.workspace
            .update(cx, |workspace, cx| workspace.dismiss(window, cx))
            .ok();
    }
}

fn row_box(
    id: impl Into<ElementId>,
    selector: String,
    active: bool,
    theme: &Theme,
) -> Stateful<Div> {
    div()
        .id(id)
        .debug_selector(move || selector)
        .flex()
        .items_center()
        .gap(px(SPACE_M))
        .h(px(ROW_HEIGHT))
        .px(px(SPACE_S))
        .rounded(px(CONTROL_RADIUS))
        .text_color(theme.text)
        .when(active, |row| row.bg(theme.selection))
}

fn dialog_button(
    id: impl Into<ElementId>,
    selector: String,
    label: &str,
    primary: bool,
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
        .when(primary, |this| {
            this.bg(theme.accent).text_color(theme.over_accent)
        })
        .when(!primary, |this| {
            this.border_1()
                .border_color(theme.control_border)
                .hover(move |style| style.bg(hover))
                .active(move |style| style.bg(pressed))
        })
        .child(label.to_owned())
}

impl Render for MoveTargetsDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let viewport = window.viewport_size();
        let width = px(WIDTH);
        let left = (viewport.width - width) / 2.;
        let top = viewport.height * 0.14;

        let mut target_rows = Vec::with_capacity(self.targets.len());
        for (index, target) in self.targets.iter().enumerate() {
            let detail = target.detail.clone().unwrap_or_default();
            target_rows.push(
                row_box(
                    ("move-targets-target", index),
                    format!("move-targets-target-{index}"),
                    index == self.active,
                    &theme,
                )
                .child(div().child(target.name.clone()))
                .child(div().text_color(theme.text_muted).child(detail))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.active = index;
                    this.status = None;
                    this.error = None;
                    cx.notify();
                }))
                .into_any_element(),
            );
        }

        let note = if let Some(error) = &self.error {
            Some(
                div()
                    .debug_selector(|| "move-targets-error".to_owned())
                    .text_color(theme.text)
                    .child(error.clone())
                    .into_any_element(),
            )
        } else if self.loading {
            Some(
                div()
                    .debug_selector(|| "move-targets-loading".to_owned())
                    .text_color(theme.text_muted)
                    .child("Reading the places that can take the method")
                    .into_any_element(),
            )
        } else {
            self.status.as_ref().map(|status| {
                div()
                    .debug_selector(|| "move-targets-status".to_owned())
                    .text_color(theme.text_muted)
                    .child(status.clone())
                    .into_any_element()
            })
        };

        let title = self.title.clone();
        let shell = surface(&theme)
            .id("move-targets")
            .debug_selector(|| "move-targets".to_owned())
            .key_context("MoveTargets")
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
            .children(target_rows)
            .children(note)
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(SPACE_M))
                    .child(
                        dialog_button(
                            "move-targets-cancel",
                            "move-targets-cancel".to_owned(),
                            "Cancel",
                            false,
                            &theme,
                        )
                        .tooltip(|_, cx| tooltip("Close without moving anything", None, cx))
                        .on_click(cx.listener(|this, _, window, cx| this.close(window, cx))),
                    )
                    .child(
                        dialog_button(
                            "move-targets-refactor",
                            "move-targets-refactor".to_owned(),
                            "Refactor",
                            true,
                            &theme,
                        )
                        .tooltip(|_, cx| tooltip("Move the method into the chosen place", None, cx))
                        .on_click(cx.listener(|this, _, window, cx| this.refactor(window, cx))),
                    ),
            );

        let close = cx.listener(|this, _: &ClickEvent, window, cx| this.close(window, cx));
        dialog_window(
            DialogSpec {
                key: "move-targets",
                title: title.into(),
                extra: None,
                place: DialogPlace {
                    left: f32::from(left),
                    top: f32::from(top),
                    width: f32::from(width),
                    height: None,
                },
                min: (360., 240.),
            },
            shell,
            window,
            cx,
            close,
            contents,
        )
    }
}
