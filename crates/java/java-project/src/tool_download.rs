//! A build tool downloaded from its official site: the newest stable version, the
//! archive of the platform, the checksum from the neighbouring file, and the unpacked
//! home under the data folder of Arro. Only the site the tool names is asked; no mirror
//! is used.
//!
//! Nothing here touches the window. The calls block on the network and on the disk, so
//! the window runs them in the background; [`base::Cancel`] stops a download between the
//! pieces of a file. An archive is written under a temporary name and its home appears
//! only after the checksum and the unpacking, so a failure leaves nothing the build could
//! run.

use std::cmp::Ordering;
use std::fs::{self, File};
use std::io::{self, BufReader, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use base::Cancel;
use project_model::ToolProgress;
use ring::digest;

const USER_AGENT: &str = "Arro";
/// The folder of the downloaded copies under the data folder of the IDE.
const TOOLS: &str = "tools";
/// The work folder of a download, beside the versions it fills.
const WORK: &str = ".download";
/// The most a metadata or checksum file may be.
const SMALL_LIMIT: usize = 1024 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(60);

/// The kind of archive a platform takes, which is the one the official page offers it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Archive {
    Zip,
    TarGz,
}

impl Archive {
    /// The archive of the platform the IDE runs on.
    pub(crate) fn of_host() -> Self {
        if cfg!(windows) {
            Self::Zip
        } else {
            Self::TarGz
        }
    }

    pub(crate) fn extension(self) -> &'static str {
        match self {
            Self::Zip => "zip",
            Self::TarGz => "tar.gz",
        }
    }
}

/// The digest a checksum file of a download carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Checksum {
    Sha256,
    Sha512,
}

impl Checksum {
    fn algorithm(self) -> &'static digest::Algorithm {
        match self {
            Self::Sha256 => &digest::SHA256,
            Self::Sha512 => &digest::SHA512,
        }
    }

    fn file_suffix(self) -> &'static str {
        match self {
            Self::Sha256 => "sha256",
            Self::Sha512 => "sha512",
        }
    }
}

/// What a request to the server, the checksum or the unpacking can fail with.
#[derive(Debug)]
pub(crate) enum DownloadError {
    Cancelled,
    /// The machine has no way to reach the server, or the server did not answer.
    Offline(String),
    /// The server answered, but not with the file.
    NotFound(String),
    Http(u16),
    Checksum {
        expected: String,
        actual: String,
    },
    /// The archive is not the one the server promises, or a file cannot be written.
    Files(io::Error),
    /// An archive ended in the middle or holds no usable home.
    Archive(String),
    NoStableVersion,
}

impl std::fmt::Display for DownloadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("The download was cancelled."),
            Self::Offline(detail) => {
                write!(formatter, "There is no connection to the server: {detail}")
            }
            Self::NotFound(url) => write!(formatter, "The server has no file at {url}."),
            Self::Http(status) => write!(formatter, "The server answered with HTTP {status}."),
            Self::Checksum { expected, actual } => write!(
                formatter,
                "The checksum of the archive does not match: the server says {expected}, \
                 the file is {actual}."
            ),
            Self::Files(error) => write!(
                formatter,
                "The archive cannot be written: {error}. A full disk leaves no room for the \
                 download."
            ),
            Self::Archive(detail) => write!(formatter, "The archive cannot be unpacked: {detail}"),
            Self::NoStableVersion => {
                formatter.write_str("The server offers no stable version of the tool.")
            }
        }
    }
}

impl std::error::Error for DownloadError {}

/// A version of a tool to install: the archive of the platform, its checksum file, and
/// the folder the archive keeps its home in.
#[derive(Clone, Debug)]
pub(crate) struct Release {
    /// The folder of this tool under `<data>/tools`.
    pub tool: &'static str,
    pub version: String,
    /// The top folder of the archive, `<artifact>-<version>`.
    pub top: String,
    /// The absolute URL of the archive.
    pub url: String,
    /// The absolute URL of the checksum file.
    pub checksum: String,
    pub algorithm: Checksum,
    pub archive: Archive,
    /// The program a home carries, `bin/<program>`.
    pub program: String,
}

