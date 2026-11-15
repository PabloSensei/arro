//! The pieces of a source file the spelling check reads: the comments, the string
//! literals and the names of the declarations. A use of a declaration and a keyword are
//! no place of their own: the editor renames the symbol instead of editing the use.

use std::ops::Range;

use tree_sitter::{Node, Parser, Tree};

/// A piece of a source the check reads, by bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Place {
    pub range: Range<usize>,
    /// The place is the name of a declaration.
    pub declaration: bool,
}

fn parse(text: &str) -> Option<Tree> {
    let mut parser = Parser::new();
    parser.set_language(&super::config().grammar).ok()?;
    parser.parse(text, None)
}

/// The places of a source file, in the order they appear.
pub fn places(text: &str) -> Vec<Place> {
    let Some(tree) = parse(text) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    visit(tree.root_node(), &mut found);
    found.sort_by_key(|place| place.range.start);
    found.dedup_by(|left, right| left.range == right.range);
    found
}

fn visit(node: Node<'_>, found: &mut Vec<Place>) {
    match node.kind() {
        // A comment and a literal are read whole: the markers and the escapes inside
        // them are no words of their own, so no walk into the node is needed.
        "line_comment" | "block_comment" | "string_literal" | "text_block" => {
            push(found, node.byte_range(), false);
            return;
        }
        "inferred_parameters" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if child.kind() == "identifier" {
                    push(found, child.byte_range(), true);
                }
            }
        }
        _ => {
            if let Some(name) = node.child_by_field_name("name")
                && lenient_name(node.kind())
                && matches!(name.kind(), "identifier" | "type_identifier")
            {
                push(found, name.byte_range(), true);
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        visit(child, found);
    }
}

/// The declarations whose name is a place: a type, a method, a field, a variable, a
/// parameter and a constant of an enumeration. A constructor keeps the name of its
/// type, which the type declaration already names.
fn lenient_name(kind: &str) -> bool {
    matches!(
        kind,
        "class_declaration"
            | "interface_declaration"
            | "enum_declaration"
            | "record_declaration"
            | "annotation_type_declaration"
            | "method_declaration"
            | "annotation_type_element_declaration"
            | "variable_declarator"
            | "formal_parameter"
            | "catch_formal_parameter"
            | "spread_parameter"
            | "receiver_parameter"
            | "enhanced_for_statement"
            | "enum_constant"
    )
}

fn push(found: &mut Vec<Place>, range: Range<usize>, declaration: bool) {
    if range.start < range.end {
        found.push(Place { range, declaration });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = r#"package com.example;

import com.example.util.List;

// The calcualte of the total.
/** Javadoc with a calcualte. */
class Calculator {
    private int calcualteTotal;

    void calcualteTotal(int serverCount) {
        String note = "a calcualte string";
        int calcualteTotal = serverCount + 1;
        return calcualteTotal;
    }
}
"#;

    fn places_with(text: &str, needle: &str) -> Vec<Place> {
        let at = text.find(needle).expect("the text has the needle");
        places(text)
            .into_iter()
            .filter(|place| place.range.start <= at && at < place.range.end)
            .collect()
    }

    #[test]
    fn comments_and_strings_are_places_but_imports_are_not() {
        let found = places(SOURCE);
        let comment = &SOURCE[found[0].range.clone()];
        assert!(comment.starts_with("// The calcualte"), "{comment}");
        assert!(!found[0].declaration);
        assert!(
            places_with(SOURCE, "Javadoc with a calcualte")
                .iter()
                .any(|place| !place.declaration)
        );
        assert!(
            places_with(SOURCE, "a calcualte string")
                .iter()
                .any(|place| !place.declaration)
        );
        assert!(
            places_with(SOURCE, "util.List").is_empty(),
            "an import is no place of its own"
        );
        assert!(
            places_with(SOURCE, "class Calculator").is_empty()
                || places_with(SOURCE, "class Calculator")
                    .iter()
                    .all(|place| place.range.start > SOURCE.find("class Calculator").unwrap()),
            "the keyword is no place"
        );
    }

    #[test]
    fn the_names_of_the_declarations_are_places_and_their_uses_are_not() {
        let class = places_with(SOURCE, "Calculator {");
        assert_eq!(class.len(), 1);
        assert!(class[0].declaration, "the class name is a declaration");

        let field = places_with(SOURCE, "calcualteTotal;");
        assert_eq!(field.len(), 1);
        assert!(field[0].declaration);

        let parameter = places_with(SOURCE, "serverCount)");
        assert_eq!(parameter.len(), 1);
        assert!(parameter[0].declaration);

        let local = places_with(SOURCE, "calcualteTotal = serverCount");
        assert_eq!(local.len(), 1);
        assert!(local[0].declaration);

        let used = SOURCE.rfind("calcualteTotal").unwrap();
        assert!(
            places(SOURCE)
                .iter()
                .all(|place| !(place.range.start <= used && used < place.range.end)),
            "a use of the name is no place"
        );
    }
}
