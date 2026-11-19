//! What the tools of an agent answer from inside the window: the language server, the
//! file list of the index, the open buffers, the project model and Git. The window writes
//! its latest snapshot on the UI thread, and a tool call reads it on its own worker
//! thread. The text of an open document is not in the snapshot: the window hands over the
//! document itself, and a tool copies the text only when it asks.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use project_model::{EnvironmentProvider, ProjectModel, TEST_UNIT};
use tool_gateway::{
    AnalyzeCallsQuery, BreakpointId, BreakpointInfo, BreakpointQuery, BuildOutcome, BuildRequest,
    CallHierarchy, CallHierarchyQuery, CallTree, DebugEvaluateQuery, DebugRunOutcome,
    DebugRunRequest, DebugSessionState, DebugStack, DebugStackQuery, DebugStepQuery, DebugValue,
    DebugVariables, DebugVariablesQuery, DebugWaitQuery, DiagnosticsPage, DiagnosticsQuery,
    EditOutcome, EditorContext, ExecuteRunQuery, FileActionOutcome, FileActionQuery, FileContent,
    FileText, GitFileEntry, GitRepositoryStatus, GitStatus, GitStatusQuery, LanguageTools,
    PathPage, PathQuery, Position, ProjectStructure, ProjectStructureQuery, ProposeEditQuery,
    ProposedEdit, ReadFileQuery, RenameSymbolQuery, RenamedSymbol, RunConfiguration, RunOutput,
    RunPointInfo, RunPointQuery, SearchHit, SearchPage, SearchQuery, SessionScope, StartRunRequest,
    SymbolInfo, SymbolPage, SymbolQuery, TerminalOutput, TerminalQuery, TestsOutcome, TestsRequest,
    ToolBackend, ToolError, TypeHierarchy, TypeHierarchyQuery, UsagePage, UsagesQuery, VcsChanges,
    VcsDiff, VcsFile, VcsHunk, VcsQuery,
};
use ui_kit::Global;
use vcs::{Change, FileStatus, GitRunner, Repository};

use crate::mcp_edits::{EditKind, EditReply, EditRequests};

/// One open editor as the last update of the window saw it. The document shares its text
/// with the editor, so the window copies nothing: a tool that needs the text reads it when
/// it asks.
pub struct OpenDocument {
    pub path: PathBuf,
    /// The version of the text the language server holds.
    pub version: i32,
    pub dirty: bool,
    /// Another view of the same document; the text lives in the editor.
    pub document: text_core::Buffer,
}

struct OpenBuffer {
    version: i32,
    dirty: bool,
    document: text_core::Buffer,
}

/// The latest snapshot, shared between the window and the tool calls.
#[derive(Default)]
struct State {
    files: RwLock<Arc<Vec<PathBuf>>>,
    open: RwLock<HashMap<PathBuf, OpenBuffer>>,
    model: RwLock<Option<Arc<ProjectModel>>>,
}

/// The backend of the tools of an agent in this window.
pub struct WindowTools {
    languages: Arc<LanguageTools>,
    root: PathBuf,
    environment: Option<Arc<dyn EnvironmentProvider>>,
    state: Arc<State>,
    /// The edits that wait for the window, and the threads that wait for their answers.
    edits: Arc<EditRequests>,
}

/// The tools of the window, for the tests and for the commands of the window.
pub struct AgentTools(pub Arc<WindowTools>);

impl Global for AgentTools {}

/// How many dependencies of one module an answer lists.
const MAX_DEPENDENCIES: usize = 20;
/// How many hunks and lines of one diff an answer shows.
const MAX_HUNKS: usize = 50;
const MAX_DIFF_LINES: usize = 400;
/// How many matches a search keeps before it stops the pool, so one answer cannot flood
/// the memory of the process.
const MAX_MATCHES: usize = 5_000;

impl WindowTools {
    pub fn new(
        languages: Arc<LanguageTools>,
        root: PathBuf,
        environment: Option<Arc<dyn EnvironmentProvider>>,
        edits: Arc<EditRequests>,
    ) -> Self {
        Self {
            languages,
            root,
            environment,
            state: Arc::new(State::default()),
            edits,
        }
    }

    /// The requests the panel takes every frame, shared with the tool calls.
    pub fn edits(&self) -> Arc<EditRequests> {
        self.edits.clone()
    }

    /// The file list of the project index, after the scan or after a change.
    pub fn update_files(&self, files: Arc<Vec<PathBuf>>) {
        let mut known = self
            .state
            .files
            .write()
            .unwrap_or_else(|error| error.into_inner());
        *known = files;
    }

    /// The open editors of the window, as the last frame saw them. The snapshot keeps the
    /// version and the unsaved state of each document; the text stays in the editor and a
    /// tool call reads it only when it asks.
    pub fn update_open(&self, documents: Vec<OpenDocument>) {
        let mut open = self
            .state
            .open
            .write()
            .unwrap_or_else(|error| error.into_inner());
        let mut seen = HashSet::with_capacity(documents.len());
        for document in documents {
            let changed = open.get(&document.path).is_none_or(|known| {
                known.version != document.version || known.dirty != document.dirty
            });
            if changed {
                open.insert(
                    document.path.clone(),
                    OpenBuffer {
                        version: document.version,
                        dirty: document.dirty,
                        document: document.document,
                    },
                );
            }
            seen.insert(document.path);
        }
        open.retain(|path, _| seen.contains(path));
    }

