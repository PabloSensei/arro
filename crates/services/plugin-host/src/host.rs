use std::collections::{BTreeSet, VecDeque};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use proc_sup::{Process, ProcessSpec};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::manifest::{API_VERSION, KeyBinding, Manifest, ManifestError};
use crate::protocol::{HostRequest, Routed, route};
use crate::rpc::{CallError, Connection};
use crate::theme::PluginTheme;

/// Lines of log and stderr kept per plugin for the list.
const LOG_LINES: usize = 200;
/// What a plugin process gets from the environment of the IDE. Nothing else reaches it:
/// no MCP token, no keys of the harnesses.
const PASSED_ENV: &[&str] = &[
    "APPDATA",
    "COMPUTERNAME",
    "ComSpec",
    "HOMEDRIVE",
    "HOMEPATH",
    "LOCALAPPDATA",
    "NUMBER_OF_PROCESSORS",
    "OS",
    "PATH",
    "PATHEXT",
    "PROCESSOR_ARCHITECTURE",
    "ProgramData",
    "ProgramFiles",
    "ProgramFiles(x86)",
    "SystemDrive",
    "SystemRoot",
    "TEMP",
    "TMP",
    "USERNAME",
    "USERPROFILE",
    "windir",
];

/// `%APPDATA%/Arro/plugins`, unless `IDE_PLUGINS_DIR` points elsewhere. Plugins of an
/// opened project are never loaded: opening someone's repository must not run its code.
pub fn default_plugins_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("IDE_PLUGINS_DIR") {
        return Some(PathBuf::from(dir));
    }
    let appdata = std::env::var_os("APPDATA")?;
    Some(base::app::folder_under(Path::new(&appdata)).join("plugins"))
}

#[derive(Clone, Copy, Debug)]
pub struct Timeouts {
    /// From the start of the process to the answer to `initialize`.
    pub start: Duration,
    pub command: Duration,
    pub shutdown: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            start: Duration::from_secs(10),
            command: Duration::from_secs(30),
            shutdown: Duration::from_secs(2),
        }
    }
}

/// What the user switched off, kept by the IDE in its settings folder.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct PluginSettings {
    #[serde(default)]
    pub disabled: BTreeSet<String>,
}

impl PluginSettings {
    /// A missing file means every plugin is on.
    pub fn load(path: &Path) -> Result<Self, String> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                toml::from_str(&text).map_err(|error| format!("{}: {error}", path.display()))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(format!("{}: {error}", path.display())),
        }
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = toml::to_string(self).map_err(std::io::Error::other)?;
        std::fs::write(path, text)
    }
}

/// What the plugin list shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PluginState {
    /// The manifest could not be read, nothing of the plugin is used.
    Broken(String),
    Disabled,
    /// Its process starts with the first call of one of its commands.
    NotStarted,
    Running,
    /// The process crashed, hung or broke the protocol. The next command starts it again.
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PluginError {
    NoSuchCommand(String),
    /// Off, broken, or the process could not start.
    Unavailable(String),
    /// The process is gone or did not answer in time; the plugin is marked failed.
    Failed(String),
    /// The plugin is fine and said its command failed.
    Command(String),
}

impl std::fmt::Display for PluginError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoSuchCommand(id) => write!(f, "no plugin command {id}"),
            Self::Unavailable(message) | Self::Failed(message) | Self::Command(message) => {
                f.write_str(message)
            }
        }
    }
}

impl std::error::Error for PluginError {}

/// A command for the registry, Find Action and the keymap.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContributedCommand {
    /// `<plugin id>.<command id>`.
    pub id: String,
    pub title: String,
    pub key: Option<String>,
    pub plugin: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContributedTheme {
    pub plugin: String,
    pub theme: Result<PluginTheme, String>,
}

type Log = Arc<Mutex<VecDeque<String>>>;

fn push_log(log: &Log, line: String) {
    let mut log = log.lock().unwrap();
    if log.len() == LOG_LINES {
        log.pop_front();
    }
    log.push_back(line);
}

