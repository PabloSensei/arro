//! Carets and selections. A caret is a selection with `anchor == head`.

use std::ops::Range;

use crate::edit::Edit;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub anchor: usize,
    pub head: usize,
}

impl Selection {
    pub fn caret(offset: usize) -> Self {
        Self {
            anchor: offset,
            head: offset,
        }
    }

    pub fn new(anchor: usize, head: usize) -> Self {
        Self { anchor, head }
    }

    pub fn start(&self) -> usize {
        self.anchor.min(self.head)
    }

    pub fn end(&self) -> usize {
        self.anchor.max(self.head)
    }

    pub fn range(&self) -> Range<usize> {
        self.start()..self.end()
    }

    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }

    fn reversed(&self) -> bool {
        self.head < self.anchor
    }
}

/// All carets of a document: sorted, never overlapping and never empty. Which caret was
/// added last is kept, because scrolling and Alt+J follow it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectionSet {
    selections: Vec<Selection>,
    primary: usize,
}

impl Default for SelectionSet {
    fn default() -> Self {
        Self::single(Selection::caret(0))
    }
}

impl SelectionSet {
    pub fn single(selection: Selection) -> Self {
        Self {
            selections: vec![selection],
            primary: 0,
        }
    }

    /// Builds a set from any list; the last element becomes the primary caret.
    pub fn from_vec(selections: Vec<Selection>) -> Self {
        assert!(!selections.is_empty(), "a document always has a caret");
        let primary = selections[selections.len() - 1];
        let mut set = Self {
            selections,
            primary: 0,
        };
        set.normalize(primary);
        set
    }

    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &Selection> {
        self.selections.iter()
    }

    pub fn as_slice(&self) -> &[Selection] {
        &self.selections
    }

    pub fn len(&self) -> usize {
        self.selections.len()
    }

    pub fn is_empty(&self) -> bool {
        false
    }

    pub fn primary(&self) -> Selection {
        self.selections[self.primary]
    }

    pub fn add(&mut self, selection: Selection) {
        self.selections.push(selection);
        self.normalize(selection);
    }

    /// Applies `f` to every selection, then merges what now overlaps.
    pub fn map(&self, mut f: impl FnMut(&Selection) -> Selection) -> Self {
        let primary = f(&self.selections[self.primary]);
        let mut set = Self {
            selections: self.selections.iter().map(f).collect(),
            primary: 0,
        };
        set.normalize(primary);
        set
    }

    /// Where the selections end up after `edits`. A caret inside a replaced range moves
    /// to the end of the new text.
    pub fn transformed(&self, edits: &[Edit]) -> Self {
        self.map(|selection| Selection {
            anchor: transform_offset(selection.anchor, edits),
            head: transform_offset(selection.head, edits),
        })
    }

    fn normalize(&mut self, primary: Selection) {
        self.selections
            .sort_by_key(|selection| (selection.start(), selection.end()));
        let mut merged: Vec<Selection> = Vec::with_capacity(self.selections.len());
        let mut primary_index = 0;
        for selection in self.selections.drain(..) {
            match merged.last_mut() {
                Some(last) if overlaps(last, &selection) => {
                    let (start, end) = (last.start(), last.end().max(selection.end()));
                    *last = if selection.reversed() {
                        Selection::new(end, start)
                    } else {
                        Selection::new(start, end)
                    };
                }
                _ => merged.push(selection),
            }
            let last = merged[merged.len() - 1];
            if last.start() <= primary.start() && primary.end() <= last.end() {
                primary_index = merged.len() - 1;
            }
        }
        self.selections = merged;
        self.primary = primary_index;
    }
}

fn overlaps(left: &Selection, right: &Selection) -> bool {
    right.start() < left.end()
        || right.start() == left.start()
        || (left.is_empty() && right.start() == left.end())
}

/// Where `offset` is after `edits`, which refer to the text before them.
pub fn transform_offset(offset: usize, edits: &[Edit]) -> usize {
    let mut delta = 0isize;
    for edit in edits {
        if edit.range.end <= offset {
            delta += edit.delta();
        } else if edit.range.start < offset {
            return (edit.range.start as isize + delta) as usize + edit.text.len();
        } else {
            break;
        }
    }
    (offset as isize + delta) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlapping_selections_merge_and_keep_the_primary() {
        let set = SelectionSet::from_vec(vec![
            Selection::new(5, 10),
            Selection::new(0, 2),
            Selection::new(8, 12),
        ]);
        assert_eq!(
            set.as_slice(),
            &[Selection::new(0, 2), Selection::new(5, 12)]
        );
        assert_eq!(set.primary(), Selection::new(5, 12));
    }

    #[test]
    fn adjacent_selections_stay_apart() {
        let set = SelectionSet::from_vec(vec![Selection::new(0, 2), Selection::new(2, 4)]);
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn carets_follow_an_insertion_before_them() {
        let set = SelectionSet::single(Selection::caret(4));
        let moved = set.transformed(&[Edit::insert(4, "ab"), Edit::insert(9, "c")]);
        assert_eq!(moved.primary(), Selection::caret(6));
    }

    #[test]
    fn a_caret_inside_a_replaced_range_lands_after_the_new_text() {
        let set = SelectionSet::single(Selection::caret(3));
        let moved = set.transformed(&[Edit::new(1..5, "xy")]);
        assert_eq!(moved.primary(), Selection::caret(3));
    }
}
