//! The sequence diagram of a method: the messages its body makes in order, the
//! participants they reach and the fragments of `if`, `switch`, loops and `forEach`
//! lambdas around them. The scan runs in the background and reads the project sources
//! with tree-sitter; calls of types the project does not declare become one message to
//! an external participant without a body to walk.

use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::thread;

use base::Cancel;
use lang_api::{
    Answer, Confidence, DocumentSpot, Provider, Reply, SequenceDiagram, SequenceFragment,
    SequenceFragmentKind, SequenceMessage, SequenceMessageKind, SequenceParticipant,
    SequenceParticipantKind, SequenceQuery, SequenceSection, SequenceSettings, SourceId,
    SourceSpan, offset_in,
};
use lsp_client::lsp_types::Uri;
use lsp_client::uri::file_path;
use tree_sitter::{Node, Parser, Tree};

use crate::JavaProject;

const SOURCE: SourceId = SourceId::new("sequence-diagram");
/// The most source files the scan walks.
const MAX_FILES: usize = 20_000;
/// How many messages are added between two partial answers.
const PROGRESS_STEP: usize = 16;
/// The longest argument text a message keeps.
const ARGUMENT_LIMIT: usize = 24;

impl Provider<SequenceQuery, SequenceDiagram> for JavaProject {
    fn start(&self, query: SequenceQuery, cancel: Cancel, mut reply: Reply<SequenceDiagram>) {
        let root = self.root.clone();
        thread::spawn(move || {
            let diagram = build(&root, &query, &cancel, &mut |snapshot| {
                let _ = reply.send(Answer::new(snapshot.clone(), Confidence::Partial, SOURCE));
            });
            if cancel.is_cancelled() {
                return;
            }
            let confidence = if diagram.title.is_empty() {
                Confidence::Unknown
            } else {
                Confidence::Certain
            };
            let _ = reply.send(Answer::new(diagram, confidence, SOURCE));
        });
    }
}

/// The diagram a query asks for, read from the sources under `root`.
#[cfg(test)]
pub(crate) fn sequence_of(
    root: &Path,
    query: &SequenceQuery,
    cancel: &Cancel,
    progress: &mut dyn FnMut(&SequenceDiagram),
) -> SequenceDiagram {
    build(root, query, cancel, progress)
}

fn build(
    root: &Path,
    query: &SequenceQuery,
    cancel: &Cancel,
    progress: &mut dyn FnMut(&SequenceDiagram),
) -> SequenceDiagram {
    let index = Index::read(root, cancel);
    if cancel.is_cancelled() {
        return SequenceDiagram::default();
    }
    let Some(spot) = spot_path(&query.spot) else {
        return SequenceDiagram::default();
    };
    let Some(entry) = index.entry_at(&spot, &query.spot) else {
        return SequenceDiagram::default();
    };
    let mut builder = Builder {
        index: &index,
        settings: query.settings,
        cancel,
        progress,
        participants: Vec::new(),
        positions: HashMap::new(),
        messages: Vec::new(),
        fragments: Vec::new(),
        stack: Vec::new(),
        next_progress: PROGRESS_STEP,
        title: String::new(),
    };
    builder.run(entry)
}

/// One source file of the project with its text.
struct SourceFile {
    path: PathBuf,
    text: String,
}

/// How visible a method is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Visibility {
    Public,
    Protected,
    Package,
    Private,
}

/// One method or constructor of a type.
struct MethodInfo {
    name: String,
    return_type: String,
    parameter_names: Vec<String>,
    parameter_types: Vec<String>,
    visibility: Visibility,
    /// The method is a constructor, which the source does not give a return type.
    constructor: bool,
    /// The byte range of the name.
    name_offset: usize,
    /// The byte range of the whole declaration.
    declaration: Range<usize>,
    /// The byte range of the body; a method without one has none.
    body: Option<Range<usize>>,
}

/// One type declaration of the project with the members the scan needs.
struct TypeInfo {
    file: usize,
    name: String,
    qualified: String,
    kind: SequenceParticipantKind,
    name_offset: usize,
    package: String,
    imports: HashMap<String, String>,
    wildcards: Vec<String>,
    fields: HashMap<String, String>,
    methods: Vec<MethodInfo>,
}

/// The types of the project, by qualified and by simple name.
#[derive(Default)]
struct Index {
    files: Vec<SourceFile>,
    types: Vec<TypeInfo>,
    by_qualified: HashMap<String, usize>,
    by_simple: HashMap<String, Vec<usize>>,
    by_path: HashMap<PathBuf, Vec<usize>>,
}

/// A method found in the index.
#[derive(Clone, Copy)]
struct MethodRef {
    owner: usize,
    method: usize,
    file: usize,
}

/// Where a call lands.
enum TargetType {
    /// A type of the project.
    Known(usize),
    /// A type the project does not declare.
    External(String),
}

/// The participant of a call and the project method to walk, when there is one.
struct CallTarget {
    target: TargetType,
    method: Option<usize>,
}

