//! Edits that a language server computes: Ctrl+Alt+L, Ctrl+Alt+O, Alt+Enter, and the
//! occurrences of the symbol under the caret.

use std::ops::Range;
use std::time::Duration;

use lang_api::{
    ActionEntry, ActionKind, ActionQuery, Confidence, DocumentEdit, GenerateKind, SourceSpan,
    TextPoint, TextSpan, UnwrapEdit, WorkspaceChange, ask,
};
use text_core::{Edit, RopeStore, TextStore};
use ui_kit::*;

use crate::semantic::{NO_ANSWER, first, offset_at, point_at};
use crate::view::{EditorEvent, EditorView};

/// The caret rests this long before its occurrences are asked for.
const HIGHLIGHT_DELAY: Duration = Duration::from_millis(250);

/// A row of a pick list: an action to apply or a place to go to.
pub(crate) enum Pick {
    Action(ActionEntry),
    /// A move needs a destination from a second list, so the pick only starts it.
    Move {
        label: String,
    },
    /// A change of signature opens a form of the window, so the pick only starts it.
    ChangeSignature {
        label: String,
    },
    Place {
        label: String,
        target: SourceSpan,
    },
    /// A row of Source Generate; most need a list of parts before any edit.
    Generate {
        label: String,
        kind: GenerateKind,
    },
    /// A row of Unwrap/Remove, with the edit it applies.
    Unwrap {
        label: String,
        edit: UnwrapEdit,
    },
}

impl Pick {
    pub fn label(&self) -> &str {
        match self {
            Self::Action(entry) => &entry.title,
            Self::Move { label }
            | Self::ChangeSignature { label }
            | Self::Place { label, .. }
            | Self::Generate { label, .. }
            | Self::Unwrap { label, .. } => label,
        }
    }
}

/// Alt+Enter, Generate, usages, the type hierarchy: rows picked with the keys of
/// completion.
pub(crate) struct ActionMenu {
    pub at: usize,
    pub rows: Vec<Pick>,
    pub selected: usize,
}

impl ActionMenu {
    pub fn window(&self) -> Range<usize> {
        let start = self.selected.saturating_sub(crate::semantic::MAX_SHOWN - 1);
        start..(start + crate::semantic::MAX_SHOWN).min(self.rows.len())
    }
}

/// Edits of one server answer, all in the coordinates of `store`, in the order the
/// buffer needs. LSP does not sort them; inserts at one place keep their order, since
/// that is the order of their text. Overlapping edits are forbidden by LSP but sent by
/// real servers: `None`, nothing of them applies.
pub(crate) fn server_edits(store: &RopeStore, edits: Vec<DocumentEdit>) -> Option<Vec<Edit>> {
    let mut edits: Vec<(Range<usize>, String)> = edits
        .into_iter()
        .map(|edit| {
            let start = offset_at(store, edit.span.start);
            let end = offset_at(store, edit.span.end).max(start);
            (start..end, edit.text.replace("\r\n", "\n"))
        })
        .collect();
    edits.sort_by_key(|(range, _)| (range.start, range.end));
    let overlap = edits.windows(2).any(|pair| pair[1].0.start < pair[0].0.end);
    if overlap {
        return None;
    }
    let edits = edits
        .into_iter()
        .map(|(range, text)| Edit::new(range, text.as_str()));
    Some(edits.collect())
}

const OVERLAPPING: &str = "The language server sent overlapping edits, nothing was changed";

/// Whole-word occurrences of `needle` in `text`, in text order.
pub(crate) fn word_ranges(text: &str, needle: &str) -> Vec<Range<usize>> {
    let is_word =
        |character: char| character.is_alphanumeric() || character == '_' || character == '$';
    let mut ranges = Vec::new();
    if needle.is_empty() {
        return ranges;
    }
    let mut from = 0;
    while let Some(found) = text[from..].find(needle) {
        let start = from + found;
        let end = start + needle.len();
        let clean = !text[..start].chars().next_back().is_some_and(is_word)
            && !text[end..].chars().next().is_some_and(is_word);
        if clean {
            ranges.push(start..end);
        }
        from = end;
    }
    ranges
}

