//! Ctrl+O, Ctrl+I, Ctrl+Alt+H, Ctrl+Shift+B, Ctrl+Shift+I and Ctrl+Alt+Shift+C: the
//! structure of the code around the caret, as the language describes it.

use std::path::{Component, Path, PathBuf};

use lang_api::{
    AddOverridableMethodsRequest, Confidence, DefinitionPreviewQuery, DocumentSpot,
    LanguageSupport, NavigationKind, NavigationQuery, OverridableMethod, OverridableMethodsQuery,
    ReferenceQuery, SourceSpan, ask,
};
use syntax::HighlightSpan;
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;

use crate::actions::Pick;
use crate::quick_doc::highlight_source;
use crate::semantic::{NO_ANSWER, first, uri_path};
use crate::style::EditorStyle;
use crate::view::{EditorEvent, EditorView, FileIcon, Languages};

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

/// The heading of a group of the Override or Implement list: the short name of the
/// declaring type with short arguments, and the package it comes from for the grey tail
/// of the row.
pub(crate) fn owner_heading(owner: &str) -> (String, Option<String>) {
    let owner = owner.trim();
    let raw = owner.split('<').next().unwrap_or(owner).trim();
    let package = raw
        .rsplit_once('.')
        .map(|(package, _)| package.trim())
        .filter(|package| !package.is_empty())
        .map(str::to_owned);
    (lang_api::short_type_name(owner), package)
}

/// Ctrl+O and Ctrl+I: the methods of the type at the caret, filtered by typing. Space
/// marks a method, Ctrl+A marks all of them, Enter adds the marked ones in one step.
pub(crate) struct OverridePopup {
    pub at: usize,
    spot: DocumentSpot,
    pub methods: Vec<OverridableMethod>,
    /// The methods that pass the filter, as indices into `methods`.
    pub shown: Vec<usize>,
    pub selected: usize,
    pub checked: Vec<bool>,
    pub text: String,
    pub only_abstract: bool,
}

impl OverridePopup {
    fn new(
        at: usize,
        spot: DocumentSpot,
        methods: Vec<OverridableMethod>,
        only_abstract: bool,
    ) -> Self {
        let checked = vec![false; methods.len()];
        let mut popup = Self {
            at,
            spot,
            methods,
            shown: Vec::new(),
            selected: 0,
            checked,
            text: String::new(),
            only_abstract,
        };
        popup.filter();
        popup
    }

    fn filter(&mut self) {
        let query = self.text.to_lowercase();
        self.shown = self
            .methods
            .iter()
            .enumerate()
            .filter(|(_, method)| {
                if query.is_empty() {
                    return true;
                }
                let parameters = method.parameters.join(", ");
                format!(
                    "{} {} {} {}",
                    method.owner, method.name, parameters, method.abstract_method
                )
                .to_lowercase()
                .contains(&query)
            })
            .map(|(index, _)| index)
            .collect();
        self.selected = 0;
    }

    fn toggle_selected(&mut self) {
        if let Some(index) = self.shown.get(self.selected).copied() {
            self.checked[index] = !self.checked[index];
            self.selected = (self.selected + 1).min(self.shown.len().saturating_sub(1));
        }
    }

    fn check_all(&mut self) {
        for index in &self.shown {
            self.checked[*index] = true;
        }
    }

    /// The marked methods, or the one under the cursor when none is marked.
    fn chosen(&self) -> Vec<OverridableMethod> {
        let marked: Vec<OverridableMethod> = self
            .checked
            .iter()
            .enumerate()
            .filter(|(_, checked)| **checked)
            .map(|(index, _)| self.methods[index].clone())
            .collect();
        if !marked.is_empty() {
            return marked;
        }
        self.shown
            .get(self.selected)
            .map(|index| self.methods[*index].clone())
            .into_iter()
            .collect()
    }
}

/// The most lines of a definition the popup shows before it scrolls.
const DEFINITION_MAX_LINES: usize = 20;
/// The narrowest the popup gets: the header and a few lines of code fit, as they do in
/// the definition window of the reference IDE.
const DEFINITION_MIN_WIDTH: f32 = 480.;
/// The lowest the code area of the popup gets when the user drags its corner.
const DEFINITION_MIN_HEIGHT: f32 = 32.;
/// The width of one character of the code font of the popup, as a part of its size.
const DEFINITION_CHAR: f32 = 0.6;
/// The grab area of the lower right corner of the popup.
const DEFINITION_GRIP: f32 = 12.;

