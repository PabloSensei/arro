//! Declarations that nothing in the project names, because a framework or a test runner
//! calls them: the test methods of JUnit and TestNG and the classes that hold one, the
//! `main` method of a program, and the beans, handlers and listeners of the usual
//! frameworks. The scan is native, so the editor reads it without a language server.

use std::ops::Range;

use tree_sitter::Node;

use crate::spring_beans::{
    FACTORY, STEREOTYPES, TYPE_DECLARATIONS, annotations, has_modifier, parse, text_of,
};

/// The annotations of a method that a test runner calls: JUnit 4 and 5 and TestNG.
const TEST_METHODS: &[&str] = &[
    "Test",
    "ParameterizedTest",
    "RepeatedTest",
    "TestFactory",
    "BeforeEach",
    "AfterEach",
    "BeforeAll",
    "AfterAll",
    "Before",
    "After",
    "BeforeClass",
    "AfterClass",
    "BeforeMethod",
    "AfterMethod",
    "BeforeSuite",
    "AfterSuite",
    "BeforeTest",
    "AfterTest",
    "BeforeGroups",
    "AfterGroups",
    "DataProvider",
];
/// The annotations of a method that a framework calls: a listener, a schedule and the
/// hooks of a life cycle. A bean is `FACTORY` and a handler ends in `MAPPING`.
const FRAMEWORK_METHODS: &[&str] = &["EventListener", "Scheduled", "PostConstruct", "PreDestroy"];
/// The annotations of a class that a framework starts, besides the stereotypes.
const FRAMEWORK_TYPES: &[&str] = &["SpringBootApplication"];
/// The suffix of the annotations of a handler, `GetMapping` and `RequestMapping`.
const MAPPING: &str = "Mapping";

/// The spans of one source where such declarations stand, from the first line of a
/// declaration (its annotations included) to the line of its name. Lines count from zero
/// and the end of a span is not in it.
pub fn implicit_use_spans(text: &str) -> Vec<Range<u32>> {
    let Some(tree) = parse(text) else {
        return Vec::new();
    };
    let mut spans = Vec::new();
    collect(tree.root_node(), text, &mut spans);
    spans
}

/// Visits every type declaration of a node, the nested ones too.
fn collect(node: Node<'_>, text: &str, spans: &mut Vec<Range<u32>>) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if TYPE_DECLARATIONS.contains(&child.kind()) {
            declare_type(child, text, spans);
        } else {
            collect(child, text, spans);
        }
    }
}

/// A class is implicit when a framework starts it, or when one of its methods is a test:
/// the class of tests needs no annotation of its own.
fn declare_type(node: Node<'_>, text: &str, spans: &mut Vec<Range<u32>>) {
    let body = node.child_by_field_name("body");
    let methods = body.map_or_else(Vec::new, methods_of);
    let framework = annotations(node, text).iter().any(|annotation| {
        let name = annotation.name.as_str();
        STEREOTYPES.contains(&name) || FRAMEWORK_TYPES.contains(&name)
    });
    let test_class = methods.iter().any(|method| is_test_method(*method, text));
    if framework || test_class {
        push_span(node, spans);
    }
    for method in methods {
        if is_implicit_method(method, text) {
            push_span(method, spans);
        }
    }
    if let Some(body) = body {
        collect(body, text, spans);
    }
}

