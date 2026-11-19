//! Editing commands on all carets at once. They know nothing about keys or the screen,
//! so an agent or a test runs the same code as a key press.

use std::ops::Range;

use crate::buffer::{Buffer, Change};
use crate::edit::{Edit, EditKind, EditSource};
pub use crate::editing::newline;
use crate::editing::pair_to_delete;
use crate::selection::{Selection, SelectionSet};
use crate::store::TextStore;

/// Replaces every selection with `text`. Carets land after the inserted text.
pub fn insert<S: TextStore>(buffer: &mut Buffer<S>, text: &str, source: EditSource) -> Change {
    let single_character = text.chars().count() == 1 && text != "\n";
    let kind = if single_character {
        EditKind::Typing
    } else {
        EditKind::Command
    };
    let edits: Vec<Edit> = buffer
        .selections()
        .iter()
        .map(|selection| Edit::new(selection.range(), text))
        .collect();
    let after = carets_after(&edits);
    buffer.apply(edits, Some(after), source, kind)
}

/// Selections go first; an empty one takes the character before the caret.
pub fn backspace<S: TextStore>(buffer: &mut Buffer<S>, source: EditSource) -> Change {
    let store = buffer.store();
    let ranges = buffer.selections().iter().map(|selection| {
        if selection.is_empty() {
            pair_to_delete(&store, selection.head)
                .unwrap_or_else(|| store.prev_boundary(selection.head)..selection.head)
        } else {
            selection.range()
        }
    });
    let edits = deletions(ranges.collect());
    buffer.apply(edits, None, source, EditKind::Typing)
}

/// Backspace in the indentation of a line. A step of the indentation goes at once, and an
/// empty line drops to its proper indent before it joins the line above:
///
/// - in the leading whitespace of a line, the spaces back to the previous step of
///   `indent.size` columns go;
/// - an empty line deeper than the line above it goes to the indent of that line, or one
///   step deeper when that line opens a block;
/// - an empty line at its proper indent joins the line above.
pub fn backspace_indent<S: TextStore>(
    buffer: &mut Buffer<S>,
    indent: &crate::indent::IndentSettings,
    source: EditSource,
) -> Change {
    let store = buffer.store();
    let ranges = buffer.selections().iter().map(|selection| {
        if selection.is_empty() {
            pair_to_delete(&store, selection.head)
                .unwrap_or_else(|| indent_range(&store, selection.head, indent))
        } else {
            selection.range()
        }
    });
    let edits = deletions(ranges.collect());
    buffer.apply(edits, None, source, EditKind::Typing)
}

/// What Backspace takes before `head` when the caret is in the indentation.
fn indent_range<S: TextStore>(
    store: &S,
    head: usize,
    indent: &crate::indent::IndentSettings,
) -> Range<usize> {
    let line = store.offset_to_line(head);
    let start = store.line_to_offset(line);
    let before = store.slice(start..head);
    if !before
        .chars()
        .all(|character| character == ' ' || character == '\t')
    {
        return store.prev_boundary(head)..head;
    }
    let end = store.line_end(line);
    let blank = head == end
        && store
            .slice(start..end)
            .chars()
            .all(|character| character == ' ' || character == '\t');
    if blank {
        let proper = proper_indent(store, line, indent);
        let columns = indentation_columns(&before, indent.tab_width);
        if columns > proper {
            return start + offset_of_columns(&before, proper, indent.tab_width)..head;
        }
        if line > 0 {
            return start.saturating_sub(1)..head;
        }
        return start..head;
    }
    let columns = indentation_columns(&before, indent.tab_width);
    if columns == 0 {
        return store.prev_boundary(head)..head;
    }
    let step = indent.size.max(1);
    let target = ((columns - 1) / step) * step;
    start + offset_of_columns(&before, target, indent.tab_width)..head
}

