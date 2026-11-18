//! The MCP clients of a project and what they called: who connected, when, how many calls
//! it made and what they were. In memory only, limited, with the arguments that may carry
//! a secret hidden the way the Console of the Git tool hides them.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::tools::ToolClass;

/// The calls one project keeps. The older ones go first.
pub const CALL_LIMIT: usize = 500;
/// The clients one project keeps. A client that reconnects takes its row back.
pub const CLIENT_LIMIT: usize = 64;
/// What stands in place of a password or a token.
pub const HIDDEN: &str = "***";
/// The longest arguments one call keeps. A call is read to see what it touched, not to
/// read the whole file it passed.
const ARGUMENTS_LIMIT: usize = 160;
/// The words of a name that make its value a secret.
const SECRET_WORDS: &[&str] = &[
    "password",
    "passwd",
    "token",
    "secret",
    "api-key",
    "apikey",
    "authorization",
    "credential",
];

/// The name and the version an MCP client gave itself in `initialize`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientInfo {
    pub name: String,
    pub version: String,
}

/// One client of the project as the Agents tab lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientRecord {
    pub session: String,
    pub name: String,
    pub version: String,
    /// The moment of the `initialize` that made the client known, in milliseconds since
    /// the Unix epoch.
    pub connected_at_ms: u64,
    pub calls: usize,
    pub connected: bool,
    /// The transport session of the server, for Disconnect; `None` until the client has
    /// sent a request that carries it.
    pub connection: Option<String>,
}

/// How a call ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallOutcome {
    Ok,
    Error,
    /// The user said no, or the question went unanswered.
    Refused,
}

impl CallOutcome {
    pub fn name(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Error => "error",
            Self::Refused => "refused",
        }
    }
}

/// One call of an MCP client, as the Agents tab shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallRecord {
    /// Unique and growing with every call.
    pub id: u64,
    pub time_ms: u64,
    pub session: String,
    pub client: String,
    pub tool: String,
    /// Short and with the paths relative to the project; a secret is [`HIDDEN`].
    pub arguments: String,
    pub outcome: CallOutcome,
    pub class: ToolClass,
    pub duration_ms: u64,
}

#[derive(Default)]
struct State {
    clients: VecDeque<ClientRecord>,
    calls: VecDeque<CallRecord>,
}

/// The clients and the calls of one gateway: in memory, one per project window.
#[derive(Default)]
pub struct ClientLog {
    state: Mutex<State>,
    next_call: AtomicU64,
}

impl ClientLog {
    /// The client said hello: it is on the list, connected, with the time it arrived. A
    /// client that comes back keeps its calls; a client that was disconnected starts a
    /// new stay.
    pub fn connect(&self, session: &str, client: &ClientInfo) {
        let mut state = self.lock();
        let now = now_millis();
        if let Some(record) = find(&mut state, session, &client.name, Some(&client.version)) {
            if !record.connected {
                record.connected_at_ms = now;
            }
            record.connected = true;
            record.connection = None;
            return;
        }
        if state.clients.len() == CLIENT_LIMIT {
            state.clients.pop_front();
        }
        state.clients.push_back(ClientRecord {
            session: session.to_string(),
            name: client.name.clone(),
            version: client.version.clone(),
            connected_at_ms: now,
            calls: 0,
            connected: true,
            connection: None,
        });
    }

    /// A request named the transport session of the client: Disconnect can close it.
    pub fn bind(&self, session: &str, client: &ClientInfo, connection: &str) {
        let mut state = self.lock();
        if find(&mut state, session, &client.name, Some(&client.version)).is_none() {
            drop(state);
            self.connect(session, client);
            state = self.lock();
        }
        if let Some(record) = find(&mut state, session, &client.name, Some(&client.version)) {
            record.connection = Some(connection.to_string());
            record.connected = true;
        }
    }

