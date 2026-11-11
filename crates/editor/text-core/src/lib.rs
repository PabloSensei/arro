//! Text of one document: storage, edits, selections, history and the bytes on disk.
//! Inside, line ends are always `\n`; the file's own line ends live in [`FileFormat`].

mod buffer;
pub mod commands;
mod edit;
pub mod editing;
pub mod editorconfig;
mod encoding;
mod file;
pub mod find;
mod history;
pub mod indent;
mod selection;
mod store;

pub use buffer::{Buffer, Change, apply_edits};
pub use edit::{Edit, EditKind, EditSource, difference};
pub use editorconfig::{
    Charset, EndOfLine, IndentSize, IndentStyle, Properties, apply_save, for_file,
};
pub use encoding::{
    Decoded, Eol, EolStyle, FileFormat, TextEncoding, decode, encode, normalize_newlines,
};
pub use file::{FileUpdate, MAX_EDITABLE_BYTES, read_file, write_file, write_files};
pub use history::History;
pub use indent::{IndentSettings, IndentSource};
pub use selection::{Selection, SelectionSet, transform_offset};
pub use store::{RopeStore, TextStore};
