//! Quick documentation: the markdown answer of a language server as blocks and the
//! popup that shows them. The answer comes as it is, with markdown markers and the
//! HTML that documentation comments use; this module turns it into neutral blocks and
//! draws them in the colours of the editor.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures_util::StreamExt;
use lang_api::{HoverPlace, LanguageSupport, SourceSpan, TextPoint, TextSpan, ask};
use markdown::mdast;
use syntax::{DocumentId, HighlightSpan, LanguageConfig, SyntaxService};
use text_core::{Change, RopeStore, TextStore};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;

use crate::semantic::is_word;
use crate::style::EditorStyle;
use crate::view::EditorView;

/// The wait before the documentation opens under the pointer.
pub(crate) const HOVER_DELAY: Duration = Duration::from_millis(500);
/// How long the pointer may be off the name while it goes to the popup.
const HOVER_LEAVE_DELAY: Duration = Duration::from_millis(300);
/// The widest the popup gets.
const MAX_WIDTH: f32 = 600.;
const MIN_WIDTH: f32 = 320.;
/// The room the buttons of the popup take on its right, with a gap to the text.
const BUTTONS_ROOM: f32 = ICON_BUTTON * 2. + SPACE_XS * 2. + SPACE_S;

static NEXT_SOURCE: AtomicU64 = AtomicU64::new(1);

/// One block of an answer.
#[derive(Clone, Debug, PartialEq)]
pub enum DocBlock {
    Paragraph(Vec<DocInline>),
    Heading {
        level: usize,
        text: Vec<DocInline>,
    },
    List {
        ordered: bool,
        start: u32,
        items: Vec<Vec<DocInline>>,
        /// The blocks under each item after its line, such as a nested list: one entry per
        /// item. An answer of a server keeps them in the line, so its entries are empty.
        nested: Vec<Vec<DocBlock>>,
    },
    Code {
        language: Option<String>,
        text: String,
        /// The colours of the text, once the syntax service has read it.
        spans: Vec<HighlightSpan>,
    },
    /// A markdown table: the alignment of each column, the header and the body rows.
    Table {
        alignments: Vec<DocAlign>,
        header: Vec<Vec<DocInline>>,
        rows: Vec<Vec<Vec<DocInline>>>,
    },
    /// A picture alone in its paragraph: the text for the places that do not draw it
    /// and the address it comes from.
    Image {
        alt: String,
        url: String,
    },
    /// A quoted part of the answer with the blocks it holds.
    Quote(Vec<DocBlock>),
    /// A `<details>` block of a file: the summary line and the blocks under it. It is drawn
    /// closed unless its tag says `open`. `id` numbers the blocks of this kind in the order of
    /// the file, so that the choice of a reader stays with its block while the text changes.
    Details {
        id: usize,
        open: bool,
        summary: Vec<DocInline>,
        blocks: Vec<DocBlock>,
    },
    /// The place a declaration is in, such as the method of a local variable. It is a line of
    /// its own under the declaration and is drawn in a secondary colour.
    Place(String),
    Divider,
}

/// How the text of a table column sits in its cell.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DocAlign {
    #[default]
    Left,
    Center,
    Right,
}

/// A run of a line of an answer.
#[derive(Clone, Debug, PartialEq)]
pub enum DocInline {
    Text(String),
    Strong(Vec<DocInline>),
    Emphasis(Vec<DocInline>),
    Code(String),
    Link {
        text: Vec<DocInline>,
        url: String,
    },
    /// A picture among the words, drawn where the place that shows the answer can
    /// reach it and named by its text elsewhere.
    Image(DocImage),
}

/// A picture of a document: the text for the places that do not draw it, the address it
/// comes from and the size the document asks for.
#[derive(Clone, Debug, PartialEq)]
pub struct DocImage {
    pub alt: String,
    pub url: String,
    /// The width the document asks for, in pixels.
    pub width: Option<f32>,
    /// The height the document asks for, in pixels.
    pub height: Option<f32>,
}

/// A picture the place that shows the answer has already found: a file of the project,
/// bytes from the internet, or the reason there is nothing to draw.
#[derive(Clone)]
pub enum DocPicture {
    File(PathBuf),
    Bytes(Arc<Image>),
    /// The bytes are on their way.
    Loading,
    /// The file is not there, the address is not a picture, or the bytes did not come.
    Unavailable,
}

/// The quick documentation popup and what it belongs to.
pub(crate) struct QuickDoc {
    /// The offset the popup is anchored at.
    pub at: usize,
    pub blocks: Vec<DocBlock>,
    /// The text of the problem under the pointer, shown above the documentation.
    pub diagnostic: Option<String>,
    /// The name under the pointer, when the pointer opened the popup.
    pub hover_word: Option<Range<usize>>,
    pub scroll: ScrollHandle,
    /// The pointer is over the popup: it stays while it is.
    pub hovered: bool,
    /// The whole text the popup draws, in the order of a selection.
    pub text: String,
    /// The selected part of [`Self::text`], empty while nothing is selected.
    pub selection: Option<Range<usize>>,
    /// Where the selection started; a drag moves the other end.
    pub anchor: Option<usize>,
    /// The pointer holds the selection: the popup does not close on leaving it.
    pub selecting: bool,
    /// Tells a late syntax answer which popup it belongs to.
    pub serial: u64,
}

impl QuickDoc {
    pub(crate) fn new(
        at: usize,
        blocks: Vec<DocBlock>,
        diagnostic: Option<String>,
        hover_word: Option<Range<usize>>,
        serial: u64,
    ) -> Self {
        let text = display_text(&blocks, diagnostic.as_deref());
        Self {
            at,
            blocks,
            diagnostic,
            hover_word,
            scroll: ScrollHandle::new(),
            hovered: false,
            text,
            selection: None,
            anchor: None,
            selecting: false,
            serial,
        }
    }
}

/// Turns the answer of a server into blocks. Markdown gives the structure; the simple
/// HTML tags of documentation comments are read as markdown first, and other tags are
/// dropped while their text stays.
pub fn parse(text: &str) -> Vec<DocBlock> {
    parse_blocks(text, Reading::Answer).0
}

/// Turns the answer of a hover request into blocks. The language that answered splits the
/// place of the declaration off it, see [`split_place`].
pub fn parse_hover(text: &str, support: &dyn LanguageSupport) -> Vec<DocBlock> {
    split_place(parse(text), |line| support.hover_place(line))
}

/// Moves the place off the declaration that a single-line code block holds. The declaration
/// stays the code, so the grammar colours only the declaration, and the place becomes a line
/// of its own under it.
fn split_place(
    mut blocks: Vec<DocBlock>,
    split: impl Fn(&str) -> Option<HoverPlace>,
) -> Vec<DocBlock> {
    let (language, text) = match blocks.first() {
        Some(DocBlock::Code { language, text, .. }) => {
            (language.clone(), text.trim_end_matches('\n').to_owned())
        }
        _ => return blocks,
    };
    if text.contains('\n') {
        return blocks;
    }
    let Some(HoverPlace { declaration, place }) = split(&text) else {
        return blocks;
    };
    blocks[0] = DocBlock::Code {
        language,
        text: declaration,
        spans: Vec::new(),
    };
    blocks.insert(1, DocBlock::Place(format!("in {place}")));
    blocks
}

/// The blocks of a markdown file and the line each one starts on, counted from zero.
/// HTML is left as it is, so the lines stay the lines of the file: a preview follows
/// the scroll of the editor by them.
pub(crate) fn parse_with_lines(text: &str) -> (Vec<DocBlock>, Vec<usize>) {
    let (mut blocks, lines) = parse_blocks(text, Reading::File);
    number_details(&mut blocks, &mut 0);
    (blocks, lines)
}

/// How a text is read. An answer of a server keeps its line breaks, as the servers write
/// them, and its HTML becomes markdown first. A file reads the way CommonMark does: a line
/// break inside a paragraph is a space, and its HTML is read as it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reading {
    Answer,
    File,
}

fn parse_blocks(text: &str, reading: Reading) -> (Vec<DocBlock>, Vec<usize>) {
    let normalized = (reading == Reading::Answer).then(|| html_to_markdown(text));
    let source = normalized.as_deref().unwrap_or(text);
    let fallback = || {
        (
            vec![DocBlock::Paragraph(vec![DocInline::Text(
                text.trim().to_owned(),
            )])],
            vec![0],
        )
    };
    let Ok(mut root) = markdown::to_mdast(source, &markdown::ParseOptions::gfm()) else {
        return fallback();
    };
    if reading == Reading::File {
        resolve_references(&mut root);
    }
    let mdast::Node::Root(root) = root else {
        return (Vec::new(), Vec::new());
    };
    let mut blocks = Vec::new();
    let mut lines = Vec::new();
    push_blocks(&root.children, &mut blocks, &mut lines, reading);
    (blocks, lines)
}

fn push_blocks(
    nodes: &[mdast::Node],
    blocks: &mut Vec<DocBlock>,
    lines: &mut Vec<usize>,
    reading: Reading,
) {
    // The cursor is a node and a byte offset in its value: a `<details>` tag that closes in the
    // middle of an HTML block leaves the rest of that block to the blocks after it.
    let mut index = 0;
    let mut offset = 0;
    while let Some(node) = nodes.get(index) {
        let line = node
            .position()
            .map_or(0, |position| position.start.line.saturating_sub(1));
        let before = blocks.len();
        if let Some(read) = (reading == Reading::File)
            .then(|| details_at(nodes, index, offset))
            .flatten()
        {
            blocks.push(read.block);
            (index, offset) = read.resume;
            for _ in before..blocks.len() {
                lines.push(line);
            }
            continue;
        }
        match node {
            mdast::Node::Paragraph(paragraph) => {
                if let Some((alt, url)) = single_image(&paragraph.children) {
                    blocks.push(DocBlock::Image { alt, url });
                } else {
                    let inlines = inlines_of(&paragraph.children, reading);
                    if !inlines.is_empty() {
                        blocks.push(DocBlock::Paragraph(inlines));
                    }
                }
            }
            mdast::Node::Heading(heading) => {
                let text = inlines_of(&heading.children, reading);
                if !text.is_empty() {
                    blocks.push(DocBlock::Heading {
                        level: usize::from(heading.depth).clamp(1, 6),
                        text,
                    });
                }
            }
            mdast::Node::List(list) => {
                let mut items = Vec::new();
                let mut nested = Vec::new();
                for child in &list.children {
                    let mdast::Node::ListItem(item) = child else {
                        continue;
                    };
                    let (inlines, below) = list_item(item, reading);
                    if !inlines.is_empty() || !below.is_empty() {
                        items.push(inlines);
                        nested.push(below);
                    }
                }
                if !items.is_empty() {
                    blocks.push(DocBlock::List {
                        ordered: list.ordered,
                        start: list.start.unwrap_or(1),
                        items,
                        nested,
                    });
                }
            }
            mdast::Node::Code(code) => blocks.push(DocBlock::Code {
                language: code.lang.clone(),
                text: code.value.clone(),
                spans: Vec::new(),
            }),
            mdast::Node::Table(table) => {
                if let Some(block) = table_block(table, reading) {
                    blocks.push(block);
                }
            }
            mdast::Node::ThematicBreak(_) => blocks.push(DocBlock::Divider),
            mdast::Node::Blockquote(quote) => {
                let mut inner = Vec::new();
                let mut inner_lines = Vec::new();
                push_blocks(&quote.children, &mut inner, &mut inner_lines, reading);
                if !inner.is_empty() {
                    blocks.push(DocBlock::Quote(inner));
                }
            }
            mdast::Node::Html(html) => html_blocks(&html.value[offset..], reading, blocks),
            _ => {}
        }
        index += 1;
        offset = 0;
        for _ in before..blocks.len() {
            lines.push(line);
        }
    }
}

/// The blocks of a piece of HTML that is not a `<details>` block: a rule, a heading, or the
/// text of the piece as a paragraph.
fn html_blocks(html: &str, reading: Reading, blocks: &mut Vec<DocBlock>) {
    if is_rule(html) {
        blocks.push(DocBlock::Divider);
        return;
    }
    let (level, inner) =
        html_heading(html).map_or((None, html), |(level, inner)| (Some(level), inner));
    let inlines = trim_inlines(html_inlines(inner, reading));
    if !inlines.is_empty() {
        blocks.push(match level {
            Some(level) => DocBlock::Heading {
                level,
                text: inlines,
            },
            None => DocBlock::Paragraph(inlines),
        });
    }
}

/// A `<details>` block that starts at `offset` in the HTML node `nodes[index]`.
struct DetailsRead {
    block: DocBlock,
    /// Where the blocks after the block start: a node and a byte offset in its value.
    resume: (usize, usize),
}

/// The `<details>` block that opens at `offset` in the HTML node `nodes[index]`, `None` when
/// the text there does not open one. Its closing tag is the one that balances the opening
/// tags after it, so a block that holds other blocks closes at its own tag.
fn details_at(nodes: &[mdast::Node], index: usize, offset: usize) -> Option<DetailsRead> {
    let mdast::Node::Html(opener) = nodes.get(index)? else {
        return None;
    };
    let value = opener.value.as_str();
    let from = &value[offset..];
    let head = from.trim_start();
    let start = offset + from.len() - head.len();
    let after_name = head.strip_prefix("<details")?;
    if !after_name.starts_with(|character: char| character == '>' || character.is_whitespace()) {
        return None;
    }
    let open_end = start + "<details".len() + after_name.find('>')? + 1;
    let open = has_word(&value[start..open_end], "open");
    let closer = details_close(nodes, index, open_end);
    let closer_node = closer.map(|(node, _, _)| node);
    // The text of the opener after its tag, up to the closing tag when the tag is in it.
    let opener_end = match closer {
        Some((node, at, _)) if node == index => at,
        _ => value.len(),
    };
    let first = &value[open_end..opener_end];
    let mut body = Vec::new();
    let mut summary = None;
    let mut summary_node = None;
    let mut middle = index + 1;
    match split_summary(first) {
        Some((text, rest)) => {
            summary = Some(text);
            push_text(&mut body, &rest);
        }
        None => {
            // The summary may be the first line of the next HTML block.
            let next = match nodes.get(index + 1) {
                Some(mdast::Node::Html(next)) => Some(next.value.as_str()),
                _ => None,
            };
            let next_end = match closer {
                Some((node, at, _)) if node == index + 1 => at,
                _ => next.map_or(0, str::len),
            };
            let split = next
                .filter(|next| next.trim_start().starts_with("<summary"))
                .and_then(|next| split_summary(&next[..next_end]));
            match split {
                Some((text, rest)) => {
                    summary = Some(text);
                    push_text(&mut body, &rest);
                    summary_node = Some(index + 1);
                    middle = index + 2;
                }
                None => push_text(&mut body, first),
            }
        }
    }
    let middle_end = closer_node.unwrap_or(nodes.len());
    if middle < middle_end {
        push_blocks(
            &nodes[middle..middle_end],
            &mut body,
            &mut Vec::new(),
            Reading::File,
        );
    }
    if let Some((node, at, _)) = closer
        && node > index
        && Some(node) != summary_node
        && let mdast::Node::Html(html) = &nodes[node]
    {
        push_text(&mut body, &html.value[..at]);
    }
    let mut summary = trim_inlines(html_inlines(summary.unwrap_or_default(), Reading::File));
    if summary.is_empty() {
        summary = vec![DocInline::Text("Details".to_owned())];
    }
    Some(DetailsRead {
        block: DocBlock::Details {
            id: 0,
            open,
            summary,
            blocks: body,
        },
        resume: closer.map_or((nodes.len(), 0), |(node, _, end)| (node, end)),
    })
}

