//! `lang-api` capabilities of a language that lives behind an LSP server.
//!
//! A server is the authority on what it answers, so an answer is `Certain`. When the
//! server fails to answer, the empty value comes as `Unknown`.

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use base::Cancel;
use lang_api::{
    ActionEntry, ActionKind, ActionQuery, AddOverridableMethodsRequest, Answer, CallDirection,
    CallHierarchyItem, CallHierarchyQuery, CallParameter, CallSignature, ChangeSignatureInfo,
    ChangeSignatureOutcome, ChangeSignatureRequest, CodeLensCommand, CodeLensEntry, CodeLensQuery,
    CodeLensResolveQuery, CommandQuery, CompletionCommand, CompletionEntry, CompletionKind,
    CompletionLabelDetails, CompletionList, Confidence, DiagnosticEntry, DiagnosticNote,
    DiagnosticsQuery, DocumentEdit, DocumentSpot, ExpectedType, ExternalDoc, FileDiagnostics,
    FileRename, FormatQuery, GenerateApplyRequest, GenerateChoice, GenerateKind, GenerateQuery,
    Generation, HierarchyDirection, HierarchyQuery, HoverText, InlayHintEntry, InlayHintKind,
    InlayHintQuery, MethodSignature, MoveDestination, MoveKind, MoveOutcome, MoveQuery,
    MoveRequest, NavigationKind, NavigationQuery, OverridableMethod, OverridableMethods,
    OverridableMethodsQuery, Provider, RangeFormatQuery, RenameQuery, Reply, SemanticServer,
    SemanticToken, SemanticTokenKind, SemanticTokenModifiers, SemanticTokensQuery, Severity,
    SignatureException, SignatureHelp, SignatureParameter, SignatureQuery, SignatureText,
    SourceEdit, SourceId, SourceSpan, SymbolEntry, SymbolQuery, TextPoint, TextSpan, UsageEntry,
    UsageKind, WorkspaceChange, WorkspaceDiagnosticsQuery, offset_in,
};
use lsp_client::SemanticTokensCapability;
use lsp_client::lsp_types::{
    self as lsp, CallHierarchyItem as LspCallHierarchyItem, CodeLens, CompletionResponse,
    DiagnosticSeverity, DiagnosticTag, DocumentHighlightKind, DocumentSymbol,
    DocumentSymbolResponse, GotoDefinitionResponse, Hover, HoverContents, MarkedString,
    NumberOrString, Position, Range, TextDocumentContentChangeEvent, Uri, WorkspaceSymbolResponse,
};
use tokio::runtime::Handle;
use tokio::sync::watch;
use tokio::time::sleep;

use crate::{Place, Published, RouteError, Router};

const CANCEL_POLL: Duration = Duration::from_millis(25);

/// A source action the server offers only through its prompt: the client asks whether
/// the source already exists and which fields it may include, then asks for the edits.
#[derive(Clone, Copy)]
pub struct PromptSource {
    /// The kind of the action, as the server names it.
    pub kind: &'static str,
    /// The row of the pick list.
    pub title: &'static str,
    /// The request that answers whether the source exists and its fields.
    pub check: &'static str,
    /// The request that answers the edits for the chosen fields.
    pub generate: &'static str,
}

/// The requests behind the Source Generate actions of a server.
#[derive(Clone, Copy)]
pub struct GenerateRequests {
    pub constructor_check: &'static str,
    pub constructor_generate: &'static str,
    pub accessor_check: &'static str,
    pub accessor_generate: &'static str,
    pub equals_check: &'static str,
    pub equals_generate: &'static str,
    pub tostring_check: &'static str,
    pub tostring_generate: &'static str,
    pub delegate_check: &'static str,
    pub delegate_generate: &'static str,
}

/// All capabilities of one server. Queries run on `runtime`, `start` returns at once.
pub struct LspProviders {
    router: Arc<Router>,
    published: Arc<Published>,
    revisions: watch::Receiver<u64>,
    runtime: Handle,
    language: &'static str,
    source: SourceId,
    edit_command: Option<&'static str>,
    super_request: Option<&'static str>,
    refactor_action: Option<&'static str>,
    refactor_edit_request: Option<&'static str>,
    move_destinations_request: Option<&'static str>,
    move_request: Option<&'static str>,
    change_signature_info_request: Option<&'static str>,
    change_signature_edit_request: Option<&'static str>,
    change_signature_command: Option<&'static str>,
    override_list_request: Option<&'static str>,
    override_add_request: Option<&'static str>,
    prompt_sources: Vec<PromptSource>,
    generate: Option<GenerateRequests>,
    import_line: fn(&str) -> bool,
    /// The legend the server declared at `initialize`, when it has semantic tokens.
    semantic_tokens: Option<lsp_client::SemanticTokensSetup>,
    /// What the server declared about its code lenses at `initialize`.
    code_lens: Option<lsp_client::CodeLensSetup>,
}

fn point(position: Position) -> TextPoint {
    TextPoint {
        line: position.line,
        character: position.character,
    }
}

fn span(range: Range) -> TextSpan {
    TextSpan {
        start: point(range.start),
        end: point(range.end),
    }
}

fn position(point: TextPoint) -> Position {
    Position::new(point.line, point.character)
}

fn range(span: TextSpan) -> Range {
    Range::new(position(span.start), position(span.end))
}

fn place(spot: &DocumentSpot) -> Option<Place> {
    let uri = Uri::from_str(&spot.uri).ok()?;
    Some(Place {
        uri,
        version: spot.version,
        position: position(spot.point),
    })
}

fn source_span(location: lsp::Location) -> SourceSpan {
    SourceSpan {
        uri: location.uri.as_str().to_owned(),
        span: span(location.range),
    }
}

fn locations(response: Option<GotoDefinitionResponse>) -> Vec<SourceSpan> {
    match response {
        None => Vec::new(),
        Some(GotoDefinitionResponse::Scalar(location)) => vec![source_span(location)],
        Some(GotoDefinitionResponse::Array(locations)) => {
            locations.into_iter().map(source_span).collect()
        }
        Some(GotoDefinitionResponse::Link(links)) => links
            .into_iter()
            .map(|link| SourceSpan {
                uri: link.target_uri.as_str().to_owned(),
                span: span(link.target_selection_range),
            })
            .collect(),
    }
}

fn completion_entry(item: lsp::CompletionItem) -> CompletionEntry {
    // The whole item goes back to the server for the parts it sends only on resolve.
    let resolve = serde_json::to_string(&item).ok();
    let edit = item.text_edit.map(|edit| match edit {
        lsp::CompletionTextEdit::Edit(edit) => (edit.new_text, edit.range),
        lsp::CompletionTextEdit::InsertAndReplace(edit) => (edit.new_text, edit.replace),
    });
    let (edit_text, edit_span) = edit.map(|(text, range)| (text, span(range))).unzip();
    let additional_edits = item
        .additional_text_edits
        .unwrap_or_default()
        .into_iter()
        .map(document_edit)
        .collect();
    let command = item.command.map(|command| CompletionCommand {
        title: command.title,
        command: command.command,
        arguments: serde_json::to_string(&command.arguments.unwrap_or_default())
            .unwrap_or_else(|_| "[]".to_owned()),
    });
    CompletionEntry {
        label: item.label,
        label_details: item.label_details.map(|details| CompletionLabelDetails {
            detail: details.detail,
            description: details.description,
        }),
        detail: item.detail,
        filter_text: item.filter_text,
        sort_text: item.sort_text,
        insert_text: edit_text.or(item.insert_text),
        span: edit_span,
        snippet: item.insert_text_format == Some(lsp::InsertTextFormat::SNIPPET),
        additional_edits,
        command,
        resolve,
        kind: completion_kind(item.kind),
    }
}

fn completion_kind(kind: Option<lsp::CompletionItemKind>) -> CompletionKind {
    use lsp::CompletionItemKind as Kind;
    match kind {
        Some(Kind::TEXT) => CompletionKind::Text,
        Some(Kind::METHOD) => CompletionKind::Method,
        Some(Kind::FUNCTION) => CompletionKind::Function,
        Some(Kind::CONSTRUCTOR) => CompletionKind::Constructor,
        Some(Kind::FIELD) => CompletionKind::Field,
        Some(Kind::VARIABLE) => CompletionKind::Variable,
        Some(Kind::CLASS) => CompletionKind::Class,
        Some(Kind::INTERFACE) => CompletionKind::Interface,
        Some(Kind::MODULE) => CompletionKind::Module,
        Some(Kind::PROPERTY) => CompletionKind::Property,
        Some(Kind::UNIT) => CompletionKind::Unit,
        Some(Kind::VALUE) => CompletionKind::Value,
        Some(Kind::ENUM) => CompletionKind::Enum,
        Some(Kind::KEYWORD) => CompletionKind::Keyword,
        Some(Kind::SNIPPET) => CompletionKind::Snippet,
        Some(Kind::COLOR) => CompletionKind::Color,
        Some(Kind::FILE) => CompletionKind::File,
        Some(Kind::REFERENCE) => CompletionKind::Reference,
        Some(Kind::FOLDER) => CompletionKind::Folder,
        Some(Kind::ENUM_MEMBER) => CompletionKind::Constant,
        Some(Kind::CONSTANT) => CompletionKind::Constant,
        Some(Kind::STRUCT) => CompletionKind::Struct,
        Some(Kind::EVENT) => CompletionKind::Event,
        Some(Kind::OPERATOR) => CompletionKind::Operator,
        Some(Kind::TYPE_PARAMETER) => CompletionKind::TypeParameter,
        _ => CompletionKind::Unknown,
    }
}

fn document_edit(edit: lsp::TextEdit) -> DocumentEdit {
    DocumentEdit {
        span: span(edit.range),
        text: edit.new_text,
    }
}

/// Text edits and the files that move. Creating and deleting files is not supported.
pub fn workspace_change(edit: lsp::WorkspaceEdit) -> WorkspaceChange {
    let mut sources = Vec::new();
    let mut renames = Vec::new();
    for (uri, edits) in edit.changes.into_iter().flatten() {
        let edits = edits.into_iter().map(document_edit).collect();
        sources.push(SourceEdit {
            uri: uri.as_str().to_owned(),
            edits,
        });
    }
    let documents = match edit.document_changes {
        None => Vec::new(),
        Some(lsp::DocumentChanges::Edits(edits)) => edits,
        Some(lsp::DocumentChanges::Operations(operations)) => operations
            .into_iter()
            .filter_map(|operation| match operation {
                lsp::DocumentChangeOperation::Edit(edit) => Some(edit),
                lsp::DocumentChangeOperation::Op(lsp::ResourceOp::Rename(rename)) => {
                    renames.push(FileRename {
                        from: rename.old_uri.as_str().to_owned(),
                        to: rename.new_uri.as_str().to_owned(),
                    });
                    None
                }
                lsp::DocumentChangeOperation::Op(_) => None,
            })
            .collect(),
    };
    for document in documents {
        let edits = document
            .edits
            .into_iter()
            .map(|edit| match edit {
                lsp::OneOf::Left(edit) => document_edit(edit),
                lsp::OneOf::Right(annotated) => document_edit(annotated.text_edit),
            })
            .collect();
        sources.push(SourceEdit {
            uri: document.text_document.uri.as_str().to_owned(),
            edits,
        });
    }
    WorkspaceChange {
        edits: sources,
        renames,
    }
}

fn action_kind(kind: &lsp::CodeActionKind) -> Option<ActionKind> {
    let kind = kind.as_str();
    if kind.starts_with("source.organizeImports") {
        Some(ActionKind::OrganizeImports)
    } else if kind.starts_with("source.fixAll") {
        Some(ActionKind::FixAll)
    } else if kind.starts_with("quickfix") {
        Some(ActionKind::QuickFix)
    } else if kind.starts_with("refactor") {
        Some(ActionKind::Refactor)
    } else if kind.starts_with("source") {
        Some(ActionKind::Source)
    } else {
        None
    }
}

fn lsp_kind(kind: ActionKind) -> lsp::CodeActionKind {
    match kind {
        ActionKind::QuickFix => lsp::CodeActionKind::QUICKFIX,
        ActionKind::Refactor => lsp::CodeActionKind::REFACTOR,
        ActionKind::Source => lsp::CodeActionKind::SOURCE,
        ActionKind::OrganizeImports => lsp::CodeActionKind::SOURCE_ORGANIZE_IMPORTS,
        ActionKind::FixAll => lsp::CodeActionKind::SOURCE_FIX_ALL,
    }
}