impl Index {
    fn read(root: &Path, cancel: &Cancel) -> Self {
        let mut paths = Vec::new();
        crate::diagram::collect_sources(root, &mut paths, cancel);
        paths.sort();
        paths.dedup();
        let mut index = Self::default();
        for path in paths {
            if cancel.is_cancelled() || index.files.len() >= MAX_FILES {
                break;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let parsed = parse_file(&text);
            let file = index.files.len();
            index.files.push(SourceFile {
                path: path.clone(),
                text,
            });
            let mut types = Vec::new();
            for mut ty in parsed {
                ty.file = file;
                let at = index.types.len();
                index.by_qualified.entry(ty.qualified.clone()).or_insert(at);
                index.by_simple.entry(ty.name.clone()).or_default().push(at);
                types.push(at);
                index.types.push(ty);
            }
            if !types.is_empty() {
                index
                    .by_path
                    .entry(platform::path_key(&path))
                    .or_default()
                    .extend(types);
            }
        }
        index
    }

    fn file_index(&self, path: &Path) -> Option<usize> {
        let key = platform::path_key(path);
        self.by_path
            .get(&key)
            .and_then(|types| types.first())
            .map(|at| self.types[*at].file)
    }

    /// The method whose body holds `spot`, the innermost one for a nested declaration.
    fn entry_at(&self, path: &Path, spot: &DocumentSpot) -> Option<MethodRef> {
        let file = self.file_index(path)?;
        let text = &self.files[file].text;
        let offset = offset_in(text, spot.point);
        let mut best: Option<(usize, usize, usize)> = None;
        for owner in self.by_path.get(&platform::path_key(path))? {
            for (index, method) in self.types[*owner].methods.iter().enumerate() {
                if !method.declaration.contains(&offset) {
                    continue;
                }
                let size = method.declaration.end - method.declaration.start;
                if best.is_none_or(|(_, _, found)| size < found) {
                    best = Some((*owner, index, size));
                }
            }
        }
        best.map(|(owner, method, _)| MethodRef {
            owner,
            method,
            file,
        })
    }

    /// The type a name at `from` points at: a nested type, a type of the package, an
    /// imported type, a wildcard import or the only type of that simple name.
    fn type_named(&self, from: usize, name: &str) -> Option<usize> {
        let name = clean_type(name);
        if name.is_empty() {
            return None;
        }
        let simple = name.rsplit('.').next().unwrap_or(&name);
        if name.contains('.')
            && let Some(at) = self.by_qualified.get(&name)
        {
            return Some(*at);
        }
        let entry = &self.types[from];
        let owner = entry
            .qualified
            .strip_suffix(&entry.name)
            .map(|prefix| prefix.trim_end_matches('.'))
            .filter(|prefix| !prefix.is_empty());
        let mut candidates = Vec::new();
        if let Some(owner) = owner {
            candidates.push(format!("{owner}.{name}"));
        }
        if !entry.package.is_empty() {
            candidates.push(format!("{}.{}", entry.package, name));
        }
        if let Some(path) = entry.imports.get(simple) {
            candidates.push(format!("{path}.{simple}"));
        }
        for package in &entry.wildcards {
            candidates.push(format!("{package}.{name}"));
        }
        for candidate in candidates {
            if let Some(at) = self.by_qualified.get(&candidate) {
                return Some(*at);
            }
        }
        if !name.contains('.')
            && let Some(found) = self.by_simple.get(&name)
            && found.len() == 1
        {
            return Some(found[0]);
        }
        None
    }

    fn field_type(&self, owner: usize, name: &str) -> Option<String> {
        self.types[owner].fields.get(name).cloned()
    }

    fn method_of(&self, owner: usize, name: &str, arity: usize) -> Option<usize> {
        let methods = &self.types[owner].methods;
        methods
            .iter()
            .position(|method| {
                !method.constructor && method.name == name && method.parameter_types.len() == arity
            })
            .or_else(|| {
                methods
                    .iter()
                    .position(|method| method.name == name && method.parameter_types.len() == arity)
            })
    }

    fn constructor_of(&self, owner: usize, arity: usize) -> Option<usize> {
        self.types[owner]
            .methods
            .iter()
            .position(|method| method.constructor && method.parameter_types.len() == arity)
    }

    /// The type of an expression, as the source spells it, when the scan can tell it.
    fn type_of_expr(
        &self,
        owner: usize,
        locals: &HashMap<String, String>,
        node: Node<'_>,
        text: &str,
        depth: usize,
    ) -> Option<String> {
        if depth > 8 {
            return None;
        }
        match node.kind() {
            "identifier" => {
                let name = text_of(text, node);
                if let Some(found) = locals.get(name) {
                    return Some(found.clone());
                }
                if let Some(found) = self.field_type(owner, name) {
                    return Some(found);
                }
                self.type_named(owner, name).map(|_| name.to_owned())
            }
            "this" => Some(self.types[owner].qualified.clone()),
            "parenthesized_expression" => node
                .named_child(0)
                .and_then(|inner| self.type_of_expr(owner, locals, inner, text, depth + 1)),
            "object_creation_expression" => node
                .child_by_field_name("type")
                .map(|kind| clean_type(text_of(text, kind))),
            "cast_expression" => node
                .child_by_field_name("type")
                .map(|kind| clean_type(text_of(text, kind))),
            "field_access" => {
                let field = node.child_by_field_name("field")?;
                let name = text_of(text, field);
                let object = node.child_by_field_name("object")?;
                let type_name = self.type_of_expr(owner, locals, object, text, depth + 1)?;
                let at = self.type_named(owner, &type_name).unwrap_or(owner);
                self.field_type(at, name)
            }
            "method_invocation" => {
                let name = node.child_by_field_name("name")?;
                let name = text_of(text, name);
                let arity = node
                    .child_by_field_name("arguments")
                    .map(|arguments| arguments.named_child_count())
                    .unwrap_or(0);
                let at = match node.child_by_field_name("object") {
                    Some(object) => {
                        let type_name =
                            self.type_of_expr(owner, locals, object, text, depth + 1)?;
                        self.type_named(owner, &type_name)?
                    }
                    None => owner,
                };
                let method = self.method_of(at, name, arity)?;
                let returns = clean_type(&self.types[at].methods[method].return_type);
                (!returns.is_empty()).then_some(returns)
            }
            _ => None,
        }
    }
}

fn spot_path(spot: &DocumentSpot) -> Option<PathBuf> {
    Uri::from_str(&spot.uri).ok().as_ref().and_then(file_path)
}

/// The types of one source file.
fn parse_file(text: &str) -> Vec<TypeInfo> {
    let Some(tree) = parse(text) else {
        return Vec::new();
    };
    let root = tree.root_node();
    let mut package = String::new();
    let mut imports = HashMap::new();
    let mut wildcards = Vec::new();
    let mut cursor = root.walk();
    for child in root.named_children(&mut cursor) {
        match child.kind() {
            "package_declaration" => {
                let raw = text_of(text, child).trim();
                package = raw
                    .strip_prefix("package")
                    .unwrap_or(raw)
                    .trim()
                    .trim_end_matches(';')
                    .trim()
                    .to_owned();
            }
            "import_declaration" => {
                let raw = text_of(text, child).trim();
                if raw.starts_with("import static ") {
                    continue;
                }
                let path = raw
                    .trim_start_matches("import ")
                    .trim_end_matches(';')
                    .trim();
                if let Some(prefix) = path.strip_suffix(".*") {
                    wildcards.push(prefix.to_owned());
                } else if let Some(index) = path.rfind('.') {
                    imports.insert(path[index + 1..].to_owned(), path[..index].to_owned());
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    let mut cursor = root.walk();
    for child in root.named_children(&mut cursor) {
        if is_type(child) {
            visit_type(child, &package, "", text, &imports, &wildcards, &mut out);
        }
    }
    out
}

fn parse(text: &str) -> Option<Tree> {
    let mut parser = Parser::new();
    parser.set_language(&java_syntax::config().grammar).ok()?;
    parser.parse(text, None)
}

fn is_type(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "class_declaration"
            | "interface_declaration"
            | "enum_declaration"
            | "record_declaration"
            | "annotation_type_declaration"
    )
}

fn visit_type(
    node: Node<'_>,
    package: &str,
    owner: &str,
    text: &str,
    imports: &HashMap<String, String>,
    wildcards: &[String],
    out: &mut Vec<TypeInfo>,
) {
    let Some(name_node) = node.child_by_field_name("name") else {
        return;
    };
    let name = text_of(text, name_node).to_owned();
    let qualified = if owner.is_empty() {
        if package.is_empty() {
            name.clone()
        } else {
            format!("{package}.{name}")
        }
    } else {
        format!("{owner}.{name}")
    };
    let mut found = TypeInfo {
        file: 0,
        name: name.clone(),
        qualified: qualified.clone(),
        kind: participant_kind(node.kind()),
        name_offset: name_node.start_byte(),
        package: package.to_owned(),
        imports: imports.clone(),
        wildcards: wildcards.to_vec(),
        fields: HashMap::new(),
        methods: Vec::new(),
    };
    if let Some(body) = node.child_by_field_name("body") {
        let mut cursor = body.walk();
        for child in body.named_children(&mut cursor) {
            match child.kind() {
                "field_declaration" | "constant_declaration" => {
                    read_fields(child, text, &mut found.fields);
                }
                "method_declaration" => {
                    if let Some(method) = read_method(child, text, false, &name) {
                        found.methods.push(method);
                    }
                }
                "constructor_declaration" => {
                    if let Some(method) = read_method(child, text, true, &name) {
                        found.methods.push(method);
                    }
                }
                "compact_constructor_declaration" => {
                    if let Some(method) = read_method(child, text, true, &name) {
                        found.methods.push(method);
                    }
                }
                _ if is_type(child) => {
                    visit_type(child, package, &qualified, text, imports, wildcards, out);
                }
                _ => {}
            }
        }
    }
    out.push(found);
}

fn participant_kind(kind: &str) -> SequenceParticipantKind {
    match kind {
        "interface_declaration" | "annotation_type_declaration" => {
            SequenceParticipantKind::Interface
        }
        _ => SequenceParticipantKind::Class,
    }
}

fn read_fields(node: Node<'_>, text: &str, out: &mut HashMap<String, String>) {
    let Some(type_node) = node.child_by_field_name("type") else {
        return;
    };
    let type_name = clean_type(text_of(text, type_node));
    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        let mut cursor = current.walk();
        for child in current.named_children(&mut cursor) {
            if child.kind() == "variable_declarator" {
                if let Some(name) = child.child_by_field_name("name") {
                    out.insert(text_of(text, name).to_owned(), type_name.clone());
                }
            } else if !is_type(child) {
                stack.push(child);
            }
        }
    }
}

fn read_method(
    node: Node<'_>,
    text: &str,
    constructor: bool,
    type_name: &str,
) -> Option<MethodInfo> {
    let name_node = node.child_by_field_name("name")?;
    let name = if constructor {
        type_name.to_owned()
    } else {
        text_of(text, name_node).to_owned()
    };
    let (parameter_names, parameter_types) = read_parameters(node, text);
    let visibility = visibility_of(node, text);
    let return_type = if constructor {
        String::new()
    } else {
        node.child_by_field_name("type")
            .map(|kind| clean_type(text_of(text, kind)))
            .unwrap_or_default()
    };
    let body = node
        .child_by_field_name("body")
        .map(|body| body.start_byte()..body.end_byte());
    Some(MethodInfo {
        name,
        return_type,
        parameter_names,
        parameter_types,
        visibility,
        constructor,
        name_offset: name_node.start_byte(),
        declaration: node.start_byte()..node.end_byte(),
        body,
    })
}

fn read_parameters(node: Node<'_>, text: &str) -> (Vec<String>, Vec<String>) {
    let Some(parameters) = node.child_by_field_name("parameters") else {
        return (Vec::new(), Vec::new());
    };
    let mut names = Vec::new();
    let mut types = Vec::new();
    let mut cursor = parameters.walk();
    for parameter in parameters.named_children(&mut cursor) {
        match parameter.kind() {
            "formal_parameter" => {
                let Some(type_node) = parameter.child_by_field_name("type") else {
                    continue;
                };
                let name = parameter
                    .child_by_field_name("name")
                    .map(|name| text_of(text, name).to_owned())
                    .unwrap_or_default();
                types.push(clean_type(text_of(text, type_node)));
                names.push(name);
            }
            "spread_parameter" => {
                let Some(type_node) = parameter.child_by_field_name("type") else {
                    continue;
                };
                let mut cursor = parameter.walk();
                let name = parameter
                    .named_children(&mut cursor)
                    .find(|child| child.kind() == "variable_declarator")
                    .and_then(|child| child.child_by_field_name("name"))
                    .map(|name| text_of(text, name).to_owned())
                    .unwrap_or_default();
                types.push(clean_type(text_of(text, type_node)));
                names.push(name);
            }
            _ => {}
        }
    }
    (names, types)
}

fn visibility_of(node: Node<'_>, text: &str) -> Visibility {
    if has_modifier(node, text, "private") {
        Visibility::Private
    } else if has_modifier(node, text, "protected") {
        Visibility::Protected
    } else if has_modifier(node, text, "public") {
        Visibility::Public
    } else {
        Visibility::Package
    }
}

fn has_modifier(node: Node<'_>, text: &str, wanted: &str) -> bool {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .filter(|child| child.kind() == "modifiers")
        .any(|modifiers| {
            text_of(text, modifiers)
                .split_whitespace()
                .any(|word| word == wanted)
        })
}

fn text_of<'a>(text: &'a str, node: Node<'_>) -> &'a str {
    &text[node.byte_range()]
}

/// A type as the source spells it, without annotations, type arguments or dimensions.
fn clean_type(text: &str) -> String {
    let mut raw = text.trim();
    while raw.starts_with('@') {
        raw = raw
            .split_once(char::is_whitespace)
            .map_or("", |rest| rest.1);
        raw = raw.trim_start();
    }
    let base = raw.split('<').next().unwrap_or(raw);
    base.trim()
        .trim_end_matches("...")
        .trim_end_matches("[]")
        .trim()
        .to_owned()
}

/// One open frame while the scan walks a body.
struct OpenFrame {
    fragment: usize,
    section: usize,
}

/// The local variables and parameters of the body being walked.
struct Scope {
    owner: usize,
    locals: HashMap<String, String>,
}

/// One message before the frames it belongs to are attached.
struct MessageSpec {
    kind: SequenceMessageKind,
    from: usize,
    to: usize,
    label: String,
    returns: Option<String>,
    place: Option<SourceSpan>,
    depth: usize,
}

struct Builder<'a> {
    index: &'a Index,
    settings: SequenceSettings,
    cancel: &'a Cancel,
    progress: &'a mut dyn FnMut(&SequenceDiagram),
    participants: Vec<SequenceParticipant>,
    positions: HashMap<String, usize>,
    messages: Vec<SequenceMessage>,
    fragments: Vec<SequenceFragment>,
    /// The methods on the way down, so that a cycle stops at the call that closes it.
    stack: Vec<(usize, usize)>,
    next_progress: usize,
    title: String,
}

impl<'a> Builder<'a> {
    fn run(&mut self, entry: MethodRef) -> SequenceDiagram {
        let index = self.index;
        let title = {
            let owner = &index.types[entry.owner];
            let method = &owner.methods[entry.method];
            format!(
                "{}.{}({})",
                owner.name,
                method.name,
                method.parameter_types.join(", ")
            )
        };
        self.title = title;
        self.participant_known(entry.owner);
        let key = (
            entry.file,
            index.types[entry.owner].methods[entry.method].name_offset,
        );
        self.stack.push(key);
        let mut frames = Vec::new();
        self.walk_body(entry, &mut frames, 1);
        self.stack.pop();
        self.snapshot()
    }

    fn snapshot(&self) -> SequenceDiagram {
        SequenceDiagram {
            participants: self.participants.clone(),
            messages: self.messages.clone(),
            fragments: self.fragments.clone(),
            title: self.title.clone(),
        }
    }

    fn maybe_progress(&mut self) {
        if self.messages.len() < self.next_progress {
            return;
        }
        self.next_progress += PROGRESS_STEP;
        if self.cancel.is_cancelled() {
            return;
        }
        let snapshot = self.snapshot();
        (self.progress)(&snapshot);
    }

    fn participant_known(&mut self, at: usize) -> usize {
        let index = self.index;
        let ty = &index.types[at];
        if let Some(found) = self.positions.get(&ty.qualified) {
            return *found;
        }
        let file = &index.files[ty.file];
        let place = crate::diagram::place_in(
            &file.path,
            &file.text,
            ty.name_offset,
            ty.name_offset + ty.name.len(),
        );
        let position = self.participants.len();
        self.positions.insert(ty.qualified.clone(), position);
        self.participants.push(SequenceParticipant {
            name: ty.name.clone(),
            qualified: ty.qualified.clone(),
            kind: ty.kind,
            place: Some(place),
        });
        position
    }

    fn participant_external(&mut self, name: &str) -> usize {
        let key = format!("external:{name}");
        if let Some(found) = self.positions.get(&key) {
            return *found;
        }
        let position = self.participants.len();
        self.positions.insert(key, position);
        self.participants.push(SequenceParticipant {
            name: name.to_owned(),
            qualified: name.to_owned(),
            kind: SequenceParticipantKind::External,
            place: None,
        });
        position
    }

    fn target_participant(&mut self, target: &TargetType) -> usize {
        match target {
            TargetType::Known(at) => self.participant_known(*at),
            TargetType::External(name) => self.participant_external(name),
        }
    }

    fn walk_body(&mut self, method: MethodRef, frames: &mut Vec<OpenFrame>, depth: usize) {
        if self.cancel.is_cancelled() {
            return;
        }
        let index = self.index;
        let ty = &index.types[method.owner];
        let info = &ty.methods[method.method];
        let Some(body) = info.body.clone() else {
            return;
        };
        let file = &index.files[method.file];
        let text = &file.text;
        let Some(tree) = parse(text) else {
            return;
        };
        let root = tree.root_node();
        let Some(node) = root.descendant_for_byte_range(body.start, body.end) else {
            return;
        };
        let mut locals = HashMap::new();
        for (name, kind) in info.parameter_names.iter().zip(info.parameter_types.iter()) {
            locals.insert(name.clone(), kind.clone());
        }
        collect_locals(node, text, &mut locals);
        let scope = Scope {
            owner: method.owner,
            locals,
        };
        self.visit_node(node, &scope, depth, frames);
    }

    /// Walks the children of a statement or an expression in the order they run.
    fn visit_node(
        &mut self,
        node: Node<'_>,
        scope: &Scope,
        depth: usize,
        frames: &mut Vec<OpenFrame>,
    ) {
        if self.cancel.is_cancelled() || depth > self.settings.depth {
            return;
        }
        match node.kind() {
            "method_invocation" => self.visit_invocation(node, scope, depth, frames),
            "object_creation_expression" => self.visit_creation(node, scope, depth, frames),
            "if_statement" => self.visit_if(node, scope, depth, frames),
            "switch_expression" => self.visit_switch(node, scope, depth, frames),
            "for_statement" | "enhanced_for_statement" | "while_statement" | "do_statement" => {
                self.visit_loop(node, scope, depth, frames)
            }
            "lambda_expression" => {
                if let Some(body) = node.child_by_field_name("body") {
                    self.visit_node(body, scope, depth, frames);
                }
            }
            _ => {
                let mut cursor = node.walk();
                let children: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
                for child in children {
                    self.visit_node(child, scope, depth, frames);
                }
            }
        }
    }

    fn visit_invocation(
        &mut self,
        node: Node<'_>,
        scope: &Scope,
        depth: usize,
        frames: &mut Vec<OpenFrame>,
    ) {
        let text = self.text_of_owner(scope.owner);
        let Some(name_node) = node.child_by_field_name("name") else {
            return;
        };
        let name = text_of(text, name_node).to_owned();
        let arguments = node.child_by_field_name("arguments");
        let arity = arguments.map_or(0, |arguments| arguments.named_child_count());
        if let Some(object) = node.child_by_field_name("object") {
            self.visit_node(object, scope, depth, frames);
        }
        let mut lambda = None;
        if let Some(arguments) = arguments {
            let mut cursor = arguments.walk();
            for argument in arguments.named_children(&mut cursor) {
                if argument.kind() == "lambda_expression" {
                    lambda = Some(argument);
                } else {
                    self.visit_node(argument, scope, depth, frames);
                }
            }
        }
        if name == "forEach"
            && let Some(lambda) = lambda
        {
            self.open_frame(SequenceFragmentKind::Loop, "forEach", frames);
            if let Some(body) = lambda.child_by_field_name("body") {
                self.visit_node(body, scope, depth, frames);
            }
            self.close_frame(frames);
            return;
        }
        if let Some(lambda) = lambda
            && let Some(body) = lambda.child_by_field_name("body")
        {
            self.visit_node(body, scope, depth, frames);
        }
        let object = node.child_by_field_name("object");
        let target = self.resolve_call(scope, object, &name, arity, text);
        if self.skipped(&target, &name, arity) {
            return;
        }
        let returns = match (&target.target, target.method) {
            (TargetType::Known(at), Some(method)) => {
                let method = &self.index.types[*at].methods[method];
                if method.constructor {
                    None
                } else {
                    let returns = clean_type(&method.return_type);
                    (!returns.is_empty() && returns != "void").then_some(returns)
                }
            }
            _ => None,
        };
        let from = self.participant_known(scope.owner);
        let to = self.target_participant(&target.target);
        let kind = if from == to {
            SequenceMessageKind::SelfCall
        } else {
            SequenceMessageKind::Call
        };
        let label = self.call_label(&name, arguments, text);
        let place = self.place_of(scope.owner, node);
        self.emit(
            MessageSpec {
                kind,
                from,
                to,
                label,
                returns: returns.clone(),
                place,
                depth,
            },
            frames,
        );
        if let TargetType::Known(at) = &target.target
            && let Some(method) = target.method
            && depth < self.settings.depth
            && self.index.types[*at].methods[method].body.is_some()
        {
            let next = MethodRef {
                owner: *at,
                method,
                file: self.index.types[*at].file,
            };
            let key = (next.file, self.index.types[*at].methods[method].name_offset);
            if !self.stack.contains(&key) {
                self.stack.push(key);
                self.walk_body(next, frames, depth + 1);
                self.stack.pop();
            }
        }
        if let Some(returns) = returns {
            self.emit(
                MessageSpec {
                    kind: SequenceMessageKind::Return,
                    from: to,
                    to: from,
                    label: returns.clone(),
                    returns: Some(returns),
                    place: None,
                    depth,
                },
                frames,
            );
        }
    }

    fn visit_creation(
        &mut self,
        node: Node<'_>,
        scope: &Scope,
        depth: usize,
        frames: &mut Vec<OpenFrame>,
    ) {
        let text = self.text_of_owner(scope.owner);
        let Some(type_node) = node.child_by_field_name("type") else {
            return;
        };
        let type_name = clean_type(text_of(text, type_node));
        let arguments = node.child_by_field_name("arguments");
        let arity = arguments.map_or(0, |arguments| arguments.named_child_count());
        if let Some(arguments) = arguments {
            let mut cursor = arguments.walk();
            let children: Vec<Node<'_>> = arguments.named_children(&mut cursor).collect();
            for argument in children {
                self.visit_node(argument, scope, depth, frames);
            }
        }
        if self.settings.skip_constructors {
            return;
        }
        let known = self.index.type_named(scope.owner, &type_name);
        let target = match known {
            Some(at) => {
                let constructor = self.index.constructor_of(at, arity);
                CallTarget {
                    target: TargetType::Known(at),
                    method: constructor,
                }
            }
            None => CallTarget {
                target: TargetType::External(simple_name(&type_name).to_owned()),
                method: None,
            },
        };
        if self.settings.project_only && matches!(target.target, TargetType::External(_)) {
            return;
        }
        let from = self.participant_known(scope.owner);
        let to = self.target_participant(&target.target);
        let label = self.call_label(&format!("new {}", simple_name(&type_name)), arguments, text);
        let place = self.place_of(scope.owner, node);
        self.emit(
            MessageSpec {
                kind: SequenceMessageKind::Creation,
                from,
                to,
                label,
                returns: None,
                place,
                depth,
            },
            frames,
        );
        if let TargetType::Known(at) = &target.target
            && let Some(method) = target.method
            && depth < self.settings.depth
        {
            let next = MethodRef {
                owner: *at,
                method,
                file: self.index.types[*at].file,
            };
            let key = (next.file, self.index.types[*at].methods[method].name_offset);
            if self.index.types[*at].methods[method].body.is_some() && !self.stack.contains(&key) {
                self.stack.push(key);
                self.walk_body(next, frames, depth + 1);
                self.stack.pop();
            }
        }
    }

    fn visit_if(
        &mut self,
        node: Node<'_>,
        scope: &Scope,
        depth: usize,
        frames: &mut Vec<OpenFrame>,
    ) {
        let text = self.text_of_owner(scope.owner);
        let condition = node
            .child_by_field_name("condition")
            .map(|condition| condition_text(text, condition))
            .unwrap_or_default();
        if let Some(condition_node) = node.child_by_field_name("condition") {
            self.visit_node(condition_node, scope, depth, frames);
        }
        let alternative = node.child_by_field_name("alternative");
        let kind = if alternative.is_some() {
            SequenceFragmentKind::Alt
        } else {
            SequenceFragmentKind::Opt
        };
        self.open_frame(kind, &condition, frames);
        if let Some(consequence) = node.child_by_field_name("consequence") {
            self.visit_node(consequence, scope, depth, frames);
        }
        if let Some(alternative) = alternative {
            self.next_section("else", frames);
            self.visit_node(alternative, scope, depth, frames);
        }
        self.close_frame(frames);
    }

    fn visit_switch(
        &mut self,
        node: Node<'_>,
        scope: &Scope,
        depth: usize,
        frames: &mut Vec<OpenFrame>,
    ) {
        let text = self.text_of_owner(scope.owner);
        if let Some(condition) = node.child_by_field_name("condition") {
            self.visit_node(condition, scope, depth, frames);
        }
        let Some(body) = node.child_by_field_name("body") else {
            return;
        };
        let mut groups = Vec::new();
        let mut cursor = body.walk();
        for child in body.named_children(&mut cursor) {
            if matches!(child.kind(), "switch_block_statement_group" | "switch_rule") {
                groups.push(child);
            }
        }
        if groups.is_empty() {
            return;
        }
        let kind = if groups.len() > 1 {
            SequenceFragmentKind::Alt
        } else {
            SequenceFragmentKind::Opt
        };
        for (index, group) in groups.iter().enumerate() {
            let condition = self.switch_condition(*group, text);
            if index == 0 {
                self.open_frame(kind, &condition, frames);
            } else {
                self.next_section(&condition, frames);
            }
            let mut cursor = group.walk();
            let parts: Vec<Node<'_>> = group.named_children(&mut cursor).collect();
            for part in parts {
                if part.kind() == "switch_label" {
                    continue;
                }
                self.visit_node(part, scope, depth, frames);
            }
        }
        self.close_frame(frames);
    }

    fn switch_condition(&self, group: Node<'_>, text: &str) -> String {
        let mut labels = Vec::new();
        let mut cursor = group.walk();
        for child in group.named_children(&mut cursor) {
            if child.kind() == "switch_label" {
                labels.push(condition_text(text, child));
            }
        }
        if labels.is_empty() {
            "case".to_owned()
        } else {
            labels.join(" / ")
        }
    }

    fn visit_loop(
        &mut self,
        node: Node<'_>,
        scope: &Scope,
        depth: usize,
        frames: &mut Vec<OpenFrame>,
    ) {
        let text = self.text_of_owner(scope.owner);
        let Some(body) = node.child_by_field_name("body") else {
            return;
        };
        let header = text[node.start_byte()..body.start_byte()]
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        match node.kind() {
            "for_statement" => {
                if let Some(init) = node.child_by_field_name("init") {
                    self.visit_node(init, scope, depth, frames);
                }
                if let Some(condition) = node.child_by_field_name("condition") {
                    self.visit_node(condition, scope, depth, frames);
                }
            }
            "enhanced_for_statement" => {
                if let Some(value) = node.child_by_field_name("value") {
                    self.visit_node(value, scope, depth, frames);
                }
            }
            "while_statement" => {
                if let Some(condition) = node.child_by_field_name("condition") {
                    self.visit_node(condition, scope, depth, frames);
                }
            }
            _ => {}
        }
        self.open_frame(SequenceFragmentKind::Loop, &header, frames);
        self.visit_node(body, scope, depth, frames);
        self.close_frame(frames);
    }

    fn resolve_call(
        &self,
        scope: &Scope,
        object: Option<Node<'_>>,
        name: &str,
        arity: usize,
        text: &str,
    ) -> CallTarget {
        let Some(object) = object else {
            return CallTarget {
                target: TargetType::Known(scope.owner),
                method: self.index.method_of(scope.owner, name, arity),
            };
        };
        if object.kind() == "this" {
            return CallTarget {
                target: TargetType::Known(scope.owner),
                method: self.index.method_of(scope.owner, name, arity),
            };
        }
        if object.kind() == "super" {
            return CallTarget {
                target: TargetType::Known(scope.owner),
                method: None,
            };
        }
        let type_name = self
            .index
            .type_of_expr(scope.owner, &scope.locals, object, text, 0);
        let Some(type_name) = type_name else {
            return CallTarget {
                target: TargetType::External(receiver_name(object, text)),
                method: None,
            };
        };
        match self.index.type_named(scope.owner, &type_name) {
            Some(at) => CallTarget {
                target: TargetType::Known(at),
                method: self.index.method_of(at, name, arity),
            },
            None => CallTarget {
                target: TargetType::External(simple_name(&type_name).to_owned()),
                method: None,
            },
        }
    }

    fn skipped(&self, target: &CallTarget, name: &str, arity: usize) -> bool {
        if self.settings.skip_accessors && is_accessor(name, arity) {
            return true;
        }
        if let TargetType::External(_) = target.target {
            return self.settings.project_only;
        }
        let TargetType::Known(at) = target.target else {
            return false;
        };
        match target.method {
            Some(method) => {
                let method = &self.index.types[at].methods[method];
                self.settings.skip_private && method.visibility == Visibility::Private
            }
            None => false,
        }
    }

    fn call_label(&self, name: &str, arguments: Option<Node<'_>>, text: &str) -> String {
        let mut parts = Vec::new();
        if let Some(arguments) = arguments {
            let mut cursor = arguments.walk();
            for argument in arguments.named_children(&mut cursor) {
                let mut raw = text_of(text, argument)
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                if raw.chars().count() > ARGUMENT_LIMIT {
                    raw = raw.chars().take(ARGUMENT_LIMIT - 3).collect::<String>() + "...";
                }
                parts.push(raw);
            }
        }
        format!("{name}({})", parts.join(", "))
    }

    fn place_of(&self, owner: usize, node: Node<'_>) -> Option<SourceSpan> {
        let ty = &self.index.types[owner];
        let file = &self.index.files[ty.file];
        Some(crate::diagram::place_in(
            &file.path,
            &file.text,
            node.start_byte(),
            node.end_byte(),
        ))
    }

    fn text_of_owner(&self, owner: usize) -> &'a str {
        let ty = &self.index.types[owner];
        &self.index.files[ty.file].text
    }

    fn emit(&mut self, message: MessageSpec, frames: &[OpenFrame]) {
        self.messages.push(SequenceMessage {
            from: message.from,
            to: message.to,
            kind: message.kind,
            label: message.label,
            returns: message.returns,
            place: message.place,
            fragments: frames.iter().map(|frame| frame.fragment).collect(),
            depth: message.depth,
        });
        self.maybe_progress();
    }

    fn open_frame(
        &mut self,
        kind: SequenceFragmentKind,
        condition: &str,
        frames: &mut Vec<OpenFrame>,
    ) {
        let fragment = self.fragments.len();
        self.fragments.push(SequenceFragment {
            kind,
            sections: vec![SequenceSection {
                condition: condition.to_owned(),
                messages: self.messages.len()..self.messages.len(),
            }],
            depth: frames.len(),
        });
        frames.push(OpenFrame {
            fragment,
            section: 0,
        });
    }

    fn next_section(&mut self, condition: &str, frames: &mut [OpenFrame]) {
        let Some(open) = frames.last_mut() else {
            return;
        };
        let section = open.section + 1;
        open.section = section;
        let at = self.messages.len();
        if let Some(found) = self.fragments[open.fragment].sections.last_mut() {
            found.messages.end = at;
        }
        self.fragments[open.fragment]
            .sections
            .push(SequenceSection {
                condition: condition.to_owned(),
                messages: at..at,
            });
    }

    fn close_frame(&mut self, frames: &mut Vec<OpenFrame>) {
        let Some(open) = frames.pop() else {
            return;
        };
        let at = self.messages.len();
        if let Some(section) = self.fragments[open.fragment].sections.last_mut() {
            section.messages.end = at;
        }
    }
}

fn collect_locals(node: Node<'_>, text: &str, out: &mut HashMap<String, String>) {
    match node.kind() {
        "local_variable_declaration" => {
            if let Some(type_node) = node.child_by_field_name("type") {
                let type_name = clean_type(text_of(text, type_node));
                let mut stack = vec![node];
                while let Some(current) = stack.pop() {
                    let mut cursor = current.walk();
                    for child in current.named_children(&mut cursor) {
                        if child.kind() == "variable_declarator" {
                            if let Some(name) = child.child_by_field_name("name") {
                                out.insert(text_of(text, name).to_owned(), type_name.clone());
                            }
                        } else if !is_type(child) {
                            stack.push(child);
                        }
                    }
                }
            }
        }
        "enhanced_for_statement" => {
            if let (Some(type_node), Some(name)) = (
                node.child_by_field_name("type"),
                node.child_by_field_name("name"),
            ) {
                out.insert(
                    text_of(text, name).to_owned(),
                    clean_type(text_of(text, type_node)),
                );
            }
        }
        "catch_formal_parameter" => {
            if let (Some(type_node), Some(name)) = (
                node.child_by_field_name("type"),
                node.child_by_field_name("name"),
            ) {
                out.insert(
                    text_of(text, name).to_owned(),
                    clean_type(text_of(text, type_node)),
                );
            }
        }
        _ => {}
    }
    let mut cursor = node.walk();
    let children: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
    for child in children {
        if is_type(child) {
            continue;
        }
        collect_locals(child, text, out);
    }
}

fn receiver_name(node: Node<'_>, text: &str) -> String {
    match node.kind() {
        "identifier" | "field_access" => text_of(text, node)
            .split(|character: char| !character.is_alphanumeric() && character != '_')
            .find(|name| !name.is_empty() && *name != "this" && *name != "super")
            .unwrap_or("object")
            .to_owned(),
        _ => simple_name(&clean_type(text_of(text, node))).to_owned(),
    }
}

fn simple_name(name: &str) -> &str {
    name.rsplit('.').next().unwrap_or(name)
}

fn condition_text(text: &str, node: Node<'_>) -> String {
    let raw = text_of(text, node)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let raw = raw
        .strip_prefix('(')
        .and_then(|rest| rest.strip_suffix(')'))
        .unwrap_or(&raw)
        .trim();
    raw.to_owned()
}

fn is_accessor(name: &str, arity: usize) -> bool {
    let getter =
        (name.starts_with("get") && name.len() > 3) || (name.starts_with("is") && name.len() > 2);
    let setter = name.starts_with("set") && name.len() > 3;
    (getter && arity == 0) || (setter && arity == 1)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use lang_api::{DocumentSpot, SequenceMessageKind, TextPoint};
    use platform::CanonPath;
    use test_support::TempDir;

    use super::*;

    fn write(root: &Path, name: &str, text: &str) {
        let path = root.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, text).unwrap();
    }

