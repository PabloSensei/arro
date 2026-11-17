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

/// The dots of `line` between `start` and its end that stand at the outermost level,
/// outside brackets, braces and literals.
fn top_level_dots(line: &str, start: usize) -> Vec<usize> {
    let bytes = line.as_bytes();
    let mut depth = 0i32;
    let mut quote: Option<u8> = None;
    let mut escaped = false;
    let mut dots = Vec::new();
    for at in start..line.len() {
        let byte = bytes[at];
        if let Some(mark) = quote {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == mark {
                quote = None;
            }
            continue;
        }
        match byte {
            b'"' | b'\'' => quote = Some(byte),
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth = (depth - 1).max(0),
            b'.' if depth == 0 => {
                let after_digit = bytes.get(at + 1).is_some_and(u8::is_ascii_digit);
                let before_digit = at > 0 && bytes[at - 1].is_ascii_digit();
                if !(after_digit && before_digit) {
                    dots.push(at);
                }
            }
            _ => {}
        }
    }
    dots
}

/// A postfix key at the end of `line`: the expressions in front of its dot, the longest
/// first, the key typed after the dot, and where that key starts. The key is empty when
/// only the dot is typed.
pub fn postfix_typed<'a>(
    line: &'a str,
    prefixes: &[&str],
) -> Option<(Vec<Range<usize>>, &'a str, usize)> {
    let key_start = word_before(line).map_or(line.len(), |word| word.start);
    let dot = if key_start > 0 && line.as_bytes()[key_start - 1] == b'.' {
        key_start - 1
    } else if line.ends_with('.') {
        line.len() - 1
    } else {
        return None;
    };
    let before = &line[..dot];
    let full = receiver_start(before, prefixes)?;
    let mut starts = vec![full];
    for at in top_level_dots(before, full) {
        if let Some(skip) =
            before[at + 1..].find(|character: char| !character.is_whitespace() && character != '.')
        {
            let start = at + 1 + skip;
            if start < dot && !starts.contains(&start) {
                starts.push(start);
            }
        }
    }
    starts.sort_unstable();
    let receivers = starts.into_iter().map(|start| start..dot).collect();
    Some((receivers, &line[key_start..], key_start))
}

/// A postfix key at the end of `line`: the range of the longest expression, and the key
/// typed after its dot.
pub fn postfix_at<'a>(line: &'a str, prefixes: &[&str]) -> Option<(Range<usize>, &'a str)> {
    let (receivers, key, _) = postfix_typed(line, prefixes)?;
    if key.is_empty() {
        return None;
    }
    Some((receivers.into_iter().next()?, key))
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

    fn candidates(line: &str) -> Option<(Vec<&str>, &str, usize)> {
        let (ranges, key, start) = postfix_typed(line, &["new"])?;
        let ranges: Vec<&str> = ranges.into_iter().map(|range| &line[range]).collect();
        Some((ranges, key, start))
    }

    #[test]
    fn a_postfix_key_lists_the_expressions_it_can_follow() {
        assert_eq!(
            candidates("a.b().c.var"),
            Some((
                vec!["a.b().c", "b().c", "c"],
                "var",
                "a.b().c.var".len() - "var".len()
            ))
        );
        assert_eq!(
            candidates("    names[i + 1].length.var"),
            Some((
                vec!["names[i + 1].length", "length"],
                "var",
                "    names[i + 1].length.var".len() - 3
            ))
        );
        assert_eq!(
            candidates("    new Pet(\"a.b\").var"),
            Some((
                vec!["new Pet(\"a.b\")"],
                "var",
                "    new Pet(\"a.b\").var".len() - 3
            ))
        );
        assert_eq!(
            candidates("    1.5.var"),
            Some((vec!["1.5"], "var", "    1.5.var".len() - 3))
        );
        assert_eq!(candidates("    list."), Some((vec!["list"], "", 9)));
        assert_eq!(candidates("    .var"), None);
    }
}
