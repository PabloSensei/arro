//! A document: text, carets, history and the format it has on disk.

use crate::edit::{Edit, EditKind, EditSource};
use crate::encoding::{EolPatch, FileFormat, normalize_newlines};
use crate::history::{History, Step};
use crate::selection::{Selection, SelectionSet};
use crate::store::{RopeStore, TextStore};

/// What a change did, for the layers that follow the text. Apply `steps` in order: the
/// edits of one step are sorted, do not overlap and refer to the text after the step
/// before it. Undo of grouped typing gives several steps.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Change {
    pub steps: Vec<Vec<Edit>>,
}

impl Change {
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }
}

pub struct Buffer<S: TextStore = RopeStore> {
    store: S,
    selections: SelectionSet,
    history: History,
    format: FileFormat,
    /// Grows with every change, also with undo. Stale background results compare it.
    version: u64,
    /// Identifies the text: undo returns to an earlier state, new edits take a new one.
    state: u64,
    next_state: u64,
    saved_state: u64,
}

impl<S: TextStore> Buffer<S> {
    pub fn new(text: &str, format: FileFormat) -> Self {
        Self {
            store: S::from_text(text),
            selections: SelectionSet::default(),
            history: History::default(),
            format,
            version: 0,
            state: 0,
            next_state: 1,
            saved_state: 0,
        }
    }

    pub fn store(&self) -> &S {
        &self.store
    }

    pub fn snapshot(&self) -> S {
        self.store.clone()
    }

    pub fn text(&self) -> String {
        self.store.text()
    }

    pub fn format(&self) -> &FileFormat {
        &self.format
    }

    /// Changes how the text is written. The buffer counts as edited until it is saved;
    /// undo does not take this back.
    pub fn set_format(&mut self, format: FileFormat) {
        if self.format == format {
            return;
        }
        self.format = format;
        self.state = self.next_state;
        self.next_state += 1;
        self.version += 1;
    }

