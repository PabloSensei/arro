//! The statements of a console file: where one ends and the next begins.
//!
//! A semicolon inside a string or a comment does not end a statement. The console runs the
//! selected text, or the statement the caret is in.

/// Splits the text at the semicolons that end a statement.
pub fn split_statements(text: &str) -> Vec<String> {
    let mut statements = Vec::new();
    let mut start = 0;
    let mut state = State::Code;
    let mut characters = text.char_indices().peekable();
    while let Some((index, character)) = characters.next() {
        match state {
            State::Code => match character {
                '\'' | '"' | '`' => {
                    state = State::Quoted(character);
                }
                '-' => {
                    if characters.peek().is_some_and(|(_, next)| *next == '-') {
                        characters.next();
                        state = State::LineComment;
                    }
                }
                '#' => state = State::LineComment,
                '/' => {
                    if characters.peek().is_some_and(|(_, next)| *next == '*') {
                        characters.next();
                        state = State::BlockComment;
                    }
                }
                ';' => {
                    push(&mut statements, &text[start..index]);
                    start = index + character.len_utf8();
                }
                _ => {}
            },
            State::Quoted(quote) => {
                if character == quote {
                    if quote != '`' && characters.peek().is_some_and(|(_, next)| *next == quote) {
                        characters.next();
                    } else {
                        state = State::Code;
                    }
                }
            }
            State::LineComment => {
                if character == '\n' {
                    state = State::Code;
                }
            }
            State::BlockComment => {
                if character == '*' && characters.peek().is_some_and(|(_, next)| *next == '/') {
                    characters.next();
                    state = State::Code;
                }
            }
        }
    }
    push(&mut statements, &text[start..]);
    statements
}

fn push(statements: &mut Vec<String>, text: &str) {
    let statement = text.trim();
    if !statement.is_empty() {
        statements.push(statement.to_owned());
    }
}

#[derive(Clone, Copy)]
enum State {
    Code,
    Quoted(char),
    LineComment,
    BlockComment,
}

/// The statement that holds `line`, counted from zero.
pub fn statement_at(text: &str, line: usize) -> Option<String> {
    let mut line_start = 0;
    let mut current_line = 0;
    for (index, character) in text.char_indices() {
        if current_line == line {
            break;
        }
        if character == '\n' {
            current_line += 1;
            line_start = index + 1;
        }
    }
    let start = statement_start(text, line_start);
    let end = statement_end(&text[start..]);
    let statement = text[start..start + end].trim();
    (!statement.is_empty()).then(|| statement.to_owned())
}

/// The offset just after the last semicolon before `limit`.
fn statement_start(text: &str, limit: usize) -> usize {
    let before = &text[..limit.min(text.len())];
    let mut start = 0;
    let mut state = State::Code;
    let mut characters = before.char_indices().peekable();
    while let Some((index, character)) = characters.next() {
        match state {
            State::Code => match character {
                '\'' | '"' | '`' => state = State::Quoted(character),
                '-' => {
                    if characters.peek().is_some_and(|(_, next)| *next == '-') {
                        characters.next();
                        state = State::LineComment;
                    }
                }
                '#' => state = State::LineComment,
                '/' => {
                    if characters.peek().is_some_and(|(_, next)| *next == '*') {
                        characters.next();
                        state = State::BlockComment;
                    }
                }
                ';' => start = index + 1,
                _ => {}
            },
            State::Quoted(quote) => {
                if character == quote {
                    state = State::Code;
                }
            }
            State::LineComment => {
                if character == '\n' {
                    state = State::Code;
                }
            }
            State::BlockComment => {
                if character == '*' && characters.peek().is_some_and(|(_, next)| *next == '/') {
                    characters.next();
                    state = State::Code;
                }
            }
        }
    }
    start
}

/// The offset of the first semicolon outside a string or a comment, or the end.
fn statement_end(text: &str) -> usize {
    let mut state = State::Code;
    let mut characters = text.char_indices().peekable();
    while let Some((index, character)) = characters.next() {
        match state {
            State::Code => match character {
                '\'' | '"' | '`' => state = State::Quoted(character),
                '-' => {
                    if characters.peek().is_some_and(|(_, next)| *next == '-') {
                        characters.next();
                        state = State::LineComment;
                    }
                }
                '#' => state = State::LineComment,
                '/' => {
                    if characters.peek().is_some_and(|(_, next)| *next == '*') {
                        characters.next();
                        state = State::BlockComment;
                    }
                }
                ';' => return index,
                _ => {}
            },
            State::Quoted(quote) => {
                if character == quote {
                    state = State::Code;
                }
            }
            State::LineComment => {
                if character == '\n' {
                    state = State::Code;
                }
            }
            State::BlockComment => {
                if character == '*' && characters.peek().is_some_and(|(_, next)| *next == '/') {
                    characters.next();
                    state = State::Code;
                }
            }
        }
    }
    text.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semicolons_in_strings_and_comments_do_not_split() {
        let text = "SELECT ';' AS s; -- one; more\nSELECT 2 /* ; */;";

        assert_eq!(
            split_statements(text),
            ["SELECT ';' AS s", "-- one; more\nSELECT 2 /* ; */"]
        );
    }

    #[test]
    fn the_statement_of_the_caret_line_is_found() {
        let text = "SELECT 1;\n\nSELECT 2\nFROM DUAL;\nSELECT 3;";

        assert_eq!(statement_at(text, 0).as_deref(), Some("SELECT 1"));
        assert_eq!(
            statement_at(text, 2).as_deref(),
            Some("SELECT 2\nFROM DUAL")
        );
        assert_eq!(statement_at(text, 4).as_deref(), Some("SELECT 3"));
        assert_eq!(statement_at("", 0), None);
    }
}
