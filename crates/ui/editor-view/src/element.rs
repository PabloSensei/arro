//! Painting. Only visible rows are shaped. Based on the code view of the S10 spike.

use std::ops::Range;
use std::rc::Rc;

use display_map::{DisplayChunk, DisplayLine};
use lang_api::Severity;
use syntax::{HighlightKind, HighlightSpan};
use text_core::TextStore;
use ui_kit::metrics::UI_FONT;
use ui_kit::*;

use crate::decor::PaneDecor;
use crate::scrollbar::{self, Bar};
use crate::semantic::TokenSpan;
use crate::style::EditorStyle;
use crate::view::{
    EditorView, GutterKind, GutterMark, InlayPaint, OutlineItem, RunMarkState, WidestRow, inside,
};

pub(crate) const GUTTER_WIDTH: f32 = 64.;
/// The width of the marks left of the numbers: a breakpoint, a run mark or a bookmark.
const GUTTER_MARK: f32 = 16.;
/// The gap between the marks, the numbers and the fold button.
const GUTTER_GAP: f32 = 2.;
/// The square of the fold button and the inset after it, before the text.
pub(crate) const FOLD_SIZE: f32 = 16.;
const GUTTER_INSET: f32 = 4.;
pub(crate) const TEXT_INSET: f32 = 3.;
/// Where the badge of a place or a handler is, in the column left of the line numbers.
pub(crate) const ENDPOINT_X: f32 = 0.;
/// The room of the blame column left of the text, when the column is on.
pub(crate) const ANNOTATE_WIDTH: f32 = 180.;
/// The room between the left edge of the blame column and its label.
const ANNOTATE_INSET: f32 = 4.;
/// Where the debug marks of a line are, left of the line numbers: a breakpoint never
/// covers a number.
pub(crate) const RUN_X: f32 = 0.;
const NUMBER_SIZE: f32 = 11.7;

/// The number of digits of the largest line number of a file of `lines` lines.
pub(crate) fn number_digits(lines: usize) -> u32 {
    lines.max(1).ilog10() + 1
}

/// The width of the gutter for a number of `number_width` pixels: the marks, the number,
/// the fold button and their gaps. A short number keeps the smallest width of the
/// column, and a longer one grows it, so that no digit is cut.
pub(crate) fn gutter_width(number_width: f32) -> f32 {
    (GUTTER_MARK + GUTTER_GAP + number_width + GUTTER_GAP + FOLD_SIZE + GUTTER_INSET)
        .max(GUTTER_WIDTH)
}

/// The right edge of the line numbers in a gutter of `gutter` pixels.
pub(crate) fn number_right(gutter: f32) -> f32 {
    gutter - GUTTER_INSET - FOLD_SIZE - GUTTER_GAP
}

/// The left edge of the fold button in a gutter of `gutter` pixels.
pub(crate) fn fold_x(gutter: f32) -> f32 {
    gutter - GUTTER_INSET - FOLD_SIZE
}

/// Rounds the size so that the advance of a monospaced font is a whole device pixel.
fn snapped_size(size: f32, scale_factor: f32) -> f32 {
    (size * 0.6 * scale_factor).round() / 0.6 / scale_factor
}

pub(crate) struct LaidOutRow {
    pub row: usize,
    pub(crate) buffer_line: usize,
    pub y: Pixels,
    pub line: DisplayLine,
    pub text: ShapedLine,
    number: ShapedLine,
    /// The number the gutter shows, when it shows one.
    pub(crate) number_value: Option<usize>,
    /// The hints of the row, shaped at their own smaller size.
    inlays: Vec<InlayText>,
    /// A fold button: `true` when the fold is collapsed.
    fold: Option<bool>,
    /// A run mark takes the place of the line number.
    run: Option<(RunMarkState, Hsla)>,
    /// The mark beside the run button, when the line starts a handler.
    endpoint: Option<Hsla>,
    /// The badge of a place, with its icon and colour, when the line has one.
    place: Option<(SharedString, Hsla)>,
    /// The badge of an action menu, with its icon and colour, when the line has one.
    action: Option<(SharedString, Hsla)>,
    /// A bookmark without a name takes the place of the line number.
    bookmark: bool,
    /// The label of the blame column on this row, on the first line of a run of a commit.
    annotation: Option<ShapedLine>,
    current: bool,
    indent: usize,
    /// The row is a block above the text and holds no line of the buffer.
    pub(crate) is_block: bool,
    /// The row is a rendered documentation comment, not a label.
    pub(crate) is_doc: bool,
    /// Whether the comment of this line is rendered, for its gutter icon.
    pub(crate) doc_icon: Option<bool>,
    /// The comment range of a rendered documentation row.
    pub(crate) doc_range: Option<std::ops::Range<usize>>,
    /// The parts of a block row: each opens its list when clicked.
    pub(crate) block_labels: Vec<BlockLabel>,
    /// The links of a rendered documentation row.
    pub(crate) doc_links: Vec<DocLinkLabel>,
    /// The values of the stopped debugger after the line.
    tail: Option<ShapedLine>,
    /// The inline watch of the line, with its cross.
    pub(crate) watch: Option<LaidOutWatch>,
}

/// One inline watch drawn after a line: its text and the cross that removes it.
pub(crate) struct LaidOutWatch {
    pub expression: String,
    pub text: ShapedLine,
    pub cross: ShapedLine,
    /// The left edges of the text and of the cross, in window coordinates.
    pub text_x: Pixels,
    pub cross_x: Pixels,
}

/// One clickable link of a rendered documentation row.
pub(crate) struct DocLinkLabel {
    pub target: String,
    pub bounds: Bounds<Pixels>,
}

/// One clickable label of a block row.
pub(crate) struct BlockLabel {
    pub kind: lang_api::CodeLensKind,
    pub text: ShapedLine,
    pub bounds: Bounds<Pixels>,
}

/// A hint of a row: the columns it takes in the row text, its own shaped line and where
/// the label and its pill go.
struct InlayText {
    columns: Range<usize>,
    text: ShapedLine,
    /// The height the label is painted at, so that its baseline is the code one.
    height: Pixels,
    /// The pill and the place of the label, `None` until the row has its columns.
    paint: Option<InlayPaint>,
}

/// What a mark on the scrollbar stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StripeKind {
    Error,
    Warning,
    Find,
    Vcs,
    Caret,
}

/// One short mark on the vertical scrollbar.
pub(crate) struct StripeMark {
    pub kind: StripeKind,
    /// The buffer line the mark belongs to, from zero.
    pub line: usize,
    pub bounds: Bounds<Pixels>,
    /// The colour the caller gave the mark, for the VCS stripes.
    pub color: Option<Hsla>,
    /// The text a hint shows, when the mark has one.
    pub message: Option<String>,
}

/// The last painted frame, for hit testing.
pub(crate) struct Layout {
    pub bounds: Bounds<Pixels>,
    pub rows: Vec<LaidOutRow>,
    /// The bars of this frame: `None` when the whole content fits and the bar hides.
    pub vertical: Option<Bar>,
    pub horizontal: Option<Bar>,
    pub vertical_area: Option<Bounds<Pixels>>,
    pub horizontal_area: Option<Bounds<Pixels>>,
    pub stripes: Vec<StripeMark>,
    /// The height of the panel of sticky lines, from zero when there is none.
    pub sticky_height: f32,
    /// The hints of the frame, for a test of their pills and baselines.
    pub inlays: Vec<InlayPaint>,
}

impl Layout {
    /// The bounds of the number of the row of `line`, for a test that checks the column
    /// of the numbers holds it whole.
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn line_number_bounds(
        &self,
        line: usize,
        height: Pixels,
        gutter: f32,
    ) -> Option<Bounds<Pixels>> {
        let row = self.rows.iter().find(|row| {
            !row.is_block
                && !row.line.continuation
                && row.buffer_line == line
                && row.number_value.is_some()
        })?;
        let width = row.number.width();
        let right = self.bounds.left() + px(number_right(gutter));
        Some(Bounds::new(
            point(right - width, row.y),
            size(width, height),
        ))
    }
}

pub(crate) struct EditorElement {
    view: Entity<EditorView>,
    /// The rows and backgrounds of this view alone; plain for the editor of a tab.
    decor: PaneDecor,
}

impl EditorElement {
    pub fn new(view: Entity<EditorView>) -> Self {
        Self {
            view,
            decor: PaneDecor::default(),
        }
    }

    /// The text with the rows and backgrounds of one view, for a view that shows the
    /// text beside another one.
    pub fn with_decor(view: Entity<EditorView>, decor: PaneDecor) -> Self {
        Self { view, decor }
    }
}

/// Thin bars between the numbers and the text for lines that differ from the base.
fn paint_marks(
    rows: &[LaidOutRow],
    marks: &[GutterMark],
    gutter_right: Pixels,
    height: Pixels,
    window: &mut Window,
) {
    for shown in rows {
        if shown.is_block {
            continue;
        }
        let line = shown.buffer_line as u32;
        for mark in marks.iter().filter(|mark| match mark.kind {
            GutterKind::Deleted => mark.lines.start == line,
            _ => mark.lines.contains(&line),
        }) {
            let (x, y, w, h) = match mark.kind {
                GutterKind::Deleted => (gutter_right - px(7.), shown.y, px(6.), px(2.)),
                _ => (gutter_right - px(5.), shown.y, px(3.), height),
            };
            window.paint_quad(fill(Bounds::new(point(x, y), size(w, h)), mark.color));
        }
    }
}

