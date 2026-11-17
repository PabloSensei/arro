//! Tests of PetClinic run through Maven and Gradle with the IDE listener, without a window.
//! Needs JDKs, the corpus, the helper jars and filled caches, so these do not run in CI:
//!
//!   cargo xtask build-helpers --out DIR
//!   cargo build -p ide-ctrl
//!   IDE_HELPERS_DIR=DIR cargo test -p java-exec --test test_runs -- --ignored --test-threads=1
//!
//! `E2E_MAVEN_REPO` moves the Maven repository, `IDE_CORPUS` points to the corpus.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use java_exec::{JavaTestProvider, find_tests};
use java_project::{BuildEnv, Gradle, Maven};
use project_model::{BuildSystem, ProjectModel};
use runner::{ConsoleEvent, TestEvent, TestKind, TestRequest, TestRun, TestState, TestTree};

const PATIENCE: Duration = Duration::from_secs(600);
const STOP_PATIENCE: Duration = Duration::from_secs(60);
const PACKAGE: &str = "org.springframework.samples.petclinic";
const FAILING: &str = "org.springframework.samples.petclinic.IdeFailingTests";
/// Line 11 of the class below holds the failing assertion.
const FAILING_SOURCE: &str = "package org.springframework.samples.petclinic;

import static org.junit.jupiter.api.Assertions.assertEquals;

import org.junit.jupiter.api.Test;

class IdeFailingTests {

