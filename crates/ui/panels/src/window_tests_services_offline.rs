//! T-0424: the Services window while Kubernetes does not answer and Docker is not running.
//! The node of an unreachable cluster has no branches and names its reason in a tooltip; a
//! chosen node names its reason on the right with a Retry button; the right side names a
//! container only when there is one; and the branches come back once the cluster answers.
//!
//! cargo test -p panels window_tests_services_offline

#![cfg(windows)]

use std::fs;
use std::path::{Path, PathBuf};

use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::{SERVICES_TOOL, Workspace};

use crate::docker::{ActiveDocker, DockerPanel};
use crate::kubernetes::{Node, TestCli};
use crate::window_tests::{
    bounds_of, click, draw, open_window, sample_project, scratch, wait_for_process,
    wait_for_settled_bounds,
};
use crate::window_tests_git::has_selector;
use crate::window_tests_kubernetes::fake_kubectl;
use crate::window_tests_services::fake_docker;
use crate::window_tests_tooltips::hover;
use crate::window_tests_ui::ENV;

/// What the stand-in cluster says while it does not answer. A batch file echoes the text,
/// so it has no `&`, `|`, `<`, `>`, `(` or `)`.
const CLUSTER_REASON: &str =
    "Unable to connect to the server: dial tcp 127.0.0.1:6443: connection refused";
/// What the stand-in Docker says while its daemon is not running.
const DOCKER_REASON: &str =
    "failed to connect to the docker API at npipe:////./pipe/dockerDesktopLinuxEngine";

/// A `kubectl.cmd` whose cluster does not answer while the flag file is missing. Once the
/// flag exists, it answers like the stand-in of the Kubernetes tests.
fn kubectl_until(name: &str, flag: &Path) -> PathBuf {
    let (answering, _) = fake_kubectl(&format!("{name}-answering"));
    let script = scratch(name).join("kubectl.cmd");
    let text = format!(
        "@echo off\r\nif not exist \"{flag}\" goto down\r\ncall \"{answering}\" %*\r\nexit /b\r\n:down\r\necho {CLUSTER_REASON} 1>&2\r\nexit /b 1\r\n",
        flag = flag.display(),
        answering = answering.display(),
    );
    fs::write(&script, text).unwrap();
    script
}

/// A `docker.cmd` whose daemon does not run: every call fails with the reason.
fn stopped_docker(name: &str) -> PathBuf {
    let script = scratch(name).join("docker.cmd");
    let text = format!("@echo off\r\necho {DOCKER_REASON} 1>&2\r\nexit /b 1\r\n");
    fs::write(&script, text).unwrap();
    script
}

/// Opens the window on the stand-ins, shows Services and waits until the panel has the
/// first answer of Docker and of the cluster.
fn open_offline(
    cx: &mut TestAppContext,
    name: &str,
    docker_cli: &Path,
    kubectl: &Path,
) -> (WindowHandle<Workspace>, gpui::Entity<DockerPanel>) {
    let root = sample_project(name);
    cx.update(|cx| cx.set_global(TestCli(kubectl.to_path_buf())));
    let _guard = ENV.lock().unwrap_or_else(|poison| poison.into_inner());
    // SAFETY: the window reads the variable at creation, under `ENV`.
    unsafe { std::env::set_var("IDE_DOCKER_CLI", docker_cli) };
    let window = open_window(cx, root);
    // SAFETY: the panel has read the variable already.
    unsafe { std::env::remove_var("IDE_DOCKER_CLI") };
    drop(_guard);
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(SERVICES_TOOL, window, cx)
        })
        .unwrap();
    draw(cx, window);
    let panel = cx.update(|cx| cx.global::<ActiveDocker>().0.clone());
    wait_for_process(cx, |cx| {
        panel.read_with(cx, |panel, _| {
            let report = panel.report();
            report.iter().any(|line| line.starts_with("docker daemon"))
                && report.iter().any(|line| line.starts_with("kube cluster"))
        })
    });
    // The tool window slides into its dock over a few frames: the rows are hovered only
    // once they stay in place.
    wait_for_settled_bounds(cx, window, "kube");
    // A tooltip that shows first warms the tooltips of the window: the first row hovered
    // after it shows its own tooltip at once, where a row hovered cold shows none.
    hover(cx, window, "docker-toolbar-collapse");
    draw(cx, window);
    (window, panel)
}

/// Hovers a row until its tooltip says `wanted`. Under the load of the whole suite a row
/// can still move while the services answer, and the tooltip of the warm-up hover stays
/// on screen; the row is hovered again from the toolbar once it stands still.
fn hover_row(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &'static str,
    wanted: &str,
) -> String {
    let mut shown = String::new();
    for _ in 0..5 {
        wait_for_settled_bounds(cx, window, selector);
        shown = hover(cx, window, selector).0;
        if shown.contains(wanted) {
            break;
        }
        hover(cx, window, "docker-toolbar-collapse");
    }
    shown
}

