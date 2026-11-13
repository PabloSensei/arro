//! Version checks and in-place updates from GitHub Releases. There is no UI and no
//! network work on its own: the caller runs every request away from the interface.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};

/// A system the release is built for. The archive of a release is named after it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    WindowsX86_64,
    LinuxX86_64,
    MacosAarch64,
    MacosX86_64,
}

impl Platform {
    /// Every system the project publishes an archive for.
    pub const ALL: [Self; 4] = [
        Self::WindowsX86_64,
        Self::LinuxX86_64,
        Self::MacosAarch64,
        Self::MacosX86_64,
    ];

    /// The suffix of the archive name, as in `arro-0.1.0-windows-x86_64.zip`.
    pub const fn name(self) -> &'static str {
        match self {
            Self::WindowsX86_64 => "windows-x86_64",
            Self::LinuxX86_64 => "linux-x86_64",
            Self::MacosAarch64 => "macos-aarch64",
            Self::MacosX86_64 => "macos-x86_64",
        }
    }

    /// The container of the archive: a zip on Windows and macOS, a compressed tar on Linux.
    pub const fn archive_extension(self) -> &'static str {
        match self {
            Self::LinuxX86_64 => "tar.gz",
            Self::WindowsX86_64 | Self::MacosAarch64 | Self::MacosX86_64 => "zip",
        }
    }

    /// The system of this build, or `None` when the project publishes no release for
    /// it. The program still builds and runs there; only the updates are off.
    pub const fn current() -> Option<Self> {
        #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
        {
            Some(Self::WindowsX86_64)
        }
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        {
            Some(Self::LinuxX86_64)
        }
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        {
            Some(Self::MacosAarch64)
        }
        #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
        {
            Some(Self::MacosX86_64)
        }
        #[cfg(not(any(
            all(target_os = "windows", target_arch = "x86_64"),
            all(target_os = "linux", target_arch = "x86_64"),
            all(target_os = "macos", target_arch = "aarch64"),
            all(target_os = "macos", target_arch = "x86_64")
        )))]
        {
            None
        }
    }
}

/// Where the latest release is described.
pub const RELEASES_URL: &str = "https://api.github.com/repos/PabloSensei/arro/releases/latest";
/// Replaces [`RELEASES_URL`], for tests and for a moved repository.
pub const UPDATE_URL_ENV: &str = "ARRO_UPDATE_URL";
/// Set to `1` to turn the automatic check off.
pub const NO_UPDATE_CHECK_ENV: &str = "ARRO_NO_UPDATE_CHECK";
/// The suffix the assets of this build carry, `windows-x86_64` for example, and empty
/// on a system without a release.
pub const PLATFORM: &str = match Platform::current() {
    Some(platform) => platform.name(),
    None => "",
};
/// What a manual "Check for Updates" says on a system without a release. No request
/// leaves the machine there.
pub const NO_RELEASE_NOTICE: &str =
    "Updates are not available for this system; build Arro from source to update.";
/// The list of files the installed release brought, one relative path per line. It
/// tells the first start after an update which folders of the old release are gone.
pub const MANIFEST_FILE: &str = "install.manifest";

