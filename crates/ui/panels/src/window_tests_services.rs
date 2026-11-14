//! T-0192: the Services window with a stand-in Docker. The compose projects of the tree,
//! the log that follows the selection, the context menu of a container, the toolbar and
//! the header that keep their places, and the splitter between the tree and the log.
//!
//! cargo test -p panels window_tests_services

#![cfg(windows)]

use std::fs;
use std::path::{Path, PathBuf};

use ui_kit::gpui;
use ui_kit::gpui::{MouseButton, TestAppContext, VisualTestContext, WindowHandle, size};
use ui_kit::{Modifiers, point, px};
use workspace::{SERVICES_TOOL, Workspace};

use crate::docker::{ActiveDocker, DockerPanel};
use crate::window_tests::{bounds_of, click, draw, open_window, sample_project, scratch};
use crate::window_tests_ui::{ENV, wait_for};

/// What the stand-in `docker` says the daemon has: two containers of the compose project
/// `petclinic` and one of no project.
const PS_LINES: [&str; 3] = [
    r#"{"ID":"web0001","Names":"petclinic-web-1","Image":"petclinic:latest","State":"running","Status":"Up 1 second","Ports":"","Labels":"com.docker.compose.project=petclinic,com.docker.compose.service=web"}"#,
    r#"{"ID":"db0002","Names":"petclinic-db-1","Image":"postgres:17","State":"exited","Status":"Exited (0) 1 second ago","Ports":"","Labels":"com.docker.compose.project=petclinic,com.docker.compose.service=db"}"#,
    r#"{"ID":"solo0003","Names":"solo-1","Image":"hello-world:latest","State":"running","Status":"Up 1 second","Ports":"","Labels":""}"#,
];

/// A `docker.cmd` that answers the calls of the panel from the constants above and writes
/// every call to `calls.txt` next to itself. `logs` prints the first line at once and a
/// tick every second until it is stopped.
fn fake_docker(name: &str) -> (PathBuf, PathBuf) {
    let folder = scratch(name);
    let script = folder.join("docker.cmd");
    let calls = folder.join("calls.txt");
    let ps: String = PS_LINES
        .iter()
        .map(|line| format!("echo {line}\r\n"))
        .collect();
    let text = format!(
        r#"@echo off
setlocal EnableExtensions
echo %*>>"{calls}"
if "%~1"=="--context" shift
if "%~1"=="--context" shift
if "%~1"=="version" goto version
if "%~1"=="context" goto context
if "%~1"=="ps" goto ps
if "%~1"=="images" goto images
if "%~1"=="logs" goto logs
if "%~1"=="compose" goto compose
if "%~1"=="run" goto done
if "%~1"=="start" goto done
if "%~1"=="stop" goto done
if "%~1"=="restart" goto done
if "%~1"=="rm" goto done
goto unknown
:version
echo {{"Client":{{"Version":"29.9.2"}},"Server":{{"Version":"29.0.0-fake","Os":"linux"}}}}
goto end
:context
echo {{"Current":false,"Description":"","DockerEndpoint":"npipe:////./pipe/docker_engine","Name":"default"}}
echo {{"Current":true,"Description":"Docker Desktop","DockerEndpoint":"npipe:////./pipe/dockerDesktopLinuxEngine","Name":"desktop-linux"}}
goto end
:ps
{ps}goto end
:images
echo {{"ID":"sha256:aa","Repository":"hello-world","Tag":"latest","Size":"13.3kB"}}
goto end
:logs
set "id=%~5"
echo hello from %id%
echo tick from %id%
ping -n 2 127.0.0.1 >nul
echo tick from %id%
goto end
:compose
echo composed
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
    );
    fs::write(&script, text).unwrap();
    (script, calls)
}

