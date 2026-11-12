//! Debugging at the language boundary: the adapter-specific requests of the debug
//! plugin, the start of a debuggee and the session of `dap-client`.

use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime};

use dap_client::{
    BreakpointSpec, DebugService, DebugSessionToken, Debugger, Launch, OutputLine, ReloadOutcome,
    Snapshot, output_sender,
};
use jdtls_adapter::Autobuild;
use lsp_client::lsp_types::Uri;
use lsp_client::uri::file_uri;
use platform::CanonPath;
use proc_sup::ProcessSpec;
use semantic_router::{RouteError, Router};
use serde_json::{Value, json};
use tokio::runtime::Handle;
use tokio::sync::mpsc::UnboundedSender;

use crate::JavaProject;

/// How long the debuggee may take to announce its debug port.
const PORT_LIMIT: Duration = Duration::from_secs(60);
/// How long a JVM that somebody else started may take to listen.
const ATTACH_LIMIT: Duration = Duration::from_secs(300);
/// Between two attempts to reach the port of a debuggee in another process.
const ATTACH_POLL: Duration = Duration::from_millis(100);

/// The requests the debug plugin adds to the language server, kept here so that the
/// neutral client never learns them.
#[derive(Clone)]
pub struct DebugExtension {
    router: Arc<Router>,
    sessions: DebugSessions,
}

/// What a debug session changes for the language server: compilation on save is off
/// until the last session ends, and a reload reads the output folders from the last
/// point the session checked.
#[derive(Clone)]
struct DebugSessions {
    router: Arc<Router>,
    autobuild: Autobuild,
    /// The moment before which class files are already in the running program, for each
    /// debugger. One session reloading does not move the point of another.
    checked: Arc<Mutex<HashMap<u64, SystemTime>>>,
}

impl DebugSessions {
    fn new(router: Arc<Router>, autobuild: Autobuild) -> Self {
        Self {
            router,
            autobuild,
            checked: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn start(&self) -> DebugSession {
        if self.autobuild.begin() {
            self.configure(false);
        }
        DebugSession {
            sessions: self.clone(),
            ended: false,
        }
    }

    fn stop(&self) {
        if self.autobuild.end() {
            self.configure(true);
            self.checked
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clear();
        }
    }

    /// The moment a build has to be newer than to count as a change in this session.
    fn checked_at(&self, debugger: &Debugger) -> SystemTime {
        self.checked
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&debugger.id())
            .copied()
            .unwrap_or_else(SystemTime::now)
    }

    /// The debuggee of `debugger` is starting now, so its classes are measured from
    /// this moment.
    fn recheck(&self, debugger: &Debugger) {
        self.checked
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(debugger.id(), SystemTime::now());
    }

    /// The server reads the one setting from the map it gets; the rest stays as it was.
    fn configure(&self, enabled: bool) {
        self.router.notify(
            "workspace/didChangeConfiguration",
            json!({"settings": {"java": {"autobuild": {"enabled": enabled}}}}),
        );
    }
}

/// One debug session as the language side counts it. Dropping it ends the session, so
/// the server gets compilation back even when the window forgets an explicit end.
pub struct DebugSession {
    sessions: DebugSessions,
    ended: bool,
}

impl DebugSession {
    /// Ends the session now; the token does nothing more when it is dropped.
    pub fn end(mut self) {
        self.ended = true;
        self.sessions.stop();
    }
}

impl Drop for DebugSession {
    fn drop(&mut self) {
        if !self.ended {
            self.sessions.stop();
        }
    }
}

impl DebugSessionToken for DebugSession {}

/// What `vscode.java.resolveMainClass` says about a source file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResolvedMain {
    pub main_class: String,
    pub project: Option<String>,
}

impl DebugExtension {
    pub fn new(project: &JavaProject) -> Self {
        project.debug_extension()
    }

    pub(crate) fn with_router(router: Arc<Router>, autobuild: Autobuild) -> Self {
        Self {
            sessions: DebugSessions::new(router.clone(), autobuild),
            router,
        }
    }

