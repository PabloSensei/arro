use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use lsp_types::notification::{
    Cancel, DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, DidSaveTextDocument,
    Exit, Notification,
};
use lsp_types::request::{Request, Shutdown};
use lsp_types::{
    CancelParams, DidChangeTextDocumentParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, DidSaveTextDocumentParams, NumberOrString,
    TextDocumentContentChangeEvent, TextDocumentIdentifier, TextDocumentItem, Uri,
    VersionedTextDocumentIdentifier,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};

use crate::error::{Error, ResponseError};
use crate::framing::{encode_frame, read_frame};

/// What the server sends on its own.
///
/// Both methods run on the task that reads the server's output. They must return
/// at once: while they wait, the server's output is not drained.
pub trait ServerEvents: Send + Sync + 'static {
    fn notification(&self, method: &str, params: Value);

    fn request(&self, method: &str, params: Value) -> Result<Value, ResponseError> {
        let _ = params;
        Err(ResponseError::method_not_found(method))
    }

    /// The connection ended. Called once, after pending requests have failed.
    fn closed(&self) {}
}

type Reply = Result<Value, Error>;

struct Shared {
    /// Unbounded on purpose. A server may stop reading stdin until its own stdout
    /// is drained, when it publishes diagnostics from its only
    /// reader thread. If a full queue could stop the reader or an editor thread,
    /// both sides would wait for each other forever. Editor edits are bounded by
    /// typing, requests by their callers.
    outgoing: mpsc::UnboundedSender<Vec<u8>>,
    pending: Mutex<Pending>,
    next_id: AtomicI64,
    versions: Mutex<HashMap<Uri, i32>>,
}

