//! Rename in place: Shift+F6 changes the name under the caret in every place of the
//! file as the user types. Escape puts the original name back; Enter asks the language
//! for the rename that reaches the other files.

use std::ops::Range;

use text_core::{Change, Edit, EditKind, Selection, SelectionSet};

use crate::actions::word_ranges;
use crate::semantic::is_word;
use crate::state::EditorState;

/// The name being typed and the places it stands in, in one file.
pub(crate) struct RenameInPlace {
    /// The name before the first typed character: Escape puts it back.
    pub(crate) original: String,
    /// The name as it stands now.
    pub(crate) text: String,
    /// Every occurrence of the name, with the main one at `main`.
    pub(crate) spans: Vec<Range<usize>>,
    /// The occurrence the caret started on and the popup points at.
    pub(crate) main: usize,
    /// Why the typed name cannot be applied, when it cannot.
    pub(crate) problem: Option<&'static str>,
    /// The text version the spans agree with.
    version: u64,
    /// The first typed character replaces the whole name, which starts selected.
    selected: bool,
}

fn problem_for(name: &str, reserved: bool) -> Option<&'static str> {
    if name.is_empty() {
        Some("A name cannot be empty")
    } else if name
        .chars()
        .next()
        .is_some_and(|character| character.is_ascii_digit())
    {
        Some("A name cannot start with a digit")
    } else if !name.chars().all(is_word) {
        Some("A name can use only letters, digits, '_' and '$'")
    } else if reserved {
        Some("A reserved word cannot be a name")
    } else {
        None
    }
}

impl RenameInPlace {
    /// The name at `word` and every whole-word occurrence of it in the text.
    pub(crate) fn new(state: &EditorState, word: Range<usize>) -> Option<Self> {
        let text = state.text();
        let name = text.get(word.clone())?;
        if name.is_empty() || !name.chars().next().is_some_and(is_word) {
            return None;
        }
        let mut spans = word_ranges(&text, name);
        let mut main = spans
            .iter()
            .position(|span| span.start == word.start && span.end == word.end);
        if main.is_none() {
            spans.push(word.clone());
            spans.sort_by_key(|span| span.start);
            main = spans.iter().position(|span| *span == word);
        }
        let main = main?;
        Some(Self {
            original: name.to_owned(),
            text: name.to_owned(),
            spans,
            main,
            problem: None,
            version: state.version(),
            selected: true,
        })
    }

    /// The offset the popup and the request point at.
    pub(crate) fn main_start(&self) -> usize {
        self.spans[self.main].start
    }

    /// The main occurrence as it stands now.
    pub(crate) fn main_range(&self) -> Range<usize> {
        self.spans[self.main].clone()
    }

    /// Checks the name against the words the language reserves.
    pub(crate) fn refresh_problem(&mut self, reserved: bool) {
        self.problem = problem_for(&self.text, reserved);
    }

    /// Replaces every occurrence with what the user typed, as one step of undo.
    pub(crate) fn type_text(
        &mut self,
        state: &mut EditorState,
        typed: &str,
        reserved: bool,
    ) -> Option<Change> {
        let text = if self.selected {
            typed.to_owned()
        } else {
            format!("{}{typed}", self.text)
        };
        self.write(state, &text, reserved)
    }

    /// Deletes the last typed character from every occurrence.
    pub(crate) fn backspace(&mut self, state: &mut EditorState, reserved: bool) -> Option<Change> {
        if self.selected {
            return self.write(state, "", reserved);
        }
        let mut text = self.text.clone();
        text.pop();
        self.write(state, &text, reserved)
    }

    /// Escape: the original name comes back into every occurrence.
    pub(crate) fn cancel(&mut self, state: &mut EditorState) -> Option<Change> {
        if self.text == self.original {
            self.selected = false;
            return None;
        }
        let original = self.original.clone();
        let change = self.replace(state, &original, false);
        self.selected = false;
        change
    }

    /// The edits that put the original name back, for the request that reaches the other
    /// files: the text the language renames from is the text before the typing.
    pub(crate) fn revert_edits(&self) -> Vec<Edit> {
        self.spans
            .iter()
            .map(|span| Edit::new(span.clone(), &self.original))
            .collect()
    }

    /// The selection after `revert_edits`: at the end of the main occurrence.
    pub(crate) fn selection_after_revert(&self) -> SelectionSet {
        let at = self.main_after(self.original.len());
        SelectionSet::single(Selection::caret(at))
    }

    /// Where the main occurrence starts once the original name is back.
    pub(crate) fn main_start_after_revert(&self) -> usize {
        self.main_after(self.original.len()) - self.original.len()
    }

    /// Moves the spans over a change made elsewhere, such as Ctrl+Z, and reads the name
    /// as it stands after it.
    pub(crate) fn follow(&mut self, steps: &[Vec<Edit>], text: &str, version: u64, reserved: bool) {
        if self.version == version {
            return;
        }
        let len = text.len();
        for step in steps {
            for span in &mut self.spans {
                span.start = text_core::transform_offset(span.start, step).min(len);
                span.end = text_core::transform_offset(span.end, step).min(len);
                if span.end < span.start {
                    span.end = span.start;
                }
            }
        }
        self.main = self.main.min(self.spans.len().saturating_sub(1));
        let span = &mut self.spans[self.main];
        span.start = boundary_at_or_after(text, span.start);
        span.end = boundary_at_or_after(text, span.end).max(span.start);
        self.text = text[span.start..span.end].to_owned();
        self.problem = problem_for(&self.text, reserved);
        self.version = version;
    }

