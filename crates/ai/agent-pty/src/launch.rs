//! How a harness starts: its arguments and environment. The product runs it in the real home of
//! the user, that is the point of a native TUI. Tests pass a throwaway environment instead.

use std::ffi::OsString;

use agent_core::{Harness, SessionSpec, ToolEndpoint};
use serde_json::json;
use terminal::{ShiftEnter, allowlisted};

/// `ide` itself is taken in Claude: the built-in IDE channel owns that name.
pub const MCP_SERVER_NAME: &str = "arro";

/// Carries the session token into the child, so that it is never written to disk.
pub const TOKEN_VARIABLE: &str = "IDE_MCP_TOKEN";

/// Read-only tools the agent may call without a question. A test keeps it equal to the list of
/// the gateway, which this crate does not depend on.
const READ_TOOLS: &[&str] = &[
    "get_editor_context",
    "find_symbol",
    "get_symbol_info",
    "get_diagnostics",
    "debug_get_state",
];

/// Prefixes a harness needs from the environment of the user: keys, gateways, cloud providers.
const HARNESS_PREFIXES: &[&str] = &[
    "ANTHROPIC_",
    "AWS_",
    "AZURE_",
    "CLAUDE_",
    "CODEX_",
    "GOOGLE_",
    "OPENAI_",
    "OPENCODE_",
    "VERTEX_",
];

const HARNESS_VARIABLES: &[&str] = &[
    "HOME",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "NODE_EXTRA_CA_CERTS",
    "SSH_AUTH_SOCK",
];

/// Where a harness runs.
#[derive(Clone, Debug)]
pub enum Launch {
    /// The home and login of the user. `inherit` names what the tools of the project need,
    /// which the language layer knows and this crate does not. `set` are values the
    /// project itself decides, such as the SDK of its build.
    UserHome {
        inherit: Vec<String>,
        set: Vec<(String, String)>,
    },
    /// A complete prepared environment. `marker` has to be on screen before any input.
    Prepared {
        env: Vec<(String, String)>,
        marker: Option<String>,
    },
}

impl Launch {
    pub(crate) fn marker(&self) -> Option<&str> {
        match self {
            Self::UserHome { .. } => None,
            Self::Prepared { marker, .. } => marker.as_deref(),
        }
    }

    pub(crate) fn environment(&self, spec: &SessionSpec) -> Vec<(String, String)> {
        let mut env = match self {
            Self::UserHome { inherit, set } => {
                let mut env = user_environment(std::env::vars_os(), inherit);
                for (key, value) in set {
                    env.retain(|(known, _)| !known.eq_ignore_ascii_case(key));
                    env.push((key.clone(), value.clone()));
                }
                env
            }
            Self::Prepared { env, .. } => env.clone(),
        };
        env.push(("TERM_PROGRAM".to_string(), "arro".to_string()));
        if let Some(tools) = &spec.tools {
            env.push((TOKEN_VARIABLE.to_string(), tools.token.clone()));
        }
        env
    }
}

fn user_environment(
    variables: impl Iterator<Item = (OsString, OsString)>,
    inherit: &[String],
) -> Vec<(String, String)> {
    let mut env = allowlisted();
    for (key, value) in variables {
        let (Ok(key), Ok(value)) = (key.into_string(), value.into_string()) else {
            continue;
        };
        let upper = key.to_uppercase();
        let named = HARNESS_VARIABLES.contains(&upper.as_str());
        let prefixed = HARNESS_PREFIXES
            .iter()
            .any(|prefix| upper.starts_with(prefix));
        let asked = inherit.iter().any(|name| name.eq_ignore_ascii_case(&key));
        if named || prefixed || asked {
            env.push((key, value));
        }
    }
    env
}

fn claude_mcp_config(url: &str) -> String {
    let server = json!({
        "type": "http",
        "url": url,
        "headers": {"Authorization": format!("Bearer ${{{TOKEN_VARIABLE}}}")},
    });
    json!({"mcpServers": {MCP_SERVER_NAME: server}}).to_string()
}

/// A TOML string, which is what `-c` values of Codex are.
fn toml_string(value: &str) -> String {
    serde_json::to_string(value).expect("a string always serializes")
}