/// Ctrl+Shift+I: the declaration at the caret, with its syntax colours.
pub(crate) struct DefinitionPopup {
    pub at: usize,
    pub text: String,
    pub spans: Vec<HighlightSpan>,
    target: Option<SourceSpan>,
    pub scroll: ScrollHandle,
    /// The width and the height of the code area, when the user dragged the corner.
    size: Option<(f32, f32)>,
    /// The corner drag under way: the pointer where it began and the size it began at.
    resize: Option<DefinitionResize>,
}

/// A drag of the lower right corner of the definition popup.
#[derive(Clone, Copy)]
struct DefinitionResize {
    from: Point<Pixels>,
    width: f32,
    height: f32,
}

/// The name and the package or folder of a definition, as the header of its popup shows
/// them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DefinitionHeader {
    /// The file name, or the name of the class of a library.
    pub name: String,
    /// The package or the folder, empty when the place has neither.
    pub folder: String,
    /// The path the icon resolver reads, or a made-up one for a class of a library.
    icon_path: PathBuf,
    /// The place is a class of a library, which has no file of the project.
    class: bool,
}

/// The name and the package or folder a definition comes from. A class of a jar or of
/// the JDK has no file, so its header names the class and the package the language gives it.
fn definition_header(
    target: Option<&SourceSpan>,
    library: Option<lang_api::LibraryPlace>,
    root: Option<&Path>,
    support: Option<&dyn LanguageSupport>,
) -> DefinitionHeader {
    let Some(target) = target else {
        return DefinitionHeader {
            name: "Definition".to_owned(),
            folder: String::new(),
            icon_path: PathBuf::from("Definition"),
            class: false,
        };
    };
    if let Some(place) = library {
        return DefinitionHeader {
            icon_path: PathBuf::from(format!("{}.class", place.name)),
            name: place.name,
            folder: place.container,
            class: place.library,
        };
    }
    if let Some(path) = uri_path(&target.uri) {
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        let folder = definition_folder(&path, root, support);
        return DefinitionHeader {
            name,
            folder,
            icon_path: path,
            class: false,
        };
    }
    let name = target
        .uri
        .rsplit('/')
        .next()
        .unwrap_or(&target.uri)
        .to_owned();
    DefinitionHeader {
        icon_path: PathBuf::from(&name),
        name,
        folder: String::new(),
        class: false,
    }
}

/// The folder of a file: from the root of the project, or, for a file outside it, the
/// package the language gives its folder, as a class of a library shows one. The header
/// never shows the disk.
fn definition_folder(
    path: &Path,
    root: Option<&Path>,
    support: Option<&dyn LanguageSupport>,
) -> String {
    let folder = path.parent().unwrap_or(path);
    if let Some(root) = root
        && let Some(relative) = relative_to(folder, root)
    {
        return path_text(&relative);
    }
    if let Some(package) = support.and_then(|support| support.package_of(folder)) {
        let package = package.trim();
        // A package has no part of a disk in it; a language that answers with a folder
        // falls through to the folders below, which never show one either.
        if !package.is_empty() && !package.contains(['/', '\\', ':']) {
            return package.to_owned();
        }
    }
    let parts: Vec<String> = folder
        .components()
        .filter_map(|part| match part {
            Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    parts.join(".")
}

/// `path` below `root`. A `file:` URI spells the drive letter in upper case, while the
/// root of the launch may keep the one it was opened with, so Windows compares the two
/// without case.
fn relative_to(path: &Path, root: &Path) -> Option<PathBuf> {
    if let Ok(relative) = path.strip_prefix(root) {
        return Some(relative.to_path_buf());
    }
    if !cfg!(windows) {
        return None;
    }
    let parts: Vec<Component<'_>> = path.components().collect();
    let count = root.components().count();
    if parts.len() < count {
        return None;
    }
    let same = parts.iter().zip(root.components()).all(|(part, root)| {
        part.as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case(&root.as_os_str().to_string_lossy())
    });
    same.then(|| parts[count..].iter().collect())
}

/// A path with the separators of the window, never the ones of the disk.
fn path_text(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// `text` cut in the middle with an ellipsis, when it is longer than `max` characters.
fn shorten_middle(text: &str, max: usize) -> String {
    let length = text.chars().count();
    if length <= max {
        return text.to_owned();
    }
    if max == 0 {
        return "…".to_owned();
    }
    let keep = max - 1;
    let front = keep / 3;
    let back = keep - front;
    let mut shortened: String = text.chars().take(front).collect();
    shortened.push('…');
    shortened.extend(text.chars().skip(length - back));
    shortened
}

/// The icon of the header: the one the application gives the file, or a generic one.
fn definition_icon(header: &DefinitionHeader, cx: &App) -> &'static str {
    cx.try_global::<FileIcon>().map_or_else(
        || {
            if header.class {
                "nodes/class"
            } else {
                "fileTypes/text"
            }
        },
        |icons| (icons.0)(&header.icon_path),
    )
}

/// The width of the header without its folder: the icon, the file name and the button.
fn definition_header_width(header: &DefinitionHeader, style: &EditorStyle) -> f32 {
    let name = header.name.chars().count() as f32 * style.font_size * DEFINITION_CHAR;
    let edit = "Edit Source".chars().count() as f32 * UI_SIZE * DEFINITION_CHAR + SPACE_S * 2.;
    SPACE_S * 2. + ICON + SPACE_XS * 3. + name + edit
}

/// How many characters of the folder fit in the header of `width`.
fn definition_folder_chars(header_width: f32, width: f32) -> usize {
    ((width - header_width - SPACE_M) / (UI_SIZE * DEFINITION_CHAR)).floor() as usize
}

/// One run of a row of the definition popup: its text and its colour.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DefinitionRun {
    pub text: String,
    pub color: Hsla,
}

/// One row of the definition popup: the line of the file it shows and its coloured runs.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DefinitionRow {
    pub number: usize,
    pub runs: Vec<DefinitionRun>,
}

