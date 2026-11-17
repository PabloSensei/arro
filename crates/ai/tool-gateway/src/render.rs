//! Answers carry their context: an agent that gets locations alone has to read every file again.

use std::fmt::Write;

use crate::backend::{
    DebugStateQuery, DiagnosticsPage, DiagnosticsQuery, EditorContext, SymbolHit, SymbolInfo,
    SymbolPage, SymbolQuery,
};

const CONTEXT_LINES: usize = 2;

pub(crate) fn editor_context(context: &EditorContext) -> String {
    let EditorContext {
        file,
        line,
        column,
        selection,
        open_files,
    } = context;
    let mut text = String::new();
    match file {
        Some(file) => {
            let path = file.display();
            let line = line.unwrap_or(1);
            let column = column.unwrap_or(1);
            let _ = writeln!(text, "active: {path}:{line}:{column}");
        }
        None => text.push_str("active: none\n"),
    }
    if let Some(selection) = selection {
        let _ = writeln!(text, "selection:\n{selection}");
    }
    let _ = writeln!(text, "open files: {}", open_files.len());
    for file in open_files {
        let _ = writeln!(text, "{}", file.display());
    }
    text
}

fn hit(text: &mut String, hit: &SymbolHit) {
    let path = hit.path.display();
    let first = hit.line as usize;
    let before_count = hit.before.len().min(CONTEXT_LINES);
    let before = &hit.before[hit.before.len() - before_count..];
    for (index, line) in before.iter().enumerate() {
        let number = first.saturating_sub(before_count - index);
        let _ = writeln!(text, "{path}-{number}-{line}");
    }
    let _ = writeln!(text, "{path}:{first}:{}", hit.content);
    for (index, line) in hit.after.iter().take(CONTEXT_LINES).enumerate() {
        let _ = writeln!(text, "{path}-{}-{line}", first + index + 1);
    }
}

pub(crate) fn symbols(query: &SymbolQuery, page: &SymbolPage) -> String {
    let SymbolPage {
        hits,
        source,
        confidence,
        next,
    } = page;
    let count = hits.len();
    let needle = &query.query;
    let confidence = confidence.name();
    let mut text = format!("{count} symbols for \"{needle}\"\n");
    let _ = writeln!(text, "source: {source}, confidence: {confidence}");
    for found in hits {
        text.push('\n');
        hit(&mut text, found);
    }
    if let Some(next) = next {
        let _ = write!(text, "\ntruncated, pass after=\"{next}\" for more\n");
    }
    text
}

pub(crate) fn symbol_info(info: &SymbolInfo) -> String {
    let SymbolInfo {
        name,
        kind,
        signature,
        documentation,
        declaration,
        source,
        confidence,
    } = info;
    let mut text = format!("{kind} {name}\n");
    let _ = writeln!(text, "source: {source}, confidence: {}", confidence.name());
    if let Some(signature) = signature {
        let _ = writeln!(text, "signature: {signature}");
    }
    match declaration {
        Some(found) => {
            let path = found.path.display();
            let _ = writeln!(text, "declared at {path}:{}:{}", found.line, found.content);
        }
        None => text.push_str("declaration: unknown\n"),
    }
    if let Some(documentation) = documentation {
        let _ = write!(text, "\n{}\n", documentation.trim_end());
    }
    text
}

pub(crate) fn diagnostics(query: &DiagnosticsQuery, page: &DiagnosticsPage) -> String {
    let DiagnosticsPage {
        items,
        source,
        confidence,
        next,
    } = page;
    let scope = match &query.path {
        Some(path) => path.display().to_string(),
        None => "the project".to_string(),
    };
    let least = query.severity.name();
    let mut text = format!(
        "{} diagnostics in {scope}, {least} and worse\n",
        items.len()
    );
    let _ = writeln!(text, "source: {source}, confidence: {}", confidence.name());
    for item in items {
        let path = item.path.display();
        let severity = item.severity.name();
        let code = item.code.as_deref().map(|code| format!(" [{code}]"));
        let code = code.unwrap_or_default();
        let _ = write!(
            text,
            "\n{path}:{}:{}: {severity}: {}{code}\n",
            item.line, item.column, item.message
        );
        let _ = writeln!(text, "{path}-{}-{}", item.line, item.content);
    }
    if let Some(next) = next {
        let _ = write!(text, "\ntruncated, pass after=\"{next}\" for more\n");
    }
    text
}

