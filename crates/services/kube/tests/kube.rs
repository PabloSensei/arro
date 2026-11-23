//! The crate against a fake `kubectl`: contexts, namespaces, resources, the arguments of
//! the actions, an unreachable cluster and the end of a port forwarding. No cluster and no
//! kubeconfig are needed.

use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use kube::{ClusterState, Kubectl, ResourceKind, take};
use serde_json::json;

const FAKE: &str = env!("CARGO_BIN_EXE_kube-fake-kubectl");
const PATIENCE: Duration = Duration::from_secs(20);
/// The environment of a process is one for the whole test binary, so the tests that name
/// a state file take turns.
static ENV: Mutex<()> = Mutex::new(());

fn state_file(name: &str, up: bool) -> PathBuf {
    let file = test_support::scratch(name).join("state.json");
    let state = json!({
        "up": up,
        "current_context": "kind-dev",
        "contexts": [
            {"name": "kind-dev", "cluster": "kind", "namespace": "shop"},
            {"name": "prod", "cluster": "prod", "namespace": "default"},
        ],
        "namespaces": ["default", "shop"],
        "resources": [
            {
                "apiVersion": "v1",
                "kind": "Pod",
                "metadata": {"name": "web-0", "namespace": "shop"},
                "spec": {"containers": [{"name": "web"}]},
                "status": {
                    "phase": "Running",
                    "containerStatuses": [{
                        "ready": true,
                        "restartCount": 2,
                        "state": {"running": {}}
                    }]
                }
            },
            {
                "apiVersion": "v1",
                "kind": "Pod",
                "metadata": {"name": "db-0", "namespace": "shop"},
                "spec": {"containers": [{"name": "db"}]},
                "status": {
                    "phase": "Running",
                    "containerStatuses": [{
                        "ready": false,
                        "restartCount": 7,
                        "state": {"waiting": {"reason": "CrashLoopBackOff"}}
                    }]
                }
            },
            {
                "apiVersion": "apps/v1",
                "kind": "Deployment",
                "metadata": {"name": "web", "namespace": "shop"},
                "spec": {"replicas": 3},
                "status": {"readyReplicas": 2}
            },
            {
                "apiVersion": "v1",
                "kind": "Secret",
                "metadata": {"name": "db", "namespace": "shop"},
                "type": "Opaque",
                "data": {"password": "czNjcjN0", "user": "YWRtaW4="}
            }
        ],
        "logs": {"web-0": ["ready", "listening on 8080"]},
        "secret_values": {"db/password": "s3cr3t"},
        "diff": "--- a\n+++ b\n@@ -1 +1 @@\n-old\n+new\n",
    });
    std::fs::write(&file, state.to_string()).unwrap();
    file
}

fn cli(file: &PathBuf) -> Kubectl {
    unsafe { std::env::set_var("FAKE_KUBE_STATE", file) };
    Kubectl::new(FAKE)
}

