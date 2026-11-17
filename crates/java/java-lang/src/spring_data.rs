//! The entities of a project and the queries of its data repositories, read natively from
//! the sources: the properties of the classes the store persists, the entity a repository
//! interface works on, the properties and operators of the method names of the
//! conventions of the framework, and the text of a query annotation with its entities,
//! aliases, properties and parameters.
//!
//! Reading runs in the background and the interface never waits on it: a reader thread
//! keeps the model of the current sources, a window puts the checks of an open document
//! into its editor, and a query answers about a place of one document at a time.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::thread;
use std::time::{Duration, SystemTime};

use base::Cancel;
use lang_api::{
    Answer, CompletionEntry, CompletionKind, CompletionList, Confidence, DiagnosticEntry,
    DocumentSpot, EmbeddedTextProvider, LanguageId, LanguageSupport, NavigationKind,
    NavigationQuery, Provider, Queries, Reply, SemanticToken, SemanticTokenKind,
    SemanticTokenModifiers, Severity, SourceId, SourceSpan, SyntaxProvider, TextPoint, TextSpan,
    offset_in,
};
use lsp_client::lsp_types::Uri;
use lsp_client::uri::{file_path, file_uri};
use platform::CanonPath;
use project_model::ProjectModel;
use tree_sitter_language::LanguageFn;

use java_syntax::{
    Annotation, Method, OPERATOR_WORDS, SpringFile, Type, method_cursor, parse_method_name,
    parse_query, query_tokens, token_at,
};

pub const LANGUAGE_ID: LanguageId = LanguageId("spring-data");

const SOURCE: SourceId = SourceId::new("spring-data");
/// The most source files the reader walks.
const MAX_FILES: usize = 20_000;
/// A word a document needs for the caret to be a place of the model.
const REPOSITORY: &str = "Repository";
/// The annotation of a written query.
const QUERY_ANNOTATION: &str = "@Query";
/// How many lines above the caret the annotation of a query may start.
const QUERY_LOOKBACK: u32 = 40;
/// A wait of a query for the reader of the sources.
const READY_STEP: Duration = Duration::from_millis(25);
/// A pause before a rebuild, so that a burst of edits rebuilds once.
const REBUILD_DELAY: Duration = Duration::from_millis(120);
/// The annotations that make a type a part of the model of the store.
const ENTITY: &str = "Entity";
const EMBEDDABLE: &str = "Embeddable";
const MAPPED_SUPERCLASS: &str = "MappedSuperclass";
/// The field of a type that the store does not persist.
const TRANSIENT: &str = "Transient";
/// The annotations that tie a property to another type of the model.
const RELATIONS: &[&str] = &[
    "ManyToOne",
    "OneToMany",
    "OneToOne",
    "ManyToMany",
    "ElementCollection",
];
const EMBEDDED: &str = "Embedded";
/// The annotations of a method of a repository and of its parameters.
const QUERY: &str = "Query";
const PARAM: &str = "Param";
/// The types of a method that do not stand for a condition of its name.
const PAGING_TYPES: &[&str] = &["Pageable", "Sort", "Limit", "ScrollPosition"];

#[cfg(test)]
thread_local! {
    static SPRING_PARSES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn spring_parses() -> usize {
    SPRING_PARSES.with(std::cell::Cell::get)
}

#[cfg(test)]
fn reset_spring_parses() {
    SPRING_PARSES.with(|parses| parses.set(0));
}

/// Parses a document of a query. The caller of a completion pays for this only when the
/// cheap check found a place of the model.
fn parse_spring_file(text: &str) -> SpringFile {
    #[cfg(test)]
    SPRING_PARSES.with(|parses| parses.set(parses.get() + 1));
    java_syntax::spring_file(text)
}

unsafe extern "C" fn no_grammar() -> *const () {
    std::ptr::null()
}

const NO_QUERIES: Queries = Queries {
    highlights: "",
    folds: "",
    indents: "",
    locals: "",
};

/// One persistent property of an entity: a field of the class or of a mapped superclass.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Property {
    pub name: String,
    /// The type without its package and its arguments: for a collection, its element.
    pub type_name: String,
    /// The entity or embeddable the property leads to, for a relation or an embedded
    /// property.
    pub target: Option<String>,
    /// The declaration, for Ctrl+B from a query.
    pub file: PathBuf,
    /// The line of the name, from zero.
    pub line: u32,
    /// The UTF-16 column of the name, where the caret of a jump lands.
    pub column: u32,
}

/// One type the store persists or embeds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Entity {
    /// The name a query knows the entity by.
    pub name: String,
    pub type_name: String,
    pub qualified: String,
    /// The type is embedded in another one and is no root of a query.
    pub embeddable: bool,
    pub file: PathBuf,
    /// The line of the type name, from zero.
    pub line: u32,
    /// The UTF-16 column of the type name, where the caret of a jump lands.
    pub column: u32,
    /// The fields of the type and of its mapped superclasses.
    pub properties: Vec<Property>,
}

/// One interface of the project, for the chain of the repositories that inherit a base
/// interface of their own before the one of the framework.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SuperInterface {
    /// The type parameters the interface declares, `T` and `ID` for `BaseRepo<T, ID>`.
    pub type_parameters: Vec<String>,
    pub supers: Vec<String>,
    pub super_arguments: Vec<Vec<String>>,
}

/// Everything the reader holds about a project at one moment.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SpringDataModel {
    pub entities: Vec<Entity>,
    /// The interfaces of the project by their simple name.
    pub interfaces: HashMap<String, SuperInterface>,
}

impl SpringDataModel {
    /// The entity or embeddable of a name, by the name of a query or of a type.
    pub fn entity(&self, name: &str) -> Option<&Entity> {
        let name = simple_name(name);
        self.entities
            .iter()
            .find(|entity| entity.name == name || entity.type_name == name)
    }

    /// The entities a query may name after `FROM` or `JOIN`.
    pub fn roots(&self) -> impl Iterator<Item = &Entity> {
        self.entities.iter().filter(|entity| !entity.embeddable)
    }

    /// The property of an entity of a name as the name of a method spells it.
    pub fn property<'a>(&'a self, entity: &'a Entity, name: &str) -> Option<&'a Property> {
        entity
            .properties
            .iter()
            .find(|property| property.name.eq_ignore_ascii_case(name))
    }

    /// Follows a path of properties, written as the camel words of a method name or
    /// separated by `_`. The error is the part that names no property and where it
    /// starts in the path.
    pub fn resolve<'a>(
        &'a self,
        entity: &'a Entity,
        path: &str,
    ) -> Result<Vec<Resolved<'a>>, (String, usize)> {
        if path.contains('_') {
            return self.resolve_separated(entity, path);
        }
        let mut resolved = Vec::new();
        let mut current = entity;
        let mut at = 0;
        while at < path.len() {
            let rest = &path[at..];
            let mut best: Option<&Property> = None;
            for property in &current.properties {
                let name = &property.name;
                let Some(head) = rest.get(..name.len()) else {
                    continue;
                };
                if !head.eq_ignore_ascii_case(name) {
                    continue;
                }
                let boundary = rest
                    .as_bytes()
                    .get(name.len())
                    .is_none_or(|next| !next.is_ascii_lowercase());
                if boundary && best.is_none_or(|found| found.name.len() < property.name.len()) {
                    best = Some(property);
                }
            }
            let Some(property) = best else {
                return Err((rest.to_owned(), at));
            };
            at += property.name.len();
            resolved.push(Resolved {
                property,
                start: at - property.name.len(),
                end: at,
            });
            if at == path.len() {
                break;
            }
            let Some(next) = property
                .target
                .as_deref()
                .and_then(|target| self.entity(target))
            else {
                return Err((path[at..].to_owned(), at));
            };
            current = next;
        }
        Ok(resolved)
    }

    fn resolve_separated<'a>(
        &'a self,
        entity: &'a Entity,
        path: &str,
    ) -> Result<Vec<Resolved<'a>>, (String, usize)> {
        let mut resolved = Vec::new();
        let mut current = entity;
        let mut at = 0;
        for part in path.split('_') {
            let start = at;
            at += part.len() + 1;
            let Some(property) = self.property(current, part) else {
                return Err((part.to_owned(), start));
            };
            resolved.push(Resolved {
                property,
                start,
                end: start + property.name.len(),
            });
            if at > path.len() {
                break;
            }
            let Some(next) = property
                .target
                .as_deref()
                .and_then(|target| self.entity(target))
            else {
                let rest = &path[(start + part.len()).min(path.len())..];
                return Err((rest.to_owned(), start + part.len()));
            };
            current = next;
        }
        Ok(resolved)
    }

    /// The entity every alias of a query names, following the joins of associations.
    fn aliases_of(&self, query: &java_syntax::Query) -> HashMap<String, String> {
        let mut known: HashMap<String, String> = HashMap::new();
        for alias in &query.aliases {
            match &alias.source {
                java_syntax::AliasSource::Entity(name) => {
                    if let Some(entity) = self.entity(name) {
                        known.insert(alias.name.clone(), entity.type_name.clone());
                    }
                }
                java_syntax::AliasSource::Path {
                    alias: base,
                    property,
                } => {
                    let Some(found) = known
                        .get(base)
                        .and_then(|name| self.entity(name))
                        .and_then(|entity| self.resolve(entity, property).ok())
                    else {
                        continue;
                    };
                    if let Some(target) = found
                        .last()
                        .and_then(|resolved| resolved.property.target.clone())
                    {
                        known.insert(alias.name.clone(), target);
                    }
                }
            }
        }
        known
    }
}