/// An action the editor can apply by itself, with its edits; `None` for the rest.
/// `edit_command` is the command of the server whose one argument is a workspace edit.
fn action_entry(
    action: lsp::CodeActionOrCommand,
    edit_command: Option<&str>,
) -> Option<ActionEntry> {
    let (title, code, preferred, edit, command) = match action {
        lsp::CodeActionOrCommand::Command(command) => {
            (command.title.clone(), None, false, None, Some(command))
        }
        lsp::CodeActionOrCommand::CodeAction(action) => (
            action.title,
            action.kind,
            action.is_preferred.unwrap_or(false),
            action.edit,
            action.command,
        ),
    };
    let kind = code.as_ref().and_then(action_kind);
    let change_signature = code
        .as_ref()
        .is_some_and(|code| code.as_str().starts_with("refactor.change.signature"));
    if edit.is_none() && change_signature {
        // The window asks for this form separately, and the server computes its edits
        // only after the form is filled in.
        return Some(ActionEntry {
            title,
            kind,
            code: code.map(|code| code.as_str().to_owned()),
            preferred,
            edits: Vec::new(),
            renames: Vec::new(),
        });
    }
    let edit = match (edit, command) {
        (Some(edit), _) => edit,
        (None, Some(command)) if Some(command.command.as_str()) == edit_command => {
            let argument = command.arguments?.into_iter().next()?;
            serde_json::from_value(argument).ok()?
        }
        _ => return None,
    };
    let change = workspace_change(edit);
    Some(ActionEntry {
        title,
        kind,
        code: code.map(|code| code.as_str().to_owned()),
        preferred,
        edits: change.edits,
        renames: change.renames,
    })
}

/// Refactorings the server offers only as a command carry their edits behind another
/// request: asking for them turns an action with no edit into one the editor applies
/// like any other.
async fn resolve_action(
    router: &Router,
    action: lsp::CodeActionOrCommand,
    refactor_action: Option<&str>,
    edit_request: Option<&str>,
) -> lsp::CodeActionOrCommand {
    let lsp::CodeActionOrCommand::CodeAction(mut action) = action else {
        return action;
    };
    if action.edit.is_some() {
        return lsp::CodeActionOrCommand::CodeAction(action);
    }
    let (Some(command), Some(refactor_action), Some(edit_request)) =
        (&action.command, refactor_action, edit_request)
    else {
        return lsp::CodeActionOrCommand::CodeAction(action);
    };
    if command.command != refactor_action {
        return lsp::CodeActionOrCommand::CodeAction(action);
    }
    let arguments = command.arguments.clone().unwrap_or_default();
    let Some(name) = arguments.first().and_then(|name| name.as_str()) else {
        return lsp::CodeActionOrCommand::CodeAction(action);
    };
    let Some(context) = arguments.get(1).cloned() else {
        return lsp::CodeActionOrCommand::CodeAction(action);
    };
    // Only the introduce parameter command is resolved here: the extract ones come
    // with their edits already, and move needs a destination from F6.
    if name != "introduceParameter" {
        return lsp::CodeActionOrCommand::CodeAction(action);
    }
    let params = serde_json::json!({
        "command": name,
        "commandArguments": [],
        "context": context,
        "options": {"tabSize": 4, "insertSpaces": true},
    });
    if let Ok(reply) = router.extension(edit_request, params).await
        && let Some(edit) = reply.get("edit")
        && let Ok(edit) = serde_json::from_value::<lsp::WorkspaceEdit>(edit.clone())
    {
        action.edit = Some(edit);
    }
    lsp::CodeActionOrCommand::CodeAction(action)
}

async fn action_entries(
    router: &Router,
    response: Option<lsp::CodeActionResponse>,
    edit_command: Option<&str>,
    refactor_action: Option<&str>,
    refactor_edit_request: Option<&str>,
) -> Vec<ActionEntry> {
    let mut entries = Vec::new();
    for action in response.into_iter().flatten() {
        let action = resolve_action(router, action, refactor_action, refactor_edit_request).await;
        entries.extend(action_entry(action, edit_command));
    }
    entries
}

/// The params a prompt request takes: the code action context of the place.
fn prompt_context(uri: &Uri, range: Range) -> serde_json::Value {
    serde_json::json!({
        "textDocument": {"uri": uri.as_str()},
        "range": {
            "start": {"line": range.start.line, "character": range.start.character},
            "end": {"line": range.end.line, "character": range.end.character},
        },
        "context": {"diagnostics": []},
    })
}

/// The code action context that the requests of a change of signature take.
fn change_context(uri: &Uri, spot: &DocumentSpot) -> serde_json::Value {
    let at = position(spot.point);
    serde_json::json!({
        "textDocument": {"uri": uri.as_str()},
        "range": {"start": at, "end": at},
        "context": {"diagnostics": []},
    })
}

/// The signature a server describes for a place.
fn change_signature_info(value: &serde_json::Value) -> ChangeSignatureInfo {
    if let Some(error) = value
        .get("errorMessage")
        .and_then(serde_json::Value::as_str)
        .filter(|error| !error.is_empty())
    {
        return ChangeSignatureInfo {
            signature: None,
            error: Some(error.to_owned()),
        };
    }
    let text = |key: &str| {
        value
            .get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let Some(name) = value
        .get("methodName")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
    else {
        return ChangeSignatureInfo {
            signature: None,
            error: Some("Cannot change the signature here".to_owned()),
        };
    };
    let parameters = value
        .get("parameters")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
        .map(|(index, parameter)| {
            let field = |key: &str| {
                parameter
                    .get(key)
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_owned()
            };
            let original_index = parameter
                .get("originalIndex")
                .and_then(serde_json::Value::as_i64)
                .filter(|index| *index >= 0)
                .map_or(Some(index), |index| Some(index as usize));
            SignatureParameter {
                type_name: field("type"),
                name: field("name"),
                default_value: field("defaultValue"),
                original_index,
            }
        })
        .collect();
    let exceptions = value
        .get("exceptions")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .map(|exception| SignatureException {
            type_name: exception
                .get("type")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            handle: exception
                .get("typeHandleIdentifier")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        })
        .collect();
    ChangeSignatureInfo {
        signature: Some(MethodSignature {
            name,
            visibility: text("modifier"),
            return_type: text("returnType"),
            parameters,
            exceptions,
        }),
        error: None,
    }
}

/// The action of a prompt-only source, or `None` when the source exists already.
async fn prompt_entry(
    router: &Router,
    uri: &Uri,
    range: Range,
    source: PromptSource,
) -> Option<ActionEntry> {
    let context = prompt_context(uri, range);
    let status = router.extension(source.check, context.clone()).await.ok()?;
    if status.get("exists").and_then(|exists| exists.as_bool()) == Some(true) {
        return None;
    }
    let fields = status
        .get("fields")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let params = serde_json::json!({"context": context, "fields": fields});
    let edit = router.extension(source.generate, params).await.ok()?;
    let edit: lsp::WorkspaceEdit = serde_json::from_value(edit).ok()?;
    let change = workspace_change(edit);
    Some(ActionEntry {
        title: source.title.to_owned(),
        kind: Some(ActionKind::Source),
        code: Some(source.kind.to_owned()),
        preferred: false,
        edits: change.edits,
        renames: change.renames,
    })
}

fn highlight_spans(highlights: Option<Vec<lsp::DocumentHighlight>>) -> Vec<TextSpan> {
    highlights
        .into_iter()
        .flatten()
        .map(|highlight| span(highlight.range))
        .collect()
}

fn completion_list(response: Option<CompletionResponse>) -> CompletionList {
    let (items, complete) = match response {
        None => (Vec::new(), true),
        Some(CompletionResponse::Array(items)) => (items, true),
        Some(CompletionResponse::List(list)) => (list.items, !list.is_incomplete),
    };
    let entries = items.into_iter().map(completion_entry).collect();
    CompletionList { entries, complete }
}

fn marked(text: MarkedString) -> String {
    match text {
        MarkedString::String(text) => text,
        MarkedString::LanguageString(code) => {
            format!("```{}\n{}\n```", code.language, code.value)
        }
    }
}

fn hover_text(hover: Hover) -> HoverText {
    HoverText(match hover.contents {
        HoverContents::Scalar(text) => marked(text),
        HoverContents::Array(parts) => parts
            .into_iter()
            .map(marked)
            .collect::<Vec<_>>()
            .join("\n\n"),
        HoverContents::Markup(markup) => markup.value,
    })
}

fn symbols(response: Option<WorkspaceSymbolResponse>) -> Vec<SymbolEntry> {
    match response {
        None => Vec::new(),
        Some(WorkspaceSymbolResponse::Flat(symbols)) => symbols
            .into_iter()
            .map(|symbol| SymbolEntry {
                name: symbol.name,
                container: symbol.container_name,
                place: Some(source_span(symbol.location)),
            })
            .collect(),
        Some(WorkspaceSymbolResponse::Nested(symbols)) => symbols
            .into_iter()
            .map(|symbol| SymbolEntry {
                name: symbol.name,
                container: symbol.container_name,
                place: match symbol.location {
                    lsp::OneOf::Left(location) => Some(source_span(location)),
                    lsp::OneOf::Right(_) => None,
                },
            })
            .collect(),
    }
}

fn signature_text(help: Option<lsp::SignatureHelp>) -> SignatureText {
    let Some(help) = help else {
        return SignatureText::default();
    };
    let active = help.active_signature.unwrap_or(0) as usize;
    let mut labels: Vec<String> = help
        .signatures
        .into_iter()
        .map(|signature| signature.label)
        .collect();
    if active < labels.len() {
        let first = labels.remove(active);
        labels.insert(0, first);
    }
    SignatureText(labels.join(
        "
",
    ))
}

/// A part of a label given as UTF-16 offsets, the units LSP counts in.
fn slice_utf16(text: &str, start: u32, end: u32) -> String {
    let mut units = 0u32;
    let mut from = None;
    for (at, character) in text.char_indices() {
        if units >= start && from.is_none() {
            from = Some(at);
        }
        units += character.len_utf16() as u32;
        if units >= end {
            return text[from.unwrap_or(at)..at + character.len_utf8()].to_owned();
        }
    }
    text[from.unwrap_or(text.len())..].to_owned()
}

fn parameter_text(
    signature: &lsp::SignatureInformation,
    parameter: &lsp::ParameterInformation,
) -> String {
    match &parameter.label {
        lsp::ParameterLabel::Simple(text) => text.clone(),
        lsp::ParameterLabel::LabelOffsets([start, end]) => {
            slice_utf16(&signature.label, *start, *end)
        }
    }
}

/// The parameters inside the parentheses of a signature label, comma separated at the
/// outermost level. Used when the server gives no parameter labels.
fn parameters_in_label(label: &str) -> Vec<CallParameter> {
    let Some(open) = label.find('(') else {
        return Vec::new();
    };
    let mut depth = 0i32;
    let mut close = None;
    for (at, character) in label[open..].char_indices() {
        match character {
            '(' | '<' | '[' => depth += 1,
            ')' | '>' | ']' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(open + at);
                    break;
                }
            }
            _ => {}
        }
    }
    let Some(close) = close else {
        return Vec::new();
    };
    let inside = &label[open + 1..close];
    if inside.trim().is_empty() {
        return Vec::new();
    }
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    for (at, character) in inside.char_indices() {
        match character {
            '(' | '<' | '[' => depth += 1,
            ')' | '>' | ']' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(CallParameter {
                    label: inside[start..at].trim().to_owned(),
                });
                start = at + 1;
                continue;
            }
            _ => {}
        }
    }
    parts.push(CallParameter {
        label: inside[start..].trim().to_owned(),
    });
    parts
}

fn signature_help_info(help: lsp::SignatureHelp) -> SignatureHelp {
    let active_signature = help
        .active_signature
        .map_or(0, |active| active as usize)
        .min(help.signatures.len().saturating_sub(1));
    let mut parameters = Vec::with_capacity(help.signatures.len());
    let mut per_signature = Vec::with_capacity(help.signatures.len());
    for signature in &help.signatures {
        let labels: Vec<CallParameter> = signature
            .parameters
            .as_deref()
            .unwrap_or_default()
            .iter()
            .map(|parameter| CallParameter {
                label: parameter_text(signature, parameter),
            })
            .filter(|parameter| !parameter.label.is_empty())
            .collect();
        parameters.push(if labels.is_empty() {
            parameters_in_label(&signature.label)
        } else {
            labels
        });
        per_signature.push(signature.active_parameter.map(|index| index as usize));
    }
    let active_parameter = per_signature
        .get(active_signature)
        .copied()
        .flatten()
        .or_else(|| help.active_parameter.map(|index| index as usize))
        .unwrap_or(0);
    let signatures = help
        .signatures
        .into_iter()
        .zip(parameters)
        .map(|(signature, parameters)| CallSignature {
            label: signature.label,
            parameters,
        })
        .collect();
    SignatureHelp {
        signatures,
        active_signature,
        active_parameter,
    }
}

/// The type of a parameter from the text of the parameter: everything before the name.
fn parameter_type(label: &str) -> Option<String> {
    let label = label.trim();
    if label.is_empty() {
        return None;
    }
    let end = label
        .rfind(|character: char| character.is_whitespace())
        .unwrap_or(label.len());
    let type_part = label[..end].trim();
    if type_part.is_empty() {
        // A label that is only a type, with no name.
        return Some(label.trim_end_matches('.').to_owned());
    }
    Some(strip_modifiers(type_part).trim_end_matches('.').to_owned())
}

const TYPE_MODIFIERS: &[&str] = &[
    "final",
    "static",
    "private",
    "protected",
    "public",
    "volatile",
    "transient",
    "abstract",
    "synchronized",
    "native",
    "strictfp",
];

