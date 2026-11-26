//! The settings files of the MCP clients this machine has, and the one entry of Arro in
//! them.
//!
//! A client is found by its folder of settings or by the file itself. A write touches the
//! `arro` entry alone and keeps the rest of the file as it was, as far as the format lets
//! it: the JSON of a client is parsed as a tree and written back, the TOML keeps every
//! line outside the table of the server. A copy of the file stays next to it under
//! `.arro-backup` before the first write.
//!
//! Every path comes from the caller: nothing here reads the profile of the user on its
//! own, so a test works in a folder of its own.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

/// The name of the entry Arro writes into every client.
pub const ENTRY: &str = "arro";
/// The copy of a file taken before the entry changes.
const BACKUP: &str = ".arro-backup";

/// One program that can speak MCP.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Client {
    ClaudeCode,
    Codex,
    Junie,
    VsCode,
    CopilotCli,
}

/// The clients the Settings page lists, in the order of the page.
pub const CLIENTS: [Client; 5] = [
    Client::ClaudeCode,
    Client::Codex,
    Client::Junie,
    Client::VsCode,
    Client::CopilotCli,
];

/// The file of the user or the one of the project.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    User,
    Project,
}

/// How a client reaches the server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transport {
    HttpStream,
    Stdio,
    Sse,
}

impl Transport {
    pub fn title(self) -> &'static str {
        match self {
            Self::HttpStream => "HTTP Stream",
            Self::Stdio => "Stdio",
            Self::Sse => "SSE",
        }
    }
}

/// The program and the address a client starts or calls.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Server {
    /// The stdio shim next to the program of the window.
    pub command: String,
    /// The Streamable HTTP endpoint of the window, when it is published.
    pub url: Option<String>,
    /// The token of the endpoint.
    pub token: Option<String>,
}

/// What the Settings page says about one client.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// The folder or the settings file of the client is there.
    Found,
    /// Neither the folder nor the file is there.
    Missing,
    /// The file is there but it is not the format it should be.
    Damaged,
}

/// One client and the state of its entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientState {
    pub client: Client,
    pub status: Status,
    pub configured: bool,
    /// The settings file itself is there, rather than only the folder of the client.
    pub file: bool,
    pub path: PathBuf,
}

/// A settings file that cannot be read or written as the format wants it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigError(pub String);

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for ConfigError {}

fn error(text: impl Into<String>) -> ConfigError {
    ConfigError(text.into())
}

impl Client {
    /// The name the settings of the window keep.
    pub fn id(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude",
            Self::Codex => "codex",
            Self::Junie => "junie",
            Self::VsCode => "vscode",
            Self::CopilotCli => "copilot",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::ClaudeCode => "Claude Code",
            Self::Codex => "Codex",
            Self::Junie => "Junie",
            Self::VsCode => "VS Code",
            Self::CopilotCli => "GitHub Copilot CLI",
        }
    }

    /// The settings of the client are TOML rather than JSON.
    pub fn is_toml(self) -> bool {
        self == Self::Codex
    }

    fn json_root(self) -> &'static str {
        match self {
            Self::VsCode => "servers",
            _ => "mcpServers",
        }
    }

    /// The folder whose presence means the client is installed.
    pub fn home_folder(self, home: &Path) -> PathBuf {
        match self {
            Self::ClaudeCode => home.join(".claude"),
            Self::Codex => home.join(".codex"),
            Self::Junie => home.join(".junie"),
            Self::VsCode => home.join("Code").join("User"),
            Self::CopilotCli => home.join(".copilot"),
        }
    }

    /// The file of the user the entry goes to.
    pub fn user_config(self, home: &Path) -> PathBuf {
        match self {
            Self::ClaudeCode => home.join(".claude.json"),
            Self::Codex => home.join(".codex").join("config.toml"),
            Self::Junie => home.join(".junie").join("mcp.json"),
            Self::VsCode => vscode_config(home),
            Self::CopilotCli => home.join(".copilot").join("mcp-config.json"),
        }
    }

    /// The file of the project the entry goes to.
    pub fn project_config(self, project: &Path) -> PathBuf {
        match self {
            Self::ClaudeCode => project.join(".mcp.json"),
            Self::Codex => project.join(".codex").join("config.toml"),
            Self::Junie => project.join(".junie").join("mcp.json"),
            Self::VsCode => project.join(".vscode").join("mcp.json"),
            Self::CopilotCli => project.join(".github").join("copilot").join("mcp.json"),
        }
    }

    /// The file [`configure`] and [`remove`] work on.
    pub fn config(self, scope: Scope, home: &Path, project: &Path) -> PathBuf {
        match scope {
            Scope::User => self.user_config(home),
            Scope::Project => self.project_config(project),
        }
    }

    /// Whether the folder or the file of the client is on this machine. A missing folder
    /// whose config file is there still counts as a client.
    pub fn is_installed(self, scope: Scope, home: &Path, project: &Path) -> bool {
        let file = self.config(scope, home, project);
        if file.is_file() {
            return true;
        }
        let folder = match scope {
            Scope::User => self.home_folder(home),
            Scope::Project => file.parent().map(Path::to_path_buf).unwrap_or_default(),
        };
        folder.is_dir()
    }
}

