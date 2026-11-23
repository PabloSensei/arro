//! A Gradle project without a wrapper and without Gradle on the machine: the window
//! offers a download, and after it the sync finds the downloaded copy. The search, the
//! notification and the home are the ones the product uses; only the transfer comes from
//! a local server, so no test reaches the network and no JVM starts.

use std::fs;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use base::Cancel;
use proc_sup::ProcessSpec;
use project_model::{BuildSystem, ProjectModel, ToolHome, ToolProgress, ToolSetup};
use ui_kit::gpui;
use ui_kit::gpui::TestAppContext;
use window_language::{BuildEnv, Gradle};

use crate::run::ProjectSetup;
use crate::window_tests::{click, open_window_with, scratch};
use crate::window_tests_build_tool_missing::Server;
use crate::window_tests_run::{run_panel, wait_for};

const VERSION: &str = "9.1.0";

/// The real Gradle with the download of the test. The sync reports the home the search
/// finds rather than starting a JVM, so the test proves which copy the window picked.
struct Downloading {
    gradle: Gradle,
    data: PathBuf,
    server: String,
    installs: AtomicUsize,
    syncs: AtomicUsize,
}

impl Downloading {
    fn new(gradle: Gradle, data: PathBuf, server: String) -> Self {
        Self {
            gradle,
            data,
            server,
            installs: AtomicUsize::new(0),
            syncs: AtomicUsize::new(0),
        }
    }

    fn home(&self) -> PathBuf {
        self.data.join("tools").join("gradle").join(VERSION)
    }
}

impl BuildSystem for Downloading {
    fn detect(&self, root: &Path) -> bool {
        self.gradle.detect(root)
    }

    fn scan(&self, root: &Path) -> base::Result<ProjectModel> {
        self.gradle.scan(root)
    }

    fn sync(&self, root: &Path, _cancel: &Cancel) -> base::Result<ProjectModel> {
        self.syncs.fetch_add(1, Ordering::SeqCst);
        let found = self
            .gradle
            .resolve(root)
            .map_err(|error| error.to_string())?;
        Ok(ProjectModel {
            root: root.to_owned(),
            tool: "gradle".into(),
            tool_version: found
                .home
                .file_name()
                .map(|name| name.to_string_lossy().into_owned()),
            resolved: true,
            ..ProjectModel::default()
        })
    }

    fn assemble(&self, _: &Path) -> base::Result<ProcessSpec> {
        Err("not used".into())
    }

    fn missing_tool(&self, root: &Path) -> Option<ToolSetup> {
        self.gradle.missing_tool(root)
    }

    fn install_tool(
        &self,
        progress: &ToolProgress,
        cancel: &Cancel,
    ) -> base::Result<Option<PathBuf>> {
        self.installs.fetch_add(1, Ordering::SeqCst);
        let body = fetch(&self.server)?;
        if cancel.is_cancelled() {
            return Err("The download was cancelled.".into());
        }
        progress.set(body.len() as u64, Some(body.len() as u64));
        let home = self.home();
        let bin = home.join("bin");
        fs::create_dir_all(&bin)?;
        // The search looks for the name of the system the test runs on; the other one
        // makes the fixture the same everywhere.
        fs::write(bin.join("gradle"), &body)?;
        fs::write(bin.join("gradle.bat"), &body)?;
        Ok(Some(home))
    }

    fn use_home(&self, home: &Path) {
        self.gradle.use_home(home);
    }

    fn tool_bin(&self) -> Option<PathBuf> {
        self.gradle.tool_bin()
    }

    fn terminal_path(&self) -> bool {
        self.gradle.terminal_path()
    }
}

/// The body of the one answer of the test server.
fn fetch(server: &str) -> Result<Vec<u8>, String> {
    let address = server
        .strip_prefix("http://")
        .ok_or("no server")?
        .to_owned();
    let mut socket = TcpStream::connect(&address).map_err(|error| format!("offline: {error}"))?;
    socket.set_read_timeout(Some(Duration::from_secs(10))).ok();
    socket
        .write_all(
            format!("GET /gradle HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .map_err(|error| format!("offline: {error}"))?;
    let mut answer = Vec::new();
    socket
        .read_to_end(&mut answer)
        .map_err(|error| format!("offline: {error}"))?;
    let body = answer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|at| answer[at + 4..].to_vec())
        .ok_or("the server answered no file")?;
    Ok(body)
}

fn setup(tool: Arc<Downloading>) -> ProjectSetup {
    ProjectSetup {
        tool: Some(tool),
        providers: Vec::new(),
        tests: None,
        test_sites: |_, _| Vec::new(),
        cache_root: scratch("gradle-missing-cache"),
        templates: Vec::new(),
        skipped_folders: Vec::new(),
        environment: None,
        debug: None,
    }
}

#[ui_kit::gpui::test]
fn a_gradle_project_without_a_wrapper_offers_a_download_and_syncs_with_it(cx: &mut TestAppContext) {
    let root = scratch("gradle-missing-project");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("build.gradle"), "// a Gradle build\n").unwrap();
    let data = scratch("gradle-missing-data");
    let server = Server::start(b"a downloaded gradle\n");
    // The chosen home is the only source of the search, so the result does not depend on
    // the Gradle the machine happens to have on `PATH`.
    let mut build_tools = window_language::BuildTools::default();
    build_tools.second.home_kind = ToolHome::Custom;
    let env = BuildEnv {
        data_dir: Some(data.clone()),
        build_tools,
        ..BuildEnv::default()
    };
    let tool = Arc::new(Downloading::new(
        Gradle::new(env),
        data.clone(),
        server.base.clone(),
    ));
    let window = open_window_with(cx, root, setup(tool.clone()));

    wait_for(cx, "the notification of the missing Gradle", |cx| {
        window
            .read_with(cx, |workspace, _| workspace.last_notification().is_some())
            .unwrap()
    });
    let (title, text) = window
        .read_with(cx, |workspace, _| workspace.last_notification())
        .unwrap()
        .expect("the sync failure names the missing tool");
    assert_eq!(title.to_string(), "Gradle not found");
    assert!(text.to_string().contains("Create the wrapper"), "{text}");
    let buttons: Vec<String> = window
        .read_with(cx, |workspace, _| workspace.last_notification_buttons())
        .unwrap()
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(
        buttons,
        [
            "Download Gradle",
            "Choose Gradle home\u{2026}",
            "Create Gradle Wrapper",
            "Use Downloaded Gradle",
            "Build Tools \u{2192} Gradle"
        ]
    );
    assert_eq!(tool.installs.load(Ordering::SeqCst), 0);

    click(cx, window, "notification-action");
    wait_for(cx, "the sync with the downloaded Gradle", |cx| {
        let panel = run_panel(cx);
        panel.read_with(cx, |panel, _| {
            panel
                .model()
                .and_then(|model| model.tool_version.clone())
                .as_deref()
                == Some(VERSION)
        })
    });

    assert_eq!(tool.installs.load(Ordering::SeqCst), 1);
    assert!(tool.syncs.load(Ordering::SeqCst) >= 2, "the sync ran again");
    assert_eq!(
        fs::read_to_string(tool.home().join("bin/gradle")).unwrap(),
        "a downloaded gradle\n",
        "the sync found the copy of the download"
    );
    assert!(
        tool.gradle
            .resolve(Path::new(""))
            .is_ok_and(|found| found.home == tool.home()),
        "the chosen home is the one the search answers with"
    );
}
