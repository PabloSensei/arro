//! Move Members: the members of a type that another type of the project takes, and the
//! types that can take them. The members come from the tree of the open file, the types
//! from the sources of the project.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;

use base::Cancel;
use lang_api::{
    Answer, Confidence, MoveDestination, MoveMembersForm, MoveMembersQuery, Provider, Reply,
    SourceId,
};
use lsp_client::lsp_types::Uri;
use lsp_client::uri::file_path;
use semantic_router::Router;
use tokio::runtime::Handle;

const SOURCE: SourceId = SourceId::new("jdtls");

const TITLE: &str = "Move Members";
const MEMBERS_LABEL: &str = "Members";
const TARGET_LABEL: &str = "To class";

/// The form of Move Members of one project.
pub(crate) struct MoveMembers {
    root: PathBuf,
    router: Arc<Router>,
    runtime: Handle,
}

impl MoveMembers {
    pub(crate) fn new(root: PathBuf, router: Arc<Router>, runtime: Handle) -> Self {
        Self {
            root,
            router,
            runtime,
        }
    }
}

/// The types declared under `root` that a member can go to, without the source file.
fn targets_of(root: &Path, source: &Path, cancel: &Cancel) -> Vec<MoveDestination> {
    let mut seen = HashSet::new();
    let mut targets = Vec::new();
    for path in crate::members::java_files(root) {
        if cancel.is_cancelled() {
            return Vec::new();
        }
        if path == source {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Some(name) = crate::members::type_name(&text) else {
            continue;
        };
        let package = crate::members::package_name(&text);
        let handle = match package.as_deref() {
            Some(package) if !package.is_empty() => format!("{package}.{name}"),
            _ => name.clone(),
        };
        if !seen.insert(handle.clone()) {
            continue;
        }
        targets.push(MoveDestination {
            name,
            detail: package,
            folder: None,
            handle,
        });
    }
    targets.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.handle.cmp(&right.handle))
    });
    targets
}

impl Provider<MoveMembersQuery, Option<MoveMembersForm>> for MoveMembers {
    fn start(
        &self,
        query: MoveMembersQuery,
        cancel: Cancel,
        mut reply: Reply<Option<MoveMembersForm>>,
    ) {
        let root = self.root.clone();
        let router = self.router.clone();
        let spot = query.spot;
        self.runtime.spawn(async move {
            // The text of the open file, and the file on disk otherwise.
            let Some(uri) = Uri::from_str(&spot.uri).ok() else {
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
            let offset = lang_api::offset_in(&text, spot.point);
            let Some(members) = crate::members::move_members(&text, offset) else {
                let _ = reply.send(Answer::new(None, Confidence::Certain, SOURCE));
                return;
            };
            let targets = targets_of(&root, &path, &cancel);
            if cancel.is_cancelled() {
                return;
            }
            let form = MoveMembersForm {
                title: TITLE.to_owned(),
                members_label: MEMBERS_LABEL.to_owned(),
                members,
                target_label: TARGET_LABEL.to_owned(),
                targets,
            };
            let _ = reply.send(Answer::new(Some(form), Confidence::Certain, SOURCE));
        });
    }
}
