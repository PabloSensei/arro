//! T-0243: the Kubernetes node of the Services window with a stand-in `kubectl`. The
//! resources and their state, the log of a Pod, Delete with its question, Scale, a Secret
//! that stays hidden until Show Value, Apply from the gutter of a YAML document and the
//! line of a machine without `kubectl`.
//!
//! cargo test -p panels window_tests_kubernetes

#![cfg(windows)]

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::{SERVICES_TOOL, Workspace};

use crate::docker::{ActiveDocker, DockerPanel};
use crate::kubernetes::TestCli;
use crate::window_tests::{
    click, click_gutter, draw, open_path, open_window, sample_project, scratch, type_text,
};
use crate::window_tests_services::{menu_labels, right_click};
use crate::window_tests_ui::{ENV, wait_for};

/// The cluster of the stand-in: a running Pod, one in CrashLoopBackOff, a Deployment that
/// is not fully ready and a Secret with two keys.
const RESOURCES: &str = r#"{"apiVersion":"v1","kind":"List","items":[
{"apiVersion":"v1","kind":"Pod","metadata":{"name":"web-0","namespace":"shop"},"spec":{"containers":[{"name":"web"}]},"status":{"phase":"Running","containerStatuses":[{"ready":true,"restartCount":2,"state":{"running":{}}}]}},
{"apiVersion":"v1","kind":"Pod","metadata":{"name":"db-0","namespace":"shop"},"spec":{"containers":[{"name":"db"}]},"status":{"phase":"Running","containerStatuses":[{"ready":false,"restartCount":7,"state":{"waiting":{"reason":"CrashLoopBackOff"}}}]}},
{"apiVersion":"apps/v1","kind":"Deployment","metadata":{"name":"web","namespace":"shop"},"spec":{"replicas":3},"status":{"readyReplicas":2}},
{"apiVersion":"v1","kind":"Secret","metadata":{"name":"db","namespace":"shop"},"type":"Opaque","data":{"password":"czNjcjN0","user":"YWRtaW4="}}
]}"#;

/// A `kubectl.cmd` that answers the calls of the node from the constants above and writes
/// every call to `calls.txt` next to itself.
pub(crate) fn fake_kubectl(name: &str) -> (PathBuf, PathBuf) {
    let folder = scratch(name);
    let script = folder.join("kubectl.cmd");
    let calls = folder.join("calls.txt");
    let resources = folder.join("resources.json");
    fs::write(&resources, RESOURCES).unwrap();
    let text = format!(
        r#"@echo off
setlocal EnableExtensions
set "calls={calls}"
:record
echo %~1 %~2 %~3 %~4 >>"%calls%"
:recorded
if "%~1"=="--context" shift
if "%~1"=="--context" shift
if "%~1"=="--kubeconfig" shift
if "%~1"=="--kubeconfig" shift
if "%~1"=="version" goto version
if "%~1"=="config" goto config
if "%~1"=="get" goto get
if "%~1"=="delete" goto done
if "%~1"=="rollout" goto done
if "%~1"=="scale" goto scale
if "%~1"=="apply" goto done
if "%~1"=="diff" goto diff
if "%~1"=="logs" goto logs
if "%~1"=="describe" goto describe
if "%~1"=="port-forward" goto done
if "%~1"=="exec" goto done
goto unknown
:version
echo {{"clientVersion":{{"gitVersion":"v1.31.0"}},"serverVersion":{{"gitVersion":"v1.30.2"}}}}
goto end
:config
echo {{"current-context":"kind-dev","contexts":[{{"name":"kind-dev","context":{{"cluster":"kind","namespace":"shop"}}}},{{"name":"prod","context":{{"cluster":"prod","namespace":"default"}}}}]}}
goto end
:get
if "%~2"=="namespaces" goto namespaces
if "%~2"=="secret/db" goto secret
if "%~2"=="pods,deployments,statefulsets,services,configmaps,secrets,ingresses,jobs,cronjobs" goto resources
echo apiVersion: v1
echo kind: Resource
echo metadata:
echo   name: demo
goto end
:namespaces
echo {{"apiVersion":"v1","kind":"List","items":[{{"metadata":{{"name":"default"}}}},{{"metadata":{{"name":"shop"}}}}]}}
goto end
:resources
type "{resources}"
goto end
:secret
echo {{"kind":"Secret","data":{{"password":"czNjcjN0","user":"YWRtaW4="}}}}
goto end
:diff
echo --- a
echo +++ b
exit /b 1
:logs
echo ready
echo listening on 8080
ping -n 2 127.0.0.1 >nul
echo tick 2
ping -n 2 127.0.0.1 >nul
echo tick 3
goto end
:scale
echo scaled
goto end
:describe
echo Name:         web-0
echo Namespace:    shop
goto end
:done
echo ok
goto end
:unknown
exit /b 2
:end
endlocal
"#,
        calls = calls.display(),
        resources = resources.display(),
    );
    fs::write(&script, text.replace('\n', "\r\n")).unwrap();
    (script, calls)
}

