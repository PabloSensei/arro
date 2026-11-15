//! The versions of the dependencies in `pom.xml`, `build.gradle`, `build.gradle.kts` and
//! `libs.versions.toml`: a weak warning on a version a repository has a newer one of, the
//! fixes of that version, and the list of versions while one is typed.
//!
//! The lists come from the repositories of the project ([`java_project::updates`]) on a
//! thread of this support, and only the cache is read on the thread that asks. A hint
//! appears when a repository answers, so the interface never waits; a project without a
//! network or with the check switched off simply shows nothing.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, Weak};
use std::thread;
use std::time::Duration;

use base::Cancel;
use java_project::updates::{self, BuildFile, Slot, VersionSource};
use lang_api::{
    ActionEntry, ActionProvider, ActionQuery, Answer, CompletionEntry, CompletionKind,
    CompletionList, CompletionProvider, Confidence, DependencyUpdatesProvider, DependencyVersion,
    DependencyVersionQuery, DiagnosticEntry, DiagnosticsProvider, DiagnosticsQuery, DocumentEdit,
    DocumentSpot, LanguageId, LanguageSupport, Provider, Queries, Reply, SemanticServer, Severity,
    SourceEdit, SourceId, SyntaxProvider, TextSpan, offset_in,
};
use lsp_client::lsp_types::Uri;
use lsp_client::uri::file_path;
use tree_sitter_language::LanguageFn;

pub const LANGUAGE_ID: LanguageId = LanguageId("build-script");

const SOURCE: SourceId = SourceId::new("build-scripts");
/// The pause before the checks of a file that just changed run again.
const CHECK_DELAY: Duration = Duration::from_millis(120);

unsafe extern "C" fn no_grammar() -> *const () {
    std::ptr::null()
}

const NO_QUERIES: Queries = Queries {
    highlights: "",
    folds: "",
    indents: "",
    locals: "",
};

/// The support of the build scripts of one project. Cloning it shares the documents, the
/// cache and the thread that reads the repositories.
#[derive(Clone)]
pub struct BuildScriptSupport {
    inner: Arc<Inner>,
}

struct Inner {
    root: PathBuf,
    source: VersionSource,
    offline: AtomicBool,
    enabled: AtomicBool,
    documents: Mutex<HashMap<String, Arc<str>>>,
    replies: Mutex<HashMap<String, Reply<Vec<DiagnosticEntry>>>>,
    /// The coordinates a repository was asked for already: a failed question stays
    /// unanswered until the window opens the project again.
    asked: Mutex<HashSet<String>>,
    queue: Mutex<Option<Sender<String>>>,
    /// The checks the user turned off: one that is off is not computed at all.
    inspection_checks: Mutex<lang_api::DisabledInspections>,
}

impl BuildScriptSupport {
    pub fn new(root: PathBuf, cache: PathBuf, offline: bool) -> Self {
        Self {
            inner: Arc::new(Inner {
                root,
                source: VersionSource::new(cache),
                offline: AtomicBool::new(offline),
                enabled: AtomicBool::new(true),
                documents: Mutex::new(HashMap::new()),
                replies: Mutex::new(HashMap::new()),
                asked: Mutex::new(HashSet::new()),
                queue: Mutex::new(None),
                inspection_checks: Mutex::new(lang_api::DisabledInspections::default()),
            }),
        }
    }

    /// The checks the user turned off, shared with the rest of the language.
    pub fn set_inspection_checks(&self, checks: lang_api::DisabledInspections) {
        *self
            .inner
            .inspection_checks
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = checks;
    }

    fn check_enabled(&self, id: &str) -> bool {
        self.inner
            .inspection_checks
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .enabled(id)
    }

    /// Whether the window asks for newer versions at all.
    pub fn set_enabled(&self, enabled: bool) {
        self.inner.enabled.store(enabled, Ordering::SeqCst);
        self.refresh_all();
    }

    fn enabled(&self) -> bool {
        self.inner.enabled.load(Ordering::SeqCst) && !self.inner.offline.load(Ordering::SeqCst)
    }

    fn document(&self, uri: &str) -> Option<Arc<str>> {
        self.inner
            .documents
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .get(uri)
            .cloned()
    }

