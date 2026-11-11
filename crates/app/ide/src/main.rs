//! The Arro binary: `arro [--state-dir DIR] [--script STEPS] [--mock-agents] [PATH]`. `PATH` is
//! a project folder, or a file, which opens in the folder that holds it. `--mock-agents` runs
//! harnesses in an empty home against a local model API, in builds with the `mock-agents`
//! feature only.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::thread;

use editor_view::Languages;
use java_lang::{JavaSession, Missing, Toolchain, Trust};
use lang_api::LanguageSupport;
use panels::LanguageServer;
use panels::{Creates, FileTemplate, ProjectSetup, ProjectTemplate};
use syntax::LanguageConfig;
use workspace::{AppInfo, FileIcons, Launch};

mod migration;
mod panic_log;

fn file_icons() -> FileIcons {
    FileIcons {
        by_name: vec![
            (".gitignore", "fileTypes/gitignore"),
            ("java", "nodes/class"),
            ("xml", "fileTypes/xml"),
            ("iml", "fileTypes/ideaModule"),
        ],
        fallback: "fileTypes/text",
    }
}

/// What a file is: Java first, then the languages around a project.
fn language_for(path: &Path) -> Option<LanguageConfig> {
    java_lang::language_for(path).or_else(|| dockerfile_syntax::language_for(path))
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

/// What the project tree creates besides a plain file and folder.
fn file_templates() -> Vec<FileTemplate> {
    let source = |label, icon, content| FileTemplate {
        label,
        icon,
        extension: "java",
        creates: Creates::File,
        content,
    };
    vec![
        source("Java Class", "nodes/class", class_text),
        source("Interface", "nodes/interface", interface_text),
        source("Enum", "nodes/enum", enum_text),
        source("Record", "nodes/record", record_text),
        FileTemplate {
            label: "Package",
            icon: "nodes/package",
            extension: "",
            creates: Creates::Folder { dotted: true },
            content: |_, _, _| String::new(),
        },
    ]
}

#[derive(Debug, Default, PartialEq)]
struct Args {
    path: Option<PathBuf>,
    state_dir: Option<PathBuf>,
    script: Option<String>,
    mock_agents: bool,
}

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Args, String> {
    let mut parsed = Args::default();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let mut value = |name: &str| args.next().ok_or(format!("{name} needs a value"));
        match arg.as_str() {
            "--state-dir" => parsed.state_dir = Some(value("--state-dir")?.into()),
            "--script" => parsed.script = Some(value("--script")?),
            "--mock-agents" if cfg!(feature = "mock-agents") => parsed.mock_agents = true,
            flag if flag.starts_with("--") => return Err(format!("unknown argument {flag}")),
            path => parsed.path = Some(path.into()),
        }
    }
    Ok(parsed)
}

/// Windows gives canonical paths the `\\?\` prefix, which must not reach the UI.
fn canonical(path: &Path) -> PathBuf {
    let full = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let text = full.to_string_lossy();
    text.strip_prefix(r"\\?\")
        .map_or(full.clone(), PathBuf::from)
}

/// The build tool of the project and what runs it. With `--state-dir` the model cache
/// moves there too, so that a scripted run leaves nothing behind.
fn project_setup(root: &Path, state_dir: Option<&Path>) -> ProjectSetup {
    let build = java_lang::project_build(root, state_dir.map(|dir| dir.join("projects")));
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

fn launch(args: Args) -> Result<(Launch, ProjectSetup, Args), String> {
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
    let project = project_setup(&launch.root, args.state_dir.as_deref());
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
                    session: Arc::new(Mutex::new(None)),
                })),
            };
            return (None, none(status), Some(setup));
        }
    };
    let config = java_lang::config_of(&toolchain, &launch.root, &launch.state_dir);
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
    session: Arc<Mutex<Option<JavaSession>>>,
}

impl panels::SetupCheck for JavaSetupCheck {
    fn check(
        &self,
        started: futures_channel::mpsc::UnboundedSender<Option<panels::StartedServer>>,
    ) {
        let root = self.root.clone();
        let state_dir = self.state_dir.clone();
        let session = Arc::clone(&self.session);
        thread::spawn(move || {
            let exe = std::env::current_exe().ok();
            let search = java_lang::Search::from_env(exe.as_deref().and_then(Path::parent));
            let Ok(toolchain) = java_lang::discover(&search) else {
                let _ = started.unbounded_send(None);
                return;
            };
            let config = java_lang::config_of(&toolchain, &root, &state_dir);
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

/// Without a project and without a script the window is the start screen.
fn start_screen(args: &Args) {
    let state_dir = args.state_dir.clone().unwrap_or_else(state_folder);
    let settings_dir = args.state_dir.clone().unwrap_or_else(settings_folder);
    let context = workspace::start_context(&state_dir, &settings_dir);
    panels::run_start(context, state_dir, project_templates(), false);
}

fn main() -> ExitCode {
    let migration = migration::take_over_old_folders();
    let parsed = parse_args(std::env::args().skip(1));
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
    if let Ok(args) = &parsed
        && args.path.is_none()
        && args.script.is_none()
    {
        start_screen(args);
        return ExitCode::SUCCESS;
    }
    let toolchain = find_toolchain();
    let started = parsed.and_then(launch);
    match started {
        Ok((launch, project, args)) => {
            // The search started before the settings and the project were read, so
            // the window waits only for the slower of the two.
            let toolchain = toolchain.join().unwrap_or(Err(Missing::Jdtls));
            let settings =
                workspace::Settings::load(&launch.settings_dir.join(workspace::SETTINGS_FILE));
            platform::MemoryPressure::configure(platform::MemoryThresholds::new(
                settings.memory_low_percent,
                settings.memory_low_megabytes,
            ));
            let languages = Languages {
                resolve: language_for,
                templates: java_lang::templates_for,
            };
            let (session, server, setup) = language_server(&launch, toolchain);
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
            panels::run_with_setup(
                launch,
                languages,
                search,
                project,
                Some(server),
                setup,
                cache,
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
    }

    #[test]
    fn refuses_unknown_flags_and_missing_values() {
        assert!(parse(&["--nope"]).is_err());
        assert!(parse(&["--script"]).is_err());
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
        assert_eq!(icons.icon_for(Path::new("notes.md")), "fileTypes/text");
    }
}
