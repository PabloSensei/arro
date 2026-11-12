//! Inspect Code, Run Inspection by Name and the Inspection Results window. The source
//! is the diagnostics of the language server over the files of a scope; a server that
//! publishes only open documents is asked file by file in the background.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ::base::Cancel;
use ::platform::CanonPath;
use editor_view::{EditorView, Semantic};
use futures_util::StreamExt;
use lang_api::{
    ActionEntry, ActionKind, ActionQuery, DiagnosticNote, DiagnosticsQuery, DocumentEdit,
    FormatQuery, PrepareDiagnosticsQuery, Severity, SourceEdit, TextPoint, TextSpan,
    WorkspaceChange, WorkspaceDiagnosticsQuery, ask,
};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::{INSPECTION_TOOL, RerunInspection, Workspace};

use crate::git::ActiveGit;
use crate::items::open_at;

const GROUP: &str = "bottom-panel";
const WIDTH: f32 = 560.;

/// How many closed files the inspection opens on the server at once.
const OPEN_BATCH: usize = 50;
/// How long one opened batch may take to publish its diagnostics.
const BATCH_WAIT: Duration = Duration::from_secs(30);

/// One diagnostic of the results, with its place in a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    pub path: PathBuf,
    pub line: usize,
    pub column: usize,
    pub end_line: usize,
    pub end_character: usize,
    pub severity: Severity,
    pub message: String,
    /// The code of the problem the server names, when it names one.
    pub code: Option<String>,
    /// The short title of the kind of the problem, for the groups of the tree.
    pub kind: String,
    /// A quick fix was applied: the tree shows it as fixed until the next run.
    pub fixed: bool,
}

impl Finding {
    fn from_note(
        path: &Path,
        note: &DiagnosticNote,
        support: &dyn lang_api::LanguageSupport,
    ) -> Self {
        Self {
            path: path.to_path_buf(),
            line: note.span.start.line as usize,
            column: note.span.start.character as usize,
            end_line: note.span.end.line as usize,
            end_character: note.span.end.character as usize,
            severity: note.severity,
            message: note.message.clone(),
            code: note.code.clone(),
            kind: support.problem_kind(note.code.as_deref(), &note.message),
            fixed: false,
        }
    }

    fn span(&self) -> TextSpan {
        TextSpan {
            start: TextPoint {
                line: self.line as u32,
                character: self.column as u32,
            },
            end: TextPoint {
                line: self.end_line as u32,
                character: self.end_character as u32,
            },
        }
    }

    /// The same problem of the same place, for a result that repeats after a recheck.
    fn same_problem(&self, other: &Finding) -> bool {
        self.path == other.path
            && self.line == other.line
            && self.column == other.column
            && self.message == other.message
    }
}

fn sort_findings(findings: &mut [Finding]) {
    findings.sort_by_key(|finding| {
        (
            finding.severity,
            finding.path.clone(),
            finding.line,
            finding.column,
        )
    });
}

fn severity_label(severity: Severity) -> &'static str {
    match severity {
        Severity::Error => "Error",
        Severity::Warning => "Warning",
        Severity::Information => "Information",
        Severity::Hint => "Hint",
    }
}

fn severity_icon(severity: Severity) -> &'static str {
    match severity {
        Severity::Error => "status/error",
        Severity::Warning => "status/warning",
        Severity::Information | Severity::Hint => "status/info",
    }
}

/// The part of the project an inspection runs on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Scope {
    Project,
    Module(PathBuf),
    CurrentFile(PathBuf),
    Uncommitted,
    Directory(PathBuf),
    /// The files the user chose, such as the files of a commit.
    Files(Vec<PathBuf>),
}

/// What a scope dialog starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    Inspect(Option<String>),
    Cleanup,
}

/// One row of the scope dialog.
#[derive(Clone)]
struct ScopeOption {
    scope: Scope,
    label: String,
    detail: String,
    enabled: bool,
}

struct RunState {
    cancel: Cancel,
    label: String,
    filter: Option<String>,
}

/// Keeps the last results, the running inspection and the window that lists them.
pub struct InspectHub {
    root: PathBuf,
    files: WeakEntity<Arc<Vec<PathBuf>>>,
    workspace: WeakEntity<Workspace>,
    window: AnyWindowHandle,
    panel: Entity<InspectPanel>,
    findings: Vec<Finding>,
    run: Option<RunState>,
    kinds: Vec<String>,
    /// The last finished run, so that Rerun repeats the same area and mode.
    last: Option<(Mode, Scope)>,
}

/// The hub of the window, for scripts and tests.
pub struct ActiveInspect(pub Entity<InspectHub>);

impl Global for ActiveInspect {}

impl InspectHub {
    pub fn create(
        root: PathBuf,
        files: WeakEntity<Arc<Vec<PathBuf>>>,
        workspace: WeakEntity<Workspace>,
        window: AnyWindowHandle,
        cx: &mut App,
    ) -> Entity<Self> {
        let weak = workspace.clone();
        let panel = InspectPanel::create(root.clone(), weak, cx);
        let hub = cx.new(|_| Self {
            root,
            files,
            workspace,
            window,
            panel: panel.clone(),
            findings: Vec::new(),
            run: None,
            kinds: Vec::new(),
            last: None,
        });
        panel.update(cx, |panel, _| panel.hub = Some(hub.downgrade()));
        hub
    }

    pub fn panel(&self) -> &Entity<InspectPanel> {
        &self.panel
    }

    /// The findings of the last inspection, for tests.
    pub fn findings(&self) -> &[Finding] {
        &self.findings
    }

    /// The problem kinds of the last results, for Run Inspection by Name.
    pub fn kinds(&self) -> Vec<String> {
        self.kinds.clone()
    }

    /// Whether there is a finished run that Rerun can repeat.
    pub fn can_rerun(&self) -> bool {
        self.last.is_some() && self.run.is_none()
    }

    /// Whether an inspection is still collecting its results.
    pub(crate) fn is_running(&self) -> bool {
        self.run.is_some()
    }

    /// Runs Inspect Code over exactly these files, for the commit analysis. `false` when
    /// there is nothing to inspect.
    pub(crate) fn start_files(&mut self, files: Vec<PathBuf>, cx: &mut Context<Self>) -> bool {
        if files.is_empty() {
            return false;
        }
        let supported = cx
            .try_global::<Semantic>()
            .is_some_and(|semantic| files.iter().any(|path| semantic.0.matches(path)));
        self.start(Mode::Inspect(None), Scope::Files(files), cx);
        supported && self.run.is_some()
    }

    fn options(&self, current: Option<PathBuf>, cx: &App) -> Vec<ScopeOption> {
        let mut options = vec![ScopeOption {
            scope: Scope::Project,
            label: "Whole project".to_owned(),
            detail: self.root.display().to_string(),
            enabled: true,
        }];
        if let Some(run) = cx.try_global::<crate::script::ActiveRunPanel>() {
            for module in run.0.read(cx).modules() {
                options.push(ScopeOption {
                    scope: Scope::Module(module.dir.clone()),
                    label: format!("Module {}", module.name),
                    detail: module.dir.display().to_string(),
                    enabled: true,
                });
            }
        }
        options.push(ScopeOption {
            scope: current.clone().map_or(Scope::Project, Scope::CurrentFile),
            label: "Current file".to_owned(),
            detail: current.clone().map_or_else(
                || "no file is open".to_owned(),
                |path| path.display().to_string(),
            ),
            enabled: current.is_some(),
        });
        let changed = self.changed_files(cx);
        options.push(ScopeOption {
            scope: Scope::Uncommitted,
            label: "Uncommitted files".to_owned(),
            detail: format!("{} files", changed.len()),
            enabled: !changed.is_empty(),
        });
        let folder = current
            .and_then(|path| path.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| self.root.clone());
        options.push(ScopeOption {
            scope: Scope::Directory(folder.clone()),
            label: "Directory".to_owned(),
            detail: folder.display().to_string(),
            enabled: true,
        });
        options
    }

