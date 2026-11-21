//! M5b on top of the server: usages, rename, extract and inline, Generate, the type
//! under the caret, the super method and the type hierarchy.

use std::ops::Range;
use std::path::{Component, Path};

use ::platform;
use futures_channel::mpsc::UnboundedReceiver;
use futures_util::StreamExt;
use lang_api::{
    ActionEntry, ActionKind, Confidence, DocumentEdit, DocumentSpot, ExtractKind, MoveDestination,
    MoveKind, MovePlace, MoveQuery, MoveRequest, PrepareRename, RenamePosition, RenameQuery,
    SelectionQuery, SourceSpan, TextSpan, WorkspaceChange, apply_to_text, ask,
};
use text_core::{EditKind, TextStore};
use ui_kit::*;

use crate::actions::{Own, Pick};
use crate::rename::{
    NOT_READY, PrepareOutcome, REFUSED, RenameInPlace, WAITING_FOR_LANGUAGE, can_rename_in_place,
    prepare_outcome,
};
use crate::semantic::{MAX_SHOWN, NO_ANSWER, first, is_word, offset_at, point_at, uri_path};
use crate::view::{DetailedUsage, EditorEvent, EditorView, Usage, UsagesUpdate};
use keymap::editor as editor_actions;

/// The places of the answer, each carrying the name the popup shows.
fn usages_of(name: &str, spans: Vec<SourceSpan>) -> Vec<Usage> {
    spans
        .into_iter()
        .map(|target| Usage {
            name: name.to_owned(),
            target,
        })
        .collect()
}

/// Whether the place names a file, compared the way the system compares paths.
fn same_file(uri: &str, path: &Path) -> bool {
    uri_path(uri).is_some_and(|found| platform::path_key(&found) == platform::path_key(path))
}

/// The places of the answer with the kind and the container of each, and the module of
/// the file it is in.
fn detailed_usages_of(
    name: &str,
    entries: Vec<lang_api::UsageEntry>,
    support: &dyn lang_api::LanguageSupport,
) -> Vec<DetailedUsage> {
    entries
        .into_iter()
        .map(|entry| {
            let module = uri_path(&entry.target.uri).and_then(|path| support.module_of(&path));
            DetailedUsage {
                name: name.to_owned(),
                target: entry.target,
                kind: entry.kind,
                container: entry.container,
                module,
            }
        })
        .collect()
}

/// A key that becomes text: one character, or a named one the keyboard sends as text.
fn is_typed_key(key: &str) -> bool {
    key.chars().count() == 1
        || matches!(
            key,
            "space"
                | "comma"
                | "period"
                | "minus"
                | "equal"
                | "slash"
                | "backslash"
                | "semicolon"
                | "quote"
                | "backquote"
                | "bracketleft"
                | "bracketright"
        )
}

/// The byte offset of a place the server names in UTF-16 units.
fn byte_at_utf16(text: &str, units: u32) -> Option<usize> {
    let mut seen = 0u32;
    for (at, character) in text.char_indices() {
        if seen == units {
            return Some(at);
        }
        seen += character.len_utf16() as u32;
        if seen > units {
            return None;
        }
    }
    (seen == units).then_some(text.len())
}

/// The text a place of the server covers in a text.
fn name_at(text: &str, position: &RenamePosition) -> Option<String> {
    let start = byte_at_utf16(text, position.offset)?;
    let end = byte_at_utf16(text, position.offset + position.length)?;
    text.get(start..end).map(str::to_owned)
}

/// The text of the file after the edits of the entry, or `None` when the entry has no
/// edits for it.
fn text_after(entry: &ActionEntry, uri: &str, text: &str) -> Option<String> {
    let source = entry.edits.iter().find(|source| source.uri == uri)?;
    Some(apply_to_text(text, &source.edits))
}

/// Shift+F6: the new name is typed here, not into the text. The first Enter asks
/// the server and shows what changes, the second applies it.
pub(crate) struct RenameField {
    pub at: usize,
    spot: DocumentSpot,
    pub text: String,
    /// The answer of the server for `text`, with the document version it is for.
    preview: Option<(i32, WorkspaceChange)>,
    pub summary: Option<String>,
}

/// F6: the destinations of the server, filtered by the text typed into the field.
/// Enter asks the server to move the file, Escape closes the field.
pub(crate) struct MoveField {
    pub at: usize,
    spot: DocumentSpot,
    kind: MoveKind,
    destinations: Vec<MoveDestination>,
    pub text: String,
    pub selected: usize,
    shown: Vec<usize>,
}

impl MoveField {
    fn new(
        at: usize,
        spot: DocumentSpot,
        kind: MoveKind,
        destinations: Vec<MoveDestination>,
    ) -> Self {
        let mut field = Self {
            at,
            spot,
            kind,
            destinations,
            text: String::new(),
            selected: 0,
            shown: Vec::new(),
        };
        field.filter();
        field
    }

    fn filter(&mut self) {
        let prefix = self.text.to_lowercase();
        self.shown = self
            .destinations
            .iter()
            .enumerate()
            .filter(|(_, destination)| {
                prefix.is_empty() || destination.name.to_lowercase().contains(&prefix)
            })
            .map(|(index, _)| index)
            .collect();
        self.selected = 0;
    }

    pub fn label(&self, row: usize) -> (&str, Option<&str>) {
        let destination = &self.destinations[self.shown[row]];
        (&destination.name, destination.detail.as_deref())
    }

    pub fn window(&self) -> Range<usize> {
        let start = self.selected.saturating_sub(MAX_SHOWN - 1);
        start..(start + MAX_SHOWN).min(self.shown.len())
    }

    pub fn chosen(&self) -> Option<&MoveDestination> {
        self.destinations.get(*self.shown.get(self.selected)?)
    }

    pub fn rows(&self) -> usize {
        self.shown.len()
    }
}

/// How visible the method an extract introduces is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Visibility {
    Private,
    Protected,
    Package,
    Public,
}

impl Visibility {
    fn label(self) -> &'static str {
        match self {
            Self::Private => "private",
            Self::Protected => "protected",
            Self::Package => "package-private",
            Self::Public => "public",
        }
    }

    /// The modifier the text of a method starts with, and the word a package-private
    /// method has none of.
    fn modifier(self) -> Option<&'static str> {
        match self {
            Self::Private => Some("private"),
            Self::Protected => Some("protected"),
            Self::Package => None,
            Self::Public => Some("public"),
        }
    }

    fn next(self, forward: bool) -> Self {
        let steps = [Self::Private, Self::Protected, Self::Package, Self::Public];
        let at = steps.iter().position(|step| *step == self).unwrap_or(0);
        let len = steps.len();
        steps[(at + if forward { 1 } else { len - 1 }) % len]
    }
}

/// The field over an extract: the name the server chose, ready to be typed over, and
/// the visibility of a new method. Enter applies the refactoring with that name.
pub(crate) struct ExtractNameField {
    pub at: usize,
    pub label: &'static str,
    pub text: String,
    /// The first typed character replaces the name of the server instead of following it.
    selected: bool,
    entry: ActionEntry,
    version: i32,
    rename: RenamePosition,
    default_name: String,
    /// Extract Method has a visibility of its own; the other extracts do not.
    visibility: Option<Visibility>,
}

impl ExtractNameField {
    /// The line under the name: what Enter writes into the code.
    pub fn preview(&self) -> String {
        match self.visibility {
            Some(visibility) => format!("{} {}(...)", visibility.label(), self.text),
            None => format!("Name: {}", self.text),
        }
    }

    pub fn visibility_line(&self) -> Option<&'static str> {
        self.visibility.map(Visibility::label)
    }
}

/// What a refactoring key asks the server for: a prefix of the kind of its action.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refactoring {
    ExtractMethod,
    ExtractVariable,
    ExtractField,
    ExtractConstant,
    IntroduceParameter,
    Inline,
}

impl Refactoring {
    fn code(self) -> &'static str {
        match self {
            Self::ExtractMethod => "refactor.extract.function",
            Self::ExtractVariable => "refactor.extract.variable",
            Self::ExtractField => "refactor.extract.field",
            Self::ExtractConstant => "refactor.extract.constant",
            Self::IntroduceParameter => "refactor.introduce.parameter",
            Self::Inline => "refactor.inline",
        }
    }

    fn missing(self) -> &'static str {
        match self {
            Self::Inline => "Nothing to inline here",
            Self::IntroduceParameter => "Select an expression to introduce as a parameter",
            _ => "Select an expression or statements to extract",
        }
    }

    /// The name the window gives the entry of Local History.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::ExtractMethod => "Extract Method",
            Self::ExtractVariable => "Extract Variable",
            Self::ExtractField => "Extract Field",
            Self::ExtractConstant => "Extract Constant",
            Self::IntroduceParameter => "Introduce Parameter",
            Self::Inline => "Inline",
        }
    }

    fn from_code(code: &str) -> &'static str {
        if code.starts_with("refactor.extract.function") {
            "Extract Method"
        } else if code.starts_with("refactor.extract.variable") {
            "Extract Variable"
        } else if code.starts_with("refactor.extract.constant") {
            "Extract Constant"
        } else if code.starts_with("refactor.extract.field") {
            "Extract Field"
        } else if code.starts_with("refactor.introduce.parameter") {
            "Introduce Parameter"
        } else if code.starts_with("refactor.inline") {
            "Inline"
        } else {
            "Refactoring"
        }
    }
}

/// The file name and the line of a place, for a row of a list.
pub(crate) fn place_label(target: &SourceSpan, detail: Option<&str>) -> String {
    let name = target.uri.rsplit('/').next().unwrap_or(&target.uri);
    let name = name.split(['?', '#']).next().unwrap_or(name);
    match detail {
        Some(detail) => format!("{detail}  {name}:{}", target.span.start.line + 1),
        None => format!("{name}:{}", target.span.start.line + 1),
    }
}

