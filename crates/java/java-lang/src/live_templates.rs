//! Live templates, postfix templates and Surround With of the language, as data for the
//! neutral `templates` crate. Bodies follow IntelliJ IDEA's default Java set.

use std::collections::HashMap;
use std::ops::Range;
use std::path::Path;
use std::sync::OnceLock;

use templates::{ANY, Declarations, Kind, PostfixRules, Receiver, Template, TemplateLanguage};
use tree_sitter::{Node, Parser, Tree};

use crate::is_source;

const STATEMENT: &[&str] = &["statement"];
const DECLARATION: &[&str] = &["declaration"];
/// A postfix template that is a statement of its own, or an expression inside another.
const VALUE: &[&str] = &["statement", "expression"];

/// `statement` inside method bodies, `declaration` in class bodies, `expression` inside
/// arguments and parentheses.
const CONTEXTS: &str = r#"
(class_body) @declaration
(interface_body) @declaration
(enum_body_declarations) @declaration
(annotation_type_body) @declaration
(block) @statement
(constructor_body) @statement
(switch_block_statement_group) @statement
(argument_list) @expression
(parenthesized_expression) @expression
"#;

const fn abbreviation(
    key: &'static str,
    description: &'static str,
    body: &'static str,
    fields: &'static [(&'static str, &'static str)],
    contexts: &'static [&'static str],
) -> Template {
    Template {
        key,
        description,
        kind: Kind::Abbreviation,
        body,
        fields,
        contexts,
    }
}

const fn postfix(
    key: &'static str,
    description: &'static str,
    body: &'static str,
    fields: &'static [(&'static str, &'static str)],
    contexts: &'static [&'static str],
) -> Template {
    Template {
        key,
        description,
        kind: Kind::Postfix,
        body,
        fields,
        contexts,
    }
}

const fn surround(
    key: &'static str,
    description: &'static str,
    body: &'static str,
    fields: &'static [(&'static str, &'static str)],
) -> Template {
    Template {
        key,
        description,
        kind: Kind::Surround,
        body,
        fields,
        contexts: STATEMENT,
    }
}

/// A template of the Surround with Live Template list, not of Surround With.
const fn surround_live(
    key: &'static str,
    description: &'static str,
    body: &'static str,
    fields: &'static [(&'static str, &'static str)],
) -> Template {
    Template {
        key,
        description,
        kind: Kind::SurroundLive,
        body,
        fields,
        contexts: STATEMENT,
    }
}

const MAIN: &str = "public static void main(String[] args) {\n\t$END$\n}";
const FORI: &str = "for (int $INDEX$ = 0; $INDEX$ < $LIMIT$; $INDEX$++) {\n\t$END$\n}";

