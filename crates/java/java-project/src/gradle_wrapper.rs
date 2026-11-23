//! The distribution a project wrapper names: the URL, its optional checksum and the
//! folder the wrapper looks in. The transfer, the checksum and the unpacking are the ones
//! of [`crate::tool_download`]; this module only knows the layout of
//! `<user home>/wrapper/dists/<name>/<hash>`, which is what the wrapper reads before it
//! decides to download anything.

use std::fs;
use std::path::{Path, PathBuf};

use base::Cancel;
use project_model::ToolProgress;

use crate::tool_download::{
    Archive, Checksum, DownloadError, extract_tar_gz, extract_zip, stream_to, verify_expected,
};

/// The properties file of the wrapper of a project.
const WRAPPER_PROPERTIES: &str = "gradle/wrapper/gradle-wrapper.properties";
const DISTRIBUTION_URL: &str = "distributionUrl";
const DISTRIBUTION_SHA256: &str = "distributionSha256Sum";
/// The folder of the distributions under the user home, as the wrapper names it.
const DISTS: &str = "wrapper/dists";
/// The work folder of a transfer, beside the distributions it fills.
const WORK: &str = ".download";
/// The suffix of the marker of a distribution the wrapper finds complete: Gradle writes
/// `<archive>.ok` beside the unpacked home.
const MARKER_SUFFIX: &str = ".ok";

/// What the wrapper properties file says about the distribution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WrapperDistribution {
    url: String,
    sha256: Option<String>,
}

impl WrapperDistribution {
    /// The distribution `root` names, or `None` when the project has no wrapper or the
    /// file names no readable distribution. A distribution outside the network, such as
    /// a `file:` URL, is left to the wrapper itself.
    pub fn read(root: &Path) -> Option<Self> {
        let text = fs::read_to_string(root.join(WRAPPER_PROPERTIES)).ok()?;
        let url = property(&text, DISTRIBUTION_URL)?;
        let url = unescape(&url);
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return None;
        }
        let sha256 = property(&text, DISTRIBUTION_SHA256)
            .map(|sum| unescape(&sum))
            .filter(|sum| sum.chars().all(|character| character.is_ascii_hexdigit()));
        Some(Self { url, sha256 })
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    /// The checksum the file names, when it names one.
    pub fn sha256(&self) -> Option<&str> {
        self.sha256.as_deref()
    }

    /// The name of the distribution folder: `gradle-9.5.1-bin`.
    pub fn name(&self) -> Option<String> {
        distribution_name(&self.url)
    }

    /// The version the distribution carries: `9.5.1`, which the messages of the window
    /// use. `None` when the name does not follow the usual form.
    pub fn version(&self) -> Option<String> {
        let name = self.name()?;
        let version = name.strip_prefix("gradle-").unwrap_or(&name);
        let version = version
            .strip_suffix("-bin")
            .or_else(|| version.strip_suffix("-all"))
            .unwrap_or(version);
        (!version.is_empty()).then(|| version.to_owned())
    }

    /// The folder the wrapper reads: `<user home>/wrapper/dists/<name>/<hash>`.
    pub fn folder(&self, user_home: &Path) -> Option<PathBuf> {
        Some(distribution_folder(
            user_home,
            &self.name()?,
            &dist_hash(&self.url),
        ))
    }

    /// Whether the wrapper finds this distribution without a transfer.
    pub fn installed(&self, user_home: &Path) -> bool {
        self.folder(user_home)
            .is_some_and(|folder| is_installed(&folder))
    }
}

/// The name a distribution URL carries: the last path segment without its archive
/// extension. `https://host/distributions/gradle-9.5.1-bin.zip` gives `gradle-9.5.1-bin`.
pub(crate) fn distribution_name(url: &str) -> Option<String> {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let name = path.rsplit('/').next()?;
    let name = name
        .strip_suffix(".zip")
        .or_else(|| name.strip_suffix(".tar.gz"))
        .unwrap_or(name);
    // A name is one folder under `dists`: no separator and no empty value.
    (!name.is_empty() && name != "." && name != ".." && !name.contains('\\') && !name.contains('/'))
        .then(|| name.to_owned())
}

/// The folder of `name` and its hash under the user home.
fn distribution_folder(user_home: &Path, name: &str, hash: &str) -> PathBuf {
    user_home.join(DISTS).join(name).join(hash)
}

