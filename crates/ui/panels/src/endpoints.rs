//! The Endpoints tool window: the HTTP handlers of the project as a tree of module,
//! type and `GET /owners/{ownerId}`. A handler opens in its source, becomes a request in
//! the HTTP client, or copies its URL.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use editor_view::{EditorView, Spot};
use lang_api::{Endpoint, EndpointParameterKind, EndpointScan, HttpMethod, LanguageSupport};
use runner::LiveRequest;
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::Workspace;

use crate::navigation;

const GROUP: &str = "bottom-panel";
/// The tool window state that keeps the filter while the panel is open.
const FILTER_CONTEXT: &str = "EndpointsPanel";
/// The port a generated request uses when the module names none.
const DEFAULT_PORT: u16 = 8080;
/// The request file the HTTP client opens, in the folder of the scratch files.
const REQUEST_FILE: &str = "generated-requests.http";
/// The methods the filter offers.
const FILTERS: [HttpMethod; 7] = [
    HttpMethod::Get,
    HttpMethod::Post,
    HttpMethod::Put,
    HttpMethod::Delete,
    HttpMethod::Patch,
    HttpMethod::Head,
    HttpMethod::Options,
];

/// One row of the tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EndpointRow {
    pub kind: EndpointRowKind,
    pub label: String,
    pub depth: usize,
    /// The place of the handler in [`EndpointsPanel::endpoints`] for a handler row.
    pub endpoint: Option<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndpointRowKind {
    Module,
    Class,
    Endpoint,
}

/// A handler with the port of its module.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Shown {
    endpoint: Endpoint,
    port: Option<u16>,
}

/// The panel of the window, for the gutter menu and the tests.
pub struct ActiveEndpoints(pub Entity<EndpointsPanel>);

impl Global for ActiveEndpoints {}

pub struct EndpointsPanel {
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    files: Entity<Arc<Vec<PathBuf>>>,
    support: Option<Arc<dyn LanguageSupport>>,
    scans: HashMap<PathBuf, EndpointScan>,
    /// Bumped per file, so a slow scan does not replace a newer text.
    versions: HashMap<PathBuf, u64>,
    query: TextField,
    method: Option<HttpMethod>,
    selected: Option<usize>,
    rows: Vec<EndpointRow>,
    endpoints: Vec<Shown>,
    scroll: UniformListScrollHandle,
    _files: Subscription,
}

impl EndpointsPanel {
    pub fn create(
        files: Entity<Arc<Vec<PathBuf>>>,
        workspace: WeakEntity<Workspace>,
        cx: &mut App,
    ) -> Entity<Self> {
        cx.new(|cx| {
            let _files = cx.observe(&files, |panel: &mut Self, _, cx| panel.sync_files(cx));
            Self {
                workspace,
                focus: cx.focus_handle(),
                files,
                support: None,
                scans: HashMap::new(),
                versions: HashMap::new(),
                query: TextField::default(),
                method: None,
                selected: None,
                rows: Vec::new(),
                endpoints: Vec::new(),
                scroll: UniformListScrollHandle::new(),
                _files,
            }
        })
    }

    pub fn focus(&self) -> FocusHandle {
        self.focus.clone()
    }

    /// The number of handlers the tree shows.
    pub fn handlers(&self) -> usize {
        self.endpoints.len()
    }

    /// The rows of the tree, group rows included.
    pub fn rows(&self) -> &[EndpointRow] {
        &self.rows
    }

    /// The language is up: the scan of the project can start.
    pub fn language_ready(&mut self, cx: &mut Context<Self>) {
        self.support = cx
            .try_global::<editor_view::Semantic>()
            .map(|semantic| semantic.0.clone());
        self.sync_files(cx);
    }

