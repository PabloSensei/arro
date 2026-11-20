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
/// The extension descriptor of Quarkus, whose `config` list names keys and prefixes.
const QUARKUS_EXTENSION: &str = "META-INF/quarkus-extension.yaml";
/// The configuration metadata of a Quarkus extension, in the shape of the other
/// frameworks when an extension ships one.
const QUARKUS_METADATA: &str = "META-INF/quarkus-config-metadata.json";
/// The configuration metadata of Micronaut: one file or a folder of them.
const MICRONAUT_FOLDER: &str = "META-INF/micronaut/";
const MICRONAUT_METADATA: &str = "META-INF/micronaut/configuration-metadata.json";
/// The checks of this language, for the handle of the disabled ones.
const SPRING_CHECKS: &[&str] = &[
    "spring.config.unknownProperty",
    "spring.config.deprecatedProperty",
    "spring.config.invalidValue",
    "quarkus.config.unknownProperty",
    "micronaut.config.unknownProperty",
];
/// A wait of a query for the background thread that reads the metadata.
const READY_STEP: Duration = Duration::from_millis(25);
/// The pause before the checks of a file that just changed run again.
const CHECK_DELAY: Duration = Duration::from_millis(120);
/// The most source files the placeholders of `@Value` are looked in.
const MAX_PLACEHOLDER_FILES: usize = 20_000;
/// The longest chain of supertypes a member of a configuration class is looked for in.
const MAX_SUPERTYPE_DEPTH: usize = 5;

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
    /// The checks the user turned off, shared with the rest of the language.
    pub fn set_inspection_checks(&self, checks: lang_api::DisabledInspections) {
        *self
            .inner
            .inspection_checks
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = checks;
    }

    fn inspection_checks(&self) -> lang_api::DisabledInspections {
        self.inner
            .inspection_checks
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }

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

    /// The resource roots the keys are searched for in, for a caller that has no model.
    pub fn set_resources(&self, resources: Vec<PathBuf>) {
        let mut state = self.lock_state();
        if state.paths.resources == resources {
            return;
        }
        state.paths.resources = resources;
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
    /// The checks the user turned off: one that is off is not computed at all.
    inspection_checks: Mutex<lang_api::DisabledInspections>,
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
            inspection_checks: Mutex::new(lang_api::DisabledInspections::default()),
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
            for name in [
                CONFIGURATION_METADATA,
                ADDITIONAL_METADATA,
                QUARKUS_METADATA,
                MICRONAUT_METADATA,
            ] {
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
    // The configuration classes of the project declare their own keys.
    let declared = config_properties_in_sources(&paths.sources);
    metadata.add(declared, true);
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
    for index in 0..archive.len() {
        let Ok(mut entry) = archive.by_index(index) else {
            continue;
        };
        let Ok(raw_name) = entry.name() else {
            continue;
        };
        let name = raw_name.to_string();
        let kind = if name == CONFIGURATION_METADATA
            || name == ADDITIONAL_METADATA
            || name == QUARKUS_METADATA
            || name == MICRONAUT_METADATA
            || name.ends_with("configuration-metadata.json")
            || (name.starts_with(MICRONAUT_FOLDER) && name.ends_with(".json"))
        {
            Some(MetadataKind::Json)
        } else if name == QUARKUS_EXTENSION {
            Some(MetadataKind::QuarkusExtension)
        } else {
            None
        };
        let Some(kind) = kind else {
            continue;
        };
        let mut text = String::new();
        if entry.read_to_string(&mut text).is_err() {
            continue;
        }
        let parsed_file = match kind {
            MetadataKind::Json => parse_metadata(&text),
            MetadataKind::QuarkusExtension => parse_quarkus_extension(&text),
        };
        parsed = parsed.merged(parsed_file);
    }
    parsed
}

/// The shape of one metadata file of a jar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MetadataKind {
    Json,
    QuarkusExtension,
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
    /// The roots of the keys the metadata knows, `spring`, `quarkus` or `micronaut`.
    roots: HashSet<String>,
    /// The full prefixes a framework descriptor names without listing their keys.
    prefixes: HashSet<String>,
}

