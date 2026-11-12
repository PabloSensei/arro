//! The expressions of the variables of a template: what each part means, what a part
//! falls back to when the language does not know it, and the names the language gives
//! about the place of the caret.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use tree_sitter::Tree;

use crate::overrides::TemplateField;
use crate::template::TemplateLanguage;

/// The expressions a variable may use, as the settings page offers them.
pub const EXPRESSIONS: &[&str] = &[
    "className()",
    "methodName()",
    "packageName()",
    "date()",
    "time()",
    "user()",
    "clipboard()",
    "lineNumber()",
    "fileName()",
    "fileNameWithoutExtension()",
    "capitalize()",
    "decapitalize()",
    "camelCase()",
    "snakeCase()",
    "lowercaseAndDash()",
    "concat()",
];

/// The declarations around the caret, as the language reads them from the text and
/// its parse tree.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Declarations {
    pub class_name: String,
    pub method_name: String,
    pub package_name: String,
}

/// The function of a language that fills [`Declarations`] from the text and its tree.
pub type DeclarationsProvider = fn(&TemplateLanguage, &str, Option<&Tree>, usize) -> Declarations;

/// The declarations around `offset`, read by the language; empty names when the
/// language has no reader.
pub fn declarations(
    language: &TemplateLanguage,
    text: &str,
    tree: Option<&Tree>,
    offset: usize,
) -> Declarations {
    language
        .declarations
        .map(|provider| provider(language, text, tree, offset))
        .unwrap_or_default()
}

/// The values of the variables that do not come from the template itself.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Variables {
    pub declarations: Declarations,
    /// The name of the file with its extension.
    pub file_name: String,
    /// The line of the caret, from one.
    pub line_number: usize,
    pub clipboard: String,
    pub user: String,
    /// Today as `YYYY-MM-DD`.
    pub date: String,
    /// Now as `HH:MM`.
    pub time: String,
}

impl Variables {
    /// The values of this moment. `line_number` counts from one.
    pub fn now(
        declarations: Declarations,
        file_name: &str,
        line_number: usize,
        clipboard: &str,
    ) -> Self {
        let seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or_default();
        Self {
            declarations,
            file_name: file_name.to_owned(),
            line_number,
            clipboard: clipboard.to_owned(),
            user: crate::file::user_name(),
            date: crate::file::today(),
            time: time_of(seconds),
        }
    }
}

/// The time of day as `HH:MM`.
fn time_of(seconds: u64) -> String {
    let rest = seconds % 86_400;
    format!("{:02}:{:02}", rest / 3_600, rest % 3_600 / 60)
}

/// A part of an expression.
enum Term {
    Text(String),
    Variable(String),
    Call(String, Vec<Term>),
}

/// The number of arguments a known expression takes.
enum Arity {
    Exact(usize),
    Any,
}

fn arity(name: &str) -> Option<Arity> {
    const WITHOUT_ARGUMENTS: &[&str] = &[
        "className",
        "methodName",
        "packageName",
        "date",
        "time",
        "user",
        "clipboard",
        "lineNumber",
        "fileName",
        "fileNameWithoutExtension",
    ];
    const WITH_ONE_ARGUMENT: &[&str] = &[
        "capitalize",
        "decapitalize",
        "camelCase",
        "snakeCase",
        "lowercaseAndDash",
    ];
    if WITHOUT_ARGUMENTS.contains(&name) {
        Some(Arity::Exact(0))
    } else if WITH_ONE_ARGUMENT.contains(&name) {
        Some(Arity::Exact(1))
    } else if name == "concat" {
        Some(Arity::Any)
    } else {
        None
    }
}

struct Reader<'a> {
    source: &'a str,
    at: usize,
}