/// Opens the window on the stand-in `kubectl`, shows Services and waits for the resources
/// of the cluster.
fn open_kubernetes(
    cx: &mut TestAppContext,
    name: &str,
) -> (
    WindowHandle<Workspace>,
    gpui::Entity<DockerPanel>,
    PathBuf,
    PathBuf,
) {
    let root = sample_project(name);
    let (script, calls) = fake_kubectl(&format!("{name}-cli"));
    let window = open_with_cli(cx, root, &script);
    wait_for(cx, "the resources of the stand-in cluster", |cx| {
        let docker = cx.update(|cx| cx.global::<ActiveDocker>().0.clone());
        docker.read_with(cx, |panel, _| {
            panel
                .report()
                .iter()
                .any(|line| line.contains("Pods web-0"))
        })
    });
    draw(cx, window);
    // The Docker sections close, so the Kubernetes node of the tree is on screen.
    click(cx, window, "docker-toolbar-collapse");
    draw(cx, window);
    let docker = cx.update(|cx| cx.global::<ActiveDocker>().0.clone());
    (window, docker, calls, script)
}

fn open_with_cli(cx: &mut TestAppContext, root: PathBuf, script: &Path) -> WindowHandle<Workspace> {
    let missing_docker = script
        .parent()
        .map(|folder| folder.join("docker-missing.exe"))
        .unwrap_or_default();
    cx.update(|cx| cx.set_global(TestCli(script.to_path_buf())));
    let _guard = ENV.lock().unwrap_or_else(|poison| poison.into_inner());
    // SAFETY: the window reads the variable at creation, under `ENV`. The stand-in
    // Docker keeps the tree short, so the Kubernetes rows are on screen.
    unsafe { std::env::set_var("IDE_DOCKER_CLI", missing_docker) };
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
    window
}

fn calls_of(path: &Path) -> String {
    for _ in 0..20 {
        match fs::read_to_string(path) {
            Ok(text) => return text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return String::new(),
            Err(_) => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    String::new()
}

fn click_menu(cx: &mut TestAppContext, window: WindowHandle<Workspace>, label: &str) {
    let labels = menu_labels(cx, window);
    let index = labels
        .iter()
        .position(|row| row == label)
        .unwrap_or_else(|| panic!("no `{label}` in {labels:?}"));
    let selector: &'static str = Box::leak(format!("menu-{index}").into_boxed_str());
    click(cx, window, selector);
}

/// Scrolls the tree of Services by a number of rows; a negative number goes down.
fn scroll_tree(cx: &mut TestAppContext, window: WindowHandle<Workspace>, rows: f32) {
    let bounds = crate::window_tests::bounds_of(cx, window, "scroll-area-docker-tree");
    let mut visual = ui_kit::gpui::VisualTestContext::from_window(window.into(), cx);
    visual.simulate_event(ui_kit::gpui::ScrollEvent {
        position: bounds.center(),
        delta: ui_kit::gpui::ScrollDelta::Lines(ui_kit::point(0., -rows)),
        modifiers: ui_kit::Modifiers::default(),
        ..Default::default()
    });
    cx.run_until_parked();
    draw(cx, window);
}

/// Whether a row of the tree is on screen with its middle inside the visible part.
fn row_in_view(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &'static str,
) -> Option<bool> {
    let mut visual = ui_kit::gpui::VisualTestContext::from_window(window.into(), cx);
    visual.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear(cx);
    });
    let row = visual.debug_bounds(selector)?;
    let tree = visual.debug_bounds("scroll-area-docker-tree")?;
    Some(tree.contains(&row.center()))
}

/// Brings a row of the Services tree that stands below the visible part into view.
fn reveal(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) {
    for _ in 0..80 {
        match row_in_view(cx, window, selector) {
            Some(true) => return,
            Some(false) => scroll_tree(cx, window, 2.),
            None => scroll_tree(cx, window, 4.),
        }
    }
    panic!("no element {selector} in the tree");
}

