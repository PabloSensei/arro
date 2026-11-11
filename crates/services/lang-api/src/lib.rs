//! The boundary between the core and a language. See docs/ARCHITECTURE.md section 8.
//!
//! A language is registered statically in `crates/app/ide`. These traits are internal and
//! may change: a new extension point appears here together with its first real caller.

mod answer;
mod hierarchy;
mod inlay;
mod language;
mod semantic;
mod stream;
mod symbol;
mod syntax;

pub use answer::{Answer, Confidence, SourceId};
pub use hierarchy::{
    AddOverridableMethodsProvider, AddOverridableMethodsRequest, CallDirection, CallHierarchyItem,
    CallHierarchyProvider, CallHierarchyQuery, DefinitionPreview, DefinitionPreviewProvider,
    DefinitionPreviewQuery, FileSymbol, FileSymbolsProvider, FileSymbolsQuery,
    MethodHierarchyEntry, MethodHierarchyProvider, MethodHierarchyQuery, MethodRelation,
    OverridableMethod, OverridableMethods, OverridableMethodsProvider, OverridableMethodsQuery,
    ReferenceQuery, ReferenceStringProvider,
};
pub use inlay::{
    InlayHintEntry, InlayHintKind, InlayHintProvider, InlayHintQuery, InlayHintSettings,
    InlayParameterNames,
};
pub use language::{LanguageId, LanguageSupport};
pub use semantic::{
    ActionEntry, ActionKind, ActionProvider, ActionQuery, ChangeSignatureInfo,
    ChangeSignatureInfoProvider, ChangeSignatureOutcome, ChangeSignatureProvider,
    ChangeSignatureRequest, CommandQuery, CompletionCommand, CompletionEntry,
    CompletionLabelDetails, CompletionList, CompletionProvider, CompletionResolveProvider,
    DiagnosticEntry, DiagnosticsProvider, DiagnosticsQuery, DocumentEdit, DocumentSpot,
    ExecuteCommandProvider, FileChange, FileRename, FormatProvider, FormatQuery,
    GenerateApplyProvider, GenerateApplyRequest, GenerateChoice, GenerateKind, GenerateProvider,
    GenerateQuery, GeneratedFile, Generation, HierarchyDirection, HierarchyProvider,
    HierarchyQuery, HighlightProvider, HoverProvider, HoverText, MethodSignature, MoveDestination,
    MoveDestinationProvider, MoveKind, MoveOutcome, MoveProvider, MoveQuery, MoveRequest,
    NavigationKind, NavigationProvider, NavigationQuery, ProjectProblem, ProjectProblemsProvider,
    ProjectProblemsQuery, ReferencesProvider, RenameProvider, RenameQuery, SemanticServer,
    Severity, SignatureException, SignatureParameter, SignatureProvider, SignatureQuery,
    SignatureText, SourceEdit, SourceSpan, SourceTextProvider, SourceTextQuery, SymbolEntry,
    SymbolQuery, TextPoint, TextSpan, WorkspaceChange, WorkspaceSymbolProvider, apply_to_text,
    offset_in,
};
pub use semantic::{
    CallParameter, CallSignature, CompletionKind, ExpectedType, ExpectedTypeProvider, ExternalDoc,
    ExternalDocProvider, SemanticToken, SemanticTokenKind, SemanticTokenModifiers,
    SemanticTokensProvider, SemanticTokensQuery, SignatureHelp, SignatureInfoProvider,
};
pub use semantic::{
    DiagnosticNote, FileDiagnostics, PrepareDiagnosticsProvider, PrepareDiagnosticsQuery,
    ProblemKind, RangeFormatProvider, RangeFormatQuery, WorkspaceDiagnosticsProvider,
    WorkspaceDiagnosticsQuery,
};
pub use semantic::{UsageEntry, UsageKind, UsageProvider};
pub use stream::{Provider, Replies, Reply, ReplyClosed, ask, channel};
pub use symbol::{Symbol, SymbolKind, SymbolProvider};
pub use syntax::{Deletion, DeletionKind, Queries, SyntaxProvider, UnwrapEdit};
pub use tree_sitter_language::LanguageFn;
