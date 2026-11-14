//! The open document and every command on it.

use std::ops::Range;
use std::path::Path;
use std::sync::Arc;

use display_map::{Bias, DisplayChunk, DisplayMap, DisplayPoint, DisplaySnapshot, Fold};
use keymap::editor as actions;
use syntax::{
    HighlightSpan, LanguageConfig, Query, SyntaxSnapshot, Tree, enclosing_range, indent_after,
    normalize_spans, shift_spans,
};
use text_core::{
    Buffer, Change, Decoded, Edit, EditKind, EditSource, Followed, RopeStore, Selection,
    SelectionSet, TextStore, commands, difference, editing, normalize_newlines, read_file,
    transform_offset, write_file,
};

use crate::annotate::BlameNote;
use crate::blocks::{brace_pairs, containing_block, element_list_at, enclosing_block, units_in};
use crate::semantic::{TokenSpan, normalize_tokens, shift_tokens};

/// Above this size the file opens without highlighting. The parse runs on its own thread,
/// so a big file shows its text first and the colours follow.
const SYNTAX_LIMIT_BYTES: usize = 20 * 1024 * 1024;

#[derive(Debug)]
pub struct LoadError(pub String);

/// The file changed on disk while the buffer has unsaved edits.
#[derive(Debug, PartialEq, Eq)]
pub struct Conflict;

/// What a finished background save reports back.
#[derive(Debug)]
pub struct Saved {
    pub version: u64,
    /// The text that went to the disk, as it was when the write started.
    pub text: String,
    /// The encoding, BOM and line ends the text went out in.
    pub format: text_core::FileFormat,
    pub result: Result<(), String>,
}

/// Which lines a save cleans of the spaces and tabs at their end.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TrimTrailing {
    /// Keep every line as it is.
    #[default]
    None,
    /// The lines changed since the file was read or saved.
    Modified,
    /// Every line.
    All,
}

/// The save rules of the window, where `.editorconfig` says nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SaveOptions {
    pub trim_trailing: TrimTrailing,
    /// A save puts a line break after the last line when the file lacks one.
    pub ensure_final_newline: bool,
}

struct Parsed {
    version: u64,
    tree: Tree,
    indents: Arc<Query>,
}

/// One document as the editor sees it.
pub struct EditorState {
    buffer: Buffer,
    map: DisplayMap,
    language: Option<LanguageConfig>,
    parsed: Option<Parsed>,
    spans: Vec<HighlightSpan>,
    /// Semantic tokens of the language server, over the syntax spans.
    tokens: Vec<TokenSpan>,
    opened_folds: bool,
    /// Column the caret tries to return to on vertical moves, in characters.
    goal: Option<usize>,
    /// Selections before each Ctrl+W, for Ctrl+Shift+W.
    grown: Vec<SelectionSet>,
    marked: Option<Range<usize>>,
    read_only: bool,
    /// The file carries the read-only attribute of the disk.
    attribute_read_only: bool,
    /// Where the latest edits were, oldest first, for Ctrl+Shift+Backspace.
    places: Vec<usize>,
    place_cursor: Option<usize>,
    /// Bookmarks as the offset of a place on their line, which follows the text.
    bookmarks: Vec<(usize, Option<char>)>,
    /// The blame of the lines by the offset of the start of the line; `None` when the
    /// column is off. The offsets follow the text as the bookmarks do.
    annotations: Option<Vec<(usize, BlameNote)>>,
    /// The window keeps the bookmarks of this file from this view; a second view of the
    /// document only reads them, so the two never erase each other in the shared list.
    writes_bookmarks: bool,
    /// Insert replaces the character under the caret.
    overwrite: bool,
    /// The closing characters the editor inserted together with an opening one.
    auto_closers: Vec<usize>,
    /// The `.editorconfig` properties of the file, for the text of a save.
    properties: text_core::Properties,
    /// The indentation of this file: its unit, the width of a tab and where it came from.
    indent: text_core::IndentSettings,
    /// The file had no text when it was opened, so a save may give it the line ends of
    /// the `.editorconfig`.
    new_file: bool,
    /// The text as it was read or last saved, for the modified lines of a save.
    saved_text: String,
    /// The save rules of the window; `.editorconfig` wins over them.
    save: SaveOptions,
}

/// Edit places kept.
const PLACES: usize = 50;

/// Which lines of `text` differ from the same line of `saved`, for the modified lines of
/// a save. A line past the end of the saved text counts as modified.
fn modified_lines(text: &str, saved: &str) -> Vec<bool> {
    let mut before = saved.split('\n');
    text.split('\n')
        .map(|line| before.next().is_none_or(|old| old != line))
        .collect()
}

fn chars_in(text: &str, column: usize) -> usize {
    text[..column.min(text.len())].chars().count()
}

fn column_of_char(text: &str, chars: usize) -> usize {
    text.char_indices()
        .nth(chars)
        .map_or(text.len(), |(at, _)| at)
}

/// Moves the offset of a blame label with the text. Text inserted exactly at the offset
/// leaves the label with the line that follows it, so a line typed above takes no label
/// and the lines under it keep theirs.
fn shift_annotation(offset: usize, edits: &[Edit]) -> usize {
    let mut delta = 0isize;
    for edit in edits {
        if edit.range.end < offset {
            delta += edit.text.len() as isize - edit.range.len() as isize;
        } else if edit.range.start < offset {
            return (edit.range.start as isize + delta) as usize + edit.text.len();
        } else if edit.range.start == offset && edit.range.is_empty() {
            delta += edit.text.len() as isize;
        } else {
            break;
        }
    }
    (offset as isize + delta) as usize
}

impl EditorState {
    pub fn new(
        text: &str,
        format: text_core::FileFormat,
        language: Option<LanguageConfig>,
        read_only: bool,
    ) -> Self {
        let language = language.filter(|_| text.len() <= SYNTAX_LIMIT_BYTES);
        let unit = language
            .as_ref()
            .map_or("    ", |language| language.indent_unit);
        let indent =
            text_core::IndentSettings::resolve(&text_core::Properties::default(), text, unit);
        let mut map = DisplayMap::default();
        map.tab_size = indent.tab_width;
        Self {
            buffer: Buffer::new(text, format),
            map,
            language,
            parsed: None,
            spans: Vec::new(),
            tokens: Vec::new(),
            opened_folds: false,
            goal: None,
            grown: Vec::new(),
            marked: None,
            read_only,
            attribute_read_only: false,
            places: Vec::new(),
            place_cursor: None,
            bookmarks: Vec::new(),
            annotations: None,
            writes_bookmarks: true,
            overwrite: false,
            auto_closers: Vec::new(),
            properties: text_core::Properties::default(),
            indent,
            new_file: text.is_empty(),
            saved_text: text.to_owned(),
            save: SaveOptions::default(),
        }
    }

    /// Another view of the same document: the text and its undo history are shared, while
    /// the carets, folds, scroll and selections belong to this view alone.
    pub fn fork(&self) -> Self {
        let mut map = DisplayMap::default();
        map.tab_size = self.map.tab_size;
        Self {
            buffer: self.buffer.fork(),
            map,
            language: self.language.clone(),
            parsed: None,
            spans: self.spans.clone(),
            tokens: self.tokens.clone(),
            opened_folds: false,
            goal: None,
            grown: Vec::new(),
            marked: None,
            read_only: self.read_only,
            attribute_read_only: self.attribute_read_only,
            places: Vec::new(),
            place_cursor: None,
            bookmarks: self.bookmarks.clone(),
            annotations: self.annotations.clone(),
            writes_bookmarks: false,
            overwrite: false,
            auto_closers: Vec::new(),
            properties: self.properties.clone(),
            indent: self.indent.clone(),
            new_file: self.new_file,
            saved_text: self.saved_text.clone(),
            save: self.save,
        }
    }

    /// Whether the two states are views of one document.
    pub fn shares_document(&self, other: &Self) -> bool {
        self.buffer.shares_document(&other.buffer)
    }

    /// Takes the changes another view of the document already made: no undo step is
    /// added, because the shared history has it, and the carets and folds here follow the
    /// text. A view that was away for too long cannot move its own ranges and drops them
    /// instead, so nothing points past the text.
    pub fn follow(&mut self) -> Followed {
        match self.buffer.follow() {
            Followed::Nothing => Followed::Nothing,
            Followed::Changes(change) => {
                self.finish_with(change.clone(), false);
                Followed::Changes(change)
            }
            Followed::Missed => {
                self.forget_ranges();
                Followed::Missed
            }
        }
    }

    /// Whether the window takes the bookmarks of the file from this view. A second view
    /// of the document reads them without writing, so a cleared list of one view never
    /// erases the bookmarks of the file.
    pub(crate) fn writes_bookmarks(&self) -> bool {
        self.writes_bookmarks
    }

    /// Says whether this view writes the bookmarks of the file into the window list.
    pub(crate) fn set_writes_bookmarks(&mut self, writes: bool) {
        self.writes_bookmarks = writes;
    }

    /// Takes the changes of the other views of the document into the carets, folds and
    /// ranges of this state, before a command counts its edits from them. A view above
    /// follows through [`crate::EditorView::catch_up`], which also moves the ranges the
    /// view keeps itself; a bare state used without a view gets back what it followed.
    fn catch_up(&mut self) -> Followed {
        self.follow()
    }

    /// Whether another view changed the document since this view last followed it.
    pub(crate) fn has_unseen_changes(&self) -> bool {
        self.buffer.has_unseen_changes()
    }

    /// Everything this view keeps as offsets into the text goes away, because the edits
    /// that would have moved it are not known. The parser and the language fill it again.
    fn forget_ranges(&mut self) {
        self.map.folds.clear();
        self.map.folds.set_candidates(Vec::new());
        self.map.inlays.set(Vec::new());
        self.map.set_blocks(Vec::new());
        self.map.set_doc_comments(Vec::new());
        self.spans.clear();
        self.tokens.clear();
        self.bookmarks.clear();
        self.auto_closers.clear();
        self.places.clear();
        self.grown.clear();
        self.marked = None;
        self.goal = None;
        self.parsed = None;
        self.opened_folds = false;
    }

    /// The buffer lines the display shows, without the ones a fold hides.
    pub fn visible_lines(&self) -> Vec<u32> {
        self.map
            .snapshot_owned(self.buffer.store())
            .visible_lines()
            .to_vec()
    }

    /// Reads and decodes the file, and reads the `.editorconfig` files around it. Blocks:
    /// not for the UI thread.
    pub fn load(path: &Path, language: Option<LanguageConfig>) -> Result<Self, LoadError> {
        let decoded = read_file(path).map_err(|error| LoadError(error.to_string()))?;
        let properties = text_core::for_file(path);
        let mut state = Self::new(&decoded.text, decoded.format, language, !decoded.lossless);
        let unit = state
            .language
            .as_ref()
            .map_or("    ", |language| language.indent_unit);
        state.indent = text_core::IndentSettings::resolve(&properties, &decoded.text, unit);
        state.map.tab_size = state.indent.tab_width;
        state.properties = properties;
        state.new_file = decoded.text.is_empty();
        state.attribute_read_only = std::fs::metadata(path)
            .map(|metadata| metadata.permissions().readonly())
            .unwrap_or(false);
        Ok(state)
    }

    /// Writes the text as it was at `snapshot`. Blocks: not for the UI thread.
    pub fn save_snapshot(path: &Path, snapshot: (String, text_core::FileFormat, u64)) -> Saved {
        let (text, format, version) = snapshot;
        let result = write_file(path, &text, &format).map_err(|error| error.to_string());
        Saved {
            version,
            text,
            format,
            result,
        }
    }

    pub fn snapshot_for_save(&self) -> (String, text_core::FileFormat, u64) {
        let mut format = self.buffer.format();
        // The text rules of `.editorconfig` went into the buffer in `prepare_save`; here
        // only the encoding and the line ends of the file are left, so the disk and the
        // buffer hold the same text.
        let properties = text_core::Properties {
            trim_trailing_whitespace: None,
            insert_final_newline: None,
            ..self.properties.clone()
        };
        let text =
            text_core::apply_save(&self.buffer.text(), &properties, self.new_file, &mut format);
        (text, format, self.buffer.version())
    }

