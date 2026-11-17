//! Alt+Delete and Ctrl+Shift+Delete: Safe Delete of the declaration under the caret,
//! and Unwrap/Remove of the constructs around it.

use lang_api::{
    Confidence, DeletionKind, DocumentEdit, DocumentSpot, GenerateKind, MethodSignature,
    SourceSpan, TextSpan, UnwrapEdit, WorkspaceChange, ask,
};
use text_core::TextStore;
use ui_kit::*;

use crate::actions::Pick;
use crate::semantic::{NO_ANSWER, first, point_at};
use crate::view::{EditorEvent, EditorView, Usage as ShownUsage};

/// What Delete Anyway does.
pub(crate) enum DeleteAction {
    /// Remove the declaration from this document.
    Edit { edit: DocumentEdit },
    /// Remove the declaration and every argument of the calls, through the signature.
    Change {
        version: i32,
        change: WorkspaceChange,
    },
    /// The type is alone in its file, so the file goes too.
    File { path: std::path::PathBuf },
}

/// The dialog Alt+Delete shows when the symbol is used: View Usages and Delete Anyway.
pub(crate) struct SafeDeletePopup {
    pub at: usize,
    pub name: String,
    pub count: usize,
    usages: Vec<SourceSpan>,
    pub selected: usize,
    action: DeleteAction,
}

fn byte_range(store: &text_core::RopeStore, span: TextSpan) -> std::ops::Range<usize> {
    let start = crate::semantic::offset_at(store, span.start);
    let end = crate::semantic::offset_at(store, span.end).max(start);
    start..end
}

fn edit_of(
    store: &text_core::RopeStore,
    start: usize,
    end: usize,
    replacement: String,
) -> Option<DocumentEdit> {
    Some(DocumentEdit {
        span: TextSpan {
            start: point_at(store, start),
            end: point_at(store, end),
        },
        text: replacement,
    })
}

impl EditorView {
    /// Alt+Delete on the declaration under the caret.
    pub(crate) fn safe_delete(&mut self, cx: &mut Context<Self>) {
        let caret = self
            .state
            .as_ref()
            .map(|state| state.selections().primary().head);
        if let Some(caret) = caret {
            self.safe_delete_at(caret, cx);
        }
    }

    /// Alt+Delete on the first type of a file opened from the project tree.
    pub fn safe_delete_at_offset(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.safe_delete_at(offset, cx);
    }

