//! A move to one of the places around a symbol: a method of a type goes to a parameter
//! or a field whose type is a class of the project. The places come from the server, the
//! name of the method from the tree of the open file.

use std::str::FromStr;
use std::sync::Arc;

use base::Cancel;
use futures_util::StreamExt;
use lang_api::{
    Answer, Confidence, MoveDestination, MoveDestinationProvider, MoveKind, MoveQuery,
    MoveTargetsForm, MoveTargetsQuery, Provider, Reply, SourceId,
};
use lsp_client::lsp_types::Uri;
use lsp_client::uri::file_path;
use semantic_router::Router;
use tokio::runtime::Handle;

const SOURCE: SourceId = SourceId::new("jdtls");

const TITLE: &str = "Move Instance Method";

/// The form of a move to a parameter or a field of the type of a symbol.
pub(crate) struct MoveTargets {
    router: Arc<Router>,
    destinations: Arc<MoveDestinationProvider>,
    runtime: Handle,
}

impl MoveTargets {
    pub(crate) fn new(
        router: Arc<Router>,
        destinations: Arc<MoveDestinationProvider>,
        runtime: Handle,
    ) -> Self {
        Self {
            router,
            destinations,
            runtime,
        }
    }
}

impl Provider<MoveTargetsQuery, Option<MoveTargetsForm>> for MoveTargets {
    fn start(
        &self,
        query: MoveTargetsQuery,
        cancel: Cancel,
        mut reply: Reply<Option<MoveTargetsForm>>,
    ) {
        if query.kind != MoveKind::InstanceMethod {
            let _ = reply.send(Answer::new(None, Confidence::Certain, SOURCE));
            return;
        }
        let router = self.router.clone();
        let destinations = self.destinations.clone();
        self.runtime.spawn(async move {
            // The text of the open file, and the file on disk otherwise.
            let Some(uri) = Uri::from_str(&query.spot.uri).ok() else {
                let _ = reply.send(Answer::new(None, Confidence::Unknown, SOURCE));
                return;
            };
            let Some(path) = file_path(&uri) else {
                let _ = reply.send(Answer::new(None, Confidence::Unknown, SOURCE));
                return;
            };
            let text = router
                .text(&uri)
                .map(|text| text.to_string())
                .or_else(|| std::fs::read_to_string(&path).ok());
            let Some(text) = text else {
                let _ = reply.send(Answer::new(None, Confidence::Unknown, SOURCE));
                return;
            };
            let offset = lang_api::offset_in(&text, query.spot.point);
            let Some(name) = crate::members::method_name(&text, offset) else {
                let _ = reply.send(Answer::new(None, Confidence::Unknown, SOURCE));
                return;
            };
            // The request to the server keeps the cancellation of the form: closing the
            // form before the answer stops both.
            let (inner_reply, mut replies) = lang_api::channel::<Vec<MoveDestination>>();
            destinations.start(
                MoveQuery {
                    spot: query.spot.clone(),
                    kind: MoveKind::InstanceMethod,
                },
                cancel.clone(),
                inner_reply,
            );
            let answer = replies.next().await;
            if cancel.is_cancelled() || reply.is_cancelled() {
                return;
            }
            let Some(answer) = answer else {
                let _ = reply.send(Answer::new(None, Confidence::Unknown, SOURCE));
                return;
            };
            if answer.confidence == Confidence::Unknown {
                let _ = reply.send(Answer::new(None, Confidence::Unknown, SOURCE));
                return;
            }
            if answer.value.is_empty() {
                let _ = reply.send(Answer::new(None, Confidence::Certain, SOURCE));
                return;
            }
            let form = MoveTargetsForm {
                title: format!("{TITLE}: {name}"),
                targets: answer.value,
            };
            let _ = reply.send(Answer::new(Some(form), Confidence::Certain, SOURCE));
        });
    }
}
