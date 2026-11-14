//! The dependency-injection model of a project, read natively from its sources: the
//! beans of the usual component annotations and factory methods, the repositories of
//! Spring Data, the points that receive a bean and the checks of those points.
//!
//! Reading runs in the background and the interface never waits on it: a reader thread
//! keeps the model of the current sources, and a query answers with an immutable
//! snapshot and then again after every rebuild. Every model is assembled from parsed
//! files that keep their parse until the text or the modification time changes, so an
//! edit costs one file, not the project.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::thread;
use std::time::{Duration, SystemTime};

use base::Cancel;
use lang_api::{
    Answer, Bean, BeanModel, BeanQuery, BeanWarning, Confidence, InjectionPlace, InjectionPoint,
    Provider, Reply, SourceId,
};
use lsp_client::lsp_types::Uri;
use lsp_client::uri::file_path;
use project_model::ProjectModel;

const SOURCE: SourceId = SourceId::new("spring-beans");
/// The annotations that declare a bean of a class.
const STEREOTYPES: &[&str] = &[
    "Component",
    "Service",
    "Repository",
    "Controller",
    "RestController",
    "Configuration",
];
/// The annotations that mark a field or a parameter as a point of an injection.
const INJECTIONS: &[&str] = &["Autowired", "Inject", "Resource"];
/// The annotation of a method that declares a bean in a configuration.
const FACTORY: &str = "Bean";
/// The annotation that narrows a point to the bean of a name.
const QUALIFIER: &str = "Qualifier";
/// The annotation that wins among the candidates of a type.
const PRIMARY: &str = "Primary";
/// The interfaces of the data repositories of the framework.
pub(crate) const DATA_REPOSITORIES: &[&str] = &[
    "Repository",
    "CrudRepository",
    "PagingAndSortingRepository",
    "JpaRepository",
    "JpaSpecificationExecutor",
    "MongoRepository",
    "ReactiveCrudRepository",
    "R2dbcRepository",
];
/// The most source files the reader walks.
const MAX_FILES: usize = 20_000;
/// A wait of a query for the reader of the sources.
const READY_STEP: Duration = Duration::from_millis(25);
/// A pause before a rebuild, so that a burst of edits rebuilds once.
const REBUILD_DELAY: Duration = Duration::from_millis(120);

/// The model of the beans of one project. Cloning it shares the index.
#[derive(Clone)]
pub struct BeanIndex {
    inner: Arc<Inner>,
}

/// An index that starts reading as soon as it is made: its own root first, then the
/// source roots of the project model.
pub fn bean_index(root: PathBuf) -> Arc<BeanIndex> {
    let index = Arc::new(BeanIndex {
        inner: Arc::new(Inner::new(root)),
    });
    {
        let mut state = index.lock_state();
        state.generation = 1;
        state.loading = true;
    }
    start_reader(index.inner.clone());
    index
}

impl BeanIndex {
    /// The source roots of the project, from its model. A model that names the same
    /// roots as the previous one starts no new read.
    pub fn set_model(&self, model: &ProjectModel) {
        self.set_sources(sources_of(model));
    }

    /// The roots the sources are read from, for a caller that has no model.
    pub fn set_sources(&self, sources: Vec<PathBuf>) {
        let mut state = self.lock_state();
        if state.sources == sources {
            return;
        }
        state.sources = sources;
        drop(state);
        self.bump();
    }

    /// The text of an open document, so that the model follows an unsaved editor. `None`
    /// forgets the document when its tab closes.
    pub fn document_changed(&self, path: &Path, text: Option<Arc<str>>) {
        let mut state = self.lock_state();
        let changed = match text {
            Some(text) => match state.open.get(path) {
                Some(open) if open.text == text => false,
                _ => {
                    let version = state.version + 1;
                    state
                        .open
                        .insert(path.to_path_buf(), OpenDocument { version, text });
                    state.version = version;
                    true
                }
            },
            None => state.open.remove(path).is_some(),
        };
        drop(state);
        if changed {
            self.bump();
        }
    }

    /// The sources of the project changed: the model is read again after the quiet.
    fn bump(&self) {
        let mut state = self.lock_state();
        state.generation += 1;
        if state.loading {
            return;
        }
        state.loading = true;
        drop(state);
        start_reader(self.inner.clone());
    }

