//! Editing commands for everyday typing: brackets and quotes, new lines, joining, case,
//! deleting words, block comments and finding the partner of a bracket. Like the others in
//! [`crate::commands`] they work on all carets at once and know nothing about keys.

use std::ops::Range;

use crate::buffer::{Buffer, Change};
use crate::commands::{touched_lines, word_at};
use crate::edit::{Edit, EditKind, EditSource};
use crate::selection::{Selection, SelectionSet};
use crate::store::TextStore;

/// How far from a bracket its partner is looked for.
const BRACKET_REACH: usize = 64 * 1024;

/// One caret's replacement and where the selection lands inside the new text, counted from
/// the start of the replaced range after the earlier plans took effect.
pub(crate) struct Plan {
    pub range: Range<usize>,
    pub text: String,
    pub select: Range<usize>,
}

/// Applies the plans of all carets. A plan that changes nothing still moves its caret.
pub(crate) fn apply_plans<S: TextStore>(
    buffer: &mut Buffer<S>,
    plans: Vec<Plan>,
    source: EditSource,
    kind: EditKind,
) -> Change {
    let mut delta = 0isize;
    let mut carets = Vec::with_capacity(plans.len());
    let mut edits = Vec::with_capacity(plans.len());
    for plan in plans {
        let start = (plan.range.start as isize + delta) as usize;
        carets.push(Selection::new(
            start + plan.select.start,
            start + plan.select.end,
        ));
        delta += plan.text.len() as isize - plan.range.len() as isize;
        edits.push(Edit::new(plan.range, plan.text));
    }
    let after = SelectionSet::from_vec(carets);
    let change = buffer.apply(edits, Some(after.clone()), source, kind);
    if change.is_empty() {
        buffer.set_selections(after);
    }
    change
}

pub(crate) fn closer_of(open: char) -> Option<char> {
    match open {
        '(' => Some(')'),
        '[' => Some(']'),
        '{' => Some('}'),
        '"' | '\'' | '`' => Some(open),
        _ => None,
    }
}

fn is_closer(character: char) -> bool {
    matches!(character, ')' | ']' | '}' | '"' | '\'' | '`')
}

fn is_quote(character: char) -> bool {
    matches!(character, '"' | '\'' | '`')
}

fn is_word(character: char) -> bool {
    character.is_alphanumeric() || character == '_'
}

fn char_before<S: TextStore>(store: &S, offset: usize) -> Option<char> {
    (offset > 0)
        .then(|| store.char_at(store.prev_boundary(offset)))
        .flatten()
}

/// Types one character. Brackets and quotes close themselves, a closing character in front
/// of the caret is typed over, a selection is wrapped, and `}` takes back one indent when it
/// starts a line.
pub fn type_char<S: TextStore>(
    buffer: &mut Buffer<S>,
    character: char,
    indent_unit: &str,
    source: EditSource,
) -> Change {
    let store = buffer.snapshot();
    let plans: Vec<Plan> = buffer
        .selections()
        .iter()
        .map(|selection| plan_char(&store, selection, character, indent_unit))
        .collect();
    apply_plans(buffer, plans, source, EditKind::Typing)
}

fn plan_char<S: TextStore>(store: &S, selection: &Selection, ch: char, unit: &str) -> Plan {
    let size = ch.len_utf8();
    let plain = Plan {
        range: selection.range(),
        text: ch.to_string(),
        select: size..size,
    };
    if !selection.is_empty() {
        let Some(close) = closer_of(ch) else {
            return plain;
        };
        let inner = store.slice(selection.range());
        return Plan {
            range: selection.range(),
            select: size..size + inner.len(),
            text: format!("{ch}{inner}{close}"),
        };
    }
    let at = selection.head;
    let (next, previous) = (store.char_at(at), char_before(store, at));
    if is_closer(ch) && next == Some(ch) {
        return Plan {
            range: at..at,
            text: String::new(),
            select: size..size,
        };
    }
    if ch == '}' && !unit.is_empty() {
        let line = store.offset_to_line(at);
        let before = store.slice(store.line_to_offset(line)..at);
        if before.ends_with(unit) && before.trim().is_empty() {
            return Plan {
                range: at - unit.len()..at,
                text: "}".to_owned(),
                select: size..size,
            };
        }
    }
    if let Some(close) = closer_of(ch) {
        let before_ok = !(is_quote(ch) && (previous.is_some_and(is_word) || previous == Some(ch)));
        let after_ok =
            next.is_none_or(|c| c.is_whitespace() || is_closer(c) || c == ',' || c == ';');
        if before_ok && after_ok {
            return Plan {
                range: at..at,
                text: format!("{ch}{close}"),
                select: size..size,
            };
        }
    }
    plain
}