/// The indent of the block a line sits in: the indent of the nearest line above it that
/// has text, one step deeper when that line opens a block.
fn proper_indent<S: TextStore>(
    store: &S,
    line: usize,
    indent: &crate::indent::IndentSettings,
) -> usize {
    let mut above = line;
    while above > 0 {
        above -= 1;
        let text = store.line_text(above);
        let trimmed = text.trim_start_matches([' ', '\t']);
        if trimmed.is_empty() {
            continue;
        }
        let leading = &text[..text.len() - trimmed.len()];
        let mut columns = indentation_columns(leading, indent.tab_width);
        if trimmed.trim_end().ends_with(['{', '(', '[']) {
            columns += indent.size.max(1);
        }
        return columns;
    }
    0
}

/// The columns of a run of spaces and tabs, a tab reaching the next stop.
fn indentation_columns(text: &str, tab_width: usize) -> usize {
    let tab_width = tab_width.max(1);
    let mut columns = 0;
    for character in text.chars() {
        if character == '\t' {
            columns += tab_width - columns % tab_width;
        } else {
            columns += 1;
        }
    }
    columns
}

/// How many bytes of `text` hold `columns` columns, or the last place before that.
fn offset_of_columns(text: &str, columns: usize, tab_width: usize) -> usize {
    let tab_width = tab_width.max(1);
    let mut reached = 0;
    let mut at = 0;
    for (index, character) in text.char_indices() {
        let next = if character == '\t' {
            reached + tab_width - reached % tab_width
        } else {
            reached + 1
        };
        if next > columns {
            break;
        }
        reached = next;
        at = index + character.len_utf8();
    }
    at
}

/// Selections go first; an empty one takes the character after the caret.
pub fn delete_forward<S: TextStore>(buffer: &mut Buffer<S>, source: EditSource) -> Change {
    let store = buffer.store();
    let ranges = buffer.selections().iter().map(|selection| {
        if selection.is_empty() {
            selection.head..store.next_boundary(selection.head)
        } else {
            selection.range()
        }
    });
    let edits = deletions(ranges.collect());
    buffer.apply(edits, None, source, EditKind::Typing)
}

/// Ctrl+D: a selection is copied after itself, a caret copies its line below.
pub fn duplicate<S: TextStore>(buffer: &mut Buffer<S>, source: EditSource) -> Change {
    let store = buffer.snapshot();
    let mut planned: Vec<(Edit, Vec<Range<usize>>)> = Vec::new();
    let mut last_line = None;
    for selection in buffer.selections().iter() {
        if !selection.is_empty() {
            let text = store.slice(selection.range());
            let copy = 0..text.len();
            planned.push((Edit::insert(selection.end(), text), vec![copy]));
            continue;
        }
        let line = store.offset_to_line(selection.head);
        let column = selection.head - store.line_to_offset(line);
        let caret = 1 + column..1 + column;
        if last_line == Some(line) {
            if let Some((_, carets)) = planned.last_mut() {
                carets.push(caret);
            }
        } else {
            planned.push((
                Edit::insert(store.line_end(line), format!("\n{}", store.line_text(line))),
                vec![caret],
            ));
            last_line = Some(line);
        }
    }
    planned.sort_by_key(|(edit, _)| edit.range.start);

    let mut delta = 0isize;
    let mut carets = Vec::new();
    let mut edits = Vec::with_capacity(planned.len());
    for (edit, inside) in planned {
        let start = (edit.range.start as isize + delta) as usize;
        carets.extend(
            inside
                .iter()
                .map(|range| Selection::new(start + range.start, start + range.end)),
        );
        delta += edit.delta();
        edits.push(edit);
    }
    buffer.apply(
        edits,
        Some(SelectionSet::from_vec(carets)),
        source,
        EditKind::Command,
    )
}

/// Ctrl+Y: removes every line with a caret or a selection on it.
pub fn delete_lines<S: TextStore>(buffer: &mut Buffer<S>, source: EditSource) -> Change {
    let store = buffer.snapshot();
    let blocks = touched_lines(&store, buffer.selections());
    let last_line = store.line_count() - 1;
    let mut edits = Vec::new();
    let mut carets = Vec::new();
    let mut removed = 0usize;
    for block in &blocks {
        let range = whole_lines(&store, block.clone());
        let column = buffer
            .selections()
            .iter()
            .find(|selection| block.contains(&store.offset_to_line(selection.head)))
            .map_or(0, |selection| {
                selection.head - store.line_to_offset(store.offset_to_line(selection.head))
            });
        // The caret stays in its column on the line that takes the place of the removed ones.
        let caret = if block.end <= last_line {
            range.start - removed + column.min(store.line_text(block.end).len())
        } else if block.start > 0 {
            let line = block.start - 1;
            store.line_to_offset(line) - removed + column.min(store.line_text(line).len())
        } else {
            0
        };
        carets.push(Selection::caret(caret));
        removed += range.len();
        edits.push(Edit::delete(range));
    }
    buffer.apply(
        edits,
        Some(SelectionSet::from_vec(carets)),
        source,
        EditKind::Command,
    )
}

