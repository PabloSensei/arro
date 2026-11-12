//! The manifest of an installed release, the cleanup of the first start after an
//! update and an update from a local release server, without a window and without the
//! network.

use super::*;
use std::io::{BufRead, BufReader};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

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
    stream.set_nonblocking(false).ok();
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut request = String::new();
    reader.read_line(&mut request).ok();
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

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "arro-update-{}-{name}-{}",
        std::process::id(),
        NEXT_ID.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_zip(path: &Path, files: Vec<(String, String)>) {
    let file = File::create(path).unwrap();
    let mut writer = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default();
    for (name, text) in files {
        writer.start_file(name, options).unwrap();
        writer.write_all(text.as_bytes()).unwrap();
    }
    writer.finish().unwrap();
}

fn version(text: &str) -> Version {
    Version::parse(text).unwrap()
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap()
}

#[test]
fn the_manifest_lists_the_files_of_the_installed_release() {
    let dir = temp("manifest");
    let install = dir.join("install");
    let zip = dir.join("update.zip");
    write_zip(
        &zip,
        vec![
            ("arro-0.2.0-windows-x86_64/arro.exe".into(), "exe".into()),
            (
                "arro-0.2.0-windows-x86_64/tools/engine-1.63.0/plugins/server.jar".into(),
                "jar".into(),
            ),
        ],
    );

    stage_as(&zip, &install, Some("arro.exe")).unwrap();

    let files = read_manifest(&install).unwrap().unwrap();
    assert_eq!(
        files,
        vec![
            PathBuf::from("arro.exe"),
            PathBuf::from("tools/engine-1.63.0/plugins/server.jar")
        ]
    );
    fs::remove_dir_all(dir).ok();
}

#[test]
fn the_first_start_removes_old_files_and_tools_that_the_manifest_does_not_list() {
    let dir = temp("cleanup");
    let tools = dir.join("tools");
    fs::create_dir_all(tools.join("kept").join("plugin")).unwrap();
    fs::create_dir_all(tools.join("stale").join("plugin")).unwrap();
    fs::write(tools.join("kept").join("plugin").join("server.jar"), "jar").unwrap();
    fs::write(tools.join("stale").join("plugin").join("server.jar"), "jar").unwrap();
    fs::write(dir.join("arro.exe"), "new exe").unwrap();
    fs::write(dir.join("arro.exe.old"), "old exe").unwrap();
    fs::write(
        dir.join(MANIFEST_FILE),
        "arro.exe\ntools/kept/plugin/server.jar\n",
    )
    .unwrap();

    assert_eq!(cleanup(&dir).unwrap(), 2);

    assert!(!dir.join("arro.exe.old").exists());
    assert!(
        tools
            .join("kept")
            .join("plugin")
            .join("server.jar")
            .is_file()
    );
    assert!(!tools.join("stale").exists());
    fs::remove_dir_all(dir).ok();
}

#[test]
fn a_folder_without_a_manifest_keeps_its_tools() {
    let dir = temp("no-manifest");
    let tool = dir.join("tools").join("kept").join("server.jar");
    fs::create_dir_all(tool.parent().unwrap()).unwrap();
    fs::write(&tool, "jar").unwrap();

    assert_eq!(remove_stale_tools(&dir).unwrap(), 0);
    assert!(tool.is_file());
    fs::remove_dir_all(dir).ok();
}

#[test]
fn the_wait_ends_when_the_condition_is_met() {
    let running = Arc::new(AtomicBool::new(true));
    let ended = running.clone();
    let thread = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(80));
        ended.store(false, Ordering::Relaxed);
    });

    let waited = wait_until(|| !running.load(Ordering::Relaxed), Duration::from_secs(5));

    assert!(waited);
    thread.join().unwrap();
}