const TEMPLATES: &[Template] = &[
    abbreviation(
        "sout",
        "Prints a string to System.out",
        "System.out.println($END$);",
        &[],
        STATEMENT,
    ),
    abbreviation(
        "soutv",
        "Prints a value to System.out",
        "System.out.println(\"$VALUE$ = \" + $VALUE$);$END$",
        &[("VALUE", "value")],
        STATEMENT,
    ),
    abbreviation(
        "souf",
        "Prints a formatted string to System.out",
        "System.out.printf(\"$END$\");",
        &[],
        STATEMENT,
    ),
    abbreviation("psvm", "main() method declaration", MAIN, &[], DECLARATION),
    abbreviation("main", "main() method declaration", MAIN, &[], DECLARATION),
    abbreviation(
        "fori",
        "Creates an iteration loop",
        FORI,
        &[("INDEX", "i"), ("LIMIT", "")],
        STATEMENT,
    ),
    abbreviation(
        "iter",
        "Iterates over an Iterable or an array",
        "for ($TYPE$ $VAR$ : $ITERABLE$) {\n\t$END$\n}",
        &[("ITERABLE", ""), ("TYPE", "var"), ("VAR", "item")],
        STATEMENT,
    ),
    abbreviation(
        "itar",
        "Iterates over the elements of an array",
        "for (int $INDEX$ = 0; $INDEX$ < $ARRAY$.length; $INDEX$++) {\n\t$TYPE$ $VAR$ = $ARRAY$[$INDEX$];\n\t$END$\n}",
        &[
            ("ARRAY", "array"),
            ("INDEX", "i"),
            ("TYPE", "var"),
            ("VAR", "item"),
        ],
        STATEMENT,
    ),
    abbreviation(
        "ifn",
        "Inserts an 'if null' statement",
        "if ($VAR$ == null) {\n\t$END$\n}",
        &[("VAR", "")],
        STATEMENT,
    ),
    abbreviation(
        "inn",
        "Inserts an 'if not null' statement",
        "if ($VAR$ != null) {\n\t$END$\n}",
        &[("VAR", "")],
        STATEMENT,
    ),
    abbreviation("thr", "throw new", "throw new $END$", &[], STATEMENT),
    abbreviation("St", "String", "String $END$", &[], &[ANY]),
    abbreviation(
        "prsf",
        "private static final",
        "private static final $END$",
        &[],
        DECLARATION,
    ),
    abbreviation(
        "psf",
        "public static final",
        "public static final $END$",
        &[],
        DECLARATION,
    ),
    postfix(
        "var",
        "Introduces a local variable",
        "var $NAME$ = $EXPR$;$END$",
        &[("NAME", "value")],
        STATEMENT,
    ),
    postfix(
        "field",
        "Introduces a field",
        "private $TYPE$ $NAME$ = $EXPR$;$END$",
        &[("TYPE", "Object"), ("NAME", "field")],
        STATEMENT,
    ),
    postfix(
        "if",
        "Checks a condition",
        "if ($EXPR$) {\n\t$END$\n}",
        &[],
        STATEMENT,
    ),
    postfix(
        "else",
        "Inserts an else branch",
        "else {\n\t$END$\n}",
        &[],
        STATEMENT,
    ),
    postfix("not", "Negates a boolean", "!$EXPR$$END$", &[], VALUE),
    postfix(
        "null",
        "Checks that the expression is null",
        "if ($EXPR$ == null) {\n\t$END$\n}",
        &[],
        STATEMENT,
    ),
    postfix(
        "notnull",
        "Checks that the expression is not null",
        "if ($EXPR$ != null) {\n\t$END$\n}",
        &[],
        STATEMENT,
    ),
    postfix(
        "nn",
        "Checks that the expression is not null",
        "if ($EXPR$ != null) {\n\t$END$\n}",
        &[],
        STATEMENT,
    ),
    postfix(
        "for",
        "Iterates over an Iterable or an array",
        "for (var $VAR$ : $EXPR$) {\n\t$END$\n}",
        &[("VAR", "item")],
        STATEMENT,
    ),
    postfix(
        "fori",
        "Iterates up to a number",
        "for (int $INDEX$ = 0; $INDEX$ < $EXPR$; $INDEX$++) {\n\t$END$\n}",
        &[("INDEX", "i")],
        STATEMENT,
    ),
    postfix(
        "forr",
        "Iterates down from a number",
        "for (int $INDEX$ = $EXPR$ - 1; $INDEX$ >= 0; $INDEX$--) {\n\t$END$\n}",
        &[("INDEX", "i")],
        STATEMENT,
    ),
    postfix(
        "iter",
        "Iterates over an Iterable or an array",
        "for (var $VAR$ : $EXPR$) {\n\t$END$\n}",
        &[("VAR", "item")],
        STATEMENT,
    ),
    postfix(
        "while",
        "Creates a while loop",
        "while ($EXPR$) {\n\t$END$\n}",
        &[],
        STATEMENT,
    ),
    postfix(
        "try",
        "Wraps the statement in try / catch",
        "try {\n\t$EXPR$;$END$\n} catch ($EXCEPTION$ e) {\n\tthrow new RuntimeException(e);\n}",
        &[("EXCEPTION", "Exception")],
        STATEMENT,
    ),
    postfix(
        "twr",
        "Wraps the statement in try-with-resources",
        "try ($RESOURCE$ = $EXPR$) {\n\t$END$\n}",
        &[("RESOURCE", "resource")],
        STATEMENT,
    ),
    postfix(
        "return",
        "Returns the value",
        "return $EXPR$;$END$",
        &[],
        STATEMENT,
    ),
    postfix(
        "throw",
        "Throws the value",
        "throw $EXPR$;$END$",
        &[],
        STATEMENT,
    ),
    postfix(
        "sout",
        "Prints the value",
        "System.out.println($EXPR$);$END$",
        &[],
        STATEMENT,
    ),
    postfix(
        "soutv",
        "Prints the expression with its name",
        "System.out.println(\"$EXPR$ = \" + $EXPR$);$END$",
        &[],
        STATEMENT,
    ),
    postfix(
        "serr",
        "Prints the value to System.err",
        "System.err.println($EXPR$);$END$",
        &[],
        STATEMENT,
    ),
    postfix(
        "par",
        "Wraps the expression in parentheses",
        "($EXPR$)$END$",
        &[],
        VALUE,
    ),
    postfix(
        "cast",
        "Casts the expression",
        "(($TYPE$) $EXPR$)$END$",
        &[("TYPE", "Object")],
        VALUE,
    ),
    postfix(
        "new",
        "Creates a new object",
        "new $TYPE$($EXPR$)$END$",
        &[("TYPE", "Object")],
        VALUE,
    ),
    postfix(
        "lambda",
        "Turns the expression into a lambda",
        "() -> $EXPR$$END$",
        &[],
        VALUE,
    ),
    postfix(
        "opt",
        "Wraps the expression into an Optional",
        "java.util.Optional.ofNullable($EXPR$)$END$",
        &[],
        VALUE,
    ),
    postfix(
        "stream",
        "Turns the collection into a stream",
        "$EXPR$.stream()$END$",
        &[],
        VALUE,
    ),
    postfix(
        "switch",
        "Creates a switch statement",
        "switch ($EXPR$) {\n\tcase $CASE$:\n\t\t$END$\n\t\tbreak;\n}",
        &[("CASE", "")],
        STATEMENT,
    ),
    postfix(
        "synchronized",
        "Wraps the statement in a synchronized block",
        "synchronized ($EXPR$) {\n\t$END$\n}",
        &[],
        STATEMENT,
    ),
    postfix(
        "format",
        "Formats a string",
        "String.format($EXPR$, $ARGS$)$END$",
        &[("ARGS", "")],
        VALUE,
    ),
    postfix(
        "arg",
        "Passes the expression as an argument",
        "$EXPR$$END$",
        &[],
        VALUE,
    ),
    postfix(
        "inst",
        "Checks the expression type with instanceof",
        "if ($EXPR$ instanceof $TYPE$) {\n\t$END$\n}",
        &[("TYPE", "Object")],
        STATEMENT,
    ),
    surround("if", "if", "if ($CONDITION$) {\n\t$SELECTION$\n}$END$", &[]),
    surround(
        "try",
        "try / catch",
        "try {\n\t$SELECTION$\n} catch ($EXCEPTION$ e) {\n\tthrow new RuntimeException(e);$END$\n}",
        &[("EXCEPTION", "Exception")],
    ),
    surround(
        "for",
        "for",
        "for (int $INDEX$ = 0; $INDEX$ < $LIMIT$; $INDEX$++) {\n\t$SELECTION$\n}$END$",
        &[("INDEX", "i"), ("LIMIT", "")],
    ),
    surround(
        "while",
        "while",
        "while ($CONDITION$) {\n\t$SELECTION$\n}$END$",
        &[],
    ),
    surround(
        "synchronized",
        "synchronized",
        "synchronized ($LOCK$) {\n\t$SELECTION$\n}$END$",
        &[("LOCK", "this")],
    ),
    surround_live(
        "C",
        "Callable",
        "new java.util.concurrent.Callable<$TYPE$>() {\n\t@Override\n\tpublic $TYPE$ call() throws Exception {\n\t\t$SELECTION$\n\t\treturn $END$;\n\t}\n}",
        &[("TYPE", "Object")],
    ),
    surround_live(
        "RL",
        "readLock / unlock",
        "$LOCK$.readLock().lock();\ntry {\n\t$SELECTION$\n} finally {\n\t$LOCK$.readLock().unlock();\n}$END$",
        &[("LOCK", "lock")],
    ),
    surround_live(
        "WL",
        "writeLock / unlock",
        "$LOCK$.writeLock().lock();\ntry {\n\t$SELECTION$\n} finally {\n\t$LOCK$.writeLock().unlock();\n}$END$",
        &[("LOCK", "lock")],
    ),
];