/// The closing tag of a `<details>` block whose opening tag ends at `from` in the node
/// `nodes[index]`: the node, the byte offsets of the tag and of its end. `None` when no tag
/// closes the block.
fn details_close(
    nodes: &[mdast::Node],
    index: usize,
    from: usize,
) -> Option<(usize, usize, usize)> {
    let mut depth = 0usize;
    for (node, found) in nodes.iter().enumerate().skip(index) {
        let mdast::Node::Html(html) = found else {
            continue;
        };
        let from = if node == index { from } else { 0 };
        for (at, opens) in details_tags(&html.value, from) {
            if opens {
                depth += 1;
            } else if depth > 0 {
                depth -= 1;
            } else {
                let end = at + html.value[at..].find('>')? + 1;
                return Some((node, at, end));
            }
        }
    }
    None
}

/// The tags of `<details>` and `</details>` in a piece of HTML from `from` on, in the order
/// they come: the byte offset of each and whether it opens a block.
fn details_tags(html: &str, from: usize) -> Vec<(usize, bool)> {
    let mut tags: Vec<(usize, bool)> = html[from..]
        .match_indices("<details")
        .map(|(at, _)| (from + at, true))
        .chain(
            html[from..]
                .match_indices("</details")
                .map(|(at, _)| (from + at, false)),
        )
        .collect();
    tags.sort_unstable();
    tags
}

/// The text inside the `<summary>` tag of a piece of HTML, and the text around the tag.
fn split_summary(text: &str) -> Option<(&str, String)> {
    let start = text.find("<summary")?;
    let inner_start = start + text[start..].find('>')? + 1;
    let inner_end = inner_start + text[inner_start..].find("</summary>")?;
    let rest = format!(
        "{}{}",
        &text[..start],
        &text[inner_end + "</summary>".len()..]
    );
    Some((&text[inner_start..inner_end], rest))
}

/// Whether a tag has an attribute that is written without a value, such as `open`.
fn has_word(tag: &str, word: &str) -> bool {
    tag.trim_end_matches('>')
        .split_whitespace()
        .skip(1)
        .any(|part| part.eq_ignore_ascii_case(word))
}

/// Adds the blocks of a fragment of markdown, such as the body of a `<details>` block that
/// is written on the line of its tag.
fn push_text(out: &mut Vec<DocBlock>, text: &str) {
    if !text.trim().is_empty() {
        out.extend(parse_blocks(text, Reading::File).0);
    }
}

/// Gives each `<details>` block of a file its number, in the order the blocks open in the
/// file, nested ones after the block they are in.
fn number_details(blocks: &mut [DocBlock], next: &mut usize) {
    for block in blocks {
        match block {
            DocBlock::Details { id, blocks, .. } => {
                *id = *next;
                *next += 1;
                number_details(blocks, next);
            }
            DocBlock::Quote(inner) => number_details(inner, next),
            DocBlock::List { nested, .. } => {
                for blocks in nested {
                    number_details(blocks, next);
                }
            }
            _ => {}
        }
    }
}

/// Puts the address of each reference of a file in its place. A link or a picture that names
/// a label takes the address of the definition of that label, which may be anywhere in the
/// file. The definitions are not drawn: they are not blocks of the text.
fn resolve_references(root: &mut mdast::Node) {
    let mut definitions = HashMap::new();
    collect_definitions(root, &mut definitions);
    if !definitions.is_empty() {
        rewrite_references(root, &definitions);
    }
}

/// The address of each label of the file, by the normalized label. The first definition of a
/// label is the one a reference takes, as in CommonMark.
fn collect_definitions(node: &mdast::Node, out: &mut HashMap<String, String>) {
    if let mdast::Node::Definition(definition) = node {
        out.entry(definition.identifier.clone())
            .or_insert_with(|| definition.url.clone());
    }
    for child in node.children().into_iter().flatten() {
        collect_definitions(child, out);
    }
}

fn rewrite_references(node: &mut mdast::Node, definitions: &HashMap<String, String>) {
    let resolved = match node {
        mdast::Node::LinkReference(reference) => {
            definitions.get(&reference.identifier).map(|url| {
                mdast::Node::Link(mdast::Link {
                    children: std::mem::take(&mut reference.children),
                    position: reference.position.take(),
                    url: url.clone(),
                    title: None,
                })
            })
        }
        mdast::Node::ImageReference(reference) => {
            definitions.get(&reference.identifier).map(|url| {
                mdast::Node::Image(mdast::Image {
                    position: reference.position.take(),
                    alt: reference.alt.clone(),
                    url: url.clone(),
                    title: None,
                })
            })
        }
        _ => None,
    };
    if let Some(resolved) = resolved {
        *node = resolved;
    }
    for child in node.children_mut().into_iter().flatten() {
        rewrite_references(child, definitions);
    }
}

/// Whether an HTML block is a horizontal rule and nothing else, such as `<hr>`.
fn is_rule(html: &str) -> bool {
    let html = html.trim();
    html.starts_with("<hr") && html.ends_with('>')
}

/// The level and the text of an HTML block that is one heading, such as
/// `<h1 align="center">Title</h1>`.
fn html_heading(html: &str) -> Option<(usize, &str)> {
    let html = html.trim();
    let level = html
        .strip_prefix('<')?
        .strip_prefix('h')?
        .chars()
        .next()?
        .to_digit(10)?;
    if !(1..=6).contains(&level) {
        return None;
    }
    let inner = html[html.find('>')? + 1..].strip_suffix(format!("</h{level}>").as_str())?;
    Some((level as usize, inner))
}

/// The text and the address of the only picture of a paragraph.
fn single_image(nodes: &[mdast::Node]) -> Option<(String, String)> {
    let [mdast::Node::Image(image)] = nodes else {
        return None;
    };
    Some((image.alt.clone(), image.url.clone()))
}

/// A table of the answer: the first row is the header, the alignment of a column comes
/// from the separators under it.
fn table_block(table: &mdast::Table, reading: Reading) -> Option<DocBlock> {
    let mut rows = Vec::new();
    for child in &table.children {
        let mdast::Node::TableRow(row) = child else {
            continue;
        };
        let mut cells = Vec::new();
        for cell in &row.children {
            let mdast::Node::TableCell(cell) = cell else {
                continue;
            };
            cells.push(inlines_of(&cell.children, reading));
        }
        rows.push(cells);
    }
    if rows.is_empty() {
        return None;
    }
    let header = rows.remove(0);
    let alignments = table
        .align
        .iter()
        .map(|align| match align {
            mdast::AlignKind::Center => DocAlign::Center,
            mdast::AlignKind::Right => DocAlign::Right,
            _ => DocAlign::Left,
        })
        .collect();
    Some(DocBlock::Table {
        alignments,
        header,
        rows,
    })
}

/// The line of a list item and the blocks under it. A file keeps what is under the line
/// as blocks, such as a nested list or a second paragraph; an answer keeps all of it in
/// the line, as it always has.
fn list_item(item: &mdast::ListItem, reading: Reading) -> (Vec<DocInline>, Vec<DocBlock>) {
    if reading == Reading::Answer {
        let mut inlines = Vec::new();
        collect_item(item, &mut inlines);
        return (inlines, Vec::new());
    }
    let mut inlines = Vec::new();
    if let Some(checked) = item.checked {
        let mark = if checked { "\u{2611} " } else { "\u{2610} " };
        merge(&mut inlines, vec![DocInline::Text(mark.to_owned())]);
    }
    // The first paragraph is the line of the item. The blocks after it are read together, so
    // that a `<details>` block can close in a later node of the item.
    let mut rest = item.children.as_slice();
    if let Some(mdast::Node::Paragraph(paragraph)) = rest.first() {
        merge(&mut inlines, inlines_of(&paragraph.children, reading));
        rest = &rest[1..];
    }
    let mut below = Vec::new();
    push_blocks(rest, &mut below, &mut Vec::new(), reading);
    (inlines, below)
}

/// A list item of an answer: its text in one line, the nested items included. A task item
/// starts with its mark.
fn collect_item(item: &mdast::ListItem, inlines: &mut Vec<DocInline>) {
    if let Some(checked) = item.checked {
        let mark = if checked { "\u{2611} " } else { "\u{2610} " };
        merge(inlines, vec![DocInline::Text(mark.to_owned())]);
    }
    for node in &item.children {
        match node {
            mdast::Node::Paragraph(paragraph) => {
                merge(inlines, inlines_of(&paragraph.children, Reading::Answer));
            }
            mdast::Node::List(list) => {
                for child in &list.children {
                    if let mdast::Node::ListItem(item) = child {
                        collect_item(item, inlines);
                    }
                }
            }
            other => {
                let text = plain_node(other);
                if !text.is_empty() {
                    merge(inlines, vec![DocInline::Text(text)]);
                }
            }
        }
    }
}

fn inlines_of(nodes: &[mdast::Node], reading: Reading) -> Vec<DocInline> {
    let mut inlines = Vec::new();
    for node in nodes {
        push_inline(node, &mut inlines, reading);
    }
    inlines
}

/// The text of a run as the reading shows it. A file's line break that is not a hard one
/// is a space, so a paragraph runs on to the width of its place.
fn text_run(value: &str, reading: Reading) -> String {
    match reading {
        Reading::Answer => value.to_owned(),
        Reading::File => value.replace("\r\n", " ").replace(['\r', '\n'], " "),
    }
}

fn push_inline(node: &mdast::Node, out: &mut Vec<DocInline>, reading: Reading) {
    match node {
        mdast::Node::Text(text) => {
            merge(out, vec![DocInline::Text(text_run(&text.value, reading))]);
        }
        mdast::Node::Strong(strong) => merge(
            out,
            vec![DocInline::Strong(inlines_of(&strong.children, reading))],
        ),
        mdast::Node::Emphasis(emphasis) => merge(
            out,
            vec![DocInline::Emphasis(inlines_of(&emphasis.children, reading))],
        ),
        mdast::Node::Delete(delete) => merge(
            out,
            vec![DocInline::Emphasis(inlines_of(&delete.children, reading))],
        ),
        mdast::Node::InlineCode(code) => merge(out, vec![DocInline::Code(code.value.clone())]),
        mdast::Node::Link(link) => merge(
            out,
            vec![DocInline::Link {
                text: inlines_of(&link.children, reading),
                url: link.url.clone(),
            }],
        ),
        mdast::Node::LinkReference(link) => merge(
            out,
            vec![DocInline::Link {
                text: inlines_of(&link.children, reading),
                url: String::new(),
            }],
        ),
        // A picture by reference shows its text: the address is not in the line.
        mdast::Node::ImageReference(image) => {
            merge(out, vec![DocInline::Text(image.alt.clone())]);
        }
        mdast::Node::Image(image) => {
            merge(
                out,
                vec![DocInline::Image(DocImage {
                    alt: image.alt.clone(),
                    url: image.url.clone(),
                    width: None,
                    height: None,
                })],
            );
        }
        mdast::Node::Break(_) => merge(out, vec![DocInline::Text("\n".to_owned())]),
        mdast::Node::Html(html) => merge(out, html_inlines(&html.value, reading)),
        other => {
            let text = plain_node(other);
            if !text.is_empty() {
                merge(out, vec![DocInline::Text(text)]);
            }
        }
    }
}

/// The text of a node this walker does not know, so that nothing disappears.
fn plain_node(node: &mdast::Node) -> String {
    match node {
        mdast::Node::Text(text) => text.value.clone(),
        mdast::Node::InlineCode(code) => code.value.clone(),
        mdast::Node::Code(code) => code.value.clone(),
        _ => String::new(),
    }
}

/// Joins neighbouring plain runs, so that a paragraph has as few pieces as possible.
fn merge(out: &mut Vec<DocInline>, more: Vec<DocInline>) {
    for inline in more {
        match (out.last_mut(), &inline) {
            (Some(DocInline::Text(known)), DocInline::Text(more)) => known.push_str(more),
            _ => out.push(inline),
        }
    }
}

/// Reads `text` with the grammar of `language` on the syntax thread and returns the colours
/// of its parts, `None` when the grammar gives no answer.
pub(crate) async fn highlight_source(
    syntax: SyntaxService,
    language: LanguageConfig,
    text: String,
) -> Option<Vec<HighlightSpan>> {
    let id = DocumentId(NEXT_SOURCE.fetch_add(1, Ordering::Relaxed));
    let (sender, mut receiver) = futures_channel::mpsc::unbounded();
    syntax.open(id, language, move |snapshot| {
        sender.unbounded_send(snapshot).ok();
    });
    syntax.update(id, 1, RopeStore::from_text(&text), &Change::default());
    let snapshot = receiver.next().await;
    syntax.close(id);
    snapshot.map(|snapshot| snapshot.highlights)
}

/// The plain text of the answer, for the copy button and for tests.
pub(crate) fn plain_text(doc: &QuickDoc) -> String {
    let mut out = String::new();
    if let Some(diagnostic) = &doc.diagnostic {
        out.push_str(diagnostic);
        out.push_str("\n\n");
    }
    for block in &doc.blocks {
        plain_block(&mut out, block);
    }
    out.trim().to_owned()
}

/// The text the popup draws, from its first character to its last. A selection is a range
/// of this text, so the renderer and the copy of a part walk it the same way.
pub fn display_text(blocks: &[DocBlock], diagnostic: Option<&str>) -> String {
    let mut out = String::new();
    if let Some(diagnostic) = diagnostic {
        out.push_str(diagnostic);
        out.push_str("\n\n");
    }
    display_blocks(&mut out, blocks, 0);
    out
}

/// The text of the blocks, `depth` lists deep: the mark of an item depends on its list.
fn display_blocks(out: &mut String, blocks: &[DocBlock], depth: usize) {
    for block in blocks {
        match block {
            DocBlock::Paragraph(inlines) => {
                out.push_str(&plain_inlines(inlines));
                out.push('\n');
            }
            DocBlock::Heading { text, .. } => {
                out.push_str(&plain_inlines(text));
                out.push('\n');
            }
            DocBlock::List {
                ordered,
                start,
                items,
                nested,
            } => {
                for (index, item) in items.iter().enumerate() {
                    out.push_str(&bullet_of(*ordered, *start, index, depth));
                    out.push(' ');
                    out.push_str(&plain_inlines(item));
                    out.push('\n');
                    for block in nested.get(index).into_iter().flatten() {
                        display_blocks(out, std::slice::from_ref(block), depth + 1);
                    }
                }
            }
            DocBlock::Code { text, .. } => {
                out.push_str(text);
                if !text.ends_with('\n') {
                    out.push('\n');
                }
            }
            DocBlock::Table {
                alignments,
                header,
                rows,
            } => {
                let columns = table_columns(alignments, header, rows);
                for cells in std::iter::once(header).chain(rows) {
                    for column in 0..columns {
                        if let Some(cell) = cells.get(column) {
                            out.push_str(&plain_inlines(cell));
                        }
                        if column + 1 < columns {
                            out.push_str(" | ");
                        }
                    }
                    out.push('\n');
                }
            }
            DocBlock::Image { alt, .. } => {
                out.push_str(alt);
                out.push('\n');
            }
            DocBlock::Details {
                summary, blocks, ..
            } => {
                out.push_str(&plain_inlines(summary));
                out.push('\n');
                display_blocks(out, blocks, depth);
            }
            DocBlock::Quote(inner) => display_blocks(out, inner, depth),
            DocBlock::Place(text) => {
                out.push_str(text);
                out.push('\n');
            }
            DocBlock::Divider => out.push('\n'),
        }
    }
}