    /// The files of the project are new: the handlers of the missing ones are read.
    pub fn sync_files(&mut self, cx: &mut Context<Self>) {
        let Some(support) = self.support.clone() else {
            return;
        };
        if support.endpoints().is_none() {
            return;
        }
        let listed = self.files.read(cx).clone();
        let wanted: HashSet<PathBuf> = listed
            .iter()
            .filter(|path| support.matches(path))
            .cloned()
            .collect();
        self.scans.retain(|path, _| wanted.contains(path));
        self.versions.retain(|path, _| wanted.contains(path));
        let missing: Vec<(PathBuf, Option<String>)> = wanted
            .iter()
            .filter(|path| !self.scans.contains_key(*path))
            .map(|path| (path.clone(), None))
            .collect();
        if !missing.is_empty() {
            self.scan_paths(missing, cx);
        }
        self.rebuild();
    }

    /// A file on disk changed: its handlers are read again, or it leaves the list.
    pub(crate) fn disk_changed(&mut self, changes: &[fs_watch::FsChange], cx: &mut Context<Self>) {
        let Some(support) = self.support.clone() else {
            return;
        };
        let mut scan = Vec::new();
        let mut removed = false;
        for change in changes {
            match change {
                fs_watch::FsChange::Created(path) | fs_watch::FsChange::Changed(path)
                    if support.matches(path) =>
                {
                    scan.push((path.clone(), None));
                }
                fs_watch::FsChange::Removed(path) => {
                    self.scans.remove(path);
                    self.versions.remove(path);
                    removed = true;
                }
                fs_watch::FsChange::Rescan => {
                    self.sync_files(cx);
                    return;
                }
                _ => {}
            }
        }
        if !scan.is_empty() {
            self.scan_paths(scan, cx);
        }
        if removed {
            self.rebuild();
            cx.notify();
        }
    }

    /// A file changed in an editor: its handlers are read from the text on screen.
    pub fn file_changed(&mut self, path: PathBuf, text: Option<String>, cx: &mut Context<Self>) {
        let Some(support) = self.support.clone() else {
            return;
        };
        if !support.matches(&path) {
            return;
        }
        self.scan_paths(vec![(path, text)], cx);
    }

    /// Reads the handlers of the paths in the background. A newer text of a path wins.
    fn scan_paths(&mut self, paths: Vec<(PathBuf, Option<String>)>, cx: &mut Context<Self>) {
        let Some(support) = self.support.clone() else {
            return;
        };
        if support.endpoints().is_none() {
            return;
        }
        let mut requested = Vec::with_capacity(paths.len());
        for (path, text) in paths {
            let version = self.versions.entry(path.clone()).or_insert(0);
            *version += 1;
            requested.push((path, text, *version));
        }
        let task = cx.background_executor().spawn(async move {
            let Some(provider) = support.endpoints() else {
                return Vec::new();
            };
            let mut scans = Vec::new();
            for (path, text, version) in requested {
                let text = text.or_else(|| fs::read_to_string(&path).ok());
                let Some(text) = text else {
                    continue;
                };
                scans.push((path.clone(), version, provider.file_endpoints(&path, &text)));
            }
            scans
        });
        cx.spawn(async move |this, cx| {
            let scans = task.await;
            let applied = this.update(cx, |this, cx| {
                let mut changed = false;
                for (path, version, scan) in scans {
                    if this.versions.get(&path) == Some(&version) {
                        this.scans.insert(path, scan);
                        changed = true;
                    }
                }
                if changed {
                    this.rebuild();
                    cx.notify();
                }
            });
            applied.ok();
        })
        .detach();
    }

    fn module_of(&self, path: &Path) -> String {
        self.support
            .as_ref()
            .and_then(|support| support.module_of(path))
            .unwrap_or_default()
    }

    fn matches(&self, endpoint: &Endpoint) -> bool {
        if let Some(method) = self.method
            && endpoint.method != method
        {
            return false;
        }
        let needle = self.query.text.trim().to_lowercase();
        if needle.is_empty() {
            return true;
        }
        format!(
            "{} {} {}",
            endpoint.class_name(),
            endpoint.method.as_str(),
            endpoint.path
        )
        .to_lowercase()
        .contains(&needle)
    }

