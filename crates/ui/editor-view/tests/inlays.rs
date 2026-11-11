//! Hints in the editor state: the text, the caret and a copy do not see them.

use display_map::Inlay;
use editor_view::EditorState;
use text_core::FileFormat;

fn state(text: &str) -> EditorState {
    EditorState::new(text, FileFormat::default(), None, false)
}

#[test]
fn a_hint_is_drawn_between_the_characters_and_the_buffer_stays_the_same() {
    let mut state = state("foo(1)");
    state.set_inlays(vec![Inlay {
        offset: 4,
        text: "count: ".to_owned(),
    }]);

    assert_eq!(state.text(), "foo(1)");
    let snapshot = state.map().snapshot(state.store());
    assert_eq!(snapshot.line(0).text, "foo(count: 1)");
}

#[test]
fn a_copy_and_a_selection_have_the_bytes_around_a_hint_only() {
    let mut state = state("foo(1)");
    state.set_inlays(vec![Inlay {
        offset: 4,
        text: "count: ".to_owned(),
    }]);
    state.select_range(0..6);

    assert_eq!(state.selected_chars(), 6);
    assert_eq!(state.copy_text(), "foo(1)");
    assert_eq!(
        state.map().snapshot(state.store()).to_display(4).column,
        11,
        "the caret after the bracket stands after the hint"
    );
}

#[test]
fn an_edit_moves_a_hint_and_typing_near_it_keeps_the_text() {
    let mut state = state("foo(1);\n");
    state.set_inlays(vec![Inlay {
        offset: 8,
        text: "count: ".to_owned(),
    }]);
    state.select_range(0..0);
    state.type_text("xx");

    assert_eq!(state.text(), "xxfoo(1);\n");
    assert_eq!(state.inlays()[0].offset, 10);
}

#[test]
fn soft_wrap_counts_the_width_of_a_hint() {
    let mut state = state("aaaaaaaaaaaa");
    state.set_wrap(Some(8));
    let rows = state.map().snapshot(state.store()).row_count();
    state.set_inlays(vec![Inlay {
        offset: 5,
        text: "xxxxx".to_owned(),
    }]);
    assert!(state.map().snapshot(state.store()).row_count() > rows);
}