const USER_AGENT: &str = "Arro";
const TIMEOUT: Duration = Duration::from_secs(60);
/// How often a wait asks whether a process or a file is gone.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

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
    /// and drafts are skipped. A system without a release has nothing to update to, so
    /// no request is made there.
    pub fn check(&self, current: &Version) -> Result<Option<Release>, Error> {
        if Platform::current().is_none() {
            return Ok(None);
        }
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
        {
            // The archive goes to the disk as it comes: it is tens of megabytes, more
            // than a body read into memory may hold.
            let mut response = self.call(release.archive.url.as_str(), None)?;
            let mut file = File::create(partial)?;
            let mut body = response.body_mut().as_reader();
            io::copy(&mut body, &mut file).map_err(|error| Error::Http(error.to_string()))?;
            file.flush()?;
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
        let mut response = self.call(url, Some(TIMEOUT))?;
        response
            .body_mut()
            .read_to_vec()
            .map_err(|error| Error::Http(error.to_string()))
    }

    /// Sends a GET to `url`. `total` bounds the whole call; without it only the connection
    /// and the start of the answer are bounded, so a long download on a slow line is not
    /// cut while bytes still arrive.
    fn call(
        &self,
        url: &str,
        total: Option<Duration>,
    ) -> Result<ureq::http::Response<ureq::Body>, Error> {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(total)
            .timeout_connect(Some(TIMEOUT))
            .timeout_recv_response(Some(TIMEOUT))
            .tls_config(
                ureq::tls::TlsConfig::builder()
                    .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                    .build(),
            )
            .build()
            .into();
        agent
            .get(url)
            .header("User-Agent", USER_AGENT)
            .header("Accept", "application/vnd.github+json")
            .call()
            .map_err(|error| Error::Http(error.to_string()))
    }
}

/// The release in a GitHub Releases response. `None` for a draft, a prerelease and a
/// system without a release of its own.
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
    let Some(expected) = archive_name(&version) else {
        return Ok(None);
    };
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

/// What the archive of `version` is called on `platform`. `None` when the project
/// publishes no release for that system: there is nothing to update to.
pub fn archive_name_for(version: &Version, platform: Option<Platform>) -> Option<String> {
    platform.map(|platform| {
        format!(
            "arro-{version}-{}.{}",
            platform.name(),
            platform.archive_extension()
        )
    })
}

/// What the archive of `version` is called on this build's platform.
pub fn archive_name(version: &Version) -> Option<String> {
    archive_name_for(version, Platform::current())
}

/// The message a manual "Check for Updates" shows on `platform`, or `None` when the
/// project publishes a release for it and the check may ask the server.
pub fn manual_check_notice(platform: Option<Platform>) -> Option<&'static str> {
    platform.is_none().then_some(NO_RELEASE_NOTICE)
}

/// A version string such as `0.1.0` as a version, for the running build.
pub fn parse_version(text: &str) -> Option<Version> {
    Version::parse(text.trim()).ok()
}

/// The folder an update replaces: the folder of the executable, or the `*.app` bundle
/// that holds it on macOS.
pub fn install_dir() -> io::Result<PathBuf> {
    let executable = std::env::current_exe()?;
    Ok(bundle_root(&executable))
}

/// [`install_dir`] for a program at `executable`. macOS keeps the program inside a
/// bundle, so the folder to replace is the bundle, not `Contents/MacOS`.
fn bundle_root(executable: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        let mut folder = executable.parent();
        while let Some(path) = folder {
            if path.extension().is_some_and(|extension| extension == "app") {
                return path.to_owned();
            }
            folder = path.parent();
        }
    }
    executable.parent().unwrap_or(executable).to_owned()
}

/// The program of an installed release, next to [`install_dir`] or inside the macOS
/// bundle. The running executable may have been renamed to `*.old` by the update, so
/// the caller starts this one instead.
fn executable_for(install_dir: &Path, name: &str) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        bundle_executable(install_dir, name)
    }
    #[cfg(not(target_os = "macos"))]
    {
        install_dir.join(name)
    }
}

/// The program inside a `*.app` bundle. The bundle is also replaced on other systems
/// in the tests, so this does not depend on the system it is compiled for.
#[cfg(any(target_os = "macos", test))]
fn bundle_executable(bundle: &Path, name: &str) -> PathBuf {
    bundle.join("Contents").join("MacOS").join(name)
}

