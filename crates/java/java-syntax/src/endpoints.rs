//! HTTP handlers of one Java source: the annotations of a controller and the paths and
//! parameters they declare. The scan is native, so the Endpoints window fills without a
//! running server.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use lang_api::{Endpoint, EndpointParameter, EndpointParameterKind, HttpMethod};
use tree_sitter::{Node, Parser, Tree};

/// The annotations that mark a type as a controller of a web framework.
const CONTROLLERS: &[&str] = &["RestController", "Controller"];
/// The annotation of a type that prefixes every path under it.
const CLASS_MAPPING: &str = "RequestMapping";
/// The annotation of a path of the second family of frameworks.
const CLASS_PATH: &str = "Path";

/// The mappings whose name is their method, `GetMapping`.
const METHOD_MAPPINGS: &[(&str, HttpMethod)] = &[
    ("GetMapping", HttpMethod::Get),
    ("PostMapping", HttpMethod::Post),
    ("PutMapping", HttpMethod::Put),
    ("DeleteMapping", HttpMethod::Delete),
    ("PatchMapping", HttpMethod::Patch),
    ("RequestMapping", HttpMethod::Any),
];

/// The annotations whose name is the method they answer, `@GET`.
const METHOD_ANNOTATIONS: &[(&str, HttpMethod)] = &[
    ("GET", HttpMethod::Get),
    ("POST", HttpMethod::Post),
    ("PUT", HttpMethod::Put),
    ("DELETE", HttpMethod::Delete),
    ("PATCH", HttpMethod::Patch),
    ("HEAD", HttpMethod::Head),
    ("OPTIONS", HttpMethod::Options),
];

const TYPE_DECLARATIONS: &[&str] = &[
    "class_declaration",
    "interface_declaration",
    "enum_declaration",
    "record_declaration",
];

fn parse(text: &str) -> Option<Tree> {
    let mut parser = Parser::new();
    parser.set_language(&super::config().grammar).ok()?;
    parser.parse(text, None)
}

/// The handlers of a source file, in the order they appear.
pub fn endpoints(path: &Path, text: &str) -> Vec<Endpoint> {
    let Some(tree) = parse(text) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    let package = package_of(tree.root_node(), text);
    let mut cursor = tree.root_node().walk();
    for child in tree.root_node().named_children(&mut cursor) {
        if is_type(child) {
            visit_type(child, &package, "", path, text, &mut found);
        }
    }
    found
}

fn is_type(node: Node<'_>) -> bool {
    TYPE_DECLARATIONS.contains(&node.kind())
}

fn text_of<'a>(text: &'a str, node: Node<'_>) -> &'a str {
    &text[node.byte_range()]
}

fn package_of(root: Node<'_>, text: &str) -> String {
    let mut cursor = root.walk();
    for child in root.named_children(&mut cursor) {
        if child.kind() == "package_declaration" {
            return text_of(text, child)
                .trim_start_matches("package")
                .trim()
                .trim_end_matches(';')
                .trim()
                .to_owned();
        }
    }
    String::new()
}

/// The annotations of a declaration, whether the grammar puts them under `modifiers` or
/// beside the other children.
fn annotations<'a>(declaration: Node<'a>) -> Vec<Node<'a>> {
    let mut found = Vec::new();
    let mut cursor = declaration.walk();
    for child in declaration.named_children(&mut cursor) {
        if is_annotation(child) {
            found.push(child);
        } else if child.kind() == "modifiers" {
            let mut inner = child.walk();
            for part in child.named_children(&mut inner) {
                if is_annotation(part) {
                    found.push(part);
                }
            }
        }
    }
    found
}

fn is_annotation(node: Node<'_>) -> bool {
    matches!(node.kind(), "annotation" | "marker_annotation")
}

/// The simple name of an annotation, without its package.
fn annotation_name<'a>(annotation: Node<'_>, text: &'a str) -> &'a str {
    annotation
        .child_by_field_name("name")
        .map_or("", |name| text_of(text, name))
        .rsplit('.')
        .next()
        .unwrap_or("")
}

fn find_annotation<'a>(annotations: &[Node<'a>], name: &str, text: &str) -> Option<Node<'a>> {
    annotations
        .iter()
        .copied()
        .find(|annotation| annotation_name(*annotation, text) == name)
}