    fn file_of(&self, uri: &str) -> Option<(BuildFile, String)> {
        let path = file_path(&Uri::from_str(uri).ok()?)?;
        let file = updates::kind_of(&path)?;
        let text = self.document(uri)?;
        Some((file, text.to_string()))
    }

    /// The checks of one document: a hint under every version a repository knows a newer
    /// stable one of. A coordinate the cache does not hold is asked for on the thread of
    /// the repositories, and the checks run again when it answers.
    fn checks(&self, uri: &str, text: &str) -> Vec<DiagnosticEntry> {
        if !self.enabled() || !self.check_enabled("build.dependencyUpdate") {
            return Vec::new();
        }
        let Some(path) = Uri::from_str(uri).ok().and_then(|uri| file_path(&uri)) else {
            return Vec::new();
        };
        let Some(file) = updates::kind_of(&path) else {
            return Vec::new();
        };
        let now = updates::now();
        let mut entries = Vec::new();
        for slot in updates::sites(file, text) {
            let Some(versions) = self.inner.source.cached(&slot.group, &slot.artifact, now) else {
                self.request(&slot);
                continue;
            };
            let offers = updates::updates_for(&slot.version, &versions);
            let Some(latest) = offers.stable else {
                continue;
            };
            entries.push(DiagnosticEntry {
                span: span_in(text, slot.range.start, slot.range.end),
                severity: Severity::Information,
                message: format!("Newer version available: {latest}"),
                unnecessary: false,
                code: Some("build.dependencyUpdate".to_owned()),
            });
        }
        entries
    }

    fn send_checks(&self, uri: &str, entries: Vec<DiagnosticEntry>) {
        let answer = Answer::new(entries, Confidence::Certain, SOURCE);
        let mut replies = self
            .inner
            .replies
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if let Some(reply) = replies.get_mut(uri) {
            // A full queue drops one list, not the stream: the next answer replaces it.
            let _ = reply.send(answer);
        }
    }

    fn refresh_all(&self) {
        let documents: Vec<String> = self
            .inner
            .documents
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .keys()
            .cloned()
            .collect();
        for uri in documents {
            self.schedule(uri, Duration::ZERO);
        }
    }

    /// Starts the check of a document after `delay`, unless another change of the same
    /// document came first.
    fn schedule(&self, uri: String, delay: Duration) {
        let Some(text) = self.document(&uri) else {
            return;
        };
        let support = self.clone();
        thread::spawn(move || {
            thread::sleep(delay);
            let current = support.document(&uri);
            if !current.is_some_and(|current| Arc::ptr_eq(&current, &text)) {
                return;
            }
            let entries = support.checks(&uri, &text);
            support.send_checks(&uri, entries);
        });
    }

    /// Asks the repositories for one coordinate, once, and lets one thread serve every
    /// document that waits for an answer.
    fn request(&self, slot: &Slot) {
        let key = format!("{}:{}", slot.group, slot.artifact);
        {
            let mut asked = self
                .inner
                .asked
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            if !asked.insert(key.clone()) {
                return;
            }
        }
        let mut queue = self
            .inner
            .queue
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if queue.is_none() {
            let (sender, receiver) = mpsc::channel();
            let weak = Arc::downgrade(&self.inner);
            thread::spawn(move || fetch_versions(weak, receiver));
            *queue = Some(sender);
        }
        if let Some(sender) = queue.as_ref() {
            let _ = sender.send(key);
        }
    }

    /// The offers of the version under the caret: the newest stable and the newest patch.
    fn fixes(&self, query: &ActionQuery) -> Vec<ActionEntry> {
        if !self.enabled() {
            return Vec::new();
        }
        let Some((file, text)) = self.file_of(&query.uri) else {
            return Vec::new();
        };
        let offset = offset_in(&text, query.span.start);
        let Some(slot) = updates::slot_at(file, &text, offset) else {
            return Vec::new();
        };
        if !updates::sites(file, &text)
            .iter()
            .any(|site| site.range == slot.range)
        {
            return Vec::new();
        }
        let Some(versions) = self.versions_of(&slot) else {
            return Vec::new();
        };
        let offers = updates::updates_for(&slot.version, &versions);
        let mut entries = Vec::new();
        if let Some(stable) = offers.stable {
            entries.push(fix_entry(
                &format!("Update to {stable}"),
                true,
                &query.uri,
                &text,
                &slot,
                stable,
            ));
        }
        if let Some(patch) = offers.patch {
            entries.push(fix_entry(
                &format!("Update to latest patch {patch}"),
                false,
                &query.uri,
                &text,
                &slot,
                patch,
            ));
        }
        entries
    }

