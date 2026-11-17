//! The bundled copy of Maven: the newest stable version of the official archive, its
//! SHA-512 from the neighbouring file, and the unpacked home under the data folder of
//! Arro. Only the central repository of the Java ecosystem is asked; no mirror is used.
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

/// The official repository of Apache Maven. Mirrors are not asked: a copy from anywhere
/// else could carry anything.
pub(crate) const REPOSITORY: &str = "https://repo.maven.apache.org/maven2";
const GROUP_PATH: &str = "org/apache/maven";
const ARTIFACT: &str = "apache-maven";
const USER_AGENT: &str = "Arro";
/// The folder of the downloaded copies under the data folder of the IDE.
const TOOLS: &str = "tools";
const MAVEN: &str = "maven";
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

    fn file_name(self, version: &str) -> String {
        let extension = match self {
            Self::Zip => "zip",
            Self::TarGz => "tar.gz",
        };
        format!("{ARTIFACT}-{version}-bin.{extension}")
    }
}

/// What a request to the repository, the checksum or the unpacking can fail with.
#[derive(Debug)]
pub(crate) enum DownloadError {
    Cancelled,
    /// The machine has no way to reach the repository, or the repository did not answer.
    Offline(String),
    /// The repository answered, but not with the file.
    NotFound(String),
    Http(u16),
    Checksum {
        expected: String,
        actual: String,
    },
    /// The archive is not the one the repository promises, or a file cannot be written.
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
                write!(
                    formatter,
                    "There is no connection to the repository: {detail}"
                )
            }
            Self::NotFound(url) => write!(formatter, "The repository has no file at {url}."),
            Self::Http(status) => {
                write!(formatter, "The repository answered with HTTP {status}.")
            }
            Self::Checksum { expected, actual } => write!(
                formatter,
                "The checksum of the archive does not match: the repository says {expected}, \
                 the file is {actual}."
            ),
            Self::Files(error) => write!(
                formatter,
                "The archive of Maven cannot be written: {error}. A full disk leaves no room \
                 for the download."
            ),
            Self::Archive(detail) => {
                write!(
                    formatter,
                    "The archive of Maven cannot be unpacked: {detail}"
                )
            }
            Self::NoStableVersion => {
                formatter.write_str("The repository offers no stable version of Maven 3.")
            }
        }
    }
}

impl std::error::Error for DownloadError {}

/// The newest stable version of Maven 3 the repository lists, without alpha, beta,
/// release candidates and snapshots.
pub(crate) fn latest_stable(repository: &str) -> Result<String, DownloadError> {
    let url = format!("{repository}/{GROUP_PATH}/{ARTIFACT}/maven-metadata.xml");
    let metadata = fetch(&url)?;
    let text = String::from_utf8(metadata)
        .map_err(|error| DownloadError::Offline(format!("the metadata is not text: {error}")))?;
    stable_versions(&text)
        .into_iter()
        .next()
        .ok_or(DownloadError::NoStableVersion)
}

/// Downloads the named version, or the newest stable one, and unpacks it into
/// `<data>/tools/maven/<version>`. A copy of an earlier download is returned without a
/// request.
pub(crate) fn download(
    repository: &str,
    data_dir: &Path,
    version: Option<&str>,
    progress: &ToolProgress,
    cancel: &Cancel,
) -> Result<PathBuf, DownloadError> {
    let version = match version.map(str::trim).filter(|version| !version.is_empty()) {
        Some(version) => version.to_owned(),
        None => match crate::maven::bundled_home(data_dir) {
            Some(home) => return Ok(home),
            None => latest_stable(repository)?,
        },
    };
    install_from(
        repository,
        data_dir,
        &version,
        Archive::of_host(),
        progress,
        cancel,
    )
}

/// The versions of Maven 3 that are not pre-releases, the newest first.
fn stable_versions(metadata: &str) -> Vec<String> {
    let Ok(document) = roxmltree::Document::parse(metadata) else {
        return Vec::new();
    };
    let mut versions: Vec<String> = document
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == "version")
        .filter_map(|node| node.text().map(str::trim))
        .filter(|version| is_stable_three(version))
        .map(str::to_owned)
        .collect();
    versions.sort_by(|left, right| compare_versions(right, left));
    versions.dedup();
    versions
}

