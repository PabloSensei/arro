//! `check-scrollbars`: a scrolled area in the UI crates has bars around it. The check
//! finds `overflow_y_scroll`, `overflow_x_scroll` and `uniform_list` and wants the call
//! to stand inside a `scrollable(..)` of `ui-kit`. A scrolled area that must not have
//! bars says why in a `// scrollbar-exempt: reason` comment on or next to the call.

use std::fs;
use std::path::Path;

use crate::Result;
use crate::tooltips::{Kind, excluded, relative, sources, tokens};

/// The UI crates whose sources are checked.
const CRATES: &[&str] = &[
    "crates/ui/ui-kit/src",
    "crates/ui/workspace/src",
    "crates/ui/panels/src",
];

/// Methods and functions that make an area scrollable.
const SCROLLED: &[&str] = &["overflow_y_scroll", "overflow_x_scroll", "uniform_list"];

pub fn run(root: &Path) -> Result {
    let mut found = Vec::new();
    for dir in CRATES {
        for file in sources(&root.join(dir))? {
            if excluded(&file) {
                continue;
            }
            let text = fs::read_to_string(&file)?;
            for (line, message) in unconnected(&text) {
                found.push(format!("{}:{line}: {message}", relative(root, &file)));
            }
        }
    }
    if found.is_empty() {
        return Ok(());
    }
    Err(format!(
        "{}\nwrap the area in ui_kit::scrollable(..) or add // scrollbar-exempt: reason",
        found.join("\n")
    )
    .into())
}

/// One-based lines and messages of scrolled areas without bars.
fn unconnected(text: &str) -> Vec<(usize, String)> {
    let tokens = tokens(text);
    let mut found = Vec::new();
    let mut previous_word: Option<&str> = None;
    // One flag per open parenthesis: was it opened by `scrollable`?
    let mut inside: Vec<bool> = Vec::new();
    for token in &tokens {
        match token.kind {
            Kind::Punct('(') => {
                inside.push(previous_word == Some("scrollable"));
                previous_word = None;
            }
            Kind::Punct(')') => {
                inside.pop();
                previous_word = None;
            }
            Kind::Word => {
                let raw = &text[token.start..token.end];
                let word = raw.trim();
                let at = token.start + (raw.len() - raw.trim_start().len());
                if SCROLLED.contains(&word) && !inside.iter().any(|open| *open) {
                    let line = line_of(text, at);
                    if !exempt(text, at) {
                        found.push((line, format!("{word} has no scrollbar")));
                    }
                }
                previous_word = Some(word);
            }
            _ => previous_word = None,
        }
    }
    found
}

fn line_of(text: &str, byte: usize) -> usize {
    text[..byte].bytes().filter(|byte| *byte == b'\n').count() + 1
}

/// Whether a `// scrollbar-exempt: reason` comment stands on the line of `byte`, the one
/// above or the one below it, with a reason after the colon.
fn exempt(text: &str, byte: usize) -> bool {
    let line = line_of(text, byte);
    let lines: Vec<&str> = text.lines().collect();
    let first = line.saturating_sub(2);
    let last = line.min(lines.len().saturating_sub(1));
    lines[first..=last].iter().any(|line| {
        line.find("// scrollbar-exempt:")
            .is_some_and(|at| !line[at + "// scrollbar-exempt:".len()..].trim().is_empty())
    })
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::unconnected;

    #[test]
    fn a_scrolled_div_without_bars_is_found() {
        let text = "div()\n    .overflow_y_scroll()\n    .children(rows);";
        assert_eq!(
            unconnected(text),
            vec![(2, "overflow_y_scroll has no scrollbar".to_owned())]
        );
    }

    #[test]
    fn a_list_without_bars_is_found() {
        let text = "let rows = uniform_list(\"rows\", 10, move |..| rows);";
        assert_eq!(
            unconnected(text),
            vec![(1, "uniform_list has no scrollbar".to_owned())]
        );
    }

    #[test]
    fn the_content_of_scrollable_is_left_alone() {
        let text = "scrollable(\n    \"rows\",\n    &handle,\n    div().overflow_x_scroll().child(uniform_list(\"a\", 1, f)),\n    &theme,\n    window,\n    cx,\n);";
        assert!(unconnected(text).is_empty());
    }

    #[test]
    fn an_exemption_needs_a_reason() {
        let text = "// scrollbar-exempt: the terminal paints its own bar\n\
                    div().overflow_y_scroll();\n\
                    // scrollbar-exempt:\n\
                    div().overflow_y_scroll();";
        assert_eq!(
            unconnected(text),
            vec![(4, "overflow_y_scroll has no scrollbar".to_owned())]
        );
    }

    #[test]
    fn the_ui_crates_pass() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        crate::scrollbars::run(&root).unwrap();
    }
}
