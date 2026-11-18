//! What the caret of a console asks for: the objects of a statement, the columns of a
//! table and the names that cannot be resolved.
//!
//! The parser is small on purpose: it reads the words around the caret and the table
//! references of the statement, not the grammar of every dialect. A schema the window has
//! already loaded is the only input, so a keystroke never waits for the database.

use std::collections::HashSet;

use crate::data::{Dialect, TableRef};
use crate::schema::{Table, TableKind};

/// One name the caret may take, with the kind the window draws next to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SqlSuggestion {
    pub label: String,
    pub kind: SuggestionKind,
    /// The type of a column or the kind of an object, as the list shows it.
    pub detail: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SuggestionKind {
    Table,
    View,
    Column,
    Function,
    Keyword,
}

/// A table the statement names, with the alias it gave it and the place of both.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableUse {
    pub schema: Option<String>,
    pub name: String,
    pub alias: Option<String>,
    /// The byte range of the name, without the schema in front of it.
    pub name_start: usize,
    pub name_end: usize,
    /// The byte range of the alias, when the statement gave one.
    pub alias_start: Option<usize>,
    pub alias_end: Option<usize>,
}

/// What the caret of a statement is in front of.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SqlContext {
    /// A table or a view follows: after `FROM`, `JOIN`, `INTO` or `UPDATE`.
    Objects,
    /// A name ends with a dot: the columns of that table, or the objects of that schema.
    Qualified {
        qualifier: String,
        tables: Vec<TableUse>,
    },
    /// A place of an expression: the columns of the statement, its functions and the words
    /// of the dialect.
    Expression { tables: Vec<TableUse> },
    /// Nothing is being written, such as an empty statement.
    Empty,
}

/// A name of the statement that the loaded schema does not have.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SqlProblem {
    /// The byte range of the name in the text of the console.
    pub start: usize,
    pub end: usize,
    pub message: String,
}

/// The suggestions of the caret, in the order the window shows them.
pub fn complete(
    text: &str,
    caret: usize,
    tables: &[Table],
    dialect: Dialect,
    default_schema: Option<&str>,
) -> Vec<SqlSuggestion> {
    let context = context(text, caret);
    let mut suggestions = Vec::new();
    match context {
        SqlContext::Objects => {
            push_objects(&mut suggestions, tables, default_schema, None);
        }
        SqlContext::Qualified {
            qualifier,
            tables: used,
        } => {
            let resolved = resolve_qualifier(&used, &qualifier, tables, default_schema);
            match resolved {
                Some(found) => push_columns(&mut suggestions, found),
                // A qualifier that is not a table may be a schema of the tree.
                None => push_objects_for_schema(&mut suggestions, tables, &qualifier),
            }
        }
        SqlContext::Expression { tables: used } => {
            let resolved = resolve_tables(&used, tables, default_schema);
            for table in &resolved {
                push_columns(&mut suggestions, table);
            }
            push_members(&mut suggestions, dialect, false);
            push_members(&mut suggestions, dialect, true);
        }
        SqlContext::Empty => {}
    }
    dedupe(suggestions)
}

/// The names of the statement that the loaded schema cannot answer. An empty schema is not
/// checked: the window underlines nothing until a source has told it its tables.
pub fn problems(
    text: &str,
    tables: &[Table],
    dialect: Dialect,
    default_schema: Option<&str>,
) -> Vec<SqlProblem> {
    if tables.is_empty() {
        return Vec::new();
    }
    let tokens = lex(text);
    let mut problems = Vec::new();
    for statement in statements(&tokens) {
        let uses = table_uses(statement);
        let mut resolved = Vec::new();
        for table_use in &uses {
            match lookup(
                tables,
                table_use.schema.as_deref(),
                &table_use.name,
                default_schema,
            ) {
                Some(table) => resolved.push(table),
                None => problems.push(SqlProblem {
                    start: table_use.name_start,
                    end: table_use.name_end,
                    message: format!("Unable to resolve table '{}'", table_use.name),
                }),
            }
        }
        if resolved.is_empty() {
            continue;
        }
        let known = known_names(&uses, &resolved);
        let mut index = 0;
        while index < statement.len() {
            let token = &statement[index];
            if !matches!(token.kind, TokenKind::Word | TokenKind::Quoted) {
                index += 1;
                continue;
            }
            let name = name_of(token);
            let lower = name.to_ascii_lowercase();
            let after_as = statement
                .get(index.wrapping_sub(1))
                .is_some_and(|previous| is_word_text(previous, "as"));
            let skip = name.is_empty()
                || !name.starts_with(|character: char| character.is_alphabetic())
                || is_keyword(&lower, dialect)
                || is_function(&lower, dialect)
                || known.contains(&lower)
                || in_table_use(&uses, token)
                || statement
                    .get(index + 1)
                    .is_some_and(|next| is_punct(next, "("))
                || after_as
                || statement
                    .get(index.wrapping_sub(1))
                    .is_some_and(|previous| matches!(previous.text, ":" | "@" | "$"))
                || statement
                    .get(index + 1)
                    .is_some_and(|next| is_punct(next, "."));
            if skip {
                index += 1;
                continue;
            }
            // A name after a dot belongs to the table the qualifier names, and the message
            // says so when that table does not have it.
            if let Some(qualifier) = qualifier_before(statement, index) {
                let found = uses
                    .iter()
                    .find(|table_use| {
                        table_use
                            .alias
                            .as_deref()
                            .is_some_and(|alias| alias.eq_ignore_ascii_case(&qualifier))
                            || table_use.name.eq_ignore_ascii_case(&qualifier)
                    })
                    .and_then(|table_use| {
                        lookup(
                            tables,
                            table_use.schema.as_deref(),
                            &table_use.name,
                            default_schema,
                        )
                    });
                if let Some(table) = found
                    && !table
                        .columns
                        .iter()
                        .any(|column| column.name.eq_ignore_ascii_case(&name))
                {
                    problems.push(SqlProblem {
                        start: token.start,
                        end: token.end,
                        message: format!("Unable to resolve column '{name}'"),
                    });
                }
                index += 1;
                continue;
            }
            problems.push(SqlProblem {
                start: token.start,
                end: token.end,
                message: format!("Unable to resolve column '{name}'"),
            });
            index += 1;
        }
    }
    problems
}