impl Release {
    /// Downloads the archive, checks it and unpacks it into
    /// `<data>/tools/<tool>/<version>`. A copy of an earlier download is returned without
    /// a request.
    pub(crate) fn install(
        &self,
        data_dir: &Path,
        progress: &ToolProgress,
        cancel: &Cancel,
    ) -> Result<PathBuf, DownloadError> {
        let version = safe_name(&self.version);
        let home = version_folder(data_dir, self.tool, &version);
        if has_program(&home, &self.program) {
            return Ok(home);
        }
        let work = data_dir.join(TOOLS).join(self.tool).join(WORK);
        fs::create_dir_all(&work).map_err(DownloadError::Files)?;
        let file = self.file_name();
        let part = work.join(format!("{file}.part"));
        if let Err(error) = stream_to(&self.url, &part, progress, cancel) {
            let _ = fs::remove_file(&part);
            return Err(error);
        }
        if let Err(error) = verify(&self.checksum, &part, self.algorithm) {
            // A file that does not match the server is not the tool: it never reaches a
            // name the build would run.
            let _ = fs::remove_file(&part);
            return Err(error);
        }
        let unpacked = work.join(format!("{version}.unpack"));
        let _ = fs::remove_dir_all(&unpacked);
        fs::create_dir_all(&unpacked).map_err(DownloadError::Files)?;
        let extracted = match self.archive {
            Archive::Zip => extract_zip(&part, &unpacked, &self.top, cancel),
            Archive::TarGz => extract_tar_gz(&part, &unpacked, &self.top, cancel),
        };
        if let Err(error) = extracted {
            let _ = fs::remove_dir_all(&unpacked);
            let _ = fs::remove_file(&part);
            return Err(error);
        }
        let Some(found) = find_home(&unpacked, &self.top) else {
            let _ = fs::remove_dir_all(&unpacked);
            let _ = fs::remove_file(&part);
            return Err(DownloadError::Archive(format!(
                "no bin/{} in {file}",
                self.program
            )));
        };
        if home.exists() {
            fs::remove_dir_all(&home).map_err(DownloadError::Files)?;
        }
        fs::rename(&found, &home).map_err(|error| {
            let _ = fs::remove_dir_all(&unpacked);
            let _ = fs::remove_file(&part);
            DownloadError::Files(error)
        })?;
        let _ = fs::remove_dir_all(&unpacked);
        let _ = fs::remove_file(&part);
        Ok(home)
    }

    /// The name of the archive: the top folder plus the kind of the archive.
    fn file_name(&self) -> String {
        format!("{}-bin.{}", self.top, self.archive.extension())
    }
}

/// The URL of the checksum file of `url`.
pub(crate) fn checksum_url(url: &str, algorithm: Checksum) -> String {
    format!("{url}.{}", algorithm.file_suffix())
}

/// The folder of a version under the data folder: `<data>/tools/<tool>/<version>`.
pub(crate) fn version_folder(data_dir: &Path, tool: &str, version: &str) -> PathBuf {
    data_dir.join(TOOLS).join(tool).join(safe_name(version))
}

/// The newest copy of a tool the IDE downloaded under `<data>/tools/<tool>`, when it
/// carries one of `programs` in its `bin` folder.
pub(crate) fn newest_home(data_dir: &Path, tool: &str, programs: &[&str]) -> Option<PathBuf> {
    let root = data_dir.join(TOOLS).join(tool);
    let mut versions: Vec<String> = fs::read_dir(&root)
        .ok()?
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();
    versions.sort_by(|left, right| compare_versions(right, left));
    versions.into_iter().find_map(|version| {
        let home = root.join(&version);
        programs
            .iter()
            .any(|program| home.join("bin").join(program).is_file())
            .then_some(home)
    })
}

/// Whether a home carries the program the build starts.
fn has_program(home: &Path, program: &str) -> bool {
    home.join("bin").join(program).is_file()
}

/// The home an archive brought: the named top folder, the folder itself, or its only
/// folder that has `bin`.
fn find_home(unpacked: &Path, top: &str) -> Option<PathBuf> {
    let named = unpacked.join(top);
    if named.join("bin").is_dir() {
        return Some(named);
    }
    if unpacked.join("bin").is_dir() {
        return Some(unpacked.to_path_buf());
    }
    fs::read_dir(unpacked)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .find(|path| path.join("bin").is_dir())
}

/// A name that stays one folder and cannot leave the data folder.
pub(crate) fn safe_name(name: &str) -> String {
    let name: String = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect();
    if name.is_empty() || name == "." || name == ".." {
        "version".to_owned()
    } else {
        name
    }
}

/// The order of two versions: numbers compare as numbers, so `3.9.10` is newer than
/// `3.9.9`, and a number is newer than a word in the same place.
pub(crate) fn compare_versions(left: &str, right: &str) -> Ordering {
    let left: Vec<&str> = left.split(['.', '-', '_']).collect();
    let right: Vec<&str> = right.split(['.', '-', '_']).collect();
    for (left, right) in left.iter().zip(right.iter()) {
        let order = match (left.parse::<u64>(), right.parse::<u64>()) {
            (Ok(left), Ok(right)) => left.cmp(&right),
            (Ok(_), Err(_)) => Ordering::Greater,
            (Err(_), Ok(_)) => Ordering::Less,
            (Err(_), Err(_)) => left.to_ascii_lowercase().cmp(&right.to_ascii_lowercase()),
        };
        if order != Ordering::Equal {
            return order;
        }
    }
    left.len().cmp(&right.len())
}

/// The path of an archive entry inside the home, without the top folder of the archive.
fn entry_path(path: &Path, top: &str) -> Result<PathBuf, DownloadError> {
    let mut parts = Vec::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::Normal(name) => parts.push(name),
            _ => {
                return Err(DownloadError::Archive(format!(
                    "{} leaves the home folder",
                    path.display()
                )));
            }
        }
    }
    if parts.first().is_some_and(|first| *first == top) {
        parts.remove(0);
    }
    Ok(parts.iter().collect())
}