impl Metadata {
    fn add(&mut self, parsed: Parsed, replace: bool) {
        for property in parsed.properties {
            note_prefix(&mut self.roots, &property.name);
            if replace {
                self.properties.insert(property.name.clone(), property);
            } else {
                self.properties
                    .entry(property.name.clone())
                    .or_insert(property);
            }
        }
        for group in parsed.groups {
            note_prefix(&mut self.roots, &group.name);
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
        for prefix in parsed.prefixes {
            note_prefix(&mut self.roots, &prefix);
            self.prefixes.insert(prefix);
        }
    }

    fn is_empty(&self) -> bool {
        self.properties.is_empty() && self.groups.is_empty()
    }

    /// Whether the metadata knows anything under the root of a key.
    fn knows_root(&self, root: &str) -> bool {
        self.roots.contains(root)
    }

    /// Whether a key lives under a prefix a descriptor named without listing its keys.
    fn under_known_prefix(&self, key: &str) -> bool {
        self.prefixes.iter().any(|prefix| {
            key.len() > prefix.len()
                && key.starts_with(prefix.as_str())
                && key.as_bytes()[prefix.len()] == b'.'
        })
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

    /// The property a key names under relaxed binding: `max-connections`,
    /// `maxConnections` and `max_connections` are the same key.
    fn navigation_property(&self, key: &str) -> Option<&Property> {
        let mut end = key.len();
        loop {
            let prefix = &key[..end];
            if let Some(found) = self.properties.get(prefix) {
                return Some(found);
            }
            let wanted = normalized_key(prefix);
            if let Some(found) = self
                .properties
                .values()
                .find(|property| normalized_key(&property.name) == wanted)
            {
                return Some(found);
            }
            end = prefix.rfind('.')?;
        }
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
    /// Keys and prefixes of a framework that ships no property list: a Quarkus
    /// extension descriptor names what it reads, not the type of every key.
    prefixes: Vec<String>,
}

impl Parsed {
    fn merged(mut self, other: Parsed) -> Self {
        self.properties.extend(other.properties);
        self.groups.extend(other.groups);
        self.hints.extend(other.hints);
        self.prefixes.extend(other.prefixes);
        self
    }
}

/// The root of a key, `quarkus` for `quarkus.http.port`.
fn note_prefix(prefixes: &mut HashSet<String>, name: &str) {
    if let Some(root) = name.split('.').next()
        && !root.is_empty()
    {
        prefixes.insert(root.to_owned());
    }
}

/// Reads the JSON metadata of the frameworks. The Spring shape names `properties` and
/// `groups`; the Micronaut shape maps every key to its type, description and default
/// value, so a top-level map without those lists is read as a property list.
fn parse_metadata(text: &str) -> Parsed {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return Parsed::default();
    };
    if value.get("properties").is_none()
        && value.get("groups").is_none()
        && value.get("hints").is_none()
        && let Some(map) = value.as_object()
    {
        let mut parsed = Parsed::default();
        for (name, entry) in map {
            if !name.contains('.') || !entry.is_object() {
                continue;
            }
            parsed.properties.push(Property {
                name: name.clone(),
                type_name: string(entry, "type"),
                description: string(entry, "description"),
                default_value: text_value(entry, "defaultValue"),
                source_type: string(entry, "sourceType"),
                source_method: string(entry, "sourceMethod"),
                deprecation: parse_deprecation(entry.get("deprecation")),
            });
            if let Some(values) = entry.get("values").and_then(|value| value.as_array()) {
                let hints: Vec<HintValue> = values
                    .iter()
                    .filter_map(|value| {
                        Some(HintValue {
                            value: string(value, "value")?,
                            description: string(value, "description"),
                        })
                    })
                    .collect();
                if !hints.is_empty() {
                    parsed.hints.push((name.clone(), hints));
                }
            }
        }
        return parsed;
    }
    parse_spring_metadata(&value)
}

/// Reads a Quarkus extension descriptor. Its `config` list names the keys and the
/// prefixes the extension reads; an entry that ends with a dot is a prefix, the rest are
/// keys. A version of the descriptor that carries no `config` list gives nothing.
fn parse_quarkus_extension(text: &str) -> Parsed {
    let mut parsed = Parsed::default();
    let mut in_config = false;
    let mut config_indent = 0;
    for line in text.lines() {
        let without_comment = line.split('#').next().unwrap_or("");
        if without_comment.trim().is_empty() {
            continue;
        }
        let indent = without_comment.len() - without_comment.trim_start().len();
        let content = without_comment.trim();
        if !in_config {
            if content == "config:" {
                in_config = true;
                config_indent = indent;
            }
            continue;
        }
        if indent <= config_indent {
            in_config = false;
            continue;
        }
        let Some(entry) = content.strip_prefix('-') else {
            continue;
        };
        let entry = entry.trim().trim_matches('"').trim_matches('\'');
        let name = entry
            .split_whitespace()
            .next()
            .unwrap_or(entry)
            .trim_end_matches(':');
        if name.is_empty() {
            continue;
        }
        if let Some(prefix) = name.strip_suffix('.') {
            if !prefix.is_empty() {
                parsed.prefixes.push(prefix.to_owned());
            }
            continue;
        }
        if !parsed.properties.iter().any(|found| found.name == name) {
            parsed.properties.push(Property {
                name: name.to_owned(),
                ..Property::default()
            });
        }
    }
    parsed
}

fn parse_spring_metadata(value: &serde_json::Value) -> Parsed {
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

/// The keys the configuration classes of the project declare: `@ConfigMapping` and
/// `@ConfigProperties` of Quarkus, `@ConfigurationProperties` and `@EachProperty` of
/// Micronaut. The types of a framework's own jars are not read.
fn config_properties_in_sources(sources: &[PathBuf]) -> Parsed {
    let mut files: Vec<(PathBuf, java_syntax::SpringFile)> = Vec::new();
    let mut seen = 0;
    for root in sources {
        collect_java_sources(root, &mut files, &mut seen);
        if seen >= MAX_PLACEHOLDER_FILES {
            break;
        }
    }
    let mut types: HashMap<&str, &java_syntax::Type> = HashMap::new();
    for (_, file) in &files {
        for kind in &file.types {
            types.entry(kind.name.as_str()).or_insert(kind);
        }
    }
    let mut parsed = Parsed::default();
    for (_, file) in &files {
        for kind in &file.types {
            let Some((annotation, shape)) = config_annotation(&kind.annotations) else {
                continue;
            };
            let Some(prefix) = config_prefix(annotation, shape) else {
                continue;
            };
            expand_config_type(kind, &prefix, shape, &types, &mut parsed, 0);
        }
    }
    parsed
}

/// What a configuration annotation declares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConfigShape {
    /// Methods of an interface or a class declare the keys.
    Mapping,
    /// Fields and getters of a class declare the keys.
    Properties,
    /// Every instance under the prefix has the keys of the type.
    EachProperty,
}

fn config_annotation(
    annotations: &[java_syntax::Annotation],
) -> Option<(&java_syntax::Annotation, ConfigShape)> {
    let find = |name: &str| annotations.iter().find(|found| found.name == name);
    if let Some(annotation) = find("ConfigMapping") {
        return Some((annotation, ConfigShape::Mapping));
    }
    if let Some(annotation) = find("ConfigProperties") {
        return Some((annotation, ConfigShape::Mapping));
    }
    if let Some(annotation) = find("ConfigurationProperties") {
        return Some((annotation, ConfigShape::Properties));
    }
    if let Some(annotation) = find("EachProperty") {
        return Some((annotation, ConfigShape::EachProperty));
    }
    None
}

/// The prefix of a configuration type, with the dot that joins its members.
fn config_prefix(annotation: &java_syntax::Annotation, shape: ConfigShape) -> Option<String> {
    let prefix = annotation
        .argument("prefix")
        .or(annotation.value.as_deref())
        .map(str::trim)
        .filter(|prefix| !prefix.is_empty())?;
    let mut prefix = prefix.to_owned();
    if shape == ConfigShape::EachProperty {
        prefix.push_str(".*");
    }
    if !prefix.ends_with('.') {
        prefix.push('.');
    }
    Some(prefix)
}

fn expand_config_type(
    kind: &java_syntax::Type,
    prefix: &str,
    shape: ConfigShape,
    types: &HashMap<&str, &java_syntax::Type>,
    parsed: &mut Parsed,
    depth: usize,
) {
    if depth > 4 {
        return;
    }
    parsed.groups.push(Group {
        name: prefix.trim_end_matches('.').to_owned(),
        type_name: Some(kind.name.clone()),
        description: None,
        source_type: Some(kind.qualified.clone()),
    });
    for (name, type_text) in config_members(kind, shape) {
        let key = format!("{prefix}{}", kebab(&name));
        let short = simple_config_type(&type_text);
        if let Some(nested) = types.get(short)
            && config_annotation(&nested.annotations).is_some()
        {
            expand_config_type(
                nested,
                &format!("{key}."),
                config_annotation(&nested.annotations).map_or(shape, |(_, shape)| shape),
                types,
                parsed,
                depth + 1,
            );
            continue;
        }
        parsed.properties.push(Property {
            name: key,
            type_name: Some(type_text),
            description: None,
            default_value: None,
            source_type: Some(kind.qualified.clone()),
            source_method: None,
            deprecation: None,
        });
    }
}

/// The names and types of the members that declare keys.
fn config_members(kind: &java_syntax::Type, shape: ConfigShape) -> Vec<(String, String)> {
    let mut found = Vec::new();
    match shape {
        ConfigShape::Mapping => {
            for method in &kind.methods {
                if method.parameters.is_empty() && !method.return_type.is_empty() {
                    found.push((method.name.clone(), method.return_type.clone()));
                }
            }
        }
        ConfigShape::Properties | ConfigShape::EachProperty => {
            for field in &kind.fields {
                if !field.is_static {
                    found.push((field.name.clone(), field.type_text.clone()));
                }
            }
            for method in &kind.methods {
                if !method.parameters.is_empty() || method.return_type.is_empty() {
                    continue;
                }
                let name = method
                    .name
                    .strip_prefix("get")
                    .or_else(|| method.name.strip_prefix("is"));
                if let Some(name) = name
                    && !name.is_empty()
                {
                    found.push((name.to_owned(), method.return_type.clone()));
                }
            }
        }
    }
    found
}

/// The type of a member without its type arguments and its package, as the map of the
/// project knows it.
fn simple_config_type(type_text: &str) -> &str {
    let without_arguments = type_text.split('<').next().unwrap_or(type_text).trim();
    without_arguments
        .rsplit('.')
        .next()
        .unwrap_or(without_arguments)
}

/// A member name as the configuration spells it: `httpPort` gives `http-port`.
fn kebab(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    for (index, character) in name.char_indices() {
        if character.is_ascii_uppercase() {
            if index > 0 {
                out.push('-');
            }
            out.push(character.to_ascii_lowercase());
        } else {
            out.push(character);
        }
    }
    out
}

/// A key without the differences relaxed binding allows: the case, the `-` and the `_`
/// of its levels, and the index of a list entry.
fn normalized_key(key: &str) -> String {
    let mut out = String::with_capacity(key.len());
    let mut in_index = false;
    for character in key.chars() {
        match character {
            '[' => in_index = true,
            ']' => in_index = false,
            _ if in_index => {}
            '-' | '_' => {}
            _ => out.extend(character.to_lowercase()),
        }
    }
    out
}

/// The member a level of a key names: `max-connections`, `maxConnections` and
/// `max_connections` all give `maxConnections`, and `roles[0]` gives `roles`.
fn member_name(segment: &str) -> Option<String> {
    let segment = segment.split('[').next().unwrap_or(segment).trim();
    if segment.is_empty() {
        return None;
    }
    let mut out = String::with_capacity(segment.len());
    let mut capitalize = false;
    for character in segment.chars() {
        if character == '-' || character == '_' {
            capitalize = true;
            continue;
        }
        if capitalize {
            out.extend(character.to_uppercase());
            capitalize = false;
        } else {
            out.push(character);
        }
    }
    Some(out)
}

/// The member a key of the metadata names: the last level of the key.
fn member_of(key: &str) -> Option<String> {
    member_name(key.rsplit('.').next()?)
}

fn capitalized(name: &str) -> String {
    let mut characters = name.chars();
    let Some(first) = characters.next() else {
        return String::new();
    };
    first.to_uppercase().collect::<String>() + characters.as_str()
}

fn collect_java_sources(
    folder: &Path,
    out: &mut Vec<(PathBuf, java_syntax::SpringFile)>,
    seen: &mut usize,
) {
    if *seen >= MAX_PLACEHOLDER_FILES {
        return;
    }
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        if *seen >= MAX_PLACEHOLDER_FILES {
            return;
        }
        let path = entry.path();
        if path.is_dir() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if matches!(
                name.as_ref(),
                "target" | "build" | "out" | ".git" | ".gradle"
            ) {
                continue;
            }
            collect_java_sources(&path, out, seen);
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
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        out.push((path, java_syntax::spring_file(&text)));
    }
}

/// The key without the profile prefix of Quarkus, `%dev.quarkus.http.port` giving
/// `quarkus.http.port`.
fn without_profile(key: &str) -> &str {
    let Some(rest) = key.strip_prefix('%') else {
        return key;
    };
    rest.split_once('.').map_or(key, |(_, key)| key)
}

/// The profile prefix of a key, with the dot that joins it to the key.
fn profile_of(key: &str) -> Option<&str> {
    let rest = key.strip_prefix('%')?;
    let (profile, _) = rest.split_once('.')?;
    Some(&key[..profile.len() + 2])
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

/// Every `${...}` of a text with the name it reads and the range of that name. The range
/// covers the name alone, not the braces or the default value.
fn placeholder_places(text: &str) -> Vec<(String, Range<usize>)> {
    let mut found = Vec::new();
    let mut at = 0;
    while let Some(start) = text[at..].find("${") {
        let start = at + start;
        let Some(end) = text[start + 2..].find('}') else {
            break;
        };
        let inner = &text[start + 2..start + 2 + end];
        if let Some(name) = placeholder_name(inner) {
            let raw = inner.split(':').next().unwrap_or(inner);
            let lead = raw.len() - raw.trim_start().len();
            let name_start = start + 2 + lead;
            found.push((name, name_start..name_start + raw.trim().len()));
        }
        at = start + 2 + end + 1;
    }
    found
}

/// The name of the `${...}` that holds an offset.
fn placeholder_at(text: &str, offset: usize) -> Option<String> {
    placeholder_places(text)
        .into_iter()
        .find(|(_, range)| offset >= range.start && offset <= range.end)
        .map(|(name, _)| name)
}

/// The places the `${name}` of a Java source reads, for the usages of a key.
fn placeholder_spans_in_sources(sources: &[PathBuf], key: &str) -> Vec<SourceSpan> {
    let mut found = Vec::new();
    let mut seen = 0;
    for root in sources {
        collect_placeholder_spans(root, key, &mut found, &mut seen);
        if seen >= MAX_PLACEHOLDER_FILES {
            break;
        }
    }
    found
}

fn collect_placeholder_spans(root: &Path, key: &str, out: &mut Vec<SourceSpan>, seen: &mut usize) {
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
            collect_placeholder_spans(&path, key, out, seen);
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
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let places: Vec<Range<usize>> = placeholder_places(&text)
            .into_iter()
            .filter(|(name, _)| name == key)
            .map(|(_, range)| range)
            .collect();
        if places.is_empty() {
            continue;
        }
        let Ok(canonical) = CanonPath::new(&path) else {
            continue;
        };
        let Some(uri) = file_uri(&canonical) else {
            continue;
        };
        let uri = uri.as_str().to_owned();
        for range in places {
            out.push(SourceSpan {
                uri: uri.clone(),
                span: span_in(&text, range.start, range.end),
            });
        }
    }
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
    let profile = profile_of(&flat).unwrap_or("");
    let lookup = without_profile(&flat).to_owned();
    let filter_from = lookup.rfind('.').map(|index| index + 1).unwrap_or(0);
    let span = Some(span_in(text, context.key_start, offset));
    let mut entries = Vec::new();
    for (name, property) in &metadata.properties {
        if name.len() <= lookup.len() || !name.starts_with(&lookup) {
            continue;
        }
        let label = name[filter_from..].to_owned();
        let insert = match kind {
            ConfigKind::Properties => format!("{profile}{name}"),
            ConfigKind::Yaml => {
                if context.has_separator {
                    format!("{profile}{name}")
                } else {
                    yaml_insertion(text, &context, &format!("{profile}{name}"))
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
            documentation: None,
        });
    }
    for prefix in &metadata.prefixes {
        if prefix.len() <= lookup.len()
            || !prefix.starts_with(&lookup)
            || filter_from > prefix.len()
        {
            continue;
        }
        let label = format!("{}.", &prefix[filter_from..]);
        entries.push(CompletionEntry {
            label: label.clone(),
            label_details: None,
            detail: Some("prefix".to_owned()),
            filter_text: Some(label),
            sort_text: Some(format!("1{prefix}")),
            insert_text: Some(format!("{profile}{prefix}.")),
            span,
            snippet: false,
            additional_edits: Vec::new(),
            command: None,
            resolve: None,
            kind: CompletionKind::Property,
            deprecated: false,
            documentation: None,
        });
    }
    entries.sort_by(|left, right| left.sort_text.cmp(&right.sort_text));
    entries
}

/// The keys an annotation of a Java source offers at the caret of its key. The entry
/// replaces the text before the caret and inserts the whole key.
fn annotation_key_entries(
    text: &str,
    offset: usize,
    snapshot: &Snapshot,
) -> Option<Vec<CompletionEntry>> {
    let found = java_syntax::config_key_at(text, offset)?;
    let typed_end = offset.clamp(found.range.start, found.range.end);
    let typed_len = (typed_end - found.range.start).min(found.key.len());
    let typed = &found.key[..typed_len];
    let metadata = &snapshot.metadata;
    if metadata.is_empty() {
        return None;
    }
    let filter_from = typed.rfind('.').map(|index| index + 1).unwrap_or(0);
    let span = Some(span_in(text, found.range.start, typed_end));
    let mut entries = Vec::new();
    for (name, property) in &metadata.properties {
        if !name.starts_with(typed) || name == typed {
            continue;
        }
        let label = name[filter_from..].to_owned();
        entries.push(CompletionEntry {
            label: label.clone(),
            label_details: Some(CompletionLabelDetails {
                detail: None,
                description: property.description.as_deref().map(short_description),
            }),
            detail: property.detail(),
            filter_text: Some(label),
            sort_text: Some(format!("0{name}")),
            insert_text: Some(name.clone()),
            span,
            snippet: false,
            additional_edits: Vec::new(),
            command: None,
            resolve: None,
            kind: CompletionKind::Property,
            deprecated: property.deprecated(),
            documentation: None,
        });
    }
    for prefix in &metadata.prefixes {
        if !prefix.starts_with(typed) || prefix == typed || filter_from > prefix.len() {
            continue;
        }
        let label = format!("{}.", &prefix[filter_from..]);
        entries.push(CompletionEntry {
            label: label.clone(),
            label_details: None,
            detail: Some("prefix".to_owned()),
            filter_text: Some(label),
            sort_text: Some(format!("1{prefix}")),
            insert_text: Some(format!("{prefix}.")),
            span,
            snippet: false,
            additional_edits: Vec::new(),
            command: None,
            resolve: None,
            kind: CompletionKind::Property,
            deprecated: false,
            documentation: None,
        });
    }
    entries.sort_by(|left, right| left.sort_text.cmp(&right.sort_text));
    Some(entries)
}

/// The places a key is written in the configuration files of the resource roots, the key
/// itself being the place a jump underlines. A key of a profile file belongs to the key
/// without the profile prefix of Quarkus.
fn key_locations(key: &str, resources: &[PathBuf]) -> Vec<SourceSpan> {
    let wanted = without_profile(key);
    let mut found = Vec::new();
    for root in resources {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        let mut files: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| config_kind(path).is_some())
            .collect();
        files.sort();
        for path in files {
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let mut places = Vec::new();
            match config_kind(&path) {
                Some(ConfigKind::Properties) => {
                    for (name, range, _) in properties_entries(&text) {
                        if without_profile(&name) == wanted {
                            places.push(range);
                        }
                    }
                }
                Some(ConfigKind::Yaml) => {
                    for pair in yaml_pairs(&text) {
                        if without_profile(&pair.full_key) == wanted {
                            places.push(pair.key);
                        }
                    }
                }
                None => {}
            }
            if places.is_empty() {
                continue;
            }
            let Ok(canonical) = CanonPath::new(&path) else {
                continue;
            };
            let Some(uri) = file_uri(&canonical) else {
                continue;
            };
            let uri = uri.as_str().to_owned();
            for range in places {
                found.push(SourceSpan {
                    uri: uri.clone(),
                    span: span_in(&text, range.start, range.end),
                });
            }
        }
    }
    found
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
            documentation: None,
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

fn diagnostics_for(
    text: &str,
    kind: ConfigKind,
    snapshot: &Snapshot,
    checks: &lang_api::DisabledInspections,
) -> Vec<DiagnosticEntry> {
    // A check that is off costs nothing: the metadata is not read for it.
    if !checks.any_enabled(SPRING_CHECKS) {
        return Vec::new();
    }
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
        let lookup = without_profile(&key);
        let exact = metadata.properties.get(lookup);
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
                code: Some("spring.config.deprecatedProperty".to_owned()),
            });
        } else if exact.is_none()
            && !metadata.knows(lookup)
            && !metadata.under_map(lookup)
            && !metadata.under_known_prefix(lookup)
            && !local.contains(&key)
            && !local.contains(lookup)
            && !snapshot.placeholders.contains(lookup)
            && lookup.contains('.')
            && let Some((code, label)) = unknown_key(lookup, metadata)
        {
            found.push(DiagnosticEntry {
                span: span_in(text, key_range.start, key_range.end),
                severity: Severity::Warning,
                message: format!("Unknown {label} property `{key}`"),
                unnecessary: false,
                code: Some(code.to_owned()),
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
                    code: Some("spring.config.invalidValue".to_owned()),
                });
            }
        } else if let Some(allowed) = enum_values(metadata, lookup, property)
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
                code: Some("spring.config.invalidValue".to_owned()),
            });
        }
    }
    found.retain(|entry| {
        entry
            .code
            .as_deref()
            .is_none_or(|code| checks.enabled(code))
    });
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

