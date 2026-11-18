//! The one door from agents into the IDE. Whatever the transport, a call passes the schema, the
//! policy and the audit log here, and only then reaches a service.

mod audit;
mod backend;
mod clients;
mod edit;
mod gateway;
mod hint;
mod language;
mod render;
mod tools;

pub use audit::{AuditEntry, AuditLog};
pub use backend::{
    BreakpointId, BreakpointInfo, BreakpointQuery, BuildOutcome, BuildProblem, BuildRequest,
    CallHierarchy, CallHierarchyQuery, Confidence, DebugControl, DebugEvaluateQuery, DebugFrame,
    DebugRunOutcome, DebugRunRequest, DebugScope, DebugSessionState, DebugSnapshotSource,
    DebugStack, DebugStackQuery, DebugStateQuery, DebugStep, DebugStepQuery, DebugThread,
    DebugValue, DebugVariable, DebugVariables, DebugVariablesQuery, DebugWaitQuery, Diagnostic,
    DiagnosticsPage, DiagnosticsQuery, EditOutcome, EditorContext, FileActionOutcome,
    FileActionQuery, FileText, HierarchyNode, Location, OUTPUT_TAIL, Position, ProjectModule,
    ProjectRunner, ProjectStructure, ProjectStructureQuery, ProposeEditQuery, ProposedEdit,
    READ_LINE_LIMIT, ReadFileQuery, RenameSymbolQuery, RenamedSymbol, RunConfiguration, RunId,
    RunOutput, SearchHit, SearchPage, SearchQuery, SessionScope, Severity, StartRunRequest,
    SymbolHit, SymbolInfo, SymbolPage, SymbolQuery, TestCase, TestOutcome, TestsOutcome,
    TestsRequest, TextReplacement, ToolBackend, ToolError, TypeHierarchy, TypeHierarchyQuery,
    Unavailable, Usage, UsagePage, UsagesQuery, VcsChanges, VcsDiff, VcsFile, VcsHunk, VcsQuery,
};
pub use clients::{
    CALL_LIMIT, CLIENT_LIMIT, CallOutcome, CallRecord, ClientInfo, ClientLog, ClientRecord, HIDDEN,
    arguments as client_arguments,
};
pub use edit::apply_replacements;
pub use gateway::{Approval, ApprovalAnswer, ApprovalRequest, Decision, Policy, ToolGateway};
pub use hint::{agents_md_section, with_agents_md_section};
pub use language::LanguageTools;
pub use render::build_problems;
pub use tools::{TOOL_NAMES, ToolClass, ToolSpec};