pub(crate) fn extract_zip(
    archive: &Path,
    into: &Path,
    top: &str,
    cancel: &Cancel,
) -> Result<(), DownloadError> {
    let file = File::open(archive).map_err(DownloadError::Files)?;
    let mut zip =
        zip::ZipArchive::new(file).map_err(|error| DownloadError::Archive(error.to_string()))?;
    for index in 0..zip.len() {
        if cancel.is_cancelled() {
            return Err(DownloadError::Cancelled);
        }
        let mut entry = zip
            .by_index(index)
            .map_err(|error| DownloadError::Archive(error.to_string()))?;
        let Some(path) = entry.enclosed_name() else {
            return Err(DownloadError::Archive(format!(
                "entry {index} has no name inside the archive"
            )));
        };
        let relative = entry_path(&path, top)?;
        if relative.as_os_str().is_empty() {
            continue;
        }
        let target = into.join(&relative);
        if entry.is_dir() {
            fs::create_dir_all(&target).map_err(DownloadError::Files)?;
            let mode = directory_mode(entry.unix_mode().unwrap_or(0o755));
            platform::set_mode(&target, mode).map_err(DownloadError::Files)?;
            continue;
        }
        let mode = file_mode(entry.unix_mode(), is_program(&relative));
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(DownloadError::Files)?;
        }
        let mut out = File::create(&target).map_err(DownloadError::Files)?;
        io::copy(&mut entry, &mut out).map_err(DownloadError::Files)?;
        if let Some(mode) = mode {
            platform::set_mode(&target, mode).map_err(DownloadError::Files)?;
        }
    }
    Ok(())
}

/// Whether a file of a home is the program the build starts: `bin/<name>` without an
/// extension. An archive that names no mode still has to leave it runnable.
fn is_program(path: &Path) -> bool {
    let mut parts = path.components();
    matches!(parts.next(), Some(Component::Normal(name)) if name == "bin")
        && path.extension().is_none()
}

/// The permission bits of an unpacked file. A program has to stay runnable even when the
/// archive records no execute bits for it: `zip` writes `0o644` for an entry whose writer
/// named no mode, so the bits are added to whatever the entry records. An empty mode, as
/// an old tar header may leave it, means the usual `0o755`.
fn file_mode(recorded: Option<u32>, program: bool) -> Option<u32> {
    match recorded {
        None | Some(0) if program => Some(0o755),
        Some(mode) if program => Some(mode | 0o111),
        mode => mode,
    }
}

/// The permission bits of an unpacked directory. The files below it are written after it,
/// so a directory without the owner's write bit cannot take them: an empty header means
/// the usual `0o755`, and a directory whose owner cannot write gets the bits to do so.
fn directory_mode(recorded: u32) -> u32 {
    match recorded {
        0 => 0o755,
        mode if mode & 0o200 == 0 => mode | 0o700,
        mode => mode,
    }
}

pub(crate) fn extract_tar_gz(
    archive: &Path,
    into: &Path,
    top: &str,
    cancel: &Cancel,
) -> Result<(), DownloadError> {
    let file = File::open(archive).map_err(DownloadError::Files)?;
    let mut reader = BufReader::new(flate2::read::GzDecoder::new(file));
    let mut long_name: Option<String> = None;
    loop {
        if cancel.is_cancelled() {
            return Err(DownloadError::Cancelled);
        }
        let mut header = [0u8; 512];
        match reader.read_exact(&mut header) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(error) => return Err(DownloadError::Offline(error.to_string())),
        }
        if header.iter().all(|byte| *byte == 0) {
            break;
        }
        let size = tar_size(&header)?;
        let mode = tar_mode(&header)?;
        let kind = header[156];
        let name = match long_name.take() {
            Some(name) => name,
            None => tar_name(&header),
        };
        if kind == b'L' {
            // A GNU long name: the data of this entry is the name of the next one.
            let mut bytes = vec![0u8; size];
            reader
                .read_exact(&mut bytes)
                .map_err(|error| DownloadError::Archive(error.to_string()))?;
            skip_padding(&mut reader, size)?;
            long_name = Some(
                String::from_utf8_lossy(&bytes)
                    .trim_end_matches('\0')
                    .to_owned(),
            );
            continue;
        }
        if kind == b'x' || kind == b'g' {
            skip_data(&mut reader, size)?;
            continue;
        }
        if kind == b'5' || name.ends_with('/') {
            let relative = entry_path(Path::new(&name), top)?;
            if !relative.as_os_str().is_empty() {
                let target = into.join(relative);
                fs::create_dir_all(&target).map_err(DownloadError::Files)?;
                platform::set_mode(&target, directory_mode(mode)).map_err(DownloadError::Files)?;
            }
            skip_data(&mut reader, size)?;
            continue;
        }
        if kind == 0 || kind == b'0' {
            let relative = entry_path(Path::new(&name), top)?;
            if relative.as_os_str().is_empty() {
                skip_data(&mut reader, size)?;
                continue;
            }
            let program = is_program(&relative);
            let target = into.join(relative);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).map_err(DownloadError::Files)?;
            }
            let mut out = File::create(&target).map_err(DownloadError::Files)?;
            copy_exactly(&mut reader, &mut out, size, cancel)?;
            skip_padding(&mut reader, size)?;
            if let Some(mode) = file_mode(Some(mode), program) {
                platform::set_mode(&target, mode).map_err(DownloadError::Files)?;
            }
            continue;
        }
        // Links and the other kinds have no place in a distribution of a build tool.
        skip_data(&mut reader, size)?;
    }
    Ok(())
}

