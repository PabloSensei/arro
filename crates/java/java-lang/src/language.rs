use std::path::{Path, PathBuf};
use std::sync::Arc;

use lang_api::{
    ActionProvider, AddOverridableMethodsProvider, CallHierarchyProvider,
    ChangeSignatureInfoProvider, ChangeSignatureProvider, CompletionProvider,
    CompletionResolveProvider, DefinitionPreviewProvider, Deletion, DeletionKind,
    DiagnosticsProvider, ExecuteCommandProvider, ExpressionTypePlace, FileSymbolsProvider,
    FormatProvider, GenerateApplyProvider, GenerateProvider, HierarchyProvider, HighlightProvider,
    HoverPlace, HoverProvider, InlayHintProvider, InlayHintSettings, LanguageId, LanguageSupport,
    MethodHierarchyProvider, MoveDestinationProvider, MoveMembersFormProvider, MovePlace,
    MoveProvider, MoveTargetsFormProvider, NavigationProvider, OverridableMethodsProvider,
    PrepareDiagnosticsProvider, ProjectProblemsProvider, Queries, RangeFormatProvider,
    ReferenceStringProvider, ReferencesProvider, RenameProvider, SemanticServer,
    SemanticTokensProvider, SignatureProvider, SourceTextProvider, StructuralEdit, StructuralMatch,
    StructuralQuery, StructuralSearchProvider, StructuralTemplate, SymbolProvider, SyntaxProvider,
    TypeHierarchyProvider, UnwrapEdit, WorkspaceDiagnosticsProvider, WorkspaceSymbolProvider,
};
use lang_api::{ExpectedTypeProvider, ExternalDocProvider, SignatureInfoProvider};
use tree_sitter_language::LanguageFn;

use crate::JavaProject;
use crate::problems;

pub const LANGUAGE_ID: LanguageId = LanguageId("java");

/// The names of the Java types people type most. The completion list puts them before
/// the other types and before the types of the libraries, as they do in IDEA.
const PREFERRED_COMPLETION_NAMES: &[&str] = &[
    "String",
    "Object",
    "Integer",
    "Long",
    "Boolean",
    "Double",
    "Float",
    "Character",
    "Byte",
    "Short",
    "Number",
    "Math",
    "System",
    "Exception",
    "RuntimeException",
    "Thread",
    "Class",
    "Enum",
    "Iterable",
    "Comparable",
    "CharSequence",
];

/// The type every Java type inherits. The completion list puts its members last.
const ROOT_COMPLETION_TYPE: &str = "Object";

/// The keywords and the literals of the language, none of which names a declaration.
const JAVA_KEYWORDS: &[&str] = &[
    "abstract",
    "assert",
    "boolean",
    "break",
    "byte",
    "case",
    "catch",
    "char",
    "class",
    "const",
    "continue",
    "default",
    "do",
    "double",
    "else",
    "enum",
    "extends",
    "false",
    "final",
    "finally",
    "float",
    "for",
    "goto",
    "if",
    "implements",
    "import",
    "instanceof",
    "int",
    "interface",
    "long",
    "native",
    "new",
    "null",
    "package",
    "private",
    "protected",
    "public",
    "return",
    "short",
    "static",
    "strictfp",
    "super",
    "switch",
    "synchronized",
    "this",
    "throw",
    "throws",
    "transient",
    "true",
    "try",
    "void",
    "volatile",
    "while",
    "_",
];

/// The grammar and the `queries/java` files the editor uses.
pub struct JavaSyntax {
    queries: Queries,
}

impl JavaSyntax {
    pub fn new() -> Self {
        let config = java_syntax::config();
        let queries = Queries {
            highlights: config.highlights,
            folds: config.folds,
            indents: config.indents,
            locals: "",
        };
        Self { queries }
    }
}

impl Default for JavaSyntax {
    fn default() -> Self {
        Self::new()
    }
}