    /// The model the build tool read, or `None` while there is none.
    pub fn update_model(&self, model: Option<Arc<ProjectModel>>) {
        let mut known = self
            .state
            .model
            .write()
            .unwrap_or_else(|error| error.into_inner());
        *known = model;
    }

    fn open_text(&self, path: &Path) -> Option<(String, bool)> {
        let open = self
            .state
            .open
            .read()
            .unwrap_or_else(|error| error.into_inner());
        open.get(path)
            // The text and its unsaved state come from the document itself, which may be
            // newer than the frame that published the snapshot.
            .map(|buffer| (buffer.document.text(), buffer.document.is_dirty()))
    }

    fn text_of(&self, path: &Path) -> Result<(String, bool), ToolError> {
        if let Some((text, dirty)) = self.open_text(path) {
            return Ok((text, dirty));
        }
        let text = std::fs::read_to_string(path).map_err(|error| {
            ToolError::Failed(format!("could not read {}: {error}", path.display()))
        })?;
        Ok((text, false))
    }

    fn file_content(&self, path: &Path) -> Result<FileContent, ToolError> {
        let (text, dirty) = self.text_of(path)?;
        Ok(FileContent {
            path: path.to_path_buf(),
            text,
            dirty,
        })
    }

    fn find_files(&self, query: &PathQuery) -> PathPage {
        let mut items: Vec<PathBuf> = self
            .files()
            .iter()
            .filter(|path| {
                let relative = crate::git::relative_of(&self.root, path);
                tool_gateway::path_allowed(&query.paths, &relative)
                    && tool_gateway::matches_glob(&query.pattern, &relative)
            })
            .cloned()
            .collect();
        items.sort();
        let total = items.len();
        let limit = query.limit.max(1);
        items.truncate(limit);
        PathPage {
            items,
            total,
            clipped: total > limit,
        }
    }

    fn repositories_of(&self) -> Result<Vec<PathBuf>, ToolError> {
        let git = GitRunner::from_env()
            .ok_or_else(|| ToolError::Failed("git is not on PATH".to_string()))?;
        let repo = Repository::open(git, &self.root).map_err(|error| {
            ToolError::Failed(format!(
                "{} is not inside a git repository: {error}",
                self.root.display()
            ))
        })?;
        Ok(vec![repo.root().to_path_buf()])
    }

    fn git_status_inner(&self, query: &GitStatusQuery) -> Result<GitStatus, ToolError> {
        let git = GitRunner::from_env()
            .ok_or_else(|| ToolError::Failed("git is not on PATH".to_string()))?;
        let repo = Repository::open(git, &self.root).map_err(|error| {
            ToolError::Failed(format!(
                "{} is not inside a git repository: {error}",
                self.root.display()
            ))
        })?;
        let root = repo.root().to_path_buf();
        if let Some(wanted) = &query.repository_path_relative_to_project {
            let relative = crate::git::relative_of(&self.root, &root);
            let wanted = wanted.to_string_lossy().replace('\\', "/");
            if !wanted.is_empty() && wanted != "." && wanted != relative {
                return Ok(GitStatus {
                    repositories: Vec::new(),
                });
            }
        }
        let status = repo
            .status()
            .map_err(|error| ToolError::Failed(format!("git status failed: {error}")))?;
        let mut files: Vec<GitFileEntry> = status
            .files
            .iter()
            .filter(|file| {
                query.include_untracked
                    || (file.index != Change::Untracked && file.worktree != Change::Untracked)
            })
            .map(|file| GitFileEntry {
                path: file.path.replace('\\', "/"),
                index: code_of(file.index),
                worktree: code_of(file.worktree),
            })
            .collect();
        files.sort_by(|left, right| left.path.cmp(&right.path));
        let total = files.len();
        let limit = query.limit.max(1);
        files.truncate(limit);
        Ok(GitStatus {
            repositories: vec![GitRepositoryStatus {
                path: crate::git::relative_of(&self.root, &root),
                branch: status.branch,
                files,
                total,
                clipped: total > limit,
            }],
        })
    }

