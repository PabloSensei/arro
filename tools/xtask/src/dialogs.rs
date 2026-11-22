//! `check-dialogs`: a modal window of the UI crates is drawn by the one frame of
//! `ui_kit::dialog` or `ui_kit::floating`, so every one drags, resizes and closes the same
//! way. The check is a text check over the crates that build windows:
//!
//! - a file that shows an overlay (a `show_overlay*` of `workspace`) or that builds a
//!   block of its own over the window (`.absolute()` and `.occlude()` on the same file)
//!   calls `dialog_window` or `popup_window`;
//! - a menu, a tooltip or an anchored hint is no overlay: it is drawn by `deferred` and
//!   `anchored`, names no `show_overlay`, and is excepted with a reason where it shares a
//!   file with an absolute element.
//!
//! The frame itself lives in `ui-kit`, so only the crates that build windows are read.

use std::fs;
use std::path::Path;

use crate::Result;
use crate::tooltips::{Kind, Token, relative, sources};

/// The crates that build windows and overlays.
const CRATES: &[&str] = &[
    "crates/ui/workspace/src",
    "crates/ui/panels/src",
    "crates/ui/editor-view/src",
];

/// The files that are no modal window, each with the reason it may stand without the
/// frame. A file leaves this list once it is drawn by `dialog_window` or `popup_window`.
const EXCEPTIONS: &[(&str, &str)] = &[
    (
        "crates/ui/editor-view/src/quick_doc.rs",
        "the documentation popup: an anchored hint next to the caret, closed by Escape",
    ),
    (
        "crates/ui/editor-view/src/hierarchy.rs",
        "Quick Definition: an anchored popup next to the caret, and the catcher of its corner drag",
    ),
    (
        "crates/ui/editor-view/src/view.rs",
        "completion, Parameter Info and the inline menus: anchored popups, not windows",
    ),
    (
        "crates/ui/workspace/src/chrome.rs",
        "the menu bar, its backdrop and the tool window buttons: menus, not windows",
    ),
    (
        "crates/ui/workspace/src/window.rs",
        "the workspace that keeps the overlay and shows it: it builds no window of its own",
    ),
    (
        "crates/ui/panels/src/agent_hunk.rs",
        "the popup of an AI change: an anchored hint next to its hunk",
    ),
    (
        "crates/ui/panels/src/branches.rs",
        "the Branches dropdown with its submenu: a menu, not a window",
    ),
    (
        "crates/ui/panels/src/build.rs",
        "the history list under the console input: a dropdown, not a window",
    ),
    (
        "crates/ui/panels/src/clipboard.rs",
        "the paste history picker: a list of copied texts without fields or buttons",
    ),
    (
        "crates/ui/panels/src/hunk_popup.rs",
        "the popup of a hunk: an anchored hint next to the line",
    ),
    (
        "crates/ui/panels/src/plugins.rs",
        "the Plugins picker: a list of plugins without fields or buttons",
    ),
    (
        "crates/ui/panels/src/select_in.rs",
        "the Select In picker of Alt+F1: a numbered list of places without fields or buttons",
    ),
    (
        "crates/ui/panels/src/window_popups.rs",
        "the file path, navigation bar, scheme and scratch language pickers: dropdown lists",
    ),
];

pub fn run(root: &Path) -> Result {
    let mut found = Vec::new();
    for dir in CRATES {
        for file in sources(&root.join(dir))? {
            let relative = relative(root, &file);
            if excluded(&file) || excepted(&relative) {
                continue;
            }
            let text = fs::read_to_string(&file)?;
            if let Some(line) = overlay_without_frame(&text) {
                found.push(format!(
                    "{relative}:{line}: a modal overlay has no shared frame"
                ));
            }
        }
    }
    if found.is_empty() {
        return Ok(());
    }
    Err(format!(
        "{}\nbuild the window with ui_kit::dialog_window(..) or ui_kit::floating::popup_window(..), or name the file in EXCEPTIONS with a reason",
        found.join("\n")
    )
    .into())
}

/// Test files are not windows of the application.
fn excluded(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.contains("tests"))
}

/// Whether the file is a known menu, hint or picker rather than a modal window.
pub(crate) fn excepted(relative: &str) -> bool {
    EXCEPTIONS
        .iter()
        .any(|(path, reason)| *path == relative && !reason.is_empty())
}

