//! A connection to a debug adapter: requests with a deadline, events in a channel,
//! reverse requests refused.

use std::collections::HashMap;
use std::fmt;
use std::io;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};
use tokio::time::timeout;

use crate::framing::{encode_frame, read_frame};

/// A reply that does not come in this time means a stuck adapter, not a slow one.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, PartialEq)]
pub struct ResponseError {
    pub command: String,
    pub message: String,
    pub body: Option<Value>,
}

#[derive(Debug)]
pub enum Error {
    /// The connection ended, this request will never be answered.
    Closed,
    /// The adapter did not answer in time.
    TimedOut,
    /// The adapter answered that the request failed.
    Server(ResponseError),
    /// A message that does not fit the protocol.
    Decode(serde_json::Error),
    /// The socket could not be opened.
    Io(io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Closed => f.write_str("connection closed"),
            Self::TimedOut => f.write_str("the adapter did not answer in time"),
            Self::Server(error) => write!(f, "{} failed: {}", error.command, error.message),
            Self::Decode(error) => write!(f, "bad message: {error}"),
            Self::Io(error) => write!(f, "connection failed: {error}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<serde_json::Error> for Error {
    fn from(error: serde_json::Error) -> Self {
        Self::Decode(error)
    }
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// What the adapter sends on its own. Unknown events stay available in `Other`.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// The adapter is ready for the launch arguments.
    Initialized,
    Stopped {
        reason: String,
        thread_id: Option<i64>,
        all_threads: bool,
    },
    Continued {
        thread_id: Option<i64>,
        all_threads: bool,
    },
    Thread {
        reason: String,
        thread_id: i64,
    },
    Output {
        category: String,
        text: String,
    },
    Breakpoint {
        reason: String,
        body: Value,
    },
    Terminated,
    Exited {
        code: i64,
    },
    Process {
        name: String,
        system_process_id: Option<u32>,
    },
    Other {
        event: String,
        body: Value,
    },
    /// The connection ended. The last event of the channel.
    Closed,
}

impl Event {
    fn parse(name: &str, body: Value) -> Self {
        let number = |key: &str| body.get(key).and_then(Value::as_i64);
        let flag = |key: &str| body.get(key).and_then(Value::as_bool).unwrap_or(false);
        match name {
            "initialized" => Self::Initialized,
            "stopped" => Self::Stopped {
                reason: text(&body, "reason"),
                thread_id: number("threadId"),
                all_threads: flag("allThreadsStopped"),
            },
            "continued" => Self::Continued {
                thread_id: number("threadId"),
                all_threads: flag("allThreadsContinued"),
            },
            "thread" => Self::Thread {
                reason: text(&body, "reason"),
                thread_id: number("threadId").unwrap_or(0),
            },
            "output" => Self::Output {
                category: text(&body, "category"),
                text: body
                    .get("output")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            },
            "breakpoint" => Self::Breakpoint {
                reason: text(&body, "reason"),
                body,
            },
            "terminated" => Self::Terminated,
            "exited" => Self::Exited {
                code: number("exitCode").unwrap_or(0),
            },
            "process" => Self::Process {
                name: text(&body, "name"),
                system_process_id: number("systemProcessId").and_then(|id| u32::try_from(id).ok()),
            },
            other => Self::Other {
                event: other.to_owned(),
                body,
            },
        }
    }
}

fn text(body: &Value, key: &str) -> String {
    body.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

type Reply = Result<Value, Error>;

struct Shared {
    outgoing: mpsc::UnboundedSender<Vec<u8>>,
    pending: Mutex<Option<HashMap<i64, oneshot::Sender<Reply>>>>,
    next_seq: AtomicI64,
}

impl Shared {
    fn send(&self, message: &Value) -> Result<(), Error> {
        let frame = encode_frame(&serde_json::to_vec(message)?);
        self.outgoing.send(frame).map_err(|_| Error::Closed)
    }

    fn answer(&self, request_seq: &Value, message: &str) {
        let reply = json!({
            "seq": 0,
            "type": "response",
            "request_seq": request_seq,
            "success": false,
            "command": "",
            "message": message,
        });
        // The writer ended first; the reader will see the end of stream too.
        let _ = self.send(&reply);
    }

    fn deliver(&self, response: &Value) {
        let Some(id) = response.get("request_seq").and_then(Value::as_i64) else {
            return;
        };
        let Some(sender) = lock(&self.pending)
            .as_mut()
            .and_then(|pending| pending.remove(&id))
        else {
            return;
        };
        let _ = sender.send(reply_of(response));
    }

    fn close(&self) {
        let pending = lock(&self.pending).take().unwrap_or_default();
        for sender in pending.into_values() {
            let _ = sender.send(Err(Error::Closed));
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn reply_of(response: &Value) -> Reply {
    if response
        .get("success")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Ok(response.get("body").cloned().unwrap_or(Value::Null));
    }
    Err(Error::Server(ResponseError {
        command: text(response, "command"),
        message: text(response, "message"),
        body: response.get("body").cloned(),
    }))
}

/// Sends a cancel-free request: the adapter protocol has no cancellation.
struct InFlight {
    shared: Arc<Shared>,
    seq: i64,
}

impl Drop for InFlight {
    fn drop(&mut self) {
        lock(&self.shared.pending)
            .as_mut()
            .map(|pending| pending.remove(&self.seq));
    }
}

/// A connection to a debug adapter. Cheap to clone.
#[derive(Clone)]
pub struct Client {
    shared: Arc<Shared>,
}

impl Client {
    /// Opens a TCP connection to an adapter listening on `address`.
    pub async fn connect(address: &str) -> Result<(Self, mpsc::UnboundedReceiver<Event>), Error> {
        let stream = TcpStream::connect(address).await?;
        stream.set_nodelay(true).ok();
        let (output, input) = tokio::io::split(stream);
        Ok(Self::start(output, input))
    }

    /// Starts the reader and writer tasks on the current runtime. `input` is what the
    /// adapter reads, `output` what it writes.
    pub fn start<R, W>(output: R, input: W) -> (Self, mpsc::UnboundedReceiver<Event>)
    where
        R: AsyncRead + Send + Unpin + 'static,
        W: AsyncWrite + Send + Unpin + 'static,
    {
        let (outgoing, receiver) = mpsc::unbounded_channel();
        let (events, incoming) = mpsc::unbounded_channel();
        let shared = Arc::new(Shared {
            outgoing,
            pending: Mutex::new(Some(HashMap::new())),
            next_seq: AtomicI64::new(1),
        });
        tokio::spawn(write_loop(input, receiver));
        tokio::spawn(read_loop(output, shared.clone(), events));
        (Self { shared }, incoming)
    }

    /// Sends a request and waits for its answer with the default deadline.
    pub async fn request(&self, command: &str, arguments: Value) -> Result<Value, Error> {
        self.request_with_timeout(command, arguments, REQUEST_TIMEOUT)
            .await
    }

    pub async fn request_with_timeout(
        &self,
        command: &str,
        arguments: Value,
        limit: Duration,
    ) -> Result<Value, Error> {
        let seq = self.shared.next_seq.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = oneshot::channel();
        lock(&self.shared.pending)
            .as_mut()
            .ok_or(Error::Closed)?
            .insert(seq, sender);
        let in_flight = InFlight {
            shared: self.shared.clone(),
            seq,
        };
        let message = json!({
            "seq": seq,
            "type": "request",
            "command": command,
            "arguments": arguments,
        });
        let sent = self.shared.send(&message);
        if let Err(error) = sent {
            drop(in_flight);
            return Err(error);
        }
        let reply = timeout(limit, receiver)
            .await
            .map_err(|_| Error::TimedOut)
            .and_then(|reply| reply.map_err(|_| Error::Closed));
        drop(in_flight);
        reply?
    }
}

async fn write_loop<W>(input: W, mut frames: mpsc::UnboundedReceiver<Vec<u8>>)
where
    W: AsyncWrite + Send + Unpin,
{
    let mut input = input;
    while let Some(frame) = frames.recv().await {
        if input.write_all(&frame).await.is_err() || input.flush().await.is_err() {
            return;
        }
    }
}

async fn read_loop<R>(output: R, shared: Arc<Shared>, events: mpsc::UnboundedSender<Event>)
where
    R: AsyncRead + Send + Unpin,
{
    let mut output = BufReader::new(output);
    while let Ok(Some(frame)) = read_frame(&mut output).await {
        let Ok(message) = serde_json::from_slice::<Value>(&frame) else {
            continue;
        };
        match message.get("type").and_then(Value::as_str) {
            Some("response") => shared.deliver(&message),
            Some("event") => {
                let name = message
                    .get("event")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let body = message.get("body").cloned().unwrap_or(Value::Null);
                if events.send(Event::parse(name, body)).is_err() {
                    return;
                }
            }
            Some("request") => {
                // Only reverse requests would come this way, and this client answers none.
                let seq = message.get("seq").cloned().unwrap_or(Value::Null);
                shared.answer(&seq, "reverse requests are not supported");
            }
            _ => {}
        }
    }
    shared.close();
    let _ = events.send(Event::Closed);
}

#[cfg(test)]
mod tests {
    use tokio::io::{
        AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader,
    };

    use super::*;
    use crate::framing::encode_frame;

    async fn read_message(stream: &mut (impl AsyncBufRead + Unpin)) -> Value {
        let mut length = 0;
        let mut line = String::new();
        loop {
            line.clear();
            stream.read_line(&mut line).await.unwrap();
            if line.trim().is_empty() {
                break;
            }
            if let Some(value) = line.trim().strip_prefix("Content-Length: ") {
                length = value.parse().unwrap();
            }
        }
        let mut body = vec![0; length];
        stream.read_exact(&mut body).await.unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    async fn write_message(stream: &mut (impl AsyncWrite + Unpin), message: Value) {
        stream
            .write_all(&encode_frame(message.to_string().as_bytes()))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn requests_carry_a_sequence_and_take_the_answer() {
        let (ours, theirs) = tokio::io::duplex(4096);
        let (output, input) = tokio::io::split(theirs);
        let (client, _events) = Client::start(output, input);
        let far = tokio::spawn(async move {
            let mut far = BufReader::new(ours);
            let request = read_message(&mut far).await;
            assert_eq!(request["type"], "request");
            assert_eq!(request["command"], "threads");
            let seq = request["seq"].as_i64().unwrap();
            write_message(
                &mut far,
                json!({
                    "seq": 1,
                    "type": "response",
                    "request_seq": seq,
                    "success": true,
                    "command": "threads",
                    "body": {"threads": []},
                }),
            )
            .await;
        });
        let answer = client.request("threads", json!({})).await.unwrap();
        far.await.unwrap();
        assert_eq!(answer["threads"], json!([]));
    }

    #[tokio::test]
    async fn a_failed_request_is_an_error_and_a_silent_one_times_out() {
        let (ours, theirs) = tokio::io::duplex(4096);
        let (output, input) = tokio::io::split(theirs);
        let (client, _events) = Client::start(output, input);
        let far = tokio::spawn(async move {
            let mut far = BufReader::new(ours);
            let request = read_message(&mut far).await;
            let seq = request["seq"].as_i64().unwrap();
            write_message(
                &mut far,
                json!({
                    "seq": 1,
                    "type": "response",
                    "request_seq": seq,
                    "success": false,
                    "command": "attach",
                    "message": "no port",
                }),
            )
            .await;
            // The second request stays unanswered while this side is alive.
            tokio::time::sleep(Duration::from_secs(1)).await;
        });
        let failed = client.request("attach", json!({})).await;
        assert!(matches!(failed, Err(Error::Server(_))));
        let silent = client
            .request_with_timeout("threads", json!({}), Duration::from_millis(50))
            .await;
        assert!(matches!(silent, Err(Error::TimedOut)));
        far.await.unwrap();
    }

    #[tokio::test]
    async fn events_reach_the_channel_and_reverse_requests_are_refused() {
        let (ours, theirs) = tokio::io::duplex(4096);
        let (output, input) = tokio::io::split(theirs);
        let (_client, mut events) = Client::start(output, input);
        let far = tokio::spawn(async move {
            let mut far = BufReader::new(ours);
            write_message(
                &mut far,
                json!({"seq": 1, "type": "event", "event": "initialized"}),
            )
            .await;
            write_message(
                &mut far,
                json!({
                    "seq": 2,
                    "type": "event",
                    "event": "stopped",
                    "body": {"reason": "breakpoint", "threadId": 3, "allThreadsStopped": true},
                }),
            )
            .await;
            write_message(
                &mut far,
                json!({"seq": 3, "type": "request", "command": "runInTerminal"}),
            )
            .await;
            read_message(&mut far).await
        });
        assert_eq!(events.recv().await, Some(Event::Initialized));
        assert_eq!(
            events.recv().await,
            Some(Event::Stopped {
                reason: "breakpoint".into(),
                thread_id: Some(3),
                all_threads: true,
            })
        );
        let refusal = far.await.unwrap();
        assert_eq!(refusal["type"], "response");
        assert_eq!(refusal["success"], false);
    }

    #[tokio::test]
    async fn a_closed_stream_ends_the_channel() {
        let (ours, theirs) = tokio::io::duplex(4096);
        let (output, input) = tokio::io::split(theirs);
        let (_client, mut events) = Client::start(output, input);
        drop(ours);
        assert_eq!(events.recv().await, Some(Event::Closed));
    }

    #[test]
    fn unknown_events_stay_available() {
        let event = Event::parse("memory", json!({"x": 1}));
        assert!(matches!(event, Event::Other { event, .. } if event == "memory"));
    }
}
