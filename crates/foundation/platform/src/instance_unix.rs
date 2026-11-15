//! The instance endpoint on Unix: a socket file in the state folder.

use std::io::{self, BufReader};
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use super::{Request, read_ack, serve, write_request};

/// The socket file of the state folder. The name is short: a socket path of Unix stops
/// at about a hundred bytes, and the state folder of a user is already long.
pub(super) const ENDPOINT: &str = "instance";
/// How long a second start waits for the acknowledgement, and how long the listener
/// waits for the rest of a request.
const WAIT: Duration = Duration::from_secs(5);

/// The listener of a state folder. Dropping it stops the thread and removes the socket.
pub struct Instance {
    endpoint: PathBuf,
    stop: Arc<AtomicBool>,
    finished: Receiver<()>,
    thread: Option<JoinHandle<()>>,
}

impl Instance {
    /// Listens on the endpoint of `state_dir` and passes every request to `on_request`.
    /// A socket file left by a program that died is removed first.
    pub fn listen<F>(state_dir: &Path, mut on_request: F) -> io::Result<Self>
    where
        F: FnMut(Request) + Send + 'static,
    {
        std::fs::create_dir_all(state_dir)?;
        let endpoint = state_dir.join(ENDPOINT);
        let listener = bind(&endpoint)?;
        let stop = Arc::new(AtomicBool::new(false));
        let (sender, finished) = channel();
        let listener_stop = Arc::clone(&stop);
        let thread = thread::Builder::new()
            .name("instance".into())
            .spawn(move || {
                while !listener_stop.load(Ordering::Relaxed) {
                    let Ok((stream, _)) = listener.accept() else {
                        break;
                    };
                    // A request that never finishes must not keep the window from
                    // stopping the listener.
                    stream.set_read_timeout(Some(WAIT)).ok();
                    stream.set_write_timeout(Some(WAIT)).ok();
                    let mut reader = BufReader::new(&stream);
                    let mut writer = &stream;
                    // A malformed or abandoned connection only ends itself.
                    let _ = serve(&mut reader, &mut writer, &mut on_request);
                }
                sender.send(()).ok();
            })?;
        Ok(Self {
            endpoint,
            stop,
            finished,
            thread: Some(thread),
        })
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // The listener waits in `accept`: a connection of our own ends that wait.
        let _ = UnixStream::connect(&self.endpoint);
        if self.finished.recv_timeout(WAIT).is_ok() {
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
            let _ = std::fs::remove_file(&self.endpoint);
        } else {
            // A client that never sends keeps the thread: it ends with the program, and
            // the socket file of the state folder is removed then.
            self.thread.take();
        }
    }
}

fn bind(endpoint: &Path) -> io::Result<UnixListener> {
    match UnixListener::bind(endpoint) {
        Ok(listener) => Ok(listener),
        Err(error) if error.kind() == io::ErrorKind::AddrInUse => {
            // A live program answers. Then the endpoint is not a socket of a dead one.
            if UnixStream::connect(endpoint).is_ok() {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    "another instance is listening",
                ));
            }
            match std::fs::symlink_metadata(endpoint) {
                Ok(meta) if meta.file_type().is_socket() => std::fs::remove_file(endpoint)?,
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            UnixListener::bind(endpoint)
        }
        Err(error) => Err(error),
    }
}

/// Passes `request` to the program listening in `state_dir`. `Ok(true)` means the
/// program took it and answered; `Ok(false)` means no program listens.
pub fn send(state_dir: &Path, request: &Request) -> io::Result<bool> {
    let endpoint = state_dir.join(ENDPOINT);
    let mut stream = match UnixStream::connect(&endpoint) {
        Ok(stream) => stream,
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound
                    | io::ErrorKind::ConnectionRefused
                    | io::ErrorKind::ConnectionReset
            ) =>
        {
            return Ok(false);
        }
        Err(error) => return Err(error),
    };
    stream.set_read_timeout(Some(WAIT))?;
    stream.set_write_timeout(Some(WAIT))?;
    write_request(&mut stream, request)?;
    let mut reader = BufReader::new(&stream);
    read_ack(&mut reader)?;
    Ok(true)
}