/// The names a postfix expression may refer to in one file: its variables and its
/// methods, with the type each one names.
#[derive(Default)]
struct Declared {
    variables: HashMap<String, String>,
    methods: HashMap<String, String>,
}

fn node_text<'a>(text: &'a str, node: Node<'_>) -> &'a str {
    &text[node.byte_range()]
}

fn is_var(ty: &str) -> bool {
    ty.trim() == "var"
}

fn inferred_type(expression: &str, found: &Declared) -> String {
    let expression = expression.trim();
    if let Some(ty) = new_type(expression) {
        return if expression.contains('[') {
            format!("{ty}[]")
        } else {
            ty.to_owned()
        };
    }
    if expression.starts_with('"') {
        return "String".to_owned();
    }
    if expression.starts_with('\'') {
        return "char".to_owned();
    }
    if matches!(expression, "true" | "false") {
        return "boolean".to_owned();
    }
    if expression
        .chars()
        .next()
        .is_some_and(|character| character.is_ascii_digit())
    {
        return "int".to_owned();
    }
    if let Some(ty) = found.variables.get(last_segment(expression)) {
        return ty.clone();
    }
    if let Some(ty) = found.variables.get(first_segment(expression)) {
        return ty.clone();
    }
    "var".to_owned()
}

fn collect_declarations(node: Node<'_>, text: &str, found: &mut Declared) {
    match node.kind() {
        "local_variable_declaration" | "field_declaration" | "constant_declaration" => {
            if let Some(kind) = node.child_by_field_name("type") {
                let ty = node_text(text, kind).to_owned();
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    if child.kind() != "variable_declarator" {
                        continue;
                    }
                    let Some(name) = child.child_by_field_name("name") else {
                        continue;
                    };
                    let ty = if is_var(&ty) {
                        child.child_by_field_name("value").map_or_else(
                            || ty.clone(),
                            |value| inferred_type(node_text(text, value), found),
                        )
                    } else {
                        ty.clone()
                    };
                    found.variables.insert(node_text(text, name).to_owned(), ty);
                }
            }
        }
        "method_declaration" => {
            if let (Some(name), Some(kind)) = (
                node.child_by_field_name("name"),
                node.child_by_field_name("type"),
            ) {
                found.methods.insert(
                    node_text(text, name).to_owned(),
                    node_text(text, kind).to_owned(),
                );
            }
        }
        "formal_parameter"
        | "catch_formal_parameter"
        | "spread_parameter"
        | "enhanced_for_statement"
        | "resource" => {
            let ty = node
                .child_by_field_name("type")
                .map_or_else(String::new, |kind| node_text(text, kind).to_owned());
            let name = node.child_by_field_name("name").or_else(|| {
                let mut cursor = node.walk();
                node.named_children(&mut cursor)
                    .find(|child| child.kind() == "identifier")
            });
            if let Some(name) = name {
                let ty = if node.kind() == "spread_parameter" {
                    format!("{ty}[]")
                } else {
                    ty
                };
                found.variables.insert(node_text(text, name).to_owned(), ty);
            }
        }
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_declarations(child, text, found);
    }
}

