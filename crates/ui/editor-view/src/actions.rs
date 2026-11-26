//! Edits that a language server computes: Ctrl+Alt+L, Ctrl+Alt+O, Alt+Enter, and the
//! occurrences of the symbol under the caret.

use std::collections::BTreeMap;
use std::ops::Range;
use std::time::Duration;

use lang_api::{
    ActionEntry, ActionKind, ActionQuery, ActionResolveQuery, Confidence, DocumentEdit,
    DocumentSpot, GenerateKind, Replies, SourceEdit, SourceSpan, SuppressionQuery,
    SuppressionTarget, TextPoint, TextSpan, UnwrapEdit, WorkspaceChange, ask,
};
use text_core::{Edit, RopeStore, TextStore, editing};
use ui_kit::*;

use crate::declaration::Declaration;
use crate::semantic::{NO_ANSWER, first, offset_at, point_at, uri_path};
use crate::view::{AiProblem, EditorEvent, EditorView};

/// The caret rests this long before its occurrences are asked for.
const HIGHLIGHT_DELAY: Duration = Duration::from_millis(250);

/// The kind of the source action that Code → Sort Members runs.
pub(crate) const SORT_MEMBERS_KIND: &str = "source.sortMembers";

/// The kind of the source action that Code → Add final Modifiers runs. It changes the
/// modifiers of the members, so it is a row of Code and not of Alt+Insert.
pub(crate) const FINAL_MODIFIERS_KIND: &str = "source.generate.finalModifiers";

/// A refactoring the editor runs itself, by the same code as its key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Own {
    Rename,
    ChangeSignature,
    Move,
    Copy,
    SafeDelete,
    ExtractMethod,
    ExtractVariable,
    ExtractField,
    ExtractConstant,
    IntroduceParameter,
    PullMembersUp,
    PushMembersDown,
    ExtractSuperclass,
    IntroduceParameterObject,
    Inline,
}

/// A group of the Alt+Enter menu: the quick fixes come first, then the intentions.
/// The kind comes from the language server's code action kind or the kind field of the
/// language, never from the title of the action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MenuSection {
    QuickFix,
    Intention,
}

/// The section of an action the language gave.
fn section_of(entry: &ActionEntry) -> MenuSection {
    match entry.kind {
        Some(ActionKind::QuickFix) => MenuSection::QuickFix,
        Some(_) => MenuSection::Intention,
        None => {
            let code = entry.code.as_deref().unwrap_or_default();
            if code == "quickfix" || code.starts_with("quickfix.") {
                MenuSection::QuickFix
            } else {
                MenuSection::Intention
            }
        }
    }
}

/// A row of a pick list: an action to apply or a place to go to.
pub(crate) enum Pick {
    Action(ActionEntry),
    /// A change of signature opens a form of the window, so the pick only starts it.
    ChangeSignature {
        label: String,
    },
    /// A refactoring of the editor, with the key that runs it from the text.
    Refactor {
        label: String,
        hint: String,
        own: Own,
    },
    /// One expression the server offers under the caret for an extract.
    Expression {
        label: String,
        span: TextSpan,
        refactoring: crate::refactor::Refactoring,
    },
    /// An action of the server that asks for values in a form before it runs.
    Form {
        label: String,
        handle: String,
    },
    Place {
        label: String,
        target: SourceSpan,
    },
    /// A row of Source Generate; most need a list of parts before any edit.
    Generate {
        label: String,
        kind: GenerateKind,
    },
    /// A row of Unwrap/Remove, with the edit it applies.
    Unwrap {
        label: String,
        edit: UnwrapEdit,
    },
    /// A suppression of a check, with the edit it writes.
    Suppress {
        label: String,
        edit: DocumentEdit,
    },
    /// Fix all the problems of one check in the file, with the same quick fix at every
    /// place of the check.
    FixAllInFile {
        label: String,
        code: String,
    },
    /// A row that turns a check off in the settings of the project.
    DisableInspection {
        label: String,
        id: String,
        title: String,
    },
    /// A row that opens the settings of a check.
    EditInspection {
        label: String,
        id: String,
    },
    /// Create Test: the window shows its form for this place.
    CreateTest {
        spot: DocumentSpot,
    },
    /// Fix with AI: the window sends the diagnostic under the caret to the agent.
    FixWithAi {
        problem: AiProblem,
    },
    /// Add Inline Watch: the expression of the caret and its line, from zero.
    InlineWatch {
        line: usize,
        expression: String,
    },
    /// A correction of a misspelled word. A word of a declaration renames the symbol.
    Spelling {
        label: String,
        range: Range<usize>,
        replacement: String,
        declaration: bool,
    },
    /// Alt+Enter saved a misspelled word to a dictionary.
    SaveSpelling {
        label: String,
        word: String,
        project: bool,
    },
    /// A line that says more actions come once the language server has imported the
    /// project. It is greyed out, and the selection never rests on it.
    Waiting {
        label: String,
    },
}

impl Pick {
    pub fn label(&self) -> &str {
        match self {
            Self::Action(entry) => &entry.title,
            Self::ChangeSignature { label }
            | Self::Refactor { label, .. }
            | Self::Expression { label, .. }
            | Self::Form { label, .. }
            | Self::Place { label, .. }
            | Self::Generate { label, .. }
            | Self::Unwrap { label, .. }
            | Self::Suppress { label, .. }
            | Self::FixAllInFile { label, .. }
            | Self::DisableInspection { label, .. }
            | Self::EditInspection { label, .. }
            | Self::Spelling { label, .. }
            | Self::SaveSpelling { label, .. }
            | Self::Waiting { label } => label,
            Self::CreateTest { .. } => "Create Test",
            Self::InlineWatch { .. } => "Add Inline Watch",
            Self::FixWithAi { .. } => "Fix with AI",
        }
    }

    /// Whether the row is the greyed line of a list that waits for the server.
    pub fn is_waiting(&self) -> bool {
        matches!(self, Self::Waiting { .. })
    }

    /// The key beside the row, as the menus write it.
    pub fn hint(&self) -> Option<&str> {
        match self {
            Self::Refactor { hint, .. } if !hint.is_empty() => Some(hint),
            _ => None,
        }
    }

    /// Whether the selected row marks the text in the editor: the expression or the
    /// construct it would take, or the places an extract would replace.
    pub fn marks_text(&self) -> bool {
        match self {
            Self::Expression { .. } | Self::Unwrap { .. } => true,
            Self::Action(entry) => entry
                .code
                .as_deref()
                .is_some_and(|code| code.starts_with("refactor.extract.variable")),
            _ => false,
        }
    }

    /// The section a row belongs to. A correction of a misspelled word is a quick fix;
    /// everything the editor adds itself, such as Create Test, is an intention.
    fn section(&self) -> MenuSection {
        match self {
            Self::Action(entry) => section_of(entry),
            // The fix of a whole file belongs beside the quick fixes of the places.
            Self::Spelling { .. } | Self::FixAllInFile { .. } => MenuSection::QuickFix,
            _ => MenuSection::Intention,
        }
    }
}

/// Alt+Enter, Generate, usages, the type hierarchy: rows picked with the keys of
/// completion.
pub(crate) struct ActionMenu {
    pub at: usize,
    pub rows: Vec<Pick>,
    /// The section of every row, of the same length as `rows`. Empty for the flat
    /// lists that have no sections.
    pub sections: Vec<MenuSection>,
    /// The group of every row, of the same length as `rows`, for a list whose rows fall into
    /// groups such as Refactor This. Empty for the lists without groups.
    pub groups: Vec<usize>,
    pub selected: usize,
    /// The question for the preview of the selected row, while the language computes
    /// the edit on resolve. Dropping it cancels the question.
    pub preview_task: Option<Task<()>>,
    /// The wait for the server's actions while the server imports. Dropping the list ends
    /// the wait, so a list that closed never takes the actions of another place.
    pub waiting_task: Option<Task<()>>,
    /// The question for the declaration at the caret, which the inline rows of the list wait
    /// for. Dropping the list cancels it, so a closed list never takes the rows.
    pub declaration_task: Option<Task<()>>,
    /// The heading of the list, for the menus that have one, such as Generate.
    pub title: Option<&'static str>,
}

impl ActionMenu {
    pub fn window(&self) -> Range<usize> {
        let start = self.selected.saturating_sub(crate::semantic::MAX_SHOWN - 1);
        start..(start + crate::semantic::MAX_SHOWN).min(self.rows.len())
    }

    /// Whether the selection may rest on the row: the greyed line of a waiting list may not.
    pub fn selectable(&self, row: usize) -> bool {
        self.rows.get(row).is_some_and(|pick| !pick.is_waiting())
    }

    /// The next selectable row after `from`, moving down or up and wrapping around. `from`
    /// stays when no other row can be selected.
    pub fn step(&self, from: usize, down: bool) -> usize {
        let count = self.rows.len();
        let mut at = from;
        for _ in 0..count {
            at = if down {
                (at + 1) % count
            } else {
                (at + count - 1) % count
            };
            if self.selectable(at) {
                return at;
            }
        }
        from
    }

    /// Whether the row is a quick fix, which carries the red bulb and comes first.
    pub fn quick_fix(&self, row: usize) -> bool {
        self.sections.get(row) == Some(&MenuSection::QuickFix)
    }

    /// Whether a separator goes above the row: the quick fixes of Alt+Enter end there, or
    /// the groups of the list meet there. A group with no row in the list makes no separator.
    pub fn separated(&self, row: usize) -> bool {
        if row == 0 {
            return false;
        }
        let quick_fixes = self.has_sections() && self.quick_fix(row) != self.quick_fix(row - 1);
        let groups = self.groups.get(row) != self.groups.get(row - 1);
        quick_fixes || groups
    }

