//! The identifiers of the lines of one declaration, for the values a stopped debugger
//! shows after those lines.

use lang_api::IdentifierLines;
use tree_sitter::Node;

use super::edit::{node_at, parse};

const METHOD_DECLARATIONS: &[&str] = &[
    "method_declaration",
    "constructor_declaration",
    "compact_constructor_declaration",
];

/// The identifiers of the lines from the start of the method around `line` through
/// `line`. `None` when the line has no method around it.
pub fn identifiers_around(text: &str, line: usize) -> Option<IdentifierLines> {
    let start = line_start(text, line)?;
    let tree = parse(text)?;
    let mut node = node_at(&tree, start);
    while !METHOD_DECLARATIONS.contains(&node.kind()) {
        node = node.parent()?;
    }
    let first_line = node.start_position().row;
    let last_line = line;
    if last_line < first_line {
        return None;
    }
    let mut lines = vec![Vec::new(); last_line - first_line + 1];
    collect(node, text, first_line, last_line, &mut lines);
    Some(IdentifierLines { first_line, lines })
}

/// Adds every identifier of `node` that stands on the lines from `first_line` through
/// `last_line`, keeping the order of the text and one entry per name and line.
fn collect(
    node: Node<'_>,
    text: &str,
    first_line: usize,
    last_line: usize,
    lines: &mut [Vec<String>],
) {
    if node.kind() == "identifier" {
        let row = node.start_position().row;
        if (first_line..=last_line).contains(&row) {
            let name = &text[node.byte_range()];
            let line = &mut lines[row - first_line];
            if !line.iter().any(|kept| kept == name) {
                line.push(name.to_owned());
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect(child, text, first_line, last_line, lines);
    }
}

/// The byte offset where `line` starts, when the text has that line.
fn line_start(text: &str, line: usize) -> Option<usize> {
    let mut offset = 0;
    for (index, part) in text.split_inclusive('\n').enumerate() {
        if index == line {
            return Some(offset);
        }
        offset += part.len();
    }
    (line == 0).then_some(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = "\
class Demo {
    void work(int a) {
        int total = a + 1;
        other(total);
    }

    void other(int value) {
        int unused = 0;
    }
}
";

    #[test]
    fn the_lines_of_a_method_hold_its_identifiers() {
        let found = identifiers_around(SOURCE, 3).expect("the line is in a method");

        assert_eq!(found.first_line, 1);
        assert_eq!(found.lines.len(), 3);
        assert_eq!(found.lines[0], vec!["work", "a"]);
        assert_eq!(found.lines[1], vec!["total", "a"]);
        assert_eq!(found.lines[2], vec!["other", "total"]);
    }

    #[test]
    fn the_identifiers_of_another_method_stay_out() {
        let found = identifiers_around(SOURCE, 2).expect("the line is in a method");

        assert_eq!(found.first_line, 1);
        assert!(
            found
                .lines
                .iter()
                .flatten()
                .all(|name| name != "unused" && name != "other")
        );
    }

    #[test]
    fn a_line_outside_a_method_has_none() {
        assert!(identifiers_around("class Demo {}\n", 0).is_none());
    }
}