/// One property of a path, with the bytes it takes in the path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Resolved<'a> {
    pub property: &'a Property,
    pub start: usize,
    pub end: usize,
}

/// The type name of a source without its package, its arguments and its arrays.
fn simple_name(raw: &str) -> String {
    let raw = raw.trim();
    let raw = raw.strip_suffix("[]").unwrap_or(raw);
    let raw = raw.split('<').next().unwrap_or(raw);
    raw.rsplit('.').next().unwrap_or(raw).trim().to_owned()
}

fn has(annotations: &[Annotation], name: &str) -> bool {
    annotations.iter().any(|annotation| annotation.name == name)
}

/// The property a field gives the store, `None` for a static or transient field.
fn property_of_field(
    field: &java_syntax::Field,
    file: &Path,
    known: &HashMap<String, bool>,
) -> Option<Property> {
    if field.is_static || has(&field.annotations, TRANSIENT) {
        return None;
    }
    let collection = !field.type_arguments.is_empty();
    let element = field
        .type_arguments
        .first()
        .map(|argument| simple_name(argument))
        .unwrap_or_else(|| simple_name(&field.type_text));
    let relation = field
        .annotations
        .iter()
        .any(|annotation| RELATIONS.contains(&annotation.name.as_str()));
    let embedded = has(&field.annotations, EMBEDDED)
        || (!relation && !collection && known.get(&element).copied().unwrap_or(false));
    let target = (relation || embedded)
        .then(|| known.contains_key(&element).then_some(element.clone()))
        .flatten();
    Some(Property {
        name: field.name.clone(),
        type_name: element,
        target,
        file: file.to_path_buf(),
        line: field.line,
        column: field.column,
    })
}

/// Assembles the model of the entities of a project from its parsed sources.
pub fn build_model(files: &[(&Path, &SpringFile)]) -> SpringDataModel {
    let by_simple: HashMap<&str, (&Path, &Type)> = files
        .iter()
        .flat_map(|(path, file)| {
            file.types
                .iter()
                .map(move |kind| (kind.name.as_str(), (*path, kind)))
        })
        .collect();
    // The simple names of the types the model holds, and whether each one is embedded.
    let mut known: HashMap<String, bool> = HashMap::new();
    for (_, file) in files {
        for kind in &file.types {
            if has(&kind.annotations, ENTITY) {
                known.entry(kind.name.clone()).or_insert(false);
            } else if has(&kind.annotations, EMBEDDABLE) {
                known.entry(kind.name.clone()).or_insert(true);
            }
        }
    }
    let mut model = SpringDataModel::default();
    for (_, file) in files {
        for kind in &file.types {
            if !kind.is_interface {
                continue;
            }
            model.interfaces.insert(
                kind.name.clone(),
                SuperInterface {
                    type_parameters: kind.type_parameters.clone(),
                    supers: kind.supers.clone(),
                    super_arguments: kind.super_arguments.clone(),
                },
            );
        }
    }
    for (path, file) in files {
        for kind in &file.types {
            let entity = has(&kind.annotations, ENTITY);
            let embeddable = has(&kind.annotations, EMBEDDABLE);
            if !entity && !embeddable {
                continue;
            }
            let mut properties = Vec::new();
            for field in &kind.fields {
                if let Some(property) = property_of_field(field, path, &known) {
                    properties.push(property);
                }
            }
            // A mapped superclass contributes its fields to every type below it.
            let mut seen = HashSet::new();
            seen.insert(kind.name.as_str());
            let mut supers = kind.supers.clone();
            while let Some(super_name) = supers.pop() {
                let Some((super_path, super_type)) = by_simple.get(super_name.as_str()) else {
                    continue;
                };
                if !seen.insert(super_type.name.as_str()) {
                    continue;
                }
                if !has(&super_type.annotations, MAPPED_SUPERCLASS) {
                    continue;
                }
                for field in &super_type.fields {
                    if let Some(property) = property_of_field(field, super_path, &known) {
                        properties.push(property);
                    }
                }
                supers.extend(super_type.supers.iter().cloned());
            }
            properties.sort_by(|left, right| left.name.cmp(&right.name));
            model.entities.push(Entity {
                name: kind
                    .annotations
                    .iter()
                    .find(|annotation| annotation.name == ENTITY)
                    .and_then(|annotation| annotation.value.clone())
                    .filter(|name| !name.is_empty())
                    .unwrap_or_else(|| kind.name.clone()),
                type_name: kind.name.clone(),
                qualified: kind.qualified.clone(),
                embeddable,
                file: path.to_path_buf(),
                line: kind.line,
                column: kind.column,
                properties,
            });
        }
    }
    model
        .entities
        .sort_by(|left, right| (&left.name, &left.file).cmp(&(&right.name, &right.file)));
    model
}

/// The entity a repository interface works on, from the type arguments of the interface
/// of the framework it extends, directly or through an interface of the project. A
/// concrete `OwnerRepo extends BaseRepo<Owner, Integer>` takes `Owner` through the
/// parameters of `BaseRepo<T, ID>`.
fn repository_entity(model: &SpringDataModel, kind: &Type) -> Option<String> {
    let found = repository_of(
        model,
        &kind.supers,
        &kind.super_arguments,
        &HashMap::new(),
        0,
    )?;
    // A generic base interface itself has no entity of its own until a repository names
    // one, and its own parameters are no entity of the model.
    (!kind
        .type_parameters
        .iter()
        .any(|parameter| parameter == &found))
    .then_some(found)
}

fn repository_of(
    model: &SpringDataModel,
    supers: &[String],
    arguments: &[Vec<String>],
    bindings: &HashMap<String, String>,
    depth: usize,
) -> Option<String> {
    if depth > 16 {
        return None;
    }
    for (index, name) in supers.iter().enumerate() {
        let name = simple_name(name);
        let passed = arguments
            .get(index)
            .map(|arguments| substitute(arguments, bindings))
            .unwrap_or_default();
        if crate::beans::DATA_REPOSITORIES.contains(&name.as_str()) {
            return passed.first().map(|argument| simple_name(argument));
        }
        let Some(parent) = model.interfaces.get(&name) else {
            continue;
        };
        if parent.type_parameters.len() != passed.len() {
            continue;
        }
        let bound: HashMap<String, String> = parent
            .type_parameters
            .iter()
            .cloned()
            .zip(passed.iter().map(|argument| simple_name(argument)))
            .collect();
        if let Some(found) = repository_of(
            model,
            &parent.supers,
            &parent.super_arguments,
            &bound,
            depth + 1,
        ) {
            return Some(found);
        }
    }
    None
}

/// The type arguments of a supertype with the parameters of its interface replaced by
/// what a repository passed.
fn substitute(arguments: &[String], bindings: &HashMap<String, String>) -> Vec<String> {
    arguments
        .iter()
        .map(|argument| {
            bindings
                .get(argument)
                .cloned()
                .unwrap_or_else(|| argument.clone())
        })
        .collect()
}

/// A place of a repository that the model explains: the name of a query method or the
/// text of its query annotation.
enum Site<'a> {
    Method {
        entity: String,
        method: &'a Method,
    },
    Query {
        entity: String,
        annotation: &'a Annotation,
    },
}

/// The repository place at `offset`: a method name or the text of its query.
fn site_at<'a>(file: &'a SpringFile, model: &SpringDataModel, offset: usize) -> Option<Site<'a>> {
    for kind in &file.types {
        let Some(entity) = repository_entity(model, kind) else {
            continue;
        };
        for method in &kind.methods {
            let start = method.offset;
            let end = start + method.name.len();
            if start <= offset && offset <= end {
                return Some(Site::Method { entity, method });
            }
            for annotation in &method.annotations {
                if annotation.name != QUERY {
                    continue;
                }
                if let Some(range) = &annotation.value_range
                    && range.start <= offset
                    && offset <= range.end
                {
                    return Some(Site::Query { entity, annotation });
                }
            }
        }
    }
    None
}

