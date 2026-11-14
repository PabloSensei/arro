//! Semantic capabilities. The shapes follow LSP, so a language behind a server needs no
//! conversion of meaning. Positions are zero-based and count UTF-16 units.

use std::path::PathBuf;
use std::sync::Arc;

use crate::Provider;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TextPoint {
    pub line: u32,
    pub character: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TextSpan {
    pub start: TextPoint,
    pub end: TextPoint,
}

/// A place in a document as the asker saw it. An answer for an older version is dropped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentSpot {
    pub uri: String,
    pub version: i32,
    pub point: TextPoint,
}

/// A place in any source. The URI may name something that is not a file on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceSpan {
    pub uri: String,
    pub span: TextSpan,
}

/// The less prominent parts a server may give for a label: `detail` follows the name
/// without a space, `description` comes after it, a fully qualified name for a type.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CompletionLabelDetails {
    pub detail: Option<String>,
    pub description: Option<String>,
}

/// A command the server asks the client to run once an entry is inserted.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CompletionCommand {
    pub title: String,
    pub command: String,
    /// The arguments as the server gave them, in JSON.
    pub arguments: String,
}

/// What a completion entry is, as far as the list needs to know. The editor filters a
/// list of only types without a language of its own.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CompletionKind {
    #[default]
    Unknown,
    Text,
    Method,
    Function,
    Constructor,
    Field,
    Variable,
    Class,
    Interface,
    Module,
    Property,
    Unit,
    Value,
    Enum,
    Keyword,
    Snippet,
    Color,
    File,
    Reference,
    Folder,
    Constant,
    Struct,
    Event,
    Operator,
    TypeParameter,
}

impl CompletionKind {
    /// A class, an interface, an enum or another name of a type.
    pub fn is_type(self) -> bool {
        matches!(
            self,
            CompletionKind::Class
                | CompletionKind::Interface
                | CompletionKind::Enum
                | CompletionKind::Struct
                | CompletionKind::TypeParameter
        )
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CompletionEntry {
    pub label: String,
    pub label_details: Option<CompletionLabelDetails>,
    pub detail: Option<String>,
    pub filter_text: Option<String>,
    pub sort_text: Option<String>,
    pub insert_text: Option<String>,
    /// The text the entry replaces. `None` is the word before the caret. A template
    /// that rewrites an expression, `.var` for one, gives its own span.
    pub span: Option<TextSpan>,
    /// `insert_text` is a snippet with tab stops, not plain text.
    pub snippet: bool,
    /// Edits of other places of the same document, an import line for one. They apply
    /// with the insertion as one step of undo.
    pub additional_edits: Vec<DocumentEdit>,
    /// Run after the insertion.
    pub command: Option<CompletionCommand>,
    /// The provider's own handle of the entry, for [`CompletionResolveProvider`].
    pub resolve: Option<String>,
    /// What the entry names, when the provider tells.
    pub kind: CompletionKind,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CompletionList {
    pub entries: Vec<CompletionEntry>,
    /// False when typing more can bring entries that are not in this list.
    pub complete: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavigationKind {
    Definition,
    TypeDefinition,
    Implementation,
    /// Ctrl+U: the method this one overrides, or the super class.
    SuperMethod,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NavigationQuery {
    pub kind: NavigationKind,
    pub spot: DocumentSpot,
}

/// Documentation in Markdown.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HoverText(pub String);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SymbolEntry {
    pub name: String,
    pub container: Option<String>,
    pub place: Option<SourceSpan>,
}

/// The text a user typed into a symbol search.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SymbolQuery(pub String);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Error,
    Warning,
    Information,
    Hint,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiagnosticEntry {
    pub span: TextSpan,
    pub severity: Severity,
    pub message: String,
    /// The server marked the span as something that can go, an unused declaration for
    /// one. The editor greys it out the way an IDE does.
    pub unnecessary: bool,
}

/// The document whose diagnostics are wanted, by URI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiagnosticsQuery(pub String);

/// The kind of a semantic token, as far as the editor draws it. A language maps the
/// names of its server onto these, so the editor needs no vocabulary of its own.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum SemanticTokenKind {
    #[default]
    Unknown,
    Namespace,
    /// A class, an interface, an enum or another name of a type.
    Type,
    TypeParameter,
    Parameter,
    /// A local variable.
    Variable,
    /// A field or another member of a type.
    Property,
    EnumMember,
    Function,
    Method,
    Keyword,
    Modifier,
    Comment,
    String,
    Number,
    Operator,
    /// An annotation or a decorator.
    Annotation,
}

impl SemanticTokenKind {
    /// The kind of the token a server calls `name`, or `None` for a name the editor
    /// does not colour.
    pub fn from_lsp(name: &str) -> Option<Self> {
        Some(match name {
            "namespace" => Self::Namespace,
            "type" | "class" | "interface" | "enum" | "struct" => Self::Type,
            "typeParameter" => Self::TypeParameter,
            "parameter" => Self::Parameter,
            "variable" => Self::Variable,
            "property" | "field" | "event" => Self::Property,
            "enumMember" => Self::EnumMember,
            "function" | "macro" => Self::Function,
            "method" => Self::Method,
            "keyword" => Self::Keyword,
            "modifier" => Self::Modifier,
            "comment" => Self::Comment,
            "string" | "regexp" => Self::String,
            "number" => Self::Number,
            "operator" => Self::Operator,
            "annotation" | "decorator" => Self::Annotation,
            _ => return None,
        })
    }
}

/// What the server says about a token besides its kind. A language fills the ones it
/// knows; the rest stay off.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct SemanticTokenModifiers {
    /// The place where the symbol is declared.
    pub declaration: bool,
    pub static_member: bool,
    pub readonly: bool,
    pub deprecated: bool,
    pub abstract_member: bool,
}

impl SemanticTokenModifiers {
    /// Turns on the modifiers a server calls `name`. Unknown names are ignored.
    pub fn add(&mut self, name: &str) {
        match name {
            "declaration" | "definition" => self.declaration = true,
            "static" => self.static_member = true,
            "readonly" => self.readonly = true,
            "deprecated" => self.deprecated = true,
            "abstract" => self.abstract_member = true,
            _ => {}
        }
    }

    /// A constant member, which servers report as static and read-only.
    pub fn is_constant(self) -> bool {
        self.static_member && self.readonly
    }
}

/// One token of a document, in the positions the server answered with.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SemanticToken {
    pub span: TextSpan,
    pub kind: SemanticTokenKind,
    pub modifiers: SemanticTokenModifiers,
}

/// The tokens of a document, or of the part of a big one that is on screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticTokensQuery {
    pub uri: String,
    pub version: i32,
    /// `None` asks for the whole document.
    pub range: Option<TextSpan>,
}

