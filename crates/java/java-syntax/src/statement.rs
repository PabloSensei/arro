//! Ctrl+Shift+Enter: the marks the statement at a place is missing, read from the tree of
//! the file and from the brackets that are still open before the caret.

use tree_sitter::{Node, Tree};

use super::edit::{node_at, parse};

/// The plan of Complete Statement: the bytes of the text the edit replaces, the text that
/// takes their place and the place of the caret inside it, counted from the start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatementEdit {
    pub start: usize,
    pub end: usize,
    pub text: String,
    pub caret: usize,
}

/// The words whose statement opens a block after a condition in brackets.
const HEADER_WORDS: &[&str] = &["if", "while", "for", "switch", "synchronized"];
/// The words whose statement is a block and nothing else.
const BLOCK_WORDS: &[&str] = &["else", "try", "finally", "do"];
/// The words that start a declaration with a body.
const DECLARATION_WORDS: &[&str] = &["class", "interface", "enum", "record"];
/// The nodes whose braces hold declarations rather than statements.
const MEMBER_BODIES: &[&str] = &[
    "class_body",
    "interface_body",
    "enum_body",
    "enum_body_declarations",
    "annotation_type_body",
];
/// The nodes whose braces hold statements.
const BLOCK_BODIES: &[&str] = &[
    "block",
    "constructor_body",
    "switch_block",
    "switch_block_statement_group",
    "array_initializer",
];
/// The nodes of a method or a constructor header.
const METHOD_HEADERS: &[&str] = &[
    "method_declaration",
    "constructor_declaration",
    "compact_constructor_declaration",
];

/// Finishes the statement whose caret is at `offset`. The lines of the statement before
/// the caret stay as they are; the missing marks go to the end of the caret line, in
/// front of the comment that closes it.
pub fn complete_statement(text: &str, offset: usize, indent_unit: &str) -> Option<StatementEdit> {
    let offset = offset.min(text.len());
    let line_start = text[..offset].rfind('\n').map_or(0, |at| at + 1);
    let line_end = text[offset..]
        .find('\n')
        .map_or(text.len(), |at| offset + at);
    let line = &text[line_start..line_end];
    let lead = line.len() - line.trim_start().len();
    let first = line_start + lead;
    let last = line_start + line.trim_end().len();
    // A caret behind the end of the line takes the blanks with it.
    let end = last.max(offset);
    let head = text.get(first..last)?;
    if head.trim().is_empty() {
        return None;
    }
    let indent = &line[..lead];
    let tree = parse(text)?;

    // The plan of a line that needs nothing: the next line at the same indent.
    let next_line = |closed: &str, note: &str| StatementEdit {
        start: first,
        end,
        text: format!("{closed}{note}\n{indent}"),
        caret: closed.len() + note.len() + 1 + indent.len(),
    };
    if is_comment(&tree, first) || head.starts_with('@') {
        return Some(next_line(head, ""));
    }
    // The marks of the statement belong in front of the comment that ends its line, not
    // inside that comment. A line that is a comment alone was taken above.
    let (code, comment) = split_comment(head);
    if code.is_empty() {
        return Some(next_line(head, ""));
    }
    let note = if comment.is_empty() {
        String::new()
    } else {
        format!(" {comment}")
    };
    let open = unclosed_brackets(text, end);
    let closed = format!("{code}{}", closers_of(&open));
    if closed.ends_with(';') || closed.ends_with('}') {
        return Some(next_line(&closed, &note));
    }
    // A statement that began on an earlier line lends its own indent to the body of a
    // block: a continuation line, such as the condition of a multi-line `if`, is
    // indented deeper than the statement.
    let continued = continued_statement(&tree, offset, first);
    let block_indent = continued.map_or(indent, |node| line_indent(text, node.start_byte()));
    let block_inner = format!("{block_indent}{indent_unit}");
    if closed.ends_with('{') {
        // The block is already open: the body on its own line and the closing brace.
        let before = format!("{closed}{note}\n{block_inner}");
        return Some(StatementEdit {
            start: first,
            end,
            caret: before.len(),
            text: format!("{before}\n{block_indent}}}"),
        });
    }

    // The word that opens the statement: the one before its first open bracket, such as
    // `if` in `if (x`, or the first word of the caret line when no bracket is open. A
    // continuation line starts in the middle of a statement, so the word that opens it is
    // the first word of the line the statement began on.
    let line_word = open
        .first()
        .map_or_else(|| first_word(head), |(_, at)| word_before(text, *at));
    let opener = if names_shape(line_word) {
        line_word
    } else {
        continued.map_or(line_word, |node| word_of(&text[node.start_byte()..]))
    };
    if opener == "catch" {
        return Some(catch_block(
            first,
            end,
            code,
            &block_inner,
            block_indent,
            &note,
        ));
    }
    let header = HEADER_WORDS.contains(&opener)
        || BLOCK_WORDS.contains(&opener)
        || DECLARATION_WORDS.contains(&opener);
    // A member of a type that ends with its parameter list is the header of a method or a
    // constructor; a field holds `=` and is a statement.
    let method_header = continued.is_some_and(|node| METHOD_HEADERS.contains(&node.kind()));
    let method = !header
        && closed.ends_with(')')
        && (method_header || (member_context(&tree, first) && !code.contains('=')));
    if header || method {
        let before = format!("{closed} {{{note}\n{block_inner}");
        return Some(StatementEdit {
            start: first,
            end,
            caret: before.len(),
            text: format!("{before}\n{block_indent}}}"),
        });
    }
    let body = format!("{closed};{note}\n{indent}");
    Some(StatementEdit {
        start: first,
        end,
        caret: body.len(),
        text: body,
    })
}

