//! Tools answered by the language layer through `lang-api`. Languages register when their server
//! is up; a file of no registered language gets a plain error, never an invented answer.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, RwLock};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use futures_core::Stream;
use lang_api::{
    ActionKind, ActionQuery, Answer, CallDirection, CallHierarchyItem,
    CallHierarchyQuery as LangCallHierarchy, DiagnosticsQuery as LangDiagnostics, DocumentEdit,
    DocumentSpot, FormatQuery, HierarchyDirection, HierarchyQuery, LanguageSupport, NavigationKind,
    NavigationQuery, RenameQuery, Replies, SourceEdit, SourceSpan, SymbolQuery as LangSymbolQuery,
    TextPoint, TextSpan, WorkspaceChange,
};
use platform::{CanonPath, path_from_uri};

use crate::backend::{
    AnalyzeCallsQuery, BreakpointId, BreakpointInfo, BreakpointQuery, BuildOutcome, BuildRequest,
    CallHierarchy, CallHierarchyQuery, CallNode, CallTree, Confidence, DatabaseControl,
    DebugControl, DebugEvaluateQuery, DebugRunOutcome, DebugRunRequest, DebugSessionState,
    DebugSnapshotSource, DebugStack, DebugStackQuery, DebugStepQuery, DebugValue, DebugVariables,
    DebugVariablesQuery, DebugWaitQuery, Diagnostic, DiagnosticsPage, DiagnosticsQuery,
    EditorContext, FileContent, FileText, HierarchyNode, Location, Position, ProjectRunner,
    READ_LINE_LIMIT, ReadFileQuery, RenameSymbolQuery, RunConfiguration, RunOutput, RunPointInfo,
    RunPointQuery, SessionScope, Severity, StartRunRequest, SymbolInfo, SymbolPage, SymbolQuery,
    TestsOutcome, TestsRequest, ToolBackend, ToolError, TypeHierarchy, TypeHierarchyQuery, Usage,
    UsagePage, UsagesQuery,
};
use crate::database_tools::{DatabaseAnswer, DatabaseQuery};
use crate::xdebug::{DebugAnswer, DebugQuery};

/// How long a tool waits for the first answer of the server.
const ANSWER_WAIT: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(10);

/// The answer of a provider, read without an executor: the gateway runs on a worker thread.
enum Outcome<T> {
    Answer(Answer<T>),
    /// The stream ended without an answer: the question went stale.
    Ended,
    TimedOut,
}

fn first_answer<T>(mut replies: Replies<T>, wait: Duration) -> Outcome<T> {
    let deadline = Instant::now() + wait;
    let mut context = Context::from_waker(Waker::noop());
    loop {
        match Pin::new(&mut replies).poll_next(&mut context) {
            Poll::Ready(Some(answer)) => return Outcome::Answer(answer),
            Poll::Ready(None) => return Outcome::Ended,
            Poll::Pending if Instant::now() >= deadline => return Outcome::TimedOut,
            Poll::Pending => std::thread::sleep(POLL),
        }
    }
}

/// The value of an answer, or the error the agent should see instead.
fn value_of<T>(outcome: Outcome<T>, what: &str) -> Result<(T, Confidence, String), ToolError> {
    let answer = match outcome {
        Outcome::Answer(answer) => answer,
        Outcome::Ended => {
            return Err(ToolError::Failed(format!(
                "the {what} question went stale while the file changed, ask again"
            )));
        }
        Outcome::TimedOut => {
            return Err(ToolError::Failed(format!(
                "the language server gave no {what} in time, ask again later"
            )));
        }
    };
    let confidence = match answer.confidence {
        lang_api::Confidence::Certain => Confidence::Certain,
        lang_api::Confidence::Partial => Confidence::Likely,
        lang_api::Confidence::Unknown => {
            return Err(ToolError::Failed(format!(
                "the language server could not answer the {what} question: it is not ready, \
                 timed out or failed. This does not mean there is nothing"
            )));
        }
    };
    Ok((answer.value, confidence, answer.source.name().to_string()))
}

fn uri_of(path: &Path) -> Result<String, ToolError> {
    let canonical = CanonPath::new(path).map_err(|error| ToolError::Failed(error.to_string()))?;
    Ok(canonical.to_uri())
}

fn line_text(path: &Path, line: u32) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let found = text.lines().nth(line as usize)?;
    Some(found.trim_end_matches('\r').to_string())
}

/// 1-based characters into zero-based UTF-16 units of `line`.
fn utf16_of(line: &str, column: u32) -> u32 {
    let before = line.chars().take(column.saturating_sub(1) as usize);
    before.map(|ch| ch.len_utf16() as u32).sum()
}

/// Zero-based UTF-16 units of `line` into 1-based characters.
fn column_of(line: &str, character: u32) -> u32 {
    let mut units = 0;
    let mut column = 1;
    for ch in line.chars() {
        if units >= character {
            break;
        }
        units += ch.len_utf16() as u32;
        column += 1;
    }
    column
}

/// The identifier under a column, the name shown for the symbol.
fn word_at(line: &str, column: u32) -> Option<String> {
    let chars: Vec<char> = line.chars().collect();
    let at = (column as usize).checked_sub(1)?;
    let part = |ch: &char| ch.is_alphanumeric() || *ch == '_' || *ch == '$';
    if !chars.get(at).is_some_and(part) {
        return None;
    }
    let start = (0..=at)
        .rev()
        .take_while(|&index| part(&chars[index]))
        .last()?;
    let end = (at..chars.len())
        .take_while(|&index| part(&chars[index]))
        .last()?;
    Some(chars[start..=end].iter().collect())
}

