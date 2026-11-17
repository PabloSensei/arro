//! The pictures of a markdown preview: a file of the project drawn in place, the text of
//! a picture that is not there, an address of a server downloaded through a local one,
//! the size an `<img>` asks for, the link around a picture, and the mode of the tab a
//! second open of the project brings back.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use editor_view::{ExternalLinks, PreviewMode, PreviewModes};
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use workspace::{Layout, PreviewLayout, Settings, Workspace};

use crate::window_tests::{
    bounds_of, click, draw, editor_of, open_path, open_sample_window, open_window, scratch,
    wait_for_signal,
};

/// A 40x20 picture of one colour, as the bytes of a PNG file.
const PNG: [u8; 105] = [
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 40, 0, 0, 0, 20, 8, 6,
    0, 0, 0, 255, 70, 127, 187, 0, 0, 0, 48, 73, 68, 65, 84, 120, 156, 237, 206, 177, 17, 0, 32,
    12, 0, 161, 140, 238, 230, 113, 10, 239, 45, 40, 232, 153, 57, 187, 95, 203, 3, 130, 130, 117,
    64, 80, 176, 14, 8, 10, 214, 1, 65, 193, 58, 32, 248, 216, 5, 74, 220, 202, 42, 22, 24, 9, 71,
    0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
];

/// A picture of the project under a name of the test.
fn picture(root: &Path, name: &str) -> PathBuf {
    let path = root.join(name);
    fs::write(&path, PNG).unwrap();
    path
}

/// A markdown file of the project with the text of the test.
fn markdown_file(root: &Path, name: &str, text: &str) -> PathBuf {
    let path = root.join(name);
    fs::write(&path, text).unwrap();
    path
}

/// Waits until the preview draws words of the text.
fn wait_for_preview(
    cx: &mut TestAppContext,
    view: &gpui::Entity<editor_view::EditorView>,
    words: &str,
) {
    wait_for_signal(cx, "the markdown preview", |cx| {
        view.read_with(cx, |view, _| {
            view.preview_text().is_some_and(|text| text.contains(words))
        })
    });
}

/// The size of an element with a debug selector in the last frame, `None` when the frame
/// drew no such element.
fn size_of(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &'static str,
) -> Option<(f32, f32)> {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual
        .debug_bounds(selector)
        .map(|bounds| (f32::from(bounds.size.width), f32::from(bounds.size.height)))
}

/// Waits until a picture is drawn with a size of its own.
fn wait_for_picture(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &'static str,
) {
    wait_for_signal(cx, "the picture of the preview", |cx| {
        size_of(cx, window, selector).is_some_and(|(width, height)| width > 0. && height > 0.)
    });
}

/// A server on `127.0.0.1` with a port of its own: every request gets the PNG of the
/// test, and the paths of the requests are kept for the test.
struct PictureServer {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    requests: Arc<Mutex<Vec<String>>>,
}

impl PictureServer {
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

    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.address)
    }

    fn requests(&self) -> Vec<String> {
        self.requests
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }
}

impl Drop for PictureServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            thread.join().ok();
        }
    }
}

/// Reads the request line and the headers, writes the PNG of the test and closes.
fn answer(mut stream: TcpStream, log: &Mutex<Vec<String>>) {
    stream.set_nonblocking(false).ok();
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }
    let path = line
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .to_owned();
    loop {
        let mut header = String::new();
        match reader.read_line(&mut header) {
            Ok(0) => break,
            Ok(_) if header.trim().is_empty() => break,
            Ok(_) => {}
            Err(_) => return,
        }
    }
    log.lock()
        .unwrap_or_else(|error| error.into_inner())
        .push(path);
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        PNG.len()
    );
    stream.write_all(head.as_bytes()).ok();
    stream.write_all(&PNG).ok();
    stream.flush().ok();
    stream.shutdown(std::net::Shutdown::Both).ok();
}

#[ui_kit::gpui::test]
fn a_local_picture_of_the_project_is_drawn(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "markdown-images-local");
    picture(&root, "logo.png");
    let path = markdown_file(&root, "guide.md", "# Guide\n\n![the logo](logo.png)\n");
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    wait_for_preview(cx, &view, "the logo");
    draw(cx, window);
    wait_for_picture(cx, window, "markdown-preview-image-0");

    let (width, height) = size_of(cx, window, "markdown-preview-image-0").expect("the picture");
    assert!(
        (width - 40.).abs() <= 1. && (height - 20.).abs() <= 1.,
        "the picture keeps the size of its bytes: {width}x{height}"
    );
}