/// Whether a statement may be counted by running it inside `SELECT COUNT(*) FROM (...)`
/// after it already ran: only a statement that yields rows, and never a `WITH` whose body
/// writes. The words of strings and comments do not count, so a quoted `DELETE` or a
/// comment about one keeps the statement countable.
pub fn countable_statement(statement: &str) -> bool {
    let tokens = lex(statement);
    let Some(first) = tokens.iter().find_map(word_upper) else {
        return false;
    };
    match first.as_str() {
        "SELECT" => true,
        "WITH" => !tokens
            .iter()
            .filter_map(word_upper)
            .any(|word| matches!(word.as_str(), "INSERT" | "UPDATE" | "DELETE" | "MERGE")),
        _ => false,
    }
}

/// The one table a statement reads from, when its answer is a copy of that table: a
/// `SELECT` over one name and no join. An export of such an answer names the rows after
/// it without asking.
pub fn single_table(statement: &str) -> Option<TableRef> {
    let tokens = lex(statement);
    if tokens
        .iter()
        .filter_map(word_upper)
        .any(|word| word == "JOIN")
    {
        return None;
    }
    let uses = table_uses(&tokens);
    let [one] = uses.as_slice() else {
        return None;
    };
    Some(TableRef::new(
        one.schema.clone().unwrap_or_default(),
        one.name.clone(),
    ))
}

/// What the caret of the statement is in front of.
pub fn context(text: &str, caret: usize) -> SqlContext {
    let caret = caret.min(text.len());
    let tokens = lex(text);
    let Some(statement) = statement_at(&tokens, caret) else {
        return SqlContext::Empty;
    };
    let word_start = word_start(text, caret);
    let before = statement.iter().rposition(|token| token.end <= word_start);
    // `alias.` and `table.` ask for the columns of that table.
    if let Some(index) = before
        && is_punct(&statement[index], ".")
        && let Some(qualifier) = statement.get(index.wrapping_sub(1))
        && matches!(qualifier.kind, TokenKind::Word | TokenKind::Quoted)
    {
        return SqlContext::Qualified {
            qualifier: name_of(qualifier),
            tables: table_uses(statement),
        };
    }
    let uses = table_uses(statement);
    let previous = before.map(|index| &statement[index]);
    if previous.is_some_and(|token| {
        matches!(
            word_upper(token).as_deref(),
            Some("FROM" | "JOIN" | "INTO" | "UPDATE")
        )
    }) {
        return SqlContext::Objects;
    }
    let clause = clause_of(&statement[..before.map_or(0, |index| index + 1)]);
    if previous.is_some_and(|token| is_punct(token, ","))
        && matches!(clause, Clause::From | Clause::Join)
    {
        return SqlContext::Objects;
    }
    SqlContext::Expression { tables: uses }
}

/// The objects of a statement that start with `schema`, or of the default schema when the
/// caller does not name one.
fn push_objects(
    suggestions: &mut Vec<SqlSuggestion>,
    tables: &[Table],
    default_schema: Option<&str>,
    schema: Option<&str>,
) {
    let mut ordered: Vec<&Table> = tables
        .iter()
        .filter(|table| schema.is_none_or(|schema| table.schema.eq_ignore_ascii_case(schema)))
        .collect();
    ordered.sort_by(|left, right| {
        let left_default =
            default_schema.is_some_and(|schema| left.schema.eq_ignore_ascii_case(schema));
        let right_default =
            default_schema.is_some_and(|schema| right.schema.eq_ignore_ascii_case(schema));
        right_default
            .cmp(&left_default)
            .then_with(|| left.schema.cmp(&right.schema))
            .then_with(|| left.name.cmp(&right.name))
    });
    for table in ordered {
        suggestions.push(object_suggestion(table));
    }
}