/// What the definition popup shows, for a test.
#[cfg(any(test, feature = "test-support"))]
#[derive(Clone, Debug, PartialEq)]
pub struct DefinitionView {
    /// The file name, or the name of the class of a library.
    pub name: String,
    /// The package or the folder of the definition.
    pub folder: String,
    pub rows: Vec<DefinitionRowView>,
}

/// One row of the definition popup, as a test reads it.
#[cfg(any(test, feature = "test-support"))]
#[derive(Clone, Debug, PartialEq)]
pub struct DefinitionRowView {
    /// The line of the file the row shows.
    pub number: usize,
    pub text: String,
    /// The colour of each run of the row, in the order of its text.
    pub colors: Vec<Hsla>,
}

/// The rows of the definition popup: the lines of the text split by the colours of its
/// spans, numbered from the line of the declaration. The indent every line shares is
/// dropped; the rest of the indent stays, and a tab takes the width it has in the editor.
fn definition_rows(
    popup: &DefinitionPopup,
    style: &EditorStyle,
    tab_width: usize,
) -> Vec<DefinitionRow> {
    let text = popup.text.trim_end_matches('\n');
    let first = popup
        .target
        .as_ref()
        .map_or(1, |target| target.span.start.line as usize + 1);
    let indent = common_indent(text);
    let mut rows = Vec::new();
    let mut line_start = 0;
    for (index, line) in text.split('\n').enumerate() {
        let line_end = line_start + line.len();
        let mut runs = Vec::new();
        let mut at = line_start;
        let mut spans: Vec<&HighlightSpan> = popup
            .spans
            .iter()
            .filter(|span| (span.start as usize) < line_end && (span.end as usize) > line_start)
            .collect();
        spans.sort_by_key(|span| span.start);
        for span in spans {
            let start = (span.start as usize).max(line_start).max(at);
            let end = (span.end as usize).min(line_end);
            if end <= start {
                continue;
            }
            if start > at {
                runs.push(DefinitionRun {
                    text: text[at..start].to_owned(),
                    color: style.code,
                });
            }
            runs.push(DefinitionRun {
                text: text[start..end].to_owned(),
                color: style.color_of(span.kind),
            });
            at = end;
        }
        if at < line_end {
            runs.push(DefinitionRun {
                text: text[at..line_end].to_owned(),
                color: style.code,
            });
        }
        let kept = indent.min(line.len() - line.trim_start_matches([' ', '\t']).len());
        strip_runs(&mut runs, kept);
        expand_tabs(&mut runs, tab_width);
        rows.push(DefinitionRow {
            number: first + index,
            runs,
        });
        line_start = line_end + 1;
    }
    rows
}

/// The prefix of spaces and tabs every non-empty line of `text` starts with.
fn common_indent(text: &str) -> usize {
    let mut common: Option<&str> = None;
    for line in text.split('\n') {
        if line.trim_matches([' ', '\t']).is_empty() {
            continue;
        }
        let length = line.len() - line.trim_start_matches([' ', '\t']).len();
        let indent = &line[..length];
        common = Some(match common {
            None => indent,
            Some(common) => {
                let shared = common
                    .bytes()
                    .zip(indent.bytes())
                    .take_while(|(left, right)| left == right)
                    .count();
                &common[..shared]
            }
        });
    }
    common.map_or(0, str::len)
}