fn deletion_kind(kind: java_syntax::DeletionKind) -> DeletionKind {
    match kind {
        java_syntax::DeletionKind::Type => DeletionKind::Type,
        java_syntax::DeletionKind::Method => DeletionKind::Method,
        java_syntax::DeletionKind::Field => DeletionKind::Field,
        java_syntax::DeletionKind::Parameter => DeletionKind::Parameter,
        java_syntax::DeletionKind::Local => DeletionKind::Local,
    }
}

impl SyntaxProvider for JavaSyntax {
    fn grammar(&self) -> LanguageFn {
        tree_sitter_java_orchard::LANGUAGE
    }

    fn queries(&self) -> &Queries {
        &self.queries
    }

    fn identifiers_around(&self, text: &str, line: usize) -> lang_api::IdentifierLines {
        java_syntax::identifiers_around(text, line).unwrap_or_default()
    }

    fn unwrap_edits(&self, text: &str, offset: usize) -> Vec<UnwrapEdit> {
        java_syntax::unwrap_edits(text, offset)
            .into_iter()
            .map(|edit| UnwrapEdit {
                title: edit.title,
                start: edit.start,
                end: edit.end,
                replacement: edit.replacement,
            })
            .collect()
    }

    fn deletion(&self, text: &str, offset: usize) -> Option<Deletion> {
        let found = java_syntax::deletion(text, offset)?;
        Some(Deletion {
            kind: Some(deletion_kind(found.kind)),
            name: found.name,
            range: found.range,
            name_range: found.name_range,
            method: found.method,
            method_name: found.method_name,
            parameter_index: found.parameter_index,
            sole_type: found.sole_type,
        })
    }

    fn first_type_offset(&self, text: &str) -> Option<usize> {
        java_syntax::first_type_offset(text)
    }

    fn complete_statement(
        &self,
        text: &str,
        offset: usize,
        indent_unit: &str,
    ) -> Option<lang_api::StatementEdit> {
        let edit = java_syntax::complete_statement(text, offset, indent_unit)?;
        Some(lang_api::StatementEdit {
            start: edit.start,
            end: edit.end,
            text: edit.text,
            caret: edit.caret,
        })
    }
}

impl lang_api::DocumentationCommentProvider for JavaProject {
    fn documentation_comments(&self, text: &str) -> Vec<lang_api::DocumentationComment> {
        crate::documentation_comments(text)
    }
}

impl lang_api::EndpointProvider for JavaProject {
    fn file_endpoints(&self, path: &Path, text: &str) -> lang_api::EndpointScan {
        crate::endpoints::scan(path, text)
    }
}

impl lang_api::ImplicitUseProvider for JavaProject {
    fn implicit_use_spans(&self, text: &str) -> Vec<std::ops::Range<u32>> {
        java_syntax::implicit_use_spans(text)
    }
}

