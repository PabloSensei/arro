//! Ctrl+Alt+V: the expressions around the caret, read from the tree of the file. The
//! language server names the expressions it finds; the tree gives the ones that enclose
//! the caret when the server names fewer than two.

use std::ops::Range;

use tree_sitter::{Node, Tree};

use super::edit::{node_at, parse};

/// The named nodes of the grammar that are expressions. An identifier is one only where it
/// names a value, see [`is_expression`].
const EXPRESSIONS: &[&str] = &[
    "array_access",
    "array_creation_expression",
    "assignment_expression",
    "binary_expression",
    "cast_expression",
    "class_literal",
    "field_access",
    "identifier",
    "instanceof_expression",
    "lambda_expression",
    "method_invocation",
    "method_reference",
    "object_creation_expression",
    "parenthesized_expression",
    "switch_expression",
    "template_expression",
    "ternary_expression",
    "this",
    "unary_expression",
    "update_expression",
    // The literals of the grammar.
    "binary_integer_literal",
    "character_literal",
    "decimal_floating_point_literal",
    "decimal_integer_literal",
    "false",
    "hex_floating_point_literal",
    "hex_integer_literal",
    "null_literal",
    "octal_integer_literal",
    "string_literal",
    "true",
];

/// Whether the walk up stops at the node: the statement or the declaration that holds the
/// caret is not an expression, and nothing above it is part of the expression at the caret.
fn ends_the_walk(kind: &str) -> bool {
    kind == "block"
        || kind == "program"
        || kind.ends_with("_statement")
        || kind.ends_with("_declaration")
        || kind.ends_with("_body")
}

/// Whether the node is an expression of its own. An identifier names a value unless it is
/// the name of its parent, such as a method or a variable being declared, the field of an
/// access, or a part of a qualified name.
pub(super) fn is_expression(node: Node) -> bool {
    if !node.is_named() || !EXPRESSIONS.contains(&node.kind()) {
        return false;
    }
    if node.kind() != "identifier" {
        return true;
    }
    let Some(parent) = node.parent() else {
        return false;
    };
    let names_parent = ["name", "field"]
        .iter()
        .any(|field| parent.child_by_field_name(field) == Some(node));
    !names_parent && parent.kind() != "scoped_identifier"
}

/// Appends the expressions nested directly in `node`: the receiver and the arguments of a
/// call, the operands of a binary expression. The expressions inside those are left out,
/// and so is anything in a body, such as the statements of a lambda.
fn push_nested(node: Node, found: &mut Vec<Range<usize>>) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if ends_the_walk(child.kind()) {
            continue;
        }
        if is_expression(child) {
            found.push(child.start_byte()..child.end_byte());
        } else {
            push_nested(child, found);
        }
    }
}

/// The byte ranges of the expressions around `offset`: the nested expressions of the innermost
/// one first, then the enclosing ones, the innermost first. The walk goes up from the node under
/// the caret and stops at the statement that holds it. A caret on the name of a call gives the
/// receiver of the call too, as the caret is inside the call.
pub fn expressions_at(text: &str, offset: usize) -> Vec<Range<usize>> {
    let Some(tree) = parse(text) else {
        return Vec::new();
    };
    let mut enclosing = Vec::new();
    let mut innermost = None;
    let mut node = node_at(&tree, offset);
    loop {
        if ends_the_walk(node.kind()) {
            break;
        }
        if is_expression(node) {
            if innermost.is_none() {
                innermost = Some(node);
            }
            enclosing.push(node.start_byte()..node.end_byte());
        }
        match node.parent() {
            Some(parent) => node = parent,
            None => break,
        }
    }
    let mut found = Vec::new();
    if let Some(innermost) = innermost {
        push_nested(innermost, &mut found);
    }
    found.extend(enclosing);
    found
}

/// The kind of a literal, as the node of the grammar tells it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiteralKind {
    Int,
    Long,
    Float,
    Double,
    Char,
    String,
    Boolean,
    Null,
}

/// The shape of an expression, as its node of the grammar tells it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExpressionShape {
    /// A literal of the kind.
    Literal(LiteralKind),
    /// A name, a field access or a call: the byte where the name starts. The type of the
    /// expression is the one the server declares under that name.
    Named(usize),
    /// Any other expression.
    Other,
}

/// The shape of the expression that covers exactly `range` of `text`, or `None` when the
/// grammar has no expression of that range there. Whitespace at the ends of the range is left
/// out.
pub fn expression_shape(text: &str, range: Range<usize>) -> Option<ExpressionShape> {
    let selected = text.get(range.clone())?;
    let start = range.start + (selected.len() - selected.trim_start().len());
    let end = range.end - (selected.len() - selected.trim_end().len());
    if start >= end {
        return None;
    }
    let tree = parse(text)?;
    let node = exact_expression(&tree, start..end)?;
    let spelling = &text[node.byte_range()];
    Some(match node.kind() {
        // The suffix L makes a long; a hex literal ends in a digit or a letter of its own.
        "decimal_integer_literal"
        | "hex_integer_literal"
        | "octal_integer_literal"
        | "binary_integer_literal" => ExpressionShape::Literal(if spelling.ends_with(['l', 'L']) {
            LiteralKind::Long
        } else {
            LiteralKind::Int
        }),
        "decimal_floating_point_literal" | "hex_floating_point_literal" => {
            ExpressionShape::Literal(if spelling.ends_with(['f', 'F']) {
                LiteralKind::Float
            } else {
                LiteralKind::Double
            })
        }
        "character_literal" => ExpressionShape::Literal(LiteralKind::Char),
        "string_literal" => ExpressionShape::Literal(LiteralKind::String),
        "true" | "false" => ExpressionShape::Literal(LiteralKind::Boolean),
        "null_literal" => ExpressionShape::Literal(LiteralKind::Null),
        "identifier" => ExpressionShape::Named(node.start_byte()),
        "field_access" => node
            .child_by_field_name("field")
            .map_or(ExpressionShape::Other, |field| {
                ExpressionShape::Named(field.start_byte())
            }),
        "method_invocation" => node
            .child_by_field_name("name")
            .map_or(ExpressionShape::Other, |name| {
                ExpressionShape::Named(name.start_byte())
            }),
        _ => ExpressionShape::Other,
    })
}