#[test]
fn the_kubernetes_settings_are_under_tools() {
    use crate::settings_catalog::{SettingsPage, find};

    assert_eq!(SettingsPage::Kubernetes.parent(), Some(SettingsPage::Tools));
    assert_eq!(SettingsPage::Kubernetes.depth(), 1);
    for id in ["kubectl_path", "kubeconfig_path"] {
        assert_eq!(
            find(id).map(|setting| setting.page),
            Some(SettingsPage::Kubernetes),
            "{id}"
        );
    }
}

#[ui_kit::gpui::test]
fn the_node_lists_contexts_namespaces_resources_and_their_state(cx: &mut TestAppContext) {
    let (window, docker, _, _) = open_kubernetes(cx, "kube-resources");

    let report = docker.read_with(cx, |panel, _| panel.report());
    for wanted in [
        "kube cluster up v1.30.2",
        "kube context kind-dev true",
        "kube namespace shop",
        "Pods web-0 Running 1/1 restarts=2",
        "Pods db-0 CrashLoopBackOff 0/1 restarts=7",
        "Deployments web Pending 2/3",
        "key=password",
        "key=user",
    ] {
        assert!(
            report.iter().any(|line| line.contains(wanted)),
            "the report has `{wanted}`: {report:?}"
        );
    }
    assert!(
        !report.iter().any(|line| line.contains("s3cr3t")),
        "a secret value is not in the report: {report:?}"
    );

    // The tree shows the contexts, the namespaces and the kinds of the cluster.
    for selector in [
        "kube",
        "kube-contexts",
        "kube-context-kind-dev",
        "kube-namespaces",
        "kube-namespace-shop",
    ] {
        crate::window_tests::bounds_of(cx, window, selector);
    }

    // A Pod shows its live log in the console of the panel.
    reveal(cx, window, "kube-resource-pods-web-0");
    crate::window_tests::bounds_of(cx, window, "kube-kind-pods");
    crate::window_tests::bounds_of(cx, window, "kube-resource-pods-web-0");
    click(cx, window, "kube-resource-pods-web-0");
    draw(cx, window);
    wait_for(cx, "the log of the Pod", |cx| {
        docker.read_with(cx, |panel, _| {
            panel.report().iter().any(|line| line == "kube log ready")
        })
    });
    crate::window_tests::bounds_of(cx, window, "scroll-area-kube-log");
}

#[ui_kit::gpui::test]
fn a_hidden_tool_window_stops_the_watch(cx: &mut TestAppContext) {
    let (window, docker, _, _) = open_kubernetes(cx, "kube-idle");
    assert!(docker.read_with(cx, |panel, _| panel.kube.watching()));

    window
        .update(cx, |workspace, window, cx| {
            workspace.toggle_tool(SERVICES_TOOL, window, cx)
        })
        .unwrap();
    draw(cx, window);

    wait_for(cx, "the closed watch", |cx| {
        docker.read_with(cx, |panel, _| !panel.kube.watching())
    });
}

#[ui_kit::gpui::test]
fn delete_asks_first_and_then_reaches_the_cli(cx: &mut TestAppContext) {
    let (window, _, calls, _) = open_kubernetes(cx, "kube-delete");

    reveal(cx, window, "kube-resource-pods-web-0");
    right_click(cx, window, "kube-resource-pods-web-0");
    let labels = menu_labels(cx, window);
    assert!(labels.iter().any(|label| label == "Delete"), "{labels:?}");
    click_menu(cx, window, "Delete");

    wait_for(cx, "the delete question", |cx| {
        crate::window_tests_git::has_selector(cx, window, "prompt")
    });
    click(cx, window, "prompt");
    crate::window_tests::press(cx, window, "enter");

    wait_for(cx, "the delete call of the stand-in kubectl", |_| {
        calls_of(&calls).contains("delete pods web-0")
    });
}

#[ui_kit::gpui::test]
fn scale_asks_for_the_number_and_reaches_the_cli(cx: &mut TestAppContext) {
    let (window, _, calls, _) = open_kubernetes(cx, "kube-scale");

    reveal(cx, window, "kube-resource-deployments-web");
    click(cx, window, "kube-resource-deployments-web");
    draw(cx, window);
    click(cx, window, "kube-action-scale");
    wait_for(cx, "the scale question", |cx| {
        crate::window_tests_git::has_selector(cx, window, "prompt")
    });
    crate::window_tests::press(cx, window, "backspace");
    type_text(cx, window, "3");
    crate::window_tests::press(cx, window, "enter");

    wait_for(cx, "the scale call of the stand-in kubectl", |_| {
        calls_of(&calls).contains("scale deployments/web --replicas 3")
    });
}