/// Opens the window on the stand-in CLI, shows Services and waits for the tree.
fn open_services(
    cx: &mut TestAppContext,
    name: &str,
) -> (WindowHandle<Workspace>, gpui::Entity<DockerPanel>, PathBuf) {
    let root = sample_project(name);
    let (script, calls) = fake_docker(&format!("{name}-cli"));
    let _guard = ENV.lock().unwrap_or_else(|poison| poison.into_inner());
    // SAFETY: the window reads the variable at creation, under `ENV`.
    unsafe { std::env::set_var("IDE_DOCKER_CLI", &script) };
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
    let docker = cx.update(|cx| cx.global::<ActiveDocker>().0.clone());
    wait_for(cx, "the containers of the stand-in Docker", |cx| {
        docker.read_with(cx, |panel, _| {
            panel
                .report()
                .iter()
                .any(|line| line.contains("petclinic-web-1"))
        })
    });
    draw(cx, window);
    (window, docker, calls)
}

fn calls_of(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_default()
}

fn right_click(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) {
    let position = bounds_of(cx, window, selector).center();
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_down(position, MouseButton::Right, Modifiers::default());
    visual.simulate_mouse_up(position, MouseButton::Right, Modifiers::default());
    cx.run_until_parked();
    draw(cx, window);
}

