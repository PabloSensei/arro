//! The buffer's side of a language server, through `lang-api`. The editor knows no
//! language: the application sets [`Semantic`], and files it does not match get none.
//! Every question runs off the UI thread; an answer for an older text is dropped.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use ::platform::{self, CanonPath};
use display_map::{Bias, DisplayPoint};
use futures_util::StreamExt;
use keymap::editor as actions;
use lang_api::{
    CallParameter, CallSignature, CommandQuery, CompletionCommand, CompletionEntry, CompletionKind,
    CompletionLabelDetails, Confidence, DiagnosticEntry, DiagnosticsQuery, DocumentEdit,
    DocumentSpot, InlayHintEntry, InlayHintKind, InlayHintQuery, InlayParameterNames,
    LanguageSupport, NavigationKind, NavigationQuery, Replies, SemanticToken, SemanticTokenKind,
    SemanticTokenModifiers, SemanticTokensQuery, Severity, SignatureHelp, SignatureQuery,
    SourceSpan, SourceTextQuery, TextPoint, ask, offset_in,
};
use lsp_client::lsp_types::Uri;
use lsp_client::uri::file_path;
use templates::{Expansion, snippet_expansion};
use text_core::{Edit, EditKind, RopeStore, Selection, SelectionSet, TextStore};
use ui_kit::prelude::*;
use ui_kit::*;

use crate::actions::ActionMenu;
use crate::refactor::{MoveField, RenameField};
use crate::state::EditorState;
use crate::view::{EditorEvent, EditorView};
use crate::{DocBlock, DocInline};

pub(crate) const NO_ANSWER: &str = "The language server did not answer, try again";
pub(crate) const MAX_SHOWN: usize = 12;

/// The wait before the semantic tokens are asked for after an edit, in milliseconds.
const TOKENS_DELAY_MS: u64 = 250;
/// The wait before the documentation of the selected entry is asked for, so a pick made
/// at once does not send the same resolve twice.
const COMPLETION_DOC_DELAY_MS: u64 = 150;
/// How long a message about a question that found nothing stays on screen, in
/// milliseconds. It closes by itself, as the messages of the status bar do in IDEA.
const NOTE_LINGER_MS: u64 = 2500;
/// Above this many lines the editor asks only for the visible part of a document, when
/// the server colours a range.
const TOKENS_RANGE_LINES: usize = 2000;
/// Rows a range request covers at least, so that the first frame before layout still
/// colours a useful part of the file.
const TOKENS_RANGE_ROWS: usize = 160;

/// Entries made in the editor without a server, code templates for one. They show
/// next to the server's entries and do not wait for it.
pub trait LocalCompletions: 'static {
    /// `before` is the line up to the caret, `caret` where it is. An entry that
    /// replaces more than the word before the caret gives its own span.
    fn entries(&self, path: &Path, before: &str, caret: TextPoint) -> Vec<CompletionEntry>;
}

/// All local sources of completion. Set by the application, empty by default.
#[derive(Clone, Default)]
pub struct CompletionSources(pub Vec<Rc<dyn LocalCompletions>>);

impl Global for CompletionSources {}

/// Opens a documentation page in a browser.
pub type ExternalOpener = Arc<dyn Fn(&str) + Send + Sync>;

/// Opens a documentation page. The application leaves it unset and the platform opens
/// the default browser; tests put their own function here.
pub struct ExternalLinks(pub Option<ExternalOpener>);

impl Global for ExternalLinks {}

/// The language behind the files it matches. Set by the application.
pub struct Semantic(pub Arc<dyn LanguageSupport>);

impl Global for Semantic {}

/// The place of a declaration of a name, or `None` when the project has no such name.
pub type SymbolFinder = dyn Fn(&str) -> Option<SourceSpan> + Send + Sync;

/// Finds the declaration of a name in the project index, as Ctrl+B falls back to it while
/// the language server is not ready. Set by the application; `None` keeps the fallback
/// off. The answer is the place of the declaration.
pub struct SymbolLookup(pub Option<Arc<SymbolFinder>>);

impl Global for SymbolLookup {}

/// The choices of the user for the checks of the project, shared by the open editors.
pub struct InspectionProfile(pub lang_api::InspectionSettings);

impl Global for InspectionProfile {}

/// Languages for files that no project server owns, such as the console of a tool. The
/// application sets them; the editor asks them when the project language does not match.
pub struct FileLanguages(pub Vec<Arc<dyn LanguageSupport>>);

impl Global for FileLanguages {}

/// The text of a document that is not a file, and the origin it came from.
#[derive(Clone)]
pub(crate) struct SourceDoc {
    pub text: Arc<str>,
    /// The origin the language gave for the text; a class of a library has one.
    pub origin: Option<Arc<str>>,
}

/// Texts that are not files, opened read only under a made-up path.
#[derive(Default)]
pub(crate) struct Sources(pub HashMap<PathBuf, SourceDoc>);

impl Global for Sources {}

/// Where the caret goes once a file is shown, by path.
#[derive(Default)]
pub(crate) struct Reveals(pub HashMap<PathBuf, TextPoint>);

impl Global for Reveals {}

pub(crate) fn point_at(store: &RopeStore, offset: usize) -> TextPoint {
    let line = store.offset_to_line(offset);
    let start = store.line_to_offset(line);
    let character = store.offset_to_utf16(offset) - store.offset_to_utf16(start);
    TextPoint {
        line: line as u32,
        character: character as u32,
    }
}

/// A point past the last line is the end of the text, as servers send for "to the end".
pub(crate) fn offset_at(store: &RopeStore, point: TextPoint) -> usize {
    if point.line as usize >= store.line_count() {
        return store.len();
    }
    let line = point.line as usize;
    let start = store.line_to_offset(line);
    let end = store.line_end(line);
    let utf16 = store.offset_to_utf16(start) + point.character as usize;
    store.utf16_to_offset(utf16.min(store.offset_to_utf16(end)))
}

pub(crate) fn is_word(character: char) -> bool {
    character.is_alphanumeric() || character == '_' || character == '$'
}

/// The start of the word that ends at `offset`.
pub(crate) fn word_start(store: &RopeStore, offset: usize) -> usize {
    let mut start = offset;
    while start > 0 {
        let previous = store.prev_boundary(start);
        if !store.char_at(previous).is_some_and(is_word) {
            break;
        }
        start = previous;
    }
    start
}

/// The end of the word that starts at `offset`.
fn word_end(store: &RopeStore, offset: usize) -> usize {
    let mut end = offset;
    while end < store.len() {
        let Some(character) = store.char_at(end) else {
            break;
        };
        if !is_word(character) {
            break;
        }
        end += character.len_utf8();
    }
    end
}

/// A made-up path for a URI that is not a file, named after its last segment. It takes
/// the extension of the file the navigation started in, so it gets the same language.
fn source_path(uri: &str, extension: &str) -> PathBuf {
    let name = uri.split(['?', '#']).next().unwrap_or(uri);
    let name = name.rsplit('/').next().unwrap_or(name);
    let stem = name.split('.').next().unwrap_or(name);
    let stem = stem.replace(['%', ':', '<', '>', '|', '*', '"'], "_");
    let name = if extension.is_empty() {
        stem
    } else {
        format!("{stem}.{extension}")
    };
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in uri.bytes() {
        hash = (hash ^ u64::from(byte)).wrapping_mul(0x100_0000_01b3);
    }
    PathBuf::from(format!("[source {hash:016x}]")).join(name)
}

/// The file a `file:` URI names, `None` for any other URI.
pub fn uri_path(uri: &str) -> Option<PathBuf> {
    file_path(&Uri::from_str(uri).ok()?)
}

/// Prepares `target` to be shown and calls `open` with the path to open: the file, or a
/// made-up path whose text the language gives. The caret goes to the place once the
/// editor of the path shows it. `missing` runs when there is nothing to show. Both run
/// later, never inside the caller, which may be the editor being updated.
pub fn show_location(
    target: &SourceSpan,
    extension: &str,
    cx: &mut App,
    open: impl FnOnce(PathBuf, &mut App) + 'static,
    missing: impl FnOnce(&mut App) + 'static,
) {
    let point = target.span.start;
    let file = Uri::from_str(&target.uri)
        .ok()
        .and_then(|uri| file_path(&uri));
    if let Some(path) = file {
        cx.default_global::<Reveals>().0.insert(path.clone(), point);
        cx.defer(move |cx| open(path, cx));
        return;
    }
    let support = cx
        .try_global::<Semantic>()
        .map(|semantic| semantic.0.clone());
    let uri = target.uri.clone();
    let path = source_path(&uri, extension);
    // A class of a library that was opened before already has its text: showing it
    // again asks the server nothing, which keeps a repeated Ctrl+B instant.
    if cx
        .try_global::<Sources>()
        .is_some_and(|sources| sources.0.contains_key(&path))
    {
        cx.default_global::<Reveals>().0.insert(path.clone(), point);
        cx.defer(move |cx| open(path, cx));
        return;
    }
    let Some(provider) = support.as_ref().and_then(|support| support.source_text()) else {
        cx.defer(missing);
        return;
    };
    let replies = ask(provider, SourceTextQuery(uri.clone()));
    cx.spawn(async move |cx| {
        let answer = first(replies).await;
        cx.update(|cx| match answer {
            Some((text, _)) if !text.is_empty() => {
                let origin = uri.clone();
                cx.default_global::<Sources>().0.insert(
                    path.clone(),
                    SourceDoc {
                        text: text.into(),
                        origin: Some(origin.into()),
                    },
                );
                cx.default_global::<Reveals>().0.insert(path.clone(), point);
                open(path, cx);
            }
            _ => missing(cx),
        });
    })
    .detach();
}

#[derive(Clone)]
pub(crate) struct Mark {
    pub range: Range<usize>,
    pub severity: Severity,
    pub message: String,
    /// The server marked the range as something that can go. The editor greys it out.
    pub unnecessary: bool,
    /// The check that gave the mark, for its settings and its suppression comment.
    pub code: Option<String>,
}

/// A semantic token of the document, in byte offsets. It follows the text the way the
/// syntax spans do, so a colour stays on its name until the next answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TokenSpan {
    pub start: u32,
    pub end: u32,
    pub kind: SemanticTokenKind,
    pub modifiers: SemanticTokenModifiers,
}

/// Moves `tokens` along an edit, with the same rules as the syntax spans: a token that
/// crosses a replaced range keeps its free ends and leaves the inserted text out.
pub(crate) fn shift_tokens(tokens: &mut Vec<TokenSpan>, edits: &[Edit]) {
    tokens.sort_by_key(|token| (token.end, token.start));
    for edit in edits.iter().rev() {
        let (start, end) = (edit.range.start as u32, edit.range.end as u32);
        let delta = i64::from(edit.text.len() as u32) - i64::from(end - start);
        let first = tokens.partition_point(|token| token.end <= start);
        for token in &mut tokens[first..] {
            if token.start >= end {
                token.start = (i64::from(token.start) + delta) as u32;
                token.end = (i64::from(token.end) + delta) as u32;
            } else {
                token.start = token.start.min(start);
                token.end = if token.end <= end {
                    start
                } else {
                    (i64::from(token.end) + delta) as u32
                };
            }
        }
        tokens.retain(|token| token.start < token.end);
    }
}

/// Sorts tokens, cuts them to `len` and drops the parts that overlap a token before
/// them, so that every range is painted once.
pub(crate) fn normalize_tokens(tokens: &mut Vec<TokenSpan>, len: usize) {
    let len = len.min(u32::MAX as usize) as u32;
    tokens.sort_by_key(|token| (token.start, token.end));
    let mut taken = 0u32;
    tokens.retain_mut(|token| {
        if token.start >= len || token.end > len {
            return false;
        }
        token.start = token.start.max(taken);
        if token.start >= token.end {
            return false;
        }
        taken = token.end;
        true
    });
}

pub(crate) struct Completion {
    /// Where the typed prefix starts: an accepted entry replaces from here.
    pub start: usize,
    entries: Vec<CompletionEntry>,
    /// The text the list compares with the typed prefix, and its folded copy. They are
    /// made once per answer: a keystroke only filters the list and never allocates them
    /// again.
    keys: Vec<String>,
    folded: Vec<String>,
    /// The class of every entry, made once per answer: a member or a local comes before
    /// a type, a name the language prefers before a rare one.
    ranks: Vec<u8>,
    pub shown: Vec<usize>,
    pub selected: usize,
    /// The first row of the part of the list on screen: the list scrolls under the
    /// selection when there are more entries than [`MAX_SHOWN`].
    scroll: usize,
    /// Whether the server sent every entry that can match the prefix. An incomplete list
    /// is asked again when typing goes on.
    pub complete: bool,
}

/// One completion row: the label split into the text on the left and the type on the
/// right, with the class that declares it.
pub(crate) struct CompletionRow {
    pub left: String,
    pub right: Option<String>,
    pub owner: Option<String>,
    /// The entry is out of use: the row strikes its name through.
    pub deprecated: bool,
}

impl Completion {
    fn new(
        start: usize,
        entries: Vec<CompletionEntry>,
        complete: bool,
        prefix: &str,
        case: CompletionCase,
        preferred: &'static [&'static str],
    ) -> Self {
        let keys: Vec<String> = entries.iter().map(completion_key).collect();
        let folded: Vec<String> = keys.iter().map(|key| key.to_lowercase()).collect();
        let ranks: Vec<u8> = entries
            .iter()
            .map(|entry| entry_class(entry, preferred))
            .collect();
        let mut completion = Self {
            start,
            entries,
            keys,
            folded,
            ranks,
            shown: Vec::new(),
            selected: 0,
            scroll: 0,
            complete,
        };
        completion.filter_with(prefix, case);
        completion
    }

    #[cfg(test)]
    fn filter(&mut self, prefix: &str) {
        self.filter_with(prefix, CompletionCase::None);
    }

    fn filter_with(&mut self, prefix: &str, case: CompletionCase) {
        self.shown = order_entries(
            &self.entries,
            &self.keys,
            &self.folded,
            &self.ranks,
            prefix,
            case,
        );
        self.selected = 0;
        self.scroll = 0;
    }

    pub fn label(&self, row: usize) -> CompletionRow {
        let entry = &self.entries[self.shown[row]];
        let (left, right, owner) = completion_parts_with_label_details(
            &entry.label,
            entry.detail.as_deref(),
            entry.label_details.as_ref(),
        );
        CompletionRow {
            left,
            right,
            owner,
            deprecated: entry.deprecated,
        }
    }

    /// What the row names, for the icon before it.
    pub fn kind(&self, row: usize) -> CompletionKind {
        self.entries
            .get(self.shown[row])
            .map_or(CompletionKind::Unknown, |entry| entry.kind)
    }

    /// The text to insert and the span it replaces, when the entry has its own.
    #[cfg(test)]
    fn chosen(&self) -> Option<(String, Option<lang_api::TextSpan>)> {
        let entry = &self.entries[*self.shown.get(self.selected)?];
        let text = entry
            .insert_text
            .clone()
            .unwrap_or_else(|| entry.label.clone());
        Some((text, entry.span))
    }

    /// The chosen entry with everything the server sent for it.
    fn chosen_entry(&self) -> Option<CompletionEntry> {
        let entry = &self.entries[*self.shown.get(self.selected)?];
        Some(entry.clone())
    }

    /// The labels of the rows on screen and the label of the selected row. A test reads
    /// them; nothing else does.
    #[cfg(any(test, feature = "test-support"))]
    pub fn visible_labels(&self) -> (Vec<String>, Option<String>) {
        let labels = self
            .window()
            .map(|row| self.entries[self.shown[row]].label.clone())
            .collect();
        let selected = self
            .shown
            .get(self.selected)
            .map(|index| self.entries[*index].label.clone());
        (labels, selected)
    }

    /// The labels of the rows this list shows, in their order. A test reads them.
    #[cfg(any(test, feature = "test-support"))]
    pub fn labels(&self) -> Vec<String> {
        self.shown
            .iter()
            .map(|index| self.entries[*index].label.clone())
            .collect()
    }

    /// The rows on screen, at most [`MAX_SHOWN`], from the row the list was scrolled to.
    pub fn window(&self) -> Range<usize> {
        self.scroll..(self.scroll + MAX_SHOWN).min(self.shown.len())
    }

    /// Rows the list can scroll by: zero when every entry fits on screen.
    fn scroll_limit(&self) -> usize {
        self.shown.len().saturating_sub(MAX_SHOWN)
    }

    /// Scrolls the rows on screen by `rows`, a wheel notch or a page.
    pub fn scroll_by(&mut self, rows: isize) {
        let next = self.scroll as isize + rows;
        self.scroll = next.clamp(0, self.scroll_limit() as isize) as usize;
    }

    /// Keeps the selected row on screen after the selection moved.
    fn reveal_selected(&mut self) {
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + MAX_SHOWN {
            self.scroll = self.selected + 1 - MAX_SHOWN;
        }
    }

    /// Moves the selection by `rows`, wrapping around the list, and scrolls to it.
    fn step_selection(&mut self, rows: isize) {
        let len = self.shown.len();
        if len == 0 {
            return;
        }
        let len = len as isize;
        self.selected = (self.selected as isize + rows).rem_euclid(len) as usize;
        self.reveal_selected();
    }

    /// Moves the selection to `selected`, clamped to the list, and scrolls to it.
    fn jump_selection(&mut self, selected: isize) {
        let last = self.shown.len().saturating_sub(1) as isize;
        self.selected = selected.clamp(0, last) as usize;
        self.reveal_selected();
    }
}