/// The type without the modifiers in front of it.
fn strip_modifiers(text: &str) -> &str {
    let mut rest = text.trim();
    loop {
        let Some((word, tail)) = rest.split_once(char::is_whitespace) else {
            return rest;
        };
        if TYPE_MODIFIERS.contains(&word) {
            rest = tail.trim_start();
        } else {
            return rest;
        }
    }
}

/// The type before `parameter` inside a signature label, for a parameter label that is
/// only the name of the parameter.
fn parameter_type_in_label(signature: &str, parameter: &str) -> Option<String> {
    let at = signature.find(parameter)?;
    let before = &signature[..at];
    let start = before.rfind([',', '(']).map_or(0, |offset| offset + 1);
    let segment = before[start..].trim();
    let token = last_type_token(segment)?;
    let token = strip_modifiers(token).trim_end_matches('.');
    (!token.is_empty()).then(|| token.to_owned())
}

fn expected_type_from_signature(help: &lsp::SignatureHelp) -> Option<String> {
    let active = help
        .active_signature
        .map_or(0, |index| index as usize)
        .min(help.signatures.len().saturating_sub(1));
    let signature = help.signatures.get(active)?;
    let parameter = signature.active_parameter.map_or_else(
        || help.active_parameter.map_or(0, |index| index as usize),
        |index| index as usize,
    );
    let parameters = signature
        .parameters
        .as_deref()
        .filter(|parameters| !parameters.is_empty())
        .map(|parameters| {
            parameters
                .iter()
                .map(|parameter| parameter_text(signature, parameter))
                .collect::<Vec<_>>()
        })
        .unwrap_or_else(|| {
            parameters_in_label(&signature.label)
                .into_iter()
                .map(|parameter| parameter.label)
                .collect()
        });
    let text = parameters.get(parameter)?;
    if text.split_whitespace().count() > 1 {
        parameter_type(text)
    } else {
        // A label of only the name: the type is in the signature before it.
        parameter_type_in_label(&signature.label, text).or_else(|| parameter_type(text))
    }
}

/// The type of the declaration to the left of the caret: `Type name =` or `return`.
fn expected_type_from_declaration(text: &str, offset: usize) -> Option<String> {
    let before = &text[..offset.min(text.len())];
    let line_start = before.rfind('\n').map_or(0, |at| at + 1);
    let line = &before[line_start..];
    let trimmed = line.trim_start();
    if let Some(rest) = trimmed.strip_prefix("return")
        && rest.trim().is_empty()
    {
        return return_type_before(text, line_start);
    }
    let equals = line.rfind('=')?;
    let left = &line[..equals];
    let right = &line[equals + 1..];
    if right.contains(';')
        || left
            .trim_end()
            .ends_with(['=', '!', '<', '>', '+', '-', '*', '/', '%', '&', '|', '^'])
    {
        return None;
    }
    let left = left.trim_end();
    let split = left.rfind(char::is_whitespace)?;
    let type_part = left[..split].trim();
    if type_part.is_empty()
        || type_part.contains('(')
        || type_part.contains(')')
        || type_part.contains(';')
        || type_part.contains('=')
        || !type_part.starts_with(|character: char| character.is_alphabetic() || character == '_')
    {
        return None;
    }
    let type_part = strip_modifiers(type_part);
    (!type_part.is_empty()).then(|| type_part.to_owned())
}

/// The last type of a declaration head: `public Map<String, Integer>` gives
/// `Map<String, Integer>` and `public String` gives `String`.
fn last_type_token(text: &str) -> Option<&str> {
    let mut depth = 0i32;
    let mut start = 0;
    for (at, character) in text.char_indices().rev() {
        match character {
            '>' | ')' | ']' => depth += 1,
            '<' | '(' | '[' => depth -= 1,
            character if character.is_whitespace() && depth == 0 => {
                start = at + character.len_utf8();
                break;
            }
            _ => {}
        }
    }
    let token = text[start..].trim();
    (!token.is_empty()).then_some(token)
}

/// The return type of the method whose body holds `at`.
fn return_type_before(text: &str, at: usize) -> Option<String> {
    let before = &text[..at.min(text.len())];
    let brace = before.rfind('{')?;
    let close = before[..brace].rfind(')')?;
    let mut depth = 0i32;
    let mut open = None;
    for (offset, character) in before[..close].char_indices().rev() {
        match character {
            ')' => depth += 1,
            '(' => {
                depth -= 1;
                if depth <= 0 {
                    open = Some(offset);
                    break;
                }
            }
            _ => {}
        }
    }
    let open = open?;
    let head = before[..open].trim_end();
    let name_start = head
        .rfind(|character: char| character.is_whitespace() || character == '>')
        .map_or(0, |at| at + 1);
    let before_name = head[..name_start].trim_end();
    let token = last_type_token(before_name)?;
    (!token.is_empty()).then(|| token.to_owned())
}

/// The first web address in a documentation text, from a Markdown link or bare.
fn documentation_url(text: &str) -> Option<String> {
    if let Some(start) = text.find("](") {
        let rest = &text[start + 2..];
        if let Some(end) = rest.find(')') {
            let url = rest[..end].trim();
            if url.starts_with("http://") || url.starts_with("https://") {
                return Some(url.to_owned());
            }
        }
    }
    for token in text.split(|character: char| {
        character.is_whitespace() || matches!(character, '<' | '>' | '"' | '(' | ')')
    }) {
        if token.starts_with("http://") || token.starts_with("https://") {
            return Some(token.trim_end_matches([',', '.', ';']).to_owned());
        }
    }
    None
}

fn severity(severity: Option<DiagnosticSeverity>) -> Severity {
    match severity {
        Some(DiagnosticSeverity::ERROR) => Severity::Error,
        Some(DiagnosticSeverity::INFORMATION) => Severity::Information,
        Some(DiagnosticSeverity::HINT) => Severity::Hint,
        // The protocol leaves a missing severity to the client.
        _ => Severity::Warning,
    }
}

fn unnecessary(diagnostic: &lsp::Diagnostic) -> bool {
    diagnostic
        .tags
        .as_ref()
        .is_some_and(|tags| tags.contains(&DiagnosticTag::UNNECESSARY))
}

fn diagnostic(diagnostic: lsp::Diagnostic) -> DiagnosticEntry {
    let unnecessary = unnecessary(&diagnostic);
    DiagnosticEntry {
        span: span(diagnostic.range),
        severity: severity(diagnostic.severity),
        message: diagnostic.message,
        unnecessary,
    }
}

fn diagnostic_note(diagnostic: lsp::Diagnostic) -> DiagnosticNote {
    let code = diagnostic.code.map(|code| match code {
        NumberOrString::Number(code) => code.to_string(),
        NumberOrString::String(code) => code,
    });
    DiagnosticNote {
        span: span(diagnostic.range),
        severity: severity(diagnostic.severity),
        message: diagnostic.message,
        code,
    }
}

/// The text of a hint label: one string, or the parts an interactive label is made of.
fn inlay_label(label: lsp::InlayHintLabel) -> String {
    match label {
        lsp::InlayHintLabel::String(text) => text,
        lsp::InlayHintLabel::LabelParts(parts) => {
            parts.into_iter().map(|part| part.value).collect()
        }
    }
}

fn inlay_kind(kind: Option<lsp::InlayHintKind>) -> InlayHintKind {
    match kind {
        Some(kind) if kind == lsp::InlayHintKind::PARAMETER => InlayHintKind::Parameter,
        Some(kind) if kind == lsp::InlayHintKind::TYPE => InlayHintKind::Type,
        _ => InlayHintKind::Other,
    }
}

/// The hints of an answer, without the ones that say nothing. A hint without a kind is
/// an `Other` one: the editor shows it only when it shows every kind.
fn inlay_hints(hints: Option<Vec<lsp::InlayHint>>) -> Vec<InlayHintEntry> {
    hints
        .into_iter()
        .flatten()
        .filter_map(|hint| {
            let label = inlay_label(hint.label);
            if label.trim().is_empty() {
                return None;
            }
            Some(InlayHintEntry {
                position: point(hint.position),
                label,
                kind: inlay_kind(hint.kind),
                padding_left: hint.padding_left.unwrap_or(false),
                padding_right: hint.padding_right.unwrap_or(false),
            })
        })
        .collect()
}

async fn cancelled(cancel: &Cancel) {
    while !cancel.is_cancelled() {
        sleep(CANCEL_POLL).await;
    }
}

/// One label of an answer. A label without a command has no title yet: its kind comes
/// from the command the server resolves later.
fn code_lens_entry(lens: CodeLens) -> CodeLensEntry {
    let title = lens
        .command
        .as_ref()
        .map(|command| command.title.clone())
        .unwrap_or_default();
    let command = lens.command.as_ref().map(|command| CodeLensCommand {
        title: command.title.clone(),
        command: command.command.clone(),
        arguments: command
            .arguments
            .as_ref()
            .and_then(|arguments| serde_json::to_string(arguments).ok()),
    });
    let kind = lang_api::kind_of(
        &title,
        lens.command
            .as_ref()
            .map(|command| command.command.as_str()),
    );
    CodeLensEntry {
        span: span(lens.range),
        title,
        kind,
        resolve: serde_json::to_string(&lens).ok(),
        command,
    }
}

fn code_lens_entries(lenses: Option<Vec<CodeLens>>) -> Vec<CodeLensEntry> {
    lenses.into_iter().flatten().map(code_lens_entry).collect()
}

/// How long a code lens request waits for the server to say what it can do. The answer
/// to `initialize` comes before the first request of a document in practice.
const CODE_LENS_READY_LIMIT: Duration = Duration::from_secs(15);
const CODE_LENS_READY_POLL: Duration = Duration::from_millis(50);

async fn code_lens_capability(
    setup: &lsp_client::CodeLensSetup,
) -> Option<lsp_client::CodeLensCapability> {
    let mut waited = Duration::ZERO;
    loop {
        if let Some(capability) = setup.get() {
            return Some(capability);
        }
        if waited >= CODE_LENS_READY_LIMIT {
            return None;
        }
        sleep(CODE_LENS_READY_POLL).await;
        waited += CODE_LENS_READY_POLL;
    }
}

impl LspProviders {
    pub fn new(
        router: Arc<Router>,
        published: Arc<Published>,
        revisions: watch::Receiver<u64>,
        runtime: Handle,
        language: &'static str,
        source: SourceId,
    ) -> Self {
        Self {
            router,
            published,
            revisions,
            runtime,
            language,
            source,
            edit_command: None,
            super_request: None,
            refactor_action: None,
            refactor_edit_request: None,
            move_destinations_request: None,
            move_request: None,
            change_signature_info_request: None,
            change_signature_edit_request: None,
            change_signature_command: None,
            override_list_request: None,
            override_add_request: None,
            prompt_sources: Vec::new(),
            generate: None,
            import_line: |_| false,
            semantic_tokens: None,
            code_lens: None,
        }
    }

    /// Takes what the server said about its code lenses at `initialize`. Without it the
    /// provider asks for no labels.
    pub fn with_code_lens(mut self, setup: lsp_client::CodeLensSetup) -> Self {
        self.code_lens = Some(setup);
        self
    }

    /// Takes the legend of the server's semantic tokens, which it declares at
    /// `initialize`. Without one the provider asks for no tokens.
    pub fn with_semantic_tokens(mut self, setup: lsp_client::SemanticTokensSetup) -> Self {
        self.semantic_tokens = Some(setup);
        self
    }

    /// Whether the server colours a range of a document, once it has declared it.
    pub fn semantic_tokens_range(&self) -> bool {
        self.semantic_tokens
            .as_ref()
            .and_then(|setup| setup.get())
            .is_some_and(|capability| capability.range)
    }

    /// Names the command whose edits are asked for with a separate request, and that
    /// request: the action carries the arguments, the request turns them into edits.
    pub fn with_refactor_action(mut self, command: &'static str, request: &'static str) -> Self {
        self.refactor_action = Some(command);
        self.refactor_edit_request = Some(request);
        self
    }

    /// Names the requests that list the places a file can move to and that move it.
    pub fn with_move_requests(
        mut self,
        destinations: &'static str,
        move_request: &'static str,
    ) -> Self {
        self.move_destinations_request = Some(destinations);
        self.move_request = Some(move_request);
        self
    }

    /// Names the requests of a change of signature: the one that describes the
    /// signature under a place, the one that turns a new signature into edits, and
    /// the command of the server that edits are asked for.
    pub fn with_change_signature(
        mut self,
        info_request: &'static str,
        edit_request: &'static str,
        command: &'static str,
    ) -> Self {
        self.change_signature_info_request = Some(info_request);
        self.change_signature_edit_request = Some(edit_request);
        self.change_signature_command = Some(command);
        self
    }

    /// Names the requests of Override and Implement: the one that lists the methods of
    /// the type at a place and the one that turns the picked methods into edits.
    pub fn with_override_methods(
        mut self,
        list_request: &'static str,
        add_request: &'static str,
    ) -> Self {
        self.override_list_request = Some(list_request);
        self.override_add_request = Some(add_request);
        self
    }

    /// Names the request of the server that finds the super method: it takes
    /// `{"type": "superImplementation", "position": ...}` and answers locations.
    pub fn with_super_request(mut self, method: &'static str) -> Self {
        self.super_request = Some(method);
        self
    }

