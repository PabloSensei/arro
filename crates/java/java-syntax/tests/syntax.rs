//! The queries against the grammar, through the same thread the editor uses.

use std::sync::mpsc;
use std::time::Duration;

use syntax::{
    DocumentId, HighlightKind, SyntaxService, SyntaxSnapshot, enclosing_range, indent_after,
};
use text_core::{Buffer, Change, Edit, EditKind, EditSource, FileFormat, RopeStore, TextStore};

const SAMPLE: &str = r#"package com.example;

import java.util.List;
import java.util.Map;

/**
 * Orders.
 */
@Service
public class OrderService {
    private static final int LIMIT = 10;
    private final List<String> names = List.of("a");

    public int size() {
        // count
        return this.names.size() + 1;
    }
}
"#;

fn parse(text: &str) -> (SyntaxService, mpsc::Receiver<SyntaxSnapshot>, RopeStore) {
    let service = SyntaxService::new();
    let (sender, receiver) = mpsc::channel();
    service.open(DocumentId(1), java_syntax::config(), move |snapshot| {
        let _ = sender.send(snapshot);
    });
    let store = RopeStore::from_text(text);
    service.update(DocumentId(1), 1, store.clone(), &Change::default());
    (service, receiver, store)
}

fn wait(receiver: &mpsc::Receiver<SyntaxSnapshot>) -> SyntaxSnapshot {
    receiver
        .recv_timeout(Duration::from_secs(10))
        .expect("the query compiles and the parse ends")
}

fn kind_of(snapshot: &SyntaxSnapshot, text: &str, word: &str) -> Option<HighlightKind> {
    let at = text.find(word).unwrap() as u32;
    snapshot
        .highlights
        .iter()
        .find(|span| span.start <= at && at < span.end)
        .map(|span| span.kind)
}

#[test]
fn highlights_cover_the_main_kinds() {
    let (_service, receiver, _) = parse(SAMPLE);
    let snapshot = wait(&receiver);
    assert!(!snapshot.tree.root_node().has_error());
    assert_eq!(
        kind_of(&snapshot, SAMPLE, "public class"),
        Some(HighlightKind::Keyword)
    );
    assert_eq!(
        kind_of(&snapshot, SAMPLE, "\"a\""),
        Some(HighlightKind::String)
    );
    assert_eq!(
        kind_of(&snapshot, SAMPLE, "10;"),
        Some(HighlightKind::Number)
    );
    assert_eq!(
        kind_of(&snapshot, SAMPLE, "// count"),
        Some(HighlightKind::Comment)
    );
    assert_eq!(
        kind_of(&snapshot, SAMPLE, "/**"),
        Some(HighlightKind::DocComment)
    );
    assert_eq!(
        kind_of(&snapshot, SAMPLE, "@Service"),
        Some(HighlightKind::Annotation)
    );
    assert_eq!(
        kind_of(&snapshot, SAMPLE, "size() {"),
        Some(HighlightKind::Function)
    );
    assert_eq!(
        kind_of(&snapshot, SAMPLE, "names ="),
        Some(HighlightKind::Field)
    );
    assert_eq!(
        kind_of(&snapshot, SAMPLE, "LIMIT"),
        Some(HighlightKind::Constant)
    );
    assert_eq!(kind_of(&snapshot, SAMPLE, "OrderService"), None);
}

#[test]
fn spans_are_sorted_and_do_not_overlap() {
    let (_service, receiver, _) = parse(SAMPLE);
    let snapshot = wait(&receiver);
    for pair in snapshot.highlights.windows(2) {
        assert!(pair[0].end <= pair[1].start);
    }
}

#[test]
fn imports_fold_together_and_collapse_on_open() {
    let (_service, receiver, _) = parse(SAMPLE);
    let snapshot = wait(&receiver);
    let imports = snapshot
        .folds
        .iter()
        .find(|fold| fold.collapsed_on_open)
        .expect("an imports fold");
    let start = SAMPLE.find("import java.util.List").unwrap() + "import ".len();
    assert_eq!(imports.range.start, start);
    assert_eq!(
        imports.range.end,
        SAMPLE.find("Map;").unwrap() + "Map;".len()
    );
    assert!(
        snapshot
            .folds
            .iter()
            .any(|fold| fold.placeholder == "/** ... */")
    );
    assert!(
        snapshot
            .folds
            .iter()
            .filter(|fold| fold.placeholder == "{...}")
            .count()
            >= 2
    );
}