    fn changed_files(&self, cx: &App) -> Vec<PathBuf> {
        let Some(git) = cx.try_global::<ActiveGit>().map(|git| git.0.clone()) else {
            return Vec::new();
        };
        let git = git.read(cx);
        git.status()
            .files
            .iter()
            .filter_map(|file| git.absolute(&file.path))
            .filter(|path| path.is_file())
            .collect()
    }

    fn files_of(&self, scope: &Scope, cx: &App) -> Vec<PathBuf> {
        let all: Arc<Vec<PathBuf>> = self
            .files
            .upgrade()
            .map(|files| files.read(cx).clone())
            .unwrap_or_default();
        match scope {
            Scope::Project => all.iter().cloned().collect(),
            Scope::Module(folder) | Scope::Directory(folder) => all
                .iter()
                .filter(|path| path.starts_with(folder))
                .cloned()
                .collect(),
            Scope::CurrentFile(path) => vec![path.clone()],
            Scope::Uncommitted => self.changed_files(cx),
            Scope::Files(files) => files.clone(),
        }
    }

    fn scope_label(&self, scope: &Scope) -> String {
        match scope {
            Scope::Project => "Whole project".to_owned(),
            Scope::Module(folder) | Scope::Directory(folder) => folder.display().to_string(),
            Scope::CurrentFile(path) => path.display().to_string(),
            Scope::Uncommitted => "Uncommitted files".to_owned(),
            Scope::Files(files) => format!("{} chosen files", files.len()),
        }
    }

    /// Starts an inspection or a cleanup over the files of `scope`.
    pub(crate) fn start(&mut self, mode: Mode, scope: Scope, cx: &mut Context<Self>) {
        if let Some(run) = self.run.take() {
            run.cancel.cancel();
        }
        let Some(support) = cx
            .try_global::<Semantic>()
            .map(|semantic| semantic.0.clone())
        else {
            return;
        };
        self.last = Some((mode.clone(), scope.clone()));
        let files: Vec<PathBuf> = self
            .files_of(&scope, cx)
            .into_iter()
            .filter(|path| support.matches(path))
            .collect();
        let label = self.scope_label(&scope);
        let filter = match &mode {
            Mode::Inspect(code) => code.clone(),
            Mode::Cleanup => None,
        };
        let cancel = Cancel::new();
        self.findings.clear();
        self.panel.update(cx, |panel, cx| {
            panel.set_findings(Vec::new(), cx);
            panel.set_progress(
                Some(Progress {
                    done: 0,
                    total: files.len(),
                    label: label.clone(),
                    message: None,
                }),
                cx,
            );
        });
        self.panel.update(cx, |panel, cx| {
            let title = match &mode {
                Mode::Inspect(_) => "Inspection Results".to_owned(),
                Mode::Cleanup => "Code Cleanup".to_owned(),
            };
            panel.set_title(title, cx);
        });
        if files.is_empty() {
            self.panel.update(cx, |panel, cx| {
                panel.set_progress(None, cx);
                panel.set_title("Inspection Results (0 problems)".to_owned(), cx);
            });
            return;
        }
        self.run = Some(RunState {
            cancel: cancel.clone(),
            label: label.clone(),
            filter,
        });
        let total = files.len();
        let prepare = matches!(
            &scope,
            Scope::Project | Scope::Module(_) | Scope::Directory(_)
        );
        match &mode {
            Mode::Inspect(_) => self.start_inspect(support, files, total, prepare, cancel, cx),
            Mode::Cleanup => self.start_cleanup(support, files, total, cancel, cx),
        }
    }

    /// Repeats the last finished inspection or cleanup over the same area.
    pub(crate) fn rerun(&mut self, cx: &mut Context<Self>) {
        if let Some((mode, scope)) = self.last.clone() {
            self.start(mode, scope, cx);
        }
    }

