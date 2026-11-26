//! The addresses an update may come from: https on GitHub, the redirects of its
//! release files, and the loopback server the tests run.

use super::test_server::{Answer, TestServer};
use super::*;

fn version(text: &str) -> Version {
    Version::parse(text).unwrap()
}

fn temp(name: &str) -> PathBuf {
    test_support::scratch(&format!("updater-url-{name}"))
}

fn release(archive_url: &str, checksum_url: &str) -> Release {
    Release {
        version: version("0.2.0"),
        tag: "v0.2.0".into(),
        notes: String::new(),
        page_url: "https://github.com/PabloSensei/arro/releases/tag/v0.2.0".into(),
        archive: Asset {
            name: "arro-0.2.0-windows-x86_64.zip".into(),
            url: archive_url.into(),
        },
        checksum: Asset {
            name: "arro-0.2.0-windows-x86_64.zip.sha256".into(),
            url: checksum_url.into(),
        },
        signature: None,
    }
}

fn checksum_answer() -> Answer {
    Answer::text(format!("{}  arro.zip\n", "0".repeat(64)))
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

#[test]
fn https_addresses_of_github_are_allowed() {
    for url in [
        "https://github.com/PabloSensei/arro/releases/download/v0.2.0/arro.zip",
        "https://api.github.com/repos/PabloSensei/arro/releases/latest",
        "https://objects.githubusercontent.com/x/y",
        "https://release-assets.githubusercontent.com/x/y",
        "HTTPS://GITHUB.COM/PabloSensei/arro",
    ] {
        assert!(update_url_allowed(url), "{url}");
    }
}

#[test]
fn an_http_address_is_refused() {
    for url in [
        "http://github.com/PabloSensei/arro/releases/download/v0.2.0/arro.zip",
        "http://api.github.com/repos/PabloSensei/arro/releases/latest",
    ] {
        assert!(!update_url_allowed(url), "{url}");
    }
}

#[test]
fn an_address_of_another_host_is_refused() {
    for url in [
        "https://evil.example/arro.zip",
        "https://github.com.evil.example/arro.zip",
        "https://evil.example/github.com/arro.zip",
        "https://user@evil.example/arro.zip",
        "https://github%2ecom/arro.zip",
        "ftp://github.com/arro.zip",
        "https:///arro.zip",
    ] {
        assert!(!update_url_allowed(url), "{url}");
    }
}

#[test]
fn a_download_from_an_http_address_of_github_does_not_touch_the_disk() {
    let dir = temp("http-archive");
    let server = TestServer::start(|_| {
        vec![(
            "/archive.zip.sha256".into(),
            Answer::text(format!("{}  arro.zip\n", "0".repeat(64))),
        )]
    });
    let release = release(
        "http://github.com/PabloSensei/arro/releases/download/v0.2.0/arro.zip",
        &server.url("/archive.zip.sha256"),
    );
    let error = UpdateSource::with_url(server.url("/latest"))
        .download(&release, &dir)
        .unwrap_err();
    assert!(matches!(error, Error::InsecureUrl(_)), "{error}");
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
}

#[test]
fn a_checksum_from_another_host_is_refused_before_anything_is_written() {
    let dir = temp("evil-checksum");
    let error = UpdateSource::with_url("https://api.github.com/repos/x/y/releases/latest")
        .download(
            &release(
                "https://github.com/PabloSensei/arro/releases/download/v0.2.0/arro.zip",
                "https://evil.example/arro.zip.sha256",
            ),
            &dir,
        )
        .unwrap_err();
    assert!(matches!(error, Error::InsecureUrl(_)), "{error}");
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
}

#[test]
fn a_redirect_to_another_host_is_an_error() {
    let dir = temp("redirect-evil");
    let server = TestServer::start(|_| {
        vec![
            (
                "/archive.zip".into(),
                Answer::Redirect("https://evil.example/arro.zip".into()),
            ),
            ("/archive.zip.sha256".into(), checksum_answer()),
        ]
    });
    let error = UpdateSource::with_url(server.url("/latest"))
        .download(
            &release(
                &server.url("/archive.zip"),
                &server.url("/archive.zip.sha256"),
            ),
            &dir,
        )
        .unwrap_err();
    assert!(matches!(error, Error::InsecureUrl(_)), "{error}");
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
}

#[test]
fn a_redirect_to_an_allowed_address_is_followed() {
    let dir = temp("redirect-good");
    let zip = dir.join("source.zip");
    write_zip(&zip, &[("a.txt", "a")]);
    let bytes = fs::read(&zip).unwrap();
    let hash = sha256_file(&zip).unwrap();
    let server = TestServer::start(move |address| {
        vec![
            (
                "/start.zip".into(),
                Answer::Redirect(format!("http://{address}/archive.zip")),
            ),
            ("/archive.zip".into(), Answer::Body(bytes.clone())),
            (
                "/start.zip.sha256".into(),
                Answer::text(format!("{hash}  arro.zip\n")),
            ),
        ]
    });
    let downloaded = UpdateSource::with_url(server.url("/latest"))
        .download(
            &release(&server.url("/start.zip"), &server.url("/start.zip.sha256")),
            &dir.join("download"),
        )
        .unwrap();
    assert!(downloaded.is_file());
}

#[cfg(debug_assertions)]
#[test]
fn the_update_url_variable_is_taken_in_a_debug_build() {
    // SAFETY: no other test of this crate reads the variable, and the two tests that
    // change it are compiled one at a time.
    unsafe {
        std::env::remove_var(ALLOW_UPDATE_URL_ENV);
        std::env::set_var(
            UPDATE_URL_ENV,
            "https://api.github.com/repos/PabloSensei/arro/releases/latest",
        );
    }
    let source = UpdateSource::new();
    unsafe {
        std::env::remove_var(UPDATE_URL_ENV);
    }
    assert_eq!(
        source.url(),
        "https://api.github.com/repos/PabloSensei/arro/releases/latest"
    );
}

#[cfg(not(debug_assertions))]
#[test]
fn the_update_url_variable_is_ignored_in_a_release_build() {
    // SAFETY: no other test of this crate reads the variable, and the two tests that
    // change it are compiled one at a time.
    unsafe {
        std::env::remove_var(ALLOW_UPDATE_URL_ENV);
        std::env::set_var(UPDATE_URL_ENV, "https://evil.example/latest");
    }
    let source = UpdateSource::new();
    unsafe {
        std::env::remove_var(UPDATE_URL_ENV);
    }
    assert_eq!(source.url(), RELEASES_URL);
}
