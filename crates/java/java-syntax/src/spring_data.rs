//! The shape of the queries a data repository spells out: the names of the methods of
//! the conventions of the framework and the text of a query annotation. Both are read as
//! text: the name of a property is resolved by the caller against the model of the
//! entities, and the meaning of a token is decided there too.

use std::ops::Range;

/// The verb a query method starts with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Prefix {
    Find,
    Read,
    Get,
    Query,
    Search,
    Stream,
    Count,
    Exists,
    Delete,
    Remove,
}

const PREFIXES: &[(&str, Prefix)] = &[
    ("find", Prefix::Find),
    ("read", Prefix::Read),
    ("get", Prefix::Get),
    ("query", Prefix::Query),
    ("search", Prefix::Search),
    ("stream", Prefix::Stream),
    ("count", Prefix::Count),
    ("exists", Prefix::Exists),
    ("delete", Prefix::Delete),
    ("remove", Prefix::Remove),
];

/// What the name asks of a property after it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Operator {
    /// No operator: the property takes one parameter as it is.
    #[default]
    Is,
    Between,
    LessThan,
    LessThanEqual,
    GreaterThan,
    GreaterThanEqual,
    After,
    Before,
    IsNull,
    IsNotNull,
    NotNull,
    Like,
    NotLike,
    StartingWith,
    EndingWith,
    Containing,
    Not,
    In,
    NotIn,
    True,
    False,
    IsEmpty,
    IsNotEmpty,
}

impl Operator {
    /// The parameters the operator takes from the method.
    pub fn parameters(self) -> usize {
        match self {
            Self::Between => 2,
            Self::IsNull
            | Self::IsNotNull
            | Self::NotNull
            | Self::True
            | Self::False
            | Self::IsEmpty
            | Self::IsNotEmpty => 0,
            _ => 1,
        }
    }

    /// The word of the method name, `None` for a property without an operator.
    pub fn word(self) -> Option<&'static str> {
        Some(match self {
            Self::Is => return None,
            Self::Between => "Between",
            Self::LessThan => "LessThan",
            Self::LessThanEqual => "LessThanEqual",
            Self::GreaterThan => "GreaterThan",
            Self::GreaterThanEqual => "GreaterThanEqual",
            Self::After => "After",
            Self::Before => "Before",
            Self::IsNull => "IsNull",
            Self::IsNotNull => "IsNotNull",
            Self::NotNull => "NotNull",
            Self::Like => "Like",
            Self::NotLike => "NotLike",
            Self::StartingWith => "StartingWith",
            Self::EndingWith => "EndingWith",
            Self::Containing => "Containing",
            Self::Not => "Not",
            Self::In => "In",
            Self::NotIn => "NotIn",
            Self::True => "True",
            Self::False => "False",
            Self::IsEmpty => "IsEmpty",
            Self::IsNotEmpty => "IsNotEmpty",
        })
    }
}

/// The operators the name may end a condition with, longest first, so that
/// `GreaterThanEqual` wins over `GreaterThan` and `IsNotNull` over `NotNull`.
const OPERATORS: &[(&str, Operator)] = &[
    ("GreaterThanEqual", Operator::GreaterThanEqual),
    ("LessThanEqual", Operator::LessThanEqual),
    ("GreaterThan", Operator::GreaterThan),
    ("LessThan", Operator::LessThan),
    ("StartingWith", Operator::StartingWith),
    ("EndingWith", Operator::EndingWith),
    ("Containing", Operator::Containing),
    ("IsNotEmpty", Operator::IsNotEmpty),
    ("IsNotNull", Operator::IsNotNull),
    ("NotEmpty", Operator::IsNotEmpty),
    ("Between", Operator::Between),
    ("IsEmpty", Operator::IsEmpty),
    ("NotNull", Operator::NotNull),
    ("NotLike", Operator::NotLike),
    ("NotIn", Operator::NotIn),
    ("Before", Operator::Before),
    ("After", Operator::After),
    ("IsNull", Operator::IsNull),
    ("Empty", Operator::IsEmpty),
    ("True", Operator::True),
    ("False", Operator::False),
    ("Like", Operator::Like),
    ("Not", Operator::Not),
    ("In", Operator::In),
];

