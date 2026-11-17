//! `check-fields`: a `TextField` is drawn by the one renderer of `ui-kit`, so its caret
//! and its selection are on screen wherever the user types. The check is a text check
//! with two rules about the files that hold a field:
//!
//! - such a file calls `text_field_children` or `text_field_lines`, the renderers of
//!   `ui-kit`;
//! - a field of the view is not put into `child` or `children` as a plain string, as in
//!   `child(self.message.text.clone())`.
//!
//! A local that holds the text of a field, or a helper that takes `&str`, cannot be told
//! from any other text without types: a placeholder branch draws the same string and is
//! right. The window tests of the fields cover that case.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use crate::Result;
use crate::tooltips::{Kind, Token, relative, sources};

/// The sources of the fields: identifiers, punctuation, string and character literals.
/// Whitespace and comments are left out, so `let` is a word of its own and a lifetime is
/// not mistaken for a character literal.
fn source_tokens(text: &str) -> Vec<Token> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let start = at;
        match bytes[at] {
            byte if byte.is_ascii_whitespace() => {
                at += 1;
            }
            b'"' => {
                at += 1;
                while at < bytes.len() {
                    if bytes[at] == b'\\' {
                        at += 2;
                    } else if bytes[at] == b'"' {
                        at += 1;
                        break;
                    } else {
                        at += 1;
                    }
                }
                out.push(Token {
                    kind: Kind::Word,
                    start,
                    end: at.min(bytes.len()),
                });
            }
            b'\'' => {
                // A character literal is `'x'` or `'\n'`; `'static` is a lifetime.
                let literal = match bytes.get(at + 1) {
                    Some(b'\\') => true,
                    Some(_) => bytes.get(at + 2) == Some(&b'\''),
                    None => false,
                };
                if literal {
                    at += 1;
                    while at < bytes.len() {
                        if bytes[at] == b'\\' {
                            at += 2;
                        } else if bytes[at] == b'\'' {
                            at += 1;
                            break;
                        } else {
                            at += 1;
                        }
                    }
                    out.push(Token {
                        kind: Kind::Word,
                        start,
                        end: at.min(bytes.len()),
                    });
                } else {
                    at += 1;
                    out.push(Token {
                        kind: Kind::Punct('\''),
                        start,
                        end: at,
                    });
                }
            }
            b'/' if bytes.get(at + 1) == Some(&b'/') => {
                while at < bytes.len() && bytes[at] != b'\n' {
                    at += 1;
                }
            }
            b'/' if bytes.get(at + 1) == Some(&b'*') => {
                at += 2;
                while at + 1 < bytes.len() && !(bytes[at] == b'*' && bytes[at + 1] == b'/') {
                    at += 1;
                }
                at = (at + 2).min(bytes.len());
            }
            byte if is_punctuation(byte) => {
                at += 1;
                out.push(Token {
                    kind: Kind::Punct(byte as char),
                    start,
                    end: at,
                });
            }
            _ => {
                while at < bytes.len()
                    && !is_punctuation(bytes[at])
                    && !bytes[at].is_ascii_whitespace()
                    && bytes[at] != b'"'
                    && bytes[at] != b'\''
                {
                    at += 1;
                }
                out.push(Token {
                    kind: Kind::Word,
                    start,
                    end: at,
                });
            }
        }
    }
    out
}

fn is_punctuation(byte: u8) -> bool {
    matches!(
        byte,
        b'(' | b')'
            | b'{'
            | b'}'
            | b'['
            | b']'
            | b';'
            | b','
            | b'.'
            | b'|'
            | b'='
            | b'>'
            | b'<'
            | b'*'
            | b'&'
            | b'!'
            | b'?'
            | b':'
    )
}

/// The crates whose fields are drawn. `ui-kit` holds the renderer itself and is left out.
const CRATES: &[&str] = &["crates/ui/panels/src", "crates/ui/editor-view/src"];

/// The renderers a file with a field calls.
const RENDERERS: &[&str] = &[
    "text_field_children",
    "text_field_children_named",
    "text_field_lines",
    "text_field_lines_named",
];

pub fn run(root: &Path) -> Result {
    let mut found = Vec::new();
    for dir in CRATES {
        for file in sources(&root.join(dir))? {
            if excluded(&file) {
                continue;
            }
            let text = fs::read_to_string(&file)?;
            for (line, message) in raw_fields(&text) {
                found.push(format!("{}:{line}: {message}", relative(root, &file)));
            }
        }
    }
    if found.is_empty() {
        return Ok(());
    }
    Err(format!(
        "{}\nda TextField is drawn by ui_kit::text_field_children(..) or \
         ui_kit::text_field_lines(..), never by its text alone",
        found.join("\n")
    )
    .into())
}

fn excluded(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.contains("tests"))
}

fn word<'a>(text: &'a str, token: &Token) -> &'a str {
    text[token.start..token.end].trim()
}

fn is_word(text: &str, token: Option<&Token>, value: &str) -> bool {
    token.is_some_and(|token| token.kind == Kind::Word && word(text, token) == value)
}

fn is_punct(token: Option<&Token>, value: char) -> bool {
    token.is_some_and(|token| token.kind == Kind::Punct(value))
}