    /// Names the command whose one argument is a workspace edit, so actions that come
    /// as that command are applied by the editor like any other.
    pub fn with_edit_command(mut self, command: &'static str) -> Self {
        self.edit_command = Some(command);
        self
    }

    /// Adds a source action that the server answers in two steps instead of listing
    /// it with the other actions.
    pub fn with_prompt_source(mut self, source: PromptSource) -> Self {
        self.prompt_sources.push(source);
        self
    }

    /// Names the requests of the Source Generate actions.
    pub fn with_generate(mut self, requests: GenerateRequests) -> Self {
        self.generate = Some(requests);
        self
    }

    /// Tells which line of a file of this language is an import. A language without
    /// imports keeps the default, which says that no line is one.
    pub fn with_import_line(mut self, is_import: fn(&str) -> bool) -> Self {
        self.import_line = is_import;
        self
    }

    /// Runs `work` and sends its answer. A question about an old text stays silent: the
    /// caller already asks about the new one. A failure, a timeout during indexing for
    /// one, answers an empty value with `Unknown`, so it does not read as "nothing found".
    /// Dropping `work` cancels the request at the server.
    fn answer<T: Default + Send + 'static>(
        &self,
        cancel: Cancel,
        mut reply: Reply<T>,
        work: impl Future<Output = Option<Result<T, RouteError>>> + Send + 'static,
    ) {
        let source = self.source;
        self.runtime.spawn(async move {
            tokio::select! {
                outcome = work => {
                    let answer = match outcome {
                        Some(Ok(value)) => Answer::new(value, Confidence::Certain, source),
                        None | Some(Err(RouteError::Stale | RouteError::Superseded)) => return,
                        Some(Err(_)) => Answer::new(T::default(), Confidence::Unknown, source),
                    };
                    let _ = reply.send(answer);
                }
                () = cancelled(&cancel) => {}
            }
        });
    }
}

impl Provider<DocumentSpot, CompletionList> for LspProviders {
    fn start(&self, spot: DocumentSpot, cancel: Cancel, reply: Reply<CompletionList>) {
        let router = self.router.clone();
        self.answer(cancel, reply, async move {
            Some(router.completion(place(&spot)?).await.map(completion_list))
        });
    }
}

impl Provider<CompletionEntry, CompletionEntry> for LspProviders {
    fn start(&self, entry: CompletionEntry, _: Cancel, mut reply: Reply<CompletionEntry>) {
        let Some(handle) = entry.resolve.clone() else {
            return;
        };
        let router = self.router.clone();
        let source = self.source;
        self.runtime.spawn(async move {
            let Ok(item) = serde_json::from_str::<lsp::CompletionItem>(&handle) else {
                return;
            };
            let Ok(params) = serde_json::to_value(item) else {
                return;
            };
            let Ok(value) = router.extension("completionItem/resolve", params).await else {
                return;
            };
            let Ok(item) = serde_json::from_value::<lsp::CompletionItem>(value) else {
                return;
            };
            let _ = reply.send(Answer::new(
                completion_entry(item),
                Confidence::Certain,
                source,
            ));
        });
    }
}

impl Provider<CommandQuery, ()> for LspProviders {
    fn start(&self, query: CommandQuery, _: Cancel, mut reply: Reply<()>) {
        let router = self.router.clone();
        let source = self.source;
        self.runtime.spawn(async move {
            let arguments: serde_json::Value = serde_json::from_str(&query.arguments)
                .unwrap_or_else(|_| serde_json::Value::Array(Vec::new()));
            let params = serde_json::json!({
                "command": query.command,
                "arguments": arguments,
            });
            let _ = router.extension("workspace/executeCommand", params).await;
            let _ = reply.send(Answer::new((), Confidence::Certain, source));
        });
    }
}

impl Provider<NavigationQuery, Vec<SourceSpan>> for LspProviders {
    fn start(&self, query: NavigationQuery, cancel: Cancel, reply: Reply<Vec<SourceSpan>>) {
        let router = self.router.clone();
        let super_request = self.super_request;
        self.answer(cancel, reply, async move {
            let place = place(&query.spot)?;
            if query.kind == NavigationKind::SuperMethod {
                let Some(method) = super_request else {
                    return Some(Ok(Vec::new()));
                };
                let params = serde_json::json!({
                    "type": "superImplementation",
                    "position": place.text_document_position(),
                });
                let found = router.extension(method, params).await;
                return Some(found.map(|value| {
                    let found: Vec<lsp::Location> =
                        serde_json::from_value(value).unwrap_or_default();
                    found.into_iter().map(source_span).collect()
                }));
            }
            let response = match query.kind {
                NavigationKind::Definition => router.definition(place).await,
                NavigationKind::TypeDefinition => router.type_definition(place).await,
                NavigationKind::Implementation => router.implementation(place).await,
                NavigationKind::SuperMethod => Ok(None),
            };
            Some(response.map(locations))
        });
    }
}

impl Provider<DocumentSpot, HoverText> for LspProviders {
    fn start(&self, spot: DocumentSpot, cancel: Cancel, reply: Reply<HoverText>) {
        let router = self.router.clone();
        self.answer(cancel, reply, async move {
            let hover = router.hover(place(&spot)?).await;
            Some(hover.map(|hover| hover.map(hover_text).unwrap_or_default()))
        });
    }
}

impl Provider<InlayHintQuery, Vec<InlayHintEntry>> for LspProviders {
    fn start(&self, query: InlayHintQuery, cancel: Cancel, mut reply: Reply<Vec<InlayHintEntry>>) {
        let router = self.router.clone();
        let mut refreshes = self.published.inlay_refresh();
        let source = self.source;
        self.runtime.spawn(async move {
            let Ok(uri) = Uri::from_str(&query.uri) else {
                return;
            };
            let range = range(query.range);
            // The hints of the previous answer come first, already moved by the edits:
            // the text does not blink while the server thinks.
            let cached = router.cached_inlay_hints(&uri, query.version, &range);
            if !cached.is_empty() {
                let _ = reply.send(Answer::new(
                    inlay_hints(Some(cached)),
                    Confidence::Partial,
                    source,
                ));
            }
            loop {
                // A server that still imports the project knows no parameter names of
                // the libraries; the hints are asked once it is ready.
                let ready = tokio::select! {
                    ready = router.ready_after(None) => ready,
                    () = cancelled(&cancel) => return,
                };
                let Some(ready) = ready else {
                    return;
                };
                let generation = Some(ready);
                let place = Place {
                    uri: uri.clone(),
                    version: query.version,
                    position: Position::default(),
                };
                match router.inlay_hints(place, range).await {
                    Ok(answer) => {
                        let answer = Answer::new(inlay_hints(answer), Confidence::Certain, source);
                        if reply.send(answer).is_err() {
                            return;
                        }
                    }
                    Err(RouteError::Stale | RouteError::Superseded) => return,
                    Err(_) => {
                        if reply
                            .send(Answer::new(Vec::new(), Confidence::Unknown, source))
                            .is_err()
                        {
                            return;
                        }
                    }
                }
                // A server that changed what it would answer asks for the hints again,
                // and so does a new server after a restart.
                tokio::select! {
                    changed = refreshes.changed() => {
                        if changed.is_err() {
                            return;
                        }
                    }
                    _ = router.ready_after(generation) => {}
                    () = cancelled(&cancel) => return,
                }
            }
        });
    }
}

impl Provider<SignatureQuery, SignatureText> for LspProviders {
    fn start(&self, query: SignatureQuery, cancel: Cancel, reply: Reply<SignatureText>) {
        let router = self.router.clone();
        self.answer(cancel, reply, async move {
            let help = router.signature_help(place(&query.0)?).await;
            Some(help.map(signature_text))
        });
    }
}

impl Provider<SignatureQuery, SignatureHelp> for LspProviders {
    fn start(&self, query: SignatureQuery, cancel: Cancel, reply: Reply<SignatureHelp>) {
        let router = self.router.clone();
        self.answer(cancel, reply, async move {
            let help = router.signature_help(place(&query.0)?).await;
            Some(help.map(|help| help.map(signature_help_info).unwrap_or_default()))
        });
    }
}

impl Provider<DocumentSpot, ExpectedType> for LspProviders {
    fn start(&self, spot: DocumentSpot, cancel: Cancel, reply: Reply<ExpectedType>) {
        let router = self.router.clone();
        self.answer(cancel, reply, async move {
            let place = place(&spot)?;
            if let Ok(Some(help)) = router.signature_help(place.clone()).await
                && let Some(type_name) = expected_type_from_signature(&help)
            {
                return Some(Ok(ExpectedType {
                    type_name: Some(type_name),
                }));
            }
            let text = router.text(&place.uri)?;
            let offset = offset_in(
                &text,
                TextPoint {
                    line: place.position.line,
                    character: place.position.character,
                },
            );
            Some(Ok(ExpectedType {
                type_name: expected_type_from_declaration(&text, offset),
            }))
        });
    }
}

impl Provider<DocumentSpot, ExternalDoc> for LspProviders {
    fn start(&self, spot: DocumentSpot, cancel: Cancel, reply: Reply<ExternalDoc>) {
        let router = self.router.clone();
        self.answer(cancel, reply, async move {
            let hover = router.hover(place(&spot)?).await;
            let url = hover
                .ok()
                .flatten()
                .map(hover_text)
                .and_then(|text| documentation_url(&text.0));
            Some(Ok(ExternalDoc { url }))
        });
    }
}

impl Provider<FormatQuery, Vec<DocumentEdit>> for LspProviders {
    fn start(&self, query: FormatQuery, cancel: Cancel, reply: Reply<Vec<DocumentEdit>>) {
        let router = self.router.clone();
        self.answer(cancel, reply, async move {
            let place = Place {
                uri: Uri::from_str(&query.uri).ok()?,
                version: query.version,
                position: Position::default(),
            };
            let options = lsp::FormattingOptions {
                tab_size: query.tab_size,
                insert_spaces: query.insert_spaces,
                ..lsp::FormattingOptions::default()
            };
            let edits = router.formatting(place, options).await;
            Some(edits.map(|edits| edits.into_iter().flatten().map(document_edit).collect()))
        });
    }
}

impl Provider<ActionQuery, Vec<ActionEntry>> for LspProviders {
    fn start(&self, query: ActionQuery, cancel: Cancel, reply: Reply<Vec<ActionEntry>>) {
        let router = self.router.clone();
        let published = self.published.clone();
        let edit_command = self.edit_command;
        let refactor_action = self.refactor_action;
        let refactor_edit_request = self.refactor_edit_request;
        let prompt_sources = self.prompt_sources.clone();
        self.answer(cancel, reply, async move {
            let place = Place {
                uri: Uri::from_str(&query.uri).ok()?,
                version: query.version,
                position: position(query.span.start),
            };
            let only = query.only.map(|kind| vec![lsp_kind(kind)]);
            let asking_source = query.only.is_none_or(|kind| kind == ActionKind::Source);
            let range = range(query.span);
            let actions = router
                .code_actions(place.clone(), range, &published, only)
                .await;
            let mut entries = match actions {
                Ok(actions) => {
                    action_entries(
                        &router,
                        actions,
                        edit_command,
                        refactor_action,
                        refactor_edit_request,
                    )
                    .await
                }
                Err(error) => return Some(Err(error)),
            };
            if asking_source {
                for source in prompt_sources {
                    if entries
                        .iter()
                        .any(|entry| entry.code.as_deref() == Some(source.kind))
                    {
                        continue;
                    }
                    if let Some(entry) = prompt_entry(&router, &place.uri, range, source).await {
                        entries.push(entry);
                    }
                }
            }
            Some(Ok(entries))
        });
    }
}

impl Provider<DocumentSpot, Vec<TextSpan>> for LspProviders {
    fn start(&self, spot: DocumentSpot, cancel: Cancel, reply: Reply<Vec<TextSpan>>) {
        let router = self.router.clone();
        self.answer(cancel, reply, async move {
            let highlights = router.document_highlight(place(&spot)?).await;
            Some(highlights.map(highlight_spans))
        });
    }
}

/// How long a token request waits for the server to say what its legend is. The answer
/// to `initialize` comes before the first request of a document in practice.
const TOKENS_READY_LIMIT: Duration = Duration::from_secs(15);
const TOKENS_READY_POLL: Duration = Duration::from_millis(50);

async fn semantic_capability(
    setup: &lsp_client::SemanticTokensSetup,
) -> Option<SemanticTokensCapability> {
    let mut waited = Duration::ZERO;
    loop {
        if let Some(capability) = setup.get() {
            return Some(capability);
        }
        if waited >= TOKENS_READY_LIMIT {
            return None;
        }
        sleep(TOKENS_READY_POLL).await;
        waited += TOKENS_READY_POLL;
    }
}