fn last_segment(expression: &str) -> &str {
    expression.rsplit('.').next().unwrap_or(expression).trim()
}

fn first_segment(expression: &str) -> &str {
    expression.split('.').next().unwrap_or(expression).trim()
}

/// The simple name of a type and whether it is an array.
fn base_type(ty: &str) -> (String, bool) {
    let mut head = ty.trim();
    let array = head.contains('[') || head.ends_with("...");
    if let Some(at) = head.find('<') {
        head = &head[..at];
    }
    if let Some(at) = head.find('[') {
        head = &head[..at];
    }
    if let Some(rest) = head.trim_end().strip_suffix("...") {
        head = rest;
    }
    let head = head.trim();
    let name = head.rsplit('.').next().unwrap_or(head).trim().to_owned();
    (name, array)
}

fn reference_receiver() -> Receiver {
    Receiver {
        known: true,
        reference: true,
        ..Receiver::default()
    }
}

fn sequence_receiver() -> Receiver {
    Receiver {
        known: true,
        reference: true,
        sequence: true,
        ..Receiver::default()
    }
}

fn receiver_of_type(ty: &str) -> Receiver {
    let (name, array) = base_type(ty);
    if name.is_empty() || name == "var" {
        return Receiver::default();
    }
    if array {
        return sequence_receiver();
    }
    match name.as_str() {
        "boolean" => Receiver {
            known: true,
            boolean: true,
            ..Receiver::default()
        },
        "Boolean" => Receiver {
            known: true,
            boolean: true,
            reference: true,
            ..Receiver::default()
        },
        "byte" | "short" | "int" | "long" | "float" | "double" => Receiver {
            known: true,
            number: true,
            ..Receiver::default()
        },
        "Byte" | "Short" | "Integer" | "Long" | "Float" | "Double" => Receiver {
            known: true,
            number: true,
            reference: true,
            ..Receiver::default()
        },
        "char" => Receiver {
            known: true,
            ..Receiver::default()
        },
        "Character" => reference_receiver(),
        "String" | "CharSequence" | "StringBuilder" | "StringBuffer" => Receiver {
            known: true,
            reference: true,
            text: true,
            ..Receiver::default()
        },
        "List"
        | "ArrayList"
        | "LinkedList"
        | "Vector"
        | "Stack"
        | "Set"
        | "HashSet"
        | "LinkedHashSet"
        | "TreeSet"
        | "SortedSet"
        | "NavigableSet"
        | "Collection"
        | "Iterable"
        | "Queue"
        | "Deque"
        | "ArrayDeque"
        | "CopyOnWriteArrayList" => sequence_receiver(),
        _ => reference_receiver(),
    }
}