/// The words of the operators, for a list of completion after a property.
pub const OPERATOR_WORDS: &[&str] = &[
    "Between",
    "LessThan",
    "LessThanEqual",
    "GreaterThan",
    "GreaterThanEqual",
    "After",
    "Before",
    "IsNull",
    "IsNotNull",
    "NotNull",
    "Like",
    "NotLike",
    "StartingWith",
    "EndingWith",
    "Containing",
    "Not",
    "In",
    "NotIn",
    "True",
    "False",
    "IsEmpty",
    "IsNotEmpty",
];

/// A part of a method name with the bytes it takes there.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Segment {
    pub text: String,
    pub start: usize,
    pub end: usize,
}

/// The conjunction between two conditions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Connector {
    #[default]
    And,
    Or,
}

/// One condition of a method name: a property path and the operator at its end.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Condition {
    /// The conjunction before this condition; the first one keeps `And`.
    pub connector: Connector,
    /// The property as the name spells it, with its nested parts joined, without `_`.
    pub property: Segment,
    pub operator: Operator,
    /// Where the operator starts, for a completion after the property.
    pub operator_start: usize,
    pub ignore_case: bool,
}

/// One ordering of a method name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Order {
    pub property: Segment,
    pub descending: bool,
}

/// A method name of the conventions of the framework, read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MethodName {
    pub prefix: Option<Prefix>,
    pub distinct: bool,
    /// The number of `Top` or `First`, when the name limits the rows.
    pub top: Option<usize>,
    pub conditions: Vec<Condition>,
    pub orders: Vec<Order>,
    pub all_ignore_case: bool,
}

impl MethodName {
    /// The parameters the conditions take, `Pageable` and `Sort` aside.
    pub fn parameters(&self) -> usize {
        self.conditions
            .iter()
            .map(|condition| condition.operator.parameters())
            .sum()
    }
}

/// Reads a method name of the conventions of the framework. `None` is a method that is
/// not a query method, such as `save` or a default method of the interface.
pub fn parse_method_name(name: &str) -> Option<MethodName> {
    let (prefix, mut at) = prefix_of(name)?;
    let mut distinct = false;
    if starts_word(name, at, "Distinct") {
        distinct = true;
        at += "Distinct".len();
    }
    let top = top_of(name, &mut at);
    let mut read = MethodName {
        prefix: Some(prefix),
        distinct,
        top,
        ..MethodName::default()
    };
    if at == name.len() {
        return Some(read);
    }
    // The words between the verb and `By` are the subject of the query and do not name a
    // property, as in `findAll` or `findAllBy...`.
    let Some(by) = find_word(name, at, name.len(), "By") else {
        return Some(read);
    };
    at = by + "By".len();
    let mut end = name.len();
    if name.len() > at + "AllIgnoreCase".len() && name.ends_with("AllIgnoreCase") {
        read.all_ignore_case = true;
        end -= "AllIgnoreCase".len();
    }
    let order_at = find_word(name, at, end, "OrderBy");
    let conditions_end = order_at.unwrap_or(end);
    if conditions_end > at {
        read.conditions = read_conditions(name, at, conditions_end);
        if read.conditions.is_empty() {
            return None;
        }
    }
    if let Some(order_at) = order_at {
        read.orders = read_orders(name, order_at + "OrderBy".len(), end);
    }
    Some(read)
}

/// The verb at the start of a name and the byte after it.
fn prefix_of(name: &str) -> Option<(Prefix, usize)> {
    for (word, prefix) in PREFIXES {
        if !name.starts_with(word) {
            continue;
        }
        let at = word.len();
        // `finder` is not a query method: the verb ends before a capital letter.
        let next = name[at..].chars().next();
        if next.is_some_and(|next| next.is_lowercase()) {
            continue;
        }
        return Some((*prefix, at));
    }
    None
}

/// The number of rows of a `Top` or `First` and the byte after it.
fn top_of(name: &str, at: &mut usize) -> Option<usize> {
    let rest = &name[*at..];
    let word = ["Top", "First"]
        .into_iter()
        .find(|word| rest.starts_with(word))?;
    let mut end = word.len();
    while rest
        .as_bytes()
        .get(end)
        .is_some_and(|byte| byte.is_ascii_digit())
    {
        end += 1;
    }
    let count = rest[word.len()..end]
        .parse::<usize>()
        .ok()
        .filter(|count| *count > 0)
        .unwrap_or(1);
    *at += end;
    Some(count)
}

