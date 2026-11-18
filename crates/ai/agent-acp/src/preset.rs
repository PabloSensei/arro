//! What to run for an agent. The registry adapters of Codex and Claude Agent ship through npm,
//! so they need Node; we do not bundle it and say so plainly.

use std::ffi::OsStr;
use std::path::PathBuf;

/// Versions stay pinned together with the harness they wrap, see section 7 of the architecture.
const CODEX_PACKAGE: &str = "@agentclientprotocol/codex-acp@2.5.0";
const CLAUDE_PACKAGE: &str = "@agentclientprotocol/claude-agent-acp@0.100.0";

/// The npm runner, which is a batch file on Windows and a link of the package manager on Unix.
#[cfg(windows)]
const NPX: &str = "npx.cmd";
#[cfg(unix)]
const NPX: &str = "npx";

/// The shell of this system, which a whole command line goes through.
#[cfg(windows)]
const SHELL: &str = "cmd.exe";
#[cfg(unix)]
const SHELL: &str = "sh";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Preset {
    Codex,
    /// Runs on the Agent SDK, which takes an API key and not a subscription login.
    ClaudeAgent,
    /// A command line of the user's choice.
    Custom(String),
}

impl Preset {
    pub fn title(&self) -> &str {
        match self {
            Self::Codex => "Codex (ACP)",
            Self::ClaudeAgent => "Claude Agent (ACP)",
            Self::Custom(_) => "Custom agent",
        }
    }

    /// The tooltip of its start button: what the agent is and what it asks for.
    pub fn note(&self) -> Option<&'static str> {
        match self {
            Self::ClaudeAgent => Some(
                "Runs Claude through the Agent Client Protocol. Needs an Anthropic API key in \
                 ANTHROPIC_API_KEY; a Claude subscription login does not work here.",
            ),
            Self::Codex => {
                Some("Runs Codex through the Agent Client Protocol. Signs in through Codex itself.")
            }
            Self::Custom(_) => None,
        }
    }

    /// The variable that holds the API key of the agent, for a preset that takes one. The agent
    /// inherits the environment of the IDE, so the key has to be set there.
    pub fn api_key_variable(&self) -> Option<&'static str> {
        match self {
            Self::ClaudeAgent => Some("ANTHROPIC_API_KEY"),
            Self::Codex | Self::Custom(_) => None,
        }
    }

    /// The command for this machine. `path` is the PATH to look for `npx` on.
    pub fn command(&self, path: &OsStr) -> Result<AcpCommand, String> {
        let package = match self {
            Self::Codex => CODEX_PACKAGE,
            Self::ClaudeAgent => CLAUDE_PACKAGE,
            Self::Custom(line) => return Ok(AcpCommand::shell(line.clone())),
        };
        let npx = find(path, NPX).ok_or(
            "Node.js is not on PATH: the Codex and Claude Agent agents need npx. \
             Install Node.js and restart the IDE",
        )?;
        let args = vec!["-y".to_string(), package.to_string()];
        Ok(AcpCommand::new(npx, args).through_shell())
    }
}

fn find(path: &OsStr, file: &str) -> Option<PathBuf> {
    let mut candidates = std::env::split_paths(path).map(|directory| directory.join(file));
    candidates.find(|candidate| candidate.is_file())
}

/// A process that speaks ACP on stdio.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcpCommand {
    pub program: PathBuf,
    pub args: Vec<String>,
    /// `None` runs with the environment of the user, `Some` with exactly this one.
    pub env: Option<Vec<(String, String)>>,
}

/// `cmd.exe /d /c` on Windows, `sh -c` on Unix.
fn shell_flags() -> Vec<String> {
    if cfg!(windows) {
        ["/d", "/c"].map(String::from).to_vec()
    } else {
        vec!["-c".to_string()]
    }
}

impl AcpCommand {
    pub fn new(program: impl Into<PathBuf>, args: Vec<String>) -> Self {
        Self {
            program: program.into(),
            args,
            env: None,
        }
    }

    /// A whole command line for the shell of this system, which a custom agent is. On Windows
    /// a part with spaces gets quotes, and `cmd /c` drops them again around a whole line.
    pub fn shell(line: String) -> Self {
        let mut parts = shell_flags();
        parts.push(line);
        Self::new(SHELL, parts)
    }

    /// Through the shell when the program is a script. An npm shim is a batch file on
    /// Windows only: on Unix the package manager links an entry point the system runs as it is.
    pub fn through_shell(self) -> Self {
        #[cfg(windows)]
        {
            let mut args = shell_flags();
            args.push(self.program.to_string_lossy().into_owned());
            args.extend(self.args);
            Self {
                program: SHELL.into(),
                args,
                env: self.env,
            }
        }
        #[cfg(unix)]
        {
            self
        }
    }

    pub fn with_env(mut self, env: Vec<(String, String)>) -> Self {
        self.env = Some(env);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_without_node_say_what_is_missing() {
        let error = Preset::Codex.command(OsStr::new("")).unwrap_err();

        assert!(error.contains("Node.js"), "{error}");
        assert!(
            Preset::ClaudeAgent
                .note()
                .unwrap()
                .contains("ANTHROPIC_API_KEY")
        );
        assert_eq!(
            Preset::ClaudeAgent.api_key_variable(),
            Some("ANTHROPIC_API_KEY")
        );
        assert_eq!(Preset::Codex.api_key_variable(), None);
    }

    #[cfg(windows)]
    #[test]
    fn a_custom_command_goes_through_cmd() {
        let command = Preset::Custom("my-agent --acp".to_string())
            .command(OsStr::new(""))
            .unwrap();

        assert_eq!(command.program, PathBuf::from("cmd.exe"));
        assert_eq!(command.args, ["/d", "/c", "my-agent --acp"]);
    }

    #[cfg(unix)]
    #[test]
    fn a_custom_command_goes_through_sh() {
        let command = Preset::Custom("my-agent --acp".to_string())
            .command(OsStr::new(""))
            .unwrap();

        assert_eq!(command.program, PathBuf::from("sh"));
        assert_eq!(command.args, ["-c", "my-agent --acp"]);
    }

    #[cfg(windows)]
    #[test]
    fn npx_goes_through_cmd() {
        let directory = test_support::TempDir::new("acp-preset-cmd");
        let npx = directory.join(NPX);
        std::fs::write(&npx, "").unwrap();

        let command = Preset::Codex.command(directory.as_os_str()).unwrap();

        assert_eq!(command.program, PathBuf::from("cmd.exe"));
        assert_eq!(command.args[..2], ["/d", "/c"]);
        assert_eq!(command.args[2], npx.to_string_lossy());
        assert_eq!(command.args[3..], ["-y", CODEX_PACKAGE]);
    }

    #[cfg(unix)]
    #[test]
    fn npx_runs_as_it_is_without_a_shell() {
        let directory = test_support::TempDir::new("acp-preset-sh");
        let npx = directory.join(NPX);
        std::fs::write(&npx, "#!/bin/sh\n").unwrap();

        let command = Preset::ClaudeAgent.command(directory.as_os_str()).unwrap();

        assert_eq!(command.program, npx);
        assert_eq!(command.args, ["-y", CLAUDE_PACKAGE]);
    }
}