/// The name of a tar entry: the prefix and the name, which older tools split at 100
/// characters.
fn tar_name(header: &[u8; 512]) -> String {
    let field = |range: std::ops::Range<usize>| {
        let bytes = &header[range];
        let end = bytes
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(bytes.len());
        String::from_utf8_lossy(&bytes[..end]).trim().to_owned()
    };
    let name = field(0..100);
    let prefix = field(345..500);
    if prefix.is_empty() {
        name
    } else {
        format!("{prefix}/{name}")
    }
}

fn tar_size(header: &[u8; 512]) -> Result<usize, DownloadError> {
    let field = &header[124..136];
    let text = String::from_utf8_lossy(field);
    let text = text.trim_matches(|character: char| character == '\0' || character == ' ');
    if text.is_empty() {
        return Ok(0);
    }
    usize::from_str_radix(text, 8)
        .map_err(|error| DownloadError::Archive(format!("the size {text} is not octal: {error}")))
}

/// The permission bits of a tar entry. An old archive may leave the field empty.
fn tar_mode(header: &[u8; 512]) -> Result<u32, DownloadError> {
    let field = &header[100..108];
    let text = String::from_utf8_lossy(field);
    let text = text.trim_matches(|character: char| character == '\0' || character == ' ');
    if text.is_empty() {
        return Ok(0);
    }
    u32::from_str_radix(text, 8)
        .map_err(|error| DownloadError::Archive(format!("the mode {text} is not octal: {error}")))
}

fn copy_exactly(
    reader: &mut impl Read,
    out: &mut impl Write,
    size: usize,
    cancel: &Cancel,
) -> Result<(), DownloadError> {
    let mut left = size;
    let mut buffer = [0u8; 64 * 1024];
    while left > 0 {
        if cancel.is_cancelled() {
            return Err(DownloadError::Cancelled);
        }
        let wanted = left.min(buffer.len());
        let read = reader
            .read(&mut buffer[..wanted])
            .map_err(|error| DownloadError::Archive(error.to_string()))?;
        if read == 0 {
            return Err(DownloadError::Archive(
                "the archive ends in the middle of a file".to_owned(),
            ));
        }
        out.write_all(&buffer[..read])
            .map_err(DownloadError::Files)?;
        left -= read;
    }
    Ok(())
}

fn skip_data(reader: &mut impl Read, size: usize) -> Result<(), DownloadError> {
    let copied = io::copy(&mut reader.by_ref().take(size as u64), &mut io::sink())
        .map_err(|error| DownloadError::Archive(error.to_string()))?;
    if copied != size as u64 {
        return Err(DownloadError::Archive(
            "the archive ends in the middle of an entry".to_owned(),
        ));
    }
    skip_padding(reader, size)
}

fn skip_padding(reader: &mut impl Read, size: usize) -> Result<(), DownloadError> {
    let padding = (512 - size % 512) % 512;
    if padding == 0 {
        return Ok(());
    }
    let mut buffer = [0u8; 512];
    reader
        .read_exact(&mut buffer[..padding])
        .map_err(|error| DownloadError::Archive(error.to_string()))
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(None)
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .timeout_recv_response(Some(RESPONSE_TIMEOUT))
        .timeout_recv_body(Some(RESPONSE_TIMEOUT))
        .http_status_as_error(false)
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .build(),
        )
        .build()
        .into()
}

fn get(url: &str) -> Result<ureq::http::Response<ureq::Body>, DownloadError> {
    let response = agent()
        .get(url)
        .header("User-Agent", USER_AGENT)
        .call()
        .map_err(network)?;
    let status = response.status().as_u16();
    match status {
        200..=299 => Ok(response),
        404 => Err(DownloadError::NotFound(url.to_owned())),
        status => Err(DownloadError::Http(status)),
    }
}

fn network(error: ureq::Error) -> DownloadError {
    match error {
        ureq::Error::Timeout(_) => {
            DownloadError::Offline("the server did not answer in time".to_owned())
        }
        ureq::Error::HostNotFound => DownloadError::Offline("the host is not known".to_owned()),
        ureq::Error::Io(error) if error.kind() == io::ErrorKind::TimedOut => {
            DownloadError::Offline("the server did not answer in time".to_owned())
        }
        other => DownloadError::Offline(other.to_string()),
    }
}