/// The span of the whole text, the shape Organize Imports asks about.
fn whole_span(text: &str) -> TextSpan {
    let point = |offset: usize| {
        let before = &text[..offset.min(text.len())];
        let line = before.bytes().filter(|byte| *byte == b'\n').count() as u32;
        let start = before.rfind('\n').map_or(0, |index| index + 1);
        TextPoint {
            line,
            character: before[start..].encode_utf16().count() as u32,
        }
    };
    TextSpan {
        start: point(0),
        end: point(text.len()),
    }
}

/// The first line of the first code block of hover Markdown, which servers fill with the
/// declaration.
fn signature_of(markdown: &str) -> Option<String> {
    let mut lines = markdown.lines();
    lines.find(|line| line.trim_start().starts_with("```"))?;
    let first = lines.next()?.trim();
    (!first.is_empty() && !first.starts_with("```")).then(|| first.to_string())
}

fn severity_of(severity: lang_api::Severity) -> Severity {
    match severity {
        lang_api::Severity::Error => Severity::Error,
        lang_api::Severity::Warning => Severity::Warning,
        lang_api::Severity::Information => Severity::Info,
        lang_api::Severity::Hint => Severity::Hint,
    }
}

const NOT_CONNECTED: &str = "not connected yet";

/// More nodes than this would not fit an answer; the rest is reported as clipped.
const MAX_NODES: usize = 200;

/// The languages whose servers are up, looked up by the file a tool asks about.
#[derive(Default)]
pub struct LanguageTools {
    languages: RwLock<Vec<Arc<dyn LanguageSupport>>>,
    debug: RwLock<Vec<(PathBuf, Arc<dyn DebugSnapshotSource>)>>,
    debug_controls: RwLock<Vec<(PathBuf, Arc<dyn DebugControl>)>>,
    databases: RwLock<Vec<(PathBuf, Arc<dyn DatabaseControl>)>>,
    projects: RwLock<Vec<(PathBuf, Arc<dyn ProjectRunner>)>>,
    wait: Option<Duration>,
}

impl LanguageTools {
    pub fn new() -> Self {
        Self::default()
    }

    /// For tests that should not wait long for a silent provider.
    pub fn with_wait(wait: Duration) -> Self {
        Self {
            wait: Some(wait),
            ..Self::default()
        }
    }

    /// A language with a running server. Replaces an earlier one with the same id.
    pub fn register(&self, language: Arc<dyn LanguageSupport>) {
        let mut languages = self
            .languages
            .write()
            .unwrap_or_else(|error| error.into_inner());
        languages.retain(|known| known.id() != language.id());
        languages.push(language);
    }

    pub fn unregister(&self, id: lang_api::LanguageId) {
        let mut languages = self
            .languages
            .write()
            .unwrap_or_else(|error| error.into_inner());
        languages.retain(|known| known.id() != id);
    }

    /// The ids of the languages whose servers are up, in the order they registered.
    pub fn ids(&self) -> Vec<lang_api::LanguageId> {
        let languages = self
            .languages
            .read()
            .unwrap_or_else(|error| error.into_inner());
        languages.iter().map(|language| language.id()).collect()
    }

    /// Makes `debug_get_state` read the session of this project. Replaces an earlier
    /// source of the same project.
    pub fn register_debug(&self, project: PathBuf, source: Arc<dyn DebugSnapshotSource>) {
        let mut debug = self
            .debug
            .write()
            .unwrap_or_else(|error| error.into_inner());
        debug.retain(|(known, _)| !same_project(known, &project));
        debug.push((project, source));
    }

    pub fn unregister_debug(&self, project: &Path) {
        let mut debug = self
            .debug
            .write()
            .unwrap_or_else(|error| error.into_inner());
        debug.retain(|(known, _)| !same_project(known, project));
    }

    fn debug_source(&self, project: &Path) -> Option<Arc<dyn DebugSnapshotSource>> {
        let debug = self.debug.read().unwrap_or_else(|error| error.into_inner());
        debug
            .iter()
            .rev()
            .find(|(known, _)| same_project(known, project))
            .map(|(_, source)| source.clone())
    }

    /// Makes the debug tools reach the Debug window of this project. Replaces an earlier
    /// control of the same project.
    pub fn register_debug_control(&self, project: PathBuf, control: Arc<dyn DebugControl>) {
        let mut controls = self
            .debug_controls
            .write()
            .unwrap_or_else(|error| error.into_inner());
        controls.retain(|(known, _)| !same_project(known, &project));
        controls.push((project, control));
    }

    pub fn unregister_debug_control(&self, project: &Path) {
        let mut controls = self
            .debug_controls
            .write()
            .unwrap_or_else(|error| error.into_inner());
        controls.retain(|(known, _)| !same_project(known, project));
    }

    fn debug_control_for(&self, project: &Path) -> Result<Arc<dyn DebugControl>, ToolError> {
        let controls = self
            .debug_controls
            .read()
            .unwrap_or_else(|error| error.into_inner());
        controls
            .iter()
            .rev()
            .find(|(known, _)| same_project(known, project))
            .map(|(_, control)| control.clone())
            .ok_or_else(|| {
                ToolError::Failed(format!(
                    "the Debug window of {} is not connected yet",
                    project.display()
                ))
            })
    }

    /// Makes the database tools reach the Database window of this project. Replaces an
    /// earlier control of the same project.
    pub fn register_database_control(&self, project: PathBuf, control: Arc<dyn DatabaseControl>) {
        let mut controls = self
            .databases
            .write()
            .unwrap_or_else(|error| error.into_inner());
        controls.retain(|(known, _)| !same_project(known, &project));
        controls.push((project, control));
    }

