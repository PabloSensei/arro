use std::path::{Path, PathBuf};

use crate::{
    ActionProvider, AddOverridableMethodsProvider, CallHierarchyProvider,
    ChangeSignatureInfoProvider, ChangeSignatureProvider, CompletionProvider,
    CompletionResolveProvider, DefinitionPreviewProvider, DiagnosticsProvider,
    ExecuteCommandProvider, FileSymbolsProvider, FormatProvider, GenerateApplyProvider,
    GenerateProvider, HierarchyProvider, HighlightProvider, HoverProvider, MethodHierarchyProvider,
    MoveDestinationProvider, MoveProvider, NavigationProvider, OverridableMethodsProvider,
    PrepareDiagnosticsProvider, ProjectProblemsProvider, RangeFormatProvider,
    ReferenceStringProvider, ReferencesProvider, RenameProvider, SemanticServer, SignatureProvider,
    SourceTextProvider, SymbolProvider, SyntaxProvider, WorkspaceDiagnosticsProvider,
    WorkspaceSymbolProvider,
};
use crate::{ExpectedTypeProvider, ExternalDocProvider, SignatureInfoProvider};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LanguageId(pub &'static str);

/// Everything the core knows about a language. Having none of the optional capabilities
/// is the normal case until a server for the language is up.
pub trait LanguageSupport: Send + Sync {
    fn id(&self) -> LanguageId;
    fn matches(&self, path: &Path) -> bool;

    /// Whether a change of this file on disk concerns the server, such as a build
    /// script the server imports. Open documents reach it through [`SemanticServer`].
    fn watches_file(&self, path: &Path) -> bool {
        let _ = path;
        false
    }

    fn syntax(&self) -> &dyn SyntaxProvider;

    /// Declarations of one source file, read natively.
    fn symbols(&self) -> Option<&dyn SymbolProvider> {
        None
    }

    fn server(&self) -> Option<&dyn SemanticServer> {
        None
    }

    fn completion(&self) -> Option<&CompletionProvider> {
        None
    }

    /// The lazy parts of a completion entry, such as an import line.
    fn completion_resolve(&self) -> Option<&CompletionResolveProvider> {
        None
    }

    /// Commands the server runs itself, a project import for one.
    fn execute_command(&self) -> Option<&ExecuteCommandProvider> {
        None
    }

    fn navigation(&self) -> Option<&NavigationProvider> {
        None
    }

    fn hover(&self) -> Option<&HoverProvider> {
        None
    }

    fn workspace_symbols(&self) -> Option<&WorkspaceSymbolProvider> {
        None
    }

    fn diagnostics(&self) -> Option<&DiagnosticsProvider> {
        None
    }

    /// Hints the language puts between the characters of a line: parameter names,
    /// types. The editor filters what Settings shows.
    fn inlay_hints(&self) -> Option<&crate::InlayHintProvider> {
        None
    }

    /// Tells the language what Settings shows, so that its server can narrow its
    /// answers. A language whose server decides on its own keeps the default.
    fn set_inlay_hint_settings(&self, settings: crate::InlayHintSettings) {
        let _ = settings;
    }

    /// Problems of the project itself, such as a build file the server could not
    /// import. The window shows them in Problems and in a notification.
    fn project_problems(&self) -> Option<&ProjectProblemsProvider> {
        None
    }

    fn signatures(&self) -> Option<&SignatureProvider> {
        None
    }

    /// The overloads and the active parameter of the call around a place.
    fn signature_info(&self) -> Option<&SignatureInfoProvider> {
        None
    }

    /// The type expected at a place, for a list of only matching entries.
    fn expected_type(&self) -> Option<&ExpectedTypeProvider> {
        None
    }

    /// The documentation page of the symbol at a place.
    fn external_doc(&self) -> Option<&ExternalDocProvider> {
        None
    }

    fn source_text(&self) -> Option<&SourceTextProvider> {
        None
    }

    fn format(&self) -> Option<&FormatProvider> {
        None
    }

    fn actions(&self) -> Option<&ActionProvider> {
        None
    }

    /// The form of an action that needs values before its server computes the edits.
    fn action_parameters(&self) -> Option<&crate::ActionParametersProvider> {
        None
    }

    /// The edits of a filled form, asked with the token of its action.
    fn action_apply(&self) -> Option<&crate::ActionApplyProvider> {
        None
    }

    fn highlights(&self) -> Option<&HighlightProvider> {
        None
    }

    fn references(&self) -> Option<&ReferencesProvider> {
        None
    }

    fn rename(&self) -> Option<&RenameProvider> {
        None
    }

    fn move_destinations(&self) -> Option<&MoveDestinationProvider> {
        None
    }

    fn move_file(&self) -> Option<&MoveProvider> {
        None
    }

    /// Ctrl+F6: the signature the caret is on, and the edits of a new one.
    fn change_signature_info(&self) -> Option<&ChangeSignatureInfoProvider> {
        None
    }

    fn change_signature(&self) -> Option<&ChangeSignatureProvider> {
        None
    }

    fn hierarchy(&self) -> Option<&HierarchyProvider> {
        None
    }

    /// The declarations of a file with the details the server gives, for the Structure
    /// tool window. The native symbols are the fallback when the server has none.
    fn file_symbols(&self) -> Option<&FileSymbolsProvider> {
        None
    }

    /// Ctrl+O: the methods a type at a place can override.
    fn overridable_methods(&self) -> Option<&OverridableMethodsProvider> {
        None
    }

    /// Ctrl+O after the methods are picked: the edits that add them.
    fn add_overridable_methods(&self) -> Option<&AddOverridableMethodsProvider> {
        None
    }

    /// Ctrl+Alt+H: the calls that reach a place, and the calls of a call.
    fn call_hierarchy(&self) -> Option<&CallHierarchyProvider> {
        None
    }

    /// Ctrl+Shift+I: the source of the definition at a place.
    fn definition_preview(&self) -> Option<&DefinitionPreviewProvider> {
        None
    }

    /// Ctrl+Alt+Shift+C: the reference to the symbol at a place.
    fn reference_string(&self) -> Option<&ReferenceStringProvider> {
        None
    }

    /// Alt+Insert: the parts a generation offers and the edits it applies.
    fn generate(&self) -> Option<&GenerateProvider> {
        None
    }

    /// Alt+Insert after the parts are marked.
    fn generate_apply(&self) -> Option<&GenerateApplyProvider> {
        None
    }

    /// Ctrl+Shift+H: where the method under a place is defined, inherited or awaited.
    fn method_hierarchy(&self) -> Option<&MethodHierarchyProvider> {
        None
    }

    /// Starts the server again after it stopped, for example for low memory. False
    /// when there is no stopped server to start.
    fn restart_server(&self) -> bool {
        false
    }

    /// Every diagnostic the server has published, for Inspect Code over a scope.
    fn workspace_diagnostics(&self) -> Option<&WorkspaceDiagnosticsProvider> {
        None
    }

    /// Edits that format a part of a document, for the only-changed-lines mode.
    fn range_format(&self) -> Option<&RangeFormatProvider> {
        None
    }

    /// Makes the diagnostics of the whole project current, by building it.
    fn prepare_diagnostics(&self) -> Option<&PrepareDiagnosticsProvider> {
        None
    }

    /// Alt+F7: the uses of the symbol at a place, each with the kind the language knows.
    /// A language that cannot tell the kinds leaves this out and the editor asks
    /// [`Self::references`] instead.
    fn usages(&self) -> Option<&crate::UsageProvider> {
        None
    }

    /// The semantic tokens of a document, which sit on top of the syntax colours.
    fn semantic_tokens(&self) -> Option<&crate::SemanticTokensProvider> {
        None
    }

    /// Labels above the declarations of a document, such as their number of usages.
    fn code_lens(&self) -> Option<&crate::CodeLensProvider> {
        None
    }

    /// The documentation comments of a source file, read natively. The editor shows
    /// them as rendered blocks.
    fn documentation_comments(&self) -> Option<&dyn crate::DocumentationCommentProvider> {
        None
    }

    /// The command of a label the editor shows, when the server resolves labels apart.
    fn code_lens_resolve(&self) -> Option<&crate::CodeLensResolveProvider> {
        None
    }

    /// Tells the language which labels Settings shows, so that its server can stop
    /// sending the ones that are off.
    fn set_code_vision_settings(&self, settings: crate::CodeVisionSettings) {
        let _ = settings;
    }

    /// Whether the provider colours a part of a document on its own. The editor then
    /// asks only for the visible part of a big file.
    fn semantic_tokens_range(&self) -> bool {
        false
    }

    /// The module a file belongs to, for the grouping of Usages and TODO. A project
    /// without modules answers with its own name.
    fn module_of(&self, path: &Path) -> Option<String> {
        let _ = path;
        None
    }

    /// The package or folder a file belongs to, for the grouping of TODO.
    fn package_of(&self, path: &Path) -> Option<String> {
        let _ = path;
        None
    }

    /// Ctrl+Alt+Home: files the language ties to `path`, such as a test for a class.
    /// The window adds the relations it finds by file names.
    fn related_files(&self, path: &Path) -> Vec<PathBuf> {
        let _ = path;
        Vec::new()
    }

    /// F5: the text of a copy of `text` with its declaration named `name`. `None` keeps
    /// the text as is, for a file the language gives no name to.
    fn copy_element_text(&self, text: &str, name: &str) -> Option<String> {
        let _ = (text, name);
        None
    }

    /// The title of a problem in the Inspection Results tree. A language that knows the
    /// codes of its server gives a short title for each, so that findings of one kind
    /// form one group; by default the code is the title and a problem without a code
    /// keeps its message.
    fn problem_kind(&self, code: Option<&str>, message: &str) -> String {
        code.map(str::to_owned)
            .unwrap_or_else(|| message.lines().next().unwrap_or("").to_owned())
    }

    /// The problem kinds the language knows before any inspection ran, for Run
    /// Inspection by Name.
    fn problem_kinds(&self) -> Vec<crate::ProblemKind> {
        Vec::new()
    }

    /// Copy Class: the text of a copy of `text` named `name` in `folder` of the project
    /// at `root`, with every part that depends on the place, such as a package line,
    /// made for the new one. `None` keeps the text as is.
    fn copy_element_in_folder(
        &self,
        text: &str,
        name: &str,
        root: &Path,
        folder: &Path,
    ) -> Option<String> {
        let _ = (text, name, root, folder);
        None
    }
}
