//! How to run jdtls: the pinned pair, JVM settings, runtimes and client settings.
//!
//! Facts behind each value are in `spikes/s9-jdtls/S9.md`. Downloading the pair
//! with consent and hash checks comes later, for now the paths are given.

use std::collections::HashMap;
use std::fs;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use base::program::executable;
use lsp_client::lsp_types::notification::{DidChangeConfiguration, Notification};
use lsp_client::lsp_types::{
    ClientCapabilities, ClientInfo, InitializeParams, InitializeResult, TraceValue, WorkspaceFolder,
};
use lsp_client::uri::file_uri;
use lsp_client::{
    Client, CodeLensCapability, CodeLensSetup, ResponseError, SemanticTokensCapability,
    SemanticTokensSetup,
};
use platform::CanonPath;
use serde_json::{Value, json};
use sidecar::{Plan, ServerSpec, State};
use tokio::sync::watch;

mod code_style;
mod import;
mod install;
mod log;
mod startup;

pub use import::{BuildTool, ImportFailure, is_build_file};

/// The server release this adapter is tested with, for the message about its JDK.
const JDTLS_VERSION: &str = "1.63";

/// The oldest Java that runs the bundles of [`JDTLS_VERSION`].
pub const MIN_JAVA_VERSION: u32 = 25;

/// The heap of the first start, in MiB, and of every start after an out-of-memory
/// end: 1, 2, 4 and 6 GiB. The last value stays.
const HEAP_LADDER_MB: &[u32] = &[1024, 2048, 4096, 6144];

/// The smallest heap of the first start.
const FIRST_HEAP_MB: u32 = HEAP_LADDER_MB[0];
/// The largest heap of the first start on a very large machine.
const LARGEST_FIRST_HEAP_MB: u32 = 4096;

/// The JVM settings of the server. The heap comes from the ladder and is added
/// separately, so the list has no `-Xmx`.
const JVM_SETTINGS: &[&str] = &[
    // A small start keeps the committed memory near the live set of an idle server.
    "-Xms100m",
    // Spend at most one part in five in collections and grow the heap instead.
    "-XX:GCTimeRatio=4",
    // Do not map jars into the address space, so their bytes stay out of the
    // working set of the server.
    "-Dsun.zip.disableMemoryMapping=true",
    // Out of memory ends the JVM with EXIT_OUT_OF_MEMORY, the next start gets more.
    "-XX:+ExitOnOutOfMemoryError",
    // Ctrl+B into a jar without pom.properties asks Maven Central inline.
    "-Djava.lsp.mavensearch.timeout=1",
    // A system property of the filesystem bundle, not an LSP setting.
    "-Djava.import.generatesMetadataFilesAtProjectRoot=false",
];

/// The collector of the server and how it gives unused memory back. The list is added
/// by [`memory_arguments`] on its own, so another change of the start does not touch it.
///
/// JDK 25 also knows `G1PeriodicGCInterval` and `UseStringDeduplication`, but G1 with
/// them kept 866.6 MiB of Private Working Set on PetClinic after five idle minutes
/// (measured), so the server keeps ParallelGC, which already returned the unused heap:
/// the free-ratio bounds make it shrink after a collection instead of keeping the peak.
const MEMORY_SETTINGS: &[&str] = &[
    "-XX:+UseParallelGC",
    "-XX:MinHeapFreeRatio=20",
    "-XX:MaxHeapFreeRatio=40",
    "-XX:AdaptiveSizePolicyWeight=90",
];

/// From `bin/jdtls.py` of the release.
const LAUNCH_SETTINGS: &[&str] = &[
    "-Declipse.application=org.eclipse.jdt.ls.core.id1",
    "-Dosgi.bundles.defaultStartLevel=4",
    "-Declipse.product=org.eclipse.jdt.ls.core.product",
    "-Dosgi.checkConfiguration=true",
    "-Dosgi.sharedConfiguration.area.readOnly=true",
    "-Dosgi.configuration.cascaded=true",
    "-Djdk.xml.maxGeneralEntitySizeLimit=0",
    "-Djdk.xml.totalEntitySizeLimit=0",
    "--add-modules=ALL-SYSTEM",
    "--add-opens",
    "java.base/java.util=ALL-UNNAMED",
    "--add-opens",
    "java.base/java.lang=ALL-UNNAMED",
];

/// The stderr of the JVM, which the sidecar writes and the crash checks read.
const LOG_FILE: &str = "jdtls-stderr.log";

/// The heading of the JVM output that joins the log of the server messages.
const JVM_OUTPUT_HEADING: &str = "--- JVM output ---";

/// A JVM of this version knows `--sun-misc-unsafe-memory-access`, which silences the
/// warning of JEP 498 about Guice and other libraries of the bundles.
const UNSAFE_MEMORY_FLAG_JAVA: u32 = 24;

/// The configuration folder of the jdtls release for this system.
const CONFIG_DIRECTORY: &str = if cfg!(windows) {
    "config_win"
} else if cfg!(target_os = "macos") {
    "config_mac"
} else {
    "config_linux"
};

/// The Gradle daemon of an import stops after ten idle minutes instead of the three
/// hours it keeps by default. The property travels as an import argument, as S9 did.
const GRADLE_IDLE_TIMEOUT_MS: u64 = 600_000;
const GRADLE_IDLE_TIMEOUT_PROPERTY: &str = "org.gradle.daemon.idletimeout";
/// A test asks for a shorter timeout through the environment of the server.
const GRADLE_IDLE_TIMEOUT_ENV: &str = "IDE_GRADLE_IDLE_TIMEOUT_MS";

/// The daemon registry of the imports of the IDE, under its state folder. A registry of
/// the machine may still name the JDK of a daemon whose folder was removed, and Gradle
/// then fails to connect before it starts a daemon on the JVM the user chose. The server
/// keeps a registry of its own per JVM, so a registry left by another JVM cannot follow
/// the user to the one just chosen.
const GRADLE_REGISTRY_PROPERTY: &str = "org.gradle.daemon.registry.base";
const GRADLE_REGISTRY_DIR: &str = "gradle-daemons";

/// The registry of the JVM that starts Gradle. A new choice of the user is a new folder,
/// which no daemon of the previous choice can block.
fn gradle_registry_dir(config: &JdtlsConfig) -> PathBuf {
    use std::hash::{Hash, Hasher};
    let home = config
        .gradle_java_home
        .as_ref()
        .unwrap_or(&config.java_home);
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    home.to_string_lossy().to_lowercase().hash(&mut hasher);
    config
        .state_dir
        .join(format!("{GRADLE_REGISTRY_DIR}-{:016x}", hasher.finish()))
}

/// Whether the server may compile the project by itself.
///
/// A debug launch turns it off: the running classes were compiled by the build tool, and
/// bytes from another compiler cannot replace them. The count keeps it off until the last
/// debug session ends, and a server that restarts during a session reads the same state.
#[derive(Clone, Debug, Default)]
pub struct Autobuild {
    sessions: Arc<AtomicUsize>,
}

impl Autobuild {
    /// Starts one debug session; true when it was the first, so the server has to be told.
    pub fn begin(&self) -> bool {
        self.sessions.fetch_add(1, Ordering::SeqCst) == 0
    }

    /// Ends one debug session; true when it was the last, so the server has to be told.
    pub fn end(&self) -> bool {
        self.sessions.fetch_sub(1, Ordering::SeqCst) == 1
    }

    pub fn enabled(&self) -> bool {
        self.sessions.load(Ordering::SeqCst) == 0
    }

    pub fn active(&self) -> bool {
        !self.enabled()
    }
}

/// Which parameter names jdtls puts before arguments, as `java.inlayHints` names them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InlayParameterNames {
    None,
    /// Only before a literal or another argument whose meaning the name explains.
    #[default]
    Literals,
    All,
}

impl InlayParameterNames {
    fn as_setting(self) -> &'static str {
        match self {
            InlayParameterNames::None => "none",
            InlayParameterNames::Literals => "literals",
            InlayParameterNames::All => "all",
        }
    }
}

