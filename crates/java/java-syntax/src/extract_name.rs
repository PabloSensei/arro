//! The name an extraction or an Introduce Parameter suggests for an expression, read from
//! the tree of the file. The first of these sources that holds gives the name:
//!
//! 1. the text of a string literal, when the text reads as a name: an argument such as
//!    `"totalItems"` names its value, not the parameter it fills;
//! 2. the parameter the expression is an argument of, when the editor knows its name from
//!    the parameter hints;
//! 3. the variable the expression initializes.
//!
//! A constant takes the name in upper case with underscores between the words. A name that
//! a variable, a parameter or a field of the scope already has gets a number, such as
//! `pageSize1`. The variable the expression initializes stays in the code after the
//! extraction, so its own name counts as taken too: for `int pageSize = 5;` a parameter, a
//! variable or a field gets `pageSize1`, and a constant gets `PAGE_SIZE`.

use std::ops::Range;

use lang_api::NameRole;
use tree_sitter::Node;

use super::edit::parse;
use super::expressions::is_expression;

/// The name for the expression at `expression`, a byte range of `text`, as a name of `role`.
/// `None` when the range is not exactly one expression, or when no source gives a name.
pub fn suggest_name(
    text: &str,
    expression: Range<usize>,
    role: NameRole,
    hint: Option<&str>,
) -> Option<String> {
    let tree = parse(text)?;
    let node = tree
        .root_node()
        .named_descendant_for_byte_range(expression.start, expression.end)?;
    if node.start_byte() != expression.start
        || node.end_byte() != expression.end
        || !is_expression(node)
    {
        return None;
    }
    let parent = node.parent()?;
    let declared = if parent.kind() == "variable_declarator"
        && parent.child_by_field_name("value") == Some(node)
    {
        parent.child_by_field_name("name")
    } else {
        None
    };
    let hinted = hint
        .filter(|_| parent.kind() == "argument_list")
        .filter(|hint| is_identifier(hint));
    let base = match literal_name(node, text) {
        Some(name) => name,
        None => match hinted {
            Some(hint) => hint.to_owned(),
            None => {
                let name = declared?;
                text[name.start_byte()..name.end_byte()].to_owned()
            }
        },
    };
    let name = match role {
        NameRole::Constant => constant_name(&base),
        NameRole::Variable | NameRole::Field | NameRole::Parameter => base,
    };
    if !is_identifier(&name) {
        return None;
    }
    let names = scope_names(node, text);
    let mut candidate = name.clone();
    let mut number = 1;
    while names.contains(&candidate) {
        candidate = format!("{name}{number}");
        number += 1;
    }
    Some(candidate)
}

/// The text of a string literal that reads as a name, without its quotes.
fn literal_name(node: Node, text: &str) -> Option<String> {
    if node.kind() != "string_literal" {
        return None;
    }
    let raw = &text[node.start_byte()..node.end_byte()];
    let content = raw.strip_prefix('"')?.strip_suffix('"')?;
    is_identifier(content).then(|| content.to_owned())
}

/// The name of a constant: the words of `name` in upper case, joined by underscores.
fn constant_name(name: &str) -> String {
    let mut upper = String::with_capacity(name.len() + 4);
    let mut after_lower = false;
    for character in name.chars() {
        if character.is_uppercase() && after_lower {
            upper.push('_');
        }
        upper.extend(character.to_uppercase());
        after_lower = character.is_lowercase() || character.is_ascii_digit();
    }
    upper
}

/// Whether `name` is an identifier of the grammar: a letter, `_` or `$` first, then letters,
/// digits, `_` or `$`.
fn is_identifier(name: &str) -> bool {
    let mut characters = name.chars();
    characters
        .next()
        .is_some_and(|first| first.is_alphabetic() || first == '_' || first == '$')
        && characters
            .all(|character| character.is_alphanumeric() || character == '_' || character == '$')
}

/// The names the scope of `node` declares: the variables and the parameters of the outermost
/// method, lambda or constructor around it, and the fields of the type around it.
fn scope_names(node: Node, text: &str) -> Vec<String> {
    let mut method = None;
    let mut body = None;
    let mut ancestor = node.parent();
    while let Some(current) = ancestor {
        match current.kind() {
            "method_declaration" | "constructor_declaration" | "lambda_expression" => {
                method = Some(current);
            }
            "class_body" | "interface_body" | "enum_body" if body.is_none() => {
                body = Some(current);
            }
            _ => {}
        }
        ancestor = current.parent();
    }
    let mut found = Vec::new();
    if let Some(method) = method {
        declared_names(method, text, &mut found);
    }
    if let Some(body) = body {
        let mut cursor = body.walk();
        for member in body.named_children(&mut cursor) {
            if member.kind() == "field_declaration" {
                declared_names(member, text, &mut found);
            }
        }
    }
    found
}

/// Appends the name of every variable and parameter declared under `node`.
fn declared_names(node: Node, text: &str, found: &mut Vec<String>) {
    if matches!(node.kind(), "variable_declarator" | "formal_parameter")
        && let Some(name) = node.child_by_field_name("name")
    {
        found.push(text[name.start_byte()..name.end_byte()].to_owned());
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        declared_names(child, text, found);
    }
}