/// A diagnostic with the code the server gives it, for the tree of the Inspection
/// Results window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiagnosticNote {
    pub span: TextSpan,
    pub severity: Severity,
    pub message: String,
    /// The code of the problem, such as the name of a compiler option.
    pub code: Option<String>,
}

/// The diagnostics the server has for one file, open or not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileDiagnostics {
    pub uri: String,
    pub diagnostics: Vec<DiagnosticNote>,
}

/// A kind of problem a language knows before any result names it, for Run Inspection
/// by Name. Several codes may share one title.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProblemKind {
    /// One code of the server that the kind covers.
    pub code: String,
    /// The short title that groups the findings of the kind.
    pub name: String,
}

/// Every diagnostic the server has published for the project, for Inspect Code over a
/// whole scope. A server that publishes only the open documents answers with those.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WorkspaceDiagnosticsQuery;

/// Asks the server to make the diagnostics of the whole project current, by building
/// the workspace. The answer comes when the work is done.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PrepareDiagnosticsQuery;

/// A problem of the project itself rather than of one source file: a build script
/// that the language server could not import, or a project it could not configure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectProblem {
    /// The file to show the problem on, when the server names one.
    pub file: Option<PathBuf>,
    /// The line in `file`, counted from zero.
    pub line: u32,
    pub severity: Severity,
    /// The whole text of the problem.
    pub message: String,
    /// One line that names the cause, for a status bar and a notification.
    pub summary: String,
    /// The server log, when it has one, for the Show Log action.
    pub log: Option<PathBuf>,
}

/// The problems of the project itself. The stream stays open and sends the current
/// list whenever it changes; an empty list clears them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProjectProblemsQuery;

/// The parameters of the call around a place, Ctrl+P.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignatureQuery(pub DocumentSpot);

/// Signatures, one per line, the active one first.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SignatureText(pub String);

