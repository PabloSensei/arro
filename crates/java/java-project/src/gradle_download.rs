//! The bundled copy of Gradle: the current stable version of the official site, its
//! SHA-256 from the neighbouring file, and the unpacked home under the data folder of
//! Arro. The transfer, the checksum and the unpacking live in
//! [`crate::tool_download`], which the other build tool uses too.
//!
//! Only `services.gradle.org` is asked; no mirror is used.

use std::path::{Path, PathBuf};

use base::Cancel;
use base::program::bat_script;
use project_model::ToolProgress;
use serde::Deserialize;

use crate::tool_download::{
    Archive, Checksum, DownloadError, Release, checksum_url, fetch, safe_name,
};

/// The official site of Gradle. The list of the versions and the distributions are both
/// under it.
pub(crate) const REPOSITORY: &str = "https://services.gradle.org";
const CURRENT: &str = "/versions/current";
const DISTRIBUTIONS: &str = "/distributions";
/// The folder of the downloaded copies under the data folder of the IDE.
const GRADLE: &str = "gradle";

/// The answer of `versions/current`: the version of the release the site offers.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Current {
    version: String,
    #[serde(default)]
    snapshot: bool,
    #[serde(default)]
    nightly: bool,
    #[serde(default)]
    active_rc: bool,
    #[serde(default)]
    broken: bool,
    #[serde(default)]
    rc_for: String,
    #[serde(default)]
    milestone_for: String,
}

/// The current stable version of Gradle the site lists, without snapshots, nightly
/// builds, release candidates and milestones.
pub(crate) fn latest_stable(repository: &str) -> Result<String, DownloadError> {
    let versions = fetch(&format!("{repository}{CURRENT}"))?;
    parse_current(&versions)
}

/// The version of a `versions/current` answer, when the answer names a stable release.
fn parse_current(answer: &[u8]) -> Result<String, DownloadError> {
    let current: Current = serde_json::from_slice(answer).map_err(|error| {
        DownloadError::Offline(format!("the list of versions is not JSON: {error}"))
    })?;
    let stable = !current.snapshot
        && !current.nightly
        && !current.active_rc
        && !current.broken
        && current.rc_for.trim().is_empty()
        && current.milestone_for.trim().is_empty()
        && !current.version.trim().is_empty()
        && !current.version.contains('-');
    if !stable {
        return Err(DownloadError::NoStableVersion);
    }
    Ok(current.version.trim().to_owned())
}

/// Downloads the named version, or the current stable one, and unpacks it into
/// `<data>/tools/gradle/<version>`. A copy of an earlier download is returned without a
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
        None => match crate::gradle::bundled_home(data_dir) {
            Some(home) => return Ok(home),
            None => latest_stable(repository)?,
        },
    };
    install_from(repository, data_dir, &version, progress, cancel)
}

