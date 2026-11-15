//! Message bundles: the `*.properties` files of one base name with their locales, the
//! places of the project that use a key and the checks of both sides.
//!
//! A bundle is `<base>.properties` and `<base>_<locale>.properties` in a resource folder
//! of a module. The base names come from `spring.messages.basename`, `messages` by
//! default, and from the strings of `getBundle("…")` in the sources. Keys are used in
//! Java — `getMessage("key", …)`, `getString`, `getObject` and `message = "{key}"` of a
//! validation annotation — and in the templates — `#{key}` inside a `th:*` attribute.
//!
//! The scan is native, so completion, Ctrl+B, the checks and Find Usages work without a
//! server. The index keeps one cache entry per file, keyed by the buffer text or by the
//! modification time and size on disk, so an edit re-reads that file alone. A worker
//! thread does the reading and the parsing and answers the queries; the caller never
//! waits on it.

use std::collections::{HashMap, HashSet, VecDeque};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, RwLock, Weak};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use base::Cancel;
use futures_util::StreamExt;
use lang_api::{
    ActionEntry, ActionKind, ActionProvider, ActionQuery, Answer, CompletionEntry, CompletionKind,
    CompletionList, CompletionProvider, Confidence, DiagnosticEntry, DiagnosticsProvider,
    DiagnosticsQuery, DocumentEdit, DocumentSpot, HoverProvider, HoverText, LanguageId,
    LanguageSupport, NavigationKind, NavigationProvider, NavigationQuery, Provider, Queries,
    RenameProvider, RenameQuery, Reply, SemanticServer, Severity, SourceEdit, SourceId, SourceSpan,
    SyntaxProvider, TextPoint, TextSpan, UsageEntry, UsageKind, UsageProvider, WorkspaceChange,
    offset_in,
};
use lsp_client::lsp_types::Uri;
use lsp_client::uri::file_path;
use platform::{CanonPath, path_key};
use project_model::ProjectModel;
use semantic_router::{LspProviders, Router};
use tree_sitter_language::LanguageFn;

pub const LANGUAGE_ID: LanguageId = LanguageId("message-bundle");

const SOURCE: SourceId = SourceId::new("message-bundles");
/// The base name Spring uses when a project configures none.
const DEFAULT_BASE: &str = "messages";
/// The methods whose first argument is a key of a bundle.
const KEY_METHODS: &[&str] = &["getMessage", "getString", "getObject"];
/// The Spring setting that names the base names of the bundles of a project.
const BASENAME_SETTING: &str = "spring.messages.basename";
/// The member of a validation annotation that holds a key.
const MESSAGE_MEMBER: &str = "message";
/// The pause before the checks of a file that just changed run again.
const CHECK_DELAY: Duration = Duration::from_millis(120);
/// How long the keys of the bundles are read again even though no file changed.
const KEYS_TTL: Duration = Duration::from_millis(400);
/// How long the sources are walked for new, removed or changed files again.
const SOURCES_TTL: Duration = Duration::from_secs(3);
/// The folders whose sources and templates never carry a key.
const SKIP_FOLDERS: &[&str] = &[
    "target",
    "build",
    "out",
    "bin",
    "node_modules",
    ".git",
    ".gradle",
    ".idea",
];

unsafe extern "C" fn no_grammar() -> *const () {
    std::ptr::null()
}

const NO_QUERIES: Queries = Queries {
    highlights: "",
    folds: "",
    indents: "",
    locals: "",
};

/// Which side of a message a file is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FileKind {
    Java,
    Template,
    Properties,
}

fn file_kind(path: &Path) -> Option<FileKind> {
    let extension = path.extension()?.to_str()?;
    if extension.eq_ignore_ascii_case("java") {
        Some(FileKind::Java)
    } else if extension.eq_ignore_ascii_case("html") || extension.eq_ignore_ascii_case("htm") {
        Some(FileKind::Template)
    } else if extension.eq_ignore_ascii_case("properties") {
        Some(FileKind::Properties)
    } else {
        None
    }
}

/// The messages of one project. Cloning it shares the index.
#[derive(Clone)]
pub struct MessageBundles {
    inner: Arc<Inner>,
}

/// The message bundles of the project at `root`.
pub fn message_bundles(root: PathBuf) -> Arc<MessageBundles> {
    Arc::new(MessageBundles {
        inner: Arc::new(Inner::new(root)),
    })
}

impl MessageBundles {
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

    /// The folders the bundles are read from: the resource folders of the modules. An
    /// empty list falls back to the standard resource folders of the project.
    pub fn set_roots(&self, roots: Vec<PathBuf>) {
        let mut state = self.lock_roots();
        if *state == roots {
            return;
        }
        *state = roots;
        drop(state);
        self.inner.invalidate_keys();
    }

    /// The resource and source folders of the project model.
    pub fn set_model(&self, model: &ProjectModel) {
        let roots: Vec<PathBuf> = model
            .modules
            .iter()
            .flat_map(|module| &module.units)
            .flat_map(|unit| unit.resource_roots.iter().cloned())
            .collect();
        self.set_roots(roots);
    }

    /// The root of the project, for a reader of the sources.
    pub fn root(&self) -> &Path {
        &self.inner.root
    }

    fn lock_roots(&self) -> std::sync::MutexGuard<'_, Vec<PathBuf>> {
        self.inner
            .roots
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    fn roots(&self) -> Vec<PathBuf> {
        let configured = self.lock_roots().clone();
        if !configured.is_empty() {
            return dedupe(configured);
        }
        let mut roots = Vec::new();
        for folder in ["src/main/resources", "src/test/resources"] {
            let candidate = self.inner.root.join(folder);
            if candidate.is_dir() {
                roots.push(candidate);
            }
        }
        if roots.is_empty() {
            roots.push(self.inner.root.clone());
        }
        dedupe(roots)
    }

    fn text_of(&self, uri: &str) -> Option<Arc<str>> {
        if let Some(text) = self
            .inner
            .documents
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .get(uri)
        {
            return Some(text.clone());
        }
        let path = file_path(&Uri::from_str(uri).ok()?)?;
        std::fs::read_to_string(path).ok().map(Arc::from)
    }

    fn text_of_path(&self, path: &Path) -> Option<Arc<str>> {
        let uri = uri_of(path)?;
        self.text_of(&uri)
    }

    fn lock_state(&self) -> MutexGuard<'_, IndexState> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    /// The text of the open buffers, so that a refresh does not lock the map once per
    /// file.
    fn buffers(&self) -> HashMap<String, Arc<str>> {
        self.inner
            .documents
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }

    /// Where the text of `path` comes from now: the open buffer, or the file on disk
    /// with what makes it stale.
    fn origin_of(
        &self,
        path: &Path,
        buffers: &HashMap<String, Arc<str>>,
    ) -> Option<(FileOrigin, Option<Arc<str>>)> {
        if let Some(uri) = uri_of(path)
            && let Some(text) = buffers.get(&uri)
        {
            return Some((FileOrigin::Buffer(text.clone()), Some(text.clone())));
        }
        let metadata = std::fs::metadata(path).ok()?;
        Some((
            FileOrigin::Disk {
                modified: metadata.modified().ok(),
                size: metadata.len(),
            },
            None,
        ))
    }

    /// The keys and files of the bundles, read again when a buffer changed or the
    /// previous read is old enough. Only a `.properties` file whose origin changed is
    /// parsed again.
    fn keys(&self) -> Arc<KeyIndex> {
        let dirty = self.inner.dirty_keys.swap(false, Ordering::Relaxed);
        let cached = self
            .inner
            .keys
            .read()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone();
        if !dirty
            && let Some(cached) = cached
            && cached.at.elapsed() < KEYS_TTL
        {
            return cached.index;
        }
        let index = Arc::new(self.build_keys());
        *self
            .inner
            .keys
            .write()
            .unwrap_or_else(|poison| poison.into_inner()) = Some(CachedKeys {
            at: Instant::now(),
            index: index.clone(),
        });
        index
    }

    /// The places of the sources that use a key, and the bases of `getBundle`. The
    /// project is walked for new and removed files at most every `SOURCES_TTL`; an edit
    /// of a buffer re-reads that file alone.
    fn sources(&self) -> Arc<Sources> {
        let _building = self
            .inner
            .sources_lock
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let dirty = self.inner.dirty_sources.swap(false, Ordering::Relaxed);
        let cached = self
            .inner
            .sources
            .read()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone();
        if !dirty
            && let Some(cached) = cached
            && cached.at.elapsed() < SOURCES_TTL
        {
            return cached.index;
        }
        let touched = self.take_dirty_paths();
        let index = Arc::new(self.build_sources(&touched, dirty));
        *self
            .inner
            .sources
            .write()
            .unwrap_or_else(|poison| poison.into_inner()) = Some(CachedSources {
            at: Instant::now(),
            index: index.clone(),
        });
        // `getBundle` strings of a new or changed source can name another bundle.
        self.inner.dirty_keys.store(true, Ordering::Relaxed);
        index
    }

    fn take_dirty_paths(&self) -> Vec<PathBuf> {
        let mut paths = self
            .inner
            .dirty_paths
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        paths.drain().collect()
    }

    /// Reads one source into the cache at once, so that a `getBundle` string of a file
    /// that was just opened or saved names its bundle without a walk of the project.
    fn refresh_source(&self, path: PathBuf) {
        let buffers = self.buffers();
        let Some((origin, buffer_text)) = self.origin_of(&path, &buffers) else {
            return;
        };
        let Some(text) = buffer_text.or_else(|| std::fs::read_to_string(&path).ok().map(Arc::from))
        else {
            return;
        };
        let entry = source_entry(&path, &text, origin);
        let mut state = self.lock_state();
        state.sources.insert(path.clone(), entry);
        if !state.source_files.iter().any(|known| known == &path) {
            state.source_files.push(path);
        }
        drop(state);
        self.inner.invalidate_keys();
    }

    /// The base names of the bundles: the Spring setting of every resource folder, the
    /// `getBundle` strings of the sources read so far, and the default.
    fn bases(&self, buffers: &HashMap<String, Arc<str>>) -> Vec<String> {
        let mut bases = Vec::new();
        for root in self.roots() {
            bases.extend(self.configured_bases(&root, buffers));
        }
        bases.extend(
            self.lock_state()
                .sources
                .values()
                .flat_map(|source| source.bases.iter().cloned()),
        );
        bases.push(DEFAULT_BASE.to_owned());
        dedupe_names(bases)
    }

    fn build_keys(&self) -> KeyIndex {
        let buffers = self.buffers();
        let mut bundles = Vec::new();
        for base in self.bases(&buffers) {
            for root in self.roots() {
                if let Some(bundle) = self.read_bundle(&root, &base, &buffers) {
                    bundles.push(bundle);
                }
            }
        }
        KeyIndex { bundles }
    }

