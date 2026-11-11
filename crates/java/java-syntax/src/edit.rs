//! Edits that need the tree of one file: the constructs around a place for
//! Unwrap/Remove, and the declaration under a place for Safe Delete.

use std::ops::Range;

use tree_sitter::{Node, Parser, Tree};

fn parse(text: &str) -> Option<Tree> {
    let mut parser = Parser::new();
    parser.set_language(&super::config().grammar).ok()?;
    parser.parse(text, None)
}

fn node_at(tree: &Tree, offset: usize) -> Node<'_> {
    let offset = offset.min(tree.root_node().end_byte());
    tree.root_node()
        .descendant_for_byte_range(offset, offset)
        .or_else(|| {
            offset
                .checked_sub(1)
                .and_then(|before| tree.root_node().descendant_for_byte_range(before, before))
        })
        .unwrap_or(tree.root_node())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeletionKind {
    Type,
    Method,
    Field,
    Parameter,
    Local,
}

/// The declaration under the caret. Byte offsets are in the text that was asked about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Deletion {
    pub kind: DeletionKind,
    pub name: String,
    pub range: Range<usize>,
    pub name_range: Range<usize>,
    pub method: Option<Range<usize>>,
    pub method_name: Option<Range<usize>>,
    pub parameter_index: Option<usize>,
    pub sole_type: bool,
}

/// One entry of Unwrap/Remove.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnwrapEdit {
    pub title: String,
    pub start: usize,
    pub end: usize,
    pub replacement: String,
}

const TYPE_DECLARATIONS: &[&str] = &[
    "class_declaration",
    "interface_declaration",
    "enum_declaration",
    "record_declaration",
    "annotation_type_declaration",
];

const METHOD_DECLARATIONS: &[&str] = &[
    "method_declaration",
    "constructor_declaration",
    "compact_constructor_declaration",
];

fn is_type(node: Node<'_>) -> bool {
    TYPE_DECLARATIONS.contains(&node.kind())
}

fn is_method(node: Node<'_>) -> bool {
    METHOD_DECLARATIONS.contains(&node.kind())
}

fn text_of<'a>(text: &'a str, node: Node<'_>) -> &'a str {
    &text[node.byte_range()]
}

fn name_node<'a>(node: Node<'a>) -> Option<Node<'a>> {
    node.child_by_field_name("name")
}

fn body_node<'a>(node: Node<'a>) -> Option<Node<'a>> {
    node.child_by_field_name("body")
}

/// `true` when `offset` is above the body of a type or a method, where its declaration
/// is. A place inside a body belongs to what that body declares.
fn above_body(node: Node<'_>, offset: usize) -> bool {
    body_node(node).is_none_or(|body| offset < body.start_byte())
}

/// The whole lines of `node`, so a removed member leaves no empty line.
fn lines_of(text: &str, node: Node<'_>) -> Range<usize> {
    let mut start = node.start_byte();
    let line_start = text[..start].rfind('\n').map_or(0, |at| at + 1);
    if text[line_start..start].trim().is_empty() {
        start = line_start;
    }
    let mut end = node.end_byte();
    let line_end = text[end..].find('\n').map_or(text.len(), |at| end + at + 1);
    if text[end..line_end].trim().is_empty() {
        end = line_end;
    }
    start..end
}

fn sole_type(root: Node<'_>) -> bool {
    let mut count = 0;
    let mut cursor = root.walk();
    for child in root.named_children(&mut cursor) {
        if is_type(child) {
            count += 1;
        }
    }
    count == 1
}