/// Turns the delta-encoded tokens of a server into spans. The numbers of the answer name
/// the types and the modifiers of the legend, so the same answer means nothing without it.
pub fn semantic_tokens_from_lsp(
    data: &[lsp::SemanticToken],
    capability: &SemanticTokensCapability,
) -> Vec<SemanticToken> {
    let mut line = 0u32;
    let mut character = 0u32;
    let mut tokens = Vec::with_capacity(data.len());
    for token in data {
        line = line.saturating_add(token.delta_line);
        character = if token.delta_line == 0 {
            character.saturating_add(token.delta_start)
        } else {
            token.delta_start
        };
        let Some(kind) = capability
            .token_types
            .get(token.token_type as usize)
            .and_then(|name| SemanticTokenKind::from_lsp(name))
        else {
            continue;
        };
        let mut modifiers = SemanticTokenModifiers::default();
        for (index, name) in capability.token_modifiers.iter().enumerate() {
            if index < u32::BITS as usize && token.token_modifiers_bitset & (1 << index) != 0 {
                modifiers.add(name);
            }
        }
        let end = character.saturating_add(token.length);
        tokens.push(SemanticToken {
            span: TextSpan {
                start: TextPoint { line, character },
                end: TextPoint {
                    line,
                    character: end,
                },
            },
            kind,
            modifiers,
        });
    }
    tokens
}

impl Provider<SemanticTokensQuery, Vec<SemanticToken>> for LspProviders {
    fn start(&self, query: SemanticTokensQuery, cancel: Cancel, reply: Reply<Vec<SemanticToken>>) {
        let Some(setup) = self.semantic_tokens.clone() else {
            return;
        };
        let Ok(uri) = Uri::from_str(&query.uri) else {
            return;
        };
        let place = Place {
            uri,
            version: query.version,
            position: Position::new(0, 0),
        };
        let asked_range = query.range.map(range);
        let router = self.router.clone();
        self.answer(cancel, reply, async move {
            let capability = semantic_capability(&setup).await?;
            let range = asked_range.filter(|_| capability.range);
            // A server that colours neither the whole document nor a range has nothing
            // to add to the syntax colours.
            if range.is_none() && !capability.full {
                return Some(Ok(Vec::new()));
            }
            let tokens = router.semantic_tokens(place, range).await;
            Some(tokens.map(|tokens| {
                tokens
                    .map(|tokens| semantic_tokens_from_lsp(&tokens.data, &capability))
                    .unwrap_or_default()
            }))
        });
    }
}

impl Provider<DocumentSpot, Vec<SourceSpan>> for LspProviders {
    fn start(&self, spot: DocumentSpot, cancel: Cancel, reply: Reply<Vec<SourceSpan>>) {
        let router = self.router.clone();
        self.answer(cancel, reply, async move {
            let found = router.references(place(&spot)?).await;
            let spans = |found: Option<Vec<lsp::Location>>| {
                found.into_iter().flatten().map(source_span).collect()
            };
            Some(found.map(spans))
        });
    }
}

impl Provider<CodeLensQuery, Vec<CodeLensEntry>> for LspProviders {
    fn start(&self, query: CodeLensQuery, cancel: Cancel, mut reply: Reply<Vec<CodeLensEntry>>) {
        let Some(setup) = self.code_lens.clone() else {
            return;
        };
        let Ok(uri) = Uri::from_str(&query.uri) else {
            return;
        };
        let router = self.router.clone();
        let mut refreshes = self.published.code_lens_refresh();
        let source = self.source;
        self.runtime.spawn(async move {
            // The server says at `initialize` whether it sends the labels resolved.
            let Some(_capability) = code_lens_capability(&setup).await else {
                return;
            };
            loop {
                // Counts of usages need the imported project; a server that still
                // imports is asked once it is ready.
                let ready = tokio::select! {
                    ready = router.ready_after(None) => ready,
                    () = cancelled(&cancel) => return,
                };
                let Some(ready) = ready else {
                    return;
                };
                let generation = Some(ready);
                // The labels of the previous answer come first, already moved by the
                // edits: the rows do not blink while the server thinks.
                let cached = router.cached_code_lens(&uri, query.version);
                if !cached.is_empty()
                    && reply
                        .send(Answer::new(
                            code_lens_entries(Some(cached)),
                            Confidence::Partial,
                            source,
                        ))
                        .is_err()
                {
                    return;
                }
                let place = Place {
                    uri: uri.clone(),
                    version: query.version,
                    position: Position::default(),
                };
                match router.code_lens(place).await {
                    Ok(answer) => {
                        let entries = code_lens_entries(answer);
                        if reply
                            .send(Answer::new(entries, Confidence::Certain, source))
                            .is_err()
                        {
                            return;
                        }
                    }
                    Err(RouteError::Stale | RouteError::Superseded) => return,
                    Err(_) => {
                        if reply
                            .send(Answer::new(Vec::new(), Confidence::Unknown, source))
                            .is_err()
                        {
                            return;
                        }
                    }
                }
                // A server that changed its labels asks for them again, and so does a
                // new server after a restart.
                tokio::select! {
                    changed = refreshes.changed() => {
                        if changed.is_err() {
                            return;
                        }
                    }
                    _ = router.ready_after(generation) => {}
                    () = cancelled(&cancel) => return,
                }
            }
        });
    }
}

impl Provider<CodeLensResolveQuery, CodeLensEntry> for LspProviders {
    fn start(&self, query: CodeLensResolveQuery, cancel: Cancel, reply: Reply<CodeLensEntry>) {
        let router = self.router.clone();
        self.answer(cancel, reply, async move {
            let lens: CodeLens = serde_json::from_str(query.lens.resolve.as_deref()?).ok()?;
            let place = Place {
                uri: Uri::from_str(&query.uri).ok()?,
                version: query.version,
                position: Position::default(),
            };
            let resolved = router.code_lens_resolve(place, lens).await.ok()?;
            Some(Ok(code_lens_entry(resolved)))
        });
    }
}

/// Files opened on the server for the usages search, so a closed file can tell the
/// kind of a use. Dropping the guard closes them, even when the search is cancelled.
struct OpenedDocuments {
    router: Arc<Router>,
    uris: Vec<Uri>,
}

impl OpenedDocuments {
    fn close_all(&mut self) {
        for uri in self.uris.drain(..) {
            self.router.close(&uri);
        }
    }
}

impl Drop for OpenedDocuments {
    fn drop(&mut self) {
        self.close_all();
    }
}

/// How many closed files the usages search opens at once.
const USAGES_OPEN_BATCH: usize = 16;

impl Provider<DocumentSpot, Vec<UsageEntry>> for LspProviders {
    fn start(&self, spot: DocumentSpot, cancel: Cancel, reply: Reply<Vec<UsageEntry>>) {
        let router = self.router.clone();
        let language = self.language;
        let work_cancel = cancel.clone();
        let import_line = self.import_line;
        self.answer(cancel, reply, async move {
            let found = router
                .references(place(&spot)?)
                .await
                .ok()?
                .unwrap_or_default();
            let definitions = router
                .definition(place(&spot)?)
                .await
                .ok()
                .flatten()
                .map(|response| locations(Some(response)))
                .unwrap_or_default();
            let mut targets: Vec<Option<SourceSpan>> = vec![None; found.len()];
            let mut groups: HashMap<String, Vec<usize>> = HashMap::new();
            for (index, location) in found.into_iter().enumerate() {
                let target = source_span(location);
                let is_declaration = definitions.iter().any(|definition| {
                    definition.uri == target.uri
                        && (definition.span == target.span
                            || definition.span.start == target.span.start)
                });
                // IDEA leaves the declaration out of Alt+F7 and Show Usages.
                if is_declaration {
                    continue;
                }
                groups.entry(target.uri.clone()).or_default().push(index);
                targets[index] = Some(target);
            }
            let mut texts: HashMap<String, Vec<String>> = HashMap::new();
            let mut entries: Vec<Option<UsageEntry>> = (0..targets.len()).map(|_| None).collect();
            let groups: Vec<(String, Vec<usize>)> = groups.into_iter().collect();
            let mut opened = OpenedDocuments {
                router: router.clone(),
                uris: Vec::new(),
            };
            for chunk in groups.chunks(USAGES_OPEN_BATCH) {
                for (uri, _) in chunk {
                    if work_cancel.is_cancelled() {
                        break;
                    }
                    let Ok(uri) = Uri::from_str(uri) else {
                        continue;
                    };
                    // A document the editor holds open stays the editor's: opening it
                    // again and closing it below would end its sync with the server.
                    if router.version(&uri).is_some() {
                        continue;
                    }
                    let Some(path) = lsp_client::uri::file_path(&uri) else {
                        continue;
                    };
                    let Ok(text) = std::fs::read_to_string(path) else {
                        continue;
                    };
                    router.open(uri.clone(), language, 1, text.into());
                    opened.uris.push(uri);
                }
                for (uri_text, indexes) in chunk {
                    if work_cancel.is_cancelled() {
                        break;
                    }
                    let Ok(uri) = Uri::from_str(uri_text) else {
                        continue;
                    };
                    let Some(first) = indexes.iter().find_map(|index| targets[*index].as_ref())
                    else {
                        continue;
                    };
                    let place = Place {
                        uri: uri.clone(),
                        version: router.version(&uri).unwrap_or(spot.version),
                        position: position(first.span.start),
                    };
                    let highlights = router
                        .document_highlight(place.clone())
                        .await
                        .ok()
                        .flatten()
                        .unwrap_or_default();
                    let symbols = match router.document_symbol(place).await {
                        Ok(Some(DocumentSymbolResponse::Nested(found))) => found,
                        _ => Vec::new(),
                    };
                    for index in indexes {
                        let Some(target) = targets[*index].as_ref() else {
                            continue;
                        };
                        let line = text_line(&target.uri, target.span.start.line, &mut texts);
                        let highlight = highlights
                            .iter()
                            .find(|highlight| span(highlight.range) == target.span)
                            .and_then(|highlight| highlight.kind);
                        let kind = usage_kind(&line, &target.span, highlight, false, import_line);
                        let container = container_of(&symbols, target.span.start);
                        entries[*index] = Some(UsageEntry {
                            target: target.clone(),
                            kind,
                            container,
                        });
                    }
                }
                opened.close_all();
            }
            Some(Ok(entries.into_iter().flatten().collect()))
        });
    }
}

/// One line of the file a URI names, read once per file.
fn text_line(uri: &str, line: u32, texts: &mut HashMap<String, Vec<String>>) -> String {
    let lines = texts.entry(uri.to_owned()).or_insert_with(|| {
        Uri::from_str(uri)
            .ok()
            .and_then(|uri| lsp_client::uri::file_path(&uri))
            .and_then(|path| std::fs::read_to_string(path).ok())
            .map(|text| text.lines().map(ToOwned::to_owned).collect())
            .unwrap_or_default()
    });
    lines.get(line as usize).cloned().unwrap_or_default()
}

fn usage_kind(
    line: &str,
    span: &TextSpan,
    highlight: Option<DocumentHighlightKind>,
    is_declaration: bool,
    import_line: fn(&str) -> bool,
) -> UsageKind {
    if import_line(line) {
        return UsageKind::Import;
    }
    if is_declaration {
        return UsageKind::Declaration;
    }
    if let Some(kind) = highlight {
        if kind == DocumentHighlightKind::READ {
            return UsageKind::Read;
        }
        if kind == DocumentHighlightKind::WRITE {
            return UsageKind::Write;
        }
    }
    let after = line
        .get(span.end.character as usize..)
        .unwrap_or_default()
        .trim_start();
    // A server names a call by its name, or by the whole invocation with arguments.
    let inside = line
        .get(span.start.character as usize..span.end.character as usize)
        .unwrap_or_default();
    if after.starts_with('(') || inside.contains('(') {
        return UsageKind::Call;
    }
    UsageKind::Unknown
}

/// The declarations that contain a point, outermost first, joined by dots.
fn container_of(symbols: &[DocumentSymbol], point: TextPoint) -> Option<String> {
    let mut best: Option<(&DocumentSymbol, Vec<String>)> = None;
    for symbol in symbols {
        let found = container_in(symbol, point);
        if let Some(names) = found
            && best
                .as_ref()
                .is_none_or(|(_, best)| names.len() >= best.len())
        {
            best = Some((symbol, names));
        }
    }
    best.map(|(_, names)| names.join("."))
}

fn container_in(symbol: &DocumentSymbol, point: TextPoint) -> Option<Vec<String>> {
    if !contains(symbol.range, point) {
        return None;
    }
    for child in symbol.children.iter().flatten() {
        if let Some(mut names) = container_in(child, point) {
            names.insert(0, symbol.name.clone());
            return Some(names);
        }
    }
    Some(vec![symbol.name.clone()])
}

fn contains(range: Range, point: TextPoint) -> bool {
    let start = point_of(range.start);
    let end = point_of(range.end);
    (start.line, start.character) <= (point.line, point.character)
        && (point.line, point.character) <= (end.line, end.character)
}

fn point_of(point: Position) -> TextPoint {
    TextPoint {
        line: point.line,
        character: point.character,
    }
}

impl Provider<RenameQuery, WorkspaceChange> for LspProviders {
    fn start(&self, query: RenameQuery, cancel: Cancel, reply: Reply<WorkspaceChange>) {
        let router = self.router.clone();
        self.answer(cancel, reply, async move {
            let edit = router.rename(place(&query.spot)?, query.new_name).await;
            Some(edit.map(|edit| edit.map(workspace_change).unwrap_or_default()))
        });
    }
}