/// Backspace between a bracket or quote and its closing partner removes both.
pub fn pair_to_delete<S: TextStore>(store: &S, head: usize) -> Option<Range<usize>> {
    let open = char_before(store, head)?;
    let next = store.char_at(head)?;
    (closer_of(open) == Some(next)).then(|| store.prev_boundary(head)..store.next_boundary(head))
}

/// The indent of the line that holds `offset`.
pub fn line_indent_of<S: TextStore>(store: &S, offset: usize) -> String {
    let line = store.line_text(store.offset_to_line(offset));
    line[..line.len() - line.trim_start().len()].to_owned()
}

/// Enter. Between a bracket and its partner the partner moves to a line of its own and
/// the caret lands in the empty line between them.
pub fn newline<S: TextStore>(
    buffer: &mut Buffer<S>,
    indent: impl Fn(&S, usize) -> String,
    source: EditSource,
) -> Change {
    let store = buffer.snapshot();
    let plans = buffer
        .selections()
        .iter()
        .map(|selection| {
            let inner = indent(&store, selection.start());
            let between = selection.is_empty()
                && char_before(&store, selection.head)
                    .zip(store.char_at(selection.head))
                    .is_some_and(|(open, close)| !is_quote(open) && closer_of(open) == Some(close));
            if between {
                let outer = line_indent_of(&store, selection.head);
                let at = 1 + inner.len();
                return Plan {
                    range: selection.range(),
                    text: format!("\n{inner}\n{outer}"),
                    select: at..at,
                };
            }
            let text = format!("\n{inner}");
            let end = text.len();
            Plan {
                range: selection.range(),
                text,
                select: end..end,
            }
        })
        .collect();
    apply_plans(buffer, plans, source, EditKind::Command)
}

/// Shift+Enter below the line, Ctrl+Alt+Enter above it, whatever the caret column.
pub fn start_line<S: TextStore>(
    buffer: &mut Buffer<S>,
    above: bool,
    indent_after: impl Fn(&S, usize) -> String,
    source: EditSource,
) -> Change {
    let store = buffer.snapshot();
    let mut lines: Vec<usize> = buffer
        .selections()
        .iter()
        .map(|selection| store.offset_to_line(selection.head))
        .collect();
    lines.dedup();
    let plans = lines
        .into_iter()
        .map(|line| {
            if above {
                let indent = line_indent_of(&store, store.line_to_offset(line));
                let at = indent.len();
                Plan {
                    range: store.line_to_offset(line)..store.line_to_offset(line),
                    text: format!("{indent}\n"),
                    select: at..at,
                }
            } else {
                let end = store.line_end(line);
                let text = format!("\n{}", indent_after(&store, end));
                let at = text.len();
                Plan {
                    range: end..end,
                    text,
                    select: at..at,
                }
            }
        })
        .collect();
    apply_plans(buffer, plans, source, EditKind::Command)
}

