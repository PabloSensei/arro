//! `check-tooltips`: a button that shows only an icon names itself in a tooltip. The
//! check finds the hand-built `div().id(..).on_click(..).child(icon(..))` buttons, which
//! bypass the one `icon_button` of `ui-kit` and its required tooltip. It is a text
//! check with a narrow rule: a chain that has an id, a click handler and a direct icon
//! child but no tooltip. A chain with any other visible child is not an icon button and
//! is left alone.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::Result;

/// The UI crates whose sources are checked.
const CRATES: &[&str] = &[
    "crates/ui/ui-kit/src",
    "crates/ui/workspace/src",
    "crates/ui/panels/src",
];

pub fn run(root: &Path) -> Result {
    let mut found = Vec::new();
    for dir in CRATES {
        for file in sources(&root.join(dir))? {
            if excluded(&file) {
                continue;
            }
            let text = fs::read_to_string(&file)?;
            for (line, message) in icon_buttons_without_tooltip(&text) {
                found.push(format!("{}:{line}: {message}", relative(root, &file)));
            }
        }
    }
    if found.is_empty() {
        return Ok(());
    }
    Err(format!(
        "{}\nuse ui_kit::icon_button(id, icon, name, shortcut, theme) or add .tooltip(..)",
        found.join("\n")
    )
    .into())
}

pub(crate) fn sources(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_owned()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir)? {
            let path = entry?.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                out.push(path);
            }
        }
    }
    out.sort();
    Ok(out)
}

pub(crate) fn excluded(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.contains("tests") || name == "usages.rs")
}

pub(crate) fn relative(root: &Path, file: &Path) -> String {
    file.strip_prefix(root)
        .unwrap_or(file)
        .to_string_lossy()
        .replace('\\', "/")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// A run of anything that is not punctuation: names, numbers, literals.
    Word,
    Punct(char),
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Token {
    pub(crate) kind: Kind,
    pub(crate) start: usize,
    pub(crate) end: usize,
}

/// Splits the source into punctuation and runs, leaving out string, character and
/// comment contents: a brace or a semicolon inside a label is not syntax.
pub(crate) fn tokens(text: &str) -> Vec<Token> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let start = at;
        match bytes[at] {
            b'"' => {
                at += 1;
                while at < bytes.len() {
                    if bytes[at] == b'\\' {
                        at += 2;
                    } else if bytes[at] == b'"' {
                        at += 1;
                        break;
                    } else {
                        at += 1;
                    }
                }
                out.push(Token {
                    kind: Kind::Word,
                    start,
                    end: at.min(bytes.len()),
                });
            }
            b'\'' => {
                at += 1;
                while at < bytes.len() {
                    if bytes[at] == b'\\' {
                        at += 2;
                    } else if bytes[at] == b'\'' {
                        at += 1;
                        break;
                    } else {
                        at += 1;
                    }
                }
                out.push(Token {
                    kind: Kind::Word,
                    start,
                    end: at.min(bytes.len()),
                });
            }
            b'/' if bytes.get(at + 1) == Some(&b'/') => {
                while at < bytes.len() && bytes[at] != b'\n' {
                    at += 1;
                }
            }
            b'/' if bytes.get(at + 1) == Some(&b'*') => {
                at += 2;
                while at + 1 < bytes.len() && !(bytes[at] == b'*' && bytes[at + 1] == b'/') {
                    at += 1;
                }
                at = (at + 2).min(bytes.len());
            }
            byte if is_punct(byte) => {
                at += 1;
                out.push(Token {
                    kind: Kind::Punct(byte as char),
                    start,
                    end: at,
                });
            }
            _ => {
                while at < bytes.len() && !is_punct(bytes[at]) && bytes[at] != b'"' {
                    at += 1;
                }
                out.push(Token {
                    kind: Kind::Word,
                    start,
                    end: at,
                });
            }
        }
    }
    out
}

fn is_punct(byte: u8) -> bool {
    matches!(
        byte,
        b'(' | b')'
            | b'{'
            | b'}'
            | b'['
            | b']'
            | b';'
            | b','
            | b'.'
            | b'|'
            | b'='
            | b'>'
            | b'<'
            | b'*'
            | b'&'
            | b'!'
            | b'?'
            | b':'
    )
}

#[derive(Debug)]
struct Method {
    name: String,
    /// The byte range of the argument between the parentheses.
    argument: (usize, usize),
}

/// The top-level method calls of the expression that holds the token at `at`, and the
/// byte range of that expression.
fn methods_around(text: &str, tokens: &[Token], at: usize) -> Option<Vec<Method>> {
    let index = tokens
        .iter()
        .position(|token| token.start == at && token.kind == Kind::Punct('.'))?;
    let start = expression_start(tokens, index);
    let end = expression_end(tokens, index);
    Some(methods(text, tokens, start, end))
}

pub(crate) fn expression_start(tokens: &[Token], index: usize) -> usize {
    let mut depth = 0usize;
    let mut at = index;
    while at > 0 {
        at -= 1;
        match tokens[at].kind {
            Kind::Punct(')') | Kind::Punct(']') | Kind::Punct('}') => depth += 1,
            Kind::Punct('(') | Kind::Punct('[') | Kind::Punct('{') => {
                if depth == 0 {
                    return at + 1;
                }
                depth -= 1;
            }
            Kind::Punct(';') | Kind::Punct(',') if depth == 0 => return at + 1,
            _ => {}
        }
    }
    0
}