/// Whether the caret may stand on a place of the model, read from the text alone: a method
/// of a repository interface, or the literal of a query annotation. A document with
/// neither never reaches the parse.
fn data_candidate(text: &str, offset: usize) -> bool {
    if text.contains(REPOSITORY) && on_method_line(text, offset) {
        return true;
    }
    text.contains(QUERY_ANNOTATION) && in_query_literal(text, offset)
}

/// Whether the line of the caret looks like a method declaration and not an annotation or
/// an import.
fn on_method_line(text: &str, offset: usize) -> bool {
    let offset = offset.min(text.len());
    let start = text[..offset].rfind('\n').map_or(0, |index| index + 1);
    let end = text[offset..]
        .find('\n')
        .map_or(text.len(), |index| offset + index);
    let line = text[start..end].trim();
    !line.starts_with('@')
        && !line.starts_with("import ")
        && line.contains('(')
        && (line.contains(')') || line.ends_with('(') || line.ends_with(','))
}

/// The part of a document the caret is in.
enum Literal {
    Code,
    Quoted,
    Block,
}

/// Whether the caret sits inside the literal of a `@Query` annotation: the annotation is a
/// few lines above, and the caret is between its quotes. A text block counts as one
/// literal.
fn in_query_literal(text: &str, offset: usize) -> bool {
    let offset = offset.min(text.len());
    let Some(annotation) = text[..offset].rfind(QUERY_ANNOTATION) else {
        return false;
    };
    if point_in(text, offset).line > point_in(text, annotation).line + QUERY_LOOKBACK {
        return false;
    }
    let bytes = text.as_bytes();
    let mut at = annotation;
    let mut state = Literal::Code;
    while at < offset {
        match state {
            Literal::Code => {
                if bytes[at] == b'"' {
                    if bytes.get(at + 1) == Some(&b'"') && bytes.get(at + 2) == Some(&b'"') {
                        state = Literal::Block;
                        at += 3;
                        continue;
                    }
                    state = Literal::Quoted;
                }
            }
            Literal::Quoted => {
                if bytes[at] == b'\\' {
                    at += 2;
                    continue;
                }
                if bytes[at] == b'"' {
                    state = Literal::Code;
                }
            }
            Literal::Block => {
                if bytes[at] == b'"'
                    && bytes.get(at + 1) == Some(&b'"')
                    && bytes.get(at + 2) == Some(&b'"')
                {
                    state = Literal::Code;
                    at += 3;
                    continue;
                }
            }
        }
        at += 1;
    }
    !matches!(state, Literal::Code)
}

fn is_native(annotation: &Annotation) -> bool {
    annotation
        .argument("nativeQuery")
        .is_some_and(|value| value.eq_ignore_ascii_case("true"))
}

/// The checks of the repository methods and the query annotations of one document.
pub fn diagnostics_for(
    file: &SpringFile,
    text: &str,
    model: &SpringDataModel,
) -> Vec<DiagnosticEntry> {
    let mut found = Vec::new();
    for kind in &file.types {
        let Some(entity_name) = repository_entity(model, kind) else {
            continue;
        };
        let entity = model.entity(&entity_name);
        for method in &kind.methods {
            found.extend(method_diagnostics(method, entity, model, text));
            let Some(annotation) = method
                .annotations
                .iter()
                .find(|annotation| annotation.name == QUERY)
            else {
                continue;
            };
            if is_native(annotation) {
                continue;
            }
            let (Some(value), Some(range)) = (&annotation.value, &annotation.value_range) else {
                continue;
            };
            let query = parse_query(value);
            let parameters = declared_parameters(method);
            found.extend(query_diagnostics(
                &query,
                &parameters,
                model,
                text,
                range.start,
            ));
        }
    }
    found.sort_by_key(|entry| (entry.span.start.line, entry.span.start.character));
    found
}

fn method_diagnostics(
    method: &Method,
    entity: Option<&Entity>,
    model: &SpringDataModel,
    text: &str,
) -> Vec<DiagnosticEntry> {
    let mut found = Vec::new();
    let Some(name) = parse_method_name(&method.name) else {
        return found;
    };
    // A method of no condition of its name takes whatever it is given.
    if name.conditions.is_empty() && name.orders.is_empty() {
        return found;
    }
    let needed = name.parameters();
    let actual = method
        .parameters
        .iter()
        .filter(|parameter| !PAGING_TYPES.contains(&simple_name(&parameter.type_text).as_str()))
        .count();
    if needed != actual {
        found.push(DiagnosticEntry {
            span: span_in(text, method.offset, method.offset + method.name.len()),
            severity: Severity::Warning,
            message: format!(
                "Method '{}' has {actual} parameters but the query needs {needed}",
                method.name
            ),
            unnecessary: false,
            code: Some("spring.data.methodQuery".to_owned()),
        });
    }
    let Some(entity) = entity else {
        return found;
    };
    let paths = name
        .conditions
        .iter()
        .map(|condition| &condition.property)
        .chain(name.orders.iter().map(|order| &order.property));
    for path in paths {
        if let Err((failed, at)) = model.resolve(entity, &path.text) {
            let start = method.offset + path.start + at;
            found.push(DiagnosticEntry {
                span: span_in(text, start, start + failed.len()),
                severity: Severity::Error,
                message: format!("Cannot resolve property '{failed}'"),
                unnecessary: false,
                code: Some("spring.data.query".to_owned()),
            });
        }
    }
    found
}

/// The names of the parameters of a method: the value of its `@Param` or its own name.
fn declared_parameters(method: &Method) -> HashSet<String> {
    method
        .parameters
        .iter()
        .filter_map(|parameter| {
            parameter
                .annotations
                .iter()
                .find(|annotation| annotation.name == PARAM)
                .and_then(|annotation| annotation.value.clone())
                .or_else(|| (!parameter.name.is_empty()).then(|| parameter.name.clone()))
        })
        .collect()
}

fn query_diagnostics(
    query: &java_syntax::Query,
    declared: &HashSet<String>,
    model: &SpringDataModel,
    text: &str,
    base: usize,
) -> Vec<DiagnosticEntry> {
    let mut found = Vec::new();
    for entity in &query.entities {
        if model.entity(&entity.text).is_none() {
            found.push(DiagnosticEntry {
                span: span_in(text, base + entity.range.start, base + entity.range.end),
                severity: Severity::Error,
                message: format!("Cannot resolve entity '{}'", entity.text),
                unnecessary: false,
                code: Some("spring.data.query".to_owned()),
            });
        }
    }
    let aliases = model.aliases_of(query);
    for path in &query.properties {
        let Some(entity) = aliases.get(&path.alias).and_then(|name| model.entity(name)) else {
            continue;
        };
        if let Err((failed, at)) = model.resolve(entity, &path.path) {
            let start = base + path.range.start + path.alias.len() + 1 + at;
            found.push(DiagnosticEntry {
                span: span_in(text, start, start + failed.len()),
                severity: Severity::Error,
                message: format!("Cannot resolve property '{failed}'"),
                unnecessary: false,
                code: Some("spring.data.query".to_owned()),
            });
        }
    }
    for parameter in &query.parameters {
        if !declared.contains(&parameter.text) {
            found.push(DiagnosticEntry {
                span: span_in(
                    text,
                    base + parameter.range.start,
                    base + parameter.range.end,
                ),
                severity: Severity::Error,
                message: format!("Cannot resolve parameter '{}'", parameter.text),
                unnecessary: false,
                code: Some("spring.data.query".to_owned()),
            });
        }
    }
    found
}

/// The completion of a method name and of the text of a query annotation.
pub fn completions_for(
    file: &SpringFile,
    text: &str,
    offset: usize,
    model: &SpringDataModel,
) -> Vec<CompletionEntry> {
    match site_at(file, model, offset) {
        Some(Site::Method { entity, method }) => {
            let Some(entity) = model.entity(&entity) else {
                return Vec::new();
            };
            method_completions(entity, model, method, offset, text)
        }
        Some(Site::Query {
            entity, annotation, ..
        }) => {
            if is_native(annotation) {
                return Vec::new();
            }
            let Some(owner) = model.entity(&entity) else {
                return Vec::new();
            };
            let (Some(value), Some(range)) = (&annotation.value, &annotation.value_range) else {
                return Vec::new();
            };
            if offset < range.start || offset > range.end {
                return Vec::new();
            }
            query_completions(value, offset - range.start, owner, model, text, range.start)
        }
        None => Vec::new(),
    }
}

