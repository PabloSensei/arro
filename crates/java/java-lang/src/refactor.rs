//! Refactorings of the server that take values from the user before it computes any
//! edit: the members and the name of a new interface, the place a field starts.

use std::sync::Arc;

use base::Cancel;
use lang_api::{
    ActionApplyRequest, ActionChoice, ActionField, ActionMember, ActionOutcome, ActionParameters,
    ActionParametersQuery, Answer, Confidence, Provider, Reply, SourceId,
};
use lsp_client::lsp_types;
use semantic_router::{Router, workspace_change};
use serde_json::{Value, json};
use tokio::runtime::Handle;

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
