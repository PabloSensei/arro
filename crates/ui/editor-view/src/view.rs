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
use futures_util::future::{Either, select};
use keymap::{Keymap, Keystroke, Lookup, default_keymap, editor as actions};
use lang_api::{
    ActionKind, ActionQuery, DiagnosticEntry, DocumentEdit, FormatQuery, GraphVisibility,
    NavigationKind, RangeFormatQuery, Replies, Severity, SourceSpan, TextSpan, ask,
};
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
use crate::rename::WAITING_FOR_LANGUAGE;
use crate::scrollbar::{Axis, Drag};
use crate::semantic::{
    CompletionMode, Lsp, Reveals, completion_doc_rows, completion_kind_icon, location_label,
    signature_segments,
};
use crate::state::{Conflict, EditorState, LoadError, Saved};
use crate::style::EditorStyle;

const BLINK: Duration = Duration::from_millis(530);
/// The keys whose frame has not measured them yet are kept at most, so a view that is
/// painted as a pane of another one and never renders itself cannot grow with typing.
const PENDING_FRAMES: usize = 64;

/// The columns of the widest row of a text and the rows they were counted on. A frame
/// that shows the same rows reuses the count instead of walking the lines of a big file
/// again; an edit, a fold or a tab width starts a new count.
#[derive(Clone, Copy)]
pub(crate) struct WidestRow {
    version: u64,
    tab_size: usize,
    folds: usize,
    inlays: usize,
    columns: usize,
}

impl WidestRow {
    pub(crate) fn of(state: &EditorState, columns: usize) -> Self {
        Self {
            version: state.version(),
            tab_size: state.map().tab_size,
            folds: state.map().folds.as_slice().len(),
            inlays: state.map().inlays.as_slice().len(),
            columns,
        }
    }

    /// Whether the count still belongs to the rows `state` shows.
    pub(crate) fn matches(&self, state: &EditorState) -> bool {
        self.version == state.version()
            && self.tab_size == state.map().tab_size
            && self.folds == state.map().folds.as_slice().len()
            && self.inlays == state.map().inlays.as_slice().len()
    }

    pub(crate) fn columns(&self) -> usize {
        self.columns
    }
}

/// Names the syntax documents of the views that paint text outside an editor.
static NEXT_SOURCE: AtomicU64 = AtomicU64::new(1);

/// Which language a file is. Set by the application, the editor knows none.
pub struct Languages {
    pub resolve: fn(&Path) -> Option<LanguageConfig>,
    /// Live templates of the file's language.
    pub templates: fn(&Path) -> Option<&'static TemplateLanguage>,
}

impl Global for Languages {}

/// Live templates of a format that a tool adds beside the languages of the application,
/// such as the SQL console. Every builder is asked in turn and the first answer wins.
pub struct ExtraTemplates(pub Vec<fn(&Path) -> Option<&'static TemplateLanguage>>);

impl Global for ExtraTemplates {}

/// Names the code style language of a file when its syntax does not. The application
/// knows the language that the project style files spell differently from the editor.
pub struct CodeStyleLanguages(pub fn(&Path) -> Option<&'static str>);

impl Global for CodeStyleLanguages {}

/// The name a project code style file gives the language of this file: what the
/// application says, else the name of the syntax with the few names the other IDE
/// spells its own way.
pub fn code_style_language(
    path: &Path,
    language: Option<&LanguageConfig>,
    cx: &App,
) -> Option<&'static str> {
    if let Some(names) = cx.try_global::<CodeStyleLanguages>()
        && let Some(name) = (names.0)(path)
    {
        return Some(name);
    }
    language.map(|language| match language.name {
        "Shell" => "Shell Script",
        "Windows Batch" => "Batch",
        name => name,
    })
}

/// Colour spans of a format that has no grammar, from its lines alone. The application
/// names the format and gives the spans; the editor only paints them.
pub struct PlainSyntax {
    pub resolve: fn(&Path, &str) -> Option<Vec<HighlightSpan>>,
    /// Files where Ctrl+Enter asks the application to run what is there rather than
    /// split the line.
    pub runs_on_enter: fn(&Path) -> bool,
}

impl Global for PlainSyntax {}

/// Turns the text of a paste into the text this kind of file takes, before the editor
/// inserts it. The application names the format; the editor knows none.
pub type PasteTextBuilder = dyn Fn(&Path, &str) -> Option<String>;

pub struct PasteText(pub Rc<PasteTextBuilder>);

impl Global for PasteText {}

/// Files where Ctrl+Enter asks a tool window to run what is there. A format that already
/// owns Ctrl+Enter keeps it: the predicates are independent and either may take the key.
pub struct RunFiles(pub std::sync::Arc<dyn Fn(&Path) -> bool + Send + Sync>);

impl Global for RunFiles {}

/// The builder of the row the application puts above the text of a kind of file.
pub type FileBarBuilder =
    dyn Fn(&Path, &mut Window, &mut Context<EditorView>) -> Option<AnyElement>;

/// A row the application puts above the text of a kind of file, for instance the
/// environment selector of a request file.
pub struct FileBar(pub Rc<FileBarBuilder>);

impl Global for FileBar {}

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
        /// Ctrl+F2 in a file whose format runs something: stop what it started.
        StopRunning,
    ]
);

struct Keys(Rc<Keymap>);

/// Puts a keymap in place for every editor, including the open ones: a keymap file of
/// the user changes what the editor runs at once.
pub fn install_keymap(cx: &mut App, keymap: Rc<Keymap>) {
    cx.set_global(Keys(keymap));
    // A file whose format runs something keeps Ctrl+F2 for stopping it: the deeper
    // context wins over the Stop of the window, which belongs to the debugger.
    cx.bind_keys([KeyBinding::new(
        "ctrl-f2",
        StopRunning,
        Some("RunnableFile"),
    )]);
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

/// Writes the history of what was copied, so that it survives a restart. The window
/// sets it at start; it runs off the UI thread.
type ClipSaver = std::sync::Arc<dyn Fn(Vec<String>)>;

struct ClipHistorySink(Option<ClipSaver>);

impl Global for ClipHistorySink {}

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

/// Puts the history of an earlier run in place, the newest copy first.
pub fn set_clipboard_history(cx: &mut App, entries: Vec<String>) {
    let mut history: Vec<String> = Vec::new();
    for entry in entries {
        if !entry.is_empty() && !history.contains(&entry) {
            history.push(entry);
        }
        if history.len() == CLIP_HISTORY {
            break;
        }
    }
    cx.default_global::<ClipHistory>().0 = history;
}

/// The window saves every copy with this, off the UI thread.
pub fn install_clipboard_history_sink(cx: &mut App, sink: impl Fn(Vec<String>) + 'static) {
    cx.set_global(ClipHistorySink(Some(std::sync::Arc::new(sink))));
}

fn remember_clip(cx: &mut App, text: String) {
    if text.is_empty() {
        return;
    }
    let history = &mut cx.default_global::<ClipHistory>().0;
    history.retain(|kept| *kept != text);
    history.insert(0, text);
    history.truncate(CLIP_HISTORY);
    let entries = history.clone();
    if let Some(sink) = cx
        .try_global::<ClipHistorySink>()
        .and_then(|sink| sink.0.clone())
    {
        sink(entries);
    }
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
    /// How visible the declaration is, when the language says so.
    pub visibility: Option<GraphVisibility>,
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

/// A variable of the stopped frame as the editor draws it after the lines that name it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DebugValue {
    pub name: String,
    pub value: String,
    /// The value differs from the one of the previous stop.
    pub changed: bool,
}

/// The values of one buffer line, shown after its text while the program is stopped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InlineValue {
    /// The buffer line, from zero.
    pub line: usize,
    pub text: String,
    /// One of the values changed since the previous stop.
    pub changed: bool,
}

/// An inline watch of the window: an expression of the stopped program and its last
/// value, shown after the line it was set on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InlineWatchRow {
    /// The buffer line, from zero.
    pub line: usize,
    pub expression: String,
    pub text: String,
}

/// The most characters of one value the line shows; the rest is cut with an ellipsis.
const INLINE_VALUE_LIMIT: usize = 60;

/// `value` cut to `limit` characters, with an ellipsis when something was left out.
fn cut_value(value: &str, limit: usize) -> String {
    let mut characters = value.chars();
    let mut cut: String = characters.by_ref().take(limit).collect();
    if characters.next().is_some() {
        cut.push('…');
    }
    cut
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

/// Every bookmark of the window, files in path order.
pub fn bookmarks(cx: &App) -> Vec<crate::bookmarks::BookmarkEntry> {
    crate::bookmarks::bookmark_entries(cx)
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

/// A button in the gutter beside the run button: the line starts something that is not
/// a program, such as an HTTP handler. The colour is the caller's.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EndpointMark {
    /// Buffer line from zero.
    pub line: u32,
    pub color: Hsla,
}

/// A place a badge of the gutter goes to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaceTarget {
    /// The text of the place in the list of a badge.
    pub label: String,
    pub file: PathBuf,
    pub offset: usize,
}

/// A badge of the gutter that opens a menu of actions rather than a list of places: the
/// caller decides what those actions are.
#[derive(Clone, Debug, PartialEq)]
pub struct ActionMark {
    /// Buffer line from zero.
    pub line: u32,
    /// The icon of the badge, from the icon set of the editor.
    pub icon: SharedString,
    pub color: Hsla,
    /// The hint of the badge under the pointer.
    pub hint: SharedString,
}

/// A badge in the gutter beside the line number: a click with one place goes to it, a
/// click with several opens their list. The colour and the icon are the caller's.
#[derive(Clone, Debug, PartialEq)]
pub struct PlaceMark {
    /// Buffer line from zero.
    pub line: u32,
    /// The icon of the badge, from the icon set of the editor.
    pub icon: SharedString,
    pub color: Hsla,
    /// The hint of the badge under the pointer.
    pub hint: SharedString,
    /// The badge opens its list even when it holds one place, as the usages of a bean do.
    pub list: bool,
    pub places: Vec<PlaceTarget>,
}

/// How a line looks after a run with coverage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoverageState {
    /// The line ran.
    Covered,
    /// The line ran, but a branch of it was not taken.
    Partial,
    /// The line did not run.
    NotCovered,
}

/// The coverage stripe of one line of the gutter. The colour is the caller's, the view
/// knows nothing about how the numbers were collected.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CoverageMark {
    /// Buffer line from zero.
    pub line: u32,
    pub state: CoverageState,
    /// The instructions of the line that ran.
    pub hits: u32,
    /// The branches of the line, `covered` and `total`.
    pub branches: Option<(u32, u32)>,
    pub color: Hsla,
}

impl CoverageMark {
    /// The hint of the mark under the pointer.
    pub fn hint(&self) -> String {
        match self.branches {
            Some((covered, total)) => {
                format!("Hits: {}\nCovered {covered} of {total} branches", self.hits)
            }
            None => format!("Hits: {}", self.hits),
        }
    }
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

/// A diagnostic of the editor that the user sends to the agent from Alt+Enter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AiProblem {
    pub path: PathBuf,
    /// The first line of the diagnostic, from zero.
    pub line: usize,
    pub severity: lang_api::Severity,
    pub message: String,
    pub code: Option<String>,
    /// The text of the file with the unsaved edits of the user.
    pub text: String,
}

/// The number of the next usages search of the process. A late answer of an older search
/// does not replace the list of a newer one.
pub fn next_usages_search() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1
}

