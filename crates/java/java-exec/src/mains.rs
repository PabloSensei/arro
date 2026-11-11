use std::fs;
use std::path::{Path, PathBuf};

use tree_sitter::{Node, Parser};

/// JEP 512: `main` without `static`, `public` or arguments, and classes without a
/// declaration. Final in 25, before that a preview feature.
const FIRST_WITH_SIMPLE_MAIN: u32 = 25;
const SPRING_BOOT_ANNOTATION: &str = "SpringBootApplication";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntryPoint {
    /// The binary name, what `java` takes: `demo.Outer$Inner`.
    pub class: String,
    pub file: PathBuf,
    pub spring_boot: bool,
    /// The line of the `main` method, from one.
    pub line: u32,
    /// The line of the type that holds `main`, from one. `None` for a file without a
    /// type declaration, where the method itself is the program.
    pub type_line: Option<u32>,
}

/// Every class under `roots` that `java` of this version can start.
pub fn find_entry_points(roots: &[PathBuf], jdk_feature: u32) -> Vec<EntryPoint> {
    let mut files = Vec::new();
    for root in roots {
        collect_sources(root, &mut files);
    }
    let mut parser = new_parser();
    let mut found = Vec::new();
    for file in files {
        let Ok(bytes) = fs::read(&file) else { continue };
        // Parsing is the expensive part, and very few files mention the word at all.
        if bytes.windows(4).any(|window| window == b"main") {
            found.extend(entry_points_of(&mut parser, &bytes, &file, jdk_feature));
        }
    }
    found.sort_by(|left, right| left.class.cmp(&right.class));
    found
}

/// The same for one source held in memory.
pub fn parse_entry_points(source: &str, file: &Path, jdk_feature: u32) -> Vec<EntryPoint> {
    entry_points_of(&mut new_parser(), source.as_bytes(), file, jdk_feature)
}

pub(crate) fn new_parser() -> Parser {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_java_orchard::LANGUAGE.into())
        .expect("the grammar and the runtime come as a pinned pair");
    parser
}

pub(crate) fn collect_sources(directory: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => collect_sources(&path, files),
            Ok(kind) if kind.is_file() && path.extension().is_some_and(|ext| ext == "java") => {
                files.push(path);
            }
            _ => {}
        }
    }
}

fn entry_points_of(
    parser: &mut Parser,
    source: &[u8],
    file: &Path,
    jdk_feature: u32,
) -> Vec<EntryPoint> {
    let Some(tree) = parser.parse(source, None) else {
        return Vec::new();
    };
    let walker = Walker {
        source,
        file,
        simple_main: jdk_feature >= FIRST_WITH_SIMPLE_MAIN,
    };
    let root = tree.root_node();
    let mut found = Vec::new();
    let package = walker.package_of(root);
    for child in named_children(root) {
        if is_type(child) {
            walker.visit_type(child, &package, &mut found);
        }
    }
    if walker.simple_main
        && let Some(main) = walker.implicit_main(root)
    {
        let stem = file
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned());
        found.push(EntryPoint {
            class: stem.unwrap_or_default(),
            file: file.to_owned(),
            spring_boot: false,
            line: walker.line(main),
            type_line: None,
        });
    }
    found
}

pub(crate) fn named_children(node: Node) -> impl Iterator<Item = Node> {
    (0..node.named_child_count()).filter_map(move |index| node.named_child(index as u32))
}

pub(crate) fn is_type(node: Node) -> bool {
    matches!(
        node.kind(),
        "class_declaration" | "interface_declaration" | "enum_declaration" | "record_declaration"
    )
}

