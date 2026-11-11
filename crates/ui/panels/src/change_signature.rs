//! Ctrl+F6: the form of Change Signature. The language describes the signature under
//! the caret, the form edits it and the language turns the new one into edits.

use futures_util::StreamExt;
use lang_api::{
    ChangeSignatureInfo, ChangeSignatureRequest, Confidence, DocumentSpot, MethodSignature,
    SignatureException, SignatureParameter, WorkspaceChange, ask,
};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::Workspace;

use crate::prompt::TextField;

const WIDTH: f32 = 760.;
const FIELD_HEIGHT: f32 = 26.;

const NO_ANSWER: &str = "The language server did not answer";
const NO_SUPPORT: &str = "Change Signature is not supported by this language";

/// Which field takes the typed text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Field {
    Name,
    Visibility,
    ReturnType,
    ParameterType(usize),
    ParameterName(usize),
    ParameterDefault(usize),
}

/// One row of the parameter table.
struct ParameterRow {
    type_name: TextField,
    name: TextField,
    default_value: TextField,
    original_index: Option<usize>,
}

impl ParameterRow {
    fn new() -> Self {
        Self {
            type_name: TextField::default(),
            name: TextField::default(),
            default_value: TextField::default(),
            original_index: None,
        }
    }

    fn from(parameter: SignatureParameter) -> Self {
        Self {
            type_name: TextField::from_text(parameter.type_name),
            name: TextField::from_text(parameter.name),
            default_value: TextField::from_text(parameter.default_value),
            original_index: parameter.original_index,
        }
    }
}

/// The Change Signature window. It opens over the workspace, so Escape and the buttons
/// work without editing the file under it.
pub struct ChangeSignatureDialog {
    spot: DocumentSpot,
    name: TextField,
    visibility: TextField,
    return_type: TextField,
    parameters: Vec<ParameterRow>,
    exceptions: Vec<SignatureException>,
    active: Field,
    loading: bool,
    busy: bool,
    status: Option<String>,
    error: Option<String>,
    focus: FocusHandle,
    workspace: WeakEntity<Workspace>,
}