fn local_declarator(node: Node<'_>) -> Option<Node<'_>> {
    if node.kind() == "variable_declarator" {
        return Some(node);
    }
    let mut current = Some(node);
    while let Some(found) = current {
        if found.kind() == "variable_declarator" {
            return Some(found);
        }
        if found.kind() == "local_variable_declaration" {
            return found.child_by_field_name("declarator");
        }
        current = found.parent();
    }
    None
}

/// The declaration at `offset`, the way Safe Delete reads it.
pub fn deletion(text: &str, offset: usize) -> Option<Deletion> {
    let tree = parse(text)?;
    let start = node_at(&tree, offset);
    let mut node = start;
    // A parameter's name and a local variable's name are identifiers inside the
    // declaration that is removed.
    if let Some(parent) = node.parent()
        && node.kind().ends_with("identifier")
    {
        node = parent;
    }
    loop {
        match node.kind() {
            "formal_parameter" | "spread_parameter" | "catch_formal_parameter" => {
                return parameter(text, node);
            }
            "local_variable_declaration" => {
                return local(text, node);
            }
            "field_declaration" | "constant_declaration" | "enum_constant" => {
                return field(text, node);
            }
            _ if is_method(node) && above_body(node, offset) => {
                return method(text, node);
            }
            _ if is_type(node) && above_body(node, offset) => {
                return type_declaration(text, node, &tree);
            }
            _ => {}
        }
        node = node.parent()?;
    }
}

fn type_declaration(text: &str, node: Node<'_>, tree: &Tree) -> Option<Deletion> {
    let name = name_node(node).or_else(|| {
        node.named_children(&mut node.walk())
            .find(|child| child.kind() == "identifier")
    })?;
    Some(Deletion {
        kind: DeletionKind::Type,
        name: text_of(text, name).to_owned(),
        range: lines_of(text, node),
        name_range: name.byte_range(),
        method: None,
        method_name: None,
        parameter_index: None,
        sole_type: sole_type(tree.root_node()),
    })
}

fn method(text: &str, node: Node<'_>) -> Option<Deletion> {
    let name = name_node(node).or_else(|| {
        node.named_children(&mut node.walk())
            .find(|child| child.kind() == "identifier")
    })?;
    Some(Deletion {
        kind: DeletionKind::Method,
        name: text_of(text, name).to_owned(),
        range: lines_of(text, node),
        name_range: name.byte_range(),
        method: None,
        method_name: None,
        parameter_index: None,
        sole_type: false,
    })
}

fn field(text: &str, node: Node<'_>) -> Option<Deletion> {
    let declarator = node
        .child_by_field_name("declarator")
        .or_else(|| local_declarator(node));
    let name = declarator
        .and_then(|declarator| declarator.child_by_field_name("name"))
        .or_else(|| {
            node.named_children(&mut node.walk())
                .find(|child| child.kind().ends_with("identifier"))
        })?;
    Some(Deletion {
        kind: DeletionKind::Field,
        name: text_of(text, name).to_owned(),
        range: lines_of(text, node),
        name_range: name.byte_range(),
        method: None,
        method_name: None,
        parameter_index: None,
        sole_type: false,
    })
}

fn local(text: &str, node: Node<'_>) -> Option<Deletion> {
    let declarator = node.child_by_field_name("declarator")?;
    let name = declarator.child_by_field_name("name")?;
    Some(Deletion {
        kind: DeletionKind::Local,
        name: text_of(text, name).to_owned(),
        range: lines_of(text, node),
        name_range: name.byte_range(),
        method: None,
        method_name: None,
        parameter_index: None,
        sole_type: false,
    })
}

fn parameter(text: &str, node: Node<'_>) -> Option<Deletion> {
    let name = name_node(node).or_else(|| {
        node.named_children(&mut node.walk())
            .find(|child| child.kind().ends_with("identifier"))
    })?;
    let parent = node.parent()?;
    let index = parent
        .named_children(&mut parent.walk())
        .filter(|child| {
            matches!(
                child.kind(),
                "formal_parameter" | "spread_parameter" | "catch_formal_parameter"
            )
        })
        .position(|child| child.id() == node.id())?;
    let mut range = node.byte_range();
    // A comma belongs to the parameter that is removed, not to the empty place left.
    let mut siblings = parent.walk();
    let mut cursor = parent.children(&mut siblings).peekable();
    while let Some(child) = cursor.next() {
        if child.id() == node.id() {
            if let Some(next) = cursor.peek() {
                let after = text[child.end_byte()..next.start_byte()].trim();
                if after.is_empty() && next.kind() == "," {
                    range.end = next.end_byte();
                }
            }
            break;
        }
    }
    if range.end == node.end_byte() {
        let mut siblings = parent.walk();
        let mut previous = None;
        for child in parent.children(&mut siblings) {
            if child.id() == node.id() {
                break;
            }
            previous = Some(child);
        }
        if let Some(previous) = previous.filter(|child| child.kind() == ",") {
            range.start = previous.start_byte();
        }
    }
    let mut method = node.parent();
    while let Some(found) = method {
        if is_method(found) {
            let method_name = name_node(found).map(|name| name.byte_range());
            return Some(Deletion {
                kind: DeletionKind::Parameter,
                name: text_of(text, name).to_owned(),
                range,
                name_range: name.byte_range(),
                method: Some(found.byte_range()),
                method_name,
                parameter_index: Some(index),
                sole_type: false,
            });
        }
        method = found.parent();
    }
    Some(Deletion {
        kind: DeletionKind::Parameter,
        name: text_of(text, name).to_owned(),
        range,
        name_range: name.byte_range(),
        method: None,
        method_name: None,
        parameter_index: None,
        sole_type: false,
    })
}

/// The method declaration around `offset`, with its name and the range of that name.
pub fn method_at(text: &str, offset: usize) -> Option<(String, Range<usize>)> {
    let tree = parse(text)?;
    let mut node = node_at(&tree, offset);
    while !is_method(node) {
        node = node.parent()?;
    }
    let name = name_node(node)?;
    Some((text_of(text, name).to_owned(), name.byte_range()))
}

/// The type declaration around `offset`, with its name and the range of that name.
pub fn enclosing_type_at(text: &str, offset: usize) -> Option<(String, Range<usize>)> {
    let tree = parse(text)?;
    let mut node = node_at(&tree, offset);
    loop {
        if is_type(node) {
            let name = name_node(node)?;
            return Some((text_of(text, name).to_owned(), name.byte_range()));
        }
        node = node.parent()?;
    }
}

/// The names of the methods that a test class for this source can call, in source
/// order. Constructors and private methods are left out.
pub fn testable_methods(text: &str) -> Vec<String> {
    let Some(tree) = parse(text) else {
        return Vec::new();
    };
    let mut names = Vec::new();
    collect_methods(tree.root_node(), text, &mut names);
    names
}

fn collect_methods(node: Node<'_>, text: &str, names: &mut Vec<String>) {
    if node.kind() == "method_declaration"
        && !is_private(node, text)
        && let Some(name) = name_node(node)
    {
        names.push(text_of(text, name).to_owned());
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_methods(child, text, names);
    }
}

fn has_modifier(node: Node<'_>, text: &str, wanted: &str) -> bool {
    let modifiers = node.child_by_field_name("modifiers").or_else(|| {
        node.named_children(&mut node.walk())
            .find(|child| child.kind() == "modifiers")
    });
    modifiers.is_some_and(|modifiers| {
        text_of(text, modifiers)
            .split(|character: char| !character.is_alphanumeric() && character != '_')
            .any(|word| word == wanted)
    })
}

fn is_private(node: Node<'_>, text: &str) -> bool {
    has_modifier(node, text, "private")
}

/// The name of the first top-level type of the text.
pub fn first_type_name(text: &str) -> Option<String> {
    let tree = parse(text)?;
    let root = tree.root_node();
    let mut cursor = root.walk();
    root.named_children(&mut cursor)
        .find(|child| is_type(*child))
        .and_then(name_node)
        .map(|name| text_of(text, name).to_owned())
}

/// Whether the first type of the text declares `method`, and whether that type is
/// abstract or an interface.
pub fn type_declares_method(text: &str, method: &str) -> (bool, bool) {
    let Some(tree) = parse(text) else {
        return (false, false);
    };
    let root = tree.root_node();
    let mut cursor = root.walk();
    let Some(found) = root
        .named_children(&mut cursor)
        .find(|child| is_type(*child))
    else {
        return (false, false);
    };
    let mut names = Vec::new();
    collect_methods(found, text, &mut names);
    let abstract_type = matches!(
        found.kind(),
        "interface_declaration" | "annotation_type_declaration"
    ) || has_modifier(found, text, "abstract");
    (names.iter().any(|found| found == method), abstract_type)
}

/// The offset of the name of the first top-level type, for Safe Delete on a file.
pub fn first_type_offset(text: &str) -> Option<usize> {
    let tree = parse(text)?;
    let root = tree.root_node();
    let mut cursor = root.walk();
    root.named_children(&mut cursor)
        .find(|child| is_type(*child))
        .and_then(name_node)
        .map(|name| name.start_byte())
}

fn indent_at(text: &str, offset: usize) -> String {
    let line_start = text[..offset.min(text.len())]
        .rfind('\n')
        .map_or(0, |at| at + 1);
    let line = &text[line_start..offset.min(text.len())];
    line.chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect()
}

/// The body of a block without its braces, moved to `indent`.
fn block_body(text: &str, block: Node<'_>, indent: &str) -> String {
    let inner = &text[block.start_byte() + 1..block.end_byte().saturating_sub(1)];
    let inner = inner.strip_prefix('\n').unwrap_or(inner);
    let inner = inner.trim_end();
    let lines: Vec<&str> = inner.lines().collect();
    let base = lines
        .iter()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.len() - line.trim_start().len())
        .min()
        .unwrap_or(0);
    let mut out = Vec::new();
    for line in lines {
        if line.trim().is_empty() {
            out.push(String::new());
            continue;
        }
        let mut moved = String::from(indent);
        moved.push_str(&line[base.min(line.len())..]);
        out.push(moved);
    }
    out.join("\n")
}