/// The objects of one schema, when the part before the dot names a schema and not a table.
fn push_objects_for_schema(suggestions: &mut Vec<SqlSuggestion>, tables: &[Table], schema: &str) {
    let mut found: Vec<&Table> = Vec::new();
    for table in tables {
        if table.schema.eq_ignore_ascii_case(schema) {
            found.push(table);
        }
    }
    if found.is_empty() {
        return;
    }
    found.sort_by(|left, right| left.name.cmp(&right.name));
    for table in found {
        suggestions.push(object_suggestion(table));
    }
}

fn object_suggestion(table: &Table) -> SqlSuggestion {
    SqlSuggestion {
        label: table.name.clone(),
        kind: match table.kind {
            TableKind::Table => SuggestionKind::Table,
            TableKind::View => SuggestionKind::View,
        },
        detail: match table.kind {
            TableKind::Table => "table".to_owned(),
            TableKind::View => "view".to_owned(),
        },
    }
}

fn push_columns(suggestions: &mut Vec<SqlSuggestion>, table: &Table) {
    for column in &table.columns {
        suggestions.push(SqlSuggestion {
            label: column.name.clone(),
            kind: SuggestionKind::Column,
            detail: if column.type_name.is_empty() {
                "column".to_owned()
            } else {
                column.type_name.clone()
            },
        });
    }
}

fn push_members(suggestions: &mut Vec<SqlSuggestion>, dialect: Dialect, functions: bool) {
    let (keywords, function_list) = if functions {
        (COMMON_FUNCTIONS, dialect_functions(dialect))
    } else {
        (KEYWORDS, dialect_keywords(dialect))
    };
    let detail = if functions { "function" } else { "keyword" };
    let kind = if functions {
        SuggestionKind::Function
    } else {
        SuggestionKind::Keyword
    };
    for name in keywords.iter().chain(function_list) {
        suggestions.push(SqlSuggestion {
            label: (*name).to_owned(),
            kind,
            detail: detail.to_owned(),
        });
    }
}

/// One entry per name: a column of two joined tables is offered once.
fn dedupe(suggestions: Vec<SqlSuggestion>) -> Vec<SqlSuggestion> {
    let mut seen = HashSet::new();
    suggestions
        .into_iter()
        .filter(|suggestion| seen.insert(suggestion.label.to_ascii_lowercase()))
        .collect()
}

fn resolve_qualifier<'a>(
    statement: &[TableUse],
    qualifier: &str,
    tables: &'a [Table],
    default_schema: Option<&str>,
) -> Option<&'a Table> {
    if let Some(table_use) = statement.iter().find(|table_use| {
        table_use
            .alias
            .as_deref()
            .is_some_and(|alias| alias.eq_ignore_ascii_case(qualifier))
    }) {
        return lookup(
            tables,
            table_use.schema.as_deref(),
            &table_use.name,
            default_schema,
        );
    }
    lookup(tables, None, qualifier, default_schema)
}

fn resolve_tables<'a>(
    statement: &[TableUse],
    tables: &'a [Table],
    default_schema: Option<&str>,
) -> Vec<&'a Table> {
    let mut resolved: Vec<&'a Table> = Vec::new();
    for table_use in statement {
        if let Some(table) = lookup(
            tables,
            table_use.schema.as_deref(),
            &table_use.name,
            default_schema,
        ) && !resolved
            .iter()
            .any(|known| known.schema == table.schema && known.name == table.name)
        {
            resolved.push(table);
        }
    }
    resolved
}

/// A table of the loaded schema, by name and optionally by schema. The default schema of
/// the console wins when the name is not qualified.
fn lookup<'a>(
    tables: &'a [Table],
    schema: Option<&str>,
    name: &str,
    default_schema: Option<&str>,
) -> Option<&'a Table> {
    if let Some(schema) = schema {
        return tables.iter().find(|table| {
            table.schema.eq_ignore_ascii_case(schema) && table.name.eq_ignore_ascii_case(name)
        });
    }
    if let Some(default_schema) = default_schema
        && let Some(table) = tables.iter().find(|table| {
            table.schema.eq_ignore_ascii_case(default_schema)
                && table.name.eq_ignore_ascii_case(name)
        })
    {
        return Some(table);
    }
    tables
        .iter()
        .find(|table| table.name.eq_ignore_ascii_case(name))
}

fn known_names(uses: &[TableUse], resolved: &[&Table]) -> HashSet<String> {
    let mut known = HashSet::new();
    for table_use in uses {
        known.insert(table_use.name.to_ascii_lowercase());
        if let Some(alias) = &table_use.alias {
            known.insert(alias.to_ascii_lowercase());
        }
        if let Some(schema) = &table_use.schema {
            known.insert(schema.to_ascii_lowercase());
        }
    }
    for table in resolved {
        known.insert(table.name.to_ascii_lowercase());
        known.insert(table.schema.to_ascii_lowercase());
        for column in &table.columns {
            known.insert(column.name.to_ascii_lowercase());
        }
    }
    known
}