    pub fn unregister_database_control(&self, project: &Path) {
        let mut controls = self
            .databases
            .write()
            .unwrap_or_else(|error| error.into_inner());
        controls.retain(|(known, _)| !same_project(known, project));
    }

    fn database_control_for(&self, project: &Path) -> Result<Arc<dyn DatabaseControl>, ToolError> {
        let controls = self
            .databases
            .read()
            .unwrap_or_else(|error| error.into_inner());
        controls
            .iter()
            .rev()
            .find(|(known, _)| same_project(known, project))
            .map(|(_, control)| control.clone())
            .ok_or_else(|| {
                ToolError::Failed(format!(
                    "the Database window of {} is not connected yet",
                    project.display()
                ))
            })
    }

    /// Makes the tools that run the project reach the Run window of this project.
    /// Replaces an earlier runner of the same project.
    pub fn register_project(&self, project: PathBuf, runner: Arc<dyn ProjectRunner>) {
        let mut projects = self
            .projects
            .write()
            .unwrap_or_else(|error| error.into_inner());
        projects.retain(|(known, _)| !same_project(known, &project));
        projects.push((project, runner));
    }

    pub fn unregister_project(&self, project: &Path) {
        let mut projects = self
            .projects
            .write()
            .unwrap_or_else(|error| error.into_inner());
        projects.retain(|(known, _)| !same_project(known, project));
    }

    fn project_for(&self, project: &Path) -> Result<Arc<dyn ProjectRunner>, ToolError> {
        let projects = self
            .projects
            .read()
            .unwrap_or_else(|error| error.into_inner());
        projects
            .iter()
            .rev()
            .find(|(known, _)| same_project(known, project))
            .map(|(_, runner)| runner.clone())
            .ok_or_else(|| {
                ToolError::Failed(format!(
                    "the Run window of {} is not connected yet",
                    project.display()
                ))
            })
    }

    fn language_for(&self, path: &Path) -> Result<Arc<dyn LanguageSupport>, ToolError> {
        let languages = self
            .languages
            .read()
            .unwrap_or_else(|error| error.into_inner());
        let found = languages.iter().find(|language| language.matches(path));
        found.cloned().ok_or_else(|| {
            let shown = path.display();
            ToolError::Failed(format!("no language server runs for {shown}"))
        })
    }

    fn wait(&self) -> Duration {
        self.wait.unwrap_or(ANSWER_WAIT)
    }

    /// The place the language server was told about, with the text of its line.
    fn spot_of(
        &self,
        language: &dyn LanguageSupport,
        path: &Path,
        line: u32,
        column: u32,
    ) -> Result<(DocumentSpot, String), ToolError> {
        let uri = uri_of(path)?;
        let version = Self::open_version(language, &uri, path)?;
        let content = line_text(path, line - 1).unwrap_or_default();
        let spot = DocumentSpot {
            uri,
            version,
            point: TextPoint {
                line: line - 1,
                character: utf16_of(&content, column),
            },
        };
        Ok((spot, content))
    }

    /// A place of a language server, which may be a source that is not a file, such as a
    /// class of a library.
    fn location_of(&self, span: &SourceSpan) -> Location {
        let line = span.span.start.line;
        match path_from_uri(&span.uri) {
            Some(path) => Location {
                content: line_text(&path, line).unwrap_or_default(),
                path,
                line: line + 1,
            },
            None => Location {
                path: PathBuf::from(span.uri.clone()),
                line: line + 1,
                content: String::new(),
            },
        }
    }

    /// The language that can look a name up in the project.
    fn language_with_symbols(&self) -> Result<Arc<dyn LanguageSupport>, ToolError> {
        let languages = self
            .languages
            .read()
            .unwrap_or_else(|error| error.into_inner());
        languages
            .iter()
            .find(|language| language.workspace_symbols().is_some())
            .cloned()
            .ok_or_else(|| {
                ToolError::Failed(
                    "no language looks symbols up by name, pass a file, a line and a column"
                        .to_string(),
                )
            })
    }

    /// The place of the named declaration, found through the symbol search of the server.
    fn declaration_spot(
        &self,
        language: &dyn LanguageSupport,
        name: &str,
    ) -> Result<DocumentSpot, ToolError> {
        let provider = language
            .workspace_symbols()
            .ok_or_else(|| missing("symbol search"))?;
        let outcome = first_answer(
            lang_api::ask(provider, LangSymbolQuery(name.to_string())),
            self.wait(),
        );
        let (entries, _, _) = value_of(outcome, "symbol search")?;
        let found = entries
            .into_iter()
            .find(|entry| entry.name == name && entry.place.is_some());
        let Some(place) = found.and_then(|entry| entry.place) else {
            return Err(ToolError::Failed(format!(
                "no declaration named {name} was found, pass a file, a line and a column"
            )));
        };
        let server = language
            .server()
            .ok_or_else(|| missing("language server"))?;
        let version = server.version(&place.uri).ok_or_else(|| {
            ToolError::Failed(format!(
                "the declaration of {name} is at {}, which is not open in the editor",
                place.uri
            ))
        })?;
        Ok(DocumentSpot {
            uri: place.uri,
            version,
            point: place.span.start,
        })
    }