fn menu_labels(cx: &mut TestAppContext, window: WindowHandle<Workspace>) -> Vec<String> {
    window
        .read_with(cx, |workspace, _| {
            workspace
                .menu_entries()
                .map(|entries| {
                    entries
                        .iter()
                        .map(|entry| entry.label.to_string())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        })
        .unwrap()
}

#[ui_kit::gpui::test]
fn containers_are_grouped_by_compose_project_with_their_counter(cx: &mut TestAppContext) {
    let (window, docker, _) = open_services(cx, "services-groups");

    let (groups, loose) = docker.read_with(cx, |panel, _| {
        (panel.compose_groups(), panel.loose_containers())
    });
    assert_eq!(groups, vec![("petclinic".to_owned(), 1, 2)]);
    assert_eq!(loose, vec!["solo-1".to_owned()]);

    click(cx, window, "docker-group-petclinic");
    draw(cx, window);
    let (title, hint) = docker.read_with(cx, |panel, _| panel.header_text());
    assert_eq!(title, "Docker-compose: petclinic");
    assert_eq!(hint, "Running: (1/2)");

    // The project of the group and its containers are in the tree.
    bounds_of(cx, window, "docker-group-petclinic");
    bounds_of(cx, window, "docker-container-petclinic-web-1");
    bounds_of(cx, window, "docker-container-petclinic-db-1");

    // The log of the project mixes its containers, and every line names its own.
    wait_for(cx, "the prefixed log of the project", |cx| {
        docker.read_with(cx, |panel, _| {
            panel
                .report()
                .iter()
                .any(|line| line == "docker log petclinic-web-1  hello from web0001")
        })
    });
    assert!(
        docker
            .read_with(cx, |panel, _| panel.log_targets())
            .contains(&"petclinic-web-1".to_owned())
    );
}

#[ui_kit::gpui::test]
fn selecting_another_container_stops_the_log_of_the_first(cx: &mut TestAppContext) {
    let (window, docker, _) = open_services(cx, "services-logs");

    click(cx, window, "docker-container-petclinic-web-1");
    draw(cx, window);
    wait_for(cx, "the line of the first log", |cx| {
        docker.read_with(cx, |panel, _| {
            panel
                .report()
                .iter()
                .any(|line| line == "docker log hello from web0001")
        })
    });
    assert_eq!(
        docker.read_with(cx, |panel, _| panel.log_targets()),
        vec!["petclinic-web-1".to_owned()]
    );

    click(cx, window, "docker-container-solo-1");
    draw(cx, window);
    wait_for(cx, "the log of the second container", |cx| {
        docker.read_with(cx, |panel, _| {
            panel.log_targets() == vec!["solo-1".to_owned()]
                && panel
                    .report()
                    .iter()
                    .any(|line| line == "docker log hello from solo0003")
        })
    });
    let report = docker.read_with(cx, |panel, _| panel.report());
    assert!(
        !report
            .iter()
            .any(|line| line.contains("hello from web0001")),
        "the lines of the first log went away: {report:?}"
    );
}

#[ui_kit::gpui::test]
fn the_container_menu_has_the_actions_and_stop_reaches_docker(cx: &mut TestAppContext) {
    let (window, _, calls) = open_services(cx, "services-menu");

    right_click(cx, window, "docker-container-petclinic-web-1");
    let labels = menu_labels(cx, window);
    for label in [
        "Start",
        "Stop",
        "Restart",
        "Remove",
        "Show Logs",
        "Exec",
        "Copy Container ID",
    ] {
        assert!(
            labels.iter().any(|row| row == label),
            "the menu has {label}: {labels:?}"
        );
    }

    let index = labels
        .iter()
        .position(|label| label == "Stop")
        .expect("the Stop row");
    let selector: &'static str = Box::leak(format!("menu-{index}").into_boxed_str());
    click(cx, window, selector);
    wait_for(cx, "the stop call of the stand-in docker", |_| {
        calls_of(&calls).contains("stop web0001")
    });

    // The menu of a compose project stops the whole project through its name.
    right_click(cx, window, "docker-group-petclinic");
    let labels = menu_labels(cx, window);
    assert_eq!(labels, vec!["Up", "Down", "Restart", "Stop"]);
    click(cx, window, "menu-3");
    wait_for(cx, "the compose stop call", |_| {
        calls_of(&calls).lines().any(|line| {
            line.starts_with("compose --project-name petclinic")
                && line.contains("--project-directory")
                && line.ends_with(" stop")
        })
    });
}

#[ui_kit::gpui::test]
fn the_toolbar_and_the_header_buttons_do_not_overlap(cx: &mut TestAppContext) {
    let (window, _, _) = open_services(cx, "services-overlap");
    click(cx, window, "docker-container-petclinic-web-1");
    draw(cx, window);

    let selectors = [
        "docker-toolbar-stopped",
        "docker-toolbar-expand",
        "docker-toolbar-collapse",
        "docker-title",
        "docker-action-start",
        "docker-action-stop",
        "docker-action-restart",
        "docker-action-remove",
    ];
    for selector in selectors {
        bounds_of(cx, window, selector);
    }
    assert_apart(cx, window, &selectors);

    // The same at a width where the actions keep only their icons.
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.update(|window, cx| {
        window.resize(size(px(520.), px(700.)));
        window.bounds_changed(cx);
    });
    cx.run_until_parked();
    draw(cx, window);
    assert_apart(cx, window, &selectors);
}

fn assert_apart(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selectors: &[&'static str],
) {
    let bounds: Vec<_> = selectors
        .iter()
        .map(|selector| (*selector, bounds_of(cx, window, selector)))
        .collect();
    for (index, (name, first)) in bounds.iter().enumerate() {
        for (other, second) in &bounds[index + 1..] {
            let apart = first.right() <= second.left()
                || second.right() <= first.left()
                || first.bottom() <= second.top()
                || second.bottom() <= first.top();
            assert!(apart, "{name} {:?} overlaps {other} {:?}", first, second);
        }
    }
}

#[ui_kit::gpui::test]
fn dragging_the_splitter_changes_the_width_of_the_tree(cx: &mut TestAppContext) {
    let (window, _, _) = open_services(cx, "services-splitter");
    let before = bounds_of(cx, window, "docker-tree-pane").size.width;
    let from = bounds_of(cx, window, "docker-split").center();
    let to = point(from.x + px(120.), from.y);

    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_move(from, None, Modifiers::default());
    visual.simulate_mouse_down(from, MouseButton::Left, Modifiers::default());
    for step in 1..=6 {
        let fraction = step as f32 / 6.;
        visual.simulate_mouse_move(
            point(from.x + (to.x - from.x) * fraction, from.y),
            Some(MouseButton::Left),
            Modifiers::default(),
        );
    }
    visual.simulate_mouse_up(to, MouseButton::Left, Modifiers::default());
    cx.run_until_parked();
    draw(cx, window);

    let after = bounds_of(cx, window, "docker-tree-pane").size.width;
    assert!(
        after > before + px(60.),
        "the tree grew from {before:?} to {after:?}"
    );
}