fn in_table_use(uses: &[TableUse], token: &Token<'_>) -> bool {
    uses.iter().any(|table_use| {
        let in_name = token.start >= table_use.name_start && token.end <= table_use.name_end;
        let in_alias = table_use
            .alias_start
            .zip(table_use.alias_end)
            .is_some_and(|(start, end)| token.start >= start && token.end <= end);
        in_name || in_alias
    })
}

/// The name before a dot, two tokens back from `index` when the token before belongs to a
/// qualified name.
fn qualifier_before(statement: &[Token<'_>], index: usize) -> Option<String> {
    let dot = index.checked_sub(1)?;
    if !is_punct(&statement[dot], ".") || dot == 0 {
        return None;
    }
    let qualifier = &statement[dot - 1];
    matches!(qualifier.kind, TokenKind::Word | TokenKind::Quoted).then(|| name_of(qualifier))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Clause {
    Select,
    From,
    Join,
    Into,
    Update,
    Where,
    On,
    Order,
    Group,
    Having,
    Set,
    Values,
    Other,
}

fn clause_of(tokens: &[Token<'_>]) -> Clause {
    let mut clause = Clause::Other;
    for token in tokens {
        clause = match word_upper(token).as_deref() {
            Some("SELECT") => Clause::Select,
            Some("FROM") => Clause::From,
            Some("JOIN") => Clause::Join,
            Some("INTO") => Clause::Into,
            Some("UPDATE") => Clause::Update,
            Some("WHERE") => Clause::Where,
            Some("ON") => Clause::On,
            Some("ORDER") => Clause::Order,
            Some("GROUP") => Clause::Group,
            Some("HAVING") => Clause::Having,
            Some("SET") => Clause::Set,
            Some("VALUES") => Clause::Values,
            _ => clause,
        };
    }
    clause
}

fn statements<'a, 't>(tokens: &'a [Token<'t>]) -> Vec<&'a [Token<'t>]> {
    let mut statements = Vec::new();
    let mut start = 0;
    for (index, token) in tokens.iter().enumerate() {
        if is_punct(token, ";") {
            if start < index {
                statements.push(&tokens[start..index]);
            }
            start = index + 1;
        }
    }
    if start < tokens.len() {
        statements.push(&tokens[start..]);
    }
    statements
}

fn statement_at<'a, 't>(tokens: &'a [Token<'t>], caret: usize) -> Option<&'a [Token<'t>]> {
    let mut start = 0;
    for (index, token) in tokens.iter().enumerate() {
        if is_punct(token, ";") {
            if token.end <= caret {
                start = index + 1;
            } else if token.start >= caret {
                return (start < index).then_some(&tokens[start..index]);
            }
        }
    }
    (start < tokens.len()).then_some(&tokens[start..])
}

