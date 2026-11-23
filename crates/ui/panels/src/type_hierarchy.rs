//! The Hierarchy tool window in its Type view: the types above and below the one at the
//! caret. Expanding a row asks the language for the next level; the window shows the
//! root at once and never waits on the server.

use std::sync::Arc;

use ::base::Cancel;
use editor_view::Semantic;
use futures_util::StreamExt;
use lang_api::{
    Confidence, DocumentSpot, HierarchyDirection, LanguageSupport, TypeHierarchyItem,
    TypeHierarchyQuery, TypeKind, ask,
};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::{TYPE_HIERARCHY_TOOL, Workspace};

use crate::hierarchy_scope::{HierarchyScope, ScopeRoots, header_controls};
use crate::navigation;

const GROUP: &str = "left-panel";
const ROW_MARGIN: f32 = SPACE_L;
const TITLE: &str = "Hierarchy";
/// Expand All loads every branch down to this depth.
const MAX_DEPTH: usize = 5;
/// The longest chain of supertypes the Type view walks upwards.
const MAX_ANCESTORS: usize = 32;

/// The view a button of the header picks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HierarchyView {
    /// The topmost superclass down through the selected type to its subtypes.
    Type,
    /// The selected type and the types it extends or implements.
    Supertypes,
    /// The selected type and the types that extend or implement it.
    Subtypes,
}

struct Node {
    item: TypeHierarchyItem,
    depth: usize,
    expanded: bool,
    loading: bool,
    /// The children of this node have been asked for, even when there are none.
    loaded: bool,
    children: Vec<usize>,
}

impl Node {
    fn new(item: TypeHierarchyItem, depth: usize) -> Self {
        Self {
            item,
            depth,
            expanded: false,
            loading: false,
            loaded: false,
            children: Vec::new(),
        }
    }

    fn expandable(&self) -> bool {
        self.loading || !self.children.is_empty() || !self.expanded
    }
}

/// The window that shows the type hierarchy of one place.
pub struct TypeHierarchyPanel {
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    support: Option<Arc<dyn LanguageSupport>>,
    spot: Option<DocumentSpot>,
    view: HierarchyView,
    /// The folders the scope filters the rows by, from the model of the project.
    scope: HierarchyScope,
    scope_roots: ScopeRoots,
    nodes: Vec<Node>,
    roots: Vec<usize>,
    selected: Option<usize>,
    scroll: ScrollHandle,
    /// Bumped on every new root, so a late answer is dropped.
    serial: u64,
    /// Set when the window goes away, so the deep expansion stops asking.
    cancel: Cancel,
    /// The requests of the window: dropping one drops its replies, which cancels the
    /// request in the language.
    pending: Vec<Task<()>>,
}

/// The panel of the window, for the editor tab that receives the place.
pub struct ActiveTypeHierarchy(pub Entity<TypeHierarchyPanel>);

impl Global for ActiveTypeHierarchy {}

impl TypeHierarchyPanel {
    pub fn create(workspace: WeakEntity<Workspace>, cx: &mut App) -> Entity<Self> {
        cx.new(|cx| Self {
            workspace,
            focus: cx.focus_handle(),
            support: None,
            spot: None,
            view: HierarchyView::Type,
            scope: HierarchyScope::All,
            scope_roots: ScopeRoots::default(),
            nodes: Vec::new(),
            roots: Vec::new(),
            selected: None,
            scroll: ScrollHandle::new(),
            serial: 0,
            cancel: Cancel::new(),
            pending: Vec::new(),
        })
    }

    pub fn focus(&self) -> FocusHandle {
        self.focus.clone()
    }

    /// The view the header has picked.
    #[allow(dead_code)]
    pub fn view(&self) -> HierarchyView {
        self.view
    }

    /// The scope the header has picked.
    #[allow(dead_code)]
    pub fn scope(&self) -> HierarchyScope {
        self.scope
    }