/// The settings folder of VS Code of this system, under `home`.
fn vscode_config(home: &Path) -> PathBuf {
    if cfg!(windows) {
        home.join("AppData")
            .join("Roaming")
            .join("Code")
            .join("User")
            .join("mcp.json")
    } else if cfg!(target_os = "macos") {
        home.join("Library")
            .join("Application Support")
            .join("Code")
            .join("User")
            .join("mcp.json")
    } else {
        home.join(".config")
            .join("Code")
            .join("User")
            .join("mcp.json")
    }
}

/// The home folder of the user, as the settings files see it.
pub fn home_folder() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_default()
}

/// What the Settings page shows for one client: whether it is there and whether its file
/// already names Arro.
pub fn state(client: Client, scope: Scope, home: &Path, project: &Path) -> ClientState {
    let path = client.config(scope, home, project);
    let file = path.is_file();
    if !client.is_installed(scope, home, project) {
        return ClientState {
            client,
            status: Status::Missing,
            configured: false,
            file,
            path,
        };
    }
    match has_entry(client, &path) {
        Ok(configured) => ClientState {
            client,
            status: Status::Found,
            configured,
            file,
            path,
        },
        Err(_) => ClientState {
            client,
            status: Status::Damaged,
            configured: false,
            file,
            path,
        },
    }
}

/// Whether the file already names Arro.
pub fn has_entry(client: Client, path: &Path) -> Result<bool, ConfigError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(cause) => return Err(error(format!("{}: {cause}", path.display()))),
    };
    if client.is_toml() {
        return Ok(toml_table(&text).is_some());
    }
    let root: Value = serde_json::from_str(&text)
        .map_err(|cause| error(format!("{}: {cause}", path.display())))?;
    Ok(root
        .get(client.json_root())
        .and_then(Value::as_object)
        .is_some_and(|servers| servers.contains_key(ENTRY)))
}

/// The transport a settings file gets. A project file may be committed to the repository, so
/// the address of the window and its token never go into one: a streamed or SSE server gets
/// the stdio shim there, which finds the window by the project folder, as a user file gets it.
pub fn file_transport(scope: Scope, transport: Transport) -> Transport {
    match (scope, transport) {
        (Scope::Project, Transport::HttpStream | Transport::Sse) => Transport::Stdio,
        _ => transport,
    }
}

/// Adds or replaces the entry of Arro. The rest of the file stays as it was: only the
/// object of the servers or the table of the server changes. A copy of the file lies next
/// to it under `.arro-backup` after the first write. The transport is the one of
/// [`file_transport`], so a project file never carries the token.
pub fn configure(
    client: Client,
    scope: Scope,
    transport: Transport,
    server: &Server,
    home: &Path,
    project: &Path,
) -> Result<PathBuf, ConfigError> {
    let path = client.config(scope, home, project);
    let transport = file_transport(scope, transport);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(cause) => return Err(error(format!("{}: {cause}", path.display()))),
    };
    if client.is_toml() {
        let table = toml_entry(transport, server)?;
        let text = match toml_table(&text) {
            Some((start, end)) => format!("{}{}{}", &text[..start], table, &text[end..]),
            None => {
                let mut text = text;
                if !text.is_empty() && !text.ends_with('\n') {
                    text.push('\n');
                }
                if !text.trim().is_empty() {
                    text.push('\n');
                }
                text.push_str(&table);
                text
            }
        };
        write(&path, &text)?;
        return Ok(path);
    }
    let entry = json_entry(client, transport, server)?;
    let text = json_configure(&text, client.json_root(), &entry)
        .map_err(|cause| error(format!("{}: {cause}", path.display())))?;
    write(&path, &text)?;
    Ok(path)
}

