//! What the tools of an agent answer from inside the window: the language server, the
//! file list of the index, the open buffers, the project model and Git. The window writes
//! its latest snapshot on the UI thread, and a tool call reads it on its own worker
//! thread.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use project_model::{EnvironmentProvider, ProjectModel, TEST_UNIT};
use tool_gateway::{
    BuildOutcome, BuildRequest, CallHierarchy, CallHierarchyQuery, DiagnosticsPage,
    DiagnosticsQuery, EditOutcome, EditorContext, FileActionOutcome, FileActionQuery, FileText,
    LanguageTools, Position, ProjectStructure, ProjectStructureQuery, ProposeEditQuery,
    ProposedEdit, ReadFileQuery, RenameSymbolQuery, RenamedSymbol, RunConfiguration, RunOutput,
    SearchHit, SearchPage, SearchQuery, SessionScope, StartRunRequest, SymbolInfo, SymbolPage,
    SymbolQuery, TestsOutcome, TestsRequest, ToolBackend, ToolError, TypeHierarchy,
    TypeHierarchyQuery, UsagePage, UsagesQuery, VcsChanges, VcsDiff, VcsFile, VcsHunk, VcsQuery,
};
use ui_kit::Global;
use vcs::{Change, FileStatus, GitRunner, Repository};

use crate::mcp_edits::{EditKind, EditReply, EditRequests};

/// One open editor as the last update of the window saw it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenDocument {
    pub path: PathBuf,
    /// The version of the text the language server holds.
    pub version: i32,
    pub dirty: bool,
}

#[derive(Clone, Debug)]
struct OpenBuffer {
    version: i32,
    dirty: bool,
    text: String,
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

    /// The open editors and the text of those the window has not sent yet. The closure is
    /// called only for a document whose version or unsaved state changed.
    pub fn update_open(
        &self,
        documents: &[OpenDocument],
        mut text_of: impl FnMut(&Path) -> Option<String>,
    ) {
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
                match text_of(&document.path) {
                    Some(text) => {
                        open.insert(
                            document.path.clone(),
                            OpenBuffer {
                                version: document.version,
                                dirty: document.dirty,
                                text,
                            },
                        );
                    }
                    None => {
                        open.remove(&document.path);
                    }
                }
            }
            seen.insert(document.path.clone());
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
            .map(|buffer| (buffer.text.clone(), buffer.dirty))
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
            .map(|(path, buffer)| (path.clone(), buffer.text.clone()))
            .collect();
        let text_query = search::TextQuery {
            pattern: query.pattern.clone(),
            regex: query.regex,
            match_case: false,
            whole_words: false,
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