#[derive(Clone, Debug)]
pub enum EditorEvent {
    Loaded,
    /// Alt+Enter picked Fix with AI on a diagnostic: the window sends the problem to
    /// the agent.
    FixWithAi(AiProblem),
    Dirty(bool),
    Saved,
    Failed(SharedString),
    /// A message about a language answer that the status bar shows, such as a name the
    /// language refuses to rename.
    Status(SharedString),
    /// A save met a language server that could not run its actions; the text went to
    /// the disk without them.
    SaveNote(SharedString),
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
    /// The label names the refactoring in Local History, when it has one.
    ApplyChange {
        change: lang_api::WorkspaceChange,
        label: Option<String>,
    },
    /// Ctrl+Z in the file a refactoring of several files started in: the window takes
    /// the whole change back, closed files of it included.
    UndoRefactoring(u64),
    /// Ctrl+Shift+Z after such an undo: the window applies the whole change again.
    RedoRefactoring(u64),
    /// Ctrl+F6: the window shows the Change Signature form for this place.
    ChangeSignature(lang_api::DocumentSpot),
    /// Pull Members Up and the like: the window shows their form for this place.
    MemberRefactoring {
        refactoring: lang_api::MemberRefactoring,
        spot: lang_api::DocumentSpot,
    },
    /// F6 on a member: the window shows the form that moves it into another type.
    MoveMembers(lang_api::DocumentSpot),
    /// F6 on an instance method: the window shows the form that moves it to one of the
    /// places around it.
    MoveTargets(lang_api::DocumentSpot),
    /// Refactor This picked an action of the server that needs values: the window shows
    /// its form, and the handle names the action for the language.
    RefactorForm(String),
    /// Refactor This picked Copy: the window copies the current file, as F5 does.
    CopyElement,
    /// Ctrl+Q repeated or the pin button: the window shows the Documentation tool window.
    ShowDocumentation,
    /// Ctrl+Alt+H: the window shows the call hierarchy of this place.
    CallHierarchy(lang_api::DocumentSpot),
    /// Ctrl+H: the window shows the type hierarchy of this place.
    TypeHierarchy(lang_api::DocumentSpot),
    /// Ctrl+Shift+H: the window shows the hierarchy of the method at this place.
    MethodHierarchy(lang_api::DocumentSpot),
    /// Ctrl+Alt+Shift+U: the window shows the diagram of the type at this place.
    ShowDiagram(lang_api::DocumentSpot),
    /// The window shows the sequence diagram of the method at this place.
    ShowSequenceDiagram(lang_api::DocumentSpot),
    /// Safe Delete of a type alone in its file: the window removes the file too.
    DeleteFile(PathBuf),
    /// A generation created files: the window writes them and opens the first one.
    CreateFiles(Vec<lang_api::GeneratedFile>),
    /// Alt+Enter picked Create Test: the window shows its form for this place.
    CreateTest(lang_api::DocumentSpot),
    /// Ctrl+Alt+L: the window formats with the flags of the Reformat File dialog.
    Reformat,
    /// Ctrl+Alt+O is about to apply the edits of the language: the window records the
    /// file in Local History before they reach the buffer.
    BeforeOptimizeImports,
    /// The text changed, by typing or from outside.
    Changed,
    /// The caret or the selection moved without changing the text.
    Moved,
    /// A run mark was clicked: the line it stands on, from zero.
    RunMark(u32),
    /// The mark beside the run button was clicked: the line it stands on, from zero.
    EndpointMark(u32),
    /// A badge of the gutter was clicked: the line it stands on, from zero.
    PlaceMark(u32),
    /// A badge of the gutter that opens a menu of actions was clicked: the line it
    /// stands on, from zero.
    ActionMark(u32),
    /// Ctrl+Enter in a file whose format runs something, such as a request file: the
    /// line of the caret, from zero.
    RunRequest(u32),
    /// Ctrl+F2 in a file whose format runs something: stop what it started.
    StopRequest,
    /// Alt+click on a run mark: run the same test under the debugger.
    DebugRunMark(u32),
    /// The gutter was clicked away from the fold column: the line, from zero, and
    /// whether Shift was held, which puts a breakpoint that does not stop the program.
    GutterClick {
        line: u32,
        shift: bool,
    },
    /// A change mark beside the line numbers was clicked: the line of the mark, from
    /// zero. The window shows what the line was before the change.
    ChangeMarkClick(u32),
    /// The pointer came to a change mark of the gutter, or left the marks: the line of
    /// the mark, from zero, and `None` when no mark is under the pointer.
    ChangeMarkHover(Option<u32>),
    /// A mark of an agent's change was clicked: the line of the mark, from zero. The
    /// window shows the old text of the hunk and takes it in or puts it back.
    AgentMarkClick(u32),
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
    /// Alt+F7 or Ctrl+F7 started a search: the Usages window opens at once with the name
    /// of the symbol, and the answer comes later under the same number.
    UsagesSearchStarted {
        /// The number of the run, which its answer carries.
        search: u64,
        /// The name of the symbol, the label of the tab while the search runs.
        name: String,
        /// Whether the search looks in this file only.
        in_file: bool,
    },
    /// Alt+F7: the uses of the symbol, for the window that lists them by file.
    Usages {
        search: u64,
        usages: Vec<Usage>,
    },
    /// Alt+F7 with the language that tells the kind and the container of a use.
    UsagesDetailed {
        search: u64,
        usages: Vec<DetailedUsage>,
    },
    /// Ctrl+F7: the uses of the symbol in this file, for the window that lists them.
    UsagesInFile {
        search: u64,
        usages: Vec<Usage>,
    },
    /// Ctrl+F7 with the language that tells the kind and the container of a use.
    UsagesDetailedInFile {
        search: u64,
        usages: Vec<DetailedUsage>,
    },
    /// Ctrl+Alt+F7: the uses of the symbol, for the popup at the caret.
    ShowUsages(UsagesUpdate),
    /// Alt+F7 or Ctrl+F7 while the language server imports: the Usages window opens with a
    /// waiting line. The search runs by itself when the server is ready; `cancel` ends the wait.
    UsagesWaiting {
        /// The number of the run, which its answer carries.
        search: u64,
        name: String,
        in_file: bool,
        cancel: ::base::Cancel,
    },
    /// A right click in the text: the window shows the menu of the editor here.
    ContextMenu(Point<Pixels>),
    /// A right click in the gutter of the line numbers: the window shows the menu of the
    /// gutter for this line, counted from zero.
    GutterMenu {
        position: Point<Pixels>,
        line: u32,
    },
    /// A right click in the blame column: the window shows the menu of the column for
    /// this line, counted from zero.
    BlameMenu {
        position: Point<Pixels>,
        line: u32,
    },
    /// Alt+Enter chose to turn a check off in the settings of the project.
    DisableInspection {
        id: String,
        title: String,
    },
    /// Alt+Enter saved a misspelled word to the project dictionary or to the one of the
    /// user. The window writes the file and asks the check again.
    SaveSpelling {
        word: String,
        project: bool,
    },
    /// Alt+Enter chose to open the settings of a check.
    EditInspection(String),
    /// Alt+Enter picked Add Inline Watch: the line, from zero, and the expression.
    AddInlineWatch {
        line: usize,
        expression: String,
    },
    /// The cross of an inline watch was clicked: its expression and line, from zero.
    InlineWatchRemoved {
        line: usize,
        expression: String,
    },
    /// The text scrolled: the first visible row in pixels, for a view that follows it.
    Scrolled(f32),
    /// An action found nothing to do and says so in the status bar of the window.
    StatusMessage(SharedString),
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

/// What the last frame showed of the caret: its head, the scroll and whether the whole row
/// was on screen. A row that stops showing while none of these changed is revealed again.
#[derive(Clone, Copy, Debug)]
pub(crate) struct CaretWatch {
    pub head: usize,
    pub scroll_top: f32,
    pub shown: bool,
}

/// The list of the neighbours of one declaration, which a click on its breadcrumb opens:
/// the other members of the same class, or the other declarations of the file.
pub(crate) struct CrumbMenu {
    /// The crumb the list belongs to: 0 is the outermost declaration of the file.
    level: usize,
    /// Where the list stands, under the crumb that opened it.
    position: Point<Pixels>,
    /// The text that narrows the list while the user types.
    filter: TextField,
    /// The row the keys act on, counted in the filtered list.
    selected: usize,
    scroll: ScrollHandle,
}

/// The icon of a declaration in the list of a crumb.
fn crumb_icon(kind: OutlineKind) -> &'static str {
    match kind {
        OutlineKind::Module => "nodes/package",
        OutlineKind::Class => "nodes/class",
        OutlineKind::Interface => "nodes/interface",
        OutlineKind::Enum => "nodes/enum",
        OutlineKind::Function | OutlineKind::Method | OutlineKind::Constructor => "nodes/method",
        OutlineKind::Field | OutlineKind::Other => "nodes/field",
        OutlineKind::Constant => "nodes/constant",
    }
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

/// A press in the gutter that waits for the release: a click places a breakpoint, a
/// drag selects whole lines.
struct GutterPress {
    /// The line of the press, from zero.
    line: u32,
    /// Shift was held, which puts a breakpoint that does not stop the program.
    shift: bool,
    /// Where the press began, so a drag is told from a click.
    start: Point<Pixels>,
    /// The pointer moved far enough to select lines rather than place a breakpoint.
    dragging: bool,
}

/// A drag that began with a multiple click: the selection grows by words or by lines
/// from the place of the press, as IDEA selects them.
#[derive(Clone, Copy)]
enum DragUnit {
    /// The offset of the press; a drag takes the whole words around both ends.
    Words(usize),
    /// The offset of the press; a drag takes the whole lines around both ends.
    Lines(usize),
}

/// The unit of a drag for a press that is the second or the third click of a series.
fn drag_of(click_count: usize, offset: usize) -> Option<DragUnit> {
    match click_count {
        2 => Some(DragUnit::Words(offset)),
        3.. => Some(DragUnit::Lines(offset)),
        _ => None,
    }
}

/// The rows of an open Alt+Enter list and the preview of the selected one, for a test.
#[cfg(any(test, feature = "test-support"))]
pub struct ActionRows {
    /// The section of the row, true for a quick fix, and its label.
    pub rows: Vec<(bool, String)>,
    pub preview: Option<Vec<String>>,
}

/// The behaviour of an open editor that comes from Settings. The colours and the size
/// stay in [`EditorStyle`]; this is what a settings change flips without a new file.
#[derive(Clone, Debug, PartialEq)]
pub struct EditorOptions {
    /// Numbers in the gutter.
    pub line_numbers: bool,
    /// The icons of the gutter: breakpoints, run marks, bookmarks and the fold arrows.
    pub gutter_icons: bool,
    /// The background of the caret line.
    pub highlight_current_line: bool,
    /// The vertical lines at the steps of the indentation.
    pub indent_guides: bool,
    /// The declarations around the caret under the text.
    pub breadcrumbs: bool,
    /// The declarations that the top of the editor holds while the caret is in their body.
    pub sticky_lines: bool,
    /// The text is centred, with the gutter hidden: the mode of Distraction Free.
    pub centered_text: bool,
    /// The column of the right margin, and the width of the centred text.
    pub right_margin: usize,
    /// The caret blinks while the editor has the focus.
    pub caret_blink: bool,
    /// Ctrl+wheel changes the font size rather than scrolling.
    pub wheel_zoom: bool,
    /// What a save does with the spaces and tabs at the end of a line.
    pub save: crate::state::SaveOptions,
    /// The work of the language a save does before the text reaches the disk.
    pub actions_on_save: crate::state::ActionsOnSave,
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
    /// The preview of a markdown file takes the pictures of an address of the internet.
    pub markdown_images_from_internet: bool,
    /// Alt+Enter offers Fix with AI on the diagnostic under the caret.
    pub ai_features: bool,
    /// Long lines break into the rows of the width, as View | Soft-Wrap sets for every
    /// editor.
    pub soft_wrap: bool,
    /// The spaces and tabs of the text show as marks, as View | Show Whitespaces sets
    /// for every editor.
    pub show_whitespace: bool,
}

impl Default for EditorOptions {
    fn default() -> Self {
        Self {
            line_numbers: true,
            gutter_icons: true,
            highlight_current_line: true,
            indent_guides: true,
            breadcrumbs: true,
            sticky_lines: true,
            centered_text: false,
            right_margin: 120,
            caret_blink: true,
            wheel_zoom: true,
            save: crate::state::SaveOptions::default(),
            actions_on_save: crate::state::ActionsOnSave::default(),
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
            markdown_images_from_internet: true,
            ai_features: true,
            soft_wrap: false,
            show_whitespace: false,
        }
    }
}

pub struct EditorView {
    pub(crate) path: PathBuf,
    /// The root of the project the file belongs to; `None` for a file opened alone.
    project_root: Option<PathBuf>,
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
    soft_wrap: bool,
    /// View | Active Editor | Soft-Wrap for this view alone; `None` follows Settings.
    soft_wrap_override: Option<bool>,
    /// A pane of a comparison keeps its rows on the lines of the other pane, so soft
    /// wrap stays off there.
    wrap_locked: bool,
    show_whitespace: bool,
    /// View | Active Editor | Show Whitespaces for this view alone; `None` follows Settings.
    show_whitespace_override: Option<bool>,
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
    /// A press in the gutter that waits for the release.
    gutter_press: Option<GutterPress>,
    /// The first key of a chord that waits for the next one.
    chords: Vec<Keystroke>,
    /// Alt+Q: the header of the enclosing declaration, shown while the caret stays.
    context_info: Option<ContextInfo>,
    /// Ctrl+Alt+.: the regions of the file and the filter over them.
    pub(crate) region_popup: Option<crate::regions::RegionPopup>,
    /// The list of the neighbours of a crumb, under the crumb that opened it.
    pub(crate) crumb_menu: Option<CrumbMenu>,
    pub(crate) focus_handle: FocusHandle,
    pub(crate) scroll_top: f32,
    pub(crate) scroll_left: f32,
    /// The room that centres the text of Distraction Free Mode, in pixels.
    pub(crate) center_inset: f32,
    /// The width of the gutter of the last frame: it follows the digits of the numbers.
    pub(crate) gutter_width: f32,
    /// The horizontal bar was painted in the last frame.
    pub(crate) horizontal_bar: bool,
    /// The widest row of the text and the rows it was counted on, so that a repaint
    /// without a text change, a blink of the caret for one, does not walk every line.
    pub(crate) widest: Option<WidestRow>,
    /// The pointer is over the bar: it takes the wide look.
    pub(crate) scrollbar_hovered: bool,
    /// The thumb the pointer holds.
    pub(crate) scrollbar_drag: Option<crate::scrollbar::Drag>,
    /// The hint of the mark under the pointer: its text and where it stands.
    stripe_hint: Option<(String, Point<Pixels>)>,
    /// The hint of the blame label under the pointer, and where it stands.
    annotate_hint: Option<(String, Point<Pixels>)>,
    /// The commit whose labels the pointer rests on: every line of it takes the stronger
    /// background.
    pub(crate) annotate_hovered: Option<String>,
    /// The width of the blame column of the last frame, and the longest label it was
    /// measured for, so that the column follows the labels of the view.
    pub(crate) annotation_room: f32,
    pub(crate) annotation_chars: usize,
    /// The line whose bookmark the next typed key names, when the command came from a
    /// menu rather than from the caret.
    awaiting_mnemonic_line: Option<usize>,
    /// The hint of the coverage stripe under the pointer, and where it stands.
    coverage_hint: Option<(String, Point<Pixels>)>,
    /// The hint of a badge of the gutter under the pointer, and where it stands.
    place_hint: Option<(String, Point<Pixels>)>,
    /// The line of the change mark under the pointer, from zero, so that the window
    /// hears about coming to a mark and leaving it once.
    change_mark_hovered: Option<u32>,
    pub(crate) autoscroll: bool,
    /// The next autoscroll puts the caret near the middle, as a jump to a place does.
    pub(crate) reveal_center: bool,
    pub(crate) caret_on: bool,
    pub(crate) viewport: Size<Pixels>,
    /// What the last frame showed of the caret, see [`CaretWatch`].
    pub(crate) caret_watch: Option<CaretWatch>,
    pub(crate) layout: Option<Rc<Layout>>,
    pub(crate) gutter_hovered: bool,
    pub(crate) run_marks: Vec<RunMark>,
    pub(crate) endpoint_marks: Vec<EndpointMark>,
    pub(crate) place_marks: Vec<PlaceMark>,
    /// The badges that open a menu of actions of their own, such as Apply for a YAML
    /// document.
    pub(crate) action_marks: Vec<ActionMark>,
    pub(crate) coverage: Vec<CoverageMark>,
    /// Buffer lines of the breakpoints, from zero.
    pub(crate) breakpoints: Vec<u32>,
    /// The breakpoints that do not stop the program, painted as a muted point.
    pub(crate) muted_breakpoints: Vec<u32>,
    /// The breakpoints the window muted: the gutter paints them grey and the program
    /// does not stop at them.
    pub(crate) disabled_breakpoints: Vec<u32>,
    /// The breakpoints that go away at the first stop, painted as an outline.
    pub(crate) temporary_breakpoints: Vec<u32>,
    /// The breakpoints an agent set, painted in the colour of the accent.
    pub(crate) agent_breakpoints: Vec<u32>,
    /// Buffer lines of the method breakpoints, painted as a diamond.
    pub(crate) function_breakpoints: Vec<u32>,
    /// Buffer lines of the field watchpoints, painted as an eye.
    pub(crate) field_watchpoints: Vec<u32>,
    pub(crate) gutter_marks: Vec<GutterMark>,
    /// The hunks an agent changed and the user has not accepted yet. The caller paints
    /// them with a colour of its own and opens its own window for them.
    pub(crate) agent_marks: Vec<GutterMark>,
    selecting: bool,
    /// How a drag that began with a double or a triple click grows the selection.
    drag: Option<DragUnit>,
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
    /// The open completion question; a new key cancels the old at the server.
    pub(crate) completion_task: Option<Task<()>>,
    /// The open question for the documentation of the selected completion entry.
    pub(crate) completion_doc_task: Option<Task<()>>,
    /// The message under the caret that closes by itself.
    pub(crate) note_task: Option<Task<()>>,
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
    /// The labels of the visible rows waiting for their counts; a new question or a
    /// keystroke stops it.
    pub(crate) code_lens_resolve_task: Option<Task<()>>,
    /// The documentation comments of the language and the choices made for each.
    pub(crate) docs: crate::rendered_doc::RenderedDocs,
    /// The preview of a markdown file beside its text, `None` for other files.
    pub(crate) preview: Option<crate::markdown_preview::MarkdownPreview>,
    /// The folder the preview keeps the pictures it takes from the internet.
    pub(crate) picture_cache: Option<PathBuf>,
    /// Keys waiting for the next frame, so the latency of a key includes the paint.
    pending_frames: Vec<Instant>,
    /// The values of the stopped frame, by buffer line, drawn after the line.
    pub(crate) debug_values: Vec<InlineValue>,
    /// The buffer line the stopped program is on, from zero: its row is tinted and the
    /// gutter draws the arrow of it.
    pub(crate) execution_line: Option<usize>,
    /// The values of the stopped frame that came while the text of the file was still being
    /// read, with their line: the text names their lines once it is there.
    debug_source: Option<(usize, Vec<DebugValue>)>,
    /// The inline watches of the stopped program, by buffer line.
    pub(crate) inline_watches: Vec<InlineWatchRow>,
    /// The program is stopped in this file, so a caret expression can become an inline
    /// watch.
    pub(crate) inline_watch_ready: bool,
    /// The words a spelling check found, by the bytes of the text they cover.
    pub(crate) spelling_marks: Vec<crate::spelling::SpellingMark>,
    /// The buffer version the marks were checked at.
    pub(crate) spelling_version: Option<i32>,
    /// The buffer version whose check is on its way.
    pub(crate) spelling_asked: Option<i32>,
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

/// How long a save waits for the language before it writes without the actions.
const SAVE_ACTIONS_TIMEOUT: Duration = Duration::from_secs(2);

fn whole_span(store: &RopeStore) -> TextSpan {
    TextSpan {
        start: lang_api::TextPoint::default(),
        end: crate::semantic::point_at(store, store.len()),
    }
}

/// The span of whole lines, the shape a diff hunk names.
fn lines_span(store: &RopeStore, lines: std::ops::Range<u32>) -> TextSpan {
    let at = |line: u32| {
        if line as usize >= store.line_count() {
            store.len()
        } else {
            store.line_to_offset(line as usize)
        }
    };
    TextSpan {
        start: crate::semantic::point_at(store, at(lines.start)),
        end: crate::semantic::point_at(store, at(lines.end)),
    }
}

/// Whether the text carries the markers of a merge conflict, which a formatter must not
/// touch.
fn has_conflict_markers(text: &str) -> bool {
    let (mut start, mut middle) = (false, false);
    for line in text.lines() {
        if line.starts_with("<<<<<<< ") {
            start = true;
            middle = false;
        } else if start && line == "=======" {
            middle = true;
        } else if middle && line.starts_with(">>>>>>> ") {
            return true;
        }
    }
    false
}

const SAVE_SERVER_SLOW: &str =
    "The language server did not answer in time; the file was saved without the actions on save";
const SAVE_SERVER_MISSING: &str =
    "The language server is not ready; the file was saved without the actions on save";

/// The questions a save puts to the language before it writes.
struct SaveActionRequests {
    /// The text version the answers belong to.
    version: u64,
    /// Whole-file or per-range formatting answers.
    format: Vec<Replies<Vec<DocumentEdit>>>,
    /// Organize Imports and Code Cleanup answers.
    actions: Vec<Replies<Vec<lang_api::ActionEntry>>>,
    /// The lines the format may touch; empty for the whole file.
    ranges: Vec<TextSpan>,
    /// A provider the settings wanted but the language does not offer.
    missing: bool,
}

/// What the language answered for a save.
struct SaveActionOutcome {
    version: u64,
    /// `None` when the answer did not come in time.
    edits: Option<Vec<DocumentEdit>>,
    ranges: Vec<TextSpan>,
    note: Option<&'static str>,
}

/// Awaits every answer of a save under one deadline, so a slow provider costs the wait
/// of the whole group rather than its own.
async fn collect_save_actions(
    requests: SaveActionRequests,
    deadline: impl std::future::Future<Output = ()>,
) -> SaveActionOutcome {
    let SaveActionRequests {
        version,
        format,
        actions,
        ranges,
        missing,
    } = requests;
    let answers = async move {
        let mut edits = Vec::new();
        for mut replies in format {
            if let Some(answer) = replies.next().await {
                edits.extend(answer.value);
            }
        }
        for mut replies in actions {
            if let Some(answer) = replies.next().await
                && let Some(entry) = answer.value.into_iter().next()
            {
                for source in entry.edits {
                    edits.extend(source.edits);
                }
            }
        }
        edits
    };
    let answer = select(Box::pin(answers), Box::pin(deadline)).await;
    match answer {
        Either::Left((edits, _)) => SaveActionOutcome {
            version,
            edits: Some(edits),
            ranges,
            note: missing.then_some(SAVE_SERVER_MISSING),
        },
        Either::Right(_) => SaveActionOutcome {
            version,
            edits: None,
            ranges,
            note: Some(SAVE_SERVER_SLOW),
        },
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
                let style_language = code_style_language(&load, language.as_ref(), cx);
                let task = cx.background_spawn(async move {
                    EditorState::load(&load, language, style_language)
                });
                cx.spawn_in(window, async move |this, cx| {
                    let result = task.await;
                    this.update(cx, |this, cx| this.loaded(result, cx)).ok();
                })
                .detach();
            }
        }
        cx.observe_global::<Reveals>(|this, cx| this.reveal(cx))
            .detach();
        // The editor lost the focus: a name changed in place is applied, as IDEA applies
        // it when the user clicks away.
        let focus = view.focus_handle.clone();
        cx.on_blur(&focus, window, |view, _window, cx| {
            if view
                .lsp
                .as_ref()
                .is_some_and(|lsp| lsp.rename_in_place.is_some())
            {
                view.leave_rename_in_place(cx);
            }
        })
        .detach();
        // The window lost the focus of the system: its popups go with it, as IDEA
        // closes them.
        cx.observe_window_activation(window, |view, window, cx| {
            if !window.is_window_active() {
                view.dismiss_popups(cx);
            }
        })
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
        if shows_bookmarks {
            let saved = crate::bookmarks::bookmark_lines(&view.path, cx);
            if let Some(state) = &mut view.state {
                state.set_bookmarks(&saved);
            }
        }
        view.refresh_plain_spans(cx);
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
                    lsp.server_marks.clear();
                    lsp.native_marks.clear();
                    lsp.server_tokens.clear();
                    lsp.native_tokens.clear();
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
                self.debug_values.clear();
                // The bookmarks of the file belong to the window, not to this view: read
                // them again instead of syncing an empty list over the stored ones.
                let saved = crate::bookmarks::bookmark_lines(&self.path, cx);
                if let Some(state) = &mut self.state {
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
            .try_global::<ExtraTemplates>()
            .and_then(|extra| extra.0.iter().find_map(|templates| templates(&path)))
            .or_else(|| {
                cx.try_global::<Languages>()
                    .and_then(|languages| (languages.templates)(&path))
            });
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
            project_root: None,
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
            soft_wrap: false,
            soft_wrap_override: None,
            wrap_locked: false,
            show_whitespace: false,
            show_whitespace_override: None,
            outline: Vec::new(),
            line_declarations: Vec::new(),
            outline_serial: 0,
            outline_generation: 0,
            syntax_generation: 0,
            awaiting_mnemonic: false,
            sticky: Vec::new(),
            block_drag: None,
            gutter_press: None,
            chords: Vec::new(),
            context_info: None,
            region_popup: None,
            crumb_menu: None,
            focus_handle: cx.focus_handle(),
            scroll_top: 0.,
            scroll_left: 0.,
            center_inset: 0.,
            gutter_width: GUTTER_WIDTH,
            horizontal_bar: false,
            widest: None,
            scrollbar_hovered: false,
            scrollbar_drag: None,
            stripe_hint: None,
            annotate_hint: None,
            annotate_hovered: None,
            annotation_room: ANNOTATE_WIDTH,
            annotation_chars: 0,
            awaiting_mnemonic_line: None,
            coverage_hint: None,
            place_hint: None,
            change_mark_hovered: None,
            autoscroll: false,
            reveal_center: false,
            caret_on: true,
            viewport: Size::default(),
            caret_watch: None,
            layout: None,
            gutter_hovered: false,
            run_marks: Vec::new(),
            endpoint_marks: Vec::new(),
            place_marks: Vec::new(),
            action_marks: Vec::new(),
            coverage: Vec::new(),
            breakpoints: Vec::new(),
            muted_breakpoints: Vec::new(),
            disabled_breakpoints: Vec::new(),
            temporary_breakpoints: Vec::new(),
            agent_breakpoints: Vec::new(),
            function_breakpoints: Vec::new(),
            field_watchpoints: Vec::new(),
            gutter_marks: Vec::new(),
            agent_marks: Vec::new(),
            selecting: false,
            drag: None,
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
            completion_task: None,
            completion_doc_task: None,
            note_task: None,
            code_lens: Vec::new(),
            code_vision_labels: Vec::new(),
            code_vision_hovered: None,
            code_lens_asked: 0,
            code_lens_resolved: Vec::new(),
            code_lens_task: None,
            code_lens_resolve_task: None,
            docs: crate::rendered_doc::RenderedDocs::default(),
            preview: None,
            picture_cache: None,
            pending_frames: Vec::with_capacity(8),
            debug_values: Vec::new(),
            execution_line: None,
            debug_source: None,
            inline_watches: Vec::new(),
            inline_watch_ready: false,
            spelling_marks: Vec::new(),
            spelling_version: None,
            spelling_asked: None,
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
        let bar = self.render_bar(window, cx).map(|bar| {
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

    /// Replaces the marks beside the run button in the gutter.
    pub fn set_endpoint_marks(&mut self, marks: Vec<EndpointMark>, cx: &mut Context<Self>) {
        if self.endpoint_marks != marks {
            self.endpoint_marks = marks;
            cx.notify();
        }
    }

    pub fn endpoint_marks(&self) -> &[EndpointMark] {
        &self.endpoint_marks
    }

    /// Replaces the badges of the gutter that open a list of places.
    pub fn set_place_marks(&mut self, marks: Vec<PlaceMark>, cx: &mut Context<Self>) {
        if self.place_marks != marks {
            self.place_marks = marks;
            cx.notify();
        }
    }

    pub fn place_marks(&self) -> &[PlaceMark] {
        &self.place_marks
    }

    /// Replaces the badges of the gutter that open a menu of actions.
    pub fn set_action_marks(&mut self, marks: Vec<ActionMark>, cx: &mut Context<Self>) {
        if self.action_marks != marks {
            self.action_marks = marks;
            cx.notify();
        }
    }

    pub fn action_marks(&self) -> &[ActionMark] {
        &self.action_marks
    }

    /// Replaces the coverage stripes of the gutter.
    pub fn set_coverage(&mut self, marks: Vec<CoverageMark>, cx: &mut Context<Self>) {
        if self.coverage != marks {
            self.coverage = marks;
            cx.notify();
        }
    }

    pub fn coverage(&self) -> &[CoverageMark] {
        &self.coverage
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

    /// Replaces the breakpoints in the gutter that do not stop the program.
    pub fn set_muted_breakpoints(&mut self, lines: Vec<u32>, cx: &mut Context<Self>) {
        if self.muted_breakpoints != lines {
            self.muted_breakpoints = lines;
            cx.notify();
        }
    }

    pub fn muted_breakpoints(&self) -> &[u32] {
        &self.muted_breakpoints
    }

    /// Replaces the breakpoints the window muted: the gutter paints them grey.
    pub fn set_disabled_breakpoints(&mut self, lines: Vec<u32>, cx: &mut Context<Self>) {
        if self.disabled_breakpoints != lines {
            self.disabled_breakpoints = lines;
            cx.notify();
        }
    }

    pub fn disabled_breakpoints(&self) -> &[u32] {
        &self.disabled_breakpoints
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

    /// Replaces the breakpoints an agent set in the gutter, painted in the accent colour.
    pub fn set_agent_breakpoints(&mut self, lines: Vec<u32>, cx: &mut Context<Self>) {
        if self.agent_breakpoints != lines {
            self.agent_breakpoints = lines;
            cx.notify();
        }
    }

    pub fn agent_breakpoints(&self) -> &[u32] {
        &self.agent_breakpoints
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

    /// The marks of an agent's changes the user has not accepted yet.
    pub fn agent_marks(&self) -> &[GutterMark] {
        &self.agent_marks
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

    /// Where the change mark of the gutter stands, for a click in a test.
    pub fn change_mark_bounds(&self, line: usize) -> Option<Bounds<Pixels>> {
        self.mark_bounds(&self.gutter_marks, line)
    }

    /// Where the mark of an agent's change stands, for a click in a test.
    pub fn agent_mark_bounds(&self, line: usize) -> Option<Bounds<Pixels>> {
        self.mark_bounds(&self.agent_marks, line)
    }

    fn mark_bounds(&self, marks: &[GutterMark], line: usize) -> Option<Bounds<Pixels>> {
        let layout = self.layout.as_ref()?;
        let shown = layout
            .rows
            .iter()
            .find(|row| !row.is_block && row.buffer_line == line)?;
        let kind = marks
            .iter()
            .find(|mark| match mark.kind {
                GutterKind::Deleted => mark.lines.start == line as u32,
                _ => mark.lines.contains(&(line as u32)),
            })?
            .kind;
        let right = layout.bounds.left() + px(self.gutter_width);
        Some(crate::element::change_mark_bounds(
            right,
            shown.y,
            px(self.style.line_height),
            kind == GutterKind::Deleted,
        ))
    }

    /// The place of the editor in the window, as the last frame painted it.
    pub fn editor_bounds(&self) -> Option<Bounds<Pixels>> {
        self.layout.as_ref().map(|layout| layout.bounds)
    }

    /// The row of `line` in the window, when the last frame painted it; `None` while the
    /// line is out of sight or taken by a fold.
    pub fn line_bounds(&self, line: usize) -> Option<Bounds<Pixels>> {
        let layout = self.layout.as_ref()?;
        let shown = layout
            .rows
            .iter()
            .find(|row| !row.is_block && row.buffer_line == line)?;
        Some(Bounds::new(
            point(layout.bounds.left(), shown.y),
            size(layout.bounds.size.width, px(self.style.line_height)),
        ))
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

    /// The width of the gutter of the last frame, for a test that checks the column of
    /// the numbers follows the digits of the file.
    #[cfg(any(test, feature = "test-support"))]
    pub fn gutter_width(&self) -> f32 {
        self.gutter_width
    }

    /// The bounds of the number of `line` in the last frame, for a test that checks the
    /// column of the numbers holds it whole. `None` when the row is out of sight.
    #[cfg(any(test, feature = "test-support"))]
    pub fn line_number_bounds(&self, line: usize) -> Option<Bounds<Pixels>> {
        self.layout.as_ref()?.line_number_bounds(
            line,
            px(self.style.line_height),
            self.gutter_width,
        )
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

    /// The values of the stopped frame, by buffer line. A test reads them; the paint
    /// takes one line at a time.
    pub fn debug_values(&self) -> &[InlineValue] {
        &self.debug_values
    }

    /// The values of one buffer line, when the debugger stopped there.
    pub(crate) fn debug_value_at(&self, line: usize) -> Option<&InlineValue> {
        self.debug_values.iter().find(|value| value.line == line)
    }

    /// The inline watch of one buffer line, when it has one.
    pub(crate) fn inline_watch_at(&self, line: usize) -> Option<&InlineWatchRow> {
        self.inline_watches.iter().find(|watch| watch.line == line)
    }

    /// Shows the inline watches of the stopped program after their lines, and takes
    /// them away when the program runs.
    pub fn set_inline_watches(&mut self, watches: &[InlineWatchRow], cx: &mut Context<Self>) {
        if self.inline_watches != watches {
            self.inline_watches = watches.to_vec();
            cx.notify();
        }
    }

    /// The inline watches the editor shows, for a test.
    pub fn inline_watches(&self) -> &[InlineWatchRow] {
        &self.inline_watches
    }

    /// Whether Alt+Enter offers Add Inline Watch: the program is stopped in this file.
    pub fn set_inline_watch_ready(&mut self, ready: bool, cx: &mut Context<Self>) {
        if self.inline_watch_ready != ready {
            self.inline_watch_ready = ready;
            cx.notify();
        }
    }

    /// The line of the caret and the expression of the identifier chain around it, for
    /// an inline watch.
    pub fn inline_watch_at_caret(&self) -> Option<(usize, String)> {
        let state = self.state.as_ref()?;
        let caret = state.selections().primary().head;
        let expression = identifier_chain_at(&state.text(), caret)?;
        Some((state.store().offset_to_line(caret), expression))
    }

    /// The labels of an open completion list, in the order it shows them. A test reads
    /// them; nothing else does.
    #[cfg(any(test, feature = "test-support"))]
    pub fn completion_rows(&self) -> Option<Vec<String>> {
        let completion = self.lsp.as_ref()?.completion.as_ref()?;
        Some(completion.labels())
    }

    /// The headings of the groups of an open Override or Implement list, as the window
    /// shows them. A test reads them; the paint uses the same names.
    #[cfg(any(test, feature = "test-support"))]
    pub fn override_headers(&self) -> Option<Vec<String>> {
        let popup = self.lsp.as_ref()?.overrides.as_ref()?;
        let mut headers = Vec::new();
        let mut owner: Option<&str> = None;
        for index in &popup.shown {
            let method = &popup.methods[*index];
            if owner != Some(method.owner.as_str()) {
                owner = Some(method.owner.as_str());
                headers.push(crate::hierarchy::owner_heading(&method.owner).0);
            }
        }
        Some(headers)
    }

    /// The text of the note under the caret, when one is on screen. A test reads it; nothing
    /// else does.
    #[cfg(any(test, feature = "test-support"))]
    pub fn note_text(&self) -> Option<String> {
        Some(self.lsp.as_ref()?.note.as_ref()?.text.to_string())
    }

    /// The rows of an open completion list on screen and the selected one. A test reads
    /// them; nothing else does.
    #[cfg(any(test, feature = "test-support"))]
    pub fn completion_visible(&self) -> Option<(Vec<String>, Option<String>)> {
        let completion = self.lsp.as_ref()?.completion.as_ref()?;
        Some(completion.visible_labels())
    }

    /// The documentation of the selected entry of an open completion list, when it has
    /// arrived. A test reads it; the popup of the third Ctrl+Space uses the same text.
    #[cfg(any(test, feature = "test-support"))]
    pub fn completion_documentation(&self) -> Option<String> {
        let doc = self.lsp.as_ref()?.completion_doc.as_ref()?;
        Some(doc.text.clone())
    }

    /// The label of every row of an open completion list and the letters of it that the
    /// typed prefix matches. A test reads them; the paint uses the same ranges.
    #[cfg(any(test, feature = "test-support"))]
    pub fn completion_highlighted(&self) -> Option<Vec<(String, Vec<String>)>> {
        let completion = self.lsp.as_ref()?.completion.as_ref()?;
        Some(
            (0..completion.shown.len())
                .map(|row| {
                    let parts = completion.label(row);
                    let matched = completion
                        .highlights(row, &parts.left)
                        .into_iter()
                        .map(|range| parts.left[range].to_owned())
                        .collect();
                    (parts.left, matched)
                })
                .collect(),
        )
    }

    /// The sections and the labels of an open Alt+Enter list, in the order it shows
    /// them, and the preview lines of the selected row. A test reads them; nothing else
    /// does.
    #[cfg(any(test, feature = "test-support"))]
    pub fn actions_visible(&self) -> Option<ActionRows> {
        let menu = self.lsp.as_ref()?.actions.as_ref()?;
        let rows = menu
            .rows
            .iter()
            .enumerate()
            .map(|(row, pick)| (menu.quick_fix(row), pick.label().to_owned()))
            .collect();
        let preview = menu
            .has_sections()
            .then(|| self.action_preview(menu))
            .flatten()
            .map(|preview| preview.lines);
        Some(ActionRows { rows, preview })
    }

    /// The label of the selected row of an open Alt+Enter list. A test reads it; nothing else
    /// does.
    #[cfg(any(test, feature = "test-support"))]
    pub fn actions_selected(&self) -> Option<String> {
        let menu = self.lsp.as_ref()?.actions.as_ref()?;
        menu.rows
            .get(menu.selected)
            .map(|pick| pick.label().to_owned())
    }

    /// The places of an open Ctrl+B list, from zero. A test reads them; nothing else does.
    #[cfg(any(test, feature = "test-support"))]
    pub fn navigation_targets(&self) -> Option<usize> {
        self.lsp
            .as_ref()?
            .navigation_list
            .as_ref()
            .map(|list| list.targets.len())
    }

    /// The place of the cross of the inline watch on `line`, for a test that clicks it.
    #[cfg(any(test, feature = "test-support"))]
    pub fn inline_watch_cross_point(&self, line: usize) -> Option<Point<Pixels>> {
        let layout = self.layout.as_ref()?;
        let row = layout.rows.iter().find(|row| {
            row.buffer_line == line && row.watch.as_ref().is_some_and(|_| !row.line.continuation)
        })?;
        let watch = row.watch.as_ref()?;
        Some(point(
            watch.cross_x + watch.cross.width() / 2.,
            row.y + px(self.style.line_height / 2.),
        ))
    }

    /// Shows the values of the stopped frame after the lines of its method that name
    /// them. `line` is the buffer line of the stop, from zero; `None` takes the values
    /// away when the program runs.
    pub fn set_debug_values(
        &mut self,
        line: Option<usize>,
        values: &[DebugValue],
        cx: &mut Context<Self>,
    ) {
        // Until the text is read there are no names to find the values in: the loaded text
        // takes them from here.
        self.debug_source = match line {
            Some(line) if self.state.is_none() && !values.is_empty() => {
                Some((line, values.to_vec()))
            }
            _ => None,
        };
        let shown = line
            .filter(|_| !values.is_empty())
            .and_then(|line| self.values_on_lines(line, values))
            .unwrap_or_default();
        if self.debug_values != shown {
            self.debug_values = shown;
            cx.notify();
        }
    }

    /// Tints the row of `line`, the buffer line the stopped program is on, and draws its
    /// arrow in the gutter. `None` takes both away, when the program runs or stops nowhere
    /// in this file.
    pub fn set_execution_line(&mut self, line: Option<usize>, cx: &mut Context<Self>) {
        if self.execution_line != line {
            self.execution_line = line;
            cx.notify();
        }
    }

    /// The buffer line the stopped program is on, from zero, for a test.
    pub fn execution_line(&self) -> Option<usize> {
        self.execution_line
    }

    /// The values of `values` grouped by the lines that name them, from the start of the
    /// declaration around `line` through `line`.
    fn values_on_lines(&self, line: usize, values: &[DebugValue]) -> Option<Vec<InlineValue>> {
        let state = self.state.as_ref()?;
        let text = state.text();
        let identifiers = match &self.lsp {
            Some(lsp) => lsp.support.syntax().identifiers_around(&text, line),
            None => lang_api::identifiers_around(&text, line),
        };
        let mut shown = Vec::new();
        for (index, names) in identifiers.lines.iter().enumerate() {
            let mut parts = Vec::new();
            let mut changed = false;
            for name in names {
                let Some(value) = values.iter().find(|value| &value.name == name) else {
                    continue;
                };
                parts.push(format!(
                    "{} = {}",
                    value.name,
                    cut_value(&value.value, INLINE_VALUE_LIMIT)
                ));
                changed |= value.changed;
            }
            if !parts.is_empty() {
                shown.push(InlineValue {
                    line: identifiers.first_line + index,
                    text: parts.join(", "),
                    changed,
                });
            }
        }
        Some(shown)
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

    /// The line of the caret in window coordinates, as the last frame drew it. `None` while
    /// the frame did not draw the row of the caret.
    pub fn caret_line_bounds(&self) -> Option<Bounds<Pixels>> {
        let (layout, state) = (self.layout.as_ref()?, self.state.as_ref()?);
        let at = state.display().to_display(state.head());
        let shown = layout.rows.iter().find(|shown| shown.row == at.row)?;
        Some(Bounds::new(
            point(layout.bounds.left(), shown.y),
            size(layout.bounds.size.width, px(self.style.line_height)),
        ))
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

    /// Edits from outside the keyboard, joined with the previous step when `merge` says
    /// they belong to the same edit: a field of a table keeps one undo step per run.
    pub fn apply_edits_merging(
        &mut self,
        edits: Vec<text_core::Edit>,
        merge: bool,
        cx: &mut Context<Self>,
    ) {
        self.catch_up(cx);
        let Some(state) = &mut self.state else {
            return;
        };
        let change = state.apply_edits(edits);
        if change.is_some() && merge {
            state.merge_last_undo(2);
        }
        self.changed(change, cx);
    }

    /// Takes back the last edit, as the undo key of the editor does.
    pub fn undo(&mut self, cx: &mut Context<Self>) {
        self.catch_up(cx);
        let change = self
            .state
            .as_mut()
            .and_then(|state| state.perform(keymap::editor::UNDO, 1));
        self.changed(change, cx);
    }

    /// Applies the last undone edit again, as the redo key of the editor does.
    pub fn redo(&mut self, cx: &mut Context<Self>) {
        self.catch_up(cx);
        let change = self
            .state
            .as_mut()
            .and_then(|state| state.perform(keymap::editor::REDO, 1));
        self.changed(change, cx);
    }

    /// The change of several files the window keeps as one step, if any.
    pub fn workspace_undo(&self) -> Option<u64> {
        self.lsp.as_ref()?.workspace_undo
    }

    pub fn workspace_redo(&self) -> Option<u64> {
        self.lsp.as_ref()?.workspace_redo
    }

    /// The window names the step of its history that this file takes back.
    pub fn set_workspace_undo(&mut self, group: Option<u64>) {
        if let Some(lsp) = &mut self.lsp {
            lsp.workspace_undo = group;
        }
    }

    /// The window names the step of its history that this file applies again.
    pub fn set_workspace_redo(&mut self, group: Option<u64>) {
        if let Some(lsp) = &mut self.lsp {
            lsp.workspace_redo = group;
        }
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
                self.refresh_plain_spans(cx);
                self.attach(cx);
                self.reveal(cx);
                // The values a window gave before the text was read name their lines now.
                if let Some((line, values)) = self.debug_source.take() {
                    self.set_debug_values(Some(line), &values, cx);
                }
                let saved = crate::bookmarks::bookmark_lines(&self.path, cx);
                if let Some(state) = &mut self.state {
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

    /// Replaces the marks of an agent's changes beside the line numbers.
    pub fn set_agent_marks(&mut self, marks: Vec<GutterMark>, cx: &mut Context<Self>) {
        if self.agent_marks != marks {
            self.agent_marks = marks;
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
            self.annotate_hovered = None;
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

    /// The blame of a line from zero, for the menu of the column.
    pub fn blame_note_at_line(&self, line: usize) -> Option<BlameNote> {
        self.state.as_ref()?.annotation_label_at(line).cloned()
    }

    /// The room of the blame column left of the text, from zero when it is off. The width
    /// follows the longest label of the column.
    pub(crate) fn annotation_width(&self) -> f32 {
        if self.has_annotations() {
            self.annotation_room
        } else {
            0.
        }
    }

    /// The room of the gutter before the text; zero in the mode of Distraction Free.
    pub(crate) fn gutter_room(&self) -> f32 {
        if self.options.centered_text {
            self.annotation_width()
        } else {
            self.gutter_width + self.annotation_width()
        }
    }

    /// The left edge of the fold button of the gutter.
    fn fold_x(&self) -> f32 {
        crate::element::fold_x(self.gutter_width)
    }

    /// The text as the buffer holds it now, with LF line ends. `None` while loading.
    pub fn text(&self) -> Option<String> {
        self.state.as_ref().map(EditorState::text)
    }

    /// The document of the view. Its text is shared with the editor, so a reader outside
    /// the window copies it only when it asks. `None` while loading.
    pub fn document(&self) -> Option<text_core::Buffer> {
        self.state.as_ref().map(EditorState::document)
    }

    /// The selected text of the primary selection, `None` when the caret covers nothing.
    pub fn selected_text(&self) -> Option<String> {
        let state = self.state.as_ref()?;
        let range = state.selections().primary().range();
        (!range.is_empty()).then(|| state.store().slice(range))
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

    /// The primary selection as zero-based lines and UTF-16 characters, the units the
    /// language protocols count in.
    pub fn selection_points(&self) -> Option<((u32, u32), (u32, u32))> {
        let state = self.state.as_ref()?;
        let range = state.selections().primary().range();
        let store = state.store();
        let point = |offset: usize| {
            let line = store.offset_to_line(offset);
            let line_start = store.line_to_offset(line);
            let character = store.offset_to_utf16(offset) - store.offset_to_utf16(line_start);
            (line as u32, character as u32)
        };
        Some((point(range.start), point(range.end)))
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

    /// Where the indentation of the open file came from, when a file of the project
    /// named it: `.editorconfig` or the code style of the project.
    pub fn indent_source_name(&self) -> Option<&'static str> {
        self.state
            .as_ref()
            .and_then(|state| match state.indent().source {
                text_core::IndentSource::EditorConfig => Some("EditorConfig"),
                text_core::IndentSource::Project => Some("Project"),
                _ => None,
            })
    }

    /// Uses the window settings for this file from now on, because the user turned the
    /// project code style off. The next open reads the switch from the project.
    pub fn use_arro_code_style(&mut self, cx: &mut Context<Self>) {
        if let Some(state) = &mut self.state {
            state.drop_project_indent(
                self.options.use_tabs,
                self.options.indent_size,
                self.options.tab_size,
            );
        }
        cx.notify();
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
        let pictures_changed =
            self.options.markdown_images_from_internet != options.markdown_images_from_internet;
        self.options = options;
        // Settings reach every editor; View | Active Editor holds this one until it closes.
        if !self.wrap_locked {
            let soft_wrap = self.soft_wrap_override.unwrap_or(self.options.soft_wrap);
            if self.soft_wrap != soft_wrap {
                self.soft_wrap = soft_wrap;
                self.scroll_left = 0.;
            }
        }
        self.show_whitespace = self
            .show_whitespace_override
            .unwrap_or(self.options.show_whitespace);
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
            self.code_lens_resolve_task = None;
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
        if pictures_changed {
            self.pictures_from_internet(cx);
        }
        cx.notify();
    }

    /// What Settings turned on and off in this editor.
    pub fn options(&self) -> &EditorOptions {
        &self.options
    }

    /// The root of the project the file belongs to. Actions on save touch a file only
    /// inside it; a file opened alone has no root and no such check.
    pub fn set_project_root(&mut self, root: Option<PathBuf>) {
        self.project_root = root;
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
        let converted = self.application_paste(text, cx);
        let text = converted.as_deref().unwrap_or(text);
        let change = self.state.as_mut().and_then(|state| state.paste(text));
        self.changed(change, cx);
    }

    /// The text a paste inserts: the application may turn a copied command line into the
    /// request of this file.
    fn application_paste(&self, text: &str, cx: &App) -> Option<String> {
        let hook = cx.try_global::<PasteText>()?;
        (hook.0)(&self.path, text)
    }

    /// Ctrl+V: the clipboard as the application wants it in this file.
    fn paste_from_clipboard(&mut self, cx: &mut Context<Self>) {
        self.catch_up(cx);
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        let converted = self.application_paste(&text, cx);
        let text = converted.as_deref().unwrap_or(&text);
        let change = self.state.as_mut().and_then(|state| state.paste(text));
        self.changed(change, cx);
    }

    pub fn save(&mut self, cx: &mut Context<Self>) {
        let Some(requests) = self.save_action_requests() else {
            self.write_save(cx);
            return;
        };
        // The count covers the wait for the language: an external change of the file
        // during it is a change against a save that has not written yet.
        self.saving.fetch_add(1, Ordering::SeqCst);
        let mut guard = SavingGuard::new(self.saving.clone());
        let deadline = cx.background_executor().timer(SAVE_ACTIONS_TIMEOUT);
        cx.spawn(async move |this, cx| {
            let outcome = collect_save_actions(requests, deadline).await;
            guard.finish();
            this.update(cx, |this, cx| this.finish_save_actions(outcome, cx))
                .ok();
        })
        .detach();
    }

    /// The questions the settings of actions on save put to the language, `None` when a
    /// save has nothing to ask or may not touch the file.
    fn save_action_requests(&self) -> Option<SaveActionRequests> {
        let actions = &self.options.actions_on_save;
        if !actions.any() || !actions.matches(&self.path) {
            return None;
        }
        let state = self.state.as_ref()?;
        if state.is_read_only() || !state.is_dirty() {
            return None;
        }
        if has_conflict_markers(&state.text()) {
            return None;
        }
        if let Some(root) = &self.project_root
            && !self.path.starts_with(root)
        {
            return None;
        }
        let lsp = self.lsp.as_ref()?;
        let uri = lsp.uri.clone();
        let (tab_size, insert_spaces) = self.formatting_options();
        let version = state.version();
        let mut ranges = Vec::new();
        if actions.reformat && actions.reformat_changed {
            let store = state.store();
            ranges = self
                .gutter_marks
                .iter()
                .map(|mark| lines_span(&store, mark.lines.clone()))
                .collect();
        }
        let support = &lsp.support;
        let mut format = Vec::new();
        let mut missing = false;
        if actions.reformat {
            if !ranges.is_empty()
                && let Some(provider) = support.range_format()
            {
                for range in &ranges {
                    format.push(ask(
                        provider,
                        RangeFormatQuery {
                            uri: uri.clone(),
                            version: version as i32,
                            range: *range,
                            tab_size,
                            insert_spaces,
                        },
                    ));
                }
            } else if let Some(provider) = support.format() {
                format.push(ask(
                    provider,
                    FormatQuery {
                        uri: uri.clone(),
                        version: version as i32,
                        tab_size,
                        insert_spaces,
                    },
                ));
            } else {
                missing = true;
            }
        }
        let mut replies = Vec::new();
        if actions.optimize_imports || actions.cleanup {
            match support.actions() {
                Some(provider) => {
                    let store = state.store();
                    let span = whole_span(&store);
                    if actions.optimize_imports {
                        replies.push(ask(
                            provider,
                            ActionQuery {
                                uri: uri.clone(),
                                version: version as i32,
                                span,
                                only: Some(ActionKind::OrganizeImports),
                            },
                        ));
                    }
                    if actions.cleanup {
                        replies.push(ask(
                            provider,
                            ActionQuery {
                                uri: uri.clone(),
                                version: version as i32,
                                span,
                                only: Some(ActionKind::FixAll),
                            },
                        ));
                    }
                }
                None => missing = true,
            }
        }
        // With no request to wait for, the note of the missing provider is the answer.
        Some(SaveActionRequests {
            version,
            format,
            actions: replies,
            ranges,
            missing,
        })
    }

    /// The language answered or the wait is over: the edits of the save go into the
    /// buffer as one undo step, then the text is written.
    fn finish_save_actions(&mut self, outcome: SaveActionOutcome, cx: &mut Context<Self>) {
        let current = self.state.as_ref().map(EditorState::version);
        if current == Some(outcome.version)
            && let Some(edits) = outcome.edits
            && let Some(state) = &self.state
        {
            let store = state.store();
            let edits: Vec<DocumentEdit> = if outcome.ranges.is_empty() {
                edits
            } else {
                edits
                    .into_iter()
                    .filter(|edit| {
                        let start = crate::semantic::offset_at(&store, edit.span.start);
                        let end = crate::semantic::offset_at(&store, edit.span.end).max(start);
                        outcome.ranges.iter().any(|range| {
                            let range_start = crate::semantic::offset_at(&store, range.start);
                            let range_end =
                                crate::semantic::offset_at(&store, range.end).max(range_start);
                            start <= range_end && end >= range_start
                        })
                    })
                    .collect()
            };
            if let Some(edits) = crate::actions::server_edits(&store, edits) {
                let scroll = (self.scroll_top, self.scroll_left);
                let change = self
                    .state
                    .as_mut()
                    .and_then(|state| state.apply_save_action_edits(edits));
                if change.is_some() {
                    self.changed(change, cx);
                    self.scroll_top = scroll.0;
                    self.scroll_left = scroll.1;
                    self.autoscroll = false;
                }
            }
        }
        if let Some(note) = outcome.note {
            cx.emit(EditorEvent::SaveNote(note.into()));
        }
        self.write_save(cx);
    }

    /// The save without the language: the rules of `.editorconfig` and the settings go
    /// into the buffer, then its text is written.
    fn write_save(&mut self, cx: &mut Context<Self>) {
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

    /// Reads a class of a library again, after its sources were attached. The caret stays
    /// on the word it was on where the new text still has that word.
    pub fn replace_source_text(&mut self, text: String, cx: &mut Context<Self>) {
        let Some(state) = &self.state else {
            return;
        };
        let anchor = word_at_caret(&state.text(), self.caret());
        let Some(state) = &mut self.state else {
            return;
        };
        let change = state.take_source_text(&text);
        Self::set_source_text(&self.path, &text, cx);
        self.changed(change, cx);
        if let Some((word, offset)) = anchor
            && let Some(found) = nearest_word(&text, &word, offset)
        {
            let line = text[..found].bytes().filter(|byte| *byte == b'\n').count();
            let line_start = text[..found].rfind('\n').map_or(0, |at| at + 1);
            let column = text[line_start..found].chars().count();
            if let Some(state) = &mut self.state {
                state.set_caret(line, column);
            }
        }
        cx.notify();
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
        // The menu of Alt+Enter belongs to the text and the word it opened on.
        if change.is_some() {
            self.dismiss_actions_popup(cx);
            // An edit of this file is newer than the refactoring of the window: its
            // undo step no longer follows this text.
            if let Some(lsp) = &mut self.lsp {
                lsp.workspace_undo = None;
                lsp.workspace_redo = None;
            }
        } else {
            self.follow_actions_popup(cx);
        }
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
            self.request_code_lens(crate::semantic::CODE_LENS_TYPING_DELAY, cx);
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
                if lsp.rename_in_place.is_some()
                    && let Some(state) = &self.state
                {
                    let support = lsp.support.clone();
                    let text = state.text();
                    let version = state.version();
                    if let Some(rename) = &mut lsp.rename_in_place {
                        rename.follow(&change.steps, &text, version, false);
                        let reserved = support.reserved_word(&rename.text);
                        rename.refresh_problem(reserved);
                    }
                }
                lsp.retain_transformed_marks(&change.steps, len);
                lsp.retain_transformed_tokens(&change.steps, len);
            }
            self.apply_tokens(cx);
            self.apply_marks(cx);
            self.usages = None;
            self.sync_bookmarks(cx);
        }
        if change.is_some() {
            self.debug_values.clear();
            cx.emit(EditorEvent::Changed);
            if self.bar.is_some() {
                self.refresh_matches(false, cx);
            }
            self.refresh_plain_spans(cx);
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

    /// Asks the application for the colours of a format without a grammar and puts them
    /// on the text.
    fn refresh_plain_spans(&mut self, cx: &App) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        if state.language().is_some() {
            return;
        }
        let Some(plain) = cx.try_global::<PlainSyntax>() else {
            return;
        };
        let snapshot = state.snapshot();
        let text = snapshot.slice(0..snapshot.len());
        if let Some(spans) = (plain.resolve)(&self.path, &text) {
            state.set_spans(spans);
        }
    }

    /// Whether Ctrl+Enter belongs to the application in this file.
    fn runs_on_enter(&self, cx: &App) -> bool {
        let plain = cx
            .try_global::<PlainSyntax>()
            .is_some_and(|plain| (plain.runs_on_enter)(&self.path));
        plain
            || cx
                .try_global::<RunFiles>()
                .is_some_and(|files| (files.0)(&self.path))
    }

    /// The row the application adds above the text of this file.
    fn file_bar(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let bar = cx.try_global::<FileBar>().map(|bar| bar.0.clone())?;
        let path = self.path.clone();
        bar(&path, window, cx)
    }

    /// The colour the editor paints at `offset`: the colour of the highlight span that
    /// covers it, or the plain code colour. A test of markup compares the colours of
    /// plain text and of the words inside a span.
    pub fn painted_color_at(&self, offset: usize) -> Option<Hsla> {
        let state = self.state.as_ref()?;
        if offset >= state.store().len() {
            return None;
        }
        let mut font = ui_kit::font(self.style.font_family.clone());
        font.features = FontFeatures::disable_ligatures();
        Some(crate::element::look_at(offset, state.spans(), &self.style, &font).color)
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

    /// Lets soft wrap follow the options again after [`Self::lock_soft_wrap`].
    pub fn unlock_soft_wrap(&mut self) {
        self.wrap_locked = false;
        self.soft_wrap = self.soft_wrap_override.unwrap_or(self.options.soft_wrap);
    }

    /// Collapses the bytes of `range` behind `placeholder`, as a comparison does with the
    /// lines that did not change. A click on the placeholder shows the lines again.
    pub fn collapse_range(
        &mut self,
        range: std::ops::Range<usize>,
        placeholder: impl Into<String>,
        cx: &mut Context<Self>,
    ) {
        if let Some(state) = self.state.as_mut() {
            state.collapse_lines(range, placeholder.into());
            cx.notify();
        }
    }

    /// Shows every folded line again, including the ones a comparison hid.
    pub fn clear_folds(&mut self, cx: &mut Context<Self>) {
        if let Some(state) = self.state.as_mut() {
            state.clear_folds();
            cx.notify();
        }
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

    /// A menu of the gutter toggles the bookmark of `line` from zero, not of the caret.
    pub fn toggle_bookmark_at_line(
        &mut self,
        line: usize,
        mnemonic: Option<char>,
        cx: &mut Context<Self>,
    ) {
        self.catch_up(cx);
        if let Some(state) = &mut self.state {
            state.toggle_bookmark_at_line(line, mnemonic);
        }
        self.sync_bookmarks(cx);
        cx.notify();
    }

    /// Delete of the Bookmarks window takes the bookmark of `line` from zero away.
    pub fn remove_bookmark_at_line(&mut self, line: usize, cx: &mut Context<Self>) {
        self.catch_up(cx);
        if let Some(state) = &mut self.state {
            state.remove_bookmark_at_line(line);
        }
        self.sync_bookmarks(cx);
        cx.notify();
    }

    /// Ctrl+F11 from a menu: the next typed key names the bookmark of `line`.
    pub fn await_mnemonic_bookmark_at(&mut self, line: usize, cx: &mut Context<Self>) {
        self.awaiting_mnemonic = true;
        self.awaiting_mnemonic_line = Some(line);
        cx.notify();
    }

    /// The bookmark of a line from zero, `Some(None)` for one without a mnemonic.
    pub fn bookmark_at_line(&self, line: usize) -> Option<Option<char>> {
        self.state.as_ref()?.bookmark_at_line(line)
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
        crate::bookmarks::replace_bookmark_lines(&self.path, &lines, cx);
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

    /// The height of the text: the viewport less the horizontal scrollbar under the text.
    fn text_height(&self) -> f32 {
        f32::from(self.viewport.height)
            - if self.horizontal_bar {
                crate::scrollbar::HORIZONTAL_HEIGHT
            } else {
                0.
            }
    }

    fn page_rows(&self) -> usize {
        (self.text_height() / self.style.line_height)
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
        let text_height = self.text_height();
        let Some(state) = &mut self.state else {
            return;
        };
        let line_height = self.style.line_height.max(1.);
        let display = state.display();
        let rows = display.row_count();
        let caret_row = display.to_display(state.head()).row as isize;
        let visible = (text_height / line_height).floor().max(1.) as isize;
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
        let height = self.text_height().max(line_height);
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
        // A view painted as a pane of another one has no render of its own to drain the
        // list, so it keeps only the latest keys.
        if self.pending_frames.len() < PENDING_FRAMES {
            self.pending_frames.push(Instant::now());
        }
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
        // The documentation keeps its place for the keys that scroll it; Escape and a
        // typed letter close it, and a click in the editor or a tab switch as well.
        if self.quick_doc_open()
            && !stroke.ctrl
            && !stroke.alt
            && !stroke.shift
            && self.quick_doc_scroll_key(&stroke.key, cx)
        {
            cx.stop_propagation();
            return;
        }
        if !quick_key {
            self.cancel_hover();
        }
        // The documentation outlives the keys that do not change the text: Escape closes
        // it, a key that types or edits closes it and reaches the editor, and so does a
        // click outside or a tab switch, as in IDEA.
        if self.quick_doc_open() && !quick_key {
            let escape = stroke.key == "escape" && !stroke.ctrl && !stroke.alt && !stroke.shift;
            let edits_text = event.keystroke.key_char.is_some()
                || matches!(
                    stroke.key.as_str(),
                    "backspace" | "delete" | "enter" | "tab"
                );
            if escape || edits_text {
                self.close_quick_doc(cx);
                if escape {
                    cx.stop_propagation();
                    return;
                }
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
                let letter = letter.to_ascii_uppercase();
                match self.awaiting_mnemonic_line.take() {
                    Some(line) => self.toggle_bookmark_at_line(line, Some(letter), cx),
                    None => self.toggle_bookmark(Some(letter), cx),
                }
            }
            self.awaiting_mnemonic_line = None;
            cx.stop_propagation();
            return;
        }
        if self.region_popup.is_some() && self.region_key(event, cx) {
            cx.stop_propagation();
            return;
        }
        if self.crumb_key(event, cx) {
            cx.stop_propagation();
            return;
        }
        if self.bar_active() && self.bar_key(event, cx) {
            cx.stop_propagation();
            return;
        }
        // Escape closes the topmost popup of the editor and nothing else.
        if stroke.key == "escape"
            && !stroke.ctrl
            && !stroke.alt
            && !stroke.shift
            && self.close_top_popup(cx)
        {
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
        if plain && self.name_key(&stroke.key, cx) {
            cx.stop_propagation();
            return;
        }
        // Enter expands a template that chose Enter, when its abbreviation ends here and
        // no list of a popup is open.
        if plain && stroke.key == "enter" && self.template_expand_key(ExpandWith::Enter, cx) {
            cx.stop_propagation();
            return;
        }
        if plain && self.navigation_key(&stroke.key, cx) {
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
        // Ctrl+Enter in a format the application runs sends the thing at the caret
        // instead of splitting the line.
        if stroke.ctrl
            && !stroke.alt
            && !stroke.shift
            && stroke.key == "enter"
            && self.runs_on_enter(cx)
        {
            let line = self.caret_line().unwrap_or(0) as u32;
            cx.emit(EditorEvent::RunRequest(line));
            cx.stop_propagation();
            return;
        }
        // Ctrl+F2 stops what such a format runs, as it stops a program elsewhere.
        // The action of the runnable context does that, before this handler sees a key
        // the window has bound.
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
        // A refactoring of several files is one step of the window's history: this file
        // asks the window for it instead of taking back its own buffer alone.
        if action == actions::UNDO
            && let Some(group) = self.workspace_undo()
        {
            cx.emit(EditorEvent::UndoRefactoring(group));
            return;
        }
        if action == actions::REDO
            && let Some(group) = self.workspace_redo()
        {
            cx.emit(EditorEvent::RedoRefactoring(group));
            return;
        }
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
            actions::PARAMETER_INFO => return self.parameter_info_key(cx),
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
            actions::FIND_USAGES_IN_FILE => return self.find_usages_in_file(cx),
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
                if self.navigation_key("enter", cx) || self.completion_key("enter", cx) {
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
                if self.navigation_key("up", cx) || self.completion_key("up", cx) {
                    return;
                }
            }
            actions::LOOKUP_DOWN => {
                if self.navigation_key("down", cx) || self.completion_key("down", cx) {
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
            actions::PULL_MEMBERS_UP => {
                return self.member_refactoring(lang_api::MemberRefactoring::PullUp, cx);
            }
            actions::PUSH_MEMBERS_DOWN => {
                return self.member_refactoring(lang_api::MemberRefactoring::PushDown, cx);
            }
            actions::EXTRACT_SUPERCLASS => {
                return self.member_refactoring(lang_api::MemberRefactoring::ExtractSuperclass, cx);
            }
            actions::INTRODUCE_PARAMETER_OBJECT => {
                return self
                    .member_refactoring(lang_api::MemberRefactoring::IntroduceParameterObject, cx);
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
            actions::SHOW_DIAGRAM => return self.show_diagram(cx),
            actions::SHOW_SEQUENCE_DIAGRAM => return self.show_sequence_diagram(cx),
            actions::GOTO_TYPE_DECLARATION => return self.type_declaration(cx),
            actions::QUICK_DEFINITION => return self.quick_definition(cx),
            actions::COPY_REFERENCE => return self.copy_reference(cx),
            actions::SAFE_DELETE => return self.safe_delete(cx),
            actions::UNWRAP => return self.unwrap_remove(cx),
            actions::COMPLETE_STATEMENT => return self.complete_statement(cx),
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
            actions::PASTE_HISTORY => return cx.emit(EditorEvent::PasteHistory),
            actions::PASTE => return self.paste_from_clipboard(cx),
            actions::TOGGLE_SOFT_WRAP => {
                if !self.wrap_locked {
                    self.soft_wrap = !self.soft_wrap;
                    self.soft_wrap_override = Some(self.soft_wrap);
                    self.scroll_left = 0.;
                }
                return cx.notify();
            }
            actions::TOGGLE_WHITESPACE => {
                self.show_whitespace = !self.show_whitespace;
                self.show_whitespace_override = Some(self.show_whitespace);
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
            actions::COLUMN_MODE => {
                state.toggle_column_mode();
                cx.notify();
                return;
            }
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
        let gutter = self.gutter_room() - self.annotation_width();
        self.layout
            .as_ref()
            .is_some_and(|layout| gutter > 0. && position.x < layout.bounds.left() + px(gutter))
    }

    /// Whether the left column of the gutter shows a mark of the line: a debug mark, a
    /// run mark or a bookmark. The badge of a place or a handler gives way to it, so its
    /// own click waits for a line without one.
    pub(crate) fn debug_mark_on_line(&self, line: u32) -> bool {
        self.function_breakpoints.contains(&line)
            || self.field_watchpoints.contains(&line)
            || self.breakpoints.contains(&line)
            || self.run_marks.iter().any(|mark| mark.line == line)
            || self
                .state
                .as_ref()
                .is_some_and(|state| state.bookmark_at_line(line as usize).is_some())
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
        let x = position.x
            - layout.bounds.left()
            - px(self.gutter_room() + TEXT_INSET)
            - px(self.center_inset)
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
            && position.x >= layout.bounds.left() + px(self.gutter_width)
            && position.x < layout.bounds.left() + px(self.gutter_width + width)
    }

    /// The line from zero under `position`, whichever column it falls in. Rows of a
    /// block speak for the line under them.
    fn line_at(&self, position: Point<Pixels>) -> Option<u32> {
        let layout = self.layout.as_ref()?;
        let line_height = px(self.style.line_height);
        let shown = layout
            .rows
            .iter()
            .find(|laid_out| position.y >= laid_out.y && position.y < laid_out.y + line_height)?;
        Some(shown.buffer_line as u32)
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

    /// The hint of a badge of the gutter under the pointer: its text and where it stands.
    fn place_hint_at(&self, position: Point<Pixels>) -> Option<(String, Point<Pixels>)> {
        let layout = self.layout.as_ref()?;
        let left = layout.bounds.left();
        if !self.in_gutter(position) || position.x < left || position.x >= left + px(self.fold_x())
        {
            return None;
        }
        let line_height = px(self.style.line_height);
        let shown = layout.rows.iter().find(|laid_out| {
            !laid_out.is_block
                && !laid_out.line.continuation
                && position.y >= laid_out.y
                && position.y < laid_out.y + line_height
        })?;
        let mark = self
            .place_marks
            .iter()
            .find(|mark| mark.line as usize == shown.buffer_line)?;
        if self.debug_mark_on_line(shown.buffer_line as u32) {
            return None;
        }
        Some((mark.hint.to_string(), point(position.x, shown.y)))
    }

    /// The change mark of the gutter under `position`: its line, from zero. The marks
    /// stand against the right edge of the number column, where the text begins, and a
    /// click anywhere on the mark of a line opens it.
    fn change_mark_at(&self, position: Point<Pixels>) -> Option<u32> {
        self.mark_at(&self.gutter_marks, position)
    }

    /// The mark of an agent's change under `position`, if there is one. The marks of
    /// agents and of the version control may share a line: the agent's mark wins, so a
    /// click reaches the hunk the user has not accepted yet.
    fn agent_mark_at(&self, position: Point<Pixels>) -> Option<u32> {
        self.mark_at(&self.agent_marks, position)
    }

    fn mark_at(&self, marks: &[GutterMark], position: Point<Pixels>) -> Option<u32> {
        if !self.options.gutter_icons || self.options.centered_text {
            return None;
        }
        let layout = self.layout.as_ref()?;
        let line_height = px(self.style.line_height);
        let shown = layout.rows.iter().find(|row| {
            !row.is_block
                && !row.line.continuation
                && position.y >= row.y
                && position.y < row.y + line_height
        })?;
        let line = shown.buffer_line as u32;
        let mark = marks.iter().find(|mark| match mark.kind {
            GutterKind::Deleted => mark.lines.start == line,
            _ => mark.lines.contains(&line),
        })?;
        let right = layout.bounds.left() + px(self.gutter_width);
        let bounds = crate::element::change_mark_bounds(
            right,
            shown.y,
            line_height,
            mark.kind == GutterKind::Deleted,
        );
        inside(bounds, position).then_some(line)
    }

    /// The hint of the coverage stripe under the pointer, when the pointer is in the
    /// gutter: its text and where it stands.
    fn coverage_hint_at(&self, position: Point<Pixels>) -> Option<(String, Point<Pixels>)> {
        let layout = self.layout.as_ref()?;
        if !self.in_gutter(position) || position.x < layout.bounds.left() {
            return None;
        }
        let line_height = px(self.style.line_height);
        let shown = layout.rows.iter().find(|laid_out| {
            !laid_out.is_block
                && !laid_out.line.continuation
                && position.y >= laid_out.y
                && position.y < laid_out.y + line_height
        })?;
        let mark = self
            .coverage
            .iter()
            .find(|mark| mark.line as usize == shown.buffer_line)?;
        Some((mark.hint(), point(position.x, shown.y)))
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
        // A click outside the name applies the rename in place, as Enter does.
        if self
            .lsp
            .as_ref()
            .is_some_and(|lsp| lsp.rename_in_place.is_some())
        {
            self.leave_rename_in_place(cx);
        }
        self.cancel_hover();
        self.close_quick_doc(cx);
        // A click in the editor closes the list of entries in the way.
        self.dismiss_click_popups(cx);
        self.gutter_press = None;
        self.drag = None;
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
        // The cross of an inline watch removes it; the text of the watch is not a link.
        if layout.rows[shown].watch.as_ref().is_some_and(|watch| {
            event.position.x >= watch.cross_x
                && event.position.x < watch.cross_x + watch.cross.width() + px(4.)
        }) {
            let expression = layout.rows[shown]
                .watch
                .as_ref()
                .map(|watch| watch.expression.clone())
                .unwrap_or_default();
            let line = layout.rows[shown].buffer_line;
            cx.emit(EditorEvent::InlineWatchRemoved { line, expression });
            return;
        }
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
            self.drag = drag_of(event.click_count, offset);
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
        // A mark of the left column owns only its own icon; the number beside it still
        // places a breakpoint, as it does on a line without a mark.
        let over_mark_icon =
            event.position.x - layout.bounds.left() < px(crate::element::GUTTER_MARK);
        if self.options.gutter_icons
            && let Some(buffer_line) = run_line
            && !line.continuation
        {
            if over_mark_icon
                && !self.debug_mark_on_line(buffer_line as u32)
                && self
                    .place_marks
                    .iter()
                    .any(|mark| mark.line as usize == buffer_line)
            {
                cx.emit(EditorEvent::PlaceMark(buffer_line as u32));
                return;
            }
            if over_mark_icon
                && !self.debug_mark_on_line(buffer_line as u32)
                && self
                    .action_marks
                    .iter()
                    .any(|mark| mark.line as usize == buffer_line)
            {
                cx.emit(EditorEvent::ActionMark(buffer_line as u32));
                return;
            }
            if over_mark_icon
                && !self.debug_mark_on_line(buffer_line as u32)
                && self
                    .endpoint_marks
                    .iter()
                    .any(|mark| mark.line as usize == buffer_line)
            {
                cx.emit(EditorEvent::EndpointMark(buffer_line as u32));
                return;
            }
        }
        if self.options.gutter_icons
            && let Some(buffer_line) = run_line
            && !line.continuation
            && over_mark_icon
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
        if self.options.gutter_icons
            && let Some(buffer_line) = run_line
            && !line.continuation
            && layout.rows[shown].doc_icon.is_some()
        {
            let x = event.position.x - layout.bounds.left();
            if x >= px(self.fold_x()) && x < px(self.fold_x() + crate::element::FOLD_SIZE) {
                if let Some(range) = self.doc_range_at_line(buffer_line) {
                    self.toggle_rendered_doc_at(range, cx);
                }
                return;
            }
        }
        // A mark of an agent's change opens its own window, and the version control
        // marks open theirs.
        if let Some(mark) = self.agent_mark_at(event.position) {
            cx.emit(EditorEvent::AgentMarkClick(mark));
            return;
        }
        // A change mark opens the window of the hunk, not a breakpoint.
        if let Some(mark) = self.change_mark_at(event.position) {
            self.change_mark_hovered = Some(mark);
            cx.emit(EditorEvent::ChangeMarkClick(mark));
            return;
        }
        let in_gutter = self.in_gutter(event.position);
        self.catch_up(cx);
        if self.options.gutter_icons
            && position_in_fold_column(event.position, layout.bounds, self.fold_x())
            && let Some(state) = &mut self.state
        {
            state.toggle_fold_at_line(state.store().offset_to_line(line.start_offset));
            cx.notify();
            return;
        }
        if in_gutter && !line.continuation {
            // The breakpoint waits for the release: a drag over the lines selects them
            // instead of toggling, as IDEA does. The whole number and icon area is one
            // zone, so a click on a number places a breakpoint.
            if let Some(buffer_line) = run_line {
                self.gutter_press = Some(GutterPress {
                    line: buffer_line as u32,
                    shift: event.modifiers.shift,
                    start: event.position,
                    dragging: false,
                });
            }
            return;
        }
        let Some(state) = &mut self.state else {
            return;
        };
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
        let block = (modifiers.alt && !modifiers.shift) || state.column_mode();
        let add_caret = modifiers.alt;
        let kept = state.carets();
        state.click(
            offset,
            modifiers.shift && !add_caret,
            add_caret,
            event.click_count,
        );
        self.drag = drag_of(event.click_count, offset);
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
        self.dismiss_click_popups(cx);
        // The blame column and the line numbers have menus of their own: the rows of the
        // editor belong to the text alone.
        if self.in_annotations(event.position) {
            let line = self.line_at(event.position);
            cx.emit(EditorEvent::BlameMenu {
                position: event.position,
                line: line.unwrap_or(0),
            });
            return;
        }
        if self.in_gutter(event.position) {
            let line = self.line_at(event.position);
            cx.emit(EditorEvent::GutterMenu {
                position: event.position,
                line: line.unwrap_or(0),
            });
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

        // A drag that began on a line number selects whole lines; the click that
        // places a breakpoint comes on the release.
        if event.dragging() && self.gutter_drag(event.position, cx) {
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
                            StripeKind::Agent => "Agent change".to_owned(),
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
        let annotate_hovered = self
            .annotation_under(event.position)
            .map(|(note, _, _)| note.commit.clone())
            .filter(|commit| !commit.is_empty());
        if annotate_hovered != self.annotate_hovered {
            self.annotate_hovered = annotate_hovered;
            cx.notify();
        }

        let coverage_hint = self.coverage_hint_at(event.position);
        if coverage_hint != self.coverage_hint {
            self.coverage_hint = coverage_hint;
            cx.notify();
        }

        let place_hint = self.place_hint_at(event.position);
        if place_hint != self.place_hint {
            self.place_hint = place_hint;
            cx.notify();
        }

        let change_mark = if self.agent_mark_at(event.position).is_some() {
            None
        } else {
            self.change_mark_at(event.position)
        };
        if change_mark != self.change_mark_hovered {
            self.change_mark_hovered = change_mark;
            cx.emit(EditorEvent::ChangeMarkHover(change_mark));
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
            match (&self.block_drag, self.drag) {
                (Some((origin, kept)), _) => state.select_block(kept, *origin, offset),
                (None, Some(DragUnit::Words(anchor))) => state.drag_words(anchor, offset),
                (None, Some(DragUnit::Lines(anchor))) => state.drag_lines(anchor, offset),
                (None, None) => state.drag_to(offset),
            }
            self.autoscroll = true;
            cx.notify();
        }
    }

    pub(crate) fn mouse_up(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        // A press in the gutter that did not become a drag is a click: the line places
        // or removes a breakpoint, or the run mark of it, as the language allows. A
        // release outside the gutter is no click of it.
        if let Some(press) = self.gutter_press.take()
            && !press.dragging
            && self.in_gutter(position)
        {
            cx.emit(EditorEvent::GutterClick {
                line: press.line,
                shift: press.shift,
            });
        }
        self.selecting = false;
        self.drag = None;
        self.block_drag = None;
        self.scrollbar_drag = None;
    }

    /// A pointer held down after a press in the gutter. A drag selects the lines it
    /// crosses, the way IDEA does; the press itself only waits for the release.
    /// Answers whether the pointer still belongs to the gutter.
    fn gutter_drag(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) -> bool {
        let Some(press) = &mut self.gutter_press else {
            return false;
        };
        if (position.y - press.start.y).abs() >= px(2.) {
            press.dragging = true;
        }
        if !press.dragging {
            return true;
        }
        let first = press.line as usize;
        let Some(layout) = self.layout.clone() else {
            return true;
        };
        let line_height = px(self.style.line_height);
        let Some(row) = layout
            .rows
            .iter()
            .find(|row| !row.is_block && position.y >= row.y && position.y < row.y + line_height)
        else {
            return true;
        };
        let last = row.buffer_line;
        let Some(state) = &mut self.state else {
            return true;
        };
        let store = state.store();
        let (first, last) = (first.min(last), first.max(last));
        let from = store.line_to_offset(first);
        let to = if last + 1 < store.line_count() {
            store.line_to_offset(last + 1)
        } else {
            store.line_end(last)
        };
        state.select_range(from..to);
        self.caret_on = true;
        self.changed(None, cx);
        true
    }
}

/// The identifier chain, with dots, around a byte offset of the text.
pub(crate) fn identifier_chain_at(text: &str, head: usize) -> Option<String> {
    if text.is_empty() {
        return None;
    }
    let head = head.min(text.len());
    let ident = |ch: char| ch.is_alphanumeric() || ch == '_' || ch == '$' || ch == '.';
    let mut start = head;
    while start > 0 {
        let previous = text[..start].chars().next_back()?;
        if !ident(previous) {
            break;
        }
        start -= previous.len_utf8();
    }
    let mut end = head;
    while end < text.len() {
        let next = text[end..].chars().next()?;
        if !ident(next) {
            break;
        }
        end += next.len_utf8();
    }
    let expression = text.get(start..end)?.trim_matches('.');
    if expression.is_empty()
        || !expression
            .chars()
            .next()
            .is_some_and(|ch| ch.is_alphanumeric() || ch == '_' || ch == '$')
    {
        return None;
    }
    Some(expression.to_owned())
}

/// The word under a caret, with the byte offset of its start, for a text that is read
/// again under the same caret.
fn word_at_caret(text: &str, caret: (usize, usize)) -> Option<(String, usize)> {
    let (line, column) = caret;
    let start = text
        .split_inclusive('\n')
        .take(line)
        .map(str::len)
        .sum::<usize>();
    let line_text = text[start..].split('\n').next().unwrap_or_default();
    let mut offset = start
        + line_text
            .char_indices()
            .nth(column)
            .map_or(line_text.len(), |(at, _)| at);
    let is_word =
        |character: char| character.is_alphanumeric() || character == '_' || character == '$';
    while offset > start && text[..offset].chars().next_back().is_some_and(is_word) {
        offset -= text[..offset].chars().next_back()?.len_utf8();
    }
    let end = text[offset..]
        .char_indices()
        .find(|(_, character)| !is_word(*character))
        .map_or(text.len(), |(at, _)| offset + at);
    (offset < end).then(|| (text[offset..end].to_owned(), offset))
}

/// The occurrence of `word` in `text` nearest to `offset`, for a caret that should stay
/// on the same symbol after the text changed.
fn nearest_word(text: &str, word: &str, offset: usize) -> Option<usize> {
    let is_word =
        |character: char| character.is_alphanumeric() || character == '_' || character == '$';
    let mut best = None;
    let mut from = 0;
    while let Some(found) = text[from..].find(word) {
        let start = from + found;
        let end = start + word.len();
        let whole = !text[..start].chars().next_back().is_some_and(is_word)
            && !text[end..].chars().next().is_some_and(is_word);
        if whole {
            let distance = start.abs_diff(offset);
            if best.is_none_or(|(_, best): (usize, usize)| distance < best) {
                best = Some((start, distance));
            }
        }
        from = end;
    }
    best.map(|(start, _)| start)
}

fn position_in_fold_column(position: Point<Pixels>, bounds: Bounds<Pixels>, fold_x: f32) -> bool {
    // The arrow is a square; a little room around it makes it easier to hit, but the
    // rest of the column still places a breakpoint.
    const MARGIN: f32 = 2.;
    let x = position.x - bounds.left();
    x >= px((fold_x - MARGIN).max(0.)) && x <= px(fold_x + crate::element::FOLD_SIZE + MARGIN)
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
        if !self.options.breadcrumbs {
            return None;
        }
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
            row = row.child(
                div()
                    .id(("crumb", index))
                    .debug_selector(move || format!("crumb-{index}"))
                    .cursor_pointer()
                    .hover(|this| this.text_color(style.code))
                    .child(SharedString::from(item.name.clone()))
                    .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                        // The list filters with the keys, so the click makes sure that
                        // they reach this view.
                        window.focus(&this.focus_handle, cx);
                        this.open_crumb_menu(index, event.position(), cx);
                    })),
            );
        }
        Some(row)
    }

    /// The declarations of the level of the crumb at `level`: the members of the
    /// declaration above it, or the outermost declarations of the file.
    fn crumb_members(&self, level: usize) -> Vec<OutlineItem> {
        let around: Vec<(u32, std::ops::Range<usize>)> = self
            .enclosing()
            .into_iter()
            .map(|item| (item.depth, item.range.clone()))
            .collect();
        let (depth, parent) = match level.checked_sub(1).and_then(|at| around.get(at)) {
            Some((depth, range)) => (depth + 1, Some(range.clone())),
            None => (0, None),
        };
        self.outline
            .iter()
            .filter(|item| item.depth == depth)
            .filter(|item| {
                parent.as_ref().is_none_or(|parent| {
                    parent.start <= item.range.start && item.range.end <= parent.end
                })
            })
            .cloned()
            .collect()
    }

    /// The rows of the open crumb list after the typed filter, if the list is open.
    fn crumb_rows(&self) -> Vec<OutlineItem> {
        let Some(menu) = &self.crumb_menu else {
            return Vec::new();
        };
        let needle = menu.filter.text.to_lowercase();
        self.crumb_members(menu.level)
            .into_iter()
            .filter(|item| needle.is_empty() || item.name.to_lowercase().contains(&needle))
            .collect()
    }

    /// Opens the list of the neighbours of the crumb at `level` under the pointer. The
    /// current declaration of that level starts selected.
    fn open_crumb_menu(&mut self, level: usize, position: Point<Pixels>, cx: &mut Context<Self>) {
        let members = self.crumb_members(level);
        if members.is_empty() {
            return;
        }
        let current = self
            .enclosing()
            .get(level)
            .map(|item| item.name_range.start);
        let selected = current
            .and_then(|at| members.iter().position(|item| item.name_range.start == at))
            .unwrap_or(0);
        self.crumb_menu = Some(CrumbMenu {
            level,
            position,
            filter: TextField::default(),
            selected,
            scroll: ScrollHandle::new(),
        });
        cx.notify();
    }

    /// The row of the open crumb list that the pointer or the keys pick becomes the caret.
    fn open_crumb_row(&mut self, index: usize, cx: &mut Context<Self>) {
        let rows = self.crumb_rows();
        let Some(item) = rows.get(index) else {
            return;
        };
        let at = item.name_range.start;
        self.crumb_menu = None;
        self.go_to(Spot::Offset(at), cx);
    }

    /// The keys of the open crumb list: Escape closes it, the arrows move the selection,
    /// Enter goes to the row and typing narrows the list. Returns whether the key
    /// belonged to the list.
    fn crumb_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        if self.crumb_menu.is_none() {
            return false;
        }
        let stroke = &event.keystroke;
        if stroke.modifiers.control || stroke.modifiers.alt {
            return false;
        }
        match stroke.key.as_str() {
            "escape" => self.crumb_menu = None,
            "enter" => {
                let selected = self.crumb_menu.as_ref().map_or(0, |menu| menu.selected);
                self.open_crumb_row(selected, cx);
                cx.stop_propagation();
                return true;
            }
            "up" | "down" => {
                let count = self.crumb_rows().len();
                if let Some(menu) = &mut self.crumb_menu
                    && count > 0
                {
                    let step = if stroke.key == "up" { -1 } else { 1 };
                    menu.selected =
                        (menu.selected as isize + step).clamp(0, count as isize - 1) as usize;
                }
            }
            _ => {
                let Some(menu) = &mut self.crumb_menu else {
                    return false;
                };
                if !menu.filter.key_with_clipboard(event, false, cx) {
                    return false;
                }
                menu.selected = 0;
            }
        }
        if let Some(menu) = &mut self.crumb_menu {
            menu.scroll.scroll_to_item(menu.selected);
        }
        cx.notify();
        true
    }

    /// The list of the open crumb: its filter and the neighbours it found.
    fn render_crumb_menu(
        &self,
        menu: &CrumbMenu,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let rows: Vec<AnyElement> = self
            .crumb_rows()
            .into_iter()
            .enumerate()
            .map(|(index, item)| {
                let selected = index == menu.selected;
                let name = item.name.clone();
                let detail = item
                    .signature
                    .as_deref()
                    .map(|signature| {
                        signature
                            .strip_prefix(item.name.as_str())
                            .unwrap_or(signature)
                            .to_owned()
                    })
                    .unwrap_or_default();
                let icon = crumb_icon(item.kind);
                let hover = theme.selection_hover;
                div()
                    .id(("crumb-row", index))
                    .debug_selector(move || format!("crumb-row-{index}"))
                    .flex()
                    .items_center()
                    .gap(px(SPACE_S))
                    .flex_none()
                    .h(px(ROW_HEIGHT))
                    .px(px(ROW_PADDING_X))
                    .rounded(px(ROW_RADIUS))
                    .when(selected, |this| this.bg(theme.selection))
                    .when(!selected, |this| this.hover(move |style| style.bg(hover)))
                    .on_click(cx.listener(move |this, _, _, cx| this.open_crumb_row(index, cx)))
                    .child(icon_sized(icon, 14., theme))
                    .child(name)
                    .when(!detail.is_empty(), |this| {
                        this.child(div().text_color(theme.text_secondary).child(detail))
                    })
                    .into_any_element()
            })
            .collect();
        let filter = div()
            .flex()
            .items_center()
            .flex_none()
            .gap(px(SPACE_XS))
            .h(px(ROW_HEIGHT))
            .px(px(ROW_PADDING_X))
            .border_b_1()
            .border_color(theme.border)
            .child(icon_sized("general/filter", 14., theme))
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .when(menu.filter.text.is_empty(), |this| {
                        this.text_color(theme.text_secondary).child("Filter")
                    })
                    .children(text_field_children(
                        FieldMouse::new(cx, |editor: &mut Self| {
                            editor.crumb_menu.as_mut().map(|menu| &mut menu.filter)
                        }),
                        &menu.filter,
                        theme.text,
                        theme.text,
                        theme.selection,
                        theme.caret,
                        14.,
                        true,
                    )),
            );
        let list = scrollable(
            "crumb-list",
            &menu.scroll,
            div()
                .id("crumb-list")
                .flex()
                .flex_col()
                .overflow_y_scroll()
                .track_scroll(&menu.scroll)
                .children(rows),
            theme,
            window,
            cx,
        );
        let body = surface(theme)
            .id("crumb-menu")
            .debug_selector(|| "crumb-menu".to_owned())
            .occlude()
            .min_w(px(240.))
            .max_w(px(420.))
            .max_h(px(320.))
            .py(px(SPACE_XS))
            .text_size(px(UI_SIZE))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                if this.crumb_menu.take().is_some() {
                    cx.notify();
                }
            }))
            .child(filter)
            .child(list);
        deferred(
            anchored()
                .position(menu.position)
                .snap_to_window_with_margin(px(SPACE_M))
                .child(body),
        )
        .with_priority(1)
        .into_any_element()
    }

    /// The rows of the open crumb list, for a test.
    #[cfg(any(test, feature = "test-support"))]
    pub fn crumb_menu_rows(&self) -> Vec<String> {
        self.crumb_rows()
            .into_iter()
            .map(|item| item.name)
            .collect()
    }

    /// The row of the open crumb list that the keys act on, for a test.
    #[cfg(any(test, feature = "test-support"))]
    pub fn crumb_menu_selected(&self) -> Option<String> {
        let menu = self.crumb_menu.as_ref()?;
        self.crumb_rows()
            .get(menu.selected)
            .map(|item| item.name.clone())
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
        let x = layout.bounds.left() + px(self.gutter_room() + TEXT_INSET + self.center_inset)
            - px(self.scroll_left)
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
        let x = layout.bounds.left() + px(self.gutter_room() + TEXT_INSET + self.center_inset)
            - px(self.scroll_left)
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
            popups.extend(self.render_region_popup(popup, &theme, cx));
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
        if let Some((text, position)) = &self.place_hint {
            let body = surface(&theme)
                .debug_selector(|| "editor-place-hint".to_owned())
                .max_w(px(420.))
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
        if let Some((text, position)) = &self.coverage_hint {
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
                .debug_selector(|| "editor-coverage-hint".to_owned())
                .max_w(px(420.))
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
        if let Some(menu) = &self.crumb_menu {
            popups.push(self.render_crumb_menu(menu, &theme, window, cx));
        }
        let Some(lsp) = &self.lsp else {
            return popups;
        };
        if let Some(doc) = &lsp.quick {
            popups.push(self.render_quick_doc(doc, window, cx));
        }
        if let Some(completion) = &lsp.completion
            && let Some(height) = self.completion_height()
            && let Some((position, anchor)) = self.popup_position(completion.start, height)
        {
            let rows =
                completion.window().map(|row| {
                    let parts = completion.label(row);
                    let kind = completion.kind(row);
                    let selected = row == completion.selected;
                    let mut highlights: Vec<(std::ops::Range<usize>, HighlightStyle)> = Vec::new();
                    if parts.deprecated {
                        highlights.push((
                            0..parts.left.len(),
                            HighlightStyle {
                                strikethrough: Some(StrikethroughStyle {
                                    thickness: px(1.),
                                    color: None,
                                }),
                                ..HighlightStyle::default()
                            },
                        ));
                    }
                    // The letters the typed prefix matches, as the list itself matched
                    // them: a run of the name, its humps or the letters of a typo.
                    for range in completion.highlights(row, &parts.left) {
                        highlights.push((
                            range,
                            HighlightStyle {
                                color: Some(theme.accent),
                                ..HighlightStyle::default()
                            },
                        ));
                    }
                    let left = if highlights.is_empty() {
                        parts.left.into_any_element()
                    } else {
                        StyledText::new(parts.left)
                            .with_highlights(highlights)
                            .into_any_element()
                    };
                    div()
                        .id(("completion-row", row))
                        .debug_selector(move || format!("completion-row-{row}"))
                        .flex()
                        .items_center()
                        .gap_3()
                        .px_2()
                        .when(selected, |row| row.bg(theme.selection))
                        .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                            this.completion_click(row, event.click_count(), cx);
                        }))
                        .child(icon_sized(completion_kind_icon(kind), 14., &theme))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_color(theme.text)
                                .child(left),
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
            let documentation = lsp
                .completion_doc
                .as_ref()
                .filter(|doc| completion.shown.get(completion.selected) == Some(&doc.entry))
                .map(|doc| {
                    let font = self.style.font_family.clone();
                    let rows =
                        completion_doc_rows(doc)
                            .into_iter()
                            .enumerate()
                            .map(|(index, row)| {
                                let font = font.clone();
                                let code = row.code;
                                let runs = row.runs.into_iter().map(|run| {
                                    div()
                                        .min_w_0()
                                        .child(run.text)
                                        .when(run.code, |this| {
                                            this.font_family(font.clone())
                                                .bg(theme.pressed)
                                                .px(px(2.))
                                                .rounded(px(2.))
                                        })
                                        .when(run.strong, |this| {
                                            this.font_weight(FontWeight::MEDIUM)
                                        })
                                        .when(run.emphasis, |this| this.italic())
                                });
                                div()
                                    .debug_selector(move || format!("completion-doc-row-{index}"))
                                    .flex()
                                    .flex_wrap()
                                    .items_center()
                                    .gap(px(2.))
                                    .overflow_hidden()
                                    .when(code, |this| this.text_color(theme.text))
                                    .pl(px(row.indent as f32 * SPACE_S))
                                    .children(runs)
                            });
                    div()
                        .debug_selector(|| "completion-doc".to_owned())
                        .flex()
                        .flex_col()
                        .mx_2()
                        .mt_1()
                        .pt_1()
                        .border_t_1()
                        .border_color(theme.border)
                        .text_size(px(self.style.font_size))
                        .font_family(self.style.font_family.clone())
                        .text_color(theme.text_muted)
                        .children(rows)
                });
            let row_height = self.style.line_height.max(CODE_LINE_HEIGHT);
            let list = surface(&theme)
                .id("completion-list")
                // A click on a row belongs to the list: the text under the popup must
                // not take it and close the list, as a click in the editor does.
                .occlude()
                .debug_selector(|| "completion-popup".to_owned())
                .min_w(px(280.))
                .max_w(px(640.))
                .py_1()
                .font_family(self.style.font_family.clone())
                .text_size(px(self.style.font_size))
                .on_scroll(cx.listener(move |this, event: &ScrollEvent, _, cx| {
                    // The wheel delta points against the content, as it does over the
                    // text: a notch down has a negative `y` and shows the rows below.
                    let delta = event.delta.pixel_delta(px(row_height));
                    let mut rows = (-f32::from(delta.y) / row_height).round();
                    // A device that reports pixels may send less than a row: the list
                    // still moves one row, so it never looks stuck.
                    if rows == 0. && delta.y != px(0.) {
                        rows = if delta.y < px(0.) { 1. } else { -1. };
                    }
                    if rows != 0. {
                        this.completion_scroll(rows as isize, cx);
                    }
                }))
                .children(rows)
                .children(documentation);
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
        if let Some(list) = &lsp.navigation_list
            && let Some((position, anchor)) = self.popup_position(
                list.at,
                (list.targets.len() + 1) as f32 * self.style.line_height + 8.,
            )
        {
            let rows = list.targets.iter().enumerate().map(|(row, target)| {
                let selected = row == list.selected;
                div()
                    .debug_selector(move || format!("navigation-row-{row}"))
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_2()
                    .when(selected, |row| row.bg(theme.selection))
                    .text_color(theme.text)
                    .child(location_label(target))
            });
            let body = surface(&theme)
                .debug_selector(|| "navigation-popup".to_owned())
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
                        .child(body),
                )
                .with_priority(1)
                .into_any_element(),
            );
        }
        if let Some(signature) = &lsp.signatures
            && let Some((position, anchor)) =
                self.parameter_info_position(signature.at, 3. * self.style.line_height + 8.)
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
                (menu.window().len() + 1 + usize::from(menu.title.is_some())) as f32
                    * self.style.line_height
                    + 8.,
            )
        {
            let mut rows: Vec<Div> = Vec::new();
            if let Some(title) = menu.title {
                rows.push(
                    div()
                        .debug_selector(|| "actions-title".to_owned())
                        .px_2()
                        .text_color(theme.text_muted)
                        .child(title),
                );
            }
            let sections = menu.has_sections();
            for row in menu.window() {
                let selected = row == menu.selected;
                let quick_fix = menu.quick_fix(row);
                let nested = menu.nested(row);
                if sections && row > 0 && quick_fix != menu.quick_fix(row - 1) {
                    rows.push(
                        div()
                            .debug_selector(|| "actions-separator".to_owned())
                            .mx_2()
                            .my_1()
                            .h(px(1.))
                            .bg(theme.border),
                    );
                }
                let mut item = div()
                    .debug_selector({
                        let selector = if quick_fix {
                            format!("actions-quick-fix-{row}")
                        } else {
                            format!("actions-intention-{row}")
                        };
                        move || selector.clone()
                    })
                    .px_2()
                    .flex()
                    .items_center()
                    .gap(px(SPACE_S))
                    .when(selected, |row| row.bg(theme.selection))
                    .when(nested, |row| row.pl(px(SPACE_L)))
                    .text_color(if menu.rows[row].is_waiting() {
                        theme.text_muted
                    } else {
                        theme.text
                    });
                if sections {
                    item = item.child(icon_sized(
                        if quick_fix {
                            "codeInsight/quickFix"
                        } else {
                            "codeInsight/intention"
                        },
                        14.,
                        &theme,
                    ));
                }
                rows.push(
                    item.child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .truncate()
                            .child(menu.rows[row].label().to_owned()),
                    )
                    .children(menu.rows[row].hint().map(|hint| {
                        div()
                            .flex_none()
                            .text_color(theme.text_muted)
                            .child(hint.to_owned())
                    })),
                );
            }
            let list = surface(&theme)
                .debug_selector(|| "actions-popup".to_owned())
                .min_w(px(320.))
                .max_w(px(640.))
                .py_1()
                .text_size(px(self.style.font_size))
                .children(rows);
            let preview = menu
                .has_sections()
                .then(|| self.action_preview(menu))
                .flatten()
                .map(|preview| {
                    let lines = preview.lines.into_iter().map(|line| {
                        let color = if line.starts_with('+') {
                            theme.success
                        } else if line.starts_with('-') {
                            theme.error
                        } else {
                            theme.text
                        };
                        div().whitespace_nowrap().text_color(color).child(line)
                    });
                    surface(&theme)
                        .debug_selector(|| "actions-preview".to_owned())
                        .w(px(380.))
                        .max_h(px(420.))
                        .overflow_hidden()
                        .p_2()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .text_size(px(self.style.font_size))
                        .child(
                            div()
                                .text_color(theme.text_muted)
                                .truncate()
                                .child(preview.title),
                        )
                        .children(lines)
                });
            let menu_view = div()
                .flex()
                .items_start()
                .gap(px(SPACE_S))
                .child(list)
                .children(preview);
            popups.push(
                deferred(
                    anchored()
                        .anchor(anchor)
                        .position(position)
                        .snap_to_window_with_margin(px(SPACE_M))
                        .child(menu_view),
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
            && let Some((position, anchor)) = self.popup_position(
                field.at,
                (if field.summary.is_some() { 3. } else { 2. }) * (self.style.line_height + 4.)
                    + 12.,
            )
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
                        .anchor(anchor)
                        .position(position)
                        .snap_to_window_with_margin(px(SPACE_M))
                        .child(body),
                )
                .with_priority(1)
                .into_any_element(),
            );
        }
        if let Some(rename) = &lsp.rename_in_place
            && let Some((position, anchor)) =
                self.popup_position(rename.main_start(), self.style.line_height + 2. * SPACE_S)
        {
            let hint = rename.problem.unwrap_or(if rename.waiting {
                WAITING_FOR_LANGUAGE
            } else {
                "Enter to rename, Escape to cancel · Shift+F6 for more options"
            });
            let body = surface(&theme)
                .debug_selector(|| "rename-in-place-hint".to_owned())
                .max_w(px(720.))
                .p_2()
                .text_size(px(self.style.font_size))
                .text_color(if rename.problem.is_some() {
                    theme.error
                } else {
                    theme.text_muted
                })
                .child(hint);
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
        if let Some(field) = &lsp.name
            && let Some(position) = self.below(field.at)
        {
            let body = surface(&theme)
                .debug_selector(|| "extract-name-popup".to_owned())
                .min_w(px(280.))
                .max_w(px(640.))
                .p_2()
                .flex()
                .flex_col()
                .gap_1()
                .text_size(px(self.style.font_size))
                .child(div().text_color(theme.text_muted).child(field.label))
                .child(
                    div()
                        .font_family(self.style.font_family.clone())
                        .text_color(theme.text)
                        .child(format!("{}|", field.text)),
                )
                .children(field.visibility_line().map(|visibility| {
                    div()
                        .debug_selector(|| "extract-name-visibility".to_owned())
                        .text_color(theme.text_muted)
                        .child(format!("Visibility: {visibility} (Up/Down)"))
                }))
                .child(
                    div()
                        .debug_selector(|| "extract-name-preview".to_owned())
                        .text_color(theme.text_muted)
                        .child(field.preview()),
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
            let mut group = 0usize;
            for (row, index) in popup.shown.iter().enumerate() {
                let method = &popup.methods[*index];
                if owner != Some(method.owner.as_str()) {
                    owner = Some(method.owner.as_str());
                    let (heading, package) = crate::hierarchy::owner_heading(&method.owner);
                    let mut text = heading;
                    let mut highlights = Vec::new();
                    if let Some(package) = package {
                        text.push_str("  ");
                        let start = text.len();
                        text.push_str(&package);
                        highlights.push((
                            start..text.len(),
                            HighlightStyle {
                                color: Some(theme.text_muted),
                                ..HighlightStyle::default()
                            },
                        ));
                    }
                    rows.push(
                        div()
                            .px_2()
                            .pt_1()
                            .debug_selector(move || format!("override-owner-{group}"))
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme.text_secondary)
                                    .child(StyledText::new(text).with_highlights(highlights)),
                            )
                            .into_any_element(),
                    );
                    group += 1;
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
        // A list of the editor belongs to the editor that holds the focus: a click into
        // another view closes it, as IDEA does. The pointer popup stays, the pointer
        // never took the focus.
        if self.list_popups_open() && !self.focus_handle.is_focused(window) {
            self.dismiss_list_popups(cx);
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
        let runs = self.runs_on_enter(cx);
        let root = div()
            .id("editor-view")
            .key_context("Editor")
            .when(runs, |root| root.key_context("RunnableFile"))
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
            .on_action(cx.listener(|_, _: &StopRunning, _, cx| {
                cx.emit(EditorEvent::StopRequest);
                cx.stop_propagation();
            }))
            .on_key_down(cx.listener(Self::on_key));
        match &self.failed {
            Some(message) => root.p_4().child(message.clone()),
            None => {
                let bar = self.render_bar(window, cx);
                let toolbar = self.preview_toolbar(cx);
                let bundle_toolbar = self.bundle_toolbar(cx);
                let bundle = self.bundle_area(window, cx);
                let crumbs = self.render_crumbs(cx);
                let widget = self.render_problem_widget(cx);
                let file_bar = self.file_bar(window, cx);
                let text = match bundle {
                    Some(bundle) => bundle,
                    None => {
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
                        self.preview_area(text.into_any_element(), window, cx)
                    }
                };
                root.flex()
                    .flex_col()
                    .children(bar)
                    .children(toolbar)
                    .children(bundle_toolbar)
                    .children(file_bar)
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
            visibility: None,
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
