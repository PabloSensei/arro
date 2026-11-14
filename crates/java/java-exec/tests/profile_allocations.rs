//! A profile run on the JDK of the corpus: the allocations of a program by weight, and a
//! JUnit test of a real Maven project recorded through the test provider. Needs JDK 21,
//! the corpus, the helper jars and a filled Maven cache, so these do not run in CI:
//!
//!   cargo xtask build-helpers --out DIR
//!   IDE_HELPERS_DIR=DIR cargo test -p java-exec --test profile_allocations -- --ignored --test-threads=1

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use base::Cancel;
use java_exec::{JavaProvider, JavaTestProvider};
use java_project::{BuildEnv, Jdk, Maven};
use project_model::{BuildSystem, MAIN_UNIT, Module, ProjectModel, Unit};
use runner::{
    Console, ConsoleEvent, RunConfigProvider, TestProvider, TestRequest, TestRun, TestState,
    TestTree,
};

const PATIENCE: Duration = Duration::from_secs(600);

const ALLOCATING: &str = "package demo;

public class Alloc {
    static byte[] keep;

    static void fill() {
        keep = new byte[256 * 1024];
    }

    public static void main(String[] args) throws Exception {
        long end = System.nanoTime() + 2_000_000_000L;
        int made = 0;
        while (System.nanoTime() < end) {
            fill();
            made++;
        }
        System.out.println(made + \" \" + keep.length);
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

fn build_env(name: &str) -> BuildEnv {
    let maven_repo = env::var_os("E2E_MAVEN_REPO")
        .map(PathBuf::from)
        .or_else(|| {
            env::var_os("MAVEN_USER_HOME").map(|home| PathBuf::from(home).join("repository"))
        });
    let maven_args = maven_repo
        .map(|repo| vec![format!("-Dmaven.repo.local={}", repo.display())])
        .unwrap_or_default();
    BuildEnv {
        maven_args,
        helpers: env::var_os("IDE_HELPERS_DIR").map(PathBuf::from),
        cache_root: Some(test_support::scratch(name).join("cache")),
        ..BuildEnv::default()
    }
}

fn project(root: &Path) -> ProjectModel {
    fs::create_dir_all(root.join("src/demo")).unwrap();
    fs::write(root.join("src/demo/Alloc.java"), ALLOCATING).unwrap();
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
fn a_recorded_run_names_the_allocated_byte_arrays_by_weight() {
    let jdk = Jdk::at(&corpus().join("jdk/21")).expect("JDK 21 of the corpus");
    let folder = test_support::TempDir::new("java-profile-allocations");
    let root = folder.path();
    let model = project(root);
    let out = root.join("out");
    fs::create_dir_all(&out).unwrap();
    let status = Command::new(javac(&jdk))
        .arg("--release")
        .arg("21")
        .arg("-d")
        .arg(&out)
        .arg(root.join("src/demo/Alloc.java"))
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
        .find(|config| config.name == "Alloc")
        .expect("the main class is found");

    let profile = provider.profile_plan(&config, &model).unwrap();
    assert!(
        arguments(&profile.plan.program)
            .iter()
            .any(|argument| argument.starts_with("-XX:StartFlightRecording=")),
        "the plan starts the JVM with a recording"
    );
    run_to_end(profile.plan);

    assert!(
        profile.recording.is_file(),
        "{}",
        profile.recording.display()
    );
    let report = provider.profile_report(&profile.recording, &model).unwrap();
    assert!(report.has_allocations(), "the recording has no allocations");
    let byte_array = report
        .types
        .iter()
        .find(|entry| entry.allocated.class == "[B")
        .expect("the byte arrays are in the Allocated Types");
    assert!(
        byte_array.allocated.weight >= 256 * 1024,
        "{} bytes",
        byte_array.allocated.weight
    );
    assert!(byte_array.allocated.count > 0);
    let tree = report.type_tree("[B").unwrap();
    assert!(tree.samples > 0, "the stacks of the byte arrays are folded");
    let fill = tree
        .roots
        .iter()
        .flat_map(|root| root.children.iter())
        .find(|node| node.frame.name() == "demo.Alloc.fill");
    assert!(
        fill.is_some_and(|node| node.total > 0),
        "the method that allocates is in the tree: {:?}",
        tree.roots
    );
}

/// Drives a test run to its end and returns the tree it built.
fn run_tests(run: TestRun) -> TestTree {
    let mut tree = TestTree::default();
    let started = Instant::now();
    let mut log = String::new();
    loop {
        assert!(
            started.elapsed() < PATIENCE,
            "the tests took too long:\n{log}"
        );
        while let Ok(event) = run.events().try_recv() {
            tree.apply(&event);
        }
        match run
            .console()
            .events()
            .recv_timeout(Duration::from_millis(50))
        {
            Ok(ConsoleEvent::Output { text, .. }) => log.push_str(&text),
            Ok(ConsoleEvent::Failed { message }) => panic!("{message}\n{log}"),
            Ok(ConsoleEvent::Exited { .. }) => break,
            Ok(ConsoleEvent::Started { .. } | ConsoleEvent::Closed) | Err(_) => {}
        }
    }
    std::thread::sleep(Duration::from_millis(300));
    while let Ok(event) = run.events().try_recv() {
        tree.apply(&event);
    }
    tree
}

#[test]
#[ignore = "needs JDKs, the corpus, the helpers and a Maven cache"]
fn a_junit_test_of_a_maven_project_records_its_jvm() {
    let env = build_env("profile-junit");
    let folder = test_support::TempDir::new("profile-junit-project");
    let root = folder.path().join("petclinic");
    copy_project(&corpus().join("src/petclinic"), &root);
    let model = Maven::new(env.clone()).scan(&root).unwrap();
    assert_eq!(model.tool, "maven");
    let provider = JavaTestProvider::new(env.clone());
    assert!(provider.supports_profiler());

    let request = TestRequest {
        module: None,
        suite: Some("org.springframework.samples.petclinic.vet.VetTests".into()),
        case: Some("serialization".into()),
        ..TestRequest::default()
    };
    let started = TestRun::profile_start(&provider, &model, &request).unwrap();
    assert!(
        started.recording.starts_with(env.cache_root()),
        "{:?}",
        started.recording
    );
    let tree = run_tests(started.run);
    assert_eq!(
        tree.case(
            "org.springframework.samples.petclinic.vet.VetTests",
            "serialization"
        )
        .map(|node| &node.state),
        Some(&TestState::Passed),
        "the test passed"
    );

    assert!(
        started.recording.is_file(),
        "the test JVM wrote {}",
        started.recording.display()
    );
    let report = provider.profile_report(&started.recording, &model).unwrap();
    assert!(
        report.cpu_tree.samples > 0,
        "the recording of the test JVM has samples"
    );
}