fn arguments<'a>(annotation: Node<'a>) -> Vec<Node<'a>> {
    let Some(list) = annotation.child_by_field_name("arguments") else {
        return Vec::new();
    };
    let mut cursor = list.walk();
    list.named_children(&mut cursor).collect()
}

/// The value of one named part of an annotation, or the value written without a name.
fn argument<'a>(annotation: Node<'a>, key: &str, text: &str) -> Option<Node<'a>> {
    for argument in arguments(annotation) {
        if argument.kind() == "element_value_pair" {
            let name = argument.child_by_field_name("key")?;
            if text_of(text, name) == key {
                return argument.child_by_field_name("value");
            }
        } else if key == "value" {
            return Some(argument);
        }
    }
    None
}

/// A path of an annotation: the text the code writes, or a constant it resolved to.
#[derive(Clone, Debug, PartialEq, Eq)]
enum PathValue {
    Literal(String),
    Expression(String),
}

impl PathValue {
    fn text(&self) -> &str {
        match self {
            Self::Literal(text) | Self::Expression(text) => text,
        }
    }

    fn literal(&self) -> Option<&str> {
        match self {
            Self::Literal(text) => Some(text),
            Self::Expression(_) => None,
        }
    }
}

/// Every path of a mapping annotation, in the order the code writes them.
fn path_values(
    annotation: Node<'_>,
    text: &str,
    constants: &HashMap<String, String>,
) -> Vec<PathValue> {
    for key in ["value", "path"] {
        if let Some(value) = argument(annotation, key, text) {
            let values = values_of(value, text, constants);
            if !values.is_empty() {
                return values;
            }
        }
    }
    Vec::new()
}

fn values_of(value: Node<'_>, text: &str, constants: &HashMap<String, String>) -> Vec<PathValue> {
    if value.kind() == "element_value_array_initializer" {
        let mut cursor = value.walk();
        value
            .named_children(&mut cursor)
            .map(|child| resolve(child, text, constants))
            .collect()
    } else {
        vec![resolve(value, text, constants)]
    }
}

/// The text of a string literal without its quotes, or the expression as the code writes
/// it. A constant of the same type is replaced with its value.
fn resolve(node: Node<'_>, text: &str, constants: &HashMap<String, String>) -> PathValue {
    match node.kind() {
        "string_literal" => PathValue::Literal(literal(node, text)),
        "identifier" => {
            let name = text_of(text, node);
            constants
                .get(name)
                .cloned()
                .map(PathValue::Literal)
                .unwrap_or_else(|| PathValue::Expression(name.to_owned()))
        }
        "field_access" => {
            let raw = text_of(text, node);
            let name = raw.rsplit('.').next().unwrap_or(raw);
            constants
                .get(name)
                .cloned()
                .map(PathValue::Literal)
                .unwrap_or_else(|| PathValue::Expression(raw.to_owned()))
        }
        "parenthesized_expression" => node.named_child(0).map_or_else(
            || PathValue::Expression(text_of(text, node).to_owned()),
            |inner| resolve(inner, text, constants),
        ),
        "binary_expression" => {
            let operator = node.child_by_field_name("operator");
            let is_plus = operator.is_some_and(|operator| text_of(text, operator) == "+");
            if is_plus
                && let (Some(left), Some(right)) = (
                    node.child_by_field_name("left"),
                    node.child_by_field_name("right"),
                )
                && let (PathValue::Literal(left), PathValue::Literal(right)) = (
                    resolve(left, text, constants),
                    resolve(right, text, constants),
                )
            {
                return PathValue::Literal(format!("{left}{right}"));
            }
            PathValue::Expression(text_of(text, node).trim().to_owned())
        }
        _ => PathValue::Expression(text_of(text, node).trim().to_owned()),
    }
}

fn literal(node: Node<'_>, text: &str) -> String {
    let raw = text_of(text, node);
    let raw = raw.strip_prefix('"').unwrap_or(raw);
    let raw = raw.strip_suffix('"').unwrap_or(raw);
    raw.replace("\\\"", "\"").replace("\\\\", "\\")
}

