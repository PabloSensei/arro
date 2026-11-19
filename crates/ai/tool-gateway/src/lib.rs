//! The one door from agents into the IDE. Whatever the transport, a call passes the schema, the
//! policy and the audit log here, and only then reaches a service.

mod audit;
mod backend;
mod clients;
mod database_tools;
mod edit;
mod exposure;
mod filters;
mod gateway;
mod hint;
mod language;
mod patch;
mod render;
mod tools;
mod tree;
mod xdebug;

pub use audit::{AuditEntry, AuditLog};
pub(crate) use backend::relative_path;
pub use backend::{
    AnalyzeCallsQuery, BreakpointId, BreakpointInfo, BreakpointQuery, BuildOutcome, BuildProblem,
    BuildRequest, CallDirection, CallHierarchy, CallHierarchyQuery, CallNode, CallTree, Confidence,
    DatabaseControl, DebugControl, DebugEvaluateQuery, DebugFrame, DebugRunOutcome,
    DebugRunRequest, DebugScope, DebugSessionState, DebugSnapshotSource, DebugStack,
    DebugStackQuery, DebugStateQuery, DebugStep, DebugStepQuery, DebugThread, DebugValue,
    DebugVariable, DebugVariables, DebugVariablesQuery, DebugWaitQuery, Diagnostic,
    DiagnosticsPage, DiagnosticsQuery, DirectoryTree, EditOutcome, EditorContext, ExecuteRunQuery,
    FileActionOutcome, FileActionQuery, FileContent, FileText, GitFileEntry, GitRepositoryStatus,
    GitStatus, GitStatusQuery, HierarchyNode, Location, OUTPUT_TAIL, PathPage, PathQuery, Position,
    ProjectModule, ProjectRunner, ProjectStructure, ProjectStructureQuery, ProposeEditQuery,
    ProposedEdit, READ_LINE_LIMIT, ReadFileQuery, RenameSymbolQuery, RenamedSymbol,
    RunConfiguration, RunId, RunOutput, RunPointInfo, RunPointQuery, SearchHit, SearchPage,
    SearchQuery, SessionScope, Severity, StartRunRequest, SymbolHit, SymbolInfo, SymbolPage,
    SymbolQuery, TerminalOutput, TerminalQuery, TestCase, TestOutcome, TestsOutcome, TestsRequest,
    TextReplacement, ToolBackend, ToolError, TreeEntry, TypeHierarchy, TypeHierarchyQuery,
    Unavailable, Usage, UsagePage, UsagesQuery, VcsChanges, VcsDiff, VcsFile, VcsHunk, VcsQuery,
};
pub use clients::{
    CALL_LIMIT, CLIENT_LIMIT, CallOutcome, CallRecord, ClientInfo, ClientLog, ClientRecord, HIDDEN,
    arguments as client_arguments, hide_secrets, without_password,
};
pub use database_tools::{
    DatabaseAnswer, DatabaseConnection, DatabaseDescription, DatabaseIntrospection, DatabaseObject,
    DatabaseObjectKind, DatabaseQuery, DatabaseRecentQuery, DatabaseRows, DatabaseSchema,
    DatabaseTestResult, DescriptionColumn, DescriptionIndex, DescriptionKey, PAGE_ROWS,
    QueryResults, STORED_ROWS,
};
pub use edit::apply_replacements;
pub use exposure::{RouterScope, ToolExposure, ToolState, default_state, is_debug_tool};
pub use filters::{matches_glob, path_allowed};
pub use gateway::{Approval, ApprovalAnswer, ApprovalRequest, Decision, Policy, ToolGateway};
pub use hint::{agents_md_section, with_agents_md_section};
pub use language::LanguageTools;
pub use render::build_problems;
pub use tools::{TOOL_GROUPS, TOOL_NAMES, ToolClass, ToolSpec, all_tools, group_of};
pub use xdebug::{
    BreakpointOwner, DebugAnswer, DebugBreakpoint, DebugBreakpointQuery, DebugControlAction,
    DebugControlOutcome, DebugControlQuery, DebugExpressionQuery, DebugFrameInfo, DebugFramePage,
    DebugFrameValues, DebugFramesQuery, DebugPathQuery, DebugQuery, DebugRemoveBreakpointsQuery,
    DebugRunToLineQuery, DebugSetVariableOutcome, DebugSetVariableQuery, DebugStartQuery,
    DebugThreadInfo, DebugThreadPage, DebugThreadSelector, DebugThreadsQuery, DebugValueNode,
    DebugValuesQuery, DebuggerSession, DebuggerStatus, breakpoint_id,
};