struct Walker<'a> {
    source: &'a [u8],
    file: &'a Path,
    simple_main: bool,
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

    fn modifiers(&self, declaration: Node) -> Vec<String> {
        let modifiers = named_children(declaration).find(|child| child.kind() == "modifiers");
        let Some(modifiers) = modifiers else {
            return Vec::new();
        };
        let mut words = Vec::new();
        for index in 0..modifiers.child_count() {
            let Some(child) = modifiers.child(index) else {
                continue;
            };
            match child.kind() {
                "marker_annotation" | "annotation" => {
                    if let Some(name) = child.child_by_field_name("name") {
                        let text = self.text(name);
                        words.push(format!("@{}", text.rsplit('.').next().unwrap_or(text)));
                    }
                }
                _ => words.push(self.text(child).to_owned()),
            }
        }
        words
    }

    fn visit_type(&self, node: Node, prefix: &str, found: &mut Vec<EntryPoint>) {
        let Some(name) = node.child_by_field_name("name") else {
            return;
        };
        let Some(body) = node.child_by_field_name("body") else {
            return;
        };
        let class = format!("{prefix}{}", self.text(name));
        let in_interface = node.kind() == "interface_declaration";
        let members = || {
            // An enum keeps its methods one level further down.
            named_children(body).flat_map(|member| {
                if member.kind() == "enum_body_declarations" {
                    named_children(member).collect::<Vec<_>>()
                } else {
                    vec![member]
                }
            })
        };
        if members().any(|member| self.is_main(member, in_interface)) {
            let modifiers = self.modifiers(node);
            let annotation = format!("@{SPRING_BOOT_ANNOTATION}");
            let main = members()
                .find(|member| self.is_main(*member, in_interface))
                .expect("the check above found the method");
            found.push(EntryPoint {
                class: class.clone(),
                file: self.file.to_owned(),
                spring_boot: modifiers.contains(&annotation),
                line: self.line(main),
                type_line: Some(self.line(name)),
            });
        }
        let nested_prefix = format!("{class}$");
        for member in members().filter(|member| is_type(*member)) {
            // An inner class needs an instance to exist, the launcher has none.
            let is_static = member.kind() != "class_declaration"
                || in_interface
                || self.modifiers(member).iter().any(|word| word == "static");
            if is_static {
                self.visit_type(member, &nested_prefix, found);
            }
        }
    }

    fn is_main(&self, member: Node, in_interface: bool) -> bool {
        if member.kind() != "method_declaration" || !self.is_void_main(member) {
            return false;
        }
        let modifiers = self.modifiers(member);
        let has = |word: &str| modifiers.iter().any(|modifier| modifier == word);
        let parameters = self.parameter_count(member);
        let takes_arguments = parameters == Some(1) && self.takes_strings(member);
        if self.simple_main {
            return !has("private") && (parameters == Some(0) || takes_arguments);
        }
        let public = has("public") || in_interface;
        public && has("static") && takes_arguments
    }

    fn is_void_main(&self, method: Node) -> bool {
        let name = method.child_by_field_name("name");
        let returns = method.child_by_field_name("type");
        name.is_some_and(|name| self.text(name) == "main")
            && returns.is_some_and(|kind| kind.kind() == "void_type")
    }

    fn parameter_count(&self, method: Node) -> Option<usize> {
        let parameters = method.child_by_field_name("parameters")?;
        Some(
            named_children(parameters)
                .filter(|child| is_parameter(*child))
                .count(),
        )
    }

    fn takes_strings(&self, method: Node) -> bool {
        let Some(parameters) = method.child_by_field_name("parameters") else {
            return false;
        };
        let parameter = named_children(parameters).find(|child| is_parameter(*child));
        parameter.is_some_and(|parameter| {
            let text = self.text(parameter);
            let spaced = text.split_whitespace().collect::<Vec<_>>().join(" ");
            let plain = spaced.trim_start_matches("final ");
            let string = plain.strip_prefix("java.lang.").unwrap_or(plain);
            let Some(rest) = string.strip_prefix("String") else {
                return false;
            };
            rest.contains("[]") || rest.contains("...")
        })
    }

    /// Methods right in the file are the members of a class the file itself declares.
    fn implicit_main<'a>(&self, root: Node<'a>) -> Option<Node<'a>> {
        named_children(root).find(|child| self.is_main(*child, false))
    }

    /// The line of a node, from one.
    fn line(&self, node: Node) -> u32 {
        node.start_position().row as u32 + 1
    }
}

fn is_parameter(node: Node) -> bool {
    matches!(node.kind(), "formal_parameter" | "spread_parameter")
}

#[cfg(test)]
mod tests {
    use super::*;

    const OLD: u32 = 17;
    const NEW: u32 = 25;

    fn classes(source: &str, feature: u32) -> Vec<String> {
        let found = parse_entry_points(source, Path::new("Main.java"), feature);
        found.into_iter().map(|entry| entry.class).collect()
    }

