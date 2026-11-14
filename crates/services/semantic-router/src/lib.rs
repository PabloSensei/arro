//! Sends semantic requests where they belong and drops answers that came too late.
//!
//! M5a has one mode, `Lsp`. The rules that do not depend on the mode are here:
//! a request remembers the document version it was asked for, one completion is
//! in flight per document, and the last one wins.

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use base::perf;
use lang_api::FileChange;
mod providers;

use lsp_client::lsp_types::notification::{Notification, PublishDiagnostics};
use lsp_client::lsp_types::request::{
    CallHierarchyIncomingCalls, CallHierarchyOutgoingCalls, CallHierarchyPrepare,
    CodeActionRequest, CodeLensRefresh, CodeLensRequest, CodeLensResolve, Completion,
    DocumentHighlightRequest, DocumentSymbolRequest, Formatting, GotoDefinition,
    GotoImplementation, GotoImplementationParams, GotoTypeDefinition, GotoTypeDefinitionParams,
    HoverRequest, InlayHintRefreshRequest, InlayHintRequest, RangeFormatting, References, Rename,
    Request, SemanticTokensFullRequest, SemanticTokensRangeRequest, SignatureHelpRequest,
    TypeHierarchyPrepare, TypeHierarchySubtypes, TypeHierarchySupertypes, WorkspaceSymbolRequest,
};
use lsp_client::lsp_types::{
    CallHierarchyIncomingCallsParams, CallHierarchyItem, CallHierarchyOutgoingCallsParams,
    CallHierarchyPrepareParams, CodeActionContext, CodeActionKind, CodeActionParams,
    CodeActionResponse, CodeLens, CodeLensParams, CompletionParams, CompletionResponse, Diagnostic,
    DocumentFormattingParams, DocumentHighlight, DocumentHighlightParams,
    DocumentRangeFormattingParams, DocumentSymbolParams, DocumentSymbolResponse, FormattingOptions,
    GotoDefinitionParams, GotoDefinitionResponse, Hover, HoverParams, InlayHint, InlayHintParams,
    Location, PartialResultParams, Position, PublishDiagnosticsParams, Range, ReferenceContext,
    ReferenceParams, RenameParams, SemanticTokens, SemanticTokensParams, SemanticTokensRangeParams,
    SemanticTokensRangeResult, SemanticTokensResult, SignatureHelp, SignatureHelpParams,
    TextDocumentContentChangeEvent, TextDocumentIdentifier, TextDocumentPositionParams, TextEdit,
    TypeHierarchyItem, TypeHierarchyPrepareParams, TypeHierarchySubtypesParams,
    TypeHierarchySupertypesParams, Uri, WorkDoneProgressParams, WorkspaceEdit,
    WorkspaceSymbolParams, WorkspaceSymbolResponse,
};
use lsp_client::{Client, Error as LspError, ServerEvents};
use serde_json::Value;
use sidecar::Status;
use tokio::sync::watch;
use tokio::time::{sleep, timeout};

pub use providers::{
    GenerateRequests, LspProviders, PromptSource, RefactorSteps, semantic_tokens_from_lsp,
    workspace_change,
};

/// Where a capability is answered. Native modes arrive with M12.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Lsp,
}

/// Keystrokes closer together than this ask once.
const COALESCE: Duration = Duration::from_millis(60);

/// A hung server fails the request, it never stalls the caller for good.
const REQUEST_LIMIT: Duration = Duration::from_secs(30);

#[derive(Debug)]
pub enum RouteError {
    /// The server is not up. The editor goes on without an answer.
    NotReady,
    /// The document changed since the question was asked.
    Stale,
    /// A newer request of this document replaced this one.
    Superseded,
    /// The server did not answer in time.
    TimedOut,
    Server(LspError),
}