    fn point_of(text: &str, offset: usize) -> TextPoint {
        let offset = offset.min(text.len());
        let before = &text[..offset];
        let line = before.bytes().filter(|byte| *byte == b'\n').count() as u32;
        let line_start = before.rfind('\n').map_or(0, |index| index + 1);
        let character = text[line_start..offset].encode_utf16().count() as u32;
        TextPoint { line, character }
    }

    fn spot_at(path: &Path, marker: &str) -> DocumentSpot {
        let text = fs::read_to_string(path).unwrap();
        let offset = text.find(marker).unwrap_or(0) + marker.len() / 2;
        DocumentSpot {
            uri: CanonPath::new(path).unwrap().to_uri(),
            version: 1,
            point: point_of(&text, offset),
        }
    }

    fn diagram(
        root: &Path,
        path: &Path,
        marker: &str,
        settings: SequenceSettings,
    ) -> SequenceDiagram {
        let query = SequenceQuery {
            spot: spot_at(path, marker),
            settings,
        };
        let mut progress = |_: &SequenceDiagram| {};
        sequence_of(root, &query, &Cancel::new(), &mut progress)
    }

    fn labels(diagram: &SequenceDiagram) -> Vec<&str> {
        diagram
            .messages
            .iter()
            .map(|message| message.label.as_str())
            .collect()
    }