/// Whether the text at `at` is `word` and ends at a word boundary.
fn starts_word(text: &str, at: usize, word: &str) -> bool {
    text[at..].starts_with(word)
        && text
            .as_bytes()
            .get(at + word.len())
            .is_none_or(|next| !next.is_ascii_lowercase())
}

/// The first place of `word` from `at` to `end` that starts after a letter.
fn find_word(text: &str, at: usize, end: usize, word: &str) -> Option<usize> {
    let mut found = at.max(1);
    while found + word.len() <= end {
        if text[found..].starts_with(word)
            && text.as_bytes()[found - 1].is_ascii_alphanumeric()
            && text
                .as_bytes()
                .get(found + word.len())
                .is_none_or(|next| !next.is_ascii_lowercase())
        {
            return Some(found);
        }
        found += 1;
        while !text.is_char_boundary(found) {
            found += 1;
        }
    }
    None
}

fn read_conditions(name: &str, at: usize, end: usize) -> Vec<Condition> {
    let mut conditions = Vec::new();
    let mut connector = Connector::And;
    let mut start = at;
    let mut cursor = at + 1;
    while cursor < end {
        if !name.is_char_boundary(cursor) {
            cursor += 1;
            continue;
        }
        let split = if name[cursor..].starts_with("And") {
            Some((Connector::And, "And".len()))
        } else if name[cursor..].starts_with("Or") {
            Some((Connector::Or, "Or".len()))
        } else {
            None
        };
        // `And` and `Or` split a condition only before a capital letter: the words of a
        // property are lower case otherwise.
        if let Some((found, size)) = split
            && name
                .as_bytes()
                .get(cursor + size)
                .is_none_or(|next| next.is_ascii_uppercase() || *next == b'_')
        {
            conditions.push(condition(name, start, cursor, connector));
            connector = found;
            start = cursor + size;
            cursor = start + 1;
            continue;
        }
        cursor += 1;
    }
    if start < end {
        conditions.push(condition(name, start, end, connector));
    }
    conditions
}

/// One condition of `name` between two bytes: its property, its operator and the case of
/// the comparison.
fn condition(name: &str, start: usize, end: usize, connector: Connector) -> Condition {
    let mut end = end;
    let mut ignore_case = false;
    if end > start + "IgnoreCase".len() && name[start..end].ends_with("IgnoreCase") {
        ignore_case = true;
        end -= "IgnoreCase".len();
    }
    let (operator, property_end) = OPERATORS
        .iter()
        .find_map(|(word, operator)| {
            let size = word.len();
            if end > start + size && name[start..end].ends_with(word) {
                Some((*operator, end - size))
            } else {
                None
            }
        })
        .unwrap_or((Operator::Is, end));
    Condition {
        connector,
        property: Segment {
            text: name[start..property_end].to_owned(),
            start,
            end: property_end,
        },
        operator,
        operator_start: property_end,
        ignore_case,
    }
}

fn read_orders(name: &str, at: usize, end: usize) -> Vec<Order> {
    let mut orders = Vec::new();
    let mut start = at;
    while start < end {
        let mut direction = None;
        let mut cursor = start + 1;
        while cursor < end {
            if !name.is_char_boundary(cursor) {
                cursor += 1;
                continue;
            }
            let tail = &name[cursor..end];
            if tail.starts_with("Asc") || tail.starts_with("Desc") {
                let size = if tail.starts_with("Asc") { 3 } else { 4 };
                let next = name.as_bytes().get(cursor + size).copied();
                if next.is_none_or(|next| next.is_ascii_uppercase() || next == b'_') {
                    direction = Some((cursor, size, tail.starts_with("Desc")));
                    break;
                }
            }
            cursor += 1;
        }
        let (property_end, descending, next) = match direction {
            Some((at, size, descending)) => (at, descending, at + size),
            None => (end, false, end),
        };
        if property_end > start {
            orders.push(Order {
                property: Segment {
                    text: name[start..property_end].to_owned(),
                    start,
                    end: property_end,
                },
                descending,
            });
        }
        start = next;
    }
    orders
}

/// The word of a method name a caret stands after, for the completion of the properties
/// and the operators.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Boundary {
    By,
    And,
    Or,
    OrderBy,
}

