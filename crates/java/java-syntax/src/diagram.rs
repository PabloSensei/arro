//! The declarations a diagram of types reads from one source: the types with their
//! supertypes, fields and methods. The scan is native and never blocks the interface.

use tree_sitter::{Node, Parser};

/// What a declaration declares.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DiagramTypeKind {
    #[default]
    Class,
    Interface,
    Enum,
    Record,
    Annotation,
}

/// How visible a member is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DiagramVisibility {
    Public,
    Protected,
    #[default]
    Package,
    Private,
}

/// A type a field keeps or a supertype of a declaration.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiagramTypeRef {
    /// The name as the source spells it, without type arguments.
    pub name: String,
    /// The type is a collection of this type, so the diagram marks it with `*`.
    pub collection: bool,
}

/// One field of a declaration.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiagramField {
    pub name: String,
    /// The declared type as the source spells it.
    pub type_text: String,
    /// The types a value of the field may hold, the elements of a collection among them.
    pub refs: Vec<DiagramTypeRef>,
    pub visibility: DiagramVisibility,
    pub static_member: bool,
    pub offset: usize,
    pub line: u32,
}

/// One method of a declaration.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiagramMethod {
    pub name: String,
    /// The return type as the source spells it.
    pub return_type: String,
    pub parameters: Vec<String>,
    pub visibility: DiagramVisibility,
    pub static_member: bool,
    pub abstract_member: bool,
    pub offset: usize,
    pub line: u32,
}

/// One type of a source file. A nested type carries its outer types in `qualified`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiagramType {
    pub name: String,
    /// The name with the package and the outer types, `demo.Owner.Inner`.
    pub qualified: String,
    pub package: String,
    pub kind: DiagramTypeKind,
    pub abstract_type: bool,
    /// The supertypes of the declaration, from `extends`.
    pub extends: Vec<String>,
    /// The interfaces of the declaration, from `implements` and from `extends` of an
    /// interface.
    pub implements: Vec<String>,
    pub fields: Vec<DiagramField>,
    pub methods: Vec<DiagramMethod>,
    /// The line of the type name, from zero.
    pub line: u32,
    /// The byte range of the whole declaration.
    pub start: usize,
    pub end: usize,
    /// The byte offset of the type name.
    pub name_offset: usize,
    /// The byte offset just after the type name.
    pub name_end: usize,
}

/// Everything of one source that the diagram reads.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiagramFile {
    pub package: String,
    /// The imported names: the simple name and its package or full name. A wildcard
    /// import has `*` as its simple name and the package as its value.
    pub imports: Vec<(String, String)>,
    pub types: Vec<DiagramType>,
}

const TYPE_DECLARATIONS: &[&str] = &[
    "class_declaration",
    "interface_declaration",
    "enum_declaration",
    "record_declaration",
    "annotation_type_declaration",
];

/// The collections whose type arguments are the elements the diagram shows with `*`.
const COLLECTIONS: &[&str] = &[
    "Collection",
    "Iterable",
    "List",
    "ArrayList",
    "LinkedList",
    "Set",
    "HashSet",
    "LinkedHashSet",
    "SortedSet",
    "TreeSet",
    "Queue",
    "Deque",
    "ArrayDeque",
    "Map",
    "HashMap",
    "LinkedHashMap",
    "SortedMap",
    "TreeMap",
    "Optional",
    "Stream",
];

fn parse(text: &str) -> Option<tree_sitter::Tree> {
    let mut parser = Parser::new();
    parser.set_language(&super::config().grammar).ok()?;
    parser.parse(text, None)
}

