//! One window, every action of its chrome in turn: no action may enter the workspace
//! while the workspace is already being updated. The menu bar and every row of every
//! menu, the buttons of the title bar, Alt and the Alt letters, About, a notification
//! with a command, the update notification and the activation of the window are all
//! pressed here. A panic of any of them fails the test.

#![cfg(windows)]
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use ui_kit::MenuEntry;
use ui_kit::gpui;
use ui_kit::gpui::{
    Modifiers, ModifiersChangedEvent, TestAppContext, VisualTestContext, WindowHandle,
};
use workspace::Workspace;

use crate::git::ActiveGit;
use crate::window_tests::{click, draw, git_repository, open_window, press, wait_for_signal};

/// A release that is newer than the build of the test, served from this process. The
/// archive has a checksum that cannot match, so Update and Restart fails without
/// installing anything, and the page is not a web address, so Release Notes opens
/// nothing.
struct UpdateServer {
    address: std::net::SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl UpdateServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let thread = std::thread::spawn(move || {
            while !stopped.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => answer(stream, address),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            address,
            stop,
            thread: Some(thread),
        }
    }

    fn url(&self) -> String {
        format!("http://{}/releases", self.address)
    }
}

impl Drop for UpdateServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            thread.join().ok();
        }
    }
}

fn answer(mut stream: TcpStream, address: std::net::SocketAddr) {
    // On Windows an accepted socket inherits the non-blocking mode of the listener,
    // and a read before the request arrives would fail under load.
    stream.set_nonblocking(false).ok();
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut request = String::new();
    reader.read_line(&mut request).ok();
    // The rest of the request head is read too, so closing the socket does not reset
    // the connection before the client has the answer.
    let mut line = String::new();
    while reader.read_line(&mut line).is_ok_and(|read| read > 2) {
        line.clear();
    }
    let path = request.split_whitespace().nth(1).unwrap_or("");
    let (kind, body) = match path {
        "/releases" => ("application/json", release_body(address)),
        "/arro.zip" => ("application/zip", "not a release archive".to_owned()),
        "/arro.zip.sha256" => ("text/plain", format!("{}  arro.zip\n", "0".repeat(64))),
        _ => ("text/plain", String::new()),
    };
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).ok();
    stream.write_all(body.as_bytes()).ok();
    stream.flush().ok();
}

fn release_body(address: std::net::SocketAddr) -> String {
    format!(
        r#"{{"tag_name":"v0.2.0","name":"Arro 0.2.0","body":"Faster startup.","html_url":"","prerelease":false,"draft":false,"assets":[
        {{"name":"arro-0.2.0-windows-x86_64.zip","browser_download_url":"http://{address}/arro.zip"}},
        {{"name":"arro-0.2.0-windows-x86_64.zip.sha256","browser_download_url":"http://{address}/arro.zip.sha256"}}
    ]}}"#
    )
}

fn bar_selector(index: usize) -> &'static str {
    Box::leak(format!("menu-bar-{index}").into_boxed_str())
}

/// `menu-1-3`: the debug selector of a row at a path.
fn row_selector(path: &[usize]) -> &'static str {
    let parts: Vec<String> = path.iter().map(|index| index.to_string()).collect();
    Box::leak(format!("menu-{}", parts.join("-")).into_boxed_str())
}

fn sections(cx: &mut TestAppContext, window: WindowHandle<Workspace>) -> Vec<MenuEntry> {
    window
        .update(cx, |workspace, _, cx| workspace.main_menu(cx))
        .unwrap()
}

/// The path and the label of every row that does something, from the top menu down.
fn rows_of(entries: &[MenuEntry]) -> Vec<(Vec<usize>, String)> {
    fn walk(entries: &[MenuEntry], prefix: &mut Vec<usize>, rows: &mut Vec<(Vec<usize>, String)>) {
        for (index, entry) in entries.iter().enumerate() {
            prefix.push(index);
            if entry.has_submenu() {
                walk(&entry.submenu, prefix, rows);
            } else if entry.on_click.is_some() {
                rows.push((prefix.clone(), entry.label.to_string()));
            }
            prefix.pop();
        }
    }
    let mut rows = Vec::new();
    walk(entries, &mut Vec::new(), &mut rows);
    rows
}