/// Ctrl+Shift+J: joins the selected lines, or the line with the next one.
pub fn join_lines<S: TextStore>(buffer: &mut Buffer<S>, source: EditSource) -> Change {
    let store = buffer.snapshot();
    let last_line = store.line_count() - 1;
    let mut junctions: Vec<usize> = Vec::new();
    for selection in buffer.selections().iter() {
        let first = store.offset_to_line(selection.start());
        let mut last = store.offset_to_line(selection.end());
        if !selection.is_empty() && selection.end() == store.line_to_offset(last) && last > first {
            last -= 1;
        }
        if selection.is_empty() || last == first {
            last = first + 1;
        }
        junctions.extend(first..last);
    }
    junctions.sort_unstable();
    junctions.dedup();
    junctions.retain(|line| *line < last_line);

    let mut edits = Vec::new();
    let mut carets = Vec::new();
    let mut delta = 0isize;
    for line in junctions {
        let text = store.line_text(line);
        let code = text.trim_end().len();
        let start = store.line_to_offset(line) + code;
        let next = store.line_text(line + 1);
        let skipped = next.len() - next.trim_start().len();
        let end = store.line_to_offset(line + 1) + skipped;
        if edits
            .last()
            .is_some_and(|last: &Edit| last.range.end > start)
        {
            continue;
        }
        let glue = if code == 0 || next.trim().is_empty() {
            ""
        } else {
            " "
        };
        carets.push(Selection::caret((start as isize + delta) as usize));
        delta += glue.len() as isize - (end - start) as isize;
        edits.push(Edit::new(start..end, glue));
    }
    if edits.is_empty() {
        return Change::default();
    }
    let after = SelectionSet::from_vec(carets);
    buffer.apply(edits, Some(after), source, EditKind::Command)
}

/// Ctrl+Shift+U: a selection, or the word at a caret, becomes upper case, or lower case
/// when it already is upper case.
pub fn toggle_case<S: TextStore>(buffer: &mut Buffer<S>, source: EditSource) -> Change {
    let store = buffer.snapshot();
    let mut edits = Vec::new();
    let mut after = Vec::new();
    let mut delta = 0isize;
    for selection in buffer.selections().iter() {
        let range = if selection.is_empty() {
            word_at(&store, selection.head)
        } else {
            selection.range()
        };
        let text = store.slice(range.clone());
        let changed = if text == text.to_uppercase() {
            text.to_lowercase()
        } else {
            text.to_uppercase()
        };
        let start = (range.start as isize + delta) as usize;
        if selection.is_empty() {
            let inside = (selection.head - range.start).min(changed.len());
            after.push(Selection::caret(start + inside));
        } else if selection.head < selection.anchor {
            after.push(Selection::new(start + changed.len(), start));
        } else {
            after.push(Selection::new(start, start + changed.len()));
        }
        delta += changed.len() as isize - text.len() as isize;
        if changed != text {
            edits.push(Edit::new(range, changed));
        }
    }
    let after = SelectionSet::from_vec(after);
    let change = buffer.apply(edits, Some(after.clone()), source, EditKind::Command);
    if change.is_empty() {
        buffer.set_selections(after);
    }
    change
}

/// Ctrl+Backspace and Ctrl+Delete. A selection goes first; at the end of a line only the
/// line end goes; blanks go by themselves, else a word or a run of punctuation.
pub fn delete_word<S: TextStore>(
    buffer: &mut Buffer<S>,
    forward: bool,
    source: EditSource,
) -> Change {
    let store = buffer.snapshot();
    let mut ranges: Vec<Range<usize>> = buffer
        .selections()
        .iter()
        .map(|selection| {
            if !selection.is_empty() {
                selection.range()
            } else if forward {
                selection.head..word_end(&store, selection.head)
            } else {
                word_start(&store, selection.head)..selection.head
            }
        })
        .filter(|range| !range.is_empty())
        .collect();
    ranges.sort_by_key(|range| range.start);
    let mut merged: Vec<Range<usize>> = Vec::with_capacity(ranges.len());
    for range in ranges {
        match merged.last_mut() {
            Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
            _ => merged.push(range),
        }
    }
    let edits = merged.into_iter().map(Edit::delete).collect();
    buffer.apply(edits, None, source, EditKind::Typing)
}

#[derive(PartialEq)]
enum Class {
    Newline,
    Blank,
    Word,
    Other,
}

fn class_of(character: char) -> Class {
    if character == '\n' {
        Class::Newline
    } else if character.is_whitespace() {
        Class::Blank
    } else if is_word(character) {
        Class::Word
    } else {
        Class::Other
    }
}

fn word_start<S: TextStore>(store: &S, offset: usize) -> usize {
    let Some(first) = char_before(store, offset) else {
        return offset;
    };
    let class = class_of(first);
    if class == Class::Newline {
        return store.prev_boundary(offset);
    }
    let mut at = offset;
    while let Some(character) = char_before(store, at) {
        if class_of(character) != class {
            break;
        }
        at = store.prev_boundary(at);
    }
    at
}