/// Replaces the files of `install_dir` with the contents of `archive` and returns the
/// program of the new release to start. The running executable is renamed to `*.old`
/// first; any error puts every file back the way it was. On macOS the whole `*.app`
/// bundle is replaced.
pub fn stage(archive: &Path, install_dir: &Path) -> Result<PathBuf, Error> {
    let name = std::env::current_exe()
        .ok()
        .and_then(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .ok_or_else(|| {
            Error::Io(io::Error::new(
                io::ErrorKind::NotFound,
                "the name of the running program is unknown",
            ))
        })?;
    #[cfg(target_os = "macos")]
    stage_bundle(archive, install_dir, Some(&name))?;
    #[cfg(not(target_os = "macos"))]
    stage_as(archive, install_dir, Some(&name))?;
    Ok(executable_for(install_dir, &name))
}

/// [`stage`] with the name of the executable that is running. Tests name it themselves.
#[cfg_attr(target_os = "macos", allow(dead_code))]
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
    if let Err(error) = transaction.replace(&staged, install_dir, &temp, &files, running) {
        transaction.rollback();
        let _ = fs::remove_dir_all(&temp);
        return Err(error);
    }
    if let Some(name) = running
        && let Err(error) = platform::make_executable(&install_dir.join(name))
    {
        transaction.rollback();
        let _ = fs::remove_dir_all(&temp);
        return Err(error.into());
    }
    // The start of the new version reads this list to remove the folders of the old
    // one, so a failure here takes the replacement back too.
    if let Err(error) = write_manifest(install_dir, &files) {
        transaction.rollback();
        let _ = fs::remove_file(install_dir.join(MANIFEST_FILE));
        let _ = fs::remove_dir_all(&temp);
        return Err(error.into());
    }
    let _ = fs::remove_dir_all(&temp);
    Ok(())
}

/// [`stage`] for macOS: the archive holds the whole `*.app` bundle, and the bundle of
/// the running program is renamed to `*.old` and replaced in one step. A running
/// bundle can be renamed, so nothing inside it has to be touched file by file.
#[cfg(any(target_os = "macos", test))]
fn stage_bundle(archive: &Path, bundle: &Path, running: Option<&str>) -> Result<(), Error> {
    let parent = bundle.parent().unwrap_or(bundle);
    let temp = parent.join(format!(".arro-stage-{}-{}", std::process::id(), next_id()));
    if temp.exists() {
        fs::remove_dir_all(&temp)?;
    }
    let extracted = temp.join("files");
    fs::create_dir_all(&extracted)?;

    let files = extract(archive, &extracted).inspect_err(|_| {
        let _ = fs::remove_dir_all(&temp);
    })?;
    // macOS archives hold `Arro.app/...`; a bundle is replaced as a folder, not file
    // by file, so the folder inside the archive is the new bundle. A folder of the
    // same name wins over the only folder of the archive.
    let same_name = bundle
        .file_name()
        .map(|name| extracted.join(name))
        .filter(|folder| folder.is_dir());
    let staged = same_name
        .or_else(|| single_root(&files).map(|root| extracted.join(root)))
        .unwrap_or_else(|| extracted.clone());
    if !staged.is_dir() {
        let _ = fs::remove_dir_all(&temp);
        return Err(Error::Tar(format!(
            "{} has no application folder",
            archive.display()
        )));
    }

    let old = old_bundle(bundle);
    if old.exists() {
        fs::remove_dir_all(&old)?;
    }
    fs::rename(bundle, &old)?;
    if let Err(error) = fs::rename(&staged, bundle) {
        let _ = fs::rename(&old, bundle);
        let _ = fs::remove_dir_all(&temp);
        return Err(error.into());
    }
    if let Some(name) = running {
        platform::make_executable(&bundle_executable(bundle, name))?;
    }
    let _ = fs::remove_dir_all(&temp);
    Ok(())
}

/// `Arro.app` and `Arro.app.old` next to each other.
#[cfg(any(target_os = "macos", test))]
fn old_bundle(bundle: &Path) -> PathBuf {
    let mut name = bundle.file_name().unwrap_or_default().to_os_string();
    name.push(".old");
    bundle.with_file_name(name)
}

/// Deletes the bundle an earlier update replaced, `Arro.app.old`. The first start of
/// the new version does this, while no process runs from the old bundle any more.
#[cfg(any(target_os = "macos", test))]
pub fn remove_old_bundle(bundle: &Path) -> io::Result<usize> {
    let old = old_bundle(bundle);
    if old.is_dir() {
        fs::remove_dir_all(&old)?;
        return Ok(1);
    }
    Ok(0)
}