impl LanguageSupport for JavaProject {
    fn id(&self) -> LanguageId {
        LANGUAGE_ID
    }
    fn matches(&self, path: &Path) -> bool {
        path.extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("java"))
    }

    fn watches_file(&self, path: &Path) -> bool {
        jdtls_adapter::is_build_file(path)
    }

    fn server_ready(&self) -> bool {
        self.status().borrow().state == sidecar::State::Ready
    }

    fn stop_server(&self) -> bool {
        self.request_stop()
    }

    fn preferred_completion_names(&self) -> &'static [&'static str] {
        PREFERRED_COMPLETION_NAMES
    }

    fn root_completion_type(&self) -> Option<&'static str> {
        Some(ROOT_COMPLETION_TYPE)
    }

    fn embedded_languages(&self) -> Vec<Arc<dyn LanguageSupport>> {
        vec![
            self.spring.clone(),
            self.messages.clone(),
            Arc::new(self.build_scripts.clone()),
            self.migration_files.clone(),
        ]
    }

    fn related_files(&self, path: &Path) -> Vec<PathBuf> {
        let (Some(stem), Some(extension)) = (
            path.file_stem().and_then(|stem| stem.to_str()),
            path.extension(),
        ) else {
            return Vec::new();
        };
        let base = stem
            .strip_suffix("Tests")
            .or_else(|| stem.strip_suffix("Test"))
            .or_else(|| stem.strip_prefix("Test"))
            .unwrap_or(stem);
        let mut names = vec![format!("{base}Test"), format!("{base}Tests")];
        names.push(format!("Test{base}"));
        if stem != base {
            names.push(base.to_owned());
        }
        let Some(parent) = path.parent() else {
            return Vec::new();
        };
        names
            .into_iter()
            .map(|name| parent.join(format!("{name}.{}", extension.to_string_lossy())))
            .filter(|candidate| candidate != path && candidate.exists())
            .collect()
    }

    fn copy_element_text(&self, text: &str, name: &str) -> Option<String> {
        let start = java_syntax::first_type_offset(text)?;
        let end = text[start..]
            .char_indices()
            .find(|(_, character)| {
                !(character.is_alphanumeric() || *character == '_' || *character == '$')
            })
            .map_or(text.len(), |(offset, _)| start + offset);
        if start >= end || !text[start..end].chars().next()?.is_alphabetic() {
            return None;
        }
        Some(rename_word(text, &text[start..end], name))
    }

    fn copy_element_in_folder(
        &self,
        text: &str,
        name: &str,
        root: &Path,
        folder: &Path,
    ) -> Option<String> {
        let renamed = self.copy_element_text(text, name)?;
        Some(repackage(&renamed, &crate::package_of(root, folder)))
    }

    fn syntax(&self) -> &dyn SyntaxProvider {
        &self.syntax
    }

    fn embedded_text(&self) -> Option<&dyn lang_api::EmbeddedTextProvider> {
        Some(self.data.as_ref())
    }

    fn stack_traces(&self) -> Option<&lang_api::TraceProvider> {
        Some(&self.stack)
    }

    fn thread_dump(&self) -> Option<&lang_api::DumpProvider> {
        Some(&self.stack)
    }

    fn symbols(&self) -> Option<&dyn SymbolProvider> {
        Some(java_syntax::java_symbols())
    }

    fn server(&self) -> Option<&dyn SemanticServer> {
        Some(&self.server)
    }

    fn completion(&self) -> Option<&CompletionProvider> {
        Some(&self.java_messages)
    }

    fn completion_resolve(&self) -> Option<&CompletionResolveProvider> {
        Some(self.providers.as_ref())
    }

    fn execute_command(&self) -> Option<&ExecuteCommandProvider> {
        Some(self.providers.as_ref())
    }

    fn navigation(&self) -> Option<&NavigationProvider> {
        Some(&self.java_messages)
    }

    fn hover(&self) -> Option<&HoverProvider> {
        Some(&self.java_messages)
    }

    fn hover_place(&self, line: &str) -> Option<HoverPlace> {
        crate::hover_place::split(line)
    }

    fn expression_type_place(
        &self,
        text: &str,
        expression: std::ops::Range<usize>,
    ) -> Option<ExpressionTypePlace> {
        crate::expression_type::place(text, expression)
    }

    fn expression_type_from_hover(
        &self,
        text: &str,
        offset: usize,
        answer: &str,
    ) -> Option<String> {
        crate::expression_type::from_hover(text, offset, answer)
    }

    fn workspace_symbols(&self) -> Option<&WorkspaceSymbolProvider> {
        Some(self.providers.as_ref())
    }

    fn diagnostics(&self) -> Option<&DiagnosticsProvider> {
        Some(&self.java_messages)
    }

    fn dependency_updates(&self) -> Option<&lang_api::DependencyUpdatesProvider> {
        Some(&self.build_scripts)
    }

    fn set_dependency_updates(&self, enabled: bool) {
        self.build_scripts.set_enabled(enabled);
    }

    fn set_max_heap_megabytes(&self, megabytes: Option<u32>) {
        self.set_max_heap_mb(megabytes);
    }

    fn semantic_tokens(&self) -> Option<&SemanticTokensProvider> {
        Some(self.providers.as_ref())
    }

    fn semantic_tokens_range(&self) -> bool {
        self.providers.semantic_tokens_range()
    }

    fn code_lens(&self) -> Option<&lang_api::CodeLensProvider> {
        Some(self.providers.as_ref())
    }

    fn code_lens_resolve(&self) -> Option<&lang_api::CodeLensResolveProvider> {
        Some(self.providers.as_ref())
    }

    fn documentation_comments(&self) -> Option<&dyn lang_api::DocumentationCommentProvider> {
        Some(self)
    }

    fn endpoints(&self) -> Option<&dyn lang_api::EndpointProvider> {
        Some(self)
    }

    fn implicit_uses(&self) -> Option<&dyn lang_api::ImplicitUseProvider> {
        Some(self)
    }

    fn beans(&self) -> Option<&dyn lang_api::BeanProvider> {
        Some(self.beans.as_ref())
    }

    fn type_graph(&self) -> Option<&lang_api::TypeGraphProvider> {
        Some(self)
    }

    fn sequence_diagram(&self) -> Option<&lang_api::SequenceDiagramProvider> {
        Some(self)
    }

    fn structural_search(&self) -> Option<&dyn StructuralSearchProvider> {
        Some(&STRUCTURAL_SEARCH)
    }

    fn set_code_vision_settings(&self, settings: lang_api::CodeVisionSettings) {
        self.apply_code_vision_settings(settings);
    }

    fn inspections(&self) -> Vec<lang_api::InspectionInfo> {
        crate::inspections::catalog()
    }

    fn spelling(&self) -> Option<&dyn lang_api::SpellingProvider> {
        Some(self)
    }

    fn set_inspection_settings(&self, settings: lang_api::InspectionSettings) {
        self.apply_inspection_settings(&settings);
    }

    fn inspection_of(&self, code: Option<&str>, message: &str) -> Option<lang_api::InspectionInfo> {
        crate::inspections::inspection_of(code, message)
    }

    fn suppression_edit(
        &self,
        query: lang_api::SuppressionQuery,
    ) -> Option<lang_api::DocumentEdit> {
        crate::inspections::suppression_edit(
            &query.text,
            query.offset,
            query.code.as_deref(),
            query.target,
        )
    }

    fn inlay_hints(&self) -> Option<&InlayHintProvider> {
        Some(self.providers.as_ref())
    }

    fn set_inlay_hint_settings(&self, settings: InlayHintSettings) {
        self.apply_inlay_hint_settings(settings);
    }

    fn project_problems(&self) -> Option<&ProjectProblemsProvider> {
        Some(&self.problems)
    }

    fn signatures(&self) -> Option<&SignatureProvider> {
        Some(self.providers.as_ref())
    }

    fn signature_info(&self) -> Option<&SignatureInfoProvider> {
        Some(self.providers.as_ref())
    }

    fn expected_type(&self) -> Option<&ExpectedTypeProvider> {
        Some(self.providers.as_ref())
    }

    fn external_doc(&self) -> Option<&ExternalDocProvider> {
        Some(self)
    }

    fn source_text(&self) -> Option<&SourceTextProvider> {
        Some(&self.class_files)
    }

    fn library_class(&self) -> Option<&lang_api::LibraryClassProvider> {
        Some(&self.library_sources)
    }

    fn library_sources(&self) -> Option<&lang_api::SourcesProvider> {
        Some(&self.library_sources)
    }

    fn describe_library_uri(&self, uri: &str) -> Option<lang_api::LibraryPlace> {
        crate::library_place::describe(uri)
    }

    fn format(&self) -> Option<&FormatProvider> {
        Some(self.providers.as_ref())
    }

    fn actions(&self) -> Option<&ActionProvider> {
        Some(&self.java_messages)
    }

    fn refactor_selections(&self) -> Option<&lang_api::RefactorSelectionsProvider> {
        Some(self.providers.as_ref())
    }

    fn expressions_at(&self, text: &str, offset: usize) -> Vec<std::ops::Range<usize>> {
        java_syntax::expressions_at(text, offset)
    }

    fn suggest_name(
        &self,
        text: &str,
        expression: std::ops::Range<usize>,
        role: lang_api::NameRole,
        hint: Option<&str>,
    ) -> Option<String> {
        suggested_name(text, expression, role, hint)
    }

    fn debug_processes(&self) -> Option<&lang_api::DebugProcessProvider> {
        Some(&crate::debug::DEBUG_PROCESSES)
    }

    fn action_resolve(&self) -> Option<&lang_api::ActionResolveProvider> {
        Some(self.providers.as_ref())
    }

    fn action_parameters(&self) -> Option<&lang_api::ActionParametersProvider> {
        Some(&self.forms)
    }

    fn action_apply(&self) -> Option<&lang_api::ActionApplyProvider> {
        Some(&self.forms)
    }

    fn highlights(&self) -> Option<&HighlightProvider> {
        Some(self.providers.as_ref())
    }

    fn usages(&self) -> Option<&lang_api::UsageProvider> {
        Some(self.providers.as_ref())
    }

    fn module_of(&self, path: &Path) -> Option<String> {
        let relative = path.strip_prefix(&self.root).ok()?;
        let mut parts = relative.components();
        let first = parts.next()?.as_os_str().to_string_lossy().into_owned();
        if parts.next().is_some() && !matches!(first.as_str(), "src" | "main" | "test") {
            return Some(first);
        }
        self.root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
    }

    fn package_of(&self, path: &Path) -> Option<String> {
        let folder = path.parent()?;
        Some(crate::package_of(&self.root, folder))
    }

    fn references(&self) -> Option<&ReferencesProvider> {
        Some(self.providers.as_ref())
    }

    fn rename(&self) -> Option<&RenameProvider> {
        Some(self.providers.as_ref())
    }

    fn prepare_rename(&self) -> Option<&lang_api::PrepareRenameProvider> {
        Some(self.providers.as_ref())
    }

    /// The words of the grammar that cannot name a declaration.
    fn reserved_word(&self, name: &str) -> bool {
        JAVA_KEYWORDS.contains(&name)
    }

    fn move_destinations(&self) -> Option<&MoveDestinationProvider> {
        Some(self.providers.as_ref())
    }

    fn move_file(&self) -> Option<&MoveProvider> {
        Some(self.providers.as_ref())
    }

    fn move_members_form(&self) -> Option<&MoveMembersFormProvider> {
        Some(&self.move_members)
    }

    fn move_targets_form(&self) -> Option<&MoveTargetsFormProvider> {
        Some(&self.move_targets)
    }

    /// F6 on the declaration of a member moves the member, and on anything else the
    /// file.
    fn move_place(&self, text: &str, offset: usize) -> MovePlace {
        crate::members::move_place(text, offset)
    }

    fn change_signature_info(&self) -> Option<&ChangeSignatureInfoProvider> {
        Some(&self.change_signature)
    }

    fn change_signature(&self) -> Option<&ChangeSignatureProvider> {
        Some(self.providers.as_ref())
    }

    fn member_action_form(&self) -> Option<&lang_api::MemberActionParametersProvider> {
        Some(&self.members)
    }

    fn apply_member_action(&self) -> Option<&lang_api::MemberActionProvider> {
        Some(&self.members)
    }

    fn hierarchy(&self) -> Option<&HierarchyProvider> {
        Some(self.providers.as_ref())
    }

    fn type_hierarchy(&self) -> Option<&TypeHierarchyProvider> {
        Some(self)
    }

    fn file_symbols(&self) -> Option<&FileSymbolsProvider> {
        Some(self)
    }

    fn overridable_methods(&self) -> Option<&OverridableMethodsProvider> {
        Some(self.providers.as_ref())
    }

    fn add_overridable_methods(&self) -> Option<&AddOverridableMethodsProvider> {
        Some(self.providers.as_ref())
    }

    fn call_hierarchy(&self) -> Option<&CallHierarchyProvider> {
        Some(self.providers.as_ref())
    }

    fn definition_preview(&self) -> Option<&DefinitionPreviewProvider> {
        Some(self)
    }

    fn reference_string(&self) -> Option<&ReferenceStringProvider> {
        Some(self)
    }

    fn generate(&self) -> Option<&GenerateProvider> {
        Some(self)
    }

    fn generate_apply(&self) -> Option<&GenerateApplyProvider> {
        Some(self)
    }

    fn test_form(&self) -> Option<&lang_api::TestFormProvider> {
        Some(self)
    }

    fn create_test(&self) -> Option<&lang_api::TestCreateProvider> {
        Some(self)
    }

    fn test_framework_fix(&self) -> Option<&lang_api::TestFixProvider> {
        Some(self)
    }

    /// Alt+Enter on the name of a type offers Create Test for it.
    fn can_create_test(&self, text: &str, offset: usize) -> bool {
        java_syntax::enclosing_type_at(text, offset)
            .is_some_and(|(_, name)| name.start <= offset && offset <= name.end)
    }

    fn method_hierarchy(&self) -> Option<&MethodHierarchyProvider> {
        Some(self)
    }

    fn run_points(&self) -> Option<&lang_api::RunPointsProvider> {
        Some(self)
    }

    fn restart_server(&self) -> bool {
        self.restart()
    }

    fn workspace_diagnostics(&self) -> Option<&WorkspaceDiagnosticsProvider> {
        Some(self.providers.as_ref())
    }

    fn range_format(&self) -> Option<&RangeFormatProvider> {
        Some(self.providers.as_ref())
    }

    fn prepare_diagnostics(&self) -> Option<&PrepareDiagnosticsProvider> {
        Some(self)
    }

    fn problem_kind(&self, code: Option<&str>, message: &str) -> String {
        code.and_then(problems::name)
            .map(str::to_owned)
            .unwrap_or_else(|| problems::message_template(message))
    }

    fn problem_kinds(&self) -> Vec<lang_api::ProblemKind> {
        problems::kinds()
    }
}

