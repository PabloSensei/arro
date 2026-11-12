//! The Call Hierarchy tool window: the calls that reach the function at the caret, or
//! the calls it makes. Expanding a row asks the language for the next level.

use std::sync::Arc;

use ::base::Cancel;
use editor_view::Semantic;
use futures_util::StreamExt;
use lang_api::{
    CallDirection, CallHierarchyItem, CallHierarchyQuery, Confidence, DocumentSpot,
    LanguageSupport, ask,
};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::{CALL_HIERARCHY_TOOL, Workspace};

use crate::navigation;

const GROUP: &str = "left-panel";
const ROW_MARGIN: f32 = SPACE_L;
/// Expand All loads every branch down to this depth.
const MAX_DEPTH: usize = 5;

struct Node {
    item: CallHierarchyItem,
    depth: usize,
    expanded: bool,
    loading: bool,
    /// The children of this node have been asked for, even when there are none.
    loaded: bool,
    children: Vec<usize>,
}

/// The window that shows the call hierarchy of one place.
pub struct CallHierarchyPanel {
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    support: Option<Arc<dyn LanguageSupport>>,
    spot: Option<DocumentSpot>,
    direction: CallDirection,
    title: String,
    nodes: Vec<Node>,
    roots: Vec<usize>,
    selected: Option<usize>,
    scroll: ScrollHandle,
    /// Bumped on every new root, so a late answer is dropped.
    serial: u64,
    /// Set when the window goes away, so the deep expansion stops asking.
    cancel: Cancel,
}

/// The panel of the window, for the editor tab that receives the place.
pub struct ActiveCallHierarchy(pub Entity<CallHierarchyPanel>);

impl Global for ActiveCallHierarchy {}

impl CallHierarchyPanel {
    pub fn create(workspace: WeakEntity<Workspace>, cx: &mut App) -> Entity<Self> {
        cx.new(|cx| Self {
            workspace,
            focus: cx.focus_handle(),
            support: None,
            spot: None,
            direction: CallDirection::Incoming,
            title: "Call Hierarchy".to_owned(),
            nodes: Vec::new(),
            roots: Vec::new(),
            selected: None,
            scroll: ScrollHandle::new(),
            serial: 0,
            cancel: Cancel::new(),
        })
    }

    pub fn focus(&self) -> FocusHandle {
        self.focus.clone()
    }