    /// The text of an open document, as the server names it with a URI.
    pub fn document_uri_changed(&self, uri: &str, text: Option<Arc<str>>) {
        let Some(path) = Uri::from_str(uri).ok().as_ref().and_then(file_path) else {
            return;
        };
        self.document_changed(&path, text);
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

    fn snapshot(&self) -> Arc<BeanModel> {
        self.inner
            .snapshot
            .read()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }

    /// Waits on the caller's thread, never on the interface, for the model of
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

    /// Waits until the sources changed after the model of `generation` was sent.
    fn wait_change<T>(&self, generation: u64, reply: &Reply<T>) -> bool {
        let mut state = self.lock_state();
        while state.generation == generation {
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
}

struct Inner {
    root: PathBuf,
    state: Mutex<Load>,
    ready: Condvar,
    snapshot: RwLock<Arc<BeanModel>>,
}

impl Inner {
    fn new(root: PathBuf) -> Self {
        Self {
            root,
            state: Mutex::new(Load::default()),
            ready: Condvar::new(),
            snapshot: RwLock::new(Arc::new(BeanModel::default())),
        }
    }
}

#[derive(Default)]
struct Load {
    sources: Vec<PathBuf>,
    /// Grows with every change of the sources or of an open text; a snapshot is
    /// complete when the reader has caught up with it.
    generation: u64,
    loaded: u64,
    loading: bool,
    /// Grows with every open text, so a text keeps its parse while it stays the same.
    version: u64,
    cache: HashMap<PathBuf, Cached>,
    open: HashMap<PathBuf, OpenDocument>,
}

#[derive(Clone)]
struct OpenDocument {
    version: u64,
    text: Arc<str>,
}

#[derive(Clone)]
struct Cached {
    stamp: Stamp,
    parsed: Arc<java_syntax::SpringFile>,
}

#[derive(Clone, PartialEq, Eq)]
enum Stamp {
    /// The modification time and the size of a file on disk.
    Disk(Option<SystemTime>, u64),
    /// The version of an open text.
    Open(u64),
}

fn sources_of(model: &ProjectModel) -> Vec<PathBuf> {
    let mut sources = Vec::new();
    for module in &model.modules {
        for unit in &module.units {
            sources.extend(unit.source_roots.iter().cloned());
        }
    }
    sources.sort();
    sources.dedup();
    sources
}

fn start_reader(inner: Arc<Inner>) {
    thread::spawn(move || {
        loop {
            let stale = {
                let state = inner
                    .state
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner());
                state.loaded < state.generation
            };
            if !stale {
                inner
                    .state
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner())
                    .loading = false;
                return;
            }
            // A burst of edits waits for quiet, so that the model is assembled once.
            thread::sleep(REBUILD_DELAY);
            let (generation, sources, cache, open) = {
                let state = inner
                    .state
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner());
                (
                    state.generation,
                    state.sources.clone(),
                    state.cache.clone(),
                    state.open.clone(),
                )
            };
            // A project whose model has not arrived yet still reads its own folder.
            let sources = if sources.is_empty() {
                vec![inner.root.clone()]
            } else {
                sources
            };
            let (snapshot, cache) = read_model(&sources, &cache, &open);
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

fn read_model(
    sources: &[PathBuf],
    cache: &HashMap<PathBuf, Cached>,
    open: &HashMap<PathBuf, OpenDocument>,
) -> (BeanModel, HashMap<PathBuf, Cached>) {
    let mut paths = Vec::new();
    for root in sources {
        collect_sources(root, &mut paths);
        if paths.len() >= MAX_FILES {
            break;
        }
    }
    paths.sort();
    paths.dedup();
    let mut next = HashMap::new();
    let mut parsed = Vec::new();
    for path in paths {
        let (text, stamp) = match open.get(&path) {
            Some(document) => (document.text.clone(), Stamp::Open(document.version)),
            None => {
                let Ok(meta) = std::fs::metadata(&path) else {
                    continue;
                };
                let stamp = Stamp::Disk(meta.modified().ok(), meta.len());
                if let Some(found) = cache.get(&path)
                    && found.stamp == stamp
                {
                    next.insert(path.clone(), found.clone());
                    parsed.push((path, found.parsed.clone()));
                    continue;
                }
                let Ok(text) = std::fs::read_to_string(&path) else {
                    continue;
                };
                (Arc::from(text), stamp)
            }
        };
        let file = if let Some(found) = cache.get(&path)
            && found.stamp == stamp
        {
            found.parsed.clone()
        } else {
            Arc::new(java_syntax::spring_file(&text))
        };
        next.insert(
            path.clone(),
            Cached {
                stamp,
                parsed: file.clone(),
            },
        );
        parsed.push((path, file));
    }
    let files: Vec<(&Path, &java_syntax::SpringFile)> = parsed
        .iter()
        .map(|(path, file)| (path.as_path(), file.as_ref()))
        .collect();
    (build_model(&files), next)
}

fn collect_sources(folder: &Path, out: &mut Vec<PathBuf>) {
    if out.len() >= MAX_FILES {
        return;
    }
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        if out.len() >= MAX_FILES {
            return;
        }
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if matches!(
                name.as_ref(),
                "target" | "build" | "out" | ".git" | ".gradle" | "node_modules"
            ) {
                continue;
            }
            collect_sources(&path, out);
            continue;
        }
        if path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("java"))
        {
            out.push(path);
        }
    }
}

/// A file of the project with the declarations the model combines.
struct FileData<'a> {
    path: &'a Path,
    file: &'a java_syntax::SpringFile,
    imports: HashMap<&'a str, &'a str>,
}