/// `None` once the connection has ended.
type Pending = Option<HashMap<i64, oneshot::Sender<Reply>>>;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Shared {
    fn send(&self, message: &Value) -> Result<(), Error> {
        let frame = encode_frame(&serde_json::to_vec(message)?);
        self.outgoing.send(frame).map_err(|_| Error::Closed)
    }

    fn notify<N: Notification>(&self, params: N::Params) -> Result<(), Error> {
        self.send(&message(N::METHOD, serde_json::to_value(params)?))
    }

    fn cancel(&self, id: i64) {
        lock(&self.pending)
            .as_mut()
            .map(|pending| pending.remove(&id));
        let params = CancelParams {
            id: NumberOrString::Number(id as i32),
        };
        // A closed connection has nothing left to cancel.
        let _ = self.notify::<Cancel>(params);
    }

    fn answer(&self, id: Value, outcome: Result<Value, ResponseError>) {
        let reply = match outcome {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
            Err(error) => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {"code": error.code, "message": error.message, "data": error.data},
            }),
        };
        // The writer ended first, the reader will see the end of stream too.
        let _ = self.send(&reply);
    }

    fn deliver(&self, response: &Value) {
        let Some(id) = response.get("id").and_then(Value::as_i64) else {
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

fn message(method: &str, params: Value) -> Value {
    let mut message = json!({"jsonrpc": "2.0", "method": method});
    if !params.is_null() {
        message["params"] = params;
    }
    message
}

fn reply_of(response: &Value) -> Reply {
    let Some(error) = response.get("error").filter(|error| !error.is_null()) else {
        return Ok(response.get("result").cloned().unwrap_or(Value::Null));
    };
    let code = error.get("code").and_then(Value::as_i64).unwrap_or(0);
    if code == ResponseError::REQUEST_CANCELLED {
        return Err(Error::Cancelled);
    }
    Err(Error::Server(ResponseError {
        code,
        message: error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        data: error.get("data").cloned(),
    }))
}

/// Sends `$/cancelRequest` if the caller gave up before the reply arrived.
struct InFlight {
    shared: Arc<Shared>,
    id: Option<i64>,
}

impl Drop for InFlight {
    fn drop(&mut self) {
        if let Some(id) = self.id {
            self.shared.cancel(id);
        }
    }
}

/// A connection to a language server. Cheap to clone.
///
/// Sending never waits, so it is safe from any thread.
#[derive(Clone)]
pub struct Client {
    shared: Arc<Shared>,
}

impl Client {
    /// Starts the reader and writer tasks on the current tokio runtime.
    pub fn start<R, W>(output: R, input: W, events: Arc<dyn ServerEvents>) -> Self
    where
        R: AsyncRead + Send + Unpin + 'static,
        W: AsyncWrite + Send + Unpin + 'static,
    {
        let (outgoing, receiver) = mpsc::unbounded_channel();
        let shared = Arc::new(Shared {
            outgoing,
            pending: Mutex::new(Some(HashMap::new())),
            next_id: AtomicI64::new(1),
            versions: Mutex::new(HashMap::new()),
        });
        tokio::spawn(write_loop(input, receiver));
        tokio::spawn(read_loop(output, shared.clone(), events));
        Self { shared }
    }

    /// Sends a request. Dropping the future sends `$/cancelRequest`.
    pub async fn request<R: Request>(&self, params: R::Params) -> Result<R::Result, Error>
    where
        R::Params: Serialize,
        R::Result: DeserializeOwned,
    {
        let result = self
            .raw_request(R::METHOD, serde_json::to_value(params)?)
            .await?;
        Ok(serde_json::from_value(result)?)
    }

    /// For extensions that have no `lsp_types` definition.
    pub async fn raw_request(&self, method: &str, params: Value) -> Result<Value, Error> {
        let id = self.shared.next_id.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = oneshot::channel();
        lock(&self.shared.pending)
            .as_mut()
            .ok_or(Error::Closed)?
            .insert(id, sender);
        let mut in_flight = InFlight {
            shared: self.shared.clone(),
            id: Some(id),
        };
        let mut request = message(method, params);
        request["id"] = id.into();
        self.shared.send(&request)?;
        let reply = receiver.await.map_err(|_| Error::Closed);
        in_flight.id = None;
        reply?
    }

    pub fn notify<N: Notification>(&self, params: N::Params) -> Result<(), Error>
    where
        N::Params: Serialize,
    {
        self.shared.notify::<N>(params)
    }

    /// For extensions that have no `lsp_types` definition.
    pub fn raw_notify(&self, method: &str, params: Value) -> Result<(), Error> {
        self.shared.send(&message(method, params))
    }

    /// `shutdown` and `exit`. The server is expected to end after this.
    pub async fn shutdown(&self) -> Result<(), Error> {
        self.request::<Shutdown>(()).await?;
        self.notify::<Exit>(())
    }

    /// Opens a document at version 1.
    pub fn open(&self, uri: Uri, language_id: &str, text: String) -> Result<i32, Error> {
        let mut versions = lock(&self.shared.versions);
        if versions.contains_key(&uri) {
            return Err(Error::AlreadyOpen(uri));
        }
        let document = TextDocumentItem::new(uri.clone(), language_id.to_owned(), 1, text);
        self.notify::<DidOpenTextDocument>(DidOpenTextDocumentParams {
            text_document: document,
        })?;
        versions.insert(uri, 1);
        Ok(1)
    }

    /// Sends one batch of changes and returns the new document version.
    pub fn change(
        &self,
        uri: &Uri,
        changes: Vec<TextDocumentContentChangeEvent>,
    ) -> Result<i32, Error> {
        let mut versions = lock(&self.shared.versions);
        let Some(version) = versions.get_mut(uri) else {
            return Err(Error::NotOpen(uri.clone()));
        };
        let next = *version + 1;
        self.notify::<DidChangeTextDocument>(DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier::new(uri.clone(), next),
            content_changes: changes,
        })?;
        *version = next;
        Ok(next)
    }

    pub fn save(&self, uri: &Uri, text: Option<String>) -> Result<(), Error> {
        if !lock(&self.shared.versions).contains_key(uri) {
            return Err(Error::NotOpen(uri.clone()));
        }
        self.notify::<DidSaveTextDocument>(DidSaveTextDocumentParams {
            text_document: TextDocumentIdentifier::new(uri.clone()),
            text,
        })
    }

    pub fn close(&self, uri: &Uri) -> Result<(), Error> {
        if lock(&self.shared.versions).remove(uri).is_none() {
            return Err(Error::NotOpen(uri.clone()));
        }
        self.notify::<DidCloseTextDocument>(DidCloseTextDocumentParams {
            text_document: TextDocumentIdentifier::new(uri.clone()),
        })
    }

    /// The version of an open document.
    pub fn version(&self, uri: &Uri) -> Option<i32> {
        lock(&self.shared.versions).get(uri).copied()
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

async fn read_loop<R>(output: R, shared: Arc<Shared>, events: Arc<dyn ServerEvents>)
where
    R: AsyncRead + Send + Unpin,
{
    let mut output = BufReader::new(output);
    while let Ok(Some(frame)) = read_frame(&mut output).await {
        let Ok(mut incoming) = serde_json::from_slice::<Value>(&frame) else {
            break;
        };
        let params = incoming
            .get_mut("params")
            .map(Value::take)
            .unwrap_or(Value::Null);
        match (
            incoming.get("method").and_then(Value::as_str),
            incoming.get("id"),
        ) {
            (Some(method), Some(id)) => shared.answer(id.clone(), events.request(method, params)),
            (Some(method), None) => events.notification(method, params),
            (None, _) => shared.deliver(&incoming),
        }
    }
    shared.close();
    events.closed();
}
