//! The HTTP client of a `.http` file without a window: the gutter marks of the request
//! lines, a click that sends the request to a local server, the answer in the HTTP
//! Response window and the environment selector above the text.

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use ui_kit::gpui;
use ui_kit::gpui::{Modifiers, TestAppContext, VisualTestContext, WindowHandle};
use ui_kit::metrics::CONTROL_HEIGHT;
use ui_kit::px;
use workspace::Workspace;

use crate::http::ActiveHttp;
use crate::window_tests::{
    bounds_of, draw, editor_of, line_height, open_path, open_window, press, scratch,
};

const LIMIT: Duration = Duration::from_secs(20);

/// A server on `127.0.0.1` with a port of its own. It answers `/owners` with a JSON body
/// that names the environment, and records every request it receives.
struct Server {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    requests: Arc<Mutex<Vec<String>>>,
}

impl Server {
    fn start(name: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let log = requests.clone();
        let thread = std::thread::spawn(move || {
            while !stopped.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => answer(stream, name, &log),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            address,
            stop,
            thread: Some(thread),
            requests,
        }
    }

    fn host(&self) -> String {
        format!("http://{}", self.address)
    }

    fn requests(&self) -> Vec<String> {
        self.requests
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
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

fn answer(mut stream: TcpStream, name: &str, log: &Mutex<Vec<String>>) {
    stream.set_nonblocking(false).ok();
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }
    let mut words = line.split_whitespace();
    let method = words.next().unwrap_or_default().to_owned();
    let path = words.next().unwrap_or_default().to_owned();
    let mut accept = String::new();
    loop {
        let mut header = String::new();
        match reader.read_line(&mut header) {
            Ok(0) => break,
            Ok(_) if header.trim().is_empty() => break,
            Ok(_) => {
                if let Some((name, value)) = header.split_once(':')
                    && name.trim().eq_ignore_ascii_case("accept")
                {
                    accept = value.trim().to_owned();
                }
            }
            Err(_) => return,
        }
    }
    log.lock()
        .unwrap_or_else(|error| error.into_inner())
        .push(format!("{method} {path} accept={accept}"));
    let reply = format!(r#"{{"owners":[{{"id":1,"lastName":"Davis"}}],"env":"{name}"}}"#);
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        reply.len()
    );
    stream.write_all(head.as_bytes()).ok();
    stream.write_all(reply.as_bytes()).ok();
    stream.flush().ok();
    stream.shutdown(std::net::Shutdown::Both).ok();
}

/// A project with one request file and the environments next to it.
fn project(name: &str, dev: &str, prod: &str) -> PathBuf {
    let root = scratch(name);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("api.http"),
        "### Get owners\nGET {{host}}/owners\nAccept: application/json\n\n\
         ### Second\nGET {{host}}/owners?second=1\n",
    )
    .unwrap();
    std::fs::write(
        root.join("http-client.env.json"),
        format!(r#"{{"dev":{{"host":"{dev}"}},"prod":{{"host":"{prod}"}}}}"#),
    )
    .unwrap();
    root
}

fn wait_for(
    cx: &mut TestAppContext,
    what: &str,
    mut check: impl FnMut(&mut TestAppContext) -> bool,
) {
    let started = Instant::now();
    loop {
        if check(cx) {
            return;
        }
        assert!(started.elapsed() < LIMIT, "waited for {what} in vain");
        std::thread::sleep(Duration::from_millis(10));
        cx.executor().advance_clock(Duration::from_millis(50));
        cx.run_until_parked();
    }
}

fn panel(cx: &TestAppContext) -> gpui::Entity<crate::http::HttpPanel> {
    cx.read(|cx| cx.global::<ActiveHttp>().0.clone())
}

fn click_selector(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &str) {
    let selector: &'static str = Box::leak(selector.to_string().into_boxed_str());
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    let bounds = visual
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("no element {selector} on screen"));
    visual.simulate_click(bounds.center(), Modifiers::default());
    cx.run_until_parked();
}

fn menu_labels(cx: &TestAppContext, window: WindowHandle<Workspace>) -> Vec<String> {
    window
        .read_with(cx, |workspace, _| {
            workspace
                .menu_entries()
                .map(|entries| {
                    entries
                        .iter()
                        .map(|entry| entry.label.to_string())
                        .collect()
                })
                .unwrap_or_default()
        })
        .unwrap()
}

fn click_menu(cx: &mut TestAppContext, window: WindowHandle<Workspace>, label: &str) {
    wait_for(cx, "the menu row", |cx| {
        menu_labels(cx, window).iter().any(|entry| entry == label)
    });
    let index = menu_labels(cx, window)
        .iter()
        .position(|entry| entry == label)
        .unwrap_or_else(|| panic!("no menu row {label}"));
    click_selector(cx, window, &format!("menu-{index}"));
}

/// A click on the run icon of a request line: the bar above the text moves the rows
/// down by its height, which `click_gutter` does not know about.
fn click_request_mark(cx: &mut TestAppContext, window: WindowHandle<Workspace>, line: usize) {
    let height = line_height(cx);
    let bounds = bounds_of(cx, window, "editor-body-0");
    let x = bounds.left() + px(12.);
    let y = bounds.top() + px(CONTROL_HEIGHT + height * line as f32 + height / 2.);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_click(ui_kit::point(x, y), Modifiers::default());
    cx.run_until_parked();
}

fn request_marks(cx: &TestAppContext, view: &gpui::Entity<editor_view::EditorView>) -> Vec<u32> {
    view.read_with(cx, |view, _| {
        view.run_marks().iter().map(|mark| mark.line).collect()
    })
}

#[test]
fn the_new_menu_offers_a_request_file() {
    let template = crate::tree_ops::http_request();
    assert_eq!(template.label, "HTTP Request");
    assert_eq!(template.extension, "http");
    let text = (template.content)(std::path::Path::new("."), std::path::Path::new("."), "api");
    assert!(text.starts_with("### api\nGET "), "{text}");
    assert!(text.contains("Accept: application/json"), "{text}");
}

#[ui_kit::gpui::test]
fn a_request_line_has_a_run_mark_and_a_click_shows_the_answer(cx: &mut TestAppContext) {
    let dev = Server::start("dev");
    let root = project("http-run", &dev.host(), &dev.host());
    let file = root.join("api.http");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    open_path(cx, window, &file);
    let view = editor_of(cx, window, &file);
    wait_for(cx, "the gutter marks", |cx| {
        request_marks(cx, &view).len() >= 2
    });
    let marks = request_marks(cx, &view);
    assert_eq!(marks, [1, 5], "the request lines carry the marks");

    click_request_mark(cx, window, 1);
    let panel = panel(cx);
    wait_for(cx, "the answer", |cx| {
        panel.read_with(cx, |panel, _| panel.status_text().is_some())
    });
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.status_text()),
        Some("200 OK".to_owned())
    );
    let body = panel
        .read_with(cx, |panel, _| panel.body_text().map(str::to_owned))
        .expect("the body of the answer");
    assert!(
        body.contains("\n  \"owners\""),
        "the JSON body is indented: {body}"
    );
    assert!(body.contains("\"env\": \"dev\""), "{body}");
    assert!(
        panel.read_with(cx, |panel, _| panel.body_is_pretty()),
        "the JSON body is painted as JSON"
    );
    let sent = dev.requests();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0], "GET /owners accept=application/json");
    let state = window
        .read_with(cx, |workspace, _| workspace.state_dir().to_path_buf())
        .unwrap();
    let kept = panel.read_with(cx, |panel, _| {
        panel.history().iter().any(|entry| {
            ::http_client::history_dir(&state)
                .join(format!("{}.body", entry.id))
                .is_file()
        })
    });
    assert!(kept, "the body of an answer is kept for Open in Editor");
}