/// Reads a small file of the server: the list of the versions or a checksum.
pub(crate) fn fetch(url: &str) -> Result<Vec<u8>, DownloadError> {
    let response = get(url)?;
    let mut body = response.into_body().into_reader();
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 16 * 1024];
    loop {
        match body.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                bytes.extend_from_slice(&buffer[..read]);
                if bytes.len() > SMALL_LIMIT {
                    return Err(DownloadError::Offline(format!("{url} is not a small file")));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(DownloadError::Offline(error.to_string())),
        }
    }
    Ok(bytes)
}

/// Writes the archive to `path` as it comes, reporting the bytes and stopping on `cancel`.
pub(crate) fn stream_to(
    url: &str,
    path: &Path,
    progress: &ToolProgress,
    cancel: &Cancel,
) -> Result<(), DownloadError> {
    if cancel.is_cancelled() {
        return Err(DownloadError::Cancelled);
    }
    let response = get(url)?;
    let total = response
        .headers()
        .get("content-length")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok());
    let mut body = response.into_body().into_reader();
    let mut file = File::create(path).map_err(DownloadError::Files)?;
    let mut written = 0u64;
    progress.set(0, total);
    let mut buffer = [0u8; 64 * 1024];
    loop {
        if cancel.is_cancelled() {
            return Err(DownloadError::Cancelled);
        }
        match body.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                file.write_all(&buffer[..read])
                    .map_err(DownloadError::Files)?;
                written += read as u64;
                progress.set(written, total);
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(DownloadError::Offline(error.to_string())),
        }
    }
    if let Some(total) = total
        && written < total
    {
        return Err(DownloadError::Offline(
            "the server closed the connection before the file ended".to_owned(),
        ));
    }
    file.flush().map_err(DownloadError::Files)
}

/// Checks the archive against the checksum file of the server.
fn verify(url: &str, path: &Path, algorithm: Checksum) -> Result<(), DownloadError> {
    let bytes = fetch(url)?;
    let expected = parse_checksum(&bytes)?;
    verify_expected(&expected, path, algorithm)
}

/// Checks `path` against a checksum the caller already has, such as the one a wrapper
/// properties file names.
pub(crate) fn verify_expected(
    expected: &str,
    path: &Path,
    algorithm: Checksum,
) -> Result<(), DownloadError> {
    let expected = expected.trim().to_ascii_lowercase();
    let actual = digest_file(algorithm.algorithm(), path).map_err(DownloadError::Files)?;
    if !expected.eq_ignore_ascii_case(&actual) {
        return Err(DownloadError::Checksum { expected, actual });
    }
    Ok(())
}

/// The checksum a checksum file carries: the first token of its first line.
fn parse_checksum(bytes: &[u8]) -> Result<String, DownloadError> {
    let text = String::from_utf8_lossy(bytes);
    let checksum = text
        .split_whitespace()
        .next()
        .map(str::trim)
        .filter(|checksum| !checksum.is_empty())
        .ok_or_else(|| DownloadError::Offline("the checksum file is empty".to_owned()))?;
    if !checksum
        .chars()
        .all(|character| character.is_ascii_hexdigit())
    {
        return Err(DownloadError::Offline(
            "the checksum file has no checksum".to_owned(),
        ));
    }
    Ok(checksum.to_ascii_lowercase())
}

fn digest_file(algorithm: &'static digest::Algorithm, path: &Path) -> io::Result<String> {
    let mut context = digest::Context::new(algorithm);
    let mut file = File::open(path)?;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => context.update(&buffer[..read]),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(hex(context.finish().as_ref()))
}

fn hex(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push_str(&format!("{byte:02x}"));
    }
    text
}

#[cfg(test)]
pub(crate) mod testing {
    //! The parts every download test needs: a server of the answers a test asks for, a
    //! zip or tar package of a few files, and their checksums.

    use std::io;
    use std::io::{Cursor, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::thread;
    use std::time::Duration;

    use ring::digest;

    /// A stand-in of the server: it answers the paths a test asks for and nothing else,
    /// so the suite never reaches the network.
    pub(crate) struct Server {
        pub(crate) base: String,
        requests: Arc<AtomicUsize>,
        stop: Arc<AtomicBool>,
        handle: Option<thread::JoinHandle<()>>,
    }

    impl Server {
        pub(crate) fn start(answers: Vec<(String, Vec<u8>)>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("a local port");
            let address = listener.local_addr().expect("the address of the listener");
            listener
                .set_nonblocking(true)
                .expect("a non-blocking listener");
            let requests = Arc::new(AtomicUsize::new(0));
            let stop = Arc::new(AtomicBool::new(false));
            let counter = requests.clone();
            let stopping = stop.clone();
            let handle = thread::spawn(move || {
                while !stopping.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((mut socket, _)) => {
                            // On Windows an accepted socket inherits the non-blocking mode
                            // of the listener, and a read before the request arrives would
                            // split the answer from it.
                            socket.set_nonblocking(false).ok();
                            socket.set_read_timeout(Some(Duration::from_secs(10))).ok();
                            socket.set_write_timeout(Some(Duration::from_secs(10))).ok();
                            let path = request_path(&mut socket);
                            counter.fetch_add(1, Ordering::Relaxed);
                            let answer = answers
                                .iter()
                                .find(|(known, _)| known == &path)
                                .map(|(_, answer)| answer);
                            let written = match answer {
                                Some(bytes) => {
                                    let head = format!(
                                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                        bytes.len()
                                    );
                                    socket
                                        .write_all(head.as_bytes())
                                        .and_then(|_| socket.write_all(bytes))
                                }
                                None => socket.write_all(
                                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                                ),
                            };
                            let _ = written;
                            let _ = socket.flush();
                        }
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(_) => break,
                    }
                }
            });
            Self {
                base: format!("http://{address}"),
                requests,
                stop,
                handle: Some(handle),
            }
        }