    fn start_inspect(
        &mut self,
        support: Arc<dyn lang_api::LanguageSupport>,
        files: Vec<PathBuf>,
        total: usize,
        prepare_scope: bool,
        cancel: Cancel,
        cx: &mut Context<Self>,
    ) {
        let support = support.clone();
        let weak = cx.weak_entity();
        cx.spawn(async move |_, cx| {
            let workspace_provider = support.workspace_diagnostics();
            let per_file = support.diagnostics();
            let server = support.server();
            let prepare = if prepare_scope {
                support.prepare_diagnostics()
            } else {
                None
            };
            if let Some(prepare) = prepare {
                weak.update(cx, |hub, cx| {
                    hub.panel.update(cx, |panel, cx| {
                        panel.set_progress(
                            Some(Progress {
                                done: 0,
                                total,
                                label: "Building workspace".to_owned(),
                                message: None,
                            }),
                            cx,
                        )
                    })
                })
                .ok();
                let mut replies = ask(prepare, PrepareDiagnosticsQuery);
                loop {
                    if replies.poll_ready().is_some() {
                        break;
                    }
                    if cancel.is_cancelled() {
                        return;
                    }
                    cx.background_executor()
                        .timer(Duration::from_millis(100))
                        .await;
                }
            }
            let mut known: std::collections::HashMap<String, Vec<DiagnosticNote>> =
                std::collections::HashMap::new();
            if let Some(provider) = workspace_provider {
                let mut replies = ask(provider, WorkspaceDiagnosticsQuery);
                if let Some(answer) = replies.next().await {
                    for file in answer.value {
                        known.insert(file.uri, file.diagnostics);
                    }
                }
            }
            // A server that publishes only open documents has nothing for a closed
            // file. Opening the files of the scope in small batches asks for their
            // diagnostics, and each batch closes again before the next one opens: a
            // large project never holds the whole scope open on the server.
            if prepare_scope && server.is_some() && per_file.is_some() {
                let mut missing = Vec::new();
                for path in &files {
                    let Ok(uri) = CanonPath::new(path) else {
                        continue;
                    };
                    let uri = uri.to_uri();
                    // A document the editor holds open stays the editor's: opening it
                    // again and closing it below would end its sync with the server.
                    if server.is_some_and(|server| server.version(&uri).is_some()) {
                        continue;
                    }
                    // The build answered for this file already, even with an empty
                    // list: asking the server again would only cost a round of work.
                    if known.contains_key(&uri) {
                        continue;
                    }
                    missing.push((path.clone(), uri));
                }
                let pending = missing.len();
                let mut done = 0;
                for batch in missing.chunks(OPEN_BATCH) {
                    if cancel.is_cancelled() {
                        break;
                    }
                    let mut opened = Vec::new();
                    for (path, uri) in batch {
                        if cancel.is_cancelled() {
                            break;
                        }
                        let target = path.clone();
                        let text = cx
                            .background_executor()
                            .spawn(async move { std::fs::read_to_string(&target) })
                            .await
                            .unwrap_or_default();
                        if let Some(server) = server {
                            server.open(uri, 1, text.into());
                            opened.push(uri.clone());
                        }
                    }
                    let started = Instant::now();
                    while started.elapsed() < BATCH_WAIT {
                        // Every opened file answered, with problems or with none.
                        if cancel.is_cancelled()
                            || batch.iter().all(|(_, uri)| known.contains_key(uri))
                        {
                            break;
                        }
                        if let Some(provider) = workspace_provider {
                            let mut replies = ask(provider, WorkspaceDiagnosticsQuery);
                            if let Some(answer) = replies.next().await {
                                for file in answer.value {
                                    known.insert(file.uri, file.diagnostics);
                                }
                            }
                        }
                        cx.background_executor()
                            .timer(Duration::from_millis(100))
                            .await;
                    }
                    if let Some(server) = server {
                        for uri in &opened {
                            server.close(uri);
                        }
                    }
                    done += batch.len();
                    weak.update(cx, |hub, cx| {
                        hub.panel.update(cx, |panel, cx| {
                            panel.set_progress(
                                Some(Progress {
                                    done,
                                    total: pending,
                                    label: String::new(),
                                    message: Some("Opening closed files".to_owned()),
                                }),
                                cx,
                            )
                        })
                    })
                    .ok();
                }
            }
            let mut findings = Vec::new();
            for (done, path) in files.into_iter().enumerate() {
                if cancel.is_cancelled() {
                    break;
                }
                let uri = CanonPath::new(&path).ok().map(|path| path.to_uri());
                let cached = uri
                    .as_ref()
                    .and_then(|uri| known.remove(uri))
                    .filter(|notes| !notes.is_empty());
                let notes: Vec<DiagnosticNote> = match cached {
                    Some(notes) => notes,
                    None => match (per_file, uri) {
                        (Some(provider), Some(uri)) => {
                            let mut replies = ask(provider, DiagnosticsQuery(uri));
                            let mut found = Vec::new();
                            if let Some(answer) = replies.next().await {
                                found = answer
                                    .value
                                    .iter()
                                    .map(|entry| DiagnosticNote {
                                        span: entry.span,
                                        severity: entry.severity,
                                        message: entry.message.clone(),
                                        code: None,
                                    })
                                    .collect();
                            }
                            found
                        }
                        _ => Vec::new(),
                    },
                };
                for note in &notes {
                    findings.push(Finding::from_note(&path, note, support.as_ref()));
                }
                weak.update(cx, |hub, cx| hub.progress(done + 1, total, cx))
                    .ok();
            }
            let cancelled = cancel.is_cancelled();
            weak.update(cx, |hub, cx| {
                if cancelled {
                    hub.stop(cx);
                } else {
                    hub.finish_inspect(findings, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    fn start_cleanup(
        &mut self,
        support: Arc<dyn lang_api::LanguageSupport>,
        files: Vec<PathBuf>,
        total: usize,
        cancel: Cancel,
        cx: &mut Context<Self>,
    ) {
        let workspace = self.workspace.clone();
        let window = self.window;
        let weak = cx.weak_entity();
        cx.spawn(async move |_, cx| {
            let actions = support.actions();
            let format = support.format();
            let server = support.server();
            let mut changed = 0;
            for (done, path) in files.into_iter().enumerate() {
                if cancel.is_cancelled() {
                    break;
                }
                let Ok(uri) = CanonPath::new(&path) else {
                    continue;
                };
                let uri = uri.to_uri();
                // An open document answers at its own version. A closed file is opened
                // on the server for the time of the request, with its text from disk:
                // version zero belongs to no document and the server refuses it.
                let open_document = workspace
                    .read_with(cx, |workspace, cx| {
                        workspace
                            .view_of(&path)
                            .and_then(|view| view.downcast::<EditorView>().ok())
                            .and_then(|view| {
                                let view = view.read(cx);
                                let version = view.version()?;
                                Some((version, view.text()?))
                            })
                    })
                    .ok()
                    .flatten();
                let (version, temporary, text) = match open_document {
                    Some((version, text)) => (version, false, text),
                    None => {
                        let target = path.clone();
                        let text = cx
                            .background_executor()
                            .spawn(async move { std::fs::read_to_string(&target) })
                            .await
                            .unwrap_or_default();
                        let mut opened = false;
                        if let Some(server) = server {
                            server.open(&uri, 1, text.clone().into());
                            opened = true;
                        }
                        (if opened { 1 } else { 0 }, opened, text)
                    }
                };
                let open_options = workspace
                    .read_with(cx, |workspace, cx| {
                        workspace
                            .view_of(&path)
                            .and_then(|view| view.downcast::<EditorView>().ok())
                            .map(|view| view.read(cx).formatting_options())
                    })
                    .ok()
                    .flatten();
                let (tab_size, insert_spaces) = match open_options {
                    Some(options) => options,
                    None => {
                        let path = path.clone();
                        cx.background_executor()
                            .spawn(async move { formatting_options_of(&path) })
                            .await
                    }
                };
                let span = whole_of(&text);
                let mut edits = Vec::new();
                if let Some(provider) = actions {
                    let mut replies = ask(
                        provider,
                        ActionQuery {
                            uri: uri.clone(),
                            version,
                            span,
                            only: Some(ActionKind::FixAll),
                        },
                    );
                    if let Some(answer) = replies.next().await
                        && let Some(entry) = answer.value.into_iter().next()
                    {
                        edits = edits_of(entry, &uri);
                    }
                    if edits.is_empty() {
                        let mut organized = Vec::new();
                        let mut replies = ask(
                            provider,
                            ActionQuery {
                                uri: uri.clone(),
                                version,
                                span,
                                only: Some(ActionKind::OrganizeImports),
                            },
                        );
                        if let Some(answer) = replies.next().await
                            && let Some(entry) = answer.value.into_iter().next()
                        {
                            organized = edits_of(entry, &uri);
                        }
                        let mut formatted = Vec::new();
                        if let Some(provider) = format {
                            let mut replies = ask(
                                provider,
                                FormatQuery {
                                    uri: uri.clone(),
                                    version,
                                    tab_size,
                                    insert_spaces,
                                },
                            );
                            if let Some(answer) = replies.next().await {
                                formatted = answer.value;
                            }
                        }
                        edits = merge_edits(organized.into_iter().chain(formatted).collect());
                    }
                } else if edits.is_empty()
                    && let Some(provider) = format
                {
                    let mut replies = ask(
                        provider,
                        FormatQuery {
                            uri: uri.clone(),
                            version,
                            tab_size,
                            insert_spaces,
                        },
                    );
                    if let Some(answer) = replies.next().await {
                        edits = answer.value;
                    }
                }
                if !edits.is_empty() {
                    changed += 1;
                    let change = WorkspaceChange {
                        edits: vec![SourceEdit {
                            uri: uri.clone(),
                            edits,
                        }],
                        renames: Vec::new(),
                    };
                    let applied = window
                        .update(cx, |_, window, cx| {
                            workspace
                                .update(cx, |workspace, cx| {
                                    crate::changes::apply(workspace, change, window, cx);
                                    true
                                })
                                .unwrap_or(false)
                        })
                        .unwrap_or(false);
                    if !applied {
                        changed -= 1;
                    }
                }
                if temporary && let Some(server) = server {
                    server.close(&uri);
                }
                weak.update(cx, |hub, cx| hub.progress(done + 1, total, cx))
                    .ok();
            }
            let cancelled = cancel.is_cancelled();
            weak.update(cx, |hub, cx| {
                if cancelled {
                    hub.stop(cx);
                } else {
                    hub.finish_cleanup(changed, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    fn progress(&mut self, done: usize, total: usize, cx: &mut Context<Self>) {
        let label = if let Some(run) = &mut self.run {
            run.label.clone()
        } else {
            String::new()
        };
        self.panel.update(cx, |panel, cx| {
            panel.set_progress(
                Some(Progress {
                    done,
                    total,
                    label,
                    message: None,
                }),
                cx,
            )
        });
    }

    fn finish_inspect(&mut self, mut findings: Vec<Finding>, cx: &mut Context<Self>) {
        let filter = self.run.as_ref().and_then(|run| run.filter.clone());
        if let Some(kind) = filter {
            findings.retain(|finding| finding.kind == kind);
        }
        sort_findings(&mut findings);
        let mut kinds: Vec<String> = findings
            .iter()
            .map(|finding| finding.kind.clone())
            .collect();
        kinds.sort();
        kinds.dedup();
        self.kinds = kinds;
        self.findings = findings;
        self.run = None;
        let count = self.findings.len();
        let shown = self.findings.clone();
        self.panel.update(cx, |panel, cx| {
            panel.set_progress(None, cx);
            panel.set_findings(shown, cx);
            panel.set_title(format!("Inspection Results ({count} problems)"), cx);
        });
        cx.notify();
    }

    fn finish_cleanup(&mut self, changed: usize, cx: &mut Context<Self>) {
        self.run = None;
        self.panel.update(cx, |panel, cx| {
            panel.set_progress(None, cx);
            panel.set_title(format!("Code Cleanup ({changed} files changed)"), cx);
        });
        if let Some(workspace) = self.workspace.upgrade() {
            let text = format!("Changed {changed} files");
            workspace.update(cx, |workspace, cx| {
                workspace.notify_user("Code Cleanup", text, cx)
            });
        }
        cx.notify();
    }

    /// Marks the findings a quick fix changed as fixed, as IDEA does, and asks the
    /// server for the diagnostics of their files again in the background. A problem
    /// that is still there after the fix replaces nothing: the fixed row stays until
    /// the next run.
    fn mark_fixed_and_recheck(
        &mut self,
        problems: Vec<Finding>,
        support: Arc<dyn lang_api::LanguageSupport>,
        cx: &mut Context<Self>,
    ) {
        if problems.is_empty() {
            return;
        }
        for finding in &mut self.findings {
            if problems.iter().any(|problem| problem.same_problem(finding)) {
                finding.fixed = true;
            }
        }
        let mut files: Vec<PathBuf> = Vec::new();
        for problem in &problems {
            if !files.contains(&problem.path) {
                files.push(problem.path.clone());
            }
        }
        let shown = self.findings.clone();
        self.panel
            .update(cx, |panel, cx| panel.set_findings(shown, cx));
        let weak = cx.weak_entity();
        cx.spawn(async move |_, cx| {
            // The editor sends its change first; the server needs a moment to publish.
            cx.background_executor()
                .timer(Duration::from_millis(250))
                .await;
            let workspace_provider = support.workspace_diagnostics();
            let per_file = support.diagnostics();
            let mut known: HashMap<String, Vec<DiagnosticNote>> = HashMap::new();
            if let Some(provider) = workspace_provider {
                let mut replies = ask(provider, WorkspaceDiagnosticsQuery);
                if let Some(answer) = replies.next().await {
                    for file in answer.value {
                        known.insert(file.uri, file.diagnostics);
                    }
                }
            }
            for path in files {
                let Ok(uri) = CanonPath::new(&path) else {
                    continue;
                };
                let uri = uri.to_uri();
                let notes = match known.remove(&uri) {
                    Some(notes) => notes,
                    None => match per_file {
                        Some(provider) => {
                            let mut replies = ask(provider, DiagnosticsQuery(uri));
                            let mut found = Vec::new();
                            if let Some(answer) = replies.next().await {
                                found = answer
                                    .value
                                    .iter()
                                    .map(|entry| DiagnosticNote {
                                        span: entry.span,
                                        severity: entry.severity,
                                        message: entry.message.clone(),
                                        code: None,
                                    })
                                    .collect();
                            }
                            found
                        }
                        None => Vec::new(),
                    },
                };
                weak.update(cx, |hub, cx| {
                    hub.replace_file_findings(&path, notes, support.as_ref(), cx)
                })
                .ok();
            }
        })
        .detach();
    }

    /// Takes the new diagnostics of one file, keeping the fixed rows of the file.
    fn replace_file_findings(
        &mut self,
        path: &Path,
        notes: Vec<DiagnosticNote>,
        support: &dyn lang_api::LanguageSupport,
        cx: &mut Context<Self>,
    ) {
        let fixed: Vec<Finding> = self
            .findings
            .iter()
            .filter(|finding| finding.path == path && finding.fixed)
            .cloned()
            .collect();
        self.findings
            .retain(|finding| finding.path != path || finding.fixed);
        for note in &notes {
            let finding = Finding::from_note(path, note, support);
            if fixed.iter().any(|known| known.same_problem(&finding)) {
                continue;
            }
            self.findings.push(finding);
        }
        sort_findings(&mut self.findings);
        let mut kinds: Vec<String> = self
            .findings
            .iter()
            .map(|finding| finding.kind.clone())
            .collect();
        kinds.sort();
        kinds.dedup();
        self.kinds = kinds;
        let shown = self.findings.clone();
        self.panel
            .update(cx, |panel, cx| panel.set_findings(shown, cx));
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        if let Some(run) = self.run.take() {
            run.cancel.cancel();
        }
        self.panel.update(cx, |panel, cx| {
            panel.set_progress(None, cx);
            panel.set_title("Inspection Results".to_owned(), cx);
        });
        cx.notify();
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        self.stop(cx);
    }
}

/// The whole text of a document, from its first character to the end of its last line.
/// A server rejects a range that ends past the text, so the end comes from the text.
fn whole_of(text: &str) -> TextSpan {
    let line = text.lines().count().saturating_sub(1) as u32;
    let character = text
        .lines()
        .last()
        .map_or(0, |line| line.encode_utf16().count() as u32);
    TextSpan {
        start: TextPoint::default(),
        end: TextPoint { line, character },
    }
}

/// The tab size and the spaces flag of a file that is not open, from the
/// `.editorconfig` files around it. Runs off the UI thread.
fn formatting_options_of(path: &Path) -> (u32, bool) {
    text_core::for_file(path)
        .indentation()
        .map_or((4, true), |(tabs, size, _)| (size as u32, !tabs))
}

fn edits_of(entry: ActionEntry, uri: &str) -> Vec<lang_api::DocumentEdit> {
    let mut edits = Vec::new();
    for source in entry.edits {
        if source.uri == uri {
            edits.extend(source.edits);
        }
    }
    edits
}

/// A scope dialog, the first step of Inspect Code and Code Cleanup.
pub(crate) fn show_scope(
    hub: Entity<InspectHub>,
    mode: Mode,
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let options = hub.read(cx).options(workspace.active_path(), cx);
    let title = match &mode {
        Mode::Inspect(Some(code)) => format!("Inspect Code: {code}"),
        Mode::Inspect(None) => "Inspect Code".to_owned(),
        Mode::Cleanup => "Code Cleanup".to_owned(),
    };
    let weak = cx.weak_entity();
    let view = cx.new(|cx| ScopePopup {
        workspace: weak,
        hub: hub.downgrade(),
        focus: cx.focus_handle(),
        options,
        selected: 0,
        title,
        mode,
    });
    let focus = view.read(cx).focus.clone();
    workspace.show_overlay(view.into(), focus, window, cx);
}

/// Ctrl+Alt+Shift+I: the problem kinds of the last results and the kinds the language
/// knows before any run.
pub(crate) fn show_run_inspection(
    hub: Entity<InspectHub>,
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let mut kinds: Vec<String> = Vec::new();
    if let Some(support) = cx
        .try_global::<Semantic>()
        .map(|semantic| semantic.0.clone())
    {
        for kind in support.problem_kinds() {
            if !kinds.contains(&kind.name) {
                kinds.push(kind.name);
            }
        }
    }
    for kind in hub.read(cx).kinds() {
        if !kinds.contains(&kind) {
            kinds.push(kind);
        }
    }
    let weak = cx.weak_entity();
    let view = cx.new(|cx| RunInspectionPopup {
        workspace: weak,
        hub: hub.downgrade(),
        focus: cx.focus_handle(),
        field: TextField::default(),
        kinds,
        selected: 0,
    });
    let focus = view.read(cx).focus.clone();
    workspace.show_overlay(view.into(), focus, window, cx);
}

pub(crate) fn bind_commands(
    hub: Entity<InspectHub>,
    workspace: &mut Workspace,
    _cx: &mut Context<Workspace>,
) {
    let inspect = hub.clone();
    workspace.register_command(
        keymap::workspace::INSPECT_CODE,
        "Inspect Code...",
        move |workspace, window, cx| {
            show_scope(inspect.clone(), Mode::Inspect(None), workspace, window, cx)
        },
    );
    let run = hub.clone();
    workspace.register_command(
        keymap::workspace::RUN_INSPECTION,
        "Run Inspection by Name...",
        move |workspace, window, cx| show_run_inspection(run.clone(), workspace, window, cx),
    );
    let cleanup = hub;
    workspace.register_command(
        keymap::workspace::CODE_CLEANUP,
        "Code Cleanup...",
        move |workspace, window, cx| {
            show_scope(cleanup.clone(), Mode::Cleanup, workspace, window, cx)
        },
    );
}

struct ScopePopup {
    workspace: WeakEntity<Workspace>,
    hub: WeakEntity<InspectHub>,
    focus: FocusHandle,
    options: Vec<ScopeOption>,
    selected: usize,
    title: String,
    mode: Mode,
}

impl ScopePopup {
    fn pick(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(option) = self.options.get(index).cloned() else {
            return;
        };
        if !option.enabled {
            return;
        }
        self.selected = index;
        let hub = self.hub.clone();
        let mode = self.mode.clone();
        let scope = option.scope;
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.dismiss(window, cx);
                workspace.show_tool(INSPECTION_TOOL, window, cx);
                let _ = hub.update(cx, |hub, cx| hub.start(mode, scope, cx));
            })
            .ok();
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace
            .update(cx, |workspace, cx| workspace.dismiss(window, cx))
            .ok();
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "up" | "left" => {
                self.selected = self.selected.saturating_sub(1);
                cx.stop_propagation();
                cx.notify();
            }
            "down" | "right" => {
                self.selected = (self.selected + 1).min(self.options.len().saturating_sub(1));
                cx.stop_propagation();
                cx.notify();
            }
            "enter" => {
                self.pick(self.selected, window, cx);
                cx.stop_propagation();
            }
            "escape" => {
                self.close(window, cx);
                cx.stop_propagation();
            }
            _ => {}
        }
    }
}

impl Render for ScopePopup {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let left = (window.viewport_size().width - px(WIDTH)) / 2.;
        let top = window.viewport_size().height * 0.16;
        let rows: Vec<_> = self
            .options
            .iter()
            .enumerate()
            .map(|(index, option)| {
                let selected = index == self.selected;
                let color = if option.enabled {
                    theme.text
                } else {
                    theme.text_disabled
                };
                div()
                    .id(("inspect-scope", index))
                    .debug_selector(move || format!("inspect-scope-{index}"))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(12.))
                    .flex_none()
                    .h(px(ROW_HEIGHT))
                    .px(px(8.))
                    .rounded(px(CONTROL_RADIUS))
                    .text_color(color)
                    .when(selected && option.enabled, |this| this.bg(theme.selection))
                    .when(!selected && option.enabled, |this| {
                        this.hover(|style| style.bg(theme.selection_hover))
                    })
                    .child(SharedString::from(option.label.clone()))
                    .child(
                        div()
                            .text_color(theme.text_secondary)
                            .child(SharedString::from(option.detail.clone())),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| this.pick(index, window, cx)))
            })
            .collect();
        let body = surface(&theme)
            .id("inspect-scope-popup")
            .debug_selector(|| "inspect-scope-popup".to_owned())
            .track_focus(&self.focus)
            .occlude()
            .w(px(WIDTH))
            .p(px(POPUP_PADDING))
            .on_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)))
            .on_mouse_down_out(cx.listener(|this, _, window, cx| this.close(window, cx)))
            .child(
                div()
                    .px(px(8.))
                    .h(px(28.))
                    .flex()
                    .items_center()
                    .text_color(theme.text_secondary)
                    .child(SharedString::from(self.title.clone())),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .min_h_0()
                    .overflow_hidden()
                    .children(rows),
            );
        div()
            .absolute()
            .left(left)
            .top(top)
            .child(enter("inspect-scope-enter", body))
    }
}

struct RunInspectionPopup {
    workspace: WeakEntity<Workspace>,
    hub: WeakEntity<InspectHub>,
    focus: FocusHandle,
    field: TextField,
    kinds: Vec<String>,
    selected: usize,
}

impl RunInspectionPopup {
    fn matches(&self) -> Vec<String> {
        let needle = self.field.text.to_lowercase();
        self.kinds
            .iter()
            .filter(|kind| needle.is_empty() || kind.to_lowercase().contains(&needle))
            .cloned()
            .collect()
    }

    fn pick(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(kind) = self.matches().get(index).cloned() else {
            return;
        };
        let Some(hub) = self.hub.upgrade() else {
            return;
        };
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.dismiss(window, cx);
                show_scope(
                    hub.clone(),
                    Mode::Inspect(Some(kind.clone())),
                    workspace,
                    window,
                    cx,
                );
            })
            .ok();
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace
            .update(cx, |workspace, cx| workspace.dismiss(window, cx))
            .ok();
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "up" => {
                self.selected = self.selected.saturating_sub(1);
                cx.stop_propagation();
                cx.notify();
            }
            "down" => {
                self.selected = (self.selected + 1).min(self.matches().len().saturating_sub(1));
                cx.stop_propagation();
                cx.notify();
            }
            "enter" => {
                self.pick(self.selected, window, cx);
                cx.stop_propagation();
            }
            "escape" => {
                self.close(window, cx);
                cx.stop_propagation();
            }
            _ => {
                if self.field.key_with_clipboard(event, false, cx) {
                    self.selected = 0;
                    cx.stop_propagation();
                    cx.notify();
                }
            }
        }
    }
}

impl Render for RunInspectionPopup {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let left = (window.viewport_size().width - px(WIDTH)) / 2.;
        let top = window.viewport_size().height * 0.16;
        let rows: Vec<_> = self
            .matches()
            .into_iter()
            .enumerate()
            .map(|(index, code)| {
                let selected = index == self.selected;
                div()
                    .id(("run-inspection-row", index))
                    .debug_selector(move || format!("run-inspection-row-{index}"))
                    .flex()
                    .items_center()
                    .flex_none()
                    .h(px(ROW_HEIGHT))
                    .px(px(8.))
                    .rounded(px(CONTROL_RADIUS))
                    .when(selected, |this| this.bg(theme.selection))
                    .when(!selected, |this| {
                        this.hover(|style| style.bg(theme.selection_hover))
                    })
                    .child(SharedString::from(code))
                    .on_click(cx.listener(move |this, _, window, cx| this.pick(index, window, cx)))
            })
            .collect();
        let shown: SharedString = if self.field.text.is_empty() {
            if self.kinds.is_empty() {
                "No inspections known yet. Run Inspect Code first.".into()
            } else {
                "Inspection name".into()
            }
        } else {
            self.field.text.clone().into()
        };
        let body = surface(&theme)
            .id("run-inspection")
            .debug_selector(|| "run-inspection".to_owned())
            .track_focus(&self.focus)
            .occlude()
            .w(px(WIDTH))
            .p(px(POPUP_PADDING))
            .on_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)))
            .on_mouse_down_out(cx.listener(|this, _, window, cx| this.close(window, cx)))
            .child(
                div()
                    .mx(px(SPACE_S))
                    .my(px(SPACE_S))
                    .h(px(CONTROL_HEIGHT))
                    .px(px(SPACE_M))
                    .flex()
                    .items_center()
                    .rounded(px(CONTROL_RADIUS))
                    .border_2()
                    .border_color(theme.accent)
                    .text_color(if self.field.text.is_empty() {
                        theme.text_secondary
                    } else {
                        theme.text
                    })
                    .child(shown),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .min_h_0()
                    .overflow_hidden()
                    .children(rows),
            );
        div()
            .absolute()
            .left(left)
            .top(top)
            .child(enter("run-inspection-enter", body))
    }
}

