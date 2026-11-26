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

/// The password names a JDBC address may carry as a parameter or a property, as in
/// `jdbc:postgresql://db/owners?password=s3cret` or `jdbc:sqlserver://db;password=s3cret`.
const PASSWORD_NAMES: &[&str] = &["password", "passwd", "pwd"];

/// The parameters of an address that make its driver run code or load a script as soon as
/// it connects. Such an address is refused: a connection is a place to read a database, not
/// a way to run what a project file or an agent wrote.
///
/// * `INIT`, `RUNSCRIPT` run a script of the product (H2).
/// * `autoDeserialize` makes the product read a serialized object and build it (MySQL).
/// * `socketFactory`, `socketFactoryArg`, `sslfactory` name a class the driver builds, which
///   is code from outside the project (PostgreSQL, MySQL).
/// * `allowLoadLocalInfile`, `allowLoadLocalInfileInPath`, `allowUrlInLocalInfile` read a
///   file of this machine or a URL on the request of the server (MySQL).
const RUNNING_PARAMETERS: &[&str] = &[
    "INIT",
    "RUNSCRIPT",
    "autoDeserialize",
    "socketFactory",
    "socketFactoryArg",
    "sslfactory",
    "allowLoadLocalInfile",
    "allowLoadLocalInfileInPath",
    "allowUrlInLocalInfile",
];

/// The parameter of an address that runs code or loads a script, with the reason the window
/// shows; `None` for an address that only names a database.
pub fn running_parameter(url: &str) -> Option<String> {
    let name = url
        .split(['?', ';', '&'])
        .skip(1)
        .filter_map(|token| token.split(['=', ' ']).next())
        .map(str::trim)
        .find(|name| {
            RUNNING_PARAMETERS
                .iter()
                .any(|known| name.eq_ignore_ascii_case(known))
        })?;
    Some(format!(
        "The address sets {name}, which runs code or loads a script when the driver connects."
    ))
}

/// The parameters of an address after its first `?` or `;`, as name and value.
fn parameters(url: &str) -> impl Iterator<Item = (&str, &str)> {
    url.split(['?', ';', '&'])
        .skip(1)
        .filter_map(|token| token.split_once('='))
        .map(|(name, value)| (name.trim(), value.trim()))
}

/// Whether two addresses name another machine, port or database, so that a password kept
/// for the first must not go to the second. An address that cannot be read counts as
/// another one: asking for the password again is safe, sending it to a stranger is not.
pub fn points_elsewhere(old: &str, new: &str) -> bool {
    address_target(old) != address_target(new)
}

/// The product, host, port and database of an address, as two addresses compare.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct AddressTarget {
    product: String,
    host: String,
    port: String,
    database: String,
}

fn address_target(url: &str) -> AddressTarget {
    let text = url.trim();
    let text = if text
        .get(..5)
        .is_some_and(|head| head.eq_ignore_ascii_case("jdbc:"))
    {
        &text[5..]
    } else {
        text
    };
    let (product, base) = match text.find(':') {
        Some(at) => (text[..at].to_ascii_lowercase(), &text[at + 1..]),
        None => (text.to_ascii_lowercase(), ""),
    };
    let base = strip_user_information(base);
    let mut target = AddressTarget {
        product,
        ..AddressTarget::default()
    };
    // The part after `//` of an address, or after the `://` of a sub-protocol such as
    // `jdbc:h2:tcp://host/db`.
    let authority = base
        .strip_prefix("//")
        .or_else(|| base.find("://").map(|at| &base[at + 3..]));
    let rest = if let Some(after) = authority {
        let end = after.find(['/', '?', ';', '#']).unwrap_or(after.len());
        let (host, port) = split_authority(&after[..end]);
        target.host = host;
        target.port = port;
        after.get(end..).unwrap_or_default()
    } else if let Some(after) = base.strip_prefix('@') {
        // The thin form of Oracle: `@host:port:sid`.
        let mut parts = after.splitn(3, ':');
        target.host = parts.next().unwrap_or_default().trim().to_ascii_lowercase();
        target.port = parts.next().unwrap_or_default().trim().to_owned();
        target.database = parts.next().unwrap_or_default().trim().to_owned();
        return target;
    } else {
        &base
    };
    target.database = database_of(rest);
    if target.port.is_empty() {
        target.port = default_port(&target.product).to_owned();
    }
    target
}

