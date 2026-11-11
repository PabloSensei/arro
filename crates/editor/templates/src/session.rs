//! A template being filled in: its fields in the document, kept in place across edits.

use std::ops::Range;

use text_core::{Change, Edit};

use crate::expand::{Expansion, Field};

/// Where Tab or Shift+Tab goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// Select this field.
    Select(Range<usize>),
    /// The last field is done: the caret goes here and the session ends.
    Finish(usize),
}

pub struct Session {
    fields: Vec<Field>,
    current: usize,
    end: usize,
}

fn shifted(range: &Range<usize>, by: usize) -> Range<usize> {
    range.start + by..range.end + by
}

impl Session {
    /// Starts on an expansion inserted at `at`. `None` when it has no fields.
    pub fn start(expansion: &Expansion, at: usize) -> Option<Self> {
        if expansion.fields.is_empty() {
            return None;
        }
        let fields = expansion.fields.iter().map(|field| Field {
            name: field.name.clone(),
            ranges: field
                .ranges
                .iter()
                .map(|range| shifted(range, at))
                .collect(),
        });
        Some(Self {
            fields: fields.collect(),
            current: 0,
            end: expansion.end + at,
        })
    }

    /// The range the user types in now.
    pub fn current(&self) -> Range<usize> {
        self.fields[self.current].ranges[0].clone()
    }

    /// Every field range, for drawing.
    pub fn ranges(&self) -> impl Iterator<Item = &Range<usize>> {
        self.fields.iter().flat_map(|field| field.ranges.iter())
    }

    /// `offset` is in one of the fields or at the end.
    pub fn contains(&self, offset: usize) -> bool {
        offset == self.end
            || self
                .ranges()
                .any(|range| range.start <= offset && offset <= range.end)
    }

    pub fn advance(&mut self) -> Step {
        if self.current + 1 < self.fields.len() {
            self.current += 1;
            Step::Select(self.current())
        } else {
            Step::Finish(self.end)
        }
    }

    pub fn back(&mut self) -> Step {
        self.current = self.current.saturating_sub(1);
        Step::Select(self.current())
    }

    /// Follows a change of the document. `false` means an edit landed outside the fields
    /// and the session is over.
    pub fn follow(&mut self, change: &Change) -> bool {
        for step in &change.steps {
            // Later edits do not move earlier offsets, so going backwards keeps every edit's
            // range valid against the ranges updated so far.
            for edit in step.iter().rev() {
                if !self.follow_edit(edit) {
                    return false;
                }
            }
        }
        true
    }

    fn follow_edit(&mut self, edit: &Edit) -> bool {
        let range = &edit.range;
        let inside = |field: &Range<usize>| field.start <= range.start && range.end <= field.end;
        let current = self.current;
        let owner = self.fields[current]
            .ranges
            .iter()
            .position(inside)
            .map(|index| (current, index))
            .or_else(|| {
                self.fields.iter().enumerate().find_map(|(field, known)| {
                    let index = known.ranges.iter().position(inside)?;
                    Some((field, index))
                })
            });
        let Some(owner) = owner else {
            return false;
        };
        let grows = edit.text.len() as isize - range.len() as isize;
        let moved = |offset: usize| (offset as isize + grows) as usize;
        for (field_index, field) in self.fields.iter_mut().enumerate() {
            for (index, known) in field.ranges.iter_mut().enumerate() {
                if (field_index, index) == owner {
                    known.end = moved(known.end);
                } else if range.end <= known.start {
                    *known = moved(known.start)..moved(known.end);
                } else if range.start < known.end {
                    return false;
                }
            }
        }
        if range.end <= self.end {
            self.end = moved(self.end);
        }
        true
    }

    /// Follows a change that may land outside the fields, such as an import line the
    /// server adds after the insertion. The fields move when the change is before them
    /// and the session stays open.
    pub fn follow_around(&mut self, change: &Change) {
        for step in &change.steps {
            for edit in step.iter().rev() {
                let range = &edit.range;
                let grows = edit.text.len() as isize - range.len() as isize;
                let moved = |offset: usize| (offset as isize + grows) as usize;
                for field in &mut self.fields {
                    for known in &mut field.ranges {
                        if range.end <= known.start {
                            *known = moved(known.start)..moved(known.end);
                        }
                    }
                }
                if range.end <= self.end {
                    self.end = moved(self.end);
                }
            }
        }
    }

    /// Edits that make every copy of a field show what its first range holds.
    pub fn mirror_edits(&self, slice: impl Fn(Range<usize>) -> String) -> Vec<Edit> {
        let mut edits = Vec::new();
        for field in &self.fields {
            let typed = slice(field.ranges[0].clone());
            for copy in &field.ranges[1..] {
                if slice(copy.clone()) != typed {
                    edits.push(Edit::new(copy.clone(), typed.clone()));
                }
            }
        }
        edits.sort_by_key(|edit| edit.range.start);
        edits
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expand::expand;
    use crate::template::{ANY, Kind, Template};

    const FORI: Template = Template {
        key: "fori",
        description: "",
        kind: Kind::Abbreviation,
        body: "for (int $I$ = 0; $I$ < $N$; $I$++) {$END$}",
        fields: &[("I", "i")],
        contexts: &[ANY],
    };

    /// Applies edits to `text` the way the buffer does and tells the session.
    fn apply(text: &mut String, session: &mut Session, edits: Vec<Edit>) -> bool {
        for edit in edits.iter().rev() {
            text.replace_range(edit.range.clone(), &edit.text);
        }
        session.follow(&Change { steps: vec![edits] })
    }

    #[test]
    fn typing_in_a_field_updates_its_copies_and_tab_walks_to_the_end() {
        let expansion = expand(&FORI, "", "    ", None, None);
        let mut text = format!("x;{}", expansion.text);
        let mut session = Session::start(&expansion, 2).unwrap();
        let first = session.current();
        assert_eq!(&text[first.clone()], "i");

        assert!(apply(
            &mut text,
            &mut session,
            vec![Edit::new(first, "index")]
        ));
        let mirrors = session.mirror_edits(|range| text[range].to_string());
        assert!(apply(&mut text, &mut session, mirrors));
        assert_eq!(text, "x;for (int index = 0; index < ; index++) {}");

        let Step::Select(limit) = session.advance() else {
            panic!("a second field");
        };
        assert!(apply(
            &mut text,
            &mut session,
            vec![Edit::insert(limit.start, "10")]
        ));
        assert_eq!(text, "x;for (int index = 0; index < 10; index++) {}");
        assert_eq!(session.advance(), Step::Finish(text.len() - 1));
        let Step::Select(back) = session.back() else {
            panic!("back to the first field");
        };
        assert_eq!(&text[back], "index");
    }

    #[test]
    fn an_edit_outside_the_fields_ends_the_session() {
        let expansion = expand(&FORI, "", "    ", None, None);
        let mut text = format!("x;{}", expansion.text);
        let mut session = Session::start(&expansion, 2).unwrap();

        assert!(!apply(&mut text, &mut session, vec![Edit::insert(0, "y")]));
    }
}