/// Alt+Shift+Up and Down: moves the touched lines past their neighbour.
pub fn move_lines<S: TextStore>(buffer: &mut Buffer<S>, down: bool, source: EditSource) -> Change {
    let store = buffer.snapshot();
    let lines = touched_lines(&store, buffer.selections());
    let last = store.line_count() - 1;
    if lines.iter().any(|block| {
        if down {
            block.end > last
        } else {
            block.start == 0
        }
    }) {
        return Change::default();
    }
    let mut edits = Vec::new();
    let mut shifts: Vec<(Range<usize>, isize)> = Vec::new();
    for block in &lines {
        let (from, to) = if down {
            (block.start, block.end)
        } else {
            (block.start - 1, block.end - 1)
        };
        let (moved, neighbour) = if down {
            (from..to, to)
        } else {
            (from + 1..to + 1, from)
        };
        let moved_text =
            store.slice(store.line_to_offset(moved.start)..store.line_end(moved.end - 1));
        let neighbour_text = store.line_text(neighbour);
        let replaced = store.line_to_offset(from)..store.line_end(to);
        let text = if down {
            format!("{neighbour_text}\n{moved_text}")
        } else {
            format!("{moved_text}\n{neighbour_text}")
        };
        let source_range = store.line_to_offset(moved.start)..store.line_end(moved.end - 1) + 1;
        let shift = if down {
            neighbour_text.len() as isize + 1
        } else {
            -(neighbour_text.len() as isize + 1)
        };
        shifts.push((source_range, shift));
        edits.push(Edit::new(replaced, text));
    }
    let after = buffer.selections().map(|selection| {
        let shifted = |offset: usize| {
            let found = shifts
                .iter()
                .find(|(range, _)| range.start <= offset && offset <= range.end);
            found.map_or(offset, |(_, shift)| (offset as isize + shift) as usize)
        };
        Selection::new(shifted(selection.anchor), shifted(selection.head))
    });
    buffer.apply(edits, Some(after), source, EditKind::Command)
}

/// Ctrl+/: comments the touched lines at the first column, or uncomments them when all
/// their non-blank lines are comments.
pub fn toggle_line_comment<S: TextStore>(
    buffer: &mut Buffer<S>,
    prefix: &str,
    source: EditSource,
) -> Change {
    let store = buffer.snapshot();
    let lines: Vec<usize> = touched_lines(&store, buffer.selections())
        .into_iter()
        .flatten()
        .collect();
    let filled: Vec<usize> = lines
        .iter()
        .copied()
        .filter(|line| !store.line_text(*line).trim().is_empty())
        .collect();
    if filled.is_empty() {
        return Change::default();
    }
    let commented = filled
        .iter()
        .all(|line| store.line_text(*line).trim_start().starts_with(prefix));
    let edits: Vec<Edit> = filled
        .iter()
        .map(|line| {
            let start = store.line_to_offset(*line);
            if commented {
                let text = store.line_text(*line);
                let indent = text.len() - text.trim_start().len();
                Edit::delete(start + indent..start + indent + prefix.len())
            } else {
                Edit::insert(start, prefix)
            }
        })
        .collect();
    buffer.apply(edits, None, source, EditKind::Command)
}