fn statement_body(text: &str, body: Node<'_>, indent: &str) -> String {
    let body_text = text[body.byte_range()].trim();
    if body_text.contains('\n') {
        let base = indent_at(text, body.start_byte());
        let mut out = Vec::new();
        for line in body_text.lines() {
            let line = line.strip_prefix(&base).unwrap_or(line);
            out.push(format!("{indent}{line}"));
        }
        out.join("\n")
    } else {
        format!("{indent}{body_text}")
    }
}

/// The indentation before `offset` is already in the text the edit keeps, so the first
/// line of a replacement at the start of a line carries no indentation of its own.
fn without_first_indent(replacement: String, indent: &str, at_line_start: bool) -> String {
    match replacement.strip_prefix(indent) {
        Some(rest) if at_line_start => rest.to_owned(),
        _ => replacement,
    }
}

fn at_line_start(text: &str, offset: usize) -> bool {
    let line_start = text[..offset.min(text.len())]
        .rfind('\n')
        .map_or(0, |at| at + 1);
    text[line_start..offset.min(text.len())].trim().is_empty()
}

fn unwrap_body(text: &str, owner: Node<'_>, body: Node<'_>) -> String {
    let indent = indent_at(text, owner.start_byte());
    let replacement = if body.kind() == "block" {
        block_body(text, body, &indent)
    } else {
        statement_body(text, body, &indent)
    };
    without_first_indent(
        replacement,
        &indent,
        at_line_start(text, owner.start_byte()),
    )
}

