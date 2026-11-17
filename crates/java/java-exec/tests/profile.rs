//! A profile run on the JDK of the corpus: the JVM records samples, `jfr print` turns the
//! recording into JSON, and the parser finds the method of the program in the tree.
//! Needs JDK 21 and the corpus, so this does not run in CI:
//!
//!   IDE_CORPUS=C:/path/to/corpus cargo test -p java-exec --test profile -- --ignored --test-threads=1

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use base::Cancel;
use java_exec::JavaProvider;
use java_project::{BuildEnv, Jdk};
use project_model::{BuildSystem, MAIN_UNIT, Module, ProjectModel, Unit};
use runner::{Console, ConsoleEvent, RunConfigProvider};

const PATIENCE: Duration = Duration::from_secs(120);

const MAIN: &str = "package demo;

public class Main {
    static long work(long n) {
        long sum = 0;
        for (long i = 0; i < n; i++) {
            sum += (i * 31) % 7;
        }
        return sum;
    }

    public static void main(String[] args) {
        long end = System.nanoTime() + 600_000_000L;
        long total = 0;
        while (System.nanoTime() < end) {
            total += work(3000);
        }
        System.out.println(total);
    }
}
";

fn corpus() -> PathBuf {
    env::var_os("IDE_CORPUS").map_or_else(
        || PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../corpus")),
        PathBuf::from,
    )
}

fn javac(jdk: &Jdk) -> PathBuf {
    let name = if cfg!(windows) { "javac.exe" } else { "javac" };
    jdk.home.join("bin").join(name)
}

/// A project of one class compiled into `out`, with no build tool behind it.
fn project(root: &Path) -> ProjectModel {
    fs::create_dir_all(root.join("src/demo")).unwrap();
    fs::write(root.join("src/demo/Main.java"), MAIN).unwrap();
    ProjectModel {
        root: root.to_owned(),
        resolved: true,
        tool: "manual".into(),
        modules: vec![Module {
            id: "app".into(),
            dir: root.to_owned(),
            units: vec![Unit {
                name: MAIN_UNIT.into(),
                source_roots: vec![root.join("src")],
                output_dirs: vec![root.join("out")],
                ..Unit::default()
            }],
            ..Module::default()
        }],
        ..ProjectModel::default()
    }
}

/// Nothing builds this project: the test compiles it before the run.
struct NoTool;

impl BuildSystem for NoTool {
    fn detect(&self, _root: &Path) -> bool {
        true
    }

    fn scan(&self, _root: &Path) -> base::Result<ProjectModel> {
        Err("not used".into())
    }

    fn sync(&self, _root: &Path, _cancel: &Cancel) -> base::Result<ProjectModel> {
        Err("not used".into())
    }

    fn assemble(&self, _root: &Path) -> base::Result<proc_sup::ProcessSpec> {
        Err("not used".into())
    }

    fn assemble_if_needed(&self, _root: &Path) -> base::Result<Option<proc_sup::ProcessSpec>> {
        Ok(None)
    }
}

fn arguments(spec: &proc_sup::ProcessSpec) -> Vec<String> {
    spec.arguments()
        .iter()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect()
}

fn run_to_end(plan: runner::RunPlan) {
    let console = Console::start(plan);
    let started = Instant::now();
    loop {
        assert!(started.elapsed() < PATIENCE, "the program took too long");
        match console.events().recv_timeout(Duration::from_millis(100)) {
            Ok(ConsoleEvent::Exited { code, .. }) => {
                assert_eq!(code, Some(0), "the program ended with a failure");
                return;
            }
            Ok(ConsoleEvent::Closed) => return,
            Ok(ConsoleEvent::Failed { message }) => panic!("{message}"),
            Ok(_) | Err(_) => {}
        }
    }
}

#[test]
#[ignore = "needs JDK 21 and the corpus"]
fn a_recorded_run_names_the_method_of_the_program_in_the_tree() {
    let jdk = Jdk::at(&corpus().join("jdk/21")).expect("JDK 21 of the corpus");
    let folder = test_support::TempDir::new("java-profile");
    let root = folder.path();
    let model = project(root);
    let out = root.join("out");
    fs::create_dir_all(&out).unwrap();
    let status = Command::new(javac(&jdk))
        .arg("--release")
        .arg("21")
        .arg("-d")
        .arg(&out)
        .arg(root.join("src/demo/Main.java"))
        .status()
        .unwrap();
    assert!(status.success(), "javac compiled the program");

    let env = BuildEnv {
        jdk: Some(jdk),
        cache_root: Some(root.join("cache")),
        ..BuildEnv::default()
    };
    let provider = JavaProvider::new(env, Arc::new(NoTool));
    let config = provider
        .discover(&model)
        .unwrap()
        .into_iter()
        .find(|config| config.name == "Main")
        .expect("the main class is found");

    let profile = provider.profile_plan(&config, &model).unwrap();
    let args = arguments(&profile.plan.program);
    assert!(
        args.iter()
            .any(|arg| arg.starts_with("-XX:StartFlightRecording=")),
        "the plan starts the JVM with a recording: {args:?}"
    );
    assert!(
        args.iter().any(|arg| arg.contains("dumponexit=true")),
        "the recording is written when the JVM ends: {args:?}"
    );
    assert!(
        profile.recording.starts_with(root.join("cache")),
        "{:?}",
        profile.recording
    );

    run_to_end(profile.plan);

    assert!(
        profile.recording.is_file(),
        "the JVM wrote {}",
        profile.recording.display()
    );
    let snapshot = provider
        .profile_snapshot(&profile.recording, &model)
        .unwrap();
    assert!(snapshot.samples > 0, "the recording has samples");
    assert!(!snapshot.truncated);
    let main = snapshot
        .roots
        .iter()
        .find(|node| node.frame.name() == "demo.Main.main")
        .expect("the method of the program is in the tree");
    assert!(main.total > 0, "{main:?}");
    assert_eq!(main.frame.source, Some(root.join("src/demo/Main.java")));
}