    /// What `.editorconfig` asks of a save, put into the buffer as one undo step: the
    /// trailing whitespace of every line without a caret, as IDEA keeps the line being
    /// typed, and the final newline.
    pub fn prepare_save(&mut self) -> Option<Change> {
        self.catch_up();
        if self.is_read_only() {
            return None;
        }
        let text = self.buffer.text();
        let mut edits = Vec::new();
        let trim = match self.properties.trim_trailing_whitespace {
            Some(trim) => trim,
            None => self.save.trim_trailing != TrimTrailing::None,
        };
        if trim {
            let carets: Vec<usize> = self
                .buffer
                .selections()
                .iter()
                .map(|selection| selection.head)
                .collect();
            let modified: Vec<bool> = match (
                self.properties.trim_trailing_whitespace,
                self.save.trim_trailing,
            ) {
                (None, TrimTrailing::Modified) => modified_lines(&text, &self.saved_text),
                _ => Vec::new(),
            };
            let mut start = 0;
            for (index, line) in text.split('\n').enumerate() {
                let end = start + line.len();
                let kept = start + line.trim_end_matches([' ', '\t']).len();
                let has_caret = carets.iter().any(|caret| (start..=end).contains(caret));
                let wanted = modified.get(index).copied().unwrap_or(true);
                if kept < end && wanted && !has_caret {
                    edits.push(Edit::new(kept..end, ""));
                }
                start = end + 1;
            }
        }
        let final_newline = match self.properties.insert_final_newline {
            Some(value) => Some(value),
            None => self.save.ensure_final_newline.then_some(true),
        };
        match final_newline {
            Some(true) if !text.is_empty() && !text.ends_with('\n') => {
                edits.push(Edit::new(text.len()..text.len(), "\n"));
            }
            Some(false) => {
                let kept = text.trim_end_matches('\n').len();
                if kept < text.len() {
                    edits.retain(|edit| edit.range.end <= kept);
                    edits.push(Edit::new(kept..text.len(), ""));
                }
            }
            _ => {}
        }
        if edits.is_empty() {
            return None;
        }
        let appended = edits
            .last()
            .is_some_and(|edit| edit.range == (text.len()..text.len()));
        let change = self.apply_edits(edits);
        if appended {
            // The final newline goes after a caret at the end of the text: the caret stays
            // on its line instead of moving to the new empty one.
            let end = self.buffer.text().len().saturating_sub(1);
            let selections: Vec<Selection> = self
                .buffer
                .selections()
                .iter()
                .map(|selection| Selection {
                    anchor: selection.anchor.min(end),
                    head: selection.head.min(end),
                })
                .collect();
            self.buffer
                .set_selections(SelectionSet::from_vec(selections));
        }
        change
    }

    pub fn finish_save(&mut self, saved: &Saved) {
        if saved.result.is_ok() && saved.version == self.buffer.version() {
            self.buffer.mark_saved();
            self.saved_text = self.buffer.text();
        }
    }

    /// The file changed on disk and was read again. A clean buffer takes the new text as
    /// one external edit, so undo can take it back. A dirty buffer keeps its edits and
    /// `Err` comes back: the user decides.
    pub fn reload(&mut self, decoded: Decoded) -> Result<Option<Change>, Conflict> {
        if self.is_dirty() {
            // The disk has what the buffer has, for example our own save: nothing to decide.
            return if decoded.text == self.buffer.text() {
                Ok(None)
            } else {
                Err(Conflict)
            };
        }
        Ok(self.take_disk_text(decoded))
    }

    /// The user chose the text on disk over the edits of the buffer. It comes in as one
    /// edit, so undo brings the edits back: nothing is lost.
    pub fn reload_from_disk(&mut self, decoded: Decoded) -> Option<Change> {
        self.take_disk_text(decoded)
    }

    fn take_disk_text(&mut self, decoded: Decoded) -> Option<Change> {
        self.catch_up();
        let edit = difference(&self.buffer.text(), &decoded.text)?;
        let change = self
            .buffer
            .apply(vec![edit], None, EditSource::External, EditKind::Command);
        self.buffer.mark_saved();
        self.saved_text = self.buffer.text();
        self.read_only = !decoded.lossless;
        self.finish_with(change, false)
    }

    pub fn text(&self) -> String {
        self.buffer.text()
    }

    /// The text for reading. A copy, which does not change when the document does.
    pub fn store(&self) -> RopeStore {
        self.buffer.store()
    }

    pub fn snapshot(&self) -> RopeStore {
        self.buffer.snapshot()
    }

    pub fn version(&self) -> u64 {
        self.buffer.version()
    }

    pub fn is_dirty(&self) -> bool {
        self.buffer.is_dirty()
    }

    pub fn is_read_only(&self) -> bool {
        self.read_only || self.attribute_read_only
    }

    /// Follows the read-only attribute of the file on disk.
    pub fn set_read_only(&mut self, read_only: bool) {
        self.attribute_read_only = read_only;
    }

    pub fn language(&self) -> Option<&LanguageConfig> {
        self.language.as_ref()
    }

    pub fn selections(&self) -> &SelectionSet {
        self.buffer.selections()
    }

    pub fn spans(&self) -> &[HighlightSpan] {
        &self.spans
    }

    pub(crate) fn tokens(&self) -> &[TokenSpan] {
        &self.tokens
    }

    pub(crate) fn set_semantic_tokens(&mut self, tokens: Vec<TokenSpan>) {
        self.tokens = tokens;
    }

    pub(crate) fn clear_semantic_tokens(&mut self) {
        self.tokens.clear();
    }

    /// The hints drawn between the characters. They are not part of the text: a copy,
    /// a selection and the caret never include them.
    pub fn set_inlays(&mut self, inlays: Vec<display_map::Inlay>) {
        self.map.inlays.set(inlays);
    }

    pub fn inlays(&self) -> &[display_map::Inlay] {
        self.map.inlays.as_slice()
    }

    /// The rows above lines that hold a label of their own, not text of the buffer.
    pub fn set_blocks(&mut self, blocks: Vec<display_map::Block>) {
        self.map.set_blocks(blocks);
    }

    /// Replaces the labelled rows and keeps the empty rows a view added to line up with
    /// another text.
    pub fn set_labels(&mut self, labels: Vec<display_map::Block>) {
        let mut blocks: Vec<display_map::Block> = self
            .map
            .blocks()
            .iter()
            .filter(|block| block.text.is_empty())
            .cloned()
            .collect();
        blocks.extend(labels);
        self.map.set_blocks(blocks);
    }

    pub fn blocks(&self) -> &[display_map::Block] {
        self.map.blocks()
    }

    /// The documentation comments shown as rendered rows in place of their text.
    pub fn set_doc_comments(&mut self, comments: Vec<display_map::DocComment>) {
        self.map.set_doc_comments(comments);
    }

    pub fn map(&self) -> &DisplayMap {
        &self.map
    }