fn method_completions(
    entity: &Entity,
    model: &SpringDataModel,
    method: &Method,
    offset: usize,
    text: &str,
) -> Vec<CompletionEntry> {
    let caret = offset.saturating_sub(method.offset).min(method.name.len());
    let Some(cursor) = method_cursor(&method.name, caret) else {
        return Vec::new();
    };
    let typed = &method.name[cursor.start..caret];
    let prefix = &method.name[..cursor.start];
    let span = Some(span_in(text, offset - typed.len(), offset));
    let resolved = model.resolve(entity, typed).ok().filter(|found| {
        found
            .last()
            .is_some_and(|resolved| resolved.end == typed.len())
    });
    let mut entries = Vec::new();
    if resolved.is_some() {
        // A whole property is typed: the name continues with an operator or a conjunction.
        for word in OPERATOR_WORDS
            .iter()
            .copied()
            .chain(["And", "Or", "OrderBy", "IgnoreCase"])
        {
            entries.push(CompletionEntry {
                label: word.to_owned(),
                label_details: None,
                detail: None,
                filter_text: Some(format!("{prefix}{typed}{word}")),
                sort_text: Some(format!("0{word}")),
                insert_text: Some(word.to_owned()),
                span: Some(span_in(text, offset, offset)),
                snippet: false,
                additional_edits: Vec::new(),
                command: None,
                resolve: None,
                kind: CompletionKind::Keyword,
                deprecated: false,
                documentation: None,
            });
        }
        // An ordering takes its direction after the property.
        if cursor.boundary == java_syntax::Boundary::OrderBy {
            for word in ["Asc", "Desc"] {
                entries.push(CompletionEntry {
                    label: word.to_owned(),
                    label_details: None,
                    detail: None,
                    filter_text: Some(format!("{prefix}{typed}{word}")),
                    sort_text: Some(format!("0{word}")),
                    insert_text: Some(word.to_owned()),
                    span: Some(span_in(text, offset, offset)),
                    snippet: false,
                    additional_edits: Vec::new(),
                    command: None,
                    resolve: None,
                    kind: CompletionKind::Keyword,
                    deprecated: false,
                    documentation: None,
                });
            }
        }
        for resolved in resolved.iter().flatten() {
            let property = resolved.property;
            if let Some(target) = property
                .target
                .as_deref()
                .and_then(|name| model.entity(name))
            {
                for nested in &target.properties {
                    let word =
                        format!("{}{}", capitalize(&property.name), capitalize(&nested.name));
                    entries.push(completion_entry(
                        &word,
                        prefix,
                        span,
                        CompletionKind::Property,
                    ));
                }
            }
        }
    } else {
        for property in &entity.properties {
            let word = capitalize(&property.name);
            entries.push(completion_entry(
                &word,
                prefix,
                span,
                CompletionKind::Property,
            ));
        }
    }
    entries
}

fn completion_entry(
    word: &str,
    prefix: &str,
    span: Option<TextSpan>,
    kind: CompletionKind,
) -> CompletionEntry {
    CompletionEntry {
        label: word.to_owned(),
        label_details: None,
        detail: None,
        filter_text: Some(format!("{prefix}{word}")),
        sort_text: Some(format!("0{word}")),
        insert_text: Some(word.to_owned()),
        span,
        snippet: false,
        additional_edits: Vec::new(),
        command: None,
        resolve: None,
        kind,
        deprecated: false,
        documentation: None,
    }
}

fn capitalize(name: &str) -> String {
    let mut characters = name.chars();
    let Some(first) = characters.next() else {
        return String::new();
    };
    let mut out = String::with_capacity(name.len());
    out.extend(first.to_uppercase());
    out.push_str(characters.as_str());
    out
}

fn query_completions(
    query_text: &str,
    caret: usize,
    _owner: &Entity,
    model: &SpringDataModel,
    text: &str,
    base: usize,
) -> Vec<CompletionEntry> {
    let tokens = query_tokens(query_text);
    let start = word_start(query_text, caret);
    let typed = &query_text[start..caret];
    let span = Some(span_in(text, base + start, base + caret));
    let before = tokens
        .iter()
        .filter(|token| token.range.end <= start)
        .collect::<Vec<_>>();
    let declaring = before
        .iter()
        .rev()
        .find(|token| {
            token.kind == java_syntax::TokenKind::Keyword
                || token.kind == java_syntax::TokenKind::Identifier
                || token.kind == java_syntax::TokenKind::Symbol
        })
        .map(|token| token.text.to_ascii_uppercase());
    // `alias.` offers the properties of the alias.
    if let Some(last) = before.last()
        && last.text == "."
        && let Some(alias) = before.get(before.len().saturating_sub(2))
    {
        let query = parse_query(query_text);
        let aliases = model.aliases_of(&query);
        if let Some(entity) = aliases.get(&alias.text).and_then(|name| model.entity(name)) {
            return entity
                .properties
                .iter()
                .filter(|property| starts_ignore_case(&property.name, typed))
                .map(|property| {
                    completion_entry(&property.name, "", span, CompletionKind::Property)
                })
                .collect();
        }
        return Vec::new();
    }
    if declaring.is_some_and(|word| word == "FROM" || word == "JOIN") {
        return model
            .roots()
            .filter(|entity| starts_ignore_case(&entity.name, typed))
            .map(|entity| completion_entry(&entity.name, "", span, CompletionKind::Class))
            .collect();
    }
    Vec::new()
}

fn starts_ignore_case(name: &str, typed: &str) -> bool {
    name.get(..typed.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(typed))
}

/// The start of the word at `offset` in a query: a name after a dot or a space.
fn word_start(text: &str, offset: usize) -> usize {
    let bytes = text.as_bytes();
    let mut at = offset.min(text.len());
    while at > 0 {
        let before = bytes[at - 1];
        if before.is_ascii_alphanumeric() || before == b'_' || before == b'$' {
            at -= 1;
        } else {
            break;
        }
    }
    at
}

/// The declarations the name at `offset` leads to: a field of an entity, or the class of
/// an entity.
pub fn definitions_for(
    file: &SpringFile,
    offset: usize,
    model: &SpringDataModel,
) -> Vec<SourceSpan> {
    match site_at(file, model, offset) {
        Some(Site::Method { entity, method }) => {
            let Some(entity) = model.entity(&entity) else {
                return Vec::new();
            };
            method_definition(entity, model, method, offset)
                .into_iter()
                .collect()
        }
        Some(Site::Query {
            entity, annotation, ..
        }) => {
            if is_native(annotation) {
                return Vec::new();
            }
            if model.entity(&entity).is_none() {
                return Vec::new();
            }
            let (Some(value), Some(range)) = (&annotation.value, &annotation.value_range) else {
                return Vec::new();
            };
            if offset < range.start || offset > range.end {
                return Vec::new();
            }
            query_definition(value, offset - range.start, model)
                .into_iter()
                .collect()
        }
        None => Vec::new(),
    }
}

fn method_definition(
    entity: &Entity,
    model: &SpringDataModel,
    method: &Method,
    offset: usize,
) -> Option<SourceSpan> {
    let name = &method.name;
    let caret = offset.saturating_sub(method.offset).min(name.len());
    let name = parse_method_name(name)?;
    let paths = name
        .conditions
        .iter()
        .map(|condition| &condition.property)
        .chain(name.orders.iter().map(|order| &order.property));
    for path in paths {
        if caret < path.start || caret > path.end {
            continue;
        }
        let resolved = model.resolve(entity, &path.text).ok()?;
        let inside = caret.saturating_sub(path.start).min(path.text.len());
        let found = resolved
            .iter()
            .find(|resolved| resolved.start <= inside && inside <= resolved.end)
            .or_else(|| resolved.last())?;
        return property_span(found.property);
    }
    None
}

fn query_definition(query_text: &str, caret: usize, model: &SpringDataModel) -> Option<SourceSpan> {
    let query = parse_query(query_text);
    let token = token_at(&query.tokens, caret)?;
    // The name of an entity of `FROM` or of a `JOIN`.
    if let Some(entity) = query
        .entities
        .iter()
        .find(|entity| entity.range.contains(&token.range.start))
        .and_then(|entity| model.entity(&entity.text))
    {
        return entity_span(entity);
    }
    // A name written from an alias: its property, or the entity of the alias itself.
    let aliases = model.aliases_of(&query);
    if let Some(path) = query
        .properties
        .iter()
        .find(|path| path.range.start <= token.range.start && token.range.end <= path.range.end)
    {
        let entity = aliases
            .get(&path.alias)
            .and_then(|name| model.entity(name))?;
        let inside = token
            .range
            .start
            .saturating_sub(path.range.start + path.alias.len() + 1);
        let resolved = model.resolve(entity, &path.path).ok()?;
        let found = resolved
            .iter()
            .find(|resolved| resolved.start <= inside && inside <= resolved.end)
            .or_else(|| resolved.last())?;
        return property_span(found.property);
    }
    if let Some(alias) = query.aliases.iter().find(|alias| alias.name == token.text)
        && let Some(alias_name) = aliases.get(&alias.name)
    {
        return model.entity(alias_name).and_then(entity_span);
    }
    None
}

