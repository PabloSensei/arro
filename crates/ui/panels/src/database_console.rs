//! The language of the SQL console: the completion list and the names the loaded schema
//! cannot answer.
//!
//! The editor knows no SQL and the window knows no grammar: this module asks
//! [`database`] for the suggestions of the caret and turns its answers into the entries
//! and diagnostics of the editor. A console file belongs to a source, and the schema of a
//! source is kept here as the tree of the Database window reads it: a keystroke never asks
//! the database.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ::base::Cancel;
use editor_view::{FileLanguages, uri_path};
use lang_api::{
    Answer, CompletionEntry, CompletionKind, CompletionList, CompletionProvider, Confidence,
    DiagnosticEntry, DiagnosticsProvider, DiagnosticsQuery, DocumentEdit, DocumentSpot, LanguageFn,
    LanguageId, LanguageSupport, Provider, Queries, Reply, SemanticServer, Severity,
    SourceId as AnswerSource, SyntaxProvider, TextPoint, TextSpan, offset_in,
};
use ui_kit::{App, Global};

use database::{
    Dialect, SourceId, SqlProblem, SuggestionKind, Table, complete, problems, source_of_console,
};

/// How long the answers of one edit wait for the next one, so typing a name publishes its
/// checks once.
const DEBOUNCE: Duration = Duration::from_millis(120);

/// The name this language answers under.
const NAME: &str = "sql-console";

/// The tables, the dialect and the default schema of one source.
#[derive(Clone, Default)]
struct SourceSchema {
    tables: Vec<Table>,
    dialect: Dialect,
    default_schema: Option<String>,
}

/// The state the editor and the window share. The window writes the schema, the provider
/// threads read it, and the diagnostics of every open console follow both.
struct Shared {
    state_dir: PathBuf,
    documents: Mutex<HashMap<String, Arc<str>>>,
    sources: Mutex<HashMap<SourceId, SourceSchema>>,
    /// One serial per document: an answer of an older text is not published.
    serials: Mutex<HashMap<String, u64>>,
    next_serial: AtomicU64,
    /// The open diagnostics streams of each document, in the language of the editor.
    listeners: Mutex<HashMap<String, Vec<Reply<Vec<DiagnosticEntry>>>>>,
}

impl Shared {
    fn text(&self, uri: &str) -> Option<Arc<str>> {
        self.documents
            .lock()
            .ok()
            .and_then(|documents| documents.get(uri).cloned())
    }

    fn serial(&self, uri: &str) -> u64 {
        self.serials
            .lock()
            .ok()
            .and_then(|serials| serials.get(uri).copied())
            .unwrap_or(0)
    }

    /// Raises the serial of a document and hands the new text to the worker.
    fn changed(&self, uri: &str, text: Arc<str>) -> u64 {
        if let Ok(mut documents) = self.documents.lock() {
            documents.insert(uri.to_owned(), text);
        }
        let serial = self.next_serial.fetch_add(1, Ordering::Relaxed) + 1;
        if let Ok(mut serials) = self.serials.lock() {
            serials.insert(uri.to_owned(), serial);
        }
        serial
    }

    /// The source of a console document, as the state folder names it.
    fn source_of(&self, uri: &str) -> Option<SourceId> {
        let path = uri_path(uri)?;
        source_of_console(&self.state_dir, &path)
    }

    fn schema_of(&self, source: &SourceId) -> SourceSchema {
        self.sources
            .lock()
            .ok()
            .and_then(|sources| sources.get(source).cloned())
            .unwrap_or_default()
    }

    /// The diagnostics of `uri` as the loaded schema answers them now.
    fn entries(&self, uri: &str) -> Vec<DiagnosticEntry> {
        let Some(text) = self.text(uri) else {
            return Vec::new();
        };
        let Some(source) = self.source_of(uri) else {
            return Vec::new();
        };
        let schema = self.schema_of(&source);
        entries_of(
            &text,
            problems(
                &text,
                &schema.tables,
                schema.dialect,
                schema.default_schema.as_deref(),
            ),
        )
    }