/// The check and the word of the framework an unknown key belongs to. A key of a
/// framework the metadata knows nothing about is left alone: the project may use the
/// framework without its jars being on the class path yet.
fn unknown_key(key: &str, metadata: &Metadata) -> Option<(&'static str, &'static str)> {
    let root = key.split('.').next().unwrap_or(key);
    if root == "quarkus" && !metadata.knows_root("quarkus") {
        return None;
    }
    if root == "micronaut" && !metadata.knows_root("micronaut") {
        return None;
    }
    if metadata.knows_root("micronaut") {
        Some(("micronaut.config.unknownProperty", "Micronaut"))
    } else if metadata.knows_root("quarkus") {
        Some(("quarkus.config.unknownProperty", "Quarkus"))
    } else if metadata.is_empty() {
        None
    } else {
        Some(("spring.config.unknownProperty", "Spring Boot"))
    }
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
    let key = without_profile(key);
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

/// The place a key of the metadata leads to: the member of its configuration class that
/// declares the key, or the declaration of the class when the member is not found.
fn source_span(snapshot: &Snapshot, key: &str, sources: &[PathBuf]) -> Option<SourceSpan> {
    let key = without_profile(key);
    let metadata = &snapshot.metadata;
    let (source_type, member, source_method) =
        if let Some(property) = metadata.navigation_property(key) {
            (
                property.source_type.as_deref(),
                member_of(&property.name),
                property.source_method.as_deref(),
            )
        } else {
            (
                metadata
                    .groups
                    .get(key)
                    .and_then(|group| group.source_type.as_deref()),
                None,
                None,
            )
        };
    let source_type = source_type?;
    let path = source_file(source_type, sources)?;
    let text = std::fs::read_to_string(&path).ok()?;
    let file = java_syntax::spring_file(&text);
    let kind = type_in_file(&file, source_type);
    let source = ParsedSource {
        file: &file,
        path: &path,
        text: &text,
    };
    let found = kind.and_then(|kind| {
        member_in_supertypes(kind, &source, member.as_deref(), source_method, sources, 0)
    });
    let (path, span) = match found {
        Some(found) => found,
        None => (
            path,
            kind.map(|kind| type_span(kind, &text)).unwrap_or_default(),
        ),
    };
    let canonical = CanonPath::new(&path).ok()?;
    let uri = file_uri(&canonical)?;
    Some(SourceSpan {
        uri: uri.as_str().to_owned(),
        span,
    })
}

/// A parsed source file and where it lives.
struct ParsedSource<'a> {
    file: &'a java_syntax::SpringFile,
    path: &'a Path,
    text: &'a str,
}

