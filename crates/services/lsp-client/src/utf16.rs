//! Conversions between byte offsets in UTF-8 text and LSP positions in UTF-16 units.
//!
//! Lines end at `\n`, `\r\n` or `\r`, as the protocol says.

use lsp_types::{Position, Range, TextDocumentContentChangeEvent};

/// The line containing `offset` starts here; `line` counts terminators before it.
fn line_start(text: &str, offset: usize) -> (u32, usize) {
    let bytes = text.as_bytes();
    let mut line = 0;
    let mut start = 0;
    let mut at = 0;
    while at < offset.min(bytes.len()) {
        let terminator = match bytes[at] {
            b'\n' => 1,
            b'\r' if bytes.get(at + 1) == Some(&b'\n') => 2,
            b'\r' => 1,
            _ => 0,
        };
        at += terminator.max(1);
        if terminator > 0 && at <= offset {
            line += 1;
            start = at;
        }
    }
    (line, start)
}

/// The position of byte `offset`, rounded down to a character boundary.
pub fn position_at(text: &str, offset: usize) -> Position {
    let offset = text.floor_char_boundary(offset);
    let (line, start) = line_start(text, offset);
    let character = text[start..offset].encode_utf16().count();
    Position::new(line, character as u32)
}

/// The byte offset of `position`.
///
/// A column past the line end means the line end, a line past the text means
/// the text end. A column inside a surrogate pair rounds down to the character.
pub fn offset_at(text: &str, position: Position) -> usize {
    let bytes = text.as_bytes();
    let mut line = 0;
    let mut at = 0;
    while line < position.line {
        match bytes.get(at) {
            None => return text.len(),
            Some(b'\n') => {
                at += 1;
                line += 1;
            }
            Some(b'\r') => {
                at += if bytes.get(at + 1) == Some(&b'\n') {
                    2
                } else {
                    1
                };
                line += 1;
            }
            Some(_) => at += 1,
        }
    }
    let mut units = 0;
    for (index, character) in text[at..].char_indices() {
        if character == '\n' || character == '\r' {
            return at + index;
        }
        units += character.len_utf16() as u32;
        if units > position.character {
            return at + index;
        }
    }
    text.len()
}

/// The incremental change that replaces bytes `start..end` of `before`.
pub fn change_event(
    before: &str,
    start: usize,
    end: usize,
    text: String,
) -> TextDocumentContentChangeEvent {
    TextDocumentContentChangeEvent {
        range: Some(Range::new(
            position_at(before, start),
            position_at(before, end),
        )),
        range_length: None,
        text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn position(line: u32, character: u32) -> Position {
        Position::new(line, character)
    }

    #[test]
    fn counts_utf16_units_not_bytes_or_chars() {
        let text = "a😀ж b";
        let offset = text.find('b').unwrap();
        assert_eq!(position_at(text, offset), position(0, 5));
        assert_eq!(offset_at(text, position(0, 5)), offset);
    }

    #[test]
    fn every_terminator_starts_a_line() {
        let text = "a\nb\r\nc\rd";
        assert_eq!(position_at(text, text.find('b').unwrap()), position(1, 0));
        assert_eq!(position_at(text, text.find('c').unwrap()), position(2, 0));
        assert_eq!(position_at(text, text.find('d').unwrap()), position(3, 0));
        assert_eq!(offset_at(text, position(3, 0)), text.find('d').unwrap());
    }

    #[test]
    fn crlf_is_one_terminator_even_between_its_bytes() {
        let text = "ab\r\ncd";
        assert_eq!(position_at(text, 3), position(0, 3));
        assert_eq!(position_at(text, 4), position(1, 0));
    }

    #[test]
    fn roundtrip_at_every_boundary() {
        let text = "class Ж {\r\n  String s = \"😀😀\"; int x;\n\r\n}\n";
        for offset in (0..=text.len()).filter(|at| text.is_char_boundary(*at)) {
            let on_terminator = matches!(text.as_bytes().get(offset), Some(b'\n'))
                && offset > 0
                && text.as_bytes()[offset - 1] == b'\r';
            if !on_terminator {
                assert_eq!(
                    offset_at(text, position_at(text, offset)),
                    offset,
                    "{offset}"
                );
            }
        }
    }

    #[test]
    fn out_of_range_positions_clamp() {
        let text = "ab\ncd";
        assert_eq!(offset_at(text, position(0, 99)), 2);
        assert_eq!(offset_at(text, position(1, 99)), 5);
        assert_eq!(offset_at(text, position(9, 0)), 5);
    }

    #[test]
    fn middle_of_surrogate_pair_rounds_down() {
        let text = "😀a";
        assert_eq!(offset_at(text, position(0, 1)), 0);
        assert_eq!(offset_at(text, position(0, 2)), 4);
    }

    #[test]
    fn offset_inside_a_character_rounds_down() {
        let text = "😀a";
        assert_eq!(position_at(text, 2), position(0, 0));
    }

    #[test]
    fn empty_text() {
        assert_eq!(position_at("", 0), position(0, 0));
        assert_eq!(offset_at("", position(3, 3)), 0);
    }

    #[test]
    fn change_event_uses_positions_of_the_old_text() {
        let change = change_event("a😀\nbc", 7, 8, "x".into());
        let range = change.range.unwrap();
        assert_eq!(range.start, position(1, 1));
        assert_eq!(range.end, position(1, 2));
    }
}