/// What the server shows in the text. The editor asks for it on every settings change,
/// so the server applies it without a restart.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct InlayHints {
    parameters: InlayParameterNames,
    types: bool,
}

/// The labels the server puts above declarations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CodeVision {
    usages: bool,
    implementations: bool,
}

impl Default for CodeVision {
    fn default() -> Self {
        Self {
            usages: true,
            implementations: true,
        }
    }
}

/// The `.prefs` file of the checks the user changed, under the state folder of the
/// server. The server reads the path from `java.settings.url`, and its preference
/// listener (`StandardProjectsManager$3.preferencesChange`) reloads the file only when
/// the path itself changes. The name therefore carries the content: the same choices
/// give the same path, new choices give a new one, and `workspace/didChangeConfiguration`
/// applies them to the running server.
const INSPECTION_PREFS_PREFIX: &str = "inspections-";
const INSPECTION_PREFS_SUFFIX: &str = ".prefs";

/// The Lombok agent of the project: the jar copied next to the server state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lombok {
    pub version: String,
    pub jar: PathBuf,
}

/// Where the copy of the agent waits: under the state folder of the server, not in the
/// repository that the build tool may update while the server runs.
const LOMBOK_DIR: &str = "lombok";

/// A JDK for compiling projects, as in `java.configuration.runtimes`.
#[derive(Clone, Debug)]
pub struct Runtime {
    /// `JavaSE-17`, `JavaSE-1.8`.
    pub name: String,
    pub home: PathBuf,
    pub default: bool,
}

/// Where the pinned pair lives and how to run it on one project.
#[derive(Clone, Debug)]
pub struct JdtlsConfig {
    /// The unpacked release of jdtls.
    pub jdtls_home: PathBuf,
    /// `com.microsoft.java.debug.plugin-*.jar`.
    pub debug_plugin: PathBuf,
    /// A JDK 25 or newer to run jdtls itself.
    pub java_home: PathBuf,
    pub project_root: PathBuf,
    /// Per workspace, holds `-data`, the OSGi configuration and the log.
    pub state_dir: PathBuf,
    pub runtimes: Vec<Runtime>,
    /// Maven `settings.xml`, for a local repository outside the default place.
    pub maven_settings: Option<PathBuf>,
    /// The Gradle installation, when the user names one instead of the wrapper.
    pub gradle_home: Option<PathBuf>,
    pub gradle_user_home: Option<PathBuf>,
    /// The JDK of the project, so that the daemon matches the command line
    /// and the project level is not taken from the JDK that runs jdtls.
    pub gradle_java_home: Option<PathBuf>,
    /// No network for dependency resolution during import.
    pub offline: bool,
    pub environment: HashMap<String, String>,
    /// The most heap of the server in megabytes, which is also its heap from the first
    /// start on. `None` lets the server pick the heap from the size of the machine and
    /// grow it after an out-of-memory end.
    pub max_heap_mb: Option<u32>,
}

/// jdtls as a server of the sidecar.
/// The exit code of a JVM that `-XX:+ExitOnOutOfMemoryError` ended.
const EXIT_OUT_OF_MEMORY: i32 = 3;

/// Held while the server runs: a second IDE on the same project gets no second jdtls,
/// which would break the shared workspace folder.
const LOCK_FILE: &str = "jdtls.lock";

pub struct Jdtls {
    config: JdtlsConfig,
    root: CanonPath,
    launcher_jar: PathBuf,
    /// The release this state folder belongs to; another one clears the OSGi cache and
    /// the model of the workspace.
    installation: install::InstallMark,
    autobuild: Autobuild,
    heap_mb: AtomicU32,
    /// The heap the user set in megabytes, zero when the server chooses. A set heap is the
    /// heap of every start: the ladder does not grow it.
    max_heap_mb: AtomicU32,
    /// `java.configuration.runtimes` as the server reads it at every start. Project
    /// Structure changes it without starting a new process of the window.
    runtimes: Mutex<Vec<Runtime>>,
    /// The Inlay Hints settings of the window, read on every settings question.
    inlay: Mutex<InlayHints>,
    /// The Code Vision settings of the window, read on every settings question.
    code_vision: Mutex<CodeVision>,
    /// `org.eclipse.jdt.core.prefs` entries of the checks the user changed. The server
    /// reads the file named by `java.settings.url` when it initializes.
    inspection_prefs: Mutex<Vec<(String, String)>>,
    /// The formatter profile and import choices of the project code style, prepared
    /// when the server starts.
    code_style: Option<code_style::Prepared>,
    /// The Lombok agent of the running server, if the project uses Lombok.
    lombok: Mutex<Option<Lombok>>,
    /// When the last start began, so that a stale log is not read as its cause.
    started: Mutex<Option<SystemTime>>,
    /// The log of the messages of the server, open while it runs.
    messages: Mutex<Option<log::ServerLog>>,
    /// How much of the stderr of the JVM is already in the log of messages.
    jvm_output: Mutex<u64>,
    /// The latest import failure the server reported, and the imports running now.
    imports: import::ImportState,
    /// The legend of the semantic tokens, which the server gives at `initialize`.
    semantic_tokens: SemanticTokensSetup,
    /// What the server gives about its code lenses at `initialize`.
    code_lens: CodeLensSetup,
    _lock: fs::File,
}

