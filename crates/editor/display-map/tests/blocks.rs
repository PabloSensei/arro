//! Rows above a line of the buffer: a label of its own that takes no byte of the text.

use display_map::{Block, DisplayMap, Fold};
use text_core::{RopeStore, TextStore};

fn block(offset: usize, text: &str, indent: usize) -> Block {
    Block {
        offset,
        text: text.to_owned(),
        indent,
    }
}

fn fold(start: usize, end: usize) -> Fold {
    Fold {
        range: start..end,
        placeholder: "...".into(),
    }
}

#[test]
fn a_block_row_holds_its_label_and_leaves_the_text_of_the_lines_alone() {
    let store = RopeStore::from_text("one\ntwo\nthree");
    let mut map = DisplayMap::default();
    map.set_blocks(vec![block(4, "2 usages", 0)]);

    let snapshot = map.snapshot(&store);
    assert_eq!(snapshot.row_count(), 4, "the block takes a row of its own");
    assert_eq!(snapshot.line(0).text, "one");
    assert_eq!(snapshot.line(1).text, "2 usages");
    assert_eq!(snapshot.line(1).block.map(|block| block.indent), Some(0));
    assert_eq!(snapshot.line(2).text, "two");
    assert_eq!(snapshot.line(3).text, "three");
    assert_eq!(store.text(), "one\ntwo\nthree", "the block is no text");
}

#[test]
fn one_step_down_from_the_line_above_a_block_reaches_the_line_under_it() {
    let store = RopeStore::from_text("one\ntwo\nthree");
    let mut map = DisplayMap::default();
    map.set_blocks(vec![block(4, "2 usages", 2)]);

    let snapshot = map.snapshot(&store);
    assert!(snapshot.is_block_row(1));
    assert_eq!(snapshot.text_row(0, 1), Some(2), "the block is passed over");
    assert_eq!(
        snapshot.text_row(2, -1),
        Some(0),
        "up goes to the line above"
    );
    assert_eq!(snapshot.text_row(2, 1), Some(3));
    assert_eq!(snapshot.text_row(0, -1), None, "nothing stands above");
}

#[test]
fn a_block_has_no_line_number_of_its_own() {
    let store = RopeStore::from_text("one\ntwo\nthree");
    let mut map = DisplayMap::default();
    map.set_blocks(vec![block(4, "2 usages", 0)]);

    let snapshot = map.snapshot(&store);
    assert_eq!(
        snapshot.row_to_line(1),
        1,
        "the block speaks for the line below"
    );
    assert_eq!(
        snapshot.line_to_row(1),
        2,
        "the declaration keeps its own text row"
    );
    assert_eq!(
        snapshot.to_display(4),
        display_map::DisplayPoint { row: 2, column: 0 }
    );
}

#[test]
fn a_fold_keeps_the_block_of_its_first_line_and_hides_the_ones_inside() {
    let text = "a {\n  b\n  c\n} d";
    let store = RopeStore::from_text(text);
    let mut map = DisplayMap::default();
    map.folds.collapse(fold(2, 13));
    map.set_blocks(vec![block(0, "first line", 0), block(6, "inside", 2)]);

    let snapshot = map.snapshot(&store);
    assert_eq!(snapshot.row_count(), 2, "the label and the folded line");
    assert!(snapshot.is_block_row(0));
    assert_eq!(snapshot.line(0).text, "first line");
    assert_eq!(snapshot.line(1).text, "a ... d");
    assert!(
        !(0..snapshot.row_count()).any(|row| snapshot.line(row).text == "inside"),
        "a block inside the collapsed region has no row"
    );
}

#[test]
fn soft_wrap_and_the_rows_of_the_scroll_bar_count_the_block() {
    let store = RopeStore::from_text("one two three four\nend");
    let mut map = DisplayMap::default();
    map.set_wrap(Some(9));
    let without = map.snapshot(&store).row_count();
    map.set_blocks(vec![block(19, "1 implementation", 0)]);

    let snapshot = map.snapshot(&store);
    assert_eq!(snapshot.row_count(), without + 1);
    assert_eq!(snapshot.line(without - 1).text, "1 implementation");
    assert_eq!(snapshot.line(without).text, "end");
    assert_eq!(
        snapshot.line_to_row(1),
        without,
        "the last line stands after the block"
    );
    assert_eq!(snapshot.text_row(without - 2, 1), Some(without));
}

#[test]
fn a_block_moves_with_the_text_above_it() {
    let mut map = DisplayMap::default();
    map.set_blocks(vec![block(4, "2 usages", 0)]);
    map.apply_edits(&[text_core::Edit::insert(0, "x\n")]);

    let store = RopeStore::from_text("x\none\ntwo");
    let snapshot = map.snapshot(&store);
    assert_eq!(map.blocks()[0].offset, 6);
    assert!(snapshot.is_block_row(2));
    assert_eq!(snapshot.line(2).text, "2 usages");
}
