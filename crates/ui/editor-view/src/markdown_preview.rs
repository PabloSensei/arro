//! The preview of a markdown file beside its text. A tab of a file with the `.md` or
//! `.markdown` extension shows the editor, the rendered document, or both; the document
//! is parsed off the window thread after a pause and follows the scroll of the editor by
//! the block that starts on its top line.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;

use crate::quick_doc::{
    DocBlock, DocHost, DocLinkHandler, DocRenderer, display_text, parse_with_lines, plain_inlines,
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
/// when it closed.
#[derive(Default)]
struct PreviewModes(HashMap<PathBuf, PreviewMode>);

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
    /// The version of the text the blocks are made of.
    parsed: Option<u64>,
    /// The version of the parse on its way, so the text is asked for once.
    pending: Option<u64>,
}

impl MarkdownPreview {
    fn new(mode: PreviewMode) -> Self {
        Self {
            blocks: Vec::new(),
            lines: Vec::new(),
            target: None,
            scroll: ScrollHandle::new(),
            mode,
            parsed: None,
            pending: None,
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
        let mut renderer = DocRenderer::new(&self.style, &theme, &host);
        let Some(preview) = &self.preview else {
            return editor;
        };
        let mut content = div()
            .id("markdown-preview-scroll")
            .flex()
            .flex_col()
            .w_full()
            .gap(px(SPACE_S))
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