/// Writes the files of the installed release, relative to `install_dir`, into
/// [`MANIFEST_FILE`].
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn write_manifest(install_dir: &Path, files: &[PathBuf]) -> io::Result<()> {
    let mut listed: Vec<String> = files
        .iter()
        .map(|file| file.to_string_lossy().replace('\\', "/"))
        .collect();
    listed.sort();
    let mut text = String::new();
    for file in listed {
        text.push_str(&file);
        text.push('\n');
    }
    fs::write(install_dir.join(MANIFEST_FILE), text)
}

/// The files of the release that installed this folder, or `None` when the installer
/// left no list.
pub fn read_manifest(install_dir: &Path) -> io::Result<Option<Vec<PathBuf>>> {
    let text = match fs::read_to_string(install_dir.join(MANIFEST_FILE)) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    Ok(Some(
        text.lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(PathBuf::from)
            .collect(),
    ))
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

/// Removes the folders under `tools` that the manifest of the installed release does
/// not name. A tool that the new release still ships keeps its folder. An entry that
/// cannot be removed goes to the journal and does not stop the others.
pub fn remove_stale_tools(install_dir: &Path) -> io::Result<usize> {
    let Some(files) = read_manifest(install_dir)? else {
        return Ok(0);
    };
    let listed: Vec<String> = files
        .iter()
        .map(|file| file.to_string_lossy().replace('\\', "/"))
        .collect();
    let entries = match fs::read_dir(install_dir.join("tools")) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let prefix = format!("tools/{name}/");
        if listed.iter().any(|file| file.starts_with(&prefix)) {
            continue;
        }
        match fs::remove_dir_all(&path) {
            Ok(()) => removed += 1,
            Err(error) => eprintln!("{} is not removed: {error}", path.display()),
        }
    }
    Ok(removed)
}

/// The work of the first start after an update: the `*.old` files and the tool folders
/// of an older release. Errors go to the journal and do not stop the others.
pub fn cleanup(install_dir: &Path) -> io::Result<usize> {
    let mut removed = remove_old(install_dir)?;
    #[cfg(target_os = "macos")]
    {
        removed += remove_old_bundle(install_dir)?;
    }
    match remove_stale_tools(install_dir) {
        Ok(stale) => removed += stale,
        Err(error) => eprintln!("the tools of an older release are not removed: {error}"),
    }
    Ok(removed)
}

/// Polls `gone` until it returns true, for at most `timeout`. True when it returned
/// true before the timeout. Waits on the calling thread, so a caller keeps it off the
/// interface.
pub fn wait_until(mut gone: impl FnMut() -> bool, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if gone() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(POLL_INTERVAL);
    }
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

fn extract(archive: &Path, into: &Path) -> Result<Vec<PathBuf>, Error> {
    if is_tar_gz(archive) {
        extract_tar_gz(archive, into)
    } else {
        extract_zip(archive, into)
    }
}

/// Whether the release is a compressed tar rather than a zip.
fn is_tar_gz(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(".tar.gz") || name.ends_with(".tgz"))
}

fn extract_zip(zip: &Path, into: &Path) -> Result<Vec<PathBuf>, Error> {
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
            platform::set_mode(&target, entry.unix_mode().unwrap_or(0o755))?;
            continue;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut out = File::create(&target)?;
        io::copy(&mut entry, &mut out)?;
        if let Some(mode) = entry.unix_mode() {
            platform::set_mode(&target, mode)?;
        }
        files.push(relative);
    }
    Ok(files)
}

const TAR_BLOCK: usize = 512;

