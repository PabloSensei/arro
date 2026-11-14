//! The real language and debugger for the window tests of `panels`.
//!
//! The product reaches Java through the application crate only. A window test needs the
//! same real server, but it lives in a neutral crate, so this test-only tool is the one
//! place that knows both. Nothing here is part of a release.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use base::Cancel;
use dap_client::DebugService;
use java_lang::{JavaDebugger, JavaSession, JdtlsConfig, Runtime, Trust};
use lang_api::{LanguageSupport, SymbolProvider};
use proc_sup::ProcessSpec;
use project_model::{BuildSystem, ProjectModel};
use runner::{RunConfig, RunConfigProvider, RunPlan, TestSite};
use syntax::LanguageConfig;
use templates::TemplateLanguage;

pub use java_lang::BuildTools;
pub use java_project::{BuildEnv, Gradle, Jdk, Maven};
pub use jdtls_adapter::{self, settings_of};

/// The names the language gives its two build tools, in the order of their pages.
pub fn build_tool_names() -> (String, String) {
    ("Maven".to_owned(), "Gradle".to_owned())
}

/// The support of the configuration files of a project whose class path is known: the
/// jars and the output folders, for a window test that builds them itself.
pub fn spring_config(root: &Path, classpath: Vec<PathBuf>) -> Arc<dyn LanguageSupport> {
    let support = java_lang::spring_config(root.to_path_buf());
    support.set_classpath(classpath);
    support
}

/// The support of the configuration files of a project with the class path of its model.
pub fn spring_config_with_model(root: &Path, model: &ProjectModel) -> Arc<dyn LanguageSupport> {
    let support = java_lang::spring_config(root.to_path_buf());
    support.set_model(model);
    support
}

/// The support of the entities and the queries of the data repositories of a project,
/// read from its sources.
pub fn spring_data(root: &Path) -> Arc<dyn LanguageSupport> {
    java_lang::spring_data_support(root.to_path_buf())
}

/// The support of the configuration files of a project whose class path and source roots
/// the caller gives, for a window test that builds the jar itself.
pub fn spring_config_with_sources(
    root: &Path,
    classpath: Vec<PathBuf>,
    sources: Vec<PathBuf>,
) -> Arc<dyn LanguageSupport> {
    let support = java_lang::spring_config(root.to_path_buf());
    support.set_classpath(classpath);
    support.set_sources(sources);
    support
}

/// The message bundles of a project: the `*.properties` files of a base name with their
/// locales and the places of the sources that use a key.
pub fn message_bundles(root: &Path) -> Arc<dyn LanguageSupport> {
    java_lang::message_bundles(root.to_path_buf())
}

/// The message bundles of a project whose resource folders the caller names.
pub fn message_bundles_with_roots(root: &Path, roots: Vec<PathBuf>) -> Arc<dyn LanguageSupport> {
    let support = java_lang::message_bundles(root.to_path_buf());
    support.set_roots(roots);
    support
}

/// The path of a source file of a test project, under the source root of its only module.
pub fn source_path(root: &Path, name: &str) -> PathBuf {
    root.join("src")
        .join("main")
        .join("java")
        .join("com")
        .join("example")
        .join(format!("{name}.java"))
}

/// Writes a source file of a test project and returns its path.
pub fn write_test_source(root: &Path, name: &str, text: &str) -> PathBuf {
    let path = source_path(root, name);
    fs::create_dir_all(path.parent().expect("the source folder")).expect("the source folder");
    fs::write(&path, text).expect("the source of the test");
    path
}

/// A source file named by the `sourceType` of the metadata jar, for Ctrl+B from a key.
pub fn write_spring_source(root: &Path) -> PathBuf {
    let file = spring_source_root(root).join("com/example/JpaProperties.java");
    fs::create_dir_all(file.parent().expect("the source folder")).expect("the source folder");
    fs::write(&file, "package com.example;\n\nclass JpaProperties {\n}\n")
        .expect("the source of the test");
    file
}

/// The root of the sources of a project the window tests build themselves.
pub fn spring_source_root(root: &Path) -> PathBuf {
    root.join("src").join("main").join("java")
}

/// A settings file for the build tool of a project, with the repository of
/// `MAVEN_USER_HOME` and no network.
pub fn offline_build_settings(state_dir: &Path) -> PathBuf {
    write_maven_settings(state_dir).expect("MAVEN_USER_HOME with the filled repository")
}

/// The full model of a real project, read by its build tool. The helpers of the test
/// build and the repository of `MAVEN_USER_HOME` are the only outside world.
pub fn project_model(root: &Path, cache: &Path, settings: &Path) -> Result<ProjectModel, String> {
    let mut build_tools = project_model::BuildTools::default();
    build_tools.first.settings_file = settings.display().to_string();
    build_tools.first.offline = true;
    let environment = Jdk::at(&corpus().join("jdk").join("17"));
    let env = BuildEnv {
        jdk: environment,
        // The formatting and style plugins of the sample project are not part of reading
        // its model, and they would slow the sync down or fail offline.
        maven_args: vec![
            "-Dspring-javaformat.skip=true".to_owned(),
            "-Dcheckstyle.skip=true".to_owned(),
        ],
        cache_root: Some(cache.to_path_buf()),
        helpers: helpers_dir(),
        build_tools,
        ..BuildEnv::default()
    };
    let tool = java_project::detect_build_system(root, &env)
        .ok_or_else(|| "the project has no build tool".to_owned())?;
    tool.sync(root, &Cancel::new())
        .map_err(|error| error.to_string())
}

fn helpers_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("IDE_HELPERS_DIR") {
        return Some(PathBuf::from(dir));
    }
    let exe = std::env::current_exe().ok()?;
    // The test binary is under `<target>/<profile>/deps`, the helpers under the profile.
    let beside = exe.parent()?.parent()?.join("helpers");
    beside.is_dir().then_some(beside)
}

/// The command the first build tool runs for `task` of the project, with the settings of
/// its page.
pub fn first_build_tool_command(
    root: &Path,
    build_tools: &BuildTools,
    jdk: Jdk,
    task: &str,
) -> ProcessSpec {
    let env = BuildEnv {
        jdk: Some(jdk),
        build_tools: build_tools.clone(),
        ..BuildEnv::default()
    };
    Maven::new(env)
        .task(root, task)
        .expect("the command of the first build tool")
}

/// The command of the second build tool for `task` of the project.
pub fn second_build_tool_command(root: &Path, build_tools: &BuildTools, task: &str) -> ProcessSpec {
    let env = BuildEnv {
        build_tools: build_tools.clone(),
        ..BuildEnv::default()
    };
    Gradle::new(env)
        .task(root, task)
        .expect("the command of the second build tool")
}

