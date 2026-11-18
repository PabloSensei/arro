//! The IDE protocol of Claude Code: a lock file under `<config>/ide`, a WebSocket server
//! on the loopback and the MCP tools of the IDE over it. The editor side of a proposed
//! file is not here: a diff waits in [`Bridge`] until the window decides, and only then
//! does the file on disk change.

mod protocol;
mod ws;

pub use protocol::file_uri;

use std::collections::HashMap;
use std::fs;
use std::io;
use std::net::{TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, RwLock};
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};

/// The variable of the terminal that points the harness at this server. The harness reads
/// the lock files of the home too, but the variable makes the IDE its own project.
pub const PORT_VARIABLE: &str = "CLAUDE_CODE_SSE_PORT";

/// The variable that moves the configuration home of the harness: the lock file goes to
/// `ide` inside it, without a `.claude` of its own.
pub const CONFIG_VARIABLE: &str = "CLAUDE_CONFIG_DIR";

/// What the harness shows the user as the name of the IDE.
pub const IDE_NAME: &str = "Arro";

/// The configuration home of the harness: `given` when it names an absolute folder, as
/// the harness reads the variable, and the `.claude` folder of the home otherwise.
pub fn config_home(given: Option<&Path>, home: Option<&Path>) -> Option<PathBuf> {
    match given {
        Some(dir) if dir.is_absolute() => Some(dir.to_path_buf()),
        _ => home.map(|home| home.join(".claude")),
    }
}

/// How a proposed file ends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiffDecision {
    /// The final text of the file, which is what the user kept.
    Accept(String),
    /// The user kept the file as it was.
    Reject,
    /// The tab closed, or the connection ended, before the user decided.
    Closed,
}

/// A file the harness proposes, as it stood on disk and as it would become.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diff {
    pub id: u64,
    /// Absolute, inside the project.
    pub path: PathBuf,
    pub tab_name: String,
    /// `None` when the file does not exist yet.
    pub base: Option<String>,
    pub proposed: String,
}

/// What the window has to know about a diff.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BridgeEvent {
    Open(Diff),
    /// The harness closed the tab or went away; nothing is left to decide.
    Close(u64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Error,
    Warning,
    Info,
    Hint,
}

impl Severity {
    pub fn name(self) -> &'static str {
        match self {
            Self::Error => "Error",
            Self::Warning => "Warning",
            Self::Info => "Info",
            Self::Hint => "Hint",
        }
    }
}

/// One problem of the Problems window, in the units the protocol wants: zero-based lines
/// and UTF-16 characters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub line: u32,
    pub character: u32,
    pub severity: Severity,
    pub message: String,
    pub source: Option<String>,
    pub code: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileDiagnostics {
    pub path: PathBuf,
    pub diagnostics: Vec<Diagnostic>,
}

/// What the Problems window shows, for `getDiagnostics`. The window owns the data and
/// replaces it as the diagnostics arrive.
#[derive(Debug, Default)]
pub struct Diagnostics {
    files: RwLock<Vec<FileDiagnostics>>,
}

impl Diagnostics {
    pub fn replace(&self, files: Vec<FileDiagnostics>) {
        *self
            .files
            .write()
            .unwrap_or_else(|error| error.into_inner()) = files;
    }