    pub fn display(&self) -> DisplaySnapshot<'_, RopeStore> {
        self.map.snapshot_owned(self.buffer.store())
    }

    pub fn marked(&self) -> Option<Range<usize>> {
        self.marked.clone()
    }

    /// Line and column of the primary caret, from zero. The column counts characters.
    pub fn caret(&self) -> (usize, usize) {
        let store = self.buffer.store();
        let head = self.buffer.selections().primary().head;
        let line = store.offset_to_line(head);
        (
            line,
            store
                .slice(store.line_to_offset(line)..head)
                .chars()
                .count(),
        )
    }

    /// Puts the caret at a line and a column counted in characters, both clamped to the text.
    pub fn set_caret(&mut self, line: usize, column: usize) {
        let store = self.buffer.store();
        let line = line.min(store.line_count().saturating_sub(1));
        let start = store.line_to_offset(line);
        let text = store.line_text(line);
        let offset = start + column_of_char(&text, column);
        self.buffer
            .set_selections(SelectionSet::single(Selection::caret(offset)));
        self.goal = None;
    }

    /// How many characters the primary selection covers.
    pub fn selected_chars(&self) -> usize {
        let range = self.buffer.selections().primary().range();
        self.buffer.store().slice(range).chars().count()
    }

    /// Lets the status bar change line ends and encoding of the file.
    pub fn set_format(&mut self, format: text_core::FileFormat) {
        self.buffer.set_format(format);
    }

    pub fn format(&self) -> text_core::FileFormat {
        self.buffer.format()
    }

    pub fn encoding_name(&self) -> &'static str {
        self.buffer.encoding_name()
    }

    pub fn eol_name(&self) -> &'static str {
        self.buffer.eol_name()
    }

    /// Takes a finished parse. A parse of an older version is dropped, the spans moved
    /// along with the edits stay on screen until the parse of the latest one arrives.
    pub fn accept_syntax(&mut self, snapshot: SyntaxSnapshot) -> bool {
        if snapshot.version != self.buffer.version() {
            return false;
        }
        self.spans = snapshot.highlights;
        normalize_spans(&mut self.spans, self.buffer.store().len());
        self.parsed = Some(Parsed {
            version: snapshot.version,
            tree: snapshot.tree,
            indents: snapshot.indents,
        });
        let folds: Vec<_> = snapshot
            .folds
            .iter()
            .map(|fold| Fold {
                range: fold.range.clone(),
                placeholder: fold.placeholder.to_owned(),
            })
            .collect();
        if !self.opened_folds {
            self.opened_folds = true;
            let collapsed = snapshot
                .folds
                .iter()
                .zip(&folds)
                .filter(|(fold, _)| fold.collapsed_on_open);
            for (_, fold) in collapsed {
                self.map.folds.collapse(fold.clone());
            }
        }
        self.map.folds.set_candidates(folds);
        true
    }

    fn finish(&mut self, change: Change) -> Option<Change> {
        self.finish_with(change, true)
    }

    fn finish_with(&mut self, change: Change, by_user: bool) -> Option<Change> {
        if change.is_empty() {
            return None;
        }
        for step in &change.steps {
            self.map.apply_edits(step);
            shift_spans(&mut self.spans, step);
            shift_tokens(&mut self.tokens, step);
            for place in &mut self.places {
                *place = transform_offset(*place, step);
            }
            for (at, _) in &mut self.bookmarks {
                *at = transform_offset(*at, step);
            }
            if let Some(notes) = &mut self.annotations {
                for (at, _) in notes.iter_mut() {
                    *at = shift_annotation(*at, step);
                }
            }
            self.auto_closers
                .retain(|at| !step.iter().any(|edit| edit.range.contains(at)));
            for at in &mut self.auto_closers {
                *at = transform_offset(*at, step);
            }
        }
        normalize_spans(&mut self.spans, self.buffer.store().len());
        normalize_tokens(&mut self.tokens, self.buffer.store().len());
        if let Some(notes) = &mut self.annotations {
            notes.dedup_by_key(|(at, _)| *at);
        }
        if by_user {
            self.remember_place();
        }
        self.goal = None;
        self.grown.clear();
        self.marked = None;
        self.reveal();
        Some(change)
    }

    fn remember_place(&mut self) {
        let store = self.buffer.store();
        let head = self.buffer.selections().primary().head;
        let line = store.offset_to_line(head);
        let same_line = self
            .places
            .last()
            .is_some_and(|last| store.offset_to_line((*last).min(store.len())) == line);
        if same_line {
            self.places.pop();
        }
        self.places.push(head);
        let extra = self.places.len().saturating_sub(PLACES);
        self.places.drain(..extra);
        self.place_cursor = None;
    }

    /// Ctrl+Shift+Backspace: the place of the latest edit, and with every press an older one.
    fn jump_to_last_edit(&mut self) {
        let Some(newest) = self.places.len().checked_sub(1) else {
            return;
        };
        let store = self.buffer.store();
        let here = store.offset_to_line(self.buffer.selections().primary().head);
        let line_of = |place: usize| store.offset_to_line(place.min(store.len()));
        let next = match self.place_cursor {
            Some(at) => at.saturating_sub(1),
            None if newest > 0 && line_of(self.places[newest]) == here => newest - 1,
            None => newest,
        };
        let place = self.places[next].min(store.len());
        self.place_cursor = Some(next);
        self.select_range(place..place);
    }

    /// No caret stays hidden in a fold.
    fn reveal(&mut self) {
        let heads: Vec<_> = self
            .buffer
            .selections()
            .iter()
            .map(|selection| selection.head)
            .collect();
        let hidden: Vec<_> = heads
            .into_iter()
            .filter(|head| {
                self.map
                    .folds
                    .as_slice()
                    .iter()
                    .any(|fold| fold.range.start < *head && *head < fold.range.end)
            })
            .collect();
        for head in hidden {
            self.map.folds.unfold_at(head);
        }
    }

    fn select(&mut self, selections: SelectionSet) {
        self.buffer.set_selections(selections);
        self.reveal();
    }

    /// Runs an action by its keymap name. Returns the change to hand to the parser.
    pub fn perform(&mut self, action: &str, page_rows: usize) -> Option<Change> {
        self.catch_up();
        let keeps_goal = matches!(
            action,
            actions::MOVE_UP
                | actions::MOVE_DOWN
                | actions::SELECT_UP
                | actions::SELECT_DOWN
                | actions::PAGE_UP
                | actions::PAGE_DOWN
        );
        if !keeps_goal {
            self.goal = None;
        }
        if !matches!(
            action,
            actions::EXTEND_SELECTION | actions::SHRINK_SELECTION
        ) {
            self.grown.clear();
        }
        if let Some(level) = actions::EXPAND_TO_LEVELS
            .iter()
            .position(|name| *name == action)
        {
            self.expand_to_level(level + 1, true);
            return None;
        }
        if let Some(level) = actions::EXPAND_ALL_TO_LEVELS
            .iter()
            .position(|name| *name == action)
        {
            self.expand_to_level(level + 1, false);
            return None;
        }
        match action {
            actions::MOVE_LEFT => self.move_horizontally(false, false),
            actions::MOVE_RIGHT => self.move_horizontally(true, false),
            actions::SELECT_LEFT => self.move_horizontally(false, true),
            actions::SELECT_RIGHT => self.move_horizontally(true, true),
            actions::MOVE_UP => self.move_rows(-1, false),
            actions::MOVE_DOWN => self.move_rows(1, false),
            actions::SELECT_UP => self.move_rows(-1, true),
            actions::SELECT_DOWN => self.move_rows(1, true),
            actions::PAGE_UP => self.move_rows(-(page_rows as isize), false),
            actions::PAGE_DOWN => self.move_rows(page_rows as isize, false),
            actions::MOVE_WORD_LEFT => self.move_heads(false, commands::previous_word),
            actions::MOVE_WORD_RIGHT => self.move_heads(false, commands::next_word),
            actions::SELECT_WORD_LEFT => self.move_heads(true, commands::previous_word),
            actions::SELECT_WORD_RIGHT => self.move_heads(true, commands::next_word),
            actions::MOVE_LINE_START => self.move_line_edge(false, false),
            actions::MOVE_LINE_END => self.move_line_edge(true, false),
            actions::SELECT_LINE_START => self.move_line_edge(false, true),
            actions::SELECT_LINE_END => self.move_line_edge(true, true),
            actions::MOVE_FILE_START => self.move_heads(false, |_, _| 0),
            actions::MOVE_FILE_END => self.move_heads(false, |store, _| store.len()),
            actions::SELECT_FILE_START => self.move_heads(true, |_, _| 0),
            actions::SELECT_FILE_END => self.move_heads(true, |store, _| store.len()),
            actions::SELECT_ALL => {
                self.map.folds.clear();
                let all = Selection::new(0, self.buffer.store().len());
                self.buffer.set_selections(SelectionSet::single(all));
            }
            actions::SELECT_NEXT_OCCURRENCE => {
                commands::select_next_occurrence(&mut self.buffer);
                self.reveal();
            }
            actions::UNSELECT_LAST_OCCURRENCE => self.unselect_last_occurrence(),
            actions::EXTEND_SELECTION => self.extend_selection(),
            actions::SHRINK_SELECTION => {
                if let Some(selections) = self.grown.pop() {
                    self.buffer.set_selections(selections);
                }
            }
            actions::CLEAR_CARETS => self.clear_carets(),
            actions::FOLD => self.fold_at_caret(),
            actions::UNFOLD => self.unfold_at_caret(),
            actions::MATCH_BRACKET => self.jump_to_bracket(),
            actions::SELECT_ALL_OCCURRENCES => {
                editing::select_all_occurrences(&mut self.buffer);
                self.reveal();
            }
            actions::LAST_EDIT => self.jump_to_last_edit(),
            actions::FOLD_ALL => self.fold_all(),
            actions::UNFOLD_ALL => self.map.folds.clear(),
            actions::FOLD_SELECTION => self.fold_selection(),
            actions::COLLAPSE_BLOCK => self.collapse_block(),
            actions::COLLAPSE_REGION_RECURSIVELY => {
                self.map
                    .folds
                    .collapse_recursive(self.buffer.selections().primary().head);
                self.settle_carets_into_folds();
            }
            actions::EXPAND_REGION_RECURSIVELY => {
                self.map
                    .folds
                    .expand_recursive(self.buffer.selections().primary().head);
            }
            actions::PAGE_UP_WITH_SELECTION => self.move_rows(-(page_rows as isize), true),
            actions::PAGE_DOWN_WITH_SELECTION => self.move_rows(page_rows as isize, true),
            actions::ADD_CARET_PER_SELECTED_LINE => self.add_caret_per_selected_line(),
            actions::TOGGLE_INSERT_STATE => self.overwrite = !self.overwrite,
            _ if self.is_read_only() => {}
            actions::SORT_LINES => {
                return self.edit(|buffer| editing::sort_lines(buffer, EditSource::User));
            }
            actions::REVERSE_LINES => {
                return self.edit(|buffer| editing::reverse_lines(buffer, EditSource::User));
            }
            actions::CONVERT_INDENTS_TO_SPACES => {
                let width = self.indent.tab_width;
                return self.edit(|buffer| {
                    editing::convert_indents(buffer, false, width, EditSource::User)
                });
            }
            actions::CONVERT_INDENTS_TO_TABS => {
                let width = self.indent.tab_width;
                return self.edit(|buffer| {
                    editing::convert_indents(buffer, true, width, EditSource::User)
                });
            }
            actions::INDENT_SELECTION => return self.tab(),
            actions::UNDO => {
                let change = self.buffer.undo()?;
                return self.finish(change);
            }
            actions::REDO => {
                let change = self.buffer.redo()?;
                return self.finish(change);
            }
            actions::BACKSPACE => {
                return self.edit(|buffer| commands::backspace(buffer, EditSource::User));
            }
            actions::DELETE => {
                return self.edit(|buffer| commands::delete_forward(buffer, EditSource::User));
            }
            actions::NEWLINE => return self.newline(),
            actions::TAB => return self.tab(),
            actions::DUPLICATE => {
                return self.edit(|buffer| commands::duplicate(buffer, EditSource::User));
            }
            actions::DELETE_LINE => {
                return self.edit(|buffer| commands::delete_lines(buffer, EditSource::User));
            }
            actions::MOVE_LINE_UP => {
                return self.edit(|buffer| commands::move_lines(buffer, false, EditSource::User));
            }
            actions::MOVE_LINE_DOWN => {
                return self.edit(|buffer| commands::move_lines(buffer, true, EditSource::User));
            }
            actions::TOGGLE_COMMENT => return self.toggle_comment(),
            actions::BLOCK_COMMENT => {
                let marks = self.language.as_ref()?.block_comment?;
                return self
                    .edit(|buffer| editing::toggle_block_comment(buffer, marks, EditSource::User));
            }
            actions::UNINDENT => {
                let unit = self.indent_unit().to_owned();
                return self.edit(|buffer| editing::unindent(buffer, &unit, EditSource::User));
            }
            actions::AUTO_INDENT => return self.auto_indent(),
            actions::COMPLETE_STATEMENT => return self.complete_statement(),
            actions::START_LINE => return self.start_line(false),
            actions::START_LINE_ABOVE => return self.start_line(true),
            actions::JOIN_LINES => {
                return self.edit(|buffer| editing::join_lines(buffer, EditSource::User));
            }
            actions::TOGGLE_CASE => {
                return self.edit(|buffer| editing::toggle_case(buffer, EditSource::User));
            }
            actions::DELETE_WORD_BACK => {
                return self.edit(|buffer| editing::delete_word(buffer, false, EditSource::User));
            }
            actions::DELETE_WORD_FORWARD => {
                return self.edit(|buffer| editing::delete_word(buffer, true, EditSource::User));
            }
            actions::MOVE_STATEMENT_UP => return self.move_statement(false),
            actions::MOVE_STATEMENT_DOWN => return self.move_statement(true),
            actions::MOVE_ELEMENT_LEFT => return self.move_element(false),
            actions::MOVE_ELEMENT_RIGHT => return self.move_element(true),
            actions::CODE_BLOCK_START => self.code_block_step(false, false),
            actions::CODE_BLOCK_END => self.code_block_step(true, false),
            actions::CODE_BLOCK_START_WITH_SELECTION => self.code_block_step(false, true),
            actions::CODE_BLOCK_END_WITH_SELECTION => self.code_block_step(true, true),
            actions::SPLIT_LINE => return self.split_line(),
            _ => {}
        }
        None
    }

    fn edit(&mut self, command: impl FnOnce(&mut Buffer) -> Change) -> Option<Change> {
        self.catch_up();
        let change = command(&mut self.buffer);
        self.finish(change)
    }

    pub fn type_text(&mut self, text: &str) -> Option<Change> {
        self.catch_up();
        if self.is_read_only() || text.is_empty() {
            return None;
        }
        let text = normalize_newlines(text);
        if self.overwrite && text.chars().count() == 1 && !text.contains('\n') {
            let (edits, after) = self.overwrite_edits(&text);
            return self.edit(|buffer| {
                buffer.apply(edits, Some(after), EditSource::User, EditKind::Typing)
            });
        }
        self.edit(|buffer| commands::insert(buffer, &text, EditSource::User))
    }

    /// Ctrl+V: like typing, but the lines after the first take the indent of the caret
    /// line, and the indentation of the pasted block keeps its own shape above that.
    pub fn paste(&mut self, text: &str) -> Option<Change> {
        self.catch_up();
        if self.is_read_only() || text.is_empty() || !text.contains(['\n', '\r']) {
            return self.type_text(text);
        }
        let text = normalize_newlines(text);
        let store = self.buffer.snapshot();
        let head = self.buffer.selections().primary().head;
        let base = line_indent(&store, head);
        let pasted = reindent_paste(&text, &base);
        self.edit(|buffer| commands::insert(buffer, &pasted, EditSource::User))
    }

    fn indent_unit(&self) -> &str {
        &self.indent.unit
    }

    /// The indentation of this file, for Enter, Tab and the status bar.
    pub fn indent(&self) -> &text_core::IndentSettings {
        &self.indent
    }

    /// The width a tab takes on screen, for the display map and the indent guides.
    pub fn tab_width(&self) -> usize {
        self.indent.tab_width
    }

    /// The columns between two indent guides: the size of a step, or the width of a tab
    /// when the file indents with tabs.
    pub fn indent_columns(&self) -> usize {
        if self.indent.uses_tabs() {
            self.indent.tab_width
        } else {
            self.indent.size
        }
    }

    /// Picks the indentation of this open file until it closes. `None` keeps the part
    /// the user did not name.
    pub fn set_indent(&mut self, tabs: Option<bool>, size: Option<usize>) {
        let tabs = tabs.unwrap_or(self.indent.uses_tabs());
        let size = size.unwrap_or(self.indent.size).max(1);
        let tab_width = if tabs { size } else { self.indent.tab_width };
        self.indent = text_core::IndentSettings::manual(tabs, size, tab_width);
        self.map.tab_size = self.indent.tab_width;
    }

    /// The indentation the formatter of the language gets: `(tab_size, insert_spaces)`.
    pub fn formatting_options(&self) -> (u32, bool) {
        (self.indent.tab_width as u32, !self.indent.uses_tabs())
    }

    /// The save rules of the window, where `.editorconfig` says nothing.
    pub fn set_save_options(&mut self, save: SaveOptions) {
        self.save = save;
    }

    /// The Code Style defaults of the window, for a file that neither `.editorconfig`
    /// nor its own indentation names a rule for. The tab size reaches every file that
    /// `.editorconfig` does not own; the step changes only a file without an indentation
    /// of its own.
    pub fn apply_indent_defaults(&mut self, tabs: bool, size: usize, tab_size: usize) {
        if self.indent.source == text_core::IndentSource::EditorConfig {
            return;
        }
        if self.indent.source == text_core::IndentSource::Language {
            self.indent = text_core::IndentSettings::new(
                tabs,
                size,
                tab_size,
                text_core::IndentSource::Language,
            );
        } else {
            self.indent.tab_width = tab_size.max(1);
        }
        self.map.tab_size = self.indent.tab_width;
    }

    /// Shift+Enter and Ctrl+Alt+Enter.
    fn start_line(&mut self, above: bool) -> Option<Change> {
        let (parsed, unit) = self.take_parsed();
        let change = editing::start_line(
            &mut self.buffer,
            above,
            |store, offset| indent_for(store, parsed.as_ref(), offset, &unit),
            EditSource::User,
        );
        self.parsed = parsed;
        self.finish(change)
    }

    /// Ctrl+Alt+I: the indent of a line is that of the line above, one unit deeper after a
    /// line that opens a block and one less on a line that starts with a closing bracket.
    fn auto_indent(&mut self) -> Option<Change> {
        let (parsed, unit) = self.take_parsed();
        let change = editing::reindent(
            &mut self.buffer,
            |store, line| {
                if line == 0 {
                    return String::new();
                }
                let mut above = line - 1;
                while above > 0 && store.line_text(above).trim().is_empty() {
                    above -= 1;
                }
                let mut indent = indent_for(store, parsed.as_ref(), store.line_end(above), &unit);
                let closes = store
                    .line_text(line)
                    .trim_start()
                    .starts_with(['}', ')', ']']);
                if closes && indent.ends_with(&unit) {
                    indent.truncate(indent.len() - unit.len());
                }
                indent
            },
            EditSource::User,
        );
        self.parsed = parsed;
        self.finish(change)
    }

    /// The parse of the current text, taken out so that a command can borrow the buffer.
    fn take_parsed(&mut self) -> (Option<Parsed>, String) {
        let version = self.buffer.version();
        let parsed = self
            .parsed
            .take()
            .filter(|parsed| parsed.version == version);
        (parsed, self.indent.unit.clone())
    }

    /// Ctrl+Shift+Enter, by the rules of the language.
    fn complete_statement(&mut self) -> Option<Change> {
        let rules = self.language.as_ref()?.statements?;
        let unit = self.indent_unit().to_owned();
        self.edit(|buffer| {
            editing::complete_statement(
                buffer,
                rules.terminator,
                rules.block_words,
                &unit,
                EditSource::User,
            )
        })
    }

    /// Collapses the regions that hold no other region, so the structure stays in sight.
    fn fold_all(&mut self) {
        let candidates = self.map.folds.candidates().to_vec();
        let leaves = candidates.iter().filter(|fold| {
            !candidates.iter().any(|other| {
                other.range != fold.range
                    && fold.range.start <= other.range.start
                    && other.range.end <= fold.range.end
            })
        });
        for fold in leaves.cloned().collect::<Vec<_>>() {
            self.map.folds.collapse(fold);
        }
        let moved = self.buffer.selections().map(|selection| {
            let hidden =
                self.map.folds.as_slice().iter().find(|fold| {
                    fold.range.start < selection.head && selection.head < fold.range.end
                });
            hidden.map_or(*selection, |fold| Selection::caret(fold.range.start))
        });
        self.buffer.set_selections(moved);
    }

    /// Ctrl+.: the selected lines fold into `...`.
    fn fold_selection(&mut self) {
        let selection = self.buffer.selections().primary();
        if selection.is_empty() {
            return;
        }
        let store = self.buffer.store();
        let start = selection.start();
        let end = selection.end();
        if store.offset_to_line(start) == store.offset_to_line(end) {
            return;
        }
        self.collapse(Fold {
            range: start..end,
            placeholder: "...".to_owned(),
        });
    }

    /// F11: a bookmark on the caret line, or none when it has one. With a mnemonic the
    /// bookmark takes it from the one that had it.
    pub fn toggle_bookmark(&mut self, mnemonic: Option<char>) {
        let store = self.buffer.store();
        let line = store.offset_to_line(self.buffer.selections().primary().head);
        let line_of = |at: usize| store.offset_to_line(at.min(store.len()));
        let here = self
            .bookmarks
            .iter()
            .position(|(at, _)| line_of(*at) == line);
        let start = store.line_to_offset(line);
        match (here, mnemonic) {
            (Some(index), None) => {
                self.bookmarks.remove(index);
            }
            (Some(index), Some(letter)) if self.bookmarks[index].1 == Some(letter) => {
                self.bookmarks.remove(index);
            }
            (Some(index), Some(letter)) => {
                self.bookmarks[index].1 = Some(letter);
                self.drop_mnemonic(letter, Some(index));
            }
            (None, mnemonic) => {
                self.bookmarks.push((start, mnemonic));
                if let Some(letter) = mnemonic {
                    let kept = self.bookmarks.len() - 1;
                    self.drop_mnemonic(letter, Some(kept));
                }
            }
        }
    }

    /// A mnemonic is on one bookmark only.
    fn drop_mnemonic(&mut self, letter: char, keep: Option<usize>) {
        for (index, (_, mnemonic)) in self.bookmarks.iter_mut().enumerate() {
            if *mnemonic == Some(letter) && Some(index) != keep {
                *mnemonic = None;
            }
        }
    }

    /// The bookmarks as lines from zero, in order.
    pub fn bookmarks(&self) -> Vec<(u32, Option<char>)> {
        let store = self.buffer.store();
        let mut lines: Vec<_> = self
            .bookmarks
            .iter()
            .map(|(at, mnemonic)| {
                (
                    store.offset_to_line((*at).min(store.len())) as u32,
                    *mnemonic,
                )
            })
            .collect();
        lines.sort_by_key(|(line, _)| *line);
        lines.dedup_by_key(|(line, _)| *line);
        lines
    }

    pub fn set_bookmarks(&mut self, lines: &[(u32, Option<char>)]) {
        let store = self.buffer.store();
        self.bookmarks = lines
            .iter()
            .filter(|(line, _)| (*line as usize) < store.line_count())
            .map(|(line, mnemonic)| (store.line_to_offset(*line as usize), *mnemonic))
            .collect();
    }

    /// `Some(None)` for a bookmark without mnemonic.
    pub fn bookmark_at_line(&self, line: usize) -> Option<Option<char>> {
        let store = self.buffer.store();
        self.bookmarks
            .iter()
            .find(|(at, _)| store.offset_to_line((*at).min(store.len())) == line)
            .map(|(_, mnemonic)| *mnemonic)
    }

    pub fn bookmark_line(&self, mnemonic: char) -> Option<usize> {
        let store = self.buffer.store();
        let found = self.bookmarks.iter().find(|(_, m)| *m == Some(mnemonic));
        found.map(|(at, _)| store.offset_to_line((*at).min(store.len())))
    }

    /// Puts the blame of the lines in the column before the text. The lines are counted
    /// from one; one past the end of the text is left out.
    pub fn set_annotations(&mut self, lines: &[(u32, BlameNote)]) {
        let store = self.buffer.store();
        let count = store.line_count();
        let mut notes: Vec<(usize, BlameNote)> = lines
            .iter()
            .filter(|(line, _)| (*line as usize) >= 1 && (*line as usize) <= count)
            .map(|(line, note)| (store.line_to_offset(*line as usize - 1), note.clone()))
            .collect();
        notes.sort_by_key(|(at, _)| *at);
        notes.dedup_by_key(|(at, _)| *at);
        self.annotations = Some(notes);
    }

    /// Takes the blame column away.
    pub fn clear_annotations(&mut self) {
        self.annotations = None;
    }

    /// Whether the blame column is on.
    pub fn has_annotations(&self) -> bool {
        self.annotations.is_some()
    }

    /// The blame of `line`, whichever line of its group it is.
    fn annotation_of_line(&self, line: usize) -> Option<&BlameNote> {
        let notes = self.annotations.as_deref()?;
        let store = self.buffer.store();
        if line >= store.line_count() {
            return None;
        }
        let start = store.line_to_offset(line);
        let index = notes.partition_point(|(at, _)| *at <= start);
        let (at, note) = notes.get(index.checked_sub(1)?)?;
        (store.offset_to_line(*at) == line).then_some(note)
    }

    /// The blame whose label the column paints on `line`: the first line of a run of one
    /// commit carries the label of the whole run.
    pub fn annotation_label_at(&self, line: usize) -> Option<&BlameNote> {
        let note = self.annotation_of_line(line)?;
        let follows = line > 0
            && self
                .annotation_of_line(line - 1)
                .is_some_and(|previous| previous.commit == note.commit);
        (!follows).then_some(note)
    }

    /// The painted labels with their line from zero and their commit, for a test.
    pub fn annotation_labels(&self) -> Vec<(u32, String, String)> {
        let Some(notes) = &self.annotations else {
            return Vec::new();
        };
        let store = self.buffer.store();
        let mut labels = Vec::new();
        let mut previous: Option<(usize, &BlameNote)> = None;
        for (at, note) in notes {
            let line = store.offset_to_line((*at).min(store.len()));
            let follows = previous.is_some_and(|(before, previous)| {
                before + 1 == line && previous.commit == note.commit
            });
            if !follows {
                labels.push((line as u32, note.label.clone(), note.commit.clone()));
            }
            previous = Some((line, note));
        }
        labels
    }

    /// Soft wrap at this many columns, `None` for no wrap.
    pub fn set_wrap(&mut self, columns: Option<usize>) {
        self.map.set_wrap(columns);
    }

    pub fn wrap(&self) -> Option<usize> {
        self.map.wrap()
    }

    fn jump_to_bracket(&mut self) {
        let store = self.buffer.snapshot();
        let head = self.buffer.selections().primary().head;
        // The bracket next to the caret first, as before. A caret in the middle of a pair
        // goes to the opening bracket that surrounds it, as in IDEA.
        let pair = editing::bracket_pair(&store, head)
            .or_else(|| editing::enclosing_bracket_pair(&store, head));
        let Some((open, close)) = pair else {
            return;
        };
        let target = if head <= open + 1 { close + 1 } else { open };
        self.select_range(target..target);
    }

    /// The offsets of the brackets next to the caret and their partners, for highlighting.
    pub fn bracket_pair(&self) -> Option<(usize, usize)> {
        let selections = self.buffer.selections();
        (selections.len() == 1 && selections.primary().is_empty())
            .then(|| editing::bracket_pair(&self.buffer.store(), selections.primary().head))
            .flatten()
    }

    /// The word at a lone caret, for highlighting its other occurrences.
    pub fn caret_word(&self) -> Option<Range<usize>> {
        let selections = self.buffer.selections();
        if selections.len() != 1 || !selections.primary().is_empty() {
            return None;
        }
        let word = commands::word_at(&self.buffer.store(), selections.primary().head);
        (!word.is_empty()).then_some(word)
    }

    /// Ctrl+Enter: breaks the line at the caret and leaves the caret where it is.
    fn split_line(&mut self) -> Option<Change> {
        let head = self.buffer.selections().primary().head;
        let selections = self.buffer.selections().clone();
        let change = self.buffer.apply(
            vec![Edit::insert(head, "\n")],
            Some(selections),
            EditSource::User,
            EditKind::Command,
        );
        self.finish(change)
    }

    /// Ctrl+[ and Ctrl+], with Shift for a selection: the start or the end of the
    /// innermost block around each caret.
    fn code_block_step(&mut self, end: bool, extend: bool) {
        let store = self.buffer.snapshot();
        let text = store.text();
        let pairs = brace_pairs(&text);
        let moved = self.buffer.selections().map(|selection| {
            let Some((open, close)) = enclosing_block(&pairs, selection.head) else {
                return *selection;
            };
            let target = if end { close } else { open };
            if extend {
                Selection::new(selection.anchor, target)
            } else {
                Selection::caret(target)
            }
        });
        self.buffer.set_selections(moved);
        self.reveal();
    }

    /// Ctrl+Shift+Up and Ctrl+Shift+Down: the statement, member or whole block moves
    /// past its neighbour and never leaves its own block.
    fn move_statement(&mut self, down: bool) -> Option<Change> {
        let store = self.buffer.snapshot();
        let text = store.text();
        let pairs = brace_pairs(&text);
        let selections = self.buffer.selections().clone();
        let primary = selections.primary().range();
        let block = containing_block(&pairs, primary.clone())?;
        let units = units_in(&text, &pairs, block);
        if units.is_empty() {
            return None;
        }
        let touches = |unit: &Range<usize>| {
            if primary.start == primary.end {
                unit.start <= primary.start
                    && (primary.start < unit.end || primary.start == text.len())
            } else {
                unit.start < primary.end && primary.start < unit.end
            }
        };
        let first = units.iter().position(touches)?;
        let last = units.iter().rposition(touches)?;
        let moving_start = units[first].start;
        let moving_end = units[last].end;
        let (other, replacement, shift) = if down {
            let next = units.get(last + 1)?;
            let between = &text[moving_end..next.start];
            let moving = &text[moving_start..moving_end];
            let other = &text[next.start..next.end];
            (
                next.clone(),
                format!("{other}{between}{moving}"),
                (other.len() + between.len()) as isize,
            )
        } else {
            let previous = units.get(first.checked_sub(1)?)?;
            let between = &text[previous.end..moving_start];
            let moving = &text[moving_start..moving_end];
            let other = &text[previous.start..previous.end];
            (
                previous.clone(),
                format!("{moving}{between}{other}"),
                -((other.len() + between.len()) as isize),
            )
        };
        let range = if down {
            moving_start..other.end
        } else {
            other.start..moving_end
        };
        let shift_offset = |at: usize| {
            if moving_start <= at && at <= moving_end {
                (at as isize + shift) as usize
            } else {
                at
            }
        };
        let after = selections.map(|selection| {
            Selection::new(shift_offset(selection.anchor), shift_offset(selection.head))
        });
        let change = self.buffer.apply(
            vec![Edit::new(range, replacement)],
            Some(after),
            EditSource::User,
            EditKind::Command,
        );
        self.finish(change)
    }

    fn newline(&mut self) -> Option<Change> {
        let (parsed, unit) = self.take_parsed();
        let change = commands::newline(
            &mut self.buffer,
            |store, offset| indent_for(store, parsed.as_ref(), offset, &unit),
            EditSource::User,
        );
        self.parsed = parsed;
        self.finish(change)
    }

    fn tab(&mut self) -> Option<Change> {
        let unit = self.indent.unit.clone();
        let store = self.buffer.snapshot();
        let selections = self.buffer.selections().clone();
        let selected = selections.iter().any(|selection| !selection.is_empty());
        let multiline = selections.iter().any(|selection| {
            store.offset_to_line(selection.start()) != store.offset_to_line(selection.end())
        });
        if !multiline && !selected {
            if unit.starts_with('\t') {
                return self.type_text("\t");
            }
            let head = selections.primary().head;
            let column = head - store.line_to_offset(store.offset_to_line(head));
            let spaces = unit.len() - column % unit.len();
            return self.type_text(&" ".repeat(spaces));
        }
        let mut lines: Vec<usize> = Vec::new();
        for selection in selections.iter() {
            let last = store.offset_to_line(selection.end());
            let last = if last > 0 && store.line_to_offset(last) == selection.end() {
                last - 1
            } else {
                last
            };
            lines.extend(store.offset_to_line(selection.start())..=last);
        }
        lines.sort_unstable();
        lines.dedup();
        let edits = lines
            .into_iter()
            .filter(|line| !store.line_text(*line).is_empty())
            .map(|line| Edit::insert(store.line_to_offset(line), unit.clone()))
            .collect();
        self.edit(|buffer| buffer.apply(edits, None, EditSource::User, EditKind::Command))
    }

    fn toggle_comment(&mut self) -> Option<Change> {
        if let Some(prefix) = self
            .language
            .as_ref()
            .and_then(|language| language.line_comment)
        {
            return self
                .edit(|buffer| commands::toggle_line_comment(buffer, prefix, EditSource::User));
        }
        // A format without a line comment, such as XML, takes the block comment.
        let marks = self.language.as_ref()?.block_comment?;
        self.edit(|buffer| editing::toggle_block_comment(buffer, marks, EditSource::User))
    }

    fn move_heads(&mut self, extend: bool, mut target: impl FnMut(&RopeStore, usize) -> usize) {
        let store = self.buffer.snapshot();
        let moved = self.buffer.selections().map(|selection| {
            let head = target(&store, selection.head);
            if extend {
                Selection::new(selection.anchor, head)
            } else {
                Selection::caret(head)
            }
        });
        self.select(moved);
    }

    fn move_horizontally(&mut self, forward: bool, extend: bool) {
        let store = self.buffer.snapshot();
        let map = &self.map;
        let moved = self.buffer.selections().map(|selection| {
            if !extend && !selection.is_empty() {
                return Selection::caret(if forward {
                    selection.end()
                } else {
                    selection.start()
                });
            }
            let step = if forward {
                store.next_boundary(selection.head)
            } else {
                store.prev_boundary(selection.head)
            };
            let head = skip_folds(map, step, forward);
            if extend {
                Selection::new(selection.anchor, head)
            } else {
                Selection::caret(head)
            }
        });
        self.buffer.set_selections(moved);
    }

    fn move_rows(&mut self, rows: isize, extend: bool) {
        let display = self.map.snapshot_owned(self.buffer.store());
        let primary = display.to_display(self.buffer.selections().primary().head);
        let goal = self
            .goal
            .unwrap_or_else(|| chars_in(&display.line(primary.row).text, primary.column));
        let moved = self.buffer.selections().map(|selection| {
            let point = display.to_display(selection.head);
            let head = match display.text_row(point.row, rows) {
                Some(row) => {
                    let column = column_of_char(&display.line(row).text, goal);
                    display.to_offset(DisplayPoint { row, column }, Bias::Left)
                }
                None if rows < 0 => 0,
                None => self.buffer.store().len(),
            };
            if extend {
                Selection::new(selection.anchor, head)
            } else {
                Selection::caret(head)
            }
        });
        self.goal = Some(goal);
        self.buffer.set_selections(moved);
    }

    /// Home goes to the first non-blank character, then to the column zero.
    fn move_line_edge(&mut self, end: bool, extend: bool) {
        let display = self.map.snapshot_owned(self.buffer.store());
        let store = self.buffer.store();
        let moved = self.buffer.selections().map(|selection| {
            let line = display.line(display.to_display(selection.head).row);
            let head = if end {
                line.end_offset
            } else {
                let text = store.line_text(store.offset_to_line(line.start_offset));
                let first = line.start_offset + (text.len() - text.trim_start().len());
                if selection.head == first {
                    line.start_offset
                } else {
                    first
                }
            };
            if extend {
                Selection::new(selection.anchor, head)
            } else {
                Selection::caret(head)
            }
        });
        self.buffer.set_selections(moved);
    }

    fn unselect_last_occurrence(&mut self) {
        let selections = self.buffer.selections();
        if selections.len() < 2 {
            return;
        }
        let primary = selections.primary();
        let rest: Vec<_> = selections
            .iter()
            .copied()
            .filter(|selection| *selection != primary)
            .collect();
        self.buffer.set_selections(SelectionSet::from_vec(rest));
    }

    fn extend_selection(&mut self) {
        let store = self.buffer.snapshot();
        let version = self.buffer.version();
        let tree = self
            .parsed
            .as_ref()
            .filter(|parsed| parsed.version == version)
            .map(|parsed| &parsed.tree);
        let grown = self.buffer.selections().map(|selection| {
            let range = if selection.is_empty() {
                let word = commands::word_at(&store, selection.head);
                (!word.is_empty()).then_some(word)
            } else {
                None
            };
            let range = range
                .or_else(|| tree.and_then(|tree| enclosing_range(tree, selection.range())))
                .unwrap_or(0..store.len());
            Selection::new(range.start, range.end)
        });
        self.grown.push(self.buffer.selections().clone());
        self.select(grown);
    }

    fn clear_carets(&mut self) {
        let primary = self.buffer.selections().primary();
        self.buffer
            .set_selections(SelectionSet::single(Selection::caret(primary.head)));
    }

    fn fold_at_caret(&mut self) {
        let head = self.buffer.selections().primary().head;
        let line = self.buffer.store().offset_to_line(head);
        // A region header folds the region it names.
        let region = self
            .regions()
            .into_iter()
            .find(|region| region.line == line && !region.fold.is_empty());
        if let Some(region) = region {
            self.collapse(Fold {
                range: region.fold,
                placeholder: "...".to_owned(),
            });
            return;
        }
        let inner = self
            .map
            .folds
            .candidates()
            .iter()
            .filter(|fold| fold.range.start <= head && head <= fold.range.end)
            .min_by_key(|fold| fold.range.len())
            .cloned();
        if let Some(fold) = inner {
            self.collapse(fold);
        }
    }

    fn collapse(&mut self, fold: Fold) {
        let range = fold.range.clone();
        self.map.folds.collapse(fold);
        let moved = self.buffer.selections().map(|selection| {
            if range.start < selection.head && selection.head < range.end {
                Selection::caret(range.start)
            } else {
                *selection
            }
        });
        self.buffer.set_selections(moved);
    }

    fn unfold_at_caret(&mut self) {
        let display = self.map.snapshot_owned(self.buffer.store());
        let line = display.line(
            display
                .to_display(self.buffer.selections().primary().head)
                .row,
        );
        for (_, chunk) in line.chunks {
            if let DisplayChunk::Placeholder { fold_start, .. } = chunk {
                self.map.folds.unfold_at(fold_start);
            }
        }
    }

    /// The fold button in the gutter.
    pub fn toggle_fold_at_line(&mut self, line: usize) {
        let collapsed_before = self.map.folds.as_slice().len();
        self.map.folds.toggle_at_line(&self.buffer.store(), line);
        if self.map.folds.as_slice().len() > collapsed_before {
            let fold = self.map.folds.as_slice().iter().find(|fold| {
                let store = self.buffer.store();
                store.offset_to_line(fold.range.start) == line
            });
            if let Some(fold) = fold.cloned() {
                self.collapse(fold);
            }
        }
    }

    pub fn expand_fold(&mut self, offset: usize) {
        self.map.folds.unfold_at(offset);
    }

    /// Insert replaces the character under the caret; the status bar says which mode.
    pub fn overwrite(&self) -> bool {
        self.overwrite
    }

    /// One typed character in overwrite mode: a selection is replaced, a caret replaces
    /// the character after it unless that is the end of the line. The carets land after
    /// the typed character, counted in the text after the edits.
    fn overwrite_edits(&self, text: &str) -> (Vec<Edit>, SelectionSet) {
        let store = self.buffer.snapshot();
        let mut ranges: Vec<Range<usize>> = self
            .buffer
            .selections()
            .iter()
            .map(|selection| {
                if !selection.is_empty() {
                    return selection.range();
                }
                let head = selection.head;
                match store.char_at(head) {
                    Some(c) if c != '\n' && c != '\r' => head..store.next_boundary(head),
                    _ => head..head,
                }
            })
            .collect();
        ranges.sort_by_key(|range| range.start);
        let mut shift = 0isize;
        let mut edits = Vec::with_capacity(ranges.len());
        let mut after = Vec::with_capacity(ranges.len());
        for range in ranges {
            let caret = (range.start as isize + shift) as usize + text.len();
            shift += text.len() as isize - range.len() as isize;
            edits.push(Edit::new(range, text));
            after.push(Selection::caret(caret));
        }
        (edits, SelectionSet::from_vec(after))
    }

    /// Ctrl+Shift+.: folds the `{}` block at the caret, its braces stay visible.
    fn collapse_block(&mut self) {
        let store = self.buffer.snapshot();
        let text = store.text();
        let head = self.buffer.selections().primary().head;
        let pairs = brace_pairs(&text);
        let line = store.offset_to_line(head);
        let pair = enclosing_block(&pairs, head).or_else(|| {
            pairs
                .iter()
                .filter(|(open, _)| store.offset_to_line(*open) == line)
                .min_by_key(|(open, close)| close - open)
                .copied()
        });
        let Some((open, close)) = pair else {
            return;
        };
        let range = open + 1..close;
        if !text[range.clone()].contains('\n') {
            return;
        }
        self.collapse(Fold {
            range,
            placeholder: "...".to_owned(),
        });
    }

    /// Ctrl+Alt+- and Ctrl+Alt+= work on every fold around the caret.
    fn expand_to_level(&mut self, level: usize, around_caret: bool) {
        let head = self.buffer.selections().primary().head;
        let scope = around_caret.then(|| {
            self.map
                .folds
                .candidates()
                .iter()
                .filter(|fold| fold.range.start <= head && head <= fold.range.end)
                .max_by_key(|fold| fold.range.len())
                .map(|fold| fold.range.clone())
        });
        let scope = scope.flatten();
        self.map.folds.expand_to_level(level, scope);
        self.settle_carets_into_folds();
    }

    fn settle_carets_into_folds(&mut self) {
        let hidden = |fold: &Fold, selection: &Selection| {
            fold.range.start < selection.head && selection.head < fold.range.end
        };
        let moved = self.buffer.selections().map(|selection| {
            let found = self
                .map
                .folds
                .as_slice()
                .iter()
                .find(|fold| hidden(fold, selection));
            found.map_or(*selection, |fold| Selection::caret(fold.range.start))
        });
        self.buffer.set_selections(moved);
    }

    /// The named regions of the text, for Go to Custom Region and for the fold of a
    /// header line.
    pub fn regions(&self) -> Vec<syntax::Region> {
        let store = self.buffer.store();
        let prefix = self
            .language
            .as_ref()
            .and_then(|language| language.line_comment)
            .or(Some("//"));
        syntax::custom_regions(&store.slice(0..store.len()), prefix)
    }

    /// Tab between an opening bracket or quote and the closing one the editor inserted
    /// with it moves past the closing one.
    pub fn brace_or_quote_out(&mut self) -> bool {
        let store = self.buffer.snapshot();
        let selections = self.buffer.selections().clone();
        if selections.iter().any(|selection| !selection.is_empty()) {
            return false;
        }
        let mut after = Vec::with_capacity(selections.len());
        for selection in selections.iter() {
            let head = selection.head;
            let Some(next) = store.char_at(head).filter(|c| is_closer(*c)) else {
                return false;
            };
            let inserted = self.auto_closers.contains(&head);
            let paired = store
                .char_at(store.prev_boundary(head))
                .is_some_and(|open| closing_of(open) == Some(next));
            if !inserted && !paired {
                return false;
            }
            after.push(Selection::caret(store.next_boundary(head)));
        }
        self.auto_closers
            .retain(|at| !after.iter().any(|s| s.head > *at));
        self.buffer.set_selections(SelectionSet::from_vec(after));
        true
    }

    /// Alt+Shift+G: a caret at the end of every selected line.
    fn add_caret_per_selected_line(&mut self) {
        let store = self.buffer.snapshot();
        let selections = self.buffer.selections().clone();
        let mut lines: Vec<usize> = Vec::new();
        for selection in selections.iter() {
            let first = store.offset_to_line(selection.start());
            let mut last = store.offset_to_line(selection.end());
            if last > first && store.line_to_offset(last) == selection.end() {
                last -= 1;
            }
            lines.extend(first..=last);
        }
        lines.sort_unstable();
        lines.dedup();
        let carets: Vec<Selection> = lines
            .into_iter()
            .map(|line| Selection::caret(store.line_end(line)))
            .collect();
        if !carets.is_empty() {
            self.buffer.set_selections(SelectionSet::from_vec(carets));
        }
    }

    /// Ctrl+Alt+Shift+Left and Right: the element under the caret changes places with
    /// its neighbour, one undo step, and the caret stays on it.
    pub fn has_element_at_caret(&self) -> bool {
        let store = self.buffer.store();
        let head = self.buffer.selections().primary().start();
        element_list_at(&store.slice(0..store.len()), head).is_some()
    }

    fn move_element(&mut self, forward: bool) -> Option<Change> {
        let store = self.buffer.snapshot();
        let text = store.text();
        let selections = self.buffer.selections().clone();
        let (elements, index) = element_list_at(&text, selections.primary().start())?;
        let other = if forward {
            elements.get(index + 1)?
        } else {
            elements.get(index.checked_sub(1)?)?
        };
        let moving = &elements[index];
        let (range, replacement, shift) = if forward {
            let between = &text[moving.end..other.start];
            let moving_text = &text[moving.clone()];
            let other_text = &text[other.clone()];
            (
                moving.start..other.end,
                format!("{other_text}{between}{moving_text}"),
                (other_text.len() + between.len()) as isize,
            )
        } else {
            let between = &text[other.end..moving.start];
            let moving_text = &text[moving.clone()];
            let other_text = &text[other.clone()];
            (
                other.start..moving.end,
                format!("{moving_text}{between}{other_text}"),
                -((other_text.len() + between.len()) as isize),
            )
        };
        let moved = selections.map(|selection| {
            let shift = |at: usize| {
                if moving.start <= at && at <= moving.end {
                    (at as isize + shift) as usize
                } else {
                    at
                }
            };
            Selection::new(shift(selection.anchor), shift(selection.head))
        });
        let change = self.buffer.apply(
            vec![Edit::new(range, replacement)],
            Some(moved),
            EditSource::User,
            EditKind::Command,
        );
        self.finish(change)
    }

    /// Text for the clipboard. Without a selection IDEA copies the whole line.
    pub fn copy_text(&self) -> String {
        let store = self.buffer.store();
        let selections = self.buffer.selections();
        if selections.iter().all(Selection::is_empty) {
            let mut lines = String::new();
            for selection in selections.iter() {
                lines.push_str(&store.line_text(store.offset_to_line(selection.head)));
                lines.push('\n');
            }
            return lines;
        }
        let parts: Vec<_> = selections
            .iter()
            .filter(|selection| !selection.is_empty())
            .map(|selection| store.slice(selection.range()))
            .collect();
        parts.join("\n")
    }

    pub fn cut(&mut self) -> Option<Change> {
        if self.is_read_only() {
            return None;
        }
        if self.buffer.selections().iter().all(Selection::is_empty) {
            return self.edit(|buffer| commands::delete_lines(buffer, EditSource::User));
        }
        self.edit(|buffer| commands::backspace(buffer, EditSource::User))
    }

    pub fn click(&mut self, offset: usize, extend: bool, add_caret: bool, count: usize) {
        self.goal = None;
        self.grown.clear();
        let store = self.buffer.store();
        let range = match count {
            2 => commands::word_at(&store, offset),
            3.. => {
                let line = store.offset_to_line(offset);
                let next = (line + 1 < store.line_count()).then(|| store.line_to_offset(line + 1));
                store.line_to_offset(line)..next.unwrap_or_else(|| store.line_end(line))
            }
            _ => offset..offset,
        };
        let selection = Selection::new(range.start, range.end);
        if add_caret {
            let selections = self.buffer.selections();
            let hit = |s: &Selection| s.start() <= offset && offset <= s.end();
            if count == 1 && selections.len() > 1 && selections.iter().any(hit) {
                let rest = selections.iter().filter(|s| !hit(s)).copied().collect();
                self.buffer.set_selections(SelectionSet::from_vec(rest));
            } else {
                let mut selections = selections.clone();
                selections.add(selection);
                self.buffer.set_selections(selections);
            }
        } else if extend {
            let primary = self.buffer.selections().primary();
            self.buffer
                .set_selections(SelectionSet::single(Selection::new(primary.anchor, offset)));
        } else {
            self.buffer.set_selections(SelectionSet::single(selection));
        }
    }

    /// Alt+drag: a block of the text between two offsets, one selection per line, next to
    /// the carets that were there before the drag.
    pub fn select_block(&mut self, kept: &[Selection], from: usize, to: usize) {
        let store = self.buffer.store();
        let (from_line, to_line) = (store.offset_to_line(from), store.offset_to_line(to));
        let column_of = |line: usize, offset: usize| {
            chars_in(&store.line_text(line), offset - store.line_to_offset(line))
        };
        let (from_column, to_column) = (column_of(from_line, from), column_of(to_line, to));
        let mut selections: Vec<Selection> = kept.to_vec();
        let (first, last) = (from_line.min(to_line), from_line.max(to_line));
        for line in first..=last {
            let start = self.offset_at(line, from_column);
            let end = self.offset_at(line, to_column);
            selections.push(Selection::new(start, end));
        }
        if to_line < from_line {
            selections.reverse();
        }
        self.buffer
            .set_selections(SelectionSet::from_vec(selections));
    }

    /// The carets as they are, to start a block drag from.
    pub fn carets(&self) -> Vec<Selection> {
        self.buffer.selections().as_slice().to_vec()
    }

    pub fn drag_to(&mut self, offset: usize) {
        let primary = self.buffer.selections().primary();
        let moved = self.buffer.selections().map(|selection| {
            if *selection == primary {
                Selection::new(primary.anchor, offset)
            } else {
                *selection
            }
        });
        self.buffer.set_selections(moved);
    }

    /// IME: replaces `range`, or the marked text, or the selection.
    pub fn replace_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        mark: bool,
    ) -> Option<Change> {
        self.catch_up();
        if self.is_read_only() {
            return None;
        }
        let target = self.marked.take().or(range);
        let text = normalize_newlines(text);
        let single = text.chars().count() == 1 && !mark;
        let typed = single.then(|| text.chars().next()).flatten();
        let lone =
            target.is_none() && single && self.buffer.selections().iter().all(Selection::is_empty);
        let start = lone.then(|| self.buffer.selections().primary().head);
        let change = match target {
            Some(range) => {
                let caret = range.start + text.len();
                let selections = SelectionSet::single(Selection::caret(caret));
                let edit = Edit::new(range.clone(), text.as_ref());
                self.buffer.apply(
                    vec![edit],
                    Some(selections),
                    EditSource::User,
                    EditKind::Typing,
                )
            }
            None if lone && self.overwrite => {
                let (edits, after) = self.overwrite_edits(&text);
                self.buffer
                    .apply(edits, Some(after), EditSource::User, EditKind::Typing)
            }
            None if single => {
                let unit = self.indent_unit().to_owned();
                let character = typed.unwrap_or(' ');
                editing::type_char(&mut self.buffer, character, &unit, EditSource::User)
            }
            None => commands::insert(&mut self.buffer, &text, EditSource::User),
        };
        let changed = self.finish(change);
        if let (Some(head), Some(character)) = (start, typed)
            && !self.overwrite
            && let Some(close) = closing_of(character)
        {
            let at = head + text.len();
            if self.buffer.store().char_at(at) == Some(close) {
                self.auto_closers.push(at);
            }
        }
        if mark && !text.is_empty() {
            let end = self.buffer.selections().primary().head;
            self.marked = Some(end - text.len()..end);
        }
        changed
    }

    pub fn unmark(&mut self) {
        self.marked = None;
    }

    /// The latest parse, maybe of an older version: fine for questions about the place.
    pub(crate) fn tree(&self) -> Option<&Tree> {
        self.parsed.as_ref().map(|parsed| &parsed.tree)
    }

    /// Applies `edits` of the current text and puts the carets at `after`. A command is one
    /// step of undo, typing groups with the typing around it.
    pub(crate) fn apply_template_edits(
        &mut self,
        edits: Vec<Edit>,
        after: SelectionSet,
        kind: EditKind,
    ) -> Option<Change> {
        self.catch_up();
        if self.is_read_only() || edits.is_empty() {
            return None;
        }
        let change = self
            .buffer
            .apply(edits, Some(after), EditSource::User, kind);
        self.finish(change)
    }

    /// Joins the last `count` undo steps into one, for an answer that arrives after the
    /// change it belongs to.
    pub fn merge_last_undo(&mut self, count: usize) {
        self.buffer.merge_last_undo(count);
    }

    /// Stops grouping, so the next change starts an undo step of its own.
    pub fn seal_undo(&mut self) {
        self.buffer.seal_undo();
    }

    pub fn select_range(&mut self, range: Range<usize>) {
        self.buffer
            .set_selections(SelectionSet::single(Selection::new(range.start, range.end)));
        self.reveal();
    }

    /// Puts the primary caret at `offset`; with `extend` the selection keeps its anchor.
    pub fn select_to(&mut self, offset: usize, extend: bool) {
        let selection = self.buffer.selections().primary();
        let kept = if extend {
            Selection::new(selection.anchor, offset)
        } else {
            Selection::caret(offset)
        };
        self.select(SelectionSet::single(kept));
    }

    /// Where the primary caret is, in bytes.
    pub fn head(&self) -> usize {
        self.buffer.selections().primary().head
    }

    /// The offset of a 0-based line and column in characters, clamped to the text.
    pub fn offset_at(&self, line: usize, column: usize) -> usize {
        let store = self.buffer.store();
        let line = line.min(store.line_count().saturating_sub(1));
        let start = store.line_to_offset(line);
        let text = store.line_text(line);
        let text = text.trim_end_matches(['\n', '\r']);
        let within = text
            .char_indices()
            .nth(column)
            .map_or(text.len(), |(at, _)| at);
        start + within
    }

    /// Edits from outside the keyboard, such as Replace in Files. One undo step.
    pub fn apply_edits(&mut self, edits: Vec<Edit>) -> Option<Change> {
        self.catch_up();
        if self.is_read_only() {
            return None;
        }
        let change = self
            .buffer
            .apply(edits, None, EditSource::User, EditKind::Command);
        self.finish(change)
    }

    /// Several changes that came in one answer. They are one undo step, even when the
    /// server sent more than one `TextDocumentEdit` for this file.
    pub fn apply_edits_batches(&mut self, batches: Vec<Vec<Edit>>) -> Option<Change> {
        self.catch_up();
        if self.is_read_only() {
            return None;
        }
        let change = self.buffer.apply_batches(batches);
        self.finish(change)
    }
}

