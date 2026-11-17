//! The preview of a markdown file beside its text. A tab of a file with the `.md` or
//! `.markdown` extension shows the editor, the rendered document, or both; the document
//! is parsed off the window thread after a pause and follows the scroll of the editor by
//! the block that starts on its top line.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use syntax::{HighlightSpan, LanguageConfig};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;

use crate::quick_doc::{
    DocBlock, DocHost, DocImage, DocInline, DocLinkHandler, DocPicture, DocRenderer,
    DocToggleHandler, display_text, highlight_source, image_path, parse_with_lines, plain_inlines,
    split_line_fragment,
};
use crate::view::EditorView;

/// The pause after the last key before the text is parsed again.
const PREVIEW_DELAY: Duration = Duration::from_millis(120);
/// The name of the blocks and links of the preview in debug selectors.
const PREVIEW: &str = "markdown-preview";

/// What a markdown tab shows: the text, the rendered document, or both.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PreviewMode {
    EditorOnly,
    #[default]
    EditorAndPreview,
    PreviewOnly,
}

impl PreviewMode {
    fn shows_editor(self) -> bool {
        matches!(self, Self::EditorOnly | Self::EditorAndPreview)
    }

    fn shows_preview(self) -> bool {
        matches!(self, Self::EditorAndPreview | Self::PreviewOnly)
    }
}

/// The mode picked for each file of the session: a tab opened again shows what it showed
/// when it closed. The saved layout keeps the same choice for a project opened again.
#[derive(Default)]
pub struct PreviewModes(HashMap<PathBuf, PreviewMode>);

impl Global for PreviewModes {}

/// Whether the file is a markdown document with a preview.
fn is_markdown(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("md") || extension.eq_ignore_ascii_case("markdown")
        })
}

/// The preview of one tab: the blocks of its text and the state of its panel.
pub(crate) struct MarkdownPreview {
    pub(crate) blocks: Vec<DocBlock>,
    /// The line each block starts on, from zero.
    lines: Vec<usize>,
    /// The block the panel was scrolled to, so a still editor does not move it again.
    target: Option<usize>,
    pub(crate) scroll: ScrollHandle,
    pub(crate) mode: PreviewMode,
    /// The pictures of the document by their address, once the tab has found them.
    pub(crate) pictures: HashMap<String, DocPicture>,
    /// The version of the text the blocks are made of.
    parsed: Option<u64>,
    /// The version of the parse on its way, so the text is asked for once.
    pending: Option<u64>,
    /// The `<details>` blocks the reader has turned away from their default state, by id.
    /// The parse keeps no state of its own, so the choice survives every new parse.
    toggled_details: HashSet<usize>,
}

impl MarkdownPreview {
    fn new(mode: PreviewMode) -> Self {
        Self {
            blocks: Vec::new(),
            lines: Vec::new(),
            target: None,
            scroll: ScrollHandle::new(),
            mode,
            pictures: HashMap::new(),
            parsed: None,
            pending: None,
            toggled_details: HashSet::new(),
        }
    }

    /// The preview of `path`, `None` for a file that has none.
    pub(crate) fn state_for(path: &Path, cx: &App) -> Option<Self> {
        if !is_markdown(path) {
            return None;
        }
        let mode = cx
            .try_global::<PreviewModes>()
            .and_then(|modes| modes.0.get(path).copied())
            .unwrap_or_default();
        Some(Self::new(mode))
    }

    /// The block that covers the line: the last one that starts on it or above.
    fn block_for_line(&self, line: usize) -> Option<usize> {
        let after = self.lines.partition_point(|start| *start <= line);
        let index = after.checked_sub(1)?;
        (index < self.lines.len()).then_some(index)
    }

    /// The block of the heading an `#anchor` names, by the slug of its text.
    fn heading(&self, anchor: &str) -> Option<usize> {
        let wanted = slug(anchor);
        self.blocks.iter().position(|block| match block {
            DocBlock::Heading { text, .. } => slug(&plain_inlines(text)) == wanted,
            _ => false,
        })
    }
}