/// Whether a line of a Java file imports a type or a static member.
pub(crate) fn is_import_line(line: &str) -> bool {
    let line = line.trim_start();
    line.starts_with("import ") || line.starts_with("import static ")
}

/// Every whole-word `old` of `text` as `new`: the declaration, its constructors and the
/// references of the class to itself, as Copy Class of IDEA renames them.
fn rename_word(text: &str, old: &str, new: &str) -> String {
    let is_word =
        |character: char| character.is_alphanumeric() || character == '_' || character == '$';
    let mut changed = String::with_capacity(text.len() + new.len());
    let mut rest = text;
    while let Some(found) = rest.find(old) {
        let before = rest[..found].chars().next_back();
        let after = rest[found + old.len()..].chars().next();
        changed.push_str(&rest[..found]);
        if before.is_some_and(is_word) || after.is_some_and(is_word) {
            changed.push_str(old);
        } else {
            changed.push_str(new);
        }
        rest = &rest[found + old.len()..];
    }
    changed.push_str(rest);
    changed
}

/// The text with a `package` line for `package`, which is empty for the root of the
/// sources: the line is replaced, added or taken away, and the rest stays as it is.
fn repackage(text: &str, package: &str) -> String {
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut lines: Vec<String> = text
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line).to_owned())
        .collect();
    let header = lines.iter().position(|line| {
        let trimmed = line.trim_start();
        trimmed.starts_with("package ") && trimmed.trim_end().ends_with(';')
    });
    match header {
        Some(index) if package.is_empty() => {
            lines.remove(index);
            if lines.get(index).is_some_and(|line| line.trim().is_empty()) {
                lines.remove(index);
            }
        }
        Some(index) => {
            let line = format!("package {package};");
            if lines[index].trim() != line {
                lines[index] = line;
            }
        }
        None if !package.is_empty() => {
            lines.insert(0, format!("package {package};"));
            lines.insert(1, String::new());
        }
        None => {}
    }
    lines.join(newline)
}