impl Jdtls {
    pub fn new(config: JdtlsConfig) -> io::Result<Self> {
        let root = CanonPath::new(&config.project_root)?;
        let launcher_jar = equinox_launcher(&config.jdtls_home)?;
        let installation = install::InstallMark::of(&config.jdtls_home, &launcher_jar)?;
        fs::create_dir_all(&config.state_dir)?;
        let code_style = code_style::read_project_style(&config.project_root)
            .map(|style| code_style::prepare(&config.state_dir, &style));
        // The default choices name a file too, so the server always reads one at its
        // start and never misses the path it is given.
        let prefs = code_style
            .as_ref()
            .map(|style| style.prefs.clone())
            .unwrap_or_default();
        if let Err(error) = fs::write(
            inspection_prefs_path(&config.state_dir, &prefs),
            code_style::prefs_text(&prefs),
        ) {
            eprintln!("the inspections of the project are not saved: {error}");
        }
        let lock = fs::File::create(config.state_dir.join(LOCK_FILE))?;
        if lock.try_lock().is_err() {
            let message = "jdtls already runs for this project in another window";
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, message));
        }
        let max_heap = config.max_heap_mb.filter(|mb| *mb > 0).unwrap_or(0);
        let heap = if max_heap > 0 {
            max_heap
        } else {
            first_heap_mb(platform::physical_memory())
        };
        Ok(Self {
            runtimes: Mutex::new(config.runtimes.clone()),
            inlay: Mutex::new(InlayHints::default()),
            code_vision: Mutex::new(CodeVision::default()),
            inspection_prefs: Mutex::new(Vec::new()),
            code_style,
            lombok: Mutex::new(None),
            config,
            root,
            launcher_jar,
            installation,
            autobuild: Autobuild::default(),
            heap_mb: AtomicU32::new(heap),
            max_heap_mb: AtomicU32::new(max_heap),
            started: Mutex::new(None),
            messages: Mutex::new(None),
            jvm_output: Mutex::new(0),
            imports: import::ImportState::new(),
            semantic_tokens: SemanticTokensSetup::new(),
            code_lens: CodeLensSetup::new(),
            _lock: lock,
        })
    }

    /// Sets the most heap the server takes from its next start on; `None` lets the server
    /// choose again. A running server keeps the heap it started with.
    pub fn set_max_heap_mb(&self, megabytes: Option<u32>) {
        let max = megabytes.filter(|mb| *mb > 0).unwrap_or(0);
        self.max_heap_mb.store(max, Ordering::Relaxed);
        let heap = if max > 0 {
            max
        } else {
            first_heap_mb(platform::physical_memory())
        };
        self.heap_mb.store(heap, Ordering::Relaxed);
    }

    /// The legend the server declared, filled when it answers `initialize`.
    pub fn semantic_tokens(&self) -> SemanticTokensSetup {
        self.semantic_tokens.clone()
    }

    /// What the server declared about its code lenses, filled at `initialize`.
    pub fn code_lens(&self) -> CodeLensSetup {
        self.code_lens.clone()
    }

    /// The SDK the server itself runs on.
    pub fn java_home(&self) -> &Path {
        &self.config.java_home
    }

    /// The SDKs the server compiles and resolves with. The caller restarts the server:
    /// the list is read when it initializes.
    pub fn set_runtimes(&self, runtimes: Vec<Runtime>) {
        *self
            .runtimes
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = runtimes;
    }

    /// The version of Lombok the server runs with, for the pages that show it.
    pub fn lombok_version(&self) -> Option<String> {
        self.lombok
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .as_ref()
            .map(|lombok| lombok.version.clone())
    }

    /// Uses the Lombok jar of the project, or none. The jar is copied under the state
    /// folder, so the build tool may replace its own copy while the server runs. True
    /// when the server has to start again: the agent is read by the JVM at its start.
    pub fn set_lombok(&self, lombok: Option<Lombok>) -> io::Result<bool> {
        let mut current = self
            .lombok
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let unchanged = match (&*current, &lombok) {
            (None, None) => true,
            // The copy is enough: the repository may have changed the source file.
            (Some(active), Some(wanted)) => {
                active.version == wanted.version && active.jar.is_file()
            }
            _ => false,
        };
        if unchanged {
            return Ok(false);
        }
        let stored = match lombok {
            Some(lombok) => {
                let jar = copy_agent(&self.config.state_dir, &lombok)?;
                Some(Lombok {
                    version: lombok.version,
                    jar,
                })
            }
            None => None,
        };
        *current = stored;
        Ok(true)
    }

    /// The live setting of the server, so a restart during a debug session keeps it off.
    pub fn settings(&self) -> Value {
        let runtimes = self
            .runtimes
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone();
        let inlay = *self
            .inlay
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let code_vision = *self
            .code_vision
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let prefs = self
            .inspection_prefs
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone();
        let mut combined = self
            .code_style
            .as_ref()
            .map(|style| style.prefs.clone())
            .unwrap_or_default();
        combined.extend(prefs);
        let formatter = self
            .code_style
            .as_ref()
            .and_then(|style| style.formatter.clone());
        let import_order = self
            .code_style
            .as_ref()
            .map(|style| style.import_order.clone())
            .unwrap_or_default();
        settings(
            &self.config,
            &runtimes,
            self.autobuild.enabled(),
            inlay,
            code_vision,
            &combined,
            formatter.as_deref(),
            &import_order,
        )
    }

    /// What the server shows in the text. The caller sends the new settings to a
    /// running server, which applies them without a restart. `false` when nothing
    /// changed, so every opened file does not resend the settings.
    pub fn set_inlay_hints(&self, parameters: InlayParameterNames, types: bool) -> bool {
        let mut inlay = self
            .inlay
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let wanted = InlayHints { parameters, types };
        let changed = *inlay != wanted;
        *inlay = wanted;
        changed
    }

    /// The labels above declarations. The caller sends the new settings to a running
    /// server, which applies them without a restart. `false` when nothing changed.
    /// The compiler problems of the checks the user changed, as `.prefs` lines. A new
    /// choice goes to a file of its own, which the running server reads at once when
    /// the caller sends the new settings; `true` means that the path changed.
    pub fn set_inspection_prefs(&self, prefs: Vec<(String, String)>) -> bool {
        let mut current = self
            .inspection_prefs
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if *current == prefs {
            return false;
        }
        *current = prefs;
        let mut combined = self
            .code_style
            .as_ref()
            .map(|style| style.prefs.clone())
            .unwrap_or_default();
        combined.extend(current.iter().cloned());
        let file = inspection_prefs_path(&self.config.state_dir, &combined);
        let text = code_style::prefs_text(&combined);
        if let Err(error) = fs::write(&file, text) {
            eprintln!("the inspections of the project are not saved: {error}");
        }
        // The earlier choice names a file the settings no longer point at.
        for entry in fs::read_dir(&self.config.state_dir)
            .into_iter()
            .flatten()
            .flatten()
        {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with(INSPECTION_PREFS_PREFIX)
                && name.ends_with(INSPECTION_PREFS_SUFFIX)
                && entry.path() != file
            {
                let _ = fs::remove_file(entry.path());
            }
        }
        true
    }

    pub fn set_code_vision(&self, usages: bool, implementations: bool) -> bool {
        let mut code_vision = self
            .code_vision
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let wanted = CodeVision {
            usages,
            implementations,
        };
        let changed = *code_vision != wanted;
        *code_vision = wanted;
        changed
    }

    /// Turns compilation on save off while a debug session runs.
    pub fn autobuild(&self) -> Autobuild {
        self.autobuild.clone()
    }

    /// The log of the server messages, which Show Log opens. The stderr of the JVM
    /// stays in its own file and reaches this one through [`Self::append_jvm_output`].
    pub fn log_file(&self) -> PathBuf {
        self.config.state_dir.join(log::MESSAGES_LOG)
    }

    fn stderr_file(&self) -> PathBuf {
        self.config.state_dir.join(LOG_FILE)
    }

    /// Appends the lines of the JVM output that are not the warnings of a new JDK to
    /// the log of messages, under a heading, and waits until the log is written. Each
    /// line moves once; the stderr file stays as the sidecar writes it.
    pub fn append_jvm_output(&self) {
        let lines = self.unseen_jvm_output();
        let log = self
            .messages
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone();
        if let Some(log) = log {
            if let Some(lines) = lines {
                log.section(JVM_OUTPUT_HEADING, &lines);
            }
            log.flush();
        }
    }

    /// The JVM output since the previous call, without the warnings of the JVM.
    fn unseen_jvm_output(&self) -> Option<String> {
        let mut seen = self
            .jvm_output
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let path = self.stderr_file();
        let metadata = fs::metadata(&path).ok()?;
        // The sidecar truncates the file at every start; a shorter file is a new one.
        if metadata.len() < *seen {
            *seen = 0;
        }
        let mut file = fs::File::open(&path).ok()?;
        file.seek(SeekFrom::Start(*seen)).ok()?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).ok()?;
        // A line without its newline may still be written by the sidecar, so it waits.
        let end = bytes.iter().rposition(|byte| *byte == b'\n')? + 1;
        *seen += end as u64;
        let text = String::from_utf8_lossy(&bytes[..end]);
        let lines: String = text
            .lines()
            .filter(|line| !line.starts_with("WARNING:"))
            .map(|line| format!("{line}\n"))
            .collect();
        (!lines.is_empty()).then_some(lines)
    }

    /// Waits until every message of the server that has arrived is in the log, for a
    /// caller that opens the file right away.
    pub fn flush_log(&self) {
        let log = self
            .messages
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone();
        if let Some(log) = log {
            log.flush();
        }
    }

    /// One message of the server, as Show Log shows it. A multi-line stack trace stays
    /// whole. Works on the reader of the protocol, so it only queues the line.
    fn log_message(&self, method: &str, params: &Value) {
        if !matches!(method, "window/logMessage" | "window/showMessage") {
            return;
        }
        let Some(text) = params.get("message").and_then(Value::as_str) else {
            return;
        };
        let level = match params.get("type").and_then(Value::as_i64) {
            Some(1) => "Error",
            Some(2) => "Warning",
            Some(3) => "Info",
            _ => "Log",
        };
        let log = self
            .messages
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone();
        if let Some(log) = log {
            log.line(level, text);
        }
    }

    /// The latest failure of the build import, and every later one.
    pub fn watch_import_failure(&self) -> watch::Receiver<Option<ImportFailure>> {
        self.imports.watch_failure()
    }

    /// Whether a Gradle import runs now, so that the status bar can say `importing`
    /// while a retry is in flight.
    pub fn watch_importing(&self) -> watch::Receiver<bool> {
        self.imports.watch_running()
    }

    /// How many Gradle imports have begun in this server session. A repeat import
    /// raises it, so a reader tells a refusal of the first attempt from one of the
    /// repeat.
    pub fn watch_import_starts(&self) -> watch::Receiver<u64> {
        self.imports.watch_starts()
    }

    fn section(&self, section: &str) -> Value {
        let settings = self.settings();
        section
            .split('.')
            .filter(|part| !part.is_empty())
            .try_fold(&settings, |value, part| value.get(part))
            .cloned()
            .unwrap_or(Value::Null)
    }
}