    fn project(name: &str) -> TempDir {
        let root = TempDir::new(name);
        write(
            &root,
            "src/demo/Controller.java",
            r#"package demo;

import java.util.List;

public class Controller {
    private Repository repository;
    private List<String> items;

    public void run(String name) {
        String found = repository.find(name);
        helper();
        repository.save(name);
    }

    public void choose(boolean flag) {
        if (flag) {
            repository.save("a");
        } else {
            repository.save("b");
        }
    }

    public void repeat(String name) {
        for (int index = 0; index < 2; index++) {
            repository.save(name);
        }
    }

    public void walk() {
        walk();
    }

    public void accessors(Widget widget) {
        widget.getName();
        widget.paint();
        widget.setName("x");
    }

    public void library() {
        items.add("x");
        items.size();
    }

    private void helper() {}
}
"#,
        );
        write(
            &root,
            "src/demo/Repository.java",
            r#"package demo;

public class Repository {
    public String find(String name) {
        return helper(name);
    }

    public void save(String name) {
    }

    private String helper(String name) {
        return name;
    }
}
"#,
        );
        write(
            &root,
            "src/demo/Widget.java",
            r#"package demo;

public class Widget {
    public String getName() { return null; }
    public void setName(String name) {}
    public void paint() {}
}
"#,
        );
        root
    }