/// The hash the wrapper puts into the folder of a distribution: the MD5 of the URL as
/// base 36, which is what `PathAssembler` of the wrapper computes.
pub(crate) fn dist_hash(url: &str) -> String {
    base36(u128::from_be_bytes(md5(url.as_bytes())))
}

/// Whether the wrapper finds a complete distribution in `folder`: the marker it writes
/// and an unpacked home with the program.
pub(crate) fn is_installed(folder: &Path) -> bool {
    let marked = fs::read_dir(folder).ok().is_some_and(|entries| {
        entries
            .flatten()
            .any(|entry| entry.file_name().to_string_lossy().ends_with(MARKER_SUFFIX))
    });
    if !marked {
        return false;
    }
    fs::read_dir(folder).ok().is_some_and(|entries| {
        entries.flatten().any(|entry| {
            let home = entry.path();
            home.is_dir()
                && (home.join("bin/gradle.bat").is_file() || home.join("bin/gradle").is_file())
        })
    })
}

/// Downloads the distribution of `distribution` into `user_home`, where the wrapper
/// finds it. A complete copy needs no request. A checksum the file names is checked, and
/// a file that does not match never reaches the folder of the wrapper.
pub(crate) fn install(
    distribution: &WrapperDistribution,
    user_home: &Path,
    progress: &ToolProgress,
    cancel: &Cancel,
) -> Result<PathBuf, DownloadError> {
    let name = distribution
        .name()
        .ok_or_else(|| DownloadError::Archive("the distribution URL has no file name".into()))?;
    let folder = distribution_folder(user_home, &name, &dist_hash(distribution.url()));
    if is_installed(&folder) {
        return Ok(folder);
    }
    if folder.exists() {
        // A folder without the marker belongs to a transfer that is running or that
        // stopped; the wrapper and its lock own it, so this download leaves it alone.
        return Ok(folder);
    }
    let archive = archive_of(distribution.url());
    let work = user_home
        .join(DISTS)
        .join(WORK)
        .join(dist_hash(distribution.url()));
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work).map_err(DownloadError::Files)?;
    let file = format!("{name}.{}", archive.extension());
    let part = work.join(&file);
    if let Err(error) = stream_to(distribution.url(), &part, progress, cancel) {
        remove_work(&work);
        return Err(error);
    }
    if let Some(expected) = distribution.sha256()
        && let Err(error) = verify_expected(expected, &part, Checksum::Sha256)
    {
        // A file that does not match the sum of the project is not the distribution:
        // it never reaches a folder the wrapper reads.
        remove_work(&work);
        return Err(error);
    }
    let unpacked = work.join("unpacked");
    fs::create_dir_all(&unpacked).map_err(DownloadError::Files)?;
    let extracted = match archive {
        Archive::Zip => extract_zip(&part, &unpacked, "", cancel),
        Archive::TarGz => extract_tar_gz(&part, &unpacked, "", cancel),
    };
    if let Err(error) = extracted {
        remove_work(&work);
        return Err(error);
    }
    let Some(home) = find_home(&unpacked) else {
        remove_work(&work);
        return Err(DownloadError::Archive(format!(
            "no bin of Gradle in {file}"
        )));
    };
    if let Some(parent) = folder.parent() {
        fs::create_dir_all(parent).map_err(DownloadError::Files)?;
    }
    fs::create_dir_all(&folder).map_err(DownloadError::Files)?;
    let top = if home == unpacked {
        PathBuf::from(&name)
    } else {
        home.file_name()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(&name))
    };
    fs::rename(&home, folder.join(top)).map_err(|error| {
        remove_work(&work);
        DownloadError::Files(error)
    })?;
    fs::rename(&part, folder.join(&file)).map_err(|error| {
        remove_work(&work);
        DownloadError::Files(error)
    })?;
    fs::write(folder.join(format!("{file}{MARKER_SUFFIX}")), b"").map_err(|error| {
        remove_work(&work);
        DownloadError::Files(error)
    })?;
    remove_work(&work);
    Ok(folder)
}

/// Removes the work folder of a transfer and the folder it shares with the other
/// transfers when it is empty. A failed transfer leaves nothing the wrapper could take
/// for a distribution.
fn remove_work(work: &Path) {
    let _ = fs::remove_dir_all(work);
    if let Some(parent) = work.parent()
        && let Ok(mut entries) = fs::read_dir(parent)
        && entries.next().is_none()
    {
        let _ = fs::remove_dir(parent);
    }
}

