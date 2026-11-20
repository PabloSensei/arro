//! The tools of the IDE over MCP: Streamable HTTP on 127.0.0.1. A token per session ties every
//! call to its session and lives in memory only. A browser never gets in: any `Origin` is refused,
//! which is the defense against CVE-2025-52882 and DNS rebinding.

use std::borrow::Cow;
use std::collections::HashMap;
use std::convert::Infallible;
use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex, RwLock};
use std::task::{Context, Poll};
use std::thread;

use axum::extract::{Extension, RawQuery, Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use futures_core::Stream;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, InitializeRequestParams,
    InitializeResult, JsonObject, ListToolsResult, PaginatedRequestParams, ServerCapabilities,
    ServerConfig, Tool, ToolAnnotations,
};
use rmcp::service::{NotificationContext, RequestContext, RoleServer};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{
    SessionId, SessionManager, StreamableHttpServerConfig, StreamableHttpService,
};
use rmcp::{ErrorData, ServerHandler};
use serde_json::Value;
use tokio::sync::oneshot;
use tool_gateway::{ClientInfo, SessionScope, ToolClass, ToolGateway, ToolSpec};

const WORKER_THREADS: usize = 2;
const TOKEN_BYTES: usize = 32;

type Tokens = RwLock<HashMap<String, SessionScope>>;

#[derive(Clone)]
struct Handler {
    gateway: Arc<ToolGateway>,
}

fn client_info(info: Option<Arc<InitializeRequestParams>>) -> Option<ClientInfo> {
    info.map(|info| ClientInfo {
        name: info.client_info.name.clone(),
        version: info.client_info.version.clone(),
    })
}

impl Handler {
    /// Notes who is on the other side of a request: the Agents tab lists the client and
    /// Disconnect closes the transport session that the request carries.
    fn note_client(&self, parts: Option<&http::request::Parts>, info: Option<ClientInfo>) {
        let (Some(parts), Some(info)) = (parts, info) else {
            return;
        };
        let Some(scope) = parts.extensions.get::<SessionScope>() else {
            return;
        };
        let connection = parts
            .headers
            .get("mcp-session-id")
            .and_then(|value| value.to_str().ok());
        match connection {
            Some(connection) => self
                .gateway
                .clients()
                .bind(&scope.session, &info, connection),
            None => self.gateway.clients().connect(&scope.session, &info),
        }
    }
}

fn tool(spec: &ToolSpec) -> Tool {
    let schema: JsonObject = match &spec.input_schema {
        Value::Object(schema) => schema.clone(),
        _ => JsonObject::new(),
    };
    let annotations = ToolAnnotations::new().read_only(spec.class == ToolClass::Read);
    Tool::new(
        Cow::Borrowed(spec.name),
        Cow::Borrowed(spec.description),
        Arc::new(schema),
    )
    .annotate(annotations)
}

impl ServerHandler for Handler {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
    }

    async fn initialize(
        &self,
        request: InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, ErrorData> {
        context.peer.set_peer_info(request.clone());
        let parts = context.extensions.get::<http::request::Parts>();
        self.note_client(parts, client_info(Some(Arc::new(request.clone()))));
        self.negotiate_initialize(&request)
    }

    async fn on_initialized(&self, context: NotificationContext<RoleServer>) {
        let parts = context.extensions.get::<http::request::Parts>();
        self.note_client(parts, client_info(context.peer.peer_info()));
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        // The Exposed Tools table of the window decides what this client sees: a tool
        // that is off, a router-only tool and the router-only mode itself.
        let scope = context
            .extensions
            .get::<http::request::Parts>()
            .and_then(|parts| parts.extensions.get::<SessionScope>())
            .cloned();
        let tools = match scope {
            Some(scope) => self.gateway.visible_tools(&scope),
            None => self.gateway.tools().to_vec(),
        };
        let tools = tools.iter().map(tool).collect();
        Ok(ListToolsResult::with_all_items(tools))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let parts = context.extensions.get::<http::request::Parts>();
        let scope = parts.and_then(|parts| parts.extensions.get::<SessionScope>());
        let Some(scope) = scope.cloned() else {
            return Err(ErrorData::invalid_request("no session", None));
        };
        let gateway = self.gateway.clone();
        // The name the client gave itself in `initialize` names it in the audit log and
        // in the Agents tab, and the transport session lets Disconnect close it.
        let client = client_info(context.peer.peer_info());
        self.note_client(parts, client.clone());
        let name = request.name.into_owned();
        let input = Value::Object(request.arguments.unwrap_or_default());
        // A service may take its time, and the transport threads have other sessions to serve.
        let work = move || gateway.call_as(client.as_ref(), &scope, &name, &input);
        let result = tokio::task::spawn_blocking(work).await;
        let result = result.map_err(|error| ErrorData::internal_error(error.to_string(), None))?;
        let answer = match result {
            Ok(text) => CallToolResult::success(vec![ContentBlock::text(text)]),
            Err(error) => CallToolResult::error(vec![ContentBlock::text(error.to_string())]),
        };
        Ok(answer.into())
    }
}