fn calls(file: &Path) -> String {
    std::fs::read_to_string(format!("{}.calls", file.display())).unwrap_or_default()
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let start = Instant::now();
    while !done() {
        assert!(start.elapsed() < PATIENCE, "no {what} came");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn the_contexts_namespaces_and_resources_of_the_fake_are_read() {
    let _guard = ENV.lock().unwrap_or_else(|poison| poison.into_inner());
    let file = state_file("kube-read", true);
    let cli = cli(&file);

    let snapshot = take(&cli, Some("shop"));

    assert!(matches!(snapshot.cluster, ClusterState::Up { .. }));
    assert_eq!(snapshot.current_context, "kind-dev");
    assert_eq!(snapshot.contexts.len(), 2);
    assert_eq!(snapshot.namespaces, vec!["default", "shop"]);
    assert_eq!(snapshot.namespace, "shop");
    assert_eq!(snapshot.problem, None);
    let pod = snapshot
        .resources
        .iter()
        .find(|resource| resource.name == "web-0")
        .expect("the running pod");
    assert_eq!(pod.state, "Running");
    assert_eq!(pod.ready, "1/1");
    assert_eq!(pod.restarts, 2);
    let failing = snapshot
        .resources
        .iter()
        .find(|resource| resource.name == "db-0")
        .expect("the failing pod");
    assert_eq!(failing.state, "CrashLoopBackOff");
    let deployment = snapshot
        .resources
        .iter()
        .find(|resource| resource.kind == ResourceKind::Deployment)
        .expect("the deployment");
    assert_eq!(deployment.ready, "2/3");
    let secret = snapshot
        .resources
        .iter()
        .find(|resource| resource.kind == ResourceKind::Secret)
        .expect("the secret");
    assert_eq!(secret.keys, vec!["password".to_owned(), "user".to_owned()]);
    let report = secret.report();
    assert!(!report.contains("s3cr3t"));
    assert!(!report.contains("czNjcjN0"));
}

#[test]
fn a_context_switch_writes_the_kubeconfig_and_the_next_read_sees_it() {
    let _guard = ENV.lock().unwrap_or_else(|poison| poison.into_inner());
    let file = state_file("kube-context-switch", false);
    let cli = cli(&file);

    let snapshot = take(&cli, None);
    assert_eq!(snapshot.current_context, "kind-dev");
    assert!(
        snapshot
            .contexts
            .iter()
            .any(|context| context.name == "prod" && !context.current)
    );

    cli.use_context("prod").unwrap();

    let calls = calls(&file);
    assert!(
        calls.lines().any(|line| line == "config use-context prod"),
        "{calls}"
    );
    assert!(
        calls
            .lines()
            .any(|line| line == "config get-contexts -o name"),
        "{calls}"
    );
    assert!(
        calls.lines().any(|line| line == "config current-context"),
        "{calls}"
    );
    let snapshot = take(&cli, None);
    assert_eq!(snapshot.current_context, "prod");
    assert!(
        snapshot
            .contexts
            .iter()
            .any(|context| context.name == "prod" && context.current)
    );
    assert!(
        snapshot
            .contexts
            .iter()
            .any(|context| context.name == "kind-dev" && !context.current)
    );
}

#[test]
fn a_missing_cli_is_a_state_too() {
    let cli = Kubectl::new("C:/nowhere/kubectl.exe");

    assert_eq!(cli.state(), ClusterState::NoCli);
    assert_eq!(cli.calls(), 0);
    let snapshot = take(&cli, None);
    assert_eq!(snapshot.cluster, ClusterState::NoCli);
    assert!(snapshot.resources.is_empty());
}

#[test]
fn an_unreachable_cluster_says_why_and_has_no_resources() {
    let _guard = ENV.lock().unwrap_or_else(|poison| poison.into_inner());
    let file = state_file("kube-down", false);
    let cli = cli(&file);

    let snapshot = take(&cli, None);

    let ClusterState::Unreachable { reason } = &snapshot.cluster else {
        panic!("the cluster is down: {:?}", snapshot.cluster);
    };
    assert!(reason.contains("Unable to connect"), "{reason}");
    assert!(snapshot.resources.is_empty());
    assert!(snapshot.namespaces.is_empty());
    assert_eq!(snapshot.contexts.len(), 2);
}

#[test]
fn every_action_passes_the_arguments_of_the_cli() {
    let _guard = ENV.lock().unwrap_or_else(|poison| poison.into_inner());
    let file = state_file("kube-actions", true);
    let cli = cli(&file).with_namespace(Some("shop".into()));

    cli.delete(ResourceKind::Pod, "web-0").unwrap();
    cli.restart(ResourceKind::Deployment, "web").unwrap();
    cli.scale(ResourceKind::Deployment, "web", 4).unwrap();
    cli.apply_manifest("apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: demo\n")
        .unwrap();
    let diff = cli
        .diff_manifest("apiVersion: v1\nkind: ConfigMap\n")
        .unwrap();
    assert!(diff.contains("+new"), "{diff}");

    let calls = calls(&file);
    assert!(
        calls.contains("delete pods web-0 --namespace shop"),
        "{calls}"
    );
    assert!(
        calls.contains("rollout restart deployments/web --namespace shop"),
        "{calls}"
    );
    assert!(
        calls.contains("scale deployments/web --replicas 4 --namespace shop"),
        "{calls}"
    );
    let apply = calls
        .lines()
        .find(|line| line.starts_with("apply -f "))
        .expect("the apply call");
    let manifest = apply.trim_start_matches("apply -f ");
    assert!(
        !std::path::Path::new(manifest).exists(),
        "the temporary manifest went away"
    );
    assert!(
        calls.lines().any(|line| line.starts_with("diff -f ")),
        "{calls}"
    );
}

#[test]
fn a_secret_value_is_asked_for_by_its_key() {
    let _guard = ENV.lock().unwrap_or_else(|poison| poison.into_inner());
    let file = state_file("kube-secret", true);
    let cli = cli(&file).with_namespace(Some("shop".into()));

    assert_eq!(cli.secret_value("db", "password").unwrap(), "s3cr3t");

    let calls = calls(&file);
    assert!(calls.contains("get secret/db -o json"), "{calls}");
}

#[test]
fn a_port_forward_ends_with_its_value() {
    let _guard = ENV.lock().unwrap_or_else(|poison| poison.into_inner());
    let file = state_file("kube-forward", true);
    let cli = cli(&file).with_namespace(Some("shop".into()));
    let port = TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port();

    let forward = cli
        .port_forward("pod/web-0", &format!("{port}:80"))
        .unwrap();
    wait_until("the forwarded port", || {
        TcpStream::connect(("127.0.0.1", port)).is_ok()
    });

    drop(forward);

    wait_until("the closed port", || {
        TcpStream::connect(("127.0.0.1", port)).is_err()
    });
}