    #[test]
    fn messages_follow_the_body_and_the_returns_come_back() {
        let root = project("sequence-order");
        let path = root.join("src/demo/Controller.java");
        let found = diagram(&root, &path, "run", SequenceSettings::default());
        let names: Vec<&str> = found
            .participants
            .iter()
            .map(|participant| participant.name.as_str())
            .collect();
        assert_eq!(names, ["Controller", "Repository"], "{names:?}");
        let labels = labels(&found);
        assert_eq!(labels[0], "find(name)", "{labels:?}");
        assert_eq!(labels[1], "helper(name)", "{labels:?}");
        assert_eq!(labels[2], "String", "{labels:?}");
        assert_eq!(labels[3], "String", "{labels:?}");
        assert_eq!(labels[5], "save(name)", "{labels:?}");
        assert_eq!(
            found.messages[2].kind,
            SequenceMessageKind::Return,
            "the helper returns to find"
        );
        assert_eq!(found.messages[2].from, 1);
        assert_eq!(found.messages[2].to, 1);
        assert_eq!(
            found.messages[3].kind,
            SequenceMessageKind::Return,
            "find returns to run"
        );
        assert_eq!(found.messages[3].to, 0);
    }

    #[test]
    fn a_call_of_the_own_type_is_a_self_call() {
        let root = project("sequence-self");
        let path = root.join("src/demo/Controller.java");
        let found = diagram(&root, &path, "run", SequenceSettings::default());
        let helper = found
            .messages
            .iter()
            .find(|message| message.label == "helper()")
            .expect("run calls helper");
        assert_eq!(helper.kind, SequenceMessageKind::SelfCall);
        assert_eq!(helper.from, helper.to);
    }

