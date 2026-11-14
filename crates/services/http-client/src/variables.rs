//! The variables of a request: `{{name}}` from the environment, and the built-in
//! `{{$uuid}}`, `{{$timestamp}}` and `{{$randomInt}}`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::execute::Error;
use crate::parse::{Body, Request};

/// The values a request substitutes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Variables {
    values: BTreeMap<String, String>,
}

impl Variables {
    pub fn new(values: BTreeMap<String, String>) -> Self {
        Self { values }
    }

    /// The values of an environment with the built-in variables added.
    pub fn for_request(values: BTreeMap<String, String>) -> Self {
        let mut variables = Self::new(values);
        variables.values.insert("$uuid".to_owned(), uuid());
        variables
            .values
            .insert("$timestamp".to_owned(), timestamp().to_string());
        variables
            .values
            .insert("$randomInt".to_owned(), random_int().to_string());
        variables
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.values.get(name).map(String::as_str)
    }

    pub fn insert(&mut self, name: impl Into<String>, value: impl Into<String>) {
        self.values.insert(name.into(), value.into());
    }
}

/// Replaces every `{{name}}` whose name is known. An unknown name stays as it is, so a
/// request of a file without its environment is still readable.
pub fn substitute(text: &str, variables: &Variables) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            out.push_str(&rest[start..]);
            return out;
        };
        let name = after[..end].trim();
        match variables.get(name) {
            Some(value) => out.push_str(value),
            None => {
                out.push_str("{{");
                out.push_str(&after[..end]);
                out.push_str("}}");
            }
        }
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    out
}

static UUID_SERIAL: AtomicU64 = AtomicU64::new(1);
static RANDOM_SERIAL: AtomicU64 = AtomicU64::new(1);

fn nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos())
}

