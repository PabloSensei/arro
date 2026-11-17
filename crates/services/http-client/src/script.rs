//! The script of a response handler: the small part of the language of the other clients
//! that these scripts use. There is no engine here: the statements are read and run at
//! once, and everything outside the subset stops with a line number and a message.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::execute::Response;

/// One `client.test` of a script.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestResult {
    pub name: String,
    pub passed: bool,
    /// What the first failed assertion said.
    pub message: Option<String>,
}

/// A line of a script that cannot run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScriptError {
    /// The line of the script, from one.
    pub line: usize,
    pub message: String,
}

impl std::fmt::Display for ScriptError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "Line {}: {}", self.line, self.message)
    }
}

/// What a script did with the answer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScriptOutput {
    pub tests: Vec<TestResult>,
    /// The lines of `client.log`.
    pub logs: Vec<String>,
    /// The place of the script that stopped it, when one did.
    pub error: Option<ScriptError>,
}

/// Runs the script of a handler over the answer. `globals` keeps the values of
/// `client.global` between requests.
pub fn run(
    source: &str,
    response: &Response,
    globals: &mut BTreeMap<String, String>,
) -> ScriptOutput {
    let tokens = match lex(source) {
        Ok(tokens) => tokens,
        Err(error) => {
            return ScriptOutput {
                error: Some(error),
                ..ScriptOutput::default()
            };
        }
    };
    let mut run = Run {
        tokens,
        at: 0,
        last_line: 1,
        response,
        globals,
        locals: BTreeMap::new(),
        tests: Vec::new(),
        logs: Vec::new(),
    };
    let error = run.program().err().map(Into::into);
    ScriptOutput {
        tests: run.tests,
        logs: run.logs,
        error,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Equality {
    Strict,
    StrictNot,
    Loose,
    LooseNot,
}

#[derive(Clone, Debug, PartialEq)]
enum Token {
    Ident(String),
    Number(f64),
    String(String),
    Punct(&'static str),
    Eof,
}

struct Lexed {
    token: Token,
    line: usize,
}

/// A statement that cannot run. `assertion` marks the failure of a `client.assert`, which
/// only fails the test around it.
struct Failure {
    line: usize,
    message: String,
    assertion: bool,
}

impl Failure {
    fn error(line: usize, message: impl Into<String>) -> Self {
        Self {
            line,
            message: message.into(),
            assertion: false,
        }
    }

    fn assertion(line: usize, message: impl Into<String>) -> Self {
        Self {
            line,
            message: message.into(),
            assertion: true,
        }
    }
}

impl From<Failure> for ScriptError {
    fn from(failure: Failure) -> Self {
        Self {
            line: failure.line,
            message: failure.message,
        }
    }
}

struct Run<'a> {
    tokens: Vec<Lexed>,
    at: usize,
    last_line: usize,
    response: &'a Response,
    globals: &'a mut BTreeMap<String, String>,
    locals: BTreeMap<String, Value>,
    tests: Vec<TestResult>,
    logs: Vec<String>,
}

impl Run<'_> {
    fn program(&mut self) -> Result<(), Failure> {
        while !matches!(self.peek(), Token::Eof) {
            self.statement()?;
        }
        Ok(())
    }

    fn statement(&mut self) -> Result<(), Failure> {
        let line = self.line();
        let Some(name) = self.ident() else {
            return Err(Failure::error(
                line,
                format!("unsupported statement: {}", self.token_text()),
            ));
        };
        match name.as_str() {
            "let" | "var" | "const" => {
                let name = self
                    .ident()
                    .ok_or_else(|| Failure::error(line, "the declaration has no name"))?;
                self.expect_punct("=")?;
                let value = self.expression()?;
                self.locals.insert(name, value);
                self.end_statement()?;
                Ok(())
            }
            "client" => {
                self.expect_punct(".")?;
                self.client_call(line)?;
                self.end_statement()?;
                Ok(())
            }
            other => Err(Failure::error(
                line,
                format!("unsupported statement: {other}"),
            )),
        }
    }

    fn client_call(&mut self, line: usize) -> Result<(), Failure> {
        let Some(member) = self.ident() else {
            return Err(Failure::error(line, "the client has no member"));
        };
        match member.as_str() {
            "log" => {
                self.expect_punct("(")?;
                let mut parts = Vec::new();
                while !self.eat_punct(")") {
                    parts.push(as_text(&self.expression()?));
                    if !self.eat_punct(",") {
                        self.expect_punct(")")?;
                        break;
                    }
                }
                self.logs.push(parts.join(" "));
                Ok(())
            }
            "assert" => {
                self.expect_punct("(")?;
                let condition = self.expression()?;
                let message = if self.eat_punct(",") {
                    as_text(&self.expression()?)
                } else {
                    "the assertion failed".to_owned()
                };
                self.expect_punct(")")?;
                if truthy(&condition) {
                    Ok(())
                } else {
                    Err(Failure::assertion(line, message))
                }
            }
            "test" => self.test(line),
            "global" => {
                self.expect_punct(".")?;
                let Some(method) = self.ident() else {
                    return Err(Failure::error(line, "client.global has no method"));
                };
                if method != "set" {
                    return Err(Failure::error(
                        line,
                        format!("unsupported call: client.global.{method}"),
                    ));
                }
                self.expect_punct("(")?;
                let name = match self.expression()? {
                    Value::String(name) => name,
                    other => {
                        return Err(Failure::error(
                            line,
                            format!("the name of a global variable is not a string: {other}"),
                        ));
                    }
                };
                self.expect_punct(",")?;
                let value = self.expression()?;
                self.expect_punct(")")?;
                self.globals.insert(name, as_text(&value));
                Ok(())
            }
            other => Err(Failure::error(
                line,
                format!("unsupported call: client.{other}"),
            )),
        }
    }

    fn test(&mut self, line: usize) -> Result<(), Failure> {
        self.expect_punct("(")?;
        let name = as_text(&self.expression()?);
        self.expect_punct(",")?;
        let Some(keyword) = self.ident() else {
            return Err(Failure::error(line, "client.test takes a function"));
        };
        if keyword != "function" {
            return Err(Failure::error(
                line,
                format!("client.test takes a function, not {keyword}"),
            ));
        }
        self.expect_punct("(")?;
        self.expect_punct(")")?;
        self.expect_punct("{")?;
        let index = self.tests.len();
        self.tests.push(TestResult {
            name,
            passed: true,
            message: None,
        });
        loop {
            if self.eat_punct("}") {
                break;
            }
            if matches!(self.peek(), Token::Eof) {
                return Err(Failure::error(line, "the test has no closing brace"));
            }
            match self.statement() {
                Ok(()) => {}
                Err(failure) if failure.assertion => {
                    self.tests[index].passed = false;
                    self.tests[index].message = Some(failure.message);
                    self.skip_block()?;
                    break;
                }
                Err(failure) => {
                    self.tests[index].passed = false;
                    self.tests[index].message = Some(failure.message.clone());
                    return Err(failure);
                }
            }
        }
        self.expect_punct(")")?;
        Ok(())
    }

    /// Steps over the rest of a test whose assertion failed, up to its closing brace.
    fn skip_block(&mut self) -> Result<(), Failure> {
        let line = self.line();
        let mut depth = 1usize;
        while depth > 0 {
            match self.peek() {
                Token::Eof => return Err(Failure::error(line, "the test has no closing brace")),
                Token::Punct("{") => depth += 1,
                Token::Punct("}") => depth -= 1,
                _ => {}
            }
            self.at += 1;
        }
        Ok(())
    }

    fn expression(&mut self) -> Result<Value, Failure> {
        self.or_expression()
    }

    fn or_expression(&mut self) -> Result<Value, Failure> {
        let mut left = self.and_expression()?;
        while self.eat_punct("||") {
            let right = self.and_expression()?;
            left = Value::Bool(truthy(&left) || truthy(&right));
        }
        Ok(left)
    }

    fn and_expression(&mut self) -> Result<Value, Failure> {
        let mut left = self.equality()?;
        while self.eat_punct("&&") {
            let right = self.equality()?;
            left = Value::Bool(truthy(&left) && truthy(&right));
        }
        Ok(left)
    }

    fn equality(&mut self) -> Result<Value, Failure> {
        let mut left = self.comparison()?;
        loop {
            let operator = if self.eat_punct("===") {
                Some(Equality::Strict)
            } else if self.eat_punct("!==") {
                Some(Equality::StrictNot)
            } else if self.eat_punct("==") {
                Some(Equality::Loose)
            } else if self.eat_punct("!=") {
                Some(Equality::LooseNot)
            } else {
                None
            };
            let Some(operator) = operator else {
                break;
            };
            let right = self.comparison()?;
            let equal = match operator {
                Equality::Strict => left == right,
                Equality::StrictNot => left != right,
                Equality::Loose => loose_equal(&left, &right),
                Equality::LooseNot => !loose_equal(&left, &right),
            };
            left = Value::Bool(equal);
        }
        Ok(left)
    }

    fn comparison(&mut self) -> Result<Value, Failure> {
        let mut left = self.additive()?;
        loop {
            let operator = ["<=", ">=", "<", ">"]
                .into_iter()
                .find(|operator| self.eat_punct(operator));
            let Some(operator) = operator else {
                break;
            };
            let line = self.line();
            let right = self.additive()?;
            let ordering = compare(&left, &right)
                .ok_or_else(|| Failure::error(line, "the values cannot be compared"))?;
            let result = match operator {
                "<=" => ordering.is_le(),
                ">=" => ordering.is_ge(),
                "<" => ordering.is_lt(),
                _ => ordering.is_gt(),
            };
            left = Value::Bool(result);
        }
        Ok(left)
    }

    fn additive(&mut self) -> Result<Value, Failure> {
        let mut left = self.unary()?;
        while self.eat_punct("+") {
            let right = self.unary()?;
            left = match (&left, &right) {
                (Value::Number(left), Value::Number(right)) => {
                    number_value(left.as_f64().unwrap_or(0.) + right.as_f64().unwrap_or(0.))
                }
                _ => Value::String(format!("{}{}", as_text(&left), as_text(&right))),
            };
        }
        Ok(left)
    }

    fn unary(&mut self) -> Result<Value, Failure> {
        if self.eat_punct("!") {
            return Ok(Value::Bool(!truthy(&self.unary()?)));
        }
        if self.eat_punct("-") {
            let value = self.unary()?;
            let number = value
                .as_f64()
                .ok_or_else(|| Failure::error(self.line(), "the value is not a number"))?;
            return Ok(number_value(-number));
        }
        self.primary()
    }

    fn primary(&mut self) -> Result<Value, Failure> {
        let line = self.line();
        let value = match self.peek().clone() {
            Token::String(text) => {
                self.at += 1;
                Value::String(text)
            }
            Token::Number(number) => {
                self.at += 1;
                number_value(number)
            }
            Token::Punct("(") => {
                self.at += 1;
                let value = self.expression()?;
                self.expect_punct(")")?;
                value
            }
            Token::Ident(name) => {
                self.at += 1;
                match name.as_str() {
                    "true" => Value::Bool(true),
                    "false" => Value::Bool(false),
                    "null" => Value::Null,
                    "response" => self.response_member(line)?,
                    "client" => self.client_expression(line)?,
                    _ => match self.locals.get(&name) {
                        Some(value) => value.clone(),
                        None => return Err(Failure::error(line, format!("unknown name: {name}"))),
                    },
                }
            }
            other => {
                return Err(Failure::error(
                    line,
                    format!("unsupported expression: {}", token_text(&other)),
                ));
            }
        };
        self.postfix(value, line)
    }

    /// The path after a value: `.field`, `[index]` and `.length`.
    fn postfix(&mut self, mut value: Value, line: usize) -> Result<Value, Failure> {
        loop {
            if self.eat_punct(".") {
                let Some(field) = self.ident() else {
                    return Err(Failure::error(line, "the path has no field"));
                };
                value = field_of(&value, &field, line)?;
            } else if self.eat_punct("[") {
                let key = self.expression()?;
                self.expect_punct("]")?;
                value = index_of(&value, &key, line)?;
            } else {
                return Ok(value);
            }
        }
    }

    fn response_member(&mut self, line: usize) -> Result<Value, Failure> {
        self.expect_punct(".")?;
        let Some(member) = self.ident() else {
            return Err(Failure::error(line, "response has no member"));
        };
        match member.as_str() {
            "status" => Ok(Value::Number(self.response.status.into())),
            "body" => {
                let mut value = body_value(self.response);
                loop {
                    if self.eat_punct(".") {
                        let Some(field) = self.ident() else {
                            return Err(Failure::error(line, "the path has no field"));
                        };
                        value = field_of(&value, &field, line)?;
                    } else if self.eat_punct("[") {
                        let key = self.expression()?;
                        self.expect_punct("]")?;
                        value = index_of(&value, &key, line)?;
                    } else {
                        break;
                    }
                }
                Ok(value)
            }
            "headers" => {
                self.expect_punct(".")?;
                let Some(method) = self.ident() else {
                    return Err(Failure::error(line, "response.headers has no method"));
                };
                if method != "valueOf" {
                    return Err(Failure::error(
                        line,
                        format!("unsupported call: response.headers.{method}"),
                    ));
                }
                self.expect_punct("(")?;
                let name = as_text(&self.expression()?);
                self.expect_punct(")")?;
                Ok(self
                    .response
                    .header(&name)
                    .map_or(Value::Null, |value| Value::String(value.to_owned())))
            }
            "contentType" => {
                self.expect_punct(".")?;
                let Some(member) = self.ident() else {
                    return Err(Failure::error(line, "response.contentType has no member"));
                };
                if member != "mimeType" {
                    return Err(Failure::error(
                        line,
                        format!("unsupported member: response.contentType.{member}"),
                    ));
                }
                Ok(self
                    .response
                    .header("Content-Type")
                    .map_or(Value::Null, |value| {
                        Value::String(value.split(';').next().unwrap_or(value).trim().to_owned())
                    }))
            }
            other => Err(Failure::error(
                line,
                format!("unsupported member: response.{other}"),
            )),
        }
    }

    fn client_expression(&mut self, line: usize) -> Result<Value, Failure> {
        self.expect_punct(".")?;
        let Some(member) = self.ident() else {
            return Err(Failure::error(line, "the client has no member"));
        };
        if member != "global" {
            return Err(Failure::error(
                line,
                format!("unsupported expression: client.{member}"),
            ));
        }
        self.expect_punct(".")?;
        let Some(method) = self.ident() else {
            return Err(Failure::error(line, "client.global has no method"));
        };
        if method != "get" {
            return Err(Failure::error(
                line,
                format!("unsupported call: client.global.{method}"),
            ));
        }
        self.expect_punct("(")?;
        let name = as_text(&self.expression()?);
        self.expect_punct(")")?;
        Ok(self
            .globals
            .get(&name)
            .map_or(Value::Null, |value| Value::String(value.clone())))
    }

    fn end_statement(&mut self) -> Result<(), Failure> {
        self.eat_punct(";");
        Ok(())
    }

    fn peek(&self) -> &Token {
        self.tokens
            .get(self.at)
            .map_or(&Token::Eof, |token| &token.token)
    }

    fn line(&self) -> usize {
        self.tokens
            .get(self.at)
            .map_or(self.last_line, |token| token.line)
    }

    fn token_text(&self) -> String {
        token_text(self.peek())
    }

    fn eat_punct(&mut self, punct: &str) -> bool {
        if let Token::Punct(found) = self.peek()
            && *found == punct
        {
            self.last_line = self.line();
            self.at += 1;
            return true;
        }
        false
    }

    fn expect_punct(&mut self, punct: &str) -> Result<(), Failure> {
        if self.eat_punct(punct) {
            return Ok(());
        }
        Err(Failure::error(
            self.line(),
            format!("expected {punct}, found {}", self.token_text()),
        ))
    }

    fn ident(&mut self) -> Option<String> {
        if let Token::Ident(name) = self.peek().clone() {
            self.last_line = self.line();
            self.at += 1;
            Some(name)
        } else {
            None
        }
    }
}

fn body_value(response: &Response) -> Value {
    serde_json::from_slice(&response.body).unwrap_or(Value::Null)
}

fn number_value(number: f64) -> Value {
    if number.fract() == 0. && number >= i64::MIN as f64 && number <= i64::MAX as f64 {
        return Value::Number((number as i64).into());
    }
    serde_json::Number::from_f64(number).map_or(Value::Null, Value::Number)
}

fn field_of(value: &Value, field: &str, line: usize) -> Result<Value, Failure> {
    if field == "length" {
        return Ok(match value {
            Value::Array(items) => Value::Number(items.len().into()),
            Value::String(text) => Value::Number(text.chars().count().into()),
            Value::Object(map) => Value::Number(map.len().into()),
            _ => Value::Null,
        });
    }
    match value {
        Value::Object(map) => Ok(map.get(field).cloned().unwrap_or(Value::Null)),
        _ => Err(Failure::error(
            line,
            format!("response.body has no field {field}"),
        )),
    }
}

fn index_of(value: &Value, key: &Value, line: usize) -> Result<Value, Failure> {
    match (value, key) {
        (Value::Array(items), Value::Number(index)) => {
            let index = index.as_u64().unwrap_or(u64::MAX) as usize;
            Ok(items.get(index).cloned().unwrap_or(Value::Null))
        }
        (Value::Object(map), Value::String(key)) => {
            Ok(map.get(key).cloned().unwrap_or(Value::Null))
        }
        _ => Err(Failure::error(
            line,
            "the value is not an array or an object with that key",
        )),
    }
}

fn as_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => "null".to_owned(),
        other => other.to_string(),
    }
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(number) => number.as_f64().is_some_and(|number| number != 0.),
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(map) => !map.is_empty(),
    }
}

