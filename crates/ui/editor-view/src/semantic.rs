//! The buffer's side of a language server, through `lang-api`. The editor knows no
//! language: the application sets [`Semantic`], and files it does not match get none.
//! Every question runs off the UI thread; an answer for an older text is dropped.

use std::collections::HashMap;
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

pub(crate) const NO_ANSWER: &str = "The language server did not answer, try again";
pub(crate) const MAX_SHOWN: usize = 12;

/// The wait before the semantic tokens are asked for after an edit, in milliseconds.
const TOKENS_DELAY_MS: u64 = 250;
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

/// Texts that are not files, opened read only under a made-up path.
#[derive(Default)]
pub(crate) struct Sources(pub HashMap<PathBuf, Arc<str>>);

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
    let Some(provider) = support.as_ref().and_then(|support| support.source_text()) else {
        cx.defer(missing);
        return;
    };
    let uri = target.uri.clone();
    let path = source_path(&uri, extension);
    let replies = ask(provider, SourceTextQuery(uri));
    cx.spawn(async move |cx| {
        let answer = first(replies).await;
        cx.update(|cx| match answer {
            Some((text, _)) if !text.is_empty() => {
                cx.default_global::<Sources>()
                    .0
                    .insert(path.clone(), text.into());
                cx.default_global::<Reveals>().0.insert(path.clone(), point);
                open(path, cx);
            }
            _ => missing(cx),
        });
    })
    .detach();
}

pub(crate) struct Mark {
    pub range: Range<usize>,
    pub severity: Severity,
    pub message: String,
    /// The server marked the range as something that can go. The editor greys it out.
    pub unnecessary: bool,
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
    pub shown: Vec<usize>,
    pub selected: usize,
}

/// One completion row: the label split into the text on the left and the type on the
/// right, with the class that declares it.
pub(crate) struct CompletionRow {
    pub left: String,
    pub right: Option<String>,
    pub owner: Option<String>,
}

impl Completion {
    #[cfg(test)]
    fn filter(&mut self, prefix: &str) {
        self.filter_with(prefix, CompletionCase::None);
    }

    fn filter_with(&mut self, prefix: &str, case: CompletionCase) {
        self.shown = completion_order_with(self.entries.as_slice(), prefix, case);
        self.selected = 0;
    }

