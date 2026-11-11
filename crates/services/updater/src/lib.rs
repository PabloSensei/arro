//! Version checks and in-place updates from GitHub Releases. There is no UI and no
//! network work on its own: the caller runs every request away from the interface.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};

/// Where the latest release is described.
pub const RELEASES_URL: &str = "https://api.github.com/repos/PabloSensei/arro/releases/latest";
/// Replaces [`RELEASES_URL`], for tests and for a moved repository.
pub const UPDATE_URL_ENV: &str = "ARRO_UPDATE_URL";
/// Set to `1` to turn the automatic check off.
pub const NO_UPDATE_CHECK_ENV: &str = "ARRO_NO_UPDATE_CHECK";
/// The asset suffix of the only platform the distribution is made for.
pub const PLATFORM: &str = "windows-x86_64";

const USER_AGENT: &str = "Arro";
const TIMEOUT: Duration = Duration::from_secs(60);

/// One file of a release.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub url: String,
}

/// A release that is newer than the running version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub version: Version,
    pub tag: String,
    /// The text of the release.
    pub notes: String,
    /// The page of the release in a browser.
    pub page_url: String,
    pub archive: Asset,
    pub checksum: Asset,
}

impl Release {
    /// The version as the notification writes it, `0.2.0`.
    pub fn version_text(&self) -> String {
        self.version.to_string()
    }
}

/// Where the release information comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateSource {
    url: String,
}

impl Default for UpdateSource {
    fn default() -> Self {
        Self::new()
    }
}

impl UpdateSource {
    /// [`RELEASES_URL`], unless the environment names another address.
    pub fn new() -> Self {
        let url = std::env::var(UPDATE_URL_ENV)
            .ok()
            .filter(|url| !url.trim().is_empty())
            .unwrap_or_else(|| RELEASES_URL.to_owned());
        Self { url }
    }

    pub fn with_url(url: impl Into<String>) -> Self {
        Self { url: url.into() }
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    /// The newer release, or `None` when the running version is the latest. Prereleases
    /// and drafts are skipped.
    pub fn check(&self, current: &Version) -> Result<Option<Release>, Error> {
        let text = self.fetch_text(self.url.as_str())?;
        let Some(release) = parse_release(&text)? else {
            return Ok(None);
        };
        Ok((release.version > *current).then_some(release))
    }

    /// Downloads the archive of `release` into `dir`, checks its SHA-256 and returns the
    /// file. A failed or interrupted download leaves nothing behind.
    pub fn download(&self, release: &Release, dir: &Path) -> Result<PathBuf, Error> {
        fs::create_dir_all(dir)?;
        let target = dir.join(&release.archive.name);
        let partial = dir.join(format!(".{}.part", release.archive.name));
        let result = self.download_inner(release, &partial);
        match result {
            Ok(()) => {
                if target.exists() {
                    fs::remove_file(&target)?;
                }
                fs::rename(&partial, &target)?;
                Ok(target)
            }
            Err(error) => {
                let _ = fs::remove_file(&partial);
                Err(error)
            }
        }
    }

    fn download_inner(&self, release: &Release, partial: &Path) -> Result<(), Error> {
        let bytes = self.fetch(release.archive.url.as_str())?;
        {
            let mut file = File::create(partial)?;
            file.write_all(&bytes)?;
        }
        let text = self.fetch_text(release.checksum.url.as_str())?;
        let expected = parse_checksum(&text)?;
        let actual = sha256_file(partial)?;
        if !expected.eq_ignore_ascii_case(&actual) {
            return Err(Error::Checksum { expected, actual });
        }
        Ok(())
    }

    fn fetch_text(&self, url: &str) -> Result<String, Error> {
        let bytes = self.fetch(url)?;
        String::from_utf8(bytes).map_err(|error| Error::Http(error.to_string()))
    }

    fn fetch(&self, url: &str) -> Result<Vec<u8>, Error> {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(TIMEOUT))
            .tls_config(
                ureq::tls::TlsConfig::builder()
                    .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                    .build(),
            )
            .build()
            .into();
        let mut response = agent
            .get(url)
            .header("User-Agent", USER_AGENT)
            .header("Accept", "application/vnd.github+json")
            .call()
            .map_err(|error| Error::Http(error.to_string()))?;
        response
            .body_mut()
            .read_to_vec()
            .map_err(|error| Error::Http(error.to_string()))
    }
}