/// Writes the few files that make a folder look like an installed JDK.
pub fn write_fake_jdk(home: &Path) {
    fs::create_dir_all(home.join("bin")).expect("the bin folder of the fake JDK");
    fs::write(home.join("bin/javac.exe"), b"").expect("the compiler of the fake JDK");
    fs::write(home.join("release"), "JAVA_VERSION=\"17.0.1\"\n")
        .expect("the release file of the fake JDK");
}

/// Writes the build script of the second build tool into the project folder.
pub fn write_second_tool_project(root: &Path) {
    fs::write(root.join("build.gradle"), "plugins { id 'java' }\n")
        .expect("the build script of the fake project");
}

/// Writes the launcher that makes a folder look like an installed build tool.
pub fn write_fake_distribution(home: &Path) {
    fs::create_dir_all(home.join("bin")).expect("the bin folder of the fake distribution");
    fs::write(home.join("bin/gradle.bat"), b"").expect("the launcher of the fake distribution");
}

fn environment_value(spec: &ProcessSpec, name: &str) -> Option<String> {
    spec.environment()
        .iter()
        .find(|(known, _)| known.to_string_lossy().eq_ignore_ascii_case(name))
        .and_then(|(_, value)| value.clone())
        .map(|value| value.to_string_lossy().into_owned())
}

/// The options of the JVM that runs the first build tool.
pub fn first_tool_vm_options(spec: &ProcessSpec) -> Option<String> {
    environment_value(spec, "MAVEN_OPTS")
}

/// The user home of the second build tool in the environment of its command.
pub fn second_tool_user_home(spec: &ProcessSpec) -> Option<String> {
    environment_value(spec, "GRADLE_USER_HOME")
}

/// The runtime of the second build tool the server of `config` uses, if any.
pub fn server_second_tool_jvm(config: &JdtlsConfig) -> Option<String> {
    config
        .gradle_java_home
        .as_ref()
        .map(|path| path.display().to_string())
}

/// A settings object of the language facade for `build_tools`, with a toolchain that
/// only has to be present: the session does not start in the test.
pub fn config_with(root: &Path, state_dir: &Path, build_tools: &BuildTools) -> JdtlsConfig {
    let toolchain = java_lang::Toolchain {
        java_home: root.join("jdk"),
        jdtls_home: root.join("jdtls"),
        debug_plugin: root.join("debug.jar"),
    };
    java_lang::config_of_with(&toolchain, root, state_dir, build_tools)
}

/// The settings file of the first build tool the server of `config` reads.
pub fn server_first_tool_settings(config: &JdtlsConfig) -> Option<String> {
    config
        .maven_settings
        .as_ref()
        .map(|path| path.display().to_string())
}

/// The installation of the second build tool the server of `config` uses, if any.
pub fn server_second_tool_home(config: &JdtlsConfig) -> Option<String> {
    config
        .gradle_home
        .as_ref()
        .map(|path| path.display().to_string())
}

/// The user home of the second build tool the server of `config` uses, if any.
pub fn server_second_tool_user_home(config: &JdtlsConfig) -> Option<String> {
    config
        .gradle_user_home
        .as_ref()
        .map(|path| path.display().to_string())
}

/// No network for the dependencies of the first build tool.
pub fn server_first_tool_offline(config: &JdtlsConfig) -> bool {
    let json = settings_of(config);
    json["java"]["import"]["maven"]["offline"]["enabled"]
        .as_bool()
        .unwrap_or(false)
}

/// The settings file in the server settings of the first build tool.
pub fn server_first_tool_settings_setting(config: &JdtlsConfig) -> Option<String> {
    let json = settings_of(config);
    json["java"]["configuration"]["maven"]["userSettings"]
        .as_str()
        .map(str::to_owned)
}

/// No network for the dependencies of the second build tool.
pub fn server_second_tool_offline(config: &JdtlsConfig) -> bool {
    let json = settings_of(config);
    json["java"]["import"]["gradle"]["offline"]["enabled"]
        .as_bool()
        .unwrap_or(false)
}

/// Whether the server takes the wrapper of the second build tool.
pub fn server_second_tool_wrapper(config: &JdtlsConfig) -> bool {
    let json = settings_of(config);
    json["java"]["import"]["gradle"]["wrapper"]["enabled"]
        .as_bool()
        .unwrap_or(false)
}

/// The installation in the server settings of the second build tool.
pub fn server_second_tool_home_setting(config: &JdtlsConfig) -> Option<String> {
    let json = settings_of(config);
    json["java"]["import"]["gradle"]["home"]
        .as_str()
        .map(str::to_owned)
}

/// Folders of a build that the tests never look into.
const SKIPPED: &[&str] = &[".git", "target", "build", "bin", ".gradle", "node_modules"];

/// A source file with a `main`, compiled for the debug tests.
pub struct Probe {
    pub path: PathBuf,
    pub line: u32,
    classes: PathBuf,
}

impl Probe {
    /// The classes folder, which the debuggee command line carries.
    pub fn classes(&self) -> &Path {
        &self.classes
    }
}

/// A source file with a static field and an object that holds a field, compiled for the
/// field watchpoint tests.
pub struct WatchProbe {
    pub path: PathBuf,
    /// The line of the static field declaration, from one.
    pub field_line: u32,
    /// The line of the stop before the writes, from one.
    pub break_line: u32,
    /// The line that writes the static field, from one.
    pub write_line: u32,
    /// The line that writes the field of the object, from one.
    pub object_write_line: u32,
    classes: PathBuf,
}

impl WatchProbe {
    /// The classes folder, which the debuggee command line carries.
    pub fn classes(&self) -> &Path {
        &self.classes
    }
}

/// One open project with jdtls, the build tool of the test and the debugger of the
/// language. Dropping it ends the server once the last clone of the language is gone.
pub struct RealLanguage {
    session: JavaSession,
    root: PathBuf,
    java_home: PathBuf,
    state_dir: PathBuf,
}

impl RealLanguage {
    /// Starts jdtls on `root` and returns at once. The status comes through
    /// [`RealLanguage::watch_status`].
    pub fn open(root: PathBuf, state_dir: PathBuf) -> Result<Self, String> {
        let corpus = corpus();
        let java_home = jdk(&corpus, "25");
        let settings = write_maven_settings(&state_dir)?;
        write_maven_config(&root, &settings);
        let config = JdtlsConfig {
            jdtls_home: corpus.join("tools/jdtls-1.63.0"),
            debug_plugin: corpus.join(
                "tools/vscode-java-debug-0.60.0/extension/server/com.microsoft.java.debug.plugin-0.53.2.jar",
            ),
            java_home: java_home.clone(),
            project_root: root.clone(),
            state_dir: state_dir.join("server"),
            runtimes: vec![
                Runtime {
                    name: "JavaSE-17".to_owned(),
                    home: jdk(&corpus, "17"),
                    default: false,
                },
                Runtime {
                    name: "JavaSE-25".to_owned(),
                    home: java_home.clone(),
                    default: true,
                },
            ],
            maven_settings: Some(settings),
            gradle_home: None,
            gradle_user_home: None,
            gradle_java_home: None,
            offline: true,
            environment: HashMap::new(),
        };
        Self::start(config, root, java_home, state_dir)
    }