    pub fn label(&self, row: usize) -> CompletionRow {
        let entry = &self.entries[self.shown[row]];
        let (left, right, owner) = completion_parts_with_label_details(
            &entry.label,
            entry.detail.as_deref(),
            entry.label_details.as_ref(),
        );
        CompletionRow { left, right, owner }
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

    /// Rows around the selection, at most [`MAX_SHOWN`].
    pub fn window(&self) -> Range<usize> {
        let start = self.selected.saturating_sub(MAX_SHOWN - 1);
        start..(start + MAX_SHOWN).min(self.shown.len())
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
/// `String` comes before `StringBuilder` and `StringIndexOutOfBoundsException`.
pub fn completion_order(entries: &[CompletionEntry], prefix: &str) -> Vec<usize> {
    completion_order_with(entries, prefix, CompletionCase::None)
}

/// [`completion_order`] with the case rule of Settings: `None` ignores case,
/// `FirstLetter` compares the case of the first letter and `All` compares every letter.
pub fn completion_order_with(
    entries: &[CompletionEntry],
    prefix: &str,
    case: CompletionCase,
) -> Vec<usize> {
    let folded = prefix.to_lowercase();
    let keys: Vec<String> = entries
        .iter()
        .map(|entry| {
            entry
                .filter_text
                .as_deref()
                .unwrap_or(&entry.label)
                .to_owned()
        })
        .collect();
    let folded_keys: Vec<String> = keys.iter().map(|key| key.to_lowercase()).collect();
    let matches_case = |index: usize, at_start: bool| match case {
        CompletionCase::None => true,
        CompletionCase::FirstLetter => {
            let key = &keys[index];
            match (prefix.chars().next(), key.chars().next()) {
                (Some(typed), Some(key)) => {
                    typed.is_uppercase() == key.is_uppercase() || !typed.is_alphabetic()
                }
                _ => true,
            }
        }
        CompletionCase::All => {
            if at_start {
                keys[index].starts_with(prefix)
            } else {
                keys[index].contains(prefix)
            }
        }
    };
    let mut starts: Vec<usize> = Vec::new();
    let mut contains: Vec<usize> = Vec::new();
    for (index, key) in folded_keys.iter().enumerate() {
        if key.starts_with(&folded) && matches_case(index, true) {
            starts.push(index);
        } else if key.contains(&folded) && matches_case(index, false) {
            contains.push(index);
        }
    }
    // Entries without a `sortText` are the local templates, which stay in front and in
    // their own order.
    let order = |a: &usize, b: &usize| match (&entries[*a].sort_text, &entries[*b].sort_text) {
        (Some(a), Some(b)) if a != b => a.cmp(b),
        (Some(_), Some(_)) => folded_keys[*a]
            .chars()
            .count()
            .cmp(&folded_keys[*b].chars().count()),
        (None, Some(_)) => std::cmp::Ordering::Less,
        (Some(_), None) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    };
    starts.sort_by(order);
    contains.sort_by(order);
    starts.extend(contains);
    starts
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
/// label may carry its package after ` - ` (`StrictMath - com.example`); the package goes to
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

pub(crate) struct Lsp {
    pub support: Arc<dyn LanguageSupport>,
    pub uri: String,
    pub marks: Vec<Mark>,
    /// The server answered about the diagnostics of this text at least once.
    pub received: bool,
    pub completion: Option<Completion>,
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
    /// How the list compares the typed prefix with its entries.
    pub case: CompletionCase,
}

impl Lsp {
    pub fn close(&self) {
        if let Some(server) = self.support.server() {
            server.close(&self.uri);
        }
    }
}

/// How long the typing settles before the server is asked for hints again.
pub(crate) const INLAY_TYPING_DELAY: Duration = Duration::from_millis(150);

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
                    start: point_at(state.store(), start),
                    end: point_at(state.store(), end),
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
            let offset = offset_at(state.store(), entry.position);
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
        cx.try_global::<Sources>()?.0.get(path).cloned()
    }

    /// Opens the document at the server once it is loaded, and follows its diagnostics.
    pub(crate) fn attach(&mut self, cx: &mut Context<Self>) {
        let Some(semantic) = cx.try_global::<Semantic>() else {
            return;
        };
        let support = semantic.0.clone();
        let Some(state) = &self.state else {
            return;
        };
        if state.is_read_only() || !support.matches(&self.path) {
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
            received: !follows_diagnostics,
            completion: None,
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
            start: point_at(state.store(), start),
            end: point_at(state.store(), end),
        })
    }

    fn apply_semantic_tokens(&mut self, tokens: Vec<SemanticToken>, cx: &mut Context<Self>) {
        if !self.options.semantic_highlighting {
            return;
        }
        let Some(state) = &mut self.state else {
            return;
        };
        let store = state.store();
        let mut spans: Vec<TokenSpan> = tokens
            .into_iter()
            .filter_map(|token| {
                let start = offset_at(store, token.span.start);
                let end = offset_at(store, token.span.end).max(start);
                (start < end).then_some(TokenSpan {
                    start: start as u32,
                    end: end as u32,
                    kind: token.kind,
                    modifiers: token.modifiers,
                })
            })
            .collect();
        normalize_tokens(&mut spans, store.len());
        state.set_semantic_tokens(spans);
        cx.notify();
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
        lsp.marks = entries
            .iter()
            .map(|entry| {
                let start = offset_at(store, entry.span.start);
                let end = offset_at(store, entry.span.end).max(start);
                Mark {
                    range: start..end,
                    severity: entry.severity,
                    message: entry.message.clone(),
                    unnecessary: entry.unnecessary,
                }
            })
            .collect();
        lsp.received = true;
        cx.emit(EditorEvent::Diagnostics(entries));
        cx.notify();
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
        if lsp.completion.is_some() {
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
            point: point_at(state.store(), caret),
        };
        Some((spot, caret))
    }

    /// The place at `offset`, for a question that does not move the caret.
    pub(crate) fn spot_at(&self, offset: usize) -> Option<DocumentSpot> {
        let (lsp, state) = (self.lsp.as_ref()?, self.state.as_ref()?);
        Some(DocumentSpot {
            uri: lsp.uri.clone(),
            version: state.version() as i32,
            point: point_at(state.store(), offset),
        })
    }

    pub(crate) fn show_note(
        &mut self,
        at: usize,
        text: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) {
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
        let Some((spot, caret)) = self.spot() else {
            return;
        };
        let case = self.options.completion_case;
        let (Some(lsp), Some(state)) = (&mut self.lsp, &self.state) else {
            return;
        };
        let start = word_start(state.store(), caret);
        let line_start = state
            .store()
            .line_to_offset(state.store().offset_to_line(caret));
        let before = state.store().slice(line_start..caret);
        let local: Vec<CompletionEntry> = if mode == CompletionMode::All {
            cx.try_global::<CompletionSources>()
                .map(|sources| sources.0.clone())
                .unwrap_or_default()
                .iter()
                .flat_map(|source| source.entries(&self.path, &before, spot.point))
                .collect()
        } else {
            Vec::new()
        };
        lsp.asked += 1;
        let asked = lsp.asked;
        lsp.parameter_walk = None;
        if !local.is_empty() {
            let mut completion = Completion {
                start,
                entries: local.clone(),
                shown: Vec::new(),
                selected: 0,
            };
            completion.filter_with(&state.store().slice(start..caret), case);
            lsp.completion = (!completion.shown.is_empty()).then_some(completion);
            cx.notify();
        }
        let support = lsp.support.clone();
        if support.completion().is_none() {
            return;
        }
        let expected_spot = spot.clone();
        cx.spawn(async move |this, cx| {
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
                let Some((list, confidence)) = answer else {
                    return;
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
                let entries = visible_entries(list.entries, mode, expected.as_deref());
                let entries = merge_completions(local, entries);
                let mut completion = Completion {
                    start,
                    entries,
                    shown: Vec::new(),
                    selected: 0,
                };
                completion.filter_with(&state.store().slice(start..caret), case);
                lsp.completion = (!completion.shown.is_empty()).then_some(completion);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Keys of an open completion: arrows move, Enter inserts before the rest of the
    /// word, Tab replaces that rest, Ctrl+. inserts the entry with a dot and opens the
    /// list again, Ctrl+Shift+Enter finishes the statement, Escape closes. Returns
    /// whether the key was taken.
    pub(crate) fn completion_key(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
        let Some(lsp) = &mut self.lsp else {
            return false;
        };
        if key == "escape" && lsp.signatures.take().is_some() {
            cx.notify();
            if lsp.completion.is_none() {
                return true;
            }
        }
        if key == "escape" && lsp.note.take().is_some() {
            cx.notify();
            if lsp.completion.is_none() {
                return true;
            }
        }
        let Some(completion) = &mut lsp.completion else {
            return false;
        };
        let rows = completion.shown.len();
        match key {
            "up" => {
                completion.selected = (completion.selected + rows - 1) % rows;
                cx.notify();
                return true;
            }
            "down" => {
                completion.selected = (completion.selected + 1) % rows;
                cx.notify();
                return true;
            }
            "escape" => {
                lsp.completion = None;
                cx.notify();
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
        let support = lsp.support.clone();
        let Some(entry) = entry else {
            return true;
        };
        let Some(state) = &self.state else {
            return true;
        };
        let caret = state.head();
        let start = entry
            .span
            .map_or(start, |span| offset_at(state.store(), span.start))
            .min(caret);
        let end = if replace {
            word_end(state.store(), caret).max(caret)
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
        let mut shift = 0isize;
        let mut edits: Vec<Edit> = Vec::new();
        for edit in additional {
            let from = offset_at(state.store(), edit.span.start);
            let to = offset_at(state.store(), edit.span.end).max(from);
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

    /// Ctrl+B and Ctrl+Alt+B.
    pub(crate) fn navigate(&mut self, kind: NavigationKind, cx: &mut Context<Self>) {
        let Some((spot, caret)) = self.spot() else {
            return;
        };
        let Some(provider) = self.lsp.as_ref().and_then(|lsp| lsp.support.navigation()) else {
            return;
        };
        let replies = ask(provider, NavigationQuery { kind, spot });
        cx.spawn(async move |this, cx| {
            let answer = first(replies).await;
            this.update(cx, |this, cx| match answer {
                Some((spans, _)) if !spans.is_empty() => this.go_to_location(&spans[0], cx),
                Some((_, Confidence::Unknown)) => this.show_note(caret, NO_ANSWER, cx),
                Some(_) => this.show_note(caret, "Cannot find declaration to go to", cx),
                None => {}
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn go_to_location(&mut self, target: &SourceSpan, cx: &mut Context<Self>) {
        let caret = self.spot().map_or(0, |(_, caret)| caret);
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
                        this.show_note(caret, "No source for this place", cx)
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
        let at = offset_at(state.store(), point);
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
        Completion, completion_order, completion_parts, completion_parts_with_label_details,
        merge_completions, offset_at, point_at, source_path, word_start,
    };

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
        let mut completion = Completion {
            start: 0,
            entries: vec![entry("isEmpty"), entry("size"), entry("resize")],
            shown: Vec::new(),
            selected: 0,
        };
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
        let labels: Vec<&str> = completion_order(&entries, "Str")
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
            entry("String", Some("0002")),
            entry("StringBuilder", Some("0001")),
        ];
        let order = completion_order(&entries, "Str");
        assert_eq!(order, [1, 0]);
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
        completion_order_with(&entries, prefix, case)
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