#[derive(Clone)]
struct Progress {
    done: usize,
    total: usize,
    label: String,
    /// A phase of the run that is not the per-file pass, such as opening closed files.
    message: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RowKind {
    Severity,
    Kind,
    File,
    Finding(usize),
}

#[derive(Clone, Debug)]
pub(crate) struct Row {
    pub(crate) key: String,
    pub(crate) depth: usize,
    pub(crate) label: String,
    pub(crate) hint: String,
    pub(crate) icon: &'static str,
    pub(crate) expandable: bool,
    pub(crate) open: bool,
    pub(crate) fixed: bool,
    pub(crate) kind: RowKind,
}

pub struct InspectPanel {
    title: SharedString,
    root: PathBuf,
    workspace: WeakEntity<Workspace>,
    hub: Option<WeakEntity<InspectHub>>,
    focus: FocusHandle,
    scroll: ScrollHandle,
    findings: Vec<Finding>,
    rows: Vec<Row>,
    collapsed: HashSet<String>,
    selected: Vec<String>,
    anchor: Option<usize>,
    progress: Option<Progress>,
    menu_at: Point<Pixels>,
}

impl InspectPanel {
    fn create(root: PathBuf, workspace: WeakEntity<Workspace>, cx: &mut App) -> Entity<Self> {
        cx.new(|cx| Self {
            title: "Inspection Results".into(),
            root,
            workspace,
            hub: None,
            focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            findings: Vec::new(),
            rows: Vec::new(),
            collapsed: HashSet::new(),
            selected: Vec::new(),
            anchor: None,
            progress: None,
            menu_at: point(px(200.), px(200.)),
        })
    }