/// The table references of a statement: the names after `FROM`, `JOIN`, `INTO` and
/// `UPDATE`, with the aliases the statement gives them.
fn table_uses(tokens: &[Token<'_>]) -> Vec<TableUse> {
    let mut uses = Vec::new();
    let mut index = 0;
    while index < tokens.len() {
        let Some(word) = word_upper(&tokens[index]) else {
            index += 1;
            continue;
        };
        match word.as_str() {
            "FROM" => index = parse_list(tokens, index + 1, &mut uses),
            "JOIN" | "UPDATE" | "INTO" => {
                let (next, found) = parse_item(tokens, index + 1);
                if let Some(found) = found {
                    uses.push(found);
                }
                index = next.max(index + 1);
            }
            _ => index += 1,
        }
    }
    uses
}

fn parse_list(tokens: &[Token<'_>], mut index: usize, uses: &mut Vec<TableUse>) -> usize {
    loop {
        let (next, found) = parse_item(tokens, index);
        if let Some(found) = found {
            uses.push(found);
        }
        index = next.max(index + 1);
        if tokens.get(index).is_some_and(|token| is_punct(token, ",")) {
            index += 1;
            continue;
        }
        return index;
    }
}

fn parse_item(tokens: &[Token<'_>], index: usize) -> (usize, Option<TableUse>) {
    let Some(first) = tokens.get(index) else {
        return (index, None);
    };
    if is_punct(first, "(") || !matches!(first.kind, TokenKind::Word | TokenKind::Quoted) {
        return (index + 1, None);
    }
    let first_name = name_of(first);
    let mut next = index + 1;
    let mut schema = None;
    let mut name = first_name;
    let mut name_start = first.start;
    let mut name_end = first.end;
    if let (Some(dot), Some(second)) = (tokens.get(next), tokens.get(next + 1))
        && is_punct(dot, ".")
        && matches!(second.kind, TokenKind::Word | TokenKind::Quoted)
    {
        schema = Some(name);
        name = name_of(second);
        name_start = second.start;
        name_end = second.end;
        next += 2;
    }
    let mut alias = None;
    let mut alias_start = None;
    let mut alias_end = None;
    if let Some(token) = tokens.get(next) {
        if word_upper(token).as_deref() == Some("AS") {
            if let Some(alias_token) = tokens.get(next + 1)
                && matches!(alias_token.kind, TokenKind::Word | TokenKind::Quoted)
            {
                alias = Some(name_of(alias_token));
                alias_start = Some(alias_token.start);
                alias_end = Some(alias_token.end);
                next += 2;
            }
        } else if matches!(token.kind, TokenKind::Word | TokenKind::Quoted)
            && word_upper(token).is_none_or(|word| !is_reserved(&word))
        {
            alias = Some(name_of(token));
            alias_start = Some(token.start);
            alias_end = Some(token.end);
            next += 1;
        }
    }
    (
        next,
        Some(TableUse {
            schema,
            name,
            alias,
            name_start,
            name_end,
            alias_start,
            alias_end,
        }),
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TokenKind {
    Word,
    Quoted,
    Text,
    Number,
    Punct,
}

#[derive(Clone, Copy, Debug)]
struct Token<'a> {
    kind: TokenKind,
    text: &'a str,
    start: usize,
    end: usize,
}

fn lex(text: &str) -> Vec<Token<'_>> {
    let mut tokens = Vec::new();
    let mut characters = text.char_indices().peekable();
    while let Some((index, character)) = characters.next() {
        let start = index;
        let token = match character {
            character if character.is_whitespace() => continue,
            '-' => {
                if characters.peek().is_some_and(|(_, next)| *next == '-') {
                    characters.next();
                    skip_line(&mut characters);
                    continue;
                }
                single(TokenKind::Punct, text, start, character)
            }
            '#' => {
                skip_line(&mut characters);
                continue;
            }
            '/' => {
                if characters.peek().is_some_and(|(_, next)| *next == '*') {
                    characters.next();
                    skip_block(&mut characters);
                    continue;
                }
                single(TokenKind::Punct, text, start, character)
            }
            '\'' => {
                let end = quoted_end(&mut characters, start + character.len_utf8(), '\'');
                Token {
                    kind: TokenKind::Text,
                    text: &text[start..end],
                    start,
                    end,
                }
            }
            '"' | '`' => {
                let end = quoted_end(&mut characters, start + character.len_utf8(), character);
                Token {
                    kind: TokenKind::Quoted,
                    text: &text[start..end],
                    start,
                    end,
                }
            }
            '[' => {
                let end = quoted_end(&mut characters, start + character.len_utf8(), ']');
                Token {
                    kind: TokenKind::Quoted,
                    text: &text[start..end],
                    start,
                    end,
                }
            }
            character if character.is_ascii_digit() => {
                let mut end = start + character.len_utf8();
                while let Some(&(at, next)) = characters.peek() {
                    if !(next.is_ascii_alphanumeric() || next == '.') {
                        break;
                    }
                    characters.next();
                    end = at + next.len_utf8();
                }
                Token {
                    kind: TokenKind::Number,
                    text: &text[start..end],
                    start,
                    end,
                }
            }
            character if is_word(character) => {
                let mut end = start + character.len_utf8();
                while let Some(&(at, next)) = characters.peek() {
                    if !is_word(next) {
                        break;
                    }
                    characters.next();
                    end = at + next.len_utf8();
                }
                Token {
                    kind: TokenKind::Word,
                    text: &text[start..end],
                    start,
                    end,
                }
            }
            character => single(TokenKind::Punct, text, start, character),
        };
        tokens.push(token);
    }
    tokens
}

fn single(kind: TokenKind, text: &str, start: usize, character: char) -> Token<'_> {
    let end = start + character.len_utf8();
    Token {
        kind,
        text: &text[start..end],
        start,
        end,
    }
}

fn skip_line(characters: &mut std::iter::Peekable<std::str::CharIndices<'_>>) {
    for (_, next) in characters.by_ref() {
        if next == '\n' {
            break;
        }
    }
}

fn skip_block(characters: &mut std::iter::Peekable<std::str::CharIndices<'_>>) {
    let mut previous = '\0';
    for (_, next) in characters.by_ref() {
        if previous == '*' && next == '/' {
            break;
        }
        previous = next;
    }
}

/// The end of a quoted run, with the doubled closing character of a string or a name.
fn quoted_end(
    characters: &mut std::iter::Peekable<std::str::CharIndices<'_>>,
    mut end: usize,
    close: char,
) -> usize {
    while let Some((at, next)) = characters.next() {
        end = at + next.len_utf8();
        if next == close {
            if characters
                .peek()
                .is_some_and(|(_, following)| *following == close)
            {
                characters.next();
                end += close.len_utf8();
            } else {
                break;
            }
        }
    }
    end
}

fn is_word(character: char) -> bool {
    character.is_alphanumeric() || character == '_' || character == '$'
}

fn word_start(text: &str, caret: usize) -> usize {
    let mut start = caret.min(text.len());
    while let Some((index, character)) = text[..start].char_indices().next_back() {
        if !is_word(character) {
            break;
        }
        start = index;
    }
    start
}

fn word_upper(token: &Token<'_>) -> Option<String> {
    matches!(token.kind, TokenKind::Word).then(|| token.text.to_ascii_uppercase())
}

fn name_of(token: &Token<'_>) -> String {
    if token.kind != TokenKind::Quoted {
        return token.text.to_owned();
    }
    let text = token.text;
    let Some(open) = text.chars().next() else {
        return String::new();
    };
    let close = if open == '[' { ']' } else { open };
    let inner = text
        .strip_prefix(open)
        .and_then(|text| text.strip_suffix(close))
        .unwrap_or(text);
    let doubled = format!("{close}{close}");
    inner.replace(&doubled, &close.to_string())
}

fn is_punct(token: &Token<'_>, text: &str) -> bool {
    token.kind == TokenKind::Punct && token.text == text
}

/// Whether the token is the word `word`, whatever its case and quoting.
fn is_word_text(token: &Token<'_>, word: &str) -> bool {
    token.kind == TokenKind::Word && token.text.eq_ignore_ascii_case(word)
}

fn is_keyword(word: &str, dialect: Dialect) -> bool {
    KEYWORDS
        .iter()
        .chain(dialect_keywords(dialect))
        .any(|keyword| keyword.eq_ignore_ascii_case(word))
}

fn is_function(word: &str, dialect: Dialect) -> bool {
    COMMON_FUNCTIONS
        .iter()
        .chain(dialect_functions(dialect))
        .any(|function| function.eq_ignore_ascii_case(word))
}

fn is_reserved(word: &str) -> bool {
    KEYWORDS
        .iter()
        .chain(RESERVED.iter())
        .any(|keyword| keyword.eq_ignore_ascii_case(word))
}

/// The keywords of a dialect beyond the common ones.
fn dialect_keywords(dialect: Dialect) -> &'static [&'static str] {
    match dialect {
        Dialect::H2 => H2_KEYWORDS,
        Dialect::PostgreSql => POSTGRES_KEYWORDS,
        Dialect::MySql | Dialect::MariaDb => MYSQL_KEYWORDS,
        Dialect::Sqlite => SQLITE_KEYWORDS,
        Dialect::Oracle => ORACLE_KEYWORDS,
        Dialect::SqlServer => SQLSERVER_KEYWORDS,
    }
}

/// The functions of a dialect beyond the common ones.
fn dialect_functions(dialect: Dialect) -> &'static [&'static str] {
    match dialect {
        Dialect::H2 => H2_FUNCTIONS,
        Dialect::PostgreSql => POSTGRES_FUNCTIONS,
        Dialect::MySql | Dialect::MariaDb => MYSQL_FUNCTIONS,
        Dialect::Sqlite => SQLITE_FUNCTIONS,
        Dialect::Oracle => ORACLE_FUNCTIONS,
        Dialect::SqlServer => SQLSERVER_FUNCTIONS,
    }
}

