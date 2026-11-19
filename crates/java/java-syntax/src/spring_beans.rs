//! The declarations of one Java source that matter to a dependency-injection model:
//! the types with their supertypes, the beans of the usual annotations, the fields,
//! constructors and factory methods that receive beans. The scan is native and never
//! blocks the interface; the meaning of an annotation is decided by the caller.

use std::collections::HashMap;
use std::ops::Range;

use tree_sitter::{Node, Parser};

/// An annotation with the name it may give a bean.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Annotation {
    /// The simple name, without its package.
    pub name: String,
    /// The `value` or `name` argument written as a string literal.
    pub value: Option<String>,
    /// The text inside the quotes of `value`, for a place that maps its own offsets.
    pub value_range: Option<Range<usize>>,
    /// Every element of the arguments with its literal text: `nativeQuery` and `true` for
    /// `@Query(value = "...", nativeQuery = true)`. A string keeps its text without the
    /// quotes; a list keeps its first string, as `value` does.
    pub arguments: Vec<(String, String)>,
}

impl Annotation {
    /// The literal text of one named argument.
    pub fn argument(&self, name: &str) -> Option<&str> {
        self.arguments
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

/// One parameter of a constructor, a method or a factory method.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Parameter {
    /// The type as the source spells it, `OwnerRepository` or `List<Owner>`.
    pub type_text: String,
    pub name: String,
    pub annotations: Vec<Annotation>,
    /// The line of the parameter name, from zero.
    pub line: u32,
    /// The byte offset of the parameter name.
    pub offset: usize,
    /// The byte offset of the declared type, the place a check underlines.
    pub type_offset: usize,
}

/// One field of a type.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Field {
    pub type_text: String,
    /// The type arguments of the field type, `Pet` for `List<Pet>`.
    pub type_arguments: Vec<String>,
    pub name: String,
    pub annotations: Vec<Annotation>,
    /// The field is declared `static`, so no framework of a store persists it.
    pub is_static: bool,
    pub line: u32,
    /// The UTF-16 column of the name, where a jump to the field puts the caret.
    pub column: u32,
    pub offset: usize,
    pub type_offset: usize,
}

/// One constructor of a type.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Constructor {
    pub annotations: Vec<Annotation>,
    pub parameters: Vec<Parameter>,
}

/// One method of a type. A factory method is a method with the marker of the framework.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Method {
    pub name: String,
    /// The return type as the source spells it.
    pub return_type: String,
    pub annotations: Vec<Annotation>,
    pub parameters: Vec<Parameter>,
    pub line: u32,
    pub offset: usize,
}

/// One type of a source file. `qualified` names it with its package and its outer types.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Type {
    pub name: String,
    pub qualified: String,
    pub is_interface: bool,
    /// The declaration of an annotation type, `@interface`.
    pub is_annotation: bool,
    /// The supertypes as the source spells them, without packages and type arguments.
    pub supers: Vec<String>,
    /// The type arguments of every supertype, in the order of `supers`: a
    /// `JpaRepository<Owner, Integer>` gives `["Owner", "Integer"]`.
    pub super_arguments: Vec<Vec<String>>,
    /// The names of the type parameters the type declares, `T` and `ID` for
    /// `BaseRepo<T, ID>`.
    pub type_parameters: Vec<String>,
    pub annotations: Vec<Annotation>,
    pub fields: Vec<Field>,
    /// The components of a record, which the canonical constructor takes as parameters.
    pub components: Vec<Field>,
    pub constructors: Vec<Constructor>,
    pub methods: Vec<Method>,
    /// The line of the type name, from zero.
    pub line: u32,
    /// The UTF-16 column of the type name, where a jump to the type puts the caret.
    pub column: u32,
    /// The byte offset of the type name.
    pub offset: usize,
}

/// Everything of one source that the model reads.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct File {
    pub package: String,
    /// The imported names: the simple name and its package or full name. A wildcard
    /// import has `*` as its simple name and the package as its value.
    pub imports: Vec<(String, String)>,
    pub types: Vec<Type>,
}

impl File {
    pub fn imports(&self) -> HashMap<&str, &str> {
        self.imports
            .iter()
            .map(|(name, path)| (name.as_str(), path.as_str()))
            .collect()
    }
}

const TYPE_DECLARATIONS: &[&str] = &[
    "class_declaration",
    "interface_declaration",
    "enum_declaration",
    "record_declaration",
    "annotation_type_declaration",
];