    #[test]
    fn an_if_with_an_else_is_one_alt_with_two_sections() {
        let root = project("sequence-alt");
        let path = root.join("src/demo/Controller.java");
        let found = diagram(&root, &path, "choose", SequenceSettings::default());
        assert_eq!(found.fragments.len(), 1, "{:?}", found.fragments);
        let fragment = &found.fragments[0];
        assert_eq!(fragment.kind, SequenceFragmentKind::Alt);
        assert_eq!(fragment.sections.len(), 2);
        assert_eq!(fragment.sections[0].condition, "flag");
        assert_eq!(fragment.sections[1].condition, "else");
        assert_eq!(fragment.sections[0].messages, 0..1);
        assert_eq!(fragment.sections[1].messages, 1..2);
        assert_eq!(found.messages[0].fragments, [0]);
        assert_eq!(found.messages[1].fragments, [0]);
    }

    #[test]
    fn a_for_becomes_a_loop_with_the_messages_inside() {
        let root = project("sequence-loop");
        let path = root.join("src/demo/Controller.java");
        let found = diagram(&root, &path, "repeat", SequenceSettings::default());
        assert_eq!(found.fragments.len(), 1);
        let fragment = &found.fragments[0];
        assert_eq!(fragment.kind, SequenceFragmentKind::Loop);
        assert!(
            fragment.sections[0].condition.starts_with("for ("),
            "{}",
            fragment.sections[0].condition
        );
        assert_eq!(found.messages[0].fragments, [0]);
    }