/// The release in a GitHub Releases response. `None` for a draft or a prerelease.
pub fn parse_release(text: &str) -> Result<Option<Release>, Error> {
    let response: GithubRelease =
        serde_json::from_str(text).map_err(|error| Error::InvalidJson(error.to_string()))?;
    if response.draft || response.prerelease {
        return Ok(None);
    }
    let tag = response.tag_name.ok_or(Error::MissingField("tag_name"))?;
    let version = parse_tag(&tag)?;
    if !version.pre.is_empty() {
        return Ok(None);
    }
    let expected = archive_name(&version);
    let checksum_name = format!("{expected}.sha256");
    let archive = asset_of(&response.assets, &expected)
        .ok_or_else(|| Error::MissingAsset(expected.clone()))?;
    let checksum =
        asset_of(&response.assets, &checksum_name).ok_or(Error::MissingAsset(checksum_name))?;
    Ok(Some(Release {
        version,
        tag,
        notes: response.body.unwrap_or_default(),
        page_url: response.html_url.unwrap_or_default(),
        archive,
        checksum,
    }))
}

/// `v0.1.0` or `0.1.0` as a version.
pub fn parse_tag(tag: &str) -> Result<Version, Error> {
    let text = tag.trim();
    let text = text
        .strip_prefix('v')
        .or_else(|| text.strip_prefix('V'))
        .unwrap_or(text);
    Version::parse(text).map_err(|_| Error::InvalidVersion(tag.to_owned()))
}

/// What the archive of `version` is called.
pub fn archive_name(version: &Version) -> String {
    format!("arro-{version}-{PLATFORM}.zip")
}

/// A version string such as `0.1.0` as a version, for the running build.
pub fn parse_version(text: &str) -> Option<Version> {
    Version::parse(text.trim()).ok()
}

/// Replaces the files of `install_dir` with the contents of `zip`. The running executable
/// is renamed to `*.old` first; any error puts every file back the way it was.
pub fn stage(zip: &Path, install_dir: &Path) -> Result<(), Error> {
    let running = std::env::current_exe().ok().and_then(|path| {
        path.file_name()
            .map(|name| name.to_string_lossy().into_owned())
    });
    stage_as(zip, install_dir, running.as_deref())
}

/// [`stage`] with the name of the executable that is running. Tests name it themselves.
fn stage_as(zip: &Path, install_dir: &Path, running: Option<&str>) -> Result<(), Error> {
    let parent = install_dir.parent().unwrap_or(install_dir);
    let temp = parent.join(format!(".arro-stage-{}-{}", std::process::id(), next_id()));
    if temp.exists() {
        fs::remove_dir_all(&temp)?;
    }
    let extracted = temp.join("files");
    fs::create_dir_all(&extracted)?;

    let files = extract(zip, &extracted).inspect_err(|_| {
        let _ = fs::remove_dir_all(&temp);
    })?;
    // The release archive holds one folder named after the release; its contents go
    // into the install folder.
    let (staged, files) = match single_root(&files) {
        Some(root) => {
            let inner = files
                .iter()
                .filter_map(|file| file.strip_prefix(&root).ok().map(Path::to_path_buf))
                .collect();
            (extracted.join(&root), inner)
        }
        None => (extracted.clone(), files),
    };

    let mut transaction = Transaction::default();
    let result = transaction.replace(&staged, install_dir, &temp, &files, running);
    if let Err(error) = result {
        transaction.rollback();
        let _ = fs::remove_dir_all(&temp);
        return Err(error);
    }
    let _ = fs::remove_dir_all(&temp);
    Ok(())
}