/// Takes the entry of Arro out and leaves the rest alone. False when the file or the entry
/// was not there.
pub fn remove(
    client: Client,
    scope: Scope,
    home: &Path,
    project: &Path,
) -> Result<bool, ConfigError> {
    let path = client.config(scope, home, project);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(cause) => return Err(error(format!("{}: {cause}", path.display()))),
    };
    let changed = if client.is_toml() {
        match toml_table(&text) {
            Some((start, end)) => {
                let text = format!("{}{}", &text[..start], &text[end..]);
                write(&path, &text)?;
                true
            }
            None => false,
        }
    } else {
        let changed = json_remove(&text, client.json_root())
            .map_err(|cause| error(format!("{}: {cause}", path.display())))?;
        if let Some(text) = &changed {
            write(&path, text)?;
        }
        changed.is_some()
    };
    Ok(changed)
}

/// The lines a client needs for one transport, ready to paste into its settings.
pub fn snippet(
    client: Client,
    transport: Transport,
    server: &Server,
) -> Result<String, ConfigError> {
    if client.is_toml() {
        let mut text = toml_entry(transport, server)?;
        if text.ends_with('\n') {
            text.pop();
        }
        Ok(text)
    } else {
        let entry = json_entry(client, transport, server)?;
        let root = client.json_root();
        let mut servers = Map::new();
        servers.insert(ENTRY.to_string(), entry);
        let mut object = Map::new();
        object.insert(root.to_string(), Value::Object(servers));
        Ok(Value::Object(object).to_string())
    }
}

fn json_entry(client: Client, transport: Transport, server: &Server) -> Result<Value, ConfigError> {
    let mut entry = Map::new();
    match transport {
        Transport::Stdio => {
            if server.command.trim().is_empty() {
                return Err(error("the program of the stdio server is not set"));
            }
            // VS Code wants the kind of the server written out; the others take the
            // command alone, and the kind does no harm there.
            entry.insert("type".to_string(), Value::String("stdio".to_string()));
            entry.insert("command".to_string(), Value::String(server.command.clone()));
            entry.insert("args".to_string(), Value::Array(Vec::new()));
        }
        Transport::HttpStream | Transport::Sse => {
            let url = server.url.clone().ok_or_else(|| {
                error("the address of the server is not known; enable the server first")
            })?;
            let kind = if transport == Transport::Sse {
                "sse"
            } else {
                "http"
            };
            entry.insert("type".to_string(), Value::String(kind.to_string()));
            entry.insert("url".to_string(), Value::String(url));
            if let Some(token) = &server.token {
                let mut headers = Map::new();
                headers.insert(
                    "Authorization".to_string(),
                    Value::String(format!("Bearer {token}")),
                );
                entry.insert("headers".to_string(), Value::Object(headers));
            }
        }
    }
    let _ = client;
    Ok(Value::Object(entry))
}

fn toml_entry(transport: Transport, server: &Server) -> Result<String, ConfigError> {
    let mut text = format!("[mcp_servers.{ENTRY}]\n");
    match transport {
        Transport::Stdio => {
            if server.command.trim().is_empty() {
                return Err(error("the program of the stdio server is not set"));
            }
            text.push_str(&format!("command = \"{}\"\n", toml_string(&server.command)));
        }
        Transport::HttpStream | Transport::Sse => {
            let url = server.url.clone().ok_or_else(|| {
                error("the address of the server is not known; enable the server first")
            })?;
            text.push_str(&format!("url = \"{}\"\n", toml_string(&url)));
        }
    }
    Ok(text)
}

fn toml_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

/// The table of the server of Arro in the text, from the header to the next table.
fn toml_table(text: &str) -> Option<(usize, usize)> {
    let mut start = None;
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if start.is_none() {
            if is_arro_header(trimmed) {
                start = Some(offset);
            }
        } else if trimmed.starts_with('[') && !is_arro_header(trimmed) {
            return Some((start.unwrap_or(offset), offset));
        }
        offset += line.len();
    }
    start.map(|start| (start, text.len()))
}