    /// Ctrl+H: the hierarchy of the type at `spot`, in the Type view.
    pub fn start(&mut self, spot: DocumentSpot, cx: &mut Context<Self>) {
        self.support = cx
            .try_global::<Semantic>()
            .map(|semantic| semantic.0.clone());
        if self.support.is_none() {
            return;
        }
        self.refresh_scope_roots(cx);
        self.spot = Some(spot);
        self.view = HierarchyView::Type;
        self.load(cx);
    }

    /// A button of the header: the same type in another view.
    pub fn set_view(&mut self, view: HierarchyView, cx: &mut Context<Self>) {
        if self.view == view {
            return;
        }
        self.view = view;
        self.load(cx);
    }

    /// A button of the header: which folders of the project the rows come from. The
    /// filter works over the rows that are already here, without a new request.
    pub fn set_scope(&mut self, scope: HierarchyScope, cx: &mut Context<Self>) {
        if self.scope == scope {
            return;
        }
        self.scope = scope;
        self.refresh_scope_roots(cx);
        if self
            .selected
            .is_some_and(|selected| !self.visible().iter().any(|(index, _)| *index == selected))
        {
            self.selected = self.visible().first().map(|(index, _)| *index);
        }
        cx.notify();
    }

    /// Reads the source and test folders of the project from the model of the window.
    fn refresh_scope_roots(&mut self, cx: &App) {
        self.scope_roots = ScopeRoots::from_model(cx);
    }

    /// Ctrl+Equal: every branch opens and its children are loaded down to [`MAX_DEPTH`].
    /// The answers come in the background; a closing window cancels the rest.
    pub fn expand_all(&mut self, cx: &mut Context<Self>) {
        for node in &mut self.nodes {
            if node.depth < MAX_DEPTH {
                node.expanded = true;
            }
        }
        cx.notify();
        let Some(support) = self.support.clone() else {
            return;
        };
        let direction = self.children_direction();
        let serial = self.serial;
        let cancel = self.cancel.clone();
        let task = cx.spawn(async move |this, cx| {
            let Some(provider) = support.type_hierarchy() else {
                return;
            };
            loop {
                if cancel.is_cancelled() {
                    return;
                }
                let next = this.update(cx, |this, cx| {
                    if this.serial != serial {
                        return None;
                    }
                    let index = this.next_to_load()?;
                    let node = &mut this.nodes[index];
                    node.loading = true;
                    node.expanded = true;
                    let item = node.item.clone();
                    cx.notify();
                    Some((index, item))
                });
                let Ok(Some((index, item))) = next else {
                    break;
                };
                let mut replies = ask(provider, TypeHierarchyQuery::Neighbours { item, direction });
                let neighbours = replies.next().await.and_then(|answer| {
                    (answer.confidence != Confidence::Unknown).then_some(answer.value)
                });
                let updated = this.update(cx, |this, cx| {
                    if this.serial != serial {
                        return;
                    }
                    this.add_children(index, neighbours.unwrap_or_default(), cx);
                });
                if updated.is_err() {
                    return;
                }
            }
        });
        self.pending.push(task);
    }

    /// The first node below [`MAX_DEPTH`] whose children have not been asked for. A
    /// chain of the Type view already has its child, so its own subtypes stay out.
    fn next_to_load(&self) -> Option<usize> {
        self.nodes.iter().position(|node| {
            node.depth < MAX_DEPTH && !node.loaded && !node.loading && node.children.is_empty()
        })
    }

    /// Ctrl+Minus: every branch closes.
    pub fn collapse_all(&mut self, cx: &mut Context<Self>) {
        for node in &mut self.nodes {
            node.expanded = false;
        }
        cx.notify();
    }

    /// The labels of the visible rows, indented by depth, for tests and scripts.
    #[allow(dead_code)]
    pub fn lines(&self) -> Vec<String> {
        self.visible()
            .into_iter()
            .map(|(index, depth)| {
                let node = &self.nodes[index];
                let hint = node
                    .item
                    .detail
                    .as_deref()
                    .map_or(String::new(), |detail| format!("  {detail}"));
                format!("{}{}{hint}", "  ".repeat(depth), node.item.name)
            })
            .collect()
    }

