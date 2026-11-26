//! Data loss and crashes of the 2026-11-24 audit: encoding, overlapping plans, undo
//! memory, regex replacements, the format in the saved state and concurrent saves.

use std::fs;
use std::sync::Arc;
use std::thread;

use text_core::find::{FindOptions, replacements};
use text_core::{
    Buffer, Edit, EditKind, EditSource, Eol, FileFormat, Selection, SelectionSet, TextEncoding,
    editing, read_file, write_file,
};

fn buffer(text: &str) -> Buffer {
    Buffer::new(text, FileFormat::default())
}

fn carets(offsets: &[usize]) -> SelectionSet {
    SelectionSet::from_vec(
        offsets
            .iter()
            .map(|offset| Selection::caret(*offset))
            .collect(),
    )
}

#[test]
fn an_encoding_that_cannot_store_a_character_leaves_the_file_alone() {
    let directory = test_support::scratch("text-core-unmappable");
    let path = directory.join("legacy.txt");
    fs::write(&path, b"one \xE9\n").unwrap();
    let format = read_file(&path).unwrap().format;
    assert!(matches!(format.encoding, TextEncoding::Legacy(_)));

    let bytes = "one \u{2192}\n";
    assert_eq!(
        text_core::unmappable_encoding(bytes, &format),
        Some(format.encoding.name())
    );
    let error = write_file(&path, bytes, &format).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert_eq!(fs::read(&path).unwrap(), b"one \xE9\n");
}

#[test]
fn two_carets_in_one_word_change_it_once() {
    let mut buffer = buffer("one two\n");
    buffer.set_selections(carets(&[0, 1]));

    editing::toggle_case(&mut buffer, EditSource::User);

    assert_eq!(buffer.text(), "ONE two\n");
}

#[test]
fn two_carets_in_the_indentation_insert_one_brace() {
    let mut buffer = buffer("    x\n");
    buffer.set_selections(carets(&[2, 4]));

    editing::type_char(&mut buffer, '}', "    ", EditSource::User);

    assert_eq!(buffer.text().matches('}').count(), 1, "{}", buffer.text());
}

#[test]
fn complete_statement_twice_on_a_line_adds_one_semicolon() {
    let mut buffer = buffer("foo");
    buffer.set_selections(carets(&[0, 2]));

    editing::complete_statement(&mut buffer, ";", &[], "    ", EditSource::User);

    assert_eq!(buffer.text().matches(';').count(), 1, "{}", buffer.text());
}

#[test]
fn a_group_number_stops_before_letters() {
    let options = FindOptions {
        regex: true,
        ..FindOptions::default()
    };
    let edits = replacements("foo", r"(\w+)", options, "$1_old", None).unwrap();
    assert_eq!(edits, [Edit::new(0..3, "foo_old")]);

    let dollars = replacements("foo", "foo", options, "$$", None).unwrap();
    assert_eq!(dollars, [Edit::new(0..3, "$")]);
}

#[test]
fn an_overlapping_change_changes_nothing_instead_of_panicking() {
    let mut buffer = buffer("abcdef");
    let change = buffer.apply(
        vec![Edit::new(0..3, "x"), Edit::new(2..5, "y")],
        None,
        EditSource::User,
        EditKind::Command,
    );
    assert!(change.is_empty());
    assert_eq!(buffer.text(), "abcdef");
    // The document is still usable after the refused change.
    buffer.apply(
        vec![Edit::insert(0, "z")],
        None,
        EditSource::User,
        EditKind::Command,
    );
    assert_eq!(buffer.text(), "zabcdef");
}

#[test]
fn the_undo_history_stays_longer_than_one_step() {
    let big = "a".repeat(4 * 1024 * 1024);
    let replaced = "b".repeat(4 * 1024 * 1024);
    let other = "c".repeat(4 * 1024 * 1024);
    let mut buffer = Buffer::<text_core::RopeStore>::new(&big, FileFormat::default());
    let edit = |text: &str| vec![Edit::new(0..big.len(), text.to_owned())];
    // Enough large changes to fill a part of the memory limit, so that the limit can
    // trim the history during the cycles below.
    for _ in 0..20 {
        buffer.apply(edit(&replaced), None, EditSource::User, EditKind::Command);
    }
    for index in 0..100 {
        buffer.undo();
        let text = if index % 2 == 0 { &replaced } else { &other };
        buffer.apply(edit(text), None, EditSource::User, EditKind::Command);
    }
    assert!(buffer.undo().is_some(), "the latest change comes back");
    assert!(
        buffer.undo().is_some(),
        "the history keeps more than one step"
    );
}

#[test]
fn a_format_change_alone_keeps_the_buffer_dirty_after_undo() {
    let mut buffer = buffer("a\n");
    buffer.apply(
        vec![Edit::insert(0, "x")],
        None,
        EditSource::User,
        EditKind::Command,
    );
    buffer.set_format(buffer.format().with_eol(Eol::CrLf));
    assert!(buffer.is_dirty());

    buffer.undo();

    assert!(buffer.is_dirty(), "the format on disk is still the old one");
}

#[test]
fn two_saves_of_one_path_both_finish_and_the_file_holds_one_of_them() {
    let directory = test_support::scratch("text-core-parallel-saves");
    let path = directory.join("note.txt");
    let first = "a".repeat(1024 * 1024);
    let second = "b".repeat(1024 * 1024);
    let (one, two) = (Arc::new(first.clone()), Arc::new(second.clone()));
    let (first_path, second_path) = (path.clone(), path.clone());

    let one = thread::spawn(move || write_file(&first_path, &one, &FileFormat::default()));
    let two = thread::spawn(move || write_file(&second_path, &two, &FileFormat::default()));

    one.join().unwrap().unwrap();
    two.join().unwrap().unwrap();
    let bytes = fs::read(&path).unwrap();
    assert!(
        bytes == first.as_bytes() || bytes == second.as_bytes(),
        "the file is one whole snapshot"
    );
}

#[cfg(unix)]
#[test]
fn a_symbolic_link_stays_a_link_and_its_target_changes() {
    let directory = test_support::scratch("text-core-symlink");
    let target = directory.join("target.txt");
    let link = directory.join("link.txt");
    fs::write(&target, b"one\n").unwrap();
    platform::symlink_file(&target, &link).unwrap();
    let format = read_file(&link).unwrap().format;

    write_file(&link, "two\n", &format).unwrap();

    assert!(
        fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read(&target).unwrap(), b"two\n");
    assert_eq!(fs::read(&link).unwrap(), b"two\n");
}
