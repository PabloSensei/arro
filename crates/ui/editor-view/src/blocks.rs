//! Braces of the text: block bounds, the units of a block and its members.
//!
//! The structure of a file comes from the language when it has an outline. These
//! helpers cover the rest: the same commands work on any text with `{}`.

use std::ops::Range;

/// One line of the text, with and without its line break.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Line {
    pub start: usize,
    /// The last byte before the line break.
    pub end: usize,
    /// The byte after the line break, or the end of the text.
    pub full_end: usize,
}

pub(crate) fn lines(text: &str) -> Vec<Line> {
    let mut found = Vec::new();
    let mut start = 0;
    for (at, byte) in text.bytes().enumerate() {
        if byte == b'\n' {
            found.push(Line {
                start,
                end: at,
                full_end: at + 1,
            });
            start = at + 1;
        }
    }
    found.push(Line {
        start,
        end: text.len(),
        full_end: text.len(),
    });
    found
}

pub(crate) fn line_of(lines: &[Line], offset: usize) -> usize {
    lines
        .partition_point(|line| line.start <= offset)
        .saturating_sub(1)
}

/// Offsets of the matched `{}` pairs, outermost first. Braces in strings, character
/// literals and comments are not block braces.
pub(crate) fn brace_pairs(text: &str) -> Vec<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut pairs = Vec::new();
    let mut open = Vec::new();
    let mut quote: Option<u8> = None;
    let mut line_comment = false;
    let mut block_comment = false;
    let mut at = 0;
    while at < bytes.len() {
        let byte = bytes[at];
        let next = bytes.get(at + 1).copied();
        if line_comment {
            if byte == b'\n' {
                line_comment = false;
            }
            at += 1;
            continue;
        }
        if block_comment {
            if byte == b'*' && next == Some(b'/') {
                block_comment = false;
                at += 2;
            } else {
                at += 1;
            }
            continue;
        }
        if let Some(delimiter) = quote {
            if byte == b'\\' {
                at += 2;
                continue;
            }
            if byte == delimiter {
                quote = None;
            }
            at += 1;
            continue;
        }
        match byte {
            b'"' | b'\'' => {
                quote = Some(byte);
                at += 1;
            }
            b'/' if next == Some(b'/') => {
                line_comment = true;
                at += 2;
            }
            b'/' if next == Some(b'*') => {
                block_comment = true;
                at += 2;
            }
            b'{' => {
                open.push(at);
                at += 1;
            }
            b'}' => {
                if let Some(start) = open.pop() {
                    pairs.push((start, at));
                }
                at += 1;
            }
            _ => at += 1,
        }
    }
    pairs.sort_by_key(|(start, _)| *start);
    pairs
}

/// How many blocks contain `offset`.
pub(crate) fn depth_at(pairs: &[(usize, usize)], offset: usize) -> usize {
    pairs
        .iter()
        .filter(|(start, end)| *start < offset && offset <= *end)
        .count()
}

fn innermost(pairs: &[(usize, usize)], range: Range<usize>) -> Option<(usize, usize)> {
    pairs
        .iter()
        .filter(|(start, end)| *start < range.start && range.end <= *end)
        .min_by_key(|(start, end)| end - start)
        .copied()
}

/// The innermost block that holds `range`.
pub(crate) fn containing_block(
    pairs: &[(usize, usize)],
    range: Range<usize>,
) -> Option<(usize, usize)> {
    innermost(pairs, range)
}

/// The innermost block that holds `offset`, the block that starts at it included.
pub(crate) fn enclosing_block(pairs: &[(usize, usize)], offset: usize) -> Option<(usize, usize)> {
    pairs
        .iter()
        .filter(|(start, end)| *start <= offset && offset <= *end)
        .min_by_key(|(start, end)| end - start)
        .copied()
}

/// The block whose direct lines are the members of a class: a body that starts with
/// `)` (a method or a control block) is walked outwards.
pub(crate) fn member_block(
    pairs: &[(usize, usize)],
    text: &str,
    offset: usize,
) -> Option<(usize, usize)> {
    let all = lines(text);
    let mut block = enclosing_block(pairs, offset);
    while let Some((open, close)) = block {
        let line = line_of(&all, open);
        let before = &text[all[line].start..open];
        if before.contains(')') {
            block = innermost(pairs, open..open);
            continue;
        }
        return Some((open, close));
    }
    None
}

/// The starts of the lines that begin a member of the block around `offset`.
pub(crate) fn member_starts(text: &str, offset: usize) -> Vec<usize> {
    let pairs = brace_pairs(text);
    let all = lines(text);
    let Some((open, close)) = member_block(&pairs, text, offset) else {
        return Vec::new();
    };
    let base = depth_at(&pairs, open) + 1;
    starts_between(&all, text, &pairs, base, open, close)
}