    #[test]
    fn the_classic_form_is_found_with_its_package() {
        let source = "package demo.app;
            public class Main { public static void main(String[] args) {} }";

        assert_eq!(classes(source, OLD), ["demo.app.Main"]);
    }

    #[test]
    fn every_way_to_write_the_parameter_counts() {
        for parameter in [
            "String[] args",
            "String... args",
            "String args[]",
            "final String[] args",
            "java.lang.String[] args",
        ] {
            let source = format!("class A {{ public static void main({parameter}) {{}} }}");

            assert_eq!(classes(&source, OLD), ["A"], "{parameter}");
        }
    }

    #[test]
    fn what_only_looks_like_main_is_not_found() {
        for member in [
            "static void main(String[] a) {}",
            "public void main(String[] a) {}",
            "public static int main(String[] a) { return 0; }",
            "public static void main(int[] a) {}",
            "public static void main(String a) {}",
            "public static void main(String[] a, int b) {}",
            "public static void main() {}",
            "public static void mainly(String[] a) {}",
        ] {
            let source = format!("class A {{ {member} }}");

            assert!(classes(&source, OLD).is_empty(), "{member}");
        }
    }

    #[test]
    fn a_comment_or_a_string_is_not_a_method() {
        let source = r#"class A {
            // public static void main(String[] args) {}
            String text = "public static void main(String[] args) {}";
        }"#;

        assert!(classes(source, OLD).is_empty());
    }

    #[test]
    fn jep_512_forms_count_from_25_on() {
        for source in [
            "class A { void main() {} }",
            "class A { void main(String[] args) {} }",
            "class A { static void main() {} }",
            "class A { protected static void main(String... a) {} }",
        ] {
            assert_eq!(classes(source, NEW), ["A"], "{source}");
            assert!(classes(source, OLD).is_empty(), "{source}");
        }
    }

    #[test]
    fn a_private_main_never_counts() {
        assert!(classes("class A { private void main() {} }", NEW).is_empty());
    }

    #[test]
    fn a_file_without_a_class_is_named_after_the_file() {
        let source = "void main() { IO.println(1); }";

        assert_eq!(classes(source, NEW), ["Main"]);
        assert!(classes(source, OLD).is_empty());
    }

    #[test]
    fn other_kinds_of_types_can_hold_main() {
        let interface = "interface A { static void main(String[] a) {} }";
        let enumeration = "enum A { X; public static void main(String[] a) {} }";
        let record = "record A() { public static void main(String[] a) {} }";

        for source in [interface, enumeration, record] {
            assert_eq!(classes(source, OLD), ["A"], "{source}");
        }
    }

    #[test]
    fn static_nested_classes_are_found_and_inner_ones_are_not() {
        let source = "package p; class A {
            static class Nested { public static void main(String[] a) {} }
            class Inner { public static void main(String[] a) {} }
        }";

        assert_eq!(classes(source, OLD), ["p.A$Nested"]);
    }

    #[test]
    fn spring_boot_is_told_by_its_annotation() {
        let spring = "@org.springframework.boot.autoconfigure.SpringBootApplication
            public class A { public static void main(String[] a) {} }";
        let short = "@SpringBootApplication(proxyBeanMethods = false)
            public class A { public static void main(String[] a) {} }";
        let plain = "public class A { public static void main(String[] a) {} }";
        let spring_boot =
            |source| parse_entry_points(source, Path::new("A.java"), OLD)[0].spring_boot;

        assert!(spring_boot(spring));
        assert!(spring_boot(short));
        assert!(!spring_boot(plain));
    }

    #[test]
    fn sources_are_found_on_disk() {
        let root = std::env::temp_dir().join(format!("java-exec-mains-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("demo")).unwrap();
        let main = "package demo; public class B { public static void main(String[] a) {} }";
        fs::write(root.join("demo/B.java"), main).unwrap();
        fs::write(root.join("demo/Plain.java"), "package demo; class Plain {}").unwrap();
        fs::write(root.join("demo/notes.txt"), "main").unwrap();

        let found = find_entry_points(&[root.clone(), root.join("missing")], OLD);

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].class, "demo.B");
        assert_eq!(found[0].file, root.join("demo").join("B.java"));
        let _ = fs::remove_dir_all(root);
    }
}
