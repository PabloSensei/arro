//! T-0262: a source without a driver names the product and offers the download of the
//! driver, and a driver that came from the stand-in repository takes the connection on
//! to the helper. No test reaches the network: the language side is the stand-in below.
//!
//! `cargo test -p panels window_tests_database_driver`

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use base::Cancel;
use database::{
    DownloadProgress, Driver, DriverKind, DriverPackage, Launch, Source, SourceId, Suggestion,
    Support, VendorTemplate,
};
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use workspace::{DATABASE_TOOL, Workspace};

use crate::database_panel::{ActiveDatabase, DatabasePanel};
use crate::window_tests::{click, draw, open_window, press, sample_project, wait_for_signal};

const SOURCE: &str = "owners";
const VERSION: &str = "42.7.4";

fn source() -> Source {
    Source {
        id: SourceId::new(SOURCE),
        name: "PetClinic".into(),
        kind: DriverKind::PostgreSql,
        url: "jdbc:postgresql://localhost:5432/pet".into(),
        user: "pet".into(),
        module: None,
        driver_jar: None,
    }
}

/// The language side of the window: no driver in the project, one driver to download and
/// a helper that stops before it starts. It records what the window asked it for.
struct StandIn {
    dir: PathBuf,
    calls: Arc<Mutex<Vec<Vec<PathBuf>>>>,
}

impl StandIn {
    fn new(dir: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            dir,
            calls: Arc::new(Mutex::new(Vec::new())),
        })
    }

    fn calls(&self) -> Arc<Mutex<Vec<Vec<PathBuf>>>> {
        self.calls.clone()
    }
}

impl Support for StandIn {
    fn vendors(&self) -> Vec<VendorTemplate> {
        Vec::new()
    }

    fn drivers(&self) -> Vec<Driver> {
        Vec::new()
    }

    fn suggestions(&self) -> Vec<Suggestion> {
        Vec::new()
    }

    fn helper(&self, jars: &[PathBuf]) -> Result<Launch, String> {
        self.calls
            .lock()
            .expect("the calls of the stand-in")
            .push(jars.to_vec());
        Err("the stand-in helper stops here".into())
    }

    fn packages(&self) -> Vec<DriverPackage> {
        vec![
            DriverPackage {
                kind: DriverKind::H2,
                coordinate: "com.h2database:h2".into(),
                license: "MPL-2.0 or EPL-1.0".into(),
                repository: "the stand-in repository".into(),
            },
            DriverPackage {
                kind: DriverKind::PostgreSql,
                coordinate: "org.postgresql:postgresql".into(),
                license: "PostgreSQL License".into(),
                repository: "the stand-in repository".into(),
            },
        ]
    }

    fn versions(&self, kind: DriverKind) -> Result<Vec<String>, String> {
        match kind {
            DriverKind::H2 | DriverKind::PostgreSql => {
                Ok(vec![VERSION.to_owned(), "42.7.3".to_owned()])
            }
            _ => Err("No driver can be downloaded for this source.".into()),
        }
    }

    fn download(
        &self,
        kind: DriverKind,
        version: Option<&str>,
        progress: &DownloadProgress,
        cancel: &Cancel,
    ) -> Result<PathBuf, String> {
        if !matches!(kind, DriverKind::H2 | DriverKind::PostgreSql) {
            return Err("No driver can be downloaded for this source.".into());
        }
        if cancel.is_cancelled() {
            return Err("The download was cancelled.".into());
        }
        let version = version.unwrap_or(VERSION);
        let folder = self.dir.join(version);
        fs::create_dir_all(&folder).map_err(|error| error.to_string())?;
        let jar = folder.join(format!("postgresql-{version}.jar"));
        fs::write(&jar, b"the stand-in jar").map_err(|error| error.to_string())?;
        progress.set(16, Some(16));
        Ok(jar)
    }
}

fn panel(cx: &TestAppContext) -> gpui::Entity<DatabasePanel> {
    cx.update(|cx| cx.global::<ActiveDatabase>().0.clone())
}