    /// Whether the row is an item under another one, such as the fix of a whole file
    /// under the fix of one place. The list indents it.
    pub fn nested(&self, row: usize) -> bool {
        matches!(self.rows.get(row), Some(Pick::FixAllInFile { .. }))
    }

    /// Whether the list is the menu of Alt+Enter with its sections and its preview.
    /// The flat lists of Generate and the hierarchies keep one look.
    pub fn has_sections(&self) -> bool {
        !self.sections.is_empty()
    }
}

/// The change the selected row would make, shown beside the list when the language
/// gave the edits without applying them.
pub(crate) struct ActionPreview {
    pub title: String,
    pub lines: Vec<String>,
}

/// Edits of one server answer, all in the coordinates of `store`, in the order the
/// buffer needs. LSP does not sort them; inserts at one place keep their order, since
/// that is the order of their text. Overlapping edits are forbidden by LSP but sent by
/// real servers: `None`, nothing of them applies.
pub(crate) fn server_edits(store: &RopeStore, edits: Vec<DocumentEdit>) -> Option<Vec<Edit>> {
    let mut edits: Vec<(Range<usize>, String)> = edits
        .into_iter()
        .map(|edit| {
            let start = offset_at(store, edit.span.start);
            let end = offset_at(store, edit.span.end).max(start);
            (start..end, edit.text.replace("\r\n", "\n"))
        })
        .collect();
    edits.sort_by_key(|(range, _)| (range.start, range.end));
    // Two identical places in one answer are one replacement, not an overlap.
    edits.dedup_by(|later, earlier| later.0 == earlier.0 && later.1 == earlier.1);
    let overlap = edits.windows(2).any(|pair| pair[1].0.start < pair[0].0.end);
    if overlap {
        return None;
    }
    let edits = edits
        .into_iter()
        .map(|(range, text)| Edit::new(range, text.as_str()));
    Some(edits.collect())
}

const OVERLAPPING: &str = "The language server sent overlapping edits, nothing was changed";

/// Alt+Enter with nothing to offer after the filter, as IDEA says it.
const NO_ACTIONS: &str = "No intentions available";
/// The greyed last line of Alt+Enter while the server imports: more actions come later.
const IMPORTING_ACTIONS: &str = "More actions after the project is imported";

/// Fix all found no fix it could apply to every place of the check.
const NO_FIX_ALL: &str = "The language gave no fix for every place of the check";

/// The byte range a span of the text covers.
fn expression_range(store: &RopeStore, span: TextSpan) -> Range<usize> {
    let start = offset_at(store, span.start);
    start..offset_at(store, span.end).max(start)
}

/// Whole-word occurrences of `needle` in `text`, in text order.
pub(crate) fn word_ranges(text: &str, needle: &str) -> Vec<Range<usize>> {
    let is_word =
        |character: char| character.is_alphanumeric() || character == '_' || character == '$';
    let mut ranges = Vec::new();
    if needle.is_empty() {
        return ranges;
    }
    let mut from = 0;
    while let Some(found) = text[from..].find(needle) {
        let start = from + found;
        let end = start + needle.len();
        let clean = !text[..start].chars().next_back().is_some_and(is_word)
            && !text[end..].chars().next().is_some_and(is_word);
        if clean {
            ranges.push(start..end);
        }
        from = end;
    }
    ranges
}

/// Edits of every `TextDocumentEdit` of one answer, each in the coordinates of the text
/// after the one before it. `None` when any of them overlaps, so nothing applies.
fn server_batches(
    store: &RopeStore,
    sources: impl IntoIterator<Item = Vec<DocumentEdit>>,
) -> Option<Vec<Vec<Edit>>> {
    let mut probe = store.clone();
    let mut format = text_core::FileFormat::default();
    let mut batches = Vec::new();
    for edits in sources {
        if edits.is_empty() {
            continue;
        }
        let edits = server_edits(&probe, edits)?;
        text_core::apply_edits(&mut probe, &mut format, &edits);
        batches.push(edits);
    }
    Some(batches)
}

fn whole(store: &RopeStore) -> TextSpan {
    TextSpan {
        start: TextPoint::default(),
        end: point_at(store, store.len()),
    }
}

impl EditorView {
    pub(crate) fn asked_version(&self) -> Option<(String, i32, usize)> {
        let (lsp, state) = (self.lsp.as_ref()?, self.state.as_ref()?);
        let caret = state.selections().primary().head;
        Some((lsp.uri.clone(), state.version() as i32, caret))
    }

    /// Edits in the coordinates of this document as it is now, as one undo step.
    pub fn apply_document_edits(&mut self, edits: Vec<DocumentEdit>, cx: &mut Context<Self>) {
        self.apply_document_batches(vec![edits], cx);
    }

    /// Several `TextDocumentEdit`s of this file from one answer. Each works on the text
    /// after the one before it and all undo in one step.
    pub fn apply_document_batches(
        &mut self,
        batches: Vec<Vec<DocumentEdit>>,
        cx: &mut Context<Self>,
    ) {
        self.catch_up(cx);
        let Some(state) = &mut self.state else {
            return;
        };
        let caret = state.selections().primary().head;
        let Some(batches) = server_batches(&state.store(), batches) else {
            self.show_note(caret, OVERLAPPING, cx);
            return;
        };
        let change = state.apply_edits_batches(batches);
        self.changed(change, cx);
    }

    /// Applies a change if the text is still the one asked about. A change of this
    /// document alone applies here; one that reaches other files or moves files goes
    /// to the window, which applies every part of it.
    ///
    /// Each `TextDocumentEdit` applies to the text after the one before it, as LSP
    /// says. All of them are checked first: if any one overlaps, nothing is applied.
    pub(crate) fn apply_change(
        &mut self,
        version: i32,
        change: WorkspaceChange,
        cx: &mut Context<Self>,
    ) {
        self.apply_change_labeled(version, change, None, cx);
    }

    /// Applies a change and gives the refactoring a name for Local History.
    pub(crate) fn apply_change_labeled(
        &mut self,
        version: i32,
        change: WorkspaceChange,
        label: Option<&'static str>,
        cx: &mut Context<Self>,
    ) {
        self.catch_up(cx);
        let (Some(lsp), Some(state)) = (&self.lsp, &self.state) else {
            return;
        };
        if state.version() as i32 != version || change.is_empty() {
            return;
        }
        let local = change.renames.is_empty()
            && change
                .edits
                .iter()
                .all(|source| source.uri == lsp.uri || source.edits.is_empty());
        if !local {
            cx.emit(EditorEvent::ApplyChange {
                change,
                label: label.map(str::to_owned),
            });
            return;
        }
        let caret = state.selections().primary().head;
        let sources = change.edits.into_iter().map(|source| source.edits);
        let Some(batches) = server_batches(&state.store(), sources) else {
            self.show_note(caret, OVERLAPPING, cx);
            return;
        };
        let Some(state) = &mut self.state else {
            return;
        };
        let change = state.apply_edits_batches(batches);
        self.changed(change, cx);
    }

    fn apply_entry(&mut self, version: i32, entry: ActionEntry, cx: &mut Context<Self>) {
        self.apply_entry_labeled(version, entry, None, None, cx);
    }

    /// Applies the entry, or first opens the field of the name it introduces. `asked` is the
    /// expression the action was asked about, when the editor knows it.
    pub(crate) fn apply_entry_labeled(
        &mut self,
        version: i32,
        entry: ActionEntry,
        label: Option<&'static str>,
        asked: Option<Range<usize>>,
        cx: &mut Context<Self>,
    ) {
        // The place the server marks is the name of the extract. An Introduce Parameter names
        // its parameter at the expression it replaced, where the server marks the place in most
        // answers; `start_extract_name` finds that place from the text when the server marks none.
        let introduces_a_name = entry.code.as_deref().is_some_and(|code| {
            code.starts_with("refactor.extract.variable")
                || code.starts_with("refactor.extract.constant")
                || code.starts_with("refactor.extract.field")
                || code.starts_with("refactor.extract.function")
        });
        let introduces_parameter = entry.code.as_deref().is_some_and(|code| {
            code.starts_with("refactor.introduce.parameter")
                && !code.starts_with("refactor.introduce.parameter.object")
        });
        if (entry.rename.is_some() && introduces_a_name) || introduces_parameter {
            self.start_extract_name(version, entry, label, asked, cx);
            return;
        }
        let change = WorkspaceChange {
            edits: entry.edits,
            renames: entry.renames,
        };
        self.apply_change_labeled(version, change, label, cx);
    }

