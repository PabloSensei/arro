//! T-0445: the contexts of the kubeconfig in the Services window while the cluster does not
//! answer. The tree lists every context, marks the current one and names the reason at it; a
//! "Use Context" of another context writes the kubeconfig in the background and the tree
//! follows; a kubeconfig with one context keeps the view of the offline window.
//!
//! cargo test -p panels window_tests_services_contexts

#![cfg(windows)]

use std::fs;
use std::path::PathBuf;

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
use crate::window_tests_journal::journal_writer;
use crate::window_tests_services::{menu_labels, right_click};
use crate::window_tests_tooltips::hover;
use crate::window_tests_ui::ENV;

/// What the stand-in cluster says while it does not answer. A batch file echoes the text,
/// so it has no `&`, `|`, `<`, `>` or `(`.
const CLUSTER_REASON: &str =
    "Unable to connect to the server: dial tcp 127.0.0.1:6443: connection refused";
/// What the stand-in Docker says while its daemon is not running.
const DOCKER_REASON: &str =
    "failed to connect to the docker API at npipe:////./pipe/dockerDesktopLinuxEngine";

/// A `kubectl.cmd` whose cluster does not answer but whose kubeconfig does: `version` and
/// every call that would reach the cluster fail, while the `config` calls answer with the
/// given contexts. `config use-context` writes the chosen name into a file of its own, so
/// the next `config current-context` sees it.
fn dead_cluster_kubectl(name: &str, contexts: &[&str]) -> (PathBuf, PathBuf) {
    let folder = scratch(name);
    let script = folder.join("kubectl.cmd");
    let journal = folder.join("calls");
    let current = folder.join("current");
    let writer = journal_writer(&journal);
    let names: String = contexts
        .iter()
        .map(|context| format!("echo {context}\r\n"))
        .collect();
    let first = contexts.first().copied().unwrap_or("kind-dev");
    let text = format!(
        "@echo off\r\nsetlocal EnableExtensions\r\n{writer}\r\n\
         if \"%~1\"==\"--context\" shift\r\n\
         if \"%~1\"==\"--context\" shift\r\n\
         if \"%~1\"==\"--kubeconfig\" shift\r\n\
         if \"%~1\"==\"--kubeconfig\" shift\r\n\
         if \"%~1\"==\"version\" goto version\r\n\
         if \"%~1\"==\"config\" goto config\r\n\
         if \"%~1\"==\"get\" goto down\r\n\
         goto down\r\n\
         :version\r\n\
         echo {reason} 1>&2\r\n\
         exit /b 1\r\n\
         :config\r\n\
         if \"%~2\"==\"get-contexts\" goto names\r\n\
         if \"%~2\"==\"current-context\" goto current\r\n\
         if \"%~2\"==\"use-context\" goto use\r\n\
         if \"%~2\"==\"view\" goto view\r\n\
         goto down\r\n\
         :names\r\n\
         {names}goto end\r\n\
         :current\r\n\
         if exist \"{file}\" goto current_file\r\n\
         echo {first}\r\n\
         goto end\r\n\
         :current_file\r\n\
         type \"{file}\"\r\n\
         goto end\r\n\
         :use\r\n\
         >\"{file}\" echo %~3\r\n\
         echo Switched to context \"%~3\".\r\n\
         goto end\r\n\
         :view\r\n\
         echo {{}}\r\n\
         goto end\r\n\
         :down\r\n\
         echo {reason} 1>&2\r\n\
         exit /b 1\r\n\
         :end\r\n\
         endlocal\r\n",
        writer = writer,
        names = names,
        file = current.display(),
        first = first,
        reason = CLUSTER_REASON,
    );
    fs::write(&script, text).unwrap();
    (script, journal)
}

/// A `docker.cmd` whose daemon does not run: one short row in the tree, so the rows of the
/// cluster stay on screen.
fn stopped_docker(name: &str) -> PathBuf {
    let script = scratch(name).join("docker.cmd");
    let text = format!("@echo off\r\necho {DOCKER_REASON} 1>&2\r\nexit /b 1\r\n");
    fs::write(&script, text).unwrap();
    script
}

/// Opens the window on the stand-ins, shows Services and waits until the panel has the
/// first answer of Docker and of the cluster.
fn open_contexts(
    cx: &mut TestAppContext,
    name: &str,
    contexts: &[&str],
) -> (WindowHandle<Workspace>, gpui::Entity<DockerPanel>, PathBuf) {
    let root = sample_project(name);
    let (kubectl, journal) = dead_cluster_kubectl(&format!("{name}-kube"), contexts);
    let docker_cli = stopped_docker(&format!("{name}-docker"));
    cx.update(|cx| cx.set_global(TestCli(kubectl)));
    let _guard = ENV.lock().unwrap_or_else(|poison| poison.into_inner());
    // SAFETY: the window reads the variable at creation, under `ENV`.
    unsafe { std::env::set_var("IDE_DOCKER_CLI", &docker_cli) };
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
                && report
                    .iter()
                    .any(|line| line.starts_with("kube cluster unreachable"))
        })
    });
    wait_for_settled_bounds(cx, window, "kube");
    // A tooltip that shows first warms the tooltips of the window: the first row hovered
    // after it shows its own tooltip at once, where a row hovered cold shows none.
    hover(cx, window, "docker-toolbar-collapse");
    draw(cx, window);
    (window, panel, journal)
}

