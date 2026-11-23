//! The Arro binary: `arro [--state-dir DIR] [--script STEPS] [--script-file PATH]
//! [--mock-agents] [--new-instance] [--after-update PID] [PATH]`. `PATH` is a project
//! folder, or a file, which opens in the folder that holds it. A start without
//! `--new-instance` hands its path to the program that already runs in the same state
//! folder and leaves. `--mock-agents` runs harnesses in an empty home against a local
//! model API, in builds with the `mock-agents` feature only. `--after-update` is set by
//! a restart and waits for the old process before the project opens. `--take-over` is
//! set by the start screen, which closes after a project opens: the project listens for
//! the next start once the start screen lets the state folder go.
//! `--script-file` reads the steps of `--script` from a file, one step per line or
//! separated by `;`.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use editor_view::Languages;
use java_lang::{BuildTools, JavaSession, Missing, Toolchain, Trust};
use lang_api::LanguageSupport;
use panels::LanguageServer;
use panels::{Creates, FileTemplate, ProjectSetup, ProjectTemplate, TemplatesSetup};
use syntax::LanguageConfig;
use workspace::{AppInfo, FileIcons, Launch, OpenRequests, Request, Settings};

mod migration;
mod panic_log;

/// The text of a new file from its folder, the project root and its name.
type FileContent = fn(&Path, &Path, &str) -> String;

fn file_icons() -> FileIcons {
    let mut icons = FileIcons {
        by_name: vec![
            (".gitignore", "fileTypes/gitignore"),
            ("java", "nodes/class"),
            ("xml", "fileTypes/xml"),
            ("iml", "fileTypes/ideaModule"),
        ],
        fallback: "fileTypes/text",
    }
    .with_languages();
    icons.by_name.extend(java_lang::file_icons());
    icons
}

/// What a file is: Java first, then the languages around a project.
fn language_for(path: &Path) -> Option<LanguageConfig> {
    java_lang::language_for(path)
        .or_else(|| dockerfile_syntax::language_for(path))
        .or_else(|| file_syntax::language_for(path))
        .map(|config| config.with_injection_resolver(language_by_name))
}

/// The language a code fence or another document names in its text.
fn language_by_name(name: &str) -> Option<LanguageConfig> {
    let name = name.split_whitespace().next()?;
    language_for(Path::new(&format!("fence.{name}")))
}

/// The name the code style files of a project give the language of this file, where the
/// editor's own name is spelled differently there. Other languages use the editor name.
fn code_style_language(path: &Path) -> Option<&'static str> {
    path.extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("java"))
        .then_some("JAVA")
}

fn class_text(folder: &Path, root: &Path, name: &str) -> String {
    java_lang::new_source("class", root, folder, name)
}

fn interface_text(folder: &Path, root: &Path, name: &str) -> String {
    java_lang::new_source("interface", root, folder, name)
}

fn enum_text(folder: &Path, root: &Path, name: &str) -> String {
    java_lang::new_source("enum", root, folder, name)
}

fn record_text(folder: &Path, root: &Path, name: &str) -> String {
    java_lang::new_source("record", root, folder, name)
}

fn annotation_text(folder: &Path, root: &Path, name: &str) -> String {
    java_lang::new_source("annotation", root, folder, name)
}

fn package_info_text(folder: &Path, root: &Path, name: &str) -> String {
    java_lang::new_source("package-info", root, folder, name)
}

/// What the project tree creates besides a plain file and folder.
fn file_templates() -> Vec<FileTemplate> {
    let mut templates = Vec::new();
    for definition in java_lang::file_templates() {
        let (icon, content): (&'static str, FileContent) = match definition.id.as_str() {
            "interface" => ("nodes/interface", interface_text),
            "enum" => ("nodes/enum", enum_text),
            "record" => ("nodes/record", record_text),
            "annotation" => ("fileTypes/text", annotation_text),
            "package-info" => ("fileTypes/text", package_info_text),
            _ => ("nodes/class", class_text),
        };
        templates.push(FileTemplate {
            label: Box::leak(definition.label.into_boxed_str()),
            icon,
            extension: "java",
            creates: Creates::File,
            content,
        });
    }
    templates.push(FileTemplate {
        label: "Package",
        icon: "nodes/package",
        extension: "",
        creates: Creates::Folder { dotted: true },
        content: |_, _, _| String::new(),
    });
    templates
}