    /// The selected row, for tests and scripts.
    #[allow(dead_code)]
    pub fn selected_line(&self) -> Option<String> {
        let index = self.selected?;
        self.nodes.get(index).map(|node| node.item.name.clone())
    }

    /// Opens one row or closes it. Opening asks the language for its children once.
    pub fn expand(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(node) = self.nodes.get_mut(index) else {
            return;
        };
        if node.expanded {
            node.expanded = false;
            cx.notify();
            return;
        }
        if !node.children.is_empty() {
            node.expanded = true;
            cx.notify();
            return;
        }
        self.load_children(index, cx);
    }

    fn load_children(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(support) = self.support.clone() else {
            return;
        };
        let Some(node) = self.nodes.get_mut(index) else {
            return;
        };
        let item = node.item.clone();
        node.loading = true;
        let serial = self.serial;
        let direction = self.children_direction();
        let query = TypeHierarchyQuery::Neighbours { item, direction };
        cx.notify();
        let task = cx.spawn(async move |this, cx| {
            let Some(provider) = support.type_hierarchy() else {
                return;
            };
            let mut replies = ask(provider, query);
            let neighbours = replies.next().await.and_then(|answer| {
                (answer.confidence != Confidence::Unknown).then_some(answer.value)
            });
            this.update(cx, |this, cx| {
                if this.serial != serial {
                    return;
                }
                let Some(neighbours) = neighbours else {
                    if let Some(node) = this.nodes.get_mut(index) {
                        node.loading = false;
                    }
                    cx.notify();
                    return;
                };
                this.add_children(index, neighbours, cx);
            })
            .ok();
        });
        self.pending.push(task);
    }

    /// The types a row asks for: the selected type of the Type view goes down, and so
    /// does every row of the Subtypes view.
    fn children_direction(&self) -> HierarchyDirection {
        match self.view {
            HierarchyView::Supertypes => HierarchyDirection::Supertypes,
            HierarchyView::Type | HierarchyView::Subtypes => HierarchyDirection::Subtypes,
        }
    }

    fn add_children(
        &mut self,
        index: usize,
        items: Vec<TypeHierarchyItem>,
        cx: &mut Context<Self>,
    ) {
        let Some(node) = self.nodes.get_mut(index) else {
            return;
        };
        node.loading = false;
        node.loaded = true;
        node.expanded = true;
        let depth = node.depth + 1;
        let start = self.nodes.len();
        for item in items {
            self.nodes.push(Node::new(item, depth));
        }
        let children: Vec<usize> = (start..self.nodes.len()).collect();
        if let Some(node) = self.nodes.get_mut(index) {
            node.children = children;
        }
        cx.notify();
    }

    /// The root at once, then the path to the top: the window has no answer to wait for
    /// before it shows the type under the caret.
    fn load(&mut self, cx: &mut Context<Self>) {
        let (Some(support), Some(spot)) = (self.support.clone(), self.spot.clone()) else {
            return;
        };
        let Some(provider) = support.type_hierarchy() else {
            return;
        };
        self.serial += 1;
        let serial = self.serial;
        self.nodes.clear();
        self.roots.clear();
        self.selected = None;
        // The requests of the previous root and view are gone with their replies.
        self.pending.clear();
        cx.notify();
        let view = self.view;
        let mut replies = ask(provider, TypeHierarchyQuery::Root(spot));
        let task = cx.spawn(async move |this, cx| {
            let roots = replies.next().await.and_then(|answer| {
                (answer.confidence != Confidence::Unknown).then_some(answer.value)
            });
            this.update(cx, |this, cx| {
                if this.serial != serial {
                    return;
                }
                let Some(item) = roots.and_then(|items| items.into_iter().next()) else {
                    return;
                };
                this.nodes = vec![Node::new(item.clone(), 0)];
                this.roots = vec![0];
                this.selected = Some(0);
                cx.notify();
                if view == HierarchyView::Type {
                    this.walk_supertypes(serial, item, cx);
                } else {
                    this.expand(0, cx);
                }
            })
            .ok();
        });
        self.pending.push(task);
    }

