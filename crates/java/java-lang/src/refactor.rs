//! Refactorings that take values from the user before they compute any edit: the
//! members and the name of a new interface, the place a field starts, and the four
//! refactorings that move members or make a type from them.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use base::Cancel;
use futures_util::StreamExt;
use lang_api::{
    ActionApplyRequest, ActionChoice, ActionField, ActionMember, ActionOutcome, ActionParameters,
    ActionParametersQuery, Answer, Confidence, DocumentSpot, MemberActionForm, MemberActionQuery,
    MemberActionRequest, MemberRefactoring, Provider, Reply, SourceId, SourceSpan,
};
use lsp_client::lsp_types;
use lsp_client::uri::file_path;
use semantic_router::{LspProviders, Router, workspace_change};
use serde_json::{Value, json};
use tokio::runtime::Handle;

use crate::members::{self, CallSite, SourceFile, TypeFile};

const SOURCE: SourceId = SourceId::new("jdtls");

/// The request that describes the members and the packages of a new interface.
const CHECK_INTERFACE: &str = "java/checkExtractInterfaceStatus";
/// The request that turns the values of a refactoring into its edits.
const REFACTOR_EDITS: &str = "java/getRefactorEdit";

const EXTRACT_INTERFACE: &str = "extractInterface";
const EXTRACT_FIELD: &str = "extractField";
const CONVERT_VARIABLE_TO_FIELD: &str = "convertVariableToField";

const NO_ANSWER: &str = "The language server did not answer";
const NO_EDITS: &str = "The server returned no edits";

/// The refactorings of jdtls whose form the window shows.
pub(crate) struct RefactorForms {
    router: Arc<Router>,
    runtime: Handle,
}

impl RefactorForms {
    pub(crate) fn new(router: Arc<Router>, runtime: Handle) -> Self {
        Self { router, runtime }
    }
}

/// The command of an entry's handle, the code action context of it, its extra
/// arguments and the title of its row.
struct Command {
    name: String,
    context: Value,
    extra: Vec<Value>,
    title: String,
}

fn command_of(handle: &str) -> Option<Command> {
    let value: Value = serde_json::from_str(handle).ok()?;
    let title = value
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let arguments = value.get("arguments").and_then(Value::as_array)?;
    Some(Command {
        name: arguments.first()?.as_str()?.to_owned(),
        context: arguments.get(1)?.clone(),
        extra: arguments.get(2..).unwrap_or_default().to_vec(),
        title,
    })
}

/// The title of the form, without the dots the row of the server ends it with.
fn clean(title: &str) -> String {
    title.trim_end_matches('.').trim_end().to_owned()
}