    fn execute_terminal_inner(
        &self,
        scope: &SessionScope,
        query: &TerminalQuery,
    ) -> Result<TerminalOutput, ToolError> {
        let mut command = if query.execute_in_shell {
            let mut command = std::process::Command::new(shell_program());
            command.args(shell_arguments(&query.command));
            command
        } else {
            let mut parts = query.command.split_whitespace();
            let program = parts
                .next()
                .ok_or_else(|| ToolError::BadInput("the command has no program".to_string()))?;
            let mut command = std::process::Command::new(program);
            command.args(parts);
            command
        };
        command
            .current_dir(&scope.cwd)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|error| ToolError::Failed(format!("the command did not start: {error}")))?;
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let out_reader = stdout.map(|mut out| {
            std::thread::spawn(move || {
                let mut buffer = String::new();
                use std::io::Read;
                let _ = out.read_to_string(&mut buffer);
                buffer
            })
        });
        let err_reader = stderr.map(|mut err| {
            std::thread::spawn(move || {
                let mut buffer = String::new();
                use std::io::Read;
                let _ = err.read_to_string(&mut buffer);
                buffer
            })
        });
        let started = std::time::Instant::now();
        let timeout = std::time::Duration::from_millis(query.timeout.unwrap_or(60_000));
        let mut timed_out = false;
        let exit_code = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status.code(),
                Ok(None) if started.elapsed() >= timeout => {
                    timed_out = true;
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(20)),
                Err(error) => {
                    return Err(ToolError::Failed(format!(
                        "the command did not answer: {error}"
                    )));
                }
            }
        };
        let mut text = out_reader
            .and_then(|reader| reader.join().ok())
            .unwrap_or_default();
        let errors = err_reader
            .and_then(|reader| reader.join().ok())
            .unwrap_or_default();
        text.push_str(&errors);
        let (text, truncated) = truncate_lines(&text, query.max_lines_count, &query.truncate_mode);
        Ok(TerminalOutput {
            output: text,
            exit_code,
            timed_out,
            truncated,
        })
    }

    fn files(&self) -> Arc<Vec<PathBuf>> {
        self.state
            .files
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    fn range_of(
        &self,
        path: &Path,
        text: &str,
        start_line: u32,
        end_line: u32,
        dirty: bool,
    ) -> FileText {
        let mut lines: Vec<&str> = if text.is_empty() {
            Vec::new()
        } else {
            text.split('\n').collect()
        };
        if text.ends_with('\n') {
            lines.pop();
        }
        let total = lines.len() as u32;
        let requested_start = start_line.max(1);
        let requested_end = end_line.max(requested_start);
        if total == 0 {
            return FileText {
                path: path.to_path_buf(),
                text: String::new(),
                start_line: 1,
                end_line: 0,
                total_lines: 0,
                dirty,
                clipped: false,
            };
        }
        let start = requested_start.min(total).max(1);
        let end = requested_end.min(total).max(start);
        let text = lines[start as usize - 1..end as usize].join("\n");
        FileText {
            path: path.to_path_buf(),
            text,
            start_line: start,
            end_line: end,
            total_lines: total,
            dirty,
            clipped: start != requested_start || end != requested_end,
        }
    }

    fn project_structure(
        &self,
        query: &ProjectStructureQuery,
    ) -> Result<ProjectStructure, ToolError> {
        let model = self
            .state
            .model
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
            .ok_or_else(|| {
                ToolError::Failed("the build tool has not read the project yet".to_string())
            })?;
        let total_modules = model.modules.len();
        let limit = query.limit.max(1);
        let mut modules = Vec::new();
        for module in model.modules.iter().take(limit) {
            let mut sources = Vec::new();
            let mut tests = Vec::new();
            for unit in &module.units {
                if unit.name == TEST_UNIT {
                    tests.extend(unit.source_roots.iter().cloned());
                } else {
                    sources.extend(unit.source_roots.iter().cloned());
                }
            }
            let mut dependencies = Vec::new();
            if let Some(found) = model.dependencies_of(&module.id) {
                for scope in &found.scopes {
                    for root in &scope.roots {
                        let coordinates = root.coordinates.clone();
                        if !dependencies.contains(&coordinates) {
                            dependencies.push(coordinates);
                        }
                    }
                }
            }
            let dependencies_clipped = dependencies.len() > MAX_DEPENDENCIES;
            dependencies.truncate(MAX_DEPENDENCIES);
            let relative = crate::git::relative_of(&self.root, &module.dir);
            let path = if relative.is_empty() {
                PathBuf::from(".")
            } else {
                PathBuf::from(relative)
            };
            modules.push(tool_gateway::ProjectModule {
                name: module.name.clone(),
                path,
                source_roots: roots_relative(&self.root, sources),
                test_roots: roots_relative(&self.root, tests),
                dependencies,
                dependencies_clipped,
            });
        }
        let info = self
            .environment
            .as_ref()
            .map(|environment| environment.describe());
        let runtime = info
            .as_ref()
            .and_then(|info| info.selected_runtime())
            .map(|runtime| format!("{} ({})", runtime.name, runtime.version));
        let language_level = info
            .as_ref()
            .and_then(|info| info.language.as_ref())
            .and_then(|language| language.selected.clone());
        Ok(ProjectStructure {
            modules,
            total_modules,
            clipped: total_modules > limit,
            build_system: model.tool.clone(),
            build_system_version: model.tool_version.clone(),
            resolved: model.resolved,
            runtime,
            language_level,
            problems: model.problems.clone(),
        })
    }

    fn search(&self, query: &SearchQuery) -> Result<SearchPage, ToolError> {
        let mask = query.mask.as_deref().unwrap_or_default();
        let files: Vec<PathBuf> = self
            .files()
            .iter()
            .filter(|path| mask.is_empty() || crate::finder::accepts_mask(mask, path))
            .cloned()
            .collect();
        let open: HashMap<PathBuf, String> = self
            .state
            .open
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .map(|(path, buffer)| (path.clone(), buffer.document.text()))
            .collect();
        let text_query = search::TextQuery {
            pattern: query.pattern.clone(),
            regex: query.regex,
            match_case: false,
            whole_words: false,
            preserve_case: false,
        };
        let cancel = base::Cancel::new();
        let stop = cancel.clone();
        let seen = std::sync::atomic::AtomicUsize::new(0);
        let found = std::sync::Mutex::new(Vec::new());
        search::find_in_files(&text_query, &files, &open, &cancel, &|file| {
            let lines = file.lines.len();
            found
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(file);
            let seen = seen.fetch_add(lines, std::sync::atomic::Ordering::Relaxed) + lines;
            if seen >= MAX_MATCHES {
                stop.cancel();
            }
        })
        .map_err(|error| ToolError::BadInput(error.to_string()))?;
        let found = found
            .into_inner()
            .unwrap_or_else(|error| error.into_inner());
        let mut items = Vec::new();
        let mut found_files = 0;
        for file in found {
            let mut matched = false;
            for line in file.lines {
                let Some(first) = line.matches.first() else {
                    continue;
                };
                let column = line.text[..first.start].chars().count() as u32 + 1;
                matched = true;
                items.push(SearchHit {
                    path: file.path.clone(),
                    line: line.line as u32,
                    column,
                    content: line.text,
                });
            }
            if matched {
                found_files += 1;
            }
        }
        items.sort_by(|left, right| {
            (&left.path, left.line, left.column).cmp(&(&right.path, right.line, right.column))
        });
        items.truncate(MAX_MATCHES);
        let total = items.len();
        let limit = query.limit.max(1);
        let clipped = total > limit;
        items.truncate(limit);
        Ok(SearchPage {
            items,
            total,
            clipped,
            files: found_files,
        })
    }

    fn vcs(&self, query: &VcsQuery) -> Result<VcsChanges, ToolError> {
        let git = GitRunner::from_env()
            .ok_or_else(|| ToolError::Failed("git is not on PATH".to_string()))?;
        let repo = Repository::open(git, &self.root).map_err(|error| {
            ToolError::Failed(format!(
                "{} is not inside a git repository: {error}",
                self.root.display()
            ))
        })?;
        let status = repo
            .status()
            .map_err(|error| ToolError::Failed(format!("git status failed: {error}")))?;
        let mut files: Vec<VcsFile> = status
            .files
            .iter()
            .filter_map(|file| {
                let absolute = repo.root().join(&file.path);
                let relative = inside(&absolute, &self.root)?;
                Some(VcsFile {
                    path: relative,
                    status: status_of(file).to_string(),
                    original: file.original.clone(),
                })
            })
            .collect();
        let total = files.len();
        let limit = query.limit.max(1);
        let clipped = total > limit;
        files.truncate(limit);
        let diff = match &query.path {
            Some(path) => Some(self.diff(&repo, path)?),
            None => None,
        };
        Ok(VcsChanges {
            files,
            total,
            clipped,
            branch: status.branch,
            diff,
        })
    }

    fn diff(&self, repo: &Repository, path: &Path) -> Result<VcsDiff, ToolError> {
        let relative = inside(path, repo.root()).ok_or_else(|| {
            ToolError::Failed(format!(
                "{} is outside the repository {}",
                path.display(),
                repo.root().display()
            ))
        })?;
        let current = match self.open_text(path) {
            Some((text, _)) => text,
            None => std::fs::read_to_string(path).unwrap_or_default(),
        };
        let mut blobs = repo
            .blobs()
            .map_err(|error| ToolError::Failed(format!("git cat-file failed: {error}")))?;
        let file = repo
            .file_diff(&mut blobs, &relative, &current)
            .map_err(|error| ToolError::Failed(format!("git diff failed: {error}")))?;
        let before: Vec<&str> = file.base.split('\n').collect();
        let after: Vec<&str> = file.current.split('\n').collect();
        let mut hunks = Vec::new();
        let mut lines = 0;
        let mut clipped = file.hunks.len() > MAX_HUNKS;
        for hunk in file.hunks.iter().take(MAX_HUNKS) {
            let removed = lines_in(&before, hunk.before.start, hunk.before.end);
            let added = lines_in(&after, hunk.after.start, hunk.after.end);
            lines += removed.len() + added.len();
            if lines > MAX_DIFF_LINES {
                clipped = true;
                break;
            }
            hunks.push(VcsHunk {
                before: (hunk.before.start, hunk.before.end),
                after: (hunk.after.start, hunk.after.end),
                removed,
                added,
            });
        }
        Ok(VcsDiff {
            path: crate::git::relative_of(&self.root, path),
            hunks,
            clipped,
        })
    }

    /// Puts a change of one file to the window, which applies it at once, and says whether
    /// the text changed.
    fn apply_file_action(
        &self,
        path: &Path,
        change: lang_api::WorkspaceChange,
    ) -> Result<FileActionOutcome, ToolError> {
        let edits: usize = change.edits.iter().map(|source| source.edits.len()).sum();
        if change.edits.iter().all(|source| source.edits.is_empty()) {
            return Ok(FileActionOutcome {
                path: path.to_path_buf(),
                changed: false,
                edits: 0,
            });
        }
        match self.edits.ask(EditKind::Apply { change }) {
            EditReply::Applied { changed } => Ok(FileActionOutcome {
                path: path.to_path_buf(),
                changed,
                edits,
            }),
            EditReply::Failed(message) => Err(ToolError::Failed(message)),
            EditReply::NoDecision => Err(ToolError::Failed(
                "the window did not apply the change".to_string(),
            )),
            _ => Err(ToolError::Failed(
                "the window answered a file action the wrong way".to_string(),
            )),
        }
    }
}