/// Closes whatever popup the previous action left open, so the menu bar answers again.
fn dismiss(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    window
        .update(cx, |workspace, window, cx| {
            if workspace.has_overlay() {
                workspace.dismiss(window, cx);
            }
        })
        .ok();
    cx.run_until_parked();
}

/// One press of a row of the menu bar: its section, every submenu on the way, the row.
fn press_row(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    section: usize,
    path: &[usize],
) {
    click(cx, window, bar_selector(section));
    draw(cx, window);
    for depth in 1..path.len() {
        click(cx, window, row_selector(&path[..depth]));
        draw(cx, window);
    }
    click(cx, window, row_selector(path));
    cx.run_until_parked();
    draw(cx, window);
}

/// A single Alt: the press and the release, without another key between them.
fn alt_tap(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    let change = |modifiers| ModifiersChangedEvent {
        modifiers,
        ..ModifiersChangedEvent::default()
    };
    visual.simulate_event(change(Modifiers {
        alt: true,
        ..Modifiers::default()
    }));
    visual.simulate_event(change(Modifiers::default()));
    cx.run_until_parked();
}

fn git(cx: &TestAppContext) -> gpui::Entity<crate::git::GitState> {
    cx.read(|cx| cx.global::<ActiveGit>().0.clone())
}