/// The member of a configuration class or of a superclass of the project, with the file
/// that declares it. A member the class does not declare is looked for in the types it
/// extends, up to [`MAX_SUPERTYPE_DEPTH`] levels above it.
fn member_in_supertypes(
    kind: &java_syntax::Type,
    source: &ParsedSource<'_>,
    member: Option<&str>,
    source_method: Option<&str>,
    sources: &[PathBuf],
    depth: usize,
) -> Option<(PathBuf, TextSpan)> {
    if let Some(span) = member_span(kind, member, source_method, source.text) {
        return Some((source.path.to_path_buf(), span));
    }
    let member = member?;
    if depth >= MAX_SUPERTYPE_DEPTH {
        return None;
    }
    for super_name in &kind.supers {
        let Some((super_path, super_text)) = super_source(super_name, source.file, sources) else {
            continue;
        };
        let super_file = java_syntax::spring_file(&super_text);
        let Some(super_kind) = type_in_file(&super_file, super_name) else {
            continue;
        };
        let super_source = ParsedSource {
            file: &super_file,
            path: &super_path,
            text: &super_text,
        };
        if let Some(found) = member_in_supertypes(
            super_kind,
            &super_source,
            Some(member),
            source_method,
            sources,
            depth + 1,
        ) {
            return Some(found);
        }
    }
    None
}