/// The roots of one module, relative to the project when they are inside it.
fn roots_relative(root: &Path, roots: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    roots
        .into_iter()
        .map(|path| PathBuf::from(crate::git::relative_of(root, &path)))
        .filter(|path| seen.insert(path.clone()))
        .collect()
}

fn lines_in(lines: &[&str], start: u32, end: u32) -> Vec<String> {
    let from = (start as usize).min(lines.len());
    let to = (end as usize).min(lines.len()).max(from);
    lines[from..to]
        .iter()
        .map(|line| line.to_string())
        .collect()
}

/// The path of `path` relative to `root` with `/`, when the path is inside it.
fn inside(path: &Path, root: &Path) -> Option<String> {
    let text = path.to_string_lossy().replace('\\', "/");
    let root = root.to_string_lossy().replace('\\', "/");
    let rest = if text.eq_ignore_ascii_case(&root) {
        String::new()
    } else {
        let prefix = format!("{}/", root.trim_end_matches('/'));
        let lower = text.to_lowercase();
        if lower.starts_with(&prefix.to_lowercase()) {
            text[prefix.len()..].to_string()
        } else {
            return None;
        }
    };
    Some(rest)
}

fn status_of(file: &FileStatus) -> &'static str {
    let conflicted = Change::Conflicted;
    if file.index == conflicted || file.worktree == conflicted {
        return "conflicted";
    }
    let change = if file.worktree != Change::Unmodified {
        file.worktree
    } else {
        file.index
    };
    match change {
        Change::Unmodified => "unmodified",
        Change::Modified => "modified",
        Change::TypeChanged => "type changed",
        Change::Added => "added",
        Change::Deleted => "deleted",
        Change::Renamed => "renamed",
        Change::Copied => "copied",
        Change::Untracked => "untracked",
        Change::Conflicted => "conflicted",
    }
}

