//! Reformat Code of SQL: one main clause per line, the columns of a list on lines of
//! their own and a subquery one level deeper. The formatter reads words and separators,
//! not the grammar of a dialect: strings, comments and quoted names keep their text, so a
//! statement that runs before the reformat runs after it as well.

use std::ops::Range;

use crate::complete::{COMMON_FUNCTIONS, KEYWORDS};

/// The case of the keywords after a reformat.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum KeywordCase {
    #[default]
    Upper,
    Lower,
    Keep,
}

/// What a reformat does with a statement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormatOptions {
    pub keyword_case: KeywordCase,
    /// The text of one indentation level; the window gives the step of the file.
    pub indent: String,
}

impl Default for FormatOptions {
    fn default() -> Self {
        Self {
            keyword_case: KeywordCase::Upper,
            indent: "    ".to_owned(),
        }
    }
}

/// The whole text with its statements reformatted.
pub fn format_sql(text: &str, options: &FormatOptions) -> String {
    Formatter::new(options).run(text)
}

/// The lines the byte range touches, reformatted, and where they were. `None` when the
/// text would stay as it is.
pub fn format_sql_range(
    text: &str,
    range: Range<usize>,
    options: &FormatOptions,
) -> Option<(Range<usize>, String)> {
    let start = range.start.min(text.len());
    let end = range.end.min(text.len()).max(start);
    // A selection that ends where a line starts does not touch that line.
    let end = if end > start && text.as_bytes().get(end - 1) == Some(&b'\n') {
        end - 1
    } else {
        end
    };
    let line_start = text[..start].rfind('\n').map_or(0, |index| index + 1);
    let line_end = text[end..]
        .find('\n')
        .map_or(text.len(), |index| end + index);
    let piece = &text[line_start..line_end];
    let formatted = format_sql(piece, options);
    let formatted = formatted.strip_suffix('\n').unwrap_or(&formatted);
    (formatted != piece).then(|| (line_start..line_end, formatted.to_owned()))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Word,
    Number,
    Text,
    Quoted,
    LineComment,
    BlockComment,
    Punct,
}

#[derive(Clone, Copy, Debug)]
struct Token<'a> {
    kind: Kind,
    text: &'a str,
}

/// The tokens of `text`; whitespace only separates them and is not a token of its own.
fn lex(text: &str) -> Vec<Token<'_>> {
    let mut tokens = Vec::new();
    let mut characters = text.char_indices().peekable();
    while let Some((index, character)) = characters.next() {
        let start = index;
        if character.is_whitespace() {
            continue;
        }
        let token = match character {
            '-' if characters.peek().is_some_and(|(_, next)| *next == '-') => {
                characters.next();
                let end = line_end(text, start + 2);
                Token {
                    kind: Kind::LineComment,
                    text: &text[start..end],
                }
            }
            '#' => {
                let end = line_end(text, start + 1);
                Token {
                    kind: Kind::LineComment,
                    text: &text[start..end],
                }
            }
            '/' if characters.peek().is_some_and(|(_, next)| *next == '*') => {
                characters.next();
                let end = block_end(text, start + 2, &mut characters);
                Token {
                    kind: Kind::BlockComment,
                    text: &text[start..end],
                }
            }
            '\'' => {
                let end = quoted_end(text, start + 1, '\'', &mut characters);
                Token {
                    kind: Kind::Text,
                    text: &text[start..end],
                }
            }
            '"' | '`' => {
                let end = quoted_end(text, start + 1, character, &mut characters);
                Token {
                    kind: Kind::Quoted,
                    text: &text[start..end],
                }
            }
            '[' => {
                let end = quoted_end(text, start + 1, ']', &mut characters);
                Token {
                    kind: Kind::Quoted,
                    text: &text[start..end],
                }
            }
            character if character.is_ascii_digit() => {
                let end = word_end(text, start + character.len_utf8(), |next| {
                    next.is_ascii_alphanumeric() || next == '.'
                });
                Token {
                    kind: Kind::Number,
                    text: &text[start..end],
                }
            }
            character if is_word(character) => {
                let end = word_end(text, start + character.len_utf8(), is_word);
                Token {
                    kind: Kind::Word,
                    text: &text[start..end],
                }
            }
            character => {
                let end = if text[start..].starts_with("::")
                    || text[start..].starts_with("<=")
                    || text[start..].starts_with(">=")
                    || text[start..].starts_with("<>")
                    || text[start..].starts_with("!=")
                    || text[start..].starts_with("||")
                    || text[start..].starts_with(":=")
                {
                    start + 2
                } else {
                    start + character.len_utf8()
                };
                Token {
                    kind: Kind::Punct,
                    text: &text[start..end],
                }
            }
        };
        // A quoted run or a two-character operator was read on its own; a word, a number
        // and the rest are skipped here, so the next token starts after this one.
        let end = start + token.text.len();
        while characters.peek().is_some_and(|(at, _)| *at < end) {
            characters.next();
        }
        tokens.push(token);
    }
    tokens
}