fn new_type(expression: &str) -> Option<&str> {
    let rest = expression.trim().strip_prefix("new ")?;
    let end = rest.find(['<', '(', '[', '{']).unwrap_or(rest.len());
    let ty = rest[..end].trim();
    (!ty.is_empty()).then_some(ty)
}

/// The closing bracket of the one at `open`, outside string literals.
fn matching_close(text: &str, open: usize, opening: u8, closing: u8) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut depth = 0i32;
    let mut quote: Option<u8> = None;
    let mut escaped = false;
    for (at, byte) in bytes.iter().enumerate().skip(open) {
        let byte = *byte;
        if let Some(mark) = quote {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == mark {
                quote = None;
            }
            continue;
        }
        if byte == b'"' || byte == b'\'' {
            quote = Some(byte);
        } else if byte == opening {
            depth += 1;
        } else if byte == closing {
            depth -= 1;
            if depth == 0 {
                return Some(at);
            }
        }
    }
    None
}

fn strip_outer_parens(mut expression: &str) -> &str {
    loop {
        if !expression.starts_with('(') || !expression.ends_with(')') {
            return expression;
        }
        if matching_close(expression, 0, b'(', b')') != Some(expression.len() - 1) {
            return expression;
        }
        expression = expression[1..expression.len() - 1].trim();
    }
}

fn looks_like_type(text: &str) -> bool {
    !text.is_empty()
        && text.chars().all(|character| {
            character.is_alphanumeric()
                || matches!(
                    character,
                    '_' | '.' | '<' | '>' | '[' | ']' | '?' | ',' | ' ' | '@'
                )
        })
}

/// A cast in front of an expression: its type and the expression it casts.
fn cast_prefix(expression: &str) -> Option<(&str, &str)> {
    if !expression.starts_with('(') {
        return None;
    }
    let close = matching_close(expression, 0, b'(', b')')?;
    if close + 1 >= expression.len() {
        return None;
    }
    let inside = expression[1..close].trim();
    looks_like_type(inside).then_some((inside, expression[close + 1..].trim()))
}