fn install_from(
    repository: &str,
    data_dir: &Path,
    version: &str,
    progress: &ToolProgress,
    cancel: &Cancel,
) -> Result<PathBuf, DownloadError> {
    let version = safe_name(version);
    let top = format!("gradle-{version}");
    let file = format!("{top}-bin.zip");
    let url = format!("{repository}{DISTRIBUTIONS}/{file}");
    Release {
        tool: GRADLE,
        version,
        top,
        checksum: checksum_url(&url, Checksum::Sha256),
        url,
        algorithm: Checksum::Sha256,
        archive: Archive::Zip,
        program: bat_script("gradle"),
    }
    .install(data_dir, progress, cancel)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use base::Cancel;
    use project_model::ToolProgress;

    use super::{download, install_from, latest_stable, parse_current};
    use crate::tool_download::DownloadError;
    use crate::tool_download::testing::{Server, sha256, zip_archive};

    const VERSION: &str = "9.1.0";

    fn current(version: &str, extra: &str) -> Vec<u8> {
        format!("{{\"version\":\"{version}\",\"buildTime\":\"2025-10-01 00:00:00 UTC\"{extra}}}")
            .into_bytes()
    }

    fn program() -> String {
        if cfg!(windows) {
            "gradle.bat".to_owned()
        } else {
            "gradle".to_owned()
        }
    }

    fn archive_files() -> Vec<(String, Vec<u8>)> {
        vec![
            (
                format!("bin/{}", program()),
                b"#!/bin/sh\necho gradle\n".to_vec(),
            ),
            (
                "lib/gradle-launcher-9.1.0.jar".to_owned(),
                b"library".to_vec(),
            ),
        ]
    }

    fn archive_bytes(top: &str) -> Vec<u8> {
        let files = archive_files();
        let files: Vec<(&str, &[u8])> = files
            .iter()
            .map(|(name, text)| (name.as_str(), text.as_slice()))
            .collect();
        zip_archive(top, &files)
    }

    fn archive_path(version: &str) -> String {
        format!("/distributions/gradle-{version}-bin.zip")
    }

    fn download_version(
        server: &Server,
        data: &Path,
        version: &str,
    ) -> Result<PathBuf, DownloadError> {
        install_from(
            &server.base,
            data,
            version,
            &ToolProgress::new(),
            &Cancel::new(),
        )
    }

    #[test]
    fn the_current_version_of_the_site_is_read() {
        let answer = current(VERSION, ",\"snapshot\":false,\"activeRc\":false");

        assert_eq!(parse_current(&answer).ok().as_deref(), Some(VERSION));
        assert!(matches!(
            parse_current(&current(
                "9.2.0-rc-1",
                ",\"activeRc\":true,\"rcFor\":\"9.2.0\""
            )),
            Err(DownloadError::NoStableVersion)
        ));
        assert!(matches!(
            parse_current(&current("9.2.0-rc-1", ",\"rcFor\":\"9.2.0\"")),
            Err(DownloadError::NoStableVersion)
        ));
        assert!(matches!(
            parse_current(&current("9.2.0", ",\"snapshot\":true")),
            Err(DownloadError::NoStableVersion)
        ));
        assert!(matches!(
            parse_current(&current(VERSION, ",\"nightly\":true")),
            Err(DownloadError::NoStableVersion)
        ));
        assert!(matches!(
            parse_current(b"not JSON"),
            Err(DownloadError::Offline(_))
        ));
    }

    #[test]
    fn the_current_version_of_the_server_is_asked() {
        let server = Server::start(vec![(super::CURRENT.to_owned(), current(VERSION, ""))]);

        let version = latest_stable(&server.base).expect("the list of versions is read");

        assert_eq!(version, VERSION);
    }

    #[test]
    fn a_zip_archive_becomes_the_bundled_home() {
        let data = test_support::TempDir::new("gradle-zip");
        let bytes = archive_bytes(&format!("gradle-{VERSION}"));
        let path = archive_path(VERSION);
        let server = Server::start(vec![
            (path.clone(), bytes.clone()),
            (
                format!("{path}.sha256"),
                format!("{}  gradle-{VERSION}-bin.zip\n", sha256(&bytes)).into_bytes(),
            ),
        ]);

        let home =
            download_version(&server, data.path(), VERSION).expect("the archive is unpacked");

        assert_eq!(home, data.path().join(format!("tools/gradle/{VERSION}")));
        assert_eq!(
            fs::read_to_string(home.join("bin").join(program())).unwrap(),
            "#!/bin/sh\necho gradle\n"
        );
        assert_eq!(
            fs::read_to_string(home.join("lib/gradle-launcher-9.1.0.jar")).unwrap(),
            "library"
        );
        assert!(
            !data
                .path()
                .join(format!(
                    "tools/gradle/.download/gradle-{VERSION}-bin.zip.part"
                ))
                .exists()
        );
    }

    #[test]
    fn a_bad_sha256_removes_the_archive() {
        let data = test_support::TempDir::new("gradle-checksum");
        let bytes = b"not the promised archive".to_vec();
        let path = archive_path(VERSION);
        let server = Server::start(vec![
            (path.clone(), bytes),
            (
                format!("{path}.sha256"),
                b"0000000000000000000000000000000000000000000000000000000000000000\n".to_vec(),
            ),
        ]);

        let error = download_version(&server, data.path(), VERSION)
            .expect_err("the checksum does not match");

        assert!(matches!(error, DownloadError::Checksum { .. }), "{error}");
        assert!(!data.path().join(format!("tools/gradle/{VERSION}")).exists());
        let work = data.path().join("tools/gradle/.download");
        let left: Vec<_> = fs::read_dir(&work)
            .map(|entries| entries.flatten().map(|entry| entry.path()).collect())
            .unwrap_or_default();
        assert!(left.is_empty(), "{left:?}");
    }

    #[test]
    fn an_earlier_download_is_not_requested_again() {
        let data = test_support::TempDir::new("gradle-again");
        let bytes = archive_bytes(&format!("gradle-{VERSION}"));
        let path = archive_path(VERSION);
        let server = Server::start(vec![
            (path.clone(), bytes.clone()),
            (
                format!("{path}.sha256"),
                format!("{}\n", sha256(&bytes)).into_bytes(),
            ),
        ]);
        let first = download_version(&server, data.path(), VERSION).unwrap();
        let requests = server.requests();

        let second = download_version(&server, data.path(), VERSION).unwrap();

        assert_eq!(first, second);
        assert_eq!(
            server.requests(),
            requests,
            "a home on the disk needs no request"
        );
    }

    #[test]
    fn the_newest_downloaded_copy_is_taken_without_a_request() {
        let data = test_support::TempDir::new("gradle-download-current");
        let home = data.path().join(format!("tools/gradle/{VERSION}"));
        fs::create_dir_all(home.join("bin")).unwrap();
        fs::write(home.join("bin").join(program()), b"").unwrap();
        let server = Server::start(Vec::new());

        assert_eq!(
            download(
                &server.base,
                data.path(),
                None,
                &ToolProgress::new(),
                &Cancel::new()
            )
            .ok()
            .as_deref(),
            Some(home.as_path())
        );
        assert_eq!(server.requests(), 0, "no request for a copy that is there");
    }
}