/// Reads the declarations of a source file. A file the grammar cannot parse gives an
/// empty set.
pub fn diagram_file(text: &str) -> DiagramFile {
    let Some(tree) = parse(text) else {
        return DiagramFile::default();
    };
    let root = tree.root_node();
    let mut file = DiagramFile::default();
    let mut cursor = root.walk();
    for child in root.named_children(&mut cursor) {
        match child.kind() {
            "package_declaration" => {
                let raw = text_of(text, child).trim();
                let raw = raw
                    .strip_prefix("package")
                    .unwrap_or(raw)
                    .trim()
                    .trim_end_matches(';');
                file.package = raw.trim().to_owned();
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
                    file.imports.push(("*".to_owned(), prefix.to_owned()));
                } else if let Some(index) = path.rfind('.') {
                    file.imports
                        .push((path[index + 1..].to_owned(), path[..index].to_owned()));
                }
            }
            _ => {
                if is_type(child) {
                    visit_type(child, &file.package, "", text, &mut file.types);
                }
            }
        }
    }
    file
}

fn is_type(node: Node<'_>) -> bool {
    TYPE_DECLARATIONS.contains(&node.kind())
}

fn text_of<'a>(text: &'a str, node: Node<'_>) -> &'a str {
    &text[node.byte_range()]
}

fn line_of(text: &str, offset: usize) -> u32 {
    text[..offset.min(text.len())]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count() as u32
}

fn kind_of(node: Node<'_>) -> DiagramTypeKind {
    match node.kind() {
        "interface_declaration" => DiagramTypeKind::Interface,
        "enum_declaration" => DiagramTypeKind::Enum,
        "record_declaration" => DiagramTypeKind::Record,
        "annotation_type_declaration" => DiagramTypeKind::Annotation,
        _ => DiagramTypeKind::Class,
    }
}

fn visit_type(node: Node<'_>, package: &str, owner: &str, text: &str, out: &mut Vec<DiagramType>) {
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
    let modifiers = modifiers_of(node, text);
    let mut found = DiagramType {
        name,
        qualified: qualified.clone(),
        package: package.to_owned(),
        kind: kind_of(node),
        abstract_type: modifiers.abstract_type,
        extends: extended_types(node, text),
        implements: implemented_types(node, text),
        line: line_of(text, name_node.start_byte()),
        start: node.start_byte(),
        end: node.end_byte(),
        name_offset: name_node.start_byte(),
        name_end: name_node.end_byte(),
        ..DiagramType::default()
    };
    if let Some(body) = node.child_by_field_name("body") {
        let mut cursor = body.walk();
        for child in body.named_children(&mut cursor) {
            match child.kind() {
                "field_declaration" => read_field(child, text, &mut found.fields),
                "constant_declaration" => read_field(child, text, &mut found.fields),
                "enum_constant" => read_enum_constant(child, text, &mut found.fields),
                "method_declaration" => read_method(child, text, &mut found.methods),
                "annotation_type_element_declaration" => {
                    read_method(child, text, &mut found.methods)
                }
                _ if is_type(child) => visit_type(child, package, &qualified, text, out),
                _ => {}
            }
        }
    }
    // A record names its components in the header; each one is a field with an accessor.
    if let Some(parameters) = node.child_by_field_name("parameters") {
        let mut cursor = parameters.walk();
        for parameter in parameters.named_children(&mut cursor) {
            let (Some(name_node), type_node) = (
                parameter.child_by_field_name("name"),
                parameter.child_by_field_name("type"),
            ) else {
                continue;
            };
            let type_text = type_node.map_or(String::new(), |kind| clean_type(text_of(text, kind)));
            found.fields.push(DiagramField {
                name: text_of(text, name_node).to_owned(),
                refs: type_node.map_or_else(Vec::new, |kind| refs_of(kind, text)),
                type_text,
                visibility: DiagramVisibility::Public,
                static_member: false,
                offset: name_node.start_byte(),
                line: line_of(text, name_node.start_byte()),
            });
        }
    }
    if matches!(found.kind, DiagramTypeKind::Interface) {
        for method in &mut found.methods {
            if method.visibility == DiagramVisibility::Package {
                method.visibility = DiagramVisibility::Public;
            }
        }
    }
    out.push(found);
}