/// Shortens a value on a character boundary. A debugger shows strings of any size, and
/// an answer of an agent should stay readable.
fn clipped(text: &str, length: usize) -> String {
    if text.chars().count() <= length {
        return text.to_owned();
    }
    let mut short: String = text.chars().take(length).collect();
    short.push('…');
    short
}

fn debug_variable(
    text: &mut String,
    snapshot: &dap_client::Snapshot,
    variable: &dap_client::Variable,
    depth: usize,
    shown: &mut usize,
    query: &DebugStateQuery,
) {
    if *shown >= query.variables {
        return;
    }
    *shown += 1;
    let indent = "  ".repeat(2 + depth);
    let type_name = if variable.type_name.is_empty() {
        String::new()
    } else {
        format!(" ({})", variable.type_name)
    };
    let value = clipped(&variable.value, query.length);
    let _ = writeln!(text, "{indent}{} = {value}{type_name}", variable.name);
    if depth >= query.depth || variable.reference == 0 {
        return;
    }
    let Some(children) = snapshot.variables.get(&variable.reference) else {
        return;
    };
    for child in children {
        debug_variable(text, snapshot, child, depth + 1, shown, query);
    }
}

/// The answer of `debug_get_state`: a plain sentence when no session runs, and the
/// parts of the snapshot an agent needs to continue the investigation.
pub(crate) fn debug_state(
    snapshot: Option<&dap_client::Snapshot>,
    query: &DebugStateQuery,
) -> String {
    let Some(snapshot) = snapshot else {
        return "debugging is not running\n".to_string();
    };
    let state = match &snapshot.state {
        dap_client::DebugState::Inactive | dap_client::DebugState::Terminated => {
            return "debugging is not running\n".to_string();
        }
        dap_client::DebugState::Starting => "starting".to_string(),
        dap_client::DebugState::Running => "running".to_string(),
        dap_client::DebugState::Stopped { reason } => format!("stopped ({reason})"),
    };
    let virtual_threads = snapshot
        .threads
        .iter()
        .filter(|thread| dap_client::is_virtual_thread(&thread.name))
        .count();
    let platform_threads = snapshot.threads.len() - virtual_threads;
    let mut text = format!("debugging: {state}\n");
    let _ = writeln!(
        text,
        "threads: {platform_threads} platform, {virtual_threads} virtual"
    );
    let thread = snapshot
        .current_thread
        .and_then(|id| snapshot.threads.iter().find(|thread| thread.id == id));
    match thread {
        Some(thread) => {
            let virtual_thread = if dap_client::is_virtual_thread(&thread.name) {
                " (virtual)"
            } else {
                ""
            };
            let _ = writeln!(
                text,
                "thread: {} #{}{virtual_thread}",
                thread.name, thread.id
            );
        }
        None => text.push_str("thread: none\n"),
    }

    let _ = writeln!(
        text,
        "frames: {} (showing {})",
        snapshot.frames.len(),
        snapshot.frames.len().min(query.frames)
    );
    for frame in snapshot.frames.iter().take(query.frames) {
        match &frame.source {
            Some(path) => {
                let _ = writeln!(text, "{}:{}:{}", path.display(), frame.line, frame.name);
            }
            None => {
                let _ = writeln!(text, "{}: unknown source", frame.name);
            }
        }
    }

    let mut shown = 0;
    let mut variables = String::new();
    for scope in &snapshot.scopes {
        let Some(items) = snapshot.variables.get(&scope.reference) else {
            continue;
        };
        if items.is_empty() {
            continue;
        }
        let _ = writeln!(variables, "  {}:", scope.name);
        for variable in items {
            debug_variable(&mut variables, snapshot, variable, 0, &mut shown, query);
        }
        if shown >= query.variables {
            break;
        }
    }
    if variables.is_empty() {
        text.push_str("variables: none loaded\n");
    } else {
        text.push_str("variables:\n");
        text.push_str(&variables);
        if shown >= query.variables {
            let _ = writeln!(text, "  ... more variables");
        }
    }

    let _ = writeln!(text, "breakpoints: {}", snapshot.breakpoints.len());
    for breakpoint in &snapshot.breakpoints {
        let state = if breakpoint.verified {
            "verified"
        } else {
            "unverified"
        };
        let message = breakpoint
            .message
            .as_deref()
            .map(|message| format!(": {message}"))
            .unwrap_or_default();
        let _ = writeln!(
            text,
            "  {}:{} {state}{message}",
            breakpoint.file.display(),
            breakpoint.line
        );
    }
    text
}