    fn versions_of(&self, slot: &Slot) -> Option<Vec<String>> {
        let now = updates::now();
        self.inner
            .source
            .cached(&slot.group, &slot.artifact, now)
            .or_else(|| {
                let repositories = updates::repositories(&self.inner.root);
                self.inner
                    .source
                    .versions(&slot.group, &slot.artifact, &repositories, now)
            })
    }

    /// The versions of the artifact under the caret, the newest first, the pre-releases
    /// grey at the end.
    fn completion(&self, query: &DocumentSpot) -> Vec<CompletionEntry> {
        if !self.enabled() {
            return Vec::new();
        }
        let Some((file, text)) = self.file_of(&query.uri) else {
            return Vec::new();
        };
        let offset = offset_in(&text, query.point);
        let Some(slot) = updates::slot_at(file, &text, offset) else {
            return Vec::new();
        };
        let Some(versions) = self.versions_of(&slot) else {
            return Vec::new();
        };
        let prefix = text
            .get(slot.range.start..offset)
            .filter(|_| offset <= slot.range.end)
            .unwrap_or_default();
        let word: String = prefix
            .chars()
            .rev()
            .take_while(|character| character.is_alphanumeric() || matches!(character, '_' | '$'))
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let tail = (offset < slot.range.end).then(|| DocumentEdit {
            span: span_in(&text, offset, slot.range.end),
            text: String::new(),
        });
        let newest = updates::newest_first(&versions);
        let mut entries: Vec<CompletionEntry> = newest
            .into_iter()
            .filter(|version| prefix.is_empty() || version.starts_with(prefix))
            .enumerate()
            .map(|(index, version)| {
                let prerelease = updates::is_prerelease(&version);
                CompletionEntry {
                    label: version.clone(),
                    detail: Some(if prerelease { "pre-release" } else { "stable" }.to_owned()),
                    filter_text: Some(format!("{word}{version}")),
                    sort_text: Some(format!("{}{index:04}", if prerelease { '1' } else { '0' })),
                    insert_text: Some(version),
                    span: Some(span_in(&text, slot.range.start, slot.range.end)),
                    kind: CompletionKind::Value,
                    deprecated: prerelease,
                    additional_edits: tail.iter().cloned().collect(),
                    ..CompletionEntry::default()
                }
            })
            .collect();
        // The stable ones first, the pre-releases in the grey tail.
        entries.sort_by(|left, right| left.sort_text.cmp(&right.sort_text));
        entries
    }

    /// The newest stable version of every named dependency that is newer than the one
    /// the project asks for.
    fn dependency_versions(
        &self,
        query: &DependencyVersionQuery,
        fetch: bool,
    ) -> Vec<DependencyVersion> {
        if !self.enabled() {
            return query
                .dependencies
                .iter()
                .map(|(name, current)| DependencyVersion {
                    name: name.clone(),
                    current: current.clone(),
                    latest: None,
                })
                .collect();
        }
        let repositories = updates::repositories(&self.inner.root);
        query
            .dependencies
            .iter()
            .map(|(name, current)| {
                let latest = name.split_once(':').and_then(|(group, artifact)| {
                    let now = updates::now();
                    let versions = if fetch {
                        self.inner
                            .source
                            .versions(group, artifact, &repositories, now)
                    } else {
                        self.inner.source.cached(group, artifact, now)
                    }?;
                    updates::updates_for(current, &versions).stable
                });
                DependencyVersion {
                    name: name.clone(),
                    current: current.clone(),
                    latest,
                }
            })
            .collect()
    }
}