fn move_kind(kind: MoveKind) -> &'static str {
    match kind {
        MoveKind::File => "moveResource",
    }
}

/// The folder a destination names, when the server names it as a `file:` URI.
fn destination_folder(destination: &serde_json::Value) -> Option<PathBuf> {
    let uri = destination.get("uri")?.as_str()?;
    let uri = Uri::from_str(uri).ok()?;
    lsp_client::uri::file_path(&uri)
}

impl Provider<MoveQuery, Vec<MoveDestination>> for LspProviders {
    fn start(&self, query: MoveQuery, cancel: Cancel, reply: Reply<Vec<MoveDestination>>) {
        let router = self.router.clone();
        let Some(request) = self.move_destinations_request else {
            self.answer(cancel, reply, async { Some(Ok(Vec::new())) });
            return;
        };
        self.answer(cancel, reply, async move {
            let place = place(&query.spot)?;
            let params = serde_json::json!({
                "moveKind": move_kind(query.kind),
                "sourceUris": [place.uri.as_str()],
            });
            let answer = router.extension(request, params).await;
            Some(answer.map(|value| {
                value
                    .get("destinations")
                    .and_then(serde_json::Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|destination| {
                        let name = destination.get("displayName")?.as_str()?.to_owned();
                        let project = destination.get("project").and_then(|v| v.as_str());
                        let path = destination.get("path").and_then(|v| v.as_str());
                        let detail = match (project, path) {
                            (Some(project), Some(path)) => Some(format!("{project}  {path}")),
                            (Some(project), None) => Some(project.to_owned()),
                            _ => None,
                        };
                        let folder = destination_folder(destination);
                        Some(MoveDestination {
                            name,
                            detail,
                            folder,
                            handle: destination.to_string(),
                        })
                    })
                    .collect()
            }))
        });
    }
}

impl Provider<MoveRequest, MoveOutcome> for LspProviders {
    fn start(&self, query: MoveRequest, cancel: Cancel, reply: Reply<MoveOutcome>) {
        let router = self.router.clone();
        let Some(request) = self.move_request else {
            self.answer(cancel, reply, async { Some(Ok(MoveOutcome::default())) });
            return;
        };
        self.answer(cancel, reply, async move {
            let place = place(&query.spot)?;
            let destination: serde_json::Value =
                serde_json::from_str(&query.destination.handle).ok()?;
            let params = serde_json::json!({
                "moveKind": move_kind(query.kind),
                "sourceUris": [place.uri.as_str()],
                "destination": destination,
                "updateReferences": query.update_references,
            });
            let answer = router.extension(request, params).await;
            Some(answer.map(|value| {
                let error = value
                    .get("errorMessage")
                    .and_then(|error| error.as_str())
                    .filter(|error| !error.is_empty())
                    .map(str::to_owned);
                let change = value
                    .get("edit")
                    .cloned()
                    .and_then(|edit| serde_json::from_value::<lsp::WorkspaceEdit>(edit).ok())
                    .map(workspace_change)
                    .unwrap_or_default();
                MoveOutcome { change, error }
            }))
        });
    }
}

impl Provider<DocumentSpot, ChangeSignatureInfo> for LspProviders {
    fn start(&self, spot: DocumentSpot, cancel: Cancel, mut reply: Reply<ChangeSignatureInfo>) {
        let Some(request) = self.change_signature_info_request else {
            let answer = Answer::new(
                ChangeSignatureInfo {
                    signature: None,
                    error: Some("Change Signature is not supported by this language".to_owned()),
                },
                Confidence::Unknown,
                self.source,
            );
            let _ = reply.send(answer);
            return;
        };
        let router = self.router.clone();
        let source = self.source;
        self.runtime.spawn(async move {
            let Some(place) = place(&spot) else {
                return;
            };
            let context = change_context(&place.uri, &spot);
            tokio::select! {
                result = router.extension(request, context) => {
                    let answer = match result {
                        Ok(value) => Answer::new(
                            change_signature_info(&value),
                            Confidence::Certain,
                            source,
                        ),
                        Err(RouteError::Stale | RouteError::Superseded) => return,
                        Err(error) => Answer::new(
                            ChangeSignatureInfo {
                                signature: None,
                                error: Some(error.to_string()),
                            },
                            Confidence::Unknown,
                            source,
                        ),
                    };
                    let _ = reply.send(answer);
                }
                () = cancelled(&cancel) => {}
            }
        });
    }
}

impl Provider<ChangeSignatureRequest, ChangeSignatureOutcome> for LspProviders {
    fn start(
        &self,
        query: ChangeSignatureRequest,
        cancel: Cancel,
        mut reply: Reply<ChangeSignatureOutcome>,
    ) {
        let (Some(info_request), Some(edit_request), Some(command)) = (
            self.change_signature_info_request,
            self.change_signature_edit_request,
            self.change_signature_command,
        ) else {
            let answer = Answer::new(
                ChangeSignatureOutcome {
                    change: WorkspaceChange::default(),
                    error: Some("Change Signature is not supported by this language".to_owned()),
                },
                Confidence::Unknown,
                self.source,
            );
            let _ = reply.send(answer);
            return;
        };
        let router = self.router.clone();
        let source = self.source;
        self.runtime.spawn(async move {
            let Some(place) = place(&query.spot) else {
                return;
            };
            let context = change_context(&place.uri, &query.spot);
            let work = async {
                let info = router.extension(info_request, context.clone()).await?;
                if let Some(error) = info
                    .get("errorMessage")
                    .and_then(serde_json::Value::as_str)
                    .filter(|error| !error.is_empty())
                {
                    return Ok(ChangeSignatureOutcome {
                        change: WorkspaceChange::default(),
                        error: Some(error.to_owned()),
                    });
                }
                let Some(method) = info
                    .get("methodIdentifier")
                    .and_then(serde_json::Value::as_str)
                else {
                    return Ok(ChangeSignatureOutcome {
                        change: WorkspaceChange::default(),
                        error: Some("Cannot change the signature here".to_owned()),
                    });
                };
                // The server keeps the exceptions of the method itself, so its own
                // description of them is given back unchanged.
                let exceptions = info
                    .get("exceptions")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!([]));
                let parameters: Vec<serde_json::Value> = query
                    .signature
                    .parameters
                    .iter()
                    .map(|parameter| {
                        serde_json::json!({
                            "type": parameter.type_name,
                            "name": parameter.name,
                            "defaultValue": parameter.default_value,
                            "originalIndex": parameter
                                .original_index
                                .map_or(-1, |index| index as i64),
                        })
                    })
                    .collect();
                let arguments = serde_json::json!([
                    method,
                    false,
                    query.signature.name,
                    query.signature.visibility,
                    query.signature.return_type,
                    parameters,
                    exceptions,
                    true
                ]);
                let params = serde_json::json!({
                    "command": command,
                    "commandArguments": arguments,
                    "context": context,
                    "options": {"tabSize": 4, "insertSpaces": true},
                });
                let reply = router.extension(edit_request, params).await?;
                let mut outcome = ChangeSignatureOutcome::default();
                if let Some(error) = reply
                    .get("errorMessage")
                    .and_then(serde_json::Value::as_str)
                    .filter(|error| !error.is_empty())
                {
                    outcome.error = Some(error.to_owned());
                } else if let Some(edit) = reply.get("edit") {
                    match serde_json::from_value::<lsp::WorkspaceEdit>(edit.clone()) {
                        Ok(edit) => outcome.change = workspace_change(edit),
                        Err(error) => {
                            outcome.error = Some(format!(
                                "The server sent edits that cannot be read: {error}"
                            ))
                        }
                    }
                } else {
                    outcome.error = Some("The server returned no edits".to_owned());
                }
                Ok(outcome)
            };
            tokio::select! {
                result = work => {
                    let answer = match result {
                        Ok(outcome) => Answer::new(outcome, Confidence::Certain, source),
                        Err(RouteError::Stale | RouteError::Superseded) => return,
                        Err(error) => Answer::new(
                            ChangeSignatureOutcome {
                                change: WorkspaceChange::default(),
                                error: Some(error.to_string()),
                            },
                            Confidence::Unknown,
                            source,
                        ),
                    };
                    let _ = reply.send(answer);
                }
                () = cancelled(&cancel) => {}
            }
        });
    }
}

/// One item of a type hierarchy as the raw reply carries it. The server's `kind` is
/// left out: some servers send a value this LSP shape does not read.
fn hierarchy_entry(item: &serde_json::Value) -> Option<SymbolEntry> {
    let text = |key: &str| item.get(key).and_then(serde_json::Value::as_str);
    let range = item.get("selectionRange")?;
    let point = |value: &serde_json::Value| {
        Some(TextPoint {
            line: value.get("line")?.as_u64()? as u32,
            character: value.get("character")?.as_u64()? as u32,
        })
    };
    Some(SymbolEntry {
        name: text("name")?.to_owned(),
        container: text("detail").map(str::to_owned),
        place: Some(SourceSpan {
            uri: text("uri")?.to_owned(),
            span: TextSpan {
                start: point(range.get("start")?)?,
                end: point(range.get("end")?)?,
            },
        }),
    })
}

/// The type hierarchy of a place through the JSON of the protocol, so a server that
/// sends a kind outside the LSP enum still answers.
async fn type_hierarchy(
    router: &Router,
    uri: &Uri,
    at: Position,
    supertypes: bool,
) -> Result<Vec<SymbolEntry>, RouteError> {
    let params = serde_json::json!({
        "textDocument": {"uri": uri.as_str()},
        "position": {"line": at.line, "character": at.character},
    });
    let prepared = router
        .extension("textDocument/prepareTypeHierarchy", params)
        .await?;
    let Some(item) = prepared.as_array().and_then(|items| items.first()) else {
        return Ok(Vec::new());
    };
    let method = if supertypes {
        "typeHierarchy/supertypes"
    } else {
        "typeHierarchy/subtypes"
    };
    let reply = router
        .extension(method, serde_json::json!({"item": item}))
        .await?;
    Ok(reply
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(hierarchy_entry)
        .collect())
}

impl Provider<HierarchyQuery, Vec<SymbolEntry>> for LspProviders {
    fn start(&self, query: HierarchyQuery, cancel: Cancel, reply: Reply<Vec<SymbolEntry>>) {
        let router = self.router.clone();
        self.answer(cancel, reply, async move {
            let place = place(&query.spot)?;
            let supertypes = query.direction == HierarchyDirection::Supertypes;
            let items = type_hierarchy(&router, &place.uri, place.position, supertypes).await;
            Some(items)
        });
    }
}

impl Provider<SymbolQuery, Vec<SymbolEntry>> for LspProviders {
    fn start(&self, query: SymbolQuery, cancel: Cancel, reply: Reply<Vec<SymbolEntry>>) {
        let router = self.router.clone();
        self.answer(cancel, reply, async move {
            Some(router.workspace_symbol(&query.0).await.map(symbols))
        });
    }
}

impl Provider<DiagnosticsQuery, Vec<DiagnosticEntry>> for LspProviders {
    fn start(
        &self,
        query: DiagnosticsQuery,
        cancel: Cancel,
        mut reply: Reply<Vec<DiagnosticEntry>>,
    ) {
        let Ok(uri) = Uri::from_str(&query.0) else {
            return;
        };
        let published = self.published.clone();
        let mut revisions = self.revisions.clone();
        let source = self.source;
        self.runtime.spawn(async move {
            loop {
                let list = published
                    .diagnostics(&uri)
                    .into_iter()
                    .map(diagnostic)
                    .collect();
                let answer = Answer::new(list, Confidence::Certain, source);
                // A full queue only delays this list: the next one replaces it.
                if reply.send(answer).is_err() && reply.is_cancelled() {
                    return;
                }
                tokio::select! {
                    changed = revisions.changed() => {
                        if changed.is_err() {
                            return;
                        }
                    }
                    () = cancelled(&cancel) => return,
                }
            }
        });
    }
}

impl Provider<WorkspaceDiagnosticsQuery, Vec<FileDiagnostics>> for LspProviders {
    fn start(
        &self,
        _: WorkspaceDiagnosticsQuery,
        _: Cancel,
        mut reply: Reply<Vec<FileDiagnostics>>,
    ) {
        let published = self.published.clone();
        let source = self.source;
        self.runtime.spawn(async move {
            let files = published
                .all_diagnostics()
                .into_iter()
                .map(|(uri, diagnostics)| FileDiagnostics {
                    uri: uri.as_str().to_owned(),
                    diagnostics: diagnostics.into_iter().map(diagnostic_note).collect(),
                })
                .collect();
            let _ = reply.send(Answer::new(files, Confidence::Certain, source));
        });
    }
}

impl Provider<RangeFormatQuery, Vec<DocumentEdit>> for LspProviders {
    fn start(&self, query: RangeFormatQuery, cancel: Cancel, reply: Reply<Vec<DocumentEdit>>) {
        let router = self.router.clone();
        self.answer(cancel, reply, async move {
            let place = Place {
                uri: Uri::from_str(&query.uri).ok()?,
                version: query.version,
                position: position(query.range.start),
            };
            let options = lsp::FormattingOptions {
                tab_size: query.tab_size,
                insert_spaces: query.insert_spaces,
                ..lsp::FormattingOptions::default()
            };
            let edits = router
                .range_formatting(place, range(query.range), options)
                .await;
            Some(edits.map(|edits| edits.into_iter().flatten().map(document_edit).collect()))
        });
    }
}