/// The string constants of a type body, so that a mapping written with one shows its
/// value. A constant that builds on an earlier one is resolved in the same pass.
fn constants(body: Node<'_>, text: &str) -> HashMap<String, String> {
    let mut found = HashMap::new();
    let mut cursor = body.walk();
    for field in body.named_children(&mut cursor) {
        if field.kind() != "field_declaration" {
            continue;
        }
        let is_string = field
            .child_by_field_name("type")
            .is_some_and(|kind| text_of(text, kind).trim().ends_with("String"));
        if !is_string {
            continue;
        }
        let mut inner = field.walk();
        for declarator in field.named_children(&mut inner) {
            if declarator.kind() != "variable_declarator" {
                continue;
            }
            let (Some(name), Some(value)) = (
                declarator.child_by_field_name("name"),
                declarator.child_by_field_name("value"),
            ) else {
                continue;
            };
            if let PathValue::Literal(value) = resolve(value, text, &found) {
                found.insert(text_of(text, name).to_owned(), value);
            }
        }
    }
    found
}

fn mapping_methods(annotation: Node<'_>, text: &str) -> Vec<HttpMethod> {
    let Some(value) = argument(annotation, "method", text) else {
        return Vec::new();
    };
    let mut methods = Vec::new();
    collect_methods(value, text, &mut methods);
    methods
}

fn collect_methods(node: Node<'_>, text: &str, methods: &mut Vec<HttpMethod>) {
    if node.kind() == "element_value_array_initializer" {
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            collect_methods(child, text, methods);
        }
        return;
    }
    let raw = text_of(text, node);
    for word in raw.split(|character: char| !character.is_ascii_alphanumeric()) {
        if let Some(method) = HttpMethod::parse(word)
            && !methods.contains(&method)
        {
            methods.push(method);
        }
    }
}

fn parameters(method: Node<'_>, text: &str) -> Vec<EndpointParameter> {
    let Some(list) = method.child_by_field_name("parameters") else {
        return Vec::new();
    };
    let mut found = Vec::new();
    let mut cursor = list.walk();
    for parameter in list.named_children(&mut cursor) {
        if parameter.kind() != "formal_parameter" {
            continue;
        }
        let name = parameter
            .child_by_field_name("name")
            .map_or("", |name| text_of(text, name))
            .to_owned();
        let type_name = parameter
            .child_by_field_name("type")
            .map(|kind| text_of(text, kind).trim().to_owned());
        for annotation in annotations(parameter) {
            let annotation = annotation_name(annotation, text);
            let kind = match annotation {
                "PathVariable" | "PathParam" => Some(EndpointParameterKind::Path),
                "RequestParam" | "QueryParam" => Some(EndpointParameterKind::Query),
                "RequestBody" => Some(EndpointParameterKind::Body),
                _ => None,
            };
            let Some(kind) = kind else {
                continue;
            };
            let declared = annotations(parameter)
                .into_iter()
                .find(|candidate| annotation_name(*candidate, text) == annotation)
                .and_then(|candidate| {
                    argument(candidate, "value", text).or_else(|| argument(candidate, "name", text))
                })
                .map(|argument| resolve(argument, text, &HashMap::new()))
                .and_then(|value| value.literal().map(str::to_owned))
                .filter(|value| !value.is_empty());
            found.push(EndpointParameter {
                name: declared.unwrap_or_else(|| name.clone()),
                kind,
                type_name: (kind == EndpointParameterKind::Body)
                    .then(|| type_name.clone())
                    .flatten(),
            });
        }
    }
    found
}

/// The line of a byte offset in the source, from zero.
fn line_of(text: &str, offset: usize) -> u32 {
    text[..offset.min(text.len())]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count() as u32
}

fn join(prefix: &PathValue, path: &PathValue) -> String {
    let prefix = prefix.text().trim().trim_matches('/');
    let path = path.text().trim().trim_start_matches('/');
    match (prefix.is_empty(), path.is_empty()) {
        (true, true) => "/".to_owned(),
        (true, false) => format!("/{path}"),
        (false, true) => format!("/{prefix}"),
        (false, false) => format!("/{prefix}/{path}"),
    }
}