fn is_arro_header(line: &str) -> bool {
    let line = line.split('#').next().unwrap_or_default().trim();
    line == "[mcp_servers.arro]"
        || line == "[mcp_servers.\"arro\"]"
        || line == "[mcp_servers.'arro']"
        || line.starts_with("[mcp_servers.arro.")
        || line.starts_with("[mcp_servers.\"arro\".")
}

/// Adds the entry to the object of the servers, or replaces the one that is there.
fn json_configure(text: &str, root_key: &str, entry: &Value) -> Result<String, ConfigError> {
    let text = if text.trim().is_empty() {
        "{}".to_string()
    } else {
        text.to_string()
    };
    let (root_start, root_end) = object_span(&text, 0)?;
    match member_span(&text, root_start, root_end, root_key) {
        Some(span) => {
            let value: Value = serde_json::from_str(&text[span.0..span.1])
                .map_err(|cause| error(cause.to_string()))?;
            let mut servers = value
                .as_object()
                .cloned()
                .ok_or_else(|| error(format!("{root_key} is not an object")))?;
            servers.insert(ENTRY.to_string(), entry.clone());
            let written = Value::Object(servers).to_string();
            Ok(format!("{}{}{}", &text[..span.0], written, &text[span.1..]))
        }
        None => {
            let written =
                Value::Object(Map::from_iter([(ENTRY.to_string(), entry.clone())])).to_string();
            let inside = text[root_start + 1..root_end - 1].trim();
            let comma = if inside.is_empty() { "" } else { "," };
            Ok(format!(
                "{}{}\"{root_key}\": {written}\n{}",
                &text[..root_end - 1],
                comma,
                &text[root_end - 1..]
            ))
        }
    }
}

/// Takes the entry of Arro out of the object of the servers. `None` when it was not there.
fn json_remove(text: &str, root_key: &str) -> Result<Option<String>, ConfigError> {
    let Ok((root_start, root_end)) = object_span(text, 0) else {
        return Ok(None);
    };
    let Some(span) = member_span(text, root_start, root_end, root_key) else {
        return Ok(None);
    };
    let value: Value =
        serde_json::from_str(&text[span.0..span.1]).map_err(|cause| error(cause.to_string()))?;
    let Some(mut servers) = value.as_object().cloned() else {
        return Err(error(format!("{root_key} is not an object")));
    };
    if servers.remove(ENTRY).is_none() {
        return Ok(None);
    }
    let written = Value::Object(servers).to_string();
    Ok(Some(format!(
        "{}{}{}",
        &text[..span.0],
        written,
        &text[span.1..]
    )))
}

/// The inside of the object that starts at or after `from`: the places of `{` and `}`.
fn object_span(text: &str, from: usize) -> Result<(usize, usize), ConfigError> {
    let bytes = text.as_bytes();
    let mut at = from;
    while at < bytes.len() && bytes[at].is_ascii_whitespace() {
        at += 1;
    }
    if at >= bytes.len() || bytes[at] != b'{' {
        return Err(error("the settings file is not a JSON object"));
    }
    let start = at;
    let end =
        object_end(text, start).ok_or_else(|| error("the settings file is not a JSON object"))?;
    Ok((start, end))
}

/// The place after the `}` that closes the object at `start`.
fn object_end(text: &str, start: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escaped = false;
    let mut at = start;
    while at < bytes.len() {
        let byte = bytes[at];
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            at += 1;
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(at + 1);
                }
            }
            _ => {}
        }
        at += 1;
    }
    None
}

/// The value of the member `key` of the object from `start` to `end`, quotes included for
/// a string.
fn member_span(text: &str, start: usize, end: usize, key: &str) -> Option<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut at = start + 1;
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escaped = false;
    while at < end {
        let byte = bytes[at];
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            at += 1;
            continue;
        }
        match byte {
            b'"' => {
                let (name, after) = read_string(text, at)?;
                if depth == 0 {
                    let mut next = after;
                    while next < end && bytes[next].is_ascii_whitespace() {
                        next += 1;
                    }
                    if next < end && bytes[next] == b':' {
                        next += 1;
                        while next < end && bytes[next].is_ascii_whitespace() {
                            next += 1;
                        }
                        if name == key {
                            let value_end = value_end(text, next, end)?;
                            return Some((next, value_end));
                        }
                    }
                }
                at = after;
                continue;
            }
            b'{' | b'[' => depth += 1,
            b'}' | b']' => depth -= 1,
            _ => {}
        }
        at += 1;
    }
    None
}