impl Boundary {
    /// The byte offset a completion replaces from.
    pub fn start(self) -> usize {
        match self {
            Self::By => "By".len(),
            Self::And => "And".len(),
            Self::Or => "Or".len(),
            Self::OrderBy => "OrderBy".len(),
        }
    }
}

impl Boundary {
    fn word(self) -> &'static str {
        match self {
            Self::By => "By",
            Self::And => "And",
            Self::Or => "Or",
            Self::OrderBy => "OrderBy",
        }
    }
}

/// Where a caret of a method name stands: the word before the part it types and the
/// offset that part starts at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MethodCursor {
    pub boundary: Boundary,
    /// The byte offset of the typed part inside the name.
    pub start: usize,
}

/// Finds the last `By`, `And`, `Or` or `OrderBy` before `caret`.
pub fn method_cursor(name: &str, caret: usize) -> Option<MethodCursor> {
    let caret = caret.min(name.len());
    let head = &name[..caret];
    let mut best: Option<(Boundary, usize)> = None;
    for boundary in [Boundary::OrderBy, Boundary::By, Boundary::And, Boundary::Or] {
        let word = boundary.word();
        let mut found = 1;
        while found + word.len() <= head.len() {
            if !head.is_char_boundary(found) {
                found += 1;
                continue;
            }
            // The `By` of `OrderBy` belongs to the longer word.
            let in_order_by = word == "By" && found >= 5 && &head[found - 5..found] == "Order";
            if !in_order_by
                && head[found..].starts_with(word)
                && head.as_bytes()[found - 1].is_ascii_alphabetic()
                && head
                    .as_bytes()
                    .get(found + word.len())
                    .is_none_or(|next| !next.is_ascii_lowercase())
                && best.is_none_or(|(_, at)| found > at)
            {
                best = Some((boundary, found));
            }
            found += 1;
        }
    }
    let (boundary, at) = best?;
    Some(MethodCursor {
        boundary,
        start: at + boundary.word().len(),
    })
}

/// One word of a query.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenKind {
    Keyword,
    Identifier,
    /// A `:name` placeholder, with the name in its text.
    Parameter,
    String,
    Number,
    Symbol,
}

/// One word of a query with its bytes inside the text of the query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub text: String,
    pub range: Range<usize>,
}

const KEYWORDS: &[&str] = &[
    "SELECT",
    "FROM",
    "WHERE",
    "JOIN",
    "LEFT",
    "RIGHT",
    "INNER",
    "OUTER",
    "FULL",
    "ON",
    "ORDER",
    "BY",
    "GROUP",
    "HAVING",
    "AS",
    "AND",
    "OR",
    "NOT",
    "IN",
    "IS",
    "NULL",
    "LIKE",
    "BETWEEN",
    "NEW",
    "DISTINCT",
    "SET",
    "UPDATE",
    "DELETE",
    "INSERT",
    "INTO",
    "VALUES",
    "ASC",
    "DESC",
    "COUNT",
    "SUM",
    "AVG",
    "MIN",
    "MAX",
    "EXISTS",
    "ALL",
    "ANY",
    "SOME",
    "MEMBER",
    "OF",
    "EMPTY",
    "TRUE",
    "FALSE",
    "FETCH",
    "WITH",
    "CASE",
    "WHEN",
    "THEN",
    "ELSE",
    "END",
    "CURRENT_DATE",
    "CURRENT_TIME",
    "CURRENT_TIMESTAMP",
    "ESCAPE",
    "LOCATE",
    "SIZE",
    "INDEX",
    "KEY",
    "TREAT",
    "CONCAT",
    "SUBSTRING",
    "TRIM",
    "LOWER",
    "UPPER",
    "LENGTH",
    "ABS",
    "SQRT",
    "MOD",
    "COALESCE",
    "NULLIF",
    "TYPE",
    "VALUE",
    "ENTRY",
];