    /// The Type view puts the topmost superclass above the selected type: the chain
    /// grows as the answers come, then the selected type shows its subtypes.
    fn walk_supertypes(&mut self, serial: u64, first: TypeHierarchyItem, cx: &mut Context<Self>) {
        let Some(support) = self.support.clone() else {
            return;
        };
        let cancel = self.cancel.clone();
        let task = cx.spawn(async move |this, cx| {
            let Some(provider) = support.type_hierarchy() else {
                return;
            };
            let mut chain = vec![first];
            while !cancel.is_cancelled() && chain.len() < MAX_ANCESTORS {
                let Some(current) = chain.last().cloned() else {
                    break;
                };
                let mut replies = ask(
                    provider,
                    TypeHierarchyQuery::Neighbours {
                        item: current,
                        direction: HierarchyDirection::Supertypes,
                    },
                );
                let Some(answer) = replies.next().await else {
                    break;
                };
                if answer.confidence == Confidence::Unknown {
                    break;
                }
                let Some(parent) = pick_parent(&answer.value, &chain) else {
                    break;
                };
                chain.push(parent);
                let shown = chain.clone();
                let updated = this.update(cx, |this, cx| {
                    if this.serial != serial {
                        return false;
                    }
                    this.show_chain(&shown);
                    cx.notify();
                    true
                });
                if !matches!(updated, Ok(true)) {
                    return;
                }
            }
            this.update(cx, |this, cx| {
                if this.serial != serial {
                    return;
                }
                if let Some(index) = this.selected {
                    this.load_children(index, cx);
                }
            })
            .ok();
        });
        self.pending.push(task);
    }

    /// Draws the chain of supertypes, the selected type first in `chain` and the
    /// topmost ancestor last.
    fn show_chain(&mut self, chain: &[TypeHierarchyItem]) {
        let mut nodes: Vec<Node> = chain
            .iter()
            .rev()
            .enumerate()
            .map(|(depth, item)| {
                let mut node = Node::new(item.clone(), depth);
                node.expanded = true;
                node
            })
            .collect();
        for index in 0..nodes.len().saturating_sub(1) {
            nodes[index].children = vec![index + 1];
        }
        let selected = nodes.len().saturating_sub(1);
        self.nodes = nodes;
        self.roots = vec![0];
        self.selected = Some(selected);
    }

    /// The node indices on screen, depth first, and their depths. A row the scope hides
    /// takes its whole branch with it.
    fn visible(&self) -> Vec<(usize, usize)> {
        fn add(panel: &TypeHierarchyPanel, index: usize, found: &mut Vec<(usize, usize)>) {
            let Some(node) = panel.nodes.get(index) else {
                return;
            };
            if !panel.in_scope(node) {
                return;
            }
            found.push((index, node.depth));
            if node.expanded {
                for child in &node.children {
                    add(panel, *child, found);
                }
            }
        }
        let mut found = Vec::new();
        for root in &self.roots {
            add(self, *root, &mut found);
        }
        found
    }

    /// Whether the scope keeps a row. A type outside the project, such as one of the JDK
    /// or a library, has no folder of the model and stays in every scope.
    fn in_scope(&self, node: &Node) -> bool {
        self.scope_roots.keeps(
            self.scope,
            node.item.place.as_ref().map(|place| place.uri.as_str()),
        )
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let rows = self.visible();
        if rows.is_empty() {
            return;
        }
        let current = self
            .selected
            .and_then(|selected| rows.iter().position(|(index, _)| *index == selected));
        let next = match current {
            Some(at) => (at as isize + delta).clamp(0, rows.len() as isize - 1) as usize,
            None if delta < 0 => rows.len() - 1,
            None => 0,
        };
        self.selected = Some(rows[next].0);
        cx.notify();
    }