    pub fn selections(&self) -> &SelectionSet {
        &self.selections
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn is_dirty(&self) -> bool {
        self.state != self.saved_state
    }

    pub fn mark_saved(&mut self) {
        self.saved_state = self.state;
        self.history.seal();
    }

    pub fn history(&self) -> &History {
        &self.history
    }

    /// Moves the carets.
    pub fn set_selections(&mut self, selections: SelectionSet) {
        self.set_selections_keeping_history(selections);
        self.history.seal();
    }

    /// Offsets are snapped to character boundaries inside the text.
    fn set_selections_keeping_history(&mut self, selections: SelectionSet) {
        let store = &self.store;
        let clamp = |offset: usize| {
            let mut offset = offset.min(store.len());
            while !store.is_char_boundary(offset) {
                offset -= 1;
            }
            offset
        };
        self.selections = selections
            .map(|selection| Selection::new(clamp(selection.anchor), clamp(selection.head)));
    }

    /// Applies `edits`, which refer to the current text. Without `after` the carets follow
    /// the text. Inserted line ends of any kind become `\n`.
    pub fn apply(
        &mut self,
        edits: Vec<Edit>,
        after: Option<SelectionSet>,
        source: EditSource,
        kind: EditKind,
    ) -> Change {
        let edits = self.prepare(edits);
        if edits.is_empty() {
            return Change::default();
        }
        let before = self.selections.clone();
        let (inverse, eol) = self.replace(&edits);
        let after = after.unwrap_or_else(|| before.transformed(&edits));
        self.set_selections_keeping_history(after.clone());

        let state_before = self.state;
        self.state = self.next_state;
        self.next_state += 1;
        let step = Step {
            edits: edits.clone(),
            inverse,
            eol,
            state_before,
            state_after: self.state,
        };
        self.history.record(step, before, after, source, kind);
        Change { steps: vec![edits] }
    }

    /// Applies each batch in turn, in the coordinates of the text after the one before
    /// it, as one undo step.
    pub fn apply_batches(&mut self, batches: Vec<Vec<Edit>>) -> Change {
        let mut combined = Change::default();
        let mut applied = 0;
        for edits in batches {
            let change = self.apply(edits, None, EditSource::User, EditKind::Command);
            if change.is_empty() {
                continue;
            }
            applied += 1;
            combined.steps.extend(change.steps);
        }
        if applied > 1 {
            self.history.merge_last(applied);
        }
        combined
    }

    /// Joins the last `count` undo steps into one, so a late answer that belongs to an
    /// earlier insertion comes back with it.
    pub fn merge_last_undo(&mut self, count: usize) {
        self.history.merge_last(count);
    }

    /// Stops grouping, so the next change starts an undo step of its own.
    pub fn seal_undo(&mut self) {
        self.history.seal();
    }

    /// Reverts the latest transaction, or returns `None` when there is nothing to revert.
    pub fn undo(&mut self) -> Option<Change> {
        let transaction = self.history.pop_undo()?;
        let mut steps = Vec::with_capacity(transaction.steps.len());
        for step in transaction.steps.iter().rev() {
            self.replace_text(&step.inverse);
            for patch in step.eol.iter().rev() {
                self.format.eol.restore(patch);
            }
            self.state = step.state_before;
            steps.push(step.inverse.clone());
        }
        self.set_selections_keeping_history(transaction.before.clone());
        self.history.push_redo(transaction);
        Some(Change { steps })
    }

    pub fn redo(&mut self) -> Option<Change> {
        let transaction = self.history.pop_redo()?;
        let mut steps = Vec::with_capacity(transaction.steps.len());
        for step in &transaction.steps {
            self.replace(&step.edits);
            self.state = step.state_after;
            steps.push(step.edits.clone());
        }
        self.set_selections_keeping_history(transaction.after.clone());
        self.history.push_undo(transaction);
        Some(Change { steps })
    }

    /// Normalizes the text of `edits`, drops those that change nothing and checks order.
    fn prepare(&self, edits: Vec<Edit>) -> Vec<Edit> {
        let mut prepared: Vec<Edit> = Vec::with_capacity(edits.len());
        for edit in edits {
            assert!(
                edit.range.end <= self.store.len(),
                "edit past the end of the text"
            );
            assert!(
                prepared
                    .last()
                    .is_none_or(|last| last.range.end <= edit.range.start),
                "edits must be sorted and must not overlap"
            );
            let text = normalize_newlines(&edit.text).into_owned();
            if !edit.range.is_empty() || !text.is_empty() {
                prepared.push(Edit::new(edit.range, text));
            }
        }
        prepared
    }

    /// Replaces the text and returns what undoes it. `edits` refer to the text before the
    /// call, the returned ones to the text after it.
    fn replace(&mut self, edits: &[Edit]) -> (Vec<Edit>, Vec<EolPatch>) {
        let mut inverse = Vec::with_capacity(edits.len());
        let mut delta = 0isize;
        for edit in edits {
            let removed = self.store.slice(edit.range.clone());
            let start = (edit.range.start as isize + delta) as usize;
            inverse.push(Edit::new(start..start + edit.text.len(), removed));
            delta += edit.delta();
        }
        let mut eol = Vec::new();
        for (edit, undo) in edits.iter().zip(&inverse).rev() {
            let first_line = self.store.offset_to_line(edit.range.start);
            let removed_lines = undo.text.bytes().filter(|byte| *byte == b'\n').count();
            let inserted_lines = edit.text.bytes().filter(|byte| *byte == b'\n').count();
            eol.push(
                self.format
                    .eol
                    .splice(first_line, removed_lines, inserted_lines),
            );
        }
        self.replace_text(edits);
        (inverse, eol)
    }

    fn replace_text(&mut self, edits: &[Edit]) {
        for edit in edits.iter().rev() {
            self.store.replace(edit.range.clone(), &edit.text);
        }
        self.version += 1;
    }
}

/// Applies `edits`, sorted and without overlaps, to a text that no [`Buffer`] owns. Keeps the
/// table of mixed line ends in step, the way [`Buffer::apply`] does.
pub fn apply_edits<S: TextStore>(store: &mut S, format: &mut FileFormat, edits: &[Edit]) {
    let lines = |text: &str| text.bytes().filter(|byte| *byte == b'\n').count();
    for edit in edits.iter().rev() {
        let first_line = store.offset_to_line(edit.range.start);
        let removed = lines(&store.slice(edit.range.clone()));
        format.eol.splice(first_line, removed, lines(&edit.text));
        store.replace(edit.range.clone(), &edit.text);
    }
}