/// `user:password@` in front of the address, without the user information: a change of it is
/// not a change of the machine.
fn strip_user_information(base: &str) -> String {
    let start = if base.starts_with("//") {
        Some(2)
    } else {
        base.find("://").map(|at| at + 3)
    };
    let Some(start) = start else {
        return base.to_owned();
    };
    let text = &base[start..];
    let Some(at) = text.find('@') else {
        return base.to_owned();
    };
    let end = text.find(['/', '?', ';', '#']).unwrap_or(text.len());
    if at > end {
        return base.to_owned();
    }
    format!("{}{}", &base[..start], &text[at + 1..])
}

/// The host and the port of an authority such as `db.local:5432` or `[::1]:5432`.
fn split_authority(authority: &str) -> (String, String) {
    if let Some(end) = authority.find(']') {
        let host = authority[..=end].to_ascii_lowercase();
        let port = authority[end + 1..]
            .strip_prefix(':')
            .unwrap_or_default()
            .to_owned();
        return (host, port);
    }
    match authority.rsplit_once(':') {
        Some((host, port))
            if !port.is_empty() && port.chars().all(|digit| digit.is_ascii_digit()) =>
        {
            (host.to_ascii_lowercase(), port.to_owned())
        }
        _ => (authority.to_ascii_lowercase(), String::new()),
    }
}

/// The database of the rest of an address: a `databaseName` or `database` property when one
/// is there, and otherwise the first part of the path.
fn database_of(rest: &str) -> String {
    if let Some((_, value)) = parameters(rest).find(|(name, _)| {
        name.eq_ignore_ascii_case("databaseName") || name.eq_ignore_ascii_case("database")
    }) {
        return value.to_owned();
    }
    rest.split(['?', ';'])
        .next()
        .unwrap_or_default()
        .trim_start_matches('/')
        .to_owned()
}

/// The port a product uses when the address names none.
fn default_port(product: &str) -> &'static str {
    match product {
        "postgresql" | "postgres" => "5432",
        "mysql" | "mariadb" => "3306",
        "sqlserver" => "1433",
        "oracle" => "1521",
        "h2" => "9092",
        _ => "",
    }
}

/// The address without its password, and the password it carried: in a `password`,
/// `passwd` or `pwd` parameter or after the user of its user information. An address that
/// keeps a password is never written to the state folder.
pub fn split_password(url: &str) -> (String, Option<Secret>) {
    let (text, user_information) = strip_user_password(url);
    let (text, parameter) = strip_parameter_password(&text);
    let password = parameter
        .or(user_information)
        .filter(|text| !text.is_empty());
    (text, password.map(Secret::new))
}

/// `scheme://user:password@host` without the password of the user information.
fn strip_user_password(text: &str) -> (String, Option<String>) {
    let Some(scheme) = text.find("://") else {
        return (text.to_owned(), None);
    };
    let rest = &text[scheme + 3..];
    let Some(at) = rest.find('@') else {
        return (text.to_owned(), None);
    };
    let host_end = rest.find(['/', '?', ';', '#']).unwrap_or(rest.len());
    if at > host_end {
        return (text.to_owned(), None);
    }
    let user_information = &rest[..at];
    let Some(colon) = user_information.find(':') else {
        return (text.to_owned(), None);
    };
    let password = user_information[colon + 1..].to_owned();
    (
        format!("{}{}{}", &text[..scheme + 3], &rest[..colon], &rest[at..]),
        Some(password),
    )
}

