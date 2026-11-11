//! Collapsed ranges of the buffer. The list stays sorted and without nesting; a fold
//! inside a collapsed one is not kept.

use std::ops::Range;

use text_core::{Edit, TextStore};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fold {
    pub range: Range<usize>,
    pub placeholder: String,
}

#[derive(Clone, Debug, Default)]
pub struct FoldMap {
    /// Ranges that can fold, from the parser. Sorted by start.
    candidates: Vec<Fold>,
    collapsed: Vec<Fold>,
}

impl FoldMap {
    pub fn as_slice(&self) -> &[Fold] {
        &self.collapsed
    }

    pub fn candidates(&self) -> &[Fold] {
        &self.candidates
    }

    pub fn set_candidates(&mut self, mut candidates: Vec<Fold>) {
        candidates.sort_by_key(|fold| (fold.range.start, std::cmp::Reverse(fold.range.end)));
        self.candidates = candidates;
    }

    pub fn collapse(&mut self, fold: Fold) {
        self.collapsed
            .retain(|old| old.range.end <= fold.range.start || old.range.start >= fold.range.end);
        let at = self
            .collapsed
            .partition_point(|old| old.range.start < fold.range.start);
        self.collapsed.insert(at, fold);
    }

    /// Removes the folds that hide or touch `offset`. Returns whether there were any.
    pub fn unfold_at(&mut self, offset: usize) -> bool {
        let before = self.collapsed.len();
        self.collapsed
            .retain(|fold| offset < fold.range.start || offset > fold.range.end);
        before != self.collapsed.len()
    }

    pub fn clear(&mut self) {
        self.collapsed.clear();
    }

    /// The candidate that holds `offset` and starts first: collapsing it hides every
    /// candidate inside it too, which is what a recursive collapse means.
    pub fn collapse_recursive(&mut self, offset: usize) -> bool {
        let found = self
            .candidates
            .iter()
            .filter(|fold| fold.range.start <= offset && offset <= fold.range.end)
            .min_by_key(|fold| (fold.range.start, std::cmp::Reverse(fold.range.end)))
            .cloned();
        match found {
            Some(fold) => {
                self.collapse(fold);
                true
            }
            None => false,
        }
    }

    /// Expands every collapsed fold that holds `offset`, the nested ones included.
    pub fn expand_recursive(&mut self, offset: usize) -> bool {
        let before = self.collapsed.len();
        self.collapsed
            .retain(|fold| offset < fold.range.start || offset > fold.range.end);
        before != self.collapsed.len()
    }

    /// The nesting depth of a candidate, from one for an outermost one.
    pub fn depth_of(&self, fold: &Fold) -> usize {
        self.candidates
            .iter()
            .filter(|other| {
                other.range != fold.range
                    && other.range.start <= fold.range.start
                    && fold.range.end <= other.range.end
            })
            .count()
            + 1
    }

    /// Keeps the folds down to `level` open and collapses the next level. With a
    /// `scope`, only the candidates inside it take part.
    pub fn expand_to_level(&mut self, level: usize, scope: Option<Range<usize>>) -> bool {
        let level = level.max(1);
        let in_scope = |fold: &Fold| match &scope {
            Some(scope) => scope.start <= fold.range.start && fold.range.end <= scope.end,
            None => true,
        };
        let opened: Vec<Range<usize>> = self
            .candidates
            .iter()
            .filter(|fold| in_scope(fold) && self.depth_of(fold) <= level)
            .map(|fold| fold.range.clone())
            .collect();
        let mut changed = false;
        let before = self.collapsed.len();
        self.collapsed.retain(|fold| !opened.contains(&fold.range));
        changed |= before != self.collapsed.len();
        let next: Vec<Fold> = self
            .candidates
            .iter()
            .filter(|fold| in_scope(fold) && self.depth_of(fold) == level + 1)
            .cloned()
            .collect();
        for fold in next {
            let before = self.collapsed.len();
            self.collapse(fold);
            changed |= before != self.collapsed.len();
        }
        changed
    }

    /// The innermost candidate that starts on `line`, the largest when several do.
    pub fn candidate_on_line<S: TextStore>(&self, store: &S, line: usize) -> Option<&Fold> {
        let start = store.line_to_offset(line);
        let end = store.line_end(line);
        let first = self
            .candidates
            .partition_point(|fold| fold.range.start < start);
        self.candidates[first..]
            .iter()
            .take_while(|fold| fold.range.start <= end)
            .find(|fold| fold.range.end > end)
    }