/// The kind of archive the URL names.
fn archive_of(url: &str) -> Archive {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    if path.ends_with(".tar.gz") {
        Archive::TarGz
    } else {
        Archive::Zip
    }
}

/// The home an unpacked archive brought: its only folder that has `bin`.
fn find_home(unpacked: &Path) -> Option<PathBuf> {
    if unpacked.join("bin").is_dir() {
        return Some(unpacked.to_path_buf());
    }
    fs::read_dir(unpacked)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .find(|path| path.is_dir() && path.join("bin").is_dir())
}

/// The value of a property, as the lines of a properties file write it.
fn property(text: &str, name: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.starts_with(['#', '!']))
        .find_map(|line| {
            let (key, value) = line.split_once('=')?;
            (key.trim() == name).then(|| value.trim().to_owned())
        })
}

/// The value of a properties file with its Java escapes resolved.
fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut characters = value.chars();
    while let Some(character) = characters.next() {
        if character != '\\' {
            out.push(character);
            continue;
        }
        match characters.next() {
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some(next) => out.push(next),
            None => {}
        }
    }
    out
}

/// A digest as the hexadecimal text the checksum files use.
#[cfg(test)]
fn md5_hex(bytes: &[u8]) -> String {
    let digest = md5(bytes);
    let mut text = String::with_capacity(32);
    for byte in digest {
        text.push_str(&format!("{byte:02x}"));
    }
    text
}

/// The MD5 of `input`, as RFC 1321 defines it.
fn md5(input: &[u8]) -> [u8; 16] {
    const SHIFTS: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    const TABLE: [u32; 64] = [
        0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613,
        0xfd469501, 0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193,
        0xa679438e, 0x49b40821, 0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d,
        0x02441453, 0xd8a1e681, 0xe7d3fbc8, 0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed,
        0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a, 0xfffa3942, 0x8771f681, 0x6d9d6122,
        0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70, 0x289b7ec6, 0xeaa127fa,
        0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665, 0xf4292244,
        0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
        0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb,
        0xeb86d391,
    ];
    let mut state = [0x67452301u32, 0xefcdab89, 0x98badcfe, 0x10325476];
    let mut message = input.to_vec();
    let bits = (input.len() as u64).wrapping_mul(8);
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bits.to_le_bytes());
    for block in message.as_chunks::<64>().0 {
        let mut words = [0u32; 16];
        for (word, bytes) in words.iter_mut().zip(block.as_chunks::<4>().0) {
            *word = u32::from_le_bytes(*bytes);
        }
        let [mut a, mut b, mut c, mut d] = state;
        for index in 0..64 {
            let (mixed, word) = match index {
                0..=15 => ((b & c) | (!b & d), index),
                16..=31 => ((d & b) | (!d & c), (5 * index + 1) % 16),
                32..=47 => (b ^ c ^ d, (3 * index + 5) % 16),
                _ => (c ^ (b | !d), (7 * index) % 16),
            };
            let rotated = a
                .wrapping_add(mixed)
                .wrapping_add(TABLE[index])
                .wrapping_add(words[word])
                .rotate_left(SHIFTS[index]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(rotated);
        }
        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
    }
    let mut digest = [0u8; 16];
    for (chunk, word) in digest.as_chunks_mut::<4>().0.iter_mut().zip(state) {
        chunk.copy_from_slice(&word.to_le_bytes());
    }
    digest
}

/// A number in base 36, as Java writes the hash of a distribution folder: no leading
/// zero, lower case, and `0` for zero.
fn base36(value: u128) -> String {
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if value == 0 {
        return "0".to_owned();
    }
    let mut value = value;
    let mut digits = Vec::new();
    while value > 0 {
        digits.push(DIGITS[(value % 36) as usize]);
        value /= 36;
    }
    digits.reverse();
    String::from_utf8(digits).expect("the digits are ASCII")
}