fn property_span(property: &Property) -> Option<SourceSpan> {
    span_of(&property.file, property.line, property.column)
}

fn entity_span(entity: &Entity) -> Option<SourceSpan> {
    span_of(&entity.file, entity.line, entity.column)
}

fn span_of(path: &Path, line: u32, character: u32) -> Option<SourceSpan> {
    let canonical = CanonPath::new(path).ok()?;
    let uri = file_uri(&canonical)?;
    Some(SourceSpan {
        uri: uri.as_str().to_owned(),
        span: TextSpan {
            start: TextPoint { line, character },
            end: TextPoint { line, character },
        },
    })
}

/// The colours of the keywords and the placeholders of every query of one document.
pub fn tokens_for(file: &SpringFile, text: &str) -> Vec<SemanticToken> {
    let mut tokens = Vec::new();
    for kind in &file.types {
        for method in &kind.methods {
            let Some(annotation) = method.annotations.iter().find(|a| a.name == QUERY) else {
                continue;
            };
            if is_native(annotation) {
                continue;
            }
            let (Some(value), Some(range)) = (&annotation.value, &annotation.value_range) else {
                continue;
            };
            for token in query_tokens(value) {
                let kind = match token.kind {
                    java_syntax::TokenKind::Keyword => SemanticTokenKind::Keyword,
                    java_syntax::TokenKind::Parameter => SemanticTokenKind::Parameter,
                    java_syntax::TokenKind::Number => SemanticTokenKind::Number,
                    java_syntax::TokenKind::String => SemanticTokenKind::String,
                    _ => continue,
                };
                tokens.push(SemanticToken {
                    span: span_in(
                        text,
                        range.start + token.range.start,
                        range.start + token.range.end,
                    ),
                    kind,
                    modifiers: SemanticTokenModifiers::default(),
                });
            }
        }
    }
    tokens.sort_by_key(|token| (token.span.start.line, token.span.start.character));
    tokens
}

fn point_in(text: &str, offset: usize) -> TextPoint {
    let offset = offset.min(text.len());
    let line_start = text[..offset].rfind('\n').map_or(0, |index| index + 1);
    let character = text[line_start..offset].encode_utf16().count() as u32;
    let line = text[..line_start]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count() as u32;
    TextPoint { line, character }
}

fn span_in(text: &str, start: usize, end: usize) -> TextSpan {
    TextSpan {
        start: point_in(text, start),
        end: point_in(text, end),
    }
}

/// The index of the entities of one project. Cloning it shares the index.
#[derive(Clone)]
pub struct SpringDataIndex {
    inner: Arc<Inner>,
}