    /// Starts jdtls on a Gradle project with the cache the environment prepared.
    pub fn open_import_project(root: PathBuf, state_dir: PathBuf) -> Result<Self, String> {
        let corpus = corpus();
        let java_home = jdk(&corpus, "25");
        let config = JdtlsConfig {
            jdtls_home: corpus.join("tools/jdtls-1.63.0"),
            debug_plugin: corpus.join(
                "tools/vscode-java-debug-0.60.0/extension/server/com.microsoft.java.debug.plugin-0.53.2.jar",
            ),
            java_home: java_home.clone(),
            project_root: root.clone(),
            state_dir: state_dir.join("server"),
            runtimes: vec![
                Runtime {
                    name: "JavaSE-17".to_owned(),
                    home: jdk(&corpus, "17"),
                    default: false,
                },
                Runtime {
                    name: "JavaSE-25".to_owned(),
                    home: java_home.clone(),
                    default: true,
                },
            ],
            maven_settings: None,
            gradle_home: None,
            gradle_user_home: gradle_home(),
            gradle_java_home: Some(jdk(&corpus, "17")),
            offline: false,
            environment: HashMap::new(),
        };
        Self::start(config, root, java_home, state_dir)
    }

    fn start(
        config: JdtlsConfig,
        root: PathBuf,
        java_home: PathBuf,
        state_dir: PathBuf,
    ) -> Result<Self, String> {
        let session =
            JavaSession::open(config, Trust::Trusted).map_err(|error| error.to_string())?;
        Ok(Self {
            session,
            root,
            java_home,
            state_dir,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The language as the window sees it.
    pub fn support(&self) -> Arc<dyn LanguageSupport> {
        self.session.project().clone()
    }

    pub fn watch_status(&self, on_change: impl Fn(String) + Send + 'static) {
        self.session.watch_status(on_change);
    }

    /// Hands the model of the project to the language, as the window does after a sync.
    pub fn set_model(&self, model: &ProjectModel) {
        self.session
            .project()
            .set_project_model(&Arc::new(model.clone()));
    }

    /// Starts debugging through the real plugin inside jdtls.
    pub fn debugger(&self) -> Arc<dyn DebugService> {
        Arc::new(JavaDebugger::new(
            self.session.project().clone(),
            self.session.handle(),
        ))
    }

    /// The text every process of this server carries on its command line.
    pub fn process_marker(&self) -> String {
        self.state_dir.join("server").display().to_string()
    }

    /// Compiles a probe class with a `main` and a line to stop on.
    pub fn install_probe(&self) -> Result<Probe, String> {
        let source = self.root.join("src/main/java/probe/DebugProbe.java");
        if let Some(parent) = source.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        fs::write(&source, PROBE).map_err(|error| error.to_string())?;
        let classes = self.state_dir.join("probe-classes");
        self.compile_probe(&source, &classes)?;
        Ok(Probe {
            path: source,
            line: BREAK_LINE,
            classes,
        })
    }

    /// Compiles the probe of the whole debug session: a local `int` and an object with
    /// children, a call, and output on both sides of the breakpoint.
    pub fn install_flow_probe(&self) -> Result<Probe, String> {
        let source = self.root.join("src/main/java/probe/DebugProbe.java");
        if let Some(parent) = source.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        fs::write(&source, FLOW_PROBE).map_err(|error| error.to_string())?;
        let classes = self.state_dir.join("probe-classes-flow");
        self.compile_probe(&source, &classes)?;
        Ok(Probe {
            path: source,
            line: FLOW_BREAK_LINE,
            classes,
        })
    }

    /// Compiles the probe of the field watchpoints: a static field and an object with one
    /// field, each written once after the stop.
    pub fn install_watch_probe(&self) -> Result<WatchProbe, String> {
        let source = self.root.join("src/main/java/probe/DebugProbe.java");
        if let Some(parent) = source.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        fs::write(&source, WATCH_PROBE).map_err(|error| error.to_string())?;
        let classes = self.state_dir.join("probe-classes-watch");
        self.compile_probe(&source, &classes)?;
        Ok(WatchProbe {
            path: source,
            field_line: WATCH_FIELD_LINE,
            break_line: WATCH_BREAK_LINE,
            write_line: WATCH_WRITE_LINE,
            object_write_line: WATCH_OBJECT_WRITE_LINE,
            classes,
        })
    }

    fn compile_probe(&self, source: &Path, classes: &Path) -> Result<(), String> {
        let _ = fs::remove_dir_all(classes);
        fs::create_dir_all(classes).map_err(|error| error.to_string())?;
        let compiled = Command::new(self.java_home.join("bin/javac.exe"))
            .arg("-g")
            .arg("--release")
            .arg("8")
            .arg("-d")
            .arg(classes)
            .arg(source)
            .output()
            .map_err(|error| error.to_string())?;
        if !compiled.status.success() {
            return Err(format!(
                "javac failed: {}",
                String::from_utf8_lossy(&compiled.stderr)
            ));
        }
        Ok(())
    }

    /// Compiles a probe class with a `main` and a line to stop on. The class starts
    /// `count` virtual threads that wait on a latch, then stops on a breakpoint in
    /// `main` before it lets them go.
    pub fn install_virtual_threads_probe(&self, count: usize) -> Result<Probe, String> {
        install_virtual_threads_probe(&self.root, &self.state_dir, count)
    }

    /// The command that starts `probe` with the debug agent, on the JDK of `java_home`.
    pub fn debug_process_with(&self, probe: &Probe, java_home: &Path) -> ProcessSpec {
        ProbeRun {
            root: self.root.clone(),
            java: java_home.join("bin/java.exe"),
            classes: probe.classes.clone(),
        }
        .program(true)
    }

    /// What the language server says about the main class of a source file.
    pub fn resolve_main(&self, path: &Path) -> Result<Option<java_lang::ResolvedMain>, String> {
        let canonical = platform::CanonPath::new(path).map_err(|error| error.to_string())?;
        let uri = lsp_client::uri::file_uri(&canonical)
            .ok_or_else(|| format!("{} has no file URI", path.display()))?;
        let extension = java_lang::DebugExtension::new(self.session.project());
        self.session
            .handle()
            .block_on(extension.resolve_main_class(&uri))
            .map_err(|error| error.to_string())
    }

    /// Opens a file in the server the way the editor does.
    pub fn open_document(&self, path: &Path) -> Result<(), String> {
        let canonical = platform::CanonPath::new(path).map_err(|error| error.to_string())?;
        let uri = lsp_client::uri::file_uri(&canonical)
            .ok_or_else(|| format!("{} has no file URI", path.display()))?;
        let text = fs::read_to_string(path).map_err(|error| error.to_string())?;
        let support = self.support();
        let server = support
            .server()
            .ok_or_else(|| "the language server is not connected".to_string())?;
        server.open(uri.as_str(), 1, Arc::from(text));
        Ok(())
    }

    /// A JDK of the corpus, by version folder name.
    pub fn jdk_home(version: &str) -> PathBuf {
        jdk(&corpus(), version)
    }

    /// Starts the probe of [`Probe::classes`] under a JVM that waits for a debugger on
    /// `port`. The caller owns the process and ends it.
    pub fn start_remote_probe(classes: &Path, port: u16) -> std::process::Child {
        let name = if cfg!(windows) { "java.exe" } else { "java" };
        Command::new(Self::jdk_home("25").join("bin").join(name))
            .arg(format!(
                "-agentlib:jdwp=transport=dt_socket,server=y,suspend=y,address=127.0.0.1:{port}"
            ))
            .arg("-cp")
            .arg(classes)
            .arg("probe.DebugProbe")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("the remote probe starts")
    }

    /// A build tool that answers at once: the debug test needs configurations, not a
    /// build, so no tool of the project has to run.
    pub fn probe_build(&self) -> Arc<dyn BuildSystem> {
        Arc::new(StaticTool(ProjectModel {
            root: self.root.clone(),
            tool: "fixture".into(),
            resolved: true,
            ..ProjectModel::default()
        }))
    }

    pub fn probe_providers(&self, probe: &Probe) -> Vec<Arc<dyn RunConfigProvider>> {
        vec![Arc::new(ProbeRun {
            root: self.root.clone(),
            java: self.java_home.join("bin/java.exe"),
            classes: probe.classes.clone(),
        })]
    }

    /// The configuration that starts the watch probe under the debugger.
    pub fn watch_providers(&self, probe: &WatchProbe) -> Vec<Arc<dyn RunConfigProvider>> {
        vec![Arc::new(ProbeRun {
            root: self.root.clone(),
            java: self.java_home.join("bin/java.exe"),
            classes: probe.classes.clone(),
        })]
    }

    /// The configuration that starts the flow probe under the debugger, on the JDK of
    /// the server.
    pub fn flow_providers(&self, probe: &Probe) -> Vec<Arc<dyn RunConfigProvider>> {
        vec![Arc::new(FlowRun {
            program: self.debug_process_with(probe, &self.java_home),
        })]
    }
}

/// Writes and compiles the virtual thread probe before the server imports the project,
/// so that the project knows the file when the first stack is asked for.
pub fn install_virtual_threads_probe(
    root: &Path,
    state_dir: &Path,
    count: usize,
) -> Result<Probe, String> {
    let source = root.join("src/main/java/probe/DebugProbe.java");
    if let Some(parent) = source.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let text = VIRTUAL_PROBE.replace("{count}", &count.to_string());
    fs::write(&source, text).map_err(|error| error.to_string())?;
    let classes = state_dir.join("probe-classes-virtual");
    let _ = fs::remove_dir_all(&classes);
    fs::create_dir_all(&classes).map_err(|error| error.to_string())?;
    let compiler = jdk(&corpus(), "21").join("bin/javac.exe");
    let compiled = Command::new(compiler)
        .arg("-g")
        .arg("--release")
        .arg("8")
        .arg("-d")
        .arg(&classes)
        .arg(&source)
        .output()
        .map_err(|error| error.to_string())?;
    if !compiled.status.success() {
        return Err(format!(
            "javac failed: {}",
            String::from_utf8_lossy(&compiled.stderr)
        ));
    }
    Ok(Probe {
        path: source,
        line: VIRTUAL_BREAK_LINE,
        classes,
    })
}

/// Whether the status line says the server is ready for requests.
pub fn status_is_ready(text: &str) -> bool {
    text.ends_with("ready")
}

/// Whether a hint of Problems names the root build script of a Gradle project.
pub fn is_root_build_script(hint: &str) -> bool {
    hint.starts_with("build.gradle.kts:")
}

/// Whether a hint of Problems names any build script of the project root.
pub fn is_build_script(hint: &str) -> bool {
    hint.starts_with("build.gradle:") || hint.starts_with("build.gradle.kts:")
}

/// A small Gradle project without dependencies, so its import needs no network. The
/// wrapper files come from the corpus, whose distribution is in the shared Gradle home.
/// Returns the build script whose configuration error the test adds and removes.
pub fn write_import_project(root: &Path, broken: bool) -> PathBuf {
    let wrapper_source = corpus().join("src/petclinic/gradle/wrapper");
    let wrapper = root.join("gradle/wrapper");
    fs::create_dir_all(&wrapper).unwrap();
    for name in ["gradle-wrapper.jar", "gradle-wrapper.properties"] {
        fs::copy(wrapper_source.join(name), wrapper.join(name)).unwrap();
    }
    fs::create_dir_all(root.join("src/main/java/probe")).unwrap();
    fs::write(
        root.join("settings.gradle"),
        "rootProject.name = 'import-failure-probe'\n",
    )
    .unwrap();
    fs::write(
        root.join("src/main/java/probe/App.java"),
        "package probe;\n\npublic class App {\n}\n",
    )
    .unwrap();
    write_build_script(root, broken)
}

/// Rewrites the build script of [`write_import_project`] with or without the
/// configuration error that fails the import without network.
pub fn write_build_script(root: &Path, broken: bool) -> PathBuf {
    let text = if broken {
        "plugins {\n    id 'java'\n}\n\ntasks.named('noSuchTask')\n"
    } else {
        "plugins {\n    id 'java'\n}\n"
    };
    let path = root.join("build.gradle");
    fs::write(&path, text).unwrap();
    path
}

/// The status line of the server state, from the text the window receives.
/// Writes one source of a probe package and returns its path.
pub fn write_probe(root: &Path, package: &str, class: &str, text: &str) -> PathBuf {
    let folder = root
        .join("src")
        .join("main")
        .join("java")
        .join(package.replace('.', "/"));
    let _ = fs::create_dir_all(&folder);
    let path = folder.join(format!("{class}.java"));
    fs::write(&path, text).expect("the probe is written");
    path
}

pub fn language_for(path: &Path) -> Option<LanguageConfig> {
    java_lang::language_for(path)
}

pub fn templates_for(path: &Path) -> Option<&'static TemplateLanguage> {
    java_lang::templates_for(path)
}

pub fn symbols_for(path: &Path) -> Option<&'static dyn SymbolProvider> {
    java_lang::symbols_for(path)
}

pub fn test_sites(source: &str, path: &Path) -> Vec<TestSite> {
    java_lang::test_sites(source, path)
}

/// Folders the project scan skips, as the application configures them.
pub fn skipped_folders() -> Vec<String> {
    ["target", "build", "out", ".gradle"]
        .iter()
        .map(|name| (*name).to_owned())
        .collect()
}

pub fn search_excludes(root: &Path) -> Vec<PathBuf> {
    ["target", "build", "out", ".gradle"]
        .iter()
        .map(|name| root.join(name))
        .collect()
}

/// A class whose return type is changed by the test to an error and back.
pub fn write_error_sample(root: &Path) -> PathBuf {
    write_source(
        root,
        "probe/WindowCheck.java",
        r#"package probe;

public class WindowCheck {

    int value() {
        return 1;
    }
}
"#,
    )
}

/// A class with a method used from another file, an unused import and a call for the
/// completion test.
pub fn write_refactor_samples(root: &Path) -> (PathBuf, PathBuf) {
    let sample = write_source(
        root,
        "probe/Sample.java",
        r#"package probe;

import java.util.List;

public class Sample {

    int add(int a, int b) {
        int sum = a + b;
        return sum;
    }

    int use() {
        return add(1, 2);
    }
}
"#,
    );
    let other = write_source(
        root,
        "probe/Other.java",
        r#"package probe;

public class Other {

    int call(Sample sample) {
        return sample.add(3, 4);
    }
}
"#,
    );
    (sample, other)
}

pub fn write_completion_sample(root: &Path) -> PathBuf {
    write_source(
        root,
        "probe/Complete.java",
        r#"package probe;

public class Complete {

    void show() {
        
    }
}
"#,
    )
}

/// A class with calls of a method whose parameters have names of a literal and of a
/// variable; the real server names them before its arguments.
pub fn write_inlay_hints_sample(root: &Path) -> PathBuf {
    write_source(
        root,
        "probe/InlayCheck.java",
        r#"package probe;

public class InlayCheck {

    void call(boolean flag, int count) {
    }

    void use(boolean value) {
        call(true, 5);
        call(value, 5);
    }
}
"#,
    )
}

/// A class with a method used twice and an interface with one implementation: the
/// real server counts them above the declarations. Returns the class and the interface.
pub fn write_code_vision_sample(root: &Path) -> (PathBuf, PathBuf) {
    let class = write_source(
        root,
        "probe/CodeVision.java",
        r#"package probe;

public class CodeVision {

    void used() {
    }

    void use() {
        used();
        used();
    }
}
"#,
    );
    let interface = write_source(
        root,
        "probe/Shape.java",
        r#"package probe;

public interface Shape {

    void area();
}

class Circle implements Shape {

    public void area() {
    }
}
"#,
    );
    (class, interface)
}

pub fn write_extract_sample(root: &Path) -> PathBuf {
    write_source(
        root,
        "probe/Extract.java",
        r#"package probe;

public class Extract {

    int total(int a, int b) {
        int sum = a + b;
        return sum;
    }
}
"#,
    )
}

/// A class that calls the string formatter of the platform; the documentation of the
/// call names the parameters, for the quick documentation test.
pub fn write_documentation_sample(root: &Path) -> PathBuf {
    write_source(
        root,
        "probe/DocSample.java",
        r#"package probe;

public class DocSample {

    String run(String value) {
        return String.format("%s", value);
    }
}
"#,
    )
}

pub fn write_format_sample(root: &Path) -> PathBuf {
    write_source(
        root,
        "probe/Messy.java",
        "package probe;\npublic class Messy {\npublic int value(){return  1;}\n}\n",
    )
}

/// A source the server reports an error for even when no editor holds it.
pub fn write_broken_sample(root: &Path) -> PathBuf {
    write_source(
        root,
        "probe/Broken.java",
        "package probe;\n\nclass Broken {\n    int value = ;\n}\n",
    )
}

/// A source with two methods whose formatting is easy to tell apart, for the
/// only-changed-lines mode.
pub fn write_vcs_format_sample(root: &Path) -> PathBuf {
    write_source(
        root,
        "probe/VcsFormat.java",
        "package probe;\n\npublic class VcsFormat {\n    public int first() {\n        return  1;\n    }\n\n    public int second(){return  2;}\n}\n",
    )
}

/// A class to move to the `probe.dest` package, which exists before the test.
pub fn write_move_samples(root: &Path) -> (PathBuf, PathBuf) {
    let movable = write_source(
        root,
        "probe/Movable.java",
        r#"package probe;

public class Movable {

    int value() {
        return 1;
    }
}
"#,
    );
    write_source(
        root,
        "probe/dest/Dest.java",
        "package probe.dest;\n\npublic class Dest {\n}\n",
    );
    (movable, root.join("src/main/java/probe/dest/Movable.java"))
}

/// A class of the main sources by its package and simple name.
pub fn main_class(root: &Path, package: &str, class: &str) -> PathBuf {
    let mut path = root.join("src/main/java");
    for part in package.split('.') {
        path.push(part);
    }
    path.join(format!("{class}.java"))
}

fn write_source(root: &Path, relative: &str, text: &str) -> PathBuf {
    let path = root.join("src/main/java").join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(&path, text).unwrap();
    path
}

/// The sample keeps an import that nothing uses.
pub fn has_unused_import(text: &str) -> bool {
    text.contains("import java.util.List;")
}

/// The import of the sample is gone after Optimize Imports.
pub fn unused_import_is_gone(text: &str) -> bool {
    !has_unused_import(text)
}

/// The process ids whose command line contains `marker`.
pub fn processes_with(marker: &str) -> Vec<u32> {
    let script = format!(
        "Get-CimInstance Win32_Process -Filter \"Name='java.exe'\" | Where-Object {{ \
         $_.CommandLine -and $_.CommandLine.Contains('{marker}') }} | \
         ForEach-Object {{ $_.ProcessId }}"
    );
    let output = Command::new("powershell")
        .args(["-NoProfile", "-Command", &script])
        .output()
        .expect("powershell runs");
    String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .filter_map(|pid| pid.parse().ok())
        .collect()
}

pub fn is_process_running(process_id: u32) -> bool {
    let output = Command::new("tasklist")
        .args(["/FI", &format!("PID eq {process_id}"), "/NH"])
        .output()
        .expect("tasklist runs");
    String::from_utf8_lossy(&output.stdout).contains(&process_id.to_string())
}

/// The corpus folder, or the `corpus/` of the checkout.
pub fn corpus_dir() -> PathBuf {
    corpus()
}

/// Copies PetClinic and keeps only its Maven build, so one tool imports the project.
pub fn prepare_project(from: &Path, to: &Path) {
    copy_tree(from, to, false);
    for name in ["build.gradle", "settings.gradle", "gradlew", "gradlew.bat"] {
        let _ = fs::remove_file(to.join(name));
    }
    let _ = fs::remove_dir_all(to.join("gradle"));
}

/// A jar with the configuration metadata of a Spring Boot library, for the window tests
/// of the neutral crates: the file name and its folder are the caller's.
pub fn write_spring_metadata_jar(folder: &Path) -> PathBuf {
    const METADATA: &str = r#"{
        "groups": [
            {"name": "spring.jpa", "type": "com.example.JpaProperties", "sourceType": "com.example.JpaProperties"},
            {"name": "spring.jpa.hibernate", "type": "com.example.HibernateSettings", "sourceType": "com.example.HibernateSettings"},
            {"name": "logging.level", "type": "java.util.Map<java.lang.String,java.lang.String>"}
        ],
        "properties": [
            {
                "name": "spring.jpa.open-in-view",
                "type": "java.lang.Boolean",
                "defaultValue": true,
                "description": "Whether to enable the open entity manager in view interceptor.",
                "sourceType": "com.example.JpaProperties"
            },
            {
                "name": "spring.jpa.hibernate.ddl-auto",
                "type": "com.example.DdlAuto",
                "description": "What to do with the schema at start up.",
                "sourceType": "com.example.HibernateSettings"
            },
            {
                "name": "spring.jpa.database-platform",
                "type": "java.lang.String",
                "description": "The name of the target database.",
                "sourceType": "com.example.JpaProperties"
            },
            {
                "name": "spring.jpa.properties",
                "type": "java.util.Map<java.lang.String,java.lang.String>",
                "description": "Extra properties of the persistence unit."
            },
            {
                "name": "spring.jpa.show-sql",
                "type": "boolean",
                "defaultValue": false,
                "description": "Whether to log the statements."
            },
            {
                "name": "spring.jpa.old-key",
                "type": "java.lang.String",
                "deprecation": {
                    "replacement": "spring.jpa.open-in-view",
                    "reason": "The key was renamed."
                }
            },
            {
                "name": "spring.jpa.encoding",
                "type": "java.nio.charset.Charset",
                "description": "The encoding of the stored text."
            }
        ],
        "hints": [
            {
                "name": "spring.jpa.hibernate.ddl-auto",
                "values": [
                    {"value": "none", "description": "Do nothing."},
                    {"value": "update", "description": "Update the schema."},
                    {"value": "validate", "description": "Only check the schema."},
                    {"value": "create", "description": "Create the schema."},
                    {"value": "create-drop", "description": "Create it and drop it at the end."}
                ]
            }
        ]
    }"#;
    const ADDITIONAL: &str = r#"{
        "properties": [
            {
                "name": "spring.jpa.timeout",
                "type": "java.time.Duration",
                "defaultValue": "30s",
                "description": "How long a statement may take."
            }
        ]
    }"#;
    let file = folder.join("spring-metadata.jar");
    let mut writer = zip::ZipWriter::new(fs::File::create(&file).expect("the jar of the test"));
    for (name, text) in [
        ("META-INF/spring-configuration-metadata.json", METADATA),
        (
            "META-INF/additional-spring-configuration-metadata.json",
            ADDITIONAL,
        ),
    ] {
        use std::io::Write as _;
        writer
            .start_file(
                name,
                zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Stored),
            )
            .expect("an entry of the jar");
        writer
            .write_all(text.as_bytes())
            .expect("the text of the jar");
    }
    writer.finish().expect("the finished jar");
    file
}