/// One parameter of a call signature, as it appears inside the label.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CallParameter {
    pub label: String,
}

/// One overload of a call, the label of the whole signature and the parts of it that
/// are parameters. The list marks the active parameter of the active overload.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CallSignature {
    pub label: String,
    pub parameters: Vec<CallParameter>,
}

/// The signatures around a place, the active overload and the parameter the caret is
/// in. The active parameter follows the caret; Ctrl+Up and Ctrl+Down pick an overload.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SignatureHelp {
    pub signatures: Vec<CallSignature>,
    pub active_signature: usize,
    pub active_parameter: usize,
}

/// The type expected at a place, from the signature of a call or the declaration to
/// the left. `None` when nothing proves a type.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExpectedType {
    pub type_name: Option<String>,
}

/// The documentation page of the symbol at a place. A language that knows no link gives
/// `None`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExternalDoc {
    pub url: Option<String>,
}

/// The text behind a URI that is not a file, a compiled class for one. Read only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceTextQuery(pub String);

/// One edit of a document, in the coordinates of the text before it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentEdit {
    pub span: TextSpan,
    pub text: String,
}

/// Edits of several documents, the answer of a code action.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SourceEdit {
    pub uri: String,
    pub edits: Vec<DocumentEdit>,
}

/// A whole document to format, Ctrl+Alt+L.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormatQuery {
    pub uri: String,
    pub version: i32,
    pub tab_size: u32,
    pub insert_spaces: bool,
}

/// A part of a document to format, for the only-changed-lines mode of the Reformat
/// File dialog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RangeFormatQuery {
    pub uri: String,
    pub version: i32,
    pub range: TextSpan,
    pub tab_size: u32,
    pub insert_spaces: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionKind {
    QuickFix,
    Refactor,
    Source,
    /// Ctrl+Alt+O.
    OrganizeImports,
    /// The safe fixes of the whole file, for Code Cleanup.
    FixAll,
}

/// Actions for a span, Alt+Enter. `only` narrows them to one kind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionQuery {
    pub uri: String,
    pub version: i32,
    pub span: TextSpan,
    pub only: Option<ActionKind>,
}

/// An action with its edits already computed, or one whose server runs it from values
/// the user picks first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionEntry {
    pub title: String,
    pub kind: Option<ActionKind>,
    /// The kind as the server names it, `refactor.extract.function` for one.
    pub code: Option<String>,
    pub preferred: bool,
    pub edits: Vec<SourceEdit>,
    pub renames: Vec<FileRename>,
    /// The provider's own token of an action that needs a form before it runs, passed
    /// back in [`ActionParametersQuery`]. `None` when the edits are all it needs.
    pub handle: Option<String>,
}

/// What an action needs from the user before it runs. The window draws a small form
/// of it; an action without one runs from the edits of its [`ActionEntry`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ActionParameters {
    pub title: String,
    /// The rows to tick, such as the members of a new interface.
    pub members: Vec<ActionMember>,
    pub members_label: String,
    /// The values to type, such as the name of a new type.
    pub fields: Vec<ActionField>,
    /// The places a new file may go, of which the action needs exactly one.
    pub choices: Vec<ActionChoice>,
    pub choices_label: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionMember {
    pub name: String,
    pub detail: Option<String>,
    pub checked: bool,
    /// The value the provider expects back for this row.
    pub value: String,
}

/// A value the form asks for, with the key the provider gave it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionField {
    pub key: String,
    pub label: String,
    pub value: String,
}

/// One row of a single choice, with the value the provider expects back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionChoice {
    pub name: String,
    pub detail: Option<String>,
    pub value: String,
}

/// The token of an action whose form the window shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionParametersQuery(pub String);

/// The values of a filled form.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionApplyRequest {
    pub handle: String,
    pub members: Vec<String>,
    pub fields: Vec<ActionField>,
    pub choice: Option<String>,
}

/// The edits of a filled form, or the reason the language refused it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ActionOutcome {
    pub change: WorkspaceChange,
    pub error: Option<String>,
}

/// A file that a change moves, by URI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileRename {
    pub from: String,
    pub to: String,
}

/// Edits of many documents and the files they move, applied together.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkspaceChange {
    pub edits: Vec<SourceEdit>,
    pub renames: Vec<FileRename>,
}