/// The block as the copy button writes it: a list has one `- ` per item, whatever the
/// numbers the popup shows.
fn plain_block(out: &mut String, block: &DocBlock) {
    match block {
        DocBlock::Paragraph(inlines) => {
            out.push_str(&plain_inlines(inlines));
            out.push('\n');
        }
        DocBlock::Heading { text, .. } => {
            out.push_str(&plain_inlines(text));
            out.push('\n');
        }
        DocBlock::List { items, nested, .. } => {
            for (index, item) in items.iter().enumerate() {
                out.push_str("- ");
                out.push_str(&plain_inlines(item));
                out.push('\n');
                for block in nested.get(index).into_iter().flatten() {
                    plain_block(out, block);
                }
            }
        }
        DocBlock::Code { text, .. } => {
            out.push_str(text);
            if !text.ends_with('\n') {
                out.push('\n');
            }
        }
        DocBlock::Table {
            alignments,
            header,
            rows,
        } => {
            let columns = table_columns(alignments, header, rows);
            for cells in std::iter::once(header).chain(rows) {
                for column in 0..columns {
                    if let Some(cell) = cells.get(column) {
                        out.push_str(&plain_inlines(cell));
                    }
                    if column + 1 < columns {
                        out.push_str(" | ");
                    }
                }
                out.push('\n');
            }
        }
        DocBlock::Image { alt, .. } => {
            out.push_str(alt);
            out.push('\n');
        }
        DocBlock::Quote(inner) => {
            for block in inner {
                plain_block(out, block);
            }
        }
        DocBlock::Details {
            summary, blocks, ..
        } => {
            out.push_str(&plain_inlines(summary));
            out.push('\n');
            for block in blocks {
                plain_block(out, block);
            }
        }
        DocBlock::Place(text) => {
            out.push_str(text);
            out.push('\n');
        }
        DocBlock::Divider => out.push('\n'),
    }
}

/// The mark of an item of a list: its number in an ordered list, and in a bulleted one a dot
/// that changes with the depth of the list, as the IDEA preview draws them.
fn bullet_of(ordered: bool, start: u32, index: usize, depth: usize) -> String {
    if ordered {
        format!("{}.", start + index as u32)
    } else {
        ["\u{2022}", "\u{25e6}", "\u{25aa}"][depth % 3].to_owned()
    }
}

fn table_columns(
    alignments: &[DocAlign],
    header: &[Vec<DocInline>],
    rows: &[Vec<Vec<DocInline>>],
) -> usize {
    rows.iter()
        .map(Vec::len)
        .chain(std::iter::once(header.len()))
        .chain(std::iter::once(alignments.len()))
        .max()
        .unwrap_or(0)
}

/// The plain text of a run, for the block walkers and for the preview of a file.
pub(crate) fn plain_inlines(inlines: &[DocInline]) -> String {
    let mut out = String::new();
    for inline in inlines {
        match inline {
            DocInline::Text(text) | DocInline::Code(text) => out.push_str(text),
            DocInline::Strong(children) | DocInline::Emphasis(children) => {
                out.push_str(&plain_inlines(children));
            }
            DocInline::Link { text, .. } => out.push_str(&plain_inlines(text)),
            DocInline::Image(image) => {
                out.push_str(if image.alt.is_empty() {
                    &image.url
                } else {
                    &image.alt
                });
            }
        }
    }
    out
}

/// A piece of HTML as the runs of a line: the pictures it holds in place, the text around
/// them, and the emphasis and the links of its simple tags. A comment is not drawn, and a
/// line break tag starts a new line. A tag that is not known is dropped and its text stays.
fn html_inlines(html: &str, reading: Reading) -> Vec<DocInline> {
    let mut frames = vec![HtmlFrame::default()];
    let mut at = 0;
    while let Some(found) = html[at..].find('<') {
        let start = at + found;
        push_html_text(&mut frames, &html[at..start], reading);
        if html[start..].starts_with("<!--") {
            // A comment ends at its own mark, whatever tags it names inside.
            at = html[start..]
                .find("-->")
                .map_or(html.len(), |end| start + end + 3);
            continue;
        }
        let Some(close) = html[start..].find('>') else {
            at = html.len();
            break;
        };
        at = start + close + 1;
        apply_html_tag(&mut frames, &html[start + 1..at - 1]);
    }
    push_html_text(&mut frames, &html[at..], reading);
    while frames.len() > 1 {
        close_html_frame(&mut frames);
    }
    frames.pop().map(|root| root.runs).unwrap_or_default()
}

/// A tag of a piece of HTML that is open, with the runs it holds so far.
#[derive(Default)]
struct HtmlFrame {
    name: String,
    /// The address of an anchor.
    href: Option<String>,
    runs: Vec<DocInline>,
}

fn top_frame(frames: &mut [HtmlFrame]) -> &mut HtmlFrame {
    frames.last_mut().expect("the root frame is never closed")
}

fn push_html_text(frames: &mut [HtmlFrame], text: &str, reading: Reading) {
    if !text.is_empty() {
        let run = DocInline::Text(html_text(text, reading));
        merge(&mut top_frame(frames).runs, vec![run]);
    }
}

/// The text between two HTML tags as the reading shows it. A file decodes the names of the
/// characters that HTML writes most, since the markdown parser leaves HTML as it is.
fn html_text(text: &str, reading: Reading) -> String {
    match reading {
        Reading::Answer => text.to_owned(),
        Reading::File => text_run(text, reading)
            .replace("&nbsp;", " ")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&#39;", "'")
            .replace("&amp;", "&"),
    }
}

/// Applies one tag of a piece of HTML: a picture or a line break is added to the runs,
/// and an anchor, an emphasis or a strong tag opens a run that its end tag closes.
fn apply_html_tag(frames: &mut Vec<HtmlFrame>, tag: &str) {
    let name = tag_name(tag);
    let opens_run = matches!(name.as_str(), "a" | "b" | "strong" | "i" | "em" | "summary");
    if tag.trim_start().starts_with('/') {
        if opens_run {
            close_html_tag(frames, &name);
        }
    } else if opens_run {
        let href = if name == "a" {
            attribute(tag, "href")
        } else {
            None
        };
        frames.push(HtmlFrame {
            name,
            href,
            runs: Vec::new(),
        });
    } else if name == "img" {
        if let Some(image) = html_image(tag) {
            merge(&mut top_frame(frames).runs, vec![DocInline::Image(image)]);
        }
    } else if name == "br" {
        merge(
            &mut top_frame(frames).runs,
            vec![DocInline::Text("\n".to_owned())],
        );
    }
}

/// Closes the innermost open tag of this name, and every tag opened after it, so that a tag
/// which is never closed does not take the rest of the line with it.
fn close_html_tag(frames: &mut Vec<HtmlFrame>, name: &str) {
    let Some(index) = (1..frames.len())
        .rev()
        .find(|&index| frames[index].name == name)
    else {
        return;
    };
    while frames.len() > index {
        close_html_frame(frames);
    }
}

/// Closes the innermost open tag: its runs become a link, an emphasis or a strong run in
/// the tag around it. An anchor without an address keeps its words only.
fn close_html_frame(frames: &mut Vec<HtmlFrame>) {
    let Some(HtmlFrame { name, href, runs }) = frames.pop() else {
        return;
    };
    let pieces = match (name.as_str(), href) {
        ("a", Some(url)) => vec![DocInline::Link { text: runs, url }],
        ("a", None) => runs,
        ("i" | "em", _) => vec![DocInline::Emphasis(runs)],
        _ => vec![DocInline::Strong(runs)],
    };
    let parent = top_frame(frames);
    for piece in pieces {
        merge(&mut parent.runs, vec![piece]);
    }
}

/// Drops the spaces at the two ends of a run of HTML the markdown parser kept whole.
fn trim_inlines(mut inlines: Vec<DocInline>) -> Vec<DocInline> {
    if let Some(DocInline::Text(first)) = inlines.first_mut() {
        *first = first.trim_start().to_owned();
    }
    if let Some(DocInline::Text(last)) = inlines.last_mut() {
        *last = last.trim_end().to_owned();
    }
    inlines.retain(|inline| !matches!(inline, DocInline::Text(text) if text.is_empty()));
    inlines
}

/// The name of a tag, lowercased, without the slash of a closing one.
fn tag_name(tag: &str) -> String {
    tag.trim_start_matches('/')
        .trim_start()
        .chars()
        .take_while(|character| character.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase()
}

/// The picture of an `<img>` tag: its address, its text for the places that do not draw
/// it and the size it asks for.
fn html_image(tag: &str) -> Option<DocImage> {
    let url = attribute(tag, "src")?;
    if url.trim().is_empty() {
        return None;
    }
    Some(DocImage {
        alt: attribute(tag, "alt").unwrap_or_default(),
        url,
        width: size_attribute(tag, "width"),
        height: size_attribute(tag, "height"),
    })
}

/// The size of one attribute in pixels: `width="1042"`, `width="1042px"` and
/// `width="1042.5"` all count.
fn size_attribute(body: &str, wanted: &str) -> Option<f32> {
    let value = attribute(body, wanted)?;
    let value = value.trim().trim_end_matches("px").trim();
    value.parse::<f32>().ok().filter(|size| *size > 0.)
}

/// The simple tags a documentation comment uses, as markdown. Tags are only read
/// outside code, so that `<T>` of a signature stays.
fn html_to_markdown(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 16);
    let mut at = 0;
    let mut line_start = true;
    let mut fenced = false;
    let mut link: Option<String> = None;
    while at < text.len() {
        let rest = &text[at..];
        if line_start {
            let end = rest.find('\n').map_or(text.len(), |index| at + index);
            let line = text[at..end].trim_start();
            if line.starts_with("```") || line.starts_with("~~~") {
                fenced = !fenced;
            }
            line_start = false;
        }
        let Some(character) = rest.chars().next() else {
            break;
        };
        if character == '\n' {
            out.push('\n');
            at += 1;
            line_start = true;
            continue;
        }
        if fenced {
            out.push(character);
            at += character.len_utf8();
            continue;
        }
        if character == '`' {
            let run = rest.chars().take_while(|c| *c == '`').count();
            let fence = &rest[..run];
            out.push_str(fence);
            at += run;
            if let Some(found) = text[at..].find(fence) {
                let end = at + found + run;
                out.push_str(&text[at..end]);
                at = end;
            }
            continue;
        }
        if character == '<'
            && let Some(end) = rest.find('>')
        {
            let inner = &rest[1..end];
            if let Some(replacement) = html_tag(inner, &mut link) {
                out.push_str(&replacement);
                at += end + 1;
                continue;
            }
        }
        out.push(character);
        at += character.len_utf8();
    }
    out
}

/// The markdown for one simple tag. `None` leaves an unknown tag out or keeps it as
/// text, which the caller decides by the returned value.
fn html_tag(inner: &str, link: &mut Option<String>) -> Option<String> {
    let inner = inner.trim();
    if inner.is_empty() || inner.starts_with('!') {
        return None;
    }
    let closing = inner.starts_with('/');
    let body = inner.trim_start_matches('/').trim_start();
    let name: String = body
        .chars()
        .take_while(|character| character.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase();
    if name.is_empty() || !known_tag(&name) {
        return None;
    }
    let replacement = match name.as_str() {
        "p" | "div" | "ul" | "ol" | "dl" | "table" | "tbody" | "thead" | "tr" => "\n\n".to_owned(),
        "br" => "\n".to_owned(),
        "li" => {
            if closing {
                "\n".to_owned()
            } else {
                "\n- ".to_owned()
            }
        }
        "dt" => {
            if closing {
                "**\n".to_owned()
            } else {
                "\n**".to_owned()
            }
        }
        "dd" => "\n".to_owned(),
        "b" | "strong" => "**".to_owned(),
        "i" | "em" => "*".to_owned(),
        "code" => "`".to_owned(),
        "pre" => "\n```\n".to_owned(),
        "hr" => "\n\n---\n\n".to_owned(),
        "blockquote" => "\n\n> ".to_owned(),
        "h1" => heading_markdown(closing, 1),
        "h2" => heading_markdown(closing, 2),
        "h3" => heading_markdown(closing, 3),
        "h4" => heading_markdown(closing, 4),
        "h5" => heading_markdown(closing, 5),
        "h6" => heading_markdown(closing, 6),
        "td" | "th" => {
            if closing {
                " | ".to_owned()
            } else {
                String::new()
            }
        }
        "a" => {
            if closing {
                match link.take() {
                    Some(url) if !url.is_empty() => format!("]({url})"),
                    _ => "]".to_owned(),
                }
            } else {
                if let Some(url) = attribute(body, "href") {
                    *link = Some(url);
                }
                "[".to_owned()
            }
        }
        "img" => attribute(body, "alt").unwrap_or_default(),
        _ => String::new(),
    };
    Some(replacement)
}

fn known_tag(name: &str) -> bool {
    matches!(
        name,
        "p" | "div"
            | "br"
            | "ul"
            | "ol"
            | "li"
            | "dl"
            | "dt"
            | "dd"
            | "table"
            | "tbody"
            | "thead"
            | "tr"
            | "td"
            | "th"
            | "b"
            | "strong"
            | "i"
            | "em"
            | "code"
            | "pre"
            | "hr"
            | "blockquote"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "a"
            | "img"
            | "span"
            | "sup"
            | "sub"
            | "s"
            | "strike"
            | "del"
            | "ins"
            | "u"
            | "font"
            | "center"
            | "html"
            | "body"
            | "head"
            | "meta"
            | "link"
            | "style"
            | "script"
    )
}

fn heading_markdown(closing: bool, level: usize) -> String {
    if closing {
        "\n\n".to_owned()
    } else {
        format!("\n\n{} ", "#".repeat(level))
    }
}

/// The value of one attribute of a tag: `href="/a"` and `href='/a'` both work.
fn attribute(body: &str, wanted: &str) -> Option<String> {
    let mut rest = body;
    while let Some(index) = rest.find(wanted) {
        let after = &rest[index + wanted.len()..];
        let after = after.trim_start();
        if let Some(after) = after.strip_prefix('=') {
            let after = after.trim_start();
            let quote = after.chars().next()?;
            if quote == '"' || quote == '\'' {
                let value = &after[1..];
                let end = value.find(quote)?;
                return Some(value[..end].to_owned());
            }
        }
        rest = &rest[index + wanted.len()..];
    }
    None
}

/// One piece of a rendered line, already split by the emphasis around it: a run of
/// words or a picture.
struct Piece {
    text: String,
    bold: bool,
    italic: bool,
    code: bool,
    url: Option<String>,
    link: Option<usize>,
    /// The picture this piece draws instead of its words.
    image: Option<DocImage>,
}

impl Piece {
    fn words(text: String, style: InlineStyle<'_>, code: bool) -> Self {
        Self {
            text,
            bold: style.bold,
            italic: style.italic,
            code,
            url: style.url.map(str::to_owned),
            link: style.link,
            image: None,
        }
    }
}

#[derive(Clone, Copy)]
struct InlineStyle<'a> {
    bold: bool,
    italic: bool,
    code: bool,
    url: Option<&'a str>,
    link: Option<usize>,
}