/// The members of the interface, its name and the packages it may go to.
async fn interface_form(router: &Router, command: &Command) -> Option<ActionParameters> {
    let reply = router
        .extension(CHECK_INTERFACE, command.context.clone())
        .await
        .ok()?;
    let members = reply
        .get("members")
        .and_then(Value::as_array)
        .map(|members| {
            members
                .iter()
                .filter_map(|member| {
                    Some(ActionMember {
                        name: member.get("name")?.as_str()?.to_owned(),
                        detail: member
                            .get("typeName")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                        checked: true,
                        // The server picks the members by their handles, not by name.
                        value: member.get("handleIdentifier")?.as_str()?.to_owned(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let type_name = reply
        .get("subTypeName")
        .and_then(Value::as_str)
        .unwrap_or("Interface");
    let destinations: Vec<Value> = reply
        .get("destinationResponse")
        .and_then(|response| response.get("destinations"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let destination = |value: &Value| ActionChoice {
        name: value
            .get("displayName")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        detail: value.get("path").and_then(Value::as_str).map(str::to_owned),
        value: value.to_string(),
    };
    let mut choices: Vec<ActionChoice> = destinations.iter().map(destination).collect();
    // The package of the type comes first, as the server marks it.
    if let Some(index) = destinations.iter().position(|value| {
        value.get("isParentOfSelectedFile").and_then(Value::as_bool) == Some(true)
    }) && let Some(choice) = choices.get(index).cloned()
    {
        choices.remove(index);
        choices.insert(0, choice);
    }
    Some(ActionParameters {
        title: clean(&command.title),
        members,
        members_label: "Members".to_owned(),
        fields: vec![ActionField {
            key: "name".to_owned(),
            label: "Interface name".to_owned(),
            value: format!("{type_name}Interface"),
        }],
        choices,
        choices_label: "Destination".to_owned(),
    })
}

/// The places the field may start in, which the server names as the arguments of
/// the command.
fn scope_form(command: &Command) -> Option<ActionParameters> {
    let scopes = command
        .extra
        .first()?
        .get("initializedScopes")?
        .as_array()?;
    Some(ActionParameters {
        title: clean(&command.title),
        members: Vec::new(),
        members_label: String::new(),
        fields: Vec::new(),
        choices: scopes
            .iter()
            .filter_map(Value::as_str)
            .map(|scope| ActionChoice {
                name: scope.to_owned(),
                detail: None,
                value: Value::String(scope.to_owned()).to_string(),
            })
            .collect(),
        choices_label: "Initialize in".to_owned(),
    })
}

impl Provider<ActionParametersQuery, Option<ActionParameters>> for RefactorForms {
    fn start(
        &self,
        query: ActionParametersQuery,
        _: Cancel,
        mut reply: Reply<Option<ActionParameters>>,
    ) {
        let router = self.router.clone();
        self.runtime.spawn(async move {
            let form = match command_of(&query.0) {
                Some(command) if command.name == EXTRACT_INTERFACE => {
                    interface_form(&router, &command).await
                }
                Some(command)
                    if command.name == EXTRACT_FIELD
                        || command.name == CONVERT_VARIABLE_TO_FIELD =>
                {
                    scope_form(&command)
                }
                _ => None,
            };
            let source = match form {
                Some(form) => Answer::new(Some(form), Confidence::Certain, SOURCE),
                None => Answer::new(None, Confidence::Unknown, SOURCE),
            };
            let _ = reply.send(source);
        });
    }
}

fn outcome(error: &str) -> ActionOutcome {
    ActionOutcome {
        change: Default::default(),
        error: Some(error.to_owned()),
    }
}

impl Provider<ActionApplyRequest, ActionOutcome> for RefactorForms {
    fn start(&self, query: ActionApplyRequest, _: Cancel, mut reply: Reply<ActionOutcome>) {
        let router = self.router.clone();
        self.runtime.spawn(async move {
            let answer = apply(&router, &query).await;
            let _ = reply.send(Answer::new(answer, Confidence::Certain, SOURCE));
        });
    }
}

async fn apply(router: &Router, request: &ActionApplyRequest) -> ActionOutcome {
    let Some(command) = command_of(&request.handle) else {
        return outcome("The refactoring is not known");
    };
    let arguments = match command.name.as_str() {
        EXTRACT_INTERFACE => {
            let name = request
                .fields
                .iter()
                .find(|field| field.key == "name")
                .map(|field| field.value.trim())
                .unwrap_or_default();
            if name.is_empty() {
                return outcome("The name is empty");
            }
            let Some(destination) = request
                .choice
                .as_deref()
                .and_then(|value| serde_json::from_str::<Value>(value).ok())
            else {
                return outcome("No package is chosen");
            };
            let members =
                Value::Array(request.members.iter().cloned().map(Value::String).collect());
            vec![members, Value::String(name.to_owned()), destination]
        }
        EXTRACT_FIELD | CONVERT_VARIABLE_TO_FIELD => {
            let Some(scope) = request
                .choice
                .as_deref()
                .and_then(|value| serde_json::from_str::<Value>(value).ok())
            else {
                return outcome("No place to initialize the field is chosen");
            };
            vec![scope]
        }
        _ => return outcome("The language server has no such refactoring"),
    };
    let params = json!({
        "command": command.name,
        "commandArguments": arguments,
        "context": command.context,
        "options": {"tabSize": 4, "insertSpaces": true},
    });
    let Ok(reply) = router.extension(REFACTOR_EDITS, params).await else {
        return outcome(NO_ANSWER);
    };
    if let Some(error) = reply
        .get("errorMessage")
        .and_then(Value::as_str)
        .filter(|error| !error.is_empty())
    {
        return outcome(error);
    }
    let Some(edit) = reply
        .get("edit")
        .and_then(|edit| serde_json::from_value::<lsp_types::WorkspaceEdit>(edit.clone()).ok())
    else {
        return outcome(NO_EDITS);
    };
    ActionOutcome {
        change: workspace_change(edit),
        error: None,
    }
}

/// Pull Members Up, Push Members Down, Extract Superclass and Introduce Parameter
/// Object: the form the window shows and the edits of the filled form.
#[derive(Clone)]
pub(crate) struct MemberRefactorings {
    router: Arc<Router>,
    runtime: Handle,
    root: PathBuf,
    providers: Arc<LspProviders>,
}

impl MemberRefactorings {
    pub(crate) fn new(
        root: PathBuf,
        router: Arc<Router>,
        providers: Arc<LspProviders>,
        runtime: Handle,
    ) -> Self {
        Self {
            router,
            runtime,
            root,
            providers,
        }
    }

    fn text_of_uri(&self, uri: &str) -> Option<(PathBuf, String)> {
        let parsed = lsp_client::lsp_types::Uri::from_str(uri).ok()?;
        let path = file_path(&parsed)?;
        let text = self
            .router
            .text(&parsed)
            .map(|text| text.to_string())
            .or_else(|| std::fs::read_to_string(&path).ok())?;
        Some((path, text))
    }

    fn file(&self, uri: &str) -> Option<SourceFile> {
        let (_, text) = self.text_of_uri(uri)?;
        Some(SourceFile {
            uri: uri.to_owned(),
            text,
        })
    }

    /// The file a supertype of the source is declared in, through the import when the
    /// source has one and by a walk of the project otherwise.
    fn type_file(&self, source_path: &Path, source_text: &str, name: &str) -> Option<TypeFile> {
        let package = members::package_name(source_text).unwrap_or_default();
        let root = source_root(source_path, &package);
        let mut candidates = Vec::new();
        if let Some(imported) = imported_package(source_text, name) {
            candidates.push(
                root.join(imported.replace('.', std::path::MAIN_SEPARATOR_STR))
                    .join(format!("{name}.java")),
            );
        }
        if let Some(parent) = source_path.parent() {
            candidates.push(parent.join(format!("{name}.java")));
        }
        candidates.push(root.join(format!("{name}.java")));
        for candidate in candidates {
            if let Ok(text) = std::fs::read_to_string(&candidate) {
                return Some(TypeFile {
                    name: name.to_owned(),
                    uri: uri_of_path(&candidate)?,
                    package: members::package_name(&text),
                    text,
                });
            }
        }
        members::java_files(&self.root)
            .into_iter()
            .filter(|path| {
                path.file_stem()
                    .and_then(|stem| stem.to_str())
                    .is_some_and(|stem| stem == name)
            })
            .find_map(|path| {
                let text = std::fs::read_to_string(&path).ok()?;
                Some(TypeFile {
                    name: name.to_owned(),
                    uri: uri_of_path(&path)?,
                    package: members::package_name(&text),
                    text,
                })
            })
    }

    /// The supertypes of the source up its hierarchy, the nearest first.
    fn supertype_files(&self, source_path: &Path, source_text: &str) -> Vec<TypeFile> {
        let mut found = Vec::new();
        let mut seen = HashSet::new();
        let mut queue: Vec<(PathBuf, String)> =
            vec![(source_path.to_path_buf(), source_text.to_owned())];
        while let Some((path, text)) = queue.pop() {
            for name in members::direct_supertypes(&text) {
                if !seen.insert(name.clone()) {
                    continue;
                }
                let Some(file) = self.type_file(&path, &text, &name) else {
                    continue;
                };
                let next = lsp_client::lsp_types::Uri::from_str(&file.uri)
                    .ok()
                    .and_then(|uri| file_path(&uri))
                    .unwrap_or_else(|| path.clone());
                queue.push((next, file.text.clone()));
                found.push(file);
            }
        }
        found
    }

    /// The direct subtypes of the source type in the project.
    fn subtypes(&self, source_path: &Path, source_name: &str) -> Vec<TypeFile> {
        members::java_files(&self.root)
            .into_iter()
            .filter(|path| path != source_path)
            .filter_map(|path| {
                let text = std::fs::read_to_string(&path).ok()?;
                if !members::extends_or_implements(&text, source_name) {
                    return None;
                }
                Some(TypeFile {
                    name: members::type_name(&text)?,
                    uri: uri_of_path(&path)?,
                    package: members::package_name(&text),
                    text,
                })
            })
            .collect()
    }
}

fn uri_of_path(path: &Path) -> Option<String> {
    Some(platform::CanonPath::new(path).ok()?.to_uri())
}

/// The folder a package of `path` starts under.
fn source_root(path: &Path, package: &str) -> PathBuf {
    let mut root = path.parent().map(Path::to_path_buf).unwrap_or_default();
    for _ in package.split('.').filter(|part| !part.is_empty()) {
        match root.parent() {
            Some(parent) => root = parent.to_path_buf(),
            None => return PathBuf::new(),
        }
    }
    root
}

/// The package of an import of `name`, when the file imports it.
fn imported_package(text: &str, name: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| line.starts_with("import ") && !line.starts_with("import static "))
        .find(|line| {
            line.trim_end_matches(';')
                .rsplit('.')
                .next()
                .is_some_and(|last| last == name)
        })
        .and_then(|line| {
            let raw = line["import ".len()..].trim_end_matches(';');
            raw.rsplit_once('.').map(|(package, _)| package.to_owned())
        })
}

fn value_of<'a>(request: &'a MemberActionRequest, key: &str) -> &'a str {
    request
        .fields
        .iter()
        .find(|field| field.key == key)
        .map_or("", |field| field.value.trim())
}

fn done(change: lang_api::WorkspaceChange) -> ActionOutcome {
    ActionOutcome {
        change,
        error: None,
    }
}

impl Provider<MemberActionQuery, Option<MemberActionForm>> for MemberRefactorings {
    fn start(
        &self,
        query: MemberActionQuery,
        _: Cancel,
        mut reply: Reply<Option<MemberActionForm>>,
    ) {
        let this = self.clone();
        self.runtime.spawn(async move {
            let form = this.form(&query).await;
            let confidence = if form.is_some() {
                Confidence::Certain
            } else {
                Confidence::Unknown
            };
            let _ = reply.send(Answer::new(form, confidence, SOURCE));
        });
    }
}

impl MemberRefactorings {
    async fn form(&self, query: &MemberActionQuery) -> Option<MemberActionForm> {
        let (path, text) = self.text_of_uri(&query.spot.uri)?;
        let offset = lang_api::offset_in(&text, query.spot.point);
        let source = SourceFile {
            uri: query.spot.uri.clone(),
            text,
        };
        match query.refactoring {
            MemberRefactoring::PullUp => {
                let targets = self.supertype_files(&path, &source.text);
                members::pull_up_form(&source, offset, &targets)
            }
            MemberRefactoring::PushDown => {
                let name = members::type_name_at(&source.text, offset)?;
                let targets = self.subtypes(&path, &name);
                members::push_down_form(&source, offset, &targets)
            }
            MemberRefactoring::ExtractSuperclass => {
                let name = members::type_name_at(&source.text, offset)?;
                let package = members::package_name(&source.text).unwrap_or_default();
                let root = source_root(&path, &package);
                let mut packages: Vec<String> = members::java_files(&root)
                    .into_iter()
                    .filter_map(|file| std::fs::read_to_string(file).ok())
                    .filter_map(|text| members::package_name(&text))
                    .collect();
                packages.sort();
                packages.dedup();
                packages.retain(|found| *found != package);
                packages.insert(0, package);
                members::extract_superclass_form(
                    &source,
                    offset,
                    &format!("Abstract{name}"),
                    &packages,
                )
            }
            MemberRefactoring::IntroduceParameterObject => {
                members::parameter_object_form(&source, offset)
            }
        }
    }

    async fn apply(&self, request: &MemberActionRequest) -> ActionOutcome {
        let Some((path, text)) = self.text_of_uri(&request.spot.uri) else {
            return outcome("The file is not open");
        };
        let source = SourceFile {
            uri: request.spot.uri.clone(),
            text,
        };
        let offset = lang_api::offset_in(&source.text, request.spot.point);
        let result = match request.refactoring {
            MemberRefactoring::PullUp => {
                let Some(target) = request.target.as_deref() else {
                    return outcome("No supertype is chosen");
                };
                let Some(target_file) = self.file(target) else {
                    return outcome("The supertype file is not available");
                };
                members::pull_up_apply(&source, &target_file, &request.members)
            }
            MemberRefactoring::PushDown => {
                let Some(name) = members::type_name_at(&source.text, offset) else {
                    return outcome("The file declares no type");
                };
                let targets: Vec<SourceFile> = self
                    .subtypes(&path, &name)
                    .into_iter()
                    .map(|target| SourceFile {
                        uri: target.uri,
                        text: target.text,
                    })
                    .collect();
                members::push_down_apply(&source, &targets, &request.members)
            }
            MemberRefactoring::ExtractSuperclass => {
                let name = value_of(request, "name");
                if name.is_empty() {
                    return outcome("The superclass name is empty");
                }
                let package = request.target.clone().unwrap_or_default();
                let root = source_root(&path, &package);
                let mut new_path = root;
                for part in package.split('.').filter(|part| !part.is_empty()) {
                    new_path.push(part);
                }
                new_path.push(format!("{name}.java"));
                if new_path.exists() {
                    return outcome("A file with that name is already there");
                }
                let Some(new_uri) = uri_of_path(&new_path) else {
                    return outcome("The package folder is not reachable");
                };
                members::extract_superclass_apply(
                    &source,
                    &new_uri,
                    name,
                    &package,
                    &request.members,
                )
            }
            MemberRefactoring::IntroduceParameterObject => {
                let name = value_of(request, "name");
                if name.is_empty() {
                    return outcome("The class name is empty");
                }
                let package = members::package_name(&source.text).unwrap_or_default();
                let mut new_path = path.parent().map(Path::to_path_buf).unwrap_or_default();
                new_path.push(format!("{name}.java"));
                if new_path.exists() {
                    return outcome("A file with that name is already there");
                }
                let Some(new_uri) = uri_of_path(&new_path) else {
                    return outcome("The package folder is not reachable");
                };
                let use_record = java_project::language_level(&self.root)
                    .and_then(|level| level.selected)
                    .and_then(|level| level.parse::<u32>().ok())
                    .is_some_and(|level| level >= 16);
                let calls = self.calls(&request.spot).await;
                members::parameter_object_apply(members::ParameterObjectRequest {
                    source: &source,
                    spot_offset: offset,
                    class_name: name,
                    package: &package,
                    new_uri: &new_uri,
                    use_record,
                    selections: &request.members,
                    calls: &calls,
                })
            }
        };
        match result {
            Ok(change) => done(change),
            Err(error) => outcome(&error),
        }
    }

    /// The places a call of the method at `spot` is in, by file, for the new arguments.
    async fn calls(&self, spot: &DocumentSpot) -> Vec<CallSite> {
        let provider: &dyn Provider<DocumentSpot, Vec<SourceSpan>> = self.providers.as_ref();
        let mut replies = lang_api::ask(provider, spot.clone());
        // A server that is off or still importing must not keep the form busy.
        let spans = tokio::time::timeout(Duration::from_secs(5), replies.next())
            .await
            .ok()
            .flatten()
            .map(|answer| answer.value)
            .unwrap_or_default();
        let mut calls: Vec<CallSite> = Vec::new();
        for span in spans {
            let Some((_, text)) = self.text_of_uri(&span.uri) else {
                continue;
            };
            let offset = lang_api::offset_in(&text, span.span.start);
            match calls.iter_mut().find(|call| call.uri == span.uri) {
                Some(call) => call.offsets.push(offset),
                None => calls.push(CallSite {
                    uri: span.uri,
                    text,
                    offsets: vec![offset],
                }),
            }
        }
        calls
    }
}

impl Provider<MemberActionRequest, ActionOutcome> for MemberRefactorings {
    fn start(&self, query: MemberActionRequest, _: Cancel, mut reply: Reply<ActionOutcome>) {
        let this = self.clone();
        self.runtime.spawn(async move {
            let answer = this.apply(&query).await;
            let _ = reply.send(Answer::new(answer, Confidence::Certain, SOURCE));
        });
    }
}

#[cfg(test)]
mod tests {
    use super::{clean, command_of};

    #[test]
    fn a_handle_carries_the_command_its_context_and_its_extra_arguments() {
        let handle = serde_json::json!({
            "title": "Extract interface...",
            "arguments": [
                "extractInterface",
                {"textDocument": {"uri": "file:///a/B.java"}, "range": {}, "context": {}},
            ],
        })
        .to_string();
        let command = command_of(&handle).unwrap();
        assert_eq!(command.name, "extractInterface");
        assert_eq!(command.title, "Extract interface...");
        assert_eq!(command.extra.len(), 0);
        assert_eq!(command.context["textDocument"]["uri"], "file:///a/B.java");
    }

    #[test]
    fn the_extra_arguments_of_a_field_hold_the_scopes() {
        let handle = serde_json::json!({
            "title": "Extract to field",
            "arguments": [
                "extractField",
                {"textDocument": {"uri": "file:///a/B.java"}},
                {"initializedScopes": ["Current method", "Field declaration"]},
            ],
        })
        .to_string();
        let command = command_of(&handle).unwrap();
        assert_eq!(command.extra.len(), 1);
        assert_eq!(clean(&command.title), "Extract to field");
    }
}