    /// Marks the start of a debug session of a program this process launches.
    pub fn begin_session(&self) -> DebugSession {
        self.sessions.start()
    }

    /// The debuggee is starting now: the classes on disk are the ones it loads, so a
    /// later build is measured from this moment. The point belongs to this session.
    pub fn session_started(&self, debugger: &Debugger) {
        self.sessions.recheck(debugger);
    }

    /// Replaces in the running program the classes under `outputs` that a build wrote
    /// since the last check of this session, and returns what the adapter said.
    pub async fn reload_classes(&self, debugger: &Debugger, outputs: &[PathBuf]) -> ReloadOutcome {
        let since = self.sessions.checked_at(debugger);
        let changed = changed_class_files(outputs, since);
        if changed.is_empty() {
            return ReloadOutcome::default();
        }
        for path in &changed {
            if let Some(uri) = CanonPath::new(path).ok().and_then(|path| file_uri(&path)) {
                self.router.file_changed(uri.as_str());
            }
        }
        // The watched-file notification goes through the language protocol, while the
        // adapter answers on its own socket; the adapter reads the resources when the
        // server has updated them, so the request waits one turn of its event loop.
        tokio::time::sleep(Duration::from_millis(500)).await;
        let outcome = self.request_reload(debugger).await;
        self.sessions.recheck(debugger);
        outcome
    }

    /// The hot code replace request of the plugin, without a build in front of it.
    pub(crate) async fn request_reload(&self, debugger: &Debugger) -> ReloadOutcome {
        match debugger.request("redefineClasses", json!({})).await {
            Ok(body) => reload_outcome(&body),
            Err(error) => ReloadOutcome {
                reloaded: Vec::new(),
                error: Some(error),
            },
        }
    }

    /// The main class of the file, and the name of its project for `attach`.
    ///
    /// The plugin filters by the URI itself, but on Windows that filter finds nothing,
    /// so a miss falls back to the whole list and the path of the file.
    pub async fn resolve_main_class(&self, uri: &Uri) -> Result<Option<ResolvedMain>, RouteError> {
        let filtered = self
            .command("vscode.java.resolveMainClass", json!([uri.as_str()]))
            .await?;
        if let Some(found) = parse_main_class(&filtered, None) {
            return Ok(Some(found));
        }
        let all = self
            .command("vscode.java.resolveMainClass", json!([]))
            .await?;
        Ok(parse_main_class(
            &all,
            lsp_client::uri::file_path(uri).as_deref(),
        ))
    }

    /// The port of the debug adapter the plugin runs inside the language server.
    pub async fn start_debug_session(&self) -> Result<u16, RouteError> {
        let answer = self
            .command("vscode.java.startDebugSession", json!([]))
            .await?;
        port_of(&answer).ok_or(RouteError::TimedOut)
    }

    /// The plugin adds its commands to `workspace/executeCommand`, not as requests.
    async fn command(&self, command: &str, arguments: Value) -> Result<Value, RouteError> {
        let params = json!({"command": command, "arguments": arguments});
        self.router
            .extension("workspace/executeCommand", params)
            .await
    }
}

/// The debug plugin answers with a list of candidates, sometimes with one object.
/// With `file`, the candidate of that file; without it, the first candidate.
fn parse_main_class(answer: &Value, file: Option<&Path>) -> Option<ResolvedMain> {
    let entries: Vec<&Value> = match answer {
        Value::Array(items) => items.iter().collect(),
        Value::Object(_) => vec![answer],
        _ => return None,
    };
    let entry = match file {
        Some(file) => {
            let wanted = file.to_string_lossy().replace('\\', "/");
            entries.into_iter().find(|entry| {
                entry
                    .get("filePath")
                    .and_then(Value::as_str)
                    .is_some_and(|path| path.replace('\\', "/").eq_ignore_ascii_case(&wanted))
            })?
        }
        None => *entries.first()?,
    };
    let main_class = entry.get("mainClass").and_then(Value::as_str)?;
    if main_class.is_empty() {
        return None;
    }
    Some(ResolvedMain {
        main_class: main_class.to_owned(),
        project: entry
            .get("projectName")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .map(str::to_owned),
    })
}