struct Session {
    process: Mutex<Process>,
    connection: Connection,
}

enum Runtime {
    Idle,
    Running(Arc<Session>),
    Failed(String),
}

pub struct Plugin {
    dir: PathBuf,
    manifest: Result<Manifest, ManifestError>,
    enabled: bool,
    runtime: Mutex<Runtime>,
    /// Held while the process starts, so that two commands start one process.
    starting: Mutex<()>,
    log: Log,
}

impl Plugin {
    fn new(dir: PathBuf, enabled: bool) -> Self {
        Self {
            manifest: Manifest::read(&dir),
            dir,
            enabled,
            runtime: Mutex::new(Runtime::Idle),
            starting: Mutex::new(()),
            log: Log::default(),
        }
    }

    /// The manifest id, or the folder name when the manifest is broken.
    pub fn id(&self) -> String {
        match &self.manifest {
            Ok(manifest) => manifest.id.clone(),
            Err(_) => self
                .dir
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn manifest(&self) -> Option<&Manifest> {
        self.manifest.as_ref().ok()
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    fn active(&self) -> Option<&Manifest> {
        self.manifest().filter(|_| self.enabled)
    }

    pub fn state(&self) -> PluginState {
        if let Err(error) = &self.manifest {
            return PluginState::Broken(error.0.clone());
        }
        if !self.enabled {
            return PluginState::Disabled;
        }
        let mut runtime = self.runtime.lock().unwrap();
        if let Runtime::Running(session) = &*runtime
            && session.connection.is_closed()
        {
            let reason = self.exit_reason(session);
            *runtime = Runtime::Failed(reason);
        }
        match &*runtime {
            Runtime::Idle => PluginState::NotStarted,
            Runtime::Running(_) => PluginState::Running,
            Runtime::Failed(reason) => PluginState::Failed(reason.clone()),
        }
    }

    /// The last lines the plugin logged or printed to stderr.
    pub fn log(&self) -> Vec<String> {
        self.log.lock().unwrap().iter().cloned().collect()
    }

    fn exit_reason(&self, session: &Session) -> String {
        // The pipe may close a moment before the process is reaped.
        let deadline = Instant::now() + Duration::from_millis(500);
        let mut process = session.process.lock().unwrap();
        let status = loop {
            match process.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
                _ => break None,
            }
        };
        let mut reason = match status.and_then(|status| status.code()) {
            Some(code) => format!("the process exited with code {code}"),
            None => "the process closed its output".to_owned(),
        };
        let log = self.log.lock().unwrap();
        if let Some(last) = log.iter().rev().find(|line| line.starts_with("stderr: ")) {
            reason.push_str(": ");
            reason.push_str(last.trim_start_matches("stderr: "));
        }
        reason
    }

    fn fail(&self, reason: String) {
        let previous =
            std::mem::replace(&mut *self.runtime.lock().unwrap(), Runtime::Failed(reason));
        if let Runtime::Running(session) = previous {
            let _ = session.process.lock().unwrap().stop();
        }
    }

    fn session(
        &self,
        requests: &Sender<HostRequest>,
        timeouts: &Timeouts,
    ) -> Result<Arc<Session>, PluginError> {
        let manifest = self.active().ok_or_else(|| {
            PluginError::Unavailable(format!("the plugin {} is off or broken", self.id()))
        })?;
        let _starting = self.starting.lock().unwrap();
        // `state` turns a dead session into a failure, and a failure starts again.
        if let PluginState::Running = self.state()
            && let Runtime::Running(session) = &*self.runtime.lock().unwrap()
        {
            return Ok(session.clone());
        }
        let session = self.start(manifest, requests.clone())?;
        let params = json!({
            "api": API_VERSION,
            "plugin": {"id": manifest.id, "dir": self.dir},
            "ide": {"name": "Arro", "version": env!("CARGO_PKG_VERSION")},
        });
        match session
            .connection
            .call("initialize", params, timeouts.start)
        {
            Ok(_) => {}
            Err(error) => {
                let reason = match error {
                    CallError::Closed => self.exit_reason(&session),
                    error => format!("initialize: {error}"),
                };
                let _ = session.process.lock().unwrap().stop();
                *self.runtime.lock().unwrap() = Runtime::Failed(reason.clone());
                return Err(PluginError::Failed(reason));
            }
        }
        *self.runtime.lock().unwrap() = Runtime::Running(session.clone());
        Ok(session)
    }

    fn start(
        &self,
        manifest: &Manifest,
        requests: Sender<HostRequest>,
    ) -> Result<Arc<Session>, PluginError> {
        let command = manifest.command.as_deref().unwrap_or_default();
        let unavailable = |message: String| {
            *self.runtime.lock().unwrap() = Runtime::Failed(message.clone());
            PluginError::Unavailable(message)
        };
        let [program, args @ ..] = command else {
            return Err(unavailable(format!(
                "the plugin {} has no process",
                manifest.id
            )));
        };
        // A program in the plugin folder wins over one of the same name in PATH.
        let local = self.dir.join(program);
        let program = if local.is_file() {
            local.into_os_string()
        } else {
            program.into()
        };
        let mut spec = ProcessSpec::new(&program)
            .args(args)
            .cwd(&self.dir)
            .env_clear();
        for name in PASSED_ENV {
            if let Some(value) = std::env::var_os(name) {
                spec = spec.env(name, value);
            }
        }
        spec = spec
            .env("IDE_PLUGIN_ID", &manifest.id)
            .env("IDE_PLUGIN_DIR", &self.dir);
        let mut process = spec.start().map_err(|error| {
            unavailable(format!("{}: {error}", PathBuf::from(&program).display()))
        })?;
        let (Some(stdin), Some(stdout), Some(stderr)) = (
            process.take_stdin(),
            process.take_stdout(),
            process.take_stderr(),
        ) else {
            return Err(unavailable("no pipes to the plugin process".into()));
        };
        let log = self.log.clone();
        thread::Builder::new()
            .name("plugin-stderr".into())
            .spawn(move || {
                for line in BufReader::new(stderr).lines() {
                    let Ok(line) = line else { break };
                    push_log(&log, format!("stderr: {line}"));
                }
            })
            .map_err(|error| unavailable(error.to_string()))?;
        let (connection, incoming) = Connection::new(stdout, stdin);
        let (id, log) = (manifest.id.clone(), self.log.clone());
        thread::Builder::new()
            .name("plugin-calls".into())
            .spawn(move || {
                for message in incoming {
                    match route(&id, message) {
                        Routed::ToWindow(request) => {
                            // A window that is gone drops the request, which answers it.
                            let _ = requests.send(request);
                        }
                        Routed::Log(line) => push_log(&log, line),
                        Routed::Handled => {}
                    }
                }
            })
            .map_err(|error| unavailable(error.to_string()))?;
        Ok(Arc::new(Session {
            process: Mutex::new(process),
            connection,
        }))
    }

    fn shutdown(&self, timeout: Duration) {
        let previous = std::mem::replace(&mut *self.runtime.lock().unwrap(), Runtime::Idle);
        let Runtime::Running(session) = previous else {
            return;
        };
        let _ = session
            .connection
            .call("shutdown", serde_json::Value::Null, timeout);
        let _ = session.connection.notify("exit", serde_json::Value::Null);
        let mut process = session.process.lock().unwrap();
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if let Ok(Some(_)) = process.try_wait() {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        let _ = process.stop();
    }
}

/// The plugins of one IDE run. Enabling or disabling one takes a restart.
pub struct PluginHost {
    plugins: Vec<Arc<Plugin>>,
    requests: Sender<HostRequest>,
    timeouts: Timeouts,
}

impl PluginHost {
    /// Every folder in `dir` is one plugin, in name order. A missing `dir` has none. Calls
    /// of the plugins to the window arrive on the receiver.
    pub fn load(
        dir: &Path,
        settings: &PluginSettings,
        timeouts: Timeouts,
    ) -> (Self, Receiver<HostRequest>) {
        let mut folders: Vec<PathBuf> = match std::fs::read_dir(dir) {
            Ok(entries) => entries
                .filter_map(|entry| entry.ok())
                .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
                .map(|entry| entry.path())
                .collect(),
            Err(_) => Vec::new(),
        };
        folders.sort();
        let plugins = folders
            .into_iter()
            .map(|folder| {
                let name = folder.file_name().unwrap_or_default().to_string_lossy();
                let enabled = !settings.disabled.contains(name.as_ref());
                Arc::new(Plugin::new(folder, enabled))
            })
            .collect();
        let (requests, received) = mpsc::channel();
        let host = Self {
            plugins,
            requests,
            timeouts,
        };
        (host, received)
    }

    pub fn plugins(&self) -> &[Arc<Plugin>] {
        &self.plugins
    }

    fn active(&self) -> impl Iterator<Item = &Manifest> {
        self.plugins.iter().filter_map(|plugin| plugin.active())
    }

    pub fn commands(&self) -> Vec<ContributedCommand> {
        let commands = self.active().flat_map(|manifest| {
            manifest
                .contributes
                .commands
                .iter()
                .map(|command| ContributedCommand {
                    id: manifest.command_id(&command.id),
                    title: command.title.clone(),
                    key: command.key.clone(),
                    plugin: manifest.id.clone(),
                })
        });
        commands.collect()
    }

    /// Bindings on top of the default keymap, in plugin order: a later one wins.
    pub fn keybindings(&self) -> Vec<KeyBinding> {
        let bindings = self
            .active()
            .flat_map(|manifest| manifest.contributes.keybindings.iter().cloned());
        bindings.collect()
    }

    /// Reads the theme files now. A broken file is reported and the others still load.
    pub fn themes(&self) -> Vec<ContributedTheme> {
        let themes = self
            .plugins
            .iter()
            .filter_map(|plugin| Some((plugin, plugin.active()?)));
        let themes = themes.flat_map(|(plugin, manifest)| {
            manifest
                .contributes
                .themes
                .iter()
                .map(|theme| ContributedTheme {
                    plugin: manifest.id.clone(),
                    theme: PluginTheme::read(&theme.name, &plugin.dir.join(&theme.file)),
                })
        });
        themes.collect()
    }

    /// Runs a plugin command, starting its process first if needed. Blocks until the
    /// plugin answers or the timeout passes, so call it off the UI thread: meanwhile the
    /// plugin's own calls arrive on the receiver of [`Self::load`].
    pub fn execute(&self, command_id: &str) -> Result<(), PluginError> {
        let no_such = || PluginError::NoSuchCommand(command_id.to_owned());
        let (plugin, local) = self
            .plugins
            .iter()
            .find_map(|plugin| {
                let manifest = plugin.active()?;
                let local = command_id.strip_prefix(&manifest.id)?.strip_prefix('.')?;
                let known = manifest
                    .contributes
                    .commands
                    .iter()
                    .any(|command| command.id == local);
                known.then_some((plugin, local))
            })
            .ok_or_else(no_such)?;
        let session = plugin.session(&self.requests, &self.timeouts)?;
        let params = json!({"command": local});
        match session
            .connection
            .call("command/execute", params, self.timeouts.command)
        {
            Ok(_) => Ok(()),
            Err(CallError::Remote { message, .. }) => Err(PluginError::Command(message)),
            Err(CallError::TimedOut) => {
                let reason = format!("{command_id} did not finish in {:?}", self.timeouts.command);
                plugin.fail(reason.clone());
                Err(PluginError::Failed(reason))
            }
            Err(CallError::Closed) => {
                let reason = plugin.exit_reason(&session);
                plugin.fail(reason.clone());
                Err(PluginError::Failed(reason))
            }
        }
    }

    /// Asks every running plugin to stop and ends the ones that do not, one by one.
    pub fn shutdown(&self) {
        for plugin in &self.plugins {
            plugin.shutdown(self.timeouts.shutdown);
        }
    }
}

impl Drop for PluginHost {
    fn drop(&mut self) {
        self.shutdown();
    }
}