impl Reader<'_> {
    fn peek(&self) -> Option<char> {
        self.source[self.at..].chars().next()
    }

    fn bump(&mut self) -> Option<char> {
        let character = self.peek()?;
        self.at += character.len_utf8();
        Some(character)
    }

    fn skip_space(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.bump();
        }
    }

    fn name(&mut self) -> String {
        let start = self.at;
        while self
            .peek()
            .is_some_and(|character| character.is_alphanumeric() || character == '_')
        {
            self.bump();
        }
        self.source[start..self.at].to_owned()
    }

    fn term(&mut self) -> Result<Term, ()> {
        self.skip_space();
        let character = self.peek().ok_or(())?;
        if character == '"' {
            self.bump();
            let mut text = String::new();
            loop {
                match self.bump().ok_or(())? {
                    '"' => return Ok(Term::Text(text)),
                    '\\' => {
                        let escaped = self.bump().ok_or(())?;
                        text.push(match escaped {
                            'n' => '\n',
                            't' => '\t',
                            other => other,
                        });
                    }
                    other => text.push(other),
                }
            }
        }
        if character.is_alphabetic() || character == '_' {
            let name = self.name();
            self.skip_space();
            if self.peek() != Some('(') {
                return Ok(Term::Variable(name));
            }
            self.bump();
            let mut arguments = Vec::new();
            self.skip_space();
            if self.peek() == Some(')') {
                self.bump();
                return Ok(Term::Call(name, arguments));
            }
            loop {
                arguments.push(self.term()?);
                self.skip_space();
                match self.bump().ok_or(())? {
                    ',' => self.skip_space(),
                    ')' => break,
                    _ => return Err(()),
                }
            }
            return Ok(Term::Call(name, arguments));
        }
        Err(())
    }
}

fn parse(source: &str) -> Result<Term, ()> {
    let mut reader = Reader { source, at: 0 };
    let term = reader.term()?;
    reader.skip_space();
    if reader.at < source.len() {
        return Err(());
    }
    Ok(term)
}

/// Why the expression falls back to the default value, when it does. `names` are the
/// variables of the template, the ones a bare name may refer to.
pub fn expression_warning(expression: &str, names: &[String]) -> Option<String> {
    if expression.trim().is_empty() {
        return None;
    }
    let term = match parse(expression) {
        Ok(term) => term,
        Err(()) => {
            return Some("The expression is not read; the default value is used.".to_owned());
        }
    };
    warning_of(&term, names)
}

fn warning_of(term: &Term, names: &[String]) -> Option<String> {
    match term {
        Term::Text(_) => None,
        Term::Variable(name) => (!names.iter().any(|known| known == name))
            .then(|| format!("Unknown variable {name}; the default value is used.")),
        Term::Call(name, arguments) => {
            let fits = match arity(name) {
                None => false,
                Some(Arity::Exact(count)) => arguments.len() == count,
                Some(Arity::Any) => true,
            };
            if !fits {
                return Some(match arity(name) {
                    Some(Arity::Exact(count)) => {
                        format!("{name}() takes {count} argument(s); the default value is used.")
                    }
                    _ => format!("Unknown expression {name}(); the default value is used."),
                });
            }
            arguments
                .iter()
                .find_map(|argument| warning_of(argument, names))
        }
    }
}

/// The values of the variables of one expansion, each one computed once.
pub(crate) struct Values<'a> {
    fields: &'a [TemplateField],
    variables: &'a Variables,
    cache: HashMap<String, String>,
    /// The fields being computed, so that a reference back to one of them is empty.
    path: Vec<String>,
}

impl<'a> Values<'a> {
    pub(crate) fn new(fields: &'a [TemplateField], variables: &'a Variables) -> Self {
        Self {
            fields,
            variables,
            cache: HashMap::new(),
            path: Vec::new(),
        }
    }

    /// The value of the variable `name`: its expression, or its default when the
    /// expression is missing, empty or not known.
    pub(crate) fn value_of(&mut self, name: &str) -> String {
        if let Some(known) = self.cache.get(name) {
            return known.clone();
        }
        let Some(field) = self.fields.iter().find(|field| field.name == name).cloned() else {
            return String::new();
        };
        if self.path.iter().any(|at| at == name) {
            return String::new();
        }
        self.path.push(name.to_owned());
        let computed = if field.expression.trim().is_empty() {
            None
        } else {
            parse(&field.expression).ok().map(|term| self.term(&term))
        };
        self.path.pop();
        let value = match computed {
            Some(value) if !value.is_empty() => value,
            _ => field.default,
        };
        self.cache.insert(name.to_owned(), value.clone());
        value
    }

    fn term(&mut self, term: &Term) -> String {
        match term {
            Term::Text(text) => text.clone(),
            Term::Variable(name) => self.value_of(name),
            Term::Call(name, arguments) => self.call(name, arguments),
        }
    }