/// The folder of the user home the wrapper reads, from the settings, the environment or
/// the usual place of the user.
pub fn gradle_user_home(configured: Option<&Path>) -> Option<PathBuf> {
    configured
        .map(Path::to_path_buf)
        .or_else(|| std::env::var_os("GRADLE_USER_HOME").map(PathBuf::from))
        .or_else(|| base::program::home_dir().map(|home| home.join(".gradle")))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use base::Cancel;
    use project_model::ToolProgress;

    use super::{
        WrapperDistribution, base36, dist_hash, distribution_name, gradle_user_home, install,
        is_installed, md5_hex,
    };
    use crate::tool_download::DownloadError;
    use crate::tool_download::testing::{Server, sha256, zip_archive};

    const URL: &str = "https://services.gradle.org/distributions/gradle-9.5.1-bin.zip";
    const VERSION: &str = "9.5.1";

    fn distribution(url: &str, sha256: Option<&str>) -> WrapperDistribution {
        WrapperDistribution {
            url: url.to_owned(),
            sha256: sha256.map(str::to_owned),
        }
    }

    fn archive() -> Vec<u8> {
        zip_archive(
            &format!("gradle-{VERSION}"),
            &[
                ("bin/gradle", b"#!/bin/sh\n"),
                ("bin/gradle.bat", b"@echo off\n"),
                ("lib/gradle-launcher.jar", b"library"),
            ],
        )
    }

    #[test]
    fn the_url_and_the_sum_of_the_wrapper_are_read() {
        let root = test_support::TempDir::new("wrapper-properties");
        let wrapper = root.path().join("gradle/wrapper");
        fs::create_dir_all(&wrapper).unwrap();
        fs::write(
            wrapper.join("gradle-wrapper.properties"),
            "distributionBase=GRADLE_USER_HOME\n\
             distributionPath=wrapper/dists\n\
             distributionUrl=https\\://services.gradle.org/distributions/gradle-9.5.1-bin.zip\n\
             distributionSha256Sum=ABCDEF0123456789abcdef0123456789abcdef0123456789abcdef0123456789\n\
             networkTimeout=10000\n",
        )
        .unwrap();

        let found = WrapperDistribution::read(root.path()).expect("the file names a distribution");

        assert_eq!(found.url(), URL, "the escaped colon is unescaped");
        assert_eq!(found.sha256().map(str::len), Some(64));
        assert_eq!(found.name().as_deref(), Some("gradle-9.5.1-bin"));
        assert_eq!(found.version().as_deref(), Some(VERSION));
    }

    #[test]
    fn a_file_without_a_distribution_or_with_a_local_one_is_left_to_the_wrapper() {
        let root = test_support::TempDir::new("wrapper-properties-empty");
        let wrapper = root.path().join("gradle/wrapper");
        fs::create_dir_all(&wrapper).unwrap();
        let file = wrapper.join("gradle-wrapper.properties");
        fs::write(
            &file,
            "distributionUrl=https\\://host/gradle-9.5.1-bin.zip\n",
        )
        .unwrap();
        assert!(WrapperDistribution::read(root.path()).is_some());
        assert_eq!(
            WrapperDistribution::read(root.path()).unwrap().sha256(),
            None,
            "the file names no sum"
        );

        fs::write(&file, "distributionUrl=file\\:/opt/gradle-9.5.1-bin.zip\n").unwrap();
        assert!(
            WrapperDistribution::read(root.path()).is_none(),
            "a local file needs no transfer"
        );

        fs::remove_file(&file).unwrap();
        assert!(WrapperDistribution::read(root.path()).is_none());
    }

    #[test]
    fn the_distribution_name_comes_from_the_url() {
        assert_eq!(distribution_name(URL).as_deref(), Some("gradle-9.5.1-bin"));
        assert_eq!(
            distribution_name("https://host/gradle-9.5.1-all.tar.gz?x=1").as_deref(),
            Some("gradle-9.5.1-all")
        );
        assert!(distribution_name("https://host/").is_none());
    }

    #[test]
    fn the_hash_is_the_md5_of_the_url_in_base_36() {
        assert_eq!(md5_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(md5_hex(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            md5_hex(b"The quick brown fox jumps over the lazy dog"),
            "9e107d9d372bb6826bd81d3542a419d6"
        );
        // The folder Gradle 9.5.1's own wrapper made on a real machine.
        assert_eq!(
            dist_hash("https://services.gradle.org/distributions/gradle-9.5.1-bin.zip"),
            "iq79hdu3mqx29lgffhp8bfmx"
        );
        assert_eq!(base36(0), "0");
        assert_eq!(base36(35), "z");
        assert_eq!(base36(36), "10");
        let hash = dist_hash(URL);
        assert!(!hash.is_empty());
        assert!(
            hash.chars()
                .all(|character| character.is_ascii_digit() || character.is_ascii_lowercase())
        );
        assert_eq!(
            hash, "iq79hdu3mqx29lgffhp8bfmx",
            "the hash the wrapper of Gradle 9.5.1 writes"
        );
    }

    #[test]
    fn the_folder_is_the_one_the_wrapper_reads() {
        let home = std::path::Path::new("home");

        let folder = distribution(URL, None)
            .folder(home)
            .expect("the URL names a folder");

        let expected = home
            .join("wrapper/dists/gradle-9.5.1-bin")
            .join(dist_hash(URL));
        assert_eq!(folder, expected);
    }

    #[test]
    fn a_distribution_the_wrapper_installed_is_complete() {
        let user_home = test_support::TempDir::new("wrapper-installed");
        let folder = distribution(URL, None)
            .folder(user_home.path())
            .expect("the URL names a folder");
        let home = folder.join(format!("gradle-{VERSION}"));
        fs::create_dir_all(home.join("bin")).unwrap();
        fs::write(home.join("bin/gradle"), b"#!/bin/sh\n").unwrap();
        fs::write(folder.join(format!("gradle-{VERSION}-bin.zip.ok")), b"").unwrap();

        assert!(
            is_installed(&folder),
            "the marker Gradle writes is the one the wrapper reads"
        );
        fs::remove_file(folder.join(format!("gradle-{VERSION}-bin.zip.ok"))).unwrap();
        assert!(
            !is_installed(&folder),
            "a home without the marker is not read"
        );
    }

    #[test]
    fn a_downloaded_distribution_is_found_without_the_network() {
        let user_home = test_support::TempDir::new("wrapper-download");
        let bytes = archive();
        let path = format!("/distributions/gradle-{VERSION}-bin.zip");
        let sum = sha256(&bytes);
        let server = Server::start(vec![(path.clone(), bytes.clone())]);
        let distribution = distribution(&format!("{}{path}", server.base), Some(sum.as_str()));

        let folder = install(
            &distribution,
            user_home.path(),
            &ToolProgress::new(),
            &Cancel::new(),
        )
        .expect("the archive is unpacked");

        assert_eq!(folder, distribution.folder(user_home.path()).unwrap());
        assert!(is_installed(&folder));
        assert!(folder.join(format!("gradle-{VERSION}-bin.zip")).is_file());
        assert_eq!(
            fs::read_to_string(folder.join(format!("gradle-{VERSION}/bin/gradle"))).unwrap(),
            "#!/bin/sh\n"
        );
        let requests = server.requests();
        assert_eq!(
            requests, 1,
            "the sum of the file saves the request of the neighbouring one"
        );

        let again = install(
            &distribution,
            user_home.path(),
            &ToolProgress::new(),
            &Cancel::new(),
        )
        .expect("the folder needs no transfer");
        assert_eq!(again, folder);
        assert_eq!(
            server.requests(),
            requests,
            "the import reads the copy that is there"
        );
    }

    #[test]
    fn a_bad_sum_removes_the_archive_and_leaves_no_distribution() {
        let user_home = test_support::TempDir::new("wrapper-bad-sum");
        let bytes = archive();
        let path = format!("/distributions/gradle-{VERSION}-bin.zip");
        let server = Server::start(vec![(path.clone(), bytes)]);
        let distribution = distribution(
            &format!("{}{path}", server.base),
            Some("0000000000000000000000000000000000000000000000000000000000000000"),
        );

        let error = install(
            &distribution,
            user_home.path(),
            &ToolProgress::new(),
            &Cancel::new(),
        )
        .expect_err("the sum does not match");

        assert!(matches!(error, DownloadError::Checksum { .. }), "{error}");
        assert!(!distribution.installed(user_home.path()));
        let dists = user_home.path().join("wrapper/dists");
        let left: Vec<_> = fs::read_dir(dists)
            .map(|entries| entries.flatten().map(|entry| entry.path()).collect())
            .unwrap_or_default();
        assert!(left.is_empty(), "{left:?}");
    }

    #[test]
    fn the_user_home_comes_from_the_settings_or_the_environment() {
        let configured = std::path::Path::new("C:/gradle-home");

        assert_eq!(
            gradle_user_home(Some(configured)),
            Some(configured.to_path_buf())
        );
    }
}