/// The name of the method of the last call of the expression.
fn called_name(expression: &str) -> Option<&str> {
    let bytes = expression.as_bytes();
    let mut depth = 0i32;
    let mut open = None;
    for at in (0..bytes.len()).rev() {
        match bytes[at] {
            b')' => depth += 1,
            b'(' => {
                depth -= 1;
                if depth == 0 {
                    open = Some(at);
                    break;
                }
            }
            _ => {}
        }
    }
    let open = open?;
    let before = expression[..open].trim_end();
    let start = before
        .rfind(|character: char| {
            !(character.is_alphanumeric() || character == '_' || character == '$')
        })
        .map_or(0, |at| at + 1);
    (start < before.len()).then_some(&before[start..])
}

/// The base of an indexing expression `base[index]`.
fn index_base(expression: &str) -> Option<&str> {
    let close = expression.len().checked_sub(1)?;
    let bytes = expression.as_bytes();
    if bytes.get(close) != Some(&b']') {
        return None;
    }
    let mut depth = 0i32;
    let mut open = None;
    for at in (0..=close).rev() {
        match bytes[at] {
            b']' => depth += 1,
            b'[' => {
                depth -= 1;
                if depth == 0 {
                    open = Some(at);
                    break;
                }
            }
            _ => {}
        }
    }
    Some(expression[..open?].trim())
}

fn expression_receiver(expression: &str, found: &Declared) -> Receiver {
    let expression = strip_outer_parens(expression.trim());
    if expression.is_empty() {
        return Receiver::default();
    }
    if let Some((ty, rest)) = cast_prefix(expression)
        && !rest.is_empty()
    {
        return receiver_of_type(ty);
    }
    if expression.starts_with("new ") {
        if expression.contains('[') {
            return sequence_receiver();
        }
        return new_type(expression).map_or_else(reference_receiver, receiver_of_type);
    }
    if expression.starts_with('"') {
        return Receiver {
            known: true,
            reference: true,
            text: true,
            ..Receiver::default()
        };
    }
    if expression.starts_with('\'') {
        return Receiver {
            known: true,
            ..Receiver::default()
        };
    }
    match expression {
        "true" | "false" => {
            return Receiver {
                known: true,
                boolean: true,
                ..Receiver::default()
            };
        }
        "null" | "this" | "super" => return reference_receiver(),
        _ => {}
    }
    if expression
        .chars()
        .next()
        .is_some_and(|character| character.is_ascii_digit())
    {
        return Receiver {
            known: true,
            number: true,
            ..Receiver::default()
        };
    }
    if expression.ends_with(')') {
        if let Some(name) = called_name(expression)
            && let Some(ty) = found.methods.get(name)
        {
            return receiver_of_type(ty);
        }
        return Receiver::default();
    }
    if expression.ends_with(']') {
        if let Some(base) = index_base(expression)
            && let Some(ty) = found.variables.get(last_segment(base))
        {
            let mut element = ty.trim();
            if let Some(at) = element.find('[') {
                element = &element[..at];
            }
            return receiver_of_type(element);
        }
        return Receiver::default();
    }
    if expression.ends_with('}') {
        return Receiver::default();
    }
    if let Some(ty) = found.variables.get(last_segment(expression)) {
        return receiver_of_type(ty);
    }
    Receiver::default()
}

fn postfix_receiver(text: &str, tree: Option<&Tree>, range: Range<usize>) -> Receiver {
    let end = range.end.min(text.len());
    let start = range.start.min(end);
    let expression = &text[start..end];
    let parsed;
    let tree = match tree {
        Some(tree) => tree,
        None => {
            let mut parser = Parser::new();
            if parser
                .set_language(&tree_sitter_java_orchard::LANGUAGE.into())
                .is_err()
            {
                return Receiver::default();
            }
            let Some(tree) = parser.parse(text, None) else {
                return Receiver::default();
            };
            parsed = tree;
            &parsed
        }
    };
    let mut found = Declared::default();
    collect_declarations(tree.root_node(), text, &mut found);
    expression_receiver(expression, &found)
}

