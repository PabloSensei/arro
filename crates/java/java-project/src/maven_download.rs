//! The bundled copy of Maven: the newest stable version of the official archive, its
//! SHA-512 from the neighbouring file, and the unpacked home under the data folder of
//! Arro. The transfer, the checksum and the unpacking live in [`crate::tool_download`],
//! which the other build tool uses too.
//!
//! Only the central repository of the Java ecosystem is asked; no mirror is used.

use std::path::{Path, PathBuf};

use base::Cancel;
use base::program::cmd_script;
use project_model::ToolProgress;

use crate::tool_download::{
    Archive, Checksum, DownloadError, Release, checksum_url, fetch, safe_name,
};

/// The official repository of Apache Maven. Mirrors are not asked: a copy from anywhere
/// else could carry anything.
pub(crate) const REPOSITORY: &str = "https://repo.maven.apache.org/maven2";
const GROUP_PATH: &str = "org/apache/maven";
const ARTIFACT: &str = "apache-maven";
/// The folder of the downloaded copies under the data folder of the IDE.
const MAVEN: &str = "maven";

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
    versions.sort_by(|left, right| crate::tool_download::compare_versions(right, left));
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

fn install_from(
    repository: &str,
    data_dir: &Path,
    version: &str,
    archive: Archive,
    progress: &ToolProgress,
    cancel: &Cancel,
) -> Result<PathBuf, DownloadError> {
    let version = safe_name(version);
    let top = format!("{ARTIFACT}-{version}");
    let file = format!("{top}-bin.{}", archive.extension());
    let url = format!("{repository}/{GROUP_PATH}/{ARTIFACT}/{version}/{file}");
    Release {
        tool: MAVEN,
        version,
        top,
        checksum: checksum_url(&url, Checksum::Sha512),
        url,
        algorithm: Checksum::Sha512,
        archive,
        program: cmd_script("mvn"),
    }
    .install(data_dir, progress, cancel)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use base::Cancel;
    use project_model::ToolProgress;

    use super::{
        Archive, DownloadError, install_from, is_stable_three, latest_stable, stable_versions,
    };
    use crate::tool_download::testing::{Server, sha512, tar_gz_archive, zip_archive};

    fn metadata(versions: &[&str]) -> String {
        let mut text = String::from("<metadata><versioning><versions>");
        for version in versions {
            text.push_str(&format!("<version>{version}</version>"));
        }
        text.push_str("</versions></versioning></metadata>");
        text
    }

    fn program() -> &'static str {
        if cfg!(windows) { "mvn.cmd" } else { "mvn" }
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

    fn archive_files() -> Vec<(String, Vec<u8>)> {
        vec![
            (
                format!("bin/{}", program()),
                b"#!/bin/sh\necho maven\n".to_vec(),
            ),
            ("lib/A.txt".to_owned(), b"library".to_vec()),
        ]
    }

    fn archive_bytes(top: &str, archive: Archive) -> Vec<u8> {
        let files = archive_files();
        let files: Vec<(&str, &[u8])> = files
            .iter()
            .map(|(name, text)| (name.as_str(), text.as_slice()))
            .collect();
        match archive {
            Archive::Zip => zip_archive(top, &files),
            Archive::TarGz => tar_gz_archive(top, &files),
        }
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
        let bytes = archive_bytes("apache-maven-3.9.11", Archive::Zip);
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
        let bytes = archive_bytes("apache-maven-3.9.11", Archive::TarGz);
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
        let bytes = archive_bytes("apache-maven-3.9.11", Archive::Zip);
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