/// The source file and text of a supertype a declaration names. The package and the
/// imports of the file that extends it give the name to look for.
fn super_source(
    super_name: &str,
    file: &java_syntax::SpringFile,
    sources: &[PathBuf],
) -> Option<(PathBuf, String)> {
    let mut candidates = Vec::new();
    if super_name.contains('.') {
        candidates.push(super_name.to_owned());
    }
    let imports = file.imports();
    if let Some(package) = imports.get(super_name) {
        candidates.push(format!("{package}.{super_name}"));
    }
    if !file.package.is_empty() {
        candidates.push(format!("{}.{}", file.package, super_name));
    }
    for (name, package) in &file.imports {
        if name == "*" {
            candidates.push(format!("{package}.{super_name}"));
        }
    }
    for candidate in candidates {
        let Some(path) = source_file(&candidate, sources) else {
            continue;
        };
        let text = std::fs::read_to_string(&path).ok()?;
        return Some((path, text));
    }
    None
}

/// The places a key leads to: its lines in the configuration files, or the member of the
/// class that declares it when no file has the key.
fn key_navigation(
    snapshot: &Snapshot,
    key: &str,
    sources: &[PathBuf],
    resources: &[PathBuf],
) -> Vec<SourceSpan> {
    let mut found = key_locations(key, resources);
    if found.is_empty() {
        found.extend(source_span(snapshot, key, sources));
    }
    found
}

/// The type of a parsed file the `sourceType` names. A nested type lives in the file of
/// its outer type, and the simple name is the fallback for a file the package moved in.
fn type_in_file<'a>(
    file: &'a java_syntax::SpringFile,
    source_type: &str,
) -> Option<&'a java_syntax::Type> {
    let name = source_type.split('<').next().unwrap_or(source_type).trim();
    file.types
        .iter()
        .find(|kind| kind.qualified == name)
        .or_else(|| {
            let simple = name.rsplit('.').next().unwrap_or(name);
            file.types.iter().find(|kind| kind.name == simple)
        })
}

/// The declaration of the member a key names: the field or the record component first,
/// then the method the metadata names, the setter, the getter and a method of that name.
fn member_span(
    kind: &java_syntax::Type,
    name: Option<&str>,
    source_method: Option<&str>,
    text: &str,
) -> Option<TextSpan> {
    let name = name?;
    let field = kind
        .fields
        .iter()
        .chain(kind.components.iter())
        .find(|field| field.name == name);
    if let Some(field) = field {
        return Some(span_in(text, field.offset, field.offset + field.name.len()));
    }
    if let Some(method) =
        source_method.and_then(|method| kind.methods.iter().find(|found| found.name == method))
    {
        return Some(span_in(
            text,
            method.offset,
            method.offset + method.name.len(),
        ));
    }
    let capitalized = capitalized(name);
    let candidates = [
        format!("set{capitalized}"),
        format!("get{capitalized}"),
        format!("is{capitalized}"),
        name.to_owned(),
    ];
    for candidate in &candidates {
        if let Some(method) = kind.methods.iter().find(|method| method.name == *candidate) {
            return Some(span_in(
                text,
                method.offset,
                method.offset + method.name.len(),
            ));
        }
    }
    None
}