/// A port as a number, a numeric string or an object with one of the usual names.
fn port_of(answer: &Value) -> Option<u16> {
    let number = |value: &Value| {
        value
            .as_u64()
            .or_else(|| value.as_str().and_then(|text| text.trim().parse().ok()))
    };
    let raw = number(answer).or_else(|| answer.as_object()?.get("port").and_then(number))?;
    u16::try_from(raw).ok()
}

/// The classes of a build as the adapter reports them, or why it refused.
fn reload_outcome(body: &Value) -> ReloadOutcome {
    let reloaded = body
        .get("changedClasses")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let error = body
        .get("errorMessage")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|message| !message.is_empty())
        .map(str::to_owned);
    ReloadOutcome { reloaded, error }
}

/// The class files under `outputs` that a build wrote at or after `since`, sorted. A
/// missing folder is not an error: a tool may not have created it yet.
pub(crate) fn changed_class_files(outputs: &[PathBuf], since: SystemTime) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for root in outputs {
        collect_classes(root, since, &mut found);
    }
    found.sort();
    found.dedup();
    found
}

fn collect_classes(directory: &Path, since: SystemTime, found: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            collect_classes(&path, since, found);
            continue;
        }
        let is_class = path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("class"));
        if !is_class {
            continue;
        }
        let written = entry.metadata().and_then(|metadata| metadata.modified());
        if written.is_ok_and(|written| written >= since) {
            found.push(path);
        }
    }
}

/// The target of a debug session as the runner prepared it.
pub struct JavaDebugger {
    runtime: Handle,
    extension: DebugExtension,
}

impl JavaDebugger {
    pub fn new(project: Arc<JavaProject>, runtime: Handle) -> Self {
        Self {
            runtime,
            extension: project.debug_extension(),
        }
    }

    fn extension(&self) -> DebugExtension {
        self.extension.clone()
    }

    #[allow(clippy::too_many_arguments)]
    fn attach_with(
        &self,
        host: String,
        port: u16,
        name: String,
        project: Option<String>,
        breakpoints: Vec<BreakpointSpec>,
        watch: Vec<String>,
        exception_filters: Vec<String>,
        wait: bool,
    ) -> Result<Debugger, String> {
        if host.trim().is_empty() {
            return Err("the host is empty".to_owned());
        }
        let (commands, receiver) = tokio::sync::mpsc::unbounded_channel();
        let (snapshots, incoming) = channel();
        let debugger = Debugger::new(commands, incoming);
        let extension = self.extension();
        extension.session_started(&debugger);
        let failures = snapshots.clone();
        self.runtime.spawn(async move {
            let message = attach_session(
                extension,
                &host,
                port,
                name,
                project,
                breakpoints,
                watch,
                exception_filters,
                failures.clone(),
                wait,
            )
            .await;
            match message {
                Ok(launch) => {
                    let _ = dap_client::run(launch, receiver, failures).await;
                }
                Err(message) => {
                    let snapshot = Snapshot {
                        state: dap_client::DebugState::Terminated,
                        console: vec![dap_client::ConsoleLine::new(message, true)],
                        process_id: None,
                        ..Snapshot::default()
                    };
                    let _ = failures.send(Arc::new(snapshot));
                }
            }
        });
        Ok(debugger)
    }
}

/// Takes the standard input of the debuggee and gives its writer to the session, so the
/// console can type into the program. The pipe stays open until Send EOF, the end of the
/// process or the end of the session.
fn with_program_input(debugger: Debugger, process: &mut proc_sup::Process) -> Debugger {
    let Some(stdin) = process.take_stdin() else {
        return debugger;
    };
    let (input, writer) = proc_sup::Input::channel();
    writer.start(stdin);
    debugger.with_input(input)
}

