//! The release server of the tests: a loopback port that answers routes without the
//! network. Only the tests of this crate use it; it lives here so that the tests of
//! the addresses and of the signature do not each write their own.

use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

/// What a route answers.
pub enum Answer {
    /// Bytes with the body of a `200 OK` answer.
    Body(Vec<u8>),
    /// A `302 Found` to this address.
    Redirect(String),
}

impl Answer {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Body(text.into().into_bytes())
    }
}

pub struct TestServer {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl TestServer {
    /// Starts the server; the routes are built once the address is known, so an answer
    /// may name it.
    pub fn start(build: impl FnOnce(SocketAddr) -> Vec<(String, Answer)>) -> Self {
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
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
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

    pub fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.address)
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            thread.join().ok();
        }
    }
}

fn answer(mut stream: TcpStream, routes: &[(String, Answer)]) {
    // On Windows an accepted socket inherits the non-blocking mode of the listener, and
    // a socket closed with an unread request resets the connection: read the whole head
    // of the request first, however slow a loaded machine sends it.
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
    let Some((_, answer)) = routes.iter().find(|(route, _)| route == path) else {
        let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
        return;
    };
    match answer {
        Answer::Body(body) => {
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(head.as_bytes()).ok();
            stream.write_all(body).ok();
        }
        Answer::Redirect(location) => {
            let head = format!(
                "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\n\
                 Connection: close\r\n\r\n"
            );
            stream.write_all(head.as_bytes()).ok();
        }
    }
    stream.flush().ok();
    stream.shutdown(std::net::Shutdown::Both).ok();
}