impl IntoElement for EditorElement {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

/// A declaration line pinned to the top of the text.
struct StickyRow {
    line: usize,
    text: ShapedLine,
    number: ShapedLine,
}

/// Declarations pinned at most.
const STICKY_MAX: usize = 4;

/// The room between the last pinned declaration and the first row of the text below it.
const STICKY_GAP: f32 = 4.;

/// The height of the panel of `count` pinned declarations.
pub(crate) fn sticky_height(count: usize, line_height: f32) -> f32 {
    if count == 0 {
        0.
    } else {
        count as f32 * line_height + STICKY_GAP
    }
}

/// The declarations whose first line is above `top_line` while their body still reaches
/// it, outermost first, the innermost `STICKY_MAX` of them.
pub(crate) fn pinned_items(
    outline: &[OutlineItem],
    top_line: usize,
    line_of: impl Fn(usize) -> usize,
) -> Vec<&OutlineItem> {
    let mut pinned: Vec<_> = outline
        .iter()
        .filter(|item| item.kind.is_block())
        .filter(|item| {
            let last = item.range.end.saturating_sub(1).max(item.range.start);
            line_of(item.name_range.start) < top_line && line_of(last) >= top_line
        })
        .collect();
    pinned.sort_by_key(|item| item.range.start);
    let skip = pinned.len().saturating_sub(STICKY_MAX);
    pinned.split_off(skip)
}

pub(crate) struct EditorPrepaint {
    layout: Rc<Layout>,
    sticky: Vec<StickyRow>,
    highlights: Vec<(Bounds<Pixels>, Hsla)>,
    selections: Vec<Bounds<Pixels>>,
    carets: Vec<Bounds<Pixels>>,
    marked: Option<Bounds<Pixels>>,
    /// Diagnostics: a line under the text and a stripe in the gutter.
    underlines: Vec<(Bounds<Pixels>, Hsla)>,
    gutter_marks: Vec<(Bounds<Pixels>, Hsla)>,
    occurrences: Vec<Bounds<Pixels>>,
    /// Backgrounds of whole lines and of byte ranges, under the text and the selection.
    line_tints: Vec<(Bounds<Pixels>, Hsla)>,
    range_tints: Vec<(Bounds<Pixels>, Hsla)>,
    hitbox: Hitbox,
    text_hitbox: Hitbox,
    char_width: Pixels,
    text_x: Pixels,
    /// The width of the gutter that the frame leaves before the text.
    gutter: f32,
    /// The rectangle the text may paint in: the bar and the horizontal bar stay out.
    text_mask: Bounds<Pixels>,
    scroll_top: f32,
    scroll_left: f32,
    scrollbar_hovered: bool,
}

/// The look of a run: its colour, the background behind it and the marks a token,
/// a diagnostic or markup may add.
#[derive(Clone, Copy)]
struct RunLook {
    color: Hsla,
    background: Option<Hsla>,
    italic: bool,
    bold: bool,
    strikethrough: bool,
}

impl RunLook {
    fn plain(color: Hsla) -> Self {
        Self {
            color,
            background: None,
            italic: false,
            bold: false,
            strikethrough: false,
        }
    }
}

fn run(len: usize, font: &Font, color: Hsla, italic: bool) -> TextRun {
    styled_run(
        len,
        font,
        RunLook {
            italic,
            ..RunLook::plain(color)
        },
    )
}

/// A run with the marks a semantic token, a diagnostic or markup may add: italics
/// for a static or abstract member, a weight for bold text, a line through the text
/// of a deprecated or struck one.
fn styled_run(len: usize, font: &Font, look: RunLook) -> TextRun {
    let mut font = font.clone();
    if look.italic {
        font.style = FontStyle::Italic;
    }
    if look.bold {
        font.weight = FontWeight::SEMIBOLD;
    }
    TextRun {
        len,
        font,
        color: look.color,
        background_color: look.background,
        underline: None,
        strikethrough: look.strikethrough.then(|| StrikethroughStyle {
            thickness: px(1.),
            color: Some(look.color),
        }),
    }
}

/// Paints `len` characters with `look`, joining the run before it when the look is
/// the same, so that a line of one colour stays one run.
fn push_run(runs: &mut Vec<TextRun>, len: usize, font: &Font, look: RunLook) {
    if len == 0 {
        return;
    }
    let weight = if look.bold {
        FontWeight::SEMIBOLD
    } else {
        font.weight
    };
    if let Some(last) = runs.last_mut()
        && last.color == look.color
        && last.background_color == look.background
        && last.font.style
            == if look.italic {
                FontStyle::Italic
            } else {
                FontStyle::Normal
            }
        && last.font.weight == weight
        && last.strikethrough.is_some() == look.strikethrough
    {
        last.len += len;
        return;
    }
    runs.push(styled_run(len, font, look));
}

/// Text runs of the buffer text in `range`, gaps between spans are plain code. The runs
/// cover `range` exactly once: spans and tokens that overlap, run backwards or are left
/// over from an older text (an answer that came before the new parse) are cut to what is
/// still free, because runs longer than the line bring the text system down.
///
/// Semantic tokens are painted over the syntax spans, and the ranges a server marked as
/// unnecessary over both.
fn runs_in(
    runs: &mut Vec<TextRun>,
    range: Range<usize>,
    spans: &[HighlightSpan],
    tokens: &[TokenSpan],
    unnecessary: &[Range<usize>],
    style: &EditorStyle,
    font: &Font,
) {
    if range.start >= range.end {
        return;
    }
    let mut points = Vec::with_capacity(8);
    points.push(range.start);
    points.push(range.end);
    let first_span = spans.partition_point(|span| (span.end as usize) <= range.start);
    let first_token = tokens.partition_point(|token| (token.end as usize) <= range.start);
    let first_unused = unnecessary.partition_point(|range| range.end <= range.start);
    let mut add = |start: usize, end: usize| {
        let start = start.max(range.start);
        let end = end.min(range.end);
        if start < end {
            points.push(start);
            points.push(end);
        }
    };
    for span in &spans[first_span..] {
        add(span.start as usize, span.end as usize);
    }
    for token in &tokens[first_token..] {
        add(token.start as usize, token.end as usize);
    }
    for range in &unnecessary[first_unused..] {
        add(range.start, range.end);
    }
    points.sort_unstable();
    points.dedup();

    let mut span_at = first_span;
    let mut token_at = first_token;
    let mut unused_at = first_unused;
    for pair in points.windows(2) {
        let (start, end) = (pair[0], pair[1]);
        if start >= end {
            continue;
        }
        while span_at < spans.len() && (spans[span_at].end as usize) <= start {
            span_at += 1;
        }
        while token_at < tokens.len() && (tokens[token_at].end as usize) <= start {
            token_at += 1;
        }
        while unused_at < unnecessary.len() && unnecessary[unused_at].end <= start {
            unused_at += 1;
        }
        let unused = unnecessary
            .get(unused_at)
            .is_some_and(|range| range.start <= start && start < range.end);
        let token = tokens
            .get(token_at)
            .filter(|token| (token.start as usize) <= start && start < token.end as usize)
            .and_then(|token| style.token_style(token.kind, token.modifiers));
        if unused {
            push_run(runs, end - start, font, RunLook::plain(style.unused));
        } else if let Some(look) = token {
            push_run(
                runs,
                end - start,
                font,
                RunLook {
                    color: look.color,
                    italic: look.italic,
                    strikethrough: look.strikethrough,
                    ..RunLook::plain(look.color)
                },
            );
        } else if let Some(span) = spans
            .get(span_at)
            .filter(|span| (span.start as usize) <= start && start < span.end as usize)
        {
            let kind: HighlightKind = span.kind;
            push_run(
                runs,
                end - start,
                font,
                RunLook {
                    color: style.color_of(kind),
                    background: style.background_of(kind),
                    italic: style.is_italic(kind),
                    bold: style.is_bold(kind),
                    strikethrough: style.is_strikethrough(kind),
                },
            );
        } else {
            push_run(runs, end - start, font, RunLook::plain(style.code));
        }
    }
}

/// The run of one byte at `offset`: the look the editor gives it. The caller keeps
/// the offset inside the text and on a character boundary.
pub(crate) fn look_at(
    offset: usize,
    spans: &[HighlightSpan],
    style: &EditorStyle,
    font: &Font,
) -> TextRun {
    let mut runs = Vec::with_capacity(1);
    runs_in(&mut runs, offset..offset + 1, spans, &[], &[], style, font);
    match runs.pop() {
        Some(run) => run,
        None => styled_run(1, font, RunLook::plain(style.code)),
    }
}

/// The runs of `spans` alone, for the tests of the merge.
#[cfg(test)]
fn push_text_runs(
    runs: &mut Vec<TextRun>,
    range: Range<usize>,
    spans: &[HighlightSpan],
    style: &EditorStyle,
    font: &Font,
) {
    runs_in(runs, range, spans, &[], &[], style, font);
}

fn runs_of(
    line: &DisplayLine,
    spans: &[HighlightSpan],
    tokens: &[TokenSpan],
    unnecessary: &[Range<usize>],
    style: &EditorStyle,
    font: &Font,
) -> Vec<TextRun> {
    let mut runs = Vec::with_capacity(line.chunks.len() * 2);
    for (range, chunk) in &line.chunks {
        match chunk {
            DisplayChunk::Text { buffer_start } => {
                let start = *buffer_start;
                runs_in(
                    &mut runs,
                    start..start + range.len(),
                    spans,
                    tokens,
                    unnecessary,
                    style,
                    font,
                );
            }
            DisplayChunk::Tab { .. } => {
                push_run(&mut runs, range.len(), font, RunLook::plain(style.code))
            }
            DisplayChunk::Placeholder { .. } => push_run(
                &mut runs,
                range.len(),
                font,
                RunLook::plain(style.fold_text),
            ),
            DisplayChunk::Inlay { .. } => {
                // Painted on its own at a smaller size; the row keeps the width.
                let clear = Hsla {
                    a: 0.,
                    ..style.inlay
                };
                push_run(&mut runs, range.len(), font, RunLook::plain(clear))
            }
        }
    }
    runs
}

// The number of counts of the widest row of a text, for tests of big files: a count
// walks the rows of the whole text once, so the number must follow the text changes, not
// the frames. The count belongs to the thread of a test, so windows of tests that run
// side by side do not add to each other's numbers.
#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static WIDEST_COUNTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How many times a frame counted the widest row of a text since the window opened.
#[cfg(any(test, feature = "test-support"))]
pub fn widest_row_counts() -> usize {
    WIDEST_COUNTS.with(std::cell::Cell::get)
}

/// Starts the count of [`widest_row_counts`] over.
#[cfg(any(test, feature = "test-support"))]
pub fn reset_widest_row_counts() {
    WIDEST_COUNTS.with(|count| count.set(0));
}

fn note_widest_count() {
    #[cfg(any(test, feature = "test-support"))]
    WIDEST_COUNTS.with(|count| count.set(count.get() + 1));
}

/// Adds one mark to the vertical bar unless one of the same kind already stands on
/// the line.
#[allow(clippy::too_many_arguments)]
fn add_stripe<S: TextStore>(
    stripes: &mut Vec<StripeMark>,
    seen: &mut Vec<(StripeKind, usize)>,
    display: &display_map::DisplaySnapshot<'_, S>,
    bar: Bar,
    x: f32,
    width: f32,
    kind: StripeKind,
    line: usize,
    thickness: f32,
    message: Option<String>,
) {
    if seen.contains(&(kind, line)) {
        return;
    }
    seen.push((kind, line));
    let y = bar.track_start + scrollbar::stripe_fraction(display, line) * bar.track_len;
    stripes.push(StripeMark {
        kind,
        line,
        bounds: Bounds::new(
            point(px(x), px(y - thickness / 2.)),
            size(px(width), px(thickness)),
        ),
        color: None,
        message,
    });
}

/// A blank row takes the smaller indent of its neighbours, so guides do not break.
fn fill_blank_indents(indents: &[Option<usize>]) -> Vec<usize> {
    let mut next = vec![0; indents.len()];
    let mut seen = 0;
    for index in (0..indents.len()).rev() {
        if let Some(indent) = indents[index] {
            seen = indent;
        }
        next[index] = seen;
    }
    let mut previous = 0;
    indents
        .iter()
        .zip(next)
        .map(|(indent, next)| match indent {
            Some(indent) => {
                previous = *indent;
                *indent
            }
            None => previous.min(next),
        })
        .collect()
}

/// Where the baseline of a line painted into a box of `height` stands, from its top.
fn baseline(line: &ShapedLine, height: Pixels) -> Pixels {
    (height - line.ascent - line.descent) * 0.5 + line.ascent
}

/// Pushes the green wave of a misspelled word: short pieces of a line that step up and
/// down under the letters.
fn wave(
    underlines: &mut Vec<(Bounds<Pixels>, Hsla)>,
    left: Pixels,
    width: Pixels,
    top: Pixels,
    height: Pixels,
    color: Hsla,
) {
    let step = px(4.);
    let mut at = px(0.);
    let mut high = true;
    while at < width {
        let piece = (width - at).min(step);
        let rise = if high { px(3.) } else { px(1.) };
        underlines.push((
            Bounds::new(
                point(left + at, top + height - rise),
                size((piece - px(1.)).max(px(1.)), px(1.)),
            ),
            color,
        ));
        at += step;
        high = !high;
    }
}

impl Element for EditorElement {
    type RequestLayoutState = ();
    type PrepaintState = EditorPrepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> EditorPrepaint {
        let (style, options) = {
            let view = self.view.read(cx);
            (view.style.clone(), view.options.clone())
        };
        let mut code_font = font(style.font_family.clone());
        code_font.features = FontFeatures::disable_ligatures();
        #[cfg(unix)]
        {
            // A distribution does not always ship JetBrains Mono.
            code_font.fallbacks = Some(FontFallbacks::from_fonts(vec![
                "DejaVu Sans Mono".to_owned(),
                "monospace".to_owned(),
            ]));
        }
        let ui_font = font(UI_FONT);
        let font_size = px(snapped_size(style.font_size, window.scale_factor()));
        let inlay_size = px((f32::from(font_size) * 0.82).max(9.));
        let block_size = inlay_size;
        let text_system = window.text_system().clone();
        let probe = [run(1, &code_font, style.code, false)];
        let char_width = text_system
            .shape_line("0".into(), font_size, &probe, None)
            .width();
        let line_height = style.line_height;

        let annotate = self.view.read(cx).annotation_width();
        // The column of the numbers follows the count of them: a file that crosses 999,
        // 9999 or 99999 lines grows it and a shorter one gives it back. The width of a
        // digit comes from the font of the numbers, not from a guess.
        let digits = self
            .view
            .read(cx)
            .state
            .as_ref()
            .map_or(1, |state| number_digits(state.store().line_count()));
        let number_width = {
            let zeros = "0".repeat(digits as usize);
            let runs = [run(zeros.len(), &code_font, style.line_number, false)];
            text_system
                .shape_line(zeros.into(), px(NUMBER_SIZE), &runs, None)
                .width()
        };
        let gutter = if options.centered_text {
            0.
        } else {
            gutter_width(f32::from(number_width))
        };
        let text_width =
            (bounds.size.width - px(gutter + TEXT_INSET + annotate + scrollbar::BAND)).max(px(1.));
        // Distraction Free Mode centres a column of the width of the right margin.
        let center = if options.centered_text {
            let margin = f32::from(char_width) * options.right_margin.max(1) as f32;
            ((f32::from(text_width) - margin) / 2.).max(0.)
        } else {
            0.
        };
        let text_x = bounds.left() + px(gutter + TEXT_INSET + annotate + center);
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        let mut text_bounds = bounds;
        text_bounds.origin.x += px(gutter + annotate + center);
        text_bounds.size.width -= px(gutter + annotate + center + scrollbar::BAND);
        let text_hitbox = window.insert_hitbox(text_bounds, HitboxBehavior::Normal);

        let decor = self.decor.clone();
        let columns = (f32::from(text_width) / f32::from(char_width).max(1.)).floor() as usize;
        let wrap = self
            .view
            .read(cx)
            .soft_wrap()
            .then(|| columns.saturating_sub(1).max(8));
        self.view.update(cx, |view, _| {
            view.viewport = bounds.size;
            view.center_inset = center;
            view.gutter_width = gutter_width(f32::from(number_width));
            if let Some(state) = &mut view.state
                && state.wrap() != wrap
            {
                state.set_wrap(wrap);
            }
            if wrap.is_some() {
                view.scroll_left = 0.;
            }
            if view.state.is_none() {
                view.horizontal_bar = false;
            }
        });

        // One snapshot of the display serves the widest row, the scroll and the rows of
        // this frame: building it again for each of them would cost the file twice.
        let view = self.view.read(cx);
        let blocks = view
            .state
            .as_ref()
            .map(|state| decor.blocks(&state.store()))
            .unwrap_or_default();
        let display = view.state.as_ref().map(|state| {
            state
                .map()
                .snapshot_with_blocks_owned(state.store(), &blocks)
        });
        let mut widest_width = 0f32;
        let mut horizontal_bar = false;
        let mut scroll_top = view.scroll_top;
        let mut reveal_center = view.reveal_center;
        let mut widest_row = None;
        if let Some(state) = &view.state
            && let Some(display) = &display
        {
            if wrap.is_none() {
                let counted = match view.widest {
                    Some(widest) if widest.matches(state) => Some(widest.columns()),
                    _ => None,
                };
                let columns = counted.unwrap_or_else(|| display.widest_row_columns());
                if counted.is_none() {
                    note_widest_count();
                    widest_row = Some(WidestRow::of(state, columns));
                }
                widest_width = columns as f32 * f32::from(char_width);
                horizontal_bar = widest_width > f32::from(text_width) + 0.5;
            }
            let height = f32::from(bounds.size.height)
                - if horizontal_bar {
                    scrollbar::HORIZONTAL_HEIGHT
                } else {
                    0.
                };
            if view.autoscroll {
                let row = display.to_display(state.selections().primary().head).row;
                let top = row as f32 * line_height;
                let near_bottom = top + line_height > scroll_top + height;
                let near_top = top < scroll_top;
                if reveal_center {
                    // A jump to a place centers the line when it is out of sight or at
                    // the edge, as IDEA does.
                    let edge = line_height * 2.;
                    let at_edge =
                        top < scroll_top + edge || top + line_height > scroll_top + height - edge;
                    if near_top || near_bottom || at_edge {
                        let margin = (height - line_height) / 2.;
                        scroll_top = (top - margin).max(0.);
                    }
                    reveal_center = false;
                } else if near_top {
                    scroll_top = top;
                } else if near_bottom {
                    scroll_top = top + line_height - height;
                }
            }
        }

        let mut rows = Vec::new();
        let mut highlights = Vec::new();
        let mut line_tints = Vec::new();
        let mut range_tints = Vec::new();
        let mut sticky = Vec::new();
        let mut inlay_paints = Vec::new();
        let mut selections = Vec::new();
        let mut carets = Vec::new();
        let mut marked = None;
        let mut underlines = Vec::new();
        let mut gutter_marks = Vec::new();
        let mut occurrences = Vec::new();
        let mut stripes = Vec::new();
        let mut vertical = None;
        let mut horizontal = None;
        let mut vertical_area = None;
        let mut horizontal_area = None;
        let mut scroll_left = view.scroll_left;
        let text_height = f32::from(bounds.size.height)
            - if horizontal_bar {
                scrollbar::HORIZONTAL_HEIGHT
            } else {
                0.
            };

        if let Some(state) = &view.state
            && let Some(display) = &display
        {
            let store = state.store();
            let primary_row = display.to_display(state.selections().primary().head).row;
            let first = (scroll_top / line_height) as usize;
            let line_of = |at: usize| store.offset_to_line(at.min(store.len()));
            let top_line = if first < display.row_count() {
                display.row_to_line(first)
            } else {
                store.line_count()
            };
            let pinned = pinned_items(view.outline(), top_line, line_of);
            // The panel of pinned declarations takes whole rows and the gap under them:
            // the text starts on a row boundary below it, so no letter is cut in half.
            let offset = if pinned.is_empty() {
                -(scroll_top % line_height)
            } else {
                STICKY_GAP
            };
            let visible = (text_height / line_height).ceil() as usize + pinned.len() + 1;
            let last = (first + visible).min(display.row_count());

            let lines: Vec<DisplayLine> = (first..last).map(|row| display.line(row)).collect();
            let tokens: &[TokenSpan] = if view.options.semantic_highlighting {
                state.tokens()
            } else {
                &[]
            };
            let unnecessary: Vec<Range<usize>> = view
                .lsp
                .as_ref()
                .map(|lsp| {
                    lsp.marks
                        .iter()
                        .filter(|mark| mark.unnecessary)
                        .map(|mark| mark.range.clone())
                        .collect()
                })
                .unwrap_or_default();
            let indents: Vec<Option<usize>> = lines
                .iter()
                .map(|line| {
                    let blank = line.text.trim().is_empty();
                    (!blank).then(|| line.text.len() - line.text.trim_start().len())
                })
                .collect();
            let indents = fill_blank_indents(&indents);

            for (slot, (line, indent)) in lines.into_iter().zip(indents).enumerate() {
                let row = first + slot;
                let doc_row = display.doc_row(row);
                let block = display.is_block_row(row) && doc_row.is_none();
                let current = row == primary_row && !block && doc_row.is_none();
                let buffer_line = store.offset_to_line(line.start_offset);
                let continuation = line.continuation;
                let indent = if block {
                    line.block.as_ref().map_or(0, |block| block.indent)
                } else {
                    indent
                };
                let bookmark = if continuation || block || doc_row.is_some() {
                    None
                } else {
                    state.bookmark_at_line(buffer_line)
                };
                // A named bookmark shows its letter where the number is.
                let number_text: SharedString = match (continuation, bookmark) {
                    (_, _) if block => "".into(),
                    (_, _) if doc_row.is_some() => "".into(),
                    (true, _) => "".into(),
                    (false, Some(Some(letter))) => letter.to_string().into(),
                    (false, _) if !options.line_numbers => "".into(),
                    (false, _) => (buffer_line + 1).to_string().into(),
                };
                let number_value = if block
                    || doc_row.is_some()
                    || continuation
                    || matches!(bookmark, Some(Some(_)))
                    || !options.line_numbers
                {
                    None
                } else {
                    Some(buffer_line + 1)
                };
                let number_color = match bookmark {
                    Some(Some(_)) => style.accent,
                    _ if current => style.line_number_current,
                    _ => style.line_number,
                };
                let number_run = [run(number_text.len(), &code_font, number_color, false)];
                let used = if number_text.is_empty() {
                    0
                } else {
                    number_run.len()
                };
                let number =
                    text_system.shape_line(number_text, px(NUMBER_SIZE), &number_run[..used], None);

                let mut doc_links = Vec::new();
                let (text, block_labels) = if let Some(doc) = doc_row {
                    let y = bounds.top() + px(offset + slot as f32 * line_height);
                    let mut label = String::new();
                    let mut runs = Vec::with_capacity(doc.runs.len());
                    let mut links: Vec<(usize, usize, String)> = Vec::new();
                    for piece in &doc.runs {
                        let start = label.len();
                        label.push_str(&piece.text);
                        let end = label.len();
                        let mut font = match piece.kind {
                            display_map::DocRunKind::Code => code_font.clone(),
                            _ => ui_font.clone(),
                        };
                        let (color, italic, bold) = match piece.kind {
                            display_map::DocRunKind::Text => (style.code, false, false),
                            display_map::DocRunKind::Code => (style.code, false, false),
                            display_map::DocRunKind::Strong => (style.doc_tag, false, true),
                            display_map::DocRunKind::Emphasis => (style.doc_tag, true, false),
                            display_map::DocRunKind::Link => (style.accent, false, false),
                        };
                        if bold {
                            font.weight = FontWeight::SEMIBOLD;
                        }
                        runs.push(run(piece.text.len(), &font, color, italic));
                        if piece.kind == display_map::DocRunKind::Link
                            && let Some(target) = &piece.target
                        {
                            links.push((start, end, target.clone()));
                        }
                    }
                    let shaped = text_system.shape_line(label.into(), font_size, &runs, None);
                    let text_start = text_x + char_width * doc.indent as f32;
                    for (start, end, target) in links {
                        let left = shaped.x_for_index(start);
                        let right = shaped.x_for_index(end).max(left + px(1.));
                        doc_links.push(DocLinkLabel {
                            target,
                            bounds: Bounds::new(
                                point(text_start + left, y),
                                size(right - left, px(line_height)),
                            ),
                        });
                    }
                    (shaped, Vec::new())
                } else if block {
                    let y = bounds.top() + px(offset + slot as f32 * line_height);
                    let labels = view.code_vision_labels(buffer_line);
                    let mut block_labels = Vec::with_capacity(labels.len());
                    let mut x = char_width * indent as f32;
                    for (kind, label) in labels {
                        let runs = [run(label.len(), &ui_font, style.code_vision, false)];
                        let shaped = text_system.shape_line(label.into(), block_size, &runs, None);
                        let width = shaped.width();
                        block_labels.push(BlockLabel {
                            kind: *kind,
                            text: shaped,
                            bounds: Bounds::new(point(text_x + x, y), size(width, px(line_height))),
                        });
                        x += width + px(8.);
                    }
                    let runs = [run(line.text.len(), &ui_font, style.code_vision, false)];
                    let shaped =
                        text_system.shape_line(line.text.clone().into(), block_size, &runs, None);
                    (shaped, block_labels)
                } else {
                    let runs = runs_of(
                        &line,
                        state.spans(),
                        tokens,
                        &unnecessary,
                        &style,
                        &code_font,
                    );
                    let shaped =
                        text_system.shape_line(line.text.clone().into(), font_size, &runs, None);
                    (shaped, Vec::new())
                };
                let mut inlays = Vec::new();
                if !block && doc_row.is_none() {
                    let code_baseline = baseline(&text, px(line_height));
                    for (range, chunk) in &line.chunks {
                        if !matches!(chunk, DisplayChunk::Inlay { .. }) {
                            continue;
                        }
                        // The padding spaces of the server stay as columns around the pill,
                        // the pill itself hugs the label.
                        let hint = line.text[range.clone()].trim().to_owned();
                        if hint.is_empty() {
                            continue;
                        }
                        let runs = [run(hint.len(), &code_font, style.inlay, false)];
                        let shaped = text_system.shape_line(hint.into(), inlay_size, &runs, None);
                        // A smaller font centered in the row puts its baseline above the
                        // one of the code: the label gets a box that lands on that line.
                        let padding = (code_baseline - shaped.ascent).max(px(0.));
                        let height = shaped.ascent + shaped.descent + padding * 2.;
                        inlays.push(InlayText {
                            columns: range.clone(),
                            text: shaped,
                            height,
                            paint: None,
                        });
                    }
                }

                let collapsed = line
                    .chunks
                    .iter()
                    .any(|(_, chunk)| matches!(chunk, DisplayChunk::Placeholder { .. }));
                let foldable = !block
                    && doc_row.is_none()
                    && state
                        .map()
                        .folds
                        .candidate_on_line(&store, buffer_line)
                        .is_some();
                let fold = if continuation || block || doc_row.is_some() {
                    None
                } else if collapsed {
                    Some(true)
                } else {
                    (foldable && view.gutter_hovered).then_some(false)
                };
                let run = view
                    .run_marks
                    .iter()
                    .find(|mark| {
                        !block
                            && doc_row.is_none()
                            && !continuation
                            && mark.line as usize == buffer_line
                    })
                    .map(|mark| (mark.state, mark.color));
                let endpoint = view
                    .endpoint_marks
                    .iter()
                    .find(|mark| {
                        !block
                            && doc_row.is_none()
                            && !continuation
                            && mark.line as usize == buffer_line
                    })
                    .map(|mark| mark.color);
                let place = view
                    .place_marks
                    .iter()
                    .find(|mark| {
                        !block
                            && doc_row.is_none()
                            && !continuation
                            && mark.line as usize == buffer_line
                    })
                    .map(|mark| (mark.icon.clone(), mark.color));
                let action = view
                    .action_marks
                    .iter()
                    .find(|mark| {
                        !block
                            && doc_row.is_none()
                            && !continuation
                            && mark.line as usize == buffer_line
                    })
                    .map(|mark| (mark.icon.clone(), mark.color));
                let doc_icon = (!continuation)
                    .then(|| view.rendered_doc_at_line(buffer_line))
                    .flatten();
                let doc_range = display.doc_range(row);
                let annotation = if annotate > 0. && !block && doc_row.is_none() && !continuation {
                    state.annotation_label_at(buffer_line).map(|note| {
                        let runs = [crate::element::run(
                            note.label.len(),
                            &code_font,
                            style.line_number,
                            false,
                        )];
                        let used = if note.label.is_empty() { 0 } else { 1 };
                        text_system.shape_line(
                            note.label.clone().into(),
                            px(NUMBER_SIZE),
                            &runs[..used],
                            None,
                        )
                    })
                } else {
                    None
                };
                // The values of the stopped frame stand after the line in grey italics,
                // and in the accent colour when they changed since the previous stop.
                let tail = if block || continuation || doc_row.is_some() {
                    None
                } else {
                    view.debug_value_at(buffer_line).map(|value| {
                        let color = if value.changed {
                            style.accent
                        } else {
                            style.inlay
                        };
                        let runs = [crate::element::run(
                            value.text.len(),
                            &code_font,
                            color,
                            true,
                        )];
                        text_system.shape_line(value.text.clone().into(), font_size, &runs, None)
                    })
                };
                let tail_width = tail.as_ref().map_or(px(0.), |tail| tail.width());
                let watch = if block || continuation || doc_row.is_some() {
                    None
                } else {
                    view.inline_watch_at(buffer_line).map(|watch| {
                        let runs = [crate::element::run(
                            watch.text.len(),
                            &code_font,
                            style.inlay,
                            true,
                        )];
                        let text = text_system.shape_line(
                            watch.text.clone().into(),
                            font_size,
                            &runs,
                            None,
                        );
                        let cross_runs = [crate::element::run(
                            "✕".len(),
                            &code_font,
                            style.line_number,
                            false,
                        )];
                        let cross =
                            text_system.shape_line("✕".into(), font_size, &cross_runs, None);
                        let start = text_x + text.width() + px(8.);
                        let start = if tail.is_some() {
                            start + tail_width + px(8.)
                        } else {
                            start
                        };
                        LaidOutWatch {
                            expression: watch.expression.clone(),
                            cross_x: start + text.width() + px(6.),
                            text_x: start,
                            text,
                            cross,
                        }
                    })
                };
                rows.push(LaidOutRow {
                    row,
                    buffer_line,
                    y: bounds.top() + px(offset + slot as f32 * line_height),
                    line,
                    text,
                    number,
                    number_value,
                    inlays,
                    fold,
                    run,
                    endpoint,
                    place,
                    action,
                    bookmark: !block && matches!(bookmark, Some(None)),
                    annotation,
                    current,
                    indent,
                    is_block: block || doc_row.is_some(),
                    is_doc: doc_row.is_some(),
                    doc_icon,
                    doc_range,
                    block_labels,
                    doc_links,
                    tail,
                    watch,
                });
            }

            if view.autoscroll
                && let Some(shown) = rows.iter().find(|laid_out| laid_out.current)
            {
                let head = state.selections().primary().head;
                let x = f32::from(shown.text.x_for_index(shown.line.column_of(head)));
                let width = f32::from(bounds.size.width)
                    - gutter
                    - annotate
                    - TEXT_INSET
                    - scrollbar::BAND
                    - f32::from(char_width) * 2.;
                if x < scroll_left {
                    scroll_left = x;
                } else if x > scroll_left + width {
                    scroll_left = x - width;
                }
            }

            if options.sticky_lines && !pinned.is_empty() {
                for item in &pinned {
                    let line = line_of(item.name_range.start);
                    let text = store.line_text(line);
                    let text = text.trim_end().replace('\t', "    ");
                    let runs = [run(text.len(), &code_font, style.code, false)];
                    let used = if text.is_empty() { 0 } else { 1 };
                    let shaped =
                        text_system.shape_line(text.into(), font_size, &runs[..used], None);
                    let label: SharedString = if options.line_numbers {
                        (line + 1).to_string().into()
                    } else {
                        "".into()
                    };
                    let label_run = [run(label.len(), &code_font, style.line_number, false)];
                    let used = if label.is_empty() { 0 } else { 1 };
                    let number =
                        text_system.shape_line(label, px(NUMBER_SIZE), &label_run[..used], None);
                    sticky.push(StickyRow {
                        line,
                        text: shaped,
                        number,
                    });
                }
            }

            let origin_x = text_x - px(scroll_left);
            let height = px(line_height);
            for shown in rows.iter_mut() {
                if shown.is_block {
                    continue;
                }
                for inlay in &mut shown.inlays {
                    let left = origin_x + shown.text.x_for_index(inlay.columns.start);
                    let right = origin_x + shown.text.x_for_index(inlay.columns.end);
                    let width = inlay.text.width();
                    let x = left + ((right - left) - width).max(px(0.)) * 0.5;
                    // The pill hugs the label but never covers the characters beside it.
                    let pill_left = (x - px(3.)).max(left);
                    let pill_right = (x + width + px(3.)).min(right).max(pill_left + px(1.));
                    let pill = Bounds::new(
                        point(pill_left, shown.y + px(2.)),
                        size(pill_right - pill_left, height - px(4.)),
                    );
                    let label_baseline = (inlay.height - inlay.text.ascent - inlay.text.descent)
                        * 0.5
                        + inlay.text.ascent;
                    inlay_paints.push(InlayPaint {
                        line: shown.buffer_line,
                        columns: inlay.columns.clone(),
                        columns_left: left,
                        columns_right: right,
                        pill,
                        label_x: x,
                        baseline: f32::from(label_baseline),
                        code_baseline: f32::from(baseline(&shown.text, height)),
                    });
                    inlay.paint = inlay_paints.last().cloned();
                }
            }
            if let (Some(first), Some(last)) = (rows.first(), rows.last()) {
                let visible = first.line.start_offset..last.line.end_offset;
                for (range, color) in view.highlights(state, visible) {
                    for shown in &rows {
                        if shown.is_block {
                            continue;
                        }
                        let (start, end) = (shown.line.start_offset, shown.line.end_offset);
                        if range.end <= start || range.start >= end {
                            continue;
                        }
                        let column = |offset: usize| shown.line.column_of(offset.clamp(start, end));
                        let left = shown.text.x_for_index(column(range.start));
                        let right = shown.text.x_for_index(column(range.end));
                        highlights.push((
                            Bounds::new(
                                point(origin_x + left, shown.y),
                                size(right - left, height),
                            ),
                            color,
                        ));
                    }
                }
            }
            for (line, color) in &self.decor.lines {
                for shown in &rows {
                    if shown.is_block || shown.buffer_line != *line {
                        continue;
                    }
                    line_tints.push((
                        Bounds::new(
                            point(bounds.left(), shown.y),
                            size(bounds.size.width, height),
                        ),
                        *color,
                    ));
                }
            }
            for (range, color) in &self.decor.ranges {
                for shown in &rows {
                    if shown.is_block {
                        continue;
                    }
                    let (start, end) = (shown.line.start_offset, shown.line.end_offset);
                    if range.end <= start || range.start >= end {
                        continue;
                    }
                    let column = |offset: usize| shown.line.column_of(offset.clamp(start, end));
                    let left = shown.text.x_for_index(column(range.start));
                    let right = shown.text.x_for_index(column(range.end));
                    range_tints.push((
                        Bounds::new(point(origin_x + left, shown.y), size(right - left, height)),
                        *color,
                    ));
                }
            }
            for shown in &rows {
                if shown.is_block {
                    continue;
                }
                let (start, end) = (shown.line.start_offset, shown.line.end_offset);
                for selection in state
                    .selections()
                    .iter()
                    .filter(|selection| !selection.is_empty())
                {
                    if selection.end() < start || selection.start() > end {
                        continue;
                    }
                    let left = if selection.start() <= start {
                        px(0.)
                    } else {
                        shown
                            .text
                            .x_for_index(shown.line.column_of(selection.start()))
                    };
                    let right = if selection.end() > end {
                        shown.text.width() + char_width
                    } else {
                        shown
                            .text
                            .x_for_index(shown.line.column_of(selection.end()))
                    };
                    selections.push(Bounds::new(
                        point(origin_x + left, shown.y),
                        size(right - left, height),
                    ));
                }
            }
            if view.caret_on {
                for selection in state.selections().iter() {
                    let point_at = display.to_display(selection.head);
                    if let Some(shown) = rows.iter().find(|laid_out| laid_out.row == point_at.row) {
                        let x = origin_x + shown.text.x_for_index(point_at.column);
                        let width = if state.overwrite() {
                            char_width
                        } else {
                            px(2.)
                        };
                        carets.push(Bounds::new(
                            point(x, shown.y + px(1.)),
                            size(width, height - px(2.)),
                        ));
                    }
                }
            }
            if let Some(range) = state.marked() {
                let (from, to) = (
                    display.to_display(range.start),
                    display.to_display(range.end),
                );
                if let Some(shown) = rows.iter().find(|laid_out| laid_out.row == from.row) {
                    let left = shown.text.x_for_index(from.column);
                    let right = shown.text.x_for_index(to.column);
                    marked = Some(Bounds::new(
                        point(origin_x + left, shown.y + height - px(2.)),
                        size(right - left, px(1.)),
                    ));
                }
            }
        }

        let occurrence_x = text_x - px(scroll_left);
        let occurrence_height = px(line_height);
        if let Some(usages) = &view.usages {
            for range in usages {
                for shown in &rows {
                    if shown.is_block {
                        continue;
                    }
                    let (start, end) = (shown.line.start_offset, shown.line.end_offset);
                    if range.end < start || range.start > end {
                        continue;
                    }
                    let from = range.start.max(start);
                    let to = range.end.min(end).max(from);
                    let left = shown.text.x_for_index(shown.line.column_of(from));
                    let right = shown.text.x_for_index(shown.line.column_of(to));
                    occurrences.push(Bounds::new(
                        point(occurrence_x + left, shown.y),
                        size(right - left, occurrence_height),
                    ));
                }
            }
        }

        if let Some(lsp) = &view.lsp {
            let origin_x = text_x - px(scroll_left);
            let height = px(line_height);
            if view.usages.is_none() {
                for range in &lsp.highlights {
                    for shown in &rows {
                        if shown.is_block {
                            continue;
                        }
                        let (start, end) = (shown.line.start_offset, shown.line.end_offset);
                        if range.end < start || range.start > end {
                            continue;
                        }
                        let from = range.start.max(start);
                        let to = range.end.min(end).max(from);
                        let left = shown.text.x_for_index(shown.line.column_of(from));
                        let right = shown.text.x_for_index(shown.line.column_of(to));
                        occurrences.push(Bounds::new(
                            point(origin_x + left, shown.y),
                            size(right - left, height),
                        ));
                    }
                }
            }
            for mark in &lsp.marks {
                // An unnecessary range is grey in the text; a line under it would only
                // draw the eye to a name that is already muted.
                if mark.unnecessary {
                    continue;
                }
                let spelling = mark.code.as_deref() == Some(lang_api::SPELLING_INSPECTION);
                let color = match mark.severity {
                    Severity::Error => style.error_mark,
                    Severity::Warning => style.warning_mark,
                    Severity::Information | Severity::Hint if !spelling => continue,
                    Severity::Information | Severity::Hint => style.spell_mark,
                };
                for shown in &rows {
                    if shown.is_block {
                        continue;
                    }
                    let (start, end) = (shown.line.start_offset, shown.line.end_offset);
                    if mark.range.end < start || mark.range.start > end {
                        continue;
                    }
                    let from = mark.range.start.max(start);
                    let to = mark.range.end.min(end).max(from);
                    let left = shown.text.x_for_index(shown.line.column_of(from));
                    let right = shown
                        .text
                        .x_for_index(shown.line.column_of(to))
                        .max(left + char_width);
                    if spelling {
                        // A misspelled word keeps the green wave of an IDE and no mark
                        // on the gutter: it is no problem of the compiler.
                        wave(
                            &mut underlines,
                            origin_x + left,
                            right - left,
                            shown.y,
                            height,
                            color,
                        );
                        continue;
                    }
                    underlines.push((
                        Bounds::new(
                            point(origin_x + left, shown.y + height - px(2.)),
                            size(right - left, px(2.)),
                        ),
                        color,
                    ));
                    gutter_marks.push((
                        Bounds::new(
                            point(bounds.left() + px(28.), shown.y + px(3.)),
                            size(px(4.), height - px(6.)),
                        ),
                        color,
                    ));
                }
            }
        }

        if let Some(state) = &view.state
            && let Some(display) = &display
        {
            let store = state.store();
            let row_count = display.row_count().max(1);
            let horizontal_viewport = f32::from(text_width);
            let horizontal_track_start = f32::from(text_x);
            let horizontal_track_len = f32::from(text_width);
            let horizontal_bar = (widest_width > horizontal_viewport + 0.5
                && horizontal_track_len > 0.)
                .then_some(Bar {
                    track_start: horizontal_track_start,
                    track_len: horizontal_track_len,
                    content: widest_width,
                    viewport: horizontal_viewport,
                });

            let track_height = f32::from(bounds.size.height)
                - if horizontal_bar.is_some() {
                    scrollbar::HORIZONTAL_HEIGHT
                } else {
                    0.
                };
            let content_height = row_count as f32 * line_height;
            let vertical_bar = (content_height > track_height + 0.5).then_some(Bar {
                track_start: f32::from(bounds.top()),
                track_len: track_height,
                content: content_height,
                viewport: track_height,
            });

            let track_right = f32::from(bounds.right()) - scrollbar::MARGIN;
            if vertical_bar.is_some() {
                vertical_area = Some(Bounds::new(
                    point(px(track_right - scrollbar::HOVER_WIDTH), bounds.top()),
                    size(px(scrollbar::HOVER_WIDTH), px(track_height)),
                ));
            }
            if horizontal_bar.is_some() {
                horizontal_area = Some(Bounds::new(
                    point(
                        px(horizontal_track_start),
                        px(f32::from(bounds.bottom()) - scrollbar::HORIZONTAL_HOVER_HEIGHT),
                    ),
                    size(
                        px(horizontal_track_len),
                        px(scrollbar::HORIZONTAL_HOVER_HEIGHT),
                    ),
                ));
            }

            if let Some(bar) = vertical_bar {
                let width = if view.scrollbar_hovered {
                    scrollbar::HOVER_WIDTH
                } else {
                    scrollbar::WIDTH
                };
                let x = track_right - width;
                let mut seen: Vec<(StripeKind, usize)> = Vec::new();

                for mark in &view.gutter_marks {
                    let line = mark.lines.start as usize;
                    let end = mark.lines.end.saturating_sub(1).max(mark.lines.start) as usize;
                    let start_row = display.line_to_row(line);
                    let end_row = display.line_to_row(end);
                    let top = bar.track_start + start_row as f32 / row_count as f32 * bar.track_len;
                    let bottom =
                        bar.track_start + (end_row + 1) as f32 / row_count as f32 * bar.track_len;
                    stripes.push(StripeMark {
                        kind: StripeKind::Vcs,
                        line,
                        bounds: Bounds::new(
                            point(px(x), px(top)),
                            size(px(width), px((bottom - top).max(2.))),
                        ),
                        color: Some(mark.color),
                        message: None,
                    });
                }
                if let Some(find) = &view.bar
                    && find.kind != crate::bar::BarKind::Goto
                {
                    for found in &find.matches {
                        let line = store.offset_to_line(found.start.min(store.len()));
                        add_stripe(
                            &mut stripes,
                            &mut seen,
                            display,
                            bar,
                            x,
                            width,
                            StripeKind::Find,
                            line,
                            2.,
                            None,
                        );
                    }
                }
                let caret_line = store.offset_to_line(state.selections().primary().head);
                add_stripe(
                    &mut stripes,
                    &mut seen,
                    display,
                    bar,
                    x,
                    width,
                    StripeKind::Caret,
                    caret_line,
                    2.,
                    None,
                );
                if let Some(lsp) = &view.lsp {
                    for mark in &lsp.marks {
                        if mark.unnecessary {
                            continue;
                        }
                        if mark.code.as_deref() == Some(lang_api::SPELLING_INSPECTION) {
                            // A misspelled word keeps no mark on the bar: the check is
                            // no problem of the project.
                            continue;
                        }
                        let kind = match mark.severity {
                            Severity::Error => StripeKind::Error,
                            Severity::Warning => StripeKind::Warning,
                            Severity::Information | Severity::Hint => continue,
                        };
                        let line = store.offset_to_line(mark.range.start.min(store.len()));
                        add_stripe(
                            &mut stripes,
                            &mut seen,
                            display,
                            bar,
                            x,
                            width,
                            kind,
                            line,
                            3.,
                            Some(mark.message.clone()),
                        );
                    }
                }
            }
            vertical = vertical_bar;
            horizontal = horizontal_bar;
        }

        let layout = Rc::new(Layout {
            bounds,
            rows,
            vertical,
            horizontal,
            vertical_area,
            horizontal_area,
            stripes,
            sticky_height: sticky_height(sticky.len(), line_height),
            inlays: inlay_paints,
        });
        let shared = layout.clone();
        let sticky_lines: Vec<usize> = sticky.iter().map(|row: &StickyRow| row.line).collect();
        self.view.update(cx, |view, _| {
            view.sticky = sticky_lines;
            view.layout = Some(shared);
            view.autoscroll = false;
            view.reveal_center = reveal_center;
            view.scroll_top = scroll_top;
            view.scroll_left = scroll_left;
            view.horizontal_bar = horizontal.is_some();
            if let Some(widest) = widest_row {
                view.widest = Some(widest);
            }
        });

        let text_mask = Bounds::from_corners(
            point(bounds.left() + px(gutter + 1.), bounds.top()),
            point(
                bounds.right() - px(scrollbar::BAND),
                bounds.bottom()
                    - if horizontal.is_some() {
                        px(scrollbar::HORIZONTAL_HEIGHT)
                    } else {
                        px(0.)
                    },
            ),
        );
        let scrollbar_hovered = self.view.read(cx).scrollbar_hovered;

        EditorPrepaint {
            layout,
            sticky,
            highlights,
            selections,
            carets,
            marked,
            underlines,
            gutter_marks,
            occurrences,
            line_tints,
            range_tints,
            hitbox,
            text_hitbox,
            char_width,
            text_x: text_x - px(scroll_left),
            gutter,
            text_mask,
            scroll_top,
            scroll_left,
            scrollbar_hovered,
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        prepaint: &mut EditorPrepaint,
        window: &mut Window,
        cx: &mut App,
    ) {
        let (style, focus_handle, options) = {
            let view = self.view.read(cx);
            (
                view.style.clone(),
                view.focus_handle.clone(),
                view.options.clone(),
            )
        };
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.view.clone()),
            cx,
        );
        window.set_cursor_style(CursorStyle::IBeam, &prepaint.text_hitbox);
        let annotate = self.view.read(cx).annotation_width();
        let marks = self.view.read(cx).gutter_marks.clone();
        let coverage = self.view.read(cx).coverage.clone();
        let breakpoints = self.view.read(cx).breakpoints.clone();
        let muted_breakpoints = self.view.read(cx).muted_breakpoints.clone();
        let temporary_breakpoints = self.view.read(cx).temporary_breakpoints.clone();
        let function_breakpoints = self.view.read(cx).function_breakpoints.clone();
        let field_watchpoints = self.view.read(cx).field_watchpoints.clone();
        let show_whitespace = self.view.read(cx).show_whitespace();
        let indent_columns = self.view.read(cx).indent_columns().max(1);
        let code_vision_hovered = self.view.read(cx).code_vision_hovered;
        let pointer = window.mouse_position();
        let over_code_vision = prepaint.layout.rows.iter().any(|row| {
            row.block_labels
                .iter()
                .any(|label| inside(label.bounds, pointer))
                || row
                    .doc_links
                    .iter()
                    .any(|link| inside(link.bounds, pointer))
        });
        if over_code_vision {
            window.set_cursor_style(CursorStyle::PointingHand, &prepaint.hitbox);
        }