/// Alt+J: selects the word at a lone caret, then adds the next occurrence of the selection.
pub fn select_next_occurrence<S: TextStore>(buffer: &mut Buffer<S>) {
    let store = buffer.snapshot();
    let primary = buffer.selections().primary();
    if primary.is_empty() {
        let word = word_at(&store, primary.head);
        let selected = buffer.selections().map(|selection| {
            if selection == &primary {
                Selection::new(word.start, word.end)
            } else {
                *selection
            }
        });
        buffer.set_selections(selected);
        return;
    }
    let needle = store.slice(primary.range());
    let text = store.text();
    let from = primary.end();
    let found = text[from..]
        .find(&needle)
        .map(|at| at + from)
        .or_else(|| text[..primary.start()].find(&needle));
    if let Some(at) = found {
        let mut selections = buffer.selections().clone();
        selections.add(Selection::new(at, at + needle.len()));
        buffer.set_selections(selections);
    }
}

/// The word around `offset`, or the empty range at it.
pub fn word_at<S: TextStore>(store: &S, offset: usize) -> Range<usize> {
    let is_word = |at: usize| store.char_at(at).is_some_and(is_word_char);
    let mut start = offset;
    while start > 0 {
        let previous = store.prev_boundary(start);
        if !is_word(previous) {
            break;
        }
        start = previous;
    }
    let mut end = offset;
    while is_word(end) {
        end = store.next_boundary(end);
    }
    start..end
}

/// Ctrl+Left: the start of the previous word.
pub fn previous_word<S: TextStore>(store: &S, offset: usize) -> usize {
    let mut at = offset;
    while at > 0
        && !store
            .char_at(store.prev_boundary(at))
            .is_some_and(is_word_char)
    {
        at = store.prev_boundary(at);
    }
    while at > 0
        && store
            .char_at(store.prev_boundary(at))
            .is_some_and(is_word_char)
    {
        at = store.prev_boundary(at);
    }
    at
}

/// Ctrl+Right: the end of the next word.
pub fn next_word<S: TextStore>(store: &S, offset: usize) -> usize {
    let mut at = offset;
    while at < store.len() && !store.char_at(at).is_some_and(is_word_char) {
        at = store.next_boundary(at);
    }
    while at < store.len() && store.char_at(at).is_some_and(is_word_char) {
        at = store.next_boundary(at);
    }
    at
}

fn is_word_char(character: char) -> bool {
    character.is_alphanumeric() || character == '_'
}

/// Carets after a list of replacements, one at the end of each new text.
fn carets_after(edits: &[Edit]) -> SelectionSet {
    let mut delta = 0isize;
    let carets = edits.iter().map(|edit| {
        delta += edit.delta();
        Selection::caret((edit.range.end as isize + delta) as usize)
    });
    SelectionSet::from_vec(carets.collect())
}

/// Neighbouring carets can claim the same character.
fn deletions(mut ranges: Vec<Range<usize>>) -> Vec<Edit> {
    ranges.sort_by_key(|range| range.start);
    let mut merged: Vec<Range<usize>> = Vec::with_capacity(ranges.len());
    for range in ranges {
        match merged.last_mut() {
            Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
            _ => merged.push(range),
        }
    }
    merged.into_iter().map(Edit::delete).collect()
}

/// Runs of whole lines with a selection or caret on them, sorted and joined.
pub(crate) fn touched_lines<S: TextStore>(
    store: &S,
    selections: &SelectionSet,
) -> Vec<Range<usize>> {
    let mut blocks: Vec<Range<usize>> = Vec::new();
    for selection in selections.iter() {
        let first = store.offset_to_line(selection.start());
        let mut last = store.offset_to_line(selection.end());
        // A selection that ends at the start of a line does not touch that line.
        if !selection.is_empty() && selection.end() == store.line_to_offset(last) {
            last -= 1;
        }
        match blocks.last_mut() {
            Some(block) if first <= block.end => block.end = block.end.max(last + 1),
            _ => blocks.push(first..last + 1),
        }
    }
    blocks
}

/// The bytes of `lines` with one line end. The last line of the text takes the one before it.
fn whole_lines<S: TextStore>(store: &S, lines: Range<usize>) -> Range<usize> {
    if lines.end < store.line_count() {
        store.line_to_offset(lines.start)..store.line_to_offset(lines.end)
    } else {
        let start = store.line_to_offset(lines.start);
        start.saturating_sub(1)..store.len()
    }
}
