//! Hints between the characters: they take room on screen and no byte of the text.

use display_map::{Bias, DisplayChunk, DisplayMap, DisplayPoint, Fold, Inlay};
use text_core::{Edit, RopeStore, TextStore};

fn map_with(inlays: Vec<Inlay>) -> DisplayMap {
    let mut map = DisplayMap::default();
    map.inlays.set(inlays);
    map
}

#[test]
fn a_hint_changes_the_row_and_no_byte_of_the_buffer() {
    let store = RopeStore::from_text("foo(1)");
    let map = map_with(vec![Inlay {
        offset: 4,
        text: "count: ".to_owned(),
    }]);
    assert_eq!(store.text(), "foo(1)", "the hint is not part of the text");
    let line = map.snapshot(&store).line(0);
    assert_eq!(line.text, "foo(count: 1)");
    assert!(
        line.chunks
            .iter()
            .any(|(_, chunk)| matches!(chunk, DisplayChunk::Inlay { offset: 4 }))
    );
}

#[test]
fn the_caret_stands_before_or_after_a_hint_and_never_inside_it() {
    let store = RopeStore::from_text("foo(1)");
    let map = map_with(vec![Inlay {
        offset: 4,
        text: "count: ".to_owned(),
    }]);
    let snapshot = map.snapshot(&store);
    assert_eq!(snapshot.to_display(3).column, 3, "before the bracket");
    assert_eq!(snapshot.to_display(4).column, 11, "after the hint");
    for column in 4..11 {
        assert_eq!(
            snapshot.to_offset(DisplayPoint { row: 0, column }, Bias::Left),
            4,
            "every column of the hint is the place it begins"
        );
    }
    assert_eq!(
        snapshot.to_offset(DisplayPoint { row: 0, column: 1 }, Bias::Left),
        1,
        "a copy of the text has the bytes around the hint only"
    );
}

#[test]
fn soft_wrap_counts_the_width_of_a_hint() {
    let store = RopeStore::from_text("aaaaaaaaaaaa");
    let mut map = DisplayMap::default();
    map.set_wrap(Some(8));
    let without = map.snapshot(&store).row_count();
    map.inlays.set(vec![Inlay {
        offset: 5,
        text: "xxxxx".to_owned(),
    }]);
    let snapshot = map.snapshot(&store);
    assert!(
        snapshot.row_count() > without,
        "the hint makes the line longer"
    );
    let joined: String = (0..snapshot.row_count())
        .map(|row| snapshot.line(row).text)
        .collect();
    assert_eq!(joined, "aaaaaxxxxxaaaaaaa");
    assert_eq!(snapshot.line(0).text, "aaaaaxxx");
    assert!(snapshot.line(1).continuation);
}

#[test]
fn a_fold_after_a_hint_keeps_the_hint_in_front_of_its_placeholder() {
    let store = RopeStore::from_text("foo(1)\nmore\nend");
    let mut map = DisplayMap::default();
    map.folds.collapse(Fold {
        range: 4..11,
        placeholder: "{...}".to_owned(),
    });
    map.inlays.set(vec![Inlay {
        offset: 5,
        text: "x".to_owned(),
    }]);
    let line = map.snapshot(&store).line(0);
    assert_eq!(line.text, "foo({...}");
}

#[test]
fn edits_move_a_hint_and_an_edit_over_it_takes_it_away() {
    let mut map = DisplayMap::default();
    map.inlays.set(vec![Inlay {
        offset: 8,
        text: "name: ".to_owned(),
    }]);
    map.apply_edits(&[Edit::insert(2, "xx")]);
    assert_eq!(map.inlays.as_slice()[0].offset, 10);
    map.apply_edits(&[Edit::new(4..12, String::new())]);
    assert!(map.inlays.as_slice().is_empty(), "the argument went away");
}

#[test]
fn hints_at_one_place_keep_their_order() {
    let mut map = DisplayMap::default();
    map.inlays.set(vec![
        Inlay {
            offset: 4,
            text: "second".to_owned(),
        },
        Inlay {
            offset: 4,
            text: "first".to_owned(),
        },
    ]);
    let store = RopeStore::from_text("foo(1)");
    assert_eq!(map.snapshot(&store).line(0).text, "foo(secondfirst1)");
}