/// The words of a query: keywords, names, placeholders, literals and punctuation.
pub fn tokens(text: &str) -> Vec<Token> {
    let bytes = text.as_bytes();
    let mut found = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let start = at;
        let byte = bytes[at];
        if byte.is_ascii_whitespace() {
            at += 1;
            continue;
        }
        if byte == b':' {
            at += 1;
            let name_start = at;
            while at < bytes.len() && is_word_byte(bytes[at]) {
                at += 1;
            }
            if at > name_start {
                found.push(Token {
                    kind: TokenKind::Parameter,
                    text: text[name_start..at].to_owned(),
                    range: name_start..at,
                });
            }
            continue;
        }
        if byte == b'\'' {
            at += 1;
            while at < bytes.len() {
                if bytes[at] == b'\'' {
                    at += 1;
                    // A doubled quote is a quote inside the literal.
                    if bytes.get(at) == Some(&b'\'') {
                        at += 1;
                        continue;
                    }
                    break;
                }
                at += 1;
            }
            found.push(Token {
                kind: TokenKind::String,
                text: text[start..at].to_owned(),
                range: start..at,
            });
            continue;
        }
        if byte.is_ascii_digit() {
            at += 1;
            while at < bytes.len() && (bytes[at].is_ascii_digit() || bytes[at] == b'.') {
                at += 1;
            }
            found.push(Token {
                kind: TokenKind::Number,
                text: text[start..at].to_owned(),
                range: start..at,
            });
            continue;
        }
        if is_word_byte(byte) {
            at += 1;
            while at < bytes.len() && is_word_byte(bytes[at]) {
                at += 1;
            }
            let word = &text[start..at];
            let kind = if KEYWORDS
                .iter()
                .any(|keyword| keyword.eq_ignore_ascii_case(word))
            {
                TokenKind::Keyword
            } else {
                TokenKind::Identifier
            };
            found.push(Token {
                kind,
                text: word.to_owned(),
                range: start..at,
            });
            continue;
        }
        at += 1;
        // `<>` and `<=` stay two symbols: no caller reads them as one.
        found.push(Token {
            kind: TokenKind::Symbol,
            text: text[start..at].to_owned(),
            range: start..at,
        });
    }
    found
}

fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'$'
}

/// What an alias of a query names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AliasSource {
    /// The name of an entity of `FROM` or of a `JOIN` of its own.
    Entity(String),
    /// An association of another alias, `pets` of `o` for `JOIN o.pets p`.
    Path { alias: String, property: String },
}

/// One alias of a query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Alias {
    pub name: String,
    pub source: AliasSource,
}

/// A name of a query with the bytes it takes there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reference {
    pub text: String,
    pub range: Range<usize>,
}

/// A property path written from an alias, `o.lastName`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PropertyPath {
    pub alias: String,
    /// The parts after the alias, joined with dots.
    pub path: String,
    pub range: Range<usize>,
}

/// A query of a repository method, read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Query {
    pub tokens: Vec<Token>,
    pub aliases: Vec<Alias>,
    /// The entities a `FROM` or a `JOIN` names directly.
    pub entities: Vec<Reference>,
    pub properties: Vec<PropertyPath>,
    /// The `:name` placeholders without the colon.
    pub parameters: Vec<Reference>,
}