/// An index that starts reading as soon as it is made: its own root first, then the
/// source roots of the project model.
pub fn spring_data_index(root: PathBuf) -> Arc<SpringDataIndex> {
    let index = Arc::new(SpringDataIndex {
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

impl SpringDataIndex {
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

    /// The text of an open document, as the server names it with a URI.
    pub fn document_uri_changed(&self, uri: &str, text: Option<Arc<str>>) {
        let Some(path) = Uri::from_str(uri).ok().as_ref().and_then(file_path) else {
            return;
        };
        self.document_changed(&path, text);
    }

    /// The text of an open document, without reading the disk: the caller may be the
    /// interface thread, which never waits on a file.
    pub fn open_text_of_uri(&self, uri: &str) -> Option<Arc<str>> {
        let path = file_path(&Uri::from_str(uri).ok()?)?;
        self.lock_state()
            .open
            .get(&path)
            .map(|document| document.text.clone())
    }

    /// The text of a document: the open one, else the file on disk.
    pub fn text_of_uri(&self, uri: &str) -> Option<Arc<str>> {
        if let Some(text) = self.open_text_of_uri(uri) {
            return Some(text);
        }
        let path = file_path(&Uri::from_str(uri).ok()?)?;
        std::fs::read_to_string(path).ok().map(Arc::from)
    }

    /// The model as it stands now, for a caller that may not wait.
    pub fn model(&self) -> Arc<SpringDataModel> {
        self.snapshot()
    }

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

    fn lock_state(&self) -> std::sync::MutexGuard<'_, Load> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    fn generation(&self) -> u64 {
        self.lock_state().generation
    }

    fn snapshot(&self) -> Arc<SpringDataModel> {
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
    snapshot: RwLock<Arc<SpringDataModel>>,
    /// The checks the user turned off: one that is off is not computed at all.
    inspection_checks: Mutex<lang_api::DisabledInspections>,
}

impl Inner {
    fn new(root: PathBuf) -> Self {
        Self {
            root,
            state: Mutex::new(Load::default()),
            ready: Condvar::new(),
            snapshot: RwLock::new(Arc::new(SpringDataModel::default())),
            inspection_checks: Mutex::new(lang_api::DisabledInspections::default()),
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
    parsed: Arc<SpringFile>,
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
) -> (SpringDataModel, HashMap<PathBuf, Cached>) {
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
    let files: Vec<(&Path, &SpringFile)> = parsed
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

impl EmbeddedTextProvider for SpringDataIndex {
    fn completions(&self, _path: &Path, text: &str, offset: usize) -> Vec<CompletionEntry> {
        let file = java_syntax::spring_file(text);
        completions_for(&file, text, offset, &self.snapshot())
    }

    fn diagnostics(&self, _path: &Path, text: &str) -> Vec<DiagnosticEntry> {
        if !self.check_enabled("spring.data.methodQuery")
            && !self.check_enabled("spring.data.query")
        {
            return Vec::new();
        }
        let file = java_syntax::spring_file(text);
        let mut found = diagnostics_for(&file, text, &self.snapshot());
        found.retain(|entry| {
            entry
                .code
                .as_deref()
                .is_none_or(|code| self.check_enabled(code))
        });
        found
    }

    fn definitions(&self, _path: &Path, text: &str, offset: usize) -> Vec<SourceSpan> {
        let file = java_syntax::spring_file(text);
        definitions_for(&file, offset, &self.snapshot())
    }

    fn tokens(&self, _path: &Path, text: &str) -> Vec<SemanticToken> {
        let file = java_syntax::spring_file(text);
        tokens_for(&file, text)
    }

    fn updates(&self) -> Option<lang_api::Replies<()>> {
        let index = self.clone();
        let (mut reply, replies) = lang_api::channel::<()>();
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
                if reply
                    .send(Answer::new((), Confidence::Certain, SOURCE))
                    .is_err()
                {
                    return;
                }
                sent = Some(generation);
            }
        });
        Some(replies)
    }
}

impl SyntaxProvider for SpringDataIndex {
    fn grammar(&self) -> LanguageFn {
        // The editor colours a source file with `file-syntax`; this language exists for
        // the places inside its text.
        unsafe { LanguageFn::from_raw(no_grammar) }
    }

    fn queries(&self) -> &Queries {
        &NO_QUERIES
    }
}

/// The support of the entities and the queries of one project, for a window that has no
/// server: the same readers the application runs, reachable through `lang-api`.
pub struct SpringDataSupport {
    root: PathBuf,
    index: Arc<SpringDataIndex>,
    completion: NativeCompletion,
    navigation: NativeNavigation,
}

/// A support that starts reading as soon as it is made.
pub fn spring_data_support(root: PathBuf) -> Arc<SpringDataSupport> {
    let index = spring_data_index(root.clone());
    let completion = NativeCompletion {
        data: index.clone(),
        fallback: None,
    };
    let navigation = NativeNavigation {
        data: index.clone(),
        fallback: None,
    };
    Arc::new(SpringDataSupport {
        root,
        index,
        completion,
        navigation,
    })
}

impl SpringDataSupport {
    pub fn index(&self) -> &Arc<SpringDataIndex> {
        &self.index
    }
}

impl LanguageSupport for SpringDataSupport {
    fn id(&self) -> LanguageId {
        LANGUAGE_ID
    }

    fn matches(&self, path: &Path) -> bool {
        path.extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("java"))
            && path.starts_with(&self.root)
    }

    fn syntax(&self) -> &dyn SyntaxProvider {
        self.index.as_ref()
    }

    fn embedded_text(&self) -> Option<&dyn EmbeddedTextProvider> {
        Some(self.index.as_ref())
    }

    fn completion(&self) -> Option<&lang_api::CompletionProvider> {
        Some(&self.completion)
    }

    fn navigation(&self) -> Option<&lang_api::NavigationProvider> {
        Some(&self.navigation)
    }

    fn server(&self) -> Option<&dyn lang_api::SemanticServer> {
        Some(self)
    }
}

impl lang_api::SemanticServer for SpringDataSupport {
    fn open(&self, uri: &str, _version: i32, text: Arc<str>) {
        self.index.document_uri_changed(uri, Some(text));
    }

    fn change(
        &self,
        uri: &str,
        _version: i32,
        text: Arc<str>,
        _edits: Vec<lang_api::DocumentEdit>,
    ) {
        self.index.document_uri_changed(uri, Some(text));
    }

    fn save(&self, _uri: &str) {}

    fn close(&self, uri: &str) {
        self.index.document_uri_changed(uri, None);
    }

    fn version(&self, uri: &str) -> Option<i32> {
        self.index.text_of_uri(uri).is_some().then_some(0)
    }
}

/// The completion of the places of the text of a document, then the one of the language
/// around it.
pub(crate) struct NativeCompletion {
    pub data: Arc<SpringDataIndex>,
    pub fallback: Option<Arc<dyn Provider<DocumentSpot, CompletionList>>>,
}

/// The declarations of the places of the text of a document, then the ones of the
/// language around it.
pub(crate) struct NativeNavigation {
    pub data: Arc<SpringDataIndex>,
    pub fallback: Option<Arc<dyn Provider<NavigationQuery, Vec<SourceSpan>>>>,
}

impl Provider<DocumentSpot, CompletionList> for NativeCompletion {
    fn start(&self, query: DocumentSpot, cancel: Cancel, mut reply: Reply<CompletionList>) {
        // The common completion of a Java file never reaches a thread of this provider:
        // the text alone says whether it may hold a place of the model.
        let Some(text) = self.data.open_text_of_uri(&query.uri) else {
            delegate_completion(self.fallback.clone(), query, cancel, reply);
            return;
        };
        let offset = offset_in(&text, query.point);
        if !data_candidate(&text, offset) {
            delegate_completion(self.fallback.clone(), query, cancel, reply);
            return;
        }
        let data = self.data.clone();
        let fallback = self.fallback.clone();
        thread::spawn(move || {
            let file = parse_spring_file(&text);
            let generation = data.generation();
            if !data.wait_ready(generation, &reply) {
                return;
            }
            let model = data.snapshot();
            if site_at(&file, &model, offset).is_none() {
                delegate_completion(fallback, query, cancel, reply);
                return;
            }
            let entries = completions_for(&file, &text, offset, &model);
            if entries.is_empty() {
                delegate_completion(fallback, query, cancel, reply);
                return;
            }
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

fn delegate_completion(
    fallback: Option<Arc<dyn Provider<DocumentSpot, CompletionList>>>,
    query: DocumentSpot,
    cancel: Cancel,
    reply: Reply<CompletionList>,
) {
    match fallback {
        Some(provider) => provider.start(query, cancel, reply),
        None => {
            let _ = reply;
        }
    }
}

impl Provider<NavigationQuery, Vec<SourceSpan>> for NativeNavigation {
    fn start(&self, query: NavigationQuery, cancel: Cancel, mut reply: Reply<Vec<SourceSpan>>) {
        if query.kind != NavigationKind::Definition {
            delegate_navigation(self.fallback.clone(), query, cancel, reply);
            return;
        }
        let Some(text) = self.data.open_text_of_uri(&query.spot.uri) else {
            delegate_navigation(self.fallback.clone(), query, cancel, reply);
            return;
        };
        let offset = offset_in(&text, query.spot.point);
        if !data_candidate(&text, offset) {
            delegate_navigation(self.fallback.clone(), query, cancel, reply);
            return;
        }
        let data = self.data.clone();
        let fallback = self.fallback.clone();
        thread::spawn(move || {
            let file = parse_spring_file(&text);
            let generation = data.generation();
            if !data.wait_ready(generation, &reply) {
                return;
            }
            let model = data.snapshot();
            if site_at(&file, &model, offset).is_none() {
                delegate_navigation(fallback, query, cancel, reply);
                return;
            }
            let spans = definitions_for(&file, offset, &model);
            if spans.is_empty() {
                delegate_navigation(fallback, query, cancel, reply);
                return;
            }
            let _ = reply.send(Answer::new(spans, Confidence::Certain, SOURCE));
        });
    }
}

fn delegate_navigation(
    fallback: Option<Arc<dyn Provider<NavigationQuery, Vec<SourceSpan>>>>,
    query: NavigationQuery,
    cancel: Cancel,
    reply: Reply<Vec<SourceSpan>>,
) {
    match fallback {
        Some(provider) => provider.start(query, cancel, reply),
        None => {
            let _ = reply;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ENTITIES: &str = r#"package demo;

import jakarta.persistence.Embeddable;
import jakarta.persistence.Embedded;
import jakarta.persistence.Entity;
import jakarta.persistence.Id;
import jakarta.persistence.ManyToOne;
import jakarta.persistence.MappedSuperclass;
import jakarta.persistence.OneToMany;
import jakarta.persistence.Transient;
import java.util.List;

@MappedSuperclass
class Person {
    private String firstName;
    private String lastName;
}

@Entity
class Owner extends Person {
    @Id
    private Integer id;
    private String city;
    @Embedded
    private Address address;
    @OneToMany
    private List<Pet> pets;
    private static String cache;
    @Transient
    private String scratch;
}

@Embeddable
class Address {
    private String city;
    private String street;
}

@Entity
class Pet {
    private String name;
    @ManyToOne
    private Owner owner;
}
"#;

    fn parsed(files: &[(&str, &str)]) -> Vec<(PathBuf, SpringFile)> {
        files
            .iter()
            .map(|(name, text)| {
                (
                    PathBuf::from(format!("C:/p/src/demo/{name}")),
                    java_syntax::spring_file(text),
                )
            })
            .collect()
    }

    fn model_of(files: &[(&str, &str)]) -> SpringDataModel {
        let files = parsed(files);
        let refs: Vec<(&Path, &SpringFile)> = files
            .iter()
            .map(|(path, file)| (path.as_path(), file))
            .collect();
        build_model(&refs)
    }

    #[test]
    fn the_entities_of_a_project_keep_their_properties_and_their_bases() {
        let model = model_of(&[("Entities.java", ENTITIES)]);
        let names: Vec<&str> = model
            .entities
            .iter()
            .map(|entity| entity.name.as_str())
            .collect();
        assert_eq!(names, ["Address", "Owner", "Pet"]);
        let owner = model
            .entity("Owner")
            .expect("the entity of the name of its annotation");
        assert_eq!(owner.type_name, "Owner");
        let properties: Vec<&str> = owner
            .properties
            .iter()
            .map(|property| property.name.as_str())
            .collect();
        assert_eq!(
            properties,
            ["address", "city", "firstName", "id", "lastName", "pets"],
            "the mapped superclass adds its fields, the static and transient ones stay out"
        );
        assert_eq!(
            model
                .property(owner, "address")
                .and_then(|p| p.target.as_deref()),
            Some("Address")
        );
        assert_eq!(
            model
                .property(owner, "pets")
                .and_then(|p| p.target.as_deref()),
            Some("Pet")
        );
        assert_eq!(
            model
                .property(owner, "pets")
                .map(|property| property.type_name.as_str()),
            Some("Pet"),
            "a collection takes its element"
        );
        assert_eq!(
            model
                .property(owner, "pets")
                .map(|property| property.file.clone()),
            Some(PathBuf::from("C:/p/src/demo/Entities.java"))
        );
        let pet = model.entity("Pet").unwrap();
        assert_eq!(
            model
                .property(pet, "owner")
                .and_then(|p| p.target.as_deref()),
            Some("Owner")
        );
        let address = model.entity("Address").unwrap();
        assert!(address.embeddable);
        assert_eq!(
            model.roots().count(),
            2,
            "the embeddable is no root of a query"
        );
    }

    #[test]
    fn an_entity_name_of_its_annotation_names_it_in_a_query() {
        let source = r#"package demo;
@Entity(name = "owners")
class Owner {}
"#;
        let model = model_of(&[("Owner.java", source)]);
        assert_eq!(model.entities[0].name, "owners");
        assert!(model.entity("owners").is_some());
        assert!(
            model.entity("Owner").is_some(),
            "the type name still finds it"
        );
    }

    /// The real PetClinic needs `IDE_CORPUS`: its owner repository follows the naming
    /// conventions of the framework, and its entities extend mapped superclasses.
    ///
    /// `cargo test -p java-lang spring_data -- --ignored --test-threads=1`
    #[test]
    #[ignore = "needs IDE_CORPUS with the PetClinic sources"]
    fn petclinic_entities_and_its_owner_repository_follow_the_conventions() {
        let corpus = std::env::var("IDE_CORPUS").expect("IDE_CORPUS names the corpus");
        let root = PathBuf::from(corpus).join("src/petclinic/src/main/java");
        let mut paths = Vec::new();
        collect_sources(&root, &mut paths);
        assert!(
            !paths.is_empty(),
            "the sources of PetClinic are in {root:?}"
        );
        let files: Vec<(PathBuf, SpringFile)> = paths
            .iter()
            .filter_map(|path| {
                let text = std::fs::read_to_string(path).ok()?;
                Some((path.clone(), java_syntax::spring_file(&text)))
            })
            .collect();
        let refs: Vec<(&Path, &SpringFile)> = files
            .iter()
            .map(|(path, file)| (path.as_path(), file))
            .collect();
        let model = build_model(&refs);
        for name in ["Owner", "Pet", "Vet"] {
            assert!(model.entity(name).is_some(), "the entity {name} is found");
        }
        let owner = model.entity("Owner").unwrap();
        let properties: Vec<&str> = owner
            .properties
            .iter()
            .map(|property| property.name.as_str())
            .collect();
        assert!(properties.contains(&"lastName"), "{properties:?}");
        assert!(properties.contains(&"firstName"), "{properties:?}");
        assert_eq!(
            model
                .property(owner, "pets")
                .and_then(|p| p.target.as_deref()),
            Some("Pet")
        );
        let repository = files
            .iter()
            .find(|(path, _)| path.ends_with("owner/OwnerRepository.java"))
            .expect("the owner repository");
        let text = std::fs::read_to_string(&repository.0).unwrap();
        let found = diagnostics_for(&repository.1, &text, &model);
        assert!(found.is_empty(), "{found:?}");
        println!(
            "entities of PetClinic: {}",
            model
                .entities
                .iter()
                .filter(|entity| !entity.embeddable)
                .count()
        );
    }

    #[test]
    fn a_path_of_a_method_name_follows_the_embedded_and_the_related_types() {
        let model = model_of(&[("Entities.java", ENTITIES)]);
        let owner = model.entity("Owner").unwrap();
        let resolved = model.resolve(owner, "AddressCity").unwrap();
        assert_eq!(
            resolved
                .iter()
                .map(|resolved| resolved.property.name.as_str())
                .collect::<Vec<_>>(),
            ["address", "city"]
        );
        let resolved = model.resolve(owner, "PetsName").unwrap();
        assert_eq!(resolved.last().unwrap().property.name, "name");
        let resolved = model.resolve(owner, "Address_City").unwrap();
        assert_eq!(resolved.len(), 2);
        let (failed, at) = model.resolve(owner, "AddressZip").unwrap_err();
        assert_eq!(failed, "Zip");
        assert_eq!(at, "Address".len());
        let (failed, _) = model.resolve(owner, "Nickname").unwrap_err();
        assert_eq!(failed, "Nickname");
    }

    #[test]
    fn a_repository_method_is_checked_against_the_entity_of_its_interface() {
        let source = r#"package demo;

import java.util.List;
import org.springframework.data.domain.Pageable;
import org.springframework.data.jpa.repository.JpaRepository;

interface OwnerRepository extends JpaRepository<Owner, Integer> {
    List<Owner> findByLastNameStartingWith(String lastName, Pageable pageable);
    List<Owner> findByNickname(String nickname);
    List<Owner> findByCity(String city, String other);
}
"#;
        let model = model_of(&[("Entities.java", ENTITIES)]);
        let files = parsed(&[("OwnerRepository.java", source)]);
        let (path, file) = &files[0];
        let _ = path;
        let found = diagnostics_for(file, source, &model);
        let messages: Vec<&str> = found.iter().map(|entry| entry.message.as_str()).collect();
        assert_eq!(
            messages,
            [
                "Cannot resolve property 'Nickname'",
                "Method 'findByCity' has 2 parameters but the query needs 1"
            ],
            "{found:?}"
        );
        let error = &found[0];
        assert_eq!(error.severity, Severity::Error);
        assert_eq!(error.span.start.line, 8);
        assert_eq!(
            error.span.start.character,
            ("    List<Owner> findBy").len() as u32
        );
        assert_eq!(found[1].severity, Severity::Warning);
    }

    #[test]
    fn a_query_is_checked_for_its_entity_its_properties_and_its_parameters() {
        let source = r#"package demo;

import java.util.List;
import org.springframework.data.jpa.repository.JpaRepository;
import org.springframework.data.jpa.repository.Query;
import org.springframework.data.repository.query.Param;

interface OwnerRepository extends JpaRepository<Owner, Integer> {
    @Query("select o from Owner o join o.pets p where p.name = :petName and o.unknown = :missing")
    List<Owner> find(@Param("petName") String petName, String other);

    @Query("select o from Nope n")
    List<Owner> broken();

    @Query(value = "select x from Nope x where x.nothing = :gone", nativeQuery = true)
    List<Owner> native(String gone);
}
"#;
        let model = model_of(&[("Entities.java", ENTITIES)]);
        let files = parsed(&[("OwnerRepository.java", source)]);
        let found = diagnostics_for(&files[0].1, source, &model);
        let messages: Vec<&str> = found.iter().map(|entry| entry.message.as_str()).collect();
        assert_eq!(
            messages,
            [
                "Cannot resolve property 'unknown'",
                "Cannot resolve parameter 'missing'",
                "Cannot resolve entity 'Nope'"
            ],
            "{found:?}"
        );
        let property = &found[0];
        let line = source
            .lines()
            .nth(property.span.start.line as usize)
            .unwrap();
        assert!(
            line[property.span.start.character as usize..].starts_with("unknown"),
            "the error underlines the property"
        );
        assert_eq!(
            &line[property.span.end.character as usize..],
            " = :missing\")"
        );
    }

    #[test]
    fn a_method_name_is_completed_with_the_properties_and_then_the_operators() {
        let source = r#"package demo;

import java.util.List;
import org.springframework.data.jpa.repository.JpaRepository;

interface OwnerRepository extends JpaRepository<Owner, Integer> {
    List<Owner> findByLastNa(String name);
    List<Owner> findByLastName(String name);
}
"#;
        let model = model_of(&[("Entities.java", ENTITIES)]);
        let files = parsed(&[("OwnerRepository.java", source)]);
        let file = &files[0].1;
        let partial = source.find("findByLastNa").unwrap() + "findByLastNa".len();
        let entries = completions_for(file, source, partial, &model);
        let labels: Vec<&str> = entries.iter().map(|entry| entry.label.as_str()).collect();
        assert!(labels.contains(&"LastName"), "{labels:?}");
        assert!(labels.contains(&"FirstName"), "{labels:?}");
        let span = entries[0]
            .span
            .expect("a completion replaces the typed part");
        assert_eq!(
            span.start.character as usize,
            "    List<Owner> findBy".len()
        );
        let last_name = entries
            .iter()
            .find(|entry| entry.label == "LastName")
            .expect("the property is offered");
        assert_eq!(last_name.filter_text.as_deref(), Some("findByLastName"));

        let done = source.find("findByLastName(").unwrap() + "findByLastName".len();
        let entries = completions_for(file, source, done, &model);
        let labels: Vec<&str> = entries.iter().map(|entry| entry.label.as_str()).collect();
        assert!(labels.contains(&"StartingWith"), "{labels:?}");
        assert!(labels.contains(&"And"), "{labels:?}");
        assert!(
            entries
                .iter()
                .any(|entry| entry.filter_text.as_deref() == Some("findByLastNameStartingWith"))
        );
    }

    #[test]
    fn a_query_is_completed_with_its_entities_and_the_properties_of_an_alias() {
        let source = r#"package demo;

import java.util.List;
import org.springframework.data.jpa.repository.JpaRepository;
import org.springframework.data.jpa.repository.Query;

interface OwnerRepository extends JpaRepository<Owner, Integer> {
    @Query("select o from Own")
    List<Owner> all();

    @Query("select o from Owner o where o.ci")
    List<Owner> byCity(String city);
}
"#;
        let model = model_of(&[("Entities.java", ENTITIES)]);
        let files = parsed(&[("OwnerRepository.java", source)]);
        let file = &files[0].1;
        let entities = source.find("from Own\"").unwrap() + "from Own".len();
        let entries = completions_for(file, source, entities, &model);
        let labels: Vec<&str> = entries.iter().map(|entry| entry.label.as_str()).collect();
        assert_eq!(labels, ["Owner"], "the embeddable is no query root");

        let property = source.find("where o.ci").unwrap() + "where o.ci".len();
        let entries = completions_for(file, source, property, &model);
        let labels: Vec<&str> = entries.iter().map(|entry| entry.label.as_str()).collect();
        assert_eq!(labels, ["city"], "{labels:?}");
    }

    #[test]
    fn a_name_of_a_query_leads_to_its_field_and_an_entity_to_its_class() {
        let source = r#"package demo;

import java.util.List;
import org.springframework.data.jpa.repository.JpaRepository;
import org.springframework.data.jpa.repository.Query;

interface OwnerRepository extends JpaRepository<Owner, Integer> {
    @Query("select o from Owner o join o.pets p where p.name = :name")
    List<Owner> find(String name);
}
"#;
        let model = model_of(&[("Entities.java", ENTITIES)]);
        let files = parsed(&[("OwnerRepository.java", source)]);
        let file = &files[0].1;

        let property = source.find("p.name").unwrap() + "p.".len();
        let span = definitions_for(file, property, &model).pop().unwrap();
        assert!(span.uri.ends_with("Entities.java"), "{}", span.uri);
        assert_eq!(span.span.start.line, 40, "the name of the pet");

        let entity = source.find("from Owner").unwrap() + "from ".len();
        let span = definitions_for(file, entity, &model).pop().unwrap();
        assert!(span.uri.ends_with("Entities.java"));
        assert_eq!(span.span.start.line, 19, "the type of the owner");

        let method = source.find("find(String").unwrap() + "find".len();
        assert!(
            definitions_for(file, method, &model).is_empty(),
            "a method without a condition goes nowhere"
        );
    }

    #[test]
    fn a_repository_that_inherits_a_project_interface_takes_its_entity_through_it() {
        let base = r#"package demo;
import org.springframework.data.jpa.repository.JpaRepository;
interface BaseRepo<T, ID> extends JpaRepository<T, ID> {}
"#;
        let source = r#"package demo;

import java.util.List;

interface OwnerRepository extends BaseRepo<Owner, Integer> {
    List<Owner> findByNickname(String nickname);
    List<Owner> findByCity(String city);
}
"#;
        let model = model_of(&[
            ("Entities.java", ENTITIES),
            ("BaseRepo.java", base),
            ("OwnerRepository.java", source),
        ]);
        assert_eq!(
            repository_entity(
                &model,
                &parsed(&[("OwnerRepository.java", source)])[0].1.types[0]
            ),
            Some("Owner".to_owned()),
            "the entity comes through the parameters of the base interface"
        );
        let files = parsed(&[("OwnerRepository.java", source)]);
        let file = &files[0].1;
        let found = diagnostics_for(file, source, &model);
        let messages: Vec<&str> = found.iter().map(|entry| entry.message.as_str()).collect();
        assert_eq!(
            messages,
            ["Cannot resolve property 'Nickname'"],
            "{found:?}"
        );
        let caret = source.find("findByCi").unwrap() + "findByCi".len();
        let entries = completions_for(file, source, caret, &model);
        let labels: Vec<&str> = entries.iter().map(|entry| entry.label.as_str()).collect();
        assert!(labels.contains(&"City"), "{labels:?}");
    }

    #[test]
    fn a_text_block_of_a_query_is_read_with_the_places_of_its_lines() {
        let source = concat!(
            "package demo;\n",
            "\n",
            "import java.util.List;\n",
            "import org.springframework.data.jpa.repository.JpaRepository;\n",
            "import org.springframework.data.jpa.repository.Query;\n",
            "\n",
            "interface OwnerRepository extends JpaRepository<Owner, Integer> {\n",
            "    @Query(\"\"\"\n",
            "        select o from Owner o\n",
            "        where o.nickname = :name\n",
            "        \"\"\")\n",
            "    List<Owner> broken(String other);\n",
            "}\n",
        );
        let model = model_of(&[("Entities.java", ENTITIES)]);
        let files = parsed(&[("OwnerRepository.java", source)]);
        let file = &files[0].1;
        let found = diagnostics_for(file, source, &model);
        let messages: Vec<&str> = found.iter().map(|entry| entry.message.as_str()).collect();
        assert_eq!(
            messages,
            [
                "Cannot resolve property 'nickname'",
                "Cannot resolve parameter 'name'"
            ],
            "{found:?}"
        );
        let line = source
            .lines()
            .nth(found[0].span.start.line as usize)
            .unwrap();
        assert!(
            line[found[0].span.start.character as usize..].starts_with("nickname"),
            "the error underlines the property of the block"
        );
        let caret = source.find("from Own").unwrap() + "from Own".len();
        let entries = completions_for(file, source, caret, &model);
        let labels: Vec<&str> = entries.iter().map(|entry| entry.label.as_str()).collect();
        assert_eq!(labels, ["Owner"], "{labels:?}");
    }

    /// A completion of an ordinary source hands the query to the server at once: the
    /// text alone tells that it holds no place of the model.
    #[test]
    fn a_file_without_a_repository_is_handed_to_the_server_without_a_parse() {
        let dir = test_support::TempDir::new("spring-data-fast-path");
        let path = dir.join("src/main/java/demo/Plain.java");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let text = "package demo;\n\nclass Plain {\n    void run() { }\n}\n";
        std::fs::write(&path, text).unwrap();
        let index = spring_data_index(dir.to_path_buf());
        let uri = file_uri(&CanonPath::new(&path).unwrap()).unwrap();
        index.document_uri_changed(uri.as_str(), Some(Arc::from(text)));

        let fallback = Arc::new(CountingCompletion::default());
        let completion = NativeCompletion {
            data: index,
            fallback: Some(fallback.clone()),
        };
        reset_spring_parses();
        let spot = DocumentSpot {
            uri: uri.as_str().to_owned(),
            version: 1,
            point: TextPoint {
                line: 3,
                character: 6,
            },
        };
        let _ = lang_api::ask(
            &completion as &dyn Provider<DocumentSpot, CompletionList>,
            spot,
        );
        assert_eq!(
            fallback.calls.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "the server answers in the same call"
        );
        assert_eq!(spring_parses(), 0, "the text is not parsed");
    }

    #[derive(Default)]
    struct CountingCompletion {
        calls: std::sync::atomic::AtomicUsize,
    }

    impl Provider<DocumentSpot, CompletionList> for CountingCompletion {
        fn start(&self, _: DocumentSpot, _: Cancel, mut reply: Reply<CompletionList>) {
            self.calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let _ = reply.send(Answer::new(
                CompletionList {
                    entries: Vec::new(),
                    complete: true,
                },
                Confidence::Certain,
                SOURCE,
            ));
        }
    }

    #[test]
    fn a_place_of_a_repository_is_a_candidate_and_a_plain_literal_is_not() {
        let source = "interface OwnerRepository extends JpaRepository<Owner, Integer> {\n    List<Owner> findByLastNa(String name);\n}\n";
        assert!(data_candidate(
            source,
            source.find("findByLastNa").unwrap() + 8
        ));
        let plain = "class App {\n    void run() { String text = \"hello\"; }\n}\n";
        assert!(!data_candidate(plain, plain.find("hello").unwrap()));
        let query = "interface R {\n    @Query(\"select o from Own\")\n    List<O> find();\n}\n";
        assert!(data_candidate(query, query.find("from Own").unwrap() + 5));
        assert!(!data_candidate(query, query.find("List<O>").unwrap()));
        let block = concat!(
            "interface R {\n",
            "    @Query(\"\"\"\n",
            "        select o from Own\n",
            "        \"\"\")\n",
            "    List<O> find();\n",
            "}\n",
        );
        assert!(data_candidate(block, block.find("from Own").unwrap() + 5));
    }

    #[test]
    fn the_keywords_of_a_query_are_coloured() {
        let source = r#"package demo;

import java.util.List;
import org.springframework.data.jpa.repository.JpaRepository;
import org.springframework.data.jpa.repository.Query;

interface OwnerRepository extends JpaRepository<Owner, Integer> {
    @Query("select o from Owner o where o.city = :city")
    List<Owner> find(String city);
}
"#;
        let files = parsed(&[("OwnerRepository.java", source)]);
        let tokens = tokens_for(&files[0].1, source);
        let kinds: Vec<SemanticTokenKind> = tokens.iter().map(|token| token.kind).collect();
        assert_eq!(
            kinds,
            [
                SemanticTokenKind::Keyword,
                SemanticTokenKind::Keyword,
                SemanticTokenKind::Keyword,
                SemanticTokenKind::Parameter
            ]
        );
        assert!(tokens.iter().all(|token| token.span.start.line == 7));
    }
}
