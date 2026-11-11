//! The Method Hierarchy tool window: every type above and below the one at the caret,
//! with what each of them does about the method.

use ::base::Cancel;
use editor_view::{Semantic, uri_path};
use lang_api::{Confidence, DocumentSpot, MethodHierarchyQuery, MethodRelation, ask};
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::{METHOD_HIERARCHY_TOOL, Workspace};

use crate::items::{Item, ItemsPanel};

/// The window that shows the hierarchy of one method.
pub struct MethodHierarchyPanel {
    panel: Entity<ItemsPanel>,
    /// The place Expand All asks about again: the list is flat, so the whole hierarchy
    /// is the only thing there is to load.
    spot: Option<DocumentSpot>,
    /// Set when the window goes away, so a late answer stops.
    cancel: Cancel,
}

/// The panel of the window, for the editor tab that receives the place.
pub struct ActiveMethodHierarchy(pub Entity<MethodHierarchyPanel>);

impl Global for ActiveMethodHierarchy {}

fn relation_text(relation: MethodRelation) -> &'static str {
    match relation {
        MethodRelation::Defines => "defines",
        MethodRelation::Inherits => "inherits",
        MethodRelation::MustDefine => "must define",
    }
}

impl MethodHierarchyPanel {
    pub fn create(workspace: WeakEntity<Workspace>, cx: &mut App) -> Entity<Self> {
        let panel = ItemsPanel::create(
            "Method Hierarchy",
            METHOD_HIERARCHY_TOOL,
            "No method hierarchy here.",
            workspace,
            cx,
        );
        cx.new(|_| Self {
            panel,
            spot: None,
            cancel: Cancel::new(),
        })
    }

    pub fn panel(&self) -> &Entity<ItemsPanel> {
        &self.panel
    }

    /// Ctrl+Equal: asks for the whole hierarchy again, so every type above and below is
    /// in the list. The list has no child rows to fetch one by one.
    pub fn expand_all(&mut self, cx: &mut Context<Self>) {
        if let Some(spot) = self.spot.clone() {
            self.load(spot, cx);
        }
    }

    /// Ctrl+Minus: the list is flat, there is nothing to close.
    pub fn collapse_all(&mut self, cx: &mut Context<Self>) {
        cx.notify();
    }

    /// Ctrl+Shift+H: loads the hierarchy of the method at `spot`.
    pub fn start(&mut self, spot: DocumentSpot, cx: &mut Context<Self>) {
        self.spot = Some(spot.clone());
        self.load(spot, cx);
    }

    fn load(&mut self, spot: DocumentSpot, cx: &mut Context<Self>) {
        let Some(support) = cx
            .try_global::<Semantic>()
            .map(|semantic| semantic.0.clone())
        else {
            return;
        };
        let Some(provider) = support.method_hierarchy() else {
            return;
        };
        let entries = ask(provider, MethodHierarchyQuery(spot.clone()));
        self.panel.update(cx, |panel, cx| {
            panel.set_title("Method Hierarchy".to_owned(), cx);
            panel.set_items(Vec::new(), cx);
        });
        let cancel = self.cancel.clone();
        cx.spawn(async move |this, cx| {
            use futures_util::StreamExt;
            let mut entries = entries;
            let answer = entries.next().await;
            if cancel.is_cancelled() {
                return;
            }
            this.update(cx, |this, cx| {
                let Some(answer) = answer else {
                    return;
                };
                if answer.confidence == Confidence::Unknown {
                    return;
                }
                let items = answer
                    .value
                    .into_iter()
                    .map(|entry| {
                        let (path, line, column) = match &entry.place {
                            Some(place) => match uri_path(&place.uri) {
                                Some(path) => (
                                    path,
                                    place.span.start.line as usize,
                                    place.span.start.character as usize,
                                ),
                                None => (Default::default(), 0, 0),
                            },
                            None => (uri_path(&spot.uri).unwrap_or_default(), 0, 0),
                        };
                        Item {
                            path,
                            line,
                            column,
                            label: format!(
                                "{} — {}",
                                entry.type_name,
                                relation_text(entry.relation)
                            ),
                            hint: entry.method,
                            icon: "nodes/class",
                        }
                    })
                    .collect();
                this.panel.update(cx, |panel, cx| {
                    panel.set_title("Method Hierarchy".to_owned(), cx);
                    panel.set_items(items, cx);
                });
            })
            .ok();
        })
        .detach();
    }
}

impl Drop for MethodHierarchyPanel {
    fn drop(&mut self) {
        // The window is gone: a late answer is not applied.
        self.cancel.cancel();
    }
}
