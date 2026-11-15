//! The Diagram tab: the types around a place or a folder with their ancestors, their
//! subtypes and the fields that tie them together, drawn as a graph of rectangles.

use std::cell::Cell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use editor_view::{Semantic, uri_path};
use lang_api::{
    Confidence, DocumentSpot, GraphEdgeKind, GraphMember, GraphMemberKind, GraphType,
    GraphTypeKind, GraphVisibility, LanguageSupport, SymbolKind, TypeGraph, TypeGraphQuery, ask,
};
use search::SymbolMatch;
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::Workspace;

use crate::diagram_layout::{self, Layout, Measure};
use crate::diagram_svg::{self, SvgOptions};

/// The width of a node before the zoom.
const NODE_WIDTH: f32 = 240.;
/// The height of the header of a node.
const NODE_HEADER: f32 = 30.;
/// The height of one member row.
const MEMBER_ROW: f32 = 20.;
/// The padding of a node around its rows.
const NODE_PADDING: f32 = 8.;
/// The space a separator between two sections takes.
const SECTION_GAP: f32 = 5.;
/// The smallest and the largest zoom of the diagram.
const MIN_ZOOM: f32 = 0.25;
const MAX_ZOOM: f32 = 2.5;
/// One press of a zoom button or key.
const ZOOM_STEP: f32 = 1.2;
/// The space Fit leaves around the diagram.
const FIT_MARGIN: f32 = 24.;
const TAB_PATH_FOLDER: &str = ".diagram";
const TAB_PATH_NAME: &str = "Diagram.diagram";

/// What a diagram is asked about.
#[derive(Clone)]
pub(crate) enum DiagramSource {
    /// The type at a place of a document.
    Spot(DocumentSpot),
    /// Every type of the package in a folder.
    Folder(PathBuf),
}

/// A drag of a node.
#[derive(Clone)]
struct Dragged {
    node: usize,
    /// Where in the node the pointer took it.
    grab: Point<f32>,
}

/// The view of one diagram in a tab of the editor.
pub struct DiagramView {
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    support: Option<Arc<dyn LanguageSupport>>,
    source: Option<DiagramSource>,
    graph: TypeGraph,
    layout: Option<Layout>,
    /// The positions the user dragged the nodes to, in diagram units.
    placed: HashMap<String, Point<f32>>,
    show_fields: bool,
    show_methods: bool,
    show_properties: bool,
    show_dependencies: bool,
    only_public: bool,
    zoom: f32,
    pan: Point<f32>,
    selected: Option<usize>,
    drag: Option<Dragged>,
    pan_from: Option<Point<Pixels>>,
    /// The box of the drawing area of the last frame, in window coordinates.
    bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// The query that is in flight; a late answer of an older one is dropped.
    serial: u64,
    /// The layout that is in flight; a late layout of an older shape is dropped.
    layout_serial: u64,
    add_open: bool,
    add_query: String,
    add_matches: Vec<SymbolMatch>,
    add_selected: usize,
    /// Why the last export did not happen, for the status of the tab.
    note: Option<SharedString>,
}

impl DiagramView {
    fn create(workspace: WeakEntity<Workspace>, cx: &mut App) -> Entity<Self> {
        let support = cx
            .try_global::<Semantic>()
            .map(|semantic| semantic.0.clone());
        cx.new(|cx| Self {
            workspace,
            focus: cx.focus_handle(),
            support,
            source: None,
            graph: TypeGraph::default(),
            layout: None,
            placed: HashMap::new(),
            show_fields: true,
            show_methods: true,
            show_properties: false,
            show_dependencies: true,
            only_public: false,
            zoom: 1.,
            pan: point(0., 0.),
            selected: None,
            drag: None,
            pan_from: None,
            bounds: Rc::new(Cell::new(None)),
            serial: 0,
            layout_serial: 0,
            add_open: false,
            add_query: String::new(),
            add_matches: Vec::new(),
            add_selected: 0,
            note: None,
        })
    }

    pub fn focus(&self) -> FocusHandle {
        self.focus.clone()
    }

    /// The names of the nodes on screen, for tests and scripts.
    #[allow(dead_code)]
    pub fn node_names(&self) -> Vec<String> {
        self.graph
            .nodes
            .iter()
            .map(|node| node.qualified.clone())
            .collect()
    }