/// Arguments that connect a harness to the tools of the IDE.
pub fn launch_arguments(harness: Harness, spec: &SessionSpec) -> Vec<String> {
    let mut arguments = always(harness);
    if let Some(tools) = &spec.tools {
        arguments.extend(tool_arguments(harness, tools));
    }
    arguments
}

/// Two sessions on one home would share a daemon that lives in the job of the first one, and
/// closing the first would take it away from the second.
fn always(harness: Harness) -> Vec<String> {
    match harness {
        Harness::Codex => vec!["--no-daemon".to_string()],
        Harness::Claude | Harness::OpenCode => Vec::new(),
    }
}

fn tool_arguments(harness: Harness, tools: &ToolEndpoint) -> Vec<String> {
    match harness {
        Harness::Claude => {
            let allowed = READ_TOOLS
                .iter()
                .map(|tool| format!("mcp__{MCP_SERVER_NAME}__{tool}"));
            let mut arguments = vec!["--mcp-config".to_string(), claude_mcp_config(&tools.url)];
            arguments.push("--allowedTools".to_string());
            arguments.push(allowed.collect::<Vec<_>>().join(","));
            arguments
        }
        Harness::Codex => {
            let url = toml_string(&tools.url);
            let token = toml_string(TOKEN_VARIABLE);
            let server = format!("mcp_servers.{MCP_SERVER_NAME}");
            vec![
                "-c".to_string(),
                format!("{server}.url={url}"),
                "-c".to_string(),
                format!("{server}.bearer_token_env_var={token}"),
            ]
        }
        Harness::OpenCode => Vec::new(),
    }
}

pub(crate) fn shift_enter(harness: Harness) -> ShiftEnter {
    match harness {
        Harness::Claude => ShiftEnter::CsiU,
        Harness::Codex | Harness::OpenCode => ShiftEnter::Win32Record,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> SessionSpec {
        SessionSpec {
            project: "project".into(),
            cwd: "project".into(),
            cols: 80,
            rows: 24,
            tools: Some(ToolEndpoint {
                url: "http://127.0.0.1:1/mcp".to_string(),
                token: "secret-token".to_string(),
            }),
        }
    }

    #[test]
    fn the_token_stays_out_of_the_command_line() {
        for harness in [Harness::Claude, Harness::Codex] {
            let line = launch_arguments(harness, &spec()).join(" ");

            assert!(line.contains("127.0.0.1"));
            assert!(!line.contains("secret-token"));
        }
    }

    #[test]
    fn the_token_reaches_the_child_through_its_environment() {
        let launch = Launch::Prepared {
            env: Vec::new(),
            marker: None,
        };

        let env = launch.environment(&spec());

        let token = env.iter().find(|(key, _)| key == TOKEN_VARIABLE);
        assert_eq!(token.map(|(_, value)| value.as_str()), Some("secret-token"));
    }

    #[test]
    fn a_harness_gets_its_keys_and_what_was_asked_for_and_nothing_else() {
        let variables = [
            ("ANTHROPIC_API_KEY", "a"),
            ("my_tool_home", "b"),
            ("HTTPS_PROXY", "c"),
            ("GITHUB_TOKEN", "d"),
        ];
        let variables = variables.map(|(key, value)| (key.into(), value.into()));

        let env = user_environment(variables.into_iter(), &["MY_TOOL_HOME".to_string()]);

        let has = |name: &str| env.iter().any(|(key, _)| key == name);
        assert!(has("ANTHROPIC_API_KEY"));
        assert!(has("my_tool_home"));
        assert!(has("HTTPS_PROXY"));
        assert!(!has("GITHUB_TOKEN"));
    }

    #[test]
    fn no_tools_means_no_arguments() {
        let spec = SessionSpec {
            tools: None,
            ..spec()
        };

        assert!(launch_arguments(Harness::Claude, &spec).is_empty());
    }

    #[test]
    fn every_tool_of_the_gateway_is_allowed() {
        assert_eq!(READ_TOOLS, tool_gateway::TOOL_NAMES);
    }

    #[test]
    fn the_server_name_is_not_the_reserved_one() {
        assert_ne!(MCP_SERVER_NAME, "ide");
    }
}