/// The type a method returns, from the field the provider names or from the end of a
/// binding key: `owner.name(args)ReturnType`.
fn return_type_of(method: &serde_json::Value) -> Option<String> {
    if let Some(detail) = method
        .get("returnType")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|detail| !detail.is_empty())
    {
        return Some(detail.to_owned());
    }
    let key = method
        .get("bindingKey")
        .and_then(serde_json::Value::as_str)?;
    type_descriptor(key.rsplit_once(')')?.1)
}

/// A type name from the descriptor at the end of a binding key: `V`, `[I`,
/// `Lsome/Type;` or `TName;`.
fn type_descriptor(descriptor: &str) -> Option<String> {
    let mut rest = descriptor.trim();
    let mut dimensions = 0;
    while let Some(after) = rest.strip_prefix('[') {
        dimensions += 1;
        rest = after;
    }
    let mut name = match rest.chars().next()? {
        'V' => "void".to_owned(),
        'Z' => "boolean".to_owned(),
        'B' => "byte".to_owned(),
        'C' => "char".to_owned(),
        'S' => "short".to_owned(),
        'I' => "int".to_owned(),
        'J' => "long".to_owned(),
        'F' => "float".to_owned(),
        'D' => "double".to_owned(),
        'L' | 'T' => {
            let body = rest.get(1..)?;
            let erased = body.split(';').next()?;
            let name = erased.rsplit(['/', '.']).next().unwrap_or(erased);
            let name = name.split('<').next().unwrap_or(name);
            if name.is_empty() {
                return None;
            }
            name.to_owned()
        }
        _ => return None,
    };
    for _ in 0..dimensions {
        name.push_str("[]");
    }
    Some(name)
}

fn overridable_methods(value: &serde_json::Value, only_abstract: bool) -> OverridableMethods {
    let owner_type = value
        .get("type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let methods = value
        .get("methods")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|method| {
            let abstract_method = method
                .get("unimplemented")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            if only_abstract && !abstract_method {
                return None;
            }
            let text = |key: &str| {
                method
                    .get(key)
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_owned()
            };
            let parameters = method
                .get("parameters")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_owned)
                .collect();
            Some(OverridableMethod {
                name: text("name"),
                owner: text("declaringClass"),
                parameters,
                return_type: return_type_of(method),
                abstract_method,
                handle: method.to_string(),
            })
        })
        .collect();
    OverridableMethods {
        owner_type,
        methods,
    }
}

impl Provider<OverridableMethodsQuery, OverridableMethods> for LspProviders {
    fn start(
        &self,
        query: OverridableMethodsQuery,
        cancel: Cancel,
        reply: Reply<OverridableMethods>,
    ) {
        let router = self.router.clone();
        let Some(request) = self.override_list_request else {
            self.answer(cancel, reply, async {
                Some(Ok(OverridableMethods::default()))
            });
            return;
        };
        self.answer(cancel, reply, async move {
            let place = place(&query.spot)?;
            let context = change_context(&place.uri, &query.spot);
            let answer = router.extension(request, context).await;
            Some(answer.map(|value| overridable_methods(&value, query.only_abstract)))
        });
    }
}

impl Provider<AddOverridableMethodsRequest, WorkspaceChange> for LspProviders {
    fn start(
        &self,
        query: AddOverridableMethodsRequest,
        cancel: Cancel,
        reply: Reply<WorkspaceChange>,
    ) {
        let router = self.router.clone();
        let Some(request) = self.override_add_request else {
            self.answer(cancel, reply, async {
                Some(Ok(WorkspaceChange::default()))
            });
            return;
        };
        self.answer(cancel, reply, async move {
            let place = place(&query.spot)?;
            let methods: Vec<serde_json::Value> = query
                .methods
                .iter()
                .filter_map(|method| serde_json::from_str(&method.handle).ok())
                .collect();
            if methods.is_empty() {
                return Some(Ok(WorkspaceChange::default()));
            }
            let params = serde_json::json!({
                "context": change_context(&place.uri, &query.spot),
                "overridableMethods": methods,
            });
            let answer = router.extension(request, params).await;
            Some(answer.map(|value| {
                serde_json::from_value::<lsp::WorkspaceEdit>(value)
                    .map(workspace_change)
                    .unwrap_or_default()
            }))
        });
    }
}

fn variable_choices(value: &serde_json::Value) -> Vec<GenerateChoice> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .map(|field| {
            let text = |key: &str| {
                field
                    .get(key)
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_owned()
            };
            GenerateChoice {
                label: text("name"),
                detail: Some(text("type")).filter(|detail| !detail.is_empty()),
                checked: field
                    .get("isSelected")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(true),
                handle: field.to_string(),
            }
        })
        .collect()
}

fn accessor_choices(value: &serde_json::Value) -> Vec<GenerateChoice> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .map(|field| {
            let text = |key: &str| {
                field
                    .get(key)
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_owned()
            };
            GenerateChoice {
                label: text("fieldName"),
                detail: Some(text("typeName")).filter(|detail| !detail.is_empty()),
                checked: true,
                handle: field.to_string(),
            }
        })
        .collect()
}

