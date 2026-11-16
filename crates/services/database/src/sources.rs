//! The connections of a project and the secrets of their users.
//!
//! A source keeps what a project may share: its kind, its address, its user and the module
//! whose driver it uses. The password is a [`Secret`]: it is never written to a file, never
//! put on a command line and never shown by [`std::fmt::Debug`].

use std::fmt;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use base::Cancel;
use serde::{Deserialize, Serialize};

use crate::session::Launch;

/// The folder of the sources and of the console files inside the state folder.
const FOLDER: &str = "database";
const SOURCES_FILE: &str = "sources.json";
const CONSOLE_FOLDER: &str = "console";
const CONSOLE_FILE: &str = "console.sql";

/// The stable name of a source, from the folder of its console.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SourceId(String);

impl SourceId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SourceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// The database products the dialog offers. The address template of each lives in the
/// language layer, which is the only place that knows how its driver spells one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum DriverKind {
    #[default]
    H2,
    PostgreSql,
    MySql,
    MariaDb,
    Sqlite,
    Oracle,
    SqlServer,
}

impl DriverKind {
    /// The name of the product, as the window and the form show it.
    pub fn title(self) -> &'static str {
        match self {
            Self::H2 => "H2",
            Self::PostgreSql => "PostgreSQL",
            Self::MySql => "MySQL",
            Self::MariaDb => "MariaDB",
            Self::Sqlite => "SQLite",
            Self::Oracle => "Oracle",
            Self::SqlServer => "SQL Server",
        }
    }
}

/// One connection, as the project state keeps it. No password is a part of it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    pub id: SourceId,
    pub name: String,
    pub kind: DriverKind,
    /// The address as the driver wrote it, from the user or from a project file.
    pub url: String,
    pub user: String,
    /// The module whose dependencies hold the driver, when one was found.
    pub module: Option<String>,
    /// A driver file the user picked, when it is not among the dependencies.
    pub driver_jar: Option<PathBuf>,
}

/// A password that a caller may pass to a helper and read nowhere else.
pub use platform::secrets::Secret;

/// A product of the dialog and the address its driver expects.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VendorTemplate {
    pub kind: DriverKind,
    pub title: String,
    /// `{host}`, `{port}`, `{database}` and `{file}` are filled by the dialog.
    pub url_template: String,
    pub host: String,
    pub port: Option<u16>,
}

/// A driver the project already has, with the files that carry it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Driver {
    pub kind: DriverKind,
    pub label: String,
    pub jars: Vec<PathBuf>,
    pub module: Option<String>,
}

/// A driver the language layer can fetch from the repository of its ecosystem, with the
/// license the window shows next to the button.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DriverPackage {
    pub kind: DriverKind,
    /// The coordinate of the artifact in its repository, as the window shows it.
    pub coordinate: String,
    /// The license of the driver, as the window shows it.
    pub license: String,
    /// The repository the file comes from, as the window shows it.
    pub repository: String,
}

/// How far a download has come: the bytes written and the size the server announced.
#[derive(Debug, Default)]
pub struct DownloadProgress {
    written: AtomicU64,
    total: AtomicU64,
}

impl DownloadProgress {
    pub fn new() -> Self {
        Self::default()
    }

    /// Records the bytes written so far; `total` is `None` until the server says.
    pub fn set(&self, written: u64, total: Option<u64>) {
        self.written.store(written, Ordering::Relaxed);
        self.total.store(total.unwrap_or(0), Ordering::Relaxed);
    }

    /// The bytes written so far.
    pub fn written(&self) -> u64 {
        self.written.load(Ordering::Relaxed)
    }

    /// The whole size of the file, when the server announced it.
    pub fn total(&self) -> Option<u64> {
        match self.total.load(Ordering::Relaxed) {
            0 => None,
            total => Some(total),
        }
    }
}

/// A source a project file suggests, for the dialog to offer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Suggestion {
    pub module: String,
    /// The profile of the file, as `application-<profile>.properties` names it; `None`
    /// for a file that every profile shares.
    pub profile: Option<String>,
    pub url: String,
    pub user: String,
    pub kind: Option<DriverKind>,
    pub driver_jar: Option<PathBuf>,
}

/// What the language layer adds to the window: the address templates of its drivers, the
/// drivers of the project and how to start the helper with them.
pub trait Support: Send + Sync {
    fn vendors(&self) -> Vec<VendorTemplate>;

    fn drivers(&self) -> Vec<Driver>;

    fn suggestions(&self) -> Vec<Suggestion>;

    /// Starts the helper with the given driver files. The launch carries no password.
    fn helper(&self, jars: &[PathBuf]) -> Result<Launch, String>;

    /// The drivers of the language layer that the user may download.
    fn packages(&self) -> Vec<DriverPackage> {
        Vec::new()
    }

    /// The stable versions of the driver of `kind`, the newest first. Reads the network:
    /// the window calls it away from its frame.
    fn versions(&self, _kind: DriverKind) -> Result<Vec<String>, String> {
        Err("No driver can be downloaded for this source.".to_owned())
    }

    /// Downloads the driver of `kind`, or the newest stable version when `version` is
    /// empty, into the data folder of the IDE and returns its jar. Reads the network and
    /// writes a file, so the window calls it away from its frame. A driver downloaded
    /// earlier is returned without a request. `progress` follows the bytes and `cancel`
    /// stops the download.
    fn download(
        &self,
        _kind: DriverKind,
        _version: Option<&str>,
        _progress: &DownloadProgress,
        _cancel: &Cancel,
    ) -> Result<PathBuf, String> {
        Err("No driver can be downloaded for this source.".to_owned())
    }
}