#[derive(Debug, Default, PartialEq)]
struct Args {
    path: Option<PathBuf>,
    state_dir: Option<PathBuf>,
    script: Option<String>,
    script_file: Option<PathBuf>,
    mock_agents: bool,
    /// This start is a window of its own: it does not hand its path to a running program.
    new_instance: bool,
    /// Waits for the state folder to be free and listens there: the start screen closes
    /// after it opens a project, and the project becomes the program of the next start.
    take_over: bool,
    /// The process this one replaces, set by a restart after an update.
    after_update: Option<u32>,
}

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Args, String> {
    let mut parsed = Args::default();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let mut value = |name: &str| args.next().ok_or(format!("{name} needs a value"));
        match arg.as_str() {
            "--state-dir" => parsed.state_dir = Some(value("--state-dir")?.into()),
            "--script" => parsed.script = Some(value("--script")?),
            "--script-file" => parsed.script_file = Some(value("--script-file")?.into()),
            "--after-update" => {
                let text = value("--after-update")?;
                parsed.after_update = Some(
                    text.parse()
                        .map_err(|_| format!("--after-update needs a process id: {text}"))?,
                );
            }
            "--mock-agents" if cfg!(feature = "mock-agents") => parsed.mock_agents = true,
            "--new-instance" => parsed.new_instance = true,
            "--take-over" => parsed.take_over = true,
            flag if flag.starts_with("--") => return Err(format!("unknown argument {flag}")),
            path => parsed.path = Some(path.into()),
        }
    }
    Ok(parsed)
}

