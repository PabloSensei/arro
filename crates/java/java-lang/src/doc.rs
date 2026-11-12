//! Documentation comments of a source file and the blocks their text renders to.

use lang_api::{DocumentationBlock, DocumentationComment, DocumentationInline};
use tree_sitter::{Node, Parser};

/// Every documentation comment of `text`, in the order of the file.
pub fn documentation_comments(text: &str) -> Vec<DocumentationComment> {
    let mut parser = Parser::new();
    if parser.set_language(&java_syntax::config().grammar).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(text, None) else {
        return Vec::new();
    };
    let mut comments = Vec::new();
    collect(tree.root_node(), text, &mut comments);
    comments
}

fn collect(node: Node<'_>, text: &str, comments: &mut Vec<DocumentationComment>) {
    if node.kind() == "block_comment" {
        let source = &text[node.byte_range()];
        if source.starts_with("/**") && source.len() > 4 {
            comments.push(DocumentationComment {
                range: node.byte_range(),
                blocks: blocks(source),
            });
        }
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect(child, text, comments);
    }
}

/// The body of a documentation comment as blocks: paragraphs, lists, code and tags.
fn blocks(comment: &str) -> Vec<DocumentationBlock> {
    let body = comment
        .strip_prefix("/**")
        .and_then(|body| body.strip_suffix("*/"))
        .unwrap_or(comment);
    let lines: Vec<String> = body.lines().map(clean_line).collect();
    blocks_of_lines(&lines)
}

fn clean_line(line: &str) -> String {
    let trimmed = line.trim_start();
    let trimmed = trimmed.strip_prefix('*').unwrap_or(trimmed);
    let trimmed = trimmed.strip_prefix(' ').unwrap_or(trimmed);
    trimmed.trim_end().to_owned()
}

fn blocks_of_lines(lines: &[String]) -> Vec<DocumentationBlock> {
    let mut blocks = Vec::new();
    let mut at = 0;
    while at < lines.len() {
        let line = lines[at].trim();
        if line.is_empty() {
            at += 1;
            continue;
        }
        if line.starts_with('@') {
            let (block, next) = tag_block(lines, at);
            blocks.push(block);
            at = next;
            continue;
        }
        if starts_list(line) {
            let (block, next) = list_block(lines, at);
            if let Some(block) = block {
                blocks.push(block);
            }
            at = next;
            continue;
        }
        if line.starts_with("<pre") {
            let (block, next) = code_block(lines, at);
            if let Some(block) = block {
                blocks.push(block);
            }
            at = next;
            continue;
        }
        let mut paragraph = String::new();
        while at < lines.len() {
            let line = lines[at].trim();
            if line.is_empty()
                || line.starts_with('@')
                || starts_list(line)
                || line.starts_with("<pre")
            {
                break;
            }
            if !paragraph.is_empty() {
                paragraph.push(' ');
            }
            paragraph.push_str(line);
            at += 1;
        }
        push_paragraphs(&paragraph, &mut blocks);
    }
    blocks
}

fn starts_list(line: &str) -> bool {
    let line = line.to_ascii_lowercase();
    line.starts_with("<ul") || line.starts_with("<ol")
}

fn tag_block(lines: &[String], at: usize) -> (DocumentationBlock, usize) {
    let first = lines[at].trim();
    let body = first.trim_start_matches('@');
    let (name, rest) = split_word(body);
    let name = name.to_owned();
    let mut parts = vec![rest.trim().to_owned()];
    let mut next = at + 1;
    while next < lines.len() {
        let line = lines[next].trim();
        if line.is_empty() || line.starts_with('@') || starts_list(line) {
            break;
        }
        parts.push(line.to_owned());
        next += 1;
    }
    let description = parts.join(" ").trim().to_owned();
    let (subject, description) = match name.as_str() {
        "param" | "throws" | "exception" => {
            let (subject, rest) = split_word(&description);
            (!subject.is_empty()).then(|| (subject.to_owned(), rest.trim().to_owned()))
        }
        _ => None,
    }
    .map_or((None, description), |(subject, rest)| (Some(subject), rest));
    (
        DocumentationBlock::Tag {
            name,
            subject,
            description: inline(&description),
        },
        next,
    )
}

