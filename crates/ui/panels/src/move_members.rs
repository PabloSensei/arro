//! Move Members: the members of a type go into another type of the project. The form
//! opens at once and asks the language for the members and the types it knows.

use futures_util::StreamExt;
use lang_api::{
    Confidence, DocumentSpot, MoveDestination, MoveKind, MoveMember, MoveMembersForm,
    MoveMembersQuery, MoveRequest, ask,
};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::Workspace;

use crate::prompt::TextField;

const WIDTH: f32 = 560.;
const FIELD_HEIGHT: f32 = 26.;
/// How many types of the project the field hints at once.
const SUGGESTIONS: usize = 6;

const NO_ANSWER: &str = "The language server did not answer";
const NO_SUPPORT: &str = "This language does not move members";
const NO_MEMBER: &str = "No member is chosen";
const NO_TARGET: &str = "Choose a type to move the member to";
const NO_EDITS: &str = "The server returned no edits";

/// A row of the form the keys move between: a member to tick or the target field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Row {
    Member(usize),
    Field,
}

/// The form of Move Members. It opens over the workspace, so Escape and the buttons
/// work without editing the file under it.
pub struct MoveMembersDialog {
    spot: DocumentSpot,
    title: String,
    members_label: String,
    members: Vec<MoveMember>,
    target_label: String,
    targets: Vec<MoveDestination>,
    target: TextField,
    rows: Vec<Row>,
    active: usize,
    loading: bool,
    busy: bool,
    status: Option<String>,
    error: Option<String>,
    focus: FocusHandle,
    workspace: WeakEntity<Workspace>,
    /// The editor the move started in, whose Ctrl+Z takes the whole change back.
    origin: Option<Entity<editor_view::EditorView>>,
}

impl MoveMembersDialog {
    pub(crate) fn show(
        spot: DocumentSpot,
        origin: Option<Entity<editor_view::EditorView>>,
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        let weak = cx.weak_entity();
        let view = cx.new(|cx| Self {
            spot,
            title: "Move Members".to_owned(),
            members_label: String::new(),
            members: Vec::new(),
            target_label: "To class".to_owned(),
            targets: Vec::new(),
            target: TextField::default(),
            rows: Vec::new(),
            active: 0,
            loading: true,
            busy: false,
            status: None,
            error: None,
            focus: cx.focus_handle(),
            workspace: weak,
            origin,
        });
        let focus = view.read(cx).focus.clone();
        workspace.show_overlay(view.clone().into(), focus, window, cx);
        view.update(cx, |this, cx| this.load(cx));
    }