    #[test]
    fn the_depth_stops_the_walk_below_the_setting() {
        let root = project("sequence-depth");
        let path = root.join("src/demo/Controller.java");
        let shallow = diagram(
            &root,
            &path,
            "run",
            SequenceSettings {
                depth: 1,
                ..SequenceSettings::default()
            },
        );
        assert!(
            !labels(&shallow).contains(&"helper(name)"),
            "the call inside find stays out: {:?}",
            labels(&shallow)
        );
        let deep = diagram(
            &root,
            &path,
            "run",
            SequenceSettings {
                depth: 2,
                ..SequenceSettings::default()
            },
        );
        assert!(
            labels(&deep).contains(&"helper(name)"),
            "the second level comes in: {:?}",
            labels(&deep)
        );
    }

    #[test]
    fn a_recursive_call_does_not_walk_again() {
        let root = project("sequence-recursion");
        let path = root.join("src/demo/Controller.java");
        let found = diagram(&root, &path, "walk", SequenceSettings::default());
        assert_eq!(found.messages.len(), 1, "{:?}", labels(&found));
        assert_eq!(found.messages[0].label, "walk()");
    }

    #[test]
    fn the_filters_leave_the_accessors_private_methods_and_constructors_out() {
        let root = project("sequence-filters");
        let path = root.join("src/demo/Controller.java");
        let filtered = diagram(
            &root,
            &path,
            "accessors",
            SequenceSettings {
                skip_accessors: true,
                ..SequenceSettings::default()
            },
        );
        assert_eq!(labels(&filtered), ["paint()"]);
        let all = diagram(&root, &path, "accessors", SequenceSettings::default());
        assert_eq!(
            labels(&all),
            ["getName()", "String", "paint()", "setName(\"x\")"]
        );
        let private = diagram(
            &root,
            &path,
            "run",
            SequenceSettings {
                skip_private: true,
                ..SequenceSettings::default()
            },
        );
        assert!(
            !labels(&private).contains(&"helper()"),
            "the private method of the type stays out: {:?}",
            labels(&private)
        );
    }