        let view = self.view.clone();
        let hitbox = prepaint.hitbox.clone();
        let line_height = px(style.line_height);
        window.on_mouse_event(move |event: &ScrollEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble && hitbox.is_hovered(window) {
                let delta = event.delta.pixel_delta(line_height);
                if event.modifiers.control {
                    if delta.y != px(0.) {
                        view.update(cx, |view, cx| {
                            if view.options.wheel_zoom {
                                view.zoom_by(delta.y > px(0.), cx);
                            } else {
                                view.scroll_by(point(px(0.), delta.y), cx);
                            }
                        });
                    }
                } else if event.modifiers.shift {
                    // Shift+wheel scrolls sideways, as the IDE does.
                    view.update(cx, |view, cx| view.scroll_by(point(delta.y, px(0.)), cx));
                } else {
                    view.update(cx, |view, cx| view.scroll_by(delta, cx));
                }
            }
        });

        let view = self.view.clone();
        let hitbox = prepaint.hitbox.clone();
        window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble
                && event.button == MouseButton::Left
                && hitbox.is_hovered(window)
            {
                view.update(cx, |view, cx| {
                    window.focus(&view.focus_handle.clone(), cx);
                    view.mouse_down(event, cx);
                });
            }
        });

        let view = self.view.clone();
        let hitbox = prepaint.hitbox.clone();
        window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble
                && event.button == MouseButton::Right
                && hitbox.is_hovered(window)
            {
                view.update(cx, |view, cx| {
                    window.focus(&view.focus_handle.clone(), cx);
                    view.right_click(event, cx);
                });
            }
        });

        let view = self.view.clone();
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
            if phase == DispatchPhase::Bubble {
                view.update(cx, |view, cx| view.mouse_moved(event, cx));
            }
        });

        let view = self.view.clone();
        window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
            if phase == DispatchPhase::Bubble {
                view.update(cx, |view, cx| view.mouse_up(event.position, cx));
            }
        });

        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            let height = line_height;
            let char_width = prepaint.char_width;
            let text_x = prepaint.text_x;
            let gutter_right = bounds.left() + px(prepaint.gutter);
            let numbers_right = number_right(prepaint.gutter);
            let folds_left = fold_x(prepaint.gutter);
            let vertical = |x: Pixels| {
                fill(
                    Bounds::new(point(x, bounds.top()), size(px(1.), bounds.size.height)),
                    style.indent_guide,
                )
            };

            for (area, color) in &prepaint.line_tints {
                window.paint_quad(fill(*area, *color));
            }

            for shown in prepaint
                .layout
                .rows
                .iter()
                .filter(|shown| shown.current && options.highlight_current_line)
            {
                window.paint_quad(fill(
                    Bounds::new(
                        point(bounds.left(), shown.y),
                        size(bounds.size.width, height),
                    ),
                    style.caret_row,
                ));
            }

            // The blame column has a background of its own, so the labels stand apart
            // from the text; the gutter keeps the editor background.
            if annotate > 0. {
                window.paint_quad(fill(
                    Bounds::new(
                        point(gutter_right, bounds.top()),
                        size(px(annotate), bounds.size.height),
                    ),
                    style.caret_row,
                ));
                window.paint_quad(vertical(gutter_right + px(annotate)));
            }

            for shown in &prepaint.layout.rows {
                if shown.is_block && !shown.is_doc {
                    continue;
                }
                let line = shown.buffer_line as u32;
                // The left column of the gutter holds one mark of the line: a debug
                // mark or the icon of a run or a bookmark.
                let mut icon_taken = shown.run.is_some() || shown.bookmark;
                if options.gutter_icons && !shown.is_doc && function_breakpoints.contains(&line) {
                    // A method breakpoint is a diamond, not a filled dot.
                    icon_taken = true;
                    let suffix = if style.dark { "_dark" } else { "" };
                    let mark = Bounds::new(
                        point(bounds.left() + px(RUN_X), shown.y + px(3.)),
                        size(px(13.), px(13.)),
                    );
                    let path: SharedString =
                        format!("icons/debug/functionBreakpoint{suffix}.svg").into();
                    window
                        .paint_svg(
                            mark,
                            path,
                            None,
                            TransformationMatrix::unit(),
                            style.error,
                            cx,
                        )
                        .ok();
                } else if options.gutter_icons && !shown.is_doc && field_watchpoints.contains(&line)
                {
                    // A field watchpoint is an eye.
                    icon_taken = true;
                    let suffix = if style.dark { "_dark" } else { "" };
                    let mark = Bounds::new(
                        point(bounds.left() + px(RUN_X), shown.y + px(3.)),
                        size(px(13.), px(13.)),
                    );
                    let path: SharedString =
                        format!("icons/debug/fieldWatchpoint{suffix}.svg").into();
                    window
                        .paint_svg(
                            mark,
                            path,
                            None,
                            TransformationMatrix::unit(),
                            style.error,
                            cx,
                        )
                        .ok();
                } else if options.gutter_icons && !shown.is_doc && breakpoints.contains(&line) {
                    icon_taken = true;
                    if muted_breakpoints.contains(&line) {
                        // A point that does not stop the program is a muted point.
                        let suffix = if style.dark { "_dark" } else { "" };
                        let mark = Bounds::new(
                            point(bounds.left() + px(RUN_X), shown.y + px(3.)),
                            size(px(13.), px(13.)),
                        );
                        let path: SharedString =
                            format!("icons/debug/breakpointMuted{suffix}.svg").into();
                        window
                            .paint_svg(
                                mark,
                                path,
                                None,
                                TransformationMatrix::unit(),
                                style.error,
                                cx,
                            )
                            .ok();
                    } else {
                        let dot = Bounds::new(
                            point(bounds.left() + px(RUN_X + 2.), shown.y + px(5.)),
                            size(px(9.), px(9.)),
                        );
                        window.paint_quad(fill(dot, style.error).corner_radii(px(4.5)));
                        if temporary_breakpoints.contains(&line) {
                            // A temporary breakpoint is a ring, not a filled dot.
                            let hole = Bounds::new(
                                point(bounds.left() + px(RUN_X + 4.), shown.y + px(7.)),
                                size(px(5.), px(5.)),
                            );
                            window.paint_quad(fill(hole, style.background).corner_radii(px(2.5)));
                        }
                    }
                }
                let number_x = bounds.left() + px(numbers_right) - shown.number.width();
                if options.gutter_icons
                    && let Some((_, color)) = shown.run
                {
                    let suffix = if style.dark { "_dark" } else { "" };
                    let mark = Bounds::new(
                        point(bounds.left() + px(RUN_X), shown.y + px(3.)),
                        size(px(16.), px(16.)),
                    );
                    let path: SharedString = format!("icons/run/run{suffix}.svg").into();
                    window
                        .paint_svg(mark, path, None, TransformationMatrix::unit(), color, cx)
                        .ok();
                } else if options.gutter_icons && shown.bookmark {
                    let suffix = if style.dark { "_dark" } else { "" };
                    let mark = Bounds::new(
                        point(bounds.left() + px(RUN_X), shown.y + px(3.)),
                        size(px(16.), px(16.)),
                    );
                    let path: SharedString =
                        format!("icons/toolwindows/bookmarks{suffix}.svg").into();
                    window
                        .paint_svg(
                            mark,
                            path,
                            None,
                            TransformationMatrix::unit(),
                            style.accent,
                            cx,
                        )
                        .ok();
                } else {
                    shown
                        .number
                        .paint(
                            point(number_x, shown.y + px(2.)),
                            px(18.),
                            TextAlign::Left,
                            None,
                            window,
                            cx,
                        )
                        .ok();
                }
                // A place or a handler of the line has its badge in the column of the
                // debug marks; a breakpoint of the line takes it.
                if !icon_taken
                    && options.gutter_icons
                    && let Some((icon, color)) = &shown.place
                {
                    let mark = Bounds::new(
                        point(bounds.left() + px(ENDPOINT_X), shown.y + px(3.)),
                        size(px(13.), px(13.)),
                    );
                    window
                        .paint_svg(
                            mark,
                            icon.clone(),
                            None,
                            TransformationMatrix::unit(),
                            *color,
                            cx,
                        )
                        .ok();
                } else if !icon_taken
                    && options.gutter_icons
                    && let Some((icon, color)) = &shown.action
                {
                    let mark = Bounds::new(
                        point(bounds.left() + px(ENDPOINT_X), shown.y + px(3.)),
                        size(px(13.), px(13.)),
                    );
                    window
                        .paint_svg(
                            mark,
                            icon.clone(),
                            None,
                            TransformationMatrix::unit(),
                            *color,
                            cx,
                        )
                        .ok();
                } else if !icon_taken
                    && options.gutter_icons
                    && let Some(color) = shown.endpoint
                {
                    let suffix = if style.dark { "_dark" } else { "" };
                    let mark = Bounds::new(
                        point(bounds.left() + px(ENDPOINT_X), shown.y + px(3.)),
                        size(px(13.), px(13.)),
                    );
                    let path: SharedString =
                        format!("icons/toolwindows/services{suffix}.svg").into();
                    window
                        .paint_svg(mark, path, None, TransformationMatrix::unit(), color, cx)
                        .ok();
                }
                if options.gutter_icons
                    && let Some(rendered) = shown.doc_icon
                {
                    let name = if rendered { "hide" } else { "show" };
                    let suffix = if style.dark { "_dark" } else { "" };
                    let icon = Bounds::new(
                        point(bounds.left() + px(folds_left), shown.y + px(3.)),
                        size(px(16.), px(16.)),
                    );
                    let path: SharedString = format!("icons/general/{name}{suffix}.svg").into();
                    window
                        .paint_svg(
                            icon,
                            path,
                            None,
                            TransformationMatrix::unit(),
                            style.line_number_current,
                            cx,
                        )
                        .ok();
                }
                if options.gutter_icons
                    && let Some(collapsed) = shown.fold
                {
                    let name = if collapsed {
                        "chevronRight"
                    } else {
                        "chevronDown"
                    };
                    let suffix = if style.dark { "_dark" } else { "" };
                    let chevron = Bounds::new(
                        point(bounds.left() + px(folds_left), shown.y + px(3.)),
                        size(px(16.), px(16.)),
                    );
                    let path: SharedString = format!("icons/general/{name}{suffix}.svg").into();
                    window
                        .paint_svg(
                            chevron,
                            path,
                            None,
                            TransformationMatrix::unit(),
                            style.line_number_current,
                            cx,
                        )
                        .ok();
                }
                if let Some(label) = &shown.annotation {
                    label
                        .paint(
                            point(gutter_right + px(ANNOTATE_INSET), shown.y + px(2.)),
                            px(18.),
                            TextAlign::Left,
                            None,
                            window,
                            cx,
                        )
                        .ok();
                }
            }
            if options.gutter_icons {
                for (mark, color) in &prepaint.gutter_marks {
                    window.paint_quad(fill(*mark, *color).corner_radii(px(1.)));
                }
            }
            if prepaint.gutter > 0. {
                window.paint_quad(vertical(gutter_right));
            }
            if prepaint.gutter > 0. && options.gutter_icons {
                for shown in &prepaint.layout.rows {
                    if shown.is_block {
                        continue;
                    }
                    if let Some(mark) = coverage
                        .iter()
                        .find(|mark| mark.line as usize == shown.buffer_line)
                    {
                        // The stripe sits left of the run marks, so a program that starts on
                        // the line keeps its button.
                        let stripe = Bounds::new(
                            point(bounds.left() + px(2.), shown.y),
                            size(px(3.), height),
                        );
                        window.paint_quad(fill(stripe, mark.color));
                    }
                }
                paint_marks(&prepaint.layout.rows, &marks, gutter_right, height, window);
            }

            let text_mask = prepaint.text_mask;
            window.with_content_mask(Some(ContentMask { bounds: text_mask }), |window| {
                window.paint_quad(vertical(
                    text_x + char_width * options.right_margin.max(1) as f32,
                ));

                for (area, color) in &prepaint.range_tints {
                    window.paint_quad(fill(*area, *color));
                }
                for (area, color) in &prepaint.highlights {
                    window.paint_quad(fill(*area, *color));
                }
                for occurrence in &prepaint.occurrences {
                    window.paint_quad(fill(*occurrence, style.occurrence));
                }
                for selection in &prepaint.selections {
                    window.paint_quad(fill(*selection, style.selection));
                }

                for shown in &prepaint.layout.rows {
                    if shown.is_doc {
                        window.paint_quad(fill(
                            Bounds::new(
                                point(text_x, shown.y),
                                size(prepaint.text_mask.size.width, height),
                            ),
                            style.doc_background,
                        ));
                        shown
                            .text
                            .paint(
                                point(text_x + prepaint.char_width * shown.indent as f32, shown.y),
                                height,
                                TextAlign::Left,
                                None,
                                window,
                                cx,
                            )
                            .ok();
                        for link in &shown.doc_links {
                            let underline = Bounds::new(
                                point(link.bounds.origin.x, link.bounds.origin.y + height - px(1.)),
                                size(link.bounds.size.width, px(1.)),
                            );
                            window.paint_quad(fill(underline, style.accent));
                        }
                        continue;
                    }
                    if shown.is_block {
                        for (index, label) in shown.block_labels.iter().enumerate() {
                            label
                                .text
                                .paint(
                                    label.bounds.origin,
                                    height,
                                    TextAlign::Left,
                                    None,
                                    window,
                                    cx,
                                )
                                .ok();
                            if code_vision_hovered == Some((shown.row, index)) {
                                let underline = Bounds::new(
                                    point(
                                        label.bounds.origin.x,
                                        label.bounds.origin.y + height - px(2.),
                                    ),
                                    size(label.bounds.size.width, px(1.)),
                                );
                                window.paint_quad(fill(underline, style.code_vision));
                            }
                        }
                        continue;
                    }
                    if options.indent_guides {
                        for level in 1..(shown.indent / indent_columns) {
                            let x = text_x + char_width * (level * indent_columns) as f32;
                            window.paint_quad(fill(
                                Bounds::new(point(x, shown.y), size(px(1.), height)),
                                style.indent_guide,
                            ));
                        }
                    }
                    for (range, chunk) in &shown.line.chunks {
                        let background = match chunk {
                            DisplayChunk::Placeholder { .. } => style.fold_background,
                            // The pill of a hint is painted with its label below.
                            _ => continue,
                        };
                        let left = shown.text.x_for_index(range.start);
                        let right = shown.text.x_for_index(range.end);
                        let area = Bounds::new(
                            point(text_x + left - px(1.5), shown.y + px(1.)),
                            size(right - left + px(3.), height - px(2.)),
                        );
                        window.paint_quad(fill(area, background).corner_radii(px(3.)));
                    }
                    shown
                        .text
                        .paint(
                            point(text_x, shown.y),
                            height,
                            TextAlign::Left,
                            None,
                            window,
                            cx,
                        )
                        .ok();
                    if let Some(tail) = &shown.tail {
                        tail.paint(
                            point(text_x + shown.text.width() + px(8.), shown.y),
                            height,
                            TextAlign::Left,
                            None,
                            window,
                            cx,
                        )
                        .ok();
                    }
                    if let Some(watch) = &shown.watch {
                        watch
                            .text
                            .paint(
                                point(watch.text_x, shown.y),
                                height,
                                TextAlign::Left,
                                None,
                                window,
                                cx,
                            )
                            .ok();
                        watch
                            .cross
                            .paint(
                                point(watch.cross_x, shown.y),
                                height,
                                TextAlign::Left,
                                None,
                                window,
                                cx,
                            )
                            .ok();
                    }
                    for inlay in &shown.inlays {
                        let Some(paint) = &inlay.paint else {
                            continue;
                        };
                        window.paint_quad(
                            fill(paint.pill, style.inlay_background).corner_radii(px(3.)),
                        );
                        inlay
                            .text
                            .paint(
                                point(paint.label_x, shown.y),
                                inlay.height,
                                TextAlign::Left,
                                None,
                                window,
                                cx,
                            )
                            .ok();
                    }
                    if show_whitespace {
                        let dim = Hsla {
                            a: 0.5,
                            ..style.line_number
                        };
                        for (range, chunk) in &shown.line.chunks {
                            match chunk {
                                DisplayChunk::Text { .. } => {
                                    let text = &shown.line.text[range.clone()];
                                    for (at, _) in text.match_indices(' ') {
                                        let x = shown.text.x_for_index(range.start + at);
                                        window.paint_quad(fill(
                                            Bounds::new(
                                                point(
                                                    text_x + x + char_width / 2. - px(1.),
                                                    shown.y + height / 2. - px(1.),
                                                ),
                                                size(px(2.), px(2.)),
                                            ),
                                            dim,
                                        ));
                                    }
                                }
                                DisplayChunk::Tab { .. } => {
                                    let left = shown.text.x_for_index(range.start);
                                    let right = shown.text.x_for_index(range.end);
                                    window.paint_quad(fill(
                                        Bounds::new(
                                            point(text_x + left + px(1.), shown.y + height / 2.),
                                            size((right - left - px(3.)).max(px(2.)), px(1.)),
                                        ),
                                        dim,
                                    ));
                                }
                                _ => {}
                            }
                        }
                    }
                }

                for (underline, color) in &prepaint.underlines {
                    window.paint_quad(fill(*underline, *color));
                }
                if let Some(marked) = prepaint.marked {
                    window.paint_quad(fill(marked, style.code));
                }
                for caret in &prepaint.carets {
                    window.paint_quad(fill(*caret, style.caret));
                }
            });

            if !prepaint.sticky.is_empty() {
                let panel_height = px(prepaint.layout.sticky_height);
                // The panel is opaque, so no text shows through it, and one thin line
                // closes it at the bottom instead of a line under every pinned row.
                window.paint_quad(fill(
                    Bounds::new(
                        point(bounds.left(), bounds.top()),
                        size(bounds.size.width, panel_height),
                    ),
                    style.background,
                ));
                for (index, row) in prepaint.sticky.iter().enumerate() {
                    let y = bounds.top() + height * index as f32;
                    let number_x = bounds.left() + px(numbers_right) - row.number.width();
                    row.number
                        .paint(
                            point(number_x, y + px(2.)),
                            px(18.),
                            TextAlign::Left,
                            None,
                            window,
                            cx,
                        )
                        .ok();
                    let clip = Bounds::from_corners(
                        point(gutter_right + px(1.), y),
                        point(bounds.right(), y + height),
                    );
                    window.with_content_mask(Some(ContentMask { bounds: clip }), |window| {
                        row.text
                            .paint(
                                point(gutter_right + px(TEXT_INSET), y),
                                height,
                                TextAlign::Left,
                                None,
                                window,
                                cx,
                            )
                            .ok();
                    });
                }
                window.paint_quad(fill(
                    Bounds::new(
                        point(bounds.left(), bounds.top() + panel_height - px(1.)),
                        size(bounds.size.width, px(1.)),
                    ),
                    style.indent_guide,
                ));
            }

            paint_scrollbars(prepaint, &style, window);
        });
    }
}