/// Deletes the `*.old` files an earlier update left next to the executable.
pub fn remove_old(install_dir: &Path) -> io::Result<usize> {
    let mut removed = 0;
    for entry in fs::read_dir(install_dir)? {
        let entry = entry?;
        if entry.file_type()?.is_file()
            && entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "old")
        {
            fs::remove_file(entry.path())?;
            removed += 1;
        }
    }
    Ok(removed)
}

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: Option<String>,
    body: Option<String>,
    html_url: Option<String>,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    assets: Vec<GithubAsset>,
}

#[derive(Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
}

fn asset_of(assets: &[GithubAsset], name: &str) -> Option<Asset> {
    assets
        .iter()
        .find(|asset| asset.name == name)
        .map(|asset| Asset {
            name: asset.name.clone(),
            url: asset.browser_download_url.clone(),
        })
}

fn parse_checksum(text: &str) -> Result<String, Error> {
    let hash = text
        .split_whitespace()
        .next()
        .filter(|hash| hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit()))
        .ok_or_else(|| Error::ChecksumFile(text.trim().to_owned()))?;
    Ok(hash.to_ascii_lowercase())
}

fn sha256_file(path: &Path) -> Result<String, Error> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn extract(zip: &Path, into: &Path) -> Result<Vec<PathBuf>, Error> {
    let file = File::open(zip)?;
    let mut archive = zip::ZipArchive::new(file).map_err(|error| Error::Zip(error.to_string()))?;
    let mut files = Vec::new();
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| Error::Zip(error.to_string()))?;
        let Some(relative) = entry.enclosed_name() else {
            let name = entry
                .name()
                .map(|name| name.into_owned())
                .unwrap_or_default();
            return Err(Error::Zip(format!("{name} is outside the archive")));
        };
        let target = into.join(&relative);
        if entry.is_dir() {
            fs::create_dir_all(&target)?;
            continue;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut out = File::create(&target)?;
        io::copy(&mut entry, &mut out)?;
        files.push(relative);
    }
    Ok(files)
}

/// The one folder every file of the archive sits in, if there is such a folder.
fn single_root(files: &[PathBuf]) -> Option<PathBuf> {
    let mut root: Option<&std::ffi::OsStr> = None;
    for file in files {
        let mut parts = file.components();
        let first = parts.next()?.as_os_str();
        parts.next()?;
        match root {
            Some(known) if known != first => return None,
            _ => root = Some(first),
        }
    }
    root.map(PathBuf::from)
}

/// Whether two files hold the same bytes, read without loading them whole.
fn same_content(left: &Path, right: &Path) -> io::Result<bool> {
    if fs::metadata(left)?.len() != fs::metadata(right)?.len() {
        return Ok(false);
    }
    let mut left = io::BufReader::new(File::open(left)?);
    let mut right = io::BufReader::new(File::open(right)?);
    let mut a = [0u8; 64 * 1024];
    let mut b = [0u8; 64 * 1024];
    loop {
        let read = left.read(&mut a)?;
        if read == 0 {
            return Ok(true);
        }
        right.read_exact(&mut b[..read])?;
        if a[..read] != b[..read] {
            return Ok(false);
        }
    }
}

/// Files that were put in place, so that a failure can take them away again.
#[derive(Default)]
struct Transaction {
    created: Vec<PathBuf>,
    /// The backup and the place it came from.
    restored: Vec<(PathBuf, PathBuf)>,
}

