//! Where the declaration of the symbol at the caret lies. A row that inlines the symbol is
//! offered only when its declaration is in the sources of the project.

use ::platform;
use lang_api::{Confidence, NavigationKind, NavigationQuery, SourceSpan, ask};
use ui_kit::*;

use crate::semantic::{first, uri_path};
use crate::view::EditorView;

/// The answer about the declaration at the caret, as far as the editor knows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Declaration {
    /// The language has no navigation to ask. Its rows keep the place they always had.
    Unasked,
    /// A place of the declaration is a file under the root of the project.
    Project,
    /// The language names places, and none of them is a file of the project: a class of a
    /// library or of the JDK.
    Library,
    /// The language names no place, or it did not answer.
    Unknown,
}

impl Declaration {
    /// Whether a row that inlines the symbol is offered.
    pub(crate) fn offers_inline(self) -> bool {
        matches!(self, Self::Unasked | Self::Project)
    }
}

impl EditorView {
    /// The declaration at the caret when it is known without a question: `Unasked` for a
    /// language with no navigation, and the definition of the place when the navigation cache
    /// has it (Ctrl+B fills it). `None` when the language must be asked.
    pub(crate) fn known_declaration(&self) -> Option<Declaration> {
        let lsp = self.lsp.as_ref()?;
        if lsp.support.navigation().is_none() {
            return Some(Declaration::Unasked);
        }
        let (spot, _) = self.spot()?;
        let key = (
            spot.uri,
            spot.version,
            spot.point,
            NavigationKind::Definition,
        );
        let spans = lsp.navigation_cache.get(&key)?;
        Some(self.declaration_of(spans))
    }

    /// The declaration at the caret. `done` gets it at once when it is known, and otherwise
    /// once the language answers. The task of a question that is still open is returned, for
    /// the list that waits for the answer.
    pub(crate) fn with_declaration(
        &mut self,
        cx: &mut Context<Self>,
        done: impl FnOnce(&mut Self, Declaration, &mut Context<Self>) + 'static,
    ) -> Option<Task<()>> {
        if let Some(known) = self.known_declaration() {
            done(self, known, cx);
            return None;
        }
        self.ask_declaration(cx, done)
    }

    /// Asks the language for the definition at the caret and calls `done` with the answer.
    /// The answer is not put into the navigation cache: a cached answer would let Ctrl+B skip
    /// the check that a declaration names itself.
    pub(crate) fn ask_declaration(
        &mut self,
        cx: &mut Context<Self>,
        done: impl FnOnce(&mut Self, Declaration, &mut Context<Self>) + 'static,
    ) -> Option<Task<()>> {
        let Some((spot, _)) = self.spot() else {
            done(self, Declaration::Unknown, cx);
            return None;
        };
        let support = self.lsp.as_ref()?.support.clone();
        let Some(provider) = support.navigation() else {
            done(self, Declaration::Unasked, cx);
            return None;
        };
        let replies = ask(
            provider,
            NavigationQuery {
                kind: NavigationKind::Definition,
                spot,
            },
        );
        Some(cx.spawn(async move |this, cx| {
            let answer = first(replies).await;
            this.update(cx, |this, cx| {
                let spans = match answer {
                    Some((spans, confidence)) if confidence != Confidence::Unknown => spans,
                    _ => Vec::new(),
                };
                let declaration = this.declaration_of(&spans);
                done(this, declaration, cx);
            })
            .ok();
        }))
    }

    /// What the places of a definition say about the sources of the project. Without a root a
    /// file counts as a file of the project; a place that is not a file never does.
    pub(crate) fn declaration_of(&self, spans: &[SourceSpan]) -> Declaration {
        if spans.is_empty() {
            return Declaration::Unknown;
        }
        let root = self.project_root().map(platform::path_key);
        let in_project = spans.iter().any(|span| {
            uri_path(&span.uri).is_some_and(|path| {
                root.as_ref()
                    .is_none_or(|root| platform::path_key(&path).starts_with(root))
            })
        });
        if in_project {
            Declaration::Project
        } else {
            Declaration::Library
        }
    }
}
