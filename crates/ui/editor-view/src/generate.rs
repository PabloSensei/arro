//! Alt+Insert: the Source Generate list and the fields or methods it offers.

use lang_api::{
    Confidence, DocumentSpot, GenerateApplyRequest, GenerateChoice, GenerateKind, GenerateQuery,
    Generation, ask,
};
use ui_kit::*;

use crate::semantic::{NO_ANSWER, first};
use crate::view::{EditorEvent, EditorView};

/// The popup with a checkbox for each field or method a generation may include.
pub(crate) struct GeneratePopup {
    pub at: usize,
    spot: DocumentSpot,
    pub kind: GenerateKind,
    pub choices: Vec<GenerateChoice>,
    /// The choices that pass the filter, as indices into `choices`.
    pub shown: Vec<usize>,
    pub selected: usize,
    pub checked: Vec<bool>,
    pub text: String,
}

impl GeneratePopup {
    fn new(
        at: usize,
        spot: DocumentSpot,
        kind: GenerateKind,
        choices: Vec<GenerateChoice>,
    ) -> Self {
        let checked = choices.iter().map(|choice| choice.checked).collect();
        let mut popup = Self {
            at,
            spot,
            kind,
            choices,
            shown: Vec::new(),
            selected: 0,
            checked,
            text: String::new(),
        };
        popup.filter();
        popup
    }

    fn filter(&mut self) {
        let query = self.text.to_lowercase();
        self.shown = self
            .choices
            .iter()
            .enumerate()
            .filter(|(_, choice)| {
                query.is_empty()
                    || choice.label.to_lowercase().contains(&query)
                    || choice
                        .detail
                        .as_deref()
                        .is_some_and(|detail| detail.to_lowercase().contains(&query))
            })
            .map(|(index, _)| index)
            .collect();
        self.selected = 0;
    }

    fn toggle_selected(&mut self) {
        if let Some(index) = self.shown.get(self.selected).copied() {
            self.checked[index] = !self.checked[index];
            self.selected = (self.selected + 1).min(self.shown.len().saturating_sub(1));
        }
    }

    fn check_all(&mut self) {
        for index in &self.shown {
            self.checked[*index] = true;
        }
    }

    /// The marked choices, or the one under the cursor when none is marked.
    fn chosen(&self) -> Vec<String> {
        let marked: Vec<String> = self
            .checked
            .iter()
            .enumerate()
            .filter(|(_, checked)| **checked)
            .map(|(index, _)| self.choices[index].handle.clone())
            .collect();
        if !marked.is_empty() {
            return marked;
        }
        self.shown
            .get(self.selected)
            .map(|index| self.choices[*index].handle.clone())
            .into_iter()
            .collect()
    }
}

fn is_typed_key(key: &str) -> bool {
    key.chars().count() == 1
        || matches!(
            key,
            "space"
                | "comma"
                | "period"
                | "minus"
                | "equal"
                | "slash"
                | "backslash"
                | "semicolon"
                | "quote"
                | "backquote"
                | "bracketleft"
                | "bracketright"
        )
}

impl EditorView {
    /// Alt+Insert: the list of what can be generated here.
    pub(crate) fn generate_menu(&mut self, cx: &mut Context<Self>) {
        let Some(caret) = self
            .state
            .as_ref()
            .map(|state| state.selections().primary().head)
        else {
            return;
        };
        if self
            .lsp
            .as_ref()
            .is_none_or(|lsp| lsp.support.generate().is_none())
        {
            self.generate_source_actions(cx);
            return;
        }
        let rows = crate::delete::generate_rows();
        self.show_picks(caret, rows, "Nothing to generate here", cx);
    }

    /// Alt+Insert for a language that only lists its source actions: the actions
    /// themselves are the rows, as they were before the fixed Source Generate list.
    fn generate_source_actions(&mut self, cx: &mut Context<Self>) {
        let Some(span) = self.selection_span() else {
            return;
        };
        let only = Some(lang_api::ActionKind::Source);
        self.ask_actions(span, only, cx, |this, _, caret, entries, cx| {
            let rows = entries
                .into_iter()
                .filter(|entry| {
                    let code = entry.code.as_deref().unwrap_or_default();
                    code.starts_with("source.generate") || entry.title.starts_with("Generate")
                })
                .map(crate::actions::Pick::Action)
                .collect();
            this.show_picks(caret, rows, "Nothing to generate here", cx);
        });
    }