/// Windows gives canonical paths the `\\?\` prefix, which must not reach the UI.
fn canonical(path: &Path) -> PathBuf {
    let full = std::fs::canonicalize(path)
        .or_else(|_| std::path::absolute(path))
        .unwrap_or_else(|_| path.to_path_buf());
    let text = full.to_string_lossy();
    text.strip_prefix(r"\\?\")
        .map_or(full.clone(), PathBuf::from)
}

/// The build tool of the project and what runs it. With `--state-dir` the model cache
/// moves there too, so that a scripted run leaves nothing behind.
fn project_setup(root: &Path, state_dir: Option<&Path>, build_tools: &BuildTools) -> ProjectSetup {
    let build = java_lang::project_build(
        root,
        state_dir.map(|dir| dir.join("projects")),
        state_dir.map(PathBuf::from),
        build_tools.clone(),
    );
    ProjectSetup {
        tool: build.tool,
        providers: build.providers,
        tests: build.tests,
        test_sites: java_lang::test_sites,
        cache_root: build.cache_root,
        templates: file_templates(),
        skipped_folders: vec![
            "target".into(),
            "build".into(),
            "out".into(),
            ".gradle".into(),
        ],
        debug: None,
        environment: None,
    }
}

/// `%APPDATA%/Arro`, where plugins and the settings file live.
fn settings_folder() -> PathBuf {
    base::app::folder("APPDATA")
}

/// `%LOCALAPPDATA%/Arro`, where the state and the caches live.
fn state_folder() -> PathBuf {
    store::state_dir(base::app::FOLDER)
}

/// Records why an old folder could not be taken over. The window has no console in release.
fn log_migration(logs: &Path, failed: &[String]) {
    let _ = std::fs::create_dir_all(logs);
    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(logs.join("migration.log"))
    else {
        return;
    };
    for reason in failed {
        let _ = writeln!(file, "{reason}");
    }
}

fn launch(mut args: Args) -> Result<(Launch, ProjectSetup, Args), String> {
    // The steps of a scenario file run after the steps of `--script`, so a caller can
    // give a file and add a step of its own to it.
    if let Some(path) = args.script_file.take() {
        let text = std::fs::read_to_string(&path)
            .map_err(|error| format!("{} is not read: {error}", path.display()))?;
        args.script = Some(match args.script.take() {
            Some(script) => format!("{script};{text}"),
            None => text,
        });
    }
    let path = match args.path.clone() {
        Some(path) => path,
        None => std::env::current_dir().map_err(|error| error.to_string())?,
    };
    if !path.exists() {
        return Err(format!("{} does not exist", path.display()));
    }
    let path = canonical(&path);
    let (root, open) = if path.is_dir() {
        (path, None)
    } else {
        let folder = path.parent().ok_or("a file needs a folder")?.to_path_buf();
        (folder, Some(path))
    };
    let launch = Launch {
        root,
        open,
        state_dir: args.state_dir.clone().unwrap_or_else(state_folder),
        settings_dir: args.state_dir.clone().unwrap_or_else(settings_folder),
        icons: file_icons(),
        background: args.script.is_some(),
        info: AppInfo {
            name: "Arro",
            version: env!("IDE_VERSION"),
            commit: env!("IDE_COMMIT"),
            date: env!("IDE_BUILD_DATE"),
            channel: env!("IDE_CHANNEL"),
        },
    };
    // File > Invalidate Caches leaves its folders for this start, before the language
    // server and the project model open them.
    for failure in workspace::apply_pending_invalidation(&launch.state_dir, &[&launch.state_dir]) {
        eprintln!("a cache is not invalidated: {failure}");
    }
    // The build tools need the settings before the project model opens, and the windows
    // need the names of those tools before the Settings window can be shown.
    let settings = Settings::load(&launch.settings_dir.join(workspace::SETTINGS_FILE));
    let project = project_setup(
        &launch.root,
        args.state_dir.as_deref(),
        &settings.build_tools,
    );
    Ok((launch, project, args))
}

/// One place to ask whether a project may run its build tools and servers. Until the
/// shared trust of the harnesses arrives, a project opened from the command line is.
fn trust_of(_root: &Path) -> Trust {
    Trust::Trusted
}

/// Looks for the JDK of jdtls, jdtls and java-debug on its own thread while the
/// settings and the project load: the search reads folders of the machine. It does
/// not start the server; `language_server` does that after the join.
fn find_toolchain() -> thread::JoinHandle<Result<Toolchain, Missing>> {
    thread::Builder::new()
        .name("java-setup".into())
        .spawn(|| {
            let exe = std::env::current_exe().ok();
            let search = java_lang::Search::from_env(exe.as_deref().and_then(Path::parent));
            java_lang::discover(&search)
        })
        .expect("the java-setup thread starts")
}

/// Starts jdtls in the background. The session must live as long as the window.
/// `toolchain` is what `find_toolchain` found; when it is missing, the status line
/// names the tool and explains where it goes.
fn language_server(
    launch: &Launch,
    toolchain: Result<Toolchain, Missing>,
    build_tools: &BuildTools,
    max_heap_mb: Option<u32>,
) -> (
    Option<JavaSession>,
    LanguageServer,
    Option<panels::LanguageSetup>,
) {
    let (sender, status) = futures_channel::mpsc::unbounded();
    let none = |status| LanguageServer {
        support: None,
        status,
    };
    let toolchain = match toolchain {
        Ok(toolchain) => toolchain,
        Err(missing) => {
            // The first line stands in the status bar, the rest in its balloon.
            let text = format!("{}\n{}", missing.status_line(), missing.details());
            sender.unbounded_send(text).ok();
            let setup = panels::LanguageSetup {
                download: (missing == Missing::Jdk).then(|| {
                    (
                        "Download JDK 25".into(),
                        "https://adoptium.net/temurin/releases/?version=25&os=windows&arch=x64&package=jdk"
                            .to_owned(),
                    )
                }),
                check: Some(Box::new(JavaSetupCheck {
                    root: launch.root.clone(),
                    state_dir: launch.state_dir.clone(),
                    build_tools: build_tools.clone(),
                    max_heap_mb,
                    session: Arc::new(Mutex::new(None)),
                })),
            };
            return (None, none(status), Some(setup));
        }
    };
    let mut config =
        java_lang::config_of_with(&toolchain, &launch.root, &launch.state_dir, build_tools);
    config.max_heap_mb = max_heap_mb;
    match JavaSession::open(config, trust_of(&launch.root)) {
        Ok(session) => {
            session.watch_status(move |text| {
                sender.unbounded_send(text).ok();
            });
            let support: Arc<dyn LanguageSupport> = session.project().clone();
            let server = LanguageServer {
                support: Some(support),
                status,
            };
            (Some(session), server, None)
        }
        Err(error) => {
            sender
                .unbounded_send(java_lang::open_error_text(&error))
                .ok();
            (None, none(status), None)
        }
    }
}

/// Searches again from the balloon when a tool was missing, and keeps the session it
/// starts alive with the window.
struct JavaSetupCheck {
    root: PathBuf,
    state_dir: PathBuf,
    build_tools: BuildTools,
    max_heap_mb: Option<u32>,
    session: Arc<Mutex<Option<JavaSession>>>,
}

impl panels::SetupCheck for JavaSetupCheck {
    fn check(
        &self,
        started: futures_channel::mpsc::UnboundedSender<Option<panels::StartedServer>>,
    ) {
        let root = self.root.clone();
        let state_dir = self.state_dir.clone();
        let build_tools = self.build_tools.clone();
        let max_heap_mb = self.max_heap_mb;
        let session = Arc::clone(&self.session);
        thread::spawn(move || {
            let exe = std::env::current_exe().ok();
            let search = java_lang::Search::from_env(exe.as_deref().and_then(Path::parent));
            let Ok(toolchain) = java_lang::discover(&search) else {
                let _ = started.unbounded_send(None);
                return;
            };
            let mut config = java_lang::config_of_with(&toolchain, &root, &state_dir, &build_tools);
            config.max_heap_mb = max_heap_mb;
            let Ok(started_session) = JavaSession::open(config, Trust::Trusted) else {
                let _ = started.unbounded_send(None);
                return;
            };
            let (sender, status) = futures_channel::mpsc::unbounded();
            started_session.watch_status(move |text| {
                sender.unbounded_send(text).ok();
            });
            let support: Arc<dyn LanguageSupport> = started_session.project().clone();
            let debugger = java_lang::JavaDebugger::new(
                started_session.project().clone(),
                started_session.handle(),
            );
            let mut slot = session.lock().unwrap_or_else(|poison| poison.into_inner());
            *slot = Some(started_session);
            let _ = started.unbounded_send(Some(panels::StartedServer {
                support,
                status,
                debug: Some(Arc::new(debugger)),
            }));
        });
    }
}

fn maven_files(name: &str) -> Vec<(String, String)> {
    java_lang::new_project_files("maven", name)
}

fn gradle_files(name: &str) -> Vec<(String, String)> {
    java_lang::new_project_files("gradle", name)
}

/// What New Project offers on the start screen.
fn project_templates() -> Vec<ProjectTemplate> {
    vec![
        ProjectTemplate {
            label: "Maven",
            files: maven_files,
        },
        ProjectTemplate {
            label: "Gradle",
            files: gradle_files,
        },
    ]
}

/// The factories of the generators of New Project, from the language of the application.
fn set_project_generators(state_dir: &Path) {
    let directory = state_dir.to_path_buf();
    panels::set_project_generators(Arc::new(move || {
        java_lang::project_generators(&directory, &java_lang::NewProjectOptions::default())
    }));
}

/// Without a project and without a script the window is the start screen.
fn start_screen(args: &Args, requests: OpenRequests) {
    panels::set_build_tool_names("Maven", "Gradle");
    let state_dir = args.state_dir.clone().unwrap_or_else(state_folder);
    let settings_dir = args.state_dir.clone().unwrap_or_else(settings_folder);
    set_project_generators(&state_dir);
    let generators =
        java_lang::project_generators(&state_dir, &java_lang::NewProjectOptions::default());
    let context = workspace::start_context(&state_dir, &settings_dir);
    panels::run_start(
        context,
        state_dir,
        project_templates(),
        generators,
        false,
        requests,
    );
}

/// What the first start of a state folder found there.
enum InstanceStart {
    /// A running program took the request: this start leaves.
    Forwarded,
    /// No program runs in the state folder: this one listens for the next start.
    Listening(platform::instance::Instance),
    /// The endpoint belongs to nobody this start can reach: open without listening.
    Alone,
}

/// Hands the path to the program that already runs in the state folder, or becomes the
/// program that listens for the next start. A socket file of a program that died is
/// replaced by the listener. A restart after an update waits for the old process and does
/// not hand its path back to it: `forward` is false there. A start that takes the folder
/// over waits `wait` for the program that leaves it.
fn become_instance(
    state_dir: &Path,
    path: Option<&Path>,
    forward: bool,
    wait: Duration,
    requests: &OpenRequests,
) -> InstanceStart {
    let request = match path {
        Some(path) => Request::Open(canonical(path)),
        None => Request::Focus,
    };
    if forward {
        match platform::instance::send(state_dir, &request) {
            Ok(true) => return InstanceStart::Forwarded,
            Ok(false) => {}
            Err(error) => eprintln!("the running program is not asked to open the path: {error}"),
        }
    }
    let deadline = Instant::now() + wait;
    loop {
        let queue = requests.clone();
        match platform::instance::Instance::listen(state_dir, move |request| queue.push(request)) {
            Ok(instance) => return InstanceStart::Listening(instance),
            Err(error) if error.kind() == std::io::ErrorKind::AddrInUse && forward => {
                // A program started at the same time took the endpoint: ask it again.
                return match platform::instance::send(state_dir, &request) {
                    Ok(true) => InstanceStart::Forwarded,
                    _ => InstanceStart::Alone,
                };
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::AddrInUse && Instant::now() < deadline =>
            {
                // The start screen closes after this start opened its project: wait for
                // it to let go of the endpoint.
                std::thread::sleep(TAKE_OVER_STEP);
            }
            Err(error) => {
                eprintln!("this program does not listen for a second start: {error}");
                return InstanceStart::Alone;
            }
        }
    }
}

/// How long a start that takes the state folder over waits for the program that leaves
/// it, and how often it asks.
const TAKE_OVER_WAIT: Duration = Duration::from_secs(5);
const TAKE_OVER_STEP: Duration = Duration::from_millis(50);

/// How long the new process of a restart waits for the one it replaces.
const AFTER_UPDATE_TIMEOUT: Duration = Duration::from_secs(30);

/// Waits for the process that started this one, so that its language server has let
/// go of the project folder before this one opens it. The window opens after the
/// wait, and a stuck process loses the argument after [`AFTER_UPDATE_TIMEOUT`].
fn wait_for_old_process(process_id: Option<u32>) {
    let Some(process_id) = process_id else {
        return;
    };
    let waiter = match platform::ProcessWaiter::open(process_id) {
        Ok(Some(waiter)) => waiter,
        Ok(None) => return,
        Err(error) => {
            eprintln!("the previous process is not waited for: {error}");
            return;
        }
    };
    let ended = updater::wait_until(
        || waiter.wait(Duration::from_millis(50)).unwrap_or(true),
        AFTER_UPDATE_TIMEOUT,
    );
    if !ended {
        eprintln!(
            "the previous process did not end in {} seconds",
            AFTER_UPDATE_TIMEOUT.as_secs()
        );
    }
}

/// The old process passes its environment to this one, including the tool folders of
/// the release that was just replaced. A variable that names a folder inside the
/// program folder is dropped, so the search reads the new release; a folder the user
/// chose elsewhere stays.
fn forget_replaced_tools() {
    let Ok(install) = updater::install_dir() else {
        return;
    };
    for name in [java_lang::JDTLS_HOME, java_lang::DEBUG_PLUGIN] {
        let Some(value) = std::env::var_os(name) else {
            continue;
        };
        if !inside(Path::new(&value), &install) {
            continue;
        }
        // SAFETY: this runs at the start, before any thread reads the variables.
        unsafe { std::env::remove_var(name) };
    }
}

/// Whether `path` lies in `folder`, comparing the spelling of the disk.
fn inside(path: &Path, folder: &Path) -> bool {
    match (
        platform::CanonPath::new(path),
        platform::CanonPath::new(folder),
    ) {
        (Ok(path), Ok(folder)) => path.as_path().starts_with(folder.as_path()),
        _ => path.starts_with(folder),
    }
}

fn main() -> ExitCode {
    // The keys and the hints follow the system: Command on macOS, Control elsewhere.
    workspace::choose_platform_keymap();
    let migration = migration::take_over_old_folders();
    let parsed = parse_args(std::env::args().skip(1));
    if let Ok(args) = &parsed {
        if args.after_update.is_some() {
            forget_replaced_tools();
        }
        wait_for_old_process(args.after_update);
    }
    let state_dir = parsed
        .as_ref()
        .ok()
        .and_then(|args| args.state_dir.clone())
        .unwrap_or_else(state_folder);
    let logs = state_dir.join("logs");
    panic_log::install(logs.clone());
    let _ = platform::install_crash_handler(
        logs.clone(),
        env!("IDE_VERSION").to_owned(),
        env!("IDE_COMMIT").to_owned(),
    );
    if !migration.is_empty() {
        log_migration(&logs, &migration);
    }
    let requests = OpenRequests::default();
    let mut instance = None;
    if let Ok(args) = &parsed
        && !args.new_instance
        && args.script.is_none()
        && args.script_file.is_none()
    {
        match become_instance(
            &state_dir,
            args.path.as_deref(),
            args.after_update.is_none() && !args.take_over,
            if args.take_over {
                TAKE_OVER_WAIT
            } else {
                Duration::ZERO
            },
            &requests,
        ) {
            InstanceStart::Forwarded => return ExitCode::SUCCESS,
            InstanceStart::Listening(server) => instance = Some(server),
            InstanceStart::Alone => {}
        }
    }
    if let Ok(args) = &parsed
        && args.path.is_none()
        && args.script.is_none()
        && args.script_file.is_none()
    {
        start_screen(args, requests);
        return ExitCode::SUCCESS;
    }
    // The listener lives as long as the window: it is the program the next start talks to.
    let _instance = instance;
    let toolchain = find_toolchain();
    let started = parsed.and_then(launch);
    match started {
        Ok((launch, project, args)) => {
            // The search started before the settings and the project were read, so
            // the window waits only for the slower of the two.
            let toolchain = toolchain.join().unwrap_or(Err(Missing::Jdtls));
            let settings =
                workspace::Settings::load(&launch.settings_dir.join(workspace::SETTINGS_FILE));
            panels::set_build_tool_names("Maven", "Gradle");
            set_project_generators(&launch.state_dir);
            platform::MemoryPressure::configure(settings.memory_thresholds());
            let languages = Languages {
                resolve: language_for,
                templates: java_lang::templates_for,
            };
            let (session, server, setup) = language_server(
                &launch,
                toolchain,
                &settings.build_tools,
                settings.language_max_heap(),
            );
            let mut project = project;
            if let Some(session) = &session {
                let debugger =
                    java_lang::JavaDebugger::new(session.project().clone(), session.handle());
                project.debug = Some(Arc::new(debugger));
            }
            project.environment = Some(java_lang::project_environment(
                launch.root.clone(),
                session.as_ref().map(|session| session.project().clone()),
            ));
            let root = &launch.root;
            let search = panels::SearchSetup {
                excludes: ["target", "build", "out", ".gradle"]
                    .iter()
                    .map(|folder| root.join(folder))
                    .collect(),
                symbols_for: java_lang::symbols_for,
                extra_sources: Vec::new(),
            };
            let cache = panels::CacheLocations {
                server: Some(java_lang::project_state_dir(
                    &launch.state_dir,
                    &launch.root,
                )),
            };
            let templates = TemplatesSetup {
                language: java_lang::templates_for(Path::new("Arro.java")),
                files: java_lang::file_templates(),
            };
            let database = Arc::new(
                java_lang::DatabaseSupport::new(
                    launch.root.clone(),
                    project.tool.clone(),
                    project.cache_root.clone(),
                )
                .with_data_dir(launch.state_dir.clone()),
            );
            panels::set_database_support(database.clone());
            panels::set_migration_support(database);
            panels::set_jshell_support(Arc::new(java_lang::JShellSupport::new(
                launch.root.clone(),
                project.tool.clone(),
                project.cache_root.clone(),
            )));
            let scratch = panels::ScratchFileTypes(vec![panels::ScratchFileType {
                extension: "java",
                label: language_for(Path::new("scratch.java")).map_or("Java", |config| config.name),
            }]);
            panels::run_with_setup(
                launch,
                requests,
                languages,
                scratch,
                search,
                project,
                Some(server),
                setup,
                cache,
                templates,
                code_style_language,
                args.mock_agents,
                args.script,
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Args, String> {
        parse_args(args.iter().map(|arg| arg.to_string()))
    }

    #[test]
    fn reads_flags_and_the_path() {
        let args = parse(&["--state-dir", "x", "--script", "quit", "project"]).unwrap();
        assert_eq!(args.path, Some(PathBuf::from("project")));
        assert_eq!(args.state_dir, Some(PathBuf::from("x")));
        assert_eq!(args.script.as_deref(), Some("quit"));
        assert!(!args.new_instance);
    }

    #[test]
    fn reads_the_flag_of_a_window_of_its_own() {
        let args = parse(&["--new-instance", "project"]).unwrap();
        assert!(args.new_instance);
        assert!(!args.take_over);
        assert_eq!(args.path, Some(PathBuf::from("project")));
    }

    #[test]
    fn reads_the_flag_of_a_start_that_takes_the_folder_over() {
        let args = parse(&["--take-over", "project"]).unwrap();
        assert!(args.take_over);
        assert!(!args.new_instance);
        assert_eq!(args.path, Some(PathBuf::from("project")));
    }

    #[test]
    fn a_second_start_hands_the_path_to_the_running_instance() {
        let folder = test_support::TempDir::new("ide-second-start");
        let file = folder.path().join("notes.txt");
        std::fs::write(&file, "x").unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        let listener = platform::instance::Instance::listen(folder.path(), move |request| {
            sender.send(request).ok();
        })
        .unwrap();

        let requests = OpenRequests::default();
        let started = become_instance(folder.path(), Some(&file), true, Duration::ZERO, &requests);

        assert!(matches!(started, InstanceStart::Forwarded));
        assert_eq!(
            receiver.recv_timeout(Duration::from_secs(10)).unwrap(),
            Request::Open(canonical(&file))
        );
        drop(listener);
    }

    #[test]
    fn the_first_start_listens_for_the_next_one() {
        let folder = test_support::TempDir::new("ide-first-start");
        let requests = OpenRequests::default();
        let started = become_instance(folder.path(), None, true, Duration::ZERO, &requests);
        let InstanceStart::Listening(listener) = started else {
            panic!("the first start listens");
        };

        assert!(
            platform::instance::send(folder.path(), &Request::Focus).unwrap(),
            "the running instance takes the request"
        );
        assert_eq!(requests.take(), [Request::Focus]);
        drop(listener);
    }

    #[test]
    fn refuses_unknown_flags_and_missing_values() {
        assert!(parse(&["--nope"]).is_err());
        assert!(parse(&["--script"]).is_err());
    }

    #[test]
    fn reads_a_script_file_flag() {
        let args = parse(&["--script-file", "tools/smoke/release.script", "project"]).unwrap();
        assert_eq!(
            args.script_file,
            Some(PathBuf::from("tools/smoke/release.script"))
        );
        assert_eq!(args.script, None);
        assert!(parse(&["--script-file"]).is_err());
    }

    #[test]
    fn missing_path_is_an_error() {
        let args = Args {
            path: Some(PathBuf::from("Z:/no/such/folder")),
            ..Args::default()
        };
        assert!(launch(args).is_err());
    }

    #[test]
    fn canonical_paths_have_no_extended_prefix() {
        let path = canonical(&std::env::temp_dir());
        assert!(!path.to_string_lossy().starts_with(r"\\?\"));
    }

    #[test]
    fn a_new_class_knows_its_package_from_the_folder() {
        let root = Path::new("C:/shop");
        let folder = root.join("src/main/java/com/shop");
        let class = class_text(&folder, root, "Order");
        assert!(class.starts_with("package com.shop;"), "{class}");
        assert!(class.contains("public class Order"), "{class}");
        assert!(file_templates().iter().any(|t| t.label == "Package"));
    }

    #[test]
    fn the_start_screen_offers_maven_and_gradle() {
        let templates = project_templates();
        let labels: Vec<&str> = templates.iter().map(|template| template.label).collect();
        assert_eq!(labels, ["Maven", "Gradle"]);
        for template in &templates {
            let files = (template.files)("demo");
            assert!(files.iter().any(|(path, _)| path.ends_with("App.java")));
        }
    }

    #[test]
    fn known_extensions_have_icons() {
        let icons = file_icons();
        assert_eq!(icons.icon_for(Path::new("App.java")), "nodes/class");
        assert_eq!(icons.icon_for(Path::new("notes.md")), "fileTypes/markdown");
        assert_eq!(icons.icon_for(Path::new("notes.log")), "fileTypes/text");
    }

    #[test]
    fn file_types_of_a_project_carry_their_own_icons() {
        let icons = file_icons();
        for (path, icon) in [
            ("application.properties", "fileTypes/properties"),
            ("gradle-wrapper.jar", "fileTypes/archive"),
            ("logo.svg", "fileTypes/image"),
            ("build.gradle.kts", "fileTypes/gradle"),
            (".editorconfig", "fileTypes/editorConfig"),
            ("App.class", "fileTypes/javaClass"),
            ("notes.unknown", "fileTypes/text"),
        ] {
            assert_eq!(icons.icon_for(Path::new(path)), icon, "{path}");
        }
    }

    #[test]
    fn a_fenced_block_named_java_is_parsed_as_java() {
        use syntax::{DocumentId, HighlightKind, SyntaxService};
        use text_core::{Change, RopeStore, TextStore};

        let text = "```java\nclass Demo { int value = 7; }\n```\n";
        let config = file_syntax::markdown().with_injection_resolver(language_by_name);
        let service = SyntaxService::new();
        let (sender, receiver) = std::sync::mpsc::channel();
        service.open(DocumentId(1), config, move |snapshot| {
            sender.send(snapshot).ok();
        });
        service.update(
            DocumentId(1),
            1,
            RopeStore::from_text(text),
            &Change::default(),
        );
        let snapshot = receiver
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("the parse");
        let keywords: Vec<_> = snapshot
            .highlights
            .iter()
            .filter(|span| span.kind == HighlightKind::Keyword)
            .map(|span| &text[span.start as usize..span.end as usize])
            .collect();
        assert!(keywords.contains(&"class"), "{keywords:?}");
        assert!(keywords.contains(&"int"), "{keywords:?}");
    }
}
