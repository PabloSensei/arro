//! The start of the server: the memory settings of the JVM and the Gradle import
//! arguments. No server runs here; the plan and the settings are read as data.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use jdtls_adapter::{Jdtls, JdtlsConfig};
use sidecar::ServerSpec;

fn project() -> (Jdtls, PathBuf) {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!("jdtls-plan-{}-{n}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let plugins = root.join("home").join("plugins");
    fs::create_dir_all(&plugins).unwrap();
    fs::write(plugins.join("org.eclipse.equinox.launcher_1.8.0.jar"), b"").unwrap();
    let project = root.join("project");
    fs::create_dir_all(&project).unwrap();
    fs::write(project.join("build.gradle"), "plugins { id 'java' }\n").unwrap();
    let config = JdtlsConfig {
        jdtls_home: root.join("home"),
        debug_plugin: root.join("debug.jar"),
        java_home: root.join("jdk"),
        project_root: project,
        state_dir: root.join("state"),
        runtimes: Vec::new(),
        maven_settings: None,
        gradle_home: None,
        gradle_user_home: None,
        gradle_java_home: None,
        offline: true,
        environment: HashMap::new(),
    };
    (Jdtls::new(config).unwrap(), root)
}

#[test]
fn the_plan_has_the_collector_that_returns_memory() {
    let (jdtls, root) = project();
    let args = jdtls.plan().args;

    for setting in [
        "-XX:+UseParallelGC",
        "-XX:MinHeapFreeRatio=20",
        "-XX:MaxHeapFreeRatio=40",
        "-XX:AdaptiveSizePolicyWeight=90",
    ] {
        assert!(
            args.iter().any(|arg| arg == setting),
            "{setting} is missing: {args:?}"
        );
    }
    assert!(
        args.iter().any(|arg| arg == "-Xmx1024m"),
        "the first start stays at one gigabyte: {args:?}"
    );
    // The heap and the collector are JVM arguments, not arguments of jdtls.
    let jar = args.iter().position(|arg| arg == "-jar").unwrap();
    assert!(
        args[..jar]
            .iter()
            .all(|arg| !arg.contains("org.eclipse.equinox.launcher_")),
        "the launcher jar comes after the JVM arguments"
    );
    drop(jdtls);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn the_gradle_import_gets_a_ten_minute_daemon_timeout() {
    let (jdtls, root) = project();
    let arguments = jdtls.settings()["java"]["import"]["gradle"]["arguments"]
        .as_str()
        .unwrap()
        .to_owned();

    assert!(
        arguments.contains("-Dorg.gradle.daemon.idletimeout=600000"),
        "{arguments}"
    );
    assert!(arguments.contains("--no-scan"), "{arguments}");
    drop(jdtls);
    let _ = fs::remove_dir_all(root);
}
