//! The request the window sends to the AI Sessions agent for a problem the user sees:
//! a diagnostic of the editor, a row of Problems, a failed test or an exception of the
//! console. The messages are short, name every path relative to the project, and the
//! agent reads the rest through the MCP tools of Arro.

use std::path::Path;

use editor_view::AiProblem;
use ui_kit::gpui::{App, Window};
use ui_kit::prelude::*;
use workspace::{AI_TOOL, Settings, Workspace};

use crate::agents::ActiveAgents;
use crate::outline::OpenEditors;

/// The one sentence that points the agent at the tools of the window.
const MORE: &str = "Use the Arro MCP tools to read more.";
/// The source lines around a diagnostic that the message carries.
const CONTEXT_LINES: usize = 20;
/// The stack lines the message carries before it says the rest is cut.
const MAX_STACK_LINES: usize = 40;
/// One stack line longer than this is cut too.
const MAX_LINE_CHARS: usize = 400;

/// Whether the AI features are on, as the settings page Tools | AI says.
pub(crate) fn enabled(cx: &App) -> bool {
    cx.try_global::<Settings>()
        .is_none_or(|settings| settings.ai_enabled)
}

/// `path` under `root` with forward slashes, as the messages write it.
pub(crate) fn relative(root: &Path, path: &Path) -> String {
    let relative = path.strip_prefix(root).unwrap_or(path);
    relative.to_string_lossy().replace('\\', "/")
}

/// The lines of `text` around `line` (from zero), at most [`CONTEXT_LINES`] of them, with
/// numbers from one. A problem near either end gets the first or the last lines.
fn context(text: &str, line: usize) -> Vec<(usize, String)> {
    let all: Vec<&str> = text.split('\n').collect();
    let line = line.min(all.len().saturating_sub(1));
    let mut start = line.saturating_sub(CONTEXT_LINES / 2);
    let mut end = (start + CONTEXT_LINES).min(all.len());
    if end - start < CONTEXT_LINES {
        start = end.saturating_sub(CONTEXT_LINES);
    }
    end = end.max(start);
    all[start..end]
        .iter()
        .enumerate()
        .map(|(index, text)| (start + index + 1, text.trim_end_matches('\r').to_string()))
        .collect()
}

/// The message of a diagnostic: where it is, what it says, the code around it and the
/// request to fix it.
pub(crate) fn diagnostic_at(
    root: &Path,
    path: &Path,
    line: usize,
    kind: &str,
    message: &str,
    code: Option<&str>,
    text: Option<&str>,
) -> String {
    let mut out = format!("Fix this {kind} in {}:{}.", relative(root, path), line + 1);
    out.push_str("\n\nDiagnostic: ");
    out.push_str(message.trim());
    if let Some(code) = code.filter(|code| !code.is_empty()) {
        out.push_str("\nCode: ");
        out.push_str(code);
    }
    if let Some(text) = text {
        let lines = context(text, line);
        if !lines.is_empty() {
            out.push_str("\n\n");
            for (number, text) in lines {
                out.push_str(&format!("{number}: {text}\n"));
            }
            out.pop();
        }
    }
    out.push_str("\n\n");
    out.push_str(MORE);
    out
}

/// The message of a diagnostic of the editor, which carries the text with the unsaved
/// edits of the user.
pub(crate) fn diagnostic_message(root: &Path, problem: &AiProblem) -> String {
    let kind = match problem.severity {
        lang_api::Severity::Error => "error",
        lang_api::Severity::Warning => "warning",
        _ => "problem",
    };
    diagnostic_at(
        root,
        &problem.path,
        problem.line,
        kind,
        &problem.message,
        problem.code.as_deref(),
        Some(&problem.text),
    )
}

/// The message of a failed test: its name, the message, the stack and the file. A long
/// stack is cut and says so.
pub(crate) fn test_failure(
    root: &Path,
    name: &str,
    file: Option<&Path>,
    message: &str,
    stack: &str,
) -> String {
    let mut out = format!("Fix the failing test {name}");
    if let Some(file) = file {
        out.push_str(" in ");
        out.push_str(&relative(root, file));
    }
    out.push('.');
    out.push_str("\n\n");
    if !message.trim().is_empty() {
        out.push_str("Message: ");
        out.push_str(message.trim());
        out.push_str("\n\n");
    }
    out.push_str("Stack:\n");
    if stack.trim().is_empty() {
        out.push_str("(not reported)");
    } else {
        out.push_str(&truncated(stack));
    }
    out.push_str("\n\n");
    out.push_str(MORE);
    out
}

/// The message about an exception of the console: the exception and its stack, with the
/// request to explain the cause.
pub(crate) fn exception(text: &str) -> String {
    format!(
        "Explain the cause of this exception.\n\n{}\n\n{MORE}",
        truncated(text.trim_end())
    )
}

/// The text of `path`, from its open editor with unsaved edits, or from the disk.
pub(crate) fn file_text(path: &Path, cx: &App) -> Option<String> {
    OpenEditors::text(path, cx).or_else(|| std::fs::read_to_string(path).ok())
}