    pub fn focus(&self) -> FocusHandle {
        self.focus.clone()
    }

    #[cfg(test)]
    pub(crate) fn rows(&self) -> &[Row] {
        &self.rows
    }

    fn set_title(&mut self, title: String, cx: &mut Context<Self>) {
        self.title = title.into();
        cx.notify();
    }

    fn set_progress(&mut self, progress: Option<Progress>, cx: &mut Context<Self>) {
        self.progress = progress;
        cx.notify();
    }

    fn set_findings(&mut self, findings: Vec<Finding>, cx: &mut Context<Self>) {
        self.findings = findings;
        self.rebuild();
        self.selected
            .retain(|key| self.rows.iter().any(|row| &row.key == key));
        cx.notify();
    }

    fn rebuild(&mut self) {
        let mut rows = Vec::new();
        for severity in [
            Severity::Error,
            Severity::Warning,
            Severity::Information,
            Severity::Hint,
        ] {
            let mut indices: Vec<usize> = (0..self.findings.len())
                .filter(|index| self.findings[*index].severity == severity)
                .collect();
            if indices.is_empty() {
                continue;
            }
            indices.sort_by_key(|index| {
                let finding = &self.findings[*index];
                (finding.path.clone(), finding.line, finding.column)
            });
            let severity_key = format!("severity:{severity:?}");
            let open = !self.collapsed.contains(&severity_key);
            rows.push(Row {
                key: severity_key.clone(),
                depth: 0,
                label: severity_label(severity).to_owned(),
                hint: indices.len().to_string(),
                icon: severity_icon(severity),
                expandable: true,
                open,
                fixed: false,
                kind: RowKind::Severity,
            });
            if !open {
                continue;
            }
            let mut kinds: BTreeMap<String, Vec<usize>> = BTreeMap::new();
            for index in indices {
                kinds
                    .entry(self.findings[index].kind.clone())
                    .or_default()
                    .push(index);
            }
            for (label, indices) in kinds {
                let kind_key = format!("{severity_key}/kind:{label}");
                let open = !self.collapsed.contains(&kind_key);
                rows.push(Row {
                    key: kind_key.clone(),
                    depth: 1,
                    label: label.clone(),
                    hint: indices.len().to_string(),
                    icon: severity_icon(severity),
                    expandable: true,
                    open,
                    fixed: false,
                    kind: RowKind::Kind,
                });
                if !open {
                    continue;
                }
                let mut files: BTreeMap<PathBuf, Vec<usize>> = BTreeMap::new();
                for index in indices {
                    files
                        .entry(self.findings[index].path.clone())
                        .or_default()
                        .push(index);
                }
                for (path, indices) in files {
                    let relative = path.strip_prefix(&self.root).unwrap_or(&path);
                    let file_key = format!("{kind_key}/file:{}", path.display());
                    let open = !self.collapsed.contains(&file_key);
                    rows.push(Row {
                        key: file_key.clone(),
                        depth: 2,
                        label: relative.display().to_string().replace('\\', "/"),
                        hint: indices.len().to_string(),
                        icon: "fileTypes/text",
                        expandable: true,
                        open,
                        fixed: false,
                        kind: RowKind::File,
                    });
                    if !open {
                        continue;
                    }
                    for index in indices {
                        let finding = &self.findings[index];
                        rows.push(Row {
                            key: format!("finding:{index}"),
                            depth: 3,
                            label: finding.message.lines().next().unwrap_or("").to_owned(),
                            hint: format!("{}:{}", relative.display(), finding.line + 1),
                            icon: severity_icon(finding.severity),
                            expandable: false,
                            open: false,
                            fixed: finding.fixed,
                            kind: RowKind::Finding(index),
                        });
                    }
                }
            }
        }
        self.rows = rows;
    }