    /// Builds the tree from the scans, the filter and the method.
    fn rebuild(&mut self) {
        let mut modules: BTreeMap<String, BTreeMap<String, Vec<Shown>>> = BTreeMap::new();
        for (path, scan) in &self.scans {
            let module = self.module_of(path);
            for endpoint in &scan.endpoints {
                if !self.matches(endpoint) {
                    continue;
                }
                modules
                    .entry(module.clone())
                    .or_default()
                    .entry(endpoint.class.clone())
                    .or_default()
                    .push(Shown {
                        endpoint: endpoint.clone(),
                        port: scan.port,
                    });
            }
        }
        self.endpoints.clear();
        self.rows.clear();
        for (module, classes) in modules {
            self.rows.push(EndpointRow {
                kind: EndpointRowKind::Module,
                label: module,
                depth: 0,
                endpoint: None,
            });
            for (class, handlers) in classes {
                self.rows.push(EndpointRow {
                    kind: EndpointRowKind::Class,
                    label: class.rsplit('.').next().unwrap_or(&class).to_owned(),
                    depth: 1,
                    endpoint: None,
                });
                for shown in handlers {
                    let label =
                        format!("{} {}", shown.endpoint.method.as_str(), shown.endpoint.path);
                    self.endpoints.push(shown);
                    self.rows.push(EndpointRow {
                        kind: EndpointRowKind::Endpoint,
                        label,
                        depth: 2,
                        endpoint: Some(self.endpoints.len() - 1),
                    });
                }
            }
        }
        self.selected = self
            .selected
            .filter(|index| {
                self.rows
                    .get(*index)
                    .is_some_and(|row| row.endpoint.is_some())
            })
            .or_else(|| self.first_endpoint_row());
    }

    fn first_endpoint_row(&self) -> Option<usize> {
        self.rows.iter().position(|row| row.endpoint.is_some())
    }

    fn shown_at(&self, row: usize) -> Option<&Shown> {
        let index = self.rows.get(row)?.endpoint?;
        self.endpoints.get(index)
    }

    /// A click or Enter on a handler row opens its source.
    pub fn open_row(&mut self, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(shown) = self.shown_at(row) else {
            return;
        };
        let path = shown.endpoint.file.clone();
        let offset = shown.endpoint.name_range.start;
        let workspace = self.workspace.clone();
        window.defer(cx, move |window, cx| {
            workspace
                .update(cx, |workspace, cx| {
                    navigation::go_to(
                        workspace,
                        path,
                        Some(Spot::Offset(offset)),
                        true,
                        window,
                        cx,
                    )
                })
                .ok();
        });
    }

    /// A new filter text, as typing changes it.
    pub fn set_query(&mut self, text: impl Into<String>, cx: &mut Context<Self>) {
        self.query = TextField::from_text(text);
        self.rebuild();
        cx.notify();
    }

    /// The method the filter keeps, `None` for all of them.
    pub fn set_method(&mut self, method: Option<HttpMethod>, cx: &mut Context<Self>) {
        self.method = method;
        self.rebuild();
        cx.notify();
    }

    /// Selects the handler of a place, as Show in Endpoints does.
    pub fn show_at(&mut self, path: &Path, line: u32, cx: &mut Context<Self>) {
        self.query = TextField::default();
        self.method = None;
        self.rebuild();
        self.selected = self.rows.iter().enumerate().find_map(|(index, _row)| {
            let shown = self.shown_at(index)?;
            (shown.endpoint.file == path && shown.endpoint.line == line).then_some(index)
        });
        cx.notify();
    }

    /// Generate Request in HTTP Client for the handler of a row.
    pub fn generate_row(&mut self, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(shown) = self.shown_at(row).cloned() else {
            return;
        };
        let workspace = self.workspace.clone();
        window.defer(cx, move |window, cx| {
            workspace
                .update(cx, |workspace, cx| {
                    generate_request(&shown.endpoint, shown.port, workspace, window, cx)
                })
                .ok();
        });
    }