/// Unpacks a `.tar.gz` release. The tar reader is small on purpose: the archives the
/// project publishes hold folders and regular files, and the paths are checked here so
/// nothing may land outside `into`.
fn extract_tar_gz(archive: &Path, into: &Path) -> Result<Vec<PathBuf>, Error> {
    let file = File::open(archive)?;
    let mut reader = io::BufReader::new(flate2::read::GzDecoder::new(file));
    let mut files = Vec::new();
    // A long name or a pax header comes in an entry of its own, before the file it
    // describes.
    let mut long_name: Option<String> = None;
    loop {
        let mut header = [0u8; TAR_BLOCK];
        match reader.read_exact(&mut header) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
                return Err(Error::Tar(format!(
                    "{} ends in the middle of a header",
                    archive.display()
                )));
            }
            Err(error) => return Err(error.into()),
        }
        if header.iter().all(|byte| *byte == 0) {
            break;
        }
        let size = octal(&header[124..136])?;
        let mode = octal(&header[100..108])? as u32;
        let kind = header[156];
        let mut name = text_field(&header[0..100]);
        let prefix = text_field(&header[345..500]);
        if !prefix.is_empty() {
            name = format!("{prefix}/{name}");
        }

        match kind {
            b'L' => {
                long_name = Some(text_of(&read_tar_data(&mut reader, size)?));
                continue;
            }
            b'x' => {
                let data = read_tar_data(&mut reader, size)?;
                long_name = pax_path(&data);
                continue;
            }
            b'g' => {
                skip_tar_data(&mut reader, size)?;
                continue;
            }
            _ => {}
        }
        let name = long_name.take().unwrap_or(name);
        let relative = safe_relative(&name)?;
        let target = into.join(&relative);

        if kind == b'5' || name.ends_with('/') {
            fs::create_dir_all(&target)?;
            platform::set_mode(&target, mode)?;
            skip_tar_data(&mut reader, size)?;
            continue;
        }
        if kind != b'0' && kind != 0 && kind != b'7' {
            skip_tar_data(&mut reader, size)?;
            return Err(Error::Tar(format!(
                "{name} is not a file or a folder of the release"
            )));
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut out = File::create(&target)?;
        io::copy(&mut reader.by_ref().take(size), &mut out)?;
        skip_padding(&mut reader, size)?;
        platform::set_mode(&target, mode)?;
        files.push(relative);
    }
    Ok(files)
}

/// The bytes of a header entry and the padding that aligns the next header.
fn read_tar_data(reader: &mut impl Read, size: u64) -> Result<Vec<u8>, Error> {
    let mut data = vec![0u8; usize::try_from(size).map_err(|_| Error::Tar("a huge name".into()))?];
    reader.read_exact(&mut data)?;
    skip_padding(reader, size)?;
    Ok(data)
}

fn skip_tar_data(reader: &mut impl Read, size: u64) -> Result<(), Error> {
    io::copy(&mut reader.by_ref().take(size), &mut io::sink())?;
    skip_padding(reader, size)
}

fn skip_padding(reader: &mut impl Read, size: u64) -> Result<(), Error> {
    let padding = (TAR_BLOCK as u64 - size % TAR_BLOCK as u64) % TAR_BLOCK as u64;
    if padding > 0 {
        io::copy(&mut reader.by_ref().take(padding), &mut io::sink())?;
    }
    Ok(())
}

/// A NUL-terminated text field of a tar header.
fn text_field(field: &[u8]) -> String {
    let end = field
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(field.len());
    String::from_utf8_lossy(&field[..end]).trim().to_owned()
}

fn text_of(data: &[u8]) -> String {
    String::from_utf8_lossy(data)
        .trim_end_matches('\0')
        .to_owned()
}

/// The `path` of a pax extended header, `18 path=some/name\n`.
fn pax_path(data: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(data);
    for record in text.split('\n') {
        let Some((_, field)) = record.split_once(' ') else {
            continue;
        };
        let Some(value) = field.strip_prefix("path=") else {
            continue;
        };
        let value = value.trim_end_matches('\0');
        if !value.is_empty() {
            return Some(value.to_owned());
        }
    }
    None
}

