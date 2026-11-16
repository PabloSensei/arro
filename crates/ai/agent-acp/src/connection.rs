//! Newline-delimited JSON-RPC 2.0 over the pipes of the agent process.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;

use crossbeam_channel::{Receiver, Sender, bounded, unbounded};
use serde_json::{Value, json};

/// What the agent sends on its own: a request that wants an answer, or a notification.
#[derive(Debug)]
pub(crate) enum Incoming {
    Request {
        id: Value,
        method: String,
        params: Value,
    },
    Notification {
        method: String,
        params: Value,
    },
    /// The agent closed its output.
    Closed,
}

pub(crate) type Response = Result<Value, String>;

pub(crate) struct Connection {
    writer: Mutex<Box<dyn Write + Send>>,
    next_id: AtomicU64,
    pending: Mutex<HashMap<u64, Sender<Response>>>,
}

impl Connection {
    /// Starts the reader thread. Requests and notifications of the agent arrive on the receiver.
    pub fn start(
        stdin: impl Write + Send + 'static,
        stdout: impl Read + Send + 'static,
    ) -> std::io::Result<(std::sync::Arc<Self>, Receiver<Incoming>)> {
        let connection = std::sync::Arc::new(Self {
            writer: Mutex::new(Box::new(stdin)),
            next_id: AtomicU64::new(1),
            pending: Mutex::new(HashMap::new()),
        });
        let (incoming, inbox) = unbounded();
        let reader = connection.clone();
        thread::Builder::new()
            .name("acp-reader".to_string())
            .spawn(move || reader.read(stdout, incoming))?;
        Ok((connection, inbox))
    }

    fn read(&self, stdout: impl Read, incoming: Sender<Incoming>) {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let Ok(message) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let method = message["method"].as_str().map(str::to_string);
            let params = message.get("params").cloned().unwrap_or(Value::Null);
            match (method, message.get("id").cloned()) {
                (Some(method), Some(id)) => {
                    let _ = incoming.send(Incoming::Request { id, method, params });
                }
                (Some(method), None) => {
                    let _ = incoming.send(Incoming::Notification { method, params });
                }
                // One thread reads the lines in their order, so everything the agent sent
                // before an answer is already in the channel when the answer resolves.
                (None, Some(id)) => self.resolve(&id, &message),
                (None, None) => {}
            }
        }
        // Whoever still waits learns that no answer comes.
        self.pending.lock().unwrap().clear();
        let _ = incoming.send(Incoming::Closed);
    }

    fn resolve(&self, id: &Value, message: &Value) {
        let Some(id) = id.as_u64() else {
            return;
        };
        let Some(waiter) = self.pending.lock().unwrap().remove(&id) else {
            return;
        };
        let response = match message.get("error") {
            Some(error) => Err(error["message"].as_str().unwrap_or("error").to_string()),
            None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
        };
        let _ = waiter.send(response);
    }

    fn write(&self, message: &Value) -> Result<(), String> {
        let mut writer = self.writer.lock().unwrap();
        writeln!(writer, "{message}").map_err(|error| error.to_string())?;
        writer.flush().map_err(|error| error.to_string())
    }

    /// Sends a request; its answer arrives on the receiver, or the receiver closes.
    pub fn request(&self, method: &str, params: Value) -> Receiver<Response> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (waiter, answer) = bounded(1);
        self.pending.lock().unwrap().insert(id, waiter);
        let message = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        if let Err(error) = self.write(&message)
            && let Some(waiter) = self.pending.lock().unwrap().remove(&id)
        {
            let _ = waiter.send(Err(error));
        }
        answer
    }

    pub fn notify(&self, method: &str, params: Value) {
        let _ = self.write(&json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    pub fn reply(&self, id: &Value, result: Value) {
        let _ = self.write(&json!({"jsonrpc": "2.0", "id": id, "result": result}));
    }

    pub fn reply_error(&self, id: &Value, code: i64, message: &str) {
        let error = json!({"code": code, "message": message});
        let _ = self.write(&json!({"jsonrpc": "2.0", "id": id, "error": error}));
    }
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::time::Duration;

    use super::*;

    /// The output of a fake agent: the reader thread blocks until the test hands it the next
    /// part, so the order of the lines belongs to the test.
    struct Script(Receiver<Vec<u8>>);

    impl Read for Script {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let Ok(part) = self.0.recv() else {
                return Ok(0);
            };
            assert!(part.len() <= buf.len());
            buf[..part.len()].copy_from_slice(&part);
            Ok(part.len())
        }
    }

    #[test]
    fn a_notification_waits_in_the_channel_before_the_answer_that_followed_it() {
        let (feed, script) = unbounded();
        let (connection, incoming) = Connection::start(Vec::new(), Script(script)).unwrap();
        // The first request of a connection gets id 1.
        let answer = connection.request("session/prompt", json!({}));
        let notification = json!({"jsonrpc": "2.0", "method": "session/update", "params": {}});
        let response = json!({"jsonrpc": "2.0", "id": 1, "result": {"stopReason": "end_turn"}});
        feed.send(format!("{notification}\n{response}\n").into_bytes())
            .unwrap();

        let response = answer.recv_timeout(Duration::from_secs(10)).unwrap();

        assert!(response.is_ok(), "{response:?}");
        let message = incoming
            .try_recv()
            .expect("the notification is waiting before the answer");
        assert!(
            matches!(message, Incoming::Notification { .. }),
            "{message:?}"
        );
    }
}
