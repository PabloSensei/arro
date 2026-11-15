//! The configuration key a Java string literal names: `@ConfigProperty(name = "...")` of
//! MicroProfile, `@Property(name = "...")` of Micronaut and a `${...}` placeholder of
//! `@Value`. The scan is native; it gives the key and the byte range inside the source,
//! so completion and navigation work without a server.

use std::ops::Range;

use tree_sitter::{Node, Parser};

/// The key a string literal of an annotation names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigKey {
    /// The simple name of the annotation, `ConfigProperty`, `Property` or `Value`.
    pub annotation: String,
    /// The key as the source spells it, without a default value.
    pub key: String,
    /// The byte range of the key inside the source text.
    pub range: Range<usize>,
}

fn parse(text: &str) -> Option<tree_sitter::Tree> {
    let mut parser = Parser::new();
    parser.set_language(&super::config().grammar).ok()?;
    parser.parse(text, None)
}

/// The configuration key at a byte offset, when the offset sits inside the key of an
/// annotation that names one.
pub fn config_key_at(text: &str, offset: usize) -> Option<ConfigKey> {
    let tree = parse(text)?;
    let literal = literal_at(tree.root_node(), offset)?;
    let annotation = annotation_of(literal)?;
    let name_node = annotation.child_by_field_name("name")?;
    let name = &text[name_node.byte_range()];
    let annotation = name.rsplit('.').next().unwrap_or(name).to_owned();
    let content = string_range(literal, text)?;
    match annotation.as_str() {
        "Value" => value_key(text, content, offset).map(|(key, range)| ConfigKey {
            annotation,
            key,
            range,
        }),
        "ConfigProperty" | "Property" => {
            if !names_key(literal, text) {
                return None;
            }
            Some(ConfigKey {
                annotation,
                key: text[content.clone()].to_owned(),
                range: content,
            })
        }
        _ => None,
    }
}

/// The deepest string literal that holds the offset.
fn literal_at<'a>(node: Node<'a>, offset: usize) -> Option<Node<'a>> {
    let mut found = None;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.start_byte() > offset || child.end_byte() < offset {
            continue;
        }
        if let Some(inner) = literal_at(child, offset) {
            found = Some(inner);
        } else if child.kind() == "string_literal" {
            found = Some(child);
        }
    }
    if found.is_none() && node.kind() == "string_literal" {
        return Some(node);
    }
    found
}

fn annotation_of(node: Node<'_>) -> Option<Node<'_>> {
    let mut current = node.parent();
    while let Some(parent) = current {
        if matches!(parent.kind(), "annotation" | "marker_annotation") {
            return Some(parent);
        }
        current = parent.parent();
    }
    None
}

/// The width of the quotes of a string literal: three for a text block.
fn quote_width(node: Node<'_>, text: &str) -> usize {
    if text[node.byte_range()].starts_with("\"\"\"") {
        3
    } else {
        1
    }
}

fn string_range(node: Node<'_>, text: &str) -> Option<Range<usize>> {
    let start = node.start_byte();
    let end = node.end_byte();
    let width = quote_width(node, text);
    (end >= start + 2 * width).then_some(start + width..end - width)
}

/// Whether the literal is the `name` argument of its annotation, or its only argument.
fn names_key(literal: Node<'_>, text: &str) -> bool {
    let Some(parent) = literal.parent() else {
        return true;
    };
    if parent.kind() != "element_value_pair" {
        return true;
    }
    parent
        .child_by_field_name("key")
        .is_some_and(|key| text[key.byte_range()].trim() == "name")
}

/// The key of a `${...}` placeholder that holds the offset, with its range.
fn value_key(text: &str, content: Range<usize>, offset: usize) -> Option<(String, Range<usize>)> {
    let inner = &text[content.clone()];
    let mut at = 0;
    while let Some(start) = inner[at..].find("${") {
        let start = at + start;
        let after = &inner[start + 2..];
        let Some(end) = after.find('}') else {
            break;
        };
        let body = &inner[start + 2..start + 2 + end];
        let key = body.split(':').next().unwrap_or(body);
        let key_start = content.start + start + 2;
        let key_end = key_start + key.len();
        if offset >= key_start && offset <= key_end {
            return Some((key.to_owned(), key_start..key_end));
        }
        at = start + 2 + end + 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(source: &str, at: &str) -> Option<ConfigKey> {
        let offset = source.find(at).expect("the place of the test") + at.len();
        config_key_at(source, offset)
    }

    #[test]
    fn a_config_property_names_its_key() {
        let source = r#"class Shop {
    @ConfigProperty(name = "shop.name")
    String name;
}
"#;
        let found = key(source, "shop.na").expect("the key of the annotation");
        assert_eq!(found.annotation, "ConfigProperty");
        assert_eq!(found.key, "shop.name");
        assert_eq!(&source[found.range], "shop.name");
    }

    #[test]
    fn a_micronaut_property_names_its_key() {
        let source = r#"class Shop {
    @Property(name = "micronaut.server.port")
    int port;
}
"#;
        let found = key(source, "micronaut.server").expect("the key of the annotation");
        assert_eq!(found.annotation, "Property");
        assert_eq!(found.key, "micronaut.server.port");
    }

    #[test]
    fn a_value_placeholder_gives_the_key_without_its_default() {
        let source = r#"class Shop {
    @Value("${shop.name:demo}")
    String name;
}
"#;
        let found = key(source, "shop.n").expect("the key of the placeholder");
        assert_eq!(found.annotation, "Value");
        assert_eq!(found.key, "shop.name");
        assert_eq!(&source[found.range], "shop.name");
    }

    #[test]
    fn a_second_placeholder_of_the_same_value_is_found() {
        let source = r#"class Shop {
    @Value("${one:1}-${two:2}")
    String name;
}
"#;
        let found = key(source, "tw").expect("the second key");
        assert_eq!(found.key, "two");
    }

    #[test]
    fn a_literal_without_a_key_gives_nothing() {
        assert!(key(r#"class Shop { @Value("plain") String name; }"#, "pla").is_none());
        assert!(
            key(
                r#"class Shop { @ConfigProperty(defaultValue = "x") String name; }"#,
                "x"
            )
            .is_none()
        );
    }
}