    fn read_bundle(
        &self,
        resource_root: &Path,
        base: &str,
        buffers: &HashMap<String, Arc<str>>,
    ) -> Option<Bundle> {
        let (folder_name, stem) = match base.rsplit_once('/') {
            Some((folder, stem)) => (folder, stem),
            None => ("", base),
        };
        if stem.is_empty() {
            return None;
        }
        let folder = resource_root.join(folder_name);
        let entries = std::fs::read_dir(&folder).ok()?;
        let prefix = format!("{stem}_");
        let mut files = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file()
                || path
                    .extension()
                    .is_none_or(|extension| extension != "properties")
            {
                continue;
            }
            let Some(name) = path.file_stem().and_then(|name| name.to_str()) else {
                continue;
            };
            let locale = if name == stem {
                None
            } else if let Some(locale) = name.strip_prefix(&prefix) {
                Some(locale.to_owned())
            } else {
                continue;
            };
            let Some(keys) = self.bundle_keys(&path, buffers) else {
                continue;
            };
            files.push(BundleFile { path, locale, keys });
        }
        if files.is_empty() {
            return None;
        }
        files.sort_by(|left, right| left.locale.cmp(&right.locale));
        Some(Bundle {
            base: base.to_owned(),
            base_file: files.iter().position(|file| file.locale.is_none()),
            files,
        })
    }

    /// The keys of one `.properties` file, parsed again only when its origin changed.
    fn bundle_keys(
        &self,
        path: &Path,
        buffers: &HashMap<String, Arc<str>>,
    ) -> Option<Arc<Vec<KeyLine>>> {
        let (origin, buffer_text) = self.origin_of(path, buffers)?;
        if let Some(cached) = self.lock_state().properties.get(path).cloned()
            && cached.origin.matches(&origin)
        {
            return Some(cached.keys);
        }
        let text = buffer_text.or_else(|| std::fs::read_to_string(path).ok().map(Arc::from))?;
        let keys = Arc::new(properties_keys(&text));
        self.lock_state().properties.insert(
            path.to_path_buf(),
            CachedKeysFile {
                origin,
                keys: keys.clone(),
            },
        );
        Some(keys)
    }

    /// The base names `spring.messages.basename` of the configuration files of a
    /// resource folder. The setting holds a comma-separated list; a value without a
    /// separator is one name. `classpath:` and leading slashes are not part of a name.
    fn configured_bases(
        &self,
        resource_root: &Path,
        buffers: &HashMap<String, Arc<str>>,
    ) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(resource_root) else {
            return Vec::new();
        };
        let mut files: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| {
                        extension.eq_ignore_ascii_case("properties")
                            || extension.eq_ignore_ascii_case("yml")
                            || extension.eq_ignore_ascii_case("yaml")
                    })
            })
            .collect();
        files.sort();
        let mut bases = Vec::new();
        for path in files {
            let Some(name) = path.file_stem().and_then(|name| name.to_str()) else {
                continue;
            };
            if !name.starts_with("application") {
                continue;
            }
            let Some((origin, buffer_text)) = self.origin_of(&path, buffers) else {
                continue;
            };
            if let Some(cached) = self.lock_state().settings.get(&path).cloned()
                && cached.origin.matches(&origin)
            {
                bases.extend(cached.bases.iter().cloned());
                continue;
            }
            let Some(text) =
                buffer_text.or_else(|| std::fs::read_to_string(&path).ok().map(Arc::from))
            else {
                continue;
            };
            let yaml = path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| !extension.eq_ignore_ascii_case("properties"));
            let parsed = if let Some(value) = if yaml {
                yaml_setting(&text, BASENAME_SETTING)
            } else {
                properties_setting(&text, BASENAME_SETTING)
            } {
                normalize_bases(&value)
            } else {
                Vec::new()
            };
            self.lock_state().settings.insert(
                path.clone(),
                CachedBases {
                    origin,
                    bases: parsed.clone(),
                },
            );
            bases.extend(parsed);
        }
        bases
    }

    fn build_sources(&self, touched: &[PathBuf], dirty: bool) -> Sources {
        let buffers = self.buffers();
        let (previous, walked, known) = {
            let state = self.lock_state();
            (
                state.sources.clone(),
                state.walked,
                state.source_files.clone(),
            )
        };
        let stale = walked.is_none_or(|at| at.elapsed() >= SOURCES_TTL);
        let touched: HashSet<PathBuf> = if dirty {
            touched.iter().cloned().collect()
        } else {
            HashSet::new()
        };
        let files = if stale {
            let mut files = Vec::new();
            collect_sources(&self.inner.root, &mut files);
            for uri in buffers.keys() {
                let Some(path) = path_of_uri(uri) else {
                    continue;
                };
                if matches!(file_kind(&path), Some(FileKind::Java | FileKind::Template))
                    && !files.iter().any(|known| known == &path)
                {
                    files.push(path);
                }
            }
            files.sort();
            files.dedup();
            files
        } else {
            let mut files = known;
            for path in &touched {
                if !files.iter().any(|known| known == path) {
                    files.push(path.clone());
                }
            }
            files
        };
        let mut entries: HashMap<PathBuf, CachedSource> = HashMap::new();
        for path in &files {
            let cached = previous.get(path).cloned();
            if !stale && !touched.contains(path) {
                if let Some(cached) = cached {
                    entries.insert(path.clone(), cached);
                }
                continue;
            }
            let Some((origin, buffer_text)) = self.origin_of(path, &buffers) else {
                continue;
            };
            if let Some(cached) = &cached
                && cached.origin.matches(&origin)
            {
                entries.insert(path.clone(), cached.clone());
                continue;
            }
            let Some(text) =
                buffer_text.or_else(|| std::fs::read_to_string(path).ok().map(Arc::from))
            else {
                continue;
            };
            entries.insert(path.clone(), source_entry(path, &text, origin));
        }
        let mut usages = Vec::new();
        let mut bases = Vec::new();
        for source in entries.values() {
            usages.extend(source.usages.iter().cloned());
            bases.extend(source.bases.iter().cloned());
        }
        let mut state = self.lock_state();
        state.sources = entries;
        if stale {
            state.source_files = files;
            state.walked = Some(Instant::now());
        } else {
            for path in touched {
                if !state.source_files.contains(&path) {
                    state.source_files.push(path);
                }
            }
        }
        Sources { usages, bases }
    }

    /// The files of the localized set that holds `path`: the base file and its locales,
    /// as the language found them. `None` when `path` is not in a set of two or more
    /// files.
    pub fn files_of(&self, path: &Path) -> Option<Vec<lang_api::BundleFile>> {
        let index = self.keys();
        let wanted = path_key(path);
        for bundle in &index.bundles {
            if !bundle
                .files
                .iter()
                .any(|file| path_key(&file.path) == wanted)
            {
                continue;
            }
            if bundle.files.len() < 2 {
                return None;
            }
            return Some(
                bundle
                    .files
                    .iter()
                    .map(|file| lang_api::BundleFile {
                        path: file.path.clone(),
                        locale: file.locale.clone(),
                    })
                    .collect(),
            );
        }
        None
    }

    /// The bundle and file that hold `path`.
    fn bundle_file(&self, path: &Path) -> Option<(Arc<KeyIndex>, usize, usize)> {
        let index = self.keys();
        let wanted = path_key(path);
        for (bundle_at, bundle) in index.bundles.iter().enumerate() {
            for (file_at, file) in bundle.files.iter().enumerate() {
                if path_key(&file.path) == wanted {
                    return Some((index.clone(), bundle_at, file_at));
                }
            }
        }
        None
    }

    fn bundle_of(&self, path: &Path) -> Option<Arc<KeyIndex>> {
        self.bundle_file(path).map(|(index, _, _)| index)
    }

    /// The bundle whose base file is the place a new key goes into.
    fn base_bundle(&self) -> Option<(String, PathBuf, Arc<str>)> {
        for bundle in &self.keys().bundles {
            let Some(file) = bundle.base_file() else {
                continue;
            };
            let Some(text) = self.text_of_path(&file.path) else {
                continue;
            };
            return Some((bundle.base.clone(), file.path.clone(), text));
        }
        None
    }

    /// The keys of the bundles where `key` appears, with the values of every locale.
    fn values_of(&self, key: &str) -> Vec<(String, Option<String>, String)> {
        let mut found = Vec::new();
        for bundle in &self.keys().bundles {
            for file in &bundle.files {
                if let Some(line) = file.keys.iter().find(|line| line.key == key) {
                    found.push((bundle.base.clone(), file.locale.clone(), line.value.clone()));
                }
            }
        }
        found
    }

    /// The text a query sees: the open buffer, or the file on disk.
    fn query_text(&self, uri: &str) -> Option<(PathBuf, Arc<str>)> {
        let path = file_path(&Uri::from_str(uri).ok()?)?;
        let text = self.text_of(uri)?;
        Some((path, text))
    }

    fn key_at(&self, path: &Path, text: &str, offset: usize) -> Option<KeyContext> {
        match file_kind(path)? {
            FileKind::Properties => property_at(text, offset),
            FileKind::Java => java_key_at(text, offset),
            FileKind::Template => template_key_at(text, offset),
        }
    }

    /// The keys of the bundles that fit `prefix`, for the completion inside a key.
    fn completion(&self, kind: FileKind, text: &str, offset: usize) -> Option<CompletionList> {
        if kind == FileKind::Properties {
            return None;
        }
        let context = match kind {
            FileKind::Java => java_key_at(text, offset),
            FileKind::Template => template_key_at(text, offset),
            FileKind::Properties => None,
        }?;
        let typed_end = offset.max(context.span.start).min(text.len());
        let typed = &text[context.span.start..typed_end];
        let mut entries = Vec::new();
        let mut known = HashSet::new();
        for bundle in &self.keys().bundles {
            for file in &bundle.files {
                for line in file.keys.iter() {
                    if !known.insert(line.key.clone()) {
                        continue;
                    }
                    if !line.key.starts_with(typed) {
                        continue;
                    }
                    let locales = bundle
                        .files
                        .iter()
                        .filter(|file| file.has_key(&line.key))
                        .count();
                    entries.push(CompletionEntry {
                        label: line.key.clone(),
                        detail: Some(bundle.base.clone()),
                        insert_text: Some(line.key.clone()),
                        span: Some(span_in(text, context.span.start, typed_end)),
                        kind: CompletionKind::Property,
                        filter_text: Some(line.key.clone()),
                        label_details: Some(lang_api::CompletionLabelDetails {
                            detail: None,
                            description: Some(format!(
                                "{locales} {}",
                                if locales == 1 { "locale" } else { "locales" }
                            )),
                        }),
                        ..CompletionEntry::default()
                    });
                }
            }
        }
        entries.sort_by(|left, right| left.label.cmp(&right.label));
        Some(CompletionList {
            entries,
            complete: true,
        })
    }

    /// The values of a key in every locale, as Markdown.
    fn hover(&self, kind: FileKind, text: &str, offset: usize) -> Option<String> {
        let context = match kind {
            FileKind::Java => java_key_at(text, offset),
            FileKind::Template => template_key_at(text, offset),
            FileKind::Properties => property_at(text, offset),
        }?;
        let values = self.values_of(&context.key);
        if values.is_empty() {
            return None;
        }
        let mut out = format!("**{}**\n\n", context.key);
        let mut seen = HashSet::new();
        for (_, locale, value) in values {
            let locale = locale.unwrap_or_else(|| "default".to_owned());
            if !seen.insert(locale.clone()) {
                continue;
            }
            out.push_str(&format!("- {locale}: {value}\n"));
        }
        Some(out)
    }

    /// The places of the key at the caret.
    fn navigation(&self, kind: FileKind, text: &str, offset: usize) -> Option<Vec<SourceSpan>> {
        let context = match kind {
            FileKind::Java => java_key_at(text, offset),
            FileKind::Template => template_key_at(text, offset),
            FileKind::Properties => property_at(text, offset),
        }?;
        let mut spans = Vec::new();
        for bundle in &self.keys().bundles {
            if !bundle.has_key(&context.key) {
                continue;
            }
            let file = bundle
                .base_file()
                .filter(|file| file.has_key(&context.key))
                .or_else(|| bundle.files.iter().find(|file| file.has_key(&context.key)));
            let Some(file) = file else {
                continue;
            };
            let Some(line) = file.keys.iter().find(|line| line.key == context.key) else {
                continue;
            };
            let Some(uri) = uri_of(&file.path) else {
                continue;
            };
            spans.push(SourceSpan {
                uri,
                span: line.span,
            });
        }
        Some(spans)
    }

    /// The checks of one file: an unknown key, a key without a use and a key a locale
    /// misses.
    fn diagnostics_for(&self, path: &Path, text: &str) -> Vec<DiagnosticEntry> {
        let Some(kind) = file_kind(path) else {
            return Vec::new();
        };
        match kind {
            FileKind::Java | FileKind::Template => {
                if !self.check_enabled("messages.unknownKey") {
                    return Vec::new();
                }
                unknown_keys(text, kind, &self.keys())
            }
            FileKind::Properties => {
                let unused = self.check_enabled("messages.unusedProperty");
                let missing = self.check_enabled("messages.missingTranslation");
                if !unused && !missing {
                    // The walk of the sources below is the costly part of this check.
                    return Vec::new();
                }
                let Some((index, bundle_at, _file_at)) = self.bundle_file(path) else {
                    return Vec::new();
                };
                let bundle = &index.bundles[bundle_at];
                let sources = self.sources();
                let used = sources
                    .usages
                    .iter()
                    .any(|usage| bundle.has_key(&usage.key));
                let mut entries = Vec::new();
                for line in &properties_keys(text) {
                    if unused && used && !sources.usages.iter().any(|usage| usage.key == line.key) {
                        entries.push(DiagnosticEntry {
                            span: line.span,
                            severity: Severity::Hint,
                            message: "Unused property".to_owned(),
                            unnecessary: true,
                            code: Some("messages.unusedProperty".to_owned()),
                        });
                    }
                    let missing_locales = bundle.missing_locales(&line.key);
                    if missing && !missing_locales.is_empty() {
                        entries.push(DiagnosticEntry {
                            span: line.span,
                            severity: Severity::Warning,
                            message: format!(
                                "Missing translation for: {}",
                                missing_locales.join(", ")
                            ),
                            unnecessary: false,
                            code: Some("messages.missingTranslation".to_owned()),
                        });
                    }
                }
                entries
            }
        }
    }

    /// The quick fixes of the caret: a key that no bundle knows becomes a new line of
    /// the base file.
    fn actions_for(&self, path: &Path, text: &str, offset: usize) -> Vec<ActionEntry> {
        let Some(kind) = file_kind(path) else {
            return Vec::new();
        };
        if kind == FileKind::Properties {
            return Vec::new();
        }
        let Some(context) = (match kind {
            FileKind::Java => java_key_at(text, offset),
            FileKind::Template => template_key_at(text, offset),
            FileKind::Properties => None,
        }) else {
            return Vec::new();
        };
        if context.key.is_empty() {
            return Vec::new();
        }
        let keys = self.keys();
        if keys.bundles.is_empty()
            || keys
                .bundles
                .iter()
                .any(|bundle| bundle.has_key(&context.key))
        {
            return Vec::new();
        }
        let Some((_, file, base_text)) = self.base_bundle() else {
            return Vec::new();
        };
        let Some(uri) = uri_of(&file) else {
            return Vec::new();
        };
        let mut insert = String::new();
        if !base_text.is_empty() && !base_text.ends_with('\n') {
            insert.push('\n');
        }
        insert.push_str(&context.key);
        insert.push_str("=\n");
        let end = point_in(&base_text, base_text.len());
        let edits = vec![DocumentEdit {
            span: TextSpan { start: end, end },
            text: insert,
        }];
        vec![ActionEntry {
            title: format!("Create property '{}'", context.key),
            kind: Some(ActionKind::QuickFix),
            code: Some("messages.create-property".to_owned()),
            preferred: true,
            edits: vec![SourceEdit { uri, edits }],
            renames: Vec::new(),
            handle: None,
        }]
    }

    fn usages_for(&self, path: &Path, text: &str, offset: usize) -> Vec<UsageEntry> {
        let Some(context) = self.key_at(path, text, offset) else {
            return Vec::new();
        };
        if file_kind(path) != Some(FileKind::Properties) {
            return Vec::new();
        }
        self.sources()
            .usages
            .iter()
            .filter(|usage| usage.key == context.key)
            .map(|usage| UsageEntry {
                target: SourceSpan {
                    uri: usage.uri.clone(),
                    span: usage.span,
                },
                kind: UsageKind::Read,
                container: usage
                    .path
                    .file_stem()
                    .map(|stem| stem.to_string_lossy().into_owned()),
            })
            .collect()
    }

    /// Every file and usage of the key gets the new name.
    fn rename_for(
        &self,
        path: &Path,
        text: &str,
        offset: usize,
        new_name: &str,
    ) -> Option<WorkspaceChange> {
        if file_kind(path) != Some(FileKind::Properties) {
            return None;
        }
        let context = self.key_at(path, text, offset)?;
        if new_name.is_empty() || new_name.chars().any(char::is_whitespace) {
            return None;
        }
        let mut by_uri: Vec<(String, Vec<DocumentEdit>)> = Vec::new();
        let mut add = |uri: String, span: TextSpan| {
            let edit = DocumentEdit {
                span,
                text: new_name.to_owned(),
            };
            match by_uri.iter_mut().find(|(known, _)| *known == uri) {
                Some((_, edits)) => edits.push(edit),
                None => by_uri.push((uri, vec![edit])),
            }
        };
        for bundle in &self.keys().bundles {
            if !bundle.has_key(&context.key) {
                continue;
            }
            for file in &bundle.files {
                let Some(uri) = uri_of(&file.path) else {
                    continue;
                };
                for line in file.keys.iter().filter(|line| line.key == context.key) {
                    add(uri.clone(), line.span);
                }
            }
        }
        for usage in self
            .sources()
            .usages
            .iter()
            .filter(|u| u.key == context.key)
        {
            add(usage.uri.clone(), usage.span);
        }
        if by_uri.is_empty() {
            return None;
        }
        let edits = by_uri
            .into_iter()
            .map(|(uri, mut edits)| {
                edits.sort_by_key(|edit| (edit.span.start.line, edit.span.start.character));
                SourceEdit { uri, edits }
            })
            .collect();
        Some(WorkspaceChange {
            edits,
            renames: Vec::new(),
        })
    }
}