/// Edits of every `TextDocumentEdit` of one answer, each in the coordinates of the text
/// after the one before it. `None` when any of them overlaps, so nothing applies.
fn server_batches(
    store: &RopeStore,
    sources: impl IntoIterator<Item = Vec<DocumentEdit>>,
) -> Option<Vec<Vec<Edit>>> {
    let mut probe = store.clone();
    let mut format = text_core::FileFormat::default();
    let mut batches = Vec::new();
    for edits in sources {
        if edits.is_empty() {
            continue;
        }
        let edits = server_edits(&probe, edits)?;
        text_core::apply_edits(&mut probe, &mut format, &edits);
        batches.push(edits);
    }
    Some(batches)
}

fn whole(store: &RopeStore) -> TextSpan {
    TextSpan {
        start: TextPoint::default(),
        end: point_at(store, store.len()),
    }
}

impl EditorView {
    pub(crate) fn asked_version(&self) -> Option<(String, i32, usize)> {
        let (lsp, state) = (self.lsp.as_ref()?, self.state.as_ref()?);
        let caret = state.selections().primary().head;
        Some((lsp.uri.clone(), state.version() as i32, caret))
    }

    /// Edits in the coordinates of this document as it is now, as one undo step.
    pub fn apply_document_edits(&mut self, edits: Vec<DocumentEdit>, cx: &mut Context<Self>) {
        self.apply_document_batches(vec![edits], cx);
    }

    /// Several `TextDocumentEdit`s of this file from one answer. Each works on the text
    /// after the one before it and all undo in one step.
    pub fn apply_document_batches(
        &mut self,
        batches: Vec<Vec<DocumentEdit>>,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = &mut self.state else {
            return;
        };
        let caret = state.selections().primary().head;
        let Some(batches) = server_batches(state.store(), batches) else {
            self.show_note(caret, OVERLAPPING, cx);
            return;
        };
        let change = state.apply_edits_batches(batches);
        self.changed(change, cx);
    }

    /// Applies a change if the text is still the one asked about. A change of this
    /// document alone applies here; one that reaches other files or moves files goes
    /// to the window, which applies every part of it.
    ///
    /// Each `TextDocumentEdit` applies to the text after the one before it, as LSP
    /// says. All of them are checked first: if any one overlaps, nothing is applied.
    pub(crate) fn apply_change(
        &mut self,
        version: i32,
        change: WorkspaceChange,
        cx: &mut Context<Self>,
    ) {
        let (Some(lsp), Some(state)) = (&self.lsp, &self.state) else {
            return;
        };
        if state.version() as i32 != version || change.is_empty() {
            return;
        }
        let local = change.renames.is_empty()
            && change
                .edits
                .iter()
                .all(|source| source.uri == lsp.uri || source.edits.is_empty());
        if !local {
            cx.emit(EditorEvent::ApplyChange(change));
            return;
        }
        let caret = state.selections().primary().head;
        let sources = change.edits.into_iter().map(|source| source.edits);
        let Some(batches) = server_batches(state.store(), sources) else {
            self.show_note(caret, OVERLAPPING, cx);
            return;
        };
        let Some(state) = &mut self.state else {
            return;
        };
        let change = state.apply_edits_batches(batches);
        self.changed(change, cx);
    }

    fn apply_entry(&mut self, version: i32, entry: ActionEntry, cx: &mut Context<Self>) {
        let change = WorkspaceChange {
            edits: entry.edits,
            renames: entry.renames,
        };
        self.apply_change(version, change, cx);
    }

    /// Opens a pick list at the caret, or says `empty` when there is nothing.
    pub(crate) fn show_picks(
        &mut self,
        at: usize,
        rows: Vec<Pick>,
        empty: &str,
        cx: &mut Context<Self>,
    ) {
        if rows.is_empty() {
            self.show_note(at, empty.to_owned(), cx);
            return;
        }
        if let Some(lsp) = &mut self.lsp {
            lsp.completion = None;
            lsp.note = None;
            lsp.move_to = None;
            lsp.actions = Some(ActionMenu {
                at,
                rows,
                selected: 0,
            });
            cx.notify();
        }
    }

    /// Ctrl+Alt+L: the whole file, through the flags of the Reformat File dialog.
    pub(crate) fn reformat(&mut self, cx: &mut Context<Self>) {
        cx.emit(EditorEvent::Reformat);
    }