    fn call(&mut self, name: &str, arguments: &[Term]) -> String {
        let declarations = &self.variables.declarations;
        match (name, arguments) {
            ("className", []) => declarations.class_name.clone(),
            ("methodName", []) => declarations.method_name.clone(),
            ("packageName", []) => declarations.package_name.clone(),
            ("date", []) => self.variables.date.clone(),
            ("time", []) => self.variables.time.clone(),
            ("user", []) => self.variables.user.clone(),
            ("clipboard", []) => self.variables.clipboard.clone(),
            ("lineNumber", []) => self.variables.line_number.to_string(),
            ("fileName", []) => self.variables.file_name.clone(),
            ("fileNameWithoutExtension", []) => without_extension(&self.variables.file_name),
            ("capitalize", [argument]) => capitalize(&self.term(argument)),
            ("decapitalize", [argument]) => decapitalize(&self.term(argument)),
            ("camelCase", [argument]) => camel_case(&self.term(argument)),
            ("snakeCase", [argument]) => snake_case(&self.term(argument)),
            ("lowercaseAndDash", [argument]) => lowercase_and_dash(&self.term(argument)),
            ("concat", arguments) => arguments
                .iter()
                .map(|argument| self.term(argument))
                .collect(),
            _ => String::new(),
        }
    }
}

/// The name of the file without its last extension.
fn without_extension(name: &str) -> String {
    match name.rfind('.') {
        Some(at) if at > 0 => name[..at].to_owned(),
        _ => name.to_owned(),
    }
}

fn capitalized(text: &str, first: impl Fn(char) -> char) -> String {
    let mut characters = text.chars();
    match characters.next() {
        Some(character) => first(character).to_string() + characters.as_str(),
        None => String::new(),
    }
}

fn capitalize(text: &str) -> String {
    capitalized(text, |character| {
        character.to_uppercase().next().unwrap_or(character)
    })
}

fn decapitalize(text: &str) -> String {
    capitalized(text, |character| {
        character.to_lowercase().next().unwrap_or(character)
    })
}

