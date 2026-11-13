//! Quick documentation: the markdown answer of a language server as blocks and the
//! popup that shows them. The answer comes as it is, with markdown markers and the
//! HTML that documentation comments use; this module turns it into neutral blocks and
//! draws them in the colours of the editor.

use std::ops::Range;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures_util::StreamExt;
use lang_api::{SourceSpan, TextPoint, TextSpan, ask};
use markdown::mdast;
use syntax::{DocumentId, HighlightSpan};
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
    Link { text: Vec<DocInline>, url: String },
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
    let normalized = html_to_markdown(text);
    let Ok(root) = markdown::to_mdast(&normalized, &markdown::ParseOptions::gfm()) else {
        return vec![DocBlock::Paragraph(vec![DocInline::Text(
            text.trim().to_owned(),
        )])];
    };
    let mdast::Node::Root(root) = root else {
        return Vec::new();
    };
    let mut blocks = Vec::new();
    push_blocks(&root.children, &mut blocks);
    blocks
}

fn push_blocks(nodes: &[mdast::Node], blocks: &mut Vec<DocBlock>) {
    for node in nodes {
        match node {
            mdast::Node::Paragraph(paragraph) => {
                let inlines = inlines_of(&paragraph.children);
                if !inlines.is_empty() {
                    blocks.push(DocBlock::Paragraph(inlines));
                }
            }
            mdast::Node::Heading(heading) => {
                let text = inlines_of(&heading.children);
                if !text.is_empty() {
                    blocks.push(DocBlock::Heading {
                        level: usize::from(heading.depth).clamp(1, 6),
                        text,
                    });
                }
            }
            mdast::Node::List(list) => {
                let mut items = Vec::new();
                for child in &list.children {
                    let mdast::Node::ListItem(item) = child else {
                        continue;
                    };
                    let mut inlines = Vec::new();
                    collect_item(&item.children, &mut inlines);
                    if !inlines.is_empty() {
                        items.push(inlines);
                    }
                }
                if !items.is_empty() {
                    blocks.push(DocBlock::List {
                        ordered: list.ordered,
                        start: list.start.unwrap_or(1),
                        items,
                    });
                }
            }
            mdast::Node::Code(code) => blocks.push(DocBlock::Code {
                language: code.lang.clone(),
                text: code.value.clone(),
                spans: Vec::new(),
            }),
            mdast::Node::Table(table) => {
                if let Some(block) = table_block(table) {
                    blocks.push(block);
                }
            }
            mdast::Node::ThematicBreak(_) => blocks.push(DocBlock::Divider),
            mdast::Node::Blockquote(quote) => push_blocks(&quote.children, blocks),
            mdast::Node::Html(html) => {
                let text = strip_tags(&html.value);
                let text = text.trim();
                if !text.is_empty() {
                    blocks.push(DocBlock::Paragraph(vec![DocInline::Text(text.to_owned())]));
                }
            }
            _ => {}
        }
    }
}