struct Inner {
    root: PathBuf,
    roots: Mutex<Vec<PathBuf>>,
    documents: Mutex<HashMap<String, Arc<str>>>,
    /// The parsed files, one entry per path.
    state: Mutex<IndexState>,
    /// Serializes the reads of the sources, so that two threads do not walk the
    /// project at once. The interface never takes it.
    sources_lock: Mutex<()>,
    keys: RwLock<Option<CachedKeys>>,
    sources: RwLock<Option<CachedSources>>,
    dirty_keys: AtomicBool,
    dirty_sources: AtomicBool,
    /// The buffers that changed since the last refresh of the sources.
    dirty_paths: Mutex<HashSet<PathBuf>>,
    /// The queue of the worker that reads and parses for the providers.
    worker: Mutex<Option<mpsc::Sender<Job>>>,
    /// The answers of the open documents, which get a new one when the text changes.
    checks: Mutex<HashMap<String, Reply<Vec<DiagnosticEntry>>>>,
    /// The files whose checks wait for a quiet moment.
    checks_queue: Arc<CheckQueue>,
    checks_started: AtomicBool,
    /// The checks the user turned off: one that is off is not computed at all.
    inspection_checks: Mutex<lang_api::DisabledInspections>,
}

type Job = Box<dyn FnOnce(&MessageBundles) + Send + 'static>;

/// The per-file cache of the keys, the settings and the usages of a project.
#[derive(Default)]
struct IndexState {
    properties: HashMap<PathBuf, CachedKeysFile>,
    settings: HashMap<PathBuf, CachedBases>,
    sources: HashMap<PathBuf, CachedSource>,
    /// The files of the last walk of the project.
    source_files: Vec<PathBuf>,
    /// When the last walk happened.
    walked: Option<Instant>,
}

/// Where the text of a file comes from.
#[derive(Clone)]
enum FileOrigin {
    Buffer(Arc<str>),
    Disk {
        modified: Option<SystemTime>,
        size: u64,
    },
}

impl FileOrigin {
    /// Whether the text behind this origin is the text of `other`. A buffer is the same
    /// text only when it is the same buffer; a file on disk when its time and size agree.
    fn matches(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Buffer(left), Self::Buffer(right)) => Arc::ptr_eq(left, right),
            (
                Self::Disk {
                    modified: left_time,
                    size: left_size,
                },
                Self::Disk {
                    modified: right_time,
                    size: right_size,
                },
            ) => left_time == right_time && left_size == right_size,
            _ => false,
        }
    }
}

#[derive(Clone)]
struct CachedKeysFile {
    origin: FileOrigin,
    keys: Arc<Vec<KeyLine>>,
}

#[derive(Clone)]
struct CachedBases {
    origin: FileOrigin,
    bases: Vec<String>,
}

#[derive(Clone)]
struct CachedSource {
    origin: FileOrigin,
    usages: Arc<Vec<Usage>>,
    bases: Arc<Vec<String>>,
}

/// The file changes that wait for the checks of the open documents.
struct CheckQueue {
    state: Mutex<CheckState>,
    ready: Condvar,
}

struct CheckState {
    /// Every request, oldest first. A file can be listed more than once.
    order: VecDeque<String>,
    /// Grows with every request of a file.
    generation: HashMap<String, u64>,
    /// Grows with every request of any file.
    revision: u64,
    closed: bool,
}

