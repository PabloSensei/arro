//! A snippet in the syntax of the language server protocol turned into text with fields.
//! `$1`, `${1}`, `${1:default}` and `${1|one,two|}` are tab stops, `$0` ends the session,
//! a variable such as `$TM_FILENAME` becomes empty, and `\$`, `\}` and `\\` are literals.

use std::collections::BTreeMap;
use std::ops::Range;

use crate::expand::{Expansion, Field};

struct Builder<'a> {
    text: String,
    indent: &'a str,
    unit: &'a str,
    at_line_start: bool,
    fields: BTreeMap<u32, Vec<Range<usize>>>,
    end: Option<usize>,
}

impl<'a> Builder<'a> {
    fn new(indent: &'a str, unit: &'a str) -> Self {
        Self {
            text: String::new(),
            indent,
            unit,
            at_line_start: false,
            fields: BTreeMap::new(),
            end: None,
        }
    }

    fn push(&mut self, character: char) {
        match character {
            '\n' => {
                self.text.push('\n');
                self.text.push_str(self.indent);
                self.at_line_start = true;
            }
            // A tab at the start of a body line is one indent unit of the file.
            '\t' if self.at_line_start => self.text.push_str(self.unit),
            other => {
                self.text.push(other);
                self.at_line_start = false;
            }
        }
    }

    fn push_escaped(&mut self, character: char) {
        self.text.push(match character {
            'n' => '\n',
            't' => '\t',
            'r' => '\r',
            other => other,
        });
        self.at_line_start = false;
    }

    /// A tab stop of number `number` over the text of `range`. Zero is the end.
    fn field(&mut self, number: u32, range: Range<usize>) {
        if number == 0 {
            self.end.get_or_insert(range.start);
        } else {
            self.fields.entry(number).or_default().push(range);
        }
    }

    /// The text the first occurrence of a field already put in, for a bare `$n` after it.
    fn value_of(&self, number: u32) -> Option<&str> {
        let range = self.fields.get(&number)?.first()?.clone();
        self.text.get(range)
    }

    fn finish(self) -> Expansion {
        let end = self.end.unwrap_or(self.text.len());
        let fields = self
            .fields
            .into_iter()
            .map(|(number, ranges)| Field {
                name: number.to_string(),
                ranges,
            })
            .collect();
        Expansion {
            text: self.text,
            fields,
            end,
        }
    }
}

struct Parser<'a> {
    chars: std::iter::Peekable<std::str::CharIndices<'a>>,
}

impl<'a> Parser<'a> {
    fn new(snippet: &'a str) -> Self {
        Self {
            chars: snippet.char_indices().peekable(),
        }
    }

    fn next(&mut self) -> Option<char> {
        self.chars.next().map(|(_, character)| character)
    }

    fn peek(&mut self) -> Option<char> {
        self.chars.peek().map(|(_, character)| *character)
    }

    /// Parses text until `}` when `nested`, or to the end of the snippet.
    fn sequence(&mut self, builder: &mut Builder<'_>, nested: bool) {
        while let Some(character) = self.next() {
            match character {
                '}' if nested => return,
                '\\' => {
                    if let Some(escaped) = self.next() {
                        builder.push_escaped(escaped);
                    }
                }
                '$' => self.dollar(builder),
                other => builder.push(other),
            }
        }
    }

    fn dollar(&mut self, builder: &mut Builder<'_>) {
        match self.peek() {
            Some('$') => {
                self.next();
                builder.push('$');
            }
            Some('{') => {
                self.next();
                self.brace(builder);
            }
            Some(character) if character.is_ascii_digit() => {
                let number = self.number();
                let start = builder.text.len();
                if number != 0
                    && let Some(value) = builder.value_of(number).map(str::to_owned)
                {
                    for character in value.chars() {
                        builder.push(character);
                    }
                }
                builder.field(number, start..builder.text.len());
            }
            Some(character) if character == '_' || character.is_alphabetic() => {
                while self.peek().is_some_and(is_variable) {
                    self.next();
                }
                // A variable has no text of its own here.
            }
            _ => builder.push('$'),
        }
    }

    fn number(&mut self) -> u32 {
        let mut number = String::new();
        while let Some(character) = self.peek().filter(|character| character.is_ascii_digit()) {
            self.next();
            number.push(character);
        }
        number.parse().unwrap_or(u32::MAX)
    }