/// The line of the evidence that a file builds a modal overlay without the shared frame.
/// `None` when the file has a frame or no overlay at all.
pub(crate) fn overlay_without_frame(text: &str) -> Option<usize> {
    let tokens = source_tokens(text);
    if call(&tokens, text, "dialog_window") || call(&tokens, text, "popup_window") {
        return None;
    }
    let mut shown = None;
    let mut absolute = None;
    let mut occlude = None;
    for (index, token) in tokens.iter().enumerate() {
        if token.kind != Kind::Word {
            continue;
        }
        let word = &text[token.start..token.end];
        if !followed_by_open(&tokens, index) || defined(&tokens, text, index) {
            continue;
        }
        match word {
            "show_overlay" | "show_overlay_tagged" | "show_overlay_with_rows" => {
                shown.get_or_insert(token.start);
            }
            "absolute" => {
                absolute.get_or_insert(token.start);
            }
            "occlude" => {
                occlude.get_or_insert(token.start);
            }
            _ => {}
        }
    }
    let at = match (shown, absolute, occlude) {
        (Some(shown), _, _) => Some(shown),
        (None, Some(_), Some(occlude)) => Some(occlude),
        _ => None,
    }?;
    Some(line_of(text, at))
}

/// Whether `name` is called somewhere in the file.
fn call(tokens: &[Token], text: &str, name: &str) -> bool {
    tokens.iter().enumerate().any(|(index, token)| {
        token.kind == Kind::Word
            && &text[token.start..token.end] == name
            && followed_by_open(tokens, index)
            && !defined(tokens, text, index)
    })
}

/// The token after the name opens its argument list: `name(..)`.
fn followed_by_open(tokens: &[Token], index: usize) -> bool {
    tokens
        .get(index + 1)
        .is_some_and(|token| token.kind == Kind::Punct('('))
}

/// `fn name(..)` declares the function rather than calling it.
fn defined(tokens: &[Token], text: &str, index: usize) -> bool {
    index
        .checked_sub(1)
        .and_then(|before| tokens.get(before))
        .is_some_and(|token| token.kind == Kind::Word && &text[token.start..token.end] == "fn")
}

/// The identifiers, literals and punctuation of the source. Whitespace and comments are
/// left out, and a lifetime such as `'static` is no character literal.
fn source_tokens(text: &str) -> Vec<Token> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let start = at;
        match bytes[at] {
            byte if byte.is_ascii_whitespace() => {
                at += 1;
            }
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
                // A character literal is `'x'` or `'\n'`; `'static` is a lifetime.
                let literal = match bytes.get(at + 1) {
                    Some(b'\\') => true,
                    Some(_) => bytes.get(at + 2) == Some(&b'\''),
                    None => false,
                };
                if literal {
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
                } else {
                    at += 1;
                    out.push(Token {
                        kind: Kind::Punct('\''),
                        start,
                        end: at,
                    });
                }
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
            byte if is_punctuation(byte) => {
                at += 1;
                out.push(Token {
                    kind: Kind::Punct(byte as char),
                    start,
                    end: at,
                });
            }
            _ => {
                while at < bytes.len()
                    && !is_punctuation(bytes[at])
                    && !bytes[at].is_ascii_whitespace()
                    && bytes[at] != b'"'
                    && bytes[at] != b'\''
                {
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

fn is_punctuation(byte: u8) -> bool {
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

fn line_of(text: &str, byte: usize) -> usize {
    text[..byte].bytes().filter(|byte| *byte == b'\n').count() + 1
}

#[cfg(test)]
mod tests {
    use super::{excepted, overlay_without_frame};

    #[test]
    fn an_overlay_without_the_frame_is_found() {
        let text = "fn render(&mut self) {\n    let body = surface(&theme).occlude();\n    div().absolute().left(px(10.)).child(body)\n}";
        assert_eq!(overlay_without_frame(text), Some(2));
    }

    #[test]
    fn a_shown_overlay_without_the_frame_is_found() {
        let text = "fn show(workspace: &mut Workspace) {\n    workspace.show_overlay(view.into(), focus, window, cx);\n}";
        assert_eq!(overlay_without_frame(text), Some(2));
    }

    #[test]
    fn a_window_on_dialog_window_passes() {
        let text = "fn render(&mut self, window: &mut Window) {\n    dialog_window(DialogSpec { .. }, shell, window, cx, close, contents)\n}";
        assert_eq!(overlay_without_frame(text), None);
    }

    #[test]
    fn a_search_popup_on_popup_window_passes() {
        let text =
            "fn render(&mut self) {\n    popup_window(PopupSpec { .. }, body, window, cx)\n}";
        assert_eq!(overlay_without_frame(text), None);
    }

    #[test]
    fn a_menu_is_no_overlay() {
        let text = "fn menu(&self) {\n    deferred(anchored().position(position).child(menu(entries, state, geometry)))\n}";
        assert_eq!(overlay_without_frame(text), None);
    }

    #[test]
    fn a_declaration_of_the_overlay_is_no_overlay() {
        let text = "pub fn show_overlay(&mut self, view: AnyView) {\n    self.overlay = Some(Overlay::View { view });\n}";
        assert_eq!(overlay_without_frame(text), None);
    }

    #[test]
    fn a_known_picker_is_excepted_with_a_reason() {
        assert!(excepted("crates/ui/panels/src/window_popups.rs"));
        assert!(excepted("crates/ui/workspace/src/chrome.rs"));
        assert!(!excepted("crates/ui/panels/src/choice.rs"));
    }
}