    fn activate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.selected else {
            return;
        };
        let Some(target) = self
            .nodes
            .get(index)
            .and_then(|node| node.item.place.clone())
        else {
            return;
        };
        let Some(path) = editor_view::uri_path(&target.uri) else {
            return;
        };
        let spot = editor_view::Spot::Line {
            line: target.span.start.line as usize,
            column: target.span.start.character as usize,
        };
        let workspace = self.workspace.clone();
        let handle = window.window_handle();
        cx.defer(move |cx| {
            handle
                .update(cx, |_, window, cx| {
                    workspace
                        .update(cx, |workspace, cx| {
                            navigation::go_to(workspace, path, Some(spot), true, window, cx)
                        })
                        .ok();
                })
                .ok();
        });
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        match key {
            "up" => {
                cx.stop_propagation();
                self.move_selection(-1, cx);
            }
            "down" => {
                cx.stop_propagation();
                self.move_selection(1, cx);
            }
            "right" => {
                cx.stop_propagation();
                if let Some(selected) = self.selected {
                    if self.nodes.get(selected).is_some_and(|node| node.expanded) {
                        self.move_selection(1, cx);
                    } else {
                        self.expand(selected, cx);
                    }
                }
            }
            "left" => {
                cx.stop_propagation();
                if let Some(selected) = self.selected
                    && self.nodes.get(selected).is_some_and(|node| node.expanded)
                {
                    self.expand(selected, cx);
                }
            }
            "enter" => {
                cx.stop_propagation();
                self.activate(window, cx);
            }
            "=" if event.keystroke.modifiers.control => {
                cx.stop_propagation();
                self.expand_all(cx);
            }
            "-" if event.keystroke.modifiers.control => {
                cx.stop_propagation();
                self.collapse_all(cx);
            }
            _ => {}
        }
    }
}

/// The parent the Type view puts above a type: a class rather than an interface when
/// the server names both, and never a type that is already in the chain.
fn pick_parent(
    supertypes: &[TypeHierarchyItem],
    chain: &[TypeHierarchyItem],
) -> Option<TypeHierarchyItem> {
    let fresh =
        |candidate: &TypeHierarchyItem| !chain.iter().any(|seen| seen.name == candidate.name);
    supertypes
        .iter()
        .find(|candidate| {
            fresh(candidate)
                && matches!(
                    candidate.kind,
                    TypeKind::Class | TypeKind::Record | TypeKind::Enum
                )
        })
        .or_else(|| supertypes.iter().find(|candidate| fresh(candidate)))
        .cloned()
}

/// The icon of a type, with its own look for an abstract class.
fn icon_of(kind: TypeKind, abstract_type: bool) -> &'static str {
    match (kind, abstract_type) {
        (TypeKind::Class, true) => "nodes/abstractClass",
        (TypeKind::Class, false) => "nodes/class",
        (TypeKind::Interface | TypeKind::Annotation, _) => "nodes/interface",
        (TypeKind::Enum, _) => "nodes/enum",
        (TypeKind::Record, _) => "nodes/record",
    }
}

fn view_button(
    id: &'static str,
    label: &'static str,
    active: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let text = if active {
        theme.accent
    } else {
        theme.text_secondary
    };
    div()
        .id(id)
        .debug_selector(move || id.to_owned())
        .px(px(SPACE_S))
        .py(px(SPACE_XS))
        .rounded(px(CONTROL_RADIUS))
        .text_size(px(TEXT_SMALL))
        .text_color(text)
        .when(active, |button| button.bg(theme.hover))
        .child(label)
}