impl WorkspaceChange {
    pub fn is_empty(&self) -> bool {
        self.renames.is_empty() && self.edits.iter().all(|source| source.edits.is_empty())
    }
}

/// Shift+F6: the symbol at `spot` gets `new_name` everywhere.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenameQuery {
    pub spot: DocumentSpot,
    pub new_name: String,
}

/// What F6 moves. A file holds its type, so moving it into another package moves the
/// type with every reference and its `package` line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MoveKind {
    File,
}

/// F6: where the file at `spot` can go.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MoveQuery {
    pub spot: DocumentSpot,
    pub kind: MoveKind,
}

/// One destination of a move. `handle` is the provider's own value for it and is given
/// back unchanged in [`MoveRequest`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MoveDestination {
    pub name: String,
    pub detail: Option<String>,
    /// The folder itself, when the provider names it. A server may name its project and
    /// source root in its own notation, which need not match the folder on disk.
    pub folder: Option<PathBuf>,
    pub handle: String,
}

/// F6 after a destination was chosen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MoveRequest {
    pub spot: DocumentSpot,
    pub kind: MoveKind,
    pub destination: MoveDestination,
    pub update_references: bool,
}

/// The edits of a move, or the reason the server refused it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MoveOutcome {
    pub change: WorkspaceChange,
    pub error: Option<String>,
}

/// One parameter of a signature. `original_index` tells the provider which parameter of
/// the old signature this row came from; an added one has none.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SignatureParameter {
    pub type_name: String,
    pub name: String,
    /// The value a caller of the old signature gets for an added parameter.
    pub default_value: String,
    pub original_index: Option<usize>,
}

/// One declared exception of a signature. `handle` is the provider's own value for the
/// type and is given back unchanged.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SignatureException {
    pub type_name: String,
    pub handle: String,
}

/// The signature of what the caret is on, for the Change Signature window.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MethodSignature {
    pub name: String,
    pub visibility: String,
    pub return_type: String,
    pub parameters: Vec<SignatureParameter>,
    pub exceptions: Vec<SignatureException>,
}

/// The signature under a place, or the reason the language cannot give one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChangeSignatureInfo {
    pub signature: Option<MethodSignature>,
    pub error: Option<String>,
}

/// Ctrl+F6: the method at `spot` gets `signature`, and every call is updated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangeSignatureRequest {
    pub spot: DocumentSpot,
    pub signature: MethodSignature,
}

/// The edits of a change of signature, or the reason the language refused it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChangeSignatureOutcome {
    pub change: WorkspaceChange,
    pub error: Option<String>,
}

/// What Source Generate offers for a type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GenerateKind {
    Constructor,
    Getter,
    Setter,
    GetterAndSetter,
    EqualsAndHashCode,
    ToString,
    OverrideMethods,
    ImplementMethods,
    Delegate,
    Test,
}

/// Alt+Insert: what can be generated at `spot`, or the edits for the chosen parts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GenerateQuery {
    pub spot: DocumentSpot,
    pub kind: GenerateKind,
}

/// One field or method that a generation may include. `handle` is the provider's own
/// value for it and is given back unchanged in [`GenerateApplyRequest`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GenerateChoice {
    pub label: String,
    pub detail: Option<String>,
    /// Whether the answer starts with the choice marked.
    pub checked: bool,
    pub handle: String,
}

/// A source file that a generation creates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GeneratedFile {
    pub path: PathBuf,
    pub text: String,
}

/// The parts to choose for a generation, the edits when nothing is chosen, or the files
/// that the generation creates.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Generation {
    pub choices: Vec<GenerateChoice>,
    pub change: Option<WorkspaceChange>,
    pub files: Vec<GeneratedFile>,
    pub error: Option<String>,
}

/// Alt+Insert after the parts are marked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GenerateApplyRequest {
    pub spot: DocumentSpot,
    pub kind: GenerateKind,
    pub handles: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HierarchyDirection {
    Supertypes,
    Subtypes,
}

/// Ctrl+H: types above or below the one at `spot`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HierarchyQuery {
    pub spot: DocumentSpot,
    pub direction: HierarchyDirection,
}