/// The corpus folder, or the `corpus/` of the checkout.
fn corpus() -> PathBuf {
    if let Some(folder) = std::env::var_os("IDE_CORPUS") {
        return PathBuf::from(folder);
    }
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .ancestors()
        .map(|folder| folder.join("corpus"))
        .find(|folder| folder.join("src/petclinic").exists())
        .unwrap_or_else(|| panic!("set IDE_CORPUS to the folder that holds src/petclinic"))
}

fn jdk(corpus: &Path, version: &str) -> PathBuf {
    fs::canonicalize(corpus.join("jdk").join(version)).unwrap_or_else(|_| {
        panic!(
            "the JDK {version} is missing under {}",
            corpus.join("jdk").display()
        )
    })
}

/// A Gradle user home that the environment prepared next to the Maven one.
fn gradle_home() -> Option<PathBuf> {
    if let Ok(home) = std::env::var("GRADLE_USER_HOME") {
        return Some(PathBuf::from(home));
    }
    let maven = PathBuf::from(std::env::var("MAVEN_USER_HOME").ok()?);
    let gradle = maven.parent()?.join("gradle");
    gradle.is_dir().then_some(gradle)
}

fn write_maven_settings(state_dir: &Path) -> Result<PathBuf, String> {
    let m2 = PathBuf::from(
        std::env::var("MAVEN_USER_HOME")
            .map_err(|_| "set MAVEN_USER_HOME to the Maven cache".to_owned())?,
    );
    let repository = m2
        .join("repository")
        .display()
        .to_string()
        .replace('\\', "/");
    let path = state_dir.join("maven-settings.xml");
    let xml = format!(
        "<settings><localRepository>{repository}</localRepository><offline>true</offline></settings>"
    );
    fs::create_dir_all(state_dir).map_err(|error| error.to_string())?;
    fs::write(&path, xml).map_err(|error| error.to_string())?;
    Ok(path)
}

