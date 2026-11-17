//! Soft wrap: where a long row breaks into several. The columns are characters, the
//! view works them out from the width of the window and of a character.

use std::ops::Range;

/// The pieces of `text` that make the display rows. A row breaks after the last blank that
/// fits, or in the middle of a word that has none. The pieces cover the text.
pub fn segments(text: &str, columns: usize) -> Vec<Range<usize>> {
    let columns = columns.max(2);
    let mut pieces = Vec::new();
    let mut start = 0;
    loop {
        let rest = &text[start..];
        // Byte where the first character that does not fit begins.
        let Some((limit, _)) = rest.char_indices().nth(columns) else {
            pieces.push(start..text.len());
            return pieces;
        };
        let fitting = &rest[..limit];
        let blank_end = fitting
            .char_indices()
            .filter(|(_, c)| c.is_whitespace())
            .map(|(at, c)| at + c.len_utf8())
            .rfind(|end| *end > 0);
        // A blank right at the limit is also a place to break.
        let at_limit = rest[limit..]
            .chars()
            .next()
            .is_some_and(char::is_whitespace)
            .then(|| limit + rest[limit..].chars().next().map_or(0, char::len_utf8));
        let cut = at_limit.or(blank_end).unwrap_or(limit);
        pieces.push(start..start + cut);
        start += cut;
        if start >= text.len() {
            return pieces;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::segments;

    fn cut(text: &str, columns: usize) -> Vec<&str> {
        segments(text, columns)
            .into_iter()
            .map(|range| &text[range])
            .collect()
    }

    #[test]
    fn a_short_line_stays_whole() {
        assert_eq!(cut("let a = 1;", 20), ["let a = 1;"]);
        assert_eq!(cut("", 20), [""]);
    }

    #[test]
    fn a_line_breaks_after_the_last_blank_that_fits() {
        assert_eq!(cut("one two three four", 9), ["one two ", "three ", "four"]);
    }

    #[test]
    fn a_word_without_blanks_breaks_at_the_limit() {
        assert_eq!(cut("abcdefghij", 4), ["abcd", "efgh", "ij"]);
    }

    #[test]
    fn the_pieces_cover_the_text_also_with_wide_characters() {
        let text = "\u{43f}\u{440}\u{438}\u{432}\u{435}\u{442} \u{43c}\u{438}\u{440} \u{447}\u{442}\u{43e}";
        let joined: String = cut(text, 5).concat();
        assert_eq!(joined, text);
    }
}
