//! The type graph of a diagram: the types around a place or of a folder with the
//! relationships the sources declare. The scan runs in the background and reads the
//! declarations with tree-sitter; no class path is needed, so a library type is kept by
//! its name without its members.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::thread;

use base::Cancel;
use lang_api::{
    Answer, Confidence, DocumentSpot, GraphEdge, GraphEdgeKind, GraphMember, GraphMemberKind,
    GraphType, GraphTypeKind, GraphVisibility, Provider, Reply, SourceId, SourceSpan, TextPoint,
    TextSpan, TypeGraph, TypeGraphQuery, offset_in,
};
use lsp_client::lsp_types::Uri;
use lsp_client::uri::file_path;
use platform::CanonPath;

use crate::JavaProject;

const SOURCE: SourceId = SourceId::new("type-graph");
/// The most source files the scan walks.
const MAX_FILES: usize = 20_000;
/// The type every chain of supertypes ends at; the diagram leaves it out.
const ROOT_TYPE: &str = "Object";

impl Provider<TypeGraphQuery, TypeGraph> for JavaProject {
    fn start(&self, query: TypeGraphQuery, cancel: Cancel, mut reply: Reply<TypeGraph>) {
        let root = self.root.clone();
        thread::spawn(move || {
            let graph = type_graph_of_cancellable(&root, query, &cancel);
            if cancel.is_cancelled() {
                return;
            }
            let _ = reply.send(Answer::new(graph, Confidence::Partial, SOURCE));
        });
    }
}

/// The graph a query asks for, read from the sources under `root`.
#[cfg(test)]
pub(crate) fn type_graph_of(root: &Path, query: TypeGraphQuery) -> TypeGraph {
    type_graph_of_cancellable(root, query, &Cancel::new())
}

fn type_graph_of_cancellable(root: &Path, query: TypeGraphQuery, cancel: &Cancel) -> TypeGraph {
    let index = Index::read(root, cancel);
    if cancel.is_cancelled() {
        return TypeGraph::default();
    }
    match query {
        TypeGraphQuery::At(spot) => index.graph_at(&spot),
        TypeGraphQuery::Folder(folder) => index.graph_of_folder(&folder),
        TypeGraphQuery::Type(name) => index.graph_of_type(&name),
    }
}

/// One type of the project with the imports of its file.
struct Entry {
    file: PathBuf,
    text: String,
    package: String,
    imports: HashMap<String, String>,
    wildcards: Vec<String>,
    declaration: java_syntax::DiagramType,
}

/// The types of the project, by qualified and by simple name.
struct Index {
    entries: Vec<Entry>,
    by_qualified: HashMap<String, usize>,
    by_simple: HashMap<String, Vec<usize>>,
}

/// Where a name of a source points.
enum Target {
    Known(usize),
    External(String),
}