/// The sources of one project, kept in its state folder. Passwords are not a part of it.
#[derive(Clone, Debug, Default)]
pub struct SourceStore {
    pub sources: Vec<Source>,
}

#[derive(Serialize, Deserialize)]
struct Saved {
    version: u32,
    sources: Vec<Source>,
}

impl SourceStore {
    /// Reads the sources. A missing or broken file is an empty list: the file only keeps
    /// what the dialog entered, and the next save writes it again.
    pub fn load(state_dir: &Path) -> Self {
        let file = sources_file(state_dir);
        fs::read_to_string(file)
            .ok()
            .and_then(|text| serde_json::from_str::<Saved>(&text).ok())
            .map(|saved| Self {
                sources: saved.sources,
            })
            .unwrap_or_default()
    }

    /// Writes the sources through a temporary file, so that a crash in the middle of the
    /// write does not lose the connections the user entered earlier.
    pub fn save(&self, state_dir: &Path) -> io::Result<()> {
        let file = sources_file(state_dir);
        if let Some(directory) = file.parent() {
            fs::create_dir_all(directory)?;
        }
        let saved = Saved {
            version: 1,
            sources: self.sources.clone(),
        };
        let text = serde_json::to_string_pretty(&saved)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let temporary = file.with_extension("tmp");
        fs::write(&temporary, text)?;
        match fs::rename(&temporary, &file) {
            Ok(()) => Ok(()),
            Err(error) => {
                let _ = fs::remove_file(&temporary);
                Err(error)
            }
        }
    }
}

/// The folder of the state of the database window.
pub fn state_folder(state_dir: &Path) -> PathBuf {
    state_dir.join(FOLDER)
}

fn sources_file(state_dir: &Path) -> PathBuf {
    state_folder(state_dir).join(SOURCES_FILE)
}

/// The console file of a source: a `.sql` file in the state folder of the project.
pub fn console_path(state_dir: &Path, source: &SourceId) -> PathBuf {
    state_folder(state_dir)
        .join(CONSOLE_FOLDER)
        .join(safe_name(source.as_str()))
        .join(CONSOLE_FILE)
}

/// Creates the folder of the console file and an empty file, when they are not there yet.
pub fn ensure_console(state_dir: &Path, source: &SourceId) -> io::Result<PathBuf> {
    let file = console_path(state_dir, source);
    if let Some(directory) = file.parent() {
        fs::create_dir_all(directory)?;
    }
    if !file.exists() {
        fs::write(&file, "")?;
    }
    Ok(file)
}

/// The source whose console is `path`, when the file lies in the console folder of one.
pub fn source_of_console(state_dir: &Path, path: &Path) -> Option<SourceId> {
    let name = path.file_name()?;
    if !name.eq_ignore_ascii_case(CONSOLE_FILE) {
        return None;
    }
    let root = state_folder(state_dir).join(CONSOLE_FOLDER);
    // The file may not exist yet while the editor asks whether Ctrl+Enter belongs to it.
    let (root, path) = match (root.canonicalize(), path.canonicalize()) {
        (Ok(root), Ok(path)) => (root, path),
        _ => (root, path.to_path_buf()),
    };
    let relative = path.strip_prefix(&root).ok()?;
    let mut parts = relative.components();
    let Component::Normal(folder) = parts.next()? else {
        return None;
    };
    let source = folder.to_str()?;
    match parts.next() {
        Some(Component::Normal(file)) if file.eq_ignore_ascii_case(CONSOLE_FILE) => {
            Some(SourceId::new(source))
        }
        _ => None,
    }
}

/// A name of a folder that keeps the identity of a source and cannot leave the state
/// folder: only letters, digits, `-` and `_` stay.
fn safe_name(id: &str) -> String {
    let name: String = id
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect();
    if name.is_empty() || name == "." || name == ".." {
        "source".to_owned()
    } else {
        name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_saved_source_is_read_back_without_a_password() {
        let directory = test_support::TempDir::new("database-sources");
        let store = SourceStore {
            sources: vec![Source {
                id: SourceId::new("owners"),
                name: "Owners".into(),
                kind: DriverKind::H2,
                url: "jdbc:h2:mem:owners".into(),
                user: "sa".into(),
                module: Some("app".into()),
                driver_jar: None,
            }],
        };

        store.save(directory.path()).unwrap();
        let read = SourceStore::load(directory.path());

        assert_eq!(read.sources, store.sources);
        let text = fs::read_to_string(sources_file(directory.path())).unwrap();
        assert!(!text.contains("password"));
    }

    #[test]
    fn a_console_file_names_its_source_and_nothing_outside_it() {
        let directory = test_support::TempDir::new("database-console");
        let source = SourceId::new("owners");
        let console = ensure_console(directory.path(), &source).unwrap();
        fs::write(&console, "SELECT 1;").unwrap();

        assert_eq!(
            source_of_console(directory.path(), &console),
            Some(source.clone())
        );
        assert_eq!(source_of_console(directory.path(), &console), Some(source));
        assert_eq!(
            source_of_console(directory.path(), &directory.path().join("other.sql")),
            None
        );
    }

    #[test]
    fn a_name_with_a_separator_cannot_leave_the_state_folder() {
        let path = console_path(Path::new("state"), &SourceId::new("../old/h2"));

        assert!(!path.to_string_lossy().contains(".."));
        assert_eq!(
            path,
            Path::new("state/database/console/___old_h2/console.sql")
        );
    }
}