    fn write(&mut self, state: &mut EditorState, text: &str, reserved: bool) -> Option<Change> {
        let change = self.replace(state, text, reserved);
        self.selected = false;
        change
    }

    /// Replaces every occurrence with `text`. Each step of undo is one typed character.
    fn replace(&mut self, state: &mut EditorState, text: &str, reserved: bool) -> Option<Change> {
        let after = SelectionSet::single(Selection::caret(self.main_after(text.len())));
        let edits: Vec<Edit> = self
            .spans
            .iter()
            .map(|span| Edit::new(span.clone(), text))
            .collect();
        let change = state.apply_template_edits(edits, after, EditKind::Command)?;
        let current = state.text();
        self.follow(&change.steps, &current, state.version(), reserved);
        Some(change)
    }

    /// Where the end of the main occurrence lands after replacing every occurrence with
    /// a name of `len` bytes.
    fn main_after(&self, len: usize) -> usize {
        let mut shift = 0isize;
        for span in &self.spans[..self.main] {
            shift += len as isize - (span.end - span.start) as isize;
        }
        (self.spans[self.main].start as isize + shift).max(0) as usize + len
    }
}

fn boundary_at_or_after(text: &str, at: usize) -> usize {
    let mut at = at.min(text.len());
    while at < text.len() && !text.is_char_boundary(at) {
        at += 1;
    }
    at
}

#[cfg(test)]
mod tests {
    use keymap::editor as actions;
    use text_core::FileFormat;

    use super::RenameInPlace;
    use crate::state::EditorState;

    fn state(text: &str) -> EditorState {
        EditorState::new(text, FileFormat::default(), None, false)
    }

    fn rename(state: &EditorState, word: std::ops::Range<usize>) -> RenameInPlace {
        RenameInPlace::new(state, word).expect("the word is a name")
    }

    fn type_in(rename: &mut RenameInPlace, state: &mut EditorState, text: &str) {
        for character in text.chars() {
            rename
                .type_text(state, &character.to_string(), false)
                .unwrap_or_else(|| panic!("the text took {character}"));
        }
    }

    #[test]
    fn typing_changes_every_occurrence_at_once() {
        let mut state = state("alpha beta alpha\n");
        let mut rename = rename(&state, 0..5);

        type_in(&mut rename, &mut state, "d");
        assert_eq!(state.text(), "d beta d\n");

        type_in(&mut rename, &mut state, "elta");
        assert_eq!(state.text(), "delta beta delta\n");
        assert_eq!(rename.text, "delta");
    }

    #[test]
    fn escape_puts_the_original_name_back() {
        let mut state = state("alpha beta alpha\n");
        let mut rename = rename(&state, 0..5);
        type_in(&mut rename, &mut state, "delta");

        rename.cancel(&mut state);

        assert_eq!(state.text(), "alpha beta alpha\n");
    }

    #[test]
    fn ctrl_z_takes_one_typed_character_at_a_time() {
        let mut state = state("alpha beta alpha\n");
        let mut rename = rename(&state, 0..5);
        type_in(&mut rename, &mut state, "de");
        assert_eq!(state.text(), "de beta de\n");

        let change = state.perform(actions::UNDO, 1).expect("a step of undo");
        rename.follow(&change.steps, &state.text(), state.version(), false);

        assert_eq!(state.text(), "d beta d\n");
        assert_eq!(rename.text, "d");

        let change = state.perform(actions::UNDO, 1).expect("a step of undo");
        rename.follow(&change.steps, &state.text(), state.version(), false);
        assert_eq!(state.text(), "alpha beta alpha\n");
    }

    #[test]
    fn an_empty_or_broken_name_is_not_applied() {
        let mut state = state("alpha beta\n");
        let mut rename = rename(&state, 0..5);

        rename
            .type_text(&mut state, "1x", false)
            .expect("the text still changes as the user types");
        assert!(rename.problem.is_some());
        assert_eq!(state.text(), "1x beta\n");

        rename.cancel(&mut state);
        assert_eq!(state.text(), "alpha beta\n");
    }

    #[test]
    fn an_empty_name_is_a_problem_and_escape_restores_the_original() {
        let mut state = state("alpha\n");
        let mut rename = rename(&state, 0..5);

        rename.backspace(&mut state, false);
        assert_eq!(state.text(), "\n");
        assert_eq!(rename.problem, Some("A name cannot be empty"));

        rename.cancel(&mut state);
        assert_eq!(state.text(), "alpha\n");
    }

    #[test]
    fn a_reserved_word_is_a_problem_of_the_language() {
        assert_eq!(
            super::problem_for("value", true),
            Some("A reserved word cannot be a name")
        );
        assert_eq!(super::problem_for("value", false), None);
    }
}