/// Sends `message` to the AI Sessions window, which starts its usual chat session when
/// there is none.
pub(crate) fn ask(
    workspace: &mut Workspace,
    message: String,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(panel) = cx.try_global::<ActiveAgents>().map(|panel| panel.0.clone()) else {
        return;
    };
    workspace.show_tool(AI_TOOL, window, cx);
    panel.update(cx, |panel, cx| panel.ask_message(message, window, cx));
}

/// `text` with at most [`MAX_STACK_LINES`] lines and every line at most
/// [`MAX_LINE_CHARS`] long. The cut says so.
fn truncated(text: &str) -> String {
    let lines: Vec<String> = text.lines().map(cut_line).collect();
    let total = lines.len();
    let kept = total.min(MAX_STACK_LINES);
    let mut out = lines[..kept].join("\n");
    if total > kept {
        out.push_str("\n... (truncated)");
    }
    out
}

fn cut_line(line: &str) -> String {
    if line.chars().count() <= MAX_LINE_CHARS {
        return line.to_string();
    }
    let kept: String = line.chars().take(MAX_LINE_CHARS).collect();
    format!("{kept}...")
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{CONTEXT_LINES, MAX_STACK_LINES, exception, relative, test_failure, truncated};

    fn numbered(count: usize) -> String {
        let lines: Vec<String> = (1..=count).map(|number| format!("line{number}")).collect();
        lines.join("\n")
    }

    #[test]
    fn a_diagnostic_near_the_start_keeps_the_first_lines_of_the_file() {
        let text = numbered(30);
        let message = super::diagnostic_at(
            Path::new("proj"),
            Path::new("proj/src/Main.txt"),
            1,
            "error",
            "cannot find symbol",
            Some("compiler.err.cant.resolve"),
            Some(&text),
        );
        assert!(message.contains("src/Main.txt:2"), "{message}");
        assert!(message.contains("cannot find symbol"), "{message}");
        assert!(message.contains("compiler.err.cant.resolve"), "{message}");
        assert!(message.contains("1: line1"), "{message}");
        assert!(
            message.contains(&format!("{CONTEXT_LINES}: line{CONTEXT_LINES}")),
            "{message}"
        );
        assert!(
            !message.contains(&format!("{}: line{}", CONTEXT_LINES + 1, CONTEXT_LINES + 1)),
            "the message has no lines past the window: {message}"
        );
        assert!(message.contains("Use the Arro MCP tools to read more."));
    }

    #[test]
    fn a_diagnostic_near_the_end_keeps_the_last_lines_of_the_file() {
        let text = numbered(30);
        let message = super::diagnostic_at(
            Path::new("proj"),
            Path::new("proj/src/Main.txt"),
            29,
            "warning",
            "unused",
            None,
            Some(&text),
        );
        assert!(message.contains("30: line30"), "{message}");
        assert!(!message.contains("\n1: line1\n"), "{message}");
        let first = 30 - CONTEXT_LINES + 1;
        assert!(
            message.contains(&format!("{first}: line{first}")),
            "{message}"
        );
    }

    #[test]
    fn paths_go_relative_to_the_project_with_forward_slashes() {
        assert_eq!(
            relative(Path::new("proj"), Path::new("proj/src/demo/Main.txt")),
            "src/demo/Main.txt"
        );
    }

    #[test]
    fn a_long_stack_is_cut_and_says_so() {
        let stack: Vec<String> = (0..MAX_STACK_LINES + 20)
            .map(|index| format!("at demo.Main.frame{index}(Main.txt:{index})"))
            .collect();
        let message = test_failure(
            Path::new("proj"),
            "demo.MainTest#fails",
            Some(Path::new("proj/src/demo/MainTest.txt")),
            "expected 1 but was 2",
            &stack.join("\n"),
        );
        assert!(message.contains("demo.MainTest#fails"), "{message}");
        assert!(message.contains("src/demo/MainTest.txt"), "{message}");
        assert!(message.contains("expected 1 but was 2"), "{message}");
        assert!(message.contains("truncated"), "{message}");
        assert!(
            message.contains(&format!("frame{}", MAX_STACK_LINES - 1)),
            "{message}"
        );
        assert!(
            !message.contains(&format!("frame{}", MAX_STACK_LINES)),
            "{message}"
        );
    }

    #[test]
    fn an_exception_asks_for_the_cause_and_keeps_the_stack() {
        let message =
            exception("demo.IllegalStateException: boom\n\tat demo.Main.main(Main.txt:3)");
        assert!(message.contains("Explain the cause of this exception."));
        assert!(message.contains("demo.IllegalStateException: boom"));
        assert!(message.contains("at demo.Main.main(Main.txt:3)"));
        assert!(message.contains("Use the Arro MCP tools to read more."));
    }

    #[test]
    fn a_line_of_a_stack_longer_than_the_limit_is_cut() {
        let line = "x".repeat(600);
        let text = truncated(&line);
        assert!(text.ends_with("..."), "{text}");
        assert!(text.chars().count() < line.chars().count(), "{text}");
    }
}
