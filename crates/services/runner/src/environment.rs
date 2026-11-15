//! Environment variables of a run read from a file: `KEY=value` lines, `#` comments,
//! quotes around a value with spaces, and an optional `export` before the name.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use proc_sup::ProcessSpec;

/// Reads `KEY=value` lines. A value starts after the first `=`; quotes keep a space and
/// may hold `#`; an unquoted `#` after a space starts a comment. An empty value is a
/// variable set to an empty string.
pub fn parse_environment_file(text: &str) -> Result<BTreeMap<String, String>, String> {
    let mut variables = BTreeMap::new();
    for (index, line) in text.lines().enumerate() {
        let number = index + 1;
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = match line.strip_prefix("export") {
            Some(rest) if rest.starts_with(char::is_whitespace) => rest.trim_start(),
            _ => line,
        };
        let Some((name, value)) = line.split_once('=') else {
            return Err(format!("line {number}: expected NAME=value"));
        };
        let name = name.trim();
        if !is_name(name) {
            return Err(format!("line {number}: {name:?} is not a variable name"));
        }
        let value = parse_value(value).map_err(|reason| format!("line {number}: {reason}"))?;
        variables.insert(name.to_owned(), value);
    }
    Ok(variables)
}

/// The variables of a file. A missing file is an error: the user asked for its values.
pub fn read_environment_file(path: &Path) -> Result<BTreeMap<String, String>, String> {
    let text = fs::read_to_string(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    parse_environment_file(&text).map_err(|error| format!("{}: {error}", path.display()))
}

/// Adds the variables the specification does not set itself. A later value would win in
/// the child, so an explicit variable of the configuration stays as it is.
pub fn with_environment(
    mut spec: ProcessSpec,
    variables: &BTreeMap<String, String>,
) -> ProcessSpec {
    let set: Vec<String> = spec
        .environment()
        .iter()
        .map(|(name, _)| lowercase(name.to_string_lossy().as_ref()))
        .collect();
    for (name, value) in variables {
        if !set.contains(&lowercase(name)) {
            spec = spec.env(name, value);
        }
    }
    spec
}

/// Windows compares variable names without case, so a file cannot add a second spelling.
fn lowercase(name: &str) -> String {
    name.to_ascii_lowercase()
}

fn is_name(name: &str) -> bool {
    let mut characters = name.chars();
    match characters.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => {}
        _ => return false,
    }
    characters.all(|character| character.is_ascii_alphanumeric() || character == '_')
}

/// A value with the quotes taken off and a comment after it dropped.
fn parse_value(raw: &str) -> Result<String, String> {
    let value = raw.trim_start();
    // A `#` after the spaces that follow `=` starts a comment; one right after it is a
    // value of its own, as a password may begin with it.
    if value.starts_with('#') && raw.len() > value.len() {
        return Ok(String::new());
    }
    match value.chars().next() {
        Some('"') => {
            let (parsed, rest) = unquote(value, '"', true)?;
            after_quote(rest)?;
            Ok(parsed)
        }
        Some('\'') => {
            let (parsed, rest) = unquote(value, '\'', false)?;
            after_quote(rest)?;
            Ok(parsed)
        }
        _ => {
            let cut = value
                .char_indices()
                .find(|(index, character)| {
                    *character == '#'
                        && *index > 0
                        && value[..*index]
                            .chars()
                            .next_back()
                            .is_some_and(char::is_whitespace)
                })
                .map_or(value.len(), |(index, _)| index);
            Ok(value[..cut].trim_end().to_owned())
        }
    }
}

/// The text of a quoted value and what follows the closing quote.
fn unquote(value: &str, quote: char, escapes: bool) -> Result<(String, &str), String> {
    let mut parsed = String::new();
    let mut characters = value[1..].char_indices();
    while let Some((index, character)) = characters.next() {
        if character == quote {
            return Ok((parsed, &value[index + 1 + quote.len_utf8()..]));
        }
        if escapes && character == '\\' {
            match characters.next() {
                Some((_, 'n')) => parsed.push('\n'),
                Some((_, 't')) => parsed.push('\t'),
                Some((_, 'r')) => parsed.push('\r'),
                Some((_, escaped @ ('\\' | '"' | '\''))) => parsed.push(escaped),
                Some((_, other)) => {
                    parsed.push('\\');
                    parsed.push(other);
                }
                None => return Err("the quoted value ends with a backslash".to_owned()),
            }
            continue;
        }
        parsed.push(character);
    }
    Err("the quoted value has no closing quote".to_owned())
}