fn starts_between(
    all: &[Line],
    text: &str,
    pairs: &[(usize, usize)],
    depth: usize,
    from: usize,
    to: usize,
) -> Vec<usize> {
    all.iter()
        .filter(|line| line.start > from && line.start < to)
        .filter(|line| depth_at(pairs, line.start) == depth)
        .filter(|line| {
            let text = &text[line.start..line.end];
            let trimmed = text.trim_start();
            !trimmed.is_empty() && !trimmed.starts_with('}')
        })
        .map(|line| line.start)
        .collect()
}

/// The units of a block: a statement up to its `;`, or a line that opens a block and
/// everything through its closing `}`.
pub(crate) fn units_in(
    text: &str,
    pairs: &[(usize, usize)],
    block: (usize, usize),
) -> Vec<Range<usize>> {
    let (open, close) = block;
    let all = lines(text);
    let base = depth_at(pairs, open) + 1;
    let starts = starts_between(&all, text, pairs, base, open, close);
    let mut units = Vec::new();
    let mut next = open + 1;
    for start in starts {
        if start < next {
            continue;
        }
        let line = line_of(&all, start);
        if let Some(unit) = unit_from(text, &all, pairs, line, base, close) {
            next = unit.end;
            units.push(unit);
        }
    }
    units
}

fn unit_from(
    text: &str,
    all: &[Line],
    pairs: &[(usize, usize)],
    index: usize,
    base: usize,
    close: usize,
) -> Option<Range<usize>> {
    let start = all[index].start;
    let mut at = index;
    loop {
        let line = *all.get(at)?;
        if line.start >= close {
            let previous = all.get(at.saturating_sub(1))?;
            return Some(start..previous.full_end.min(close));
        }
        if at > index && depth_at(pairs, line.start) < base {
            let previous = all[at - 1];
            return Some(start..previous.full_end);
        }
        if at > index && depth_at(pairs, line.start) > base {
            // A line that continues an open block belongs to the unit that opened it.
            at += 1;
            continue;
        }
        let body = &text[line.start..line.end];
        let open_here = pairs
            .iter()
            .filter(|(open, end)| *open >= line.start && *open < line.end && *end <= close)
            .map(|(open, _)| *open)
            .min();
        if let Some(open) = open_here {
            let semicolon = body.find(';');
            if semicolon.is_none_or(|semicolon| open - line.start < semicolon)
                && let Some((_, end)) = pairs.iter().find(|(pair_open, _)| *pair_open == open)
            {
                let end_line = line_of(all, *end);
                return Some(start..all[end_line].full_end.min(text.len()));
            }
        }
        if body.contains(';') {
            return Some(start..line.full_end.min(text.len()));
        }
        at += 1;
    }
}

/// The elements of the list under `offset` and which one holds it. An argument list,
/// a parameter list, an array initializer and a list of types after a word on a line
/// are all comma separated lists here; the text does not say what the words mean.
pub(crate) fn element_list_at(text: &str, offset: usize) -> Option<(Vec<Range<usize>>, usize)> {
    let pairs = bracket_pairs(text);
    let pair = pairs
        .iter()
        .filter(|(open, close)| *open < offset && offset < *close)
        .min_by_key(|(open, close)| close - open);
    let (start, end, inside) = match pair {
        Some((open, close)) => (*open + 1, *close, true),
        None => {
            let all = lines(text);
            let line = all.get(line_of(&all, offset))?;
            (line.start, line.end, false)
        }
    };
    let mut elements = split_list(text, start, end);
    if elements.len() < 2 {
        return None;
    }
    let index = elements
        .iter()
        .position(|element| element.start <= offset && offset <= element.end)?;
    if !inside {
        // The first element of a statement may hold the declaration before the list:
        // `class A implements B, C` keeps `class A implements` and moves the names.
        if let Some(first) = elements.first().cloned() {
            let token = token_at(text, offset.max(first.start), first.clone());
            if !token.is_empty() {
                elements[0] = token;
            }
        }
    }
    Some((elements, index))
}

/// The innermost comma separated list that holds `offset`.
fn split_list(text: &str, start: usize, end: usize) -> Vec<Range<usize>> {
    let mut elements = Vec::new();
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut quote: Option<u8> = None;
    let mut line_comment = false;
    let mut block_comment = false;
    let mut from = start;
    let mut at = start;
    while at < end {
        let byte = bytes[at];
        let next = bytes.get(at + 1).copied();
        if line_comment {
            if byte == b'\n' {
                line_comment = false;
            }
            at += 1;
            continue;
        }
        if block_comment {
            if byte == b'*' && next == Some(b'/') {
                block_comment = false;
                at += 2;
            } else {
                at += 1;
            }
            continue;
        }
        if let Some(delimiter) = quote {
            if byte == b'\\' {
                at += 2;
                continue;
            }
            if byte == delimiter {
                quote = None;
            }
            at += 1;
            continue;
        }
        match byte {
            b'"' | b'\'' | b'`' => {
                quote = Some(byte);
                at += 1;
            }
            b'/' if next == Some(b'/') => {
                line_comment = true;
                at += 2;
            }
            b'/' if next == Some(b'*') => {
                block_comment = true;
                at += 2;
            }
            b'(' | b'[' | b'{' => {
                depth += 1;
                at += 1;
            }
            b')' | b']' | b'}' => {
                depth = depth.saturating_sub(1);
                at += 1;
            }
            b',' if depth == 0 => {
                push_element(text, from..at, &mut elements);
                from = at + 1;
                at += 1;
            }
            _ => at += 1,
        }
    }
    push_element(text, from..end, &mut elements);
    elements
}

