//! M5b on top of the server: usages, rename, extract and inline, Generate, the type
//! under the caret, the super method and the type hierarchy.

use std::ops::Range;
use std::path::{Component, Path};

use futures_channel::mpsc::UnboundedReceiver;
use futures_util::StreamExt;
use lang_api::{
    ActionEntry, ActionKind, Confidence, DocumentSpot, HierarchyDirection, HierarchyQuery,
    MoveDestination, MoveKind, MoveQuery, MoveRequest, RenameQuery, SourceSpan, SymbolEntry,
    WorkspaceChange, ask,
};
use text_core::TextStore;
use ui_kit::*;

use crate::actions::{Own, Pick};
use crate::semantic::{MAX_SHOWN, NO_ANSWER, first, is_word, offset_at, uri_path};
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

/// What a refactoring key asks the server for: a prefix of the kind of its action.
#[derive(Clone, Copy)]
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

fn symbol_row(symbol: SymbolEntry, arrow: &str) -> Option<Pick> {
    let target = symbol.place?;
    let detail = format!("{arrow} {}", symbol.name);
    Some(Pick::Place {
        label: place_label(&target, Some(&detail)),
        target,
    })
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
        self.usage_search(false, cx);
    }

    /// Ctrl+Alt+F7: the uses of the symbol, in the popup at the caret.
    pub(crate) fn show_usages(&mut self, cx: &mut Context<Self>) {
        self.usage_search(true, cx);
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

    pub(crate) fn usage_search(&mut self, popup: bool, cx: &mut Context<Self>) {
        let Some((spot, caret)) = self.spot() else {
            return;
        };
        let Some(support) = self.lsp.as_ref().map(|lsp| lsp.support.clone()) else {
            return;
        };
        let name = self.word_at_caret(caret);
        if !popup {
            let detailed = support.usages();
            let references = support.references();
            if let Some(provider) = detailed {
                let replies = ask(provider, spot);
                let support = support.clone();
                cx.spawn(async move |this, cx| {
                    let answer = first(replies).await;
                    this.update(cx, |this, cx| match answer {
                        Some((_, Confidence::Unknown)) => this.show_note(caret, NO_ANSWER, cx),
                        Some((entries, _)) => {
                            let usages = detailed_usages_of(&name, entries, &*support);
                            cx.emit(EditorEvent::UsagesDetailed(usages));
                        }
                        None => {}
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
            cx.spawn(async move |this, cx| {
                let answer = first(replies).await;
                this.update(cx, |this, cx| match answer {
                    Some((_, Confidence::Unknown)) => this.show_note(caret, NO_ANSWER, cx),
                    Some((spans, _)) => {
                        this.set_usage_ranges(&spans);
                        cx.emit(EditorEvent::Usages(usages_of(&name, spans)));
                    }
                    None => {}
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

    /// Shift+F6 opens the field with the word under the caret.
    pub(crate) fn start_rename(&mut self, cx: &mut Context<Self>) {
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
            self.show_note(caret, "Put the caret on a name to rename", cx);
            return;
        }
        let text = store.slice(start..end);
        if let Some(lsp) = &mut self.lsp {
            lsp.completion = None;
            lsp.actions = None;
            lsp.note = None;
            lsp.move_to = None;
            lsp.rename = Some(RenameField {
                at: start,
                spot,
                text,
                preview: None,
                summary: None,
            });
            cx.notify();
        }
    }

    /// Typed text goes to an open rename field. Returns whether it took the text.
    pub(crate) fn rename_typed(&mut self, text: &str, cx: &mut Context<Self>) -> bool {
        let Some(field) = self.lsp.as_mut().and_then(|lsp| lsp.rename.as_mut()) else {
            return false;
        };
        field.text.push_str(text.trim_end_matches(['\r', '\n']));
        field.preview = None;
        field.summary = None;
        cx.notify();
        true
    }

    /// Keys of an open rename field. Returns whether the key was taken.
    pub(crate) fn rename_key(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
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
                    self.apply_change(version, change, cx);
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

    /// Ctrl+F6: the window shows the signature of the place and edits it.
    pub(crate) fn change_signature(&mut self, cx: &mut Context<Self>) {
        let Some((spot, _)) = self.spot() else {
            return;
        };
        cx.emit(EditorEvent::ChangeSignature(spot));
    }

    /// F6: asks where the file can go and shows the destinations in a searchable list.
    pub(crate) fn start_move(&mut self, cx: &mut Context<Self>) {
        let Some((spot, caret)) = self.spot() else {
            return;
        };
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
                    this.apply_change(version, outcome.change, cx);
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
    pub(crate) fn refactor(&mut self, refactoring: Refactoring, cx: &mut Context<Self>) {
        let Some(span) = self.selection_span() else {
            return;
        };
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
                    let change = WorkspaceChange {
                        edits: entry.edits,
                        renames: entry.renames,
                    };
                    this.apply_change(version, change, cx);
                }
                _ => {
                    // Some of the rows ask for values first, in a form of the window.
                    let rows = found
                        .into_iter()
                        .filter_map(|entry| match entry.handle {
                            Some(handle) => Some(Pick::Form {
                                label: entry.title,
                                handle,
                            }),
                            None if !entry.edits.is_empty() || !entry.renames.is_empty() => {
                                Some(Pick::Action(entry))
                            }
                            None => None,
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

    /// Ctrl+H: supertypes and subtypes of the type at the caret.
    pub(crate) fn type_hierarchy(&mut self, cx: &mut Context<Self>) {
        let Some((spot, caret)) = self.spot() else {
            return;
        };
        let Some(provider) = self.lsp.as_ref().and_then(|lsp| lsp.support.hierarchy()) else {
            return;
        };
        let query = |direction| HierarchyQuery {
            spot: spot.clone(),
            direction,
        };
        let up = ask(provider, query(HierarchyDirection::Supertypes));
        let down = ask(provider, query(HierarchyDirection::Subtypes));
        cx.spawn(async move |this, cx| {
            let (up, down) = (first(up).await, first(down).await);
            this.update(cx, |this, cx| {
                let unknown = [&up, &down]
                    .iter()
                    .any(|answer| matches!(answer, Some((_, Confidence::Unknown))));
                if unknown {
                    this.show_note(caret, NO_ANSWER, cx);
                    return;
                }
                let up = up.map(|(list, _)| list).unwrap_or_default();
                let down = down.map(|(list, _)| list).unwrap_or_default();
                let rows = up
                    .into_iter()
                    .filter_map(|symbol| symbol_row(symbol, "↑"))
                    .chain(
                        down.into_iter()
                            .filter_map(|symbol| symbol_row(symbol, "↓")),
                    )
                    .collect();
                this.show_picks(caret, rows, "No type hierarchy here", cx);
            })
            .ok();
        })
        .detach();
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