#[cfg(windows)]
#[test]
fn the_wait_follows_a_stand_in_process_until_it_ends() {
    use std::process::Stdio;

    let mut child = std::process::Command::new("cmd")
        .args(["/C", "ping", "-n", "2", "127.0.0.1"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("the stand-in process starts");

    let waited = wait_until(
        || {
            child
                .try_wait()
                .map(|status| status.is_some())
                .unwrap_or(true)
        },
        Duration::from_secs(30),
    );

    assert!(waited);
    child.wait().ok();
}

#[test]
fn the_wait_gives_up_after_its_timeout() {
    assert!(!wait_until(|| false, Duration::from_millis(120)));
}

#[cfg(windows)]
#[test]
fn an_update_from_the_release_server_replaces_files_and_cleans_up_the_old_release() {
    use std::os::windows::fs::OpenOptionsExt;

    const OLD: &str = "arro-0.1.0-windows-x86_64";
    const NEW: &str = "arro-0.2.0-windows-x86_64";

    let dir = temp("release");
    let install = dir.join("install");
    let old_zip = dir.join("old.zip");
    write_zip(
        &old_zip,
        vec![
            (format!("{OLD}/arro.exe"), "old exe".into()),
            (format!("{OLD}/README.txt"), "old readme".into()),
            (
                format!("{OLD}/tools/engine-1.62.0/plugins/server.jar"),
                "old server".into(),
            ),
            (
                format!(
                    "{OLD}/tools/debugger-0.59.0/extension/server/\
                     debugger-0.52.0.jar"
                ),
                "old debug".into(),
            ),
            (format!("{OLD}/tools/shared/same.jar"), "same jar".into()),
        ],
    );
    stage_as(&old_zip, &install, Some("arro.exe")).unwrap();
    assert_eq!(read(&install.join("README.txt")), "old readme");
    let shared = install.join("tools").join("shared").join("same.jar");
    assert_eq!(read(&shared), "same jar");
    // A process keeps the unchanged jar open without FILE_SHARE_DELETE, as a running
    // server does.
    let held = fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(&shared)
        .unwrap();

    let new_zip = dir.join("new.zip");
    write_zip(
        &new_zip,
        vec![
            (format!("{NEW}/arro.exe"), "new exe".into()),
            (format!("{NEW}/README.txt"), "new readme".into()),
            (
                format!("{NEW}/tools/engine-1.63.0/plugins/server.jar"),
                "new server".into(),
            ),
            (
                format!(
                    "{NEW}/tools/debugger-0.60.0/extension/server/\
                     debugger-0.53.2.jar"
                ),
                "new debug".into(),
            ),
            (format!("{NEW}/tools/shared/same.jar"), "same jar".into()),
        ],
    );
    let archive = archive_name(&version("0.2.0"));
    let checksum = format!("{archive}.sha256");
    let bytes = fs::read(&new_zip).unwrap();
    let hash = sha256_file(&new_zip).unwrap();
    let server = Server::start_with(|address| {
        let json = format!(
            r#"{{"tag_name":"v0.2.0","name":"Arro 0.2.0","body":"Notes","html_url":"https://example.test/release","prerelease":false,"draft":false,"assets":[{{"name":"{archive}","browser_download_url":"http://{address}/archive.zip"}},{{"name":"{checksum}","browser_download_url":"http://{address}/archive.zip.sha256"}}]}}"#
        );
        vec![
            ("/latest".into(), json.into_bytes()),
            ("/archive.zip".into(), bytes.clone()),
            (
                "/archive.zip.sha256".into(),
                format!("{hash}  arro.zip\n").into_bytes(),
            ),
        ]
    });
    let source = UpdateSource::with_url(server.url("/latest"));
    let release = source.check(&version("0.1.0")).unwrap().unwrap();
    let downloads = dir.join("downloads");
    let downloaded = source.download(&release, &downloads).unwrap();

    stage_as(&downloaded, &install, Some("arro.exe")).unwrap();

    assert_eq!(read(&install.join("arro.exe")), "new exe");
    assert_eq!(read(&install.join("README.txt")), "new readme");
    assert_eq!(
        read(
            &install
                .join("tools")
                .join("engine-1.63.0")
                .join("plugins")
                .join("server.jar")
        ),
        "new server"
    );
    assert_eq!(read(&install.join("arro.exe.old")), "old exe");
    assert_eq!(read(&shared), "same jar");
    assert!(install.join("tools").join("engine-1.62.0").is_dir());

    // The second start: the old executable and the folders of the old release go.
    assert_eq!(cleanup(&install).unwrap(), 3);

    assert!(!install.join("arro.exe.old").exists());
    assert!(!install.join("tools").join("engine-1.62.0").exists());
    assert!(!install.join("tools").join("debugger-0.59.0").exists());
    assert!(install.join("tools").join("engine-1.63.0").is_dir());
    assert!(
        install
            .join("tools")
            .join("shared")
            .join("same.jar")
            .is_file()
    );
    drop(held);
    fs::remove_dir_all(dir).ok();
}