impl Render for TypeHierarchyPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        self.refresh_scope_roots(cx);
        let workspace = self.workspace.clone();
        let options = crate::tool_menu::options_menu(
            workspace.clone(),
            workspace::TYPE_HIERARCHY_TOOL,
            |_| Vec::new(),
        );
        let hide = move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
            workspace
                .update(cx, |workspace, cx| {
                    workspace.toggle_tool(TYPE_HIERARCHY_TOOL, window, cx)
                })
                .ok();
        };
        let view = self.view;
        let scope = self.scope;
        let switch = div()
            .flex()
            .items_center()
            .gap_2()
            .px(px(ROW_MARGIN))
            .pb_1()
            .child(
                view_button(
                    "type-hierarchy-view-type",
                    "Type Hierarchy",
                    view == HierarchyView::Type,
                    &theme,
                )
                .on_click(cx.listener(|this, _, _, cx| this.set_view(HierarchyView::Type, cx))),
            )
            .child(
                view_button(
                    "type-hierarchy-view-supertypes",
                    "Supertypes Hierarchy",
                    view == HierarchyView::Supertypes,
                    &theme,
                )
                .on_click(
                    cx.listener(|this, _, _, cx| this.set_view(HierarchyView::Supertypes, cx)),
                ),
            )
            .child(
                view_button(
                    "type-hierarchy-view-subtypes",
                    "Subtypes Hierarchy",
                    view == HierarchyView::Subtypes,
                    &theme,
                )
                .on_click(cx.listener(|this, _, _, cx| this.set_view(HierarchyView::Subtypes, cx))),
            )
            .child(div().flex_1())
            .children(header_controls(
                "type-hierarchy",
                scope,
                &theme,
                cx,
                Self::set_scope,
                Self::expand_all,
                Self::collapse_all,
            ));
        let rows: Vec<_> = self
            .visible()
            .into_iter()
            .map(|(index, depth)| {
                let node = &self.nodes[index];
                let selected = self.selected == Some(index);
                let expandable = node.expandable();
                let open = node.expanded;
                let label: SharedString = node.item.name.clone().into();
                let hint: Option<SharedString> = node.item.detail.clone().map(Into::into);
                let icon = icon_of(node.item.kind, node.item.abstract_type);
                let id = ("type-hierarchy-row", index).into();
                let row = tree_row(
                    TreeRow {
                        id,
                        depth,
                        label,
                        hint,
                        icon,
                        expandable,
                        open,
                        selected,
                    },
                    &theme,
                );
                div()
                    .w_full()
                    .px(px(ROW_MARGIN))
                    .child(row.on_click(cx.listener(
                        move |this, event: &ClickEvent, window, cx| {
                            this.selected = Some(index);
                            if event.click_count() >= 2 {
                                this.activate(window, cx);
                            } else if this.nodes.get(index).is_some_and(|node| node.expandable()) {
                                this.expand(index, cx);
                            }
                            cx.notify();
                        },
                    )))
            })
            .collect();
        let empty = rows.is_empty();
        island(&theme)
            .id("type-hierarchy-panel")
            .debug_selector(|| "type-hierarchy-panel".to_owned())
            .key_context("TypeHierarchyPanel ToolWindow")
            .track_focus(&self.focus)
            .group(GROUP)
            .h_full()
            .on_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)))
            .child(panel_header(
                TITLE,
                GROUP,
                "type-hierarchy",
                options,
                hide,
                &theme,
            ))
            .child(switch)
            .child(
                scrollable(
                    "type-hierarchy-rows",
                    &self.scroll,
                    div()
                        .id("type-hierarchy-rows")
                        .flex()
                        .flex_col()
                        .pt(px(SPACE_S))
                        .overflow_y_scroll()
                        .track_scroll(&self.scroll)
                        .children(rows)
                        .when(empty, |this| {
                            this.child(
                                div()
                                    .px(px(ROW_MARGIN + SPACE_M))
                                    .text_color(theme.text_secondary)
                                    .child("No type hierarchy here"),
                            )
                        }),
                    &theme,
                    window,
                    cx,
                )
                .flex_1()
                .min_h_0(),
            )
    }
}

impl Drop for TypeHierarchyPanel {
    fn drop(&mut self) {
        // The window is gone: the deep expansion stops asking, and the requests still
        // waiting for an answer are dropped with their replies.
        self.cancel.cancel();
        self.pending.clear();
    }
}
