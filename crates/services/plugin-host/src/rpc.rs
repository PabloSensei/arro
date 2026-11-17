//! JSON-RPC 2.0 over stdio, one message per line. A line keeps a plugin without
//! dependencies simple: JSON escapes every newline inside a message.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};

pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;
/// The IDE could not do what was asked, the message says why.
pub const FAILED: i64 = -32000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CallError {
    /// The process went away, or its output stopped making sense.
    Closed,
    TimedOut,
    /// The other side answered with an error.
    Remote {
        code: i64,
        message: String,
    },
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Closed => f.write_str("the plugin process is gone"),
            Self::TimedOut => f.write_str("the plugin did not answer in time"),
            Self::Remote { code, message } => write!(f, "{message} ({code})"),
        }
    }
}

impl std::error::Error for CallError {}

/// A request or notification from the other side.
#[derive(Debug)]
pub struct Incoming {
    pub method: String,
    pub params: Value,
    /// `None` for a notification, which gets no answer.
    pub responder: Option<Responder>,
}

type Pending = Arc<Mutex<HashMap<u64, Sender<Result<Value, CallError>>>>>;
type Writer = Arc<Mutex<Box<dyn Write + Send>>>;

/// Answers one request. Dropping it unanswered tells the other side the call failed.
pub struct Responder {
    id: Value,
    writer: Option<Writer>,
}

impl std::fmt::Debug for Responder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Responder").field("id", &self.id).finish()
    }
}

impl Responder {
    pub fn ok(mut self, result: Value) {
        let message = json!({"jsonrpc": "2.0", "id": self.id, "result": result});
        self.send(&message);
    }

    pub fn error(mut self, code: i64, message: impl Into<String>) {
        let error = json!({"code": code, "message": message.into()});
        let message = json!({"jsonrpc": "2.0", "id": self.id, "error": error});
        self.send(&message);
    }

    fn send(&mut self, message: &Value) {
        if let Some(writer) = self.writer.take() {
            // A process that is gone hears no answer, and nothing waits for one.
            let _ = write_message(&writer, message);
        }
    }
}

impl Drop for Responder {
    fn drop(&mut self) {
        if self.writer.is_some() {
            let id = self.id.take();
            let message = json!({"jsonrpc": "2.0", "id": id,
                "error": {"code": FAILED, "message": "Arro dropped the request"}});
            self.send(&message);
        }
    }
}

pub struct Connection {
    writer: Writer,
    pending: Pending,
    next_id: AtomicU64,
    closed: Arc<AtomicBool>,
}

