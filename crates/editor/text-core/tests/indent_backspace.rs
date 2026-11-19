//! Backspace in the indentation: one step of the indentation goes, an empty line drops
//! to its proper indent and then joins the line above, and an empty pair still goes.

use text_core::{
    Buffer, EditSource, FileFormat, IndentSettings, RopeStore, Selection, SelectionSet, commands,
};

const USER: EditSource = EditSource::User;

fn buffer(marked: &str) -> Buffer<RopeStore> {
    let mut text = String::new();
    let mut caret = 0;
    for character in marked.chars() {
        if character == '|' {
            caret = text.len();
        } else {
            text.push(character);
        }
    }
    let mut buffer = Buffer::new(&text, FileFormat::default());
    buffer.set_selections(SelectionSet::single(Selection::caret(caret)));
    buffer
}

#[test]
fn eight_spaces_of_indent_lose_a_step_of_four() {
    let mut buffer = buffer("    one\n        |two\n");
    let indent = IndentSettings::manual(false, 4, 4);
    commands::backspace_indent(&mut buffer, &indent, USER);
    assert_eq!(buffer.text(), "    one\n    two\n");
}

#[test]
fn an_empty_line_drops_to_its_proper_indent_and_then_joins() {
    let mut buffer = buffer("    one\n            |\n");
    let indent = IndentSettings::manual(false, 4, 4);
    commands::backspace_indent(&mut buffer, &indent, USER);
    assert_eq!(buffer.text(), "    one\n    \n");
    commands::backspace_indent(&mut buffer, &indent, USER);
    assert_eq!(buffer.text(), "    one\n");
}

#[test]
fn an_empty_pair_still_goes_together() {
    let mut buffer = buffer("f(|)");
    let indent = IndentSettings::manual(false, 4, 4);
    commands::backspace_indent(&mut buffer, &indent, USER);
    assert_eq!(buffer.text(), "f");
}