impl CheckQueue {
    fn new() -> Self {
        Self {
            state: Mutex::new(CheckState {
                order: VecDeque::new(),
                generation: HashMap::new(),
                revision: 0,
                closed: false,
            }),
            ready: Condvar::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, CheckState> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
}

impl Inner {
    fn new(root: PathBuf) -> Self {
        Self {
            root,
            roots: Mutex::new(Vec::new()),
            documents: Mutex::new(HashMap::new()),
            state: Mutex::new(IndexState::default()),
            sources_lock: Mutex::new(()),
            keys: RwLock::new(None),
            sources: RwLock::new(None),
            dirty_keys: AtomicBool::new(false),
            dirty_sources: AtomicBool::new(false),
            dirty_paths: Mutex::new(HashSet::new()),
            worker: Mutex::new(None),
            checks: Mutex::new(HashMap::new()),
            checks_queue: Arc::new(CheckQueue::new()),
            checks_started: AtomicBool::new(false),
            inspection_checks: Mutex::new(lang_api::DisabledInspections::default()),
        }
    }

    fn invalidate_keys(&self) {
        self.dirty_keys.store(true, Ordering::Relaxed);
    }

    fn invalidate_sources(&self) {
        self.dirty_sources.store(true, Ordering::Relaxed);
    }

    fn mark_source(&self, path: PathBuf) {
        self.dirty_paths
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .insert(path);
        self.invalidate_sources();
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        let mut queue = self.checks_queue.lock();
        queue.closed = true;
        drop(queue);
        self.checks_queue.ready.notify_all();
    }
}

impl MessageBundles {
    /// Runs `job` on the worker that reads and parses for the providers, starting it on
    /// the first call. The caller never waits for the job.
    fn submit(&self, job: Job) {
        let mut worker = self
            .inner
            .worker
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if worker.is_none() {
            let (sender, receiver) = mpsc::channel::<Job>();
            let weak = Arc::downgrade(&self.inner);
            let spawned = thread::Builder::new()
                .name("message-bundles".to_owned())
                .spawn(move || {
                    while let Ok(job) = receiver.recv() {
                        let Some(inner) = weak.upgrade() else {
                            break;
                        };
                        job(&MessageBundles { inner });
                    }
                });
            if spawned.is_err() {
                return;
            }
            *worker = Some(sender);
        }
        if let Some(sender) = worker.as_ref() {
            let _ = sender.send(job);
        }
    }

    fn start_checks(&self) {
        if self.inner.checks_started.swap(true, Ordering::SeqCst) {
            return;
        }
        let queue = self.inner.checks_queue.clone();
        let weak = Arc::downgrade(&self.inner);
        let spawned = thread::Builder::new()
            .name("message-bundle-checks".to_owned())
            .spawn(move || check_loop(queue, weak));
        if spawned.is_err() {
            self.inner.checks_started.store(false, Ordering::SeqCst);
        }
    }

    /// Asks for the checks of `uri`, once the edits of the project go quiet.
    fn request_check(&self, uri: String) {
        self.start_checks();
        let queue = &self.inner.checks_queue;
        let mut state = queue.lock();
        let generation = state.generation.entry(uri.clone()).or_insert(0);
        *generation = generation.wrapping_add(1);
        state.order.push_back(uri);
        state.revision = state.revision.wrapping_add(1);
        drop(state);
        queue.ready.notify_all();
    }

    fn run_check(&self, uri: &str, generation: u64) {
        let Some(path) = path_of_uri(uri) else {
            return;
        };
        let Some(text) = self.text_of(uri) else {
            return;
        };
        let entries = self.diagnostics_for(&path, &text);
        // A newer edit came in while the checks ran: its own request sends the answer.
        let current = self.inner.checks_queue.lock().generation.get(uri).copied();
        if current != Some(generation) {
            return;
        }
        let mut checks = self
            .inner
            .checks
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let Some(reply) = checks.get_mut(uri) else {
            return;
        };
        if reply
            .send(Answer::new(entries, Confidence::Certain, SOURCE))
            .is_err()
        {
            checks.remove(uri);
        }
    }
}

/// Checks the files of the queue after the edits go quiet. Fewer than `CHECK_DELAY`
/// apart, a burst of keys checks once.
fn check_loop(queue: Arc<CheckQueue>, weak: Weak<Inner>) {
    let mut state = queue.lock();
    loop {
        while state.order.is_empty() {
            if state.closed {
                return;
            }
            state = queue
                .ready
                .wait(state)
                .unwrap_or_else(|poison| poison.into_inner());
        }
        let seen = state.revision;
        let (next, _) = queue
            .ready
            .wait_timeout(state, CHECK_DELAY)
            .unwrap_or_else(|poison| poison.into_inner());
        state = next;
        if state.closed {
            return;
        }
        if state.revision != seen {
            // An edit arrived during the pause: wait for the next quiet moment.
            continue;
        }
        let drained: Vec<String> = state.order.drain(..).collect();
        let mut batch: HashMap<String, u64> = HashMap::new();
        for uri in drained {
            if let Some(generation) = state.generation.get(&uri) {
                batch.insert(uri, *generation);
            }
        }
        drop(state);
        let Some(inner) = weak.upgrade() else {
            return;
        };
        let support = MessageBundles { inner };
        for (uri, generation) in batch {
            support.run_check(&uri, generation);
        }
        drop(support);
        state = queue.lock();
    }
}

#[derive(Clone)]
struct CachedKeys {
    at: Instant,
    index: Arc<KeyIndex>,
}

#[derive(Clone)]
struct CachedSources {
    at: Instant,
    index: Arc<Sources>,
}

/// The bundles of a project, with the keys of every file.
struct KeyIndex {
    bundles: Vec<Bundle>,
}

struct Bundle {
    base: String,
    base_file: Option<usize>,
    files: Vec<BundleFile>,
}

impl Bundle {
    fn base_file(&self) -> Option<&BundleFile> {
        self.base_file.map(|at| &self.files[at])
    }

    fn has_key(&self, key: &str) -> bool {
        self.files.iter().any(|file| file.has_key(key))
    }

    /// The locales of the bundle that do not have `key`. A locale file without a single
    /// key inherits the base file, as an empty `messages_en.properties` does in a
    /// project whose default locale is English.
    fn missing_locales(&self, key: &str) -> Vec<String> {
        let mut missing: Vec<String> = self
            .files
            .iter()
            .filter(|file| file.locale.is_some() && !file.keys.is_empty() && !file.has_key(key))
            .map(|file| file.locale.clone().unwrap_or_default())
            .collect();
        missing.sort();
        missing
    }
}

struct BundleFile {
    path: PathBuf,
    locale: Option<String>,
    keys: Arc<Vec<KeyLine>>,
}

impl BundleFile {
    fn has_key(&self, key: &str) -> bool {
        self.keys.iter().any(|line| line.key == key)
    }
}

/// One `key=value` line of a properties file.
#[derive(Clone, Debug)]
struct KeyLine {
    key: String,
    value: String,
    span: TextSpan,
}

/// The usages of the sources and the base names of `getBundle`.
struct Sources {
    usages: Vec<Usage>,
    /// The bases of `getBundle`. The bundles read the per-file cache; the field stays for
    /// a reader of the snapshot, the tests among them.
    #[cfg_attr(not(test), allow(dead_code))]
    bases: Vec<String>,
}

#[derive(Clone, Debug)]
struct Usage {
    key: String,
    path: PathBuf,
    uri: String,
    span: TextSpan,
}

/// A key at the caret: its text and where it starts.
#[derive(Clone, Debug)]
pub(crate) struct KeyContext {
    pub key: String,
    pub span: Range<usize>,
}

fn uri_of(path: &Path) -> Option<String> {
    Some(CanonPath::new(path).ok()?.to_uri())
}

fn path_of_uri(uri: &str) -> Option<PathBuf> {
    file_path(&Uri::from_str(uri).ok()?)
}

fn dedupe(roots: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    roots
        .into_iter()
        .filter(|root| seen.insert(path_key(root)))
        .collect()
}

fn dedupe_names(names: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    names
        .into_iter()
        .filter(|name| seen.insert(name.clone()))
        .collect()
}

/// The names of a `spring.messages.basename` value.
fn normalize_bases(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(|base| {
            base.trim()
                .trim_start_matches("classpath:")
                .trim_start_matches('/')
                .replace('\\', "/")
        })
        .filter(|base| !base.is_empty())
        .collect()
}

/// The value of `key` in a properties text, without a comment.
fn properties_setting(text: &str, key: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.starts_with('!') {
            continue;
        }
        let Some((name, value)) = split_property(line) else {
            continue;
        };
        if name == key {
            return Some(value.to_owned());
        }
    }
    None
}

/// The value of a dotted setting of a YAML document, read line by line: the levels are
/// the keys above the line, and only the last level names the setting.
fn yaml_setting(text: &str, key: &str) -> Option<String> {
    let wanted: Vec<&str> = key.split('.').collect();
    let mut levels: Vec<(usize, String)> = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let indent = line.len() - trimmed.len();
        let Some((name, value)) = trimmed.split_once(':') else {
            continue;
        };
        let name = name.trim().trim_matches(['"', '\'']);
        while levels.last().is_some_and(|(level, _)| *level >= indent) {
            levels.pop();
        }
        if value.trim().is_empty() {
            levels.push((indent, name.to_owned()));
            continue;
        }
        let mut path: Vec<&str> = levels.iter().map(|(_, name)| name.as_str()).collect();
        path.push(name);
        if path == wanted {
            return Some(value.trim().trim_matches(['"', '\'']).to_owned());
        }
    }
    None
}

/// A properties line split into its key and its value, after the leading spaces.
fn split_property(line: &str) -> Option<(&str, &str)> {
    let start = line.len() - line.trim_start().len();
    let body = &line[start..];
    if body.is_empty() || body.starts_with('#') || body.starts_with('!') {
        return None;
    }
    let mut escaped = false;
    let mut key_end = body.len();
    for (at, character) in body.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match character {
            '\\' => escaped = true,
            '=' | ':' => {
                key_end = at;
                break;
            }
            character if character.is_whitespace() => {
                key_end = at;
                break;
            }
            _ => {}
        }
    }
    let key = &body[..key_end];
    if key.is_empty() {
        return None;
    }
    let mut rest = &body[key_end..];
    rest = rest.trim_start();
    if let Some(stripped) = rest.strip_prefix('=').or_else(|| rest.strip_prefix(':')) {
        rest = stripped.trim_start();
    }
    Some((key, rest.trim_end()))
}

/// The keys of a properties file, with their places.
fn properties_keys(text: &str) -> Vec<KeyLine> {
    let mut keys = Vec::new();
    let mut line_number = 0;
    for line in text.split_inclusive('\n') {
        let raw = line.trim_end_matches(['\r', '\n']);
        let Some((key, value)) = split_property(raw) else {
            line_number += 1;
            continue;
        };
        let leading = raw.len() - raw.trim_start().len();
        let character = raw[..leading].encode_utf16().count() as u32;
        let finish = character + key.encode_utf16().count() as u32;
        keys.push(KeyLine {
            key: key.to_owned(),
            value: value.to_owned(),
            span: TextSpan {
                start: TextPoint {
                    line: line_number,
                    character,
                },
                end: TextPoint {
                    line: line_number,
                    character: finish,
                },
            },
        });
        line_number += 1;
    }
    keys
}

/// A key at a caret of a properties file.
fn property_at(text: &str, offset: usize) -> Option<KeyContext> {
    let offset = offset.min(text.len());
    for line in properties_keys(text) {
        let start = byte_of(text, line.span.start);
        let end = start + line.key.len();
        if start <= offset && offset <= end {
            return Some(KeyContext {
                key: line.key,
                span: start..end,
            });
        }
    }
    None
}