fn edit(title: String, range: Range<usize>, replacement: String) -> UnwrapEdit {
    UnwrapEdit {
        title,
        start: range.start,
        end: range.end,
        replacement,
    }
}

fn line_edit(title: String, text: &str, node: Node<'_>) -> UnwrapEdit {
    let range = node.byte_range();
    let line_start = text[..range.start].rfind('\n').map_or(0, |at| at + 1);
    let line_end = text[range.end..]
        .find('\n')
        .map_or(text.len(), |at| range.end + at + 1);
    let start = if text[line_start..range.start].trim().is_empty() {
        line_start
    } else {
        range.start
    };
    let end = if text[range.end..line_end].trim().is_empty() {
        line_end
    } else {
        range.end
    };
    edit(title, start..end, String::new())
}

/// Ctrl+Shift+Delete: the constructs around `offset`, nearest first.
pub fn unwrap_edits(text: &str, offset: usize) -> Vec<UnwrapEdit> {
    let Some(tree) = parse(text) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    let mut node = Some(node_at(&tree, offset));
    while let Some(current) = node {
        let kind = current.kind();
        match kind {
            "if_statement" => {
                if let Some(consequence) = current.child_by_field_name("consequence") {
                    found.push(edit(
                        "Unwrap 'if'".to_owned(),
                        current.byte_range(),
                        unwrap_body(text, current, consequence),
                    ));
                }
                if let Some(alternative) = current.child_by_field_name("alternative") {
                    let inside =
                        alternative.start_byte() <= offset && offset <= alternative.end_byte();
                    if inside {
                        found.push(edit(
                            "Unwrap 'else'".to_owned(),
                            current.byte_range(),
                            unwrap_body(text, current, alternative),
                        ));
                    } else {
                        let start = current
                            .child_by_field_name("consequence")
                            .map_or(current.start_byte(), |body| body.end_byte());
                        found.push(edit(
                            "Remove 'else'".to_owned(),
                            start..alternative.end_byte(),
                            String::new(),
                        ));
                    }
                }
            }
            "for_statement" | "while_statement" | "do_statement" | "synchronized_statement" => {
                if let Some(body) = body_node(current) {
                    let name = match kind {
                        "for_statement" => "for",
                        "while_statement" => "while",
                        "do_statement" => "do",
                        _ => "synchronized",
                    };
                    found.push(edit(
                        format!("Unwrap '{name}'"),
                        current.byte_range(),
                        unwrap_body(text, current, body),
                    ));
                }
            }
            "try_statement" => {
                if let Some(body) = body_node(current) {
                    found.push(edit(
                        "Unwrap 'try'".to_owned(),
                        current.byte_range(),
                        unwrap_body(text, current, body),
                    ));
                }
            }
            "catch_clause" => {
                found.push(line_edit("Remove 'catch'".to_owned(), text, current));
            }
            "finally_clause" => {
                found.push(line_edit("Remove 'finally'".to_owned(), text, current));
            }
            "block" => {
                let indent = indent_at(text, current.start_byte());
                let body = block_body(text, current, &indent);
                found.push(edit(
                    "Remove braces".to_owned(),
                    current.byte_range(),
                    without_first_indent(body, &indent, at_line_start(text, current.start_byte())),
                ));
            }
            "lambda_expression" => {
                if let Some(body) = body_node(current) {
                    found.push(edit(
                        "Unwrap lambda body".to_owned(),
                        current.byte_range(),
                        unwrap_body(text, current, body),
                    ));
                }
            }
            "ternary_expression" => {
                if let Some(then) = current.child_by_field_name("consequence") {
                    found.push(edit(
                        "Unwrap '?:' (then)".to_owned(),
                        current.byte_range(),
                        text_of(text, then).trim().to_owned(),
                    ));
                }
                if let Some(otherwise) = current.child_by_field_name("alternative") {
                    found.push(edit(
                        "Unwrap '?:' (else)".to_owned(),
                        current.byte_range(),
                        text_of(text, otherwise).trim().to_owned(),
                    ));
                }
            }
            _ => {}
        }
        node = current.parent();
    }
    found
}

