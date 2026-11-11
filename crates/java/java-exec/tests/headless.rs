//! The whole way from a project on disk to a stopped program, without a window. Needs JDKs,
//! the corpus, the helper jar and network or filled caches, so these do not run in CI:
//!
//!   cargo xtask build-helpers --out DIR
//!   cargo build -p ide-ctrl
//!   IDE_HELPERS_DIR=DIR cargo test -p java-exec --test headless -- --ignored --test-threads=1
//!
//! `E2E_MAVEN_REPO` moves the Maven repository, `IDE_CORPUS` points to the corpus.

use std::collections::HashSet;
use std::env;
use std::fs;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use java_exec::{JavaProvider, JavaTarget, PROVIDER_ID};
use java_project::{BuildEnv, Gradle, Jdk, Maven};
use project_model::{BuildSystem, MAIN_UNIT, Module, ProjectModel, SyncEvent, Unit, open_project};
use runner::{Console, ConsoleEvent, RunConfig, RunConfigProvider, RunPlan, Step};
use workspace_core::Delivery;

const SYNC_PATIENCE: Duration = Duration::from_secs(600);
const RUN_PATIENCE: Duration = Duration::from_secs(420);
const STOP_PATIENCE: Duration = Duration::from_secs(30);
const MAIN_CLASS: &str = "org.springframework.samples.petclinic.PetClinicApplication";
const STARTED: &str = "Started PetClinicApplication";
const PORT_MARK: &str = "started on port ";
/// Printed by the connection pool from a shutdown hook, so only a graceful stop has it.
const HOOK_RAN: &str = "Shutdown completed";
/// The exit code [`proc_sup::Process::stop`] leaves, a program that was killed.
const FORCED: i32 = 1;

fn corpus() -> PathBuf {
    env::var_os("IDE_CORPUS").map_or_else(
        || PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../corpus")),
        PathBuf::from,
    )
}

fn target_dir() -> PathBuf {
    let exe = env::current_exe().unwrap();
    exe.ancestors().nth(3).unwrap().to_owned()
}

fn build_env(name: &str) -> BuildEnv {
    let exe = env::current_exe().unwrap();
    let helper = exe.parent().unwrap().with_file_name("ide-ctrl.exe");
    assert!(helper.is_file(), "run cargo build -p ide-ctrl first");
    let maven_args = env::var("E2E_MAVEN_REPO")
        .map(|repo| vec![format!("-Dmaven.repo.local={repo}")])
        .unwrap_or_default();
    BuildEnv {
        maven_args,
        cache_root: Some(work_dir(name).join("cache")),
        ctrl_helper: Some(helper),
        ..BuildEnv::default()
    }
}

fn work_dir(name: &str) -> PathBuf {
    target_dir().join("e2e").join(name)
}