impl Transaction {
    fn replace(
        &mut self,
        staged: &Path,
        install: &Path,
        temp: &Path,
        files: &[PathBuf],
        running: Option<&str>,
    ) -> Result<(), Error> {
        let backup = temp.join("backup");
        fs::create_dir_all(&backup)?;
        for relative in files {
            let target = install.join(relative);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            // An unchanged file stays where it is: the language server and the
            // terminal keep their jars and libraries open, and Windows does not let a
            // file in use be renamed.
            if target.is_file() && same_content(&staged.join(relative), &target)? {
                continue;
            }
            if target.exists() {
                let old = old_path(&target);
                if running.is_some_and(|name| relative.file_name().is_some_and(|file| file == name))
                {
                    if old.exists() {
                        fs::remove_file(&old)?;
                    }
                    fs::rename(&target, &old)?;
                    self.restored.push((old, target.clone()));
                } else {
                    let saved = backup.join(relative);
                    if let Some(parent) = saved.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    fs::rename(&target, &saved)?;
                    self.restored.push((saved, target.clone()));
                }
            }
            fs::copy(staged.join(relative), &target)?;
            self.created.push(target);
        }
        Ok(())
    }

    fn rollback(&mut self) {
        for path in self.created.drain(..).rev() {
            let _ = fs::remove_file(path);
        }
        for (saved, target) in self.restored.drain(..).rev() {
            let _ = fs::rename(saved, target);
        }
    }
}

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

fn next_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

/// `ide.exe` becomes `ide.exe.old`; the running file can be renamed on Windows.
fn old_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".old");
    PathBuf::from(name)
}