async fn guard(State(tokens): State<Arc<Tokens>>, mut request: Request, next: Next) -> Response {
    if request.headers().contains_key(header::ORIGIN) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let given = request.headers().get(header::AUTHORIZATION);
    let given = given.and_then(|value| value.to_str().ok());
    let token = given.and_then(|value| value.strip_prefix("Bearer "));
    let scope = token.and_then(|token| tokens.read().unwrap().get(token).cloned());
    let Some(scope) = scope else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    request.extensions_mut().insert(scope);
    next.run(request).await
}

/// The SSE transport of the clients that still speak it: one stream of events per
/// connection and one POST endpoint that feeds it. The switch of Settings still decides
/// what a call sees, as it does over Streamable HTTP.
#[derive(Clone)]
struct SseState {
    gateway: Arc<ToolGateway>,
    sessions: Arc<Mutex<HashMap<String, tokio::sync::mpsc::UnboundedSender<String>>>>,
}

impl SseState {
    fn new(gateway: Arc<ToolGateway>) -> Self {
        Self {
            gateway,
            sessions: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

/// The text of the first event of a stream and the events of the POSTs after it.
struct SseStream {
    first: Option<String>,
    receiver: tokio::sync::mpsc::UnboundedReceiver<String>,
}

impl Stream for SseStream {
    type Item = Result<String, Infallible>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if let Some(first) = self.first.take() {
            return Poll::Ready(Some(Ok(first)));
        }
        match self.receiver.poll_recv(cx) {
            Poll::Ready(Some(text)) => Poll::Ready(Some(Ok(text))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

fn session_id() -> io::Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|error| io::Error::other(error.to_string()))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

async fn sse_connect(
    State(state): State<SseState>,
    Extension(scope): Extension<SessionScope>,
) -> Response {
    let Ok(id) = session_id() else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    state
        .sessions
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .insert(id.clone(), sender);
    // The client posts every request to the address of this event, with its token.
    let first = format!("event: endpoint\ndata: /message?sessionId={id}\n\n");
    let stream = SseStream {
        first: Some(first),
        receiver,
    };
    let _ = scope;
    Response::builder()
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .body(axum::body::Body::from_stream(stream))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

async fn sse_message(
    State(state): State<SseState>,
    Extension(scope): Extension<SessionScope>,
    RawQuery(query): RawQuery,
    request: Request,
) -> Response {
    let Some(id) = query.and_then(|query| query_value(&query, "sessionId")) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let sender = state
        .sessions
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .get(&id)
        .cloned();
    let Some(sender) = sender else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Ok(bytes) = axum::body::to_bytes(request.into_body(), 1_000_000).await else {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    };
    let Ok(body) = serde_json::from_slice::<Value>(&bytes) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let gateway = state.gateway.clone();
    let answer = tokio::task::spawn_blocking(move || sse_answer(&gateway, &scope, &body)).await;
    if let Ok(Some(answer)) = answer {
        let event = format!("event: message\ndata: {answer}\n\n");
        if sender.send(event).is_err() {
            state
                .sessions
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .remove(&id);
        }
    }
    StatusCode::ACCEPTED.into_response()
}

/// The value of one key of a query string, for the session id of an SSE POST.
fn query_value(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (name, value) = pair.split_once('=')?;
        (name == key).then(|| value.to_string())
    })
}

/// The JSON-RPC answer of one SSE request. `None` for a notification, which has none.
fn sse_answer(gateway: &ToolGateway, scope: &SessionScope, body: &Value) -> Option<Value> {
    let id = body.get("id").cloned();
    let method = body
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let params = body
        .get("params")
        .cloned()
        .unwrap_or_else(|| Value::Object(Default::default()));
    let id = id?;
    let result = match method {
        "initialize" => {
            let protocol = params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or("2025-11-25");
            Ok(serde_json::json!({
                "protocolVersion": protocol,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "arro", "version": env!("CARGO_PKG_VERSION")},
            }))
        }
        "ping" => Ok(serde_json::json!({})),
        "tools/list" => {
            let tools: Vec<Value> = gateway
                .visible_tools(scope)
                .iter()
                .map(|spec| {
                    serde_json::json!({
                        "name": spec.name,
                        "description": spec.description,
                        "inputSchema": spec.input_schema,
                    })
                })
                .collect();
            Ok(serde_json::json!({"tools": tools}))
        }
        "tools/call" => {
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let arguments = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| Value::Object(Default::default()));
            match gateway.call(scope, name, &arguments) {
                Ok(text) => Ok(serde_json::json!({
                    "content": [{"type": "text", "text": text}],
                    "isError": false,
                })),
                Err(error) => Ok(serde_json::json!({
                    "content": [{"type": "text", "text": error.to_string()}],
                    "isError": true,
                })),
            }
        }
        other => Err((-32601, format!("unknown method {other}"))),
    };
    match result {
        Ok(result) => Some(serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result,
        })),
        Err((code, message)) => Some(serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {"code": code, "message": message},
        })),
    }
}