    /// A row of the Generate list is chosen.
    pub(crate) fn start_generation(&mut self, kind: GenerateKind, cx: &mut Context<Self>) {
        if kind == GenerateKind::OverrideMethods {
            return self.override_methods(false, cx);
        }
        if kind == GenerateKind::ImplementMethods {
            return self.override_methods(true, cx);
        }
        let Some((spot, caret)) = self.spot() else {
            return;
        };
        let Some(provider) = self.lsp.as_ref().and_then(|lsp| lsp.support.generate()) else {
            self.show_note(caret, "This language cannot generate sources", cx);
            return;
        };
        let replies = ask(
            provider,
            GenerateQuery {
                spot: spot.clone(),
                kind,
            },
        );
        cx.spawn(async move |this, cx| {
            let answer = first(replies).await;
            this.update(cx, |this, cx| match answer {
                Some((_, Confidence::Unknown)) => this.show_note(caret, NO_ANSWER, cx),
                Some((generation, _)) => this.generation_answer(spot, kind, caret, generation, cx),
                None => {}
            })
            .ok();
        })
        .detach();
    }

    fn generation_answer(
        &mut self,
        spot: DocumentSpot,
        kind: GenerateKind,
        caret: usize,
        generation: Generation,
        cx: &mut Context<Self>,
    ) {
        if let Some(error) = generation.error {
            self.show_note(caret, error, cx);
            return;
        }
        if !generation.choices.is_empty() {
            let Some(lsp) = &mut self.lsp else {
                return;
            };
            lsp.completion = None;
            lsp.actions = None;
            lsp.note = None;
            lsp.rename = None;
            lsp.move_to = None;
            lsp.overrides = None;
            lsp.definition = None;
            lsp.safe_delete = None;
            lsp.generation = Some(GeneratePopup::new(caret, spot, kind, generation.choices));
            cx.notify();
            return;
        }
        if !generation.files.is_empty() {
            cx.emit(EditorEvent::CreateFiles(generation.files));
            return;
        }
        if let Some(change) = generation.change
            && !change.is_empty()
        {
            let version = spot.version;
            self.apply_change(version, change, cx);
            return;
        }
        self.show_note(caret, "Nothing to generate here", cx);
    }

    /// Typed text filters an open Generate popup. Returns whether it took the text.
    pub(crate) fn generation_typed(&mut self, text: &str, cx: &mut Context<Self>) -> bool {
        let Some(popup) = self.lsp.as_mut().and_then(|lsp| lsp.generation.as_mut()) else {
            return false;
        };
        popup.text.push_str(text.trim_end_matches(['\r', '\n']));
        popup.filter();
        cx.notify();
        true
    }

    /// Keys of an open Generate popup. Returns whether the key was taken.
    pub(crate) fn generation_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        let key = event.keystroke.key.to_lowercase();
        let modifiers = event.keystroke.modifiers;
        let Some(lsp) = &mut self.lsp else {
            return false;
        };
        let Some(popup) = &mut lsp.generation else {
            return false;
        };
        let rows = popup.shown.len();
        match key.as_str() {
            "escape" => lsp.generation = None,
            "up" if rows > 0 => popup.selected = (popup.selected + rows - 1) % rows,
            "down" if rows > 0 => popup.selected = (popup.selected + 1) % rows,
            "space" if !modifiers.control && !modifiers.alt => popup.toggle_selected(),
            "a" if modifiers.control && !modifiers.alt => popup.check_all(),
            "backspace" => {
                popup.text.pop();
                popup.filter();
            }
            "enter" => {
                let chosen = popup.chosen();
                let (spot, kind) = (popup.spot.clone(), popup.kind);
                lsp.generation = None;
                if !chosen.is_empty() {
                    self.apply_generation(spot, kind, chosen, cx);
                }
            }
            other => {
                if is_typed_key(other) && !modifiers.control && !modifiers.alt {
                    return false;
                }
                return true;
            }
        }
        cx.notify();
        true
    }

    fn apply_generation(
        &mut self,
        spot: DocumentSpot,
        kind: GenerateKind,
        handles: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(provider) = self
            .lsp
            .as_ref()
            .and_then(|lsp| lsp.support.generate_apply())
        else {
            return;
        };
        let version = spot.version;
        let replies = ask(
            provider,
            GenerateApplyRequest {
                spot,
                kind,
                handles,
            },
        );
        let caret = self
            .state
            .as_ref()
            .map_or(0, |state| state.selections().primary().head);
        cx.spawn(async move |this, cx| {
            let answer = first(replies).await;
            this.update(cx, |this, cx| match answer {
                Some((_, Confidence::Unknown)) => this.show_note(caret, NO_ANSWER, cx),
                Some((generation, _)) if generation.error.is_some() => {
                    let error = generation.error.unwrap_or_default();
                    this.show_note(caret, error, cx);
                }
                Some((generation, _)) if !generation.files.is_empty() => {
                    cx.emit(EditorEvent::CreateFiles(generation.files));
                }
                Some((generation, _)) => match generation.change {
                    Some(change) if !change.is_empty() => this.apply_change(version, change, cx),
                    _ => this.show_note(caret, "Nothing to generate here", cx),
                },
                None => {}
            })
            .ok();
        })
        .detach();
    }
}
