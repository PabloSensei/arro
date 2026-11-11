//! How to run jdtls: the pinned pair, JVM settings, runtimes and client settings.
//!
//! Facts behind each value are in `spikes/s9-jdtls/S9.md`. Downloading the pair
//! with consent and hash checks comes later, for now the paths are given.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use lsp_client::lsp_types::notification::{DidChangeConfiguration, Notification};
use lsp_client::lsp_types::{
    ClientCapabilities, ClientInfo, InitializeParams, InitializeResult, TraceValue, WorkspaceFolder,
};
use lsp_client::uri::file_uri;
use lsp_client::{Client, ResponseError, SemanticTokensCapability, SemanticTokensSetup};
use platform::CanonPath;
use serde_json::{Value, json};
use sidecar::{Plan, ServerSpec, State};
use tokio::sync::watch;

mod import;
mod startup;

pub use import::{ImportFailure, is_build_file};

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

const LOG_FILE: &str = "jdtls-stderr.log";

/// The Gradle daemon of an import stops after ten idle minutes instead of the three
/// hours it keeps by default. The property travels as an import argument, as S9 did.
const GRADLE_IDLE_TIMEOUT_MS: u64 = 600_000;
const GRADLE_IDLE_TIMEOUT_PROPERTY: &str = "org.gradle.daemon.idletimeout";
/// A test asks for a shorter timeout through the environment of the server.
const GRADLE_IDLE_TIMEOUT_ENV: &str = "IDE_GRADLE_IDLE_TIMEOUT_MS";

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
    pub gradle_user_home: Option<PathBuf>,
    /// The JDK of the project, so that the daemon matches the command line
    /// and the project level is not taken from the JDK that runs jdtls.
    pub gradle_java_home: Option<PathBuf>,
    /// No network for dependency resolution during import.
    pub offline: bool,
    pub environment: HashMap<String, String>,
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
    autobuild: Autobuild,
    heap_mb: AtomicU32,
    /// `java.configuration.runtimes` as the server reads it at every start. Project
    /// Structure changes it without starting a new process of the window.
    runtimes: Mutex<Vec<Runtime>>,
    /// The Inlay Hints settings of the window, read on every settings question.
    inlay: Mutex<InlayHints>,
    /// The Lombok agent of the running server, if the project uses Lombok.
    lombok: Mutex<Option<Lombok>>,
    /// When the last start began, so that a stale log is not read as its cause.
    started: Mutex<Option<SystemTime>>,
    /// The latest import failure the server reported, for the window.
    import_failure: watch::Sender<Option<ImportFailure>>,
    /// The legend of the semantic tokens, which the server gives at `initialize`.
    semantic_tokens: SemanticTokensSetup,
    _lock: fs::File,
}

impl Jdtls {
    pub fn new(config: JdtlsConfig) -> io::Result<Self> {
        let root = CanonPath::new(&config.project_root)?;
        let launcher_jar = equinox_launcher(&config.jdtls_home)?;
        fs::create_dir_all(&config.state_dir)?;
        let lock = fs::File::create(config.state_dir.join(LOCK_FILE))?;
        if lock.try_lock().is_err() {
            let message = "jdtls already runs for this project in another window";
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, message));
        }
        let (import_failure, _) = watch::channel(None);
        Ok(Self {
            runtimes: Mutex::new(config.runtimes.clone()),
            inlay: Mutex::new(InlayHints::default()),
            lombok: Mutex::new(None),
            config,
            root,
            launcher_jar,
            autobuild: Autobuild::default(),
            heap_mb: AtomicU32::new(first_heap_mb(platform::physical_memory())),
            started: Mutex::new(None),
            import_failure,
            semantic_tokens: SemanticTokensSetup::new(),
            _lock: lock,
        })
    }

    /// The legend the server declared, filled when it answers `initialize`.
    pub fn semantic_tokens(&self) -> SemanticTokensSetup {
        self.semantic_tokens.clone()
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
        settings(&self.config, &runtimes, self.autobuild.enabled(), inlay)
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

    /// Turns compilation on save off while a debug session runs.
    pub fn autobuild(&self) -> Autobuild {
        self.autobuild.clone()
    }

    pub fn log_file(&self) -> PathBuf {
        self.config.state_dir.join(LOG_FILE)
    }

    /// The latest failure of the build import, and every later one.
    pub fn watch_import_failure(&self) -> watch::Receiver<Option<ImportFailure>> {
        self.import_failure.subscribe()
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

fn settings(
    config: &JdtlsConfig,
    runtimes: &[Runtime],
    autobuild: bool,
    inlay: InlayHints,
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
        "wrapper": {"enabled": true},
    });
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
    json!({
        "java": {
            "autobuild": {"enabled": autobuild},
            "completion": {
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
            },
            "signatureHelp": {"enabled": true},
            "semanticHighlighting": {"enabled": true},
            "format": {"enabled": true},
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
    })
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
        "advancedExtractRefactoringSupport": false,
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
                // No resolveSupport: the editor applies an action as soon as it is picked,
                // so the edits come with the list.
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
        match startup::version_problem(&self.config.java_home) {
            Some(problem) => Err(problem),
            None => Ok(()),
        }
    }

    fn starting(&self) {
        let now = SystemTime::now();
        *self
            .started
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = Some(now);
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
            config.jdtls_home.join("config_win").display()
        ));
        args.extend(JVM_SETTINGS.iter().map(|arg| (*arg).to_owned()));
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
        args.extend([
            "-jar".to_owned(),
            self.launcher_jar.display().to_string(),
            "-configuration".to_owned(),
            config.state_dir.join("config").display().to_string(),
            "-data".to_owned(),
            config.state_dir.join("workspace").display().to_string(),
        ]);
        Plan {
            program: config.java_home.join("bin").join("java.exe"),
            args,
            env: config.environment.clone(),
            directory: Some(config.state_dir.clone()),
            log: Some(self.log_file()),
            label: Some("Language server"),
        }
    }

    fn crashed(&self, exit_code: Option<i32>) {
        // A new server imports the project again, so an old failure says nothing.
        self.import_failure.send_replace(None);
        if exit_code == Some(EXIT_OUT_OF_MEMORY) {
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
    }

    fn state_of(&self, method: &str, params: &Value) -> Option<State> {
        import::observe(method, params, &self.import_failure);
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
        static NEXT_ROOT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("jdtls-adapter-{}-{n}", std::process::id()));
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
            gradle_user_home: Some(PathBuf::from(r"E:\gradle")),
            gradle_java_home: None,
            offline: true,
            environment: HashMap::new(),
        };
        (Jdtls::new(config).unwrap(), root)
    }

    fn jdtls_on(project_root: PathBuf) -> (Jdtls, PathBuf) {
        static NEXT_PROJECT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT_PROJECT.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("jdtls-adapter-proj-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
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
            gradle_user_home: None,
            gradle_java_home: None,
            offline: true,
            environment: HashMap::new(),
        };
        (Jdtls::new(config).unwrap(), root)
    }

    #[test]
    fn the_import_shortens_the_gradle_daemon_idle_timeout_unless_the_project_sets_one() {
        let project =
            std::env::temp_dir().join(format!("jdtls-adapter-gradle-props-{}", std::process::id()));
        let _ = fs::remove_dir_all(&project);
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join("build.gradle"), "plugins { id 'java' }\n").unwrap();
        let (jdtls, root) = jdtls_on(project.clone());
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
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&project);
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
