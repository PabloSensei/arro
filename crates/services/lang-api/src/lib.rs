//! The boundary between the core and a language. See docs/ARCHITECTURE.md section 8.
//!
//! A language is registered statically in `crates/app/ide`. These traits are internal and
//! may change: a new extension point appears here together with its first real caller.

mod answer;
mod bundle;
mod code_lens;
mod coverage;
mod di;
mod diagram;
mod documentation;
mod embedded;
mod endpoint;
mod hierarchy;
mod inlay;
mod inspection;
mod language;
mod profile;
mod semantic;
mod sources;
mod stack;
mod stream;
mod structural;
mod symbol;
mod syntax;
mod test_gen;

pub use answer::{Answer, Confidence, SourceId};
pub use bundle::BundleFile;
pub use code_lens::{
    CodeLensCommand, CodeLensEntry, CodeLensKind, CodeLensProvider, CodeLensQuery,
    CodeLensResolveProvider, CodeLensResolveQuery, CodeVisionSettings, kind_of,
};
pub use coverage::{
    BranchCoverage, Counters, CoverageReport, FileCoverage, LineCoverage, LineState, TypeCoverage,
};
pub use di::{
    Bean, BeanModel, BeanProvider, BeanQuery, BeanWarning, InjectionPlace, InjectionPoint,
};
pub use diagram::{
    GraphEdge, GraphEdgeKind, GraphMember, GraphMemberKind, GraphType, GraphTypeKind,
    GraphVisibility, TypeGraph, TypeGraphProvider, TypeGraphQuery,
};
pub use documentation::{
    DocumentationBlock, DocumentationComment, DocumentationCommentProvider, DocumentationInline,
};
pub use embedded::EmbeddedTextProvider;
pub use endpoint::{
    Endpoint, EndpointParameter, EndpointParameterKind, EndpointProvider, EndpointScan, HttpMethod,
};
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
pub use inspection::{
    DisabledInspections, InspectionInfo, InspectionLevel, InspectionSetting, InspectionSettings,
    Suppressions,
};
pub use language::{LanguageId, LanguageSupport};
pub use profile::{
    ProfileFrame, ProfileMethod, ProfileNode, ProfileRecording, ProfileReport, ProfileSample,
    ProfileSamples, ProfileSnapshot, ProfileType, ProfileTypeTree, ProfileUnit, bytes_text,
};
pub use semantic::{
    ActionApplyProvider, ActionApplyRequest, ActionChoice, ActionEntry, ActionField, ActionKind,
    ActionMember, ActionOutcome, ActionParameters, ActionParametersProvider, ActionParametersQuery,
    ActionProvider, ActionQuery, ChangeSignatureInfo, ChangeSignatureInfoProvider,
    ChangeSignatureOutcome, ChangeSignatureProvider, ChangeSignatureRequest, CommandQuery,
    CompletionCommand, CompletionEntry, CompletionLabelDetails, CompletionList, CompletionProvider,
    CompletionResolveProvider, DependencyUpdatesProvider, DependencyVersion,
    DependencyVersionQuery, DiagnosticEntry, DiagnosticsProvider, DiagnosticsQuery, DocumentEdit,
    DocumentSpot, ExecuteCommandProvider, FileChange, FileRename, FormatProvider, FormatQuery,
    GenerateApplyProvider, GenerateApplyRequest, GenerateChoice, GenerateKind, GenerateProvider,
    GenerateQuery, GeneratedFile, Generation, HierarchyDirection, HierarchyProvider,
    HierarchyQuery, HighlightProvider, HoverProvider, HoverText, MethodSignature, MoveDestination,
    MoveDestinationProvider, MoveKind, MoveOutcome, MoveProvider, MoveQuery, MoveRequest,
    NavigationKind, NavigationProvider, NavigationQuery, ProjectProblem, ProjectProblemsProvider,
    ProjectProblemsQuery, ReferencesProvider, RenameProvider, RenameQuery, SemanticServer,
    Severity, SignatureException, SignatureParameter, SignatureProvider, SignatureQuery,
    SignatureText, SourceEdit, SourceSpan, SourceTextProvider, SourceTextQuery, SuppressionQuery,
    SuppressionStyle, SuppressionTarget, SymbolEntry, SymbolQuery, TextPoint, TextSpan,
    WorkspaceChange, WorkspaceSymbolProvider, apply_to_text, offset_in,
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
pub use sources::{
    LibraryClass, LibraryClassProvider, LibraryClassQuery, SourcesEvent, SourcesKind,
    SourcesProvider, SourcesRequest,
};
pub use stack::{
    DumpAnswer, DumpProvider, DumpQuery, Frame, FrameOrigin, Line as StackLine, LineKind, Report,
    Thread as ThreadStack, TraceProvider, TraceQuery,
};
pub use stream::{Provider, Replies, Reply, ReplyClosed, ask, channel};
pub use structural::{
    StructuralEdit, StructuralMatch, StructuralQuery, StructuralSearchProvider, StructuralTemplate,
    StructuralVariable,
};
pub use symbol::{Symbol, SymbolKind, SymbolProvider};
pub use syntax::{
    Deletion, DeletionKind, IdentifierLines, Queries, SyntaxProvider, UnwrapEdit,
    identifiers_around,
};
pub use test_gen::{
    TestChoice, TestCreateOutcome, TestCreateProvider, TestCreateRequest, TestFile, TestFixOutcome,
    TestFixProvider, TestFixRequest, TestFolder, TestForm, TestFormProvider, TestFormQuery,
    TestFramework,
};
pub use tree_sitter_language::LanguageFn;
