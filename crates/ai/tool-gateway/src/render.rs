//! Answers carry their context: an agent that gets locations alone has to read every file again.

use std::fmt::Write;
use std::path::Path;

use crate::backend::{
    AnalyzeCallsQuery, BreakpointId, BreakpointInfo, BuildOutcome, BuildProblem, BuildRequest,
    CallHierarchy, CallHierarchyQuery, CallTree, DebugFrame, DebugRunOutcome, DebugRunRequest,
    DebugSessionState, DebugStack, DebugStackQuery, DebugStateQuery, DebugValue, DebugVariable,
    DebugVariables, DebugVariablesQuery, DiagnosticsPage, DiagnosticsQuery, DirectoryTree,
    EditorContext, FileActionOutcome, FileText, GitStatus, HierarchyNode, PathPage, PathQuery,
    ProblemSeverity, ProjectStructure, ProjectStructureQuery, ProposedEdit, RenamedSymbol,
    RunConfiguration, RunOutput, RunPointInfo, SearchPage, SearchQuery, SessionScope, Severity,
    SymbolHit, SymbolInfo, SymbolPage, SymbolQuery, TerminalOutput, TestOutcome, TestsOutcome,
    TestsRequest, TypeHierarchy, TypeHierarchyQuery, UsagePage, UsagesQuery, VcsChanges, VcsQuery,
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

pub(crate) fn usages(query: &UsagesQuery, page: &UsagePage) -> String {
    let UsagePage {
        items,
        total,
        clipped,
        source,
        confidence,
    } = page;
    let what = match query.name.as_deref() {
        Some(name) => name.to_string(),
        None => match query.position() {
            Some((path, line, column)) => format!("{}:{line}:{column}", path.display()),
            None => "the symbol".to_string(),
        },
    };
    let mut text = format!("{total} usages of {what}\n");
    let _ = writeln!(text, "source: {source}, confidence: {}", confidence.name());
    for item in items {
        let path = item.path.display();
        let _ = writeln!(
            text,
            "{path}:{}:{}: {}",
            item.line, item.column, item.content
        );
    }
    if *clipped {
        let shown = items.len();
        let _ = write!(
            text,
            "\nclipped: showing {shown} of {total}, raise the limit for more\n"
        );
    }
    text
}

fn hierarchy_node(text: &mut String, node: &HierarchyNode) {
    let indent = "  ".repeat(node.depth as usize);
    let detail = node
        .detail
        .as_deref()
        .map(|detail| format!(" ({detail})"))
        .unwrap_or_default();
    match &node.location {
        Some(found) => {
            let path = found.path.display();
            let _ = writeln!(
                text,
                "{indent}{}{detail} at {path}:{}:{}",
                node.name, found.line, found.content
            );
        }
        None => {
            let _ = writeln!(text, "{indent}{}{detail}", node.name);
        }
    }
}

pub(crate) fn type_hierarchy(query: &TypeHierarchyQuery, hierarchy: &TypeHierarchy) -> String {
    let TypeHierarchy {
        supertypes,
        subtypes,
        source,
        confidence,
        clipped,
    } = hierarchy;
    let path = query.path.display();
    let mut text = format!("type hierarchy at {path}:{}:{}\n", query.line, query.column);
    let _ = writeln!(text, "source: {source}, confidence: {}", confidence.name());
    if supertypes.is_empty() {
        text.push_str("supertypes: none\n");
    } else {
        let _ = writeln!(text, "supertypes ({}):", supertypes.len());
        for node in supertypes {
            hierarchy_node(&mut text, node);
        }
    }
    if subtypes.is_empty() {
        text.push_str("subtypes: none\n");
    } else {
        let _ = writeln!(text, "subtypes ({}):", subtypes.len());
        for node in subtypes {
            hierarchy_node(&mut text, node);
        }
    }
    if *clipped {
        text.push_str("clipped: not every type is shown\n");
    }
    text
}

pub(crate) fn call_hierarchy(query: &CallHierarchyQuery, hierarchy: &CallHierarchy) -> String {
    let CallHierarchy {
        callers,
        source,
        confidence,
        clipped,
    } = hierarchy;
    let path = query.path.display();
    let mut text = format!(
        "call hierarchy at {path}:{}:{}, {} levels\n",
        query.line, query.column, query.depth
    );
    let _ = writeln!(text, "source: {source}, confidence: {}", confidence.name());
    if callers.is_empty() {
        text.push_str("callers: none\n");
    } else {
        let _ = writeln!(text, "callers ({}):", callers.len());
        for node in callers {
            hierarchy_node(&mut text, node);
        }
    }
    if *clipped {
        text.push_str("clipped: not every caller is shown\n");
    }
    text
}

pub(crate) fn project_structure(
    _query: &ProjectStructureQuery,
    structure: &ProjectStructure,
) -> String {
    let ProjectStructure {
        modules,
        total_modules,
        clipped,
        build_system,
        build_system_version,
        resolved,
        runtime,
        language_level,
        problems,
    } = structure;
    let tool = match build_system_version {
        Some(version) => format!("{build_system} {version}"),
        None => build_system.clone(),
    };
    let resolved = if *resolved {
        "resolved"
    } else {
        "scanned only"
    };
    let mut text = format!("project: {tool}, {resolved}, {total_modules} modules\n");
    let mut environment = Vec::new();
    if let Some(runtime) = runtime {
        environment.push(format!("runtime: {runtime}"));
    }
    if let Some(level) = language_level {
        environment.push(format!("language level: {level}"));
    }
    if !environment.is_empty() {
        let _ = writeln!(text, "{}", environment.join(", "));
    }
    if !problems.is_empty() {
        let _ = writeln!(text, "problems ({}):", problems.len());
        for problem in problems {
            let _ = writeln!(text, "  {problem}");
        }
    }
    let _ = writeln!(text, "modules ({}):", modules.len());
    for module in modules {
        let path = module.path.display();
        let _ = writeln!(text, "  {} at {path}", module.name);
        if !module.source_roots.is_empty() {
            let roots: Vec<String> = module
                .source_roots
                .iter()
                .map(|root| root.display().to_string())
                .collect();
            let _ = writeln!(text, "    sources: {}", roots.join(", "));
        }
        if !module.test_roots.is_empty() {
            let roots: Vec<String> = module
                .test_roots
                .iter()
                .map(|root| root.display().to_string())
                .collect();
            let _ = writeln!(text, "    tests: {}", roots.join(", "));
        }
        if !module.dependencies.is_empty() {
            let more = if module.dependencies_clipped {
                ", more"
            } else {
                ""
            };
            let _ = writeln!(
                text,
                "    dependencies: {}{more}",
                module.dependencies.join(", ")
            );
        }
    }
    if *clipped {
        let shown = modules.len();
        let _ = write!(
            text,
            "\nclipped: showing {shown} of {total_modules} modules\n"
        );
    }
    text
}

pub(crate) fn search(query: &SearchQuery, page: &SearchPage) -> String {
    let SearchPage {
        items,
        total,
        clipped,
        timed_out,
        files,
    } = page;
    let pattern = &query.pattern;
    let how = if query.regex { "regex" } else { "text" };
    let mut text = if *total == 0 {
        format!("no matches for {how} \"{pattern}\"\n")
    } else {
        format!("{total} matches in {files} files for {how} \"{pattern}\"\n")
    };
    for item in items {
        let path = item.path.display();
        let _ = writeln!(
            text,
            "{path}:{}:{}: {}",
            item.line, item.column, item.content
        );
    }
    if *clipped {
        let shown = items.len();
        let _ = write!(
            text,
            "\nclipped: showing {shown} of {total}, raise the limit or narrow the mask\n"
        );
    }
    if *timed_out {
        text.push_str("\nthe time limit stopped the search: the answer is not complete\n");
    }
    text
}

pub(crate) fn file_text(file: &FileText) -> String {
    let path = file.path.display();
    if file.total_lines == 0 {
        let mut text = format!("{path}, empty file");
        if file.dirty {
            text.push_str(", unsaved changes");
        }
        text.push('\n');
        return text;
    }
    let mut text = format!(
        "{path}, lines {}-{} of {}",
        file.start_line, file.end_line, file.total_lines
    );
    if file.dirty {
        text.push_str(", unsaved changes");
    }
    if file.clipped {
        text.push_str(", clipped");
    }
    text.push('\n');
    for (index, line) in file.text.split('\n').enumerate() {
        let number = file.start_line as usize + index;
        let _ = writeln!(text, "{number}|{line}");
    }
    text
}

pub(crate) fn vcs_changes(_query: &VcsQuery, changes: &VcsChanges) -> String {
    let VcsChanges {
        files,
        total,
        clipped,
        branch,
        diff,
    } = changes;
    let branch = branch
        .as_deref()
        .map(|branch| format!("branch {branch}, "))
        .unwrap_or_default();
    let word = if *total == 1 { "file" } else { "files" };
    let mut text = format!("git: {branch}{total} changed {word}\n");
    for file in files {
        let original = file
            .original
            .as_deref()
            .map(|original| format!(" (from {original})"))
            .unwrap_or_default();
        let _ = writeln!(text, "{}: {}{original}", file.status, file.path);
    }
    if *clipped {
        let shown = files.len();
        let _ = write!(
            text,
            "\nclipped: showing {shown} of {total}, raise the limit for more\n"
        );
    }
    if let Some(diff) = diff {
        let _ = write!(text, "\ndiff {}:\n", diff.path);
        if diff.hunks.is_empty() {
            text.push_str("no changes against HEAD\n");
        }
        for hunk in &diff.hunks {
            let _ = writeln!(
                text,
                "@@ -{},{} +{},{} @@",
                hunk.before.0,
                hunk.before.1.saturating_sub(hunk.before.0),
                hunk.after.0,
                hunk.after.1.saturating_sub(hunk.after.0)
            );
            for line in &hunk.removed {
                let _ = writeln!(text, "-{line}");
            }
            for line in &hunk.added {
                let _ = writeln!(text, "+{line}");
            }
        }
        if diff.clipped {
            text.push_str("clipped: not every change is shown\n");
        }
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

/// What the user decided about a proposed edit, with the text the file has after it.
pub(crate) fn proposed_edit(edit: &ProposedEdit) -> String {
    let path = edit.path.display();
    let mut text = match edit.outcome {
        crate::backend::EditOutcome::NoDecision => {
            format!("{path}: no decision, the file is unchanged\n")
        }
        crate::backend::EditOutcome::Rejected => {
            format!("{path}: rejected, the file is unchanged\n")
        }
        outcome => {
            let word = if edit.hunks == 1 { "hunk" } else { "hunks" };
            format!(
                "{path}: {}, {} of {} {word} kept\n",
                outcome.name(),
                edit.kept,
                edit.hunks
            )
        }
    };
    if let Some(body) = &edit.text {
        text.push_str("text:\n");
        text.push_str(body);
        if !body.ends_with('\n') {
            text.push('\n');
        }
    }
    text
}

/// The files a rename changed, with the number of edits.
pub(crate) fn renamed_symbol(rename: &RenamedSymbol) -> String {
    if rename.files.is_empty() {
        return "rename: no changes\n".to_string();
    }
    let files = if rename.files.len() == 1 {
        "file".to_string()
    } else {
        format!("{} files", rename.files.len())
    };
    let edits = if rename.edits == 1 {
        "1 change".to_string()
    } else {
        format!("{} changes", rename.edits)
    };
    let mut text = format!("rename: {edits} in {files}\n");
    for file in &rename.files {
        let _ = writeln!(text, "  {}", file.display());
    }
    text
}

/// Whether an action of the IDE changed a file.
pub(crate) fn file_action(outcome: &FileActionOutcome) -> String {
    let path = outcome.path.display();
    if outcome.changed {
        let edits = if outcome.edits == 1 {
            "1 change".to_string()
        } else {
            format!("{} changes", outcome.edits)
        };
        format!("{path}: changed, {edits}\n")
    } else {
        format!("{path}: unchanged\n")
    }
}

/// A build tool writes its problems as `path:line: message`, `path:[line,column]
/// message` or `path:line:column: message`, with `error: ` or `warning: ` before the
/// message of some tools and a `[ERROR]` or `[WARNING]` prefix on the line of others.
/// This reads those lines without knowing which tool wrote them; a line without a place
/// in the sources stays out of the list.
pub fn build_problems(lines: &[String], root: &Path) -> Vec<BuildProblem> {
    let mut problems: Vec<BuildProblem> = Vec::new();
    for line in lines {
        let Some((severity, text)) = marked_severity(line) else {
            continue;
        };
        let Some((path, number, column, message)) = split_problem(text.trim()) else {
            continue;
        };
        let (severity, message) = named_severity(severity, message);
        let path = Path::new(path);
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            root.join(path)
        };
        let message = message.trim().to_string();
        if message.is_empty()
            || problems.iter().any(|known| {
                known.path == path
                    && known.line == number
                    && known.column == column
                    && known.severity == severity
                    && known.message == message
            })
        {
            continue;
        }
        problems.push(BuildProblem {
            path,
            line: number,
            column,
            severity,
            message,
        });
        if problems.len() >= 50 {
            break;
        }
    }
    problems
}

/// The rest of a line whose bracket prefix names the severity. A line without a prefix is
/// an error from a tool that writes its own `error:` mark or none at all; `None` is for a
/// prefix that says nothing about a problem, such as `[INFO]`.
fn marked_severity(line: &str) -> Option<(ProblemSeverity, &str)> {
    let text = line.trim();
    let Some(rest) = text.strip_prefix('[') else {
        return Some((ProblemSeverity::Error, text));
    };
    let Some(close) = rest.find(']') else {
        return Some((ProblemSeverity::Error, text));
    };
    let severity = match rest[..close].trim().to_ascii_uppercase().as_str() {
        "ERROR" => ProblemSeverity::Error,
        "WARNING" | "WARN" => ProblemSeverity::Warning,
        _ => return None,
    };
    Some((severity, &rest[close + 1..]))
}

/// The severity the tool wrote before the message, as in `error: cannot find it`.
fn named_severity(marked: ProblemSeverity, message: &str) -> (ProblemSeverity, &str) {
    let lower = message.to_ascii_lowercase();
    if lower.starts_with("warning:") {
        return (
            ProblemSeverity::Warning,
            message["warning:".len()..].trim_start(),
        );
    }
    if lower.starts_with("error:") {
        return (
            ProblemSeverity::Error,
            message["error:".len()..].trim_start(),
        );
    }
    (marked, message)
}

/// The path, line, column and message of `path:12: message`, `path:[12,5] message` or
/// `path:12:5: message`.
fn split_problem(text: &str) -> Option<(&str, u32, Option<u32>, &str)> {
    let bytes = text.as_bytes();
    for (index, byte) in bytes.iter().enumerate() {
        if *byte != b':' {
            continue;
        }
        let path = text[..index].trim();
        if !(path.contains('/') || path.contains('\\')) || path.ends_with(']') {
            continue;
        }
        let mut at = index + 1;
        if bytes.get(at) == Some(&b'[') {
            at += 1;
        }
        let start = at;
        while bytes.get(at).is_some_and(|byte| byte.is_ascii_digit()) {
            at += 1;
        }
        if at == start {
            continue;
        }
        let number: u32 = text[start..at].parse().ok()?;
        let mut rest = &text[at..];
        let mut column = None;
        if rest.starts_with(',') {
            // The `[line,column]` form of a build tool.
            let close = rest.find(']')?;
            column = rest[1..close].trim().parse().ok();
            rest = &rest[close + 1..];
        } else if rest.starts_with(':') {
            // The `line:column:` form, where a message follows the third number.
            let digits = &rest[1..];
            let width = digits.bytes().take_while(u8::is_ascii_digit).count();
            if width > 0 && digits.as_bytes().get(width) == Some(&b':') {
                column = digits[..width].parse().ok();
                rest = &digits[width..];
            }
        }
        let message = rest.trim_start_matches([':', ' ', ']']).trim();
        if message.is_empty() {
            continue;
        }
        return Some((path, number, column, message));
    }
    None
}

fn duration_text(milliseconds: u64) -> String {
    if milliseconds >= 1000 {
        format!("{:.1} s", milliseconds as f64 / 1000.)
    } else {
        format!("{milliseconds} ms")
    }
}

pub(crate) fn build(request: &BuildRequest, outcome: &BuildOutcome) -> String {
    let scope = match &request.module {
        Some(module) => format!("module {module}"),
        None => "the project".to_string(),
    };
    let verdict = if outcome.success {
        "succeeded"
    } else {
        "failed"
    };
    let took = duration_text(outcome.duration_ms);
    let mut text = format!("build of {scope} {verdict} in {took}\n");
    let _ = writeln!(text, "output: {}", outcome.output);
    if outcome.problems.is_empty() {
        return text;
    }
    let count = outcome
        .problems
        .iter()
        .filter(|problem| problem.severity == ProblemSeverity::Error)
        .count();
    let _ = writeln!(text, "{count} compile errors:");
    for problem in &outcome.problems {
        let path = problem.path.display();
        let mark = if problem.severity == ProblemSeverity::Warning {
            "warning: "
        } else {
            ""
        };
        let _ = writeln!(text, "{path}:{}: {mark}{}", problem.line, problem.message);
    }
    text
}

pub(crate) fn tests(request: &TestsRequest, outcome: &TestsOutcome) -> String {
    let scope = match (&request.suite, &request.case, &request.package) {
        (Some(suite), Some(case), _) => format!("{suite}.{case}"),
        (Some(suite), None, _) => suite.clone(),
        (None, _, Some(package)) => package.clone(),
        _ => "all tests".to_string(),
    };
    let took = duration_text(outcome.duration_ms);
    let mut text = format!(
        "tests of {scope}: {} passed, {} failed, {} skipped in {took}\n",
        outcome.passed, outcome.failed, outcome.skipped
    );
    let _ = writeln!(text, "output: {}", outcome.output);
    let failed: Vec<&crate::backend::TestCase> = outcome
        .cases
        .iter()
        .filter(|case| case.outcome == TestOutcome::Failed)
        .collect();
    if !failed.is_empty() {
        text.push_str("failed:\n");
    }
    for case in failed {
        let name = match &case.case {
            Some(name) if !case.suite.is_empty() => format!("{}.{name}", case.suite),
            Some(name) => name.clone(),
            None => case.suite.clone(),
        };
        let _ = writeln!(text, "{name}");
        if let Some(message) = &case.message {
            let _ = writeln!(text, "  {message}");
        }
        for line in case.trace.iter().take(3) {
            let _ = writeln!(text, "  {line}");
        }
    }
    text
}

pub(crate) fn run_configurations(configurations: &[RunConfiguration]) -> String {
    if configurations.is_empty() {
        return "no run configurations\n".to_string();
    }
    let count = configurations.len();
    let mut text = format!("{count} run configurations:\n");
    for configuration in configurations {
        let _ = writeln!(text, "{} ({})", configuration.name, configuration.kind);
    }
    text
}

pub(crate) fn run_started(name: &str, id: &str) -> String {
    format!("started {name}: {id}\n")
}

pub(crate) fn run_output(output: &RunOutput) -> String {
    let state = if output.running {
        "running".to_string()
    } else {
        match output.exit_code {
            Some(code) => format!("finished, exit code {code}"),
            None => "stopped".to_string(),
        }
    };
    let mut text = format!("{}: {state}\n", output.id);
    if output.lines.is_empty() {
        text.push_str("no output\n");
        return text;
    }
    let _ = writeln!(text, "last {} lines:", output.lines.len());
    for line in &output.lines {
        let _ = writeln!(text, "{line}");
    }
    text
}

/// One breakpoint, with the properties the user gave it.
fn breakpoint(text: &mut String, info: &BreakpointInfo) {
    let path = info.path.display();
    let state = if !info.enabled {
        "off".to_string()
    } else if info.verified {
        "on, verified".to_string()
    } else {
        "on".to_string()
    };
    let condition = info
        .condition
        .as_deref()
        .map(|condition| format!(", condition: {}", clipped(condition, 200)))
        .unwrap_or_default();
    let message = info
        .log_message
        .as_deref()
        .map(|message| format!(", log: {}", clipped(message, 200)))
        .unwrap_or_default();
    let _ = writeln!(text, "{path}:{} {state}{condition}{message}", info.line);
}

pub(crate) fn breakpoint_set(info: &BreakpointInfo) -> String {
    let mut text = String::from("breakpoint set: ");
    breakpoint(&mut text, info);
    text
}

pub(crate) fn breakpoint_removed(id: &BreakpointId) -> String {
    format!("breakpoint removed: {}:{}\n", id.path.display(), id.line)
}

pub(crate) fn breakpoints(items: &[BreakpointInfo]) -> String {
    if items.is_empty() {
        return "no breakpoints\n".to_string();
    }
    let count = items.len();
    let mut text = format!("{count} breakpoints:\n");
    for info in items {
        breakpoint(&mut text, info);
    }
    text
}

fn debug_frame(text: &mut String, frame: &DebugFrame) {
    match (&frame.path, frame.line) {
        (Some(path), line) if line > 0 => {
            let column = if frame.column > 0 {
                format!(":{}", frame.column)
            } else {
                String::new()
            };
            let _ = writeln!(
                text,
                "#{} {} at {}:{line}{column}",
                frame.index,
                frame.name,
                path.display()
            );
        }
        _ => {
            let _ = writeln!(text, "#{} {}", frame.index, frame.name);
        }
    }
}

/// Where the session is, with the thread and the top frame when it stopped.
pub(crate) fn debug_session(state: &DebugSessionState) -> String {
    let state_text = match state.stopped_reason() {
        Some(reason) => format!("stopped ({reason})"),
        None => state.state.clone(),
    };
    let mut text = format!("debugging: {state_text}\n");
    if let Some(thread) = &state.thread {
        let _ = writeln!(text, "thread: {} #{}", thread.name, thread.id);
    }
    if let Some(frame) = &state.frame {
        debug_frame(&mut text, frame);
    }
    text
}

pub(crate) fn debug_run(request: &DebugRunRequest, outcome: &DebugRunOutcome) -> String {
    let what = match (&request.suite, &request.case, &request.configuration) {
        (Some(suite), Some(case), _) => format!("{suite}.{case}"),
        (Some(suite), None, _) => suite.clone(),
        (None, _, Some(configuration)) => configuration.clone(),
        _ => "the program".to_string(),
    };
    let mut text = format!("debugging {what}: session {}\n", outcome.session);
    text.push_str(&debug_session(&outcome.state));
    text
}

pub(crate) fn debug_stack(stack: &DebugStack, _query: &DebugStackQuery) -> String {
    let head = match &stack.thread {
        Some(thread) => format!("stack of {} #{}", thread.name, thread.id),
        None => "stack".to_string(),
    };
    let mut text = format!("{head}: {} frames\n", stack.total);
    for frame in &stack.frames {
        debug_frame(&mut text, frame);
    }
    if stack.frames.len() < stack.total {
        let shown = stack.frames.len();
        let total = stack.total;
        let _ = writeln!(text, "clipped: showing {shown} of {total} frames");
    }
    text
}

fn debug_variable_tree(
    text: &mut String,
    variable: &DebugVariable,
    depth: usize,
    query: &DebugVariablesQuery,
    shown: &mut usize,
) {
    if *shown >= query.variables {
        return;
    }
    *shown += 1;
    let indent = "  ".repeat(depth + 1);
    let type_name = if variable.type_name.is_empty() {
        String::new()
    } else {
        format!(" ({})", variable.type_name)
    };
    let value = clipped(&variable.value, query.length);
    let _ = writeln!(text, "{indent}{} = {value}{type_name}", variable.name);
    if depth >= query.depth {
        return;
    }
    for child in &variable.children {
        debug_variable_tree(text, child, depth + 1, query, shown);
    }
}

pub(crate) fn debug_variables(variables: &DebugVariables, query: &DebugVariablesQuery) -> String {
    let frame = query.frame;
    let mut text = format!("variables of frame #{frame}\n");
    let mut shown = 0;
    for scope in &variables.scopes {
        if scope.variables.is_empty() {
            continue;
        }
        let _ = writeln!(text, "  {}:", scope.name);
        for variable in &scope.variables {
            debug_variable_tree(&mut text, variable, 0, query, &mut shown);
        }
    }
    if variables.total == 0 {
        text.push_str("variables: none loaded\n");
    } else if variables.clipped || shown >= query.variables {
        let _ = writeln!(
            text,
            "clipped: {} variables, raise the limits for more",
            variables.total
        );
    }
    text
}

pub(crate) fn debug_value(value: &DebugValue) -> String {
    let type_name = if value.type_name.is_empty() {
        String::new()
    } else {
        format!(" ({})", value.type_name)
    };
    format!("{} = {}{type_name}\n", value.expression, value.value)
}

/// The text of a class or a source the language service decompiled.
pub(crate) fn source_text(text: &str) -> String {
    let mut answer = String::from("source:\n");
    answer.push_str(text);
    if !answer.ends_with('\n') {
        answer.push('\n');
    }
    answer
}

fn call_node_json(node: &crate::backend::CallNode) -> serde_json::Value {
    let children: Vec<serde_json::Value> = node.children.iter().map(call_node_json).collect();
    serde_json::json!({
        "name": node.name,
        "detail": node.detail,
        "filePath": node.file_path.as_ref().map(|path| path.display().to_string()),
        "line": node.line,
        "treePath": node.tree_path,
        "children": children,
    })
}

pub(crate) fn call_tree(query: &AnalyzeCallsQuery, tree: &CallTree) -> String {
    let root = tree.root.as_ref().map(call_node_json);
    let answer = serde_json::json!({
        "symbol": tree.symbol,
        "analysisKind": tree.direction.name(),
        "depth": query.depth,
        "root": root,
        "more": tree.more,
    });
    answer.to_string()
}

pub(crate) fn file_problems(path: &Path, page: &DiagnosticsPage) -> String {
    let problems: Vec<serde_json::Value> = page
        .items
        .iter()
        .map(|item| {
            serde_json::json!({
                "severity": item.severity.name(),
                "description": item.message,
                "line": item.line,
                "column": item.column,
                "code": item.code,
            })
        })
        .collect();
    let answer = serde_json::json!({
        "filePath": path.display().to_string(),
        "problems": problems,
        "more": page.next.is_some(),
    });
    answer.to_string()
}

pub(crate) fn lint_results(
    _scope: &SessionScope,
    _severity: Severity,
    reports: &[(std::path::PathBuf, Result<DiagnosticsPage, String>)],
) -> String {
    let files: Vec<serde_json::Value> = reports
        .iter()
        .map(|(path, report)| match report {
            Ok(page) => {
                let problems: Vec<serde_json::Value> = page
                    .items
                    .iter()
                    .map(|item| {
                        serde_json::json!({
                            "severity": item.severity.name(),
                            "description": item.message,
                            "line": item.line,
                            "column": item.column,
                        })
                    })
                    .collect();
                serde_json::json!({
                    "filePath": path.display().to_string(),
                    "problems": problems,
                    "more": page.next.is_some(),
                })
            }
            Err(message) => serde_json::json!({
                "filePath": path.display().to_string(),
                "notAnalyzedReason": message,
                "problems": [],
            }),
        })
        .collect();
    let answer = serde_json::json!({
        "files": files,
        "more": reports
            .iter()
            .any(|(_, report)| report.as_ref().is_ok_and(|page: &DiagnosticsPage| page.next.is_some())),
    });
    answer.to_string()
}

pub(crate) fn project_dependencies(structure: &ProjectStructure) -> String {
    let mut names: Vec<String> = Vec::new();
    for module in &structure.modules {
        for dependency in &module.dependencies {
            if !names.contains(dependency) {
                names.push(dependency.clone());
            }
        }
    }
    let dependencies: Vec<serde_json::Value> = names
        .iter()
        .map(|name| {
            let modules: Vec<&str> = structure
                .modules
                .iter()
                .filter(|module| module.dependencies.contains(name))
                .map(|module| module.name.as_str())
                .collect();
            serde_json::json!({"name": name, "modules": modules})
        })
        .collect();
    serde_json::json!({
        "dependencies": dependencies,
        "resolved": structure.resolved,
    })
    .to_string()
}

pub(crate) fn project_modules(structure: &ProjectStructure) -> String {
    let modules: Vec<serde_json::Value> = structure
        .modules
        .iter()
        .map(|module| {
            serde_json::json!({
                "name": module.name,
                "type": "module",
                "path": module.path.display().to_string(),
                "sourceRoots": module.source_roots.iter().map(|path| path.display().to_string()).collect::<Vec<_>>(),
                "testRoots": module.test_roots.iter().map(|path| path.display().to_string()).collect::<Vec<_>>(),
            })
        })
        .collect();
    serde_json::json!({
        "modules": modules,
        "more": structure.clipped,
    })
    .to_string()
}

pub(crate) fn run_points(scope: &SessionScope, points: &[RunPointInfo]) -> String {
    let points: Vec<serde_json::Value> = points
        .iter()
        .map(|point| {
            serde_json::json!({
                "kind": point.kind,
                "name": point.name,
                "container": point.container,
                "filePath": crate::relative_path(&scope.project, &point.file),
                "line": point.line,
                "framework": point.framework,
            })
        })
        .collect();
    serde_json::json!({"runPoints": points}).to_string()
}

pub(crate) fn run_configurations_details(
    scope: &SessionScope,
    configurations: &[RunConfiguration],
) -> String {
    let configurations: Vec<serde_json::Value> = configurations
        .iter()
        .map(|configuration| {
            serde_json::json!({
                "name": configuration.name,
                "kind": configuration.kind,
                "commandLine": configuration.command_line,
                "workingDirectory": configuration.working_directory.as_ref().map(|path| crate::relative_path(&scope.project, Path::new(path))),
                "envs": configuration.environment.iter().map(|(name, value)| format!("{name}={value}")).collect::<Vec<_>>(),
                "supportsDynamicLaunchOverrides": configuration.supports_dynamic_launch_overrides,
            })
        })
        .collect();
    serde_json::json!({"configurations": configurations}).to_string()
}

pub(crate) fn executed_run(output: &RunOutput, timed_out: bool) -> String {
    let answer = serde_json::json!({
        "id": output.id,
        "running": output.running,
        "exitCode": output.exit_code,
        "fullOutputPath": output.full_output_path.as_ref().map(|path| path.display().to_string()),
        "timedOut": timed_out,
        "output": output.lines.join("\n"),
    });
    answer.to_string()
}

pub(crate) fn created_file(scope: &SessionScope, edit: &ProposedEdit) -> String {
    let answer = serde_json::json!({
        "filePath": crate::relative_path(&scope.project, &edit.path),
        "created": edit.outcome == crate::backend::EditOutcome::Accepted,
        "outcome": edit.outcome.name(),
    });
    answer.to_string()
}

pub(crate) fn opened_file(scope: &SessionScope, path: &Path) -> String {
    serde_json::json!({"filePath": crate::relative_path(&scope.project, path), "opened": true})
        .to_string()
}

pub(crate) fn open_paths(scope: &SessionScope, files: &[std::path::PathBuf]) -> String {
    let files: Vec<String> = files
        .iter()
        .map(|path| crate::relative_path(&scope.project, path))
        .collect();
    serde_json::json!({"files": files}).to_string()
}

pub(crate) fn directory_tree(_scope: &SessionScope, tree: &DirectoryTree) -> String {
    let mut text = String::new();
    for entry in &tree.entries {
        let indent = "  ".repeat(entry.depth);
        let mark = if entry.directory { "/" } else { "" };
        let _ = writeln!(text, "{indent}{}{mark}", entry.name);
    }
    if tree.clipped {
        text.push_str("...\n");
    }
    if tree.timed_out {
        text.push_str("the time limit stopped the answer: the list is not complete\n");
    }
    text
}

pub(crate) fn patch_outcome(edit: &ProposedEdit) -> String {
    edit.outcome.name().to_string()
}

pub(crate) fn patch_results(
    scope: &SessionScope,
    results: &[(std::path::PathBuf, String)],
) -> String {
    let files: Vec<serde_json::Value> = results
        .iter()
        .map(|(path, outcome)| {
            serde_json::json!({
                "filePath": path.display().to_string(),
                "outcome": outcome,
            })
        })
        .collect();
    let answer = serde_json::json!({
        "projectPath": scope.project.display().to_string(),
        "files": files,
    });
    answer.to_string()
}

pub(crate) fn path_search(scope: &SessionScope, query: &PathQuery, page: &PathPage) -> String {
    let files: Vec<String> = page
        .items
        .iter()
        .map(|path| crate::relative_path(&scope.project, path))
        .collect();
    let answer = serde_json::json!({
        "q": query.pattern,
        "files": files,
        "total": page.total,
        "more": page.clipped,
        "timedOut": page.timed_out,
    });
    answer.to_string()
}

pub(crate) fn search_matches(
    scope: &SessionScope,
    _query: &SearchQuery,
    page: &SearchPage,
) -> String {
    let matches: Vec<serde_json::Value> = page
        .items
        .iter()
        .map(|item| {
            serde_json::json!({
                "filePath": crate::relative_path(&scope.project, &item.path),
                "line": item.line,
                "column": item.column,
                "text": item.content,
            })
        })
        .collect();
    let answer = serde_json::json!({
        "matches": matches,
        "total": page.total,
        "more": page.clipped,
        "timedOut": page.timed_out,
    });
    answer.to_string()
}

pub(crate) fn symbol_search(
    scope: &SessionScope,
    query: &SymbolQuery,
    page: &SymbolPage,
    paths: &[String],
) -> String {
    let symbols: Vec<serde_json::Value> = page
        .hits
        .iter()
        .filter(|hit| {
            paths.is_empty()
                || crate::path_allowed(paths, &crate::relative_path(&scope.project, &hit.path))
        })
        .map(|hit| {
            serde_json::json!({
                "name": hit.name,
                "kind": hit.kind,
                "filePath": crate::relative_path(&scope.project, &hit.path),
                "line": hit.line,
                "text": hit.content,
            })
        })
        .collect();
    let answer = serde_json::json!({
        "q": query.query,
        "symbols": symbols,
        "total": symbols.len(),
        "more": page.next.is_some(),
    });
    answer.to_string()
}

pub(crate) fn terminal_output(output: &TerminalOutput) -> String {
    let answer = serde_json::json!({
        "output": output.output,
        "exitCode": output.exit_code,
        "timedOut": output.timed_out,
        "truncated": output.truncated,
    });
    answer.to_string()
}

pub(crate) fn repositories(scope: &SessionScope, roots: &[std::path::PathBuf]) -> String {
    let repositories: Vec<String> = roots
        .iter()
        .map(|path| crate::relative_path(&scope.project, path))
        .collect();
    serde_json::json!({"repositories": repositories}).to_string()
}

pub(crate) fn git_status(scope: &SessionScope, status: &GitStatus) -> String {
    let repositories: Vec<serde_json::Value> = status
        .repositories
        .iter()
        .map(|repository| {
            let files: Vec<serde_json::Value> = repository
                .files
                .iter()
                .map(|file| {
                    serde_json::json!({
                        "path": file.path,
                        "index": file.index,
                        "worktree": file.worktree,
                    })
                })
                .collect();
            serde_json::json!({
                "path": repository.path,
                "branch": repository.branch,
                "files": files,
                "total": repository.total,
                "more": repository.clipped,
            })
        })
        .collect();
    serde_json::json!({
        "repositories": repositories,
        "root": scope.project.display().to_string(),
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use crate::backend::{
        CallHierarchy, CallHierarchyQuery, Confidence, DebugScope, DebugThread, Diagnostic,
        FileText, HierarchyNode, Location, ProjectModule, ProjectStructure, ProjectStructureQuery,
        SearchHit, SearchPage, SearchQuery, Severity, TestCase, TypeHierarchy, TypeHierarchyQuery,
        Usage, UsagePage, UsagesQuery, VcsChanges, VcsDiff, VcsFile, VcsHunk, VcsQuery,
    };

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

    #[test]
    fn usages_count_every_place_and_say_what_was_cut() {
        let query = UsagesQuery {
            path: Some("src/Foo.txt".into()),
            line: Some(1),
            column: Some(1),
            name: None,
            limit: 1,
        };
        let page = UsagePage {
            items: vec![Usage {
                path: "src/Bar.txt".into(),
                line: 3,
                column: 5,
                content: "foo();".to_string(),
            }],
            total: 7,
            clipped: true,
            source: "lsp".to_string(),
            confidence: Confidence::Certain,
        };

        let text = usages(&query, &page);

        assert!(text.starts_with("7 usages of src/Foo.txt:1:1\n"), "{text}");
        assert!(text.contains("src/Bar.txt:3:5: foo();\n"), "{text}");
        assert!(text.contains("showing 1 of 7"), "{text}");
    }

    #[test]
    fn hierarchies_indent_their_levels_and_name_the_place() {
        let query = TypeHierarchyQuery {
            path: "src/Foo.txt".into(),
            line: 4,
            column: 7,
        };
        let hierarchy = TypeHierarchy {
            supertypes: vec![HierarchyNode {
                name: "Base".to_string(),
                detail: None,
                location: Some(Location {
                    path: "src/Base.txt".into(),
                    line: 1,
                    content: "class Base {}".to_string(),
                }),
                depth: 1,
            }],
            subtypes: Vec::new(),
            source: "lsp".to_string(),
            confidence: Confidence::Likely,
            clipped: false,
        };

        let text = type_hierarchy(&query, &hierarchy);

        assert!(
            text.starts_with("type hierarchy at src/Foo.txt:4:7\n"),
            "{text}"
        );
        assert!(
            text.contains("supertypes (1):\n  Base at src/Base.txt:1:class Base {}"),
            "{text}"
        );
        assert!(text.ends_with("subtypes: none\n"), "{text}");

        let calls = CallHierarchy {
            callers: vec![HierarchyNode {
                name: "main".to_string(),
                detail: Some("void main()".to_string()),
                location: None,
                depth: 1,
            }],
            source: "lsp".to_string(),
            confidence: Confidence::Certain,
            clipped: true,
        };
        let query = CallHierarchyQuery {
            path: "src/Foo.txt".into(),
            line: 4,
            column: 7,
            depth: 1,
        };
        let text = call_hierarchy(&query, &calls);
        assert!(
            text.contains("callers (1):\n  main (void main())\n"),
            "{text}"
        );
        assert!(
            text.contains("clipped: not every caller is shown"),
            "{text}"
        );
    }

    #[test]
    fn the_project_structure_nests_roots_under_their_module() {
        let query = ProjectStructureQuery { limit: 1 };
        let structure = ProjectStructure {
            modules: vec![ProjectModule {
                name: "app".to_string(),
                path: ".".into(),
                source_roots: vec!["src/main".into()],
                test_roots: vec!["src/test".into()],
                dependencies: vec!["group:lib:1".to_string()],
                dependencies_clipped: false,
            }],
            total_modules: 2,
            clipped: true,
            build_system: "Tool".to_string(),
            build_system_version: Some("1.0".to_string()),
            resolved: true,
            runtime: Some("21".to_string()),
            language_level: Some("21".to_string()),
            problems: Vec::new(),
        };

        let text = project_structure(&query, &structure);

        assert!(
            text.starts_with("project: Tool 1.0, resolved, 2 modules\n"),
            "{text}"
        );
        assert!(text.contains("runtime: 21, language level: 21\n"), "{text}");
        assert!(
            text.contains("  app at .\n    sources: src/main\n    tests: src/test\n"),
            "{text}"
        );
        assert!(text.contains("dependencies: group:lib:1\n"), "{text}");
        assert!(text.contains("showing 1 of 2 modules"), "{text}");
    }

    #[test]
    fn a_text_search_shows_a_line_for_each_match() {
        let query = SearchQuery {
            pattern: "foo".to_string(),
            regex: false,
            mask: None,
            paths: Vec::new(),
            limit: 1,
            timeout: None,
        };
        let page = SearchPage {
            items: vec![SearchHit {
                path: "src/Foo.txt".into(),
                line: 2,
                column: 4,
                content: "  foo();".to_string(),
            }],
            total: 3,
            clipped: true,
            timed_out: false,
            files: 2,
        };

        let text = search(&query, &page);

        assert!(
            text.starts_with("3 matches in 2 files for text \"foo\"\n"),
            "{text}"
        );
        assert!(text.contains("src/Foo.txt:2:4:   foo();\n"), "{text}");
        assert!(text.contains("showing 1 of 3"), "{text}");
    }

    #[test]
    fn a_file_keeps_the_numbers_of_its_lines() {
        let file = FileText {
            path: "src/Foo.txt".into(),
            text: "one\ntwo".to_string(),
            start_line: 5,
            end_line: 6,
            total_lines: 9,
            dirty: true,
            clipped: false,
        };

        let text = file_text(&file);

        assert_eq!(
            text,
            "src/Foo.txt, lines 5-6 of 9, unsaved changes\n5|one\n6|two\n"
        );
    }

    #[test]
    fn build_errors_come_from_the_lines_of_the_tool() {
        let lines = vec![
            "[INFO] Building demo".to_string(),
            "[ERROR] C:\\work\\demo\\src\\Main.txt:[12,5] cannot find symbol".to_string(),
            "[ERROR] src\\other\\Moved.txt:3: error: package does not exist".to_string(),
            "[ERROR] C:\\work\\demo\\src\\Main.txt:[12,5] cannot find symbol".to_string(),
        ];

        let problems = build_problems(&lines, std::path::Path::new("C:\\work\\demo"));

        assert_eq!(problems.len(), 2, "{problems:?}");
        assert_eq!(problems[0].line, 12);
        assert_eq!(problems[0].column, Some(5));
        assert_eq!(problems[0].severity, ProblemSeverity::Error);
        assert_eq!(problems[0].message, "cannot find symbol");
        assert_eq!(problems[1].line, 3);
        assert_eq!(problems[1].column, None);
        assert_eq!(problems[1].message, "package does not exist");
        assert_eq!(
            problems[1].path,
            std::path::Path::new("C:\\work\\demo").join("src\\other\\Moved.txt")
        );
    }

    #[test]
    fn a_warning_is_not_an_error() {
        let lines = vec![
            "[WARNING] C:\\work\\demo\\src\\Main.txt:[2,1] unused name".to_string(),
            "/work/demo/src/Other.txt:7: warning: unchecked value".to_string(),
            "/work/demo/src/Other.txt:9: error: unknown name".to_string(),
            "[ERROR] /work/demo/src/Other.txt:9: unknown name".to_string(),
        ];

        let problems = build_problems(&lines, std::path::Path::new("C:\\work\\demo"));

        assert_eq!(problems.len(), 3, "{problems:?}");
        assert_eq!(problems[0].severity, ProblemSeverity::Warning);
        assert_eq!(problems[0].column, Some(1));
        assert_eq!(problems[0].message, "unused name");
        assert_eq!(problems[1].severity, ProblemSeverity::Warning);
        assert_eq!(problems[1].line, 7);
        assert_eq!(problems[1].message, "unchecked value");
        assert_eq!(problems[2].severity, ProblemSeverity::Error);
        assert_eq!(problems[2].message, "unknown name");
    }

    #[test]
    fn a_line_without_a_place_stays_out() {
        let lines = [
            "The build failed".to_string(),
            "error: no path here".to_string(),
            "[ERROR] Failed to execute the goal".to_string(),
            "/work/demo/src/Main.txt:4: cannot find symbol".to_string(),
        ];

        let problems = build_problems(&lines, std::path::Path::new("/work/demo"));

        assert_eq!(problems.len(), 1, "{problems:?}");
        assert_eq!(problems[0].line, 4);
    }

    #[test]
    fn a_problem_of_each_file_keeps_its_own_place() {
        let lines = [
            "C:\\work\\demo\\src\\Main.txt:[12,5] cannot find symbol".to_string(),
            "C:\\work\\demo\\src\\Other.txt:[3,2] warning: unused name".to_string(),
            "C:\\work\\demo\\src\\Main.txt:[20,1] unknown name".to_string(),
        ];

        let problems = build_problems(&lines, std::path::Path::new("C:\\work\\demo"));

        assert_eq!(problems.len(), 3, "{problems:?}");
        assert_eq!(
            problems
                .iter()
                .filter(|problem| problem.path.ends_with("Main.txt"))
                .count(),
            2
        );
        assert!(problems[1].path.ends_with("Other.txt"));
        assert_eq!(problems[1].column, Some(2));
    }

    #[test]
    fn the_three_number_form_gives_the_column() {
        let lines = ["src/Main.txt:12:5: unknown name".to_string()];
        let problems = build_problems(&lines, std::path::Path::new("/work/demo"));
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert_eq!(problems[0].column, Some(5));
        assert_eq!(problems[0].message, "unknown name");
    }

    #[test]
    fn a_diff_names_the_file_and_each_range() {
        let query = VcsQuery {
            path: Some("src/Foo.txt".into()),
            limit: 10,
        };
        let changes = VcsChanges {
            files: vec![VcsFile {
                path: "src/Foo.txt".to_string(),
                status: "modified".to_string(),
                original: None,
            }],
            total: 1,
            clipped: false,
            branch: Some("main".to_string()),
            diff: Some(VcsDiff {
                path: "src/Foo.txt".to_string(),
                hunks: vec![VcsHunk {
                    before: (2, 3),
                    after: (2, 4),
                    removed: vec!["old".to_string()],
                    added: vec!["new".to_string()],
                }],
                clipped: false,
            }),
        };

        let text = vcs_changes(&query, &changes);

        assert!(
            text.starts_with("git: branch main, 1 changed file\n"),
            "{text}"
        );
        assert!(text.contains("modified: src/Foo.txt\n"), "{text}");
        assert!(text.contains("@@ -2,1 +2,2 @@\n-old\n+new\n"), "{text}");
    }

    #[test]
    fn a_build_answer_names_the_verdict_the_errors_and_the_time() {
        let request = BuildRequest::default();
        let outcome = BuildOutcome {
            success: false,
            problems: vec![BuildProblem {
                path: "src/Foo.txt".into(),
                line: 4,
                column: None,
                severity: ProblemSeverity::Error,
                message: "cannot find symbol".to_string(),
            }],
            duration_ms: 1250,
            output: "tool-1".to_string(),
        };

        let text = build(&request, &outcome);

        assert!(
            text.starts_with("build of the project failed in 1.2 s\n"),
            "{text}"
        );
        assert!(text.contains("output: tool-1"), "{text}");
        assert!(text.contains("1 compile errors:"), "{text}");
        assert!(text.contains("src/Foo.txt:4: cannot find symbol"), "{text}");
    }

    #[test]
    fn a_test_answer_counts_and_names_every_failure() {
        let request = TestsRequest {
            suite: Some("demo.Vitest".to_string()),
            ..TestsRequest::default()
        };
        let outcome = TestsOutcome {
            passed: 1,
            failed: 1,
            skipped: 0,
            cases: vec![
                TestCase {
                    suite: "demo.Vitest".to_string(),
                    case: Some("passes".to_string()),
                    outcome: TestOutcome::Passed,
                    message: None,
                    trace: Vec::new(),
                    duration_ms: 2,
                },
                TestCase {
                    suite: "demo.Vitest".to_string(),
                    case: Some("fails".to_string()),
                    outcome: TestOutcome::Failed,
                    message: Some("expected 1, got 2".to_string()),
                    trace: vec!["at demo.Vitest.fails(Vitest.code:2)".to_string()],
                    duration_ms: 3,
                },
            ],
            duration_ms: 900,
            output: "tool-2".to_string(),
        };

        let text = tests(&request, &outcome);

        assert!(
            text.starts_with("tests of demo.Vitest: 1 passed, 1 failed, 0 skipped in 900 ms\n"),
            "{text}"
        );
        assert!(text.contains("demo.Vitest.fails\n"), "{text}");
        assert!(text.contains("  expected 1, got 2\n"), "{text}");
        assert!(text.contains("at demo.Vitest.fails"), "{text}");
        assert!(!text.contains("passes"), "a passed case stays out: {text}");
    }

    #[test]
    fn a_run_answer_has_its_state_its_code_and_the_tail() {
        let output = RunOutput {
            id: "run-3".to_string(),
            running: false,
            exit_code: Some(0),
            lines: vec!["hello from run".to_string()],
            full_output_path: None,
        };

        let text = run_output(&output);

        assert!(text.starts_with("run-3: finished, exit code 0\n"), "{text}");
        assert!(text.contains("\nhello from run\n"), "{text}");

        let empty = run_output(&RunOutput {
            id: "run-4".to_string(),
            running: true,
            exit_code: None,
            lines: Vec::new(),
            full_output_path: None,
        });
        assert!(
            empty.contains("running") && empty.contains("no output"),
            "{empty}"
        );
    }

    #[test]
    fn a_breakpoint_shows_its_line_its_state_and_its_properties() {
        let info = BreakpointInfo {
            path: "src/A.txt".into(),
            line: 12,
            enabled: true,
            verified: true,
            condition: Some("x > 1".to_string()),
            log_message: Some("x is {x}".to_string()),
        };

        let text = breakpoint_set(&info);

        assert_eq!(
            text,
            "breakpoint set: src/A.txt:12 on, verified, condition: x > 1, log: x is {x}\n"
        );
        let listed = breakpoints(&[info]);
        assert_eq!(
            listed,
            "1 breakpoints:\nsrc/A.txt:12 on, verified, condition: x > 1, log: x is {x}\n"
        );
        assert_eq!(breakpoints(&[]), "no breakpoints\n");
    }

    #[test]
    fn a_debug_session_names_the_reason_the_thread_and_the_top_frame() {
        let state = DebugSessionState {
            state: "stopped".to_string(),
            reason: Some("breakpoint".to_string()),
            thread: Some(DebugThread {
                id: 1,
                name: "main".to_string(),
            }),
            frame: Some(DebugFrame {
                index: 0,
                name: "run".to_string(),
                path: Some("src/A.txt".into()),
                line: 12,
                column: 3,
            }),
        };

        assert_eq!(
            debug_session(&state),
            "debugging: stopped (breakpoint)\nthread: main #1\n#0 run at src/A.txt:12:3\n"
        );
        let running = DebugSessionState {
            state: "running".to_string(),
            reason: None,
            thread: None,
            frame: None,
        };
        assert_eq!(debug_session(&running), "debugging: running\n");
    }

    #[test]
    fn variables_are_indented_and_cut_to_the_length() {
        let query = DebugVariablesQuery {
            depth: 1,
            length: 5,
            ..DebugVariablesQuery::default()
        };
        let variables = DebugVariables {
            scopes: vec![DebugScope {
                name: "Locals".to_string(),
                variables: vec![DebugVariable {
                    name: "items".to_string(),
                    value: "ArrayList".to_string(),
                    type_name: "List".to_string(),
                    children: vec![DebugVariable {
                        name: "0".to_string(),
                        value: "abcdefgh".to_string(),
                        type_name: "String".to_string(),
                        children: Vec::new(),
                    }],
                }],
            }],
            total: 2,
            clipped: false,
        };

        let text = debug_variables(&variables, &query);

        assert!(
            text.starts_with("variables of frame #0\n  Locals:\n"),
            "{text}"
        );
        assert!(text.contains("  items = Array… (List)\n"), "{text}");
        assert!(text.contains("    0 = abcde… (String)\n"), "{text}");
    }

    #[test]
    fn a_stack_says_how_many_frames_it_left_out() {
        let stack = DebugStack {
            thread: None,
            total: 3,
            frames: vec![DebugFrame {
                index: 0,
                name: "run".to_string(),
                path: None,
                line: 0,
                column: 0,
            }],
        };

        let text = debug_stack(&stack, &DebugStackQuery::default());

        assert!(text.starts_with("stack: 3 frames\n#0 run\n"), "{text}");
        assert!(text.contains("clipped: showing 1 of 3 frames"), "{text}");
    }
}