impl Index {
    fn read(root: &Path, cancel: &Cancel) -> Self {
        let mut paths = Vec::new();
        collect_sources(root, &mut paths, cancel);
        paths.sort();
        paths.dedup();
        let mut entries = Vec::new();
        for path in paths {
            if cancel.is_cancelled() {
                break;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let file = java_syntax::diagram_file(&text);
            let imports = file.imports.iter().cloned().collect::<HashMap<_, _>>();
            let wildcards: Vec<String> = file
                .imports
                .iter()
                .filter(|(name, _)| name == "*")
                .map(|(_, path)| path.clone())
                .collect();
            for declaration in file.types {
                entries.push(Entry {
                    file: path.clone(),
                    text: text.clone(),
                    package: file.package.clone(),
                    imports: imports.clone(),
                    wildcards: wildcards.clone(),
                    declaration,
                });
            }
        }
        let mut index = Self {
            entries,
            by_qualified: HashMap::new(),
            by_simple: HashMap::new(),
        };
        for (at, entry) in index.entries.iter().enumerate() {
            index
                .by_qualified
                .entry(entry.declaration.qualified.clone())
                .or_insert(at);
            index
                .by_simple
                .entry(entry.declaration.name.clone())
                .or_default()
                .push(at);
        }
        index
    }

    fn resolve(&self, from: usize, name: &str) -> Target {
        let simple = name.rsplit('.').next().unwrap_or(name);
        if name.contains('.')
            && let Some(at) = self.by_qualified.get(name)
        {
            return Target::Known(*at);
        }
        let entry = &self.entries[from];
        let owner = entry
            .declaration
            .qualified
            .strip_suffix(&entry.declaration.name)
            .map(|prefix| prefix.trim_end_matches('.'))
            .filter(|prefix| !prefix.is_empty());
        let mut candidates = Vec::new();
        if let Some(owner) = owner {
            candidates.push(format!("{owner}.{name}"));
        }
        candidates.push(format!("{}.{}", entry.package, name));
        if let Some(path) = entry.imports.get(simple)
            && simple == name
        {
            candidates.push(format!("{path}.{simple}"));
        }
        for package in &entry.wildcards {
            candidates.push(format!("{package}.{name}"));
        }
        for candidate in candidates {
            if let Some(at) = self.by_qualified.get(&candidate) {
                return Target::Known(*at);
            }
        }
        if !name.contains('.')
            && let Some(found) = self.by_simple.get(name)
            && found.len() == 1
        {
            return Target::Known(found[0]);
        }
        Target::External(simple.to_owned())
    }

    fn graph_at(&self, spot: &DocumentSpot) -> TypeGraph {
        let Some(path) = Uri::from_str(&spot.uri).ok().as_ref().and_then(file_path) else {
            return TypeGraph::default();
        };
        let Some(entry) = self.entry_at(&path, spot) else {
            return TypeGraph::default();
        };
        self.class_graph(entry)
    }

    fn entry_at(&self, path: &Path, spot: &DocumentSpot) -> Option<usize> {
        let key = platform::path_key(path);
        let mut best: Option<(usize, usize)> = None;
        for (at, entry) in self.entries.iter().enumerate() {
            if platform::path_key(&entry.file) != key {
                continue;
            }
            let offset = offset_in(&entry.text, spot.point);
            if entry.declaration.start <= offset && offset < entry.declaration.end {
                let size = entry.declaration.end - entry.declaration.start;
                if best.is_none_or(|(_, found)| size < found) {
                    best = Some((at, size));
                }
            }
        }
        best.map(|(at, _)| at)
    }

    fn graph_of_type(&self, name: &str) -> TypeGraph {
        let found = self
            .by_qualified
            .get(name)
            .copied()
            .or_else(|| self.resolve_first(name));
        match found {
            Some(at) => self.class_graph(at),
            None => TypeGraph::default(),
        }
    }

    fn resolve_first(&self, name: &str) -> Option<usize> {
        let simple = name.rsplit('.').next().unwrap_or(name);
        let found = self.by_simple.get(simple)?;
        found
            .iter()
            .copied()
            .find(|at| self.entries[*at].declaration.qualified == name)
            .or_else(|| (found.len() == 1).then_some(found[0]))
    }

    /// One type with its ancestors and its direct subtypes of the project.
    fn class_graph(&self, start: usize) -> TypeGraph {
        let mut graph = Builder::default();
        graph.add_known(self, start, false);
        self.walk_ancestors(start, &mut graph);
        for (at, entry) in self.entries.iter().enumerate() {
            if at == start {
                continue;
            }
            for name in entry
                .declaration
                .extends
                .iter()
                .chain(entry.declaration.implements.iter())
            {
                if matches!(self.resolve(at, name), Target::Known(target) if target == start) {
                    graph.add_known(self, at, false);
                    graph.push_edge(self, at, start, self.super_kind(at, name, start), None);
                }
            }
        }
        self.add_associations(&mut graph);
        graph.finish(self)
    }

    /// Every type of a folder with the relationships between them.
    fn graph_of_folder(&self, folder: &Path) -> TypeGraph {
        let key = platform::path_key(folder);
        let mut graph = Builder::default();
        let mut inside = HashSet::new();
        for (at, entry) in self.entries.iter().enumerate() {
            let parent = entry.file.parent().map(platform::path_key);
            if parent.as_ref() != Some(&key) {
                continue;
            }
            inside.insert(at);
            graph.add_known(self, at, false);
        }
        for at in &inside {
            self.walk_folder_ancestors(*at, &inside, &mut graph, &mut HashSet::new());
        }
        self.add_associations(&mut graph);
        graph.finish(self)
    }

    /// The ancestors of one type of the folder. A type of the project outside the folder
    /// stays a name without its members, as a library type does, and its own ancestors
    /// are followed so that the chain above the folder stays whole.
    fn walk_folder_ancestors(
        &self,
        at: usize,
        inside: &HashSet<usize>,
        graph: &mut Builder,
        seen: &mut HashSet<usize>,
    ) {
        if !seen.insert(at) {
            return;
        }
        let entry = &self.entries[at];
        for name in entry
            .declaration
            .extends
            .iter()
            .chain(entry.declaration.implements.iter())
        {
            match self.resolve(at, name) {
                Target::Known(target) if target == at => {}
                Target::Known(target) => {
                    if self.entries[target].declaration.qualified == ROOT_TYPE {
                        continue;
                    }
                    let external = !inside.contains(&target);
                    graph.add_known(self, target, external);
                    graph.push_edge(self, at, target, self.super_kind(at, name, target), None);
                    self.walk_folder_ancestors(target, inside, graph, seen);
                }
                Target::External(name) => {
                    if name == ROOT_TYPE {
                        continue;
                    }
                    graph.add_external(&name);
                    graph.push_external_edge(
                        &entry.declaration.qualified,
                        &name,
                        self.super_kind_external(at, name.as_str()),
                        None,
                    );
                }
            }
        }
    }

    /// Every ancestor of a type, down to the root type, and the edges between them.
    fn walk_ancestors(&self, start: usize, graph: &mut Builder) {
        let mut seen = HashSet::new();
        let mut queue = vec![start];
        while let Some(at) = queue.pop() {
            if !seen.insert(at) {
                continue;
            }
            let entry = &self.entries[at];
            for name in entry
                .declaration
                .extends
                .iter()
                .chain(entry.declaration.implements.iter())
            {
                match self.resolve(at, name) {
                    Target::Known(target) => {
                        let qualified = &self.entries[target].declaration.qualified;
                        if qualified == ROOT_TYPE {
                            continue;
                        }
                        graph.add_known(self, target, false);
                        graph.push_edge(self, at, target, self.super_kind(at, name, target), None);
                        queue.push(target);
                    }
                    Target::External(name) => {
                        if name == ROOT_TYPE {
                            continue;
                        }
                        graph.add_external(&name);
                        graph.push_external_edge(
                            &entry.declaration.qualified,
                            &name,
                            self.super_kind_external(at, name.as_str()),
                            None,
                        );
                    }
                }
            }
        }
    }

    /// The fields of the types already in the graph that hold a type of the graph.
    fn add_associations(&self, graph: &mut Builder) {
        let included: Vec<usize> = graph
            .order
            .iter()
            .filter_map(|(_, at, external)| if *external { None } else { *at })
            .collect();
        for at in included {
            let from = self.entries[at].declaration.qualified.clone();
            for field in &self.entries[at].declaration.fields {
                for reference in &field.refs {
                    let Some(target) = graph.index_of_resolved(self, at, &reference.name) else {
                        continue;
                    };
                    if target == from {
                        continue;
                    }
                    let multiplicity = reference.collection.then(|| "*".to_owned());
                    graph.push_named_edge(
                        from.clone(),
                        target,
                        GraphEdgeKind::Association,
                        multiplicity,
                    );
                }
            }
        }
    }

    /// Whether a supertype declaration draws an implementation edge.
    fn super_kind(&self, from: usize, name: &str, target: usize) -> GraphEdgeKind {
        let source = &self.entries[from].declaration;
        let from_implements = source.implements.iter().any(|extends| extends == name);
        if from_implements && source.kind != java_syntax::DiagramTypeKind::Interface {
            return GraphEdgeKind::Implementation;
        }
        if source.kind == java_syntax::DiagramTypeKind::Interface
            && self.entries[target].declaration.kind == java_syntax::DiagramTypeKind::Interface
        {
            return GraphEdgeKind::Inheritance;
        }
        if from_implements
            && self.entries[target].declaration.kind == java_syntax::DiagramTypeKind::Interface
        {
            return GraphEdgeKind::Implementation;
        }
        GraphEdgeKind::Inheritance
    }

    fn super_kind_external(&self, from: usize, name: &str) -> GraphEdgeKind {
        let source = &self.entries[from].declaration;
        if source.kind != java_syntax::DiagramTypeKind::Interface
            && source
                .implements
                .iter()
                .any(|implements| implements == name)
        {
            GraphEdgeKind::Implementation
        } else {
            GraphEdgeKind::Inheritance
        }
    }
}

#[derive(Default)]
struct Builder {
    /// The qualified name of every node, the entry it stands for and whether it stays a
    /// name without its members.
    order: Vec<(String, Option<usize>, bool)>,
    positions: HashMap<String, usize>,
    edges: Vec<GraphEdge>,
    seen_edges: HashSet<(String, String, GraphEdgeKind)>,
}

impl Builder {
    fn add_known(&mut self, index: &Index, at: usize, external: bool) {
        let entry = &index.entries[at];
        self.add_node(&entry.declaration.qualified, Some(at), external);
    }