/// Structural search of the language: the templates and the matcher of `java-syntax`.
struct JavaStructuralSearch;

static STRUCTURAL_SEARCH: JavaStructuralSearch = JavaStructuralSearch;

impl StructuralSearchProvider for JavaStructuralSearch {
    fn templates(&self) -> Vec<StructuralTemplate> {
        java_syntax::ready_templates()
    }

    fn variables(&self, pattern: &str) -> Result<Vec<String>, String> {
        java_syntax::structural_variables(pattern)
    }

    fn find(&self, text: &str, query: &StructuralQuery) -> Result<Vec<StructuralMatch>, String> {
        java_syntax::structural_find(query, text)
    }

    fn replace(&self, text: &str, query: &StructuralQuery) -> Result<Vec<StructuralEdit>, String> {
        java_syntax::structural_replace(query, text)
    }
}

/// The name an extraction suggests, read from the syntax of the file. A keyword of the grammar
/// is no name, so the editor keeps the name the server chose.
fn suggested_name(
    text: &str,
    expression: std::ops::Range<usize>,
    role: lang_api::NameRole,
    hint: Option<&str>,
) -> Option<String> {
    java_syntax::suggest_name(text, expression, role, hint)
        .filter(|name| !JAVA_KEYWORDS.contains(&name.as_str()))
}

