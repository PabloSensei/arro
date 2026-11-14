//! The view: owns the document, turns toolkit events into commands.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use ::base::perf;

use display_map::{Bias, DisplayChunk, DisplayPoint};
use futures_channel::mpsc::unbounded;
use futures_util::StreamExt;
use keymap::{Keymap, Keystroke, Lookup, default_keymap, editor as actions};
use lang_api::{DiagnosticEntry, NavigationKind, Severity, SourceSpan};
use syntax::{DocumentId, HighlightSpan, LanguageConfig, SyntaxService};
use templates::{ExpandWith, FileTemplateDefinition, TemplateLanguage, TemplateOverrides};
use text_core::{Change, FileFormat, RopeStore, Selection, TextStore, read_file};
use ui_kit::gpui::EntityId;
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;

use crate::annotate::BlameNote;
use crate::bar::{Bar, LastFind};
use crate::blocks::{brace_pairs, enclosing_block};
use crate::element::{ANNOTATE_WIDTH, EditorElement, GUTTER_WIDTH, Layout, StripeKind, TEXT_INSET};
use crate::live::LiveTemplates;
use crate::refactor::Refactoring;
use crate::scrollbar::{Axis, Drag};
use crate::semantic::{CompletionMode, Lsp, Reveals, signature_segments};
use crate::state::{Conflict, EditorState, LoadError, Saved};
use crate::style::EditorStyle;

const BLINK: Duration = Duration::from_millis(530);

/// Names the syntax documents of the views that paint text outside an editor.
static NEXT_SOURCE: AtomicU64 = AtomicU64::new(1);

/// Which language a file is. Set by the application, the editor knows none.
pub struct Languages {
    pub resolve: fn(&Path) -> Option<LanguageConfig>,
    /// Live templates of the file's language.
    pub templates: fn(&Path) -> Option<&'static TemplateLanguage>,
}

impl Global for Languages {}

/// The live templates and the new-file templates of the user, over the built-ins of the
/// languages. The Settings window writes it; every editor reads it before a template key.
#[derive(Clone, Default)]
pub struct TemplateSettings {
    /// The language whose built-in templates the Settings window shows.
    pub language: Option<&'static TemplateLanguage>,
    /// The changes of `templates.json`.
    pub overrides: std::sync::Arc<TemplateOverrides>,
    /// The new-file templates with the text in force.
    pub files: Vec<FileTemplateDefinition>,
    /// Bumped on every change, so an editor resolves the templates once.
    pub generation: usize,
}

impl Global for TemplateSettings {}

#[derive(Default)]
struct SyntaxThread(SyntaxService);

impl Global for SyntaxThread {}

actions!(
    editor_view,
    [
        /// The editor takes the group keys of the window while it has the focus.
        MoveElementLeft,
        MoveElementRight,
    ]
);

struct Keys(Rc<Keymap>);

/// Puts a keymap in place for every editor, including the open ones: a keymap file of
/// the user changes what the editor runs at once.
pub fn install_keymap(cx: &mut App, keymap: Rc<Keymap>) {
    cx.set_global(Keys(keymap));
}

/// The syntax colours of `text` as the editor of `path` would paint them, for a view
/// that shows the same file or an earlier version of it outside the editor. `None`
/// when the path has no grammar.
pub fn highlight_text(path: &Path, text: String, cx: &mut App) -> Option<Task<Vec<HighlightSpan>>> {
    let language = cx
        .try_global::<Languages>()
        .and_then(|languages| (languages.resolve)(path))?;
    let syntax = cx.default_global::<SyntaxThread>().0.clone();
    Some(cx.background_executor().spawn(async move {
        let id = DocumentId(NEXT_SOURCE.fetch_add(1, Ordering::Relaxed));
        let (sender, mut receiver) = unbounded();
        syntax.open(id, language, move |snapshot| {
            sender.unbounded_send(snapshot).ok();
        });
        syntax.update(id, 1, RopeStore::from_text(&text), &Change::default());
        let snapshot = receiver.next().await;
        syntax.close(id);
        snapshot.map_or_else(Vec::new, |snapshot| snapshot.highlights)
    }))
}

impl Global for Keys {}

/// What was copied in this window, newest first, for Ctrl+Shift+V.
#[derive(Default)]
struct ClipHistory(Vec<String>);

impl Global for ClipHistory {}

/// The texts of the editors open in the window, for hippie completion. Each editor puts
/// its own text here as it changes and reads the others from here.
#[derive(Default)]
struct OpenTexts(HashMap<EntityId, OpenText>);

struct OpenText {
    path: PathBuf,
    view: WeakEntity<EditorView>,
    store: RopeStore,
}

impl Global for OpenTexts {}

const CLIP_HISTORY: usize = 20;
/// Smallest and largest font scale of Ctrl+wheel.
const ZOOM: (f32, f32) = (0.5, 4.);

pub fn clipboard_history(cx: &App) -> Vec<String> {
    cx.try_global::<ClipHistory>()
        .map(|history| history.0.clone())
        .unwrap_or_default()
}

fn remember_clip(cx: &mut App, text: String) {
    if text.is_empty() {
        return;
    }
    let history = &mut cx.default_global::<ClipHistory>().0;
    history.retain(|kept| *kept != text);
    history.insert(0, text);
    history.truncate(CLIP_HISTORY);
}

/// What a declaration is, for the breadcrumbs, the sticky lines and the structure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutlineKind {
    Module,
    Class,
    Interface,
    Enum,
    Function,
    Method,
    Constructor,
    Field,
    Constant,
    Other,
}

impl OutlineKind {
    /// A declaration with a body: its first line stays in view while the body is read.
    pub fn is_block(self) -> bool {
        matches!(
            self,
            Self::Class
                | Self::Interface
                | Self::Enum
                | Self::Function
                | Self::Method
                | Self::Constructor
        )
    }
}

/// The declarations that contain `offset`, outermost first.
pub(crate) fn around(outline: &[OutlineItem], offset: usize) -> Vec<&OutlineItem> {
    let mut around: Vec<_> = outline
        .iter()
        .filter(|item| item.range.start <= offset && offset <= item.range.end)
        .collect();
    around.sort_by_key(|item| (item.range.start, std::cmp::Reverse(item.range.end)));
    around
}

/// A declaration of the file, as the language reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutlineItem {
    pub name: String,
    /// The declaration with its type and parameters, when the language gives them:
    /// `process(int, Owner): String`.
    pub signature: Option<String>,
    pub kind: OutlineKind,
    pub depth: u32,
    /// The whole declaration.
    pub range: std::ops::Range<usize>,
    pub name_range: std::ops::Range<usize>,
}

/// One row of the last frame, for a test of what stands where: the line it shows, the top
/// of the row and the number the gutter gives it. A block row holds a label of its own.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RowPaint {
    /// The buffer line of the row; a block speaks for the line under it.
    pub line: usize,
    /// The top of the row in pixels below the top of the editor.
    pub top: f32,
    /// The number the gutter shows, when it shows one.
    pub number: Option<usize>,
    pub block: bool,
}

/// A hint in the text of the last frame: the columns it takes and the pill behind it.
#[derive(Clone, Debug, PartialEq)]
pub struct InlayPaint {
    /// The buffer line of the row that holds the hint.
    pub line: usize,
    /// The columns the hint takes in the row text.
    pub columns: std::ops::Range<usize>,
    /// Where those columns begin and end, in window pixels.
    pub columns_left: Pixels,
    pub columns_right: Pixels,
    /// The pill behind the label, never wider than the columns of the hint.
    pub pill: Bounds<Pixels>,
    /// Where the label itself begins.
    pub label_x: Pixels,
    /// The baseline of the label below the top of the row.
    pub baseline: f32,
    /// The baseline of the code of the same row, below the top of the row.
    pub code_baseline: f32,
}

impl OutlineItem {
    /// The row of the Structure tool window: the signature when the language has one.
    pub fn label(&self) -> &str {
        self.signature.as_deref().unwrap_or(&self.name)
    }
}

/// A declaration whose name starts on a line, as the gutter chooses the kind of a
/// breakpoint there. `owner` is the type that declares it, when the language names one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineDeclaration {
    /// Buffer line from zero.
    pub line: usize,
    pub name: String,
    pub owner: Option<String>,
    pub kind: OutlineKind,
}

/// The bookmarks of all files seen in this window, by path, lines from zero.
#[derive(Default)]
struct BookmarkStore(std::collections::HashMap<PathBuf, Vec<(u32, Option<char>)>>);

impl Global for BookmarkStore {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BookmarkEntry {
    pub path: PathBuf,
    pub line: u32,
    pub mnemonic: Option<char>,
}

/// Every bookmark, files in path order.
pub fn bookmarks(cx: &App) -> Vec<BookmarkEntry> {
    let Some(store) = cx.try_global::<BookmarkStore>() else {
        return Vec::new();
    };
    let mut entries: Vec<_> = store
        .0
        .iter()
        .flat_map(|(path, lines)| {
            lines.iter().map(|(line, mnemonic)| BookmarkEntry {
                path: path.clone(),
                line: *line,
                mnemonic: *mnemonic,
            })
        })
        .collect();
    entries.sort_by(|a, b| (&a.path, a.line).cmp(&(&b.path, b.line)));
    entries
}

/// What a run mark says about its test.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunMarkState {
    Ready,
    Running,
    Passed,
    Failed,
}

/// A button in the gutter that runs what starts on its line. The colour is the caller's.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RunMark {
    /// Buffer line from zero.
    pub line: u32,
    pub state: RunMarkState,
    pub color: Hsla,
}

/// What a changed line looks like in the gutter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GutterKind {
    Added,
    Modified,
    /// Lines were removed after the line the range starts at.
    Deleted,
}

/// A mark beside the line numbers. `lines` are buffer lines from zero, end excluded;
/// the colour is the caller's, the view does not know what the marks mean.
#[derive(Clone, Debug, PartialEq)]
pub struct GutterMark {
    pub lines: std::ops::Range<u32>,
    pub kind: GutterKind,
    pub color: Hsla,
}

/// One use of the symbol under the caret, for the usages window.
#[derive(Clone, Debug)]
pub struct Usage {
    pub name: String,
    pub target: SourceSpan,
}

/// One use of the symbol with what the language knows about it, for the grouping and
/// the filters of the Usages window.
#[derive(Clone, Debug)]
pub struct DetailedUsage {
    pub name: String,
    pub target: SourceSpan,
    pub kind: lang_api::UsageKind,
    /// The declarations around the use, outermost first, joined by dots.
    pub container: Option<String>,
    /// The module of the file the use is in.
    pub module: Option<String>,
}

/// What Show Usages has found so far. The popup opens on the first update and fills
/// in as later answers arrive; `done` marks the end of the search.
#[derive(Clone, Debug)]
pub struct UsagesUpdate {
    pub name: String,
    pub usages: Vec<Usage>,
    pub done: bool,
}

#[derive(Clone, Debug)]
pub enum EditorEvent {
    Loaded,
    Dirty(bool),
    Saved,
    Failed(SharedString),
    /// The file changed on disk while the buffer has unsaved edits.
    Conflict,
    /// A clean buffer took text from disk that differs from what it showed. The bytes
    /// are the text before the change, for the Local History of the window.
    ExternalChange(Vec<u8>),
    /// Go to definition asks the workspace to show this file. The caret position
    /// waits in the editor until the file is shown.
    Navigate(PathBuf),
    /// Ctrl+S: the window saves every open file, as the IDE does.
    SaveAll,
    /// A refactoring that reaches other files or moves files; the window applies it.
    ApplyChange(lang_api::WorkspaceChange),
    /// Ctrl+F6: the window shows the Change Signature form for this place.
    ChangeSignature(lang_api::DocumentSpot),
    /// Refactor This picked an action of the server that needs values: the window shows
    /// its form, and the handle names the action for the language.
    RefactorForm(String),
    /// Refactor This picked Copy: the window copies the current file, as F5 does.
    CopyElement,
    /// Ctrl+Q repeated or the pin button: the window shows the Documentation tool window.
    ShowDocumentation,
    /// Ctrl+Alt+H: the window shows the call hierarchy of this place.
    CallHierarchy(lang_api::DocumentSpot),
    /// Ctrl+Shift+H: the window shows the hierarchy of the method at this place.
    MethodHierarchy(lang_api::DocumentSpot),
    /// Safe Delete of a type alone in its file: the window removes the file too.
    DeleteFile(PathBuf),
    /// A generation created files: the window writes them and opens the first one.
    CreateFiles(Vec<lang_api::GeneratedFile>),
    /// Ctrl+Alt+L: the window formats with the flags of the Reformat File dialog.
    Reformat,
    /// The text changed, by typing or from outside.
    Changed,
    /// The caret or the selection moved without changing the text.
    Moved,
    /// A run mark was clicked: the line it stands on, from zero.
    RunMark(u32),
    /// Alt+click on a run mark: run the same test under the debugger.
    DebugRunMark(u32),
    /// The gutter was clicked away from the fold column: the line, from zero.
    GutterClick(u32),
    /// A blame label was clicked: the window shows this commit in the history.
    BlameCommit(String),
    /// Ctrl+Shift+V: the window shows what was copied, [`EditorView::paste_text`] takes the pick.
    PasteHistory,
    /// Shift+F11: the window lists [`bookmarks`].
    ShowBookmarks,
    /// Ctrl+digit went to a bookmark of another file.
    OpenBookmark {
        path: PathBuf,
        line: u32,
    },
    /// The language server sent the diagnostics of the file, empty when it has none.
    Diagnostics(Vec<DiagnosticEntry>),
    /// Alt+F7: the uses of the symbol, for the window that lists them by file.
    Usages(Vec<Usage>),
    /// Alt+F7 with the language that tells the kind and the container of a use.
    UsagesDetailed(Vec<DetailedUsage>),
    /// Ctrl+Alt+F7: the uses of the symbol, for the popup at the caret.
    ShowUsages(UsagesUpdate),
    /// A right click in the text: the window shows the menu of the editor here.
    ContextMenu(Point<Pixels>),
    /// The text scrolled: the first visible row in pixels, for a view that follows it.
    Scrolled(f32),
}

/// What the Documentation tool window asks about the symbol at the caret: the place, the
/// problem above it and the language that answers.
pub struct DocumentationQuery {
    pub spot: lang_api::DocumentSpot,
    pub offset: usize,
    pub diagnostic: Option<String>,
    pub support: std::sync::Arc<dyn lang_api::LanguageSupport>,
}

/// A place in a file to move the caret to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Spot {
    /// Bytes from the start of the text.
    Offset(usize),
    /// 0-based, the column in characters.
    Line { line: usize, column: usize },
}

/// The numbers the problem widget shows, and whether the server has answered yet.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProblemSummary {
    pub errors: usize,
    pub warnings: usize,
    pub analyzing: bool,
}

/// The declaration header that Context Info shows while the caret stays in its body.
struct ContextInfo {
    /// The caret when the popup opened; a move closes it.
    head: usize,
    lines: Vec<String>,
}

/// A walk through the words of hippie completion: the typed prefix stays, the word that
/// replaced it changes on every key.
struct HippieCycle {
    /// The word inserted now, and where it sits.
    range: std::ops::Range<usize>,
    word: String,
    /// Every candidate in the forward order, so both directions walk the same list.
    words: Vec<String>,
    index: usize,
}

/// The behaviour of an open editor that comes from Settings. The colours and the size
/// stay in [`EditorStyle`]; this is what a settings change flips without a new file.
#[derive(Clone, Debug, PartialEq)]
pub struct EditorOptions {
    /// Numbers in the gutter.
    pub line_numbers: bool,
    /// The background of the caret line.
    pub highlight_current_line: bool,
    /// The vertical lines at the steps of the indentation.
    pub indent_guides: bool,
    /// The caret blinks while the editor has the focus.
    pub caret_blink: bool,
    /// Ctrl+wheel changes the font size rather than scrolling.
    pub wheel_zoom: bool,
    /// What a save does with the spaces and tabs at the end of a line.
    pub save: crate::state::SaveOptions,
    /// How the completion list compares the typed prefix with its entries.
    pub completion_case: crate::semantic::CompletionCase,
    /// The completion list opens while the user types.
    pub completion_auto_popup: bool,
    /// The wait before that list opens, in milliseconds.
    pub completion_delay_ms: u64,
    /// Columns a tab takes on screen.
    pub tab_size: usize,
    /// Columns of one indentation step.
    pub indent_size: usize,
    /// A new file indents with tabs rather than spaces.
    pub use_tabs: bool,
    /// The tokens of the language server colour the names over the syntax colours.
    pub semantic_highlighting: bool,
    /// Which parameter names the server shows as hints.
    pub inlay_parameter_names: lang_api::InlayParameterNames,
    /// The type hints of the server.
    pub inlay_type_hints: bool,
    /// The documentation popup opens under the pointer after a delay.
    pub quick_doc_on_hover: bool,
    /// The number of usages of a declaration above it.
    pub code_vision_usages: bool,
    /// The implementations or inheritors of a declaration above it.
    pub code_vision_implementations: bool,
    /// Documentation comments of the language show as rendered text.
    pub render_doc_comments: bool,
}

impl Default for EditorOptions {
    fn default() -> Self {
        Self {
            line_numbers: true,
            highlight_current_line: true,
            indent_guides: true,
            caret_blink: true,
            wheel_zoom: true,
            save: crate::state::SaveOptions::default(),
            completion_case: crate::semantic::CompletionCase::None,
            completion_auto_popup: true,
            completion_delay_ms: 0,
            tab_size: 4,
            indent_size: 4,
            use_tabs: false,
            semantic_highlighting: true,
            inlay_parameter_names: lang_api::InlayParameterNames::Literals,
            inlay_type_hints: false,
            quick_doc_on_hover: true,
            code_vision_usages: true,
            code_vision_implementations: true,
            render_doc_comments: false,
        }
    }
}

