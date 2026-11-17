//! The tools of the IDE over MCP: Streamable HTTP on 127.0.0.1. A token per session ties every
//! call to its session and lives in memory only. A browser never gets in: any `Origin` is refused,
//! which is the defense against CVE-2025-52882 and DNS rebinding.

use std::borrow::Cow;
use std::collections::HashMap;
use std::io;
use std::sync::{Arc, RwLock};
use std::thread;

use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, JsonObject,
    ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
    ToolAnnotations,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{ErrorData, ServerHandler};
use serde_json::Value;
use tokio::sync::oneshot;
use tool_gateway::{SessionScope, ToolClass, ToolGateway, ToolSpec};

const WORKER_THREADS: usize = 2;
const TOKEN_BYTES: usize = 32;

type Tokens = RwLock<HashMap<String, SessionScope>>;

#[derive(Clone)]
struct Handler {
    gateway: Arc<ToolGateway>,
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

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let tools = self.gateway.tools().iter().map(tool).collect();
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
        let name = request.name.into_owned();
        let input = Value::Object(request.arguments.unwrap_or_default());
        // A service may take its time, and the transport threads have other sessions to serve.
        let work = move || gateway.call(&scope, &name, &input);
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

/// Serves on a runtime of its own until dropped.
pub struct McpServer {
    port: u16,
    tokens: Arc<Tokens>,
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

        // Nothing is on the allowlist, so rmcp refuses every Origin too.
        let config = StreamableHttpServerConfig::default().enforce_origin_validation();
        let handler = Handler { gateway };
        let service = StreamableHttpService::new(
            move || Ok(handler.clone()),
            Arc::new(LocalSessionManager::default()),
            config,
        );
        let router = axum::Router::new()
            .nest_service("/mcp", service)
            .layer(middleware::from_fn_with_state(tokens.clone(), guard));

        let (stop, stopped) = oneshot::channel::<()>();
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
}

impl Drop for McpServer {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}
