//! A document: text, history and the format it has on disk, with one or more views of it.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use crate::edit::{Edit, EditKind, EditSource};
use crate::encoding::{Eol, EolPatch, EolStyle, FileFormat, TextEncoding, normalize_newlines};
use crate::history::{History, Step};
use crate::selection::{Selection, SelectionSet};
use crate::store::{RopeStore, TextStore};

/// Changes kept for the views that have not taken them into their carets and folds yet.
/// A view takes them at once, when the edit is reported, so the list stays short; the
/// limit only keeps a view nobody reports to from growing it forever.
const KEPT_CHANGES: usize = 64;

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

/// What a view takes from the changes of the document since it last looked.
pub enum Followed {
    /// Nothing changed.
    Nothing,
    /// The changes, in the order they happened.
    Changes(Change),
    /// The changes are older than the list keeps: their edits are unknown, so everything
    /// of the view that points into the text must be thrown away and read again.
    Missed,
}

/// What every view of one file shares: the text, its format, the undo history, the
/// version that tells a saved text from an edited one, and the changes the views have
/// not taken yet.
struct Shared<S: TextStore> {
    store: S,
    /// The bytes on disk: the encoding, the BOM and the table of the line ends.
    format: FileFormat,
    history: History,
    /// Grows with every change, also with undo. Stale background results compare it.
    version: u64,
    /// Identifies the text: undo returns to an earlier state, new edits take a new one.
    state: u64,
    next_state: u64,
    saved_state: u64,
    /// The number of the latest change.
    serial: u64,
    /// The latest changes, by number, for the views that have not followed them yet.
    changes: VecDeque<Logged>,
}

/// A change, for the views that follow it.
struct Logged {
    serial: u64,
    change: Change,
}

impl<S: TextStore> Shared<S> {
    /// Remembers a change for the other views of the text and gives it a number.
    fn record_change(&mut self, change: Change) {
        self.serial += 1;
        let serial = self.serial;
        self.changes.push_back(Logged { serial, change });
        if self.changes.len() > KEPT_CHANGES {
            self.changes.pop_front();
        }
    }
}

/// One view of a document. The text, its format, the undo history and the saved state
/// come from the [`Shared`] part, so a change made through any view is the text of all
/// of them at once. The carets and the selections belong to the view alone.
pub struct Buffer<S: TextStore = RopeStore> {
    shared: Arc<Mutex<Shared<S>>>,
    selections: SelectionSet,
    /// The number of the last change this view took into its own carets and folds.
    seen: u64,
}

impl<S: TextStore> Buffer<S> {
    pub fn new(text: &str, format: FileFormat) -> Self {
        Self {
            shared: Arc::new(Mutex::new(Shared {
                store: S::from_text(text),
                format,
                history: History::default(),
                version: 0,
                state: 0,
                next_state: 1,
                saved_state: 0,
                serial: 0,
                changes: VecDeque::new(),
            })),
            selections: SelectionSet::default(),
            seen: 0,
        }
    }

    /// Another view of the same document: the text, its format, the undo history and the
    /// saved state are shared, so an edit through either view is the text of both. The
    /// carets are this view's own.
    pub fn fork(&self) -> Self {
        let seen = self.shared.lock().unwrap().serial;
        Self {
            shared: self.shared.clone(),
            selections: SelectionSet::default(),
            seen,
        }
    }

