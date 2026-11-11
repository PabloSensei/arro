//! A JDK older than the server needs is refused without a process, so the window can
//! say why the language features are off.
//!
//! cargo test -p jdtls-adapter --test startup

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use jdtls_adapter::{Jdtls, JdtlsConfig};
use lsp_client::ServerEvents;
use serde_json::Value;
use sidecar::{Launcher, Limits, Plan, ServerProcess, Sidecar, State, Status, Trust};
use tokio::sync::watch;
use tokio::time::timeout;

/// A launcher that records every call; a refused JDK must never reach it.
struct Recording(Arc<AtomicBool>);

impl Launcher for Recording {
    fn launch(&self, _: &Plan) -> io::Result<ServerProcess> {
        self.0.store(true, Ordering::SeqCst);
        Err(io::Error::other("no server in this test"))
    }
}

struct Silent;

impl ServerEvents for Silent {
    fn notification(&self, _: &str, _: Value) {}
}

fn scratch(name: &str) -> PathBuf {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let serial = NEXT.fetch_add(1, Ordering::Relaxed);
    PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("startup-{name}-{serial}"))
}

fn jdk(root: &Path, version: &str) -> PathBuf {
    let home = root.join("jdk");
    fs::create_dir_all(home.join("bin")).unwrap();
    fs::write(
        home.join("release"),
        format!("JAVA_VERSION=\"{version}\"\n"),
    )
    .unwrap();
    home
}

fn config(root: &Path, java_home: PathBuf) -> JdtlsConfig {
    let plugins = root.join("home").join("plugins");
    fs::create_dir_all(&plugins).unwrap();
    fs::write(plugins.join("org.eclipse.equinox.launcher_1.8.0.jar"), b"").unwrap();
    fs::create_dir_all(root.join("project")).unwrap();
    JdtlsConfig {
        jdtls_home: root.join("home"),
        debug_plugin: root.join("debug.jar"),
        java_home,
        project_root: root.join("project"),
        state_dir: root.join("state"),
        runtimes: Vec::new(),
        maven_settings: None,
        gradle_user_home: None,
        gradle_java_home: None,
        offline: true,
        environment: HashMap::new(),
    }
}

async fn until(status: &mut watch::Receiver<Status>, state: State) -> Status {
    timeout(
        Duration::from_secs(10),
        status.wait_for(|it| it.state == state),
    )
    .await
    .expect("the sidecar did not reach the state in time")
    .unwrap()
    .clone()
}

#[tokio::test]
async fn an_old_jdk_degrades_without_launching_anything() {
    let root = scratch("old-jdk");
    let _ = fs::remove_dir_all(&root);
    let home = jdk(&root, "21.0.5");
    let spec = Arc::new(Jdtls::new(config(&root, home.clone())).unwrap());
    let (_, memory) = watch::channel(false);
    let launched = Arc::new(AtomicBool::new(false));
    let sidecar = Sidecar::start_with_memory(
        memory,
        spec,
        Arc::new(Recording(launched.clone())),
        Arc::new(Silent),
        Limits {
            restarts: 0,
            ..Limits::default()
        },
        Trust::Trusted,
    )
    .unwrap();

    let mut status = sidecar.status();
    let failed = until(&mut status, State::Degraded).await;
    let reason = failed.last_error.clone().unwrap();
    assert!(reason.contains("needs Java 25 or newer"), "{reason}");
    assert!(reason.contains("found Java 21 at"), "{reason}");
    let path = home.to_string_lossy().into_owned();
    assert!(reason.contains(&path), "{reason}");
    assert!(
        !launched.load(Ordering::SeqCst),
        "a refused JDK starts no process"
    );

    timeout(Duration::from_secs(5), sidecar.stop())
        .await
        .expect("the sidecar stops");
}