impl<'a> FileData<'a> {
    fn new(path: &'a Path, file: &'a java_syntax::SpringFile) -> Self {
        Self {
            path,
            file,
            imports: file.imports(),
        }
    }

    /// The name of a type as the project knows it: an import first, then the package of
    /// the file, then a wildcard import, then the name as written.
    fn resolve(&self, raw: &str, types: &HashSet<String>) -> String {
        let raw = short_name(raw);
        if raw.is_empty() {
            return String::new();
        }
        if let Some(import) = self.imports.get(raw.as_str()) {
            return format!("{import}.{raw}");
        }
        if !self.file.package.is_empty() {
            let candidate = format!("{}.{raw}", self.file.package);
            if types.contains(&candidate) {
                return candidate;
            }
        }
        for (name, path) in &self.file.imports {
            if name == "*" {
                let candidate = format!("{path}.{raw}");
                if types.contains(&candidate) {
                    return candidate;
                }
            }
        }
        raw
    }
}

/// A type without its type arguments, its arrays and its package.
fn short_name(raw: &str) -> String {
    let mut out = String::new();
    let mut depth = 0usize;
    for character in raw.chars() {
        match character {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            _ if depth > 0 => {}
            '[' | ']' => {}
            _ => out.push(character),
        }
    }
    let out = out.trim();
    out.rsplit('.').next().unwrap_or(out).trim().to_owned()
}

/// Removes the package of a qualified type, for a message.
fn simple_type(qualified: &str) -> &str {
    qualified.rsplit('.').next().unwrap_or(qualified)
}

/// The annotation of a bean, when it declares one.
fn bean_annotation<'a>(
    annotations: &'a [java_syntax::Annotation],
    component_annotations: &HashSet<String>,
) -> Option<&'a java_syntax::Annotation> {
    annotations.iter().find(|annotation| {
        STEREOTYPES.contains(&annotation.name.as_str())
            || component_annotations.contains(&annotation.name)
    })
}

fn has(annotations: &[java_syntax::Annotation], name: &str) -> bool {
    annotations.iter().any(|annotation| annotation.name == name)
}

fn value_of<'a>(annotations: &'a [java_syntax::Annotation], name: &str) -> Option<&'a str> {
    annotations
        .iter()
        .find(|annotation| annotation.name == name)
        .and_then(|annotation| annotation.value.as_deref())
}

/// The name of a bean a declaration of a class gives: the value of its annotation or the
/// name of the type with a lower first letter.
fn bean_name(annotation: Option<&java_syntax::Annotation>, type_name: &str) -> String {
    annotation
        .and_then(|annotation| annotation.value.clone())
        .unwrap_or_else(|| decapitalize(type_name))
}

fn decapitalize(name: &str) -> String {
    let mut characters = name.chars();
    let Some(first) = characters.next() else {
        return String::new();
    };
    if first.is_uppercase() && name.chars().nth(1).is_some_and(|next| next.is_uppercase()) {
        // `URLHandler` keeps its name, as the rules of a bean name do.
        return name.to_owned();
    }
    let mut out = String::with_capacity(name.len());
    out.extend(first.to_lowercase());
    out.push_str(characters.as_str());
    out
}

