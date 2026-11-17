//! Sending one prepared request and reading its answer. The call blocks: the caller
//! runs it away from the window and keeps the token to stop it.

use std::io::Read;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::parse::Method;
use crate::variables::PreparedRequest;

/// How long a request may take when the setting says nothing else.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(crate::DEFAULT_TIMEOUT_SECONDS as u64);
/// The longest wait for a connection of its own, under the whole timeout.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// The redirects followed, as the client of the other editors does.
const REDIRECTS: u32 = 10;

/// A token that asks a request in flight to stop. Stopping is checked before the request
/// and between the pieces of the answer; a connection that has not answered yet ends
/// with its timeout.
#[derive(Clone, Debug, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// Everything a request can fail with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// The user stopped the request.
    Cancelled,
    Timeout,
    InvalidUrl(String),
    /// The connection, the protocol or the TLS of the server.
    Network(String),
    /// The body was to come from a file that cannot be read.
    BodyFile {
        path: String,
        message: String,
    },
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => write!(formatter, "The request was stopped"),
            Self::Timeout => write!(formatter, "The request timed out"),
            Self::InvalidUrl(message) => write!(formatter, "The URL is not valid: {message}"),
            Self::Network(message) => write!(formatter, "The request failed: {message}"),
            Self::BodyFile { path, message } => {
                write!(formatter, "Cannot read the body file {path}: {message}")
            }
        }
    }
}

impl std::error::Error for Error {}

/// The answer of the server, with the whole body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    /// The reason phrase the server sent, empty when it sent none.
    pub reason: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub elapsed: Duration,
}

impl Response {
    /// The first value of a header, whatever its case.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(header, _)| header.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// The body as text, with the bytes that are not UTF-8 replaced.
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// The size of the body in bytes.
    pub fn size(&self) -> usize {
        self.body.len()
    }
}

fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .timeout_connect(Some(timeout.min(CONNECT_TIMEOUT)))
        .http_status_as_error(false)
        .max_redirects(REDIRECTS)
        .max_redirects_will_error(true)
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .build(),
        )
        .build()
        .into()
}

fn headers_on<B>(
    mut builder: ureq::RequestBuilder<B>,
    headers: &[(String, String)],
) -> ureq::RequestBuilder<B> {
    for (name, value) in headers {
        builder = builder.header(name.as_str(), value.as_str());
    }
    builder
}

fn network(error: ureq::Error) -> Error {
    match error {
        ureq::Error::Timeout(_) => Error::Timeout,
        ureq::Error::Io(io) if io.kind() == std::io::ErrorKind::TimedOut => Error::Timeout,
        ureq::Error::BadUri(message) => Error::InvalidUrl(message),
        other => Error::Network(other.to_string()),
    }
}

/// Sends `request` and reads the answer. Blocks; `cancel` is checked before the request
/// and between the pieces of the answer.
pub fn execute(
    request: &PreparedRequest,
    timeout: Duration,
    cancel: &Cancel,
) -> Result<Response, Error> {
    if cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    let lower = request.url.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return Err(Error::InvalidUrl(request.url.clone()));
    }
    let agent = agent(timeout);
    let started = Instant::now();
    let url = request.url.as_str();
    let body = request.body.as_deref().unwrap_or_default();
    let headers = request.headers.as_slice();
    let answer = match request.method {
        Method::Get => headers_on(agent.get(url), headers).call(),
        Method::Delete => headers_on(agent.delete(url), headers).call(),
        Method::Head => headers_on(agent.head(url), headers).call(),
        Method::Options => headers_on(agent.options(url), headers).call(),
        Method::Post => match request.body {
            Some(_) => headers_on(agent.post(url), headers).send(body),
            None => headers_on(agent.post(url), headers).send_empty(),
        },
        Method::Put => headers_on(agent.put(url), headers).send(body),
        Method::Patch => headers_on(agent.patch(url), headers).send(body),
    }
    .map_err(network)?;
    let elapsed = started.elapsed();
    let status = answer.status();
    let reason = status.canonical_reason().unwrap_or("").to_owned();
    let headers = answer
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_owned(),
                String::from_utf8_lossy(value.as_bytes()).into_owned(),
            )
        })
        .collect();
    let mut reader = answer.into_body().into_reader();
    let mut body = Vec::new();
    let mut buffer = [0u8; 16 * 1024];
    loop {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => body.extend_from_slice(&buffer[..read]),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(Error::Network(error.to_string())),
        }
    }
    Ok(Response {
        status: status.as_u16(),
        reason,
        headers,
        body,
        elapsed,
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{Cancel, Error, execute};
    use crate::parse::Method;
    use crate::variables::PreparedRequest;

    fn request(method: Method, url: String, body: Option<&str>) -> PreparedRequest {
        PreparedRequest {
            name: None,
            method,
            url,
            headers: Vec::new(),
            body: body.map(|text| text.as_bytes().to_vec()),
            line: 0,
        }
    }

    #[test]
    fn a_stopped_request_is_not_sent() {
        let cancel = Cancel::new();
        cancel.cancel();
        let error = execute(
            &request(Method::Get, "http://127.0.0.1:1/".to_owned(), None),
            Duration::from_secs(1),
            &cancel,
        )
        .expect_err("the request is stopped");
        assert_eq!(error, Error::Cancelled);
    }

    #[test]
    fn a_url_without_a_scheme_is_reported() {
        let error = execute(
            &request(Method::Get, "127.0.0.1/nothing".to_owned(), None),
            Duration::from_secs(1),
            &Cancel::new(),
        )
        .expect_err("the URL cannot be sent");
        assert!(matches!(error, Error::InvalidUrl(_)), "{error}");
    }
}
