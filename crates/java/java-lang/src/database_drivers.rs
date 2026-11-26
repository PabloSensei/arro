//! The drivers of the Database window: the artifact of each product in the repository,
//! the versions it offers, the download of the jar and its checksum.
//!
//! Nothing here touches the window. Every call blocks on the network and on the disk, so
//! the window runs it in the background; [`base::Cancel`] stops a download between the
//! pieces of the file. A jar is written next to its final name and moved into place only
//! after its checksum matches, so a failure never leaves a file the helper would load.

use std::cmp::Ordering;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use base::Cancel;
use database::{DownloadProgress, DriverKind, DriverPackage};
use ring::digest;

/// The official channel of the drivers below: the central repository of the Java
/// ecosystem, the same one the build tools of the project read.
const REPOSITORY: &str = "https://repo1.maven.org/maven2";
const REPOSITORY_TITLE: &str = "Maven Central";
const USER_AGENT: &str = "Arro";
/// The folder of the downloaded drivers under the data folder of the IDE.
const DRIVERS: &str = "drivers";
/// The most a metadata or checksum file may be; a larger answer is not one of them.
const SMALL_LIMIT: usize = 1024 * 1024;

/// The artifact of one driver and the license the user reads next to its button.
struct Package {
    kind: DriverKind,
    group: &'static str,
    artifact: &'static str,
    license: &'static str,
}

const PACKAGES: &[Package] = &[
    Package {
        kind: DriverKind::H2,
        group: "com.h2database",
        artifact: "h2",
        license: "MPL-2.0 or EPL-1.0",
    },
    Package {
        kind: DriverKind::PostgreSql,
        group: "org.postgresql",
        artifact: "postgresql",
        license: "PostgreSQL License",
    },
    Package {
        kind: DriverKind::MySql,
        group: "com.mysql",
        artifact: "mysql-connector-j",
        license: "GPLv2 with the FOSS Exception",
    },
    Package {
        kind: DriverKind::MariaDb,
        group: "org.mariadb.jdbc",
        artifact: "mariadb-java-client",
        license: "LGPL-2.1",
    },
    Package {
        kind: DriverKind::Sqlite,
        group: "org.xerial",
        artifact: "sqlite-jdbc",
        license: "Apache-2.0",
    },
    Package {
        kind: DriverKind::Oracle,
        group: "com.oracle.database.jdbc",
        artifact: "ojdbc11",
        license: "Oracle Free Use Terms and Conditions",
    },
    Package {
        kind: DriverKind::SqlServer,
        group: "com.microsoft.sqlserver",
        artifact: "mssql-jdbc",
        license: "MIT",
    },
];

/// What the window shows of every driver the user may download.
pub(crate) fn packages() -> Vec<DriverPackage> {
    PACKAGES
        .iter()
        .map(|package| DriverPackage {
            kind: package.kind,
            coordinate: format!("{}:{}", package.group, package.artifact),
            license: package.license.to_owned(),
            repository: REPOSITORY_TITLE.to_owned(),
        })
        .collect()
}

/// The stable versions of the driver of `kind`, the newest first.
pub(crate) fn versions(kind: DriverKind) -> Result<Vec<String>, String> {
    let package =
        package(kind).ok_or_else(|| "No driver can be downloaded for this source.".to_owned())?;
    versions_from(REPOSITORY, package).map_err(|error| error.to_string())
}

/// Downloads the driver of `kind` at `version`, or the newest stable one when the version
/// is empty, and returns the jar. A driver already in `data_dir` is returned without the
/// network.
pub(crate) fn download(
    kind: DriverKind,
    version: Option<&str>,
    data_dir: &Path,
    progress: &DownloadProgress,
    cancel: &Cancel,
) -> Result<PathBuf, String> {
    let package =
        package(kind).ok_or_else(|| "No driver can be downloaded for this source.".to_owned())?;
    download_from(REPOSITORY, package, data_dir, version, progress, cancel)
        .map_err(|error| error.to_string())
}