fn word_end<S: TextStore>(store: &S, offset: usize) -> usize {
    let Some(first) = store.char_at(offset) else {
        return offset;
    };
    let class = class_of(first);
    if class == Class::Newline {
        return store.next_boundary(offset);
    }
    let mut at = offset;
    while let Some(character) = store.char_at(at) {
        if class_of(character) != class {
            break;
        }
        at = store.next_boundary(at);
    }
    at
}

/// Ctrl+Shift+/: wraps the selection, or the code of the line at a caret, in the comment
/// marks, or takes them off when they are there.
pub fn toggle_block_comment<S: TextStore>(
    buffer: &mut Buffer<S>,
    (open, close): (&str, &str),
    source: EditSource,
) -> Change {
    let store = buffer.snapshot();
    let mut edits: Vec<Edit> = Vec::new();
    for selection in buffer.selections().iter() {
        let range = if selection.is_empty() {
            let line = store.offset_to_line(selection.head);
            let text = store.line_text(line);
            let start = store.line_to_offset(line) + (text.len() - text.trim_start().len());
            start..store.line_to_offset(line) + text.trim_end().len()
        } else {
            selection.range()
        };
        if range.is_empty()
            || edits
                .last()
                .is_some_and(|last| last.range.end > range.start)
        {
            continue;
        }
        let text = store.slice(range.clone());
        let trimmed = text.trim();
        if trimmed.len() >= open.len() + close.len()
            && trimmed.starts_with(open)
            && trimmed.ends_with(close)
        {
            let lead = text.len() - text.trim_start().len();
            let from = range.start + lead;
            edits.push(Edit::delete(from..from + open.len()));
            let to = range.start + lead + trimmed.len();
            edits.push(Edit::delete(to - close.len()..to));
        } else {
            edits.push(Edit::insert(range.start, open));
            edits.push(Edit::insert(range.end, close));
        }
    }
    buffer.apply(edits, None, source, EditKind::Command)
}

/// How many brackets of the line are still open, as the closing characters in the order
/// they are needed. Strings and characters are skipped, a comment ends the code.
fn open_brackets(code: &str) -> String {
    let mut stack: Vec<char> = Vec::new();
    let mut chars = code.chars().peekable();
    while let Some(character) = chars.next() {
        match character {
            '/' if chars.peek() == Some(&'/') => break,
            quote @ ('"' | '\'') => {
                while let Some(inner) = chars.next() {
                    match inner {
                        '\\' => {
                            chars.next();
                        }
                        c if c == quote => break,
                        _ => {}
                    }
                }
            }
            '(' => stack.push(')'),
            '[' => stack.push(']'),
            ')' | ']' => {
                stack.pop();
            }
            _ => {}
        }
    }
    stack.into_iter().rev().collect()
}

/// Ctrl+Shift+Enter: finishes the statement on the caret line and starts the next line.
/// Open brackets are closed; a statement that takes a block gets `{`, an empty line in it
/// and `}`; any other gets its terminator.
pub fn complete_statement<S: TextStore>(
    buffer: &mut Buffer<S>,
    terminator: &str,
    block_words: &[&str],
    indent_unit: &str,
    source: EditSource,
) -> Change {
    let store = buffer.snapshot();
    let head = buffer.selections().primary().head;
    let line = store.offset_to_line(head);
    let text = store.line_text(line);
    let code = text.trim_end();
    let indent = line_indent_of(&store, head);
    let start = store.line_to_offset(line);
    let range = start + code.len()..start + text.len();
    let mut finished = code.to_owned();
    finished.push_str(&open_brackets(code));
    let trimmed = finished.trim_start();
    let first_word = trimmed
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .next()
        .unwrap_or("");
    let has_block_word = trimmed
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .any(|word| block_words.contains(&word));
    let done = trimmed.is_empty()
        || trimmed.ends_with(terminator)
        || trimmed.ends_with('{')
        || trimmed.ends_with('}');
    let wants_block = !done && block_words.contains(&first_word)
        || !done && has_block_word && !trimmed.ends_with(')');
    let tail = &finished[code.len()..];
    let (text, caret) = if trimmed.ends_with('{') {
        let inner = format!("{indent}{indent_unit}");
        (
            format!("{tail}\n{inner}\n{indent}}}"),
            tail.len() + 1 + inner.len(),
        )
    } else if wants_block {
        let inner = format!("{indent}{indent_unit}");
        (
            format!("{tail} {{\n{inner}\n{indent}}}"),
            tail.len() + 2 + 1 + inner.len(),
        )
    } else if done {
        (format!("{tail}\n{indent}"), tail.len() + 1 + indent.len())
    } else {
        (
            format!("{tail}{terminator}\n{indent}"),
            tail.len() + terminator.len() + 1 + indent.len(),
        )
    };
    let plan = Plan {
        range,
        text,
        select: caret..caret,
    };
    apply_plans(buffer, vec![plan], source, EditKind::Command)
}