    fn safe_delete_at(&mut self, offset: usize, cx: &mut Context<Self>) {
        let Some(state) = &self.state else {
            return;
        };
        let Some((spot, _)) = self.spot() else {
            return;
        };
        let (uri, version) = (spot.uri.clone(), spot.version);
        let Some(syntax) = self.lsp.as_ref().map(|lsp| lsp.support.syntax()) else {
            return;
        };
        let text = state.text();
        let Some(target) = syntax.deletion(&text, offset) else {
            self.show_note(offset, "Put the caret on a declaration to delete", cx);
            return;
        };
        let name = target.name.clone();
        let caret = offset;
        let Some(provider) = self.lsp.as_ref().and_then(|lsp| lsp.support.references()) else {
            self.show_note(caret, "Cannot search usages here", cx);
            return;
        };
        let store = state.store().clone();
        let target_spot = DocumentSpot {
            uri: uri.clone(),
            version,
            point: point_at(&store, offset),
        };
        let replies = ask(provider, target_spot);
        let name_range = target.name_range.clone();
        let declaration = target.range.clone();
        let kind = target.kind();
        let method = target.method.clone();
        let method_name = target.method_name.clone();
        let parameter_index = target.parameter_index;
        let sole_type = target.sole_type;
        let path = self.path().to_path_buf();
        let support = self.lsp.as_ref().map(|lsp| lsp.support.clone());
        cx.spawn(async move |this, cx| {
            let answer = first(replies).await;
            let Some((spans, confidence)) = answer else {
                return;
            };
            if confidence == Confidence::Unknown {
                this.update(cx, |this, cx| this.show_note(caret, NO_ANSWER, cx))
                    .ok();
                return;
            }
            let usages: Vec<SourceSpan> = spans
                .into_iter()
                .filter(|span| {
                    let store_range = byte_range(&store, span.span);
                    store_range.start >= name_range.end || store_range.end <= name_range.start
                })
                .collect();
            let count = usages.len();
            let declaration_edit =
                edit_of(&store, declaration.start, declaration.end, String::new());
            let mut action = if kind == DeletionKind::Type && sole_type {
                DeleteAction::File { path: path.clone() }
            } else {
                match declaration_edit {
                    Some(edit) => DeleteAction::Edit { edit },
                    None => return,
                }
            };
            if kind == DeletionKind::Parameter
                && count > 0
                && let (Some(_method), Some(method_name), Some(index)) =
                    (method, method_name, parameter_index)
                && let Some(support) = &support
                && let (Some(info_provider), Some(change_provider)) =
                    (support.change_signature_info(), support.change_signature())
            {
                let method_spot = DocumentSpot {
                    uri: uri.clone(),
                    version,
                    point: point_at(&store, method_name.start),
                };
                let info = ask(info_provider, method_spot.clone());
                if let Some((info, _)) = first(info).await
                    && let Some(signature) = info.signature
                {
                    let signature = without_parameter(signature, index, &name);
                    let changed = ask(
                        change_provider,
                        lang_api::ChangeSignatureRequest {
                            spot: method_spot,
                            signature,
                        },
                    );
                    if let Some((outcome, _)) = first(changed).await
                        && outcome.error.is_none()
                        && !outcome.change.is_empty()
                    {
                        action = DeleteAction::Change {
                            version,
                            change: outcome.change,
                        };
                    }
                }
            }
            if count == 0 {
                this.update(cx, |this, cx| this.apply_delete(action, cx))
                    .ok();
                return;
            }
            this.update(cx, |this, cx| {
                let Some(lsp) = &mut this.lsp else {
                    return;
                };
                lsp.completion = None;
                lsp.actions = None;
                lsp.note = None;
                lsp.rename = None;
                lsp.move_to = None;
                lsp.overrides = None;
                lsp.definition = None;
                lsp.generation = None;
                lsp.safe_delete = Some(SafeDeletePopup {
                    at: caret,
                    name,
                    count,
                    usages,
                    selected: 0,
                    action,
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn apply_delete(&mut self, action: DeleteAction, cx: &mut Context<Self>) {
        match action {
            DeleteAction::Edit { edit } => self.apply_document_edits(vec![edit], cx),
            DeleteAction::Change { version, change } => self.apply_change(version, change, cx),
            DeleteAction::File { path } => cx.emit(EditorEvent::DeleteFile(path)),
        }
    }

    /// Keys of the Safe Delete dialog. Returns whether the key was taken.
    pub(crate) fn safe_delete_key(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
        let Some(lsp) = &mut self.lsp else {
            return false;
        };
        if lsp.safe_delete.is_none() {
            return false;
        }
        match key {
            "escape" => {
                lsp.safe_delete = None;
            }
            "left" | "right" | "tab" => {
                if let Some(popup) = &mut lsp.safe_delete {
                    popup.selected = (popup.selected + 1) % 2;
                }
            }
            "enter" => {
                let Some(popup) = lsp.safe_delete.take() else {
                    return true;
                };
                if popup.selected == 0 {
                    let usages = popup
                        .usages
                        .into_iter()
                        .map(|target| ShownUsage {
                            name: popup.name.clone(),
                            target,
                        })
                        .collect();
                    cx.emit(EditorEvent::Usages(usages));
                } else {
                    self.apply_delete(popup.action, cx);
                }
            }
            _ => return false,
        }
        cx.notify();
        true
    }

    /// Ctrl+Shift+Delete: the constructs around the caret, nearest first.
    pub(crate) fn unwrap_remove(&mut self, cx: &mut Context<Self>) {
        let Some(state) = &self.state else {
            return;
        };
        let caret = state.selections().primary().head;
        let Some(syntax) = self.lsp.as_ref().map(|lsp| lsp.support.syntax()) else {
            return;
        };
        let edits = syntax.unwrap_edits(&state.text(), caret);
        if edits.is_empty() {
            self.show_note(caret, "Nothing to unwrap here", cx);
            return;
        }
        let rows: Vec<Pick> = edits
            .into_iter()
            .map(|edit| Pick::Unwrap {
                label: edit.title.clone(),
                edit,
            })
            .collect();
        self.show_picks(caret, rows, "Nothing to unwrap here", cx);
        if let Some(lsp) = &mut self.lsp {
            lsp.highlights.clear();
        }
        self.highlight_action_row();
        cx.notify();
    }

    /// Enter on an Unwrap row: one edit, one step of undo.
    pub(crate) fn apply_unwrap(&mut self, edit: UnwrapEdit, cx: &mut Context<Self>) {
        let Some(state) = &mut self.state else {
            return;
        };
        let caret = state.selections().primary().head;
        let text_len = state.store().len();
        let (start, end) = (edit.start.min(text_len), edit.end.min(text_len));
        let edit = DocumentEdit {
            span: TextSpan {
                start: point_at(&state.store(), start),
                end: point_at(&state.store(), end),
            },
            text: edit.replacement,
        };
        let _ = caret;
        self.apply_document_edits(vec![edit], cx);
        if let Some(lsp) = &mut self.lsp {
            lsp.highlights.clear();
        }
        cx.notify();
    }
}

fn without_parameter(mut signature: MethodSignature, index: usize, name: &str) -> MethodSignature {
    let mut removed = false;
    signature.parameters.retain(|parameter| {
        if removed {
            return true;
        }
        let same = parameter.original_index == Some(index) || parameter.name == name;
        if same {
            removed = true;
            return false;
        }
        true
    });
    signature
}

/// The rows of Source Generate, in the order IDEA lists them.
pub(crate) fn generate_rows() -> Vec<Pick> {
    [
        ("Constructor", GenerateKind::Constructor),
        ("Getter", GenerateKind::Getter),
        ("Setter", GenerateKind::Setter),
        ("Getter and Setter", GenerateKind::GetterAndSetter),
        ("equals() and hashCode()", GenerateKind::EqualsAndHashCode),
        ("toString()", GenerateKind::ToString),
        ("Override Methods", GenerateKind::OverrideMethods),
        ("Implement Methods", GenerateKind::ImplementMethods),
        ("Delegate Methods", GenerateKind::Delegate),
        ("Test", GenerateKind::Test),
    ]
    .into_iter()
    .map(|(label, kind)| Pick::Generate {
        label: label.to_owned(),
        kind,
    })
    .collect()
}