impl DebugService for JavaDebugger {
    fn start(
        &self,
        program: ProcessSpec,
        name: String,
        entry: Option<PathBuf>,
        breakpoints: Vec<BreakpointSpec>,
        watch: Vec<String>,
        exception_filters: Vec<String>,
    ) -> Result<Debugger, String> {
        let mut process = program.start().map_err(|error| error.to_string())?;
        let process_id = process.pid();
        let stderr = process
            .take_stderr()
            .ok_or_else(|| "the debuggee has no error stream".to_owned())?;
        let (port_sender, port_receiver) = channel();
        let (output, output_receiver) = output_sender();
        let stdout = process.take_stdout();
        let stdout_output = output.clone();
        let stdout_port = port_sender.clone();
        thread::spawn(move || {
            if let Some(stdout) = stdout {
                read_stream(stdout, stdout_output, false, Some(stdout_port));
            }
        });
        let stderr_output = output.clone();
        thread::spawn(move || read_stream(stderr, stderr_output, true, Some(port_sender)));

        let (commands, receiver) = tokio::sync::mpsc::unbounded_channel();
        let (snapshots, incoming) = channel();
        let debugger = with_program_input(Debugger::new(commands, incoming), &mut process);
        let extension = self.extension();
        extension.session_started(&debugger);
        let failures = snapshots.clone();
        self.runtime.spawn(async move {
            match start_session(
                extension,
                process_id,
                port_receiver,
                name,
                entry,
                breakpoints,
                watch,
                exception_filters,
                snapshots,
            )
            .await
            {
                Ok(session) => {
                    // The process lives until the session ends; dropping it kills the debuggee.
                    let _process = process;
                    if let Err(error) = dap_client::run_with_output(
                        session,
                        receiver,
                        Some(output_receiver),
                        failures.clone(),
                    )
                    .await
                    {
                        let snapshot = Snapshot {
                            state: dap_client::DebugState::Terminated,
                            console: vec![dap_client::ConsoleLine::new(error.to_string(), true)],
                            process_id: Some(process_id),
                            ..Snapshot::default()
                        };
                        let _ = failures.send(Arc::new(snapshot));
                    }
                }
                Err(message) => {
                    let snapshot = Snapshot {
                        state: dap_client::DebugState::Terminated,
                        console: vec![dap_client::ConsoleLine::new(message, true)],
                        process_id: Some(process_id),
                        ..Snapshot::default()
                    };
                    let _ = failures.send(Arc::new(snapshot));
                }
            }
        });
        Ok(debugger)
    }

    fn attach(
        &self,
        host: String,
        port: u16,
        name: String,
        project: Option<String>,
        breakpoints: Vec<BreakpointSpec>,
        watch: Vec<String>,
        exception_filters: Vec<String>,
    ) -> Result<Debugger, String> {
        self.attach_with(
            host,
            port,
            name,
            project,
            breakpoints,
            watch,
            exception_filters,
            true,
        )
    }

    fn attach_ready(
        &self,
        host: String,
        port: u16,
        name: String,
        project: Option<String>,
        breakpoints: Vec<BreakpointSpec>,
        watch: Vec<String>,
        exception_filters: Vec<String>,
    ) -> Result<Debugger, String> {
        self.attach_with(
            host,
            port,
            name,
            project,
            breakpoints,
            watch,
            exception_filters,
            false,
        )
    }

    fn begin_session(&self) -> Option<Box<dyn DebugSessionToken>> {
        Some(Box::new(self.extension.begin_session()))
    }

    fn reload_classes(
        &self,
        debugger: Debugger,
        outputs: Vec<PathBuf>,
        done: Sender<ReloadOutcome>,
    ) -> bool {
        let extension = self.extension();
        self.runtime.spawn(async move {
            let outcome = extension.reload_classes(&debugger, &outputs).await;
            let _ = done.send(outcome);
        });
        true
    }
}

