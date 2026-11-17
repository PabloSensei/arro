//! `ide-mcp`: the MCP server of the IDE for a harness that only speaks stdio. Every JSON-RPC
//! line from stdin goes to the HTTP endpoint of the session, every answer comes back on stdout.
//!
//! `IDE_MCP_URL` names the endpoint, `IDE_MCP_TOKEN` carries the session token, both set by the
//! IDE for the harness it starts. The token never appears on a command line or in a file.

use std::io::{self, BufRead, Read, Write};
use std::net::TcpStream;
use std::process::ExitCode;

use serde_json::{Value, json};

const URL_VARIABLE: &str = "IDE_MCP_URL";
const TOKEN_VARIABLE: &str = "IDE_MCP_TOKEN";

struct Endpoint {
    /// `127.0.0.1:port`; the server only listens on the loopback.
    address: String,
    path: String,
    token: String,
    session: Option<String>,
    protocol: Option<String>,
}

struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Reply {
    fn header(&self, name: &str) -> Option<&str> {
        let found = self
            .headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name));
        found.map(|(_, value)| value.as_str())
    }
}

fn parse_url(url: &str) -> Result<(String, String), String> {
    let rest = url
        .strip_prefix("http://")
        .ok_or("the endpoint must be plain http on the loopback")?;
    let (address, path) = rest.split_once('/').unwrap_or((rest, ""));
    let host = address.split(':').next().unwrap_or_default();
    if host != "127.0.0.1" && host != "localhost" {
        return Err(format!("{host} is not the loopback"));
    }
    Ok((address.to_string(), format!("/{path}")))
}

fn dechunk(mut body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let Some(end) = body.windows(2).position(|pair| pair == b"\r\n") else {
            return out;
        };
        let size = std::str::from_utf8(&body[..end]).unwrap_or("0");
        let size = usize::from_str_radix(size.split(';').next().unwrap_or("0").trim(), 16);
        let size = size.unwrap_or(0);
        if size == 0 {
            return out;
        }
        let start = end + 2;
        let stop = (start + size).min(body.len());
        out.extend_from_slice(&body[start..stop]);
        body = &body[(stop + 2).min(body.len())..];
    }
}

/// JSON-RPC messages in a reply: one JSON document, or the `data:` lines of an event stream.
fn messages(reply: &Reply) -> Vec<Value> {
    let text = String::from_utf8_lossy(&reply.body);
    let stream = reply
        .header("content-type")
        .is_some_and(|kind| kind.contains("text/event-stream"));
    if !stream {
        return serde_json::from_str(text.trim()).into_iter().collect();
    }
    let data = text.lines().filter_map(|line| line.strip_prefix("data:"));
    data.filter_map(|line| serde_json::from_str(line.trim()).ok())
        .collect()
}

impl Endpoint {
    fn post(&self, body: &str) -> io::Result<Reply> {
        let mut stream = TcpStream::connect(&self.address)?;
        let mut request = format!(
            "POST {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\
             Content-Type: application/json\r\nAccept: application/json, text/event-stream\r\n\
             Authorization: Bearer {}\r\nContent-Length: {}\r\n",
            self.path,
            self.address,
            self.token,
            body.len()
        );
        if let Some(session) = &self.session {
            request.push_str(&format!("Mcp-Session-Id: {session}\r\n"));
        }
        if let Some(protocol) = &self.protocol {
            request.push_str(&format!("MCP-Protocol-Version: {protocol}\r\n"));
        }
        request.push_str("\r\n");
        request.push_str(body);
        stream.write_all(request.as_bytes())?;
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw)?;
        let split = raw.windows(4).position(|window| window == b"\r\n\r\n");
        let split = split.ok_or_else(|| io::Error::other("a reply without headers"))?;
        let head = String::from_utf8_lossy(&raw[..split]).into_owned();
        let mut lines = head.lines();
        let status = lines.next().and_then(|line| line.split_whitespace().nth(1));
        let status = status.and_then(|code| code.parse().ok()).unwrap_or(0);
        let headers: Vec<(String, String)> = lines
            .filter_map(|line| line.split_once(':'))
            .map(|(key, value)| (key.trim().to_string(), value.trim().to_string()))
            .collect();
        let mut body = raw[split + 4..].to_vec();
        let reply = Reply {
            status,
            headers,
            body: Vec::new(),
        };
        if reply
            .header("transfer-encoding")
            .is_some_and(|value| value.contains("chunked"))
        {
            body = dechunk(&body);
        }
        Ok(Reply { body, ..reply })
    }

    /// Forwards one message and returns what goes back to the harness.
    fn forward(&mut self, message: &Value) -> Vec<Value> {
        let id = message.get("id").cloned();
        let failure = |text: String| match &id {
            Some(id) if message.get("method").is_some() => vec![json!({
                "jsonrpc": "2.0", "id": id, "error": {"code": -32603, "message": text},
            })],
            _ => Vec::new(),
        };
        let reply = match self.post(&message.to_string()) {
            Ok(reply) => reply,
            Err(error) => return failure(format!("the IDE is not reachable: {error}")),
        };
        if let Some(session) = reply.header("mcp-session-id") {
            self.session = Some(session.to_string());
        }
        if !(200..300).contains(&reply.status) {
            let text = String::from_utf8_lossy(&reply.body).into_owned();
            return failure(format!(
                "the IDE answered {}: {}",
                reply.status,
                text.trim()
            ));
        }
        let answers = messages(&reply);
        if message["method"] == json!("initialize") {
            let version = answers
                .iter()
                .find_map(|answer| answer["result"]["protocolVersion"].as_str());
            self.protocol = version.map(str::to_string);
        }
        answers
    }
}

fn run() -> Result<(), String> {
    let url = std::env::var(URL_VARIABLE).map_err(|_| format!("{URL_VARIABLE} is not set"))?;
    let token =
        std::env::var(TOKEN_VARIABLE).map_err(|_| format!("{TOKEN_VARIABLE} is not set"))?;
    let (address, path) = parse_url(&url)?;
    let mut endpoint = Endpoint {
        address,
        path,
        token,
        session: None,
        protocol: None,
    };
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    for line in stdin.lock().lines() {
        let line = line.map_err(|error| error.to_string())?;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            let error = json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": "parse error"}});
            writeln!(stdout, "{error}").map_err(|error| error.to_string())?;
            continue;
        };
        for answer in endpoint.forward(&message) {
            writeln!(stdout, "{answer}").map_err(|error| error.to_string())?;
        }
        stdout.flush().map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("ide-mcp: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_loopback_urls_are_taken() {
        let (address, path) = parse_url("http://127.0.0.1:4100/mcp").unwrap();
        assert_eq!(
            (address.as_str(), path.as_str()),
            ("127.0.0.1:4100", "/mcp")
        );
        assert!(parse_url("http://example.com/mcp").is_err());
        assert!(parse_url("https://127.0.0.1/mcp").is_err());
    }

    #[test]
    fn chunks_and_event_streams_unwrap() {
        assert_eq!(dechunk(b"5\r\nhello\r\n1\r\n!\r\n0\r\n\r\n"), b"hello!");
        let reply = Reply {
            status: 200,
            headers: vec![("Content-Type".to_string(), "text/event-stream".to_string())],
            body: b"event: message\ndata: {\"id\":1}\n\n".to_vec(),
        };
        assert_eq!(messages(&reply), [json!({"id": 1})]);
    }
}
