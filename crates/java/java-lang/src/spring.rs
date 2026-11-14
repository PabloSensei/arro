//! Spring Boot configuration metadata for `application*.properties` and
//! `application*.yml`: the keys and values of completion, the documentation of a key and
//! the checks of a property file. The metadata comes from
//! `META-INF/spring-configuration-metadata.json` and its additional file, read from the
//! jars of the project class path and from the output folders of the project itself.
//!
//! Reading runs in the background and the interface never waits on it: a provider starts
//! a thread, and the thread answers once the index of the current project model is in.
//! Every query reads an immutable snapshot, so no lock is held while the metadata is
//! parsed or while an answer is written.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::thread;
use std::time::{Duration, SystemTime};

use base::Cancel;
use lang_api::{
    Answer, CompletionEntry, CompletionKind, CompletionLabelDetails, CompletionList, Confidence,
    DiagnosticEntry, DiagnosticsQuery, DocumentSpot, HoverText, LanguageId, LanguageSupport,
    NavigationProvider, NavigationQuery, Provider, Queries, Reply, SemanticServer, Severity,
    SourceId, SourceSpan, SyntaxProvider, TextPoint, TextSpan, offset_in,
};
use lsp_client::lsp_types::Uri;
use lsp_client::uri::{file_path, file_uri};
use platform::CanonPath;
use project_model::ProjectModel;
use tree_sitter_language::LanguageFn;

pub const LANGUAGE_ID: LanguageId = LanguageId("spring-config");

const SOURCE: SourceId = SourceId::new("spring-config");
const CONFIGURATION_METADATA: &str = "META-INF/spring-configuration-metadata.json";
const ADDITIONAL_METADATA: &str = "META-INF/additional-spring-configuration-metadata.json";
/// A wait of a query for the background thread that reads the metadata.
const READY_STEP: Duration = Duration::from_millis(25);
/// The pause before the checks of a file that just changed run again.
const CHECK_DELAY: Duration = Duration::from_millis(120);
/// The most source files the placeholders of `@Value` are looked in.
const MAX_PLACEHOLDER_FILES: usize = 20_000;

unsafe extern "C" fn no_grammar() -> *const () {
    std::ptr::null()
}

const NO_QUERIES: Queries = Queries {
    highlights: "",
    folds: "",
    indents: "",
    locals: "",
};

/// Which of the two formats a configuration file is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigKind {
    Properties,
    Yaml,
}

/// The support of the configuration files of one project. Cloning it shares the index.
#[derive(Clone)]
pub struct SpringConfigSupport {
    inner: Arc<Inner>,
}

/// A support that starts reading as soon as it has a class path.
pub fn spring_config(root: PathBuf) -> Arc<SpringConfigSupport> {
    Arc::new(SpringConfigSupport {
        inner: Arc::new(Inner::new(root)),
    })
}

impl SpringConfigSupport {
    /// The class path and the roots of the project, from its model. A model that names
    /// the same paths as the previous one starts no new read.
    pub fn set_model(&self, model: &ProjectModel) {
        self.set_paths(paths_of(model));
    }

    /// The entries of the class path, for a caller that has no model: a test that puts
    /// a jar with metadata next to its project, for one.
    pub fn set_classpath(&self, classpath: Vec<PathBuf>) {
        self.set_paths(Paths {
            classpath,
            sources: Vec::new(),
            resources: Vec::new(),
        });
    }

    /// The roots the `sourceType` of a key is looked in, for a caller that has no model.
    pub fn set_sources(&self, sources: Vec<PathBuf>) {
        let mut state = self.lock_state();
        if state.paths.sources == sources {
            return;
        }
        state.paths.sources = sources;
        state.generation += 1;
        if state.loading {
            return;
        }
        state.loading = true;
        drop(state);
        start_reader(self.inner.clone());
    }

