//! One program per state folder: a second start hands its path to the first one.
//!
//! The first program of a state folder listens on an endpoint of that folder: a socket
//! file on Unix, a named pipe on Windows. A second start connects, sends the request,
//! reads the acknowledgement and leaves. A socket file of a program that died stays
//! behind; [`Instance::listen`] removes it and listens again.
//!
//! The requests are read by a thread of this module and passed to the closure of the
//! caller, which hands them to the window.

use std::io::{self, BufRead, Write};
use std::path::PathBuf;

#[cfg(unix)]
#[path = "instance_unix.rs"]
mod unix;
#[cfg(unix)]
pub use unix::{Instance, send};

#[cfg(windows)]
#[path = "instance_windows.rs"]
mod windows;
#[cfg(windows)]
pub use windows::{Instance, send};

/// What a second start asks of the running program.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    /// Bring the window of the running program to the front.
    Focus,
    /// Open a file or a folder in the running program.
    Open(PathBuf),
}

/// Longer than any path of the systems we build for: a length above it is not a request.
const MAX_PATH_BYTES: usize = 32 * 1024;
const ACK: &[u8] = b"ok\n";

fn invalid(reason: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, reason)
}

/// The bytes of one request: a line with the kind and the length, then the path itself.
/// The path is not escaped, so a newline in a name is safe.
fn encode(request: &Request) -> Vec<u8> {
    match request {
        Request::Focus => b"focus\n".to_vec(),
        Request::Open(path) => {
            let path = path.to_string_lossy();
            let mut bytes = format!("open {}\n", path.len()).into_bytes();
            bytes.extend_from_slice(path.as_bytes());
            bytes
        }
    }
}

/// One request, or `None` when the other side closed without sending anything: that is
/// how a wake-up connection of [`Instance`] looks.
fn read_request<R: BufRead>(reader: &mut R) -> io::Result<Option<Request>> {
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    let line = line.trim_end_matches(['\r', '\n']);
    if line == "focus" {
        return Ok(Some(Request::Focus));
    }
    let length = line
        .strip_prefix("open ")
        .ok_or_else(|| invalid(format!("not a request: {line}")))?;
    let length: usize = length
        .parse()
        .map_err(|_| invalid(format!("not a length: {length}")))?;
    if length > MAX_PATH_BYTES {
        return Err(invalid(format!("a path of {length} bytes is too long")));
    }
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes)?;
    let text = String::from_utf8(bytes).map_err(|_| invalid("a path is not UTF-8".into()))?;
    Ok(Some(Request::Open(PathBuf::from(text))))
}

fn write_request<W: Write>(writer: &mut W, request: &Request) -> io::Result<()> {
    writer.write_all(&encode(request))?;
    writer.flush()
}

fn write_ack<W: Write>(writer: &mut W) -> io::Result<()> {
    writer.write_all(ACK)?;
    writer.flush()
}

fn read_ack<R: BufRead>(reader: &mut R) -> io::Result<()> {
    let mut line = String::new();
    reader.read_line(&mut line)?;
    if line.trim_end_matches(['\r', '\n']) == "ok" {
        return Ok(());
    }
    Err(invalid(format!("the instance answered {line:?}")))
}