/// How the list compares the typed prefix with an entry.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CompletionCase {
    /// The case of the letters does not matter.
    #[default]
    None,
    /// Only the first letter must match in case.
    FirstLetter,
    /// Every letter must match in case.
    All,
}

/// How the list filters entries; the default keeps the old case-insensitive match.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum CompletionMode {
    #[default]
    All,
    SmartType,
    ClassName,
}

/// The name of a type without its package, type arguments and array brackets.
fn simple_type(text: &str) -> String {
    let text = text.trim().trim_end_matches("...");
    let text = text.split('<').next().unwrap_or(text);
    let text = text.rsplit('.').next().unwrap_or(text);
    text.trim_end_matches("[]").trim().to_owned()
}

/// The type of what an entry names, from the right part of its label or from its detail.
fn entry_type(entry: &CompletionEntry) -> Option<String> {
    let (left, right, _) = completion_parts_with_label_details(
        &entry.label,
        entry.detail.as_deref(),
        entry.label_details.as_ref(),
    );
    if let Some(right) = right
        && entry.kind != CompletionKind::Class
        && entry.kind != CompletionKind::Interface
        && entry.kind != CompletionKind::Enum
        && entry.kind != CompletionKind::Struct
    {
        return Some(right);
    }
    if matches!(
        entry.kind,
        CompletionKind::Class
            | CompletionKind::Interface
            | CompletionKind::Enum
            | CompletionKind::Struct
            | CompletionKind::Constructor
    ) {
        let name = left.split('(').next().unwrap_or(&left).trim();
        return Some(name.to_owned());
    }
    if let Some(detail) = &entry.detail
        && let Some((_, after)) = detail.rsplit_once(" : ")
    {
        return Some(after.trim().to_owned());
    }
    None
}

/// Whether an entry can stand where `expected` is wanted. `None` keeps everything, so a
/// list without a proven type is the usual list.
fn matches_expected(entry: &CompletionEntry, expected: Option<&str>) -> bool {
    let Some(expected) = expected
        .map(simple_type)
        .filter(|expected| !expected.is_empty() && expected != "var" && expected != "Object")
    else {
        return true;
    };
    entry_type(entry).is_some_and(|found| simple_type(&found) == expected)
}

/// A type whose kind the provider did not give: a name without a call or a member type.
fn looks_like_type(entry: &CompletionEntry) -> bool {
    let label = entry.label.split(" - ").next().unwrap_or(&entry.label);
    let label = label.trim();
    label.starts_with(char::is_uppercase)
        && !label.contains('(')
        && !label.contains(" : ")
        && entry
            .detail
            .as_deref()
            .is_none_or(|detail| !detail.contains('(') && !detail.contains(" : "))
}

/// The entries a list of `mode` shows. A smart type list without a proven type is the
/// usual list, so the key still gives something.
fn visible_entries(
    entries: Vec<CompletionEntry>,
    mode: CompletionMode,
    expected: Option<&str>,
) -> Vec<CompletionEntry> {
    match mode {
        CompletionMode::All => entries,
        CompletionMode::ClassName => entries
            .into_iter()
            .filter(|entry| {
                entry.kind.is_type()
                    || (entry.kind == CompletionKind::Unknown && looks_like_type(entry))
            })
            .collect(),
        CompletionMode::SmartType => entries
            .into_iter()
            .filter(|entry| matches_expected(entry, expected))
            .collect(),
    }
}

/// A postfix snippet of the server: its `filterText` names the expression it follows,
/// `args.for` for one.
fn is_postfix_snippet(entry: &CompletionEntry) -> bool {
    entry.snippet
        && !entry.label.contains('(')
        && entry
            .filter_text
            .as_deref()
            .is_some_and(|text| text.contains('.'))
}

/// The names a local template and a server entry share when they offer the same key. A
/// postfix entry carries the expression it follows in `filterText` (`args.for`), so its
/// last segment counts too.
fn completion_keys(entry: &CompletionEntry) -> Vec<String> {
    let label = entry
        .label
        .split(" : ")
        .next()
        .unwrap_or(&entry.label)
        .trim()
        .to_lowercase();
    let mut keys = vec![label];
    if let Some(filter) = &entry.filter_text {
        let last = filter
            .rsplit('.')
            .next()
            .unwrap_or(filter)
            .trim()
            .to_lowercase();
        if !keys.contains(&last) {
            keys.push(last);
        }
    }
    keys
}

/// Local templates and the server may offer the same key; the local one, which has no
/// `sort_text`, stays and the server entry goes. Only the server's own templates are
/// dropped (snippets without a call in the label): a field or a method that happens to
/// share the name of a template stays.
fn merge_completions(
    local: Vec<CompletionEntry>,
    server: Vec<CompletionEntry>,
) -> Vec<CompletionEntry> {
    let local_keys: std::collections::HashSet<String> = local
        .iter()
        .filter(|entry| entry.sort_text.is_none())
        .flat_map(completion_keys)
        .collect();
    let mut entries = local;
    entries.extend(server.into_iter().filter(|entry| {
        let template = entry.snippet && !entry.label.contains('(');
        entry.sort_text.is_none()
            || !template
            || !completion_keys(entry)
                .iter()
                .any(|key| local_keys.contains(key))
    }));
    entries
}

/// The indices of `entries` in the order the list shows them for `prefix`: entries that
/// start with it first, then the ones that contain it. The server ranked its entries with
/// `sortText`; that order stays, and an equal `sortText` puts the shorter name first, so
/// `Alpha` comes before `Alphabet` and `AlphaBeta`.
pub fn completion_order(
    entries: &[CompletionEntry],
    prefix: &str,
    preferred: &[&str],
) -> Vec<usize> {
    completion_order_with(entries, prefix, CompletionCase::None, preferred)
}

/// [`completion_order`] with the case rule of Settings: `None` ignores case,
/// `FirstLetter` compares the case of the first letter and `All` compares every letter.
/// `preferred` names the types the language puts before the others.
pub fn completion_order_with(
    entries: &[CompletionEntry],
    prefix: &str,
    case: CompletionCase,
    preferred: &[&str],
) -> Vec<usize> {
    let keys: Vec<String> = entries.iter().map(completion_key).collect();
    let folded: Vec<String> = keys.iter().map(|key| key.to_lowercase()).collect();
    let ranks: Vec<u8> = entries
        .iter()
        .map(|entry| entry_class(entry, preferred))
        .collect();
    order_entries(entries, &keys, &folded, &ranks, prefix, case)
}

/// The text of an entry that the typed prefix is compared with: the server's own filter
/// text, or the name of the label without its type, its package or its parameters.
fn completion_key(entry: &CompletionEntry) -> String {
    if let Some(filter) = &entry.filter_text {
        return filter.clone();
    }
    let label = entry.label.split(" : ").next().unwrap_or(&entry.label);
    let label = label.split(" - ").next().unwrap_or(label);
    let label = label.split('(').next().unwrap_or(label);
    label.trim().to_owned()
}

/// Where an entry stands when the prefix matches it equally well: a template of the
/// editor, a member or a local, then a type. Among types a name the language prefers
/// comes before a rare one and before a type of a library.
fn entry_class(entry: &CompletionEntry, preferred: &[&str]) -> u8 {
    // Entries without a `sortText` are the local templates, which stay in front.
    if entry.sort_text.is_none() {
        return 0;
    }
    let is_type =
        entry.kind.is_type() || (entry.kind == CompletionKind::Unknown && looks_like_type(entry));
    if !is_type {
        return 1;
    }
    let name = entry.label.split(" - ").next().unwrap_or(&entry.label);
    let name = name.split('(').next().unwrap_or(name).trim();
    if preferred.contains(&name) { 2 } else { 3 }
}

/// The indices of `entries` in the order the list shows them for `prefix`. An entry that
/// starts with the prefix and has the same case comes before one that only starts with
/// it, then one that contains it; within a group a template, a member or a local comes
/// before a type, and the order of the server's `sortText` decides between equal kinds.
/// An equal `sortText` puts the shorter name first, so `Alpha` comes before `Alphabet`.
fn order_entries(
    entries: &[CompletionEntry],
    keys: &[String],
    folded: &[String],
    ranks: &[u8],
    prefix: &str,
    case: CompletionCase,
) -> Vec<usize> {
    let folded_prefix = prefix.to_lowercase();
    let matches_case = |index: usize, at_start: bool| match case {
        CompletionCase::None => true,
        CompletionCase::FirstLetter => match (prefix.chars().next(), keys[index].chars().next()) {
            (Some(typed), Some(key)) => {
                typed.is_uppercase() == key.is_uppercase() || !typed.is_alphabetic()
            }
            _ => true,
        },
        CompletionCase::All => {
            if at_start {
                keys[index].starts_with(prefix)
            } else {
                keys[index].contains(prefix)
            }
        }
    };
    // The group of an entry by how well the prefix matches: the exact text, the same
    // case at the start, the text at the start, the same case inside, the text inside.
    let bucket = |index: usize| -> Option<u8> {
        let key = &keys[index];
        let folded = &folded[index];
        if key == prefix {
            Some(0)
        } else if key.starts_with(prefix) {
            Some(1)
        } else if folded.starts_with(&folded_prefix) {
            Some(2)
        } else if key.contains(prefix) {
            Some(3)
        } else if folded.contains(&folded_prefix) {
            Some(4)
        } else {
            None
        }
    };
    let mut shown: Vec<(u8, u8, usize)> = Vec::new();
    for index in 0..entries.len() {
        let starts = keys[index].starts_with(prefix);
        let Some(bucket) = bucket(index) else {
            continue;
        };
        if !matches_case(index, starts) {
            continue;
        }
        shown.push((bucket, ranks[index], index));
    }
    shown.sort_by(|(bucket_a, class_a, a), (bucket_b, class_b, b)| {
        bucket_a
            .cmp(bucket_b)
            .then(class_a.cmp(class_b))
            .then_with(|| match (&entries[*a].sort_text, &entries[*b].sort_text) {
                // Entries without a sort text are the local templates: their own order
                // stays.
                (Some(one), Some(other)) if one != other => one.cmp(other),
                (Some(_), Some(_)) => folded[*a].chars().count().cmp(&folded[*b].chars().count()),
                (None, Some(_)) => std::cmp::Ordering::Less,
                (Some(_), None) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            })
            .then_with(|| a.cmp(b))
    });
    shown.into_iter().map(|(_, _, index)| index).collect()
}

/// A completion label in the three parts of the list: the name and parameters, the type
/// on the right (`getLastName(String) : String`) and the class that declares it, from the
/// detail (`Person.getLastName(String) : String`).
pub fn completion_parts(
    label: &str,
    detail: Option<&str>,
) -> (String, Option<String>, Option<String>) {
    completion_parts_with_label_details(label, detail, None)
}

/// [`completion_parts`] with the parts the server already split in `labelDetails`. A type
/// label may carry its package after ` - ` (`Widget - com.example`); the package goes to
/// the right and does not repeat the owner.
pub(crate) fn completion_parts_with_label_details(
    label: &str,
    detail: Option<&str>,
    label_details: Option<&CompletionLabelDetails>,
) -> (String, Option<String>, Option<String>) {
    let (name, package) = match label.split_once(" - ") {
        Some((name, package)) => (name.trim(), Some(package.trim().to_owned())),
        None => (label, None),
    };
    let description = label_details.and_then(|details| details.description.clone());
    match name.split_once(" : ") {
        Some((left, right)) => (
            left.trim().to_owned(),
            Some(right.trim().to_owned()),
            detail.and_then(owner_of),
        ),
        None => {
            let left = name.to_owned();
            match description.or(package) {
                Some(package) => (left, Some(package), None),
                None => (left, None, detail.and_then(owner_of)),
            }
        }
    }
}

/// The class that declares a method or a field, from the detail of the entry:
/// `Person.getLastName(String) : String` gives `Person`.
fn owner_of(detail: &str) -> Option<String> {
    let head = detail.split(" : ").next().unwrap_or(detail).trim();
    if head.is_empty() {
        return None;
    }
    let Some((signature, _)) = head.split_once('(') else {
        // No signature: `Person.size : int` names the member after the class, while
        // `org.example.List` is the class itself.
        return match detail
            .contains(" : ")
            .then(|| head.rsplit_once('.'))
            .flatten()
        {
            Some((owner, _)) if !owner.is_empty() => Some(owner.to_owned()),
            _ => Some(head.to_owned()),
        };
    };
    match signature.rsplit_once('.') {
        Some((owner, _)) if !owner.is_empty() => Some(owner.to_owned()),
        _ => None,
    }
}

/// Puts a snippet in: `$1`, `${2:name}` and `$0` become text. The caret goes to the first
/// numbered tab stop, else to the final one `$0` (`greet(${0})`); the second value is that
/// offset, `None` for the end. Variables and transforms become empty.
pub fn expand_snippet(snippet: &str) -> (String, Option<usize>) {
    let expansion = snippet_expansion(snippet, "", "    ");
    let caret = expansion
        .fields
        .first()
        .map(|field| field.ranges[0].start)
        .or_else(|| (expansion.end < expansion.text.len()).then_some(expansion.end));
    (expansion.text, caret)
}

/// The text an entry inserts and the fields it starts, for a snippet of the server or a
/// plain entry.
fn expand_entry(entry: &CompletionEntry, indent: &str, unit: &str) -> Expansion {
    let text = entry
        .insert_text
        .clone()
        .unwrap_or_else(|| entry.label.clone());
    if entry.snippet {
        snippet_expansion(&text, indent, unit)
    } else {
        Expansion {
            end: text.len(),
            text,
            fields: Vec::new(),
        }
    }
}

/// The parameters inside the parentheses of a signature label, comma separated at the
/// outermost level. Used when the server gives no parameter labels.
pub(crate) fn parameters_in_label(label: &str) -> Vec<CallParameter> {
    let Some(open) = label.find('(') else {
        return Vec::new();
    };
    let mut depth = 0i32;
    let mut close = None;
    for (at, character) in label[open..].char_indices() {
        match character {
            '(' | '<' | '[' => depth += 1,
            ')' | '>' | ']' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(open + at);
                    break;
                }
            }
            _ => {}
        }
    }
    let Some(close) = close else {
        return Vec::new();
    };
    let inside = &label[open + 1..close];
    if inside.trim().is_empty() {
        return Vec::new();
    }
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    for (at, character) in inside.char_indices() {
        match character {
            '(' | '<' | '[' => depth += 1,
            ')' | '>' | ']' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(CallParameter {
                    label: inside[start..at].trim().to_owned(),
                });
                start = at + 1;
            }
            _ => {}
        }
    }
    parts.push(CallParameter {
        label: inside[start..].trim().to_owned(),
    });
    parts
}