fn line_end(text: &str, from: usize) -> usize {
    text[from..]
        .find('\n')
        .map_or(text.len(), |index| from + index)
}

fn block_end(
    text: &str,
    from: usize,
    characters: &mut std::iter::Peekable<std::str::CharIndices<'_>>,
) -> usize {
    let mut end = text.len().min(from);
    let mut previous = '\0';
    for (at, next) in characters.by_ref() {
        end = at + next.len_utf8();
        if previous == '*' && next == '/' {
            break;
        }
        previous = next;
    }
    end
}

/// The end of a quoted run, with a doubled closing character.
fn quoted_end(
    text: &str,
    from: usize,
    close: char,
    characters: &mut std::iter::Peekable<std::str::CharIndices<'_>>,
) -> usize {
    let mut end = text.len().min(from);
    while let Some((at, next)) = characters.next() {
        end = at + next.len_utf8();
        if next == close {
            if characters
                .peek()
                .is_some_and(|(_, following)| *following == close)
            {
                characters.next();
            } else {
                break;
            }
        }
    }
    end
}

fn word_end(text: &str, from: usize, keep: impl Fn(char) -> bool) -> usize {
    let mut end = text.len().min(from);
    for (at, next) in text[from..].char_indices() {
        if !keep(next) {
            break;
        }
        end = from + at + next.len_utf8();
    }
    end
}

fn is_word(character: char) -> bool {
    character.is_alphanumeric() || character == '_' || character == '$'
}

fn is_known_word(word: &str) -> bool {
    KEYWORDS
        .iter()
        .chain(COMMON_FUNCTIONS.iter())
        .any(|known| known.eq_ignore_ascii_case(word))
}

fn is_function(word: &str) -> bool {
    COMMON_FUNCTIONS
        .iter()
        .any(|known| known.eq_ignore_ascii_case(word))
}

fn is_keyword(word: &str) -> bool {
    KEYWORDS
        .iter()
        .any(|known| known.eq_ignore_ascii_case(word))
}