fn line_of(text: &str, at: usize) -> usize {
    text[..at].bytes().filter(|byte| *byte == b'\n').count() + 1
}

/// The names of the `TextField` members of the structs of the file.
fn field_names<'a>(text: &'a str, tokens: &[Token]) -> BTreeSet<&'a str> {
    let mut names = BTreeSet::new();
    for (index, token) in tokens.iter().enumerate() {
        if token.kind != Kind::Word || word(text, token) != "TextField" || index < 2 {
            continue;
        }
        if is_punct(tokens.get(index - 1), ':') && tokens[index - 2].kind == Kind::Word {
            names.insert(word(text, &tokens[index - 2]));
        }
    }
    names
}

/// Whether the tokens read the text of one of the fields: `self.<field>.text`.
fn reads_field_text(text: &str, tokens: &[Token], names: &BTreeSet<&str>) -> bool {
    for index in 0..tokens.len() {
        if !is_word(text, tokens.get(index), "self")
            || !is_punct(tokens.get(index + 1), '.')
            || !tokens
                .get(index + 2)
                .is_some_and(|token| token.kind == Kind::Word && names.contains(word(text, token)))
            || !is_punct(tokens.get(index + 3), '.')
            || !is_word(text, tokens.get(index + 4), "text")
        {
            continue;
        }
        return true;
    }
    false
}

/// One-based line and message of every place that draws a field as a plain string.
fn raw_fields(text: &str) -> Vec<(usize, String)> {
    let tokens = source_tokens(text);
    if !tokens
        .iter()
        .any(|token| is_word(text, Some(token), "TextField"))
    {
        return Vec::new();
    }
    let names = field_names(text, &tokens);
    let mut found = Vec::new();

    for (index, token) in tokens.iter().enumerate() {
        if token.kind != Kind::Punct('.')
            || !(is_word(text, tokens.get(index + 1), "child")
                || is_word(text, tokens.get(index + 1), "children"))
            || !is_punct(tokens.get(index + 2), '(')
        {
            continue;
        }
        let mut depth = 1usize;
        let mut end = index + 3;
        while let Some(token) = tokens.get(end) {
            match token.kind {
                Kind::Punct('(') => depth += 1,
                Kind::Punct(')') => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
            end += 1;
        }
        if reads_field_text(text, &tokens[index + 3..end], &names) {
            found.push((
                line_of(text, token.start),
                "the field is drawn as a plain string, so it has no caret and no selection"
                    .to_owned(),
            ));
        }
    }

    if !tokens.iter().any(|token| {
        RENDERERS
            .iter()
            .any(|name| is_word(text, Some(token), name))
    }) {
        let at = tokens
            .iter()
            .find(|token| is_word(text, Some(token), "TextField"))
            .map_or(0, |token| token.start);
        found.push((
            line_of(text, at),
            "a file with a TextField calls neither text_field_children nor text_field_lines"
                .to_owned(),
        ));
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    const VIEW: &str = "struct View {\n    message: TextField,\n}\n";

    #[test]
    fn a_field_put_into_a_child_is_found() {
        let text = format!(
            "{VIEW}fn render(&self) {{\n    div().child(self.message.text.clone())\n        \
             .children(text_field_children(&self.message, a, b, c, d, 16., true))\n}}\n"
        );
        assert_eq!(
            raw_fields(&text),
            vec![(
                5,
                "the field is drawn as a plain string, so it has no caret and no selection"
                    .to_owned()
            )]
        );
    }

    #[test]
    fn a_placeholder_branch_of_a_local_is_left_alone() {
        let text = format!(
            "{VIEW}fn render(&self) {{\n    let shown = self.message.text.clone();\n    \
             div().child(shown)\n        .children(text_field_children(&self.message, a, b, c, d, \
             16., true))\n}}\n"
        );
        assert!(raw_fields(&text).is_empty());
    }

    #[test]
    fn the_renderers_clear_the_field() {
        let text = format!(
            "{VIEW}fn render(&self) {{\n    div().children(text_field_children_named(\
             \"view-field\", &self.message, a, b, c, d, 16., true))\n}}\n"
        );
        assert!(raw_fields(&text).is_empty());
    }

    #[test]
    fn the_text_of_another_struct_is_left_alone() {
        let text = format!(
            "{VIEW}fn render(&self) {{\n    div().child(line.text.clone())\n    \
             .children(text_field_children(&self.message, a, b, c, d, 16., true))\n}}\n"
        );
        assert!(raw_fields(&text).is_empty());
    }

    #[test]
    fn a_file_without_a_field_is_left_alone() {
        assert!(raw_fields("fn render() { div().child(line.text.clone()) }\n").is_empty());
    }

    #[test]
    fn a_field_without_a_renderer_is_found() {
        let text = format!("{VIEW}fn render(&self) {{ div().child(self.message.text.clone()) }}\n");
        let messages: Vec<String> = raw_fields(&text)
            .into_iter()
            .map(|(_, message)| message)
            .collect();
        assert_eq!(messages.len(), 2);
        assert!(messages[1].contains("calls neither text_field_children"));
    }

    #[test]
    fn the_ui_crates_pass() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        run(&root).unwrap();
    }
}