/// The lines of a pasted text after the first take `base`, and the piece of indentation
/// that all its lines share is dropped first, so a block keeps its own shape.
fn reindent_paste(text: &str, base: &str) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    let common = common_indent(&lines);
    let mut out = String::with_capacity(text.len() + base.len() * lines.len());
    for (index, line) in lines.iter().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        if index == 0 || line.trim().is_empty() {
            out.push_str(line);
            continue;
        }
        out.push_str(base);
        out.push_str(line.strip_prefix(&common).unwrap_or(line));
    }
    out
}

/// The indentation every non-empty line starts with.
fn common_indent(lines: &[&str]) -> String {
    let mut common: Option<&str> = None;
    for line in lines.iter().filter(|line| !line.trim().is_empty()) {
        let indent = &line[..line.len() - line.trim_start_matches([' ', '\t']).len()];
        common = Some(match common {
            None => indent,
            Some(shared) => {
                let length = shared
                    .chars()
                    .zip(indent.chars())
                    .take_while(|(left, right)| left == right)
                    .count();
                &shared[..length]
            }
        });
    }
    common.unwrap_or("").to_owned()
}

/// The indent of a new line after `offset`, from the parse when there is one.
fn indent_for(store: &RopeStore, parsed: Option<&Parsed>, offset: usize, unit: &str) -> String {
    match parsed {
        Some(parsed) => indent_after(store, &parsed.tree, &parsed.indents, offset, unit),
        None => {
            let mut indent = line_indent(store, offset);
            let start = store.line_to_offset(store.offset_to_line(offset));
            if store
                .slice(start..offset)
                .trim_end()
                .ends_with(['{', '(', '['])
            {
                indent.push_str(unit);
            }
            indent
        }
    }
}