    /// Asks the language for the members and the types of the project. The window is on
    /// screen before the answer arrives, and the request ends with the form.
    fn load(&mut self, cx: &mut Context<Self>) {
        let Some(provider) = cx
            .try_global::<editor_view::Semantic>()
            .and_then(|semantic| semantic.0.move_members_form())
        else {
            self.loading = false;
            self.error = Some(NO_SUPPORT.to_owned());
            cx.notify();
            return;
        };
        let query = MoveMembersQuery {
            spot: self.spot.clone(),
        };
        let mut replies = ask(provider, query);
        cx.spawn(async move |this, cx| {
            let answer = replies.next().await;
            this.update(cx, |this, cx| {
                this.loading = false;
                match answer {
                    Some(answer) if answer.confidence == Confidence::Unknown => {
                        this.error = Some(NO_ANSWER.to_owned());
                    }
                    Some(answer) => match answer.value {
                        Some(form) => this.form_arrived(form, cx),
                        None => this.error = Some("Nothing to move here".to_owned()),
                    },
                    None => this.error = Some(NO_ANSWER.to_owned()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn form_arrived(&mut self, form: MoveMembersForm, cx: &mut Context<Self>) {
        self.title = form.title;
        self.members_label = form.members_label;
        self.members = form.members;
        self.target_label = form.target_label;
        self.targets = form.targets;
        self.rows = (0..self.members.len())
            .map(Row::Member)
            .chain(std::iter::once(Row::Field))
            .collect();
        // The member under the caret is ticked, and the caret of the form is in the
        // field that names the type, so typing reaches it at once.
        self.active = self.members.len();
        cx.notify();
    }

    fn next_row(&self, step: i32) -> usize {
        if self.rows.is_empty() {
            return 0;
        }
        let len = self.rows.len() as i32;
        (self.active as i32 + step).rem_euclid(len) as usize
    }

    /// Only one member goes per move: ticking a row unticks the others.
    fn tick(&mut self, index: usize, cx: &mut Context<Self>) {
        for (at, member) in self.members.iter_mut().enumerate() {
            member.checked = at == index;
        }
        self.status = None;
        self.error = None;
        cx.notify();
    }

    fn field_row(&self) -> Option<usize> {
        matches!(self.rows.get(self.active), Some(Row::Field)).then_some(0)
    }

    fn checked_member(&self) -> Option<&MoveMember> {
        self.members.iter().find(|member| member.checked)
    }

    /// The destination of the typed text: a type of the project when it names one, and
    /// the text itself otherwise.
    fn destination(&self) -> MoveDestination {
        let typed = self.target.text.trim();
        if let Some(target) = self
            .targets
            .iter()
            .find(|target| target.name == typed || target.handle == typed)
        {
            return target.clone();
        }
        MoveDestination {
            name: typed.to_owned(),
            detail: None,
            folder: None,
            handle: typed.to_owned(),
        }
    }

    /// Keys of the form: the arrows move between rows, Space ticks, text goes to the
    /// field, Enter applies.
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
        if modifiers.alt || modifiers.control || modifiers.platform {
            return;
        }
        match key {
            "up" => {
                self.active = self.next_row(-1);
                cx.notify();
                cx.stop_propagation();
                return;
            }
            "down" | "tab" => {
                let step = if modifiers.shift { -1 } else { 1 };
                self.active = self.next_row(step);
                cx.notify();
                cx.stop_propagation();
                return;
            }
            "space" if self.field_row().is_none() => {
                if let Some(Row::Member(index)) = self.rows.get(self.active).copied() {
                    let ticked = !self.members[index].checked;
                    for (at, member) in self.members.iter_mut().enumerate() {
                        member.checked = at == index && ticked;
                    }
                    self.status = None;
                    self.error = None;
                    cx.notify();
                }
                cx.stop_propagation();
                return;
            }
            _ => {}
        }
        let typed = self
            .field_row()
            .is_some_and(|_| self.target.key_with_clipboard(event, false, cx));
        if typed {
            self.status = None;
            self.error = None;
            cx.notify();
            cx.stop_propagation();
        }
    }

    fn refactor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.loading {
            return;
        }
        let Some(member) = self.checked_member() else {
            self.error = Some(NO_MEMBER.to_owned());
            self.status = None;
            cx.notify();
            return;
        };
        let member_spot = DocumentSpot {
            uri: self.spot.uri.clone(),
            version: self.spot.version,
            point: member.span.start,
        };
        let destination = self.destination();
        if destination.name.is_empty() {
            self.error = Some(NO_TARGET.to_owned());
            self.status = None;
            cx.notify();
            return;
        }
        let Some(provider) = cx
            .try_global::<editor_view::Semantic>()
            .and_then(|semantic| semantic.0.move_file())
        else {
            self.error = Some(NO_SUPPORT.to_owned());
            cx.notify();
            return;
        };
        let query = MoveRequest {
            spot: member_spot,
            kind: MoveKind::Member,
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
        cx.spawn(async move |this, cx| {
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
                                Some("Move Members".to_owned()),
                                window,
                                cx,
                            );
                        })
                        .ok();
                })
                .ok();
        })
        .detach();
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
        .h(px(FIELD_HEIGHT))
        .px(px(SPACE_S))
        .rounded(px(CONTROL_RADIUS))
        .text_color(theme.text)
        .when(active, |row| row.bg(theme.selection))
}

fn check_box(checked: bool, theme: &Theme) -> Div {
    div()
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .w(px(14.))
        .h(px(14.))
        .rounded(px(3.))
        .border_1()
        .border_color(theme.control_border)
        .when(checked, |box_| box_.bg(theme.accent))
        .child(
            div()
                .text_size(px(10.))
                .text_color(theme.over_accent)
                .child(if checked { "✓" } else { "" }),
        )
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

impl Render for MoveMembersDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let viewport = window.viewport_size();
        let width = px(WIDTH);
        let left = (viewport.width - width) / 2.;
        let top = viewport.height * 0.14;
        let active = self.rows.get(self.active).copied();

        let mut member_rows = Vec::with_capacity(self.members.len());
        for (index, member) in self.members.iter().enumerate() {
            let detail = member.detail.clone().unwrap_or_default();
            member_rows.push(
                row_box(
                    ("move-members-member", index),
                    format!("move-members-member-{index}"),
                    active == Some(Row::Member(index)),
                    &theme,
                )
                .child(check_box(member.checked, &theme))
                .child(div().child(member.name.clone()))
                .child(div().text_color(theme.text_muted).child(detail))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.active = this
                        .rows
                        .iter()
                        .position(|row| *row == Row::Member(index))
                        .unwrap_or(0);
                    this.tick(index, cx);
                }))
                .into_any_element(),
            );
        }

        let field = div()
            .id("move-members-field")
            .debug_selector(|| "move-members-field".to_owned())
            .flex()
            .items_center()
            .h(px(FIELD_HEIGHT))
            .flex_1()
            .px(px(SPACE_M))
            .rounded(px(CONTROL_RADIUS))
            .border_1()
            .border_color(if active == Some(Row::Field) {
                theme.accent
            } else {
                theme.control_border
            })
            .text_color(theme.text)
            .children(text_field_children_named(
                "move-members-text",
                &self.target,
                theme.text,
                theme.text,
                theme.selection,
                theme.caret,
                18.,
                active == Some(Row::Field),
            ))
            .on_click(cx.listener(|this, _, window, cx| {
                this.active = this.members.len();
                window.focus(&this.focus, cx);
                cx.notify();
            }));

        let field_row = div()
            .flex()
            .items_center()
            .gap(px(SPACE_M))
            .child(
                div()
                    .w(px(120.))
                    .flex_none()
                    .text_color(theme.text_muted)
                    .child(self.target_label.clone()),
            )
            .child(field);

        // The types of the project that match the typed text are the hint of the field.
        let prefix = self.target.text.to_lowercase();
        let matching: Vec<(usize, &MoveDestination)> = self
            .targets
            .iter()
            .enumerate()
            .filter(|(_, target)| {
                prefix.is_empty()
                    || target.name.to_lowercase().contains(&prefix)
                    || target.handle.to_lowercase().contains(&prefix)
            })
            .take(SUGGESTIONS)
            .collect();
        let mut target_rows = Vec::with_capacity(matching.len());
        for (index, target) in matching {
            let handle = target.name.clone();
            let detail = target.detail.clone().unwrap_or_default();
            target_rows.push(
                row_box(
                    ("move-members-target", index),
                    format!("move-members-target-{index}"),
                    false,
                    &theme,
                )
                .child(div().child(target.name.clone()))
                .child(div().text_color(theme.text_muted).child(detail))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.target = TextField::from_text(handle.clone());
                    this.active = this.members.len();
                    this.status = None;
                    this.error = None;
                    window.focus(&this.focus, cx);
                    cx.notify();
                }))
                .into_any_element(),
            );
        }

        let hint = self.target.text.trim().is_empty().then(|| {
            div()
                .debug_selector(|| "move-members-hint".to_owned())
                .text_size(px(TEXT_SMALL))
                .text_color(theme.text_muted)
                .child("The types of the project")
                .into_any_element()
        });

        let note = if let Some(error) = &self.error {
            Some(
                div()
                    .debug_selector(|| "move-members-error".to_owned())
                    .text_color(theme.text)
                    .child(error.clone())
                    .into_any_element(),
            )
        } else if self.loading {
            Some(
                div()
                    .debug_selector(|| "move-members-loading".to_owned())
                    .text_color(theme.text_muted)
                    .child("Reading the members of the type")
                    .into_any_element(),
            )
        } else {
            self.status.as_ref().map(|status| {
                div()
                    .debug_selector(|| "move-members-status".to_owned())
                    .text_color(theme.text_muted)
                    .child(status.clone())
                    .into_any_element()
            })
        };

        let title = self.title.clone();
        let shell = surface(&theme)
            .id("move-members")
            .debug_selector(|| "move-members".to_owned())
            .key_context("MoveMembers")
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
            .children((!member_rows.is_empty()).then(|| {
                div()
                    .font_weight(FontWeight::MEDIUM)
                    .child(self.members_label.clone())
                    .into_any_element()
            }))
            .children(member_rows)
            .child(field_row)
            .children(hint)
            .children(target_rows)
            .children(note)
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(SPACE_M))
                    .child(
                        dialog_button(
                            "move-members-cancel",
                            "move-members-cancel".to_owned(),
                            "Cancel",
                            false,
                            &theme,
                        )
                        .tooltip(|_, cx| tooltip("Close without moving anything", None, cx))
                        .on_click(cx.listener(|this, _, window, cx| this.close(window, cx))),
                    )
                    .child(
                        dialog_button(
                            "move-members-refactor",
                            "move-members-refactor".to_owned(),
                            "Refactor",
                            true,
                            &theme,
                        )
                        .tooltip(|_, cx| tooltip("Move the chosen member into this type", None, cx))
                        .on_click(cx.listener(|this, _, window, cx| this.refactor(window, cx))),
                    ),
            );

        let close = cx.listener(|this, _: &ClickEvent, window, cx| this.close(window, cx));
        dialog_window(
            DialogSpec {
                key: "move-members",
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