        pub(crate) fn requests(&self) -> usize {
            self.requests.load(Ordering::Relaxed)
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

    fn request_path(socket: &mut TcpStream) -> String {
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            match socket.read(&mut byte) {
                Ok(0) => break,
                Ok(_) => head.push(byte[0]),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        let text = String::from_utf8_lossy(&head);
        text.split_whitespace()
            .nth(1)
            .unwrap_or_default()
            .to_owned()
    }

    /// A zip of the files, each under the top folder of the archive.
    pub(crate) fn zip_archive(top: &str, files: &[(&str, &[u8])]) -> Vec<u8> {
        zip_archive_with(top, files, None)
    }

    /// A zip whose entries carry `mode` as their Unix permission bits.
    #[cfg(unix)]
    pub(crate) fn zip_archive_with_mode(top: &str, files: &[(&str, &[u8])], mode: u32) -> Vec<u8> {
        zip_archive_with(top, files, Some(mode))
    }

    fn zip_archive_with(top: &str, files: &[(&str, &[u8])], mode: Option<u32>) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut writer = zip::ZipWriter::new(Cursor::new(&mut bytes));
            let mut options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            if let Some(mode) = mode {
                options = options.unix_permissions(mode);
            }
            writer
                .add_directory(format!("{top}/"), options)
                .expect("a folder");
            for (name, text) in files {
                writer
                    .start_file(format!("{top}/{name}"), options)
                    .expect("a file");
                writer.write_all(text).unwrap();
            }
            writer.finish().expect("the end of the archive");
        }
        bytes
    }

    /// A tar.gz of the files, each under the top folder of the archive.
    pub(crate) fn tar_gz_archive(top: &str, files: &[(&str, &[u8])]) -> Vec<u8> {
        tar_gz_archive_with_mode(top, files, 0o755)
    }

    /// A tar.gz whose file entries carry `mode` as their permission bits.
    pub(crate) fn tar_gz_archive_with_mode(
        top: &str,
        files: &[(&str, &[u8])],
        mode: u32,
    ) -> Vec<u8> {
        let mut tar = Vec::new();
        tar_entry(&mut tar, &format!("{top}/"), b'5', b"", 0o755);
        for (name, text) in files {
            tar_entry(&mut tar, &format!("{top}/{name}"), b'0', text, mode);
        }
        tar_entry(&mut tar, &format!("{top}/bin/"), b'5', b"", 0o755);
        tar.extend_from_slice(&[0u8; 1024]);
        let mut bytes = Vec::new();
        {
            let mut encoder =
                flate2::write::GzEncoder::new(&mut bytes, flate2::Compression::default());
            encoder.write_all(&tar).unwrap();
            encoder.finish().unwrap();
        }
        bytes
    }

    /// A tar.gz whose `lib` folder entry carries `mode`, with the files below that folder.
    #[cfg(unix)]
    pub(crate) fn tar_gz_archive_with_folder_mode(
        top: &str,
        mode: u32,
        files: &[(&str, &[u8])],
    ) -> Vec<u8> {
        let mut tar = Vec::new();
        tar_entry(&mut tar, &format!("{top}/"), b'5', b"", 0o755);
        tar_entry(&mut tar, &format!("{top}/lib/"), b'5', b"", mode);
        for (name, text) in files {
            tar_entry(&mut tar, &format!("{top}/{name}"), b'0', text, 0o644);
        }
        tar.extend_from_slice(&[0u8; 1024]);
        let mut bytes = Vec::new();
        {
            let mut encoder =
                flate2::write::GzEncoder::new(&mut bytes, flate2::Compression::default());
            encoder.write_all(&tar).unwrap();
            encoder.finish().unwrap();
        }
        bytes
    }