/// A signature split into the text to draw and whether it is the active parameter.
pub(crate) fn signature_segments(signature: &CallSignature, active: usize) -> Vec<(String, bool)> {
    let parameters = if signature.parameters.is_empty() {
        parameters_in_label(&signature.label)
    } else {
        signature.parameters.clone()
    };
    let mut segments: Vec<(String, bool)> = Vec::new();
    let mut at = 0;
    for (index, parameter) in parameters.iter().enumerate() {
        if parameter.label.is_empty() {
            continue;
        }
        let Some(found) = signature.label[at..].find(&parameter.label) else {
            continue;
        };
        let start = at + found;
        if start > at {
            push_segment(&mut segments, signature.label[at..start].to_owned(), false);
        }
        push_segment(&mut segments, parameter.label.clone(), index == active);
        at = start + parameter.label.len();
    }
    if at < signature.label.len() {
        push_segment(&mut segments, signature.label[at..].to_owned(), false);
    }
    segments
}

fn push_segment(segments: &mut Vec<(String, bool)>, text: String, active: bool) {
    match segments.last_mut() {
        Some((known, known_active)) if *known_active == active => {
            known.push_str(&text);
            *known_active = active;
        }
        _ => segments.push((text, active)),
    }
}

/// A line of text over the caret: documentation, a signature, an error.
pub(crate) struct Note {
    pub at: usize,
    pub text: SharedString,
}

/// How an accepted entry goes in.
#[derive(Clone, Copy, Default)]
pub(crate) struct AcceptOptions {
    /// Ctrl+Shift+Enter: finish the statement after the entry.
    pub complete_statement: bool,
    /// Ctrl+.: leave a dot and ask for the list again.
    pub dot: bool,
}

/// An answer that belongs to an insertion made earlier.
pub(crate) struct LateEdit {
    pub start: usize,
    pub end: usize,
    pub inserted: usize,
    /// The version of the text right after the insertion.
    pub version: u64,
}

/// The Parameter Info popup: the overloads of the call around the caret, the one the
/// user picked and the parameter the caret is in.
pub(crate) struct SignaturePopup {
    pub at: usize,
    pub help: SignatureHelp,
    pub selected: usize,
    pub parameter: usize,
}

impl SignaturePopup {
    fn new(at: usize, help: SignatureHelp) -> Option<Self> {
        if help.signatures.is_empty() {
            return None;
        }
        let selected = help.active_signature.min(help.signatures.len() - 1);
        let parameter = help.signatures.get(selected).map_or(0, |signature| {
            help.active_parameter
                .min(signature.parameters.len().saturating_sub(1))
        });
        Some(Self {
            at,
            help,
            selected,
            parameter,
        })
    }

    pub fn signature(&self) -> &CallSignature {
        &self.help.signatures[self.selected]
    }
}

/// The argument under each stop of a call that was just inserted without snippet fields.
/// Tab and Shift+Tab walk the stops while the caret stays inside the call.
pub(crate) struct ParameterWalk {
    pub stops: Vec<Range<usize>>,
    /// The position just after the closing bracket, where the caret lands after the
    /// text is inserted.
    pub limit: usize,
}

impl ParameterWalk {
    pub fn new(text: &str, at: usize, caret: usize) -> Option<Self> {
        let open = text.find('(')?;
        let mut depth = 0i32;
        let mut close = None;
        for (offset, character) in text[open..].char_indices() {
            match character {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        close = Some(open + offset);
                        break;
                    }
                }
                _ => {}
            }
        }
        let close = close?;
        let inside = &text[open + 1..close];
        let mut stops = Vec::new();
        let mut depth = 0i32;
        let mut start = 0;
        if inside.trim().is_empty() {
            stops.push(at + open + 1..at + open + 1);
        } else {
            for (offset, character) in inside.char_indices() {
                match character {
                    '(' | '<' | '[' | '{' => depth += 1,
                    ')' | '>' | ']' | '}' => depth -= 1,
                    ',' if depth == 0 => {
                        stops.push(trimmed_range(inside, start, offset, at + open + 1));
                        start = offset + 1;
                    }
                    _ => {}
                }
            }
            stops.push(trimmed_range(inside, start, inside.len(), at + open + 1));
        }
        let inside_call = at + open..=at + close + 1;
        inside_call.contains(&caret).then_some(Self {
            stops,
            limit: at + close + 1,
        })
    }

    pub fn contains(&self, caret: usize) -> bool {
        self.stops
            .iter()
            .any(|stop| stop.start <= caret && caret <= stop.end)
            || caret == self.limit
    }
}

fn trimmed_range(text: &str, start: usize, end: usize, base: usize) -> Range<usize> {
    let piece = &text[start..end];
    let leading = piece.len() - piece.trim_start().len();
    let trailing = piece.trim_end().len();
    base + start + leading..base + start + trailing
}

/// The lines of the documentation the list shows under its rows. The list keeps a few
/// of them, as IDEA does: the popup stays a list and never takes the screen.
pub(crate) const COMPLETION_DOC_LINES: usize = 8;

/// The documentation of the selected completion entry, loaded after the list is shown.
pub(crate) struct CompletionDoc {
    /// The index of the entry in the list, so a late answer for another one is dropped.
    pub entry: usize,
    /// The lines of the markdown, read once: a frame only draws them.
    pub rows: Vec<CompletionDocRow>,
}

impl CompletionDoc {
    pub(crate) fn new(entry: usize, text: impl AsRef<str>) -> Self {
        Self {
            entry,
            rows: completion_doc_rows_of(text.as_ref()),
        }
    }
}

/// The icon the completion list puts before a name, from the icon set of the window.
pub(crate) fn completion_kind_icon(kind: CompletionKind) -> &'static str {
    match kind {
        CompletionKind::Method | CompletionKind::Function | CompletionKind::Constructor => {
            "nodes/method"
        }
        CompletionKind::Field | CompletionKind::Property => "nodes/field",
        CompletionKind::Variable | CompletionKind::Value | CompletionKind::Unit => "nodes/variable",
        CompletionKind::Class | CompletionKind::Struct | CompletionKind::TypeParameter => {
            "nodes/class"
        }
        CompletionKind::Interface => "nodes/interface",
        CompletionKind::Enum => "nodes/enum",
        CompletionKind::Module => "nodes/package",
        CompletionKind::Constant => "nodes/constant",
        CompletionKind::Keyword | CompletionKind::Operator => "nodes/keyword",
        CompletionKind::Snippet => "nodes/template",
        CompletionKind::Folder => "nodes/folder",
        CompletionKind::Color => "status/info",
        CompletionKind::Text
        | CompletionKind::Reference
        | CompletionKind::File
        | CompletionKind::Event
        | CompletionKind::Unknown => "fileTypes/text",
    }
}

/// A run of a line of documentation under the completion rows: text, or code set in the
/// editor font on a tint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DocRun {
    pub text: String,
    pub code: bool,
    pub strong: bool,
    pub emphasis: bool,
}

/// One line of that documentation. A line of a quote or of a code block keeps the code
/// look, and a list item keeps its indent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CompletionDocRow {
    pub runs: Vec<DocRun>,
    pub indent: usize,
    pub code: bool,
}

/// The lines of the documentation that stand under the rows: at most
/// [`COMPLETION_DOC_LINES`]. The markdown the server sends becomes plain lines, so the
/// backticks, the bullets and the quote marks do not reach the screen.
pub(crate) fn completion_doc_rows(doc: &CompletionDoc) -> Vec<CompletionDocRow> {
    doc.rows.clone()
}

/// The markdown of a documentation answer as the lines of the list.
fn completion_doc_rows_of(text: &str) -> Vec<CompletionDocRow> {
    let mut rows = Vec::new();
    for block in crate::parse_documentation(text) {
        push_doc_block(block, false, &mut rows);
        if rows.len() >= COMPLETION_DOC_LINES {
            break;
        }
    }
    rows.truncate(COMPLETION_DOC_LINES);
    if rows.is_empty() {
        let text = text.trim();
        if !text.is_empty() {
            rows.push(CompletionDocRow {
                runs: vec![DocRun::text(text)],
                indent: 0,
                code: false,
            });
        }
    }
    rows
}

impl DocRun {
    fn text(text: &str) -> Self {
        Self {
            text: text.to_owned(),
            code: false,
            strong: false,
            emphasis: false,
        }
    }
}

fn push_doc_block(block: DocBlock, code: bool, rows: &mut Vec<CompletionDocRow>) {
    match block {
        DocBlock::Paragraph(inlines) => {
            for line in doc_run_lines(doc_runs(&inlines, code)) {
                rows.push(CompletionDocRow {
                    runs: line,
                    indent: 0,
                    code,
                });
            }
        }
        DocBlock::Heading { text, .. } => rows.push(CompletionDocRow {
            runs: doc_runs(&text, code)
                .into_iter()
                .map(|run| DocRun {
                    strong: true,
                    ..run
                })
                .collect(),
            indent: 0,
            code,
        }),
        DocBlock::List {
            ordered,
            start,
            items,
            ..
        } => {
            for (at, item) in items.iter().enumerate() {
                let bullet = if ordered {
                    format!("{}. ", start as usize + at)
                } else {
                    "• ".to_owned()
                };
                for (line, mut runs) in doc_run_lines(doc_runs(item, code)).into_iter().enumerate()
                {
                    if line == 0 {
                        runs.insert(0, DocRun::text(&bullet));
                    }
                    rows.push(CompletionDocRow {
                        runs,
                        indent: 1,
                        code,
                    });
                }
            }
        }
        DocBlock::Code { text, .. } => {
            for line in text.lines() {
                rows.push(CompletionDocRow {
                    runs: vec![DocRun {
                        code: true,
                        ..DocRun::text(line)
                    }],
                    indent: 1,
                    code: true,
                });
            }
        }
        // A quote reads as code: the `>` marks are the markup of the answer, not text.
        DocBlock::Quote(blocks) => {
            for inner in blocks {
                push_doc_block(inner, true, rows);
            }
        }
        // A `<details>` block of a file reads as its summary and the blocks under it.
        DocBlock::Details {
            summary, blocks, ..
        } => {
            rows.push(CompletionDocRow {
                runs: doc_runs(&summary, code),
                indent: 0,
                code,
            });
            for inner in blocks {
                push_doc_block(inner, code, rows);
            }
        }
        DocBlock::Table {
            header, rows: body, ..
        } => {
            let cells = |row: &[Vec<DocInline>]| {
                row.iter()
                    .map(|cell| {
                        doc_runs(cell, code)
                            .into_iter()
                            .map(|run| run.text)
                            .collect::<String>()
                    })
                    .collect::<Vec<_>>()
                    .join("  ")
            };
            rows.push(CompletionDocRow {
                runs: vec![DocRun {
                    strong: true,
                    ..DocRun::text(&cells(&header))
                }],
                indent: 0,
                code,
            });
            for row in body {
                rows.push(CompletionDocRow {
                    runs: vec![DocRun::text(&cells(&row))],
                    indent: 1,
                    code,
                });
            }
        }
        DocBlock::Image { alt, .. } => rows.push(CompletionDocRow {
            runs: vec![DocRun::text(&alt)],
            indent: 0,
            code,
        }),
        DocBlock::Divider => rows.push(CompletionDocRow {
            runs: vec![DocRun::text("────")],
            indent: 0,
            code,
        }),
    }
}

fn doc_runs(inlines: &[DocInline], code: bool) -> Vec<DocRun> {
    let mut runs = Vec::new();
    push_doc_runs(inlines, code, false, false, &mut runs);
    runs
}

/// The runs of one paragraph or list item as lines of their own: a line break of the
/// answer starts a new row under the row above, so the labels of a documentation comment
/// (`Returns:`, `Params:`, `Specified by:`) stand under the text rather than beside it.
/// Empty lines are left out.
fn doc_run_lines(runs: Vec<DocRun>) -> Vec<Vec<DocRun>> {
    let mut lines: Vec<Vec<DocRun>> = vec![Vec::new()];
    for run in runs {
        let parts: Vec<String> = run.text.split('\n').map(str::to_owned).collect();
        for (at, part) in parts.into_iter().enumerate() {
            if at > 0 {
                lines.push(Vec::new());
            }
            let part = part.strip_suffix('\r').unwrap_or(&part);
            if part.is_empty() {
                continue;
            }
            lines.last_mut().expect("a line is open").push(DocRun {
                text: part.to_owned(),
                code: run.code,
                strong: run.strong,
                emphasis: run.emphasis,
            });
        }
    }
    lines.retain(|line| line.iter().any(|run| !run.text.trim().is_empty()));
    lines
}

fn push_doc_runs(
    inlines: &[DocInline],
    code: bool,
    strong: bool,
    emphasis: bool,
    runs: &mut Vec<DocRun>,
) {
    for inline in inlines {
        match inline {
            DocInline::Text(text) => runs.push(DocRun {
                text: text.clone(),
                code,
                strong,
                emphasis,
            }),
            DocInline::Code(text) => runs.push(DocRun {
                text: text.clone(),
                code: true,
                strong,
                emphasis,
            }),
            DocInline::Strong(inner) => push_doc_runs(inner, code, true, emphasis, runs),
            DocInline::Emphasis(inner) => push_doc_runs(inner, code, strong, true, runs),
            DocInline::Link { text, .. } => push_doc_runs(text, code, strong, emphasis, runs),
            DocInline::Image(image) => runs.push(DocRun {
                text: image.alt.clone(),
                code,
                strong,
                emphasis,
            }),
        }
    }
}

/// How many lines the documentation takes under the rows, which is the height the list
/// adds for it.
pub(crate) fn completion_doc_line_count(doc: &CompletionDoc) -> usize {
    doc.rows.len()
}

/// Ctrl+B with more than one target: the places to pick from.
pub(crate) struct NavigationList {
    pub at: usize,
    /// The places the server named, without repeats.
    pub targets: Vec<SourceSpan>,
    pub selected: usize,
}

impl NavigationList {
    fn new(at: usize, mut targets: Vec<SourceSpan>) -> Self {
        // A server sometimes names one place twice; the list keeps the first row.
        let mut seen = HashSet::new();
        targets.retain(|target| {
            seen.insert((
                target.uri.clone(),
                target.span.start.line,
                target.span.start.character,
            ))
        });
        Self {
            at,
            targets,
            selected: 0,
        }
    }

    fn target(&self) -> Option<&SourceSpan> {
        self.targets.get(self.selected)
    }
}

/// The name of the file and the line of one target, as the list of the places shows it.
pub(crate) fn location_label(target: &SourceSpan) -> String {
    let file = uri_path(&target.uri)
        .and_then(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| {
            let uri = &target.uri;
            uri.rsplit(['/', '\\']).next().unwrap_or(uri).to_owned()
        });
    format!("{file}:{}", target.span.start.line + 1)
}

pub(crate) struct Lsp {
    pub support: Arc<dyn LanguageSupport>,
    pub uri: String,
    pub marks: Vec<Mark>,
    /// The marks the server sent, before the native ones join them.
    pub(crate) server_marks: Vec<Mark>,
    /// The marks of a native model, such as the checks of a framework, by the source of
    /// the model.
    pub(crate) native_marks: HashMap<&'static str, Vec<Mark>>,
    /// The tokens the server sent, before the native ones join them.
    pub(crate) server_tokens: Vec<TokenSpan>,
    /// The tokens of a native model, by the source of the model.
    pub(crate) native_tokens: HashMap<&'static str, Vec<TokenSpan>>,
    /// The server answered about the diagnostics of this text at least once.
    pub received: bool,
    pub completion: Option<Completion>,
    /// The documentation of the selected entry, when it arrived.
    pub completion_doc: Option<CompletionDoc>,
    /// Ctrl+B found more than one place: the list waits for the user's pick.
    pub navigation_list: Option<NavigationList>,
    /// The places Ctrl+B found in this text, by place and kind. A repeated press answers
    /// at once; an edit clears the answers, as the places may have moved.
    pub(crate) navigation_cache: HashMap<(String, i32, TextPoint, NavigationKind), Vec<SourceSpan>>,
    pub note: Option<Note>,
    /// Ctrl+Q and the hover popup: the documentation with its blocks and links.
    pub quick: Option<crate::quick_doc::QuickDoc>,
    pub actions: Option<ActionMenu>,
    pub rename: Option<RenameField>,
    pub move_to: Option<MoveField>,
    /// Ctrl+O and Ctrl+I: the methods that can be overridden here.
    pub overrides: Option<crate::hierarchy::OverridePopup>,
    /// Ctrl+Shift+I: the source of the definition at the caret.
    pub definition: Option<crate::hierarchy::DefinitionPopup>,
    /// Alt+Insert: the fields or methods a generation may include.
    pub generation: Option<crate::generate::GeneratePopup>,
    /// Alt+Delete: the dialog about the usages of the declaration.
    pub safe_delete: Option<crate::delete::SafeDeletePopup>,
    /// Occurrences of the symbol under the caret.
    pub highlights: Vec<Range<usize>>,
    pub highlight_asked: u64,
    /// Ctrl+P: the overloads of the call around the caret.
    pub signatures: Option<SignaturePopup>,
    /// Grows with each signature question, so a late answer is dropped.
    signature_asked: u64,
    /// Tab and Shift+Tab between the arguments of a call just inserted.
    pub parameter_walk: Option<ParameterWalk>,
    /// Grows with each completion question, so a late answer is dropped.
    asked: u64,
    /// The newest completion question the server has answered, so the editor knows
    /// whether one is still on its way.
    answered: u64,
    /// The Ctrl+B questions asked and answered, for the same reason.
    nav_asked: u64,
    nav_answered: u64,
    /// Grows with each request of the documentation of the selected entry.
    doc_asked: u64,
    /// How the list compares the typed prefix with its entries.
    pub case: CompletionCase,
}