    /// Copy URL for the handler of a row.
    pub fn copy_row_url(&mut self, row: usize, cx: &mut Context<Self>) {
        let Some(shown) = self.shown_at(row) else {
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(url_of(
            &shown.endpoint,
            shown.port,
        )));
        cx.notify();
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let rows = &self.rows;
        let selectable: Vec<usize> = rows
            .iter()
            .enumerate()
            .filter_map(|(index, row)| row.endpoint.map(|_| index))
            .collect();
        if selectable.is_empty() {
            return;
        }
        let next = match self.selected {
            Some(current) => {
                let at = selectable
                    .iter()
                    .position(|index| *index == current)
                    .unwrap_or(0);
                let at = (at as isize + delta).clamp(0, selectable.len() as isize - 1);
                selectable[at as usize]
            }
            None if delta < 0 => *selectable.last().unwrap_or(&selectable[0]),
            None => selectable[0],
        };
        self.selected = Some(next);
        cx.notify();
    }

    fn row_menu(&mut self, row: usize, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(shown) = self.shown_at(row).cloned() else {
            return;
        };
        let endpoint = shown.endpoint;
        let panel = cx.weak_entity();
        let go_workspace = self.workspace.clone();
        let go_endpoint = endpoint.clone();
        let generate_panel = panel.clone();
        let copy_panel = panel;
        let entries = vec![
            MenuEntry::new("Go to Declaration", move |window, cx| {
                let path = go_endpoint.file.clone();
                let offset = go_endpoint.name_range.start;
                go_workspace
                    .update(cx, |workspace, cx| {
                        navigation::go_to(
                            workspace,
                            path,
                            Some(Spot::Offset(offset)),
                            true,
                            window,
                            cx,
                        )
                    })
                    .ok();
            })
            .icon("general/locate"),
            MenuEntry::new("Generate Request in HTTP Client", move |window, cx| {
                generate_panel
                    .update(cx, |panel, cx| panel.generate_row(row, window, cx))
                    .ok();
            })
            .icon("toolwindows/services"),
            MenuEntry::new("Copy URL", move |_, cx| {
                copy_panel
                    .update(cx, |panel, cx| panel.copy_row_url(row, cx))
                    .ok();
            })
            .icon("general/copy"),
        ];
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.show_menu(position, entries, cx)
            })
            .ok();
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        match key {
            "up" => {
                self.move_selection(-1, cx);
                cx.stop_propagation();
            }
            "down" => {
                self.move_selection(1, cx);
                cx.stop_propagation();
            }
            "enter" => {
                if let Some(row) = self.selected {
                    self.open_row(row, window, cx);
                }
                cx.stop_propagation();
            }
            _ => {
                if self.query.key_with_clipboard(event, false, cx) {
                    self.rebuild();
                    cx.stop_propagation();
                    cx.notify();
                }
            }
        }
    }

    fn row_element(
        &self,
        index: usize,
        row: &EndpointRow,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let label = row.label.clone();
        let kind = row.kind;
        let depth = row.depth;
        let selected = self.selected == Some(index) && kind == EndpointRowKind::Endpoint;
        let icon_name = match kind {
            EndpointRowKind::Module => "nodes/module",
            EndpointRowKind::Class => "nodes/class",
            EndpointRowKind::Endpoint => "toolwindows/services",
        };
        let text_color = match kind {
            EndpointRowKind::Endpoint => theme.text,
            _ => theme.text_secondary,
        };
        div()
            .id(("endpoints-row", index))
            .debug_selector(move || format!("endpoints-row-{index}"))
            .flex()
            .items_center()
            .flex_none()
            .w_full()
            .h(px(ROW_HEIGHT))
            .gap(px(SPACE_XS))
            .pl(px(SPACE_M + depth as f32 * SPACE_L))
            .pr(px(SPACE_M))
            .rounded(px(ROW_RADIUS))
            .when(selected, |this| this.bg(theme.selection))
            .hover(move |style| style.bg(theme.selection_hover))
            .on_click(cx.listener(move |this, _, window, cx| {
                window.focus(&this.focus, cx);
                this.selected = Some(index);
                this.open_row(index, window, cx);
                cx.notify();
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    this.selected = Some(index);
                    this.row_menu(index, event.position, cx);
                    cx.notify();
                }),
            )
            .child(icon(icon_name, theme))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(text_color)
                    .child(label),
            )
    }

    fn method_button(
        &self,
        method: Option<HttpMethod>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let label = method.map_or("All", HttpMethod::as_str);
        let active = self.method == method;
        let slot = method
            .and_then(|method| FILTERS.iter().position(|found| *found == method))
            .map_or(0, |index| index + 1);
        let name = format!("Show {label} handlers");
        div()
            .id(("endpoints-method", slot))
            .debug_selector(move || format!("endpoints-method-{label}"))
            .flex()
            .items_center()
            .justify_center()
            .h(px(CONTROL_HEIGHT))
            .px(px(SPACE_S))
            .rounded(px(CONTROL_RADIUS))
            .text_size(px(TEXT_SMALL))
            .text_color(if active {
                theme.text
            } else {
                theme.text_secondary
            })
            .when(active, |this| this.bg(theme.selection))
            .hover(move |style| style.bg(theme.selection_hover))
            .cursor_pointer()
            .tooltip(move |_, cx| tooltip(name.clone(), None, cx))
            .tooltip_show_delay(TOOLTIP_CREATE_DELAY)
            .on_click(cx.listener(move |this, _, _, cx| this.set_method(method, cx)))
            .child(label)
    }
}

