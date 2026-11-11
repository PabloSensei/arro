//! Ctrl+O, Ctrl+I, Ctrl+Alt+H, Ctrl+Shift+B, Ctrl+Shift+I and Ctrl+Alt+Shift+C: the
//! structure of the code around the caret, as the language describes it.

use std::sync::atomic::{AtomicU64, Ordering};

use futures_util::StreamExt;
use lang_api::{
    AddOverridableMethodsRequest, Confidence, DefinitionPreviewQuery, DocumentSpot, NavigationKind,
    NavigationQuery, OverridableMethod, OverridableMethodsQuery, ReferenceQuery, SourceSpan, ask,
};
use syntax::{DocumentId, HighlightSpan};
use text_core::{Change, RopeStore, TextStore};
use ui_kit::*;

use crate::actions::Pick;
use crate::semantic::{NO_ANSWER, first};
use crate::view::{EditorEvent, EditorView};

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

/// Ctrl+O and Ctrl+I: the methods of the type at the caret, filtered by typing. Space
/// marks a method, Ctrl+A marks all of them, Enter adds the marked ones in one step.
pub(crate) struct OverridePopup {
    pub at: usize,
    spot: DocumentSpot,
    pub methods: Vec<OverridableMethod>,
    /// The methods that pass the filter, as indices into `methods`.
    pub shown: Vec<usize>,
    pub selected: usize,
    pub checked: Vec<bool>,
    pub text: String,
    pub only_abstract: bool,
}

impl OverridePopup {
    fn new(
        at: usize,
        spot: DocumentSpot,
        methods: Vec<OverridableMethod>,
        only_abstract: bool,
    ) -> Self {
        let checked = vec![false; methods.len()];
        let mut popup = Self {
            at,
            spot,
            methods,
            shown: Vec::new(),
            selected: 0,
            checked,
            text: String::new(),
            only_abstract,
        };
        popup.filter();
        popup
    }