pub type CompletionProvider = dyn Provider<DocumentSpot, CompletionList>;
/// Asks the provider for the parts a server sends lazily, its additional edits for one.
pub type CompletionResolveProvider = dyn Provider<CompletionEntry, CompletionEntry>;
/// The overloads and the active parameter around a place, Ctrl+P.
pub type SignatureInfoProvider = dyn Provider<SignatureQuery, SignatureHelp>;
/// The type expected at a place, Ctrl+Shift+Space.
pub type ExpectedTypeProvider = dyn Provider<DocumentSpot, ExpectedType>;
/// The documentation page of the symbol at a place, Shift+F1.
pub type ExternalDocProvider = dyn Provider<DocumentSpot, ExternalDoc>;

/// A command the server runs itself through `workspace/executeCommand`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandQuery {
    pub command: String,
    /// The arguments as the server gave them, in JSON.
    pub arguments: String,
}

pub type ExecuteCommandProvider = dyn Provider<CommandQuery, ()>;
pub type NavigationProvider = dyn Provider<NavigationQuery, Vec<SourceSpan>>;
pub type HoverProvider = dyn Provider<DocumentSpot, HoverText>;
pub type WorkspaceSymbolProvider = dyn Provider<SymbolQuery, Vec<SymbolEntry>>;
pub type SignatureProvider = dyn Provider<SignatureQuery, SignatureText>;
pub type SourceTextProvider = dyn Provider<SourceTextQuery, String>;
pub type FormatProvider = dyn Provider<FormatQuery, Vec<DocumentEdit>>;
/// A range of a document formatted on its own.
pub type RangeFormatProvider = dyn Provider<RangeFormatQuery, Vec<DocumentEdit>>;
pub type ActionProvider = dyn Provider<ActionQuery, Vec<ActionEntry>>;
/// The form of an action whose server computes its edits only from the values the
/// user picks, as the provider token of its [`ActionEntry`] names it.
pub type ActionParametersProvider = dyn Provider<ActionParametersQuery, Option<ActionParameters>>;
pub type ActionApplyProvider = dyn Provider<ActionApplyRequest, ActionOutcome>;
/// Alt+F7: every use of the symbol at a spot, its declaration included.
pub type ReferencesProvider = dyn Provider<DocumentSpot, Vec<SourceSpan>>;
pub type RenameProvider = dyn Provider<RenameQuery, WorkspaceChange>;
pub type MoveDestinationProvider = dyn Provider<MoveQuery, Vec<MoveDestination>>;
pub type MoveProvider = dyn Provider<MoveRequest, MoveOutcome>;
pub type ChangeSignatureInfoProvider = dyn Provider<DocumentSpot, ChangeSignatureInfo>;
pub type ChangeSignatureProvider = dyn Provider<ChangeSignatureRequest, ChangeSignatureOutcome>;
pub type HierarchyProvider = dyn Provider<HierarchyQuery, Vec<SymbolEntry>>;
pub type GenerateProvider = dyn Provider<GenerateQuery, Generation>;
pub type GenerateApplyProvider = dyn Provider<GenerateApplyRequest, Generation>;
/// Other occurrences of the symbol at a spot in the same document.
pub type HighlightProvider = dyn Provider<DocumentSpot, Vec<TextSpan>>;

/// The semantic tokens of a document, or of the visible part of a big one.
pub type SemanticTokensProvider = dyn Provider<SemanticTokensQuery, Vec<SemanticToken>>;

/// What a use of a symbol does with it, as far as a language can tell.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum UsageKind {
    #[default]
    Unknown,
    Read,
    Write,
    Import,
    Call,
    Declaration,
}

impl UsageKind {
    /// The group title of the Usages window.
    pub fn label(self) -> &'static str {
        match self {
            Self::Unknown => "Other",
            Self::Read => "Read",
            Self::Write => "Write",
            Self::Import => "Import",
            Self::Call => "Call",
            Self::Declaration => "Declaration",
        }
    }
}

/// One use of a symbol with what the language knows about it. `container` names the
/// declarations around the use, outermost first, joined by dots.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsageEntry {
    pub target: SourceSpan,
    pub kind: UsageKind,
    pub container: Option<String>,
}

/// Alt+F7: every use of the symbol at a spot, with its kind and its container.
pub type UsageProvider = dyn Provider<DocumentSpot, Vec<UsageEntry>>;

/// The stream stays open and sends the full list whenever it changes.
pub type DiagnosticsProvider = dyn Provider<DiagnosticsQuery, Vec<DiagnosticEntry>>;