/// `catch` alone gets its parameter list with the caret inside, as IDEA does; a `catch`
/// that already names its exception gets the body, the way the other headers do.
fn catch_block(
    start: usize,
    end: usize,
    code: &str,
    inner: &str,
    indent: &str,
    note: &str,
) -> StatementEdit {
    let after = code
        .find("catch")
        .map_or("", |at| code[at + "catch".len()..].trim());
    let (closed, caret) = if after.is_empty() {
        (format!("{code} ()"), Some(code.len() + 2))
    } else if after == "(" {
        (format!("{code})"), Some(code.len()))
    } else {
        (format!("{code}{}", missing_closers(code, code.len())), None)
    };
    let body = format!("{closed} {{{note}\n{inner}\n{indent}}}");
    StatementEdit {
        // A named exception sends the caret into the body, where the code goes.
        caret: caret.unwrap_or_else(|| format!("{closed} {{{note}\n{inner}").len()),
        start,
        end,
        text: body,
    }
}

/// Whether the word alone says what shape the statement has: a header that takes a block,
/// a block word or a declaration.
fn names_shape(word: &str) -> bool {
    word == "catch"
        || HEADER_WORDS.contains(&word)
        || BLOCK_WORDS.contains(&word)
        || DECLARATION_WORDS.contains(&word)
}

/// The code of a caret line without the comment that ends it, and that comment. Strings
/// and character literals hold no comments, so a `//` in a URL stays code.
fn split_comment(head: &str) -> (&str, &str) {
    let mut rest = 0;
    while let Some(found) = head[rest..].find(['/', '"', '\'']) {
        let at = rest + found;
        let after = &head[at..];
        if after.starts_with("//") {
            return (head[..at].trim_end(), after);
        }
        if after.starts_with("/*") {
            // A block comment that closes on the line is the end of the code only when
            // nothing follows it.
            match after.find("*/") {
                None => return (head[..at].trim_end(), after),
                Some(close) => {
                    let closed = at + close + "*/".len();
                    if head[closed..].trim().is_empty() {
                        return (head[..at].trim_end(), after);
                    }
                    rest = closed;
                }
            }
            continue;
        }
        if !after.starts_with(['"', '\'']) {
            // A slash of the code itself, as in a division: nothing to skip.
            rest = at + 1;
            continue;
        }
        // A string or a character literal, up to the quote that closes it. A quote alone
        // at the end of the line takes the rest of the line.
        let quote = after.chars().next().unwrap_or('"');
        let mut next = at + quote.len_utf8();
        while next < head.len() {
            let character = head[next..].chars().next().unwrap_or(quote);
            next += character.len_utf8();
            if character == '\\' {
                next += head[next..].chars().next().map_or(0, char::len_utf8);
            } else if character == quote {
                break;
            }
        }
        rest = next;
    }
    (head, "")
}

/// The indentation of the line that holds `offset`.
fn line_indent(text: &str, offset: usize) -> &str {
    let start = text[..offset.min(text.len())]
        .rfind('\n')
        .map_or(0, |at| at + 1);
    let line = &text[start..];
    &line[..line.len() - line.trim_start().len()]
}

/// The statement or the method header whose continuation the caret line is: the first
/// statement or member above the place, when it began on an earlier line. A place inside
/// a body, where a statement of its own starts, continues nothing.
fn continued_statement(tree: &Tree, offset: usize, first: usize) -> Option<Node<'_>> {
    let mut node = node_at(tree, offset.saturating_sub(1));
    loop {
        let kind = node.kind();
        if is_body(node) {
            return None;
        }
        let construct = kind.ends_with("_statement") || kind == "switch_expression";
        if construct || kind.ends_with("_declaration") {
            let continues = construct || METHOD_HEADERS.contains(&kind);
            return (continues && node.start_byte() < first).then_some(node);
        }
        node = node.parent()?;
    }
}

/// Whether the node holds statements or declarations rather than being one.
fn is_body(node: Node<'_>) -> bool {
    let kind = node.kind();
    BLOCK_BODIES.contains(&kind) || MEMBER_BODIES.contains(&kind) || kind == "program"
}