/// Reads the entities, aliases, property paths and placeholders of a query.
pub fn parse_query(text: &str) -> Query {
    let tokens = tokens(text);
    let mut query = Query {
        tokens: tokens.clone(),
        ..Query::default()
    };
    let mut at = 0;
    while at < tokens.len() {
        let token = &tokens[at];
        let declaration = token.kind == TokenKind::Keyword
            && matches!(token.text.to_ascii_uppercase().as_str(), "FROM" | "JOIN");
        if declaration {
            at += 1;
            if tokens
                .get(at)
                .is_some_and(|token| token.text.eq_ignore_ascii_case("FETCH"))
            {
                at += 1;
            }
            let Some(first) = tokens.get(at).filter(|token| {
                token.kind == TokenKind::Identifier || token.kind == TokenKind::Keyword
            }) else {
                continue;
            };
            if first.kind == TokenKind::Keyword {
                continue;
            }
            // A name of the store may be written with its package or as a path of an
            // association: `com.example.Owner` and `o.pets` are one name.
            let mut text = first.text.clone();
            let mut range = first.range.clone();
            let mut end_token = at;
            while tokens
                .get(end_token + 1)
                .is_some_and(|token| token.text == ".")
                && tokens.get(end_token + 2).is_some_and(|token| {
                    token.kind == TokenKind::Identifier || token.kind == TokenKind::Keyword
                })
            {
                let part = &tokens[end_token + 2];
                text.push('.');
                text.push_str(&part.text);
                range.end = part.range.end;
                end_token += 2;
            }
            let mut next = end_token + 1;
            if tokens
                .get(next)
                .is_some_and(|token| token.text.eq_ignore_ascii_case("AS"))
            {
                next += 1;
            }
            let alias = tokens
                .get(next)
                .filter(|token| {
                    token.kind == TokenKind::Identifier
                        && !matches!(
                            token.text.to_ascii_uppercase().as_str(),
                            "WHERE" | "ORDER" | "GROUP" | "HAVING" | "SET" | "ON"
                        )
                })
                .map(|token| token.text.clone());
            let (name, name_range, association) = match text.split_once('.') {
                Some((base, _)) if query.aliases.iter().any(|alias| alias.name == base) => {
                    (text.clone(), range.clone(), true)
                }
                // A type written with its package names the entity by its last part.
                Some(_) => {
                    let simple = text.rsplit('.').next().unwrap_or(&text).to_owned();
                    let start = range.end - simple.len();
                    (simple, start..range.end, false)
                }
                None => (text.clone(), range.clone(), false),
            };
            if association {
                let (base, property) = name.split_once('.').unwrap_or((name.as_str(), ""));
                if let Some(alias) = &alias {
                    query.aliases.push(Alias {
                        name: alias.clone(),
                        source: AliasSource::Path {
                            alias: base.to_owned(),
                            property: property.to_owned(),
                        },
                    });
                }
            } else {
                query.entities.push(Reference {
                    text: name.clone(),
                    range: name_range,
                });
                if let Some(alias) = &alias {
                    query.aliases.push(Alias {
                        name: alias.clone(),
                        source: AliasSource::Entity(name),
                    });
                }
            }
            if alias.is_some() {
                at = next;
            }
        }
        at += 1;
    }
    for (index, token) in tokens.iter().enumerate() {
        if token.kind == TokenKind::Parameter {
            query.parameters.push(Reference {
                text: token.text.clone(),
                range: token.range.clone(),
            });
            continue;
        }
        if token.kind != TokenKind::Identifier {
            continue;
        }
        // Only a path written from a declared alias is a property: a qualified name of an
        // entity or a function of the store stays out.
        if !query.aliases.iter().any(|alias| alias.name == token.text) {
            continue;
        }
        let Some(dot) = tokens.get(index + 1) else {
            continue;
        };
        if dot.text != "." {
            continue;
        }
        let mut path = String::new();
        let mut end = token.range.end;
        let mut cursor = index + 2;
        while cursor < tokens.len() {
            let part = &tokens[cursor];
            if part.kind != TokenKind::Identifier && part.kind != TokenKind::Keyword {
                break;
            }
            if !path.is_empty() {
                path.push('.');
            }
            path.push_str(&part.text);
            end = part.range.end;
            cursor += 1;
            if tokens.get(cursor).is_none_or(|next| next.text != ".") {
                break;
            }
            cursor += 1;
        }
        if path.is_empty() {
            continue;
        }
        query.properties.push(PropertyPath {
            alias: token.text.clone(),
            path,
            range: token.range.start..end,
        });
    }
    query
}