/// Assembles the model of a project from its parsed sources.
pub fn build_model(files: &[(&Path, &java_syntax::SpringFile)]) -> BeanModel {
    let data: Vec<FileData<'_>> = files
        .iter()
        .map(|(path, file)| FileData::new(path, file))
        .collect();
    let names: HashSet<String> = data
        .iter()
        .flat_map(|file| file.file.types.iter().map(|kind| kind.qualified.clone()))
        .collect();
    // The supertypes are resolved with the imports and the package of their own file,
    // so that a subtype walk compares the names of the project.
    let type_names = &names;
    let types: HashMap<String, ProjectType> = data
        .iter()
        .flat_map(move |file| {
            file.file.types.iter().map(move |kind| {
                (
                    kind.qualified.clone(),
                    ProjectType {
                        supers: kind
                            .supers
                            .iter()
                            .map(|raw| file.resolve(raw, type_names))
                            .collect(),
                    },
                )
            })
        })
        .collect();
    // An annotation of the project that carries a stereotype marks a bean as well. A
    // chain of such annotations is followed to its end.
    let mut component_annotations: HashSet<String> = HashSet::new();
    let mut changed = true;
    while changed {
        changed = false;
        for file in &data {
            for kind in &file.file.types {
                if !kind.is_annotation || component_annotations.contains(&kind.name) {
                    continue;
                }
                let carries = kind.annotations.iter().any(|annotation| {
                    STEREOTYPES.contains(&annotation.name.as_str())
                        || component_annotations.contains(&annotation.name)
                });
                if carries && component_annotations.insert(kind.name.clone()) {
                    changed = true;
                }
            }
        }
    }

    let mut model = BeanModel::default();
    for file in &data {
        for kind in &file.file.types {
            if let Some(annotation) = bean_annotation(&kind.annotations, &component_annotations)
                && !kind.is_annotation
            {
                model.beans.push(Bean {
                    name: bean_name(Some(annotation), &kind.name),
                    type_name: kind.name.clone(),
                    qualified_type: kind.qualified.clone(),
                    stereotype: format!("@{}", annotation.name),
                    primary: has(&kind.annotations, PRIMARY),
                    file: file.path.to_path_buf(),
                    line: kind.line,
                    offset: kind.offset,
                    usages: Vec::new(),
                });
            } else if kind.is_interface && is_data_repository(kind, file, &names) {
                model.beans.push(Bean {
                    name: decapitalize(&kind.name),
                    type_name: kind.name.clone(),
                    qualified_type: kind.qualified.clone(),
                    stereotype: "Spring Data".to_owned(),
                    primary: false,
                    file: file.path.to_path_buf(),
                    line: kind.line,
                    offset: kind.offset,
                    usages: Vec::new(),
                });
            }
            let configuration =
                bean_annotation(&kind.annotations, &component_annotations).is_some();
            if !configuration {
                continue;
            }
            for method in &kind.methods {
                let Some(annotation) = method
                    .annotations
                    .iter()
                    .find(|annotation| annotation.name == FACTORY)
                else {
                    continue;
                };
                model.beans.push(Bean {
                    name: bean_name(Some(annotation), &method.name),
                    type_name: short_name(&method.return_type),
                    qualified_type: file.resolve(&method.return_type, &names),
                    stereotype: format!("@{FACTORY}"),
                    primary: has(&method.annotations, PRIMARY),
                    file: file.path.to_path_buf(),
                    line: method.line,
                    offset: method.offset,
                    usages: Vec::new(),
                });
            }
        }
    }

    // The points that receive a bean, with the place of the type a warning underlines.
    let mut points: Vec<(InjectionPoint, Option<String>)> = Vec::new();
    for file in &data {
        for kind in &file.file.types {
            for field in &kind.fields {
                if !field
                    .annotations
                    .iter()
                    .any(|annotation| INJECTIONS.contains(&annotation.name.as_str()))
                {
                    continue;
                }
                points.push((
                    point(
                        file,
                        &names,
                        &field.type_text,
                        field.type_offset,
                        field.line,
                        field.offset,
                    ),
                    value_of(&field.annotations, QUALIFIER).map(str::to_owned),
                ));
            }
            let component = bean_annotation(&kind.annotations, &component_annotations).is_some()
                && !kind.is_interface;
            if component {
                let constructors: Vec<&java_syntax::Constructor> = if kind.constructors.len() == 1 {
                    kind.constructors.iter().collect()
                } else {
                    kind.constructors
                        .iter()
                        .filter(|constructor| has(&constructor.annotations, "Autowired"))
                        .collect()
                };
                for constructor in constructors {
                    for parameter in &constructor.parameters {
                        points.push((
                            point(
                                file,
                                &names,
                                &parameter.type_text,
                                parameter.type_offset,
                                parameter.line,
                                parameter.offset,
                            ),
                            value_of(&parameter.annotations, QUALIFIER).map(str::to_owned),
                        ));
                    }
                }
            }
            if !component {
                continue;
            }
            for method in &kind.methods {
                if !has(&method.annotations, FACTORY) {
                    continue;
                }
                for parameter in &method.parameters {
                    points.push((
                        point(
                            file,
                            &names,
                            &parameter.type_text,
                            parameter.type_offset,
                            parameter.line,
                            parameter.offset,
                        ),
                        value_of(&parameter.annotations, QUALIFIER).map(str::to_owned),
                    ));
                }
            }
        }
    }

    // A candidate has the type of the point or inherits or implements it.
    let mut placements: HashMap<usize, Vec<InjectionPlace>> = HashMap::new();
    for (mut injection, qualifier) in points {
        let mut candidates: Vec<usize> = model
            .beans
            .iter()
            .enumerate()
            .filter(|(_, bean)| {
                bean.qualified_type == injection.qualified_type
                    || is_subtype(&bean.qualified_type, &injection.qualified_type, &types)
            })
            .map(|(index, _)| index)
            .collect();
        if let Some(qualifier) = &qualifier {
            candidates.retain(|index| model.beans[*index].name == *qualifier);
        } else if candidates.len() > 1 && candidates.iter().any(|index| model.beans[*index].primary)
        {
            // The primary bean wins over its siblings of the same type.
            candidates.retain(|index| model.beans[*index].primary);
        }
        candidates.sort_by(|left, right| model.beans[*left].name.cmp(&model.beans[*right].name));
        if candidates.len() > 1 {
            let names: Vec<&str> = candidates
                .iter()
                .map(|index| model.beans[*index].name.as_str())
                .collect();
            let short = simple_type(&injection.qualified_type).to_owned();
            model.warnings.push(BeanWarning {
                file: injection.file.clone(),
                range: injection.range.clone(),
                message: format!(
                    "Could not autowire. There is more than one bean of '{short}' type: {}",
                    names.join(", ")
                ),
            });
        } else if candidates.is_empty() && names.contains(&injection.qualified_type) {
            let short = simple_type(&injection.qualified_type).to_owned();
            model.warnings.push(BeanWarning {
                file: injection.file.clone(),
                range: injection.range.clone(),
                message: format!("Could not autowire. No beans of '{short}' type found"),
            });
        }
        for index in &candidates {
            let bean = &model.beans[*index];
            placements.entry(*index).or_default().push(InjectionPlace {
                label: format!("{} — {}", bean.name, file_name(&injection.file)),
                file: injection.file.clone(),
                offset: injection.offset,
            });
            injection.candidates.push(InjectionPlace {
                label: bean.name.clone(),
                file: bean.file.clone(),
                offset: bean.offset,
            });
        }
        model.injections.push(injection);
    }
    for (index, usages) in placements {
        model.beans[index].usages = usages;
    }
    model.beans.sort_by(|left, right| {
        (&left.stereotype, &left.name, &left.file).cmp(&(
            &right.stereotype,
            &right.name,
            &right.file,
        ))
    });
    model
        .injections
        .sort_by(|left, right| (&left.file, left.line).cmp(&(&right.file, right.line)));
    model.warnings.sort_by(|left, right| {
        (&left.file, left.range.start, left.range.end).cmp(&(
            &right.file,
            right.range.start,
            right.range.end,
        ))
    });
    model
}