/// The place of the name in the declaration of a type.
fn type_span(kind: &java_syntax::Type, text: &str) -> TextSpan {
    span_in(text, kind.offset, kind.offset + kind.name.len())
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

    fn references(&self) -> Option<&lang_api::ReferencesProvider> {
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

    /// The metadata of the project, waited for off the interface thread.
    fn ready_snapshot(&self) -> Arc<Snapshot> {
        let generation = self.generation();
        let mut state = self.lock_state();
        while state.loaded < generation {
            let (next, _) = self
                .inner
                .ready
                .wait_timeout(state, READY_STEP)
                .unwrap_or_else(|poison| poison.into_inner());
            state = next;
        }
        drop(state);
        self.snapshot()
    }

    /// The keys a `@ConfigProperty`, `@Property` or `@Value` of a Java source offers at
    /// the caret of its key.
    pub(crate) fn annotation_completion(
        &self,
        text: &str,
        offset: usize,
    ) -> Option<CompletionList> {
        let snapshot = self.ready_snapshot();
        let entries = annotation_key_entries(text, offset, &snapshot)?;
        Some(CompletionList {
            entries,
            complete: true,
        })
    }

    /// The places a key of an annotation leads to: its lines in the configuration files,
    /// or the member of the class that declares it.
    pub fn annotation_navigation(&self, text: &str, offset: usize) -> Option<Vec<SourceSpan>> {
        let found = java_syntax::config_key_at(text, offset)?;
        let snapshot = self.ready_snapshot();
        let (sources, resources) = {
            let state = self.lock_state();
            (state.paths.sources.clone(), state.paths.resources.clone())
        };
        let spans = key_navigation(&snapshot, &found.key, &sources, &resources);
        (!spans.is_empty()).then_some(spans)
    }

    /// The documentation of the key an annotation names.
    pub(crate) fn annotation_documentation(&self, text: &str, offset: usize) -> Option<String> {
        let found = java_syntax::config_key_at(text, offset)?;
        let snapshot = self.ready_snapshot();
        documentation(&snapshot, &found.key)
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
            let (sources, resources) = {
                let state = support.lock_state();
                (state.paths.sources.clone(), state.paths.resources.clone())
            };
            let spans = support
                .kind_of(&query.spot.uri)
                .zip(support.text_of(&query.spot.uri))
                .map(|(kind, text)| {
                    let offset = offset_in(&text, query.spot.point);
                    // A `${...}` of a value names another key of the files.
                    if let Some(key) = placeholder_at(&text, offset) {
                        key_navigation(&snapshot, &key, &sources, &resources)
                    } else if let Some(key) = key_at(&text, offset, kind) {
                        source_span(&snapshot, &key, &sources).into_iter().collect()
                    } else {
                        Vec::new()
                    }
                })
                .unwrap_or_default();
            let _ = reply.send(Answer::new(spans, Confidence::Certain, SOURCE));
        });
    }
}

