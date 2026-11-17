//! Linear undo with grouping of fast typing and a limit on memory.

use std::time::{Duration, Instant};

use crate::edit::{Edit, EditKind, EditSource};
use crate::encoding::EolPatch;
use crate::selection::SelectionSet;

const GROUP_WINDOW: Duration = Duration::from_millis(300);
const MEMORY_LIMIT: usize = 256 * 1024 * 1024;
/// Rough cost of one edit besides its text.
const EDIT_OVERHEAD: usize = 64;

/// One change and what reverts it.
#[derive(Clone)]
pub(crate) struct Step {
    pub edits: Vec<Edit>,
    pub inverse: Vec<Edit>,
    /// The line ends that `edits` replaced, in the order they were taken.
    pub eol: Vec<EolPatch>,
    /// The state of the document before and after, compared with the saved state.
    pub state_before: u64,
    pub state_after: u64,
}

/// What Ctrl+Z reverts in one press.
pub(crate) struct Transaction {
    pub steps: Vec<Step>,
    pub before: SelectionSet,
    pub after: SelectionSet,
    pub source: EditSource,
    kind: EditKind,
    at: Instant,
    bytes: usize,
}

fn cost(step: &Step) -> usize {
    let edits = step.edits.iter().chain(&step.inverse);
    edits.map(|edit| edit.text.len() + EDIT_OVERHEAD).sum()
}

#[derive(Default)]
pub struct History {
    undo: Vec<Transaction>,
    redo: Vec<Transaction>,
    bytes: usize,
}

impl History {
    pub(crate) fn record(
        &mut self,
        step: Step,
        before: SelectionSet,
        after: SelectionSet,
        source: EditSource,
        kind: EditKind,
    ) {
        self.redo.clear();
        let now = Instant::now();
        let bytes = cost(&step);
        self.bytes += bytes;
        match self.undo.last_mut() {
            Some(last)
                if kind == EditKind::Typing
                    && last.kind == EditKind::Typing
                    && last.source == source
                    && now.duration_since(last.at) < GROUP_WINDOW =>
            {
                last.steps.push(step);
                last.after = after;
                last.at = now;
                last.bytes += bytes;
            }
            _ => self.undo.push(Transaction {
                steps: vec![step],
                before,
                after,
                source,
                kind,
                at: now,
                bytes,
            }),
        }
        while self.bytes > MEMORY_LIMIT && self.undo.len() > 1 {
            self.bytes -= self.undo.remove(0).bytes;
        }
    }

    /// Joins the last `count` transactions into one, for answers that arrive as several
    /// changes but must undo in one press. Their steps stay in the order they were made.
    pub(crate) fn merge_last(&mut self, count: usize) {
        if count < 2 || self.undo.len() < count {
            return;
        }
        let start = self.undo.len() - count;
        let merged = self.undo.split_off(start);
        let mut merged = merged.into_iter();
        let Some(mut first) = merged.next() else {
            return;
        };
        for next in merged {
            first.steps.extend(next.steps);
            first.after = next.after;
            first.kind = next.kind;
            first.at = next.at;
            first.bytes += next.bytes;
        }
        self.undo.push(first);
    }

    /// Stops grouping, so the next change starts a new undo step.
    pub fn seal(&mut self) {
        if let Some(last) = self.undo.last_mut() {
            last.kind = EditKind::Command;
        }
    }

    pub(crate) fn pop_undo(&mut self) -> Option<Transaction> {
        let transaction = self.undo.pop()?;
        self.bytes -= transaction.bytes;
        Some(transaction)
    }

    pub(crate) fn push_redo(&mut self, transaction: Transaction) {
        self.bytes += transaction.bytes;
        self.redo.push(transaction);
    }

    pub(crate) fn pop_redo(&mut self) -> Option<Transaction> {
        let transaction = self.redo.pop()?;
        self.bytes -= transaction.bytes;
        Some(transaction)
    }

    pub(crate) fn push_undo(&mut self, transaction: Transaction) {
        self.bytes += transaction.bytes;
        self.undo.push(transaction);
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
}
