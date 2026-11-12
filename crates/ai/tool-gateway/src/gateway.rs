use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use serde::Deserialize;
use serde_json::Value;

use crate::audit::AuditLog;
use crate::backend::{
    DebugStateQuery, DiagnosticsQuery, Position, SessionScope, SymbolQuery, ToolBackend, ToolError,
};
use crate::render;
use crate::tools::{
    DEBUG_STATE, DIAGNOSTICS, EDITOR_CONTEXT, FIND_SYMBOL, SYMBOL_INFO, TOOLS, ToolClass, ToolSpec,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Allow,
    /// The user has to say yes. There is no way to ask from here yet, so the call is refused.
    Ask,
    Deny,
}

impl Decision {
    fn name(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Ask => "ask",
            Self::Deny => "deny",
        }
    }
}

/// Reads pass, whatever runs code of the project needs a yes from the user.
#[derive(Clone, Copy, Debug, Default)]
pub struct Policy;

impl Policy {
    pub fn decide(&self, spec: &ToolSpec) -> Decision {
        match spec.class {
            ToolClass::Read => Decision::Allow,
            ToolClass::Execute => Decision::Ask,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PositionInput {
    path: PathBuf,
    line: u32,
    column: u32,
}

fn parse<T: for<'de> Deserialize<'de>>(input: &Value) -> Result<T, ToolError> {
    serde_json::from_value(input.clone()).map_err(|error| ToolError::BadInput(error.to_string()))
}

/// A path of the agent inside the project, made absolute. `..` cannot leave the project, and
/// nothing is read here: the path need not exist yet.
fn resolve(scope: &SessionScope, path: &Path) -> Result<PathBuf, ToolError> {
    let mut resolved = PathBuf::new();
    for component in scope.project.join(path).components() {
        match component {
            Component::ParentDir => {
                resolved.pop();
            }
            Component::CurDir => {}
            other => resolved.push(other),
        }
    }
    let lower = |path: &Path| PathBuf::from(path.to_string_lossy().to_lowercase());
    if !lower(&resolved).starts_with(lower(&scope.project)) {
        let shown = path.display();
        return Err(ToolError::Denied(format!("{shown} is outside the project")));
    }
    Ok(resolved)
}

pub struct ToolGateway {
    backend: Arc<dyn ToolBackend>,
    policy: Policy,
    audit: AuditLog,
}

impl ToolGateway {
    pub fn new(backend: Arc<dyn ToolBackend>, audit: AuditLog) -> Self {
        Self {
            backend,
            policy: Policy,
            audit,
        }
    }

    pub fn tools(&self) -> &'static [ToolSpec] {
        &TOOLS
    }

    pub fn audit(&self) -> &AuditLog {
        &self.audit
    }

    /// Blocks while the service works, so it belongs on a background thread.
    pub fn call(
        &self,
        scope: &SessionScope,
        tool: &str,
        input: &Value,
    ) -> Result<String, ToolError> {
        let Some(spec) = TOOLS.iter().find(|spec| spec.name == tool) else {
            self.audit
                .record(&scope.session, tool, "unknown", "deny", false);
            return Err(ToolError::UnknownTool(tool.to_string()));
        };
        let class = spec.class.name();
        let decision = self.policy.decide(spec);
        if decision != Decision::Allow {
            let name = decision.name();
            self.audit.record(&scope.session, tool, class, name, false);
            return Err(ToolError::Denied(format!("{tool} needs permission")));
        }
        let result = self.dispatch(scope, tool, input);
        let name = decision.name();
        self.audit
            .record(&scope.session, tool, class, name, result.is_ok());
        result
    }

    fn dispatch(
        &self,
        scope: &SessionScope,
        tool: &str,
        input: &Value,
    ) -> Result<String, ToolError> {
        match tool {
            EDITOR_CONTEXT => {
                let context = self.backend.editor_context(scope)?;
                Ok(render::editor_context(&context))
            }
            FIND_SYMBOL => {
                let query: SymbolQuery = serde_json::from_value(input.clone())
                    .map_err(|error| ToolError::BadInput(error.to_string()))?;
                if query.query.trim().is_empty() {
                    return Err(ToolError::BadInput("the query is empty".to_string()));
                }
                let page = self.backend.find_symbol(scope, &query)?;
                Ok(render::symbols(&query, &page))
            }
            SYMBOL_INFO => {
                let input: PositionInput = parse(input)?;
                if input.line == 0 || input.column == 0 {
                    return Err(ToolError::BadInput(
                        "line and column start at 1".to_string(),
                    ));
                }
                let position = Position {
                    path: resolve(scope, &input.path)?,
                    line: input.line,
                    column: input.column,
                };
                let info = self.backend.symbol_info(scope, &position)?;
                Ok(render::symbol_info(&info))
            }
            DIAGNOSTICS => {
                let mut query: DiagnosticsQuery = parse(input)?;
                if let Some(path) = &query.path {
                    query.path = Some(resolve(scope, path)?);
                }
                let page = self.backend.diagnostics(scope, &query)?;
                Ok(render::diagnostics(&query, &page))
            }
            DEBUG_STATE => {
                let query: DebugStateQuery = parse::<DebugStateQuery>(input)?.clipped();
                let snapshot = self.backend.debug_snapshot(scope);
                Ok(render::debug_state(snapshot.as_deref(), &query))
            }
            _ => Err(ToolError::UnknownTool(tool.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::backend::{
        Confidence, Diagnostic, DiagnosticsPage, EditorContext, Severity, SymbolHit, SymbolInfo,
        SymbolPage,
    };

    struct Fake;

    impl ToolBackend for Fake {
        fn editor_context(&self, _scope: &SessionScope) -> Result<EditorContext, ToolError> {
            Ok(EditorContext {
                file: Some("src/Foo.txt".into()),
                line: Some(3),
                column: Some(7),
                ..EditorContext::default()
            })
        }

        fn find_symbol(
            &self,
            scope: &SessionScope,
            query: &SymbolQuery,
        ) -> Result<SymbolPage, ToolError> {
            Ok(SymbolPage {
                hits: vec![SymbolHit {
                    name: query.query.clone(),
                    kind: "class".to_string(),
                    path: scope.project.join("Foo.txt"),
                    line: 1,
                    content: "class Foo".to_string(),
                    before: Vec::new(),
                    after: Vec::new(),
                }],
                source: "fake".to_string(),
                confidence: Confidence::Certain,
                next: None,
            })
        }

        fn symbol_info(
            &self,
            _scope: &SessionScope,
            position: &Position,
        ) -> Result<SymbolInfo, ToolError> {
            Ok(SymbolInfo {
                name: format!("at {}", position.path.display()),
                kind: "method".to_string(),
                signature: None,
                documentation: None,
                declaration: None,
                source: "fake".to_string(),
                confidence: Confidence::Likely,
            })
        }

        fn diagnostics(
            &self,
            scope: &SessionScope,
            query: &DiagnosticsQuery,
        ) -> Result<DiagnosticsPage, ToolError> {
            let path = query.path.clone();
            Ok(DiagnosticsPage {
                items: vec![Diagnostic {
                    path: path.unwrap_or_else(|| scope.project.join("Foo.txt")),
                    line: 2,
                    column: 1,
                    severity: Severity::Error,
                    message: "broken".to_string(),
                    code: None,
                    content: "x".to_string(),
                }],
                source: "fake".to_string(),
                confidence: Confidence::Certain,
                next: None,
            })
        }
    }

    fn gateway() -> ToolGateway {
        ToolGateway::new(Arc::new(Fake), AuditLog::default())
    }

    fn scope() -> SessionScope {
        SessionScope {
            session: "s1".to_string(),
            project: "project".into(),
            cwd: "project".into(),
        }
    }

    #[test]
    fn a_read_tool_answers_and_leaves_a_trace() {
        let gateway = gateway();

        let text = gateway
            .call(&scope(), "find_symbol", &json!({"query": "Foo"}))
            .expect("call");

        assert!(text.contains(":1:class Foo"), "{text}");
        let audit = gateway.audit().recent();
        assert_eq!(audit.len(), 1);
        assert_eq!(audit[0].tool, "find_symbol");
        assert_eq!(audit[0].decision, "allow");
        assert!(audit[0].ok);
    }

    #[test]
    fn the_audit_log_does_not_keep_inputs() {
        let gateway = gateway();

        let _ = gateway.call(&scope(), "find_symbol", &json!({"query": "secret-value"}));

        let line = format!("{:?}", gateway.audit().recent());
        assert!(!line.contains("secret-value"));
    }

    #[test]
    fn the_editor_context_names_the_caret() {
        let text = gateway()
            .call(&scope(), "get_editor_context", &json!({}))
            .expect("call");

        assert!(text.starts_with("active: src/Foo.txt:3:7"), "{text}");
    }

    #[test]
    fn bad_input_and_unknown_tools_are_errors() {
        let gateway = gateway();

        let empty = gateway.call(&scope(), "find_symbol", &json!({"query": " "}));
        let missing = gateway.call(&scope(), "find_symbol", &json!({}));
        let unknown = gateway.call(&scope(), "rm", &json!({}));

        assert!(matches!(empty, Err(ToolError::BadInput(_))));
        assert!(matches!(missing, Err(ToolError::BadInput(_))));
        assert_eq!(unknown, Err(ToolError::UnknownTool("rm".to_string())));
    }

    #[test]
    fn symbol_info_gets_a_path_inside_the_project() {
        let gateway = gateway();
        let input = json!({"path": "src/./Foo.txt", "line": 3, "column": 9});

        let text = gateway
            .call(&scope(), "get_symbol_info", &input)
            .expect("call");

        let expected = Path::new("project").join("src").join("Foo.txt");
        assert!(text.contains(&expected.display().to_string()), "{text}");
    }

    #[cfg(windows)]
    #[test]
    fn paths_outside_the_project_are_refused() {
        let gateway = gateway();
        let input = json!({"path": "../project-other/secret.txt", "line": 1, "column": 1});
        let absolute = json!({"path": "C:/Windows/win.ini", "line": 1, "column": 1});

        let up = gateway.call(&scope(), "get_symbol_info", &input);
        let elsewhere = gateway.call(&scope(), "get_diagnostics", &json!({"path": "C:/x"}));
        let other = gateway.call(&scope(), "get_symbol_info", &absolute);

        assert!(matches!(up, Err(ToolError::Denied(_))), "{up:?}");
        assert!(
            matches!(elsewhere, Err(ToolError::Denied(_))),
            "{elsewhere:?}"
        );
        assert!(matches!(other, Err(ToolError::Denied(_))), "{other:?}");
    }

    #[test]
    fn diagnostics_take_a_severity_and_reject_zero_positions() {
        let gateway = gateway();

        let errors = gateway.call(&scope(), "get_diagnostics", &json!({"severity": "error"}));
        let bad = gateway.call(&scope(), "get_diagnostics", &json!({"severity": "fatal"}));
        let zero = gateway.call(
            &scope(),
            "get_symbol_info",
            &json!({"path": "a", "line": 0, "column": 1}),
        );

        let errors = errors.expect("call");
        assert!(errors.contains("error and worse"), "{errors}");
        assert!(errors.contains(":2:1: error: broken"), "{errors}");
        assert!(matches!(bad, Err(ToolError::BadInput(_))));
        assert!(matches!(zero, Err(ToolError::BadInput(_))));
    }

    #[test]
    fn running_code_needs_a_yes() {
        let spec = ToolSpec {
            name: "run",
            description: "",
            class: ToolClass::Execute,
            input_schema: Value::Null,
        };

        assert_eq!(Policy.decide(&spec), Decision::Ask);
    }
}
