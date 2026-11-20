//! Refactor This with values: the form of a refactoring whose server computes the
//! edits only after the user picks the members, types the name and chooses a package.

use futures_util::StreamExt;
use lang_api::{
    ActionApplyRequest, ActionChoice, ActionField, ActionMember, ActionParameters,
    ActionParametersQuery, Confidence, ask,
};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::Workspace;

use crate::prompt::TextField;

const WIDTH: f32 = 560.;

const NO_ANSWER: &str = "The language server did not answer";
const NO_SUPPORT: &str = "This language has no refactorings with values";

/// A row of the form the keys move between: a member to tick, a value to type or a
/// place to choose.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Row {
    Member(usize),
    Field(usize),
    Choice(usize),
}

/// The form of a refactoring. It opens over the workspace, so Escape and the buttons
/// work without editing the file under it.
pub struct RefactorFormDialog {
    handle: String,
    title: String,
    members_label: String,
    choices_label: String,
    members: Vec<ActionMember>,
    fields: Vec<(ActionField, TextField)>,
    choices: Vec<ActionChoice>,
    choice: usize,
    rows: Vec<Row>,
    active: usize,
    loading: bool,
    busy: bool,
    status: Option<String>,
    error: Option<String>,
    focus: FocusHandle,
    workspace: WeakEntity<Workspace>,
    /// The editor the refactoring started in, whose Ctrl+Z takes the whole change back.
    origin: Option<Entity<editor_view::EditorView>>,
}