struct ProjectType {
    supers: Vec<String>,
}

fn point(
    file: &FileData<'_>,
    names: &HashSet<String>,
    type_text: &str,
    type_offset: usize,
    line: u32,
    offset: usize,
) -> InjectionPoint {
    InjectionPoint {
        type_name: short_name(type_text),
        qualified_type: file.resolve(type_text, names),
        file: file.path.to_path_buf(),
        line,
        offset,
        range: type_offset..type_offset + type_text.len(),
        candidates: Vec::new(),
    }
}

fn is_data_repository(
    kind: &java_syntax::Type,
    file: &FileData<'_>,
    names: &HashSet<String>,
) -> bool {
    kind.supers.iter().any(|raw| {
        let resolved = file.resolve(raw, names);
        let short = simple_type(&resolved);
        DATA_REPOSITORIES.contains(&short)
    })
}

/// Whether a type inherits or implements another, from the declarations of the project.
fn is_subtype(candidate: &str, wanted: &str, types: &HashMap<String, ProjectType>) -> bool {
    let mut seen = HashSet::new();
    let mut stack = vec![candidate.to_owned()];
    while let Some(current) = stack.pop() {
        if current == wanted {
            return true;
        }
        if !seen.insert(current.clone()) {
            continue;
        }
        let Some(found) = types.get(&current) else {
            continue;
        };
        for raw in &found.supers {
            stack.push(raw.clone());
        }
    }
    false
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

impl lang_api::BeanProvider for BeanIndex {
    fn sources_changed(&self) {
        self.bump();
    }
}

impl Provider<BeanQuery, BeanModel> for BeanIndex {
    fn start(&self, _: BeanQuery, _: Cancel, mut reply: Reply<BeanModel>) {
        let index = self.clone();
        thread::spawn(move || {
            let mut sent = None;
            loop {
                let generation = index.generation();
                if sent == Some(generation) {
                    if !index.wait_change(generation, &reply) {
                        return;
                    }
                    continue;
                }
                if !index.wait_ready(generation, &reply) {
                    return;
                }
                let snapshot = index.snapshot();
                let sent_answer = reply.send(Answer::new(
                    snapshot.as_ref().clone(),
                    Confidence::Certain,
                    SOURCE,
                ));
                if sent_answer.is_err() && reply.is_cancelled() {
                    return;
                }
                sent = Some(generation);
            }
        });
    }
}

impl BeanIndex {
    /// The model as it stands now, for a caller that may not wait: the empty one before
    /// the first read.
    pub fn model(&self) -> Arc<BeanModel> {
        self.snapshot()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(files: &[(&str, &str)]) -> BeanModel {
        let parsed: Vec<(PathBuf, java_syntax::SpringFile)> = files
            .iter()
            .map(|(name, text)| {
                (
                    PathBuf::from(format!("C:/p/src/demo/{name}")),
                    java_syntax::spring_file(text),
                )
            })
            .collect();
        let files: Vec<(&Path, &java_syntax::SpringFile)> = parsed
            .iter()
            .map(|(path, file)| (path.as_path(), file))
            .collect();
        build_model(&files)
    }

    fn names(model: &BeanModel) -> Vec<&str> {
        model.beans.iter().map(|bean| bean.name.as_str()).collect()
    }

    fn candidates(model: &BeanModel, line: u32) -> Vec<&str> {
        model
            .injections
            .iter()
            .find(|point| point.line == line)
            .map(|point| {
                point
                    .candidates
                    .iter()
                    .map(|place| place.label.as_str())
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn component_annotations_declare_beans_with_their_names() {
        let source = r#"package demo;

import org.springframework.stereotype.Component;
import org.springframework.stereotype.Service;
import org.springframework.web.bind.annotation.RestController;

@Component
class Plain {}

@Service("shop")
class ShopService {}

@RestController
class ShopController {}
"#;
        let model = model(&[("Beans.java", source)]);
        assert_eq!(names(&model), ["plain", "shopController", "shop"]);
        let stereotypes: Vec<&str> = model
            .beans
            .iter()
            .map(|bean| bean.stereotype.as_str())
            .collect();
        assert_eq!(stereotypes, ["@Component", "@RestController", "@Service"]);
        let plain = &model.beans[0];
        assert_eq!(plain.type_name, "Plain");
        assert_eq!(plain.qualified_type, "demo.Plain");
        assert_eq!(&source[plain.offset..plain.offset + 5], "Plain");
        assert_eq!(plain.line, 7);
    }

    #[test]
    fn an_annotation_of_the_project_marks_a_bean() {
        let source = r#"package demo;

import org.springframework.stereotype.Component;
import java.lang.annotation.Retention;

@Component
@Retention(RetentionPolicy.RUNTIME)
@interface MyService {}

@MyService
class OrderService {}
"#;
        let model = model(&[("MyService.java", source)]);
        assert_eq!(names(&model), ["orderService"]);
        assert_eq!(model.beans[0].stereotype, "@MyService");
        assert_eq!(model.beans[0].qualified_type, "demo.OrderService");
    }

    #[test]
    fn factory_methods_declare_beans_of_their_return_type() {
        let source = r#"package demo;

import org.springframework.context.annotation.Bean;
import org.springframework.context.annotation.Configuration;
import org.springframework.context.annotation.Primary;

@Configuration
class Factory {
    @Bean
    Clock clock() { return null; }

    @Bean(name = "ownerRepository")
    OwnerRepository repository() { return null; }

    @Bean
    @Primary
    Store store() { return null; }
}
"#;
        let model = model(&[("Factory.java", source)]);
        assert_eq!(
            names(&model),
            ["clock", "ownerRepository", "store", "factory"]
        );
        let clock = &model.beans[0];
        assert_eq!(clock.stereotype, "@Bean");
        assert_eq!(clock.type_name, "Clock");
        assert_eq!(clock.qualified_type, "Clock");
        assert_eq!(&source[clock.offset..clock.offset + 5], "clock");
        assert!(model.beans[2].primary);
    }

    #[test]
    fn a_data_repository_interface_is_a_bean() {
        let source = r#"package demo.store;

import org.springframework.data.jpa.repository.JpaRepository;

interface OwnerRepository extends JpaRepository<Owner, Long> {}
"#;
        let model = model(&[("OwnerRepository.java", source)]);
        assert_eq!(names(&model), ["ownerRepository"]);
        assert_eq!(model.beans[0].stereotype, "Spring Data");
        assert_eq!(model.beans[0].qualified_type, "demo.store.OwnerRepository");
    }

    #[test]
    fn a_constructor_parameter_receives_a_bean_of_its_type_and_of_its_subtypes() {
        let source = r#"package demo;

interface Animal {}

@Component
class Dog implements Animal {}

@Component
class Zoo {
    Zoo(Animal animal) {}
}
"#;
        let model = model(&[("Zoo.java", source)]);
        assert_eq!(candidates(&model, 9), ["dog"]);
        assert!(model.warnings.is_empty());
        let dog = model
            .beans
            .iter()
            .find(|bean| bean.name == "dog")
            .expect("the dog is a bean");
        assert_eq!(dog.usages.len(), 1);
        assert!(dog.usages[0].label.contains("dog"));
    }

    #[test]
    fn a_qualifier_narrows_the_candidates_to_a_name() {
        let source = r#"package demo;

import org.springframework.beans.factory.annotation.Qualifier;

interface Animal {}

@Component
class Dog implements Animal {}

@Component
class Cat implements Animal {}

@Component
class Zoo {
    Zoo(@Qualifier("cat") Animal animal) {}
}
"#;
        let model = model(&[("Zoo.java", source)]);
        assert_eq!(candidates(&model, 14), ["cat"]);
        assert!(model.warnings.is_empty());
    }

    #[test]
    fn a_primary_bean_wins_over_its_siblings() {
        let source = r#"package demo;

import org.springframework.context.annotation.Primary;

interface Animal {}

@Primary
@Component
class Dog implements Animal {}

@Component
class Cat implements Animal {}

@Component
class Zoo {
    Zoo(Animal animal) {}
}
"#;
        let model = model(&[("Zoo.java", source)]);
        assert_eq!(candidates(&model, 15), ["dog"]);
        assert!(model.warnings.is_empty());
    }

    #[test]
    fn two_candidates_without_a_qualifier_warn_and_list_both() {
        let source = r#"package demo;

interface Animal {}

@Component
class Dog implements Animal {}

@Component
class Cat implements Animal {}

@Component
class Zoo {
    Zoo(Animal animal) {}
}
"#;
        let model = model(&[("Zoo.java", source)]);
        assert_eq!(candidates(&model, 12), ["cat", "dog"]);
        assert_eq!(model.warnings.len(), 1);
        assert_eq!(
            model.warnings[0].message,
            "Could not autowire. There is more than one bean of 'Animal' type: cat, dog"
        );
        assert_eq!(
            model.warnings[0].file,
            PathBuf::from("C:/p/src/demo/Zoo.java")
        );
    }

    #[test]
    fn a_project_type_without_a_bean_warns() {
        let source = r#"package demo;

interface Animal {}

@Component
class Zoo {
    Zoo(@org.springframework.beans.factory.annotation.Autowired Animal animal) {}
}
"#;
        let model = model(&[("Zoo.java", source)]);
        assert!(candidates(&model, 6).is_empty());
        assert_eq!(
            model.warnings[0].message,
            "Could not autowire. No beans of 'Animal' type found"
        );
    }

    #[test]
    fn a_library_type_without_a_bean_does_not_warn() {
        let source = r#"package demo;

import java.util.List;
import org.springframework.beans.factory.annotation.Autowired;

@Component
class Zoo {
    @Autowired
    private List<String> names;

    @Autowired
    private SomeLibraryThing thing;
}
"#;
        let model = model(&[("Zoo.java", source)]);
        assert_eq!(model.injections.len(), 2);
        assert!(model.warnings.is_empty(), "{:?}", model.warnings);
    }

    #[test]
    fn a_factory_method_parameter_is_a_point_of_an_injection() {
        let source = r#"package demo;

import org.springframework.context.annotation.Bean;
import org.springframework.context.annotation.Configuration;

@Configuration
class Factory {
    @Bean
    Service service(Repository repository) { return null; }
}

@Component
class Repository {}
"#;
        let model = model(&[("Factory.java", source)]);
        assert_eq!(candidates(&model, 8), ["repository"]);
        assert_eq!(model.injections[0].type_name, "Repository");
    }

    #[test]
    fn an_injected_field_names_the_bean_it_receives() {
        let source = r#"package demo;

import org.springframework.beans.factory.annotation.Autowired;

@Component
class Owner {}

@Component
class Controller {
    @Autowired
    private Owner owner;
}
"#;
        let model = model(&[("Controller.java", source)]);
        assert_eq!(candidates(&model, 10), ["owner"]);
        let owner = model
            .beans
            .iter()
            .find(|bean| bean.name == "owner")
            .expect("the owner is a bean");
        assert_eq!(owner.usages.len(), 1);
        assert_eq!(owner.usages[0].offset, source.find("owner;").unwrap());
    }

    #[test]
    fn the_index_follows_the_files_and_an_open_text() {
        use futures_util::StreamExt;
        use std::time::Duration;

        let root = test_support::TempDir::new("bean-index");
        let source = root.path().join("src").join("demo");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("A.java"), "@Component class A {}\n").unwrap();
        let owner = source.join("B.java").to_path_buf();
        std::fs::write(&owner, "@Component class B {\n    B(A a) {}\n}\n").unwrap();
        let index = bean_index(root.path().to_path_buf());
        index.set_sources(vec![root.path().join("src")]);
        let (reply, mut replies) = lang_api::channel::<BeanModel>();
        Provider::start(index.as_ref(), BeanQuery, Cancel::new(), reply);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        let mut next = || {
            runtime
                .block_on(async {
                    tokio::time::timeout(Duration::from_secs(10), replies.next()).await
                })
                .expect("the index answers")
                .expect("the answer holds a model")
                .value
        };
        let first = next();
        assert!(first.beans.iter().any(|bean| bean.name == "a"));
        assert_eq!(
            first.injections[0]
                .candidates
                .iter()
                .map(|place| place.label.as_str())
                .collect::<Vec<_>>(),
            ["a"]
        );
        // The point leaves the model when the open text no longer has it.
        index.document_changed(&owner, Some(Arc::from("@Component class B {}\n")));
        let edited = next();
        assert!(edited.injections.is_empty(), "{:?}", edited.injections);
        // Closing the tab reads the file on disk again.
        index.document_changed(&owner, None);
        let restored = next();
        assert_eq!(restored.injections.len(), 1);
    }

    /// The real PetClinic needs `IDE_CORPUS`: its owner controller receives the owner
    /// repository, which is an interface of Spring Data.
    ///
    /// `cargo test -p java-lang beans -- --ignored --test-threads=1`
    #[test]
    #[ignore = "needs IDE_CORPUS with the PetClinic sources"]
    fn petclinic_owner_controller_receives_its_repository() {
        let corpus = std::env::var("IDE_CORPUS").expect("IDE_CORPUS names the corpus");
        let root = PathBuf::from(corpus).join("src/petclinic/src/main/java");
        let mut paths = Vec::new();
        collect_sources(&root, &mut paths);
        assert!(
            !paths.is_empty(),
            "the sources of PetClinic are in {root:?}"
        );
        let files: Vec<(PathBuf, java_syntax::SpringFile)> = paths
            .iter()
            .filter_map(|path| {
                let text = std::fs::read_to_string(path).ok()?;
                Some((path.clone(), java_syntax::spring_file(&text)))
            })
            .collect();
        let parsed: Vec<(&Path, &java_syntax::SpringFile)> = files
            .iter()
            .map(|(path, file)| (path.as_path(), file))
            .collect();
        let model = build_model(&parsed);
        let controller = model
            .injections
            .iter()
            .find(|point| point.file.ends_with("owner/OwnerController.java"))
            .expect("the owner controller has an injected repository");
        assert_eq!(controller.type_name, "OwnerRepository");
        assert!(
            controller
                .candidates
                .iter()
                .any(|place| place.label == "ownerRepository"),
            "{:?}",
            controller.candidates
        );
        println!("beans of PetClinic: {}", model.beans.len());
        assert!(model.beans.len() > 10);
    }
}
