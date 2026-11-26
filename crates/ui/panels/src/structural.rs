//! Search Structurally and Replace Structurally: a template of code with `$name$`
//! variables, the filters of those variables, the ready templates of the language and
//! the places it finds. The places go to the Find window, grouped by file.

use std::collections::{BTreeMap, HashMap};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ::base::Cancel;
use editor_view::{EditorView, HighlightKind, HighlightSpan, Semantic};
use futures_channel::mpsc::unbounded;
use futures_util::StreamExt;
use lang_api::{
    LanguageSupport, StructuralMatch, StructuralQuery, StructuralTemplate, StructuralVariable,
};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::{USAGES_TOOL, Workspace};

use crate::items::{Item, PanelRow};
use crate::usages::ActiveUsages;

const CONTEXT: &str = "StructuralSearch";
const WIDTH: f32 = 960.;
const HEIGHT: f32 = 640.;
const LIST_WIDTH: f32 = 250.;
const EDITOR_LINE: f32 = 18.;

/// The dialog on screen, if any. The window tests read it; the commands only set it.
#[allow(dead_code)]
pub struct ActiveStructuralSearch(pub Option<Entity<StructuralSearch>>);

impl Global for ActiveStructuralSearch {}

/// The templates of the last searches, newest first.
#[derive(Default)]
pub struct StructuralHistory(pub Vec<String>);

impl Global for StructuralHistory {}

/// Where a structural search looks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StructuralScope {
    #[default]
    Project,
    Module,
    Directory,
    CurrentFile,
    Selection,
}

