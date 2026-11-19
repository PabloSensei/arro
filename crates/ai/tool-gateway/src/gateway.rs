use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::audit::AuditLog;
use crate::backend::{
    AnalyzeCallsQuery, BreakpointId, BreakpointQuery, BuildRequest, CallHierarchyQuery,
    DebugEvaluateQuery, DebugRunRequest, DebugStackQuery, DebugStateQuery, DebugStep,
    DebugStepQuery, DebugVariablesQuery, DebugWaitQuery, DiagnosticsQuery, ExecuteRunQuery,
    FileActionQuery, GitStatusQuery, PathQuery, Position, ProjectStructureQuery, ProposeEditQuery,
    ReadFileQuery, RenameSymbolQuery, RunId, RunPointQuery, SearchQuery, SessionScope,
    StartRunRequest, SymbolQuery, TerminalQuery, TestsRequest, ToolBackend, ToolError,
    TypeHierarchyQuery, UsagesQuery, VcsQuery,
};
use crate::clients::{self, CallOutcome, ClientInfo, ClientLog};
use crate::database_tools::{self, DatabaseAnswer, DatabaseQuery, QueryResults};
use crate::exposure::ToolExposure;
use crate::render;
use crate::tools::{
    ANALYZE_CALLS, APPLY_PATCH, BUILD_PROJECT, CALL_HIERARCHY, CREATE_NEW_FILE, DATABASE_CANCEL,
    DATABASE_CONNECTIONS, DATABASE_CREATE, DATABASE_DESCRIBE, DATABASE_EDIT, DATABASE_EXECUTE,
    DATABASE_FETCH, DATABASE_INTROSPECT, DATABASE_KINDS, DATABASE_OBJECTS, DATABASE_PREVIEW,
    DATABASE_RECENT, DATABASE_SCHEMAS, DATABASE_TEST, DEBUG_EVALUATE, DEBUG_RUN, DEBUG_STACK,
    DEBUG_STATE, DEBUG_STEP, DEBUG_STOP, DEBUG_VARIABLES, DEBUG_WAIT, DIAGNOSTICS, EDITOR_CONTEXT,
    EXECUTE_RUN_CONFIGURATION, EXECUTE_TERMINAL_COMMAND, EXECUTE_TOOL, FIND_SYMBOL, FIND_USAGES,
    FORMAT_FILE, GET_ALL_OPEN_FILE_PATHS, GET_FILE_PROBLEMS, GET_PROJECT_DEPENDENCIES,
    GET_PROJECT_MODULES, GET_REPOSITORIES, GET_RUN_CONFIGURATIONS, GET_RUN_OUTPUT, GIT_STATUS,
    LINT_FILES, LIST_BREAKPOINTS, LIST_DIRECTORY_TREE, LIST_RUN_CONFIGURATIONS,
    OPEN_FILE_IN_EDITOR, ORGANIZE_IMPORTS, PROJECT_STRUCTURE, PROPOSE_EDIT, READ_FILE,
    REFORMAT_FILE, REMOVE_BREAKPOINT, RENAME_REFACTORING, RENAME_SYMBOL, RUN_CONFIGURATION,
    RUN_TESTS, SEARCH_FILE, SEARCH_REGEX, SEARCH_SYMBOL, SEARCH_TEXT, SET_BREAKPOINT, SKILL_SEARCH,
    STOP_RUN, SYMBOL_INFO, TOOLS, TYPE_HIERARCHY, ToolClass, ToolSpec, VCS_CHANGES, XDEBUG_CONTROL,
    XDEBUG_EVALUATE, XDEBUG_FRAME_VALUES, XDEBUG_LIST_BREAKPOINTS, XDEBUG_REMOVE_BREAKPOINT,
    XDEBUG_RUN_TO_LINE, XDEBUG_SET_BREAKPOINT, XDEBUG_SET_VARIABLE, XDEBUG_STACK, XDEBUG_START,
    XDEBUG_STATUS, XDEBUG_THREADS, XDEBUG_VALUE_BY_PATH,
};
use crate::xdebug::{
    self, BreakpointOwner, DebugAnswer, DebugBreakpointQuery, DebugControlAction,
    DebugControlQuery, DebugExpressionQuery, DebugFramesQuery, DebugQuery,
    DebugRemoveBreakpointsQuery, DebugSetVariableQuery, DebugStartQuery, DebugThreadSelector,
    DebugThreadsQuery, DebugValuesQuery,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Allow,
    /// The user has to say yes.
    Ask,
    Deny,
}

impl Decision {
    fn name(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Ask => "ask",
            Self::Deny => "deny",
        }
    }
}

/// Reads pass, whatever runs code of the project needs a yes from the user.
#[derive(Clone, Copy, Debug, Default)]
pub struct Policy;

impl Policy {
    pub fn decide(&self, spec: &ToolSpec) -> Decision {
        match spec.class {
            ToolClass::Read => Decision::Allow,
            ToolClass::Execute => Decision::Ask,
        }
    }
}

/// A call of the class `Execute`, put to the user by the window that shows the question.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApprovalRequest {
    pub session: String,
    /// The name the MCP client gave itself in `initialize`, when it came from outside.
    pub client: Option<String>,
    pub tool: String,
}

/// What the user answered to a question of an `Execute` tool.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApprovalAnswer {
    /// This call runs, the next one asks again.
    Once,
    /// Every call of this session runs without a question while the gateway lives.
    Session,
    /// The user said no.
    Deny,
    /// The question timed out or the window went away without an answer.
    NoAnswer,
}

/// Asks the user before a tool of the class `Execute` runs.
pub trait Approval: Send + Sync {
    fn ask(&self, request: &ApprovalRequest) -> ApprovalAnswer;

    /// [`Self::ask`] with the name or place the call works on, for a question that says
    /// what is about to run.
    fn ask_with(&self, request: &ApprovalRequest, _detail: &str) -> ApprovalAnswer {
        self.ask(request)
    }
}

