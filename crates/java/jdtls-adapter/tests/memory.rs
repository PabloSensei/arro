//! The Private Working Set of the server on PetClinic after the import and 30 s of
//! idling. A measurement, no threshold: the number goes into the report.
//!
//! cargo test -p jdtls-adapter --test memory -- --ignored --test-threads=1
//!
//! `JDTLS_OLD_JVM_ARGS=1` runs the same test with the JVM arguments from before
//! T-0108, so both numbers come from the same project and machine.
//!
//! Needs `IDE_CORPUS` with `src/petclinic` and `MAVEN_USER_HOME` with a filled
//! repository. The project is copied, the corpus stays untouched.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use jdtls_adapter::{Jdtls, JdtlsConfig, Runtime};
use lsp_client::lsp_types::InitializeParams;
use lsp_client::{Client, ResponseError, ServerEvents};
use serde_json::Value;
use sidecar::{Limits, Plan, ServerSpec, Sidecar, State, Status, SupervisedLauncher, Trust};
use tokio::sync::watch;
use tokio::time::{sleep, timeout};

const READY: Duration = Duration::from_secs(1800);
const IDLE: Duration = Duration::from_secs(30);
/// The idle wait the report of T-0134 asks for, long enough for the periodic G1 cycle.
const IDLE_LONG: Duration = Duration::from_secs(300);

const SKIPPED: &[&str] = &[".git", "target", "build", "bin", ".gradle", "node_modules"];

/// Set to any value to measure the JVM arguments from before T-0108: a 4 GiB heap
/// from the first start and G1 instead of the parallel collector.
const OLD_ARGUMENTS: &str = "JDTLS_OLD_JVM_ARGS";

/// The settings of T-0108 that the old arguments did not have.
const NEW_SETTINGS: &[&str] = &[
    "-Xms100m",
    "-XX:+UseParallelGC",
    "-XX:GCTimeRatio=4",
    "-XX:AdaptiveSizePolicyWeight=90",
    "-Dsun.zip.disableMemoryMapping=true",
];

const OLD_HEAP_MB: u32 = 4096;

struct Silent;

impl ServerEvents for Silent {
    fn notification(&self, _: &str, _: Value) {}
}

/// The server with either the current JVM arguments or the old ones, so one test
/// measures both on the same project.
struct Measured {
    inner: Jdtls,
    old: bool,
}

impl ServerSpec for Measured {
    fn preflight(&self) -> Result<(), String> {
        self.inner.preflight()
    }

    fn starting(&self) {
        self.inner.starting();
    }

    fn crash_reason(&self) -> Option<String> {
        self.inner.crash_reason()
    }

    fn plan(&self) -> Plan {
        let mut plan = self.inner.plan();
        if self.old {
            plan.args
                .retain(|arg| !NEW_SETTINGS.contains(&arg.as_str()) && !arg.starts_with("-Xmx"));
            // The JVM settings end before `-jar`; everything after it goes to jdtls.
            let at = plan
                .args
                .iter()
                .position(|arg| arg == "-jar")
                .unwrap_or(plan.args.len());
            plan.args.insert(at, format!("-Xmx{OLD_HEAP_MB}m"));
            plan.args.insert(at + 1, "-XX:+UseG1GC".to_owned());
        }
        plan
    }

    fn crashed(&self, exit_code: Option<i32>) {
        self.inner.crashed(exit_code);
    }

    fn initialize_params(&self) -> InitializeParams {
        self.inner.initialize_params()
    }

    fn initialized(&self, client: &Client) {
        self.inner.initialized(client);
    }

    fn state_of(&self, method: &str, params: &Value) -> Option<State> {
        self.inner.state_of(method, params)
    }

    fn request(&self, method: &str, params: Value) -> Result<Value, ResponseError> {
        self.inner.request(method, params)
    }
}

fn corpus() -> PathBuf {
    PathBuf::from(std::env::var("IDE_CORPUS").expect("set IDE_CORPUS"))
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        let text = name.to_string_lossy();
        if SKIPPED.contains(&text.as_ref()) {
            continue;
        }
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &to.join(&name));
        } else {
            fs::copy(entry.path(), to.join(&name)).unwrap();
        }
    }
}

/// A private copy of PetClinic and a fresh server state next to it.
fn project() -> (PathBuf, PathBuf) {
    let corpus = corpus();
    let scratch = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("memory");
    let root = scratch.join("project");
    if !root.exists() {
        copy_tree(&corpus.join("src").join("petclinic"), &root);
    }
    let state_dir = scratch.join("state");
    let _ = fs::remove_dir_all(&state_dir);
    fs::create_dir_all(&state_dir).unwrap();
    (root, state_dir)
}

