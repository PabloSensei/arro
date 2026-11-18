//! The statements of a console file: where one ends and the next begins.
//!
//! A semicolon inside a string or a comment does not end a statement. The console runs the
//! selected text, or the statement the caret is in. The same reading finds the values a
//! statement asks for before it runs.

use std::collections::HashMap;

use crate::protocol::Param;

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

/// One value a statement asks for: a `?`, a `:name` or a `${name}`. The value is bound by
/// the driver, so the text of the statement is never changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Parameter {
    /// The name of the value in the dialog and in the memory of the console: the name of
    /// a named placeholder, `?1`, `?2` for the positions of `?`.
    pub key: String,
    /// The text of the placeholder, as the dialog shows it.
    pub label: String,
    /// The byte range of the placeholder in the statement.
    pub start: usize,
    pub end: usize,
}

/// The placeholders of a statement, in the order the driver binds them. A `?`, a `:name`
/// or a `${name}` inside a string or a comment is not one.
pub fn parameters(text: &str) -> Vec<Parameter> {
    let characters: Vec<(usize, char)> = text.char_indices().collect();
    let mut found = Vec::new();
    let mut state = State::Code;
    let mut index = 0;
    let mut position = 0;
    while index < characters.len() {
        let (offset, character) = characters[index];
        match state {
            State::Code => match character {
                '\'' | '"' | '`' => state = State::Quoted(character),
                '-' => {
                    if at(&characters, index + 1) == Some('-') {
                        index += 1;
                        state = State::LineComment;
                    }
                }
                '#' => state = State::LineComment,
                '/' => {
                    if at(&characters, index + 1) == Some('*') {
                        index += 1;
                        state = State::BlockComment;
                    }
                }
                '?' => {
                    position += 1;
                    let key = format!("?{position}");
                    found.push(Parameter {
                        key: key.clone(),
                        label: key,
                        start: offset,
                        end: offset + 1,
                    });
                }
                ':' => {
                    // `::` is a cast and `:=` is an assignment, not a value.
                    let next = at(&characters, index + 1);
                    if next.is_some_and(is_name_start) {
                        let end = name_end(&characters, index + 1);
                        let name: String = characters[index + 1..end]
                            .iter()
                            .map(|(_, character)| *character)
                            .collect();
                        found.push(Parameter {
                            key: name.clone(),
                            label: name,
                            start: offset,
                            end: characters[end - 1].0 + characters[end - 1].1.len_utf8(),
                        });
                        index = end - 1;
                    } else if next == Some(':') {
                        index += 1;
                    }
                }
                '$' if at(&characters, index + 1) == Some('{') => {
                    let mut end = index + 2;
                    while end < characters.len() && characters[end].1 != '}' {
                        end += 1;
                    }
                    if end < characters.len() {
                        let name: String = characters[index + 2..end]
                            .iter()
                            .map(|(_, character)| *character)
                            .collect();
                        if !name.trim().is_empty() {
                            found.push(Parameter {
                                key: name.clone(),
                                label: name,
                                start: offset,
                                end: characters[end].0 + 1,
                            });
                        }
                        index = end;
                    }
                }
                _ => {}
            },
            State::Quoted(quote) => {
                if character == quote {
                    if quote != '`' && at(&characters, index + 1) == Some(quote) {
                        index += 1;
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
                if character == '*' && at(&characters, index + 1) == Some('/') {
                    index += 1;
                    state = State::Code;
                }
            }
        }
        index += 1;
    }
    found
}

/// The placeholders of a statement with the repeats of a name left out: the dialog asks
/// for each name once.
pub fn asked_parameters(text: &str) -> Vec<Parameter> {
    let mut asked: Vec<Parameter> = Vec::new();
    for parameter in parameters(text) {
        if !asked.iter().any(|known| known.key == parameter.key) {
            asked.push(parameter);
        }
    }
    asked
}

/// The values of the placeholders in the order the driver binds them. A name the user did
/// not answer is empty text.
pub fn bind_parameters(text: &str, values: &HashMap<String, String>) -> Vec<Param> {
    parameters(text)
        .into_iter()
        .map(|parameter| match values.get(&parameter.key) {
            Some(value) => Param::parse_text(value),
            None => Param::Text(String::new()),
        })
        .collect()
}

fn at(characters: &[(usize, char)], index: usize) -> Option<char> {
    characters.get(index).map(|(_, character)| *character)
}

fn is_name_start(character: char) -> bool {
    character.is_alphabetic() || character == '_'
}

fn is_name(character: char) -> bool {
    character.is_alphanumeric() || character == '_'
}

/// The index of the character after the name that starts at `start`.
fn name_end(characters: &[(usize, char)], start: usize) -> usize {
    let mut end = start + 1;
    while end < characters.len() && is_name(characters[end].1) {
        end += 1;
    }
    end
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

    #[test]
    fn the_placeholders_of_a_statement_keep_their_order() {
        let text = "SELECT * FROM OWNERS WHERE ID = :id AND NAME = ? OR AGE = ${age}";
        let found = parameters(text);

        let keys: Vec<&str> = found
            .iter()
            .map(|parameter| parameter.key.as_str())
            .collect();
        assert_eq!(keys, ["id", "?1", "age"]);
        assert_eq!(found[0].label, "id");
        assert_eq!(&text[found[0].start..found[0].end], ":id");
        assert_eq!(&text[found[1].start..found[1].end], "?");
        assert_eq!(&text[found[2].start..found[2].end], "${age}");
        assert_eq!(asked_parameters(text).len(), 3);
    }

    #[test]
    fn a_placeholder_in_a_string_or_a_comment_is_text() {
        let text = "SELECT '? :id ${x}', \"?\" -- :no\nFROM t /* ? */ WHERE a = :yes";

        let found = parameters(text);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].key, "yes");
        assert_eq!(&text[found[0].start..found[0].end], ":yes");
    }

    #[test]
    fn a_cast_and_a_repeated_name_ask_nothing_extra() {
        let text = "SELECT a::text, b FROM t WHERE id = :id AND other = :id";

        let found = parameters(text);
        assert_eq!(found.len(), 2);
        assert_eq!(
            found
                .iter()
                .map(|parameter| parameter.key.as_str())
                .collect::<Vec<_>>(),
            ["id", "id"]
        );
        assert_eq!(asked_parameters(text).len(), 1);
        assert!(parameters("SELECT : and ${} and $x").is_empty());
    }

    #[test]
    fn the_values_are_bound_in_the_order_of_the_placeholders() {
        let text = "SELECT * FROM OWNERS WHERE ID = :id AND NAME = ?";
        let values = HashMap::from([
            ("id".to_owned(), "7".to_owned()),
            ("?1".to_owned(), "Ann".to_owned()),
        ]);

        assert_eq!(
            bind_parameters(text, &values),
            [Param::Integer(7), Param::Text("Ann".into())]
        );
    }
}