/// The words of a name: split at characters that are not letters or digits and where
/// a lower-case word turns into an upper-case one.
fn words(text: &str) -> Vec<String> {
    let characters: Vec<char> = text.chars().collect();
    let mut words = Vec::new();
    let mut current = String::new();
    for (index, character) in characters.iter().enumerate() {
        if !character.is_alphanumeric() {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            continue;
        }
        let previous = index.checked_sub(1).map(|at| characters[at]);
        let next = characters.get(index + 1);
        let turns = character.is_uppercase()
            && previous.is_some_and(|previous| {
                previous.is_lowercase()
                    || previous.is_numeric()
                    || (previous.is_uppercase() && next.is_some_and(|next| next.is_lowercase()))
            });
        if turns && !current.is_empty() {
            words.push(std::mem::take(&mut current));
        }
        current.push(*character);
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

fn camel_case(text: &str) -> String {
    let mut out = String::new();
    for (index, word) in words(text).iter().enumerate() {
        let word = word.to_lowercase();
        if index == 0 {
            out.push_str(&word);
        } else {
            out.push_str(&capitalize(&word));
        }
    }
    out
}

fn snake_case(text: &str) -> String {
    words(text)
        .iter()
        .map(|word| word.to_lowercase())
        .collect::<Vec<_>>()
        .join("_")
}

fn lowercase_and_dash(text: &str) -> String {
    words(text)
        .iter()
        .map(|word| word.to_lowercase())
        .collect::<Vec<_>>()
        .join("-")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(name: &str, expression: &str) -> TemplateField {
        TemplateField {
            name: name.to_owned(),
            expression: expression.to_owned(),
            ..TemplateField::default()
        }
    }

    fn value(fields: &[TemplateField], variables: &Variables, name: &str) -> String {
        Values::new(fields, variables).value_of(name)
    }

    fn variables() -> Variables {
        Variables {
            declarations: Declarations {
                class_name: "Order".to_owned(),
                method_name: "total".to_owned(),
                package_name: "com.shop".to_owned(),
            },
            file_name: "Order.txt".to_owned(),
            line_number: 12,
            clipboard: "copied".to_owned(),
            user: "me".to_owned(),
            date: "2026-11-12".to_owned(),
            time: "09:30".to_owned(),
        }
    }

    #[test]
    fn every_expression_has_its_value() {
        let cases = [
            ("className()", "Order"),
            ("methodName()", "total"),
            ("packageName()", "com.shop"),
            ("date()", "2026-11-12"),
            ("time()", "09:30"),
            ("user()", "me"),
            ("clipboard()", "copied"),
            ("lineNumber()", "12"),
            ("fileName()", "Order.txt"),
            ("fileNameWithoutExtension()", "Order"),
            ("capitalize(\"order total\")", "Order total"),
            ("decapitalize(\"Order\")", "order"),
            ("camelCase(\"order total\")", "orderTotal"),
            ("snakeCase(\"orderTotal\")", "order_total"),
            ("lowercaseAndDash(\"orderTotal\")", "order-total"),
            ("concat(\"a\", \"-\", \"b\")", "a-b"),
            ("concat(fileNameWithoutExtension(), \"!\")", "Order!"),
            ("capitalize(camelCase(\"my order\"))", "MyOrder"),
        ];
        for (expression, expected) in cases {
            let fields = [field("NAME", expression)];
            assert_eq!(
                value(&fields, &variables(), "NAME"),
                expected,
                "{expression}"
            );
        }
        assert_eq!(capitalize(""), "");
        assert_eq!(camel_case("my_name"), "myName");
        assert_eq!(snake_case("HTTPServer"), "http_server");
    }

    #[test]
    fn an_unknown_expression_falls_back_to_the_default() {
        let mut rich = field("NAME", "noSuchExpression(\"x\")");
        rich.default = "fallback".to_owned();
        let fields = [rich];
        assert_eq!(value(&fields, &variables(), "NAME"), "fallback");
        assert_eq!(
            expression_warning("noSuchExpression(\"x\")", &[]).unwrap(),
            "Unknown expression noSuchExpression(); the default value is used."
        );
        assert!(expression_warning("not read(", &[]).is_some());
        assert!(expression_warning("", &[]).is_none());
        assert!(expression_warning("className()", &[]).is_none());
        assert!(expression_warning("capitalize()", &[]).is_some());
        assert_eq!(
            expression_warning("$END$", &[]).unwrap(),
            "The expression is not read; the default value is used."
        );
    }

    #[test]
    fn a_reference_takes_the_value_of_another_variable() {
        let fields = [
            field("CLASS", "className()"),
            field("DESCRIPTION", "concat(CLASS, \" record\")"),
        ];
        assert_eq!(value(&fields, &variables(), "DESCRIPTION"), "Order record");
        assert_eq!(
            expression_warning("concat(MISSING, \"x\")", &["CLASS".to_owned()]).unwrap(),
            "Unknown variable MISSING; the default value is used."
        );
    }

    #[test]
    fn a_reference_back_to_the_same_variable_takes_no_value() {
        let mut looping = field("NAME", "NAME");
        looping.default = "start".to_owned();
        let fields = [looping];
        assert_eq!(value(&fields, &variables(), "NAME"), "start");

        let fields = [field("NAME", "concat(NAME, \"x\")")];
        assert_eq!(value(&fields, &variables(), "NAME"), "x");
    }

    #[test]
    fn the_time_of_day_has_two_digits() {
        assert_eq!(time_of(0), "00:00");
        assert_eq!(time_of(34_200), "09:30");
        assert_eq!(time_of(86_399), "23:59");
    }

    #[test]
    fn the_reader_of_the_language_gives_its_names() {
        use std::sync::OnceLock;
        static LANGUAGE: OnceLock<TemplateLanguage> = OnceLock::new();
        let language = LANGUAGE.get_or_init(|| TemplateLanguage {
            templates: &[],
            contexts: "",
            grammar: || panic!("the declarations test does not parse"),
            indent_unit: "    ",
            receiver_prefixes: &[],
            query: OnceLock::new(),
            declarations: Some(|_, _, _, _| Declarations {
                class_name: "Widget".to_owned(),
                ..Declarations::default()
            }),
        });
        assert_eq!(declarations(language, "", None, 0).class_name, "Widget");
        assert_eq!(
            variables().declarations,
            Declarations {
                class_name: "Order".to_owned(),
                method_name: "total".to_owned(),
                package_name: "com.shop".to_owned(),
            }
        );
    }
}