    fn toggle_row(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(row) = self.rows.get(index) else {
            return;
        };
        if !row.expandable {
            return;
        }
        let key = row.key.clone();
        if !self.collapsed.remove(&key) {
            self.collapsed.insert(key);
        }
        self.rebuild();
        cx.notify();
    }

    fn select(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(row) = self.rows.get(index) else {
            return;
        };
        self.selected = vec![row.key.clone()];
        self.anchor = Some(index);
        self.scroll.scroll_to_item(index);
        cx.notify();
    }

    fn toggle_selection(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(row) = self.rows.get(index) else {
            return;
        };
        let key = row.key.clone();
        if let Some(at) = self.selected.iter().position(|known| *known == key) {
            if self.selected.len() > 1 {
                self.selected.remove(at);
            }
        } else {
            self.selected.push(key);
        }
        self.anchor = Some(index);
        cx.notify();
    }

    fn extend_selection(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(row) = self.rows.get(index) else {
            return;
        };
        let anchor = self.anchor.unwrap_or(index).min(self.rows.len() - 1);
        let (from, to) = if anchor <= index {
            (anchor, index)
        } else {
            (index, anchor)
        };
        self.selected = self.rows[from..=to]
            .iter()
            .map(|row| row.key.clone())
            .collect();
        self.selected.retain(|key| *key != row.key);
        self.selected.push(row.key.clone());
        cx.notify();
    }