fn visit_type(
    type_node: Node<'_>,
    package: &str,
    owner: &str,
    file: &Path,
    text: &str,
    found: &mut Vec<Endpoint>,
) {
    let annotations = annotations(type_node);
    let name = type_node
        .child_by_field_name("name")
        .map_or("", |name| text_of(text, name));
    let class = if owner.is_empty() {
        if package.is_empty() {
            name.to_owned()
        } else {
            format!("{package}.{name}")
        }
    } else if package.is_empty() {
        format!("{owner}.{name}")
    } else {
        format!("{package}.{owner}.{name}")
    };
    let nested_owner = if owner.is_empty() {
        name.to_owned()
    } else {
        format!("{owner}.{name}")
    };
    let body = type_node.child_by_field_name("body");
    let constants = body.map_or_else(HashMap::new, |body| constants(body, text));

    let is_jax_rs = find_annotation(&annotations, CLASS_PATH, text).is_some();
    let is_spring = CONTROLLERS
        .iter()
        .any(|controller| find_annotation(&annotations, controller, text).is_some());

    if is_jax_rs || is_spring {
        let prefixes = if is_jax_rs {
            find_annotation(&annotations, CLASS_PATH, text)
                .map(|annotation| path_values(annotation, text, &constants))
        } else {
            find_annotation(&annotations, CLASS_MAPPING, text)
                .map(|annotation| path_values(annotation, text, &constants))
        };
        let prefixes = prefixes
            .filter(|prefixes| !prefixes.is_empty())
            .unwrap_or_else(|| vec![PathValue::Literal(String::new())]);

        if let Some(body) = body {
            let mut cursor = body.walk();
            for method in body.named_children(&mut cursor) {
                if method.kind() != "method_declaration" {
                    continue;
                }
                visit_method(
                    method, &prefixes, &class, is_jax_rs, file, text, &constants, found,
                );
            }
        }
    }

    if let Some(body) = body {
        let mut cursor = body.walk();
        for nested in body.named_children(&mut cursor) {
            if is_type(nested) {
                visit_type(nested, package, &nested_owner, file, text, found);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn visit_method(
    method: Node<'_>,
    prefixes: &[PathValue],
    class: &str,
    is_jax_rs: bool,
    file: &Path,
    text: &str,
    constants: &HashMap<String, String>,
    found: &mut Vec<Endpoint>,
) {
    let annotations = annotations(method);
    let mut mappings: Vec<(HttpMethod, Option<Node<'_>>)> = Vec::new();
    for annotation in &annotations {
        let name = annotation_name(*annotation, text);
        if is_jax_rs {
            if let Some((_, http)) = METHOD_ANNOTATIONS.iter().find(|(word, _)| *word == name) {
                mappings.push((*http, None));
            }
            continue;
        }
        if let Some((_, http)) = METHOD_MAPPINGS.iter().find(|(word, _)| *word == name) {
            let methods = mapping_methods(*annotation, text);
            if methods.is_empty() {
                mappings.push((*http, Some(*annotation)));
            } else {
                for http in methods {
                    mappings.push((http, Some(*annotation)));
                }
            }
        }
    }
    if mappings.is_empty() {
        return;
    }
    let method_paths = if is_jax_rs {
        find_annotation(&annotations, CLASS_PATH, text)
            .map(|annotation| path_values(annotation, text, constants))
            .filter(|paths| !paths.is_empty())
            .unwrap_or_else(|| vec![PathValue::Literal(String::new())])
    } else {
        Vec::new()
    };
    let Some(name) = method.child_by_field_name("name") else {
        return;
    };
    let parameters = parameters(method, text);
    for (http, annotation) in mappings {
        let paths = if is_jax_rs {
            method_paths.clone()
        } else {
            annotation
                .map(|annotation| path_values(annotation, text, constants))
                .filter(|paths| !paths.is_empty())
                .unwrap_or_else(|| vec![PathValue::Literal(String::new())])
        };
        for prefix in prefixes {
            for path in &paths {
                found.push(Endpoint {
                    method: http,
                    path: join(prefix, path),
                    class: class.to_owned(),
                    file: PathBuf::from(file),
                    line: line_of(text, name.start_byte()),
                    name_range: name.byte_range(),
                    parameters: parameters.clone(),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(source: &str) -> Vec<Endpoint> {
        endpoints(Path::new("C:/p/src/demo/OwnerController.java"), source)
    }

    fn paths(found: &[Endpoint]) -> Vec<String> {
        found
            .iter()
            .map(|endpoint| format!("{} {}", endpoint.method.as_str(), endpoint.path))
            .collect()
    }

    const SPRING: &str = r#"package demo;

import org.springframework.web.bind.annotation.*;

@RestController
@RequestMapping("/owners")
public class OwnerController {
    @GetMapping("/{ownerId}")
    public Owner show(@PathVariable("ownerId") long ownerId) { return null; }

    @GetMapping("/new")
    public String create() { return "owners"; }

    @PostMapping("/new")
    public String save(@RequestParam("name") String name) { return "owners"; }
}
"#;

    #[test]
    fn a_class_prefix_joins_the_paths_of_its_methods() {
        let found = scan(SPRING);
        assert_eq!(
            paths(&found),
            [
                "GET /owners/{ownerId}",
                "GET /owners/new",
                "POST /owners/new"
            ]
        );
        assert_eq!(found[0].class, "demo.OwnerController");
        assert_eq!(&SPRING[found[0].name_range.clone()], "show");
        let parameters: Vec<_> = found[0]
            .parameters
            .iter()
            .map(|parameter| (parameter.name.as_str(), parameter.kind))
            .collect();
        assert_eq!(parameters, [("ownerId", EndpointParameterKind::Path)]);
        assert_eq!(found[2].parameters[0].name, "name");
        assert_eq!(found[2].parameters[0].kind, EndpointParameterKind::Query);
    }

    #[test]
    fn a_mapping_may_have_several_paths() {
        let source = r#"@Controller
class Shop {
    @GetMapping({"/one", "/two"})
    String items() { return ""; }
}
"#;
        let found = scan(source);
        assert_eq!(paths(&found), ["GET /one", "GET /two"]);
    }

    #[test]
    fn request_mapping_method_names_its_method() {
        let source = r#"@RestController
@RequestMapping("/api")
class Shop {
    @RequestMapping(path = "/items", method = RequestMethod.POST)
    String save() { return ""; }
}
"#;
        let found = scan(source);
        assert_eq!(paths(&found), ["POST /api/items"]);
    }

    #[test]
    fn jax_rs_paths_of_a_class_and_a_method_join() {
        let source = r#"package demo;

@Path("/owners")
public class OwnerResource {
    @GET
    @Path("/{id}")
    public Owner find(@PathParam("id") long id) { return null; }

    @DELETE
    public void remove(@QueryParam("id") long id) {}
}
"#;
        let found = scan(source);
        assert_eq!(paths(&found), ["GET /owners/{id}", "DELETE /owners"]);
        assert_eq!(found[0].parameters[0].kind, EndpointParameterKind::Path);
        assert_eq!(found[1].parameters[0].kind, EndpointParameterKind::Query);
    }

    #[test]
    fn a_path_constant_of_the_same_class_is_used() {
        let source = r#"@RestController
@RequestMapping("/api")
class Shop {
    private static final String ITEMS = "/items";
    private static final String ONE = ITEMS + "/{id}";

    @GetMapping(ONE)
    String item() { return ""; }
}
"#;
        let found = scan(source);
        assert_eq!(paths(&found), ["GET /api/items/{id}"]);
    }

    #[test]
    fn a_controller_without_a_prefix_keeps_the_path_of_its_method() {
        let source = r#"@RestController
class Shop {
    @GetMapping("/items")
    String items() { return ""; }
}
"#;
        let found = scan(source);
        assert_eq!(paths(&found), ["GET /items"]);
    }

    #[test]
    fn a_body_names_its_type() {
        let source = r#"@RestController
class Shop {
    @PostMapping("/items")
    String save(@RequestBody Owner owner) { return ""; }
}
"#;
        let found = scan(source);
        let body = &found[0].parameters[0];
        assert_eq!(body.kind, EndpointParameterKind::Body);
        assert_eq!(body.type_name.as_deref(), Some("Owner"));
    }
}