    /// The names of the members a node shows now.
    #[allow(dead_code)]
    pub fn member_names(&self, qualified: &str) -> Vec<String> {
        self.graph
            .nodes
            .iter()
            .find(|node| node.qualified == qualified)
            .map(|node| {
                self.shown_members(node)
                    .into_iter()
                    .map(|member| member.name.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Where a node is drawn, in diagram units.
    #[allow(dead_code)]
    pub fn position_of(&self, qualified: &str) -> Option<Point<f32>> {
        let index = self
            .graph
            .nodes
            .iter()
            .position(|node| node.qualified == qualified)?;
        self.place_of(index)
    }

    /// The document the diagram is built from, for tests that wait for an answer.
    #[allow(dead_code)]
    pub fn node_count(&self) -> usize {
        self.graph.nodes.len()
    }

    #[allow(dead_code)]
    pub fn edge_count(&self) -> usize {
        self.graph.edges.len()
    }

    /// Shows a place or a folder, replacing what the tab had.
    pub(crate) fn start(&mut self, source: DiagramSource, cx: &mut Context<Self>) {
        self.source = Some(source);
        self.graph = TypeGraph::default();
        self.layout = None;
        self.placed.clear();
        self.selected = None;
        self.note = None;
        self.serial += 1;
        cx.notify();
        self.load(cx);
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        let (Some(source), Some(support)) = (self.source.clone(), self.support.clone()) else {
            return;
        };
        let Some(provider) = support.type_graph() else {
            self.note = Some("No diagram for this language".into());
            cx.notify();
            return;
        };
        let query = match source {
            DiagramSource::Spot(spot) => TypeGraphQuery::At(spot),
            DiagramSource::Folder(folder) => TypeGraphQuery::Folder(folder),
        };
        let serial = self.serial;
        let mut replies = ask(provider, query);
        cx.spawn(async move |this, cx| {
            use futures_util::StreamExt;
            let answer = replies.next().await;
            this.update(cx, |this, cx| {
                if this.serial != serial {
                    return;
                }
                let Some(answer) = answer else {
                    return;
                };
                if answer.confidence == Confidence::Unknown {
                    this.note = Some("No diagram for this place".into());
                    cx.notify();
                    return;
                }
                this.adopt(answer.value, cx);
            })
            .ok();
        })
        .detach();
    }

    /// Merges a new graph into the open one and lays it out again.
    fn adopt(&mut self, graph: TypeGraph, cx: &mut Context<Self>) {
        self.graph = graph;
        self.relayout(cx);
        cx.notify();
    }

    fn relayout(&mut self, cx: &mut Context<Self>) {
        let measures: Vec<Measure> = self
            .graph
            .nodes
            .iter()
            .map(|node| self.measure(node))
            .collect();
        let positions: HashMap<&str, usize> = self
            .graph
            .nodes
            .iter()
            .enumerate()
            .map(|(index, node)| (node.qualified.as_str(), index))
            .collect();
        let edges: Vec<(usize, usize)> = self
            .graph
            .edges
            .iter()
            .filter(|edge| edge.kind != GraphEdgeKind::Association)
            .filter_map(|edge| {
                Some((
                    *positions.get(edge.from.as_str())?,
                    *positions.get(edge.to.as_str())?,
                ))
            })
            .collect();
        let layout_serial = self.layout_serial + 1;
        self.layout_serial = layout_serial;
        let serial = self.serial;
        let task = cx
            .background_executor()
            .spawn(async move { diagram_layout::compute(&measures, &edges) });
        cx.spawn(async move |this, cx| {
            let layout = task.await;
            this.update(cx, |this, cx| {
                if this.serial != serial || this.layout_serial != layout_serial {
                    return;
                }
                this.layout = Some(layout);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn measure(&self, node: &GraphType) -> Measure {
        let rows = self.shown_members(node).len();
        let sections = self.sections(node).len();
        let separators = sections.saturating_sub(1) as f32 * SECTION_GAP;
        Measure::new(
            NODE_WIDTH,
            NODE_HEADER + NODE_PADDING * 2. + rows as f32 * MEMBER_ROW + separators,
        )
    }

    /// The member sections a node shows: `Field`, `Method`, and the accessors folded by
    /// Properties into the field section.
    fn sections(&self, node: &GraphType) -> Vec<GraphMemberKind> {
        let mut sections = Vec::new();
        if self.show_fields
            && node
                .members
                .iter()
                .any(|member| member.kind == GraphMemberKind::Field && self.shown(member))
        {
            sections.push(GraphMemberKind::Field);
        }
        if self.show_methods
            && node.members.iter().any(|member| {
                member.kind == GraphMemberKind::Method
                    && self.shown(member)
                    && !diagram_svg::is_accessor(member, &self.svg_options())
            })
        {
            sections.push(GraphMemberKind::Method);
        }
        sections
    }

    fn shown(&self, member: &GraphMember) -> bool {
        !self.only_public || member.visibility == GraphVisibility::Public
    }

    fn shown_members<'a>(&self, node: &'a GraphType) -> Vec<&'a GraphMember> {
        let mut fields = Vec::new();
        let mut methods = Vec::new();
        for member in &node.members {
            if !self.shown(member) {
                continue;
            }
            match member.kind {
                GraphMemberKind::Field if self.show_fields => fields.push(member),
                GraphMemberKind::Method
                    if self.show_methods
                        && !diagram_svg::is_accessor(member, &self.svg_options()) =>
                {
                    methods.push(member)
                }
                _ => {}
            }
        }
        fields.extend(methods);
        fields
    }

    fn svg_options(&self) -> SvgOptions {
        SvgOptions {
            fields: self.show_fields,
            methods: self.show_methods,
            properties: self.show_properties,
            dependencies: self.show_dependencies,
            public_only: self.only_public,
        }
    }

    fn place_of(&self, index: usize) -> Option<Point<f32>> {
        let node = self.graph.nodes.get(index)?;
        if let Some(placed) = self.placed.get(&node.qualified) {
            return Some(*placed);
        }
        let placed = self.layout.as_ref()?.nodes.get(index)?;
        Some(point(placed.x, placed.y))
    }

    fn size_of(&self, index: usize) -> (f32, f32) {
        let Some(layout) = &self.layout else {
            return (NODE_WIDTH, NODE_HEADER + NODE_PADDING * 2.);
        };
        layout
            .nodes
            .get(index)
            .map_or((NODE_WIDTH, NODE_HEADER), |placed| {
                (placed.width, placed.height)
            })
    }

    fn to_graph(&self, position: Point<Pixels>) -> Point<f32> {
        let origin = self
            .bounds
            .get()
            .map(|bounds| bounds.origin)
            .unwrap_or_default();
        let local = point(
            f32::from(position.x - origin.x) - self.pan.x,
            f32::from(position.y - origin.y) - self.pan.y,
        );
        point(local.x, local.y)
    }

    fn zoom_at(&mut self, factor: f32, around: Point<Pixels>) {
        let bounds = self
            .bounds
            .get()
            .map(|bounds| bounds.origin)
            .unwrap_or_default();
        let local = point(
            f32::from(around.x - bounds.x),
            f32::from(around.y - bounds.y),
        );
        let before = point(
            (local.x - self.pan.x) / self.zoom,
            (local.y - self.pan.y) / self.zoom,
        );
        self.zoom = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        let after = point(before.x * self.zoom, before.y * self.zoom);
        self.pan = point(local.x - after.x, local.y - after.y);
    }

    fn fit(&mut self) {
        let Some(bounds) = self.bounds.get() else {
            return;
        };
        if self.graph.nodes.is_empty() {
            self.zoom = 1.;
            self.pan = point(0., 0.);
            return;
        }
        let (mut left, mut top) = (f32::MAX, f32::MAX);
        let (mut right, mut bottom) = (f32::MIN, f32::MIN);
        for index in 0..self.graph.nodes.len() {
            let Some(place) = self.place_of(index) else {
                continue;
            };
            let (width, height) = self.size_of(index);
            left = left.min(place.x);
            top = top.min(place.y);
            right = right.max(place.x + width);
            bottom = bottom.max(place.y + height);
        }
        if right <= left || bottom <= top {
            return;
        }
        let width = f32::from(bounds.size.width).max(1.);
        let height = f32::from(bounds.size.height).max(1.);
        let fit_x = (width - FIT_MARGIN * 2.) / (right - left);
        let fit_y = (height - FIT_MARGIN * 2.) / (bottom - top);
        self.zoom = fit_x.min(fit_y).clamp(MIN_ZOOM, MAX_ZOOM);
        self.pan = point(
            (width - (right - left) * self.zoom) / 2. - left * self.zoom,
            (height - (bottom - top) * self.zoom) / 2. - top * self.zoom,
        );
    }

    fn is_visible(&self, index: usize) -> bool {
        let Some(bounds) = self.bounds.get() else {
            return true;
        };
        let Some(place) = self.place_of(index) else {
            return false;
        };
        let (width, height) = self.size_of(index);
        let left = place.x * self.zoom + self.pan.x;
        let top = place.y * self.zoom + self.pan.y;
        let right = left + width * self.zoom;
        let bottom = top + height * self.zoom;
        right > 0.
            && bottom > 0.
            && left < f32::from(bounds.size.width)
            && top < f32::from(bounds.size.height)
    }

    /// Deletes the selected node and the edges that reach it.
    fn delete_selected(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.selected else {
            return;
        };
        let Some(node) = self.graph.nodes.get(index).cloned() else {
            return;
        };
        self.graph
            .nodes
            .retain(|other| other.qualified != node.qualified);
        self.graph
            .edges
            .retain(|edge| edge.from != node.qualified && edge.to != node.qualified);
        self.placed.remove(&node.qualified);
        self.selected = None;
        self.relayout(cx);
        cx.notify();
    }

    fn select(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.selected != Some(index) {
            self.selected = Some(index);
            cx.notify();
        }
    }

    fn open_type(&self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(node) = self.graph.nodes.get(index) else {
            return;
        };
        self.open_place(node.place.clone(), window, cx);
    }

    fn open_place(
        &self,
        place: Option<lang_api::SourceSpan>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(place) = place else {
            return;
        };
        let Some(path) = uri_path(&place.uri) else {
            return;
        };
        let spot = editor_view::Spot::Line {
            line: place.span.start.line as usize,
            column: place.span.start.character as usize,
        };
        let workspace = self.workspace.clone();
        let handle = window.window_handle();
        cx.defer(move |cx| {
            handle
                .update(cx, |_, window, cx| {
                    workspace
                        .update(cx, |workspace, cx| {
                            crate::navigation::go_to(workspace, path, Some(spot), true, window, cx)
                        })
                        .ok();
                })
                .ok();
        });
    }

    /// Searches the project index for a type to add.
    fn search_types(&self, cx: &App) -> Vec<SymbolMatch> {
        let Some(index) = cx.try_global::<crate::index::ActiveIndex>() else {
            return Vec::new();
        };
        let Ok(symbols) = index.0.symbols.read() else {
            return Vec::new();
        };
        let wanted = |symbol: &lang_api::Symbol| {
            matches!(
                symbol.kind,
                SymbolKind::Class | SymbolKind::Interface | SymbolKind::Enum
            )
        };
        symbols
            .search(&self.add_query, wanted, 50)
            .into_iter()
            .map(|ranked| ranked.item)
            .collect()
    }

    /// Adds a type of the project to the open diagram.
    pub(crate) fn add_type(&mut self, qualified: &str, cx: &mut Context<Self>) {
        let Some(support) = self.support.clone() else {
            return;
        };
        let Some(provider) = support.type_graph() else {
            return;
        };
        self.serial += 1;
        let serial = self.serial;
        let query = TypeGraphQuery::Type(qualified.to_owned());
        let mut replies = ask(provider, query);
        cx.spawn(async move |this, cx| {
            use futures_util::StreamExt;
            let answer = replies.next().await;
            this.update(cx, |this, cx| {
                if this.serial != serial {
                    return;
                }
                let Some(answer) = answer else {
                    return;
                };
                if answer.confidence == Confidence::Unknown {
                    return;
                }
                this.merge(answer.value, cx);
            })
            .ok();
        })
        .detach();
    }

    fn merge(&mut self, graph: TypeGraph, cx: &mut Context<Self>) {
        for node in graph.nodes {
            if !self
                .graph
                .nodes
                .iter()
                .any(|known| known.qualified == node.qualified)
            {
                self.graph.nodes.push(node);
            }
        }
        for edge in graph.edges {
            let known = self.graph.edges.iter().any(|other| {
                other.from == edge.from && other.to == edge.to && other.kind == edge.kind
            });
            if !known {
                self.graph.edges.push(edge);
            }
        }
        self.relayout(cx);
        cx.notify();
    }

    fn add_popup_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        let key = event.keystroke.key.to_lowercase();
        match key.as_str() {
            "escape" => self.add_open = false,
            "up" if !self.add_matches.is_empty() => {
                self.add_selected =
                    (self.add_selected + self.add_matches.len() - 1) % self.add_matches.len();
            }
            "down" if !self.add_matches.is_empty() => {
                self.add_selected = (self.add_selected + 1) % self.add_matches.len();
            }
            "backspace" => {
                self.add_query.pop();
                self.refresh_candidates(cx);
            }
            "enter" => {
                let chosen = self.add_matches.get(self.add_selected).map(qualified_of);
                self.add_open = false;
                if let Some(chosen) = chosen {
                    self.add_type(&chosen, cx);
                }
            }
            other if other.chars().count() == 1 && !event.keystroke.modifiers.control => {
                self.add_query.push_str(other);
                self.refresh_candidates(cx);
            }
            _ => {}
        }
        cx.notify();
        true
    }

    fn refresh_candidates(&mut self, cx: &mut Context<Self>) {
        let found = self.search_types(cx);
        self.add_matches = found;
        self.add_selected = 0;
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.add_open {
            self.add_popup_key(event, cx);
            return;
        }
        let viewport = window.viewport_size();
        let center = point(viewport.width / 2., viewport.height / 2.);
        match event.keystroke.key.to_lowercase().as_str() {
            "delete" => self.delete_selected(cx),
            "=" | "+" => {
                self.zoom_at(ZOOM_STEP, center);
                cx.notify();
            }
            "-" => {
                self.zoom_at(1. / ZOOM_STEP, center);
                cx.notify();
            }
            _ => {}
        }
    }

    #[cfg(test)]
    pub(crate) fn export_to(&self, path: &Path) -> std::io::Result<()> {
        let svg = self.export_svg();
        std::fs::write(path, svg)
    }

    pub(crate) fn export_svg(&self) -> String {
        let Some(layout) = &self.layout else {
            return String::new();
        };
        let mut layout = layout.clone();
        let mut width = layout.width;
        let mut height = layout.height;
        for index in 0..layout.nodes.len() {
            let Some(place) = self.place_of(index) else {
                continue;
            };
            let (node_width, node_height) = self.size_of(index);
            let placed = &mut layout.nodes[index];
            placed.x = place.x;
            placed.y = place.y;
            placed.width = node_width;
            placed.height = node_height;
            width = width.max(place.x + node_width + FIT_MARGIN);
            height = height.max(place.y + node_height + FIT_MARGIN);
        }
        layout.width = width;
        layout.height = height;
        diagram_svg::render(&self.graph, &layout, &self.svg_options())
    }

    fn export(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        let directory = workspace.read(cx).root().to_path_buf();
        let chosen = cx.prompt_for_new_path(&directory, Some("diagram.svg"));
        let svg = self.export_svg();
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(path))) = chosen.await else {
                return;
            };
            let target = path.clone();
            let write = cx
                .background_executor()
                .spawn(async move { std::fs::write(&target, svg) });
            let result = write.await;
            this.update(cx, |this, cx| {
                this.note = Some(
                    match &result {
                        Ok(()) => format!("Saved {}", path.display()),
                        Err(error) => format!("{}: {error}", path.display()),
                    }
                    .into(),
                );
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

fn qualified_of(found: &SymbolMatch) -> String {
    match found.symbol.container.as_deref() {
        Some(container) if !container.is_empty() => {
            format!("{}.{}", container.replace('$', "."), found.symbol.name)
        }
        _ => found.symbol.name.clone(),
    }
}

/// The icon of a type, as the Structure tree draws it.
fn icon_of(kind: GraphTypeKind) -> &'static str {
    match kind {
        GraphTypeKind::Class => "nodes/class",
        GraphTypeKind::Interface | GraphTypeKind::Annotation => "nodes/interface",
        GraphTypeKind::Enum => "nodes/enum",
        GraphTypeKind::Record => "nodes/record",
    }
}

/// A getter or a setter, which Properties folds into its field.
impl Render for DiagramView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let toolbar = self.toolbar(&theme, cx);
        let drawing = self.drawing(&theme, window, cx);
        let add = self.add_popup(&theme, cx);
        let note = self.note.clone();
        div()
            .id("diagram-tab")
            .key_context("DiagramTab")
            .track_focus(&self.focus)
            .flex()
            .flex_col()
            .size_full()
            .bg(theme.editor_bg)
            .on_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)))
            .child(toolbar)
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .child(drawing)
                    .children(add),
            )
            .when_some(note, |this, note| {
                this.child(
                    div()
                        .px(px(SPACE_M))
                        .py(px(SPACE_XS))
                        .text_size(px(TEXT_SMALL))
                        .text_color(theme.text_secondary)
                        .child(note),
                )
            })
    }
}

impl DiagramView {
    fn toolbar(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let toggle = |id: &'static str,
                      label: &'static str,
                      on: bool,
                      theme: &Theme,
                      cx: &mut Context<Self>,
                      run: fn(&mut Self, &mut Context<Self>)| {
            let theme = *theme;
            div()
                .id(id)
                .debug_selector(move || id.to_owned())
                .flex()
                .items_center()
                .h(px(24.))
                .px(px(SPACE_S))
                .rounded(px(CONTROL_RADIUS))
                .cursor_pointer()
                .when(on, |this| this.bg(theme.selection))
                .when(!on, |this| this.text_color(theme.text_secondary))
                .tooltip(move |_, cx| tooltip(SharedString::from(label), None, cx))
                .child(label)
                .on_click(cx.listener(move |this, _, _, cx| run(this, cx)))
        };
        let icon = |id: &'static str,
                    name: &'static str,
                    label: &'static str,
                    theme: &Theme,
                    cx: &mut Context<Self>,
                    run: fn(&mut Self, &mut Context<Self>)| {
            icon_button(id, name, label, None, theme)
                .debug_selector(move || id.to_owned())
                .on_click(cx.listener(move |this, _, _, cx| run(this, cx)))
        };
        let mut bar = div()
            .id("diagram-toolbar")
            .flex()
            .items_center()
            .flex_wrap()
            .gap(px(SPACE_XS))
            .px(px(SPACE_M))
            .py(px(SPACE_XS))
            .border_b_1()
            .border_color(theme.toolbar_border);
        bar = bar
            .child(toggle(
                "diagram-fields",
                "Fields",
                self.show_fields,
                theme,
                cx,
                |this, cx| {
                    this.show_fields = !this.show_fields;
                    this.relayout(cx);
                    cx.notify();
                },
            ))
            .child(toggle(
                "diagram-methods",
                "Methods",
                self.show_methods,
                theme,
                cx,
                |this, cx| {
                    this.show_methods = !this.show_methods;
                    this.relayout(cx);
                    cx.notify();
                },
            ))
            .child(toggle(
                "diagram-properties",
                "Properties",
                self.show_properties,
                theme,
                cx,
                |this, cx| {
                    this.show_properties = !this.show_properties;
                    this.relayout(cx);
                    cx.notify();
                },
            ))
            .child(toggle(
                "diagram-dependencies",
                "Show Dependencies",
                self.show_dependencies,
                theme,
                cx,
                |this, cx| {
                    this.show_dependencies = !this.show_dependencies;
                    cx.notify();
                },
            ))
            .child(toggle(
                "diagram-public",
                "Public Only",
                self.only_public,
                theme,
                cx,
                |this, cx| {
                    this.only_public = !this.only_public;
                    this.relayout(cx);
                    cx.notify();
                },
            ))
            .child(div().flex_1())
            .child(icon(
                "diagram-add",
                "general/add",
                "Add Class",
                theme,
                cx,
                |this, cx| {
                    this.add_open = !this.add_open;
                    if this.add_open {
                        this.add_query.clear();
                        this.refresh_candidates(cx);
                    }
                    cx.notify();
                },
            ))
            .child(icon(
                "diagram-fit",
                "general/expandAll",
                "Fit",
                theme,
                cx,
                |this, cx| {
                    this.fit();
                    cx.notify();
                },
            ))
            .child(icon(
                "diagram-zoom-out",
                "general/minus",
                "Zoom Out",
                theme,
                cx,
                |this, cx| {
                    this.zoom = (this.zoom / ZOOM_STEP).clamp(MIN_ZOOM, MAX_ZOOM);
                    cx.notify();
                },
            ))
            .child(icon(
                "diagram-zoom-in",
                "general/add",
                "Zoom In",
                theme,
                cx,
                |this, cx| {
                    this.zoom = (this.zoom * ZOOM_STEP).clamp(MIN_ZOOM, MAX_ZOOM);
                    cx.notify();
                },
            ))
            .child(
                icon_button(
                    "diagram-export",
                    "general/download",
                    "Export to SVG",
                    None,
                    theme,
                )
                .debug_selector(|| "diagram-export".to_owned())
                .on_click(cx.listener(|this, _, window, cx| this.export(window, cx))),
            );
        bar.into_any_element()
    }