    fn selected_indices(&self) -> Vec<usize> {
        let mut found: Vec<usize> = self
            .rows
            .iter()
            .filter(|row| self.selected.contains(&row.key))
            .filter_map(|row| match row.kind {
                RowKind::Finding(index) => Some(index),
                _ => None,
            })
            .collect();
        if found.is_empty()
            && let Some(index) = self
                .rows
                .iter()
                .position(|row| self.selected.contains(&row.key))
                .or(self.anchor)
            && let Some(RowKind::Finding(finding)) = self.rows.get(index).map(|row| row.kind)
        {
            found.push(finding);
        }
        found
    }

    fn open_index(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(RowKind::Finding(finding)) = self.rows.get(index).map(|row| row.kind) else {
            return;
        };
        let Some(finding) = self.findings.get(finding) else {
            return;
        };
        let (path, line, column) = (finding.path.clone(), finding.line, finding.column);
        self.workspace
            .update(cx, |workspace, cx| {
                open_at(workspace, path, line, column, window, cx)
            })
            .ok();
    }

    fn open_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self
            .rows
            .iter()
            .position(|row| self.selected.contains(&row.key))
            .or(self.anchor)
        else {
            return;
        };
        self.open_index(index, window, cx);
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.rows.is_empty() {
            return;
        }
        let last = self.rows.len() as isize - 1;
        let current = self
            .rows
            .iter()
            .position(|row| self.selected.contains(&row.key))
            .or(self.anchor)
            .map_or(0, |index| index as isize);
        self.select((current + delta).clamp(0, last) as usize, cx);
    }

    fn click(
        &mut self,
        index: usize,
        event: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus, cx);
        self.menu_at = event.position();
        let modifiers = event.modifiers();
        if modifiers.control {
            self.toggle_selection(index, cx);
        } else if modifiers.shift {
            self.extend_selection(index, cx);
        } else {
            self.select(index, cx);
        }
        let expandable = self.rows.get(index).is_some_and(|row| row.expandable);
        if expandable && !modifiers.control && !modifiers.shift {
            self.toggle_row(index, cx);
        } else if !expandable && event.click_count() >= 2 {
            self.open_index(index, window, cx);
        }
    }

    fn show_fixes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let indices = self.selected_indices();
        if indices.is_empty() {
            return;
        }
        let Some(support) = cx
            .try_global::<Semantic>()
            .map(|semantic| semantic.0.clone())
        else {
            return;
        };
        let mut chosen: Vec<Finding> = Vec::new();
        for index in &indices {
            if let Some(finding) = self.findings.get(*index).cloned() {
                chosen.push(finding);
            }
        }
        if chosen.is_empty() {
            return;
        }
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        // The server answers for an open document, so a result of a closed file opens
        // first, as it does in IDEA.
        let mut opened: Vec<PathBuf> = Vec::new();
        for finding in &chosen {
            if opened.contains(&finding.path) {
                continue;
            }
            if workspace.read(cx).view_of(&finding.path).is_none() {
                workspace.update(cx, |workspace, cx| {
                    workspace.open_file(finding.path.clone(), window, cx)
                });
            }
            opened.push(finding.path.clone());
        }
        let workspace = self.workspace.clone();
        let Some(hub) = self.hub.clone() else {
            return;
        };
        let position = self.menu_at;
        cx.spawn(async move |_, cx| {
            let Some(provider) = support.actions() else {
                return;
            };
            let mut found: Vec<(Finding, Vec<ActionEntry>)> = Vec::new();
            for finding in chosen {
                let Ok(uri) = CanonPath::new(&finding.path) else {
                    continue;
                };
                let mut version = 0;
                for _ in 0..40 {
                    let known = workspace
                        .read_with(cx, |workspace, cx| {
                            workspace
                                .view_of(&finding.path)
                                .and_then(|view| view.downcast::<EditorView>().ok())
                                .and_then(|view| view.read(cx).version())
                        })
                        .ok()
                        .flatten();
                    if let Some(known) = known {
                        version = known;
                        break;
                    }
                    cx.background_executor()
                        .timer(Duration::from_millis(25))
                        .await;
                }
                let mut replies = ask(
                    provider,
                    ActionQuery {
                        uri: uri.to_uri(),
                        version,
                        span: finding.span(),
                        only: Some(ActionKind::QuickFix),
                    },
                );
                if let Some(answer) = replies.next().await {
                    found.push((finding, answer.value));
                }
            }
            let Some((_, actions)) = found.first() else {
                return;
            };
            let entries: Vec<MenuEntry> = actions
                .iter()
                .filter(|entry| !entry.edits.is_empty() || !entry.renames.is_empty())
                .map(|entry| {
                    let title = entry.title.clone();
                    let code = entry.code.clone();
                    let found = found.clone();
                    let entry_workspace = workspace.clone();
                    let entry_hub = hub.clone();
                    let entry_support = support.clone();
                    let problems: Vec<Finding> =
                        found.iter().map(|(finding, _)| finding.clone()).collect();
                    MenuEntry::new(title.clone(), move |window, cx| {
                        let mut change = WorkspaceChange::default();
                        let mut applied = false;
                        for (_, actions) in &found {
                            let Some(action) =
                                actions.iter().find(|action| match (&code, &action.code) {
                                    (Some(code), Some(other)) => code == other,
                                    _ => action.title == title,
                                })
                            else {
                                continue;
                            };
                            applied = true;
                            for source in &action.edits {
                                push_source(&mut change, source);
                            }
                        }
                        if applied {
                            entry_workspace
                                .update(cx, |workspace, cx| {
                                    crate::changes::apply(workspace, change.clone(), window, cx)
                                })
                                .ok();
                            if let Some(hub) = entry_hub.upgrade() {
                                hub.update(cx, |hub, cx| {
                                    hub.mark_fixed_and_recheck(
                                        problems.clone(),
                                        entry_support.clone(),
                                        cx,
                                    )
                                });
                            }
                        }
                    })
                })
                .collect();
            if entries.is_empty() {
                return;
            }
            cx.update(|cx| {
                workspace
                    .update(cx, |workspace, cx| {
                        workspace.show_menu(position, entries, cx)
                    })
                    .ok();
            });
        })
        .detach();
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        match key {
            "up" => {
                self.move_selection(-1, cx);
                cx.stop_propagation();
            }
            "down" => {
                self.move_selection(1, cx);
                cx.stop_propagation();
            }
            "enter" | "f4" if event.keystroke.modifiers.alt => {
                self.show_fixes(window, cx);
                cx.stop_propagation();
            }
            "enter" | "f4" => {
                self.open_selected(window, cx);
                cx.stop_propagation();
            }
            "escape" => {
                if let Some(hub) = self.hub.clone() {
                    let _ = hub.update(cx, |hub, cx| hub.cancel(cx));
                }
                cx.stop_propagation();
            }
            _ => {}
        }
    }
}