/// The thin bar at rest, the wide one under the pointer, and the marks on it.
fn paint_scrollbars(prepaint: &EditorPrepaint, style: &EditorStyle, window: &mut Window) {
    let layout = &prepaint.layout;
    if let (Some(bar), Some(area)) = (layout.vertical, layout.vertical_area) {
        let width = if prepaint.scrollbar_hovered {
            scrollbar::HOVER_WIDTH
        } else {
            scrollbar::WIDTH
        };
        let x = f32::from(area.right()) - width;
        let track = Bounds::new(point(px(x), area.top()), size(px(width), area.size.height));
        window.paint_quad(fill(track, style.scrollbar_track).corner_radii(px(width / 2.)));
        for mark in &layout.stripes {
            let color = match mark.kind {
                StripeKind::Error => style.stripe_error,
                StripeKind::Warning => style.stripe_warning,
                StripeKind::Find => style.stripe_find,
                StripeKind::Caret => style.stripe_caret,
                StripeKind::Vcs => mark.color.unwrap_or(style.stripe_caret),
            };
            window.paint_quad(fill(mark.bounds, color).corner_radii(px(1.)));
        }
        if let Some(thumb) = bar.thumb(prepaint.scroll_top) {
            let color = if prepaint.scrollbar_hovered {
                style.scrollbar_thumb_hover
            } else {
                style.scrollbar_thumb
            };
            let thumb = Bounds::new(
                point(px(x), px(bar.track_start + thumb.offset)),
                size(px(width), px(thumb.len)),
            );
            window.paint_quad(fill(thumb, color).corner_radii(px(width / 2.)));
        }
    }
    if let (Some(bar), Some(area)) = (layout.horizontal, layout.horizontal_area) {
        let height = if prepaint.scrollbar_hovered {
            scrollbar::HORIZONTAL_HOVER_HEIGHT
        } else {
            scrollbar::HORIZONTAL_HEIGHT
        };
        let y = f32::from(area.bottom()) - height;
        let track = Bounds::new(
            point(px(bar.track_start), px(y)),
            size(px(bar.track_len), px(height)),
        );
        window.paint_quad(fill(track, style.scrollbar_track).corner_radii(px(height / 2.)));
        if let Some(thumb) = bar.thumb(prepaint.scroll_left) {
            let thumb = Bounds::new(
                point(px(bar.track_start + thumb.offset), px(y)),
                size(px(thumb.len), px(height)),
            );
            window.paint_quad(fill(thumb, style.scrollbar_thumb).corner_radii(px(height / 2.)));
        }
    }
}