fn delegate_choices(value: &serde_json::Value) -> Vec<GenerateChoice> {
    let mut choices = Vec::new();
    let fields = value
        .get("delegateFields")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten();
    for field in fields {
        let owner = field
            .get("field")
            .and_then(|field| field.get("name"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let methods = field
            .get("delegateMethods")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten();
        for method in methods {
            let name = method
                .get("name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let parameters: Vec<&str> = method
                .get("parameters")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(serde_json::Value::as_str)
                .collect();
            let entry = serde_json::json!({"field": field.get("field"), "delegateMethod": method});
            choices.push(GenerateChoice {
                label: format!("{owner}.{name}({})", parameters.join(", ")),
                detail: Some(owner.clone()),
                checked: false,
                handle: entry.to_string(),
            });
        }
    }
    choices
}

fn generate_context(uri: &Uri, spot: &DocumentSpot) -> serde_json::Value {
    change_context(uri, spot)
}

/// The choices of a generate check, as one shape for the editor.
fn generate_answer(kind: GenerateKind, value: &serde_json::Value) -> Generation {
    if let Some(error) = value
        .get("errorMessage")
        .and_then(serde_json::Value::as_str)
        .filter(|error| !error.is_empty())
    {
        return Generation {
            error: Some(error.to_owned()),
            ..Generation::default()
        };
    }
    match kind {
        GenerateKind::Constructor => Generation {
            choices: variable_choices(value.get("fields").unwrap_or(&serde_json::Value::Null)),
            ..Generation::default()
        },
        GenerateKind::Getter | GenerateKind::Setter | GenerateKind::GetterAndSetter => Generation {
            choices: accessor_choices(value),
            ..Generation::default()
        },
        GenerateKind::EqualsAndHashCode => Generation {
            choices: variable_choices(value.get("fields").unwrap_or(&serde_json::Value::Null)),
            ..Generation::default()
        },
        GenerateKind::ToString => {
            if value
                .get("exists")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
            {
                return Generation {
                    error: Some("toString() already exists".to_owned()),
                    ..Generation::default()
                };
            }
            Generation {
                choices: variable_choices(value.get("fields").unwrap_or(&serde_json::Value::Null)),
                ..Generation::default()
            }
        }
        GenerateKind::OverrideMethods | GenerateKind::ImplementMethods => Generation::default(),
        GenerateKind::Delegate => {
            let choices = delegate_choices(value);
            if choices.is_empty() {
                return Generation {
                    error: Some("Nothing to delegate".to_owned()),
                    ..Generation::default()
                };
            }
            Generation {
                choices,
                ..Generation::default()
            }
        }
        GenerateKind::Test => Generation::default(),
    }
}

fn selected_handles(query: &GenerateApplyRequest) -> Vec<serde_json::Value> {
    query
        .handles
        .iter()
        .filter_map(|handle| serde_json::from_str(handle).ok())
        .collect()
}

/// The request and its params for the apply step of a generation.
fn generate_params(
    query: &GenerateApplyRequest,
    requests: GenerateRequests,
    context: serde_json::Value,
    selected: Vec<serde_json::Value>,
    checked: &serde_json::Value,
) -> Option<(&'static str, serde_json::Value)> {
    let (request, params) = match query.kind {
        GenerateKind::Constructor => {
            let constructors = checked
                .get("constructors")
                .cloned()
                .unwrap_or(serde_json::Value::Array(Vec::new()));
            (
                requests.constructor_generate,
                serde_json::json!({
                    "context": context,
                    "constructors": constructors,
                    "fields": selected,
                }),
            )
        }
        GenerateKind::Getter | GenerateKind::Setter | GenerateKind::GetterAndSetter => (
            requests.accessor_generate,
            serde_json::json!({"context": context, "accessors": selected}),
        ),
        GenerateKind::EqualsAndHashCode => (
            requests.equals_generate,
            serde_json::json!({"context": context, "fields": selected, "regenerate": false}),
        ),
        GenerateKind::ToString => (
            requests.tostring_generate,
            serde_json::json!({"context": context, "fields": selected}),
        ),
        GenerateKind::Delegate => (
            requests.delegate_generate,
            serde_json::json!({"context": context, "delegateEntries": selected}),
        ),
        GenerateKind::OverrideMethods | GenerateKind::ImplementMethods | GenerateKind::Test => {
            return None;
        }
    };
    Some((request, params))
}

impl Provider<GenerateQuery, Generation> for LspProviders {
    fn start(&self, query: GenerateQuery, cancel: Cancel, reply: Reply<Generation>) {
        let router = self.router.clone();
        let requests = self.generate;
        self.answer(cancel, reply, async move {
            let Some(requests) = requests else {
                return Some(Ok(Generation::default()));
            };
            let place = place(&query.spot)?;
            let context = generate_context(&place.uri, &query.spot);
            let check = match query.kind {
                GenerateKind::Constructor => requests.constructor_check,
                GenerateKind::Getter | GenerateKind::Setter | GenerateKind::GetterAndSetter => {
                    requests.accessor_check
                }
                GenerateKind::EqualsAndHashCode => requests.equals_check,
                GenerateKind::ToString => requests.tostring_check,
                GenerateKind::Delegate => requests.delegate_check,
                GenerateKind::OverrideMethods
                | GenerateKind::ImplementMethods
                | GenerateKind::Test => return Some(Ok(Generation::default())),
            };
            let mut params = context;
            if matches!(
                query.kind,
                GenerateKind::Getter | GenerateKind::Setter | GenerateKind::GetterAndSetter
            ) {
                let kind = match query.kind {
                    GenerateKind::Getter => "GETTER",
                    GenerateKind::Setter => "SETTER",
                    _ => "BOTH",
                };
                if let Some(map) = params.as_object_mut() {
                    map.insert(
                        "kind".to_owned(),
                        serde_json::Value::String(kind.to_owned()),
                    );
                }
            }
            let answer = router.extension(check, params).await;
            Some(answer.map(|value| generate_answer(query.kind, &value)))
        });
    }
}

impl Provider<GenerateApplyRequest, Generation> for LspProviders {
    fn start(&self, query: GenerateApplyRequest, cancel: Cancel, reply: Reply<Generation>) {
        let router = self.router.clone();
        let requests = self.generate;
        self.answer(cancel, reply, async move {
            let Some(requests) = requests else {
                return Some(Ok(Generation::default()));
            };
            let place = place(&query.spot)?;
            let context = generate_context(&place.uri, &query.spot);
            let check = match query.kind {
                GenerateKind::Constructor => requests.constructor_check,
                GenerateKind::Getter | GenerateKind::Setter | GenerateKind::GetterAndSetter => {
                    requests.accessor_check
                }
                GenerateKind::EqualsAndHashCode => requests.equals_check,
                GenerateKind::ToString => requests.tostring_check,
                GenerateKind::Delegate => requests.delegate_check,
                GenerateKind::OverrideMethods
                | GenerateKind::ImplementMethods
                | GenerateKind::Test => return Some(Ok(Generation::default())),
            };
            let mut check_params = context.clone();
            if matches!(
                query.kind,
                GenerateKind::Getter | GenerateKind::Setter | GenerateKind::GetterAndSetter
            ) {
                let kind = match query.kind {
                    GenerateKind::Getter => "GETTER",
                    GenerateKind::Setter => "SETTER",
                    _ => "BOTH",
                };
                if let Some(map) = check_params.as_object_mut() {
                    map.insert(
                        "kind".to_owned(),
                        serde_json::Value::String(kind.to_owned()),
                    );
                }
            }
            let checked = router
                .extension(check, check_params)
                .await
                .unwrap_or(serde_json::Value::Null);
            let selected = selected_handles(&query);
            let Some((request, params)) =
                generate_params(&query, requests, context, selected, &checked)
            else {
                return Some(Ok(Generation::default()));
            };
            let answer = router.extension(request, params).await;
            Some(answer.map(|value| {
                let change = serde_json::from_value::<lsp::WorkspaceEdit>(value)
                    .map(workspace_change)
                    .unwrap_or_default();
                Generation {
                    change: Some(change),
                    ..Generation::default()
                }
            }))
        });
    }
}

fn call_item(item: LspCallHierarchyItem) -> CallHierarchyItem {
    CallHierarchyItem {
        name: item.name.clone(),
        detail: item.detail.clone(),
        place: Some(SourceSpan {
            uri: item.uri.as_str().to_owned(),
            span: span(item.selection_range),
        }),
        handle: serde_json::to_string(&item).unwrap_or_default(),
    }
}

fn lsp_call_item(item: &CallHierarchyItem) -> Option<LspCallHierarchyItem> {
    serde_json::from_str(&item.handle).ok()
}

impl Provider<CallHierarchyQuery, Vec<CallHierarchyItem>> for LspProviders {
    fn start(
        &self,
        query: CallHierarchyQuery,
        cancel: Cancel,
        reply: Reply<Vec<CallHierarchyItem>>,
    ) {
        let router = self.router.clone();
        self.answer(cancel, reply, async move {
            match query {
                CallHierarchyQuery::Roots(spot) => {
                    let items = router.prepare_call_hierarchy(place(&spot)?).await;
                    Some(items.map(|items| items.into_iter().map(call_item).collect()))
                }
                CallHierarchyQuery::Calls { item, direction } => {
                    let item = lsp_call_item(&item)?;
                    let incoming = direction == CallDirection::Incoming;
                    let calls = router.call_hierarchy_calls(item, incoming).await;
                    Some(calls.map(|calls| calls.into_iter().map(call_item).collect()))
                }
            }
        });
    }
}

impl SemanticServer for LspProviders {
    fn open(&self, uri: &str, version: i32, text: Arc<str>) {
        if let Ok(uri) = Uri::from_str(uri) {
            self.router.open(uri, self.language, version, text);
        }
    }

    fn change(&self, uri: &str, version: i32, text: Arc<str>, edits: Vec<DocumentEdit>) {
        let Ok(uri) = Uri::from_str(uri) else {
            return;
        };
        let mut changes: Vec<TextDocumentContentChangeEvent> = edits
            .into_iter()
            .map(|edit| TextDocumentContentChangeEvent {
                range: Some(range(edit.span)),
                range_length: None,
                text: edit.text,
            })
            .collect();
        if changes.is_empty() {
            changes.push(TextDocumentContentChangeEvent {
                range: None,
                range_length: None,
                text: text.to_string(),
            });
        }
        self.router.change(&uri, version, text, changes);
    }

    fn save(&self, uri: &str) {
        if let Ok(uri) = Uri::from_str(uri) {
            self.router.save(&uri);
        }
    }

    fn close(&self, uri: &str) {
        if let Ok(uri) = Uri::from_str(uri) {
            self.router.close(&uri);
        }
    }

    fn file_changed(&self, uri: &str, change: lang_api::FileChange) {
        self.router.file_changed_as(uri, change);
    }

    fn version(&self, uri: &str) -> Option<i32> {
        self.router.version(&Uri::from_str(uri).ok()?)
    }
}

#[cfg(test)]
mod tests {
    #[cfg(windows)]
    use std::path::PathBuf;

    use lang_api::ActionKind;
    use lsp_client::lsp_types as lsp;
    use serde_json::json;

    #[cfg(windows)]
    use super::destination_folder;
    use super::{
        TextPoint, TextSpan, action_entry, completion_entry, diagnostic, documentation_url,
        expected_type_from_declaration, inlay_hints, parameter_type, return_type_of,
        signature_help_info, usage_kind,
    };

    fn action(value: serde_json::Value) -> lsp::CodeActionOrCommand {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn an_action_brings_the_edits_of_its_documents() {
        let entry = action_entry(
            action(json!({
                "title": "Organize imports",
                "kind": "source.organizeImports",
                "edit": {"documentChanges": [{
                    "textDocument": {"uri": "file:///a/B.x", "version": 3},
                    "edits": [{"range": {"start": {"line": 2, "character": 0},
                                         "end": {"line": 3, "character": 0}},
                               "newText": ""}],
                }]},
            })),
            None,
        )
        .unwrap();
        assert_eq!(entry.kind, Some(ActionKind::OrganizeImports));
        assert_eq!(entry.edits.len(), 1);
        assert_eq!(entry.edits[0].uri, "file:///a/B.x");
        assert_eq!(entry.edits[0].edits[0].span.start.line, 2);
    }

    #[test]
    fn a_snippet_completion_carries_its_format_and_edited_text() {
        let entry = completion_entry(
            serde_json::from_value(json!({
                "label": "getLastName() : String",
                "detail": "Person.getLastName() : String",
                "insertText": "getLastName($0)",
                "insertTextFormat": 2,
                "sortText": "00001",
            }))
            .unwrap(),
        );
        assert!(entry.snippet);
        assert_eq!(entry.insert_text.as_deref(), Some("getLastName($0)"));
        assert_eq!(entry.sort_text.as_deref(), Some("00001"));
        assert_eq!(
            entry.detail.as_deref(),
            Some("Person.getLastName() : String")
        );
    }

    #[test]
    fn the_change_signature_action_keeps_its_place_without_edits() {
        let entry = action_entry(
            action(json!({
                "title": "Change Signature of combine",
                "kind": "refactor.change.signature",
                "command": {
                    "title": "Change Signature of combine",
                    "command": "lang.applyRefactoringCommand",
                    "arguments": ["changeSignature", {}],
                },
            })),
            Some("lang.applyEdit"),
        )
        .unwrap();
        assert_eq!(entry.kind, Some(ActionKind::Refactor));
        assert_eq!(entry.code.as_deref(), Some("refactor.change.signature"));
        assert!(entry.edits.is_empty() && entry.renames.is_empty());
    }

    #[test]
    fn only_the_named_command_counts_as_an_edit() {
        let command = json!({
            "title": "Add import",
            "command": "lang.applyEdit",
            "arguments": [{"changes": {"file:///a/B.x": [
                {"range": {"start": {"line": 0, "character": 0},
                           "end": {"line": 0, "character": 0}},
                 "newText": "import a.C;\n"}]}}],
        });
        let entry = action_entry(action(command.clone()), Some("lang.applyEdit")).unwrap();
        assert_eq!(entry.edits[0].edits[0].text, "import a.C;\n");
        assert!(action_entry(action(command), None).is_none());
    }

    #[test]
    fn the_return_type_comes_from_the_binding_key() {
        assert_eq!(
            return_type_of(
                &json!({"bindingKey": "Lsome/Type;.name()Lsome/Result;", "name": "name"})
            ),
            Some("Result".to_owned())
        );
        assert_eq!(
            return_type_of(&json!({"bindingKey": "Lsome/Type;.name()V"})),
            Some("void".to_owned())
        );
        assert_eq!(
            return_type_of(&json!({"bindingKey": "Lsome/Type;.name()[I"})),
            Some("int[]".to_owned())
        );
        assert_eq!(
            return_type_of(&json!({"bindingKey": "Lsome/Type;.name()TItem;"})),
            Some("Item".to_owned())
        );
        assert_eq!(
            return_type_of(&json!({"returnType": " String "})),
            Some("String".to_owned())
        );
        assert_eq!(return_type_of(&json!({"name": "name"})), None);
    }

    #[cfg(windows)]
    #[test]
    fn a_destination_carries_the_folder_of_its_uri() {
        let folder = destination_folder(&json!({
            "uri": "file:///E:/p/src/main/source/org/example/",
        }));
        assert_eq!(
            folder,
            Some(PathBuf::from(r"E:\p\src\main\source\org\example"))
        );
        assert_eq!(
            destination_folder(&json!({"displayName": "org.example"})),
            None
        );
    }

    #[test]
    fn the_type_of_a_name_comes_from_its_declaration() {
        let text = "    Map<String, Integer> names = ";
        assert_eq!(
            expected_type_from_declaration(text, text.len()),
            Some("Map<String, Integer>".to_owned())
        );
        let text = "    final String name = ";
        assert_eq!(
            expected_type_from_declaration(text, text.len()),
            Some("String".to_owned())
        );
        let text = "        return ";
        let method = "    String value() {\n        return ";
        assert_eq!(
            expected_type_from_declaration(method, method.len()),
            Some("String".to_owned())
        );
        assert_eq!(expected_type_from_declaration(text, text.len()), None);
        let comparison = "    if (left == ";
        assert_eq!(
            expected_type_from_declaration(comparison, comparison.len()),
            None
        );
    }

    #[test]
    fn a_parameter_type_is_the_part_before_its_name() {
        assert_eq!(parameter_type("String name"), Some("String".to_owned()));
        assert_eq!(parameter_type("int count"), Some("int".to_owned()));
        assert_eq!(
            parameter_type("Map<String, Integer> names"),
            Some("Map<String, Integer>".to_owned())
        );
    }

    #[test]
    fn signature_help_keeps_the_parameter_labels_and_the_active_parameter() {
        let help: lsp::SignatureHelp = serde_json::from_value(json!({
            "signatures": [
                {
                    "label": "call(String one, int two) : void",
                    "parameters": [
                        {"label": "String one"},
                        {"label": "int two"}
                    ]
                }
            ],
            "activeSignature": 0,
            "activeParameter": 1
        }))
        .unwrap();
        let info = signature_help_info(help);
        assert_eq!(info.active_parameter, 1);
        assert_eq!(info.signatures[0].parameters.len(), 2);
        assert_eq!(info.signatures[0].parameters[1].label, "int two");
    }

    #[test]
    fn the_first_web_address_of_documentation_is_taken() {
        assert_eq!(
            documentation_url("See [the type](https://docs.example/Type.html?x=1) here"),
            Some("https://docs.example/Type.html?x=1".to_owned())
        );
        assert_eq!(documentation_url("no link"), None);
    }

    #[test]
    fn only_the_language_says_which_line_is_an_import() {
        let span = TextSpan {
            start: TextPoint {
                line: 0,
                character: 0,
            },
            end: TextPoint {
                line: 0,
                character: 1,
            },
        };
        let import = |line: &str| line.trim_start().starts_with("import ");
        assert_eq!(
            usage_kind("import a.B;", &span, None, false, import),
            lang_api::UsageKind::Import
        );
        assert_eq!(
            usage_kind("import a.B;", &span, None, false, |_| false),
            lang_api::UsageKind::Unknown,
            "a language without imports knows none"
        );
    }

    #[test]
    fn a_hint_answer_keeps_the_place_the_kind_and_the_padding() {
        let hints: Vec<lsp::InlayHint> = serde_json::from_value(json!([
            {
                "position": {"line": 4, "character": 13},
                "label": "flag:",
                "kind": 2,
                "paddingRight": true
            },
            {
                "position": {"line": 4, "character": 19},
                "label": [{"value": "count"}, {"value": ":"}],
                "kind": 2
            },
            {
                "position": {"line": 2, "character": 8},
                "label": "String",
                "kind": 1
            },
            {
                "position": {"line": 2, "character": 9},
                "label": "  "
            }
        ]))
        .unwrap();
        let entries = inlay_hints(Some(hints));
        assert_eq!(entries.len(), 3, "a hint of only spaces says nothing");
        assert_eq!(
            (entries[0].position.line, entries[0].position.character),
            (4, 13)
        );
        assert_eq!(entries[0].label, "flag:");
        assert_eq!(entries[0].kind, lang_api::InlayHintKind::Parameter);
        assert!(entries[0].padding_right && !entries[0].padding_left);
        assert_eq!(entries[1].label, "count:", "a label of parts is one text");
        assert_eq!(entries[2].kind, lang_api::InlayHintKind::Type);
        assert!(inlay_hints(None).is_empty());
    }

    #[test]
    fn a_call_is_known_by_its_name_or_by_its_whole_invocation() {
        let line = "        return probe.sum(1, 2);";
        let name = TextSpan {
            start: TextPoint {
                line: 3,
                character: 21,
            },
            end: TextPoint {
                line: 3,
                character: 24,
            },
        };
        let invocation = TextSpan {
            end: TextPoint {
                line: 3,
                character: 30,
            },
            ..name
        };
        assert_eq!(
            usage_kind(line, &name, None, false, |_| false),
            lang_api::UsageKind::Call
        );
        assert_eq!(
            usage_kind(line, &invocation, None, false, |_| false),
            lang_api::UsageKind::Call,
            "the server may name the whole invocation"
        );
    }

    #[test]
    fn a_diagnostic_tagged_unnecessary_is_greyed_out_by_the_editor() {
        let tag = json!({
            "range": {
                "start": {"line": 0, "character": 0},
                "end": {"line": 0, "character": 4},
            },
            "severity": 2,
            "message": "the value is never used",
            "tags": [1],
        });
        let marked: lsp::Diagnostic = serde_json::from_value(tag).unwrap();
        assert!(diagnostic(marked).unnecessary);

        let plain = json!({
            "range": {
                "start": {"line": 0, "character": 0},
                "end": {"line": 0, "character": 4},
            },
            "severity": 2,
            "message": "cannot resolve",
        });
        let plain: lsp::Diagnostic = serde_json::from_value(plain).unwrap();
        assert!(!diagnostic(plain).unnecessary);
    }
}