#[cfg(test)]
mod name_tests {
    use std::ops::Range;

    use lang_api::NameRole;

    use super::suggested_name;

    /// The byte range of the first `needle` of `text`: the expression a test names.
    fn at(text: &str, needle: &str) -> Range<usize> {
        let start = text.find(needle).expect("the needle is in the text");
        start..start + needle.len()
    }

    #[test]
    fn an_initializer_keeps_its_own_name_taken_so_the_name_gets_a_number() {
        let text = "class A {\n    void f() {\n        int pageSize = 5;\n    }\n}\n";
        let five = at(text, "5");
        assert_eq!(
            suggested_name(text, five.clone(), NameRole::Parameter, None).as_deref(),
            Some("pageSize1")
        );
        assert_eq!(
            suggested_name(text, five.clone(), NameRole::Variable, None).as_deref(),
            Some("pageSize1")
        );
        assert_eq!(
            suggested_name(text, five, NameRole::Field, None).as_deref(),
            Some("pageSize1")
        );
    }

    #[test]
    fn a_constant_takes_the_upper_case_of_the_initialized_name() {
        let text = "class A {\n    void f() {\n        int pageSize = 5;\n    }\n}\n";
        assert_eq!(
            suggested_name(text, at(text, "5"), NameRole::Constant, None).as_deref(),
            Some("PAGE_SIZE")
        );
    }