fn read_field(node: Node<'_>, text: &str, out: &mut Vec<DiagramField>) {
    let modifiers = modifiers_of(node, text);
    let type_text = node
        .child_by_field_name("type")
        .map_or(String::new(), |kind| clean_type(text_of(text, kind)));
    let refs = node
        .child_by_field_name("type")
        .map_or_else(Vec::new, |kind| refs_of(kind, text));
    let mut cursor = node.walk();
    for declarator in node.named_children(&mut cursor) {
        if declarator.kind() != "variable_declarator" {
            continue;
        }
        let Some(name) = declarator.child_by_field_name("name") else {
            continue;
        };
        out.push(DiagramField {
            name: text_of(text, name).to_owned(),
            type_text: type_text.clone(),
            refs: refs.clone(),
            visibility: modifiers.visibility,
            static_member: modifiers.static_member,
            offset: name.start_byte(),
            line: line_of(text, name.start_byte()),
        });
    }
}

fn read_enum_constant(node: Node<'_>, text: &str, out: &mut Vec<DiagramField>) {
    let Some(name) = node.child_by_field_name("name") else {
        return;
    };
    out.push(DiagramField {
        name: text_of(text, name).to_owned(),
        type_text: String::new(),
        refs: Vec::new(),
        visibility: DiagramVisibility::Public,
        static_member: true,
        offset: name.start_byte(),
        line: line_of(text, name.start_byte()),
    });
}

fn read_method(node: Node<'_>, text: &str, out: &mut Vec<DiagramMethod>) {
    let Some(name) = node.child_by_field_name("name") else {
        return;
    };
    let modifiers = modifiers_of(node, text);
    let parameters = node
        .child_by_field_name("parameters")
        .map(|list| {
            let mut cursor = list.walk();
            list.named_children(&mut cursor)
                .filter(|parameter| parameter.kind() == "formal_parameter")
                .filter_map(|parameter| parameter.child_by_field_name("type"))
                .map(|kind| clean_type(text_of(text, kind)))
                .filter(|name| !name.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let mut cursor = node.walk();
    let has_body = node.child_by_field_name("body").is_some()
        || node
            .named_children(&mut cursor)
            .any(|child| child.kind() == "block");
    out.push(DiagramMethod {
        name: text_of(text, name).to_owned(),
        return_type: node
            .child_by_field_name("type")
            .map_or(String::new(), |kind| clean_type(text_of(text, kind))),
        parameters,
        visibility: modifiers.visibility,
        static_member: modifiers.static_member,
        abstract_member: modifiers.abstract_type || !has_body,
        offset: name.start_byte(),
        line: line_of(text, name.start_byte()),
    });
}

#[derive(Clone, Copy)]
struct Modifiers {
    visibility: DiagramVisibility,
    static_member: bool,
    abstract_type: bool,
}

fn modifiers_of(node: Node<'_>, text: &str) -> Modifiers {
    let mut modifiers = Modifiers {
        visibility: DiagramVisibility::Package,
        static_member: false,
        abstract_type: false,
    };
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() != "modifiers" {
            continue;
        }
        for word in text_of(text, child).split_whitespace() {
            match word {
                "public" => modifiers.visibility = DiagramVisibility::Public,
                "protected" => modifiers.visibility = DiagramVisibility::Protected,
                "private" => modifiers.visibility = DiagramVisibility::Private,
                "static" => modifiers.static_member = true,
                "abstract" => modifiers.abstract_type = true,
                _ => {}
            }
        }
    }
    modifiers
}

/// The class of the `extends` clause, as the source spells it.
fn extended_types(node: Node<'_>, text: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        let is_extended = match child.kind() {
            "superclass" => true,
            // `interface A extends B, C`
            "extends_interfaces" => node.kind() == "interface_declaration",
            _ => false,
        };
        if !is_extended {
            continue;
        }
        let raw = text_of(text, child).trim();
        let raw = raw.strip_prefix("extends").unwrap_or(raw);
        for part in split_types(raw) {
            let name = type_name_of(part);
            if !name.is_empty() {
                found.push(name);
            }
        }
    }
    found
}