#[cfg(test)]
mod tests {
    use crate::backend::{Confidence, Diagnostic, Location, Severity};

    use super::*;

    fn query() -> SymbolQuery {
        SymbolQuery {
            query: "Foo".to_string(),
            kind: None,
            limit: 10,
            after: None,
        }
    }

    fn found() -> SymbolHit {
        SymbolHit {
            name: "Foo".to_string(),
            kind: "class".to_string(),
            path: "src/Foo.txt".into(),
            line: 10,
            content: "class Foo {".to_string(),
            before: vec!["a".to_string(), "b".to_string(), "c".to_string()],
            after: vec!["d".to_string(), "e".to_string(), "f".to_string()],
        }
    }

    #[test]
    fn a_hit_has_its_line_and_two_lines_around_it() {
        let page = SymbolPage {
            hits: vec![found()],
            source: "index".to_string(),
            confidence: Confidence::Likely,
            next: None,
        };

        let text = symbols(&query(), &page);

        let expected = "1 symbols for \"Foo\"\n\
                        source: index, confidence: likely\n\
                        \n\
                        src/Foo.txt-8-b\n\
                        src/Foo.txt-9-c\n\
                        src/Foo.txt:10:class Foo {\n\
                        src/Foo.txt-11-d\n\
                        src/Foo.txt-12-e\n";
        assert_eq!(text, expected);
    }

    #[test]
    fn truncation_says_how_to_continue() {
        let page = SymbolPage {
            hits: Vec::new(),
            source: "index".to_string(),
            confidence: Confidence::Unknown,
            next: Some("cursor-1".to_string()),
        };

        assert!(symbols(&query(), &page).contains("after=\"cursor-1\""));
    }

    #[test]
    fn symbol_info_names_the_declaration_line() {
        let info = SymbolInfo {
            name: "Foo".to_string(),
            kind: "class".to_string(),
            signature: Some("class Foo extends Bar".to_string()),
            documentation: Some("A foo.\n".to_string()),
            declaration: Some(Location {
                path: "src/Foo.txt".into(),
                line: 10,
                content: "class Foo {".to_string(),
            }),
            source: "lsp".to_string(),
            confidence: Confidence::Certain,
        };

        let expected = "class Foo\n\
                        source: lsp, confidence: certain\n\
                        signature: class Foo extends Bar\n\
                        declared at src/Foo.txt:10:class Foo {\n\
                        \n\
                        A foo.\n";
        assert_eq!(symbol_info(&info), expected);
    }

    #[test]
    fn a_diagnostic_has_its_place_severity_and_line() {
        let query = DiagnosticsQuery {
            path: None,
            severity: Severity::Warning,
            limit: 10,
            after: None,
        };
        let page = DiagnosticsPage {
            items: vec![Diagnostic {
                path: "src/Foo.txt".into(),
                line: 3,
                column: 5,
                severity: Severity::Error,
                message: "cannot find symbol".to_string(),
                code: Some("E1".to_string()),
                content: "    bar();".to_string(),
            }],
            source: "lsp".to_string(),
            confidence: Confidence::Certain,
            next: Some("2".to_string()),
        };

        let text = diagnostics(&query, &page);

        assert!(text.starts_with("1 diagnostics in the project, warning and worse\n"));
        assert!(text.contains("\nsrc/Foo.txt:3:5: error: cannot find symbol [E1]\n"));
        assert!(text.contains("src/Foo.txt-3-    bar();\n"));
        assert!(text.contains("after=\"2\""));
    }

    #[test]
    fn no_open_file_is_said_plainly() {
        let text = editor_context(&EditorContext::default());

        assert!(text.starts_with("active: none\n"));
    }
}
