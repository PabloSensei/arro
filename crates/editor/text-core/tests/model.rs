//! `RopeStore` and `Buffer` against a plain `String`.

use std::ops::Range;

use proptest::prelude::*;
use text_core::commands;
use text_core::{
    Buffer, Edit, EditKind, EditSource, FileFormat, RopeStore, Selection, SelectionSet, TextStore,
};

/// The reference: a `String` with the same contract as the rope.
#[derive(Clone, Default)]
struct StringStore(String);

impl TextStore for StringStore {
    fn from_text(text: &str) -> Self {
        Self(text.to_owned())
    }

    fn len(&self) -> usize {
        self.0.len()
    }

    fn line_count(&self) -> usize {
        self.0.bytes().filter(|byte| *byte == b'\n').count() + 1
    }

    fn line_to_offset(&self, line: usize) -> usize {
        if line == 0 {
            return 0;
        }
        match self.0.match_indices('\n').nth(line - 1) {
            Some((at, _)) => at + 1,
            None => self.0.len(),
        }
    }

    fn offset_to_line(&self, offset: usize) -> usize {
        self.0.as_bytes()[..offset]
            .iter()
            .filter(|byte| **byte == b'\n')
            .count()
    }

    fn is_char_boundary(&self, offset: usize) -> bool {
        self.0.is_char_boundary(offset)
    }

    fn slice(&self, range: Range<usize>) -> String {
        self.0[range].to_owned()
    }

    fn replace(&mut self, range: Range<usize>, text: &str) {
        self.0.replace_range(range, text);
    }

    fn chunk_at(&self, _: usize) -> (&str, usize) {
        (&self.0, 0)
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        self.0[..offset].encode_utf16().count()
    }

    fn utf16_to_offset(&self, utf16: usize) -> usize {
        let mut units = 0;
        for (at, character) in self.0.char_indices() {
            if units >= utf16 {
                return at;
            }
            units += character.len_utf16();
        }
        self.0.len()
    }
}

#[derive(Clone, Debug)]
enum Op {
    Replace {
        at: usize,
        length: usize,
        text: String,
    },
    Undo,
    Redo,
    Caret(usize),
    AddCaret(usize),
    Type(String),
    Backspace,
    Delete,
    Duplicate,
    DeleteLines,
    MoveLines(bool),
    Comment,
    NextOccurrence,
}

fn op() -> impl Strategy<Value = Op> {
    let text = "[a-cж\n ]{0,6}";
    prop_oneof![
        (any::<usize>(), 0..12usize, text).prop_map(|(at, length, text)| Op::Replace {
            at,
            length,
            text
        }),
        Just(Op::Undo),
        Just(Op::Redo),
        any::<usize>().prop_map(Op::Caret),
        any::<usize>().prop_map(Op::AddCaret),
        text.prop_map(Op::Type),
        Just(Op::Backspace),
        Just(Op::Delete),
        Just(Op::Duplicate),
        Just(Op::DeleteLines),
        any::<bool>().prop_map(Op::MoveLines),
        Just(Op::Comment),
        Just(Op::NextOccurrence),
    ]
}

