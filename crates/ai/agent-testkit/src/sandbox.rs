//! An empty home for one test: `~/.claude`, `~/.codex` and the settings of the user stay out of
//! reach, so a harness started here sees no login at all.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::Duration;

use serde_json::json;
use terminal::allowlisted;

use crate::mock_api::{FAKE_TOKEN, MOCK_MODEL, MockApi};

const CLAUDE_VERSION: &str = "2.1.312";

const NO_BACKGROUND_TRAFFIC: &[(&str, &str)] = &[
    ("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1"),
    ("DISABLE_AUTOUPDATER", "1"),
    ("DISABLE_TELEMETRY", "1"),
    ("DISABLE_ERROR_REPORTING", "1"),
];

/// Removed on every way out, panics included. A home holds a copy of the Codex daemon, and the
/// processes that lock it die with the job of their session, so removal retries for a moment.
pub struct Sandbox {
    pub home: PathBuf,
    pub project: PathBuf,
}

impl Sandbox {
    /// Stays short: Codex binds an AF_UNIX socket under `CODEX_HOME`, limited to 108 chars.
    pub fn new(name: &str) -> io::Result<Self> {
        let home = fresh_home(name)?;
        let project = home.join("project");
        fs::create_dir_all(&project)?;
        Ok(Self { home, project })
    }

    /// A home for a project that lives elsewhere and stays when the home goes.
    pub fn for_project(name: &str, project: &Path) -> io::Result<Self> {
        let home = fresh_home(name)?;
        fs::create_dir_all(&home)?;
        Ok(Self {
            home,
            project: project.to_path_buf(),
        })
    }

    /// The child environment with every home variable pointing here.
    pub fn env(&self) -> io::Result<Vec<(String, String)>> {
        let appdata = self.home.join("AppData/Roaming");
        let local = self.home.join("AppData/Local");
        let claude = self.home.join(".claude");
        let codex = self.home.join(".codex");
        for directory in [&appdata, &local, &claude, &codex] {
            fs::create_dir_all(directory)?;
        }
        let path = |value: &Path| value.to_string_lossy().replace('/', "\\");
        let mut env: Vec<(String, String)> = allowlisted();
        let overrides = [
            ("USERPROFILE", path(&self.home)),
            ("HOME", path(&self.home)),
            ("HOMEPATH", path(&self.home)),
            ("APPDATA", path(&appdata)),
            ("LOCALAPPDATA", path(&local)),
            ("CLAUDE_CONFIG_DIR", path(&claude)),
            ("CODEX_HOME", path(&codex)),
        ];
        for (key, value) in overrides {
            env.retain(|(known, _)| !known.eq_ignore_ascii_case(key));
            env.push((key.to_string(), value));
        }
        Ok(env)
    }
}

fn fresh_home(name: &str) -> io::Result<PathBuf> {
    let id = std::process::id();
    let home = std::env::temp_dir()
        .join("ide-t")
        .join(format!("{name}-{id}"));
    if home.exists() {
        fs::remove_dir_all(&home)?;
    }
    Ok(home)
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        for _ in 0..20 {
            if !self.home.exists() || fs::remove_dir_all(&self.home).is_ok() {
                return;
            }
            sleep(Duration::from_millis(300));
        }
    }
}

/// A sandbox for `claude` with onboarding marked done, the project trusted, and the mock as API.
pub fn claude_env(sandbox: &Sandbox, mock: &MockApi) -> io::Result<Vec<(String, String)>> {
    let mut env = sandbox.env()?;
    let vars = NO_BACKGROUND_TRAFFIC
        .iter()
        .map(|(key, value)| (key.to_string(), value.to_string()));
    env.extend(vars);
    let project = sandbox.project.to_string_lossy().replace('\\', "/");
    let state = json!({
        "hasCompletedOnboarding": true,
        "lastOnboardingVersion": CLAUDE_VERSION,
        "theme": "dark",
        "bypassPermissionsModeAccepted": true,
        "projects": {
            project: { "hasTrustDialogAccepted": true, "hasCompletedProjectOnboarding": true }
        },
    });
    fs::write(sandbox.home.join(".claude/.claude.json"), state.to_string())?;
    env.push(("ANTHROPIC_BASE_URL".to_string(), mock.anthropic_url()));
    env.push(("ANTHROPIC_AUTH_TOKEN".to_string(), FAKE_TOKEN.to_string()));
    // Deferred tools would need a scripted search turn, the tool channel is what is under test.
    env.push(("ENABLE_TOOL_SEARCH".to_string(), "false".to_string()));
    Ok(env)
}

/// A sandbox for `codex` with a provider that points at the mock and asks for no auth.
pub fn codex_env(sandbox: &Sandbox, mock: &MockApi) -> io::Result<Vec<(String, String)>> {
    let env = sandbox.env()?;
    let project = sandbox.project.to_string_lossy().replace('\\', "/");
    let url = mock.openai_url();
    // Without an explicit sandbox mode Codex offers an elevated sandbox setup on first start.
    let config = format!(
        r#"model = "{MOCK_MODEL}"
model_provider = "mock"
check_for_update_on_startup = false
sandbox_mode = "danger-full-access"
approval_policy = "never"

[model_providers.mock]
name = "mock"
base_url = "{url}"
wire_api = "responses"
requires_openai_auth = false
supports_websockets = false

[projects."{project}"]
trust_level = "trusted"

[analytics]
enabled = false

[otel]
log_user_prompt = false
"#
    );
    fs::write(sandbox.home.join(".codex/config.toml"), config)?;
    Ok(env)
}
