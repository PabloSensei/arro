use std::path::{Path, PathBuf};

use lang_api::{
    ActionProvider, AddOverridableMethodsProvider, CallHierarchyProvider,
    ChangeSignatureInfoProvider, ChangeSignatureProvider, CompletionProvider,
    CompletionResolveProvider, DefinitionPreviewProvider, Deletion, DeletionKind,
    DiagnosticsProvider, ExecuteCommandProvider, FileSymbolsProvider, FormatProvider,
    GenerateApplyProvider, GenerateProvider, HierarchyProvider, HighlightProvider, HoverProvider,
    InlayHintProvider, InlayHintSettings, LanguageId, LanguageSupport, MethodHierarchyProvider,
    MoveDestinationProvider, MoveProvider, NavigationProvider, OverridableMethodsProvider,
    PrepareDiagnosticsProvider, ProjectProblemsProvider, Queries, RangeFormatProvider,
    ReferenceStringProvider, ReferencesProvider, RenameProvider, SemanticServer,
    SemanticTokensProvider, SignatureProvider, SourceTextProvider, SymbolProvider, SyntaxProvider,
    UnwrapEdit, WorkspaceDiagnosticsProvider, WorkspaceSymbolProvider,
};
use lang_api::{ExpectedTypeProvider, ExternalDocProvider, SignatureInfoProvider};
use tree_sitter_language::LanguageFn;

use crate::JavaProject;
use crate::problems;

pub const LANGUAGE_ID: LanguageId = LanguageId("java");

/// The grammar and the `queries/java` files the editor uses.
pub struct JavaSyntax {
    queries: Queries,
}

impl JavaSyntax {
    pub(crate) fn new() -> Self {
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
}

impl lang_api::DocumentationCommentProvider for JavaProject {
    fn documentation_comments(&self, text: &str) -> Vec<lang_api::DocumentationComment> {
        crate::documentation_comments(text)
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

    fn symbols(&self) -> Option<&dyn SymbolProvider> {
        Some(java_syntax::java_symbols())
    }

    fn server(&self) -> Option<&dyn SemanticServer> {
        Some(&self.server)
    }

    fn completion(&self) -> Option<&CompletionProvider> {
        Some(self.providers.as_ref())
    }

    fn completion_resolve(&self) -> Option<&CompletionResolveProvider> {
        Some(self.providers.as_ref())
    }

    fn execute_command(&self) -> Option<&ExecuteCommandProvider> {
        Some(self.providers.as_ref())
    }

    fn navigation(&self) -> Option<&NavigationProvider> {
        Some(self.providers.as_ref())
    }

    fn hover(&self) -> Option<&HoverProvider> {
        Some(self.providers.as_ref())
    }

    fn workspace_symbols(&self) -> Option<&WorkspaceSymbolProvider> {
        Some(self.providers.as_ref())
    }

    fn diagnostics(&self) -> Option<&DiagnosticsProvider> {
        Some(self.providers.as_ref())
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

    fn set_code_vision_settings(&self, settings: lang_api::CodeVisionSettings) {
        self.apply_code_vision_settings(settings);
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

    fn format(&self) -> Option<&FormatProvider> {
        Some(self.providers.as_ref())
    }

    fn actions(&self) -> Option<&ActionProvider> {
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

    fn move_destinations(&self) -> Option<&MoveDestinationProvider> {
        Some(self.providers.as_ref())
    }

    fn move_file(&self) -> Option<&MoveProvider> {
        Some(self.providers.as_ref())
    }

    fn change_signature_info(&self) -> Option<&ChangeSignatureInfoProvider> {
        Some(self.providers.as_ref())
    }

    fn change_signature(&self) -> Option<&ChangeSignatureProvider> {
        Some(self.providers.as_ref())
    }

    fn hierarchy(&self) -> Option<&HierarchyProvider> {
        Some(self.providers.as_ref())
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

    fn method_hierarchy(&self) -> Option<&MethodHierarchyProvider> {
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