    fn tar_entry(out: &mut Vec<u8>, name: &str, kind: u8, data: &[u8], mode: u32) {
        let size = data.len();
        let mut header = [0u8; 512];
        header[..name.len()].copy_from_slice(name.as_bytes());
        let mode_field = format!("{mode:07o}\0");
        header[100..108].copy_from_slice(mode_field.as_bytes());
        header[108..116].copy_from_slice(b"0000000\0");
        header[116..124].copy_from_slice(b"0000000\0");
        let size_field = format!("{size:011o}\0");
        header[124..136].copy_from_slice(size_field.as_bytes());
        header[136..148].copy_from_slice(b"00000000000\0");
        header[148..156].copy_from_slice(b"        ");
        header[156] = kind;
        header[257..263].copy_from_slice(b"ustar\0");
        header[263..265].copy_from_slice(b"00");
        let sum: u32 = header.iter().map(|byte| u32::from(*byte)).sum();
        let sum_field = format!("{sum:06o}\0 ");
        header[148..156].copy_from_slice(sum_field.as_bytes());
        out.extend_from_slice(&header);
        out.extend_from_slice(data);
        let padding = (512 - size % 512) % 512;
        out.extend(std::iter::repeat_n(0u8, padding));
    }

    pub(crate) fn sha256(bytes: &[u8]) -> String {
        super::hex(digest::digest(&digest::SHA256, bytes).as_ref())
    }