/// Only a comment may follow the closing quote.
fn after_quote(rest: &str) -> Result<(), String> {
    let rest = rest.trim_start();
    if rest.is_empty() || rest.starts_with('#') {
        return Ok(());
    }
    Err("unexpected text after the quoted value".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_values_are_read_with_comments_and_empty_lines() {
        let variables = parse_environment_file(
            "# a comment\nA=1\n\n  B = two words  \nexport C=three\nEMPTY=\nD=#not-a-value\n",
        )
        .unwrap();

        assert_eq!(variables.get("A").map(String::as_str), Some("1"));
        assert_eq!(variables.get("B").map(String::as_str), Some("two words"));
        assert_eq!(variables.get("C").map(String::as_str), Some("three"));
        assert_eq!(variables.get("EMPTY").map(String::as_str), Some(""));
        assert_eq!(variables.get("D").map(String::as_str), Some("#not-a-value"));
    }

    #[test]
    fn quotes_keep_spaces_comments_and_equals() {
        let variables = parse_environment_file(
            "A=\"one two\"\nB='one # two'\nC=\"a=b # c\"\nD=plain # comment\nE=''\n",
        )
        .unwrap();

        assert_eq!(variables.get("A").map(String::as_str), Some("one two"));
        assert_eq!(variables.get("B").map(String::as_str), Some("one # two"));
        assert_eq!(variables.get("C").map(String::as_str), Some("a=b # c"));
        assert_eq!(variables.get("D").map(String::as_str), Some("plain"));
        assert_eq!(variables.get("E").map(String::as_str), Some(""));
    }

    #[test]
    fn escaped_quotes_and_line_breaks_are_read() {
        let variables =
            parse_environment_file("A=\"say \\\"hi\\\"\"\nB=\"two\\nlines\"\nC='back\\slash'\n")
                .unwrap();

        assert_eq!(variables.get("A").map(String::as_str), Some("say \"hi\""));
        assert_eq!(variables.get("B").map(String::as_str), Some("two\nlines"));
        assert_eq!(variables.get("C").map(String::as_str), Some("back\\slash"));
    }

    #[test]
    fn a_broken_line_names_its_number() {
        let error = parse_environment_file("A=1\nnot a variable\nB=2\n").unwrap_err();
        assert!(error.starts_with("line 2:"), "{error}");

        let error = parse_environment_file("A=\"open\n").unwrap_err();
        assert!(error.starts_with("line 1:"), "{error}");

        let error = parse_environment_file("A=\"text\" more\n").unwrap_err();
        assert!(error.starts_with("line 1:"), "{error}");

        let error = parse_environment_file("1BAD=1\n").unwrap_err();
        assert!(error.starts_with("line 1:"), "{error}");
    }

    #[test]
    fn the_configuration_wins_over_the_file() {
        let spec = ProcessSpec::new("program").env("A", "from-config");
        let mut file = BTreeMap::new();
        file.insert("A".to_owned(), "from-file".to_owned());
        file.insert("B".to_owned(), "from-file".to_owned());

        let merged = with_environment(spec, &file);

        let values: Vec<(String, String)> = merged
            .environment()
            .iter()
            .map(|(name, value)| {
                (
                    name.to_string_lossy().into_owned(),
                    value
                        .as_ref()
                        .map(|value| value.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                )
            })
            .collect();
        assert!(values.contains(&("A".to_owned(), "from-config".to_owned())));
        assert!(values.contains(&("B".to_owned(), "from-file".to_owned())));
        assert_eq!(
            values.iter().filter(|(name, _)| name == "A").count(),
            1,
            "the program must not get two values of A"
        );
    }

    #[test]
    fn the_case_of_a_name_does_not_make_a_second_value() {
        let spec = ProcessSpec::new("program").env("Path", "from-config");
        let mut file = BTreeMap::new();
        file.insert("PATH".to_owned(), "from-file".to_owned());

        let merged = with_environment(spec, &file);

        assert_eq!(merged.environment().len(), 1);
    }
}