#[allow(clippy::too_many_arguments)]
async fn start_session(
    extension: DebugExtension,
    process_id: u32,
    port_receiver: std::sync::mpsc::Receiver<u16>,
    name: String,
    entry: Option<PathBuf>,
    breakpoints: Vec<BreakpointSpec>,
    watch: Vec<String>,
    exception_filters: Vec<String>,
    snapshots: Sender<Arc<Snapshot>>,
) -> Result<Launch, String> {
    let resolved = match entry {
        Some(entry) => {
            let uri = CanonPath::new(&entry)
                .ok()
                .and_then(|path| file_uri(&path))
                .ok_or_else(|| format!("{} has no file URI", entry.display()))?;
            match extension.resolve_main_class(&uri).await {
                Ok(found) => found,
                Err(error) => {
                    let _ = snapshots.send(Arc::new(Snapshot {
                        state: dap_client::DebugState::Terminated,
                        console: vec![dap_client::ConsoleLine::new(
                            format!("cannot resolve the main class: {error}"),
                            true,
                        )],
                        ..Snapshot::default()
                    }));
                    None
                }
            }
        }
        None => None,
    };
    let adapter = extension
        .start_debug_session()
        .await
        .map_err(|error| format!("cannot start the debug session: {error}"))?;

    let port = tokio::task::spawn_blocking(move || port_receiver.recv_timeout(PORT_LIMIT))
        .await
        .map_err(|error| error.to_string())
        .map_err(|_| "waiting for the debug port failed".to_owned())?
        .map_err(|_| {
            "the debuggee did not announce a debug port; is the debug agent allowed?".to_owned()
        })?;

    Ok(Launch {
        address: format!("127.0.0.1:{adapter}"),
        adapter_id: "java".to_owned(),
        project: resolved.as_ref().and_then(|main| main.project.clone()),
        attach_host: Some("127.0.0.1".to_owned()),
        attach_port: Some(port),
        name: resolved
            .as_ref()
            .map_or(name.clone(), |main| main.main_class.clone()),
        breakpoints,
        exception_filters,
        watch,
        process_id: Some(process_id),
    })
}

#[allow(clippy::too_many_arguments)]
async fn attach_session(
    extension: DebugExtension,
    host: &str,
    port: u16,
    name: String,
    project: Option<String>,
    breakpoints: Vec<BreakpointSpec>,
    watch: Vec<String>,
    exception_filters: Vec<String>,
    _snapshots: Sender<Arc<Snapshot>>,
    wait: bool,
) -> Result<Launch, String> {
    if wait {
        wait_for_port(host, port, ATTACH_LIMIT)
            .await
            .map_err(|_| format!("nothing is listening on {host}:{port}"))?;
    }
    let adapter = extension
        .start_debug_session()
        .await
        .map_err(|error| format!("cannot start the debug session: {error}"))?;
    Ok(Launch {
        address: format!("127.0.0.1:{adapter}"),
        adapter_id: "java".to_owned(),
        project,
        attach_host: Some(host.to_owned()),
        attach_port: Some(port),
        name,
        breakpoints,
        exception_filters,
        watch,
        process_id: None,
    })
}

/// Waits until a JVM that somebody else started listens on the port. A connection without
/// the handshake leaves the debugger listening for the real one. A probe of a JVM this
/// process started reads the agent line instead, so the JVM never sees a broken handshake.
async fn wait_for_port(host: &str, port: u16, limit: Duration) -> Result<(), ()> {
    let started = std::time::Instant::now();
    loop {
        if tokio::net::TcpStream::connect((host, port)).await.is_ok() {
            return Ok(());
        }
        if started.elapsed() >= limit {
            return Err(());
        }
        tokio::time::sleep(ATTACH_POLL).await;
    }
}

