//! The checks and the colours of the places inside the text of a document: the queries of
//! a data repository, for one. The language reads its model in the background and the
//! window puts what it knows into every open editor again after each rebuild.

use std::sync::Arc;

use editor_view::{EditorView, Semantic};
use futures_util::StreamExt;
use lang_api::LanguageSupport;
use ui_kit::gpui::{App, Context};
use workspace::Workspace;

/// The source the marks and the tokens of the document text come from.
const KEY: &str = "embedded-text";

/// The window follows one model of the text of its documents; a server that starts again
/// does not add a second one.
pub struct Followed(bool);

impl ui_kit::gpui::Global for Followed {}

/// Puts the checks and the colours of the text of an editor into it. A language without
/// such a place leaves the editor alone.
pub(crate) fn refresh(view: &mut EditorView, cx: &mut Context<EditorView>) {
    let Some(support) = cx
        .try_global::<Semantic>()
        .map(|semantic| semantic.0.clone())
    else {
        return;
    };
    let Some(provider) = support.embedded_text() else {
        return;
    };
    let Some(text) = view.text() else {
        return;
    };
    let path = view.path().to_path_buf();
    let diagnostics = provider.diagnostics(&path, &text);
    let tokens = provider.tokens(&path, &text);
    view.set_native_marks_from(KEY, diagnostics, cx);
    view.set_native_tokens(KEY, tokens, cx);
}

/// Puts the checks of the text into every open editor again.
pub(crate) fn apply_all(cx: &mut App) {
    for view in crate::outline::OpenEditors::all(cx) {
        view.update(cx, refresh);
    }
}

/// Follows the model of the language and puts its checks into the editors again after
/// every rebuild. A language without such a model starts nothing.
pub(crate) fn follow(support: Arc<dyn LanguageSupport>, cx: &mut Context<Workspace>) {
    if support.embedded_text().is_none() {
        return;
    }
    if cx
        .try_global::<Followed>()
        .is_some_and(|followed| followed.0)
    {
        return;
    }
    cx.set_global(Followed(true));
    cx.spawn(async move |workspace, cx| {
        let Some(mut updates) = support
            .embedded_text()
            .and_then(|provider| provider.updates())
        else {
            return;
        };
        while updates.next().await.is_some() {
            if workspace.update(cx, |_, cx| apply_all(cx)).is_err() {
                return;
            }
        }
    })
    .detach();
}