/// Reads a request and answers it, after the caller has taken it. False when the other
/// side sent nothing, which is a wake-up connection.
fn serve<R: BufRead, W: Write, F: FnMut(Request)>(
    reader: &mut R,
    writer: &mut W,
    on_request: &mut F,
) -> io::Result<bool> {
    let Some(request) = read_request(reader)? else {
        return Ok(false);
    };
    on_request(request);
    write_ack(writer)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::sync::mpsc;

    use test_support::TempDir;

    #[test]
    fn a_focus_request_survives_the_endpoint() {
        let bytes = encode(&Request::Focus);
        let mut reader = Cursor::new(bytes);
        assert_eq!(read_request(&mut reader).unwrap(), Some(Request::Focus));
    }

    #[test]
    fn a_path_with_a_newline_survives_the_endpoint() {
        let path = PathBuf::from("/tmp/a\nb c.txt");
        let bytes = encode(&Request::Open(path.clone()));
        let mut reader = Cursor::new(bytes);
        assert_eq!(
            read_request(&mut reader).unwrap(),
            Some(Request::Open(path))
        );
    }

    #[test]
    fn a_request_reaches_the_listening_instance() {
        let folder = TempDir::new("i-path");
        let file = folder.path().join("Order.txt");
        std::fs::write(&file, "an order\n").unwrap();
        let (sender, receiver) = mpsc::channel();
        let instance = Instance::listen(folder.path(), move |request| {
            sender.send(request).ok();
        })
        .unwrap();

        assert!(
            send(folder.path(), &Request::Open(file.clone())).unwrap(),
            "the second start reached the instance"
        );
        assert_eq!(
            receiver
                .recv_timeout(std::time::Duration::from_secs(10))
                .unwrap(),
            Request::Open(file)
        );
        drop(instance);
    }

    #[test]
    fn a_focus_request_reaches_the_listening_instance() {
        let folder = TempDir::new("i-focus");
        let (sender, receiver) = mpsc::channel();
        let instance = Instance::listen(folder.path(), move |request| {
            sender.send(request).ok();
        })
        .unwrap();

        assert!(send(folder.path(), &Request::Focus).unwrap());
        assert_eq!(
            receiver
                .recv_timeout(std::time::Duration::from_secs(10))
                .unwrap(),
            Request::Focus
        );
        drop(instance);
    }

    #[test]
    fn without_an_instance_the_path_is_not_sent() {
        let folder = TempDir::new("i-none");
        assert!(!send(folder.path(), &Request::Focus).unwrap());
    }

    /// A program that died leaves its socket file behind. The next start removes it and
    /// listens on a fresh one.
    #[test]
    fn a_socket_of_a_dead_instance_is_replaced() {
        let folder = TempDir::new("i-dead");
        let first = Instance::listen(folder.path(), |_| {}).unwrap();
        drop(first);

        let (sender, receiver) = mpsc::channel();
        let second = Instance::listen(folder.path(), move |request| {
            sender.send(request).ok();
        })
        .unwrap();
        assert!(send(folder.path(), &Request::Focus).unwrap());
        assert_eq!(
            receiver
                .recv_timeout(std::time::Duration::from_secs(10))
                .unwrap(),
            Request::Focus
        );
        drop(second);
    }

    /// A crashed program cannot remove its socket file: the next start removes it and
    /// listens on a fresh one.
    #[cfg(unix)]
    #[test]
    fn a_socket_file_of_a_crashed_instance_is_removed() {
        let folder = TempDir::new("i-crash");
        let endpoint = folder.path().join(unix::ENDPOINT);
        // Closing a socket does not remove its file, as the death of a program does not.
        let crashed = std::os::unix::net::UnixListener::bind(&endpoint).unwrap();
        drop(crashed);
        assert!(endpoint.exists(), "the file of the dead socket stays");

        let instance = Instance::listen(folder.path(), |_| {}).unwrap();
        assert!(send(folder.path(), &Request::Focus).unwrap());
        drop(instance);
    }

    /// A socket file of a listener that is gone proves nothing by itself: on some systems
    /// a connection to it succeeds. Only an answer means that a program waits there.
    #[cfg(unix)]
    #[test]
    fn only_a_listener_answers_on_a_socket_file() {
        let folder = TempDir::new("i-answer");
        let endpoint = folder.path().join(unix::ENDPOINT);
        let crashed = std::os::unix::net::UnixListener::bind(&endpoint).unwrap();
        drop(crashed);
        assert!(endpoint.exists(), "the file of the dead socket stays");
        assert!(
            !unix::answers(&endpoint),
            "a socket file without a listener answers nothing"
        );

        let instance = Instance::listen(folder.path(), |_| {}).unwrap();
        assert!(unix::answers(&endpoint), "the listener answers");
        drop(instance);
    }

    /// The child of this test is the same test binary with `--exact`: it sends the path
    /// and leaves with the status of a successful run, as a second start of the program
    /// does.
    #[test]
    fn a_second_start_sends_the_path_and_leaves_with_zero() {
        if std::env::var_os(CHILD_STATE).is_some() {
            return;
        }
        let folder = TempDir::new("i-child");
        let file = folder.path().join("pom.xml");
        std::fs::write(&file, "<project/>").unwrap();
        let (sender, receiver) = mpsc::channel();
        let instance = Instance::listen(folder.path(), move |request| {
            sender.send(request).ok();
        })
        .unwrap();

        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "instance::tests::the_child_sends_the_path_and_leaves",
                "--nocapture",
            ])
            .env(CHILD_STATE, folder.path())
            .env(CHILD_FILE, &file)
            .status()
            .unwrap();

        assert!(status.success(), "the second process left with {status}");
        assert_eq!(
            receiver
                .recv_timeout(std::time::Duration::from_secs(30))
                .unwrap(),
            Request::Open(file)
        );
        drop(instance);
    }

    /// The child of [`a_second_start_sends_the_path_and_leaves_with_zero`].
    #[test]
    fn the_child_sends_the_path_and_leaves() {
        let Some(state) = std::env::var_os(CHILD_STATE) else {
            return;
        };
        let file = PathBuf::from(std::env::var_os(CHILD_FILE).expect("the file of the child"));
        let sent = send(PathBuf::from(state).as_path(), &Request::Open(file)).unwrap();
        assert!(sent, "the child reached the instance");
    }

    const CHILD_STATE: &str = "ARRO_INSTANCE_TEST_STATE";
    const CHILD_FILE: &str = "ARRO_INSTANCE_TEST_FILE";
}