/// The interfaces of the `implements` clause and of `extends` of an interface.
fn implemented_types(node: Node<'_>, text: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        let is_implemented = match child.kind() {
            "super_interfaces" => true,
            "extends_interfaces" => node.kind() != "interface_declaration",
            "interfaces" => true,
            _ => false,
        };
        if !is_implemented {
            continue;
        }
        let raw = text_of(text, child).trim();
        let raw = raw
            .strip_prefix("implements")
            .or_else(|| raw.strip_prefix("extends"))
            .unwrap_or(raw);
        for part in split_types(raw) {
            let name = type_name_of(part);
            if !name.is_empty() {
                found.push(name);
            }
        }
    }
    found
}

/// The types a field may hold: the type itself, or the type arguments of a collection.
fn refs_of(node: Node<'_>, text: &str) -> Vec<DiagramTypeRef> {
    let raw = text_of(text, node);
    let base = type_name_of(raw);
    if base.is_empty() {
        return Vec::new();
    }
    let arguments = argument_refs(raw);
    if !arguments.is_empty() && COLLECTIONS.contains(&base.as_str()) {
        return arguments
            .into_iter()
            .map(|name| DiagramTypeRef {
                name,
                collection: true,
            })
            .collect();
    }
    vec![DiagramTypeRef {
        name: base,
        collection: false,
    }]
}

/// The names inside the type arguments of a type, the arguments of a nested generic
/// included.
fn argument_refs(raw: &str) -> Vec<String> {
    let Some(open) = raw.find('<') else {
        return Vec::new();
    };
    let Some(close) = raw.rfind('>') else {
        return Vec::new();
    };
    if close <= open {
        return Vec::new();
    }
    split_types(&raw[open + 1..close])
        .into_iter()
        .flat_map(|part| {
            if part.contains('<') {
                argument_refs(part)
            } else {
                let name = type_name_of(part);
                if name.is_empty() {
                    Vec::new()
                } else {
                    vec![name]
                }
            }
        })
        .collect()
}