    fn filter(&mut self) {
        let query = self.text.to_lowercase();
        self.shown = self
            .methods
            .iter()
            .enumerate()
            .filter(|(_, method)| {
                if query.is_empty() {
                    return true;
                }
                let parameters = method.parameters.join(", ");
                format!(
                    "{} {} {} {}",
                    method.owner, method.name, parameters, method.abstract_method
                )
                .to_lowercase()
                .contains(&query)
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

    /// The marked methods, or the one under the cursor when none is marked.
    fn chosen(&self) -> Vec<OverridableMethod> {
        let marked: Vec<OverridableMethod> = self
            .checked
            .iter()
            .enumerate()
            .filter(|(_, checked)| **checked)
            .map(|(index, _)| self.methods[index].clone())
            .collect();
        if !marked.is_empty() {
            return marked;
        }
        self.shown
            .get(self.selected)
            .map(|index| self.methods[*index].clone())
            .into_iter()
            .collect()
    }
}

/// Ctrl+Shift+I: the declaration at the caret, with its syntax colours.
pub(crate) struct DefinitionPopup {
    pub at: usize,
    pub text: String,
    pub spans: Vec<HighlightSpan>,
    target: Option<SourceSpan>,
}

static NEXT_SOURCE: AtomicU64 = AtomicU64::new(1);

impl EditorView {
    /// Ctrl+O, and Ctrl+I with `only_abstract`.
    pub(crate) fn override_methods(&mut self, only_abstract: bool, cx: &mut Context<Self>) {
        let Some((spot, caret)) = self.spot() else {
            return;
        };
        let Some(provider) = self
            .lsp
            .as_ref()
            .and_then(|lsp| lsp.support.overridable_methods())
        else {
            return;
        };
        let replies = ask(
            provider,
            OverridableMethodsQuery {
                spot: spot.clone(),
                only_abstract,
            },
        );
        cx.spawn(async move |this, cx| {
            let answer = first(replies).await;
            this.update(cx, |this, cx| match answer {
                Some((_, Confidence::Unknown)) => this.show_note(caret, NO_ANSWER, cx),
                Some((methods, _)) if methods.methods.is_empty() => {
                    let text = if only_abstract {
                        "Nothing to implement here"
                    } else {
                        "Nothing to override here"
                    };
                    this.show_note(caret, text, cx);
                }
                Some((methods, _)) => {
                    let methods = methods.methods;
                    if let Some(lsp) = &mut this.lsp {
                        lsp.completion = None;
                        lsp.actions = None;
                        lsp.note = None;
                        lsp.move_to = None;
                        lsp.rename = None;
                        lsp.overrides =
                            Some(OverridePopup::new(caret, spot, methods, only_abstract));
                    }
                    cx.notify();
                }
                None => {}
            })
            .ok();
        })
        .detach();
    }

    /// Typed text goes to an open Override or Implement popup. Returns whether it did.
    pub(crate) fn override_typed(&mut self, text: &str, cx: &mut Context<Self>) -> bool {
        let Some(popup) = self.lsp.as_mut().and_then(|lsp| lsp.overrides.as_mut()) else {
            return false;
        };
        popup.text.push_str(text.trim_end_matches(['\r', '\n']));
        popup.filter();
        cx.notify();
        true
    }

    /// Keys of an open Override or Implement popup. Returns whether the key was taken.
    pub(crate) fn override_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        let key = event.keystroke.key.to_lowercase();
        let modifiers = event.keystroke.modifiers;
        let Some(lsp) = &mut self.lsp else {
            return false;
        };
        let Some(popup) = &mut lsp.overrides else {
            return false;
        };
        let rows = popup.shown.len();
        match key.as_str() {
            "escape" => lsp.overrides = None,
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
                let spot = popup.spot.clone();
                lsp.overrides = None;
                if !chosen.is_empty() {
                    self.add_overrides(spot, chosen, cx);
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

    fn add_overrides(
        &mut self,
        spot: DocumentSpot,
        methods: Vec<OverridableMethod>,
        cx: &mut Context<Self>,
    ) {
        let Some(provider) = self
            .lsp
            .as_ref()
            .and_then(|lsp| lsp.support.add_overridable_methods())
        else {
            return;
        };
        let version = spot.version;
        let replies = ask(provider, AddOverridableMethodsRequest { spot, methods });
        cx.spawn(async move |this, cx| {
            let answer = first(replies).await;
            this.update(cx, |this, cx| match answer {
                Some((_, Confidence::Unknown)) => {
                    let caret = this.spot().map_or(0, |(_, caret)| caret);
                    this.show_note(caret, NO_ANSWER, cx);
                }
                Some((change, _)) => this.apply_change(version, change, cx),
                None => {}
            })
            .ok();
        })
        .detach();
    }

    /// Ctrl+Shift+B: the declared type with every place it can be declared.
    pub(crate) fn type_declaration(&mut self, cx: &mut Context<Self>) {
        let Some((spot, caret)) = self.spot() else {
            return;
        };
        let Some(provider) = self.lsp.as_ref().and_then(|lsp| lsp.support.navigation()) else {
            return;
        };
        let replies = ask(
            provider,
            NavigationQuery {
                kind: NavigationKind::TypeDefinition,
                spot,
            },
        );
        cx.spawn(async move |this, cx| {
            let answer = first(replies).await;
            this.update(cx, |this, cx| match answer {
                Some((_, Confidence::Unknown)) => this.show_note(caret, NO_ANSWER, cx),
                Some((spans, _)) if spans.is_empty() => {
                    this.show_note(caret, "Cannot find the type declaration", cx)
                }
                Some((spans, _)) if spans.len() == 1 => this.go_to_location(&spans[0], cx),
                Some((spans, _)) => {
                    let rows = spans
                        .into_iter()
                        .map(|target| Pick::Place {
                            label: crate::refactor::place_label(&target, None),
                            target,
                        })
                        .collect();
                    this.show_picks(caret, rows, "Cannot find the type declaration", cx);
                }
                None => {}
            })
            .ok();
        })
        .detach();
    }

    /// Ctrl+Shift+I: the source of the definition at the caret.
    pub(crate) fn quick_definition(&mut self, cx: &mut Context<Self>) {
        let Some((spot, caret)) = self.spot() else {
            return;
        };
        let Some(provider) = self
            .lsp
            .as_ref()
            .and_then(|lsp| lsp.support.definition_preview())
        else {
            return;
        };
        let language = self
            .state
            .as_ref()
            .and_then(|state| state.language().cloned());
        let syntax = self.syntax.clone();
        let replies = ask(provider, DefinitionPreviewQuery(spot));
        cx.spawn(async move |this, cx| {
            let answer = first(replies).await;
            let opened = this.update(cx, |this, cx| match answer {
                Some((_, Confidence::Unknown)) => {
                    this.show_note(caret, NO_ANSWER, cx);
                    None
                }
                Some((preview, _)) if preview.text.trim().is_empty() => {
                    this.show_note(caret, "No definition found", cx);
                    None
                }
                Some((preview, _)) => {
                    let text = preview.text;
                    if let Some(lsp) = &mut this.lsp {
                        lsp.note = None;
                        lsp.overrides = None;
                        lsp.definition = Some(DefinitionPopup {
                            at: caret,
                            text: text.clone(),
                            spans: Vec::new(),
                            target: preview.target,
                        });
                    }
                    cx.notify();
                    Some(text)
                }
                None => None,
            });
            let Ok(Some(text)) = opened else {
                return;
            };
            let Some(language) = language else {
                return;
            };
            let id = DocumentId(NEXT_SOURCE.fetch_add(1, Ordering::Relaxed));
            let (sender, mut receiver) = futures_channel::mpsc::unbounded();
            syntax.open(id, language, move |snapshot| {
                sender.unbounded_send(snapshot).ok();
            });
            let text_for_syntax = text.clone();
            syntax.update(
                id,
                1,
                RopeStore::from_text(&text_for_syntax),
                &Change::default(),
            );
            let snapshot = receiver.next().await;
            syntax.close(id);
            let Some(snapshot) = snapshot else {
                return;
            };
            this.update(cx, |this, cx| {
                if let Some(popup) = this.lsp.as_mut().and_then(|lsp| lsp.definition.as_mut())
                    && popup.text == text
                {
                    popup.spans = snapshot.highlights;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Escape closes Ctrl+Shift+I, Enter goes to the declaration.
    pub(crate) fn definition_key(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
        let Some(lsp) = &mut self.lsp else {
            return false;
        };
        let Some(popup) = lsp.definition.take() else {
            return false;
        };
        match key {
            "escape" => {}
            "enter" => {
                if let Some(target) = popup.target {
                    self.go_to_location(&target, cx);
                }
            }
            _ => {
                lsp.definition = Some(popup);
                return false;
            }
        }
        cx.notify();
        true
    }

    /// Ctrl+Alt+Shift+C: the reference to the symbol at the caret.
    pub(crate) fn copy_reference(&mut self, cx: &mut Context<Self>) {
        let Some((spot, caret)) = self.spot() else {
            return;
        };
        let Some(provider) = self
            .lsp
            .as_ref()
            .and_then(|lsp| lsp.support.reference_string())
        else {
            return;
        };
        let replies = ask(provider, ReferenceQuery(spot));
        cx.spawn(async move |this, cx| {
            let answer = first(replies).await;
            this.update(cx, |this, cx| match answer {
                Some((_, Confidence::Unknown)) => this.show_note(caret, NO_ANSWER, cx),
                Some((reference, _)) if reference.is_empty() => {
                    this.show_note(caret, "Nothing to copy here", cx)
                }
                Some((reference, _)) => {
                    cx.write_to_clipboard(ClipboardItem::new_string(reference));
                }
                None => {}
            })
            .ok();
        })
        .detach();
    }

    /// Ctrl+Shift+H: the window shows where the method is defined, inherited or still
    /// awaited by an abstract type.
    pub(crate) fn method_hierarchy(&mut self, cx: &mut Context<Self>) {
        let Some((spot, _)) = self.spot() else {
            return;
        };
        if self
            .lsp
            .as_ref()
            .and_then(|lsp| lsp.support.method_hierarchy())
            .is_none()
        {
            return;
        }
        cx.emit(EditorEvent::MethodHierarchy(spot));
    }

    /// Ctrl+Alt+H: the window shows the call hierarchy of the place.
    pub(crate) fn call_hierarchy(&mut self, cx: &mut Context<Self>) {
        let Some((spot, _)) = self.spot() else {
            return;
        };
        if self
            .lsp
            .as_ref()
            .and_then(|lsp| lsp.support.call_hierarchy())
            .is_none()
        {
            return;
        }
        cx.emit(EditorEvent::CallHierarchy(spot));
    }
}