impl StructuralScope {
    const ALL: [StructuralScope; 5] = [
        StructuralScope::Project,
        StructuralScope::Module,
        StructuralScope::Directory,
        StructuralScope::CurrentFile,
        StructuralScope::Selection,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Project => "Project",
            Self::Module => "Module",
            Self::Directory => "Directory",
            Self::CurrentFile => "Current File",
            Self::Selection => "Selection",
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::Module => "module",
            Self::Directory => "directory",
            Self::CurrentFile => "file",
            Self::Selection => "selection",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FieldKind {
    Text,
    Count,
    Type,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FocusAt {
    Template,
    Replacement,
    Variable(usize, FieldKind),
}

struct VariableRow {
    name: String,
    text: TextField,
    count: TextField,
    type_filter: TextField,
}

impl VariableRow {
    fn from_variable(variable: &StructuralVariable) -> Self {
        let count = match variable.count {
            Some((min, max)) => {
                let max = max.map_or_else(|| "∞".to_owned(), |max| max.to_string());
                format!("[{min},{max}]")
            }
            None => String::new(),
        };
        Self {
            name: variable.name.clone(),
            text: TextField::from_text(variable.text.clone().unwrap_or_default()),
            count: TextField::from_text(count),
            type_filter: TextField::from_text(variable.type_pattern.clone().unwrap_or_default()),
        }
    }

    fn filter(&self) -> Result<StructuralVariable, String> {
        Ok(StructuralVariable {
            name: self.name.clone(),
            text: (!self.text.text.trim().is_empty()).then(|| self.text.text.trim().to_owned()),
            count: parse_count(&self.count.text)?,
            type_pattern: (!self.type_filter.text.trim().is_empty())
                .then(|| self.type_filter.text.trim().to_owned()),
        })
    }
}

/// `[min,max]`, `[min,∞]` or a single number. An empty text keeps the default of one.
fn parse_count(text: &str) -> Result<Option<(usize, Option<usize>)>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    if let Some(inner) = text.strip_prefix('[').and_then(|it| it.strip_suffix(']')) {
        let (min, max) = inner
            .split_once(',')
            .ok_or_else(|| format!("count `{text}` wants `[min,max]`"))?;
        let min: usize = min
            .trim()
            .parse()
            .map_err(|_| format!("count `{text}` has no minimum"))?;
        let max = max.trim();
        let max = if max.is_empty() || max == "∞" || max == "*" {
            None
        } else {
            Some(
                max.parse()
                    .map_err(|_| format!("count `{text}` has no maximum"))?,
            )
        };
        if max.is_some_and(|max| max < min) {
            return Err(format!("count `{text}` ends before it starts"));
        }
        return Ok(Some((min, max)));
    }
    let count: usize = text
        .parse()
        .map_err(|_| format!("count `{text}` is not a number"))?;
    Ok(Some((count, Some(count))))
}

enum Message {
    File {
        path: PathBuf,
        text: String,
        matches: Vec<StructuralMatch>,
    },
    Progress {
        searched: usize,
        total: usize,
    },
    Error(String),
    Done,
}

pub struct StructuralSearch {
    files: Entity<Arc<Vec<PathBuf>>>,
    root: Arc<PathBuf>,
    workspace: WeakEntity<Workspace>,
    support: Option<Arc<dyn LanguageSupport>>,
    focus: FocusHandle,
    templates_scroll: ScrollHandle,
    variables_scroll: ScrollHandle,
    history_scroll: ScrollHandle,
    replace_mode: bool,
    template: TextField,
    replacement: TextField,
    variables: Vec<VariableRow>,
    templates: Vec<StructuralTemplate>,
    history: Vec<String>,
    scope: StructuralScope,
    focus_at: FocusAt,
    error: Option<String>,
    status: Option<String>,
    searched: usize,
    total: usize,
    matches: usize,
    result_files: usize,
    results: BTreeMap<PathBuf, (String, Vec<StructuralMatch>)>,
    searching: bool,
    serial: usize,
    cancel: Cancel,
    task: Option<Task<()>>,
    highlights: Vec<HighlightSpan>,
    highlight_serial: usize,
    highlight_for: String,
}

type ScopeFiles = (Vec<PathBuf>, HashMap<PathBuf, String>, Option<Range<usize>>);

impl StructuralSearch {
    pub fn show(
        replace_mode: bool,
        files: Entity<Arc<Vec<PathBuf>>>,
        root: Arc<PathBuf>,
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        let support = cx
            .try_global::<Semantic>()
            .map(|semantic| semantic.0.clone());
        let templates = support
            .as_ref()
            .and_then(|support| support.structural_search())
            .map(|provider| provider.templates())
            .unwrap_or_default();
        let history = cx
            .try_global::<StructuralHistory>()
            .map_or_else(Vec::new, |history| history.0.clone());
        let weak = cx.weak_entity();
        let view = cx.new(|cx| Self {
            files,
            root,
            workspace: weak,
            support,
            focus: cx.focus_handle(),
            templates_scroll: ScrollHandle::new(),
            variables_scroll: ScrollHandle::new(),
            history_scroll: ScrollHandle::new(),
            replace_mode,
            template: TextField::default(),
            replacement: TextField::default(),
            variables: Vec::new(),
            templates,
            history,
            scope: StructuralScope::default(),
            focus_at: FocusAt::Template,
            error: None,
            status: None,
            searched: 0,
            total: 0,
            matches: 0,
            result_files: 0,
            results: BTreeMap::new(),
            searching: false,
            serial: 0,
            cancel: Cancel::new(),
            task: None,
            highlights: Vec::new(),
            highlight_serial: 0,
            highlight_for: String::new(),
        });
        cx.set_global(ActiveStructuralSearch(Some(view.clone())));
        let focus = view.read(cx).focus.clone();
        workspace.show_overlay(view.into(), focus, window, cx);
    }

    fn template_pattern(&self) -> String {
        self.template.text.trim().to_owned()
    }

    fn query(&self) -> Result<StructuralQuery, String> {
        let mut variables = Vec::new();
        for row in &self.variables {
            variables.push(row.filter()?);
        }
        Ok(StructuralQuery {
            pattern: self.template.text.clone(),
            replacement: self
                .replace_mode
                .then(|| self.replacement.text.clone())
                .filter(|text| !text.trim().is_empty()),
            variables,
        })
    }

    /// The path the syntax colours of the template come from: the first file of the
    /// project the language owns.
    fn language_path(&self, cx: &App) -> Option<PathBuf> {
        let support = self.support.as_ref()?;
        let files = self.files.read(cx);
        files
            .iter()
            .find(|path| support.matches(path))
            .cloned()
            .or_else(|| {
                let workspace = self.workspace.upgrade()?;
                let path = workspace.read(cx).active_path()?;
                support.matches(&path).then_some(path)
            })
    }

    fn update_highlight(&mut self, cx: &mut Context<Self>) {
        let text = self.template.text.clone();
        if text == self.highlight_for {
            return;
        }
        self.highlight_for = text.clone();
        self.highlight_serial += 1;
        let serial = self.highlight_serial;
        let Some(path) = self.language_path(cx) else {
            self.highlights.clear();
            return;
        };
        let Some(task) = editor_view::highlight_text(&path, text, cx) else {
            self.highlights.clear();
            return;
        };
        cx.spawn(async move |this, cx| {
            let spans = task.await;
            this.update(cx, |this, cx| {
                if this.highlight_serial == serial {
                    this.highlights = spans;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// The variable rows follow the names of the template, keeping the filters typed
    /// for the names that stay.
    fn sync_variables(&mut self) {
        let Some(provider) = self
            .support
            .as_ref()
            .and_then(|support| support.structural_search())
        else {
            return;
        };
        match provider.variables(&self.template.text) {
            Ok(names) => {
                let mut rows = Vec::with_capacity(names.len());
                for name in names {
                    if let Some(at) = self.variables.iter().position(|row| row.name == name) {
                        rows.push(self.variables.remove(at));
                    } else {
                        rows.push(VariableRow::from_variable(&StructuralVariable::named(name)));
                    }
                }
                self.variables = rows;
                if let FocusAt::Variable(index, _) = self.focus_at
                    && index >= self.variables.len()
                {
                    self.focus_at = FocusAt::Template;
                }
                self.error = None;
            }
            Err(error) => {
                self.error = Some(error);
            }
        }
    }

    /// The files and texts of the scope, and the range of the selection when the scope
    /// is the selection of the editor.
    fn scope(&self, cx: &App) -> Result<ScopeFiles, String> {
        let Some(support) = self.support.as_ref() else {
            return Err("The language of the project has no structural search".to_owned());
        };
        let workspace = self.workspace.upgrade();
        let active = workspace
            .as_ref()
            .and_then(|workspace| workspace.read(cx).active_path());
        let all: Vec<PathBuf> = self
            .files
            .read(cx)
            .iter()
            .filter(|path| support.matches(path))
            .cloned()
            .collect();
        // The unsaved text of a file that no tab shows is the text to search, as in Find in Files.
        let open = crate::finder::open_texts(&self.workspace, cx);
        let files = match self.scope {
            StructuralScope::Project => all,
            StructuralScope::Module => {
                let Some(active) = active.clone() else {
                    return Err("No file is open".to_owned());
                };
                let module = support.module_of(&active);
                all.into_iter()
                    .filter(|path| support.module_of(path) == module)
                    .collect()
            }
            StructuralScope::Directory => {
                let Some(active) = active.clone() else {
                    return Err("No file is open".to_owned());
                };
                let folder = active.parent().map(Path::to_path_buf);
                folder.map_or_else(Vec::new, |folder| {
                    all.into_iter()
                        .filter(|path| path.starts_with(&folder))
                        .collect()
                })
            }
            StructuralScope::CurrentFile | StructuralScope::Selection => {
                let Some(active) = active else {
                    return Err("No file is open".to_owned());
                };
                vec![active]
            }
        };
        let mut range = None;
        if self.scope == StructuralScope::Selection {
            let Some(workspace) = &workspace else {
                return Err("No file is open".to_owned());
            };
            let workspace = workspace.read(cx);
            let Some(path) = workspace.active_path() else {
                return Err("No file is open".to_owned());
            };
            let Some(view) = workspace
                .view_of(&path)
                .and_then(|view| view.downcast::<EditorView>().ok())
            else {
                return Err("No file is open".to_owned());
            };
            let Some((selection, _)) = view.read(cx).selection() else {
                return Err("Select a piece of the file first".to_owned());
            };
            if selection.is_empty() {
                return Err("Select a piece of the file first".to_owned());
            }
            range = Some(selection);
        }
        let open = crate::finder::spelled_as(open, &files);
        Ok((files, open, range))
    }

    fn refresh(&mut self, window: Option<&mut Window>, cx: &mut Context<Self>) {
        self.serial += 1;
        self.cancel.cancel();
        self.cancel = Cancel::new();
        self.task = None;
        self.error = None;
        self.update_highlight(cx);
        self.sync_variables();
        if self.template_pattern().is_empty() {
            self.searching = false;
            self.status = None;
            self.matches = 0;
            self.result_files = 0;
            self.results.clear();
            self.publish(cx);
            if let Some(window) = window {
                window.focus(&self.focus, cx);
            }
            cx.notify();
            return;
        }
        let query = match self.query() {
            Ok(query) => query,
            Err(error) => {
                self.error = Some(error);
                self.searching = false;
                cx.notify();
                return;
            }
        };
        let (files, open, range) = match self.scope(cx) {
            Ok(scope) => scope,
            Err(error) => {
                self.error = Some(error);
                self.searching = false;
                cx.notify();
                return;
            }
        };
        let Some(support) = self.support.clone() else {
            self.error = Some("The language of the project has no structural search".to_owned());
            cx.notify();
            return;
        };
        let serial = self.serial;
        self.searching = true;
        self.searched = 0;
        self.total = files.len();
        self.results.clear();
        self.status = Some(format!("Searching {} files…", files.len()));
        if let Some(window) = window
            && let Some(workspace) = self.workspace.upgrade()
        {
            workspace.update(cx, |workspace, cx| {
                workspace.show_tool(USAGES_TOOL, window, cx)
            });
            window.focus(&self.focus, cx);
        }
        let cancel = self.cancel.clone();
        let (sender, mut receiver) = unbounded();
        let job = cx.background_executor().spawn(async move {
            let Some(provider) = support.structural_search() else {
                sender.unbounded_send(Message::Done).ok();
                return;
            };
            let total = files.len();
            for (index, path) in files.iter().enumerate() {
                if cancel.is_cancelled() {
                    return;
                }
                let text = open
                    .get(path)
                    .cloned()
                    .or_else(|| std::fs::read_to_string(path).ok());
                if let Some(text) = text {
                    match provider.find(&text, &query) {
                        Ok(mut found) => {
                            if let Some(range) = &range {
                                found.retain(|found| {
                                    range.start <= found.start && found.end <= range.end
                                });
                            }
                            if !found.is_empty() {
                                sender
                                    .unbounded_send(Message::File {
                                        path: path.clone(),
                                        text,
                                        matches: found,
                                    })
                                    .ok();
                            }
                        }
                        Err(error) => {
                            sender.unbounded_send(Message::Error(error)).ok();
                            return;
                        }
                    }
                }
                sender
                    .unbounded_send(Message::Progress {
                        searched: index + 1,
                        total,
                    })
                    .ok();
            }
            sender.unbounded_send(Message::Done).ok();
        });
        self.task = Some(cx.spawn(async move |this, cx| {
            let _job = job;
            while let Some(message) = receiver.next().await {
                let keep = this
                    .update(cx, |this, cx| {
                        if this.serial != serial {
                            return false;
                        }
                        this.arrived(message, cx);
                        true
                    })
                    .unwrap_or(false);
                if !keep {
                    break;
                }
            }
        }));
        cx.notify();
    }

    fn arrived(&mut self, message: Message, cx: &mut Context<Self>) {
        match message {
            Message::File {
                path,
                text,
                matches,
            } => {
                self.results.insert(path, (text, matches));
                self.publish(cx);
            }
            Message::Progress { searched, total } => {
                self.searched = searched;
                self.total = total;
                self.searching = searched < total;
                if self.searching {
                    self.status = Some(format!("Searching… {searched} of {total} files"));
                }
                cx.notify();
            }
            Message::Error(error) => {
                self.error = Some(error);
                self.searching = false;
                cx.notify();
            }
            Message::Done => {
                self.searching = false;
                let text = match (self.matches, self.result_files) {
                    (0, _) => "Nothing found".to_owned(),
                    (matches, files) => format!("{matches} matches in {files} files"),
                };
                self.status = Some(text);
                self.remember(cx);
                self.publish(cx);
                cx.notify();
            }
        }
    }

    /// Puts the places of the search into the Find window, grouped by file.
    fn publish(&mut self, cx: &mut Context<Self>) {
        let icons = self
            .workspace
            .upgrade()
            .map(|workspace| workspace.read(cx).icons().clone())
            .unwrap_or_default();
        let mut items = Vec::new();
        let mut rows = Vec::new();
        for (path, (text, matches)) in &self.results {
            let relative = path.strip_prefix(self.root.as_path()).unwrap_or(path);
            rows.push(PanelRow::Group {
                label: relative.display().to_string(),
                depth: 0,
                icon: icons.icon_for(path),
                hint: None,
            });
            for found in matches {
                let line_start = text[..found.start.min(text.len())]
                    .rfind('\n')
                    .map_or(0, |at| at + 1);
                let line = text[..found.start.min(text.len())]
                    .bytes()
                    .filter(|byte| *byte == b'\n')
                    .count();
                let column = text[line_start..found.start.min(text.len())]
                    .encode_utf16()
                    .count();
                let at = items.len();
                items.push(Item {
                    path: path.clone(),
                    line,
                    column,
                    label: one_line(&found.text),
                    hint: found
                        .replacement
                        .as_ref()
                        .map(|replacement| format!("→ {}", one_line(replacement)))
                        .or_else(|| (!found.notes.is_empty()).then(|| found.notes.join(", ")))
                        .unwrap_or_else(|| format!("{}:{}", line + 1, column + 1)),
                    icon: "general/search",
                });
                rows.push(PanelRow::Item {
                    index: at,
                    depth: 1,
                    excluded: false,
                });
            }
        }
        self.matches = items.len();
        self.result_files = self.results.len();
        let title = if self.replace_mode {
            "Replace Structurally"
        } else {
            "Search Structurally"
        };
        let hub = cx.try_global::<ActiveUsages>().map(|hub| hub.0.clone());
        if let Some(hub) = hub {
            hub.update(cx, |hub, cx| {
                hub.show_structural(title.to_owned(), items, rows, cx)
            });
        }
    }

    fn remember(&mut self, cx: &mut Context<Self>) {
        let pattern = self.template_pattern();
        if pattern.is_empty() {
            return;
        }
        self.history.retain(|kept| *kept != pattern);
        self.history.insert(0, pattern);
        self.history.truncate(20);
        cx.default_global::<StructuralHistory>().0 = self.history.clone();
    }

    fn load_template(&mut self, template: &StructuralTemplate, cx: &mut Context<Self>) {
        self.template = TextField::from_text(template.pattern.clone());
        self.replacement = TextField::from_text(template.replacement.clone().unwrap_or_default());
        self.variables = template
            .variables
            .iter()
            .map(VariableRow::from_variable)
            .collect();
        self.focus_at = FocusAt::Template;
        self.highlight_for.clear();
        self.refresh(None, cx);
    }

    fn fields(&self) -> Vec<FocusAt> {
        let mut fields = vec![FocusAt::Template];
        if self.replace_mode {
            fields.push(FocusAt::Replacement);
        }
        for (index, _) in self.variables.iter().enumerate() {
            for kind in [FieldKind::Text, FieldKind::Count, FieldKind::Type] {
                fields.push(FocusAt::Variable(index, kind));
            }
        }
        fields
    }

    fn move_focus(&mut self, back: bool) {
        let fields = self.fields();
        let at = fields
            .iter()
            .position(|field| *field == self.focus_at)
            .unwrap_or(0);
        let next = if back {
            (at + fields.len() - 1) % fields.len()
        } else {
            (at + 1) % fields.len()
        };
        self.focus_at = fields[next];
    }

    fn focused_field(&mut self) -> Option<&mut TextField> {
        match self.focus_at {
            FocusAt::Template => Some(&mut self.template),
            FocusAt::Replacement => Some(&mut self.replacement),
            FocusAt::Variable(index, kind) => {
                let row = self.variables.get_mut(index)?;
                Some(match kind {
                    FieldKind::Text => &mut row.text,
                    FieldKind::Count => &mut row.count,
                    FieldKind::Type => &mut row.type_filter,
                })
            }
        }
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let key = &event.keystroke;
        if key.key == "escape" {
            self.dismiss(window, cx);
            return true;
        }
        if key.modifiers.control && key.key == "enter" {
            if self.replace_mode {
                self.replace_all(cx);
            } else {
                self.refresh(Some(window), cx);
            }
            return true;
        }
        if key.key == "tab" && !key.modifiers.control && !key.modifiers.alt {
            self.move_focus(key.modifiers.shift);
            cx.notify();
            return true;
        }
        let multiline = matches!(self.focus_at, FocusAt::Template | FocusAt::Replacement);
        let Some(field) = self.focused_field() else {
            return false;
        };
        if !field.key_with_clipboard(event, multiline, cx) {
            return false;
        }
        if matches!(self.focus_at, FocusAt::Template | FocusAt::Replacement) {
            self.refresh(Some(window), cx);
        } else {
            self.refresh(None, cx);
        }
        true
    }

    fn replace_all(&mut self, cx: &mut Context<Self>) {
        if !self.replace_mode || self.results.is_empty() {
            return;
        }
        let query = match self.query() {
            Ok(query) => query,
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                return;
            }
        };
        let Some(support) = self.support.clone() else {
            return;
        };
        let workspace = self.workspace.upgrade();
        let mut done = 0;
        let mut closed = Vec::new();
        for path in self.results.keys() {
            // The view of the tab of the file, or of the buffer of a file that no tab shows.
            let view = workspace
                .as_ref()
                .and_then(|workspace| workspace.read(cx).view_of(path))
                .and_then(|view| view.downcast::<EditorView>().ok());
            let Some(view) = view else {
                closed.push(path.clone());
                continue;
            };
            let text = view.read(cx).text().unwrap_or_default();
            let Ok(edits) = support
                .structural_search()
                .expect("the search ran with the same language")
                .replace(&text, &query)
            else {
                continue;
            };
            done += edits.len();
            let bytes = view.read(cx).file_bytes();
            crate::history::snapshot_bytes(path, bytes, local_history::Cause::BeforeReplace, cx);
            let edits: Vec<text_core::Edit> = edits
                .into_iter()
                .map(|edit| text_core::Edit::new(edit.start..edit.end, edit.replacement))
                .collect();
            // Unsaved text stays unsaved: the replacement waits in the buffer until Save.
            let keep_unsaved = view.read(cx).is_dirty();
            view.update(cx, |view, cx| {
                view.apply_edits(edits, cx);
                if !keep_unsaved {
                    view.save(cx);
                }
            });
        }
        let files = self.results.len();
        let history = crate::history::get(cx);
        let work = cx.background_executor().spawn(async move {
            if let Some(history) = &history {
                for path in &closed {
                    let _ = history.record_file(path, local_history::Cause::BeforeReplace);
                }
            }
            replace_on_disk(&support, &query, &closed)
        });
        cx.spawn(async move |this, cx| {
            let outcome = work.await;
            let (text, failed) = match outcome {
                Ok(count) => (
                    format!("{} replacements in {files} files", done + count),
                    false,
                ),
                Err(message) => (message, true),
            };
            this.update(cx, |this, cx| {
                if let Some(workspace) = this.workspace.upgrade() {
                    workspace.update(cx, |workspace, cx| {
                        if failed {
                            workspace.notify_error("Replace Structurally", text, cx)
                        } else {
                            workspace.notify_user("Replace Structurally", text, cx)
                        }
                    });
                }
                this.refresh(None, cx);
            })
            .ok();
        })
        .detach();
    }

    fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.cancel.cancel();
        self.task = None;
        cx.set_global(ActiveStructuralSearch(None));
        self.workspace
            .update(cx, |workspace, cx| workspace.dismiss(window, cx))
            .ok();
    }

    /// The template of the dialog, for tests.
    #[cfg(test)]
    pub(crate) fn template_text(&self) -> &str {
        &self.template.text
    }

    /// The variable names the dialog listed, for tests.
    #[cfg(test)]
    pub(crate) fn variable_names(&self) -> Vec<String> {
        self.variables.iter().map(|row| row.name.clone()).collect()
    }

    /// The line of the status under the template, for tests.
    #[cfg(test)]
    pub(crate) fn status_text(&self) -> Option<&str> {
        self.error.as_deref().or(self.status.as_deref())
    }

    fn input<'a>(
        mouse: FieldMouse,
        value: &'a TextField,
        label: &'a str,
        active: bool,
        selector: impl Fn() -> String + 'static,
        width: Option<f32>,
        theme: &Theme,
    ) -> Div {
        let empty = value.text.is_empty();
        let mut boxed = div()
            .debug_selector(selector)
            .flex()
            .items_center()
            .h(px(24.))
            .px(px(SPACE_S))
            .rounded(px(CONTROL_RADIUS))
            .border_1()
            .border_color(if active {
                theme.accent
            } else {
                theme.control_border
            })
            .overflow_hidden()
            .whitespace_nowrap();
        if let Some(width) = width {
            boxed = boxed.w(px(width));
        } else {
            boxed = boxed.flex_1();
        }
        boxed
            .when(empty && !active, |this| {
                this.text_color(theme.text_muted).child(label.to_owned())
            })
            .when(!empty || active, |this| {
                this.children(text_field_children(
                    mouse,
                    value,
                    theme.text,
                    theme.text,
                    theme.selection,
                    theme.caret,
                    EDITOR_LINE,
                    active,
                ))
            })
    }

    /// The template editor: the syntax colours of the language and the `$name$`
    /// variables in the accent colour.
    fn editor(
        &self,
        field: &TextField,
        label: &str,
        active: bool,
        heights: usize,
        theme: &Theme,
    ) -> impl IntoElement + use<> {
        let variables = variable_ranges(&field.text);
        let lines = editor_lines(
            field,
            &self.highlights,
            &variables,
            theme,
            active,
            field.text.is_empty() && !active,
        );
        div()
            .flex()
            .flex_col()
            .flex_none()
            .w_full()
            .min_h(px(EDITOR_LINE * heights as f32 + 12.))
            .px(px(SPACE_S))
            .py(px(SPACE_XS))
            .rounded(px(CONTROL_RADIUS))
            .border_1()
            .border_color(if active {
                theme.accent
            } else {
                theme.control_border
            })
            .when(field.text.is_empty() && !active, |this| {
                this.text_color(theme.text_muted).child(label.to_owned())
            })
            .when(!field.text.is_empty() || active, |this| {
                this.children(lines)
            })
    }
}

/// Replaces in the files that are not open: all of them change or none does.
fn replace_on_disk(
    support: &Arc<dyn LanguageSupport>,
    query: &StructuralQuery,
    paths: &[PathBuf],
) -> Result<usize, String> {
    let Some(provider) = support.structural_search() else {
        return Err("The language of the project has no structural search".to_owned());
    };
    let mut store = workspace_core::DocumentStore::new();
    let mut transaction =
        workspace_core::WorkspaceTransaction::new(workspace_core::EditSource::User);
    let mut documents = Vec::new();
    let mut count = 0;
    for path in paths {
        let file = workspace_core::LoadedFile::read(path)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        let document = store.open(file);
        let Some(state) = store.state(document) else {
            continue;
        };
        let text = text_core::TextStore::text(&state.text);
        let edits = provider.replace(&text, query)?;
        count += edits.len();
        let edits: Vec<text_core::Edit> = edits
            .into_iter()
            .map(|edit| text_core::Edit::new(edit.start..edit.end, edit.replacement))
            .collect();
        transaction = transaction.edit_at(document, state.version, edits);
        documents.push(document);
    }
    store
        .apply(transaction)
        .map_err(|error| error.to_string())?;
    let requests: Vec<workspace_core::SaveRequest> = documents
        .into_iter()
        .filter_map(|document| store.save_request(document))
        .collect();
    workspace_core::SaveRequest::write_all(requests).map_err(|error| error.to_string())?;
    Ok(count)
}

/// The `$name$` variables of a text with the ranges they cover.
fn variable_ranges(text: &str) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut rest = 0;
    while let Some(at) = text[rest..].find('$') {
        let start = rest + at;
        let after = &text[start + 1..];
        let Some(end) = after.find('$') else {
            break;
        };
        let name = &after[..end];
        let valid = !name.is_empty()
            && name
                .chars()
                .next()
                .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
            && name
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '_');
        if valid {
            ranges.push(start..start + 1 + end + 1);
        }
        rest = start + 1 + end + 1;
    }
    ranges
}

fn kind_color(kind: HighlightKind, theme: &Theme) -> Hsla {
    match kind {
        HighlightKind::Keyword => theme.keyword,
        HighlightKind::String => theme.string,
        HighlightKind::Number => theme.number,
        HighlightKind::Comment | HighlightKind::DocComment => theme.comment,
        HighlightKind::Function => theme.function,
        HighlightKind::Field => theme.field,
        HighlightKind::Constant => theme.semantic_constant,
        HighlightKind::Annotation => theme.annotation,
        _ => theme.text,
    }
}

fn editor_lines(
    field: &TextField,
    highlights: &[HighlightSpan],
    variables: &[Range<usize>],
    theme: &Theme,
    active: bool,
    placeholder: bool,
) -> Vec<AnyElement> {
    let mut lines = Vec::new();
    let mut start = 0;
    loop {
        let end = field.text[start..]
            .find('\n')
            .map_or(field.text.len(), |at| start + at);
        lines.push(editor_line(
            field,
            start..end,
            highlights,
            variables,
            theme,
            active,
            placeholder,
        ));
        if end == field.text.len() {
            return lines;
        }
        start = end + 1;
    }
}

#[allow(clippy::too_many_arguments)]
fn editor_line(
    field: &TextField,
    line: Range<usize>,
    highlights: &[HighlightSpan],
    variables: &[Range<usize>],
    theme: &Theme,
    active: bool,
    placeholder: bool,
) -> AnyElement {
    if placeholder {
        return div().h(px(EDITOR_LINE)).into_any_element();
    }
    let mut boundaries: Vec<usize> = vec![line.start, line.end];
    for span in highlights {
        let span = span.start as usize..span.end as usize;
        if span.start < line.end && span.end > line.start {
            boundaries.push(span.start.clamp(line.start, line.end));
            boundaries.push(span.end.clamp(line.start, line.end));
        }
    }
    for variable in variables {
        if variable.start < line.end && variable.end > line.start {
            boundaries.push(variable.start.clamp(line.start, line.end));
            boundaries.push(variable.end.clamp(line.start, line.end));
        }
    }
    let selection = field.selection();
    if selection.start < line.end && selection.end > line.start {
        boundaries.push(selection.start.clamp(line.start, line.end));
        boundaries.push(selection.end.clamp(line.start, line.end));
    }
    if field.caret() > line.start && field.caret() < line.end {
        boundaries.push(field.caret());
    }
    boundaries.sort_unstable();
    boundaries.dedup();
    let mut parts: Vec<(Range<usize>, Hsla, bool)> = Vec::new();
    for pair in boundaries.windows(2) {
        let (start, end) = (pair[0], pair[1]);
        if start >= end {
            continue;
        }
        let color = if variables
            .iter()
            .any(|variable| variable.start <= start && end <= variable.end)
        {
            theme.accent
        } else {
            highlights
                .iter()
                .find(|span| span.start as usize <= start && end <= span.end as usize)
                .map_or(theme.text, |span| kind_color(span.kind, theme))
        };
        let selected = selection.start <= start && end <= selection.end;
        match parts.last_mut() {
            Some((range, last, was_selected))
                if *last == color && *was_selected == selected && range.end == start =>
            {
                range.end = end;
            }
            _ => parts.push((start..end, color, selected)),
        }
    }
    let caret_in_line = active && field.caret() >= line.start && field.caret() <= line.end;
    let mut children: Vec<AnyElement> = Vec::new();
    let mut caret_placed = false;
    for (range, color, selected) in parts {
        if caret_in_line && !caret_placed && field.caret() <= range.start {
            children.push(caret(theme));
            caret_placed = true;
        }
        let split = if caret_in_line && !caret_placed && field.caret() < range.end {
            field.caret()
        } else {
            range.end
        };
        if split > range.start {
            children.push(
                div()
                    .whitespace_nowrap()
                    .text_color(color)
                    .when(selected, |this| this.bg(theme.selection))
                    .child(field.text[range.start..split].to_owned())
                    .into_any_element(),
            );
        }
        if caret_in_line && !caret_placed && field.caret() == split {
            children.push(caret(theme));
            caret_placed = true;
        }
        if range.end > split {
            children.push(
                div()
                    .whitespace_nowrap()
                    .text_color(color)
                    .when(selected, |this| this.bg(theme.selection))
                    .child(field.text[split..range.end].to_owned())
                    .into_any_element(),
            );
        }
    }
    if caret_in_line && !caret_placed {
        children.push(caret(theme));
    }
    div()
        .flex()
        .items_center()
        .flex_none()
        .h(px(EDITOR_LINE))
        .whitespace_nowrap()
        .children(children)
        .into_any_element()
}

fn caret(theme: &Theme) -> AnyElement {
    div()
        .flex_none()
        .w(px(1.5))
        .h(px(EDITOR_LINE))
        .bg(theme.caret)
        .into_any_element()
}

fn one_line(text: &str) -> String {
    let line = text
        .split('\n')
        .map(str::trim)
        .find(|line| !line.is_empty());
    let text = line.unwrap_or("").to_owned();
    let mut shortened: String = text.chars().take(120).collect();
    if text.chars().count() > 120 {
        shortened.push('…');
    }
    shortened
}

impl Render for StructuralSearch {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let viewport = window.viewport_size();
        let width = px(WIDTH).min(viewport.width - px(40.));
        let height = px(HEIGHT).min(viewport.height - px(40.));
        let left = (viewport.width - width) / 2.;
        let top = (viewport.height - height) / 2.;

        let template_rows: Vec<AnyElement> = self
            .templates
            .iter()
            .map(|template| {
                let id = format!("structural-template-{}", template.id);
                let name = template.name.clone();
                let description = template.description.clone();
                let chosen = template.pattern == self.template.text;
                div()
                    .id(SharedString::from(id.clone()))
                    .debug_selector({
                        let id = id.clone();
                        move || id.clone()
                    })
                    .flex()
                    .flex_none()
                    .items_center()
                    .h(px(22.))
                    .px(px(SPACE_S))
                    .rounded(px(ROW_RADIUS))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_color(if chosen { theme.accent } else { theme.text })
                    .hover(move |style| style.bg(theme.hover))
                    .tooltip(move |_, cx| tooltip(description.clone(), None, cx))
                    .child(name)
                    .on_click(cx.listener(move |this: &mut Self, _, _, cx| {
                        let template = this
                            .templates
                            .iter()
                            .find(|template| template.id == id)
                            .cloned();
                        if let Some(template) = template {
                            this.load_template(&template, cx);
                        }
                    }))
                    .into_any_element()
            })
            .collect();
        let history_rows: Vec<AnyElement> = self
            .history
            .iter()
            .enumerate()
            .map(|(index, pattern)| {
                let pattern = pattern.clone();
                div()
                    .id(("structural-history", index))
                    .debug_selector(move || format!("structural-history-{index}"))
                    .flex()
                    .flex_none()
                    .items_center()
                    .h(px(22.))
                    .px(px(SPACE_S))
                    .rounded(px(ROW_RADIUS))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_color(theme.text_secondary)
                    .hover(move |style| style.bg(theme.hover))
                    .child(one_line(&pattern))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.template = TextField::from_text(pattern.clone());
                        this.focus_at = FocusAt::Template;
                        this.highlight_for.clear();
                        this.refresh(None, cx);
                    }))
                    .into_any_element()
            })
            .collect();
        let left_column = div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(LIST_WIDTH))
            .h_full()
            .gap(px(SPACE_XS))
            .child(
                div()
                    .flex_none()
                    .h(px(20.))
                    .px(px(SPACE_S))
                    .text_color(theme.text_secondary)
                    .child("Ready templates"),
            )
            .child(
                scrollable(
                    "structural-templates",
                    &self.templates_scroll,
                    div()
                        .id("structural-templates")
                        .flex()
                        .flex_col()
                        .overflow_y_scroll()
                        .children(template_rows),
                    &theme,
                    window,
                    cx,
                )
                .flex_1()
                .min_h_0(),
            )
            .child(
                div()
                    .flex_none()
                    .h(px(20.))
                    .px(px(SPACE_S))
                    .text_color(theme.text_secondary)
                    .child("Recent templates"),
            )
            .child(
                scrollable(
                    "structural-history",
                    &self.history_scroll,
                    div()
                        .id("structural-history")
                        .flex()
                        .flex_col()
                        .overflow_y_scroll()
                        .children(history_rows),
                    &theme,
                    window,
                    cx,
                )
                .flex_none()
                .h(px(150.)),
            );

        let scopes: Vec<AnyElement> = StructuralScope::ALL
            .iter()
            .map(|scope| {
                let chosen = self.scope == *scope;
                let scope = *scope;
                div()
                    .id(scope.id())
                    .debug_selector(move || format!("structural-scope-{}", scope.id()))
                    .flex()
                    .items_center()
                    .h(px(22.))
                    .px(px(SPACE_S))
                    .rounded(px(ROW_RADIUS))
                    .border_1()
                    .border_color(if chosen {
                        theme.tab_selected_border
                    } else {
                        theme.control_border
                    })
                    .when(chosen, |this| this.bg(theme.tab_selected))
                    .text_color(if chosen {
                        theme.accent
                    } else {
                        theme.text_secondary
                    })
                    .child(scope.label())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.scope = scope;
                        this.refresh(Some(window), cx);
                    }))
                    .into_any_element()
            })
            .collect();

        let variable_rows: Vec<AnyElement> = self
            .variables
            .iter()
            .enumerate()
            .map(|(index, row)| {
                let text_active = self.focus_at == FocusAt::Variable(index, FieldKind::Text);
                let count_active = self.focus_at == FocusAt::Variable(index, FieldKind::Count);
                let type_active = self.focus_at == FocusAt::Variable(index, FieldKind::Type);
                let text = Self::input(
                    FieldMouse::new(cx, move |this| {
                        this.variables.get_mut(index).map(|row| &mut row.text)
                    }),
                    &row.text,
                    "text",
                    text_active,
                    move || format!("structural-var-{index}-text"),
                    None,
                    &theme,
                );
                let count = Self::input(
                    FieldMouse::new(cx, move |this| {
                        this.variables.get_mut(index).map(|row| &mut row.count)
                    }),
                    &row.count,
                    "[0,∞]",
                    count_active,
                    move || format!("structural-var-{index}-count"),
                    Some(92.),
                    &theme,
                );
                let type_filter = Self::input(
                    FieldMouse::new(cx, move |this| {
                        this.variables
                            .get_mut(index)
                            .map(|row| &mut row.type_filter)
                    }),
                    &row.type_filter,
                    "type",
                    type_active,
                    move || format!("structural-var-{index}-type"),
                    None,
                    &theme,
                );
                div()
                    .flex()
                    .items_center()
                    .flex_none()
                    .gap(px(SPACE_S))
                    .child(
                        div()
                            .w(px(110.))
                            .flex_none()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_color(theme.accent)
                            .child(format!("${}$", row.name)),
                    )
                    .child(
                        div()
                            .id(("structural-var-text", index))
                            .debug_selector(move || format!("structural-var-{index}-text-box"))
                            .flex()
                            .flex_1()
                            .min_w_0()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.focus_at = FocusAt::Variable(index, FieldKind::Text);
                                cx.notify();
                            }))
                            .child(text),
                    )
                    .child(count)
                    .child(
                        div()
                            .id(("structural-var-type", index))
                            .flex()
                            .flex_1()
                            .min_w_0()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.focus_at = FocusAt::Variable(index, FieldKind::Type);
                                cx.notify();
                            }))
                            .child(type_filter),
                    )
                    .into_any_element()
            })
            .collect();

        let template_editor = div()
            .id("structural-template-field")
            .debug_selector(|| "structural-template-field".to_owned())
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(2.))
            .on_click(cx.listener(|this, _, _, cx| {
                this.focus_at = FocusAt::Template;
                cx.notify();
            }))
            .child(self.editor(
                &self.template,
                "Search template",
                self.focus_at == FocusAt::Template,
                3,
                &theme,
            ));
        let replacement_editor = self.replace_mode.then(|| {
            div()
                .id("structural-replace-field")
                .debug_selector(|| "structural-replace-field".to_owned())
                .flex()
                .flex_col()
                .flex_none()
                .gap(px(2.))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.focus_at = FocusAt::Replacement;
                    cx.notify();
                }))
                .child(self.editor(
                    &self.replacement,
                    "Replace template, with the same variables",
                    self.focus_at == FocusAt::Replacement,
                    2,
                    &theme,
                ))
        });

        let message = self
            .error
            .clone()
            .or_else(|| self.status.clone())
            .unwrap_or_default();
        let status_color = if self.error.is_some() {
            theme.stripe_error
        } else {
            theme.text_secondary
        };
        let replace_button = self.replace_mode.then(|| {
            div()
                .id("structural-replace-all")
                .debug_selector(|| "structural-replace-all".to_owned())
                .flex()
                .items_center()
                .h(px(24.))
                .px(px(SPACE_M))
                .rounded(px(CONTROL_RADIUS))
                .border_1()
                .border_color(theme.control_border)
                .hover(move |style| style.bg(theme.hover))
                .child("Replace All")
                .on_click(cx.listener(|this, _, _, cx| this.replace_all(cx)))
        });

        let title: SharedString = if self.replace_mode {
            "Replace Structurally"
        } else {
            "Search Structurally"
        }
        .into();
        let shell = surface(&theme)
            .id("structural-search")
            .debug_selector(|| "structural-search".to_owned())
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event, window, cx| {
                this.key(event, window, cx);
            }))
            .on_action(
                cx.listener(|this, _: &workspace::Dismiss, window, cx| this.dismiss(window, cx)),
            );
        let contents = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .p(px(POPUP_PADDING))
            .gap(px(POPUP_PADDING))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(SPACE_M))
                    .child(div().flex_1())
                    .children(replace_button),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .gap(px(SPACE_L))
                    .child(left_column)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .gap(px(SPACE_S))
                            .child(
                                div()
                                    .flex()
                                    .flex_none()
                                    .items_center()
                                    .gap(px(SPACE_XS))
                                    .children(scopes),
                            )
                            .child(template_editor)
                            .children(replacement_editor)
                            .child(
                                div()
                                    .flex_none()
                                    .h(px(20.))
                                    .text_color(theme.text_secondary)
                                    .child("Variables"),
                            )
                            .child(
                                scrollable(
                                    "structural-variables",
                                    &self.variables_scroll,
                                    div()
                                        .id("structural-variables")
                                        .flex()
                                        .flex_col()
                                        .overflow_y_scroll()
                                        .gap(px(SPACE_XS))
                                        .children(variable_rows),
                                    &theme,
                                    window,
                                    cx,
                                )
                                .flex_1()
                                .min_h_0(),
                            )
                            .child(
                                div()
                                    .id("structural-status")
                                    .debug_selector(|| "structural-status".to_owned())
                                    .flex()
                                    .flex_none()
                                    .h(px(22.))
                                    .text_color(status_color)
                                    .child(message),
                            ),
                    ),
            );

        let close = cx.listener(|this, _: &ClickEvent, window, cx| this.dismiss(window, cx));
        dialog_window(
            DialogSpec {
                key: "structural-search",
                title,
                extra: None,
                place: DialogPlace {
                    left: f32::from(left),
                    top: f32::from(top),
                    width: f32::from(width),
                    height: Some(f32::from(height)),
                },
                min: (480., 320.),
            },
            shell,
            window,
            cx,
            close,
            contents,
        )
    }
}