#[cfg(test)]
mod tests {
    use super::{DeletionKind, deletion, first_type_offset, unwrap_edits};

    fn apply(text: &str, start: usize, end: usize, replacement: &str) -> String {
        let mut out = String::from(text);
        out.replace_range(start..end, replacement);
        out
    }

    #[test]
    fn an_if_is_unwrapped_to_the_body_with_its_indentation() {
        let text = "class A {\n    void f() {\n        if (ready) {\n            work();\n            more();\n        }\n    }\n}\n";
        let offset = text.find("ready").unwrap();
        let options = unwrap_edits(text, offset);
        let if_unwrap = options
            .iter()
            .find(|option| option.title == "Unwrap 'if'")
            .unwrap();
        let edited = apply(text, if_unwrap.start, if_unwrap.end, &if_unwrap.replacement);
        assert_eq!(
            edited,
            "class A {\n    void f() {\n        work();\n        more();\n    }\n}\n"
        );
    }

    #[test]
    fn a_ternary_offers_each_side() {
        let text =
            "class A {\n    int f(boolean b) {\n        return b ? one() : two();\n    }\n}\n";
        let options = unwrap_edits(text, text.find("one").unwrap());
        let titles: Vec<&str> = options.iter().map(|option| option.title.as_str()).collect();
        assert!(titles.contains(&"Unwrap '?:' (then)"));
        assert!(titles.contains(&"Unwrap '?:' (else)"));
    }