#[ui_kit::gpui::test]
fn an_unreachable_cluster_has_no_branches_and_names_its_reason_in_a_tooltip(
    cx: &mut TestAppContext,
) {
    let flag = scratch("offline-cluster-flag").join("answering");
    let kubectl = kubectl_until("offline-cluster-kube", &flag);
    let docker_cli = stopped_docker("offline-cluster-docker");
    let (window, panel) = open_offline(cx, "offline-cluster", &docker_cli, &kubectl);

    // The node is on screen, and none of the branches of a cluster that answers is.
    bounds_of(cx, window, "kube");
    for selector in [
        "kube-contexts",
        "kube-namespaces",
        "kube-kind-pods",
        "kube-kind-deployments",
    ] {
        assert!(
            !has_selector(cx, window, selector),
            "{selector} is not in the tree of an unreachable cluster"
        );
    }

    // The reason is in the tooltip of the row.
    let shown = hover_row(cx, window, "kube", CLUSTER_REASON);
    assert!(
        shown.contains(CLUSTER_REASON),
        "the tooltip names the reason: {shown}"
    );

    // Choosing the node names the reason on the right, with a Retry button.
    click(cx, window, "kube");
    draw(cx, window);
    let selected = panel.read_with(cx, |panel, _| panel.kube.selected().cloned());
    assert_eq!(selected, Some(Node::Root));
    let problem = panel.read_with(cx, |panel, _| panel.kube.problem());
    assert_eq!(problem.as_deref(), Some(CLUSTER_REASON));
    bounds_of(cx, window, "services-problem");
    let (label, _) = hover(cx, window, "kube-action-refresh");
    assert_eq!(label, "Retry");
}

#[ui_kit::gpui::test]
fn a_stopped_docker_names_its_reason_on_the_right_with_a_retry(cx: &mut TestAppContext) {
    let flag = scratch("offline-docker-flag").join("answering");
    let kubectl = kubectl_until("offline-docker-kube", &flag);
    let docker_cli = stopped_docker("offline-docker-cli");
    let (window, panel) = open_offline(cx, "offline-docker", &docker_cli, &kubectl);

    // The row is short; the reason is in its tooltip.
    let shown = hover_row(cx, window, "docker-daemon", DOCKER_REASON);
    assert!(
        shown.contains(DOCKER_REASON),
        "the tooltip names the reason: {shown}"
    );

    // Choosing the node names the reason on the right, with a Retry button.
    click(cx, window, "docker-daemon");
    draw(cx, window);
    let (title, hint) = panel.read_with(cx, |panel, _| panel.header_text());
    assert_eq!(title, "Docker is not running");
    assert_eq!(hint, "");
    bounds_of(cx, window, "services-problem");
    let shown = panel.read_with(cx, |panel, _| panel.placeholder_text());
    assert_eq!(shown, DOCKER_REASON);
    let (label, _) = hover(cx, window, "docker-action-retry");
    assert_eq!(label, "Retry");

    // Retry asks Docker again; the reason stays while the daemon does not run.
    click(cx, window, "docker-action-retry");
    draw(cx, window);
    let shown = panel.read_with(cx, |panel, _| panel.placeholder_text());
    assert_eq!(shown, DOCKER_REASON);
}

#[ui_kit::gpui::test]
fn without_containers_the_right_side_names_no_container(cx: &mut TestAppContext) {
    let flag = scratch("offline-empty-flag").join("answering");
    let kubectl = kubectl_until("offline-empty-kube", &flag);
    let docker_cli = stopped_docker("offline-empty-cli");
    let (_, panel) = open_offline(cx, "offline-empty", &docker_cli, &kubectl);

    let shown = panel.read_with(cx, |panel, _| panel.placeholder_text());
    assert_eq!(shown, "Select a service to see its details");
}

#[ui_kit::gpui::test]
fn with_containers_the_right_side_names_a_container(cx: &mut TestAppContext) {
    let flag = scratch("offline-containers-flag").join("answering");
    let kubectl = kubectl_until("offline-containers-kube", &flag);
    let (docker_cli, _) = fake_docker("offline-containers-cli");
    let (window, panel) = open_offline(cx, "offline-containers", &docker_cli, &kubectl);
    wait_for_process(cx, |cx| {
        panel.read_with(cx, |panel, _| {
            panel
                .report()
                .iter()
                .any(|line| line.starts_with("docker container petclinic-web-1"))
        })
    });
    draw(cx, window);

    let shown = panel.read_with(cx, |panel, _| panel.placeholder_text());
    assert_eq!(shown, "Select a container or a compose project");
}

#[ui_kit::gpui::test]
fn the_branches_of_the_cluster_come_back_when_it_answers(cx: &mut TestAppContext) {
    let flag = scratch("offline-back-flag").join("answering");
    let kubectl = kubectl_until("offline-back-kube", &flag);
    let docker_cli = stopped_docker("offline-back-cli");
    let (window, panel) = open_offline(cx, "offline-back", &docker_cli, &kubectl);
    assert!(!has_selector(cx, window, "kube-contexts"));

    // The cluster starts to answer; Retry on its node asks it at once.
    fs::write(&flag, "").unwrap();
    click(cx, window, "kube");
    draw(cx, window);
    click(cx, window, "kube-action-refresh");
    wait_for_process(cx, |cx| has_selector(cx, window, "kube-contexts"));
    assert!(has_selector(cx, window, "kube-kind-pods"));
    let problem = panel.read_with(cx, |panel, _| panel.kube.problem());
    assert_eq!(problem, None);
}