/// The porcelain code of one side of a change.
fn code_of(change: Change) -> String {
    let code = match change {
        Change::Unmodified => " ",
        Change::Modified => "M",
        Change::TypeChanged => "T",
        Change::Added => "A",
        Change::Deleted => "D",
        Change::Renamed => "R",
        Change::Copied => "C",
        Change::Untracked => "?",
        Change::Conflicted => "U",
    };
    code.to_string()
}

#[cfg(windows)]
fn shell_program() -> &'static str {
    "cmd"
}

#[cfg(windows)]
fn shell_arguments(command: &str) -> [&str; 3] {
    ["/d", "/c", command]
}

#[cfg(not(windows))]
fn shell_program() -> &'static str {
    "sh"
}

#[cfg(not(windows))]
fn shell_arguments(command: &str) -> [&str; 2] {
    ["-c", command]
}

/// The first, middle or last lines of an answer, as `truncateMode` asks.
fn truncate_lines(text: &str, max: usize, mode: &str) -> (String, bool) {
    let lines: Vec<&str> = text.lines().collect();
    if mode == "none" {
        return (text.to_string(), false);
    }
    if lines.len() <= max {
        return (text.to_string(), false);
    }
    let kept: Vec<&str> = match mode {
        "start" => lines[..max].to_vec(),
        "middle" => {
            let first = max / 2;
            let last = max - first;
            let mut kept = lines[..first].to_vec();
            kept.extend_from_slice(&lines[lines.len() - last..]);
            kept
        }
        _ => lines[lines.len() - max..].to_vec(),
    };
    let mut shown = kept.join("\n");
    shown.push('\n');
    (shown, true)
}

impl ToolBackend for WindowTools {
    fn editor_context(&self, scope: &SessionScope) -> Result<EditorContext, ToolError> {
        self.languages.editor_context(scope)
    }

    fn find_symbol(
        &self,
        scope: &SessionScope,
        query: &SymbolQuery,
    ) -> Result<SymbolPage, ToolError> {
        self.languages.find_symbol(scope, query)
    }

    fn symbol_info(
        &self,
        scope: &SessionScope,
        position: &Position,
    ) -> Result<SymbolInfo, ToolError> {
        self.languages.symbol_info(scope, position)
    }

    fn diagnostics(
        &self,
        scope: &SessionScope,
        query: &DiagnosticsQuery,
    ) -> Result<DiagnosticsPage, ToolError> {
        self.languages.diagnostics(scope, query)
    }

    fn debug_snapshot(&self, scope: &SessionScope) -> Option<Arc<dap_client::Snapshot>> {
        self.languages.debug_snapshot(scope)
    }