    #[test]
    fn a_library_call_is_one_message_to_an_external_participant() {
        let root = project("sequence-library");
        let path = root.join("src/demo/Controller.java");
        let found = diagram(&root, &path, "library", SequenceSettings::default());
        let external = found
            .participants
            .iter()
            .find(|participant| participant.name == "List")
            .expect("List is an external participant");
        assert_eq!(external.kind, SequenceParticipantKind::External);
        assert_eq!(labels(&found), ["add(\"x\")", "size()"]);
        assert!(
            found
                .messages
                .iter()
                .all(|message| message.kind == SequenceMessageKind::Call),
            "no return arrow of a library call"
        );
        let only_project = diagram(
            &root,
            &path,
            "library",
            SequenceSettings {
                project_only: true,
                ..SequenceSettings::default()
            },
        );
        assert!(
            only_project.messages.is_empty(),
            "{:?}",
            labels(&only_project)
        );
    }

    #[test]
    fn a_creation_is_a_message_and_the_constructor_setting_leaves_it_out() {
        let root = TempDir::new("sequence-creation");
        write(
            &root,
            "src/demo/Maker.java",
            r#"package demo;

public class Maker {
    public void make() {
        new Widget();
    }
}
"#,
        );
        write(
            &root,
            "src/demo/Widget.java",
            "package demo;\n\npublic class Widget {\n    public Widget() {}\n}\n",
        );
        let path = root.join("src/demo/Maker.java");
        let found = diagram(&root, &path, "make", SequenceSettings::default());
        assert_eq!(labels(&found), ["new Widget()"]);
        assert_eq!(found.messages[0].kind, SequenceMessageKind::Creation);
        let skipped = diagram(
            &root,
            &path,
            "make",
            SequenceSettings {
                skip_constructors: true,
                ..SequenceSettings::default()
            },
        );
        assert!(skipped.messages.is_empty());
    }

    #[test]
    fn a_for_each_lambda_becomes_a_loop_and_a_switch_an_alt() {
        let root = TempDir::new("sequence-control");
        write(
            &root,
            "src/demo/Worker.java",
            r#"package demo;

import java.util.List;

public class Worker {
    private List<String> items;
    private Repository repository;

    public void each() {
        items.forEach(item -> repository.save(item));
    }

    public void pick(int kind) {
        switch (kind) {
            case 1:
                repository.save("one");
                break;
            default:
                repository.save("other");
        }
    }
}
"#,
        );
        write(
            &root,
            "src/demo/Repository.java",
            "package demo;\n\npublic class Repository {\n    public void save(String name) {}\n}\n",
        );
        let path = root.join("src/demo/Worker.java");
        let each = diagram(&root, &path, "each", SequenceSettings::default());
        assert_eq!(each.fragments.len(), 1, "{:?}", each.fragments);
        assert_eq!(each.fragments[0].kind, SequenceFragmentKind::Loop);
        assert_eq!(each.fragments[0].sections[0].condition, "forEach");
        assert_eq!(labels(&each), ["save(item)"]);
        let pick = diagram(&root, &path, "pick", SequenceSettings::default());
        assert_eq!(pick.fragments.len(), 1, "{:?}", pick.fragments);
        assert_eq!(pick.fragments[0].kind, SequenceFragmentKind::Alt);
        assert_eq!(pick.fragments[0].sections.len(), 2);
        assert_eq!(labels(&pick), ["save(\"one\")", "save(\"other\")"]);
    }

    #[test]
    #[ignore = "needs IDE_CORPUS with src/petclinic"]
    fn petclinic_process_find_form_reaches_the_repository() {
        let corpus = std::env::var_os("IDE_CORPUS")
            .map(PathBuf::from)
            .unwrap_or_else(|| panic!("set IDE_CORPUS to the folder that holds src/petclinic"));
        let root = corpus.join("src/petclinic");
        let path = root
            .join("src/main/java/org/springframework/samples/petclinic/owner/OwnerController.java");
        let found = diagram(&root, &path, "processFindForm", SequenceSettings::default());
        println!(
            "petclinic processFindForm: {} participants, {} messages",
            found.participants.len(),
            found.messages.len()
        );
        for message in &found.messages {
            println!(
                "  {} -> {} : {}",
                found.participants[message.from].name,
                found.participants[message.to].name,
                message.label
            );
        }
        assert!(
            found.participants.iter().any(|participant| {
                participant.name == "OwnerRepository"
                    && participant.kind == SequenceParticipantKind::Interface
            }),
            "{:?}",
            found
                .participants
                .iter()
                .map(|participant| &participant.name)
                .collect::<Vec<_>>()
        );
        assert!(
            labels(&found)
                .iter()
                .any(|label| label.starts_with("findByLastNameStartingWith(")),
            "{:?}",
            labels(&found)
        );
    }
}
