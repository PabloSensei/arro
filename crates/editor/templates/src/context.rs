//! The context at the caret from the syntax tree.

use streaming_iterator::StreamingIterator;
use tree_sitter::{QueryCursor, Tree};

use crate::template::TemplateLanguage;

/// The name of the innermost captured node with `offset` inside it, after its first byte
/// and before its last. Without a tree or a capture the context is `other`.
pub fn context_at(
    language: &'static TemplateLanguage,
    tree: Option<&Tree>,
    text: &[u8],
    offset: usize,
) -> &'static str {
    let (Some(tree), Some(query)) = (tree, language.query()) else {
        return "other";
    };
    let mut cursor = QueryCursor::new();
    cursor.set_byte_range(offset.saturating_sub(1)..offset + 1);
    let mut best: Option<(usize, usize)> = None;
    let mut captures = cursor.captures(query, tree.root_node(), text);
    while let Some((found, index)) = captures.next() {
        let capture = found.captures()[*index];
        let range = capture.node.byte_range();
        if range.start < offset && offset < range.end {
            let size = range.len();
            if best.is_none_or(|(known, _)| size < known) {
                best = Some((size, capture.index as usize));
            }
        }
    }
    best.map_or("other", |(_, index)| query.capture_names()[index])
}