impl Lsp {
    /// Moves both sets of marks over the edits of a text change and joins them again.
    pub fn retain_transformed_marks(&mut self, steps: &[Vec<text_core::Edit>], len: usize) {
        self.navigation_cache.clear();
        for mark in self
            .server_marks
            .iter_mut()
            .chain(self.native_marks.values_mut().flatten())
        {
            for step in steps {
                mark.range.start = text_core::transform_offset(mark.range.start, step);
                mark.range.end = text_core::transform_offset(mark.range.end, step);
            }
            mark.range.end = mark.range.end.min(len);
            mark.range.start = mark.range.start.min(mark.range.end);
        }
        self.marks = self
            .server_marks
            .iter()
            .chain(self.native_marks.values().flatten())
            .cloned()
            .collect();
    }

    /// Moves both sets of tokens over the edits of a text change.
    pub fn retain_transformed_tokens(&mut self, steps: &[Vec<text_core::Edit>], len: usize) {
        let mut tokens: Vec<&mut TokenSpan> = self
            .server_tokens
            .iter_mut()
            .chain(self.native_tokens.values_mut().flatten())
            .collect();
        for token in &mut tokens {
            for step in steps {
                token.start = text_core::transform_offset(token.start as usize, step) as u32;
                token.end = text_core::transform_offset(token.end as usize, step) as u32;
            }
            token.end = token.end.min(len as u32);
            token.start = token.start.min(token.end);
        }
    }

    /// A question of the user is on its way to the language: a completion or a jump.
    /// A long job of the editor, such as the search behind a usage count, waits for it,
    /// because the server answers one thing at a time.
    pub(crate) fn question_pending(&self) -> bool {
        self.asked != self.answered || self.nav_asked != self.nav_answered
    }

    /// The tokens of the server and of the native models, in one list.
    pub fn joined_tokens(&self) -> Vec<TokenSpan> {
        self.server_tokens
            .iter()
            .chain(self.native_tokens.values().flatten())
            .copied()
            .collect()
    }

    pub fn close(&self) {
        if let Some(server) = self.support.server() {
            server.close(&self.uri);
        }
    }
}

/// How long the typing settles before the server is asked for hints again.
pub(crate) const INLAY_TYPING_DELAY: Duration = Duration::from_millis(150);

/// How long the typing settles before the labels above declarations are asked for
/// again. A label of a language server is a whole document question, and the server
/// answers it while the user is still typing.
pub(crate) const CODE_LENS_TYPING_DELAY: Duration = Duration::from_millis(700);

/// The room around the visible rows that one question covers, in screens. Scrolling
/// within it asks nothing.
const INLAY_MARGIN_SCREENS: usize = 3;

pub(crate) fn inlay_settings(options: &crate::view::EditorOptions) -> lang_api::InlayHintSettings {
    lang_api::InlayHintSettings {
        parameter_names: options.inlay_parameter_names,
        type_hints: options.inlay_type_hints,
    }
}

/// What Settings shows above declarations, for the language and its server.
pub(crate) fn code_vision_settings(
    options: &crate::view::EditorOptions,
) -> lang_api::CodeVisionSettings {
    lang_api::CodeVisionSettings {
        usages: options.code_vision_usages,
        implementations: options.code_vision_implementations,
    }
}