    /// The lines of `query` from a text, as the editor would show them.
    fn range_of(&self, path: &Path, text: &str, query: &ReadFileQuery) -> FileText {
        let mut lines: Vec<&str> = if text.is_empty() {
            Vec::new()
        } else {
            text.split('\n').collect()
        };
        if text.ends_with('\n') {
            lines.pop();
        }
        let total = lines.len() as u32;
        let requested_start = query.start_line.unwrap_or(1).max(1);
        let requested_end = query
            .end_line
            .unwrap_or(requested_start.saturating_add(READ_LINE_LIMIT - 1));
        if total == 0 {
            return FileText {
                path: path.to_path_buf(),
                text: String::new(),
                start_line: 1,
                end_line: 0,
                total_lines: 0,
                dirty: false,
                clipped: false,
            };
        }
        let start = requested_start.min(total).max(1);
        let last = requested_end
            .min(total)
            .min(start.saturating_add(READ_LINE_LIMIT - 1));
        let end = last.max(start);
        let text = lines[start as usize - 1..end as usize].join("\n");
        FileText {
            path: path.to_path_buf(),
            text,
            start_line: start,
            end_line: end,
            total_lines: total,
            dirty: false,
            clipped: start != requested_start || end < requested_end,
        }
    }

    /// The version the server has of an open file, the only text it can answer about.
    fn open_version(
        language: &dyn LanguageSupport,
        uri: &str,
        path: &Path,
    ) -> Result<i32, ToolError> {
        let server = language
            .server()
            .ok_or_else(|| missing("language server"))?;
        server.version(uri).ok_or_else(|| {
            let shown = path.display();
            ToolError::Failed(format!(
                "{shown} is not open in the editor, the language server only answers about \
                 open files. Ask the user to open it"
            ))
        })
    }

    fn declaration(&self, language: &dyn LanguageSupport, spot: DocumentSpot) -> Option<Location> {
        let navigation = language.navigation()?;
        let query = NavigationQuery {
            kind: NavigationKind::Definition,
            spot,
        };
        let replies = lang_api::ask(navigation, query);
        let Outcome::Answer(answer) = first_answer(replies, self.wait()) else {
            return None;
        };
        let span = answer.value.into_iter().next()?;
        Some(self.location_of(&span))
    }

    /// The edits a rename of the symbol at a place makes, through the server of the
    /// language of the file. Nothing is written here.
    pub fn rename_change(&self, query: &RenameSymbolQuery) -> Result<WorkspaceChange, ToolError> {
        let language = self.language_for(&query.path)?;
        let (spot, _) = self.spot_of(language.as_ref(), &query.path, query.line, query.column)?;
        let provider = language.rename().ok_or_else(|| missing("rename"))?;
        let asked = RenameQuery {
            spot,
            new_name: query.new_name.clone(),
        };
        let outcome = first_answer(lang_api::ask(provider, asked), self.wait());
        let (change, _, _) = value_of(outcome, "rename")?;
        Ok(change)
    }

    /// The edits that format a file, through the server of its language. The file has to
    /// be open: a server answers about the documents it holds.
    pub fn format_edits(&self, path: &Path) -> Result<Vec<DocumentEdit>, ToolError> {
        let language = self.language_for(path)?;
        let uri = uri_of(path)?;
        let version = Self::open_version(language.as_ref(), &uri, path)?;
        let provider = language.format().ok_or_else(|| missing("formatting"))?;
        let asked = FormatQuery {
            uri,
            version,
            tab_size: 4,
            insert_spaces: true,
        };
        let outcome = first_answer(lang_api::ask(provider, asked), self.wait());
        let (edits, _, _) = value_of(outcome, "formatting")?;
        Ok(edits)
    }

    /// The edits of Organize Imports for a file, through the server of its language.
    /// `text` is the text of the document the server holds, which the span covers.
    pub fn organize_imports_edits(
        &self,
        path: &Path,
        text: &str,
    ) -> Result<Vec<SourceEdit>, ToolError> {
        let language = self.language_for(path)?;
        let uri = uri_of(path)?;
        let version = Self::open_version(language.as_ref(), &uri, path)?;
        let provider = language.actions().ok_or_else(|| missing("actions"))?;
        let asked = ActionQuery {
            uri,
            version,
            span: whole_span(text),
            only: Some(ActionKind::OrganizeImports),
        };
        let outcome = first_answer(lang_api::ask(provider, asked), self.wait());
        let (entries, _, _) = value_of(outcome, "organize imports")?;
        Ok(entries
            .into_iter()
            .next()
            .map(|entry| entry.edits)
            .unwrap_or_default())
    }
}

/// Whether two paths name the same project. The comparison is case-insensitive on
/// Windows and ignores a trailing separator.
fn same_project(left: &Path, right: &Path) -> bool {
    let normalize = |path: &Path| {
        path.to_string_lossy()
            .trim_end_matches(['\\', '/'])
            .to_lowercase()
    };
    normalize(left) == normalize(right)
}

fn missing(what: &str) -> ToolError {
    ToolError::Failed(format!("this language has no {what}"))
}

/// The node at a `treePath` of the roots, with the path's place among the siblings.
fn take_tree_path(mut nodes: Vec<CallNode>, path: &[usize]) -> Option<CallNode> {
    if path.is_empty() {
        return nodes.drain(..).next();
    }
    let mut nodes = nodes.drain(..);
    let mut node = nodes.nth(*path.first()?)?;
    for index in &path[1..] {
        node = node.children.into_iter().nth(*index)?;
    }
    Some(node)
}

impl ToolBackend for LanguageTools {
    fn editor_context(&self, _: &SessionScope) -> Result<EditorContext, ToolError> {
        Err(ToolError::Failed(format!(
            "the editor context is {NOT_CONNECTED}"
        )))
    }

    fn find_symbol(&self, _: &SessionScope, _: &SymbolQuery) -> Result<SymbolPage, ToolError> {
        Err(ToolError::Failed(format!(
            "the symbol index is {NOT_CONNECTED}"
        )))
    }