/// Serves on a runtime of its own until dropped.
pub struct McpServer {
    port: u16,
    tokens: Arc<Tokens>,
    /// The transport sessions of the clients, so Disconnect can close one.
    sessions: Arc<LocalSessionManager>,
    runtime: tokio::runtime::Handle,
    stop: Option<oneshot::Sender<()>>,
}

impl McpServer {
    pub fn start(gateway: Arc<ToolGateway>) -> io::Result<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(WORKER_THREADS)
            .thread_name("mcp-server")
            .enable_all()
            .build()?;
        let listener = runtime.block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))?;
        let port = listener.local_addr()?.port();
        let tokens = Arc::new(Tokens::default());
        let sessions = Arc::new(LocalSessionManager::default());

        // Nothing is on the allowlist, so rmcp refuses every Origin too.
        let config = StreamableHttpServerConfig::default().enforce_origin_validation();
        let handler = Handler { gateway };
        let sse = SseState::new(handler.gateway.clone());
        let service =
            StreamableHttpService::new(move || Ok(handler.clone()), sessions.clone(), config);
        let router = axum::Router::new()
            .route("/sse", get(sse_connect))
            .route("/message", post(sse_message))
            .with_state(sse)
            .nest_service("/mcp", service)
            .layer(middleware::from_fn_with_state(tokens.clone(), guard));

        let (stop, stopped) = oneshot::channel::<()>();
        let handle = runtime.handle().clone();
        thread::Builder::new()
            .name("mcp-listener".to_string())
            .spawn(move || {
                let server = axum::serve(listener, router)
                    .with_graceful_shutdown(async { drop(stopped.await) });
                let _ = runtime.block_on(server.into_future());
                runtime.shutdown_background();
            })?;
        Ok(Self {
            port,
            tokens,
            sessions,
            runtime: handle,
            stop: Some(stop),
        })
    }

    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}/mcp", self.port)
    }

    /// A fresh token bound to the session. It works until revoked.
    pub fn issue_token(&self, scope: SessionScope) -> io::Result<String> {
        let mut bytes = [0u8; TOKEN_BYTES];
        getrandom::fill(&mut bytes).map_err(|error| io::Error::other(error.to_string()))?;
        let token: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        self.tokens.write().unwrap().insert(token.clone(), scope);
        Ok(token)
    }

    pub fn revoke(&self, token: &str) {
        self.tokens.write().unwrap().remove(token);
    }

    /// Disconnect: the transport session of one client ends on the server, while the
    /// tokens of the window and of the other clients stay.
    pub fn disconnect(&self, connection: &str) {
        let sessions = self.sessions.clone();
        let id: SessionId = SessionId::from(connection);
        self.runtime.spawn(async move {
            let _ = sessions.close_session(&id).await;
        });
    }
}

impl Drop for McpServer {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}
