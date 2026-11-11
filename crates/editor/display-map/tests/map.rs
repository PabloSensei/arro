use display_map::{Bias, DisplayChunk, DisplayMap, DisplayPoint, Fold, Inlay};
use proptest::prelude::*;
use text_core::{Edit, RopeStore, TextStore};

fn fold(range: std::ops::Range<usize>) -> Fold {
    Fold {
        range,
        placeholder: "{...}".to_owned(),
    }
}

#[test]
fn without_folds_rows_are_lines() {
    let store = RopeStore::from_text("one\ntwo\nthree");
    let map = DisplayMap::default();
    let snapshot = map.snapshot(&store);
    assert_eq!(snapshot.row_count(), 3);
    assert_eq!(snapshot.line(1).text, "two");
    assert_eq!(snapshot.to_display(6), DisplayPoint { row: 1, column: 2 });
    assert_eq!(
        snapshot.to_offset(DisplayPoint { row: 2, column: 5 }, Bias::Left),
        13
    );
}

#[test]
fn a_fold_joins_lines_into_one_row() {
    let text = "a {\n  b\n  c\n} d\nend";
    let store = RopeStore::from_text(text);
    let mut map = DisplayMap::default();
    map.folds.collapse(fold(2..13));
    let snapshot = map.snapshot(&store);
    assert_eq!(snapshot.row_count(), 2);
    assert_eq!(snapshot.line(0).text, "a {...} d");
    assert_eq!(snapshot.line(1).text, "end");
    assert_eq!(snapshot.row_to_line(1), 4);
    assert_eq!(snapshot.line_to_row(2), 0);
    assert_eq!(snapshot.to_display(14), DisplayPoint { row: 0, column: 8 });
    assert_eq!(
        snapshot.to_offset(DisplayPoint { row: 0, column: 3 }, Bias::Left),
        2
    );
    assert_eq!(
        snapshot.to_offset(DisplayPoint { row: 0, column: 3 }, Bias::Right),
        13
    );
}

#[test]
fn tabs_expand_to_the_next_stop() {
    let store = RopeStore::from_text("\tab\tc");
    let map = DisplayMap::new(4);
    let snapshot = map.snapshot(&store);
    let line = snapshot.line(0);
    assert_eq!(line.text, "    ab  c");
    assert_eq!(snapshot.to_display(1).column, 4);
    assert_eq!(snapshot.to_display(3).column, 6);
    assert_eq!(
        snapshot.to_offset(DisplayPoint { row: 0, column: 8 }, Bias::Left),
        4
    );
}

#[test]
fn inlays_take_columns_without_taking_offsets() {
    let store = RopeStore::from_text("foo(1)");
    let mut map = DisplayMap::default();
    map.inlays.set(vec![Inlay {
        offset: 4,
        text: "x: ".to_owned(),
    }]);
    let snapshot = map.snapshot(&store);
    let line = snapshot.line(0);
    assert_eq!(line.text, "foo(x: 1)");
    assert!(
        line.chunks
            .iter()
            .any(|(_, chunk)| matches!(chunk, DisplayChunk::Inlay { offset: 4 }))
    );
    assert_eq!(snapshot.to_display(4).column, 7);
    assert_eq!(
        snapshot.to_offset(DisplayPoint { row: 0, column: 5 }, Bias::Left),
        4
    );
}

#[test]
fn folds_move_with_edits_and_an_edit_inside_expands_them() {
    let mut map = DisplayMap::default();
    map.folds.collapse(fold(10..20));
    map.apply_edits(&[Edit::insert(0, "xx")]);
    assert_eq!(map.folds.as_slice()[0].range, 12..22);
    map.apply_edits(&[Edit::insert(15, "y")]);
    assert!(map.folds.as_slice().is_empty());
}

#[test]
fn toggling_collapses_the_candidate_on_the_line_and_expands_it_back() {
    let text = "class A {\n  void b() {\n  }\n}\n";
    let store = RopeStore::from_text(text);
    let mut map = DisplayMap::default();
    map.folds.set_candidates(vec![fold(8..28), fold(21..26)]);
    assert!(map.folds.toggle_at_line(&store, 1));
    assert_eq!(map.folds.as_slice()[0].range, 21..26);
    assert_eq!(map.snapshot(&store).row_count(), 4);
    assert!(map.folds.toggle_at_line(&store, 1));
    assert!(map.folds.as_slice().is_empty());
    assert!(!map.folds.toggle_at_line(&store, 2));
}

