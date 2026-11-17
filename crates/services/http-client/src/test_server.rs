//! A server on `127.0.0.1` with a port of its own, for the tests of the client: it
//! records what it receives and answers from a small table of paths.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

/// One request the server received.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Received {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Received {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(header, _)| header.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

pub struct TestServer {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    received: Arc<Mutex<Vec<Received>>>,
}

impl TestServer {
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let received = Arc::new(Mutex::new(Vec::new()));
        let stopped = stop.clone();
        let log = received.clone();
        let thread = std::thread::spawn(move || {
            while !stopped.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => answer(stream, &log, &stopped),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            address,
            stop,
            thread: Some(thread),
            received,
        }
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.address)
    }

    pub fn received(&self) -> Vec<Received> {
        self.received
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
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

fn answer(mut stream: TcpStream, log: &Mutex<Vec<Received>>, stop: &AtomicBool) {
    stream.set_nonblocking(false).ok();
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }
    let mut words = line.split_whitespace();
    let method = words.next().unwrap_or_default().to_owned();
    let path = words.next().unwrap_or_default().to_owned();
    let mut headers = Vec::new();
    loop {
        let mut header = String::new();
        match reader.read_line(&mut header) {
            Ok(0) => break,
            Ok(_) if header.trim().is_empty() => break,
            Ok(_) => {
                if let Some((name, value)) = header.split_once(':') {
                    headers.push((name.trim().to_owned(), value.trim().to_owned()));
                }
            }
            Err(_) => return,
        }
    }
    let length: usize = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; length];
    if length > 0 && reader.read_exact(&mut body).is_err() {
        return;
    }
    log.lock()
        .unwrap_or_else(|error| error.into_inner())
        .push(Received {
            method,
            path: path.clone(),
            headers,
            body: body.clone(),
        });

    match path.split('?').next().unwrap_or_default() {
        "/get" => {
            let body = br#"{"owners":[{"id":1,"lastName":"Davis"}]}"#;
            let _ = respond(
                &mut stream,
                "200 OK",
                &[
                    ("Content-Type", "application/json"),
                    ("Content-Length", &body.len().to_string()),
                    ("X-Server", "test"),
                ],
                body,
            );
        }
        "/post" => {
            let reply = String::from_utf8_lossy(&body).into_owned();
            let _ = respond(
                &mut stream,
                "201 Created",
                &[
                    ("Content-Type", "application/json"),
                    ("Content-Length", &reply.len().to_string()),
                ],
                reply.as_bytes(),
            );
        }
        "/redirect" => {
            let _ = respond(
                &mut stream,
                "302 Found",
                &[("Location", "/get"), ("Content-Length", "0")],
                b"",
            );
        }
        "/slow" => {
            std::thread::sleep(Duration::from_secs(3));
            let _ = respond(&mut stream, "200 OK", &[("Content-Length", "2")], b"ok");
        }
        "/trickle" => {
            // The head promises a large body that arrives one byte at a time, so the
            // client is reading while it is stopped.
            let head = "HTTP/1.1 200 OK\r\nContent-Length: 1000000\r\nConnection: close\r\n\r\n";
            stream.write_all(head.as_bytes()).ok();
            stream.flush().ok();
            for _ in 0..1000 {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                if stream.write_all(b"x").is_err() {
                    break;
                }
                stream.flush().ok();
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        _ => {
            let _ = respond(
                &mut stream,
                "404 Not Found",
                &[("Content-Length", "0")],
                b"",
            );
        }
    }
    stream.shutdown(std::net::Shutdown::Both).ok();
}

fn respond(
    stream: &mut TcpStream,
    status: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> std::io::Result<()> {
    let mut head = format!("HTTP/1.1 {status}\r\nConnection: close\r\n");
    for (name, value) in headers {
        head.push_str(name);
        head.push_str(": ");
        head.push_str(value);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}
