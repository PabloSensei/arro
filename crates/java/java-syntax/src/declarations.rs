//! The signatures and the visibility of the declarations of a Java file, read from the tree
//! of the grammar. The outline shows the signature after the name and the visibility as a
//! mark beside the icon of the kind.

use lang_api::{Declared, GraphVisibility, Symbol};
use tree_sitter::{Node, Tree};

use crate::diagram::{DiagramVisibility, modifiers_of};

/// The declaration of `symbol` with its signature and visibility. `tree` is the tree of
/// `text` that the symbol came from.
pub(crate) fn declared(tree: &Tree, text: &str, symbol: Symbol) -> Declared {
    let (signature, visibility) = match declaration_of(tree, &symbol) {
        Some((declaration, declarator)) => details(declaration, declarator, text),
        None => (None, None),
    };
    Declared {
        symbol,
        signature,
        visibility,
    }
}

/// The node that declares the name of `symbol`, and the declarator of a field: a field
/// keeps its name in a declarator, and the declaration is one node above it.
fn declaration_of<'tree>(
    tree: &'tree Tree,
    symbol: &Symbol,
) -> Option<(Node<'tree>, Option<Node<'tree>>)> {
    let name = tree
        .root_node()
        .descendant_for_byte_range(symbol.name_range.start, symbol.name_range.end)?;
    let parent = name.parent()?;
    if parent.kind() == "variable_declarator" {
        return Some((parent.parent()?, Some(parent)));
    }
    Some((parent, None))
}

fn details(
    declaration: Node<'_>,
    declarator: Option<Node<'_>>,
    text: &str,
) -> (Option<String>, Option<GraphVisibility>) {
    match declaration.kind() {
        "method_declaration" => (
            method_signature(declaration, text),
            Some(visibility_of(declaration, text)),
        ),
        "constructor_declaration" => (
            constructor_signature(declaration, text),
            Some(visibility_of(declaration, text)),
        ),
        "field_declaration" => (
            declarator.and_then(|declarator| field_signature(declaration, declarator, text)),
            Some(visibility_of(declaration, text)),
        ),
        // The constants of an enum are public, whatever the modifiers say.
        "enum_constant" => (None, Some(GraphVisibility::Public)),
        "class_declaration"
        | "interface_declaration"
        | "enum_declaration"
        | "record_declaration"
        | "annotation_type_declaration"
        | "compact_constructor_declaration" => (None, Some(visibility_of(declaration, text))),
        _ => (None, None),
    }
}

/// `getPet(String, boolean): Pet`, the result as the source spells it.
fn method_signature(method: Node<'_>, text: &str) -> Option<String> {
    let name = node_text(method.child_by_field_name("name")?, text);
    let parameters = parameter_types(method.child_by_field_name("parameters")?, text);
    let result = node_text(method.child_by_field_name("type")?, text);
    Some(format!("{name}({}): {result}", parameters.join(", ")))
}

/// `Owner(String)`: a constructor has no result.
fn constructor_signature(constructor: Node<'_>, text: &str) -> Option<String> {
    let name = node_text(constructor.child_by_field_name("name")?, text);
    let parameters = parameter_types(constructor.child_by_field_name("parameters")?, text);
    Some(format!("{name}({})", parameters.join(", ")))
}

/// `count: int`, with the dimensions that stand after the name.
fn field_signature(field: Node<'_>, declarator: Node<'_>, text: &str) -> Option<String> {
    let name = node_text(declarator.child_by_field_name("name")?, text);
    let mut spelled = node_text(field.child_by_field_name("type")?, text).to_owned();
    if let Some(dimensions) = declarator.child_by_field_name("dimensions") {
        spelled.push_str(&compact(node_text(dimensions, text)));
    }
    Some(format!("{name}: {spelled}"))
}

/// The types of the parameters in order, as the source spells them. Annotations and names
/// are left out; a variable argument ends in `...`.
fn parameter_types(parameters: Node<'_>, text: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut cursor = parameters.walk();
    for parameter in parameters.named_children(&mut cursor) {
        // A receiver parameter has no type field and is not a parameter of the signature.
        let Some(kind) = parameter.child_by_field_name("type") else {
            continue;
        };
        let mut spelled = node_text(kind, text).to_owned();
        if let Some(dimensions) = parameter.child_by_field_name("dimensions") {
            spelled.push_str(&compact(node_text(dimensions, text)));
        }
        match parameter.kind() {
            "formal_parameter" => found.push(spelled),
            "spread_parameter" => found.push(format!("{spelled}...")),
            _ => {}
        }
    }
    found
}