    fn symbol_info(&self, _: &SessionScope, position: &Position) -> Result<SymbolInfo, ToolError> {
        let language = self.language_for(&position.path)?;
        let (spot, line) = self.spot_of(
            language.as_ref(),
            &position.path,
            position.line,
            position.column,
        )?;
        let hover = language.hover().ok_or_else(|| missing("hover"))?;
        let outcome = first_answer(lang_api::ask(hover, spot.clone()), self.wait());
        let (text, confidence, source) = value_of(outcome, "hover")?;
        let markdown = text.0;
        let name = word_at(&line, position.column).unwrap_or_else(|| "symbol".to_string());
        Ok(SymbolInfo {
            name,
            kind: "symbol".to_string(),
            signature: signature_of(&markdown),
            documentation: (!markdown.trim().is_empty()).then_some(markdown),
            declaration: self.declaration(language.as_ref(), spot),
            source,
            confidence,
        })
    }

    fn diagnostics(
        &self,
        _: &SessionScope,
        query: &DiagnosticsQuery,
    ) -> Result<DiagnosticsPage, ToolError> {
        let Some(path) = &query.path else {
            return Err(ToolError::BadInput(
                "pass a path: diagnostics come one file at a time for now".to_string(),
            ));
        };
        let language = self.language_for(path)?;
        let provider = language
            .diagnostics()
            .ok_or_else(|| missing("diagnostics"))?;
        let uri = uri_of(path)?;
        // The stream stays open for later changes; the first list is the answer, then it closes.
        let outcome = first_answer(lang_api::ask(provider, LangDiagnostics(uri)), self.wait());
        let (entries, confidence, source) = value_of(outcome, "diagnostics")?;
        let text = std::fs::read_to_string(path).unwrap_or_default();
        let lines: Vec<&str> = text.lines().collect();
        let mut items: Vec<Diagnostic> = entries
            .into_iter()
            .map(|entry| {
                let start = entry.span.start;
                let content = lines.get(start.line as usize).copied().unwrap_or_default();
                let content = content.trim_end_matches('\r');
                Diagnostic {
                    path: path.clone(),
                    line: start.line + 1,
                    column: column_of(content, start.character),
                    severity: severity_of(entry.severity),
                    message: entry.message,
                    code: None,
                    content: content.to_string(),
                }
            })
            .filter(|item| item.severity <= query.severity)
            .collect();
        items.sort_by_key(|item| (item.severity, item.line, item.column));
        let skip: usize = match &query.after {
            Some(after) => after
                .parse()
                .map_err(|_| ToolError::BadInput(format!("bad continuation token {after}")))?,
            None => 0,
        };
        let total = items.len();
        let limit = query.limit.max(1);
        let page: Vec<Diagnostic> = items.into_iter().skip(skip).take(limit).collect();
        let next = (skip + page.len() < total).then(|| (skip + page.len()).to_string());
        Ok(DiagnosticsPage {
            items: page,
            source,
            confidence,
            next,
        })
    }

    fn debug_snapshot(&self, scope: &SessionScope) -> Option<Arc<dap_client::Snapshot>> {
        self.debug_source(&scope.project)
            .and_then(|source| source.snapshot())
    }

    fn find_usages(&self, _: &SessionScope, query: &UsagesQuery) -> Result<UsagePage, ToolError> {
        let (language, spot) = match query.position() {
            Some((path, line, column)) => {
                let language = self.language_for(path)?;
                let (spot, _) = self.spot_of(language.as_ref(), path, line, column)?;
                (language, spot)
            }
            None => {
                let name = query
                    .name
                    .as_deref()
                    .ok_or_else(|| ToolError::BadInput("pass a place or a name".to_string()))?;
                let language = self.language_with_symbols()?;
                let spot = self.declaration_spot(language.as_ref(), name)?;
                (language, spot)
            }
        };
        let provider = language.references().ok_or_else(|| missing("references"))?;
        let outcome = first_answer(lang_api::ask(provider, spot), self.wait());
        let (spans, confidence, source) = value_of(outcome, "references")?;
        let mut texts: HashMap<PathBuf, Option<String>> = HashMap::new();
        let mut items: Vec<Usage> = spans
            .into_iter()
            .filter_map(|span| {
                let path = path_from_uri(&span.uri)?;
                let line = span.span.start.line;
                let text = texts
                    .entry(path.clone())
                    .or_insert_with(|| std::fs::read_to_string(&path).ok());
                let content = text
                    .as_deref()?
                    .lines()
                    .nth(line as usize)?
                    .trim_end_matches('\r')
                    .to_string();
                Some(Usage {
                    path,
                    line: line + 1,
                    column: column_of(&content, span.span.start.character),
                    content,
                })
            })
            .collect();
        items.sort_by(|left, right| {
            (&left.path, left.line, left.column).cmp(&(&right.path, right.line, right.column))
        });
        items.dedup_by(|left, right| {
            left.path == right.path && left.line == right.line && left.column == right.column
        });
        let total = items.len();
        let limit = query.limit.max(1);
        let clipped = total > limit;
        items.truncate(limit);
        Ok(UsagePage {
            items,
            total,
            clipped,
            source,
            confidence,
        })
    }