    /// Disconnect: the client leaves the list as connected and the transport session it
    /// used comes back, so the caller can close it on the server.
    pub fn disconnect(&self, session: &str, client: &ClientInfo) -> Option<String> {
        let mut state = self.lock();
        let record = find(&mut state, session, &client.name, Some(&client.version))?;
        record.connected = false;
        record.connection.take()
    }

    pub fn connected(&self) -> usize {
        self.lock()
            .clients
            .iter()
            .filter(|record| record.connected)
            .count()
    }

    pub fn clients(&self) -> Vec<ClientRecord> {
        self.lock().clients.iter().cloned().collect()
    }

    /// The calls of one client, newest first.
    pub fn calls(&self, session: &str, client: &str) -> Vec<CallRecord> {
        self.lock()
            .calls
            .iter()
            .rev()
            .filter(|call| call.session == session && call.client == client)
            .cloned()
            .collect()
    }

    /// The calls of every client, newest first.
    pub fn all_calls(&self) -> Vec<CallRecord> {
        self.lock().calls.iter().rev().cloned().collect()
    }

    /// Adds a call to the journal and to the count of its client. The arguments are taken
    /// as the caller prepared them: short, relative and without secrets.
    #[allow(clippy::too_many_arguments)]
    pub fn record(
        &self,
        session: &str,
        client: &ClientInfo,
        tool: &str,
        class: ToolClass,
        arguments: String,
        outcome: CallOutcome,
        duration: Duration,
    ) {
        let mut state = self.lock();
        if let Some(record) = find(&mut state, session, &client.name, None) {
            record.calls += 1;
        }
        let id = self.next_call.fetch_add(1, Ordering::SeqCst) + 1;
        if state.calls.len() == CALL_LIMIT {
            state.calls.pop_front();
        }
        state.calls.push_back(CallRecord {
            id,
            time_ms: now_millis(),
            session: session.to_string(),
            client: client.name.clone(),
            tool: tool.to_string(),
            arguments,
            outcome,
            class,
            duration_ms: duration.as_millis() as u64,
        });
    }

    /// Forgets the clients and their calls.
    pub fn clear(&self) {
        *self.lock() = State::default();
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
}

fn find<'a>(
    state: &'a mut State,
    session: &str,
    name: &str,
    version: Option<&str>,
) -> Option<&'a mut ClientRecord> {
    state.clients.iter_mut().find(|record| {
        record.session == session
            && record.name == name
            && version.is_none_or(|version| record.version == version)
    })
}

/// The arguments of a call as the journal keeps them: `path: src/Foo.txt, line: 3`. The
/// paths inside the project lose the prefix of the project, and a value whose name says
/// it is a secret is [`HIDDEN`].
pub fn arguments(project: &Path, input: &Value) -> String {
    let shown = match input {
        Value::Object(map) => map
            .iter()
            .map(|(name, value)| format!("{name}: {}", shown_value(Some(name), value, project)))
            .collect::<Vec<_>>()
            .join(", "),
        other => shown_value(None, other, project),
    };
    clipped(&shown)
}