impl EditorView {
    /// The hints drawn in the text, as the offset where each begins and its text. A
    /// test reads them; nothing else does.
    pub fn inlay_texts(&self) -> Vec<(usize, String)> {
        self.state
            .as_ref()
            .map(|state| {
                state
                    .inlays()
                    .iter()
                    .map(|inlay| (inlay.offset, inlay.text.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Asks the server for the hints of the visible part after `delay`. An open question
    /// is cancelled: its answer describes a text or a part of the document nobody sees.
    pub(crate) fn request_inlays(&mut self, delay: Duration, cx: &mut Context<Self>) {
        self.inlays_task = None;
        let (first, last, query, version, support) = {
            let Some(state) = &self.state else {
                return;
            };
            let Some(lsp) = &self.lsp else {
                return;
            };
            if state.is_read_only() || lsp.support.inlay_hints().is_none() {
                return;
            }
            let (first, last, start, end) = visible_rows(self, state);
            let version = state.version() as i32;
            let query = InlayHintQuery {
                uri: lsp.uri.clone(),
                version,
                range: lang_api::TextSpan {
                    start: point_at(&state.store(), start),
                    end: point_at(&state.store(), end),
                },
            };
            (first, last, query, version, lsp.support.clone())
        };
        self.inlay_range = Some((first, last));
        self.inlays_task = Some(cx.spawn(async move |this, cx| {
            if !delay.is_zero() {
                cx.background_executor().timer(delay).await;
            }
            let Some(provider) = support.inlay_hints() else {
                return;
            };
            let mut replies = ask(provider, query);
            while let Some(answer) = replies.next().await {
                if answer.confidence == Confidence::Unknown {
                    return;
                }
                this.update(cx, |this, cx| {
                    this.apply_inlays(answer.value, version, cx);
                })
                .ok();
            }
        }));
    }

    /// A scroll or a jump asks for the hints of the new part unless the last answer
    /// already covers it.
    pub(crate) fn inlays_if_needed(&mut self, cx: &mut Context<Self>) {
        self.resolve_visible_code_lens(cx);
        let range = {
            let Some(state) = &self.state else {
                return;
            };
            if self.lsp.is_none() {
                return;
            }
            screen_rows(self, state)
        };
        if let Some((first, last)) = self.inlay_range
            && first <= range.0
            && range.1 <= last
        {
            return;
        }
        self.request_inlays(Duration::ZERO, cx);
    }

    /// Puts the hints of an answer in the text, keeping the kinds Settings shows.
    fn apply_inlays(&mut self, entries: Vec<InlayHintEntry>, version: i32, cx: &mut Context<Self>) {
        let parameter_names = self.options.inlay_parameter_names;
        let type_hints = self.options.inlay_type_hints;
        let Some(state) = &mut self.state else {
            return;
        };
        if state.version() as i32 != version {
            return;
        }
        let mut inlays = Vec::with_capacity(entries.len());
        let store = state.store();
        for entry in entries {
            let wanted = match entry.kind {
                InlayHintKind::Parameter => parameter_names != InlayParameterNames::None,
                InlayHintKind::Type => type_hints,
                // Some servers send names and types without a kind; the settings of the
                // request already narrowed what comes.
                InlayHintKind::Other => parameter_names != InlayParameterNames::None || type_hints,
            };
            if !wanted {
                continue;
            }
            let offset = offset_at(&store, entry.position);
            let mut text = String::new();
            if entry.padding_left {
                text.push(' ');
            }
            text.push_str(&entry.label);
            if entry.padding_right {
                text.push(' ');
            }
            inlays.push(display_map::Inlay { offset, text });
        }
        state.set_inlays(inlays);
        cx.notify();
    }
}

/// The first and last rows on the screen, without the room around them.
fn screen_rows(view: &EditorView, state: &EditorState) -> (usize, usize) {
    let display = state.display();
    let line_height = view.style.line_height.max(1.);
    let shown = (f32::from(view.viewport.height) / line_height).ceil() as usize;
    let rows = if shown == 0 { 40 } else { shown };
    let top = (view.scroll_top / line_height) as usize;
    let last = display.row_count().saturating_sub(1);
    (top.min(last), (top + rows).min(last))
}

/// The first and last rows of the part of the document the editor shows with the room
/// to scroll into, and its byte range.
fn visible_rows(view: &EditorView, state: &EditorState) -> (usize, usize, usize, usize) {
    let display = state.display();
    let line_height = view.style.line_height.max(1.);
    let shown = (f32::from(view.viewport.height) / line_height).ceil() as usize;
    // Before the first paint the height is unknown: a screen of the usual size is asked.
    let rows = if shown == 0 { 40 } else { shown };
    let top = (view.scroll_top / line_height) as usize;
    let margin = rows * INLAY_MARGIN_SCREENS + 4;
    let first = top.saturating_sub(margin);
    let last = (top + rows + margin).min(display.row_count().saturating_sub(1));
    let start = display.line(first).start_offset;
    let end = display.line(last).end_offset;
    (first, last, start, end)
}

/// The first answer of `replies`, `None` when the question went stale.
pub(crate) async fn first<T>(mut replies: Replies<T>) -> Option<(T, Confidence)> {
    let answer = replies.next().await?;
    Some((answer.value, answer.confidence))
}

/// Keeps the active parameter inside the overload the user picked.
fn maybe_clamp_parameter(popup: &mut SignaturePopup) {
    let count = popup.signature().parameters.len();
    popup.parameter = popup.parameter.min(count.saturating_sub(1));
}

impl EditorView {
    /// Text for a made-up path that a navigation opened.
    pub(crate) fn source_text(path: &Path, cx: &App) -> Option<Arc<str>> {
        cx.try_global::<Sources>()?
            .0
            .get(path)
            .map(|doc| doc.text.clone())
    }

    /// The origin of the text of a made-up path: a class of a library has one.
    pub fn source_origin(path: &Path, cx: &App) -> Option<String> {
        let origin = cx.try_global::<Sources>()?.0.get(path)?.origin.clone()?;
        Some(origin.to_string())
    }

    /// Takes new text for a made-up path, for a document that was read again.
    pub(crate) fn set_source_text(path: &Path, text: &str, cx: &mut App) {
        let origin = cx
            .try_global::<Sources>()
            .and_then(|sources| sources.0.get(path))
            .and_then(|doc| doc.origin.clone());
        let Some(origin) = origin else {
            return;
        };
        cx.default_global::<Sources>().0.insert(
            path.to_path_buf(),
            SourceDoc {
                text: text.into(),
                origin: Some(origin),
            },
        );
    }

    /// Opens the document at the server once it is loaded, and follows its diagnostics.
    pub(crate) fn attach(&mut self, cx: &mut Context<Self>) {
        // A project language also serves other files of its project, such as
        // configuration files, through the languages it brings for them. A file that no
        // project language owns may still belong to a tool, such as a console.
        let mut support = None;
        if let Some(semantic) = cx.try_global::<Semantic>() {
            let main = semantic.0.clone();
            if main.matches(&self.path) {
                support = Some(main);
            } else if let Some(language) = main
                .embedded_languages()
                .into_iter()
                .find(|language| language.matches(&self.path))
            {
                support = Some(language);
            }
        }
        if support.is_none() {
            support = cx.try_global::<FileLanguages>().and_then(|files| {
                files
                    .0
                    .iter()
                    .find(|file| file.matches(&self.path))
                    .cloned()
            });
        }
        let Some(support) = support else {
            return;
        };
        let Some(state) = &self.state else {
            return;
        };
        if state.is_read_only() {
            return;
        }
        let Ok(canonical) = CanonPath::new(&self.path) else {
            return;
        };
        let uri = canonical.to_uri();
        if let Some(server) = support.server() {
            server.open(&uri, state.version() as i32, state.text().into());
        }
        let follows_diagnostics = support.diagnostics().is_some();
        if let Some(diagnostics) = support.diagnostics() {
            let mut replies = ask(diagnostics, DiagnosticsQuery(uri.clone()));
            cx.spawn(async move |this, cx| {
                while let Some(answer) = replies.next().await {
                    let shown = this.update(cx, |this, cx| this.set_marks(answer.value, cx));
                    if shown.is_err() {
                        break;
                    }
                }
            })
            .detach();
        }
        self.lsp = Some(Lsp {
            support,
            uri,
            marks: Vec::new(),
            server_marks: Vec::new(),
            native_marks: HashMap::new(),
            server_tokens: Vec::new(),
            native_tokens: HashMap::new(),
            received: !follows_diagnostics,
            completion: None,
            completion_doc: None,
            navigation_list: None,
            navigation_cache: HashMap::new(),
            note: None,
            quick: None,
            actions: None,
            rename: None,
            move_to: None,
            overrides: None,
            definition: None,
            generation: None,
            safe_delete: None,
            highlights: Vec::new(),
            highlight_asked: 0,
            signatures: None,
            signature_asked: 0,
            parameter_walk: None,
            asked: 0,
            answered: 0,
            nav_asked: 0,
            nav_answered: 0,
            doc_asked: 0,
            case: self.options.completion_case,
        });
        self.semantic_tokens_now(cx);
        if let Some(lsp) = &self.lsp {
            lsp.support
                .set_inlay_hint_settings(inlay_settings(&self.options));
            lsp.support
                .set_code_vision_settings(code_vision_settings(&self.options));
        }
        self.request_inlays(Duration::ZERO, cx);
        self.request_code_lens(Duration::ZERO, cx);
        self.request_doc_comments(cx);
    }

    /// Asks the languages of the window for this file again after the application made
    /// one of them match it, such as the source picked for a SQL script.
    pub fn attach_support(&mut self, cx: &mut Context<Self>) {
        self.attach(cx);
    }

    /// Asks for the semantic tokens once the file is open. The whole document is asked
    /// for; the visible part comes later for a big one.
    pub(crate) fn semantic_tokens_now(&mut self, cx: &mut Context<Self>) {
        self.semantic_tokens_after(0, cx);
    }

    /// Asks for the semantic tokens after a pause, so that typing does not send one
    /// request per key. A newer ask replaces the older one.
    pub(crate) fn semantic_tokens_later(&mut self, cx: &mut Context<Self>) {
        self.semantic_tokens_after(TOKENS_DELAY_MS, cx);
    }

    /// A scroll asks again only for a big document, whose tokens cover the part on
    /// screen; a small one is coloured whole already.
    pub(crate) fn semantic_tokens_scrolled(&mut self, cx: &mut Context<Self>) {
        let ranged = self
            .state
            .as_ref()
            .is_some_and(|state| state.store().line_count() > TOKENS_RANGE_LINES)
            && self
                .lsp
                .as_ref()
                .is_some_and(|lsp| lsp.support.semantic_tokens_range());
        if ranged {
            self.semantic_tokens_later(cx);
        }
    }

    fn semantic_tokens_after(&mut self, delay: u64, cx: &mut Context<Self>) {
        if !self.options.semantic_highlighting {
            return;
        }
        let asked = self
            .lsp
            .as_ref()
            .is_some_and(|lsp| lsp.support.semantic_tokens().is_some());
        if !asked || self.state.is_none() {
            return;
        }
        self.tokens_asked = self.tokens_asked.wrapping_add(1);
        let generation = self.tokens_asked;
        cx.spawn(async move |this, cx| {
            if delay > 0 {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(delay))
                    .await;
            }
            let started = this.update(cx, |this, _| this.start_semantic_tokens(generation));
            let Ok(Some((mut replies, version))) = started else {
                return;
            };
            if let Some(answer) = replies.next().await {
                this.update(cx, |this, cx| {
                    let current = this.state.as_ref().map(|state| state.version() as i32);
                    if this.tokens_asked == generation && current == Some(version) {
                        this.apply_semantic_tokens(answer.value, cx);
                    }
                })
                .ok();
            }
        })
        .detach();
    }

    fn start_semantic_tokens(&self, generation: u64) -> Option<(Replies<Vec<SemanticToken>>, i32)> {
        if self.tokens_asked != generation {
            return None;
        }
        let lsp = self.lsp.as_ref()?;
        let provider = lsp.support.semantic_tokens()?;
        let state = self.state.as_ref()?;
        let version = state.version() as i32;
        let range = if lsp.support.semantic_tokens_range() {
            self.semantic_range(state)
        } else {
            None
        };
        let query = SemanticTokensQuery {
            uri: lsp.uri.clone(),
            version,
            range,
        };
        Some((ask(provider, query), version))
    }

    /// The part of a big document on screen, or `None` for a document small enough to
    /// colour whole.
    fn semantic_range(&self, state: &EditorState) -> Option<lang_api::TextSpan> {
        let lines = state.store().line_count();
        if lines <= TOKENS_RANGE_LINES {
            return None;
        }
        let line_height = self.style.line_height.max(1.);
        let display = state.display();
        let rows = display.row_count();
        if rows == 0 {
            return None;
        }
        let first = ((self.scroll_top / line_height).floor().max(0.) as usize).min(rows - 1);
        let visible = (f32::from(self.viewport.height) / line_height)
            .ceil()
            .max(1.) as usize;
        let last = (first + visible.max(TOKENS_RANGE_ROWS)).min(rows - 1);
        let start = display.to_offset(
            DisplayPoint {
                row: first,
                column: 0,
            },
            Bias::Left,
        );
        let end = display.to_offset(
            DisplayPoint {
                row: last,
                column: 0,
            },
            Bias::Right,
        );
        drop(display);
        Some(lang_api::TextSpan {
            start: point_at(&state.store(), start),
            end: point_at(&state.store(), end),
        })
    }

    fn apply_semantic_tokens(&mut self, tokens: Vec<SemanticToken>, cx: &mut Context<Self>) {
        if !self.options.semantic_highlighting {
            return;
        }
        let Some(state) = &self.state else {
            return;
        };
        let store = state.store();
        let spans: Vec<TokenSpan> = tokens
            .into_iter()
            .filter_map(|token| {
                let start = offset_at(&store, token.span.start);
                let end = offset_at(&store, token.span.end).max(start);
                (start < end).then_some(TokenSpan {
                    start: start as u32,
                    end: end as u32,
                    kind: token.kind,
                    modifiers: token.modifiers,
                })
            })
            .collect();
        if let Some(lsp) = &mut self.lsp {
            lsp.server_tokens = spans;
        }
        self.apply_tokens(cx);
    }

    /// The colour and the marks of the semantic token over `offset`. `None` when
    /// Settings turn the tokens off or no token covers the place.
    pub fn semantic_look_at(&self, offset: usize) -> Option<crate::style::SemanticLook> {
        if !self.options.semantic_highlighting {
            return None;
        }
        let token = self
            .state
            .as_ref()?
            .tokens()
            .iter()
            .find(|token| (token.start as usize) <= offset && offset < token.end as usize)?;
        self.style.token_style(token.kind, token.modifiers)
    }

    fn set_marks(&mut self, entries: Vec<DiagnosticEntry>, cx: &mut Context<Self>) {
        let (Some(lsp), Some(state)) = (&mut self.lsp, &self.state) else {
            return;
        };
        let store = state.store();
        lsp.server_marks = entries
            .iter()
            .map(|entry| {
                let start = offset_at(&store, entry.span.start);
                let end = offset_at(&store, entry.span.end).max(start);
                Mark {
                    range: start..end,
                    severity: entry.severity,
                    message: entry.message.clone(),
                    unnecessary: entry.unnecessary,
                    code: entry.code.clone(),
                }
            })
            .collect();
        self.apply_marks(cx);
    }

    /// Replaces the marks of a native model of the language, such as the checks of a
    /// framework. The marks of the server stay, so an edit of the model does not hide
    /// them.
    pub fn set_native_marks(&mut self, entries: Vec<DiagnosticEntry>, cx: &mut Context<Self>) {
        self.set_native_marks_from("language", entries, cx);
    }

    /// Replaces the marks of one native model of the language, keyed by the source that
    /// gave them, so that two models do not hide each other's checks. The marks of the
    /// server stay.
    pub fn set_native_marks_from(
        &mut self,
        key: &'static str,
        entries: Vec<DiagnosticEntry>,
        cx: &mut Context<Self>,
    ) {
        let (Some(lsp), Some(state)) = (&mut self.lsp, &self.state) else {
            return;
        };
        let store = state.store();
        let marks: Vec<Mark> = entries
            .iter()
            .map(|entry| {
                let start = offset_at(&store, entry.span.start);
                let end = offset_at(&store, entry.span.end).max(start);
                Mark {
                    range: start..end,
                    severity: entry.severity,
                    message: entry.message.clone(),
                    unnecessary: entry.unnecessary,
                    code: entry.code.clone(),
                }
            })
            .collect();
        if marks.is_empty() {
            lsp.native_marks.remove(key);
        } else {
            lsp.native_marks.insert(key, marks);
        }
        self.apply_marks(cx);
    }

    /// Replaces the tokens of one native model of the language, such as the keywords of
    /// a query inside a string. The tokens of the server stay.
    pub fn set_native_tokens(
        &mut self,
        key: &'static str,
        tokens: Vec<SemanticToken>,
        cx: &mut Context<Self>,
    ) {
        if !self.options.semantic_highlighting {
            return;
        }
        let Some(state) = &self.state else {
            return;
        };
        let store = state.store();
        let spans: Vec<TokenSpan> = tokens
            .into_iter()
            .filter_map(|token| {
                let start = offset_at(&store, token.span.start);
                let end = offset_at(&store, token.span.end).max(start);
                (start < end).then_some(TokenSpan {
                    start: start as u32,
                    end: end as u32,
                    kind: token.kind,
                    modifiers: token.modifiers,
                })
            })
            .collect();
        let Some(lsp) = &mut self.lsp else {
            return;
        };
        if spans.is_empty() {
            lsp.native_tokens.remove(key);
        } else {
            lsp.native_tokens.insert(key, spans);
        }
        self.apply_tokens(cx);
    }

    /// Joins the tokens of the server with the tokens of the native models.
    pub(crate) fn apply_tokens(&mut self, cx: &mut Context<Self>) {
        if !self.options.semantic_highlighting {
            return;
        }
        let Some(state) = &mut self.state else {
            return;
        };
        let len = state.store().len();
        let mut tokens = match &self.lsp {
            Some(lsp) => lsp.joined_tokens(),
            None => return,
        };
        normalize_tokens(&mut tokens, len);
        state.set_semantic_tokens(tokens);
        cx.notify();
    }

    /// Joins the marks of the server with the marks of a native model, and leaves out
    /// the checks the user turned off and the places a `//noinspection` comment covers.
    pub(crate) fn apply_marks(&mut self, cx: &mut Context<Self>) {
        let Some(state) = &self.state else {
            return;
        };
        let store = state.store();
        let settings = cx
            .try_global::<InspectionProfile>()
            .map(|profile| profile.0.clone())
            .unwrap_or_default();
        let suppressions = lang_api::Suppressions::from_text(&state.text());
        let Some(lsp) = &mut self.lsp else {
            return;
        };
        let mut marks = Vec::new();
        let mut entries = Vec::new();
        for mark in lsp
            .server_marks
            .iter()
            .chain(lsp.native_marks.values().flatten())
        {
            let line = store.offset_to_line(mark.range.start) as u32;
            let mut severity = mark.severity;
            if let Some(code) = mark.code.as_deref() {
                // The server names a compiler problem by its numeric `IProblem` id; the
                // settings and the `@SuppressWarnings` tokens use the key of the option.
                let inspection = lsp
                    .support
                    .inspection_of(Some(code), &mark.message)
                    .map(|info| info.id);
                let id = inspection.as_deref().unwrap_or(code);
                if suppressions.covers(code, line) || suppressions.covers(id, line) {
                    continue;
                }
                let level = settings
                    .inspections
                    .get(id)
                    .map(|setting| {
                        if setting.enabled {
                            setting
                                .level
                                .unwrap_or(lang_api::InspectionLevel::from_severity(mark.severity))
                        } else {
                            lang_api::InspectionLevel::Off
                        }
                    })
                    .unwrap_or_else(|| lang_api::InspectionLevel::from_severity(mark.severity));
                let Some(shown) = level.severity() else {
                    continue;
                };
                severity = shown;
            }
            marks.push(Mark {
                range: mark.range.clone(),
                severity,
                message: mark.message.clone(),
                unnecessary: mark.unnecessary,
                code: mark.code.clone(),
            });
            entries.push(DiagnosticEntry {
                span: lang_api::TextSpan {
                    start: point_at(&store, mark.range.start),
                    end: point_at(&store, mark.range.end),
                },
                severity,
                message: mark.message.clone(),
                unnecessary: mark.unnecessary,
                code: mark.code.clone(),
            });
        }
        lsp.marks = marks;
        lsp.received = true;
        cx.emit(EditorEvent::Diagnostics(entries));
        cx.notify();
    }

    /// Applies a change of the inspection settings to the marks that are on screen.
    pub fn reapply_inspections(&mut self, cx: &mut Context<Self>) {
        self.apply_marks(cx);
    }

    /// Tells the server about a new text and keeps an open completion in step with the
    /// caret.
    pub(crate) fn sync_server(&mut self, edited: bool) {
        let (Some(lsp), Some(state)) = (&mut self.lsp, &self.state) else {
            return;
        };
        if !edited {
            if lsp.completion.is_some()
                && state.selections().primary().head
                    < lsp.completion.as_ref().map_or(0, |c| c.start)
            {
                lsp.completion = None;
            }
            return;
        }
        if let Some(server) = lsp.support.server() {
            server.change(
                &lsp.uri,
                state.version() as i32,
                state.text().into(),
                Vec::new(),
            );
        }
        lsp.note = None;
        let caret = state.selections().primary().head;
        let case = lsp.case;
        if let Some(completion) = &mut lsp.completion {
            if caret < completion.start {
                lsp.completion = None;
            } else {
                let prefix = state.store().slice(completion.start..caret);
                if prefix.chars().all(is_word) {
                    completion.filter_with(&prefix, case);
                } else {
                    lsp.completion = None;
                }
            }
        }
    }

    pub(crate) fn saved_on_disk(&self) {
        if let Some(lsp) = &self.lsp
            && let Some(server) = lsp.support.server()
        {
            server.save(&lsp.uri);
        }
    }

    /// Typed text opens completion after a dot or on the first letter of a word, when
    /// Settings allow the list to open on its own.
    pub(crate) fn typed(&mut self, text: &str, cx: &mut Context<Self>) {
        let Some(lsp) = &self.lsp else {
            return;
        };
        if let Some(completion) = &lsp.completion {
            // The list filters itself as the prefix grows. An incomplete answer may
            // still hold entries beyond the ones it sent, so only such a list asks the
            // server again.
            let incomplete = !completion.complete;
            if incomplete {
                self.complete(false, cx);
            }
            return;
        }
        let auto = self.options.completion_auto_popup;
        self.completion_typed += 1;
        let generation = self.completion_typed;
        if !auto {
            return;
        }
        let opens = text == "." || (text.chars().count() == 1 && text.chars().all(is_word));
        if !opens {
            return;
        }
        let delay = self.options.completion_delay_ms;
        if delay == 0 {
            self.complete(false, cx);
            return;
        }
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(delay))
                .await;
            this.update(cx, |this, cx| {
                if this.completion_typed == generation
                    && this
                        .lsp
                        .as_ref()
                        .is_some_and(|lsp| lsp.completion.is_none())
                {
                    this.complete(false, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    pub fn spot(&self) -> Option<(DocumentSpot, usize)> {
        let (lsp, state) = (self.lsp.as_ref()?, self.state.as_ref()?);
        let caret = state.selections().primary().head;
        let spot = DocumentSpot {
            uri: lsp.uri.clone(),
            version: state.version() as i32,
            point: point_at(&state.store(), caret),
        };
        Some((spot, caret))
    }

    /// The place at `offset`, for a question that does not move the caret.
    pub(crate) fn spot_at(&self, offset: usize) -> Option<DocumentSpot> {
        let (lsp, state) = (self.lsp.as_ref()?, self.state.as_ref()?);
        Some(DocumentSpot {
            uri: lsp.uri.clone(),
            version: state.version() as i32,
            point: point_at(&state.store(), offset),
        })
    }

    /// A message about a question that found nothing, such as a jump without a place.
    /// It stands under the caret for a moment and then goes away by itself: the user is
    /// not asked to press a key to clear the screen.
    pub(crate) fn show_transient_note(
        &mut self,
        at: usize,
        text: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) {
        self.show_note(at, text, cx);
        let shown = self
            .lsp
            .as_ref()
            .and_then(|lsp| lsp.note.as_ref().map(|note| note.text.clone()));
        self.note_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(NOTE_LINGER_MS))
                .await;
            this.update(cx, |this, cx| {
                let still_shown = shown.is_some()
                    && this
                        .lsp
                        .as_ref()
                        .and_then(|lsp| lsp.note.as_ref())
                        .is_some_and(|note| Some(&note.text) == shown.as_ref());
                if !still_shown {
                    return;
                }
                if let Some(lsp) = &mut this.lsp {
                    lsp.note = None;
                }
                cx.notify();
            })
            .ok();
        }));
    }

    pub(crate) fn show_note(
        &mut self,
        at: usize,
        text: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) {
        if self.lsp.is_none() {
            return;
        }
        // A message replaces the one before it, and that one does not close this one.
        self.note_task = None;
        self.before_note_popup(cx);
        if let Some(lsp) = &mut self.lsp {
            lsp.quick = None;
            lsp.note = Some(Note {
                at,
                text: text.into(),
            });
            cx.notify();
        }
    }

    /// Ctrl+Space, or typing: asks for entries at the caret.
    pub(crate) fn complete(&mut self, explicit: bool, cx: &mut Context<Self>) {
        self.complete_with(CompletionMode::All, explicit, cx);
    }

    /// Ctrl+Shift+Space and Ctrl+Alt+Space: the usual list narrowed to the expected type
    /// or to types only.
    pub(crate) fn complete_with(
        &mut self,
        mode: CompletionMode,
        explicit: bool,
        cx: &mut Context<Self>,
    ) {
        self.before_completion_popup(cx);
        let Some((spot, caret)) = self.spot() else {
            return;
        };
        // A new question replaces the old one: the answer of the old text or the old
        // prefix would only be dropped, and the server should not keep computing it.
        self.completion_task = None;
        self.completion_doc_task = None;
        let case = self.options.completion_case;
        // A server that offers postfix snippets knows the types better than the local
        // templates; they are a fallback for a language without such a server.
        let server_postfix = self
            .lsp
            .as_ref()
            .is_some_and(|lsp| lsp.support.completion().is_some());
        let (Some(lsp), Some(state)) = (&mut self.lsp, &self.state) else {
            return;
        };
        lsp.completion_doc = None;
        let start = word_start(&state.store(), caret);
        let line_start = state
            .store()
            .line_to_offset(state.store().offset_to_line(caret));
        let before = state.store().slice(line_start..caret);
        let mut local: Vec<CompletionEntry> = if mode == CompletionMode::All {
            cx.try_global::<CompletionSources>()
                .map(|sources| sources.0.clone())
                .unwrap_or_default()
                .iter()
                .flat_map(|source| source.entries(&self.path, &before, spot.point))
                .collect()
        } else {
            Vec::new()
        };
        if mode == CompletionMode::All && !server_postfix {
            local.extend(self.live.postfix_completions(state));
        }
        lsp.asked += 1;
        let asked = lsp.asked;
        lsp.parameter_walk = None;
        let support = lsp.support.clone();
        if !local.is_empty() {
            let completion = Completion::new(
                start,
                local.clone(),
                true,
                &state.store().slice(start..caret),
                case,
                support.preferred_completion_names(),
            );
            lsp.completion = (!completion.shown.is_empty()).then_some(completion);
            cx.notify();
        }
        if support.completion().is_none() {
            lsp.answered = asked;
            return;
        }
        let expected_spot = spot.clone();
        self.completion_task = Some(cx.spawn(async move |this, cx| {
            let expected = if mode == CompletionMode::SmartType {
                match support.expected_type() {
                    Some(provider) => first(ask(provider, expected_spot))
                        .await
                        .and_then(|(wanted, _)| wanted.type_name),
                    None => None,
                }
            } else {
                None
            };
            let Some(provider) = support.completion() else {
                return;
            };
            let replies = ask(provider, spot);
            let answer = first(replies).await;
            this.update(cx, |this, cx| {
                if let Some(lsp) = &mut this.lsp
                    && lsp.asked == asked
                {
                    lsp.answered = asked;
                }
                let Some((list, confidence)) = answer else {
                    return;
                };
                // A server that sends no postfix snippets leaves that part of the list to
                // the templates of the editor, which carry their descriptions.
                let fallback = if mode == CompletionMode::All
                    && !list.entries.iter().any(is_postfix_snippet)
                {
                    this.state
                        .as_ref()
                        .map_or_else(Vec::new, |state| this.live.postfix_completions(state))
                } else {
                    Vec::new()
                };
                let caret = this.spot().map(|(_, caret)| caret);
                let (Some(lsp), Some(state), Some(caret)) = (&mut this.lsp, &this.state, caret)
                else {
                    return;
                };
                if lsp.asked != asked || caret < start {
                    return;
                }
                if confidence == Confidence::Unknown && explicit {
                    lsp.note = Some(Note {
                        at: caret,
                        text: NO_ANSWER.into(),
                    });
                }
                let mut local = local;
                local.extend(fallback);
                let entries = visible_entries(list.entries, mode, expected.as_deref());
                let entries = merge_completions(local, entries);
                let completion = Completion::new(
                    start,
                    entries,
                    list.complete,
                    &state.store().slice(start..caret),
                    case,
                    support.preferred_completion_names(),
                );
                lsp.completion = (!completion.shown.is_empty()).then_some(completion);
                cx.notify();
                this.request_completion_doc(cx);
            })
            .ok();
        }));
        self.request_completion_doc(cx);
    }

    /// Asks the server for the documentation of the selected entry in the background. The
    /// list is already on screen and does not wait; a late answer for another entry is
    /// dropped. The answer may also carry the additional edits of the entry, which are
    /// kept for the insertion instead of being asked for again.
    fn request_completion_doc(&mut self, cx: &mut Context<Self>) {
        let Some((entry, index, support)) = self.lsp.as_ref().and_then(|lsp| {
            let completion = lsp.completion.as_ref()?;
            let index = *completion.shown.get(completion.selected)?;
            let entry = completion.entries.get(index)?.clone();
            Some((entry, index, lsp.support.clone()))
        }) else {
            return;
        };
        if self
            .lsp
            .as_ref()
            .and_then(|lsp| lsp.completion_doc.as_ref())
            .is_some_and(|doc| doc.entry == index)
        {
            return;
        }
        if let Some(documentation) = &entry.documentation {
            let text = documentation.trim();
            if !text.is_empty()
                && let Some(lsp) = &mut self.lsp
            {
                lsp.completion_doc = Some(CompletionDoc::new(index, text));
                cx.notify();
            }
            return;
        }
        if entry.resolve.is_none() || support.completion_resolve().is_none() {
            return;
        }
        let (asked, entry) = {
            let Some(lsp) = &mut self.lsp else {
                return;
            };
            lsp.doc_asked += 1;
            (lsp.doc_asked, entry)
        };
        self.completion_doc_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(COMPLETION_DOC_DELAY_MS))
                .await;
            let Some(provider) = support.completion_resolve() else {
                return;
            };
            let mut replies = ask(provider, entry);
            let Some(answer) = replies.next().await else {
                return;
            };
            this.update(cx, |this, cx| {
                let Some(lsp) = &mut this.lsp else {
                    return;
                };
                if lsp.doc_asked != asked {
                    return;
                }
                let selected = lsp
                    .completion
                    .as_ref()
                    .and_then(|completion| completion.shown.get(completion.selected).copied());
                if selected != Some(index) {
                    return;
                }
                // The edits the server sends with the documentation are used when the
                // user accepts the entry, unless the answer of the list already had them.
                if let Some(entry) = lsp
                    .completion
                    .as_mut()
                    .and_then(|completion| completion.entries.get_mut(index))
                    && entry.additional_edits.is_empty()
                {
                    entry.additional_edits = answer.value.additional_edits.clone();
                }
                let text = answer.value.documentation.unwrap_or_default();
                let text = text.trim();
                if text.is_empty() {
                    return;
                }
                lsp.completion_doc = Some(CompletionDoc::new(index, text));
                cx.notify();
            })
            .ok();
        }));
    }