#[ui_kit::gpui::test]
fn a_secret_stays_hidden_until_show_value(cx: &mut TestAppContext) {
    let (window, docker, calls, _) = open_kubernetes(cx, "kube-secret");

    reveal(cx, window, "kube-resource-secrets-db");
    click(cx, window, "kube-resource-secrets-db");
    draw(cx, window);
    let report = docker.read_with(cx, |panel, _| panel.report());
    assert!(
        report.iter().any(|line| line.contains("key=password")),
        "{report:?}"
    );
    assert!(
        !report.iter().any(|line| line.contains("s3cr3t")),
        "the value is hidden before Show Value: {report:?}"
    );
    assert_eq!(
        docker.read_with(cx, |panel, _| panel
            .kube
            .revealed_value()
            .map(str::to_owned)),
        None
    );

    reveal(cx, window, "kube-key-db-password");
    right_click(cx, window, "kube-key-db-password");
    let labels = menu_labels(cx, window);
    assert!(
        labels.iter().any(|label| label == "Show Value"),
        "{labels:?}"
    );
    click_menu(cx, window, "Show Value");

    wait_for(cx, "the value of the Secret", |cx| {
        docker.read_with(cx, |panel, _| panel.kube.revealed_value() == Some("s3cr3t"))
    });
    wait_for(cx, "the value call of the stand-in kubectl", |_| {
        calls_of(&calls).contains("get secret/db -o json")
    });
    let report = docker.read_with(cx, |panel, _| panel.report());
    assert!(
        !report.iter().any(|line| line.contains("s3cr3t")),
        "the report still hides the value: {report:?}"
    );
}

#[ui_kit::gpui::test]
fn a_machine_without_kubectl_shows_a_line_and_no_error(cx: &mut TestAppContext) {
    let root = sample_project("kube-missing");
    let folder = scratch("kube-missing-cli");
    let missing = folder.join("kubectl.exe");
    let window = open_with_cli(cx, root, &missing);

    wait_for(cx, "the line of a missing kubectl", |cx| {
        let docker = cx.update(|cx| cx.global::<ActiveDocker>().0.clone());
        docker.read_with(cx, |panel, _| {
            panel
                .report()
                .iter()
                .any(|line| line == "kube cluster no-cli")
        })
    });
    let docker = cx.update(|cx| cx.global::<ActiveDocker>().0.clone());
    let report = docker.read_with(cx, |panel, _| panel.report());
    assert!(
        !report.iter().any(|line| line.contains("unreachable")),
        "{report:?}"
    );
    assert_eq!(docker.read_with(cx, |panel, _| panel.kube.cli_calls()), 0);
    reveal(cx, window, "kube");
}

#[ui_kit::gpui::test]
fn apply_from_the_gutter_of_a_yaml_document_reaches_the_cluster(cx: &mut TestAppContext) {
    let root = sample_project("kube-gutter");
    let path = root.join("deploy.yaml");
    fs::write(
        &path,
        "apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: web\n  namespace: shop\n---\napiVersion: v1\nkind: Service\nmetadata:\n  name: web\n",
    )
    .unwrap();
    let (script, calls) = fake_kubectl("kube-gutter-cli");
    let window = open_with_cli(cx, root, &script);
    open_path(cx, window, &path);
    wait_for(cx, "the badges of the two documents", |cx| {
        let view = crate::window_tests::editor_of(cx, window, &path);
        view.read_with(cx, |view, _| view.action_marks().len() == 2)
    });

    let view = crate::window_tests::editor_of(cx, window, &path);
    let lines: Vec<u32> = view.read_with(cx, |view, _| {
        view.action_marks().iter().map(|mark| mark.line).collect()
    });
    assert_eq!(lines, vec![0, 6]);

    click_gutter(cx, window, 0, 0);
    wait_for(cx, "the menu of the document", |cx| {
        menu_labels(cx, window)
            .iter()
            .any(|label| label == "Apply to Cluster")
    });
    click_menu(cx, window, "Apply to Cluster");

    wait_for(cx, "the apply call of the stand-in kubectl", |_| {
        calls_of(&calls)
            .lines()
            .any(|line| line.starts_with("apply -f "))
    });

    // The menu of the second document offers the same actions, and Delete asks first.
    click_gutter(cx, window, 0, 6);
    wait_for(cx, "the menu of the second document", |cx| {
        menu_labels(cx, window)
            .iter()
            .any(|label| label == "Delete from Cluster")
    });
    click_menu(cx, window, "Delete from Cluster");
    wait_for(cx, "the question of Delete from Cluster", |cx| {
        crate::window_tests_git::has_selector(cx, window, "prompt")
    });
    crate::window_tests::press(cx, window, "escape");
}