/// An octal number of a tar header. GNU writes a huge size with the high bit of the
/// first byte set; a release does not have such a file.
fn octal(field: &[u8]) -> Result<u64, Error> {
    let text = text_field(field);
    if text.is_empty() {
        return Ok(0);
    }
    u64::from_str_radix(text.trim(), 8)
        .map_err(|_| Error::Tar(format!("{text} is not a number of a tar header")))
}

/// The path of a tar entry, relative to the folder of the unpacking. `..`, a root and
/// a Windows prefix are refused.
fn safe_relative(name: &str) -> Result<PathBuf, Error> {
    let name = name.trim_start_matches("./");
    let path = Path::new(name);
    if path.is_absolute() {
        return Err(Error::Tar(format!("{name} is outside the archive")));
    }
    let mut relative = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::Normal(part) => relative.push(part),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir
            | std::path::Component::RootDir
            | std::path::Component::Prefix(_) => {
                return Err(Error::Tar(format!("{name} is outside the archive")));
            }
        }
    }
    if relative.as_os_str().is_empty() {
        return Err(Error::Tar("the archive has an entry without a name".into()));
    }
    Ok(relative)
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
#[cfg_attr(target_os = "macos", allow(dead_code))]
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
#[cfg_attr(target_os = "macos", allow(dead_code))]
struct Transaction {
    created: Vec<PathBuf>,
    /// The backup and the place it came from.
    restored: Vec<(PathBuf, PathBuf)>,
}

#[cfg_attr(target_os = "macos", allow(dead_code))]
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
#[cfg_attr(target_os = "macos", allow(dead_code))]
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
    Tar(String),
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
            Self::Tar(message) => write!(formatter, "the archive is broken: {message}"),
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
        test_support::scratch(&format!("updater-{name}"))
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
        let archive = archive_name(&version).expect("a release platform of this build");
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
        let archive = archive_name(&version("0.2.0")).expect("a release platform of this build");
        assert_eq!(release.archive.name, archive);
        assert_eq!(release.checksum.name, format!("{archive}.sha256"));
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
        let checksum = format!(
            "{}.sha256",
            archive_name(&version("0.2.0")).expect("a release platform of this build")
        );
        let json = release_json(
            "v0.2.0",
            false,
            &[(&checksum, "https://example.test/archive.zip.sha256")],
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
    }

    #[test]
    fn an_archive_larger_than_a_read_into_memory_downloads_whole() {
        // A real release archive is tens of megabytes; 0.1.0 read it into memory, which
        // stops at 10 MB, and every update failed.
        let dir = temp("large");
        fs::create_dir_all(&dir).unwrap();
        let bytes: Vec<u8> = (0..24 * 1024 * 1024u32)
            .map(|at| (at.wrapping_mul(2_654_435_761) >> 24) as u8)
            .collect();
        let source = dir.join("source.bin");
        fs::write(&source, &bytes).unwrap();
        let hash = sha256_file(&source).unwrap();
        let server = Server::start(vec![
            ("/archive.zip".into(), Reply::bytes(bytes.clone())),
            (
                "/archive.zip.sha256".into(),
                Reply::text(format!("{hash}  arro.zip\n")),
            ),
        ]);
        let release = release_from("v0.2.0", &server);
        let file = server_source(&server)
            .download(&release, &dir.join("download"))
            .unwrap();
        assert_eq!(fs::metadata(&file).unwrap().len(), bytes.len() as u64);
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
    }

    #[test]
    fn old_files_are_removed_on_the_next_start() {
        let dir = temp("cleanup");
        fs::write(dir.join("ide.exe.old"), "old").unwrap();
        fs::write(dir.join("keep.txt"), "keep").unwrap();
        assert_eq!(remove_old(&dir).unwrap(), 1);
        assert!(!dir.join("ide.exe.old").exists());
        assert!(dir.join("keep.txt").exists());
    }
}

#[cfg(test)]
mod platform_tests;
#[cfg(test)]
mod release_tests;
#[cfg(test)]
mod update_tests;