    fn drawing(&self, theme: &Theme, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let edges = self.edge_elements(theme);
        let nodes: Vec<AnyElement> = self
            .graph
            .nodes
            .iter()
            .enumerate()
            .filter(|(index, _)| self.is_visible(*index))
            .filter_map(|(index, node)| self.node_element(index, node, theme, cx))
            .collect();
        div()
            .id("diagram-graph")
            .debug_selector(|| "diagram-graph".to_owned())
            .absolute()
            .inset_0()
            .bg(theme.editor_bg)
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    this.pan_from = Some(event.position);
                    this.drag = None;
                    cx.notify();
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                if let Some(dragged) = this.drag.clone() {
                    let at = this.to_graph(event.position);
                    let node = this.graph.nodes.get(dragged.node);
                    if let Some(node) = node {
                        this.placed.insert(
                            node.qualified.clone(),
                            point(at.x - dragged.grab.x, at.y - dragged.grab.y),
                        );
                        cx.notify();
                    }
                    return;
                }
                let Some(from) = this.pan_from else {
                    return;
                };
                if event.pressed_button != Some(MouseButton::Left) {
                    return;
                }
                this.pan = point(
                    this.pan.x + f32::from(event.position.x - from.x),
                    this.pan.y + f32::from(event.position.y - from.y),
                );
                this.pan_from = Some(event.position);
                cx.notify();
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.pan_from = None;
                    this.drag = None;
                    cx.notify();
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.pan_from = None;
                    this.drag = None;
                    cx.notify();
                }),
            )
            .on_scroll(cx.listener(|this, event: &ScrollEvent, _, cx| {
                let delta = match event.delta {
                    ScrollDelta::Pixels(delta) => point(f32::from(delta.x), f32::from(delta.y)),
                    ScrollDelta::Lines(delta) => point(delta.x * MEMBER_ROW, delta.y * MEMBER_ROW),
                };
                if event.modifiers.control {
                    let factor = if delta.y > 0. {
                        ZOOM_STEP
                    } else if delta.y < 0. {
                        1. / ZOOM_STEP
                    } else {
                        1.
                    };
                    this.zoom_at(factor, event.position);
                } else {
                    this.pan = point(this.pan.x + delta.x, this.pan.y + delta.y);
                }
                cx.notify();
            }))
            .child(
                canvas(
                    {
                        let bounds = self.bounds.clone();
                        move |area, _, _| bounds.set(Some(area))
                    },
                    {
                        let bounds = self.bounds.clone();
                        move |area, _, window, cx| {
                            bounds.set(Some(area));
                            paint_edges(&edges, area, window, cx);
                        }
                    },
                )
                .absolute()
                .inset_0(),
            )
            .children(nodes)
            .into_any_element()
    }

    fn node_element(
        &self,
        index: usize,
        node: &GraphType,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let position = self.place_of(index)?;
        let (width, height) = self.size_of(index);
        let zoom = self.zoom;
        let left = position.x * zoom + self.pan.x;
        let top = position.y * zoom + self.pan.y;
        let selected = self.selected == Some(index);
        let background = if node.external {
            theme.fold_bg
        } else {
            theme.island_bg
        };
        let border = if selected { theme.accent } else { theme.border };
        let name_size = (13. * zoom).max(4.);
        let row_size = (12. * zoom).max(4.);
        let mut body = div()
            .flex()
            .flex_col()
            .overflow_hidden()
            .p(px(NODE_PADDING * zoom))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(SPACE_XS * zoom))
                    .h(px((NODE_HEADER - NODE_PADDING) * zoom))
                    .child(icon_sized(icon_of(node.kind), 16. * zoom, theme))
                    .child(
                        div()
                            .text_size(px(name_size))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(if node.external {
                                theme.text_muted
                            } else {
                                theme.text
                            })
                            .when(node.abstract_type, |this| this.italic())
                            .child(if node.abstract_type {
                                format!("{{{}}}", node.name)
                            } else {
                                node.name.clone()
                            }),
                    ),
            );
        let members = if node.external {
            Vec::new()
        } else {
            self.shown_members(node)
        };
        let mut last: Option<GraphMemberKind> = None;
        for (row_index, member) in members.into_iter().enumerate() {
            if last.is_some_and(|kind| kind != member.kind) {
                body = body.child(
                    div()
                        .flex_none()
                        .h(px(SECTION_GAP * zoom))
                        .w_full()
                        .border_b_1()
                        .border_color(theme.border),
                );
            }
            last = Some(member.kind);
            let label = diagram_svg::member_label(member);
            let place = member.place.clone();
            let id = ElementId::NamedInteger(node.qualified.clone().into(), row_index as u64);
            let row = div()
                .id(id)
                .flex()
                .items_center()
                .flex_none()
                .h(px(MEMBER_ROW * zoom))
                .text_size(px(row_size))
                .text_color(if member.visibility == GraphVisibility::Private {
                    theme.text_secondary
                } else {
                    theme.code
                })
                .when(member.abstract_member, |this| this.italic())
                .debug_selector({
                    let qualified = node.qualified.clone();
                    let name = member.name.clone();
                    move || format!("diagram-member-{qualified}-{name}")
                })
                .hover(move |style| style.bg(theme.hover))
                .child(label)
                .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                    if event.click_count() >= 2 {
                        this.open_place(place.clone(), window, cx);
                    }
                    cx.stop_propagation();
                    cx.notify();
                }));
            body = body.child(row);
        }
        let view_width = width * zoom;
        let view_height = height * zoom;
        Some(
            div()
                .id(("diagram-node", index))
                .debug_selector({
                    let qualified = node.qualified.clone();
                    move || format!("diagram-node-{qualified}")
                })
                .absolute()
                .left(px(left))
                .top(px(top))
                .w(px(view_width))
                .h(px(view_height))
                .rounded(px(CONTROL_RADIUS))
                .bg(background)
                .border_1()
                .border_color(border)
                .child(body)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                        this.select(index, cx);
                        let at = this.to_graph(event.position);
                        let place = this.place_of(index).unwrap_or_default();
                        this.drag = Some(Dragged {
                            node: index,
                            grab: point(at.x - place.x, at.y - place.y),
                        });
                        this.pan_from = None;
                        cx.stop_propagation();
                        cx.notify();
                    }),
                )
                .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                    this.select(index, cx);
                    if event.click_count() >= 2 {
                        this.open_type(index, window, cx);
                    }
                    cx.stop_propagation();
                }))
                .into_any_element(),
        )
    }

    fn edge_elements(&self, theme: &Theme) -> Rc<Vec<PaintEdge>> {
        let Some(layout) = &self.layout else {
            return Rc::new(Vec::new());
        };
        let positions: HashMap<&str, usize> = self
            .graph
            .nodes
            .iter()
            .enumerate()
            .map(|(index, node)| (node.qualified.as_str(), index))
            .collect();
        let associations = self.show_dependencies;
        let mut edges = Vec::new();
        for edge in &self.graph.edges {
            if edge.kind == GraphEdgeKind::Association && !associations {
                continue;
            }
            let (Some(from), Some(to)) = (
                positions.get(edge.from.as_str()),
                positions.get(edge.to.as_str()),
            ) else {
                continue;
            };
            let (Some(source), Some(target)) = (layout.nodes.get(*from), layout.nodes.get(*to))
            else {
                continue;
            };
            let source_place = self
                .place_of(*from)
                .unwrap_or_else(|| point(source.x, source.y));
            let target_place = self
                .place_of(*to)
                .unwrap_or_else(|| point(target.x, target.y));
            let source_size = self.size_of(*from);
            let target_size = self.size_of(*to);
            let source_middle = source_place.y + source_size.1 / 2.;
            let target_middle = target_place.y + target_size.1 / 2.;
            let route = if (source_middle - target_middle).abs() < 0.5 {
                // Two nodes of one layer: the edge leaves the side that faces the other.
                let (sx, ex) = if source_place.x <= target_place.x {
                    (source_place.x + source_size.0, target_place.x)
                } else {
                    (source_place.x, target_place.x + target_size.0)
                };
                [
                    point(sx, source_middle),
                    point(sx, source_middle),
                    point(ex, target_middle),
                    point(ex, target_middle),
                ]
            } else if target_middle < source_middle {
                let start = point(source_place.x + source_size.0 / 2., source_place.y);
                let end = point(
                    target_place.x + target_size.0 / 2.,
                    target_place.y + target_size.1,
                );
                let middle = (start.y + end.y) / 2.;
                [start, point(start.x, middle), point(end.x, middle), end]
            } else {
                let start = point(
                    source_place.x + source_size.0 / 2.,
                    source_place.y + source_size.1,
                );
                let end = point(target_place.x + target_size.0 / 2., target_place.y);
                let middle = (start.y + end.y) / 2.;
                [start, point(start.x, middle), point(end.x, middle), end]
            };
            let points =
                route.map(|at| point(at.x * self.zoom + self.pan.x, at.y * self.zoom + self.pan.y));
            edges.push(PaintEdge {
                points,
                kind: edge.kind,
                multiplicity: edge.multiplicity.clone(),
                color: match edge.kind {
                    GraphEdgeKind::Association => theme.text_secondary,
                    _ => theme.text_muted,
                },
            });
        }
        Rc::new(edges)
    }

    fn add_popup(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.add_open {
            return None;
        }
        let rows: Vec<_> = self
            .add_matches
            .iter()
            .enumerate()
            .map(|(index, found)| {
                let label = qualified_of(found);
                let selected = index == self.add_selected;
                let text = label.clone();
                div()
                    .id(("diagram-add-row", index))
                    .debug_selector(move || format!("diagram-add-row-{index}"))
                    .flex()
                    .items_center()
                    .h(px(24.))
                    .px(px(SPACE_S))
                    .rounded(px(CONTROL_RADIUS))
                    .when(selected, |this| this.bg(theme.selection))
                    .child(text)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.add_open = false;
                        this.add_type(&label, cx);
                        cx.notify();
                    }))
                    .into_any_element()
            })
            .collect();
        let query = if self.add_query.is_empty() {
            "Type to search".to_owned()
        } else {
            self.add_query.clone()
        };
        Some(
            div()
                .id("diagram-add-popup")
                .debug_selector(|| "diagram-add-popup".to_owned())
                .absolute()
                .top(px(SPACE_S))
                .right(px(SPACE_M))
                .w(px(360.))
                .max_h(px(320.))
                .overflow_hidden()
                .rounded(px(CONTROL_RADIUS))
                .border_1()
                .border_color(theme.popup_border)
                .bg(theme.popup_bg)
                .shadow_lg()
                .child(
                    div()
                        .h(px(28.))
                        .px(px(SPACE_S))
                        .flex()
                        .items_center()
                        .text_color(theme.text_secondary)
                        .child(query),
                )
                .child(
                    div()
                        .id("diagram-add-results")
                        .flex()
                        .flex_col()
                        .p(px(SPACE_XS))
                        .children(rows),
                )
                .into_any_element(),
        )
    }
}

