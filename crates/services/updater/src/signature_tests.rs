//! The Ed25519 signature of the `.sha256` file: a release signed by the key of the
//! updater installs, and a release with a missing or a wrong signature does not.

use super::test_server::{Answer, TestServer};
use super::*;
use ring::signature::KeyPair;

fn version(text: &str) -> Version {
    Version::parse(text).unwrap()
}

fn temp(name: &str) -> PathBuf {
    test_support::scratch(&format!("updater-signature-{name}"))
}

/// A key the test makes up, and the same key as the 64 hex digits a release carries.
struct Key {
    pair: ring::signature::Ed25519KeyPair,
    hex: String,
}

impl Key {
    fn new(seed: u8) -> Self {
        let pair = ring::signature::Ed25519KeyPair::from_seed_unchecked(&[seed; 32]).unwrap();
        let hex = pair
            .public_key()
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        Self { pair, hex }
    }

    fn sign(&self, message: &[u8]) -> Vec<u8> {
        self.pair.sign(message).as_ref().to_vec()
    }
}

fn release(archive_url: &str, checksum_url: &str, signature_url: Option<&str>) -> Release {
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
        signature: signature_url.map(|url| Asset {
            name: "arro-0.2.0-windows-x86_64.zip.sha256.sig".into(),
            url: url.into(),
        }),
    }
}

fn write_zip(path: &Path) -> Vec<u8> {
    let file = File::create(path).unwrap();
    let mut writer = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default();
    writer.start_file("a.txt", options).unwrap();
    writer.write_all(b"a").unwrap();
    writer.finish().unwrap();
    fs::read(path).unwrap()
}

#[test]
fn a_signature_of_the_key_is_accepted() {
    let key = Key::new(1);
    let message = b"0123456789abcdef  arro.zip\n";
    assert!(signature::verify_with(
        &key.hex,
        message,
        &key.sign(message)
    ));
}

#[test]
fn a_signature_of_another_message_is_refused() {
    let key = Key::new(1);
    let signature = key.sign(b"one checksum\n");
    assert!(!signature::verify_with(
        &key.hex,
        b"another checksum\n",
        &signature
    ));
}

#[test]
fn a_signature_of_another_key_is_refused() {
    let signer = Key::new(1);
    let other = Key::new(2);
    let message = b"0123456789abcdef  arro.zip\n";
    assert!(!signature::verify_with(
        &other.hex,
        message,
        &signer.sign(message)
    ));
}

#[test]
fn a_key_that_is_not_the_64_hex_digits_of_a_key_is_refused() {
    let key = Key::new(1);
    let message = b"0123456789abcdef  arro.zip\n";
    let signature = key.sign(message);
    for bad in ["", "0123", &key.hex[2..], &format!("{}zz", &key.hex[..62])] {
        assert!(!signature::verify_with(bad, message, &signature), "{bad}");
    }
}

#[test]
fn a_release_signed_by_the_key_downloads() {
    let dir = temp("good");
    let zip = dir.join("source.zip");
    let bytes = write_zip(&zip);
    let hash = sha256_file(&zip).unwrap();
    let checksum = format!("{hash}  arro.zip\n");
    let key = Key::new(1);
    let signature = key.sign(checksum.as_bytes());
    let server = TestServer::start(move |_| {
        vec![
            ("/archive.zip".into(), Answer::Body(bytes.clone())),
            ("/archive.zip.sha256".into(), Answer::text(checksum.clone())),
            (
                "/archive.zip.sha256.sig".into(),
                Answer::Body(signature.clone()),
            ),
        ]
    });
    let release = release(
        &server.url("/archive.zip"),
        &server.url("/archive.zip.sha256"),
        Some(&server.url("/archive.zip.sha256.sig")),
    );
    let downloaded = UpdateSource::with_url(server.url("/latest"))
        .download_signed(&release, &dir.join("download"), &key.hex)
        .unwrap();
    assert!(downloaded.is_file());
}