    fn find_usages(
        &self,
        scope: &SessionScope,
        query: &UsagesQuery,
    ) -> Result<UsagePage, ToolError> {
        self.languages.find_usages(scope, query)
    }

    fn type_hierarchy(
        &self,
        scope: &SessionScope,
        query: &TypeHierarchyQuery,
    ) -> Result<TypeHierarchy, ToolError> {
        self.languages.type_hierarchy(scope, query)
    }

    fn call_hierarchy(
        &self,
        scope: &SessionScope,
        query: &CallHierarchyQuery,
    ) -> Result<CallHierarchy, ToolError> {
        self.languages.call_hierarchy(scope, query)
    }

    fn project_structure(
        &self,
        _scope: &SessionScope,
        query: &ProjectStructureQuery,
    ) -> Result<ProjectStructure, ToolError> {
        self.project_structure(query)
    }

    fn search_text(
        &self,
        _scope: &SessionScope,
        query: &SearchQuery,
    ) -> Result<SearchPage, ToolError> {
        self.search(query)
    }

    fn read_file(
        &self,
        _scope: &SessionScope,
        query: &ReadFileQuery,
    ) -> Result<FileText, ToolError> {
        let (text, dirty) = self.text_of(&query.path)?;
        let start = query.start_line.unwrap_or(1);
        let end = query
            .end_line
            .unwrap_or(start.saturating_add(tool_gateway::READ_LINE_LIMIT - 1))
            .min(start.saturating_add(tool_gateway::READ_LINE_LIMIT - 1));
        Ok(self.range_of(&query.path, &text, start, end, dirty))
    }

    fn file_text(&self, _: &SessionScope, path: &Path) -> Result<FileContent, ToolError> {
        self.file_content(path)
    }

    fn find_files(&self, _: &SessionScope, query: &PathQuery) -> Result<PathPage, ToolError> {
        Ok(self.find_files(query))
    }

    fn search_symbols(
        &self,
        scope: &SessionScope,
        query: &SymbolQuery,
    ) -> Result<SymbolPage, ToolError> {
        self.languages.search_symbols(scope, query)
    }

    fn analyze_calls(
        &self,
        scope: &SessionScope,
        query: &AnalyzeCallsQuery,
    ) -> Result<CallTree, ToolError> {
        self.languages.analyze_calls(scope, query)
    }

    fn run_points(
        &self,
        scope: &SessionScope,
        query: &RunPointQuery,
    ) -> Result<Vec<RunPointInfo>, ToolError> {
        self.languages.run_points(scope, query)
    }

    fn source_text(&self, scope: &SessionScope, uri: &str) -> Result<String, ToolError> {
        self.languages.source_text(scope, uri)
    }

    fn open_file(&self, _: &SessionScope, path: &Path) -> Result<(), ToolError> {
        match self.edits.ask(EditKind::Open {
            path: path.to_path_buf(),
        }) {
            EditReply::Applied { .. } => Ok(()),
            EditReply::NoDecision => Err(ToolError::Failed(
                "the window did not open the file in time".to_string(),
            )),
            EditReply::Failed(message) => Err(ToolError::Failed(message)),
            _ => Err(ToolError::Failed(
                "the window answered an open request the wrong way".to_string(),
            )),
        }
    }

    fn open_files(&self, _: &SessionScope) -> Result<Vec<PathBuf>, ToolError> {
        let open = self
            .state
            .open
            .read()
            .unwrap_or_else(|error| error.into_inner());
        Ok(open.keys().cloned().collect())
    }

    fn repositories(&self, _: &SessionScope) -> Result<Vec<PathBuf>, ToolError> {
        self.repositories_of()
    }

    fn git_status(&self, _: &SessionScope, query: &GitStatusQuery) -> Result<GitStatus, ToolError> {
        self.git_status_inner(query)
    }

    fn execute_terminal(
        &self,
        scope: &SessionScope,
        query: &TerminalQuery,
    ) -> Result<TerminalOutput, ToolError> {
        self.execute_terminal_inner(scope, query)
    }

    fn start_temporary_run(
        &self,
        scope: &SessionScope,
        query: &ExecuteRunQuery,
        client: Option<&str>,
    ) -> Result<String, ToolError> {
        let Some(file) = &query.file_path else {
            return Err(ToolError::BadInput(
                "a temporary configuration needs filePath".to_string(),
            ));
        };
        let line = query.line.unwrap_or(0);
        let points = self.languages.run_points(
            scope,
            &RunPointQuery {
                file_path: file.clone(),
            },
        )?;
        let point = points
            .iter()
            .rfind(|point| point.kind == "program" && point.line <= line)
            .or_else(|| points.iter().find(|point| point.kind == "program"))
            .ok_or_else(|| {
                ToolError::Failed(format!(
                    "{} has no program to start at line {line}",
                    file.display()
                ))
            })?;
        let simple = point
            .container
            .rsplit(['.', '$'])
            .next()
            .unwrap_or(&point.container);
        let configurations = self.languages.run_configurations(scope)?;
        let Some(configuration) = configurations
            .iter()
            .find(|configuration| {
                configuration.name == simple
                    || configuration.name == point.container
                    || configuration.name == point.name
            })
            .cloned()
        else {
            return Err(ToolError::Failed(format!(
                "the project has no configuration for {}",
                point.container
            )));
        };
        let request = StartRunRequest {
            name: configuration.name,
            program_arguments: query.program_arguments.clone(),
            working_directory: query.working_directory.clone(),
            envs: query.envs.clone(),
        };
        self.languages.start_run(scope, &request, client)
    }

