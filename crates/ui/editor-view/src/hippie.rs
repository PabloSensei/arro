//! Hippie completion: a word already in the file, or in another open buffer, that
//! starts with the text before the caret. The editor needs no language for it.

use std::ops::Range;

use crate::semantic::is_word;

/// Every word of `text`, as byte ranges in the order they appear.
pub(crate) fn tokens(text: &str) -> Vec<Range<usize>> {
    let mut tokens = Vec::new();
    let mut start = None;
    for (at, character) in text.char_indices() {
        if is_word(character) {
            start.get_or_insert(at);
        } else if let Some(start) = start.take() {
            tokens.push(start..at);
        }
    }
    if let Some(start) = start {
        tokens.push(start..text.len());
    }
    tokens
}

/// The words of `text` that start with `prefix`, nearest to the caret first: the ones
/// above the typed text from the closest, then the ones below it from the closest.
pub(crate) fn around(
    text: &str,
    prefix_start: usize,
    caret: usize,
    prefix: &str,
) -> Vec<Range<usize>> {
    let matching = |range: &Range<usize>| text[range.clone()].starts_with(prefix);
    let mut above: Vec<Range<usize>> = tokens(text)
        .into_iter()
        .filter(|range| range.end <= prefix_start && matching(range))
        .collect();
    above.sort_by_key(|range| std::cmp::Reverse(range.start));
    let mut below: Vec<Range<usize>> = tokens(text)
        .into_iter()
        .filter(|range| range.start >= caret && matching(range))
        .collect();
    below.sort_by_key(|range| range.start);
    above.extend(below);
    above
}

/// The words of another buffer, in the order of the text.
pub(crate) fn all(text: &str, prefix: &str) -> Vec<Range<usize>> {
    tokens(text)
        .into_iter()
        .filter(|range| text[range.clone()].starts_with(prefix))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{all, around};

    #[test]
    fn words_above_the_caret_come_before_words_below_it() {
        let text = "alpha\nbeta\nal\navocado\nalgebra\n";
        let caret = text.find("al\n").unwrap() + 2;
        let found: Vec<String> = around(text, caret - 2, caret, "al")
            .into_iter()
            .map(|range| text[range].to_owned())
            .collect();
        assert_eq!(found, ["alpha", "algebra"]);
    }

    #[test]
    fn the_word_under_the_caret_is_not_a_candidate() {
        let text = "alphabet\n";
        let found = around(text, 0, 2, "al");
        assert!(found.is_empty());
    }

    #[test]
    fn every_buffer_is_read_in_its_own_order() {
        let found: Vec<String> = all("one\ntwo\nthree\n", "t")
            .into_iter()
            .map(|range| "one\ntwo\nthree\n"[range].to_owned())
            .collect();
        assert_eq!(found, ["two", "three"]);
    }
}