fn flatten_inline(
    inline: &DocInline,
    style: InlineStyle<'_>,
    next_link: &mut usize,
    out: &mut Vec<Piece>,
) {
    match inline {
        DocInline::Text(text) => out.push(Piece::words(text.clone(), style, style.code)),
        DocInline::Code(text) => out.push(Piece::words(text.clone(), style, true)),
        DocInline::Strong(children) => {
            let style = InlineStyle {
                bold: true,
                ..style
            };
            for child in children {
                flatten_inline(child, style, next_link, out);
            }
        }
        DocInline::Emphasis(children) => {
            let style = InlineStyle {
                italic: true,
                ..style
            };
            for child in children {
                flatten_inline(child, style, next_link, out);
            }
        }
        DocInline::Link { text, url } => {
            let id = *next_link;
            *next_link += 1;
            let style = InlineStyle {
                url: Some(url),
                link: Some(id),
                ..style
            };
            for child in text {
                flatten_inline(child, style, next_link, out);
            }
        }
        DocInline::Image(image) => out.push(Piece {
            text: if image.alt.is_empty() {
                image.url.clone()
            } else {
                image.alt.clone()
            },
            bold: style.bold,
            italic: style.italic,
            code: false,
            url: style.url.map(str::to_owned),
            link: style.link,
            image: Some(image.clone()),
        }),
    }
}

/// Splits a run into words with their spaces, so that a wrapping row breaks between
/// words rather than between the runs of the markdown.
fn word_pieces(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut word = String::new();
    for character in text.chars() {
        if character.is_whitespace() {
            if word.is_empty() {
                match out.last_mut() {
                    Some(last) if !last.ends_with(' ') => last.push(' '),
                    Some(_) => {}
                    None => out.push(" ".to_owned()),
                }
            } else {
                word.push(' ');
                out.push(std::mem::take(&mut word));
            }
        } else {
            word.push(character);
        }
    }
    if !word.is_empty() {
        out.push(word);
    }
    out
}

impl EditorView {
    /// Opens the popup for an answer. The syntax service colours the code blocks later.
    pub(crate) fn show_quick_doc(
        &mut self,
        at: usize,
        blocks: Vec<DocBlock>,
        diagnostic: Option<String>,
        hover_word: Option<Range<usize>>,
        cx: &mut Context<Self>,
    ) {
        let Some(lsp) = &mut self.lsp else {
            return;
        };
        self.quick_serial += 1;
        let serial = self.quick_serial;
        lsp.note = None;
        lsp.quick = Some(QuickDoc::new(at, blocks, diagnostic, hover_word, serial));
        cx.notify();
        self.highlight_quick_doc(cx);
    }

    /// Asks the syntax service for the colours of every code block of the open popup.
    fn highlight_quick_doc(&mut self, cx: &mut Context<Self>) {
        let Some(language) = self
            .state
            .as_ref()
            .and_then(|state| state.language().cloned())
        else {
            return;
        };
        let Some((serial, code)) =
            self.lsp
                .as_ref()
                .and_then(|lsp| lsp.quick.as_ref())
                .map(|doc| {
                    let code = doc
                        .blocks
                        .iter()
                        .enumerate()
                        .filter_map(|(index, block)| match block {
                            DocBlock::Code { text, .. } => Some((index, text.clone())),
                            _ => None,
                        })
                        .collect::<Vec<_>>();
                    (doc.serial, code)
                })
        else {
            return;
        };
        if code.is_empty() {
            return;
        }
        let syntax = self.syntax.clone();
        cx.spawn(async move |this, cx| {
            for (index, text) in code {
                let Some(highlights) =
                    highlight_source(syntax.clone(), language.clone(), text.clone()).await
                else {
                    continue;
                };
                this.update(cx, |this, cx| {
                    let Some(doc) = this.lsp.as_mut().and_then(|lsp| lsp.quick.as_mut()) else {
                        return;
                    };
                    if doc.serial != serial {
                        return;
                    }
                    let Some(DocBlock::Code {
                        text: current,
                        spans,
                        ..
                    }) = doc.blocks.get_mut(index)
                    else {
                        return;
                    };
                    if *current == text {
                        *spans = highlights;
                        cx.notify();
                    }
                })
                .ok();
            }
        })
        .detach();
    }

    /// The plain text of the open popup, for the copy button and for tests.
    pub fn quick_doc_text(&self) -> Option<String> {
        self.lsp.as_ref()?.quick.as_ref().map(plain_text)
    }

    /// How far the open popup is scrolled, for a test of the wheel and the keys.
    #[cfg(any(test, feature = "test-support"))]
    pub fn quick_doc_scroll(&self) -> Option<f32> {
        let doc = self.lsp.as_ref()?.quick.as_ref()?;
        Some(-f32::from(doc.scroll.offset().y))
    }

    /// A key for an open documentation popup: the arrows, the pages, Home and End scroll
    /// it and stay out of the editor; `false` for every other key.
    pub(crate) fn quick_doc_scroll_key(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
        let Some(doc) = self.lsp.as_mut().and_then(|lsp| lsp.quick.as_mut()) else {
            return false;
        };
        let page = f32::from(doc.scroll.bounds().size.height).max(1.);
        let line = self.style.line_height;
        let offset = doc.scroll.offset();
        let current = -f32::from(offset.y);
        let max = f32::from(doc.scroll.max_offset().y).max(0.);
        let next = match key {
            "up" => current - line,
            "down" => current + line,
            "pageup" => current - page * 0.9,
            "pagedown" => current + page * 0.9,
            "home" => 0.,
            "end" => max,
            _ => return false,
        }
        .clamp(0., max);
        doc.scroll.set_offset(point(offset.x, -px(next)));
        cx.notify();
        true
    }

    /// Drops an open question of the pointer and its answer.
    pub(crate) fn cancel_hover(&mut self) {
        self.hover_serial = self.hover_serial.wrapping_add(1);
        self.hover_word = None;
    }

    pub(crate) fn close_quick_doc(&mut self, cx: &mut Context<Self>) -> bool {
        let open = self.lsp.as_mut().and_then(|lsp| lsp.quick.take()).is_some();
        if open {
            self.cancel_hover();
            self.hover_close = self.hover_close.wrapping_add(1);
            cx.notify();
        }
        open
    }

    /// Whether the popup is open, whatever opened it.
    pub(crate) fn quick_doc_open(&self) -> bool {
        self.lsp.as_ref().is_some_and(|lsp| lsp.quick.is_some())
    }

    /// Ctrl+Q repeated pins the answer to the Documentation tool window.
    pub(crate) fn toggle_quick_doc(&mut self, cx: &mut Context<Self>) -> bool {
        if self.quick_doc_open() {
            self.pin_quick_doc(cx);
            return true;
        }
        false
    }

    /// The second Ctrl+Q and the pin button: the documentation moves to the tool window.
    pub(crate) fn pin_quick_doc(&mut self, cx: &mut Context<Self>) {
        cx.emit(crate::EditorEvent::ShowDocumentation);
        self.close_quick_doc(cx);
    }

    /// The name under the pointer, when there is one.
    pub(crate) fn hover_word_at(&self, position: Point<Pixels>) -> Option<Range<usize>> {
        let layout = self.layout.as_ref()?;
        let state = self.state.as_ref()?;
        if position.x < layout.bounds.left() + px(self.gutter_room())
            || position.x >= layout.bounds.right()
            || position.y < layout.bounds.top()
            || position.y >= layout.bounds.bottom()
        {
            return None;
        }
        let offset = self.offset_at(position)?;
        let store = state.store();
        let start = crate::semantic::word_start(&store, offset);
        let mut end = offset;
        while end < store.len() {
            let Some(character) = store.char_at(end) else {
                break;
            };
            if !is_word(character) {
                break;
            }
            end = store.next_boundary(end);
        }
        (end > start).then_some(start..end)
    }

    /// The pointer moved: a name opens the popup after the delay, and leaving the name
    /// and the popup closes it.
    pub(crate) fn hover_moved(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        if !self.options.quick_doc_on_hover {
            return;
        }
        // A list that holds the screen keeps the pointer popup away, as IDEA does.
        if self.hover_popup_blocked() {
            self.cancel_hover();
            return;
        }
        if self.lsp.as_ref().is_some_and(|lsp| {
            lsp.quick
                .as_ref()
                .is_some_and(|doc| doc.hovered || doc.selecting || doc.hover_word.is_none())
        }) {
            return;
        }
        let Some(range) = self.hover_word_at(position) else {
            self.hover_left(cx);
            return;
        };
        if let Some(doc) = self.lsp.as_ref().and_then(|lsp| lsp.quick.as_ref())
            && doc.hover_word.as_ref() == Some(&range)
        {
            // Back on the name: the popup is not closing any more.
            self.hover_close = self.hover_close.wrapping_add(1);
            return;
        }
        if self.hover_word.as_ref() == Some(&range) {
            self.hover_close = self.hover_close.wrapping_add(1);
            return;
        }
        if self
            .lsp
            .as_ref()
            .is_none_or(|lsp| lsp.support.hover().is_none())
        {
            return;
        }
        self.hover_word = Some(range.clone());
        self.hover_serial = self.hover_serial.wrapping_add(1);
        self.hover_close = self.hover_close.wrapping_add(1);
        if self.quick_doc_open() {
            self.close_quick_doc(cx);
            self.hover_word = Some(range.clone());
        }
        let serial = self.hover_serial;
        let Some(spot) = self.spot_at(range.start) else {
            return;
        };
        let Some(support) = self.lsp.as_ref().map(|lsp| lsp.support.clone()) else {
            return;
        };
        let offset = range.start;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(HOVER_DELAY).await;
            let current = this
                .update(cx, |this, _| {
                    this.hover_serial == serial
                        && this.hover_word.as_ref() == Some(&range)
                        && this.options.quick_doc_on_hover
                        && !this.hover_popup_blocked()
                        && !this.lsp.as_ref().is_some_and(|lsp| {
                            lsp.quick
                                .as_ref()
                                .is_some_and(|doc| doc.hovered || doc.selecting)
                        })
                })
                .unwrap_or(false);
            if !current {
                return;
            }
            let Some(provider) = support.hover() else {
                return;
            };
            let answer = crate::semantic::first(ask(provider, spot)).await;
            this.update(cx, |this, cx| {
                if this.hover_serial != serial
                    || this.hover_word.as_ref() != Some(&range)
                    || !this.options.quick_doc_on_hover
                    || this.hover_popup_blocked()
                    || this.lsp.as_ref().is_some_and(|lsp| {
                        lsp.quick
                            .as_ref()
                            .is_some_and(|doc| doc.hovered || doc.selecting)
                    })
                {
                    return;
                }
                let Some((text, _)) = answer else {
                    return;
                };
                let text = text.0;
                if text.trim().is_empty() {
                    return;
                }
                let diagnostic = this.diagnostic_at(offset);
                let blocks = this.lsp.as_ref().map_or_else(
                    || parse(&text),
                    |lsp| parse_hover(&text, lsp.support.as_ref()),
                );
                this.show_quick_doc(offset, blocks, diagnostic, Some(range), cx);
            })
            .ok();
        })
        .detach();
    }

    /// The pointer is not over the name any more. The popup closes unless the pointer
    /// reaches it or comes back within a moment.
    fn hover_left(&mut self, cx: &mut Context<Self>) {
        if self.hover_word.take().is_none() {
            return;
        }
        self.hover_serial = self.hover_serial.wrapping_add(1);
        self.schedule_hover_close(cx);
    }

    /// Closes the hover popup after a moment unless the pointer is over it. The pointer
    /// walking from the name to the popup crosses the gap without closing it.
    pub(crate) fn schedule_hover_close(&mut self, cx: &mut Context<Self>) {
        let hover_popup = self.lsp.as_ref().is_some_and(|lsp| {
            lsp.quick
                .as_ref()
                .is_some_and(|doc| doc.hover_word.is_some() && !doc.selecting)
        });
        if !hover_popup {
            return;
        }
        self.hover_close = self.hover_close.wrapping_add(1);
        let close = self.hover_close;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(HOVER_LEAVE_DELAY).await;
            this.update(cx, |this, cx| {
                if this.hover_close != close {
                    return;
                }
                let over_popup = this
                    .lsp
                    .as_ref()
                    .and_then(|lsp| lsp.quick.as_ref())
                    .is_some_and(|doc| doc.hovered || doc.selecting);
                if !over_popup && this.quick_doc_open() {
                    this.close_quick_doc(cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// The text of the problem under the pointer, with its severity.
    pub(crate) fn diagnostic_at(&self, offset: usize) -> Option<String> {
        let lsp = self.lsp.as_ref()?;
        let mark = lsp
            .marks
            .iter()
            .find(|mark| mark.range.start <= offset && offset <= mark.range.end)?;
        let severity = match mark.severity {
            lang_api::Severity::Error => "Error",
            lang_api::Severity::Warning => "Warning",
            lang_api::Severity::Information => "Info",
            lang_api::Severity::Hint => "Hint",
        };
        Some(format!("{severity}: {}", mark.message))
    }

    /// A click on a link of the popup: a place in a file goes the way Go to Declaration
    /// goes, a web address opens in the browser.
    pub(crate) fn open_quick_doc_link(&mut self, url: String, cx: &mut Context<Self>) {
        if url.is_empty() {
            return;
        }
        if url.starts_with("http://") || url.starts_with("https://") {
            let custom = cx
                .try_global::<crate::ExternalLinks>()
                .and_then(|links| links.0.clone());
            match custom {
                Some(open) => open(&url),
                None => {
                    ::platform::open_url(&url);
                }
            }
            return;
        }
        let (uri, line) = split_line_fragment(&url);
        if !uri.starts_with("file:") && self.library_place(&uri, cx).is_none() {
            return;
        }
        let point = TextPoint { line, character: 0 };
        let target = SourceSpan {
            uri,
            span: TextSpan {
                start: point,
                end: point,
            },
        };
        self.go_to_location(&target, cx);
    }

    /// Copies the whole answer, as the copy button does.
    pub(crate) fn copy_quick_doc(&self, cx: &mut Context<Self>) {
        if let Some(text) = self.quick_doc_text() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    /// Ctrl+C: the selected part, or the whole answer when nothing is selected.
    pub(crate) fn copy_quick_doc_selection(&self, cx: &mut Context<Self>) {
        let Some(doc) = self.lsp.as_ref().and_then(|lsp| lsp.quick.as_ref()) else {
            return;
        };
        let selected = doc
            .selection
            .as_ref()
            .filter(|range| !range.is_empty())
            .and_then(|range| doc.text.get(range.clone()));
        let text = match selected {
            Some(text) => text.to_owned(),
            None => plain_text(doc),
        };
        if !text.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    /// Ctrl+A inside the popup selects its whole text.
    pub(crate) fn select_all_quick_doc(&mut self, cx: &mut Context<Self>) {
        let Some(doc) = self.lsp.as_mut().and_then(|lsp| lsp.quick.as_mut()) else {
            return;
        };
        doc.anchor = Some(0);
        doc.selection = Some(0..doc.text.len());
        cx.notify();
    }

    /// The selected part of the open answer, for the tests.
    pub fn quick_doc_selection(&self) -> Option<Range<usize>> {
        self.lsp
            .as_ref()?
            .quick
            .as_ref()?
            .selection
            .clone()
            .filter(|range| !range.is_empty())
    }

    /// The selected text of the open answer, for the tests.
    pub fn quick_doc_selection_text(&self) -> Option<String> {
        let doc = self.lsp.as_ref()?.quick.as_ref()?;
        doc.selection
            .as_ref()
            .filter(|range| !range.is_empty())
            .and_then(|range| doc.text.get(range.clone()))
            .map(str::to_owned)
    }

    /// A mouse event of a character of the popup: it starts, moves or ends a selection.
    pub(crate) fn select_quick_doc(&mut self, event: DocSelectEvent, cx: &mut Context<Self>) {
        let Some(doc) = self.lsp.as_mut().and_then(|lsp| lsp.quick.as_mut()) else {
            return;
        };
        // A plain press does not repaint: a link click needs the same elements to see
        // its release. A drag repaints on every step and selects the part.
        let mut notify = true;
        match event {
            DocSelectEvent::Down {
                offset,
                click_count,
                shift,
            } => {
                let offset = offset.min(doc.text.len());
                doc.selecting = true;
                doc.hovered = true;
                self.hover_close = self.hover_close.wrapping_add(1);
                if click_count >= 3 {
                    let range = paragraph_range(&doc.text, offset);
                    doc.anchor = Some(range.start);
                    doc.selection = Some(range);
                } else if click_count == 2 {
                    let range = word_range(&doc.text, offset);
                    doc.anchor = Some(range.start);
                    doc.selection = Some(range);
                } else if shift {
                    let anchor = doc.anchor.unwrap_or(offset);
                    doc.selection = Some(ordered_range(anchor, offset));
                } else {
                    doc.anchor = Some(offset);
                    doc.selection = Some(offset..offset);
                    notify = false;
                }
            }
            DocSelectEvent::Drag { offset } => {
                if doc.selecting {
                    let offset = offset.min(doc.text.len());
                    let anchor = doc.anchor.unwrap_or(offset);
                    doc.selection = Some(ordered_range(anchor, offset));
                }
            }
            DocSelectEvent::Up => {
                doc.selecting = false;
                if doc.selection.as_ref().is_some_and(|range| range.is_empty()) {
                    doc.selection = None;
                }
            }
        }
        if notify {
            cx.notify();
        }
    }

    /// The popup body.
    pub(crate) fn render_quick_doc(
        &self,
        doc: &QuickDoc,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.global::<Theme>();
        let style = &self.style;
        let viewport = window.viewport_size();
        let half = f32::from(viewport.height) * 0.5;
        let editor = cx.entity();
        let link_editor = editor.clone();
        let on_link: DocLinkHandler = Rc::new(move |url, _window, cx| {
            link_editor.update(cx, |this, cx| this.open_quick_doc_link(url, cx));
        });
        let select_editor = editor;
        let on_select: DocSelectHandler = Rc::new(move |event, _window, cx| {
            select_editor.update(cx, |this, cx| this.select_quick_doc(event, cx));
        });
        let host = DocHost {
            name: "quick-doc".to_owned(),
            on_link,
            on_select: Some(on_select),
            selection: doc.selection.clone(),
            base: None,
        };
        let content = div()
            .flex()
            .flex_col()
            .w_full()
            .gap(px(SPACE_S))
            .p(px(SPACE_M))
            .pr(px(BUTTONS_ROOM))
            .child(render_document(
                &doc.blocks,
                doc.diagnostic.as_deref(),
                style,
                &theme,
                &host,
                window,
                cx,
            ));
        let body = div()
            .id("quick-doc-scroll")
            .flex()
            .flex_col()
            .w_full()
            .max_h(px(half))
            .overflow_y_scroll()
            .track_scroll(&doc.scroll)
            .child(content);
        let body = scrollable("quick-doc", &doc.scroll, body, &theme, window, cx)
            .flex_1()
            .min_h_0();
        let pin = icon_button(
            "quick-doc-pin",
            "general/openInToolWindow",
            "Pin to tool window",
            None,
            &theme,
        )
        .on_click(cx.listener(|this, _, _, cx| this.pin_quick_doc(cx)));
        let copy = icon_button("quick-doc-copy", "general/copy", "Copy", None, &theme)
            .on_click(cx.listener(|this, _, _, cx| this.copy_quick_doc(cx)));
        let width = estimate_width(doc, style.font_size);
        let frame = surface(&theme)
            .id("quick-doc-frame")
            .debug_selector(|| "quick-doc".to_owned())
            .occlude()
            .relative()
            .w(px(width))
            .min_w(px(MIN_WIDTH))
            .max_w(px(MAX_WIDTH))
            .max_h(px(half))
            .flex()
            .flex_col()
            .child(body)
            .child(
                div()
                    .debug_selector(|| "quick-doc-buttons".to_owned())
                    .absolute()
                    .top(px(SPACE_XS))
                    .right(px(SPACE_XS))
                    .flex()
                    .gap(px(SPACE_XS))
                    .child(pin)
                    .child(copy),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, cx| {
                    this.select_quick_doc(DocSelectEvent::Up, cx)
                }),
            )
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                if let Some(doc) = this.lsp.as_mut().and_then(|lsp| lsp.quick.as_mut()) {
                    doc.hovered = *hovered;
                }
                if *hovered {
                    this.hover_close = this.hover_close.wrapping_add(1);
                } else {
                    this.schedule_hover_close(cx);
                }
                cx.notify();
            }));
        let height = estimate_height(doc, style.line_height).min(half);
        let Some((position, anchor)) = self.popup_position(doc.at, height) else {
            return frame.into_any_element();
        };
        deferred(
            anchored()
                .anchor(anchor)
                .position(position)
                .snap_to_window_with_margin(px(SPACE_M))
                .child(frame),
        )
        .with_priority(1)
        .into_any_element()
    }
}

/// Where a link of an answer goes.
pub type DocLinkHandler = Rc<dyn Fn(String, &mut Window, &mut App)>;
/// How a character of an answer starts, moves or ends a selection.
pub type DocSelectHandler = Rc<dyn Fn(DocSelectEvent, &mut Window, &mut App)>;
/// Opens or closes the `<details>` block with this id in the file that shows it.
pub(crate) type DocToggleHandler = Rc<dyn Fn(usize, &mut Window, &mut App)>;

/// What a rendered answer asks of the place that shows it: where a link goes and how a
/// character of the text starts, moves or ends a selection. `base` is the folder of the
/// file the answer describes, so a picture with a relative address can be drawn; the popup
/// of a server answer has none and shows the text of the picture instead.
pub struct DocHost {
    pub name: String,
    pub on_link: DocLinkHandler,
    pub on_select: Option<DocSelectHandler>,
    pub selection: Option<Range<usize>>,
    pub base: Option<std::path::PathBuf>,
}

/// A mouse event on one character of the answer, with its byte offset in the text.
#[derive(Clone, Debug)]
pub enum DocSelectEvent {
    Down {
        offset: usize,
        click_count: usize,
        shift: bool,
    },
    Drag {
        offset: usize,
    },
    Up,
}

/// The width of a column of a table, so that a wide one scrolls sideways.
const TABLE_COLUMN_WIDTH: f32 = 180.;

/// The file a picture of the answer comes from, when the address points at one of the
/// formats the window draws.
pub(crate) fn image_path(base: &Path, url: &str) -> Option<PathBuf> {
    let url = url.split(['#', '?']).next().unwrap_or_default();
    if url.is_empty() || url.contains("://") {
        return None;
    }
    let path = Path::new(url);
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    };
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    matches!(
        extension.as_str(),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp" | "ico" | "avif" | "tif" | "tiff"
    )
    .then_some(path)
}

/// Draws an answer with the colours of the editor. The same blocks reach the popup and
/// the Documentation tool window.
pub fn render_document(
    blocks: &[DocBlock],
    diagnostic: Option<&str>,
    style: &EditorStyle,
    theme: &Theme,
    host: &DocHost,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let mut state = DocRenderer::new(style, theme, host);
    let mut column = div().flex().flex_col().w_full().gap(px(SPACE_S));
    if let Some(diagnostic) = diagnostic {
        let text = state.label(diagnostic, theme.stripe_error);
        column = column.child(
            div()
                .debug_selector(|| format!("{}-diagnostic", host.name))
                .w_full()
                .rounded(px(4.))
                .bg(theme.selection_muted)
                .p(px(SPACE_S))
                .text_size(px(UI_SIZE))
                .text_color(theme.stripe_error)
                .child(text),
        );
    }
    for block in blocks {
        column = column.child(state.block(block, window, cx));
    }
    column.into_any_element()
}

/// Draws the blocks of an answer one at a time, so that a caller can put each of them in
/// an element of its own: the preview of a file scrolls to the block the editor shows and
/// wraps every block for that.
pub(crate) struct DocRenderer<'a>(DocRender<'a>);