    pub(crate) fn ask_actions(
        &mut self,
        span: TextSpan,
        only: Option<ActionKind>,
        cx: &mut Context<Self>,
        done: impl FnOnce(&mut Self, i32, usize, Vec<ActionEntry>, &mut Context<Self>) + 'static,
    ) {
        let Some((uri, version, caret)) = self.asked_version() else {
            return;
        };
        let Some(provider) = self.lsp.as_ref().and_then(|lsp| lsp.support.actions()) else {
            return;
        };
        let query = ActionQuery {
            uri,
            version,
            span,
            only,
        };
        let replies = ask(provider, query);
        cx.spawn(async move |this, cx| {
            let answer = first(replies).await;
            this.update(cx, |this, cx| match answer {
                Some((_, Confidence::Unknown)) => this.show_note(caret, NO_ANSWER, cx),
                Some((entries, _)) => done(this, version, caret, entries, cx),
                None => {}
            })
            .ok();
        })
        .detach();
    }

    /// Ctrl+Alt+O.
    pub(crate) fn optimize_imports(&mut self, cx: &mut Context<Self>) {
        let Some(span) = self.state.as_ref().map(|state| whole(state.store())) else {
            return;
        };
        let only = Some(ActionKind::OrganizeImports);
        self.ask_actions(span, only, cx, |this, version, _, entries, cx| {
            if let Some(entry) = entries.into_iter().next() {
                this.apply_entry(version, entry, cx);
            }
        });
    }

    /// The selection, or the caret, as a span.
    pub(crate) fn selection_span(&self) -> Option<TextSpan> {
        let state = self.state.as_ref()?;
        let selection = state.selections().primary();
        Some(TextSpan {
            start: point_at(state.store(), selection.start()),
            end: point_at(state.store(), selection.end()),
        })
    }

    /// Alt+Enter: fixes of the problem under the caret first, then the other actions.
    pub(crate) fn show_actions(&mut self, cx: &mut Context<Self>) {
        let Some(span) = self.selection_span() else {
            return;
        };
        self.ask_actions(span, None, cx, |this, _, caret, mut entries, cx| {
            entries.sort_by_key(|entry| {
                let fix = entry.kind == Some(ActionKind::QuickFix);
                (!entry.preferred, !fix)
            });
            let rows = entries
                .into_iter()
                .filter(|entry| !entry.edits.is_empty() || !entry.renames.is_empty())
                .map(Pick::Action)
                .collect();
            this.show_picks(caret, rows, "No actions here", cx);
        });
    }

    /// Keys of an open action list. Returns whether the key was taken.
    pub(crate) fn action_key(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
        let Some(lsp) = &mut self.lsp else {
            return false;
        };
        let Some(menu) = &mut lsp.actions else {
            return false;
        };
        let rows = menu.rows.len();
        let mut moved = false;
        match key {
            "up" => {
                menu.selected = (menu.selected + rows - 1) % rows;
                moved = true;
            }
            "down" => {
                menu.selected = (menu.selected + 1) % rows;
                moved = true;
            }
            "escape" => lsp.actions = None,
            "enter" | "tab" => {
                let picked = lsp.actions.take().map(|mut menu| {
                    let selected = menu.selected;
                    menu.rows.swap_remove(selected)
                });
                let version = self.state.as_ref().map(|state| state.version() as i32);
                match (picked, version) {
                    (Some(Pick::Action(entry)), Some(version)) => {
                        self.apply_entry(version, entry, cx);
                    }
                    (Some(Pick::Move { .. }), _) => self.start_move(cx),
                    (Some(Pick::ChangeSignature { .. }), _) => self.change_signature(cx),
                    (Some(Pick::Place { target, .. }), _) => self.go_to_location(&target, cx),
                    (Some(Pick::Generate { kind, .. }), _) => self.start_generation(kind, cx),
                    (Some(Pick::Unwrap { edit, .. }), _) => self.apply_unwrap(edit, cx),
                    _ => {}
                }
            }
            _ => return false,
        }
        if moved {
            self.highlight_action_row();
        }
        cx.notify();
        true
    }

    /// Marks the text an Unwrap choice leaves in place, so the pick list shows what
    /// stays before Enter applies it.
    pub(crate) fn highlight_action_row(&mut self) {
        let Some(lsp) = &mut self.lsp else {
            return;
        };
        let Some(menu) = &lsp.actions else {
            return;
        };
        let Some(Pick::Unwrap { edit, .. }) = menu.rows.get(menu.selected) else {
            return;
        };
        lsp.highlights = std::iter::once(edit.start..edit.end).collect();
    }