#[test]
fn an_edit_reparses_from_the_old_tree() {
    let (service, receiver, _) = parse(SAMPLE);
    wait(&receiver);

    let mut buffer: Buffer<RopeStore> = Buffer::new(SAMPLE, FileFormat::default());
    let at = SAMPLE.find("// count").unwrap();
    let change = buffer.apply(
        vec![Edit::insert(at, "int x = 1;\n        ")],
        None,
        EditSource::User,
        EditKind::Command,
    );
    service.update(DocumentId(1), 2, buffer.snapshot(), &change);

    let snapshot = wait(&receiver);
    assert_eq!(snapshot.version, 2);
    assert!(!snapshot.tree.root_node().has_error());
    assert_eq!(
        kind_of(&snapshot, &buffer.text(), "int x"),
        Some(HighlightKind::Keyword)
    );
    assert_eq!(
        kind_of(&snapshot, &buffer.text(), "// count"),
        Some(HighlightKind::Comment)
    );
}

#[test]
fn a_new_line_after_an_open_brace_goes_one_level_deeper() {
    let (_service, receiver, store) = parse(SAMPLE);
    let snapshot = wait(&receiver);
    let after_brace = SAMPLE.find("size() {").unwrap() + "size() {".len();
    let inside = SAMPLE.find("return this").unwrap();
    assert_eq!(
        indent_after(
            &store,
            &snapshot.tree,
            &snapshot.indents,
            after_brace,
            "    "
        ),
        "        "
    );
    assert_eq!(
        indent_after(
            &store,
            &snapshot.tree,
            &snapshot.indents,
            inside + 7,
            "    "
        ),
        "        "
    );
}

#[test]
fn selection_grows_along_the_tree() {
    let (_service, receiver, _) = parse(SAMPLE);
    let snapshot = wait(&receiver);
    let name = SAMPLE.find("names.size").unwrap();
    let first = enclosing_range(&snapshot.tree, name..name + "names".len()).unwrap();
    assert!(SAMPLE[first.clone()].starts_with("this.names"));
    let second = enclosing_range(&snapshot.tree, first.clone()).unwrap();
    assert!(second.start <= first.start && first.end <= second.end && second != first);
}

#[test]
fn a_plain_block_comment_is_a_comment() {
    let text = "/* note */\nclass A {}\n";
    let (_service, receiver, _) = parse(text);
    let snapshot = wait(&receiver);
    assert_eq!(
        kind_of(&snapshot, text, "note"),
        Some(HighlightKind::Comment)
    );
}

/// The units of Move Statement of a text, from the caret with a neighbour `forward`.
fn move_units<'a>(
    snapshot: &syntax::Tree,
    text: &'a str,
    caret: usize,
    forward: bool,
) -> (Vec<&'a str>, usize) {
    let config = java_syntax::config();
    let query =
        tree_sitter::Query::new(&config.grammar, config.move_units).expect("the query compiles");
    let (units, index) =
        syntax::unit_siblings(snapshot, &query, text.as_bytes(), caret..caret, forward)
            .expect("the caret is in a unit with a neighbour");
    (
        units
            .into_iter()
            .map(|range| &text[range])
            .collect::<Vec<_>>(),
        index,
    )
}

#[test]
fn move_units_are_the_statements_of_a_block() {
    let text = "class A {\n    void a() {\n        one();\n        two();\n    }\n}\n";
    let (_service, receiver, _) = parse(text);
    let snapshot = wait(&receiver);
    let caret = text.find("one()").unwrap();
    let (units, index) = move_units(&snapshot.tree, text, caret, true);
    assert_eq!(units[index], "one();");
    assert_eq!(units[index + 1], "two();");
    let (units, index) = move_units(&snapshot.tree, text, text.find("two()").unwrap(), false);
    assert_eq!(units[index], "two();");
    assert_eq!(units[index - 1], "one();");
}

#[test]
fn a_statement_at_the_end_of_a_block_has_no_neighbour() {
    let text = "class A {\n    void a() {\n        one();\n    }\n    void b() {}\n}\n";
    let (_service, receiver, _) = parse(text);
    let snapshot = wait(&receiver);
    let config = java_syntax::config();
    let query = tree_sitter::Query::new(&config.grammar, config.move_units).unwrap();
    let caret = text.find("one()").unwrap();
    assert!(
        syntax::unit_siblings(&snapshot.tree, &query, text.as_bytes(), caret..caret, true)
            .is_none(),
        "the last statement does not take its method with it"
    );
}

#[test]
fn move_units_are_the_members_of_a_class() {
    let text = "class A {\n    int one;\n    int two;\n}\n";
    let (_service, receiver, _) = parse(text);
    let snapshot = wait(&receiver);
    let (units, index) = move_units(&snapshot.tree, text, text.find("int one").unwrap(), true);
    assert_eq!(units[index], "int one;");
    assert_eq!(units[index + 1], "int two;");
}

#[test]
fn move_units_are_the_arguments_of_a_call() {
    let text = "class A {\n    void a() {\n        f(first, second);\n    }\n}\n";
    let (_service, receiver, _) = parse(text);
    let snapshot = wait(&receiver);
    let (units, index) = move_units(&snapshot.tree, text, text.find("first").unwrap(), true);
    assert_eq!(units[index], "first");
    assert_eq!(units[index + 1], "second");
}