fn package(kind: DriverKind) -> Option<&'static Package> {
    PACKAGES.iter().find(|package| package.kind == kind)
}

/// What a request to the repository can fail with.
#[derive(Debug)]
enum Error {
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
    Files(io::Error),
    NoStableVersion,
}

impl std::fmt::Display for Error {
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
                "The checksum of the driver does not match: the repository says {expected}, \
                 the file is {actual}."
            ),
            Self::Files(error) => write!(formatter, "The driver file cannot be written: {error}"),
            Self::NoStableVersion => {
                formatter.write_str("The repository offers no stable version of the driver.")
            }
        }
    }
}

/// The versions a metadata file lists, the newest stable first.
fn stable_versions(metadata: &str) -> Vec<String> {
    let mut versions: Vec<String> = elements(metadata, "version")
        .into_iter()
        .filter(|version| !is_pre_release(version))
        .map(str::to_owned)
        .collect();
    versions.sort_by(|left, right| compare_versions(right, left));
    versions.dedup();
    versions
}

/// The text of every `<name>` element of an XML file. The metadata of a repository is a
/// flat list of tags, so no parser is needed and none can pull the window into its rules.
fn elements<'a>(text: &'a str, name: &str) -> Vec<&'a str> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let mut rest = text;
    let mut found = Vec::new();
    while let Some(start) = rest.find(&open) {
        let after = &rest[start + open.len()..];
        let Some(end) = after.find(&close) else {
            break;
        };
        found.push(after[..end].trim());
        rest = &after[end + close.len()..];
    }
    found
}

/// Whether a version is one of the pre-releases the window does not offer: alpha, beta,
/// release candidate, snapshot, milestone, early access or preview.
fn is_pre_release(version: &str) -> bool {
    version.split(['.', '-', '_']).any(|part| {
        let part = part.to_ascii_lowercase();
        part.starts_with("alpha")
            || part.starts_with("beta")
            || part.starts_with("rc")
            || part.starts_with("snapshot")
            || part.starts_with("eap")
            || part.starts_with("preview")
            || part == "m"
            || (part.starts_with('m')
                && part[1..]
                    .chars()
                    .all(|character| character.is_ascii_digit()))
    })
}

/// The order of two versions: the numbers compare as numbers, so `9.10` is newer than
/// `9.9`, and a number is newer than a word in the same place.
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

fn versions_from(repository: &str, package: &Package) -> Result<Vec<String>, Error> {
    let url = format!(
        "{repository}/{}/{}/maven-metadata.xml",
        group_path(package.group),
        package.artifact
    );
    let metadata = fetch(&url)?;
    let text = String::from_utf8(metadata)
        .map_err(|error| Error::Offline(format!("the metadata is not text: {error}")))?;
    let versions = stable_versions(&text);
    if versions.is_empty() {
        return Err(Error::NoStableVersion);
    }
    Ok(versions)
}

fn download_from(
    repository: &str,
    package: &Package,
    data_dir: &Path,
    version: Option<&str>,
    progress: &DownloadProgress,
    cancel: &Cancel,
) -> Result<PathBuf, Error> {
    let version = match version.map(str::trim).filter(|version| !version.is_empty()) {
        Some(version) => version.to_owned(),
        None => match installed_driver(data_dir, package) {
            // A driver of an earlier download serves as well as a fresh one: nothing is
            // requested while the file is there.
            Some(jar) => return Ok(jar),
            None => versions_from(repository, package)?
                .into_iter()
                .next()
                .ok_or(Error::NoStableVersion)?,
        },
    };
    let folder = driver_folder(data_dir, package, &version);
    let jar = folder.join(format!("{}-{version}.jar", package.artifact));
    if is_jar(&jar) {
        return Ok(jar);
    }
    fs::create_dir_all(&folder).map_err(Error::Files)?;
    let part = folder.join(format!("{}-{version}.jar.part", package.artifact));
    let url = format!(
        "{repository}/{}/{}/{version}/{}-{version}.jar",
        group_path(package.group),
        package.artifact,
        package.artifact
    );
    let written = stream_to(&url, &part, progress, cancel);
    if let Err(error) = written {
        let _ = fs::remove_file(&part);
        return Err(error);
    }
    if let Err(error) = verify(&url, &part) {
        // A file that does not match the repository is not a driver: it never reaches the
        // name the helper loads.
        let _ = fs::remove_file(&part);
        return Err(error);
    }
    fs::rename(&part, &jar).map_err(|error| {
        let _ = fs::remove_file(&part);
        Error::Files(error)
    })?;
    Ok(jar)
}