    /// Sends the checks of `uri` to every open diagnostics stream of it.
    fn publish(&self, uri: &str) {
        let entries = self.entries(uri);
        let Ok(mut listeners) = self.listeners.lock() else {
            return;
        };
        let Some(list) = listeners.get_mut(uri) else {
            return;
        };
        list.retain_mut(|reply| {
            !reply.is_cancelled()
                && reply
                    .send(Answer::new(
                        entries.clone(),
                        Confidence::Certain,
                        AnswerSource::new(NAME),
                    ))
                    .is_ok()
        });
        if list.is_empty() {
            listeners.remove(uri);
        }
    }

    /// The serial of the text a job was made for, so a queued answer of an older text is
    /// dropped instead of covering the newer one.
    fn current(&self, uri: &str, serial: u64) -> bool {
        self.serial(uri) == serial
    }
}

/// One queued diagnostics pass.
struct Job {
    uri: String,
    /// The text the job belongs to, so a queued answer of an older text is dropped
    /// instead of covering the newer one.
    serial: u64,
}

/// The language of the `.sql` console files of the Database window.
pub struct SqlLanguage {
    shared: Arc<Shared>,
    jobs: Mutex<Option<Sender<Job>>>,
    syntax: NoSyntax,
}

impl SqlLanguage {
    /// The language of the consoles that live under `state_dir`, with the worker that
    /// publishes their checks away from the thread of the editor.
    pub fn new(state_dir: PathBuf) -> Self {
        let shared = Arc::new(Shared {
            state_dir,
            documents: Mutex::new(HashMap::new()),
            sources: Mutex::new(HashMap::new()),
            serials: Mutex::new(HashMap::new()),
            next_serial: AtomicU64::new(0),
            listeners: Mutex::new(HashMap::new()),
        });
        let (sender, receiver) = channel();
        let worker_shared = shared.clone();
        if std::thread::Builder::new()
            .name("database-console-checks".into())
            .spawn(move || work(receiver, worker_shared))
            .is_ok()
        {
            return Self {
                shared,
                jobs: Mutex::new(Some(sender)),
                syntax: NoSyntax(Queries::default()),
            };
        }
        Self {
            shared,
            jobs: Mutex::new(None),
            syntax: NoSyntax(Queries::default()),
        }
    }

    /// The schema of one source, as the tree of the Database window read it.
    pub fn set_source(
        &self,
        source: SourceId,
        tables: Vec<Table>,
        dialect: Dialect,
        default_schema: Option<String>,
    ) {
        if let Ok(mut sources) = self.shared.sources.lock() {
            sources.insert(
                source,
                SourceSchema {
                    tables,
                    dialect,
                    default_schema,
                },
            );
        }
        self.schedule_all();
    }

    /// Forgets the schema of a source: its console completes and checks nothing until a
    /// connection reads the tree again.
    pub fn clear_source(&self, source: &SourceId) {
        if let Ok(mut sources) = self.shared.sources.lock() {
            sources.remove(source);
        }
        self.schedule_all();
    }

    fn schedule(&self, job: Job) {
        if let Ok(jobs) = self.jobs.lock()
            && let Some(sender) = jobs.as_ref()
        {
            let _ = sender.send(job);
        }
    }

    /// Queues every open console: a schema that arrived answers the checks of all of them.
    fn schedule_all(&self) {
        let uris: Vec<String> = self
            .shared
            .documents
            .lock()
            .map(|documents| documents.keys().cloned().collect())
            .unwrap_or_default();
        for uri in uris {
            let serial = self.shared.serial(&uri);
            self.schedule(Job { uri, serial });
        }
    }

    /// The state folder whose consoles this language answers for.
    pub(crate) fn state_dir(&self) -> &Path {
        &self.shared.state_dir
    }
}

/// The SQL languages of the window, one per state folder: the panel of a source and the
/// editor of its console have to meet at the same one.
#[derive(Default)]
pub(crate) struct SqlLanguages(pub(crate) Vec<Arc<SqlLanguage>>);

impl Global for SqlLanguages {}

