//! Pull Members Up, Push Members Down, Extract Superclass and Introduce Parameter
//! Object: the form of the members, the place they go to and the name of a new type.

use futures_util::StreamExt;
use lang_api::{
    ActionField, ActionOutcome, Confidence, DocumentSpot, MemberActionForm, MemberActionQuery,
    MemberActionRequest, MemberChoice, MemberRefactoring, MemberSelection, MemberTarget, ask,
};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::Workspace;

use crate::prompt::TextField;

const WIDTH: f32 = 620.;

const NO_ANSWER: &str = "The language server did not answer";
const NO_SUPPORT: &str = "This language does not move members";

/// A row of the form the keys move between.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Row {
    Target(usize),
    Member(usize),
    Abstract(usize),
    Field(usize),
}

/// The form of a refactoring that moves members or makes a type from them. It opens
/// over the workspace, so Escape and the buttons work without editing the file under it.
pub struct MemberFormDialog {
    refactoring: MemberRefactoring,
    spot: DocumentSpot,
    title: String,
    target_label: String,
    targets: Vec<MemberTarget>,
    target: usize,
    targets_selectable: bool,
    members_label: String,
    members: Vec<MemberChoice>,
    fields: Vec<(ActionField, TextField)>,
    warning: Option<String>,
    rows: Vec<Row>,
    active: usize,
    loading: bool,
    busy: bool,
    status: Option<String>,
    error: Option<String>,
    focus: FocusHandle,
    workspace: WeakEntity<Workspace>,
}

impl MemberFormDialog {
    pub(crate) fn show(
        refactoring: MemberRefactoring,
        spot: DocumentSpot,
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        let weak = cx.weak_entity();
        let view = cx.new(|cx| Self {
            refactoring,
            spot,
            title: refactoring.title().to_owned(),
            target_label: String::new(),
            targets: Vec::new(),
            target: 0,
            targets_selectable: false,
            members_label: String::new(),
            members: Vec::new(),
            fields: Vec::new(),
            warning: None,
            rows: Vec::new(),
            active: 0,
            loading: true,
            busy: false,
            status: None,
            error: None,
            focus: cx.focus_handle(),
            workspace: weak,
        });
        let focus = view.read(cx).focus.clone();
        workspace.show_overlay(view.clone().into(), focus, window, cx);
        view.update(cx, |this, cx| this.load(cx));
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        let Some(provider) = cx
            .try_global::<editor_view::Semantic>()
            .and_then(|semantic| semantic.0.member_action_form())
        else {
            self.loading = false;
            self.error = Some(NO_SUPPORT.to_owned());
            cx.notify();
            return;
        };
        let query = MemberActionQuery {
            refactoring: self.refactoring,
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

    fn form_arrived(&mut self, form: MemberActionForm, cx: &mut Context<Self>) {
        self.title = form.title;
        self.target_label = form.target_label;
        self.targets = form.targets;
        self.targets_selectable = form.targets_selectable;
        self.members_label = form.members_label;
        self.members = form.members;
        self.fields = form
            .fields
            .into_iter()
            .map(|field| {
                let text = TextField::from_text(field.value.clone());
                (field, text)
            })
            .collect();
        self.warning = form.warning;
        self.rows = (0..self.targets.len())
            .map(Row::Target)
            .chain((0..self.members.len()).map(Row::Member))
            .chain(
                (0..self.members.len())
                    .filter(|index| self.members[*index].can_be_abstract)
                    .map(Row::Abstract),
            )
            .chain((0..self.fields.len()).map(Row::Field))
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
            Some(Row::Target(index)) if self.targets_selectable => {
                self.target = index;
            }
            Some(Row::Member(index)) => {
                if let Some(member) = self.members.get_mut(index) {
                    member.checked = !member.checked;
                }
            }
            Some(Row::Abstract(index)) => {
                if let Some(member) = self.members.get_mut(index) {
                    member.abstract_checked = !member.abstract_checked;
                    member.checked = true;
                }
            }
            _ => return,
        }
        self.status = None;
        self.error = None;
        cx.notify();
    }

    fn field_row(&self) -> Option<usize> {
        match self.rows.get(self.active) {
            Some(Row::Field(index)) => Some(*index),
            _ => None,
        }
    }

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

    fn preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.request(window, cx, false);
    }