/// The words that begin a main clause of a statement.
fn starts_clause(tokens: &[Token<'_>], index: usize) -> bool {
    let Some(word) = word_upper(tokens, index) else {
        return false;
    };
    match word.as_str() {
        "FROM" => !previous_word_is(tokens, index, "DELETE"),
        "JOIN" => !previous_word_is_join(tokens, index),
        "GROUP" | "ORDER" => {
            next_word(tokens, index).is_some_and(|next| next.eq_ignore_ascii_case("BY"))
        }
        "LEFT" | "RIGHT" | "INNER" | "OUTER" | "FULL" | "CROSS" | "NATURAL" => {
            join_follows(tokens, index) && !previous_word_is_join(tokens, index)
        }
        "SELECT" | "WHERE" | "HAVING" | "LIMIT" | "OFFSET" | "UNION" | "VALUES" | "SET" | "ON"
        | "UPDATE" | "DELETE" | "INSERT" | "WITH" => true,
        _ => false,
    }
}

/// Whether the modifier at `index` belongs to a `JOIN` that follows it.
fn join_follows(tokens: &[Token<'_>], index: usize) -> bool {
    let mut at = index + 1;
    while let Some(word) = word_upper(tokens, at) {
        match word.as_str() {
            "OUTER" => at += 1,
            "JOIN" => return true,
            _ => return false,
        }
    }
    false
}

fn previous_word_is_join(tokens: &[Token<'_>], index: usize) -> bool {
    let Some(previous) = previous_word(tokens, index) else {
        return false;
    };
    let previous = previous.to_ascii_uppercase();
    matches!(
        previous.as_str(),
        "LEFT" | "RIGHT" | "INNER" | "OUTER" | "FULL" | "CROSS" | "NATURAL"
    )
}

fn previous_word_is(tokens: &[Token<'_>], index: usize, word: &str) -> bool {
    previous_word(tokens, index).is_some_and(|previous| previous.eq_ignore_ascii_case(word))
}

fn previous_word<'a>(tokens: &'a [Token<'a>], index: usize) -> Option<&'a str> {
    tokens[..index]
        .iter()
        .rev()
        .find(|token| token.kind == Kind::Word)
        .map(|token| token.text)
}

fn next_word<'a>(tokens: &'a [Token<'a>], index: usize) -> Option<&'a str> {
    tokens[index + 1..]
        .iter()
        .find(|token| token.kind == Kind::Word)
        .map(|token| token.text)
}

fn word_upper(tokens: &[Token<'_>], index: usize) -> Option<String> {
    tokens
        .get(index)
        .filter(|token| token.kind == Kind::Word)
        .map(|token| token.text.to_ascii_uppercase())
}

struct Formatter<'a> {
    options: &'a FormatOptions,
    lines: Vec<String>,
    line: String,
    indent: usize,
    /// The comma of the open clause starts a line of its own.
    list: bool,
    /// Whether each open parenthesis holds a subquery.
    parens: Vec<bool>,
    /// The last written token takes the next one with no space: `(`, `.` or a unary sign.
    tight: bool,
    /// A statement ended: the next one starts after an empty line.
    pending_blank: bool,
}

impl<'a> Formatter<'a> {
    fn new(options: &'a FormatOptions) -> Self {
        Self {
            options,
            lines: Vec::new(),
            line: String::new(),
            indent: 0,
            list: false,
            parens: Vec::new(),
            tight: false,
            pending_blank: false,
        }
    }

    fn run(mut self, text: &str) -> String {
        let tokens = lex(text);
        let mut index = 0;
        while index < tokens.len() {
            self.start_token();
            if tokens[index].kind == Kind::Word && starts_clause(&tokens, index) {
                index = self.write_clause(&tokens, index);
                continue;
            }
            self.write_token(&tokens, index);
            index += 1;
        }
        self.finish();
        self.lines.join("\n")
    }

    fn start_token(&mut self) {
        if !self.pending_blank {
            return;
        }
        self.pending_blank = false;
        self.flush();
        if self.lines.last().is_some_and(|line| !line.is_empty()) {
            self.lines.push(String::new());
        }
    }

    fn write_clause(&mut self, tokens: &[Token<'_>], index: usize) -> usize {
        let word = word_upper(tokens, index).unwrap_or_default();
        self.close_list();
        self.newline();
        self.write_word(tokens, index);
        match word.as_str() {
            "SELECT" => self.after_select(tokens, index + 1),
            "GROUP" | "ORDER" => self.after_group(tokens, index + 1),
            _ => index + 1,
        }
    }

    /// The modifiers and the star of a `SELECT` stay on its line; the first column of a
    /// list starts the indented lines below it.
    fn after_select(&mut self, tokens: &[Token<'_>], mut index: usize) -> usize {
        while let Some(token) = tokens.get(index) {
            if token.kind == Kind::Word {
                let word = token.text.to_ascii_uppercase();
                if matches!(word.as_str(), "DISTINCT" | "ALL" | "TOP") {
                    self.write_token(tokens, index);
                    index += 1;
                    continue;
                }
            }
            break;
        }
        if tokens
            .get(index)
            .is_some_and(|token| token.kind == Kind::Punct && token.text == "*")
        {
            self.write_token(tokens, index);
            return index + 1;
        }
        self.newline();
        self.indent += 1;
        self.list = true;
        index
    }

    /// `GROUP BY` and `ORDER BY` name their columns on lines of their own.
    fn after_group(&mut self, tokens: &[Token<'_>], index: usize) -> usize {
        let mut index = index;
        if tokens
            .get(index)
            .is_some_and(|token| token.kind == Kind::Word && token.text.eq_ignore_ascii_case("BY"))
        {
            self.write_token(tokens, index);
            index += 1;
        }
        self.newline();
        self.indent += 1;
        self.list = true;
        index
    }

    fn write_token(&mut self, tokens: &[Token<'_>], index: usize) {
        let token = tokens[index];
        match token.kind {
            Kind::Word => self.write_word(tokens, index),
            Kind::Number | Kind::Text | Kind::Quoted => {
                self.space();
                self.line.push_str(token.text);
            }
            Kind::LineComment => {
                self.space();
                self.line.push_str(token.text);
                self.newline();
            }
            Kind::BlockComment => {
                self.space();
                self.line.push_str(token.text);
            }
            Kind::Punct => self.write_punct(tokens, index),
        }
    }

    fn write_word(&mut self, tokens: &[Token<'_>], index: usize) {
        let token = tokens[index];
        self.space();
        if is_known_word(token.text) {
            match self.options.keyword_case {
                KeywordCase::Upper => self.line.push_str(&token.text.to_ascii_uppercase()),
                KeywordCase::Lower => self.line.push_str(&token.text.to_ascii_lowercase()),
                KeywordCase::Keep => self.line.push_str(token.text),
            }
        } else {
            self.line.push_str(token.text);
        }
    }

    fn write_punct(&mut self, tokens: &[Token<'_>], index: usize) {
        let token = tokens[index];
        match token.text {
            "," => {
                self.trim_space();
                self.line.push(',');
                if self.list {
                    self.newline();
                } else {
                    self.line.push(' ');
                }
            }
            "(" => {
                let subquery = next_word(tokens, index).is_some_and(|word| {
                    word.eq_ignore_ascii_case("SELECT") || word.eq_ignore_ascii_case("WITH")
                });
                if self.space_before_open(tokens, index) {
                    self.line.push(' ');
                }
                self.line.push('(');
                self.parens.push(subquery);
                if subquery {
                    self.flush();
                    self.indent += 1;
                }
                self.tight = true;
            }
            ")" => {
                self.trim_space();
                let subquery = self.parens.pop().unwrap_or(false);
                if subquery {
                    self.flush();
                    self.indent = self.indent.saturating_sub(1);
                }
                self.line.push(')');
                self.tight = false;
            }
            ";" => {
                self.trim_space();
                self.line.push(';');
                self.end_statement();
            }
            "." | "::" => {
                self.trim_space();
                self.line.push_str(token.text);
                self.tight = true;
            }
            _ => {
                let unary = matches!(token.text, "-" | "+") && self.unary_position();
                if !unary {
                    self.space();
                }
                self.line.push_str(token.text);
                self.tight = unary;
            }
        }
    }

    /// A `(` of a call or of a grouping takes no space before it; one after a keyword
    /// such as `IN` or `VALUES` is a separate word.
    fn space_before_open(&self, tokens: &[Token<'_>], index: usize) -> bool {
        if self.line.is_empty() || self.line.ends_with(' ') {
            return false;
        }
        if index == 0 {
            return false;
        }
        let previous = tokens[index - 1];
        match previous.kind {
            Kind::Word => !is_function(previous.text) && is_keyword(previous.text),
            Kind::Quoted => false,
            _ => false,
        }
    }

    /// Whether a sign is a part of its value rather than a binary operator.
    fn unary_position(&self) -> bool {
        match self.line.chars().next_back() {
            None => true,
            Some(character) => {
                character.is_whitespace()
                    || matches!(
                        character,
                        '(' | ',' | '=' | '<' | '>' | '+' | '-' | '*' | '/'
                    )
            }
        }
    }

    fn space(&mut self) {
        if self.tight {
            self.tight = false;
            return;
        }
        if self.line.is_empty() || self.line.ends_with(' ') {
            return;
        }
        self.line.push(' ');
    }

    fn trim_space(&mut self) {
        while self.line.ends_with(' ') {
            self.line.pop();
        }
    }

    fn close_list(&mut self) {
        if self.list {
            self.flush();
            self.indent = self.indent.saturating_sub(1);
            self.list = false;
        }
    }

    fn newline(&mut self) {
        self.flush();
    }

    fn flush(&mut self) {
        let text = self.line.trim_end();
        if !text.is_empty() {
            let indent = self.options.indent.repeat(self.indent);
            self.lines.push(format!("{indent}{text}"));
        }
        self.line.clear();
        self.tight = false;
    }

    fn end_statement(&mut self) {
        self.flush();
        self.indent = 0;
        self.list = false;
        self.parens.clear();
        self.tight = false;
        self.pending_blank = true;
    }

    fn finish(&mut self) {
        self.flush();
        while self.lines.last().is_some_and(|line| line.is_empty()) {
            self.lines.pop();
        }
        if !self.lines.is_empty() {
            self.lines.push(String::new());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn upper(text: &str) -> String {
        format_sql(text, &FormatOptions::default())
    }

    #[test]
    fn a_simple_select_names_its_columns_on_lines_of_their_own() {
        assert_eq!(
            upper("select id, name from owners where id = 1"),
            "SELECT\n    id,\n    name\nFROM owners\nWHERE id = 1\n"
        );
    }

    #[test]
    fn a_join_and_its_on_get_lines_of_their_own() {
        assert_eq!(
            upper("select o.id from owners o join types t on t.id = o.type_id"),
            "SELECT\n    o.id\nFROM owners o\nJOIN types t\nON t.id = o.type_id\n"
        );
        assert_eq!(
            upper("select o.id from owners o left outer join types t on t.id = o.id"),
            "SELECT\n    o.id\nFROM owners o\nLEFT OUTER JOIN types t\nON t.id = o.id\n"
        );
    }

    #[test]
    fn a_subquery_goes_one_level_deeper() {
        assert_eq!(
            upper("select id from owners where id in (select id from types)"),
            "SELECT\n    id\nFROM owners\nWHERE id IN (\n    SELECT\n        id\n    FROM types\n)\n"
        );
    }

    #[test]
    fn a_string_and_a_comment_keep_their_text() {
        assert_eq!(
            upper("-- select from where\nselect 'from where' as note from owners -- trailing"),
            "-- select from where\nSELECT\n    'from where' AS note\nFROM owners -- trailing\n"
        );
    }

    #[test]
    fn a_quoted_name_is_not_a_keyword() {
        assert_eq!(
            upper("select \"from\" from [where] where name = 'select'"),
            "SELECT\n    \"from\"\nFROM [where]\nWHERE name = 'select'\n"
        );
    }

    #[test]
    fn the_case_of_the_keywords_follows_the_setting() {
        let lower = FormatOptions {
            keyword_case: KeywordCase::Lower,
            ..FormatOptions::default()
        };
        assert_eq!(
            format_sql("SELECT id FROM owners", &lower),
            "select\n    id\nfrom owners\n"
        );
        let keep = FormatOptions {
            keyword_case: KeywordCase::Keep,
            ..FormatOptions::default()
        };
        assert_eq!(
            format_sql("Select id From owners", &keep),
            "Select\n    id\nFrom owners\n"
        );
    }

    #[test]
    fn formatting_twice_changes_nothing() {
        let once = upper("select id,name from owners where id=1;select count(*) from owners;");
        assert_eq!(upper(&once), once);
        assert_eq!(
            once,
            "SELECT\n    id,\n    name\nFROM owners\nWHERE id = 1;\n\nSELECT\n    COUNT(*)\nFROM owners;\n"
        );
    }

    #[test]
    fn a_selection_formats_only_its_lines() {
        let text = "select 1;\nselect 2;\nselect 3;\n";
        let start = text.find("select 2").unwrap();
        let (range, formatted) =
            format_sql_range(text, start..start + 8, &FormatOptions::default()).unwrap();
        assert_eq!(range, 10..19);
        assert_eq!(formatted, "SELECT\n    2;");

        let mut text = text.to_owned();
        text.replace_range(range, &formatted);
        assert_eq!(text, "select 1;\nSELECT\n    2;\nselect 3;\n");
    }
}
