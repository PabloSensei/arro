//! A run of text with the places of a search coloured. A row that draws the parts as
//! children of a flex row stands them apart by the gap of the row, so the whole text is
//! one element with ranges instead.

use std::ops::Range;

use gpui_kit::*;

/// `text` as one run of text with `ranges` in `style`.
///
/// The ranges count bytes, as the search of the editor gives them. A range that starts or
/// ends inside a character is cut to the character boundaries, so a match never breaks a
/// character of the text; ranges that overlap or touch become one.
pub fn highlighted_text(
    text: impl Into<SharedString>,
    ranges: impl IntoIterator<Item = Range<usize>>,
    style: HighlightStyle,
) -> StyledText {
    let text = text.into();
    let ranges = highlight_ranges(&text, ranges);
    StyledText::new(text).with_highlights(ranges.into_iter().map(|range| (range, style)))
}

/// The byte ranges of `text` that `ranges` marks: cut to the character boundaries of the
/// text, in order, without overlaps.
///
/// A [`StyledText`] wants its ranges so. The input may come from a search over bytes and
/// still name the inside of a character when the text changed under it.
pub fn highlight_ranges(
    text: &str,
    ranges: impl IntoIterator<Item = Range<usize>>,
) -> Vec<Range<usize>> {
    let len = text.len();
    let mut found: Vec<Range<usize>> = ranges
        .into_iter()
        .filter_map(|range| {
            let start = char_boundary_up(text, range.start.min(len));
            let end = char_boundary_down(text, range.end.min(len));
            (start < end).then_some(start..end)
        })
        .collect();
    found.sort_by_key(|range| (range.start, range.end));
    let mut merged: Vec<Range<usize>> = Vec::new();
    for range in found {
        match merged.last_mut() {
            Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
            _ => merged.push(range),
        }
    }
    merged
}

/// The first character boundary of `text` at or after `at`.
fn char_boundary_up(text: &str, at: usize) -> usize {
    let mut at = at;
    while at < text.len() && !text.is_char_boundary(at) {
        at += 1;
    }
    at
}

/// The last character boundary of `text` at or before `at`.
fn char_boundary_down(text: &str, at: usize) -> usize {
    let mut at = at;
    while at > 0 && !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

#[cfg(test)]
mod tests {
    use super::{highlight_ranges, highlighted_text};
    use gpui_kit::HighlightStyle;

    /// Every range starts and ends on a character of `text`, and none of them run into
    /// the next one.
    fn assert_whole_characters(text: &str, ranges: &[std::ops::Range<usize>]) {
        for range in ranges {
            assert!(
                text.is_char_boundary(range.start),
                "{range:?} starts in a character"
            );
            assert!(
                text.is_char_boundary(range.end),
                "{range:?} ends in a character"
            );
            assert!(range.start < range.end, "{range:?} is not empty");
            assert!(
                text.get(range.clone()).is_some(),
                "{range:?} lies inside the text"
            );
        }
        assert!(
            ranges.windows(2).all(|pair| pair[0].end <= pair[1].start),
            "{ranges:?} overlap or stand out of order"
        );
    }

    #[test]
    fn the_bytes_of_a_match_inside_a_cyrillic_character_become_whole_characters() {
        // "Привет" holds two-byte characters, so most byte offsets fall inside one.
        let text = "Привет, мир";
        let ranges = highlight_ranges(text, [1..4, 13..16]);
        assert_eq!(ranges, vec![2..4, 13..16]);
        assert_whole_characters(text, &ranges);
        assert_eq!(&text[ranges[0].clone()], "р");
    }

    #[test]
    fn the_bytes_of_a_match_inside_an_emoji_keep_the_emoji_whole() {
        // The emoji takes four bytes, so a range inside it names no whole character and is
        // dropped rather than shown as a broken glyph.
        let text = "hi 🌍 there 🌍";
        let ranges = highlight_ranges(text, [4..6, 8..13, 15..17, 14..18]);
        assert_eq!(ranges, vec![8..13, 14..18]);
        assert_whole_characters(text, &ranges);
        assert_eq!(&text[ranges[0].clone()], "there");
        assert_eq!(&text[ranges[1].clone()], "🌍");
    }

    #[test]
    fn ranges_that_overlap_or_touch_become_one() {
        let text = "abcdef";
        assert_eq!(
            highlight_ranges(text, [0..2, 1..4, 4..5, 9..12]),
            vec![0..5]
        );
    }

    #[test]
    fn the_highlighted_text_keeps_the_letters_of_cyrillic_and_emoji_together() {
        let text = "Привет 🌍";
        // Ranges inside the text and one past its end: none of them may break a character
        // or reach outside, as `StyledText` checks both.
        let _ = highlighted_text(
            text,
            [1..4, 8..12, 13..16, 20..30],
            HighlightStyle::default(),
        );
    }
}