/// The methods a body declares. The constants of an enum come before its methods, in a
/// group of their own.
fn methods_of(body: Node<'_>) -> Vec<Node<'_>> {
    let mut methods = Vec::new();
    let mut cursor = body.walk();
    for child in body.named_children(&mut cursor) {
        match child.kind() {
            "method_declaration" => methods.push(child),
            "enum_body_declarations" => methods.extend(methods_of(child)),
            _ => {}
        }
    }
    methods
}

fn is_test_method(method: Node<'_>, text: &str) -> bool {
    annotations(method, text)
        .iter()
        .any(|annotation| TEST_METHODS.contains(&annotation.name.as_str()))
}

fn is_implicit_method(method: Node<'_>, text: &str) -> bool {
    let framework = annotations(method, text).iter().any(|annotation| {
        let name = annotation.name.as_str();
        FRAMEWORK_METHODS.contains(&name) || name == FACTORY || name.ends_with(MAPPING)
    });
    framework || is_test_method(method, text) || is_main(method, text)
}

/// `static void main(String[])`, the entry of a program. The arguments come as `String[]`
/// or as `String...`.
fn is_main(method: Node<'_>, text: &str) -> bool {
    let Some(name) = method.child_by_field_name("name") else {
        return false;
    };
    if text_of(text, name) != "main" || !has_modifier(method, text, "static") {
        return false;
    }
    if method
        .child_by_field_name("type")
        .map(|kind| text_of(text, kind))
        != Some("void")
    {
        return false;
    }
    let Some(parameters) = method.child_by_field_name("parameters") else {
        return false;
    };
    let mut cursor = parameters.walk();
    let list: Vec<Node<'_>> = parameters
        .named_children(&mut cursor)
        .filter(|child| matches!(child.kind(), "formal_parameter" | "spread_parameter"))
        .collect();
    match list.as_slice() {
        [only] => takes_arguments(*only, text),
        _ => false,
    }
}

/// Whether a parameter is `String[]` or `String...`, with or without `final` and the
/// package of the class.
fn takes_arguments(parameter: Node<'_>, text: &str) -> bool {
    let words: String = text_of(text, parameter).split_whitespace().collect();
    let words = words.trim_start_matches("final");
    let words = words.strip_prefix("java.lang.").unwrap_or(words);
    if parameter.kind() == "spread_parameter" {
        words.starts_with("String...")
    } else {
        words.starts_with("String[]")
    }
}

/// Adds the span of a declaration: its first line through the line of its name.
fn push_span(declaration: Node<'_>, spans: &mut Vec<Range<u32>>) {
    let Some(name) = declaration.child_by_field_name("name") else {
        return;
    };
    let start = declaration.start_position().row as u32;
    let end = name.start_position().row as u32 + 1;
    spans.push(start..end);
}

#[cfg(test)]
mod tests {
    use std::ops::Range;

    use super::implicit_use_spans;

    fn covers(spans: &[Range<u32>], line: u32) -> bool {
        spans.iter().any(|span| span.contains(&line))
    }

    #[test]
    fn test_methods_and_main_and_handlers_are_implicit_and_a_plain_method_is_not() {
        let text = "\
import org.junit.jupiter.api.Test;
class OwnerTests {
    @Test
    void addsPet() {}

    void plain() {}

    public static void main(String[] args) {}

    @GetMapping(\"/owners\")
    public String list() { return \"\"; }
}
";
        let spans = implicit_use_spans(text);
        assert!(
            covers(&spans, 1),
            "a class with a test is implicit: {spans:?}"
        );
        assert!(covers(&spans, 3), "a test method is implicit: {spans:?}");
        assert!(covers(&spans, 7), "main is implicit: {spans:?}");
        assert!(covers(&spans, 10), "a handler is implicit: {spans:?}");
        assert!(
            !covers(&spans, 5),
            "a plain method is not implicit: {spans:?}"
        );
        assert!(!covers(&spans, 0), "an import is not implicit: {spans:?}");
    }

    #[test]
    fn a_framework_class_and_its_bean_are_implicit_and_a_plain_class_is_not() {
        let text = "\
@Service
class Orders {
    void place() {}
}
@Configuration
class Setup {
    @Bean
    Clock clock() { return null; }
}
class Plain {
    void run() {}
}
";
        let spans = implicit_use_spans(text);
        assert!(covers(&spans, 1), "a service is implicit: {spans:?}");
        assert!(
            !covers(&spans, 2),
            "a plain method of a service is not: {spans:?}"
        );
        assert!(covers(&spans, 5), "a configuration is implicit: {spans:?}");
        assert!(covers(&spans, 7), "a bean method is implicit: {spans:?}");
        assert!(
            !covers(&spans, 9),
            "a plain class is not implicit: {spans:?}"
        );
        assert!(
            !covers(&spans, 10),
            "a plain method is not implicit: {spans:?}"
        );
    }
}