fn push_element(text: &str, range: Range<usize>, elements: &mut Vec<Range<usize>>) {
    let mut start = range.start;
    let mut end = range.end;
    while start < end && text.as_bytes()[start].is_ascii_whitespace() {
        start += 1;
    }
    while start < end && text.as_bytes()[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    while start < end && matches!(text.as_bytes()[end - 1], b';' | b'{' | b'}') {
        end -= 1;
        while start < end && text.as_bytes()[end - 1].is_ascii_whitespace() {
            end -= 1;
        }
    }
    if start < end {
        elements.push(start..end);
    }
}

/// The word under `caret` inside `range`, or the range itself when there is no word.
fn token_at(text: &str, caret: usize, range: Range<usize>) -> Range<usize> {
    let is_word =
        |character: char| character.is_alphanumeric() || character == '_' || character == '$';
    let mut start = caret.min(range.end);
    while start > range.start {
        let Some(character) = text[..start].chars().next_back() else {
            break;
        };
        if !is_word(character) {
            break;
        }
        start -= character.len_utf8();
    }
    let mut end = caret.min(range.end);
    while end < range.end {
        let Some(character) = text[end..].chars().next() else {
            break;
        };
        if !is_word(character) {
            break;
        }
        end += character.len_utf8();
    }
    if start == end { range } else { start..end }
}

/// The matched brackets of every kind, in byte order.
fn bracket_pairs(text: &str) -> Vec<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut pairs = Vec::new();
    let mut open: Vec<(usize, u8)> = Vec::new();
    let mut quote: Option<u8> = None;
    let mut line_comment = false;
    let mut block_comment = false;
    let mut at = 0;
    while at < bytes.len() {
        let byte = bytes[at];
        let next = bytes.get(at + 1).copied();
        if line_comment {
            if byte == b'\n' {
                line_comment = false;
            }
            at += 1;
            continue;
        }
        if block_comment {
            if byte == b'*' && next == Some(b'/') {
                block_comment = false;
                at += 2;
            } else {
                at += 1;
            }
            continue;
        }
        if let Some(delimiter) = quote {
            if byte == b'\\' {
                at += 2;
                continue;
            }
            if byte == delimiter {
                quote = None;
            }
            at += 1;
            continue;
        }
        match byte {
            b'"' | b'\'' | b'`' => {
                quote = Some(byte);
                at += 1;
            }
            b'/' if next == Some(b'/') => {
                line_comment = true;
                at += 2;
            }
            b'/' if next == Some(b'*') => {
                block_comment = true;
                at += 2;
            }
            b'(' | b'[' | b'{' => {
                open.push((at, byte));
                at += 1;
            }
            b')' | b']' | b'}' => {
                let matches = matches!(
                    (open.last().map(|(_, open)| *open), byte),
                    (Some(b'('), b')') | (Some(b'['), b']') | (Some(b'{'), b'}')
                );
                if matches && let Some((start, _)) = open.pop() {
                    pairs.push((start, at));
                }
                at += 1;
            }
            _ => at += 1,
        }
    }
    pairs.sort_by_key(|(start, _)| *start);
    pairs
}

#[cfg(test)]
mod tests {
    use super::{element_list_at, token_at};

    fn elements(text: &str, caret: usize) -> Option<(Vec<String>, usize)> {
        element_list_at(text, caret).map(|(ranges, index)| {
            (
                ranges
                    .into_iter()
                    .map(|range| text[range].to_owned())
                    .collect(),
                index,
            )
        })
    }

    #[test]
    fn the_elements_of_a_call_are_split_by_top_level_commas() {
        let text = "f(a, b(c, d), e)";
        let caret = text.find('a').unwrap();
        let (found, index) = elements(text, caret).unwrap();
        assert_eq!(found, ["a", "b(c, d)", "e"]);
        assert_eq!(index, 0);
    }

    #[test]
    fn a_list_after_a_word_keeps_the_text_before_it_out() {
        let text = "class A implements B, C {\n}";
        let caret = text.find(" B").unwrap() + 2;
        let (found, index) = elements(text, caret).unwrap();
        assert_eq!(found, ["B", "C"]);
        assert_eq!(index, 0);
    }

    #[test]
    fn the_token_at_a_caret_stays_inside_its_element() {
        let text = "int value";
        let token = token_at(text, 5, 0..9);
        assert_eq!(&text[token], "value");
        assert_eq!(token_at(text, 2, 0..9), 0..3);
    }
}
