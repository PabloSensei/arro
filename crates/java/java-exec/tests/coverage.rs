//! Run with coverage of a two-class project with the JaCoCo agent, without a window: the
//! agent writes an `.exec`, the `coverage-report` helper turns it into lines and branches.
//! Needs JDK 21, the corpus, the helpers and the JaCoCo jars, so these do not run in CI:
//!
//!   cargo xtask build-helpers --out DIR
//!   IDE_HELPERS_DIR=DIR cargo test -p java-exec --test coverage -- --ignored --test-threads=1
//!
//! `IDE_CORPUS` points to the corpus, `MAVEN_USER_HOME` to the repository with the agent.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use java_exec::JavaProvider;
use java_project::{BuildEnv, CoverageTools, Jdk};
use lang_api::{CoverageReport, LineState};
use project_model::{BuildSystem, MAIN_UNIT, Module, ProjectModel, Unit};
use runner::{Console, ConsoleEvent, RunConfig, RunConfigProvider};

const PATIENCE: Duration = Duration::from_secs(120);

const MAIN: &str = "package demo;

public class Main {
    public static void main(String[] args) {
        Counter counter = new Counter();
        counter.bump();
        counter.choose(1);
        System.out.println(\"done\");
    }
}
";

const COUNTER: &str = "package demo;

public class Counter {
    private int value;

    public int bump() {
        value = value + 1;
        return value;
    }

    public int unused() {
        return 42;
    }

    public int choose(int other) {
        if (other > 0) {
            return 1;
        }
        return 0;
    }
}
";

fn corpus() -> PathBuf {
    env::var_os("IDE_CORPUS").map_or_else(
        || PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../corpus")),
        PathBuf::from,
    )
}

/// `target/debug/helpers`, or what the run of the suite passed as `IDE_HELPERS_DIR`.
fn helpers_dir() -> PathBuf {
    env::var_os("IDE_HELPERS_DIR").map_or_else(
        || {
            env::current_exe()
                .unwrap()
                .parent()
                .unwrap()
                .with_file_name("helpers")
        },
        PathBuf::from,
    )
}

/// The agent of the release the helper jar was built against.
fn jacoco_agent() -> PathBuf {
    if let Some(path) = env::var_os("IDE_JACOCO_AGENT") {
        return PathBuf::from(path);
    }
    let home = env::var_os("MAVEN_USER_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(".m2"));
    home.join("repository/org/jacoco/org.jacoco.agent/0.8.15")
        .join("org.jacoco.agent-0.8.15-runtime.jar")
}

fn javac(jdk: &Jdk) -> PathBuf {
    let name = if cfg!(windows) { "javac.exe" } else { "javac" };
    jdk.home.join("bin").join(name)
}

/// A project of two classes compiled into `out`, with no build tool behind it.
fn project(root: &Path) -> ProjectModel {
    fs::create_dir_all(root.join("src/demo")).unwrap();
    fs::write(root.join("src/demo/Main.java"), MAIN).unwrap();
    fs::write(root.join("src/demo/Counter.java"), COUNTER).unwrap();
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

    fn sync(&self, _root: &Path, _cancel: &base::Cancel) -> base::Result<ProjectModel> {
        Err("not used".into())
    }

    fn assemble(&self, _root: &Path) -> base::Result<proc_sup::ProcessSpec> {
        Err("not used".into())
    }

    fn assemble_if_needed(&self, _root: &Path) -> base::Result<Option<proc_sup::ProcessSpec>> {
        Ok(None)
    }
}

/// The line of a source that holds `needle`, one-based as the report names lines.
fn line_of(source: &str, needle: &str) -> u32 {
    let line = source
        .lines()
        .position(|line| line.contains(needle))
        .expect("the line is in the source");
    line as u32 + 1
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
#[ignore = "needs JDK 21, the corpus, the helpers and the JaCoCo jars"]
fn a_covered_run_marks_called_lines_uncovered_lines_and_partly_covered_branches() {
    let jdk = Jdk::at(&corpus().join("jdk/21")).expect("JDK 21 of the corpus");
    let root = test_support::scratch("java-coverage");
    let model = project(&root);
    let out = root.join("out");
    fs::create_dir_all(&out).unwrap();
    let status = Command::new(javac(&jdk))
        .arg("--release")
        .arg("21")
        .arg("-d")
        .arg(&out)
        .arg(root.join("src/demo/Main.java"))
        .arg(root.join("src/demo/Counter.java"))
        .status()
        .unwrap();
    assert!(status.success(), "javac compiled the project");

    let helpers = helpers_dir();
    let tools = CoverageTools {
        agent: jacoco_agent(),
        report: helpers.join("coverage"),
    };
    assert!(
        tools.agent.is_file(),
        "no agent at {}",
        tools.agent.display()
    );
    assert!(
        tools.report.is_dir(),
        "no report jars at {}",
        tools.report.display()
    );
    let env = BuildEnv {
        jdk: Some(jdk),
        cache_root: Some(root.join("cache")),
        helpers: Some(helpers),
        coverage: Some(tools),
        ..BuildEnv::default()
    };
    let provider = JavaProvider::new(env, Arc::new(NoTool));
    let config: RunConfig = provider
        .discover(&model)
        .unwrap()
        .into_iter()
        .find(|config| config.name == "Main")
        .expect("the main class is found");

    let coverage = provider.coverage_plan(&config, &model).unwrap();
    let args = arguments(&coverage.plan.program);
    assert!(
        args.iter().any(|arg| arg.starts_with("-javaagent:"))
            && args.iter().any(|arg| arg.contains("destfile=")),
        "the plan starts the JVM with the agent: {args:?}"
    );
    assert!(
        coverage.exec.starts_with(root.join("cache")),
        "{:?}",
        coverage.exec
    );

    run_to_end(coverage.plan);

    assert!(
        coverage.exec.is_file(),
        "the agent wrote {}",
        coverage.exec.display()
    );
    let report: CoverageReport = provider.coverage_report(&coverage.exec, &model).unwrap();

    let counter = root.join("src/demo/Counter.java");
    let file = report.file(&counter).expect("the report has Counter.java");
    let called = file.line(line_of(COUNTER, "value = value + 1")).unwrap();
    assert_eq!(called.state(), LineState::Covered, "{called:?}");
    assert!(called.hits > 0, "{called:?}");
    let uncalled = file.line(line_of(COUNTER, "return 42;")).unwrap();
    assert_eq!(uncalled.state(), LineState::NotCovered, "{uncalled:?}");
    assert_eq!(uncalled.hits, 0);
    let branch = file.line(line_of(COUNTER, "if (other > 0)")).unwrap();
    assert_eq!(branch.state(), LineState::Partial, "{branch:?}");
    assert_eq!(
        branch.branches,
        Some(lang_api::BranchCoverage {
            covered: 1,
            total: 2
        }),
        "{branch:?}"
    );

    let main = root.join("src/demo/Main.java");
    let main_file = report.file(&main).expect("the report has Main.java");
    let call = main_file.line(line_of(MAIN, "counter.bump();")).unwrap();
    assert_eq!(call.state(), LineState::Covered, "{call:?}");

    let kind = report
        .types
        .iter()
        .find(|kind| kind.name == "demo.Counter")
        .expect("the report names the type");
    assert_eq!((kind.method.covered, kind.method.total), (2, 3), "{kind:?}");
    assert_eq!((kind.branch.covered, kind.branch.total), (1, 2), "{kind:?}");
}