pub(crate) const KEYWORDS: &[&str] = &[
    "ALL",
    "ALTER",
    "AND",
    "ANY",
    "AS",
    "ASC",
    "BEGIN",
    "BETWEEN",
    "BY",
    "CASE",
    "CAST",
    "COMMIT",
    "CONSTRAINT",
    "CREATE",
    "CROSS",
    "DEFAULT",
    "DELETE",
    "DESC",
    "DISTINCT",
    "DROP",
    "ELSE",
    "END",
    "EXCEPT",
    "EXISTS",
    "FALSE",
    "FETCH",
    "FOREIGN",
    "FROM",
    "FULL",
    "GROUP",
    "HAVING",
    "IN",
    "INDEX",
    "INNER",
    "INSERT",
    "INTERSECT",
    "INTO",
    "IS",
    "JOIN",
    "KEY",
    "LEFT",
    "LIKE",
    "LIMIT",
    "NATURAL",
    "NOT",
    "NULL",
    "OFFSET",
    "ON",
    "ONLY",
    "OR",
    "ORDER",
    "OUTER",
    "PRIMARY",
    "REFERENCES",
    "RIGHT",
    "ROLLBACK",
    "SELECT",
    "SET",
    "SOME",
    "TABLE",
    "THEN",
    "TRUE",
    "UNION",
    "UNIQUE",
    "UPDATE",
    "USING",
    "VALUES",
    "VIEW",
    "WHEN",
    "WHERE",
    "WITH",
    // Type names, so a cast does not look like a column.
    "BIGINT",
    "BINARY",
    "BLOB",
    "BOOLEAN",
    "CHAR",
    "CLOB",
    "DATE",
    "DECIMAL",
    "DOUBLE",
    "FLOAT",
    "INT",
    "INTEGER",
    "JSON",
    "NUMERIC",
    "REAL",
    "SMALLINT",
    "TEXT",
    "TIME",
    "TIMESTAMP",
    "UUID",
    "VARCHAR",
];

/// Words that end a table reference, beyond the keywords above.
const RESERVED: &[&str] = &[
    "ADD",
    "AUTO_INCREMENT",
    "COLUMN",
    "CONFLICT",
    "DO",
    "FIRST",
    "GENERATED",
    "IDENTITY",
    "IF",
    "LAST",
    "LATERAL",
    "NEXT",
    "NULLS",
    "PARTITION",
    "RENAME",
    "REPLACE",
    "RETURNING",
    "ROWS",
    "SEQUENCE",
    "STRAIGHT_JOIN",
    "TOP",
    "TRUNCATE",
    "WINDOW",
];