    fn add_external(&mut self, name: &str) {
        if self.positions.contains_key(name) {
            return;
        }
        self.add_node(name, None, true);
    }

    fn add_node(&mut self, qualified: &str, at: Option<usize>, external: bool) {
        if self.positions.contains_key(qualified) {
            return;
        }
        self.positions
            .insert(qualified.to_owned(), self.order.len());
        self.order.push((qualified.to_owned(), at, external));
    }

    fn index_of(&self, qualified: &str) -> Option<usize> {
        self.positions.get(qualified).copied()
    }

    fn push_edge(
        &mut self,
        index: &Index,
        from: usize,
        to: usize,
        kind: GraphEdgeKind,
        multiplicity: Option<String>,
    ) {
        let from = index.entries[from].declaration.qualified.clone();
        let to = index.entries[to].declaration.qualified.clone();
        self.push_named_edge(from, to, kind, multiplicity);
    }

    fn push_external_edge(
        &mut self,
        from: &str,
        to: &str,
        kind: GraphEdgeKind,
        multiplicity: Option<String>,
    ) {
        self.push_named_edge(from.to_owned(), to.to_owned(), kind, multiplicity);
    }

    fn push_named_edge(
        &mut self,
        from: String,
        to: String,
        kind: GraphEdgeKind,
        multiplicity: Option<String>,
    ) {
        if from == to {
            return;
        }
        let key = (from.clone(), to.clone(), kind);
        if !self.seen_edges.insert(key) {
            return;
        }
        self.edges.push(GraphEdge {
            from,
            to,
            kind,
            multiplicity,
        });
    }