fn fix_entry(
    title: &str,
    preferred: bool,
    uri: &str,
    text: &str,
    slot: &Slot,
    version: String,
) -> ActionEntry {
    ActionEntry {
        title: title.to_owned(),
        kind: Some(lang_api::ActionKind::QuickFix),
        code: Some("build.updateVersion".to_owned()),
        preferred,
        edits: vec![SourceEdit {
            uri: uri.to_owned(),
            edits: vec![DocumentEdit {
                span: span_in(text, slot.range.start, slot.range.end),
                text: version,
            }],
        }],
        renames: Vec::new(),
        handle: None,
    }
}

/// The thread of the repositories: it asks for one coordinate at a time and lets every
/// open document check its versions again when an answer is in the cache.
fn fetch_versions(weak: Weak<Inner>, queue: Receiver<String>) {
    while let Ok(key) = queue.recv() {
        let Some(inner) = weak.upgrade() else {
            return;
        };
        if let Some((group, artifact)) = key.split_once(':') {
            let repositories = if inner.offline.load(Ordering::SeqCst) {
                Vec::new()
            } else {
                updates::repositories(&inner.root)
            };
            let _ = inner
                .source
                .versions(group, artifact, &repositories, updates::now());
        }
        if let Some(support) = weak.upgrade().map(|inner| BuildScriptSupport { inner }) {
            support.refresh_all();
        }
    }
}

fn point_in(text: &str, offset: usize) -> lang_api::TextPoint {
    let offset = offset.min(text.len());
    let before = &text[..offset];
    let line = before.bytes().filter(|byte| *byte == b'\n').count() as u32;
    let line_start = before.rfind('\n').map_or(0, |index| index + 1);
    let character = text[line_start..offset].encode_utf16().count() as u32;
    lang_api::TextPoint { line, character }
}

fn span_in(text: &str, start: usize, end: usize) -> TextSpan {
    TextSpan {
        start: point_in(text, start),
        end: point_in(text, end),
    }
}

impl SyntaxProvider for BuildScriptSupport {
    fn grammar(&self) -> LanguageFn {
        // The editor colours these files with `file-syntax`; the provider exists for the
        // checks and the entries that ask a language about a version.
        unsafe { LanguageFn::from_raw(no_grammar) }
    }

    fn queries(&self) -> &Queries {
        &NO_QUERIES
    }
}

impl LanguageSupport for BuildScriptSupport {
    fn id(&self) -> LanguageId {
        LANGUAGE_ID
    }

    fn matches(&self, path: &Path) -> bool {
        updates::kind_of(path).is_some()
    }

    fn syntax(&self) -> &dyn SyntaxProvider {
        self
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

    fn actions(&self) -> Option<&ActionProvider> {
        Some(self)
    }

    fn dependency_updates(&self) -> Option<&DependencyUpdatesProvider> {
        Some(self)
    }

    fn set_dependency_updates(&self, enabled: bool) {
        self.set_enabled(enabled);
    }
}

impl SemanticServer for BuildScriptSupport {
    fn open(&self, uri: &str, _version: i32, text: Arc<str>) {
        self.inner
            .documents
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .insert(uri.to_owned(), text.clone());
        self.schedule(uri.to_owned(), Duration::ZERO);
    }

    fn change(&self, uri: &str, _version: i32, text: Arc<str>, _edits: Vec<DocumentEdit>) {
        self.inner
            .documents
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .insert(uri.to_owned(), text);
        self.schedule(uri.to_owned(), CHECK_DELAY);
    }

    fn save(&self, _uri: &str) {}

    fn version(&self, uri: &str) -> Option<i32> {
        self.inner
            .documents
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .contains_key(uri)
            .then_some(0)
    }