fn boundary(text: &str, offset: usize) -> usize {
    let mut offset = offset % (text.len() + 1);
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

fn run<S: TextStore>(buffer: &mut Buffer<S>, op: &Op) {
    let text = buffer.text();
    match op {
        Op::Replace {
            at,
            length,
            text: inserted,
        } => {
            let start = boundary(&text, *at);
            let end = boundary(&text, (start + length).min(text.len()));
            let edit = Edit::new(start..end.max(start), inserted.clone());
            buffer.apply(vec![edit], None, EditSource::User, EditKind::Command);
        }
        Op::Undo => drop(buffer.undo()),
        Op::Redo => drop(buffer.redo()),
        Op::Caret(at) => {
            buffer.set_selections(SelectionSet::single(Selection::caret(boundary(&text, *at))))
        }
        Op::AddCaret(at) => {
            let mut selections = buffer.selections().clone();
            selections.add(Selection::caret(boundary(&text, *at)));
            buffer.set_selections(selections);
        }
        Op::Type(inserted) => drop(commands::insert(buffer, inserted, EditSource::User)),
        Op::Backspace => drop(commands::backspace(buffer, EditSource::User)),
        Op::Delete => drop(commands::delete_forward(buffer, EditSource::User)),
        Op::Duplicate => drop(commands::duplicate(buffer, EditSource::User)),
        Op::DeleteLines => drop(commands::delete_lines(buffer, EditSource::User)),
        Op::MoveLines(down) => drop(commands::move_lines(buffer, *down, EditSource::User)),
        Op::Comment => drop(commands::toggle_line_comment(
            buffer,
            "//",
            EditSource::User,
        )),
        Op::NextOccurrence => commands::select_next_occurrence(buffer),
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn rope_matches_the_reference(initial in "[a-cж\n]{0,40}", ops in prop::collection::vec(op(), 0..48)) {
        let mut rope: Buffer<RopeStore> = Buffer::new(&initial, FileFormat::default());
        let mut model: Buffer<StringStore> = Buffer::new(&initial, FileFormat::default());
        for op in &ops {
            run(&mut rope, op);
            run(&mut model, op);
            prop_assert_eq!(rope.text(), model.text());
            prop_assert_eq!(rope.selections(), model.selections());
            let store = rope.store();
            prop_assert_eq!(store.line_count(), model.store().line_count());
            for line in 0..store.line_count() {
                prop_assert_eq!(store.line_to_offset(line), model.store().line_to_offset(line));
                prop_assert_eq!(store.line_text(line), model.store().line_text(line));
            }
            let last = store.len();
            prop_assert_eq!(store.offset_to_utf16(last), model.store().offset_to_utf16(last));
            for selection in rope.selections().iter() {
                prop_assert!(selection.end() <= last);
                prop_assert!(store.is_char_boundary(selection.anchor) && store.is_char_boundary(selection.head));
            }
        }
    }

    #[test]
    fn undo_of_everything_restores_the_start(initial in "[a-cж\n]{0,40}", ops in prop::collection::vec(op(), 0..32)) {
        let mut buffer: Buffer<RopeStore> = Buffer::new(&initial, FileFormat::default());
        for op in &ops {
            run(&mut buffer, op);
        }
        let end = buffer.text();
        let mut undone = 0;
        while buffer.undo().is_some() {
            undone += 1;
        }
        prop_assert_eq!(buffer.text(), initial.clone());
        prop_assert!(!buffer.is_dirty());
        for _ in 0..undone {
            prop_assert!(buffer.redo().is_some());
        }
        prop_assert_eq!(buffer.text(), end);
    }

    #[test]
    fn a_change_replays_on_a_copy(initial in "[a-cж\n]{0,40}", ops in prop::collection::vec(op(), 0..32)) {
        let mut buffer: Buffer<RopeStore> = Buffer::new(&initial, FileFormat::default());
        for op in &ops {
            run(&mut buffer, op);
        }
        let before = buffer.text();
        let Some(change) = buffer.undo() else { return Ok(()) };
        let mut copy = StringStore::from_text(&before);
        for step in &change.steps {
            for edit in step.iter().rev() {
                copy.replace(edit.range.clone(), &edit.text);
            }
        }
        prop_assert_eq!(copy.0, buffer.text());
    }
}

fn buffer(text: &str, offset: usize) -> Buffer<RopeStore> {
    let mut buffer: Buffer<RopeStore> = Buffer::new(text, FileFormat::default());
    buffer.set_selections(SelectionSet::single(Selection::caret(offset)));
    buffer
}

#[test]
fn duplicate_copies_the_line_and_keeps_the_column() {
    let mut buffer = buffer("ab\ncd\n", 4);
    commands::duplicate(&mut buffer, EditSource::User);
    assert_eq!(buffer.text(), "ab\ncd\ncd\n");
    assert_eq!(buffer.selections().primary(), Selection::caret(7));
}

#[test]
fn delete_lines_removes_the_line_with_its_end() {
    let mut buffer = buffer("ab\ncd\nef", 4);
    commands::delete_lines(&mut buffer, EditSource::User);
    assert_eq!(buffer.text(), "ab\nef");
    assert_eq!(buffer.selections().primary(), Selection::caret(4));

    let mut last = self::buffer("ab\ncd", 4);
    commands::delete_lines(&mut last, EditSource::User);
    assert_eq!(last.text(), "ab");
}

#[test]
fn move_lines_swaps_with_the_neighbour() {
    let mut buffer = buffer("ab\ncd\nef\n", 4);
    commands::move_lines(&mut buffer, false, EditSource::User);
    assert_eq!(buffer.text(), "cd\nab\nef\n");
    assert_eq!(buffer.selections().primary(), Selection::caret(1));
    commands::move_lines(&mut buffer, true, EditSource::User);
    assert_eq!(buffer.text(), "ab\ncd\nef\n");
    assert_eq!(buffer.selections().primary(), Selection::caret(4));
}

#[test]
fn comment_goes_to_the_first_column_and_comes_back() {
    let mut buffer = buffer("    ab\n\n  cd\n", 0);
    buffer.set_selections(SelectionSet::single(Selection::new(0, 12)));
    commands::toggle_line_comment(&mut buffer, "//", EditSource::User);
    assert_eq!(buffer.text(), "//    ab\n\n//  cd\n");
    commands::toggle_line_comment(&mut buffer, "//", EditSource::User);
    assert_eq!(buffer.text(), "    ab\n\n  cd\n");
}

#[test]
fn next_occurrence_selects_the_word_then_the_next_one() {
    let mut buffer = buffer("foo bar foo", 1);
    commands::select_next_occurrence(&mut buffer);
    assert_eq!(buffer.selections().primary(), Selection::new(0, 3));
    commands::select_next_occurrence(&mut buffer);
    assert_eq!(buffer.selections().len(), 2);
    assert_eq!(buffer.selections().primary(), Selection::new(8, 11));
    commands::insert(&mut buffer, "x", EditSource::User);
    assert_eq!(buffer.text(), "x bar x");
}

#[test]
fn fast_typing_is_one_undo_step() {
    let mut buffer = buffer("", 0);
    for character in ["a", "b", "c"] {
        commands::insert(&mut buffer, character, EditSource::User);
    }
    assert!(buffer.is_dirty());
    buffer.undo();
    assert_eq!(buffer.text(), "");
    assert!(!buffer.is_dirty());
}

#[test]
fn saved_state_survives_undo_and_redo() {
    let mut buffer = buffer("a", 0);
    buffer.apply(
        vec![Edit::insert(0, "b")],
        None,
        EditSource::User,
        EditKind::Command,
    );
    buffer.mark_saved();
    assert!(!buffer.is_dirty());
    buffer.undo();
    assert!(buffer.is_dirty());
    buffer.redo();
    assert!(!buffer.is_dirty());
}

#[test]
fn agent_edits_do_not_join_typing() {
    let mut buffer = buffer("", 0);
    commands::insert(&mut buffer, "a", EditSource::User);
    commands::insert(&mut buffer, "b", EditSource::Agent(7));
    buffer.undo();
    assert_eq!(buffer.text(), "a");
}
