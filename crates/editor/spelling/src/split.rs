//! Breaking a text into the words worth checking: identifiers by their humps and
//! separators, prose by its whitespace, with the things no dictionary should see left
//! out.

use std::ops::Range;

/// A word and the bytes it takes in the text it came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Word {
    pub start: usize,
    pub end: usize,
    pub text: String,
}

impl Word {
    pub fn range(&self) -> Range<usize> {
        self.start..self.end
    }
}

/// Every word of `text` that a dictionary can judge: at least three letters, not all
/// capitals, no digits, and no address or path around it.
pub fn words(text: &str) -> Vec<Word> {
    words_between(text, 0)
}

/// The same as [`words`], for a piece of a bigger text that starts at `base`.
pub fn words_between(text: &str, base: usize) -> Vec<Word> {
    let mut found = Vec::new();
    for (start, chunk) in chunks(text, base) {
        for word in split_identifier(&chunk, start) {
            if checkable(&word.text) {
                found.push(word);
            }
        }
    }
    found
}

/// The parts of one identifier or word: camelCase humps, snake_case pieces and the
/// alphabetic pieces between digits. `calcualteTotalHTTPServer2` gives `calcualte`,
/// `Total`, `HTTP` and `Server`.
pub fn split_identifier(token: &str, base: usize) -> Vec<Word> {
    let mut parts = Vec::new();
    let chars: Vec<(usize, char)> = token.char_indices().collect();
    let mut at = 0;
    while at < chars.len() {
        let start = chars[at].0;
        let mut end = at + 1;
        while end < chars.len() {
            let (_, previous) = chars[end - 1];
            let (_, current) = chars[end];
            if current == '_' || current.is_numeric() {
                break;
            }
            let next = chars.get(end + 1).map(|(_, character)| *character);
            let hump = !previous.is_lowercase()
                && previous.is_uppercase()
                && current.is_uppercase()
                && next.is_some_and(char::is_lowercase);
            if current.is_uppercase() && previous.is_lowercase() || hump {
                break;
            }
            end += 1;
        }
        let text = &token[start..chars[end - 1].0 + chars[end - 1].1.len_utf8()];
        parts.push(Word {
            start: base + start,
            end: base + start + text.len(),
            text: text.to_owned(),
        });
        at = end;
        while at < chars.len() && (chars[at].1 == '_' || chars[at].1.is_numeric()) {
            at += 1;
        }
    }
    parts
}

/// Whether a dictionary should be asked about `word`.
pub fn checkable(word: &str) -> bool {
    let mut letters = 0;
    let mut capitals = 0;
    let mut lowercase = 0;
    for character in word.chars() {
        if character.is_numeric() {
            return false;
        }
        if !character.is_alphabetic() {
            return false;
        }
        letters += 1;
        if character.is_uppercase() {
            capitals += 1;
        } else if character.is_lowercase() {
            lowercase += 1;
        }
    }
    letters >= 3 && (lowercase > 0 || capitals == 0)
}

/// The pieces of a text that are not whitespace, without the punctuation around them.
fn chunks(text: &str, base: usize) -> Vec<(usize, String)> {
    let mut found = Vec::new();
    let mut start = 0;
    for (at, character) in text.char_indices() {
        if character.is_whitespace() {
            if start < at {
                push_chunk(&mut found, &text[start..at], base + start);
            }
            start = at + character.len_utf8();
        }
    }
    if start < text.len() {
        push_chunk(&mut found, &text[start..], base + start);
    }
    found
}

fn push_chunk(found: &mut Vec<(usize, String)>, chunk: &str, base: usize) {
    let trimmed_start = chunk.len() - chunk.trim_start_matches(edge_punctuation).len();
    let trimmed = chunk
        .trim_start_matches(edge_punctuation)
        .trim_end_matches(edge_punctuation);
    if trimmed.is_empty() || is_address(trimmed) {
        return;
    }
    found.push((base + trimmed_start, trimmed.to_owned()));
}

/// Punctuation that is no part of a word at its edge. A dot stays out too, so a
/// sentence keeps its words.
fn edge_punctuation(character: char) -> bool {
    !character.is_alphanumeric() && character != '_'
}

/// Addresses, paths and hexadecimal numbers: nothing a dictionary knows.
fn is_address(chunk: &str) -> bool {
    if chunk.contains("://")
        || chunk.contains('/')
        || chunk.contains('\\')
        || chunk.contains('@')
        || chunk.starts_with("www.")
    {
        return true;
    }
    let lower = chunk.to_ascii_lowercase();
    if let Some(hex) = lower.strip_prefix("0x")
        && !hex.is_empty()
        && hex.chars().all(|character| character.is_ascii_hexdigit())
    {
        return true;
    }
    let digits = chunk.chars().filter(char::is_ascii_digit).count();
    let hexdigits = chunk
        .chars()
        .filter(|character| character.is_ascii_hexdigit())
        .count();
    hexdigits == chunk.chars().count() && chunk.len() >= 4 && (digits > 0 || chunk.len() >= 8)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(words: &[Word]) -> Vec<&str> {
        words.iter().map(|word| word.text.as_str()).collect()
    }

    #[test]
    fn an_identifier_splits_by_its_humps_and_its_digits() {
        let parts = split_identifier("calcualteTotalHTTPServer2", 0);
        assert_eq!(
            texts(&parts),
            ["calcualte", "Total", "HTTP", "Server"],
            "every part keeps its place"
        );
        assert_eq!(parts[0].start, 0);
        assert_eq!(parts[1].start, "calcualte".len());
        assert_eq!(
            texts(&split_identifier("jdbc_url", 0)),
            ["jdbc", "url"],
            "snake_case splits on the underscore"
        );
    }

    #[test]
    fn short_and_capital_words_never_reach_a_dictionary() {
        assert!(checkable("server"));
        assert!(checkable("Server"));
        assert!(!checkable("is"), "a word under three letters is no typo");
        assert!(!checkable("HTTP"));
        assert!(!checkable("JSON"));
        assert!(!checkable("abc1"));
        assert!(!checkable("snake_case"));
    }

    #[test]
    fn addresses_paths_hex_and_digits_are_left_out() {
        let text = "https://example.com/calcualte /usr/share/config 0xBEEF deadbeef \
                    C:\\Users\\pavel calcualte";
        assert_eq!(texts(&words(text)), ["calcualte"]);
    }

    #[test]
    fn the_offsets_point_at_the_text() {
        let text = "the calcualte is here";
        let words = words(text);
        assert_eq!(texts(&words), ["the", "calcualte", "here"]);
        for word in &words {
            assert_eq!(&text[word.range()], word.text);
        }
        assert_eq!(words[1].start, 4);
    }
}