/// A string literal of a Java source: its place with the quotes and what it is.
struct JavaLiteral {
    quoted: Range<usize>,
    content: Range<usize>,
    owner: Owner,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Owner {
    /// A key, written on its own or inside braces.
    Key { braced: bool },
    /// The base name of a bundle.
    Base,
}

#[cfg(test)]
thread_local! {
    static JAVA_PARSES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn java_parses() -> usize {
    JAVA_PARSES.with(std::cell::Cell::get)
}

#[cfg(test)]
fn reset_java_parses() {
    JAVA_PARSES.with(|parses| parses.set(0));
}

fn parse_java(text: &str) -> Option<tree_sitter::Tree> {
    #[cfg(test)]
    JAVA_PARSES.with(|parses| parses.set(parses.get() + 1));
    let mut parser = tree_sitter::Parser::new();
    let grammar = java_syntax::config().grammar;
    if parser.set_language(&grammar).is_err() {
        return None;
    }
    parser.parse(text, None)
}

fn java_literals(text: &str) -> Vec<JavaLiteral> {
    let Some(tree) = parse_java(text) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    collect_literals(tree.root_node(), text, &mut found);
    found
}

fn collect_literals(node: tree_sitter::Node, text: &str, out: &mut Vec<JavaLiteral>) {
    if node.kind() == "string_literal"
        && let Some(literal) = literal_of(node, text)
    {
        out.push(literal);
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_literals(child, text, out);
    }
}

fn literal_of(node: tree_sitter::Node, text: &str) -> Option<JavaLiteral> {
    let quoted = node.byte_range();
    if quoted.len() < 2 || !text[quoted.clone()].starts_with('"') {
        return None;
    }
    let content = (quoted.start + 1)..(quoted.end - 1);
    let parent = node.parent()?;
    if parent.kind() == "argument_list" {
        if parent.named_child(0).map(|first| first.id()) != Some(node.id()) {
            return None;
        }
        let call = parent.parent()?;
        if call.kind() != "method_invocation" {
            return None;
        }
        let name = call.child_by_field_name("name")?;
        let name = &text[name.byte_range()];
        if name == "getBundle" {
            return Some(JavaLiteral {
                quoted,
                content,
                owner: Owner::Base,
            });
        }
        if KEY_METHODS.contains(&name) {
            return Some(JavaLiteral {
                quoted,
                content,
                owner: Owner::Key { braced: false },
            });
        }
        return None;
    }
    if parent.kind() == "element_value_pair"
        && parent.child_by_field_name("value").map(|value| value.id()) == Some(node.id())
    {
        let key = parent.child_by_field_name("key")?;
        if &text[key.byte_range()] == MESSAGE_MEMBER {
            return Some(JavaLiteral {
                quoted,
                content,
                owner: Owner::Key { braced: true },
            });
        }
    }
    None
}

/// The key of a literal and the place of the key inside the literal. An empty body is
/// the start of a key only when the caller is a completion that has just opened the
/// literal.
fn key_of_literal(
    text: &str,
    content: &Range<usize>,
    braced: bool,
) -> Option<(String, Range<usize>)> {
    let raw = &text[content.clone()];
    let (start, body) = if braced {
        (content.start + 1, raw.strip_prefix('{')?)
    } else {
        (content.start, raw)
    };
    let end = body.find(['}', '(', ',']).unwrap_or(body.len());
    let key = body[..end].trim_end();
    if key.is_empty() {
        return None;
    }
    Some((key.to_owned(), start..(start + key.len())))
}

/// The key at a caret, which may be empty inside a literal that is just being typed.
fn key_at_caret(text: &str, content: &Range<usize>, braced: bool) -> Option<KeyContext> {
    let raw = &text[content.clone()];
    let (start, body) = if braced {
        (content.start + 1, raw.strip_prefix('{')?)
    } else {
        (content.start, raw)
    };
    let end = body.find(['}', '(', ',']).unwrap_or(body.len());
    let key = body[..end].trim_end();
    Some(KeyContext {
        key: key.to_owned(),
        span: start..(start + key.len()),
    })
}

/// A key at a caret of a Java source. A tree that did not parse the text, which a
/// half-typed literal often does, gets a textual look for the literal around the caret.
fn java_key_at(text: &str, offset: usize) -> Option<KeyContext> {
    let offset = offset.min(text.len());
    for literal in java_literals(text) {
        if literal.quoted.start > offset || offset > literal.quoted.end {
            continue;
        }
        return match literal.owner {
            Owner::Key { braced } => key_at_caret(text, &literal.content, braced),
            Owner::Base => None,
        };
    }
    let quoted = java_literal_around(text, offset)?;
    let owner = java_owner_before(text, quoted.start)?;
    let braced = match owner {
        Owner::Key { braced } => braced,
        Owner::Base => return None,
    };
    let content = (quoted.start + 1)..quoted.end;
    key_at_caret(text, &content, braced)
}

/// The quoted range of the string literal the caret is in, for a text that did not
/// parse. An unterminated literal ends at the end of its line.
fn java_literal_around(text: &str, offset: usize) -> Option<Range<usize>> {
    let bytes = text.as_bytes();
    let mut open = None;
    let mut at = offset.min(bytes.len());
    while at > 0 {
        at -= 1;
        match bytes[at] {
            b'\n' | b'\r' => break,
            b'"' if !escaped_at(bytes, at) => {
                open = Some(at);
                break;
            }
            _ => {}
        }
    }
    let open = open?;
    let mut close = None;
    let mut at = open + 1;
    while at < bytes.len() {
        match bytes[at] {
            b'\n' | b'\r' => break,
            b'"' if !escaped_at(bytes, at) => {
                close = Some(at);
                break;
            }
            _ => {}
        }
        at += 1;
    }
    let close = close.unwrap_or(at);
    if offset > close {
        return None;
    }
    Some(open..close)
}

fn escaped_at(bytes: &[u8], at: usize) -> bool {
    let mut backslashes = 0;
    let mut index = at;
    while index > 0 && bytes[index - 1] == b'\\' {
        backslashes += 1;
        index -= 1;
    }
    backslashes % 2 == 1
}

fn java_owner_before(text: &str, quote: usize) -> Option<Owner> {
    let before = text[..quote].trim_end();
    if let Some(head) = before.strip_suffix('(') {
        let head = head.trim_end();
        let start = head
            .rfind(|character: char| {
                !(character.is_alphanumeric() || character == '_' || character == '$')
            })
            .map_or(0, |at| at + 1);
        let name = &head[start..];
        if name == "getBundle" {
            return Some(Owner::Base);
        }
        if KEY_METHODS.contains(&name) {
            return Some(Owner::Key { braced: false });
        }
        return None;
    }
    let head = before.strip_suffix('=')?.trim_end();
    let before_member = head.strip_suffix(MESSAGE_MEMBER)?;
    if before_member
        .chars()
        .next_back()
        .is_some_and(|character| character.is_alphanumeric() || character == '_')
    {
        return None;
    }
    let at = before_member.rfind('@')?;
    let between = &before_member[at..];
    if between.contains(')') || between.contains(';') {
        return None;
    }
    Some(Owner::Key { braced: true })
}

/// The key at a caret of a template. A key lives in `#{…}` of a `th:*` attribute.
fn template_key_at(text: &str, offset: usize) -> Option<KeyContext> {
    let offset = offset.min(text.len());
    let start = text[..offset].rfind("#{")?;
    let after = start + 2;
    if offset < after {
        return None;
    }
    if text[after..offset].contains('}') || text[after..offset].contains('"') {
        return None;
    }
    let attribute = template_attribute(text, start)?;
    if !attribute.0.starts_with("th:") {
        return None;
    }
    let end = text[offset..]
        .find(['}', '(', ',', '"'])
        .map_or(offset, |at| offset + at);
    let key = text[after..end].trim_end();
    if key.is_empty() && offset > after {
        return None;
    }
    Some(KeyContext {
        key: key.to_owned(),
        span: after..(after + key.len()),
    })
}

/// The name of the attribute whose quoted value holds `at`, if `at` is in such a value.
fn template_attribute(text: &str, at: usize) -> Option<(String, usize, usize)> {
    let before = &text[..at];
    let equals = before.rfind('=')?;
    let between = &before[equals + 1..];
    let quote = match between.chars().next()? {
        quote @ ('"' | '\'') => quote,
        _ => return None,
    };
    if !between[1..].trim().is_empty() {
        return None;
    }
    let name_end = before[..equals].trim_end().len();
    let name_start = before[..name_end]
        .rfind(|character: char| {
            !(character.is_alphanumeric() || matches!(character, ':' | '_' | '-' | '.' | '@' | '$'))
        })
        .map_or(0, |found| found + 1);
    let name = &before[name_start..name_end];
    if name.is_empty() {
        return None;
    }
    let close = text[at + 1..]
        .find(quote)
        .map_or(text.len(), |found| at + 1 + found);
    Some((name.to_owned(), at + 2, close))
}

/// Every key a template uses and the place of the key.
fn template_keys(text: &str) -> Vec<(String, Range<usize>)> {
    let mut keys = Vec::new();
    let mut at = 0;
    while let Some(found) = text[at..].find("#{") {
        let start = at + found;
        at = start + 2;
        let Some((name, content_start, close)) = template_attribute(text, start) else {
            continue;
        };
        if !name.starts_with("th:") {
            continue;
        }
        let end = text[start + 2..close]
            .find(['}', '(', ','])
            .map_or(close, |found| start + 2 + found);
        let key = text[content_start..end].trim();
        if key.is_empty() {
            continue;
        }
        keys.push((key.to_owned(), content_start..(content_start + key.len())));
    }
    keys
}

/// Every key a template uses, as a usage of the sources.
fn template_usages(path: &Path, text: &str) -> Vec<Usage> {
    let Some(uri) = uri_of(path) else {
        return Vec::new();
    };
    template_keys(text)
        .into_iter()
        .map(|(key, span)| Usage {
            span: span_in(text, span.start, span.end),
            key,
            path: path.to_path_buf(),
            uri: uri.clone(),
        })
        .collect()
}

/// The checks of a Java source or a template: a key that no bundle knows.
fn unknown_keys(text: &str, kind: FileKind, keys: &KeyIndex) -> Vec<DiagnosticEntry> {
    if keys.bundles.is_empty() {
        return Vec::new();
    }
    let mut found = Vec::new();
    let contexts: Vec<(String, Range<usize>)> = match kind {
        FileKind::Java => java_literals(text)
            .iter()
            .filter_map(|literal| match literal.owner {
                Owner::Key { braced } => key_of_literal(text, &literal.content, braced),
                Owner::Base => None,
            })
            .collect(),
        FileKind::Template => template_keys(text),
        FileKind::Properties => Vec::new(),
    };
    for (key, span) in contexts {
        if keys.bundles.iter().any(|bundle| bundle.has_key(&key)) {
            continue;
        }
        found.push(DiagnosticEntry {
            span: span_in(text, span.start, span.end),
            severity: Severity::Warning,
            message: "Cannot resolve property key".to_owned(),
            unnecessary: false,
            code: Some("messages.unknownKey".to_owned()),
        });
    }
    found
}

/// The usages and the `getBundle` bases of one source or template.
fn source_entry(path: &Path, text: &str, origin: FileOrigin) -> CachedSource {
    let mut usages = Vec::new();
    let mut bases = Vec::new();
    match file_kind(path) {
        Some(FileKind::Java) => {
            for literal in java_literals(text) {
                match literal.owner {
                    Owner::Base => {
                        bases.push(text[literal.quoted.clone()].trim_matches('"').to_owned());
                    }
                    Owner::Key { braced } => {
                        if let Some((key, span)) = key_of_literal(text, &literal.content, braced) {
                            usages.push(usage(path.to_path_buf(), key, span, text));
                        }
                    }
                }
            }
        }
        Some(FileKind::Template) => usages.extend(template_usages(path, text)),
        _ => {}
    }
    CachedSource {
        origin,
        usages: Arc::new(usages),
        bases: Arc::new(bases),
    }
}

fn usage(path: PathBuf, key: String, span: Range<usize>, text: &str) -> Usage {
    Usage {
        key,
        uri: uri_of(&path).unwrap_or_default(),
        path,
        span: span_in(text, span.start, span.end),
    }
}

fn span_in(text: &str, start: usize, end: usize) -> TextSpan {
    TextSpan {
        start: point_in(text, start),
        end: point_in(text, end),
    }
}

fn point_in(text: &str, offset: usize) -> TextPoint {
    let offset = offset.min(text.len());
    let mut line = 0;
    let mut line_start = 0;
    for (at, byte) in text.bytes().enumerate() {
        if at >= offset {
            break;
        }
        if byte == b'\n' {
            line += 1;
            line_start = at + 1;
        }
    }
    TextPoint {
        line,
        character: text[line_start..offset].encode_utf16().count() as u32,
    }
}

fn byte_of(text: &str, point: TextPoint) -> usize {
    offset_in(text, point)
}

/// Every source and template of a project folder.
fn collect_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if SKIP_FOLDERS
                .iter()
                .any(|skip| name.eq_ignore_ascii_case(skip))
                || name.starts_with('.')
            {
                continue;
            }
            collect_sources(&path, out);
        } else if matches!(file_kind(&path), Some(FileKind::Java | FileKind::Template)) {
            out.push(path);
        }
    }
}

impl SyntaxProvider for MessageBundles {
    fn grammar(&self) -> LanguageFn {
        unsafe { LanguageFn::from_raw(no_grammar) }
    }

    fn queries(&self) -> &Queries {
        &NO_QUERIES
    }
}

impl LanguageSupport for MessageBundles {
    fn id(&self) -> LanguageId {
        LANGUAGE_ID
    }