/// Drops `prefix` bytes, the shared indent, from the start of the runs of a row.
fn strip_runs(runs: &mut Vec<DefinitionRun>, mut prefix: usize) {
    while prefix > 0 {
        let Some(first) = runs.first_mut() else {
            return;
        };
        let take = prefix.min(first.text.len());
        first.text.drain(..take);
        prefix -= take;
        if first.text.is_empty() {
            runs.remove(0);
        }
    }
}

/// Replaces a tab with the spaces up to the next stop, as the rows of the editor do.
fn expand_tabs(runs: &mut [DefinitionRun], tab_width: usize) {
    let tab_width = tab_width.max(1);
    let mut column = 0;
    for run in runs {
        if !run.text.contains('\t') {
            column += run.text.chars().count();
            continue;
        }
        let mut text = String::with_capacity(run.text.len());
        for character in run.text.chars() {
            if character == '\t' {
                let spaces = tab_width - column % tab_width;
                text.extend(std::iter::repeat_n(' ', spaces));
                column += spaces;
            } else {
                text.push(character);
                column += 1;
            }
        }
        run.text = text;
    }
}

/// The language of a definition: the language of its file, or the language of the editor
/// for a class of a library, which has no file on disk.
fn definition_language(
    target: Option<&SourceSpan>,
    view: &EditorView,
    cx: &App,
) -> Option<syntax::LanguageConfig> {
    let by_path = target
        .and_then(|target| uri_path(&target.uri))
        .and_then(|path| {
            cx.try_global::<Languages>()
                .and_then(|languages| (languages.resolve)(&path))
        });
    by_path.or_else(|| {
        view.state
            .as_ref()
            .and_then(|state| state.language().cloned())
    })
}

impl EditorView {
    /// Ctrl+O, and Ctrl+I with `only_abstract`.
    pub(crate) fn override_methods(&mut self, only_abstract: bool, cx: &mut Context<Self>) {
        self.ask_overrides(only_abstract, false, cx);
    }

    /// Implement Methods of the Generate list. With nothing to implement, the status bar says
    /// so instead of a note at the caret.
    pub(crate) fn implement_methods_of_generate(&mut self, cx: &mut Context<Self>) {
        self.ask_overrides(true, true, cx);
    }

    /// Asks for the methods that can be overridden or implemented at the caret. With
    /// `from_generate` an empty answer goes to the status bar.
    fn ask_overrides(&mut self, only_abstract: bool, from_generate: bool, cx: &mut Context<Self>) {
        let Some((spot, caret)) = self.spot() else {
            return;
        };
        let Some(provider) = self
            .lsp
            .as_ref()
            .and_then(|lsp| lsp.support.overridable_methods())
        else {
            return;
        };
        let replies = ask(
            provider,
            OverridableMethodsQuery {
                spot: spot.clone(),
                only_abstract,
            },
        );
        cx.spawn(async move |this, cx| {
            let answer = first(replies).await;
            this.update(cx, |this, cx| match answer {
                Some((_, Confidence::Unknown)) => this.show_note(caret, NO_ANSWER, cx),
                Some((methods, _)) if methods.methods.is_empty() && from_generate => {
                    this.report_status("No methods to implement", cx);
                }
                Some((methods, _)) if methods.methods.is_empty() => {
                    let text = if only_abstract {
                        "Nothing to implement here"
                    } else {
                        "Nothing to override here"
                    };
                    this.show_note(caret, text, cx);
                }
                Some((methods, _)) => {
                    let methods = methods.methods;
                    if let Some(lsp) = &mut this.lsp {
                        lsp.completion = None;
                        lsp.actions = None;
                        lsp.note = None;
                        lsp.move_to = None;
                        lsp.rename = None;
                        lsp.overrides =
                            Some(OverridePopup::new(caret, spot, methods, only_abstract));
                    }
                    cx.notify();
                }
                None => {}
            })
            .ok();
        })
        .detach();
    }

    /// Typed text goes to an open Override or Implement popup. Returns whether it did.
    pub(crate) fn override_typed(&mut self, text: &str, cx: &mut Context<Self>) -> bool {
        let Some(popup) = self.lsp.as_mut().and_then(|lsp| lsp.overrides.as_mut()) else {
            return false;
        };
        popup.text.push_str(text.trim_end_matches(['\r', '\n']));
        popup.filter();
        cx.notify();
        true
    }