/// The expression node that covers exactly `range`: the smallest node spanning the range, or
/// one of its ancestors, when that node is an expression of the range.
fn exact_expression(tree: &Tree, range: Range<usize>) -> Option<Node<'_>> {
    let mut node = tree
        .root_node()
        .descendant_for_byte_range(range.start, range.end)?;
    loop {
        if node.start_byte() == range.start && node.end_byte() == range.end && is_expression(node) {
            return Some(node);
        }
        node = node.parent()?;
    }
}

#[cfg(test)]
mod tests {
    use super::{ExpressionShape, LiteralKind, expression_shape, expressions_at};

    const TEXT: &str = "class A {\n    String last(Owner owner) {\n        String value = owner.getLastName();\n    }\n}\n";

    /// The text of every expression around the byte `offset`.
    fn texts(offset: usize) -> Vec<&'static str> {
        expressions_at(TEXT, offset)
            .into_iter()
            .map(|range| &TEXT[range])
            .collect()
    }

    /// The byte of `needle` in the text, moved `inside` bytes into it.
    fn at(needle: &str, inside: usize) -> usize {
        TEXT.find(needle).expect("the text has the needle") + inside
    }

    #[test]
    fn the_caret_on_the_owner_lists_the_owner_and_the_call() {
        assert_eq!(
            texts(at("owner.getLastName()", 2)),
            ["owner", "owner.getLastName()"]
        );
    }

    #[test]
    fn the_caret_on_the_method_name_lists_the_receiver_and_the_call() {
        // The name alone is not an expression, but the call the caret is in is one, and
        // the receiver nested in that call comes with it.
        assert_eq!(
            texts(at("owner.getLastName()", 8)),
            ["owner", "owner.getLastName()"]
        );
    }

    #[test]
    fn a_declared_name_is_not_an_expression() {
        assert!(texts(at("value =", 2)).is_empty());
    }

    /// The shape of the expression of the bytes `from..to` of the text.
    fn shape(text: &str, from: usize, to: usize) -> Option<ExpressionShape> {
        expression_shape(text, from..to)
    }

    #[test]
    fn a_name_is_named_by_its_start_and_a_call_by_its_method_name() {
        let owner = at("owner.getLastName()", 0);
        assert_eq!(
            shape(TEXT, owner, owner + "owner".len()),
            Some(ExpressionShape::Named(owner))
        );
        let call = at("owner.getLastName()", 0);
        let name = at("owner.getLastName()", 6);
        assert_eq!(
            shape(TEXT, call, call + "owner.getLastName()".len()),
            Some(ExpressionShape::Named(name))
        );
    }

    #[test]
    fn whitespace_at_the_start_of_the_range_is_left_out() {
        // The space before the receiver, as a selection that starts one byte early takes it.
        let owner = at("owner.getLastName()", 0);
        assert_eq!(
            shape(TEXT, owner - 1, owner + "owner".len()),
            Some(ExpressionShape::Named(owner))
        );
    }

    #[test]
    fn a_range_that_is_no_expression_has_no_shape() {
        let name = at("value =", 0);
        assert_eq!(shape(TEXT, name, name + "value".len()), None);
        assert_eq!(shape(TEXT, name, name), None);
    }

    #[test]
    fn literals_have_the_kind_of_their_spelling() {
        const LITERALS: &str = "class A { void run() { long a = 10L; String s = \"x\"; boolean b = true; char c = 'c'; double d = 1.5; float f = 1.5f; Object o = null; int h = 0x1F; } }";
        let cases = [
            ("10L", LiteralKind::Long),
            ("\"x\"", LiteralKind::String),
            ("true", LiteralKind::Boolean),
            ("'c'", LiteralKind::Char),
            ("1.5;", LiteralKind::Double),
            ("1.5f", LiteralKind::Float),
            ("null", LiteralKind::Null),
            ("0x1F", LiteralKind::Int),
        ];
        for (needle, kind) in cases {
            let start = LITERALS.find(needle).expect("the literal is in the text");
            let end = start + needle.trim_end_matches(';').len();
            assert_eq!(
                expression_shape(LITERALS, start..end),
                Some(ExpressionShape::Literal(kind)),
                "the literal {needle}"
            );
        }
    }

    #[test]
    fn another_expression_is_other() {
        const SUM: &str = "class A { int run() { return 1 + 2; } }";
        let start = SUM.find("1 + 2").unwrap();
        assert_eq!(
            expression_shape(SUM, start..start + "1 + 2".len()),
            Some(ExpressionShape::Other)
        );
    }
}
