//! Tools answered by the language layer through `lang-api`. Languages register when their server
//! is up; a file of no registered language gets a plain error, never an invented answer.

use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, RwLock};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use futures_core::Stream;
use lang_api::{
    Answer, DiagnosticsQuery as LangDiagnostics, DocumentSpot, LanguageSupport, NavigationKind,
    NavigationQuery, Replies, TextPoint,
};
use platform::CanonPath;

use crate::backend::{
    Confidence, DebugSnapshotSource, Diagnostic, DiagnosticsPage, DiagnosticsQuery, EditorContext,
    Location, Position, SessionScope, Severity, SymbolInfo, SymbolPage, SymbolQuery, ToolBackend,
    ToolError,
};

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

/// A `file:` URI back to a path, `None` for sources that are not files, `jdt:` ones for example.
fn path_of_uri(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file:///")?;
    let mut bytes = Vec::with_capacity(rest.len());
    let mut input = rest.bytes();
    while let Some(byte) = input.next() {
        if byte != b'%' {
            bytes.push(byte);
            continue;
        }
        let high = (input.next()? as char).to_digit(16)?;
        let low = (input.next()? as char).to_digit(16)?;
        bytes.push((high * 16 + low) as u8);
    }
    Some(PathBuf::from(String::from_utf8(bytes).ok()?))
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

/// The languages whose servers are up, looked up by the file a tool asks about.
#[derive(Default)]
pub struct LanguageTools {
    languages: RwLock<Vec<Arc<dyn LanguageSupport>>>,
    debug: RwLock<Vec<(PathBuf, Arc<dyn DebugSnapshotSource>)>>,
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
        let line = span.span.start.line;
        Some(match path_of_uri(&span.uri) {
            Some(path) => Location {
                content: line_text(&path, line).unwrap_or_default(),
                path,
                line: line + 1,
            },
            None => Location {
                path: PathBuf::from(span.uri),
                line: line + 1,
                content: String::new(),
            },
        })
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
        let uri = uri_of(&position.path)?;
        let version = Self::open_version(language.as_ref(), &uri, &position.path)?;
        let line = line_text(&position.path, position.line - 1).unwrap_or_default();
        let spot = DocumentSpot {
            uri,
            version,
            point: TextPoint {
                line: position.line - 1,
                character: utf16_of(&line, position.column),
            },
        };
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
}

#[cfg(test)]
mod tests {
    use super::{column_of, path_of_uri, signature_of, utf16_of, word_at};
    use std::path::PathBuf;

    #[test]
    fn columns_and_utf16_units_convert_both_ways() {
        let line = "a😀b = 1";
        assert_eq!(utf16_of(line, 3), 3);
        assert_eq!(column_of(line, 3), 3);
        assert_eq!(utf16_of(line, 1), 0);
        assert_eq!(column_of(line, 0), 1);
    }

    #[test]
    fn file_uris_turn_back_into_paths() {
        assert_eq!(
            path_of_uri("file:///C:/work/a%20b/Foo.src"),
            Some(PathBuf::from("C:/work/a b/Foo.src"))
        );
        assert_eq!(path_of_uri("lib://contents/core/Text.class"), None);
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