/// The project copy builds with the same cache and offline as the server imports it.
fn write_maven_config(root: &Path, settings: &Path) {
    let folder = root.join(".mvn");
    let _ = fs::create_dir_all(&folder);
    let repo = PathBuf::from(std::env::var("MAVEN_USER_HOME").unwrap_or_default())
        .join("repository")
        .display()
        .to_string();
    let text = format!(
        "-o\n-s\n{}\n-Dmaven.repo.local={repo}\n-Dspring-javaformat.skip=true\n-Dcheckstyle.skip=true\n",
        settings.display()
    );
    let _ = fs::write(folder.join("maven.config"), text);
}

/// Copies a project without its build outputs, so the corpus stays untouched.
pub fn copy_project(from: &Path, to: &Path) {
    copy_tree(from, to, false);
}

fn copy_tree(from: &Path, to: &Path, inside_sources: bool) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        let text = name.to_string_lossy();
        if !inside_sources && SKIPPED.contains(&text.as_ref()) {
            continue;
        }
        if entry.file_type().unwrap().is_dir() {
            copy_tree(
                &entry.path(),
                &to.join(&name),
                inside_sources || text == "src",
            );
        } else {
            fs::copy(entry.path(), to.join(&name)).unwrap();
        }
    }
}

struct StaticTool(ProjectModel);