/// A version 4 identifier of 36 characters, made from the clock and a counter: it only
/// has to be different between two requests, not unguessable.
fn uuid() -> String {
    let mut state = nanos() as u64
        ^ UUID_SERIAL.fetch_add(1, Ordering::Relaxed).rotate_left(17)
        ^ u64::from(std::process::id());
    let mut bytes = [0u8; 16];
    for chunk in bytes.chunks_mut(8) {
        // xorshift64*, enough to spread the clock and the counter over the bytes.
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        for (index, byte) in chunk.iter_mut().enumerate() {
            *byte = (state >> (index * 8)) as u8;
        }
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

fn timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

/// An integer from 0 to 999, as the client of the other editors gives.
fn random_int() -> u64 {
    (nanos() as u64 ^ RANDOM_SERIAL.fetch_add(1, Ordering::Relaxed)) % 1000
}

/// A request with every variable replaced and the body of a file read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedRequest {
    pub name: Option<String>,
    pub method: crate::parse::Method,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
    /// The request line of the file, from zero.
    pub line: usize,
}

impl PreparedRequest {
    /// The label of the request for the history and the status of the run.
    pub fn label(&self) -> String {
        match &self.name {
            Some(name) if !name.is_empty() => name.clone(),
            _ => format!("{} {}", self.method.as_str(), self.url),
        }
    }
}

/// Makes the request ready to send: substitutes the variables and reads `< file`.
/// Blocks on the file of the body, so the caller runs it away from the window.
pub fn prepare(
    request: &Request,
    base_dir: &Path,
    variables: &Variables,
) -> Result<PreparedRequest, Error> {
    let url = substitute(&request.url, variables);
    if url.trim().is_empty() {
        return Err(Error::InvalidUrl("the request has no URL".to_owned()));
    }
    let headers = request
        .headers
        .iter()
        .map(|(name, value)| {
            (
                substitute(name, variables).trim().to_owned(),
                substitute(value, variables),
            )
        })
        .collect();
    let body = match &request.body {
        None => None,
        Some(Body::Inline(text)) => Some(substitute(text, variables).into_bytes()),
        Some(Body::File(path)) => {
            let path = substitute(path, variables);
            let path = PathBuf::from(path);
            let path = if path.is_absolute() {
                path
            } else {
                base_dir.join(path)
            };
            let bytes = std::fs::read(&path).map_err(|error| Error::BodyFile {
                path: path.display().to_string(),
                message: error.to_string(),
            })?;
            Some(bytes)
        }
    };
    Ok(PreparedRequest {
        name: request.name.clone(),
        method: request.method,
        url,
        headers,
        body,
        line: request.line,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{Variables, prepare, substitute, uuid};
    use crate::parse::parse;

    fn variables() -> Variables {
        Variables::for_request(BTreeMap::from([
            ("host".to_owned(), "http://localhost:8080".to_owned()),
            ("name".to_owned(), "Ann".to_owned()),
        ]))
    }

    #[test]
    fn a_known_variable_is_replaced_and_an_unknown_one_stays() {
        let variables = variables();
        assert_eq!(
            substitute(
                "{{host}}/owners?lastName={{ name }}&other={{gone}}",
                &variables
            ),
            "http://localhost:8080/owners?lastName=Ann&other={{gone}}"
        );
    }

    #[test]
    fn an_opening_brace_without_a_close_is_kept() {
        assert_eq!(substitute("a {{host", &variables()), "a {{host");
    }

    #[test]
    fn the_built_in_values_have_their_shape() {
        let variables = variables();
        let id = variables.get("$uuid").unwrap().to_owned();
        assert_eq!(id.len(), 36);
        assert_eq!(id.as_bytes()[8], b'-');
        assert_eq!(id.as_bytes()[13], b'-');
        assert_eq!(id.as_bytes()[18], b'-');
        assert_eq!(id.as_bytes()[23], b'-');
        assert_eq!(id.as_bytes()[14], b'4');
        let timestamp: u64 = variables.get("$timestamp").unwrap().parse().unwrap();
        assert!(timestamp > 1_600_000_000);
        let random: u64 = variables.get("$randomInt").unwrap().parse().unwrap();
        assert!(random < 1000);
        assert_ne!(uuid(), uuid(), "two identifiers differ");
    }

    #[test]
    fn a_request_sends_its_variables_and_its_headers() {
        let file = crate::parse::parse("# @name owners\nGET {{host}}/owners\nAccept: {{name}}\n");
        let variables = variables();
        let prepared = prepare(&file.requests[0], std::path::Path::new("."), &variables).unwrap();
        assert_eq!(prepared.url, "http://localhost:8080/owners");
        assert_eq!(prepared.headers, [("Accept".to_owned(), "Ann".to_owned())]);
        assert_eq!(prepared.label(), "owners");
    }

    #[test]
    fn a_body_of_a_file_is_read_relative_to_the_request_file() {
        let root = test_support::scratch("http-client-body");
        std::fs::write(root.join("owner.json"), "{\"firstName\":\"Ann\"}").unwrap();
        let file = parse("POST http://localhost/api\n\n< owner.json\n");
        let prepared = prepare(&file.requests[0], &root, &Variables::default()).expect("the body");
        assert_eq!(
            prepared.body.as_deref(),
            Some(b"{\"firstName\":\"Ann\"}".as_slice())
        );
    }

    #[test]
    fn a_missing_body_file_is_an_error() {
        let file = parse("POST http://localhost/api\n\n< missing.json\n");
        let error = prepare(
            &file.requests[0],
            std::path::Path::new("."),
            &Variables::default(),
        )
        .expect_err("the file is missing");
        assert!(error.to_string().contains("missing.json"), "{error}");
    }

    #[test]
    fn a_request_without_a_url_is_rejected_before_it_is_sent() {
        let file = parse("GET   \n");
        let error = prepare(
            &file.requests[0],
            std::path::Path::new("."),
            &Variables::default(),
        )
        .expect_err("there is no URL");
        assert!(matches!(error, crate::execute::Error::InvalidUrl(_)));
    }
}
