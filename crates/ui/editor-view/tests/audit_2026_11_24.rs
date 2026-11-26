//! Saving and reloading of the 2026-11-24 audit: an external format change reaches the
//! buffer, and a text the encoding cannot hold is never written by accident.

use std::fs;

use editor_view::{EditorState, SaveResult};
use text_core::{Eol, EolStyle, FileFormat};

#[test]
fn a_clean_buffer_takes_the_line_ends_the_file_has_now() {
    let mut state = EditorState::new(
        "one\ntwo\n",
        FileFormat::default().with_eol(Eol::CrLf),
        None,
        false,
    );
    assert_eq!(state.eol_name(), "CRLF");
    assert!(!state.is_dirty());

    let decoded = text_core::decode(b"one\ntwo\n");
    assert_eq!(decoded.format.eol, EolStyle::Uniform(Eol::Lf));
    assert!(state.reload(decoded).is_ok());
    assert_eq!(state.eol_name(), "LF", "the format of the file comes in");

    state.type_text("x");
    let (_, format, _) = state.snapshot_for_save();
    assert_eq!(format.eol, EolStyle::Uniform(Eol::Lf));
}

#[test]
fn a_character_the_encoding_cannot_store_is_not_written() {
    let directory = test_support::TempDir::new("editor-view-unmappable");
    let path = directory.path().join("legacy.txt");
    fs::write(&path, b"one \xE9\n").unwrap();
    let mut state = EditorState::load(&path, None, None).unwrap();
    assert_eq!(state.encoding_name(), "windows-1252");

    state.type_text("\u{2192}");
    state.prepare_save();
    let saved = EditorState::save_snapshot(&path, state.snapshot_for_save());

    assert!(
        matches!(saved.result, SaveResult::Unmappable("windows-1252")),
        "{:?}",
        saved.result
    );
    assert_eq!(fs::read(&path).unwrap(), b"one \xE9\n");

    let utf8 = state.format().with_encoding_name("UTF-8").unwrap();
    state.set_format(utf8);
    let saved = EditorState::save_snapshot(&path, state.snapshot_for_save());
    assert!(saved.result.is_ok(), "{:?}", saved.result);
    assert_eq!(fs::read_to_string(&path).unwrap(), "\u{2192}one \u{E9}\n");
}
