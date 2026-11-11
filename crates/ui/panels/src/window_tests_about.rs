//! The About window, the channel label and the update notification of T-0104, without a
//! real window and without the network.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use workspace::{AppInfo, Settings, Workspace};

use crate::window_tests::{click, draw, open_sample_window, wait_for_signal};

fn has_selector(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &'static str,
) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

fn overlay_is_about(cx: &mut TestAppContext, window: WindowHandle<Workspace>) -> bool {
    window
        .read_with(cx, |workspace, _| {
            workspace
                .overlay_view()
                .and_then(|view| view.downcast::<workspace::AboutView>().ok())
                .is_some()
        })
        .unwrap()
}

#[ui_kit::gpui::test]
fn about_shows_the_version_and_copies_a_bug_report_line(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "about-window");
    window
        .update(cx, |workspace, window, cx| {
            workspace.run_command("app.about", window, cx)
        })
        .unwrap();
    draw(cx, window);
    assert!(overlay_is_about(cx, window), "About did not open");

    let overlay = window
        .read_with(cx, |workspace, _| workspace.overlay_view())
        .unwrap()
        .unwrap();
    let about = overlay.downcast::<workspace::AboutView>().ok().unwrap();
    let lines = about.read_with(cx, |about, _| about.lines());
    let text = lines.join("\n");
    let version = AppInfo::default().version;
    assert!(text.contains(version), "About has no version: {text}");
    assert!(text.contains("Commit:"), "About has no commit: {text}");
    assert!(
        text.contains("Early preview."),
        "About does not say the build is early: {text}"
    );
    assert!(
        text.contains("Written with AI"),
        "About does not say the code is written with AI: {text}"
    );
    assert!(
        text.contains("FSL-1.1-ALv2"),
        "About has no license: {text}"
    );

    click(cx, window, "about-copy");
    let copied = cx
        .read(|cx| cx.read_from_clipboard().and_then(|item| item.text()))
        .expect("Copy puts a line on the clipboard");
    assert!(copied.contains(version), "{copied}");
    assert!(copied.contains("commit"), "{copied}");
    assert!(copied.contains(std::env::consts::OS), "{copied}");
}

#[ui_kit::gpui::test]
fn the_preview_label_opens_about(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "about-badge");
    window
        .update(cx, |workspace, _, cx| {
            workspace.set_info(
                AppInfo {
                    channel: "preview",
                    ..AppInfo::default()
                },
                cx,
            )
        })
        .unwrap();
    draw(cx, window);
    assert_eq!(
        window
            .read_with(cx, |workspace, _| workspace.channel_label())
            .unwrap(),
        "Preview"
    );
    assert!(has_selector(cx, window, "app-channel"));
    click(cx, window, "app-channel");
    assert!(overlay_is_about(cx, window), "the label did not open About");
}

pub(crate) static ENV_LOCK: Mutex<()> = Mutex::new(());

struct Server {
    address: std::net::SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Server {
    fn start(body: String) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let body = Arc::new(body);
        let thread = std::thread::spawn(move || {
            while !stopped.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => answer(stream, &body),
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
        format!("http://{}/releases/latest", self.address)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            thread.join().ok();
        }
    }
}

fn answer(mut stream: TcpStream, body: &str) {
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut line = String::new();
    reader.read_line(&mut line).ok();
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).ok();
    stream.write_all(body.as_bytes()).ok();
    stream.flush().ok();
}

fn release_body() -> String {
    r#"{"tag_name":"v0.2.0","name":"Arro 0.2.0","body":"Faster startup.","html_url":"https://example.test/release","prerelease":false,"draft":false,"assets":[
        {"name":"arro-0.2.0-windows-x86_64.zip","browser_download_url":"https://example.test/arro.zip"},
        {"name":"arro-0.2.0-windows-x86_64.zip.sha256","browser_download_url":"https://example.test/arro.zip.sha256"}
    ]}"#
    .to_owned()
}

#[ui_kit::gpui::test]
fn a_new_version_offers_the_buttons_and_skip_is_remembered(cx: &mut TestAppContext) {
    let _env = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let server = Server::start(release_body());
    // SAFETY: the lock keeps the other update test out while the variable is changed.
    unsafe {
        std::env::set_var("ARRO_UPDATE_URL", server.url());
    }
    let (_, window) = open_sample_window(cx, "about-update");
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
    let buttons = window
        .read_with(cx, |workspace, _| workspace.last_notification_buttons())
        .unwrap();
    let labels: Vec<String> = buttons.iter().map(ToString::to_string).collect();
    assert_eq!(
        labels,
        ["Update and Restart", "Release Notes", "Skip This Version"]
    );
    click(cx, window, "notification-action-skip");
    assert_eq!(
        window
            .read_with(cx, |workspace, _| workspace.skipped_version())
            .unwrap()
            .as_deref(),
        Some("0.2.0")
    );
    unsafe {
        std::env::remove_var("ARRO_UPDATE_URL");
    }
}

#[test]
fn automatic_updates_are_on_by_default() {
    assert!(Settings::default().updates_check_automatically);
}