    fn close(&self, uri: &str) {
        self.inner
            .documents
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(uri);
        self.inner
            .replies
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(uri);
    }
}

impl Provider<DiagnosticsQuery, Vec<DiagnosticEntry>> for BuildScriptSupport {
    fn start(&self, query: DiagnosticsQuery, _: Cancel, reply: Reply<Vec<DiagnosticEntry>>) {
        self.inner
            .replies
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .insert(query.0.clone(), reply);
        self.schedule(query.0, Duration::ZERO);
    }
}

impl Provider<DocumentSpot, CompletionList> for BuildScriptSupport {
    fn start(&self, query: DocumentSpot, _: Cancel, mut reply: Reply<CompletionList>) {
        let support = self.clone();
        thread::spawn(move || {
            let entries = support.completion(&query);
            let _ = reply.send(Answer::new(
                CompletionList {
                    entries,
                    complete: true,
                },
                Confidence::Certain,
                SOURCE,
            ));
        });
    }
}

impl Provider<ActionQuery, Vec<ActionEntry>> for BuildScriptSupport {
    fn start(&self, query: ActionQuery, _: Cancel, mut reply: Reply<Vec<ActionEntry>>) {
        let support = self.clone();
        thread::spawn(move || {
            let entries = support.fixes(&query);
            let _ = reply.send(Answer::new(entries, Confidence::Certain, SOURCE));
        });
    }
}

impl Provider<DependencyVersionQuery, Vec<DependencyVersion>> for BuildScriptSupport {
    fn start(
        &self,
        query: DependencyVersionQuery,
        cancel: Cancel,
        mut reply: Reply<Vec<DependencyVersion>>,
    ) {
        let support = self.clone();
        thread::spawn(move || {
            let cached = support.dependency_versions(&query, false);
            let _ = reply.send(Answer::new(cached.clone(), Confidence::Partial, SOURCE));
            if !support.enabled() {
                return;
            }
            let mut answers = cached;
            for (index, (name, current)) in query.dependencies.iter().enumerate() {
                if cancel.is_cancelled() || reply.is_cancelled() {
                    return;
                }
                let Some((group, artifact)) = name.split_once(':') else {
                    continue;
                };
                let repositories = updates::repositories(&support.inner.root);
                let Some(versions) =
                    support
                        .inner
                        .source
                        .versions(group, artifact, &repositories, updates::now())
                else {
                    continue;
                };
                let latest = updates::updates_for(current, &versions).stable;
                if answers[index].latest != latest {
                    answers[index].latest = latest;
                    let _ = reply.send(Answer::new(answers.clone(), Confidence::Certain, SOURCE));
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use lang_api::{ActionQuery, DependencyVersionQuery, DocumentSpot, Replies, Severity, ask};
    use platform::CanonPath;

    use super::*;

    /// A repository on the loopback interface that serves one `maven-metadata.xml`.
    struct Repository {
        url: String,
        requests: Arc<AtomicUsize>,
        stop: Arc<AtomicBool>,
        thread: Option<thread::JoinHandle<()>>,
    }

    impl Repository {
        fn start(versions: &[&str]) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("a free port");
            listener.set_nonblocking(true).unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let body = metadata(versions);
            let requests = Arc::new(AtomicUsize::new(0));
            let stop = Arc::new(AtomicBool::new(false));
            let counted = requests.clone();
            let stopped = stop.clone();
            let thread = thread::spawn(move || {
                while !stopped.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            counted.fetch_add(1, Ordering::SeqCst);
                            answer(&mut stream, &body);
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(_) => return,
                    }
                }
            });
            Self {
                url,
                requests,
                stop,
                thread: Some(thread),
            }
        }

        fn requests(&self) -> usize {
            self.requests.load(Ordering::SeqCst)
        }
    }

    impl Drop for Repository {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    fn metadata(versions: &[&str]) -> String {
        let versions: String = versions
            .iter()
            .map(|version| format!("            <version>{version}</version>\n"))
            .collect();
        format!(
            "<?xml version=\"1.0\"?>\n<metadata>\n    <groupId>org.example</groupId>\n    \
             <artifactId>demo</artifactId>\n    <versioning>\n        <versions>\n{versions}        \
             </versions>\n    </versioning>\n</metadata>\n"
        )
    }

    fn answer(stream: &mut TcpStream, body: &str) {
        // The listener is non-blocking, and a connection inherits that on Windows: a read
        // would fail at once while the client is still on its way. An answer written then
        // closes the socket with the request unread, Windows resets it, and the client
        // loses the answer it may already hold. The connection is made blocking and the
        // head of the request is read to its end before anything is written back.
        let _ = stream.set_nonblocking(false);
        let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
        let mut request = Vec::new();
        let mut chunk = [0u8; 1024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            match stream.read(&mut chunk) {
                Ok(0) => break,
                Ok(count) => request.extend_from_slice(&chunk[..count]),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/xml\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n",
            body.len()
        );
        let _ = stream.write_all(head.as_bytes());
        let _ = stream.write_all(body.as_bytes());
    }

    fn pom(repository: &str, version: &str) -> String {
        format!(
            "<project>\n    <repositories>\n        <repository>\n            \
             <id>local</id>\n            <url>{repository}</url>\n        </repository>\n    \
             </repositories>\n    <dependencies>\n        <dependency>\n            \
             <groupId>org.example</groupId>\n            <artifactId>demo</artifactId>\n            \
             <version>{version}</version>\n        </dependency>\n    </dependencies>\n</project>\n"
        )
    }

    fn uri_of(path: &Path) -> String {
        CanonPath::new(path).unwrap().to_uri()
    }

    fn wait_for<T>(
        replies: &mut Replies<T>,
        keep: impl Fn(&T) -> bool,
        timeout: Duration,
    ) -> Option<T> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(answer) = replies.poll_ready()
                && keep(&answer.value)
            {
                return Some(answer.value);
            }
            if Instant::now() > deadline {
                return None;
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn a_hint_appears_when_the_repository_answers() {
        let repository = Repository::start(&["1.0", "1.1", "2.0-rc1"]);
        let root = test_support::TempDir::new("build-scripts-hint");
        let path = root.path().join("pom.xml");
        let text = pom(&repository.url, "1.0");
        std::fs::write(&path, &text).unwrap();
        let support =
            BuildScriptSupport::new(root.path().to_path_buf(), root.path().join("cache"), false);
        let uri = uri_of(&path);
        support.open(&uri, 1, Arc::from(text.as_str()));

        let provider: &DiagnosticsProvider = &support;
        let mut replies = ask(provider, DiagnosticsQuery(uri));
        let entries = wait_for(
            &mut replies,
            |entries| !entries.is_empty(),
            Duration::from_secs(30),
        )
        .expect("the repository answers");

        assert_eq!(entries.len(), 1, "{entries:?}");
        assert_eq!(entries[0].message, "Newer version available: 1.1");
        assert_eq!(entries[0].severity, Severity::Information);
        assert_eq!(
            entries[0].span.start.line, 11,
            "the hint stands on the version"
        );
        assert!(repository.requests() > 0);
    }

    #[test]
    fn the_fixes_replace_the_version_and_the_property() {
        let root = test_support::TempDir::new("build-scripts-fix");
        let path = root.path().join("pom.xml");
        let text = "<project>\n    <properties>\n        <demo.version>2.0.0</demo.version>\n    \
                    </properties>\n    <dependencies>\n        <dependency>\n            \
                    <groupId>org.example</groupId>\n            <artifactId>demo</artifactId>\n            \
                    <version>${demo.version}</version>\n        </dependency>\n    </dependencies>\n</project>\n";
        std::fs::write(&path, text).unwrap();
        let cache = root.path().join("cache");
        let versions = [
            "2.0.0".to_owned(),
            "2.0.1".to_owned(),
            "2.1.0".to_owned(),
            "3.0.0-rc1".to_owned(),
        ];
        let file = updates::cache_file(&cache, "org.example", "demo").unwrap();
        updates::write_cache(&file, &versions, updates::now()).unwrap();
        let support = BuildScriptSupport::new(root.path().to_path_buf(), cache, false);
        let uri = uri_of(&path);
        support.open(&uri, 1, Arc::from(text));

        let offset = text.find("2.0.0").unwrap();
        let query = ActionQuery {
            uri,
            version: 1,
            span: span_in(text, offset, offset + 5),
            only: None,
        };
        let provider: &ActionProvider = &support;
        let mut replies = ask(provider, query);
        let entries = wait_for(
            &mut replies,
            |entries| !entries.is_empty(),
            Duration::from_secs(5),
        )
        .expect("the version has fixes");

        assert_eq!(entries[0].title, "Update to 2.1.0");
        assert!(entries[0].preferred);
        assert_eq!(entries[1].title, "Update to latest patch 2.0.1");
        let edit = &entries[0].edits[0].edits[0];
        assert_eq!(edit.text, "2.1.0");
        assert_eq!(
            edit.span.start.line, 2,
            "the property is the place that changes"
        );
    }

    #[test]
    fn completion_lists_the_versions_with_the_pre_releases_last() {
        let root = test_support::TempDir::new("build-scripts-completion");
        let path = root.path().join("pom.xml");
        let text = "<project><dependencies><dependency><groupId>org.example</groupId>\
                    <artifactId>demo</artifactId><version>1.</version></dependency>\
                    </dependencies></project>";
        std::fs::write(&path, text).unwrap();
        let cache = root.path().join("cache");
        let versions = [
            "1.0".to_owned(),
            "1.10".to_owned(),
            "1.9".to_owned(),
            "2.0-rc1".to_owned(),
        ];
        let file = updates::cache_file(&cache, "org.example", "demo").unwrap();
        updates::write_cache(&file, &versions, updates::now()).unwrap();
        let support = BuildScriptSupport::new(root.path().to_path_buf(), cache, false);
        let uri = uri_of(&path);
        support.open(&uri, 1, Arc::from(text));

        let caret = text.find("1.</version>").unwrap() + 2;
        let query = DocumentSpot {
            uri,
            version: 1,
            point: point_in(text, caret),
        };
        let provider: &CompletionProvider = &support;
        let mut replies = ask(provider, query);
        let list = wait_for(
            &mut replies,
            |list| !list.entries.is_empty(),
            Duration::from_secs(5),
        )
        .expect("the cache has the versions");

        let labels: Vec<&str> = list
            .entries
            .iter()
            .map(|entry| entry.label.as_str())
            .collect();
        assert_eq!(
            labels,
            ["1.10", "1.9", "1.0"],
            "only the typed prefix, newest first"
        );
        assert!(
            list.entries.iter().all(|entry| !entry.deprecated),
            "the pre-releases are behind the typed prefix"
        );
        assert_eq!(list.entries[0].insert_text.as_deref(), Some("1.10"));
        assert!(list.entries[0].span.is_some());
    }

    #[test]
    fn a_pre_release_is_grey_and_last_in_the_whole_list() {
        let root = test_support::TempDir::new("build-scripts-pre-release");
        let path = root.path().join("pom.xml");
        let text = "<project><dependencies><dependency><groupId>org.example</groupId>\
                    <artifactId>demo</artifactId><version></version></dependency>\
                    </dependencies></project>";
        std::fs::write(&path, text).unwrap();
        let cache = root.path().join("cache");
        let versions = ["2.0-rc1".to_owned(), "1.0".to_owned(), "1.1".to_owned()];
        let file = updates::cache_file(&cache, "org.example", "demo").unwrap();
        updates::write_cache(&file, &versions, updates::now()).unwrap();
        let support = BuildScriptSupport::new(root.path().to_path_buf(), cache, false);
        let uri = uri_of(&path);
        support.open(&uri, 1, Arc::from(text));

        let caret = text.find("</version>").unwrap();
        let query = DocumentSpot {
            uri,
            version: 1,
            point: point_in(text, caret),
        };
        let provider: &CompletionProvider = &support;
        let mut replies = ask(provider, query);
        let list = wait_for(
            &mut replies,
            |list| !list.entries.is_empty(),
            Duration::from_secs(5),
        )
        .expect("the cache has the versions");

        let labels: Vec<&str> = list
            .entries
            .iter()
            .map(|entry| entry.label.as_str())
            .collect();
        assert_eq!(labels, ["1.1", "1.0", "2.0-rc1"]);
        assert!(list.entries[2].deprecated, "a pre-release is grey");
        assert!(!list.entries[0].deprecated);
    }

    #[test]
    fn an_offline_project_shows_nothing_and_asks_nothing() {
        let repository = Repository::start(&["1.0", "1.1"]);
        let root = test_support::TempDir::new("build-scripts-offline");
        let path = root.path().join("pom.xml");
        let text = pom(&repository.url, "1.0");
        std::fs::write(&path, &text).unwrap();
        let support =
            BuildScriptSupport::new(root.path().to_path_buf(), root.path().join("cache"), true);
        let uri = uri_of(&path);
        support.open(&uri, 1, Arc::from(text.as_str()));

        let provider: &DiagnosticsProvider = &support;
        let mut replies = ask(provider, DiagnosticsQuery(uri));
        let entries =
            wait_for(&mut replies, |_| true, Duration::from_secs(5)).expect("the check answers");
        thread::sleep(Duration::from_millis(100));

        assert!(entries.is_empty());
        assert_eq!(repository.requests(), 0, "offline asks no repository");
    }

    #[test]
    fn a_check_that_is_off_asks_no_repository() {
        let repository = Repository::start(&["1.0", "1.1"]);
        let root = test_support::TempDir::new("build-scripts-off");
        let path = root.path().join("pom.xml");
        let text = pom(&repository.url, "1.0");
        std::fs::write(&path, &text).unwrap();
        let support =
            BuildScriptSupport::new(root.path().to_path_buf(), root.path().join("cache"), false);
        let mut settings = lang_api::InspectionSettings::default();
        settings.set("build.dependencyUpdate", false, None);
        let checks = lang_api::DisabledInspections::default();
        checks.replace(&settings);
        support.set_inspection_checks(checks);
        let uri = uri_of(&path);
        support.open(&uri, 1, Arc::from(text.as_str()));

        let provider: &DiagnosticsProvider = &support;
        let mut replies = ask(provider, DiagnosticsQuery(uri));
        let entries =
            wait_for(&mut replies, |_| true, Duration::from_secs(5)).expect("the check answers");
        thread::sleep(Duration::from_millis(100));

        assert!(entries.is_empty());
        assert_eq!(
            repository.requests(),
            0,
            "a check that is off asks no repository"
        );
    }

    #[test]
    fn the_window_gets_the_newest_stable_version_of_a_dependency() {
        let root = test_support::TempDir::new("build-scripts-window");
        let cache = root.path().join("cache");
        let versions = ["1.0".to_owned(), "1.5".to_owned(), "2.0-M1".to_owned()];
        let file = updates::cache_file(&cache, "org.example", "demo").unwrap();
        updates::write_cache(&file, &versions, updates::now()).unwrap();
        let support = BuildScriptSupport::new(root.path().to_path_buf(), cache, false);

        let query = DependencyVersionQuery {
            dependencies: vec![
                ("org.example:demo".to_owned(), "1.0".to_owned()),
                ("org.example:demo".to_owned(), "1.5".to_owned()),
            ],
        };
        let provider: &DependencyUpdatesProvider = &support;
        let mut replies = ask(provider, query);
        let found = wait_for(
            &mut replies,
            |found| found.iter().any(|version| version.latest.is_some()),
            Duration::from_secs(5),
        )
        .expect("the cache answers");

        assert_eq!(found[0].latest.as_deref(), Some("1.5"));
        assert_eq!(found[1].latest, None);
        assert_eq!(found[1].current, "1.5");
    }

    #[test]
    fn a_document_that_closes_keeps_no_reply_and_no_text() {
        let root = test_support::TempDir::new("build-scripts-close");
        let support =
            BuildScriptSupport::new(root.path().to_path_buf(), root.path().join("c"), false);
        let uri = "file:///C:/p/pom.xml";
        support.open(uri, 1, Arc::from("<project/>"));
        assert!(support.version(uri).is_some());
        support.close(uri);
        assert!(support.version(uri).is_none());
        assert!(support.document(uri).is_none());
    }

    #[test]
    fn only_a_build_file_is_matched() {
        let root = test_support::TempDir::new("build-scripts-match");
        let support =
            BuildScriptSupport::new(root.path().to_path_buf(), root.path().join("c"), false);
        assert!(support.matches(Path::new("C:/p/pom.xml")));
        assert!(support.matches(Path::new("C:/p/build.gradle")));
        assert!(support.matches(Path::new("C:/p/build.gradle.kts")));
        assert!(support.matches(Path::new("C:/p/gradle/libs.versions.toml")));
        assert!(!support.matches(Path::new("C:/p/settings.gradle")));
        assert!(!support.matches(Path::new("C:/p/Main.java")));
    }
}