fn list_block(lines: &[String], at: usize) -> (Option<DocumentationBlock>, usize) {
    let first = lines[at].trim();
    let ordered = first.to_ascii_lowercase().starts_with("<ol");
    let closing = if ordered { "</ol" } else { "</ul" };
    let mut source = String::new();
    let mut next = at;
    while next < lines.len() {
        let line = lines[next].trim();
        source.push_str(line);
        source.push('\n');
        next += 1;
        if line.to_ascii_lowercase().contains(closing) {
            break;
        }
    }
    let mut items = Vec::new();
    let mut rest = source.as_str();
    while let Some(found) = rest.to_ascii_lowercase().find("<li") {
        rest = &rest[found + 3..];
        let body = rest.find('>').map_or(rest, |end| &rest[end + 1..]);
        let end = body.to_ascii_lowercase().find("</li").unwrap_or(body.len());
        let item = body[..end].trim();
        if !item.is_empty() {
            items.push(inline(item));
        }
        rest = &body[end..];
    }
    let block = (!items.is_empty()).then_some(DocumentationBlock::List {
        ordered,
        start: 1,
        items,
    });
    (block, next)
}

fn code_block(lines: &[String], at: usize) -> (Option<DocumentationBlock>, usize) {
    let mut source = String::new();
    let mut next = at;
    while next < lines.len() {
        let line = lines[next].trim();
        source.push_str(line.strip_prefix("<pre").unwrap_or(line));
        source.push('\n');
        next += 1;
        if line.to_ascii_lowercase().contains("</pre") {
            break;
        }
    }
    let text = source
        .strip_prefix('>')
        .unwrap_or(&source)
        .replace("</pre>", "")
        .trim_matches('\n')
        .to_owned();
    let block = (!text.trim().is_empty()).then_some(DocumentationBlock::Code {
        language: None,
        text,
    });
    (block, next)
}

/// A paragraph may carry `<p>` separators; each part becomes a block of its own.
fn push_paragraphs(text: &str, blocks: &mut Vec<DocumentationBlock>) {
    let mut rest = text;
    while let Some(open) = rest.to_ascii_lowercase().find("<p") {
        push_paragraph(&rest[..open], blocks);
        rest = rest[open..]
            .find('>')
            .map_or("", |end| &rest[open + end + 1..]);
    }
    push_paragraph(rest, blocks);
}

fn push_paragraph(text: &str, blocks: &mut Vec<DocumentationBlock>) {
    let text = text.replace("</p>", "");
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    let inlines = inline(text);
    if !inlines.is_empty() {
        blocks.push(DocumentationBlock::Paragraph(inlines));
    }
}

fn split_word(text: &str) -> (&str, &str) {
    let text = text.trim_start();
    let end = text
        .char_indices()
        .find(|(_, character)| character.is_whitespace())
        .map_or(text.len(), |(at, _)| at);
    (&text[..end], &text[end..])
}

fn inline(text: &str) -> Vec<DocumentationInline> {
    let mut parser = InlineParser {
        text,
        at: 0,
        out: Vec::new(),
    };
    parser.parse(None);
    parser.out
}

struct InlineParser<'a> {
    text: &'a str,
    at: usize,
    out: Vec<DocumentationInline>,
}