/// What can go wrong while checking or installing an update.
#[derive(Debug)]
pub enum Error {
    Http(String),
    InvalidJson(String),
    InvalidVersion(String),
    MissingField(&'static str),
    MissingAsset(String),
    ChecksumFile(String),
    Checksum { expected: String, actual: String },
    Zip(String),
    Io(io::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Http(message) => write!(formatter, "the update server did not answer: {message}"),
            Self::InvalidJson(message) => {
                write!(formatter, "the release is not valid JSON: {message}")
            }
            Self::InvalidVersion(tag) => {
                write!(formatter, "the release tag {tag} is not a version")
            }
            Self::MissingField(field) => write!(formatter, "the release has no {field}"),
            Self::MissingAsset(name) => write!(formatter, "the release has no asset {name}"),
            Self::ChecksumFile(text) => {
                write!(formatter, "the checksum file is not a SHA-256: {text}")
            }
            Self::Checksum { expected, actual } => {
                write!(formatter, "the archive is {actual}, not {expected}")
            }
            Self::Zip(message) => write!(formatter, "the archive is broken: {message}"),
            Self::Io(error) => write!(formatter, "the update cannot touch the files: {error}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::net::{TcpListener, TcpStream};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread::JoinHandle;

    fn version(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    fn release_json(tag: &str, prerelease: bool, assets: &[(&str, &str)]) -> String {
        let assets: Vec<String> = assets
            .iter()
            .map(|(name, url)| format!(r#"{{"name":"{name}","browser_download_url":"{url}"}}"#))
            .collect();
        format!(
            r#"{{"tag_name":"{tag}","name":"Arro {tag}","body":"Notes","html_url":"https://example.test/release","prerelease":{prerelease},"draft":false,"assets":[{}]}}"#,
            assets.join(",")
        )
    }

    struct Server {
        address: std::net::SocketAddr,
        stop: Arc<AtomicBool>,
        thread: Option<JoinHandle<()>>,
    }

    struct Reply {
        body: Vec<u8>,
        /// Bytes that reach the client before the connection closes.
        send: Option<usize>,
    }

    impl Reply {
        fn text(text: impl Into<String>) -> Self {
            Self {
                body: text.into().into_bytes(),
                send: None,
            }
        }

        fn bytes(body: Vec<u8>) -> Self {
            Self { body, send: None }
        }

        fn cut(body: Vec<u8>, send: usize) -> Self {
            Self {
                body,
                send: Some(send),
            }
        }
    }

    impl Server {
        fn start(routes: Vec<(String, Reply)>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            listener.set_nonblocking(true).unwrap();
            let stop = Arc::new(AtomicBool::new(false));
            let stopped = stop.clone();
            let routes = Arc::new(routes);
            let thread = std::thread::spawn(move || {
                while !stopped.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            handle(stream, &routes);
                        }
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
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

        fn url(&self, path: &str) -> String {
            format!("http://{}{path}", self.address)
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

    fn handle(mut stream: TcpStream, routes: &[(String, Reply)]) {
        // On Windows an accepted stream inherits the listener's non-blocking mode, and a
        // read before the request arrives would see nothing.
        stream.set_nonblocking(false).ok();
        stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut request = String::new();
        reader.read_line(&mut request).ok();
        // Read the headers up to the blank line: closing a socket with unread data resets
        // the connection on Windows.
        let mut line = String::new();
        while reader.read_line(&mut line).is_ok_and(|read| read > 2) {
            line.clear();
        }
        let path = request.split_whitespace().nth(1).unwrap_or("");
        let Some((_, reply)) = routes.iter().find(|(route, _)| route == path) else {
            let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
            return;
        };
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            reply.body.len()
        );
        stream.write_all(head.as_bytes()).ok();
        let sent = reply.send.unwrap_or(reply.body.len()).min(reply.body.len());
        stream.write_all(&reply.body[..sent]).ok();
        stream.flush().ok();
        stream.shutdown(std::net::Shutdown::Both).ok();
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "arro-updater-{}-{name}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_zip(path: &Path, files: &[(&str, &str)]) {
        let file = File::create(path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default();
        for (name, text) in files {
            writer.start_file(*name, options).unwrap();
            writer.write_all(text.as_bytes()).unwrap();
        }
        writer.finish().unwrap();
    }

    fn assets_for(tag: &str) -> (String, String) {
        let version = version(tag.trim_start_matches('v'));
        let archive = archive_name(&version);
        let checksum = format!("{archive}.sha256");
        (archive, checksum)
    }

    fn release_from(tag: &str, server: &Server) -> Release {
        let (archive, checksum) = assets_for(tag);
        let json = release_json(
            tag,
            false,
            &[
                (&archive, &server.url("/archive.zip")),
                (&checksum, &server.url("/archive.zip.sha256")),
            ],
        );
        parse_release(&json).unwrap().unwrap()
    }

    #[test]
    fn a_newer_tag_is_a_release() {
        let (archive, checksum) = assets_for("v0.2.0");
        let json = release_json(
            "v0.2.0",
            false,
            &[
                (&archive, "https://example.test/archive.zip"),
                (&checksum, "https://example.test/archive.zip.sha256"),
            ],
        );
        let server = Server::start(vec![("/latest".into(), Reply::text(json))]);
        let source = UpdateSource::with_url(server.url("/latest"));
        let release = source.check(&version("0.1.0")).unwrap().unwrap();
        assert_eq!(release.version, version("0.2.0"));
        assert_eq!(release.tag, "v0.2.0");
        assert_eq!(release.notes, "Notes");
        assert_eq!(release.archive.name, "arro-0.2.0-windows-x86_64.zip");
        assert_eq!(
            release.checksum.name,
            "arro-0.2.0-windows-x86_64.zip.sha256"
        );
    }

    #[test]
    fn the_same_and_older_tags_are_not_updates() {
        for tag in ["v0.1.0", "v0.0.9"] {
            let (archive, checksum) = assets_for(tag);
            let json = release_json(
                tag,
                false,
                &[
                    (&archive, "https://example.test/archive.zip"),
                    (&checksum, "https://example.test/archive.zip.sha256"),
                ],
            );
            let server = Server::start(vec![("/latest".into(), Reply::text(json))]);
            let source = UpdateSource::with_url(server.url("/latest"));
            assert!(source.check(&version("0.1.0")).unwrap().is_none(), "{tag}");
        }
    }

    #[test]
    fn a_prerelease_is_skipped() {
        let (archive, checksum) = assets_for("v0.2.0-beta.1");
        let json = release_json(
            "v0.2.0-beta.1",
            true,
            &[
                (&archive, "https://example.test/archive.zip"),
                (&checksum, "https://example.test/archive.zip.sha256"),
            ],
        );
        let server = Server::start(vec![("/latest".into(), Reply::text(json))]);
        let source = UpdateSource::with_url(server.url("/latest"));
        assert!(source.check(&version("0.1.0")).unwrap().is_none());
    }

    #[test]
    fn a_release_without_the_archive_is_an_error() {
        let json = release_json(
            "v0.2.0",
            false,
            &[(
                "arro-0.2.0-windows-x86_64.zip.sha256",
                "https://example.test/archive.zip.sha256",
            )],
        );
        let error = parse_release(&json).unwrap_err();
        assert!(matches!(error, Error::MissingAsset(_)), "{error}");
    }

    #[test]
    fn a_wrong_checksum_removes_the_archive() {
        let zip = b"PK\x03\x04not really a zip".to_vec();
        let server = Server::start(vec![
            ("/archive.zip".into(), Reply::bytes(zip)),
            (
                "/archive.zip.sha256".into(),
                Reply::text(format!("{}  arro.zip\n", "0".repeat(64))),
            ),
        ]);
        let release = release_from("v0.2.0", &server);
        let dir = temp("checksum");
        let error = server_source(&server).download(&release, &dir).unwrap_err();
        assert!(matches!(error, Error::Checksum { .. }), "{error}");
        assert!(!dir.join(&release.archive.name).exists());
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
        fs::remove_dir_all(dir).ok();
    }

    fn server_source(server: &Server) -> UpdateSource {
        UpdateSource::with_url(server.url("/latest"))
    }

    #[test]
    fn an_interrupted_download_leaves_nothing() {
        let server = Server::start(vec![
            ("/archive.zip".into(), Reply::cut(vec![7u8; 4096], 10)),
            (
                "/archive.zip.sha256".into(),
                Reply::text(format!("{}  arro.zip\n", "0".repeat(64))),
            ),
        ]);
        let release = release_from("v0.2.0", &server);
        let dir = temp("interrupted");
        assert!(server_source(&server).download(&release, &dir).is_err());
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_good_download_lands_under_its_release_name() {
        let dir = temp("good");
        let zip = dir.join("source.zip");
        write_zip(&zip, &[("a.txt", "a")]);
        let bytes = fs::read(&zip).unwrap();
        let hash = sha256_file(&zip).unwrap();
        let server = Server::start(vec![
            ("/archive.zip".into(), Reply::bytes(bytes)),
            (
                "/archive.zip.sha256".into(),
                Reply::text(format!("{hash}  arro.zip\n")),
            ),
        ]);
        let release = release_from("v0.2.0", &server);
        let download = dir.join("download");
        let file = server_source(&server)
            .download(&release, &download)
            .unwrap();
        assert_eq!(file, download.join(&release.archive.name));
        assert!(file.is_file());
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn stage_replaces_files_and_keeps_the_running_one_as_old() {
        let dir = temp("stage");
        let install = dir.join("install");
        fs::create_dir_all(&install).unwrap();
        fs::write(install.join("ide.exe"), "old exe").unwrap();
        fs::write(install.join("data.txt"), "old data").unwrap();
        let zip = dir.join("update.zip");
        write_zip(
            &zip,
            &[("ide.exe", "new exe"), ("sub/data.txt", "new data")],
        );
        stage_as(&zip, &install, Some("ide.exe")).unwrap();
        assert_eq!(
            fs::read_to_string(install.join("ide.exe")).unwrap(),
            "new exe"
        );
        assert_eq!(
            fs::read_to_string(install.join("sub/data.txt")).unwrap(),
            "new data"
        );
        assert_eq!(
            fs::read_to_string(install.join("ide.exe.old")).unwrap(),
            "old exe"
        );
        assert_eq!(
            fs::read_to_string(install.join("data.txt")).unwrap(),
            "old data"
        );
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn stage_puts_every_file_back_when_a_step_fails() {
        let dir = temp("rollback");
        let install = dir.join("install");
        fs::create_dir_all(&install).unwrap();
        fs::write(install.join("ide.exe"), "old exe").unwrap();
        fs::write(install.join("data.txt"), "old data").unwrap();
        // A file where the archive needs a folder makes the replacement stop halfway.
        fs::write(install.join("sub"), "not a folder").unwrap();
        let zip = dir.join("update.zip");
        write_zip(
            &zip,
            &[
                ("ide.exe", "new exe"),
                ("data.txt", "new data"),
                ("sub/inner.txt", "inner"),
            ],
        );
        assert!(stage_as(&zip, &install, Some("ide.exe")).is_err());
        assert_eq!(
            fs::read_to_string(install.join("ide.exe")).unwrap(),
            "old exe"
        );
        assert_eq!(
            fs::read_to_string(install.join("data.txt")).unwrap(),
            "old data"
        );
        assert!(!install.join("ide.exe.old").exists());
        assert!(!install.join("sub/inner.txt").exists());
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn the_release_folder_inside_the_archive_is_unpacked_into_the_install_folder() {
        let dir = temp("root");
        let install = dir.join("install");
        fs::create_dir_all(&install).unwrap();
        fs::write(install.join("ide.exe"), "old exe").unwrap();
        let zip = dir.join("update.zip");
        write_zip(
            &zip,
            &[
                ("arro-0.2.0-windows-x86_64/ide.exe", "new exe"),
                ("arro-0.2.0-windows-x86_64/tools/server.jar", "jar"),
            ],
        );
        stage_as(&zip, &install, Some("ide.exe")).unwrap();
        assert_eq!(
            fs::read_to_string(install.join("ide.exe")).unwrap(),
            "new exe"
        );
        assert_eq!(
            fs::read_to_string(install.join("tools/server.jar")).unwrap(),
            "jar"
        );
        assert!(!install.join("arro-0.2.0-windows-x86_64").exists());
        fs::remove_dir_all(dir).ok();
    }

    #[cfg(windows)]
    #[test]
    fn an_unchanged_file_in_use_does_not_stop_the_update() {
        use std::os::windows::fs::OpenOptionsExt;

        let dir = temp("in-use");
        let install = dir.join("install");
        fs::create_dir_all(install.join("tools")).unwrap();
        fs::write(install.join("ide.exe"), "old exe").unwrap();
        fs::write(install.join("tools/server.jar"), "same jar").unwrap();
        // A process that keeps the jar open without FILE_SHARE_DELETE, as a JVM does.
        let _held = fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(install.join("tools/server.jar"))
            .unwrap();
        let zip = dir.join("update.zip");
        write_zip(
            &zip,
            &[("ide.exe", "new exe"), ("tools/server.jar", "same jar")],
        );
        stage_as(&zip, &install, Some("ide.exe")).unwrap();
        assert_eq!(
            fs::read_to_string(install.join("ide.exe")).unwrap(),
            "new exe"
        );
        drop(_held);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn old_files_are_removed_on_the_next_start() {
        let dir = temp("cleanup");
        fs::write(dir.join("ide.exe.old"), "old").unwrap();
        fs::write(dir.join("keep.txt"), "keep").unwrap();
        assert_eq!(remove_old(&dir).unwrap(), 1);
        assert!(!dir.join("ide.exe.old").exists());
        assert!(dir.join("keep.txt").exists());
        fs::remove_dir_all(dir).ok();
    }
}
