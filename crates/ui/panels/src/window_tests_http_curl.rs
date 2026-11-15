//! The HTTP client beyond the request file: a pasted command line of `curl`, a request
//! copied back as one, the script of an answer and its tests, and the compare of two
//! answers of the history. No window is opened: the tests drive the views.

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use ui_kit::gpui;
use ui_kit::gpui::{AppContext, Modifiers, TestAppContext, VisualTestContext, WindowHandle};
use workspace::Workspace;

use crate::diff_view::DiffView;
use crate::http::ActiveHttp;
use crate::window_tests::{
    bounds_of, buffer_of, draw, editor_of, open_path, open_window, press, scratch,
};

const LIMIT: Duration = Duration::from_secs(20);

/// A server on `127.0.0.1` of its own that answers small JSON bodies and records what it
/// received.
struct Server {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    requests: Arc<Mutex<Vec<String>>>,
}

impl Server {
    fn start() -> Self {
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
                    Ok((stream, _)) => answer(stream, &log),
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

fn answer(mut stream: TcpStream, log: &Mutex<Vec<String>>) {
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
    let mut auth = String::new();
    loop {
        let mut header = String::new();
        match reader.read_line(&mut header) {
            Ok(0) => break,
            Ok(_) if header.trim().is_empty() => break,
            Ok(_) => {
                if let Some((name, value)) = header.split_once(':')
                    && name.trim().eq_ignore_ascii_case("authorization")
                {
                    auth = value.trim().to_owned();
                }
            }
            Err(_) => return,
        }
    }
    log.lock()
        .unwrap_or_else(|error| error.into_inner())
        .push(format!("{method} {path} auth={auth}"));
    let (extra, body) = match path.split('?').next().unwrap_or_default() {
        "/login" => (
            "X-Token: abc\r\n",
            r#"{"token":"abc","owners":[{"lastName":"Davis"}]}"#.to_owned(),
        ),
        "/secured" => ("", r#"{"id":2,"owners":[{"lastName":"Davis"}]}"#.to_owned()),
        "/one" => ("", r#"{"answer":1,"owners":[]}"#.to_owned()),
        "/two" => ("", r#"{"answer":2,"owners":[]}"#.to_owned()),
        _ => ("", r#"{"answer":0,"owners":[]}"#.to_owned()),
    };
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).ok();
    stream.write_all(body.as_bytes()).ok();
    stream.flush().ok();
    stream.shutdown(std::net::Shutdown::Both).ok();
}

/// A project with one request file and the environment next to it.
fn project(name: &str, host: &str, text: &str) -> (PathBuf, PathBuf) {
    let root = scratch(name);
    std::fs::create_dir_all(&root).unwrap();
    let file = root.join("api.http");
    std::fs::write(&file, text).unwrap();
    std::fs::write(
        root.join("http-client.env.json"),
        format!(r#"{{"dev":{{"host":"{host}"}}}}"#),
    )
    .unwrap();
    (root, file)
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

fn run_all(cx: &mut TestAppContext, path: &PathBuf) {
    let text = std::fs::read_to_string(path).unwrap();
    panel(cx).update(cx, |panel, cx| panel.run(path.clone(), text, None, cx));
    cx.run_until_parked();
}

#[ui_kit::gpui::test]
fn a_pasted_curl_command_becomes_a_request(cx: &mut TestAppContext) {
    let (root, file) = project("http-curl-paste", "http://localhost:8080", "");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    open_path(cx, window, &file);
    draw(cx, window);
    let view = editor_of(cx, window, &file);
    cx.update(|cx| {
        cx.write_to_clipboard(ui_kit::ClipboardItem::new_string(
            "curl 'http://localhost:8080/owners?lastName=Davis' \\\n  \
             -H 'Accept: application/json' \\\n  --compressed\n"
                .to_owned(),
        ))
    });

    press(cx, window, "ctrl-v");

    let text = buffer_of(cx, &view);
    assert_eq!(
        text,
        "GET http://localhost:8080/owners?lastName=Davis\nAccept: application/json\n"
    );
}

#[ui_kit::gpui::test]
fn copy_as_curl_puts_the_request_with_its_variables_on_the_clipboard(cx: &mut TestAppContext) {
    let (root, file) = project(
        "http-curl-copy",
        "http://localhost:8080",
        "GET {{host}}/owners\nAccept: application/json\n",
    );
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    open_path(cx, window, &file);
    let view = editor_of(cx, window, &file);
    let panel = panel(cx);
    wait_for(cx, "the environments of the file", |cx| {
        panel.read_with(cx, |panel, _| panel.environment_names().len() == 1)
    });
    view.update(cx, |view, cx| view.restore_caret(0, 0, cx));
    draw(cx, window);

    window
        .update(cx, |workspace, window, cx| {
            workspace.run_command("http.copy_curl", window, cx);
        })
        .unwrap();

    let copied = cx.read(|cx| cx.read_from_clipboard().and_then(|item| item.text()));
    assert_eq!(
        copied.as_deref(),
        Some(
            "curl --request GET 'http://localhost:8080/owners' \\\n  \
             --header 'Accept: application/json'"
        )
    );

    window
        .update(cx, |workspace, window, cx| {
            workspace.run_command("http.copy_curl_cmd", window, cx);
        })
        .unwrap();
    let copied = cx.read(|cx| cx.read_from_clipboard().and_then(|item| item.text()));
    assert_eq!(
        copied.as_deref(),
        Some(
            "curl --request GET ^\"http://localhost:8080/owners^\" ^\n  \
             --header ^\"Accept: application/json^\""
        )
    );
}

#[ui_kit::gpui::test]
fn a_script_sets_a_global_that_the_next_request_uses(cx: &mut TestAppContext) {
    let server = Server::start();
    let (root, file) = project(
        "http-curl-global",
        &server.host(),
        "### Login\n\
         POST {{host}}/login\n\n\
         > {%\n  \
         client.global.set(\"token\", response.headers.valueOf(\"X-Token\"));\n  \
         client.test(\"status\", function() {\n    \
         client.assert(response.status === 200, \"not 200\");\n  \
         });\n\
         %}\n\
         ### Secured\n\
         GET {{host}}/secured\n\
         Authorization: {{token}}\n",
    );
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    open_path(cx, window, &file);
    let panel = panel(cx);
    wait_for(cx, "the environments of the file", |cx| {
        panel.read_with(cx, |panel, _| panel.environment_names().len() == 1)
    });
    run_all(cx, &file);
    wait_for(cx, "both answers", |cx| {
        panel.read_with(cx, |panel, _| panel.history().len() >= 2)
    });
    let globals = panel.read_with(cx, |panel, _| panel.globals().clone());
    assert_eq!(globals.get("token").map(String::as_str), Some("abc"));
    let sent = server.requests();
    assert_eq!(sent[0], "POST /login auth=");
    assert_eq!(
        sent[1], "GET /secured auth=abc",
        "the second request sends the variable of the script: {sent:?}"
    );
    let tests = panel.read_with(cx, |panel, _| panel.tests().to_vec());
    assert_eq!(tests.len(), 1);
    assert!(tests[0].passed, "{tests:?}");
}

#[ui_kit::gpui::test]
fn the_tests_tab_shows_a_passing_and_a_failing_test(cx: &mut TestAppContext) {
    let server = Server::start();
    let (root, file) = project(
        "http-curl-tests",
        &server.host(),
        "### Script\n\
         GET {{host}}/one\n\n\
         > {%\n  \
         client.test(\"passes\", function() {\n    \
         client.assert(response.status === 200);\n  \
         });\n  \
         client.test(\"fails\", function() {\n    \
         client.assert(response.body.answer === 5, \"the answer is not 5\");\n  \
         });\n\
         %}\n",
    );
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    open_path(cx, window, &file);
    let panel = panel(cx);
    wait_for(cx, "the environments of the file", |cx| {
        panel.read_with(cx, |panel, _| panel.environment_names().len() == 1)
    });
    run_all(cx, &file);
    wait_for(cx, "the answer and its tests", |cx| {
        panel.read_with(cx, |panel, _| panel.tests().len() == 2)
    });
    let tests = panel.read_with(cx, |panel, _| panel.tests().to_vec());
    assert!(panel.read_with(cx, |panel, _| panel.script_error().is_none()));
    assert!(panel.read_with(cx, |panel, _| panel.test_logs().is_empty()));
    assert_eq!(tests[0].name, "passes");
    assert!(tests[0].passed);
    assert_eq!(tests[1].name, "fails");
    assert!(!tests[1].passed);
    assert_eq!(tests[1].message.as_deref(), Some("the answer is not 5"));

    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(workspace::HTTP_TOOL, window, cx);
        })
        .unwrap();
    panel.update(cx, |panel, cx| panel.choose_tests(cx));
    draw(cx, window);
    bounds_of(cx, window, "http-test-row-0");
    bounds_of(cx, window, "http-test-row-1");
}

#[ui_kit::gpui::test]
fn compare_opens_the_two_bodies_in_the_diff_window(cx: &mut TestAppContext) {
    let server = Server::start();
    let (root, file) = project(
        "http-curl-compare",
        &server.host(),
        "### One\nGET {{host}}/one\n\n### Two\nGET {{host}}/two\n",
    );
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    open_path(cx, window, &file);
    let panel = panel(cx);
    wait_for(cx, "the environments of the file", |cx| {
        panel.read_with(cx, |panel, _| panel.environment_names().len() == 1)
    });
    run_all(cx, &file);
    wait_for(cx, "two answers", |cx| {
        panel.read_with(cx, |panel, _| panel.history().len() >= 2)
    });
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(workspace::HTTP_TOOL, window, cx);
        })
        .unwrap();
    draw(cx, window);
    bounds_of(cx, window, "http-compare");
    cx.update_window(window.into(), |_, window, cx| {
        panel.update(cx, |panel, cx| panel.show_compare(window, cx));
    })
    .unwrap();
    cx.run_until_parked();
    draw(cx, window);
    assert!(
        window
            .read_with(cx, |workspace, _| workspace.overlay_view().is_some())
            .unwrap(),
        "the picker is open"
    );
    click_selector(cx, window, "http-compare-row-0");
    click_selector(cx, window, "http-compare-row-1");
    click_selector(cx, window, "http-compare-run");

    wait_for(cx, "the diff of the two answers", |cx| {
        window
            .read_with(cx, |workspace, _| {
                workspace
                    .overlay_view()
                    .and_then(|view| view.downcast::<DiffView>().ok())
                    .is_some()
            })
            .unwrap()
    });
    let diff = window
        .read_with(cx, |workspace, _| {
            workspace
                .overlay_view()
                .and_then(|view| view.downcast::<DiffView>().ok())
        })
        .unwrap()
        .expect("the diff window is open");
    let left = diff.read_with(cx, |diff, _| diff.left_editor());
    let right = diff.read_with(cx, |diff, _| diff.right_editor());
    let left = left
        .read_with(cx, |view, _| view.text())
        .unwrap_or_default();
    let right = right
        .read_with(cx, |view, _| view.text())
        .unwrap_or_default();
    assert!(left.contains("\"answer\": 2"), "{left}");
    assert!(right.contains("\"answer\": 1"), "{right}");
}
