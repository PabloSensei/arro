//! A program that runs from an AppImage: the file of the release it updates itself
//! with, the replacement of that one file, and a folder where it cannot be replaced.

use std::ffi::OsStr;
use std::io::{BufRead, BufReader};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use super::*;

fn version(text: &str) -> Version {
    Version::parse(text).unwrap()
}

fn temp(name: &str) -> PathBuf {
    test_support::scratch(&format!("appimage-{name}"))
}

/// The release server of the test: every route answers with the bytes of its body.
struct Server {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Server {
    /// The routes are built once the address is known, so an answer may name it.
    fn start_with(build: impl FnOnce(SocketAddr) -> Vec<(String, Vec<u8>)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let routes = Arc::new(build(address));
        let thread = std::thread::spawn(move || {
            while !stopped.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => answer(stream, &routes),
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

fn answer(mut stream: TcpStream, routes: &[(String, Vec<u8>)]) {
    // On Windows an accepted stream inherits the non-blocking mode of the listener, and
    // a read before the request arrives would see nothing.
    stream.set_nonblocking(false).ok();
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut request = String::new();
    reader.read_line(&mut request).ok();
    // Closing a socket with unread data resets the connection on Windows.
    let mut line = String::new();
    while reader.read_line(&mut line).is_ok_and(|read| read > 2) {
        line.clear();
    }
    let path = request.split_whitespace().nth(1).unwrap_or("");
    let Some((_, body)) = routes.iter().find(|(route, _)| route == path) else {
        let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
        return;
    };
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).ok();
    stream.write_all(body).ok();
    stream.flush().ok();
    stream.shutdown(std::net::Shutdown::Both).ok();
}

/// The response of a release that offers the AppImage of `version` at two routes of the
/// server.
fn release_json(version: &Version, image_url: &str, checksum_url: &str) -> String {
    let image = appimage_name(version);
    let checksum = format!("{image}.sha256");
    format!(
        r#"{{"tag_name":"v{version}","name":"Arro {version}","body":"Notes","html_url":"https://example.test/release","prerelease":false,"draft":false,"assets":[{{"name":"{image}","browser_download_url":"{image_url}"}},{{"name":"{checksum}","browser_download_url":"{checksum_url}"}}]}}"#
    )
}

/// A server that publishes the AppImage `payload` of 0.2.0 and the checksum the file
/// claims: the right one, or a wrong one for the refused update.
fn image_server(payload: &[u8], hash: String) -> Server {
    Server::start_with(|address| {
        let url = format!("http://{address}");
        let json = release_json(
            &version("0.2.0"),
            &format!("{url}/image"),
            &format!("{url}/image.sha256"),
        );
        vec![
            ("/latest".into(), json.into_bytes()),
            ("/image".into(), payload.to_vec()),
            (
                "/image.sha256".into(),
                format!("{hash}  arro.AppImage\n").into_bytes(),
            ),
        ]
    })
}

/// The release the server describes, as a program that runs from the image at
/// `image_path` reads it, downloaded and put in place of that file.
fn install_from(server: &Server, dir: &Path) -> Result<PathBuf, Error> {
    let image = dir.join("Arro-0.1.0-x86_64.AppImage");
    let text = release_json(
        &version("0.2.0"),
        &server.url("/image"),
        &server.url("/image.sha256"),
    );
    let release = parse_release_of(&text, Some(image.as_os_str()))
        .unwrap()
        .expect("a release");
    let source = UpdateSource::with_url(server.url("/latest"));
    let downloaded = source.download(&release, &dir.join("downloads"))?;
    replace_appimage(&downloaded, &image)
}

/// The name of an image has the version and the architecture; the updater looks for
/// exactly this file in the release. Only x86-64 is built.
#[test]
fn the_name_of_the_image_has_the_version_and_the_architecture() {
    assert_eq!(
        appimage_name(&version("0.2.0")),
        "arro-0.2.0-x86_64.AppImage"
    );
}

/// The runtime of an image names the file in `APPIMAGE`: the release of such a program
/// offers its image instead of the archive of the platform.
#[test]
fn the_appimage_of_the_environment_is_the_file_of_the_release() {
    let version = version("0.2.0");
    let image = OsStr::new("/home/user/Applications/Arro-0.1.0-x86_64.AppImage");

    assert_eq!(
        asset_name_of(&version, Some(image)),
        Some("arro-0.2.0-x86_64.AppImage".to_owned())
    );
    assert_eq!(appimage_path_of(Some(image)), Some(PathBuf::from(image)));

    // The same build started from a folder keeps the archive of its platform, so the
    // Linux tar.gz stays the update of an unpacked release.
    assert_eq!(
        asset_name_of(&version, None),
        archive_name_for(&version, Platform::current())
    );
}

#[test]
fn an_empty_appimage_variable_names_no_image() {
    let version = version("0.2.0");
    assert_eq!(appimage_path_of(Some(OsStr::new(""))), None);
    assert_eq!(
        asset_name_of(&version, Some(OsStr::new(""))),
        archive_name_for(&version, Platform::current())
    );
}

/// An image is one file: the update puts the new one in its place, and the file that
/// starts next is executable.
#[test]
fn an_update_replaces_the_image_and_keeps_it_executable() {
    let dir = temp("replace");
    let image = dir.join("Arro-0.1.0-x86_64.AppImage");
    fs::write(&image, "old image").unwrap();
    platform::set_mode(&image, 0o755).unwrap();
    let download = dir.join("downloads/arro-0.2.0-x86_64.AppImage");
    fs::create_dir_all(download.parent().unwrap()).unwrap();
    fs::write(&download, "new image").unwrap();

    let started = replace_appimage(&download, &image).unwrap();

    assert_eq!(started, image);
    assert_eq!(fs::read_to_string(&image).unwrap(), "new image");
    #[cfg(unix)]
    assert_ne!(
        platform::mode(&image).unwrap() & 0o111,
        0,
        "the new image is not executable"
    );
    // Nothing of the replacement stays next to the image: only the image and the folder
    // of the download are left.
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 2);
}

/// The image of the release server replaces the file of the running program after the
/// `.sha256` of the release matched.
#[test]
fn a_release_server_update_checks_the_sha256_and_replaces_the_image() {
    let dir = temp("server");
    let image = dir.join("Arro-0.1.0-x86_64.AppImage");
    fs::write(&image, "old image").unwrap();
    let payload = b"new image of release 0.2.0".to_vec();
    let source = dir.join("source.AppImage");
    fs::write(&source, &payload).unwrap();
    let hash = sha256_file(&source).unwrap();
    let server = image_server(&payload, hash);

    let started = install_from(&server, &dir).unwrap();

    assert_eq!(started, image);
    assert_eq!(fs::read(&image).unwrap(), payload);
}

/// A file that is not the one the `.sha256` of the release names is not put in place.
#[test]
fn a_wrong_sum_keeps_the_old_image() {
    let dir = temp("wrong-sum");
    let image = dir.join("Arro-0.1.0-x86_64.AppImage");
    fs::write(&image, "old image").unwrap();
    let server = image_server(b"new image of release 0.2.0", "0".repeat(64));

    let error = install_from(&server, &dir).unwrap_err();

    assert!(matches!(error, Error::Checksum { .. }), "{error}");
    assert_eq!(fs::read_to_string(&image).unwrap(), "old image");
    // The download does not stay behind for the next start to pick up.
    assert_eq!(fs::read_dir(dir.join("downloads")).unwrap().count(), 0);
}

/// The window offers "Download the new package" on a folder the user cannot write, as
/// it does for an installation from a package: the image cannot be replaced there.
#[cfg(unix)]
#[test]
fn an_image_in_a_folder_without_write_permission_shows_the_release_page() {
    let dir = temp("readonly");
    let folder = dir.join("apps");
    fs::create_dir_all(&folder).unwrap();
    let image = folder.join("Arro-0.1.0-x86_64.AppImage");
    fs::write(&image, "image").unwrap();
    platform::set_mode(&folder, 0o555).unwrap();

    assert!(installed_from_package(&folder));
    assert_eq!(fs::read_to_string(&image).unwrap(), "image");

    // The test folder is removed with the thread; the folder has to be writable again
    // by then.
    platform::set_mode(&folder, 0o755).unwrap();
}

/// An update of an image works in the folder of the file, not in the temporary folder
/// the runtime unpacked the program into.
#[test]
fn the_folder_of_an_image_is_the_folder_of_the_file() {
    let executable = Path::new("/tmp/.mount_ArroABC/usr/arro");
    let image = Path::new("/home/user/Applications/Arro-0.1.0-x86_64.AppImage");

    assert_eq!(
        install_folder(executable, Some(image)),
        Path::new("/home/user/Applications")
    );
    assert_eq!(
        install_folder(executable, None),
        Path::new("/tmp/.mount_ArroABC/usr")
    );
}

/// An update that stopped after the copy leaves a file next to the image. The first
/// start removes that copy and nothing else of the folder around the image.
#[test]
fn the_first_start_removes_only_the_copies_of_an_interrupted_update() {
    let dir = temp("leftovers");
    let image = dir.join("Arro-0.1.0-x86_64.AppImage");
    fs::write(&image, "image").unwrap();
    let leftover = dir.join(".Arro-0.1.0-x86_64.AppImage.new-7");
    fs::write(&leftover, "half of the image").unwrap();
    let user_file = dir.join("My Notes.txt");
    fs::write(&user_file, "mine").unwrap();

    assert_eq!(remove_image_leftovers(&image).unwrap(), 1);

    assert!(!leftover.exists());
    assert!(image.is_file());
    assert_eq!(fs::read_to_string(&user_file).unwrap(), "mine");
}