    fn index_of_resolved(&self, index: &Index, from: usize, name: &str) -> Option<String> {
        match index.resolve(from, name) {
            Target::Known(at) => {
                let qualified = &index.entries[at].declaration.qualified;
                self.index_of(qualified).map(|_| qualified.clone())
            }
            Target::External(name) => self.index_of(&name).map(|_| name),
        }
    }

    fn finish(self, index: &Index) -> TypeGraph {
        let nodes = self
            .order
            .iter()
            .map(|(qualified, at, external)| match at {
                Some(at) if !*external => known_node(index, *at),
                _ => external_node(qualified),
            })
            .collect();
        TypeGraph {
            nodes,
            edges: self.edges,
        }
    }
}

/// A type of the project with everything the diagram shows about it.
fn known_node(index: &Index, at: usize) -> GraphType {
    let entry = &index.entries[at];
    let declaration = &entry.declaration;
    let mut members = Vec::new();
    for field in &declaration.fields {
        members.push(GraphMember {
            name: field.name.clone(),
            type_name: field.type_text.clone(),
            parameters: Vec::new(),
            kind: GraphMemberKind::Field,
            visibility: visibility_of(field.visibility),
            static_member: field.static_member,
            abstract_member: false,
            place: Some(place_in(
                &entry.file,
                &entry.text,
                field.offset,
                field.offset + field.name.len(),
            )),
        });
    }
    for method in &declaration.methods {
        members.push(GraphMember {
            name: method.name.clone(),
            type_name: method.return_type.clone(),
            parameters: method.parameters.clone(),
            kind: GraphMemberKind::Method,
            visibility: visibility_of(method.visibility),
            static_member: method.static_member,
            abstract_member: method.abstract_member,
            place: Some(place_in(
                &entry.file,
                &entry.text,
                method.offset,
                method.offset + method.name.len(),
            )),
        });
    }
    GraphType {
        name: declaration.name.clone(),
        qualified: declaration.qualified.clone(),
        kind: kind_of(declaration.kind),
        abstract_type: declaration.abstract_type,
        external: false,
        members,
        place: Some(place_in(
            &entry.file,
            &entry.text,
            declaration.name_offset,
            declaration.name_end,
        )),
    }
}

fn external_node(qualified: &str) -> GraphType {
    GraphType {
        name: qualified.rsplit('.').next().unwrap_or(qualified).to_owned(),
        qualified: qualified.to_owned(),
        kind: GraphTypeKind::Class,
        abstract_type: false,
        external: true,
        members: Vec::new(),
        place: None,
    }
}

fn kind_of(kind: java_syntax::DiagramTypeKind) -> GraphTypeKind {
    match kind {
        java_syntax::DiagramTypeKind::Class => GraphTypeKind::Class,
        java_syntax::DiagramTypeKind::Interface => GraphTypeKind::Interface,
        java_syntax::DiagramTypeKind::Enum => GraphTypeKind::Enum,
        java_syntax::DiagramTypeKind::Record => GraphTypeKind::Record,
        java_syntax::DiagramTypeKind::Annotation => GraphTypeKind::Annotation,
    }
}

fn visibility_of(visibility: java_syntax::DiagramVisibility) -> GraphVisibility {
    match visibility {
        java_syntax::DiagramVisibility::Public => GraphVisibility::Public,
        java_syntax::DiagramVisibility::Protected => GraphVisibility::Protected,
        java_syntax::DiagramVisibility::Package => GraphVisibility::Package,
        java_syntax::DiagramVisibility::Private => GraphVisibility::Private,
    }
}

fn place_in(file: &Path, text: &str, start: usize, end: usize) -> SourceSpan {
    let uri = CanonPath::new(file)
        .map(|path| path.to_uri())
        .unwrap_or_default();
    let start = point_of(text, start);
    let end = point_of(text, end);
    SourceSpan {
        uri,
        span: TextSpan { start, end },
    }
}

/// The line and UTF-16 column of a byte offset.
fn point_of(text: &str, offset: usize) -> TextPoint {
    let offset = offset.min(text.len());
    let before = &text[..offset];
    let line = before.bytes().filter(|byte| *byte == b'\n').count() as u32;
    let line_start = before.rfind('\n').map_or(0, |index| index + 1);
    let character = text[line_start..offset].encode_utf16().count() as u32;
    TextPoint { line, character }
}

fn collect_sources(folder: &Path, out: &mut Vec<PathBuf>, cancel: &Cancel) {
    if out.len() >= MAX_FILES || cancel.is_cancelled() {
        return;
    }
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        if out.len() >= MAX_FILES || cancel.is_cancelled() {
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
            collect_sources(&path, out, cancel);
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

#[cfg(test)]
mod tests {
    use std::fs;

    use test_support::TempDir;

    use super::*;

    fn write(root: &Path, name: &str, text: &str) {
        let path = root.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, text).unwrap();
    }

    fn nodes(graph: &TypeGraph) -> Vec<(String, bool)> {
        graph
            .nodes
            .iter()
            .map(|node| (node.qualified.clone(), node.external))
            .collect()
    }

    fn edges(graph: &TypeGraph) -> Vec<(String, String, GraphEdgeKind, Option<String>)> {
        graph
            .edges
            .iter()
            .map(|edge| {
                (
                    edge.from.clone(),
                    edge.to.clone(),
                    edge.kind,
                    edge.multiplicity.clone(),
                )
            })
            .collect()
    }

    fn project(name: &str) -> TempDir {
        let root = TempDir::new(name);
        write(
            &root,
            "src/demo/model/BaseEntity.java",
            "package demo.model;\npublic abstract class BaseEntity {}\n",
        );
        write(
            &root,
            "src/demo/owner/Person.java",
            "package demo.owner;\npublic class Person extends BaseEntity {\n    private String name;\n}\n",
        );
        write(
            &root,
            "src/demo/owner/Owner.java",
            r#"package demo.owner;

import java.util.List;

public class Owner extends Person implements Named, Serializable {
    private List<Owner> friends;
    private Address address;
    public String getName() { return null; }

    static class Nested {}
}

interface Named {}

class Address {
    String city;
    List<Owner> residents;
}

enum Kind { HOME, WORK }

record Pair(String left, int right) {}
"#,
        );
        root
    }

    #[test]
    fn a_class_has_its_ancestors_and_its_direct_subtypes() {
        let root = project("graph-class");
        let file = root.join("src/demo/owner/Owner.java");
        let text = fs::read_to_string(&file).unwrap();
        let spot = DocumentSpot {
            uri: CanonPath::new(&file).unwrap().to_uri(),
            version: 1,
            point: point_of(&text, text.find("class Owner").unwrap() + 6),
        };
        let graph = type_graph_of(root.path(), TypeGraphQuery::At(spot));
        let names = nodes(&graph);
        assert!(
            names.contains(&("demo.owner.Owner".to_owned(), false)),
            "{names:?}"
        );
        assert!(
            names.contains(&("demo.owner.Person".to_owned(), false)),
            "{names:?}"
        );
        assert!(
            names.contains(&("demo.model.BaseEntity".to_owned(), false)),
            "{names:?}"
        );
        assert!(
            names.contains(&("Serializable".to_owned(), true)),
            "a library interface stays a name without its members: {names:?}"
        );
        let child = names
            .iter()
            .find(|(name, _)| name == "demo.owner.Address")
            .map(|(_, external)| *external);
        assert_eq!(child, None, "Address is not a subtype of Owner");
        let edges = edges(&graph);
        assert!(
            edges.contains(&(
                "demo.owner.Owner".to_owned(),
                "demo.owner.Person".to_owned(),
                GraphEdgeKind::Inheritance,
                None
            )),
            "{edges:?}"
        );
        assert!(
            edges.contains(&(
                "demo.owner.Owner".to_owned(),
                "Serializable".to_owned(),
                GraphEdgeKind::Implementation,
                None
            )),
            "{edges:?}"
        );
        assert!(
            edges.contains(&(
                "demo.owner.Person".to_owned(),
                "demo.model.BaseEntity".to_owned(),
                GraphEdgeKind::Inheritance,
                None
            )),
            "{edges:?}"
        );
    }

    #[test]
    fn a_direct_subtype_of_the_project_is_in_the_graph() {
        let root = project("graph-subtype");
        let text = fs::read_to_string(root.join("src/demo/owner/Person.java")).unwrap();
        let spot = DocumentSpot {
            uri: CanonPath::new(root.join("src/demo/owner/Person.java"))
                .unwrap()
                .to_uri(),
            version: 1,
            point: point_of(&text, text.find("class Person").unwrap() + 6),
        };
        let graph = type_graph_of(root.path(), TypeGraphQuery::At(spot));
        let names = nodes(&graph);
        assert!(names.contains(&("demo.owner.Person".to_owned(), false)));
        assert!(names.contains(&("demo.owner.Owner".to_owned(), false)));
        assert!(
            names.contains(&("demo.model.BaseEntity".to_owned(), false)),
            "{names:?}"
        );
    }

    #[test]
    fn a_field_of_a_collection_draws_an_association_with_the_multiplicity() {
        let root = project("graph-association");
        let folder = type_graph_of(
            root.path(),
            TypeGraphQuery::Folder(root.join("src/demo/owner")),
        );
        let collection = edges(&folder).into_iter().find(|(from, to, kind, _)| {
            from == "demo.owner.Address"
                && to == "demo.owner.Owner"
                && *kind == GraphEdgeKind::Association
        });
        assert_eq!(
            collection,
            Some((
                "demo.owner.Address".to_owned(),
                "demo.owner.Owner".to_owned(),
                GraphEdgeKind::Association,
                Some("*".to_owned()),
            )),
            "a collection of a type of the diagram draws an association with `*`"
        );
        let simple = edges(&folder).into_iter().find(|(from, to, kind, _)| {
            from == "demo.owner.Owner"
                && to == "demo.owner.Address"
                && *kind == GraphEdgeKind::Association
        });
        assert_eq!(
            simple.map(|edge| edge.3),
            Some(None),
            "a plain field draws an association without a multiplicity"
        );
        // A class diagram only draws the relationships between its own nodes.
        let text = fs::read_to_string(root.join("src/demo/owner/Owner.java")).unwrap();
        let spot = DocumentSpot {
            uri: CanonPath::new(root.join("src/demo/owner/Owner.java"))
                .unwrap()
                .to_uri(),
            version: 1,
            point: point_of(&text, text.find("class Owner").unwrap() + 6),
        };
        let own = type_graph_of(root.path(), TypeGraphQuery::At(spot));
        assert!(
            !edges(&own).iter().any(|(from, to, kind, _)| {
                from == "demo.owner.Owner"
                    && to == "demo.owner.Address"
                    && *kind == GraphEdgeKind::Association
            }),
            "Address is not in this graph"
        );
    }

    #[test]
    fn a_package_graph_has_its_types_and_the_chain_above_them() {
        let root = project("graph-folder");
        let graph = type_graph_of(
            root.path(),
            TypeGraphQuery::Folder(root.join("src/demo/owner")),
        );
        let names = nodes(&graph);
        for wanted in [
            "demo.owner.Owner",
            "demo.owner.Person",
            "demo.owner.Address",
            "demo.owner.Kind",
            "demo.owner.Pair",
            "demo.owner.Named",
            "demo.owner.Owner.Nested",
        ] {
            assert!(
                names.iter().any(|(name, _)| name == wanted),
                "{wanted} is missing: {names:?}"
            );
        }
        assert!(
            names.contains(&("demo.model.BaseEntity".to_owned(), true)),
            "an ancestor outside the folder stays a name: {names:?}"
        );
        assert!(
            edges(&graph).iter().any(|(from, to, kind, _)| {
                from == "demo.owner.Owner"
                    && to == "demo.owner.Named"
                    && *kind == GraphEdgeKind::Implementation
            }),
            "Owner implements Named: {:?}",
            edges(&graph)
        );
    }

    #[test]
    fn a_type_query_finds_a_class_by_its_qualified_name() {
        let root = project("graph-type");
        let graph = type_graph_of(
            root.path(),
            TypeGraphQuery::Type("demo.owner.Person".to_owned()),
        );
        let names = nodes(&graph);
        assert!(names.contains(&("demo.owner.Person".to_owned(), false)));
        assert!(names.contains(&("demo.owner.Owner".to_owned(), false)));
    }

    #[test]
    fn members_keep_their_kind_signature_and_visibility() {
        let root = project("graph-members");
        let graph = type_graph_of(
            root.path(),
            TypeGraphQuery::Type("demo.owner.Owner".to_owned()),
        );
        let owner = graph
            .nodes
            .iter()
            .find(|node| node.qualified == "demo.owner.Owner")
            .expect("the class is in the graph");
        let friends = owner
            .members
            .iter()
            .find(|member| member.name == "friends")
            .expect("the field is in the graph");
        assert_eq!(friends.kind, GraphMemberKind::Field);
        assert_eq!(friends.visibility, GraphVisibility::Private);
        let get = owner
            .members
            .iter()
            .find(|member| member.name == "getName")
            .expect("the method is in the graph");
        assert_eq!(get.kind, GraphMemberKind::Method);
        assert_eq!(get.type_name, "String");
        assert_eq!(get.visibility, GraphVisibility::Public);
        assert!(get.place.is_some());
    }

    #[test]
    #[ignore = "needs IDE_CORPUS with src/petclinic"]
    fn petclinic_owner_package_has_its_hierarchy() {
        let corpus = std::env::var_os("IDE_CORPUS")
            .map(PathBuf::from)
            .unwrap_or_else(|| panic!("set IDE_CORPUS to the folder that holds src/petclinic"));
        let root = corpus.join("src/petclinic");
        let folder = root.join("src/main/java/org/springframework/samples/petclinic/owner");
        let graph = type_graph_of(&root, TypeGraphQuery::Folder(folder));
        let names: Vec<&str> = graph
            .nodes
            .iter()
            .map(|node| node.qualified.as_str())
            .collect();
        println!(
            "petclinic owner: {} nodes, {} edges",
            graph.nodes.len(),
            graph.edges.len()
        );
        for name in &names {
            println!("  {name}");
        }
        let base = "org.springframework.samples.petclinic.model.BaseEntity";
        let person = "org.springframework.samples.petclinic.model.Person";
        let owner = "org.springframework.samples.petclinic.owner.Owner";
        assert!(names.contains(&person), "Person is above Owner");
        assert!(names.contains(&owner), "Owner is in the package");
        assert!(
            names.contains(&base),
            "the ancestor outside the package is shown"
        );
        let edges = edges(&graph);
        assert!(
            edges.iter().any(|(from, to, kind, _)| {
                from == owner && to == person && *kind == GraphEdgeKind::Inheritance
            }),
            "Owner extends Person"
        );
        assert!(
            edges.iter().any(|(from, to, kind, _)| {
                from == person && to == base && *kind == GraphEdgeKind::Inheritance
            }),
            "Person extends BaseEntity"
        );
    }
}
