//! The semantic tokens a server declares in its answer to `initialize`.
//!
//! The legend says what the numbers of a token answer mean, so it is read once and
//! kept for the requests of the same server.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use lsp_types::{SemanticTokensServerCapabilities, ServerCapabilities};

/// The legend of a server and the requests it answers for semantic tokens.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SemanticTokensCapability {
    pub token_types: Vec<String>,
    pub token_modifiers: Vec<String>,
    /// The server colours a range of a document.
    pub range: bool,
    /// The server colours a whole document.
    pub full: bool,
}

impl SemanticTokensCapability {
    /// Reads `semanticTokensProvider` of the server capabilities. `None` when the server
    /// has no semantic tokens.
    pub fn from_server(capabilities: &ServerCapabilities) -> Option<Self> {
        let options = match capabilities.semantic_tokens_provider.as_ref()? {
            SemanticTokensServerCapabilities::SemanticTokensOptions(options) => options,
            SemanticTokensServerCapabilities::SemanticTokensRegistrationOptions(options) => {
                &options.semantic_tokens_options
            }
        };
        Some(Self {
            token_types: options
                .legend
                .token_types
                .iter()
                .map(|token| token.as_str().to_owned())
                .collect(),
            token_modifiers: options
                .legend
                .token_modifiers
                .iter()
                .map(|modifier| modifier.as_str().to_owned())
                .collect(),
            range: options.range.unwrap_or(false),
            full: options.full.is_some(),
        })
    }
}

/// The capability of the running server, filled when `initialize` answers and read by
/// the token requests. A new answer replaces the old one.
#[derive(Clone, Default)]
pub struct SemanticTokensSetup(Arc<Mutex<Option<SemanticTokensCapability>>>);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl SemanticTokensSetup {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&self, capability: Option<SemanticTokensCapability>) {
        *lock(&self.0) = capability;
    }

    pub fn get(&self) -> Option<SemanticTokensCapability> {
        lock(&self.0).clone()
    }
}

#[cfg(test)]
mod tests {
    use lsp_types::ServerCapabilities;
    use serde_json::json;

    use super::SemanticTokensCapability;

    fn server(value: serde_json::Value) -> ServerCapabilities {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn the_legend_and_the_requests_come_from_the_server() {
        let capability = SemanticTokensCapability::from_server(&server(json!({
            "semanticTokensProvider": {
                "legend": {
                    "tokenTypes": ["class", "method", "property"],
                    "tokenModifiers": ["static", "readonly", "deprecated"],
                },
                "range": true,
                "full": true,
            }
        })))
        .unwrap();
        assert_eq!(capability.token_types, ["class", "method", "property"]);
        assert_eq!(
            capability.token_modifiers,
            ["static", "readonly", "deprecated"]
        );
        assert!(capability.range);
        assert!(capability.full);
    }

    #[test]
    fn a_server_without_tokens_has_no_legend() {
        assert_eq!(
            SemanticTokensCapability::from_server(&server(json!({}))),
            None
        );
        let only_range = SemanticTokensCapability::from_server(&server(json!({
            "semanticTokensProvider": {
                "legend": {"tokenTypes": [], "tokenModifiers": []},
                "range": true,
            }
        })))
        .unwrap();
        assert!(only_range.range);
        assert!(!only_range.full);
    }
}