/// Shift+Tab: takes one indent off every touched line.
pub fn unindent<S: TextStore>(buffer: &mut Buffer<S>, unit: &str, source: EditSource) -> Change {
    let store = buffer.snapshot();
    let lines = touched_lines(&store, buffer.selections())
        .into_iter()
        .flatten();
    let edits = lines
        .filter_map(|line| {
            let text = store.line_text(line);
            let start = store.line_to_offset(line);
            let width = if text.starts_with('\t') {
                1
            } else {
                text.bytes()
                    .take(unit.len().max(1))
                    .take_while(|byte| *byte == b' ')
                    .count()
            };
            (width > 0).then(|| Edit::delete(start..start + width))
        })
        .collect();
    buffer.apply(edits, None, source, EditKind::Command)
}

/// The lines a command takes when the selection is empty: the whole file.
fn whole_file<S: TextStore>(store: &S, selections: &SelectionSet) -> Range<usize> {
    if selections.iter().all(Selection::is_empty) {
        0..store.line_count()
    } else {
        touched_lines(store, selections)
            .first()
            .cloned()
            .unwrap_or(0..store.line_count())
    }
}

/// The bytes of the lines and whether the block ends with a line break.
fn block_of<S: TextStore>(store: &S, lines: Range<usize>) -> (Range<usize>, String, bool) {
    let start = store.line_to_offset(lines.start);
    let end = if lines.end < store.line_count() {
        store.line_to_offset(lines.end)
    } else {
        store.len()
    };
    let block = store.slice(start..end);
    let trailing = block.ends_with('\n');
    (start..end, block, trailing)
}

/// Sorts the lines of the selection, or of the whole file, in the order of their text.
pub fn sort_lines<S: TextStore>(buffer: &mut Buffer<S>, source: EditSource) -> Change {
    let store = buffer.snapshot();
    let lines = whole_file(&store, buffer.selections());
    let (range, block, trailing) = block_of(&store, lines);
    let body = block.strip_suffix('\n').unwrap_or(&block);
    let mut parts: Vec<&str> = body.split('\n').collect();
    parts.sort_unstable();
    let mut text = parts.join("\n");
    if trailing {
        text.push('\n');
    }
    buffer.apply(
        vec![Edit::new(range, text)],
        None,
        source,
        EditKind::Command,
    )
}

/// Reverses the lines of the selection, or of the whole file.
pub fn reverse_lines<S: TextStore>(buffer: &mut Buffer<S>, source: EditSource) -> Change {
    let store = buffer.snapshot();
    let lines = whole_file(&store, buffer.selections());
    let (range, block, trailing) = block_of(&store, lines);
    let body = block.strip_suffix('\n').unwrap_or(&block);
    let mut parts: Vec<&str> = body.split('\n').collect();
    parts.reverse();
    let mut text = parts.join("\n");
    if trailing {
        text.push('\n');
    }
    buffer.apply(
        vec![Edit::new(range, text)],
        None,
        source,
        EditKind::Command,
    )
}