impl<'a> DocRenderer<'a> {
    pub(crate) fn new(style: &'a EditorStyle, theme: &'a Theme, host: &'a DocHost) -> Self {
        Self(DocRender {
            style,
            theme,
            host,
            cursor: 0,
            next_link: 0,
            link_tokens: Vec::new(),
            table: 0,
            word: 0,
            pictures: None,
            picture_count: 0,
            document: false,
            list_depth: 0,
            toggled: None,
            on_toggle: None,
        })
    }

    /// The renderer of a markdown file: the place found the pictures of the file itself, so
    /// a picture missing from `pictures` is shown by its text, and the blocks take the
    /// spacing of a page. `toggled` holds the `<details>` blocks the reader has turned away
    /// from their default state, and `on_toggle` turns one of them.
    pub(crate) fn for_document(
        style: &'a EditorStyle,
        theme: &'a Theme,
        host: &'a DocHost,
        pictures: &'a HashMap<String, DocPicture>,
        toggled: &'a HashSet<usize>,
        on_toggle: DocToggleHandler,
    ) -> Self {
        let mut renderer = Self::new(style, theme, host);
        renderer.0.pictures = Some(pictures);
        renderer.0.toggled = Some(toggled);
        renderer.0.on_toggle = Some(on_toggle);
        renderer.0.document = true;
        renderer
    }

    /// One block, in the order of the answer: the cursor keeps the offsets of a selection
    /// running from the first block to the last.
    pub(crate) fn block(
        &mut self,
        block: &DocBlock,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        self.0.block(block, window, cx)
    }

    /// A warning above the blocks, such as the problem under the pointer. Its text is
    /// part of the answer, so the offsets of a selection start after it.
    fn label(&mut self, text: &str, color: Hsla) -> Div {
        self.0.cursor += text.len();
        self.0.cursor += 2;
        let inlines = [DocInline::Text(text.to_owned())];
        self.0.inlines(&inlines, UI_SIZE, color, false, true)
    }
}

struct DocRender<'a> {
    style: &'a EditorStyle,
    theme: &'a Theme,
    host: &'a DocHost,
    /// The byte offset of the next character in the text of the answer.
    cursor: usize,
    next_link: usize,
    /// How many words of each link have been named for a selector.
    link_tokens: Vec<usize>,
    table: usize,
    word: usize,
    /// The pictures the caller has already found by their address, `None` when it has
    /// none and every picture shows its text.
    pictures: Option<&'a std::collections::HashMap<String, DocPicture>>,
    /// How many pictures have been placed, for a selector.
    picture_count: usize,
    /// Whether the blocks are those of a markdown file, drawn with the spacing of a page.
    document: bool,
    /// How many lists the block being drawn is in: the marks of their items depend on it.
    list_depth: usize,
    /// The `<details>` blocks the reader has turned away from their default state, by id.
    toggled: Option<&'a HashSet<usize>>,
    /// Turns the `<details>` block with an id open or closed.
    on_toggle: Option<DocToggleHandler>,
}