    /// Whether the two buffers are views of one document.
    pub fn shares_document(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.shared, &other.shared)
    }

    /// Whether another view changed the document since this view last followed it.
    pub fn has_unseen_changes(&self) -> bool {
        let shared = self.shared.lock().unwrap();
        shared.serial != self.seen
    }

    /// The text for reading. A copy, which does not change when the document does; the
    /// store clones cheaply.
    pub fn store(&self) -> S {
        self.shared.lock().unwrap().store.clone()
    }

    pub fn snapshot(&self) -> S {
        self.store()
    }

    pub fn text(&self) -> String {
        self.shared.lock().unwrap().store.text()
    }

    /// The bytes on disk of this document: the encoding, the BOM and the line ends.
    pub fn format(&self) -> FileFormat {
        self.shared.lock().unwrap().format.clone()
    }

    /// The encoding for the status bar, without copying the table of line ends.
    pub fn encoding_name(&self) -> &'static str {
        let shared = self.shared.lock().unwrap();
        match shared.format.encoding {
            TextEncoding::Utf8 if shared.format.bom => "UTF-8 BOM",
            encoding => encoding.name(),
        }
    }

    /// The line ends for the status bar, without copying the table.
    pub fn eol_name(&self) -> &'static str {
        match self.shared.lock().unwrap().format.eol {
            EolStyle::Uniform(Eol::Lf) => "LF",
            EolStyle::Uniform(Eol::CrLf) => "CRLF",
            EolStyle::Uniform(Eol::Cr) => "CR",
            EolStyle::Mixed { .. } => "Mixed",
        }
    }

    /// Changes how the text is written. The document counts as edited until it is saved;
    /// undo does not take this back.
    pub fn set_format(&mut self, format: FileFormat) {
        let mut shared = self.shared.lock().unwrap();
        if shared.format == format {
            return;
        }
        shared.format = format;
        shared.state = shared.next_state;
        shared.next_state += 1;
        shared.version += 1;
    }

    pub fn selections(&self) -> &SelectionSet {
        &self.selections
    }

    pub fn version(&self) -> u64 {
        self.shared.lock().unwrap().version
    }

    pub fn is_dirty(&self) -> bool {
        let shared = self.shared.lock().unwrap();
        shared.state != shared.saved_state
    }

    pub fn mark_saved(&mut self) {
        let mut shared = self.shared.lock().unwrap();
        shared.saved_state = shared.state;
        shared.history.seal();
    }

    /// Moves the carets.
    pub fn set_selections(&mut self, selections: SelectionSet) {
        let mut shared = self.shared.lock().unwrap();
        self.selections = clamp_selections(&shared.store, selections);
        shared.history.seal();
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
        self.assert_followed();
        let mut shared = self.shared.lock().unwrap();
        let edits = prepare(&shared.store, edits);
        if edits.is_empty() {
            return Change::default();
        }
        let before = self.selections.clone();
        let (inverse, patches) = replace(&mut shared, &edits);
        shared.version += 1;
        let after = after.unwrap_or_else(|| before.transformed(&edits));
        self.selections = clamp_selections(&shared.store, after.clone());

        let (state_before, state_after) = {
            let state_before = shared.state;
            shared.state = shared.next_state;
            shared.next_state += 1;
            (state_before, shared.state)
        };
        let step = Step {
            edits: edits.clone(),
            inverse,
            eol: patches,
            state_before,
            state_after,
        };
        shared.history.record(step, before, after, source, kind);
        let change = Change { steps: vec![edits] };
        shared.record_change(change.clone());
        self.seen = shared.serial;
        change
    }

    /// Takes the changes another view of the text made since the last call into the carets
    /// of this view. The text itself is shared, so it is already here.
    pub fn follow(&mut self) -> Followed {
        let shared = self.shared.lock().unwrap();
        if shared.serial == self.seen {
            return Followed::Nothing;
        }
        let serial = shared.serial;
        let missed = shared
            .changes
            .front()
            .is_some_and(|first| first.serial > self.seen + 1);
        if missed {
            // Nobody reported the changes to this view for longer than the list holds:
            // the carets go back into the text and the layers above drop their ranges.
            self.selections = clamp_selections(&shared.store, self.selections.clone());
            self.seen = serial;
            return Followed::Missed;
        }
        let mut steps = Vec::new();
        for logged in shared
            .changes
            .iter()
            .filter(|logged| logged.serial > self.seen)
        {
            steps.extend(logged.change.steps.iter().cloned());
        }
        for step in &steps {
            self.selections = self.selections.transformed(step);
        }
        self.selections = clamp_selections(&shared.store, self.selections.clone());
        self.seen = serial;
        Followed::Changes(Change { steps })
    }

    /// A view edits the document only after the layers above have taken the changes of
    /// the other views into its carets and folds. Silently following here would hide
    /// those changes from the view, so the layers above follow first and this only checks
    /// that they did.
    fn assert_followed(&self) {
        debug_assert_eq!(
            self.shared.lock().unwrap().serial,
            self.seen,
            "a view edits the text without following the changes of the document"
        );
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
            self.shared.lock().unwrap().history.merge_last(applied);
        }
        combined
    }

    /// Joins the last `count` undo steps into one, so a late answer that belongs to an
    /// earlier insertion comes back with it.
    pub fn merge_last_undo(&mut self, count: usize) {
        self.shared.lock().unwrap().history.merge_last(count);
    }

    /// Stops grouping, so the next change starts an undo step of its own.
    pub fn seal_undo(&mut self) {
        self.shared.lock().unwrap().history.seal();
    }

    /// Reverts the latest transaction, or returns `None` when there is nothing to revert.
    pub fn undo(&mut self) -> Option<Change> {
        self.assert_followed();
        let mut shared = self.shared.lock().unwrap();
        let transaction = shared.history.pop_undo()?;
        let mut steps = Vec::with_capacity(transaction.steps.len());
        for step in transaction.steps.iter().rev() {
            replace_text(&mut shared.store, &step.inverse);
            shared.version += 1;
            for patch in step.eol.iter().rev() {
                shared.format.eol.restore(patch);
            }
            shared.state = step.state_before;
            steps.push(step.inverse.clone());
        }
        self.selections = clamp_selections(&shared.store, transaction.before.clone());
        shared.history.push_redo(transaction);
        let change = Change { steps };
        shared.record_change(change.clone());
        self.seen = shared.serial;
        Some(change)
    }

    pub fn redo(&mut self) -> Option<Change> {
        self.assert_followed();
        let mut shared = self.shared.lock().unwrap();
        let transaction = shared.history.pop_redo()?;
        let mut steps = Vec::with_capacity(transaction.steps.len());
        for step in &transaction.steps {
            let _ = replace(&mut shared, &step.edits);
            shared.version += 1;
            shared.state = step.state_after;
            steps.push(step.edits.clone());
        }
        self.selections = clamp_selections(&shared.store, transaction.after.clone());
        shared.history.push_undo(transaction);
        let change = Change { steps };
        shared.record_change(change.clone());
        self.seen = shared.serial;
        Some(change)
    }
}