    fn type_hierarchy(
        &self,
        _: &SessionScope,
        query: &TypeHierarchyQuery,
    ) -> Result<TypeHierarchy, ToolError> {
        let language = self.language_for(&query.path)?;
        let (spot, _) = self.spot_of(language.as_ref(), &query.path, query.line, query.column)?;
        let provider = language
            .hierarchy()
            .ok_or_else(|| missing("type hierarchy"))?;
        let read = |direction: HierarchyDirection| {
            let query = HierarchyQuery {
                spot: spot.clone(),
                direction,
            };
            let outcome = first_answer(lang_api::ask(provider, query), self.wait());
            value_of(outcome, "type hierarchy")
        };
        let (supertypes, confidence, source) = read(HierarchyDirection::Supertypes)?;
        let (subtypes, _, _) = read(HierarchyDirection::Subtypes)?;
        let mut clipped = false;
        let supertypes = self.hierarchy_nodes(supertypes, &mut clipped);
        let subtypes = self.hierarchy_nodes(subtypes, &mut clipped);
        Ok(TypeHierarchy {
            supertypes,
            subtypes,
            source,
            confidence,
            clipped,
        })
    }

    fn call_hierarchy(
        &self,
        _: &SessionScope,
        query: &CallHierarchyQuery,
    ) -> Result<CallHierarchy, ToolError> {
        let language = self.language_for(&query.path)?;
        let (spot, _) = self.spot_of(language.as_ref(), &query.path, query.line, query.column)?;
        let provider = language
            .call_hierarchy()
            .ok_or_else(|| missing("call hierarchy"))?;
        let outcome = first_answer(
            lang_api::ask(provider, LangCallHierarchy::Roots(spot)),
            self.wait(),
        );
        let (roots, confidence, source) = value_of(outcome, "call hierarchy")?;
        let mut callers = Vec::new();
        let mut clipped = false;
        for root in roots {
            self.collect_callers(provider, root, query.depth, 0, &mut callers, &mut clipped)?;
        }
        Ok(CallHierarchy {
            callers,
            source,
            confidence,
            clipped,
        })
    }

    fn read_file(&self, _: &SessionScope, query: &ReadFileQuery) -> Result<FileText, ToolError> {
        let text = std::fs::read_to_string(&query.path).map_err(|error| {
            let shown = query.path.display();
            ToolError::Failed(format!("could not read {shown}: {error}"))
        })?;
        Ok(self.range_of(&query.path, &text, query))
    }

    fn file_text(&self, _: &SessionScope, path: &Path) -> Result<FileContent, ToolError> {
        let text = std::fs::read_to_string(path).map_err(|error| {
            ToolError::Failed(format!("could not read {}: {error}", path.display()))
        })?;
        Ok(FileContent {
            path: path.to_path_buf(),
            text,
            dirty: false,
        })
    }

    fn search_symbols(
        &self,
        _: &SessionScope,
        query: &SymbolQuery,
    ) -> Result<SymbolPage, ToolError> {
        let language = self.language_with_symbols()?;
        let provider = language
            .workspace_symbols()
            .ok_or_else(|| missing("symbol search"))?;
        let outcome = first_answer(
            lang_api::ask(provider, LangSymbolQuery(query.query.clone())),
            self.wait(),
        );
        let (entries, confidence, source) = value_of(outcome, "symbol search")?;
        let mut hits = Vec::new();
        for entry in entries {
            let Some(place) = entry.place else {
                continue;
            };
            let location = self.location_of(&place);
            hits.push(crate::backend::SymbolHit {
                name: entry.name,
                kind: entry.container.unwrap_or_else(|| "symbol".to_string()),
                path: location.path,
                line: location.line,
                content: location.content,
                before: Vec::new(),
                after: Vec::new(),
            });
        }
        let total = hits.len();
        let limit = query.limit.max(1);
        let clipped = total > limit;
        hits.truncate(limit);
        Ok(SymbolPage {
            hits,
            source,
            confidence,
            next: clipped.then(|| limit.to_string()),
        })
    }

    fn analyze_calls(
        &self,
        _: &SessionScope,
        query: &AnalyzeCallsQuery,
    ) -> Result<CallTree, ToolError> {
        let language = self.language_with_symbols()?;
        let spot = self.declaration_spot_for(language.as_ref(), &query.symbol_fqn)?;
        let provider = language
            .call_hierarchy()
            .ok_or_else(|| missing("call hierarchy"))?;
        let outcome = first_answer(
            lang_api::ask(provider, LangCallHierarchy::Roots(spot)),
            self.wait(),
        );
        let (roots, _, _) = value_of(outcome, "call hierarchy")?;
        let direction = match query.analysis_kind {
            crate::backend::CallDirection::IncomingCalls => lang_api::CallDirection::Incoming,
            crate::backend::CallDirection::OutgoingCalls => lang_api::CallDirection::Outgoing,
        };
        let mut more = false;
        let mut budget = query.max_nodes;
        let root_items = roots
            .into_iter()
            .skip(query.child_offset)
            .take(query.max_children);
        let mut nodes = Vec::new();
        for (index, item) in root_items.enumerate() {
            if budget == 0 {
                more = true;
                break;
            }
            nodes.push(self.call_node(
                provider,
                item,
                direction,
                query.depth,
                query.max_children,
                &mut budget,
                &mut more,
                vec![index],
            )?);
        }
        let root = take_tree_path(nodes, &query.tree_path);
        Ok(CallTree {
            symbol: query.symbol_fqn.clone(),
            direction: query.analysis_kind,
            root,
            more,
        })
    }

    fn run_points(
        &self,
        _: &SessionScope,
        query: &RunPointQuery,
    ) -> Result<Vec<RunPointInfo>, ToolError> {
        let language = self.language_for(&query.file_path)?;
        let provider = language.run_points().ok_or_else(|| missing("run points"))?;
        let outcome = first_answer(
            lang_api::ask(provider, lang_api::RunPointQuery(query.file_path.clone())),
            self.wait(),
        );
        let (points, _, _) = value_of(outcome, "run points")?;
        Ok(points
            .into_iter()
            .map(|point| RunPointInfo {
                kind: point.kind.name().to_string(),
                name: point.name,
                container: point.container,
                file: point.file,
                line: point.line,
                framework: point.framework,
            })
            .collect())
    }