/// Whether a postfix template fits the kind of the expression before it. A kind the
/// language did not prove fits every template.
fn postfix_applies(template: &Template, receiver: Receiver) -> bool {
    if !receiver.known {
        return true;
    }
    // A plain reference may still be an Iterable or a CharSequence: only a type that is
    // proven to be something else keeps those templates away.
    let may_hold_elements =
        receiver.sequence || (receiver.reference && !receiver.text && !receiver.boolean);
    match template.key {
        "for" | "iter" | "stream" => may_hold_elements,
        "fori" | "forr" => receiver.number,
        "null" | "notnull" | "nn" | "opt" | "cast" | "inst" => receiver.reference,
        "not" => receiver.boolean,
        "format" => {
            receiver.text || (receiver.reference && !receiver.sequence && !receiver.boolean)
        }
        _ => true,
    }
}

fn language() -> &'static TemplateLanguage {
    static LANGUAGE: OnceLock<TemplateLanguage> = OnceLock::new();
    LANGUAGE.get_or_init(|| TemplateLanguage {
        templates: TEMPLATES,
        contexts: CONTEXTS,
        grammar: || tree_sitter_java_orchard::LANGUAGE.into(),
        indent_unit: "    ",
        receiver_prefixes: &["new"],
        postfix: Some(PostfixRules {
            receiver: postfix_receiver,
            applies: postfix_applies,
        }),
        query: OnceLock::new(),
        declarations: Some(declarations),
    })
}

/// The templates for `path`, for the editor.
pub fn templates_for(path: &Path) -> Option<&'static TemplateLanguage> {
    is_source(path).then(language)
}

/// The declarations around `offset`, for the variables of a live template: the
/// innermost type, the method it holds and the package of the file.
pub fn declarations(
    _language: &TemplateLanguage,
    text: &str,
    tree: Option<&tree_sitter::Tree>,
    offset: usize,
) -> Declarations {
    let parsed;
    let tree = match tree {
        Some(tree) => tree,
        None => {
            let mut parser = tree_sitter::Parser::new();
            let grammar = tree_sitter_java_orchard::LANGUAGE.into();
            if parser.set_language(&grammar).is_err() {
                return Declarations::default();
            }
            let Some(tree) = parser.parse(text, None) else {
                return Declarations::default();
            };
            parsed = tree;
            &parsed
        }
    };
    let mut declarations = Declarations::default();
    let root = tree.root_node();
    declarations.package_name = (0..root.named_child_count())
        .filter_map(|index| root.named_child(index as u32))
        .find(|child| child.kind() == "package_declaration")
        .and_then(|declaration| {
            (0..declaration.named_child_count())
                .filter_map(|index| declaration.named_child(index as u32))
                .find(|child| matches!(child.kind(), "identifier" | "scoped_identifier"))
        })
        .map(|name| name.utf8_text(text.as_bytes()).unwrap_or_default())
        .unwrap_or_default()
        .to_owned();
    let offset = offset.min(text.len());
    let mut node = root.descendant_for_byte_range(offset, offset);
    while let Some(current) = node {
        match current.kind() {
            "method_declaration"
            | "constructor_declaration"
            | "compact_constructor_declaration"
                if declarations.method_name.is_empty() =>
            {
                if let Some(name) = current.child_by_field_name("name") {
                    declarations.method_name = name
                        .utf8_text(text.as_bytes())
                        .unwrap_or_default()
                        .to_owned();
                }
            }
            "class_declaration"
            | "interface_declaration"
            | "enum_declaration"
            | "record_declaration"
            | "annotation_type_declaration"
                if declarations.class_name.is_empty() =>
            {
                if let Some(name) = current.child_by_field_name("name") {
                    declarations.class_name = name
                        .utf8_text(text.as_bytes())
                        .unwrap_or_default()
                        .to_owned();
                }
            }
            _ => {}
        }
        node = current.parent();
    }
    declarations
}

#[cfg(test)]
mod tests {
    use templates::{context_at, expand};
    use tree_sitter::Parser;

    use super::*;