/// A clean copy: the corpus stays as it is, and old outputs must not hide a broken build.
fn copy_project(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        let skip = [".git", "target", "build", ".gradle"].map(std::ffi::OsString::from);
        if skip.contains(&name) {
            continue;
        }
        let target = to.join(&name);
        if entry.file_type().unwrap().is_dir() {
            copy_project(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn petclinic_copy(name: &str) -> PathBuf {
    let root = work_dir(name).join("petclinic");
    let _ = fs::remove_dir_all(&root);
    copy_project(&corpus().join("src/petclinic"), &root);
    root
}

fn sync(tool: Arc<dyn BuildSystem>, root: &Path, env: &BuildEnv) -> Arc<ProjectModel> {
    let (session, mut events) = open_project(tool, root.to_owned(), &env.cache_root());
    let start = Instant::now();
    loop {
        match events.try_next() {
            Some(Delivery::Event(SyncEvent::Synced)) => break,
            Some(Delivery::Event(SyncEvent::Failed(error))) => panic!("sync failed: {error}"),
            Some(_) => {}
            None => assert!(start.elapsed() < SYNC_PATIENCE, "the sync took too long"),
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    session.model().unwrap()
}

fn petclinic_config(provider: &JavaProvider, model: &ProjectModel) -> RunConfig {
    let configs = provider.discover(model).unwrap();
    let mut config = configs
        .into_iter()
        .find(|config| config.settings["main_class"].as_str() == Some(MAIN_CLASS))
        .expect("the main class is found in the sources");
    let mut target: JavaTarget = config.settings.clone().try_into().unwrap();
    assert!(target.spring_boot);
    target.program_args = vec!["--server.port=0".into()];
    config.settings = toml::Table::try_from(target).unwrap();
    config
}

/// Reads until `done` is true of everything read so far, or the plan ends.
fn read_until(console: &Console, log: &mut String, done: impl Fn(&str) -> bool) -> bool {
    let start = Instant::now();
    while !done(log) {
        assert!(
            start.elapsed() < RUN_PATIENCE,
            "not in time:\n{}",
            tail(log)
        );
        match console.events().recv_timeout(RUN_PATIENCE) {
            Ok(ConsoleEvent::Output { text, .. }) => log.push_str(&text),
            Ok(ConsoleEvent::Failed { message }) => panic!("{message}"),
            Ok(ConsoleEvent::Started { .. }) => {}
            Ok(ConsoleEvent::Exited { code, step }) => {
                log.push_str(&format!("\n[exited {code:?} at {step:?}]\n"));
                return false;
            }
            Ok(ConsoleEvent::Closed) => {}
            Err(error) => panic!("{error}:\n{}", tail(log)),
        }
    }
    true
}

fn tail(log: &str) -> &str {
    let mut start = log.len().saturating_sub(4000);
    while !log.is_char_boundary(start) {
        start += 1;
    }
    &log[start..]
}

fn exit_code(console: &Console, log: &mut String) -> Option<i32> {
    let start = Instant::now();
    loop {
        assert!(start.elapsed() < STOP_PATIENCE, "no exit:\n{}", tail(log));
        match console.events().recv_timeout(STOP_PATIENCE).unwrap() {
            ConsoleEvent::Output { text, .. } => log.push_str(&text),
            ConsoleEvent::Exited { code, step } => {
                assert_eq!(step, Step::Program);
                return code;
            }
            ConsoleEvent::Failed { message } => panic!("{message}"),
            ConsoleEvent::Started { .. } | ConsoleEvent::Closed => {}
        }
    }
}

fn port_of(log: &str) -> u16 {
    let (_, rest) = log
        .split_once(PORT_MARK)
        .expect("the server reports its port");
    let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap();
    rest[..end].parse().unwrap()
}

fn http_status(port: u16) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .write_all(b"GET / HTTP/1.0\r\nHost: localhost\r\n\r\n")
        .unwrap();
    let mut reply = Vec::new();
    stream.read_to_end(&mut reply).unwrap();
    let reply = String::from_utf8_lossy(&reply);
    reply.lines().next().unwrap_or("").to_owned()
}

/// A program that serves requests, then stops through its shutdown hooks.
fn run_and_stop(provider: &JavaProvider, model: &ProjectModel) {
    let config = petclinic_config(provider, model);
    let console = Console::start(provider.plan(&config, model).unwrap());
    let mut log = String::new();

    let started = read_until(&console, &mut log, |text| text.contains(STARTED));
    assert!(started, "{}", tail(&log));
    let port = port_of(&log);
    let status = http_status(port);
    assert!(status.contains("200"), "{status}");

    console.stop();
    let code = exit_code(&console, &mut log);

    assert!(log.contains(HOOK_RAN), "no shutdown hook:\n{}", tail(&log));
    assert_ne!(code, Some(FORCED), "killed, not stopped:\n{}", tail(&log));
    assert!(TcpStream::connect(("127.0.0.1", port)).is_err());
}

fn assert_full_model(model: &ProjectModel) {
    assert!(model.resolved);
    assert!(model.problems.is_empty(), "{:?}", model.problems);
    let module = &model.modules[0];
    let main = module.unit(MAIN_UNIT).unwrap();
    assert!(main.source_roots.iter().any(|root| root.is_dir()));
    assert!(
        main.runtime_paths.len() > 50,
        "{}",
        main.runtime_paths.len()
    );
}

#[test]
#[ignore = "needs JDKs, the corpus and the helper jar"]
fn petclinic_on_maven_builds_runs_and_stops() {
    let env = build_env("maven");
    let root = petclinic_copy("maven");
    fs::remove_file(root.join("build.gradle")).unwrap();
    let tool: Arc<dyn BuildSystem> = Arc::new(Maven::new(env.clone()));

    let model = sync(tool.clone(), &root, &env);

    assert_eq!(model.tool, "maven");
    assert_full_model(&model);
    run_and_stop(&JavaProvider::new(env, tool), &model);
}

fn gradle_servers() -> HashSet<u32> {
    let script = "Get-CimInstance Win32_Process -Filter \"Name='java.exe'\" | \
        Where-Object { $_.CommandLine -like '*GradleDaemon*' } | \
        ForEach-Object { $_.ProcessId }";
    let output = Command::new("powershell")
        .args(["-NoProfile", "-Command", script])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    text.split_whitespace()
        .filter_map(|pid| pid.parse().ok())
        .collect()
}

#[test]
#[ignore = "needs JDKs, the corpus and a Gradle cache"]
fn petclinic_on_gradle_builds_runs_and_stops_and_the_server_stays() {
    let env = build_env("gradle");
    let root = petclinic_copy("gradle");
    fs::remove_file(root.join("pom.xml")).unwrap();
    let tool: Arc<dyn BuildSystem> = Arc::new(Gradle::new(env.clone()));

    let model = sync(tool.clone(), &root, &env);

    assert_eq!(model.tool, "gradle");
    assert_full_model(&model);
    let provider = JavaProvider::new(env, tool.clone());
    run_and_stop(&provider, &model);
    let servers = gradle_servers();
    assert!(!servers.is_empty(), "no Gradle server after a build");

    // Stopping the client in the middle of a build and closing everything.
    let plan = RunPlan {
        before: None,
        program: tool.assemble(&root).unwrap(),
        grace: Duration::from_secs(5),
    };
    let console = Console::start(plan);
    std::thread::sleep(Duration::from_secs(1));
    drop(console);
    drop(provider);

    let left = gradle_servers();
    assert!(
        servers.is_subset(&left),
        "before {servers:?}, after {left:?}"
    );
}

fn hello_model(root: &Path, output: &Path) -> ProjectModel {
    ProjectModel {
        root: root.to_owned(),
        resolved: true,
        modules: vec![Module {
            id: "hello".into(),
            dir: root.to_owned(),
            units: vec![Unit {
                name: MAIN_UNIT.into(),
                output_dirs: vec![output.to_owned()],
                ..Unit::default()
            }],
            ..Module::default()
        }],
        ..ProjectModel::default()
    }
}

struct NoTool;

impl BuildSystem for NoTool {
    fn detect(&self, _root: &Path) -> bool {
        false
    }

    fn scan(&self, _root: &Path) -> base::Result<ProjectModel> {
        Err("not used".into())
    }

    fn sync(&self, _root: &Path, _cancel: &base::Cancel) -> base::Result<ProjectModel> {
        Err("not used".into())
    }

    fn assemble(&self, _root: &Path) -> base::Result<proc_sup::ProcessSpec> {
        Err("not used".into())
    }
}

/// Stdout and stderr, every JDK of the table, and a path the classpath jar must encode.
#[test]
#[ignore = "needs the JDKs of the corpus"]
fn cyrillic_output_is_right_on_every_jdk() {
    const TEXT: &str = "Привет, мир";
    let source = format!(
        "public class Hello {{ public static void main(String[] args) {{
            System.out.println(\"out {TEXT}\");
            System.err.println(\"err {TEXT}\");
        }} }}"
    );
    for version in ["8", "17", "21", "25"] {
        let home = fs::canonicalize(corpus().join("jdk").join(version)).unwrap();
        let home = home
            .to_string_lossy()
            .trim_start_matches(r"\\?\")
            .to_owned();
        let jdk = Jdk::at(Path::new(&home)).unwrap_or_else(|| panic!("no JDK {version}"));
        let root = work_dir("cyrillic").join(format!("проект с пробелом {version}"));
        let output = root.join("классы");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&output).unwrap();
        // javac of JDK 8 cannot take such a path as an argument, the jar is what must cope.
        let compiled = work_dir("cyrillic").join(format!("compiled-{version}"));
        fs::create_dir_all(&compiled).unwrap();
        let file = compiled.join("Hello.java");
        fs::write(&file, &source).unwrap();
        let status = Command::new(jdk.home.join("bin/javac.exe"))
            .args(["-encoding", "UTF-8", "-d"])
            .arg(&compiled)
            .arg(&file)
            .status()
            .unwrap();
        assert!(status.success());
        fs::copy(compiled.join("Hello.class"), output.join("Hello.class")).unwrap();

        let env = BuildEnv {
            jdk: Some(jdk),
            ..build_env("cyrillic")
        };
        let provider = JavaProvider::new(env, Arc::new(NoTool));
        let target = JavaTarget {
            module: "hello".into(),
            main_class: "Hello".into(),
            ..JavaTarget::default()
        };
        let config = RunConfig {
            name: "Hello".into(),
            provider: PROVIDER_ID.into(),
            assemble_first: false,
            settings: toml::Table::try_from(target).unwrap(),
        };
        let model = hello_model(&root, &output);
        let console = Console::start(provider.plan(&config, &model).unwrap());
        let mut log = String::new();

        let code = exit_code(&console, &mut log);

        assert_eq!(code, Some(0), "JDK {version}: {log}");
        assert!(log.contains(&format!("out {TEXT}")), "JDK {version}: {log}");
        assert!(log.contains(&format!("err {TEXT}")), "JDK {version}: {log}");
    }
}