    #[allow(dead_code)]
    pub fn direction(&self) -> CallDirection {
        self.direction
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
        let serial = self.serial;
        let direction = self.direction;
        let cancel = self.cancel.clone();
        cx.spawn(async move |this, cx| {
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
                let Some(mut replies) = ({
                    let Some(provider) = support.call_hierarchy() else {
                        break;
                    };
                    Some(ask(provider, CallHierarchyQuery::Calls { item, direction }))
                }) else {
                    break;
                };
                let calls = replies.next().await.and_then(|answer| {
                    (answer.confidence != Confidence::Unknown).then_some(answer.value)
                });
                let updated = this.update(cx, |this, cx| {
                    if this.serial != serial {
                        return;
                    }
                    let Some(node) = this.nodes.get_mut(index) else {
                        return;
                    };
                    node.loading = false;
                    node.loaded = true;
                    node.expanded = true;
                    if let Some(calls) = calls {
                        let depth = node.depth + 1;
                        let start = this.nodes.len();
                        for item in calls {
                            this.nodes.push(Node {
                                item,
                                depth,
                                expanded: false,
                                loading: false,
                                loaded: false,
                                children: Vec::new(),
                            });
                        }
                        let children: Vec<usize> = (start..this.nodes.len()).collect();
                        if let Some(node) = this.nodes.get_mut(index) {
                            node.children = children;
                        }
                    }
                    cx.notify();
                });
                if updated.is_err() {
                    return;
                }
            }
        })
        .detach();
    }

    /// The first node below [`MAX_DEPTH`] whose children have not been asked for.
    fn next_to_load(&self) -> Option<usize> {
        self.nodes
            .iter()
            .position(|node| node.depth < MAX_DEPTH && !node.loaded && !node.loading)
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
                let hint = self.nodes[index]
                    .item
                    .detail
                    .as_deref()
                    .map_or(String::new(), |detail| format!("  {detail}"));
                format!(
                    "{}{}{hint}",
                    "  ".repeat(depth),
                    self.nodes[index].item.name
                )
            })
            .collect()
    }

    /// Ctrl+Alt+H: the hierarchy of the function at `spot`.
    pub fn start(&mut self, spot: DocumentSpot, cx: &mut Context<Self>) {
        self.support = cx
            .try_global::<Semantic>()
            .map(|semantic| semantic.0.clone());
        if self.support.is_none() {
            return;
        }
        self.spot = Some(spot);
        self.direction = CallDirection::Incoming;
        self.load(cx);
    }

    /// The switch in the header: callers or callees.
    pub fn set_direction(&mut self, direction: CallDirection, cx: &mut Context<Self>) {
        if self.direction == direction {
            return;
        }
        self.direction = direction;
        self.load(cx);
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        let (Some(support), Some(spot)) = (self.support.clone(), self.spot.clone()) else {
            return;
        };
        let Some(provider) = support.call_hierarchy() else {
            return;
        };
        self.serial += 1;
        let serial = self.serial;
        self.nodes.clear();
        self.roots.clear();
        self.selected = None;
        self.title = "Call Hierarchy".to_owned();
        cx.notify();
        let mut replies = ask(provider, CallHierarchyQuery::Roots(spot));
        cx.spawn(async move |this, cx| {
            let roots = replies.next().await.and_then(|answer| {
                (answer.confidence != Confidence::Unknown).then_some(answer.value)
            });
            this.update(cx, |this, cx| {
                if this.serial != serial {
                    return;
                }
                let Some(items) = roots else {
                    return;
                };
                this.nodes = items
                    .into_iter()
                    .map(|item| Node {
                        item,
                        depth: 0,
                        expanded: false,
                        loading: false,
                        loaded: false,
                        children: Vec::new(),
                    })
                    .collect();
                this.roots = (0..this.nodes.len()).collect();
                this.selected = (!this.nodes.is_empty()).then_some(0);
                if let Some(root) = this.nodes.first() {
                    this.title = root.item.name.clone();
                }
                cx.notify();
                if !this.nodes.is_empty() {
                    this.expand(0, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    fn expand(&mut self, index: usize, cx: &mut Context<Self>) {
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
        let (Some(support), Some(provider)) = (
            self.support.clone(),
            self.support
                .as_ref()
                .and_then(|support| support.call_hierarchy()),
        ) else {
            return;
        };
        let _ = support;
        let item = node.item.clone();
        node.loading = true;
        let serial = self.serial;
        let direction = self.direction;
        cx.notify();
        let mut replies = ask(provider, CallHierarchyQuery::Calls { item, direction });
        cx.spawn(async move |this, cx| {
            let calls = replies.next().await.and_then(|answer| {
                (answer.confidence != Confidence::Unknown).then_some(answer.value)
            });
            this.update(cx, |this, cx| {
                if this.serial != serial {
                    return;
                }
                let Some(calls) = calls else {
                    return;
                };
                if let Some(node) = this.nodes.get_mut(index) {
                    node.loading = false;
                    node.loaded = true;
                    node.expanded = true;
                    let depth = node.depth + 1;
                    let start = this.nodes.len();
                    for item in calls {
                        this.nodes.push(Node {
                            item,
                            depth,
                            expanded: false,
                            loading: false,
                            loaded: false,
                            children: Vec::new(),
                        });
                    }
                    let children: Vec<usize> = (start..this.nodes.len()).collect();
                    if let Some(node) = this.nodes.get_mut(index) {
                        node.children = children;
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The node indices on screen, depth first, and their depths.
    fn visible(&self) -> Vec<(usize, usize)> {
        fn add(nodes: &[Node], index: usize, found: &mut Vec<(usize, usize)>) {
            let Some(node) = nodes.get(index) else {
                return;
            };
            found.push((index, node.depth));
            if node.expanded {
                for child in &node.children {
                    add(nodes, *child, found);
                }
            }
        }
        let mut found = Vec::new();
        for root in &self.roots {
            add(&self.nodes, *root, &mut found);
        }
        found
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

    fn selected_node(&self) -> Option<usize> {
        self.selected
    }

    fn activate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.selected_node() else {
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
            _ => {}
        }
    }
}

impl Render for CallHierarchyPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let workspace = self.workspace.clone();
        let hide = move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
            workspace
                .update(cx, |workspace, cx| {
                    workspace.toggle_tool(CALL_HIERARCHY_TOOL, window, cx)
                })
                .ok();
        };
        let direction = self.direction;
        let rows: Vec<_> = self
            .visible()
            .into_iter()
            .map(|(index, depth)| {
                let node = &self.nodes[index];
                let selected = self.selected == Some(index);
                let expandable = !node.children.is_empty() || node.loading;
                let open = node.expanded;
                let label: SharedString = node.item.name.clone().into();
                let hint: Option<SharedString> = node.item.detail.clone().map(Into::into);
                let id = ("call-hierarchy-row", index).into();
                let row = tree_row(
                    TreeRow {
                        id,
                        depth,
                        label,
                        hint,
                        icon: "nodes/method",
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
        let empty = self.nodes.is_empty();
        let title = self.title.clone();
        let incoming = direction == CallDirection::Incoming;
        let switch = div()
            .flex()
            .gap_2()
            .px(px(ROW_MARGIN))
            .pb_1()
            .child(
                div()
                    .id("call-hierarchy-callers")
                    .when(incoming, |row| row.text_color(theme.accent))
                    .child("Callers")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.set_direction(CallDirection::Incoming, cx)
                    })),
            )
            .child(
                div()
                    .id("call-hierarchy-callees")
                    .when(!incoming, |row| row.text_color(theme.accent))
                    .child("Callees")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.set_direction(CallDirection::Outgoing, cx)
                    })),
            );
        island(&theme)
            .id("call-hierarchy-panel")
            .key_context("CallHierarchyPanel ToolWindow")
            .track_focus(&self.focus)
            .group(GROUP)
            .h_full()
            .on_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)))
            .child(panel_header(title, GROUP, hide, &theme))
            .child(switch)
            .child(
                scrollable(
                    "call-hierarchy-rows",
                    &self.scroll,
                    div()
                        .id("call-hierarchy-rows")
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
                                    .child("No calls found"),
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

impl Drop for CallHierarchyPanel {
    fn drop(&mut self) {
        // The window is gone: the deep expansion stops asking and drops its answers.
        self.cancel.cancel();
    }
}

impl Node {
    fn expandable(&self) -> bool {
        self.loading || !self.children.is_empty() || !self.expanded
    }
}