fn parse(text: &str) -> Option<tree_sitter::Tree> {
    let mut parser = Parser::new();
    parser.set_language(&super::config().grammar).ok()?;
    parser.parse(text, None)
}

/// Reads the declarations of a source file. A file the grammar cannot parse gives an
/// empty set.
pub fn spring_file(text: &str) -> File {
    let Some(tree) = parse(text) else {
        return File::default();
    };
    let root = tree.root_node();
    let mut file = File::default();
    let mut cursor = root.walk();
    for child in root.named_children(&mut cursor) {
        match child.kind() {
            "package_declaration" => {
                file.package = text_of(text, child)
                    .trim_start_matches("package")
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

/// The column of a byte offset in UTF-16 units, as a place of the text counts it.
fn column_of(text: &str, offset: usize) -> u32 {
    let offset = offset.min(text.len());
    let line_start = text[..offset].rfind('\n').map_or(0, |index| index + 1);
    text[line_start..offset].encode_utf16().count() as u32
}

/// The names of the type parameters a declaration introduces, in their order.
fn type_parameter_names(node: Node<'_>, text: &str) -> Vec<String> {
    let Some(list) = node.child_by_field_name("type_parameters") else {
        return Vec::new();
    };
    let mut names = Vec::new();
    let mut cursor = list.walk();
    for parameter in list.named_children(&mut cursor) {
        if parameter.kind() != "type_parameter" {
            continue;
        }
        let mut inner = parameter.walk();
        for child in parameter.named_children(&mut inner) {
            if child.kind() == "type_identifier" {
                names.push(text_of(text, child).to_owned());
                break;
            }
        }
    }
    names
}

fn visit_type(node: Node<'_>, package: &str, owner: &str, text: &str, out: &mut Vec<Type>) {
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
    let declared = super_types(node, text);
    let mut found = Type {
        name,
        qualified: qualified.clone(),
        is_interface: node.kind() == "interface_declaration"
            || node.kind() == "annotation_type_declaration",
        is_annotation: node.kind() == "annotation_type_declaration",
        supers: declared.iter().map(|(name, _)| name.clone()).collect(),
        super_arguments: declared
            .into_iter()
            .map(|(_, arguments)| arguments)
            .collect(),
        type_parameters: type_parameter_names(node, text),
        annotations: annotations(node, text),
        line: line_of(text, name_node.start_byte()),
        column: column_of(text, name_node.start_byte()),
        offset: name_node.start_byte(),
        components: record_components(node, text),
        ..Type::default()
    };
    if let Some(body) = node.child_by_field_name("body") {
        let mut cursor = body.walk();
        for child in body.named_children(&mut cursor) {
            match child.kind() {
                "field_declaration" => {
                    let type_node = child.child_by_field_name("type");
                    let type_text = type_node
                        .map(|kind| clean_type(text_of(text, kind)))
                        .unwrap_or_default();
                    let type_arguments = type_node
                        .map(|kind| type_arguments(text_of(text, kind)))
                        .unwrap_or_default();
                    let type_offset =
                        type_node.map_or(child.start_byte(), |kind| kind.start_byte());
                    let annotations = annotations(child, text);
                    let is_static = has_modifier(child, text, "static");
                    let mut inner = child.walk();
                    for declarator in child.named_children(&mut inner) {
                        if declarator.kind() != "variable_declarator" {
                            continue;
                        }
                        let Some(name) = declarator.child_by_field_name("name") else {
                            continue;
                        };
                        found.fields.push(Field {
                            type_text: type_text.clone(),
                            type_arguments: type_arguments.clone(),
                            name: text_of(text, name).to_owned(),
                            annotations: annotations.clone(),
                            is_static,
                            line: line_of(text, name.start_byte()),
                            column: column_of(text, name.start_byte()),
                            offset: name.start_byte(),
                            type_offset,
                        });
                    }
                }
                "constructor_declaration" | "compact_constructor_declaration" => {
                    found.constructors.push(Constructor {
                        annotations: annotations(child, text),
                        parameters: parameters(child, text),
                    });
                }
                "method_declaration" => {
                    let Some(name) = child.child_by_field_name("name") else {
                        continue;
                    };
                    found.methods.push(Method {
                        name: text_of(text, name).to_owned(),
                        return_type: child
                            .child_by_field_name("type")
                            .map(|kind| clean_type(text_of(text, kind)))
                            .unwrap_or_default(),
                        annotations: annotations(child, text),
                        parameters: parameters(child, text),
                        line: line_of(text, name.start_byte()),
                        offset: name.start_byte(),
                    });
                }
                _ if is_type(child) => {
                    visit_type(child, package, &qualified, text, out);
                }
                _ => {}
            }
        }
    }
    out.push(found);
}

/// The components a record header declares, in their order. Each one is the field of the
/// record, so a place that jumps to the declaration of a property finds it the same way.
fn record_components(node: Node<'_>, text: &str) -> Vec<Field> {
    if node.kind() != "record_declaration" {
        return Vec::new();
    }
    let Some(parameters) = node.child_by_field_name("parameters") else {
        return Vec::new();
    };
    let mut found = Vec::new();
    let mut cursor = parameters.walk();
    for parameter in parameters.named_children(&mut cursor) {
        if parameter.kind() != "formal_parameter" {
            continue;
        }
        let Some(name) = parameter.child_by_field_name("name") else {
            continue;
        };
        let type_node = parameter.child_by_field_name("type");
        found.push(Field {
            type_text: type_node
                .map(|kind| clean_type(text_of(text, kind)))
                .unwrap_or_default(),
            type_arguments: type_node
                .map(|kind| type_arguments(text_of(text, kind)))
                .unwrap_or_default(),
            name: text_of(text, name).to_owned(),
            annotations: annotations(parameter, text),
            is_static: false,
            line: line_of(text, name.start_byte()),
            column: column_of(text, name.start_byte()),
            offset: name.start_byte(),
            type_offset: type_node.map_or(parameter.start_byte(), |kind| kind.start_byte()),
        });
    }
    found
}

/// The declared supertypes as the source spells them, without type arguments, with the
/// type arguments of every one.
fn super_types(node: Node<'_>, text: &str) -> Vec<(String, Vec<String>)> {
    let mut found = Vec::new();
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if !matches!(
            child.kind(),
            "superclass" | "super_interfaces" | "extends_interfaces"
        ) {
            continue;
        }
        let raw = text_of(text, child).trim();
        let raw = raw
            .strip_prefix("extends")
            .or_else(|| raw.strip_prefix("implements"))
            .unwrap_or(raw);
        for part in split_types(raw) {
            let name = clean_type(part);
            if !name.is_empty() {
                found.push((name, type_arguments(part)));
            }
        }
    }
    found
}

/// The type arguments of a type as the source spells them, without packages: `Owner` and
/// `Integer` for `JpaRepository<Owner, Integer>`. A wildcard gives its bound.
fn type_arguments(raw: &str) -> Vec<String> {
    let (Some(open), Some(close)) = (raw.find('<'), raw.rfind('>')) else {
        return Vec::new();
    };
    if close <= open {
        return Vec::new();
    }
    split_types(&raw[open + 1..close])
        .into_iter()
        .filter_map(|part| {
            let part = clean_type(part);
            (!part.is_empty()).then_some(part)
        })
        .collect()
}

/// Whether a declaration carries one of the modifiers of its source, `static` for one.
fn has_modifier(declaration: Node<'_>, text: &str, wanted: &str) -> bool {
    let mut cursor = declaration.walk();
    declaration
        .named_children(&mut cursor)
        .filter(|child| child.kind() == "modifiers")
        .any(|modifiers| {
            text_of(text, modifiers)
                .split_whitespace()
                .any(|word| word == wanted)
        })
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
    let out = out.strip_prefix("final ").unwrap_or(out);
    out.split_whitespace().last().unwrap_or(out).to_owned()
}

/// The annotations of a declaration, whether the grammar puts them under `modifiers` or
/// beside the other children.
fn annotations(declaration: Node<'_>, text: &str) -> Vec<Annotation> {
    let mut found = Vec::new();
    let mut cursor = declaration.walk();
    for child in declaration.named_children(&mut cursor) {
        if is_annotation(child) {
            found.push(annotation(child, text));
        } else if child.kind() == "modifiers" {
            let mut inner = child.walk();
            for part in child.named_children(&mut inner) {
                if is_annotation(part) {
                    found.push(annotation(part, text));
                }
            }
        }
    }
    found
}

fn is_annotation(node: Node<'_>) -> bool {
    matches!(node.kind(), "annotation" | "marker_annotation")
}

fn annotation(node: Node<'_>, text: &str) -> Annotation {
    let name = node
        .child_by_field_name("name")
        .map_or("", |name| text_of(text, name))
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_owned();
    Annotation {
        name,
        value: annotation_value(node, text),
        value_range: annotation_value_range(node, text),
        arguments: annotation_arguments(node, text),
    }
}

/// The string a `value` or `name` argument of an annotation holds. A list gives its
/// first string.
fn annotation_value(annotation: Node<'_>, text: &str) -> Option<String> {
    let list = annotation.child_by_field_name("arguments")?;
    let mut cursor = list.walk();
    for argument in list.named_children(&mut cursor) {
        let value = if argument.kind() == "element_value_pair" {
            let key = argument.child_by_field_name("key")?;
            if !matches!(text_of(text, key), "value" | "name") {
                continue;
            }
            argument.child_by_field_name("value")
        } else {
            Some(argument)
        };
        let Some(value) = value else {
            continue;
        };
        return string_of(value, text);
    }
    None
}

/// The place of the text inside the quotes of the `value` or `name` argument.
fn annotation_value_range(annotation: Node<'_>, text: &str) -> Option<Range<usize>> {
    let list = annotation.child_by_field_name("arguments")?;
    let mut cursor = list.walk();
    for argument in list.named_children(&mut cursor) {
        let value = if argument.kind() == "element_value_pair" {
            let key = argument.child_by_field_name("key")?;
            if !matches!(text_of(text, key), "value" | "name") {
                continue;
            }
            argument.child_by_field_name("value")
        } else {
            Some(argument)
        };
        if let Some(value) = value
            && let Some(range) = string_range(value, text)
        {
            return Some(range);
        }
    }
    None
}

/// The text of every argument of an annotation, by the name of the element: a bare value
/// is the element `value`.
fn annotation_arguments(annotation: Node<'_>, text: &str) -> Vec<(String, String)> {
    let Some(list) = annotation.child_by_field_name("arguments") else {
        return Vec::new();
    };
    let mut found = Vec::new();
    let mut cursor = list.walk();
    for argument in list.named_children(&mut cursor) {
        if argument.kind() != "element_value_pair" {
            if let Some(value) = literal_of(argument, text) {
                found.push(("value".to_owned(), value));
            }
            continue;
        }
        let Some(key) = argument.child_by_field_name("key") else {
            continue;
        };
        let Some(value) = argument.child_by_field_name("value") else {
            continue;
        };
        if let Some(value) = literal_of(value, text) {
            found.push((text_of(text, key).trim().to_owned(), value));
        }
    }
    found
}

/// The text of a value node: a string without its quotes, a number, `true` or `false`, or
/// the source text of anything else.
fn literal_of(node: Node<'_>, text: &str) -> Option<String> {
    if node.kind() == "element_value_array_initializer" {
        let mut cursor = node.walk();
        return node
            .named_children(&mut cursor)
            .find_map(|child| literal_of(child, text));
    }
    if node.kind() == "string_literal" {
        return string_of(node, text);
    }
    let raw = text_of(text, node).trim();
    (!raw.is_empty()).then(|| raw.to_owned())
}

/// The width of the quotes of a string literal: three for a text block.
fn quote_width(node: Node<'_>, text: &str) -> usize {
    if text_of(text, node).starts_with("\"\"\"") {
        3
    } else {
        1
    }
}

/// The byte range of the text inside the quotes of a string literal, a text block
/// included.
fn string_range(node: Node<'_>, text: &str) -> Option<Range<usize>> {
    if node.kind() != "string_literal" {
        return None;
    }
    let start = node.start_byte();
    let end = node.end_byte();
    let width = quote_width(node, text);
    (end >= start + 2 * width).then_some(start + width..end - width)
}

/// The text inside the quotes of a string literal. A text block keeps its lines and their
/// indentation, so that a place inside it maps to the same place of the source.
fn string_of(node: Node<'_>, text: &str) -> Option<String> {
    if node.kind() == "element_value_array_initializer" {
        let mut cursor = node.walk();
        return node
            .named_children(&mut cursor)
            .find_map(|child| string_of(child, text));
    }
    if node.kind() != "string_literal" {
        return None;
    }
    let start = node.start_byte();
    let end = node.end_byte();
    let width = quote_width(node, text);
    (end >= start + 2 * width).then(|| text[start + width..end - width].to_owned())
}

fn parameters(declaration: Node<'_>, text: &str) -> Vec<Parameter> {
    let Some(list) = declaration.child_by_field_name("parameters") else {
        return Vec::new();
    };
    let mut found = Vec::new();
    let mut cursor = list.walk();
    for parameter in list.named_children(&mut cursor) {
        if parameter.kind() != "formal_parameter" {
            continue;
        }
        let name_node = parameter.child_by_field_name("name");
        let type_node = parameter.child_by_field_name("type");
        found.push(Parameter {
            type_text: type_node
                .map(|kind| clean_type(text_of(text, kind)))
                .unwrap_or_default(),
            name: name_node.map_or("", |name| text_of(text, name)).to_owned(),
            annotations: annotations(parameter, text),
            line: name_node.map_or(0, |name| line_of(text, name.start_byte())),
            offset: name_node.map_or(parameter.start_byte(), |name| name.start_byte()),
            type_offset: type_node.map_or(parameter.start_byte(), |kind| kind.start_byte()),
        });
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = r#"package demo.owner;

import org.springframework.stereotype.Service;
import org.springframework.beans.factory.annotation.Autowired;
import org.springframework.beans.factory.annotation.Qualifier;
import org.springframework.data.jpa.repository.JpaRepository;

@Service("owners")
class OwnerService implements OwnerApi, Serializable {
    @Autowired
    @Qualifier("ownerRepository")
    private OwnerRepository repository;

    @Autowired
    OwnerService(OwnerRepository repository, Clock clock) {}

    @Bean(name = "clock")
    Clock clock() { return null; }
}

interface OwnerRepository extends JpaRepository<Owner, Long> {}
"#;

    #[test]
    fn a_file_names_its_package_and_imports() {
        let file = spring_file(SOURCE);
        assert_eq!(file.package, "demo.owner");
        let imports = file.imports();
        assert_eq!(
            imports.get("Service"),
            Some(&"org.springframework.stereotype")
        );
        assert!(imports.contains_key("JpaRepository"));
    }

    #[test]
    fn a_type_keeps_its_annotations_and_its_supertypes() {
        let file = spring_file(SOURCE);
        let service = &file.types[0];
        assert_eq!(service.name, "OwnerService");
        assert_eq!(service.qualified, "demo.owner.OwnerService");
        assert_eq!(service.supers, ["OwnerApi", "Serializable"]);
        assert_eq!(service.annotations[0].name, "Service");
        assert_eq!(service.annotations[0].value.as_deref(), Some("owners"));
    }

    #[test]
    fn fields_constructors_and_methods_of_a_type_are_read() {
        let file = spring_file(SOURCE);
        let service = &file.types[0];
        assert_eq!(service.fields[0].name, "repository");
        assert_eq!(service.fields[0].type_text, "OwnerRepository");
        let names: Vec<_> = service.fields[0]
            .annotations
            .iter()
            .map(|annotation| (annotation.name.as_str(), annotation.value.as_deref()))
            .collect();
        assert_eq!(
            names,
            [("Autowired", None), ("Qualifier", Some("ownerRepository"))]
        );
        assert_eq!(service.constructors[0].parameters.len(), 2);
        assert_eq!(
            service.constructors[0].parameters[0].type_text,
            "OwnerRepository"
        );
        assert_eq!(service.methods[0].return_type, "Clock");
        assert_eq!(
            service.methods[0].annotations[0].value.as_deref(),
            Some("clock")
        );
    }

    #[test]
    fn the_components_of_a_record_are_read_as_fields() {
        let source = r#"package demo;
record ServerProperties(int port, String address) {}
"#;
        let file = spring_file(source);
        let kind = &file.types[0];
        assert_eq!(kind.fields.len(), 0, "a record header names no field body");
        let names: Vec<&str> = kind
            .components
            .iter()
            .map(|component| component.name.as_str())
            .collect();
        assert_eq!(names, ["port", "address"]);
        let port = source.find("int port").unwrap() + "int ".len();
        let line_start = source[..port].rfind('\n').map_or(0, |index| index + 1);
        assert_eq!(kind.components[0].line, 1);
        assert_eq!(
            kind.components[0].column,
            source[line_start..port].encode_utf16().count() as u32
        );
        assert_eq!(kind.components[0].type_text, "int");
    }

    #[test]
    fn an_interface_keeps_the_type_arguments_out_of_its_supertypes() {
        let file = spring_file(SOURCE);
        let repository = &file.types[1];
        assert!(repository.is_interface);
        assert_eq!(repository.supers, ["JpaRepository"]);
    }

    #[test]
    fn the_type_arguments_of_a_supertype_name_its_entity() {
        let source = r#"package demo;
interface JpaRepository<T, I> {}
interface OwnerRepository extends JpaRepository<Owner, Integer> {}
"#;
        let file = spring_file(source);
        let repository = &file.types[1];
        assert_eq!(repository.supers, ["JpaRepository"]);
        assert_eq!(repository.super_arguments, [["Owner", "Integer"]]);
    }

    #[test]
    fn a_field_keeps_its_type_arguments_its_static_flag_and_the_arguments_of_its_annotations() {
        let source = r#"package demo;
@Entity(name = "owners")
class Owner {
    @Query(value = "select o from Owner o", nativeQuery = false)
    java.util.List<Pet> findPets(String name);

    @OneToMany
    private java.util.List<Pet> pets = new java.util.ArrayList<>();

    private static final long serialVersionUID = 1L;
    @Transient
    private String cache;
}
"#;
        let file = spring_file(source);
        let owner = &file.types[0];
        assert_eq!(owner.name, "Owner");
        assert_eq!(owner.annotations[0].value.as_deref(), Some("owners"));
        let pets = &owner.fields[0];
        assert_eq!(pets.name, "pets");
        assert_eq!(pets.type_text, "java.util.List");
        assert_eq!(pets.type_arguments, ["Pet"]);
        assert!(!pets.is_static);
        let serial = owner
            .fields
            .iter()
            .find(|field| field.name == "serialVersionUID")
            .expect("the static field is read");
        assert!(serial.is_static);
        let method = &owner.methods[0];
        let query = &method.annotations[0];
        assert_eq!(query.name, "Query");
        assert_eq!(
            query.value.as_deref(),
            Some("select o from Owner o"),
            "the text inside the quotes"
        );
        assert_eq!(query.argument("nativeQuery"), Some("false"));
        let text = &source[query.value_range.clone().expect("the place of the text")];
        assert_eq!(text, "select o from Owner o");
    }

    #[test]
    fn the_type_parameters_of_an_interface_keep_their_names() {
        let source = r#"package demo;
interface BaseRepo<T, ID> extends JpaRepository<T, ID> {}
"#;
        let file = spring_file(source);
        assert_eq!(file.types[0].type_parameters, ["T", "ID"]);
        assert_eq!(file.types[0].super_arguments, [["T", "ID"]]);
    }

    #[test]
    fn a_text_block_of_a_query_keeps_the_places_of_its_text() {
        let source = concat!(
            "package demo;\n",
            "interface OwnerRepository {\n",
            "    @Query(\"\"\"\n",
            "        select o from Owner o\n",
            "        where o.city = :city\n",
            "        \"\"\")\n",
            "    List<Owner> find(String city);\n",
            "}\n",
        );
        let file = spring_file(source);
        let query = &file.types[0].methods[0].annotations[0];
        let value = query.value.as_deref().expect("the text of the block");
        assert!(
            value.starts_with("\n        select o from Owner o"),
            "{value:?}"
        );
        let range = query.value_range.clone().expect("the place of the block");
        assert_eq!(
            &source[range.clone()],
            value,
            "the place is the text itself"
        );
        let at = value.find("city").unwrap();
        assert_eq!(
            &source[range.start + at..range.start + at + "city".len()],
            "city"
        );
    }

    #[test]
    fn a_field_and_a_type_name_keep_their_columns() {
        let source = "package demo;\n@Entity\nclass Owner {\n    private String city;\n}\n";
        let file = spring_file(source);
        assert_eq!(file.types[0].line, 2);
        assert_eq!(file.types[0].column, "class ".len() as u32);
        assert_eq!(file.types[0].fields[0].line, 3);
        assert_eq!(
            file.types[0].fields[0].column,
            "    private String ".len() as u32
        );
    }

    #[test]
    fn a_file_the_grammar_cannot_read_gives_nothing() {
        assert_eq!(spring_file("class {"), File::default());
    }
}