	@Test
	void failsOnPurpose() {
		assertEquals(1, 2, \"ide check\");
	}

	@Test
	void passes() {
	}

}
";

fn corpus() -> PathBuf {
    env::var_os("IDE_CORPUS").map_or_else(
        || PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../corpus")),
        PathBuf::from,
    )
}

fn work_dir(name: &str) -> PathBuf {
    let exe = env::current_exe().unwrap();
    exe.ancestors().nth(3).unwrap().join("e2e-tests").join(name)
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

/// A copy with one test class that fails on purpose.
fn petclinic_copy(name: &str) -> PathBuf {
    let root = work_dir(name).join("petclinic");
    let _ = fs::remove_dir_all(&root);
    copy_project(&corpus().join("src/petclinic"), &root);
    let tests = root.join("src/test/java/org/springframework/samples/petclinic");
    fs::write(tests.join("IdeFailingTests.java"), FAILING_SOURCE).unwrap();
    root
}

/// Test JVMs of Surefire and Gradle that are still alive.
fn test_jvms() -> Vec<u32> {
    let script = "Get-CimInstance Win32_Process -Filter \"Name='java.exe'\" | \
        Where-Object { $_.CommandLine -like '*surefirebooter*' -or \
            $_.CommandLine -like '*Gradle Test Executor*' } | \
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

fn assert_no_test_jvms() {
    let start = Instant::now();
    loop {
        let left = test_jvms();
        if left.is_empty() {
            return;
        }
        assert!(start.elapsed() < STOP_PATIENCE, "test JVMs left: {left:?}");
        std::thread::sleep(Duration::from_millis(500));
    }
}

struct Outcome {
    tree: TestTree,
    exit: Option<i32>,
    log: String,
}

/// Runs to the end, or stops once the first case has started when `stop_early`.
fn run(
    provider: &JavaTestProvider,
    model: &ProjectModel,
    request: TestRequest,
    stop_early: bool,
) -> Outcome {
    let run = TestRun::start(provider, model, &request).unwrap();
    let mut tree = TestTree::default();
    let mut log = String::new();
    let mut stopped = false;
    let start = Instant::now();
    let exit = loop {
        assert!(start.elapsed() < PATIENCE, "the run took too long:\n{log}");
        while let Ok(event) = run.events().try_recv() {
            let case_started =
                matches!(&event, TestEvent::Started(node) if node.kind == TestKind::Case);
            tree.apply(&event);
            if stop_early && case_started && !stopped {
                stopped = true;
                run.stop();
            }
        }
        match run
            .console()
            .events()
            .recv_timeout(Duration::from_millis(50))
        {
            Ok(ConsoleEvent::Output { text, .. }) => log.push_str(&text),
            Ok(ConsoleEvent::Failed { message }) => panic!("{message}\n{log}"),
            Ok(ConsoleEvent::Exited { code, .. }) => break code,
            Ok(ConsoleEvent::Started { .. } | ConsoleEvent::Closed) | Err(_) => {}
        }
    };
    // The last events may still be on their way when the build tool exits.
    std::thread::sleep(Duration::from_millis(300));
    while let Ok(event) = run.events().try_recv() {
        tree.apply(&event);
    }
    assert!(
        !stop_early || stopped,
        "no case started before the end:\n{log}"
    );
    Outcome { tree, exit, log }
}

fn tail(log: &str) -> &str {
    let start = log.len().saturating_sub(4000);
    let start = (start..log.len())
        .find(|&at| log.is_char_boundary(at))
        .unwrap_or(0);
    &log[start..]
}

fn request(module: Option<&str>, suite: &str, case: Option<&str>) -> TestRequest {
    TestRequest {
        module: module.map(str::to_owned),
        suite: Some(format!("{PACKAGE}.{suite}")),
        case: case.map(str::to_owned),
        ..TestRequest::default()
    }
}

/// One class, one method, a failure with its line, and a stop in the middle.
fn check_tool(provider: &JavaTestProvider, model: &ProjectModel, module: &str) {
    let found = find_tests(&[model.root.join("src/test/java")]);
    let vets = found
        .iter()
        .find(|class| class.class == format!("{PACKAGE}.vet.VetControllerTests"))
        .expect("the tests are found in the sources");
    assert!(!vets.methods.is_empty());

    let class = run(
        provider,
        model,
        request(None, "vet.VetControllerTests", None),
        false,
    );
    let counts = class.tree.counts();
    assert_eq!(class.exit, Some(0), "{}", tail(&class.log));
    assert_eq!(counts.passed, vets.methods.len(), "{counts:?}");
    assert_eq!(counts.failed, 0);
    assert_no_test_jvms();

    let method = run(
        provider,
        model,
        request(Some(module), "vet.VetTests", Some("serialization")),
        false,
    );
    assert_eq!(method.exit, Some(0), "{}", tail(&method.log));
    assert_eq!(method.tree.counts().passed, 1, "{:?}", method.tree.counts());
    assert_no_test_jvms();

    let failing = run(
        provider,
        model,
        request(None, "IdeFailingTests", None),
        false,
    );
    assert_ne!(failing.exit, Some(0), "a failed test fails the build");
    let counts = failing.tree.counts();
    assert_eq!(
        (counts.passed, counts.failed),
        (1, 1),
        "{}",
        tail(&failing.log)
    );
    let node = failing.tree.case(FAILING, "failsOnPurpose").unwrap();
    let TestState::Failed { message, trace } = &node.state else {
        panic!("not failed: {:?}", node.state);
    };
    assert!(
        message.as_deref().unwrap().contains("ide check"),
        "{message:?}"
    );
    assert!(
        trace
            .as_deref()
            .unwrap()
            .contains("IdeFailingTests.java:11"),
        "{trace:?}"
    );
    assert_no_test_jvms();

    // A Spring context takes a while to start, the stop lands in the middle of the class.
    let stopped = run(
        provider,
        model,
        request(None, "service.ClinicServiceTests", None),
        true,
    );
    assert_ne!(stopped.exit, Some(0));
    assert_no_test_jvms();
}

#[test]
#[ignore = "needs JDKs, the corpus, the helpers and a Maven cache"]
fn petclinic_tests_run_on_maven() {
    let env = build_env("maven");
    let root = petclinic_copy("maven");
    let model = Maven::new(env.clone()).scan(&root).unwrap();
    assert_eq!(model.tool, "maven");
    let module = model.modules[0].id.clone();

    check_tool(&JavaTestProvider::new(env), &model, &module);

    // The project's JaCoCo agent still ran through its argLine.
    assert!(root.join("target/jacoco.exec").is_file(), "no jacoco.exec");
}

#[test]
#[ignore = "needs JDKs, the corpus, the helpers and a Gradle cache"]
fn petclinic_tests_run_on_gradle() {
    let env = build_env("gradle");
    let root = petclinic_copy("gradle");
    fs::remove_file(root.join("pom.xml")).unwrap();
    let model = Gradle::new(env.clone()).scan(&root).unwrap();
    assert_eq!(model.tool, "gradle");

    check_tool(&JavaTestProvider::new(env), &model, ":");
}