fn summary(change: &WorkspaceChange) -> String {
    let files = change
        .edits
        .iter()
        .filter(|source| !source.edits.is_empty())
        .count();
    let edits: usize = change.edits.iter().map(|source| source.edits.len()).sum();
    let mut text = format!("{edits} changes in {files} files");
    if !change.renames.is_empty() {
        text.push_str(&format!(", {} files renamed", change.renames.len()));
    }
    text + ". Enter applies, Escape cancels"
}

/// What a drop on a folder did in the editor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FolderMove {
    /// The move went to the server or the package picker opened.
    Handled,
    /// The file is not a class, so it moves as it is.
    Plain,
    /// The file is a class and the folder is not a package of any source root: moving
    /// it as it is would leave its references behind, so the window asks first.
    AskPlain,
}

/// The folder components of a path, in one case on Windows, where the file system does
/// not tell two cases apart.
fn folders(path: &Path) -> Vec<String> {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(if cfg!(windows) {
                part.to_string_lossy().to_lowercase()
            } else {
                part.to_string_lossy().into_owned()
            }),
            _ => None,
        })
        .collect()
}

fn same_case(text: &str) -> String {
    if cfg!(windows) {
        text.to_lowercase()
    } else {
        text.to_owned()
    }
}

fn ends_with(path: &[String], suffix: &[String]) -> bool {
    !suffix.is_empty() && path.len() >= suffix.len() && path[path.len() - suffix.len()..] == *suffix
}

fn contains(path: &[String], part: &[String]) -> bool {
    !part.is_empty() && path.windows(part.len()).any(|window| window == part)
}

/// The parts of a package name; the default package has none.
fn package_parts(name: &str) -> Vec<String> {
    if name == "(default package)" {
        return Vec::new();
    }
    name.split('.')
        .filter(|part| !part.is_empty())
        .map(same_case)
        .collect()
}

/// The Eclipse path of a destination: first the project, then the folders under it.
/// The server puts the source root and the package in it.
fn eclipse_path(detail: Option<&str>) -> Option<Vec<String>> {
    let detail = detail?;
    let path = detail
        .split_whitespace()
        .find(|part| part.starts_with('/'))?;
    let parts: Vec<String> = path
        .split('/')
        .filter(|part| !part.is_empty())
        .map(same_case)
        .collect();
    (!parts.is_empty()).then_some(parts)
}

/// The ends of a folder path a destination can mean. The server may print the project
/// and the source root only, or the package folder as well.
fn destination_suffixes(destination: &MoveDestination) -> Vec<Vec<String>> {
    let Some(path) = eclipse_path(destination.detail.as_deref()) else {
        return Vec::new();
    };
    let package = package_parts(&destination.name);
    let relative = path.get(1..).unwrap_or(&[]).to_vec();
    let mut suffixes = vec![path.clone(), relative.clone()];
    let mut with_package = relative;
    with_package.extend(package.iter().cloned());
    suffixes.push(with_package);
    let mut full_with_package = path;
    full_with_package.extend(package);
    suffixes.push(full_with_package);
    suffixes.retain(|suffix| !suffix.is_empty());
    suffixes
}

/// The destinations that name `folder`.
fn destinations_for(destinations: &[MoveDestination], folder: &Path) -> Vec<MoveDestination> {
    let folder = folders(folder);
    destinations
        .iter()
        .filter(|destination| {
            // A folder the server named is the exact answer: another module may have
            // the same package and source root under a different one.
            if let Some(named) = &destination.folder {
                return folders(named) == folder;
            }
            destination_suffixes(destination)
                .iter()
                .any(|suffix| ends_with(&folder, suffix))
        })
        .cloned()
        .collect()
}

/// Whether `folder` lies under the source root of any destination, so it is a package
/// folder even if the server did not offer it.
fn under_source_root(destinations: &[MoveDestination], folder: &Path) -> bool {
    let target = folders(folder);
    destinations.iter().any(|destination| {
        let package = package_parts(&destination.name);
        if let Some(named) = &destination.folder {
            let mut root = folders(named);
            if ends_with(&root, &package) {
                root.truncate(root.len() - package.len());
            }
            return !root.is_empty()
                && target.len() >= root.len()
                && target[..root.len()] == root[..];
        }
        let Some(mut root) = eclipse_path(destination.detail.as_deref()) else {
            return false;
        };
        if ends_with(&root, &package) {
            root.truncate(root.len() - package.len());
        }
        let relative = root.get(1..).unwrap_or(&[]);
        contains(&target, relative)
    })
}

impl EditorView {
    /// Alt+F7: the uses of the symbol, listed in the Usages window.
    pub fn find_usages(&mut self, cx: &mut Context<Self>) {
        self.usage_search(false, false, cx);
    }

    /// Ctrl+F7: the uses of the symbol in this file, listed in the Usages window.
    pub fn find_usages_in_file(&mut self, cx: &mut Context<Self>) {
        self.usage_search(false, true, cx);
    }

    /// Ctrl+Alt+F7: the uses of the symbol, in the popup at the caret.
    pub(crate) fn show_usages(&mut self, cx: &mut Context<Self>) {
        self.usage_search(true, false, cx);
    }

    /// The uses of the declaration the caret names: Ctrl+B on the name of a field, a
    /// method or a type shows them, one use goes there at once and several open the list
    /// of uses as Ctrl+Alt+F7 does.
    pub(crate) fn usages_of_declaration(&mut self, caret: usize, cx: &mut Context<Self>) {
        let Some((spot, _)) = self.spot() else {
            return;
        };
        let Some(support) = self.lsp.as_ref().map(|lsp| lsp.support.clone()) else {
            return;
        };
        let name = self.word_at_caret(caret);
        if let Some(provider) = support.usages() {
            let mut replies = ask(provider, spot);
            cx.spawn(async move |this, cx| {
                let mut latest = Vec::new();
                while let Some(answer) = replies.next().await {
                    if answer.confidence == Confidence::Unknown {
                        continue;
                    }
                    latest = answer.value;
                }
                this.update(cx, |this, cx| {
                    let targets: Vec<SourceSpan> =
                        latest.into_iter().map(|entry| entry.target).collect();
                    this.show_declaration_usages(caret, &name, targets, cx);
                })
                .ok();
            })
            .detach();
            return;
        }
        let Some(provider) = support.references() else {
            self.show_note(caret, "No usages found", cx);
            return;
        };
        let replies = ask(provider, spot);
        cx.spawn(async move |this, cx| {
            let Some((spans, confidence)) = first(replies).await else {
                return;
            };
            this.update(cx, |this, cx| {
                if confidence == Confidence::Unknown {
                    this.show_note(caret, "No usages found", cx);
                    return;
                }
                this.show_declaration_usages(caret, &name, spans, cx);
            })
            .ok();
        })
        .detach();
    }

    /// One use of a declaration goes there at once; several open the popup of uses.
    fn show_declaration_usages(
        &mut self,
        caret: usize,
        name: &str,
        targets: Vec<SourceSpan>,
        cx: &mut Context<Self>,
    ) {
        // The declaration is not a use of itself, and a server may name one place twice.
        let path = self.path().to_path_buf();
        let mut seen = std::collections::HashSet::new();
        let targets: Vec<SourceSpan> = targets
            .into_iter()
            .filter(|target| {
                let here = uri_path(&target.uri)
                    .is_some_and(|found| platform::path_key(&found) == platform::path_key(&path));
                let own_name = here
                    && self.state.as_ref().is_some_and(|state| {
                        let from = offset_at(&state.store(), target.span.start);
                        let to = offset_at(&state.store(), target.span.end).max(from);
                        from <= caret && caret <= to
                    });
                !own_name
                    && seen.insert((
                        target.uri.clone(),
                        target.span.start.line,
                        target.span.start.character,
                    ))
            })
            .collect();
        match targets.as_slice() {
            [] => self.show_note(caret, "No usages found", cx),
            [one] => {
                let target = one.clone();
                self.go_to_location(&target, cx);
            }
            _ => {
                // The popup opens with the search and takes the whole answer after it.
                cx.emit(EditorEvent::ShowUsages(UsagesUpdate {
                    name: name.to_owned(),
                    usages: Vec::new(),
                    done: false,
                }));
                cx.emit(EditorEvent::ShowUsages(UsagesUpdate {
                    name: name.to_owned(),
                    usages: usages_of(name, targets),
                    done: true,
                }));
            }
        }
    }

    /// The places of the usages answer that are in this file, so Ctrl+Alt+Down and
    /// Ctrl+Alt+Up walk them without the focus leaving the editor.
    fn set_usage_ranges(&mut self, spans: &[SourceSpan]) {
        let Some(state) = &self.state else {
            return;
        };
        let path = self.path().to_path_buf();
        let store = state.store();
        let mut ranges: Vec<Range<usize>> = spans
            .iter()
            .filter(|span| uri_path(&span.uri).as_deref() == Some(path.as_path()))
            .map(|span| {
                let start = offset_at(&store, span.span.start);
                start..offset_at(&store, span.span.end).max(start)
            })
            .collect();
        ranges.sort_by_key(|range| range.start);
        ranges.dedup();
        self.usages = (!ranges.is_empty()).then_some(ranges);
    }

    /// How often a search or a list that waits for the language server asks whether it is
    /// ready, in milliseconds.
    pub(crate) const WAIT_TICK_MS: u64 = 200;