#[cfg(test)]
mod checks {
    use syntax::{HighlightKind, HighlightSpan};
    use ui_kit::{Font, font};

    use crate::style::EditorStyle;
    use crate::view::{OutlineItem, OutlineKind};

    use super::{STICKY_MAX, pinned_items, push_text_runs};

    #[test]
    fn the_gutter_grows_with_the_digits_of_the_line_count() {
        let digit = 7.02_f32;
        let mut previous = 0.;
        for (lines, digits) in [(9, 1), (99, 2), (999, 3), (5238, 4), (99999, 5)] {
            assert_eq!(super::number_digits(lines), digits, "{lines} lines");
            let width = super::gutter_width(digits as f32 * digit);
            assert!(width >= super::GUTTER_WIDTH, "{lines} lines: {width}");
            if digits > 3 {
                assert!(
                    width > previous,
                    "{lines} lines grow the column: {width} after {previous}"
                );
            }
            previous = width;
        }
        // The shortest files keep the width the editor had, and a fourth digit grows it.
        assert_eq!(super::number_digits(1), 1);
        assert_eq!(super::gutter_width(3. * digit), super::GUTTER_WIDTH);
        assert!(super::gutter_width(4. * digit) > super::GUTTER_WIDTH);
        assert!(super::gutter_width(5. * digit) > super::gutter_width(4. * digit));
    }