    fn set_paths(&self, paths: Paths) {
        let mut state = self.lock_state();
        if state.paths == paths {
            return;
        }
        state.paths = paths;
        state.generation += 1;
        if state.loading {
            // The reader is running; its next round sees the new generation.
            return;
        }
        state.loading = true;
        drop(state);
        start_reader(self.inner.clone());
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, Load> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    fn generation(&self) -> u64 {
        self.lock_state().generation
    }

    /// Waits on the caller's thread, never on the interface, for the index of
    /// `generation`. A query the caller dropped stops waiting.
    fn wait_ready<T>(&self, generation: u64, reply: &Reply<T>) -> bool {
        let mut state = self.lock_state();
        while state.loaded < generation {
            if reply.is_cancelled() {
                return false;
            }
            let (next, _) = self
                .inner
                .ready
                .wait_timeout(state, READY_STEP)
                .unwrap_or_else(|poison| poison.into_inner());
            state = next;
        }
        true
    }

    fn snapshot(&self) -> Arc<Snapshot> {
        self.inner
            .snapshot
            .read()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
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
}

struct Inner {
    root: PathBuf,
    state: Mutex<Load>,
    ready: Condvar,
    snapshot: RwLock<Arc<Snapshot>>,
    documents: Mutex<HashMap<String, Arc<str>>>,
    /// The answers of the open documents, which get a new one when the text changes.
    checks: Mutex<HashMap<String, Reply<Vec<DiagnosticEntry>>>>,
}

impl Inner {
    fn new(root: PathBuf) -> Self {
        Self {
            root,
            state: Mutex::new(Load::default()),
            ready: Condvar::new(),
            snapshot: RwLock::new(Arc::new(Snapshot::default())),
            documents: Mutex::new(HashMap::new()),
            checks: Mutex::new(HashMap::new()),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Paths {
    classpath: Vec<PathBuf>,
    sources: Vec<PathBuf>,
    resources: Vec<PathBuf>,
}

#[derive(Default)]
struct Load {
    paths: Paths,
    /// Grows with every model; the snapshot of a generation is complete when the
    /// reader has caught up with it.
    generation: u64,
    loaded: u64,
    loading: bool,
    cache: HashMap<PathBuf, CachedFile>,
}

#[derive(Clone)]
struct CachedFile {
    modified: Option<SystemTime>,
    metadata: Arc<Parsed>,
}

/// The whole metadata of a project at one moment.
#[derive(Clone, Debug, Default)]
struct Snapshot {
    metadata: Metadata,
    /// The names a source file or a configuration file reads with `${name}`.
    placeholders: HashSet<String>,
}

fn paths_of(model: &ProjectModel) -> Paths {
    let mut paths = Paths::default();
    for module in &model.modules {
        for unit in &module.units {
            for entry in unit.compile_paths.iter().chain(unit.runtime_paths.iter()) {
                paths.classpath.push(entry.path.clone());
            }
            // The output of a module carries the metadata of its own
            // `@ConfigurationProperties` classes. It comes after the jars, so that the
            // metadata of the project wins over a library of the same name.
            paths.classpath.extend(unit.output_dirs.iter().cloned());
            paths.sources.extend(unit.source_roots.iter().cloned());
            paths.resources.extend(unit.resource_roots.iter().cloned());
        }
    }
    for list in [&mut paths.classpath, &mut paths.sources] {
        list.sort();
        list.dedup();
    }
    paths.resources.sort();
    paths.resources.dedup();
    paths
}

fn start_reader(inner: Arc<Inner>) {
    thread::spawn(move || {
        loop {
            let (generation, paths, cache) = {
                let mut state = inner
                    .state
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner());
                if state.loaded >= state.generation {
                    state.loading = false;
                    return;
                }
                (state.generation, state.paths.clone(), state.cache.clone())
            };
            let (snapshot, cache) = read_snapshot(&paths, &cache);
            *inner
                .snapshot
                .write()
                .unwrap_or_else(|poison| poison.into_inner()) = Arc::new(snapshot);
            let mut state = inner
                .state
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            state.cache = cache;
            state.loaded = generation;
            drop(state);
            inner.ready.notify_all();
        }
    });
}

fn read_snapshot(
    paths: &Paths,
    cache: &HashMap<PathBuf, CachedFile>,
) -> (Snapshot, HashMap<PathBuf, CachedFile>) {
    let mut metadata = Metadata::default();
    let mut next = HashMap::new();
    for path in &paths.classpath {
        if path.is_dir() {
            for name in [CONFIGURATION_METADATA, ADDITIONAL_METADATA] {
                let file = path.join(name.replace('/', std::path::MAIN_SEPARATOR_STR));
                let Some(parsed) = cached_metadata(&file, cache, &mut next) else {
                    continue;
                };
                // The output folders of the project come last and override a library.
                metadata.add(parsed.as_ref().clone(), true);
            }
        } else if path.is_file() {
            let Some(parsed) = cached_jar(path, cache, &mut next) else {
                continue;
            };
            metadata.add(parsed.as_ref().clone(), false);
        }
    }
    let placeholders = placeholders_in_sources(&paths.sources);
    (
        Snapshot {
            metadata,
            placeholders,
        },
        next,
    )
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
}

fn cached_metadata(
    file: &Path,
    cache: &HashMap<PathBuf, CachedFile>,
    next: &mut HashMap<PathBuf, CachedFile>,
) -> Option<Arc<Parsed>> {
    let modified = modified(file)?;
    if let Some(found) = cache.get(file)
        && found.modified == Some(modified)
    {
        next.insert(file.to_path_buf(), found.clone());
        return Some(found.metadata.clone());
    }
    let text = std::fs::read_to_string(file).ok()?;
    let parsed = Arc::new(parse_metadata(&text));
    next.insert(
        file.to_path_buf(),
        CachedFile {
            modified: Some(modified),
            metadata: parsed.clone(),
        },
    );
    Some(parsed)
}

fn cached_jar(
    jar: &Path,
    cache: &HashMap<PathBuf, CachedFile>,
    next: &mut HashMap<PathBuf, CachedFile>,
) -> Option<Arc<Parsed>> {
    let modified = modified(jar)?;
    if let Some(found) = cache.get(jar)
        && found.modified == Some(modified)
    {
        next.insert(jar.to_path_buf(), found.clone());
        return Some(found.metadata.clone());
    }
    let parsed = Arc::new(read_jar(jar));
    next.insert(
        jar.to_path_buf(),
        CachedFile {
            modified: Some(modified),
            metadata: parsed.clone(),
        },
    );
    Some(parsed)
}

fn read_jar(jar: &Path) -> Parsed {
    let Ok(file) = std::fs::File::open(jar) else {
        return Parsed::default();
    };
    let Ok(mut archive) = zip::ZipArchive::new(file) else {
        return Parsed::default();
    };
    let mut parsed = Parsed::default();
    for name in [CONFIGURATION_METADATA, ADDITIONAL_METADATA] {
        let Ok(mut entry) = archive.by_name(name) else {
            continue;
        };
        let mut text = String::new();
        if entry.read_to_string(&mut text).is_ok() {
            parsed = parsed.merged(parse_metadata(&text));
        }
    }
    parsed
}

/// One property of the metadata, with the parts a list and a popup show.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Property {
    name: String,
    type_name: Option<String>,
    description: Option<String>,
    default_value: Option<String>,
    source_type: Option<String>,
    source_method: Option<String>,
    deprecation: Option<Deprecation>,
}

impl Property {
    fn is_map(&self) -> bool {
        self.type_name.as_deref().is_some_and(is_map_type)
    }

    /// The type as the list shows it: no package and no type arguments.
    fn detail(&self) -> Option<String> {
        self.type_name.as_deref().map(short_type)
    }

    fn deprecated(&self) -> bool {
        self.deprecation.is_some()
    }

    #[cfg(test)]
    fn replacement(&self) -> Option<&str> {
        self.deprecation
            .as_ref()
            .and_then(|deprecation| deprecation.replacement.as_deref())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Group {
    name: String,
    type_name: Option<String>,
    description: Option<String>,
    source_type: Option<String>,
}

impl Group {
    fn is_map(&self) -> bool {
        self.type_name.as_deref().is_some_and(is_map_type)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Deprecation {
    replacement: Option<String>,
    reason: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct HintValue {
    value: String,
    description: Option<String>,
}

/// The metadata of one or more files, merged. A property of a later file replaces the
/// one of an earlier file of the same name only when the caller says so.
#[derive(Clone, Debug, Default)]
struct Metadata {
    properties: HashMap<String, Property>,
    groups: HashMap<String, Group>,
    hints: HashMap<String, Vec<HintValue>>,
}

impl Metadata {
    fn add(&mut self, parsed: Parsed, replace: bool) {
        for property in parsed.properties {
            if replace {
                self.properties.insert(property.name.clone(), property);
            } else {
                self.properties
                    .entry(property.name.clone())
                    .or_insert(property);
            }
        }
        for group in parsed.groups {
            if replace {
                self.groups.insert(group.name.clone(), group);
            } else {
                self.groups.entry(group.name.clone()).or_insert(group);
            }
        }
        for (name, values) in parsed.hints {
            if replace {
                self.hints.insert(name, values);
            } else {
                self.hints.entry(name).or_insert(values);
            }
        }
    }

    fn is_empty(&self) -> bool {
        self.properties.is_empty() && self.groups.is_empty()
    }

    /// Whether `key` names a property, a group, or a level that holds them.
    fn knows(&self, key: &str) -> bool {
        if self.properties.contains_key(key) || self.groups.contains_key(key) {
            return true;
        }
        let deeper = |name: &str| {
            name.len() > key.len() && name.starts_with(key) && name.as_bytes()[key.len()] == b'.'
        };
        self.properties.keys().any(|name| deeper(name))
            || self.groups.keys().any(|name| deeper(name))
    }

    /// Whether any level above `key`, or the property itself, holds arbitrary entries.
    fn under_map(&self, key: &str) -> bool {
        let mut end = 0;
        while let Some(offset) = key[end..].find('.') {
            end += offset;
            let prefix = &key[..end];
            if self.groups.get(prefix).is_some_and(Group::is_map) {
                return true;
            }
            if self.properties.get(prefix).is_some_and(Property::is_map) {
                return true;
            }
            end += 1;
        }
        self.properties.get(key).is_some_and(Property::is_map)
    }

    fn property(&self, key: &str) -> Option<&Property> {
        if let Some(found) = self.properties.get(key) {
            return Some(found);
        }
        // The caret may sit on a level of a map key; show what the level knows.
        let mut end = key.len();
        while let Some(offset) = key[..end].rfind('.') {
            end = offset;
            if let Some(found) = self.properties.get(&key[..end]) {
                return Some(found);
            }
        }
        None
    }

    fn hint_values(&self, key: &str) -> &[HintValue] {
        self.hints.get(key).map(Vec::as_slice).unwrap_or_default()
    }
}

/// One metadata file, before it is merged into a project.
#[derive(Clone, Debug, Default)]
struct Parsed {
    properties: Vec<Property>,
    groups: Vec<Group>,
    hints: Vec<(String, Vec<HintValue>)>,
}

impl Parsed {
    fn merged(mut self, other: Parsed) -> Self {
        self.properties.extend(other.properties);
        self.groups.extend(other.groups);
        self.hints.extend(other.hints);
        self
    }
}

fn parse_metadata(text: &str) -> Parsed {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return Parsed::default();
    };
    let mut parsed = Parsed::default();
    if let Some(properties) = value.get("properties").and_then(|value| value.as_array()) {
        for entry in properties {
            let Some(name) = string(entry, "name") else {
                continue;
            };
            parsed.properties.push(Property {
                name,
                type_name: string(entry, "type"),
                description: string(entry, "description"),
                default_value: text_value(entry, "defaultValue"),
                source_type: string(entry, "sourceType"),
                source_method: string(entry, "sourceMethod"),
                deprecation: parse_deprecation(entry.get("deprecation")),
            });
        }
    }
    if let Some(groups) = value.get("groups").and_then(|value| value.as_array()) {
        for entry in groups {
            let Some(name) = string(entry, "name") else {
                continue;
            };
            parsed.groups.push(Group {
                name,
                type_name: string(entry, "type"),
                description: string(entry, "description"),
                source_type: string(entry, "sourceType"),
            });
        }
    }
    if let Some(hints) = value.get("hints").and_then(|value| value.as_array()) {
        for entry in hints {
            let Some(name) = string(entry, "name") else {
                continue;
            };
            let values: Vec<HintValue> = entry
                .get("values")
                .and_then(|value| value.as_array())
                .map(|values| {
                    values
                        .iter()
                        .filter_map(|value| {
                            Some(HintValue {
                                value: string(value, "value")?,
                                description: string(value, "description"),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            if !values.is_empty() {
                parsed.hints.push((name, values));
            }
        }
    }
    parsed
}

fn parse_deprecation(value: Option<&serde_json::Value>) -> Option<Deprecation> {
    let value = value?;
    Some(Deprecation {
        replacement: string(value, "replacement"),
        reason: string(value, "reason"),
    })
}

fn string(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(|value| value.as_str())
        .map(str::to_owned)
}

/// A value the metadata writes as text, a number or a boolean.
fn text_value(value: &serde_json::Value, key: &str) -> Option<String> {
    let value = value.get(key)?;
    if let Some(text) = value.as_str() {
        return Some(text.to_owned());
    }
    if value.is_null() {
        return None;
    }
    Some(value.to_string())
}

fn is_map_type(type_name: &str) -> bool {
    let type_name = type_name.trim();
    type_name.starts_with("java.util.Map")
        || type_name.starts_with("java.util.HashMap")
        || type_name.starts_with("Map<")
}

fn short_type(type_name: &str) -> String {
    let type_name = type_name.trim();
    let without_arguments = type_name.split('<').next().unwrap_or(type_name);
    let simple = without_arguments
        .rsplit('.')
        .next()
        .unwrap_or(without_arguments);
    simple.trim_end_matches("[]").trim().to_owned()
}

fn is_boolean_type(type_name: Option<&str>) -> bool {
    matches!(type_name, Some("boolean" | "java.lang.Boolean"))
}

fn is_charset_type(type_name: Option<&str>) -> bool {
    matches!(type_name, Some("java.nio.charset.Charset"))
}

fn is_duration_type(type_name: Option<&str>) -> bool {
    matches!(
        type_name,
        Some("java.time.Duration" | "org.springframework.boot.convert.DurationStyle")
    )
}

fn placeholders_in_sources(sources: &[PathBuf]) -> HashSet<String> {
    let mut names = HashSet::new();
    let mut seen = 0;
    for root in sources {
        collect_placeholders(root, &mut names, &mut seen);
        if seen >= MAX_PLACEHOLDER_FILES {
            break;
        }
    }
    names
}

fn collect_placeholders(root: &Path, names: &mut HashSet<String>, seen: &mut usize) {
    if *seen >= MAX_PLACEHOLDER_FILES {
        return;
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        if *seen >= MAX_PLACEHOLDER_FILES {
            return;
        }
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if matches!(
                name.as_ref(),
                "target" | "build" | "out" | ".git" | ".gradle"
            ) {
                continue;
            }
            collect_placeholders(&path, names, seen);
            continue;
        }
        if !path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("java"))
        {
            continue;
        }
        if entry.metadata().map(|meta| meta.len()).unwrap_or(0) > 2 * 1024 * 1024 {
            continue;
        }
        *seen += 1;
        if let Ok(text) = std::fs::read_to_string(&path) {
            collect_placeholders_in_text(&text, names);
        }
    }
}

fn collect_placeholders_in_text(text: &str, names: &mut HashSet<String>) {
    let mut rest = text;
    while let Some(start) = rest.find("${") {
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            break;
        };
        if let Some(name) = placeholder_name(&after[..end]) {
            names.insert(name);
        }
        rest = &after[end + 1..];
    }
}

/// The name a `${...}` reads: the part before a default value, without spaces.
fn placeholder_name(inner: &str) -> Option<String> {
    let name = inner.split(':').next().unwrap_or(inner).trim();
    if name.is_empty()
        || name.contains(char::is_whitespace)
        || name.starts_with('#')
        || name.contains('{')
    {
        return None;
    }
    Some(name.to_owned())
}

/// The keys and the value of one line of a configuration file, for completion, hover
/// and the checks.
#[derive(Clone, Debug, Default)]
struct Context {
    /// The key of the line as it is at the last character of the line.
    key: String,
    /// The key of the line with the levels above it, joined by dots.
    full_key: String,
    /// The part of the key before the caret, joined with the levels above it.
    flat_prefix: String,
    /// Where the key of the line starts, the place a completion replaces from.
    key_start: usize,
    /// Where the value of the line starts, and where the value before the caret does.
    value_start: usize,
    /// The text of the value before the caret.
    typed_value: String,
    /// The levels of the document above the line, innermost last.
    ancestors: Vec<String>,
    /// The indent of the line in bytes.
    indent: usize,
    /// The line already has a separator, so a key completion replaces only the key.
    has_separator: bool,
}

impl Context {
    fn value(&self) -> bool {
        self.value_start > 0
    }
}

pub(crate) fn config_kind(path: &Path) -> Option<ConfigKind> {
    let name = path.file_name()?.to_str()?.to_ascii_lowercase();
    if !(name.starts_with("application") || name.starts_with("bootstrap")) {
        return None;
    }
    if name.ends_with(".properties") {
        Some(ConfigKind::Properties)
    } else if name.ends_with(".yml") || name.ends_with(".yaml") {
        Some(ConfigKind::Yaml)
    } else {
        None
    }
}

fn line_at(text: &str, offset: usize) -> (usize, usize) {
    let start = text[..offset].rfind('\n').map_or(0, |index| index + 1);
    let end = text[offset..]
        .find('\n')
        .map_or(text.len(), |index| offset + index);
    (start, end)
}

fn context(text: &str, offset: usize, kind: ConfigKind) -> Option<Context> {
    match kind {
        ConfigKind::Properties => properties_context(text, offset),
        ConfigKind::Yaml => yaml_context(text, offset),
    }
}

fn properties_context(text: &str, offset: usize) -> Option<Context> {
    let offset = offset.min(text.len());
    let (line_start, line_end) = line_at(text, offset);
    let line = &text[line_start..line_end];
    let trimmed = line.trim_start();
    if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('!') {
        return None;
    }
    let indent = line.len() - trimmed.len();
    let content_start = line_start + indent;
    let line_end = line_start + line.trim_end().len();
    let (key_end, value_start) = split_property(&text[content_start..line_end]);
    let mut context = Context {
        key: text[content_start..content_start + key_end]
            .trim()
            .to_owned(),
        full_key: String::new(),
        flat_prefix: String::new(),
        key_start: content_start,
        value_start: 0,
        typed_value: String::new(),
        ancestors: Vec::new(),
        indent,
        has_separator: key_end < line_end - content_start,
    };
    context.full_key = context.key.clone();
    if value_start > key_end {
        let value_start = content_start + value_start;
        if offset >= value_start {
            context.value_start = value_start;
            context.typed_value = text[value_start..offset].to_owned();
            context.flat_prefix = context.key.clone();
        } else {
            context.flat_prefix = text[content_start..offset].trim_end().to_owned();
        }
    } else {
        context.flat_prefix = text[content_start..offset].trim_end().to_owned();
    }
    Some(context)
}

/// The end of the key of a property line and where its value starts: any unescaped `=`,
/// `:` or whitespace ends the key, and the whitespace and one separator after it belong
/// to neither.
fn split_property(line: &str) -> (usize, usize) {
    let mut escaped = false;
    let mut key_end = line.len();
    for (index, character) in line.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' {
            escaped = true;
            continue;
        }
        if character == '=' || character == ':' || character == ' ' || character == '\t' {
            key_end = index;
            break;
        }
    }
    let mut value_start = key_end;
    while value_start < line.len() && line.as_bytes()[value_start].is_ascii_whitespace() {
        value_start += 1;
    }
    if value_start < line.len()
        && (line.as_bytes()[value_start] == b'=' || line.as_bytes()[value_start] == b':')
    {
        value_start += 1;
    }
    while value_start < line.len() && line.as_bytes()[value_start].is_ascii_whitespace() {
        value_start += 1;
    }
    (key_end, value_start)
}

fn yaml_context(text: &str, offset: usize) -> Option<Context> {
    let offset = offset.min(text.len());
    let (line_start, line_end) = line_at(text, offset);
    let line = &text[line_start..line_end];
    let trimmed = line.trim_start();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    let mut indent = line.len() - trimmed.len();
    let mut content_start = line_start + indent;
    if text[content_start..].starts_with("- ") || text[content_start..].starts_with("-\t") {
        indent += 2;
        content_start += 2;
    }
    let content_end = line_end - (line.len() - line.trim_end().len());
    if content_start >= content_end {
        return None;
    }
    let mut ancestors = Vec::new();
    yaml_ancestors(text, line_start, indent, &mut ancestors);
    let mut context = Context {
        key: String::new(),
        full_key: String::new(),
        flat_prefix: String::new(),
        key_start: content_start,
        value_start: 0,
        typed_value: String::new(),
        ancestors: ancestors.clone(),
        indent,
        has_separator: false,
    };
    if let Some(found) = text[content_start..].find(':') {
        let colon_at = content_start + found;
        if colon_at < content_end {
            let key = text[content_start..colon_at].trim();
            context.key = unquote(key).to_owned();
            context.has_separator = true;
            let mut value_start = colon_at + 1;
            while value_start < line_end && text.as_bytes()[value_start].is_ascii_whitespace() {
                value_start += 1;
            }
            if offset >= value_start {
                context.value_start = value_start;
                context.typed_value = text[value_start..offset].to_owned();
            }
            context.full_key = flat_key(&context.ancestors, &context.key);
        }
    }
    if !context.has_separator {
        context.key = text[content_start..content_end].trim().to_owned();
        context.full_key = flat_key(&context.ancestors, &context.key);
    }
    if context.value() {
        context.flat_prefix = context.full_key.clone();
    } else {
        let typed = text[content_start..offset].trim_end();
        context.flat_prefix = flat_key(&context.ancestors, typed);
    }
    Some(context)
}

/// The levels above a YAML line that are still open at its indent.
fn yaml_ancestors(text: &str, line_start: usize, indent: usize, out: &mut Vec<String>) {
    let mut level = indent;
    let mut cursor = line_start;
    while cursor > 0 {
        let end = cursor - 1;
        let start = text[..end].rfind('\n').map_or(0, |index| index + 1);
        let line = &text[start..end];
        cursor = start;
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let line_indent = line.len() - trimmed.len();
        if line_indent < level {
            if let Some(key) = yaml_line_key(trimmed) {
                out.push(key);
            }
            level = line_indent;
            if level == 0 {
                break;
            }
        }
    }
    out.reverse();
}

fn yaml_line_key(line: &str) -> Option<String> {
    let line = line.trim_start_matches("- ").trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let key = line.split(':').next().unwrap_or(line).trim();
    (!key.is_empty()).then(|| unquote(key).to_owned())
}

fn unquote(text: &str) -> &str {
    let text = text.trim();
    for quote in ['"', '\''] {
        if text.len() >= 2 && text.starts_with(quote) && text.ends_with(quote) {
            return &text[1..text.len() - 1];
        }
    }
    text
}

fn flat_key(ancestors: &[String], key: &str) -> String {
    let key = key.trim();
    if key.is_empty() {
        if ancestors.is_empty() {
            return String::new();
        }
        return format!("{}.", ancestors.join("."));
    }
    if ancestors.is_empty() {
        return key.to_owned();
    }
    let prefix = format!("{}.", ancestors.join("."));
    if key.starts_with(&prefix) {
        key.to_owned()
    } else {
        format!("{prefix}{key}")
    }
}

/// A pair of a YAML document with its flat key, from the tree.
struct YamlPair {
    full_key: String,
    key: Range<usize>,
    value: Option<Range<usize>>,
    nested: bool,
}

fn yaml_pairs(text: &str) -> Vec<YamlPair> {
    let mut parser = tree_sitter::Parser::new();
    let grammar = file_syntax::yaml().grammar;
    if parser.set_language(&grammar).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(text, None) else {
        return Vec::new();
    };
    let mut pairs = Vec::new();
    collect_yaml_pairs(tree.root_node(), text, &mut pairs);
    pairs
}

fn collect_yaml_pairs(node: tree_sitter::Node, text: &str, out: &mut Vec<YamlPair>) {
    if node.kind() == "block_mapping_pair"
        && let Some(key) = node.child_by_field_name("key")
    {
        let value = node.child_by_field_name("value");
        let nested = value.is_some_and(|value| {
            matches!(
                value.kind(),
                "block_mapping" | "flow_mapping" | "block_sequence" | "flow_sequence"
            )
        });
        out.push(YamlPair {
            full_key: yaml_pair_key(node, text),
            key: key.start_byte()..key.end_byte(),
            value: value
                .filter(|value| {
                    !matches!(
                        value.kind(),
                        "block_mapping" | "flow_mapping" | "block_sequence" | "flow_sequence"
                    )
                })
                .map(|value| value.start_byte()..value.end_byte()),
            nested,
        });
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_yaml_pairs(child, text, out);
    }
}

fn yaml_pair_key(node: tree_sitter::Node, text: &str) -> String {
    let mut levels = Vec::new();
    if let Some(key) = node.child_by_field_name("key") {
        levels.push(unquote(node_text(key, text)).to_owned());
    }
    let mut parent = node.parent();
    while let Some(found) = parent {
        if found.kind() == "block_mapping_pair"
            && let Some(key) = found.child_by_field_name("key")
        {
            levels.push(unquote(node_text(key, text)).to_owned());
        }
        parent = found.parent();
    }
    levels.reverse();
    levels.join(".")
}

fn node_text<'a>(node: tree_sitter::Node, text: &'a str) -> &'a str {
    node.utf8_text(text.as_bytes()).unwrap_or_default().trim()
}

/// The key a place of a configuration file names, for the documentation and the go to
/// declaration.
fn key_at(text: &str, offset: usize, kind: ConfigKind) -> Option<String> {
    match kind {
        ConfigKind::Properties => properties_context(text, offset)
            .filter(|context| !context.key.is_empty())
            .map(|context| context.key),
        ConfigKind::Yaml => {
            for pair in yaml_pairs(text) {
                let inside = pair.key.contains(&offset)
                    || pair
                        .value
                        .as_ref()
                        .is_some_and(|value| value.contains(&offset));
                if inside {
                    return Some(pair.full_key);
                }
            }
            yaml_context(text, offset).map(|context| context.full_key)
        }
    }
}

fn completion_entries(
    text: &str,
    offset: usize,
    kind: ConfigKind,
    snapshot: &Snapshot,
) -> Vec<CompletionEntry> {
    let metadata = &snapshot.metadata;
    let Some(context) = context(text, offset, kind) else {
        return Vec::new();
    };
    if context.value() {
        return value_entries(text, offset, &context, metadata);
    }
    if metadata.is_empty() {
        return Vec::new();
    }
    let flat = context.flat_prefix.clone();
    let filter_from = flat.rfind('.').map(|index| index + 1).unwrap_or(0);
    let span = Some(span_in(text, context.key_start, offset));
    let mut entries = Vec::new();
    for (name, property) in &metadata.properties {
        if name.len() <= flat.len() || !name.starts_with(&flat) {
            continue;
        }
        let label = name[filter_from..].to_owned();
        let insert = match kind {
            ConfigKind::Properties => name.clone(),
            ConfigKind::Yaml => {
                if context.has_separator {
                    name.clone()
                } else {
                    yaml_insertion(text, &context, name)
                }
            }
        };
        entries.push(CompletionEntry {
            label: label.clone(),
            label_details: Some(CompletionLabelDetails {
                detail: None,
                description: property
                    .description
                    .as_deref()
                    .map(short_description)
                    .or_else(|| {
                        property
                            .deprecation
                            .as_ref()
                            .and_then(|deprecation| deprecation.reason.as_deref())
                            .map(short_description)
                    }),
            }),
            detail: property.detail(),
            filter_text: Some(label),
            sort_text: Some(format!(
                "{}{}",
                if property.deprecated() { '1' } else { '0' },
                name
            )),
            insert_text: Some(insert),
            span,
            snippet: false,
            additional_edits: Vec::new(),
            command: None,
            resolve: None,
            kind: CompletionKind::Property,
            deprecated: property.deprecated(),
        });
    }
    entries.sort_by(|left, right| left.sort_text.cmp(&right.sort_text));
    entries
}

fn value_entries(
    text: &str,
    offset: usize,
    context: &Context,
    metadata: &Metadata,
) -> Vec<CompletionEntry> {
    let property = metadata.property(&context.full_key);
    let mut values: Vec<HintValue> = Vec::new();
    let type_name = property.and_then(|property| property.type_name.as_deref());
    if is_boolean_type(type_name) {
        values.push(HintValue {
            value: "true".to_owned(),
            description: None,
        });
        values.push(HintValue {
            value: "false".to_owned(),
            description: None,
        });
    }
    if is_charset_type(type_name) {
        for value in [
            "UTF-8",
            "ISO-8859-1",
            "US-ASCII",
            "UTF-16",
            "UTF-16LE",
            "UTF-16BE",
        ] {
            values.push(HintValue {
                value: value.to_owned(),
                description: None,
            });
        }
    }
    if is_duration_type(type_name) {
        for (value, description) in [
            ("10s", "ten seconds"),
            ("1m", "one minute"),
            ("30s", "thirty seconds"),
            ("1h", "one hour"),
            ("1d", "one day"),
        ] {
            values.push(HintValue {
                value: value.to_owned(),
                description: Some(description.to_owned()),
            });
        }
    }
    values.extend(metadata.hint_values(&context.full_key).iter().cloned());
    let mut seen = HashSet::new();
    values.retain(|value| seen.insert(value.value.clone()));
    let before = &text[context.value_start..offset.min(text.len())];
    let cut = before.rfind(',').map(|index| index + 1).unwrap_or(0);
    let trimmed = before[cut..].len() - before[cut..].trim_start().len();
    let start = context.value_start + cut + trimmed;
    let span = Some(span_in(text, start, offset.min(text.len())));
    let mut entries: Vec<CompletionEntry> = values
        .into_iter()
        .map(|value| CompletionEntry {
            label: value.value.clone(),
            label_details: Some(CompletionLabelDetails {
                detail: None,
                description: value.description.as_deref().map(short_description),
            }),
            detail: type_name.map(short_type),
            filter_text: Some(value.value.clone()),
            sort_text: Some(format!("0{}", value.value)),
            insert_text: Some(value.value),
            span,
            snippet: false,
            additional_edits: Vec::new(),
            command: None,
            resolve: None,
            kind: CompletionKind::Value,
            deprecated: false,
        })
        .collect();
    entries.sort_by(|left, right| left.sort_text.cmp(&right.sort_text));
    entries
}

/// A key in a YAML document with the levels it does not have yet: the first level stays
/// on the line of the caret, each level below gets one more indent.
fn yaml_insertion(text: &str, context: &Context, full_key: &str) -> String {
    let mut rest = full_key;
    if !context.ancestors.is_empty() {
        let prefix = format!("{}.", context.ancestors.join("."));
        if let Some(stripped) = full_key.strip_prefix(&prefix) {
            rest = stripped;
        }
    }
    let unit = indent_unit(text);
    let segments: Vec<&str> = rest.split('.').filter(|part| !part.is_empty()).collect();
    let mut out = String::new();
    for (index, segment) in segments.iter().enumerate() {
        if index > 0 {
            out.push('\n');
            out.push_str(&" ".repeat(context.indent + unit * index));
        }
        out.push_str(segment);
        if index + 1 == segments.len() {
            out.push_str(": ");
        } else {
            out.push(':');
        }
    }
    out
}

fn indent_unit(text: &str) -> usize {
    for line in text.lines() {
        let trimmed = line.trim_start_matches(' ');
        let spaces = line.len() - trimmed.len();
        if spaces > 0 && spaces <= 8 && !trimmed.trim().is_empty() {
            return spaces;
        }
    }
    2
}

fn short_description(description: &str) -> String {
    let line = description
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or(description);
    let mut text: String = line.chars().take(80).collect();
    if line.chars().count() > 80 {
        text.push_str("...");
    }
    text
}

fn diagnostics_for(text: &str, kind: ConfigKind, snapshot: &Snapshot) -> Vec<DiagnosticEntry> {
    let metadata = &snapshot.metadata;
    if metadata.is_empty() {
        return Vec::new();
    }
    let mut local = HashSet::new();
    collect_placeholders_in_text(text, &mut local);
    // Both formats are read from their own shape: the YAML tree gives the full key of
    // every pair, the line of a property file gives its own.
    let entries = match kind {
        ConfigKind::Properties => properties_entries(text),
        ConfigKind::Yaml => yaml_entries(text),
    };
    let mut found = Vec::new();
    for (key, key_range, value) in entries {
        if key.is_empty() {
            continue;
        }
        let exact = metadata.properties.get(&key);
        if let Some(deprecation) = exact.and_then(|property| property.deprecation.as_ref()) {
            let mut message = format!("Deprecated Spring Boot property `{key}`");
            if let Some(replacement) = &deprecation.replacement {
                message.push_str(&format!(": use `{replacement}` instead"));
            }
            if let Some(reason) = &deprecation.reason {
                message.push_str(&format!(". {reason}"));
            }
            found.push(DiagnosticEntry {
                span: span_in(text, key_range.start, key_range.end),
                severity: Severity::Warning,
                message,
                unnecessary: false,
            });
        } else if exact.is_none()
            && !metadata.knows(&key)
            && !metadata.under_map(&key)
            && !local.contains(&key)
            && !snapshot.placeholders.contains(&key)
            && key.contains('.')
        {
            found.push(DiagnosticEntry {
                span: span_in(text, key_range.start, key_range.end),
                severity: Severity::Warning,
                message: format!("Unknown Spring Boot property `{key}`"),
                unnecessary: false,
            });
        }
        let (Some((raw, range)), Some(property)) = (value, exact) else {
            continue;
        };
        if is_boolean_type(property.type_name.as_deref()) {
            let cleaned = value_text(raw);
            if !cleaned.is_empty()
                && !cleaned.eq_ignore_ascii_case("true")
                && !cleaned.eq_ignore_ascii_case("false")
            {
                found.push(DiagnosticEntry {
                    span: span_in(text, range.start, range.end),
                    severity: Severity::Error,
                    message: format!("`{cleaned}` is not a boolean: use `true` or `false`"),
                    unnecessary: false,
                });
            }
        } else if let Some(allowed) = enum_values(metadata, &key, property)
            && let Some(cleaned) = single_value(raw)
            && !allowed
                .iter()
                .any(|allowed| allowed.eq_ignore_ascii_case(&cleaned))
        {
            found.push(DiagnosticEntry {
                span: span_in(text, range.start, range.end),
                severity: Severity::Error,
                message: format!(
                    "`{cleaned}` is not a value of `{key}`: one of {}",
                    allowed.join(", ")
                ),
                unnecessary: false,
            });
        }
    }
    found
}

/// A property line with its key, the key range and the value it has, if any.
type PropertyEntry<'a> = (String, Range<usize>, Option<(&'a str, Range<usize>)>);

fn properties_entries(text: &str) -> Vec<PropertyEntry<'_>> {
    let mut entries = Vec::new();
    let mut offset = 0;
    for line in text.split('\n') {
        let start = offset;
        offset += line.len() + 1;
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('!') {
            continue;
        }
        let indent = line.len() - trimmed.len();
        let content_start = start + indent;
        let content_end = start + line.trim_end().len();
        if content_start >= content_end {
            continue;
        }
        let (key_end, value_start) = split_property(&text[content_start..content_end]);
        if key_end == 0 {
            continue;
        }
        let key_range = content_start..content_start + key_end;
        let key = text[key_range.clone()].trim().to_owned();
        let value = (value_start < content_end - content_start).then(|| {
            let value_start = content_start + value_start;
            (&text[value_start..content_end], value_start..content_end)
        });
        entries.push((key, key_range, value));
    }
    entries
}

fn yaml_entries(text: &str) -> Vec<PropertyEntry<'_>> {
    let mut entries = Vec::new();
    for pair in yaml_pairs(text) {
        if pair.nested {
            continue;
        }
        let value = pair
            .value
            .as_ref()
            .map(|range| (&text[range.clone()], range.clone()));
        entries.push((pair.full_key, pair.key, value));
    }
    entries
}

fn value_text(value: &str) -> String {
    let value = value.trim();
    if value.contains("${") || value.starts_with("#{") {
        return String::new();
    }
    value.trim_matches('"').trim_matches('\'').trim().to_owned()
}

/// The keys and the literal values of a configuration file, in the order the file spells
/// them. A commented line and a value that needs the placeholders of the environment give
/// nothing, so a caller reads only what the file says by itself.
pub(crate) fn settings(text: &str, kind: ConfigKind) -> Vec<(String, String)> {
    let entries = match kind {
        ConfigKind::Properties => properties_entries(text),
        ConfigKind::Yaml => yaml_entries(text),
    };
    entries
        .into_iter()
        .filter(|(key, _, _)| !key.is_empty())
        .filter_map(|(key, _, value)| {
            let value = value_text(value?.0);
            (!value.is_empty()).then_some((key, value))
        })
        .collect()
}

fn single_value(value: &str) -> Option<String> {
    let value = value_text(value);
    if value.is_empty() || value.contains(',') {
        return None;
    }
    Some(value)
}

/// The values an enum-like property accepts, when the metadata names them and the type
/// is not a scalar that takes any text.
fn enum_values(metadata: &Metadata, key: &str, property: &Property) -> Option<Vec<String>> {
    let hints = metadata.hint_values(key);
    if hints.is_empty() || !is_enum_type(property.type_name.as_deref()) {
        return None;
    }
    Some(hints.iter().map(|hint| hint.value.clone()).collect())
}

fn is_enum_type(type_name: Option<&str>) -> bool {
    let Some(type_name) = type_name else {
        return false;
    };
    if type_name == "java.lang.String" {
        return false;
    }
    !(is_boolean_type(Some(type_name))
        || is_charset_type(Some(type_name))
        || is_duration_type(Some(type_name))
        || is_map_type(type_name)
        || type_name.starts_with("java.util.List")
        || type_name.starts_with("java.util.Set")
        || type_name.starts_with("java.util.Collection")
        || type_name.starts_with("java.lang.")
        || type_name
            .chars()
            .next()
            .is_some_and(|first| !first.is_ascii_alphabetic()))
}

fn documentation(snapshot: &Snapshot, key: &str) -> Option<String> {
    let metadata = &snapshot.metadata;
    let property = metadata.property(key);
    let group = metadata.groups.get(key);
    if property.is_none() && group.is_none() {
        return None;
    }
    let name = property.map_or(key, |property| property.name.as_str());
    let mut text = format!("**`{name}`**\n\n");
    if let Some(property) = property {
        if let Some(type_name) = &property.type_name {
            text.push_str(&format!("`{type_name}`"));
            if let Some(default_value) = &property.default_value {
                text.push_str(&format!(", default `{default_value}`"));
            }
            text.push_str("\n\n");
        }
        if let Some(description) = &property.description {
            text.push_str(description.trim());
            text.push_str("\n\n");
        }
        if let Some(deprecation) = &property.deprecation {
            text.push_str("**Deprecated.**");
            if let Some(replacement) = &deprecation.replacement {
                text.push_str(&format!(" Use `{replacement}` instead."));
            }
            if let Some(reason) = &deprecation.reason {
                text.push_str(&format!(" {reason}"));
            }
            text.push('\n');
            text.push('\n');
        }
        let hints = metadata.hint_values(name);
        if !hints.is_empty() {
            text.push_str("Values:");
            for hint in hints {
                text.push_str(&format!(" `{}`", hint.value));
            }
            text.push('\n');
            text.push('\n');
        }
        if let Some(source_type) = &property.source_type {
            text.push_str(&format!("Source: `{source_type}`"));
            if let Some(source_method) = &property.source_method {
                text.push_str(&format!(" `{source_method}`"));
            }
            text.push('\n');
        }
    } else if let Some(group) = group {
        if let Some(type_name) = &group.type_name {
            text.push_str(&format!("`{type_name}`\n\n"));
        }
        if let Some(description) = &group.description {
            text.push_str(description.trim());
            text.push_str("\n\n");
        }
        if let Some(source_type) = &group.source_type {
            text.push_str(&format!("Source: `{source_type}`\n"));
        }
    }
    Some(text)
}

fn source_span(snapshot: &Snapshot, key: &str, sources: &[PathBuf]) -> Option<SourceSpan> {
    let metadata = &snapshot.metadata;
    let source_type = metadata
        .property(key)
        .and_then(|property| property.source_type.as_deref())
        .or_else(|| {
            metadata
                .groups
                .get(key)
                .and_then(|group| group.source_type.as_deref())
        })?;
    let path = source_file(source_type, sources)?;
    let canonical = CanonPath::new(&path).ok()?;
    let uri = file_uri(&canonical)?;
    Some(SourceSpan {
        uri: uri.as_str().to_owned(),
        span: TextSpan {
            start: TextPoint::default(),
            end: TextPoint::default(),
        },
    })
}

fn source_file(source_type: &str, sources: &[PathBuf]) -> Option<PathBuf> {
    let type_name = source_type.split('<').next().unwrap_or(source_type).trim();
    let mut candidate = type_name;
    loop {
        let relative = candidate.replace('.', std::path::MAIN_SEPARATOR_STR);
        for root in sources {
            let file = root.join(format!("{relative}.java"));
            if file.is_file() {
                return Some(file);
            }
        }
        // An inner class lives in the file of its outer class.
        let index = candidate.rfind('.')?;
        candidate = &candidate[..index];
    }
}

fn point_in(text: &str, offset: usize) -> TextPoint {
    let offset = offset.min(text.len());
    let line_start = text[..offset].rfind('\n').map_or(0, |index| index + 1);
    let line = text[line_start..offset].encode_utf16().count() as u32;
    let before = text[..line_start]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count() as u32;
    TextPoint {
        line: before,
        character: line,
    }
}

fn span_in(text: &str, start: usize, end: usize) -> TextSpan {
    TextSpan {
        start: point_in(text, start),
        end: point_in(text, end),
    }
}

impl SyntaxProvider for SpringConfigSupport {
    fn grammar(&self) -> LanguageFn {
        // The editor colours these files with `file-syntax`; the provider exists for the
        // actions that ask a language about a declaration.
        unsafe { LanguageFn::from_raw(no_grammar) }
    }

    fn queries(&self) -> &Queries {
        &NO_QUERIES
    }
}

impl LanguageSupport for SpringConfigSupport {
    fn id(&self) -> LanguageId {
        LANGUAGE_ID
    }

    fn matches(&self, path: &Path) -> bool {
        config_kind(path).is_some() && self.is_resource(path)
    }

    fn definition_falls_back_to_hover(&self) -> bool {
        true
    }

    fn syntax(&self) -> &dyn SyntaxProvider {
        self
    }

    fn server(&self) -> Option<&dyn SemanticServer> {
        Some(self)
    }

    fn completion(&self) -> Option<&lang_api::CompletionProvider> {
        Some(self)
    }

    fn hover(&self) -> Option<&lang_api::HoverProvider> {
        Some(self)
    }

    fn diagnostics(&self) -> Option<&lang_api::DiagnosticsProvider> {
        Some(self)
    }

    fn navigation(&self) -> Option<&NavigationProvider> {
        Some(self)
    }
}

impl SpringConfigSupport {
    fn is_resource(&self, path: &Path) -> bool {
        let resources = self.lock_state().paths.resources.clone();
        if resources.iter().any(|root| path.starts_with(root)) {
            return true;
        }
        // A project without a model still knows its own folder: a configuration file
        // outside it belongs to another project.
        if !path.starts_with(&self.inner.root) {
            return false;
        }
        path.components().any(|component| {
            component
                .as_os_str()
                .to_string_lossy()
                .eq_ignore_ascii_case("resources")
        })
    }

    fn kind_of(&self, uri: &str) -> Option<ConfigKind> {
        let path = file_path(&Uri::from_str(uri).ok()?)?;
        config_kind(&path)
    }
}

impl Provider<DocumentSpot, CompletionList> for SpringConfigSupport {
    fn start(&self, query: DocumentSpot, _: Cancel, mut reply: Reply<CompletionList>) {
        let support = self.clone();
        thread::spawn(move || {
            let generation = support.generation();
            if !support.wait_ready(generation, &reply) {
                return;
            }
            let snapshot = support.snapshot();
            let entries = support
                .kind_of(&query.uri)
                .zip(support.text_of(&query.uri))
                .map(|(kind, text)| {
                    completion_entries(&text, offset_in(&text, query.point), kind, &snapshot)
                })
                .unwrap_or_default();
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

impl Provider<DocumentSpot, HoverText> for SpringConfigSupport {
    fn start(&self, query: DocumentSpot, _: Cancel, mut reply: Reply<HoverText>) {
        let support = self.clone();
        thread::spawn(move || {
            let generation = support.generation();
            if !support.wait_ready(generation, &reply) {
                return;
            }
            let snapshot = support.snapshot();
            let text = support
                .kind_of(&query.uri)
                .zip(support.text_of(&query.uri))
                .and_then(|(kind, text)| {
                    key_at(&text, offset_in(&text, query.point), kind)
                        .and_then(|key| documentation(&snapshot, &key))
                })
                .unwrap_or_default();
            let _ = reply.send(Answer::new(HoverText(text), Confidence::Certain, SOURCE));
        });
    }
}

impl Provider<DiagnosticsQuery, Vec<DiagnosticEntry>> for SpringConfigSupport {
    fn start(&self, query: DiagnosticsQuery, _: Cancel, reply: Reply<Vec<DiagnosticEntry>>) {
        let support = self.clone();
        {
            let mut checks = support
                .inner
                .checks
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            checks.insert(query.0.clone(), reply);
        }
        schedule_checks(support, query.0, false);
    }
}

impl Provider<NavigationQuery, Vec<SourceSpan>> for SpringConfigSupport {
    fn start(&self, query: NavigationQuery, _: Cancel, mut reply: Reply<Vec<SourceSpan>>) {
        let support = self.clone();
        thread::spawn(move || {
            let generation = support.generation();
            if !support.wait_ready(generation, &reply) {
                return;
            }
            let snapshot = support.snapshot();
            let sources = support.lock_state().paths.sources.clone();
            let spans = support
                .kind_of(&query.spot.uri)
                .zip(support.text_of(&query.spot.uri))
                .and_then(|(kind, text)| {
                    let key = key_at(&text, offset_in(&text, query.spot.point), kind)?;
                    source_span(&snapshot, &key, &sources)
                })
                .into_iter()
                .collect();
            let _ = reply.send(Answer::new(spans, Confidence::Certain, SOURCE));
        });
    }
}

impl lang_api::SemanticServer for SpringConfigSupport {
    fn open(&self, uri: &str, _version: i32, text: Arc<str>) {
        self.inner
            .documents
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .insert(uri.to_owned(), text);
    }

    fn change(
        &self,
        uri: &str,
        _version: i32,
        text: Arc<str>,
        _edits: Vec<lang_api::DocumentEdit>,
    ) {
        self.inner
            .documents
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .insert(uri.to_owned(), text);
        schedule_checks(self.clone(), uri.to_owned(), true);
    }

    fn save(&self, _uri: &str) {}

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

/// Runs the checks of `uri` and sends them to the open editor. A change of the text
/// waits a moment, so that a burst of keys checks once.
fn schedule_checks(support: SpringConfigSupport, uri: String, delay: bool) {
    thread::spawn(move || {
        if delay {
            thread::sleep(CHECK_DELAY);
        }
        let Some(kind) = support.kind_of(&uri) else {
            return;
        };
        let text = support.text_of(&uri);
        let snapshot = support.snapshot();
        let entries = text
            .as_deref()
            .map(|text| diagnostics_for(text, kind, &snapshot))
            .unwrap_or_default();
        let mut checks = support
            .inner
            .checks
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let Some(reply) = checks.get_mut(&uri) else {
            return;
        };
        if reply
            .send(Answer::new(entries, Confidence::Certain, SOURCE))
            .is_err()
        {
            checks.remove(&uri);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata(json: &str) -> Metadata {
        let mut metadata = Metadata::default();
        metadata.add(parse_metadata(json), true);
        metadata
    }

    fn snapshot(json: &str) -> Snapshot {
        Snapshot {
            metadata: metadata(json),
            ..Snapshot::default()
        }
    }

    const SAMPLE: &str = r#"{
        "groups": [
            {
                "name": "spring.jpa",
                "type": "org.example.JpaProperties",
                "sourceType": "org.example.JpaProperties"
            },
            {
                "name": "logging.level",
                "type": "java.util.Map<java.lang.String,java.lang.String>"
            }
        ],
        "properties": [
            {
                "name": "spring.jpa.open-in-view",
                "type": "java.lang.Boolean",
                "defaultValue": true,
                "description": "Whether to enable the open entity manager in view interceptor.",
                "sourceType": "org.example.JpaProperties"
            },
            {
                "name": "spring.jpa.hibernate.ddl-auto",
                "type": "org.example.DdlAuto",
                "description": "What to do with the schema.",
                "sourceType": "org.example.JpaProperties"
            },
            {
                "name": "spring.jpa.old-mode",
                "type": "java.lang.String",
                "deprecation": {
                    "replacement": "spring.jpa.new-mode",
                    "reason": "It is gone."
                },
                "sourceType": "org.example.JpaProperties"
            },
            {
                "name": "spring.jpa.legacy",
                "type": "java.lang.String"
            }
        ],
        "hints": [
            {
                "name": "spring.jpa.hibernate.ddl-auto",
                "values": [
                    {"value": "none", "description": "No change."},
                    {"value": "update"},
                    {"value": "validate"}
                ]
            }
        ]
    }"#;

    #[test]
    fn groups_properties_hints_and_deprecations_are_read() {
        let metadata = metadata(SAMPLE);
        assert_eq!(metadata.groups.len(), 2);
        assert_eq!(metadata.properties.len(), 4);
        assert!(metadata.knows("spring.jpa"));
        assert!(metadata.knows("spring.jpa.open-in-view"));
        assert!(!metadata.knows("logging.level.com.shop"));
        assert!(metadata.under_map("logging.level.com.shop"));
        assert!(!metadata.under_map("spring.jpa.open-in-view"));
        let property = metadata.properties.get("spring.jpa.open-in-view").unwrap();
        assert_eq!(property.detail().as_deref(), Some("Boolean"));
        assert_eq!(property.default_value.as_deref(), Some("true"));
        assert!(!property.deprecated());
        assert_eq!(
            metadata.hint_values("spring.jpa.hibernate.ddl-auto").len(),
            3
        );
        let old = metadata.properties.get("spring.jpa.old-mode").unwrap();
        assert!(old.deprecated());
        assert_eq!(old.replacement(), Some("spring.jpa.new-mode"));
    }

    #[test]
    fn a_property_line_is_split_into_a_key_and_a_value() {
        let entries = properties_entries(
            "# a comment\nspring.jpa.open-in-view = true\nlogging.level.com.shop=DEBUG\n",
        );
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].0, "spring.jpa.open-in-view");
        assert_eq!(entries[0].2.as_ref().map(|(value, _)| *value), Some("true"));
        assert_eq!(entries[1].0, "logging.level.com.shop");
        assert_eq!(
            entries[1].2.as_ref().map(|(value, _)| *value),
            Some("DEBUG")
        );
    }

    fn value_of<'a>(settings: &'a [(String, String)], key: &str) -> Option<&'a str> {
        settings
            .iter()
            .rev()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    }

    #[test]
    fn the_settings_of_a_properties_file_are_read_by_their_keys() {
        let found = settings(
            "# PetClinic\ndriver=org.h2.Driver\nspring.datasource.url=jdbc:h2:mem:pet\n\
             spring.datasource.username: sa\n\
             spring.datasource.driver-class-name = org.h2.Driver\n",
            ConfigKind::Properties,
        );

        assert_eq!(
            value_of(&found, "spring.datasource.url"),
            Some("jdbc:h2:mem:pet")
        );
        assert_eq!(value_of(&found, "spring.datasource.username"), Some("sa"));
        assert_eq!(
            value_of(&found, "spring.datasource.driver-class-name"),
            Some("org.h2.Driver")
        );
    }

    #[test]
    fn the_settings_of_a_nested_yaml_are_read_by_their_full_keys() {
        let found = settings(
            "spring:\n  datasource:\n    url: \"jdbc:postgresql://localhost:5432/pet\"\n\
             \x20   username: sa\n    driver-class-name: org.postgresql.Driver\n\
             server:\n  port: 8080\n",
            ConfigKind::Yaml,
        );

        assert_eq!(
            value_of(&found, "spring.datasource.url"),
            Some("jdbc:postgresql://localhost:5432/pet")
        );
        assert_eq!(value_of(&found, "spring.datasource.username"), Some("sa"));
        assert_eq!(value_of(&found, "server.port"), Some("8080"));
    }

    #[test]
    fn a_comment_after_a_yaml_value_is_left_out() {
        let found = settings(
            "spring:\n  datasource:\n    url: jdbc:mysql://h/db # local\n",
            ConfigKind::Yaml,
        );

        assert_eq!(
            value_of(&found, "spring.datasource.url"),
            Some("jdbc:mysql://h/db")
        );
    }

    #[test]
    fn a_placeholder_gives_no_setting_of_its_own() {
        let found = settings(
            "spring.datasource.url=${DB_URL}\nspring.datasource.username=sa\n",
            ConfigKind::Properties,
        );

        assert_eq!(value_of(&found, "spring.datasource.url"), None);
        assert_eq!(value_of(&found, "spring.datasource.username"), Some("sa"));
    }

    #[test]
    fn an_unknown_key_warns_and_a_map_level_does_not() {
        let snapshot = snapshot(SAMPLE);
        let found = diagnostics_for(
            "spring.jpa.open-in-view=true\nspring.jpa.unknown=1\nlogging.level.com.shop=DEBUG\ndatabase=h2\n",
            ConfigKind::Properties,
            &snapshot,
        );
        let warnings: Vec<&str> = found
            .iter()
            .filter(|entry| entry.severity == Severity::Warning)
            .map(|entry| entry.message.as_str())
            .collect();
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("spring.jpa.unknown"));
    }

    #[test]
    fn a_placeholder_of_the_same_file_hides_its_key() {
        let snapshot = snapshot(SAMPLE);
        let found = diagnostics_for(
            "database=h2\nspring.jpa.open-in-view=${database}\n",
            ConfigKind::Properties,
            &snapshot,
        );
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn a_wrong_boolean_and_a_wrong_enum_are_errors() {
        let snapshot = snapshot(SAMPLE);
        let found = diagnostics_for(
            "spring.jpa.open-in-view=maybe\nspring.jpa.hibernate.ddl-auto=wrong\n",
            ConfigKind::Properties,
            &snapshot,
        );
        let errors = found
            .iter()
            .filter(|entry| entry.severity == Severity::Error)
            .count();
        assert_eq!(errors, 2, "{found:?}");
    }

    #[test]
    fn a_yaml_document_is_read_by_its_tree() {
        let text =
            "spring:\n  jpa:\n    open-in-view: false\n    hibernate:\n      ddl-auto: none\n";
        let pairs = yaml_pairs(text);
        let keys: Vec<&str> = pairs.iter().map(|pair| pair.full_key.as_str()).collect();
        assert_eq!(
            keys,
            [
                "spring",
                "spring.jpa",
                "spring.jpa.open-in-view",
                "spring.jpa.hibernate",
                "spring.jpa.hibernate.ddl-auto"
            ]
        );
    }

    #[test]
    fn a_dotted_key_at_the_top_of_a_yaml_file_gets_its_levels() {
        let text = "spring.jpa.open";
        let context = yaml_context(text, text.len()).unwrap();
        assert_eq!(context.flat_prefix, "spring.jpa.open");
        let property = "spring.jpa.open-in-view";
        assert_eq!(
            yaml_insertion(text, &context, property),
            "spring:\n  jpa:\n    open-in-view: "
        );
    }

    #[test]
    fn a_key_inside_an_existing_block_fills_only_the_missing_level() {
        let text = "spring:\n  jpa.op";
        let offset = text.len();
        let context = yaml_context(text, offset).unwrap();
        assert_eq!(context.ancestors, ["spring"]);
        let property = "spring.jpa.open-in-view";
        assert_eq!(
            yaml_insertion(text, &context, property),
            "jpa:\n    open-in-view: "
        );
    }

    #[test]
    fn a_value_completion_offers_the_type_values() {
        let snapshot = Snapshot {
            metadata: metadata(SAMPLE),
            placeholders: HashSet::new(),
        };
        let text = "spring.jpa.open-in-view=";
        let entries = completion_entries(text, text.len(), ConfigKind::Properties, &snapshot);
        let labels: Vec<&str> = entries.iter().map(|entry| entry.label.as_str()).collect();
        assert_eq!(labels, ["false", "true"]);
        let text = "spring.jpa.hibernate.ddl-auto=";
        let entries = completion_entries(text, text.len(), ConfigKind::Properties, &snapshot);
        let labels: Vec<&str> = entries.iter().map(|entry| entry.label.as_str()).collect();
        assert_eq!(labels, ["none", "update", "validate"]);
    }

    #[test]
    fn a_deprecated_key_is_struck_and_goes_last() {
        let snapshot = Snapshot {
            metadata: metadata(SAMPLE),
            placeholders: HashSet::new(),
        };
        let text = "spring.jpa.";
        let entries = completion_entries(text, text.len(), ConfigKind::Properties, &snapshot);
        let deprecated = entries
            .iter()
            .find(|entry| entry.label == "old-mode")
            .expect("the deprecated key is offered");
        assert!(deprecated.deprecated);
        assert_eq!(
            entries.last().map(|entry| entry.label.as_str()),
            Some("old-mode")
        );
    }

    #[test]
    fn the_documentation_has_the_type_default_description_and_source() {
        let snapshot = snapshot(SAMPLE);
        let text = documentation(&snapshot, "spring.jpa.open-in-view").unwrap();
        assert!(text.contains("java.lang.Boolean"), "{text}");
        assert!(text.contains("default `true`"), "{text}");
        assert!(text.contains("open entity manager"), "{text}");
        assert!(text.contains("org.example.JpaProperties"), "{text}");
        let old = documentation(&snapshot, "spring.jpa.old-mode").unwrap();
        assert!(old.contains("spring.jpa.new-mode"), "{old}");
    }

    fn jar_with(folder: &Path, entries: &[(&str, &str)]) -> PathBuf {
        let file = folder.join("metadata.jar");
        let mut writer = zip::ZipWriter::new(std::fs::File::create(&file).unwrap());
        for (name, text) in entries {
            use std::io::Write as _;
            writer
                .start_file(
                    *name,
                    zip::write::SimpleFileOptions::default()
                        .compression_method(zip::CompressionMethod::Stored),
                )
                .unwrap();
            writer.write_all(text.as_bytes()).unwrap();
        }
        writer.finish().unwrap();
        file
    }

    fn wait_for_snapshot(support: &SpringConfigSupport) -> Arc<Snapshot> {
        let generation = support.generation();
        let mut state = support.lock_state();
        while state.loaded < generation {
            let (next, _) = support
                .inner
                .ready
                .wait_timeout(state, Duration::from_secs(5))
                .unwrap();
            state = next;
        }
        drop(state);
        support.snapshot()
    }

    #[test]
    fn a_jar_with_metadata_is_read_from_the_class_path() {
        let folder = test_support::TempDir::new("spring-jar");
        let jar = jar_with(
            folder.path(),
            &[
                (CONFIGURATION_METADATA, SAMPLE),
                (
                    ADDITIONAL_METADATA,
                    r#"{"properties":[{"name":"spring.jpa.extra","type":"java.lang.String"}]}"#,
                ),
            ],
        );
        let support = spring_config(folder.path().to_path_buf());
        support.set_classpath(vec![jar]);
        let snapshot = wait_for_snapshot(&support);
        assert!(
            snapshot.metadata.knows("spring.jpa.open-in-view"),
            "the metadata of the jar is in"
        );
        assert!(snapshot.metadata.knows("spring.jpa.extra"));
    }

    #[test]
    fn the_output_folder_of_the_project_is_read_like_a_jar() {
        let folder = test_support::TempDir::new("spring-classes");
        let classes = folder.path().join("target");
        let meta = classes.join("META-INF");
        std::fs::create_dir_all(&meta).unwrap();
        std::fs::write(meta.join("spring-configuration-metadata.json"), SAMPLE).unwrap();
        let support = spring_config(folder.path().to_path_buf());
        support.set_classpath(vec![classes]);
        let snapshot = wait_for_snapshot(&support);
        assert!(snapshot.metadata.knows("spring.jpa.open-in-view"));
    }

    #[tokio::test]
    async fn completion_of_a_key_comes_through_the_provider() {
        use futures_util::StreamExt;

        let folder = test_support::TempDir::new("spring-complete");
        let jar = jar_with(folder.path(), &[(CONFIGURATION_METADATA, SAMPLE)]);
        let support = spring_config(folder.path().to_path_buf());
        support.set_classpath(vec![jar]);
        let path = folder.path().join("application.properties");
        std::fs::write(&path, "spring.jpa.").unwrap();
        let uri = CanonPath::new(&path).unwrap().to_uri();
        support.open(&uri, 1, Arc::from("spring.jpa."));
        let point = TextPoint {
            line: 0,
            character: 11,
        };
        let provider: &dyn Provider<DocumentSpot, CompletionList> = support.as_ref();
        let mut replies = lang_api::ask(
            provider,
            DocumentSpot {
                uri,
                version: 1,
                point,
            },
        );
        let answer = replies.next().await.expect("the provider answers");
        let labels: Vec<&str> = answer
            .value
            .entries
            .iter()
            .map(|entry| entry.label.as_str())
            .collect();
        assert!(labels.contains(&"open-in-view"), "{labels:?}");
        assert!(labels.contains(&"hibernate.ddl-auto"), "{labels:?}");
    }

    #[test]
    fn the_type_of_a_key_leads_to_its_source_file() {
        let folder = test_support::TempDir::new("spring-source");
        let root = folder.path().join("src").join("main").join("java");
        let file = root.join("org").join("example").join("JpaProperties.java");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "package org.example;\nclass JpaProperties {}\n").unwrap();
        let snapshot = snapshot(SAMPLE);
        let span = source_span(&snapshot, "spring.jpa.open-in-view", &[root]).unwrap();
        assert!(span.uri.ends_with("JpaProperties.java"), "{}", span.uri);
        assert!(
            source_span(
                &snapshot,
                "spring.jpa.legacy",
                &[folder.path().to_path_buf()]
            )
            .is_none(),
            "a key without a source type has no place to go"
        );
    }

    #[test]
    fn only_application_and_bootstrap_files_of_a_resource_folder_match() {
        let support = spring_config(PathBuf::from("C:/project"));
        assert!(support.matches(Path::new(
            "C:/project/src/main/resources/application.properties"
        )));
        assert!(support.matches(Path::new(
            "C:/project/src/test/resources/application-test.yml"
        )));
        assert!(support.matches(Path::new("C:/project/src/main/resources/bootstrap.yaml")));
        assert!(!support.matches(Path::new("C:/project/application.properties")));
        assert!(!support.matches(Path::new("C:/project/src/main/resources/other.properties")));
        assert!(!support.matches(Path::new("C:/project/src/main/java/App.java")));
    }
}