/// The `${name}` of the Java sources that read the key under the caret. Find Usages of a
/// key in a configuration file lists them.
impl Provider<DocumentSpot, Vec<SourceSpan>> for SpringConfigSupport {
    fn start(&self, query: DocumentSpot, _: Cancel, mut reply: Reply<Vec<SourceSpan>>) {
        let support = self.clone();
        thread::spawn(move || {
            let spans = support
                .kind_of(&query.uri)
                .zip(support.text_of(&query.uri))
                .map(|(kind, text)| {
                    let offset = offset_in(&text, query.point);
                    let key = placeholder_at(&text, offset)
                        .or_else(|| key_at(&text, offset, kind))
                        .unwrap_or_default();
                    if key.is_empty() {
                        return Vec::new();
                    }
                    let sources = support.lock_state().paths.sources.clone();
                    placeholder_spans_in_sources(&sources, &key)
                })
                .unwrap_or_default();
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
        let checks = support.inspection_checks();
        let entries = text
            .as_deref()
            .map(|text| diagnostics_for(text, kind, &snapshot, &checks))
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
            &lang_api::DisabledInspections::default(),
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
    fn a_check_that_is_off_reports_nothing_and_one_that_is_on_stays() {
        let snapshot = snapshot(SAMPLE);
        let text = "spring.jpa.open-in-view=true\nspring.jpa.unknown=1\n";
        let enabled = lang_api::DisabledInspections::default();
        let found = diagnostics_for(text, ConfigKind::Properties, &snapshot, &enabled);
        assert_eq!(found.len(), 1, "{found:?}");

        let mut settings = lang_api::InspectionSettings::default();
        settings.set("spring.config.unknownProperty", false, None);
        let checks = lang_api::DisabledInspections::default();
        checks.replace(&settings);
        let found = diagnostics_for(text, ConfigKind::Properties, &snapshot, &checks);
        assert!(found.is_empty(), "{found:?}");

        settings.set("spring.config.unknownProperty", true, None);
        settings.set("spring.config.deprecatedProperty", false, None);
        settings.set("spring.config.invalidValue", false, None);
        checks.replace(&settings);
        assert!(
            checks.any_enabled(SPRING_CHECKS),
            "a check that is on keeps the metadata read"
        );
    }

    #[test]
    fn a_placeholder_of_the_same_file_hides_its_key() {
        let snapshot = snapshot(SAMPLE);
        let found = diagnostics_for(
            "database=h2\nspring.jpa.open-in-view=${database}\n",
            ConfigKind::Properties,
            &snapshot,
            &lang_api::DisabledInspections::default(),
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
            &lang_api::DisabledInspections::default(),
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
        let text = "package org.example;\nclass JpaProperties {}\n";
        std::fs::write(&file, text).unwrap();
        let snapshot = snapshot(SAMPLE);
        let span = source_span(&snapshot, "spring.jpa.open-in-view", &[root]).unwrap();
        assert!(span.uri.ends_with("JpaProperties.java"), "{}", span.uri);
        assert_eq!(
            span.span.start,
            point_in(text, text.find("JpaProperties").unwrap()),
            "a key whose member is not found leads to the declaration of the class"
        );
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

    const MEMBERS: &str = r#"{
        "properties": [
            {
                "name": "server.port",
                "type": "java.lang.Integer",
                "sourceType": "com.example.ServerProperties"
            },
            {
                "name": "shop.max-connections",
                "type": "java.lang.Integer",
                "sourceType": "com.example.Limits"
            },
            {
                "name": "spring.datasource.hikari.maximum-pool-size",
                "type": "java.lang.Integer",
                "sourceType": "com.example.HikariConfig"
            }
        ]
    }"#;

    fn write_source(root: &Path, relative: &str, text: &str) -> PathBuf {
        let file = root.join(relative);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, text).unwrap();
        file
    }

    #[test]
    fn a_key_leads_to_the_field_of_its_source_type() {
        let folder = test_support::TempDir::new("spring-member");
        let root = folder.path().join("src").join("main").join("java");
        let server = "package com.example;\n\nclass ServerProperties {\n    private int port;\n}\n";
        write_source(&root, "com/example/ServerProperties.java", server);
        let snapshot = snapshot(MEMBERS);
        let span = source_span(&snapshot, "server.port", &[root]).unwrap();
        assert!(span.uri.ends_with("ServerProperties.java"), "{}", span.uri);
        assert_eq!(
            span.span.start,
            point_in(server, server.find("port;").unwrap()),
            "the caret goes to the field"
        );
        assert_eq!(
            span.span.end.character - span.span.start.character,
            "port".len() as u32,
            "the span is the name of the field"
        );
    }

    #[test]
    fn a_relaxed_key_names_the_same_field() {
        let folder = test_support::TempDir::new("spring-relaxed");
        let root = folder.path().join("src").join("main").join("java");
        let limits = "package com.example;\n\nclass Limits {\n    private int maxConnections;\n}\n";
        write_source(&root, "com/example/Limits.java", limits);
        let snapshot = snapshot(MEMBERS);
        let expected = point_in(limits, limits.find("maxConnections").unwrap());
        for key in [
            "shop.max-connections",
            "shop.maxConnections",
            "shop.max_connections",
        ] {
            let span = source_span(&snapshot, key, std::slice::from_ref(&root))
                .unwrap_or_else(|| panic!("the key {key} leads to its field"));
            assert_eq!(span.span.start, expected, "the key {key}");
        }
    }

    #[test]
    fn a_nested_key_leads_to_the_field_of_its_nested_type() {
        let folder = test_support::TempDir::new("spring-nested");
        let root = folder.path().join("src").join("main").join("java");
        let hikari =
            "package com.example;\n\nclass HikariConfig {\n    private int maximumPoolSize;\n}\n";
        write_source(&root, "com/example/HikariConfig.java", hikari);
        let snapshot = snapshot(MEMBERS);
        let span = source_span(
            &snapshot,
            "spring.datasource.hikari.maximum-pool-size",
            &[root],
        )
        .unwrap();
        assert_eq!(
            span.span.start,
            point_in(hikari, hikari.find("maximumPoolSize").unwrap())
        );
    }

    #[test]
    fn a_key_without_a_field_leads_to_its_setter() {
        let folder = test_support::TempDir::new("spring-setter");
        let root = folder.path().join("src").join("main").join("java");
        let server =
            "package com.example;\n\nclass ServerProperties {\n    void setPort(int port) {}\n}\n";
        write_source(&root, "com/example/ServerProperties.java", server);
        let snapshot = snapshot(MEMBERS);
        let span = source_span(&snapshot, "server.port", &[root]).unwrap();
        assert_eq!(
            span.span.start,
            point_in(server, server.find("setPort").unwrap())
        );
    }

    #[test]
    fn a_key_of_an_inherited_field_leads_to_the_superclass_field() {
        let folder = test_support::TempDir::new("spring-inherited");
        let root = folder.path().join("src").join("main").join("java");
        let base = "package com.example;\n\nclass BaseProperties {\n    private int port;\n}\n";
        let child = "package com.example;\n\nclass ServerProperties extends BaseProperties {\n}\n";
        write_source(&root, "com/example/BaseProperties.java", base);
        write_source(&root, "com/example/ServerProperties.java", child);
        let snapshot = snapshot(MEMBERS);
        let span = source_span(&snapshot, "server.port", &[root]).unwrap();
        assert!(
            span.uri.ends_with("BaseProperties.java"),
            "the field is in the superclass: {}",
            span.uri
        );
        assert_eq!(
            span.span.start,
            point_in(base, base.find("port;").unwrap()),
            "the caret goes to the inherited field"
        );
        assert_eq!(
            span.span.end.character - span.span.start.character,
            "port".len() as u32,
            "the span is the name of the field"
        );
    }

    #[test]
    fn a_key_without_a_file_leads_to_the_field_of_the_metadata() {
        let folder = test_support::TempDir::new("spring-key-fallback");
        let root = folder.path().join("src").join("main").join("java");
        let server = "package com.example;\n\nclass ServerProperties {\n    private int port;\n}\n";
        write_source(&root, "com/example/ServerProperties.java", server);
        let snapshot = snapshot(MEMBERS);
        let spans = key_navigation(&snapshot, "server.port", &[root], &[]);
        assert_eq!(spans.len(), 1, "the metadata names the class of the key");
        assert_eq!(
            spans[0].span.start,
            point_in(server, server.find("port;").unwrap())
        );
    }

    #[test]
    fn a_key_of_a_record_leads_to_its_component() {
        let folder = test_support::TempDir::new("spring-record");
        let root = folder.path().join("src").join("main").join("java");
        let record = "package com.example;\n\nrecord ServerProperties(int port) {}\n";
        write_source(&root, "com/example/ServerProperties.java", record);
        let snapshot = snapshot(MEMBERS);
        let span = source_span(&snapshot, "server.port", &[root]).unwrap();
        assert_eq!(
            span.span.start,
            point_in(record, record.find("int port").unwrap() + "int ".len())
        );
    }

    #[test]
    fn a_placeholder_name_drops_its_default() {
        let text = "url=${a.b:1}\n";
        let inside = text.find("a.b").unwrap() + 1;
        assert_eq!(placeholder_at(text, inside).as_deref(), Some("a.b"));

        let text = "urls=${server.port:8080},${server.port}\n";
        let places = placeholder_places(text);
        assert_eq!(places.len(), 2);
        for (name, range) in places {
            assert_eq!(name, "server.port");
            assert_eq!(&text[range], "server.port");
        }
    }

    #[test]
    fn a_key_in_two_profile_files_gives_two_places() {
        let folder = test_support::TempDir::new("spring-profiles");
        let root = folder.path().join("src").join("main").join("resources");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("application.properties"),
            "spring.application.name=demo\n",
        )
        .unwrap();
        std::fs::write(
            root.join("application-dev.properties"),
            "server.port=8081\n",
        )
        .unwrap();
        std::fs::write(
            root.join("application-prod.properties"),
            "server.port=8082\n",
        )
        .unwrap();
        let places = key_locations("server.port", &[root]);
        assert_eq!(places.len(), 2, "one place in each profile file");
        assert_ne!(places[0].uri, places[1].uri);
        assert_eq!(places[0].span.start.line, 0);
        assert_eq!(places[1].span.start.line, 0);
    }

    #[test]
    fn the_usages_of_a_key_are_its_value_placeholders() {
        let folder = test_support::TempDir::new("spring-usages");
        let root = folder.path().join("src").join("main").join("java");
        let source = "package com.example;\n\nclass Shop {\n    @Value(\"${server.port}\")\n    String port;\n    @Value(\"${server.port:8080}\")\n    String alternate;\n}\n";
        write_source(&root, "com/example/Shop.java", source);
        let spans = placeholder_spans_in_sources(&[root], "server.port");
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].span.start.line, 3);
        assert_eq!(spans[1].span.start.line, 5);
    }