impl BuildSystem for StaticTool {
    fn detect(&self, _: &Path) -> bool {
        true
    }

    fn scan(&self, _: &Path) -> base::Result<ProjectModel> {
        Ok(self.0.clone())
    }

    fn sync(&self, _: &Path, _: &Cancel) -> base::Result<ProjectModel> {
        Ok(self.0.clone())
    }

    fn assemble(&self, _: &Path) -> base::Result<ProcessSpec> {
        Ok(ProcessSpec::new("cmd").args(["/d", "/c", "exit", "0"]))
    }
}

struct ProbeRun {
    root: PathBuf,
    java: PathBuf,
    classes: PathBuf,
}

impl ProbeRun {
    fn program(&self, debug: bool) -> ProcessSpec {
        let mut spec = ProcessSpec::new(&self.java).cwd(&self.root);
        if debug {
            spec = spec.arg(java_exec::DEBUG_AGENT);
        }
        spec.arg("-cp").arg(&self.classes).arg("probe.DebugProbe")
    }
}

impl RunConfigProvider for ProbeRun {
    fn id(&self) -> &'static str {
        "probe"
    }

    fn discover(&self, _: &ProjectModel) -> base::Result<Vec<RunConfig>> {
        Ok(vec![RunConfig {
            name: "DebugProbe".into(),
            provider: "probe".into(),
            assemble_first: false,
            settings: Default::default(),
        }])
    }

    fn plan(&self, _: &RunConfig, _: &ProjectModel) -> base::Result<RunPlan> {
        Ok(RunPlan {
            before: None,
            program: self.program(false),
            grace: Duration::from_secs(1),
        })
    }

    fn debug_plan(&self, _: &RunConfig, _: &ProjectModel) -> base::Result<RunPlan> {
        Ok(RunPlan {
            before: None,
            program: self.program(true),
            grace: Duration::from_secs(1),
        })
    }
}