impl fmt::Display for RouteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotReady => f.write_str("server is not ready"),
            Self::Stale => f.write_str("the document changed"),
            Self::Superseded => f.write_str("a newer request replaced this one"),
            Self::TimedOut => f.write_str("the server did not answer in time"),
            Self::Server(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for RouteError {}

impl From<LspError> for RouteError {
    fn from(error: LspError) -> Self {
        Self::Server(error)
    }
}

/// A spot in a document as the asker saw it.
#[derive(Clone, Debug)]
pub struct Place {
    pub uri: Uri,
    /// The buffer version the position belongs to.
    pub version: i32,
    pub position: Position,
}

impl Place {
    pub(crate) fn text_document_position(&self) -> TextDocumentPositionParams {
        TextDocumentPositionParams {
            text_document: TextDocumentIdentifier::new(self.uri.clone()),
            position: self.position,
        }
    }
}

struct Document {
    language: String,
    version: i32,
    text: Arc<str>,
    completion: Arc<watch::Sender<u64>>,
    /// A newer semantic token request replaces this one.
    tokens: Arc<watch::Sender<u64>>,
    /// A newer code lens request replaces this one.
    lenses_turns: Arc<watch::Sender<u64>>,
    /// The latest answer of `textDocument/inlayHint`, moved along with the edits.
    hints: Option<lsp_client::hints::InlayHints>,
    /// The latest answer of `textDocument/codeLens`, moved along with the edits.
    lenses: Option<lsp_client::CodeLenses>,
}

#[derive(Default)]
struct Documents {
    open: HashMap<Uri, Document>,
    /// The server generation that has received all of `open`.
    synced: u64,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Receives what the server publishes. Give it to the sidecar as the consumer.
#[derive(Default)]
pub struct Published {
    diagnostics: Mutex<HashMap<Uri, Vec<Diagnostic>>>,
    revision: AtomicU64,
    changed: Option<watch::Sender<u64>>,
    /// A server that changed what hints it would give asks for them again.
    inlay_revision: AtomicU64,
    inlay_changed: Option<watch::Sender<u64>>,
    /// A server that changed its code lenses asks for them again.
    lens_revision: AtomicU64,
    lens_changed: Option<watch::Sender<u64>>,
}

impl Published {
    pub fn new() -> (Arc<Self>, watch::Receiver<u64>) {
        let (sender, receiver) = watch::channel(0);
        let (inlay_changed, _) = watch::channel(0);
        let (lens_changed, _) = watch::channel(0);
        let published = Self {
            changed: Some(sender),
            inlay_changed: Some(inlay_changed),
            lens_changed: Some(lens_changed),
            ..Self::default()
        };
        (Arc::new(published), receiver)
    }

    /// The latest diagnostics of a document, whichever version they describe.
    pub fn diagnostics(&self, uri: &Uri) -> Vec<Diagnostic> {
        lock(&self.diagnostics)
            .get(uri)
            .cloned()
            .unwrap_or_default()
    }

    /// Every publication of a new hint set bumps this. The caller asks the server for
    /// the hints of the visible range again.
    pub fn inlay_refresh(&self) -> watch::Receiver<u64> {
        match &self.inlay_changed {
            Some(changed) => changed.subscribe(),
            None => watch::channel(0).1,
        }
    }

    fn inlay_hints_changed(&self) {
        let revision = self.inlay_revision.fetch_add(1, Ordering::Relaxed) + 1;
        if let Some(changed) = &self.inlay_changed {
            changed.send_replace(revision);
        }
    }

    /// Every refresh of the code lenses bumps this. The caller asks the server again
    /// for the document.
    pub fn code_lens_refresh(&self) -> watch::Receiver<u64> {
        match &self.lens_changed {
            Some(changed) => changed.subscribe(),
            None => watch::channel(0).1,
        }
    }

    fn code_lenses_changed(&self) {
        let revision = self.lens_revision.fetch_add(1, Ordering::Relaxed) + 1;
        if let Some(changed) = &self.lens_changed {
            changed.send_replace(revision);
        }
    }

    /// The latest diagnostics of every document the server has published.
    pub fn all_diagnostics(&self) -> Vec<(Uri, Vec<Diagnostic>)> {
        let mut all: Vec<(Uri, Vec<Diagnostic>)> = lock(&self.diagnostics)
            .iter()
            .map(|(uri, list)| (uri.clone(), list.clone()))
            .collect();
        all.sort_by(|left, right| left.0.as_str().cmp(right.0.as_str()));
        all
    }
}

impl ServerEvents for Published {
    fn notification(&self, method: &str, params: Value) {
        if method == InlayHintRefreshRequest::METHOD {
            self.inlay_hints_changed();
            return;
        }
        if method == CodeLensRefresh::METHOD {
            self.code_lenses_changed();
            return;
        }
        if method != PublishDiagnostics::METHOD {
            return;
        }
        let Ok(params) = serde_json::from_value::<PublishDiagnosticsParams>(params) else {
            return;
        };
        lock(&self.diagnostics).insert(params.uri, params.diagnostics);
        let revision = self.revision.fetch_add(1, Ordering::Relaxed) + 1;
        if let Some(changed) = &self.changed {
            changed.send_replace(revision);
        }
    }

    fn request(&self, method: &str, _params: Value) -> Result<Value, lsp_client::ResponseError> {
        if method == InlayHintRefreshRequest::METHOD {
            self.inlay_hints_changed();
            return Ok(Value::Null);
        }
        if method == CodeLensRefresh::METHOD {
            self.code_lenses_changed();
            return Ok(Value::Null);
        }
        Err(lsp_client::ResponseError::method_not_found(method))
    }
}

/// The semantic side of one project.
pub struct Router {
    status: watch::Receiver<Status>,
    documents: Mutex<Documents>,
}

impl Router {
    pub fn new(status: watch::Receiver<Status>) -> Arc<Self> {
        let mut changes = status.clone();
        let router = Arc::new(Self {
            status,
            documents: Mutex::default(),
        });
        let weak = Arc::downgrade(&router);
        tokio::spawn(async move {
            loop {
                let Some(router) = weak.upgrade() else {
                    return;
                };
                router.replay();
                drop(router);
                if changes.changed().await.is_err() {
                    return;
                }
            }
        });
        router
    }

    /// Waits for a server that has imported the project and is not the `previous`
    /// generation: answers that need the libraries of the project, such as parameter hints, are
    /// complete only then. `None` when the server is gone for good.
    pub async fn ready_after(&self, previous: Option<u64>) -> Option<u64> {
        let mut status = self.status.clone();
        let ready = status
            .wait_for(|status| {
                status.state == sidecar::State::Ready
                    && status.client.is_some()
                    && Some(status.generation) != previous
            })
            .await
            .ok()?;
        Some(ready.generation)
    }

    fn live_client(&self) -> Option<(Client, u64)> {
        let status = self.status.borrow();
        status
            .client
            .clone()
            .map(|client| (client, status.generation))
    }

    /// A new server knows nothing, so it receives every open document.
    fn replay(&self) {
        let Some((client, generation)) = self.live_client() else {
            return;
        };
        let mut documents = lock(&self.documents);
        if documents.synced == generation {
            return;
        }
        for (uri, document) in &documents.open {
            let text = document.text.to_string();
            // Ends with the connection, the next generation replays again.
            let _ = client.open(uri.clone(), &document.language, text);
        }
        documents.synced = generation;
    }

    pub fn open(&self, uri: Uri, language: &str, version: i32, text: Arc<str>) {
        let (completion, _) = watch::channel(0);
        let (tokens, _) = watch::channel(0);
        let (lenses_turns, _) = watch::channel(0);
        let document = Document {
            language: language.to_owned(),
            version,
            text: text.clone(),
            completion: Arc::new(completion),
            tokens: Arc::new(tokens),
            lenses_turns: Arc::new(lenses_turns),
            hints: None,
            lenses: None,
        };
        let mut documents = lock(&self.documents);
        documents.open.insert(uri.clone(), document);
        if let Some((client, generation)) = self.live_client()
            && documents.synced == generation
        {
            let _ = client.open(uri, language, text.to_string());
        }
    }

    /// Records the new text and forwards the changes that lead to it.
    pub fn change(
        &self,
        uri: &Uri,
        version: i32,
        text: Arc<str>,
        changes: Vec<TextDocumentContentChangeEvent>,
    ) {
        let mut documents = lock(&self.documents);
        let synced = documents.synced;
        let Some(document) = documents.open.get_mut(uri) else {
            return;
        };
        document.version = version;
        document.text = text;
        if let Some(hints) = &mut document.hints {
            hints.apply(version, &changes);
        }
        if let Some(lenses) = &mut document.lenses {
            lenses.apply(version, &changes);
        }
        if let Some((client, generation)) = self.live_client()
            && synced == generation
        {
            let _ = client.change(uri, changes);
        }
    }

    pub fn close(&self, uri: &Uri) {
        let mut documents = lock(&self.documents);
        documents.open.remove(uri);
        if let Some((client, generation)) = self.live_client()
            && documents.synced == generation
        {
            let _ = client.close(uri);
        }
    }

    pub fn save(&self, uri: &Uri) {
        let documents = lock(&self.documents);
        if let Some((client, generation)) = self.live_client()
            && documents.synced == generation
        {
            let _ = client.save(uri, None);
        }
    }

    /// A file changed on disk outside the open documents. The server registered the
    /// watched files it cares about, such as build scripts.
    pub fn file_changed(&self, uri: &str) {
        self.file_changed_as(uri, FileChange::Changed);
    }

    /// The same with what happened to the file, so that the server learns about a build
    /// script of a new module or one that left the project.
    pub fn file_changed_as(&self, uri: &str, change: FileChange) {
        if let Some((client, _)) = self.live_client() {
            let changes = serde_json::json!({
                "changes": [{"uri": uri, "type": watched_file_type(change)}]
            });
            let _ = client.raw_notify("workspace/didChangeWatchedFiles", changes);
        }
    }

    /// A notification of a language extension, sent as it is. A dead server loses it;
    /// the caller keeps what it has to resend when the server is back.
    pub fn notify(&self, method: &str, params: Value) {
        if let Some((client, _)) = self.live_client() {
            let _ = client.raw_notify(method, params);
        }
    }

    /// The version of an open document, for askers that do not track edits.
    pub fn version(&self, uri: &Uri) -> Option<i32> {
        lock(&self.documents)
            .open
            .get(uri)
            .map(|document| document.version)
    }

    /// The text of an open document, as the server has it.
    pub fn text(&self, uri: &Uri) -> Option<Arc<str>> {
        lock(&self.documents)
            .open
            .get(uri)
            .map(|document| document.text.clone())
    }

    fn check(&self, place: &Place) -> Result<(), RouteError> {
        match self.version(&place.uri) {
            Some(version) if version == place.version => Ok(()),
            _ => Err(RouteError::Stale),
        }
    }

    async fn ask<R: Request>(
        &self,
        place: &Place,
        params: R::Params,
    ) -> Result<R::Result, RouteError>
    where
        R::Params: serde::Serialize,
        R::Result: serde::de::DeserializeOwned,
    {
        self.check(place)?;
        let (client, _) = self.live_client().ok_or(RouteError::NotReady)?;
        let started = Instant::now();
        let reply = timeout(REQUEST_LIMIT, client.request::<R>(params)).await;
        perf::observe(request_metric(R::METHOD), started.elapsed());
        let result = reply.map_err(|_| RouteError::TimedOut)??;
        self.check(place)?;
        Ok(result)
    }

    /// Completion at `place`. Only the newest request of a document gets an answer.
    pub async fn completion(&self, place: Place) -> Result<Option<CompletionResponse>, RouteError> {
        let turns = {
            let documents = lock(&self.documents);
            let document = documents.open.get(&place.uri).ok_or(RouteError::Stale)?;
            document.completion.clone()
        };
        let mut turn = 0;
        turns.send_modify(|current| {
            *current += 1;
            turn = *current;
        });
        let mut replaced = turns.subscribe();
        let params = CompletionParams {
            text_document_position: place.text_document_position(),
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
            context: None,
        };
        let answer = async {
            sleep(COALESCE).await;
            self.ask::<Completion>(&place, params).await
        };
        tokio::select! {
            result = answer => result,
            // Dropping the answer future sends `$/cancelRequest` if it was sent.
            _ = replaced.wait_for(|current| *current != turn) => Err(RouteError::Superseded),
        }
    }

    /// Semantic tokens of a whole document, or of `range` of a big one. Only the newest
    /// request of a document gets an answer; the older ones are cancelled.
    pub async fn semantic_tokens(
        &self,
        place: Place,
        range: Option<Range>,
    ) -> Result<Option<SemanticTokens>, RouteError> {
        let turns = {
            let documents = lock(&self.documents);
            let document = documents.open.get(&place.uri).ok_or(RouteError::Stale)?;
            document.tokens.clone()
        };
        let mut turn = 0;
        turns.send_modify(|current| {
            *current += 1;
            turn = *current;
        });
        let mut replaced = turns.subscribe();
        let answer = async {
            sleep(COALESCE).await;
            self.check(&place)?;
            let (client, _) = self.live_client().ok_or(RouteError::NotReady)?;
            let started = Instant::now();
            let document = TextDocumentIdentifier::new(place.uri.clone());
            let tokens = match range {
                Some(range) => {
                    let params = SemanticTokensRangeParams {
                        text_document: document,
                        range,
                        work_done_progress_params: WorkDoneProgressParams::default(),
                        partial_result_params: PartialResultParams::default(),
                    };
                    let reply = timeout(
                        REQUEST_LIMIT,
                        client.request::<SemanticTokensRangeRequest>(params),
                    )
                    .await;
                    perf::observe(
                        request_metric("textDocument/semanticTokens/range"),
                        started.elapsed(),
                    );
                    reply
                        .map_err(|_| RouteError::TimedOut)??
                        .map(|result| match result {
                            SemanticTokensRangeResult::Tokens(tokens) => tokens,
                            SemanticTokensRangeResult::Partial(tokens) => SemanticTokens {
                                result_id: None,
                                data: tokens.data,
                            },
                        })
                }
                None => {
                    let params = SemanticTokensParams {
                        text_document: document,
                        work_done_progress_params: WorkDoneProgressParams::default(),
                        partial_result_params: PartialResultParams::default(),
                    };
                    let reply = timeout(
                        REQUEST_LIMIT,
                        client.request::<SemanticTokensFullRequest>(params),
                    )
                    .await;
                    perf::observe(
                        request_metric("textDocument/semanticTokens/full"),
                        started.elapsed(),
                    );
                    reply
                        .map_err(|_| RouteError::TimedOut)??
                        .map(|result| match result {
                            SemanticTokensResult::Tokens(tokens) => tokens,
                            SemanticTokensResult::Partial(tokens) => SemanticTokens {
                                result_id: None,
                                data: tokens.data,
                            },
                        })
                }
            };
            self.check(&place)?;
            Ok(tokens)
        };
        tokio::select! {
            result = answer => result,
            // Dropping the answer future sends `$/cancelRequest` if it was sent.
            _ = replaced.wait_for(|current| *current != turn) => Err(RouteError::Superseded),
        }
    }

    pub async fn definition(
        &self,
        place: Place,
    ) -> Result<Option<GotoDefinitionResponse>, RouteError> {
        let params = goto_params(&place);
        self.ask::<GotoDefinition>(&place, params).await
    }

    pub async fn type_definition(
        &self,
        place: Place,
    ) -> Result<Option<GotoDefinitionResponse>, RouteError> {
        let params: GotoTypeDefinitionParams = goto_params(&place);
        self.ask::<GotoTypeDefinition>(&place, params).await
    }

    pub async fn implementation(
        &self,
        place: Place,
    ) -> Result<Option<GotoDefinitionResponse>, RouteError> {
        let params: GotoImplementationParams = goto_params(&place);
        self.ask::<GotoImplementation>(&place, params).await
    }

    pub async fn hover(&self, place: Place) -> Result<Option<Hover>, RouteError> {
        let params = HoverParams {
            text_document_position_params: place.text_document_position(),
            work_done_progress_params: WorkDoneProgressParams::default(),
        };
        self.ask::<HoverRequest>(&place, params).await
    }

    /// The hints of a range of a document. The answer is kept, so that the editor can
    /// keep showing it while the text changes under it.
    pub async fn inlay_hints(
        &self,
        place: Place,
        range: Range,
    ) -> Result<Option<Vec<InlayHint>>, RouteError> {
        let params = InlayHintParams {
            text_document: TextDocumentIdentifier::new(place.uri.clone()),
            range,
            work_done_progress_params: WorkDoneProgressParams::default(),
        };
        let answer = self.ask::<InlayHintRequest>(&place, params).await?;
        let hints = answer.clone().unwrap_or_default();
        let mut documents = lock(&self.documents);
        if let Some(document) = documents.open.get_mut(&place.uri) {
            document.hints = Some(lsp_client::hints::InlayHints::new(
                place.version,
                range,
                hints,
            ));
        }
        Ok(answer)
    }

    /// The hints of the last answer for this version and range, already moved by the
    /// edits that came after it.
    pub fn cached_inlay_hints(&self, uri: &Uri, version: i32, range: &Range) -> Vec<InlayHint> {
        let documents = lock(&self.documents);
        let Some(hints) = documents
            .open
            .get(uri)
            .and_then(|document| document.hints.as_ref())
        else {
            return Vec::new();
        };
        if !hints.matches(version, range) {
            return Vec::new();
        }
        hints
            .hints()
            .iter()
            .filter(|hint| lsp_client::hints::in_range(range, hint.position))
            .cloned()
            .collect()
    }

    /// The code lenses of a whole document. Only the newest request of a document gets
    /// an answer; the older ones are cancelled. The answer is kept, so that the editor
    /// keeps showing its labels while the text changes under them.
    pub async fn code_lens(&self, place: Place) -> Result<Option<Vec<CodeLens>>, RouteError> {
        let turns = {
            let documents = lock(&self.documents);
            let document = documents.open.get(&place.uri).ok_or(RouteError::Stale)?;
            document.lenses_turns.clone()
        };
        let mut turn = 0;
        turns.send_modify(|current| {
            *current += 1;
            turn = *current;
        });
        let mut replaced = turns.subscribe();
        let answer = async {
            sleep(COALESCE).await;
            let params = CodeLensParams {
                text_document: TextDocumentIdentifier::new(place.uri.clone()),
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
            };
            self.ask::<CodeLensRequest>(&place, params).await
        };
        let answer = tokio::select! {
            result = answer => result?,
            // Dropping the answer future sends `$/cancelRequest` if it was sent.
            _ = replaced.wait_for(|current| *current != turn) => return Err(RouteError::Superseded),
        };
        let lenses = answer.clone().unwrap_or_default();
        let mut documents = lock(&self.documents);
        if let Some(document) = documents.open.get_mut(&place.uri) {
            document.lenses = Some(lsp_client::CodeLenses::new(place.version, lenses));
        }
        Ok(answer)
    }

    /// The labels of the last answer for this version, already moved by the edits that
    /// came after it.
    pub fn cached_code_lens(&self, uri: &Uri, version: i32) -> Vec<CodeLens> {
        let documents = lock(&self.documents);
        let Some(lenses) = documents
            .open
            .get(uri)
            .and_then(|document| document.lenses.as_ref())
        else {
            return Vec::new();
        };
        if !lenses.matches(version) {
            return Vec::new();
        }
        lenses.lenses().to_vec()
    }

    /// The command of one label the editor shows.
    pub async fn code_lens_resolve(
        &self,
        place: Place,
        lens: CodeLens,
    ) -> Result<CodeLens, RouteError> {
        self.ask::<CodeLensResolve>(&place, lens).await
    }

    pub async fn signature_help(&self, place: Place) -> Result<Option<SignatureHelp>, RouteError> {
        let params = SignatureHelpParams {
            context: None,
            text_document_position_params: place.text_document_position(),
            work_done_progress_params: WorkDoneProgressParams::default(),
        };
        self.ask::<SignatureHelpRequest>(&place, params).await
    }

    pub async fn workspace_symbol(
        &self,
        query: &str,
    ) -> Result<Option<WorkspaceSymbolResponse>, RouteError> {
        let (client, _) = self.live_client().ok_or(RouteError::NotReady)?;
        let params = WorkspaceSymbolParams {
            query: query.to_owned(),
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        };
        let reply = timeout(
            REQUEST_LIMIT,
            client.request::<WorkspaceSymbolRequest>(params),
        )
        .await;
        Ok(reply.map_err(|_| RouteError::TimedOut)??)
    }

    /// Edits that format the whole document.
    pub async fn formatting(
        &self,
        place: Place,
        options: FormattingOptions,
    ) -> Result<Option<Vec<TextEdit>>, RouteError> {
        let params = DocumentFormattingParams {
            text_document: TextDocumentIdentifier::new(place.uri.clone()),
            options,
            work_done_progress_params: WorkDoneProgressParams::default(),
        };
        self.ask::<Formatting>(&place, params).await
    }

    /// Edits that format a part of the document.
    pub async fn range_formatting(
        &self,
        place: Place,
        range: Range,
        options: FormattingOptions,
    ) -> Result<Option<Vec<TextEdit>>, RouteError> {
        let params = DocumentRangeFormattingParams {
            text_document: TextDocumentIdentifier::new(place.uri.clone()),
            range,
            options,
            work_done_progress_params: WorkDoneProgressParams::default(),
        };
        self.ask::<RangeFormatting>(&place, params).await
    }

    /// Actions for `range`. The diagnostics there let the server offer its fixes.
    pub async fn code_actions(
        &self,
        place: Place,
        range: Range,
        published: &Published,
        only: Option<Vec<CodeActionKind>>,
    ) -> Result<Option<CodeActionResponse>, RouteError> {
        let diagnostics = published
            .diagnostics(&place.uri)
            .into_iter()
            .filter(|diagnostic| overlaps(diagnostic.range, range))
            .collect();
        let params = CodeActionParams {
            text_document: TextDocumentIdentifier::new(place.uri.clone()),
            range,
            context: CodeActionContext {
                diagnostics,
                only,
                trigger_kind: None,
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        };
        self.ask::<CodeActionRequest>(&place, params).await
    }

    pub async fn document_highlight(
        &self,
        place: Place,
    ) -> Result<Option<Vec<DocumentHighlight>>, RouteError> {
        let params = DocumentHighlightParams {
            text_document_position_params: place.text_document_position(),
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        };
        self.ask::<DocumentHighlightRequest>(&place, params).await
    }

    /// Every use of the symbol at `place`, its declaration included.
    pub async fn references(&self, place: Place) -> Result<Option<Vec<Location>>, RouteError> {
        let params = ReferenceParams {
            text_document_position: place.text_document_position(),
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
            context: ReferenceContext {
                include_declaration: true,
            },
        };
        self.ask::<References>(&place, params).await
    }

    pub async fn rename(
        &self,
        place: Place,
        new_name: String,
    ) -> Result<Option<WorkspaceEdit>, RouteError> {
        let params = RenameParams {
            text_document_position: place.text_document_position(),
            new_name,
            work_done_progress_params: WorkDoneProgressParams::default(),
        };
        self.ask::<Rename>(&place, params).await
    }

    /// The types next to the one at `place`, above it or below it.
    pub async fn type_hierarchy(
        &self,
        place: Place,
        supertypes: bool,
    ) -> Result<Vec<TypeHierarchyItem>, RouteError> {
        let params = TypeHierarchyPrepareParams {
            text_document_position_params: place.text_document_position(),
            work_done_progress_params: WorkDoneProgressParams::default(),
        };
        let items = self.ask::<TypeHierarchyPrepare>(&place, params).await?;
        let Some(item) = items.into_iter().flatten().next() else {
            return Ok(Vec::new());
        };
        let (client, _) = self.live_client().ok_or(RouteError::NotReady)?;
        let work_done_progress_params = WorkDoneProgressParams::default();
        let partial_result_params = PartialResultParams::default();
        let reply = if supertypes {
            let params = TypeHierarchySupertypesParams {
                item,
                work_done_progress_params,
                partial_result_params,
            };
            timeout(
                REQUEST_LIMIT,
                client.request::<TypeHierarchySupertypes>(params),
            )
            .await
        } else {
            let params = TypeHierarchySubtypesParams {
                item,
                work_done_progress_params,
                partial_result_params,
            };
            timeout(
                REQUEST_LIMIT,
                client.request::<TypeHierarchySubtypes>(params),
            )
            .await
        };
        Ok(reply
            .map_err(|_| RouteError::TimedOut)??
            .unwrap_or_default())
    }

    /// The declarations of the document at `place`, with their details when the server
    /// gives them.
    pub async fn document_symbol(
        &self,
        place: Place,
    ) -> Result<Option<DocumentSymbolResponse>, RouteError> {
        let params = DocumentSymbolParams {
            text_document: TextDocumentIdentifier::new(place.uri.clone()),
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        };
        self.ask::<DocumentSymbolRequest>(&place, params).await
    }

    /// Ctrl+Alt+H: the functions that can be called from `place`, or that call it.
    pub async fn prepare_call_hierarchy(
        &self,
        place: Place,
    ) -> Result<Vec<CallHierarchyItem>, RouteError> {
        let params = CallHierarchyPrepareParams {
            text_document_position_params: place.text_document_position(),
            work_done_progress_params: WorkDoneProgressParams::default(),
        };
        let items = self
            .ask::<CallHierarchyPrepare>(&place, params)
            .await?
            .unwrap_or_default();
        Ok(items)
    }

    /// The next level of the call hierarchy under `item`.
    pub async fn call_hierarchy_calls(
        &self,
        item: CallHierarchyItem,
        incoming: bool,
    ) -> Result<Vec<CallHierarchyItem>, RouteError> {
        let (client, _) = self.live_client().ok_or(RouteError::NotReady)?;
        let work_done_progress_params = WorkDoneProgressParams::default();
        let partial_result_params = PartialResultParams::default();
        if incoming {
            let params = CallHierarchyIncomingCallsParams {
                item,
                work_done_progress_params,
                partial_result_params,
            };
            let calls = timeout(
                REQUEST_LIMIT,
                client.request::<CallHierarchyIncomingCalls>(params),
            )
            .await
            .map_err(|_| RouteError::TimedOut)??;
            Ok(calls
                .unwrap_or_default()
                .into_iter()
                .map(|call| call.from)
                .collect())
        } else {
            let params = CallHierarchyOutgoingCallsParams {
                item,
                work_done_progress_params,
                partial_result_params,
            };
            let calls = timeout(
                REQUEST_LIMIT,
                client.request::<CallHierarchyOutgoingCalls>(params),
            )
            .await
            .map_err(|_| RouteError::TimedOut)??;
            Ok(calls
                .unwrap_or_default()
                .into_iter()
                .map(|call| call.to)
                .collect())
        }
    }

    /// A request of the language's own, with no meaning for the core.
    pub async fn extension(&self, method: &str, params: Value) -> Result<Value, RouteError> {
        let (client, _) = self.live_client().ok_or(RouteError::NotReady)?;
        let reply = timeout(REQUEST_LIMIT, client.raw_request(method, params)).await;
        Ok(reply.map_err(|_| RouteError::TimedOut)??)
    }
}

/// The `FileChangeType` of `workspace/didChangeWatchedFiles`: created, changed or
/// deleted.
fn watched_file_type(change: FileChange) -> u32 {
    match change {
        FileChange::Created => 1,
        FileChange::Changed => 2,
        FileChange::Removed => 3,
    }
}

/// Touching ranges count, so a caret right after an error still gets its fix.
fn overlaps(a: Range, b: Range) -> bool {
    a.start <= b.end && b.start <= a.end
}

/// The latency metric of a request, named after what it asks for.
fn request_metric(method: &str) -> &'static str {
    match method {
        "textDocument/completion" => "lsp.completion",
        "textDocument/hover" => "lsp.hover",
        "textDocument/inlayHint" => "lsp.inlay_hint",
        "textDocument/definition" => "lsp.definition",
        "textDocument/typeDefinition" => "lsp.type_definition",
        "textDocument/implementation" => "lsp.implementation",
        "textDocument/signatureHelp" => "lsp.signature_help",
        "textDocument/references" => "lsp.references",
        "textDocument/rename" => "lsp.rename",
        "textDocument/formatting" => "lsp.formatting",
        "textDocument/codeAction" => "lsp.code_action",
        "textDocument/documentHighlight" => "lsp.document_highlight",
        "textDocument/semanticTokens/full" => "lsp.semantic_tokens",
        "textDocument/semanticTokens/range" => "lsp.semantic_tokens_range",
        "workspace/symbol" => "lsp.workspace_symbol",
        "textDocument/prepareTypeHierarchy" => "lsp.type_hierarchy",
        _ => "lsp.request",
    }
}

fn goto_params(place: &Place) -> GotoDefinitionParams {
    GotoDefinitionParams {
        text_document_position_params: place.text_document_position(),
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::{FileChange, watched_file_type};

    #[test]
    fn a_watched_file_change_gets_its_protocol_type() {
        assert_eq!(watched_file_type(FileChange::Created), 1);
        assert_eq!(watched_file_type(FileChange::Changed), 2);
        assert_eq!(watched_file_type(FileChange::Removed), 3);
    }
}
