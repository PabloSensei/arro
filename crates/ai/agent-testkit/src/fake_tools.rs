use tool_gateway::{
    Confidence, Diagnostic, DiagnosticsPage, DiagnosticsQuery, EditorContext, Location, Position,
    SessionScope, Severity, SymbolHit, SymbolInfo, SymbolPage, SymbolQuery, ToolBackend, ToolError,
};

/// Finds whatever is asked for: one class in `Found.txt` of the project.
pub struct FakeTools;

impl ToolBackend for FakeTools {
    fn editor_context(&self, scope: &SessionScope) -> Result<EditorContext, ToolError> {
        Ok(EditorContext {
            file: Some(scope.project.join("Found.txt")),
            line: Some(1),
            column: Some(1),
            selection: None,
            open_files: Vec::new(),
        })
    }

    fn find_symbol(
        &self,
        scope: &SessionScope,
        query: &SymbolQuery,
    ) -> Result<SymbolPage, ToolError> {
        let name = &query.query;
        Ok(SymbolPage {
            hits: vec![SymbolHit {
                name: name.clone(),
                kind: "class".to_string(),
                path: scope.project.join("Found.txt"),
                line: 2,
                content: format!("class {name} {{"),
                before: vec!["// a comment".to_string()],
                after: vec!["}".to_string()],
            }],
            source: "fake".to_string(),
            confidence: Confidence::Certain,
            next: None,
        })
    }

    fn symbol_info(
        &self,
        scope: &SessionScope,
        position: &Position,
    ) -> Result<SymbolInfo, ToolError> {
        Ok(SymbolInfo {
            name: "Widget".to_string(),
            kind: "class".to_string(),
            signature: Some(format!("class Widget, asked at line {}", position.line)),
            documentation: Some("A made-up widget.".to_string()),
            declaration: Some(Location {
                path: scope.project.join("Found.txt"),
                line: 2,
                content: "class Widget {".to_string(),
            }),
            source: "fake".to_string(),
            confidence: Confidence::Certain,
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
                path: path.unwrap_or_else(|| scope.project.join("Found.txt")),
                line: 2,
                column: 7,
                severity: Severity::Error,
                message: "made-up error".to_string(),
                code: None,
                content: "class Widget {".to_string(),
            }],
            source: "fake".to_string(),
            confidence: Confidence::Certain,
            next: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;
    use tool_gateway::{AuditLog, SessionScope, ToolGateway};

    use super::FakeTools;

    #[test]
    fn the_new_tools_answer_through_the_gateway() {
        let gateway = ToolGateway::new(Arc::new(FakeTools), AuditLog::default());
        let scope = SessionScope {
            session: "s".to_string(),
            project: "project".into(),
            cwd: "project".into(),
        };
        let at = json!({"path": "Found.txt", "line": 2, "column": 7});

        let info = gateway.call(&scope, "get_symbol_info", &at).expect("info");
        let errors = gateway
            .call(&scope, "get_diagnostics", &json!({"path": "Found.txt"}))
            .expect("diagnostics");

        assert!(info.starts_with("class Widget\n"), "{info}");
        assert!(info.contains("Found.txt:2:class Widget {"), "{info}");
        assert!(
            errors.contains("Found.txt:2:7: error: made-up error"),
            "{errors}"
        );
        let calls = gateway.audit().recent();
        assert!(calls.iter().all(|call| call.ok), "{calls:?}");
    }
}