    /// The files of the snapshot: one when `path` is named, all of them otherwise.
    pub fn files(&self, path: Option<&Path>) -> Vec<FileDiagnostics> {
        let files = self.files.read().unwrap_or_else(|error| error.into_inner());
        match path {
            Some(path) => files
                .iter()
                .filter(|file| same_path(&file.path, path))
                .cloned()
                .collect(),
            None => files.clone(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OpenEditor {
    pub path: PathBuf,
    pub active: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selection {
    pub path: PathBuf,
    pub start_line: u32,
    pub start_character: u32,
    pub end_line: u32,
    pub end_character: u32,
    pub text: String,
}

/// The open editors and the selection of the active one, for the context tools.
#[derive(Debug, Default)]
pub struct Editors {
    editors: RwLock<Vec<OpenEditor>>,
    selection: RwLock<Option<Selection>>,
}

impl Editors {
    pub fn replace(&self, editors: Vec<OpenEditor>, selection: Option<Selection>) {
        *self
            .editors
            .write()
            .unwrap_or_else(|error| error.into_inner()) = editors;
        *self
            .selection
            .write()
            .unwrap_or_else(|error| error.into_inner()) = selection;
    }

    pub fn editors(&self) -> Vec<OpenEditor> {
        self.editors
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    pub fn selection(&self) -> Option<Selection> {
        self.selection
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }
}

pub(crate) struct Pending {
    connection: u64,
    request: Value,
    writer: Arc<Mutex<TcpStream>>,
    tab_name: String,
}

pub(crate) struct Shared {
    project: PathBuf,
    token: String,
    diagnostics: Arc<Diagnostics>,
    editors: Arc<Editors>,
    next_diff: AtomicU64,
    next_connection: AtomicU64,
    connected: AtomicUsize,
    pending: Mutex<HashMap<u64, Pending>>,
    events: Sender<BridgeEvent>,
}

impl Shared {
    pub(crate) fn project(&self) -> &Path {
        &self.project
    }

    pub(crate) fn diagnostics(&self) -> &Diagnostics {
        &self.diagnostics
    }

    pub(crate) fn editors(&self) -> &Editors {
        &self.editors
    }

    pub(crate) fn pending_on(&self, connection: u64, tab_name: Option<&str>) -> Vec<u64> {
        let pending = self
            .pending
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        pending
            .iter()
            .filter(|(_, entry)| entry.connection == connection)
            .filter(|(_, entry)| tab_name.is_none_or(|name| entry.tab_name == name))
            .map(|(id, _)| *id)
            .collect()
    }

    pub(crate) fn close(&self, id: u64) -> Option<Pending> {
        let pending = self
            .pending
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&id);
        if pending.is_some() {
            let _ = self.events.send(BridgeEvent::Close(id));
        }
        pending
    }

    /// Registers a proposed file and tells the window about it.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn propose(
        &self,
        path: PathBuf,
        tab_name: String,
        base: Option<String>,
        proposed: String,
        request: Value,
        writer: Arc<Mutex<TcpStream>>,
        connection: u64,
    ) -> u64 {
        let id = self.next_diff.fetch_add(1, Ordering::Relaxed) + 1;
        let diff = Diff {
            id,
            path,
            tab_name: tab_name.clone(),
            base,
            proposed,
        };
        self.pending
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(
                id,
                Pending {
                    connection,
                    request,
                    writer,
                    tab_name,
                },
            );
        let _ = self.events.send(BridgeEvent::Open(diff));
        id
    }

    fn disconnect(&self, connection: u64) {
        let closed = self.pending_on(connection, None);
        for id in closed {
            self.close(id);
        }
    }
}

/// The server of one window. Dropping it removes the lock file and closes the port.
pub struct Bridge {
    shared: Arc<Shared>,
    port: u16,
    lock: PathBuf,
    stop: Arc<AtomicBool>,
    accept: Option<thread::JoinHandle<()>>,
    events: Mutex<Receiver<BridgeEvent>>,
}

impl Bridge {
    /// Serves on a port of the loopback and writes `<config>/ide/<port>.lock`, where
    /// `config` is the configuration home of the harness, see [`config_home`].
    pub fn start(
        config: &Path,
        project: &Path,
        diagnostics: Arc<Diagnostics>,
        editors: Arc<Editors>,
    ) -> io::Result<Self> {
        let token = token()?;
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        let (sender, events) = mpsc::channel();
        let shared = Arc::new(Shared {
            project: project.to_path_buf(),
            token,
            diagnostics,
            editors,
            next_diff: AtomicU64::new(0),
            next_connection: AtomicU64::new(0),
            connected: AtomicUsize::new(0),
            pending: Mutex::new(HashMap::new()),
            events: sender,
        });
        let lock = lock_path(config, port);
        write_lock(&lock, project, &shared.token)?;
        let stop = Arc::new(AtomicBool::new(false));
        let accept_shared = shared.clone();
        let accept_stop = stop.clone();
        let spawned = thread::Builder::new()
            .name("claude-bridge".to_string())
            .spawn(move || accept(accept_shared, listener, accept_stop));
        let accept = match spawned {
            Ok(accept) => accept,
            Err(error) => {
                let _ = fs::remove_file(&lock);
                return Err(error);
            }
        };
        Ok(Self {
            shared,
            port,
            lock,
            stop,
            accept: Some(accept),
            events: Mutex::new(events),
        })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn lock_path(&self) -> &Path {
        &self.lock
    }

    /// The variables of a terminal under this window.
    pub fn environment(&self) -> Vec<(String, String)> {
        vec![(PORT_VARIABLE.to_string(), self.port.to_string())]
    }

    /// Whether a harness is connected right now.
    pub fn connected(&self) -> bool {
        self.shared.connected.load(Ordering::Relaxed) > 0
    }

    /// The diffs to show and the ones the harness took back. The window drains it.
    pub fn events(&self) -> Vec<BridgeEvent> {
        let events = self
            .events
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        events.try_iter().collect()
    }

    /// Answers `openDiff` for `id`. The file on disk is written by the window before this
    /// call; the harness only hears the outcome.
    pub fn resolve(&self, id: u64, decision: DiffDecision) {
        let Some(pending) = self
            .shared
            .pending
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&id)
        else {
            return;
        };
        let (first, last) = match decision {
            DiffDecision::Accept(text) => ("FILE_SAVED", Some(text)),
            DiffDecision::Reject => ("DIFF_REJECTED", None),
            DiffDecision::Closed => ("TAB_CLOSED", None),
        };
        let mut content = vec![json!({"type": "text", "text": first})];
        if let Some(text) = last {
            content.push(json!({"type": "text", "text": text}));
        }
        let message = json!({
            "jsonrpc": "2.0",
            "id": pending.request,
            "result": {"content": content},
        });
        write_message(&pending.writer, &message);
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        // The accepting thread waits in `accept`, so a connection of our own wakes it;
        // joining it closes the port before the lock file goes.
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(accept) = self.accept.take() {
            let _ = accept.join();
        }
        let _ = fs::remove_file(&self.lock);
    }
}

fn token() -> io::Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|error| io::Error::other(error.to_string()))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn lock_path(config: &Path, port: u16) -> PathBuf {
    config.join("ide").join(format!("{port}.lock"))
}

fn write_lock(path: &Path, project: &Path, token: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let project = project.to_string_lossy();
    let text = json!({
        "pid": std::process::id(),
        "workspaceFolders": [project],
        "ideName": IDE_NAME,
        "transport": "ws",
        "runningInWindows": cfg!(windows),
        "authToken": token,
    });
    let body = serde_json::to_vec_pretty(&text).map_err(io::Error::other)?;
    fs::write(path, body)
}

fn accept(shared: Arc<Shared>, listener: TcpListener, stop: Arc<AtomicBool>) {
    // The listener waits inside `accept`; the drop of the bridge wakes it by connecting,
    // so the loop costs nothing while no harness is here.
    while let Ok((stream, _)) = listener.accept() {
        if stop.load(Ordering::SeqCst) {
            break;
        }
        let shared = shared.clone();
        thread::Builder::new()
            .name("claude-bridge-connection".to_string())
            .spawn(move || connection(shared, stream))
            .ok();
    }
}

fn connection(shared: Arc<Shared>, mut stream: TcpStream) {
    let _ = stream.set_nodelay(true);
    match ws::handshake(&mut stream, &shared.token) {
        Ok(true) => {}
        _ => return,
    }
    let Ok(writer_stream) = stream.try_clone() else {
        return;
    };
    let _ = writer_stream.set_write_timeout(Some(Duration::from_secs(10)));
    let writer = Arc::new(Mutex::new(writer_stream));
    let connection = shared.next_connection.fetch_add(1, Ordering::Relaxed) + 1;
    shared.connected.fetch_add(1, Ordering::Relaxed);
    let mut assembled: Vec<u8> = Vec::new();
    let mut in_message = false;
    'frames: while let Ok(Some(frame)) = ws::read_frame(&mut stream) {
        match frame.opcode {
            ws::Opcode::Ping => {
                let mut writer = writer.lock().unwrap_or_else(|error| error.into_inner());
                let _ = ws::send_pong(&mut *writer, &frame.payload);
            }
            ws::Opcode::Pong => {}
            ws::Opcode::Close => {
                let mut writer = writer.lock().unwrap_or_else(|error| error.into_inner());
                let _ = ws::send_close(&mut *writer);
                break;
            }
            ws::Opcode::Binary => {}
            ws::Opcode::Text | ws::Opcode::Continuation => {
                if frame.opcode == ws::Opcode::Text {
                    assembled.clear();
                    in_message = true;
                }
                if !in_message {
                    continue;
                }
                assembled.extend_from_slice(&frame.payload);
                if assembled.len() > 8 * 1024 * 1024 {
                    break 'frames;
                }
                if !frame.fin {
                    continue;
                }
                in_message = false;
                let message = String::from_utf8_lossy(&assembled).into_owned();
                assembled.clear();
                if let Some(answer) = protocol::answer(&shared, &message, &writer, connection) {
                    write_message(&writer, &answer);
                }
            }
        }
    }
    shared.connected.fetch_sub(1, Ordering::Relaxed);
    shared.disconnect(connection);
}

pub(crate) fn write_message(writer: &Arc<Mutex<TcpStream>>, message: &Value) {
    let text = message.to_string();
    let mut writer = writer.lock().unwrap_or_else(|error| error.into_inner());
    let _ = ws::send_text(&mut *writer, &text);
}

/// A path of the harness made absolute inside the project. `..` cannot leave it.
pub(crate) fn resolve(project: &Path, path: &Path) -> Option<PathBuf> {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        project.join(path)
    };
    let mut resolved = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::ParentDir => {
                resolved.pop();
            }
            Component::CurDir => {}
            other => resolved.push(other),
        }
    }
    inside(project, &resolved).then_some(resolved)
}