    fn source_text(&self, _: &SessionScope, uri: &str) -> Result<String, ToolError> {
        let languages = self
            .languages
            .read()
            .unwrap_or_else(|error| error.into_inner());
        let provider = languages
            .iter()
            .find_map(|language| language.source_text())
            .ok_or_else(|| {
                ToolError::Failed("no decompiler is connected for this file".to_string())
            })?;
        let outcome = first_answer(
            lang_api::ask(provider, lang_api::SourceTextQuery(uri.to_string())),
            self.wait(),
        );
        let (text, _, _) = value_of(outcome, "source text")?;
        if text.is_empty() {
            return Err(ToolError::Failed(format!(
                "the language service has no text for {uri}"
            )));
        }
        Ok(text)
    }

    fn build_project(
        &self,
        scope: &SessionScope,
        request: &BuildRequest,
    ) -> Result<BuildOutcome, ToolError> {
        self.project_for(&scope.project)?.build(scope, request)
    }

    fn run_tests(
        &self,
        scope: &SessionScope,
        request: &TestsRequest,
    ) -> Result<TestsOutcome, ToolError> {
        self.project_for(&scope.project)?.test(scope, request)
    }

    fn run_configurations(&self, scope: &SessionScope) -> Result<Vec<RunConfiguration>, ToolError> {
        self.project_for(&scope.project)?.configurations(scope)
    }

    fn start_run(
        &self,
        scope: &SessionScope,
        request: &StartRunRequest,
        client: Option<&str>,
    ) -> Result<String, ToolError> {
        self.project_for(&scope.project)?
            .start(scope, request, client)
    }

    fn run_output(&self, scope: &SessionScope, id: &str) -> Result<RunOutput, ToolError> {
        self.project_for(&scope.project)?.output(scope, id)
    }

    fn stop_run(&self, scope: &SessionScope, id: &str) -> Result<(), ToolError> {
        self.project_for(&scope.project)?.stop(scope, id)
    }

    fn set_breakpoint(
        &self,
        scope: &SessionScope,
        query: &BreakpointQuery,
    ) -> Result<BreakpointInfo, ToolError> {
        self.debug_control_for(&scope.project)?
            .set_breakpoint(scope, query)
    }

    fn remove_breakpoint(&self, scope: &SessionScope, id: &BreakpointId) -> Result<(), ToolError> {
        self.debug_control_for(&scope.project)?
            .remove_breakpoint(scope, id)
    }

    fn breakpoints(&self, scope: &SessionScope) -> Result<Vec<BreakpointInfo>, ToolError> {
        self.debug_control_for(&scope.project)?.breakpoints(scope)
    }

    fn debug_run(
        &self,
        scope: &SessionScope,
        request: &DebugRunRequest,
        client: Option<&str>,
    ) -> Result<DebugRunOutcome, ToolError> {
        self.debug_control_for(&scope.project)?
            .run(scope, request, client)
    }

    fn debug_wait(
        &self,
        scope: &SessionScope,
        query: &DebugWaitQuery,
    ) -> Result<DebugSessionState, ToolError> {
        self.debug_control_for(&scope.project)?.wait(scope, query)
    }

    fn debug_stack(
        &self,
        scope: &SessionScope,
        query: &DebugStackQuery,
    ) -> Result<DebugStack, ToolError> {
        self.debug_control_for(&scope.project)?.stack(scope, query)
    }

    fn debug_variables(
        &self,
        scope: &SessionScope,
        query: &DebugVariablesQuery,
    ) -> Result<DebugVariables, ToolError> {
        self.debug_control_for(&scope.project)?
            .variables(scope, query)
    }

    fn debug_evaluate(
        &self,
        scope: &SessionScope,
        query: &DebugEvaluateQuery,
    ) -> Result<DebugValue, ToolError> {
        self.debug_control_for(&scope.project)?
            .evaluate(scope, query)
    }

    fn debug_step(
        &self,
        scope: &SessionScope,
        query: &DebugStepQuery,
    ) -> Result<DebugSessionState, ToolError> {
        self.debug_control_for(&scope.project)?.step(scope, query)
    }

    fn debug_stop(&self, scope: &SessionScope) -> Result<(), ToolError> {
        self.debug_control_for(&scope.project)?.stop(scope)
    }

    fn debug_sessions(
        &self,
        scope: &SessionScope,
        query: &DebugQuery,
    ) -> Result<DebugAnswer, ToolError> {
        self.debug_control_for(&scope.project)?.call(scope, query)
    }

    fn database(
        &self,
        scope: &SessionScope,
        query: &DatabaseQuery,
    ) -> Result<DatabaseAnswer, ToolError> {
        self.database_control_for(&scope.project)?
            .call(scope, query)
    }
}