fn line_indent(store: &RopeStore, offset: usize) -> String {
    let line = store.line_text(store.offset_to_line(offset));
    line[..line.len() - line.trim_start().len()].to_owned()
}

/// A caret that lands inside a collapsed fold moves past it.
fn skip_folds(map: &DisplayMap, offset: usize, forward: bool) -> usize {
    let fold = map
        .folds
        .as_slice()
        .iter()
        .find(|fold| fold.range.start < offset && offset < fold.range.end);
    match fold {
        Some(fold) if forward => fold.range.end,
        Some(fold) => fold.range.start,
        None => offset,
    }
}

/// A character that closes a pair.
fn is_closer(character: char) -> bool {
    matches!(character, ')' | ']' | '}' | '"' | '\'' | '`')
}

/// The closing partner of an opening bracket or quote.
fn closing_of(open: char) -> Option<char> {
    Some(match open {
        '(' => ')',
        '[' => ']',
        '{' => '}',
        '"' => '"',
        '\'' => '\'',
        '`' => '`',
        _ => return None,
    })
}

#[cfg(test)]
mod checks {
    use keymap::editor as actions;
    use text_core::FileFormat;

    use super::EditorState;

    fn state(text: &str) -> EditorState {
        EditorState::new(text, FileFormat::default(), None, false)
    }