/// The address without its password parameters, and the first password they carried.
fn strip_parameter_password(text: &str) -> (String, Option<String>) {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    let mut password = None;
    let mut at = 0;
    while at < bytes.len() {
        let starts_a_value = at == 0 || matches!(bytes[at - 1], b'?' | b'&' | b';');
        if !starts_a_value {
            at += 1;
            continue;
        }
        let Some(name) = PASSWORD_NAMES.iter().find(|name| {
            text.get(at..at + name.len())
                .is_some_and(|head| head.eq_ignore_ascii_case(name))
        }) else {
            at += 1;
            continue;
        };
        let mut cursor = at + name.len();
        if bytes.get(cursor) != Some(&b'=') {
            at = cursor;
            continue;
        }
        cursor += 1;
        let value_start = cursor;
        while bytes
            .get(cursor)
            .is_some_and(|byte| !matches!(byte, b'&' | b';' | b'\n' | b'\r' | b' '))
        {
            cursor += 1;
        }
        if password.is_none() {
            password = Some(text[value_start..cursor].to_owned());
        }
        // The chunk goes with the separator that opened it, so no `&&` or `;` is left; a `?`
        // stays when another parameter follows it, and goes otherwise.
        let (from, to) = match at.checked_sub(1).map(|index| bytes[index]) {
            Some(b'?') => match bytes.get(cursor) {
                Some(b'&' | b';') => (at, cursor + 1),
                _ => (at - 1, cursor),
            },
            Some(b'&' | b';') => (at - 1, cursor),
            _ => (at, cursor),
        };
        out.push_str(&text[copied..from]);
        copied = to;
        at = to;
    }
    out.push_str(&text[copied..]);
    (out, password)
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

    #[test]
    fn another_host_port_or_database_is_another_target() {
        assert!(!points_elsewhere(
            "jdbc:postgresql://db.local:5432/owners",
            "jdbc:postgresql://db.local:5432/owners"
        ));
        assert!(!points_elsewhere(
            "jdbc:postgresql://DB.LOCAL/owners",
            "jdbc:postgresql://db.local:5432/owners"
        ));
        assert!(!points_elsewhere(
            "jdbc:postgresql://user:first@db.local/owners",
            "jdbc:postgresql://user:second@db.local/owners?password=x"
        ));
        assert!(points_elsewhere(
            "jdbc:postgresql://db.local:5432/owners",
            "jdbc:postgresql://other.local:5432/owners"
        ));
        assert!(points_elsewhere(
            "jdbc:postgresql://db.local:5432/owners",
            "jdbc:postgresql://db.local:5433/owners"
        ));
        assert!(points_elsewhere(
            "jdbc:postgresql://db.local:5432/owners",
            "jdbc:postgresql://db.local:5432/neighbours"
        ));
        assert!(points_elsewhere(
            "jdbc:sqlserver://db.local;databaseName=owners",
            "jdbc:sqlserver://db.local;databaseName=neighbours"
        ));
        assert!(points_elsewhere(
            "jdbc:h2:tcp://db.local/~/owners",
            "jdbc:h2:tcp://other.local/~/owners"
        ));
        assert!(!points_elsewhere(
            "jdbc:h2:mem:owners",
            "jdbc:h2:mem:owners"
        ));
        assert!(points_elsewhere(
            "jdbc:h2:mem:owners",
            "jdbc:h2:mem:neighbours"
        ));
    }

    #[test]
    fn an_address_that_runs_code_or_a_script_is_named() {
        for url in [
            "jdbc:h2:mem:x;INIT=RUNSCRIPT FROM 'http://a'",
            "jdbc:h2:mem:x;runscript from 'x.sql'",
            "jdbc:mysql://db/x?autoDeserialize=true",
            "jdbc:postgresql://db/x?socketFactory=com.example.Factory",
            "jdbc:mysql://db/x?allowLoadLocalInfile=true",
            "jdbc:mysql://db/x?allowUrlInLocalInfile=true&user=app",
        ] {
            assert!(running_parameter(url).is_some(), "{url}");
        }
        assert!(running_parameter("jdbc:h2:mem:x;DB_CLOSE_DELAY=-1").is_none());
        assert!(running_parameter("jdbc:postgresql://db/x?user=app&ssl=true").is_none());
    }

    #[test]
    fn a_password_of_the_address_goes_to_its_owner() {
        assert_eq!(
            split_password("jdbc:postgresql://db/owners?user=app&password=s3cret"),
            (
                "jdbc:postgresql://db/owners?user=app".to_owned(),
                Some(Secret::new("s3cret"))
            )
        );
        assert_eq!(
            split_password("jdbc:postgresql://db/owners?password=s3cret&user=app"),
            (
                "jdbc:postgresql://db/owners?user=app".to_owned(),
                Some(Secret::new("s3cret"))
            )
        );
        assert_eq!(
            split_password("jdbc:postgresql://db/owners?password=s3cret"),
            (
                "jdbc:postgresql://db/owners".to_owned(),
                Some(Secret::new("s3cret"))
            )
        );
        assert_eq!(
            split_password("jdbc:sqlserver://db;user=app;password=s3cret;encrypt=true"),
            (
                "jdbc:sqlserver://db;user=app;encrypt=true".to_owned(),
                Some(Secret::new("s3cret"))
            )
        );
        assert_eq!(
            split_password("jdbc:mysql://user:s3cret@db/owners"),
            (
                "jdbc:mysql://user@db/owners".to_owned(),
                Some(Secret::new("s3cret"))
            )
        );
        assert_eq!(
            split_password("jdbc:h2:mem:test"),
            ("jdbc:h2:mem:test".to_owned(), None)
        );
    }
}
