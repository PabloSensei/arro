//! The security and reliability of the downloads of the tool: an insecure address is not
//! requested, a body has no total limit, an interrupted folder is replaced, and two
//! windows install one tool without sharing a folder. The JDK of a project is read only
//! from a local path, relative ones from the root of the project.

use std::cell::Cell;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use base::Cancel;
use base::program::executable;
use project_model::ToolProgress;

use crate::gradle_wrapper::{self, WrapperDistribution};
use crate::tool_download::testing::{Server, sha256, zip_archive};
use crate::tool_download::{Archive, Checksum, DownloadError, Release, Timeouts, stream_to_with};

fn wrapper_archive() -> Vec<u8> {
    zip_archive(
        "gradle-9.5.1",
        &[
            ("bin/gradle", b"#!/bin/sh\n"),
            ("bin/gradle.bat", b"@echo off\n"),
            ("lib/gradle-launcher.jar", b"library"),
        ],
    )
}

fn wrapper_properties(root: &Path, url: &str) {
    let folder = root.join("gradle/wrapper");
    fs::create_dir_all(&folder).unwrap();
    fs::write(
        folder.join("gradle-wrapper.properties"),
        format!("distributionUrl={url}\n"),
    )
    .unwrap();
}

#[test]
fn an_http_address_is_named_insecure_and_is_never_requested() {
    let root = test_support::TempDir::new("wrapper-insecure");
    let path = "/distributions/gradle-9.5.1-bin.zip";
    let url = format!("http://192.0.2.1{path}");
    wrapper_properties(root.path(), &url);

    let distribution = WrapperDistribution::read(root.path()).expect("the file names a URL");
    let user_home = test_support::TempDir::new("wrapper-insecure-home");

    assert!(!distribution.is_secure(), "the address is http");
    let secure = distribution.with_https().expect("the same host over https");
    assert!(secure.is_secure());
    assert_eq!(
        secure.url(),
        format!("https://192.0.2.1{path}"),
        "the same host and path over https"
    );
    assert_eq!(secure.sha256(), distribution.sha256());
    assert_eq!(
        secure.folder(user_home.path()),
        distribution.folder(user_home.path()),
        "the wrapper reads the folder its own file names"
    );

    let error = gradle_wrapper::install(
        &distribution,
        user_home.path(),
        &ToolProgress::new(),
        &Cancel::new(),
    )
    .expect_err("an http address is not downloaded");

    assert!(matches!(error, DownloadError::Insecure), "{error}");
    assert!(!distribution.installed(user_home.path()));
    assert!(
        !user_home.path().join("wrapper").exists(),
        "no folder is made for an address that is not requested"
    );
}

#[test]
fn a_slow_body_passes_when_no_read_waits_longer_than_the_limit() {
    let path = "/tool-1.0-bin.zip";
    let bytes = vec![b'x'; 4096];
    let server = Server::start_slow(
        vec![(path.to_owned(), bytes.clone())],
        512,
        Duration::from_millis(80),
    );
    let target = test_support::TempDir::new("download-slow");
    let file = target.path().join("tool.zip");
    let timeouts = Timeouts {
        connect: Duration::from_secs(2),
        response: Duration::from_secs(2),
        read: Duration::from_millis(400),
    };

    stream_to_with(
        &format!("{}{path}", server.base),
        &file,
        &ToolProgress::new(),
        &Cancel::new(),
        timeouts,
    )
    .expect("the body arrives in pieces, and no read waits for the whole one");

    assert_eq!(fs::read(&file).unwrap(), bytes);
}

#[test]
fn a_server_that_stays_silent_longer_than_the_limit_fails() {
    let path = "/tool-1.0-bin.zip";
    let server = Server::start_slow(
        vec![(path.to_owned(), vec![b'x'; 2048])],
        1024,
        Duration::from_millis(800),
    );
    let target = test_support::TempDir::new("download-silent");
    let file = target.path().join("tool.zip");
    let timeouts = Timeouts {
        connect: Duration::from_secs(2),
        response: Duration::from_secs(2),
        read: Duration::from_millis(250),
    };

    stream_to_with(
        &format!("{}{path}", server.base),
        &file,
        &ToolProgress::new(),
        &Cancel::new(),
        timeouts,
    )
    .expect_err("a silent body fails");
}

#[test]
fn a_wrapper_folder_without_its_marker_is_replaced() {
    let user_home = test_support::TempDir::new("wrapper-stale-home");
    let path = "/distributions/gradle-9.5.1-bin.zip";
    let bytes = wrapper_archive();
    let server = Server::start(vec![(path.to_owned(), bytes.clone())]);
    let distribution = gradle_wrapper::testing_distribution(
        &format!("{}{path}", server.base),
        Some(sha256(&bytes).as_str()),
    );
    let folder = distribution.folder(user_home.path()).expect("a folder");
    fs::create_dir_all(&folder).unwrap();
    fs::write(folder.join("half-written"), b"junk").unwrap();

    let installed = gradle_wrapper::install(
        &distribution,
        user_home.path(),
        &ToolProgress::new(),
        &Cancel::new(),
    )
    .expect("the stale folder is replaced by the distribution");

    assert_eq!(installed, folder);
    assert!(gradle_wrapper::is_installed(&folder));
    assert!(
        !folder.join("half-written").exists(),
        "the stale file is gone"
    );
    assert_eq!(server.requests(), 1, "the distribution is downloaded again");
}