/// A short description of what an `Execute` call does, for the question of the user.
fn describe(tool: &str, input: &Value) -> String {
    let text = |key: &str| input.get(key).and_then(Value::as_str).map(str::to_string);
    match tool {
        BUILD_PROJECT => {
            text("module").map_or_else(String::new, |module| format!("module {module}"))
        }
        RUN_TESTS => match (text("suite"), text("case"), text("package")) {
            (Some(suite), Some(case), _) => format!("{suite}.{case}"),
            (Some(suite), None, _) => suite,
            (None, _, Some(package)) => package,
            _ => "all tests".to_string(),
        },
        RUN_CONFIGURATION => text("name").unwrap_or_else(|| "a configuration".to_string()),
        STOP_RUN | GET_RUN_OUTPUT => text("id").unwrap_or_default(),
        SET_BREAKPOINT | REMOVE_BREAKPOINT => match (text("path"), input.get("line")) {
            (Some(path), Some(line)) => format!("{path}:{line}"),
            (Some(path), None) => path,
            _ => tool.to_string(),
        },
        DEBUG_RUN => match (text("suite"), text("case"), text("configuration")) {
            (Some(suite), Some(case), _) => format!("{suite}.{case}"),
            (Some(suite), None, _) => suite,
            (None, _, Some(configuration)) => configuration,
            _ => "a debug session".to_string(),
        },
        DEBUG_EVALUATE => text("expression").unwrap_or_else(|| "an expression".to_string()),
        DEBUG_STEP => text("action").unwrap_or_else(|| "a step".to_string()),
        EXECUTE_TERMINAL_COMMAND => text("command").unwrap_or_default(),
        EXECUTE_RUN_CONFIGURATION => text("configurationName")
            .or_else(|| text("filePath"))
            .unwrap_or_else(|| "a configuration".to_string()),
        XDEBUG_START => text("configurationName")
            .or_else(|| text("filePath"))
            .unwrap_or_else(|| "a debug session".to_string()),
        XDEBUG_CONTROL => text("action").unwrap_or_else(|| "a step".to_string()),
        XDEBUG_RUN_TO_LINE => match (text("filePath"), input.get("line")) {
            (Some(path), Some(line)) => format!("{path}:{line}"),
            _ => "a line".to_string(),
        },
        XDEBUG_EVALUATE => text("expression").unwrap_or_else(|| "an expression".to_string()),
        XDEBUG_SET_VARIABLE => {
            let path = input
                .get("path")
                .and_then(Value::as_array)
                .map(|path| {
                    path.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(".")
                })
                .unwrap_or_default();
            format!("{path} = {}", text("newValue").unwrap_or_default())
        }
        DATABASE_TEST => format!("connection {}", text("id").unwrap_or_default()),
        DATABASE_CREATE => text("name").unwrap_or_default(),
        DATABASE_EDIT => format!("connection {}", text("connectionId").unwrap_or_default()),
        DATABASE_EXECUTE => format!(
            "a statement on connection {}",
            text("connectionId").unwrap_or_default()
        ),
        APPLY_PATCH => crate::patch::summary(
            &crate::patch::parse(&text("input").or_else(|| text("patch")).unwrap_or_default())
                .unwrap_or_default(),
        ),
        CREATE_NEW_FILE => text("pathInProject").unwrap_or_default(),
        RENAME_REFACTORING => match (text("symbolName"), text("newName")) {
            (Some(name), Some(new_name)) => format!("{name} to {new_name}"),
            _ => tool.to_string(),
        },
        REFORMAT_FILE => text("path").unwrap_or_default(),
        _ => tool.to_string(),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PositionInput {
    path: PathBuf,
    line: u32,
    column: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CallHierarchyInput {
    path: PathBuf,
    line: u32,
    column: u32,
    #[serde(default)]
    depth: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StepInput {
    action: String,
    #[serde(default)]
    seconds: Option<u64>,
}

#[derive(Deserialize)]
struct SymbolInfoInput {
    #[serde(default)]
    path: Option<PathBuf>,
    #[serde(rename = "filePath", default)]
    file_path: Option<PathBuf>,
    line: u32,
    column: u32,
}

#[derive(Deserialize)]
struct FileInput {
    #[serde(rename = "filePath")]
    file_path: PathBuf,
    /// Only for the problems of one file.
    #[serde(default, rename = "errorsOnly")]
    errors_only: bool,
}

#[derive(Deserialize)]
struct ReformatInput {
    path: PathBuf,
}

#[derive(Deserialize)]
struct CreateFileInput {
    #[serde(rename = "pathInProject")]
    path_in_project: PathBuf,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    overwrite: bool,
}

#[derive(Deserialize)]
struct RenameRefactoringInput {
    #[serde(rename = "pathInProject")]
    path_in_project: PathBuf,
    #[serde(rename = "symbolName")]
    symbol_name: String,
    #[serde(rename = "newName")]
    new_name: String,
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct DirectoryInput {
    #[serde(rename = "directoryPath")]
    directory_path: Option<PathBuf>,
    #[serde(rename = "maxDepth")]
    max_depth: usize,
}

impl Default for DirectoryInput {
    fn default() -> Self {
        Self {
            directory_path: None,
            max_depth: 3,
        }
    }
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct PatchInput {
    input: Option<String>,
    patch: Option<String>,
}

#[derive(Deserialize)]
struct SkillSearchInput {
    mode: String,
    q: String,
    #[serde(default)]
    paths: Vec<String>,
    #[serde(default)]
    include_external: bool,
    #[serde(default, rename = "includeExcluded")]
    include_excluded: bool,
    #[serde(default = "default_page_for_skill")]
    limit: usize,
}

fn default_page_for_skill() -> usize {
    100
}

#[derive(Deserialize)]
struct ExecuteToolInput {
    command: String,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct IdeaReadInput {
    #[serde(default)]
    path: Option<PathBuf>,
    #[serde(default)]
    file_path: Option<PathBuf>,
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    start_line: Option<u32>,
    #[serde(default)]
    end_line: Option<u32>,
    #[serde(default)]
    max_lines: Option<u32>,
    #[serde(default)]
    start_column: Option<u32>,
    #[serde(default)]
    end_column: Option<u32>,
    #[serde(default)]
    start_offset: Option<usize>,
    #[serde(default)]
    end_offset: Option<usize>,
    #[serde(default)]
    context_lines: Option<u32>,
    #[serde(default)]
    max_levels: Option<usize>,
    #[serde(default)]
    include_siblings: bool,
    #[serde(default)]
    include_header: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReadMode {
    Slice,
    Lines,
    LineColumns,
    Offsets,
    Indentation,
}

impl ReadMode {
    fn parse(text: &str) -> Option<Self> {
        match text {
            "slice" => Some(Self::Slice),
            "lines" => Some(Self::Lines),
            "line_columns" => Some(Self::LineColumns),
            "offsets" => Some(Self::Offsets),
            "indentation" => Some(Self::Indentation),
            _ => None,
        }
    }
}

#[derive(Deserialize)]
struct LintInput {
    files: Vec<PathBuf>,
    #[serde(default)]
    min_severity: Option<String>,
    #[serde(default)]
    timeout: Option<u64>,
}

#[derive(Deserialize)]
struct RunConfigurationsInput {
    #[serde(rename = "filePath", default)]
    file_path: Option<PathBuf>,
}

#[derive(Deserialize)]
struct SearchInput {
    q: String,
    #[serde(default)]
    paths: Vec<String>,
    #[serde(default, rename = "includeExcluded")]
    include_excluded: bool,
    #[serde(default, rename = "include_external")]
    include_external: bool,
    #[serde(default = "default_search_limit")]
    limit: usize,
}

fn default_search_limit() -> usize {
    100
}

fn parse<T: for<'de> Deserialize<'de>>(input: &Value) -> Result<T, ToolError> {
    serde_json::from_value(input.clone()).map_err(|error| ToolError::BadInput(error.to_string()))
}

/// A path of the agent inside the project, made absolute. `..` cannot leave the project, and
/// nothing is read here: the path need not exist yet.
fn resolve(scope: &SessionScope, path: &Path) -> Result<PathBuf, ToolError> {
    let mut resolved = PathBuf::new();
    for component in scope.project.join(path).components() {
        match component {
            Component::ParentDir => {
                resolved.pop();
            }
            Component::CurDir => {}
            other => resolved.push(other),
        }
    }
    let lower = |path: &Path| PathBuf::from(path.to_string_lossy().to_lowercase());
    if !lower(&resolved).starts_with(lower(&scope.project)) {
        let shown = path.display();
        return Err(ToolError::Denied(format!("{shown} is outside the project")));
    }
    Ok(resolved)
}

/// An optional `projectPath` names the project the call belongs to. A different one is a
/// refusal, so an agent never sees the project of another window through this session.
fn check_project(scope: &SessionScope, input: &Value) -> Result<(), ToolError> {
    let Some(given) = input.get("projectPath").and_then(Value::as_str) else {
        return Ok(());
    };
    let resolved = resolve(scope, Path::new(given))?;
    let lower = |path: &Path| {
        path.to_string_lossy()
            .trim_end_matches(['\\', '/'])
            .to_lowercase()
    };
    if lower(&resolved) != lower(&scope.project) {
        return Err(ToolError::Denied(format!(
            "{} is not the project of this window",
            resolved.display()
        )));
    }
    Ok(())
}

/// The tools of the database that take only a connection id, and the answer each asks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InputId {
    Test,
    Schemas,
    Kinds,
    Recent,
}

/// The place a breakpoint id names: the file and the line after the last colon.
fn split_breakpoint_id(id: &str) -> Result<(String, u32), ToolError> {
    let (path, line) = id
        .rsplit_once(':')
        .ok_or_else(|| ToolError::BadInput("breakpointId is <file>:<line>".to_string()))?;
    let line: u32 = line
        .trim()
        .parse()
        .map_err(|_| ToolError::BadInput("breakpointId is <file>:<line>".to_string()))?;
    if line == 0 || path.trim().is_empty() {
        return Err(ToolError::BadInput(
            "breakpointId is <file>:<line>, with the line from 1".to_string(),
        ));
    }
    Ok((path.to_string(), line))
}

/// An address of a connection or a text of a driver, with every password hidden, so an
/// agent never reads one from an answer.
fn clean_answer(answer: DatabaseAnswer) -> DatabaseAnswer {
    match answer {
        DatabaseAnswer::Connections(mut items) => {
            for item in &mut items {
                item.url = clients::hide_secrets(&item.url);
            }
            DatabaseAnswer::Connections(items)
        }
        DatabaseAnswer::Tested(mut result) => {
            result.result = clients::hide_secrets(&result.result);
            DatabaseAnswer::Tested(result)
        }
        DatabaseAnswer::Saved(mut connection, check) => {
            connection.url = clients::hide_secrets(&connection.url);
            DatabaseAnswer::Saved(
                connection,
                check.map(|mut result| {
                    result.result = clients::hide_secrets(&result.result);
                    result
                }),
            )
        }
        other => other,
    }
}

/// A call of the class `Execute` while it runs: the status bar shows that an agent works.
struct Working<'a>(&'a AtomicUsize);

impl<'a> Working<'a> {
    fn new(counter: &'a AtomicUsize) -> Self {
        counter.fetch_add(1, Ordering::SeqCst);
        Self(counter)
    }
}

impl Drop for Working<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

pub struct ToolGateway {
    backend: Arc<dyn ToolBackend>,
    policy: Policy,
    audit: AuditLog,
    approval: Option<Arc<dyn Approval>>,
    /// The sessions the user allowed for the rest of the life of this gateway.
    allowed_sessions: Mutex<HashSet<String>>,
    /// The MCP clients of the project and the calls they made.
    clients: ClientLog,
    /// The calls of the class `Execute` that run now.
    executing: AtomicUsize,
    /// The switches of Settings | Tools | AI | MCP Server.
    exposure: RwLock<ToolExposure>,
    /// The rows of the queries of the database tools, by `resultSetId`.
    results: QueryResults,
}

impl ToolGateway {
    pub fn new(backend: Arc<dyn ToolBackend>, audit: AuditLog) -> Self {
        Self {
            backend,
            policy: Policy,
            audit,
            approval: None,
            allowed_sessions: Mutex::new(HashSet::new()),
            clients: ClientLog::default(),
            executing: AtomicUsize::new(0),
            exposure: RwLock::new(ToolExposure::default()),
            results: QueryResults::default(),
        }
    }

    /// A gateway whose tools of the class `Execute` ask the user through `approval`.
    pub fn with_approval(
        backend: Arc<dyn ToolBackend>,
        audit: AuditLog,
        approval: Arc<dyn Approval>,
    ) -> Self {
        Self {
            backend,
            policy: Policy,
            audit,
            approval: Some(approval),
            allowed_sessions: Mutex::new(HashSet::new()),
            clients: ClientLog::default(),
            executing: AtomicUsize::new(0),
            exposure: RwLock::new(ToolExposure::default()),
            results: QueryResults::default(),
        }
    }

    /// Applies the Exposed Tools table of the settings. A tool that turns off leaves the
    /// list of every client at once, and a call of it is refused.
    pub fn set_exposure(&self, exposure: ToolExposure) {
        *self
            .exposure
            .write()
            .unwrap_or_else(|poison| poison.into_inner()) = exposure;
    }

    pub fn exposure(&self) -> ToolExposure {
        self.exposure
            .read()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }

    /// The tools a client of this session sees in `tools/list`.
    pub fn visible_tools(&self, scope: &SessionScope) -> Vec<ToolSpec> {
        self.exposure
            .read()
            .unwrap_or_else(|poison| poison.into_inner())
            .visible(scope)
    }

    pub fn session_allowed(&self, session: &str) -> bool {
        self.allowed_sessions
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .contains(session)
    }

    fn allow_session(&self, session: &str) {
        self.allowed_sessions
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .insert(session.to_string());
    }

    /// Takes back "Allow for this session": the next `Execute` call asks again. False when
    /// the session had no permission to revoke.
    pub fn revoke_session(&self, session: &str) -> bool {
        self.allowed_sessions
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(session)
    }

    /// The sessions the user allowed for their lifetime, by name.
    pub fn allowed_sessions(&self) -> Vec<String> {
        let mut sessions: Vec<String> = self
            .allowed_sessions
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .iter()
            .cloned()
            .collect();
        sessions.sort();
        sessions
    }

    /// The MCP clients and the journal of their calls.
    pub fn clients(&self) -> &ClientLog {
        &self.clients
    }

    /// How many `Execute` calls run now, so the window shows that an agent is working.
    pub fn executing(&self) -> usize {
        self.executing.load(Ordering::SeqCst)
    }

    pub fn tools(&self) -> &'static [ToolSpec] {
        &TOOLS
    }

    pub fn audit(&self) -> &AuditLog {
        &self.audit
    }

    /// Blocks while the service works, so it belongs on a background thread.
    pub fn call(
        &self,
        scope: &SessionScope,
        tool: &str,
        input: &Value,
    ) -> Result<String, ToolError> {
        self.call_as(None, scope, tool, input)
    }

    /// [`Self::call`] for a client that named itself in `initialize`; the name reaches
    /// the audit log and the question the user sees.
    pub fn call_from(
        &self,
        client: Option<&str>,
        scope: &SessionScope,
        tool: &str,
        input: &Value,
    ) -> Result<String, ToolError> {
        let client = client.map(|name| ClientInfo {
            name: name.to_string(),
            version: String::new(),
        });
        self.call_as(client.as_ref(), scope, tool, input)
    }

    /// [`Self::call_from`] with the version the client gave itself, for the Agents tab.
    pub fn call_as(
        &self,
        client: Option<&ClientInfo>,
        scope: &SessionScope,
        tool: &str,
        input: &Value,
    ) -> Result<String, ToolError> {
        self.call_with(client, scope, tool, input, false)
    }

    /// A call from a client, or from `execute_tool` with `via_router`: a router-only tool
    /// answers the second and refuses the first.
    fn call_with(
        &self,
        client: Option<&ClientInfo>,
        scope: &SessionScope,
        tool: &str,
        input: &Value,
        via_router: bool,
    ) -> Result<String, ToolError> {
        let started = Instant::now();
        let Some(spec) = TOOLS.iter().find(|spec| spec.name == tool) else {
            self.audit.record(
                &scope.session,
                client.map(|it| it.name.as_str()),
                tool,
                "unknown",
                "deny",
                false,
            );
            self.note_call(
                client,
                scope,
                tool,
                ToolClass::Read,
                input,
                CallOutcome::Refused,
                started.elapsed(),
            );
            return Err(ToolError::UnknownTool(tool.to_string()));
        };
        let class = spec.class;
        let exposure = self
            .exposure
            .read()
            .unwrap_or_else(|poison| poison.into_inner());
        if let Some(reason) = exposure.denial(tool, scope, via_router) {
            self.audit.record(
                &scope.session,
                client.map(|it| it.name.as_str()),
                tool,
                class.name(),
                "deny",
                false,
            );
            self.note_call(
                client,
                scope,
                tool,
                class,
                input,
                CallOutcome::Refused,
                started.elapsed(),
            );
            return Err(ToolError::Denied(reason.to_string()));
        }
        drop(exposure);
        let _working = (class == ToolClass::Execute).then(|| Working::new(&self.executing));
        check_project(scope, input)?;
        // `projectPath` names the project and is not an argument of the tool itself.
        let mut sanitized = input.clone();
        if let Some(object) = sanitized.as_object_mut() {
            object.remove("projectPath");
        }
        let input = &sanitized;
        let decision = self.policy.decide(spec);
        let mut refusal = None;
        let approved = match decision {
            Decision::Allow => true,
            Decision::Ask if self.session_allowed(&scope.session) => true,
            Decision::Ask => match self.approval.as_ref() {
                Some(approval) => {
                    let request = ApprovalRequest {
                        session: scope.session.clone(),
                        client: client.map(|it| it.name.clone()),
                        tool: tool.to_string(),
                    };
                    match approval.ask_with(&request, &describe(tool, input)) {
                        ApprovalAnswer::Once => true,
                        ApprovalAnswer::Session => {
                            self.allow_session(&scope.session);
                            true
                        }
                        ApprovalAnswer::Deny => {
                            refusal = Some("the user refused");
                            false
                        }
                        ApprovalAnswer::NoAnswer => {
                            refusal = Some("no answer");
                            false
                        }
                    }
                }
                None => {
                    refusal = Some("the window cannot ask for permission");
                    false
                }
            },
            Decision::Deny => false,
        };
        let client_name = client.map(|it| it.name.as_str());
        if !approved {
            let name = decision.name();
            self.audit
                .record(&scope.session, client_name, tool, class.name(), name, false);
            self.note_call(
                client,
                scope,
                tool,
                class,
                input,
                CallOutcome::Refused,
                started.elapsed(),
            );
            return Err(ToolError::Denied(
                refusal.unwrap_or("needs permission").to_string(),
            ));
        }
        let result = self.dispatch(client_name, scope, tool, input);
        let name = decision.name();
        self.audit.record(
            &scope.session,
            client_name,
            tool,
            class.name(),
            name,
            result.is_ok(),
        );
        let outcome = if result.is_ok() {
            CallOutcome::Ok
        } else {
            CallOutcome::Error
        };
        self.note_call(
            client,
            scope,
            tool,
            class,
            input,
            outcome,
            started.elapsed(),
        );
        result
    }

    #[allow(clippy::too_many_arguments)]
    fn note_call(
        &self,
        client: Option<&ClientInfo>,
        scope: &SessionScope,
        tool: &str,
        class: ToolClass,
        input: &Value,
        outcome: CallOutcome,
        duration: Duration,
    ) {
        let Some(client) = client else {
            return;
        };
        self.clients.record(
            &scope.session,
            client,
            tool,
            class,
            clients::arguments(&scope.project, input),
            outcome,
            duration,
        );
    }

    fn dispatch(
        &self,
        client: Option<&str>,
        scope: &SessionScope,
        tool: &str,
        input: &Value,
    ) -> Result<String, ToolError> {
        match tool {
            EDITOR_CONTEXT => {
                let context = self.backend.editor_context(scope)?;
                Ok(render::editor_context(&context))
            }
            FIND_SYMBOL => {
                let query: SymbolQuery = serde_json::from_value(input.clone())
                    .map_err(|error| ToolError::BadInput(error.to_string()))?;
                if query.query.trim().is_empty() {
                    return Err(ToolError::BadInput("the query is empty".to_string()));
                }
                let page = self.backend.find_symbol(scope, &query)?;
                Ok(render::symbols(&query, &page))
            }
            SYMBOL_INFO => {
                let input: SymbolInfoInput = parse(input)?;
                if input.line == 0 || input.column == 0 {
                    return Err(ToolError::BadInput(
                        "line and column start at 1".to_string(),
                    ));
                }
                let path = input.path.or(input.file_path).ok_or_else(|| {
                    ToolError::BadInput("pass path or filePath: a file of the project".to_string())
                })?;
                let position = Position {
                    path: resolve(scope, &path)?,
                    line: input.line,
                    column: input.column,
                };
                let info = self.backend.symbol_info(scope, &position)?;
                Ok(render::symbol_info(&info))
            }
            DIAGNOSTICS => {
                let mut query: DiagnosticsQuery = parse(input)?;
                if let Some(path) = &query.path {
                    query.path = Some(resolve(scope, path)?);
                }
                let page = self.backend.diagnostics(scope, &query)?;
                Ok(render::diagnostics(&query, &page))
            }
            DEBUG_STATE => {
                let query: DebugStateQuery = parse::<DebugStateQuery>(input)?.clipped();
                let snapshot = self.backend.debug_snapshot(scope);
                Ok(render::debug_state(snapshot.as_deref(), &query))
            }
            FIND_USAGES => {
                let mut query: UsagesQuery = parse::<UsagesQuery>(input)?.clipped();
                match (query.position().is_some(), query.name.is_some()) {
                    (true, true) => {
                        return Err(ToolError::BadInput(
                            "pass a place or a name, not both".to_string(),
                        ));
                    }
                    (false, false) => {
                        return Err(ToolError::BadInput(
                            "pass a path with a line and a column, or a name".to_string(),
                        ));
                    }
                    _ => {}
                }
                if let Some((path, line, column)) = query.position() {
                    if line == 0 || column == 0 {
                        return Err(ToolError::BadInput(
                            "line and column start at 1".to_string(),
                        ));
                    }
                    let resolved = resolve(scope, path)?;
                    query.path = Some(resolved);
                } else if query
                    .name
                    .as_deref()
                    .is_some_and(|name| name.trim().is_empty())
                {
                    return Err(ToolError::BadInput("the name is empty".to_string()));
                }
                let page = self.backend.find_usages(scope, &query)?;
                Ok(render::usages(&query, &page))
            }
            TYPE_HIERARCHY => {
                let input: PositionInput = parse(input)?;
                if input.line == 0 || input.column == 0 {
                    return Err(ToolError::BadInput(
                        "line and column start at 1".to_string(),
                    ));
                }
                let query = TypeHierarchyQuery {
                    path: resolve(scope, &input.path)?,
                    line: input.line,
                    column: input.column,
                };
                let hierarchy = self.backend.type_hierarchy(scope, &query)?;
                Ok(render::type_hierarchy(&query, &hierarchy))
            }
            CALL_HIERARCHY => {
                let input: CallHierarchyInput = parse(input)?;
                if input.line == 0 || input.column == 0 {
                    return Err(ToolError::BadInput(
                        "line and column start at 1".to_string(),
                    ));
                }
                let query = CallHierarchyQuery {
                    path: resolve(scope, &input.path)?,
                    line: input.line,
                    column: input.column,
                    depth: input.depth.unwrap_or(1),
                }
                .clipped();
                let hierarchy = self.backend.call_hierarchy(scope, &query)?;
                Ok(render::call_hierarchy(&query, &hierarchy))
            }
            PROJECT_STRUCTURE => {
                let query: ProjectStructureQuery = parse::<ProjectStructureQuery>(input)?.clipped();
                let structure = self.backend.project_structure(scope, &query)?;
                Ok(render::project_structure(&query, &structure))
            }
            SEARCH_TEXT => {
                let query: SearchQuery = parse::<SearchQuery>(input)?.clipped();
                if query.pattern.is_empty() {
                    return Err(ToolError::BadInput("the pattern is empty".to_string()));
                }
                let page = self.backend.search_text(scope, &query)?;
                let page = filter_search(scope, &query.paths, page);
                Ok(render::search(&query, &page))
            }
            READ_FILE => {
                let input: IdeaReadInput = parse(input)?;
                let path = input
                    .path
                    .clone()
                    .or(input.file_path.clone())
                    .ok_or_else(|| {
                        ToolError::BadInput(
                            "pass path or file_path: a file of the project".to_string(),
                        )
                    })?;
                if let Some(text) = path.to_str()
                    && is_decompiled_address(text)
                {
                    let text = self.backend.source_text(scope, text)?;
                    return Ok(render::source_text(text.as_str()));
                }
                if path
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("class"))
                {
                    return Err(ToolError::Failed(format!(
                        "no decompiler is connected for {}",
                        path.display()
                    )));
                }
                if input.mode.is_some() {
                    return self.read_file_mode(scope, &input);
                }
                let query = ReadFileQuery {
                    path,
                    start_line: input.start_line,
                    end_line: input.end_line,
                };
                if query.start_line == Some(0) || query.end_line == Some(0) {
                    return Err(ToolError::BadInput("line ranges start at 1".to_string()));
                }
                if let (Some(start), Some(end)) = (query.start_line, query.end_line)
                    && start > end
                {
                    return Err(ToolError::BadInput(
                        "the first line is after the last one".to_string(),
                    ));
                }
                let mut query = query.clipped();
                query.path = resolve(scope, &query.path)?;
                let file = self.backend.read_file(scope, &query)?;
                Ok(render::file_text(&file))
            }
            VCS_CHANGES => {
                let mut query: VcsQuery = parse::<VcsQuery>(input)?.clipped();
                if let Some(path) = &query.path {
                    query.path = Some(resolve(scope, path)?);
                }
                let changes = self.backend.vcs_changes(scope, &query)?;
                Ok(render::vcs_changes(&query, &changes))
            }
            PROPOSE_EDIT => {
                let mut query: ProposeEditQuery = parse(input)?;
                query.path = resolve(scope, &query.path)?;
                match (
                    query.delete,
                    query.new_text.is_some(),
                    query.replacements.is_empty(),
                ) {
                    (true, _, _) => {}
                    (false, true, false) => {
                        return Err(ToolError::BadInput(
                            "pass new_text or replacements, not both".to_string(),
                        ));
                    }
                    (false, false, true) => {
                        return Err(ToolError::BadInput(
                            "pass new_text or replacements".to_string(),
                        ));
                    }
                    _ => {}
                }
                let edit = self.backend.propose_edit(scope, &query)?;
                Ok(render::proposed_edit(&edit))
            }
            RENAME_SYMBOL => {
                let input: RenameSymbolQuery = parse(input)?;
                if input.line == 0 || input.column == 0 {
                    return Err(ToolError::BadInput(
                        "line and column start at 1".to_string(),
                    ));
                }
                if input.new_name.trim().is_empty() {
                    return Err(ToolError::BadInput("the new name is empty".to_string()));
                }
                let query = RenameSymbolQuery {
                    path: resolve(scope, &input.path)?,
                    ..input
                };
                let rename = self.backend.rename_symbol(scope, &query)?;
                Ok(render::renamed_symbol(&rename))
            }
            ORGANIZE_IMPORTS | FORMAT_FILE => {
                let query: FileActionQuery = parse(input)?;
                let query = FileActionQuery {
                    path: resolve(scope, &query.path)?,
                };
                let outcome = if tool == ORGANIZE_IMPORTS {
                    self.backend.organize_imports(scope, &query)?
                } else {
                    self.backend.format_file(scope, &query)?
                };
                Ok(render::file_action(&outcome))
            }
            BUILD_PROJECT => {
                let mut request: BuildRequest = parse(input)?;
                for path in std::mem::take(&mut request.files_to_rebuild) {
                    request.files_to_rebuild.push(resolve(scope, &path)?);
                }
                let outcome = self.backend.build_project(scope, &request)?;
                Ok(render::build(&request, &outcome))
            }
            RUN_TESTS => {
                let request: TestsRequest = parse(input)?;
                if request.case.is_some() && request.suite.is_none() {
                    return Err(ToolError::BadInput(
                        "a case needs the suite it belongs to".to_string(),
                    ));
                }
                if request.package.is_some() && request.suite.is_some() {
                    return Err(ToolError::BadInput(
                        "pass a package or a suite, not both".to_string(),
                    ));
                }
                let outcome = self.backend.run_tests(scope, &request)?;
                Ok(render::tests(&request, &outcome))
            }
            LIST_RUN_CONFIGURATIONS => {
                let configurations = self.backend.run_configurations(scope)?;
                Ok(render::run_configurations(&configurations))
            }
            RUN_CONFIGURATION => {
                let request: StartRunRequest = parse(input)?;
                if request.name.trim().is_empty() {
                    return Err(ToolError::BadInput(
                        "the name of the configuration is empty".to_string(),
                    ));
                }
                let id = self.backend.start_run(scope, &request, client)?;
                Ok(render::run_started(&request.name, &id))
            }
            GET_RUN_OUTPUT => {
                let request: RunId = parse(input)?;
                let output = self.backend.run_output(scope, &request.id)?;
                Ok(render::run_output(&output))
            }
            STOP_RUN => {
                let request: RunId = parse(input)?;
                self.backend.stop_run(scope, &request.id)?;
                Ok(format!("stopped {}\n", request.id))
            }
            SET_BREAKPOINT => {
                let input: BreakpointQuery = parse(input)?;
                if input.line == 0 {
                    return Err(ToolError::BadInput("lines start at 1".to_string()));
                }
                let query = BreakpointQuery {
                    path: resolve(scope, &input.path)?,
                    ..input
                }
                .clipped();
                let info = self.backend.set_breakpoint(scope, &query)?;
                Ok(render::breakpoint_set(&info))
            }
            REMOVE_BREAKPOINT => {
                let input: BreakpointId = parse(input)?;
                if input.line == 0 {
                    return Err(ToolError::BadInput("lines start at 1".to_string()));
                }
                let id = BreakpointId {
                    path: resolve(scope, &input.path)?,
                    ..input
                };
                self.backend.remove_breakpoint(scope, &id)?;
                Ok(render::breakpoint_removed(&id))
            }
            LIST_BREAKPOINTS => {
                let breakpoints = self.backend.breakpoints(scope)?;
                Ok(render::breakpoints(&breakpoints))
            }
            DEBUG_RUN => {
                let request: DebugRunRequest = parse::<DebugRunRequest>(input)?.clipped();
                let named = |value: &Option<String>| {
                    value
                        .as_deref()
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .map(str::to_string)
                };
                let request = DebugRunRequest {
                    module: named(&request.module),
                    suite: named(&request.suite),
                    case: named(&request.case),
                    configuration: named(&request.configuration),
                    ..request
                };
                match (
                    request.configuration.is_some(),
                    request.suite.is_some(),
                    request.case.is_some(),
                    request.module.is_some(),
                ) {
                    (true, false, false, false) => {}
                    (true, _, _, _) => {
                        return Err(ToolError::BadInput(
                            "pass a configuration or a test, not both".to_string(),
                        ));
                    }
                    (false, false, _, _) if request.case.is_none() => {
                        return Err(ToolError::BadInput(
                            "pass a suite, a case with its suite, or a configuration".to_string(),
                        ));
                    }
                    (false, false, _, _) => {
                        return Err(ToolError::BadInput(
                            "a case needs the suite it belongs to".to_string(),
                        ));
                    }
                    (false, true, _, _) => {}
                }
                let outcome = self.backend.debug_run(scope, &request, client)?;
                Ok(render::debug_run(&request, &outcome))
            }
            DEBUG_WAIT => {
                let query: DebugWaitQuery = parse::<DebugWaitQuery>(input)?.clipped();
                let state = self.backend.debug_wait(scope, &query)?;
                Ok(render::debug_session(&state))
            }
            DEBUG_STACK => {
                let query: DebugStackQuery = parse::<DebugStackQuery>(input)?.clipped();
                let stack = self.backend.debug_stack(scope, &query)?;
                Ok(render::debug_stack(&stack, &query))
            }
            DEBUG_VARIABLES => {
                let query: DebugVariablesQuery = parse::<DebugVariablesQuery>(input)?.clipped();
                let variables = self.backend.debug_variables(scope, &query)?;
                Ok(render::debug_variables(&variables, &query))
            }
            DEBUG_EVALUATE => {
                let input: DebugEvaluateQuery = parse(input)?;
                let expression = input.expression.trim();
                if expression.is_empty() {
                    return Err(ToolError::BadInput("the expression is empty".to_string()));
                }
                let query = DebugEvaluateQuery {
                    expression: expression.to_string(),
                    frame: input.frame.map(|frame| frame.min(99)),
                };
                let value = self.backend.debug_evaluate(scope, &query)?;
                Ok(render::debug_value(&value))
            }
            DEBUG_STEP => {
                let input: StepInput = parse(input)?;
                let Some(step) = DebugStep::parse(&input.action) else {
                    let action = input.action;
                    return Err(ToolError::BadInput(format!(
                        "unknown step {action}, pass over, into, out or continue"
                    )));
                };
                let query = DebugStepQuery {
                    step,
                    seconds: input.seconds.unwrap_or(10),
                }
                .clipped();
                let state = self.backend.debug_step(scope, &query)?;
                Ok(render::debug_session(&state))
            }
            DEBUG_STOP => {
                self.backend.debug_stop(scope)?;
                Ok("debug session stopped\n".to_string())
            }
            ANALYZE_CALLS => {
                let query: AnalyzeCallsQuery = parse::<AnalyzeCallsQuery>(input)?.clipped();
                if query.symbol_fqn.trim().is_empty() {
                    return Err(ToolError::BadInput("the symbol name is empty".to_string()));
                }
                let tree = self.backend.analyze_calls(scope, &query)?;
                Ok(render::call_tree(&query, &tree))
            }
            GET_FILE_PROBLEMS => {
                let input: FileInput = parse(input)?;
                let query = DiagnosticsQuery {
                    path: Some(resolve(scope, &input.file_path)?),
                    severity: if input.errors_only {
                        crate::backend::Severity::Error
                    } else {
                        crate::backend::Severity::Warning
                    },
                    limit: 200,
                    after: None,
                };
                let page = self.backend.diagnostics(scope, &query)?;
                Ok(render::file_problems(&input.file_path, &page))
            }
            GET_PROJECT_DEPENDENCIES => {
                let query = ProjectStructureQuery { limit: 500 };
                let structure = self.backend.project_structure(scope, &query)?;
                Ok(render::project_dependencies(&structure))
            }
            GET_PROJECT_MODULES => {
                let query = ProjectStructureQuery { limit: 500 };
                let structure = self.backend.project_structure(scope, &query)?;
                Ok(render::project_modules(&structure))
            }
            LINT_FILES => self.lint_files(scope, input),
            GET_RUN_CONFIGURATIONS => self.get_run_configurations(scope, input),
            EXECUTE_RUN_CONFIGURATION => self.execute_run(scope, input, client),
            CREATE_NEW_FILE => self.create_new_file(scope, input),
            GET_ALL_OPEN_FILE_PATHS => {
                let files = self.backend.open_files(scope)?;
                Ok(render::open_paths(scope, &files))
            }
            LIST_DIRECTORY_TREE => self.directory_tree(scope, input),
            OPEN_FILE_IN_EDITOR => {
                let input: FileInput = parse(input)?;
                let path = resolve(scope, &input.file_path)?;
                self.backend.open_file(scope, &path)?;
                Ok(render::opened_file(scope, &path))
            }
            REFORMAT_FILE => {
                let input: ReformatInput = parse(input)?;
                let query = FileActionQuery {
                    path: resolve(scope, &input.path)?,
                };
                let outcome = self.backend.format_file(scope, &query)?;
                Ok(render::file_action(&outcome))
            }
            APPLY_PATCH => self.apply_patch(scope, input, client),
            RENAME_REFACTORING => self.rename_refactoring(scope, input),
            SEARCH_FILE => self.search_files(scope, input),
            SEARCH_REGEX => self.search_regex(scope, input),
            SEARCH_SYMBOL => self.search_symbol(scope, input),
            SKILL_SEARCH => self.skill_search(scope, input),
            EXECUTE_TERMINAL_COMMAND => self.execute_terminal(scope, input),
            EXECUTE_TOOL => self.execute_tool(client, scope, input),
            GET_REPOSITORIES => {
                let roots = self.backend.repositories(scope)?;
                Ok(render::repositories(scope, &roots))
            }
            GIT_STATUS => {
                let query: GitStatusQuery = parse::<GitStatusQuery>(input)?.clipped();
                let mut query = query;
                if let Some(path) = &query.repository_path_relative_to_project {
                    query.repository_path_relative_to_project = Some(resolve(scope, path)?);
                }
                let status = self.backend.git_status(scope, &query)?;
                Ok(render::git_status(scope, &status))
            }
            XDEBUG_STATUS => self.xdebug_status(scope),
            XDEBUG_START => self.xdebug_start(scope, input, client),
            XDEBUG_CONTROL => self.xdebug_control(scope, input),
            XDEBUG_RUN_TO_LINE => self.xdebug_run_to_line(scope, input),
            XDEBUG_THREADS => self.xdebug_threads(scope, input),
            XDEBUG_STACK => self.xdebug_stack(scope, input),
            XDEBUG_FRAME_VALUES => self.xdebug_frame_values(scope, input),
            XDEBUG_VALUE_BY_PATH => self.xdebug_value_by_path(scope, input),
            XDEBUG_EVALUATE => self.xdebug_evaluate(scope, input),
            XDEBUG_SET_VARIABLE => self.xdebug_set_variable(scope, input),
            XDEBUG_LIST_BREAKPOINTS => self.xdebug_list_breakpoints(scope, input),
            XDEBUG_SET_BREAKPOINT => self.xdebug_set_breakpoint(scope, input),
            XDEBUG_REMOVE_BREAKPOINT => self.xdebug_remove_breakpoint(scope, input),
            DATABASE_CONNECTIONS => self.database(scope, &DatabaseQuery::ListConnections),
            DATABASE_TEST => self.database_id(scope, input, InputId::Test),
            DATABASE_CREATE => self.database_create(scope, input),
            DATABASE_EDIT => self.database_edit(scope, input),
            DATABASE_SCHEMAS => self.database_id(scope, input, InputId::Schemas),
            DATABASE_INTROSPECT => self.database_introspect(scope, input),
            DATABASE_KINDS => self.database_id(scope, input, InputId::Kinds),
            DATABASE_OBJECTS => self.database_objects(scope, input),
            DATABASE_DESCRIBE => self.database_describe(scope, input),
            DATABASE_EXECUTE => self.database_execute(scope, input),
            DATABASE_FETCH => self.database_fetch(input),
            DATABASE_PREVIEW => self.database_preview(scope, input),
            DATABASE_RECENT => self.database_id(scope, input, InputId::Recent),
            DATABASE_CANCEL => self.database_cancel(scope, input),
            _ => Err(ToolError::UnknownTool(tool.to_string())),
        }
    }

    /// Reads a file in one of the shapes of `read_file` that IDEA names.
    fn read_file_mode(
        &self,
        scope: &SessionScope,
        input: &IdeaReadInput,
    ) -> Result<String, ToolError> {
        let Some(mode) = input.mode.as_deref().and_then(ReadMode::parse) else {
            return Err(ToolError::BadInput(
                "pass mode slice, lines, line_columns, offsets or indentation".to_string(),
            ));
        };
        let path = input
            .path
            .clone()
            .or_else(|| input.file_path.clone())
            .ok_or_else(|| {
                ToolError::BadInput("pass path or file_path: a file of the project".to_string())
            })?;
        let path = resolve(scope, &path)?;
        let content = self.backend.file_text(scope, &path)?;
        let lines: Vec<&str> = content.text.split('\n').collect();
        let total = if content.text.ends_with('\n') || content.text.is_empty() {
            lines.len().saturating_sub(1)
        } else {
            lines.len()
        };
        if total == 0 {
            return Ok(render::file_text(&crate::backend::FileText {
                path,
                text: String::new(),
                start_line: 1,
                end_line: 0,
                total_lines: 0,
                dirty: content.dirty,
                clipped: false,
            }));
        }
        let max_lines = input.max_lines.unwrap_or(2_000).clamp(1, 2_000) as usize;
        let (mut start, mut end) = match mode {
            ReadMode::Slice | ReadMode::Lines | ReadMode::LineColumns => {
                let start = input.start_line.unwrap_or(1).max(1) as usize;
                if start > total {
                    return Err(ToolError::BadInput(format!(
                        "line {start} is after the last line {total}"
                    )));
                }
                let end = input
                    .end_line
                    .map(|end| end.max(1) as usize)
                    .unwrap_or((start + max_lines - 1).min(total))
                    .min(total)
                    .min(start + max_lines - 1);
                if end < start {
                    return Err(ToolError::BadInput(
                        "the first line is after the last one".to_string(),
                    ));
                }
                (start, end)
            }
            ReadMode::Offsets => {
                let start = input.start_offset.unwrap_or(0);
                let end = input
                    .end_offset
                    .unwrap_or_else(|| content.text.chars().count());
                if end < start {
                    return Err(ToolError::BadInput(
                        "the first offset is after the last one".to_string(),
                    ));
                }
                let start_byte = byte_of_char(&content.text, start);
                let end_byte = byte_of_char(&content.text, end);
                let start_line = content.text[..start_byte].matches('\n').count() + 1;
                let end_line = content.text[..end_byte].matches('\n').count() + 1;
                (start_line, end_line.max(start_line))
            }
            ReadMode::Indentation => {
                let start = input.start_line.unwrap_or(1).max(1) as usize;
                if start > total {
                    return Err(ToolError::BadInput(format!(
                        "line {start} is after the last line {total}"
                    )));
                }
                let levels = input.max_levels.unwrap_or(1).max(1);
                indentation_block(&lines, start, levels)
            }
        };
        let context = input.context_lines.unwrap_or(0) as usize;
        if mode == ReadMode::Indentation {
            let anchored = lines.get(start - 1).copied().unwrap_or_default();
            let indent = anchored.len() - anchored.trim_start().len();
            if input.include_header {
                while start > 1 {
                    let previous = lines[start - 2];
                    if !previous.trim().is_empty()
                        && previous.len() - previous.trim_start().len() < indent
                    {
                        start -= 1;
                        break;
                    }
                    start -= 1;
                }
            }
            if input.include_siblings {
                while end < total {
                    let next = lines[end];
                    if next.trim().is_empty() || next.len() - next.trim_start().len() < indent {
                        break;
                    }
                    end += 1;
                }
            }
        }
        let start = start.max(1).saturating_sub(context).max(1);
        let end = (end + context).min(total);
        let mut text = String::new();
        let path_shown = path.display();
        let _ = std::fmt::Write::write_fmt(
            &mut text,
            format_args!(
                "{path_shown}, lines {start}-{end} of {total}{}\n",
                if content.dirty {
                    ", unsaved changes"
                } else {
                    ""
                }
            ),
        );
        for number in start..=end {
            let line = lines.get(number - 1).copied().unwrap_or_default();
            let shown = match mode {
                ReadMode::LineColumns => {
                    let from = input.start_column.unwrap_or(1).max(1) as usize;
                    let to = input.end_column.unwrap_or(u32::MAX) as usize;
                    line.chars()
                        .skip(from - 1)
                        .take(to.saturating_sub(from - 1))
                        .collect()
                }
                _ => line.to_string(),
            };
            let _ = std::fmt::Write::write_fmt(&mut text, format_args!("{number}|{shown}\n"));
        }
        Ok(text)
    }

    fn lint_files(&self, scope: &SessionScope, input: &Value) -> Result<String, ToolError> {
        let input: LintInput = parse(input)?;
        if input.files.is_empty() {
            return Err(ToolError::BadInput(
                "pass files: a list of files".to_string(),
            ));
        }
        let severity = match input.min_severity.as_deref() {
            Some("error") => crate::backend::Severity::Error,
            Some("warning") | None => crate::backend::Severity::Warning,
            Some(other) => {
                return Err(ToolError::BadInput(format!(
                    "unknown severity {other}, pass warning or error"
                )));
            }
        };
        let _ = input.timeout;
        let mut reports = Vec::new();
        for file in input.files {
            let path = resolve(scope, &file)?;
            let query = DiagnosticsQuery {
                path: Some(path),
                severity,
                limit: 200,
                after: None,
            };
            let page = self
                .backend
                .diagnostics(scope, &query)
                .map_err(|error| error.to_string());
            reports.push((file, page));
        }
        Ok(render::lint_results(scope, severity, &reports))
    }

    fn get_run_configurations(
        &self,
        scope: &SessionScope,
        input: &Value,
    ) -> Result<String, ToolError> {
        let input: RunConfigurationsInput = parse(input)?;
        if let Some(path) = input.file_path {
            let path = resolve(scope, &path)?;
            let points = self
                .backend
                .run_points(scope, &RunPointQuery { file_path: path })?;
            return Ok(render::run_points(scope, &points));
        }
        let configurations = self.backend.run_configurations(scope)?;
        Ok(render::run_configurations_details(scope, &configurations))
    }

    fn execute_run(
        &self,
        scope: &SessionScope,
        input: &Value,
        client: Option<&str>,
    ) -> Result<String, ToolError> {
        let mut query: ExecuteRunQuery = parse(input)?;
        query.envs.retain(|pair| pair.contains('='));
        if let Some(path) = &query.working_directory {
            query.working_directory = Some(resolve(scope, path)?);
        }
        let name = match (&query.configuration_name, &query.file_path) {
            (Some(name), None) => name.clone(),
            (None, Some(path)) => {
                let path = resolve(scope, path)?;
                query.file_path = Some(path);
                if query.line.unwrap_or(0) == 0 {
                    return Err(ToolError::BadInput(
                        "a temporary configuration needs line".to_string(),
                    ));
                }
                let id = self.backend.start_temporary_run(scope, &query, client)?;
                return self.wait_for_run(scope, &id, query.timeout, query.wait_for_exit);
            }
            (Some(_), Some(_)) => {
                return Err(ToolError::BadInput(
                    "pass configurationName or filePath, not both".to_string(),
                ));
            }
            (None, None) => {
                return Err(ToolError::BadInput(
                    "pass configurationName or filePath with line".to_string(),
                ));
            }
        };
        let request = StartRunRequest {
            name: name.clone(),
            program_arguments: query.program_arguments.clone(),
            working_directory: query.working_directory.clone(),
            envs: query.envs.clone(),
        };
        let id = self.backend.start_run(scope, &request, client)?;
        self.wait_for_run(scope, &id, query.timeout, query.wait_for_exit)
    }

    fn wait_for_run(
        &self,
        scope: &SessionScope,
        id: &str,
        timeout: Option<u64>,
        wait: bool,
    ) -> Result<String, ToolError> {
        let started = Instant::now();
        let limit = timeout.unwrap_or(120_000);
        loop {
            let output = self.backend.run_output(scope, id)?;
            let timed_out = wait && started.elapsed() >= Duration::from_millis(limit);
            if !wait || !output.running || timed_out {
                return Ok(render::executed_run(&output, timed_out));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// The sessions of the debug window, read by every `xdebug_*` tool that needs one.
    fn debugger_status(&self, scope: &SessionScope) -> Result<xdebug::DebuggerStatus, ToolError> {
        match self.backend.debug_sessions(scope, &DebugQuery::Status)? {
            DebugAnswer::Status(status) => Ok(status),
            _ => Err(ToolError::Failed(
                "the debug window answered the sessions wrongly".to_string(),
            )),
        }
    }

    /// The one session a call works on, or the one it names.
    fn debug_session_id(
        &self,
        scope: &SessionScope,
        requested: Option<&str>,
    ) -> Result<String, ToolError> {
        let status = self.debugger_status(scope)?;
        Ok(xdebug::select_session(&status.sessions, requested)?
            .id
            .clone())
    }

    fn xdebug_status(&self, scope: &SessionScope) -> Result<String, ToolError> {
        Ok(xdebug::json(&self.debugger_status(scope)?))
    }

    fn xdebug_start(
        &self,
        scope: &SessionScope,
        input: &Value,
        client: Option<&str>,
    ) -> Result<String, ToolError> {
        let input: xdebug::StartInput = parse(input)?;
        let named = |value: &Option<String>| {
            value
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        };
        let configuration_name = named(&input.configuration_name);
        let mut query = DebugStartQuery {
            timeout_ms: input.timeout.unwrap_or(60_000),
            grace_wait_ms: input.grace_wait_ms.unwrap_or(2_000),
            program_arguments: input.program_arguments,
            working_directory: None,
            envs: input
                .envs
                .into_iter()
                .filter(|pair| pair.contains('='))
                .collect(),
            client: client.map(str::to_string),
            ..DebugStartQuery::default()
        };
        query.timeout_ms = query.timeout_ms.min(600_000);
        query.grace_wait_ms = query.grace_wait_ms.min(60_000);
        if let Some(directory) = input.working_directory {
            query.working_directory = Some(resolve(scope, &directory)?);
        }
        query.configuration_name = match (configuration_name, input.file_path) {
            (Some(_), Some(_)) => {
                return Err(ToolError::BadInput(
                    "pass configurationName or filePath, not both".to_string(),
                ));
            }
            (Some(name), None) => Some(name),
            (None, Some(path)) => {
                let line = input.line.unwrap_or(0);
                if line == 0 {
                    return Err(ToolError::BadInput(
                        "a temporary debug configuration needs line".to_string(),
                    ));
                }
                let path = resolve(scope, &path)?;
                Some(self.configuration_at(scope, &path, line)?)
            }
            (None, None) => {
                return Err(ToolError::BadInput(
                    "pass configurationName or filePath with line".to_string(),
                ));
            }
        };
        match self
            .backend
            .debug_sessions(scope, &DebugQuery::Start(query))?
        {
            DebugAnswer::Started(session) => Ok(xdebug::started(&session)),
            _ => Err(ToolError::Failed(
                "the debug window answered the start wrongly".to_string(),
            )),
        }
    }

    /// The run configuration a place of a file starts, as the Run window names it.
    fn configuration_at(
        &self,
        scope: &SessionScope,
        file: &Path,
        line: u32,
    ) -> Result<String, ToolError> {
        let points = self.backend.run_points(
            scope,
            &RunPointQuery {
                file_path: file.to_path_buf(),
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
        let configurations = self.backend.run_configurations(scope)?;
        configurations
            .iter()
            .find(|configuration| {
                configuration.name == simple
                    || configuration.name == point.container
                    || configuration.name == point.name
            })
            .map(|configuration| configuration.name.clone())
            .ok_or_else(|| {
                ToolError::Failed(format!(
                    "the project has no configuration for {}",
                    point.container
                ))
            })
    }

    fn xdebug_control(&self, scope: &SessionScope, input: &Value) -> Result<String, ToolError> {
        let action = input
            .get("action")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::BadInput("action is required".to_string()))?;
        let Some(action) = DebugControlAction::parse(action) else {
            return Err(ToolError::BadInput(format!(
                "unknown action {action}: pass STEP_INTO, STEP_OVER, STEP_OUT, RESUME, PAUSE, \
                 STOP, WAIT_FOR_PAUSE or DRAIN_EVENTS"
            )));
        };
        let requested = xdebug::session_id_of(input)?;
        let session_id = self.debug_session_id(scope, requested.as_deref())?;
        let timeout_ms = input
            .get("timeout")
            .and_then(Value::as_u64)
            .unwrap_or(30_000)
            .min(600_000);
        let events_limit = input
            .get("eventsLimit")
            .and_then(Value::as_u64)
            .unwrap_or(100)
            .min(1_000) as usize;
        let query = DebugControlQuery {
            session_id: Some(session_id.clone()),
            action,
            timeout_ms,
            events_limit,
        };
        let mut outcome = match self
            .backend
            .debug_sessions(scope, &DebugQuery::Control(query))?
        {
            DebugAnswer::Controlled(outcome) => outcome,
            _ => {
                return Err(ToolError::Failed(
                    "the debug window answered the control wrongly".to_string(),
                ));
            }
        };
        if outcome.paused && outcome.frame_values.is_empty() {
            let values = DebugValuesQuery {
                session_id: Some(session_id),
                frame_index: 0,
                depth: 1,
            };
            if let DebugAnswer::Values(values) = self
                .backend
                .debug_sessions(scope, &DebugQuery::Values(values))?
            {
                outcome.frame_values = values.values;
            }
        }
        Ok(xdebug::json(&outcome))
    }

    fn xdebug_run_to_line(&self, scope: &SessionScope, input: &Value) -> Result<String, ToolError> {
        let requested = xdebug::session_id_of(input)?;
        let session_id = self.debug_session_id(scope, requested.as_deref())?;
        let path = input
            .get("filePath")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::BadInput("filePath is required".to_string()))?;
        let line = input.get("line").and_then(Value::as_u64).unwrap_or(0);
        if line == 0 {
            return Err(ToolError::BadInput("lines start at 1".to_string()));
        }
        let query = xdebug::DebugRunToLineQuery {
            session_id: Some(session_id),
            file: resolve(scope, Path::new(path))?,
            line: line as u32,
            timeout_ms: input
                .get("timeout")
                .and_then(Value::as_u64)
                .unwrap_or(30_000)
                .min(600_000),
        };
        match self
            .backend
            .debug_sessions(scope, &DebugQuery::RunToLine(query))?
        {
            DebugAnswer::Controlled(outcome) => Ok(xdebug::json(&outcome)),
            _ => Err(ToolError::Failed(
                "the debug window answered the run to line wrongly".to_string(),
            )),
        }
    }

    fn xdebug_threads(&self, scope: &SessionScope, input: &Value) -> Result<String, ToolError> {
        let input: xdebug::ThreadsInput = parse(input)?;
        let session_id = self.debug_session_id(scope, input.session_id.as_deref())?;
        let query = DebugThreadsQuery {
            session_id: Some(session_id),
            limit: input.limit.unwrap_or(50).clamp(1, 200),
            offset: input.offset.unwrap_or(0),
        };
        match self
            .backend
            .debug_sessions(scope, &DebugQuery::Threads(query))?
        {
            DebugAnswer::Threads(page) => Ok(xdebug::json(&json!({
                "threads": page.threads,
                "total": page.total,
            }))),
            _ => Err(ToolError::Failed(
                "the debug window answered the threads wrongly".to_string(),
            )),
        }
    }

    fn xdebug_stack(&self, scope: &SessionScope, input: &Value) -> Result<String, ToolError> {
        let input: xdebug::FramesInput = parse(input)?;
        let session_id = self.debug_session_id(scope, input.session_id.as_deref())?;
        let thread = match input.thread_id.as_ref() {
            None | Some(Value::Null) => None,
            Some(Value::Number(number)) => Some(
                number
                    .as_i64()
                    .map(DebugThreadSelector::Id)
                    .ok_or_else(|| {
                        ToolError::BadInput("threadId is a thread id or a name".to_string())
                    })?,
            ),
            Some(Value::String(text)) => Some(DebugThreadSelector::Name(text.clone())),
            Some(_) => {
                return Err(ToolError::BadInput(
                    "threadId is a thread id or a name".to_string(),
                ));
            }
        };
        let query = DebugFramesQuery {
            session_id: Some(session_id),
            thread,
            limit: input.limit.unwrap_or(200).clamp(1, 1_000),
            offset: input.offset.unwrap_or(0),
        };
        match self
            .backend
            .debug_sessions(scope, &DebugQuery::Frames(query))?
        {
            DebugAnswer::Frames(page) => Ok(xdebug::json(&json!({
                "threadId": page.thread_id,
                "threadName": page.thread_name,
                "frames": page.frames,
                "total": page.total,
            }))),
            _ => Err(ToolError::Failed(
                "the debug window answered the stack wrongly".to_string(),
            )),
        }
    }

    fn xdebug_frame_values(
        &self,
        scope: &SessionScope,
        input: &Value,
    ) -> Result<String, ToolError> {
        let input: xdebug::ValuesInput = parse(input)?;
        let session_id = self.debug_session_id(scope, input.session_id.as_deref())?;
        let frame_index = input.frame_index.unwrap_or(0);
        let query = DebugValuesQuery {
            session_id: Some(session_id),
            frame_index,
            depth: input.depth.unwrap_or(0).min(5),
        };
        match self
            .backend
            .debug_sessions(scope, &DebugQuery::Values(query))?
        {
            DebugAnswer::Values(values) => Ok(xdebug::json(&json!({
                "frameIndex": frame_index,
                "values": values.values,
            }))),
            _ => Err(ToolError::Failed(
                "the debug window answered the values wrongly".to_string(),
            )),
        }
    }

    fn xdebug_value_by_path(
        &self,
        scope: &SessionScope,
        input: &Value,
    ) -> Result<String, ToolError> {
        let input: xdebug::PathInput = parse(input)?;
        let session_id = self.debug_session_id(scope, input.session_id.as_deref())?;
        let path = xdebug::parse_path(Some(&input.path))?;
        let depth = input.depth.unwrap_or(0).min(5);
        let frame_index = input.frame_index.unwrap_or(0);
        let query = DebugValuesQuery {
            session_id: Some(session_id),
            frame_index,
            depth: path.len() + depth,
        };
        let values = match self
            .backend
            .debug_sessions(scope, &DebugQuery::Values(query))?
        {
            DebugAnswer::Values(values) => values,
            _ => {
                return Err(ToolError::Failed(
                    "the debug window answered the values wrongly".to_string(),
                ));
            }
        };
        let Some(value) = xdebug::value_at(&values.values, &path) else {
            return Err(ToolError::BadInput(format!(
                "the frame has no value at {}",
                xdebug::path_text(&path)
            )));
        };
        let mut value = value.clone();
        if depth == 0 {
            value.children.clear();
        }
        Ok(xdebug::json(&json!({
            "path": path,
            "value": value,
        })))
    }

    fn xdebug_evaluate(&self, scope: &SessionScope, input: &Value) -> Result<String, ToolError> {
        let input: xdebug::ExpressionInput = parse(input)?;
        let expression = input.expression.trim();
        if expression.is_empty() {
            return Err(ToolError::BadInput("the expression is empty".to_string()));
        }
        let session_id = self.debug_session_id(scope, input.session_id.as_deref())?;
        let query = DebugExpressionQuery {
            session_id: Some(session_id),
            frame_index: input.frame_index.or(Some(0)),
            expression: expression.to_string(),
            depth: input.depth.unwrap_or(0).min(5),
        };
        match self
            .backend
            .debug_sessions(scope, &DebugQuery::Evaluate(query))?
        {
            DebugAnswer::Value(value) => Ok(xdebug::json(&json!({
                "expression": expression,
                "value": value,
            }))),
            _ => Err(ToolError::Failed(
                "the debug window answered the expression wrongly".to_string(),
            )),
        }
    }

    fn xdebug_set_variable(
        &self,
        scope: &SessionScope,
        input: &Value,
    ) -> Result<String, ToolError> {
        let input: xdebug::SetVariableInput = parse(input)?;
        let new_value = input.new_value.trim();
        if new_value.is_empty() {
            return Err(ToolError::BadInput("the new value is empty".to_string()));
        }
        let path = xdebug::parse_path(Some(&input.path))?;
        let session_id = self.debug_session_id(scope, input.session_id.as_deref())?;
        let query = DebugSetVariableQuery {
            session_id: Some(session_id),
            frame_index: input.frame_index.unwrap_or(0),
            path,
            new_value: new_value.to_string(),
        };
        match self
            .backend
            .debug_sessions(scope, &DebugQuery::SetVariable(query))?
        {
            DebugAnswer::Set(outcome) => Ok(xdebug::json(&outcome)),
            _ => Err(ToolError::Failed(
                "the debug window answered the new value wrongly".to_string(),
            )),
        }
    }

    fn xdebug_list_breakpoints(
        &self,
        scope: &SessionScope,
        input: &Value,
    ) -> Result<String, ToolError> {
        let input: xdebug::ListBreakpointsInput = parse(input)?;
        let file = match input.file_path {
            Some(path) => Some(resolve(scope, &path)?),
            None => None,
        };
        match self
            .backend
            .debug_sessions(scope, &DebugQuery::Breakpoints(file))?
        {
            DebugAnswer::Breakpoints(points) => Ok(xdebug::json(&json!({
                "breakpoints": points,
            }))),
            _ => Err(ToolError::Failed(
                "the debug window answered the breakpoints wrongly".to_string(),
            )),
        }
    }

    fn xdebug_set_breakpoint(
        &self,
        scope: &SessionScope,
        input: &Value,
    ) -> Result<String, ToolError> {
        let input: xdebug::SetBreakpointInput = parse(input)?;
        let condition = xdebug::condition_of(input.condition.as_ref())?;
        if input.is_log_stack == Some(true) {
            return Err(ToolError::Unsupported(
                "this debugger cannot log the stack of a logpoint".to_string(),
            ));
        }
        let suspend_policy = match input.suspend_policy.as_deref() {
            None => None,
            Some("ALL") => Some("ALL".to_string()),
            Some("NONE") => Some("NONE".to_string()),
            Some("THREAD") => {
                return Err(ToolError::Unsupported(
                    "this debugger suspends every thread: THREAD is not supported".to_string(),
                ));
            }
            Some(other) => {
                return Err(ToolError::BadInput(format!(
                    "suspendPolicy is ALL, THREAD or NONE, not {other}"
                )));
            }
        };
        let (file_path, line) = match (&input.breakpoint_id, &input.file_path, input.line) {
            (Some(id), _, _) => {
                let (path, line) = split_breakpoint_id(id)?;
                (Some(resolve(scope, Path::new(&path))?), Some(line))
            }
            (None, Some(path), Some(line)) => (Some(resolve(scope, path)?), Some(line)),
            (None, None, _) | (None, Some(_), None) => {
                return Err(ToolError::BadInput(
                    "pass breakpointId, or filePath with line".to_string(),
                ));
            }
        };
        let Some(line) = line.filter(|line| *line > 0) else {
            return Err(ToolError::BadInput("lines start at 1".to_string()));
        };
        if input.is_log_message == Some(true) && condition.is_none() {
            return Err(ToolError::BadInput(
                "a log message needs a condition expression".to_string(),
            ));
        }
        let owner = match input.owner.as_deref() {
            None => Some(BreakpointOwner::Agent),
            Some(text) => Some(
                BreakpointOwner::parse(text)
                    .ok_or_else(|| ToolError::BadInput("owner is agent or user".to_string()))?,
            ),
        };
        let query = DebugBreakpointQuery {
            breakpoint_id: input.breakpoint_id,
            file_path,
            line: Some(line),
            condition,
            is_log_message: input.is_log_message,
            is_log_stack: input.is_log_stack,
            temporary: input.temporary,
            suspend_policy,
            hit_count: input.hit_count,
            enabled: input.enabled,
            owner,
        };
        match self
            .backend
            .debug_sessions(scope, &DebugQuery::SetBreakpoint(query))?
        {
            DebugAnswer::Breakpoint(point) => Ok(xdebug::json(&point)),
            _ => Err(ToolError::Failed(
                "the debug window answered the breakpoint wrongly".to_string(),
            )),
        }
    }

    fn xdebug_remove_breakpoint(
        &self,
        scope: &SessionScope,
        input: &Value,
    ) -> Result<String, ToolError> {
        let input: xdebug::RemoveBreakpointsInput = parse(input)?;
        let owner = match input.owner.as_deref() {
            None => BreakpointOwner::Agent,
            Some(text) => BreakpointOwner::parse(text)
                .ok_or_else(|| ToolError::BadInput("owner is agent or user".to_string()))?,
        };
        let (breakpoint_id, file_path, line) = match input.breakpoint_id {
            Some(id) => {
                let (path, line) = split_breakpoint_id(&id)?;
                (
                    Some(id),
                    Some(resolve(scope, Path::new(&path))?),
                    Some(line),
                )
            }
            None => (
                None,
                match input.file_path {
                    Some(path) => Some(resolve(scope, &path)?),
                    None => None,
                },
                input.line,
            ),
        };
        if owner == BreakpointOwner::User
            && breakpoint_id.is_none()
            && file_path.is_none()
            && line.is_none()
        {
            return Err(ToolError::Denied(
                "an agent cannot remove every breakpoint of the user".to_string(),
            ));
        }
        let allows_all = owner == BreakpointOwner::Agent;
        let query = DebugRemoveBreakpointsQuery {
            breakpoint_id,
            file_path,
            line,
            owner: Some(owner),
            all_of_owner_allowed: allows_all,
        };
        match self
            .backend
            .debug_sessions(scope, &DebugQuery::RemoveBreakpoints(query))?
        {
            DebugAnswer::Removed(removed) => Ok(xdebug::json(&json!({ "removed": removed }))),
            _ => Err(ToolError::Failed(
                "the debug window answered the removal wrongly".to_string(),
            )),
        }
    }

    /// Asks the Database window and cleans the address of every answer.
    fn database(&self, scope: &SessionScope, query: &DatabaseQuery) -> Result<String, ToolError> {
        let answer = self.backend.database(scope, query)?;
        Ok(database_tools::json_answer(clean_answer(answer)))
    }

    fn database_id(
        &self,
        scope: &SessionScope,
        input: &Value,
        kind: InputId,
    ) -> Result<String, ToolError> {
        let input: database_tools::DatabaseIdInput = parse(input)?;
        let id = input.id()?;
        let query = match kind {
            InputId::Test => DatabaseQuery::TestConnection { id },
            InputId::Schemas => DatabaseQuery::ListSchemas {
                connection_id: id,
                selected_only: input.selected_only,
            },
            InputId::Kinds => DatabaseQuery::ObjectKinds { connection_id: id },
            InputId::Recent => DatabaseQuery::RecentQueries { connection_id: id },
        };
        self.database(scope, &query)
    }

    fn database_create(&self, scope: &SessionScope, input: &Value) -> Result<String, ToolError> {
        let input: database_tools::CreateInput = parse(input)?;
        if database_tools::dbms_name(&input.dbms).is_none() {
            return Err(database_tools::unknown_dbms(&input.dbms));
        }
        if input.name.trim().is_empty() {
            return Err(ToolError::BadInput("the name is empty".to_string()));
        }
        let query = DatabaseQuery::CreateConnection {
            name: input.name,
            dbms: input.dbms,
            url: input.url,
            need_to_check: input.need_to_check.unwrap_or(true),
        };
        self.database(scope, &query)
    }

    fn database_edit(&self, scope: &SessionScope, input: &Value) -> Result<String, ToolError> {
        let input: database_tools::EditInput = parse(input)?;
        if let Some(dbms) = &input.dbms
            && database_tools::dbms_name(dbms).is_none()
        {
            return Err(database_tools::unknown_dbms(dbms));
        }
        let query = DatabaseQuery::EditConnection {
            connection_id: input.connection_id,
            dbms: input.dbms,
            url: input.url,
            need_to_check: input.need_to_check.unwrap_or(true),
        };
        self.database(scope, &query)
    }

    fn database_introspect(
        &self,
        scope: &SessionScope,
        input: &Value,
    ) -> Result<String, ToolError> {
        let input: database_tools::IntrospectInput = parse(input)?;
        let query = DatabaseQuery::Introspect {
            connection_id: input.connection_id,
            database_name: input.database_name,
            schema_name: input.schema_name,
        };
        self.database(scope, &query)
    }

    fn database_objects(&self, scope: &SessionScope, input: &Value) -> Result<String, ToolError> {
        let input: database_tools::ObjectInput = parse(input)?;
        let query = DatabaseQuery::ListObjects {
            connection_id: input.connection_id,
            schema_name: input.schema_name,
            database_name: input.database_name,
            kind: input.kind,
        };
        self.database(scope, &query)
    }

    fn database_describe(&self, scope: &SessionScope, input: &Value) -> Result<String, ToolError> {
        let input: database_tools::DescribeInput = parse(input)?;
        let query = DatabaseQuery::Describe {
            connection_id: input.connection_id,
            database_name: input.database_name,
            schema_name: input.schema_name,
            kind: input.kind,
            object_name: input.object_name,
        };
        self.database(scope, &query)
    }

    fn database_execute(&self, scope: &SessionScope, input: &Value) -> Result<String, ToolError> {
        let input: database_tools::ExecuteInput = parse(input)?;
        if input.query_text.trim().is_empty() {
            return Err(ToolError::BadInput("the statement is empty".to_string()));
        }
        let query = DatabaseQuery::Execute {
            connection_id: input.connection_id,
            query_text: input.query_text,
        };
        match self.backend.database(scope, &query)? {
            DatabaseAnswer::Rows(rows) => {
                let result = self.results.store(rows.clone());
                Ok(database_tools::executed(&rows, Some(&result)))
            }
            _ => Err(ToolError::Failed(
                "the Database window answered the statement wrongly".to_string(),
            )),
        }
    }

    fn database_fetch(&self, input: &Value) -> Result<String, ToolError> {
        let input: database_tools::FetchInput = parse(input)?;
        let Some((csv, count, more)) = self.results.page(&input.result_set_id, input.offset) else {
            return Err(ToolError::BadInput(format!(
                "the result {} is gone: run the statement again",
                input.result_set_id
            )));
        };
        Ok(database_tools::fetched(
            &input.result_set_id,
            input.offset,
            &csv,
            count,
            more,
        ))
    }

    fn database_preview(&self, scope: &SessionScope, input: &Value) -> Result<String, ToolError> {
        let input: database_tools::PreviewInput = parse(input)?;
        let query = DatabaseQuery::PreviewTable {
            connection_id: input.connection_id,
            schema_name: input.schema_name,
            database_name: input.database_name,
            table_name: input.table_name,
            max_row_count: input.max_row_count.unwrap_or(100).clamp(1, 10_000),
        };
        match self.backend.database(scope, &query)? {
            DatabaseAnswer::Rows(rows) => Ok(database_tools::preview(&rows)),
            _ => Err(ToolError::Failed(
                "the Database window answered the preview wrongly".to_string(),
            )),
        }
    }

    fn database_cancel(&self, scope: &SessionScope, input: &Value) -> Result<String, ToolError> {
        let input: database_tools::CancelInput = parse(input)?;
        match self.backend.database(
            scope,
            &DatabaseQuery::Cancel {
                session_id: input.session_id,
            },
        )? {
            DatabaseAnswer::Cancelled(cancelled) => Ok(database_tools::json_answer(
                DatabaseAnswer::Cancelled(cancelled),
            )),
            _ => Err(ToolError::Failed(
                "the Database window answered the cancel wrongly".to_string(),
            )),
        }
    }

    fn create_new_file(&self, scope: &SessionScope, input: &Value) -> Result<String, ToolError> {
        let input: CreateFileInput = parse(input)?;
        let path = resolve(scope, &input.path_in_project)?;
        if path.exists() && !input.overwrite {
            return Err(ToolError::BadInput(format!(
                "{} already exists",
                path.display()
            )));
        }
        let query = ProposeEditQuery {
            path: path.clone(),
            new_text: Some(input.text.unwrap_or_default()),
            replacements: Vec::new(),
            delete: false,
        };
        let edit = self.backend.propose_edit(scope, &query)?;
        Ok(render::created_file(scope, &edit))
    }

    fn directory_tree(&self, scope: &SessionScope, input: &Value) -> Result<String, ToolError> {
        let input: DirectoryInput = parse(input)?;
        let folder = match input.directory_path {
            Some(path) => resolve(scope, &path)?,
            None => scope.project.clone(),
        };
        if !folder.is_dir() {
            return Err(ToolError::BadInput(format!(
                "{} is not a folder",
                folder.display()
            )));
        }
        let tree = crate::tree::directory_tree(&folder, scope, input.max_depth.clamp(1, 20));
        Ok(render::directory_tree(scope, &tree))
    }

    fn apply_patch(
        &self,
        scope: &SessionScope,
        input: &Value,
        _client: Option<&str>,
    ) -> Result<String, ToolError> {
        let input: PatchInput = parse(input)?;
        let text = input
            .input
            .or(input.patch)
            .ok_or_else(|| ToolError::BadInput("pass input or patch".to_string()))?;
        let files = crate::patch::parse(&text).map_err(ToolError::BadInput)?;
        let mut results = Vec::new();
        for file in &files {
            let path =
                crate::patch::path_inside(&scope.project, &file.path).map_err(ToolError::Denied)?;
            let target = match &file.move_to {
                Some(to) => {
                    crate::patch::path_inside(&scope.project, to).map_err(ToolError::Denied)?
                }
                None => path.clone(),
            };
            if file.delete {
                let query = ProposeEditQuery {
                    path: path.clone(),
                    new_text: None,
                    replacements: Vec::new(),
                    delete: true,
                };
                let edit = self.backend.propose_edit(scope, &query)?;
                results.push((file.path.clone(), render::patch_outcome(&edit)));
                continue;
            }
            let base = if path.exists() {
                self.backend.file_text(scope, &path)?.text
            } else {
                String::new()
            };
            let new_text = file.text(&base).map_err(ToolError::BadInput)?;
            let query = ProposeEditQuery {
                path: target.clone(),
                new_text: Some(new_text),
                replacements: Vec::new(),
                delete: false,
            };
            let edit = self.backend.propose_edit(scope, &query)?;
            results.push((file.path.clone(), render::patch_outcome(&edit)));
            if target != path {
                let query = ProposeEditQuery {
                    path,
                    new_text: None,
                    replacements: Vec::new(),
                    delete: true,
                };
                let _ = self.backend.propose_edit(scope, &query)?;
            }
        }
        Ok(render::patch_results(scope, &results))
    }

    fn rename_refactoring(&self, scope: &SessionScope, input: &Value) -> Result<String, ToolError> {
        let input: RenameRefactoringInput = parse(input)?;
        if input.symbol_name.trim().is_empty() || input.new_name.trim().is_empty() {
            return Err(ToolError::BadInput(
                "symbolName and newName cannot be empty".to_string(),
            ));
        }
        let path = resolve(scope, &input.path_in_project)?;
        let content = self.backend.file_text(scope, &path)?;
        let (line, column) = locate_word(&content.text, &input.symbol_name).ok_or_else(|| {
            ToolError::BadInput(format!(
                "{} was not found in {}",
                input.symbol_name,
                input.path_in_project.display()
            ))
        })?;
        let query = RenameSymbolQuery {
            path,
            line,
            column,
            new_name: input.new_name.clone(),
        };
        let rename = self.backend.rename_symbol(scope, &query)?;
        Ok(render::renamed_symbol(&rename))
    }

    fn search_files(&self, scope: &SessionScope, input: &Value) -> Result<String, ToolError> {
        let input: SearchInput = parse(input)?;
        if input.q.trim().is_empty() {
            return Err(ToolError::BadInput("the glob is empty".to_string()));
        }
        let query = PathQuery {
            pattern: input.q.clone(),
            paths: input.paths.clone(),
            include_excluded: input.include_excluded,
            limit: input.limit,
        }
        .clipped();
        let page = self.backend.find_files(scope, &query)?;
        Ok(render::path_search(scope, &query, &page))
    }

    fn search_regex(&self, scope: &SessionScope, input: &Value) -> Result<String, ToolError> {
        let input: SearchInput = parse(input)?;
        if input.q.is_empty() {
            return Err(ToolError::BadInput("the pattern is empty".to_string()));
        }
        let query = SearchQuery {
            pattern: input.q.clone(),
            regex: true,
            mask: None,
            paths: input.paths.clone(),
            limit: input.limit,
        };
        let page = self.backend.search_text(scope, &query)?;
        let page = filter_search(scope, &query.paths, page);
        Ok(render::search_matches(scope, &query, &page))
    }

    fn search_symbol(&self, scope: &SessionScope, input: &Value) -> Result<String, ToolError> {
        let input: SearchInput = parse(input)?;
        let _ = input.include_external;
        if input.q.trim().is_empty() {
            return Err(ToolError::BadInput("the query is empty".to_string()));
        }
        let query = SymbolQuery {
            query: input.q.clone(),
            kind: None,
            limit: input.limit.clamp(1, 500),
            after: None,
        };
        let page = self.backend.search_symbols(scope, &query)?;
        Ok(render::symbol_search(scope, &query, &page, &input.paths))
    }

    fn skill_search(&self, scope: &SessionScope, input: &Value) -> Result<String, ToolError> {
        let input: SkillSearchInput = parse(input)?;
        let tool = match input.mode.as_str() {
            "file" => SEARCH_FILE,
            "text" => SEARCH_TEXT,
            "regex" => SEARCH_REGEX,
            "symbol" => SEARCH_SYMBOL,
            other => {
                return Err(ToolError::BadInput(format!(
                    "unknown mode {other}, pass file, text, regex or symbol"
                )));
            }
        };
        let mut arguments = serde_json::Map::new();
        arguments.insert("q".to_string(), Value::String(input.q));
        arguments.insert("limit".to_string(), Value::from(input.limit));
        if !input.paths.is_empty() {
            arguments.insert(
                "paths".to_string(),
                Value::Array(input.paths.into_iter().map(Value::String).collect()),
            );
        }
        match tool {
            SEARCH_FILE => {
                arguments.insert(
                    "includeExcluded".to_string(),
                    Value::Bool(input.include_excluded),
                );
            }
            SEARCH_SYMBOL => {
                arguments.insert(
                    "include_external".to_string(),
                    Value::Bool(input.include_external),
                );
            }
            _ => {}
        }
        let arguments = Value::Object(arguments);
        self.dispatch(None, scope, tool, &arguments)
    }

    fn execute_terminal(&self, scope: &SessionScope, input: &Value) -> Result<String, ToolError> {
        let query: TerminalQuery = parse::<TerminalQuery>(input)?.clipped();
        if query.command.trim().is_empty() {
            return Err(ToolError::BadInput("the command is empty".to_string()));
        }
        let output = self.backend.execute_terminal(scope, &query)?;
        Ok(render::terminal_output(&output))
    }

    fn execute_tool(
        &self,
        client: Option<&str>,
        scope: &SessionScope,
        input: &Value,
    ) -> Result<String, ToolError> {
        let input: ExecuteToolInput = parse(input)?;
        let (name, arguments) = parse_command(&input.command)?;
        if name == EXECUTE_TOOL {
            return Err(ToolError::BadInput(
                "execute_tool cannot call itself".to_string(),
            ));
        }
        let client = client.map(|name| ClientInfo {
            name: name.to_string(),
            version: String::new(),
        });
        self.call_with(client.as_ref(), scope, &name, &arguments, true)
    }
}

/// The byte place of a character offset, so a slice never splits a character.
fn byte_of_char(text: &str, offset: usize) -> usize {
    text.char_indices()
        .nth(offset)
        .map_or(text.len(), |(index, _)| index)
}

/// The block around a line at the indentation of that line, down to `levels` deeper.
fn indentation_block(lines: &[&str], start: usize, levels: usize) -> (usize, usize) {
    let anchored = lines.get(start - 1).copied().unwrap_or_default();
    let indent = anchored.len() - anchored.trim_start().len();
    let mut end = start;
    let mut deeper = 0;
    for (index, line) in lines.iter().enumerate().skip(start) {
        if line.trim().is_empty() {
            end = index + 1;
            continue;
        }
        let width = line.len() - line.trim_start().len();
        if width < indent {
            break;
        }
        if width > indent {
            deeper += 1;
            if deeper > levels {
                break;
            }
        }
        end = index + 1;
    }
    (start, end.max(start))
}

/// The place of a word with boundaries, from 1, for a rename by the name of a declaration.
fn locate_word(text: &str, name: &str) -> Option<(u32, u32)> {
    let bytes = text.as_bytes();
    let needle = name.as_bytes();
    let part = |byte: u8| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$');
    let mut at = 0;
    while at + needle.len() <= bytes.len() {
        let found = text[at..].find(name)?;
        let start = at + found;
        let before = start.checked_sub(1).and_then(|index| bytes.get(index));
        let after = bytes.get(start + needle.len());
        if !before.is_some_and(|byte| part(*byte)) && !after.is_some_and(|byte| part(*byte)) {
            let line = text[..start].matches('\n').count() as u32 + 1;
            let line_start = text[..start].rfind('\n').map_or(0, |index| index + 1);
            let column = text[line_start..start].chars().count() as u32 + 1;
            return Some((line, column));
        }
        at = start + needle.len();
    }
    None
}

/// `name {json}` or `name key=value key=value`, as `execute_tool` takes a command.
fn parse_command(text: &str) -> Result<(String, Value), ToolError> {
    let text = text.trim();
    let name_end = text
        .find(|character: char| character.is_whitespace() || matches!(character, '{'))
        .unwrap_or(text.len());
    let name = text[..name_end].trim();
    if name.is_empty() {
        return Err(ToolError::BadInput(
            "the command has no tool name".to_string(),
        ));
    }
    let rest = text[name_end..].trim();
    if rest.is_empty() {
        return Ok((name.to_string(), Value::Object(serde_json::Map::new())));
    }
    if rest.starts_with('{') {
        let value: Value = serde_json::from_str(rest)
            .map_err(|error| ToolError::BadInput(format!("the arguments are not JSON: {error}")))?;
        if !value.is_object() {
            return Err(ToolError::BadInput(
                "the arguments have to be a JSON object".to_string(),
            ));
        }
        return Ok((name.to_string(), value));
    }
    let mut arguments = serde_json::Map::new();
    for pair in rest.split_whitespace() {
        let (key, value) = pair
            .split_once('=')
            .ok_or_else(|| ToolError::BadInput(format!("{pair} is not key=value")))?;
        let value =
            serde_json::from_str(value).unwrap_or_else(|_| Value::String(value.to_string()));
        arguments.insert(key.to_string(), value);
    }
    Ok((name.to_string(), Value::Object(arguments)))
}

/// Whether a read path is an address of a class the language service can show.
fn is_decompiled_address(path: &str) -> bool {
    path.starts_with("jdt://")
        || path.starts_with("jar://")
        || path.starts_with("jrt://")
        || path.starts_with("file://")
}

/// Keeps only the matches inside `paths`, and says how many were left.
fn filter_search(
    scope: &SessionScope,
    paths: &[String],
    mut page: crate::backend::SearchPage,
) -> crate::backend::SearchPage {
    if paths.is_empty() {
        return page;
    }
    page.items.retain(|item| {
        crate::path_allowed(paths, &crate::relative_path(&scope.project, &item.path))
    });
    page.total = page.items.len();
    page.files = page
        .items
        .iter()
        .map(|item| item.path.clone())
        .collect::<std::collections::HashSet<_>>()
        .len();
    page
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::backend::{
        BreakpointId, BreakpointInfo, BreakpointQuery, CallHierarchy, CallHierarchyQuery,
        Confidence, DebugEvaluateQuery, DebugFrame, DebugRunOutcome, DebugRunRequest, DebugScope,
        DebugSessionState, DebugStack, DebugStackQuery, DebugStepQuery, DebugThread, DebugValue,
        DebugVariable, DebugVariables, DebugVariablesQuery, DebugWaitQuery, Diagnostic,
        DiagnosticsPage, EditOutcome, EditorContext, FileText, HierarchyNode, Location,
        ProjectModule, ProjectStructure, ProjectStructureQuery, ProposeEditQuery, ProposedEdit,
        READ_LINE_LIMIT, ReadFileQuery, SearchHit, SearchPage, SearchQuery, Severity, SymbolHit,
        SymbolInfo, SymbolPage, TypeHierarchy, TypeHierarchyQuery, Usage, UsagePage, UsagesQuery,
        VcsChanges, VcsDiff, VcsFile, VcsHunk, VcsQuery,
    };
    use crate::exposure::{RouterScope, ToolState};
    use crate::tools::TOOL_NAMES;

    struct Fake;

    impl ToolBackend for Fake {
        fn editor_context(&self, _scope: &SessionScope) -> Result<EditorContext, ToolError> {
            Ok(EditorContext {
                file: Some("src/Foo.txt".into()),
                line: Some(3),
                column: Some(7),
                ..EditorContext::default()
            })
        }

        fn find_symbol(
            &self,
            scope: &SessionScope,
            query: &SymbolQuery,
        ) -> Result<SymbolPage, ToolError> {
            Ok(SymbolPage {
                hits: vec![SymbolHit {
                    name: query.query.clone(),
                    kind: "class".to_string(),
                    path: scope.project.join("Foo.txt"),
                    line: 1,
                    content: "class Foo".to_string(),
                    before: Vec::new(),
                    after: Vec::new(),
                }],
                source: "fake".to_string(),
                confidence: Confidence::Certain,
                next: None,
            })
        }

        fn symbol_info(
            &self,
            _scope: &SessionScope,
            position: &Position,
        ) -> Result<SymbolInfo, ToolError> {
            Ok(SymbolInfo {
                name: format!("at {}", position.path.display()),
                kind: "method".to_string(),
                signature: None,
                documentation: None,
                declaration: None,
                source: "fake".to_string(),
                confidence: Confidence::Likely,
            })
        }

        fn diagnostics(
            &self,
            scope: &SessionScope,
            query: &DiagnosticsQuery,
        ) -> Result<DiagnosticsPage, ToolError> {
            let path = query.path.clone();
            Ok(DiagnosticsPage {
                items: vec![Diagnostic {
                    path: path.unwrap_or_else(|| scope.project.join("Foo.txt")),
                    line: 2,
                    column: 1,
                    severity: Severity::Error,
                    message: "broken".to_string(),
                    code: None,
                    content: "x".to_string(),
                }],
                source: "fake".to_string(),
                confidence: Confidence::Certain,
                next: None,
            })
        }

        fn find_usages(
            &self,
            scope: &SessionScope,
            query: &UsagesQuery,
        ) -> Result<UsagePage, ToolError> {
            let path = query
                .path
                .clone()
                .unwrap_or_else(|| scope.project.join("Foo.txt"));
            Ok(UsagePage {
                items: vec![Usage {
                    path,
                    line: 3,
                    column: 5,
                    content: "foo();".to_string(),
                }],
                total: 12,
                clipped: true,
                source: "fake".to_string(),
                confidence: Confidence::Certain,
            })
        }

        fn type_hierarchy(
            &self,
            scope: &SessionScope,
            _query: &TypeHierarchyQuery,
        ) -> Result<TypeHierarchy, ToolError> {
            Ok(TypeHierarchy {
                supertypes: vec![HierarchyNode {
                    name: "Base".to_string(),
                    detail: None,
                    location: Some(Location {
                        path: scope.project.join("Base.txt"),
                        line: 1,
                        content: "class Base".to_string(),
                    }),
                    depth: 1,
                }],
                subtypes: Vec::new(),
                source: "fake".to_string(),
                confidence: Confidence::Likely,
                clipped: false,
            })
        }

        fn call_hierarchy(
            &self,
            _scope: &SessionScope,
            query: &CallHierarchyQuery,
        ) -> Result<CallHierarchy, ToolError> {
            Ok(CallHierarchy {
                callers: vec![HierarchyNode {
                    name: format!("caller of depth {}", query.depth),
                    detail: Some("void run()".to_string()),
                    location: None,
                    depth: query.depth as u32,
                }],
                source: "fake".to_string(),
                confidence: Confidence::Certain,
                clipped: false,
            })
        }

        fn project_structure(
            &self,
            scope: &SessionScope,
            _query: &ProjectStructureQuery,
        ) -> Result<ProjectStructure, ToolError> {
            Ok(ProjectStructure {
                modules: vec![ProjectModule {
                    name: "app".to_string(),
                    path: ".".into(),
                    source_roots: vec![scope.project.join("src")],
                    test_roots: vec![scope.project.join("test")],
                    dependencies: vec!["group:lib:1".to_string()],
                    dependencies_clipped: false,
                }],
                total_modules: 1,
                clipped: false,
                build_system: "Fake".to_string(),
                build_system_version: Some("1".to_string()),
                resolved: true,
                runtime: Some("21".to_string()),
                language_level: Some("21".to_string()),
                problems: Vec::new(),
            })
        }

        fn search_text(
            &self,
            scope: &SessionScope,
            query: &SearchQuery,
        ) -> Result<SearchPage, ToolError> {
            Ok(SearchPage {
                items: vec![SearchHit {
                    path: scope.project.join("Foo.txt"),
                    line: 4,
                    column: 2,
                    content: format!("{}();", query.pattern),
                }],
                total: 1,
                clipped: false,
                files: 1,
            })
        }

        fn read_file(
            &self,
            _scope: &SessionScope,
            query: &ReadFileQuery,
        ) -> Result<FileText, ToolError> {
            let start = query.start_line.unwrap_or(1);
            let end = query.end_line.unwrap_or(start);
            Ok(FileText {
                path: query.path.clone(),
                text: format!("{start}-{end}"),
                start_line: start,
                end_line: end,
                total_lines: end,
                dirty: true,
                clipped: end - start + 1 >= READ_LINE_LIMIT,
            })
        }

        fn vcs_changes(
            &self,
            _scope: &SessionScope,
            query: &VcsQuery,
        ) -> Result<VcsChanges, ToolError> {
            Ok(VcsChanges {
                files: vec![VcsFile {
                    path: "Foo.txt".to_string(),
                    status: "modified".to_string(),
                    original: None,
                }],
                total: 1,
                clipped: false,
                branch: Some("main".to_string()),
                diff: query.path.as_ref().map(|path| VcsDiff {
                    path: path.display().to_string(),
                    hunks: vec![VcsHunk {
                        before: (2, 3),
                        after: (2, 4),
                        removed: vec!["old".to_string()],
                        added: vec!["new".to_string()],
                    }],
                    clipped: false,
                }),
            })
        }

        fn propose_edit(
            &self,
            _scope: &SessionScope,
            query: &ProposeEditQuery,
        ) -> Result<ProposedEdit, ToolError> {
            let hunks = query.replacements.len();
            Ok(ProposedEdit {
                path: query.path.clone(),
                outcome: EditOutcome::Partial,
                text: Some(
                    query
                        .new_text
                        .clone()
                        .unwrap_or_else(|| "edited".to_string()),
                ),
                hunks,
                kept: hunks.saturating_sub(1),
            })
        }

        fn set_breakpoint(
            &self,
            _scope: &SessionScope,
            query: &BreakpointQuery,
        ) -> Result<BreakpointInfo, ToolError> {
            Ok(BreakpointInfo {
                path: query.path.clone(),
                line: query.line,
                enabled: true,
                verified: true,
                condition: query.condition.clone(),
                log_message: query.log_message.clone(),
            })
        }

        fn remove_breakpoint(
            &self,
            _scope: &SessionScope,
            _id: &BreakpointId,
        ) -> Result<(), ToolError> {
            Ok(())
        }

        fn breakpoints(&self, scope: &SessionScope) -> Result<Vec<BreakpointInfo>, ToolError> {
            Ok(vec![BreakpointInfo {
                path: scope.project.join("Foo.txt"),
                line: 3,
                enabled: true,
                verified: false,
                condition: None,
                log_message: None,
            }])
        }

        fn debug_run(
            &self,
            _scope: &SessionScope,
            _request: &DebugRunRequest,
            _client: Option<&str>,
        ) -> Result<DebugRunOutcome, ToolError> {
            Ok(DebugRunOutcome {
                session: "debug-1".to_string(),
                state: stopped_state(),
            })
        }

        fn debug_wait(
            &self,
            _scope: &SessionScope,
            query: &DebugWaitQuery,
        ) -> Result<DebugSessionState, ToolError> {
            Ok(DebugSessionState {
                reason: Some(format!("{} s", query.seconds)),
                ..stopped_state()
            })
        }

        fn debug_stack(
            &self,
            _scope: &SessionScope,
            query: &DebugStackQuery,
        ) -> Result<DebugStack, ToolError> {
            let frames = (0..query.frames)
                .map(|index| DebugFrame {
                    index,
                    name: format!("frame {index}"),
                    path: None,
                    line: 0,
                    column: 0,
                })
                .collect();
            Ok(DebugStack {
                thread: Some(DebugThread {
                    id: 1,
                    name: "main".to_string(),
                }),
                total: query.frames + 2,
                frames,
            })
        }

        fn debug_variables(
            &self,
            _scope: &SessionScope,
            query: &DebugVariablesQuery,
        ) -> Result<DebugVariables, ToolError> {
            let child = DebugVariable {
                name: "child".to_string(),
                value: "c".repeat(300),
                type_name: "String".to_string(),
                children: Vec::new(),
            };
            let items = DebugVariable {
                name: "items".to_string(),
                value: "ArrayList (2)".to_string(),
                type_name: "ArrayList".to_string(),
                children: if query.depth > 0 {
                    vec![child]
                } else {
                    Vec::new()
                },
            };
            Ok(DebugVariables {
                scopes: vec![DebugScope {
                    name: "Locals".to_string(),
                    variables: vec![items],
                }],
                total: 1,
                clipped: false,
            })
        }

        fn debug_evaluate(
            &self,
            _scope: &SessionScope,
            query: &DebugEvaluateQuery,
        ) -> Result<DebugValue, ToolError> {
            Ok(DebugValue {
                expression: query.expression.clone(),
                value: "42".to_string(),
                type_name: "int".to_string(),
            })
        }

        fn debug_step(
            &self,
            _scope: &SessionScope,
            query: &DebugStepQuery,
        ) -> Result<DebugSessionState, ToolError> {
            Ok(DebugSessionState {
                reason: Some(query.step.name().to_string()),
                ..stopped_state()
            })
        }

        fn debug_stop(&self, _scope: &SessionScope) -> Result<(), ToolError> {
            Ok(())
        }
    }

    fn stopped_state() -> DebugSessionState {
        DebugSessionState {
            state: "stopped".to_string(),
            reason: Some("breakpoint".to_string()),
            thread: Some(DebugThread {
                id: 1,
                name: "main".to_string(),
            }),
            frame: Some(DebugFrame {
                index: 0,
                name: "run".to_string(),
                path: Some("project/Foo.txt".into()),
                line: 12,
                column: 1,
            }),
        }
    }

    struct Allow;

    impl Approval for Allow {
        fn ask(&self, _: &ApprovalRequest) -> ApprovalAnswer {
            ApprovalAnswer::Once
        }
    }

    fn gateway() -> ToolGateway {
        ToolGateway::new(Arc::new(Fake), AuditLog::default())
    }

    fn scope() -> SessionScope {
        SessionScope {
            session: "s1".to_string(),
            project: "project".into(),
            cwd: "project".into(),
        }
    }

    #[test]
    fn a_read_tool_answers_and_leaves_a_trace() {
        let gateway = gateway();

        let text = gateway
            .call(&scope(), "find_symbol", &json!({"query": "Foo"}))
            .expect("call");

        assert!(text.contains(":1:class Foo"), "{text}");
        let audit = gateway.audit().recent();
        assert_eq!(audit.len(), 1);
        assert_eq!(audit[0].tool, "find_symbol");
        assert_eq!(audit[0].decision, "allow");
        assert!(audit[0].ok);
    }

    #[test]
    fn the_audit_log_does_not_keep_inputs() {
        let gateway = gateway();

        let _ = gateway.call(&scope(), "find_symbol", &json!({"query": "secret-value"}));

        let line = format!("{:?}", gateway.audit().recent());
        assert!(!line.contains("secret-value"));
    }

    #[test]
    fn the_editor_context_names_the_caret() {
        let text = gateway()
            .call(&scope(), "get_editor_context", &json!({}))
            .expect("call");

        assert!(text.starts_with("active: src/Foo.txt:3:7"), "{text}");
    }

    #[test]
    fn bad_input_and_unknown_tools_are_errors() {
        let gateway = gateway();

        let empty = gateway.call(&scope(), "find_symbol", &json!({"query": " "}));
        let missing = gateway.call(&scope(), "find_symbol", &json!({}));
        let unknown = gateway.call(&scope(), "rm", &json!({}));

        assert!(matches!(empty, Err(ToolError::BadInput(_))));
        assert!(matches!(missing, Err(ToolError::BadInput(_))));
        assert_eq!(unknown, Err(ToolError::UnknownTool("rm".to_string())));
    }

    #[test]
    fn symbol_info_gets_a_path_inside_the_project() {
        let gateway = gateway();
        let input = json!({"path": "src/./Foo.txt", "line": 3, "column": 9});

        let text = gateway
            .call(&scope(), "get_symbol_info", &input)
            .expect("call");

        let expected = Path::new("project").join("src").join("Foo.txt");
        assert!(text.contains(&expected.display().to_string()), "{text}");
    }

    #[cfg(windows)]
    #[test]
    fn paths_outside_the_project_are_refused() {
        let gateway = gateway();
        let input = json!({"path": "../project-other/secret.txt", "line": 1, "column": 1});
        let absolute = json!({"path": "C:/Windows/win.ini", "line": 1, "column": 1});

        let up = gateway.call(&scope(), "get_symbol_info", &input);
        let elsewhere = gateway.call(&scope(), "get_diagnostics", &json!({"path": "C:/x"}));
        let other = gateway.call(&scope(), "get_symbol_info", &absolute);

        assert!(matches!(up, Err(ToolError::Denied(_))), "{up:?}");
        assert!(
            matches!(elsewhere, Err(ToolError::Denied(_))),
            "{elsewhere:?}"
        );
        assert!(matches!(other, Err(ToolError::Denied(_))), "{other:?}");
    }

    #[test]
    fn diagnostics_take_a_severity_and_reject_zero_positions() {
        let gateway = gateway();

        let errors = gateway.call(&scope(), "get_diagnostics", &json!({"severity": "error"}));
        let bad = gateway.call(&scope(), "get_diagnostics", &json!({"severity": "fatal"}));
        let zero = gateway.call(
            &scope(),
            "get_symbol_info",
            &json!({"path": "a", "line": 0, "column": 1}),
        );

        let errors = errors.expect("call");
        assert!(errors.contains("error and worse"), "{errors}");
        assert!(errors.contains(":2:1: error: broken"), "{errors}");
        assert!(matches!(bad, Err(ToolError::BadInput(_))));
        assert!(matches!(zero, Err(ToolError::BadInput(_))));
    }

    #[test]
    fn running_code_needs_a_yes() {
        let spec = ToolSpec {
            name: "run",
            description: "",
            class: ToolClass::Execute,
            input_schema: Value::Null,
        };

        assert_eq!(Policy.decide(&spec), Decision::Ask);
    }

    #[test]
    fn the_new_read_tools_answer_with_their_parts() {
        let gateway = gateway();
        let placed = json!({"path": "Foo.txt", "line": 3, "column": 1});

        let usages = gateway
            .call(&scope(), "find_usages", &placed)
            .expect("usages");
        assert!(usages.starts_with("12 usages of "), "{usages}");
        assert!(usages.contains(":3:5: foo();"), "{usages}");
        assert!(usages.contains("clipped: showing 1 of 12"), "{usages}");

        let hierarchy = gateway
            .call(&scope(), "get_type_hierarchy", &placed)
            .expect("hierarchy");
        assert!(hierarchy.contains("supertypes (1):"), "{hierarchy}");
        assert!(hierarchy.contains("Base at "), "{hierarchy}");
        assert!(hierarchy.contains("subtypes: none"), "{hierarchy}");

        let calls = gateway
            .call(
                &scope(),
                "get_call_hierarchy",
                &json!({"path": "Foo.txt", "line": 3, "column": 1, "depth": 3}),
            )
            .expect("calls");
        assert!(calls.contains("3 levels"), "{calls}");
        assert!(calls.contains("caller of depth 3 (void run())"), "{calls}");

        let structure = gateway
            .call(&scope(), "get_project_structure", &json!({}))
            .expect("structure");
        assert!(
            structure.starts_with("project: Fake 1, resolved, 1 modules"),
            "{structure}"
        );
        assert!(
            structure.contains("runtime: 21, language level: 21"),
            "{structure}"
        );
        assert!(structure.contains("app at ."), "{structure}");
        assert!(
            structure.contains("dependencies: group:lib:1"),
            "{structure}"
        );

        let search = gateway
            .call(&scope(), "search_text", &json!({"pattern": "greet"}))
            .expect("search");
        assert!(search.starts_with("1 matches in 1 files"), "{search}");
        assert!(search.contains(":4:2: greet();"), "{search}");

        let file = gateway
            .call(
                &scope(),
                "read_file",
                &json!({"path": "Foo.txt", "start_line": 2, "end_line": 3}),
            )
            .expect("file");
        assert!(file.contains("lines 2-3 of 3, unsaved changes"), "{file}");
        assert!(file.ends_with("2|2-3\n"), "{file}");

        let changes = gateway
            .call(&scope(), "get_vcs_changes", &json!({"path": "Foo.txt"}))
            .expect("changes");
        assert!(
            changes.contains("git: branch main, 1 changed file"),
            "{changes}"
        );
        assert!(changes.contains("modified: Foo.txt"), "{changes}");
        assert!(changes.contains("@@ -2,1 +2,2 @@"), "{changes}");
        assert!(changes.contains("-old"), "{changes}");
        assert!(changes.contains("+new"), "{changes}");
    }

    #[test]
    fn the_answer_limits_are_clipped_to_what_fits() {
        let gateway = gateway();

        let file = gateway
            .call(
                &scope(),
                "read_file",
                &json!({"path": "Foo.txt", "start_line": 1, "end_line": 100_000}),
            )
            .expect("file");
        assert!(file.contains("lines 1-2000 of 2000"), "{file}");
        assert!(file.contains(", clipped"), "{file}");

        let calls = gateway
            .call(
                &scope(),
                "get_call_hierarchy",
                &json!({"path": "Foo.txt", "line": 3, "column": 1, "depth": 100}),
            )
            .expect("calls");
        assert!(calls.contains("5 levels"), "{calls}");
    }

    #[test]
    fn the_new_tools_refuse_an_empty_or_conflicting_input() {
        let gateway = gateway();

        let both = gateway.call(
            &scope(),
            "find_usages",
            &json!({"path": "Foo.txt", "line": 1, "column": 1, "name": "Foo"}),
        );
        let none = gateway.call(&scope(), "find_usages", &json!({}));
        let zero = gateway.call(
            &scope(),
            "find_usages",
            &json!({"path": "Foo.txt", "line": 0, "column": 1}),
        );
        let blank = gateway.call(&scope(), "search_text", &json!({"pattern": ""}));
        let backwards = gateway.call(
            &scope(),
            "read_file",
            &json!({"path": "Foo.txt", "start_line": 9, "end_line": 2}),
        );
        let no_line = gateway.call(
            &scope(),
            "read_file",
            &json!({"path": "Foo.txt", "start_line": 0}),
        );

        assert!(matches!(both, Err(ToolError::BadInput(_))));
        assert!(matches!(none, Err(ToolError::BadInput(_))));
        assert!(matches!(zero, Err(ToolError::BadInput(_))));
        assert!(matches!(blank, Err(ToolError::BadInput(_))));
        assert!(matches!(backwards, Err(ToolError::BadInput(_))));
        assert!(matches!(no_line, Err(ToolError::BadInput(_))));
    }

    #[cfg(windows)]
    #[test]
    fn the_new_tools_refuse_paths_outside_the_project() {
        let gateway = gateway();
        let atlas = [
            (
                "find_usages",
                json!({"path": "../project-other/secret.txt", "line": 1, "column": 1}),
            ),
            (
                "get_type_hierarchy",
                json!({"path": "../project-other/secret.txt", "line": 1, "column": 1}),
            ),
            (
                "get_call_hierarchy",
                json!({"path": "C:/Windows/win.ini", "line": 1, "column": 1}),
            ),
            ("read_file", json!({"path": "C:/Windows/win.ini"})),
            (
                "get_vcs_changes",
                json!({"path": "../project-other/secret.txt"}),
            ),
        ];

        for (tool, input) in atlas {
            let error = gateway.call(&scope(), tool, &input).unwrap_err();
            assert!(matches!(error, ToolError::Denied(_)), "{tool}: {error:?}");
        }
    }

    fn allowing_gateway() -> ToolGateway {
        let gateway =
            ToolGateway::with_approval(Arc::new(Fake), AuditLog::default(), Arc::new(Allow));
        // The payload tests call every tool by name, so the router-only default of the
        // debugger rows does not stand between them and the answer.
        let mut exposure = ToolExposure::default();
        for name in TOOL_NAMES {
            exposure.set_state(
                name,
                ToolState {
                    enabled: true,
                    router_only: false,
                },
            );
        }
        gateway.set_exposure(exposure);
        gateway
    }

    #[test]
    fn a_proposed_edit_says_how_much_of_it_the_user_kept() {
        let gateway = allowing_gateway();
        let input = json!({
            "path": "Foo.txt",
            "replacements": [
                {"old_text": "one", "new_text": "ONE"},
                {"old_text": "two", "new_text": "TWO"},
            ],
        });

        let answer = gateway
            .call(&scope(), "propose_edit", &input)
            .expect("propose_edit answers");

        assert!(answer.starts_with("project"), "{answer}");
        assert!(answer.contains(": partial, 1 of 2 hunks kept"), "{answer}");
        assert!(answer.contains("text:\nedited"), "{answer}");
    }

    #[test]
    fn a_proposed_edit_takes_one_shape_of_input() {
        let gateway = allowing_gateway();
        let both = json!({
            "path": "Foo.txt",
            "new_text": "x",
            "replacements": [{"old_text": "one", "new_text": "ONE"}],
        });
        let neither = json!({"path": "Foo.txt"});

        let both = gateway.call(&scope(), "propose_edit", &both);
        let neither = gateway.call(&scope(), "propose_edit", &neither);

        assert!(matches!(both, Err(ToolError::BadInput(_))), "{both:?}");
        assert!(
            matches!(neither, Err(ToolError::BadInput(_))),
            "{neither:?}"
        );
    }

    #[test]
    fn an_empty_name_or_a_zero_place_is_a_bad_rename() {
        let gateway = allowing_gateway();
        let blank = json!({"path": "Foo.txt", "line": 1, "column": 1, "new_name": " "});
        let zero = json!({"path": "Foo.txt", "line": 0, "column": 1, "new_name": "Bar"});

        let blank = gateway.call(&scope(), "rename_symbol", &blank);
        let zero = gateway.call(&scope(), "rename_symbol", &zero);

        assert!(matches!(blank, Err(ToolError::BadInput(_))), "{blank:?}");
        assert!(matches!(zero, Err(ToolError::BadInput(_))), "{zero:?}");
    }

    struct Answering {
        answers: Mutex<std::collections::VecDeque<ApprovalAnswer>>,
        asked: std::sync::atomic::AtomicUsize,
    }

    impl Answering {
        fn new(answers: impl IntoIterator<Item = ApprovalAnswer>) -> Arc<Self> {
            Arc::new(Self {
                answers: Mutex::new(answers.into_iter().collect()),
                asked: std::sync::atomic::AtomicUsize::new(0),
            })
        }

        fn asked(&self) -> usize {
            self.asked.load(std::sync::atomic::Ordering::Relaxed)
        }
    }

    impl Approval for Answering {
        fn ask(&self, _request: &ApprovalRequest) -> ApprovalAnswer {
            self.asked
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.answers
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .pop_front()
                .unwrap_or(ApprovalAnswer::NoAnswer)
        }
    }

    fn asked_gateway(approval: Arc<Answering>) -> ToolGateway {
        ToolGateway::with_approval(Arc::new(Fake), AuditLog::default(), approval)
    }

    fn scope_named(session: &str) -> SessionScope {
        SessionScope {
            session: session.to_string(),
            ..scope()
        }
    }

    /// A call that got past the question fails in the backend, which does not run builds;
    /// a refused one never reaches it.
    fn was_allowed(result: Result<String, ToolError>) -> bool {
        !matches!(result, Err(ToolError::Denied(_)))
    }

    #[test]
    fn every_tool_says_whether_it_runs_code() {
        for spec in TOOLS.iter() {
            assert!(
                TOOL_NAMES.contains(&spec.name),
                "{} is missing from the list",
                spec.name
            );
            assert!(
                spec.input_schema.is_object(),
                "{} has no object schema",
                spec.name
            );
            assert_eq!(
                spec.input_schema.get("type").and_then(Value::as_str),
                Some("object"),
                "{} does not ask for an object",
                spec.name
            );
            let executes = matches!(
                spec.name,
                "build_project"
                    | "run_tests"
                    | "run_configuration"
                    | "stop_run"
                    | "propose_edit"
                    | "rename_symbol"
                    | "organize_imports"
                    | "format_file"
                    | "debug_run"
                    | "debug_evaluate"
                    | "debug_step"
                    | "debug_stop"
                    | "execute_run_configuration"
                    | "create_new_file"
                    | "reformat_file"
                    | "apply_patch"
                    | "rename_refactoring"
                    | "execute_terminal_command"
                    | "xdebug_start_debugger_session"
                    | "xdebug_control_session"
                    | "xdebug_run_to_line"
                    | "xdebug_evaluate_expression"
                    | "xdebug_set_variable"
                    | "test_database_connection"
                    | "create_database_connection"
                    | "edit_database_connection"
                    | "execute_sql_query"
            );
            let class = if executes {
                ToolClass::Execute
            } else {
                ToolClass::Read
            };
            assert_eq!(spec.class, class, "{} has the wrong class", spec.name);
        }
    }

    #[test]
    fn running_a_build_is_refused_without_a_question() {
        let gateway = gateway();

        let result = gateway.call(&scope(), "build_project", &json!({}));

        assert!(
            matches!(&result, Err(ToolError::Denied(message)) if message.contains("permission") || message.contains("cannot ask")),
            "{result:?}"
        );
    }

    #[test]
    fn a_question_nobody_answers_refuses_the_call() {
        let approval = Answering::new([ApprovalAnswer::NoAnswer]);
        let gateway = asked_gateway(approval.clone());

        let result = gateway.call(&scope(), "build_project", &json!({}));

        assert_eq!(
            result,
            Err(ToolError::Denied("no answer".to_string())),
            "an unanswered question is a refusal of its own"
        );
        assert_eq!(approval.asked(), 1);
        assert!(!gateway.audit().recent()[0].ok);
    }

    #[test]
    fn a_denied_call_never_reaches_the_backend() {
        let approval = Answering::new([ApprovalAnswer::Deny]);
        let gateway = asked_gateway(approval);

        let result = gateway.call(&scope(), "run_tests", &json!({}));

        assert!(matches!(result, Err(ToolError::Denied(_))), "{result:?}");
    }

    #[test]
    fn allow_once_asks_again_and_allow_for_the_session_does_not() {
        let approval = Answering::new([ApprovalAnswer::Once, ApprovalAnswer::Session]);
        let gateway = asked_gateway(approval.clone());

        assert!(was_allowed(gateway.call(
            &scope(),
            "build_project",
            &json!({})
        )));
        assert_eq!(approval.asked(), 1, "the first call asked once");
        assert!(was_allowed(gateway.call(
            &scope(),
            "build_project",
            &json!({})
        )));
        assert_eq!(approval.asked(), 2, "allow once did not last");
        assert!(was_allowed(gateway.call(
            &scope(),
            "build_project",
            &json!({})
        )));
        assert_eq!(
            approval.asked(),
            2,
            "allow for the session did not keep the next call quiet"
        );
    }

    #[test]
    fn allowing_a_session_does_not_allow_another_one() {
        let approval = Answering::new([ApprovalAnswer::Session, ApprovalAnswer::Deny]);
        let gateway = asked_gateway(approval.clone());

        assert!(was_allowed(gateway.call(
            &scope_named("one"),
            "build_project",
            &json!({})
        )));
        assert_eq!(approval.asked(), 1);
        assert!(was_allowed(gateway.call(
            &scope_named("one"),
            "run_tests",
            &json!({})
        )));
        assert_eq!(approval.asked(), 1, "the same session asks no more");
        let other = gateway.call(&scope_named("two"), "build_project", &json!({}));
        assert!(matches!(other, Err(ToolError::Denied(_))), "{other:?}");
        assert_eq!(approval.asked(), 2, "another session asks on its own");
    }

    #[test]
    fn revoking_a_session_permission_makes_the_next_call_ask_again() {
        let approval = Answering::new([ApprovalAnswer::Session, ApprovalAnswer::Deny]);
        let gateway = asked_gateway(approval.clone());

        assert!(was_allowed(gateway.call(
            &scope(),
            "build_project",
            &json!({})
        )));
        assert_eq!(approval.asked(), 1);
        assert!(gateway.session_allowed("s1"));
        assert_eq!(gateway.allowed_sessions(), ["s1".to_string()]);

        assert!(gateway.revoke_session("s1"), "the permission was there");
        assert!(!gateway.session_allowed("s1"));
        let denied = gateway.call(&scope(), "build_project", &json!({}));
        assert!(matches!(denied, Err(ToolError::Denied(_))), "{denied:?}");
        assert_eq!(approval.asked(), 2, "the next call asked again");
        assert!(!gateway.revoke_session("s1"), "nothing is left to revoke");
    }

    #[test]
    fn a_case_without_its_suite_is_bad_input() {
        let result = asked_gateway(Answering::new([ApprovalAnswer::Once])).call(
            &scope(),
            "run_tests",
            &json!({"case": "passes"}),
        );

        assert!(matches!(result, Err(ToolError::BadInput(_))), "{result:?}");
    }

    #[test]
    fn the_debug_tools_answer_with_their_parts() {
        let gateway = allowing_gateway();

        let set = gateway
            .call(
                &scope(),
                "set_breakpoint",
                &json!({"path": "Foo.txt", "line": 3, "condition": " x > 1 "}),
            )
            .expect("set breakpoint");
        assert!(set.starts_with("breakpoint set: "), "{set}");
        assert!(set.contains(":3 on, verified"), "{set}");
        assert!(set.contains("condition: x > 1"), "{set}");

        let listed = gateway
            .call(&scope(), "list_breakpoints", &json!({}))
            .expect("list breakpoints");
        assert!(listed.starts_with("1 breakpoints:"), "{listed}");
        assert!(listed.contains("project"), "{listed}");

        let removed = gateway
            .call(
                &scope(),
                "remove_breakpoint",
                &json!({"path": "Foo.txt", "line": 3}),
            )
            .expect("remove breakpoint");
        assert!(removed.contains("breakpoint removed: "), "{removed}");
        assert!(removed.contains(":3"), "{removed}");

        let run = gateway
            .call(
                &scope(),
                "debug_run",
                &json!({"suite": "demo.Vitest", "case": "fails", "wait_seconds": 12}),
            )
            .expect("debug run");
        assert!(
            run.starts_with("debugging demo.Vitest.fails: session debug-1\n"),
            "{run}"
        );
        assert!(run.contains("debugging: stopped (breakpoint)"), "{run}");
        assert!(run.contains("thread: main #1"), "{run}");
        assert!(run.contains("#0 run at project"), "{run}");

        let wait = gateway
            .call(&scope(), "debug_wait", &json!({"seconds": 5}))
            .expect("debug wait");
        assert!(wait.contains("debugging: stopped (5 s)"), "{wait}");

        let stack = gateway
            .call(&scope(), "debug_stack", &json!({"frames": 2}))
            .expect("debug stack");
        assert!(stack.starts_with("stack of main #1: 4 frames\n"), "{stack}");
        assert!(stack.contains("#0 frame 0\n#1 frame 1\n"), "{stack}");
        assert!(stack.contains("clipped: showing 2 of 4 frames"), "{stack}");

        let variables = gateway
            .call(
                &scope(),
                "debug_variables",
                &json!({"frame": 0, "depth": 1, "length": 40}),
            )
            .expect("debug variables");
        assert!(
            variables.starts_with("variables of frame #0\n"),
            "{variables}"
        );
        assert!(variables.contains("  Locals:\n"), "{variables}");
        assert!(
            variables.contains("  items = ArrayList (2) (ArrayList)"),
            "{variables}"
        );
        assert!(variables.contains("    child = cccc"), "{variables}");

        let value = gateway
            .call(
                &scope(),
                "debug_evaluate",
                &json!({"expression": "items.size()"}),
            )
            .expect("debug evaluate");
        assert_eq!(value, "items.size() = 42 (int)\n");

        let step = gateway
            .call(&scope(), "debug_step", &json!({"action": "over"}))
            .expect("debug step");
        assert!(step.contains("debugging: stopped (over)"), "{step}");

        let stopped = gateway
            .call(&scope(), "debug_stop", &json!({}))
            .expect("debug stop");
        assert_eq!(stopped, "debug session stopped\n");
    }

    #[test]
    fn the_debug_answers_stay_inside_their_limits() {
        let gateway = allowing_gateway();

        let stack = gateway
            .call(&scope(), "debug_stack", &json!({"frames": 10_000}))
            .expect("debug stack");
        assert!(stack.contains(": 102 frames"), "{stack}");
        assert!(
            stack.contains("clipped: showing 100 of 102 frames"),
            "{stack}"
        );

        let variables = gateway
            .call(
                &scope(),
                "debug_variables",
                &json!({"depth": 1, "length": 20, "variables": 5}),
            )
            .expect("debug variables");
        assert!(variables.contains("ccccccccccccccccccc…"), "{variables}");

        let one = gateway
            .call(
                &scope(),
                "debug_variables",
                &json!({"depth": 1, "variables": 1}),
            )
            .expect("debug variables");
        assert!(!one.contains("child ="), "{one}");
        assert!(one.contains("clipped: 1 variables"), "{one}");

        let short = gateway
            .call(&scope(), "debug_wait", &json!({"seconds": 0}))
            .expect("debug wait");
        assert!(short.contains("stopped (1 s)"), "{short}");
        let long = gateway
            .call(&scope(), "debug_wait", &json!({"seconds": 10_000}))
            .expect("debug wait");
        assert!(long.contains("stopped (120 s)"), "{long}");
    }

    #[test]
    fn the_debug_tools_refuse_an_empty_or_conflicting_input() {
        let gateway = allowing_gateway();

        let zero = gateway.call(
            &scope(),
            "set_breakpoint",
            &json!({"path": "Foo.txt", "line": 0}),
        );
        let nothing = gateway.call(&scope(), "debug_run", &json!({}));
        let both = gateway.call(
            &scope(),
            "debug_run",
            &json!({"configuration": "Demo", "suite": "demo.Vitest"}),
        );
        let module = gateway.call(
            &scope(),
            "debug_run",
            &json!({"configuration": "Demo", "module": "app"}),
        );
        let case = gateway.call(&scope(), "debug_run", &json!({"case": "fails"}));
        let blank = gateway.call(&scope(), "debug_evaluate", &json!({"expression": " "}));
        let step = gateway.call(&scope(), "debug_step", &json!({"action": "sideways"}));
        let no_step = gateway.call(&scope(), "debug_step", &json!({}));

        assert!(matches!(zero, Err(ToolError::BadInput(_))), "{zero:?}");
        assert!(
            matches!(nothing, Err(ToolError::BadInput(_))),
            "{nothing:?}"
        );
        assert!(matches!(both, Err(ToolError::BadInput(_))), "{both:?}");
        assert!(matches!(module, Err(ToolError::BadInput(_))), "{module:?}");
        assert!(matches!(case, Err(ToolError::BadInput(_))), "{case:?}");
        assert!(matches!(blank, Err(ToolError::BadInput(_))), "{blank:?}");
        assert!(matches!(step, Err(ToolError::BadInput(_))), "{step:?}");
        assert!(
            matches!(no_step, Err(ToolError::BadInput(_))),
            "{no_step:?}"
        );
    }

    #[cfg(windows)]
    #[test]
    fn the_debug_tools_refuse_paths_outside_the_project() {
        let gateway = allowing_gateway();
        let set = gateway.call(
            &scope(),
            "set_breakpoint",
            &json!({"path": "C:/Windows/win.ini", "line": 1}),
        );
        let remove = gateway.call(
            &scope(),
            "remove_breakpoint",
            &json!({"path": "../project-other/secret.txt", "line": 1}),
        );

        assert!(matches!(set, Err(ToolError::Denied(_))), "{set:?}");
        assert!(matches!(remove, Err(ToolError::Denied(_))), "{remove:?}");
    }

    #[test]
    fn a_command_of_execute_tool_takes_json_or_key_values() {
        let (name, arguments) =
            parse_command("get_file_problems {\"filePath\": \"src/Main.txt\"}").expect("json");
        assert_eq!(name, "get_file_problems");
        assert_eq!(arguments["filePath"], json!("src/Main.txt"));

        let (name, arguments) = parse_command("find_symbol query=Widget limit=5").expect("pairs");
        assert_eq!(name, "find_symbol");
        assert_eq!(arguments["query"], json!("Widget"));
        assert_eq!(arguments["limit"], json!(5));
    }

    #[test]
    fn a_word_is_found_with_its_place_from_one() {
        assert_eq!(
            locate_word("class Main {\n    Main run;\n}\n", "Main"),
            Some((1, 7))
        );
        assert_eq!(locate_word("Mainer\n", "Main"), None);
    }

    #[test]
    fn an_indentation_block_keeps_the_deeper_lines() {
        let lines = [
            "class A {",
            "    void run() {",
            "        call();",
            "    }",
            "}",
        ];
        assert_eq!(indentation_block(&lines, 2, 1), (2, 4));
    }

    fn names(tools: &[crate::tools::ToolSpec]) -> Vec<&'static str> {
        tools.iter().map(|tool| tool.name).collect()
    }

    #[test]
    fn a_tool_that_is_off_leaves_the_list_and_refuses_a_call() {
        let gateway = gateway();
        let mut exposure = ToolExposure::default();
        exposure.set_state(
            "find_symbol",
            ToolState {
                enabled: false,
                router_only: false,
            },
        );
        gateway.set_exposure(exposure);

        let visible = names(&gateway.visible_tools(&scope()));
        assert!(!visible.contains(&"find_symbol"), "{visible:?}");
        assert!(visible.contains(&"search_text"), "{visible:?}");

        let direct = gateway.call(&scope(), "find_symbol", &json!({"query": "Foo"}));
        assert!(
            matches!(direct, Err(ToolError::Denied(_))),
            "a call of a tool that is off does not reach the service: {direct:?}"
        );
        let through = gateway.call(
            &scope(),
            "execute_tool",
            &json!({"command": "find_symbol {\"query\": \"Foo\"}"}),
        );
        assert!(
            matches!(through, Err(ToolError::Denied(_))),
            "execute_tool does not open a tool that is off: {through:?}"
        );
    }

    #[test]
    fn a_router_only_tool_leaves_the_list_but_answers_through_execute_tool() {
        let gateway = gateway();
        let mut exposure = ToolExposure::default();
        exposure.set_state(
            "find_symbol",
            ToolState {
                enabled: true,
                router_only: true,
            },
        );
        gateway.set_exposure(exposure);

        let visible = names(&gateway.visible_tools(&scope()));
        assert!(!visible.contains(&"find_symbol"), "{visible:?}");
        assert!(visible.contains(&"execute_tool"), "{visible:?}");

        let direct = gateway.call(&scope(), "find_symbol", &json!({"query": "Foo"}));
        assert!(matches!(direct, Err(ToolError::Denied(_))), "{direct:?}");

        let through = gateway
            .call(
                &scope(),
                "execute_tool",
                &json!({"command": "find_symbol {\"query\": \"Foo\"}"}),
            )
            .expect("execute_tool reaches the router-only tool");
        assert!(through.contains(":1:class Foo"), "{through}");
    }

    #[test]
    fn router_only_mode_leaves_execute_tool_alone() {
        let gateway = gateway();
        let mut exposure = ToolExposure::default();
        exposure.set_router_only(Some(RouterScope::AllAgents));
        gateway.set_exposure(exposure);

        assert_eq!(
            names(&gateway.visible_tools(&scope())),
            ["execute_tool"],
            "a client of router-only mode sees the router alone"
        );
        let direct = gateway.call(&scope(), "find_symbol", &json!({"query": "Foo"}));
        assert!(matches!(direct, Err(ToolError::Denied(_))), "{direct:?}");
        let through = gateway
            .call(
                &scope(),
                "execute_tool",
                &json!({"command": "find_symbol {\"query\": \"Foo\"}"}),
            )
            .expect("the router still reaches the tools");
        assert!(through.contains(":1:class Foo"), "{through}");
    }

    #[test]
    fn the_acp_scope_leaves_the_other_clients_of_router_only_mode_alone() {
        let gateway = gateway();
        let mut exposure = ToolExposure::default();
        exposure.set_router_only(Some(RouterScope::AcpOnly));
        gateway.set_exposure(exposure);

        let acp = scope_named("acp-codex-1");
        assert_eq!(names(&gateway.visible_tools(&acp)), ["execute_tool"]);
        let client = scope_named("mcp");
        let visible = names(&gateway.visible_tools(&client));
        assert!(visible.contains(&"find_symbol"), "{visible:?}");
    }

    #[test]
    fn the_debugger_tools_start_router_only() {
        for name in ["debug_stack", "set_breakpoint", "debug_evaluate"] {
            assert!(crate::tools::TOOL_NAMES.contains(&name), "{name}");
            assert!(crate::exposure::is_debug_tool(name), "{name}");
            let state = crate::exposure::default_state(name);
            assert!(state.enabled && state.router_only, "{name}");
        }
        let state = crate::exposure::default_state("find_symbol");
        assert!(state.enabled && !state.router_only);
    }
}