    fn brace(&mut self, builder: &mut Builder<'_>) {
        if self
            .peek()
            .is_some_and(|character| character.is_ascii_digit())
        {
            let number = self.number();
            match self.peek() {
                Some('}') => {
                    self.next();
                    let start = builder.text.len();
                    if number != 0
                        && let Some(value) = builder.value_of(number).map(str::to_owned)
                    {
                        for character in value.chars() {
                            builder.push(character);
                        }
                    }
                    builder.field(number, start..builder.text.len());
                }
                Some(':') => {
                    self.next();
                    let start = builder.text.len();
                    self.sequence(builder, true);
                    builder.field(number, start..builder.text.len());
                }
                Some('|') => {
                    self.next();
                    let start = builder.text.len();
                    self.choice(builder);
                    builder.field(number, start..builder.text.len());
                }
                // Transforms and anything else are not part of this editor; the whole
                // construct goes away instead of showing its syntax.
                _ => self.skip_until_close(),
            }
            return;
        }
        // A variable or a transform: skip it, it does not become text.
        self.skip_until_close();
    }

    /// The first variant of `${n|one,two|}`; the others are skipped.
    fn choice(&mut self, builder: &mut Builder<'_>) {
        while let Some(character) = self.next() {
            match character {
                '\\' => {
                    if let Some(escaped) = self.next() {
                        builder.push(escaped);
                    }
                }
                ',' => {
                    self.skip_choice_tail();
                    return;
                }
                '|' if self.peek() == Some('}') => {
                    self.next();
                    return;
                }
                other => builder.push(other),
            }
        }
    }

    fn skip_choice_tail(&mut self) {
        while let Some(character) = self.next() {
            match character {
                '\\' => {
                    self.next();
                }
                '|' if self.peek() == Some('}') => {
                    self.next();
                    return;
                }
                _ => {}
            }
        }
    }

    fn skip_until_close(&mut self) {
        let mut depth = 0usize;
        while let Some(character) = self.next() {
            match character {
                '\\' => {
                    self.next();
                }
                '{' => depth += 1,
                '}' if depth == 0 => return,
                '}' => depth -= 1,
                _ => {}
            }
        }
    }
}

fn is_variable(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '_'
}

/// The text of `snippet` with every tab stop turned into a field. `indent` goes in front
/// of every line after the first, and a tab at the start of a line is one `unit`.
pub fn snippet_expansion(snippet: &str, indent: &str, unit: &str) -> Expansion {
    let mut builder = Builder::new(indent, unit);
    Parser::new(snippet).sequence(&mut builder, false);
    builder.finish()
}

#[cfg(test)]
mod tests {
    use super::snippet_expansion;

    #[test]
    fn a_loop_has_numeric_fields_and_the_copies_of_one_number() {
        let snippet = "for (${1:int} ${2:i} = ${3:0}; $2 < ${4:args}.length; $2++) {\n\t$0\n}";
        let expansion = snippet_expansion(snippet, "  ", "    ");

        assert_eq!(
            expansion.text,
            "for (int i = 0; i < args.length; i++) {\n      \n  }"
        );
        let names: Vec<&str> = expansion
            .fields
            .iter()
            .map(|field| field.name.as_str())
            .collect();
        assert_eq!(names, ["1", "2", "3", "4"]);
        assert_eq!(expansion.fields[1].ranges.len(), 3, "i, $2 and $2 again");
        assert_eq!(
            &expansion.text[..expansion.end],
            "for (int i = 0; i < args.length; i++) {\n      "
        );
    }

    #[test]
    fn choices_escapes_variables_and_a_plain_field() {
        let snippet = concat!(
            r"${1|one,two|};${2:def};\$5;\};",
            "\\\\",
            r";$TM_FILENAME;${TM_USER:x}$0"
        );
        let expansion = snippet_expansion(snippet, "", "    ");

        assert_eq!(expansion.text, "one;def;$5;};\\;;");
        let names: Vec<&str> = expansion
            .fields
            .iter()
            .map(|field| field.name.as_str())
            .collect();
        assert_eq!(names, ["1", "2"], "the variables add no field");
        assert_eq!(expansion.end, expansion.text.len());
    }

    #[test]
    fn a_nested_placeholder_keeps_its_outer_field() {
        let snippet = "${1:first ${2:second}}";
        let expansion = snippet_expansion(snippet, "", "    ");

        assert_eq!(expansion.text, "first second");
        assert_eq!(expansion.fields[0].ranges, vec![0..12]);
        assert_eq!(expansion.fields[1].ranges, vec![6..12]);
    }
}