    fn type_in(state: &mut EditorState, text: &str) {
        for character in text.chars() {
            state.replace_range(None, &character.to_string(), false);
        }
    }

    #[test]
    fn typing_closes_brackets_and_types_over_the_closing_one() {
        let mut state = state("");
        type_in(&mut state, "f(a)");
        assert_eq!(state.text(), "f(a)");
        assert_eq!(state.head(), 4);
        type_in(&mut state, "\"x");
        assert_eq!(state.text(), "f(a)\"x\"");
    }

    #[test]
    fn enter_between_braces_opens_a_block() {
        let mut state = state("class A {}");
        state.select_range(9..9);
        state.perform(actions::NEWLINE, 10);
        assert_eq!(state.text(), "class A {\n    \n}");
        assert_eq!(state.head(), 14);
    }

    #[test]
    fn join_case_and_word_deletion_run_by_action() {
        let mut state = state("a\n   b");
        state.select_range(0..0);
        state.perform(actions::JOIN_LINES, 10);
        assert_eq!(state.text(), "a b");
        state.select_range(0..3);
        state.perform(actions::TOGGLE_CASE, 10);
        assert_eq!(state.text(), "A B");
        state.select_range(3..3);
        state.perform(actions::DELETE_WORD_BACK, 10);
        assert_eq!(state.text(), "A ");
    }