impl Render for EndpointsPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(test)]
        workspace::note_view_render(cx, "endpoints");
        let theme = *cx.global::<Theme>();
        let count = self.handlers();
        let header = div()
            .flex()
            .items_center()
            .justify_between()
            .flex_none()
            .h(px(CONTROL_HEIGHT))
            .pl(px(SPACE_M))
            .pr(px(SPACE_S))
            .text_size(px(UI_SIZE))
            .font_weight(FontWeight::MEDIUM)
            .child("Endpoints")
            .child(
                div()
                    .text_color(theme.text_secondary)
                    .text_size(px(TEXT_SMALL))
                    .child(format!("{count}")),
            );
        let search = div()
            .flex()
            .items_center()
            .flex_none()
            .gap(px(SPACE_S))
            .h(px(CONTROL_HEIGHT))
            .px(px(SPACE_M))
            .child(icon("general/search", &theme))
            .child(
                div()
                    .id("endpoints-filter")
                    .debug_selector(|| "endpoints-filter".to_owned())
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .on_click(cx.listener(|this, _, window, cx| window.focus(&this.focus, cx)))
                    .children(text_field_children_named(
                        FieldMouse::with_edit(
                            cx,
                            |this| Some(&mut this.query),
                            |this, _| this.rebuild(),
                        ),
                        "endpoints-filter",
                        &self.query,
                        theme.text,
                        theme.text,
                        theme.selection,
                        theme.caret,
                        16.,
                        true,
                    )),
            )
            .when(!self.query.text.is_empty(), |this| {
                this.child(
                    icon_button(
                        "endpoints-clear",
                        "general/clear",
                        "Clear Filter",
                        None,
                        &theme,
                    )
                    .debug_selector(|| "endpoints-clear".to_owned())
                    .on_click(cx.listener(|this, _, _, cx| this.set_query("", cx))),
                )
            });
        let methods = div()
            .flex()
            .items_center()
            .flex_none()
            .gap(px(SPACE_XS))
            .px(px(SPACE_M))
            .pb(px(SPACE_XS))
            .child(self.method_button(None, &theme, cx))
            .children(
                FILTERS
                    .into_iter()
                    .map(|method| self.method_button(Some(method), &theme, cx)),
            );
        let empty = self.rows.is_empty();
        let hint = empty.then(|| {
            div()
                .px(px(SPACE_L))
                .py(px(SPACE_S))
                .text_color(theme.text_secondary)
                .child("No HTTP handlers found")
        });
        island(&theme)
            .id("endpoints-panel")
            .key_context(FILTER_CONTEXT)
            .track_focus(&self.focus)
            .group(GROUP)
            .h_full()
            .child(header)
            .child(search)
            .child(methods)
            .children(hint)
            .child(
                scrollable(
                    "endpoints-rows",
                    &self.scroll,
                    div().size_full().child(
                        uniform_list(
                            "endpoints-rows",
                            self.rows().len(),
                            cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                                range
                                    .filter_map(|index| {
                                        this.rows.get(index).map(|row| (index, row.clone()))
                                    })
                                    .map(|(index, row)| this.row_element(index, &row, &theme, cx))
                                    .collect::<Vec<_>>()
                            }),
                        )
                        .track_scroll(&self.scroll)
                        .size_full(),
                    ),
                    &theme,
                    window,
                    cx,
                )
                .flex_1()
                .min_h_0(),
            )
            .capture_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)))
    }
}

