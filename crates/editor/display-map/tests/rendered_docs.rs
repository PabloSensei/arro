//! Rendered documentation comments take the rows of their text and leave the buffer
//! unchanged.

use display_map::{DisplayMap, DocComment, DocLine, DocRun, DocRunKind};
use text_core::{RopeStore, TextStore};

fn line(text: &str) -> DocLine {
    DocLine {
        indent: 4,
        runs: vec![DocRun {
            text: text.to_owned(),
            kind: DocRunKind::Text,
            target: None,
        }],
    }
}

fn comment(range: std::ops::Range<usize>, lines: &[&str]) -> DocComment {
    DocComment {
        range,
        lines: lines.iter().map(|text| line(text)).collect(),
    }
}

const TEXT: &str = "class A {\n    /**\n     * Doc.\n     */\n    int x;\n}\n";

fn bounds() -> (usize, usize) {
    let start = TEXT.find("    /**").unwrap();
    let end = TEXT.find("    int x;").unwrap();
    (start, end)
}

#[test]
fn the_rows_of_a_comment_stand_in_place_of_the_rows_of_its_text() {
    let store = RopeStore::from_text(TEXT);
    let mut map = DisplayMap::default();
    let (start, end) = bounds();
    map.set_doc_comments(vec![comment(
        start..end,
        &["Doc.", "@param x a value", "More.", "Done."],
    )]);

    let snapshot = map.snapshot(&store);
    assert_eq!(snapshot.row_count(), 8, "four rows replace three lines");
    assert_eq!(snapshot.line(0).text, "class A {");
    assert!(snapshot.is_doc_row(1));
    assert_eq!(snapshot.doc_row(1).unwrap().text(), "Doc.");
    assert_eq!(snapshot.doc_row(2).unwrap().text(), "@param x a value");
    assert_eq!(snapshot.doc_range(2), Some(start..end));
    assert!(
        snapshot.is_block_row(1),
        "the caret and the text rows pass the comment"
    );
    assert_eq!(snapshot.line(5).text, "    int x;");
    assert_eq!(snapshot.line(6).text, "}");
    assert_eq!(store.text(), TEXT, "the comment is still the text");
}

#[test]
fn the_lines_below_a_comment_move_with_its_rows() {
    let store = RopeStore::from_text(TEXT);
    let mut map = DisplayMap::default();
    let (start, end) = bounds();
    map.set_doc_comments(vec![comment(
        start..end,
        &["Doc.", "One.", "Two.", "Three."],
    )]);

    let snapshot = map.snapshot(&store);
    assert_eq!(snapshot.line_to_row(4), 5, "the declaration stands lower");
    assert_eq!(snapshot.line_to_row(5), 6);
    assert_eq!(snapshot.row_to_line(2), 1, "a doc row speaks for its line");
    assert_eq!(snapshot.text_row(0, 1), Some(5), "one step down passes it");
    assert_eq!(snapshot.text_row(5, -1), Some(0), "one step up too");
    assert_eq!(
        snapshot.to_display(start + 8),
        display_map::DisplayPoint { row: 1, column: 0 }
    );
    assert_eq!(
        snapshot.to_offset(
            display_map::DisplayPoint { row: 2, column: 0 },
            display_map::Bias::Left
        ),
        start
    );
}

#[test]
fn an_edit_over_a_comment_brings_its_text_back() {
    let store = RopeStore::from_text(TEXT);
    let mut map = DisplayMap::default();
    let (start, end) = bounds();
    map.set_doc_comments(vec![comment(start..end, &["Doc."])]);
    map.apply_edits(&[text_core::Edit::new(start + 4..start + 5, "x".to_owned())]);

    let snapshot = map.snapshot(&store);
    assert!(map.doc_comments().is_empty());
    assert_eq!(snapshot.row_count(), store.line_count());
    assert_eq!(snapshot.line(1).text, "    /**");
}

#[test]
fn soft_wrap_counts_the_rows_of_a_comment() {
    let store = RopeStore::from_text(TEXT);
    let mut map = DisplayMap::default();
    map.set_wrap(Some(8));
    let (start, end) = bounds();
    map.set_doc_comments(vec![comment(start..end, &["Doc.", "More."])]);

    let snapshot = map.snapshot(&store);
    let text = snapshot.line_to_row(4);
    assert!(text > 2);
    assert_eq!(snapshot.doc_row(text - 2).unwrap().text(), "Doc.");
    assert_eq!(snapshot.doc_row(text - 1).unwrap().text(), "More.");
}