impl Connection {
    /// Reads `input` on its own thread. Requests and notifications of the other side go to
    /// the returned receiver, which ends when the input does.
    pub fn new(
        input: impl Read + Send + 'static,
        output: impl Write + Send + 'static,
    ) -> (Self, Receiver<Incoming>) {
        let writer: Writer = Arc::new(Mutex::new(Box::new(output)));
        let pending = Pending::default();
        let closed = Arc::new(AtomicBool::new(false));
        let (incoming, received) = mpsc::channel();
        let reader = Reader {
            writer: writer.clone(),
            pending: pending.clone(),
            closed: closed.clone(),
            incoming,
        };
        thread::Builder::new()
            .name("plugin-rpc".into())
            .spawn(move || reader.run(BufReader::new(input)))
            .expect("a thread for the plugin connection");
        let connection = Self {
            writer,
            pending,
            next_id: AtomicU64::new(1),
            closed,
        };
        (connection, received)
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    /// Blocks for at most `timeout`, so call it off the UI thread.
    pub fn call(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, CallError> {
        if self.is_closed() {
            return Err(CallError::Closed);
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (sender, answer) = mpsc::channel();
        self.pending.lock().unwrap().insert(id, sender);
        // The reader marks the end before it clears what waits, so one of the two sees it.
        if self.is_closed() {
            self.pending.lock().unwrap().remove(&id);
            return Err(CallError::Closed);
        }
        let message = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        if write_message(&self.writer, &message).is_err() {
            self.pending.lock().unwrap().remove(&id);
            return Err(CallError::Closed);
        }
        let result = answer.recv_timeout(timeout);
        self.pending.lock().unwrap().remove(&id);
        match result {
            Ok(result) => result,
            Err(RecvTimeoutError::Timeout) => Err(CallError::TimedOut),
            Err(RecvTimeoutError::Disconnected) => Err(CallError::Closed),
        }
    }

    pub fn notify(&self, method: &str, params: Value) -> Result<(), CallError> {
        let message = json!({"jsonrpc": "2.0", "method": method, "params": params});
        write_message(&self.writer, &message).map_err(|_| CallError::Closed)
    }
}

fn write_message(writer: &Writer, message: &Value) -> std::io::Result<()> {
    let mut line = serde_json::to_vec(message)?;
    line.push(b'\n');
    let mut writer = writer.lock().unwrap();
    writer.write_all(&line)?;
    writer.flush()
}

struct Reader {
    writer: Writer,
    pending: Pending,
    closed: Arc<AtomicBool>,
    incoming: Sender<Incoming>,
}

impl Reader {
    fn run(self, input: impl BufRead) {
        for line in input.split(b'\n') {
            let Ok(line) = line else { break };
            if line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            // A plugin that prints something else on stdout has broken the protocol.
            let Ok(message) = serde_json::from_slice::<Value>(&line) else {
                break;
            };
            self.dispatch(message);
        }
        self.closed.store(true, Ordering::Release);
        // Every waiting call learns at once that no answer comes.
        self.pending.lock().unwrap().clear();
    }

    fn dispatch(&self, mut message: Value) {
        let id = message.get_mut("id").map(Value::take);
        if let Some(method) = message
            .get("method")
            .and_then(Value::as_str)
            .map(str::to_owned)
        {
            let params = message.get_mut("params").map(Value::take);
            let incoming = Incoming {
                method,
                params: params.unwrap_or(Value::Null),
                responder: id.map(|id| Responder {
                    id,
                    writer: Some(self.writer.clone()),
                }),
            };
            let _ = self.incoming.send(incoming);
            return;
        }
        let Some(id) = id.as_ref().and_then(Value::as_u64) else {
            return;
        };
        let Some(sender) = self.pending.lock().unwrap().remove(&id) else {
            // An answer after its timeout.
            return;
        };
        let result = match message.get_mut("error") {
            Some(error) => Err(CallError::Remote {
                code: error.get("code").and_then(Value::as_i64).unwrap_or(FAILED),
                message: (error.get("message").and_then(Value::as_str))
                    .unwrap_or("no message")
                    .to_owned(),
            }),
            None => Ok(message
                .get_mut("result")
                .map(Value::take)
                .unwrap_or(Value::Null)),
        };
        let _ = sender.send(result);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pipe in memory: one end writes, the other reads, end of input when the writer goes.
    fn pipe() -> (impl Write + Send + 'static, impl Read + Send + 'static) {
        let (sender, receiver) = mpsc::channel::<Vec<u8>>();
        struct W(Sender<Vec<u8>>);
        impl Write for W {
            fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
                self.0
                    .send(data.to_vec())
                    .map_err(|_| std::io::ErrorKind::BrokenPipe)?;
                Ok(data.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        struct R(Receiver<Vec<u8>>, Vec<u8>);
        impl Read for R {
            fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
                if self.1.is_empty() {
                    match self.0.recv() {
                        Ok(data) => self.1 = data,
                        Err(_) => return Ok(0),
                    }
                }
                let n = out.len().min(self.1.len());
                out[..n].copy_from_slice(&self.1[..n]);
                self.1.drain(..n);
                Ok(n)
            }
        }
        (W(sender), R(receiver, Vec::new()))
    }

    fn pair() -> (
        (Connection, Receiver<Incoming>),
        (Connection, Receiver<Incoming>),
    ) {
        let (a_out, b_in) = pipe();
        let (b_out, a_in) = pipe();
        (Connection::new(a_in, a_out), Connection::new(b_in, b_out))
    }

    const SECOND: Duration = Duration::from_secs(5);

    #[test]
    fn a_call_gets_its_answer_and_an_error_its_code() {
        let ((ide, _), (plugin, requests)) = pair();
        // Each side keeps the other's input open, so neither thread ends before the test.
        thread::spawn(move || {
            let _plugin = plugin;
            for request in requests {
                let responder = request.responder.unwrap();
                match request.method.as_str() {
                    "echo" => responder.ok(request.params),
                    _ => responder.error(METHOD_NOT_FOUND, "no such method"),
                }
            }
        });

        assert_eq!(
            ide.call("echo", json!({"a": 1}), SECOND),
            Ok(json!({"a": 1}))
        );
        let error = ide.call("other", Value::Null, SECOND).unwrap_err();
        assert_eq!(
            error,
            CallError::Remote {
                code: METHOD_NOT_FOUND,
                message: "no such method".into()
            }
        );
    }

    #[test]
    fn a_silent_side_times_out_and_a_closed_one_fails_at_once() {
        let (plugin_out, ide_in) = pipe();
        let (ide_out, _plugin_in) = pipe();
        let (ide, _) = Connection::new(ide_in, ide_out);

        let error = ide.call("sleep", Value::Null, Duration::from_millis(50));
        assert_eq!(error, Err(CallError::TimedOut));

        // The process exits: its stdout ends.
        drop(plugin_out);
        let waiting = thread::spawn(move || ide.call("anything", Value::Null, SECOND));
        assert_eq!(waiting.join().unwrap(), Err(CallError::Closed));
    }

    #[test]
    fn a_dropped_responder_still_answers() {
        let ((ide, _), (_plugin, requests)) = pair();
        thread::spawn(move || {
            for request in requests {
                drop(request);
            }
        });

        let error = ide.call("x", Value::Null, SECOND).unwrap_err();

        assert!(
            matches!(error, CallError::Remote { code: FAILED, .. }),
            "{error}"
        );
    }
}