const H2_KEYWORDS: &[&str] = &["CURRENT_SCHEMA", "ILIKE", "LIMIT", "TOP"];
const POSTGRES_KEYWORDS: &[&str] = &[
    "ILIKE",
    "LATERAL",
    "RETURNING",
    "SERIAL",
    "CONFLICT",
    "RECURSIVE",
    "SEQUENCE",
];
const MYSQL_KEYWORDS: &[&str] = &["AUTO_INCREMENT", "DUPLICATE", "ENGINE", "IGNORE", "REPLACE"];
const SQLITE_KEYWORDS: &[&str] = &["AUTOINCREMENT", "GLOB", "PRAGMA", "WITHOUT"];
const ORACLE_KEYWORDS: &[&str] = &["CONNECT", "MINUS", "PRIOR", "ROWNUM", "SYNONYM"];
const SQLSERVER_KEYWORDS: &[&str] = &["NOLOCK", "OUTPUT", "PIVOT", "TOP", "UNPIVOT"];

pub(crate) const COMMON_FUNCTIONS: &[&str] = &[
    "ABS",
    "AVG",
    "CAST",
    "CEILING",
    "COALESCE",
    "CONCAT",
    "COUNT",
    "CURRENT_DATE",
    "CURRENT_TIME",
    "CURRENT_TIMESTAMP",
    "FLOOR",
    "LOWER",
    "MAX",
    "MIN",
    "NULLIF",
    "ROUND",
    "SUBSTRING",
    "SUM",
    "TRIM",
    "UPPER",
];
const H2_FUNCTIONS: &[&str] = &["LISTAGG", "NOW", "STRINGAGG"];
const POSTGRES_FUNCTIONS: &[&str] = &[
    "DATE_TRUNC",
    "EXTRACT",
    "GEN_RANDOM_UUID",
    "GENERATE_SERIES",
    "NOW",
    "STRING_AGG",
];
const MYSQL_FUNCTIONS: &[&str] = &["GROUP_CONCAT", "IFNULL", "NOW", "STR_TO_DATE"];
const SQLITE_FUNCTIONS: &[&str] = &["DATETIME", "GROUP_CONCAT", "IFNULL", "STRFTIME"];
const ORACLE_FUNCTIONS: &[&str] = &["LISTAGG", "NVL", "SYSDATE", "TO_CHAR", "TO_DATE"];
const SQLSERVER_FUNCTIONS: &[&str] = &["GETDATE", "ISNULL", "LEN", "NEWID", "STRING_AGG"];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{Column, ColumnKind};

    fn column(name: &str, type_name: &str) -> Column {
        Column {
            name: name.into(),
            type_name: type_name.into(),
            kind: ColumnKind::Other,
            nullable: true,
            primary_key: false,
        }
    }

    fn table(name: &str, columns: &[(&str, &str)]) -> Table {
        Table {
            schema: "PUBLIC".into(),
            name: name.into(),
            kind: TableKind::Table,
            columns: columns
                .iter()
                .map(|(name, type_name)| column(name, type_name))
                .collect(),
            ..Table::default()
        }
    }

    fn owners() -> Vec<Table> {
        vec![
            table("OWNERS", &[("ID", "INTEGER"), ("NAME", "VARCHAR")]),
            table("TYPES", &[("ID", "INTEGER"), ("LABEL", "VARCHAR")]),
            Table {
                name: "OWNER_NAMES".into(),
                kind: TableKind::View,
                columns: vec![column("NAME", "VARCHAR")],
                ..table("OWNER_NAMES", &[])
            },
        ]
    }

    fn labels(suggestions: &[SqlSuggestion]) -> Vec<&str> {
        suggestions
            .iter()
            .map(|suggestion| suggestion.label.as_str())
            .collect()
    }

    #[test]
    fn after_from_the_caret_asks_for_tables_and_views() {
        let text = "SELECT * FROM ";
        assert_eq!(context(text, text.len()), SqlContext::Objects);
        let suggestions = complete(text, text.len(), &owners(), Dialect::H2, Some("PUBLIC"));
        assert!(labels(&suggestions).contains(&"OWNERS"), "{suggestions:?}");
        assert!(labels(&suggestions).contains(&"TYPES"), "{suggestions:?}");
        assert!(labels(&suggestions).contains(&"OWNER_NAMES"));
        let view = suggestions
            .iter()
            .find(|suggestion| suggestion.label == "OWNER_NAMES")
            .expect("the view is offered");
        assert_eq!(view.kind, SuggestionKind::View);
        assert_eq!(view.detail, "view");
    }

    #[test]
    fn after_a_dot_the_caret_asks_for_the_columns_of_that_table() {
        let text = "SELECT * FROM OWNERS o WHERE o.";
        assert_eq!(
            context(text, text.len()),
            SqlContext::Qualified {
                qualifier: "o".into(),
                tables: vec![TableUse {
                    schema: None,
                    name: "OWNERS".into(),
                    alias: Some("o".into()),
                    name_start: 14,
                    name_end: 20,
                    alias_start: Some(21),
                    alias_end: Some(22),
                }],
            }
        );
        let suggestions = complete(text, text.len(), &owners(), Dialect::H2, None);
        assert_eq!(labels(&suggestions), ["ID", "NAME"]);
        assert_eq!(suggestions[0].kind, SuggestionKind::Column);
        assert_eq!(suggestions[0].detail, "INTEGER");
    }

    #[test]
    fn an_alias_of_from_as_is_resolved_to_its_table() {
        let text = "SELECT * FROM OWNERS AS o WHERE o.";
        let suggestions = complete(text, text.len(), &owners(), Dialect::H2, None);
        assert_eq!(labels(&suggestions), ["ID", "NAME"]);
    }

    #[test]
    fn where_offers_the_columns_of_the_tables_of_the_statement() {
        let text = "SELECT * FROM OWNERS WHERE ";
        let suggestions = complete(text, text.len(), &owners(), Dialect::H2, Some("PUBLIC"));
        let labels = labels(&suggestions);
        assert!(labels.contains(&"ID"), "{labels:?}");
        assert!(labels.contains(&"NAME"), "{labels:?}");
        assert!(labels.contains(&"WHERE"), "{labels:?}");
        assert!(labels.contains(&"UPPER"), "{labels:?}");
        // A table that the statement does not name has no columns here.
        assert!(!labels.contains(&"LABEL"), "{labels:?}");
    }

    #[test]
    fn joins_are_columns_of_both_tables_and_the_parameter_side_is_a_place() {
        let text = "SELECT * FROM OWNERS o JOIN TYPES t ON t.id = o.";
        let suggestions = complete(text, text.len(), &owners(), Dialect::H2, None);
        // `o.` names the first table, `t.` the second one.
        assert_eq!(labels(&suggestions), ["ID", "NAME"]);
        let text = "SELECT * FROM OWNERS o JOIN TYPES t ON ";
        let suggestions = complete(text, text.len(), &owners(), Dialect::H2, None);
        let labels = labels(&suggestions);
        assert!(labels.contains(&"LABEL"), "{labels:?}");
        assert!(labels.contains(&"NAME"), "{labels:?}");
    }

    #[test]
    fn an_unknown_table_is_a_problem_and_a_missing_schema_checks_nothing() {
        let text = "SELECT * FROM NOPE";
        let found = problems(text, &owners(), Dialect::H2, None);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].message, "Unable to resolve table 'NOPE'");
        assert_eq!(&text[found[0].start..found[0].end], "NOPE");
        assert!(problems("SELECT * FROM NOPE", &[], Dialect::H2, None).is_empty());
    }

    #[test]
    fn only_a_statement_that_yields_rows_without_writing_is_countable() {
        assert!(countable_statement("SELECT ID FROM OWNERS"));
        assert!(countable_statement("-- the owners\nSELECT ID FROM OWNERS;"));
        assert!(countable_statement("WITH o AS (SELECT 1) SELECT * FROM o"));
        // The words of a string and of a comment are not statements.
        assert!(countable_statement("SELECT 'DELETE' AS word -- UPDATE\n"));
        assert!(!countable_statement(
            "WITH moved AS (DELETE FROM OWNERS RETURNING ID) SELECT * FROM moved"
        ));
        assert!(!countable_statement(
            "WITH o AS (SELECT 1) UPDATE OWNERS SET NAME = 'x'"
        ));
        assert!(!countable_statement("UPDATE OWNERS SET NAME = 'x'"));
        assert!(!countable_statement("INSERT INTO OWNERS (ID) VALUES (1)"));
        assert!(!countable_statement("   "));
    }

    #[test]
    fn a_export_names_the_one_table_a_statement_reads() {
        assert_eq!(
            single_table("SELECT * FROM PUBLIC.OWNERS"),
            Some(TableRef::new("PUBLIC", "OWNERS"))
        );
        assert_eq!(
            single_table("select id from owners where id = :id"),
            Some(TableRef::new("", "owners"))
        );
        assert_eq!(
            single_table("SELECT * FROM OWNERS o JOIN TYPES t ON t.ID = o.TYPE_ID"),
            None
        );
        assert_eq!(single_table("SELECT * FROM OWNERS, TYPES"), None);
        assert_eq!(single_table("SELECT 1"), None);
    }

    #[test]
    fn an_unknown_column_of_a_known_table_is_a_problem() {
        let found = problems("SELECT nope FROM OWNERS", &owners(), Dialect::H2, None);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].message, "Unable to resolve column 'nope'");
        assert!(problems("SELECT id, name FROM OWNERS", &owners(), Dialect::H2, None).is_empty());
        assert!(
            problems(
                "SELECT o.name FROM OWNERS o WHERE o.id > 1",
                &owners(),
                Dialect::H2,
                None
            )
            .is_empty()
        );
    }
}
