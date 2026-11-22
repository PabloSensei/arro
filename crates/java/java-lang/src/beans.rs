//! The dependency-injection model of a project, read natively from its sources: the
//! beans of the usual component annotations and factory methods, the repositories of
//! Spring Data, the points that receive a bean and the checks of those points. The beans
//! of CDI and Micronaut are read the same way, for the modules that name a framework.
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
use java_project::{Framework, FrameworkLookup, Frameworks};
use java_syntax::{FACTORY, STEREOTYPES};
use lang_api::{
    Answer, Bean, BeanModel, BeanQuery, BeanWarning, Confidence, InjectionPlace, InjectionPoint,
    Provider, Reply, SourceId,
};
use lsp_client::lsp_types::Uri;
use lsp_client::uri::file_path;
use project_model::ProjectModel;

const SOURCE: SourceId = SourceId::new("spring-beans");
/// The scopes of CDI that declare a bean. Quarkus implements them with ArC.
const CDI_SCOPES: &[&str] = &[
    "ApplicationScoped",
    "RequestScoped",
    "SessionScoped",
    "ConversationScoped",
    "Dependent",
    "Singleton",
];
/// The scopes of Micronaut that declare a bean of a class.
const MICRONAUT_SCOPES: &[&str] = &["Singleton", "Prototype"];
/// The annotation of a CDI method or field that declares a bean.
const PRODUCES: &str = "Produces";
/// The annotation of a Micronaut class whose methods declare beans.
const FACTORY_CLASS: &str = "Factory";
/// The annotations of a Micronaut factory method that declare a bean.
const MICRONAUT_BEANS: &[&str] = &["Bean", "Singleton", "Prototype"];
/// The annotations that mark a field or a parameter as a point of an injection.
const INJECTIONS: &[&str] = &["Autowired", "Inject", "Resource"];
/// The annotation that narrows a point to the bean of a name.
const QUALIFIER: &str = "Qualifier";
/// The annotation that names a bean or a point.
const NAMED: &str = "Named";
/// The qualifier a point asks for when a bean has no other one.
const DEFAULT_QUALIFIER: &str = "Default";
/// The qualifier that accepts every candidate of the type.
const ANY_QUALIFIER: &str = "Any";
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
    /// roots as the previous one starts no new read. The frameworks of the module of a
    /// file decide which annotations declare a bean.
    pub fn set_model(&self, model: &ProjectModel) {
        self.set_sources_and(sources_of(model), FrameworkLookup::of(model));
    }

    /// The roots the sources are read from, for a caller that has no model.
    pub fn set_sources(&self, sources: Vec<PathBuf>) {
        self.set_sources_and(sources, FrameworkLookup::default());
    }

    fn set_sources_and(&self, sources: Vec<PathBuf>, frameworks: FrameworkLookup) {
        let mut state = self.lock_state();
        if state.sources == sources && state.frameworks == frameworks {
            return;
        }
        state.sources = sources;
        state.frameworks = frameworks;
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
    /// The frameworks of every source root of the model.
    frameworks: FrameworkLookup,
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
            let (generation, sources, frameworks, cache, open) = {
                let state = inner
                    .state
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner());
                (
                    state.generation,
                    state.sources.clone(),
                    state.frameworks.clone(),
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
            let (snapshot, cache) = read_model(&sources, &frameworks, &cache, &open);
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
    frameworks: &FrameworkLookup,
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
    (build_model_with(&files, frameworks), next)
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
/// Assembles the model of a project from its parsed sources, for the Spring annotations
/// alone.
#[cfg(test)]
pub fn build_model(files: &[(&Path, &java_syntax::SpringFile)]) -> BeanModel {
    build_model_with(files, &FrameworkLookup::default())
}

/// Assembles the model of a project from its parsed sources. The frameworks of the module
/// of a file decide which annotations declare a bean and which qualifiers narrow a point.
pub fn build_model_with(
    files: &[(&Path, &java_syntax::SpringFile)],
    frameworks: &FrameworkLookup,
) -> BeanModel {
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
    // An annotation of the project that carries `@Qualifier` is a qualifier of its own.
    let qualifier_annotations: HashSet<String> = data
        .iter()
        .flat_map(|file| file.file.types.iter())
        .filter(|kind| kind.is_annotation && has(&kind.annotations, QUALIFIER))
        .map(|kind| kind.name.clone())
        .collect();

    let mut beans: Vec<PendingBean> = Vec::new();
    for file in &data {
        let module = frameworks.of_path(file.path);
        let cdi = module.cdi();
        let micronaut = module.contains(Framework::Micronaut);
        for kind in &file.file.types {
            let scope = bean_scope(
                &kind.annotations,
                &component_annotations,
                module,
                kind.is_interface,
                kind.is_annotation,
            );
            if let Some(annotation) = scope {
                beans.push(PendingBean {
                    bean: Bean {
                        name: declaration_name(&kind.annotations, Some(annotation), &kind.name),
                        type_name: kind.name.clone(),
                        qualified_type: kind.qualified.clone(),
                        stereotype: format!("@{}", annotation.name),
                        primary: has(&kind.annotations, PRIMARY),
                        file: file.path.to_path_buf(),
                        line: kind.line,
                        offset: kind.offset,
                        usages: Vec::new(),
                    },
                    qualifiers: custom_qualifiers(&kind.annotations, &qualifier_annotations),
                });
            } else if kind.is_interface && is_data_repository(kind, file, &names) {
                beans.push(PendingBean {
                    bean: Bean {
                        name: decapitalize(&kind.name),
                        type_name: kind.name.clone(),
                        qualified_type: kind.qualified.clone(),
                        stereotype: "Spring Data".to_owned(),
                        primary: false,
                        file: file.path.to_path_buf(),
                        line: kind.line,
                        offset: kind.offset,
                        usages: Vec::new(),
                    },
                    qualifiers: Vec::new(),
                });
            }
            let spring_configuration =
                bean_annotation(&kind.annotations, &component_annotations).is_some();
            if spring_configuration {
                for method in &kind.methods {
                    let Some(annotation) = method
                        .annotations
                        .iter()
                        .find(|annotation| annotation.name == FACTORY)
                    else {
                        continue;
                    };
                    beans.push(declared_bean(
                        file,
                        &names,
                        Some(annotation),
                        &method.name,
                        &method.return_type,
                        method.line,
                        method.offset,
                        &method.annotations,
                        &qualifier_annotations,
                    ));
                }
            }
            // The methods of a Micronaut factory declare beans of their return type.
            if micronaut && has(&kind.annotations, FACTORY_CLASS) {
                for method in &kind.methods {
                    let Some(annotation) = method
                        .annotations
                        .iter()
                        .find(|annotation| MICRONAUT_BEANS.contains(&annotation.name.as_str()))
                    else {
                        continue;
                    };
                    beans.push(declared_bean(
                        file,
                        &names,
                        Some(annotation),
                        &method.name,
                        &method.return_type,
                        method.line,
                        method.offset,
                        &method.annotations,
                        &qualifier_annotations,
                    ));
                }
            }
            // A producer method or field of a CDI bean declares a bean of its own type.
            if cdi && scope.is_some() {
                for method in &kind.methods {
                    if !has(&method.annotations, PRODUCES) {
                        continue;
                    }
                    beans.push(declared_bean(
                        file,
                        &names,
                        None,
                        &method.name,
                        &method.return_type,
                        method.line,
                        method.offset,
                        &method.annotations,
                        &qualifier_annotations,
                    ));
                }
                for field in &kind.fields {
                    if !has(&field.annotations, PRODUCES) {
                        continue;
                    }
                    beans.push(PendingBean {
                        bean: Bean {
                            name: value_of(&field.annotations, NAMED)
                                .map_or_else(|| field.name.clone(), str::to_owned),
                            type_name: short_name(&field.type_text),
                            qualified_type: file.resolve(&field.type_text, &names),
                            stereotype: format!("@{PRODUCES}"),
                            primary: false,
                            file: file.path.to_path_buf(),
                            line: field.line,
                            offset: field.offset,
                            usages: Vec::new(),
                        },
                        qualifiers: custom_qualifiers(&field.annotations, &qualifier_annotations),
                    });
                }
            }
        }
    }

    // The points that receive a bean, with the place of the type a warning underlines.
    let mut points: Vec<PendingPoint> = Vec::new();
    for file in &data {
        let module = frameworks.of_path(file.path);
        let cdi = module.cdi();
        let micronaut = module.contains(Framework::Micronaut);
        for kind in &file.file.types {
            let scope = bean_scope(
                &kind.annotations,
                &component_annotations,
                module,
                kind.is_interface,
                kind.is_annotation,
            );
            let spring_component = bean_annotation(&kind.annotations, &component_annotations)
                .is_some()
                && !kind.is_interface;
            for field in &kind.fields {
                if !field
                    .annotations
                    .iter()
                    .any(|annotation| INJECTIONS.contains(&annotation.name.as_str()))
                {
                    continue;
                }
                points.push(point(
                    file,
                    &names,
                    &field.annotations,
                    &qualifier_annotations,
                    &field.type_text,
                    field.type_offset,
                    field.line,
                    field.offset,
                    module,
                ));
            }
            let constructors: Vec<&java_syntax::Constructor> = if spring_component {
                if kind.constructors.len() == 1 {
                    kind.constructors.iter().collect()
                } else {
                    kind.constructors
                        .iter()
                        .filter(|constructor| has(&constructor.annotations, "Autowired"))
                        .collect()
                }
            } else if (cdi || micronaut) && scope.is_some() {
                kind.constructors
                    .iter()
                    .filter(|constructor| has(&constructor.annotations, "Inject"))
                    .collect()
            } else {
                Vec::new()
            };
            for constructor in constructors {
                for parameter in &constructor.parameters {
                    points.push(point(
                        file,
                        &names,
                        &parameter.annotations,
                        &qualifier_annotations,
                        &parameter.type_text,
                        parameter.type_offset,
                        parameter.line,
                        parameter.offset,
                        module,
                    ));
                }
            }
            for method in &kind.methods {
                // A setter with `@Inject` receives a bean too.
                if has(&method.annotations, "Inject")
                    && method.parameters.len() == 1
                    && !has(&method.annotations, PRODUCES)
                {
                    let parameter = &method.parameters[0];
                    points.push(point(
                        file,
                        &names,
                        &method.annotations,
                        &qualifier_annotations,
                        &parameter.type_text,
                        parameter.type_offset,
                        parameter.line,
                        parameter.offset,
                        module,
                    ));
                }
                let factory = (spring_component && has(&method.annotations, FACTORY))
                    || (micronaut
                        && has(&kind.annotations, FACTORY_CLASS)
                        && method
                            .annotations
                            .iter()
                            .any(|annotation| MICRONAUT_BEANS.contains(&annotation.name.as_str())))
                    || (cdi && scope.is_some() && has(&method.annotations, PRODUCES));
                if !factory {
                    continue;
                }
                for parameter in &method.parameters {
                    points.push(point(
                        file,
                        &names,
                        &parameter.annotations,
                        &qualifier_annotations,
                        &parameter.type_text,
                        parameter.type_offset,
                        parameter.line,
                        parameter.offset,
                        module,
                    ));
                }
            }
        }
    }

    let qualifiers: Vec<Vec<String>> = beans.iter().map(|bean| bean.qualifiers.clone()).collect();
    let mut model = BeanModel {
        beans: beans.into_iter().map(|pending| pending.bean).collect(),
        injections: Vec::new(),
        warnings: Vec::new(),
    };
    // A candidate has the type of the point or inherits or implements it.
    let mut placements: HashMap<usize, Vec<InjectionPlace>> = HashMap::new();
    for pending in points {
        let PendingPoint {
            point: mut injection,
            frameworks: module,
            named,
            qualifiers: wanted,
            default,
            any,
        } = pending;
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
        if let Some(named) = &named {
            candidates.retain(|index| model.beans[*index].name == *named);
        }
        if !wanted.is_empty() {
            candidates.retain(|index| {
                wanted
                    .iter()
                    .all(|qualifier| qualifiers[*index].contains(qualifier))
            });
        }
        if module.cdi() && default && candidates.len() > 1 {
            let unqualified: Vec<usize> = candidates
                .iter()
                .copied()
                .filter(|index| qualifiers[*index].is_empty())
                .collect();
            if !unqualified.is_empty() {
                candidates = unqualified;
            }
        }
        if !any
            && candidates.len() > 1
            && candidates.iter().any(|index| model.beans[*index].primary)
        {
            // The primary bean wins over its siblings of the same type.
            candidates.retain(|index| model.beans[*index].primary);
        }
        candidates.sort_by(|left, right| model.beans[*left].name.cmp(&model.beans[*right].name));
        let (no_bean, multiple) = warning_ids(module);
        if candidates.len() > 1 && !any {
            let names: Vec<&str> = candidates
                .iter()
                .map(|index| model.beans[*index].name.as_str())
                .collect();
            let short = simple_type(&injection.qualified_type).to_owned();
            let message = if module.cdi() || module.contains(Framework::Micronaut) {
                format!(
                    "There is more than one bean of '{short}' type: {}",
                    names.join(", ")
                )
            } else {
                format!(
                    "Could not autowire. There is more than one bean of '{short}' type: {}",
                    names.join(", ")
                )
            };
            model.warnings.push(BeanWarning {
                file: injection.file.clone(),
                range: injection.range.clone(),
                id: multiple.to_owned(),
                message,
            });
        } else if candidates.is_empty() && names.contains(&injection.qualified_type) && !any {
            let short = simple_type(&injection.qualified_type).to_owned();
            let message = if module.cdi() || module.contains(Framework::Micronaut) {
                format!("No bean of '{short}' type found")
            } else {
                format!("Could not autowire. No beans of '{short}' type found")
            };
            model.warnings.push(BeanWarning {
                file: injection.file.clone(),
                range: injection.range.clone(),
                id: no_bean.to_owned(),
                message,
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
    // The qualifiers followed the beans until the places were made; the sort below moves
    // the beans alone.
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

/// A bean while the model is assembled, with the qualifiers that narrow a point to it.
struct PendingBean {
    bean: Bean,
    /// The qualifier annotations of the project on the declaration.
    qualifiers: Vec<String>,
}

/// A point that receives a bean, with what narrows its candidates.
struct PendingPoint {
    point: InjectionPoint,
    /// The frameworks of the module of the point.
    frameworks: Frameworks,
    named: Option<String>,
    qualifiers: Vec<String>,
    default: bool,
    any: bool,
}

/// The annotation that declares a bean of a class for the frameworks of its module.
fn bean_scope<'a>(
    annotations: &'a [java_syntax::Annotation],
    component_annotations: &HashSet<String>,
    frameworks: Frameworks,
    is_interface: bool,
    is_annotation: bool,
) -> Option<&'a java_syntax::Annotation> {
    if is_annotation {
        return None;
    }
    if let Some(annotation) = bean_annotation(annotations, component_annotations) {
        return Some(annotation);
    }
    if is_interface {
        return None;
    }
    let mut scopes: Vec<&str> = Vec::new();
    if frameworks.cdi() {
        scopes.extend(CDI_SCOPES.iter().copied());
    }
    if frameworks.contains(Framework::Micronaut) {
        scopes.extend(MICRONAUT_SCOPES.iter().copied());
    }
    annotations
        .iter()
        .find(|annotation| scopes.contains(&annotation.name.as_str()))
}

/// The name a declaration gives its bean: `@Named`, the value of the stereotype, or the
/// name of the declaration with a lower first letter.
fn declaration_name(
    annotations: &[java_syntax::Annotation],
    scope: Option<&java_syntax::Annotation>,
    fallback: &str,
) -> String {
    value_of(annotations, NAMED)
        .map(str::to_owned)
        .or_else(|| scope.and_then(|annotation| annotation.value.clone()))
        .unwrap_or_else(|| decapitalize(fallback))
}

/// The qualifier annotations of the project a declaration carries.
fn custom_qualifiers(
    annotations: &[java_syntax::Annotation],
    known: &HashSet<String>,
) -> Vec<String> {
    annotations
        .iter()
        .filter(|annotation| known.contains(&annotation.name))
        .map(|annotation| annotation.name.clone())
        .collect()
}

/// A bean a factory or producer declaration gives, named by the annotation or by the
/// declaration itself.
#[allow(clippy::too_many_arguments)]
fn declared_bean(
    file: &FileData<'_>,
    names: &HashSet<String>,
    annotation: Option<&java_syntax::Annotation>,
    fallback: &str,
    return_type: &str,
    line: u32,
    offset: usize,
    annotations: &[java_syntax::Annotation],
    qualifier_annotations: &HashSet<String>,
) -> PendingBean {
    PendingBean {
        bean: Bean {
            name: value_of(annotations, NAMED)
                .map_or_else(|| bean_name(annotation, fallback), str::to_owned),
            type_name: short_name(return_type),
            qualified_type: file.resolve(return_type, names),
            stereotype: format!(
                "@{}",
                annotation.map_or(PRODUCES, |annotation| annotation.name.as_str())
            ),
            primary: has(annotations, PRIMARY),
            file: file.path.to_path_buf(),
            line,
            offset,
            usages: Vec::new(),
        },
        qualifiers: custom_qualifiers(annotations, qualifier_annotations),
    }
}

/// The ids of the checks a point of a module reports.
fn warning_ids(frameworks: Frameworks) -> (&'static str, &'static str) {
    if frameworks.contains(Framework::Micronaut) {
        ("micronaut.noBean", "micronaut.multipleBeans")
    } else if frameworks.cdi() {
        ("jakarta.noBean", "jakarta.multipleBeans")
    } else {
        ("beans.noCandidate", "beans.multipleCandidates")
    }
}

struct ProjectType {
    supers: Vec<String>,
}

#[allow(clippy::too_many_arguments)]
fn point(
    file: &FileData<'_>,
    names: &HashSet<String>,
    annotations: &[java_syntax::Annotation],
    qualifier_annotations: &HashSet<String>,
    type_text: &str,
    type_offset: usize,
    line: u32,
    offset: usize,
    frameworks: Frameworks,
) -> PendingPoint {
    PendingPoint {
        point: InjectionPoint {
            type_name: short_name(type_text),
            qualified_type: file.resolve(type_text, names),
            file: file.path.to_path_buf(),
            line,
            offset,
            range: type_offset..type_offset + type_text.len(),
            candidates: Vec::new(),
        },
        frameworks,
        named: value_of(annotations, QUALIFIER)
            .or_else(|| value_of(annotations, NAMED))
            .map(str::to_owned),
        qualifiers: custom_qualifiers(annotations, qualifier_annotations),
        default: has(annotations, DEFAULT_QUALIFIER),
        any: has(annotations, ANY_QUALIFIER),
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

/// The model of the sources of a project as the model describes them, read once on the
/// caller's thread. A test support uses it; the window reads [`BeanIndex`] instead.
pub fn bean_model_for(model: &ProjectModel) -> BeanModel {
    let sources = sources_of(model);
    let sources = if sources.is_empty() {
        vec![model.root.clone()]
    } else {
        sources
    };
    let (found, _) = read_model(
        &sources,
        &FrameworkLookup::of(model),
        &HashMap::new(),
        &HashMap::new(),
    );
    found
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

    /// A model of a project whose only source root uses the frameworks of the caller.
    fn model_with(files: &[(&str, &str)], frameworks: Frameworks) -> BeanModel {
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
        let lookup = FrameworkLookup::of_roots(vec![(PathBuf::from("C:/p/src"), frameworks)]);
        build_model_with(&files, &lookup)
    }

    fn cdi() -> Frameworks {
        Frameworks::of(&[Framework::Jakarta])
    }

    fn micronaut() -> Frameworks {
        Frameworks::of(&[Framework::Micronaut])
    }

    fn candidate_names(model: &BeanModel) -> Vec<Vec<&str>> {
        model
            .injections
            .iter()
            .map(|point| {
                point
                    .candidates
                    .iter()
                    .map(|place| place.label.as_str())
                    .collect()
            })
            .collect()
    }

    #[test]
    fn cdi_scopes_declare_beans_and_inject_fields_constructors_and_setters() {
        let source = r#"package demo;

import jakarta.enterprise.context.ApplicationScoped;
import jakarta.enterprise.context.RequestScoped;
import jakarta.inject.Inject;

interface Repository {}

@ApplicationScoped
class OwnerRepository implements Repository {}

@RequestScoped
class Shop {
    @Inject
    Repository repository;

    @Inject
    Shop(OwnerRepository owners) {}

    @Inject
    void setClock(Clock clock) {}
}

@ApplicationScoped
class Clock {}
"#;
        let model = model_with(&[("Shop.java", source)], cdi());
        assert_eq!(
            names(&model),
            ["clock", "ownerRepository", "shop"],
            "{:?}",
            names(&model)
        );
        let stereotypes: Vec<&str> = model
            .beans
            .iter()
            .map(|bean| bean.stereotype.as_str())
            .collect();
        assert_eq!(
            stereotypes,
            ["@ApplicationScoped", "@ApplicationScoped", "@RequestScoped"]
        );
        assert_eq!(
            candidate_names(&model),
            [
                vec!["ownerRepository"],
                vec!["ownerRepository"],
                vec!["clock"],
            ],
            "the field, the constructor and the setter receive their beans"
        );
        assert!(model.warnings.is_empty(), "{:?}", model.warnings);
    }

    #[test]
    fn a_custom_qualifier_narrows_the_candidates_to_the_marked_bean() {
        let source = r#"package demo;

import jakarta.enterprise.context.ApplicationScoped;
import jakarta.inject.Inject;
import jakarta.inject.Qualifier;
import java.lang.annotation.Retention;

@Qualifier
@Retention(RetentionPolicy.RUNTIME)
@interface Fast {}

interface Repository {}

@Fast
@ApplicationScoped
class FastRepository implements Repository {}

@ApplicationScoped
class SlowRepository implements Repository {}

@ApplicationScoped
class Service {
    @Inject
    @Fast
    Repository repository;
}
"#;
        let model = model_with(&[("Service.java", source)], cdi());
        assert_eq!(candidate_names(&model), [vec!["fastRepository"]]);
        assert!(model.warnings.is_empty(), "{:?}", model.warnings);
    }

    #[test]
    fn a_named_point_and_a_named_bean_meet() {
        let source = r#"package demo;

import jakarta.enterprise.context.ApplicationScoped;
import jakarta.inject.Inject;
import jakarta.inject.Named;

interface Repository {}

@Named("owners")
@ApplicationScoped
class OwnerRepository implements Repository {}

@ApplicationScoped
class OtherRepository implements Repository {}

@ApplicationScoped
class Service {
    @Inject
    @Named("owners")
    Repository repository;
}
"#;
        let model = model_with(&[("Service.java", source)], cdi());
        assert_eq!(candidate_names(&model), [vec!["owners"]]);
        assert_eq!(
            names(&model),
            ["otherRepository", "owners", "service"],
            "{:?}",
            names(&model)
        );
    }

    #[test]
    fn several_cdi_beans_without_a_qualifier_warn() {
        let source = r#"package demo;

import jakarta.enterprise.context.ApplicationScoped;
import jakarta.inject.Inject;

interface Repository {}

@ApplicationScoped
class OneRepository implements Repository {}

@ApplicationScoped
class TwoRepository implements Repository {}

@ApplicationScoped
class Service {
    @Inject
    Repository repository;
}
"#;
        let model = model_with(&[("Service.java", source)], cdi());
        assert_eq!(
            candidate_names(&model),
            [vec!["oneRepository", "twoRepository"]]
        );
        assert_eq!(model.warnings.len(), 1);
        assert_eq!(model.warnings[0].id, "jakarta.multipleBeans");
        assert_eq!(
            model.warnings[0].message,
            "There is more than one bean of 'Repository' type: oneRepository, twoRepository"
        );
    }

    #[test]
    fn a_cdi_point_without_a_bean_warns() {
        let source = r#"package demo;

import jakarta.enterprise.context.ApplicationScoped;
import jakarta.inject.Inject;

interface Repository {}

@ApplicationScoped
class Service {
    @Inject
    Repository repository;
}
"#;
        let model = model_with(&[("Service.java", source)], cdi());
        assert!(candidate_names(&model)[0].is_empty());
        assert_eq!(model.warnings.len(), 1);
        assert_eq!(model.warnings[0].id, "jakarta.noBean");
        assert_eq!(
            model.warnings[0].message,
            "No bean of 'Repository' type found"
        );
    }

    #[test]
    fn a_quarkus_module_produces_beans_from_methods_and_fields() {
        let source = r#"package demo;

import jakarta.enterprise.context.ApplicationScoped;
import jakarta.enterprise.inject.Produces;
import jakarta.inject.Inject;
import jakarta.inject.Named;

@ApplicationScoped
class Beans {
    @Produces
    @Named("clock")
    Clock clock() { return null; }

    @Produces
    Store store = new Store();
}

@ApplicationScoped
class Service {
    @Inject
    Clock clock;

    @Inject
    Store store;
}

class Clock {}
class Store {}
"#;
        let model = model_with(
            &[("Beans.java", source)],
            Frameworks::of(&[Framework::Quarkus]),
        );
        assert_eq!(
            names(&model),
            ["beans", "service", "clock", "store"],
            "{:?}",
            names(&model)
        );
        let produced: Vec<&str> = model
            .beans
            .iter()
            .filter(|bean| bean.stereotype == "@Produces")
            .map(|bean| bean.name.as_str())
            .collect();
        assert_eq!(produced, ["clock", "store"]);
        assert_eq!(candidate_names(&model), [vec!["clock"], vec!["store"]]);
        assert!(model.warnings.is_empty(), "{:?}", model.warnings);
    }

    #[test]
    fn a_micronaut_factory_declares_beans_and_its_method_parameters_receive_them() {
        let source = r#"package demo;

import io.micronaut.context.annotation.Bean;
import io.micronaut.context.annotation.Factory;
import io.micronaut.context.annotation.Prototype;
import io.micronaut.context.annotation.Requires;
import jakarta.inject.Inject;
import jakarta.inject.Named;
import jakarta.inject.Singleton;

@Factory
class Beans {
    @Bean
    @Named("owners")
    Repository repository(DataSource source) { return null; }

    @Singleton
    Clock clock() { return null; }
}

@Singleton
class DataSource {}

@Singleton
class Service {
    @Inject
    @Named("owners")
    Repository repository;
}

@Prototype
class Task {}
"#;
        let model = model_with(&[("Beans.java", source)], micronaut());
        assert_eq!(
            names(&model),
            ["owners", "task", "clock", "dataSource", "service"],
            "{:?}",
            names(&model)
        );
        let stereotypes: Vec<&str> = model
            .beans
            .iter()
            .map(|bean| bean.stereotype.as_str())
            .collect();
        assert!(stereotypes.contains(&"@Bean"), "{stereotypes:?}");
        assert!(stereotypes.contains(&"@Singleton"), "{stereotypes:?}");
        assert!(stereotypes.contains(&"@Prototype"), "{stereotypes:?}");
        assert_eq!(
            candidate_names(&model),
            [vec!["dataSource"], vec!["owners"]],
            "the factory method parameter and the injected field receive their beans"
        );
        assert!(model.warnings.is_empty(), "{:?}", model.warnings);
    }

    #[test]
    fn a_micronaut_point_without_a_bean_warns_with_its_own_check() {
        let source = r#"package demo;

import jakarta.inject.Inject;
import jakarta.inject.Singleton;

interface Repository {}

@Singleton
class Service {
    @Inject
    Repository repository;
}
"#;
        let model = model_with(&[("Service.java", source)], micronaut());
        assert_eq!(model.warnings.len(), 1);
        assert_eq!(model.warnings[0].id, "micronaut.noBean");
    }

    #[test]
    fn the_frameworks_of_the_module_decide_which_annotations_make_a_bean() {
        let source = r#"package demo;

import jakarta.enterprise.context.ApplicationScoped;

@ApplicationScoped
class Shop {}
"#;
        let spring = model(&[("Shop.java", source)]);
        assert!(spring.beans.is_empty(), "{:?}", spring.beans);
        let cdi = model_with(&[("Shop.java", source)], cdi());
        assert_eq!(names(&cdi), ["shop"]);
    }
}