/// The visibility of a declaration: its own modifier, or the default of its place. A member
/// of an interface is public without a modifier.
fn visibility_of(declaration: Node<'_>, text: &str) -> GraphVisibility {
    let modifiers = modifiers_of(declaration, text);
    let in_interface = declaration
        .parent()
        .is_some_and(|parent| matches!(parent.kind(), "interface_body" | "annotation_type_body"));
    match modifiers.visibility {
        DiagramVisibility::Public => GraphVisibility::Public,
        DiagramVisibility::Protected => GraphVisibility::Protected,
        DiagramVisibility::Private => GraphVisibility::Private,
        DiagramVisibility::Package if in_interface => GraphVisibility::Public,
        DiagramVisibility::Package => GraphVisibility::Package,
    }
}

fn node_text<'a>(node: Node<'_>, text: &'a str) -> &'a str {
    text.get(node.byte_range()).unwrap_or_default()
}

/// The text without its white space, so that `[ ]` reads `[]`.
fn compact(spelled: &str) -> String {
    spelled
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use lang_api::{GraphVisibility, SymbolProvider};

    use crate::java_symbols;

    const OWNER: &str = "package demo;\n\
        \n\
        public class Owner {\n\
        \x20   private String address;\n\
        \x20   private Pet pet, spare;\n\
        \x20   int first, second;\n\
        \n\
        \x20   public Owner(String name) {}\n\
        \n\
        \x20   public String getAddress() { return address; }\n\
        \x20   public Pet getPet(String name) { return pet; }\n\
        \x20   public Pet getPet(Integer id) { return pet; }\n\
        \x20   public Pet getPet(String name, boolean active) { return pet; }\n\
        \x20   protected void keep(String... names) {}\n\
        \x20   void noop() {}\n\
        \n\
        \x20   interface Shape {\n\
        \x20       double area();\n\
        \x20   }\n\
        }\n";

    type Row = (String, Option<String>, Option<GraphVisibility>);

    fn row(name: &str, signature: Option<&str>, visibility: Option<GraphVisibility>) -> Row {
        (name.to_owned(), signature.map(str::to_owned), visibility)
    }

    #[test]
    fn a_declaration_has_its_signature_and_visibility() {
        use GraphVisibility::{Package, Private, Protected, Public};
        let found: Vec<Row> = java_symbols()
            .file_declarations(Path::new("Owner.java"), OWNER)
            .into_iter()
            .map(|found| (found.symbol.name, found.signature, found.visibility))
            .collect();
        assert_eq!(
            found,
            vec![
                row("demo", None, None),
                row("Owner", None, Some(Public)),
                row("address", Some("address: String"), Some(Private)),
                row("pet", Some("pet: Pet"), Some(Private)),
                row("spare", Some("spare: Pet"), Some(Private)),
                row("first", Some("first: int"), Some(Package)),
                row("second", Some("second: int"), Some(Package)),
                row("Owner", Some("Owner(String)"), Some(Public)),
                row("getAddress", Some("getAddress(): String"), Some(Public)),
                row("getPet", Some("getPet(String): Pet"), Some(Public)),
                row("getPet", Some("getPet(Integer): Pet"), Some(Public)),
                row("getPet", Some("getPet(String, boolean): Pet"), Some(Public)),
                row("keep", Some("keep(String...): void"), Some(Protected)),
                row("noop", Some("noop(): void"), Some(Package)),
                row("Shape", None, Some(Package)),
                row("area", Some("area(): double"), Some(Public)),
            ]
        );
    }

    #[test]
    fn the_declarations_come_in_the_order_of_the_symbols() {
        let symbols: Vec<String> = java_symbols()
            .file_symbols(Path::new("Owner.java"), OWNER)
            .into_iter()
            .map(|symbol| symbol.name)
            .collect();
        let declared: Vec<String> = java_symbols()
            .file_declarations(Path::new("Owner.java"), OWNER)
            .into_iter()
            .map(|found| found.symbol.name)
            .collect();
        assert_eq!(symbols, declared);
    }
}