    /// Ctrl+Shift+F7: shows every occurrence of the thing under the caret, from the
    /// language server when it answers and from the whole words of the text otherwise.
    pub(crate) fn highlight_usages(&mut self, cx: &mut Context<Self>) {
        let Some(state) = &self.state else {
            return;
        };
        let selection = state.selections().primary();
        let word = state.caret_word().or_else(|| {
            (!selection.is_empty())
                .then(|| text_core::commands::word_at(state.store(), selection.start()))
                .filter(|word| !word.is_empty())
        });
        let Some(word) = word else {
            return;
        };
        let needle = state.store().slice(word);
        self.usages = Some(word_ranges(&state.text(), &needle));
        let Some(support) = self.lsp.as_ref().map(|lsp| lsp.support.clone()) else {
            cx.notify();
            return;
        };
        let Some(provider) = support.highlights() else {
            cx.notify();
            return;
        };
        let Some((spot, _)) = self.spot() else {
            cx.notify();
            return;
        };
        let version = spot.version;
        let asked = match &mut self.lsp {
            Some(lsp) => {
                lsp.highlight_asked += 1;
                lsp.highlight_asked
            }
            None => {
                cx.notify();
                return;
            }
        };
        let replies = ask(provider, spot);
        cx.spawn(async move |this, cx| {
            let answer = first(replies).await;
            this.update(cx, |this, cx| {
                let Some((spans, _)) = answer else {
                    return;
                };
                let (Some(lsp), Some(state)) = (&mut this.lsp, &this.state) else {
                    return;
                };
                if lsp.highlight_asked != asked
                    || state.version() as i32 != version
                    || this.usages.is_none()
                {
                    return;
                }
                let mut ranges: Vec<Range<usize>> = spans
                    .into_iter()
                    .map(|span| {
                        let start = offset_at(state.store(), span.start);
                        start..offset_at(state.store(), span.end).max(start)
                    })
                    .collect();
                ranges.sort_by_key(|range| range.start);
                this.usages = Some(ranges);
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// F3 and Shift+F3 while Highlight Usages is on.
    pub(crate) fn step_usage(&mut self, forward: bool, cx: &mut Context<Self>) {
        let (Some(usages), Some(state)) = (self.usages.clone(), &mut self.state) else {
            return;
        };
        let selection = state.selections().primary();
        let (start, end) = (selection.start(), selection.end());
        let empty = selection.is_empty();
        let target = if forward {
            usages
                .iter()
                .find(|range| {
                    if empty {
                        range.start > start
                    } else {
                        range.start >= end
                    }
                })
                .or_else(|| usages.first())
        } else {
            usages
                .iter()
                .rev()
                .find(|range| {
                    if empty {
                        range.end < start
                    } else {
                        range.end <= start
                    }
                })
                .or_else(|| usages.last())
        };
        let Some(target) = target else {
            return;
        };
        state.select_range(target.clone());
        self.autoscroll = true;
        self.caret_on = true;
        cx.notify();
    }

    /// Asks for the occurrences under the caret once it rests.
    pub(crate) fn highlight_later(&mut self, cx: &mut Context<Self>) {
        let caret = self
            .state
            .as_ref()
            .map(|state| state.selections().primary().head);
        let Some(lsp) = &mut self.lsp else {
            return;
        };
        if lsp.support.highlights().is_none() {
            return;
        }
        lsp.highlight_asked += 1;
        let asked = lsp.highlight_asked;
        // Occurrences stay while the caret is on one of them, so they do not blink.
        let on_one = caret.is_some_and(|caret| {
            lsp.highlights
                .iter()
                .any(|range| range.start <= caret && caret <= range.end)
        });
        if !on_one {
            lsp.highlights.clear();
        }
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(HIGHLIGHT_DELAY).await;
            this.update(cx, |this, cx| this.highlight(asked, cx)).ok();
        })
        .detach();
    }

    fn highlight(&mut self, asked: u64, cx: &mut Context<Self>) {
        let Some((spot, _)) = self.spot() else {
            return;
        };
        let Some(lsp) = &self.lsp else {
            return;
        };
        if lsp.highlight_asked != asked {
            return;
        }
        let Some(provider) = lsp.support.highlights() else {
            return;
        };
        let version = spot.version;
        let replies = ask(provider, spot);
        cx.spawn(async move |this, cx| {
            let answer = first(replies).await;
            this.update(cx, |this, cx| {
                let (Some(lsp), Some(state)) = (&mut this.lsp, &this.state) else {
                    return;
                };
                if lsp.highlight_asked != asked || state.version() as i32 != version {
                    return;
                }
                let spans = answer.map(|(spans, _)| spans).unwrap_or_default();
                lsp.highlights = spans
                    .into_iter()
                    .map(|span| {
                        let start = offset_at(state.store(), span.start);
                        start..offset_at(state.store(), span.end).max(start)
                    })
                    .collect();
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use lang_api::{DocumentEdit, TextPoint, TextSpan};
    use text_core::{FileFormat, RopeStore, TextStore};

    use super::{server_batches, server_edits};
    use crate::EditorState;

    const TEXT: &str = "import b;\nimport a;\nclass X {}\n";

    fn edit(start: (u32, u32), end: (u32, u32), text: &str) -> DocumentEdit {
        let point = |(line, character)| TextPoint { line, character };
        DocumentEdit {
            span: TextSpan {
                start: point(start),
                end: point(end),
            },
            text: text.to_owned(),
        }
    }

    /// What the editor does with an answer: sort, check, apply as one step.
    fn apply(edits: Vec<DocumentEdit>) -> Option<String> {
        let mut state = EditorState::new(TEXT, FileFormat::default(), None, false);
        let edits = server_edits(&RopeStore::from_text(TEXT), edits)?;
        state.apply_edits(edits);
        Some(state.text())
    }

    // The cases of the qa check of T-0025: lists a server may send.

    #[test]
    fn unsorted_edits_apply_in_the_order_of_the_text() {
        let edits = vec![
            edit((1, 0), (1, 9), "import c;"),
            edit((0, 0), (0, 9), "import z;"),
        ];
        assert_eq!(
            apply(edits).as_deref(),
            Some("import z;\nimport c;\nclass X {}\n")
        );
    }

    #[test]
    fn inserts_at_one_place_keep_the_order_of_the_answer() {
        let edits = vec![edit((0, 0), (0, 0), "A\n"), edit((0, 0), (0, 0), "B\n")];
        assert_eq!(
            apply(edits).as_deref(),
            Some("A\nB\nimport b;\nimport a;\nclass X {}\n")
        );
    }

    #[test]
    fn overlapping_edits_change_nothing() {
        let edits = vec![edit((0, 0), (0, 9), "x"), edit((0, 5), (1, 2), "y")];
        assert_eq!(apply(edits), None);
    }

    #[test]
    fn an_insert_where_a_replace_starts_is_not_an_overlap() {
        let edits = vec![
            edit((0, 0), (0, 9), "import z;"),
            edit((0, 0), (0, 0), "// a\n"),
        ];
        assert_eq!(
            apply(edits).as_deref(),
            Some("// a\nimport z;\nimport a;\nclass X {}\n")
        );
    }

    #[test]
    fn one_overlapping_document_edit_stops_all_of_them() {
        let sources = vec![
            vec![edit((0, 0), (0, 9), "import z;")],
            vec![edit((0, 0), (0, 9), "x"), edit((0, 5), (1, 2), "y")],
        ];
        assert!(server_batches(&RopeStore::from_text(TEXT), sources).is_none());
    }

    #[test]
    fn several_document_edits_of_one_answer_undo_in_one_step() {
        let sources = vec![
            vec![edit((0, 0), (0, 0), "// header\n")],
            vec![edit((0, 0), (1, 0), "// changed\n")],
        ];
        let batches = server_batches(&RopeStore::from_text(TEXT), sources).unwrap();
        let mut state = EditorState::new(TEXT, FileFormat::default(), None, false);
        state.apply_edits_batches(batches);
        assert_eq!(
            state.text(),
            "// changed\nimport b;\nimport a;\nclass X {}\n"
        );
        state.perform(keymap::editor::UNDO, 10);
        assert_eq!(state.text(), TEXT);
    }

    #[test]
    fn each_document_edit_works_on_the_text_of_the_one_before() {
        // The first edit adds a line; the second one replaces it by its new place.
        let sources = vec![
            vec![edit((0, 0), (0, 0), "// header\n")],
            vec![edit((0, 0), (1, 0), "// changed\n")],
        ];
        let batches = server_batches(&RopeStore::from_text(TEXT), sources).unwrap();
        let mut store = RopeStore::from_text(TEXT);
        let mut format = FileFormat::default();
        for edits in batches {
            text_core::apply_edits(&mut store, &mut format, &edits);
        }
        assert_eq!(
            store.text(),
            "// changed\nimport b;\nimport a;\nclass X {}\n"
        );
    }
}