/// The carets of a view, snapped to character boundaries inside `store`.
fn clamp_selections<S: TextStore>(store: &S, selections: SelectionSet) -> SelectionSet {
    let clamp = |offset: usize| {
        let mut offset = offset.min(store.len());
        while !store.is_char_boundary(offset) {
            offset -= 1;
        }
        offset
    };
    selections.map(|selection| Selection::new(clamp(selection.anchor), clamp(selection.head)))
}

/// Normalizes the text of `edits`, drops those that change nothing and checks order.
fn prepare<S: TextStore>(store: &S, edits: Vec<Edit>) -> Vec<Edit> {
    let mut prepared: Vec<Edit> = Vec::with_capacity(edits.len());
    for edit in edits {
        assert!(
            edit.range.end <= store.len(),
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
fn replace<S: TextStore>(shared: &mut Shared<S>, edits: &[Edit]) -> (Vec<Edit>, Vec<EolPatch>) {
    let mut inverse = Vec::with_capacity(edits.len());
    let mut delta = 0isize;
    for edit in edits {
        let removed = shared.store.slice(edit.range.clone());
        let start = (edit.range.start as isize + delta) as usize;
        inverse.push(Edit::new(start..start + edit.text.len(), removed));
        delta += edit.delta();
    }
    let mut eol = Vec::new();
    for (edit, undo) in edits.iter().zip(&inverse).rev() {
        let first_line = shared.store.offset_to_line(edit.range.start);
        let removed_lines = undo.text.bytes().filter(|byte| *byte == b'\n').count();
        let inserted_lines = edit.text.bytes().filter(|byte| *byte == b'\n').count();
        eol.push(
            shared
                .format
                .eol
                .splice(first_line, removed_lines, inserted_lines),
        );
    }
    replace_text(&mut shared.store, edits);
    (inverse, eol)
}

fn replace_text<S: TextStore>(store: &mut S, edits: &[Edit]) {
    for edit in edits.iter().rev() {
        store.replace(edit.range.clone(), &edit.text);
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
