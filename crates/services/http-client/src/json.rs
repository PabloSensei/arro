//! Indentation of a JSON body without reordering its keys, which a parse into a map
//! would do.

/// The text of `text` with one field or item per line. `None` when the text is not JSON,
/// whose brace, bracket and string rules are checked before the lines are laid out.
pub fn pretty_json(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() || serde_json::from_str::<serde_json::Value>(trimmed).is_err() {
        return None;
    }
    let mut out = String::with_capacity(trimmed.len() + trimmed.len() / 4);
    let mut indent = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    // One entry per open brace or bracket: whether its content takes more than one line.
    let mut opened: Vec<bool> = Vec::new();
    let mut characters = trimmed.chars().peekable();
    while let Some(character) = characters.next() {
        if in_string {
            out.push(character);
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_string = false;
            }
            continue;
        }
        match character {
            '"' => {
                in_string = true;
                out.push(character);
            }
            '{' | '[' => {
                out.push(character);
                let more = closes_later(&mut characters);
                opened.push(more);
                if more {
                    indent += 1;
                    newline(&mut out, indent);
                }
            }
            '}' | ']' => {
                let more = opened.pop().unwrap_or(true);
                if more {
                    indent = indent.saturating_sub(1);
                    newline(&mut out, indent);
                }
                out.push(character);
            }
            ',' => {
                out.push(character);
                newline(&mut out, indent);
            }
            ':' => {
                out.push(character);
                out.push(' ');
            }
            ' ' | '\t' | '\n' | '\r' => {}
            other => out.push(other),
        }
    }
    Some(out)
}

fn newline(out: &mut String, indent: usize) {
    out.push('\n');
    for _ in 0..indent {
        out.push_str("  ");
    }
}

/// Whether an opening brace or bracket is followed by something before its closing pair.
/// An empty object stays on one line.
fn closes_later(characters: &mut std::iter::Peekable<std::str::Chars<'_>>) -> bool {
    let mut copy = characters.clone();
    for character in copy.by_ref() {
        match character {
            ' ' | '\t' | '\n' | '\r' => {}
            '}' | ']' => return false,
            _ => return true,
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::pretty_json;

    #[test]
    fn the_keys_keep_their_order_and_the_items_their_place() {
        let text = r#"{"firstName": "Ann", "lastName": "Lee", "age": 30}"#;
        assert_eq!(
            pretty_json(text).unwrap(),
            "{\n  \"firstName\": \"Ann\",\n  \"lastName\": \"Lee\",\n  \"age\": 30\n}"
        );
    }

    #[test]
    fn an_array_of_objects_gets_its_own_lines() {
        let text = r#"[{"id":1},{"id":2}]"#;
        assert_eq!(
            pretty_json(text).unwrap(),
            "[\n  {\n    \"id\": 1\n  },\n  {\n    \"id\": 2\n  }\n]"
        );
    }

    #[test]
    fn an_empty_object_stays_on_one_line() {
        assert_eq!(pretty_json("{}").unwrap(), "{}");
    }

    #[test]
    fn a_string_keeps_its_spaces_and_braces() {
        let text = r#"{"note": "a { b }, c"}"#;
        assert_eq!(
            pretty_json(text).unwrap(),
            "{\n  \"note\": \"a { b }, c\"\n}"
        );
    }

    #[test]
    fn a_text_that_is_not_json_is_not_touched() {
        assert_eq!(pretty_json("<owner/>"), None);
        assert_eq!(pretty_json("hello"), None);
        assert_eq!(pretty_json(""), None);
    }
}
