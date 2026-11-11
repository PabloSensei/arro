//! What was typed in front of the caret: a key, and for a postfix key its expression.

use std::ops::Range;

fn is_word(character: char) -> bool {
    character.is_alphanumeric() || character == '_'
}

/// The word that ends at the end of `line`, as a byte range.
pub fn word_before(line: &str) -> Option<Range<usize>> {
    let start = line
        .char_indices()
        .rev()
        .take_while(|(_, character)| is_word(*character))
        .last()
        .map(|(at, _)| at)?;
    Some(start..line.len())
}

/// Skips back over a bracketed group that ends right before `end`, returning its start.
fn group_start(bytes: &[u8], end: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut at = end;
    while at > 0 {
        at -= 1;
        match bytes[at] {
            b')' | b']' => depth += 1,
            b'(' | b'[' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(at);
                }
            }
            b'"' | b'\'' => at = quoted_start(bytes, at + 1)?,
            _ => {}
        }
    }
    None
}

/// The opening quote of a literal whose closing quote is right before `end`.
fn quoted_start(bytes: &[u8], end: usize) -> Option<usize> {
    let quote = bytes[end - 1];
    let mut at = end - 1;
    while at > 0 {
        at -= 1;
        let escaped = at > 0 && bytes[at - 1] == b'\\';
        if bytes[at] == quote && !escaped {
            return Some(at);
        }
    }
    None
}

/// Where the expression that ends at the end of `line` starts: names, calls, indexing,
/// member access and literals, and a word from `prefixes` in front of them, like `new`.
pub fn receiver_start(line: &str, prefixes: &[&str]) -> Option<usize> {
    let bytes = line.as_bytes();
    let mut start = line.len();
    loop {
        let before = &line[..start];
        match before.chars().next_back() {
            Some(')') | Some(']') => {
                start = group_start(bytes, start)?;
                // Type arguments of a constructor call, like `ArrayList<String>()`.
                if line[..start].ends_with('>') {
                    start = line[..start].rfind('<')?;
                }
            }
            Some('"') | Some('\'') => start = quoted_start(bytes, start)?,
            Some(character) if is_word(character) => {
                start = word_before(before).map_or(start, |word| word.start);
            }
            _ => break,
        }
        let before = &line[..start];
        match before.chars().next_back() {
            Some('.') => start -= 1,
            // A call or an index directly after a name, like `get(0)` or `items[1]`.
            Some(character) if is_word(character) && line[start..].starts_with(['(', '[', '<']) => {
            }
            _ => break,
        }
    }
    let start = line[start..]
        .find(|character: char| !character.is_whitespace() && character != '.')
        .map(|skip| start + skip)?;
    if start >= line.len() {
        return None;
    }
    let before = line[..start].trim_end();
    for prefix in prefixes {
        if let Some(head) = before.strip_suffix(prefix)
            && !head.ends_with(is_word)
            && before.len() < start
        {
            return Some(head.len());
        }
    }
    Some(start)
}

/// A postfix key at the end of `line`: the range of the expression, and the key typed after
/// its dot.
pub fn postfix_at<'a>(line: &'a str, prefixes: &[&str]) -> Option<(Range<usize>, &'a str)> {
    let key = word_before(line)?;
    let dot = key.start.checked_sub(1)?;
    if line.as_bytes()[dot] != b'.' {
        return None;
    }
    let start = receiver_start(&line[..dot], prefixes)?;
    Some((start..dot, &line[key]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receiver(line: &str) -> Option<&str> {
        let (range, _) = postfix_at(line, &["new"])?;
        Some(&line[range])
    }

    #[test]
    fn the_expression_in_front_of_a_postfix_key() {
        assert_eq!(receiver("    items.var"), Some("items"));
        assert_eq!(
            receiver("x = owner.getPets().get(0).var"),
            Some("owner.getPets().get(0)")
        );
        assert_eq!(
            receiver("    new ArrayList<>().var"),
            Some("new ArrayList<>()")
        );
        assert_eq!(receiver("    new Pet(\"a)\").var"), Some("new Pet(\"a)\")"));
        assert_eq!(
            receiver("    names[i + 1].length.var"),
            Some("names[i + 1].length")
        );
        assert_eq!(receiver("    \"text\".sout"), Some("\"text\""));
        assert_eq!(receiver("    .var"), None);
        assert_eq!(postfix_at("    list.size()", &[]), None);
    }

    #[test]
    fn the_word_before_the_caret() {
        assert_eq!(word_before("    sout"), Some(4..8));
        assert_eq!(word_before("    "), None);
    }
}