    fn matches(&self, path: &Path) -> bool {
        match file_kind(path) {
            Some(FileKind::Java | FileKind::Template) => true,
            Some(FileKind::Properties) => self.bundle_of(path).is_some(),
            None => false,
        }
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

    fn hover(&self) -> Option<&HoverProvider> {
        Some(self)
    }

    fn navigation(&self) -> Option<&NavigationProvider> {
        Some(self)
    }

    fn diagnostics(&self) -> Option<&DiagnosticsProvider> {
        Some(self)
    }

    fn actions(&self) -> Option<&ActionProvider> {
        Some(self)
    }

    fn usages(&self) -> Option<&UsageProvider> {
        Some(self)
    }

    fn rename(&self) -> Option<&RenameProvider> {
        Some(self)
    }

    fn bundle_files(&self, path: &Path) -> Option<Vec<lang_api::BundleFile>> {
        self.files_of(path)
    }
}

impl Provider<DocumentSpot, CompletionList> for MessageBundles {
    fn start(&self, query: DocumentSpot, cancel: Cancel, mut reply: Reply<CompletionList>) {
        self.submit(Box::new(move |support| {
            if cancel.is_cancelled() {
                return;
            }
            let _ = support.sources();
            let list = support.query_text(&query.uri).and_then(|(path, text)| {
                let kind = file_kind(&path)?;
                let offset = offset_in(&text, query.point);
                support.completion(kind, &text, offset)
            });
            let answer = match list {
                Some(list) => Answer::new(list, Confidence::Certain, SOURCE),
                None => Answer::new(CompletionList::default(), Confidence::Unknown, SOURCE),
            };
            let _ = reply.send(answer);
        }));
    }
}

impl Provider<DocumentSpot, HoverText> for MessageBundles {
    fn start(&self, query: DocumentSpot, cancel: Cancel, mut reply: Reply<HoverText>) {
        self.submit(Box::new(move |support| {
            if cancel.is_cancelled() {
                return;
            }
            let _ = support.sources();
            let text = support.query_text(&query.uri).and_then(|(path, text)| {
                let kind = file_kind(&path)?;
                let offset = offset_in(&text, query.point);
                support.hover(kind, &text, offset)
            });
            let answer = match text {
                Some(text) => Answer::new(HoverText(text), Confidence::Certain, SOURCE),
                None => Answer::new(HoverText::default(), Confidence::Unknown, SOURCE),
            };
            let _ = reply.send(answer);
        }));
    }
}

impl Provider<NavigationQuery, Vec<SourceSpan>> for MessageBundles {
    fn start(&self, query: NavigationQuery, cancel: Cancel, mut reply: Reply<Vec<SourceSpan>>) {
        self.submit(Box::new(move |support| {
            if cancel.is_cancelled() {
                return;
            }
            let _ = support.sources();
            let spans = support
                .query_text(&query.spot.uri)
                .and_then(|(path, text)| {
                    if query.kind != NavigationKind::Definition {
                        return None;
                    }
                    let kind = file_kind(&path)?;
                    let offset = offset_in(&text, query.spot.point);
                    support.navigation(kind, &text, offset)
                })
                .unwrap_or_default();
            let _ = reply.send(Answer::new(spans, Confidence::Certain, SOURCE));
        }));
    }
}

impl Provider<DiagnosticsQuery, Vec<DiagnosticEntry>> for MessageBundles {
    fn start(&self, query: DiagnosticsQuery, cancel: Cancel, reply: Reply<Vec<DiagnosticEntry>>) {
        if cancel.is_cancelled() {
            return;
        }
        self.inner
            .checks
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .insert(query.0.clone(), reply);
        self.request_check(query.0);
    }
}

impl Provider<ActionQuery, Vec<ActionEntry>> for MessageBundles {
    fn start(&self, query: ActionQuery, cancel: Cancel, mut reply: Reply<Vec<ActionEntry>>) {
        self.submit(Box::new(move |support| {
            if cancel.is_cancelled() {
                return;
            }
            let _ = support.sources();
            let entries = support
                .query_text(&query.uri)
                .map(|(path, text)| {
                    let offset = offset_in(&text, query.span.start);
                    support.actions_for(&path, &text, offset)
                })
                .unwrap_or_default();
            let _ = reply.send(Answer::new(entries, Confidence::Certain, SOURCE));
        }));
    }
}

impl Provider<DocumentSpot, Vec<UsageEntry>> for MessageBundles {
    fn start(&self, query: DocumentSpot, cancel: Cancel, mut reply: Reply<Vec<UsageEntry>>) {
        self.submit(Box::new(move |support| {
            if cancel.is_cancelled() {
                return;
            }
            let _ = support.sources();
            let entries = support
                .query_text(&query.uri)
                .map(|(path, text)| {
                    let offset = offset_in(&text, query.point);
                    support.usages_for(&path, &text, offset)
                })
                .unwrap_or_default();
            let _ = reply.send(Answer::new(entries, Confidence::Certain, SOURCE));
        }));
    }
}

impl Provider<RenameQuery, WorkspaceChange> for MessageBundles {
    fn start(&self, query: RenameQuery, cancel: Cancel, mut reply: Reply<WorkspaceChange>) {
        self.submit(Box::new(move |support| {
            if cancel.is_cancelled() {
                return;
            }
            let _ = support.sources();
            let change = support
                .query_text(&query.spot.uri)
                .and_then(|(path, text)| {
                    let offset = offset_in(&text, query.spot.point);
                    support.rename_for(&path, &text, offset, &query.new_name)
                });
            let answer = match change {
                Some(change) => Answer::new(change, Confidence::Certain, SOURCE),
                None => Answer::new(WorkspaceChange::default(), Confidence::Unknown, SOURCE),
            };
            let _ = reply.send(answer);
        }));
    }
}

impl SemanticServer for MessageBundles {
    fn open(&self, uri: &str, _version: i32, text: Arc<str>) {
        let kind = uri_file_kind(uri);
        self.inner
            .documents
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .insert(uri.to_owned(), text);
        match kind {
            Some(FileKind::Properties) => self.inner.invalidate_keys(),
            Some(FileKind::Java | FileKind::Template) => {
                if let Some(path) = path_of_uri(uri) {
                    self.inner.mark_source(path.clone());
                    self.submit(Box::new(move |support| support.refresh_source(path)));
                }
            }
            None => {}
        }
    }

    fn change(&self, uri: &str, _version: i32, text: Arc<str>, _edits: Vec<DocumentEdit>) {
        let kind = uri_file_kind(uri);
        self.inner
            .documents
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .insert(uri.to_owned(), text);
        match kind {
            Some(FileKind::Properties) => self.inner.invalidate_keys(),
            Some(FileKind::Java | FileKind::Template) => {
                if let Some(path) = path_of_uri(uri) {
                    self.inner.mark_source(path);
                }
            }
            None => {}
        }
        self.request_check(uri.to_owned());
    }

    fn save(&self, uri: &str) {
        if matches!(
            uri_file_kind(uri),
            Some(FileKind::Java | FileKind::Template)
        ) && let Some(path) = path_of_uri(uri)
        {
            self.submit(Box::new(move |support| support.refresh_source(path)));
        }
    }

    fn close(&self, uri: &str) {
        self.inner
            .documents
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(uri);
        self.inner
            .checks
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(uri);
        match uri_file_kind(uri) {
            Some(FileKind::Properties) => self.inner.invalidate_keys(),
            Some(FileKind::Java | FileKind::Template) => {
                if let Some(path) = path_of_uri(uri) {
                    self.inner.mark_source(path);
                }
            }
            None => {}
        }
    }

    fn version(&self, uri: &str) -> Option<i32> {
        self.inner
            .documents
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .contains_key(uri)
            .then_some(0)
    }
}

fn uri_file_kind(uri: &str) -> Option<FileKind> {
    let path = file_path(&Uri::from_str(uri).ok()?)?;
    file_kind(&path)
}

/// The Java side of the window: the answers of jdtls, with the answers of the message
/// bundles in front. A place that is a key does not reach the server, and the checks of
/// both sides are shown together.
pub(crate) struct JavaMessages {
    inner: Arc<LspProviders>,
    router: Arc<Router>,
    messages: Arc<MessageBundles>,
    runtime: tokio::runtime::Handle,
    /// The native answers of the places inside a query of a data repository, between the
    /// messages and the server. Their source is the text itself, not the server.
    data_completion: Option<Arc<dyn Provider<DocumentSpot, CompletionList>>>,
    data_navigation: Option<Arc<dyn Provider<NavigationQuery, Vec<SourceSpan>>>>,
}

impl JavaMessages {
    pub(crate) fn new(
        inner: Arc<LspProviders>,
        router: Arc<Router>,
        messages: Arc<MessageBundles>,
        runtime: tokio::runtime::Handle,
    ) -> Self {
        Self {
            inner,
            router,
            messages,
            runtime,
            data_completion: None,
            data_navigation: None,
        }
    }

    /// Puts the native answers of the queries of a data repository in front of the
    /// server, after the keys of the message bundles.
    pub(crate) fn with_data(
        mut self,
        completion: Arc<dyn Provider<DocumentSpot, CompletionList>>,
        navigation: Arc<dyn Provider<NavigationQuery, Vec<SourceSpan>>>,
    ) -> Self {
        self.data_completion = Some(completion);
        self.data_navigation = Some(navigation);
        self
    }

    /// The text of an open document. The disk is not read here: the caller may be the
    /// interface thread, and a closed file is left to jdtls.
    fn text_of(&self, uri: &str) -> Option<Arc<str>> {
        let parsed = Uri::from_str(uri).ok()?;
        self.router.text(&parsed)
    }