/// The debug selector of the mark of the current context.
fn current_mark(name: &str) -> &'static str {
    Box::leak(format!("kube-context-{name}-current").into_boxed_str())
}

/// Hovers a row until its tooltip says `wanted`, the way the offline tests do.
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

fn click_menu(cx: &mut TestAppContext, window: WindowHandle<Workspace>, label: &str) {
    let labels = menu_labels(cx, window);
    let index = labels
        .iter()
        .position(|row| row == label)
        .unwrap_or_else(|| panic!("no `{label}` in {labels:?}"));
    let selector: &'static str = Box::leak(format!("menu-{index}").into_boxed_str());
    click(cx, window, selector);
}

fn report_has(panel: &gpui::Entity<DockerPanel>, cx: &mut TestAppContext, line: &str) -> bool {
    panel.read_with(cx, |panel, _| panel.report().iter().any(|row| row == line))
}

#[ui_kit::gpui::test]
fn a_dead_cluster_lists_its_contexts_and_names_the_reason_at_the_current_one(
    cx: &mut TestAppContext,
) {
    let (window, panel, _) = open_contexts(cx, "contexts-two", &["kind-dev", "prod"]);

    // Both contexts of the kubeconfig are on screen while the cluster does not answer.
    for selector in [
        "kube",
        "kube-contexts",
        "kube-context-kind-dev",
        "kube-context-prod",
    ] {
        bounds_of(cx, window, selector);
    }
    // The current one carries the mark; the other does not.
    assert!(
        has_selector(cx, window, current_mark("kind-dev")),
        "the current context is marked"
    );
    assert!(
        !has_selector(cx, window, current_mark("prod")),
        "only the current context is marked"
    );
    // The resources of a dead cluster stay out of the tree.
    for selector in ["kube-namespaces", "kube-kind-pods", "kube-kind-deployments"] {
        assert!(
            !has_selector(cx, window, selector),
            "{selector} is not in the tree of an unreachable cluster"
        );
    }

    // The reason is in the tooltip of the current context.
    let shown = hover_row(cx, window, "kube-context-kind-dev", CLUSTER_REASON);
    assert!(
        shown.contains(CLUSTER_REASON),
        "the tooltip of the current context names the reason: {shown}"
    );

    // Choosing it names the reason on the right, with a Retry button.
    click(cx, window, "kube-context-kind-dev");
    draw(cx, window);
    let selected = panel.read_with(cx, |panel, _| panel.kube.selected().cloned());
    assert_eq!(selected, Some(Node::Context("kind-dev".to_owned())));
    let problem = panel.read_with(cx, |panel, _| panel.kube.problem());
    assert_eq!(problem.as_deref(), Some(CLUSTER_REASON));
    bounds_of(cx, window, "services-problem");
    let (label, _) = hover(cx, window, "kube-action-refresh");
    assert_eq!(label, "Retry");
}

#[ui_kit::gpui::test]
fn use_context_writes_the_kubeconfig_and_the_tree_follows(cx: &mut TestAppContext) {
    let (window, panel, journal) = open_contexts(cx, "contexts-use", &["kind-dev", "prod"]);

    // The menu of another context offers Use Context.
    right_click(cx, window, "kube-context-prod");
    let labels = menu_labels(cx, window);
    assert!(
        labels.iter().any(|label| label == "Use Context"),
        "{labels:?}"
    );
    click_menu(cx, window, "Use Context");

    // The write is in the background: the tree follows with the next snapshot.
    wait_for_process(cx, |cx| report_has(&panel, cx, "kube context prod true"));
    draw(cx, window);
    assert!(
        has_selector(cx, window, current_mark("prod")),
        "the mark moved to the chosen context"
    );
    assert!(
        !has_selector(cx, window, current_mark("kind-dev")),
        "the old context is no longer marked"
    );
    // The reason is at the current context, which is the chosen one now.
    let shown = hover_row(cx, window, "kube-context-prod", CLUSTER_REASON);
    assert!(
        shown.contains(CLUSTER_REASON),
        "the reason moved with the current context: {shown}"
    );

    let journal = crate::window_tests_kubernetes::journal_of(&journal);
    assert!(
        journal
            .lines()
            .any(|line| line == "config use-context prod"),
        "the stand-in kubectl recorded the switch: {journal:?}"
    );
}

#[ui_kit::gpui::test]
fn a_single_context_keeps_the_view_of_the_offline_window(cx: &mut TestAppContext) {
    let (window, panel, _) = open_contexts(cx, "contexts-one", &["kind-dev"]);

    // One context needs no list: the node itself carries the reason, as before.
    bounds_of(cx, window, "kube");
    for selector in ["kube-contexts", "kube-context-kind-dev"] {
        assert!(
            !has_selector(cx, window, selector),
            "one context needs no list"
        );
    }
    for selector in ["kube-namespaces", "kube-kind-pods"] {
        assert!(
            !has_selector(cx, window, selector),
            "{selector} is not in the tree of an unreachable cluster"
        );
    }
    let shown = hover_row(cx, window, "kube", CLUSTER_REASON);
    assert!(
        shown.contains(CLUSTER_REASON),
        "the tooltip names the reason: {shown}"
    );

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