    fn span(start: u32, end: u32) -> HighlightSpan {
        HighlightSpan {
            start,
            end,
            kind: HighlightKind::Keyword,
        }
    }

    fn covered(spans: &[HighlightSpan], range: std::ops::Range<usize>) -> Vec<usize> {
        let font: Font = font("Consolas");
        let mut runs = Vec::new();
        push_text_runs(&mut runs, range, spans, &EditorStyle::light(), &font);
        runs.iter().map(|run| run.len).collect()
    }

    #[test]
    fn runs_cover_the_line_once_whatever_the_spans() {
        // Overlapping spans, a span that runs backwards and a stale one past the line.
        let spans = [
            span(10, 30),
            span(12, 25),
            span(20, 40),
            span(35, 33),
            span(90, 200),
        ];
        let lengths = covered(&spans, 10..60);
        assert_eq!(lengths.iter().sum::<usize>(), 50, "{lengths:?}");
        assert!(lengths.iter().all(|len| *len > 0), "{lengths:?}");

        let lengths = covered(&[span(0, 1000)], 40..54);
        assert_eq!(lengths, [14]);
        assert_eq!(covered(&[], 5..9), [4]);
    }

    fn item(name: &str, kind: OutlineKind, depth: u32, start: usize, end: usize) -> OutlineItem {
        OutlineItem {
            name: name.into(),
            signature: None,
            kind,
            depth,
            range: start..end,
            name_range: start..start + name.len(),
        }
    }