/// Whether a version is a release of Maven 3: not a pre-release and not Maven 4.
fn is_stable_three(version: &str) -> bool {
    if version.split('.').next() != Some("3") {
        return false;
    }
    !version.split(['.', '-', '_']).any(|part| {
        let part = part.to_ascii_lowercase();
        part.starts_with("alpha")
            || part.starts_with("beta")
            || part.starts_with("rc")
            || part.starts_with("snapshot")
            || part.starts_with("eap")
            || part.starts_with("preview")
            || (part.starts_with('m')
                && part[1..]
                    .chars()
                    .all(|character| character.is_ascii_digit()))
    })
}

/// The order of two versions: numbers compare as numbers, so `3.9.10` is newer than
/// `3.9.9`, and a number is newer than a word in the same place.
fn compare_versions(left: &str, right: &str) -> Ordering {
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

/// The folder of a version under the data folder: `<data>/tools/maven/<version>`.
pub(crate) fn version_folder(data_dir: &Path, version: &str) -> PathBuf {
    data_dir.join(TOOLS).join(MAVEN).join(safe_name(version))
}

fn install_from(
    repository: &str,
    data_dir: &Path,
    version: &str,
    archive: Archive,
    progress: &ToolProgress,
    cancel: &Cancel,
) -> Result<PathBuf, DownloadError> {
    let version = safe_name(version);
    let home = version_folder(data_dir, &version);
    if has_program(&home) {
        return Ok(home);
    }
    let root = data_dir.join(TOOLS).join(MAVEN);
    let work = root.join(WORK);
    fs::create_dir_all(&work).map_err(DownloadError::Files)?;
    let file = archive.file_name(&version);
    let part = work.join(format!("{file}.part"));
    let url = format!("{repository}/{GROUP_PATH}/{ARTIFACT}/{version}/{file}");
    if let Err(error) = stream_to(&url, &part, progress, cancel) {
        let _ = fs::remove_file(&part);
        return Err(error);
    }
    if let Err(error) = verify(&url, &part) {
        // A file that does not match the repository is not Maven: it never reaches a
        // name the build would run.
        let _ = fs::remove_file(&part);
        return Err(error);
    }
    let unpacked = work.join(format!("{version}.unpack"));
    let _ = fs::remove_dir_all(&unpacked);
    fs::create_dir_all(&unpacked).map_err(DownloadError::Files)?;
    let top = format!("{ARTIFACT}-{version}");
    let extracted = match archive {
        Archive::Zip => extract_zip(&part, &unpacked, &top, cancel),
        Archive::TarGz => extract_tar_gz(&part, &unpacked, &top, cancel),
    };
    if let Err(error) = extracted {
        let _ = fs::remove_dir_all(&unpacked);
        let _ = fs::remove_file(&part);
        return Err(error);
    }
    let found = find_home(&unpacked, &version);
    let Some(found) = found else {
        let _ = fs::remove_dir_all(&unpacked);
        let _ = fs::remove_file(&part);
        return Err(DownloadError::Archive(format!(
            "no bin/{} in {file}",
            base::program::cmd_script("mvn")
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

/// Whether a home carries the program the build starts.
fn has_program(home: &Path) -> bool {
    home.join("bin")
        .join(base::program::cmd_script("mvn"))
        .is_file()
}

/// The home an archive brought: the named top folder, the folder itself, or its only
/// folder that has `bin`.
fn find_home(unpacked: &Path, version: &str) -> Option<PathBuf> {
    let named = unpacked.join(format!("{ARTIFACT}-{version}"));
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
fn safe_name(name: &str) -> String {
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

fn extract_zip(
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
            continue;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(DownloadError::Files)?;
        }
        let mut out = File::create(&target).map_err(DownloadError::Files)?;
        io::copy(&mut entry, &mut out).map_err(DownloadError::Files)?;
    }
    Ok(())
}

fn extract_tar_gz(
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
                fs::create_dir_all(into.join(relative)).map_err(DownloadError::Files)?;
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
            let target = into.join(relative);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).map_err(DownloadError::Files)?;
            }
            let mut out = File::create(&target).map_err(DownloadError::Files)?;
            copy_exactly(&mut reader, &mut out, size, cancel)?;
            skip_padding(&mut reader, size)?;
            continue;
        }
        // Links and the other kinds have no place in a distribution of Maven.
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
            DownloadError::Offline("the repository did not answer in time".to_owned())
        }
        ureq::Error::HostNotFound => DownloadError::Offline("the host is not known".to_owned()),
        ureq::Error::Io(error) if error.kind() == io::ErrorKind::TimedOut => {
            DownloadError::Offline("the repository did not answer in time".to_owned())
        }
        other => DownloadError::Offline(other.to_string()),
    }
}

/// Reads a small file of the repository: the metadata of the versions or a checksum.
fn fetch(url: &str) -> Result<Vec<u8>, DownloadError> {
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
fn stream_to(
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
            "the repository closed the connection before the file ended".to_owned(),
        ));
    }
    file.flush().map_err(DownloadError::Files)
}