fn equinox_launcher(jdtls_home: &Path) -> io::Result<PathBuf> {
    let plugins = jdtls_home.join("plugins");
    fs::read_dir(&plugins)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            name.starts_with("org.eclipse.equinox.launcher_") && name.ends_with(".jar")
        })
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no Equinox launcher in plugins"))
}

fn slashes(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Copies the agent under the state folder and returns the copy. A version already
/// copied stays as it is, so a repeated model does not touch the file of a server that
/// may still be reading it.
fn copy_agent(state_dir: &Path, lombok: &Lombok) -> io::Result<PathBuf> {
    let directory = state_dir.join(LOMBOK_DIR);
    fs::create_dir_all(&directory)?;
    let name: String = lombok
        .version
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect();
    let target = directory.join(format!("lombok-{name}.jar"));
    if target.is_file() {
        return Ok(target);
    }
    // A partial copy must never become the agent of a start.
    let temporary = directory.join(format!(".lombok-{}-{name}.tmp", std::process::id()));
    fs::copy(&lombok.jar, &temporary)?;
    if let Err(error) = fs::rename(&temporary, &target) {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    Ok(target)
}

/// The settings a start sends with the inlay hints of a fresh start. A check of the
/// values needs no running server.
pub fn settings_of(config: &JdtlsConfig) -> Value {
    let inlay = InlayHints {
        parameters: InlayParameterNames::default(),
        types: false,
    };
    let style = code_style::read_project_style(&config.project_root)
        .map(|style| code_style::prepare(&config.state_dir, &style));
    let prefs = style
        .as_ref()
        .map(|style| style.prefs.clone())
        .unwrap_or_default();
    let formatter = style.as_ref().and_then(|style| style.formatter.clone());
    let import_order = style
        .as_ref()
        .map(|style| style.import_order.clone())
        .unwrap_or_default();
    settings(
        config,
        &config.runtimes,
        true,
        inlay,
        CodeVision::default(),
        &prefs,
        formatter.as_deref(),
        &import_order,
    )
}

#[allow(clippy::too_many_arguments)]
fn settings(
    config: &JdtlsConfig,
    runtimes: &[Runtime],
    autobuild: bool,
    inlay: InlayHints,
    code_vision: CodeVision,
    inspection_prefs: &[(String, String)],
    formatter: Option<&Path>,
    import_order: &[String],
) -> Value {
    let runtimes: Vec<Value> = runtimes
        .iter()
        .map(|runtime| {
            let mut entry = json!({"name": runtime.name, "path": slashes(&runtime.home)});
            if runtime.default {
                entry["default"] = json!(true);
            }
            entry
        })
        .collect();
    let mut arguments = vec!["--no-scan".to_owned()];
    if is_gradle_project(config) && !gradle_idle_timeout_set(config) {
        arguments.push(format!(
            "-D{GRADLE_IDLE_TIMEOUT_PROPERTY}={}",
            gradle_idle_timeout_ms(config)
        ));
    }
    let mut gradle = json!({
        "enabled": true,
        // dgs-framework and others ask for a build scan otherwise.
        "arguments": arguments.join(" "),
        "offline": {"enabled": config.offline},
        "wrapper": {"enabled": config.gradle_home.is_none()},
    });
    if let Some(home) = &config.gradle_home {
        gradle["home"] = json!(slashes(home));
    }
    if let Some(home) = &config.gradle_user_home {
        gradle["user"] = json!({"home": slashes(home)});
    }
    if let Some(home) = &config.gradle_java_home {
        gradle["java"] = json!({"home": slashes(home)});
    }
    let mut maven_configuration = json!({});
    if let Some(path) = &config.maven_settings {
        maven_configuration["userSettings"] = json!(slashes(path));
    }
    let mut format = json!({"enabled": true});
    if let Some(path) = formatter {
        format["settings"] = json!({
            "url": formatter_url(path),
            "profile": code_style::PROFILE_NAME,
        });
    }
    let mut completion = json!({
        "enabled": true,
        "maxResults": 100,
        // `args.for`, `name.var` and the other postfix keys of the server.
        "postfix": {"enabled": true},
        // No argument names or guesses: `name()` with the caret inside, as in IDEA.
        "guessMethodArguments": "off",
        // The default list of vscode-java: service types do not crowd the popup.
        "filteredTypes": [
            "java.awt.*",
            "com.sun.*",
            "sun.*",
            "jdk.*",
            "org.graalvm.*",
            "io.micrometer.shaded.*",
        ],
    });
    if !import_order.is_empty() {
        completion["importOrder"] = json!(import_order);
    }
    let settings = json!({
        "java": {
            "autobuild": {"enabled": autobuild},
            "completion": completion,
            "signatureHelp": {"enabled": true},
            "semanticHighlighting": {"enabled": true},
            "format": format,
            "referencesCodeLens": {"enabled": code_vision.usages},
            // jdtls 1.63 names the setting in the singular and takes a scope rather
            // than a switch: `all`, `types`, `methods` or `none`.
            "implementationCodeLens": if code_vision.implementations {
                "all"
            } else {
                "none"
            },
            "inlayHints": {
                "parameterNames": {
                    "enabled": inlay.parameters.as_setting(),
                    // The server names the parameter of a call and of a constructor.
                    "exclusions": [],
                },
                "variableTypes": {"enabled": inlay.types},
                "parameterTypes": {"enabled": inlay.types},
            },
            "maven": {"downloadSources": false},
            "settings": {"url": inspection_prefs_url(config, inspection_prefs)},
            "import": {
                "maven": {"enabled": true, "offline": {"enabled": config.offline}},
                "gradle": gradle,
            },
            "configuration": {
                "updateBuildConfiguration": "automatic",
                "maven": maven_configuration,
                "runtimes": runtimes,
            },
        }
    });
    settings
}

/// The file the server reads the compiler choices from. The name is a hash of the
/// lines, so a new choice is a new path and the server reloads it without a restart.
fn inspection_prefs_path(state_dir: &Path, prefs: &[(String, String)]) -> PathBuf {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for (key, value) in prefs {
        for byte in key.bytes().chain(*b"=").chain(value.bytes()).chain(*b"\n") {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    state_dir.join(format!(
        "{INSPECTION_PREFS_PREFIX}{hash:016x}{INSPECTION_PREFS_SUFFIX}"
    ))
}

fn inspection_prefs_url(config: &JdtlsConfig, prefs: &[(String, String)]) -> String {
    let path = inspection_prefs_path(&config.state_dir, prefs);
    CanonPath::new(&path).map_or_else(
        |_| format!("file:///{}", slashes(&path).trim_start_matches('/')),
        |path| path.to_uri(),
    )
}

fn formatter_url(formatter: &Path) -> String {
    CanonPath::new(formatter).map_or_else(
        |_| format!("file:///{}", slashes(formatter).trim_start_matches('/')),
        |path| path.to_uri(),
    )
}

fn is_gradle_project(config: &JdtlsConfig) -> bool {
    [
        "build.gradle",
        "build.gradle.kts",
        "settings.gradle",
        "settings.gradle.kts",
    ]
    .iter()
    .any(|name| config.project_root.join(name).is_file())
}

/// The project and the Gradle user home may already choose the daemon idle timeout.
/// Those files stay as they are, and the import then inherits their value.
fn gradle_idle_timeout_set(config: &JdtlsConfig) -> bool {
    let project = config.project_root.join("gradle.properties");
    let user = config
        .gradle_user_home
        .as_ref()
        .map(|home| home.join("gradle.properties"));
    [Some(project), user]
        .into_iter()
        .flatten()
        .any(|path| fs::read_to_string(path).is_ok_and(|text| has_gradle_idle_timeout(&text)))
}

fn has_gradle_idle_timeout(text: &str) -> bool {
    text.lines().any(|line| {
        let line = line.split(['#', '!']).next().unwrap_or_default().trim();
        line.split_once(['=', ':'])
            .is_some_and(|(key, _)| key.trim() == GRADLE_IDLE_TIMEOUT_PROPERTY)
    })
}

/// The timeout of the daemon. The environment of the server can shorten it for a test;
/// the release uses three minutes.
fn gradle_idle_timeout_ms(config: &JdtlsConfig) -> u64 {
    if let Some(value) = config.environment.get(GRADLE_IDLE_TIMEOUT_ENV)
        && let Ok(value) = value.parse()
    {
        return value;
    }
    if let Ok(value) = std::env::var(GRADLE_IDLE_TIMEOUT_ENV)
        && let Ok(value) = value.parse()
    {
        return value;
    }
    GRADLE_IDLE_TIMEOUT_MS
}

fn extended_capabilities() -> Value {
    json!({
        // Without this Ctrl+B into a JDK or dependency type opens nothing.
        "classFileContentsSupport": true,
        // The prompts and the advanced refactorings need dialogs of the client. Without
        // them jdtls answers extract, inline and generate as code actions with edits.
        // Move is the exception: F6 asks the move requests directly and needs this on,
        // otherwise jdtls returns a move with no edits.
        "moveRefactoringSupport": true,
        // Extract, assign and invert come as commands then; the editor asks
        // `java/getRefactorEdit` for their edits, or for the values of a small form.
        "advancedExtractRefactoringSupport": true,
        // Extract Interface appears only with this and the flag above. The editor shows
        // its form and asks `java/checkExtractInterfaceStatus` for the members.
        "extractInterfaceSupport": true,
        // The command proposal carries no edits; the editor asks `java/getRefactorEdit`
        // for them, which is how the hint appears without a dialog.
        "advancedIntroduceParameterRefactoringSupport": true,
        "overrideMethodsPromptSupport": false,
        "hashCodeEqualsPromptSupport": false,
        "generateToStringPromptSupport": false,
        "advancedGenerateAccessorsSupport": false,
        "generateConstructorsPromptSupport": false,
        "generateDelegateMethodsPromptSupport": false,
        "resolveAdditionalTextEditsSupport": true,
    })
}

fn capabilities() -> ClientCapabilities {
    let capabilities = json!({
        "workspace": {
            "applyEdit": true,
            "workspaceEdit": {
                "documentChanges": true,
                "resourceOperations": ["create", "rename", "delete"],
            },
            "configuration": true,
            "didChangeConfiguration": {"dynamicRegistration": true},
            "didChangeWatchedFiles": {"dynamicRegistration": true},
            "symbol": {"dynamicRegistration": true},
            "executeCommand": {"dynamicRegistration": true},
            "workspaceFolders": true,
            // The server may ask for the labels again after a project-wide change.
            "codeLens": {"refreshSupport": true},
        },
        "textDocument": {
            "synchronization": {"didSave": true},
            "completion": {
                "completionItem": {
                    // Methods come back with `()` and a tab stop inside the brackets.
                    "snippetSupport": true,
                    "documentationFormat": ["markdown", "plaintext"],
                    "resolveSupport": {
                        "properties": ["documentation", "detail", "additionalTextEdits"],
                    },
                },
            },
            "hover": {"contentFormat": ["markdown", "plaintext"]},
            "signatureHelp": {
                "signatureInformation": {"documentationFormat": ["markdown", "plaintext"]},
            },
            "definition": {"linkSupport": false},
            "typeDefinition": {"linkSupport": false},
            "implementation": {"linkSupport": false},
            "references": {},
            "documentSymbol": {"hierarchicalDocumentSymbolSupport": true},
            "callHierarchy": {"dynamicRegistration": false},
            "typeHierarchy": {"dynamicRegistration": false},
            "formatting": {},
            "documentHighlight": {},
            // jdtls reports what a name is: a field, a parameter, a static member and
            // so on. The editor draws these over the syntax colours.
            "semanticTokens": {
                "dynamicRegistration": false,
                "requests": {"range": true, "full": true},
                "tokenTypes": [
                    "namespace", "type", "class", "enum", "interface", "struct",
                    "typeParameter", "parameter", "variable", "property", "enumMember",
                    "event", "function", "method", "macro", "keyword", "modifier",
                    "comment", "string", "number", "regexp", "operator", "decorator",
                ],
                "tokenModifiers": [
                    "declaration", "definition", "readonly", "static", "deprecated",
                    "abstract", "async", "modification", "documentation",
                    "defaultLibrary",
                ],
                "formats": ["relative"],
                "overlappingTokenSupport": false,
                "multilineTokenSupport": false,
                "augmentsSyntaxTokens": true,
            },
            "inlayHint": {"dynamicRegistration": false},
            "codeLens": {"dynamicRegistration": false},
            "rename": {"prepareSupport": true},
            "publishDiagnostics": {"relatedInformation": true},
            "codeAction": {
                "codeActionLiteralSupport": {
                    "codeActionKind": {
                        "valueSet": [
                            "", "quickfix", "refactor", "refactor.extract", "refactor.inline",
                            "refactor.rewrite", "source", "source.organizeImports",
                        ],
                    },
                },
                // The server may send an action without its edit and fill it in on
                // `codeAction/resolve`, when the row is shown.
                "resolveSupport": {"properties": ["edit"]},
                "dataSupport": true,
            },
        },
        "window": {"workDoneProgress": true},
    });
    serde_json::from_value(capabilities).unwrap_or_default()
}

/// The heap of the first start for a machine with `total` bytes: one thirty-second of
/// the physical memory, between one and four gigabytes. A medium project imports in one
/// gigabyte, while a larger machine may keep more without taking it from other programs.
fn first_heap_mb(total: u64) -> u32 {
    let share = (total / (1024 * 1024 * 32)) as u32;
    share.clamp(FIRST_HEAP_MB, LARGEST_FIRST_HEAP_MB)
}

/// The JVM arguments that make the collector give unused memory back. Separate from the
/// other settings, which another change of the start may extend.
fn memory_arguments() -> Vec<String> {
    MEMORY_SETTINGS
        .iter()
        .map(|arg| (*arg).to_owned())
        .collect()
}

/// The next step of the ladder above `heap`, or `heap` on its last step.
fn next_heap(heap: u32) -> u32 {
    HEAP_LADDER_MB
        .iter()
        .copied()
        .find(|step| *step > heap)
        .unwrap_or(heap)
}

impl ServerSpec for Jdtls {
    fn preflight(&self) -> Result<(), String> {
        if let Some(problem) = startup::version_problem(&self.config.java_home) {
            return Err(problem);
        }
        // The cache of another release would fail the import of the build tool, so no
        // server starts on it. The journal keeps the reason too.
        install::prepare(&self.config.state_dir, &self.installation).map_err(|error| {
            let message =
                format!("the OSGi configuration of an older jdtls is not removed: {error}");
            eprintln!("{message}");
            message
        })
    }

    fn starting(&self) {
        let now = SystemTime::now();
        *self
            .started
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = Some(now);
        *self
            .jvm_output
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = 0;
        let heading = format!(
            "===== jdtls {JDTLS_VERSION} started {} with Java {} =====",
            log::timestamp(now),
            self.config.java_home.display()
        );
        let opened = match log::ServerLog::open(&self.log_file(), &heading) {
            Ok(log) => Some(log),
            Err(error) => {
                eprintln!("the messages of the server are not saved: {error}");
                None
            }
        };
        let previous = {
            let mut messages = self
                .messages
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            std::mem::replace(&mut *messages, opened)
        };
        // The writer of the previous start drains before the new one writes.
        drop(previous);
    }

    fn crash_reason(&self) -> Option<String> {
        let since = *self
            .started
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        startup::crash_reason(&self.config.state_dir.join("config"), since)
    }

    fn plan(&self) -> Plan {
        let config = &self.config;
        let mut args: Vec<String> = LAUNCH_SETTINGS
            .iter()
            .map(|arg| (*arg).to_owned())
            .collect();
        args.push(format!(
            "-Dosgi.sharedConfiguration.area={}",
            config.jdtls_home.join(CONFIG_DIRECTORY).display()
        ));
        args.extend(JVM_SETTINGS.iter().map(|arg| (*arg).to_owned()));
        // Gradle looks at the daemons of the registry before it starts one. A stale
        // entry that names a removed JDK fails the connection whatever `javaHome` the
        // import asked for, so the imports of the IDE get a registry of their own.
        args.push(format!(
            "-D{GRADLE_REGISTRY_PROPERTY}={}",
            gradle_registry_dir(config).display()
        ));
        args.extend(memory_arguments());
        args.push(format!("-Xmx{}m", self.heap_mb.load(Ordering::Relaxed)));
        // The agent is a JVM setting like the rest, so it goes before `-jar`.
        if let Some(lombok) = self
            .lombok
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .as_ref()
        {
            args.push(format!("-javaagent:{}", lombok.jar.display()));
        }
        // A JVM of 24 and newer warns about `sun.misc.Unsafe` for every library that
        // still uses it, Guice inside the bundles among them. An older JVM refuses the
        // flag it does not know, so only a version that knows it gets it.
        if startup::java_version(&config.java_home)
            .is_some_and(|version| version >= UNSAFE_MEMORY_FLAG_JAVA)
        {
            args.push("--sun-misc-unsafe-memory-access=allow".to_owned());
        }
        args.extend([
            "-jar".to_owned(),
            self.launcher_jar.display().to_string(),
            "-configuration".to_owned(),
            config.state_dir.join("config").display().to_string(),
            "-data".to_owned(),
            config.state_dir.join("workspace").display().to_string(),
        ]);
        Plan {
            program: config.java_home.join("bin").join(executable("java")),
            args,
            env: config.environment.clone(),
            directory: Some(config.state_dir.clone()),
            log: Some(self.stderr_file()),
            label: Some("Language server"),
        }
    }

    fn crashed(&self, exit_code: Option<i32>) {
        // A new server imports the project again, so an old failure says nothing.
        self.imports.reset();
        // A heap the user set is the ceiling of the ladder as well as its only step.
        if exit_code == Some(EXIT_OUT_OF_MEMORY) && self.max_heap_mb.load(Ordering::Relaxed) == 0 {
            let _ = self
                .heap_mb
                .try_update(Ordering::Relaxed, Ordering::Relaxed, |heap| {
                    Some(next_heap(heap))
                });
        }
    }

    fn initialize_params(&self) -> InitializeParams {
        let root = file_uri(&self.root);
        let name = self
            .config
            .project_root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let folders = root.as_ref().map(|uri| {
            vec![WorkspaceFolder {
                uri: uri.clone(),
                name,
            }]
        });
        let folder_uris: Vec<&str> = root.iter().map(|uri| uri.as_str()).collect();
        InitializeParams {
            process_id: Some(std::process::id()),
            client_info: Some(ClientInfo {
                name: "Arro".to_owned(),
                version: Some(env!("CARGO_PKG_VERSION").to_owned()),
            }),
            workspace_folders: folders,
            capabilities: capabilities(),
            trace: Some(TraceValue::Off),
            initialization_options: Some(json!({
                "bundles": [self.config.debug_plugin.display().to_string()],
                "workspaceFolders": folder_uris,
                "settings": self.settings(),
                "extendedClientCapabilities": extended_capabilities(),
            })),
            ..Default::default()
        }
    }

    fn initialized(&self, client: &Client) {
        // The connection may have ended already, the sidecar sees that itself.
        let _ = client.raw_notify(
            <DidChangeConfiguration as Notification>::METHOD,
            json!({"settings": self.settings()}),
        );
    }

    fn initialize_result(&self, result: &InitializeResult) {
        self.semantic_tokens
            .set(SemanticTokensCapability::from_server(&result.capabilities));
        self.code_lens
            .set(CodeLensCapability::from_server(&result.capabilities));
    }

    fn state_of(&self, method: &str, params: &Value) -> Option<State> {
        self.log_message(method, params);
        self.imports.observe(method, params);
        if method != "language/status" {
            return None;
        }
        match params.get("type").and_then(Value::as_str)? {
            "ServiceReady" => Some(State::Ready),
            // Says that some project has a problem, its reason comes with the
            // server log and the diagnostics of the build script. It is not a
            // lifecycle step.
            "ProjectStatus" => None,
            _ => Some(State::Importing),
        }
    }

    fn request(&self, method: &str, params: Value) -> Result<Value, ResponseError> {
        match method {
            "workspace/configuration" => {
                let items = params.get("items").and_then(Value::as_array);
                let values = items.into_iter().flatten().map(|item| {
                    self.section(
                        item.get("section")
                            .and_then(Value::as_str)
                            .unwrap_or_default(),
                    )
                });
                Ok(values.collect())
            }
            // Edits go through the workspace transaction, not behind its back.
            "workspace/applyEdit" => Ok(json!({"applied": false})),
            _ => Ok(Value::Null),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jdtls() -> (Jdtls, PathBuf) {
        jdtls_with_gradle_java_home(None)
    }

    fn jdtls_with_gradle_java_home(gradle_java_home: Option<PathBuf>) -> (Jdtls, PathBuf) {
        jdtls_with(gradle_java_home, None)
    }

    fn jdtls_with(gradle_java_home: Option<PathBuf>, max_heap_mb: Option<u32>) -> (Jdtls, PathBuf) {
        let root = test_support::scratch("jdtls-adapter");
        let plugins = root.join("home").join("plugins");
        fs::create_dir_all(&plugins).unwrap();
        fs::write(plugins.join("org.eclipse.equinox.launcher_1.8.0.jar"), b"").unwrap();
        fs::write(plugins.join("org.eclipse.osgi_3.25.0.jar"), b"").unwrap();
        let config = JdtlsConfig {
            jdtls_home: root.join("home"),
            debug_plugin: root.join("debug.jar"),
            java_home: root.join("jdk"),
            project_root: PathBuf::from(r"C:\work\Проект"),
            state_dir: root.join("state"),
            runtimes: vec![Runtime {
                name: "JavaSE-17".to_owned(),
                home: PathBuf::from(r"C:\jdk\17"),
                default: true,
            }],
            maven_settings: None,
            gradle_home: None,
            gradle_user_home: Some(PathBuf::from(r"E:\gradle")),
            gradle_java_home,
            offline: true,
            environment: HashMap::new(),
            max_heap_mb,
        };
        (Jdtls::new(config).unwrap(), root)
    }

    fn jdtls_on(project_root: PathBuf) -> (Jdtls, PathBuf) {
        let root = test_support::scratch("jdtls-adapter-proj");
        let plugins = root.join("home").join("plugins");
        fs::create_dir_all(&plugins).unwrap();
        fs::write(plugins.join("org.eclipse.equinox.launcher_1.8.0.jar"), b"").unwrap();
        fs::create_dir_all(&project_root).unwrap();
        let config = JdtlsConfig {
            jdtls_home: root.join("home"),
            debug_plugin: root.join("debug.jar"),
            java_home: root.join("jdk"),
            project_root,
            state_dir: root.join("state"),
            runtimes: Vec::new(),
            maven_settings: None,
            gradle_home: None,
            gradle_user_home: None,
            gradle_java_home: None,
            offline: true,
            environment: HashMap::new(),
            max_heap_mb: None,
        };
        (Jdtls::new(config).unwrap(), root)
    }

    #[test]
    fn a_set_heap_goes_to_xmx_and_limits_the_ladder() {
        // From the first start on, even above the largest step of the ladder.
        let (capped, _) = jdtls_with(None, Some(5000));
        assert!(capped.plan().args.iter().any(|arg| arg == "-Xmx5000m"));
        capped.crashed(Some(EXIT_OUT_OF_MEMORY));
        assert!(
            capped.plan().args.iter().any(|arg| arg == "-Xmx5000m"),
            "an out-of-memory end does not grow a heap the user set"
        );

        // Setting the heap at run time applies at the next start, and clearing it goes back
        // to the heap of the machine.
        let (jdtls, _) = jdtls();
        jdtls.set_max_heap_mb(Some(3072));
        assert!(jdtls.plan().args.iter().any(|arg| arg == "-Xmx3072m"));
        jdtls.crashed(Some(EXIT_OUT_OF_MEMORY));
        assert!(jdtls.plan().args.iter().any(|arg| arg == "-Xmx3072m"));
        jdtls.set_max_heap_mb(None);
        let automatic = format!("-Xmx{}m", first_heap_mb(platform::physical_memory()));
        assert!(jdtls.plan().args.contains(&automatic));
    }

    #[test]
    fn a_project_code_style_sets_the_formatter_profile_and_the_import_order() {
        let project = test_support::scratch("jdtls-adapter-code-style");
        std::fs::create_dir_all(project.join(".idea").join("codeStyles")).unwrap();
        std::fs::write(
            project.join(".idea/codeStyles/Project.xml"),
            r#"<code_scheme name="Project">
  <codeStyleSettings language="JAVA">
    <option name="INDENT_SIZE" value="2" />
    <option name="TAB_SIZE" value="2" />
    <option name="USE_TAB_CHARACTER" value="false" />
  </codeStyleSettings>
</code_scheme>"#,
        )
        .unwrap();
        let (jdtls, _root) = jdtls_on(project);
        let settings = jdtls.settings();
        let url = settings["java"]["format"]["settings"]["url"]
            .as_str()
            .unwrap();
        assert!(
            url.starts_with("file:///") && url.ends_with(".xml"),
            "{url}"
        );
        assert_eq!(
            settings["java"]["format"]["settings"]["profile"],
            json!(code_style::PROFILE_NAME)
        );
        let prefs = settings["java"]["settings"]["url"].as_str().unwrap();
        assert!(prefs.ends_with(".prefs"), "{prefs}");
    }

    #[test]
    fn the_import_shortens_the_gradle_daemon_idle_timeout_unless_the_project_sets_one() {
        let project = test_support::scratch("jdtls-adapter-gradle-props");
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join("build.gradle"), "plugins { id 'java' }\n").unwrap();
        let (jdtls, _root) = jdtls_on(project.clone());
        let arguments = || {
            jdtls.settings()["java"]["import"]["gradle"]["arguments"]
                .as_str()
                .unwrap()
                .to_owned()
        };

        assert!(
            arguments().contains("-Dorg.gradle.daemon.idletimeout=600000"),
            "{}",
            arguments()
        );

        fs::write(
            project.join("gradle.properties"),
            "org.gradle.daemon.idletimeout=3600000\n",
        )
        .unwrap();
        assert!(
            !arguments().contains("org.gradle.daemon.idletimeout"),
            "{}",
            arguments()
        );

        fs::write(
            project.join("gradle.properties"),
            "# org.gradle.daemon.idletimeout=3600000\n",
        )
        .unwrap();
        assert!(
            arguments().contains("-Dorg.gradle.daemon.idletimeout=600000"),
            "{}",
            arguments()
        );
    }

    #[test]
    fn a_gradle_java_home_of_the_settings_reaches_the_initialization_options() {
        let (jdtls, _root) = jdtls_with_gradle_java_home(Some(PathBuf::from(r"C:\jdk\21")));
        let settings = jdtls.settings();
        assert_eq!(
            settings["java"]["import"]["gradle"]["java"]["home"],
            json!("C:/jdk/21")
        );
        assert_eq!(
            settings["java"]["import"]["gradle"]["wrapper"]["enabled"],
            json!(true)
        );

        let params = jdtls.initialize_params();
        let options = params.initialization_options.unwrap();
        assert_eq!(
            options["settings"]["java"]["import"]["gradle"]["java"]["home"],
            json!("C:/jdk/21")
        );

        let plan = sidecar::ServerSpec::plan(&jdtls);
        let registry = format!(
            "-D{GRADLE_REGISTRY_PROPERTY}={}",
            gradle_registry_dir(&jdtls.config).display()
        );
        assert!(
            plan.args.contains(&registry),
            "the server keeps a Gradle daemon registry of its own: {:?}",
            plan.args
        );
        assert!(
            registry.contains(GRADLE_REGISTRY_DIR),
            "the registry is named for the JVM of Gradle: {registry}"
        );
    }

    #[test]
    fn completion_filtered_types_and_snippets_follow_vscode_java() {
        let (jdtls, _) = jdtls();
        assert_eq!(
            jdtls.settings()["java"]["completion"]["filteredTypes"],
            json!([
                "java.awt.*",
                "com.sun.*",
                "sun.*",
                "jdk.*",
                "org.graalvm.*",
                "io.micrometer.shaded.*"
            ])
        );
        let capabilities = serde_json::to_value(capabilities()).unwrap();
        assert_eq!(
            capabilities["textDocument"]["completion"]["completionItem"]["snippetSupport"],
            Value::Bool(true)
        );
        assert_eq!(
            jdtls.settings()["java"]["completion"]["postfix"]["enabled"],
            Value::Bool(true)
        );
    }

    #[test]
    fn inlay_hints_start_with_literals_and_change_without_a_restart() {
        let (jdtls, _) = jdtls();
        assert_eq!(
            jdtls.settings()["java"]["inlayHints"]["parameterNames"]["enabled"],
            json!("literals")
        );
        assert_eq!(
            jdtls.settings()["java"]["inlayHints"]["variableTypes"]["enabled"],
            Value::Bool(false)
        );

        jdtls.set_inlay_hints(InlayParameterNames::All, true);

        assert_eq!(
            jdtls.settings()["java"]["inlayHints"]["parameterNames"]["enabled"],
            json!("all")
        );
        assert_eq!(
            jdtls.settings()["java"]["inlayHints"]["parameterTypes"]["enabled"],
            Value::Bool(true)
        );
    }

    #[test]
    fn code_vision_labels_start_on_and_change_without_a_restart() {
        let (jdtls, _) = jdtls();
        assert_eq!(
            jdtls.settings()["java"]["referencesCodeLens"]["enabled"],
            Value::Bool(true)
        );
        assert_eq!(
            jdtls.settings()["java"]["implementationCodeLens"],
            json!("all")
        );

        assert!(jdtls.set_code_vision(false, true));
        assert_eq!(
            jdtls.settings()["java"]["referencesCodeLens"]["enabled"],
            Value::Bool(false)
        );
        assert!(
            !jdtls.set_code_vision(false, true),
            "nothing changed, so no settings are resent"
        );
        assert_eq!(
            jdtls.settings()["java"]["implementationCodeLens"],
            json!("all")
        );
        jdtls.set_code_vision(false, false);
        assert_eq!(
            jdtls.settings()["java"]["implementationCodeLens"],
            json!("none")
        );
    }

    #[test]
    fn a_debug_session_turns_autobuild_off_until_the_last_one_ends() {
        let (jdtls, _) = jdtls();
        assert_eq!(
            jdtls.settings()["java"]["autobuild"]["enabled"],
            Value::Bool(true)
        );

        let first = jdtls.autobuild();
        assert!(first.begin());
        assert_eq!(
            jdtls.settings()["java"]["autobuild"]["enabled"],
            Value::Bool(false)
        );

        let second = jdtls.autobuild();
        assert!(
            !second.begin(),
            "the second session does not tell the server"
        );
        assert!(!second.end(), "one session still runs");
        assert_eq!(
            jdtls.settings()["java"]["autobuild"]["enabled"],
            Value::Bool(false)
        );

        assert!(second.end(), "the last session ended");
        assert_eq!(
            jdtls.settings()["java"]["autobuild"]["enabled"],
            Value::Bool(true)
        );
    }

    #[test]
    fn out_of_memory_gives_the_next_start_the_next_step_of_the_ladder() {
        let (jdtls, _) = jdtls();
        for heap in ["1024", "2048", "4096", "6144"] {
            assert!(
                jdtls
                    .plan()
                    .args
                    .iter()
                    .any(|arg| arg == &format!("-Xmx{heap}m")),
                "the first start uses -Xmx{heap}m"
            );
            jdtls.crashed(Some(EXIT_OUT_OF_MEMORY));
        }
        // The last step stays, a start that runs out of memory there does not grow.
        assert!(jdtls.plan().args.iter().any(|arg| arg == "-Xmx6144m"));
        jdtls.crashed(Some(1));
        assert!(jdtls.plan().args.iter().any(|arg| arg == "-Xmx6144m"));
    }

    #[test]
    fn the_first_heap_follows_the_size_of_the_machine() {
        let gb = 1024 * 1024 * 1024;
        assert_eq!(first_heap_mb(8 * gb), 1024);
        assert_eq!(first_heap_mb(32 * gb), 1024);
        assert_eq!(first_heap_mb(64 * gb), 2048);
        assert_eq!(first_heap_mb(256 * gb), 4096);
        assert_eq!(
            first_heap_mb(0),
            1024,
            "an unknown machine keeps the minimum"
        );
    }

    #[test]
    fn a_second_server_for_the_same_state_does_not_start() {
        let (first, root) = jdtls();
        let config = first.config.clone();
        let error = Jdtls::new(config).err().unwrap();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        drop(first);
        let _ = root;
    }

    #[cfg(windows)]
    #[test]
    fn plan_runs_java_with_the_one_setting_and_the_launcher_jar() {
        let (jdtls, root) = jdtls();
        let plan = jdtls.plan();
        assert_eq!(plan.program, root.join("jdk").join("bin").join("java.exe"));
        for setting in JVM_SETTINGS {
            assert!(plan.args.iter().any(|arg| arg == setting), "{setting}");
        }
        let jar = plan.args.iter().position(|arg| arg == "-jar").unwrap();
        assert!(plan.args[jar + 1].contains("org.eclipse.equinox.launcher_"));
        assert!(plan.args.iter().any(|arg| arg == "-data"));
        let vm_end = jar;
        assert!(plan.args[..vm_end].iter().all(|arg| !arg.contains("-data")));
        assert_eq!(plan.log, Some(root.join("state").join(LOG_FILE)));
    }

    #[test]
    fn a_missing_release_is_an_error() {
        let (_, root) = jdtls();
        let mut config_home = root.join("nowhere");
        config_home.push("x");
        assert!(equinox_launcher(&config_home).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn initialize_carries_bundles_runtimes_and_extended_capabilities() {
        let (jdtls, _) = jdtls();
        let params = jdtls.initialize_params();
        let options = params.initialization_options.unwrap();
        assert_eq!(
            options["extendedClientCapabilities"]["classFileContentsSupport"],
            true
        );
        assert_eq!(
            options["extendedClientCapabilities"]["moveRefactoringSupport"],
            true
        );
        assert!(
            options["bundles"][0]
                .as_str()
                .unwrap()
                .ends_with("debug.jar")
        );
        let runtime = &options["settings"]["java"]["configuration"]["runtimes"][0];
        assert_eq!(runtime["path"], "C:/jdk/17");
        assert_eq!(runtime["default"], true);
        let folders = params.workspace_folders.unwrap();
        assert_eq!(
            folders[0].uri.as_str(),
            "file:///C:/work/%D0%9F%D1%80%D0%BE%D0%B5%D0%BA%D1%82"
        );
        assert_eq!(folders[0].name, "Проект");
        assert_eq!(params.client_info.unwrap().name, "Arro");
        assert!(
            params
                .capabilities
                .window
                .unwrap()
                .work_done_progress
                .unwrap()
        );
    }

    #[test]
    fn service_ready_is_ready_and_other_status_is_importing() {
        let (jdtls, _) = jdtls();
        let ready = json!({"type": "ServiceReady", "message": "ServiceReady"});
        assert_eq!(
            jdtls.state_of("language/status", &ready),
            Some(State::Ready)
        );
        let started = json!({"type": "Started"});
        assert_eq!(
            jdtls.state_of("language/status", &started),
            Some(State::Importing)
        );
        assert_eq!(
            jdtls.state_of("textDocument/publishDiagnostics", &ready),
            None
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_project_status_is_not_a_lifecycle_step_and_a_build_error_is_kept() {
        let (jdtls, _) = jdtls();
        let warning = json!({"type": "ProjectStatus", "message": "WARNING"});
        assert_eq!(jdtls.state_of("language/status", &warning), None);

        let diagnostics = json!({
            "uri": "file:///c:/work/build.gradle.kts",
            "diagnostics": [{
                "range": {
                    "start": {"line": 24, "character": 0},
                    "end": {"line": 24, "character": 0},
                },
                "severity": 1,
                "message": "Could not configure the build.\nTask with name 'postRelease' not found.",
            }],
        });
        assert_eq!(
            jdtls.state_of("textDocument/publishDiagnostics", &diagnostics),
            None
        );
        let failure = jdtls.watch_import_failure().borrow().clone().unwrap();
        assert_eq!(failure.line, 24);
        assert_eq!(failure.summary, "Task with name 'postRelease' not found.");
    }

    #[test]
    fn server_requests_read_the_settings() {
        let (jdtls, _) = jdtls();
        let items = json!({"items": [
            {"section": "java.import.gradle.arguments"},
            {"section": "java.no.such.key"},
        ]});
        let reply = jdtls.request("workspace/configuration", items).unwrap();
        assert_eq!(reply, json!(["--no-scan", null]));
        let home = jdtls.section("java.import.gradle.user.home");
        assert_eq!(home, "E:/gradle");
    }

    #[test]
    fn the_checks_of_the_user_reach_the_running_server_through_a_new_file() {
        use lsp_client::uri::file_path;

        let (jdtls, _) = jdtls();
        let url = || {
            jdtls.settings()["java"]["settings"]["url"]
                .as_str()
                .unwrap()
                .to_owned()
        };
        let empty = url();
        assert!(
            empty.starts_with("file:///") && empty.ends_with(".prefs"),
            "the server always reads a file: {empty}"
        );
        let empty_path = file_path(&empty.parse().unwrap()).unwrap();
        assert_eq!(fs::read_to_string(&empty_path).unwrap(), "");

        assert!(jdtls.set_inspection_prefs(vec![(
            "org.eclipse.jdt.core.compiler.problem.unusedImport".to_owned(),
            "ignore".to_owned(),
        )]));
        let changed = url();
        assert_ne!(changed, empty, "a new choice is a new path for the server");
        let changed_path = file_path(&changed.parse().unwrap()).unwrap();
        assert_eq!(
            fs::read_to_string(&changed_path).unwrap(),
            "org.eclipse.jdt.core.compiler.problem.unusedImport=ignore\n"
        );
        assert!(!empty_path.exists(), "the file of the old choice is gone");

        assert!(
            !jdtls.set_inspection_prefs(vec![(
                "org.eclipse.jdt.core.compiler.problem.unusedImport".to_owned(),
                "ignore".to_owned(),
            )]),
            "the same choices are not sent again"
        );
        assert_eq!(url(), changed);
    }

    #[test]
    fn the_settings_of_a_check_are_sent_when_it_is_turned_off_and_after_a_reset() {
        let (jdtls, _) = jdtls();
        let url = || {
            jdtls.settings()["java"]["settings"]["url"]
                .as_str()
                .unwrap()
                .to_owned()
        };
        let empty = url();
        jdtls.set_inspection_prefs(vec![("a".to_owned(), "error".to_owned())]);
        let set = url();
        jdtls.set_inspection_prefs(Vec::new());
        let reset = url();
        assert_ne!(set, empty);
        assert_eq!(
            reset, empty,
            "the default choices name the same empty file again"
        );
    }

    #[test]
    fn registrations_are_acknowledged_and_edits_are_refused() {
        let (jdtls, _) = jdtls();
        assert_eq!(
            jdtls
                .request("client/registerCapability", json!({}))
                .unwrap(),
            Value::Null
        );
        assert_eq!(
            jdtls.request("workspace/applyEdit", json!({})).unwrap(),
            json!({"applied": false})
        );
    }
}