    #[test]
    fn a_value_annotation_leads_to_the_key_in_the_configuration_file() {
        let folder = test_support::TempDir::new("spring-annotation-nav");
        let resources = folder.path().join("src").join("main").join("resources");
        std::fs::create_dir_all(&resources).unwrap();
        std::fs::write(
            resources.join("application.properties"),
            "server.port=8080\n",
        )
        .unwrap();
        let sources = folder.path().join("src").join("main").join("java");
        let support = spring_config(folder.path().to_path_buf());
        support.set_sources(vec![sources]);
        support.set_resources(vec![resources]);
        let text = "package com.example;\n\nclass Shop {\n    @Value(\"${server.port}\")\n    int port;\n}\n";
        let offset = text.find("server.port").unwrap() + 2;
        let spans = support
            .annotation_navigation(text, offset)
            .expect("the key is in the file");
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].span.start.line, 0);
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

    #[test]
    fn a_micronaut_profile_file_is_a_configuration_file() {
        assert_eq!(
            config_kind(Path::new("C:/p/src/main/resources/application-dev.yml")),
            Some(ConfigKind::Yaml)
        );
        assert_eq!(
            config_kind(Path::new(
                "C:/p/src/main/resources/application-prod.properties"
            )),
            Some(ConfigKind::Properties)
        );
    }

    const QUARKUS: &str = r#"{
        "properties": [
            {
                "name": "quarkus.http.port",
                "type": "java.lang.Integer",
                "defaultValue": 8080,
                "description": "The HTTP port."
            },
            {
                "name": "quarkus.http.host",
                "type": "java.lang.String",
                "defaultValue": "0.0.0.0"
            },
            {
                "name": "quarkus.http.ssl-port",
                "type": "java.lang.Integer",
                "defaultValue": 8443
            }
        ]
    }"#;

    const MICRONAUT: &str = r#"{
        "micronaut.server.port": {
            "type": "java.lang.Integer",
            "description": "The port of the server.",
            "defaultValue": 8080
        },
        "micronaut.server.host": {
            "type": "java.lang.String"
        }
    }"#;

    fn inserted(entries: &[CompletionEntry]) -> Vec<&str> {
        entries
            .iter()
            .filter_map(|entry| entry.insert_text.as_deref())
            .collect()
    }

    #[test]
    fn a_quarkus_profile_key_is_completed_and_its_unknown_sibling_warns() {
        let snapshot = snapshot(QUARKUS);
        let text = "%dev.quarkus.http.";
        let entries = completion_entries(text, text.len(), ConfigKind::Properties, &snapshot);
        let keys = inserted(&entries);
        assert!(keys.contains(&"%dev.quarkus.http.port"), "{keys:?}");
        assert!(keys.contains(&"%dev.quarkus.http.host"), "{keys:?}");

        let found = diagnostics_for(
            "%dev.quarkus.http.port=8081\n%dev.quarkus.unknown=1\n",
            ConfigKind::Properties,
            &snapshot,
            &lang_api::DisabledInspections::default(),
        );
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(
            found[0].code.as_deref(),
            Some("quarkus.config.unknownProperty")
        );
        assert_eq!(
            found[0].message,
            "Unknown Quarkus property `%dev.quarkus.unknown`"
        );
    }

    #[test]
    fn a_quarkus_extension_descriptor_names_the_keys_it_reads() {
        let parsed = parse_quarkus_extension(
            "name: \"RESTEasy Reactive\"\nmetadata:\n  config:\n    - \"quarkus.resteasy-reactive.\"\n    - \"quarkus.http.port\"\n",
        );
        assert_eq!(parsed.prefixes, ["quarkus.resteasy-reactive"]);
        assert_eq!(
            parsed
                .properties
                .iter()
                .map(|property| property.name.as_str())
                .collect::<Vec<_>>(),
            ["quarkus.http.port"]
        );
        let mut metadata = Metadata::default();
        metadata.add(parsed, true);
        assert!(metadata.knows_root("quarkus"));
        assert!(metadata.under_known_prefix("quarkus.resteasy-reactive.json"));
        assert!(!metadata.under_known_prefix("quarkus.http"));
    }

    #[test]
    fn the_flat_metadata_of_micronaut_is_read() {
        let snapshot = snapshot(MICRONAUT);
        let text = "micronaut.server.";
        let entries = completion_entries(text, text.len(), ConfigKind::Properties, &snapshot);
        let keys = inserted(&entries);
        assert!(keys.contains(&"micronaut.server.port"), "{keys:?}");
        let found = diagnostics_for(
            "micronaut.server.port=8081\nmicronaut.server.unknown=1\n",
            ConfigKind::Properties,
            &snapshot,
            &lang_api::DisabledInspections::default(),
        );
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(
            found[0].code.as_deref(),
            Some("micronaut.config.unknownProperty")
        );
        assert_eq!(
            found[0].message,
            "Unknown Micronaut property `micronaut.server.unknown`"
        );
    }

    #[test]
    fn a_configuration_class_of_the_project_declares_its_keys() {
        let root = test_support::TempDir::new("config-classes");
        let source = root.path().join("src/demo");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(
            source.join("ShopConfig.java"),
            r#"package demo;

import io.quarkus.runtime.annotations.ConfigMapping;

@ConfigMapping(prefix = "shop")
interface ShopConfig {
    int httpPort();
    String host();
}
"#,
        )
        .unwrap();
        std::fs::write(
            source.join("ServerProperties.java"),
            r#"package demo;

import io.micronaut.context.annotation.ConfigurationProperties;

@ConfigurationProperties("micronaut.server")
class ServerProperties {
    private int maxPort;
}
"#,
        )
        .unwrap();

        let parsed = config_properties_in_sources(&[root.path().to_path_buf()]);
        let mut names: Vec<&str> = parsed
            .properties
            .iter()
            .map(|property| property.name.as_str())
            .collect();
        names.sort();
        assert_eq!(
            names,
            ["micronaut.server.max-port", "shop.host", "shop.http-port"],
            "{names:?}"
        );
        let shop = parsed
            .properties
            .iter()
            .find(|property| property.name == "shop.host")
            .expect("the key of the mapping");
        assert_eq!(shop.source_type.as_deref(), Some("demo.ShopConfig"));
    }

    #[test]
    fn an_annotation_key_offers_the_keys_of_the_metadata() {
        let snapshot = snapshot(QUARKUS);
        let source =
            "class Shop {\n    @ConfigProperty(name = \"quarkus.http.\")\n    int port;\n}\n";
        let offset = source.find("quarkus.http.").unwrap() + "quarkus.http.".len();
        let entries = annotation_key_entries(source, offset, &snapshot).expect("the keys");
        let keys = inserted(&entries);
        assert!(keys.contains(&"quarkus.http.port"), "{keys:?}");

        let value = "class Shop {\n    @Value(\"${quarkus.http.}\")\n    int port;\n}\n";
        let offset = value.find("quarkus.http.").unwrap() + "quarkus.http.".len();
        let entries = annotation_key_entries(value, offset, &snapshot).expect("the keys");
        assert!(
            inserted(&entries).contains(&"quarkus.http.port"),
            "{entries:?}"
        );
    }

    #[test]
    fn a_key_of_an_annotation_is_found_in_its_configuration_file() {
        let root = test_support::TempDir::new("config-key-place");
        std::fs::write(
            root.path().join("application.properties"),
            "# the server\nquarkus.http.port=8081\n",
        )
        .unwrap();
        let places = key_locations("quarkus.http.port", &[root.path().to_path_buf()]);
        assert_eq!(places.len(), 1);
        assert!(places[0].uri.ends_with("application.properties"));
        assert_eq!(places[0].span.start.line, 1);
        assert_eq!(places[0].span.start.character, 0);

        std::fs::write(
            root.path().join("application.yml"),
            "quarkus:\n  http:\n    host: 0.0.0.0\n",
        )
        .unwrap();
        let places = key_locations("quarkus.http.host", &[root.path().to_path_buf()]);
        assert_eq!(places.len(), 1);
        assert!(places[0].uri.ends_with("application.yml"));
        assert_eq!(places[0].span.start.line, 2);
        assert_eq!(places[0].span.start.character, 4);
    }
}