#[ui_kit::gpui::test]
fn a_missing_picture_shows_its_text_once(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "markdown-images-missing");
    let path = markdown_file(&root, "guide.md", "# Guide\n\n![the logo](missing.png)\n");
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    wait_for_preview(cx, &view, "the logo");
    draw(cx, window);
    draw(cx, window);

    let text = view
        .read_with(cx, |view, _| view.preview_text())
        .expect("the text of the preview");
    assert_eq!(
        text.matches("the logo").count(),
        1,
        "the text of the picture stands once: {text:?}"
    );
    assert!(
        size_of(cx, window, "markdown-preview-picture-0").is_some(),
        "the text of the picture stands in a frame of its own"
    );
    assert!(
        size_of(cx, window, "markdown-preview-image-0").is_none(),
        "a picture that is not there is not drawn"
    );
}

#[ui_kit::gpui::test]
fn a_picture_of_the_internet_is_downloaded_and_drawn(cx: &mut TestAppContext) {
    let server = PictureServer::start();
    let (root, window) = open_sample_window(cx, "markdown-images-remote");
    let path = markdown_file(
        &root,
        "guide.md",
        &format!("# Guide\n\n![the logo]({})\n", server.url("/logo.png")),
    );
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    wait_for_preview(cx, &view, "the logo");
    draw(cx, window);
    wait_for_picture(cx, window, "markdown-preview-image-0");

    assert_eq!(
        server.requests(),
        ["/logo.png"],
        "the picture comes from the server of the test"
    );
}

#[ui_kit::gpui::test]
fn no_picture_is_asked_for_when_the_setting_is_off(cx: &mut TestAppContext) {
    let server = PictureServer::start();
    let (root, window) = open_sample_window(cx, "markdown-images-off");
    cx.update(|cx| {
        cx.set_global(Settings {
            markdown_images_from_internet: false,
            ..Settings::default()
        })
    });
    let path = markdown_file(
        &root,
        "guide.md",
        &format!("# Guide\n\n![the logo]({})\n", server.url("/logo.png")),
    );
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    wait_for_preview(cx, &view, "the logo");
    draw(cx, window);
    draw(cx, window);
    assert!(
        size_of(cx, window, "markdown-preview-picture-0").is_some(),
        "the picture shows its text while the setting is off"
    );

    // A request that was sent would reach the server in this time.
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        server.requests().is_empty(),
        "the setting keeps the preview from asking the server: {:?}",
        server.requests()
    );
}

#[ui_kit::gpui::test]
fn an_html_picture_keeps_the_width_it_asks_for_and_stays_in_the_column(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "markdown-images-html");
    picture(&root, "logo.png");
    let path = markdown_file(
        &root,
        "guide.md",
        "# Guide\n\n<img width=\"30\" height=\"12\" alt=\"small\" src=\"logo.png\">\n\n\
         <img width=\"1042\" height=\"20\" alt=\"wide\" src=\"logo.png\">\n",
    );
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    wait_for_preview(cx, &view, "small");
    draw(cx, window);
    wait_for_picture(cx, window, "markdown-preview-image-0");
    wait_for_picture(cx, window, "markdown-preview-image-1");

    let (width, height) = size_of(cx, window, "markdown-preview-image-0").expect("the small one");
    assert!(
        (width - 30.).abs() <= 1.,
        "the width of the tag is the width of the picture: {width}"
    );
    assert!(
        (height - 12.).abs() <= 1.,
        "the height of the tag is the height of the picture: {height}"
    );
    let (wide, _) = size_of(cx, window, "markdown-preview-image-1").expect("the wide one");
    let pane = bounds_of(cx, window, "markdown-preview-pane");
    assert!(
        wide <= f32::from(pane.size.width),
        "a picture wider than the column of the preview stops at it: {wide} in {}",
        f32::from(pane.size.width)
    );
}