/// Reads a stream to the end, keeps the console fed and names the debug port once.
fn read_stream(
    stream: impl std::io::Read,
    output: UnboundedSender<OutputLine>,
    error: bool,
    port: Option<Sender<u16>>,
) {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    let mut announced = false;
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {}
            Err(_) => break,
        }
        let text = line.trim_end_matches(['\r', '\n']).to_owned();
        if text.is_empty() {
            continue;
        }
        if !announced
            && let Some(found) = listening_port(&text)
            && let Some(port) = &port
        {
            announced = true;
            let _ = port.send(found);
        }
        let _ = output.send(OutputLine { text, error });
    }
}

/// `Listening for transport dt_socket at address: 54321`.
fn listening_port(line: &str) -> Option<u16> {
    let at = line.find("Listening for transport")?;
    let address = line[at..].split("address:").nth(1)?.trim();
    address
        .rsplit(':')
        .next()
        .and_then(|port| port.trim().parse().ok())
}

#[cfg(test)]
mod tests {
    #[cfg(windows)]
    use std::io::Read;

    use serde_json::json;

    use super::*;

    #[test]
    fn the_debug_port_is_read_from_the_agent_line() {
        assert_eq!(
            listening_port("Listening for transport dt_socket at address: 54321"),
            Some(54321)
        );
        assert_eq!(
            listening_port("Listening for transport dt_socket at address: 127.0.0.1:1234"),
            Some(1234)
        );
        // A build tool may put its own prefix in front of the agent line.
        assert_eq!(
            listening_port("[INFO] Listening for transport dt_socket at address: 58828"),
            Some(58828)
        );
        assert_eq!(listening_port("started"), None);
    }

    #[test]
    fn a_main_class_answer_may_be_a_list_or_an_object() {
        let listed = json!([{"mainClass": "demo.Main", "projectName": "demo"}]);
        assert_eq!(
            parse_main_class(&listed, None),
            Some(ResolvedMain {
                main_class: "demo.Main".into(),
                project: Some("demo".into())
            })
        );
        let single = json!({"mainClass": "demo.Main"});
        assert_eq!(
            parse_main_class(&single, None),
            Some(ResolvedMain {
                main_class: "demo.Main".into(),
                project: None
            })
        );
        assert_eq!(parse_main_class(&json!([]), None), None);
    }

    #[test]
    fn a_port_comes_as_a_number_a_string_or_a_field() {
        assert_eq!(port_of(&json!(4321)), Some(4321));
        assert_eq!(port_of(&json!("4321")), Some(4321));
        assert_eq!(port_of(&json!({"port": 4321})), Some(4321));
        assert_eq!(port_of(&json!({})), None);
    }

    /// One session reloading must not move the point from which the next session
    /// measures its own class files.
    #[tokio::test]
    async fn each_session_has_its_own_hot_swap_checkpoint() {
        let (_revision, status) = tokio::sync::watch::channel(sidecar::Status {
            state: sidecar::State::Stopped,
            generation: 0,
            alive: false,
            client: None,
            process_id: None,
            crashes: 0,
            last_error: None,
        });
        let sessions = DebugSessions::new(Router::new(status), Autobuild::default());
        let debugger = || {
            let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
            let (_snapshots, incoming) = channel();
            Debugger::new(commands, incoming)
        };

        let first = debugger();
        sessions.recheck(&first);
        let first_point = sessions.checked_at(&first);
        std::thread::sleep(Duration::from_millis(30));

        let second = debugger();
        sessions.recheck(&second);
        let second_point = sessions.checked_at(&second);
        assert!(
            second_point > first_point,
            "a later session has its own point"
        );

        sessions.recheck(&first);
        assert!(
            sessions.checked_at(&first) > second_point,
            "the first session moves only its own point"
        );
        assert_eq!(
            sessions.checked_at(&second),
            second_point,
            "the point of the second session stays where it was"
        );
    }

