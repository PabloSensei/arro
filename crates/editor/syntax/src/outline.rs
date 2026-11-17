//! Structure read from the tree: folds, indentation and selection growth.

use std::ops::Range;

use streaming_iterator::StreamingIterator;
use text_core::{RopeStore, TextStore};
use tree_sitter::{Node, Query, QueryCursor, Tree};

use crate::language::FoldStyle;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FoldRange {
    /// Bytes hidden by the fold.
    pub range: Range<usize>,
    pub placeholder: &'static str,
    pub collapsed_on_open: bool,
}

/// A named region of the text: `//region Name` through `//endregion`, or
/// `// <editor-fold desc="Name">` through `// </editor-fold>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Region {
    pub name: String,
    /// The 0-based line of the header.
    pub line: usize,
    /// The start of the header line, where a jump to the region puts the caret.
    pub header: usize,
    /// Bytes hidden when the region folds. Empty for a one-line region.
    pub fold: Range<usize>,
}

/// The named regions of the text. The marker is written after the line comment of the
/// language (`//`, `#`), so this reads any C-like or shell-like source.
pub fn custom_regions(text: &str, line_comment: Option<&str>) -> Vec<Region> {
    let Some(prefix) = line_comment else {
        return Vec::new();
    };
    struct Open {
        name: String,
        line: usize,
        header: usize,
        header_end: usize,
    }
    let mut regions = Vec::new();
    let mut open: Vec<Open> = Vec::new();
    let mut line = 0;
    let mut start = 0;
    for body in text.split_inclusive('\n') {
        let end = start + body.trim_end_matches(['\r', '\n']).len();
        if let Some(marker) = marker_of(&body[..end - start], prefix) {
            match marker {
                Marker::Open(name) => open.push(Open {
                    name,
                    line,
                    header: start,
                    header_end: end,
                }),
                Marker::Close => {
                    let Some(found) = open.pop() else {
                        line += 1;
                        start += body.len();
                        continue;
                    };
                    let fold = found.header_end.min(end)..end;
                    regions.push(Region {
                        name: found.name,
                        line: found.line,
                        header: found.header,
                        fold: fold.start.min(fold.end)..fold.end,
                    });
                }
            }
        }
        line += 1;
        start += body.len();
    }
    regions.sort_by_key(|region| region.header);
    regions
}

/// The folds of the named regions, which the parser's own folds sit next to.
pub fn region_folds(text: &str, line_comment: Option<&str>) -> Vec<FoldRange> {
    custom_regions(text, line_comment)
        .into_iter()
        .filter(|region| text[region.fold.clone()].contains('\n'))
        .map(|region| FoldRange {
            range: region.fold,
            placeholder: "...",
            collapsed_on_open: false,
        })
        .collect()
}

enum Marker {
    Open(String),
    Close,
}

/// `None` unless the line is a region marker written after `prefix`.
fn marker_of(line: &str, prefix: &str) -> Option<Marker> {
    let trimmed = line.trim_start();
    let rest = trimmed.strip_prefix(prefix)?.trim_start();
    let lower = rest.to_ascii_lowercase();
    if lower.starts_with("endregion") {
        return Some(Marker::Close);
    }
    if lower.starts_with("</editor-fold") {
        return Some(Marker::Close);
    }
    if lower.starts_with("<editor-fold") {
        let name = attribute(rest, "desc").unwrap_or_default();
        return Some(Marker::Open(name_or_default(name)));
    }
    if let Some(tail) = rest.strip_prefix("region")
        && (tail.is_empty() || tail.starts_with([' ', '\t', ':']))
    {
        let name = tail.trim_start_matches([' ', '\t', ':']).trim();
        return Some(Marker::Open(name_or_default(name.to_owned())));
    }
    None
}

fn name_or_default(name: String) -> String {
    if name.is_empty() {
        "region".to_owned()
    } else {
        name
    }
}

/// The value of `key="value"` or `key='value'` in `text`.
fn attribute(text: &str, key: &str) -> Option<String> {
    let at = text.to_ascii_lowercase().find(key)?;
    let rest = text[at + key.len()..].trim_start();
    let rest = rest.strip_prefix('=')?.trim_start();
    let quote = rest.chars().next().filter(|c| matches!(c, '"' | '\''))?;
    let body = rest[quote.len_utf8()..]
        .split(quote)
        .next()
        .unwrap_or_default();
    Some(body.to_owned())
}