/// A name as a variable of a request file, `{{name}}`.
fn variable(name: &str) -> String {
    format!("{{{{{name}}}}}")
}

/// The URL of a handler with the placeholders of its path and query.
fn url_of(endpoint: &Endpoint, port: Option<u16>) -> String {
    let mut url = format!(
        "http://localhost:{}{}",
        port.unwrap_or(DEFAULT_PORT),
        endpoint.path
    );
    let query: Vec<String> = endpoint
        .params(EndpointParameterKind::Query)
        .map(|parameter| format!("{}={{{}}}", parameter.name, parameter.name))
        .collect();
    if !query.is_empty() {
        url.push('?');
        url.push_str(&query.join("&"));
    }
    url
}

/// The text of the request a handler becomes. The port is the one of the module, or the
/// usual one of a development server.
pub(crate) fn request_text(endpoint: &Endpoint, port: Option<u16>) -> String {
    let method = endpoint.method.request_method();
    let mut url = format!(
        "http://localhost:{}{}",
        port.unwrap_or(DEFAULT_PORT),
        endpoint.request_path()
    );
    let query: Vec<String> = endpoint
        .params(EndpointParameterKind::Query)
        .map(|parameter| format!("{}={}", parameter.name, variable(&parameter.name)))
        .collect();
    if !query.is_empty() {
        url.push('?');
        url.push_str(&query.join("&"));
    }
    let mut text = format!("{} {url}\n", method.as_str());
    if endpoint
        .params(EndpointParameterKind::Body)
        .next()
        .is_some()
    {
        text.push_str("Content-Type: application/json\n\n{}\n");
    }
    text
}

/// Appends a request for the handler to the request file of the scratch folder, opens it
/// and puts the caret on the request line.
pub(crate) fn generate_request(
    endpoint: &Endpoint,
    port: Option<u16>,
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let title = format!("### {} {}\n", endpoint.method.as_str(), endpoint.path);
    let text = request_text(endpoint, port);
    append_request(&title, &text, workspace, window, cx);
}

/// Appends a request for a path the user met outside the sources, such as a handler of a
/// running application, opens the request file and puts the caret on the request line.
pub(crate) fn generate_url(
    request: &LiveRequest,
    url: &str,
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let title = format!("### {} {}\n", request.method, request.path);
    let text = format!("{} {url}\n", request.method);
    append_request(&title, &text, workspace, window, cx);
}

/// Appends one request to the request file of the scratch folder and puts the caret on
/// its line.
fn append_request(
    title: &str,
    text: &str,
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let folder = workspace.state_dir().join("scratches");
    if let Err(error) = fs::create_dir_all(&folder) {
        workspace.notify_user(
            "HTTP Client",
            format!("The request file cannot be created: {error}"),
            cx,
        );
        return;
    }
    let file = folder.join(REQUEST_FILE);
    let mut content = fs::read_to_string(&file).unwrap_or_default();
    if !content.is_empty() {
        if !content.ends_with('\n') {
            content.push('\n');
        }
        content.push('\n');
    }
    let start = content.len() + title.len();
    content.push_str(title);
    content.push_str(text);
    if let Err(error) = fs::write(&file, content) {
        workspace.notify_user(
            "HTTP Client",
            format!("The request file cannot be written: {error}"),
            cx,
        );
        return;
    }
    navigation::go_to(workspace, file, Some(Spot::Offset(start)), true, window, cx);
}

/// Tells the window that the text of an editor changed, so its handlers are read again.
pub(crate) fn editor_changed(view: &Entity<EditorView>, cx: &mut App) {
    let Some(panel) = cx.try_global::<ActiveEndpoints>() else {
        return;
    };
    let path = view.read(cx).path().to_path_buf();
    let text = view.read(cx).text();
    let panel = panel.0.clone();
    panel.update(cx, |panel, cx| panel.file_changed(path, text, cx));
}
