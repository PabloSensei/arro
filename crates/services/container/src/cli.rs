use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

use base::Cancel;
use proc_sup::{CaptureError, ProcessSpec, capture};
use serde::Deserialize;

/// A CLI call that has not answered in this long counts as a daemon that is not there:
/// Docker Desktop that is still starting may keep the CLI waiting.
const CALL_TIMEOUT: Duration = Duration::from_secs(15);
/// Where Docker Desktop puts its CLI, for a PATH without it.
const DESKTOP_CLI: &str = "C:/Program Files/Docker/Docker/resources/bin/docker.exe";
/// How many lines of a log come before the live ones.
const LOG_TAIL: &str = "500";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DaemonState {
    /// No `docker` CLI on this machine.
    NoCli,
    /// The CLI is there, its daemon does not answer: Docker Desktop is off or starting.
    Down {
        reason: String,
    },
    Up {
        version: String,
        os: String,
    },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct Context {
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "Current")]
    pub current: bool,
    #[serde(rename = "DockerEndpoint", default)]
    pub endpoint: String,
    #[serde(rename = "Description", default)]
    pub description: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContainerState {
    Running,
    Paused,
    Restarting,
    Exited,
    Created,
    Dead,
    Other,
}

impl ContainerState {
    fn parse(text: &str) -> Self {
        match text {
            "running" => Self::Running,
            "paused" => Self::Paused,
            "restarting" => Self::Restarting,
            "exited" => Self::Exited,
            "created" => Self::Created,
            "dead" => Self::Dead,
            _ => Self::Other,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Container {
    pub id: String,
    /// The first name, without the leading slash.
    pub name: String,
    pub image: String,
    pub state: ContainerState,
    /// What `docker ps` says, such as "Up 2 minutes".
    pub status: String,
    pub ports: String,
    /// The compose project it belongs to, for grouping in the tree.
    pub compose_project: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    pub id: String,
    pub repository: String,
    pub tag: String,
    pub size: String,
}

/// One line of `docker ps --format json`. Labels come as `a=b,c=d`.
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct PsLine {
    #[serde(rename = "ID")]
    id: String,
    names: String,
    image: String,
    state: String,
    status: String,
    #[serde(default)]
    ports: String,
    #[serde(default)]
    labels: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ImageLine {
    #[serde(rename = "ID")]
    id: String,
    repository: String,
    tag: String,
    size: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Version {
    server: Option<ServerVersion>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ServerVersion {
    version: String,
    #[serde(default)]
    os: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ComposeAction {
    Up,
    Down,
}

/// The `docker` CLI. It knows the contexts, the named pipe and the credentials of the
/// user, so the IDE needs no client library of its own.
#[derive(Clone, Debug)]
pub struct DockerCli {
    program: PathBuf,
    /// `None` uses the current context of the CLI.
    context: Option<String>,
    /// Calls of the CLI, shared with the clones the watch holds, for the tests that count
    /// what an idle window does.
    calls: Arc<AtomicU64>,
}

impl DockerCli {
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            context: None,
            calls: Arc::new(AtomicU64::new(0)),
        }
    }

    /// `docker` from PATH, then the one of Docker Desktop.
    pub fn find() -> Option<Self> {
        let path = std::env::var_os("PATH").unwrap_or_default();
        let in_path = std::env::split_paths(&path).map(|directory| directory.join("docker.exe"));
        let mut candidates = in_path.chain([PathBuf::from(DESKTOP_CLI)]);
        candidates.find(|file| file.is_file()).map(Self::new)
    }

    pub fn with_context(mut self, context: Option<String>) -> Self {
        self.context = context;
        self
    }

    pub fn program(&self) -> &Path {
        &self.program
    }

    pub fn context(&self) -> Option<&str> {
        self.context.as_deref()
    }

    /// How many processes the CLI started. A test uses it to prove that a closed panel
    /// asks Docker nothing.
    pub fn calls(&self) -> u64 {
        self.calls.load(Ordering::Relaxed)
    }

    fn args(&self) -> Vec<OsString> {
        match &self.context {
            Some(context) => vec!["--context".into(), context.into()],
            None => Vec::new(),
        }
    }

    pub fn command(&self) -> ProcessSpec {
        ProcessSpec::new(&self.program)
            .args(self.args())
            .env("DOCKER_CLI_HINTS", "false")
    }

    /// Stdout of a call that succeeded, else what the CLI said.
    fn run(&self, args: &[&str]) -> Result<String, String> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        let spec = self.command().args(args);
        let cancel = Cancel::new();
        let timer = cancel.clone();
        let (done, finished) = std::sync::mpsc::channel::<()>();
        thread::spawn(move || {
            if finished.recv_timeout(CALL_TIMEOUT).is_err() {
                timer.cancel();
            }
        });
        let result = capture(&spec, &cancel);
        let _ = done.send(());
        match result {
            Ok(output) if output.status.success() => Ok(output.stdout),
            Ok(output) => Err(first_line(&output.stderr, &output.stdout)),
            Err(CaptureError::Cancelled) => Err(format!(
                "docker {} did not answer in {CALL_TIMEOUT:?}",
                args.join(" ")
            )),
            Err(CaptureError::Io(error)) => Err(error.to_string()),
        }
    }

    pub fn daemon(&self) -> DaemonState {
        if !self.program.is_file() {
            return DaemonState::NoCli;
        }
        match self.run(&["version", "--format", "json"]) {
            Ok(text) => match serde_json::from_str::<Version>(text.trim()) {
                Ok(Version {
                    server: Some(server),
                }) => DaemonState::Up {
                    version: server.version,
                    os: server.os,
                },
                Ok(Version { server: None }) => DaemonState::Down {
                    reason: "the daemon did not answer".into(),
                },
                Err(error) => DaemonState::Down {
                    reason: format!("docker version: {error}"),
                },
            },
            Err(reason) => DaemonState::Down { reason },
        }
    }

    /// Works with the daemon down: contexts are files of the CLI.
    pub fn contexts(&self) -> Result<Vec<Context>, String> {
        let text = self.run(&["context", "ls", "--format", "json"])?;
        json_lines(&text)
    }

    pub fn containers(&self) -> Result<Vec<Container>, String> {
        let text = self.run(&["ps", "--all", "--no-trunc", "--format", "json"])?;
        let lines: Vec<PsLine> = json_lines(&text)?;
        let containers = lines.into_iter().map(|line| Container {
            compose_project: label(&line.labels, "com.docker.compose.project"),
            name: line.names.split(',').next().unwrap_or("").to_owned(),
            id: line.id,
            image: line.image,
            state: ContainerState::parse(&line.state),
            status: line.status,
            ports: line.ports,
        });
        Ok(containers.collect())
    }

    pub fn images(&self) -> Result<Vec<Image>, String> {
        let text = self.run(&["images", "--format", "json"])?;
        let lines: Vec<ImageLine> = json_lines(&text)?;
        let images = lines.into_iter().map(|line| Image {
            id: line.id,
            repository: line.repository,
            tag: line.tag,
            size: line.size,
        });
        Ok(images.collect())
    }

    pub fn start(&self, id: &str) -> Result<(), String> {
        self.run(&["start", id]).map(drop)
    }

    pub fn stop(&self, id: &str) -> Result<(), String> {
        self.run(&["stop", id]).map(drop)
    }

    pub fn restart(&self, id: &str) -> Result<(), String> {
        self.run(&["restart", id]).map(drop)
    }

    /// A running container is removed too, as the panel asks first.
    pub fn remove(&self, id: &str) -> Result<(), String> {
        self.run(&["rm", "--force", id]).map(drop)
    }

    /// `docker run --detach`, for a quick start from the panel. Answers the container id.
    pub fn run_detached(&self, image: &str, args: &[&str]) -> Result<String, String> {
        let mut all = vec!["run", "--detach"];
        all.extend(args);
        all.push(image);
        self.run(&all).map(|id| id.trim().to_owned())
    }

    /// A live log, for a console: the last lines, then what comes, until it is stopped.
    pub fn logs(&self, id: &str) -> ProcessSpec {
        self.command()
            .args(["logs", "--follow", "--tail", LOG_TAIL, id])
    }

    /// For the built-in terminal: compose runs in front of the user and may ask things.
    pub fn compose_command_line(&self, file: &Path, action: ComposeAction) -> String {
        let mut words = self.words();
        words.extend([
            "compose".into(),
            "--file".into(),
            file.display().to_string(),
        ]);
        match action {
            ComposeAction::Up => words.push("up".into()),
            ComposeAction::Down => words.push("down".into()),
        }
        join_command_line(&words)
    }

    /// A shell inside a container, for the built-in terminal.
    pub fn exec_command_line(&self, id: &str, shell: &str) -> String {
        let mut words = self.words();
        words.extend(["exec", "--interactive", "--tty", id, shell].map(String::from));
        join_command_line(&words)
    }

    fn words(&self) -> Vec<String> {
        let mut words = vec![self.program.display().to_string()];
        words.extend(
            self.args()
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned()),
        );
        words
    }
}

fn first_line(stderr: &str, stdout: &str) -> String {
    let text = if stderr.trim().is_empty() {
        stdout
    } else {
        stderr
    };
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("docker failed")
        .to_owned()
}

fn json_lines<T: for<'de> Deserialize<'de>>(text: &str) -> Result<Vec<T>, String> {
    let lines = text.lines().filter(|line| !line.trim().is_empty());
    lines
        .map(|line| serde_json::from_str(line).map_err(|error| format!("{error}: {line}")))
        .collect()
}

fn label(labels: &str, name: &str) -> Option<String> {
    labels.split(',').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key == name).then(|| value.to_owned())
    })
}