    fn refactor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.request(window, cx, true);
    }

    fn request(&mut self, window: &mut Window, cx: &mut Context<Self>, apply: bool) {
        if self.busy || self.loading {
            return;
        }
        if !self.members.iter().any(|member| member.checked) {
            self.error = Some("No member is chosen".to_owned());
            self.status = None;
            cx.notify();
            return;
        }
        let Some(provider) = cx
            .try_global::<editor_view::Semantic>()
            .and_then(|semantic| semantic.0.apply_member_action())
        else {
            self.error = Some(NO_SUPPORT.to_owned());
            cx.notify();
            return;
        };
        let query = MemberActionRequest {
            refactoring: self.refactoring,
            spot: self.spot.clone(),
            target: self
                .targets
                .get(self.target)
                .map(|target| target.value.clone()),
            members: self
                .members
                .iter()
                .filter(|member| member.checked)
                .map(|member| MemberSelection {
                    value: member.value.clone(),
                    is_abstract: member.can_be_abstract && member.abstract_checked,
                })
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
        };
        let mut replies = ask(provider, query);
        self.busy = true;
        self.error = None;
        self.status = Some("Asking the language server".to_owned());
        cx.notify();

        let handle = window.window_handle();
        let workspace = self.workspace.clone();
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
            if !apply {
                this.update(cx, |this, cx| {
                    this.busy = false;
                    this.status = Some(summary(&outcome));
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
                    this.error = Some("The language returned no edits".to_owned());
                    cx.notify();
                })
                .ok();
                return;
            }
            // Escape may have closed the form while the language was answering.
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
                            crate::changes::apply(workspace, change, window, cx);
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

fn summary(outcome: &ActionOutcome) -> String {
    let change = &outcome.change;
    let files = change
        .edits
        .iter()
        .filter(|source| !source.edits.is_empty())
        .count();
    let edits: usize = change.edits.iter().map(|source| source.edits.len()).sum();
    format!("{edits} changes in {files} files. Refactor applies them")
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

impl Render for MemberFormDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let labels: Vec<&str> = self
            .fields
            .iter()
            .map(|(field, _)| field.label.as_str())
            .collect();
        let label_width = form_label_width(window, &labels);
        let viewport = window.viewport_size();
        let width = px(WIDTH);
        let left = (viewport.width - width) / 2.;
        let top = viewport.height * 0.14;
        let active = self.rows.get(self.active).copied();

        let mut target_rows = Vec::with_capacity(self.targets.len());
        for (index, target) in self.targets.iter().enumerate() {
            let checked = index == self.target;
            let selectable = self.targets_selectable;
            target_rows.push(
                row_box(
                    ("member-form-target", index),
                    format!("member-form-target-{index}"),
                    active == Some(Row::Target(index)),
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
                .child(div().child(target.name.clone()))
                .child(
                    div()
                        .text_color(theme.text_muted)
                        .child(target.detail.clone().unwrap_or_default()),
                )
                .when(selectable, |row| {
                    row.on_click(cx.listener(move |this, _, _, cx| {
                        this.target = index;
                        this.status = None;
                        this.error = None;
                        cx.notify();
                    }))
                })
                .into_any_element(),
            );
        }

        let mut member_rows = Vec::with_capacity(self.members.len());
        for (index, member) in self.members.iter().enumerate() {
            let abstract_checked = member.abstract_checked;
            let can_be_abstract = member.can_be_abstract;
            let detail = member.detail.clone().unwrap_or_default();
            let abstract_box = can_be_abstract.then(|| {
                div()
                    .id(("member-form-abstract", index))
                    .debug_selector({
                        let selector = format!("member-form-abstract-{index}");
                        move || selector.clone()
                    })
                    .flex()
                    .items_center()
                    .gap(px(SPACE_S))
                    .px(px(SPACE_S))
                    .rounded(px(CONTROL_RADIUS))
                    .text_color(if abstract_checked {
                        theme.text
                    } else {
                        theme.text_muted
                    })
                    .child(check_box(abstract_checked, &theme))
                    .child("make abstract")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(member) = this.members.get_mut(index) {
                            member.abstract_checked = !member.abstract_checked;
                            member.checked = true;
                        }
                        this.status = None;
                        this.error = None;
                        cx.notify();
                    }))
                    .into_any_element()
            });
            member_rows.push(
                row_box(
                    ("member-form-member", index),
                    format!("member-form-member-{index}"),
                    active == Some(Row::Member(index)),
                    &theme,
                )
                .child(check_box(member.checked, &theme))
                .child(div().child(member.name.clone()))
                .child(div().text_color(theme.text_muted).child(detail))
                .children(abstract_box)
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(member) = this.members.get_mut(index) {
                        member.checked = !member.checked;
                    }
                    this.active = this
                        .rows
                        .iter()
                        .position(|row| *row == Row::Member(index))
                        .unwrap_or(0);
                    this.status = None;
                    this.error = None;
                    cx.notify();
                }))
                .into_any_element(),
            );
        }

        let mut field_rows = Vec::with_capacity(self.fields.len());
        for (index, (field, text)) in self.fields.iter().enumerate() {
            let value = div()
                .id(("member-form-value", index))
                .debug_selector({
                    let selector = format!("member-form-field-{index}");
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
                    "member-form-text",
                    text,
                    theme.text,
                    theme.text,
                    theme.selection,
                    theme.caret,
                    18.,
                    active == Some(Row::Field(index)),
                ));
            field_rows.push(
                div()
                    .flex()
                    .items_center()
                    .gap(px(SPACE_M))
                    .child(form_label(label_width, &field.label, &theme))
                    .child(value.on_click(cx.listener(move |this, _, window, cx| {
                        this.active = this
                            .rows
                            .iter()
                            .position(|row| *row == Row::Field(index))
                            .unwrap_or(0);
                        window.focus(&this.focus, cx);
                        cx.notify();
                    })))
                    .into_any_element(),
            );
        }

        let note = if let Some(error) = &self.error {
            Some(
                div()
                    .debug_selector(|| "member-form-error".to_owned())
                    .text_color(theme.text)
                    .child(error.clone())
                    .into_any_element(),
            )
        } else if self.loading {
            Some(
                div()
                    .debug_selector(|| "member-form-loading".to_owned())
                    .text_color(theme.text_muted)
                    .child("Reading the type")
                    .into_any_element(),
            )
        } else {
            self.status.as_ref().map(|status| {
                div()
                    .debug_selector(|| "member-form-status".to_owned())
                    .text_color(theme.text_muted)
                    .child(status.clone())
                    .into_any_element()
            })
        };

        let warning = self.warning.as_ref().map(|warning| {
            div()
                .debug_selector(|| "member-form-warning".to_owned())
                .text_color(theme.stripe_warning)
                .child(warning.clone())
                .into_any_element()
        });

        let title = self.title.clone();
        let shell = surface(&theme)
            .id("member-form")
            .debug_selector(|| "member-form".to_owned())
            .key_context("MemberForm")
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
            .children((!target_rows.is_empty()).then(|| {
                div()
                    .font_weight(FontWeight::MEDIUM)
                    .child(self.target_label.clone())
                    .into_any_element()
            }))
            .children(target_rows)
            .children((!member_rows.is_empty()).then(|| {
                div()
                    .font_weight(FontWeight::MEDIUM)
                    .child(self.members_label.clone())
                    .into_any_element()
            }))
            .children(member_rows)
            .children(field_rows)
            .children(warning)
            .children(note)
            .child(
                DialogButtons::new()
                    .extra(
                        dialog_button(
                            "member-form-preview",
                            "member-form-preview".to_owned(),
                            "Preview",
                            false,
                            true,
                            &theme,
                        )
                        .on_click(cx.listener(|this, _, window, cx| this.preview(window, cx))),
                    )
                    .primary(
                        dialog_button(
                            "member-form-apply",
                            "member-form-apply".to_owned(),
                            "Refactor",
                            true,
                            !self.busy && !self.loading,
                            &theme,
                        )
                        .on_click(cx.listener(|this, _, window, cx| this.refactor(window, cx))),
                    )
                    .other(
                        dialog_button(
                            "member-form-cancel",
                            "member-form-cancel".to_owned(),
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
                key: "member-form",
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
