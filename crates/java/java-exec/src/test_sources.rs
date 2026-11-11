use std::fs;
use std::path::{Path, PathBuf};

use tree_sitter::{Node, Parser};

use crate::mains::{collect_sources, is_type, named_children, new_parser};

/// Annotations of JUnit 5 and later that make a method a test. `Test` is also the one of
/// JUnit 4 and TestNG, matched by its simple name like the rest.
const TEST_ANNOTATIONS: &[&str] = &[
    "Test",
    "ParameterizedTest",
    "RepeatedTest",
    "TestFactory",
    "TestTemplate",
];
const NESTED: &str = "Nested";

/// A class with tests, for the run marks in the gutter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestClass {
    /// The binary name, what the build tool filters by: `demo.OwnerTests$Nested`.
    pub class: String,
    pub file: PathBuf,
    /// One-based, the line of the class name.
    pub line: u32,
    pub methods: Vec<TestMethod>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestMethod {
    pub name: String,
    pub line: u32,
}

/// Every class under `roots` with a test method, in name order.
pub fn find_tests(roots: &[PathBuf]) -> Vec<TestClass> {
    let mut files = Vec::new();
    for root in roots {
        collect_sources(root, &mut files);
    }
    let mut parser = new_parser();
    let mut found = Vec::new();
    for file in files {
        let Ok(bytes) = fs::read(&file) else { continue };
        // No annotation with "Test" in it, no test.
        if bytes.windows(4).any(|window| window == b"Test") {
            found.extend(tests_of(&mut parser, &bytes, &file));
        }
    }
    found.sort_by(|left, right| left.class.cmp(&right.class));
    found
}

/// The same for one source held in memory, such as an open editor.
pub fn parse_tests(source: &str, file: &Path) -> Vec<TestClass> {
    tests_of(&mut new_parser(), source.as_bytes(), file)
}

fn tests_of(parser: &mut Parser, source: &[u8], file: &Path) -> Vec<TestClass> {
    let Some(tree) = parser.parse(source, None) else {
        return Vec::new();
    };
    let walker = Walker { source, file };
    let root = tree.root_node();
    let package = walker.package_of(root);
    let mut found = Vec::new();
    for child in named_children(root).filter(|child| child.kind() == "class_declaration") {
        walker.visit_class(child, &package, &mut found);
    }
    found.sort_by(|left, right| left.class.cmp(&right.class));
    found
}

struct Walker<'a> {
    source: &'a [u8],
    file: &'a Path,
}

impl Walker<'_> {
    fn text(&self, node: Node) -> &str {
        node.utf8_text(self.source).unwrap_or("")
    }

    fn package_of(&self, root: Node) -> String {
        let declaration = named_children(root).find(|child| child.kind() == "package_declaration");
        let name = declaration.and_then(|declaration| {
            named_children(declaration)
                .find(|child| matches!(child.kind(), "identifier" | "scoped_identifier"))
        });
        name.map_or_else(String::new, |name| format!("{}.", self.text(name)))
    }

    /// Simple names of the annotations and the plain modifiers of a declaration.
    fn modifiers(&self, declaration: Node) -> (Vec<String>, Vec<String>) {
        let mut annotations = Vec::new();
        let mut words = Vec::new();
        let modifiers = named_children(declaration).find(|child| child.kind() == "modifiers");
        let Some(modifiers) = modifiers else {
            return (annotations, words);
        };
        for index in 0..modifiers.child_count() {
            let Some(child) = modifiers.child(index) else {
                continue;
            };
            match child.kind() {
                "marker_annotation" | "annotation" => {
                    if let Some(name) = child.child_by_field_name("name") {
                        let text = self.text(name);
                        annotations.push(text.rsplit('.').next().unwrap_or(text).to_owned());
                    }
                }
                _ => words.push(self.text(child).to_owned()),
            }
        }
        (annotations, words)
    }

    fn visit_class(&self, node: Node, prefix: &str, found: &mut Vec<TestClass>) {
        let (Some(name), Some(body)) = (
            node.child_by_field_name("name"),
            node.child_by_field_name("body"),
        ) else {
            return;
        };
        let class = format!("{prefix}{}", self.text(name));
        let (_, words) = self.modifiers(node);
        let mut methods = Vec::new();
        for member in named_children(body) {
            if member.kind() == "method_declaration"
                && let Some(method) = self.test_method(member)
            {
                methods.push(method);
            }
        }
        // An abstract class runs only through its subclasses.
        if !methods.is_empty() && !words.iter().any(|word| word == "abstract") {
            found.push(TestClass {
                class: class.clone(),
                file: self.file.to_owned(),
                line: line_of(name),
                methods,
            });
        }
        let nested_prefix = format!("{class}$");
        for member in named_children(body).filter(|member| is_type(*member)) {
            let (annotations, words) = self.modifiers(member);
            // JUnit runs an inner class only when it is marked nested, a static one as a class
            // of its own.
            let nested = annotations.iter().any(|annotation| annotation == NESTED);
            let is_static = words.iter().any(|word| word == "static");
            if member.kind() == "class_declaration" && (nested || is_static) {
                self.visit_class(member, &nested_prefix, found);
            }
        }
    }

    fn test_method(&self, method: Node) -> Option<TestMethod> {
        let (annotations, _) = self.modifiers(method);
        let is_test = annotations
            .iter()
            .any(|annotation| TEST_ANNOTATIONS.contains(&annotation.as_str()));
        let name = method.child_by_field_name("name")?;
        is_test.then(|| TestMethod {
            name: self.text(name).to_owned(),
            line: line_of(name),
        })
    }
}

fn line_of(node: Node) -> u32 {
    node.start_position().row as u32 + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = r#"package demo;

import org.junit.jupiter.api.*;
import org.junit.jupiter.params.ParameterizedTest;

class OwnerTests {
    @Test
    void hasName() {}

    @ParameterizedTest
    @ValueSource(strings = {"a"})
    void takesValues(String value) {}

    void helper() {}

    @Nested
    class WithPets {
        @org.junit.jupiter.api.RepeatedTest(3)
        void countsPets() {}
    }

    static class Static {
        @Test
        void standsAlone() {}
    }

    class Inner {
        @Test
        void neverRuns() {}
    }
}

abstract class BaseTests {
    @Test
    void inherited() {}
}
"#;

    #[test]
    fn test_methods_and_nested_classes_are_found() {
        let found = parse_tests(SOURCE, Path::new("OwnerTests.java"));

        let classes: Vec<_> = found.iter().map(|class| class.class.as_str()).collect();
        assert_eq!(
            classes,
            [
                "demo.OwnerTests",
                "demo.OwnerTests$Static",
                "demo.OwnerTests$WithPets"
            ]
        );
        let methods: Vec<_> = found[0]
            .methods
            .iter()
            .map(|method| method.name.as_str())
            .collect();
        assert_eq!(methods, ["hasName", "takesValues"]);
        assert_eq!(found[0].line, 6);
        assert_eq!(found[0].methods[0].line, 8);
        assert_eq!(found[2].methods[0].name, "countsPets");
    }
}
