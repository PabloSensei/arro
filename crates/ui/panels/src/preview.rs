//! The read-only preview of the file under a list. The text is read off the UI thread
//! and shown by the editor of the window, so the list never waits for the disk; a big
//! file gives its first two megabytes. A match of a search is painted over the row and
//! the view is scrolled to it.

use std::ops::Range;
use std::path::{Path, PathBuf};

use editor_view::{EditorOptions, EditorStyle, EditorView, PaneDecor, Spot};
use serde::{Deserialize, Serialize};
use store::Store;
use text_core::{FileFormat, TextStore};
use ui_kit::prelude::*;
use ui_kit::*;
use workspace_core::{DocumentStore, LoadedFile};

/// The most of one file the preview shows.
const MAX_PREVIEW_BYTES: usize = 2 * 1024 * 1024;

/// The height of the preview under the list of Find in Files, first open and limits.
pub(crate) const DEFAULT_FINDER_PREVIEW: f32 = 240.;
pub(crate) const MIN_FINDER_PREVIEW: f32 = 80.;
pub(crate) const MAX_FINDER_PREVIEW: f32 = 420.;

/// What the lists of the window remember about their previews: the switch of the
/// palettes and the height shared by the list of Find in Files. It lives in the state
/// folder of the project, so a new window with the same folder shows the same previews.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct PreviewState {
    /// The Preview button of the palettes is on.
    pub palette: bool,
    /// The height of the preview of Find in Files.
    pub finder_height: Option<f32>,
}

/// The key of the state of the previews of one project.
fn state_key(root: &Path) -> String {
    format!("preview:{}", root.display())
}

/// What the project remembered, or the defaults.
pub(crate) fn load(store: Option<&Store>, root: &Path) -> PreviewState {
    store
        .and_then(|store| store.get(&state_key(root)).ok().flatten())
        .and_then(|json| serde_json::from_str(&json).ok())
        .unwrap_or_default()
}

/// Remembers the Preview switch of the palettes, keeping the height of the list.
pub(crate) fn save_palette(store: Option<&Store>, root: &Path, on: bool) {
    let mut state = load(store, root);
    state.palette = on;
    write(store, root, state);
}

/// Remembers the height of the preview under the list of Find in Files, keeping the
/// Preview switch of the palettes.
pub(crate) fn save_finder_height(store: Option<&Store>, root: &Path, height: f32) {
    let mut state = load(store, root);
    state.finder_height = Some(height);
    write(store, root, state);
}

fn write(store: Option<&Store>, root: &Path, state: PreviewState) {
    let Some(store) = store else {
        return;
    };
    let Ok(json) = serde_json::to_string(&state) else {
        return;
    };
    if let Err(error) = store.put(&state_key(root), &json) {
        eprintln!("the preview state is not saved: {error}");
    }
}

/// The place a preview opens at, and the match it paints.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PreviewTarget {
    pub path: PathBuf,
    /// Where the view is scrolled before the text shows: the declaration of a symbol,
    /// or the first line of a row that is not a match.
    pub spot: Option<Spot>,
    /// The line of a match, from one, and its bytes in that line.
    pub found: Option<(usize, Range<usize>)>,
}

impl PreviewTarget {
    /// The file at its first line.
    pub(crate) fn file(path: PathBuf) -> Self {
        Self {
            path,
            spot: None,
            found: None,
        }
    }

    /// The declaration of a symbol, at the bytes the language named.
    pub(crate) fn at(path: PathBuf, spot: Spot) -> Self {
        Self {
            path,
            spot: Some(spot),
            found: None,
        }
    }

    /// The row of a search: the line and the bytes of its match in that line.
    pub(crate) fn found(path: PathBuf, line: usize, found: Range<usize>) -> Self {
        Self {
            path,
            spot: None,
            found: Some((line, found)),
        }
    }
}

/// What the preview shows while the text is not on screen yet.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum State {
    /// No row is selected, so there is nothing to show.
    #[default]
    Empty,
    Reading,
    Ready,
    Failed,
}