#[test]
fn a_release_with_a_wrong_signature_is_refused() {
    let dir = temp("wrong");
    let zip = dir.join("source.zip");
    let bytes = write_zip(&zip);
    let hash = sha256_file(&zip).unwrap();
    let checksum = format!("{hash}  arro.zip\n");
    let signer = Key::new(1);
    let other = Key::new(2);
    let signature = other.sign(checksum.as_bytes());
    let server = TestServer::start(move |_| {
        vec![
            ("/archive.zip".into(), Answer::Body(bytes.clone())),
            ("/archive.zip.sha256".into(), Answer::text(checksum.clone())),
            (
                "/archive.zip.sha256.sig".into(),
                Answer::Body(signature.clone()),
            ),
        ]
    });
    let release = release(
        &server.url("/archive.zip"),
        &server.url("/archive.zip.sha256"),
        Some(&server.url("/archive.zip.sha256.sig")),
    );
    // The checksum and the signature do not match the key of the build, so the archive
    // is never asked for.
    let error = UpdateSource::with_url(server.url("/latest"))
        .download_signed(&release, &dir.join("download"), &signer.hex)
        .unwrap_err();
    assert!(matches!(error, Error::Unsigned), "{error}");
    assert_eq!(error.to_string(), "The update is not signed by Arro");
    assert_eq!(fs::read_dir(dir.join("download")).unwrap().count(), 0);
}

#[test]
fn a_release_without_a_signature_is_refused_when_a_key_is_set() {
    let dir = temp("missing");
    let zip = dir.join("source.zip");
    let bytes = write_zip(&zip);
    let hash = sha256_file(&zip).unwrap();
    let checksum = format!("{hash}  arro.zip\n");
    let key = Key::new(1);
    let server = TestServer::start(move |_| {
        vec![
            ("/archive.zip".into(), Answer::Body(bytes.clone())),
            ("/archive.zip.sha256".into(), Answer::text(checksum.clone())),
        ]
    });
    let release = release(
        &server.url("/archive.zip"),
        &server.url("/archive.zip.sha256"),
        None,
    );
    let error = UpdateSource::with_url(server.url("/latest"))
        .download_signed(&release, &dir.join("download"), &key.hex)
        .unwrap_err();
    assert!(matches!(error, Error::Unsigned), "{error}");
    assert_eq!(fs::read_dir(dir.join("download")).unwrap().count(), 0);
}

#[test]
fn an_empty_key_trusts_the_hash_alone() {
    let dir = temp("empty-key");
    let zip = dir.join("source.zip");
    let bytes = write_zip(&zip);
    let hash = sha256_file(&zip).unwrap();
    let server = TestServer::start(move |_| {
        vec![
            ("/archive.zip".into(), Answer::Body(bytes.clone())),
            (
                "/archive.zip.sha256".into(),
                Answer::text(format!("{hash}  arro.zip\n")),
            ),
        ]
    });
    let release = release(
        &server.url("/archive.zip"),
        &server.url("/archive.zip.sha256"),
        None,
    );
    let downloaded = UpdateSource::with_url(server.url("/latest"))
        .download_signed(&release, &dir.join("download"), "")
        .unwrap();
    assert!(downloaded.is_file());
}

#[test]
fn the_signature_of_the_release_is_the_checksum_with_sig_after_it() {
    let (archive, checksum) = {
        let version = version("0.2.0");
        let archive = archive_name(&version).expect("a release platform of this build");
        let checksum = format!("{archive}.sha256");
        (archive, checksum)
    };
    let json = format!(
        r#"{{"tag_name":"v0.2.0","body":"","html_url":"","prerelease":false,"draft":false,"assets":[
        {{"name":"{archive}","browser_download_url":"https://github.com/x/archive.zip"}},
        {{"name":"{checksum}","browser_download_url":"https://github.com/x/archive.zip.sha256"}},
        {{"name":"{checksum}.sig","browser_download_url":"https://github.com/x/archive.zip.sha256.sig"}}
        ]}}"#
    );
    let release = parse_release(&json).unwrap().expect("a release");
    let signature = release.signature.expect("a signature");
    assert_eq!(signature.name, format!("{checksum}.sig"));
    assert_eq!(signature.url, "https://github.com/x/archive.zip.sha256.sig");
}

#[test]
fn a_release_without_a_signature_is_still_parsed() {
    let (archive, checksum) = {
        let version = version("0.2.0");
        let archive = archive_name(&version).expect("a release platform of this build");
        let checksum = format!("{archive}.sha256");
        (archive, checksum)
    };
    let json = format!(
        r#"{{"tag_name":"v0.2.0","body":"","html_url":"","prerelease":false,"draft":false,"assets":[
        {{"name":"{archive}","browser_download_url":"https://github.com/x/archive.zip"}},
        {{"name":"{checksum}","browser_download_url":"https://github.com/x/archive.zip.sha256"}}
        ]}}"#
    );
    let release = parse_release(&json).unwrap().expect("a release");
    assert!(release.signature.is_none());
}