proptest! {
    #[test]
    fn every_visible_offset_round_trips(
        lines in prop::collection::vec("[ a-z\t]{0,12}", 1..12),
        folded in prop::collection::vec((0usize..12, 1usize..4), 0..3),
    ) {
        let text = lines.join("\n");
        let store = RopeStore::from_text(&text);
        let mut map = DisplayMap::new(4);
        for (line, span) in folded {
            let line = line.min(store.line_count() - 1);
            let last = (line + span).min(store.line_count() - 1);
            let start = store.line_end(line);
            let end = store.line_end(last);
            if start < end {
                map.folds.collapse(fold(start..end));
            }
        }
        let snapshot = map.snapshot(&store);
        for offset in 0..=store.len() {
            let hidden = map.folds.as_slice().iter().any(|fold| fold.range.start < offset && offset < fold.range.end);
            if hidden {
                continue;
            }
            let point = snapshot.to_display(offset);
            let back = snapshot.to_offset(point, Bias::Left);
            prop_assert_eq!(snapshot.to_display(back), point, "offset {}", offset);
            let line = store.offset_to_line(offset);
            prop_assert_eq!(snapshot.line_to_row(line), point.row);
        }
    }
}

#[test]
fn soft_wrap_breaks_a_long_line_into_rows() {
    let store = RopeStore::from_text("one two three four\nend");
    let mut map = DisplayMap::default();
    map.set_wrap(Some(9));
    let snapshot = map.snapshot(&store);
    assert_eq!(snapshot.row_count(), 4);
    assert_eq!(snapshot.line(0).text, "one two ");
    assert_eq!(snapshot.line(1).text, "three ");
    assert_eq!(snapshot.line(2).text, "four");
    assert!(!snapshot.line(0).continuation);
    assert!(snapshot.line(1).continuation);
    assert_eq!(snapshot.line(3).text, "end");
    assert_eq!(snapshot.row_to_line(2), 0);
    assert_eq!(snapshot.row_to_line(3), 1);
    assert_eq!(snapshot.line_to_row(1), 3);
}

#[test]
fn soft_wrap_maps_offsets_and_columns_both_ways() {
    let text = "one two three four\nend";
    let store = RopeStore::from_text(text);
    let mut map = DisplayMap::default();
    map.set_wrap(Some(9));
    let snapshot = map.snapshot(&store);
    // "three" begins at byte 8, the second row.
    assert_eq!(snapshot.to_display(8), DisplayPoint { row: 1, column: 0 });
    assert_eq!(snapshot.to_display(10), DisplayPoint { row: 1, column: 2 });
    assert_eq!(snapshot.to_display(14), DisplayPoint { row: 2, column: 0 });
    // The end of the line belongs to the last row of it.
    assert_eq!(snapshot.to_display(18), DisplayPoint { row: 2, column: 4 });
    assert_eq!(
        snapshot.to_offset(DisplayPoint { row: 1, column: 3 }, Bias::Left),
        11
    );
    assert_eq!(snapshot.line(1).start_offset, 8);
    assert_eq!(snapshot.line(1).end_offset, 14);
    assert_eq!(snapshot.line(2).end_offset, 18);
}

#[test]
fn soft_wrap_and_folds_work_together() {
    let text = "a {\n  b\n  c\n} d and a very long tail of words\nend";
    let store = RopeStore::from_text(text);
    let mut map = DisplayMap::default();
    map.folds.collapse(fold(2..13));
    map.set_wrap(Some(12));
    let snapshot = map.snapshot(&store);
    assert_eq!(snapshot.line(0).text, "a {...} d ");
    assert_eq!(snapshot.row_to_line(0), 0);
    assert_eq!(snapshot.line_to_row(2), 0);
    let last = snapshot.row_count() - 1;
    assert_eq!(snapshot.line(last).text, "end");
}

#[test]
fn turning_wrap_off_gives_the_lines_back() {
    let store = RopeStore::from_text("one two three four");
    let mut map = DisplayMap::default();
    map.set_wrap(Some(5));
    assert!(map.snapshot(&store).row_count() > 1);
    map.set_wrap(None);
    assert_eq!(map.snapshot(&store).row_count(), 1);
}