/// The folder of one version of a driver: `<data>/drivers/<group>/<artifact>/<version>/`.
fn driver_folder(data_dir: &Path, package: &Package, version: &str) -> PathBuf {
    data_dir
        .join(DRIVERS)
        .join(safe_name(package.group))
        .join(safe_name(package.artifact))
        .join(safe_name(version))
}

/// The newest jar of the driver that an earlier download left in the data folder.
fn installed_driver(data_dir: &Path, package: &Package) -> Option<PathBuf> {
    let root = data_dir
        .join(DRIVERS)
        .join(safe_name(package.group))
        .join(safe_name(package.artifact));
    let mut versions: Vec<String> = fs::read_dir(root)
        .ok()?
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();
    versions.sort_by(|left, right| compare_versions(right, left));
    versions.into_iter().find_map(|version| {
        let jar = driver_folder(data_dir, package, &version)
            .join(format!("{}-{version}.jar", package.artifact));
        is_jar(&jar).then_some(jar)
    })
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
        "driver".to_owned()
    } else {
        name
    }
}

fn group_path(group: &str) -> String {
    group.replace('.', "/")
}

fn is_jar(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|metadata| metadata.is_file() && metadata.len() > 0)
}

/// The agent of the repository: the same limits as every other download of the IDE, so a
/// body has no total limit and only a silent server fails.
fn agent() -> ureq::Agent {
    java_project::download_agent()
}

fn get(url: &str) -> Result<ureq::http::Response<ureq::Body>, Error> {
    let response = agent()
        .get(url)
        .header("User-Agent", USER_AGENT)
        .call()
        .map_err(network)?;
    let status = response.status().as_u16();
    match status {
        200..=299 => Ok(response),
        404 => Err(Error::NotFound(url.to_owned())),
        status => Err(Error::Http(status)),
    }
}

fn network(error: ureq::Error) -> Error {
    match error {
        ureq::Error::Timeout(_) => {
            Error::Offline("the repository did not answer in time".to_owned())
        }
        ureq::Error::HostNotFound => Error::Offline("the host is not known".to_owned()),
        ureq::Error::Io(error) if error.kind() == io::ErrorKind::TimedOut => {
            Error::Offline("the repository did not answer in time".to_owned())
        }
        other => Error::Offline(other.to_string()),
    }
}

/// Reads a small file of the repository: the metadata of the versions or a checksum.
fn fetch(url: &str) -> Result<Vec<u8>, Error> {
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
                    return Err(Error::Offline(format!("{url} is not a small file")));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(Error::Offline(error.to_string())),
        }
    }
    Ok(bytes)
}

/// Writes the jar to `path` as it comes, reporting the bytes and stopping on `cancel`.
fn stream_to(
    url: &str,
    path: &Path,
    progress: &DownloadProgress,
    cancel: &Cancel,
) -> Result<(), Error> {
    if cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    let response = get(url)?;
    let total = response
        .headers()
        .get("content-length")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok());
    let mut body = response.into_body().into_reader();
    let mut file = File::create(path).map_err(Error::Files)?;
    let mut written = 0u64;
    progress.set(0, total);
    let mut buffer = [0u8; 64 * 1024];
    loop {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        match body.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                file.write_all(&buffer[..read]).map_err(Error::Files)?;
                written += read as u64;
                progress.set(written, total);
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(Error::Offline(error.to_string())),
        }
    }
    if let Some(total) = total
        && written < total
    {
        return Err(Error::Offline(
            "the repository closed the connection before the file ended".to_owned(),
        ));
    }
    file.flush().map_err(Error::Files)
}