/// Drops the edits that overlap an earlier one: the answers of several requests all
/// count from the same text.
fn merge_edits(mut edits: Vec<DocumentEdit>) -> Vec<DocumentEdit> {
    let at = |point: TextPoint| (point.line, point.character);
    edits.sort_by_key(|edit| (at(edit.span.start), at(edit.span.end)));
    let mut kept = Vec::new();
    let mut end = (0, 0);
    for edit in edits {
        let start = at(edit.span.start);
        let stop = at(edit.span.end);
        if start < end {
            continue;
        }
        end = stop.max(end);
        kept.push(edit);
    }
    kept
}

fn push_source(change: &mut WorkspaceChange, source: &SourceEdit) {
    if source.edits.is_empty() {
        return;
    }
    if let Some(known) = change
        .edits
        .iter_mut()
        .find(|known| known.uri == source.uri)
    {
        for edit in &source.edits {
            if !known.edits.contains(edit) {
                known.edits.push(edit.clone());
            }
        }
        return;
    }
    change.edits.push(source.clone());
}

impl Render for InspectPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let (workspace, tool) = (self.workspace.clone(), INSPECTION_TOOL);
        let hide = move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
            workspace
                .update(cx, |workspace, cx| workspace.toggle_tool(tool, window, cx))
                .ok();
        };
        let can_rerun = self
            .hub
            .as_ref()
            .and_then(|hub| hub.read_with(cx, |hub, _| hub.can_rerun()).ok())
            .unwrap_or(false);
        let rows = self.rows.iter().enumerate().map(|(index, row)| {
            let style = TreeRow {
                id: ("inspection", index).into(),
                depth: row.depth,
                label: row.label.clone().into(),
                hint: Some(row.hint.clone().into()),
                icon: row.icon,
                expandable: row.expandable,
                open: row.open,
                selected: self.selected.contains(&row.key),
            };
            tree_row(style, &theme)
                .when(row.fixed, |this| {
                    this.text_color(theme.text_disabled).line_through()
                })
                .debug_selector(move || format!("inspection-row-{index}"))
                .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                    this.click(index, event, window, cx)
                }))
        });
        let progress_label = self.progress.as_ref().map_or_else(String::new, |progress| {
            if let Some(message) = &progress.message {
                return format!("{message} {} of {} files", progress.done, progress.total);
            }
            if progress.label.is_empty() || progress.label == "files" {
                format!("Inspecting {} of {} files", progress.done, progress.total)
            } else {
                format!(
                    "Inspecting {} of {} files in {}",
                    progress.done, progress.total, progress.label
                )
            }
        });
        let running = self.progress.is_some();
        let stop = running.then(|| {
            div()
                .id("inspection-stop")
                .debug_selector(|| "inspection-stop".to_owned())
                .px(px(8.))
                .rounded(px(CONTROL_RADIUS))
                .hover(|style| style.bg(theme.selection_hover))
                .child("Stop")
                .on_click(cx.listener(|this, _, _, cx| {
                    if let Some(hub) = this.hub.clone() {
                        cx.defer(move |cx| {
                            hub.update(cx, |hub, cx| hub.stop(cx)).ok();
                        });
                    }
                }))
        });
        let toolbar = div()
            .flex()
            .items_center()
            .justify_between()
            .flex_none()
            .px(px(SPACE_M))
            .h(px(CONTROL_HEIGHT))
            .text_color(theme.text_secondary)
            .child(SharedString::from(progress_label))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(SPACE_XS))
                    .child(
                        div()
                            .id("inspection-rerun")
                            .debug_selector(|| "inspection-rerun".to_owned())
                            .px(px(8.))
                            .rounded(px(CONTROL_RADIUS))
                            .text_color(if can_rerun {
                                theme.text
                            } else {
                                theme.text_disabled
                            })
                            .when(can_rerun, |this| {
                                this.hover(|style| style.bg(theme.selection_hover))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        if let Some(hub) = this.hub.clone() {
                                            cx.defer(move |cx| {
                                                hub.update(cx, |hub, cx| hub.rerun(cx)).ok();
                                            });
                                        }
                                    }))
                            })
                            .child("Rerun"),
                    )
                    .children(stop),
            );
        let body = if self.rows.is_empty() {
            let text = if self.progress.is_some() {
                "Inspecting the scope..."
            } else {
                "No problems found."
            };
            div()
                .p(px(SPACE_L))
                .text_color(theme.text_secondary)
                .child(text)
                .into_any_element()
        } else {
            scrollable(
                "inspection-list",
                &self.scroll,
                div()
                    .id("inspection-list")
                    .flex()
                    .flex_col()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .children(rows),
                &theme,
                window,
                cx,
            )
            .size_full()
            .into_any_element()
        };
        island(&theme)
            .id(INSPECTION_TOOL)
            .debug_selector(|| "inspection-results".to_owned())
            .key_context("InspectionResults ToolWindow")
            .track_focus(&self.focus)
            .group(GROUP)
            .h_full()
            .on_action(cx.listener(|this, _: &RerunInspection, _, cx| {
                if let Some(hub) = this.hub.clone() {
                    cx.defer(move |cx| {
                        hub.update(cx, |hub, cx| hub.rerun(cx)).ok();
                    });
                }
            }))
            .on_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)))
            .child(panel_header(self.title.clone(), GROUP, hide, &theme))
            .child(toolbar)
            .child(div().flex_1().min_h_0().pt(px(SPACE_S)).child(body))
    }
}