    #[test]
    fn the_last_edit_places_are_visited_newest_first() {
        let mut state = state("one\ntwo\nthree\nfour");
        state.select_range(0..0);
        type_in(&mut state, "x");
        state.select_range(state.text().len()..state.text().len());
        type_in(&mut state, "y");
        state.select_range(8..8);

        state.perform(actions::LAST_EDIT, 10);
        assert_eq!(state.head(), state.text().len());
        state.perform(actions::LAST_EDIT, 10);
        assert_eq!(state.head(), 1);
    }

    #[test]
    fn the_places_follow_later_edits() {
        let mut state = state("a\nb");
        state.select_range(1..1);
        type_in(&mut state, "1");
        state.select_range(0..0);
        type_in(&mut state, "ZZ");
        state.select_range(0..0);
        state.perform(actions::LAST_EDIT, 10);
        assert_eq!(state.head(), 2);
    }

    #[test]
    fn a_matching_bracket_is_jumped_to_and_back() {
        let mut state = state("f(a[1])");
        state.select_range(1..1);
        state.perform(actions::MATCH_BRACKET, 10);
        assert_eq!(state.head(), 7);
        state.perform(actions::MATCH_BRACKET, 10);
        assert_eq!(state.head(), 1);
    }

    #[test]
    fn a_block_drag_selects_the_same_columns_on_every_line() {
        let mut state = state("abcd\nef\nghij");
        state.select_block(&[], 1, 12);
        let selections: Vec<_> = state
            .selections()
            .iter()
            .map(|selection| selection.range())
            .collect();
        assert_eq!(selections, [1..4, 6..7, 9..12]);
    }

    #[test]
    fn alt_click_adds_a_caret_and_a_second_one_takes_it_away() {
        let mut state = state("abc def");
        state.click(1, false, false, 1);
        state.click(5, false, true, 1);
        assert_eq!(state.selections().len(), 2);
        state.click(5, false, true, 1);
        assert_eq!(state.selections().len(), 1);
    }

    #[test]
    fn the_word_at_the_caret_is_known_for_highlighting() {
        let mut state = state("pet = pet2");
        state.select_range(1..1);
        assert_eq!(state.caret_word(), Some(0..3));
        state.select_range(0..3);
        assert_eq!(state.caret_word(), None);
    }

    #[test]
    fn a_bookmark_is_toggled_named_and_follows_its_line() {
        let mut state = state(
            "a
b
c",
        );
        state.select_range(2..2);
        state.toggle_bookmark(None);
        assert_eq!(state.bookmarks(), [(1, None)]);
        state.toggle_bookmark(Some('A'));
        assert_eq!(state.bookmarks(), [(1, Some('A'))]);
        assert_eq!(state.bookmark_line('A'), Some(1));
        state.select_range(0..0);
        type_in(
            &mut state, "xx
",
        );
        assert_eq!(state.bookmarks(), [(2, Some('A'))]);
        state.select_range(5..5);
        state.toggle_bookmark(Some('A'));
        assert!(state.bookmarks().is_empty());
    }

    #[test]
    fn a_name_belongs_to_one_bookmark() {
        let mut state = state(
            "a
b
c",
        );
        state.select_range(0..0);
        state.toggle_bookmark(Some('1'));
        state.select_range(4..4);
        state.toggle_bookmark(Some('1'));
        assert_eq!(state.bookmarks(), [(0, None), (2, Some('1'))]);
    }

    #[test]
    fn a_blame_label_follows_its_line_and_a_typed_line_takes_none() {
        let mut state = state("one\ntwo\nthree");
        let note = |commit: &str| crate::annotate::BlameNote {
            commit: commit.to_owned(),
            label: commit.to_owned(),
            details: String::new(),
        };
        state.set_annotations(&[(1, note("a")), (2, note("b"))]);
        assert_eq!(
            state.annotation_labels(),
            [
                (0, "a".to_owned(), "a".to_owned()),
                (1, "b".to_owned(), "b".to_owned())
            ]
        );

        // Enter at the end of the first line: the empty line under it has no label and
        // the label of `two` moves down with its text.
        state.select_range(3..3);
        type_in(&mut state, "\n");
        assert_eq!(
            state
                .annotation_labels()
                .iter()
                .map(|(line, label, _)| (*line, label.clone()))
                .collect::<Vec<_>>(),
            [(0, "a".to_owned()), (2, "b".to_owned())]
        );
    }

