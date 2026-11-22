//! Ctrl+F6: the form of Change Signature. The language describes the signature under
//! the caret, the form edits it and the language turns the new one into edits.

use std::ops::Range;

use futures_util::StreamExt;
use lang_api::{
    ChangeSignatureInfo, ChangeSignatureRequest, Confidence, DocumentSpot, GraphVisibility,
    MethodSignature, SignatureException, SignatureParameter, WorkspaceChange, ask,
};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::Workspace;

use crate::prompt::TextField;

const WIDTH: f32 = 760.;

/// The width of the three buttons at the end of a parameter row: Move Up, Move Down and
/// Remove. The column of the headers keeps the same width, so a header stands over its
/// field.
const ROW_CONTROLS: f32 = 3. * CONTROL_HEIGHT + 2. * SPACE_S;

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

/// The modifier in front of a method or a constructor, as the visibility field offers it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Visibility {
    Public,
    Protected,
    Package,
    Private,
}

impl Visibility {
    const ALL: [Self; 4] = [Self::Public, Self::Protected, Self::Package, Self::Private];

    /// The value the field shows; a package-private member has no keyword in the text.
    fn label(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Protected => "protected",
            Self::Package => "package-private",
            Self::Private => "private",
        }
    }

    /// The word the language takes back, and the empty word of a package-private member.
    fn modifier(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Protected => "protected",
            Self::Package => "",
            Self::Private => "private",
        }
    }

    fn from_modifier(modifier: &str) -> Self {
        match modifier.trim() {
            "public" => Self::Public,
            "protected" => Self::Protected,
            "private" => Self::Private,
            _ => Self::Package,
        }
    }

    /// The place of the value in `options`, the first place when the value is not there.
    fn position_in(self, options: &[Self]) -> usize {
        options.iter().position(|value| *value == self).unwrap_or(0)
    }

    /// The values the list offers for a declaration: all of them when the language gives no
    /// list, else the allowed ones in the order of [`Self::ALL`].
    fn options(allowed: &[GraphVisibility]) -> Vec<Self> {
        if allowed.is_empty() {
            return Self::ALL.to_vec();
        }
        let allowed: Vec<Self> = allowed
            .iter()
            .map(|level| Self::from_level(*level))
            .collect();
        Self::ALL
            .into_iter()
            .filter(|value| allowed.contains(value))
            .collect()
    }

    fn from_level(level: GraphVisibility) -> Self {
        match level {
            GraphVisibility::Public => Self::Public,
            GraphVisibility::Protected => Self::Protected,
            GraphVisibility::Package => Self::Package,
            GraphVisibility::Private => Self::Private,
        }
    }
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
    visibility: Visibility,
    /// The values the visibility list offers: all of them unless the language allows fewer.
    visibility_options: Vec<Visibility>,
    /// The modifier the signature takes: the word of the server until a value is chosen.
    visibility_word: String,
    visibility_open: bool,
    visibility_marked: usize,
    return_type: TextField,
    parameters: Vec<ParameterRow>,
    /// The row the keys of the form work on, kept apart from the field that has the
    /// caret so that Alt+Up and Alt+Down reach a row from the Name field too.
    selected: usize,
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
            visibility: Visibility::Public,
            visibility_options: Visibility::ALL.to_vec(),
            visibility_word: Visibility::Public.modifier().to_owned(),
            visibility_open: false,
            visibility_marked: 0,
            return_type: TextField::default(),
            parameters: Vec::new(),
            selected: 0,
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
        self.visibility_options = Visibility::options(&info.allowed_visibility);
        if let Some(error) = info.error {
            self.error = Some(error);
        } else if confidence == Confidence::Unknown {
            self.error = Some(NO_ANSWER.to_owned());
        } else if let Some(signature) = info.signature {
            self.name.text = signature.name;
            // The list shows a value the place allows. The word of the server goes back
            // unchanged until a value is chosen, so a modifier the place leaves out stays out.
            let current = Visibility::from_modifier(&signature.visibility);
            self.visibility = if self.visibility_options.contains(&current) {
                current
            } else {
                self.visibility_options.first().copied().unwrap_or(current)
            };
            self.visibility_word = signature.visibility.trim().to_owned();
            self.visibility_open = false;
            self.visibility_marked = self.visibility.position_in(&self.visibility_options);
            self.return_type.text = signature.return_type;
            self.parameters = signature
                .parameters
                .into_iter()
                .map(ParameterRow::from)
                .collect();
            self.selected = 0;
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
            visibility: self.visibility_word.clone(),
            return_type: self.return_type.text.trim().to_owned(),
            parameters,
            exceptions: self.exceptions.clone(),
        })
    }

    /// The signature as the fields describe it, the way the preview under the table
    /// shows it. An empty visibility leaves the modifier out, as a package-private
    /// member has none, and an empty return type leaves the type out, as in a
    /// constructor.
    fn signature_text(&self) -> String {
        let mut head = Vec::new();
        if !self.visibility_word.is_empty() {
            head.push(self.visibility_word.clone());
        }
        let return_type = self.return_type.text.trim();
        if !return_type.is_empty() {
            head.push(return_type.to_owned());
        }
        let name = self.name.text.trim();
        head.push(if name.is_empty() {
            "name".to_owned()
        } else {
            name.to_owned()
        });
        let parameters = self
            .parameters
            .iter()
            .map(|row| {
                match (
                    row.type_name.text.trim().is_empty(),
                    row.name.text.trim().is_empty(),
                ) {
                    (true, true) => String::new(),
                    (true, false) => row.name.text.trim().to_owned(),
                    (false, true) => row.type_name.text.trim().to_owned(),
                    (false, false) => {
                        format!("{} {}", row.type_name.text.trim(), row.name.text.trim())
                    }
                }
            })
            .filter(|parameter| !parameter.is_empty())
            .collect::<Vec<_>>()
            .join(", ");
        format!("{}({parameters})", head.join(" "))
    }

    /// The signature of the preview, for the window tests.
    #[cfg(test)]
    pub(crate) fn preview_text(&self) -> String {
        self.signature_text()
    }

    /// Escape closes the visibility list open in the form. Returns whether it was open.
    fn close_nested(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.visibility_open {
            return false;
        }
        self.visibility_open = false;
        cx.notify();
        true
    }

    /// Keys of the form: text goes to the active field, Tab moves between fields, the
    /// visibility list opens on Space and walks on the arrows, and the parameter row
    /// under the selection follows Alt+Insert, Alt+Delete, Alt+Up and Alt+Down.
    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        if self.visibility_open {
            match key {
                "escape" => {
                    self.close_nested(cx);
                    cx.stop_propagation();
                    return;
                }
                "up" => {
                    self.mark_visibility(-1, cx);
                    cx.stop_propagation();
                    return;
                }
                "down" => {
                    self.mark_visibility(1, cx);
                    cx.stop_propagation();
                    return;
                }
                "enter" => {
                    self.choose_visibility(self.visibility_marked, cx);
                    window.focus(&self.focus, cx);
                    cx.stop_propagation();
                    return;
                }
                _ => {}
            }
        }
        if key == "escape" {
            if !self.close_nested(cx) {
                self.close(window, cx);
            }
            cx.stop_propagation();
            return;
        }
        if key == "enter" && !self.busy && !self.loading {
            self.refactor(window, cx);
            cx.stop_propagation();
            return;
        }
        if key == "tab" && !event.keystroke.modifiers.control {
            self.visibility_open = false;
            if event.keystroke.modifiers.shift {
                self.active = self.previous_field();
            } else {
                self.active = self.next_field();
            }
            self.select_active_parameter();
            cx.notify();
            cx.stop_propagation();
            return;
        }
        if event.keystroke.modifiers.alt {
            match key {
                "insert" => {
                    self.add_parameter(cx);
                    cx.stop_propagation();
                    return;
                }
                "delete" => {
                    if let Some(index) = self.selected_parameter() {
                        self.remove_parameter(index, cx);
                    }
                    cx.stop_propagation();
                    return;
                }
                "up" => {
                    if let Some(index) = self.selected_parameter() {
                        self.move_parameter(index, -1, cx);
                    }
                    cx.stop_propagation();
                    return;
                }
                "down" => {
                    if let Some(index) = self.selected_parameter() {
                        self.move_parameter(index, 1, cx);
                    }
                    cx.stop_propagation();
                    return;
                }
                _ => {}
            }
        }
        if self.active == Field::Visibility {
            match key {
                "space" => {
                    self.open_visibility(cx);
                    cx.stop_propagation();
                    return;
                }
                "backspace" | "delete" => {
                    cx.stop_propagation();
                    return;
                }
                _ => {}
            }
            if event.keystroke.key_char.is_some()
                && !event.keystroke.modifiers.control
                && !event.keystroke.modifiers.platform
            {
                cx.stop_propagation();
                return;
            }
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
            // The visibility is a list of four values, not a field that takes text.
            Field::Visibility => None,
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

    fn open_visibility(&mut self, cx: &mut Context<Self>) {
        // A place that allows one value has no list to open.
        if self.visibility_options.len() > 1 {
            self.visibility_open = true;
            self.visibility_marked = self.visibility.position_in(&self.visibility_options);
        }
        self.active = Field::Visibility;
        cx.notify();
    }

    fn mark_visibility(&mut self, step: isize, cx: &mut Context<Self>) {
        let last = self.visibility_options.len().saturating_sub(1);
        self.visibility_marked =
            (self.visibility_marked as isize + step).clamp(0, last as isize) as usize;
        cx.notify();
    }

    fn choose_visibility(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(value) = self.visibility_options.get(index).copied() else {
            return;
        };
        self.visibility = value;
        self.visibility_word = value.modifier().to_owned();
        self.visibility_open = false;
        self.visibility_marked = self.visibility.position_in(&self.visibility_options);
        self.status = None;
        self.error = None;
        cx.notify();
    }

    /// The row the keys work on, kept inside the table after a row leaves.
    fn selected_parameter(&self) -> Option<usize> {
        (!self.parameters.is_empty()).then(|| self.selected.min(self.parameters.len() - 1))
    }

    fn select_active_parameter(&mut self) {
        if let Some(index) = self.active_parameter() {
            self.selected = index.min(self.parameters.len().saturating_sub(1));
        }
    }

    fn add_parameter(&mut self, cx: &mut Context<Self>) {
        self.parameters.push(ParameterRow::new());
        self.selected = self.parameters.len() - 1;
        self.active = Field::ParameterType(self.selected);
        self.status = None;
        self.error = None;
        cx.notify();
    }

    fn remove_parameter(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.parameters.len() {
            return;
        }
        self.parameters.remove(index);
        if self.parameters.is_empty() {
            self.selected = 0;
        } else {
            self.selected = self.selected.min(self.parameters.len() - 1);
        }
        self.active = self.settle_field(self.active);
        self.select_active_parameter();
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
        self.selected = target;
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

#[allow(clippy::too_many_arguments)]
fn field_box(
    mouse: FieldMouse,
    id: impl Into<ElementId>,
    selector: String,
    field: &TextField,
    active: bool,
    hint: Option<FieldHint>,
    warning: Option<&'static str>,
    theme: &Theme,
) -> Stateful<Div> {
    let children = match hint {
        Some(hint) => text_field_children_hinted(
            mouse,
            "change-signature-field",
            field,
            theme.text,
            theme.text,
            theme.selection,
            theme.caret,
            18.,
            active,
            hint,
        ),
        None => text_field_children_named(
            mouse,
            "change-signature-field",
            field,
            theme.text,
            theme.text,
            theme.selection,
            theme.caret,
            18.,
            active,
        ),
    };
    let frame = div()
        .id(id)
        .debug_selector(move || selector)
        .flex()
        .items_center()
        .h(px(CONTROL_HEIGHT))
        .min_w(px(0.))
        .flex_1()
        .px(px(SPACE_M))
        .rounded(px(CONTROL_RADIUS))
        .border_1()
        .border_color(if warning.is_some() {
            theme.error
        } else if active {
            theme.accent
        } else {
            theme.control_border
        })
        .text_color(theme.text);
    let frame = match warning {
        Some(text) => frame
            .child(
                div()
                    .debug_selector(|| "change-signature-default-warning".to_owned())
                    .child(icon_sized("status/warning", TEXT_SMALL, theme)),
            )
            .tooltip(move |_, cx| tooltip(text, None, cx)),
        None => frame,
    };
    frame.children(children)
}

/// A header of the parameter table over its column: muted text on the height of a field,
/// as wide as the field under it.
fn column_header(id: &'static str, text: &str, theme: &Theme) -> Stateful<Div> {
    div()
        .id(id)
        .debug_selector(move || id.to_owned())
        .flex()
        .items_center()
        .flex_1()
        .min_w(px(0.))
        .text_size(px(TEXT_SMALL))
        .text_color(theme.text_muted)
        .child(text.to_owned())
}

/// The words of `text[from..to]`: names, types and values of the signature preview.
fn words_in(text: &str, from: usize, to: usize) -> Vec<Range<usize>> {
    let bytes = text.as_bytes();
    let mut ranges = Vec::new();
    let mut at = from;
    while at < to {
        if bytes[at].is_ascii_alphanumeric() || matches!(bytes[at], b'_' | b'$') {
            let start = at;
            while at < to && (bytes[at].is_ascii_alphanumeric() || matches!(bytes[at], b'_' | b'$'))
            {
                at += 1;
            }
            ranges.push(start..at);
        } else {
            at += 1;
        }
    }
    ranges
}

fn highlight(color: Hsla) -> HighlightStyle {
    HighlightStyle {
        color: Some(color),
        ..HighlightStyle::default()
    }
}

fn is_modifier(word: &str) -> bool {
    matches!(
        word,
        "public"
            | "protected"
            | "private"
            | "static"
            | "final"
            | "abstract"
            | "default"
            | "synchronized"
            | "native"
            | "strictfp"
    )
}

/// The colours of the signature preview: the modifier, the types, the name and the
/// parameters of the signature as the editor would paint them.
fn signature_highlights(text: &str, theme: &Theme) -> Vec<(Range<usize>, HighlightStyle)> {
    let open = text.find('(').unwrap_or(text.len());
    let close = text.rfind(')').unwrap_or(text.len());
    let mut highlights = Vec::new();
    let head = words_in(text, 0, open);
    if let Some((name, types)) = head.split_last() {
        for range in types {
            let word = &text[range.clone()];
            let color = if is_modifier(word) {
                theme.keyword
            } else {
                theme.semantic_type
            };
            highlights.push((range.clone(), highlight(color)));
        }
        highlights.push((name.clone(), highlight(theme.semantic_method)));
    }
    if open < close {
        let mut start = open + 1;
        let mut depth = 0i32;
        for (offset, character) in text[open + 1..close].char_indices() {
            match character {
                '<' | '[' => depth += 1,
                '>' | ']' => depth -= 1,
                ',' if depth == 0 => {
                    let end = open + 1 + offset;
                    parameter_highlights(text, start, end, theme, &mut highlights);
                    start = end + 1;
                }
                _ => {}
            }
        }
        parameter_highlights(text, start, close, theme, &mut highlights);
    }
    highlights
}

/// The type words and the name of one parameter of the preview.
fn parameter_highlights(
    text: &str,
    start: usize,
    end: usize,
    theme: &Theme,
    highlights: &mut Vec<(Range<usize>, HighlightStyle)>,
) {
    let words = words_in(text, start, end);
    let Some((name, types)) = words.split_last() else {
        return;
    };
    for range in types {
        highlights.push((range.clone(), highlight(theme.semantic_type)));
    }
    highlights.push((name.clone(), highlight(theme.semantic_parameter)));
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
        .h(px(CONTROL_HEIGHT))
        .px(px(SPACE_M))
        .rounded(px(CONTROL_RADIUS))
        .border_1()
        .border_color(theme.control_border)
        .text_color(theme.text)
        .hover(move |style| style.bg(hover))
        .active(move |style| style.bg(pressed))
        .child(label.to_owned())
}

impl Render for ChangeSignatureDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let viewport = window.viewport_size();
        let width = px(WIDTH);
        let left = (viewport.width - width) / 2.;
        let top = viewport.height * 0.12;
        let label_width = form_label_width(window, &["Name", "Visibility", "Return type"]);
        let active = self.active;

        let name = field_box(
            FieldMouse::new(cx, |this| Some(&mut this.name)),
            "change-signature-name-field",
            "change-signature-name".to_owned(),
            &self.name,
            active == Field::Name,
            None,
            None,
            &theme,
        )
        .on_click(cx.listener(|this, _, window, cx| {
            this.active = Field::Name;
            this.visibility_open = false;
            window.focus(&this.focus, cx);
            cx.notify();
        }));
        let mut visibility_field = select_field(
            "change-signature-visibility",
            self.visibility.label(),
            self.visibility_open,
            &theme,
        )
        .debug_selector(|| "change-signature-visibility".to_owned())
        .flex_1();
        // One value allowed: the field shows it muted, and its list does not open.
        if self.visibility_options.len() < 2 {
            visibility_field = visibility_field.text_color(theme.text_muted);
        }
        let visibility_field = visibility_field.on_click(cx.listener(|this, _, window, cx| {
            if this.visibility_open {
                this.visibility_open = false;
            } else {
                this.open_visibility(cx);
            }
            window.focus(&this.focus, cx);
            cx.notify();
        }));
        let visibility_menu = self.visibility_open.then(|| {
            let mut menu = select_menu(&theme)
                .debug_selector(|| "change-signature-visibility-menu".to_owned());
            for (index, value) in self.visibility_options.iter().copied().enumerate() {
                let selector = format!("change-signature-visibility-option-{}", value.label());
                menu = menu.child(
                    select_option(
                        ("change-signature-visibility-option", index),
                        value.label(),
                        self.visibility == value,
                        self.visibility_marked == index,
                        &theme,
                    )
                    .debug_selector(move || selector.clone())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.choose_visibility(index, cx);
                        window.focus(&this.focus, cx);
                    })),
                );
            }
            menu
        });
        let visibility = div()
            .flex()
            .flex_col()
            .gap(px(SPACE_XS))
            .child(visibility_field)
            .children(visibility_menu);
        let return_type = field_box(
            FieldMouse::new(cx, |this| Some(&mut this.return_type)),
            "change-signature-return-field",
            "change-signature-return".to_owned(),
            &self.return_type,
            active == Field::ReturnType,
            None,
            None,
            &theme,
        )
        .on_click(cx.listener(|this, _, window, cx| {
            this.active = Field::ReturnType;
            this.visibility_open = false;
            window.focus(&this.focus, cx);
            cx.notify();
        }));

        let mut rows = Vec::with_capacity(self.parameters.len());
        for index in 0..self.parameters.len() {
            let hint = FieldHint {
                text: "value for call sites",
                color: theme.text_muted,
            };
            let warning = (self.parameters[index].original_index.is_none()
                && self.parameters[index].default_value.text.trim().is_empty())
            .then_some("A new parameter needs a value for its call sites");
            let type_field = field_box(
                FieldMouse::new(cx, move |this| {
                    this.parameters
                        .get_mut(index)
                        .map(|parameter| &mut parameter.type_name)
                }),
                ("change-signature-type", index),
                format!("change-signature-type-{index}"),
                &self.parameters[index].type_name,
                active == Field::ParameterType(index),
                None,
                None,
                &theme,
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                this.active = Field::ParameterType(index);
                this.selected = index;
                this.visibility_open = false;
                window.focus(&this.focus, cx);
                cx.notify();
            }));
            let name_field = field_box(
                FieldMouse::new(cx, move |this| {
                    this.parameters
                        .get_mut(index)
                        .map(|parameter| &mut parameter.name)
                }),
                ("change-signature-param-name", index),
                format!("change-signature-name-{index}"),
                &self.parameters[index].name,
                active == Field::ParameterName(index),
                None,
                None,
                &theme,
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                this.active = Field::ParameterName(index);
                this.selected = index;
                this.visibility_open = false;
                window.focus(&this.focus, cx);
                cx.notify();
            }));
            let default_field = field_box(
                FieldMouse::new(cx, move |this| {
                    this.parameters
                        .get_mut(index)
                        .map(|parameter| &mut parameter.default_value)
                }),
                ("change-signature-default", index),
                format!("change-signature-default-{index}"),
                &self.parameters[index].default_value,
                active == Field::ParameterDefault(index),
                Some(hint),
                warning,
                &theme,
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                this.active = Field::ParameterDefault(index);
                this.selected = index;
                this.visibility_open = false;
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
                    .size(px(CONTROL_HEIGHT))
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
                    .size(px(CONTROL_HEIGHT))
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
                    .size(px(CONTROL_HEIGHT))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.remove_parameter(index, cx);
                    })),
                );
            let selected = index == self.selected;
            rows.push(
                form_aligned(
                    label_width,
                    div()
                        .id(("change-signature-row", index))
                        .debug_selector(move || format!("change-signature-row-{index}"))
                        .flex()
                        .items_center()
                        .gap(px(SPACE_S))
                        .rounded(px(ROW_RADIUS))
                        .when(selected, |this| this.bg(theme.selection_inactive))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.selected = index;
                            window.focus(&this.focus, cx);
                            cx.notify();
                        }))
                        .child(type_field)
                        .child(name_field)
                        .child(default_field)
                        .child(buttons),
                )
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

        let header = form_aligned(
            label_width,
            div()
                .flex()
                .items_center()
                .gap(px(SPACE_S))
                .child(column_header(
                    "change-signature-header-type",
                    "Type",
                    &theme,
                ))
                .child(column_header(
                    "change-signature-header-name",
                    "Name",
                    &theme,
                ))
                .child(column_header(
                    "change-signature-header-default",
                    "Default value",
                    &theme,
                ))
                .child(div().flex_none().w(px(ROW_CONTROLS))),
        );

        let signature = self.signature_text();
        let highlights = signature_highlights(&signature, &theme);
        let preview = div()
            .debug_selector(|| "change-signature-preview".to_owned())
            .flex()
            .flex_col()
            .gap(px(SPACE_XS))
            .child(
                div()
                    .text_size(px(TEXT_SMALL))
                    .text_color(theme.text_muted)
                    .child("Signature Preview"),
            )
            .child(
                div()
                    .id("change-signature-preview-text")
                    .debug_selector(|| "change-signature-preview-text".to_owned())
                    .flex()
                    .items_center()
                    .min_h(px(CONTROL_HEIGHT))
                    .px(px(SPACE_M))
                    .py(px(SPACE_S))
                    .rounded(px(CONTROL_RADIUS))
                    .border_1()
                    .border_color(theme.control_border)
                    .bg(theme.editor_bg)
                    .font_family(MONO_FONT)
                    .text_color(theme.text)
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(StyledText::new(signature).with_highlights(highlights)),
            );

        let shell = surface(&theme)
            .id("change-signature")
            .debug_selector(|| "change-signature".to_owned())
            .key_context("ChangeSignature")
            .track_focus(&self.focus)
            .text_size(px(UI_SIZE))
            .text_color(theme.text)
            .on_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)))
            .on_action(cx.listener(|this, _: &workspace::Dismiss, _, cx| {
                let closed = this.close_nested(cx);
                workspace::forward_dismiss(closed, cx);
            }));
        let contents = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .p(px(DIALOG_PADDING))
            .gap(px(DIALOG_GAP))
            .child(form_row(label_width, "Name", name, &theme))
            .child(form_row(label_width, "Visibility", visibility, &theme))
            .child(form_row(label_width, "Return type", return_type, &theme))
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
            .child(header)
            .children(rows)
            .children(exceptions)
            .children(note)
            .child(preview)
            .child(
                DialogButtons::new()
                    .extra(
                        dialog_button(
                            "change-signature-preview",
                            "change-signature-preview".to_owned(),
                            "Preview",
                            false,
                            true,
                            &theme,
                        )
                        .on_click(cx.listener(|this, _, window, cx| this.preview(window, cx))),
                    )
                    .primary(
                        dialog_button(
                            "change-signature-refactor",
                            "change-signature-refactor".to_owned(),
                            "Refactor",
                            true,
                            !self.busy && !self.loading,
                            &theme,
                        )
                        .on_click(cx.listener(|this, _, window, cx| this.refactor(window, cx))),
                    )
                    .other(
                        dialog_button(
                            "change-signature-cancel",
                            "change-signature-cancel".to_owned(),
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
                key: "change-signature",
                title: "Change Signature".into(),
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