fn loose_equal(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Number(_), Value::String(_))
        | (Value::String(_), Value::Number(_))
        | (Value::Bool(_), Value::String(_))
        | (Value::String(_), Value::Bool(_)) => as_text(left) == as_text(right),
        _ => left == right,
    }
}

fn compare(left: &Value, right: &Value) -> Option<std::cmp::Ordering> {
    match (left, right) {
        (Value::Number(left), Value::Number(right)) => left.as_f64()?.partial_cmp(&right.as_f64()?),
        (Value::String(left), Value::String(right)) => Some(left.cmp(right)),
        _ => None,
    }
}

fn token_text(token: &Token) -> String {
    match token {
        Token::Ident(name) => name.clone(),
        Token::Number(number) => number.to_string(),
        Token::String(text) => format!("\"{text}\""),
        Token::Punct(punct) => (*punct).to_owned(),
        Token::Eof => "the end of the script".to_owned(),
    }
}

fn lex(source: &str) -> Result<Vec<Lexed>, ScriptError> {
    let characters: Vec<char> = source.chars().collect();
    let mut tokens = Vec::new();
    let mut at = 0;
    let mut line = 1;
    while at < characters.len() {
        let character = characters[at];
        if character == '\n' {
            line += 1;
            at += 1;
            continue;
        }
        if character.is_whitespace() {
            at += 1;
            continue;
        }
        if character == '/' && characters.get(at + 1) == Some(&'/') {
            while at < characters.len() && characters[at] != '\n' {
                at += 1;
            }
            continue;
        }
        if character == '/' && characters.get(at + 1) == Some(&'*') {
            at += 2;
            while at < characters.len()
                && !(characters[at] == '*' && characters.get(at + 1) == Some(&'/'))
            {
                if characters[at] == '\n' {
                    line += 1;
                }
                at += 1;
            }
            at += 2;
            continue;
        }
        if character == '"' || character == '\'' {
            let quote = character;
            let start = line;
            at += 1;
            let mut text = String::new();
            loop {
                match characters.get(at) {
                    None => {
                        return Err(ScriptError {
                            line: start,
                            message: "a string is not closed".to_owned(),
                        });
                    }
                    Some(&found) if found == quote => {
                        at += 1;
                        break;
                    }
                    Some('\\') => {
                        at += 1;
                        match characters.get(at) {
                            Some('n') => text.push('\n'),
                            Some('r') => text.push('\r'),
                            Some('t') => text.push('\t'),
                            Some(other) => text.push(*other),
                            None => {}
                        }
                        at += 1;
                    }
                    Some(&other) => {
                        if other == '\n' {
                            line += 1;
                        }
                        text.push(other);
                        at += 1;
                    }
                }
            }
            tokens.push(Lexed {
                token: Token::String(text),
                line: start,
            });
            continue;
        }
        if character.is_ascii_digit() {
            let start = at;
            while characters
                .get(at)
                .is_some_and(|character| character.is_ascii_digit())
            {
                at += 1;
            }
            if characters.get(at) == Some(&'.')
                && characters
                    .get(at + 1)
                    .is_some_and(|character| character.is_ascii_digit())
            {
                at += 1;
                while characters
                    .get(at)
                    .is_some_and(|character| character.is_ascii_digit())
                {
                    at += 1;
                }
            }
            let text: String = characters[start..at].iter().collect();
            let number = text.parse().unwrap_or(0.);
            tokens.push(Lexed {
                token: Token::Number(number),
                line,
            });
            continue;
        }
        if character.is_alphabetic() || character == '_' || character == '$' {
            let start = at;
            while characters.get(at).is_some_and(|character| {
                character.is_alphanumeric() || *character == '_' || *character == '$'
            }) {
                at += 1;
            }
            let text: String = characters[start..at].iter().collect();
            tokens.push(Lexed {
                token: Token::Ident(text),
                line,
            });
            continue;
        }
        let rest: String = characters[at..characters.len().min(at + 3)]
            .iter()
            .collect();
        let punct = [
            "===", "!==", "==", "!=", ">=", "<=", "&&", "||", ".", ",", ";", "(", ")", "{", "}",
            "[", "]", "=", "!", ">", "<", "+",
        ]
        .into_iter()
        .find(|punct| rest.starts_with(punct))
        .ok_or_else(|| ScriptError {
            line,
            message: format!("unsupported character: {character}"),
        })?;
        tokens.push(Lexed {
            token: Token::Punct(punct),
            line,
        });
        at += punct.len();
    }
    Ok(tokens)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::Duration;

    use super::{ScriptOutput, run};
    use crate::execute::Response;

    fn response(body: &str) -> Response {
        Response {
            status: 200,
            reason: "OK".to_owned(),
            headers: vec![
                (
                    "Content-Type".to_owned(),
                    "application/json; charset=utf-8".to_owned(),
                ),
                ("X-Token".to_owned(), "abc".to_owned()),
            ],
            body: body.as_bytes().to_vec(),
            elapsed: Duration::from_millis(3),
        }
    }

    fn run_on(body: &str, source: &str) -> (ScriptOutput, BTreeMap<String, String>) {
        let mut globals = BTreeMap::new();
        let output = run(source, &response(body), &mut globals);
        (output, globals)
    }

    #[test]
    fn a_global_takes_a_field_of_the_body() {
        let (output, globals) = run_on(
            r#"{"token":"abc","owner":{"id":7}}"#,
            "client.global.set(\"token\", response.body.token);\n\
             client.global.set(\"id\", response.body.owner.id);",
        );
        assert_eq!(output.error, None);
        assert_eq!(globals.get("token").map(String::as_str), Some("abc"));
        assert_eq!(globals.get("id").map(String::as_str), Some("7"));
    }

    #[test]
    fn a_global_takes_a_header_and_the_content_type() {
        let (output, globals) = run_on(
            "{}",
            "client.global.set(\"token\", response.headers.valueOf(\"x-token\"));\n\
             client.global.set(\"type\", response.contentType.mimeType);",
        );
        assert_eq!(output.error, None);
        assert_eq!(globals.get("token").map(String::as_str), Some("abc"));
        assert_eq!(
            globals.get("type").map(String::as_str),
            Some("application/json")
        );
    }

    #[test]
    fn an_item_of_an_array_is_reached_by_its_number() {
        let (output, globals) = run_on(
            r#"{"owners":[{"lastName":"Davis"},{"lastName":"Lee"}]}"#,
            "client.global.set(\"second\", response.body.owners[1].lastName);",
        );
        assert_eq!(output.error, None);
        assert_eq!(globals.get("second").map(String::as_str), Some("Lee"));
    }

    #[test]
    fn a_passing_and_a_failing_test_are_both_kept() {
        let (output, _) = run_on(
            r#"{"id":1}"#,
            "client.test(\"status\", function() {\n\
               client.assert(response.status === 200, \"not 200\");\n\
             });\n\
             client.test(\"id\", function() {\n\
               client.assert(response.body.id === 2, \"the id is not 2\");\n\
             });",
        );
        assert_eq!(output.error, None);
        assert_eq!(output.tests.len(), 2);
        assert!(output.tests[0].passed);
        assert_eq!(output.tests[0].name, "status");
        assert!(!output.tests[1].passed);
        assert_eq!(output.tests[1].message.as_deref(), Some("the id is not 2"));
    }

    #[test]
    fn a_failed_assertion_skips_the_rest_of_its_test_only() {
        let (output, globals) = run_on(
            "{}",
            "client.test(\"one\", function() {\n\
               client.assert(false, \"stop\");\n\
               client.global.set(\"inside\", \"no\");\n\
             });\n\
             client.global.set(\"after\", \"yes\");",
        );
        assert_eq!(output.error, None);
        assert!(!output.tests[0].passed);
        assert!(!globals.contains_key("inside"));
        assert_eq!(globals.get("after").map(String::as_str), Some("yes"));
    }

    #[test]
    fn the_comparisons_of_the_subset_hold() {
        let (output, _) = run_on(
            r#"{"id":1}"#,
            "client.test(\"all\", function() {\n\
               client.assert(response.status !== 500);\n\
               client.assert(response.status == 200);\n\
               client.assert(response.status >= 200 && response.status <= 299);\n\
               client.assert(response.body.id < 2);\n\
               client.assert(response.body.missing === null);\n\
               client.assert(\"a\" !== \"b\");\n\
             });",
        );
        assert_eq!(output.error, None);
        assert!(output.tests[0].passed, "{:?}", output.tests[0]);
    }

    #[test]
    fn a_local_variable_and_the_global_of_an_earlier_set_are_read() {
        let (output, globals) = run_on(
            r#"{"token":"abc"}"#,
            "let body = response.body;\n\
             let token = response.headers.valueOf(\"X-Token\");\n\
             client.global.set(\"token\", token);\n\
             client.global.set(\"body\", body.token);\n\
             client.global.set(\"copy\", client.global.get(\"token\"));",
        );
        assert_eq!(output.error, None);
        assert_eq!(globals.get("copy").map(String::as_str), Some("abc"));
        assert_eq!(globals.get("body").map(String::as_str), Some("abc"));
    }

    #[test]
    fn a_string_and_a_number_join_with_a_plus() {
        let (output, globals) = run_on(
            r#"{"token":"abc"}"#,
            "client.global.set(\"header\", \"Bearer \" + response.body.token);",
        );
        assert_eq!(output.error, None);
        assert_eq!(
            globals.get("header").map(String::as_str),
            Some("Bearer abc")
        );
    }

    #[test]
    fn a_log_is_kept() {
        let (output, _) = run_on("{}", "client.log(\"status\", response.status);");
        assert_eq!(output.logs, ["status 200"]);
        assert_eq!(output.error, None);
    }

    #[test]
    fn a_line_outside_the_subset_is_an_error_with_its_number() {
        let (output, _) = run_on("{}", "if (response.status === 200) {\n client.log(1);\n}");
        let error = output.error.expect("the script stops");
        assert_eq!(error.line, 1);
        assert!(error.message.contains("unsupported"), "{error}");
    }

    #[test]
    fn a_field_of_a_missing_field_is_an_error() {
        let (output, _) = run_on("{}", "client.global.set(\"x\", response.body.gone.deeper);");
        let error = output.error.expect("the script stops");
        assert_eq!(error.line, 1);
    }

    #[test]
    fn a_missing_member_of_the_response_is_an_error() {
        let (output, _) = run_on("{}", "client.global.set(\"x\", response.cookies);");
        let error = output.error.expect("the script stops");
        assert!(error.message.contains("response.cookies"), "{error}");
    }

    #[test]
    fn a_script_without_a_close_is_an_error() {
        let (output, _) = run_on("{}", "client.test(\"one\", function() {");
        let error = output.error.expect("the script stops");
        assert!(error.message.contains("closing brace"), "{error}");
    }
}