fn shown_value(name: Option<&str>, value: &Value, project: &Path) -> String {
    if name.is_some_and(is_secret_name) {
        return HIDDEN.to_string();
    }
    match value {
        Value::String(text) => match relative_path(project, text) {
            Some(path) => hide_url_credentials(&path),
            None => hide_url_credentials(text),
        },
        Value::Array(items) => format!(
            "[{}]",
            items
                .iter()
                .map(|item| shown_value(None, item, project))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Object(map) => format!(
            "{{{}}}",
            map.iter()
                .map(|(name, value)| format!("{name}: {}", shown_value(Some(name), value, project)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        other => other.to_string(),
    }
}

/// A path inside the project without its prefix, with forward slashes; any other text is
/// left as it is.
fn relative_path(project: &Path, text: &str) -> Option<String> {
    let path = Path::new(text);
    let rest = path.strip_prefix(project).ok()?;
    Some(rest.to_string_lossy().replace('\\', "/"))
}

fn is_secret_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    SECRET_WORDS.iter().any(|word| name.contains(word))
}

/// `https://user:token@host/repo` with the whole user information hidden.
fn hide_url_credentials(text: &str) -> String {
    let Some(scheme) = text.find("://") else {
        return text.to_owned();
    };
    let rest = &text[scheme + 3..];
    let Some(at) = rest.find('@') else {
        return text.to_owned();
    };
    let host_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    if at > host_end {
        return text.to_owned();
    }
    format!("{}{HIDDEN}@{}", &text[..scheme + 3], &rest[at + 1..])
}

fn clipped(text: &str) -> String {
    if text.chars().count() <= ARGUMENTS_LIMIT {
        return text.to_owned();
    }
    let mut shown: String = text.chars().take(ARGUMENTS_LIMIT).collect();
    shown.push('…');
    shown
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as u64)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use serde_json::json;

    use super::{CALL_LIMIT, CallOutcome, ClientInfo, ClientLog, HIDDEN, arguments};
    use crate::tools::ToolClass;

    fn client(name: &str) -> ClientInfo {
        ClientInfo {
            name: name.to_string(),
            version: "1.0".to_string(),
        }
    }

    #[test]
    fn the_journal_keeps_the_last_calls_and_answers_the_newest_first() {
        let log = ClientLog::default();
        log.connect("s", &client("one"));
        for index in 0..CALL_LIMIT + 2 {
            log.record(
                "s",
                &client("one"),
                &format!("tool-{index}"),
                ToolClass::Read,
                String::new(),
                CallOutcome::Ok,
                std::time::Duration::from_millis(1),
            );
        }

        let calls = log.calls("s", "one");
        assert_eq!(calls.len(), CALL_LIMIT, "the journal is limited");
        assert_eq!(calls[0].tool, format!("tool-{}", CALL_LIMIT + 1));
        assert_eq!(calls[calls.len() - 1].tool, "tool-2");
        assert_eq!(log.clients()[0].calls, CALL_LIMIT + 2, "every call counts");
    }

    #[test]
    fn a_secret_argument_and_a_password_of_a_url_are_hidden() {
        let project = Path::new("project");
        let input = json!({
            "path": "project/src/Main.txt",
            "password": "hunter2",
            "url": "https://user:s3cret@example.com/repo.git",
        });

        let shown = arguments(project, &input);

        assert!(shown.contains("path: src/Main.txt"), "{shown}");
        assert!(shown.contains(&format!("password: {HIDDEN}")), "{shown}");
        assert!(
            shown.contains(&format!("https://{HIDDEN}@example.com")),
            "{shown}"
        );
        assert!(!shown.contains("hunter2"), "{shown}");
        assert!(!shown.contains("s3cret"), "{shown}");
    }

    #[test]
    fn the_arguments_are_short_and_keep_a_relative_path() {
        let shown = arguments(
            Path::new("C:/work/shop"),
            &json!({"path": "C:/work/shop/src/Main.txt", "text": "x".repeat(500)}),
        );

        assert!(shown.contains("src/Main.txt"), "{shown}");
        assert!(!shown.contains("C:/work/shop"), "{shown}");
        assert!(
            shown.chars().count() <= super::ARGUMENTS_LIMIT + 1,
            "{shown}"
        );
    }

    #[test]
    fn a_disconnect_takes_the_connection_and_keeps_the_row() {
        let log = ClientLog::default();
        log.connect("s", &client("one"));
        log.bind("s", &client("one"), "session-7");

        assert_eq!(log.connected(), 1);
        assert_eq!(
            log.disconnect("s", &client("one")),
            Some("session-7".into())
        );
        assert_eq!(log.connected(), 0);
        assert_eq!(log.disconnect("s", &client("one")), None);
        assert_eq!(log.clients().len(), 1, "the history stays on the screen");
    }
}