/// The language of the consoles that live under `state_dir`, when the window has one.
pub(crate) fn language_for(state_dir: &Path, cx: &App) -> Option<Arc<SqlLanguage>> {
    cx.try_global::<SqlLanguages>()?
        .0
        .iter()
        .find(|language| language.state_dir() == state_dir)
        .cloned()
}

/// Puts the language of the `.sql` consoles in place, so the editor asks it for the names
/// of a statement and for the names the loaded schema cannot answer. A folder that is
/// already in place keeps its language: every window of the project shares it.
pub(crate) fn install(state_dir: PathBuf, cx: &mut App) {
    if language_for(&state_dir, cx).is_some() {
        return;
    }
    let language = Arc::new(SqlLanguage::new(state_dir));
    let mut languages = cx
        .try_global::<FileLanguages>()
        .map(|languages| languages.0.clone())
        .unwrap_or_default();
    let support: Arc<dyn LanguageSupport> = language.clone();
    languages.push(support);
    cx.set_global(FileLanguages(languages));
    let mut known = cx
        .try_global::<SqlLanguages>()
        .map(|known| known.0.clone())
        .unwrap_or_default();
    known.push(language);
    cx.set_global(SqlLanguages(known));
}

fn work(receiver: Receiver<Job>, shared: Arc<Shared>) {
    let mut ready: HashMap<String, u64> = HashMap::new();
    loop {
        match receiver.recv() {
            Ok(job) => {
                ready.insert(job.uri, job.serial);
            }
            Err(_) => return,
        }
        // One pass for the burst of edits: the last text of each console wins.
        loop {
            match receiver.recv_timeout(DEBOUNCE) {
                Ok(job) => {
                    ready.insert(job.uri, job.serial);
                }
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        for (uri, serial) in ready.drain() {
            if shared.current(&uri, serial) {
                shared.publish(&uri);
            }
        }
    }
}

impl SemanticServer for SqlLanguage {
    fn open(&self, uri: &str, _: i32, text: Arc<str>) {
        self.shared.changed(uri, text);
    }

    fn change(&self, uri: &str, _: i32, text: Arc<str>, _: Vec<DocumentEdit>) {
        let serial = self.shared.changed(uri, text);
        self.schedule(Job {
            uri: uri.to_owned(),
            serial,
        });
    }

    fn save(&self, _: &str) {}

    fn close(&self, uri: &str) {
        if let Ok(mut documents) = self.shared.documents.lock() {
            documents.remove(uri);
        }
        if let Ok(mut serials) = self.shared.serials.lock() {
            serials.remove(uri);
        }
        if let Ok(mut listeners) = self.shared.listeners.lock() {
            // Dropping the replies ends the streams of the closed editor.
            listeners.remove(uri);
        }
    }

    fn version(&self, uri: &str) -> Option<i32> {
        self.shared.text(uri).map(|_| 1)
    }
}

impl Provider<DocumentSpot, CompletionList> for SqlLanguage {
    fn start(&self, query: DocumentSpot, cancel: Cancel, mut reply: Reply<CompletionList>) {
        let Some(text) = self.shared.text(&query.uri) else {
            let _ = reply.send(Answer::new(
                CompletionList {
                    entries: Vec::new(),
                    complete: true,
                },
                Confidence::Certain,
                AnswerSource::new(NAME),
            ));
            return;
        };
        let schema = self
            .shared
            .source_of(&query.uri)
            .map(|source| self.shared.schema_of(&source))
            .unwrap_or_default();
        let point = query.point;
        let spawned = std::thread::Builder::new()
            .name("database-sql-complete".into())
            .spawn(move || {
                if cancel.is_cancelled() {
                    return;
                }
                let caret = offset_in(&text, point);
                let suggestions = complete(
                    &text,
                    caret,
                    &schema.tables,
                    schema.dialect,
                    schema.default_schema.as_deref(),
                );
                if cancel.is_cancelled() {
                    return;
                }
                let entries = suggestions.into_iter().map(entry_of).collect();
                let _ = reply.send(Answer::new(
                    CompletionList {
                        entries,
                        complete: true,
                    },
                    Confidence::Certain,
                    AnswerSource::new(NAME),
                ));
            });
        let _ = spawned;
    }
}

impl Provider<DiagnosticsQuery, Vec<DiagnosticEntry>> for SqlLanguage {
    fn start(
        &self,
        query: DiagnosticsQuery,
        cancel: Cancel,
        mut reply: Reply<Vec<DiagnosticEntry>>,
    ) {
        let entries = self.shared.entries(&query.0);
        let _ = reply.send(Answer::new(
            entries,
            Confidence::Certain,
            AnswerSource::new(NAME),
        ));
        if cancel.is_cancelled() {
            return;
        }
        if let Ok(mut listeners) = self.shared.listeners.lock() {
            let list = listeners.entry(query.0).or_default();
            list.retain(|open| !open.is_cancelled());
            list.push(reply);
        }
    }
}

impl LanguageSupport for SqlLanguage {
    fn id(&self) -> LanguageId {
        LanguageId(NAME)
    }

    fn matches(&self, path: &Path) -> bool {
        source_of_console(&self.shared.state_dir, path).is_some()
    }

    fn syntax(&self) -> &dyn SyntaxProvider {
        &self.syntax
    }

    fn server(&self) -> Option<&dyn SemanticServer> {
        Some(self)
    }

    fn completion(&self) -> Option<&CompletionProvider> {
        Some(self)
    }

    fn diagnostics(&self) -> Option<&DiagnosticsProvider> {
        Some(self)
    }
}

/// One suggestion of the parser as the row of the completion list.
fn entry_of(suggestion: database::SqlSuggestion) -> CompletionEntry {
    CompletionEntry {
        label: suggestion.label.clone(),
        insert_text: Some(suggestion.label),
        detail: Some(suggestion.detail),
        kind: match suggestion.kind {
            SuggestionKind::Table => CompletionKind::Struct,
            SuggestionKind::View => CompletionKind::Interface,
            SuggestionKind::Column => CompletionKind::Field,
            SuggestionKind::Function => CompletionKind::Function,
            SuggestionKind::Keyword => CompletionKind::Keyword,
        },
        ..CompletionEntry::default()
    }
}

/// The problems of the parser as the marks of the editor.
fn entries_of(text: &str, problems: Vec<SqlProblem>) -> Vec<DiagnosticEntry> {
    problems
        .into_iter()
        .map(|problem| DiagnosticEntry {
            span: TextSpan {
                start: point_at(text, problem.start),
                end: point_at(text, problem.end),
            },
            severity: Severity::Error,
            message: problem.message,
            unnecessary: false,
            code: None,
        })
        .collect()
}

/// The place of a byte offset, counted as the editor counts it.
fn point_at(text: &str, offset: usize) -> TextPoint {
    let offset = offset.min(text.len());
    let mut line = 0;
    let mut line_start = 0;
    for (index, character) in text.char_indices() {
        if index >= offset {
            break;
        }
        if character == '\n' {
            line += 1;
            line_start = index + 1;
        }
    }
    TextPoint {
        line,
        character: text[line_start..offset].encode_utf16().count() as u32,
    }
}

unsafe extern "C" fn no_grammar() -> *const () {
    std::ptr::null()
}

struct NoSyntax(Queries);

impl SyntaxProvider for NoSyntax {
    fn grammar(&self) -> LanguageFn {
        unsafe { LanguageFn::from_raw(no_grammar) }
    }

    fn queries(&self) -> &Queries {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_point_counts_the_units_the_editor_counts() {
        let text = "SELECT 1\nFROM »x«";
        let offset = text.find('»').unwrap();
        assert_eq!(
            point_at(text, offset),
            TextPoint {
                line: 1,
                character: 5
            }
        );
        // A character outside the basic plane is two units of the editor.
        let text = "SELECT 😀 FROM";
        let offset = text.find(" FROM").unwrap();
        assert_eq!(point_at(text, offset).character, 9);
    }
}
