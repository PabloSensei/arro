//! Closers the editor inserted, Tab padding, paste blocks and the input method, checked on
//! the state of an editor without a window.

use keymap::editor as actions;
use text_core::FileFormat;

use crate::state::EditorState;

fn state(text: &str) -> EditorState {
    EditorState::new(text, FileFormat::default(), None, false)
}

fn type_in(state: &mut EditorState, text: &str) {
    for character in text.chars() {
        state.replace_range(None, &character.to_string(), false);
    }
}

#[test]
fn a_closer_typed_over_the_inserted_one_is_the_users() {
    let mut state = state("");
    type_in(&mut state, "f(x)");
    assert_eq!(state.text(), "f(x)");
    // Twenty lines of edits above the call, then the caret stands before its closer.
    state.select_range(0..0);
    for _ in 0..20 {
        state.type_text("line\n");
    }
    state.select_range(103..103);
    assert!(!state.brace_or_quote_out(), "the closer is the user's");
    state.perform(actions::TAB, 10);
    assert_eq!(state.text(), format!("{}f(x )", "line\n".repeat(20)));
}

#[test]
fn moving_out_of_the_pair_releases_the_closer() {
    let mut state = state("");
    type_in(&mut state, "f(x");
    assert_eq!(state.text(), "f(x)");
    assert_eq!(state.head(), 3);
    state.perform(actions::MOVE_LEFT, 10);
    state.perform(actions::MOVE_LEFT, 10);
    assert_eq!(state.head(), 1, "the caret is before the opening character");
    state.perform(actions::MOVE_RIGHT, 10);
    state.perform(actions::MOVE_RIGHT, 10);
    assert_eq!(state.head(), 3);
    assert!(!state.brace_or_quote_out());
}

#[test]
fn moving_to_another_line_releases_the_closer() {
    let mut state = state("ab");
    state.select_range(2..2);
    type_in(&mut state, "(");
    state.type_text("\nc");
    assert_eq!(state.text(), "ab(\nc)");
    state.set_caret(0, 0);
    state.set_caret(1, 1);
    assert!(!state.brace_or_quote_out());
}

#[test]
fn tab_pads_to_the_next_column_counted_in_characters() {
    let mut state = state("привет");
    state.select_range(12..12);
    state.perform(actions::TAB, 10);
    assert_eq!(state.text(), "привет  ");
    assert_eq!(state.head(), 14);
}

#[test]
fn tab_pads_each_caret_by_its_own_column() {
    let mut state = state("a\nbcd");
    state.click(1, false, false, 1);
    state.click(5, false, true, 1);
    state.perform(actions::TAB, 10);
    assert_eq!(state.text(), "a   \nbcd ");
}

#[test]
fn a_block_copied_from_the_middle_of_a_line_keeps_its_shape() {
    let mut state = state("        ");
    state.select_range(8..8);
    state.paste("if (x) {\n        foo();\n    }");
    assert_eq!(
        state.text(),
        "        if (x) {\n            foo();\n        }"
    );
}

#[test]
fn a_block_copied_from_the_start_of_its_line_keeps_its_shape() {
    let mut state = state("    ");
    state.select_range(4..4);
    state.paste(
        "def f():
    return 1",
    );
    assert_eq!(
        state.text(),
        "    def f():
        return 1"
    );
}

#[test]
fn a_composition_with_two_carets_leaves_no_piece_behind() {
    let mut state = state("ab\ncd");
    state.click(0, false, false, 1);
    state.click(3, false, true, 1);
    // The sequence of input.rs: the first part has no range, the updates and the commit
    // take the marked range the system reads back, and unmark follows the commit.
    state.replace_range(None, "n", true);
    let marked = state.marked();
    state.replace_range(marked, "ni", true);
    let marked = state.marked();
    state.replace_range(marked, "你", false);
    state.unmark();
    let text = state.text();
    assert!(text == "你ab\ncd" || text == "ab\n你cd", "{text:?}");
}