#[ui_kit::gpui::test]
fn the_environment_dropdown_changes_the_host_of_the_request(cx: &mut TestAppContext) {
    let dev = Server::start("dev");
    let prod = Server::start("prod");
    let root = project("http-environment", &dev.host(), &prod.host());
    let file = root.join("api.http");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    open_path(cx, window, &file);
    let panel = panel(cx);
    wait_for(cx, "the environments of the file", |cx| {
        panel.read_with(cx, |panel, _| panel.environment_names().len() == 2)
    });
    draw(cx, window);

    click_selector(cx, window, "http-environment");
    draw(cx, window);
    let labels = menu_labels(cx, window);
    assert!(labels.iter().any(|label| label == "dev"), "{labels:?}");
    assert!(labels.iter().any(|label| label == "prod"), "{labels:?}");
    click_menu(cx, window, "prod");

    click_request_mark(cx, window, 1);
    wait_for(cx, "the answer of the other environment", |cx| {
        panel.read_with(cx, |panel, _| {
            panel
                .body_text()
                .is_some_and(|body| body.contains("\"env\": \"prod\""))
        })
    });
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.selected_environment()),
        Some("prod".to_owned())
    );
    assert!(dev.requests().is_empty(), "nothing went to dev");
    assert_eq!(prod.requests(), ["GET /owners accept=application/json"]);
}