/// Every diagnostic the server has published, for Inspect Code over a scope.
pub type WorkspaceDiagnosticsProvider =
    dyn Provider<WorkspaceDiagnosticsQuery, Vec<FileDiagnostics>>;

/// Builds the workspace so that the diagnostics of every file are current.
pub type PrepareDiagnosticsProvider = dyn Provider<PrepareDiagnosticsQuery, ()>;

/// Problems of the project itself, not of one source file.
pub type ProjectProblemsProvider = dyn Provider<ProjectProblemsQuery, Vec<ProjectProblem>>;

/// What happened to a file the server watches on disk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileChange {
    Created,
    Changed,
    Removed,
}

/// Keeps a language server in step with open documents. Never waits, so it is safe
/// right after an edit on the path of a keystroke.
pub trait SemanticServer: Send + Sync {
    fn open(&self, uri: &str, version: i32, text: Arc<str>);

    /// `text` is the document after `edits`. No edits sends the whole text.
    fn change(&self, uri: &str, version: i32, text: Arc<str>, edits: Vec<DocumentEdit>);

    fn save(&self, uri: &str);

    fn close(&self, uri: &str);

    /// A file changed on disk outside the open documents, such as a build script
    /// written by another tool. A server that watches files may import the project
    /// again.
    fn file_changed(&self, uri: &str, change: FileChange) {
        let _ = (uri, change);
    }

    /// Asks the server to import the project again, as it does when a build file
    /// appears. The build tool window calls it after reading the model anew.
    fn reimport(&self) {}

    /// The version the server has of an open document. Lets a caller that does not own
    /// the buffer, an agent tool for one, ask about the text as it is now.
    fn version(&self, uri: &str) -> Option<i32>;
}

/// The byte offset of `point` in `text`. A point past a line end stops at it, past the
/// last line at the end of the text.
pub fn offset_in(text: &str, point: TextPoint) -> usize {
    let mut start = 0;
    for _ in 0..point.line {
        match text[start..].find('\n') {
            Some(end) => start += end + 1,
            None => return text.len(),
        }
    }
    let line = &text[start..];
    let line = &line[..line.find('\n').unwrap_or(line.len())];
    let mut units = 0;
    for (offset, character) in line.char_indices() {
        if units >= point.character as usize {
            return start + offset;
        }
        units += character.len_utf16();
    }
    start + line.len()
}

/// `text` with `edits` applied, all in the coordinates of `text`. For a file that no
/// buffer holds.
pub fn apply_to_text(text: &str, edits: &[DocumentEdit]) -> String {
    let mut ranges: Vec<(usize, usize, &str)> = edits
        .iter()
        .map(|edit| {
            let start = offset_in(text, edit.span.start);
            let end = offset_in(text, edit.span.end).max(start);
            (start, end, edit.text.as_str())
        })
        .collect();
    ranges.sort_by_key(|(start, end, _)| (*start, *end));
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for (start, end, new_text) in ranges {
        let start = start.max(at);
        out.push_str(&text[at..start]);
        out.push_str(new_text);
        at = end.max(start);
    }
    out.push_str(&text[at..]);
    out
}

#[cfg(test)]
mod tests {
    use super::{DocumentEdit, TextPoint, TextSpan, apply_to_text, offset_in};

    fn at(line: u32, character: u32) -> TextPoint {
        TextPoint { line, character }
    }

    #[test]
    fn offsets_count_utf16_and_clamp() {
        let text = "a😀b\nxy";
        assert_eq!(offset_in(text, at(0, 3)), "a😀".len());
        assert_eq!(offset_in(text, at(0, 99)), "a😀b".len());
        assert_eq!(offset_in(text, at(1, 1)), "a😀b\nx".len());
        assert_eq!(offset_in(text, at(5, 0)), text.len());
    }

    #[test]
    fn edits_apply_in_the_coordinates_of_the_old_text() {
        let edit = |start, end, text: &str| DocumentEdit {
            span: TextSpan { start, end },
            text: text.to_owned(),
        };
        let edits = [
            edit(at(1, 0), at(1, 2), "zz"),
            edit(at(0, 0), at(0, 1), "Old"),
        ];
        assert_eq!(apply_to_text("a\nxy\n", &edits), "Old\nzz\n");
    }
}