    #[test]
    fn the_nearest_construct_comes_first() {
        let text = "class A {\n    void f() {\n        if (a) {\n            while (b) {\n                work();\n            }\n        }\n    }\n}\n";
        let options = unwrap_edits(text, text.find("work").unwrap());
        let titles: Vec<&str> = options.iter().map(|option| option.title.as_str()).collect();
        let block = titles
            .iter()
            .position(|title| *title == "Remove braces")
            .unwrap();
        let while_at = titles
            .iter()
            .position(|title| *title == "Unwrap 'while'")
            .unwrap();
        let if_at = titles
            .iter()
            .position(|title| *title == "Unwrap 'if'")
            .unwrap();
        assert!(block < while_at && while_at < if_at, "{titles:?}");
    }

    #[test]
    fn safe_delete_reads_a_method_and_removes_its_lines() {
        let text = "class A {\n    int f() {\n        return 1;\n    }\n}\n";
        let target = deletion(text, text.find("f()").unwrap()).unwrap();
        assert_eq!(target.kind, DeletionKind::Method);
        assert_eq!(target.name, "f");
        let edited = apply(text, target.range.start, target.range.end, "");
        assert_eq!(edited, "class A {\n}\n");
    }

    #[test]
    fn safe_delete_reads_a_parameter_with_its_comma() {
        let text = "class A {\n    void f(int a, String b) {\n    }\n}\n";
        let target = deletion(text, text.find("String b").unwrap()).unwrap();
        assert_eq!(target.kind, DeletionKind::Parameter);
        assert_eq!(target.parameter_index, Some(1));
        let edited = apply(text, target.range.start, target.range.end, "");
        assert_eq!(edited, "class A {\n    void f(int a) {\n    }\n}\n");
    }

    #[test]
    fn a_sole_type_is_noticed_for_file_delete() {
        let text = "class A {\n}\n";
        assert!(deletion(text, text.find('A').unwrap()).unwrap().sole_type);
        let shared = "class A {\n}\nclass B {\n}\n";
        assert!(
            !deletion(shared, shared.find('A').unwrap())
                .unwrap()
                .sole_type
        );
    }

    #[test]
    fn the_method_and_the_type_around_a_place_are_found() {
        let text = "package p;\n\nclass A {\n    int f(int a) {\n        return a;\n    }\n}\n";
        let (method, name) = super::method_at(text, text.find("return").unwrap()).unwrap();
        assert_eq!(method, "f");
        assert_eq!(&text[name], "f");
        let (found, _) = super::enclosing_type_at(text, text.find("return").unwrap()).unwrap();
        assert_eq!(found, "A");
    }

    #[test]
    fn testable_methods_skip_constructors_and_private_ones() {
        let text = "class A {\n    A() {\n    }\n    public void one() {\n    }\n    private void secret() {\n    }\n}\n";
        assert_eq!(super::testable_methods(text), vec!["one"]);
    }

    #[test]
    fn the_first_type_offset_points_at_its_name() {
        let text = "package p;\n\nclass A {\n}\n";
        assert_eq!(first_type_offset(text), Some(text.find('A').unwrap()));
    }
}