    /// The text and the caret when the caret looks like a key of a bundle: inside a
    /// string literal of its line, with a key call or a validation member before the
    /// literal. Everything else goes to jdtls without a parse.
    fn candidate(&self, uri: &str, point: TextPoint) -> Option<(Arc<str>, usize)> {
        let text = self.text_of(uri)?;
        let offset = offset_in(&text, point);
        java_key_candidate(&text, offset).then_some((text, offset))
    }
}

/// Whether the caret sits in a string literal with a key call or a validation member
/// before it, read from the text alone. The parse of the literal comes later, off the
/// caller's thread.
fn java_key_candidate(text: &str, offset: usize) -> bool {
    let Some(quoted) = java_literal_around(text, offset) else {
        return false;
    };
    matches!(
        java_owner_before(text, quoted.start),
        Some(Owner::Key { .. })
    )
}

impl Provider<DocumentSpot, CompletionList> for JavaMessages {
    fn start(&self, query: DocumentSpot, cancel: Cancel, mut reply: Reply<CompletionList>) {
        let Some((text, offset)) = self.candidate(&query.uri, query.point) else {
            match &self.data_completion {
                Some(provider) => provider.start(query, cancel, reply),
                None => (self.inner.as_ref() as &dyn Provider<DocumentSpot, CompletionList>)
                    .start(query, cancel, reply),
            }
            return;
        };
        let messages = self.messages.clone();
        let inner = self.inner.clone();
        let data = self.data_completion.clone();
        self.runtime.spawn_blocking(move || {
            if cancel.is_cancelled() {
                return;
            }
            match messages.completion(FileKind::Java, &text, offset) {
                Some(list) => {
                    let _ = reply.send(Answer::new(list, Confidence::Certain, SOURCE));
                }
                None => match &data {
                    Some(provider) => provider.start(query, cancel, reply),
                    None => (inner.as_ref() as &dyn Provider<DocumentSpot, CompletionList>)
                        .start(query, cancel, reply),
                },
            }
        });
    }
}

impl Provider<DocumentSpot, HoverText> for JavaMessages {
    fn start(&self, query: DocumentSpot, cancel: Cancel, mut reply: Reply<HoverText>) {
        let Some((text, offset)) = self.candidate(&query.uri, query.point) else {
            (self.inner.as_ref() as &dyn Provider<DocumentSpot, HoverText>)
                .start(query, cancel, reply);
            return;
        };
        let messages = self.messages.clone();
        self.runtime.spawn_blocking(move || {
            if cancel.is_cancelled() {
                return;
            }
            let text = messages
                .hover(FileKind::Java, &text, offset)
                .unwrap_or_default();
            let confidence = if text.is_empty() {
                Confidence::Unknown
            } else {
                Confidence::Certain
            };
            let _ = reply.send(Answer::new(HoverText(text), confidence, SOURCE));
        });
    }
}

impl Provider<NavigationQuery, Vec<SourceSpan>> for JavaMessages {
    fn start(&self, query: NavigationQuery, cancel: Cancel, mut reply: Reply<Vec<SourceSpan>>) {
        let candidate = (query.kind == NavigationKind::Definition)
            .then(|| self.candidate(&query.spot.uri, query.spot.point))
            .flatten();
        let Some((text, offset)) = candidate else {
            match &self.data_navigation {
                Some(provider) => provider.start(query, cancel, reply),
                None => (self.inner.as_ref() as &dyn Provider<NavigationQuery, Vec<SourceSpan>>)
                    .start(query, cancel, reply),
            }
            return;
        };
        let messages = self.messages.clone();
        let inner = self.inner.clone();
        let data = self.data_navigation.clone();
        self.runtime.spawn_blocking(move || {
            if cancel.is_cancelled() {
                return;
            }
            let spans = messages
                .navigation(FileKind::Java, &text, offset)
                .unwrap_or_default();
            if !spans.is_empty() {
                let _ = reply.send(Answer::new(spans, Confidence::Certain, SOURCE));
                return;
            }
            match &data {
                Some(provider) => provider.start(query, cancel, reply),
                None => (inner.as_ref() as &dyn Provider<NavigationQuery, Vec<SourceSpan>>)
                    .start(query, cancel, reply),
            }
        });
    }
}

impl Provider<DiagnosticsQuery, Vec<DiagnosticEntry>> for JavaMessages {
    fn start(&self, query: DiagnosticsQuery, _: Cancel, mut reply: Reply<Vec<DiagnosticEntry>>) {
        let native = lang_api::ask(
            self.messages.as_ref() as &dyn Provider<DiagnosticsQuery, Vec<DiagnosticEntry>>,
            query.clone(),
        );
        let inner = lang_api::ask(
            self.inner.as_ref() as &dyn Provider<DiagnosticsQuery, Vec<DiagnosticEntry>>,
            query,
        );
        enum Side {
            Native(Vec<DiagnosticEntry>),
            Server(Vec<DiagnosticEntry>),
        }
        let merged = futures_util::stream::select(
            native.map(|answer| Side::Native(answer.value)),
            inner.map(|answer| Side::Server(answer.value)),
        );
        self.runtime.spawn(async move {
            let mut merged = merged;
            let mut native: Vec<DiagnosticEntry> = Vec::new();
            let mut server: Vec<DiagnosticEntry> = Vec::new();
            let mut server_answered = false;
            while let Some(side) = merged.next().await {
                match side {
                    Side::Native(entries) => native = entries,
                    Side::Server(entries) => {
                        server = entries;
                        server_answered = true;
                    }
                }
                if !server_answered && native.is_empty() {
                    // The server has published nothing yet: an empty answer of the bundles
                    // alone would clear the marks the server is about to give.
                    continue;
                }
                let mut entries = native.clone();
                entries.extend(server.clone());
                entries.sort_by_key(|entry| (entry.span.start.line, entry.span.start.character));
                if reply
                    .send(Answer::new(entries, Confidence::Certain, SOURCE))
                    .is_err()
                {
                    return;
                }
            }
        });
    }
}

impl Provider<ActionQuery, Vec<ActionEntry>> for JavaMessages {
    fn start(&self, query: ActionQuery, cancel: Cancel, mut reply: Reply<Vec<ActionEntry>>) {
        let Some((text, offset)) = self.candidate(&query.uri, query.span.start) else {
            (self.inner.as_ref() as &dyn Provider<ActionQuery, Vec<ActionEntry>>)
                .start(query, cancel, reply);
            return;
        };
        let messages = self.messages.clone();
        let inner = self.inner.clone();
        let runtime = self.runtime.clone();
        self.runtime.spawn_blocking(move || {
            if cancel.is_cancelled() {
                return;
            }
            let path = path_of_uri(&query.uri).unwrap_or_else(|| PathBuf::from("Main.java"));
            let native = messages.actions_for(&path, &text, offset);
            if native.is_empty() {
                (inner.as_ref() as &dyn Provider<ActionQuery, Vec<ActionEntry>>)
                    .start(query, cancel, reply);
                return;
            }
            runtime.spawn(async move {
                let mut replies = lang_api::ask(
                    inner.as_ref() as &dyn Provider<ActionQuery, Vec<ActionEntry>>,
                    query,
                );
                let (mut entries, confidence) = match replies.next().await {
                    Some(answer) => (answer.value, answer.confidence),
                    None => (Vec::new(), Confidence::Unknown),
                };
                let mut all = native;
                all.append(&mut entries);
                let _ = reply.send(Answer::new(all, confidence, SOURCE));
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::fs;
    use std::path::{Path, PathBuf};

    use futures_util::StreamExt;
    use lang_api::apply_to_text;
    use test_support::TempDir;

    use super::*;

    fn project(name: &str, files: &[(&str, &str)]) -> TempDir {
        let dir = TempDir::new(name);
        for (path, text) in files {
            let path = dir.join(path);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&path, text).unwrap();
        }
        dir
    }

    fn support(dir: &Path) -> Arc<MessageBundles> {
        message_bundles(dir.to_path_buf())
    }

    fn keys_of(support: &MessageBundles) -> HashSet<String> {
        support
            .keys()
            .bundles
            .iter()
            .flat_map(|bundle| &bundle.files)
            .flat_map(|file| file.keys.iter())
            .map(|line| line.key.clone())
            .collect()
    }

    #[test]
    fn the_language_names_the_files_of_a_set() {
        let dir = project(
            "messages-set-files",
            &[
                (
                    "src/main/resources/messages.properties",
                    "welcome=Welcome
",
                ),
                (
                    "src/main/resources/messages_ru.properties",
                    "welcome=Привет
",
                ),
                (
                    "src/main/resources/messages_de.properties",
                    "welcome=Hallo
",
                ),
                (
                    "src/main/resources/other.properties",
                    "key=value
",
                ),
            ],
        );
        let known = support(&dir);
        let base = dir.join("src/main/resources/messages.properties");
        let files = known.files_of(&base).expect("the set is known");
        let locales: Vec<Option<&str>> = files.iter().map(|file| file.locale.as_deref()).collect();
        assert_eq!(locales, [None, Some("de"), Some("ru")]);

        let alone = project(
            "messages-set-alone",
            &[(
                "src/main/resources/other.properties",
                "key=value
",
            )],
        );
        let lone = support(&alone);
        let path = alone.join("src/main/resources/other.properties");
        assert!(lone.files_of(&path).is_none(), "a set of one is no set");
    }

    #[test]
    fn a_bundle_is_its_base_file_and_its_locales() {
        let dir = project(
            "messages-bundle",
            &[
                (
                    "src/main/resources/messages.properties",
                    "welcome=Welcome\nfarewell=Bye\n",
                ),
                (
                    "src/main/resources/messages_ru.properties",
                    "welcome=Привет\n",
                ),
                (
                    "src/main/resources/messages_de.properties",
                    "welcome=Hallo\nfarewell=Tschüss\n",
                ),
            ],
        );
        let support = support(&dir);
        let index = support.keys();
        assert_eq!(index.bundles.len(), 1);
        let bundle = &index.bundles[0];
        assert_eq!(bundle.base, "messages");
        assert_eq!(bundle.files.len(), 3);
        let locales: Vec<Option<&str>> = bundle
            .files
            .iter()
            .map(|file| file.locale.as_deref())
            .collect();
        assert_eq!(locales, [None, Some("de"), Some("ru")]);
        assert!(bundle.base_file().is_some());
        assert_eq!(bundle.missing_locales("farewell"), ["ru"]);
        assert!(bundle.missing_locales("welcome").is_empty());
        assert_eq!(
            support.values_of("welcome").len(),
            3,
            "every locale has the key"
        );
    }

    #[test]
    fn the_base_name_comes_from_the_spring_setting() {
        let dir = project(
            "messages-basename",
            &[
                (
                    "src/main/resources/application.properties",
                    "spring.messages.basename=messages/messages\n",
                ),
                (
                    "src/main/resources/messages/messages.properties",
                    "welcome=Welcome\n",
                ),
                (
                    "src/main/resources/messages/messages_ru.properties",
                    "welcome=Привет\n",
                ),
            ],
        );
        let support = support(&dir);
        let index = support.keys();
        assert_eq!(index.bundles.len(), 1, "the configured base is found");
        assert_eq!(index.bundles[0].base, "messages/messages");
        assert!(index.bundles[0].has_key("welcome"));
        assert_eq!(support.values_of("welcome").len(), 2);
    }

    #[test]
    fn the_base_name_comes_from_get_bundle() {
        let dir = project(
            "messages-getbundle",
            &[
                (
                    "src/main/java/com/example/App.java",
                    "class App {\n  String text() {\n    return ResourceBundle.getBundle(\"Errors\").getString(\"welcome\");\n  }\n}\n",
                ),
                ("src/main/resources/Errors.properties", "welcome=Hi\n"),
            ],
        );
        let support = support(&dir);
        let sources = support.sources();
        assert!(
            sources.bases.contains(&"Errors".to_owned()),
            "{:?}",
            sources.bases
        );
        assert!(
            sources.usages.iter().any(|usage| usage.key == "welcome"),
            "getString of the bundle is a usage"
        );
        let index = support.keys();
        assert!(
            index.bundles.iter().any(|bundle| bundle.base == "Errors"),
            "the base of getBundle is a bundle"
        );
    }

    #[test]
    fn usages_in_java_have_three_forms() {
        let text = "class App {\n\
                    \x20 void run(MessageSource source, Bundle bundle) {\n\
                    \x20   source.getMessage(\"welcome\", null, Locale.ENGLISH);\n\
                    \x20   bundle.getString(\"farewell\");\n\
                    \x20   bundle.getObject(\"total\");\n\
                    \x20 }\n\
                    \x20 @NotEmpty(message = \"{required}\")\n\
                    \x20 String name;\n\
                    }\n";
        let dir = project(
            "messages-java-usages",
            &[
                ("src/main/java/App.java", text),
                ("src/main/resources/messages.properties", "welcome=W\n"),
            ],
        );
        let support = support(&dir);
        let found: HashSet<String> = support
            .sources()
            .usages
            .iter()
            .map(|usage| usage.key.clone())
            .collect();
        assert_eq!(
            found,
            HashSet::from([
                "welcome".to_owned(),
                "farewell".to_owned(),
                "total".to_owned(),
                "required".to_owned(),
            ])
        );
        let welcome = support
            .sources()
            .usages
            .iter()
            .find(|usage| usage.key == "welcome")
            .unwrap()
            .clone();
        let offset = offset_in(text, welcome.span.start);
        assert_eq!(&text[offset..offset + "welcome".len()], "welcome");
    }

    #[test]
    fn usages_in_thymeleaf_are_keys_of_th_attributes() {
        let text = "<html>\n\
                    <body>\n\
                    \x20 <span th:text=\"#{welcome}\">x</span>\n\
                    \x20 <div th:title=\"#{greeting(user.name)}\"></div>\n\
                    \x20 <p title=\"#{ignored}\">#{ignored_too}</p>\n\
                    </body>\n\
                    </html>\n";
        let dir = project(
            "messages-template-usages",
            &[
                ("src/main/resources/templates/welcome.html", text),
                ("src/main/resources/messages.properties", "welcome=W\n"),
            ],
        );
        let support = support(&dir);
        let found: HashSet<String> = support
            .sources()
            .usages
            .iter()
            .map(|usage| usage.key.clone())
            .collect();
        assert_eq!(
            found,
            HashSet::from(["welcome".to_owned(), "greeting".to_owned()])
        );
        assert_eq!(keys_of(&support), HashSet::from(["welcome".to_owned()]));
    }

    #[test]
    fn an_unknown_key_warns_and_the_fix_adds_the_line() {
        let base = "welcome=Welcome\n";
        let dir = project(
            "messages-unknown",
            &[
                (
                    "src/main/java/App.java",
                    "class App {\n  void run(MessageSource source) {\n    source.getMessage(\"missing\", null, null);\n  }\n}\n",
                ),
                ("src/main/resources/messages.properties", base),
            ],
        );
        let support = support(&dir);
        let path = dir.join("src/main/java/App.java");
        let text = fs::read_to_string(&path).unwrap();
        let offset = text.find("missing").unwrap();
        let entries = support.diagnostics_for(&path, &text);
        assert_eq!(entries.len(), 1, "{entries:?}");
        assert_eq!(entries[0].severity, Severity::Warning);
        assert_eq!(entries[0].message, "Cannot resolve property key");

        let actions = support.actions_for(&path, &text, offset);
        assert_eq!(actions.len(), 1, "{actions:?}");
        assert_eq!(actions[0].kind, Some(ActionKind::QuickFix));
        assert_eq!(actions[0].title, "Create property 'missing'");
        let edit = &actions[0].edits[0].edits[0];
        assert_eq!(
            apply_to_text(base, std::slice::from_ref(edit)),
            "welcome=Welcome\nmissing=\n"
        );
    }

    #[test]
    fn a_check_that_is_off_reports_nothing_here() {
        let base = "welcome=Welcome\nunused=Never\n";
        let dir = project(
            "messages-checks-off",
            &[
                (
                    "src/main/java/App.java",
                    "class App {\n  void run(MessageSource source) {\n    source.getMessage(\"missing\", null, null);\n  }\n}\n",
                ),
                ("src/main/resources/messages.properties", base),
                (
                    "src/main/resources/messages_ru.properties",
                    "unused=Никогда\n",
                ),
            ],
        );
        let support = support(&dir);
        let path = dir.join("src/main/resources/messages.properties");
        assert!(
            !support.diagnostics_for(&path, base).is_empty(),
            "the checks are on before the settings arrive"
        );

        let mut settings = lang_api::InspectionSettings::default();
        settings.set("messages.unusedProperty", false, None);
        settings.set("messages.missingTranslation", false, None);
        let checks = lang_api::DisabledInspections::default();
        checks.replace(&settings);
        support.set_inspection_checks(checks.clone());
        assert!(
            support.diagnostics_for(&path, base).is_empty(),
            "the two checks of the properties file are off"
        );

        let java = dir.join("src/main/java/App.java");
        let text = fs::read_to_string(&java).unwrap();
        assert_eq!(
            support.diagnostics_for(&java, &text).len(),
            1,
            "the unknown key keeps its own switch"
        );
        settings.set("messages.unknownKey", false, None);
        checks.replace(&settings);
        assert!(support.diagnostics_for(&java, &text).is_empty());
    }

    #[test]
    fn a_key_without_a_use_and_a_key_a_locale_misses_are_reported() {
        let base = "welcome=Welcome\nunused=Never\n";
        let dir = project(
            "messages-properties-checks",
            &[
                (
                    "src/main/java/App.java",
                    "class App {\n  String text(MessageSource source) {\n    return source.getMessage(\"welcome\", null, null);\n  }\n}\n",
                ),
                ("src/main/resources/messages.properties", base),
                (
                    "src/main/resources/messages_ru.properties",
                    "unused=Никогда\n",
                ),
            ],
        );
        let support = support(&dir);
        let path = dir.join("src/main/resources/messages.properties");
        let entries = support.diagnostics_for(&path, base);
        assert!(
            entries
                .iter()
                .any(|entry| entry.message == "Unused property"
                    && entry.unnecessary
                    && entry.span.start.line == 1),
            "{entries:?}"
        );
        assert!(
            entries
                .iter()
                .any(|entry| entry.message == "Missing translation for: ru"
                    && entry.span.start.line == 0),
            "{entries:?}"
        );
        let usages = support.usages_for(&path, base, 0);
        assert_eq!(usages.len(), 1);
    }

    #[test]
    fn completion_offers_the_keys_of_the_typed_prefix() {
        let dir = project(
            "messages-completion",
            &[
                (
                    "src/main/resources/messages.properties",
                    "welcome=W\nwelcome.back=Back\nfarewell=Bye\n",
                ),
                (
                    "src/main/resources/templates/page.html",
                    "<span th:text=\"#{\">x</span>\n",
                ),
            ],
        );
        let support = support(&dir);
        let java = "class App {\n  String text(MessageSource source) {\n    return source.getMessage(\"wel\", null, null);\n  }\n}\n";
        let offset = java.find("\"wel\"").unwrap() + 4;
        let list = support.completion(FileKind::Java, java, offset).unwrap();
        let labels: Vec<&str> = list
            .entries
            .iter()
            .map(|entry| entry.label.as_str())
            .collect();
        assert_eq!(labels, ["welcome", "welcome.back"]);
        assert_eq!(
            list.entries[0].span,
            Some(span_in(java, java.find("\"wel\"").unwrap() + 1, offset))
        );

        let html = "<span th:text=\"#{\">x</span>\n";
        let offset = html.find("#{").unwrap() + 2;
        let list = support
            .completion(FileKind::Template, html, offset)
            .unwrap();
        assert_eq!(
            list.entries
                .iter()
                .map(|entry| entry.label.as_str())
                .collect::<Vec<_>>(),
            ["farewell", "welcome", "welcome.back"]
        );
    }

    #[test]
    fn the_documentation_of_a_key_shows_every_locale() {
        let dir = project(
            "messages-hover",
            &[
                (
                    "src/main/resources/messages.properties",
                    "welcome=Welcome\n",
                ),
                (
                    "src/main/resources/messages_ru.properties",
                    "welcome=Привет\n",
                ),
            ],
        );
        let support = support(&dir);
        let text = "source.getMessage(\"welcome\")";
        let offset = text.find("welcome").unwrap();
        let hover = support.hover(FileKind::Java, text, offset).unwrap();
        assert!(hover.contains("default: Welcome"), "{hover}");
        assert!(hover.contains("ru: Привет"), "{hover}");
    }

    #[test]
    fn ctrl_b_of_a_key_finds_its_base_file() {
        let dir = project(
            "messages-navigation",
            &[
                (
                    "src/main/resources/messages.properties",
                    "welcome=Welcome\n",
                ),
                (
                    "src/main/resources/messages_ru.properties",
                    "welcome=Привет\n",
                ),
            ],
        );
        let support = support(&dir);
        let text = "class App {\n  String t(MessageSource s) { return s.getMessage(\"welcome\", null, null); }\n}\n";
        let offset = text.find("welcome").unwrap();
        let spans = support.navigation(FileKind::Java, text, offset).unwrap();
        assert_eq!(spans.len(), 1);
        let base = dir.join("src/main/resources/messages.properties");
        assert_eq!(spans[0].uri, uri_of(&base).unwrap());
        assert_eq!(spans[0].span.start.line, 0);
        assert_eq!(spans[0].span.start.character, 0);
        assert_eq!(spans[0].span.end.character, "welcome".len() as u32);
    }

    #[test]
    fn a_rename_changes_the_key_in_every_file_and_usage() {
        let dir = project(
            "messages-rename",
            &[
                (
                    "src/main/resources/messages.properties",
                    "welcome=Welcome\n",
                ),
                (
                    "src/main/resources/messages_ru.properties",
                    "welcome=Привет\n",
                ),
                (
                    "src/main/resources/templates/page.html",
                    "<span th:text=\"#{welcome}\">x</span>\n",
                ),
            ],
        );
        let support = support(&dir);
        let path = dir.join("src/main/resources/messages.properties");
        let text = fs::read_to_string(&path).unwrap();
        let change = support.rename_for(&path, &text, 0, "hello").unwrap();
        assert_eq!(change.edits.len(), 3, "{change:?}");
        for source in &change.edits {
            assert_eq!(source.edits.len(), 1);
        }
    }

    #[test]
    fn an_empty_locale_file_inherits_the_base_file() {
        let base_text = "welcome=Welcome\nfarewell=Bye\n";
        let dir = project(
            "messages-empty-locale",
            &[
                ("src/main/resources/messages.properties", base_text),
                (
                    "src/main/resources/messages_en.properties",
                    "# This file is intentionally empty.\n",
                ),
                (
                    "src/main/resources/messages_ru.properties",
                    "welcome=Привет\n",
                ),
            ],
        );
        let support = support(&dir);
        let index = support.keys();
        let bundle = &index.bundles[0];
        assert!(
            bundle.missing_locales("welcome").is_empty(),
            "an empty locale file is not a missing translation"
        );
        assert_eq!(bundle.missing_locales("farewell"), ["ru"]);
        let path = dir.join("src/main/resources/messages.properties");
        let entries = support.diagnostics_for(&path, base_text);
        assert!(
            entries.iter().all(|entry| !entry.message.contains("en")),
            "en inherits the base file: {entries:?}"
        );
        assert!(
            entries
                .iter()
                .any(|entry| entry.message == "Missing translation for: ru"),
            "{entries:?}"
        );
    }

    #[test]
    fn the_gate_of_a_key_reads_the_literal_and_the_call_before_it() {
        let text = "class App {\n  void run(MessageSource source) {\n    String plain = \"text\";\n    source.getMessage(\"welcome\", null, null);\n  }\n}\n";
        let plain = text.find("\"text\"").unwrap() + 2;
        assert!(
            !java_key_candidate(text, plain),
            "a plain literal is not a key"
        );
        let key = text.find("\"welcome\"").unwrap() + 2;
        assert!(
            java_key_candidate(text, key),
            "the key of getMessage passes"
        );
        let outside = text.find("void").unwrap();
        assert!(!java_key_candidate(text, outside), "no literal, no parse");
    }

    /// The Java side of a project with a bundle, for a test that measures a query.
    fn java_messages(support: Arc<MessageBundles>, uri: &str, text: &str) -> JavaMessages {
        let (_status, status) = tokio::sync::watch::channel(sidecar::Status {
            state: sidecar::State::Stopped,
            generation: 0,
            alive: false,
            client: None,
            process_id: None,
            crashes: 0,
            last_error: None,
        });
        let router = Router::new(status);
        router.open(
            Uri::from_str(uri).unwrap(),
            "java",
            1,
            Arc::from(text.to_owned()),
        );
        let (published, revisions) = semantic_router::Published::new();
        let providers = Arc::new(LspProviders::new(
            router.clone(),
            published,
            revisions,
            tokio::runtime::Handle::current(),
            LANGUAGE_ID.0,
            SOURCE,
        ));
        JavaMessages::new(
            providers,
            router,
            support,
            tokio::runtime::Handle::current(),
        )
    }

    fn many_java_files(name: &str) -> (TempDir, PathBuf) {
        let dir = TempDir::new(name);
        let sources = dir.join("src/main/java/com/example");
        fs::create_dir_all(&sources).unwrap();
        let resources = dir.join("src/main/resources");
        fs::create_dir_all(&resources).unwrap();
        let mut bundle = String::new();
        for index in 0..2000 {
            let key = format!("key{index}");
            bundle.push_str(&format!("{key}=Value {index}\n"));
            let text = format!(
                "package com.example;\n\nclass App{index} {{\n    String text(MessageSource source) {{\n        return source.getMessage(\"{key}\", null, null);\n    }}\n}}\n"
            );
            fs::write(sources.join(format!("App{index}.java")), text).unwrap();
        }
        fs::write(resources.join("messages.properties"), bundle).unwrap();
        (dir, sources)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn completion_of_a_large_project_returns_at_once() {
        let (dir, sources) = many_java_files("messages-many-completion");
        let support = support(&dir);
        let path = sources.join("App0.java");
        let text = fs::read_to_string(&path).unwrap();
        let uri = uri_of(&path).unwrap();
        let spot = DocumentSpot {
            uri: uri.clone(),
            version: 1,
            point: TextPoint {
                line: 0,
                character: 0,
            },
        };
        support.open(&uri, 1, Arc::from(text.as_str()));

        // The provider hands the work to its worker; the answer itself may come later.
        let started = Instant::now();
        let provider = LanguageSupport::completion(support.as_ref()).unwrap();
        let mut replies = lang_api::ask(provider, spot.clone());
        let took = started.elapsed();
        assert!(
            took < Duration::from_millis(50),
            "the bundles return at once, not after {took:?}"
        );
        let _ = tokio::time::timeout(Duration::from_secs(30), replies.next()).await;

        // The same caret outside a literal goes to jdtls without a parse on the caller.
        let java = java_messages(support.clone(), &uri, &text);
        let started = Instant::now();
        let mut replies = lang_api::ask(&java as &dyn Provider<DocumentSpot, CompletionList>, spot);
        let took = started.elapsed();
        assert!(
            took < Duration::from_millis(50),
            "the Java side returns at once, not after {took:?}"
        );
        let _ = tokio::time::timeout(Duration::from_secs(30), replies.next()).await;
    }

    #[test]
    fn an_edit_parses_one_file_of_a_large_project() {
        let (dir, sources) = many_java_files("messages-many-edit");
        let support = support(&dir);
        // The first read walks the project; it is not what this test measures.
        assert_eq!(support.sources().usages.len(), 2000);
        reset_java_parses();

        let path = sources.join("App1.java");
        let uri = uri_of(&path).unwrap();
        let text = "package com.example;\n\nclass App1 {\n    String text(MessageSource source) {\n        return source.getMessage(\"changed\", null, null);\n    }\n}\n";
        // An edit of an open buffer: the text and the mark come without a disk write.
        support
            .inner
            .documents
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .insert(uri, Arc::from(text));
        support.inner.mark_source(path);
        let usages = support.sources();
        assert!(
            usages.usages.iter().any(|usage| usage.key == "changed"),
            "the edit is in the index"
        );
        assert_eq!(java_parses(), 1, "the edit re-reads its own file alone");
    }
}