/// One configuration that starts the flow probe under the debugger.
struct FlowRun {
    program: ProcessSpec,
}

impl RunConfigProvider for FlowRun {
    fn id(&self) -> &'static str {
        "flow"
    }

    fn discover(&self, _: &ProjectModel) -> base::Result<Vec<RunConfig>> {
        Ok(vec![RunConfig {
            name: "DebugProbe".into(),
            provider: "flow".into(),
            assemble_first: false,
            settings: Default::default(),
        }])
    }

    fn plan(&self, _: &RunConfig, _: &ProjectModel) -> base::Result<RunPlan> {
        Ok(RunPlan {
            before: None,
            program: self.program.clone(),
            grace: Duration::from_secs(1),
        })
    }

    fn debug_plan(&self, _: &RunConfig, _: &ProjectModel) -> base::Result<RunPlan> {
        Ok(RunPlan {
            before: None,
            program: self.program.clone(),
            grace: Duration::from_secs(1),
        })
    }
}

/// The line of `int sum = a + b;`, from one.
const BREAK_LINE: u32 = 20;

/// The line of `static int counter;` in [`WATCH_PROBE`], from one.
const WATCH_FIELD_LINE: u32 = 5;

/// The line of `int stop = 1;` in [`WATCH_PROBE`], from one.
const WATCH_BREAK_LINE: u32 = 10;

/// The line of `counter = stop;` in [`WATCH_PROBE`], from one.
const WATCH_WRITE_LINE: u32 = 11;

/// The line of `holder.value = stop + 1;` in [`WATCH_PROBE`], from one.
const WATCH_OBJECT_WRITE_LINE: u32 = 12;

/// The probe of the field watchpoints: a static field, and a local object with a field of
/// its own. The normal breakpoint stops before both writes.
const WATCH_PROBE: &str = r#"package probe;

public class DebugProbe {

    static int counter;

    public static void main(String[] args) {
        System.out.println("watch-start");
        Holder holder = new Holder();
        int stop = 1;
        counter = stop;
        holder.value = stop + 1;
        System.out.println("counter=" + counter + " value=" + holder.value);
    }

    static class Holder {
        int value;
    }
}
"#;

/// The line of `int sum = add(total, 1);` in [`FLOW_PROBE`], from one.
const FLOW_BREAK_LINE: u32 = 13;

/// The probe of the whole debug session: a local `int` and an object with children, a
/// call, and output on both sides of the breakpoint.
const FLOW_PROBE: &str = r#"package probe;

import java.util.ArrayList;

public class DebugProbe {

    public static void main(String[] args) {
        System.out.println("probe-stdout");
        int total = 0;
        ArrayList<String> items = new ArrayList<>();
        items.add("alpha");
        items.add("beta");
        int sum = add(total, 1);
        System.out.println("total=" + total);
        System.out.println("items=" + items.size());
    }

    static int add(int a, int b) {
        int sum = a + b;
        return sum;
    }
}
"#;

/// The line of `int total = add(1, 2);` in [`VIRTUAL_PROBE`], from one.
const VIRTUAL_BREAK_LINE: u32 = 25;

const VIRTUAL_PROBE: &str = r#"package probe;

import java.lang.reflect.Method;
import java.util.concurrent.CountDownLatch;

public class DebugProbe {

    public static void main(String[] args) throws Exception {
        CountDownLatch ready = new CountDownLatch({count});
        CountDownLatch release = new CountDownLatch(1);
        Method startVirtualThread =
                Thread.class.getMethod("startVirtualThread", Runnable.class);
        for (int i = 0; i < {count}; i++) {
            Runnable task = () -> {
                ready.countDown();
                try {
                    release.await();
                } catch (InterruptedException ignored) {
                }
            };
            startVirtualThread.invoke(null, task);
        }
        ready.await();
        Thread.sleep(10000);
        int total = add(1, 2);
        release.countDown();
        System.out.println("total=" + total);
    }

    static int add(int a, int b) {
        int sum = a + b;
        return sum;
    }
}
"#;

/// A project with one dependency whose sources are not in the repository, a copy of the
/// local repository that holds the jar, and a loopback server that has its sources. The
/// cache of the fixture is temporary: `MAVEN_USER_HOME` points at it until the fixture
/// is dropped.
pub struct SourceFixture {
    pub root: PathBuf,
    pub main: PathBuf,
    /// The address of the loopback repository, for the project that names it.
    pub repository: String,
    home: PathBuf,
    previous: Option<std::ffi::OsString>,
}