impl LanguageTools {
    /// The place of a declaration by its fully qualified name or its simple name.
    fn declaration_spot_for(
        &self,
        language: &dyn LanguageSupport,
        fqn: &str,
    ) -> Result<DocumentSpot, ToolError> {
        let provider = language
            .workspace_symbols()
            .ok_or_else(|| missing("symbol search"))?;
        let outcome = first_answer(
            lang_api::ask(provider, LangSymbolQuery(fqn.to_string())),
            self.wait(),
        );
        let (entries, _, _) = value_of(outcome, "symbol search")?;
        let simple = fqn.rsplit(['.', '#', ':']).next().unwrap_or(fqn);
        let found = entries
            .into_iter()
            .find(|entry| {
                entry.name == fqn
                    || entry.container.as_deref().is_some_and(|container| {
                        format!("{container}.{}", entry.name) == fqn
                            || format!("{container}#{}", entry.name) == fqn
                    })
                    || (entry.name == simple && entry.place.is_some())
            })
            .and_then(|entry| entry.place);
        let Some(place) = found else {
            return Err(ToolError::Failed(format!(
                "no declaration named {fqn} was found"
            )));
        };
        let server = language
            .server()
            .ok_or_else(|| missing("language server"))?;
        let version = server.version(&place.uri).ok_or_else(|| {
            ToolError::Failed(format!(
                "the declaration of {fqn} is at {}, which is not open in the editor",
                place.uri
            ))
        })?;
        Ok(DocumentSpot {
            uri: place.uri,
            version,
            point: place.span.start,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn call_node(
        &self,
        provider: &dyn lang_api::Provider<LangCallHierarchy, Vec<CallHierarchyItem>>,
        item: CallHierarchyItem,
        direction: lang_api::CallDirection,
        remaining: usize,
        max_children: usize,
        budget: &mut usize,
        more: &mut bool,
        tree_path: Vec<usize>,
    ) -> Result<CallNode, ToolError> {
        *budget = budget.saturating_sub(1);
        let place = item.place.as_ref().map(|place| self.location_of(place));
        let mut node = CallNode {
            name: item.name.clone(),
            detail: item.detail.clone(),
            file_path: place.as_ref().map(|place| place.path.clone()),
            line: place.as_ref().map_or(0, |place| place.line),
            tree_path: tree_path.clone(),
            children: Vec::new(),
        };
        if remaining == 0 || *budget == 0 {
            if *budget == 0 {
                *more = true;
            }
            return Ok(node);
        }
        let outcome = first_answer(
            lang_api::ask(provider, LangCallHierarchy::Calls { item, direction }),
            self.wait(),
        );
        let (children, _, _) = value_of(outcome, "call hierarchy")?;
        if children.len() > max_children {
            *more = true;
        }
        for (index, child) in children.into_iter().take(max_children).enumerate() {
            if *budget == 0 {
                *more = true;
                break;
            }
            let mut child_path = tree_path.clone();
            child_path.push(index);
            node.children.push(self.call_node(
                provider,
                child,
                direction,
                remaining - 1,
                max_children,
                budget,
                more,
                child_path,
            )?);
        }
        Ok(node)
    }

    fn hierarchy_nodes(
        &self,
        entries: Vec<lang_api::SymbolEntry>,
        clipped: &mut bool,
    ) -> Vec<HierarchyNode> {
        let mut nodes: Vec<HierarchyNode> = entries
            .into_iter()
            .map(|entry| HierarchyNode {
                name: entry.name,
                detail: entry.container,
                location: entry.place.as_ref().map(|place| self.location_of(place)),
                depth: 1,
            })
            .collect();
        if nodes.len() > MAX_NODES {
            *clipped = true;
            nodes.truncate(MAX_NODES);
        }
        nodes
    }

    /// The callers of one item, then the callers of those callers, down to `remaining`
    /// levels. A short answer stops at [`MAX_NODES`] and says so.
    fn collect_callers(
        &self,
        provider: &dyn lang_api::Provider<LangCallHierarchy, Vec<CallHierarchyItem>>,
        item: CallHierarchyItem,
        remaining: usize,
        level: u32,
        out: &mut Vec<HierarchyNode>,
        clipped: &mut bool,
    ) -> Result<(), ToolError> {
        if remaining == 0 {
            return Ok(());
        }
        if out.len() >= MAX_NODES {
            *clipped = true;
            return Ok(());
        }
        let query = LangCallHierarchy::Calls {
            item,
            direction: CallDirection::Incoming,
        };
        let outcome = first_answer(lang_api::ask(provider, query), self.wait());
        let (items, _, _) = value_of(outcome, "call hierarchy")?;
        for child in items {
            if out.len() >= MAX_NODES {
                *clipped = true;
                return Ok(());
            }
            out.push(HierarchyNode {
                name: child.name.clone(),
                detail: child.detail.clone(),
                location: child.place.as_ref().map(|place| self.location_of(place)),
                depth: level + 1,
            });
            self.collect_callers(provider, child, remaining - 1, level + 1, out, clipped)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{column_of, signature_of, utf16_of, word_at};

    #[test]
    fn columns_and_utf16_units_convert_both_ways() {
        let line = "a😀b = 1";
        assert_eq!(utf16_of(line, 3), 3);
        assert_eq!(column_of(line, 3), 3);
        assert_eq!(utf16_of(line, 1), 0);
        assert_eq!(column_of(line, 0), 1);
    }

    #[test]
    fn a_file_uri_gives_an_absolute_path() {
        let uri = if cfg!(windows) {
            "file:///C:/work/a%20b/Foo.src"
        } else {
            "file:///tmp/a%20b/Foo.src"
        };
        let path = platform::path_from_uri(uri).expect("a file uri");
        assert!(path.is_absolute(), "{}", path.display());
        assert_eq!(
            path.parent().and_then(|parent| parent.file_name()),
            Some(std::ffi::OsStr::new("a b"))
        );
        assert_eq!(
            platform::path_from_uri("lib://contents/core/Text.class"),
            None
        );
    }

    #[test]
    fn the_word_and_the_signature_come_from_the_text() {
        assert_eq!(word_at("  foo.barBaz(1);", 9), Some("barBaz".to_string()));
        assert_eq!(word_at("a + b", 2), None);
        let hover = "```lang\nvoid run()\n```\nRuns it.";
        assert_eq!(signature_of(hover), Some("void run()".to_string()));
        assert_eq!(signature_of("plain"), None);
    }
}