pub struct EditorView {
    pub(crate) path: PathBuf,
    pub(crate) state: Option<EditorState>,
    pub(crate) style: EditorStyle,
    base_style: EditorStyle,
    zoom: f32,
    /// What Settings turned on and off in this editor.
    pub(crate) options: EditorOptions,
    /// Grows with every typed key, so a late completion timer does nothing.
    pub(crate) completion_typed: u64,
    /// Grows with every semantic token request, so a late answer is dropped.
    pub(crate) tokens_asked: u64,
    /// The name under the pointer, and a number that drops a late hover answer.
    pub(crate) hover_word: Option<std::ops::Range<usize>>,
    pub(crate) hover_serial: u64,
    /// A number that drops a late close of the hover popup.
    pub(crate) hover_close: u64,
    /// Grows with every quick documentation popup, for late syntax answers.
    pub(crate) quick_serial: u64,
    pub(crate) bar: Option<Bar>,
    pub(crate) last_find: LastFind,
    column_mode: bool,
    soft_wrap: bool,
    /// A pane of a comparison keeps its rows on the lines of the other pane, so soft
    /// wrap stays off there.
    wrap_locked: bool,
    show_whitespace: bool,
    outline: Vec<OutlineItem>,
    /// The declarations whose names start on a line, for the kind of a gutter breakpoint.
    line_declarations: Vec<LineDeclaration>,
    outline_serial: u64,
    /// Bumped after every answer about the outline, even an empty one.
    outline_generation: u64,
    /// Bumped after every parse of the current text reaches the state.
    syntax_generation: u64,
    /// Ctrl+F11 was pressed: the next key names the bookmark.
    awaiting_mnemonic: bool,
    /// Buffer lines of the sticky rows at the top, set when painting.
    pub(crate) sticky: Vec<usize>,
    /// Alt+drag: where it began and the carets that were there before.
    block_drag: Option<(usize, Vec<Selection>)>,
    /// The first key of a chord that waits for the next one.
    chords: Vec<Keystroke>,
    /// Alt+Q: the header of the enclosing declaration, shown while the caret stays.
    context_info: Option<ContextInfo>,
    /// Ctrl+Alt+.: the regions of the file and the filter over them.
    pub(crate) region_popup: Option<crate::regions::RegionPopup>,
    pub(crate) focus_handle: FocusHandle,
    pub(crate) scroll_top: f32,
    pub(crate) scroll_left: f32,
    /// The horizontal bar was painted in the last frame.
    pub(crate) horizontal_bar: bool,
    /// The pointer is over the bar: it takes the wide look.
    pub(crate) scrollbar_hovered: bool,
    /// The thumb the pointer holds.
    pub(crate) scrollbar_drag: Option<crate::scrollbar::Drag>,
    /// The hint of the mark under the pointer: its text and where it stands.
    stripe_hint: Option<(String, Point<Pixels>)>,
    /// The hint of the blame label under the pointer, and where it stands.
    annotate_hint: Option<(String, Point<Pixels>)>,
    pub(crate) autoscroll: bool,
    /// The next autoscroll puts the caret near the middle, as a jump to a place does.
    pub(crate) reveal_center: bool,
    pub(crate) caret_on: bool,
    pub(crate) viewport: Size<Pixels>,
    pub(crate) layout: Option<Rc<Layout>>,
    pub(crate) gutter_hovered: bool,
    pub(crate) run_marks: Vec<RunMark>,
    /// Buffer lines of the breakpoints, from zero.
    pub(crate) breakpoints: Vec<u32>,
    /// The breakpoints that go away at the first stop, painted as an outline.
    pub(crate) temporary_breakpoints: Vec<u32>,
    /// Buffer lines of the method breakpoints, painted as a diamond.
    pub(crate) function_breakpoints: Vec<u32>,
    /// Buffer lines of the field watchpoints, painted as an eye.
    pub(crate) field_watchpoints: Vec<u32>,
    pub(crate) gutter_marks: Vec<GutterMark>,
    selecting: bool,
    was_dirty: bool,
    /// The saves of this view whose text has not reached the disk yet. A count, because
    /// a blocking save may write while a background one is on its way; the task that
    /// writes lowers it, also when it panics, so the view never stays saving.
    saving: Arc<AtomicUsize>,
    /// The text and the format of the last save of this view that reached the disk.
    last_saved: Option<(String, FileFormat)>,
    pending_caret: Option<(usize, usize)>,
    failed: Option<SharedString>,
    document: DocumentId,
    pub(crate) syntax: SyntaxService,
    keymap: Rc<Keymap>,
    pub(crate) lsp: Option<Lsp>,
    pub(crate) live: LiveTemplates,
    /// The current Alt+/ walk, if the caret is still on the word it inserted.
    hippie: Option<HippieCycle>,
    /// Occurrences shown by Highlight Usages in File until Escape.
    pub(crate) usages: Option<Vec<std::ops::Range<usize>>>,
    /// A `go_to` that came before the file was read.
    pending_spot: Option<Spot>,
    /// The buffer rows the last inlay question covered, so a scroll asks only when
    /// another part of the document shows.
    pub(crate) inlay_range: Option<(usize, usize)>,
    /// The open question for inlay hints; a new one cancels the old at the server.
    pub(crate) inlays_task: Option<Task<()>>,
    /// The labels above declarations, as the language gave them for this text.
    pub(crate) code_lens: Vec<crate::code_lens::CodeLensItem>,
    /// The parts of each block row, in the order the row draws them.
    pub(crate) code_vision_labels: Vec<(usize, Vec<(lang_api::CodeLensKind, String)>)>,
    /// The label under the pointer: its display row and its place in the row.
    pub(crate) code_vision_hovered: Option<(usize, usize)>,
    /// Grows with every code lens question, so a late answer is dropped.
    pub(crate) code_lens_asked: u64,
    /// The offsets of the labels whose command was asked from the server already.
    pub(crate) code_lens_resolved: Vec<usize>,
    /// The open code lens question; a new one cancels the old at the server.
    pub(crate) code_lens_task: Option<Task<()>>,
    /// The documentation comments of the language and the choices made for each.
    pub(crate) docs: crate::rendered_doc::RenderedDocs,
    /// The preview of a markdown file beside its text, `None` for other files.
    pub(crate) preview: Option<crate::markdown_preview::MarkdownPreview>,
    /// Keys waiting for the next frame, so the latency of a key includes the paint.
    pending_frames: Vec<Instant>,
}

impl EventEmitter<EditorEvent> for EditorView {}

/// Lowers the count of saves in flight when the task that writes ends, also when it
/// panics or is dropped before its result lands.
struct SavingGuard(Option<Arc<AtomicUsize>>);

impl SavingGuard {
    fn new(count: Arc<AtomicUsize>) -> Self {
        Self(Some(count))
    }