    /// Keys of an open completion: arrows and pages move, Home and End go to the ends,
    /// Enter inserts before the rest of the word, Tab replaces that rest, Ctrl+. inserts
    /// the entry with a dot and opens the list again, Ctrl+Shift+Enter finishes the
    /// statement. Returns whether the key was taken; Escape is closed by the popup rules
    /// of the view.
    pub(crate) fn completion_key(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
        let Some(lsp) = &mut self.lsp else {
            return false;
        };
        let Some(completion) = &mut lsp.completion else {
            return false;
        };
        let page = MAX_SHOWN as isize;
        match key {
            "up" => {
                completion.step_selection(-1);
                cx.notify();
                self.request_completion_doc(cx);
                return true;
            }
            "down" => {
                completion.step_selection(1);
                cx.notify();
                self.request_completion_doc(cx);
                return true;
            }
            "pageup" => {
                completion.step_selection(-page);
                cx.notify();
                self.request_completion_doc(cx);
                return true;
            }
            "pagedown" => {
                completion.step_selection(page);
                cx.notify();
                self.request_completion_doc(cx);
                return true;
            }
            "home" => {
                completion.jump_selection(0);
                cx.notify();
                self.request_completion_doc(cx);
                return true;
            }
            "end" => {
                completion.jump_selection(isize::MAX);
                cx.notify();
                self.request_completion_doc(cx);
                return true;
            }
            "enter" | "tab" | "ctrl-." | "ctrl-shift-enter" => {}
            _ => return false,
        }
        let replace = key == "tab" || key == "ctrl-.";
        let dot = key == "ctrl-.";
        let complete_statement = key == "ctrl-shift-enter";
        let start = completion.start;
        let entry = completion.chosen_entry();
        lsp.completion = None;
        lsp.completion_doc = None;
        let support = lsp.support.clone();
        self.completion_task = None;
        self.completion_doc_task = None;
        let Some(entry) = entry else {
            return true;
        };
        let Some(state) = &self.state else {
            return true;
        };
        let caret = state.head();
        let start = entry
            .span
            .map_or(start, |span| offset_at(&state.store(), span.start))
            .min(caret);
        let end = if replace {
            word_end(&state.store(), caret).max(caret)
        } else {
            caret
        };
        let resolve = entry.additional_edits.is_empty()
            && entry.resolve.is_some()
            && support.completion_resolve().is_some();
        let options = AcceptOptions {
            complete_statement,
            dot,
        };
        if resolve {
            self.accept_with_resolve(entry, start, end, options, support, cx);
        } else {
            self.accept_completion(&entry, start, end, &entry.additional_edits, options, cx);
        }
        if dot {
            self.complete(true, cx);
        }
        cx.notify();
        true
    }

    /// A click on the row `row` of the completion list selects it; a double click also
    /// puts the entry in, as IDEA does. The row is counted from the first row on screen.
    pub(crate) fn completion_click(
        &mut self,
        row: usize,
        click_count: usize,
        cx: &mut Context<Self>,
    ) {
        let Some(completion) = self.lsp.as_mut().and_then(|lsp| lsp.completion.as_mut()) else {
            return;
        };
        if row >= completion.shown.len() {
            return;
        }
        completion.selected = row;
        completion.reveal_selected();
        cx.notify();
        self.request_completion_doc(cx);
        if click_count >= 2 {
            self.completion_key("enter", cx);
        }
    }

    /// A wheel notch over the completion list: the rows scroll under the selection,
    /// which stays where the user put it.
    pub(crate) fn completion_scroll(&mut self, rows: isize, cx: &mut Context<Self>) {
        let Some(completion) = self.lsp.as_mut().and_then(|lsp| lsp.completion.as_mut()) else {
            return;
        };
        let before = completion.scroll;
        completion.scroll_by(rows);
        if completion.scroll != before {
            cx.notify();
        }
    }

    /// Ctrl+Up and Ctrl+Down in an open Parameter Info: the previous or next overload.
    pub(crate) fn overload_key(&mut self, up: bool, cx: &mut Context<Self>) -> bool {
        let Some(lsp) = &mut self.lsp else {
            return false;
        };
        let Some(popup) = &mut lsp.signatures else {
            return false;
        };
        let count = popup.help.signatures.len();
        if count == 0 {
            lsp.signatures = None;
            return false;
        }
        popup.selected = if up {
            (popup.selected + count - 1) % count
        } else {
            (popup.selected + 1) % count
        };
        maybe_clamp_parameter(popup);
        cx.notify();
        true
    }

    /// Puts an entry in with its additional edits, as one step of undo. A snippet keeps
    /// its fields open, the first one selected; the caret of a plain entry lands after
    /// the text, moved by the additional edits above it.
    fn accept_completion(
        &mut self,
        entry: &CompletionEntry,
        start: usize,
        end: usize,
        additional: &[DocumentEdit],
        options: AcceptOptions,
        cx: &mut Context<Self>,
    ) {
        self.catch_up(cx);
        let Some(state) = &self.state else {
            return;
        };
        let start = start.min(state.store().len());
        let (indent, unit) = self.live.snippet_indent(state, start);
        let mut expansion = expand_entry(entry, &indent, &unit);
        // Ctrl+. leaves the caret after a dot and asks for the members.
        if options.dot {
            expansion.text.push('.');
            expansion.end = expansion.text.len();
        }
        let text = expansion.text.clone();
        // The input point of the first field, for the parameters of a call.
        let field_at = entry
            .snippet
            .then(|| {
                expansion
                    .fields
                    .first()
                    .map_or(expansion.end, |field| field.ranges[0].start)
            })
            .filter(|at| *at <= text.len());
        let Some(state) = &mut self.state else {
            return;
        };
        let end = end.max(start).min(state.store().len());
        let store = state.store();
        let mut shift = 0isize;
        let mut edits: Vec<Edit> = Vec::new();
        for edit in additional {
            let from = offset_at(&store, edit.span.start);
            let to = offset_at(&store, edit.span.end).max(from);
            // A server sends its additional edits elsewhere; one over the insertion is
            // dropped rather than applied twice.
            if from < end && start < to {
                continue;
            }
            if to <= start {
                shift += edit.text.len() as isize - (to - from) as isize;
            }
            edits.push(Edit::new(from..to, edit.text.clone()));
        }
        edits.push(Edit::new(start..end, text.clone()));
        edits.sort_by_key(|edit| (edit.range.start, edit.range.end));
        let at = (start as isize + shift).max(0) as usize;
        let after = match expansion.fields.first() {
            Some(field) => {
                let range = &field.ranges[0];
                Selection::new(at + range.start, at + range.end)
            }
            None => Selection::caret(at + expansion.end),
        };
        let after = SelectionSet::single(after);
        state.seal_undo();
        let change = state.apply_template_edits(edits, after, EditKind::Typing);
        let inserted = change.is_some();
        if inserted {
            self.live.start_session(&expansion, at);
        }
        self.changed(change, cx);
        // An inserted call without snippet fields: Tab and Shift+Tab walk the arguments.
        if inserted
            && !options.dot
            && expansion.fields.is_empty()
            && entry.label.contains('(')
            && let Some(walk) = ParameterWalk::new(&text, at, at + text.len())
            && let Some(lsp) = &mut self.lsp
        {
            lsp.parameter_walk = Some(walk);
        }
        // The caret inside the brackets of a call: show what goes there.
        if field_at.is_some_and(|at| text[..at].ends_with('(')) {
            self.parameter_info(cx);
        }
        if let Some(command) = &entry.command {
            self.run_completion_command(command, cx);
        }
        if options.complete_statement {
            self.run(actions::COMPLETE_STATEMENT, cx);
        }
    }

    /// Accepts an entry whose additional edits the server sends on resolve. The entry
    /// goes in at once, so typing on never waits for the server or loses the entry; the
    /// edits follow when the answer comes, if their places are intact. A quick answer
    /// joins the insertion in one step of undo.
    fn accept_with_resolve(
        &mut self,
        entry: CompletionEntry,
        start: usize,
        end: usize,
        options: AcceptOptions,
        support: Arc<dyn LanguageSupport>,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = &self.state else {
            return;
        };
        let before = state.text();
        let Some(provider) = support.completion_resolve() else {
            self.accept_completion(&entry, start, end, &[], options, cx);
            return;
        };
        let mut replies = ask(provider, entry.clone());
        let inserted = {
            let (indent, unit) = self.live.snippet_indent(state, start);
            let mut expansion = expand_entry(&entry, &indent, &unit);
            if options.dot {
                expansion.text.push('.');
            }
            expansion.text.len()
        };
        self.accept_completion(&entry, start, end, &[], options, cx);
        let version = self.state.as_ref().map_or(0, |state| state.version());
        let late = LateEdit {
            start,
            end,
            inserted,
            version,
        };
        cx.spawn(async move |this, cx| {
            let Some(answer) = replies.next().await else {
                return;
            };
            let additional = answer.value.additional_edits;
            this.update(cx, |this, cx| {
                this.apply_late_edits(&before, late, &additional, cx);
            })
            .ok();
        })
        .detach();
    }

    /// The edits of a late resolve, if the text where they belong is what it was.
    fn apply_late_edits(
        &mut self,
        before: &str,
        late: LateEdit,
        additional: &[DocumentEdit],
        cx: &mut Context<Self>,
    ) {
        self.catch_up(cx);
        let Some(state) = &mut self.state else {
            return;
        };
        let LateEdit {
            start,
            end,
            inserted,
            version,
        } = late;
        let current = state.text();
        let delta = inserted as isize - (end - start) as isize;
        let mut edits = Vec::new();
        for edit in additional {
            let from = offset_in(before, edit.span.start);
            let to = offset_in(before, edit.span.end).max(from);
            let new_from = if from >= end {
                (from as isize + delta).max(0) as usize
            } else {
                from
            };
            let new_to = new_from + (to - from);
            if new_to > current.len()
                || !current.is_char_boundary(new_from)
                || !current.is_char_boundary(new_to)
            {
                continue;
            }
            if current[new_from..new_to] != before[from..to] {
                continue;
            }
            edits.push(Edit::new(new_from..new_to, edit.text.clone()));
        }
        if edits.is_empty() {
            return;
        }
        edits.sort_by_key(|edit| (edit.range.start, edit.range.end));
        let head = state.head();
        let shift: isize = edits
            .iter()
            .filter(|edit| edit.range.end <= head)
            .map(|edit| edit.text.len() as isize - edit.range.len() as isize)
            .sum();
        let after = SelectionSet::single(Selection::caret((head as isize + shift).max(0) as usize));
        let unchanged = state.version() == version;
        let change = state.apply_template_edits(edits, after, EditKind::Command);
        // A late answer with no typing in between belongs to the insertion, even when it
        // came after the window of the typing group.
        if change.is_some() && unchanged {
            state.merge_last_undo(2);
        }
        self.changed_late(change, cx);
    }

    /// The command of an entry: two of the editor's own actions run here, everything
    /// else goes to the server. A command without a server behind it is skipped.
    fn run_completion_command(&mut self, command: &CompletionCommand, cx: &mut Context<Self>) {
        match command.command.as_str() {
            "editor.action.triggerParameterHints" => self.parameter_info(cx),
            "editor.action.triggerSuggest" => self.complete(true, cx),
            _ => {
                let Some(provider) = self
                    .lsp
                    .as_ref()
                    .and_then(|lsp| lsp.support.execute_command())
                else {
                    eprintln!("completion command {} is not supported", command.command);
                    return;
                };
                let replies = ask(
                    provider,
                    CommandQuery {
                        command: command.command.clone(),
                        arguments: command.arguments.clone(),
                    },
                );
                cx.spawn(async move |_, _| {
                    let _ = first(replies).await;
                })
                .detach();
            }
        }
    }