fn config(root: PathBuf, state_dir: PathBuf) -> JdtlsConfig {
    let corpus = corpus();
    let tools = corpus.join("tools");
    let maven = PathBuf::from(std::env::var("MAVEN_USER_HOME").expect("set MAVEN_USER_HOME"));
    let settings = state_dir.join("maven-settings.xml");
    let repository = maven
        .join("repository")
        .display()
        .to_string()
        .replace('\\', "/");
    fs::write(
        &settings,
        format!(
            "<settings><localRepository>{repository}</localRepository><offline>true</offline></settings>"
        ),
    )
    .unwrap();
    let runtime = |name: &str, version: &str, default| Runtime {
        name: name.to_owned(),
        home: fs::canonicalize(corpus.join("jdk").join(version)).unwrap(),
        default,
    };
    JdtlsConfig {
        jdtls_home: tools.join("jdtls-1.63.0"),
        debug_plugin: tools.join(
            "vscode-java-debug-0.60.0/extension/server/com.microsoft.java.debug.plugin-0.53.2.jar",
        ),
        java_home: fs::canonicalize(corpus.join("jdk").join("25")).unwrap(),
        project_root: root,
        state_dir,
        runtimes: vec![
            runtime("JavaSE-17", "17", false),
            runtime("JavaSE-25", "25", true),
        ],
        maven_settings: Some(settings),
        gradle_home: None,
        gradle_user_home: None,
        gradle_java_home: None,
        offline: true,
        environment: HashMap::new(),
    }
}

async fn until(status: &mut watch::Receiver<Status>, state: State) -> Status {
    timeout(READY, status.wait_for(|it| it.state == state))
        .await
        .expect("the server did not reach the state in time")
        .unwrap()
        .clone()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs IDE_CORPUS and a filled package cache"]
async fn private_working_set_after_the_import() {
    let old = std::env::var_os(OLD_ARGUMENTS).is_some();
    let (root, state_dir) = project();
    let spec = Arc::new(Measured {
        inner: Jdtls::new(config(root, state_dir)).unwrap(),
        old,
    });
    let plan = spec.plan();
    let jar = plan.args.iter().position(|arg| arg == "-jar").unwrap();
    let jvm: Vec<&String> = plan.args[..jar]
        .iter()
        .filter(|arg| arg.starts_with("-X") || arg.starts_with("-D"))
        .collect();
    assert!(
        plan.args[..jar].iter().any(|arg| arg.starts_with("-Xmx")),
        "the heap is a JVM setting, not an argument of the server"
    );
    println!("JVM arguments: old={old} {jvm:?}");
    let (_, memory) = watch::channel(false);
    let sidecar = Sidecar::start_with_memory(
        memory,
        spec,
        Arc::new(SupervisedLauncher),
        Arc::new(Silent),
        Limits::default(),
        Trust::Trusted,
    )
    .unwrap();
    let mut status = sidecar.status();
    let ready = until(&mut status, State::Ready).await;
    let pid = ready.process_id.expect("the server has a process");
    eprintln!("import ended, waiting {IDLE:?}");
    sleep(IDLE).await;

    let tree = platform::process_tree();
    let process = tree
        .iter()
        .find(|process| process.pid == pid)
        .expect("the server process is in the tree");
    let mib = |bytes: u64| bytes as f64 / (1024. * 1024.);
    println!(
        "MEMORY pid={pid} private_working_set_mib={:.1} working_set_mib={:.1} private_bytes_mib={:.1}",
        mib(process.private_working_set),
        mib(process.working_set),
        mib(process.private_bytes)
    );
    assert!(
        process.private_working_set > 0,
        "the memory of the server is readable"
    );

    timeout(Duration::from_secs(30), sidecar.stop())
        .await
        .expect("the server stops");
}

/// The same measurement after five idle minutes, as the report of T-0134 asks: the
/// periodic G1 cycle has run and the server should have given the unused memory back.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs IDE_CORPUS and a filled package cache; takes five idle minutes"]
async fn private_working_set_after_five_idle_minutes() {
    let (root, state_dir) = project();
    let spec = Arc::new(Measured {
        inner: Jdtls::new(config(root, state_dir)).unwrap(),
        old: false,
    });
    let (_, memory) = watch::channel(false);
    let sidecar = Sidecar::start_with_memory(
        memory,
        spec,
        Arc::new(SupervisedLauncher),
        Arc::new(Silent),
        Limits::default(),
        Trust::Trusted,
    )
    .unwrap();
    let mut status = sidecar.status();
    let ready = until(&mut status, State::Ready).await;
    let pid = ready.process_id.expect("the server has a process");
    eprintln!("import ended, waiting {IDLE_LONG:?}");
    sleep(IDLE_LONG).await;

    let tree = platform::process_tree();
    let process = tree
        .iter()
        .find(|process| process.pid == pid)
        .expect("the server process is in the tree");
    let mib = |bytes: u64| bytes as f64 / (1024. * 1024.);
    println!(
        "MEMORY_LONG pid={pid} private_working_set_mib={:.1} working_set_mib={:.1} private_bytes_mib={:.1}",
        mib(process.private_working_set),
        mib(process.working_set),
        mib(process.private_bytes)
    );
    assert!(
        process.private_working_set > 0,
        "the memory of the server is readable"
    );

    timeout(Duration::from_secs(30), sidecar.stop())
        .await
        .expect("the server stops");
}