impl InlineParser<'_> {
    /// Reads up to `stop` (a closing tag) or the end. `None` reads the whole text.
    fn parse(&mut self, stop: Option<&str>) {
        let mut plain = String::new();
        while self.at < self.text.len() {
            let rest = &self.text[self.at..];
            if let Some(stop) = stop
                && rest.to_ascii_lowercase().starts_with(stop)
            {
                self.at += stop.len();
                break;
            }
            if rest.starts_with("{@") {
                flush(&mut plain, &mut self.out);
                let end = rest.find('}').map_or(rest.len(), |end| end + 1);
                self.at += end;
                self.code_or_link(&rest[..end]);
                continue;
            }
            if rest.starts_with('<') {
                if let Some(used) = self.tag(rest) {
                    flush(&mut plain, &mut self.out);
                    self.at += used;
                    continue;
                }
                // An unknown tag stays out of the text.
                if let Some(end) = rest.find('>') {
                    self.at += end + 1;
                    continue;
                }
            }
            let Some(character) = rest.chars().next() else {
                break;
            };
            plain.push(character);
            self.at += character.len_utf8();
        }
        flush(&mut plain, &mut self.out);
    }

    fn code_or_link(&mut self, token: &str) {
        let body = token
            .strip_prefix("{@")
            .and_then(|body| body.strip_suffix('}'))
            .unwrap_or(token)
            .trim();
        let (kind, body) = split_word(body);
        let body = body.trim();
        match kind {
            "code" | "value" | "snippet" => {
                self.out
                    .push(DocumentationInline::Code(unescape(body).to_owned()));
            }
            "literal" => self
                .out
                .push(DocumentationInline::Text(unescape(body).to_owned())),
            "link" | "linkplain" => {
                let (target, label) = split_word(body);
                let text = if label.trim().is_empty() {
                    target.to_owned()
                } else {
                    unescape(label.trim()).to_owned()
                };
                self.out.push(DocumentationInline::Link {
                    text,
                    target: target.to_owned(),
                });
            }
            _ => {
                let text = format!("{{@{body}}}");
                self.out.push(DocumentationInline::Text(text));
            }
        }
    }

    /// Reads one HTML tag that carries meaning. The bytes used, or `None` for a tag
    /// this parser does not know.
    fn tag(&mut self, rest: &str) -> Option<usize> {
        let end = rest.find('>')?;
        let inner = rest[1..end].trim();
        let closing = inner.starts_with('/');
        let body = inner.trim_start_matches('/').trim();
        let name: String = body
            .chars()
            .take_while(|character| character.is_ascii_alphanumeric())
            .collect::<String>()
            .to_ascii_lowercase();
        let lower = name.as_str();
        match lower {
            "br" => {
                self.out.push(DocumentationInline::Text("\n".to_owned()));
                Some(end + 1)
            }
            "p" | "div" if !closing => {
                self.out.push(DocumentationInline::Text("\n\n".to_owned()));
                Some(end + 1)
            }
            "b" | "strong" | "i" | "em" if !closing => {
                let (stop, alt) = match lower {
                    "b" => ("</b>", "</strong>"),
                    "strong" => ("</strong>", "</b>"),
                    "i" => ("</i>", "</em>"),
                    _ => ("</em>", "</i>"),
                };
                let mut inner_parser = InlineParser {
                    text: &rest[end + 1..],
                    at: 0,
                    out: Vec::new(),
                };
                inner_parser.parse(Some(stop));
                if inner_parser.at == 0 {
                    inner_parser = InlineParser {
                        text: &rest[end + 1..],
                        at: 0,
                        out: Vec::new(),
                    };
                    inner_parser.parse(Some(alt));
                }
                let used = inner_parser.at + end + 1;
                let children = inner_parser.out;
                self.out.push(if lower == "b" || lower == "strong" {
                    DocumentationInline::Strong(children)
                } else {
                    DocumentationInline::Emphasis(children)
                });
                Some(used)
            }
            "code" if !closing => {
                let inner_text = &rest[end + 1..];
                let close = inner_text
                    .to_ascii_lowercase()
                    .find("</code>")
                    .unwrap_or(inner_text.len());
                self.out.push(DocumentationInline::Code(
                    unescape(inner_text[..close].trim()).to_owned(),
                ));
                Some(end + 1 + close + "</code>".len().min(inner_text.len() - close))
            }
            "a" if !closing => {
                let target = attribute(body, "href").unwrap_or_default();
                let inner_text = &rest[end + 1..];
                let close = inner_text
                    .to_ascii_lowercase()
                    .find("</a>")
                    .unwrap_or(inner_text.len());
                self.out.push(DocumentationInline::Link {
                    text: unescape(inner_text[..close].trim()).to_owned(),
                    target,
                });
                Some(end + 1 + close + "</a>".len().min(inner_text.len() - close))
            }
            _ => None,
        }
    }
}