/// The text of one file, read once, and the view that draws it.
pub(crate) struct FilePreview {
    editor: Option<Entity<EditorView>>,
    path: Option<PathBuf>,
    target: Option<PreviewTarget>,
    text: Option<String>,
    /// The bytes of the match in `text`, painted over its row.
    highlight: Option<Range<usize>>,
    state: State,
    /// The serial of the read in flight, so that a late one is dropped.
    serial: u64,
}

impl Default for FilePreview {
    fn default() -> Self {
        Self {
            editor: None,
            path: None,
            target: None,
            text: None,
            highlight: None,
            state: State::Empty,
            serial: 0,
        }
    }
}

impl FilePreview {
    /// Shows `target`, or nothing. The same file with another place only moves the view;
    /// another file is read again in the background.
    pub(crate) fn show(&mut self, target: Option<PreviewTarget>, cx: &mut Context<Self>) {
        if target == self.target {
            return;
        }
        self.serial += 1;
        let serial = self.serial;
        let Some(target) = target else {
            self.editor = None;
            self.path = None;
            self.target = None;
            self.text = None;
            self.highlight = None;
            self.state = State::Empty;
            cx.notify();
            return;
        };
        if self.path.as_deref() == Some(target.path.as_path())
            && self.text.is_some()
            && self.editor.is_some()
        {
            self.place(&target, cx);
            self.target = Some(target);
            cx.notify();
            return;
        }
        // A row of another file: the old text goes away at once, so that no row of the
        // list shows the file of the row before it.
        self.editor = None;
        self.path = Some(target.path.clone());
        self.text = None;
        self.highlight = None;
        self.target = Some(target.clone());
        self.state = State::Reading;
        // An open tab has the text the user sees, which may be newer than the disk.
        let open = crate::outline::OpenEditors::text(&target.path, cx);
        let path = target.path.clone();
        let job = cx
            .background_executor()
            .spawn(async move { read(&path, open) });
        cx.spawn(async move |this, cx| {
            let read = job.await;
            this.update(cx, |this, cx| this.arrived(serial, read, cx))
                .ok();
        })
        .detach();
        cx.notify();
    }

    /// The file the preview shows, or nothing while the list has no file row.
    #[cfg(test)]
    pub(crate) fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// The text of the match the preview paints, for a test.
    #[cfg(test)]
    pub(crate) fn highlight_text(&self) -> Option<String> {
        let text = self.text.as_deref()?;
        let range = self.highlight.as_ref()?;
        Some(text.get(range.clone())?.to_owned())
    }

    /// Whether the text of the target is on screen.
    #[cfg(test)]
    pub(crate) fn is_ready(&self) -> bool {
        self.state == State::Ready
    }

    /// The editor of the preview, for a test of where it scrolled.
    #[cfg(test)]
    pub(crate) fn editor(&self) -> Option<&Entity<EditorView>> {
        self.editor.as_ref()
    }

    fn arrived(
        &mut self,
        serial: u64,
        (path, loaded): (PathBuf, Option<(String, FileFormat)>),
        cx: &mut Context<Self>,
    ) {
        if serial != self.serial {
            return;
        }
        let Some((text, format)) = loaded else {
            self.state = State::Failed;
            cx.notify();
            return;
        };
        let style = EditorStyle::from_theme(cx.global::<Theme>());
        let shape = text.clone();
        let editor = cx.new(move |cx| {
            let mut view =
                EditorView::open_text_document(path, shape, format, true, false, style, cx);
            let options = EditorOptions {
                // The preview tells the place with the row it highlights; its own crumbs
                // would name the declarations of the file under the list.
                breadcrumbs: false,
                sticky_lines: false,
                highlight_current_line: false,
                inlay_type_hints: false,
                inlay_parameter_names: lang_api::InlayParameterNames::None,
                code_vision_usages: false,
                code_vision_implementations: false,
                render_doc_comments: false,
                ai_features: false,
                ..EditorOptions::default()
            };
            view.set_options(options, cx);
            view
        });
        self.text = Some(text);
        self.editor = Some(editor);
        self.state = State::Ready;
        let target = self.target.clone();
        if let Some(target) = target {
            self.place(&target, cx);
        }
        cx.notify();
    }