    /// Collapses the fold that starts on `line`, or expands the one already collapsed.
    pub fn toggle_at_line<S: TextStore>(&mut self, store: &S, line: usize) -> bool {
        let start = store.line_to_offset(line);
        let end = store.line_end(line);
        let found = self
            .collapsed
            .iter()
            .position(|fold| fold.range.start >= start && fold.range.start <= end);
        if let Some(index) = found {
            self.collapsed.remove(index);
            return true;
        }
        match self.candidate_on_line(store, line).cloned() {
            Some(fold) => {
                self.collapse(fold);
                true
            }
            None => false,
        }
    }

    /// An edit that touches a collapsed fold expands it, the others move with the text.
    pub fn apply_edits(&mut self, edits: &[Edit]) {
        for edit in edits.iter().rev() {
            self.collapsed.retain(|fold| {
                edit.range.end < fold.range.start || edit.range.start > fold.range.end
            });
            shift(&mut self.collapsed, edit);
            self.candidates.retain(|fold| {
                edit.range.end <= fold.range.start
                    || edit.range.start >= fold.range.end
                    || contains(fold, edit)
            });
            shift(&mut self.candidates, edit);
        }
    }

    /// First line of each row: lines inside a collapsed fold have no row of their own.
    pub(crate) fn visible_lines<S: TextStore>(&self, store: &S) -> Vec<u32> {
        let mut visible = Vec::with_capacity(store.line_count());
        let mut folds = self.collapsed.iter().peekable();
        let mut line = 0;
        while line < store.line_count() {
            visible.push(line as u32);
            let mut next = line + 1;
            let mut end = store.line_end(line);
            while let Some(fold) = folds.peek() {
                if fold.range.start > end {
                    break;
                }
                if fold.range.end > end {
                    next = store.offset_to_line(fold.range.end) + 1;
                    end = store.line_end(next - 1);
                }
                folds.next();
            }
            line = next;
        }
        visible
    }
}

fn contains(fold: &Fold, edit: &Edit) -> bool {
    fold.range.start < edit.range.start && edit.range.end < fold.range.end
}

fn shift(folds: &mut [Fold], edit: &Edit) {
    let delta = edit.text.len() as isize - edit.range.len() as isize;
    let moved = |offset: usize| offset.checked_add_signed(delta).unwrap_or(0);
    for fold in folds {
        if fold.range.start >= edit.range.end {
            fold.range.start = moved(fold.range.start);
        }
        if fold.range.end >= edit.range.end {
            fold.range.end = moved(fold.range.end);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Fold, FoldMap};

    fn fold(start: usize, end: usize) -> Fold {
        Fold {
            range: start..end,
            placeholder: "...".into(),
        }
    }

    fn nested() -> FoldMap {
        let mut map = FoldMap::default();
        map.set_candidates(vec![
            fold(0, 100),
            fold(10, 40),
            fold(20, 30),
            fold(50, 90),
            fold(60, 80),
        ]);
        map
    }

    #[test]
    fn a_recursive_collapse_keeps_the_outermost_region_only() {
        let mut map = nested();
        assert!(map.collapse_recursive(25));
        assert_eq!(map.as_slice(), &[fold(0, 100)]);
        assert!(map.expand_recursive(25));
        assert!(map.as_slice().is_empty());
    }

    #[test]
    fn a_recursive_expand_opens_every_fold_around_the_caret() {
        let mut map = nested();
        map.collapse(fold(10, 40));
        map.collapse(fold(50, 90));
        assert!(map.expand_recursive(20));
        assert_eq!(map.as_slice(), &[fold(50, 90)]);
    }

    #[test]
    fn a_level_opens_the_folds_above_it_and_collapses_the_next_one() {
        let mut map = nested();
        assert!(map.expand_to_level(1, None));
        assert_eq!(map.as_slice(), &[fold(10, 40), fold(50, 90)]);
        assert!(map.expand_to_level(2, None));
        assert_eq!(
            map.as_slice(),
            &[fold(20, 30), fold(60, 80)],
            "the third level is the one that folds now"
        );
        map.expand_to_level(5, None);
        assert!(
            map.as_slice().is_empty(),
            "a level deeper than the folds opens all"
        );
    }

    #[test]
    fn a_level_scope_leaves_the_other_regions_alone() {
        let mut map = nested();
        map.collapse(fold(50, 90));
        map.expand_to_level(1, Some(0..100));
        assert_eq!(
            map.as_slice(),
            &[fold(10, 40), fold(50, 90)],
            "the top level opens and the next one folds"
        );
        map.expand_to_level(2, Some(10..40));
        assert_eq!(
            map.as_slice(),
            &[fold(20, 30), fold(50, 90)],
            "only the folds of the scope change"
        );
    }
}