    #[test]
    fn an_argument_gives_the_name_of_its_parameter_when_the_hint_knows_it() {
        let text = "class A {\n    void f() {\n        show(5);\n    }\n}\n";
        let five = at(text, "5");
        assert_eq!(
            suggested_name(
                text,
                five.clone(),
                NameRole::Constant,
                Some("attributeName")
            )
            .as_deref(),
            Some("ATTRIBUTE_NAME")
        );
        assert_eq!(
            suggested_name(
                text,
                five.clone(),
                NameRole::Variable,
                Some("attributeName")
            )
            .as_deref(),
            Some("attributeName")
        );
        assert_eq!(
            suggested_name(text, five, NameRole::Variable, None),
            None,
            "an argument whose parameter is unknown has no name"
        );
    }

    #[test]
    fn a_string_literal_that_reads_as_a_name_gives_its_text() {
        let text = "class A {\n    void f(Model model) {\n        model.addAttribute(\"totalItems\", 3);\n    }\n}\n";
        let literal = at(text, "\"totalItems\"");
        assert_eq!(
            suggested_name(text, literal.clone(), NameRole::Constant, None).as_deref(),
            Some("TOTAL_ITEMS")
        );
        assert_eq!(
            suggested_name(text, literal.clone(), NameRole::Variable, None).as_deref(),
            Some("totalItems")
        );
        assert_eq!(
            suggested_name(text, literal, NameRole::Constant, Some("attributeName")).as_deref(),
            Some("TOTAL_ITEMS"),
            "the text of the literal comes before the parameter it fills"
        );
    }