    /// Alt+F7 or Ctrl+F7 while the server imports. The Usages window opens at once with the
    /// waiting line, and the search runs by itself when the server is ready, at the caret
    /// it was asked at. Escape, a closed tab or a hidden window cancel the wait.
    fn wait_for_usages(
        &mut self,
        name: String,
        in_file: bool,
        caret: usize,
        cx: &mut Context<Self>,
    ) {
        let cancel = ::base::Cancel::new();
        cx.emit(EditorEvent::UsagesWaiting {
            name,
            in_file,
            cancel: cancel.clone(),
        });
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(Self::WAIT_TICK_MS))
                    .await;
                if cancel.is_cancelled() {
                    return;
                }
                match this.update(cx, |this, _| this.server_ready()) {
                    Ok(true) => break,
                    Ok(false) => {}
                    Err(_) => return,
                }
            }
            this.update(cx, |this, cx| {
                if !cancel.is_cancelled() {
                    this.usage_search_at(caret, false, in_file, true, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// The end of a search that waited for the server, when the server then gives no
    /// answer: the Usages window shows no uses, so its waiting line does not stay.
    fn end_waiting_search(&mut self, in_file: bool, detailed: bool, cx: &mut Context<Self>) {
        cx.emit(match (detailed, in_file) {
            (true, true) => EditorEvent::UsagesDetailedInFile(Vec::new()),
            (true, false) => EditorEvent::UsagesDetailed(Vec::new()),
            (false, true) => EditorEvent::UsagesInFile(Vec::new()),
            (false, false) => EditorEvent::Usages(Vec::new()),
        });
    }

    pub(crate) fn usage_search(&mut self, popup: bool, in_file: bool, cx: &mut Context<Self>) {
        if in_file {
            let has_provider = self.lsp.as_ref().is_some_and(|lsp| {
                lsp.support.usages().is_some() || lsp.support.references().is_some()
            });
            // A file whose language has no usages keeps the marks of Ctrl+Shift+F7.
            if !has_provider {
                return self.highlight_usages(cx);
            }
        }
        let Some((_, caret)) = self.spot() else {
            return;
        };
        self.usage_search_at(caret, popup, in_file, false, cx);
    }

    /// The search of [`EditorView::usage_search`] for the caret at `caret`. A search that
    /// waited for the server is `resumed`, so an answer that is still missing ends the
    /// Usages window with no uses.
    fn usage_search_at(
        &mut self,
        caret: usize,
        popup: bool,
        in_file: bool,
        resumed: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(len) = self.state.as_ref().map(|state| state.store().len()) else {
            return;
        };
        let caret = caret.min(len);
        let Some(spot) = self.spot_at(caret) else {
            return;
        };
        let Some(support) = self.lsp.as_ref().map(|lsp| lsp.support.clone()) else {
            return;
        };
        let name = self.word_at_caret(caret);
        let searches = support.usages().is_some() || support.references().is_some();
        if searches && !self.server_ready() {
            if !popup {
                return self.wait_for_usages(name, in_file, caret, cx);
            }
            self.importing_note(caret, cx);
        }
        if !popup {
            let detailed = support.usages();
            let references = support.references();
            if let Some(provider) = detailed {
                let replies = ask(provider, spot);
                let support = support.clone();
                let here = in_file.then(|| self.path.clone());
                cx.spawn(async move |this, cx| {
                    let answer = first(replies).await;
                    this.update(cx, |this, cx| match answer {
                        Some((_, Confidence::Unknown)) => {
                            this.show_note(caret, NO_ANSWER, cx);
                            if resumed {
                                this.end_waiting_search(in_file, true, cx);
                            }
                        }
                        Some((mut entries, _)) => {
                            if let Some(here) = &here {
                                entries.retain(|entry| same_file(&entry.target.uri, here));
                            }
                            let usages = detailed_usages_of(&name, entries, &*support);
                            cx.emit(if here.is_some() {
                                EditorEvent::UsagesDetailedInFile(usages)
                            } else {
                                EditorEvent::UsagesDetailed(usages)
                            });
                        }
                        None => {
                            if resumed {
                                this.end_waiting_search(in_file, true, cx);
                            }
                        }
                    })
                    .ok();
                })
                .detach();
                return;
            }
            let Some(provider) = references else {
                return;
            };
            let replies = ask(provider, spot);
            let here = in_file.then(|| self.path.clone());
            cx.spawn(async move |this, cx| {
                let answer = first(replies).await;
                this.update(cx, |this, cx| match answer {
                    Some((_, Confidence::Unknown)) => {
                        this.show_note(caret, NO_ANSWER, cx);
                        if resumed {
                            this.end_waiting_search(in_file, false, cx);
                        }
                    }
                    Some((mut spans, _)) => {
                        if let Some(here) = &here {
                            spans.retain(|span| same_file(&span.uri, here));
                        }
                        this.set_usage_ranges(&spans);
                        let usages = usages_of(&name, spans);
                        cx.emit(if here.is_some() {
                            EditorEvent::UsagesInFile(usages)
                        } else {
                            EditorEvent::Usages(usages)
                        });
                    }
                    None => {
                        if resumed {
                            this.end_waiting_search(in_file, false, cx);
                        }
                    }
                })
                .ok();
            })
            .detach();
            return;
        }
        if let Some(provider) = support.usages() {
            let mut replies = ask(provider, spot);
            cx.spawn(async move |this, cx| {
                let opened = this.update(cx, |_, cx| {
                    cx.emit(EditorEvent::ShowUsages(UsagesUpdate {
                        name: name.clone(),
                        usages: Vec::new(),
                        done: false,
                    }));
                });
                if opened.is_err() {
                    return;
                }
                let mut latest = Vec::new();
                while let Some(answer) = replies.next().await {
                    if answer.confidence == Confidence::Unknown {
                        continue;
                    }
                    latest = answer.value;
                    let usages = usages_of(
                        &name,
                        latest
                            .iter()
                            .filter(|entry| entry.kind != lang_api::UsageKind::Declaration)
                            .map(|entry| entry.target.clone())
                            .collect(),
                    );
                    let shown = this.update(cx, |_, cx| {
                        cx.emit(EditorEvent::ShowUsages(UsagesUpdate {
                            name: name.clone(),
                            usages,
                            done: false,
                        }));
                    });
                    if shown.is_err() {
                        return;
                    }
                }
                this.update(cx, |_, cx| {
                    let spans = latest
                        .iter()
                        .filter(|entry| entry.kind != lang_api::UsageKind::Declaration)
                        .map(|entry| entry.target.clone())
                        .collect();
                    cx.emit(EditorEvent::ShowUsages(UsagesUpdate {
                        name: name.clone(),
                        usages: usages_of(&name, spans),
                        done: true,
                    }));
                })
                .ok();
            })
            .detach();
            return;
        }
        let Some(provider) = support.references() else {
            return;
        };
        let mut replies = ask(provider, spot);
        cx.spawn(async move |this, cx| {
            let opened = this.update(cx, |_, cx| {
                cx.emit(EditorEvent::ShowUsages(UsagesUpdate {
                    name: name.clone(),
                    usages: Vec::new(),
                    done: false,
                }));
            });
            if opened.is_err() {
                return;
            }
            let mut latest = Vec::new();
            while let Some(answer) = replies.next().await {
                if answer.confidence == Confidence::Unknown {
                    continue;
                }
                latest = answer.value;
                let usages = usages_of(&name, latest.clone());
                let shown = this.update(cx, |_, cx| {
                    cx.emit(EditorEvent::ShowUsages(UsagesUpdate {
                        name: name.clone(),
                        usages,
                        done: false,
                    }));
                });
                if shown.is_err() {
                    return;
                }
            }
            this.update(cx, |_, cx| {
                cx.emit(EditorEvent::ShowUsages(UsagesUpdate {
                    name: name.clone(),
                    usages: usages_of(&name, latest),
                    done: true,
                }));
            })
            .ok();
        })
        .detach();
    }

    /// The word under the caret, the name the usages popup shows.
    pub(crate) fn word_at_caret(&self, caret: usize) -> String {
        self.state
            .as_ref()
            .map(|state| {
                let store = state.store();
                let (mut start, mut end) = (caret, caret);
                while start > 0
                    && store
                        .char_at(store.prev_boundary(start))
                        .is_some_and(is_word)
                {
                    start = store.prev_boundary(start);
                }
                while store.char_at(end).is_some_and(is_word) {
                    end = store.next_boundary(end);
                }
                store.slice(start..end)
            })
            .unwrap_or_default()
    }

    /// Shift+F6 opens the field with the word under the caret, or starts changing the
    /// name in the text itself. A second Shift+F6 in that mode opens the field.
    pub(crate) fn start_rename(&mut self, cx: &mut Context<Self>) {
        if self
            .lsp
            .as_ref()
            .is_some_and(|lsp| lsp.rename_in_place.is_some())
        {
            self.open_rename_options(cx);
            return;
        }
        let Some((spot, caret)) = self.spot() else {
            return;
        };
        let Some(state) = &self.state else {
            return;
        };
        let store = state.store();
        let mut start = caret;
        while start > 0
            && store
                .char_at(store.prev_boundary(start))
                .is_some_and(is_word)
        {
            start = store.prev_boundary(start);
        }
        let mut end = caret;
        while store.char_at(end).is_some_and(is_word) {
            end = store.next_boundary(end);
        }
        if start == end {
            // Not a name: nothing changes, and the status bar says why.
            self.report_status("Put the caret on a name to rename", cx);
            return;
        }
        let text = store.slice(start..end);
        // A type whose name is the name of the file is renamed with the file.
        if self
            .path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .is_some_and(|stem| stem == text)
        {
            self.open_rename_field(start, text, spot, cx);
            return;
        }
        let Some(lsp) = &self.lsp else {
            return;
        };
        if lsp.support.prepare_rename().is_some() {
            // The mode starts at once with the places of the name in this file. The
            // language answers later, and its refusal ends the mode.
            if !can_rename_in_place(&text, lsp.support.reserved_word(&text)) {
                self.report_status(REFUSED, cx);
                return;
            }
            self.begin_rename_in_place(spot, start..end, cx);
            return;
        }
        if lsp.support.rename().is_some() || lsp.support.server().is_none() {
            if lsp.support.rename().is_none() {
                // No server: the text of this file is all the editor can change.
                if !can_rename_in_place(&text, lsp.support.reserved_word(&text)) {
                    self.report_status(REFUSED, cx);
                    return;
                }
                self.begin_rename_in_place(spot, start..end, cx);
            } else {
                self.open_rename_field(start, text, spot, cx);
            }
            return;
        }
        self.rename_not_available(start, cx);
    }

    /// The window with the name field, which previews the change of every file.
    fn open_rename_field(
        &mut self,
        at: usize,
        text: String,
        spot: DocumentSpot,
        cx: &mut Context<Self>,
    ) {
        if let Some(lsp) = &mut self.lsp {
            lsp.completion = None;
            lsp.actions = None;
            lsp.note = None;
            lsp.move_to = None;
            lsp.name = None;
            lsp.rename_in_place = None;
            lsp.rename = Some(RenameField {
                at,
                spot,
                text,
                preview: None,
                summary: None,
            });
            cx.notify();
        }
    }

    /// Starts the linked editing of the name at `word` at once, with the places of the name
    /// in this file. The language answers later: a refusal ends the mode, and its places
    /// replace the whole-word ones when they come before the first typed character.
    fn begin_rename_in_place(
        &mut self,
        spot: DocumentSpot,
        word: Range<usize>,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = &mut self.state else {
            return;
        };
        let Some(mut rename) = RenameInPlace::new(state, word) else {
            return;
        };
        let main = rename.main_range();
        let Some(lsp) = &mut self.lsp else {
            return;
        };
        lsp.rename_session += 1;
        let session = lsp.rename_session;
        rename.session = session;
        rename.waiting = lsp.support.prepare_rename().is_some();
        let asks_prepare = rename.waiting;
        lsp.completion = None;
        lsp.actions = None;
        lsp.note = None;
        lsp.move_to = None;
        lsp.name = None;
        lsp.rename = None;
        lsp.rename_pending = false;
        lsp.highlights.clear();
        lsp.rename_in_place = Some(rename);
        state.select_range(main);
        if asks_prepare {
            self.ask_prepare_in_place(spot.clone(), session, cx);
        }
        self.rename_occurrences(spot, session, cx);
        cx.notify();
    }

    /// Asks the language whether it renames the name at `spot`. Its answer counts only for
    /// the mode of `session`.
    fn ask_prepare_in_place(&mut self, spot: DocumentSpot, session: u64, cx: &mut Context<Self>) {
        let Some(provider) = self
            .lsp
            .as_ref()
            .and_then(|lsp| lsp.support.prepare_rename())
        else {
            return;
        };
        let replies = ask(provider, spot);
        cx.spawn(async move |this, cx| {
            let answer = first(replies).await;
            this.update(cx, |this, cx| this.prepare_answered(session, answer, cx))
                .ok();
        })
        .detach();
    }

    /// The language answered about the name the mode changes. A refusal ends the mode and
    /// puts the text back. An answer that never came keeps the mode on the places of this
    /// file, unless Enter waits for it: then Enter goes on or the name stays here.
    fn prepare_answered(
        &mut self,
        session: u64,
        answer: Option<(Option<PrepareRename>, Confidence)>,
        cx: &mut Context<Self>,
    ) {
        let Some(rename) = self
            .lsp
            .as_mut()
            .and_then(|lsp| lsp.rename_in_place.as_mut())
            .filter(|rename| rename.session == session)
        else {
            return;
        };
        rename.waiting = false;
        let finish = std::mem::take(&mut rename.finish_when_ready);
        match prepare_outcome(&answer) {
            PrepareOutcome::Refused => {
                self.cancel_rename_in_place(cx);
                cx.emit(EditorEvent::Status(REFUSED.into()));
            }
            PrepareOutcome::Accepted | PrepareOutcome::Stale if finish => {
                self.finish_rename_in_place(cx);
            }
            PrepareOutcome::NotReady if finish => self.end_rename_not_ready(cx),
            _ => cx.notify(),
        }
    }

    /// Asks the language for the places of the name, so that the occurrences are the
    /// symbol's and not the same word in a comment. The answer counts only for the mode of
    /// `session`.
    fn rename_occurrences(&mut self, spot: DocumentSpot, session: u64, cx: &mut Context<Self>) {
        let Some(provider) = self.lsp.as_ref().and_then(|lsp| lsp.support.highlights()) else {
            return;
        };
        let replies = ask(provider, spot.clone());
        cx.spawn(async move |this, cx| {
            let answer = first(replies).await;
            this.update(cx, |this, cx| {
                let Some((spans, Confidence::Certain)) = answer else {
                    return;
                };
                let (Some(state), Some(lsp)) = (&mut this.state, &mut this.lsp) else {
                    return;
                };
                if state.version() as i32 != spot.version {
                    return;
                }
                let Some(rename) = &mut lsp.rename_in_place else {
                    return;
                };
                if rename.session != session || rename.text != rename.original {
                    return;
                }
                let store = state.store();
                let mut ranges: Vec<Range<usize>> = spans
                    .into_iter()
                    .map(|span| {
                        let start = offset_at(&store, span.start);
                        start..offset_at(&store, span.end).max(start)
                    })
                    .filter(|range| range.start < range.end)
                    .collect();
                ranges.sort_by_key(|range| range.start);
                ranges.dedup();
                if ranges.is_empty() {
                    return;
                }
                let main_at = rename.main_start();
                let main = match ranges
                    .iter()
                    .position(|range| range.start <= main_at && main_at <= range.end)
                    .or_else(|| ranges.iter().position(|range| range.start >= main_at))
                {
                    Some(main) => main,
                    None => return,
                };
                let focused = ranges[main].clone();
                rename.spans = ranges;
                rename.main = main;
                state.select_range(focused);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// A name the language refuses: the status bar says so and no mode starts.
    fn rename_not_available(&mut self, at: usize, cx: &mut Context<Self>) {
        let message: SharedString = "This name cannot be renamed".into();
        self.show_note(at, message.clone(), cx);
        cx.emit(EditorEvent::Status(message));
    }

    /// The second Shift+F6: the field of the name with the options, as it was before the
    /// linked editing. The text of the file goes back to the original name.
    fn open_rename_options(&mut self, cx: &mut Context<Self>) {
        let Some(rename) = self.lsp.as_mut().and_then(|lsp| lsp.rename_in_place.take()) else {
            return;
        };
        let text = rename.text.clone();
        let at = if text != rename.original {
            rename.main_start_after_revert()
        } else {
            rename.main_start()
        };
        if text != rename.original {
            let edits = rename.revert_edits();
            let after = rename.selection_after_revert();
            let change = self.state.as_mut().and_then(|state| {
                let change = state.apply_template_edits(edits, after, EditKind::Command);
                state.settle_saved_text();
                change
            });
            self.changed(change, cx);
        }
        let spot = self.spot_at(at);
        if let (Some(lsp), Some(spot)) = (&mut self.lsp, spot) {
            lsp.rename = Some(RenameField {
                at,
                spot,
                text,
                preview: None,
                summary: None,
            });
        }
        cx.notify();
    }

    /// Typed text goes to an open rename field or to the name changed in place. Returns
    /// whether it took the text.
    pub(crate) fn rename_typed(&mut self, text: &str, cx: &mut Context<Self>) -> bool {
        if self
            .lsp
            .as_ref()
            .is_some_and(|lsp| lsp.rename_in_place.is_some())
        {
            let typed = text.trim_end_matches(['\r', '\n']);
            if typed.is_empty() {
                return true;
            }
            self.type_in_rename_place(typed, cx);
            return true;
        }
        let Some(field) = self.lsp.as_mut().and_then(|lsp| lsp.rename.as_mut()) else {
            // The language is still renaming the name the mode left: the typing goes on in
            // the text, and a note at the caret says why the rename has not come yet.
            let pending = self.lsp.as_ref().is_some_and(|lsp| lsp.rename_pending);
            let at = self
                .state
                .as_ref()
                .map(|state| state.selections().primary().head);
            if let (true, Some(at)) = (pending, at) {
                self.show_note(at, WAITING_FOR_LANGUAGE, cx);
            }
            return false;
        };
        field.text.push_str(text.trim_end_matches(['\r', '\n']));
        field.preview = None;
        field.summary = None;
        cx.notify();
        true
    }

    /// Keys of an open rename field, or of a name changed in place. Returns whether the
    /// key was taken.
    pub(crate) fn rename_key(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
        if self
            .lsp
            .as_ref()
            .is_some_and(|lsp| lsp.rename_in_place.is_some())
        {
            return match key {
                "escape" => {
                    self.cancel_rename_in_place(cx);
                    true
                }
                "enter" => {
                    self.finish_rename_in_place(cx);
                    true
                }
                "backspace" => {
                    self.backspace_rename_place(cx);
                    true
                }
                _ => !is_typed_key(key),
            };
        }
        let Some(lsp) = &mut self.lsp else {
            return false;
        };
        let Some(field) = &mut lsp.rename else {
            return false;
        };
        match key {
            "escape" => lsp.rename = None,
            "backspace" => {
                field.text.pop();
                field.preview = None;
                field.summary = None;
            }
            "enter" => match field.preview.take() {
                Some((version, change)) => {
                    lsp.rename = None;
                    self.apply_change_labeled(version, change, Some("Rename"), cx);
                }
                None => self.ask_rename(cx),
            },
            // A typed character goes through text input, where `rename_typed` appends it.
            // Every other key stays in the field, so the text is not edited under it.
            _ => return !is_typed_key(key),
        }
        cx.notify();
        true
    }

    fn type_in_rename_place(&mut self, text: &str, cx: &mut Context<Self>) {
        let change = match (&mut self.state, &mut self.lsp) {
            (Some(state), Some(lsp)) => lsp
                .rename_in_place
                .as_mut()
                .and_then(|rename| rename.type_text(state, text, false)),
            _ => None,
        };
        self.refresh_rename_problem();
        self.changed(change, cx);
    }

    fn backspace_rename_place(&mut self, cx: &mut Context<Self>) {
        let change = match (&mut self.state, &mut self.lsp) {
            (Some(state), Some(lsp)) => lsp
                .rename_in_place
                .as_mut()
                .and_then(|rename| rename.backspace(state, false)),
            _ => None,
        };
        self.refresh_rename_problem();
        self.changed(change, cx);
    }

    fn refresh_rename_problem(&mut self) {
        let Some(lsp) = &mut self.lsp else {
            return;
        };
        let support = lsp.support.clone();
        if let Some(rename) = &mut lsp.rename_in_place {
            let reserved = support.reserved_word(&rename.text);
            rename.refresh_problem(reserved);
        }
    }

    /// Escape: the original name comes back into every occurrence and the mode ends.
    fn cancel_rename_in_place(&mut self, cx: &mut Context<Self>) {
        let Some(mut rename) = self.lsp.as_mut().and_then(|lsp| lsp.rename_in_place.take()) else {
            return;
        };
        let collapsed = if rename.text == rename.original {
            Some(rename.main_start() + rename.original.len())
        } else {
            None
        };
        let change = self.state.as_mut().and_then(|state| {
            let change = rename.cancel(state);
            if let Some(at) = collapsed {
                state.select_range(at..at);
            }
            change
        });
        self.changed(change, cx);
    }

    /// Enter: the name stays in this file and the language renames every other place. While
    /// the language has not answered about the name, Enter waits for its answer.
    pub(crate) fn finish_rename_in_place(&mut self, cx: &mut Context<Self>) {
        self.end_rename_in_place(true, cx);
    }

    /// A click or a blur ends the mode as Enter does, except that it does not wait for the
    /// language: a name the language has not answered for stays in this file.
    pub(crate) fn leave_rename_in_place(&mut self, cx: &mut Context<Self>) {
        self.end_rename_in_place(false, cx);
    }

    fn end_rename_in_place(&mut self, may_wait: bool, cx: &mut Context<Self>) {
        if self
            .lsp
            .as_ref()
            .and_then(|lsp| lsp.rename_in_place.as_ref())
            .is_some_and(|rename| rename.problem.is_some())
        {
            cx.notify();
            return;
        }
        let renames = self
            .lsp
            .as_ref()
            .is_some_and(|lsp| lsp.support.rename().is_some());
        let waiting = renames
            && self
                .lsp
                .as_ref()
                .and_then(|lsp| lsp.rename_in_place.as_ref())
                .is_some_and(|rename| rename.waiting && rename.text != rename.original);
        if waiting {
            if may_wait {
                if let Some(rename) = self
                    .lsp
                    .as_mut()
                    .and_then(|lsp| lsp.rename_in_place.as_mut())
                {
                    rename.finish_when_ready = true;
                }
                cx.notify();
            } else {
                self.end_rename_not_ready(cx);
            }
            return;
        }
        let Some(rename) = self.lsp.as_mut().and_then(|lsp| lsp.rename_in_place.take()) else {
            return;
        };
        if rename.text == rename.original {
            if let Some(state) = &mut self.state {
                let at = rename.main_start() + rename.original.len();
                state.select_range(at..at);
                state.settle_saved_text();
            }
            cx.notify();
            return;
        }
        // No server: the text of this file is the whole rename, and it is already there.
        if !renames {
            if let Some(state) = &mut self.state {
                let at = rename.main_start() + rename.text.len();
                state.select_range(at..at);
            }
            cx.notify();
            return;
        }
        self.rename_across(rename, cx);
    }

    /// Asks the language to rename the name across the project. The edits of this file are
    /// taken back first, so that the language renames the name it knows, and its change
    /// applies. When the language does not rename it, the typed name goes back here.
    fn rename_across(&mut self, mut rename: RenameInPlace, cx: &mut Context<Self>) {
        let at = rename.main_start_after_revert();
        let new_name = rename.text.clone();
        let edits = rename.revert_edits();
        let after = rename.selection_after_revert();
        let revert = self
            .state
            .as_mut()
            .and_then(|state| state.apply_template_edits(edits, after, EditKind::Command));
        // The places of the name follow the revert, so that they can be typed over again.
        if let (Some(revert), Some(state)) = (&revert, &self.state) {
            rename.follow(&revert.steps, &state.text(), state.version(), false);
        }
        let version_after = self.state.as_ref().map_or(0, |state| state.version());
        self.changed(revert, cx);
        let Some(spot) = self.spot_at(at) else {
            self.keep_typed_name(rename, version_after, new_name, cx);
            return;
        };
        let Some(provider) = self.lsp.as_ref().and_then(|lsp| lsp.support.rename()) else {
            self.keep_typed_name(rename, version_after, new_name, cx);
            return;
        };
        let version = spot.version;
        let replies = ask(
            provider,
            RenameQuery {
                spot,
                new_name: new_name.clone(),
            },
        );
        if let Some(lsp) = &mut self.lsp {
            lsp.rename_pending = true;
        }
        cx.spawn(async move |this, cx| {
            let answer = first(replies).await;
            this.update(cx, |this, cx| {
                if let Some(lsp) = &mut this.lsp {
                    lsp.rename_pending = false;
                }
                match answer {
                    Some((change, Confidence::Certain)) if !change.is_empty() => {
                        this.apply_change_labeled(version, change, Some("Rename"), cx);
                    }
                    answer => {
                        this.keep_typed_name(rename, version_after, new_name, cx);
                        let message = match answer {
                            Some((_, Confidence::Certain)) => "Cannot rename this",
                            _ => NOT_READY,
                        };
                        this.show_note(at, message, cx);
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// Puts the typed name back into the places the rename took out, so that the name stays
    /// in this file. The text is left alone when it changed since the revert: the places of
    /// the name would be wrong.
    fn keep_typed_name(
        &mut self,
        mut rename: RenameInPlace,
        version_after: u64,
        new_name: String,
        cx: &mut Context<Self>,
    ) {
        let change = match &mut self.state {
            Some(state) if state.version() == version_after => rename.replace_all(state, &new_name),
            _ => None,
        };
        self.changed(change, cx);
    }

    /// The language cannot rename the name now: the mode ends, the typed name stays in this
    /// file, and a note at the caret says so.
    fn end_rename_not_ready(&mut self, cx: &mut Context<Self>) {
        let Some(rename) = self.lsp.as_mut().and_then(|lsp| lsp.rename_in_place.take()) else {
            return;
        };
        let at = rename.main_start() + rename.text.len();
        if let Some(state) = &mut self.state {
            state.select_range(at..at);
            if rename.text == rename.original {
                state.settle_saved_text();
            }
        }
        if rename.text != rename.original {
            self.show_note(at, NOT_READY, cx);
        }
        cx.notify();
    }

    fn ask_rename(&mut self, cx: &mut Context<Self>) {
        let Some(lsp) = &self.lsp else {
            return;
        };
        let (Some(provider), Some(field)) = (lsp.support.rename(), &lsp.rename) else {
            return;
        };
        let query = RenameQuery {
            spot: field.spot.clone(),
            new_name: field.text.clone(),
        };
        let (version, asked) = (field.spot.version, field.text.clone());
        let replies = ask(provider, query);
        cx.spawn(async move |this, cx| {
            let answer = first(replies).await;
            this.update(cx, |this, cx| {
                let Some(lsp) = this.lsp.as_mut() else {
                    return;
                };
                let Some(field) = lsp.rename.as_mut() else {
                    return;
                };
                if field.text != asked {
                    return;
                }
                match answer {
                    Some((change, Confidence::Certain)) if !change.is_empty() => {
                        field.summary = Some(summary(&change));
                        field.preview = Some((version, change));
                    }
                    Some((_, Confidence::Unknown)) => field.summary = Some(NO_ANSWER.to_owned()),
                    Some(_) => field.summary = Some("Cannot rename this".to_owned()),
                    // The text changed under the field: it asked about an old one.
                    None => lsp.rename = None,
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Whether the change stays in the text of this file.
    fn local_change(&self, change: &WorkspaceChange) -> bool {
        let Some(lsp) = &self.lsp else {
            return false;
        };
        change.renames.is_empty()
            && change
                .edits
                .iter()
                .all(|source| source.uri == lsp.uri || source.edits.is_empty())
    }

    /// Opens the field of the name the server introduced, with the name it chose as the
    /// text: the user types over it, as IDEA renames the new name in place. An action
    /// without such a place applies as it is.
    pub(crate) fn start_extract_name(
        &mut self,
        version: i32,
        entry: ActionEntry,
        label: Option<&'static str>,
        cx: &mut Context<Self>,
    ) {
        let apply = |this: &mut Self, cx: &mut Context<Self>| {
            let change = WorkspaceChange {
                edits: entry.edits.clone(),
                renames: entry.renames.clone(),
            };
            this.apply_change_labeled(version, change, label, cx);
        };
        let Some(rename) = entry.rename.clone() else {
            apply(self, cx);
            return;
        };
        let Some(lsp) = &self.lsp else {
            return;
        };
        if rename.uri != lsp.uri
            || !self.local_change(&WorkspaceChange {
                edits: entry.edits.clone(),
                renames: entry.renames.clone(),
            })
        {
            apply(self, cx);
            return;
        }
        let Some(state) = &self.state else {
            return;
        };
        let Some(after) = text_after(&entry, &lsp.uri, &state.text()) else {
            apply(self, cx);
            return;
        };
        let Some(default_name) = name_at(&after, &rename) else {
            apply(self, cx);
            return;
        };
        if default_name.is_empty() {
            apply(self, cx);
            return;
        }
        let at = state.selections().primary().head;
        let label = label
            .unwrap_or_else(|| Refactoring::from_code(entry.code.as_deref().unwrap_or_default()));
        let method = entry
            .code
            .as_deref()
            .is_some_and(|code| code.starts_with("refactor.extract.function"));
        if let Some(lsp) = &mut self.lsp {
            lsp.completion = None;
            lsp.actions = None;
            lsp.note = None;
            lsp.rename = None;
            lsp.move_to = None;
            lsp.name = Some(ExtractNameField {
                at,
                label,
                text: default_name.clone(),
                selected: true,
                entry,
                version,
                rename,
                default_name,
                visibility: method.then_some(Visibility::Private),
            });
        }
        cx.notify();
    }

    /// Typed text goes to an open extract name field. The first character replaces the
    /// name of the server.
    pub(crate) fn name_typed(&mut self, text: &str, cx: &mut Context<Self>) -> bool {
        let Some(field) = self.lsp.as_mut().and_then(|lsp| lsp.name.as_mut()) else {
            return false;
        };
        if field.selected {
            field.text.clear();
            field.selected = false;
        }
        field.text.push_str(text.trim_end_matches(['\r', '\n']));
        cx.notify();
        true
    }

    /// Keys of an open extract name field. Returns whether the key was taken.
    pub(crate) fn name_key(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
        let Some(lsp) = &mut self.lsp else {
            return false;
        };
        let Some(field) = lsp.name.as_mut() else {
            return false;
        };
        match key {
            "escape" => lsp.name = None,
            "backspace" => {
                if field.selected {
                    field.text.clear();
                    field.selected = false;
                } else {
                    field.text.pop();
                }
            }
            "up" | "down" => {
                if let Some(visibility) = field.visibility.as_mut() {
                    *visibility = visibility.next(key == "down");
                }
            }
            "enter" => {
                let name = lsp.name.take().expect("the name field is open");
                self.finish_extract_name(name, cx);
                return true;
            }
            // A typed character goes through text input, where `name_typed` writes it.
            _ => return !is_typed_key(key),
        }
        cx.notify();
        true
    }

    /// Applies the refactoring with the typed name, then puts the name into every place
    /// the server introduced, as one step of undo.
    fn finish_extract_name(&mut self, field: ExtractNameField, cx: &mut Context<Self>) {
        let ExtractNameField {
            at,
            label,
            text,
            entry,
            version,
            rename,
            default_name,
            visibility,
            ..
        } = field;
        let change = WorkspaceChange {
            edits: entry.edits.clone(),
            renames: entry.renames.clone(),
        };
        if !self.local_change(&change) {
            let message =
                "The refactoring reached other files, so the name stays as the server chose it";
            self.apply_change_labeled(version, change, Some(label), cx);
            self.show_note(at, message, cx);
            return;
        }
        self.apply_change_labeled(version, change, Some(label), cx);
        let Some(name_start) = self
            .state
            .as_ref()
            .and_then(|state| byte_at_utf16(&state.text(), rename.offset))
        else {
            return;
        };
        let mut steps = 1;
        let mut shift = 0isize;
        if let Some(visibility) = visibility {
            shift = self.apply_visibility(name_start, visibility, cx);
            if shift != 0 {
                steps += 1;
            }
        }
        let name = text.trim().to_owned();
        if name.is_empty() || name == default_name {
            self.merge_undo_steps(steps);
            return;
        }
        let start = (name_start as isize + shift).max(0) as usize;
        let Some(provider) = self.lsp.as_ref().and_then(|lsp| lsp.support.rename()) else {
            self.merge_undo_steps(steps);
            return;
        };
        let Some(spot) = self.spot_at(start) else {
            self.merge_undo_steps(steps);
            return;
        };
        let asked = spot.version;
        let replies = ask(
            provider,
            RenameQuery {
                spot,
                new_name: name.clone(),
            },
        );
        cx.spawn(async move |this, cx| {
            let answer = first(replies).await;
            this.update(cx, |this, cx| match answer {
                Some((change, Confidence::Certain)) if !change.is_empty() => {
                    this.apply_change_labeled(asked, change, Some(label), cx);
                    this.merge_undo_steps(steps + 1);
                }
                Some((_, Confidence::Unknown)) => {
                    this.merge_undo_steps(steps);
                    this.show_note(at, NO_ANSWER, cx);
                }
                Some(_) => {
                    this.merge_undo_steps(steps);
                    this.show_note(at, "Cannot rename the new name", cx);
                }
                None => this.merge_undo_steps(steps),
            })
            .ok();
        })
        .detach();
    }

    /// The step of undo per change: the extract and the name it introduced are one press.
    fn merge_undo_steps(&mut self, count: usize) {
        if count > 1
            && let Some(state) = &mut self.state
        {
            state.merge_last_undo(count);
        }
    }

    /// Replaces the modifier of the method the extract introduced. Returns how far the
    /// text moved, in bytes.
    fn apply_visibility(
        &mut self,
        name_start: usize,
        visibility: Visibility,
        cx: &mut Context<Self>,
    ) -> isize {
        let Some(state) = &self.state else {
            return 0;
        };
        let text = state.text();
        if name_start > text.len() {
            return 0;
        }
        let line_start = text[..name_start].rfind('\n').map_or(0, |at| at + 1);
        let head = &text[line_start..name_start];
        let trimmed = head.trim_start();
        let indent = head.len() - trimmed.len();
        let Some(old) = ["private", "protected", "public"]
            .into_iter()
            .find(|word| trimmed.starts_with(word))
        else {
            return 0;
        };
        let start = line_start + indent;
        let mut end = start + old.len();
        if !matches!(text[end..].chars().next(), Some(' ' | '\t')) {
            return 0;
        }
        let replacement = visibility.modifier().unwrap_or_default();
        if old == replacement {
            return 0;
        }
        // The space after the modifier goes with it only when nothing takes its place.
        if replacement.is_empty() {
            end += 1;
        }
        let shift = replacement.len() as isize - (end - start) as isize;
        let span = TextSpan {
            start: point_at(&state.store(), start),
            end: point_at(&state.store(), end),
        };
        self.apply_document_edits(
            vec![DocumentEdit {
                span,
                text: replacement.to_owned(),
            }],
            cx,
        );
        shift
    }

    /// Ctrl+F6: the window shows the signature of the place and edits it.
    pub(crate) fn change_signature(&mut self, cx: &mut Context<Self>) {
        let Some((spot, _)) = self.spot() else {
            return;
        };
        cx.emit(EditorEvent::ChangeSignature(spot));
    }

    /// Pull Members Up, Push Members Down, Extract Superclass and Introduce Parameter
    /// Object: the window asks the language for the members and the places.
    pub(crate) fn member_refactoring(
        &mut self,
        refactoring: lang_api::MemberRefactoring,
        cx: &mut Context<Self>,
    ) {
        let Some((spot, _)) = self.spot() else {
            return;
        };
        cx.emit(EditorEvent::MemberRefactoring { refactoring, spot });
    }

    /// F6: on a member it opens the form of Move Members, and elsewhere it asks where
    /// the file can go and shows the destinations in a searchable list.
    pub(crate) fn start_move(&mut self, cx: &mut Context<Self>) {
        let Some((spot, caret)) = self.spot() else {
            return;
        };
        let place = match (&self.lsp, &self.state) {
            (Some(lsp), Some(state)) => lsp.support.move_place(&state.text(), caret),
            _ => MovePlace::File,
        };
        match place {
            MovePlace::Member => {
                cx.emit(EditorEvent::MoveMembers(spot));
                return;
            }
            MovePlace::InstanceMethod => {
                cx.emit(EditorEvent::MoveTargets(spot));
                return;
            }
            MovePlace::Unmovable(reason) => {
                self.show_note(caret, reason, cx);
                return;
            }
            MovePlace::File => {}
        }
        let Some(provider) = self
            .lsp
            .as_ref()
            .and_then(|lsp| lsp.support.move_destinations())
        else {
            return;
        };
        let kind = MoveKind::File;
        let replies = ask(
            provider,
            MoveQuery {
                spot: spot.clone(),
                kind,
            },
        );
        cx.spawn(async move |this, cx| {
            let answer = first(replies).await;
            this.update(cx, |this, cx| match answer {
                Some((_, Confidence::Unknown)) => this.show_note(caret, NO_ANSWER, cx),
                Some((destinations, _)) if destinations.is_empty() => {
                    this.show_note(caret, "No package to move this to", cx)
                }
                Some((destinations, _)) => {
                    this.open_move_field(caret, spot, kind, destinations, None, cx)
                }
                None => {}
            })
            .ok();
        })
        .detach();
    }

    fn open_move_field(
        &mut self,
        caret: usize,
        spot: DocumentSpot,
        kind: MoveKind,
        destinations: Vec<MoveDestination>,
        note: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        let Some(lsp) = &mut self.lsp else {
            return;
        };
        lsp.completion = None;
        lsp.actions = None;
        lsp.rename = None;
        lsp.note = note.map(|text| crate::semantic::Note {
            at: caret,
            text: text.to_owned().into(),
        });
        lsp.move_to = Some(MoveField::new(caret, spot, kind, destinations));
        cx.notify();
    }

    /// Typed text goes to an open move field. Returns whether it took the text.
    pub(crate) fn move_typed(&mut self, text: &str, cx: &mut Context<Self>) -> bool {
        let Some(field) = self.lsp.as_mut().and_then(|lsp| lsp.move_to.as_mut()) else {
            return false;
        };
        field.text.push_str(text.trim_end_matches(['\r', '\n']));
        field.filter();
        cx.notify();
        true
    }

    /// Keys of an open move field. Returns whether the key was taken.
    pub(crate) fn move_key(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
        let Some(lsp) = &mut self.lsp else {
            return false;
        };
        let Some(field) = &mut lsp.move_to else {
            return false;
        };
        let rows = field.rows();
        match key {
            "escape" => lsp.move_to = None,
            "up" if rows > 0 => field.selected = (field.selected + rows - 1) % rows,
            "down" if rows > 0 => field.selected = (field.selected + 1) % rows,
            "backspace" => {
                field.text.pop();
                field.filter();
            }
            "enter" => {
                let chosen = field.chosen().cloned();
                let (spot, kind) = (field.spot.clone(), field.kind);
                lsp.move_to = None;
                if let Some(destination) = chosen {
                    self.ask_move(spot, kind, destination, cx);
                }
            }
            // A typed character goes through text input, where `move_typed` appends it.
            // Every other key stays in the field, so the text is not edited under it.
            _ => return !is_typed_key(key),
        }
        cx.notify();
        true
    }

    fn ask_move(
        &mut self,
        spot: DocumentSpot,
        kind: MoveKind,
        destination: MoveDestination,
        cx: &mut Context<Self>,
    ) {
        let Some(provider) = self.lsp.as_ref().and_then(|lsp| lsp.support.move_file()) else {
            return;
        };
        let version = spot.version;
        let caret = self
            .state
            .as_ref()
            .map_or(0, |state| state.selections().primary().head);
        let query = MoveRequest {
            spot,
            kind,
            destination,
            update_references: true,
        };
        let replies = ask(provider, query);
        cx.spawn(async move |this, cx| {
            let answer = first(replies).await;
            this.update(cx, |this, cx| match answer {
                Some((_, Confidence::Unknown)) => this.show_note(caret, NO_ANSWER, cx),
                Some((outcome, _)) if outcome.error.is_some() => {
                    let error = outcome.error.unwrap_or_default();
                    this.show_note(caret, error, cx);
                }
                Some((outcome, _)) if !outcome.change.is_empty() => {
                    this.apply_change_labeled(version, outcome.change, Some("Move"), cx);
                }
                Some(_) => this.show_note(caret, "Cannot move this", cx),
                None => {}
            })
            .ok();
        })
        .detach();
    }

    /// F6 to a folder, from a drag in the project tree: picks the one destination of the
    /// server that is that folder and moves the file there with its references. When no
    /// single destination matches, it opens the package picker instead of guessing.
    /// `None` means the file has no language with a move; the caller moves it as it is.
    pub fn move_to_folder(
        &mut self,
        folder: &Path,
        cx: &mut Context<Self>,
    ) -> Option<UnboundedReceiver<FolderMove>> {
        let (spot, caret) = self.spot()?;
        let provider = self.lsp.as_ref()?.support.move_destinations()?;
        let is_class = self.lsp.is_some();
        let folder = folder.to_path_buf();
        let kind = MoveKind::File;
        let replies = ask(
            provider,
            MoveQuery {
                spot: spot.clone(),
                kind,
            },
        );
        let (sender, receiver) = futures_channel::mpsc::unbounded();
        cx.spawn(async move |this, cx| {
            let answer = first(replies).await;
            let done = this
                .update(cx, |this, cx| match answer {
                    Some((_, Confidence::Unknown)) => {
                        this.show_note(caret, NO_ANSWER, cx);
                        FolderMove::Handled
                    }
                    Some((destinations, _)) => {
                        let matches = destinations_for(&destinations, &folder);
                        match matches.len() {
                            1 => {
                                if let Some(destination) = matches.into_iter().next() {
                                    this.ask_move(spot, kind, destination, cx);
                                }
                                FolderMove::Handled
                            }
                            0 if is_class
                                && !destinations.is_empty()
                                && !under_source_root(&destinations, &folder) =>
                            {
                                FolderMove::AskPlain
                            }
                            0 => {
                                let note = "No package matches that folder; choose one";
                                this.open_move_field(
                                    caret,
                                    spot,
                                    kind,
                                    destinations,
                                    Some(note),
                                    cx,
                                );
                                FolderMove::Handled
                            }
                            many => {
                                let note = format!("{many} packages match that folder; choose one");
                                this.open_move_field(
                                    caret,
                                    spot,
                                    kind,
                                    destinations,
                                    Some(&note),
                                    cx,
                                );
                                FolderMove::Handled
                            }
                        }
                    }
                    None => FolderMove::Handled,
                })
                .ok();
            if let Some(done) = done {
                let _ = sender.unbounded_send(done);
            }
        })
        .detach();
        Some(receiver)
    }

    /// The key of a refactoring of the editor as the menus write it: the binding of the
    /// open keymap first, the IDEA file for an action the window binds itself, F5.
    fn hinted(&self, cx: &App, action: &str, idea: &str) -> String {
        let key = self
            .current_keymap(cx)
            .shortcuts_for(action)
            .first()
            .map(ToString::to_string)
            .or_else(|| keymap::idea_shortcut(idea));
        key.map_or_else(String::new, |key| {
            keymap::shortcut_label(&key, keymap::platform())
        })
    }

    /// Runs a refactoring picked by name in Refactor This, as its key would.
    pub(crate) fn run_own(&mut self, own: Own, cx: &mut Context<Self>) {
        match own {
            Own::Rename => self.start_rename(cx),
            Own::ChangeSignature => self.change_signature(cx),
            Own::Move => self.start_move(cx),
            Own::Copy => cx.emit(EditorEvent::CopyElement),
            Own::SafeDelete => self.safe_delete(cx),
            Own::ExtractMethod => self.refactor(Refactoring::ExtractMethod, cx),
            Own::ExtractVariable => self.refactor(Refactoring::ExtractVariable, cx),
            Own::ExtractField => self.refactor(Refactoring::ExtractField, cx),
            Own::ExtractConstant => self.refactor(Refactoring::ExtractConstant, cx),
            Own::IntroduceParameter => self.refactor(Refactoring::IntroduceParameter, cx),
            Own::PullMembersUp => self.member_refactoring(lang_api::MemberRefactoring::PullUp, cx),
            Own::PushMembersDown => {
                self.member_refactoring(lang_api::MemberRefactoring::PushDown, cx)
            }
            Own::ExtractSuperclass => {
                self.member_refactoring(lang_api::MemberRefactoring::ExtractSuperclass, cx)
            }
            Own::IntroduceParameterObject => {
                self.member_refactoring(lang_api::MemberRefactoring::IntroduceParameterObject, cx)
            }
            Own::Inline => self.refactor(Refactoring::Inline, cx),
        }
    }

    /// Ctrl+Alt+Shift+T: every refactoring the server offers at the selection, then the
    /// ones of the editor that the server did not name, each with its key.
    pub(crate) fn refactor_this(&mut self, cx: &mut Context<Self>) {
        let Some(span) = self.selection_span() else {
            return;
        };
        let only = Some(ActionKind::Refactor);
        // The keys of the editor's own rows, read before the answer arrives.
        let own = [
            (
                Own::Rename,
                "Rename",
                editor_actions::RENAME,
                "RenameElement",
                None,
            ),
            (
                Own::ChangeSignature,
                "Change Signature",
                editor_actions::CHANGE_SIGNATURE,
                "ChangeSignature",
                Some("refactor.change.signature"),
            ),
            (Own::Move, "Move", editor_actions::MOVE, "Move", None),
            (
                Own::Copy,
                "Copy",
                "workspace.copy_element",
                "CopyElement",
                None,
            ),
            (
                Own::SafeDelete,
                "Safe Delete",
                editor_actions::SAFE_DELETE,
                "SafeDelete",
                None,
            ),
            (
                Own::ExtractMethod,
                "Extract Method",
                editor_actions::EXTRACT_METHOD,
                "ExtractMethod",
                Some("refactor.extract.function"),
            ),
            (
                Own::ExtractVariable,
                "Extract Variable",
                editor_actions::EXTRACT_VARIABLE,
                "IntroduceVariable",
                Some("refactor.extract.variable"),
            ),
            (
                Own::ExtractField,
                "Extract Field",
                editor_actions::EXTRACT_FIELD,
                "IntroduceField",
                Some("refactor.extract.field"),
            ),
            (
                Own::ExtractConstant,
                "Extract Constant",
                editor_actions::EXTRACT_CONSTANT,
                "IntroduceConstant",
                Some("refactor.extract.constant"),
            ),
            (
                Own::IntroduceParameter,
                "Introduce Parameter",
                editor_actions::INTRODUCE_PARAMETER,
                "IntroduceParameter",
                Some("refactor.introduce.parameter"),
            ),
            (
                Own::PullMembersUp,
                "Pull Members Up",
                editor_actions::PULL_MEMBERS_UP,
                "PullUp",
                Some("refactor.pull.up"),
            ),
            (
                Own::PushMembersDown,
                "Push Members Down",
                editor_actions::PUSH_MEMBERS_DOWN,
                "PushDown",
                Some("refactor.push.down"),
            ),
            (
                Own::ExtractSuperclass,
                "Extract Superclass",
                editor_actions::EXTRACT_SUPERCLASS,
                "ExtractSuperclass",
                Some("refactor.extract.superclass"),
            ),
            (
                Own::IntroduceParameterObject,
                "Introduce Parameter Object",
                editor_actions::INTRODUCE_PARAMETER_OBJECT,
                "IntroduceParameterObject",
                Some("refactor.introduce.parameter.object"),
            ),
            (
                Own::Inline,
                "Inline",
                editor_actions::INLINE,
                "Inline",
                Some("refactor.inline"),
            ),
        ];
        let hints: Vec<String> = own
            .iter()
            .map(|(_, _, action, idea, _)| self.hinted(cx, action, idea))
            .collect();
        self.ask_actions(span, only, cx, move |this, _, caret, entries, cx| {
            let mut codes = Vec::new();
            let mut rows: Vec<Pick> = Vec::new();
            for entry in entries {
                let code = entry.code.clone().unwrap_or_default();
                codes.push(code.clone());
                // A move of the server needs a destination of its own; the editor's
                // own Move row below runs F6, which moves the file.
                if code.starts_with("refactor.move") {
                    continue;
                }
                if code.starts_with("refactor.change.signature") {
                    rows.push(Pick::ChangeSignature { label: entry.title });
                    continue;
                }
                if let Some(handle) = entry.handle {
                    rows.push(Pick::Form {
                        label: entry.title,
                        handle,
                    });
                    continue;
                }
                if !entry.edits.is_empty() || !entry.renames.is_empty() {
                    rows.push(Pick::Action(entry));
                }
            }
            // The editor's own refactorings come after the contextual ones of the
            // server, and only when the server named no action of that kind.
            for ((own, label, _, _, code), hint) in own.into_iter().zip(hints) {
                if code.is_some_and(|code| codes.iter().any(|known| known.starts_with(code))) {
                    continue;
                }
                rows.push(Pick::Refactor {
                    label: label.to_owned(),
                    hint,
                    own,
                });
            }
            this.show_picks(caret, rows, "No refactorings here", cx);
        });
    }

    /// Ctrl+Alt+M, V, F, C, P and Ctrl+Alt+N: the action of that kind for the selection.
    /// An extract with no selection first asks the server which expressions are under
    /// the caret, as IDEA lists them.
    pub(crate) fn refactor(&mut self, refactoring: Refactoring, cx: &mut Context<Self>) {
        let Some(span) = self.selection_span() else {
            return;
        };
        let kind = match refactoring {
            Refactoring::ExtractMethod => Some(ExtractKind::Method),
            Refactoring::ExtractVariable => Some(ExtractKind::Variable),
            Refactoring::ExtractConstant => Some(ExtractKind::Constant),
            Refactoring::ExtractField => Some(ExtractKind::Field),
            Refactoring::IntroduceParameter | Refactoring::Inline => None,
        };
        let empty = span.start == span.end;
        let provider = self
            .lsp
            .as_ref()
            .and_then(|lsp| lsp.support.refactor_selections());
        let (Some(kind), true, Some(provider)) = (kind, empty, provider) else {
            self.refactor_span(refactoring, span, cx);
            return;
        };
        let Some((spot, caret)) = self.spot() else {
            return;
        };
        let replies = ask(provider, SelectionQuery { spot, kind });
        cx.spawn(async move |this, cx| {
            let answer = first(replies).await;
            this.update(cx, |this, cx| {
                let Some((entries, _)) = answer else {
                    this.refactor_span(refactoring, span, cx);
                    return;
                };
                let Some(state) = &this.state else {
                    return;
                };
                let text = state.text();
                let found: Vec<(String, TextSpan)> = entries
                    .into_iter()
                    .filter(|entry| entry.length > 0)
                    .filter_map(|entry| {
                        let start = byte_at_utf16(&text, entry.offset)?;
                        let end = byte_at_utf16(&text, entry.offset + entry.length)?;
                        Some((
                            entry.name,
                            TextSpan {
                                start: point_at(&state.store(), start),
                                end: point_at(&state.store(), end),
                            },
                        ))
                    })
                    .collect();
                match found.as_slice() {
                    [] => this.refactor_span(refactoring, span, cx),
                    [(_, found)] => this.refactor_span(refactoring, *found, cx),
                    _ => {
                        let rows = found
                            .into_iter()
                            .map(|(label, span)| Pick::Expression {
                                label,
                                span,
                                refactoring,
                            })
                            .collect();
                        this.show_picks(caret, rows, refactoring.missing(), cx);
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// The action of the refactoring for the span, which an extract with a choice of
    /// expressions takes from the row the user picked.
    pub(crate) fn refactor_span(
        &mut self,
        refactoring: Refactoring,
        span: TextSpan,
        cx: &mut Context<Self>,
    ) {
        let only = Some(ActionKind::Refactor);
        self.ask_actions(span, only, cx, move |this, version, caret, entries, cx| {
            let mut found: Vec<ActionEntry> = entries
                .into_iter()
                .filter(|entry| {
                    entry
                        .code
                        .as_deref()
                        .is_some_and(|code| code.starts_with(refactoring.code()))
                })
                .collect();
            match found.len() {
                0 => this.show_note(caret, refactoring.missing(), cx),
                1 => {
                    let entry = found.remove(0);
                    if let Some(handle) = entry.handle {
                        cx.emit(EditorEvent::RefactorForm(handle));
                        return;
                    }
                    this.apply_entry_labeled(version, entry, Some(refactoring.label()), cx);
                }
                _ => {
                    // An extract of a value has two rows of the server, this place and
                    // every place: they say what they replace before they run.
                    let replacing = refactoring == Refactoring::ExtractVariable
                        && found.iter().all(|entry| {
                            entry
                                .code
                                .as_deref()
                                .is_some_and(|code| code.starts_with("refactor.extract.variable"))
                        });
                    let mut all = false;
                    let rows = found
                        .into_iter()
                        .filter_map(|entry| {
                            let mut entry = entry;
                            if replacing {
                                let every = entry.title.to_lowercase().contains("all");
                                if every && all {
                                    return None;
                                }
                                all |= every;
                                entry.title = if every {
                                    // The declaration the extract inserts is a place
                                    // without text, not an occurrence it replaces.
                                    let places: usize = entry
                                        .edits
                                        .iter()
                                        .flat_map(|source| &source.edits)
                                        .filter(|edit| edit.span.start != edit.span.end)
                                        .count();
                                    format!("Replace all {places} occurrences")
                                } else {
                                    "Replace this occurrence only".to_owned()
                                };
                            }
                            match entry.handle {
                                Some(handle) => Some(Pick::Form {
                                    label: entry.title,
                                    handle,
                                }),
                                None if !entry.edits.is_empty() || !entry.renames.is_empty() => {
                                    Some(Pick::Action(entry))
                                }
                                None => None,
                            }
                        })
                        .collect();
                    this.show_picks(caret, rows, refactoring.missing(), cx);
                }
            }
        });
    }

    /// Ctrl+Shift+P: the first line of the documentation's code, the declared type.
    pub(crate) fn expression_type(&mut self, cx: &mut Context<Self>) {
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
                Some((_, Confidence::Unknown)) => this.show_note(caret, NO_ANSWER, cx),
                Some((text, _)) => {
                    let line = text
                        .0
                        .lines()
                        .map(str::trim)
                        .find(|line| !line.is_empty() && !line.starts_with("```"))
                        .map(str::to_owned);
                    let shown = line.unwrap_or_else(|| "No type here".to_owned());
                    this.show_note(caret, shown, cx);
                }
                None => {}
            })
            .ok();
        })
        .detach();
    }

    /// Ctrl+H: the window shows the type hierarchy of the type at the caret.
    pub(crate) fn type_hierarchy(&mut self, cx: &mut Context<Self>) {
        let Some((spot, caret)) = self.spot() else {
            return;
        };
        if self
            .lsp
            .as_ref()
            .and_then(|lsp| lsp.support.type_hierarchy())
            .is_none()
        {
            return;
        }
        self.importing_note(caret, cx);
        cx.emit(EditorEvent::TypeHierarchy(spot));
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use lang_api::MoveDestination;

    use super::{destinations_for, under_source_root};

    fn destination(name: &str, detail: &str, folder: &Path) -> MoveDestination {
        MoveDestination {
            name: name.to_owned(),
            detail: Some(detail.to_owned()),
            folder: Some(folder.to_path_buf()),
            handle: "handle".to_owned(),
        }
    }

    #[cfg(windows)]
    fn names(found: &[MoveDestination]) -> Vec<&str> {
        found
            .iter()
            .map(|destination| destination.name.as_str())
            .collect()
    }

    #[test]
    fn petclinic_package_folder_is_found_by_its_full_package() {
        let folder = Path::new(
            r"C:\corpus\petclinic\src\main\source\org\springframework\samples\petclinic\owner",
        );
        let owner = destination(
            "org.springframework.samples.petclinic.owner",
            "spring-petclinic  /spring-petclinic/src/main/source/org/springframework/samples/petclinic/owner",
            folder,
        );

        assert_eq!(
            destinations_for(std::slice::from_ref(&owner), folder),
            vec![owner]
        );
    }

    #[test]
    fn a_package_of_two_modules_takes_the_folder_of_its_own_module() {
        let common = Path::new(r"C:\work\apollo\apollo-common\src\main\source\com\x\util");
        let biz = Path::new(r"C:\work\apollo\apollo-biz\src\main\source\com\x\util");
        let in_common = destination(
            "com.x.util",
            "apollo-common  /apollo-common/src/main/source/com/x/util",
            common,
        );
        let in_biz = destination(
            "com.x.util",
            "apollo-biz  /apollo-biz/src/main/source/com/x/util",
            biz,
        );
        let all = [in_common.clone(), in_biz.clone()];

        assert_eq!(destinations_for(&all, common), vec![in_common]);
        assert_eq!(destinations_for(&all, biz), vec![in_biz]);
    }

    #[test]
    fn a_one_word_package_of_another_module_is_not_this_folder() {
        let folder = Path::new(r"C:\work\apollo\apollo-common\src\main\source\util");
        let other = destination(
            "util",
            "apollo-biz  /apollo-biz/src/main/source/util",
            Path::new(r"C:\work\apollo\apollo-biz\src\main\source\util"),
        );

        assert!(destinations_for(std::slice::from_ref(&other), folder).is_empty());
        assert!(!under_source_root(&[other], folder));
    }

    #[test]
    fn a_folder_that_is_not_a_package_matches_no_destination() {
        let owner = destination(
            "org.springframework.samples.petclinic.owner",
            "spring-petclinic  /spring-petclinic/src/main/source/org/springframework/samples/petclinic/owner",
            Path::new(
                r"C:\corpus\petclinic\src\main\source\org\springframework\samples\petclinic\owner",
            ),
        );
        let docs = Path::new(r"C:\corpus\petclinic\docs");

        assert!(destinations_for(std::slice::from_ref(&owner), docs).is_empty());
        assert!(!under_source_root(&[owner], docs));
    }

    #[cfg(windows)]
    #[test]
    fn a_new_package_folder_lies_under_a_source_root() {
        let owner = destination(
            "org.springframework.samples.petclinic.owner",
            "spring-petclinic  /spring-petclinic/src/main/source/org/springframework/samples/petclinic/owner",
            Path::new(
                r"C:\corpus\petclinic\src\main\source\org\springframework\samples\petclinic\owner",
            ),
        );
        let new_package = Path::new(
            r"C:\corpus\petclinic\src\main\source\org\springframework\samples\petclinic\newpkg",
        );

        assert!(destinations_for(std::slice::from_ref(&owner), new_package).is_empty());
        assert!(under_source_root(&[owner], new_package));
    }

    #[cfg(windows)]
    #[test]
    fn the_detail_alone_finds_the_folder_of_the_server_notation() {
        let folder = Path::new(r"C:\work\shop\src\main\source\com\shop\api");
        let by_path = MoveDestination {
            name: "com.shop.api".to_owned(),
            detail: Some("shop  /shop/src/main/source/com/shop/api".to_owned()),
            folder: None,
            handle: "handle".to_owned(),
        };

        assert_eq!(
            names(&destinations_for(&[by_path], folder)),
            vec!["com.shop.api"]
        );
    }
}