pub(crate) fn folds(
    tree: &Tree,
    query: &Query,
    text: &[u8],
    styles: &[FoldStyle],
) -> Vec<FoldRange> {
    let mut cursor = QueryCursor::new();
    let mut captures: Vec<(Range<usize>, usize)> = Vec::new();
    let mut matches = cursor.matches(query, tree.root_node(), text);
    while let Some(found) = matches.next() {
        for capture in found.captures() {
            let name = query.capture_names()[capture.index as usize];
            let Some(style) = styles.iter().position(|style| style.capture == name) else {
                continue;
            };
            captures.push((capture.node.byte_range(), style));
        }
    }
    // A doc comment matches two patterns, the earlier style wins.
    captures.sort_by_key(|(range, style)| (range.start, range.end, *style));
    captures.dedup_by_key(|(range, _)| range.clone());

    let is_blank = |range: Range<usize>| text[range].iter().all(u8::is_ascii_whitespace);
    let mut groups: Vec<(Range<usize>, usize)> = Vec::with_capacity(captures.len());
    for (range, style) in captures {
        match groups.last_mut() {
            Some((last, kind))
                if *kind == style && styles[style].joined && is_blank(last.end..range.start) =>
            {
                last.end = range.end;
            }
            _ => groups.push((range, style)),
        }
    }

    groups
        .into_iter()
        .filter_map(|(range, style)| {
            let style = &styles[style];
            let hidden = range.start + style.keep..range.end;
            text[hidden.clone()].contains(&b'\n').then_some(FoldRange {
                range: hidden,
                placeholder: style.placeholder,
                collapsed_on_open: style.collapsed_on_open,
            })
        })
        .collect()
}

/// The indent for a new line after `offset`: the indent of the current line, one unit
/// deeper when a node of the indent query opens on this line and is still open.
pub fn indent_after(
    store: &RopeStore,
    tree: &Tree,
    query: &Query,
    offset: usize,
    unit: &str,
) -> String {
    let line = store.offset_to_line(offset);
    let line_start = store.line_to_offset(line);
    let text = store.slice(line_start..store.line_end(line));
    let indent = &text[..text.len() - text.trim_start().len()];
    let before_caret = offset.saturating_sub(line_start).min(text.len());
    let indent = &indent[..indent.len().min(before_caret)];

    let probe = offset.saturating_sub(1).max(line_start);
    let source = store.text();
    let mut cursor = QueryCursor::new();
    cursor.set_byte_range(probe..offset.max(probe + 1));
    let mut matches = cursor.matches(query, tree.root_node(), source.as_bytes());
    let mut opens_here = false;
    while let Some(found) = matches.next() {
        for capture in found.captures() {
            let node = capture.node;
            opens_here |= node.start_byte() >= line_start
                && node.start_byte() < offset
                && node.end_byte() >= offset;
        }
    }
    if opens_here {
        format!("{indent}{unit}")
    } else {
        indent.to_owned()
    }
}

/// Ctrl+W: the smallest node that strictly contains `range`.
pub fn enclosing_range(tree: &Tree, range: Range<usize>) -> Option<Range<usize>> {
    let root = tree.root_node();
    let mut node: Node<'_> = root.named_descendant_for_byte_range(range.start, range.end)?;
    loop {
        if node.start_byte() < range.start || node.end_byte() > range.end {
            return Some(node.byte_range());
        }
        node = node.parent()?;
    }
}

#[cfg(test)]
mod tests {
    use super::{custom_regions, region_folds};

    const TEXT: &str = "// header\n\
//region First\n\
one();\n\
// endregion\n\
// <editor-fold desc=\"Second\">\n\
two();\n\
// </editor-fold>\n\
//region Third: nested\n\
//region Inner\n\
three();\n\
//endregion\n\
//endregion\n\
end();\n";

    #[test]
    fn named_regions_are_found_with_their_names() {
        let regions = custom_regions(TEXT, Some("//"));
        let names: Vec<&str> = regions.iter().map(|region| region.name.as_str()).collect();
        assert_eq!(names, ["First", "Second", "Third: nested", "Inner"]);
        assert_eq!(regions[0].line, 1);
        assert_eq!(regions[1].line, 4);
        assert_eq!(regions[2].line, 7);
        assert_eq!(regions[3].line, 8);
    }

    #[test]
    fn the_fold_of_a_region_starts_at_the_end_of_its_header() {
        let folds = region_folds(TEXT, Some("//"));
        let first = TEXT.find("First").unwrap() + "First".len();
        assert_eq!(folds[0].range.start, first, "the name stays visible");
        assert!(TEXT[folds[0].range.clone()].starts_with('\n'));
        assert!(TEXT[folds[0].range.clone()].contains("one();"));
    }

    #[test]
    fn the_mark_is_read_only_after_the_comment_of_the_language() {
        assert!(custom_regions("//region A\nx();\n//endregion\n", Some("#")).is_empty());
        let regions = custom_regions("#region A\nx();\n#endregion\n", Some("#"));
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].name, "A");
    }

    #[test]
    fn an_unclosed_region_and_a_stray_end_are_ignored() {
        let regions = custom_regions(
            "//region A\nx();\n//endregion\n//endregion\n//region B\n",
            Some("//"),
        );
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].name, "A");
    }
}