/// A table of the answer: the first row is the header, the alignment of a column comes
/// from the separators under it.
fn table_block(table: &mdast::Table) -> Option<DocBlock> {
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
            cells.push(inlines_of(&cell.children));
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

/// A list item whose child is not a paragraph, such as a nested list, keeps its text in
/// the same line rather than making another block.
fn collect_item(nodes: &[mdast::Node], inlines: &mut Vec<DocInline>) {
    for node in nodes {
        match node {
            mdast::Node::Paragraph(paragraph) => merge(inlines, inlines_of(&paragraph.children)),
            mdast::Node::List(list) => {
                for child in &list.children {
                    if let mdast::Node::ListItem(item) = child {
                        collect_item(&item.children, inlines);
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

fn inlines_of(nodes: &[mdast::Node]) -> Vec<DocInline> {
    let mut inlines = Vec::new();
    for node in nodes {
        push_inline(node, &mut inlines);
    }
    inlines
}

fn push_inline(node: &mdast::Node, out: &mut Vec<DocInline>) {
    match node {
        mdast::Node::Text(text) => merge(out, vec![DocInline::Text(text.value.clone())]),
        mdast::Node::Strong(strong) => {
            merge(out, vec![DocInline::Strong(inlines_of(&strong.children))])
        }
        mdast::Node::Emphasis(emphasis) => merge(
            out,
            vec![DocInline::Emphasis(inlines_of(&emphasis.children))],
        ),
        mdast::Node::Delete(delete) => {
            merge(out, vec![DocInline::Emphasis(inlines_of(&delete.children))])
        }
        mdast::Node::InlineCode(code) => merge(out, vec![DocInline::Code(code.value.clone())]),
        mdast::Node::Link(link) => merge(
            out,
            vec![DocInline::Link {
                text: inlines_of(&link.children),
                url: link.url.clone(),
            }],
        ),
        mdast::Node::LinkReference(link) => merge(
            out,
            vec![DocInline::Link {
                text: inlines_of(&link.children),
                url: String::new(),
            }],
        ),
        mdast::Node::Image(image) => {
            merge(out, vec![DocInline::Text(image.alt.clone())]);
        }
        mdast::Node::Break(_) => merge(out, vec![DocInline::Text("\n".to_owned())]),
        mdast::Node::Html(html) => {
            let text = strip_tags(&html.value);
            if !text.is_empty() {
                merge(out, vec![DocInline::Text(text)]);
            }
        }
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

/// The plain text of the answer, for the copy button and for tests.
pub(crate) fn plain_text(doc: &QuickDoc) -> String {
    let mut out = String::new();
    if let Some(diagnostic) = &doc.diagnostic {
        out.push_str(diagnostic);
        out.push_str("\n\n");
    }
    for block in &doc.blocks {
        match block {
            DocBlock::Paragraph(inlines) => {
                out.push_str(&plain_inlines(inlines));
                out.push('\n');
            }
            DocBlock::Heading { text, .. } => {
                out.push_str(&plain_inlines(text));
                out.push('\n');
            }
            DocBlock::List { items, .. } => {
                for item in items {
                    out.push_str("- ");
                    out.push_str(&plain_inlines(item));
                    out.push('\n');
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
            DocBlock::Divider => out.push('\n'),
        }
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
            } => {
                for (index, item) in items.iter().enumerate() {
                    out.push_str(&bullet_of(*ordered, *start, index));
                    out.push(' ');
                    out.push_str(&plain_inlines(item));
                    out.push('\n');
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
            DocBlock::Divider => out.push('\n'),
        }
    }
    out
}

fn bullet_of(ordered: bool, start: u32, index: usize) -> String {
    if ordered {
        format!("{}.", start + index as u32)
    } else {
        "\u{2022}".to_owned()
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

fn plain_inlines(inlines: &[DocInline]) -> String {
    let mut out = String::new();
    for inline in inlines {
        match inline {
            DocInline::Text(text) | DocInline::Code(text) => out.push_str(text),
            DocInline::Strong(children) | DocInline::Emphasis(children) => {
                out.push_str(&plain_inlines(children));
            }
            DocInline::Link { text, .. } => out.push_str(&plain_inlines(text)),
        }
    }
    out
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

/// The text without tags, for an HTML block the markdown parser kept whole.
fn strip_tags(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    while let Some(open) = text[at..].find('<') {
        out.push_str(&text[at..at + open]);
        let Some(close) = text[at + open..].find('>') else {
            out.push_str(&text[at + open..]);
            return out;
        };
        at += open + close + 1;
    }
    out.push_str(&text[at..]);
    out
}

/// One piece of a rendered line, already split by the emphasis around it.
struct Piece {
    text: String,
    bold: bool,
    italic: bool,
    code: bool,
    url: Option<String>,
    link: Option<usize>,
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
        DocInline::Text(text) => out.push(Piece {
            text: text.clone(),
            bold: style.bold,
            italic: style.italic,
            code: style.code,
            url: style.url.map(str::to_owned),
            link: style.link,
        }),
        DocInline::Code(text) => out.push(Piece {
            text: text.clone(),
            bold: style.bold,
            italic: style.italic,
            code: true,
            url: style.url.map(str::to_owned),
            link: style.link,
        }),
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
                let id = DocumentId(NEXT_SOURCE.fetch_add(1, Ordering::Relaxed));
                let (sender, mut receiver) = futures_channel::mpsc::unbounded();
                syntax.open(id, language.clone(), move |snapshot| {
                    sender.unbounded_send(snapshot).ok();
                });
                syntax.update(id, 1, RopeStore::from_text(&text), &Change::default());
                let snapshot = receiver.next().await;
                syntax.close(id);
                let Some(snapshot) = snapshot else {
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
                        *spans = snapshot.highlights;
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
        if position.x < layout.bounds.left() + px(crate::element::GUTTER_WIDTH)
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
                let blocks = parse(&text);
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
        if !uri.starts_with("file:") && !uri.starts_with("jdt:") {
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
        };
        let content = div()
            .flex()
            .flex_col()
            .w_full()
            .gap(px(SPACE_S))
            .p(px(SPACE_M))
            .pr(px(ICON_BUTTON * 2.))
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
        let frame = surface(&theme)
            .id("quick-doc-frame")
            .debug_selector(|| "quick-doc".to_owned())
            .occlude()
            .relative()
            .w(px(MAX_WIDTH))
            .min_w(px(MIN_WIDTH))
            .max_w(px(MAX_WIDTH))
            .max_h(px(half))
            .flex()
            .flex_col()
            .child(body)
            .child(
                div()
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

/// What a rendered answer asks of the place that shows it: where a link goes and how a
/// character of the text starts, moves or ends a selection.
pub struct DocHost {
    pub name: String,
    pub on_link: DocLinkHandler,
    pub on_select: Option<DocSelectHandler>,
    pub selection: Option<Range<usize>>,
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
    let mut state = DocRender {
        style,
        theme,
        host,
        cursor: 0,
        next_link: 0,
        link_tokens: Vec::new(),
        table: 0,
        word: 0,
    };
    let mut column = div().flex().flex_col().w_full().gap(px(SPACE_S));
    if let Some(diagnostic) = diagnostic {
        state.cursor += diagnostic.len();
        state.cursor += 2;
        let inlines = [DocInline::Text(diagnostic.to_owned())];
        column = column.child(
            div()
                .debug_selector(|| format!("{}-diagnostic", host.name))
                .w_full()
                .rounded(px(4.))
                .bg(theme.selection_muted)
                .p(px(SPACE_S))
                .text_size(px(UI_SIZE))
                .text_color(theme.stripe_error)
                .child(state.inlines(&inlines, UI_SIZE, theme.stripe_error, false, true)),
        );
    }
    for block in blocks {
        column = column.child(state.block(block, window, cx));
    }
    column.into_any_element()
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
                let size = heading_size(*level);
                let element = self
                    .inlines(text, size, theme.text, true, true)
                    .font_weight(FontWeight::SEMIBOLD)
                    .into_any_element();
                self.cursor += 1;
                element
            }
            DocBlock::List {
                ordered,
                start,
                items,
            } => {
                let mut list = div().flex().flex_col().gap(px(SPACE_XS)).w_full();
                for (index, item) in items.iter().enumerate() {
                    let bullet = bullet_of(*ordered, *start, index);
                    let bullet =
                        self.plain_run(&format!("{bullet} "), UI_SIZE, theme.text_secondary);
                    let row = self.inlines(item, UI_SIZE, theme.code, false, true);
                    list = list.child(
                        div()
                            .flex()
                            .items_start()
                            .gap(px(SPACE_S))
                            .w_full()
                            .child(bullet)
                            .child(div().flex_1().min_w_0().child(row)),
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
                let mut column = div()
                    .flex()
                    .flex_col()
                    .w_full()
                    .rounded(px(4.))
                    .bg(theme.selection_muted)
                    .p(px(SPACE_S))
                    .font_family(style.font_family.clone())
                    .text_size(px(style.font_size));
                if language.as_deref().is_some_and(|name| !name.is_empty()) {
                    column = column.debug_selector(|| format!("{}-code", self.host.name));
                }
                let mut at = 0;
                for line in text.split_inclusive('\n') {
                    let content = line.strip_suffix('\n').unwrap_or(line);
                    let end = at + content.len();
                    let mut cells = Vec::new();
                    for (index, character) in content.char_indices() {
                        let offset = self.cursor;
                        self.cursor += character.len_utf8();
                        let color = code_color(at + index, spans, style);
                        cells.push(self.character(
                            character,
                            offset,
                            color,
                            style.font_size,
                            false,
                            false,
                            true,
                        ));
                    }
                    self.cursor += 1;
                    let row = if cells.is_empty() {
                        div().flex().h(px(style.line_height)).into_any_element()
                    } else {
                        div()
                            .flex()
                            .whitespace_nowrap()
                            .children(cells)
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
            } => self.table(alignments, header, rows, window, cx),
            DocBlock::Divider => {
                self.cursor += 1;
                div().w_full().h(px(1.)).bg(theme.border).into_any_element()
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

    fn inlines(
        &mut self,
        inlines: &[DocInline],
        size: f32,
        color: Hsla,
        bold: bool,
        wrap: bool,
    ) -> Div {
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
        let mut column = div().flex().flex_col().w_full();
        for line in lines {
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
        _ => 14.,
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

/// A rough height of the popup before layout, for placing it above or below the line.
fn estimate_height(doc: &QuickDoc, line_height: f32) -> f32 {
    let mut rows = 0.;
    if let Some(diagnostic) = &doc.diagnostic {
        rows += 1. + (diagnostic.len() / 70) as f32;
    }
    for block in &doc.blocks {
        rows += match block {
            DocBlock::Paragraph(inlines) => {
                let text = plain_inlines(inlines);
                (1 + text.len() / 70) as f32
            }
            DocBlock::Heading { .. } => 1.5,
            DocBlock::List { items, .. } => items.len() as f32,
            DocBlock::Code { text, .. } => text.lines().count().max(1) as f32,
            DocBlock::Table { header, rows, .. } => (header.len().max(1) + rows.len()) as f32 * 1.6,
            DocBlock::Divider => 1.,
        };
    }
    rows * line_height.max(16.) + 24.
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
    use super::{DocBlock, DocInline, parse, plain_inlines, split_line_fragment};

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
}