/// Converts the indentation of the selection, or of the whole file, to spaces or tabs of
/// `width` columns.
pub fn convert_indents<S: TextStore>(
    buffer: &mut Buffer<S>,
    tabs: bool,
    width: usize,
    source: EditSource,
) -> Change {
    let store = buffer.snapshot();
    let width = width.max(1);
    let lines = whole_file(&store, buffer.selections());
    let edits = (lines.start..lines.end)
        .filter_map(|line| {
            let start = store.line_to_offset(line);
            let text = store.line_text(line);
            let trimmed = text.trim_start_matches([' ', '\t']);
            let leading = &text[..text.len() - trimmed.len()];
            if leading.is_empty() {
                return None;
            }
            let mut columns = 0;
            for letter in leading.chars() {
                if letter == '\t' {
                    columns += width - columns % width;
                } else {
                    columns += 1;
                }
            }
            let indent = if tabs {
                let mut indent = "\t".repeat(columns / width);
                indent.push_str(&" ".repeat(columns % width));
                indent
            } else {
                " ".repeat(columns)
            };
            (indent != leading).then(|| Edit::new(start..start + leading.len(), indent))
        })
        .collect();
    buffer.apply(edits, None, source, EditKind::Command)
}

/// Ctrl+Alt+I: gives every touched line the indent `wanted` names for it.
pub fn reindent<S: TextStore>(
    buffer: &mut Buffer<S>,
    wanted: impl Fn(&S, usize) -> String,
    source: EditSource,
) -> Change {
    let store = buffer.snapshot();
    let lines = touched_lines(&store, buffer.selections())
        .into_iter()
        .flatten();
    let edits = lines
        .filter_map(|line| {
            let text = store.line_text(line);
            if text.trim().is_empty() {
                return None;
            }
            let current = text.len() - text.trim_start().len();
            let indent = wanted(&store, line);
            let start = store.line_to_offset(line);
            (text[..current] != indent).then(|| Edit::new(start..start + current, indent))
        })
        .collect();
    buffer.apply(edits, None, source, EditKind::Command)
}

/// Ctrl+Alt+Shift+J: selects every occurrence of the selected text, or of the word at the
/// caret as a whole word.
pub fn select_all_occurrences<S: TextStore>(buffer: &mut Buffer<S>) {
    let store = buffer.snapshot();
    let primary = buffer.selections().primary();
    let (needle, whole_word) = if primary.is_empty() {
        let word = word_at(&store, primary.head);
        (store.slice(word), true)
    } else {
        (store.slice(primary.range()), false)
    };
    if needle.is_empty() {
        return;
    }
    let text = store.text();
    let found: Vec<Selection> = text
        .match_indices(&needle)
        .filter(|(at, _)| {
            let end = at + needle.len();
            !whole_word
                || (!text[..*at].chars().next_back().is_some_and(is_word)
                    && !text[end..].chars().next().is_some_and(is_word))
        })
        .map(|(at, _)| Selection::new(at, at + needle.len()))
        .collect();
    if found.is_empty() {
        return;
    }
    let mut set = SelectionSet::from_vec(found);
    // The one under the caret stays the primary.
    let keep = set
        .iter()
        .find(|s| s.start() <= primary.head && primary.head <= s.end())
        .copied();
    if let Some(keep) = keep {
        set.add(keep);
    }
    buffer.set_selections(set);
}

/// The brackets outside strings and comments in `text`, with their offsets.
fn brackets_in(text: &str, base: usize) -> Vec<(usize, char)> {
    let mut found = Vec::new();
    let bytes = text.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        match bytes[at] {
            b'/' if bytes.get(at + 1) == Some(&b'/') => {
                while at < bytes.len() && bytes[at] != b'\n' {
                    at += 1;
                }
                continue;
            }
            b'/' if bytes.get(at + 1) == Some(&b'*') => {
                at += 2;
                while at < bytes.len() && !(bytes[at] == b'*' && bytes.get(at + 1) == Some(&b'/')) {
                    at += 1;
                }
                at += 2;
                continue;
            }
            quote @ (b'"' | b'\'') => {
                let triple = quote == b'"' && text[at..].starts_with("\"\"\"");
                at += if triple { 3 } else { 1 };
                while at < bytes.len() {
                    if triple && text[at..].starts_with("\"\"\"") {
                        at += 3;
                        break;
                    }
                    match bytes[at] {
                        b'\\' => at += 2,
                        b'\n' if !triple => break,
                        byte if byte == quote && !triple => {
                            at += 1;
                            break;
                        }
                        _ => at += 1,
                    }
                }
                continue;
            }
            byte @ (b'(' | b')' | b'[' | b']' | b'{' | b'}') => {
                found.push((base + at, byte as char));
            }
            _ => {}
        }
        at += 1;
    }
    found
}