#[ui_kit::gpui::test]
fn ctrl_enter_sends_the_request_at_the_caret(cx: &mut TestAppContext) {
    let dev = Server::start("dev");
    let root = project("http-ctrl-enter", &dev.host(), &dev.host());
    let file = root.join("api.http");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    open_path(cx, window, &file);
    let view = editor_of(cx, window, &file);
    wait_for(cx, "the gutter marks", |cx| {
        request_marks(cx, &view).len() >= 2
    });
    view.update(cx, |view, cx| view.restore_caret(5, 0, cx));
    draw(cx, window);

    press(cx, window, "ctrl-enter");
    let panel = panel(cx);
    wait_for(cx, "the answer of the second request", |cx| {
        panel.read_with(cx, |panel, _| {
            panel
                .shown_url()
                .is_some_and(|url| url.ends_with("/owners?second=1"))
        })
    });
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.status_text()),
        Some("200 OK".to_owned())
    );
    assert!(
        dev.requests()
            .iter()
            .any(|request| request.starts_with("GET /owners?second=1")),
        "{:?}",
        dev.requests()
    );
}

#[ui_kit::gpui::test]
fn the_colours_of_a_request_file_come_from_its_lines(cx: &mut TestAppContext) {
    let dev = Server::start("dev");
    let root = project("http-highlight", &dev.host(), &dev.host());
    let file = root.join("api.http");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    open_path(cx, window, &file);
    let view = editor_of(cx, window, &file);
    draw(cx, window);
    let text = view.read_with(cx, |view, _| view.text()).unwrap();
    let spans = view.read_with(cx, |view, _| view.highlight_spans().to_vec());
    assert!(!spans.is_empty(), "the request file is coloured");
    let coloured = |kind: editor_view::HighlightKind| -> Vec<String> {
        spans
            .iter()
            .filter(|span| span.kind == kind)
            .map(|span| text[span.start as usize..span.end as usize].to_owned())
            .collect()
    };
    assert!(
        coloured(editor_view::HighlightKind::Keyword)
            .iter()
            .any(|word| word == "GET"),
        "the method is a keyword"
    );
    assert!(
        coloured(editor_view::HighlightKind::Field)
            .iter()
            .any(|word| word == "Accept"),
        "a header name is a field"
    );
    assert!(
        coloured(editor_view::HighlightKind::Constant)
            .iter()
            .any(|word| word == "{{host}}"),
        "the variable is a constant"
    );
}

#[ui_kit::gpui::test]
fn the_gutter_menu_runs_every_request(cx: &mut TestAppContext) {
    let dev = Server::start("dev");
    let root = project("http-run-all", &dev.host(), &dev.host());
    let file = root.join("api.http");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    open_path(cx, window, &file);
    let view = editor_of(cx, window, &file);
    wait_for(cx, "the gutter marks", |cx| {
        request_marks(cx, &view).len() >= 2
    });
    window
        .update(cx, |workspace, _, cx| {
            let model = crate::editor_menu::MenuModel::of(workspace).in_gutter(true);
            crate::editor_menu::show_now(
                workspace,
                view.clone(),
                cx.weak_entity(),
                model,
                ui_kit::point(ui_kit::px(0.), ui_kit::px(0.)),
                cx,
            );
        })
        .unwrap();
    draw(cx, window);
    click_menu(cx, window, "Run All Requests");
    let panel = panel(cx);
    wait_for(cx, "both requests", |cx| {
        panel.read_with(cx, |_panel, _| dev.requests().len() == 2)
    });
    assert!(panel.read_with(cx, |panel, _| panel.history().len()) >= 2);
}