impl DocRender<'_> {
    fn block(&mut self, block: &DocBlock, window: &mut Window, cx: &mut App) -> AnyElement {
        let theme = self.theme;
        let style = self.style;
        match block {
            DocBlock::Paragraph(inlines) => {
                let element = self
                    .inlines(inlines, UI_SIZE, theme.code, false, true)
                    .into_any_element();
                self.cursor += 1;
                element
            }
            DocBlock::Heading { level, text } => {
                let size = if self.document {
                    document_heading_size(*level)
                } else {
                    heading_size(*level)
                };
                let element = self
                    .inlines(text, size, theme.text, true, true)
                    .font_weight(FontWeight::SEMIBOLD)
                    .into_any_element();
                self.cursor += 1;
                if self.document {
                    let (above, below) = document_heading_space(*level);
                    return div()
                        .w_full()
                        .pt(px(above))
                        .pb(px(below))
                        .child(element)
                        .into_any_element();
                }
                element
            }
            DocBlock::List {
                ordered,
                start,
                items,
                nested,
            } => {
                let mut list = div().flex().flex_col().gap(px(SPACE_XS)).w_full();
                for (index, item) in items.iter().enumerate() {
                    let bullet = bullet_of(*ordered, *start, index, self.list_depth);
                    let bullet =
                        self.plain_run(&format!("{bullet} "), UI_SIZE, theme.text_secondary);
                    let row = self.inlines(item, UI_SIZE, theme.code, false, true);
                    // The blocks under the line stand in the column of its text.
                    let content = match nested.get(index).filter(|blocks| !blocks.is_empty()) {
                        None => row.into_any_element(),
                        Some(blocks) => {
                            let mut column = div()
                                .flex()
                                .flex_col()
                                .gap(px(SPACE_XS))
                                .w_full()
                                .child(row);
                            self.list_depth += 1;
                            for block in blocks {
                                column = column.child(self.block(block, window, cx));
                            }
                            self.list_depth -= 1;
                            column.into_any_element()
                        }
                    };
                    list = list.child(
                        div()
                            .flex()
                            .items_start()
                            .gap(px(SPACE_S))
                            .w_full()
                            .child(bullet)
                            .child(div().flex_1().min_w_0().child(content)),
                    );
                    self.cursor += 1;
                }
                list.into_any_element()
            }
            DocBlock::Code {
                text,
                spans,
                language,
            } => {
                let padding = if self.document { SPACE_M } else { SPACE_S };
                let mut column = div()
                    .flex()
                    .flex_col()
                    .w_full()
                    .rounded(px(4.))
                    .bg(theme.selection_muted)
                    .p(px(padding))
                    .font_family(style.font_family.clone())
                    .text_size(px(style.font_size));
                if language.as_deref().is_some_and(|name| !name.is_empty()) {
                    column = column.debug_selector(|| format!("{}-code", self.host.name));
                }
                let name = self.host.name.clone();
                let mut at = 0;
                for line in text.split_inclusive('\n') {
                    let content = line.strip_suffix('\n').unwrap_or(line);
                    let end = at + content.len();
                    // The offset of the line names its chunks, so that no two chunks of the
                    // popup share a selector.
                    let line_start = self.cursor;
                    // A line of the Quick Doc breaks after a space or a separator, so that its
                    // words wrap in the width of the popup. A document keeps its lines.
                    let mut chunks: Vec<Vec<AnyElement>> = Vec::new();
                    let mut chunk = Vec::new();
                    let mut characters = content.char_indices().peekable();
                    while let Some((index, character)) = characters.next() {
                        let offset = self.cursor;
                        self.cursor += character.len_utf8();
                        let color = code_color(at + index, spans, style);
                        chunk.push(self.character(
                            character,
                            offset,
                            color,
                            style.font_size,
                            false,
                            false,
                            true,
                        ));
                        let next = characters.peek().map(|&(_, next)| next);
                        if !self.document && breaks_after(character, next) {
                            chunks.push(std::mem::take(&mut chunk));
                        }
                    }
                    if !chunk.is_empty() {
                        chunks.push(chunk);
                    }
                    self.cursor += 1;
                    let row = if chunks.is_empty() {
                        div().flex().h(px(style.line_height)).into_any_element()
                    } else if self.document {
                        div()
                            .flex()
                            .whitespace_nowrap()
                            .children(chunks.into_iter().flatten())
                            .into_any_element()
                    } else {
                        div()
                            .flex()
                            .flex_wrap()
                            .w_full()
                            .children(chunks.into_iter().enumerate().map(|(index, chunk)| {
                                div()
                                    .flex()
                                    .flex_none()
                                    .whitespace_nowrap()
                                    .debug_selector(|| format!("{name}-chunk-{line_start}-{index}"))
                                    .children(chunk)
                            }))
                            .into_any_element()
                    };
                    column = column.child(row);
                    at = end + usize::from(line.ends_with('\n'));
                }
                column.into_any_element()
            }
            DocBlock::Table {
                alignments,
                header,
                rows,
            } => {
                if self.document {
                    self.document_table(alignments, header, rows)
                } else {
                    self.table(alignments, header, rows, window, cx)
                }
            }
            DocBlock::Details {
                id,
                open,
                summary,
                blocks,
            } => self.details(*id, *open, summary, blocks, window, cx),
            DocBlock::Image { alt, url } => {
                self.cursor += 1;
                let image = DocImage {
                    alt: alt.clone(),
                    url: url.clone(),
                    width: None,
                    height: None,
                };
                self.image(&image)
            }
            DocBlock::Quote(inner) => {
                let mut column = div()
                    .flex()
                    .flex_col()
                    .w_full()
                    .gap(px(SPACE_XS))
                    .border_l_1()
                    .border_color(theme.border)
                    .pl(px(SPACE_M));
                for block in inner {
                    column = column.child(self.block(block, window, cx));
                }
                column.into_any_element()
            }
            DocBlock::Place(text) => {
                let inlines = [DocInline::Text(text.clone())];
                let element = self
                    .inlines(&inlines, UI_SIZE, theme.text_secondary, false, true)
                    .into_any_element();
                self.cursor += 1;
                element
            }
            DocBlock::Divider => {
                self.cursor += 1;
                let rule = div().w_full().h(px(1.)).bg(theme.border);
                if self.document {
                    rule.my(px(SPACE_M)).into_any_element()
                } else {
                    rule.into_any_element()
                }
            }
        }
    }

    fn table(
        &mut self,
        alignments: &[DocAlign],
        header: &[Vec<DocInline>],
        rows: &[Vec<Vec<DocInline>>],
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let theme = self.theme;
        let style = self.style;
        let columns = table_columns(alignments, header, rows);
        let index = self.table;
        self.table += 1;
        let width = TABLE_COLUMN_WIDTH * columns.max(1) as f32;
        let row_height = style.line_height + SPACE_S;
        let count = rows.len() + 1;
        let name = self.host.name.clone();
        let mut table = div()
            .flex()
            .flex_col()
            .w(px(width))
            .h(px(row_height * count as f32))
            .debug_selector({
                let name = name.clone();
                move || format!("{name}-table-{index}")
            });
        for (row_index, cells) in std::iter::once(header)
            .chain(rows.iter().map(Vec::as_slice))
            .enumerate()
        {
            let header_row = row_index == 0;
            let mut row = div()
                .flex()
                .items_center()
                .flex_1()
                .w_full()
                .border_b_1()
                .border_color(theme.border);
            if header_row {
                row = row.bg(theme.selection_muted);
            }
            for column in 0..columns {
                let alignment = alignments.get(column).copied().unwrap_or_default();
                let content = cells.get(column).map_or_else(
                    || div().into_any_element(),
                    |cell| {
                        self.inlines(cell, UI_SIZE, theme.text, header_row, false)
                            .into_any_element()
                    },
                );
                let selector = format!("{name}-table-{index}-cell-{row_index}-{column}");
                let aligned = match alignment {
                    DocAlign::Center => div().flex().w_full().min_w_0().justify_center(),
                    DocAlign::Right => div().flex().w_full().min_w_0().justify_end(),
                    DocAlign::Left => div().flex().w_full().min_w_0().justify_start(),
                };
                let cell = div()
                    .flex_none()
                    .w(px(TABLE_COLUMN_WIDTH))
                    .h_full()
                    .min_w_0()
                    .overflow_hidden()
                    .flex()
                    .items_center()
                    .px(px(SPACE_S))
                    .border_r_1()
                    .border_color(theme.border)
                    .debug_selector(move || selector.clone())
                    .child(aligned.child(content));
                row = row.child(cell);
                if column + 1 < columns {
                    self.cursor += 3;
                }
            }
            self.cursor += 1;
            table = table.child(row);
        }
        let handle = {
            let key = ElementId::Name(format!("{name}-table-scroll-{index}").into());
            window
                .use_keyed_state(key, cx, |_, _| ScrollHandle::new())
                .read(cx)
                .clone()
        };
        let scrolled = div()
            .id(format!("{name}-table-scroll-area-{index}"))
            .overflow_x_scroll()
            .track_scroll(&handle)
            .child(table);
        scrollable(
            format!("{name}-table-{index}"),
            &handle,
            scrolled,
            theme,
            window,
            cx,
        )
        .h(px(row_height * count as f32))
        .into_any_element()
    }

    /// A table of a markdown file. Each column is as wide as its text needs, and the text of
    /// a cell wraps when the table is wider than its place. The grid lines are thin and the
    /// head is bold on a background.
    fn document_table(
        &mut self,
        alignments: &[DocAlign],
        header: &[Vec<DocInline>],
        rows: &[Vec<Vec<DocInline>>],
    ) -> AnyElement {
        let theme = self.theme;
        let columns = table_columns(alignments, header, rows);
        let index = self.table;
        self.table += 1;
        let name = self.host.name.clone();
        let mut grid = div()
            .grid()
            .grid_cols_max_content(u16::try_from(columns.max(1)).unwrap_or(u16::MAX))
            .min_w_0()
            .border_t_1()
            .border_l_1()
            .border_color(theme.border)
            .debug_selector({
                let name = name.clone();
                move || format!("{name}-table-{index}")
            });
        for (row_index, cells) in std::iter::once(header)
            .chain(rows.iter().map(Vec::as_slice))
            .enumerate()
        {
            let header_row = row_index == 0;
            for column in 0..columns {
                let alignment = alignments.get(column).copied().unwrap_or_default();
                let content = match cells.get(column) {
                    Some(cell) => self
                        .table_text(cell, theme.text, header_row, alignment)
                        .into_any_element(),
                    None => div().into_any_element(),
                };
                let selector = format!("{name}-table-{index}-cell-{row_index}-{column}");
                let mut cell = div()
                    .min_w_0()
                    .overflow_hidden()
                    .px(px(SPACE_S))
                    .py(px(SPACE_XS))
                    .border_r_1()
                    .border_b_1()
                    .border_color(theme.border)
                    .debug_selector(move || selector.clone())
                    .child(content);
                if header_row {
                    cell = cell.bg(theme.selection_muted);
                }
                grid = grid.child(cell);
                if column + 1 < columns {
                    self.cursor += 3;
                }
            }
            self.cursor += 1;
        }
        div().flex().w_full().child(grid).into_any_element()
    }

    /// A `<details>` block of a file: its summary line with an arrow, and under it the blocks
    /// while it is open. A click on the summary opens the block or closes it.
    fn details(
        &mut self,
        id: usize,
        default_open: bool,
        summary: &[DocInline],
        blocks: &[DocBlock],
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let theme = self.theme;
        let open = default_open != self.toggled.is_some_and(|toggled| toggled.contains(&id));
        let name = self.host.name.clone();
        let arrow = icon_sized(
            if open {
                "general/chevronDown"
            } else {
                "general/chevronRight"
            },
            10.,
            theme,
        );
        let text = self.inlines(summary, UI_SIZE, theme.text, false, true);
        let selector = format!("{name}-details-summary-{id}");
        let mut header = div()
            .id(ElementId::Name(selector.clone().into()))
            .flex()
            .items_start()
            .gap(px(SPACE_XS))
            .w_full()
            .cursor_pointer()
            .debug_selector(move || selector.clone())
            .child(arrow)
            .child(div().flex_1().min_w_0().child(text));
        if let Some(on_toggle) = self.on_toggle.clone() {
            header = header.on_click(move |_, window, cx| on_toggle(id, window, cx));
        }
        let mut column = div()
            .flex()
            .flex_col()
            .w_full()
            .gap(px(SPACE_XS))
            .child(header);
        if open {
            let selector = format!("{name}-details-body-{id}");
            let mut body = div()
                .debug_selector(move || selector.clone())
                .flex()
                .flex_col()
                .w_full()
                .gap(px(SPACE_S))
                .pl(px(SPACE_M))
                .border_l_1()
                .border_color(theme.border);
            for block in blocks {
                body = body.child(self.block(block, window, cx));
            }
            column = column.child(body);
        }
        column.into_any_element()
    }

    /// A picture of the text: drawn where the caller found it, the text of it in a frame
    /// when there is nothing to draw, or taken from the folder of the file when the
    /// viewer knows that folder and nothing more.
    fn image(&mut self, image: &DocImage) -> AnyElement {
        let theme = self.theme;
        let index = self.next_picture();
        if let Some(drawn) = self.picture(image, index, None) {
            return div()
                .flex()
                .flex_col()
                .w_full()
                .child(drawn)
                .into_any_element();
        }
        if self.pictures.is_some() {
            return div()
                .flex()
                .flex_col()
                .w_full()
                .child(self.picture_text(image, index, None))
                .into_any_element();
        }
        let text = if image.alt.is_empty() {
            &image.url
        } else {
            &image.alt
        };
        let Some(base) = &self.host.base else {
            let inlines = [DocInline::Text(text.to_owned())];
            return self
                .inlines(&inlines, UI_SIZE, theme.code, false, true)
                .into_any_element();
        };
        let Some(path) = image_path(base, &image.url) else {
            let inlines = [DocInline::Text(text.to_owned())];
            return self
                .inlines(&inlines, UI_SIZE, theme.code, false, true)
                .into_any_element();
        };
        let inlines = [DocInline::Text(text.to_owned())];
        let caption = self
            .inlines(&inlines, UI_SIZE, theme.text_secondary, false, true)
            .into_any_element();
        div()
            .flex()
            .flex_col()
            .gap(px(SPACE_XS))
            .w_full()
            .child(img(path.as_path()).max_w_full())
            .child(caption)
            .into_any_element()
    }

    /// The number of the next picture, for the selector of its element.
    fn next_picture(&mut self) -> usize {
        let index = self.picture_count;
        self.picture_count += 1;
        index
    }

    /// The picture itself, when the caller found it: no wider than the column that holds
    /// it, and as wide and as high as the document asks. `link` makes it open.
    fn picture(&self, image: &DocImage, index: usize, link: Option<&str>) -> Option<AnyElement> {
        let picture = self.pictures?.get(&image.url)?;
        let mut element = match picture {
            DocPicture::File(path) => img(path.as_path()),
            DocPicture::Bytes(bytes) => img(bytes.clone()),
            DocPicture::Loading | DocPicture::Unavailable => return None,
        };
        element = element.flex_none().max_w_full();
        if let Some(width) = image.width {
            element = element.w(px(width.max(1.)));
        }
        if let Some(height) = image.height {
            element = element.h(px(height.max(1.)));
        }
        let selector = format!("{}-image-{index}", self.host.name);
        let mut element = element
            .id(ElementId::Name(selector.clone().into()))
            .debug_selector(move || selector);
        if let Some(url) = link {
            let on_link = self.host.on_link.clone();
            let url = url.to_owned();
            element = element
                .cursor_pointer()
                .on_click(move |_, window, cx| on_link(url.clone(), window, cx));
        }
        Some(element.into_any_element())
    }

    /// The text of a picture there is nothing to draw for, in a frame of its own. It
    /// opens the link around the picture, as the words of a link do.
    fn picture_text(&mut self, image: &DocImage, index: usize, link: Option<&str>) -> AnyElement {
        let theme = self.theme;
        let text = if image.alt.is_empty() {
            image.url.clone()
        } else {
            image.alt.clone()
        };
        self.cursor += text.len();
        let name = self.host.name.clone();
        let mut frame = div()
            .id(ElementId::Name(format!("{name}-picture-{index}").into()))
            .flex_none()
            .mx(px(SPACE_XS))
            .px(px(SPACE_S))
            .py(px(SPACE_XS))
            .rounded(px(4.))
            .border_1()
            .border_color(theme.border)
            .text_size(px(UI_SIZE))
            .text_color(theme.text_secondary)
            .debug_selector(move || format!("{name}-picture-{index}"));
        if let Some(url) = link {
            let on_link = self.host.on_link.clone();
            let url = url.to_owned();
            frame = frame
                .cursor_pointer()
                .on_click(move |_, window, cx| on_link(url.clone(), window, cx));
        }
        frame.child(text).into_any_element()
    }

    /// The elements of the runs, one row for each line of the text: a line break starts a row,
    /// and the words of a row wrap when the caller asks for it.
    fn lines_of(
        &mut self,
        inlines: &[DocInline],
        size: f32,
        color: Hsla,
        bold: bool,
        wrap: bool,
    ) -> Vec<Vec<AnyElement>> {
        let theme = self.theme;
        let mut pieces = Vec::new();
        let base = InlineStyle {
            bold,
            italic: false,
            code: false,
            url: None,
            link: None,
        };
        for inline in inlines {
            flatten_inline(inline, base, &mut self.next_link, &mut pieces);
        }
        let mut lines: Vec<Vec<AnyElement>> = vec![Vec::new()];
        for piece in pieces {
            if let Some(image) = &piece.image {
                let link = piece.url.as_deref();
                let index = self.next_picture();
                let cell = match self.picture(image, index, link) {
                    Some(drawn) => drawn,
                    None if self.pictures.is_some() => self.picture_text(image, index, link),
                    None => {
                        let inlines = [DocInline::Text(piece.text.clone())];
                        self.inlines(&inlines, size, color, bold, wrap)
                            .into_any_element()
                    }
                };
                lines.last_mut().expect("a line").push(cell);
                continue;
            }
            for (line, text) in piece.text.split('\n').enumerate() {
                if line > 0 {
                    self.cursor += 1;
                    lines.push(Vec::new());
                }
                for word in word_pieces(text) {
                    let (selector, link_url) =
                        if let Some((link, url)) = piece.link.zip(piece.url.as_ref()) {
                            while self.link_tokens.len() <= link {
                                self.link_tokens.push(0);
                            }
                            let token = self.link_tokens[link];
                            self.link_tokens[link] += 1;
                            (
                                format!("{}-link-{link}-{token}", self.host.name),
                                Some(url.clone()),
                            )
                        } else {
                            let selector = format!("{}-word-{}", self.host.name, self.word);
                            self.word += 1;
                            (selector, None)
                        };
                    let mut cell = div()
                        .id(ElementId::Name(selector.clone().into()))
                        .flex()
                        .flex_none()
                        .whitespace_nowrap()
                        .text_size(px(size))
                        .text_color(color);
                    if piece.bold {
                        cell = cell.font_weight(FontWeight::SEMIBOLD);
                    }
                    if piece.italic {
                        cell = cell.italic();
                    }
                    if piece.code {
                        cell = cell
                            .font_family(self.style.font_family.clone())
                            .bg(theme.selection_muted)
                            .text_color(theme.code);
                    }
                    if let Some(url) = &link_url {
                        let on_link = self.host.on_link.clone();
                        let url = url.clone();
                        cell = cell
                            .cursor_pointer()
                            .text_color(theme.link)
                            .underline()
                            .on_click(move |_, window, cx| on_link(url.clone(), window, cx));
                    }
                    let mut chars = Vec::new();
                    let character_color = if piece.code { theme.code } else { color };
                    for character in word.chars() {
                        let offset = self.cursor;
                        self.cursor += character.len_utf8();
                        chars.push(self.character(
                            character,
                            offset,
                            character_color,
                            size,
                            piece.bold,
                            piece.italic,
                            piece.code,
                        ));
                    }
                    let cell = cell
                        .debug_selector(move || selector.clone())
                        .children(chars)
                        .into_any_element();
                    lines.last_mut().expect("a line").push(cell);
                }
            }
        }
        lines
    }

    fn inlines(
        &mut self,
        inlines: &[DocInline],
        size: f32,
        color: Hsla,
        bold: bool,
        wrap: bool,
    ) -> Div {
        let mut column = div().flex().flex_col().w_full();
        for line in self.lines_of(inlines, size, color, bold, wrap) {
            if line.is_empty() {
                continue;
            }
            column = column.child(if wrap {
                div().flex().flex_wrap().w_full().children(line)
            } else {
                div().flex().whitespace_nowrap().children(line)
            });
        }
        column
    }

    /// The text of a table cell: its words wrap in the width of the cell, and each line sits
    /// where the alignment of its column puts it.
    fn table_text(
        &mut self,
        inlines: &[DocInline],
        color: Hsla,
        bold: bool,
        alignment: DocAlign,
    ) -> Div {
        let mut column = div().flex().flex_col().w_full();
        for line in self.lines_of(inlines, UI_SIZE, color, bold, true) {
            if line.is_empty() {
                continue;
            }
            let row = div().flex().flex_wrap().w_full();
            let row = match alignment {
                DocAlign::Left => row.justify_start(),
                DocAlign::Center => row.justify_center(),
                DocAlign::Right => row.justify_end(),
            };
            column = column.child(row.children(line));
        }
        column
    }

    fn plain_run(&mut self, text: &str, size: f32, color: Hsla) -> Div {
        let mut chars = Vec::new();
        for character in text.chars() {
            let offset = self.cursor;
            self.cursor += character.len_utf8();
            chars.push(self.character(character, offset, color, size, false, false, false));
        }
        div().flex().flex_none().whitespace_nowrap().children(chars)
    }

    #[allow(clippy::too_many_arguments)]
    fn character(
        &self,
        character: char,
        offset: usize,
        color: Hsla,
        size: f32,
        bold: bool,
        italic: bool,
        code: bool,
    ) -> AnyElement {
        let theme = self.theme;
        let selected = self
            .host
            .selection
            .as_ref()
            .is_some_and(|range| range.start <= offset && offset < range.end);
        let mut cell = div()
            .flex_none()
            .text_size(px(size))
            .text_color(color)
            .child(character.to_string());
        if bold {
            cell = cell.font_weight(FontWeight::SEMIBOLD);
        }
        if italic {
            cell = cell.italic();
        }
        if code {
            cell = cell.font_family(self.style.font_family.clone());
        }
        if selected {
            cell = cell.bg(theme.selection);
        }
        if let Some(on_select) = &self.host.on_select {
            let down = on_select.clone();
            let moved = on_select.clone();
            let up = on_select.clone();
            cell = cell
                .on_mouse_down(
                    MouseButton::Left,
                    move |event: &MouseDownEvent, window, cx| {
                        down(
                            DocSelectEvent::Down {
                                offset,
                                click_count: event.click_count,
                                shift: event.modifiers.shift,
                            },
                            window,
                            cx,
                        )
                    },
                )
                .on_mouse_move(move |event: &MouseMoveEvent, window, cx| {
                    if event.dragging() {
                        moved(DocSelectEvent::Drag { offset }, window, cx);
                    }
                })
                .on_mouse_up(MouseButton::Left, move |_, window, cx| {
                    up(DocSelectEvent::Up, window, cx)
                });
        }
        cell.into_any_element()
    }
}