pub(crate) fn inside(project: &Path, path: &Path) -> bool {
    if cfg!(windows) {
        let normalize = |path: &Path| {
            path.to_string_lossy()
                .replace('/', "\\")
                .trim_end_matches('\\')
                .to_lowercase()
        };
        let project = normalize(project);
        let path = normalize(path);
        path == project
            || path
                .strip_prefix(&project)
                .is_some_and(|rest| rest.starts_with('\\'))
    } else {
        path.starts_with(project)
    }
}

pub(crate) fn same_path(left: &Path, right: &Path) -> bool {
    if cfg!(windows) {
        let normalize = |path: &Path| {
            path.to_string_lossy()
                .replace('/', "\\")
                .trim_end_matches('\\')
                .to_lowercase()
        };
        normalize(left) == normalize(right)
    } else {
        left == right
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lock_file_names_the_project_and_the_port() {
        let home = test_support::TempDir::new("bridge-lock");
        let project = home.path().join("project");
        fs::create_dir_all(&project).unwrap();

        let bridge = Bridge::start(
            home.path(),
            &project,
            Arc::new(Diagnostics::default()),
            Arc::new(Editors::default()),
        )
        .unwrap();

        assert!(
            bridge.lock_path().starts_with(home.path().join("ide")),
            "the lock file sits in the configuration home, under `ide`"
        );
        let lock = fs::read_to_string(bridge.lock_path()).unwrap();
        let value: Value = serde_json::from_str(&lock).unwrap();
        assert_eq!(value["ideName"], json!(IDE_NAME));
        assert_eq!(value["transport"], json!("ws"));
        assert_eq!(
            value["workspaceFolders"][0],
            json!(project.to_string_lossy())
        );
        assert_eq!(
            bridge.lock_path().file_name().unwrap().to_string_lossy(),
            format!("{}.lock", bridge.port())
        );
        assert!(
            value["authToken"]
                .as_str()
                .is_some_and(|token| !token.is_empty())
        );
        assert_eq!(
            bridge.environment(),
            [(PORT_VARIABLE.to_string(), bridge.port().to_string())]
        );

        let lock = bridge.lock_path().to_path_buf();
        drop(bridge);
        assert!(!lock.exists(), "the lock file goes with the bridge");
    }

    #[test]
    fn the_config_folder_is_the_variable_or_the_home() {
        let home = Path::new("C:/Users/me");
        let given = if cfg!(windows) {
            Path::new(r"C:\cfg\claude")
        } else {
            Path::new("/cfg/claude")
        };

        assert_eq!(
            config_home(Some(given), Some(home)),
            Some(given.to_path_buf()),
            "the variable names the folder itself, without a `.claude` of its own"
        );
        assert_eq!(config_home(None, Some(home)), Some(home.join(".claude")));
        assert_eq!(
            config_home(Some(Path::new("")), Some(home)),
            Some(home.join(".claude")),
            "an empty variable is no folder, as the harness reads it"
        );
        assert_eq!(
            config_home(Some(Path::new("relative/claude")), Some(home)),
            Some(home.join(".claude")),
            "a relative variable never puts the lock file under the current folder"
        );
        assert_eq!(config_home(None, None), None);
    }

    #[test]
    fn the_port_and_the_lock_file_go_with_the_bridge() {
        let home = test_support::TempDir::new("bridge-drop");
        let project = home.path().join("project");
        fs::create_dir_all(&project).unwrap();
        let bridge = Bridge::start(
            home.path(),
            &project,
            Arc::new(Diagnostics::default()),
            Arc::new(Editors::default()),
        )
        .unwrap();
        let port = bridge.port();
        let lock = bridge.lock_path().to_path_buf();
        let address = std::net::SocketAddr::from(([127, 0, 0, 1], port));
        assert!(TcpStream::connect_timeout(&address, Duration::from_secs(10)).is_ok());

        drop(bridge);

        assert!(!lock.exists(), "the lock file stays behind");
        assert!(
            TcpStream::connect_timeout(&address, Duration::from_millis(500)).is_err(),
            "the port still accepts connections"
        );
    }

    #[test]
    fn the_project_is_the_fence_of_a_path() {
        let project = Path::new("work/project");
        assert!(inside(project, Path::new("work/project/src/a.txt")));
        assert!(!inside(project, Path::new("work/project-other/a.txt")));
        assert_eq!(
            resolve(project, Path::new("src/../a.txt")),
            Some(PathBuf::from("work/project/a.txt"))
        );
        assert_eq!(resolve(project, Path::new("../secret")), None);
    }

    #[cfg(windows)]
    #[test]
    fn a_windows_path_is_compared_without_case_or_a_trailing_separator() {
        let project = Path::new(r"C:\work\project");
        assert!(inside(project, Path::new(r"c:\work\project\src\a.txt")));
        assert!(!inside(project, Path::new(r"C:\work\project-other\a.txt")));
        assert!(same_path(
            Path::new("C:/work/a.txt"),
            Path::new(r"c:\Work\A.txt")
        ));
    }

    #[test]
    fn diagnostics_come_by_file_or_whole() {
        let diagnostics = Diagnostics::default();
        diagnostics.replace(vec![FileDiagnostics {
            path: PathBuf::from("C:/work/project/a.txt"),
            diagnostics: vec![Diagnostic {
                line: 1,
                character: 2,
                severity: Severity::Error,
                message: "broken".to_string(),
                source: Some("ide".to_string()),
                code: None,
            }],
        }]);

        assert_eq!(diagnostics.files(None).len(), 1);
        let one = diagnostics.files(Some(Path::new("c:/work/project/A.txt")));
        assert_eq!(one.len(), if cfg!(windows) { 1 } else { 0 });
    }
}