    /// One offset per line: a line is its offset divided by ten.
    fn line_of(offset: usize) -> usize {
        offset / 10
    }

    #[test]
    fn a_class_and_its_method_stay_while_the_body_is_read() {
        let outline = [
            item("Main", OutlineKind::Class, 0, 0, 200),
            item("size", OutlineKind::Field, 1, 20, 30),
            item("run", OutlineKind::Method, 1, 40, 120),
            item("other", OutlineKind::Method, 1, 130, 190),
        ];
        let names = |top| -> Vec<_> {
            pinned_items(&outline, top, line_of)
                .iter()
                .map(|i| i.name.as_str())
                .collect()
        };
        assert_eq!(names(0), Vec::<&str>::new());
        assert_eq!(names(6), ["Main", "run"]);
        assert_eq!(names(14), ["Main", "other"]);
        assert_eq!(names(25), Vec::<&str>::new());
    }

    #[test]
    fn only_the_innermost_declarations_are_pinned() {
        let outline: Vec<_> = (0..8)
            .map(|depth| item("c", OutlineKind::Class, depth, depth as usize * 10, 500))
            .collect();
        let pinned = pinned_items(&outline, 20, line_of);
        assert_eq!(pinned.len(), STICKY_MAX);
        assert_eq!(pinned[0].depth, 4);
    }
}

