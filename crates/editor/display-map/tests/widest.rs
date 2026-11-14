//! The widest row of a text, as the layout counts it for the horizontal bar: the count
//! must be the same whether it builds the text of every row or reads the lines of the
//! store directly.

use display_map::{Block, DisplayMap, Fold, Inlay};
use text_core::{RopeStore, TextStore};

/// The widest row the slow way: the text of every row is built and counted.
fn counted_rows(map: &DisplayMap, store: &RopeStore) -> usize {
    let snapshot = map.snapshot(store);
    let mut widest = 0;
    for row in 0..snapshot.row_count() {
        if snapshot.is_block_row(row) {
            continue;
        }
        widest = widest.max(snapshot.line(row).text.chars().count());
    }
    widest
}

fn fold(start: usize, end: usize) -> Fold {
    Fold {
        range: start..end,
        placeholder: "...".into(),
    }
}

#[test]
fn the_short_count_of_a_plain_text_is_the_count_of_its_rows() {
    let store = RopeStore::from_text("one\ntwo longer\nthree");
    let map = DisplayMap::default();

    let snapshot = map.snapshot(&store);
    assert_eq!(snapshot.widest_row_columns(), 10);
    assert_eq!(snapshot.widest_row_columns(), counted_rows(&map, &store));

    for text in ["", "one", "one\r\ntwo longer\r\n", "a\n\nlongest"] {
        let store = RopeStore::from_text(text);
        let snapshot = map.snapshot(&store);
        assert_eq!(
            snapshot.widest_row_columns(),
            counted_rows(&map, &store),
            "{text:?}"
        );
    }
}

#[test]
fn a_tab_counts_as_the_spaces_it_shows() {
    let store = RopeStore::from_text("a\tb\nab");
    let mut map = DisplayMap::default();
    map.tab_size = 4;

    let snapshot = map.snapshot(&store);
    assert_eq!(
        snapshot.line(0).text,
        "a   b",
        "the row shows the tab as spaces"
    );
    assert_eq!(snapshot.widest_row_columns(), 5);
    assert_eq!(snapshot.widest_row_columns(), counted_rows(&map, &store));
}

#[test]
fn a_fold_placeholder_and_a_hint_count_with_the_row() {
    let store = RopeStore::from_text("one\ntwo\nthree");
    let mut map = DisplayMap::default();
    map.folds.collapse(fold(4, 13));

    let snapshot = map.snapshot(&store);
    assert_eq!(snapshot.line(1).text, "...");
    assert_eq!(snapshot.widest_row_columns(), counted_rows(&map, &store));

    let mut map = DisplayMap::default();
    map.inlays.set(vec![Inlay {
        offset: 3,
        text: "x: i32 ".to_owned(),
    }]);
    let snapshot = map.snapshot(&store);
    assert_eq!(snapshot.widest_row_columns(), counted_rows(&map, &store));
}

#[test]
fn a_block_row_and_a_rendered_comment_have_no_text_of_their_own() {
    let store = RopeStore::from_text("one\ntwo");
    let mut map = DisplayMap::default();
    map.set_blocks(vec![Block {
        offset: 4,
        text: "a label much longer than the text".to_owned(),
        indent: 0,
        after: false,
    }]);

    let snapshot = map.snapshot(&store);
    assert_eq!(snapshot.row_count(), 3);
    assert_eq!(
        snapshot.widest_row_columns(),
        3,
        "the label of a block is no text of the file"
    );
}