/// One edge of the drawing, in window coordinates.
struct PaintEdge {
    /// The four corners of the orthogonal route, from the source to the target.
    points: [Point<f32>; 4],
    kind: GraphEdgeKind,
    multiplicity: Option<String>,
    color: Hsla,
}

fn paint_edges(
    edges: &Rc<Vec<PaintEdge>>,
    bounds: Bounds<Pixels>,
    window: &mut Window,
    cx: &mut App,
) {
    for edge in edges.iter() {
        let width = f32::from(bounds.size.width);
        let height = f32::from(bounds.size.height);
        let left = edge.points.iter().map(|at| at.x).fold(f32::MAX, f32::min);
        let right = edge.points.iter().map(|at| at.x).fold(f32::MIN, f32::max);
        let top = edge.points.iter().map(|at| at.y).fold(f32::MAX, f32::min);
        let bottom = edge.points.iter().map(|at| at.y).fold(f32::MIN, f32::max);
        if right < 0. || bottom < 0. || left > width || top > height {
            continue;
        }
        let points = edge
            .points
            .map(|at| point(bounds.left() + px(at.x), bounds.top() + px(at.y)));
        if edge.kind == GraphEdgeKind::Implementation {
            paint_dashed(&points, edge.color, window);
        } else {
            let mut path = PathBuilder::stroke(px(1.2));
            path.move_to(points[0]);
            for at in &points[1..] {
                path.line_to(*at);
            }
            if let Ok(path) = path.build() {
                window.paint_path(path, edge.color);
            }
        }
        let end = points[3];
        let (before, tip) = {
            let mut before = points[3];
            for at in points[..3].iter().rev() {
                if at.x != end.x || at.y != end.y {
                    before = *at;
                    break;
                }
            }
            (before, end)
        };
        let dx = f32::from(tip.x - before.x);
        let dy = f32::from(tip.y - before.y);
        let length = (dx * dx + dy * dy).sqrt().max(0.001);
        let (ux, uy) = (dx / length, dy / length);
        let base = point(tip.x - px(ux * 9.), tip.y - px(uy * 9.));
        let (nx, ny) = (-uy * 5., ux * 5.);
        let left_base = point(base.x + px(nx), base.y + px(ny));
        let right_base = point(base.x - px(nx), base.y - px(ny));
        if edge.kind == GraphEdgeKind::Association {
            let mut arrow = PathBuilder::fill();
            arrow.move_to(tip);
            arrow.line_to(left_base);
            arrow.line_to(right_base);
            arrow.line_to(tip);
            if let Ok(path) = arrow.build() {
                window.paint_path(path, edge.color);
            }
            if let Some(multiplicity) = &edge.multiplicity {
                let style = window.text_style();
                let size = style.font_size.to_pixels(window.rem_size());
                let run = style.to_run(multiplicity.len());
                let line = window.text_system().shape_line(
                    multiplicity.clone().into(),
                    size,
                    &[run],
                    None,
                );
                line.paint(
                    point(tip.x + px(6.), tip.y - px(16.)),
                    px(14.),
                    TextAlign::Left,
                    None,
                    window,
                    cx,
                )
                .ok();
            }
        } else {
            let mut triangle = PathBuilder::stroke(px(1.2));
            triangle.move_to(tip);
            triangle.line_to(left_base);
            triangle.line_to(right_base);
            triangle.line_to(tip);
            if let Ok(path) = triangle.build() {
                window.paint_path(path, edge.color);
            }
        }
    }
}

