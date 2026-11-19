//! The nodes a command may move: the statements of a block, the members of a type and
//! the elements of a list. Which nodes these are is the business of the language query,
//! so nothing here names a node kind.

use std::ops::Range;

use streaming_iterator::StreamingIterator;
use tree_sitter::{Node, Query, QueryCursor, Tree};

/// The innermost unit of `query` that holds `caret` and has a neighbour in the direction
/// `forward`, together with all its siblings on the same level and its index among them.
///
/// Captures called `item` are elements of a list: a lone item falls through to the unit
/// that holds the list, so the call of a single argument moves as a statement. A capture
/// called `unit` ends the search, because a statement without a neighbour stays where it
/// is instead of taking its whole method with it.
pub fn unit_siblings(
    tree: &Tree,
    query: &Query,
    source: &[u8],
    caret: Range<usize>,
    forward: bool,
) -> Option<(Vec<Range<usize>>, usize)> {
    let mut cursor = QueryCursor::new();
    if !source.is_empty() {
        let start = caret.start.min(source.len() - 1);
        let end = caret.end.max(start + 1).min(source.len());
        cursor.set_byte_range(start..end);
    }
    let names = query.capture_names();
    let mut found: Vec<(Node, bool)> = Vec::new();
    let mut matches = cursor.matches(query, tree.root_node(), source);
    while let Some(found_match) = matches.next() {
        for capture in found_match.captures() {
            let node = capture.node;
            let holds = node.start_byte() <= caret.start && caret.end <= node.end_byte();
            if holds && !found.iter().any(|(seen, _)| seen.id() == node.id()) {
                let item = names
                    .get(capture.index as usize)
                    .is_some_and(|name| *name == "item");
                found.push((node, item));
            }
        }
    }
    found.sort_by_key(|(node, _)| node.end_byte() - node.start_byte());
    for (node, item) in found {
        let Some(parent) = node.parent() else {
            continue;
        };
        let mut walk = parent.walk();
        let children: Vec<Node> = parent.named_children(&mut walk).collect();
        let Some(index) = children.iter().position(|child| child.id() == node.id()) else {
            continue;
        };
        let neighbour = if forward {
            index + 1
        } else {
            match index.checked_sub(1) {
                Some(previous) => previous,
                None => continue,
            }
        };
        if neighbour >= children.len() {
            if item {
                continue;
            }
            return None;
        }
        let units = children
            .iter()
            .map(|child| child.byte_range())
            .collect::<Vec<_>>();
        return Some((units, index));
    }
    None
}