    /// Keys of an open Override or Implement popup. Returns whether the key was taken.
    pub(crate) fn override_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        let key = event.keystroke.key.to_lowercase();
        let modifiers = event.keystroke.modifiers;
        let Some(lsp) = &mut self.lsp else {
            return false;
        };
        let Some(popup) = &mut lsp.overrides else {
            return false;
        };
        let rows = popup.shown.len();
        match key.as_str() {
            "escape" => lsp.overrides = None,
            "up" if rows > 0 => popup.selected = (popup.selected + rows - 1) % rows,
            "down" if rows > 0 => popup.selected = (popup.selected + 1) % rows,
            "space" if !modifiers.control && !modifiers.alt => popup.toggle_selected(),
            "a" if modifiers.control && !modifiers.alt => popup.check_all(),
            "backspace" => {
                popup.text.pop();
                popup.filter();
            }
            "enter" => {
                let chosen = popup.chosen();
                let spot = popup.spot.clone();
                lsp.overrides = None;
                if !chosen.is_empty() {
                    self.add_overrides(spot, chosen, cx);
                }
            }
            other => {
                if is_typed_key(other) && !modifiers.control && !modifiers.alt {
                    return false;
                }
                return true;
            }
        }
        cx.notify();
        true
    }

    fn add_overrides(
        &mut self,
        spot: DocumentSpot,
        methods: Vec<OverridableMethod>,
        cx: &mut Context<Self>,
    ) {
        let Some(provider) = self
            .lsp
            .as_ref()
            .and_then(|lsp| lsp.support.add_overridable_methods())
        else {
            return;
        };
        let version = spot.version;
        let replies = ask(provider, AddOverridableMethodsRequest { spot, methods });
        cx.spawn(async move |this, cx| {
            let answer = first(replies).await;
            this.update(cx, |this, cx| match answer {
                Some((_, Confidence::Unknown)) => {
                    let caret = this.spot().map_or(0, |(_, caret)| caret);
                    this.show_note(caret, NO_ANSWER, cx);
                }
                Some((change, _)) => this.apply_change(version, change, cx),
                None => {}
            })
            .ok();
        })
        .detach();
    }

    /// Ctrl+Shift+B: the declared type with every place it can be declared.
    pub(crate) fn type_declaration(&mut self, cx: &mut Context<Self>) {
        let Some((spot, caret)) = self.spot() else {
            return;
        };
        let Some(provider) = self.lsp.as_ref().and_then(|lsp| lsp.support.navigation()) else {
            return;
        };
        let replies = ask(
            provider,
            NavigationQuery {
                kind: NavigationKind::TypeDefinition,
                spot,
            },
        );
        cx.spawn(async move |this, cx| {
            let answer = first(replies).await;
            this.update(cx, |this, cx| match answer {
                Some((_, Confidence::Unknown)) => this.show_note(caret, NO_ANSWER, cx),
                Some((spans, _)) if spans.is_empty() => {
                    this.show_note(caret, "Cannot find the type declaration", cx)
                }
                Some((spans, _)) if spans.len() == 1 => this.go_to_location(&spans[0], cx),
                Some((spans, _)) => {
                    let rows = spans
                        .into_iter()
                        .map(|target| Pick::Place {
                            label: crate::refactor::place_label(&target, None),
                            target,
                        })
                        .collect();
                    this.show_picks(caret, rows, "Cannot find the type declaration", cx);
                }
                None => {}
            })
            .ok();
        })
        .detach();
    }

    /// Ctrl+Shift+I: the source of the definition at the caret.
    pub(crate) fn quick_definition(&mut self, cx: &mut Context<Self>) {
        let Some((spot, caret)) = self.spot() else {
            return;
        };
        let Some(provider) = self
            .lsp
            .as_ref()
            .and_then(|lsp| lsp.support.definition_preview())
        else {
            return;
        };
        let syntax = self.syntax.clone();
        let replies = ask(provider, DefinitionPreviewQuery(spot));
        self.importing_note(caret, cx);
        cx.spawn(async move |this, cx| {
            let answer = first(replies).await;
            let opened = this.update(cx, |this, cx| match answer {
                Some((_, Confidence::Unknown)) => {
                    this.show_unanswered(caret, false, cx);
                    None
                }
                Some((preview, _)) if preview.text.trim().is_empty() => {
                    this.show_note(caret, "No definition found", cx);
                    None
                }
                Some((preview, _)) => {
                    let text = preview.text;
                    let target = preview.target;
                    let language = definition_language(target.as_ref(), this, cx);
                    if let Some(lsp) = &mut this.lsp {
                        lsp.note = None;
                        lsp.overrides = None;
                        lsp.definition = Some(DefinitionPopup {
                            at: caret,
                            text: text.clone(),
                            spans: Vec::new(),
                            target,
                            scroll: ScrollHandle::new(),
                            size: None,
                            resize: None,
                        });
                    }
                    cx.notify();
                    Some((text, language))
                }
                None => None,
            });
            let Ok(Some((text, Some(language)))) = opened else {
                return;
            };
            let Some(spans) = highlight_source(syntax, language, text.clone()).await else {
                return;
            };
            this.update(cx, |this, cx| {
                if let Some(popup) = this.lsp.as_mut().and_then(|lsp| lsp.definition.as_mut())
                    && popup.text == text
                {
                    popup.spans = spans;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Escape closes Ctrl+Shift+I, Enter and F4 go to the declaration.
    pub(crate) fn definition_key(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
        let Some(lsp) = &mut self.lsp else {
            return false;
        };
        let Some(popup) = lsp.definition.take() else {
            return false;
        };
        match key {
            "escape" => {}
            "enter" | "f4" => {
                if let Some(target) = popup.target {
                    self.go_to_location(&target, cx);
                }
            }
            _ => {
                lsp.definition = Some(popup);
                return false;
            }
        }
        cx.notify();
        true
    }

    /// The lower right corner drag of the definition popup: it begins at the pointer and
    /// the size the popup is drawn at.
    fn start_definition_resize(
        &mut self,
        position: Point<Pixels>,
        width: f32,
        height: f32,
        cx: &mut Context<Self>,
    ) {
        if let Some(popup) = self.lsp.as_mut().and_then(|lsp| lsp.definition.as_mut()) {
            popup.resize = Some(DefinitionResize {
                from: position,
                width,
                height,
            });
        }
        cx.notify();
    }

    /// Follows the pointer of a corner drag. A move that reports no button held ends it,
    /// so a missed release cannot stick.
    pub(crate) fn drag_definition_resize(
        &mut self,
        position: Point<Pixels>,
        held: bool,
        cx: &mut Context<Self>,
    ) {
        let (max_width, max_height) = self.definition_resize_limits();
        let Some(popup) = self.lsp.as_mut().and_then(|lsp| lsp.definition.as_mut()) else {
            return;
        };
        let Some(start) = popup.resize else {
            return;
        };
        if !held {
            popup.resize = None;
            cx.notify();
            return;
        }
        let dx = f32::from(position.x) - f32::from(start.from.x);
        let dy = f32::from(position.y) - f32::from(start.from.y);
        popup.size = Some((
            (start.width + dx).clamp(DEFINITION_MIN_WIDTH, max_width),
            (start.height + dy).clamp(DEFINITION_MIN_HEIGHT, max_height),
        ));
        cx.notify();
    }

    /// Ends the corner drag.
    pub(crate) fn end_definition_resize(&mut self, cx: &mut Context<Self>) {
        if let Some(popup) = self.lsp.as_mut().and_then(|lsp| lsp.definition.as_mut()) {
            popup.resize = None;
        }
        cx.notify();
    }

    /// The largest size the definition popup takes: the editor's.
    fn definition_resize_limits(&self) -> (f32, f32) {
        self.layout.as_ref().map_or((720., 600.), |layout| {
            (
                (f32::from(layout.bounds.size.width) - SPACE_M * 2.).max(DEFINITION_MIN_WIDTH),
                (f32::from(layout.bounds.size.height) - CONTROL_HEIGHT - SPACE_M * 2.)
                    .max(DEFINITION_MIN_HEIGHT),
            )
        })
    }

    /// The popup over the window while the corner of the definition popup is dragged.
    pub(crate) fn definition_drag_catcher(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.lsp
            .as_ref()
            .and_then(|lsp| lsp.definition.as_ref())
            .and_then(|popup| popup.resize)?;
        Some(
            div()
                .id("quick-definition-drag-catcher")
                .debug_selector(|| "quick-definition-drag-catcher".to_owned())
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .occlude()
                .cursor_nwse_resize()
                .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                    this.drag_definition_resize(event.position, event.dragging(), cx);
                }))
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseUpEvent, _, cx| this.end_definition_resize(cx)),
                )
                .on_mouse_up_out(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseUpEvent, _, cx| this.end_definition_resize(cx)),
                )
                .into_any_element(),
        )
    }

    /// The Ctrl+Shift+I popup: the header of the file of the definition and the source
    /// with the colours and the line numbers of the editor.
    pub(crate) fn render_definition(
        &self,
        popup: &DefinitionPopup,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let theme = *cx.global::<Theme>();
        let style = &self.style;
        let rows = definition_rows(popup, style, self.tab_width());
        let library = popup
            .target
            .as_ref()
            .and_then(|target| self.library_place(&target.uri, cx));
        let header = definition_header(
            popup.target.as_ref(),
            library,
            self.project_root(),
            self.lsp.as_ref().map(|lsp| lsp.support.as_ref()),
        );
        let icon = definition_icon(&header, cx);
        let (max_width, max_height) = self.definition_resize_limits();
        let char_width = style.font_size * DEFINITION_CHAR;
        let digits = rows.last().map_or(1, |row| row.number.to_string().len());
        let number_width = digits as f32 * char_width;
        let gutter = number_width + SPACE_M;
        let widest = rows
            .iter()
            .map(|row| row.runs.iter().map(|run| run.text.chars().count()).sum())
            .max()
            .unwrap_or(0);
        let header_width = definition_header_width(&header, style);
        let natural = (widest as f32 * char_width + gutter + SPACE_M * 2. + SPACE_S)
            .max(header_width + SPACE_M);
        let width = popup
            .size
            .map_or(natural, |size| size.0)
            .clamp(DEFINITION_MIN_WIDTH, max_width);
        let folder = shorten_middle(&header.folder, definition_folder_chars(header_width, width));
        let cap = popup
            .size
            .map_or(DEFINITION_MAX_LINES as f32 * style.line_height, |size| {
                size.1
            })
            .min(max_height);
        let code_height = (rows.len() as f32 * style.line_height).min(cap);
        let frame_height = CONTROL_HEIGHT + code_height + SPACE_XS;

        let mut body_rows = div()
            .id("quick-definition-rows")
            .debug_selector(|| "quick-definition-rows".to_owned())
            .flex()
            .flex_col();
        for (index, row) in rows.iter().enumerate() {
            let mut runs = Vec::new();
            for run in &row.runs {
                if run.text.is_empty() {
                    continue;
                }
                runs.push(
                    div()
                        .flex_none()
                        .text_color(run.color)
                        .child(run.text.clone())
                        .into_any_element(),
                );
            }
            body_rows = body_rows.child(
                div()
                    .flex()
                    .items_start()
                    .h(px(style.line_height))
                    .whitespace_nowrap()
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .justify_end()
                            .w(px(number_width))
                            .mr(px(SPACE_M))
                            .text_color(style.line_number)
                            .debug_selector(move || format!("quick-definition-number-{index}"))
                            .child(row.number.to_string()),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .debug_selector(move || format!("quick-definition-code-{index}"))
                            .children(runs),
                    ),
            );
        }
        let body = div()
            .id("quick-definition-scroll")
            .debug_selector(|| "quick-definition-scroll".to_owned())
            .flex()
            .flex_col()
            .w_full()
            .max_h(px(cap))
            .overflow_y_scroll()
            .track_scroll(&popup.scroll)
            .child(body_rows);
        let body = scrollable("quick-definition", &popup.scroll, body, &theme, window, cx)
            .flex_none()
            .min_h_0();

        let edit = div()
            .id("quick-definition-edit")
            .debug_selector(|| "quick-definition-edit".to_owned())
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .h(px(CONTROL_HEIGHT - SPACE_XS))
            .px(px(SPACE_S))
            .rounded(px(CONTROL_RADIUS))
            .text_size(px(UI_SIZE))
            .cursor_pointer()
            .hover(move |style| style.bg(theme.hover))
            .on_click(cx.listener(|this, _, _, cx| {
                this.definition_key("enter", cx);
            }))
            .tooltip(move |_, cx| tooltip("Edit Source", Some("F4".into()), cx))
            .tooltip_show_delay(TOOLTIP_CREATE_DELAY)
            .child("Edit Source");
        let header = div()
            .debug_selector(|| "quick-definition-header".to_owned())
            .flex()
            .flex_none()
            .items_center()
            .gap(px(SPACE_XS))
            .h(px(CONTROL_HEIGHT))
            .px(px(SPACE_S))
            .border_b_1()
            .border_color(theme.border)
            .child(icon_sized(icon, 16., &theme))
            .child(
                div()
                    .flex_none()
                    .text_color(theme.text)
                    .child(header.name.clone()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(UI_SIZE))
                    .text_color(theme.text_muted)
                    .child(folder),
            )
            .child(edit);
        let grip = div()
            .id("quick-definition-resize")
            .debug_selector(|| "quick-definition-resize".to_owned())
            .absolute()
            .bottom_0()
            .right_0()
            .w(px(DEFINITION_GRIP))
            .h(px(DEFINITION_GRIP))
            .occlude()
            .cursor_nwse_resize()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    this.start_definition_resize(event.position, width, cap, cx);
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                this.drag_definition_resize(event.position, event.dragging(), cx);
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, cx| this.end_definition_resize(cx)),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, cx| this.end_definition_resize(cx)),
            );
        let frame = surface(&theme)
            .id("quick-definition-frame")
            .debug_selector(|| "quick-definition".to_owned())
            .occlude()
            .relative()
            .w(px(width))
            .max_h(px(CONTROL_HEIGHT + cap + SPACE_XS))
            .flex()
            .flex_col()
            .font_family(style.font_family.clone())
            .text_size(px(style.font_size))
            .child(header)
            .child(body)
            .child(grip)
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                this.drag_definition_resize(event.position, event.dragging(), cx);
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, cx| this.end_definition_resize(cx)),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, cx| this.end_definition_resize(cx)),
            );

        let (position, anchor) = self.popup_position(popup.at, frame_height)?;
        Some(
            deferred(
                anchored()
                    .anchor(anchor)
                    .position(position)
                    .snap_to_window_with_margin(px(SPACE_M))
                    .child(frame),
            )
            .with_priority(1)
            .into_any_element(),
        )
    }

    /// What the Ctrl+Shift+I popup shows, for a test: the name and the folder of the
    /// definition, and the text and colours of every row.
    #[cfg(any(test, feature = "test-support"))]
    pub fn definition_view(&self) -> Option<DefinitionView> {
        let popup = self.lsp.as_ref()?.definition.as_ref()?;
        let library = popup
            .target
            .as_ref()
            .and_then(|target| self.lsp.as_ref()?.support.describe_library_uri(&target.uri));
        let header = definition_header(
            popup.target.as_ref(),
            library,
            self.project_root(),
            self.lsp.as_ref().map(|lsp| lsp.support.as_ref()),
        );
        let rows = definition_rows(popup, &self.style, self.tab_width())
            .into_iter()
            .map(|row| DefinitionRowView {
                number: row.number,
                text: row.runs.iter().map(|run| run.text.as_str()).collect(),
                colors: row.runs.iter().map(|run| run.color).collect(),
            })
            .collect();
        Some(DefinitionView {
            name: header.name,
            folder: header.folder,
            rows,
        })
    }

    /// Ctrl+Alt+Shift+C: the reference to the symbol at the caret.
    pub(crate) fn copy_reference(&mut self, cx: &mut Context<Self>) {
        let Some((spot, caret)) = self.spot() else {
            return;
        };
        let Some(provider) = self
            .lsp
            .as_ref()
            .and_then(|lsp| lsp.support.reference_string())
        else {
            return;
        };
        let replies = ask(provider, ReferenceQuery(spot));
        cx.spawn(async move |this, cx| {
            let answer = first(replies).await;
            this.update(cx, |this, cx| match answer {
                Some((_, Confidence::Unknown)) => this.show_note(caret, NO_ANSWER, cx),
                Some((reference, _)) if reference.is_empty() => {
                    this.show_note(caret, "Nothing to copy here", cx)
                }
                Some((reference, _)) => {
                    cx.write_to_clipboard(ClipboardItem::new_string(reference));
                }
                None => {}
            })
            .ok();
        })
        .detach();
    }

    /// Ctrl+Shift+H: the window shows where the method is defined, inherited or still
    /// awaited by an abstract type.
    pub(crate) fn method_hierarchy(&mut self, cx: &mut Context<Self>) {
        let Some((spot, _)) = self.spot() else {
            return;
        };
        if self
            .lsp
            .as_ref()
            .and_then(|lsp| lsp.support.method_hierarchy())
            .is_none()
        {
            return;
        }
        cx.emit(EditorEvent::MethodHierarchy(spot));
    }

    /// Ctrl+Alt+H: the window shows the call hierarchy of the place.
    pub(crate) fn call_hierarchy(&mut self, cx: &mut Context<Self>) {
        let Some((spot, caret)) = self.spot() else {
            return;
        };
        if self
            .lsp
            .as_ref()
            .and_then(|lsp| lsp.support.call_hierarchy())
            .is_none()
        {
            return;
        }
        self.importing_note(caret, cx);
        cx.emit(EditorEvent::CallHierarchy(spot));
    }

    /// Ctrl+Alt+Shift+U: the window shows the diagram of the type at the caret.
    pub(crate) fn show_diagram(&mut self, cx: &mut Context<Self>) {
        let Some((spot, _)) = self.spot() else {
            return;
        };
        if self
            .lsp
            .as_ref()
            .and_then(|lsp| lsp.support.type_graph())
            .is_none()
        {
            return;
        }
        cx.emit(EditorEvent::ShowDiagram(spot));
    }

    /// The window shows the sequence diagram of the method at the caret.
    pub(crate) fn show_sequence_diagram(&mut self, cx: &mut Context<Self>) {
        let Some((spot, _)) = self.spot() else {
            return;
        };
        if self
            .lsp
            .as_ref()
            .and_then(|lsp| lsp.support.sequence_diagram())
            .is_none()
        {
            return;
        }
        cx.emit(EditorEvent::ShowSequenceDiagram(spot));
    }
}
