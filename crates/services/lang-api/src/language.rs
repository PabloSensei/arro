use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::{
    ActionProvider, AddOverridableMethodsProvider, CallHierarchyProvider,
    ChangeSignatureInfoProvider, ChangeSignatureProvider, CompletionProvider,
    CompletionResolveProvider, DefinitionPreviewProvider, DependencyUpdatesProvider,
    DiagnosticsProvider, ExecuteCommandProvider, FileSymbolsProvider, FormatProvider,
    GenerateApplyProvider, GenerateProvider, HierarchyProvider, HighlightProvider, HoverProvider,
    MemberActionParametersProvider, MemberActionProvider, MethodHierarchyProvider,
    MoveDestinationProvider, MoveMembersFormProvider, MovePlace, MoveProvider, NavigationProvider,
    OverridableMethodsProvider, PrepareDiagnosticsProvider, ProjectProblemsProvider,
    RangeFormatProvider, ReferenceStringProvider, ReferencesProvider, RenameProvider,
    SemanticServer, SignatureProvider, SourceTextProvider, SymbolProvider, SyntaxProvider,
    TypeHierarchyProvider, WorkspaceDiagnosticsProvider, WorkspaceSymbolProvider,
};
use crate::{
    ExpectedTypeProvider, ExternalDocProvider, LibraryClassProvider, SignatureInfoProvider,
    SourcesProvider,
};

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

    /// Other languages of the same project, for files the main language does not
    /// match: its configuration files, for one. The editor asks each of them for such
    /// a file and keeps the first that matches; highlighting still comes from
    /// `file-syntax`, so these languages bring completion, documentation and checks.
    fn embedded_languages(&self) -> Vec<Arc<dyn LanguageSupport>> {
        Vec::new()
    }

    /// Whether a place with no declaration shows its documentation instead. A
    /// language whose places map onto configuration files answers `true`, so that
    /// Ctrl+B on a key opens what the language knows about it.
    fn definition_falls_back_to_hover(&self) -> bool {
        false
    }

    fn syntax(&self) -> &dyn SyntaxProvider;

    /// The language whose text lives inside the text of this one, such as the query of a
    /// string literal. The window asks it for the entries and the checks of those places.
    fn embedded_text(&self) -> Option<&dyn crate::EmbeddedTextProvider> {
        None
    }

    /// Reads a pasted stack trace or thread dump and finds the places of its frames.
    fn stack_traces(&self) -> Option<&crate::TraceProvider> {
        None
    }

    /// Asks a running program for the stacks of its threads.
    fn thread_dump(&self) -> Option<&crate::DumpProvider> {
        None
    }

    /// Declarations of one source file, read natively.
    fn symbols(&self) -> Option<&dyn SymbolProvider> {
        None
    }

    fn server(&self) -> Option<&dyn SemanticServer> {
        None
    }

    /// Whether the language server can answer now. Navigation falls back to the project
    /// index while it cannot, such as during the import of the project.
    fn server_ready(&self) -> bool {
        true
    }

    /// The names of the types people type most in this language. The completion list
    /// puts them before the other types, as IDEA does. Empty by default, so a language
    /// that has no such list keeps the order of its provider.
    fn preferred_completion_names(&self) -> &'static [&'static str] {
        &[]
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

    /// The newest stable version of the dependencies the project declares, for the
    /// window that lists them.
    fn dependency_updates(&self) -> Option<&DependencyUpdatesProvider> {
        None
    }

    /// Whether the project asks for newer versions of its dependencies at all.
    fn set_dependency_updates(&self, enabled: bool) {
        let _ = enabled;
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

    /// Which class of a library a read-only tab shows, and whether its sources are
    /// attached. A class whose sources are missing gets a line above its tab.
    fn library_class(&self) -> Option<&LibraryClassProvider> {
        None
    }

    /// Fetches and attaches the sources of library classes.
    fn library_sources(&self) -> Option<&SourcesProvider> {
        None
    }

    fn format(&self) -> Option<&FormatProvider> {
        None
    }

    fn actions(&self) -> Option<&ActionProvider> {
        None
    }

    /// The expressions under the caret that an extract may take, when the server names
    /// more than the one its action would use.
    fn refactor_selections(&self) -> Option<&crate::RefactorSelectionsProvider> {
        None
    }

    /// The processes of the machine that already listen for a debugger, for Attach to
    /// Process. A language that cannot list them makes the window ask for an address.
    fn debug_processes(&self) -> Option<&crate::DebugProcessProvider> {
        None
    }

    /// The edits of an action the server computes only on resolve.
    fn action_resolve(&self) -> Option<&crate::ActionResolveProvider> {
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

    /// F6 on a member: the members its type gives away and the types of the project
    /// that can take them.
    fn move_members_form(&self) -> Option<&MoveMembersFormProvider> {
        None
    }

    /// What F6 works on at a place of a file. A language that moves no members answers
    /// [`MovePlace::File`].
    fn move_place(&self, text: &str, offset: usize) -> MovePlace {
        let _ = (text, offset);
        MovePlace::File
    }

    /// Ctrl+F6: the signature the caret is on, and the edits of a new one.
    fn change_signature_info(&self) -> Option<&ChangeSignatureInfoProvider> {
        None
    }

    fn change_signature(&self) -> Option<&ChangeSignatureProvider> {
        None
    }

    /// Pull Members Up, Push Members Down, Extract Superclass and Introduce Parameter
    /// Object: the members and the places the form offers.
    fn member_action_form(&self) -> Option<&MemberActionParametersProvider> {
        None
    }

    /// The edits of a filled member refactoring form.
    fn apply_member_action(&self) -> Option<&MemberActionProvider> {
        None
    }

    fn hierarchy(&self) -> Option<&HierarchyProvider> {
        None
    }

    /// Ctrl+H: the type at a place and the types above and below it, one level per
    /// request.
    fn type_hierarchy(&self) -> Option<&TypeHierarchyProvider> {
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

    /// Create Test: the libraries, the defaults and the methods of the window.
    fn test_form(&self) -> Option<&crate::TestFormProvider> {
        None
    }

    /// Create Test after the window is filled: the file of the new test.
    fn create_test(&self) -> Option<&crate::TestCreateProvider> {
        None
    }

    /// The Fix button of Create Test: the edits that add a missing library.
    fn test_framework_fix(&self) -> Option<&crate::TestFixProvider> {
        None
    }

    /// Whether a test class can be made for the declaration at `offset`, the Create Test
    /// row of Alt+Enter.
    fn can_create_test(&self, text: &str, offset: usize) -> bool {
        let _ = (text, offset);
        false
    }

    /// Ctrl+Shift+H: where the method under a place is defined, inherited or awaited.
    fn method_hierarchy(&self) -> Option<&MethodHierarchyProvider> {
        None
    }

    /// The gutter marks of a file: the programs and the tests a Run or a Debug starts.
    fn run_points(&self) -> Option<&crate::RunPointsProvider> {
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

    /// The HTTP handlers of one source file, read natively. The Endpoints window scans
    /// the whole project with it off the UI thread.
    fn endpoints(&self) -> Option<&dyn crate::EndpointProvider> {
        None
    }

    /// The dependency-injection model of the project, read natively in the background.
    /// The answer stream sends the model again after every change of the sources.
    fn beans(&self) -> Option<&dyn crate::BeanProvider> {
        None
    }

    /// The types around a place or a folder with their relationships, read natively from
    /// the sources in the background, for a diagram.
    fn type_graph(&self) -> Option<&crate::TypeGraphProvider> {
        None
    }

    /// The messages of the method at a place, read natively from the sources in the
    /// background, for a sequence diagram.
    fn sequence_diagram(&self) -> Option<&crate::SequenceDiagramProvider> {
        None
    }

    /// Find and Replace Structurally: templates over the shape of the code, read by the
    /// language itself. The window walks the project and runs the search in the
    /// background.
    fn structural_search(&self) -> Option<&dyn crate::StructuralSearchProvider> {
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

    /// The checks the language runs, for Settings and for the menu of a problem. The
    /// list is static data, so the window may ask for it while it draws.
    fn inspections(&self) -> Vec<crate::InspectionInfo> {
        Vec::new()
    }

    /// The pieces of a document the spelling check reads. A language that owns a file
    /// answers with its comments, its strings and its declarations; `None` lets the
    /// editor read the text by the file name.
    fn spelling(&self) -> Option<&dyn crate::SpellingProvider> {
        None
    }

    /// The choices of the user for the checks of the project. A language sends the ones
    /// its server understands and leaves a check that is off unsent.
    fn set_inspection_settings(&self, settings: crate::InspectionSettings) {
        let _ = settings;
    }

    /// The check a diagnostic code names, with the title and the level of the catalog.
    fn inspection_of(&self, code: Option<&str>, message: &str) -> Option<crate::InspectionInfo> {
        let _ = (code, message);
        None
    }

    /// The edit of a suppression of a check at a place, or `None` for a language that
    /// takes the comment of the editor instead.
    fn suppression_edit(&self, query: crate::SuppressionQuery) -> Option<crate::DocumentEdit> {
        let _ = query;
        None
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

    /// The files of the localized `.properties` set `path` belongs to, the base file and
    /// the files of its locales. `None` when the language does not know the file; the
    /// editor then builds the set from the file names in the same folder.
    fn bundle_files(&self, path: &Path) -> Option<Vec<crate::BundleFile>> {
        let _ = path;
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