    /// Applies a correction of a misspelled word. A piece of a declaration name
    /// renames the symbol, so every use follows; a word of prose is a plain edit.
    fn apply_correction(
        &mut self,
        version: i32,
        range: Range<usize>,
        replacement: String,
        declaration: bool,
        cx: &mut Context<Self>,
    ) {
        let (Some(lsp), Some(state)) = (&self.lsp, &self.state) else {
            return;
        };
        if state.version() as i32 != version {
            return;
        }
        if !declaration {
            let span = TextSpan {
                start: point_at(&state.store(), range.start),
                end: point_at(&state.store(), range.end),
            };
            self.apply_document_edits(
                vec![DocumentEdit {
                    span,
                    text: replacement,
                }],
                cx,
            );
            return;
        }
        let Some(provider) = lsp.support.rename() else {
            let span = TextSpan {
                start: point_at(&state.store(), range.start),
                end: point_at(&state.store(), range.end),
            };
            self.apply_document_edits(
                vec![DocumentEdit {
                    span,
                    text: replacement,
                }],
                cx,
            );
            return;
        };
        let text = state.text();
        let (start, end) = Self::identifier_around(&text, range.clone());
        let new_name = format!(
            "{}{}{}",
            &text[start..range.start],
            replacement,
            &text[range.end..end]
        );
        let Some(spot) = self.spot_at(start) else {
            return;
        };
        let replies = ask(provider, lang_api::RenameQuery { spot, new_name });
        cx.spawn(async move |this, cx| {
            let answer = first(replies).await;
            this.update(cx, |this, cx| {
                if let Some((change, _)) = answer {
                    this.apply_change(version, change, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// The bytes of the identifier around `range`, which is a piece of it.
    fn identifier_around(text: &str, range: Range<usize>) -> (usize, usize) {
        let is_name =
            |character: char| character.is_alphanumeric() || character == '_' || character == '$';
        let mut start = range.start.min(text.len());
        while start > 0 {
            let character = text[..start].chars().next_back().unwrap_or(' ');
            if !is_name(character) {
                break;
            }
            start -= character.len_utf8();
        }
        let mut end = range.end.min(text.len());
        while end < text.len() {
            let character = text[end..].chars().next().unwrap_or(' ');
            if !is_name(character) {
                break;
            }
            end += character.len_utf8();
        }
        (start, end)
    }

    /// Opens a pick list at the caret, or says `empty` when there is nothing.
    pub(crate) fn show_picks(
        &mut self,
        at: usize,
        rows: Vec<Pick>,
        empty: &str,
        cx: &mut Context<Self>,
    ) {
        if rows.is_empty() {
            self.show_note(at, empty.to_owned(), cx);
            return;
        }
        if let Some(lsp) = &mut self.lsp {
            lsp.completion = None;
            lsp.note = None;
            lsp.move_to = None;
            lsp.actions = Some(ActionMenu {
                at,
                rows,
                sections: Vec::new(),
                groups: Vec::new(),
                selected: 0,
                preview_task: None,
                waiting_task: None,
                declaration_task: None,
                title: None,
            });
            cx.notify();
        }
    }

    /// Ctrl+Alt+L: the whole file, through the flags of the Reformat File dialog.
    pub(crate) fn reformat(&mut self, cx: &mut Context<Self>) {
        cx.emit(EditorEvent::Reformat);
    }

    /// Ctrl+Shift+Enter: the language closes the statement at every caret from its tree;
    /// a caret it has no plan for finishes its line by the rules of the language alone.
    pub(crate) fn complete_statement(&mut self, cx: &mut Context<Self>) {
        let Some(state) = &self.state else {
            return;
        };
        if state.is_read_only()
            || state
                .language()
                .is_none_or(|language| language.statements.is_none())
        {
            return;
        }
        let mut evolving = state.text();
        let unit = state.indent_unit().to_owned();
        let carets: Vec<usize> = state.selections().iter().map(|caret| caret.head).collect();
        let syntax = self.lsp.as_ref().map(|lsp| lsp.support.syntax());
        let mut plans = Vec::with_capacity(carets.len());
        if let Some(syntax) = syntax {
            // The plans are read in the order of the carets, each from the text with the
            // plans before it already in: a statement that an earlier caret closed no
            // longer holds the brackets of this one.
            let mut shift = 0isize;
            let mut done_to = 0usize;
            for caret in carets {
                let at = (caret as isize + shift).max(0) as usize;
                let Some(edit) = syntax.complete_statement(&evolving, at, &unit) else {
                    plans.push(None);
                    continue;
                };
                let start = (edit.start as isize - shift).max(0) as usize;
                let end = (edit.end as isize - shift).max(0) as usize;
                // A caret whose line another caret already finished has no plan of its own.
                if start < done_to {
                    plans.push(None);
                    continue;
                }
                // A plan of a language that names bytes outside the text is left out.
                if evolving.get(edit.start..edit.end).is_none() {
                    plans.push(None);
                    continue;
                }
                done_to = end;
                shift += edit.text.len() as isize - (edit.end - edit.start) as isize;
                evolving.replace_range(edit.start..edit.end, &edit.text);
                plans.push(Some(editing::StatementPlan {
                    range: start..end,
                    select: edit.caret..edit.caret,
                    text: edit.text,
                }));
            }
        }
        let Some(state) = &mut self.state else {
            return;
        };
        let change = state.complete_statement(plans);
        self.changed(change, cx);
    }

    pub(crate) fn ask_actions(
        &mut self,
        span: TextSpan,
        only: Option<ActionKind>,
        cx: &mut Context<Self>,
        done: impl FnOnce(&mut Self, i32, usize, Vec<ActionEntry>, &mut Context<Self>) + 'static,
    ) {
        if let Some(task) = self.ask_actions_task(None, span, only, cx, done) {
            task.detach();
        }
    }

    /// The question of [`EditorView::ask_actions`] as a task. The wait `wait` keeps it, so
    /// that taking the wait back cancels the question at the language. An answer that comes
    /// after the wait was taken back is dropped; an answer that decides nothing ends the wait.
    pub(crate) fn ask_actions_task(
        &mut self,
        wait: Option<u64>,
        span: TextSpan,
        only: Option<ActionKind>,
        cx: &mut Context<Self>,
        done: impl FnOnce(&mut Self, i32, usize, Vec<ActionEntry>, &mut Context<Self>) + 'static,
    ) -> Option<Task<()>> {
        let (uri, version, caret) = self.asked_version()?;
        let provider = self.lsp.as_ref().and_then(|lsp| lsp.support.actions())?;
        let query = ActionQuery {
            uri,
            version,
            span,
            only,
        };
        let replies = ask(provider, query);
        Some(cx.spawn(async move |this, cx| {
            let answer = first(replies).await;
            this.update(cx, |this, cx| {
                if wait.is_some() {
                    this.follow_refactor_wait(cx);
                }
                if wait.is_some_and(|token| !this.refactor_waits(token)) {
                    return;
                }
                match answer {
                    Some((_, Confidence::Unknown)) => {
                        if let Some(token) = wait {
                            this.end_refactor_wait(token, cx);
                        }
                        this.show_note(caret, NO_ANSWER, cx)
                    }
                    Some((entries, _)) => done(this, version, caret, entries, cx),
                    None => {
                        if let Some(token) = wait {
                            this.end_refactor_wait(token, cx);
                        }
                    }
                }
            })
            .ok();
        }))
    }

    /// Ctrl+Alt+O.
    pub(crate) fn optimize_imports(&mut self, cx: &mut Context<Self>) {
        let Some(span) = self.state.as_ref().map(|state| whole(&state.store())) else {
            return;
        };
        let only = Some(ActionKind::OrganizeImports);
        self.ask_actions(span, only, cx, |this, version, _, entries, cx| {
            if let Some(entry) = entries.into_iter().next() {
                cx.emit(EditorEvent::BeforeOptimizeImports);
                this.apply_entry(version, entry, cx);
            }
        });
    }

    /// The selection, or the caret, as a span.
    pub(crate) fn selection_span(&self) -> Option<TextSpan> {
        let state = self.state.as_ref()?;
        let selection = state.selections().primary();
        Some(TextSpan {
            start: point_at(&state.store(), selection.start()),
            end: point_at(&state.store(), selection.end()),
        })
    }

    /// The error or warning under the caret, for Fix with AI.
    fn ai_problem_at(&self, caret: usize) -> Option<AiProblem> {
        if !self.options.ai_features {
            return None;
        }
        let (lsp, state) = (self.lsp.as_ref()?, self.state.as_ref()?);
        let mark = lsp
            .server_marks
            .iter()
            .chain(lsp.native_marks.values().flatten())
            .find(|mark| {
                mark.range.start <= caret
                    && caret <= mark.range.end
                    && matches!(
                        mark.severity,
                        lang_api::Severity::Error | lang_api::Severity::Warning
                    )
                    && mark.code.as_deref() != Some(lang_api::SPELLING_INSPECTION)
            })?;
        Some(AiProblem {
            path: uri_path(&lsp.uri)?,
            line: state.store().offset_to_line(mark.range.start),
            severity: mark.severity,
            message: mark.message.clone(),
            code: mark.code.clone(),
            text: state.text(),
        })
    }

    /// Whether the problems under the caret are warnings and no error. The bulb of a quick
    /// fix there is yellow; an error makes it red.
    pub(crate) fn warning_under(&self, caret: usize) -> bool {
        let Some(lsp) = self.lsp.as_ref() else {
            return false;
        };
        let mut warning = false;
        for mark in lsp
            .server_marks
            .iter()
            .chain(lsp.native_marks.values().flatten())
        {
            if mark.range.start <= caret && caret <= mark.range.end {
                match mark.severity {
                    lang_api::Severity::Error => return false,
                    lang_api::Severity::Warning => warning = true,
                    lang_api::Severity::Information | lang_api::Severity::Hint => {}
                }
            }
        }
        warning
    }

    /// Alt+Enter: fixes of the problem under the caret first, then the suppressions of
    /// its check and the other actions.
    pub(crate) fn show_actions(&mut self, cx: &mut Context<Self>) {
        self.before_actions_popup(cx);
        let Some(span) = self.selection_span() else {
            return;
        };
        let caret = self
            .state
            .as_ref()
            .map_or(0, |state| state.selections().primary().head);
        let mut own: Vec<(MenuSection, Pick)> = self
            .own_action_rows()
            .into_iter()
            .map(|row| (row.section(), row))
            .collect();
        own.extend(
            self.inspection_picks(caret)
                .into_iter()
                .map(|row| (row.section(), row)),
        );
        // Fix with AI is the last row of the menu, after every fix and intention.
        if let Some(problem) = self.ai_problem_at(caret) {
            own.push((MenuSection::Intention, Pick::FixWithAi { problem }));
        }
        let Some(provider) = self.lsp.as_ref().and_then(|lsp| lsp.support.actions()) else {
            if own.is_empty() {
                self.show_transient_note(caret, NO_ACTIONS, cx);
            } else {
                self.show_action_sections(caret, own, cx);
            }
            return;
        };
        // While the server imports, the editor's own rows show at once with a greyed line
        // that says more come later, and the server is not asked then.
        let ready = self.server_ready();
        if !ready && !own.is_empty() {
            own.push((
                MenuSection::Intention,
                Pick::Waiting {
                    label: IMPORTING_ACTIONS.to_owned(),
                },
            ));
            self.show_action_sections(caret, own, cx);
            self.wait_for_server_actions(span, caret, cx);
            return;
        }
        let Some((uri, version, _)) = self.asked_version() else {
            return;
        };
        let replies = ask(
            provider,
            ActionQuery {
                uri,
                version,
                span,
                only: None,
            },
        );
        if !ready {
            self.importing_note(caret, cx);
        }
        cx.spawn(async move |this, cx| {
            let answer = first(replies).await;
            this.update(cx, |this, cx| {
                let unknown = answer
                    .as_ref()
                    .is_none_or(|(_, confidence)| *confidence == Confidence::Unknown);
                let mut entries = answered_entries(answer);
                // Inline rows wait for the declaration at the caret; the list does not wait.
                let waiting = this.gate_inline(&mut entries);
                let mut rows = server_rows(entries);
                rows.extend(own);
                if rows.is_empty() && !waiting.is_empty() {
                    // The declaration decides the only rows there are, so the list waits for it.
                    let task = this.ask_declaration(cx, move |this, declaration, cx| {
                        let rows = if declaration == Declaration::Project {
                            server_rows(waiting)
                        } else {
                            Vec::new()
                        };
                        if rows.is_empty() {
                            if unknown {
                                this.show_unanswered(caret, true, cx);
                            } else {
                                this.show_transient_note(caret, NO_ACTIONS, cx);
                            }
                        } else {
                            this.show_action_sections(caret, rows, cx);
                        }
                    });
                    if let Some(task) = task {
                        task.detach();
                    }
                    return;
                }
                if rows.is_empty() {
                    if unknown {
                        this.show_unanswered(caret, true, cx);
                    } else {
                        this.show_transient_note(caret, NO_ACTIONS, cx);
                    }
                    return;
                }
                // The question goes out before the list shows; the list does not wait for it.
                let task = if waiting.is_empty() {
                    None
                } else {
                    let version = this
                        .state
                        .as_ref()
                        .map_or(0, |state| state.version() as i32);
                    this.ask_declaration(cx, move |this, declaration, cx| {
                        if declaration == Declaration::Project {
                            this.join_declared_rows(caret, version, server_rows(waiting), cx);
                        }
                    })
                };
                this.show_action_sections(caret, rows, cx);
                if let Some(task) = task {
                    this.keep_declaration_task(Some(task));
                }
            })
            .ok();
        })
        .detach();
    }

    /// Shows the sections of Alt+Enter: the quick fixes first, then the intentions,
    /// each row with the bulb of its section.
    fn show_action_sections(
        &mut self,
        at: usize,
        mut rows: Vec<(MenuSection, Pick)>,
        cx: &mut Context<Self>,
    ) {
        // The sort is stable: the order the language and the editor gave inside a
        // section stays as it is.
        rows.sort_by_key(|(section, _)| *section != MenuSection::QuickFix);
        let sections = rows.iter().map(|(section, _)| *section).collect();
        let rows = rows.into_iter().map(|(_, row)| row).collect();
        let Some(lsp) = &mut self.lsp else {
            return;
        };
        lsp.completion = None;
        lsp.note = None;
        lsp.move_to = None;
        lsp.actions = Some(ActionMenu {
            at,
            rows,
            sections,
            groups: Vec::new(),
            selected: 0,
            preview_task: None,
            waiting_task: None,
            declaration_task: None,
            title: None,
        });
        cx.notify();
        self.request_action_preview(cx);
    }

    /// While the server imports, the list of Alt+Enter waits for the server to be ready. Then
    /// it asks once for the actions of the same place, and they join the list in place of the
    /// greyed line. The wait belongs to the list: a list that closes ends the wait.
    fn wait_for_server_actions(&mut self, span: TextSpan, caret: usize, cx: &mut Context<Self>) {
        let task = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(Self::WAIT_TICK_MS))
                    .await;
                match this.update(cx, |this, _| this.server_ready()) {
                    Ok(true) => break,
                    Ok(false) => {}
                    Err(_) => return,
                }
            }
            let Ok(Some((version, replies))) =
                this.update(cx, |this, _| this.ask_server_actions(span))
            else {
                return;
            };
            let answer = first(replies).await;
            this.update(cx, |this, cx| {
                this.join_server_actions(caret, version, answer, cx);
            })
            .ok();
        });
        if let Some(lsp) = &mut self.lsp
            && let Some(menu) = &mut lsp.actions
        {
            menu.waiting_task = Some(task);
        }
    }

    /// The question for the actions of `span` at the text as it stands, with the version of
    /// the text it is asked on.
    fn ask_server_actions(&self, span: TextSpan) -> Option<(i32, Replies<Vec<ActionEntry>>)> {
        let (uri, version, _) = self.asked_version()?;
        let provider = self.lsp.as_ref()?.support.actions()?;
        let query = ActionQuery {
            uri,
            version,
            span,
            only: None,
        };
        Some((version, ask(provider, query)))
    }

    /// The actions of the language join the list that waited for them. The greyed line goes,
    /// and the selection stays on the row it was on. The list changes only while it is still
    /// the one that waited: the same place, and the text it was asked on.
    fn join_server_actions(
        &mut self,
        caret: usize,
        version: i32,
        answer: Option<(Vec<ActionEntry>, Confidence)>,
        cx: &mut Context<Self>,
    ) {
        let mut entries = answered_entries(answer);
        let waiting = self.gate_inline(&mut entries);
        if self
            .state
            .as_ref()
            .is_none_or(|state| state.version() as i32 != version)
        {
            return;
        }
        let Some(lsp) = &mut self.lsp else {
            return;
        };
        let Some(menu) = &mut lsp.actions else {
            return;
        };
        if menu.at != caret || !menu.rows.iter().any(Pick::is_waiting) {
            return;
        }
        let selected = menu.selected;
        // The rows are sorted by section again, as the list builds them: the sort is stable,
        // so the language's rows keep their order and come before the editor's rows.
        let mut rows: Vec<(MenuSection, bool, Pick)> = server_rows(entries)
            .into_iter()
            .map(|(section, pick)| (section, false, pick))
            .collect();
        rows.extend(
            std::mem::take(&mut menu.rows)
                .into_iter()
                .enumerate()
                .filter(|(_, pick)| !pick.is_waiting())
                .map(|(row, pick)| (pick.section(), row == selected, pick)),
        );
        rows.sort_by_key(|(section, _, _)| *section != MenuSection::QuickFix);
        menu.selected = rows.iter().position(|(_, chosen, _)| *chosen).unwrap_or(0);
        menu.sections = rows.iter().map(|(section, _, _)| *section).collect();
        menu.rows = rows.into_iter().map(|(_, _, pick)| pick).collect();
        menu.preview_task = None;
        cx.notify();
        self.request_action_preview(cx);
        if !waiting.is_empty() {
            let task = self.ask_declaration(cx, move |this, declaration, cx| {
                if declaration == Declaration::Project {
                    this.join_declared_rows(caret, version, server_rows(waiting), cx);
                }
            });
            self.keep_declaration_task(task);
        }
    }

    /// The change the selected row would make, in the current document, when the
    /// language gave its edits with the action. `None` when there is nothing to show.
    pub(crate) fn action_preview(&self, menu: &ActionMenu) -> Option<ActionPreview> {
        let Some(Pick::Action(entry)) = menu.rows.get(menu.selected) else {
            return None;
        };
        let state = self.state.as_ref()?;
        let uri = self.lsp.as_ref()?.uri.as_str();
        let source = entry.edits.iter().find(|edit| edit.uri == uri)?;
        if source.edits.is_empty() {
            return None;
        }
        let store = state.store();
        let mut lines = Vec::new();
        for edit in source.edits.iter().take(6) {
            let start = offset_at(&store, edit.span.start);
            let end = offset_at(&store, edit.span.end).max(start);
            let before = store.slice(start..end);
            for line in before.lines().take(4) {
                lines.push(format!("- {line}"));
            }
            let text = edit.text.replace("\r\n", "\n");
            for line in text.lines().take(8) {
                lines.push(format!("+ {line}"));
            }
            if lines.len() >= 16 {
                break;
            }
        }
        if lines.is_empty() {
            return None;
        }
        lines.truncate(16);
        Some(ActionPreview {
            title: entry.title.clone(),
            lines,
        })
    }

    /// Asks the language for the edit of the selected row when the row has none: a
    /// server that sends its code actions without edits fills them in on resolve. The
    /// list does not wait; the preview appears when the answer comes.
    fn request_action_preview(&mut self, cx: &mut Context<Self>) {
        let (Some(lsp), Some(state)) = (&self.lsp, &self.state) else {
            return;
        };
        let Some(menu) = &lsp.actions else {
            return;
        };
        let Some(Pick::Action(entry)) = menu.rows.get(menu.selected) else {
            return;
        };
        if !entry.edits.is_empty() || !entry.renames.is_empty() {
            return;
        }
        let Some(resolve) = entry.resolve.clone() else {
            return;
        };
        let Some(provider) = lsp.support.action_resolve() else {
            return;
        };
        let uri = lsp.uri.clone();
        let version = state.version() as i32;
        let title = entry.title.clone();
        let replies = ask(
            provider,
            ActionResolveQuery {
                uri,
                version,
                resolve,
            },
        );
        let task = cx.spawn(async move |this, cx| {
            let answer = first(replies).await;
            this.update(cx, |this, cx| {
                let Some((Some(resolved), _)) = answer else {
                    return;
                };
                let Some(lsp) = &mut this.lsp else {
                    return;
                };
                let Some(menu) = &mut lsp.actions else {
                    return;
                };
                let Some(Pick::Action(entry)) = menu.rows.get_mut(menu.selected) else {
                    return;
                };
                // The row may have changed while the answer was on its way.
                if entry.title != title || !entry.edits.is_empty() {
                    return;
                }
                *entry = resolved;
                cx.notify();
            })
            .ok();
        });
        if let Some(lsp) = &mut self.lsp
            && let Some(menu) = &mut lsp.actions
        {
            menu.preview_task = Some(task);
        }
    }

    /// Applies a row whose server computes the edit only on resolve, once the answer
    /// comes. The text is checked against the version the list was opened on.
    fn resolve_and_apply(&mut self, entry: ActionEntry, version: i32, cx: &mut Context<Self>) {
        let Some(lsp) = self.lsp.as_ref() else {
            return;
        };
        let Some(resolve) = entry.resolve.clone() else {
            return;
        };
        let Some(provider) = lsp.support.action_resolve() else {
            return;
        };
        let uri = lsp.uri.clone();
        let replies = ask(
            provider,
            ActionResolveQuery {
                uri,
                version,
                resolve,
            },
        );
        cx.spawn(async move |this, cx| {
            let answer = first(replies).await;
            this.update(cx, |this, cx| {
                if let Some((Some(resolved), _)) = answer {
                    this.apply_entry(version, resolved, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Runs a row of a list the way Enter does: an action whose edits come on resolve is
    /// resolved first, and any other is applied at once.
    fn apply_picked(&mut self, entry: ActionEntry, version: i32, cx: &mut Context<Self>) {
        if entry.edits.is_empty() && entry.renames.is_empty() && entry.resolve.is_some() {
            self.resolve_and_apply(entry, version, cx);
        } else {
            self.apply_entry(version, entry, cx);
        }
    }

    /// Code → Sort Members and Add final Modifiers: the source action of the file with `code`,
    /// run as Enter runs a row. A file the server gives no such action for says `missing` at
    /// the caret.
    pub(crate) fn source_action(
        &mut self,
        code: &'static str,
        missing: &'static str,
        cx: &mut Context<Self>,
    ) {
        let Some(span) = self.state.as_ref().map(|state| whole(&state.store())) else {
            return;
        };
        let only = Some(ActionKind::Source);
        self.ask_actions(
            span,
            only,
            cx,
            move |this, version, caret, entries, cx| match entries
                .into_iter()
                .find(|entry| entry.code.as_deref() == Some(code))
            {
                Some(entry) => this.apply_picked(entry, version, cx),
                None => this.show_note(caret, missing, cx),
            },
        );
    }

    /// Takes the inline rows out of a language answer while the declaration at the caret is
    /// not known. The rows the declaration allows stay in `entries`, the rows it does not
    /// allow are dropped, and the rows of an unknown declaration are returned to wait for it.
    pub(crate) fn gate_inline(&self, entries: &mut Vec<ActionEntry>) -> Vec<ActionEntry> {
        if !entries.iter().any(is_inline_action) {
            return Vec::new();
        }
        match self.known_declaration() {
            Some(declaration) => {
                if !declaration.offers_inline() {
                    entries.retain(|entry| !is_inline_action(entry));
                }
                Vec::new()
            }
            None => {
                let (waiting, kept): (Vec<ActionEntry>, Vec<ActionEntry>) = std::mem::take(entries)
                    .into_iter()
                    .partition(is_inline_action);
                *entries = kept;
                waiting
            }
        }
    }

    /// Adds the inline rows that the declaration allowed to the list that is open at the same
    /// caret and text. They join the list the way the answer of a waiting list joins it: the
    /// rows come first in their section, and the selection stays on its row.
    fn join_declared_rows(
        &mut self,
        caret: usize,
        version: i32,
        rows: Vec<(MenuSection, Pick)>,
        cx: &mut Context<Self>,
    ) {
        if self
            .state
            .as_ref()
            .is_none_or(|state| state.version() as i32 != version)
        {
            return;
        }
        let Some(lsp) = &mut self.lsp else {
            return;
        };
        let Some(menu) = &mut lsp.actions else {
            return;
        };
        // Only the list of Alt+Enter: it has sections and no heading.
        if menu.at != caret
            || menu.title.is_some()
            || !menu.has_sections()
            || menu.rows.iter().any(Pick::is_waiting)
        {
            return;
        }
        let selected = menu.selected;
        let mut all: Vec<(MenuSection, bool, Pick)> = rows
            .into_iter()
            .map(|(section, pick)| (section, false, pick))
            .collect();
        let sections = std::mem::take(&mut menu.sections);
        all.extend(
            std::mem::take(&mut menu.rows)
                .into_iter()
                .zip(sections)
                .enumerate()
                .map(|(row, (pick, section))| (section, row == selected, pick)),
        );
        all.sort_by_key(|(section, _, _)| *section != MenuSection::QuickFix);
        menu.selected = all.iter().position(|(_, chosen, _)| *chosen).unwrap_or(0);
        menu.sections = all.iter().map(|(section, _, _)| *section).collect();
        menu.rows = all.into_iter().map(|(_, _, pick)| pick).collect();
        menu.preview_task = None;
        cx.notify();
        self.request_action_preview(cx);
    }

    /// Keeps the question of a declaration in the list that waits for its answer. Closing the
    /// list drops the task, and the question with it.
    pub(crate) fn keep_declaration_task(&mut self, task: Option<Task<()>>) {
        if let Some(lsp) = &mut self.lsp
            && let Some(menu) = &mut lsp.actions
        {
            menu.declaration_task = task;
        }
    }

    /// The rows of ALT+Enter that the window adds beside the language actions, such as
    /// Create Test on the name of a type or an inline watch on a value.
    fn own_action_rows(&self) -> Vec<Pick> {
        let Some(state) = &self.state else {
            return Vec::new();
        };
        let mut rows = Vec::new();
        if self.inline_watch_ready
            && let Some((line, expression)) = self.inline_watch_at_caret()
        {
            rows.push(Pick::InlineWatch { line, expression });
        }
        let Some(lsp) = &self.lsp else {
            return rows;
        };
        let caret = state.selections().primary().head;
        rows.extend(self.spelling_picks(caret));
        if !lsp.support.can_create_test(&state.text(), caret) {
            return rows;
        }
        if let Some(spot) = self.spot_at(caret) {
            rows.push(Pick::CreateTest { spot });
        }
        rows
    }

    /// The rows of a misspelled word: its corrections, saving the word to a dictionary
    /// and turning the check off.
    fn spelling_picks(&self, caret: usize) -> Vec<Pick> {
        let Some(mark) = self.spelling_mark_at(caret) else {
            return Vec::new();
        };
        let mut rows = Vec::new();
        for suggestion in &mark.suggestions {
            rows.push(Pick::Spelling {
                label: format!("Change to '{suggestion}'"),
                range: mark.range.clone(),
                replacement: suggestion.clone(),
                declaration: mark.declaration,
            });
        }
        rows.push(Pick::SaveSpelling {
            label: format!("Save '{}' to project dictionary", mark.word),
            word: mark.word.clone(),
            project: true,
        });
        rows.push(Pick::SaveSpelling {
            label: "Save to application dictionary".to_owned(),
            word: mark.word.clone(),
            project: false,
        });
        rows.push(Pick::DisableInspection {
            label: "Disable spell checking".to_owned(),
            id: lang_api::SPELLING_INSPECTION.to_owned(),
            title: lang_api::SPELLING_TITLE.to_owned(),
        });
        rows
    }
    fn inspection_picks(&self, caret: usize) -> Vec<Pick> {
        let (Some(lsp), Some(state)) = (&self.lsp, &self.state) else {
            return Vec::new();
        };
        let Some(mark) = lsp
            .server_marks
            .iter()
            .chain(lsp.native_marks.values().flatten())
            .find(|mark| mark.range.start <= caret && caret <= mark.range.end)
        else {
            return Vec::new();
        };
        // The spelling check names itself in the rows of [`Self::spelling_picks`].
        if mark.code.as_deref() == Some(lang_api::SPELLING_INSPECTION) {
            return Vec::new();
        }
        let Some(code) = mark.code.clone() else {
            return Vec::new();
        };
        let support = lsp.support.clone();
        let catalog = support.inspections();
        let info = support
            .inspection_of(Some(&code), &mark.message)
            .or_else(|| catalog.iter().find(|info| info.id == code).cloned());
        let Some(info) = info else {
            return Vec::new();
        };
        let text = state.text();
        let text: std::sync::Arc<str> = text.into();
        let mut rows = Vec::new();
        let places = self.marks_with_code(&code);
        if places.len() > 1 {
            rows.push(Pick::FixAllInFile {
                label: format!("Fix all '{}' in file", info.title),
                code: code.clone(),
            });
        }
        let annotation = |target| {
            support.suppression_edit(SuppressionQuery {
                text: text.clone(),
                offset: mark.range.start,
                code: Some(code.clone()),
                target,
            })
        };
        if let Some(edit) = annotation(SuppressionTarget::Statement) {
            rows.push(Pick::Suppress {
                label: "Suppress for statement".to_owned(),
                edit,
            });
            if let Some(edit) = annotation(SuppressionTarget::Method) {
                rows.push(Pick::Suppress {
                    label: "Suppress for method".to_owned(),
                    edit,
                });
            }
            if let Some(edit) = annotation(SuppressionTarget::Class) {
                rows.push(Pick::Suppress {
                    label: "Suppress for class".to_owned(),
                    edit,
                });
            }
        } else if info.configurable {
            let line = state.store().offset_to_line(mark.range.start);
            let line_start = state.store().line_to_offset(line);
            let indent: String = text[line_start..]
                .chars()
                .take_while(|character| character.is_whitespace() && *character != '\n')
                .collect();
            rows.push(Pick::Suppress {
                label: "Suppress for statement".to_owned(),
                edit: DocumentEdit {
                    span: TextSpan {
                        start: TextPoint {
                            line: line as u32,
                            character: 0,
                        },
                        end: TextPoint {
                            line: line as u32,
                            character: 0,
                        },
                    },
                    text: format!("{indent}//noinspection {}\n", info.id),
                },
            });
        }
        if info.configurable {
            rows.push(Pick::DisableInspection {
                label: format!("Disable inspection '{}'", info.title),
                id: info.id.clone(),
                title: info.title.clone(),
            });
            rows.push(Pick::EditInspection {
                label: "Edit inspection profile setting".to_owned(),
                id: info.id,
            });
        }
        rows
    }

    /// The places of the file that carry a diagnostic of one check, in text order.
    fn marks_with_code(&self, code: &str) -> Vec<Range<usize>> {
        let Some(lsp) = &self.lsp else {
            return Vec::new();
        };
        let mut ranges: Vec<Range<usize>> = lsp
            .server_marks
            .iter()
            .chain(lsp.native_marks.values().flatten())
            .filter(|mark| mark.code.as_deref() == Some(code))
            .map(|mark| mark.range.clone())
            .collect();
        ranges.sort_by_key(|range| range.start);
        ranges.dedup();
        ranges
    }

    /// Fix all the places of one check in the file with the quick fix the language gives
    /// for the first of them. Every answer works on the version the menu opened on, and
    /// the edits of all answers land as one undo step.
    fn fix_all_in_file(&mut self, version: i32, code: String, cx: &mut Context<Self>) {
        self.catch_up(cx);
        let (Some(lsp), Some(state)) = (&self.lsp, &self.state) else {
            return;
        };
        let uri = lsp.uri.clone();
        let support = lsp.support.clone();
        let store = state.store();
        let ranges = self.marks_with_code(&code);
        if ranges.len() < 2 {
            return;
        }
        let spans: Vec<TextSpan> = ranges
            .iter()
            .map(|range| TextSpan {
                start: point_at(&store, range.start),
                end: point_at(&store, range.end),
            })
            .collect();
        let caret = state.selections().primary().head;
        cx.spawn(async move |this, cx| {
            let mut title: Option<String> = None;
            let mut edits: BTreeMap<String, Vec<DocumentEdit>> = BTreeMap::new();
            let mut renames = Vec::new();
            for span in spans {
                let Some(provider) = support.actions() else {
                    break;
                };
                let replies = ask(
                    provider,
                    ActionQuery {
                        uri: uri.clone(),
                        version,
                        span,
                        only: Some(ActionKind::QuickFix),
                    },
                );
                let Some((entries, _)) = first(replies).await else {
                    continue;
                };
                let mut entry = entries
                    .into_iter()
                    .filter(|entry| {
                        !entry.edits.is_empty()
                            || !entry.renames.is_empty()
                            || entry.resolve.is_some()
                    })
                    .find(|entry| title.as_ref().is_none_or(|title| entry.title == *title));
                // The same fix as the first place may come without its edits, which the
                // server fills in on resolve.
                if let Some(found) = &entry
                    && found.edits.is_empty()
                    && found.renames.is_empty()
                    && let Some(resolve) = &found.resolve
                    && let Some(provider) = support.action_resolve()
                {
                    let replies = ask(
                        provider,
                        ActionResolveQuery {
                            uri: uri.clone(),
                            version,
                            resolve: resolve.clone(),
                        },
                    );
                    if let Some((Some(resolved), _)) = first(replies).await {
                        entry = Some(resolved);
                    }
                }
                let Some(entry) = entry else {
                    continue;
                };
                if title.is_none() {
                    title = Some(entry.title.clone());
                }
                for source in entry.edits {
                    edits.entry(source.uri).or_default().extend(source.edits);
                }
                renames.extend(entry.renames);
            }
            this.update(cx, |this, cx| {
                if edits.is_empty() && renames.is_empty() {
                    this.show_note(caret, NO_FIX_ALL.to_owned(), cx);
                    return;
                }
                let change = WorkspaceChange {
                    edits: edits
                        .into_iter()
                        .map(|(uri, edits)| SourceEdit { uri, edits })
                        .collect(),
                    renames,
                };
                this.apply_change(version, change, cx);
            })
            .ok();
        })
        .detach();
    }

    /// Keys of an open action list. Returns whether the key was taken; Escape is
    /// closed by the popup rules of the view.
    pub(crate) fn action_key(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
        let Some(lsp) = &mut self.lsp else {
            return false;
        };
        let Some(menu) = &mut lsp.actions else {
            return false;
        };
        let mut moved = false;
        // The greyed line of a waiting list picks nothing, and the selection skips it.
        if matches!(key, "enter" | "tab") && !menu.selectable(menu.selected) {
            return true;
        }
        match key {
            "up" => {
                menu.selected = menu.step(menu.selected, false);
                menu.preview_task = None;
                moved = true;
            }
            "down" => {
                menu.selected = menu.step(menu.selected, true);
                menu.preview_task = None;
                moved = true;
            }
            "enter" | "tab" => {
                let picked = lsp.actions.take().map(|mut menu| {
                    let selected = menu.selected;
                    menu.rows.swap_remove(selected)
                });
                // The marks of the list belong to the choice it made.
                lsp.highlights.clear();
                let version = self.state.as_ref().map(|state| state.version() as i32);
                match (picked, version) {
                    (Some(Pick::Action(entry)), Some(version)) => {
                        self.apply_picked(entry, version, cx)
                    }
                    (Some(Pick::ChangeSignature { .. }), _) => self.change_signature(cx),
                    (Some(Pick::Refactor { own, .. }), _) => self.run_own(own, cx),
                    (
                        Some(Pick::Expression {
                            span, refactoring, ..
                        }),
                        _,
                    ) => self.refactor_span(refactoring, span, cx),
                    (Some(Pick::Form { handle, .. }), _) => {
                        cx.emit(EditorEvent::RefactorForm(handle))
                    }
                    (Some(Pick::Place { target, .. }), _) => self.go_to_location(&target, cx),
                    (Some(Pick::Generate { kind, .. }), _) => self.start_generation(kind, cx),
                    (Some(Pick::Unwrap { edit, .. }), _) => self.apply_unwrap(edit, cx),
                    (Some(Pick::Suppress { edit, .. }), _) => {
                        self.apply_document_edits(vec![edit], cx);
                    }
                    (Some(Pick::FixAllInFile { code, .. }), Some(version)) => {
                        self.fix_all_in_file(version, code, cx);
                    }
                    (Some(Pick::DisableInspection { id, title, .. }), _) => {
                        cx.emit(EditorEvent::DisableInspection { id, title })
                    }
                    (Some(Pick::EditInspection { id, .. }), _) => {
                        cx.emit(EditorEvent::EditInspection(id))
                    }
                    (Some(Pick::CreateTest { spot }), _) => cx.emit(EditorEvent::CreateTest(spot)),
                    (Some(Pick::FixWithAi { problem }), _) => {
                        cx.emit(EditorEvent::FixWithAi(problem))
                    }
                    (Some(Pick::InlineWatch { line, expression }), _) => {
                        cx.emit(EditorEvent::AddInlineWatch { line, expression })
                    }
                    (
                        Some(Pick::Spelling {
                            range,
                            replacement,
                            declaration,
                            ..
                        }),
                        Some(version),
                    ) => self.apply_correction(version, range, replacement, declaration, cx),
                    (Some(Pick::SaveSpelling { word, project, .. }), _) => {
                        cx.emit(EditorEvent::SaveSpelling { word, project })
                    }
                    _ => {}
                }
            }
            _ => return false,
        }
        if moved {
            self.highlight_action_row();
            self.request_action_preview(cx);
        }
        cx.notify();
        true
    }

    /// Marks the text a pick list choice would take, so the list shows what it acts on
    /// before Enter applies it: the construct an Unwrap leaves, or the expression an
    /// extract takes.
    pub(crate) fn highlight_action_row(&mut self) {
        let Some(lsp) = &mut self.lsp else {
            return;
        };
        let Some(menu) = &lsp.actions else {
            return;
        };
        let Some(pick) = menu.rows.get(menu.selected) else {
            return;
        };
        let uri = lsp.uri.clone();
        let ranges: Vec<Range<usize>> = match pick {
            Pick::Unwrap { edit, .. } => std::iter::once(edit.start..edit.end).collect(),
            Pick::Expression { span, .. } => {
                let Some(state) = &self.state else {
                    return;
                };
                let store = state.store();
                std::iter::once(expression_range(&store, *span)).collect()
            }
            // An extract of a value replaces the expression at this place or at every
            // place: the list shows the places the selected row would change.
            Pick::Action(entry)
                if entry
                    .code
                    .as_deref()
                    .is_some_and(|code| code.starts_with("refactor.extract.variable")) =>
            {
                let Some(state) = &self.state else {
                    return;
                };
                let store = state.store();
                entry
                    .edits
                    .iter()
                    .filter(|source| source.uri == uri)
                    .flat_map(|source| &source.edits)
                    .filter(|edit| edit.span.start != edit.span.end)
                    .map(|edit| expression_range(&store, edit.span))
                    .collect()
            }
            _ => return,
        };
        if !ranges.is_empty() {
            lsp.highlights = ranges;
        }
    }

    /// Ctrl+Shift+F7: shows every occurrence of the thing under the caret, from the
    /// language server when it answers and from the whole words of the text otherwise.
    pub(crate) fn highlight_usages(&mut self, cx: &mut Context<Self>) {
        let Some(state) = &self.state else {
            return;
        };
        let selection = state.selections().primary();
        let word = state.caret_word().or_else(|| {
            (!selection.is_empty())
                .then(|| text_core::commands::word_at(&state.store(), selection.start()))
                .filter(|word| !word.is_empty())
        });
        let Some(word) = word else {
            return;
        };
        let needle = state.store().slice(word);
        self.usages = Some(word_ranges(&state.text(), &needle));
        let Some(support) = self.lsp.as_ref().map(|lsp| lsp.support.clone()) else {
            cx.notify();
            return;
        };
        let Some(provider) = support.highlights() else {
            cx.notify();
            return;
        };
        let Some((spot, _)) = self.spot() else {
            cx.notify();
            return;
        };
        let version = spot.version;
        let asked = match &mut self.lsp {
            Some(lsp) => {
                lsp.highlight_asked += 1;
                lsp.highlight_asked
            }
            None => {
                cx.notify();
                return;
            }
        };
        let replies = ask(provider, spot);
        cx.spawn(async move |this, cx| {
            let answer = first(replies).await;
            this.update(cx, |this, cx| {
                let Some((spans, _)) = answer else {
                    return;
                };
                let (Some(lsp), Some(state)) = (&mut this.lsp, &this.state) else {
                    return;
                };
                if lsp.highlight_asked != asked
                    || state.version() as i32 != version
                    || this.usages.is_none()
                {
                    return;
                }
                let store = state.store();
                let mut ranges: Vec<Range<usize>> = spans
                    .into_iter()
                    .map(|span| {
                        let start = offset_at(&store, span.start);
                        start..offset_at(&store, span.end).max(start)
                    })
                    .collect();
                ranges.sort_by_key(|range| range.start);
                this.usages = Some(ranges);
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// F3 and Shift+F3 while Highlight Usages is on.
    pub(crate) fn step_usage(&mut self, forward: bool, cx: &mut Context<Self>) {
        let (Some(usages), Some(state)) = (self.usages.clone(), &mut self.state) else {
            return;
        };
        let selection = state.selections().primary();
        let (start, end) = (selection.start(), selection.end());
        let empty = selection.is_empty();
        let target = if forward {
            usages
                .iter()
                .find(|range| {
                    if empty {
                        range.start > start
                    } else {
                        range.start >= end
                    }
                })
                .or_else(|| usages.first())
        } else {
            usages
                .iter()
                .rev()
                .find(|range| {
                    if empty {
                        range.end < start
                    } else {
                        range.end <= start
                    }
                })
                .or_else(|| usages.last())
        };
        let Some(target) = target else {
            return;
        };
        state.select_range(target.clone());
        self.autoscroll = true;
        self.caret_on = true;
        cx.notify();
    }

    /// Asks for the occurrences under the caret once it rests.
    pub(crate) fn highlight_later(&mut self, cx: &mut Context<Self>) {
        let caret = self
            .state
            .as_ref()
            .map(|state| state.selections().primary().head);
        let Some(lsp) = &mut self.lsp else {
            return;
        };
        if lsp.support.highlights().is_none() {
            return;
        }
        lsp.highlight_asked += 1;
        let asked = lsp.highlight_asked;
        // Occurrences stay while the caret is on one of them, so they do not blink.
        let on_one = caret.is_some_and(|caret| {
            lsp.highlights
                .iter()
                .any(|range| range.start <= caret && caret <= range.end)
        });
        if !on_one {
            lsp.highlights.clear();
        }
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(HIGHLIGHT_DELAY).await;
            this.update(cx, |this, cx| this.highlight(asked, cx)).ok();
        })
        .detach();
    }

    fn highlight(&mut self, asked: u64, cx: &mut Context<Self>) {
        let Some((spot, _)) = self.spot() else {
            return;
        };
        let Some(lsp) = &self.lsp else {
            return;
        };
        if lsp.highlight_asked != asked {
            return;
        }
        let Some(provider) = lsp.support.highlights() else {
            return;
        };
        let version = spot.version;
        let replies = ask(provider, spot);
        cx.spawn(async move |this, cx| {
            let answer = first(replies).await;
            this.update(cx, |this, cx| {
                let (Some(lsp), Some(state)) = (&mut this.lsp, &this.state) else {
                    return;
                };
                if lsp.highlight_asked != asked || state.version() as i32 != version {
                    return;
                }
                let spans = answer.map(|(spans, _)| spans).unwrap_or_default();
                let store = state.store();
                lsp.highlights = spans
                    .into_iter()
                    .map(|span| {
                        let start = offset_at(&store, span.start);
                        start..offset_at(&store, span.end).max(start)
                    })
                    .collect();
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

/// The rows of the language's answer for Alt+Enter: the quick fixes first, and the preferred
/// actions first inside a section. The source actions of the whole file are left out, and so
/// is an action with no edit, no rename and no resolve, which has nothing to apply.
fn server_rows(mut entries: Vec<ActionEntry>) -> Vec<(MenuSection, Pick)> {
    entries.retain(|entry| !is_file_action(entry));
    entries.sort_by_key(|entry| (section_of(entry) != MenuSection::QuickFix, !entry.preferred));
    without_duplicate_actions(entries)
        .into_iter()
        .filter(applicable)
        .map(|entry| (section_of(&entry), Pick::Action(entry)))
        .collect()
}

/// Whether an action works on the whole file rather than on the place of the caret: the
/// source actions, Organize Imports and Fix All. Alt+Insert and the Code menu give them.
fn is_file_action(entry: &ActionEntry) -> bool {
    matches!(
        entry.kind,
        Some(ActionKind::Source | ActionKind::OrganizeImports | ActionKind::FixAll)
    )
}

/// Whether an action has something to apply: an edit, a rename, or an edit that the server
/// gives on resolve.
fn applicable(entry: &ActionEntry) -> bool {
    !entry.edits.is_empty() || !entry.renames.is_empty() || entry.resolve.is_some()
}

/// The codes of the source actions that the fixed rows of Source Generate give already, so
/// Alt+Insert does not list the server's action for them again.
const GENERATE_ROW_CODES: [&str; 6] = [
    "source.generate.accessors",
    "source.generate.constructors",
    "source.generate.delegateMethods",
    "source.generate.hashCodeEquals",
    "source.generate.toString",
    "source.overrideMethods",
];

/// Whether a source action is a part of Source Generate, which Alt+Insert lists: the kinds
/// that write code. Code → Sort Members and Code → Add final Modifiers are other source
/// actions, and the Code menu gives them.
fn is_generate_kind(code: &str) -> bool {
    code.starts_with("source.generate.") && code != FINAL_MODIFIERS_KIND
}

/// Whether a row of the server inlines a symbol: Inline Method, Inline Variable or Inline
/// Constant. Whether it is offered depends on the declaration of the symbol.
pub(crate) fn is_inline_action(entry: &ActionEntry) -> bool {
    entry
        .code
        .as_deref()
        .is_some_and(|code| code.starts_with("refactor.inline"))
}

/// The source actions of the language as rows of Alt+Insert: only the kinds of Source
/// Generate. With the fixed rows of Source Generate (`fixed`), the actions those rows give
/// are left out.
pub(crate) fn source_rows(entries: Vec<ActionEntry>, fixed: bool) -> Vec<Pick> {
    entries
        .into_iter()
        .filter(|entry| entry.kind == Some(ActionKind::Source) && applicable(entry))
        .filter(|entry| entry.code.as_deref().is_some_and(is_generate_kind))
        .filter(|entry| {
            let covered = entry
                .code
                .as_deref()
                .is_some_and(|code| GENERATE_ROW_CODES.contains(&code));
            !(fixed && covered)
        })
        .map(Pick::Action)
        .collect()
}

/// The bulb of a row of Alt+Enter: a quick fix of an error is red, a quick fix of a warning
/// is yellow, and a refactoring or an intention has the plain bulb.
pub(crate) fn bulb_icon(quick_fix: bool, warning: bool) -> &'static str {
    match (quick_fix, warning) {
        (true, false) => "codeInsight/quickFix",
        (true, true) => "codeInsight/intention",
        (false, _) => "codeInsight/intentionBulb",
    }
}

/// The actions of a language answer. An answer that says it is unknown gives none.
pub(crate) fn answered_entries(answer: Option<(Vec<ActionEntry>, Confidence)>) -> Vec<ActionEntry> {
    match answer {
        Some((entries, confidence)) if confidence != Confidence::Unknown => entries,
        _ => Vec::new(),
    }
}

/// The rows of a pick list without the same action twice: a server can offer one fix as
/// a quick fix and as a source action, with one title and one edit or command. The row
/// with the better kind stays, because the sort put it first.
fn without_duplicate_actions(entries: Vec<ActionEntry>) -> Vec<ActionEntry> {
    let mut kept: Vec<ActionEntry> = Vec::with_capacity(entries.len());
    for entry in entries {
        let duplicate = kept.iter().any(|known| {
            known.title == entry.title
                && known.handle == entry.handle
                && known.resolve == entry.resolve
                && known.edits == entry.edits
                && known.renames == entry.renames
        });
        if !duplicate {
            kept.push(entry);
        }
    }
    kept
}

#[cfg(test)]
mod tests {
    use lang_api::{DocumentEdit, TextPoint, TextSpan};
    use text_core::{FileFormat, RopeStore, TextStore};

    use super::{
        Pick, bulb_icon, server_batches, server_edits, server_rows, source_rows,
        without_duplicate_actions,
    };
    use crate::EditorState;

    const TEXT: &str = "import b;\nimport a;\nclass X {}\n";

    fn edit(start: (u32, u32), end: (u32, u32), text: &str) -> DocumentEdit {
        let point = |(line, character)| TextPoint { line, character };
        DocumentEdit {
            span: TextSpan {
                start: point(start),
                end: point(end),
            },
            text: text.to_owned(),
        }
    }

    /// What the editor does with an answer: sort, check, apply as one step.
    fn apply(edits: Vec<DocumentEdit>) -> Option<String> {
        let mut state = EditorState::new(TEXT, FileFormat::default(), None, false);
        let edits = server_edits(&RopeStore::from_text(TEXT), edits)?;
        state.apply_edits(edits);
        Some(state.text())
    }

    // The cases of the qa check of T-0025: lists a server may send.

    #[test]
    fn one_action_offered_twice_stays_once() {
        use lang_api::{ActionEntry, ActionKind};

        let entry = |kind: ActionKind, title: &str, code: &str| ActionEntry {
            title: title.to_owned(),
            kind: Some(kind),
            code: Some(code.to_owned()),
            preferred: false,
            edits: Vec::new(),
            renames: Vec::new(),
            handle: None,
            resolve: None,
            rename: None,
        };
        // The unused import comes back as a quick fix and as a source action, with the
        // same title and the same edit but a code of its own kind.
        let rows = without_duplicate_actions(vec![
            entry(ActionKind::QuickFix, "Organize imports", "quickfix.import"),
            entry(
                ActionKind::Source,
                "Organize imports",
                "source.organizeImports",
            ),
            entry(ActionKind::Source, "Sort members", "source.sortMembers"),
        ]);
        let titles: Vec<&str> = rows.iter().map(|row| row.title.as_str()).collect();
        assert_eq!(titles, ["Organize imports", "Sort members"]);
    }

    #[test]
    fn actions_that_differ_in_their_edit_are_kept_apart() {
        use lang_api::ActionEntry;

        let entry = |text: &str| ActionEntry {
            title: "Organize imports".to_owned(),
            kind: None,
            code: None,
            preferred: false,
            edits: Vec::new(),
            renames: Vec::new(),
            handle: Some(text.to_owned()),
            resolve: None,
            rename: None,
        };
        let rows = without_duplicate_actions(vec![entry("first"), entry("first"), entry("second")]);
        assert_eq!(rows.len(), 2, "the same form is one row");
    }

    #[test]
    fn unsorted_edits_apply_in_the_order_of_the_text() {
        let edits = vec![
            edit((1, 0), (1, 9), "import c;"),
            edit((0, 0), (0, 9), "import z;"),
        ];
        assert_eq!(
            apply(edits).as_deref(),
            Some("import z;\nimport c;\nclass X {}\n")
        );
    }

    #[test]
    fn inserts_at_one_place_keep_the_order_of_the_answer() {
        let edits = vec![edit((0, 0), (0, 0), "A\n"), edit((0, 0), (0, 0), "B\n")];
        assert_eq!(
            apply(edits).as_deref(),
            Some("A\nB\nimport b;\nimport a;\nclass X {}\n")
        );
    }

    #[test]
    fn overlapping_edits_change_nothing() {
        let edits = vec![edit((0, 0), (0, 9), "x"), edit((0, 5), (1, 2), "y")];
        assert_eq!(apply(edits), None);
    }

    #[test]
    fn an_insert_where_a_replace_starts_is_not_an_overlap() {
        let edits = vec![
            edit((0, 0), (0, 9), "import z;"),
            edit((0, 0), (0, 0), "// a\n"),
        ];
        assert_eq!(
            apply(edits).as_deref(),
            Some("// a\nimport z;\nimport a;\nclass X {}\n")
        );
    }

    #[test]
    fn one_overlapping_document_edit_stops_all_of_them() {
        let sources = vec![
            vec![edit((0, 0), (0, 9), "import z;")],
            vec![edit((0, 0), (0, 9), "x"), edit((0, 5), (1, 2), "y")],
        ];
        assert!(server_batches(&RopeStore::from_text(TEXT), sources).is_none());
    }

    #[test]
    fn several_document_edits_of_one_answer_undo_in_one_step() {
        let sources = vec![
            vec![edit((0, 0), (0, 0), "// header\n")],
            vec![edit((0, 0), (1, 0), "// changed\n")],
        ];
        let batches = server_batches(&RopeStore::from_text(TEXT), sources).unwrap();
        let mut state = EditorState::new(TEXT, FileFormat::default(), None, false);
        state.apply_edits_batches(batches);
        assert_eq!(
            state.text(),
            "// changed\nimport b;\nimport a;\nclass X {}\n"
        );
        state.perform(keymap::editor::UNDO, 10);
        assert_eq!(state.text(), TEXT);
    }

    #[test]
    fn alt_enter_lists_only_the_quick_fixes_and_the_refactorings() {
        use lang_api::{ActionEntry, ActionKind};

        let entry = |title: &str, kind: ActionKind| ActionEntry {
            title: title.to_owned(),
            kind: Some(kind),
            code: None,
            preferred: false,
            edits: Vec::new(),
            renames: Vec::new(),
            handle: None,
            resolve: Some(format!("resolve {title}")),
            rename: None,
        };
        let rows = server_rows(vec![
            entry("Organize imports", ActionKind::OrganizeImports),
            entry("Sort members", ActionKind::Source),
            entry("Clean up", ActionKind::FixAll),
            entry("Inline", ActionKind::Refactor),
            entry("Add import", ActionKind::QuickFix),
        ]);
        let titles: Vec<&str> = rows.iter().map(|(_, pick)| pick.label()).collect();
        assert_eq!(titles, ["Add import", "Inline"]);
    }

    #[test]
    fn alt_insert_lists_only_the_generate_kinds_and_each_once() {
        use lang_api::{ActionEntry, ActionKind};

        let entry = |title: &str, kind: ActionKind, code: &str| ActionEntry {
            title: title.to_owned(),
            kind: Some(kind),
            code: Some(code.to_owned()),
            preferred: false,
            edits: Vec::new(),
            renames: Vec::new(),
            handle: None,
            resolve: Some(format!("resolve {title}")),
            rename: None,
        };
        let answer = || {
            vec![
                entry(
                    "Generate toString()",
                    ActionKind::Source,
                    "source.generate.toString",
                ),
                entry("Sort members", ActionKind::Source, "source.sortMembers"),
                entry(
                    "Organize imports",
                    ActionKind::OrganizeImports,
                    "source.organizeImports",
                ),
                entry(
                    "Change modifiers",
                    ActionKind::Source,
                    "source.generate.finalModifiers",
                ),
            ]
        };
        let labels = |rows: Vec<Pick>| -> Vec<String> {
            rows.iter().map(|pick| pick.label().to_owned()).collect()
        };
        // The fixed rows of Source Generate give toString already, and Sort Members and the
        // final modifiers belong to Code, so nothing is left.
        assert!(labels(source_rows(answer(), true)).is_empty());
        // Without the fixed rows the generate kind is listed; neither Organize Imports nor the
        // other source actions are.
        assert_eq!(
            labels(source_rows(answer(), false)),
            ["Generate toString()"]
        );
    }

    #[test]
    fn a_quick_fix_of_an_error_is_red_and_of_a_warning_yellow() {
        assert_eq!(bulb_icon(true, false), "codeInsight/quickFix");
        assert_eq!(bulb_icon(true, true), "codeInsight/intention");
        assert_eq!(bulb_icon(false, false), "codeInsight/intentionBulb");
        assert_eq!(bulb_icon(false, true), "codeInsight/intentionBulb");
    }

    #[test]
    fn each_document_edit_works_on_the_text_of_the_one_before() {
        // The first edit adds a line; the second one replaces it by its new place.
        let sources = vec![
            vec![edit((0, 0), (0, 0), "// header\n")],
            vec![edit((0, 0), (1, 0), "// changed\n")],
        ];
        let batches = server_batches(&RopeStore::from_text(TEXT), sources).unwrap();
        let mut store = RopeStore::from_text(TEXT);
        let mut format = FileFormat::default();
        for edits in batches {
            text_core::apply_edits(&mut store, &mut format, &edits);
        }
        assert_eq!(
            store.text(),
            "// changed\nimport b;\nimport a;\nclass X {}\n"
        );
    }
}