    /// Ctrl+B and Ctrl+Alt+B. One target goes at once; several wait for a pick from the
    /// list. While the server is not ready, Ctrl+B asks the project index instead.
    pub(crate) fn navigate(&mut self, kind: NavigationKind, cx: &mut Context<Self>) {
        let Some((spot, caret)) = self.spot() else {
            return;
        };
        let word = self
            .state
            .as_ref()
            .map(|state| {
                let store = state.store();
                store.slice(word_start(&store, caret)..word_end(&store, caret))
            })
            .filter(|word| !word.is_empty());
        let Some((support, ready)) = self
            .lsp
            .as_ref()
            .map(|lsp| (lsp.support.clone(), lsp.support.server_ready()))
        else {
            return;
        };
        let fallback = (kind == NavigationKind::Definition && !ready)
            .then(|| {
                cx.try_global::<SymbolLookup>()
                    .and_then(|lookup| lookup.0.clone())
            })
            .flatten()
            .zip(word.clone());
        // While the server imports, the project index answers at once. Only when it has
        // no such name does the question go to the server.
        if let Some((lookup, word)) = &fallback
            && let Some(target) = lookup(word)
        {
            self.go_to_location(&target, cx);
            return;
        }
        let key = (spot.uri.clone(), spot.version, spot.point, kind);
        if let Some(spans) = self
            .lsp
            .as_ref()
            .and_then(|lsp| lsp.navigation_cache.get(&key))
            .cloned()
        {
            self.show_navigation_targets(spans, caret, cx);
            return;
        }
        let Some(provider) = support.navigation() else {
            if let Some((lookup, word)) = fallback {
                if let Some(target) = lookup(&word) {
                    self.go_to_location(&target, cx);
                } else {
                    self.show_transient_note(caret, "Cannot find declaration to go to", cx);
                }
            }
            return;
        };
        let asked = match &mut self.lsp {
            Some(lsp) => {
                lsp.nav_asked += 1;
                lsp.nav_asked
            }
            None => return,
        };
        let replies = ask(provider, NavigationQuery { kind, spot });
        cx.spawn(async move |this, cx| {
            let answer = first(replies).await;
            this.update(cx, |this, cx| {
                if let Some(lsp) = &mut this.lsp
                    && lsp.nav_asked == asked
                {
                    lsp.nav_answered = asked;
                }
                match answer {
                    Some((spans, _)) if !spans.is_empty() => {
                        // Ctrl+B on the name of a declaration names the declaration
                        // itself: the uses answer instead of a jump to the same place.
                        if kind == NavigationKind::Definition
                            && this.on_the_declaration(&spans, caret)
                        {
                            this.usages_of_declaration(caret, cx);
                            return;
                        }
                        if let Some(lsp) = &mut this.lsp {
                            // The same press on the same text is answered at once, without
                            // asking the server to read the class again.
                            lsp.navigation_cache.insert(key, spans.clone());
                        }
                        this.show_navigation_targets(spans, caret, cx);
                    }
                    Some((_, confidence))
                        if confidence != Confidence::Unknown
                            && this.lsp.as_ref().is_some_and(|lsp| {
                                lsp.support.definition_falls_back_to_hover()
                            }) =>
                    {
                        // A language whose places map onto configuration files shows what
                        // it knows about the place rather than nothing.
                        this.quick_doc(cx)
                    }
                    Some((_, Confidence::Unknown)) if fallback.is_some() => {
                        let (lookup, word) = fallback.unwrap();
                        if let Some(target) = lookup(&word) {
                            this.go_to_location(&target, cx);
                        } else {
                            this.show_transient_note(caret, NO_ANSWER, cx);
                        }
                    }
                    Some((_, Confidence::Unknown)) => {
                        this.show_transient_note(caret, NO_ANSWER, cx)
                    }
                    Some(_) if fallback.is_some() => {
                        let (lookup, word) = fallback.unwrap();
                        if let Some(target) = lookup(&word) {
                            this.go_to_location(&target, cx);
                        } else {
                            this.show_transient_note(caret, "Cannot find declaration to go to", cx);
                        }
                    }
                    Some(_) => {
                        this.show_transient_note(caret, "Cannot find declaration to go to", cx)
                    }
                    None if fallback.is_some() => {
                        let (lookup, word) = fallback.unwrap();
                        if let Some(target) = lookup(&word) {
                            this.go_to_location(&target, cx);
                        }
                    }
                    None => {}
                }
            })
            .ok();
        })
        .detach();
    }

    /// Whether the one place a navigation answer gives is the name the caret stands on:
    /// the definition of a declaration is the declaration itself.
    fn on_the_declaration(&self, spans: &[SourceSpan], caret: usize) -> bool {
        let [target] = spans else {
            return false;
        };
        let Some(state) = &self.state else {
            return false;
        };
        let here = uri_path(&target.uri)
            .is_some_and(|path| platform::path_key(&path) == platform::path_key(&self.path));
        if !here {
            return false;
        }
        let from = offset_at(&state.store(), target.span.start);
        let to = offset_at(&state.store(), target.span.end).max(from);
        from <= caret && caret <= to
    }

    /// Opens one place of a navigation answer at once, or shows the list of several,
    /// with the one the user picks opened by the same path as one target.
    fn show_navigation_targets(
        &mut self,
        spans: Vec<SourceSpan>,
        caret: usize,
        cx: &mut Context<Self>,
    ) {
        if spans.len() == 1 {
            self.go_to_location(&spans[0], cx);
            return;
        }
        self.before_navigation_popup(cx);
        if let Some(lsp) = &mut self.lsp {
            lsp.navigation_list = Some(NavigationList::new(caret, spans));
            cx.notify();
        }
    }

    /// Keys of the list of places from Ctrl+B: arrows move, Enter opens the place and
    /// Escape closes the list. Returns whether the key was taken.
    pub(crate) fn navigation_key(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
        let Some(lsp) = &mut self.lsp else {
            return false;
        };
        let Some(list) = &mut lsp.navigation_list else {
            return false;
        };
        let rows = list.targets.len();
        match key {
            "up" if rows > 0 => {
                list.selected = (list.selected + rows - 1) % rows;
                cx.notify();
                true
            }
            "down" if rows > 0 => {
                list.selected = (list.selected + 1) % rows;
                cx.notify();
                true
            }
            "enter" => {
                let target = list.target();
                let target = target.cloned();
                let at = list.at;
                lsp.navigation_list = None;
                match target {
                    Some(target) => self.go_to_location(&target, cx),
                    None => self.show_transient_note(at, "Cannot find declaration to go to", cx),
                }
                true
            }
            "escape" => {
                lsp.navigation_list = None;
                cx.notify();
                true
            }
            _ => false,
        }
    }

    pub(crate) fn go_to_location(&mut self, target: &SourceSpan, cx: &mut Context<Self>) {
        let caret = self.spot().map_or(0, |(_, caret)| caret);
        // A place of this very text moves the caret here: no tab and no reveal take part.
        // The jump tells the window where it starts from before the caret moves, so Back
        // returns to that place as it does after a jump to another file.
        let here = uri_path(&target.uri)
            .is_some_and(|path| platform::path_key(&path) == platform::path_key(&self.path));
        if here && let Some(state) = &self.state {
            let at = offset_at(&state.store(), target.span.start);
            // The window learns where the jump starts while the caret is still here.
            cx.emit(EditorEvent::Navigate(self.path.clone()));
            let moves = cx.weak_entity();
            cx.defer(move |cx| {
                moves
                    .update(cx, |this, cx| {
                        if let Some(state) = &mut this.state {
                            state.select_range(at..at);
                        }
                        this.autoscroll = true;
                        this.reveal_center = true;
                        this.caret_on = true;
                        this.changed(None, cx);
                        cx.notify();
                    })
                    .ok();
            });
            return;
        }
        let extension = self
            .path
            .extension()
            .map(|extension| extension.to_string_lossy().into_owned())
            .unwrap_or_default();
        let this = cx.weak_entity();
        let missing = this.clone();
        show_location(
            target,
            &extension,
            cx,
            move |path, cx| {
                this.update(cx, |_, cx| cx.emit(EditorEvent::Navigate(path)))
                    .ok();
            },
            move |cx| {
                missing
                    .update(cx, |this, cx| {
                        this.show_transient_note(caret, "No source for this place", cx)
                    })
                    .ok();
            },
        );
    }

    /// The place, the problem and the language for the Documentation tool window.
    pub fn documentation_query(&self) -> Option<crate::DocumentationQuery> {
        let lsp = self.lsp.as_ref()?;
        lsp.support.hover()?;
        let (spot, offset) = self.spot()?;
        Some(crate::DocumentationQuery {
            spot,
            offset,
            diagnostic: self.diagnostic_at(offset),
            support: lsp.support.clone(),
        })
    }

    /// Ctrl+Q. The second press pins the popup to the Documentation tool window.
    pub(crate) fn quick_doc(&mut self, cx: &mut Context<Self>) {
        if self.toggle_quick_doc(cx) {
            return;
        }
        self.before_quick_doc_popup(cx);
        let Some((spot, caret)) = self.spot() else {
            return;
        };
        let Some(provider) = self.lsp.as_ref().and_then(|lsp| lsp.support.hover()) else {
            return;
        };
        let replies = ask(provider, spot);
        cx.spawn(async move |this, cx| {
            let answer = first(replies).await;
            this.update(cx, |this, cx| match answer {
                Some((text, _)) if !text.0.trim().is_empty() => {
                    let blocks = crate::quick_doc::parse(&text.0);
                    let diagnostic = this.diagnostic_at(caret);
                    this.show_quick_doc(caret, blocks, diagnostic, None, cx);
                }
                Some((_, Confidence::Unknown)) => this.show_note(caret, NO_ANSWER, cx),
                Some(_) => this.show_note(caret, "No documentation found", cx),
                None => {}
            })
            .ok();
        })
        .detach();
    }