/// The token at `offset`, or the one the caret ends.
pub fn token_at(tokens: &[Token], offset: usize) -> Option<&Token> {
    tokens
        .iter()
        .find(|token| token.range.start <= offset && offset <= token.range.end)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn properties(name: &str) -> Vec<(String, Operator)> {
        parse_method_name(name)
            .unwrap_or_else(|| panic!("{name} is a query method"))
            .conditions
            .into_iter()
            .map(|condition| (condition.property.text, condition.operator))
            .collect()
    }

    #[test]
    fn a_name_is_read_into_a_verb_a_property_and_an_operator() {
        let read = parse_method_name("findByLastNameStartingWith").unwrap();
        assert_eq!(read.prefix, Some(Prefix::Find));
        assert!(!read.distinct);
        assert_eq!(read.top, None);
        assert_eq!(read.conditions.len(), 1);
        let condition = &read.conditions[0];
        assert_eq!(condition.property.text, "LastName");
        assert_eq!(condition.property.start, "findBy".len());
        assert_eq!(condition.operator, Operator::StartingWith);
        assert_eq!(condition.operator_start, "findByLastName".len());
        assert!(!condition.ignore_case);
        assert_eq!(read.parameters(), 1);

        let read = parse_method_name("countDistinctByCity").unwrap();
        assert_eq!(read.prefix, Some(Prefix::Count));
        assert!(read.distinct);
        assert_eq!(read.parameters(), 1);

        let read = parse_method_name("findTop10ByCity").unwrap();
        assert_eq!(read.top, Some(10));
        let read = parse_method_name("findFirstByCity").unwrap();
        assert_eq!(read.top, Some(1));
        let read = parse_method_name("findAll").unwrap();
        assert!(read.conditions.is_empty());
        assert!(parse_method_name("save").is_none());
        assert!(parse_method_name("finder").is_none());
    }

    #[test]
    fn every_operator_of_the_conventions_is_read() {
        let cases = [
            ("findByName", Operator::Is),
            ("findByBornBetween", Operator::Between),
            ("findByAgeLessThan", Operator::LessThan),
            ("findByAgeLessThanEqual", Operator::LessThanEqual),
            ("findByAgeGreaterThan", Operator::GreaterThan),
            ("findByAgeGreaterThanEqual", Operator::GreaterThanEqual),
            ("findByBornAfter", Operator::After),
            ("findByBornBefore", Operator::Before),
            ("findByNameIsNull", Operator::IsNull),
            ("findByNameIsNotNull", Operator::IsNotNull),
            ("findByNameNotNull", Operator::NotNull),
            ("findByNameLike", Operator::Like),
            ("findByNameNotLike", Operator::NotLike),
            ("findByNameStartingWith", Operator::StartingWith),
            ("findByNameEndingWith", Operator::EndingWith),
            ("findByNameContaining", Operator::Containing),
            ("findByNameNot", Operator::Not),
            ("findByNameIn", Operator::In),
            ("findByNameNotIn", Operator::NotIn),
            ("findByActiveTrue", Operator::True),
            ("findByActiveFalse", Operator::False),
            ("findByPetsEmpty", Operator::IsEmpty),
            ("findByPetsNotEmpty", Operator::IsNotEmpty),
        ];
        for (name, operator) in cases {
            let read = parse_method_name(name).unwrap_or_else(|| panic!("{name} is read"));
            assert_eq!(read.conditions.len(), 1, "{name}");
            assert_eq!(read.conditions[0].operator, operator, "{name}");
            assert!(
                !read.conditions[0].property.text.is_empty(),
                "{name} keeps a property"
            );
        }
    }

    #[test]
    fn the_parameters_of_the_operators_add_up_and_ignore_case_takes_none() {
        let read = parse_method_name("findByBornBetweenAndNameIgnoreCase").unwrap();
        assert_eq!(read.conditions.len(), 2);
        assert_eq!(read.conditions[0].operator, Operator::Between);
        assert_eq!(read.conditions[1].operator, Operator::Is);
        assert!(read.conditions[1].ignore_case);
        assert_eq!(read.parameters(), 3);
        assert_eq!(
            parse_method_name("findByNameIsNullAndActiveTrue")
                .unwrap()
                .parameters(),
            0
        );
        let all = parse_method_name("findByLastNameAllIgnoreCase").unwrap();
        assert!(all.all_ignore_case);
        assert_eq!(all.conditions[0].property.text, "LastName");
    }

    #[test]
    fn conditions_split_at_and_and_or_and_keep_the_connector() {
        let read = parse_method_name("findByLastNameAndCityOrCountry").unwrap();
        let found: Vec<(Connector, &str)> = read
            .conditions
            .iter()
            .map(|condition| (condition.connector, condition.property.text.as_str()))
            .collect();
        assert_eq!(
            found,
            [
                (Connector::And, "LastName"),
                (Connector::And, "City"),
                (Connector::Or, "Country")
            ]
        );
        assert_eq!(read.parameters(), 3);
    }

    #[test]
    fn an_order_by_keeps_the_direction_of_every_property() {
        let read = parse_method_name("findByCityOrderByLastNameAscCityDesc").unwrap();
        assert_eq!(read.conditions[0].property.text, "City");
        let orders: Vec<(&str, bool)> = read
            .orders
            .iter()
            .map(|order| (order.property.text.as_str(), order.descending))
            .collect();
        assert_eq!(orders, [("LastName", false), ("City", true)]);
        assert_eq!(read.parameters(), 1);
        let read = parse_method_name("findAllByOrderByLastName").unwrap();
        assert_eq!(read.orders[0].property.text, "LastName");
        assert!(!read.orders[0].descending);
    }

    #[test]
    fn a_caret_finds_the_word_of_the_part_it_types() {
        let name = "findByLastNameAndCi";
        let cursor = method_cursor(name, name.len()).unwrap();
        assert_eq!(cursor.boundary, Boundary::And);
        assert_eq!(&name[cursor.start..], "Ci");

        let name = "findByLastNa";
        let cursor = method_cursor(name, name.len()).unwrap();
        assert_eq!(cursor.boundary, Boundary::By);
        assert_eq!(&name[cursor.start..], "LastNa");

        let name = "findByCityOrderByLas";
        let cursor = method_cursor(name, name.len()).unwrap();
        assert_eq!(cursor.boundary, Boundary::OrderBy);
        assert_eq!(&name[cursor.start..], "Las");

        assert!(method_cursor("findBy", 5).is_none());
    }

    #[test]
    fn a_query_is_read_into_entities_aliases_properties_and_parameters() {
        let text = "select o from Owner o join o.pets p left join PetType t on p.type = t \
                    where o.lastName like :name and p.birthDate < :date";
        let query = parse_query(text);
        assert_eq!(
            query
                .aliases
                .iter()
                .map(|alias| (alias.name.as_str(), alias.source.clone()))
                .collect::<Vec<_>>(),
            [
                ("o", AliasSource::Entity("Owner".to_owned())),
                (
                    "p",
                    AliasSource::Path {
                        alias: "o".to_owned(),
                        property: "pets".to_owned()
                    }
                ),
                ("t", AliasSource::Entity("PetType".to_owned()))
            ]
        );
        let entities: Vec<&str> = query.entities.iter().map(|n| n.text.as_str()).collect();
        assert_eq!(entities, ["Owner", "PetType"]);
        let paths: Vec<&str> = query.properties.iter().map(|n| n.path.as_str()).collect();
        assert_eq!(
            paths,
            ["pets", "type", "lastName", "birthDate"],
            "{:?}",
            query.properties
        );
        let parameters: Vec<&str> = query.parameters.iter().map(|n| n.text.as_str()).collect();
        assert_eq!(parameters, ["name", "date"]);
        assert_eq!(
            &text[query.properties[2].range.clone()],
            "o.lastName",
            "a path keeps its own bytes"
        );
        let keywords: Vec<&str> = query
            .tokens
            .iter()
            .filter(|token| token.kind == TokenKind::Keyword)
            .map(|token| token.text.as_str())
            .collect();
        assert!(keywords.contains(&"select"));
        assert!(keywords.contains(&"where"));
        assert!(keywords.contains(&"join"));
        assert!(keywords.contains(&"left"));
    }

    #[test]
    fn a_string_literal_and_a_placeholder_are_their_own_tokens() {
        let query = parse_query("from Owner o where o.city = 'O''Neil' and o.age > :age");
        let literal = query
            .tokens
            .iter()
            .find(|token| token.kind == TokenKind::String)
            .expect("the literal");
        assert_eq!(literal.text, "'O''Neil'");
        assert_eq!(
            query
                .parameters
                .iter()
                .map(|parameter| parameter.text.as_str())
                .collect::<Vec<_>>(),
            ["age"]
        );
    }

    #[test]
    fn the_token_at_a_caret_is_the_name_it_stands_on() {
        let text = "from Owner o where o.city = :city";
        let query = parse_query(text);
        let offset = text.find("city").unwrap() + 2;
        let token = token_at(&query.tokens, offset).unwrap();
        assert_eq!(token.text, "city");
        assert_eq!(token.kind, TokenKind::Identifier);
    }

    #[test]
    fn nested_properties_of_a_condition_keep_their_own_place() {
        let name = "findByAddressCityAndLastName";
        let read = parse_method_name(name).unwrap();
        assert_eq!(properties(name)[0].0, "AddressCity");
        assert_eq!(read.conditions[0].property.start, "findBy".len());
        assert_eq!(read.conditions[0].property.end, "findByAddressCity".len());
        let separated = "findByAddress_City";
        let read = parse_method_name(separated).unwrap();
        assert_eq!(read.conditions[0].property.text, "Address_City");
    }
}