    fn vcs_changes(
        &self,
        _scope: &SessionScope,
        query: &VcsQuery,
    ) -> Result<VcsChanges, ToolError> {
        self.vcs(query)
    }

    fn propose_edit(
        &self,
        _scope: &SessionScope,
        query: &ProposeEditQuery,
    ) -> Result<ProposedEdit, ToolError> {
        if query.delete {
            if !query.path.exists() {
                return Err(ToolError::BadInput(format!(
                    "{} does not exist",
                    query.path.display()
                )));
            }
            std::fs::remove_file(&query.path).map_err(|error| {
                ToolError::Failed(format!(
                    "could not remove {}: {error}",
                    query.path.display()
                ))
            })?;
            return Ok(ProposedEdit {
                path: query.path.clone(),
                outcome: EditOutcome::Accepted,
                text: None,
                hunks: 1,
                kept: 1,
            });
        }
        let base = if query.path.exists() {
            Some(self.text_of(&query.path)?.0)
        } else {
            None
        };
        let proposed = match &query.new_text {
            Some(text) => text.clone(),
            None => tool_gateway::apply_replacements(
                base.as_deref().unwrap_or_default(),
                &query.replacements,
            )?,
        };
        if base.is_none() && proposed.is_empty() {
            if let Some(parent) = query.path.parent() {
                std::fs::create_dir_all(parent).map_err(|error| {
                    ToolError::Failed(format!("could not make {}: {error}", parent.display()))
                })?;
            }
            std::fs::write(&query.path, b"").map_err(|error| {
                ToolError::Failed(format!("could not write {}: {error}", query.path.display()))
            })?;
            return Ok(ProposedEdit {
                path: query.path.clone(),
                outcome: EditOutcome::Accepted,
                text: Some(String::new()),
                hunks: 0,
                kept: 0,
            });
        }
        match self.edits.ask(EditKind::Propose {
            path: query.path.clone(),
            base,
            proposed,
        }) {
            EditReply::Proposal {
                outcome,
                text,
                hunks,
                kept,
            } => Ok(ProposedEdit {
                path: query.path.clone(),
                outcome,
                text,
                hunks,
                kept,
            }),
            EditReply::NoDecision => Ok(ProposedEdit {
                path: query.path.clone(),
                outcome: EditOutcome::NoDecision,
                text: None,
                hunks: 0,
                kept: 0,
            }),
            EditReply::Failed(message) => Err(ToolError::Failed(message)),
            _ => Err(ToolError::Failed(
                "the window answered a proposed edit the wrong way".to_string(),
            )),
        }
    }

    fn rename_symbol(
        &self,
        _scope: &SessionScope,
        query: &RenameSymbolQuery,
    ) -> Result<RenamedSymbol, ToolError> {
        let change = self.languages.rename_change(query)?;
        if change.is_empty() {
            return Ok(RenamedSymbol {
                files: Vec::new(),
                edits: 0,
            });
        }
        match self.edits.ask(EditKind::Rename {
            new_name: query.new_name.clone(),
            change,
        }) {
            EditReply::Rename { files, edits } => Ok(RenamedSymbol { files, edits }),
            EditReply::Cancelled => Err(ToolError::Denied(
                "the user did not confirm the rename".to_string(),
            )),
            EditReply::NoDecision => Err(ToolError::Failed(
                "the user did not decide the rename in time".to_string(),
            )),
            EditReply::Failed(message) => Err(ToolError::Failed(message)),
            _ => Err(ToolError::Failed(
                "the window answered a rename the wrong way".to_string(),
            )),
        }
    }

    fn organize_imports(
        &self,
        _scope: &SessionScope,
        query: &FileActionQuery,
    ) -> Result<FileActionOutcome, ToolError> {
        let (text, _) = self.text_of(&query.path)?;
        let edits = self.languages.organize_imports_edits(&query.path, &text)?;
        let change = lang_api::WorkspaceChange {
            edits,
            renames: Vec::new(),
        };
        self.apply_file_action(&query.path, change)
    }

    fn format_file(
        &self,
        _scope: &SessionScope,
        query: &FileActionQuery,
    ) -> Result<FileActionOutcome, ToolError> {
        let edits = self.languages.format_edits(&query.path)?;
        let uri = platform::CanonPath::new(&query.path)
            .map_err(|error| ToolError::Failed(error.to_string()))?
            .to_uri();
        let change = lang_api::WorkspaceChange {
            edits: vec![lang_api::SourceEdit { uri, edits }],
            renames: Vec::new(),
        };
        self.apply_file_action(&query.path, change)
    }

    fn build_project(
        &self,
        scope: &SessionScope,
        request: &BuildRequest,
    ) -> Result<BuildOutcome, ToolError> {
        self.languages.build_project(scope, request)
    }