    fn finish(&mut self) {
        if let Some(count) = self.0.take() {
            count.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

impl Drop for SavingGuard {
    fn drop(&mut self) {
        self.finish();
    }
}

impl EditorView {
    /// Opens `path`. The file is read in the background; the view stays empty until then.
    pub fn open(
        path: PathBuf,
        style: EditorStyle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut view = Self::empty(path, style, cx);
        view.preview = crate::markdown_preview::MarkdownPreview::state_for(&view.path, cx);
        let language = cx
            .try_global::<Languages>()
            .and_then(|languages| (languages.resolve)(&view.path));
        match Self::source_text(&view.path, cx) {
            Some(text) => {
                let state = EditorState::new(&text, FileFormat::default(), language, true);
                cx.defer_in(window, |this, _, cx| this.loaded(Ok(state), cx));
            }
            None => {
                let load = view.path.clone();
                let task = cx.background_spawn(async move { EditorState::load(&load, language) });
                cx.spawn_in(window, async move |this, cx| {
                    let result = task.await;
                    this.update(cx, |this, cx| this.loaded(result, cx)).ok();
                })
                .detach();
            }
        }
        cx.observe_global::<Reveals>(|this, cx| this.reveal(cx))
            .detach();
        view.start_blink(window, cx);
        view
    }

    /// Opens `text` over `path` at once, for a view of text that is not the file on disk:
    /// the other side of a comparison, or a version from the local history. The language
    /// comes from the path, so the text is coloured as the file would be.
    ///
    /// Such a view never writes the bookmarks of the file. A read-only one shows another
    /// version, where the lines of the bookmarks mean nothing, so it shows none; an
    /// editable one is the text the file has and shows them.
    pub fn open_text(
        path: PathBuf,
        text: String,
        read_only: bool,
        style: EditorStyle,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::open_document(
            path,
            text,
            FileFormat::default(),
            read_only,
            false,
            !read_only,
            style,
            cx,
        )
    }

    /// Opens `text` over `path` as a document of its own, written with `format` when it
    /// saves. `writes_bookmarks` is false for a view of a text that is not the tab of
    /// the file: it reads the bookmarks of the file and never replaces them in the
    /// window list.
    pub fn open_text_document(
        path: PathBuf,
        text: String,
        format: FileFormat,
        read_only: bool,
        writes_bookmarks: bool,
        style: EditorStyle,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::open_document(
            path,
            text,
            format,
            read_only,
            writes_bookmarks,
            true,
            style,
            cx,
        )
    }

    /// The document of `open_text` and `open_text_document`; `shows_bookmarks` reads the
    /// bookmarks the window keeps for the file.
    #[allow(clippy::too_many_arguments)]
    fn open_document(
        path: PathBuf,
        text: String,
        format: FileFormat,
        read_only: bool,
        writes_bookmarks: bool,
        shows_bookmarks: bool,
        style: EditorStyle,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut view = Self::empty(path, style, cx);
        let language = cx
            .try_global::<Languages>()
            .and_then(|languages| (languages.resolve)(&view.path));
        let mut state = EditorState::new(&text, format, language, read_only);
        state.set_writes_bookmarks(writes_bookmarks);
        state.set_save_options(view.options.save);
        state.apply_indent_defaults(
            view.options.use_tabs,
            view.options.indent_size,
            view.options.tab_size,
        );
        if let Some(language) = state.language() {
            view.start_syntax(language.clone(), &state, cx);
        }
        view.state = Some(state);
        let saved = shows_bookmarks
            .then(|| cx.try_global::<BookmarkStore>())
            .flatten()
            .and_then(|store| store.0.get(&view.path).cloned());
        if let (Some(saved), Some(state)) = (saved, &mut view.state) {
            state.set_bookmarks(&saved);
        }
        view.attach(cx);
        view.remember_text(cx);
        cx.notify();
        view
    }

    /// Opens another view of the document `source` shows. The text and the undo history
    /// are shared with `source`; the carets, selections, folds and scroll are this view's
    /// own, so one view moving never moves the other.
    pub fn open_shared(source: &Entity<EditorView>, cx: &mut App) -> Option<Entity<EditorView>> {
        let (state, path, style, options, zoom) = {
            let source = source.read(cx);
            let state = source.state.as_ref()?.fork();
            (
                state,
                source.path.clone(),
                source.base_style.clone(),
                source.options.clone(),
                source.zoom,
            )
        };
        let view = cx.new(|cx| {
            let mut view = Self::empty(path, style, cx);
            view.options = options;
            view.zoom = zoom;
            view.lock_soft_wrap();
            if let Some(language) = state.language() {
                view.start_syntax(language.clone(), &state, cx);
            }
            view.state = Some(state);
            view.attach(cx);
            view.remember_text(cx);
            view
        });
        Some(view)
    }

    /// Whether the two views show one document, so an edit through either is the text of
    /// both.
    pub fn shares_document(&self, other: &Self) -> bool {
        match (&self.state, &other.state) {
            (Some(state), Some(other)) => state.shares_document(other),
            _ => false,
        }
    }

    /// Takes the changes another view of the same document made into the carets and the
    /// folds of this view, before a key or an edit of this view counts its ranges from
    /// the text. Nothing happens when the document has not moved since the last follow.
    pub(crate) fn catch_up(&mut self, cx: &mut Context<Self>) {
        if self
            .state
            .as_ref()
            .is_some_and(EditorState::has_unseen_changes)
        {
            self.follow_changes(cx);
        }
    }

    /// Takes the changes another view of the same document made into the carets and the
    /// folds of this view, without adding an undo step: the history they belong to is
    /// shared. The text itself is shared too, so it is already here.
    pub fn follow_changes(&mut self, cx: &mut Context<Self>) {
        let followed = self.state.as_mut().map(EditorState::follow);
        match followed {
            None | Some(text_core::Followed::Nothing) => self.changed(None, cx),
            Some(text_core::Followed::Changes(change)) => self.changed(Some(change), cx),
            Some(text_core::Followed::Missed) => {
                // The edits that would move the ranges of this view are not known: drop
                // every range that points into the old text, so nothing lands past the
                // end of the new one or underlines the wrong line.
                let language = self
                    .state
                    .as_ref()
                    .and_then(|state| state.language().cloned());
                if let (Some(language), Some(state)) = (language, &self.state) {
                    self.start_syntax(language, state, cx);
                }
                if let Some(lsp) = &mut self.lsp {
                    lsp.marks.clear();
                    lsp.received = false;
                    lsp.completion = None;
                    lsp.note = None;
                    lsp.highlights.clear();
                    lsp.signatures = None;
                    lsp.parameter_walk = None;
                    lsp.quick = None;
                    lsp.actions = None;
                    lsp.rename = None;
                    lsp.move_to = None;
                    lsp.overrides = None;
                    lsp.definition = None;
                    lsp.generation = None;
                    lsp.safe_delete = None;
                }
                self.live.abandon();
                self.hippie = None;
                self.hover_word = None;
                self.region_popup = None;
                self.context_info = None;
                self.usages = None;
                self.code_lens.clear();
                self.code_vision_labels.clear();
                self.code_vision_hovered = None;
                // The bookmarks of the file belong to the window, not to this view: read
                // them again instead of syncing an empty list over the stored ones.
                let saved = cx
                    .try_global::<BookmarkStore>()
                    .and_then(|store| store.0.get(&self.path).cloned());
                if let (Some(saved), Some(state)) = (saved, &mut self.state) {
                    state.set_bookmarks(&saved);
                }
                // The server gets the new text and answers with the marks of it.
                self.sync_server(true);
                self.refresh_matches(false, cx);
                self.semantic_tokens_later(cx);
                self.request_inlays(crate::semantic::INLAY_TYPING_DELAY, cx);
                self.request_code_lens(crate::semantic::INLAY_TYPING_DELAY, cx);
                self.request_doc_comments(cx);
                self.highlight_later(cx);
                self.autoscroll = true;
                self.caret_on = true;
                cx.emit(EditorEvent::Changed);
                self.notify_dirty(cx);
                self.remember_text(cx);
                cx.notify();
            }
        }
    }

    /// The fields of a view before it has text. The file or the text comes next.
    fn empty(path: PathBuf, style: EditorStyle, cx: &mut Context<Self>) -> Self {
        static NEXT_DOCUMENT: AtomicU64 = AtomicU64::new(1);

        let templates = cx
            .try_global::<Languages>()
            .and_then(|languages| (languages.templates)(&path));
        if !cx.has_global::<Keys>() {
            cx.set_global(Keys(Rc::new(default_keymap().1)));
            // The window binds Ctrl+Alt+Shift+Left and Right to the split groups. The
            // editor binds the same keys here so that, with the focus inside it, the
            // element under the caret moves instead.
            cx.bind_keys([
                KeyBinding::new("ctrl-alt-shift-left", MoveElementLeft, Some("Editor")),
                KeyBinding::new("ctrl-alt-shift-right", MoveElementRight, Some("Editor")),
            ]);
        }
        Self {
            path,
            state: None,
            style: style.clone(),
            base_style: style,
            zoom: 1.,
            options: EditorOptions::default(),
            completion_typed: 0,
            tokens_asked: 0,
            hover_word: None,
            hover_serial: 0,
            hover_close: 0,
            quick_serial: 0,
            bar: None,
            last_find: LastFind::default(),
            column_mode: false,
            soft_wrap: false,
            wrap_locked: false,
            show_whitespace: false,
            outline: Vec::new(),
            line_declarations: Vec::new(),
            outline_serial: 0,
            outline_generation: 0,
            syntax_generation: 0,
            awaiting_mnemonic: false,
            sticky: Vec::new(),
            block_drag: None,
            chords: Vec::new(),
            context_info: None,
            region_popup: None,
            focus_handle: cx.focus_handle(),
            scroll_top: 0.,
            scroll_left: 0.,
            horizontal_bar: false,
            scrollbar_hovered: false,
            scrollbar_drag: None,
            stripe_hint: None,
            annotate_hint: None,
            autoscroll: false,
            reveal_center: false,
            caret_on: true,
            viewport: Size::default(),
            layout: None,
            gutter_hovered: false,
            run_marks: Vec::new(),
            breakpoints: Vec::new(),
            temporary_breakpoints: Vec::new(),
            function_breakpoints: Vec::new(),
            field_watchpoints: Vec::new(),
            gutter_marks: Vec::new(),
            selecting: false,
            was_dirty: false,
            saving: Arc::new(AtomicUsize::new(0)),
            last_saved: None,
            pending_caret: None,
            failed: None,
            document: DocumentId(NEXT_DOCUMENT.fetch_add(1, Ordering::Relaxed)),
            syntax: cx.default_global::<SyntaxThread>().0.clone(),
            keymap: cx.global::<Keys>().0.clone(),
            lsp: None,
            live: LiveTemplates::new(templates),
            hippie: None,
            usages: None,
            pending_spot: None,
            inlay_range: None,
            inlays_task: None,
            code_lens: Vec::new(),
            code_vision_labels: Vec::new(),
            code_vision_hovered: None,
            code_lens_asked: 0,
            code_lens_resolved: Vec::new(),
            code_lens_task: None,
            docs: crate::rendered_doc::RenderedDocs::default(),
            preview: None,
            pending_frames: Vec::with_capacity(8),
        }
    }

    /// Blinks the caret while the editor has the focus.
    fn start_blink(&self, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(BLINK).await;
                let alive = this.update_in(cx, |this, window, cx| {
                    if this.focus_handle.is_focused(window) && window.is_window_active() {
                        if this.options.caret_blink {
                            this.caret_on = !this.caret_on;
                            cx.notify();
                        } else if !this.caret_on {
                            this.caret_on = true;
                            cx.notify();
                        }
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    /// The text of the editor alone, without its bar, crumbs and problem widget, for a
    /// view that shows it beside another one. Keys and the input handler stay those of
    /// this editor, so the pane is a full editor. `context` names the keys of the view
    /// that holds the pane; it is deeper than `Editor`, so its keys win over the keys of
    /// the window.
    pub fn pane(
        &self,
        decor: crate::PaneDecor,
        context: Option<&'static str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut focus = div()
            .track_focus(&self.focus_handle)
            .size_full()
            .child(EditorElement::with_decor(cx.entity(), decor));
        if let Some(context) = context {
            focus = focus.key_context(context);
        }
        let popups = self.popups(window, cx);
        let theme = *cx.global::<Theme>();
        // The find bar stands over the first rows, so a pane beside another one keeps
        // its rows on the same lines while the bar is open.
        let bar = self.render_bar(cx).map(|bar| {
            div()
                .absolute()
                .top_0()
                .left_0()
                .right_0()
                .bg(theme.popup_bg)
                .child(bar)
        });
        div()
            .key_context("Editor")
            .relative()
            .size_full()
            .bg(self.style.background)
            .text_color(self.style.code)
            .on_action(cx.listener(|this, _: &MoveElementLeft, _, cx| {
                if !this.move_element_action(false, cx) {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &MoveElementRight, _, cx| {
                if !this.move_element_action(true, cx) {
                    cx.propagate();
                }
            }))
            .on_key_down(cx.listener(Self::on_key))
            .child(focus)
            .children(bar)
            .children(popups)
            .children(self.template_popup(cx))
            .into_any_element()
    }

    /// The height of one row of the text.
    pub fn line_height(&self) -> f32 {
        self.style.line_height
    }

    /// Puts the first visible row at `top`, as a drag of the vertical bar does.
    pub fn scroll_to_top(&mut self, top: f32, cx: &mut Context<Self>) {
        self.set_scroll_top(top, cx);
    }

    /// Moves the caret and shows it. Before the file is read the move waits for it.
    pub fn go_to(&mut self, spot: Spot, cx: &mut Context<Self>) {
        let Some(state) = &mut self.state else {
            self.pending_spot = Some(spot);
            return;
        };
        let offset = match spot {
            Spot::Offset(offset) => offset.min(state.text().len()),
            Spot::Line { line, column } => state.offset_at(line, column),
        };
        state.select_range(offset..offset);
        self.autoscroll = true;
        self.reveal_center = true;
        self.caret_on = true;
        self.inlays_if_needed(cx);
        cx.notify();
    }

    /// The first visible row, for a test of where a jump scrolled.
    pub fn scroll_top(&self) -> f32 {
        self.scroll_top
    }

    /// Replaces the run marks in the gutter.
    pub fn set_run_marks(&mut self, marks: Vec<RunMark>, cx: &mut Context<Self>) {
        if self.run_marks != marks {
            self.run_marks = marks;
            cx.notify();
        }
    }

    pub fn run_marks(&self) -> &[RunMark] {
        &self.run_marks
    }

    /// Replaces the breakpoints in the gutter. Lines are buffer lines from zero.
    pub fn set_breakpoints(&mut self, lines: Vec<u32>, cx: &mut Context<Self>) {
        if self.breakpoints != lines {
            self.breakpoints = lines;
            cx.notify();
        }
    }

    pub fn breakpoints(&self) -> &[u32] {
        &self.breakpoints
    }

    /// Replaces the temporary breakpoints in the gutter, the ones an outline marks.
    pub fn set_temporary_breakpoints(&mut self, lines: Vec<u32>, cx: &mut Context<Self>) {
        if self.temporary_breakpoints != lines {
            self.temporary_breakpoints = lines;
            cx.notify();
        }
    }

    pub fn temporary_breakpoints(&self) -> &[u32] {
        &self.temporary_breakpoints
    }

    /// Replaces the method breakpoints in the gutter, painted as diamonds.
    pub fn set_function_breakpoints(&mut self, lines: Vec<u32>, cx: &mut Context<Self>) {
        if self.function_breakpoints != lines {
            self.function_breakpoints = lines;
            cx.notify();
        }
    }

    pub fn function_breakpoints(&self) -> &[u32] {
        &self.function_breakpoints
    }

    /// Replaces the field watchpoints in the gutter, painted as eyes.
    pub fn set_field_watchpoints(&mut self, lines: Vec<u32>, cx: &mut Context<Self>) {
        if self.field_watchpoints != lines {
            self.field_watchpoints = lines;
            cx.notify();
        }
    }

    pub fn field_watchpoints(&self) -> &[u32] {
        &self.field_watchpoints
    }

    /// Marks of the changes against the last commit, as the gutter paints them.
    pub fn gutter_marks(&self) -> &[GutterMark] {
        &self.gutter_marks
    }

    /// The diagnostics of the language for the text, in buffer order: the same marks
    /// the text and the gutter paint.
    pub fn diagnostic_marks(&self) -> Vec<(Severity, std::ops::Range<usize>)> {
        self.lsp
            .as_ref()
            .map(|lsp| {
                lsp.marks
                    .iter()
                    .map(|mark| (mark.severity, mark.range.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The caret line from zero, `None` before the file is read.
    pub fn caret_line(&self) -> Option<usize> {
        self.state.as_ref().map(|state| state.caret().0)
    }

    /// The counts of the problem widget, and whether the server is still working.
    pub fn problem_summary(&self) -> ProblemSummary {
        let Some(lsp) = &self.lsp else {
            return ProblemSummary::default();
        };
        let mut summary = ProblemSummary {
            analyzing: !lsp.received,
            ..ProblemSummary::default()
        };
        for mark in &lsp.marks {
            match mark.severity {
                Severity::Error => summary.errors += 1,
                Severity::Warning => summary.warnings += 1,
                Severity::Information | Severity::Hint => {}
            }
        }
        summary
    }

    /// The marks of the last frame on the vertical bar, by kind and buffer line.
    pub fn stripe_kinds(&self) -> Vec<(crate::element::StripeKind, usize)> {
        self.layout
            .as_ref()
            .map(|layout| {
                layout
                    .stripes
                    .iter()
                    .map(|mark| (mark.kind, mark.line))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Where the mark of the bar is, for a click in a test.
    pub fn stripe_bounds(
        &self,
        kind: crate::element::StripeKind,
        line: usize,
    ) -> Option<Bounds<Pixels>> {
        self.layout
            .as_ref()?
            .stripes
            .iter()
            .find(|mark| mark.kind == kind && mark.line == line)
            .map(|mark| mark.bounds)
    }

    /// The rows of the last frame, from the top, with the number of each in the gutter.
    pub fn row_paint(&self) -> Vec<RowPaint> {
        let Some(layout) = &self.layout else {
            return Vec::new();
        };
        layout
            .rows
            .iter()
            .map(|row| RowPaint {
                line: row.buffer_line,
                top: f32::from(row.y - layout.bounds.top()),
                number: row.number_value,
                block: row.is_block,
            })
            .collect()
    }

    /// The height of the panel of sticky lines as the last frame painted it.
    pub fn sticky_panel_height(&self) -> f32 {
        self.layout
            .as_ref()
            .map_or(0., |layout| layout.sticky_height)
    }

    /// The thumb of the vertical bar as the last frame painted it, for a drag in a test.
    pub fn vertical_thumb(&self) -> Option<Bounds<Pixels>> {
        let layout = self.layout.as_ref()?;
        let bar = layout.vertical?;
        let area = layout.vertical_area?;
        let width = if self.scrollbar_hovered {
            crate::scrollbar::HOVER_WIDTH
        } else {
            crate::scrollbar::WIDTH
        };
        let thumb = bar.thumb(self.scroll_top)?;
        let x = f32::from(area.right()) - width;
        Some(Bounds::new(
            point(px(x), px(bar.track_start + thumb.offset)),
            size(px(width), px(thumb.len)),
        ))
    }

    /// The hints of the last frame, with the pill and the baseline of each.
    pub fn inlay_paint(&self) -> &[InlayPaint] {
        self.layout
            .as_ref()
            .map_or(&[], |layout| layout.inlays.as_slice())
    }

    /// The caret in bytes, `None` before the file is read.
    pub fn head_offset(&self) -> Option<usize> {
        self.state.as_ref().map(EditorState::head)
    }

    /// The occurrences of Highlight Usages in File, `None` when it is off.
    /// Whether the text has a syntax tree yet; the parse runs on its own thread, and
    /// what depends on the place of the caret (templates, for one) waits for it.
    pub fn has_syntax_tree(&self) -> bool {
        self.state
            .as_ref()
            .is_some_and(|state| state.tree().is_some())
    }

    pub fn highlighted_usages(&self) -> Option<&[std::ops::Range<usize>]> {
        self.usages.as_deref()
    }

    /// Where a popup of `height` at the caret goes, in window coordinates, and the corner
    /// it is anchored by. `None` before the first frame laid the text out.
    pub fn popup_at_caret(&self, height: f32) -> Option<(Point<Pixels>, Anchor)> {
        self.popup_position(self.head_offset()?, height)
    }

    /// Edits from outside the keyboard. One undo step, the buffer becomes dirty.
    pub fn apply_edits(&mut self, edits: Vec<text_core::Edit>, cx: &mut Context<Self>) {
        self.catch_up(cx);
        let Some(state) = &mut self.state else {
            return;
        };
        let change = state.apply_edits(edits);
        self.changed(change, cx);
    }

    fn loaded(&mut self, result: Result<EditorState, LoadError>, cx: &mut Context<Self>) {
        match result {
            Ok(mut state) => {
                if let Some(language) = state.language() {
                    self.start_syntax(language.clone(), &state, cx);
                }
                if let Some((line, column)) = self.pending_caret.take() {
                    state.set_caret(line, column);
                    self.autoscroll = true;
                }
                self.state = Some(state);
                if let Some(state) = &mut self.state {
                    state.set_save_options(self.options.save);
                    state.apply_indent_defaults(
                        self.options.use_tabs,
                        self.options.indent_size,
                        self.options.tab_size,
                    );
                }
                self.attach(cx);
                self.reveal(cx);
                let saved = cx
                    .try_global::<BookmarkStore>()
                    .and_then(|store| store.0.get(&self.path).cloned());
                if let (Some(saved), Some(state)) = (saved, &mut self.state) {
                    state.set_bookmarks(&saved);
                }
                if let Some(spot) = self.pending_spot.take() {
                    self.go_to(spot, cx);
                }
                self.remember_text(cx);
                cx.emit(EditorEvent::Loaded);
            }
            Err(LoadError(message)) => {
                let message: SharedString = message.into();
                self.failed = Some(message.clone());
                cx.emit(EditorEvent::Failed(message));
            }
        }
        cx.notify();
    }

    fn start_syntax(&self, language: LanguageConfig, state: &EditorState, cx: &mut Context<Self>) {
        let (sender, mut receiver) = unbounded();
        self.syntax.open(self.document, language, move |snapshot| {
            sender.unbounded_send(snapshot).ok();
        });
        self.syntax.update(
            self.document,
            state.version(),
            state.snapshot(),
            &Change::default(),
        );
        cx.spawn(async move |this, cx| {
            while let Some(snapshot) = receiver.next().await {
                let alive = this.update(cx, |this, cx| {
                    if let Some(state) = &mut this.state
                        && state.accept_syntax(snapshot)
                    {
                        this.syntax_generation += 1;
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    pub fn title(&self) -> SharedString {
        self.path
            .file_name()
            .map_or("".into(), |name| name.to_string_lossy().to_string().into())
    }

    /// Whether the text of the file is read and the commands can run on it.
    pub fn is_loaded(&self) -> bool {
        self.state.is_some()
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn is_dirty(&self) -> bool {
        self.state.as_ref().is_some_and(EditorState::is_dirty)
    }

    /// Whether the file is read-only, by its bytes or by the attribute of the disk.
    pub fn is_read_only(&self) -> bool {
        self.state.as_ref().is_some_and(EditorState::is_read_only)
    }

    /// Follows the read-only attribute of the file on disk.
    pub fn set_read_only(&mut self, read_only: bool, cx: &mut Context<Self>) {
        if let Some(state) = &mut self.state {
            state.set_read_only(read_only);
            cx.notify();
        }
    }

    /// Line and column of the caret, from zero.
    pub fn caret(&self) -> (usize, usize) {
        let waiting = self.pending_caret.unwrap_or((0, 0));
        self.state.as_ref().map_or(waiting, EditorState::caret)
    }

    /// The text of a line from zero, without its line break.
    pub fn line_text(&self, line: usize) -> Option<String> {
        let store = self.state.as_ref()?.store();
        (line < store.line_count()).then(|| store.line_text(line).trim_end().to_owned())
    }

    /// Replaces the marks beside the line numbers.
    pub fn set_gutter_marks(&mut self, marks: Vec<GutterMark>, cx: &mut Context<Self>) {
        if self.gutter_marks != marks {
            self.gutter_marks = marks;
            cx.notify();
        }
    }

    /// Puts the blame of the lines in the column before the text. The lines are counted
    /// from one.
    pub fn set_annotations(&mut self, lines: &[(u32, BlameNote)], cx: &mut Context<Self>) {
        if let Some(state) = &mut self.state {
            state.set_annotations(lines);
            cx.notify();
        }
    }

    /// Takes the blame column away.
    pub fn clear_annotations(&mut self, cx: &mut Context<Self>) {
        if let Some(state) = &mut self.state
            && state.has_annotations()
        {
            state.clear_annotations();
            self.annotate_hint = None;
            cx.notify();
        }
    }

    /// Whether the blame column is on.
    pub fn has_annotations(&self) -> bool {
        self.state
            .as_ref()
            .is_some_and(EditorState::has_annotations)
    }

    /// The painted labels of the blame column with their line from zero and their commit.
    pub fn annotation_lines(&self) -> Vec<(u32, String, String)> {
        self.state
            .as_ref()
            .map(EditorState::annotation_labels)
            .unwrap_or_default()
    }

    /// The room of the blame column left of the text, from zero when it is off.
    pub(crate) fn annotation_width(&self) -> f32 {
        if self.has_annotations() {
            ANNOTATE_WIDTH
        } else {
            0.
        }
    }

    /// The room of the gutter and of the blame column before the text.
    pub(crate) fn gutter_room(&self) -> f32 {
        GUTTER_WIDTH + self.annotation_width()
    }

    /// The text as the buffer holds it now, with LF line ends. `None` while loading.
    pub fn text(&self) -> Option<String> {
        self.state.as_ref().map(EditorState::text)
    }

    /// Whether the file is read, or the read failed. A script step that touches the text
    /// of the open file waits for this.
    pub fn is_ready(&self) -> bool {
        self.state.is_some() || self.failed.is_some()
    }

    /// The version of the text the language server has of this document.
    pub fn version(&self) -> Option<i32> {
        self.state.as_ref().map(|state| state.version() as i32)
    }

    /// What a save would write: the text, the encoding and line ends of the buffer,
    /// and its version. `None` while loading.
    pub fn snapshot_for_save(&self) -> Option<(String, text_core::FileFormat, u64)> {
        self.state.as_ref().map(EditorState::snapshot_for_save)
    }

    /// Bytes of the buffer, without copying the text, for the memory table.
    pub fn buffer_bytes(&self) -> usize {
        self.state.as_ref().map_or(0, |state| state.store().len())
    }

    /// The buffer as a save would write it: its encoding, BOM and line ends.
    pub fn file_bytes(&self) -> Option<Vec<u8>> {
        let (text, format, _) = self.state.as_ref()?.snapshot_for_save();
        Some(text_core::encode(&text, &format))
    }

    /// The encoding, BOM and line ends a save of this view writes.
    pub fn format(&self) -> Option<FileFormat> {
        self.state.as_ref().map(EditorState::format)
    }

    /// The text and the format of the last save of this view that reached the disk. A
    /// view that takes over the text after a close shows what the file got, not the
    /// buffer as it stood when the write started.
    pub fn saved_document(&self) -> Option<(String, FileFormat)> {
        self.last_saved.clone()
    }

    /// The primary selection as a byte range of [`Self::text`] and the caret in it.
    /// `None` while loading.
    pub fn selection(&self) -> Option<(std::ops::Range<usize>, usize)> {
        let primary = self.state.as_ref()?.selections().primary();
        Some((primary.range(), primary.head))
    }

    /// Replaces a byte range of the text as one edit of the user, so undo takes it back.
    pub fn replace_text(
        &mut self,
        range: std::ops::Range<usize>,
        text: &str,
        cx: &mut Context<Self>,
    ) {
        self.catch_up(cx);
        let Some(state) = &mut self.state else {
            return;
        };
        let change = state.replace_range(Some(range), text, false);
        self.changed(change, cx);
    }

    /// Where the caret goes once the file is read: a restored session, a jump from a list.
    pub fn restore_caret(&mut self, line: usize, column: usize, cx: &mut Context<Self>) {
        match &mut self.state {
            Some(state) => {
                state.set_caret(line, column);
                self.autoscroll = true;
                cx.notify();
            }
            None => self.pending_caret = Some((line, column)),
        }
    }

    /// Characters in the primary selection.
    pub fn selected_chars(&self) -> usize {
        self.state.as_ref().map_or(0, EditorState::selected_chars)
    }

    /// Changes the line end (`LF`, `CRLF`, `CR`) or the encoding the file is written in.
    /// The tab counts as edited until it is saved. Unknown names change nothing.
    pub fn set_format_name(&mut self, name: &str, cx: &mut Context<Self>) {
        let Some(state) = &mut self.state else {
            return;
        };
        let current = state.format();
        let format = match name {
            "LF" => Some(current.with_eol(text_core::Eol::Lf)),
            "CRLF" => Some(current.with_eol(text_core::Eol::CrLf)),
            "CR" => Some(current.with_eol(text_core::Eol::Cr)),
            other => current.with_encoding_name(other),
        };
        if let Some(format) = format {
            state.set_format(format);
            self.notify_dirty(cx);
            cx.notify();
        }
    }

    pub fn encoding(&self) -> &'static str {
        self.state.as_ref().map_or("", EditorState::encoding_name)
    }

    pub fn line_ending(&self) -> &'static str {
        self.state.as_ref().map_or("", EditorState::eol_name)
    }

    /// The name the application gave the language of the file, such as `Rust`.
    pub fn language_name(&self) -> Option<&'static str> {
        self.state
            .as_ref()
            .and_then(|state| state.language())
            .map(|language| language.name)
    }

    /// `Tab`, `2 spaces` or `4 spaces`, as the status bar shows the indentation.
    pub fn indent_label(&self) -> String {
        self.state
            .as_ref()
            .map_or_else(|| "4 spaces".to_owned(), |state| state.indent().label())
    }

    /// The indentation of the open file: tabs or spaces, the size of one step and
    /// whether a `.editorconfig` file named it.
    pub fn indent_status(&self) -> (bool, usize, bool) {
        self.state.as_ref().map_or((false, 4, false), |state| {
            let indent = state.indent();
            (
                indent.uses_tabs(),
                indent.size,
                indent.source == text_core::IndentSource::EditorConfig,
            )
        })
    }

    /// The width a tab takes on screen.
    pub fn tab_width(&self) -> usize {
        self.state.as_ref().map_or(4, EditorState::tab_width)
    }

    /// The columns between two indent guides.
    pub fn indent_columns(&self) -> usize {
        self.state.as_ref().map_or(4, EditorState::indent_columns)
    }

    /// The tab size and the spaces flag the formatter of the language gets.
    pub fn formatting_options(&self) -> (u32, bool) {
        self.state
            .as_ref()
            .map_or((4, true), EditorState::formatting_options)
    }

    /// Indents this open file with tabs or spaces, or changes its size, until it closes.
    pub fn set_indent(&mut self, tabs: Option<bool>, size: Option<usize>, cx: &mut Context<Self>) {
        let Some(state) = &mut self.state else {
            return;
        };
        state.set_indent(tabs, size);
        cx.notify();
    }

    pub fn set_style(&mut self, style: EditorStyle, cx: &mut Context<Self>) {
        self.base_style = style;
        self.apply_zoom();
        cx.notify();
    }

    /// Applies the Settings of the window to this editor at once.
    pub fn set_options(&mut self, options: EditorOptions, cx: &mut Context<Self>) {
        let indent_changed = self.options.tab_size != options.tab_size
            || self.options.indent_size != options.indent_size
            || self.options.use_tabs != options.use_tabs;
        let tokens_changed = self.options.semantic_highlighting != options.semantic_highlighting;
        let inlays_changed = self.options.inlay_parameter_names != options.inlay_parameter_names
            || self.options.inlay_type_hints != options.inlay_type_hints;
        let code_vision_changed = self.options.code_vision_usages != options.code_vision_usages
            || self.options.code_vision_implementations != options.code_vision_implementations;
        let docs_changed = self.options.render_doc_comments != options.render_doc_comments;
        self.options = options;
        if let Some(state) = &mut self.state {
            state.set_save_options(self.options.save);
            if indent_changed {
                state.apply_indent_defaults(
                    self.options.use_tabs,
                    self.options.indent_size,
                    self.options.tab_size,
                );
            }
            if tokens_changed && !self.options.semantic_highlighting {
                state.clear_semantic_tokens();
            }
        }
        if let Some(lsp) = &mut self.lsp {
            lsp.case = self.options.completion_case;
        }
        if tokens_changed && self.options.semantic_highlighting {
            self.semantic_tokens_now(cx);
        }
        if inlays_changed {
            if let Some(lsp) = &self.lsp {
                lsp.support
                    .set_inlay_hint_settings(crate::semantic::inlay_settings(&self.options));
            }
            let off = self.options.inlay_parameter_names == lang_api::InlayParameterNames::None
                && !self.options.inlay_type_hints;
            self.inlays_task = None;
            self.inlay_range = None;
            if let Some(state) = &mut self.state {
                state.set_inlays(Vec::new());
            }
            if !off {
                self.request_inlays(std::time::Duration::ZERO, cx);
            }
        }
        if code_vision_changed {
            if let Some(lsp) = &self.lsp {
                lsp.support
                    .set_code_vision_settings(crate::semantic::code_vision_settings(&self.options));
            }
            self.code_lens_task = None;
            self.code_lens_resolved.clear();
            if let Some(state) = &mut self.state {
                state.set_blocks(Vec::new());
            }
            self.code_lens.clear();
            self.code_vision_labels.clear();
            self.code_vision_hovered = None;
            if self.options.code_vision_usages || self.options.code_vision_implementations {
                self.request_code_lens(std::time::Duration::ZERO, cx);
            }
        }
        if !self.options.caret_blink {
            self.caret_on = true;
        }
        if docs_changed {
            self.apply_doc_comments(cx);
        }
        cx.notify();
    }

    /// What Settings turned on and off in this editor.
    pub fn options(&self) -> &EditorOptions {
        &self.options
    }

    fn apply_zoom(&mut self) {
        let base = self.base_style.clone();
        self.style = EditorStyle {
            font_size: (base.font_size * self.zoom).round().max(6.),
            line_height: (base.line_height * self.zoom).round().max(10.),
            ..base
        };
    }

    /// Ctrl+wheel.
    pub(crate) fn zoom_by(&mut self, up: bool, cx: &mut Context<Self>) {
        let step = if up { 0.1 } else { -0.1 };
        self.zoom = (self.zoom + step).clamp(ZOOM.0, ZOOM.1);
        self.apply_zoom();
        cx.notify();
    }

    /// Pastes text the way Ctrl+V does.
    pub fn paste_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.catch_up(cx);
        let change = self.state.as_mut().and_then(|state| state.paste(text));
        self.changed(change, cx);
    }

    pub fn save(&mut self, cx: &mut Context<Self>) {
        self.prepare_save(cx);
        let Some(state) = &self.state else {
            return;
        };
        if state.is_read_only() {
            return;
        }
        let snapshot = state.snapshot_for_save();
        let path = self.path.clone();
        self.saving.fetch_add(1, Ordering::SeqCst);
        let mut guard = SavingGuard::new(self.saving.clone());
        let task = cx.background_spawn(async move { EditorState::save_snapshot(&path, snapshot) });
        cx.spawn(async move |this, cx| {
            let saved = task.await;
            guard.finish();
            this.update(cx, |this, cx| this.saved(&saved, cx)).ok();
        })
        .detach();
    }

    /// Whether a save of this view is writing the text now. Its text is the snapshot
    /// taken when the write started, not necessarily the buffer as it is now.
    pub fn is_saving(&self) -> bool {
        self.saving.load(Ordering::SeqCst) > 0
    }

    /// The rules of `.editorconfig` for a save go into the buffer first, so the disk gets
    /// the text the editor shows.
    fn prepare_save(&mut self, cx: &mut Context<Self>) {
        let change = self.state.as_mut().and_then(EditorState::prepare_save);
        if change.is_some() {
            self.changed(change, cx);
        }
    }

    /// The file changed on disk: reads it off the UI thread and takes the new text unless
    /// the buffer has unsaved edits. Then [`EditorEvent::Conflict`] goes out.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        self.read_disk(false, cx);
    }

    /// Takes the text on disk although the buffer has edits. It comes in as one edit,
    /// so undo brings the edits back.
    pub fn reload_from_disk(&mut self, cx: &mut Context<Self>) {
        self.read_disk(true, cx);
    }

    fn read_disk(&mut self, forced: bool, cx: &mut Context<Self>) {
        let path = self.path.clone();
        let task = cx.background_spawn(async move { read_file(&path) });
        cx.spawn(async move |this, cx| {
            let Ok(decoded) = task.await else {
                return;
            };
            this.update(cx, |this, cx| {
                // The buffer as it was, before the disk text comes in; a change of the
                // disk that leaves the same text, as our own save does, is not an
                // external change and needs no snapshot.
                let before = if forced { None } else { this.file_bytes() };
                let Some(state) = &mut this.state else {
                    return;
                };
                if forced {
                    let change = state.reload_from_disk(decoded);
                    return this.changed(change, cx);
                }
                match state.reload(decoded) {
                    Ok(change) => {
                        let external = change.is_some().then_some(before).flatten();
                        this.changed(change, cx);
                        if let Some(bytes) = external {
                            cx.emit(EditorEvent::ExternalChange(bytes));
                        }
                    }
                    Err(Conflict) => cx.emit(EditorEvent::Conflict),
                }
            })
            .ok();
        })
        .detach();
    }

    /// Writes before it returns: a background write would not outlive a closing window.
    /// `Some` says why the text is still not on disk.
    pub fn save_blocking(&mut self, cx: &mut Context<Self>) -> Option<String> {
        self.prepare_save(cx);
        let Some(state) = &self.state else {
            return None;
        };
        if state.is_read_only() {
            return Some("the file is read-only".into());
        }
        let saved = EditorState::save_snapshot(&self.path, state.snapshot_for_save());
        let result = saved.result.clone();
        self.saved(&saved, cx);
        result.err()
    }

    fn saved(&mut self, saved: &Saved, cx: &mut Context<Self>) {
        if let Some(state) = &mut self.state {
            state.finish_save(saved);
        }
        if saved.result.is_ok() {
            self.last_saved = Some((saved.text.clone(), saved.format.clone()));
        }
        match &saved.result {
            Ok(()) => {
                self.saved_on_disk();
                cx.emit(EditorEvent::Saved);
            }
            Err(message) => cx.emit(EditorEvent::Failed(message.clone().into())),
        }
        self.notify_dirty(cx);
        cx.notify();
    }

    fn notify_dirty(&mut self, cx: &mut Context<Self>) {
        let dirty = self.is_dirty();
        if dirty != self.was_dirty {
            self.was_dirty = dirty;
            cx.emit(EditorEvent::Dirty(dirty));
        }
    }

    /// Hands a change to the parser and shows the caret.
    pub(crate) fn changed(&mut self, change: Option<Change>, cx: &mut Context<Self>) {
        self.context_info = None;
        let change = self.follow_template(change);
        self.finish_change(change, cx);
    }

    /// A change that arrived after the insertion it belongs to, such as an import of a
    /// late resolve. It does not close an open snippet session.
    pub(crate) fn changed_late(&mut self, change: Option<Change>, cx: &mut Context<Self>) {
        let change = self.live.follow_late(change);
        self.finish_change(change, cx);
    }

    fn finish_change(&mut self, change: Option<Change>, cx: &mut Context<Self>) {
        if let (Some(change), Some(state)) = (&change, &self.state)
            && state.language().is_some()
        {
            self.syntax
                .update(self.document, state.version(), state.snapshot(), change);
        }
        self.sync_server(change.is_some());
        if change.is_some() {
            self.semantic_tokens_later(cx);
            self.request_inlays(crate::semantic::INLAY_TYPING_DELAY, cx);
            self.request_code_lens(crate::semantic::INLAY_TYPING_DELAY, cx);
            if let Some(change) = &change {
                self.shift_code_lens(change, cx);
                self.shift_doc_comments(change);
            }
            self.request_doc_comments(cx);
        } else {
            self.inlays_if_needed(cx);
        }
        if !change.is_some()
            || self
                .lsp
                .as_ref()
                .is_some_and(|lsp| lsp.signatures.is_some())
        {
            self.refresh_parameters(cx);
        }
        self.highlight_later(cx);
        self.autoscroll = true;
        self.caret_on = true;
        if let Some(change) = &change {
            for step in &change.steps {
                for item in &mut self.outline {
                    item.range.start = text_core::transform_offset(item.range.start, step);
                    item.range.end = text_core::transform_offset(item.range.end, step);
                    item.name_range.start =
                        text_core::transform_offset(item.name_range.start, step);
                    item.name_range.end = text_core::transform_offset(item.name_range.end, step);
                }
            }
            if let Some(lsp) = &mut self.lsp {
                let len = self.state.as_ref().map_or(0, |state| state.store().len());
                for range in &mut lsp.highlights {
                    for step in &change.steps {
                        range.start = text_core::transform_offset(range.start, step);
                        range.end = text_core::transform_offset(range.end, step);
                    }
                    range.end = range.end.min(len);
                    range.start = range.start.min(range.end);
                }
                lsp.highlights.retain(|range| range.start < range.end);
                lsp.highlights.sort_by_key(|range| (range.start, range.end));
                lsp.highlights.dedup();
                for mark in &mut lsp.marks {
                    for step in &change.steps {
                        mark.range.start = text_core::transform_offset(mark.range.start, step);
                        mark.range.end = text_core::transform_offset(mark.range.end, step);
                    }
                    mark.range.end = mark.range.end.min(len);
                    mark.range.start = mark.range.start.min(mark.range.end);
                }
            }
            self.usages = None;
            self.sync_bookmarks(cx);
        }
        if change.is_some() {
            cx.emit(EditorEvent::Changed);
            if self.bar.is_some() {
                self.refresh_matches(false, cx);
            }
        } else {
            // The status line shows the caret, so the window follows moves too.
            cx.emit(EditorEvent::Moved);
        }
        self.notify_dirty(cx);
        self.remember_text(cx);
        cx.notify();
    }

    /// Keeps the text of this editor where hippie completion of the other editors finds
    /// it. A weak handle lets a closed editor drop out.
    fn remember_text(&self, cx: &mut Context<Self>) {
        let Some(state) = &self.state else {
            return;
        };
        let store = state.snapshot();
        let id = cx.entity().entity_id();
        let view = cx.weak_entity();
        let texts = cx.default_global::<OpenTexts>();
        texts.0.retain(|_, entry| entry.view.upgrade().is_some());
        texts.0.insert(
            id,
            OpenText {
                path: self.path.clone(),
                view,
                store,
            },
        );
    }

    /// Texts of the other open editors, in path order, so a walk is the same on every key.
    fn other_texts(&self, cx: &App) -> Vec<RopeStore> {
        let Some(texts) = cx.try_global::<OpenTexts>() else {
            return Vec::new();
        };
        let mut found: Vec<(PathBuf, RopeStore)> = texts
            .0
            .values()
            .filter(|entry| entry.path != self.path && entry.view.upgrade().is_some())
            .map(|entry| (entry.path.clone(), entry.store.clone()))
            .collect();
        found.sort_by(|left, right| left.0.cmp(&right.0));
        found.into_iter().map(|(_, store)| store).collect()
    }

    /// Alt+/ and Alt+Shift+/: replaces the typed prefix with a word of the file that
    /// starts with it, then walks the other words. A repeat replaces the previous pick,
    /// so the prefix stays. The words above the caret come first, then those below it,
    /// then those of the other open buffers.
    fn hippie(&mut self, forward: bool, cx: &mut Context<Self>) {
        let others = self.other_texts(cx);
        let Some(state) = &mut self.state else {
            return;
        };
        let caret = state.selections().primary().head;
        let store = state.store();
        let continuing = self.hippie.as_ref().is_some_and(|cycle| {
            cycle.range.end == caret && store.slice(cycle.range.clone()) == cycle.word
        });
        if continuing {
            let cycle = self.hippie.as_mut().unwrap();
            let count = cycle.words.len();
            let index = if forward {
                (cycle.index + 1) % count
            } else {
                (cycle.index + count - 1) % count
            };
            let word = cycle.words[index].clone();
            let previous = cycle.range.clone();
            cycle.range = cycle.range.start..cycle.range.start + word.len();
            cycle.word = word.clone();
            cycle.index = index;
            state.select_range(previous);
            let change = state.type_text(&word);
            self.changed(change, cx);
            return;
        }
        let prefix_start = crate::semantic::word_start(&store, caret);
        let prefix = store.slice(prefix_start..caret);
        if prefix.is_empty() {
            return;
        }
        let mut words: Vec<String> = Vec::new();
        let mut push = |word: String| {
            if word != prefix && !words.contains(&word) {
                words.push(word);
            }
        };
        let text = store.slice(0..store.len());
        for range in crate::hippie::around(&text, prefix_start, caret, &prefix) {
            push(text[range].to_owned());
        }
        for other in &others {
            let text = other.slice(0..other.len());
            for range in crate::hippie::all(&text, &prefix) {
                push(text[range].to_owned());
            }
        }
        if words.is_empty() {
            return;
        }
        let index = if forward { 0 } else { words.len() - 1 };
        let word = words[index].clone();
        let range = prefix_start..prefix_start + word.len();
        state.select_range(prefix_start..caret);
        let change = state.type_text(&word);
        self.hippie = Some(HippieCycle {
            range,
            word,
            words,
            index,
        });
        self.changed(change, cx);
    }

    /// The declarations of the file; the language reads them and tells the view.
    pub fn set_outline(&mut self, items: Vec<OutlineItem>, cx: &mut Context<Self>) {
        self.outline_generation += 1;
        if self.outline != items {
            self.outline = items;
            cx.notify();
        }
    }

    pub fn outline(&self) -> &[OutlineItem] {
        &self.outline
    }

    /// Replaces the declarations whose names start on a line, from the same language
    /// answer as the outline.
    pub fn set_line_declarations(
        &mut self,
        declarations: Vec<LineDeclaration>,
        cx: &mut Context<Self>,
    ) {
        if self.line_declarations != declarations {
            self.line_declarations = declarations;
            cx.notify();
        }
    }

    /// The declaration whose name starts on the buffer line, if the language marked one.
    pub fn declaration_on_line(&self, line: usize) -> Option<&LineDeclaration> {
        self.line_declarations
            .iter()
            .find(|declaration| declaration.line == line)
    }

    /// How many answers about the outline have come back. A test waits for this to grow
    /// instead of waiting for a clock.
    pub fn outline_generation(&self) -> u64 {
        self.outline_generation
    }

    /// How many parses of the current text have reached the state. A test waits for this
    /// to grow instead of waiting for a clock.
    pub fn syntax_generation(&self) -> u64 {
        self.syntax_generation
    }

    /// The spans the last parse coloured. A test reads them to check that a file format
    /// reaches the editor with its language.
    pub fn highlight_spans(&self) -> &[HighlightSpan] {
        self.state.as_ref().map_or(&[], |state| state.spans())
    }

    /// Numbers the next request for the outline, so that an older answer can be dropped.
    pub fn next_outline_serial(&mut self) -> u64 {
        self.outline_serial += 1;
        self.outline_serial
    }

    pub fn outline_serial(&self) -> u64 {
        self.outline_serial
    }

    /// The declarations around the caret, outermost first.
    pub fn enclosing(&self) -> Vec<&OutlineItem> {
        let Some(head) = self.head_offset() else {
            return Vec::new();
        };
        around(&self.outline, head)
    }

    /// The buffer lines pinned at the top of the text, from zero.
    pub fn sticky_lines(&self) -> &[usize] {
        &self.sticky
    }

    pub fn soft_wrap(&self) -> bool {
        self.soft_wrap
    }

    /// Keeps the rows of this view on the lines of the text: a comparison pane turns
    /// soft wrap off and the key that toggles it does nothing here.
    pub fn lock_soft_wrap(&mut self) {
        self.soft_wrap = false;
        self.wrap_locked = true;
    }

    /// The buffer lines the display has rows for, without the ones a fold hides.
    pub fn shown_lines(&self) -> Vec<usize> {
        self.state.as_ref().map_or_else(Vec::new, |state| {
            state
                .visible_lines()
                .into_iter()
                .map(|line| line as usize)
                .collect()
        })
    }

    /// Insert replaces the character under the caret, as the status bar says.
    pub fn overwrite(&self) -> bool {
        self.state.as_ref().is_some_and(|state| state.overwrite())
    }

    pub fn show_whitespace(&self) -> bool {
        self.show_whitespace
    }

    /// The rows the display map has now: folds take rows away, soft wrap adds them.
    pub fn display_rows(&self) -> usize {
        self.state
            .as_ref()
            .map_or(0, |state| state.display().row_count())
    }

    /// The font size after Ctrl+wheel.
    pub fn font_size(&self) -> f32 {
        self.style.font_size
    }

    /// F11 and Ctrl+F11 with a name.
    pub fn toggle_bookmark(&mut self, mnemonic: Option<char>, cx: &mut Context<Self>) {
        self.catch_up(cx);
        if let Some(state) = &mut self.state {
            state.toggle_bookmark(mnemonic);
        }
        self.sync_bookmarks(cx);
        cx.notify();
    }

    /// Tells the window-wide list where the bookmarks of this file are now.
    fn sync_bookmarks(&mut self, cx: &mut Context<Self>) {
        let Some(state) = &self.state else {
            return;
        };
        // A second view of the document keeps the bookmarks of the file to itself: its
        // list moves with its own folds, and an empty one must not erase the stored list.
        if !state.writes_bookmarks() {
            return;
        }
        let lines = state.bookmarks();
        let store = cx.default_global::<BookmarkStore>();
        if lines.is_empty() {
            store.0.remove(&self.path);
        } else {
            store.0.insert(self.path.clone(), lines);
        }
    }

    /// Ctrl+digit: the line of the bookmark with that name, in this file or another.
    fn goto_bookmark(&mut self, mnemonic: Option<char>, cx: &mut Context<Self>) {
        let Some(letter) = mnemonic else {
            return;
        };
        let local = self
            .state
            .as_ref()
            .and_then(|state| state.bookmark_line(letter));
        if let Some(line) = local {
            return self.go_to(Spot::Line { line, column: 0 }, cx);
        }
        let elsewhere = bookmarks(cx)
            .into_iter()
            .find(|entry| entry.mnemonic == Some(letter));
        if let Some(entry) = elsewhere {
            cx.emit(EditorEvent::OpenBookmark {
                path: entry.path,
                line: entry.line,
            });
        }
    }

    /// Moves the caret to a line from zero, as a click on a sticky line does.
    pub fn go_to_line(&mut self, line: usize, cx: &mut Context<Self>) {
        self.go_to(Spot::Line { line, column: 0 }, cx);
    }

    fn page_rows(&self) -> usize {
        (f32::from(self.viewport.height) / self.style.line_height)
            .floor()
            .max(1.) as usize
    }

    /// Alt+Up and Alt+Down: the previous or the next member of the class.
    fn member_step(&mut self, forward: bool, cx: &mut Context<Self>) {
        let Some(state) = &self.state else {
            return;
        };
        let head = state.head();
        let mut starts = self.member_starts(head, state);
        starts.sort_unstable();
        starts.dedup();
        let target = if forward {
            starts.iter().copied().find(|start| *start > head)
        } else {
            starts.iter().rev().copied().find(|start| *start < head)
        };
        let Some(target) = target else {
            return;
        };
        let Some(state) = &mut self.state else {
            return;
        };
        state.select_range(target..target);
        self.autoscroll = true;
        self.caret_on = true;
        cx.notify();
    }

    /// The starts of the member declarations, from the outline when it has them.
    fn member_starts(&self, head: usize, state: &EditorState) -> Vec<usize> {
        if !self.outline.is_empty() {
            let text = state.text();
            let line_start = |offset: usize| {
                text[..offset.min(text.len())]
                    .rfind('\n')
                    .map_or(0, |index| index + 1)
            };
            let class = self
                .outline
                .iter()
                .filter(|item| {
                    matches!(
                        item.kind,
                        OutlineKind::Class | OutlineKind::Interface | OutlineKind::Enum
                    ) && item.range.start <= head
                        && head <= item.range.end
                })
                .min_by_key(|item| item.range.len());
            let members: Vec<usize> = match class {
                Some(class) => self
                    .outline
                    .iter()
                    .filter(|item| {
                        item.depth == class.depth + 1
                            && class.range.start <= item.range.start
                            && item.range.end <= class.range.end
                    })
                    .map(|item| line_start(item.range.start))
                    .collect(),
                None => self
                    .outline
                    .iter()
                    .filter(|item| item.kind != OutlineKind::Module)
                    .map(|item| line_start(item.range.start))
                    .collect(),
            };
            if !members.is_empty() {
                return members;
            }
        }
        crate::blocks::member_starts(&state.text(), head)
    }

    /// Ctrl+Up and Ctrl+Down: scroll a line and keep the caret in view.
    fn scroll_step(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(state) = &mut self.state else {
            return;
        };
        let line_height = self.style.line_height.max(1.);
        let display = state.display();
        let rows = display.row_count();
        let caret_row = display.to_display(state.head()).row as isize;
        let visible = (f32::from(self.viewport.height) / line_height)
            .floor()
            .max(1.) as isize;
        let last = (rows.saturating_sub(visible as usize)) as isize;
        let top = (self.scroll_top / line_height).round() as isize;
        let new_top = (top + delta).clamp(0, last.max(0));
        drop(display);
        self.scroll_top = new_top as f32 * line_height;
        if caret_row < new_top {
            state.perform(actions::MOVE_DOWN, 1);
        } else if caret_row > new_top + visible - 1 {
            state.perform(actions::MOVE_UP, 1);
        }
        self.autoscroll = false;
        self.caret_on = true;
        self.semantic_tokens_scrolled(cx);
        self.inlays_if_needed(cx);
        cx.notify();
    }

    /// Ctrl+Alt+Shift+Left and Right in the editor. A caret outside a list leaves the
    /// key to the window, which moves the focus between the split groups.
    fn move_element_action(&mut self, forward: bool, cx: &mut Context<Self>) -> bool {
        let Some(state) = &self.state else {
            return false;
        };
        if !state.has_element_at_caret() {
            return false;
        }
        let action = if forward {
            actions::MOVE_ELEMENT_RIGHT
        } else {
            actions::MOVE_ELEMENT_LEFT
        };
        let change = self
            .state
            .as_mut()
            .and_then(|state| state.perform(action, 1));
        self.changed(change, cx);
        true
    }

    /// Ctrl+M: the caret line goes to the middle of the view, the caret stays.
    fn scroll_to_center(&mut self, cx: &mut Context<Self>) {
        let Some(state) = &self.state else {
            return;
        };
        let line_height = self.style.line_height.max(1.);
        let display = state.display();
        let row = display.to_display(state.head()).row as f32;
        let height = f32::from(self.viewport.height).max(line_height);
        let last = (display.row_count() as f32 * line_height - height).max(0.);
        let margin = (height - line_height) / 2.;
        self.scroll_top = (row * line_height - margin).clamp(0., last);
        self.autoscroll = false;
        self.caret_on = true;
        self.inlays_if_needed(cx);
        cx.notify();
    }

    /// Ctrl+PageUp and Ctrl+PageDown: the first or the last line on screen, with Shift
    /// the selection grows there.
    fn move_to_page(&mut self, bottom: bool, extend: bool, cx: &mut Context<Self>) {
        let rows = self.page_rows().max(1);
        let line_height = self.style.line_height.max(1.);
        let Some(state) = &mut self.state else {
            return;
        };
        let display = state.display();
        let first = (self.scroll_top / line_height).floor().max(0.) as usize;
        let last = display.row_count().saturating_sub(1);
        let row = if bottom {
            (first + rows - 1).min(last)
        } else {
            first.min(last)
        };
        let target = display.to_offset(DisplayPoint { row, column: 0 }, Bias::Left);
        drop(display);
        state.select_to(target, extend);
        self.autoscroll = true;
        self.caret_on = true;
        cx.notify();
    }

    /// Alt+Q: the header of the declaration around the caret, when it has scrolled
    /// above the first visible row.
    fn show_context_info(&mut self, cx: &mut Context<Self>) {
        let Some(state) = &self.state else {
            return;
        };
        let head = state.head();
        let (start, end) = match self.context_lines(head) {
            Some(lines) => lines,
            None => return,
        };
        let display = state.display();
        let row = display.to_display(start).row;
        let line_height = self.style.line_height.max(1.);
        let first = (self.scroll_top / line_height).floor().max(0.) as usize;
        if row >= first {
            return;
        }
        let store = state.store();
        let lines: Vec<String> = (store.offset_to_line(start)..=store.offset_to_line(end))
            .filter(|line| *line < store.line_count())
            .map(|line| store.line_text(line).trim_end().to_owned())
            .collect();
        if lines.is_empty() {
            return;
        }
        self.context_info = Some(ContextInfo { head, lines });
        cx.notify();
    }

    /// The byte range of the declaration header around `head`: the start of its first
    /// line and the opening brace line inclusive.
    fn context_lines(&self, head: usize) -> Option<(usize, usize)> {
        let state = self.state.as_ref()?;
        let store = state.store();
        let outline = self
            .outline
            .iter()
            .filter(|item| item.kind.is_block())
            .filter(|item| item.range.start <= head && head <= item.range.end)
            .min_by_key(|item| item.range.len());
        if let Some(item) = outline {
            let start = store.line_to_offset(store.offset_to_line(item.range.start));
            let mut end_line = store.offset_to_line(item.range.start);
            let limit = (end_line + 8).min(store.line_count());
            for line in end_line..limit {
                if store.line_text(line).contains('{') {
                    end_line = line;
                    break;
                }
            }
            return Some((start, store.line_end(end_line)));
        }
        let text = store.slice(0..store.len());
        let pairs = brace_pairs(&text);
        let (open, _) = enclosing_block(&pairs, head)?;
        let mut start_line = store.offset_to_line(open);
        while start_line > 0 {
            let previous = store.line_text(start_line - 1);
            let trimmed = previous.trim_end();
            if trimmed.trim().is_empty()
                || trimmed.ends_with(';')
                || trimmed.ends_with('}')
                || trimmed.ends_with('{')
            {
                break;
            }
            start_line -= 1;
        }
        Some((
            store.line_to_offset(start_line),
            store.line_end(store.offset_to_line(open)),
        ))
    }

    /// The keymap of the editor now. `install_keymap` replaces it for open editors too.
    pub(crate) fn current_keymap<'a>(&'a self, cx: &'a App) -> &'a Keymap {
        cx.try_global::<Keys>()
            .map_or(self.keymap.as_ref(), |keys| keys.0.as_ref())
    }

    fn on_key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let modifiers = event.keystroke.modifiers;
        // Command is the primary modifier of macOS. On the other systems the Super key is
        // not a shortcut of the editor.
        if modifiers.platform && !cfg!(target_os = "macos") {
            return;
        }
        // The frame after this key finishes the measurement; a run of keys records each.
        self.pending_frames.push(Instant::now());
        let stroke = Keystroke {
            ctrl: modifiers.control,
            alt: modifiers.alt,
            shift: modifiers.shift,
            cmd: modifiers.platform,
            key: event.keystroke.key.to_lowercase(),
        }
        .normalized();
        // Typing and Escape close the documentation; the second Ctrl+Q pins it.
        let quick_key = stroke.ctrl && !stroke.alt && !stroke.shift && stroke.key == "q";
        // Ctrl+A and Ctrl+C work on the text of the popup while it is open.
        if self.quick_doc_open() && stroke.ctrl && !stroke.alt && !stroke.shift {
            match stroke.key.as_str() {
                "a" => {
                    self.select_all_quick_doc(cx);
                    cx.stop_propagation();
                    return;
                }
                "c" => {
                    self.copy_quick_doc_selection(cx);
                    cx.stop_propagation();
                    return;
                }
                _ => {}
            }
        }
        if !quick_key {
            self.cancel_hover();
        }
        if self.quick_doc_open() && !quick_key {
            let escape = stroke.key == "escape" && !stroke.ctrl && !stroke.alt && !stroke.shift;
            self.close_quick_doc(cx);
            if escape {
                cx.stop_propagation();
                return;
            }
        }
        // Any key other than Alt+Q itself closes Context Info; Alt+Q opens it again.
        self.context_info = None;
        if std::mem::take(&mut self.awaiting_mnemonic) {
            let typed = event
                .keystroke
                .key_char
                .as_deref()
                .and_then(|text| text.chars().next());
            if let Some(letter) = typed.filter(|c| c.is_alphanumeric() && !modifiers.control) {
                self.toggle_bookmark(Some(letter.to_ascii_uppercase()), cx);
            }
            cx.stop_propagation();
            return;
        }
        if self.region_popup.is_some() && self.region_key(event, cx) {
            cx.stop_propagation();
            return;
        }
        if self.bar_active() && self.bar_key(event, cx) {
            cx.stop_propagation();
            return;
        }
        // The second key of a chord: Ctrl+* then a digit, for instance.
        if !self.chords.is_empty() {
            let mut keys = self.chords.clone();
            keys.push(stroke.clone());
            let lookup = self.current_keymap(cx).lookup(&keys, &[actions::CONTEXT]);
            match lookup {
                Lookup::Action(action) => {
                    self.chords.clear();
                    cx.stop_propagation();
                    self.run(action, cx);
                    return;
                }
                Lookup::Prefix => {
                    self.chords = keys;
                    cx.stop_propagation();
                    return;
                }
                Lookup::None => self.chords.clear(),
            }
        }
        if self.template_key(&stroke, cx) {
            cx.stop_propagation();
            return;
        }
        if self.definition_key(&stroke.key, cx)
            || self.override_key(event, cx)
            || self.safe_delete_key(&stroke.key, cx)
            || self.generation_key(event, cx)
        {
            cx.stop_propagation();
            return;
        }
        let plain = !(stroke.ctrl || stroke.alt || stroke.shift);
        if plain && self.move_key(&stroke.key, cx) {
            cx.stop_propagation();
            return;
        }
        if plain && self.rename_key(&stroke.key, cx) {
            cx.stop_propagation();
            return;
        }
        // Enter expands a template that chose Enter, when its abbreviation ends here and
        // no list of a popup is open.
        if plain && stroke.key == "enter" && self.template_expand_key(ExpandWith::Enter, cx) {
            cx.stop_propagation();
            return;
        }
        if plain && (self.action_key(&stroke.key, cx) || self.completion_key(&stroke.key, cx)) {
            cx.stop_propagation();
            return;
        }
        // Ctrl+. in an open list inserts the entry and a dot. Outside one it keeps the
        // folding it has in the keymap.
        if stroke.ctrl
            && !stroke.alt
            && !stroke.shift
            && stroke.key == "."
            && self.completion_key("ctrl-.", cx)
        {
            cx.stop_propagation();
            return;
        }
        // Ctrl+Up and Ctrl+Down switch the overload in an open Parameter Info; outside
        // one they scroll as before.
        if stroke.ctrl
            && !stroke.alt
            && !stroke.shift
            && (stroke.key == "up" || stroke.key == "down")
            && self.overload_key(stroke.key == "up", cx)
        {
            cx.stop_propagation();
            return;
        }
        // In an open list this inserts the entry and finishes the statement, as Enter
        // with a list does in IDEA.
        if stroke.ctrl
            && stroke.shift
            && stroke.key == "enter"
            && self.completion_key("ctrl-shift-enter", cx)
        {
            cx.stop_propagation();
            return;
        }
        let lookup = self
            .current_keymap(cx)
            .lookup(std::slice::from_ref(&stroke), &[actions::CONTEXT]);
        match lookup {
            Lookup::Action(action) => {
                cx.stop_propagation();
                self.run(action, cx);
            }
            Lookup::Prefix => {
                self.chords = vec![stroke];
                cx.stop_propagation();
            }
            Lookup::None => {}
        }
    }

    /// Feeds a key to the editor as if it had the focus. A view that shows the buffer
    /// elsewhere uses this to edit the same text without a second editor.
    pub fn handle_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.on_key(event, window, cx);
    }

    /// Runs a keymap action.
    pub fn run(&mut self, action: &str, cx: &mut Context<Self>) {
        // A command counts its edits from the carets and folds here, so the changes of
        // the other view of the document come in first.
        self.catch_up(cx);
        if self.quick_doc_open() {
            if action == actions::SELECT_ALL {
                self.select_all_quick_doc(cx);
                return;
            }
            if action == actions::COPY {
                self.copy_quick_doc_selection(cx);
                return;
            }
        }
        let action = match action {
            actions::NEXT_TEMPLATE_VARIABLE => {
                if let Some(change) = self.template_field(true) {
                    self.changed(change, cx);
                    return;
                }
                // Not in a template: the key is a plain Tab, which also expands an
                // abbreviation right before the caret.
                actions::TAB
            }
            actions::PREVIOUS_TEMPLATE_VARIABLE => {
                if let Some(change) = self.template_field(false) {
                    self.changed(change, cx);
                    return;
                }
                actions::UNINDENT
            }
            other => other,
        };
        match action {
            actions::COMPLETE => return self.complete(true, cx),
            actions::SMART_TYPE_COMPLETION => {
                return self.complete_with(CompletionMode::SmartType, true, cx);
            }
            actions::CLASS_NAME_COMPLETION => {
                return self.complete_with(CompletionMode::ClassName, true, cx);
            }
            actions::EXTERNAL_DOC => return self.external_doc(cx),
            actions::HIPPIE_COMPLETION => return self.hippie(true, cx),
            actions::HIPPIE_BACKWARD_COMPLETION => return self.hippie(false, cx),
            actions::PARAMETER_INFO => return self.parameter_info(cx),
            actions::GOTO_DECLARATION => return self.navigate(NavigationKind::Definition, cx),
            actions::GOTO_IMPLEMENTATION => {
                return self.navigate(NavigationKind::Implementation, cx);
            }
            actions::QUICK_DOC => return self.quick_doc(cx),
            actions::TOGGLE_RENDERED_DOC => {
                self.toggle_rendered_doc_at_caret(cx);
                return;
            }
            actions::NEXT_ERROR => return self.next_error(cx),
            actions::ERROR_DESCRIPTION => return self.error_description(cx),
            actions::FIND_USAGES_IN_FILE => return self.highlight_usages(cx),
            actions::NEXT_OCCURRENCE => {
                if !self.occurrence_step(true, cx) {
                    self.find_step(true, cx);
                }
                return;
            }
            actions::PREVIOUS_OCCURRENCE => {
                if !self.occurrence_step(false, cx) {
                    self.find_step(false, cx);
                }
                return;
            }
            actions::CHOOSE_LOOKUP_ITEM => {
                if self.completion_key("enter", cx) {
                    return;
                }
            }
            actions::CHOOSE_LOOKUP_ITEM_REPLACE => {
                if self.completion_key("tab", cx) {
                    return;
                }
            }
            actions::CHOOSE_LOOKUP_ITEM_DOT => {
                if self.completion_key("ctrl-.", cx) {
                    return;
                }
            }
            actions::LOOKUP_UP => {
                if self.completion_key("up", cx) {
                    return;
                }
            }
            actions::LOOKUP_DOWN => {
                if self.completion_key("down", cx) {
                    return;
                }
            }
            actions::METHOD_OVERLOAD_UP => {
                if self.overload_key(true, cx) {
                    return;
                }
            }
            actions::METHOD_OVERLOAD_DOWN => {
                if self.overload_key(false, cx) {
                    return;
                }
            }
            actions::NEXT_PARAMETER => {
                if self.parameter_field(true, cx) {
                    return;
                }
            }
            actions::PREVIOUS_PARAMETER => {
                if self.parameter_field(false, cx) {
                    return;
                }
            }
            actions::EXPAND_LIVE_TEMPLATE | actions::INDENT_SELECTION => {
                // The key of both is Tab, which the view handles before the action table.
            }
            actions::REFORMAT => return self.reformat(cx),
            actions::OPTIMIZE_IMPORTS => return self.optimize_imports(cx),
            actions::SHOW_ACTIONS => return self.show_actions(cx),
            actions::FIND_USAGES => return self.find_usages(cx),
            actions::SHOW_USAGES => return self.show_usages(cx),
            actions::RENAME => return self.start_rename(cx),
            actions::EXTRACT_METHOD => return self.refactor(Refactoring::ExtractMethod, cx),
            actions::EXTRACT_VARIABLE => return self.refactor(Refactoring::ExtractVariable, cx),
            actions::EXTRACT_FIELD => return self.refactor(Refactoring::ExtractField, cx),
            actions::EXTRACT_CONSTANT => return self.refactor(Refactoring::ExtractConstant, cx),
            actions::INLINE => return self.refactor(Refactoring::Inline, cx),
            actions::INTRODUCE_PARAMETER => {
                return self.refactor(Refactoring::IntroduceParameter, cx);
            }
            actions::REFACTOR_THIS => return self.refactor_this(cx),
            actions::GENERATE => return self.generate_menu(cx),
            actions::EXPRESSION_TYPE => return self.expression_type(cx),
            actions::GOTO_SUPER => return self.navigate(NavigationKind::SuperMethod, cx),
            actions::TYPE_HIERARCHY => return self.type_hierarchy(cx),
            actions::MOVE => return self.start_move(cx),
            actions::CHANGE_SIGNATURE => return self.change_signature(cx),
            actions::OVERRIDE_METHODS => return self.override_methods(false, cx),
            actions::IMPLEMENT_METHODS => return self.override_methods(true, cx),
            actions::CALL_HIERARCHY => return self.call_hierarchy(cx),
            actions::GOTO_TYPE_DECLARATION => return self.type_declaration(cx),
            actions::QUICK_DEFINITION => return self.quick_definition(cx),
            actions::COPY_REFERENCE => return self.copy_reference(cx),
            actions::SAFE_DELETE => return self.safe_delete(cx),
            actions::UNWRAP => return self.unwrap_remove(cx),
            actions::METHOD_HIERARCHY => return self.method_hierarchy(cx),
            actions::SCROLL_TO_CENTER => return self.scroll_to_center(cx),
            actions::MOVE_TO_PAGE_TOP => return self.move_to_page(false, false, cx),
            actions::MOVE_TO_PAGE_BOTTOM => return self.move_to_page(true, false, cx),
            actions::MOVE_TO_PAGE_TOP_WITH_SELECTION => {
                return self.move_to_page(false, true, cx);
            }
            actions::MOVE_TO_PAGE_BOTTOM_WITH_SELECTION => {
                return self.move_to_page(true, true, cx);
            }
            actions::CONTEXT_INFO => return self.show_context_info(cx),
            actions::GOTO_CUSTOM_REGION => return self.open_region_popup(cx),
            _ => {}
        }
        let rows = self.page_rows();
        if let Some(change) = self.template_action(action, cx) {
            self.changed(change, cx);
            return;
        }
        if action == actions::TAB
            && let Some(state) = &mut self.state
            && state.brace_or_quote_out()
        {
            self.changed(None, cx);
            return;
        }
        match action {
            actions::FIND => return self.open_find(false, cx),
            actions::REPLACE => return self.open_find(true, cx),
            actions::GOTO_LINE => return self.open_goto(cx),
            actions::FIND_NEXT if self.usages.is_some() => return self.step_usage(true, cx),
            actions::FIND_PREVIOUS if self.usages.is_some() => return self.step_usage(false, cx),
            actions::FIND_NEXT => return self.find_step(true, cx),
            actions::FIND_PREVIOUS => return self.find_step(false, cx),
            actions::FIND_WORD_AT_CARET => return self.find_word_at_caret(true, cx),
            actions::FIND_PREV_WORD_AT_CARET => return self.find_word_at_caret(false, cx),
            actions::HIGHLIGHT_USAGES => return self.highlight_usages(cx),
            actions::METHOD_UP => return self.member_step(false, cx),
            actions::METHOD_DOWN => return self.member_step(true, cx),
            actions::SCROLL_UP => return self.scroll_step(-1, cx),
            actions::SCROLL_DOWN => return self.scroll_step(1, cx),
            actions::CLEAR_CARETS
                if self.bar.is_some()
                    || self.usages.is_some()
                    || self
                        .lsp
                        .as_ref()
                        .is_some_and(|lsp| lsp.signatures.is_some()) =>
            {
                self.usages = None;
                if let Some(lsp) = &mut self.lsp {
                    lsp.highlights.clear();
                    lsp.signatures = None;
                    lsp.parameter_walk = None;
                }
                if self.bar.is_some() {
                    return self.close_bar(cx);
                }
                return cx.notify();
            }
            actions::COLUMN_MODE => {
                self.column_mode = !self.column_mode;
                return;
            }
            actions::PASTE_HISTORY => return cx.emit(EditorEvent::PasteHistory),
            actions::TOGGLE_SOFT_WRAP => {
                if !self.wrap_locked {
                    self.soft_wrap = !self.soft_wrap;
                    self.scroll_left = 0.;
                }
                return cx.notify();
            }
            actions::TOGGLE_WHITESPACE => {
                self.show_whitespace = !self.show_whitespace;
                return cx.notify();
            }
            actions::TOGGLE_BOOKMARK => return self.toggle_bookmark(None, cx),
            actions::BOOKMARK_MNEMONIC => {
                self.awaiting_mnemonic = true;
                return;
            }
            actions::SHOW_BOOKMARKS | actions::SHOW_TYPE_BOOKMARKS => {
                return cx.emit(EditorEvent::ShowBookmarks);
            }
            other => {
                if let Some(digit) = actions::SET_BOOKMARKS.iter().position(|id| *id == other) {
                    return self.toggle_bookmark(char::from_digit(digit as u32, 10), cx);
                }
                if let Some(digit) = actions::GOTO_BOOKMARKS.iter().position(|id| *id == other) {
                    return self.goto_bookmark(char::from_digit(digit as u32, 10), cx);
                }
            }
        }
        let Some(state) = &mut self.state else {
            return;
        };
        let change = match action {
            actions::SAVE => {
                self.save(cx);
                cx.emit(EditorEvent::SaveAll);
                return;
            }
            actions::COPY => {
                let text = state.copy_text();
                remember_clip(cx, text.clone());
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                return;
            }
            actions::CUT => {
                let text = state.copy_text();
                remember_clip(cx, text.clone());
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                state.cut()
            }
            actions::PASTE => {
                let text = cx.read_from_clipboard().and_then(|item| item.text());
                text.and_then(|text| state.paste(&text))
            }
            actions::PASTE_SIMPLE => {
                let text = cx.read_from_clipboard().and_then(|item| item.text());
                text.and_then(|text| state.type_text(&text))
            }
            _ => state.perform(action, rows),
        };
        self.changed(change, cx);
    }

    pub(crate) fn scroll_by(&mut self, delta: Point<Pixels>, cx: &mut Context<Self>) {
        self.cancel_hover();
        if self.quick_doc_open() {
            self.close_quick_doc(cx);
        }
        let line_height = self.style.line_height;
        let rows = self
            .state
            .as_ref()
            .map_or(1, |state| state.display().row_count());
        self.scroll_top =
            (self.scroll_top - f32::from(delta.y)).clamp(0., (rows - 1) as f32 * line_height);
        self.scroll_left = (self.scroll_left - f32::from(delta.x)).max(0.);
        self.semantic_tokens_scrolled(cx);
        self.inlays_if_needed(cx);
        cx.emit(EditorEvent::Scrolled(self.scroll_top));
        cx.notify();
    }

    /// How far the text may scroll sideways, from the bar of the last frame.
    fn scroll_max_left(&self) -> f32 {
        self.layout
            .as_ref()
            .and_then(|layout| layout.horizontal)
            .map_or(0., |bar| (bar.content - bar.viewport).max(0.))
    }

    /// Puts the first visible row at `top`, as a drag of the vertical thumb does.
    fn set_scroll_top(&mut self, top: f32, cx: &mut Context<Self>) {
        let line_height = self.style.line_height.max(1.);
        let rows = self
            .state
            .as_ref()
            .map_or(1, |state| state.display().row_count());
        let height = f32::from(self.viewport.height).max(0.);
        let max_top = (rows as f32 * line_height - height).max(0.);
        self.scroll_top = top.clamp(0., max_top);
        self.autoscroll = false;
        self.caret_on = true;
        self.semantic_tokens_scrolled(cx);
        self.inlays_if_needed(cx);
        cx.emit(EditorEvent::Scrolled(self.scroll_top));
        cx.notify();
    }

    /// Puts the first visible column at `left`, as a drag of the horizontal thumb does.
    fn set_scroll_left(&mut self, left: f32, cx: &mut Context<Self>) {
        let max_left = self.scroll_max_left();
        self.scroll_left = left.clamp(0., max_left);
        self.caret_on = true;
        cx.notify();
    }

    fn in_gutter(&self, position: Point<Pixels>) -> bool {
        self.layout
            .as_ref()
            .is_some_and(|layout| position.x < layout.bounds.left() + px(GUTTER_WIDTH))
    }

    /// Whether a right click at `position` belongs to the gutter or to the blame column,
    /// where the menu offers the annotation.
    pub fn gutter_menu_at(&self, position: Point<Pixels>) -> bool {
        self.in_gutter(position) || self.in_annotations(position)
    }

    /// Row and text column under `position`. Outside the text it clamps to the nearest.
    pub(crate) fn hit(&self, position: Point<Pixels>) -> Option<(usize, usize)> {
        let layout = self.layout.as_ref()?;
        let line_height = px(self.style.line_height);
        // Rows stand where the frame put them: the panel of sticky lines moves them.
        let shown = layout
            .rows
            .iter()
            .position(|laid_out| position.y < laid_out.y + line_height)
            .unwrap_or(layout.rows.len().checked_sub(1)?);
        let x = position.x - layout.bounds.left() - px(self.gutter_room() + TEXT_INSET)
            + px(self.scroll_left);
        Some((
            shown,
            layout.rows[shown].text.closest_index_for_x(x.max(px(0.))),
        ))
    }

    pub(crate) fn offset_at(&self, position: Point<Pixels>) -> Option<usize> {
        let (shown, column) = self.hit(position)?;
        let row = self.layout.as_ref()?.rows.get(shown)?;
        // A block row speaks for the line under it.
        Some(row.line.offset_of(column, Bias::Left))
    }

    /// Whether `position` is in the blame column left of the text.
    fn in_annotations(&self, position: Point<Pixels>) -> bool {
        let Some(layout) = &self.layout else {
            return false;
        };
        let width = self.annotation_width();
        width > 0.
            && position.x >= layout.bounds.left() + px(GUTTER_WIDTH)
            && position.x < layout.bounds.left() + px(GUTTER_WIDTH + width)
    }

    /// The label of the blame column under `position`, the line it belongs to and where
    /// the label stands.
    fn annotation_under(
        &self,
        position: Point<Pixels>,
    ) -> Option<(&BlameNote, usize, Point<Pixels>)> {
        if !self.in_annotations(position) {
            return None;
        }
        let layout = self.layout.as_ref()?;
        let line_height = px(self.style.line_height);
        let shown = layout.rows.iter().find(|laid_out| {
            !laid_out.is_block
                && !laid_out.line.continuation
                && position.y >= laid_out.y
                && position.y < laid_out.y + line_height
        })?;
        let note = self
            .state
            .as_ref()?
            .annotation_label_at(shown.buffer_line)?;
        Some((note, shown.buffer_line, point(position.x, shown.y)))
    }

    /// The hint of the blame label under the pointer: its text and where it stands.
    fn annotate_hint_at(&self, position: Point<Pixels>) -> Option<(String, Point<Pixels>)> {
        let (note, _, at) = self.annotation_under(position)?;
        Some((note.details.clone(), at))
    }

    /// A press on a scroll bar: a mark goes to its line, the thumb is grabbed and the
    /// track scrolls to the pointer. True when the press belonged to a bar.
    fn bar_pressed(
        &mut self,
        position: Point<Pixels>,
        layout: &Layout,
        cx: &mut Context<Self>,
    ) -> bool {
        if layout
            .vertical_area
            .is_some_and(|area| inside(area, position))
        {
            if let Some(mark) = layout
                .stripes
                .iter()
                .find(|mark| inside(mark.bounds, position))
            {
                let line = mark.line;
                self.stripe_hint = None;
                self.go_to_line(line, cx);
                return true;
            }
            if let Some(bar) = layout.vertical
                && let Some(thumb) = bar.thumb(self.scroll_top)
            {
                let top = bar.track_start + thumb.offset;
                let y = f32::from(position.y);
                let held = y >= top && y <= top + thumb.len;
                let grab = if held { y - top } else { thumb.len / 2. };
                if !held {
                    let scroll = bar.scroll_at(y, grab);
                    self.set_scroll_top(scroll, cx);
                }
                self.scrollbar_drag = Some(Drag {
                    axis: Axis::Vertical,
                    grab,
                });
            }
            return true;
        }
        if layout
            .horizontal_area
            .is_some_and(|area| inside(area, position))
        {
            if let Some(bar) = layout.horizontal
                && let Some(thumb) = bar.thumb(self.scroll_left)
            {
                let start = bar.track_start + thumb.offset;
                let x = f32::from(position.x);
                let held = x >= start && x <= start + thumb.len;
                let grab = if held { x - start } else { thumb.len / 2. };
                if !held {
                    let scroll = bar.scroll_at(x, grab);
                    self.set_scroll_left(scroll, cx);
                }
                self.scrollbar_drag = Some(Drag {
                    axis: Axis::Horizontal,
                    grab,
                });
            }
            return true;
        }
        false
    }

    pub(crate) fn mouse_down(&mut self, event: &MouseDownEvent, cx: &mut Context<Self>) {
        self.cancel_hover();
        self.close_quick_doc(cx);
        let Some(layout) = self.layout.clone() else {
            return;
        };
        if self.bar_pressed(event.position, &layout, cx) {
            return;
        }
        let sticky_height =
            crate::element::sticky_height(self.sticky.len(), self.style.line_height);
        if sticky_height > 0. && f32::from(event.position.y - layout.bounds.top()) < sticky_height {
            let at = (f32::from(event.position.y - layout.bounds.top()) / self.style.line_height)
                as usize;
            if let Some(line) = self.sticky.get(at).copied() {
                self.go_to(Spot::Line { line, column: 0 }, cx);
            }
            return;
        }
        let Some((shown, column)) = self.hit(event.position) else {
            return;
        };
        // The blame column takes the click: a label opens its commit, the rest is dead.
        if self.in_annotations(event.position) {
            let row = &layout.rows[shown];
            let labelled = !row.is_block && !row.line.continuation;
            let commit = labelled
                .then_some(self.state.as_ref())
                .flatten()
                .and_then(|state| state.annotation_label_at(row.buffer_line))
                .filter(|note| note.has_commit())
                .map(|note| note.commit.clone());
            if let Some(commit) = commit {
                cx.emit(EditorEvent::BlameCommit(commit));
            }
            return;
        }
        if layout.rows[shown].is_doc {
            if let Some(link) = layout.rows[shown]
                .doc_links
                .iter()
                .find(|link| inside(link.bounds, event.position))
            {
                self.open_doc_link(link.target.clone(), cx);
                return;
            }
            if let Some(range) = layout.rows[shown].doc_range.clone() {
                self.toggle_rendered_doc_at(range.clone(), cx);
                if let Some(state) = &mut self.state {
                    state.select_range(range.start..range.start);
                }
                self.caret_on = true;
                cx.notify();
            }
            return;
        }
        if layout.rows[shown].is_block {
            if let Some((index, label)) = layout.rows[shown]
                .block_labels
                .iter()
                .enumerate()
                .find(|(_, label)| inside(label.bounds, event.position))
            {
                self.code_vision_hovered = Some((layout.rows[shown].row, index));
                let kind = label.kind;
                let line = layout.rows[shown].buffer_line;
                self.activate_code_lens(line, kind, cx);
                return;
            }
            // An empty place of a block row belongs to the line under it.
            let offset = layout.rows[shown].line.offset_of(column, Bias::Left);
            let modifiers = event.modifiers;
            let add_caret = modifiers.alt;
            if let Some(state) = &mut self.state {
                state.click(
                    offset,
                    modifiers.shift && !add_caret,
                    add_caret,
                    event.click_count,
                );
            }
            if let Some(lsp) = &mut self.lsp {
                lsp.actions = None;
            }
            self.selecting = true;
            self.caret_on = true;
            self.highlight_later(cx);
            cx.notify();
            return;
        }
        let line = &layout.rows[shown].line;
        let run_line = self
            .state
            .as_ref()
            .map(|state| state.store().offset_to_line(line.start_offset));
        if let Some(buffer_line) = run_line
            && !line.continuation
            && event.position.x - layout.bounds.left() < px(crate::element::FOLD_X)
            && self
                .run_marks
                .iter()
                .any(|mark| mark.line as usize == buffer_line)
        {
            let event = if event.modifiers.alt {
                EditorEvent::DebugRunMark(buffer_line as u32)
            } else {
                EditorEvent::RunMark(buffer_line as u32)
            };
            cx.emit(event);
            return;
        }
        if let Some(buffer_line) = run_line
            && !line.continuation
            && layout.rows[shown].doc_icon.is_some()
        {
            let x = event.position.x - layout.bounds.left();
            if x >= px(crate::element::FOLD_X) && x < px(crate::element::FOLD_X + 16.) {
                if let Some(range) = self.doc_range_at_line(buffer_line) {
                    self.toggle_rendered_doc_at(range, cx);
                }
                return;
            }
        }
        self.catch_up(cx);
        let Some(state) = &mut self.state else {
            return;
        };
        if position_in_fold_column(event.position, layout.bounds) {
            state.toggle_fold_at_line(state.store().offset_to_line(line.start_offset));
            cx.notify();
            return;
        }
        let in_gutter = event.position.x - layout.bounds.left() < px(GUTTER_WIDTH);
        if in_gutter && !line.continuation {
            if let Some(buffer_line) = run_line {
                cx.emit(EditorEvent::GutterClick(buffer_line as u32));
            }
            return;
        }
        let placeholder = line.chunks.iter().find_map(|(range, chunk)| match chunk {
            DisplayChunk::Placeholder { fold_start, .. } if range.contains(&column) => {
                Some(*fold_start)
            }
            _ => None,
        });
        if let Some(fold_start) = placeholder {
            state.expand_fold(fold_start);
            cx.notify();
            return;
        }
        let modifiers = event.modifiers;
        let offset = line.offset_of(column, Bias::Left);
        // Ctrl+click is Ctrl+B on the clicked place.
        if modifiers.control && !modifiers.shift && !modifiers.alt {
            state.select_range(offset..offset);
            self.changed(None, cx);
            self.navigate(NavigationKind::Definition, cx);
            return;
        }
        let block = (modifiers.alt && !modifiers.shift) || self.column_mode;
        let add_caret = modifiers.alt;
        let kept = state.carets();
        state.click(
            offset,
            modifiers.shift && !add_caret,
            add_caret,
            event.click_count,
        );
        self.block_drag = block.then(|| (offset, if modifiers.alt { kept } else { Vec::new() }));
        if let Some(bar) = &mut self.bar {
            bar.active = false;
        }
        self.selecting = true;
        self.caret_on = true;
        if let Some(lsp) = &mut self.lsp {
            lsp.actions = None;
        }
        self.highlight_later(cx);
        cx.notify();
    }

    /// A right click puts the caret where the pointer is, unless it lands inside a
    /// selection, and then asks the window for the menu of the editor.
    pub(crate) fn right_click(&mut self, event: &MouseDownEvent, cx: &mut Context<Self>) {
        // The blame column has no text: the menu opens and the caret stays where it is.
        if self.in_annotations(event.position) {
            cx.emit(EditorEvent::ContextMenu(event.position));
            return;
        }
        let Some(offset) = self.offset_at(event.position) else {
            return;
        };
        let inside = self.state.as_ref().is_some_and(|state| {
            state.selections().iter().any(|selection| {
                !selection.is_empty() && selection.start() <= offset && offset <= selection.end()
            })
        });
        if !inside {
            if let Some(state) = &mut self.state {
                state.click(offset, false, false, 1);
            }
            self.caret_on = true;
            self.highlight_later(cx);
            cx.notify();
        }
        cx.emit(EditorEvent::ContextMenu(event.position));
    }

    pub(crate) fn mouse_moved(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if let Some(drag) = self.scrollbar_drag
            && event.dragging()
            && let Some(layout) = self.layout.clone()
        {
            match drag.axis {
                Axis::Vertical => {
                    if let Some(bar) = layout.vertical {
                        let scroll = bar.scroll_at(f32::from(event.position.y), drag.grab);
                        self.set_scroll_top(scroll, cx);
                    }
                }
                Axis::Horizontal => {
                    if let Some(bar) = layout.horizontal {
                        let scroll = bar.scroll_at(f32::from(event.position.x), drag.grab);
                        self.set_scroll_left(scroll, cx);
                    }
                }
            }
            return;
        }

        let over_bar = self.layout.as_ref().is_some_and(|layout| {
            layout
                .vertical_area
                .is_some_and(|area| inside(area, event.position))
                || layout
                    .horizontal_area
                    .is_some_and(|area| inside(area, event.position))
        });
        if over_bar != self.scrollbar_hovered {
            self.scrollbar_hovered = over_bar;
            cx.notify();
        }

        let hint = self.layout.as_ref().and_then(|layout| {
            layout
                .stripes
                .iter()
                .find(|mark| inside(mark.bounds, event.position))
                .map(|mark| {
                    let text = match &mark.message {
                        Some(message) => message.clone(),
                        None => match mark.kind {
                            StripeKind::Find => "Search match".to_owned(),
                            StripeKind::Vcs => "Changed line".to_owned(),
                            StripeKind::Caret => "Caret line".to_owned(),
                            StripeKind::Error | StripeKind::Warning => String::new(),
                        },
                    };
                    (text, mark.bounds.center())
                })
        });
        let hint = hint.filter(|(text, _)| !text.is_empty());
        if hint != self.stripe_hint {
            self.stripe_hint = hint;
            cx.notify();
        }

        let annotate_hint = self.annotate_hint_at(event.position);
        if annotate_hint != self.annotate_hint {
            self.annotate_hint = annotate_hint;
            cx.notify();
        }

        let hovered = self.in_gutter(event.position);
        if hovered != self.gutter_hovered {
            self.gutter_hovered = hovered;
            cx.notify();
        }
        if !event.dragging() && !self.selecting {
            self.hover_moved(event.position, cx);
        }

        let over_label = self.layout.as_ref().and_then(|layout| {
            layout.rows.iter().find_map(|row| {
                row.block_labels
                    .iter()
                    .position(|label| inside(label.bounds, event.position))
                    .map(|index| (row.row, index))
            })
        });
        if over_label != self.code_vision_hovered {
            self.code_vision_hovered = over_label;
            cx.notify();
        }
        if !self.selecting || !event.dragging() {
            return;
        }
        let offset = self.offset_at(event.position);
        if let (Some(offset), Some(state)) = (offset, &mut self.state) {
            match &self.block_drag {
                Some((origin, kept)) => state.select_block(kept, *origin, offset),
                None => state.drag_to(offset),
            }
            self.autoscroll = true;
            cx.notify();
        }
    }

    pub(crate) fn mouse_up(&mut self) {
        self.selecting = false;
        self.block_drag = None;
        self.scrollbar_drag = None;
    }
}

fn position_in_fold_column(position: Point<Pixels>, bounds: Bounds<Pixels>) -> bool {
    let x = position.x - bounds.left();
    x >= px(crate::element::FOLD_X) && x < px(GUTTER_WIDTH)
}

/// Whether `position` is inside `area`, edges of the left and top included.
pub(crate) fn inside(area: Bounds<Pixels>, position: Point<Pixels>) -> bool {
    position.x >= area.left()
        && position.x < area.right()
        && position.y >= area.top()
        && position.y < area.bottom()
}

impl EditorView {
    /// The declarations around the caret as a path under the text.
    fn render_crumbs(&self, cx: &mut Context<Self>) -> Option<Div> {
        let around = self.enclosing();
        if around.is_empty() {
            return None;
        }
        let style = self.style.clone();
        let mut row = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(4.))
            .h(px(24.))
            .px(px(10.))
            .w_full()
            .overflow_hidden()
            .whitespace_nowrap()
            .bg(style.panel)
            .border_t_1()
            .border_color(style.panel_border)
            .text_size(px(12.))
            .text_color(style.line_number_current)
            .debug_selector(|| "editor-crumbs".to_owned());
        for (index, item) in around.into_iter().enumerate() {
            if index > 0 {
                row = row.child(div().text_color(style.line_number).child("\u{203a}"));
            }
            let at = item.name_range.start;
            row = row.child(
                div()
                    .id(("crumb", index))
                    .debug_selector(move || format!("crumb-{index}"))
                    .cursor_pointer()
                    .hover(|this| this.text_color(style.code))
                    .child(SharedString::from(item.name.clone()))
                    .on_click(cx.listener(move |this, _, _, cx| this.go_to(Spot::Offset(at), cx))),
            );
        }
        Some(row)
    }
}

impl Drop for EditorView {
    fn drop(&mut self) {
        self.syntax.close(self.document);
        if let Some(lsp) = &self.lsp {
            lsp.close();
        }
    }
}

impl Focusable for EditorView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

/// The point of the corner [`Anchor`] of a popup of `height` whose line starts at
/// `line_top`: under the line when it fits in `editor`, above it with a small gap
/// otherwise, and inside the editor. Anchoring the bottom corner above keeps the list off
/// the line even when it grows after the position is chosen.
pub(crate) fn popup_placement(
    x: Pixels,
    line_top: Pixels,
    line_height: Pixels,
    height: f32,
    editor: Bounds<Pixels>,
) -> (Point<Pixels>, Anchor) {
    let margin = px(4.);
    let gap = px(2.);
    let height = px(height);
    let editor_top = editor.top() + margin;
    let editor_bottom = editor.bottom() - margin;
    let below = line_top + line_height;
    let above = line_top - gap;
    if below + height <= editor_bottom {
        (point(x, below), Anchor::TopLeft)
    } else if above - height >= editor_top {
        (point(x, above), Anchor::BottomLeft)
    } else {
        (
            point(x, (editor_bottom - height).max(editor_top)),
            Anchor::TopLeft,
        )
    }
}

/// The number of class groups in an Override or Implement list.
fn override_groups(popup: &crate::hierarchy::OverridePopup) -> usize {
    let mut groups = 0;
    let mut owner: Option<&str> = None;
    for index in &popup.shown {
        let method = &popup.methods[*index];
        if owner != Some(method.owner.as_str()) {
            owner = Some(method.owner.as_str());
            groups += 1;
        }
    }
    groups
}

impl EditorView {
    /// Under the character at `offset`, in window coordinates, from the last frame.
    fn below(&self, offset: usize) -> Option<Point<Pixels>> {
        let (layout, state) = (self.layout.as_ref()?, self.state.as_ref()?);
        let at = state.display().to_display(offset);
        let shown = layout.rows.iter().find(|shown| shown.row == at.row)?;
        let x = layout.bounds.left() + px(self.gutter_room() + TEXT_INSET) - px(self.scroll_left)
            + shown.text.x_for_index(at.column);
        Some(point(x, shown.y + px(self.style.line_height)))
    }

    /// Where the menu of the editor opens for the Menu key: under the caret, or at the
    /// top of the text before the first frame.
    pub fn context_menu_position(&self, _: &App) -> Point<Pixels> {
        self.head_offset()
            .and_then(|offset| self.below(offset))
            .or_else(|| {
                self.layout
                    .as_ref()
                    .map(|layout| point(layout.bounds.left(), layout.bounds.top()))
            })
            .unwrap_or_else(|| point(px(0.), px(0.)))
    }

    /// The point and the corner of a popup of `height` at `offset`: under the line when
    /// it fits in the editor, above it otherwise, and never outside the editor.
    pub(crate) fn popup_position(
        &self,
        offset: usize,
        height: f32,
    ) -> Option<(Point<Pixels>, Anchor)> {
        let (layout, state) = (self.layout.as_ref()?, self.state.as_ref()?);
        let at = state.display().to_display(offset);
        let shown = layout.rows.iter().find(|shown| shown.row == at.row)?;
        let x = layout.bounds.left() + px(self.gutter_room() + TEXT_INSET) - px(self.scroll_left)
            + shown.text.x_for_index(at.column);
        Some(popup_placement(
            x,
            shown.y,
            px(self.style.line_height),
            height,
            layout.bounds,
        ))
    }

    fn popups(&self, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = *cx.global::<Theme>();
        let mut popups = Vec::new();
        if let Some(info) = &self.context_info
            && let Some(layout) = &self.layout
        {
            let text: String = info.lines.join("\n");
            let body = surface(&theme)
                .debug_selector(|| "context-info-popup".to_owned())
                .max_w(px(720.))
                .p_2()
                .font_family(self.style.font_family.clone())
                .text_size(px(self.style.font_size))
                .text_color(self.style.code)
                .whitespace_nowrap()
                .child(text);
            popups.push(
                deferred(
                    anchored()
                        .position(point(layout.bounds.left(), layout.bounds.top()))
                        .snap_to_window_with_margin(px(SPACE_M))
                        .child(body),
                )
                .with_priority(1)
                .into_any_element(),
            );
        }
        if let Some(popup) = &self.region_popup {
            popups.extend(self.render_region_popup(popup, &theme));
        }
        if let Some((text, position)) = &self.stripe_hint {
            let body = surface(&theme)
                .debug_selector(|| "editor-stripe-hint".to_owned())
                .max_w(px(480.))
                .p_2()
                .text_size(px(UI_SIZE))
                .text_color(theme.text)
                .child(text.clone());
            popups.push(
                deferred(
                    anchored()
                        .position(point(position.x + px(12.), position.y + px(16.)))
                        .snap_to_window_with_margin(px(SPACE_M))
                        .child(body),
                )
                .with_priority(1)
                .into_any_element(),
            );
        }
        if let Some((text, position)) = &self.annotate_hint {
            let rows: Vec<AnyElement> = text
                .lines()
                .map(|line| {
                    div()
                        .whitespace_nowrap()
                        .child(line.to_owned())
                        .into_any_element()
                })
                .collect();
            let body = surface(&theme)
                .debug_selector(|| "editor-annotate-hint".to_owned())
                .max_w(px(520.))
                .p_2()
                .text_size(px(UI_SIZE))
                .text_color(theme.text)
                .children(rows);
            popups.push(
                deferred(
                    anchored()
                        .position(point(position.x + px(12.), position.y + px(16.)))
                        .snap_to_window_with_margin(px(SPACE_M))
                        .child(body),
                )
                .with_priority(1)
                .into_any_element(),
            );
        }
        let Some(lsp) = &self.lsp else {
            return popups;
        };
        if let Some(doc) = &lsp.quick {
            popups.push(self.render_quick_doc(doc, window, cx));
        }
        if let Some(completion) = &lsp.completion
            && let Some(rows) = completion.window().len().checked_sub(1)
            && let Some((position, anchor)) = self.popup_position(
                completion.start,
                (rows + 1) as f32 * self.style.line_height + 8.,
            )
        {
            let rows =
                completion.window().map(|row| {
                    let parts = completion.label(row);
                    let selected = row == completion.selected;
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .px_2()
                        .when(selected, |row| row.bg(theme.selection))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_color(theme.text)
                                .child(parts.left),
                        )
                        .children(parts.right.map(|right| {
                            div().flex_none().text_color(theme.text_muted).child(right)
                        }))
                        .children(parts.owner.map(|owner| {
                            div()
                                .flex_none()
                                .text_color(theme.text_secondary)
                                .child(owner)
                        }))
                });
            let list = surface(&theme)
                .debug_selector(|| "completion-popup".to_owned())
                .min_w(px(280.))
                .max_w(px(640.))
                .py_1()
                .font_family(self.style.font_family.clone())
                .text_size(px(self.style.font_size))
                .children(rows);
            popups.push(
                deferred(
                    anchored()
                        .anchor(anchor)
                        .position(position)
                        .snap_to_window_with_margin(px(SPACE_M))
                        .child(list),
                )
                .with_priority(1)
                .into_any_element(),
            );
        }
        if let Some(signature) = &lsp.signatures
            && let Some((position, anchor)) =
                self.popup_position(signature.at, 3. * self.style.line_height + 8.)
        {
            let shown = signature.signature();
            let segments = signature_segments(shown, signature.parameter);
            let body = div()
                .flex()
                .flex_wrap()
                .items_center()
                .font_family(self.style.font_family.clone())
                .text_size(px(self.style.font_size));
            let body = segments.into_iter().fold(body, |row, (text, active)| {
                row.child(
                    div()
                        .flex_none()
                        .whitespace_nowrap()
                        .text_color(theme.text)
                        .when(active, |part| {
                            part.font_weight(FontWeight::BOLD)
                                .debug_selector(|| "signature-active-parameter".to_owned())
                        })
                        .child(text),
                )
            });
            let list = surface(&theme)
                .debug_selector(|| "signature-popup".to_owned())
                .min_w(px(280.))
                .max_w(px(720.))
                .p_2()
                .flex()
                .flex_col()
                .gap_1()
                .child(body)
                .children((signature.help.signatures.len() > 1).then(|| {
                    div()
                        .text_color(theme.text_muted)
                        .text_size(px(self.style.font_size))
                        .child(format!(
                            "{} of {}",
                            signature.selected + 1,
                            signature.help.signatures.len()
                        ))
                }));
            popups.push(
                deferred(
                    anchored()
                        .anchor(anchor)
                        .position(position)
                        .snap_to_window_with_margin(px(SPACE_M))
                        .child(list),
                )
                .with_priority(1)
                .into_any_element(),
            );
        }
        if let Some(menu) = &lsp.actions
            && let Some((position, anchor)) = self.popup_position(
                menu.at,
                (menu.window().len() + 1) as f32 * self.style.line_height + 8.,
            )
        {
            let rows = menu.window().map(|row| {
                let selected = row == menu.selected;
                div()
                    .px_2()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(SPACE_L))
                    .when(selected, |row| row.bg(theme.selection))
                    .text_color(theme.text)
                    .child(menu.rows[row].label().to_owned())
                    .children(menu.rows[row].hint().map(|hint| {
                        div()
                            .flex_none()
                            .text_color(theme.text_muted)
                            .child(hint.to_owned())
                    }))
            });
            let list = surface(&theme)
                .debug_selector(|| "actions-popup".to_owned())
                .min_w(px(280.))
                .max_w(px(640.))
                .py_1()
                .text_size(px(self.style.font_size))
                .children(rows);
            popups.push(
                deferred(
                    anchored()
                        .anchor(anchor)
                        .position(position)
                        .snap_to_window_with_margin(px(SPACE_M))
                        .child(list),
                )
                .with_priority(1)
                .into_any_element(),
            );
        }
        if let Some(field) = &lsp.move_to
            && let Some(position) = self.below(field.at)
        {
            let rows =
                field.window().map(|row| {
                    let selected = row == field.selected;
                    let (name, detail) = field.label(row);
                    div()
                        .px_2()
                        .when(selected, |row| row.bg(theme.selection))
                        .text_color(theme.text)
                        .child(name.to_owned())
                        .children(detail.map(|detail| {
                            div().text_color(theme.text_muted).child(detail.to_owned())
                        }))
                });
            let list = surface(&theme)
                .debug_selector(|| "move-popup".to_owned())
                .min_w(px(320.))
                .max_w(px(640.))
                .py_1()
                .text_size(px(self.style.font_size))
                .child(
                    div()
                        .px_2()
                        .text_color(theme.text_muted)
                        .child("Move to package"),
                )
                .child(
                    div()
                        .px_2()
                        .font_family(self.style.font_family.clone())
                        .text_color(theme.text)
                        .child(format!("{}|", field.text)),
                )
                .children(rows);
            popups.push(
                deferred(
                    anchored()
                        .position(position)
                        .snap_to_window_with_margin(px(SPACE_M))
                        .child(list),
                )
                .with_priority(1)
                .into_any_element(),
            );
        }
        if let Some(field) = &lsp.rename
            && let Some(position) = self.below(field.at)
        {
            let body = surface(&theme)
                .debug_selector(|| "rename-popup".to_owned())
                .min_w(px(280.))
                .max_w(px(640.))
                .p_2()
                .flex()
                .flex_col()
                .gap_1()
                .text_size(px(self.style.font_size))
                .child(div().text_color(theme.text_muted).child("Rename to"))
                .child(
                    div()
                        .font_family(self.style.font_family.clone())
                        .text_color(theme.text)
                        .child(format!("{}|", field.text)),
                )
                .children(
                    field
                        .summary
                        .clone()
                        .map(|text| div().text_color(theme.text_muted).child(text)),
                );
            popups.push(
                deferred(
                    anchored()
                        .position(position)
                        .snap_to_window_with_margin(px(SPACE_M))
                        .child(body),
                )
                .with_priority(1)
                .into_any_element(),
            );
        }
        if let Some(popup) = &lsp.overrides
            && let Some((position, anchor)) = self.popup_position(
                popup.at,
                (popup.shown.len() + override_groups(popup) + 2) as f32 * self.style.line_height
                    + 8.,
            )
        {
            let mut rows: Vec<AnyElement> = Vec::new();
            let mut owner: Option<&str> = None;
            for (row, index) in popup.shown.iter().enumerate() {
                let method = &popup.methods[*index];
                if owner != Some(method.owner.as_str()) {
                    owner = Some(method.owner.as_str());
                    rows.push(
                        div()
                            .px_2()
                            .pt_1()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.text_secondary)
                            .child(method.owner.clone())
                            .into_any_element(),
                    );
                }
                let selected = row == popup.selected;
                let checked = popup.checked[*index];
                let index = *index;
                let parameters = method.parameters.join(", ");
                let return_type = method.return_type.clone();
                rows.push(
                    div()
                        .id(("override-row", index))
                        .px_2()
                        .when(selected, |row| row.bg(theme.selection))
                        .text_color(theme.text)
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            checkbox(checked, &theme)
                                .debug_selector(move || format!("override-checkbox-{index}")),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .child(format!("{}({parameters})", method.name)),
                        )
                        .children(return_type.map(|return_type| {
                            div()
                                .debug_selector(move || format!("override-return-type-{index}"))
                                .flex_none()
                                .text_color(theme.text_secondary)
                                .child(return_type)
                        }))
                        .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                            if let Some(popup) =
                                this.lsp.as_mut().and_then(|lsp| lsp.overrides.as_mut())
                            {
                                popup.selected = popup
                                    .shown
                                    .iter()
                                    .position(|shown| *shown == index)
                                    .unwrap_or(popup.selected);
                                let control = event.modifiers().control;
                                if control || event.click_count() >= 2 {
                                    popup.checked[index] = !popup.checked[index];
                                }
                                cx.notify();
                            }
                        }))
                        .into_any_element(),
                );
            }
            let body = surface(&theme)
                .debug_selector(|| "override-popup".to_owned())
                .min_w(px(360.))
                .max_w(px(720.))
                .py_1()
                .font_family(self.style.font_family.clone())
                .text_size(px(self.style.font_size))
                .child(div().px_2().text_color(theme.text).child(format!(
                    "{} {}|",
                    if popup.only_abstract {
                        "Implement"
                    } else {
                        "Override"
                    },
                    popup.text
                )))
                .children(rows);
            popups.push(
                deferred(
                    anchored()
                        .anchor(anchor)
                        .position(position)
                        .snap_to_window_with_margin(px(SPACE_M))
                        .child(body),
                )
                .with_priority(1)
                .into_any_element(),
            );
        }
        if let Some(popup) = &lsp.generation
            && let Some((position, anchor)) = self.popup_position(
                popup.at,
                (popup.shown.len() + 2) as f32 * self.style.line_height + 8.,
            )
        {
            let mut rows: Vec<AnyElement> = Vec::new();
            for (row, index) in popup.shown.iter().enumerate() {
                let choice = &popup.choices[*index];
                let selected = row == popup.selected;
                let checked = popup.checked[*index];
                let index = *index;
                rows.push(
                    div()
                        .id(("generate-row", index))
                        .px_2()
                        .when(selected, |row| row.bg(theme.selection))
                        .text_color(theme.text)
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            checkbox(checked, &theme)
                                .debug_selector(move || format!("generate-checkbox-{index}")),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .child(choice.label.clone()),
                        )
                        .children(choice.detail.clone().map(|detail| {
                            div()
                                .flex_none()
                                .text_color(theme.text_secondary)
                                .child(detail)
                        }))
                        .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                            if let Some(popup) =
                                this.lsp.as_mut().and_then(|lsp| lsp.generation.as_mut())
                            {
                                popup.selected = popup
                                    .shown
                                    .iter()
                                    .position(|shown| *shown == index)
                                    .unwrap_or(popup.selected);
                                let control = event.modifiers().control;
                                if control || event.click_count() >= 2 {
                                    popup.checked[index] = !popup.checked[index];
                                }
                                cx.notify();
                            }
                        }))
                        .into_any_element(),
                );
            }
            let body = surface(&theme)
                .debug_selector(|| "generate-popup".to_owned())
                .min_w(px(360.))
                .max_w(px(720.))
                .py_1()
                .font_family(self.style.font_family.clone())
                .text_size(px(self.style.font_size))
                .child(
                    div()
                        .px_2()
                        .text_color(theme.text)
                        .child(format!("Generate {}|", popup.text)),
                )
                .children(rows);
            popups.push(
                deferred(
                    anchored()
                        .anchor(anchor)
                        .position(position)
                        .snap_to_window_with_margin(px(SPACE_M))
                        .child(body),
                )
                .with_priority(1)
                .into_any_element(),
            );
        }
        if let Some(popup) = &lsp.safe_delete
            && let Some(position) = self.below(popup.at)
        {
            let body = surface(&theme)
                .debug_selector(|| "safe-delete-popup".to_owned())
                .min_w(px(360.))
                .p_2()
                .gap_1()
                .flex()
                .flex_col()
                .font_family(self.style.font_family.clone())
                .text_size(px(self.style.font_size))
                .child(
                    div()
                        .text_color(theme.text)
                        .child(format!("{} usages found", popup.count)),
                )
                .child(div().text_color(theme.text_muted).child(popup.name.clone()))
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(
                            div()
                                .when(popup.selected == 0, |row| row.bg(theme.selection))
                                .px_2()
                                .child("View Usages"),
                        )
                        .child(
                            div()
                                .when(popup.selected == 1, |row| row.bg(theme.selection))
                                .px_2()
                                .child("Delete Anyway"),
                        ),
                );
            popups.push(
                deferred(
                    anchored()
                        .position(position)
                        .snap_to_window_with_margin(px(SPACE_M))
                        .child(body),
                )
                .with_priority(1)
                .into_any_element(),
            );
        }
        if let Some(popup) = &lsp.definition
            && let Some(position) = self.below(popup.at)
        {
            let mut rows: Vec<AnyElement> = Vec::new();
            let mut line_start = 0;
            for line in popup.text.split('\n').take(30) {
                let line_end = line_start + line.len();
                let mut runs: Vec<AnyElement> = Vec::new();
                let mut at = line_start;
                for span in popup.spans.iter().filter(|span| {
                    (span.start as usize) < line_end && (span.end as usize) > line_start
                }) {
                    let start = (span.start as usize).max(line_start);
                    let end = (span.end as usize).min(line_end);
                    if start > at {
                        runs.push(
                            div()
                                .flex_none()
                                .text_color(self.style.code)
                                .child(line[at - line_start..start - line_start].to_owned())
                                .into_any_element(),
                        );
                    }
                    runs.push(
                        div()
                            .flex_none()
                            .text_color(self.style.color_of(span.kind))
                            .child(line[start - line_start..end - line_start].to_owned())
                            .into_any_element(),
                    );
                    at = end;
                }
                if at < line_end {
                    runs.push(
                        div()
                            .flex_none()
                            .text_color(self.style.code)
                            .child(line[at - line_start..].to_owned())
                            .into_any_element(),
                    );
                }
                rows.push(
                    div()
                        .flex()
                        .whitespace_nowrap()
                        .children(runs)
                        .into_any_element(),
                );
                line_start = line_end + 1;
            }
            let body = surface(&theme)
                .debug_selector(|| "quick-definition".to_owned())
                .max_w(px(720.))
                .p_2()
                .font_family(self.style.font_family.clone())
                .text_size(px(self.style.font_size))
                .children(rows);
            popups.push(
                deferred(
                    anchored()
                        .position(position)
                        .snap_to_window_with_margin(px(SPACE_M))
                        .child(body),
                )
                .with_priority(1)
                .into_any_element(),
            );
        }
        if let Some(note) = &lsp.note
            && let Some(position) = self.below(note.at)
        {
            let text: String = note.text.lines().take(30).collect::<Vec<_>>().join(
                "
",
            );
            let body = surface(&theme)
                .debug_selector(|| "lsp-note".to_owned())
                .max_w(px(640.))
                .p_2()
                .text_color(theme.text)
                .text_size(px(self.style.font_size))
                .child(text);
            popups.push(
                deferred(
                    anchored()
                        .position(position)
                        .snap_to_window_with_margin(px(SPACE_M))
                        .child(body),
                )
                .with_priority(1)
                .into_any_element(),
            );
        }
        popups
    }
}

impl EditorView {
    /// The counts of the problems in the top right corner of the text, as IDEA shows
    /// them. A click goes to the next problem, the way F2 does.
    fn render_problem_widget(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.lsp.as_ref()?;
        let summary = self.problem_summary();
        let theme = *cx.global::<Theme>();
        let text = if summary.analyzing {
            "Analyzing…".to_owned()
        } else {
            problems_text(summary.errors, summary.warnings)
        };
        let badge = |id: &'static str, icon: &'static str, count: usize| {
            div()
                .id(id)
                .debug_selector(move || id.to_owned())
                .flex()
                .items_center()
                .gap(px(2.))
                .child(icon_sized(icon, 14., &theme))
                .child(div().text_size(px(11.)).child(count.to_string()))
        };
        let mut row = div().flex().items_center().gap(px(8.));
        if summary.analyzing {
            row = row.child(icon_sized("general/refresh", 14., &theme));
        } else if summary.errors == 0 && summary.warnings == 0 {
            row = row.child(icon_sized("status/success", 14., &theme));
        } else {
            if summary.errors > 0 {
                row = row.child(badge(
                    "editor-problem-errors",
                    "status/error",
                    summary.errors,
                ));
            }
            if summary.warnings > 0 {
                row = row.child(badge(
                    "editor-problem-warnings",
                    "status/warning",
                    summary.warnings,
                ));
            }
        }
        Some(
            div()
                .id("editor-problem-widget")
                .debug_selector(|| "editor-problem-widget".to_owned())
                .absolute()
                .top(px(4.))
                .right(px(crate::scrollbar::BAND + 4.))
                .flex()
                .items_center()
                .h(px(20.))
                .px(px(6.))
                .rounded(px(4.))
                .bg(theme.island_bg)
                .border_1()
                .border_color(theme.popup_border)
                .text_color(theme.text)
                .occlude()
                .cursor_pointer()
                .tooltip(move |_, cx| tooltip(text.clone(), None, cx))
                .tooltip_show_delay(tooltip_show_delay())
                .on_click(cx.listener(|this, _, _, cx| this.next_error(cx)))
                .child(row)
                .into_any_element(),
        )
    }
}

/// `1 error, 2 warnings`.
fn problems_text(errors: usize, warnings: usize) -> String {
    let word = |count: usize, one: &str, many: &str| {
        format!("{count} {}", if count == 1 { one } else { many })
    };
    format!(
        "{}, {}",
        word(errors, "error", "errors"),
        word(warnings, "warning", "warnings")
    )
}

impl Render for EditorView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        for started in self.pending_frames.drain(..) {
            perf::observe("editor.keystroke", started.elapsed());
        }
        self.prepare_preview(cx);
        let head = self.state.as_ref().map(EditorState::head);
        if self
            .context_info
            .as_ref()
            .is_some_and(|info| Some(info.head) != head)
        {
            self.context_info = None;
        }
        let popups = self.popups(window, cx);
        let root = div()
            .id("editor-view")
            .key_context("Editor")
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(self.style.background)
            .text_color(self.style.code)
            .on_action(cx.listener(|this, _: &MoveElementLeft, _, cx| {
                if !this.move_element_action(false, cx) {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &MoveElementRight, _, cx| {
                if !this.move_element_action(true, cx) {
                    cx.propagate();
                }
            }))
            .on_key_down(cx.listener(Self::on_key));
        match &self.failed {
            Some(message) => root.p_4().child(message.clone()),
            None => {
                let bar = self.render_bar(cx);
                let toolbar = self.preview_toolbar(cx);
                let crumbs = self.render_crumbs(cx);
                let widget = self.render_problem_widget(cx);
                let text = div()
                    .id("editor-text")
                    .debug_selector(|| "editor-text".to_owned())
                    .relative()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .w_full()
                    .child(EditorElement::new(cx.entity()))
                    .children(widget);
                let text = self.preview_area(text.into_any_element(), window, cx);
                root.flex()
                    .flex_col()
                    .children(bar)
                    .children(toolbar)
                    .child(text)
                    .children(crumbs)
                    .children(popups)
                    .children(self.template_popup(cx))
            }
        }
    }
}

#[cfg(test)]
mod checks {
    use super::{OutlineItem, OutlineKind, around};

    fn item(name: &str, kind: OutlineKind, start: usize, end: usize) -> OutlineItem {
        OutlineItem {
            name: name.into(),
            signature: None,
            kind,
            depth: 0,
            range: start..end,
            name_range: start..start + name.len(),
        }
    }

    #[test]
    fn the_crumbs_are_the_declarations_around_the_caret_outermost_first() {
        let outline = [
            item("run", OutlineKind::Method, 40, 120),
            item("Main", OutlineKind::Class, 0, 200),
            item("other", OutlineKind::Method, 130, 190),
        ];
        let names = |at| -> Vec<_> {
            around(&outline, at)
                .iter()
                .map(|i| i.name.as_str())
                .collect()
        };
        assert_eq!(names(50), ["Main", "run"]);
        assert_eq!(names(125), ["Main"]);
        assert_eq!(names(135), ["Main", "other"]);
        assert!(names(300).is_empty());
    }

    #[test]
    fn blocks_are_the_declarations_with_a_body() {
        assert!(OutlineKind::Method.is_block());
        assert!(OutlineKind::Class.is_block());
        assert!(!OutlineKind::Field.is_block());
        assert!(!OutlineKind::Constant.is_block());
    }

    use ui_kit::{Anchor, Bounds, point, px, size};

    use super::popup_placement;

    fn editor() -> Bounds<ui_kit::Pixels> {
        Bounds::new(point(px(10.), px(20.)), size(px(600.), px(400.)))
    }

    #[test]
    fn the_list_opens_under_a_line_with_room_below() {
        let (position, anchor) = popup_placement(px(30.), px(120.), px(18.), 100., editor());
        assert_eq!(anchor, Anchor::TopLeft);
        assert_eq!(position, point(px(30.), px(138.)));
    }

    #[test]
    fn the_list_opens_above_a_line_at_the_bottom_and_does_not_cover_it() {
        let line_top = px(360.);
        let (position, anchor) = popup_placement(px(30.), line_top, px(18.), 100., editor());
        assert_eq!(anchor, Anchor::BottomLeft);
        assert_eq!(
            position,
            point(px(30.), line_top - px(2.)),
            "the bottom corner stands over the top of the line with a small gap"
        );
        let bottom = position.y;
        assert!(bottom < line_top, "the list does not cover the caret line");
        assert!(
            bottom - px(100.) >= editor().top(),
            "the list stays inside the editor"
        );
    }

    #[test]
    fn a_list_that_fits_neither_way_stays_inside_the_editor() {
        let small = Bounds::new(point(px(0.), px(0.)), size(px(200.), px(100.)));
        let (position, _) = popup_placement(px(10.), px(40.), px(18.), 120., small);
        assert_eq!(position, point(px(10.), px(4.)), "the top margin holds");
    }
}