    /// Ctrl+P: the overloads of the call around the caret, the active parameter bold.
    pub(crate) fn parameter_info(&mut self, cx: &mut Context<Self>) {
        self.before_parameter_info_popup(cx);
        let Some((spot, caret)) = self.spot() else {
            return;
        };
        let Some(lsp) = &mut self.lsp else {
            return;
        };
        let support = lsp.support.clone();
        let selected_before = lsp
            .signatures
            .as_ref()
            .map(|popup| popup.signature().label.clone());
        lsp.signature_asked += 1;
        let asked = lsp.signature_asked;
        if support.signature_info().is_none() {
            self.parameter_info_from_text(spot, caret, cx);
            return;
        }
        cx.spawn(async move |this, cx| {
            let Some(provider) = support.signature_info() else {
                return;
            };
            let replies = ask(provider, SignatureQuery(spot));
            let answer = first(replies).await;
            this.update(cx, |this, cx| {
                let caret = this.spot().map(|(_, caret)| caret);
                let (Some(lsp), Some(caret)) = (&mut this.lsp, caret) else {
                    return;
                };
                if lsp.signature_asked != asked {
                    return;
                }
                let Some((help, _)) = answer else {
                    return;
                };
                let keep = selected_before.or_else(|| {
                    lsp.signatures
                        .as_ref()
                        .map(|popup| popup.signature().label.clone())
                });
                let mut popup = SignaturePopup::new(caret, help);
                if let Some(popup) = &mut popup
                    && let Some(keep) = keep
                    && let Some(found) = popup
                        .help
                        .signatures
                        .iter()
                        .position(|signature| signature.label == keep)
                {
                    popup.selected = found;
                    maybe_clamp_parameter(popup);
                }
                lsp.signatures = popup;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The older capability: signatures as text, one per line. Used when a language has
    /// no structured answer.
    fn parameter_info_from_text(
        &mut self,
        spot: DocumentSpot,
        caret: usize,
        cx: &mut Context<Self>,
    ) {
        let Some(lsp) = &mut self.lsp else {
            return;
        };
        let support = lsp.support.clone();
        if support.signatures().is_none() {
            return;
        }
        lsp.signature_asked += 1;
        let asked = lsp.signature_asked;
        cx.spawn(async move |this, cx| {
            let Some(provider) = support.signatures() else {
                return;
            };
            let replies = ask(provider, SignatureQuery(spot));
            let answer = first(replies).await;
            this.update(cx, |this, cx| match answer {
                Some((text, _)) if !text.0.is_empty() => {
                    let Some(lsp) = &mut this.lsp else {
                        return;
                    };
                    if lsp.signature_asked != asked {
                        return;
                    }
                    let signatures = text
                        .0
                        .lines()
                        .filter(|line| !line.trim().is_empty())
                        .map(|line| CallSignature {
                            label: line.trim().to_owned(),
                            parameters: parameters_in_label(line),
                        })
                        .collect();
                    lsp.signatures = SignaturePopup::new(
                        caret,
                        SignatureHelp {
                            signatures,
                            active_signature: 0,
                            active_parameter: 0,
                        },
                    );
                    cx.notify();
                }
                Some((_, Confidence::Unknown)) => this.show_note(caret, NO_ANSWER, cx),
                Some(_) => this.show_note(caret, "No parameters", cx),
                None => {}
            })
            .ok();
        })
        .detach();
    }

    /// Asks for the active parameter again after the caret moved or a character was
    /// typed, so the bold parameter follows the caret.
    pub(crate) fn refresh_parameters(&mut self, cx: &mut Context<Self>) {
        if self
            .lsp
            .as_ref()
            .and_then(|lsp| lsp.signatures.as_ref())
            .is_none()
        {
            return;
        }
        self.parameter_info(cx);
    }

    /// Tab and Shift+Tab between the arguments of a call just inserted. False when no
    /// walk is open, so the key keeps its usual meaning.
    pub(crate) fn parameter_field(&mut self, forward: bool, cx: &mut Context<Self>) -> bool {
        let Some(state) = &self.state else {
            return false;
        };
        let head = state.head();
        let Some(lsp) = &mut self.lsp else {
            return false;
        };
        if lsp.completion.is_some() {
            lsp.parameter_walk = None;
            return false;
        }
        let found = {
            let Some(walk) = lsp.parameter_walk.as_ref() else {
                return false;
            };
            if !walk.contains(head) {
                None
            } else {
                let count = walk.stops.len();
                let current = walk
                    .stops
                    .iter()
                    .position(|stop| stop.start <= head && head <= stop.end);
                let next = match current {
                    Some(index) if forward && index + 1 < count => Some(index + 1),
                    Some(_) if forward => None,
                    Some(index) if index > 0 => Some(index - 1),
                    Some(_) => None,
                    None if forward => Some(0),
                    None => count.checked_sub(1),
                };
                next.map(|next| walk.stops[next].clone())
            }
        };
        let Some(range) = found else {
            lsp.parameter_walk = None;
            return false;
        };
        let Some(state) = &mut self.state else {
            return false;
        };
        state.select_range(range);
        self.changed(None, cx);
        true
    }

    /// Shift+F1: the documentation page of the symbol under the caret.
    pub(crate) fn external_doc(&mut self, cx: &mut Context<Self>) {
        let Some((spot, caret)) = self.spot() else {
            return;
        };
        let Some(provider) = self.lsp.as_ref().and_then(|lsp| lsp.support.external_doc()) else {
            self.show_note(caret, "No documentation found", cx);
            return;
        };
        let replies = ask(provider, spot);
        cx.spawn(async move |this, cx| {
            let answer = first(replies).await;
            this.update(cx, |this, cx| match answer {
                Some((doc, _)) if doc.url.is_some() => {
                    let url = doc.url.unwrap_or_default();
                    let custom = cx
                        .try_global::<ExternalLinks>()
                        .and_then(|links| links.0.clone());
                    match custom {
                        Some(open) => open(&url),
                        None => {
                            platform::open_url(&url);
                        }
                    }
                }
                Some((_, Confidence::Unknown)) => this.show_note(caret, NO_ANSWER, cx),
                _ => this.show_note(caret, "No documentation found", cx),
            })
            .ok();
        })
        .detach();
    }

    /// The overload and the parameter the Parameter Info popup shows, when it is open.
    pub fn signature_position(&self) -> Option<(usize, usize)> {
        let popup = self.lsp.as_ref()?.signatures.as_ref()?;
        Some((popup.selected, popup.parameter))
    }

    /// How many overloads the Parameter Info popup has, when it is open.
    pub fn signature_count(&self) -> Option<usize> {
        let popup = self.lsp.as_ref()?.signatures.as_ref()?;
        Some(popup.help.signatures.len())
    }

    /// F2: the caret moves to the next error after it, around to the first.
    pub(crate) fn next_error(&mut self, cx: &mut Context<Self>) {
        let (Some(lsp), Some(state)) = (&self.lsp, &mut self.state) else {
            return;
        };
        let caret = state.selections().primary().head;
        let mut errors: Vec<&Mark> = lsp
            .marks
            .iter()
            .filter(|mark| mark.severity == Severity::Error)
            .collect();
        if errors.is_empty() {
            errors = lsp.marks.iter().collect();
        }
        errors.sort_by_key(|mark| mark.range.start);
        let next = errors
            .iter()
            .find(|mark| mark.range.start > caret)
            .or(errors.first());
        if let Some(mark) = next {
            let at = mark.range.start;
            state.select_range(at..at);
            self.changed(None, cx);
        }
    }

    /// Ctrl+F1: the message of the mark under the caret.
    pub(crate) fn error_description(&mut self, cx: &mut Context<Self>) {
        let (Some(lsp), Some(state)) = (&self.lsp, &self.state) else {
            return;
        };
        let caret = state.selections().primary().head;
        let found = lsp
            .marks
            .iter()
            .find(|mark| mark.range.start <= caret && caret <= mark.range.end)
            .map(|mark| mark.message.clone());
        if let Some(message) = found {
            self.show_note(caret, message, cx);
        }
    }

    /// Moves the caret where a navigation asked, once the text is here.
    pub(crate) fn reveal(&mut self, cx: &mut Context<Self>) {
        let Some(state) = &mut self.state else {
            return;
        };
        let Some(point) = cx
            .try_global::<Reveals>()
            .and_then(|reveals| reveals.0.get(&self.path).copied())
        else {
            return;
        };
        cx.default_global::<Reveals>().0.remove(&self.path);
        let at = offset_at(&state.store(), point);
        state.select_range(at..at);
        self.reveal_center = true;
        self.changed(None, cx);
    }
}

#[cfg(test)]
mod tests {
    use lang_api::{CompletionEntry, CompletionLabelDetails, TextPoint};
    use text_core::{RopeStore, TextStore};

    use super::{
        Completion, CompletionCase, completion_doc_rows, completion_kind_icon, completion_order,
        completion_parts, completion_parts_with_label_details, merge_completions, offset_at,
        point_at, source_path, word_start,
    };

    #[test]
    fn every_kind_of_entry_has_its_own_picture() {
        use lang_api::CompletionKind;

        let icons = [
            (CompletionKind::Class, "nodes/class"),
            (CompletionKind::Interface, "nodes/interface"),
            (CompletionKind::Enum, "nodes/enum"),
            (CompletionKind::Method, "nodes/method"),
            (CompletionKind::Field, "nodes/field"),
            (CompletionKind::Variable, "nodes/variable"),
            (CompletionKind::Keyword, "nodes/keyword"),
            (CompletionKind::Snippet, "nodes/template"),
            (CompletionKind::Constant, "nodes/constant"),
            (CompletionKind::Module, "nodes/package"),
        ];
        for (kind, icon) in icons {
            assert_eq!(completion_kind_icon(kind), icon, "{kind:?}");
        }
        // Every kind names an icon of the set, never the empty name.
        for kind in [
            CompletionKind::Unknown,
            CompletionKind::Text,
            CompletionKind::Function,
            CompletionKind::Constructor,
            CompletionKind::Property,
            CompletionKind::Unit,
            CompletionKind::Value,
            CompletionKind::Color,
            CompletionKind::File,
            CompletionKind::Reference,
            CompletionKind::Folder,
            CompletionKind::Struct,
            CompletionKind::Event,
            CompletionKind::Operator,
            CompletionKind::TypeParameter,
        ] {
            assert!(!completion_kind_icon(kind).is_empty(), "{kind:?}");
        }
    }

    #[test]
    fn the_documentation_of_an_entry_loses_its_markup() {
        use crate::semantic::CompletionDoc;

        let doc = CompletionDoc::new(
            0,
            "`ArrayList` is a **list**.\n\n> a quoted line\n\n* first\n* second\n\n```\nnew ArrayList<>();\n```",
        );
        let rows = completion_doc_rows(&doc);
        let text: Vec<String> = rows
            .iter()
            .map(|row| row.runs.iter().map(|run| run.text.as_str()).collect())
            .collect();
        assert_eq!(text[0], "ArrayList is a list.");
        assert!(!text[0].contains('`') && !text[0].contains('*'), "{text:?}");
        // A quote reads as code, and the quote marks are gone.
        let quoted = rows
            .iter()
            .find(|row| row.code && row.runs.iter().any(|run| run.text.contains("quoted")))
            .expect("the quote line is code");
        assert!(quoted.runs.iter().all(|run| !run.text.starts_with('>')));
        // The bullets of a list are bullets, not asterisks.
        let bullets: Vec<&str> = rows
            .iter()
            .filter(|row| row.indent > 0 && !row.code)
            .flat_map(|row| row.runs.first().map(|run| run.text.as_str()))
            .collect();
        assert_eq!(bullets, ["• ", "• "]);
        // The fenced block keeps its lines as code.
        assert!(
            rows.iter().any(|row| row.code
                && row
                    .runs
                    .iter()
                    .any(|run| run.text.contains("new ArrayList"))),
            "{text:?}"
        );
        assert!(rows.len() <= super::COMPLETION_DOC_LINES);
    }

    #[test]
    fn the_labels_of_the_documentation_get_their_own_lines() {
        use crate::semantic::CompletionDoc;

        // The comment comes as the plain text of the Javadoc, one line after another.
        let doc = CompletionDoc::new(
            0,
            "Returns a string representation of this object.\nSpecified by:\ntoString in class Object\n\nOverrides:\ntoString in class Object",
        );
        let rows = completion_doc_rows(&doc);
        let lines: Vec<String> = rows
            .iter()
            .map(|row| row.runs.iter().map(|run| run.text.as_str()).collect())
            .collect();
        assert_eq!(
            lines,
            [
                "Returns a string representation of this object.",
                "Specified by:",
                "toString in class Object",
                "Overrides:",
                "toString in class Object",
            ],
            "every label stands under the text rather than beside it"
        );
    }

    #[test]
    fn a_long_line_of_the_documentation_stays_whole_for_the_popup_to_wrap() {
        use crate::semantic::CompletionDoc;

        let long = "The text of the comment, which the popup wraps by its own width, "
            .repeat(8)
            .trim_end()
            .to_owned();
        let doc = CompletionDoc::new(0, format!("Returns:\n{long}"));
        let rows = completion_doc_rows(&doc);
        let lines: Vec<String> = rows
            .iter()
            .map(|row| row.runs.iter().map(|run| run.text.as_str()).collect())
            .collect();
        assert_eq!(lines[0], "Returns:");
        assert_eq!(
            lines[1], long,
            "the row keeps the whole line, so that the popup wraps it rather than cutting it"
        );
    }

    #[test]
    fn points_count_utf16_from_the_line_start() {
        let store = RopeStore::from_text("ab\n\u{1F600}x\n");
        let offset = "ab\n\u{1F600}".len();
        let point = point_at(&store, offset);
        assert_eq!((point.line, point.character), (1, 2));
        assert_eq!(offset_at(&store, point), offset);
        let past = TextPoint {
            line: 0,
            character: 40,
        };
        assert_eq!(offset_at(&store, past), 2);
    }

    #[test]
    fn a_word_starts_after_a_dot() {
        let store = RopeStore::from_text("list.siz");
        assert_eq!(word_start(&store, 8), 5);
        assert_eq!(word_start(&store, 5), 5);
    }

    #[test]
    fn entries_that_start_with_the_prefix_come_first() {
        let entry = |label: &str| CompletionEntry {
            label: label.to_owned(),
            ..CompletionEntry::default()
        };
        let mut completion = Completion::new(
            0,
            vec![entry("isEmpty"), entry("size"), entry("resize")],
            true,
            "",
            CompletionCase::None,
            &[],
        );
        completion.filter("SI");
        assert_eq!(completion.shown, [1, 2]);
        assert_eq!(
            completion.chosen().map(|(text, _)| text).as_deref(),
            Some("size")
        );
    }

    #[test]
    fn a_type_row_shows_the_simple_name_and_the_package_once() {
        let (left, right, owner) = completion_parts_with_label_details(
            "StrictMath - com.example",
            Some("com.example.StrictMath"),
            None,
        );
        assert_eq!(left, "StrictMath");
        assert_eq!(right.as_deref(), Some("com.example"));
        assert_eq!(owner, None, "the package does not repeat as the owner");

        let details = CompletionLabelDetails {
            detail: None,
            description: Some("com.example".into()),
        };
        let (left, right, owner) = completion_parts_with_label_details(
            "StrictMath",
            Some("com.example.StrictMath"),
            Some(&details),
        );
        assert_eq!(left, "StrictMath");
        assert_eq!(right.as_deref(), Some("com.example"));
        assert_eq!(owner, None);
    }

    #[test]
    fn a_method_row_keeps_the_parameters_the_result_and_the_owner() {
        let (left, right, owner) = completion_parts(
            "getLastName(String) : String",
            Some("Person.getLastName(String) : String"),
        );
        assert_eq!(left, "getLastName(String)");
        assert_eq!(right.as_deref(), Some("String"));
        assert_eq!(owner.as_deref(), Some("Person"));
    }

    #[test]
    fn a_field_row_shows_the_field_its_type_and_the_owner() {
        let (left, right, owner) = completion_parts("size : int", Some("Person.size : int"));
        assert_eq!(left, "size");
        assert_eq!(right.as_deref(), Some("int"));
        assert_eq!(owner.as_deref(), Some("Person"));
    }

    fn entry(label: &str, sort: Option<&str>) -> CompletionEntry {
        CompletionEntry {
            label: label.to_owned(),
            sort_text: sort.map(str::to_owned),
            ..CompletionEntry::default()
        }
    }

    #[test]
    fn at_an_equal_sort_text_the_shorter_name_comes_first() {
        let entries = [
            entry("StringBuffer", Some("0000")),
            entry("String", Some("0000")),
            entry("StrictMath", Some("0000")),
            entry("StringIndexOutOfBoundsException", Some("0000")),
            entry("StringBuilder", Some("0000")),
        ];
        let labels: Vec<&str> = completion_order(&entries, "Str", &[])
            .into_iter()
            .map(|index| entries[index].label.as_str())
            .collect();
        assert_eq!(
            labels,
            [
                "String",
                "StrictMath",
                "StringBuffer",
                "StringBuilder",
                "StringIndexOutOfBoundsException",
            ]
        );
    }

    #[test]
    fn a_different_sort_text_keeps_the_order_of_the_server() {
        let entries = [
            entry("StringBuffer", Some("0002")),
            entry("StrictMath", Some("0001")),
        ];
        let order = completion_order(&entries, "Str", &[]);
        assert_eq!(order, [1, 0]);
    }

    #[test]
    fn a_preferred_type_comes_before_a_rare_one_of_the_server_order() {
        let entries = [
            entry("StrictMath", Some("0000")),
            entry("String", Some("0005")),
        ];
        let order = completion_order(&entries, "Str", &["String"]);
        assert_eq!(order, [1, 0], "the type the language prefers comes first");
    }

    #[test]
    fn a_local_postfix_key_replaces_the_same_key_of_the_server() {
        let local = CompletionEntry {
            label: "for".to_owned(),
            ..CompletionEntry::default()
        };
        let server = CompletionEntry {
            label: "for".to_owned(),
            filter_text: Some("args.for".to_owned()),
            sort_text: Some("15".to_owned()),
            snippet: true,
            ..CompletionEntry::default()
        };
        let other = CompletionEntry {
            label: "var".to_owned(),
            filter_text: Some("args.var".to_owned()),
            sort_text: Some("16".to_owned()),
            ..CompletionEntry::default()
        };
        let merged = merge_completions(vec![local], vec![server, other]);
        let labels: Vec<&str> = merged.iter().map(|entry| entry.label.as_str()).collect();
        assert_eq!(labels, ["for", "var"]);
    }

    #[test]
    fn a_field_named_like_a_template_stays() {
        let local = CompletionEntry {
            label: "log".to_owned(),
            ..CompletionEntry::default()
        };
        let field = CompletionEntry {
            label: "log : Logger".to_owned(),
            sort_text: Some("10".to_owned()),
            ..CompletionEntry::default()
        };
        let method = CompletionEntry {
            label: "log() : void".to_owned(),
            sort_text: Some("11".to_owned()),
            snippet: true,
            ..CompletionEntry::default()
        };
        let merged = merge_completions(vec![local], vec![field, method]);
        assert_eq!(merged.len(), 3, "the field and the method stay");
    }

    #[test]
    fn sources_get_stable_paths_named_after_the_class() {
        let uri = "lib://contents/core/text/String.class?=x";
        let path = source_path(uri, "txt");
        assert_eq!(path, source_path(uri, "txt"));
        assert_eq!(path.file_name().unwrap(), "String.txt");
    }
}

#[cfg(test)]
mod case_tests {
    use lang_api::CompletionEntry;

    use super::{CompletionCase, completion_order_with};

    fn labels(prefix: &str, case: CompletionCase) -> Vec<String> {
        let entries = [
            CompletionEntry {
                label: "String".to_owned(),
                ..CompletionEntry::default()
            },
            CompletionEntry {
                label: "stringBuilder".to_owned(),
                ..CompletionEntry::default()
            },
        ];
        completion_order_with(&entries, prefix, case, &[])
            .into_iter()
            .map(|index| entries[index].label.clone())
            .collect()
    }

    #[test]
    fn the_case_rule_decides_which_entries_match() {
        assert_eq!(labels("str", CompletionCase::None).len(), 2);
        assert_eq!(
            labels("str", CompletionCase::FirstLetter),
            ["stringBuilder"],
            "a lowercase first letter finds only the lowercase entry"
        );
        assert_eq!(
            labels("Str", CompletionCase::FirstLetter),
            ["String"],
            "an uppercase first letter finds only the uppercase entry"
        );
        assert_eq!(labels("str", CompletionCase::All), ["stringBuilder"]);
        assert_eq!(labels("String", CompletionCase::All), ["String"]);
        assert!(labels("STRING", CompletionCase::All).is_empty());
    }
}

#[cfg(test)]
mod token_checks {
    use lang_api::{SemanticTokenKind, SemanticTokenModifiers};
    use text_core::Edit;

    use super::{TokenSpan, normalize_tokens, shift_tokens};

    fn token(start: u32, end: u32) -> TokenSpan {
        TokenSpan {
            start,
            end,
            kind: SemanticTokenKind::Property,
            modifiers: SemanticTokenModifiers::default(),
        }
    }

    #[test]
    fn tokens_follow_an_edit_and_leave_the_inserted_text_plain() {
        let mut tokens = vec![token(0, 5), token(10, 15)];
        shift_tokens(&mut tokens, &[Edit::insert(7, "xx")]);
        assert_eq!((tokens[0].start, tokens[0].end), (0, 5));
        assert_eq!((tokens[1].start, tokens[1].end), (12, 17));

        // A replacement across the second token keeps its free end and drops the part
        // that the new text took.
        shift_tokens(&mut tokens, &[Edit::new(10..14, "a\nb")]);
        assert_eq!((tokens[1].start, tokens[1].end), (10, 16));
        normalize_tokens(&mut tokens, 40);
        assert_eq!(tokens.len(), 2);
    }

    #[test]
    fn a_stale_answer_is_cut_to_the_text_and_tokens_never_overlap() {
        let mut tokens = vec![token(0, 30), token(10, 20), token(28, 60)];
        normalize_tokens(&mut tokens, 40);
        assert_eq!(tokens.len(), 1, "the parts that overlap or pass the end go");
        assert!(tokens.iter().all(|token| token.end <= 40));
        for pair in tokens.windows(2) {
            assert!(pair[0].end <= pair[1].start, "{tokens:?}");
        }
    }
}