    pub(crate) fn sha512(bytes: &[u8]) -> String {
        super::hex(digest::digest(&digest::SHA512, bytes).as_ref())
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::testing::zip_archive;
    use super::{Archive, Checksum, compare_versions, newest_home, safe_name, version_folder};

    #[test]
    fn a_checklist_of_versions_compares_as_numbers() {
        assert_eq!(
            compare_versions("3.9.10", "3.9.9"),
            std::cmp::Ordering::Greater
        );
        assert_eq!(compare_versions("9.1.0", "9.5.0"), std::cmp::Ordering::Less);
        assert_eq!(
            compare_versions("9.1.0", "9.1.0"),
            std::cmp::Ordering::Equal
        );
    }

    #[test]
    fn a_version_stays_one_folder_of_the_data_folder() {
        let folder = version_folder(Path::new("data"), "maven", "3.9.11");

        assert_eq!(folder, Path::new("data/tools/maven/3.9.11"));
        let escaped = version_folder(Path::new("data"), "maven", "../..");
        assert!(
            !escaped
                .components()
                .any(|part| matches!(part, std::path::Component::ParentDir)),
            "{}",
            escaped.display()
        );
        assert_eq!(safe_name(""), "version");
    }

    #[test]
    fn the_newest_downloaded_copy_with_a_program_is_taken() {
        let data = test_support::TempDir::new("tool-newest");
        let older = data.path().join("tools/tool/9.1.0/bin");
        let newer = data.path().join("tools/tool/9.5.0/bin");
        std::fs::create_dir_all(&older).unwrap();
        std::fs::create_dir_all(&newer).unwrap();
        std::fs::write(older.join("tool"), b"").unwrap();

        assert_eq!(
            newest_home(data.path(), "tool", &["tool"]),
            Some(data.path().join("tools/tool/9.1.0")),
            "a copy without its program is no home, so the older one is taken"
        );
        std::fs::remove_file(older.join("tool")).unwrap();
        assert!(newest_home(data.path(), "tool", &["tool"]).is_none());
        std::fs::write(newer.join("tool"), b"").unwrap();
        assert_eq!(
            newest_home(data.path(), "tool", &["tool"]),
            Some(data.path().join("tools/tool/9.5.0"))
        );
    }

    #[test]
    fn the_checksum_file_of_a_release_is_named_after_its_digest() {
        let url = "https://example.org/gradle-9.1.0-bin.zip";

        assert_eq!(
            super::checksum_url(url, Checksum::Sha256),
            "https://example.org/gradle-9.1.0-bin.zip.sha256"
        );
        assert_eq!(
            super::checksum_url(url, Checksum::Sha512),
            "https://example.org/gradle-9.1.0-bin.zip.sha512"
        );
        assert_eq!(Archive::Zip.extension(), "zip");
    }

    #[test]
    fn a_zip_of_the_test_support_holds_the_files_of_its_top_folder() {
        let bytes = zip_archive("tool-1.0", &[("bin/tool", b"program")]);

        assert_eq!(super::testing::sha256(&bytes).len(), 64);
        assert_eq!(super::testing::sha512(&bytes).len(), 128);
    }

    #[test]
    fn a_program_gets_execute_bits_from_the_mode_of_its_entry() {
        assert_eq!(super::file_mode(Some(0o644), true), Some(0o755));
        assert_eq!(super::file_mode(Some(0o600), true), Some(0o711));
        assert_eq!(super::file_mode(Some(0o755), true), Some(0o755));
        assert_eq!(super::file_mode(Some(0), true), Some(0o755));
        assert_eq!(super::file_mode(None, true), Some(0o755));
        assert_eq!(super::file_mode(Some(0o644), false), Some(0o644));
        assert_eq!(super::file_mode(Some(0), false), Some(0));
        assert_eq!(super::file_mode(None, false), None);
    }

    #[test]
    fn a_folder_gets_the_bits_its_files_need() {
        assert_eq!(
            super::directory_mode(0),
            0o755,
            "an empty header means the usual folder"
        );
        assert_eq!(
            super::directory_mode(0o555),
            0o755,
            "a folder whose owner cannot write takes the files below it"
        );
        assert_eq!(super::directory_mode(0o500), 0o700);
        assert_eq!(super::directory_mode(0o755), 0o755);
        assert_eq!(super::directory_mode(0o775), 0o775);
    }

    /// A tar header may leave the mode of a folder empty, and a distribution may pack its
    /// folders without the write bit of the owner. The files below them still unpack, so
    /// the folder of a home is readable after the unpacking.
    #[cfg(unix)]
    #[test]
    fn a_folder_without_its_own_permissions_still_takes_the_files_inside_it() {
        let work = test_support::TempDir::new("tool-tar-folder-mode");
        let cancel = base::Cancel::new();

        for mode in [0, 0o555] {
            let tar = work.path().join(format!("tool-{mode:o}-bin.tar.gz"));
            std::fs::write(
                &tar,
                super::testing::tar_gz_archive_with_folder_mode(
                    "tool-1.0",
                    mode,
                    &[("lib/tool.txt", b"data")],
                ),
            )
            .unwrap();
            let into = work.path().join(format!("tar-{mode:o}"));
            std::fs::create_dir_all(&into).unwrap();
            super::extract_tar_gz(&tar, &into, "tool-1.0", &cancel).unwrap();

            let lib = into.join("lib");
            assert_eq!(
                std::fs::read_to_string(lib.join("tool.txt")).unwrap(),
                "data",
                "the file inside a folder of mode {mode:o} is unpacked"
            );
            assert_eq!(
                platform::mode(&lib).unwrap() & 0o700,
                0o700,
                "the owner writes into a folder of mode {mode:o}"
            );
        }
    }

    /// The build starts the program of a home directly, so a copy unpacked on a Unix
    /// system has to keep the execute bits the archive gives it.
    #[cfg(unix)]
    #[test]
    fn an_unpacked_program_stays_executable() {
        let work = test_support::TempDir::new("tool-unix-mode");
        let cancel = base::Cancel::new();

        let zip = work.path().join("gradle-9.1.0-bin.zip");
        std::fs::write(
            &zip,
            super::testing::zip_archive_with_mode(
                "gradle-9.1.0",
                &[("bin/gradle", b"#!/bin/sh\n")],
                0o755,
            ),
        )
        .unwrap();
        let into = work.path().join("zip");
        std::fs::create_dir_all(&into).unwrap();
        super::extract_zip(&zip, &into, "gradle-9.1.0", &cancel).unwrap();
        assert!(
            executable(&into.join("bin/gradle")),
            "the mode of the zip entry is kept"
        );

        // A zip entry without Unix bits still leaves `bin/<name>` runnable. `zip` writes
        // `0o644` for such an entry, so a file outside `bin` keeps that mode as it is.
        let zip = work.path().join("plain-bin.zip");
        std::fs::write(
            &zip,
            super::testing::zip_archive(
                "tool-1.0",
                &[("bin/tool", b"#!/bin/sh\n"), ("lib/tool", b"data")],
            ),
        )
        .unwrap();
        let into = work.path().join("plain");
        std::fs::create_dir_all(&into).unwrap();
        super::extract_zip(&zip, &into, "tool-1.0", &cancel).unwrap();
        assert!(
            executable(&into.join("bin/tool")),
            "a program without a mode gets one"
        );
        assert!(
            !executable(&into.join("lib/tool")),
            "a file outside bin gets no execute bits"
        );

        let tar = work.path().join("apache-maven-3.9.11-bin.tar.gz");
        std::fs::write(
            &tar,
            super::testing::tar_gz_archive("apache-maven-3.9.11", &[("bin/mvn", b"#!/bin/sh\n")]),
        )
        .unwrap();
        let into = work.path().join("tar");
        std::fs::create_dir_all(&into).unwrap();
        super::extract_tar_gz(&tar, &into, "apache-maven-3.9.11", &cancel).unwrap();
        assert!(
            executable(&into.join("bin/mvn")),
            "the mode of the tar header is kept"
        );

        // An archive that records the read-only bits of its program, as a tool that packs
        // a distribution without permissions does, still leaves it runnable.
        let tar = work.path().join("plain-bin.tar.gz");
        std::fs::write(
            &tar,
            super::testing::tar_gz_archive_with_mode(
                "tool-1.0",
                &[("bin/tool", b"#!/bin/sh\n")],
                0o644,
            ),
        )
        .unwrap();
        let into = work.path().join("tar-plain");
        std::fs::create_dir_all(&into).unwrap();
        super::extract_tar_gz(&tar, &into, "tool-1.0", &cancel).unwrap();
        assert!(
            executable(&into.join("bin/tool")),
            "a program with read-only bits gets the execute ones"
        );
    }

    #[cfg(unix)]
    fn executable(path: &Path) -> bool {
        platform::mode(path).unwrap() & 0o111 == 0o111
    }
}