    fn context(source: &str, marker: &str) -> &'static str {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_java_orchard::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let offset = source.find(marker).unwrap() + marker.len();
        context_at(language(), Some(&tree), source.as_bytes(), offset)
    }

    #[test]
    fn the_context_query_compiles_and_tells_bodies_apart() {
        assert!(language().query().is_some());
        let source =
            "class A {\n    psvm\n    void f() {\n        sout\n        g(St);\n    }\n}\n";
        assert_eq!(context(source, "psvm"), "declaration");
        assert_eq!(context(source, "sout"), "statement");
        assert_eq!(context(source, "g(St"), "expression");
        assert_eq!(context("psvm", "psvm"), "other");
    }

    #[test]
    fn every_template_fits_somewhere_and_expands() {
        let java = language();
        assert!(
            java.find(Kind::Abbreviation, "psvm", "declaration")
                .is_some()
        );
        assert!(java.find(Kind::Abbreviation, "psvm", "statement").is_none());
        assert!(java.find(Kind::Abbreviation, "St", "expression").is_some());
        for template in TEMPLATES {
            let expansion = expand(template, "", "    ", Some("a();"), Some("x"));
            assert!(!expansion.text.contains('$'), "{}", template.key);
        }
        let sout = java.find(Kind::Abbreviation, "sout", "statement").unwrap();
        let expansion = expand(sout, "", "    ", None, None);
        assert_eq!(&expansion.text[..expansion.end], "System.out.println(");
    }

    fn receiver(source: &str, expression: &str) -> Receiver {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_java_orchard::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let start = source.find(expression).unwrap();
        let rules = language().postfix.unwrap();
        (rules.receiver)(source, Some(&tree), start..start + expression.len())
    }

    fn offered(source: &str, expression: &str, key: &str) -> bool {
        let rules = language().postfix.unwrap();
        let template = language().find(Kind::Postfix, key, "statement").unwrap();
        (rules.applies)(template, receiver(source, expression))
    }

    #[test]
    fn a_postfix_template_is_offered_only_for_the_kinds_it_handles() {
        let source = r#"class A {
    void run(int count, boolean flag, int[] numbers, java.util.List<String> names, String title) {
    }
}
"#;
        assert!(!offered(source, "count", "for"), "an int is not a sequence");
        assert!(!offered(source, "count", "iter"));
        assert!(!offered(source, "count", "nn"), "an int has no null");
        assert!(!offered(source, "count", "null"));
        assert!(!offered(source, "count", "not"), "an int is not boolean");
        assert!(!offered(source, "count", "format"));
        assert!(offered(source, "flag", "not"));
        assert!(!offered(source, "flag", "for"));
        assert!(offered(source, "numbers", "for"), "an array is iterable");
        assert!(offered(source, "numbers", "iter"));
        assert!(offered(source, "numbers", "nn"));
        assert!(offered(source, "names", "for"), "a List is iterable");
        assert!(offered(source, "names", "stream"));
        assert!(offered(source, "names", "nn"));
        assert!(offered(source, "names", "opt"));
        assert!(
            !offered(source, "title", "for"),
            "a String is not a sequence"
        );
        assert!(!offered(source, "title", "stream"));
        assert!(offered(source, "title", "format"));
        assert!(offered(source, "count", "fori"));
        assert!(!offered(source, "title", "fori"));
        assert!(offered(source, "count", "var"), "var takes anything");
        assert!(offered(source, "names", "sout"));
    }

    #[test]
    fn the_type_of_an_expression_comes_from_its_declaration_and_its_method() {
        let source = r#"class A {
    int[] numbers;
    java.util.List<String> names() { return null; }
    void run() {
        var items = new java.util.ArrayList<String>();
        Object value = unknown;
    }
}
"#;
        assert!(receiver(source, "numbers").sequence);
        assert!(receiver(source, "numbers").reference);
        assert!(receiver(source, "names()").sequence);
        assert!(
            receiver(source, "items").sequence,
            "var follows the type of its initializer"
        );
        assert!(
            !receiver(source, "unknown").known,
            "an unknown name hides nothing"
        );
    }

    #[test]
    fn the_names_of_the_place_come_from_the_tree_and_the_text() {
        let source =
            "package com.shop;\nclass Order {\n    void total() {\n        int x = 1;\n    }\n}\n";
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_java_orchard::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let offset = source.find("int x").unwrap() + 6;

        let names = declarations(language(), source, Some(&tree), offset);

        assert_eq!(names.package_name, "com.shop");
        assert_eq!(names.class_name, "Order");
        assert_eq!(names.method_name, "total");
        assert!(declarations(language(), "package a.b;\n", None, 0).package_name == "a.b");
    }
}