    #[test]
    fn only_classes_a_build_wrote_after_the_check_are_reloaded() {
        let root = test_support::scratch("hot-swap");
        let nested = root.join("demo");
        fs::create_dir_all(&nested).unwrap();
        let old = root.join("Old.class");
        let new = nested.join("New.class");
        let resource = nested.join("note.txt");
        fs::write(&old, b"old").unwrap();
        fs::write(&new, b"new").unwrap();
        fs::write(&resource, b"text").unwrap();
        let then = SystemTime::now() - Duration::from_secs(60);
        fs::File::options()
            .write(true)
            .open(&old)
            .unwrap()
            .set_modified(then - Duration::from_secs(1))
            .unwrap();
        fs::File::options()
            .write(true)
            .open(&resource)
            .unwrap()
            .set_modified(SystemTime::now())
            .unwrap();

        let found = changed_class_files(&[root.clone(), root.join("missing")], then);

        assert_eq!(found, vec![new]);
    }

    #[test]
    fn a_hot_swap_answer_names_classes_or_the_reason() {
        let reloaded = reload_outcome(&json!({"changedClasses": ["demo.Probe", "demo.Probe$1"]}));
        assert_eq!(reloaded.reloaded, ["demo.Probe", "demo.Probe$1"]);
        assert_eq!(reloaded.error, None);

        let refused = reload_outcome(&json!({
            "changedClasses": [],
            "errorMessage": "Add/Delete method not implemented",
        }));
        assert!(refused.reloaded.is_empty());
        assert_eq!(
            refused.error.as_deref(),
            Some("Add/Delete method not implemented")
        );

        let empty = reload_outcome(&json!({"errorMessage": null}));
        assert!(empty.reloaded.is_empty());
        assert_eq!(empty.error, None);
    }

    #[cfg(windows)]
    #[test]
    fn the_standard_input_of_the_debuggee_stays_open_at_start() {
        let program = ProcessSpec::new("powershell.exe")
            .args([
                "-NoProfile",
                "-Command",
                "$line = [Console]::In.ReadLine(); \
                 if ($line -eq $null) { Write-Output 'closed' } \
                 else { Write-Output ('got:' + $line) }",
            ])
            .input();
        let mut process = program.start().unwrap();
        let mut stdout = process.take_stdout().unwrap();
        let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let (_snapshots, incoming) = channel();
        let debugger = with_program_input(Debugger::new(commands, incoming), &mut process);
        assert!(
            debugger.takes_input(),
            "the console may type into the program"
        );

        // A program whose input was closed at the start would read the end of the input
        // and exit.
        thread::sleep(Duration::from_millis(400));
        assert!(
            process.try_wait().unwrap().is_none(),
            "the program still waits for its input"
        );

        debugger.send_line("hello");
        let mut text = String::new();
        stdout.read_to_string(&mut text).unwrap();
        assert!(text.contains("got:hello"), "{text:?}");
    }

    /// The request of the plugin goes to a trial adapter and its answer is read here.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_trial_adapter_answers_the_hot_swap_request() {
        let server = lang_fixture::MockDap::start().unwrap();
        let (commands, receiver) = tokio::sync::mpsc::unbounded_channel();
        let (snapshots, incoming) = channel();
        let launch = Launch {
            address: server.address().to_owned(),
            adapter_id: "fixture".into(),
            name: "fixture".into(),
            ..Launch::default()
        };
        let running = tokio::spawn(dap_client::run(launch, receiver, snapshots));
        let debugger = Debugger::new(commands, incoming);
        let (_status, receiver) = tokio::sync::watch::channel(sidecar::Status {
            state: sidecar::State::Stopped,
            generation: 0,
            alive: false,
            client: None,
            process_id: None,
            crashes: 0,
            last_error: None,
        });
        let extension = DebugExtension::with_router(Router::new(receiver), Autobuild::default());

        let outcome = extension.request_reload(&debugger).await;

        assert_eq!(outcome.reloaded, ["demo.Probe", "demo.Probe$1"]);
        assert_eq!(outcome.error, None);
        assert!(debugger.send(dap_client::Command::Disconnect));
        running.await.unwrap().unwrap();
        server.join();
    }
}