#[ui_kit::gpui::test]
fn a_click_on_a_picture_opens_its_link(cx: &mut TestAppContext) {
    let opened = Arc::new(Mutex::new(Vec::new()));
    let seen = opened.clone();
    cx.update(|cx| {
        cx.set_global(ExternalLinks(Some(Arc::new(move |url: &str| {
            seen.lock().unwrap().push(url.to_owned());
        }))));
    });
    let (root, window) = open_sample_window(cx, "markdown-images-link");
    picture(&root, "logo.png");
    let path = markdown_file(
        &root,
        "guide.md",
        "# Guide\n\n[![the logo](logo.png)](https://example.com/home)\n",
    );
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    wait_for_preview(cx, &view, "the logo");
    draw(cx, window);
    wait_for_picture(cx, window, "markdown-preview-image-0");

    click(cx, window, "markdown-preview-image-0");
    assert_eq!(
        opened.lock().unwrap().as_slice(),
        ["https://example.com/home"],
        "a picture inside a link opens it, as the words of a link do"
    );
}

/// A project of the test with the README and a second markdown file.
fn project(name: &str) -> PathBuf {
    let root = scratch(name);
    fs::write(root.join("README.md"), "# Read me\n\nWelcome.\n").unwrap();
    fs::write(root.join("guide.md"), "# Guide\n\nWords.\n").unwrap();
    root
}

#[ui_kit::gpui::test]
fn the_preview_mode_of_a_file_survives_a_second_open(cx: &mut TestAppContext) {
    let root = project("markdown-images-modes");
    let window = open_window(cx, root.clone());
    window
        .update(cx, |workspace, window, cx| {
            workspace.restore_tabs(window, cx)
        })
        .unwrap();
    cx.run_until_parked();

    let readme = root.join("README.md");
    let guide = root.join("guide.md");
    let readme_view = editor_of(cx, window, &readme);
    assert_eq!(
        readme_view.read_with(cx, |view, _| view.preview_mode()),
        Some(PreviewMode::PreviewOnly),
        "the README opens as the rendered document"
    );
    open_path(cx, window, &guide);
    draw(cx, window);
    click(cx, window, "markdown-mode-editor");
    draw(cx, window);
    let guide_view = editor_of(cx, window, &guide);
    assert_eq!(
        guide_view.read_with(cx, |view, _| view.preview_mode()),
        Some(PreviewMode::EditorOnly),
        "the second file keeps a mode of its own"
    );

    let (store, state_dir) = window
        .read_with(cx, |workspace, _| {
            (
                workspace.store().expect("the test window has a store"),
                workspace.state_dir().to_path_buf(),
            )
        })
        .unwrap();
    window
        .update(cx, |workspace, window, cx| {
            workspace.close_window(window, cx)
        })
        .unwrap();
    cx.run_until_parked();

    let saved = store
        .get(&format!("layout:{}", root.display()))
        .unwrap()
        .expect("the window saved the layout of the project");
    let layout: Layout = serde_json::from_str(&saved).unwrap();
    assert!(
        layout
            .preview_modes
            .iter()
            .any(|(path, mode)| path == &readme && *mode == PreviewLayout::PreviewOnly),
        "the saved layout keeps the README as the document alone: {:?}",
        layout.preview_modes
    );
    assert!(
        layout
            .preview_modes
            .iter()
            .any(|(path, mode)| path == &guide && *mode == PreviewLayout::EditorOnly),
        "the saved layout keeps the mode of the second file: {:?}",
        layout.preview_modes
    );

    // The second window stands for a new run of the IDE: the modes of the session are
    // gone, and only the saved layout remembers them.
    cx.update(|cx| cx.set_global(PreviewModes::default()));
    let launch = workspace::Launch {
        root: root.clone(),
        open: None,
        state_dir: state_dir.clone(),
        settings_dir: state_dir.clone(),
        icons: workspace::FileIcons {
            by_name: Vec::new(),
            fallback: "fileTypes/text",
        },
        background: true,
        info: Default::default(),
    };
    let second = cx.add_window(move |window, cx| {
        let mut workspace =
            Workspace::new_for_test(launch, layout, Some(store), Vec::new(), window, cx);
        let weak = cx.weak_entity();
        workspace.set_editor_factory(crate::editor_tab::factory(weak, state_dir));
        workspace.restore_tabs(window, cx);
        workspace
    });
    cx.run_until_parked();

    let readme_view = editor_of(cx, second, &readme);
    assert_eq!(
        readme_view.read_with(cx, |view, _| view.preview_mode()),
        Some(PreviewMode::PreviewOnly),
        "the README comes back as the rendered document"
    );
    let guide_view = editor_of(cx, second, &guide);
    assert_eq!(
        guide_view.read_with(cx, |view, _| view.preview_mode()),
        Some(PreviewMode::EditorOnly),
        "the second file comes back with its own mode"
    );
}