fn flush(plain: &mut String, out: &mut Vec<DocumentationInline>) {
    if plain.is_empty() {
        return;
    }
    let text = unescape(plain).to_owned();
    plain.clear();
    if let Some(DocumentationInline::Text(known)) = out.last_mut() {
        known.push_str(&text);
    } else {
        out.push(DocumentationInline::Text(text));
    }
}

fn attribute(body: &str, wanted: &str) -> Option<String> {
    let at = body.to_ascii_lowercase().find(wanted)?;
    let after = body[at + wanted.len()..].trim_start();
    let after = after.strip_prefix('=')?.trim_start();
    let quote = after.chars().next()?;
    if quote != '"' && quote != '\'' {
        return Some(after.split_whitespace().next()?.to_owned());
    }
    let value = &after[1..];
    value.find(quote).map(|end| value[..end].to_owned())
}

fn unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use lang_api::{DocumentationBlock, DocumentationInline};

    use super::documentation_comments;

    fn first(text: &str) -> Vec<DocumentationBlock> {
        let comments = documentation_comments(text);
        assert_eq!(comments.len(), 1, "one documentation comment");
        comments.into_iter().next().unwrap().blocks
    }

    #[test]
    fn a_tag_becomes_a_block_with_its_subject() {
        let blocks = first(
            "class A {\n    /**\n     * Adds.\n     * @param a the first\n     * @return the sum\n     * @throws IllegalStateException never\n     */\n    int add(int a) {}\n}\n",
        );
        assert!(matches!(&blocks[0], DocumentationBlock::Paragraph(_)));
        let tags: Vec<_> = blocks[1..]
            .iter()
            .filter_map(|block| match block {
                DocumentationBlock::Tag {
                    name,
                    subject,
                    description,
                } => Some((name.as_str(), subject.as_deref(), description)),
                _ => None,
            })
            .collect();
        assert_eq!(tags.len(), 3);
        assert_eq!(tags[0].0, "param");
        assert_eq!(tags[0].1, Some("a"));
        assert_eq!(
            tags[0].2.as_slice(),
            [DocumentationInline::Text("the first".to_owned())]
        );
        assert_eq!(tags[1].0, "return");
        assert_eq!(tags[1].1, None);
        assert_eq!(tags[2].0, "throws");
        assert_eq!(tags[2].1, Some("IllegalStateException"));
    }

    #[test]
    fn inline_tags_become_code_and_links() {
        let blocks = first(
            "/** Calls {@code add(1, 2)} and returns {@link Result#value label}.\n */\nclass A {}\n",
        );
        let DocumentationBlock::Paragraph(inlines) = &blocks[0] else {
            panic!("a paragraph");
        };
        assert!(inlines.iter().any(|inline| matches!(
            inline,
            DocumentationInline::Code(text) if text == "add(1, 2)"
        )));
        assert!(inlines.iter().any(|inline| matches!(
            inline,
            DocumentationInline::Link { text, target }
                if text == "label" && target == "Result#value"
        )));
    }

    #[test]
    fn html_paragraphs_and_lists_become_blocks() {
        let blocks = first(
            "/** First.<p>Second with <b>bold</b>.\n * <ul><li>one</li><li>two</li></ul>\n */\nclass A {}\n",
        );
        assert!(matches!(blocks[0], DocumentationBlock::Paragraph(_)));
        assert!(matches!(blocks[1], DocumentationBlock::Paragraph(_)));
        let DocumentationBlock::List { items, .. } = &blocks[2] else {
            panic!("a list after the paragraphs: {blocks:?}");
        };
        assert_eq!(items.len(), 2);
        assert_eq!(items[0], [DocumentationInline::Text("one".to_owned())]);
        assert!(matches!(
            &blocks[1],
            DocumentationBlock::Paragraph(inlines)
                if inlines.iter().any(|inline| matches!(inline, DocumentationInline::Strong(_)))
        ));
    }

    #[test]
    fn a_comment_inside_a_string_is_not_a_comment() {
        let text = "class A { String s = \"/** no */\"; }\n";
        assert!(documentation_comments(text).is_empty());
    }
}