/// The slug of a heading, the way a markdown anchor names it.
fn slug(text: &str) -> String {
    let mut out = String::new();
    for character in text.chars() {
        if character.is_alphanumeric() || character == '_' || character == '-' {
            out.extend(character.to_lowercase());
        } else if character.is_whitespace() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_owned()
}

/// The pictures of the blocks, in the order the document names them.
fn pictures_of(blocks: &[DocBlock], out: &mut Vec<DocImage>) {
    for block in blocks {
        match block {
            DocBlock::Paragraph(inlines) => pictures_in(inlines, out),
            DocBlock::Heading { text, .. } => pictures_in(text, out),
            DocBlock::List { items, nested, .. } => {
                for item in items {
                    pictures_in(item, out);
                }
                for blocks in nested {
                    pictures_of(blocks, out);
                }
            }
            DocBlock::Table { header, rows, .. } => {
                for cell in header {
                    pictures_in(cell, out);
                }
                for row in rows {
                    for cell in row {
                        pictures_in(cell, out);
                    }
                }
            }
            DocBlock::Details {
                summary, blocks, ..
            } => {
                pictures_in(summary, out);
                pictures_of(blocks, out);
            }
            DocBlock::Image { alt, url } => out.push(DocImage {
                alt: alt.clone(),
                url: url.clone(),
                width: None,
                height: None,
            }),
            DocBlock::Quote(inner) => pictures_of(inner, out),
            DocBlock::Code { .. } | DocBlock::Divider => {}
        }
    }
}

/// Calls `visit` for each code block of the blocks, in the order they are drawn.
fn for_each_code<'a>(blocks: &'a [DocBlock], visit: &mut impl FnMut(&'a DocBlock)) {
    for block in blocks {
        match block {
            DocBlock::Code { .. } => visit(block),
            DocBlock::List { nested, .. } => {
                for blocks in nested {
                    for_each_code(blocks, visit);
                }
            }
            DocBlock::Quote(inner) => for_each_code(inner, visit),
            DocBlock::Details { blocks, .. } => for_each_code(blocks, visit),
            _ => {}
        }
    }
}

/// Puts the colours of each code block in place. The code blocks are counted in the order
/// [`for_each_code`] visits them, and `colours` holds the index of each block it colours.
fn paint_code(blocks: &mut [DocBlock], colours: &[(usize, Vec<HighlightSpan>)], next: &mut usize) {
    for block in blocks {
        match block {
            DocBlock::Code { spans, .. } => {
                if let Some((_, found)) = colours.iter().find(|(index, _)| *index == *next) {
                    *spans = found.clone();
                }
                *next += 1;
            }
            DocBlock::List { nested, .. } => {
                for blocks in nested {
                    paint_code(blocks, colours, next);
                }
            }
            DocBlock::Quote(inner) => paint_code(inner, colours, next),
            DocBlock::Details { blocks, .. } => paint_code(blocks, colours, next),
            _ => {}
        }
    }
}

/// The grammar a name of a fenced block stands for. The markdown language of the file holds
/// the resolver of its fences, the same one the editor uses for the fences of its text.
fn fence_language(markdown: &LanguageConfig, name: &str) -> Option<LanguageConfig> {
    let resolve = markdown
        .injections
        .iter()
        .find_map(|injection| injection.resolve)?;
    resolve(name)
}

/// The pictures of a run of a line, in the order the document names them.
fn pictures_in(inlines: &[DocInline], out: &mut Vec<DocImage>) {
    for inline in inlines {
        match inline {
            DocInline::Image(image) => out.push(image.clone()),
            DocInline::Strong(children) | DocInline::Emphasis(children) => {
                pictures_in(children, out);
            }
            DocInline::Link { text, .. } => pictures_in(text, out),
            DocInline::Text(_) | DocInline::Code(_) => {}
        }
    }
}

impl EditorView {
    /// The mode of the preview of this tab, `None` for a file without one.
    pub fn preview_mode(&self) -> Option<PreviewMode> {
        self.preview.as_ref().map(|preview| preview.mode)
    }

    /// The text the preview draws, for the tests.
    pub fn preview_text(&self) -> Option<String> {
        let preview = self.preview.as_ref()?;
        (!preview.blocks.is_empty()).then(|| display_text(&preview.blocks, None))
    }

    /// The block the panel was last scrolled to, for the tests.
    pub fn preview_block(&self) -> Option<usize> {
        self.preview.as_ref()?.target
    }

    /// The number of colours of each code block of the preview, for the tests.
    pub fn preview_code_spans(&self) -> Vec<usize> {
        let mut counts = Vec::new();
        if let Some(preview) = &self.preview {
            for_each_code(&preview.blocks, &mut |block| {
                if let DocBlock::Code { spans, .. } = block {
                    counts.push(spans.len());
                }
            });
        }
        counts
    }

    /// The folder the preview keeps the pictures it takes from the internet, so that a
    /// second open of the project draws them without asking again.
    pub fn set_picture_cache(&mut self, folder: PathBuf) {
        self.picture_cache = Some(folder);
    }

    /// How far the panel of the preview is scrolled, for the tests.
    pub fn preview_scroll(&self) -> f32 {
        self.preview
            .as_ref()
            .map_or(0., |preview| -f32::from(preview.scroll.offset().y))
    }

    /// Shows the editor, the preview or both, and keeps the choice for the file.
    pub fn set_preview_mode(&mut self, mode: PreviewMode, cx: &mut Context<Self>) {
        let Some(preview) = &mut self.preview else {
            return;
        };
        if preview.mode == mode {
            return;
        }
        preview.mode = mode;
        cx.default_global::<PreviewModes>()
            .0
            .insert(self.path.clone(), mode);
        if mode.shows_preview() {
            self.prepare_preview(cx);
        }
        cx.notify();
    }

    /// Asks for the blocks of the text when the preview shows them and the text moved.
    /// The answer of an older text is dropped in [`Self::accept_preview`].
    pub(crate) fn prepare_preview(&mut self, cx: &mut Context<Self>) {
        let Some(preview) = &self.preview else {
            return;
        };
        if !preview.mode.shows_preview() {
            return;
        }
        let Some(state) = &self.state else {
            return;
        };
        let version = state.version();
        if preview.parsed == Some(version) || preview.pending == Some(version) {
            return;
        }
        // The first parse of an opened file needs no pause: nothing is being typed yet.
        let wait = preview.parsed.is_some();
        self.schedule_preview(wait, cx);
    }

    fn schedule_preview(&mut self, wait: bool, cx: &mut Context<Self>) {
        let Some(text) = self.text() else {
            return;
        };
        let Some(version) = self.state.as_ref().map(crate::state::EditorState::version) else {
            return;
        };
        let Some(preview) = &mut self.preview else {
            return;
        };
        preview.pending = Some(version);
        cx.spawn(async move |this, cx| {
            if wait {
                cx.background_executor().timer(PREVIEW_DELAY).await;
                // A key of the pause moved the text again: the newer parse covers
                // this one, so its text is not read.
                let wanted = this
                    .update(cx, |this, _| {
                        this.preview.as_ref().and_then(|preview| preview.pending) == Some(version)
                    })
                    .unwrap_or(false);
                if !wanted {
                    return;
                }
            }
            let parsed = cx
                .background_executor()
                .spawn(async move { parse_with_lines(&text) })
                .await;
            this.update(cx, |this, cx| this.accept_preview(version, parsed, cx))
                .ok();
        })
        .detach();
    }

    fn accept_preview(
        &mut self,
        version: u64,
        parsed: (Vec<DocBlock>, Vec<usize>),
        cx: &mut Context<Self>,
    ) {
        let Some(preview) = &mut self.preview else {
            return;
        };
        if preview.pending == Some(version) {
            preview.pending = None;
        }
        let current = self.state.as_ref().map(crate::state::EditorState::version);
        if current != Some(version) {
            return;
        }
        let (blocks, lines) = parsed;
        preview.target = preview
            .target
            .map(|index| index.min(blocks.len().saturating_sub(1)));
        preview.blocks = blocks;
        preview.lines = lines;
        preview.parsed = Some(version);
        self.prepare_pictures(cx);
        self.highlight_preview_code(version, cx);
        cx.notify();
    }

    /// Asks the syntax service for the colours of the code blocks of the preview that name a
    /// language. The colours come back to the text they were read from, see
    /// [`Self::accept_colours`].
    fn highlight_preview_code(&mut self, version: u64, cx: &mut Context<Self>) {
        let Some(markdown) = self
            .state
            .as_ref()
            .and_then(|state| state.language())
            .cloned()
        else {
            return;
        };
        let Some(preview) = &self.preview else {
            return;
        };
        let mut code = Vec::new();
        for_each_code(&preview.blocks, &mut |block| {
            if let DocBlock::Code { language, text, .. } = block {
                code.push((language.clone(), text.clone()));
            }
        });
        let jobs: Vec<(usize, LanguageConfig, String)> = code
            .into_iter()
            .enumerate()
            .filter_map(|(index, (name, text))| {
                let language = fence_language(&markdown, name?.as_str())?;
                Some((index, language, text))
            })
            .collect();
        if jobs.is_empty() {
            return;
        }
        let syntax = self.syntax.clone();
        cx.spawn(async move |this, cx| {
            let mut colours = Vec::new();
            for (index, language, text) in jobs {
                if let Some(spans) = highlight_source(syntax.clone(), language, text).await {
                    colours.push((index, spans));
                }
            }
            this.update(cx, |this, cx| this.accept_colours(version, colours, cx))
                .ok();
        })
        .detach();
    }

    /// Puts the colours of the code blocks in place, when the text they were read from is
    /// still the text of the preview: a newer text drops them.
    fn accept_colours(
        &mut self,
        version: u64,
        colours: Vec<(usize, Vec<HighlightSpan>)>,
        cx: &mut Context<Self>,
    ) {
        if self.state.as_ref().map(crate::state::EditorState::version) != Some(version) {
            return;
        }
        let Some(preview) = &mut self.preview else {
            return;
        };
        if preview.parsed != Some(version) {
            return;
        }
        let mut next = 0;
        paint_code(&mut preview.blocks, &colours, &mut next);
        cx.notify();
    }

    /// Finds the pictures the document draws: a file next to it, or the bytes of an
    /// address of the internet from the cache of the state folder or the server. A
    /// picture already found stays as it is.
    fn prepare_pictures(&mut self, cx: &mut Context<Self>) {
        let base = self
            .path
            .parent()
            .map_or_else(PathBuf::new, Path::to_path_buf);
        let internet = self.options.markdown_images_from_internet;
        let cache = self.picture_cache.clone();
        let Some(preview) = &mut self.preview else {
            return;
        };
        if preview.blocks.is_empty() {
            return;
        }
        let mut wanted = Vec::new();
        pictures_of(&preview.blocks, &mut wanted);
        for image in wanted {
            if preview.pictures.contains_key(&image.url) {
                continue;
            }
            if crate::markdown_images::is_remote(&image.url) {
                if !internet {
                    preview.pictures.insert(image.url, DocPicture::Unavailable);
                    continue;
                }
                let url = image.url.clone();
                preview.pictures.insert(image.url, DocPicture::Loading);
                let asked = url.clone();
                let folder = cache.clone();
                let job = cx.background_executor().spawn(async move {
                    let bytes = crate::markdown_images::fetch(&asked, folder.as_deref())?;
                    let format = crate::markdown_images::format_of(&bytes, &asked)
                        .ok_or_else(|| "the address is not a picture".to_owned())?;
                    Ok::<_, String>(Arc::new(Image::from_bytes(format, bytes)))
                });
                cx.spawn(async move |this, cx| {
                    let answer = job.await;
                    this.update(cx, |this, cx| this.picture_arrived(url, answer, cx))
                        .ok();
                })
                .detach();
                continue;
            }
            let picture = match image_path(&base, &image.url) {
                Some(path) if path.is_file() => DocPicture::File(path),
                _ => DocPicture::Unavailable,
            };
            preview.pictures.insert(image.url, picture);
        }
    }

    /// The bytes of a picture of the internet have come, or the reason they did not.
    fn picture_arrived(
        &mut self,
        url: String,
        answer: Result<Arc<Image>, String>,
        cx: &mut Context<Self>,
    ) {
        let internet = self.options.markdown_images_from_internet;
        let Some(preview) = &mut self.preview else {
            return;
        };
        let remote = crate::markdown_images::is_remote(&url);
        let picture = match answer {
            Ok(bytes) if !remote || internet => DocPicture::Bytes(bytes),
            Ok(_) | Err(_) => DocPicture::Unavailable,
        };
        preview.pictures.insert(url, picture);
        cx.notify();
    }

    /// Settings turned the pictures of the internet on or off: the addresses of a
    /// server are asked for again, or dropped back to their text.
    pub(crate) fn pictures_from_internet(&mut self, cx: &mut Context<Self>) {
        let Some(preview) = &mut self.preview else {
            return;
        };
        preview
            .pictures
            .retain(|url, _| !crate::markdown_images::is_remote(url));
        self.prepare_pictures(cx);
        cx.notify();
    }

    /// Opens a `<details>` block of the preview that is closed, or closes one that is open.
    /// The choice stays while the file is open.
    pub(crate) fn toggle_details(&mut self, id: usize, cx: &mut Context<Self>) {
        let Some(preview) = &mut self.preview else {
            return;
        };
        if !preview.toggled_details.remove(&id) {
            preview.toggled_details.insert(id);
        }
        cx.notify();
    }

    /// Puts the block that starts on the top line of the editor at the top of the panel.
    fn sync_preview(&mut self) {
        let Some(preview) = &mut self.preview else {
            return;
        };
        if !preview.mode.shows_preview() || preview.blocks.is_empty() {
            return;
        }
        let Some(state) = &self.state else {
            return;
        };
        let line_height = self.style.line_height.max(1.);
        let row = (self.scroll_top.max(0.) / line_height) as usize;
        let line = state.display().row_to_line(row);
        let Some(index) = preview.block_for_line(line) else {
            return;
        };
        if preview.target == Some(index) {
            return;
        }
        preview.target = Some(index);
        preview.scroll.scroll_to_top_of_item(index);
    }

    /// The three modes of the preview in the top right corner of the editor, `None` for a
    /// file without a preview.
    pub(crate) fn preview_toolbar(&self, cx: &mut Context<Self>) -> Option<Div> {
        let mode = self.preview.as_ref()?.mode;
        let theme = *cx.global::<Theme>();
        let scripted = cx.has_global::<ScriptSpots>();
        let button = |id: &'static str, icon: &str, label: &'static str, active: bool| {
            let button = icon_button(id, icon, label, None, &theme)
                .relative()
                .debug_selector(move || id.to_owned())
                .child(script_spot(scripted, id));
            if active {
                button.bg(theme.selection)
            } else {
                button
            }
        };
        let editor = button(
            "markdown-mode-editor",
            "general/editorOnly",
            "Editor only",
            mode == PreviewMode::EditorOnly,
        )
        .on_click(cx.listener(|this, _, _, cx| this.set_preview_mode(PreviewMode::EditorOnly, cx)));
        let both = button(
            "markdown-mode-split",
            "general/editorAndPreview",
            "Editor and Preview",
            mode == PreviewMode::EditorAndPreview,
        )
        .on_click(
            cx.listener(|this, _, _, cx| this.set_preview_mode(PreviewMode::EditorAndPreview, cx)),
        );
        let only = button(
            "markdown-mode-preview",
            "general/previewOnly",
            "Preview only",
            mode == PreviewMode::PreviewOnly,
        )
        .on_click(
            cx.listener(|this, _, _, cx| this.set_preview_mode(PreviewMode::PreviewOnly, cx)),
        );
        Some(
            div()
                .flex()
                .items_center()
                .justify_end()
                .flex_none()
                .h(px(CONTROL_HEIGHT))
                .pr(px(SPACE_S))
                .gap(px(SPACE_XS))
                .child(editor)
                .child(both)
                .child(only),
        )
    }

    /// The text of the editor and the preview beside it, or one of them alone.
    pub(crate) fn preview_area(
        &mut self,
        editor: AnyElement,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(mode) = self.preview.as_ref().map(|preview| preview.mode) else {
            return editor;
        };
        if !mode.shows_preview() {
            return editor;
        }
        self.sync_preview();
        let theme = *cx.global::<Theme>();
        let scripted = cx.has_global::<ScriptSpots>();
        let entity = cx.entity();
        let on_link: DocLinkHandler = Rc::new(move |url, _window, cx| {
            entity.update(cx, |this, cx| this.open_preview_link(url, cx));
        });
        let host = DocHost {
            name: PREVIEW.to_owned(),
            on_link,
            on_select: None,
            selection: None,
            base: Some(
                self.path
                    .parent()
                    .map_or_else(PathBuf::new, Path::to_path_buf),
            ),
        };
        let toggle = cx.entity();
        let on_toggle: DocToggleHandler = Rc::new(move |id, _window, cx| {
            toggle.update(cx, |this, cx| this.toggle_details(id, cx));
        });
        let Some(preview) = &self.preview else {
            return editor;
        };
        let mut renderer = DocRenderer::for_document(
            &self.style,
            &theme,
            &host,
            &preview.pictures,
            &preview.toggled_details,
            on_toggle,
        );
        let mut content = div()
            .id("markdown-preview-scroll")
            .flex()
            .flex_col()
            .w_full()
            .gap(px(SPACE_L))
            .p(px(SPACE_M))
            .overflow_y_scroll()
            .track_scroll(&preview.scroll);
        // The blocks are the children of the scrolled element, so a bar knows their
        // places and the preview scrolls to one of them by its index.
        for (index, block) in preview.blocks.iter().enumerate() {
            content = content.child(
                div()
                    .id(("markdown-preview-block", index))
                    .debug_selector(move || format!("{PREVIEW}-block-{index}"))
                    .w_full()
                    .child(renderer.block(block, window, cx)),
            );
        }
        let rows = scrollable(PREVIEW, &preview.scroll, content, &theme, window, cx)
            .flex_1()
            .w_full()
            .min_w_0()
            .min_h_0();
        let pane = div()
            .id("markdown-preview-pane")
            .debug_selector(|| format!("{PREVIEW}-pane"))
            .relative()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .when(mode == PreviewMode::EditorAndPreview, |this| {
                this.border_l_1().border_color(theme.border)
            })
            .child(rows)
            .child(script_spot(scripted, "markdown-preview-text"));
        let mut split = div().flex().flex_1().min_h_0().w_full();
        if mode.shows_editor() {
            // The cell is a flex row of its own, so the text fills its height: a block
            // cell would give the text the height of its content, and a custom element
            // that asks for the whole height of an auto one gets nothing.
            split = split.child(
                div()
                    .relative()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .h_full()
                    .child(editor)
                    .child(script_spot(scripted, "markdown-editor-text")),
            );
        }
        split.child(pane).into_any_element()
    }

    /// A link of the preview: an `#anchor` scrolls the panel, a file opens in the window,
    /// a web address goes where the other links of the IDE go.
    fn open_preview_link(&mut self, url: String, cx: &mut Context<Self>) {
        if let Some(anchor) = url.strip_prefix('#') {
            if let Some(preview) = &self.preview
                && let Some(index) = preview.heading(anchor)
            {
                preview.scroll.scroll_to_top_of_item(index);
                cx.notify();
            }
            return;
        }
        if !url.starts_with("http://") && !url.starts_with("https://") {
            let (target, line) = split_line_fragment(&url);
            let relative = !target.is_empty()
                && !target.starts_with("file:")
                && !target.starts_with("jdt:")
                && !target.contains("://");
            if relative
                && let Some(uri) = self
                    .preview_path(&target)
                    .and_then(|path| ::platform::CanonPath::new(path).ok())
                    .map(|path| path.to_uri())
            {
                let uri = if line > 0 {
                    format!("{uri}#L{}", line + 1)
                } else {
                    uri
                };
                self.open_quick_doc_link(uri, cx);
                return;
            }
        }
        self.open_quick_doc_link(url, cx);
    }

    /// The file an address of a link names, counted from the folder of this file.
    fn preview_path(&self, target: &str) -> Option<PathBuf> {
        let target = target.split('#').next().unwrap_or_default();
        if target.is_empty() {
            return None;
        }
        let path = Path::new(target);
        if path.is_absolute() {
            return Some(path.to_path_buf());
        }
        self.path.parent().map(|base| base.join(path))
    }
}

#[cfg(test)]
mod tests {
    use super::{PreviewMode, slug};

    #[test]
    fn a_heading_anchor_is_its_lowercase_slug() {
        assert_eq!(slug("Second section"), "second-section");
        assert_eq!(slug("A `code` name!"), "a-code-name");
        assert_eq!(slug("second-section"), "second-section");
        assert_eq!(slug("  spaced  "), "spaced");
    }

    #[test]
    fn the_modes_show_the_parts_they_name() {
        assert!(PreviewMode::EditorOnly.shows_editor());
        assert!(!PreviewMode::EditorOnly.shows_preview());
        assert!(PreviewMode::EditorAndPreview.shows_editor());
        assert!(PreviewMode::EditorAndPreview.shows_preview());
        assert!(!PreviewMode::PreviewOnly.shows_editor());
        assert!(PreviewMode::PreviewOnly.shows_preview());
    }
}