fn expression_end(tokens: &[Token], index: usize) -> usize {
    let mut depth = 0usize;
    let mut at = index;
    while at < tokens.len() {
        match tokens[at].kind {
            Kind::Punct('(') | Kind::Punct('[') | Kind::Punct('{') => depth += 1,
            Kind::Punct(')') | Kind::Punct(']') | Kind::Punct('}') => {
                if depth == 0 {
                    return at;
                }
                depth -= 1;
            }
            Kind::Punct(';') | Kind::Punct(',') if depth == 0 => return at,
            _ => {}
        }
        at += 1;
    }
    tokens.len()
}

fn methods(text: &str, tokens: &[Token], start: usize, end: usize) -> Vec<Method> {
    let mut found = Vec::new();
    let mut depth = 0usize;
    let mut at = start;
    while at < end {
        match tokens[at].kind {
            Kind::Punct('(') | Kind::Punct('[') | Kind::Punct('{') => depth += 1,
            Kind::Punct(')') | Kind::Punct(']') | Kind::Punct('}') => {
                depth = depth.saturating_sub(1);
            }
            Kind::Punct('.') if depth == 0 => {
                let name = tokens.get(at + 1);
                let open = tokens.get(at + 2);
                if let (Some(name), Some(open)) = (name, open)
                    && name.kind == Kind::Word
                    && open.kind == Kind::Punct('(')
                {
                    let argument_start = open.end;
                    let mut inner = 1usize;
                    let mut close = at + 3;
                    while close < end {
                        match tokens[close].kind {
                            Kind::Punct('(') => inner += 1,
                            Kind::Punct(')') => {
                                inner -= 1;
                                if inner == 0 {
                                    break;
                                }
                            }
                            _ => {}
                        }
                        close += 1;
                    }
                    let argument_end = tokens
                        .get(close)
                        .filter(|token| token.kind == Kind::Punct(')'))
                        .map_or(argument_start, |token| token.start);
                    found.push(Method {
                        name: text[name.start..name.end].to_owned(),
                        argument: (argument_start, argument_end),
                    });
                    at = close;
                }
            }
            _ => {}
        }
        at += 1;
    }
    found
}

enum Child {
    Icon,
    Empty,
    Visible,
}

fn child(text: &str, argument: (usize, usize)) -> Child {
    let value = text[argument.0..argument.1].trim();
    if value.starts_with("icon(")
        || value.starts_with("icon_sized(")
        || value.starts_with("svg(")
        || value.starts_with("img(")
    {
        return Child::Icon;
    }
    if value.starts_with("div(")
        && !value.contains('"')
        && !value.contains(".child(")
        && !value.contains(".children(")
    {
        return Child::Empty;
    }
    Child::Visible
}

/// One-based line and message of every hand-built icon button without a tooltip.
fn icon_buttons_without_tooltip(text: &str) -> Vec<(usize, String)> {
    let tokens = tokens(text);
    let mut spans = BTreeSet::new();
    let mut found = Vec::new();
    for (index, token) in tokens.iter().enumerate() {
        if token.kind != Kind::Punct('.') {
            continue;
        }
        if tokens
            .get(index + 1)
            .is_none_or(|next| next.kind != Kind::Word || &text[next.start..next.end] != "id")
            || tokens
                .get(index + 2)
                .is_none_or(|next| next.kind != Kind::Punct('('))
        {
            continue;
        }
        let Some(methods) = methods_around(text, &tokens, token.start) else {
            continue;
        };
        let has_id = methods.iter().any(|method| method.name == "id");
        let has_click = methods
            .iter()
            .any(|method| method.name == "on_click" || method.name == "on_mouse_down");
        let has_tooltip = methods
            .iter()
            .any(|method| method.name == "tooltip" || method.name == "hoverable_tooltip");
        if !has_id || !has_click || has_tooltip {
            continue;
        }
        let children: Vec<Child> = methods
            .iter()
            .filter(|method| method.name == "child")
            .map(|method| child(text, method.argument))
            .collect();
        let has_icon = children.iter().any(|child| matches!(child, Child::Icon));
        let has_visible = children.iter().any(|child| matches!(child, Child::Visible));
        if !has_icon || has_visible {
            continue;
        }
        let line = text[..token.start]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count()
            + 1;
        if spans.insert(line) {
            found.push((line, "icon button has no tooltip".to_owned()));
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUTTON: &str = "div()\n  .id(\"b\")\n  .on_click(cx.listener(..))\n  .child(icon(\"general/add\", theme));";

    #[test]
    fn a_hand_built_icon_button_without_a_tooltip_is_found() {
        assert_eq!(
            icon_buttons_without_tooltip(BUTTON),
            vec![(2, "icon button has no tooltip".to_owned())]
        );
    }

    #[test]
    fn a_tooltip_or_a_visible_child_clears_the_button() {
        let with_tooltip =
            "div().id(\"b\").on_click(f).tooltip(t).child(icon(\"general/add\", theme));";
        assert!(icon_buttons_without_tooltip(with_tooltip).is_empty());

        let with_text =
            "div().id(\"b\").on_click(f).child(\"Add\").child(icon(\"general/add\", theme));";
        assert!(icon_buttons_without_tooltip(with_text).is_empty());

        let with_wrapper = "div().id(\"b\").on_click(f).child(div().child(\"Add\")).child(icon(\"general/add\", theme));";
        assert!(icon_buttons_without_tooltip(with_wrapper).is_empty());
    }

    #[test]
    fn a_label_in_a_string_does_not_break_the_expression() {
        let text = "div().id(\"b)\").on_click(f).tooltip(\"name (key)\").child(icon(\"general/add\", theme));";
        assert!(icon_buttons_without_tooltip(text).is_empty());
    }

    #[test]
    fn the_ui_crates_pass() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        run(&root).unwrap();
    }
}