/// Splits a list of types at the commas that do not belong to type arguments.
fn split_types(raw: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    for (index, character) in raw.char_indices() {
        match character {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                parts.push(&raw[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    parts.push(&raw[start..]);
    parts
}

/// A type without its type arguments, its array brackets and its annotations.
fn clean_type(raw: &str) -> String {
    type_name_of(raw)
}

/// The last name of a type as the source spells it: `java.util.List<Owner>` is `List`,
/// `Owner[]` is `Owner`, and `final Owner` is `Owner`.
fn type_name_of(raw: &str) -> String {
    let before_arguments = raw.split('<').next().unwrap_or(raw);
    let token = before_arguments
        .split_whitespace()
        .last()
        .unwrap_or(before_arguments);
    let token = token.trim_matches(|character| matches!(character, '[' | ']' | '?' | ',' | '&'));
    token.rsplit('.').next().unwrap_or(token).trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = r#"package demo.owner;

import java.util.List;

public abstract class Owner extends Person implements Named, Serializable {
    private static final String NAME = "owner";
    protected List<Owner> friends;

    public String getName() { return NAME; }
    private void rename(String name) {}
    abstract void notify(String message);

    static class Address {
        String city;
    }

    enum Kind { HOME, WORK }

    record Pair(String left, int right) {}

    @interface Marker {}
}

class Person extends BaseEntity {}
"#;

    #[test]
    fn a_file_names_its_package_and_imports() {
        let file = diagram_file(SOURCE);
        assert_eq!(file.package, "demo.owner");
        assert!(
            file.imports
                .iter()
                .any(|(name, path)| name == "List" && path == "java.util")
        );
    }

    #[test]
    fn a_type_keeps_its_kind_supertypes_and_modifiers() {
        let file = diagram_file(SOURCE);
        let owner = file
            .types
            .iter()
            .find(|kind| kind.name == "Owner")
            .expect("the class is read");
        assert_eq!(owner.qualified, "demo.owner.Owner");
        assert_eq!(owner.kind, DiagramTypeKind::Class);
        assert!(owner.abstract_type);
        assert_eq!(owner.extends, ["Person"]);
        assert_eq!(owner.implements, ["Named", "Serializable"]);
    }

    #[test]
    fn fields_and_methods_keep_their_signatures_and_visibility() {
        let file = diagram_file(SOURCE);
        let owner = file
            .types
            .iter()
            .find(|kind| kind.name == "Owner")
            .expect("the class is read");
        let name = owner
            .fields
            .iter()
            .find(|field| field.name == "NAME")
            .expect("the constant is a field");
        assert!(name.static_member);
        assert_eq!(name.visibility, DiagramVisibility::Private);
        let friends = owner
            .fields
            .iter()
            .find(|field| field.name == "friends")
            .expect("the list is a field");
        assert_eq!(friends.visibility, DiagramVisibility::Protected);
        assert_eq!(
            friends.refs,
            [DiagramTypeRef {
                name: "Owner".to_owned(),
                collection: true,
            }]
        );
        let get = owner
            .methods
            .iter()
            .find(|method| method.name == "getName")
            .expect("the getter is a method");
        assert!(get.parameters.is_empty());
        assert_eq!(get.return_type, "String");
        assert_eq!(get.visibility, DiagramVisibility::Public);
        let rename = owner
            .methods
            .iter()
            .find(|method| method.name == "rename")
            .expect("the method is read");
        assert_eq!(rename.parameters, ["String"]);
        assert_eq!(rename.visibility, DiagramVisibility::Private);
        let notify = owner
            .methods
            .iter()
            .find(|method| method.name == "notify")
            .expect("the abstract method is read");
        assert!(
            notify.abstract_member,
            "a method without a body is abstract"
        );
    }

    #[test]
    fn nested_types_carry_their_outer_type_and_their_own_kind() {
        let file = diagram_file(SOURCE);
        let address = file
            .types
            .iter()
            .find(|kind| kind.name == "Address")
            .expect("the nested class is read");
        assert_eq!(address.qualified, "demo.owner.Owner.Address");
        assert_eq!(address.kind, DiagramTypeKind::Class);
        assert_eq!(address.fields[0].name, "city");
        let kind = file
            .types
            .iter()
            .find(|kind| kind.name == "Kind")
            .expect("the enum is read");
        assert_eq!(kind.kind, DiagramTypeKind::Enum);
        assert!(kind.fields.iter().any(|field| field.name == "HOME"));
        let pair = file
            .types
            .iter()
            .find(|kind| kind.name == "Pair")
            .expect("the record is read");
        assert_eq!(pair.kind, DiagramTypeKind::Record);
        assert_eq!(pair.fields.len(), 2);
        assert_eq!(pair.fields[0].visibility, DiagramVisibility::Public);
        let marker = file
            .types
            .iter()
            .find(|kind| kind.name == "Marker")
            .expect("the annotation type is read");
        assert_eq!(marker.kind, DiagramTypeKind::Annotation);
    }

    #[test]
    fn an_interface_extends_another_interface() {
        let file = diagram_file("interface A extends B, C {}\n");
        assert_eq!(file.types[0].kind, DiagramTypeKind::Interface);
        assert_eq!(file.types[0].extends, ["B", "C"]);
        assert!(file.types[0].implements.is_empty());
    }

    #[test]
    fn a_file_the_grammar_cannot_read_gives_nothing() {
        assert_eq!(diagram_file("class {"), DiagramFile::default());
    }
}