    #[test]
    fn fold_all_keeps_the_outer_regions_open() {
        let text = "class A {
  void f() {
    x();
  }
  void g() {
    y();
  }
}
";
        let mut state = state(text);
        let body = |after: &str| {
            let start = text.find(after).unwrap() + after.len();
            start..start + text[start..].find('}').unwrap()
        };
        let outer = text.find('{').unwrap() + 1..text.rfind('}').unwrap();
        let fold = |range| display_map::Fold {
            range,
            placeholder: "{...}".into(),
        };
        state
            .map
            .folds
            .set_candidates(vec![fold(outer), fold(body("f() {")), fold(body("g() {"))]);
        state.perform(actions::FOLD_ALL, 10);
        assert_eq!(state.map().folds.as_slice().len(), 2);
        state.perform(actions::UNFOLD_ALL, 10);
        assert!(state.map().folds.as_slice().is_empty());
    }

    #[test]
    fn selected_lines_fold_into_dots() {
        let mut state = state(
            "a
b
c
d",
        );
        state.select_range(1..5);
        state.perform(actions::FOLD_SELECTION, 10);
        assert_eq!(state.display().line(0).text, "a...");
        assert_eq!(state.display().row_count(), 2);
    }

    #[test]
    fn the_caret_walks_the_rows_of_a_wrapped_line() {
        let mut state = state(
            "one two three four
end",
        );
        state.set_wrap(Some(9));
        state.select_range(0..0);
        state.perform(actions::MOVE_DOWN, 10);
        assert_eq!(state.head(), 8);
        state.perform(actions::MOVE_LINE_END, 10);
        assert_eq!(state.head(), 14);
        state.perform(actions::MOVE_DOWN, 10);
        state.perform(actions::MOVE_DOWN, 10);
        assert_eq!(state.head(), 22);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(text: &str) -> EditorState {
        EditorState::new(text, text_core::FileFormat::default(), None, false)
    }

    #[test]
    fn a_save_trims_the_buffer_except_the_caret_line_and_adds_the_final_newline() {
        let mut state = state("one  \ntwo \t\nthree   ");
        state.properties.trim_trailing_whitespace = Some(true);
        state.properties.insert_final_newline = Some(true);
        state.set_caret(2, 8);

        assert!(state.prepare_save().is_some());

        assert_eq!(
            state.text(),
            "one\ntwo\nthree   \n",
            "the line being typed keeps its spaces"
        );
        let (on_disk, _, _) = state.snapshot_for_save();
        assert_eq!(on_disk, state.text(), "the disk gets what the buffer shows");
        assert!(
            state.prepare_save().is_none(),
            "a second save has nothing to do"
        );
    }

    #[test]
    fn the_caret_is_placed_by_line_and_character_and_clamped() {
        let mut state = state("first\nsecond é\nlast");
        state.set_caret(1, 8);
        assert_eq!(state.caret(), (1, 8));
        state.set_caret(1, 99);
        assert_eq!(state.caret(), (1, 8), "past the end of the line");
        state.set_caret(99, 2);
        assert_eq!(state.caret(), (2, 2), "past the last line");
    }

    #[test]
    fn selected_characters_are_counted_not_bytes() {
        let mut state = state("aé\nb");
        assert_eq!(state.selected_chars(), 0);
        state.select_range(0..3);
        assert_eq!(state.selected_chars(), 2);
    }

    #[test]
    fn an_edited_buffer_asks_and_the_disk_text_comes_in_as_an_undoable_edit() {
        let mut state = state("one\n");
        state.type_text("typed ");
        assert!(state.is_dirty());
        let disk = |text: &str| text_core::decode(text.as_bytes());

        assert!(
            state.reload(disk("other\n")).is_err(),
            "edits and disk differ"
        );
        assert!(
            state.reload(disk("typed one\n")).is_ok(),
            "the disk has our text"
        );
        assert!(state.is_dirty(), "nothing was decided");

        state.reload_from_disk(disk("other\n"));
        assert_eq!(state.text(), "other\n");
        assert!(!state.is_dirty());
        state.perform(actions::UNDO, 10);
        assert_eq!(state.text(), "typed one\n", "the edits come back");
    }

    #[test]
    fn a_new_format_marks_the_buffer_edited() {
        let mut state = state("a\n");
        assert!(!state.is_dirty());
        state.set_format(state.format().with_eol(text_core::Eol::CrLf));
        assert!(state.is_dirty());
        assert_eq!(state.eol_name(), "CRLF");
    }

    #[test]
    fn edits_in_several_places_keep_the_spans_ordered_and_inside_the_text() {
        use syntax::{HighlightKind, HighlightSpan};

        let mut state = state("class A {\n    void a() {}\n    int b;\n}\n");
        state.spans = vec![
            HighlightSpan {
                start: 0,
                end: 7,
                kind: HighlightKind::Keyword,
            },
            HighlightSpan {
                start: 10,
                end: 20,
                kind: HighlightKind::Function,
            },
            HighlightSpan {
                start: 24,
                end: 33,
                kind: HighlightKind::Field,
            },
        ];
        // One batch replaces a range across the first two spans and inserts a line
        // after them; both edits carry line breaks.
        state.apply_edits_batches(vec![vec![
            Edit::new(5..14, "x\ny\nz"),
            Edit::insert(35, "\n    String c;\n"),
        ]]);
        let text = state.text();
        let spans = state.spans();
        assert!(!spans.is_empty(), "the span list survives the edit");
        for pair in spans.windows(2) {
            assert!(pair[0].start <= pair[1].start, "{spans:?}");
            assert!(pair[0].end <= pair[1].start, "{spans:?}");
        }
        assert!(
            spans.iter().all(|span| span.end as usize <= text.len()),
            "{spans:?} in {text:?}"
        );
    }
}

#[cfg(test)]
mod action_checks {
    use keymap::editor as actions;
    use text_core::{FileFormat, Selection, SelectionSet};

    use super::EditorState;

    fn state(text: &str) -> EditorState {
        EditorState::new(text, FileFormat::default(), None, false)
    }

    fn type_in(state: &mut EditorState, text: &str) {
        for character in text.chars() {
            state.replace_range(None, &character.to_string(), false);
        }
    }

    #[test]
    fn tab_leaves_the_closing_character_the_editor_inserted() {
        let mut state = state("");
        type_in(&mut state, "f(");
        assert_eq!(state.text(), "f()");
        assert_eq!(state.head(), 2);
        assert!(state.brace_or_quote_out());
        assert_eq!(state.head(), 3);
        assert!(!state.brace_or_quote_out(), "the caret is past the pair");
    }

    #[test]
    fn a_typed_closing_character_is_not_a_pair_to_leave() {
        let mut state = state(")");
        state.select_range(0..0);
        assert!(!state.brace_or_quote_out(), "nothing was inserted here");
    }

    #[test]
    fn tab_with_a_selection_indents_its_lines() {
        let mut state = state("a\nb\n");
        state.select_range(0..3);
        state.perform(actions::TAB, 10);
        assert_eq!(state.text(), "    a\n    b\n");
    }

    #[test]
    fn insert_replaces_the_character_under_the_caret() {
        let mut state = state("abc");
        state.select_range(1..1);
        state.perform(actions::TOGGLE_INSERT_STATE, 10);
        assert!(state.overwrite());
        type_in(&mut state, "X");
        assert_eq!(state.text(), "aXc");
        assert_eq!(state.head(), 2);
        state.perform(actions::TOGGLE_INSERT_STATE, 10);
        assert!(!state.overwrite());
        type_in(&mut state, "Y");
        assert_eq!(state.text(), "aXYc");
    }

    #[test]
    fn overwrite_replaces_a_selection_and_keeps_every_caret_in_place() {
        let mut state = state("abcd\nx\n");
        state.perform(actions::TOGGLE_INSERT_STATE, 10);
        state.select_range(1..3);
        type_in(&mut state, "Z");
        assert_eq!(state.text(), "aZd\nx\n");
        assert_eq!(state.head(), 2);
        // A caret at the end of a line inserts, the one after it still replaces.
        let carets = vec![Selection::caret(3), Selection::caret(4)];
        state.select(SelectionSet::from_vec(carets));
        type_in(&mut state, "Q");
        assert_eq!(state.text(), "aZdQ\nQ\n");
        let heads: Vec<usize> = state.selections().iter().map(|s| s.head).collect();
        assert_eq!(heads, [4, 6]);
    }

    #[test]
    fn a_caret_goes_to_the_end_of_every_selected_line() {
        let mut state = state("ab\ncd\nef\n");
        state.select_range(0..8);
        state.perform(actions::ADD_CARET_PER_SELECTED_LINE, 10);
        let heads: Vec<usize> = state
            .selections()
            .iter()
            .map(|selection| selection.head)
            .collect();
        assert_eq!(heads, [2, 5, 8]);
    }

    #[test]
    fn elements_swap_with_their_neighbour_and_the_caret_stays_on_one() {
        let text = "f(a, b, c)";
        let mut state = state(text);
        state.select_range(2..2);
        state.perform(actions::MOVE_ELEMENT_RIGHT, 10);
        assert_eq!(state.text(), "f(b, a, c)");
        assert_eq!(state.head(), 5);
        state.perform(actions::MOVE_ELEMENT_LEFT, 10);
        assert_eq!(state.text(), text);
        assert_eq!(state.head(), 2);
        state.perform(actions::UNDO, 10);
        assert_eq!(state.text(), "f(b, a, c)", "one step of undo per move");
    }

    #[test]
    fn a_list_after_a_word_moves_its_elements() {
        let mut state = state("class A implements B, C {\n}\n");
        let caret = state.text().find(" B").unwrap() + 2;
        state.select_range(caret..caret);
        state.perform(actions::MOVE_ELEMENT_RIGHT, 10);
        assert_eq!(state.text(), "class A implements C, B {\n}\n");
    }

    #[test]
    fn collapse_block_folds_the_braces_around_the_caret() {
        let text = "class A {\n    void f() {\n        x();\n    }\n}\n";
        let mut state = state(text);
        let caret = text.find("x()").unwrap();
        state.select_range(caret..caret);
        state.perform(actions::COLLAPSE_BLOCK, 10);
        assert_eq!(state.map().folds.as_slice().len(), 1);
        let open =
            text.find("void").unwrap() + text[text.find("void").unwrap()..].find('{').unwrap();
        let close = text[..text.rfind('}').unwrap()].rfind('}').unwrap();
        assert_eq!(state.map().folds.as_slice()[0].range, open + 1..close);
        assert!(
            state.display().line(1).text.contains("..."),
            "the folded line shows the placeholder: {:?}",
            state.display().line(1).text
        );
    }

    #[test]
    fn a_recursive_collapse_takes_the_outermost_region_and_expand_opens_it() {
        let text = "a {\n b {\n c {\n  x();\n }\n }\n}\n";
        let mut state = state(text);
        let caret = text.find("x()").unwrap();
        state.select_range(caret..caret);
        let outer_open = text.find('{').unwrap();
        let outer_close = text.rfind('}').unwrap();
        let middle_open = text[outer_open + 1..].find('{').unwrap() + outer_open + 1;
        let middle_close = text[..outer_close].rfind('}').unwrap();
        let inner_open = text[middle_open + 1..].find('{').unwrap() + middle_open + 1;
        let inner_close = text[..middle_close].rfind('}').unwrap();
        let outer = outer_open + 1..outer_close;
        let middle = middle_open + 1..middle_close;
        let inner = inner_open + 1..inner_close;
        state.map.folds.set_candidates(vec![
            display_map::Fold {
                range: outer,
                placeholder: "...".into(),
            },
            display_map::Fold {
                range: middle,
                placeholder: "...".into(),
            },
            display_map::Fold {
                range: inner,
                placeholder: "...".into(),
            },
        ]);
        state.perform(actions::COLLAPSE_REGION_RECURSIVELY, 10);
        assert_eq!(state.map().folds.as_slice().len(), 1);
        state.perform(actions::EXPAND_REGION_RECURSIVELY, 10);
        assert!(state.map().folds.as_slice().is_empty());
    }

    #[test]
    fn a_named_region_folds_from_its_header_and_shows_its_name() {
        let text = "//region First\none();\ntwo();\n//endregion\n";
        let mut state = state(text);
        assert_eq!(state.regions().len(), 1);
        state.select_range(0..0);
        state.perform(actions::FOLD, 10);
        assert_eq!(state.map().folds.as_slice().len(), 1);
        assert!(state.display().line(0).text.starts_with("//region First"));
        assert!(state.display().line(0).text.contains("..."));
    }
}
