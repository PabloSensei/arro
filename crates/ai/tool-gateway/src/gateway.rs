use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::Value;

use crate::audit::AuditLog;
use crate::backend::{
    BreakpointId, BreakpointQuery, BuildRequest, CallHierarchyQuery, DebugEvaluateQuery,
    DebugRunRequest, DebugStackQuery, DebugStateQuery, DebugStep, DebugStepQuery,
    DebugVariablesQuery, DebugWaitQuery, DiagnosticsQuery, FileActionQuery, Position,
    ProjectStructureQuery, ProposeEditQuery, ReadFileQuery, RenameSymbolQuery, RunId, SearchQuery,
    SessionScope, StartRunRequest, SymbolQuery, TestsRequest, ToolBackend, ToolError,
    TypeHierarchyQuery, UsagesQuery, VcsQuery,
};
use crate::clients::{self, CallOutcome, ClientInfo, ClientLog};
use crate::render;
use crate::tools::{
    BUILD_PROJECT, CALL_HIERARCHY, DEBUG_EVALUATE, DEBUG_RUN, DEBUG_STACK, DEBUG_STATE, DEBUG_STEP,
    DEBUG_STOP, DEBUG_VARIABLES, DEBUG_WAIT, DIAGNOSTICS, EDITOR_CONTEXT, FIND_SYMBOL, FIND_USAGES,
    FORMAT_FILE, GET_RUN_OUTPUT, LIST_BREAKPOINTS, LIST_RUN_CONFIGURATIONS, ORGANIZE_IMPORTS,
    PROJECT_STRUCTURE, PROPOSE_EDIT, READ_FILE, REMOVE_BREAKPOINT, RENAME_SYMBOL,
    RUN_CONFIGURATION, RUN_TESTS, SEARCH_TEXT, SET_BREAKPOINT, STOP_RUN, SYMBOL_INFO, TOOLS,
    TYPE_HIERARCHY, ToolClass, ToolSpec, VCS_CHANGES,
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
        }
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
        let _working = (class == ToolClass::Execute).then(|| Working::new(&self.executing));
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
                let input: PositionInput = parse(input)?;
                if input.line == 0 || input.column == 0 {
                    return Err(ToolError::BadInput(
                        "line and column start at 1".to_string(),
                    ));
                }
                let position = Position {
                    path: resolve(scope, &input.path)?,
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
                Ok(render::search(&query, &page))
            }
            READ_FILE => {
                let input: ReadFileQuery = parse(input)?;
                if input.start_line == Some(0) || input.end_line == Some(0) {
                    return Err(ToolError::BadInput("line ranges start at 1".to_string()));
                }
                if let (Some(start), Some(end)) = (input.start_line, input.end_line)
                    && start > end
                {
                    return Err(ToolError::BadInput(
                        "the first line is after the last one".to_string(),
                    ));
                }
                let mut query = input.clipped();
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
                match (query.new_text.is_some(), query.replacements.is_empty()) {
                    (true, false) => {
                        return Err(ToolError::BadInput(
                            "pass new_text or replacements, not both".to_string(),
                        ));
                    }
                    (false, true) => {
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
                let request: BuildRequest = parse(input)?;
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
            _ => Err(ToolError::UnknownTool(tool.to_string())),
        }
    }
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
        ToolGateway::with_approval(Arc::new(Fake), AuditLog::default(), Arc::new(Allow))
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
    fn the_debug_tools_that_run_the_program_need_a_yes() {
        let gateway = gateway();

        for tool in ["debug_run", "debug_evaluate", "debug_step", "debug_stop"] {
            let input = match tool {
                "debug_run" => json!({"configuration": "Demo"}),
                "debug_evaluate" => json!({"expression": "1 + 1"}),
                "debug_step" => json!({"action": "over"}),
                _ => json!({}),
            };
            let result = gateway.call(&scope(), tool, &input);
            assert!(
                matches!(result, Err(ToolError::Denied(_))),
                "{tool}: {result:?}"
            );
        }
        for (tool, input) in [
            ("set_breakpoint", json!({"path": "Foo.txt", "line": 1})),
            ("list_breakpoints", json!({})),
            ("debug_wait", json!({"seconds": 1})),
            ("debug_stack", json!({})),
            ("debug_variables", json!({})),
        ] {
            let result = gateway.call(&scope(), tool, &input);
            assert!(
                !matches!(result, Err(ToolError::Denied(_))),
                "{tool} asks no question: {result:?}"
            );
        }
    }
}