#[test]
fn a_failed_install_leaves_no_folder_of_the_wrapper() {
    let user_home = test_support::TempDir::new("wrapper-failed-home");
    let path = "/distributions/gradle-9.5.1-bin.zip";
    let server = Server::start(vec![(path.to_owned(), b"not an archive".to_vec())]);
    let distribution =
        gradle_wrapper::testing_distribution(&format!("{}{path}", server.base), None);

    gradle_wrapper::install(
        &distribution,
        user_home.path(),
        &ToolProgress::new(),
        &Cancel::new(),
    )
    .expect_err("the body is not an archive");

    let folder = distribution.folder(user_home.path()).expect("a folder");
    assert!(!folder.exists(), "{}", folder.display());
    assert!(!gradle_wrapper::is_installed(&folder));
}

#[test]
fn two_threads_install_one_tool_into_one_complete_home() {
    let path = "/tool-1.0-bin.zip";
    let bytes = zip_archive(
        "tool-1.0",
        &[("bin/tool", b"program"), ("lib/tool.jar", b"library")],
    );
    let checksum = sha256(&bytes);
    let server = Server::start(vec![
        (path.to_owned(), bytes),
        (format!("{path}.sha256"), checksum.into_bytes()),
    ]);
    let data = test_support::TempDir::new("tool-two-threads");
    let release = Release {
        tool: "tool",
        version: "1.0".to_owned(),
        top: "tool-1.0".to_owned(),
        url: format!("{}{path}", server.base),
        checksum: format!("{}{path}.sha256", server.base),
        algorithm: Checksum::Sha256,
        archive: Archive::Zip,
        program: "tool".to_owned(),
    };

    let homes = std::thread::scope(|scope| {
        let first =
            scope.spawn(|| release.install(data.path(), &ToolProgress::new(), &Cancel::new()));
        let second =
            scope.spawn(|| release.install(data.path(), &ToolProgress::new(), &Cancel::new()));
        [
            first.join().expect("the first thread"),
            second.join().expect("the second thread"),
        ]
    });

    let first = homes[0].as_ref().expect("the first install");
    let second = homes[1].as_ref().expect("the second install");
    assert_eq!(first, second, "both threads name one home");
    assert_eq!(*first, data.path().join("tools/tool/1.0"));
    assert!(first.join("bin/tool").is_file());
    assert!(
        fs::read_dir(first).unwrap().flatten().count() >= 2,
        "the home keeps the program and the library"
    );
}

fn jdk(root: &Path, name: &str, version: &str) -> PathBuf {
    let home = root.join(name);
    fs::create_dir_all(home.join("bin")).unwrap();
    fs::write(home.join("bin").join(executable("java")), "").unwrap();
    fs::write(home.join("bin").join(executable("javac")), "").unwrap();
    fs::write(
        home.join("release"),
        format!("JAVA_VERSION=\"{version}\"\n"),
    )
    .unwrap();
    home
}

#[test]
fn a_relative_sdk_of_the_project_counts_from_its_root() {
    let root = test_support::TempDir::new("jdk-relative-root");
    let home = jdk(root.path(), "jdk-17", "17.0.2");
    crate::save_settings(
        root.path(),
        &crate::ProjectSettings {
            selected: Some(PathBuf::from("jdk-17")),
            added: Vec::new(),
        },
    )
    .unwrap();

    let resolved = crate::resolve_with(root.path(), &[], None);

    assert_eq!(resolved.selected.as_ref().map(|jdk| &jdk.home), Some(&home));
    assert_eq!(resolved.reason, project_model::RuntimeReason::User);
}

#[test]
fn a_relative_gradle_java_home_counts_from_the_project_root() {
    let root = test_support::TempDir::new("jdk-gradle-home-root");
    let home = jdk(root.path(), "jdk-21", "21.0.2");
    fs::write(
        root.path().join("gradle.properties"),
        "org.gradle.java.home=jdk-21\n",
    )
    .unwrap();

    let resolved = crate::resolve_with(root.path(), &[], None);

    assert_eq!(resolved.selected.as_ref().map(|jdk| &jdk.home), Some(&home));
    assert_eq!(resolved.reason, project_model::RuntimeReason::Project);
}

#[test]
fn a_network_sdk_is_not_checked_at_all() {
    let root = test_support::TempDir::new("jdk-network-root");
    let local = jdk(root.path(), "jdk-17", "17.0.2");
    crate::save_settings(
        root.path(),
        &crate::ProjectSettings {
            selected: Some(PathBuf::from(r"\\server\share\jdk")),
            added: Vec::new(),
        },
    )
    .unwrap();

    let checked = Cell::new(false);
    assert!(
        crate::Jdk::at_with(Path::new(r"\\server\share\jdk"), |_| {
            checked.set(true);
            true
        })
        .is_none(),
        "a share of another machine is not a JDK"
    );
    assert!(!checked.get(), "no file of the share is checked");

    let resolved = crate::resolve_with(
        root.path(),
        &[crate::Jdk::at(&local).expect("the local JDK")],
        None,
    );
    assert_eq!(
        resolved.selected.as_ref().map(|jdk| &jdk.home),
        Some(&local)
    );
    let notice = resolved.notice.unwrap_or_default();
    assert!(notice.contains("Network paths are not used"), "{notice}");
}