/// Whether a line of code breaks after a character: after a space or a separator that a word
/// follows, so that a long declaration wraps between its words and its names.
fn breaks_after(character: char, next: Option<char>) -> bool {
    let separator = character.is_whitespace() || matches!(character, '.' | ',' | '(');
    separator && next.is_some_and(|next| !next.is_whitespace())
}

/// What one span of a code line colours at `at`, or `None` when no span covers it.
fn code_color(at: usize, spans: &[HighlightSpan], style: &EditorStyle) -> Hsla {
    spans
        .iter()
        .find(|span| span.start as usize <= at && at < span.end as usize)
        .map_or(style.code, |span| style.color_of(span.kind))
}

fn heading_size(level: usize) -> f32 {
    match level {
        1 => 18.,
        2 => 16.,
        3 => 15.,
        4 => 14.,
        5 => 13.,
        _ => 12.,
    }
}

/// The size of a heading in a markdown file: the first two levels stand out of the text.
fn document_heading_size(level: usize) -> f32 {
    match level {
        1 => 26.,
        2 => 21.,
        3 => 17.,
        4 => 15.,
        _ => 13.,
    }
}

/// The space above and below a heading in a markdown file.
fn document_heading_space(level: usize) -> (f32, f32) {
    if level <= 2 {
        (SPACE_XL, SPACE_M)
    } else {
        (SPACE_L, SPACE_S)
    }
}

fn ordered_range(first: usize, second: usize) -> Range<usize> {
    if first <= second {
        first..second
    } else {
        second..first
    }
}

/// The word around a byte offset, or a collapsed range on a separator.
fn word_range(text: &str, offset: usize) -> Range<usize> {
    let offset = offset.min(text.len());
    let mut start = offset;
    while start > 0 {
        let Some(previous) = text[..start].chars().next_back() else {
            break;
        };
        if !is_word(previous) {
            break;
        }
        start -= previous.len_utf8();
    }
    let mut end = offset;
    while end < text.len() {
        let Some(next) = text[end..].chars().next() else {
            break;
        };
        if !is_word(next) {
            break;
        }
        end += next.len_utf8();
    }
    if start == end {
        offset..offset
    } else {
        start..end
    }
}

/// The line of a byte offset, without its line break.
fn paragraph_range(text: &str, offset: usize) -> Range<usize> {
    let offset = offset.min(text.len());
    let start = text[..offset].rfind('\n').map_or(0, |index| index + 1);
    let end = text[offset..]
        .find('\n')
        .map_or(text.len(), |index| offset + index);
    start..end
}

/// A rough width of the popup before layout, so that a short answer gets a narrow window
/// and a long one stops at the widest of the set. The window keeps the room of its
/// buttons on the right.
fn estimate_width(doc: &QuickDoc, font_size: f32) -> f32 {
    let character = font_size * 0.62;
    let longest = doc
        .text
        .lines()
        .map(|line| line.chars().count())
        .max()
        .unwrap_or(0) as f32;
    (longest * character + SPACE_M + 2. * SPACE_S + BUTTONS_ROOM).clamp(MIN_WIDTH, MAX_WIDTH)
}

/// A rough height of the popup before layout, for placing it above or below the line.
fn estimate_height(doc: &QuickDoc, line_height: f32) -> f32 {
    let mut rows = 0.;
    if let Some(diagnostic) = &doc.diagnostic {
        rows += 1. + (diagnostic.len() / 70) as f32;
    }
    for block in &doc.blocks {
        rows += block_rows(block);
    }
    rows * line_height.max(16.) + 24.
}

/// Rough rows of one block before layout, for placing the popup.
fn block_rows(block: &DocBlock) -> f32 {
    match block {
        DocBlock::Paragraph(inlines) => {
            let text = plain_inlines(inlines);
            (1 + text.len() / 70) as f32
        }
        DocBlock::Heading { .. } => 1.5,
        DocBlock::List { items, nested, .. } => {
            items.len() as f32 + nested.iter().flatten().map(block_rows).sum::<f32>()
        }
        DocBlock::Code { text, .. } => text.lines().count().max(1) as f32,
        DocBlock::Table { header, rows, .. } => (header.len().max(1) + rows.len()) as f32 * 1.6,
        DocBlock::Image { .. } => 6.,
        DocBlock::Quote(inner) => inner.iter().map(block_rows).sum(),
        DocBlock::Details { blocks, .. } => 1. + blocks.iter().map(block_rows).sum::<f32>(),
        DocBlock::Place(_) => 1.,
        DocBlock::Divider => 1.,
    }
}