impl ChangeSignatureDialog {
    pub(crate) fn show(
        spot: DocumentSpot,
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        let weak = cx.weak_entity();
        let view = cx.new(|cx| Self {
            spot,
            name: TextField::default(),
            visibility: TextField::default(),
            return_type: TextField::default(),
            parameters: Vec::new(),
            exceptions: Vec::new(),
            active: Field::Name,
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

    /// Asks the language for the signature under the caret. The answer arrives later,
    /// so the window is already on screen when the request starts.
    fn load(&mut self, cx: &mut Context<Self>) {
        let Some(provider) = cx
            .try_global::<editor_view::Semantic>()
            .and_then(|semantic| semantic.0.change_signature_info())
        else {
            self.loading = false;
            self.error = Some(NO_SUPPORT.to_owned());
            cx.notify();
            return;
        };
        let mut replies = ask(provider, self.spot.clone());
        cx.spawn(async move |this, cx| {
            let answer = replies.next().await;
            this.update(cx, |this, cx| {
                this.loading = false;
                match answer {
                    Some(answer) => this.info_arrived(answer.value, answer.confidence, cx),
                    None => {
                        this.error = Some(NO_ANSWER.to_owned());
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    fn info_arrived(
        &mut self,
        info: ChangeSignatureInfo,
        confidence: Confidence,
        cx: &mut Context<Self>,
    ) {
        if let Some(error) = info.error {
            self.error = Some(error);
        } else if confidence == Confidence::Unknown {
            self.error = Some(NO_ANSWER.to_owned());
        } else if let Some(signature) = info.signature {
            self.name.text = signature.name;
            self.visibility.text = signature.visibility;
            self.return_type.text = signature.return_type;
            self.parameters = signature
                .parameters
                .into_iter()
                .map(ParameterRow::from)
                .collect();
            self.exceptions = signature.exceptions;
        } else {
            self.error = Some("Cannot change the signature here".to_owned());
        }
        cx.notify();
    }

    /// The signature as the fields show it, or the reason it cannot be sent.
    fn signature(&self) -> Result<MethodSignature, String> {
        let name = self.name.text.trim();
        if name.is_empty() {
            return Err("The name is empty".to_owned());
        }
        let mut parameters = Vec::with_capacity(self.parameters.len());
        for (index, row) in self.parameters.iter().enumerate() {
            let type_name = row.type_name.text.trim();
            let parameter_name = row.name.text.trim();
            if type_name.is_empty() || parameter_name.is_empty() {
                return Err(format!("Parameter {} has no type or no name", index + 1));
            }
            parameters.push(SignatureParameter {
                type_name: type_name.to_owned(),
                name: parameter_name.to_owned(),
                default_value: row.default_value.text.trim().to_owned(),
                original_index: row.original_index,
            });
        }
        Ok(MethodSignature {
            name: name.to_owned(),
            visibility: self.visibility.text.trim().to_owned(),
            return_type: self.return_type.text.trim().to_owned(),
            parameters,
            exceptions: self.exceptions.clone(),
        })
    }

    /// Keys of the form: text goes to the active field, Tab moves between fields, and
    /// the buttons have their own click handlers.
    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
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
        if key == "tab" && !event.keystroke.modifiers.control {
            if event.keystroke.modifiers.shift {
                self.active = self.previous_field();
            } else {
                self.active = self.next_field();
            }
            cx.notify();
            cx.stop_propagation();
            return;
        }
        let moved = match (event.keystroke.modifiers.alt, key, self.active_parameter()) {
            (true, "up", Some(index)) => self.move_parameter(index, -1, cx),
            (true, "down", Some(index)) => self.move_parameter(index, 1, cx),
            _ => false,
        };
        if moved {
            cx.stop_propagation();
            return;
        }
        let typed = self
            .field_mut()
            .is_some_and(|field| field.key_with_clipboard(event, false, cx));
        if typed {
            self.status = None;
            self.error = None;
            cx.notify();
            cx.stop_propagation();
        }
    }

    fn field_mut(&mut self) -> Option<&mut TextField> {
        match self.active {
            Field::Name => Some(&mut self.name),
            Field::Visibility => Some(&mut self.visibility),
            Field::ReturnType => Some(&mut self.return_type),
            Field::ParameterType(index) => {
                self.parameters.get_mut(index).map(|row| &mut row.type_name)
            }
            Field::ParameterName(index) => self.parameters.get_mut(index).map(|row| &mut row.name),
            Field::ParameterDefault(index) => self
                .parameters
                .get_mut(index)
                .map(|row| &mut row.default_value),
        }
    }

    fn fields(&self) -> Vec<Field> {
        let mut fields = vec![Field::Name, Field::Visibility, Field::ReturnType];
        for index in 0..self.parameters.len() {
            fields.push(Field::ParameterType(index));
            fields.push(Field::ParameterName(index));
            fields.push(Field::ParameterDefault(index));
        }
        fields
    }

    fn next_field(&self) -> Field {
        let fields = self.fields();
        let Some(index) = fields.iter().position(|field| *field == self.active) else {
            return fields[0];
        };
        fields[(index + 1) % fields.len()]
    }

    fn previous_field(&self) -> Field {
        let fields = self.fields();
        let Some(index) = fields.iter().position(|field| *field == self.active) else {
            return fields[0];
        };
        fields[(index + fields.len() - 1) % fields.len()]
    }

    fn add_parameter(&mut self, cx: &mut Context<Self>) {
        self.parameters.push(ParameterRow::new());
        self.active = Field::ParameterType(self.parameters.len() - 1);
        self.status = None;
        self.error = None;
        cx.notify();
    }

    fn remove_parameter(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.parameters.len() {
            return;
        }
        self.parameters.remove(index);
        self.active = self.settle_field(self.active);
        self.status = None;
        self.error = None;
        cx.notify();
    }

    /// A field that pointed past the end after a row left moves to the row before it.
    fn settle_field(&self, field: Field) -> Field {
        if self.parameters.is_empty() {
            return Field::Name;
        }
        let last = self.parameters.len() - 1;
        match field {
            Field::ParameterType(index) => Field::ParameterType(index.min(last)),
            Field::ParameterName(index) => Field::ParameterName(index.min(last)),
            Field::ParameterDefault(index) => Field::ParameterDefault(index.min(last)),
            other => other,
        }
    }

    fn active_parameter(&self) -> Option<usize> {
        match self.active {
            Field::ParameterType(index)
            | Field::ParameterName(index)
            | Field::ParameterDefault(index) => Some(index),
            _ => None,
        }
    }

    fn move_parameter(&mut self, index: usize, direction: i32, cx: &mut Context<Self>) -> bool {
        let target = index as i32 + direction;
        if target < 0 || target as usize >= self.parameters.len() {
            return false;
        }
        let target = target as usize;
        self.parameters.swap(index, target);
        self.active = match self.active {
            Field::ParameterType(_) => Field::ParameterType(target),
            Field::ParameterName(_) => Field::ParameterName(target),
            Field::ParameterDefault(_) => Field::ParameterDefault(target),
            other => other,
        };
        self.status = None;
        self.error = None;
        cx.notify();
        true
    }

    fn preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.request(window, cx, false);
    }

    fn refactor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.request(window, cx, true);
    }

    /// Asks for the edits of the form. The preview waits for the answer and shows its
    /// size; Refactor applies it as one workspace change.
    fn request(&mut self, window: &mut Window, cx: &mut Context<Self>, apply: bool) {
        if self.busy || self.loading {
            return;
        }
        let signature = match self.signature() {
            Ok(signature) => signature,
            Err(error) => {
                self.error = Some(error);
                self.status = None;
                cx.notify();
                return;
            }
        };
        let Some(provider) = cx
            .try_global::<editor_view::Semantic>()
            .and_then(|semantic| semantic.0.change_signature())
        else {
            self.error = Some(NO_SUPPORT.to_owned());
            cx.notify();
            return;
        };
        let query = ChangeSignatureRequest {
            spot: self.spot.clone(),
            signature,
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
            if !apply {
                this.update(cx, |this, cx| {
                    this.busy = false;
                    this.status = Some(summary(&change));
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

fn summary(change: &WorkspaceChange) -> String {
    let files = change
        .edits
        .iter()
        .filter(|source| !source.edits.is_empty())
        .count();
    let edits: usize = change.edits.iter().map(|source| source.edits.len()).sum();
    let mut text = format!("{edits} changes in {files} files");
    if !change.renames.is_empty() {
        text.push_str(&format!(", {} files renamed", change.renames.len()));
    }
    text + ". Refactor applies them"
}

fn field_box(
    id: impl Into<ElementId>,
    selector: String,
    text: &str,
    active: bool,
    theme: &Theme,
) -> Stateful<Div> {
    div()
        .id(id)
        .debug_selector(move || selector)
        .flex()
        .items_center()
        .h(px(FIELD_HEIGHT))
        .min_w(px(0.))
        .flex_1()
        .px(px(SPACE_M))
        .rounded(px(CONTROL_RADIUS))
        .border_1()
        .border_color(if active {
            theme.accent
        } else {
            theme.control_border
        })
        .text_color(theme.text)
        .child(if active {
            format!("{text}|")
        } else if text.is_empty() {
            "\u{200b}".to_owned()
        } else {
            text.to_owned()
        })
}

fn labelled(label: &str, field: Stateful<Div>, theme: &Theme) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap(px(SPACE_M))
        .child(
            div()
                .w(px(96.))
                .flex_none()
                .text_color(theme.text_muted)
                .child(label.to_owned()),
        )
        .child(field)
}

fn small_button(
    id: impl Into<ElementId>,
    selector: String,
    label: &str,
    theme: &Theme,
) -> Stateful<Div> {
    let hover = theme.hover;
    let pressed = theme.pressed;
    div()
        .id(id)
        .debug_selector(move || selector)
        .flex()
        .items_center()
        .justify_center()
        .h(px(FIELD_HEIGHT))
        .px(px(SPACE_M))
        .rounded(px(CONTROL_RADIUS))
        .border_1()
        .border_color(theme.control_border)
        .text_color(theme.text)
        .hover(move |style| style.bg(hover))
        .active(move |style| style.bg(pressed))
        .child(label.to_owned())
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

impl Render for ChangeSignatureDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let viewport = window.viewport_size();
        let width = px(WIDTH);
        let left = (viewport.width - width) / 2.;
        let top = viewport.height * 0.12;
        let active = self.active;

        let name = field_box(
            "change-signature-name-field",
            "change-signature-name".to_owned(),
            &self.name.text,
            active == Field::Name,
            &theme,
        )
        .on_click(cx.listener(|this, _, window, cx| {
            this.active = Field::Name;
            window.focus(&this.focus, cx);
            cx.notify();
        }));
        let visibility = field_box(
            "change-signature-visibility-field",
            "change-signature-visibility".to_owned(),
            &self.visibility.text,
            active == Field::Visibility,
            &theme,
        )
        .on_click(cx.listener(|this, _, window, cx| {
            this.active = Field::Visibility;
            window.focus(&this.focus, cx);
            cx.notify();
        }));
        let return_type = field_box(
            "change-signature-return-field",
            "change-signature-return".to_owned(),
            &self.return_type.text,
            active == Field::ReturnType,
            &theme,
        )
        .on_click(cx.listener(|this, _, window, cx| {
            this.active = Field::ReturnType;
            window.focus(&this.focus, cx);
            cx.notify();
        }));

        let mut rows = Vec::with_capacity(self.parameters.len());
        for index in 0..self.parameters.len() {
            let type_text = self.parameters[index].type_name.text.clone();
            let name_text = self.parameters[index].name.text.clone();
            let default_text = self.parameters[index].default_value.text.clone();
            let type_field = field_box(
                ("change-signature-type", index),
                format!("change-signature-type-{index}"),
                &type_text,
                active == Field::ParameterType(index),
                &theme,
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                this.active = Field::ParameterType(index);
                window.focus(&this.focus, cx);
                cx.notify();
            }));
            let name_field = field_box(
                ("change-signature-param-name", index),
                format!("change-signature-name-{index}"),
                &name_text,
                active == Field::ParameterName(index),
                &theme,
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                this.active = Field::ParameterName(index);
                window.focus(&this.focus, cx);
                cx.notify();
            }));
            let default_field = field_box(
                ("change-signature-default", index),
                format!("change-signature-default-{index}"),
                &default_text,
                active == Field::ParameterDefault(index),
                &theme,
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                this.active = Field::ParameterDefault(index);
                window.focus(&this.focus, cx);
                cx.notify();
            }));
            let buttons = div()
                .flex()
                .items_center()
                .gap(px(SPACE_S))
                .child(
                    icon_button(
                        ("change-signature-up", index),
                        "general/arrowUp",
                        "Move Up",
                        Some("Alt+Up".into()),
                        &theme,
                    )
                    .debug_selector(move || format!("change-signature-up-{index}"))
                    .size(px(FIELD_HEIGHT))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.move_parameter(index, -1, cx);
                    })),
                )
                .child(
                    icon_button(
                        ("change-signature-down", index),
                        "general/arrowDown",
                        "Move Down",
                        Some("Alt+Down".into()),
                        &theme,
                    )
                    .debug_selector(move || format!("change-signature-down-{index}"))
                    .size(px(FIELD_HEIGHT))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.move_parameter(index, 1, cx);
                    })),
                )
                .child(
                    icon_button(
                        ("change-signature-remove", index),
                        "general/closeSmall",
                        "Remove Parameter",
                        None,
                        &theme,
                    )
                    .debug_selector(move || format!("change-signature-remove-{index}"))
                    .size(px(FIELD_HEIGHT))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.remove_parameter(index, cx);
                    })),
                );
            rows.push(
                div()
                    .flex()
                    .items_center()
                    .gap(px(SPACE_S))
                    .child(type_field)
                    .child(name_field)
                    .child(default_field)
                    .child(buttons)
                    .into_any_element(),
            );
        }

        let exceptions = (!self.exceptions.is_empty()).then(|| {
            let names = self
                .exceptions
                .iter()
                .map(|exception| exception.type_name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            div()
                .debug_selector(|| "change-signature-exceptions".to_owned())
                .text_color(theme.text_muted)
                .child(format!("Throws: {names}"))
        });

        let note = if let Some(error) = &self.error {
            Some(
                div()
                    .debug_selector(|| "change-signature-error".to_owned())
                    .text_color(theme.text)
                    .child(error.clone())
                    .into_any_element(),
            )
        } else if self.loading {
            Some(
                div()
                    .debug_selector(|| "change-signature-loading".to_owned())
                    .text_color(theme.text_muted)
                    .child("Reading the signature")
                    .into_any_element(),
            )
        } else {
            self.status.as_ref().map(|status| {
                div()
                    .debug_selector(|| "change-signature-status".to_owned())
                    .text_color(theme.text_muted)
                    .child(status.clone())
                    .into_any_element()
            })
        };

        let body = surface(&theme)
            .id("change-signature")
            .debug_selector(|| "change-signature".to_owned())
            .key_context("ChangeSignature")
            .track_focus(&self.focus)
            .occlude()
            .w(width)
            .p(px(DIALOG_PADDING))
            .gap(px(DIALOG_GAP))
            .text_size(px(UI_SIZE))
            .text_color(theme.text)
            .on_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)))
            .on_mouse_down_out(cx.listener(|this, _, window, cx| this.close(window, cx)))
            .child(
                div()
                    .text_size(px(TEXT_TITLE))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Change Signature"),
            )
            .child(labelled("Name", name, &theme))
            .child(labelled("Visibility", visibility, &theme))
            .child(labelled("Return type", return_type, &theme))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(div().font_weight(FontWeight::MEDIUM).child("Parameters"))
                    .child(
                        small_button(
                            "change-signature-add",
                            "change-signature-add".to_owned(),
                            "+ Add parameter",
                            &theme,
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.add_parameter(cx))),
                    ),
            )
            .children(rows)
            .children(exceptions)
            .children(note)
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(SPACE_M))
                    .child(
                        dialog_button(
                            "change-signature-cancel",
                            "change-signature-cancel".to_owned(),
                            "Cancel",
                            false,
                            &theme,
                        )
                        .on_click(cx.listener(|this, _, window, cx| this.close(window, cx))),
                    )
                    .child(
                        dialog_button(
                            "change-signature-preview",
                            "change-signature-preview".to_owned(),
                            "Preview",
                            false,
                            &theme,
                        )
                        .on_click(cx.listener(|this, _, window, cx| this.preview(window, cx))),
                    )
                    .child(
                        dialog_button(
                            "change-signature-refactor",
                            "change-signature-refactor".to_owned(),
                            "Refactor",
                            true,
                            &theme,
                        )
                        .on_click(cx.listener(|this, _, window, cx| this.refactor(window, cx))),
                    ),
            );

        div()
            .absolute()
            .left(left)
            .top(top)
            .child(enter("change-signature-enter", body))
    }
}