/// Paints the segments of an implementation edge with dashes.
fn paint_dashed(points: &[Point<Pixels>; 4], color: Hsla, window: &mut Window) {
    const DASH: f32 = 6.;
    const GAP: f32 = 4.;
    for pair in points.windows(2) {
        let (from, to) = (pair[0], pair[1]);
        let dx = f32::from(to.x - from.x);
        let dy = f32::from(to.y - from.y);
        let length = (dx * dx + dy * dy).sqrt();
        if length < 0.5 {
            continue;
        }
        let mut at = 0.;
        while at < length {
            let finish = (at + DASH).min(length);
            let along = |distance: f32| {
                let fraction = distance / length;
                point(from.x + px(dx * fraction), from.y + px(dy * fraction))
            };
            let mut path = PathBuilder::stroke(px(1.2));
            path.move_to(along(at));
            path.line_to(along(finish));
            if let Ok(path) = path.build() {
                window.paint_path(path, color);
            }
            at += DASH + GAP;
        }
    }
}

/// Opens the diagram of a place or a folder, in the tab that shows diagrams.
pub(crate) fn show(
    workspace: &mut Workspace,
    source: DiagramSource,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let path = workspace.root().join(TAB_PATH_FOLDER).join(TAB_PATH_NAME);
    let title = "Diagram";
    let existing = workspace
        .view_of(&path)
        .and_then(|view| view.downcast::<DiagramView>().ok());
    if let Some(view) = existing {
        view.update(cx, |view, cx| view.start(source, cx));
        let focus = view.read(cx).focus();
        workspace.open_view_tab(path, title, "nodes/class", view.into(), focus, window, cx);
        return;
    }
    let weak = cx.weak_entity();
    let view = DiagramView::create(weak, cx);
    let focus = view.read(cx).focus();
    view.update(cx, |view, cx| view.start(source, cx));
    workspace.open_view_tab(path, title, "nodes/class", view.into(), focus, window, cx);
}

/// The diagram of the class under the caret, if there is one.
pub(crate) fn show_at_caret(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(view) = workspace
        .active_view()
        .and_then(|view| view.downcast::<editor_view::EditorView>().ok())
    else {
        return;
    };
    let Some((spot, _)) = view.read(cx).spot() else {
        return;
    };
    show(workspace, DiagramSource::Spot(spot), window, cx);
}

/// Find Action: the diagram of the selected package of the project tree, or of the class
/// under the caret.
pub(crate) fn show_current(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let selected = cx
        .try_global::<crate::project::ActiveProjectPanel>()
        .map(|panel| panel.0.clone())
        .filter(|panel| panel.read(cx).focus().is_focused(window))
        .and_then(|panel| panel.read(cx).selected_path().map(Path::to_path_buf));
    let Some(path) = selected else {
        show_at_caret(workspace, window, cx);
        return;
    };
    let folder = if path.is_dir() {
        path
    } else {
        match path.parent() {
            Some(parent) => parent.to_path_buf(),
            None => return,
        }
    };
    show(workspace, DiagramSource::Folder(folder), window, cx);
}

impl Drop for DiagramView {
    fn drop(&mut self) {
        self.serial += 1;
    }
}