/// A place in the file of a link: `#L5` and `#L5-L9` name the line from one.
pub fn split_line_fragment(url: &str) -> (String, u32) {
    let Some((base, fragment)) = url.rsplit_once('#') else {
        return (url.to_owned(), 0);
    };
    let digits = fragment
        .strip_prefix('L')
        .or_else(|| fragment.strip_prefix('l'))
        .map(|rest| {
            rest.chars()
                .take_while(|character| character.is_ascii_digit())
                .collect::<String>()
        })
        .unwrap_or_default();
    match digits.parse::<u32>() {
        Ok(line) if line > 0 => (base.to_owned(), line - 1),
        _ => (url.to_owned(), 0),
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{
        DocAlign, DocBlock, DocInline, HoverPlace, breaks_after, display_text, heading_size, parse,
        parse_with_lines, plain_inlines, split_line_fragment, split_place,
    };

    const HOVER: &str = "```text\npublic static String format(String format, Object... args)\n```\n\nReturns a formatted string with the **format** string and the `args` that follow it.\n\n**Parameters:**\n`format` - a [pattern](https://example.com/pattern) with `%s`\n`args` - the values\n\n**Returns:**\nthe text\n\n**Throws:**\n`IllegalState` - when the pattern is wrong\n\n<p>See <code>Formatter</code> for the rules.</p>\n\n- one\n- two\n\n---\n";

    #[test]
    fn the_answer_becomes_blocks_without_markdown_markers() {
        let blocks = parse(HOVER);

        let DocBlock::Code { language, text, .. } = &blocks[0] else {
            panic!("the signature is a code block: {blocks:?}");
        };
        assert_eq!(language.as_deref(), Some("text"));
        assert!(text.contains("public static String format"));

        let DocBlock::Paragraph(first) = &blocks[1] else {
            panic!("a paragraph follows the signature");
        };
        assert!(
            first
                .iter()
                .any(|inline| matches!(inline, DocInline::Strong(_))),
            "the bold word is a run: {first:?}"
        );
        assert!(
            first
                .iter()
                .any(|inline| matches!(inline, DocInline::Code(code) if code == "args")),
            "the inline code is a run: {first:?}"
        );

        let parameters = blocks
            .iter()
            .filter_map(|block| match block {
                DocBlock::Paragraph(inlines) => Some(plain_inlines(inlines)),
                _ => None,
            })
            .find(|text| text.contains("pattern"))
            .expect("the parameter line stays");
        assert!(parameters.contains("pattern"));
        assert!(
            parameters.contains('\n'),
            "a line of the parameters stays its own line: {parameters:?}"
        );
        assert!(
            !parameters.contains("]("),
            "the link has no marker: {parameters}"
        );

        let DocBlock::Paragraph(html) = blocks
            .iter()
            .find(|block| match block {
                DocBlock::Paragraph(inlines) => plain_inlines(inlines).contains("Formatter"),
                _ => false,
            })
            .expect("the HTML text stays")
        else {
            unreachable!()
        };
        assert_eq!(plain_inlines(html), "See Formatter for the rules.");

        assert!(
            blocks
                .iter()
                .any(|block| matches!(block, DocBlock::List { items, .. } if items.len() == 2)),
            "the list keeps its two items"
        );
        assert!(
            blocks
                .iter()
                .any(|block| matches!(block, DocBlock::Divider))
        );
    }

    #[test]
    fn a_link_keeps_its_address_and_text() {
        let blocks = parse("Read [the manual](https://example.com/manual) first.");
        let DocBlock::Paragraph(inlines) = &blocks[0] else {
            panic!("a paragraph");
        };
        let DocInline::Link { text, url } = &inlines[1] else {
            panic!("the link is a run: {inlines:?}");
        };
        assert_eq!(url, "https://example.com/manual");
        assert_eq!(plain_inlines(text), "the manual");
    }

    #[test]
    fn tags_of_a_documentation_comment_become_the_same_blocks() {
        let blocks = parse(
            "<p>One <b>bold</b> and <code>code</code>.</p><ul><li>first</li><li>second</li></ul>",
        );
        let DocBlock::Paragraph(inlines) = &blocks[0] else {
            panic!("the paragraph is first: {blocks:?}");
        };
        let text = plain_inlines(inlines);
        assert_eq!(text, "One bold and code.");
        assert!(
            inlines
                .iter()
                .any(|inline| matches!(inline, DocInline::Strong(_)))
        );
        assert!(
            inlines
                .iter()
                .any(|inline| matches!(inline, DocInline::Code(_)))
        );
        let DocBlock::List { items, .. } = &blocks[1] else {
            panic!("the list is second: {blocks:?}");
        };
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn angle_brackets_of_a_signature_are_not_tags() {
        let blocks = parse("```text\nList<String> names\n```");
        let DocBlock::Code { text, .. } = &blocks[0] else {
            panic!("a code block");
        };
        assert_eq!(text, "List<String> names");
    }

    #[test]
    fn a_file_link_line_is_counted_from_one() {
        assert_eq!(
            split_line_fragment("file:///C:/work/Other.txt#L5"),
            ("file:///C:/work/Other.txt".to_owned(), 4)
        );
        assert_eq!(
            split_line_fragment("file:///C:/work/Other.txt#L5-L9"),
            ("file:///C:/work/Other.txt".to_owned(), 4)
        );
        assert_eq!(
            split_line_fragment("https://example.com/#part"),
            ("https://example.com/#part".to_owned(), 0)
        );
    }

    #[test]
    fn a_picture_alone_in_a_paragraph_is_a_block_of_its_own() {
        let blocks = parse("![a diagram](img/a.png)\n\nSee ![an icon](img/i.svg) here.");
        let DocBlock::Image { alt, url } = &blocks[0] else {
            panic!("the picture is a block: {blocks:?}");
        };
        assert_eq!(alt, "a diagram");
        assert_eq!(url, "img/a.png");
        let DocBlock::Paragraph(inlines) = &blocks[1] else {
            panic!("a paragraph follows: {blocks:?}");
        };
        assert!(
            plain_inlines(inlines).contains("an icon"),
            "a picture inside a line keeps its text: {inlines:?}"
        );
    }

    #[test]
    fn a_picture_among_words_keeps_the_words_around_it() {
        let (blocks, _) = parse_with_lines("See ![an icon](a.png) here.\n");
        let DocBlock::Paragraph(inlines) = &blocks[0] else {
            panic!("a paragraph: {blocks:?}");
        };
        assert!(
            inlines
                .iter()
                .any(|inline| matches!(inline, DocInline::Image(_))),
            "the picture stays a picture of its own: {inlines:?}"
        );
        assert_eq!(plain_inlines(inlines), "See an icon here.");
    }

    #[test]
    fn an_html_picture_keeps_its_address_size_and_text() {
        let (blocks, _) = parse_with_lines(
            "<img width=\"1042\" height=\"20px\" alt=\"shot\" src=\"docs/a.png\">\n",
        );
        let DocBlock::Paragraph(inlines) = &blocks[0] else {
            panic!("a paragraph: {blocks:?}");
        };
        let DocInline::Image(image) = &inlines[0] else {
            panic!("the picture is a run of the line: {inlines:?}");
        };
        assert_eq!(image.alt, "shot");
        assert_eq!(image.url, "docs/a.png");
        assert_eq!(image.width, Some(1042.));
        assert_eq!(image.height, Some(20.));
    }

    #[test]
    fn a_quote_keeps_the_blocks_it_holds() {
        let blocks = parse("> quoted words\n>\n> - one\n> - two\n");
        let DocBlock::Quote(inner) = &blocks[0] else {
            panic!("the quote is a block: {blocks:?}");
        };
        assert!(matches!(&inner[0], DocBlock::Paragraph(_)));
        assert!(matches!(&inner[1], DocBlock::List { items, .. } if items.len() == 2));
    }

    #[test]
    fn a_task_item_starts_with_its_mark() {
        let blocks = parse("- [x] done\n- [ ] open\n- plain\n");
        let DocBlock::List { items, .. } = &blocks[0] else {
            panic!("a list: {blocks:?}");
        };
        assert!(plain_inlines(&items[0]).starts_with('\u{2611}'));
        assert!(plain_inlines(&items[1]).starts_with('\u{2610}'));
        assert_eq!(plain_inlines(&items[2]), "plain");
    }

    #[test]
    fn headings_of_every_level_have_their_own_size() {
        let sizes: Vec<f32> = (1..=6).map(heading_size).collect();
        let mut unique = sizes.clone();
        unique.dedup();
        assert_eq!(unique.len(), 6, "six levels, six sizes: {sizes:?}");
        assert!(
            sizes.windows(2).all(|pair| pair[0] > pair[1]),
            "a deeper heading is smaller: {sizes:?}"
        );
    }

    #[test]
    fn every_block_remembers_the_line_it_starts_on() {
        let (blocks, lines) = parse_with_lines("# One\n\ntext\n\n> quote\n\n---\n");
        assert_eq!(blocks.len(), lines.len());
        assert_eq!(lines, [0, 2, 4, 6]);
    }

    #[test]
    fn five_thousand_lines_are_parsed_without_a_blow_up() {
        let mut text = String::new();
        for index in 0..1000 {
            text.push_str(&format!(
                "## Section {index}\n\ntext with **bold** and `code`.\n\n- one\n- two\n\n"
            ));
        }
        assert!(text.lines().count() >= 5000, "the sample is a long file");
        let started = Instant::now();
        let (blocks, lines) = parse_with_lines(&text);
        let elapsed = started.elapsed();
        assert_eq!(blocks.len(), lines.len());
        // The parse runs off the window thread; this bound only catches a blow-up. A
        // debug build takes about 0.8 s on this machine, a release build less.
        assert!(
            elapsed < Duration::from_secs(3),
            "a 5000-line file took {elapsed:?}"
        );
    }

    #[test]
    fn a_comment_with_tags_inside_is_not_drawn() {
        let (blocks, _) = parse_with_lines(
            "<!--\n | <groupId>1.2</groupId> | math |\n +========+\n --->\n# Title\n\nText <!-- hidden --> here.\n",
        );
        let text = super::display_text(&blocks, None);
        assert!(
            !text.contains("math") && !text.contains("<groupId>") && !text.contains("hidden"),
            "the comments stay out of the text: {text:?}"
        );
        assert!(
            matches!(&blocks[0], DocBlock::Heading { level: 1, .. }),
            "the heading after the comment is the first block: {blocks:?}"
        );
    }

    #[test]
    fn badges_on_consecutive_lines_stay_in_one_paragraph() {
        let (blocks, _) = parse_with_lines(
            "[![CI](https://example.com/ci.svg)](https://example.com/ci)\n[![Docs](https://example.com/docs.svg)](https://example.com/docs)\n",
        );
        assert_eq!(blocks.len(), 1, "one paragraph: {blocks:?}");
        let DocBlock::Paragraph(inlines) = &blocks[0] else {
            panic!("a paragraph: {blocks:?}");
        };
        let badges = inlines
            .iter()
            .filter(|inline| {
                matches!(
                    inline,
                    DocInline::Link { text, .. } if matches!(text.as_slice(), [DocInline::Image(_)])
                )
            })
            .count();
        assert_eq!(badges, 2, "both badges are runs of the line: {inlines:?}");
        assert!(
            !plain_inlines(inlines).contains('\n'),
            "no line break between the badges: {inlines:?}"
        );
    }

    #[test]
    fn a_soft_line_break_is_a_space_and_a_hard_one_is_a_new_line() {
        let (blocks, _) = parse_with_lines("First line\nsecond line\n\nthree  \nfour\\\nfive\n");
        let DocBlock::Paragraph(first) = &blocks[0] else {
            panic!("a paragraph: {blocks:?}");
        };
        assert_eq!(plain_inlines(first), "First line second line");
        let DocBlock::Paragraph(second) = &blocks[1] else {
            panic!("a paragraph: {blocks:?}");
        };
        assert_eq!(plain_inlines(second), "three\nfour\nfive");
    }

    #[test]
    fn windows_line_ends_read_like_unix_ones() {
        let (blocks, _) = parse_with_lines("First line\r\nsecond line\r\n\r\n# Next\r\n");
        let DocBlock::Paragraph(first) = &blocks[0] else {
            panic!("a paragraph: {blocks:?}");
        };
        assert_eq!(plain_inlines(first), "First line second line");
        assert!(
            matches!(&blocks[1], DocBlock::Heading { level: 1, .. }),
            "{blocks:?}"
        );
    }

    #[test]
    fn a_nested_list_stays_under_the_line_it_belongs_to() {
        let (blocks, _) = parse_with_lines("- one\n  - inner\n  - more\n- two\n");
        let DocBlock::List { items, nested, .. } = &blocks[0] else {
            panic!("a list: {blocks:?}");
        };
        assert_eq!(items.len(), 2);
        assert_eq!(plain_inlines(&items[0]), "one");
        assert_eq!(plain_inlines(&items[1]), "two");
        let DocBlock::List { items: inner, .. } = &nested[0][0] else {
            panic!("the nested list is under the first item: {nested:?}");
        };
        assert_eq!(inner.len(), 2);
        assert_eq!(plain_inlines(&inner[1]), "more");
        assert!(nested[1].is_empty());
    }

    #[test]
    fn a_code_block_keeps_the_language_it_names() {
        let (blocks, _) = parse_with_lines("```xml\n<a/>\n```\n");
        let DocBlock::Code { language, .. } = &blocks[0] else {
            panic!("a code block: {blocks:?}");
        };
        assert_eq!(language.as_deref(), Some("xml"));
    }

    #[test]
    fn a_line_break_tag_is_a_new_line() {
        let (blocks, _) = parse_with_lines("one<br>two\n");
        let DocBlock::Paragraph(first) = &blocks[0] else {
            panic!("a paragraph: {blocks:?}");
        };
        assert_eq!(plain_inlines(first), "one\ntwo");
    }

    #[test]
    fn a_details_tag_is_a_block_that_opens_and_closes() {
        let (blocks, lines) = parse_with_lines(
            "<details>\n<summary>More</summary>\n\nHidden **words**\n\n</details>\n\nAfter\n",
        );
        assert_eq!(blocks.len(), lines.len());
        let DocBlock::Details {
            id,
            open,
            summary,
            blocks: body,
        } = &blocks[0]
        else {
            panic!("a details block: {blocks:?}");
        };
        assert_eq!((*id, *open), (0, false), "the first block is closed");
        assert_eq!(plain_inlines(summary), "More");
        assert_eq!(plain_inlines_of(&body[0]), "Hidden words");
        assert_eq!(plain_inlines_of(&blocks[1]), "After");
        assert_eq!(lines, [0, 7], "the block after it keeps its line");
    }

    #[test]
    fn a_details_tag_on_one_line_holds_its_text_and_may_start_open() {
        let (blocks, _) = parse_with_lines(
            "<details open><summary>Open</summary>Body text</details>\n\n<details>\n<summary>Closed</summary>\n\nLater text\n\n</details>\n",
        );
        let DocBlock::Details {
            open,
            blocks: first,
            ..
        } = &blocks[0]
        else {
            panic!("a details block: {blocks:?}");
        };
        assert!(*open, "the open attribute opens the block");
        assert_eq!(plain_inlines_of(&first[0]), "Body text");
        let DocBlock::Details {
            id,
            open,
            blocks: second,
            ..
        } = &blocks[1]
        else {
            panic!("a second details block: {blocks:?}");
        };
        assert_eq!((*id, *open), (1, false));
        assert_eq!(plain_inlines_of(&second[0]), "Later text");
    }

    #[test]
    fn a_closing_tag_and_the_next_opening_tag_can_share_an_html_block() {
        let (blocks, lines) = parse_with_lines(
            "<details>\n<summary>A</summary>\n\none\n\n</details>\n<details>\n<summary>B</summary>\n\ntwo\n\n</details>\n",
        );
        assert_eq!(blocks.len(), 2, "two blocks: {blocks:?}");
        assert_eq!(lines, [0, 5]);
        let DocBlock::Details {
            summary: first,
            blocks: first_body,
            ..
        } = &blocks[0]
        else {
            panic!("the first block: {blocks:?}");
        };
        assert_eq!(plain_inlines(first), "A");
        assert_eq!(plain_inlines_of(&first_body[0]), "one");
        let DocBlock::Details {
            id,
            summary: second,
            blocks: second_body,
            ..
        } = &blocks[1]
        else {
            panic!("the second block: {blocks:?}");
        };
        assert_eq!(*id, 1);
        assert_eq!(plain_inlines(second), "B");
        assert_eq!(plain_inlines_of(&second_body[0]), "two");
    }

    #[test]
    fn a_reference_takes_the_address_of_its_definition_and_the_definition_is_not_drawn() {
        let (blocks, _) = parse_with_lines(
            "See [the manual][REF], [home][] and [docs] and ![logo][Logo].\n\n[ref]: https://example.com/manual \"Title\"\n[home]: https://example.com/home\n[docs]: https://example.com/docs\n[logo]: logo.png\n",
        );
        assert_eq!(
            blocks.len(),
            1,
            "the definitions are not blocks: {blocks:?}"
        );
        let DocBlock::Paragraph(inlines) = &blocks[0] else {
            panic!("a paragraph: {blocks:?}");
        };
        let urls: Vec<&str> = inlines
            .iter()
            .filter_map(|inline| match inline {
                DocInline::Link { url, .. } => Some(url.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            urls,
            [
                "https://example.com/manual",
                "https://example.com/home",
                "https://example.com/docs"
            ]
        );
        assert!(
            inlines
                .iter()
                .any(|inline| matches!(inline, DocInline::Image(image) if image.url == "logo.png")),
            "the picture takes the address too: {inlines:?}"
        );
        assert!(
            !plain_inlines(inlines).contains("example.com"),
            "the addresses are not drawn: {inlines:?}"
        );
    }

    #[test]
    fn a_table_keeps_the_alignment_of_each_column_and_the_markup_of_its_cells() {
        let (blocks, _) = parse_with_lines(
            "| Name | Count | Note | Plain |\n| :--- | :---: | ---: | --- |\n| `code` | **bold** | [site](https://example.com) | text |\n",
        );
        let DocBlock::Table {
            alignments,
            header,
            rows,
        } = &blocks[0]
        else {
            panic!("a table: {blocks:?}");
        };
        assert_eq!(
            alignments.as_slice(),
            &[
                DocAlign::Left,
                DocAlign::Center,
                DocAlign::Right,
                DocAlign::Left
            ]
        );
        assert_eq!(plain_inlines(&header[1]), "Count");
        let row = &rows[0];
        assert!(matches!(row[0].as_slice(), [DocInline::Code(code)] if code == "code"));
        assert!(matches!(row[1].as_slice(), [DocInline::Strong(_)]));
        assert!(
            matches!(row[2].as_slice(), [DocInline::Link { url, .. }] if url == "https://example.com")
        );
    }

    #[test]
    fn a_bulleted_item_takes_the_mark_of_its_depth_and_a_number_keeps_its_start() {
        let (blocks, _) = parse_with_lines("- one\n  - two\n    - three\n\n3. third\n4. fourth\n");
        assert_eq!(
            display_text(&blocks, None),
            "\u{2022} one\n\u{25e6} two\n\u{25aa} three\n3. third\n4. fourth\n"
        );
        let DocBlock::List { ordered, start, .. } = &blocks[1] else {
            panic!("the numbered list: {blocks:?}");
        };
        assert!(*ordered);
        assert_eq!(*start, 3);
    }

    #[test]
    fn a_horizontal_rule_tag_is_a_divider() {
        let (blocks, _) = parse_with_lines("above\n\n<hr>\n\nbelow\n");
        assert!(
            matches!(blocks[1], DocBlock::Divider),
            "the rule is a divider: {blocks:?}"
        );
    }

    #[test]
    fn an_html_heading_is_a_heading_and_its_entities_are_decoded() {
        let (blocks, _) = parse_with_lines("<h2 align=\"center\">Title &amp; more</h2>\n");
        let DocBlock::Heading { level, text } = &blocks[0] else {
            panic!("a heading: {blocks:?}");
        };
        assert_eq!(*level, 2);
        assert_eq!(plain_inlines(text), "Title & more");
    }

    #[test]
    fn an_anchor_around_a_picture_is_a_link() {
        let (blocks, _) = parse_with_lines(
            "<p align=\"center\"><a href=\"https://example.com/a\"><img src=\"a.svg\" alt=\"A\"></a> <a href=\"https://example.com/b\"><img src=\"b.svg\" alt=\"B\"></a></p>\n",
        );
        let DocBlock::Paragraph(inlines) = &blocks[0] else {
            panic!("a paragraph: {blocks:?}");
        };
        let urls: Vec<&str> = inlines
            .iter()
            .filter_map(|inline| match inline {
                DocInline::Link { text, url }
                    if matches!(text.as_slice(), [DocInline::Image(_)]) =>
                {
                    Some(url.as_str())
                }
                _ => None,
            })
            .collect();
        assert_eq!(urls, ["https://example.com/a", "https://example.com/b"]);
    }

    /// A split that takes the text after a dash as the place, the way a language does.
    fn split_after_dash(line: &str) -> Option<HoverPlace> {
        let (declaration, place) = line.split_once(" - ")?;
        Some(HoverPlace {
            declaration: declaration.to_owned(),
            place: place.to_owned(),
        })
    }

    #[test]
    fn the_place_of_a_declaration_becomes_a_line_of_its_own() {
        let blocks = split_place(
            parse("```text\nPage<Owner> ownersResults - OwnerController.process()\n```\n"),
            split_after_dash,
        );

        let DocBlock::Code { text, spans, .. } = &blocks[0] else {
            panic!("the declaration is a code block: {blocks:?}");
        };
        assert_eq!(text, "Page<Owner> ownersResults");
        assert!(
            spans.is_empty(),
            "the colours are read again for the new text"
        );
        assert_eq!(
            blocks[1],
            DocBlock::Place("in OwnerController.process()".to_owned())
        );
        assert_eq!(
            display_text(&blocks, None),
            "Page<Owner> ownersResults\nin OwnerController.process()\n"
        );
    }

    #[test]
    fn a_declaration_without_a_place_keeps_its_block() {
        let before = parse("```text\nString name\n```\n");
        assert_eq!(split_place(before.clone(), |_| None), before);
    }

    #[test]
    fn a_code_block_of_several_lines_is_not_split() {
        let before = parse("```text\nvoid run() - a\nvoid stop()\n```\n");
        assert_eq!(split_place(before.clone(), split_after_dash), before);
    }

    #[test]
    fn a_line_breaks_after_a_separator_that_a_word_follows() {
        assert!(breaks_after(' ', Some('x')));
        assert!(breaks_after('.', Some('x')));
        assert!(breaks_after(',', Some('x')));
        assert!(breaks_after('(', Some('x')));
        assert!(
            !breaks_after(' ', Some(' ')),
            "a run of spaces stays together"
        );
        assert!(
            !breaks_after(',', Some(' ')),
            "a comma keeps the space after it"
        );
        assert!(!breaks_after('x', Some('y')));
        assert!(!breaks_after(' ', None), "the end of a line is no break");
    }

    /// The plain text of a paragraph block, for the tests of the blocks after the first.
    fn plain_inlines_of(block: &DocBlock) -> String {
        match block {
            DocBlock::Paragraph(inlines) => plain_inlines(inlines),
            other => panic!("a paragraph: {other:?}"),
        }
    }
}