/// Checks the jar against the `.sha1` of the repository, or its `.sha256` when the
/// repository has no `.sha1`.
fn verify(url: &str, path: &Path) -> Result<(), Error> {
    let (expected, algorithm) = match fetch(&format!("{url}.sha1")) {
        Ok(bytes) => (parse_checksum(&bytes)?, &digest::SHA1_FOR_LEGACY_USE_ONLY),
        Err(Error::NotFound(_)) => {
            let bytes = fetch(&format!("{url}.sha256"))?;
            (parse_checksum(&bytes)?, &digest::SHA256)
        }
        Err(error) => return Err(error),
    };
    let actual = digest_file(algorithm, path).map_err(Error::Files)?;
    if !expected.eq_ignore_ascii_case(&actual) {
        return Err(Error::Checksum { expected, actual });
    }
    Ok(())
}

/// The checksum a `.sha1` or `.sha256` file carries: the first token of its first line.
fn parse_checksum(bytes: &[u8]) -> Result<String, Error> {
    let text = String::from_utf8_lossy(bytes);
    let checksum = text
        .split_whitespace()
        .next()
        .map(str::trim)
        .filter(|checksum| !checksum.is_empty())
        .ok_or_else(|| Error::Offline("the checksum file is empty".to_owned()))?;
    if !checksum
        .chars()
        .all(|character| character.is_ascii_hexdigit())
    {
        return Err(Error::Offline(
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
    use std::cmp::Ordering;
    use std::fs;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering as AtomicOrdering};
    use std::thread;
    use std::time::Duration;

    use base::Cancel;
    use database::{DownloadProgress, DriverKind};

    use super::{
        Error, Package, compare_versions, download_from, driver_folder, is_pre_release,
        parse_checksum, stable_versions, versions_from,
    };

    fn package() -> &'static Package {
        super::package(DriverKind::PostgreSql).expect("PostgreSQL has a driver")
    }

    fn metadata(versions: &[&str]) -> String {
        let mut text = String::from("<metadata><versioning><versions>");
        for version in versions {
            text.push_str(&format!("<version>{version}</version>"));
        }
        text.push_str("</versions></versioning></metadata>");
        text
    }

    fn sha1(bytes: &[u8]) -> String {
        let digest = ring::digest::digest(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY, bytes);
        super::hex(digest.as_ref())
    }

    #[test]
    fn the_newest_stable_version_is_chosen() {
        let text = metadata(&[
            "42.7.3",
            "42.7.10",
            "42.8.0-beta1",
            "42.9.0-rc2",
            "42.7.9-alpha",
            "43.0.0",
        ]);

        let versions = stable_versions(&text);

        assert_eq!(versions.first().map(String::as_str), Some("43.0.0"));
        assert!(!versions.iter().any(|version| is_pre_release(version)));
        assert_eq!(compare_versions("42.7.10", "42.7.9"), Ordering::Greater);
        assert_eq!(versions[1], "42.7.10");
    }

    #[test]
    fn a_pre_release_is_not_a_default() {
        for version in [
            "1.0.0-alpha",
            "1.0.0-beta2",
            "1.0.0-rc1",
            "1.0.0-SNAPSHOT",
            "1.0.0-M1",
        ] {
            assert!(is_pre_release(version), "{version}");
        }
        for version in ["42.7.4", "12.8.1.jre11", "3.46.1.0", "23.5.0.24.07"] {
            assert!(!is_pre_release(version), "{version}");
        }
    }

    #[test]
    fn a_checksum_file_gives_its_first_token() {
        let checksum =
            parse_checksum(b"a9993e364706816aba3e25717850c26c9cd0d89d  postgresql-42.7.4.jar\n")
                .expect("the line carries a checksum");
        assert_eq!(checksum, "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert!(parse_checksum(b"not a checksum\n").is_err());
        assert!(parse_checksum(b"").is_err());
    }

    #[test]
    fn the_jar_lives_in_the_data_folder_of_the_ide() {
        let folder = driver_folder(Path::new("data"), package(), "42.7.4");

        assert_eq!(
            folder,
            Path::new("data/drivers/org.postgresql/postgresql/42.7.4")
        );
        let escaped = driver_folder(Path::new("data"), package(), "../..");
        assert!(
            !escaped
                .components()
                .any(|part| matches!(part, std::path::Component::ParentDir)),
            "{}",
            escaped.display()
        );
    }

    /// A stand-in of the repository: it answers the paths a test asks for and nothing
    /// else, so the suite never reaches the network.
    struct Server {
        base: String,
        requests: Arc<AtomicUsize>,
        stop: Arc<AtomicBool>,
        handle: Option<thread::JoinHandle<()>>,
    }

    enum Answer {
        /// A whole file.
        File(Vec<u8>),
        /// A header that promises more than the body carries, then the end of the socket.
        Cut,
        /// Bytes that come slowly, so a test stops the download in the middle.
        Slow,
    }

    impl Server {
        fn start(answers: Vec<(String, Answer)>) -> Self {
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
                            // of the listener: a read before the request arrives would see
                            // WouldBlock, the request would be answered half-read, and the
                            // socket closed with the rest of it would reset the connection.
                            // The limits keep the thread from waiting on a vanished client.
                            socket.set_nonblocking(false).ok();
                            socket.set_read_timeout(Some(Duration::from_secs(10))).ok();
                            socket.set_write_timeout(Some(Duration::from_secs(10))).ok();
                            let path = request_path(&mut socket);
                            counter.fetch_add(1, AtomicOrdering::Relaxed);
                            let answer = answers
                                .iter()
                                .find(|(known, _)| known == &path)
                                .map(|(_, answer)| answer);
                            let _ = match answer {
                                Some(Answer::File(bytes)) => {
                                    let head = format!(
                                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                        bytes.len()
                                    );
                                    socket
                                        .write_all(head.as_bytes())
                                        .and_then(|_| socket.write_all(bytes))
                                }
                                Some(Answer::Cut) => {
                                    let head = "HTTP/1.1 200 OK\r\nContent-Length: 4096\r\nConnection: close\r\n\r\n";
                                    socket
                                        .write_all(head.as_bytes())
                                        .and_then(|_| socket.write_all(b"only a part"))
                                }
                                Some(Answer::Slow) => {
                                    let head = "HTTP/1.1 200 OK\r\nContent-Length: 4096\r\nConnection: close\r\n\r\n";
                                    socket.write_all(head.as_bytes()).map(|_| {
                                        for _ in 0..64 {
                                            if stopping.load(AtomicOrdering::Relaxed)
                                                || socket.write_all(&[b'x'; 64]).is_err()
                                            {
                                                break;
                                            }
                                            thread::sleep(Duration::from_millis(30));
                                        }
                                    })
                                }
                                None => socket.write_all(
                                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                                ),
                            };
                            let _ = socket.flush();
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
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        let text = String::from_utf8_lossy(&head);
        text.split_whitespace()
            .nth(1)
            .unwrap_or_default()
            .to_owned()
    }

    fn jar_path() -> String {
        "/org/postgresql/postgresql/42.7.4/postgresql-42.7.4.jar".to_owned()
    }

    fn jar_file(dir: &Path) -> PathBuf {
        dir.join("drivers/org.postgresql/postgresql/42.7.4/postgresql-42.7.4.jar")
    }

    fn download(server: &Server, dir: &Path) -> Result<PathBuf, Error> {
        download_from(
            &server.base,
            package(),
            dir,
            Some("42.7.4"),
            &DownloadProgress::new(),
            &Cancel::new(),
        )
    }

    #[test]
    fn a_good_download_lands_under_its_version() {
        let folder = test_support::TempDir::new("database-driver-download");
        let payload = b"a driver file".to_vec();
        let path = jar_path();
        let server = Server::start(vec![
            (path.clone(), Answer::File(payload.clone())),
            (
                format!("{path}.sha1"),
                Answer::File(format!("{}  postgresql-42.7.4.jar\n", sha1(&payload)).into_bytes()),
            ),
        ]);

        let jar = download(&server, folder.path()).expect("the driver is downloaded");

        assert_eq!(jar, jar_file(folder.path()));
        assert_eq!(fs::read(&jar).unwrap(), payload);
    }

    #[test]
    fn a_request_split_over_two_writes_is_answered() {
        let server = Server::start(vec![(jar_path(), Answer::File(b"a driver file".to_vec()))]);
        let address = server
            .base
            .strip_prefix("http://")
            .expect("the base of the server");
        let mut socket = TcpStream::connect(address).expect("the server listens");
        socket
            .set_read_timeout(Some(Duration::from_secs(10)))
            .expect("a read limit");
        let head = format!("GET {} HTTP/1.1\r\nHost: {address}\r\n", jar_path());
        socket
            .write_all(head.as_bytes())
            .expect("the start of the request");
        // A non-blocking listener hands its mode to the accepted socket on Windows, and
        // the rest of such a request would be lost in a WouldBlock.
        thread::sleep(Duration::from_millis(200));
        socket
            .write_all(b"\r\n")
            .expect("the end of the request head");
        let mut answer = Vec::new();
        socket
            .read_to_end(&mut answer)
            .expect("the answer of the server");
        let text = String::from_utf8_lossy(&answer);
        assert!(text.starts_with("HTTP/1.1 200 OK"), "{text}");
        assert!(text.ends_with("a driver file"), "{text}");
    }

    #[test]
    fn a_bad_checksum_removes_the_file() {
        let folder = test_support::TempDir::new("database-driver-checksum");
        let payload = b"a driver file".to_vec();
        let path = jar_path();
        let server = Server::start(vec![
            (path.clone(), Answer::File(payload)),
            (
                format!("{path}.sha1"),
                Answer::File(b"0000000000000000000000000000000000000000\n".to_vec()),
            ),
        ]);

        let error = download(&server, folder.path()).expect_err("the checksum does not match");

        assert!(matches!(error, Error::Checksum { .. }), "{error}");
        assert!(!jar_file(folder.path()).exists());
        assert!(
            !folder
                .path()
                .join("drivers/org.postgresql/postgresql/42.7.4/postgresql-42.7.4.jar.part")
                .exists()
        );
    }

    #[test]
    fn an_interrupted_download_leaves_nothing() {
        let folder = test_support::TempDir::new("database-driver-cut");
        let server = Server::start(vec![(jar_path(), Answer::Cut)]);

        let error = download(&server, folder.path()).expect_err("the file ends early");

        assert!(matches!(error, Error::Offline(_)), "{error}");
        assert!(!jar_file(folder.path()).exists());
    }

    #[test]
    fn a_stopped_download_leaves_nothing() {
        let folder = test_support::TempDir::new("database-driver-cancel");
        let server = Server::start(vec![(jar_path(), Answer::Slow)]);
        let cancel = Cancel::new();
        let stop = cancel.clone();
        let stopper = thread::spawn(move || {
            thread::sleep(Duration::from_millis(120));
            stop.cancel();
        });

        let error = download_from(
            &server.base,
            package(),
            folder.path(),
            Some("42.7.4"),
            &DownloadProgress::new(),
            &cancel,
        )
        .expect_err("the download is stopped");

        stopper.join().unwrap();
        assert!(matches!(error, Error::Cancelled), "{error}");
        assert!(!jar_file(folder.path()).exists());
    }

    #[test]
    fn an_earlier_download_is_not_requested_again() {
        let folder = test_support::TempDir::new("database-driver-again");
        let payload = b"a driver file".to_vec();
        let path = jar_path();
        let server = Server::start(vec![
            (path.clone(), Answer::File(payload.clone())),
            (
                format!("{path}.sha1"),
                Answer::File(format!("{}\n", sha1(&payload)).into_bytes()),
            ),
        ]);

        let first = download(&server, folder.path()).expect("the first download");
        let requests = server.requests();
        let second = download_from(
            &server.base,
            package(),
            folder.path(),
            None,
            &DownloadProgress::new(),
            &Cancel::new(),
        )
        .expect("the file is already there");

        assert_eq!(first, second);
        assert_eq!(
            server.requests(),
            requests,
            "a file on the disk needs no request"
        );
    }

    #[test]
    fn a_version_without_a_file_is_reported() {
        let folder = test_support::TempDir::new("database-driver-missing");
        let server = Server::start(Vec::new());

        let error = download(&server, folder.path()).expect_err("the repository has no file");

        assert!(matches!(error, Error::NotFound(_)), "{error}");
    }

    #[test]
    fn metadata_gives_the_versions_of_the_server() {
        let text = metadata(&["42.7.4", "42.7.5"]);
        let server = Server::start(vec![(
            "/org/postgresql/postgresql/maven-metadata.xml".to_owned(),
            Answer::File(text.into_bytes()),
        )]);

        let found = versions_from(&server.base, package()).expect("the versions are read");

        assert_eq!(found, ["42.7.5", "42.7.4"]);
    }

    #[test]
    fn a_server_that_does_not_answer_is_named() {
        let folder = test_support::TempDir::new("database-driver-offline");

        let error = download_from(
            "http://127.0.0.1:1",
            package(),
            folder.path(),
            Some("42.7.4"),
            &DownloadProgress::new(),
            &Cancel::new(),
        )
        .expect_err("nothing listens on that port");

        assert!(matches!(error, Error::Offline(_)), "{error}");
    }

    #[test]
    fn a_sha256_is_read_when_the_repository_has_no_sha1() {
        let folder = test_support::TempDir::new("database-driver-sha256");
        let payload = b"a driver file".to_vec();
        let path = jar_path();
        let digest = ring::digest::digest(&ring::digest::SHA256, &payload);
        let server = Server::start(vec![
            (path.clone(), Answer::File(payload)),
            (
                format!("{path}.sha256"),
                Answer::File(format!("{}\n", super::hex(digest.as_ref())).into_bytes()),
            ),
        ]);

        let jar = download(&server, folder.path()).expect("the SHA-256 is checked");

        assert_eq!(jar, jar_file(folder.path()));
    }

    #[test]
    fn the_newest_version_of_the_repository_is_the_default() {
        let folder = test_support::TempDir::new("database-driver-default");
        let payload = b"a newer driver".to_vec();
        let path = "/org/postgresql/postgresql/42.7.5/postgresql-42.7.5.jar";
        let server = Server::start(vec![
            (
                "/org/postgresql/postgresql/maven-metadata.xml".to_owned(),
                Answer::File(metadata(&["42.7.4", "42.7.5"]).into_bytes()),
            ),
            (path.to_owned(), Answer::File(payload.clone())),
            (
                format!("{path}.sha1"),
                Answer::File(format!("{}\n", sha1(&payload)).into_bytes()),
            ),
        ]);

        let jar = download_from(
            &server.base,
            package(),
            folder.path(),
            None,
            &DownloadProgress::new(),
            &Cancel::new(),
        )
        .expect("the newest version is downloaded");

        assert_eq!(
            jar,
            folder
                .path()
                .join("drivers/org.postgresql/postgresql/42.7.5/postgresql-42.7.5.jar")
        );
    }
}
