//! A project whose build tool is not installed: the window offers a download and finds
//! the tool after it. The tool is a stand-in; its "download" comes from a local server,
//! so no test reaches the network.

use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

use base::Cancel;
use proc_sup::ProcessSpec;
use project_model::{BuildSystem, ProjectModel, ToolProgress, ToolSetup};
use ui_kit::gpui;
use ui_kit::gpui::TestAppContext;

use crate::run::ProjectSetup;
use crate::window_tests::{click, open_window_with, sample_project, scratch};
use crate::window_tests_run::{run_panel, wait_for};

/// A repository of the test: it answers every request with the same bytes.
pub(crate) struct Server {
    pub(crate) base: String,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl Server {
    pub(crate) fn start(payload: &'static [u8]) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a local port");
        let address = listener.local_addr().expect("the address of the listener");
        listener
            .set_nonblocking(true)
            .expect("a non-blocking listener");
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let handle = thread::spawn(move || {
            while !stopping.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut socket, _)) => {
                        // An accepted socket inherits the non-blocking mode of the
                        // listener on Windows, and the request would be half read.
                        socket.set_nonblocking(false).ok();
                        socket.set_read_timeout(Some(Duration::from_secs(10))).ok();
                        let mut head = Vec::new();
                        let mut buffer = [0u8; 1024];
                        while !head.windows(4).any(|window| window == b"\r\n\r\n") {
                            match socket.read(&mut buffer) {
                                Ok(0) => break,
                                Ok(read) => head.extend_from_slice(&buffer[..read]),
                                Err(_) => break,
                            }
                        }
                        let answer = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            payload.len()
                        );
                        let _ = socket
                            .write_all(answer.as_bytes())
                            .and_then(|_| socket.write_all(payload))
                            .and_then(|_| socket.flush());
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            base: format!("http://{address}"),
            stop,
            handle: Some(handle),
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// A build tool that fails its first sync and finds its program after the download.
struct FakeTool {
    root: PathBuf,
    data: PathBuf,
    server: String,
    syncs: AtomicUsize,
    installs: AtomicUsize,
}

impl FakeTool {
    fn new(root: PathBuf, data: PathBuf, server: String) -> Self {
        Self {
            root,
            data,
            server,
            syncs: AtomicUsize::new(0),
            installs: AtomicUsize::new(0),
        }
    }

    fn program(&self) -> PathBuf {
        self.data.join("tools/tool/bin/tool")
    }

    fn model(&self, version: Option<&str>) -> ProjectModel {
        ProjectModel {
            root: self.root.clone(),
            tool: "fake".into(),
            tool_version: version.map(str::to_owned),
            resolved: true,
            ..ProjectModel::default()
        }
    }
}

impl BuildSystem for FakeTool {
    fn detect(&self, _: &Path) -> bool {
        true
    }

    fn scan(&self, _: &Path) -> base::Result<ProjectModel> {
        Ok(self.model(None))
    }

    fn sync(&self, _: &Path, _: &Cancel) -> base::Result<ProjectModel> {
        self.syncs.fetch_add(1, Ordering::SeqCst);
        if self.program().is_file() {
            return Ok(self.model(Some("bundled")));
        }
        Err("no wrapper in the project and no tool in PATH".into())
    }

    fn assemble(&self, _: &Path) -> base::Result<ProcessSpec> {
        Ok(ProcessSpec::new("tool").args(["--version"]))
    }

    fn missing_tool(&self, _: &Path) -> Option<ToolSetup> {
        if self.program().is_file() {
            return None;
        }
        Some(ToolSetup {
            name: "Build tool".into(),
            title: "Build tool not found".into(),
            text: "The build tool of the project is not installed.".into(),
            download: Some("Download build tool".into()),
            choose: Some("Choose home\u{2026}".into()),
            create_wrapper: None,
            use_downloaded: None,
            settings: Some(("Build Tools".into(), "build_tool_first_home")),
        })
    }

    fn install_tool(
        &self,
        progress: &ToolProgress,
        cancel: &Cancel,
    ) -> base::Result<Option<PathBuf>> {
        self.installs.fetch_add(1, Ordering::SeqCst);
        let address = self
            .server
            .strip_prefix("http://")
            .ok_or("no server")?
            .to_owned();
        let mut socket =
            TcpStream::connect(&address).map_err(|error| format!("offline: {error}"))?;
        socket.set_read_timeout(Some(Duration::from_secs(10))).ok();
        socket
            .write_all(
                format!("GET /tool HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n")
                    .as_bytes(),
            )
            .map_err(|error| format!("offline: {error}"))?;
        let mut answer = Vec::new();
        socket
            .read_to_end(&mut answer)
            .map_err(|error| format!("offline: {error}"))?;
        if cancel.is_cancelled() {
            return Err("The download was cancelled.".into());
        }
        let body = answer
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .map(|at| answer[at + 4..].to_vec())
            .ok_or("the server answered no file")?;
        progress.set(body.len() as u64, Some(body.len() as u64));
        let program = self.program();
        if let Some(parent) = program.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&program, body)?;
        Ok(Some(self.data.join("tools/tool")))
    }

    fn tool_bin(&self) -> Option<PathBuf> {
        self.program().parent().map(Path::to_path_buf)
    }
}

fn setup(tool: Arc<FakeTool>) -> ProjectSetup {
    ProjectSetup {
        tool: Some(tool),
        providers: Vec::new(),
        tests: None,
        test_sites: |_, _| Vec::new(),
        cache_root: scratch("build-tool-missing-cache"),
        templates: Vec::new(),
        skipped_folders: Vec::new(),
        environment: None,
        debug: None,
    }
}

#[ui_kit::gpui::test]
fn a_missing_build_tool_offers_a_download_and_syncs_with_it(cx: &mut TestAppContext) {
    let root = sample_project("build-tool-missing");
    let data = scratch("build-tool-missing-data");
    let server = Server::start(b"a downloaded build tool\n");
    let tool = Arc::new(FakeTool::new(
        root.clone(),
        data.clone(),
        server.base.clone(),
    ));
    let window = open_window_with(cx, root, setup(tool.clone()));

    wait_for(cx, "the notification of the missing tool", |cx| {
        window
            .read_with(cx, |workspace, _| workspace.last_notification().is_some())
            .unwrap()
    });
    let (title, text) = window
        .read_with(cx, |workspace, _| workspace.last_notification())
        .unwrap()
        .expect("the sync failure names the missing tool");
    assert_eq!(title.to_string(), "Build tool not found");
    assert!(text.to_string().contains("not installed"), "{text}");
    let buttons: Vec<String> = window
        .read_with(cx, |workspace, _| workspace.last_notification_buttons())
        .unwrap()
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(
        buttons,
        ["Download build tool", "Choose home\u{2026}", "Build Tools"]
    );
    assert_eq!(tool.installs.load(Ordering::SeqCst), 0);

    click(cx, window, "notification-action");
    wait_for(cx, "the sync with the downloaded tool", |cx| {
        let panel = run_panel(cx);
        panel.read_with(cx, |panel, _| {
            panel
                .model()
                .and_then(|model| model.tool_version.clone())
                .as_deref()
                == Some("bundled")
        })
    });

    assert_eq!(tool.installs.load(Ordering::SeqCst), 1);
    assert!(tool.syncs.load(Ordering::SeqCst) >= 2, "the sync ran again");
    assert_eq!(
        fs::read_to_string(tool.program()).unwrap(),
        "a downloaded build tool\n",
        "the sync used the program of the download"
    );
}