const LIBRARY_SOURCES: &str = r#"package com.example;

/** SOURCE-MARKER */
public class Lib {
    public int answer() { return 42; }
}
"#;

impl SourceFixture {
    pub fn new(name: &str) -> Self {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let base = test_support::scratch(&format!("window-language-{name}"));
        let root = base.join("app");
        let home = base.join("home");
        let library = base.join("library");
        let classes = library.join("classes");
        let source_dir = library.join("src");
        fs::create_dir_all(source_dir.join("com/example")).unwrap();
        fs::create_dir_all(&classes).unwrap();
        let real_home = PathBuf::from(
            std::env::var("MAVEN_USER_HOME").expect("MAVEN_USER_HOME names the cache"),
        );
        let repository = home.join("repository");
        mirror_repository(&real_home.join("repository"), &repository);
        fs::create_dir_all(repository.join("com/example/lib/1.0")).unwrap();

        let jdk = jdk(&corpus(), "17");
        fs::write(source_dir.join("com/example/Lib.java"), LIBRARY_SOURCES).unwrap();
        let status = Command::new(jdk.join("bin/javac.exe"))
            .arg("-d")
            .arg(&classes)
            .arg(source_dir.join("com/example/Lib.java"))
            .status()
            .expect("javac runs");
        assert!(status.success(), "the fixture library compiles");
        let status = Command::new(jdk.join("bin/jar.exe"))
            .args(["--create", "--file"])
            .arg(repository.join("com/example/lib/1.0/lib-1.0.jar"))
            .arg("-C")
            .arg(&classes)
            .arg(".")
            .status()
            .expect("jar runs");
        assert!(status.success(), "the fixture library jar is built");
        let status = Command::new(jdk.join("bin/jar.exe"))
            .args(["--create", "--file"])
            .arg(library.join("lib-1.0-sources.jar"))
            .arg("-C")
            .arg(&source_dir)
            .arg(".")
            .status()
            .expect("jar runs");
        assert!(status.success(), "the fixture sources jar is built");
        fs::write(
            repository.join("com/example/lib/1.0/lib-1.0.pom"),
            "<project><modelVersion>4.0.0</modelVersion><groupId>com.example</groupId>\
             <artifactId>lib</artifactId><version>1.0</version></project>",
        )
        .unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let port = listener.local_addr().unwrap().port();
        let served = fs::read(library.join("lib-1.0-sources.jar")).unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else {
                    continue;
                };
                let mut request = [0u8; 2048];
                let read = stream.read(&mut request).unwrap_or(0);
                let request = String::from_utf8_lossy(&request[..read]).into_owned();
                let found = request.contains("lib-1.0-sources.jar");
                let body: &[u8] = if found { &served } else { b"not here" };
                let status = if found { "200 OK" } else { "404 Not Found" };
                let head = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(body);
            }
        });

        fs::create_dir_all(root.join("src/main/java/com/example")).unwrap();
        fs::write(
            root.join("pom.xml"),
            format!(
                "<project><modelVersion>4.0.0</modelVersion>\
                 <groupId>com.example</groupId><artifactId>app</artifactId>\
                 <version>1.0</version>\
                 <properties><maven.compiler.release>17</maven.compiler.release>\
                 <project.build.sourceEncoding>UTF-8</project.build.sourceEncoding></properties>\
                 <repositories><repository><id>loopback</id>\
                 <url>http://127.0.0.1:{port}</url></repository></repositories>\
                 <dependencies><dependency><groupId>com.example</groupId>\
                 <artifactId>lib</artifactId><version>1.0</version></dependency></dependencies>\
                 </project>"
            ),
        )
        .unwrap();
        let main = root.join("src/main/java/com/example/Main.java");
        fs::write(
            &main,
            "package com.example;\n\npublic class Main {\n    public int use() {\n        \
             return new Lib().answer();\n    }\n}\n",
        )
        .unwrap();

        let previous = std::env::var_os("MAVEN_USER_HOME");
        // SAFETY: the ignored test runs one at a time, and the value comes back on drop.
        unsafe { std::env::set_var("MAVEN_USER_HOME", &home) };
        Self {
            root,
            main,
            repository: format!("http://127.0.0.1:{port}"),
            home,
            previous,
        }
    }

    /// The repository the sources were written to.
    pub fn repository_dir(&self) -> PathBuf {
        self.home.join("repository")
    }
}

impl Drop for SourceFixture {
    fn drop(&mut self) {
        // SAFETY: as in `new`.
        unsafe {
            match self.previous.take() {
                Some(value) => std::env::set_var("MAVEN_USER_HOME", value),
                None => std::env::remove_var("MAVEN_USER_HOME"),
            }
        }
    }
}

/// A repository built from links to another one, so that a project import finds its
/// plugins while the fixture artifact stays in a copy of its own. Without links the
/// artifact would be written to the cache of the machine.
#[cfg(windows)]
fn mirror_repository(from: &Path, to: &Path) {
    let windows = |path: &Path| path.to_string_lossy().replace('/', "\\");
    let link = |target: &Path, at: &Path| {
        let status = Command::new("cmd")
            .args(["/d", "/c", "mklink", "/J"])
            .arg(windows(at))
            .arg(windows(target))
            .stdout(std::process::Stdio::null())
            .status()
            .expect("mklink runs");
        assert!(
            status.success(),
            "{} links to {}",
            at.display(),
            target.display()
        );
    };
    fs::create_dir_all(to).unwrap();
    let Ok(entries) = fs::read_dir(from) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name == "com" {
            let ours = to.join("com");
            fs::create_dir_all(&ours).unwrap();
            for child in fs::read_dir(entry.path()).into_iter().flatten().flatten() {
                if child.file_name() != "example" {
                    link(&child.path(), &ours.join(child.file_name()));
                }
            }
        } else {
            link(&entry.path(), &to.join(name));
        }
    }
}

#[cfg(not(windows))]
fn mirror_repository(_: &Path, _: &Path) {}

const PROBE: &str = r#"package probe;

public class DebugProbe {

    public static void main(String[] args) {
        System.out.println("probe-stdout");
        System.err.println("probe-stderr");
        int total = 0;
        for (int i = 0; i < 3; i++) {
            total = add(total, i);
        }
        System.out.println("total=" + total);
        try {
            Thread.sleep(2000);
        } catch (InterruptedException ignored) {
        }
    }

    static int add(int a, int b) {
        int sum = a + b;
        return sum;
    }
}
"#;