/// Opens the window, shows the Database tool and gives it the stand-in plus one source
/// without a driver.
fn source_shown(
    cx: &mut TestAppContext,
    name: &str,
) -> (
    WindowHandle<Workspace>,
    gpui::Entity<DatabasePanel>,
    Arc<StandIn>,
    test_support::TempDir,
) {
    let root = sample_project(name);
    let window = open_window(cx, root);
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(DATABASE_TOOL, window, cx)
        })
        .unwrap();
    draw(cx, window);
    let panel = panel(cx);
    let downloads = test_support::TempDir::new(name);
    let dir = downloads.path().to_path_buf();
    let support = StandIn::new(dir.clone());
    // The window reads the saved sources in the background when it is built; that read
    // must land before the test puts its own source in place.
    cx.run_until_parked();
    panel.update(cx, |panel, cx| {
        panel.set_support_for_test(support.clone());
        panel.sources_for_test(vec![source()], cx);
    });
    // The tree needs a dock that shows it whole, as a user would drag it.
    let focus = panel.read_with(cx, |panel, _| panel.focus());
    crate::window_tests::with_window(cx, window, |window, cx| window.focus(&focus, cx));
    for _ in 0..6 {
        press(cx, window, "ctrl-shift-up");
    }
    draw(cx, window);
    (window, panel, support, downloads)
}

fn jar_of(dir: &Path) -> PathBuf {
    dir.join(VERSION).join(format!("postgresql-{VERSION}.jar"))
}

/// Whether the element with a debug selector is on screen.
fn shown(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

#[ui_kit::gpui::test]
fn a_source_without_a_driver_names_it_and_offers_the_download(cx: &mut TestAppContext) {
    let (window, panel, _, _) = source_shown(cx, "database-driver-missing");

    assert!(
        shown(cx, window, "database-driver-missing"),
        "the source names its missing driver"
    );
    assert!(
        shown(cx, window, "database-download-driver"),
        "the button is there"
    );
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.driver_message_for_test()),
        Some("No PostgreSQL driver.".to_owned())
    );
}

#[ui_kit::gpui::test]
fn the_source_form_shows_the_version_of_the_repository(cx: &mut TestAppContext) {
    let (window, panel, _, _) = source_shown(cx, "database-driver-version");

    click(cx, window, "database-add");

    wait_for_signal(cx, "the version of the repository", |cx| {
        panel
            .read_with(cx, |panel, _| panel.dialog_version_for_test())
            .is_some()
    });
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.dialog_version_for_test()),
        Some(VERSION.to_owned())
    );
    assert!(
        shown(cx, window, "database-dialog-download"),
        "the form offers the download of the version it shows"
    );
}

#[ui_kit::gpui::test]
fn a_downloaded_driver_takes_the_connection_to_the_helper(cx: &mut TestAppContext) {
    let (window, panel, support, downloads) = source_shown(cx, "database-driver-download");

    click(cx, window, "database-download-driver");

    let calls = support.calls();
    wait_for_signal(cx, "the helper of the source", |_| {
        !calls.lock().expect("the calls of the stand-in").is_empty()
    });
    let jars = calls
        .lock()
        .expect("the calls of the stand-in")
        .last()
        .cloned()
        .expect("the helper was called");
    let jar = jar_of(downloads.path());
    assert_eq!(
        jars,
        vec![jar.clone()],
        "the helper gets the downloaded jar"
    );
    assert!(jar.is_file(), "the download wrote the file");
    let source = panel
        .read_with(cx, |panel, _| panel.sources())
        .into_iter()
        .find(|source| source.id == SourceId::new(SOURCE))
        .expect("the source is still there");
    assert_eq!(source.driver_jar, Some(jar));
    let error = panel
        .read_with(cx, |panel, _| panel.last_error())
        .expect("the helper answered the test with an error");
    assert!(error.contains("the stand-in helper stops here"), "{error}");
}