    /// Scrolls the view to the place of `target` and works out the match to paint.
    fn place(&mut self, target: &PreviewTarget, cx: &mut Context<Self>) {
        let (Some(text), Some(editor)) = (&self.text, &self.editor) else {
            return;
        };
        let highlight = target.found.as_ref().and_then(|(line, range)| {
            let start = line_start(text, *line)?;
            let start = start + range.start;
            let end = start + range.len();
            (end <= text.len()).then_some(start..end)
        });
        self.highlight = highlight.clone();
        let spot = match (highlight, &target.spot) {
            (Some(found), _) => Spot::Offset(found.start),
            (None, Some(spot)) => *spot,
            (None, None) => Spot::Offset(0),
        };
        editor.update(cx, |editor, cx| editor.go_to(spot, cx));
    }
}

impl Render for FilePreview {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let Some(editor) = &self.editor else {
            let text = match self.state {
                State::Reading => "Reading…",
                State::Failed => "The file could not be read",
                State::Empty | State::Ready => "",
            };
            return div()
                .debug_selector(|| "preview".to_owned())
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(theme.text_secondary)
                .child(text)
                .into_any_element();
        };
        let mut decor = PaneDecor::default();
        if let Some(range) = &self.highlight {
            let style = EditorStyle::from_theme(&theme);
            decor.ranges.push((range.clone(), style.find_current));
        }
        let pane = editor.update(cx, |editor, cx| editor.pane(decor, None, window, cx));
        div()
            .debug_selector(|| "preview".to_owned())
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_hidden()
            .bg(theme.editor_bg)
            .child(pane)
            .into_any_element()
    }
}

/// The text of `path` and its format, or `None` when the file cannot be read. Runs off
/// the UI thread.
fn read(path: &Path, open: Option<String>) -> (PathBuf, Option<(String, FileFormat)>) {
    if let Some(text) = open {
        return (path.to_path_buf(), Some((cut(text), FileFormat::default())));
    }
    let Ok(file) = LoadedFile::read(path) else {
        return (path.to_path_buf(), None);
    };
    let mut store = DocumentStore::new();
    let document = store.open(file);
    let Some(state) = store.state(document) else {
        return (path.to_path_buf(), None);
    };
    let text = cut(TextStore::text(&state.text));
    (path.to_path_buf(), Some((text, state.format.clone())))
}

/// The first two megabytes of `text` at a character boundary.
fn cut(mut text: String) -> String {
    if text.len() > MAX_PREVIEW_BYTES {
        let mut end = MAX_PREVIEW_BYTES;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    text
}

/// The byte offset of the line `line`, counted from one.
fn line_start(text: &str, line: usize) -> Option<usize> {
    if line == 0 {
        return None;
    }
    if line == 1 {
        return Some(0);
    }
    let mut seen = 1;
    for (at, byte) in text.bytes().enumerate() {
        if byte == b'\n' {
            seen += 1;
            if seen == line {
                return Some(at + 1);
            }
        }
    }
    None
}

#[cfg(test)]
mod checks {
    use super::{MAX_PREVIEW_BYTES, cut, line_start};

    #[test]
    fn a_big_file_keeps_its_first_megabytes() {
        let text = "x".repeat(MAX_PREVIEW_BYTES + 10);
        assert_eq!(cut(text).len(), MAX_PREVIEW_BYTES);
        let text = "é".repeat(MAX_PREVIEW_BYTES);
        let cut_text = cut(text);
        assert!(cut_text.len() <= MAX_PREVIEW_BYTES);
        assert!(std::str::from_utf8(cut_text.as_bytes()).is_ok());
    }

    #[test]
    fn a_line_counts_from_one() {
        let text = "one\ntwo\nthree";
        assert_eq!(line_start(text, 1), Some(0));
        assert_eq!(line_start(text, 2), Some(4));
        assert_eq!(line_start(text, 3), Some(8));
        assert_eq!(line_start(text, 4), None);
    }
}