#[cfg(test)]
mod token_checks {
    use lang_api::{SemanticTokenKind, SemanticTokenModifiers};
    use syntax::{HighlightKind, HighlightSpan};
    use ui_kit::{Font, FontStyle, FontWeight, TextRun, font};

    use crate::semantic::TokenSpan;
    use crate::style::EditorStyle;

    use super::runs_in;

    fn span(start: u32, end: u32, kind: HighlightKind) -> HighlightSpan {
        HighlightSpan { start, end, kind }
    }

    fn token(
        start: u32,
        end: u32,
        kind: SemanticTokenKind,
        mods: SemanticTokenModifiers,
    ) -> TokenSpan {
        TokenSpan {
            start,
            end,
            kind,
            modifiers: mods,
        }
    }

    fn runs(
        spans: &[HighlightSpan],
        tokens: &[TokenSpan],
        unnecessary: &[std::ops::Range<usize>],
        range: std::ops::Range<usize>,
        style: &EditorStyle,
    ) -> Vec<TextRun> {
        let font: Font = font("Consolas");
        let mut runs = Vec::new();
        runs_in(&mut runs, range, spans, tokens, unnecessary, style, &font);
        runs
    }

    #[test]
    fn a_semantic_token_paints_over_the_syntax_span() {
        let style = EditorStyle::light();
        let spans = [span(0, 6, HighlightKind::Keyword)];
        let tokens = [token(
            0,
            6,
            SemanticTokenKind::Property,
            SemanticTokenModifiers::default(),
        )];
        let out = runs(&spans, &tokens, &[], 0..6, &style);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].color, style.semantic_field);
        assert_eq!(out[0].font.style, FontStyle::Normal);
        assert!(out[0].strikethrough.is_none());
    }

    #[test]
    fn markdown_markup_draws_bold_italic_code_and_links() {
        let style = EditorStyle::light();
        let spans = [
            span(0, 1, HighlightKind::Strong),
            span(1, 2, HighlightKind::Emphasis),
            span(2, 3, HighlightKind::InlineCode),
            span(3, 4, HighlightKind::Link),
        ];
        let out = runs(&spans, &[], &[], 0..4, &style);
        assert_eq!(out.len(), 4, "{out:?}");
        assert_eq!(out[0].color, style.code);
        assert_eq!(out[0].font.weight, FontWeight::SEMIBOLD);
        assert_eq!(out[1].color, style.code);
        assert_eq!(out[1].font.style, FontStyle::Italic);
        assert_eq!(out[2].color, style.string);
        assert_eq!(out[2].background_color, Some(style.inline_code_background));
        assert_eq!(out[3].color, style.accent);
    }

    #[test]
    fn a_static_member_is_italic_and_a_deprecated_one_is_struck_through() {
        let style = EditorStyle::light();
        let tokens = [
            token(
                0,
                4,
                SemanticTokenKind::Method,
                SemanticTokenModifiers {
                    static_member: true,
                    ..SemanticTokenModifiers::default()
                },
            ),
            token(
                4,
                8,
                SemanticTokenKind::Method,
                SemanticTokenModifiers {
                    deprecated: true,
                    ..SemanticTokenModifiers::default()
                },
            ),
        ];
        let out = runs(&[], &tokens, &[], 0..8, &style);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].font.style, FontStyle::Italic);
        assert!(out[0].strikethrough.is_none());
        assert_eq!(out[1].font.style, FontStyle::Normal);
        assert!(out[1].strikethrough.is_some());
        assert_eq!(out[1].color, style.semantic_method);
    }

    #[test]
    fn an_unnecessary_range_is_grey_over_the_token_and_the_span() {
        let style = EditorStyle::light();
        let spans = [span(0, 8, HighlightKind::Function)];
        let tokens = [token(
            0,
            8,
            SemanticTokenKind::Method,
            SemanticTokenModifiers::default(),
        )];
        let unnecessary = vec![std::ops::Range { start: 2, end: 5 }];
        let out = runs(&spans, &tokens, &unnecessary, 0..8, &style);
        assert_eq!(out.iter().map(|run| run.len).sum::<usize>(), 8);
        let middle = out
            .iter()
            .find(|run| run.color == style.unused)
            .expect("the grey part");
        assert_eq!(middle.len, 3);
    }

    #[test]
    fn the_runs_cover_the_line_once_with_both_lists() {
        let style = EditorStyle::light();
        let spans = [
            span(0, 10, HighlightKind::Keyword),
            span(8, 12, HighlightKind::Field),
        ];
        let tokens = [
            token(
                0,
                4,
                SemanticTokenKind::Parameter,
                SemanticTokenModifiers::default(),
            ),
            token(
                12,
                20,
                SemanticTokenKind::Variable,
                SemanticTokenModifiers::default(),
            ),
        ];
        let out = runs(&spans, &tokens, &[], 0..16, &style);
        assert_eq!(out.iter().map(|run| run.len).sum::<usize>(), 16);
        assert!(out.iter().all(|run| run.len > 0));
    }
}