/// The place after the value that starts at `start`.
fn value_end(text: &str, start: usize, end: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    match bytes.get(start).copied()? {
        b'{' | b'[' => object_end(text, start).filter(|place| *place <= end),
        b'"' => read_string(text, start).map(|(_, after)| after),
        _ => {
            let mut at = start;
            while at < end && !matches!(bytes[at], b',' | b'}' | b']') {
                at += 1;
            }
            Some(at)
        }
    }
}

/// The text of the string at `start` without the quotes, and the place after it.
fn read_string(text: &str, start: usize) -> Option<(String, usize)> {
    let bytes = text.as_bytes();
    if bytes.get(start).copied()? != b'"' {
        return None;
    }
    let mut at = start + 1;
    let mut escaped = false;
    while at < bytes.len() {
        let byte = bytes[at];
        if escaped {
            escaped = false;
        } else if byte == b'\\' {
            escaped = true;
        } else if byte == b'"' {
            let raw = &text[start..=at];
            let name = serde_json::from_str::<String>(raw).ok()?;
            return Some((name, at + 1));
        }
        at += 1;
    }
    None
}

/// Writes the file, a copy of the old one next to it first. A file that is about to be
/// replaced always has that copy, so a failed write of the new text loses nothing.
fn write(path: &Path, text: &str) -> Result<(), ConfigError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|cause| error(format!("{}: {cause}", parent.display())))?;
    }
    if path.is_file() {
        let mut backup = path.as_os_str().to_os_string();
        backup.push(BACKUP);
        let backup = PathBuf::from(backup);
        std::fs::copy(path, &backup)
            .map_err(|cause| error(format!("{}: {cause}", backup.display())))?;
    }
    std::fs::write(path, text).map_err(|cause| error(format!("{}: {cause}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::{ENTRY, json_configure, json_remove, member_span, object_span};
    use serde_json::Value;

    #[test]
    fn the_member_of_an_object_is_found_after_another_one() {
        let text =
            r#"{"theme":"dark","mcpServers":{"alpha":{"command":"a"},"beta":{"command":"b"}}}"#;
        let (start, end) = object_span(text, 0).expect("the root");
        let (value_start, value_end) =
            member_span(text, start, end, "mcpServers").expect("the servers");
        let value: Value = serde_json::from_str(&text[value_start..value_end]).expect("json");
        assert_eq!(value["beta"]["command"], "b");
    }

    #[test]
    fn the_entry_replaces_the_old_one_without_a_second_key() {
        let text = r#"{"theme":"dark","mcpServers":{"arro":{"command":"old"}}}"#;
        let entry = serde_json::json!({"type": "stdio", "command": "new"});
        let written = json_configure(text, "mcpServers", &entry).expect("write");
        assert_eq!(
            written.matches(&format!("\"{ENTRY}\"")).count(),
            1,
            "{written}"
        );
        let root: Value = serde_json::from_str(&written).expect("json");
        assert_eq!(root["mcpServers"]["arro"]["command"], "new");
        assert_eq!(root["theme"], "dark");
    }

    #[test]
    fn a_missing_root_key_is_added_with_the_entry() {
        let text = r#"{"theme":"dark"}"#;
        let entry = serde_json::json!({"command": "ide-mcp"});
        let written = json_configure(text, "mcpServers", &entry).expect("write");
        let root: Value = serde_json::from_str(&written).expect("json");
        assert_eq!(root["mcpServers"]["arro"]["command"], "ide-mcp");
        assert_eq!(root["theme"], "dark");
    }

    #[test]
    fn remove_leaves_the_other_servers_in_place() {
        let text = r#"{"mcpServers":{"arro":{"command":"a"},"other":{"command":"b"}}}"#;
        let written = json_remove(text, "mcpServers")
            .expect("remove")
            .expect("there was an entry");
        let root: Value = serde_json::from_str(&written).expect("json");
        assert!(root["mcpServers"].get("arro").is_none());
        assert_eq!(root["mcpServers"]["other"]["command"], "b");
    }
}