impl RefactorFormDialog {
    pub(crate) fn show(
        handle: String,
        origin: Option<Entity<editor_view::EditorView>>,
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        let weak = cx.weak_entity();
        let view = cx.new(|cx| Self {
            handle,
            title: "Refactor".to_owned(),
            members_label: String::new(),
            choices_label: String::new(),
            members: Vec::new(),
            fields: Vec::new(),
            choices: Vec::new(),
            choice: 0,
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

    /// Asks the language for the form of the action. The window is on screen before
    /// the answer arrives.
    fn load(&mut self, cx: &mut Context<Self>) {
        let Some(provider) = cx
            .try_global::<editor_view::Semantic>()
            .and_then(|semantic| semantic.0.action_parameters())
        else {
            self.loading = false;
            self.error = Some(NO_SUPPORT.to_owned());
            cx.notify();
            return;
        };
        let mut replies = ask(provider, ActionParametersQuery(self.handle.clone()));
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
                        None => this.error = Some("Nothing to choose here".to_owned()),
                    },
                    None => this.error = Some(NO_ANSWER.to_owned()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn form_arrived(&mut self, form: ActionParameters, cx: &mut Context<Self>) {
        self.title = form.title;
        self.members_label = form.members_label;
        self.choices_label = form.choices_label;
        self.members = form.members;
        self.fields = form
            .fields
            .into_iter()
            .map(|field| {
                let text = TextField::from_text(field.value.clone());
                (field, text)
            })
            .collect();
        self.choices = form.choices;
        self.choice = 0;
        self.rows = (0..self.members.len())
            .map(Row::Member)
            .chain((0..self.fields.len()).map(Row::Field))
            .chain((0..self.choices.len()).map(Row::Choice))
            .collect();
        self.active = 0;
        cx.notify();
    }

    fn next_row(&self, step: i32) -> usize {
        if self.rows.is_empty() {
            return 0;
        }
        let len = self.rows.len() as i32;
        (self.active as i32 + step).rem_euclid(len) as usize
    }

    fn activate(&mut self, cx: &mut Context<Self>) {
        match self.rows.get(self.active).copied() {
            Some(Row::Member(index)) => {
                if let Some(member) = self.members.get_mut(index) {
                    member.checked = !member.checked;
                }
                self.status = None;
                self.error = None;
                cx.notify();
            }
            Some(Row::Choice(index)) => {
                self.choice = index;
                self.status = None;
                cx.notify();
            }
            _ => {}
        }
    }

    fn field_row(&self) -> Option<usize> {
        match self.rows.get(self.active) {
            Some(Row::Field(index)) => Some(*index),
            _ => None,
        }
    }

    /// Keys of the form: the arrows move between rows, Space ticks, text goes to the
    /// value under the focus, Enter applies.
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
                self.activate(cx);
                cx.stop_propagation();
                return;
            }
            _ => {}
        }
        let typed = self
            .field_row()
            .and_then(|index| self.fields.get_mut(index))
            .is_some_and(|(_, field)| field.key_with_clipboard(event, false, cx));
        if typed {
            self.status = None;
            self.error = None;
            cx.notify();
            cx.stop_propagation();
        }
    }

    fn request(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.loading {
            return;
        }
        let Some(provider) = cx
            .try_global::<editor_view::Semantic>()
            .and_then(|semantic| semantic.0.action_apply())
        else {
            self.error = Some(NO_SUPPORT.to_owned());
            cx.notify();
            return;
        };
        let query = ActionApplyRequest {
            handle: self.handle.clone(),
            members: self
                .members
                .iter()
                .filter(|member| member.checked)
                .map(|member| member.value.clone())
                .collect(),
            fields: self
                .fields
                .iter()
                .map(|(field, text)| ActionField {
                    key: field.key.clone(),
                    label: field.label.clone(),
                    value: text.text.clone(),
                })
                .collect(),
            choice: self
                .choices
                .get(self.choice)
                .map(|choice| choice.value.clone()),
        };
        let mut replies = ask(provider, query);
        self.busy = true;
        self.error = None;
        self.status = Some("Asking the language server".to_owned());
        cx.notify();

        let handle = window.window_handle();
        let workspace = self.workspace.clone();
        let label = self.title.clone();
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
                    this.error = Some("The server returned no edits".to_owned());
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
                                Some(label),
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

    fn refactor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.request(window, cx);
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
        .h(px(CONTROL_HEIGHT))
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
        .border_color(if checked {
            theme.control_border
        } else {
            ui_kit::checkbox_border(theme)
        })
        .when(checked, |box_| box_.bg(theme.accent))
        .child(
            div()
                .text_size(px(10.))
                .text_color(theme.over_accent)
                .child(if checked { "✓" } else { "" }),
        )
}

impl Render for RefactorFormDialog {
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
                    ("refactor-form-member", index),
                    format!("refactor-form-member-{index}"),
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
                    if let Some(member) = this.members.get_mut(index) {
                        member.checked = !member.checked;
                    }
                    cx.notify();
                }))
                .into_any_element(),
            );
        }

        let labels: Vec<&str> = self
            .fields
            .iter()
            .map(|(field, _)| field.label.as_str())
            .collect();
        let label_width = form_label_width(window, &labels);
        let mut field_rows = Vec::with_capacity(self.fields.len());
        for (index, (field, text)) in self.fields.iter().enumerate() {
            let value = div()
                .id(("refactor-form-value", index))
                .debug_selector({
                    let selector = format!("refactor-form-field-{index}");
                    move || selector.clone()
                })
                .flex()
                .items_center()
                .h(px(CONTROL_HEIGHT))
                .flex_1()
                .px(px(SPACE_M))
                .rounded(px(CONTROL_RADIUS))
                .border_1()
                .border_color(if active == Some(Row::Field(index)) {
                    theme.accent
                } else {
                    theme.control_border
                })
                .text_color(theme.text)
                .children(text_field_children_named(
                    FieldMouse::with_edit(
                        cx,
                        move |this: &mut Self| this.fields.get_mut(index).map(|(_, text)| text),
                        |this: &mut Self, _| {
                            this.status = None;
                            this.error = None;
                        },
                    ),
                    "refactor-form-text",
                    text,
                    theme.text,
                    theme.text,
                    theme.selection,
                    theme.caret,
                    18.,
                    active == Some(Row::Field(index)),
                ));
            field_rows.push(
                form_row(
                    label_width,
                    &field.label,
                    value.on_click(cx.listener(move |this, _, window, cx| {
                        this.active = this
                            .rows
                            .iter()
                            .position(|row| *row == Row::Field(index))
                            .unwrap_or(0);
                        window.focus(&this.focus, cx);
                        cx.notify();
                    })),
                    &theme,
                )
                .into_any_element(),
            );
        }

        let mut choice_rows = Vec::with_capacity(self.choices.len());
        for (index, choice) in self.choices.iter().enumerate() {
            let checked = index == self.choice;
            choice_rows.push(
                row_box(
                    ("refactor-form-choice", index),
                    format!("refactor-form-choice-{index}"),
                    active == Some(Row::Choice(index)),
                    &theme,
                )
                .child(
                    div()
                        .flex_none()
                        .w(px(14.))
                        .h(px(14.))
                        .rounded(px(7.))
                        .border_1()
                        .border_color(if checked {
                            theme.accent
                        } else {
                            theme.control_border
                        })
                        .when(checked, |dot| dot.bg(theme.accent)),
                )
                .child(div().child(choice.name.clone()))
                .child(
                    div()
                        .text_color(theme.text_muted)
                        .child(choice.detail.clone().unwrap_or_default()),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.choice = index;
                    this.active = this
                        .rows
                        .iter()
                        .position(|row| *row == Row::Choice(index))
                        .unwrap_or(0);
                    cx.notify();
                }))
                .into_any_element(),
            );
        }

        let note = if let Some(error) = &self.error {
            Some(
                div()
                    .debug_selector(|| "refactor-form-error".to_owned())
                    .text_color(theme.text)
                    .child(error.clone())
                    .into_any_element(),
            )
        } else if self.loading {
            Some(
                div()
                    .debug_selector(|| "refactor-form-loading".to_owned())
                    .text_color(theme.text_muted)
                    .child("Reading what the refactoring needs")
                    .into_any_element(),
            )
        } else {
            self.status.as_ref().map(|status| {
                div()
                    .debug_selector(|| "refactor-form-status".to_owned())
                    .text_color(theme.text_muted)
                    .child(status.clone())
                    .into_any_element()
            })
        };

        let title = self.title.clone();
        let shell = surface(&theme)
            .id("refactor-form")
            .debug_selector(|| "refactor-form".to_owned())
            .key_context("RefactorForm")
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
            .children(field_rows)
            .children((!choice_rows.is_empty()).then(|| {
                div()
                    .font_weight(FontWeight::MEDIUM)
                    .child(self.choices_label.clone())
                    .into_any_element()
            }))
            .children(choice_rows)
            .children(note)
            .child(
                DialogButtons::new()
                    .primary(
                        dialog_button(
                            "refactor-form-apply",
                            "refactor-form-apply".to_owned(),
                            "Refactor",
                            true,
                            !self.busy && !self.loading,
                            &theme,
                        )
                        .on_click(cx.listener(|this, _, window, cx| this.refactor(window, cx))),
                    )
                    .other(
                        dialog_button(
                            "refactor-form-cancel",
                            "refactor-form-cancel".to_owned(),
                            "Cancel",
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
                key: "refactor-form",
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