#[cfg(windows)]
#[ui_kit::gpui::test]
fn every_action_of_the_chrome_leaves_the_workspace_free(cx: &mut TestAppContext) {
    let _env = crate::window_tests_about::ENV_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let server = UpdateServer::start();
    // SAFETY: the lock keeps the other update test out while the variable is changed.
    unsafe {
        std::env::set_var("ARRO_UPDATE_URL", server.url());
        // A real browser tab for every link the chrome opens is noise on the desktop.
        std::env::set_var(platform::NO_BROWSER_ENV, "1");
    }

    // A repository, so that the activation of the window reads the status of Git.
    let root = git_repository("reentrancy");
    let window = open_window(cx, root);
    cx.run_until_parked();
    draw(cx, window);
    wait_for_signal(cx, "the repository", |cx| {
        git(cx).read_with(cx, |git, _| git.repo().is_some())
    });

    // The window goes to the background and comes back, as a click on another program does.
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.deactivate_window();
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.update(|window, _| window.activate_window());
    cx.run_until_parked();
    draw(cx, window);

    // The buttons of the title bar that do something on their own.
    for selector in [
        "t-search",
        "t-project",
        "t-branch",
        "t-settings",
        "t-config",
        "t-run",
        "t-debug",
        "app-channel",
        "w-max",
    ] {
        eprintln!("title bar: {selector}");
        dismiss(cx, window);
        click(cx, window, selector);
        cx.run_until_parked();
        draw(cx, window);
        dismiss(cx, window);
    }

    // Alt alone and every Alt letter of the menu bar.
    eprintln!("keys: alt");
    alt_tap(cx, window);
    press(cx, window, "escape");
    for letter in ["f", "e", "v", "n", "c", "r", "b", "u", "g", "w", "h"] {
        eprintln!("keys: alt-{letter}");
        dismiss(cx, window);
        press(cx, window, &format!("alt-{letter}"));
        draw(cx, window);
        press(cx, window, "escape");
        dismiss(cx, window);
    }

    // Every row of every menu of the menu bar. A row may close the window, so it is
    // pressed at the very end.
    let mut close_project = None;
    for (section, entries) in sections(cx, window).iter().enumerate() {
        for (path, label) in rows_of(&entries.submenu) {
            if label == "Close Project" {
                close_project = Some((section, path));
                continue;
            }
            eprintln!("menu row: {label}");
            dismiss(cx, window);
            press_row(cx, window, section, &path);
            dismiss(cx, window);
        }
    }

    // A command of the window that reads the workspace through the Find in Files view.
    eprintln!("find in selection from the window");
    press(cx, window, "ctrl-shift-f");
    draw(cx, window);
    crate::window_tests::type_text(cx, window, "alpha");
    window
        .update(cx, |workspace, window, cx| {
            workspace.run_command(keymap::workspace::TOGGLE_FIND_IN_SELECTION, window, cx);
        })
        .unwrap();
    cx.run_until_parked();
    press(cx, window, "escape");
    dismiss(cx, window);

    // About and its buttons.
    eprintln!("about");
    window
        .update(cx, |workspace, window, cx| {
            workspace.run_command("app.about", window, cx)
        })
        .unwrap();
    draw(cx, window);
    click(cx, window, "about-copy");
    click(cx, window, "about-notices");
    click(cx, window, "about-support");
    click(cx, window, "about-close");
    cx.run_until_parked();
    assert!(
        platform::recorded_urls()
            .iter()
            .any(|url| url.contains("github.com/sponsors/PabloSensei")),
        "Support Arro did not ask for its page"
    );
    assert!(
        !window
            .read_with(cx, |workspace, _| workspace.has_overlay())
            .unwrap(),
        "Close leaves About open"
    );

    // A notification that runs a command of the window.
    eprintln!("notification with a command");
    window
        .update(cx, |workspace, _window, cx| {
            workspace.register_command("test.reentrancy", "Open About", |workspace, window, cx| {
                workspace.show_about(window, cx)
            });
            workspace.notify_user_with_command(
                "Reentrancy",
                "The button runs a command",
                "Open About",
                "test.reentrancy",
                cx,
            );
        })
        .unwrap();
    draw(cx, window);
    click(cx, window, "notification-action");
    assert!(
        window
            .read_with(cx, |workspace, _| {
                workspace
                    .overlay_view()
                    .and_then(|view| view.downcast::<workspace::AboutView>().ok())
                    .is_some()
            })
            .unwrap(),
        "the button of the notification did not run its command"
    );
    dismiss(cx, window);

    // The update notification and its three buttons.
    eprintln!("update notification");
    window
        .update(cx, |workspace, window, cx| {
            workspace.run_command("app.check_updates", window, cx)
        })
        .unwrap();
    wait_for_signal(cx, "the update notification", |cx| {
        window
            .read_with(cx, |workspace, _| {
                workspace
                    .last_notification()
                    .is_some_and(|(title, _)| title.contains("Arro 0.2.0 is available"))
            })
            .unwrap()
    });
    draw(cx, window);
    click(cx, window, "notification-action-release-notes");
    click(cx, window, "notification-action-skip");
    assert_eq!(
        window
            .read_with(cx, |workspace, _| workspace.skipped_version())
            .unwrap()
            .as_deref(),
        Some("0.2.0")
    );
    window
        .update(cx, |workspace, window, cx| {
            workspace.run_command("app.check_updates", window, cx)
        })
        .unwrap();
    let serial = window
        .read_with(cx, |workspace, _| workspace.notification_serial())
        .unwrap();
    wait_for_signal(cx, "the second update notification", |cx| {
        window
            .read_with(cx, |workspace, _| {
                workspace.notification_serial() > serial
                    && workspace
                        .last_notification()
                        .is_some_and(|(title, _)| title.contains("Arro 0.2.0 is available"))
            })
            .unwrap()
    });
    draw(cx, window);
    // The checksum of the served archive does not match: the button downloads it and
    // reports the failure, it does not restart the program.
    click(cx, window, "notification-action");
    wait_for_signal(cx, "the failed install", |cx| {
        window
            .read_with(cx, |workspace, _| {
                workspace
                    .last_notification()
                    .is_some_and(|(title, _)| title.contains("did not install"))
            })
            .unwrap()
    });

    // A menu row that closes the window goes last.
    if let Some((section, path)) = close_project {
        eprintln!("menu row: Close Project");
        dismiss(cx, window);
        click(cx, window, bar_selector(section));
        draw(cx, window);
        for depth in 1..path.len() {
            click(cx, window, row_selector(&path[..depth]));
            draw(cx, window);
        }
        // Nothing is drawn after the click: the window is gone.
        click(cx, window, row_selector(&path));
        cx.run_until_parked();
    }

    unsafe {
        // The no-browser variable stays: other tests of the process press links too.
        std::env::remove_var("ARRO_UPDATE_URL");
    }
}