/// Checks the archive against the `.sha512` of the repository.
fn verify(url: &str, path: &Path) -> Result<(), DownloadError> {
    let bytes = fetch(&format!("{url}.sha512"))?;
    let expected = parse_checksum(&bytes)?;
    let actual = digest_file(&digest::SHA512, path).map_err(DownloadError::Files)?;
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
mod tests {
    use std::fs;
    use std::io;
    use std::io::{Cursor, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering as AtomicOrdering};
    use std::thread;
    use std::time::Duration;

    use base::Cancel;
    use project_model::ToolProgress;

    use super::{
        Archive, DownloadError, compare_versions, install_from, is_stable_three, latest_stable,
        parse_checksum, stable_versions, version_folder,
    };

    fn metadata(versions: &[&str]) -> String {
        let mut text = String::from("<metadata><versioning><versions>");
        for version in versions {
            text.push_str(&format!("<version>{version}</version>"));
        }
        text.push_str("</versions></versioning></metadata>");
        text
    }

    fn sha512(bytes: &[u8]) -> String {
        let digest = ring::digest::digest(&ring::digest::SHA512, bytes);
        super::hex(digest.as_ref())
    }

    fn program() -> &'static str {
        if cfg!(windows) { "mvn.cmd" } else { "mvn" }
    }

    /// A stand-in of the repository: it answers the paths a test asks for and nothing
    /// else, so the suite never reaches the network.
    struct Server {
        base: String,
        requests: Arc<AtomicUsize>,
        stop: Arc<AtomicBool>,
        handle: Option<thread::JoinHandle<()>>,
    }

    impl Server {
        fn start(answers: Vec<(String, Vec<u8>)>) -> Self {
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
                while !stopping.load(AtomicOrdering::Relaxed) {
                    match listener.accept() {
                        Ok((mut socket, _)) => {
                            // On Windows an accepted socket inherits the non-blocking mode
                            // of the listener, and a read before the request arrives would
                            // split the answer from it.
                            socket.set_nonblocking(false).ok();
                            socket.set_read_timeout(Some(Duration::from_secs(10))).ok();
                            socket.set_write_timeout(Some(Duration::from_secs(10))).ok();
                            let path = request_path(&mut socket);
                            counter.fetch_add(1, AtomicOrdering::Relaxed);
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

        fn requests(&self) -> usize {
            self.requests.load(AtomicOrdering::Relaxed)
        }
    }

    impl Drop for Server {
        fn drop(&mut self) {
            self.stop.store(true, AtomicOrdering::Relaxed);
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

    fn archive_path(version: &str, archive: Archive) -> String {
        format!(
            "/org/apache/maven/apache-maven/{version}/apache-maven-{version}-bin.{}",
            match archive {
                Archive::Zip => "zip",
                Archive::TarGz => "tar.gz",
            }
        )
    }

    fn zip_archive(top: &str) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut writer = zip::ZipWriter::new(Cursor::new(&mut bytes));
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            writer
                .add_directory(format!("{top}/"), options)
                .expect("a folder");
            writer
                .start_file(format!("{top}/bin/{}", program()), options)
                .expect("a program");
            writer.write_all(b"#!/bin/sh\necho maven\n").unwrap();
            writer
                .start_file(format!("{top}/lib/A.txt"), options)
                .expect("a library");
            writer.write_all(b"library").unwrap();
            writer.finish().expect("the end of the archive");
        }
        bytes
    }

    fn tar_gz_archive(top: &str) -> Vec<u8> {
        let mut tar = Vec::new();
        tar_entry(&mut tar, &format!("{top}/"), b'5', b"");
        tar_entry(
            &mut tar,
            &format!("{top}/bin/{}", program()),
            b'0',
            b"#!/bin/sh\necho maven\n",
        );
        tar_entry(&mut tar, &format!("{top}/bin/"), b'5', b"");
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

    fn tar_entry(out: &mut Vec<u8>, name: &str, kind: u8, data: &[u8]) {
        let size = data.len();
        let mut header = [0u8; 512];
        header[..name.len()].copy_from_slice(name.as_bytes());
        header[100..108].copy_from_slice(b"0000755\0");
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

    fn download_version(
        server: &Server,
        data: &Path,
        version: &str,
        archive: Archive,
    ) -> Result<PathBuf, DownloadError> {
        install_from(
            &server.base,
            data,
            version,
            archive,
            &ToolProgress::new(),
            &Cancel::new(),
        )
    }

    #[test]
    fn the_newest_stable_maven_three_is_chosen() {
        let text = metadata(&[
            "3.9.9",
            "3.9.10",
            "3.9.11",
            "3.9.12-rc1",
            "3.9.10-beta-1",
            "3.8.8",
            "3.10.0-alpha",
            "4.0.0-alpha-1",
        ]);

        let versions = stable_versions(&text);

        assert_eq!(versions.first().map(String::as_str), Some("3.9.11"));
        assert_eq!(versions.last().map(String::as_str), Some("3.8.8"));
        assert!(versions.iter().all(|version| is_stable_three(version)));
        assert_eq!(
            compare_versions("3.9.10", "3.9.9"),
            std::cmp::Ordering::Greater
        );
    }

    #[test]
    fn a_pre_release_is_not_offered() {
        for version in [
            "3.9.0-alpha",
            "3.9.0-beta2",
            "3.9.0-rc1",
            "3.9.0-SNAPSHOT",
            "4.0.0",
        ] {
            assert!(!is_stable_three(version), "{version}");
        }
        for version in ["3.9.11", "3.8.8", "3.9.10"] {
            assert!(is_stable_three(version), "{version}");
        }
    }

    #[test]
    fn a_checksum_file_gives_its_first_token() {
        let checksum =
            parse_checksum(b"9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08\n")
                .expect("the line carries a checksum");
        assert_eq!(
            checksum,
            "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"
        );
        assert!(parse_checksum(b"not a checksum\n").is_err());
        assert!(parse_checksum(b"").is_err());
    }

    #[test]
    fn the_home_lives_in_the_data_folder_of_the_ide() {
        let folder = version_folder(Path::new("data"), "3.9.11");

        assert_eq!(folder, Path::new("data/tools/maven/3.9.11"));
        let escaped = version_folder(Path::new("data"), "../..");
        assert!(
            !escaped
                .components()
                .any(|part| matches!(part, std::path::Component::ParentDir)),
            "{}",
            escaped.display()
        );
    }

    #[test]
    fn metadata_of_the_server_gives_the_version() {
        let server = Server::start(vec![(
            "/org/apache/maven/apache-maven/maven-metadata.xml".to_owned(),
            metadata(&["3.9.9", "3.9.10"]).into_bytes(),
        )]);

        let version = latest_stable(&server.base).expect("the metadata is read");

        assert_eq!(version, "3.9.10");
    }

    #[test]
    fn a_zip_archive_becomes_the_bundled_home() {
        let data = test_support::TempDir::new("maven-zip");
        let bytes = zip_archive("apache-maven-3.9.11");
        let path = archive_path("3.9.11", Archive::Zip);
        let server = Server::start(vec![
            (path.clone(), bytes.clone()),
            (
                format!("{path}.sha512"),
                format!("{}  apache-maven-3.9.11-bin.zip\n", sha512(&bytes)).into_bytes(),
            ),
        ]);

        let home = download_version(&server, data.path(), "3.9.11", Archive::Zip)
            .expect("the archive is unpacked");

        assert_eq!(home, data.path().join("tools/maven/3.9.11"));
        assert_eq!(
            fs::read_to_string(home.join("bin").join(program())).unwrap(),
            "#!/bin/sh\necho maven\n"
        );
        assert_eq!(
            fs::read_to_string(home.join("lib/A.txt")).unwrap(),
            "library"
        );
        assert!(
            !data
                .path()
                .join("tools/maven/.download/apache-maven-3.9.11-bin.zip.part")
                .exists()
        );
    }

    #[test]
    fn a_tar_gz_archive_becomes_the_bundled_home() {
        let data = test_support::TempDir::new("maven-tar");
        let bytes = tar_gz_archive("apache-maven-3.9.11");
        let path = archive_path("3.9.11", Archive::TarGz);
        let server = Server::start(vec![
            (path.clone(), bytes.clone()),
            (
                format!("{path}.sha512"),
                format!("{}\n", sha512(&bytes)).into_bytes(),
            ),
        ]);

        let home = download_version(&server, data.path(), "3.9.11", Archive::TarGz)
            .expect("the archive is unpacked");

        assert_eq!(
            fs::read_to_string(home.join("bin").join(program())).unwrap(),
            "#!/bin/sh\necho maven\n"
        );
    }

    #[test]
    fn a_bad_sha512_removes_the_archive() {
        let data = test_support::TempDir::new("maven-checksum");
        let bytes = b"not the promised archive".to_vec();
        let path = archive_path("3.9.11", Archive::Zip);
        let server = Server::start(vec![
            (path.clone(), bytes),
            (
                format!("{path}.sha512"),
                b"00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000\n"
                    .to_vec(),
            ),
        ]);

        let error = download_version(&server, data.path(), "3.9.11", Archive::Zip)
            .expect_err("the checksum does not match");

        assert!(matches!(error, DownloadError::Checksum { .. }), "{error}");
        assert!(!data.path().join("tools/maven/3.9.11").exists());
        let work = data.path().join("tools/maven/.download");
        let left: Vec<_> = fs::read_dir(&work)
            .map(|entries| entries.flatten().map(|entry| entry.path()).collect())
            .unwrap_or_default();
        assert!(left.is_empty(), "{left:?}");
    }

    #[test]
    fn an_earlier_download_is_not_requested_again() {
        let data = test_support::TempDir::new("maven-again");
        let bytes = zip_archive("apache-maven-3.9.11");
        let path = archive_path("3.9.11", Archive::Zip);
        let server = Server::start(vec![
            (path.clone(), bytes.clone()),
            (
                format!("{path}.sha512"),
                format!("{}\n", sha512(&bytes)).into_bytes(),
            ),
        ]);
        let first = download_version(&server, data.path(), "3.9.11", Archive::Zip).unwrap();
        let requests = server.requests();

        let second = download_version(&server, data.path(), "3.9.11", Archive::Zip).unwrap();

        assert_eq!(first, second);
        assert_eq!(
            server.requests(),
            requests,
            "a home on the disk needs no request"
        );
    }

    #[test]
    fn a_server_that_does_not_answer_is_named() {
        let data = test_support::TempDir::new("maven-offline");

        let error = install_from(
            "http://127.0.0.1:1",
            data.path(),
            "3.9.11",
            Archive::Zip,
            &ToolProgress::new(),
            &Cancel::new(),
        )
        .expect_err("nothing listens on that port");

        assert!(matches!(error, DownloadError::Offline(_)), "{error}");
    }
}