    fn run_tests(
        &self,
        scope: &SessionScope,
        request: &TestsRequest,
    ) -> Result<TestsOutcome, ToolError> {
        self.languages.run_tests(scope, request)
    }

    fn run_configurations(&self, scope: &SessionScope) -> Result<Vec<RunConfiguration>, ToolError> {
        self.languages.run_configurations(scope)
    }

    fn start_run(
        &self,
        scope: &SessionScope,
        request: &StartRunRequest,
        client: Option<&str>,
    ) -> Result<String, ToolError> {
        self.languages.start_run(scope, request, client)
    }

    fn run_output(&self, scope: &SessionScope, id: &str) -> Result<RunOutput, ToolError> {
        self.languages.run_output(scope, id)
    }

    fn stop_run(&self, scope: &SessionScope, id: &str) -> Result<(), ToolError> {
        self.languages.stop_run(scope, id)
    }

    fn set_breakpoint(
        &self,
        scope: &SessionScope,
        query: &BreakpointQuery,
    ) -> Result<BreakpointInfo, ToolError> {
        self.languages.set_breakpoint(scope, query)
    }

    fn remove_breakpoint(&self, scope: &SessionScope, id: &BreakpointId) -> Result<(), ToolError> {
        self.languages.remove_breakpoint(scope, id)
    }

    fn breakpoints(&self, scope: &SessionScope) -> Result<Vec<BreakpointInfo>, ToolError> {
        self.languages.breakpoints(scope)
    }

    fn debug_run(
        &self,
        scope: &SessionScope,
        request: &DebugRunRequest,
        client: Option<&str>,
    ) -> Result<DebugRunOutcome, ToolError> {
        self.languages.debug_run(scope, request, client)
    }

    fn debug_wait(
        &self,
        scope: &SessionScope,
        query: &DebugWaitQuery,
    ) -> Result<DebugSessionState, ToolError> {
        self.languages.debug_wait(scope, query)
    }

    fn debug_stack(
        &self,
        scope: &SessionScope,
        query: &DebugStackQuery,
    ) -> Result<DebugStack, ToolError> {
        self.languages.debug_stack(scope, query)
    }

    fn debug_variables(
        &self,
        scope: &SessionScope,
        query: &DebugVariablesQuery,
    ) -> Result<DebugVariables, ToolError> {
        self.languages.debug_variables(scope, query)
    }

    fn debug_evaluate(
        &self,
        scope: &SessionScope,
        query: &DebugEvaluateQuery,
    ) -> Result<DebugValue, ToolError> {
        self.languages.debug_evaluate(scope, query)
    }

    fn debug_step(
        &self,
        scope: &SessionScope,
        query: &DebugStepQuery,
    ) -> Result<DebugSessionState, ToolError> {
        self.languages.debug_step(scope, query)
    }

    fn debug_stop(&self, scope: &SessionScope) -> Result<(), ToolError> {
        self.languages.debug_stop(scope)
    }

    fn debug_sessions(
        &self,
        scope: &SessionScope,
        query: &tool_gateway::DebugQuery,
    ) -> Result<tool_gateway::DebugAnswer, ToolError> {
        self.languages.debug_sessions(scope, query)
    }

    fn database(
        &self,
        scope: &SessionScope,
        query: &tool_gateway::DatabaseQuery,
    ) -> Result<tool_gateway::DatabaseAnswer, ToolError> {
        self.languages.database(scope, query)
    }
}

/// The roots of one module, relative to the project when they are inside it.
#[cfg(test)]
mod tests {
    use super::{inside, status_of};
    use vcs::{Change, FileStatus};

    #[test]
    fn a_path_is_inside_the_project_or_not() {
        #[cfg(windows)]
        {
            assert_eq!(
                inside(
                    std::path::Path::new("C:/work/demo/src/A.txt"),
                    std::path::Path::new("c:/work/demo")
                ),
                Some("src/A.txt".to_string())
            );
            assert_eq!(
                inside(
                    std::path::Path::new("C:/work/demo-other/A.txt"),
                    std::path::Path::new("C:/work/demo")
                ),
                None
            );
        }
        #[cfg(not(windows))]
        {
            assert_eq!(
                inside(
                    std::path::Path::new("/work/demo/src/A.txt"),
                    std::path::Path::new("/work/demo")
                ),
                Some("src/A.txt".to_string())
            );
            assert_eq!(
                inside(
                    std::path::Path::new("/work/demo-other/A.txt"),
                    std::path::Path::new("/work/demo")
                ),
                None
            );
        }
    }

    #[test]
    fn the_worktree_side_of_a_change_wins_over_the_index_side() {
        let file = |index, worktree| FileStatus {
            path: "A.txt".to_string(),
            original: None,
            index,
            worktree,
        };

        assert_eq!(
            status_of(&file(Change::Modified, Change::Unmodified)),
            "modified"
        );
        assert_eq!(
            status_of(&file(Change::Unmodified, Change::Deleted)),
            "deleted"
        );
        assert_eq!(
            status_of(&file(Change::Unmodified, Change::Untracked)),
            "untracked"
        );
        assert_eq!(
            status_of(&file(Change::Conflicted, Change::Modified)),
            "conflicted"
        );
    }
}