/// Quotes the way the C runtime of Windows reads a command line back into arguments.
fn join_command_line(words: &[String]) -> String {
    let quote = |word: &String| {
        if !word.is_empty() && !word.contains([' ', '\t', '"']) {
            return word.clone();
        }
        let mut quoted = String::from("\"");
        let mut slashes = 0;
        for c in word.chars() {
            match c {
                '\\' => slashes += 1,
                '"' => {
                    quoted.push_str(&"\\".repeat(slashes * 2 + 1));
                    quoted.push('"');
                    slashes = 0;
                }
                _ => {
                    quoted.push_str(&"\\".repeat(slashes));
                    quoted.push(c);
                    slashes = 0;
                }
            }
        }
        quoted.push_str(&"\\".repeat(slashes * 2));
        quoted.push('"');
        quoted
    };
    words.iter().map(quote).collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ps_line_of_the_real_cli_is_read() {
        let line = r#"{"Command":"\"/docker-entrypoint.…\"","CreatedAt":"2026-11-03 15:00:00 +0100 CET","ID":"4f2a","Image":"nginx:latest","Labels":"com.docker.compose.project=petclinic,com.docker.compose.service=web","LocalVolumes":"0","Mounts":"","Names":"petclinic-web-1","Networks":"bridge","Ports":"0.0.0.0:8080->80/tcp","RunningFor":"2 minutes ago","Size":"0B","State":"running","Status":"Up 2 minutes"}"#;

        let lines: Vec<PsLine> = json_lines(line).unwrap();

        assert_eq!(lines[0].id, "4f2a");
        assert_eq!(
            ContainerState::parse(&lines[0].state),
            ContainerState::Running
        );
        assert_eq!(
            label(&lines[0].labels, "com.docker.compose.project").as_deref(),
            Some("petclinic")
        );
    }

    #[test]
    fn command_lines_quote_what_needs_it() {
        let cli = DockerCli::new("C:/Program Files/Docker/docker.exe")
            .with_context(Some("desktop-linux".into()));

        let line = cli.compose_command_line(Path::new("C:/my app/compose.yaml"), ComposeAction::Up);

        assert_eq!(
            line,
            "\"C:/Program Files/Docker/docker.exe\" --context desktop-linux compose --file \"C:/my app/compose.yaml\" up"
        );
        let words = vec![r#"a "b" c\"#.to_owned()];
        assert_eq!(join_command_line(&words), r#""a \"b\" c\\""#);
    }
}