/// The first whole word of the code, after the closing marks of the blocks that end on
/// the line, such as the brace of `} else`.
fn first_word(code: &str) -> &str {
    let start = code
        .find(|character: char| !matches!(character, '}' | ')' | ']') && !character.is_whitespace())
        .unwrap_or(code.len());
    word_of(&code[start..])
}

/// The whole word that ends right before `offset`, after the blanks before it.
fn word_before(text: &str, offset: usize) -> &str {
    let before = text.get(..offset).unwrap_or(text).trim_end();
    let start = before
        .rfind(|character: char| !(character.is_alphanumeric() || character == '_'))
        .map_or(0, |at| {
            at + before[at..].chars().next().map_or(1, char::len_utf8)
        });
    &before[start..]
}

/// The run of word characters that starts the code.
fn word_of(code: &str) -> &str {
    let len = code
        .find(|character: char| !(character.is_alphanumeric() || character == '_'))
        .unwrap_or(code.len());
    &code[..len]
}

/// The closing characters the brackets ask for, in the order they close.
fn closers_of(open: &[(char, usize)]) -> String {
    open.iter()
        .rev()
        .map(|(open, _)| match open {
            '(' => ')',
            '[' => ']',
            other => *other,
        })
        .collect()
}

/// The same for a piece of code, read on its own.
fn missing_closers(text: &str, end: usize) -> String {
    closers_of(&unclosed_brackets(text, end))
}

/// The brackets the text before `end` leaves open, with the byte offset of each, and the
/// innermost last. Strings and comments are skipped.
fn unclosed_brackets(text: &str, end: usize) -> Vec<(char, usize)> {
    let mut stack: Vec<(char, usize)> = Vec::new();
    let mut chars = text.get(..end).unwrap_or(text).char_indices().peekable();
    while let Some((at, character)) = chars.next() {
        match character {
            '/' if chars.peek().is_some_and(|(_, next)| *next == '/') => {
                while chars.peek().is_some_and(|(_, next)| *next != '\n') {
                    chars.next();
                }
            }
            '/' if chars.peek().is_some_and(|(_, next)| *next == '*') => {
                chars.next();
                let mut previous = '\0';
                for (_, next) in chars.by_ref() {
                    if previous == '*' && next == '/' {
                        break;
                    }
                    previous = next;
                }
            }
            quote @ ('"' | '\'') => {
                while let Some((_, next)) = chars.next() {
                    match next {
                        '\\' => {
                            chars.next();
                        }
                        mark if mark == quote => break,
                        _ => {}
                    }
                }
            }
            open @ ('(' | '[') => stack.push((open, at)),
            ')' | ']' => {
                stack.pop();
            }
            _ => {}
        }
    }
    stack
}

/// Whether the place at `offset` sits between the braces of a type, where declarations
/// live, rather than between the braces of a block. The last brace that is still open
/// before the place is the one that holds it.
fn member_context(tree: &Tree, offset: usize) -> bool {
    let mut open: Vec<Node<'_>> = Vec::new();
    collect_braces(tree.root_node(), offset, &mut open);
    open.last().is_none_or(|brace| opens_members(*brace))
}

fn collect_braces<'a>(node: Node<'a>, offset: usize, open: &mut Vec<Node<'a>>) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.start_byte() >= offset {
            break;
        }
        match child.kind() {
            "{" => open.push(child),
            "}" => {
                open.pop();
            }
            _ => collect_braces(child, offset, open),
        }
    }
}

/// Whether a brace that is still open at a place holds declarations. A tree of broken
/// text puts its braces under an error node, and then the token before the brace says
/// what it opens.
fn opens_members(brace: Node<'_>) -> bool {
    match brace.parent().map(|parent| parent.kind()) {
        Some(kind) if MEMBER_BODIES.contains(&kind) => true,
        Some(kind) if BLOCK_BODIES.contains(&kind) => false,
        _ => match brace.prev_sibling() {
            // A parameter list or a condition before the brace opens the body of a
            // method, of a statement or of a lambda.
            Some(before) => {
                let kind = before.kind();
                !(kind.ends_with("parameters")
                    || kind.ends_with("expression")
                    || kind.ends_with("argument_list")
                    || matches!(
                        kind,
                        ")" | "]" | "else" | "try" | "finally" | "do" | "static" | "->"
                    ))
            }
            None => true,
        },
    }
}

/// Whether the place at `offset` is inside a comment, where Complete Statement only
/// starts the next line.
fn is_comment(tree: &Tree, offset: usize) -> bool {
    let mut node = Some(node_at(tree, offset));
    while let Some(current) = node {
        match current.kind() {
            "line_comment" | "block_comment" => return true,
            "program" => return false,
            _ => node = current.parent(),
        }
    }
    false
}