fn matches_pair(open: char, close: char) -> bool {
    matches!((open, close), ('(', ')') | ('[', ']') | ('{', '}'))
}

/// The bracket next to the caret (after it, else before it) and its partner, as offsets of
/// the opening and the closing one.
pub fn bracket_pair<S: TextStore>(store: &S, caret: usize) -> Option<(usize, usize)> {
    let is_bracket = |at: usize| {
        store
            .char_at(at)
            .filter(|c| matches!(c, '(' | ')' | '[' | ']' | '{' | '}'))
    };
    let at = if is_bracket(caret).is_some() {
        caret
    } else {
        let before = store.prev_boundary(caret);
        if caret > 0 && is_bracket(before).is_some() {
            before
        } else {
            return None;
        }
    };
    let character = is_bracket(at)?;
    if matches!(character, '(' | '[' | '{') {
        let end = store.len().min(at + BRACKET_REACH);
        let end = (end..=store.len()).find(|o| store.is_char_boundary(*o))?;
        let window = store.slice(at..end);
        let mut stack: Vec<char> = Vec::new();
        for (offset, bracket) in brackets_in(&window, at) {
            if matches!(bracket, '(' | '[' | '{') {
                stack.push(bracket);
            } else {
                let open = stack.pop()?;
                if !matches_pair(open, bracket) {
                    return None;
                }
                if stack.is_empty() {
                    return Some((at, offset));
                }
            }
        }
        None
    } else {
        let start = at.saturating_sub(BRACKET_REACH);
        let start = (0..=start).rev().find(|o| store.is_char_boundary(*o))?;
        let window = store.slice(start..at + 1);
        let mut stack: Vec<(usize, char)> = Vec::new();
        for (offset, bracket) in brackets_in(&window, start) {
            if matches!(bracket, '(' | '[' | '{') {
                stack.push((offset, bracket));
            } else {
                let (open_at, open) = stack.pop()?;
                if offset == at {
                    return matches_pair(open, bracket).then_some((open_at, at));
                }
            }
        }
        None
    }
}

/// The innermost pair of brackets that surrounds the caret, as offsets of the opening
/// and the closing one. The caret itself is strictly inside: a bracket next to it is the
/// business of [`bracket_pair`].
pub fn enclosing_bracket_pair<S: TextStore>(store: &S, caret: usize) -> Option<(usize, usize)> {
    if caret == 0 || caret > store.len() {
        return None;
    }
    let start = caret.saturating_sub(BRACKET_REACH);
    let start = (0..=start).rev().find(|o| store.is_char_boundary(*o))?;
    let window = store.slice(start..caret);
    let mut openings = Vec::new();
    for (offset, bracket) in brackets_in(&window, start) {
        if matches!(bracket, '(' | '[' | '{') {
            openings.push(offset);
        }
    }
    for open in openings.into_iter().rev() {
        if open >= caret {
            continue;
        }
        if let Some(close) = matching_close(store, open)
            && close > caret
        {
            return Some((open, close));
        }
    }
    None
}

/// The closing bracket of the opening one at `open`, if the text has one.
fn matching_close<S: TextStore>(store: &S, open: usize) -> Option<usize> {
    let end = store.len().min(open + BRACKET_REACH);
    let end = (end..=store.len()).find(|o| store.is_char_boundary(*o))?;
    let window = store.slice(open..end);
    let mut stack: Vec<char> = Vec::new();
    for (offset, bracket) in brackets_in(&window, open) {
        if matches!(bracket, '(' | '[' | '{') {
            stack.push(bracket);
        } else {
            let expected = stack.pop()?;
            if !matches_pair(expected, bracket) {
                return None;
            }
            if stack.is_empty() {
                return Some(offset);
            }
        }
    }
    None
}