    #[test]
    fn a_name_the_scope_already_has_gets_a_number() {
        let field = "class A {\n    static final int PAGE_SIZE = 3;\n    void f() {\n        int pageSize = 5;\n    }\n}\n";
        assert_eq!(
            suggested_name(field, at(field, "5"), NameRole::Constant, None).as_deref(),
            Some("PAGE_SIZE1")
        );
        let local =
            "class A {\n    void f() {\n        int limit = 3;\n        show(5);\n    }\n}\n";
        assert_eq!(
            suggested_name(local, at(local, "5"), NameRole::Parameter, Some("limit")).as_deref(),
            Some("limit1")
        );
        let parameter = "class A {\n    void f(int limit) {\n        show(5);\n    }\n}\n";
        assert_eq!(
            suggested_name(
                parameter,
                at(parameter, "5"),
                NameRole::Parameter,
                Some("limit")
            )
            .as_deref(),
            Some("limit1"),
            "a parameter of the method is in the scope too"
        );
    }

    #[test]
    fn an_expression_without_a_context_has_no_name() {
        let text =
            "class A {\n    void f(boolean flag) {\n        if (flag) {\n        }\n    }\n}\n";
        let start = text.find("(flag)").expect("the condition is in the text") + 1;
        assert_eq!(
            suggested_name(text, start..start + "flag".len(), NameRole::Variable, None),
            None
        );
    }

    #[test]
    fn a_range_that_is_not_one_expression_has_no_name() {
        let text = "class A {\n    void f() {\n        int pageSize = 5;\n    }\n}\n";
        assert_eq!(
            suggested_name(text, at(text, "pageSize = 5"), NameRole::Variable, None),
            None
        );
    }

    #[test]
    fn a_keyword_is_no_name() {
        let text = "class A {\n    void f() {\n        show(\"class\");\n    }\n}\n";
        assert_eq!(
            suggested_name(text, at(text, "\"class\""), NameRole::Variable, None),
            None
        );
    }
}

#[cfg(test)]
mod copy_tests {
    use super::{rename_word, repackage};

    #[test]
    fn only_the_import_lines_of_java_count_as_imports() {
        assert!(super::is_import_line("import java.util.List;"));
        assert!(super::is_import_line(
            "  import static java.util.Objects.requireNonNull;"
        ));
        assert!(!super::is_import_line("imported java.util.List;"));
        assert!(!super::is_import_line("class Import {}"));
    }

    #[test]
    fn a_copy_renames_the_class_its_constructors_and_its_own_references() {
        let text = "class Owner {
    Owner() {}
    static Owner of() { return new Owner(); }
    OwnerRepository repository;
}
";
        assert_eq!(
            rename_word(text, "Owner", "OwnerCopy"),
            "class OwnerCopy {
    OwnerCopy() {}
    static OwnerCopy of() { return new OwnerCopy(); }
    OwnerRepository repository;
}
"
        );
    }

    #[test]
    fn a_copy_gets_the_package_of_its_new_folder() {
        let text = "package com.shop.old;\n\nclass Owner {\n}\n";
        assert_eq!(
            repackage(text, "com.shop.new"),
            "package com.shop.new;\n\nclass Owner {\n}\n"
        );
        assert_eq!(
            repackage(text, ""),
            "class Owner {\n}\n",
            "the source root has no package line"
        );
        assert_eq!(
            repackage("class Owner {\n}\n", "com.shop.new"),
            "package com.shop.new;\n\nclass Owner {\n}\n"
        );
        assert_eq!(repackage("class Owner {\n}\n", ""), "class Owner {\n}\n");
    }
}
