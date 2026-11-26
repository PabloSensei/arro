//! The Kubernetes node of the Services window: the contexts and namespaces of the
//! kubeconfig, the resources of the chosen namespace with their state, the log of a Pod,
//! and the actions on a resource. The cluster is reached through its CLI, see
//! `services/kube`; nothing of this panel waits for a process.

use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::time::Duration;

use editor_view::{ActionMark, EditorView};
use kube::{
    ClusterState, KubeContext, KubeSnapshot, KubeWatch, Kubectl, PortForward, Resource,
    ResourceKind,
};
use runner::{Console, ConsoleEvent, RunPlan, Stream};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::{TERMINAL_TOOL, Workspace};

use crate::ansi::{Line, OutputBuffers};
use crate::diff_view::DiffView;
use crate::docker::DockerPanel;
use crate::prompt::Prompt;
use crate::run::{error_style, info, line_element, wrapped_line_element};
use crate::terminal_panel::ActiveTerminal;

/// How often the section asks the cluster while it is shown and open.
const EVERY: Duration = Duration::from_secs(3);
const POLL: Duration = Duration::from_millis(100);
const LOG_GRACE: Duration = Duration::from_secs(1);
const MAX_LINES: usize = 20_000;
const EVENTS_PER_POLL: usize = 2_000;
/// Points the section at another `kubectl`, for scripted runs against a fake one.
const CLI_VARIABLE: &str = "IDE_KUBECTL";
const SHELL: &str = "sh";

/// The `kubectl` a window test uses, in place of the one the settings name.
#[cfg(test)]
#[derive(Clone)]
pub(crate) struct TestCli(pub PathBuf);

#[cfg(test)]
impl Global for TestCli {}

const ROOT: &str = "kube";
const CONTEXTS: &str = "kube-contexts";
const NAMESPACES: &str = "kube-namespaces";
const FORWARDS: &str = "kube-forwards";

/// One node of the section selected in the tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Node {
    Root,
    Context(String),
    Namespace(String),
    Kind(ResourceKind),
    Resource { kind: ResourceKind, name: String },
    Key { name: String, key: String },
    PortForward(usize),
}

/// What the right side shows for this section.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tab {
    Dashboard,
    Console,
}

struct PodLog {
    console: Console,
    process: Option<u32>,
}

/// The state of the Kubernetes node and the widgets that need no resource of the panel.
pub(crate) struct KubeSection {
    workspace: WeakEntity<Workspace>,
    window: AnyWindowHandle,
    state_dir: PathBuf,
    cli: Option<Kubectl>,
    watch: Option<KubeWatch>,
    snapshot: KubeSnapshot,
    visible: bool,
    open: bool,
    collapsed: HashSet<String>,
    selected: Option<Node>,
    /// The namespace the user picked; `None` follows the context.
    namespace: Option<String>,
    forwards: Vec<PortForward>,
    busy: Vec<String>,
    logs: Vec<PodLog>,
    lines: VecDeque<Line>,
    output: OutputBuffers,
    console_lines: VecDeque<Line>,
    tab: Tab,
    /// A Secret value shown by an explicit Show Value. No report carries it.
    revealed: Option<(String, String)>,
    polling: bool,
    scroll: UniformListScrollHandle,
    scroll_text: ScrollHandle,
    scroll_console: ScrollHandle,
    scroll_value: ScrollHandle,
    wrap: bool,
}

impl KubeSection {
    pub(crate) fn new(
        state_dir: PathBuf,
        workspace: WeakEntity<Workspace>,
        window: AnyWindowHandle,
        cx: &mut App,
    ) -> Self {
        Self {
            workspace,
            window,
            state_dir,
            cli: find_cli(cx),
            watch: None,
            snapshot: KubeSnapshot::default(),
            visible: false,
            // The node is open at first: the tool window shows the cluster without a
            // click. A closed node asks nothing, and so does a hidden tool window.
            open: true,
            collapsed: HashSet::new(),
            selected: None,
            namespace: None,
            forwards: Vec::new(),
            busy: Vec::new(),
            logs: Vec::new(),
            lines: VecDeque::new(),
            output: OutputBuffers::default(),
            console_lines: VecDeque::new(),
            tab: Tab::Dashboard,
            revealed: None,
            polling: false,
            scroll: UniformListScrollHandle::new(),
            scroll_text: ScrollHandle::new(),
            scroll_console: ScrollHandle::new(),
            scroll_value: ScrollHandle::new(),
            wrap: false,
        }
    }

    pub(crate) fn selected(&self) -> Option<&Node> {
        self.selected.as_ref()
    }

    pub(crate) fn clear_selection(&mut self) {
        self.selected = None;
    }

    /// Called by the Settings observer: the paths may have changed, so the CLI is built
    /// again from them.
    pub(crate) fn settings_changed(&mut self, cx: &mut Context<DockerPanel>) {
        let next = find_cli(cx);
        let changed = match (&self.cli, &next) {
            (Some(old), Some(new)) => {
                old.program() != new.program() || old.kubeconfig() != new.kubeconfig()
            }
            (None, None) => false,
            _ => true,
        };
        self.cli = next;
        if changed && self.watch.is_some() {
            self.restart(cx);
        }
    }

    /// The Services window opened or closed. A hidden window asks the cluster nothing.
    pub(crate) fn set_visible(&mut self, visible: bool, cx: &mut Context<DockerPanel>) {
        self.visible = visible;
        if visible && self.open {
            self.start(cx);
        } else {
            self.stop(cx);
        }
    }

    fn start(&mut self, cx: &mut Context<DockerPanel>) {
        if !self.visible || !self.open {
            return;
        }
        if self.watch.is_none()
            && let Some(cli) = self.cli.clone()
        {
            let cli = cli.with_namespace(self.namespace.clone());
            self.watch = Some(KubeWatch::start(cli, self.namespace.clone(), EVERY));
        }
        if !self.polling {
            self.polling = true;
            self.drain_in_background(cx);
        }
    }

    fn stop(&mut self, cx: &mut Context<DockerPanel>) {
        self.watch = None;
        self.stop_logs(cx);
    }

    fn restart(&mut self, cx: &mut Context<DockerPanel>) {
        self.watch = None;
        self.snapshot.resources.clear();
        self.start(cx);
    }

    fn drain_in_background(&mut self, cx: &mut Context<DockerPanel>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(POLL).await;
                let keep = this.update(cx, |this, cx| this.kube.poll(cx));
                if !matches!(keep, Ok(true)) {
                    break;
                }
            }
            this.update(cx, |this, _| this.kube.polling = false).ok();
        })
        .detach();
    }

    fn poll(&mut self, cx: &mut Context<DockerPanel>) -> bool {
        let mut changed = false;
        if let Some(watch) = &self.watch {
            while let Ok(snapshot) = watch.snapshots().try_recv() {
                if self.namespace.is_none() && !snapshot.namespace.is_empty() {
                    self.namespace = Some(snapshot.namespace.clone());
                }
                self.snapshot = snapshot;
                changed = true;
            }
        }
        let mut ended = Vec::new();
        let mut logs = std::mem::take(&mut self.logs);
        for mut log in logs.drain(..) {
            let mut lines = Vec::new();
            let mut closed = false;
            for _ in 0..EVENTS_PER_POLL {
                let Ok(event) = log.console.events().try_recv() else {
                    break;
                };
                changed = true;
                match event {
                    ConsoleEvent::Output {
                        process,
                        stream,
                        text,
                    } => {
                        self.output
                            .push(process, stream == Stream::Stderr, &text, &mut lines);
                    }
                    ConsoleEvent::Started { pid, .. } => log.process = Some(pid),
                    ConsoleEvent::Failed { message } => {
                        lines.push(Line::plain(message, error_style()));
                    }
                    ConsoleEvent::Exited { .. } => match log.process.take() {
                        Some(process) => self.output.finish(process, &mut lines),
                        None => self.output.finish_all(&mut lines),
                    },
                    ConsoleEvent::Closed => {
                        closed = true;
                        break;
                    }
                }
            }
            self.lines.extend(lines);
            if closed {
                ended.push(log);
            } else {
                self.logs.push(log);
            }
        }
        let extra = self.lines.len().saturating_sub(MAX_LINES);
        self.lines.drain(..extra);
        if !ended.is_empty() {
            cx.background_executor()
                .spawn(async move { drop(ended) })
                .detach();
        }
        if changed {
            let last = self.lines.len();
            self.scroll.scroll_to_item(last, ScrollStrategy::Bottom);
            self.scroll_text.scroll_to_bottom();
            cx.notify();
        }
        // Nothing to read once the section is hidden and its log ended.
        self.watch.is_some() || !self.logs.is_empty()
    }

    fn stop_logs(&mut self, cx: &mut Context<DockerPanel>) {
        let logs = std::mem::take(&mut self.logs);
        if logs.is_empty() {
            return;
        }
        cx.background_executor()
            .spawn(async move { drop(logs) })
            .detach();
    }

    pub(crate) fn select(&mut self, node: Node, cx: &mut Context<DockerPanel>) {
        let pod = match &node {
            Node::Resource { kind, name } => self
                .resource(*kind, name)
                .filter(|resource| resource.kind == ResourceKind::Pod),
            _ => None,
        };
        self.selected = Some(node);
        self.revealed = None;
        self.tab = Tab::Dashboard;
        match pod {
            Some(resource) => {
                let name = resource.name.clone();
                self.start_pod_log(&name, None, false, cx);
            }
            None => {
                self.stop_logs(cx);
                self.lines.clear();
                self.output = OutputBuffers::default();
            }
        }
        cx.notify();
    }

    fn start_pod_log(
        &mut self,
        pod: &str,
        container: Option<&str>,
        previous: bool,
        cx: &mut Context<DockerPanel>,
    ) {
        self.stop_logs(cx);
        self.lines.clear();
        self.output = OutputBuffers::default();
        let Some(cli) = self
            .cli
            .clone()
            .map(|cli| cli.with_namespace(self.namespace.clone()))
        else {
            cx.notify();
            return;
        };
        self.logs.push(PodLog {
            console: Console::start(RunPlan {
                before: None,
                program: cli.logs(pod, container, previous),
                grace: LOG_GRACE,
            }),
            process: None,
        });
        self.tab = Tab::Dashboard;
        cx.notify();
    }

    /// Opens the log of one container of a Pod, picked in the menu of the Pod.
    pub(crate) fn start_container_log(
        &mut self,
        pod: &str,
        container: &str,
        cx: &mut Context<DockerPanel>,
    ) {
        self.start_pod_log(pod, Some(container), false, cx);
    }

    fn cli(&self) -> Option<Kubectl> {
        self.cli
            .clone()
            .map(|cli| cli.with_namespace(self.namespace.clone()))
    }

    fn resource(&self, kind: ResourceKind, name: &str) -> Option<Resource> {
        self.snapshot
            .resources
            .iter()
            .find(|resource| resource.kind == kind && resource.name == name)
            .cloned()
    }

    fn resources_of(&self, kind: ResourceKind) -> Vec<&Resource> {
        self.snapshot
            .resources
            .iter()
            .filter(|resource| resource.kind == kind)
            .collect()
    }

    /// The reason a cluster does not answer, in full: the tooltip of its node, and the right
    /// side of the node once it is chosen.
    pub(crate) fn problem(&self) -> Option<String> {
        match &self.snapshot.cluster {
            ClusterState::NoCli => Some(NO_CLI.to_owned()),
            ClusterState::Unreachable { reason } => Some(reason.clone()),
            ClusterState::Up { .. } => None,
        }
    }

    pub(crate) fn report(&self) -> Vec<String> {
        let cluster = match &self.snapshot.cluster {
            ClusterState::NoCli => "kube cluster no-cli".to_string(),
            ClusterState::Unreachable { reason } => format!("kube cluster unreachable {reason}"),
            ClusterState::Up { version } => format!("kube cluster up {version}"),
        };
        let mut lines = vec![cluster];
        for context in &self.snapshot.contexts {
            lines.push(format!("kube context {} {}", context.name, context.current));
        }
        if !self.snapshot.namespace.is_empty() {
            lines.push(format!("kube namespace {}", self.snapshot.namespace));
        }
        for resource in &self.snapshot.resources {
            lines.push(format!("kube {}", resource.report()));
        }
        if let Some((name, _)) = &self.revealed {
            lines.push(format!("kube revealed {name}"));
        }
        if let Some(problem) = &self.snapshot.problem {
            lines.push(format!("kube problem {problem}"));
        }
        lines.push(format!("kube forwards {}", self.forwards.len()));
        for busy in &self.busy {
            lines.push(format!("kube busy {busy}"));
        }
        for line in &self.lines {
            lines.push(format!("kube log {}", line.text));
        }
        for line in &self.console_lines {
            lines.push(format!("kube console {}", line.text));
        }
        lines
    }

    /// Whether the section holds a watch of the cluster.
    #[cfg(all(test, windows))]
    pub(crate) fn watching(&self) -> bool {
        self.watch.is_some()
    }

    #[cfg(all(test, windows))]
    pub(crate) fn cli_calls(&self) -> u64 {
        self.cli.as_ref().map_or(0, Kubectl::calls)
    }

    #[cfg(all(test, windows))]
    pub(crate) fn revealed_value(&self) -> Option<&str> {
        self.revealed.as_ref().map(|(_, value)| value.as_str())
    }

    pub(crate) fn tree(&self, theme: &Theme, cx: &mut Context<DockerPanel>) -> Vec<AnyElement> {
        let mut rows = Vec::new();
        let root_open = !self.collapsed.contains(ROOT);
        // Contexts are entries of the kubeconfig, not of the cluster: the tree shows them
        // while the cluster does not answer too. A single context needs no list.
        let answers = matches!(self.snapshot.cluster, ClusterState::Up { .. });
        let shows_contexts = self.snapshot.contexts.len() > 1;
        let root_label = match &self.snapshot.cluster {
            ClusterState::NoCli => "Kubernetes is not installed: no kubectl CLI found".to_string(),
            ClusterState::Unreachable { .. } if shows_contexts => "Kubernetes".to_string(),
            ClusterState::Unreachable { .. } => {
                "Kubernetes: the cluster is unreachable".to_string()
            }
            ClusterState::Up { version } => {
                if version.is_empty() {
                    "Kubernetes".to_string()
                } else {
                    format!("Kubernetes {version}")
                }
            }
        };
        // The reason of an unreachable cluster is in the tooltip, not beside the label.
        let hint = match &self.snapshot.cluster {
            ClusterState::Unreachable { .. } => None,
            _ => self.snapshot.problem.clone().map(Into::into),
        };
        let expandable = answers || shows_contexts;
        let root = kube_row(
            TreeRow {
                id: ROOT.into(),
                depth: 0,
                label: root_label.into(),
                hint,
                icon: "toolwindows/services",
                expandable,
                open: expandable && root_open,
                selected: self.selected == Some(Node::Root),
            },
            theme,
        )
        .debug_selector(|| ROOT.to_owned())
        // A node with nothing to fold shows its reason on the right side instead.
        .on_click(cx.listener(move |this, _, _, cx| {
            if expandable {
                this.kube.toggle_root(cx)
            } else {
                this.kube_select(Node::Root, cx)
            }
        }));
        // Without a list of contexts the node itself names the reason of the dead cluster.
        let root = match &self.snapshot.cluster {
            ClusterState::Unreachable { reason } if !shows_contexts => {
                let reason = reason.clone();
                root.tooltip(move |_, cx| tooltip(reason.clone(), None, cx))
                    .tooltip_show_delay(TOOLTIP_CREATE_DELAY)
            }
            _ => root,
        };
        rows.push(root.into_any_element());
        if !expandable || !root_open {
            return rows;
        }
        if shows_contexts {
            let contexts_open = !self.collapsed.contains(CONTEXTS);
            rows.push(self.section_row(
                CONTEXTS,
                format!("Contexts ({})", self.snapshot.contexts.len()),
                1,
                contexts_open,
                theme,
                cx,
            ));
            if contexts_open {
                for context in &self.snapshot.contexts {
                    rows.push(self.context_row(context, theme, cx));
                }
            }
        }
        // Namespaces and resources belong to a cluster that answers; the contexts above do
        // not.
        if !answers {
            return rows;
        }
        let namespaces_open = !self.collapsed.contains(NAMESPACES);
        rows.push(self.section_row(
            NAMESPACES,
            format!("Namespaces ({})", self.snapshot.namespaces.len()),
            1,
            namespaces_open,
            theme,
            cx,
        ));
        if namespaces_open {
            for namespace in &self.snapshot.namespaces {
                rows.push(self.namespace_row(namespace, theme, cx));
            }
        }
        for kind in ResourceKind::ALL {
            let key = kind_key(kind);
            let resources = self.resources_of(kind);
            let open = !self.collapsed.contains(&key);
            let counter = if resources.is_empty() {
                kind.title().to_string()
            } else {
                format!("{} ({})", kind.title(), resources.len())
            };
            let selector = format!("kube-kind-{}", kind.plural());
            let toggle = key.clone();
            rows.push(
                kube_row(
                    TreeRow {
                        id: selector.clone().into(),
                        depth: 1,
                        label: counter.into(),
                        hint: None,
                        icon: "nodes/package",
                        expandable: true,
                        open,
                        selected: self.selected == Some(Node::Kind(kind)),
                    },
                    theme,
                )
                .debug_selector(move || selector.clone())
                .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                    this.kube_select(Node::Kind(kind), cx);
                    if event.click_count() >= 2 {
                        this.kube.toggle(&toggle, cx);
                    }
                }))
                .into_any_element(),
            );
            if !open {
                continue;
            }
            for resource in resources {
                rows.push(self.resource_row(resource, theme, cx));
            }
        }
        if !self.forwards.is_empty() {
            let open = !self.collapsed.contains(FORWARDS);
            rows.push(self.section_row(
                FORWARDS,
                format!("Port Forwards ({})", self.forwards.len()),
                1,
                open,
                theme,
                cx,
            ));
            if open {
                for (index, forward) in self.forwards.iter().enumerate() {
                    rows.push(self.forward_row(index, forward, theme, cx));
                }
            }
        }
        rows
    }

    fn section_row(
        &self,
        key: &'static str,
        label: String,
        depth: usize,
        open: bool,
        theme: &Theme,
        cx: &mut Context<DockerPanel>,
    ) -> AnyElement {
        kube_row(
            TreeRow {
                id: key.into(),
                depth,
                label: label.into(),
                hint: None,
                icon: "nodes/folder",
                expandable: true,
                open,
                selected: false,
            },
            theme,
        )
        .debug_selector(move || key.to_owned())
        .on_click(cx.listener(move |this, _, _, cx| this.kube.toggle(key, cx)))
        .into_any_element()
    }

    fn context_row(
        &self,
        context: &KubeContext,
        theme: &Theme,
        cx: &mut Context<DockerPanel>,
    ) -> AnyElement {
        let name = context.name.clone();
        let selector = format!("kube-context-{}", context.name);
        let clicked = name.clone();
        let menu_name = name.clone();
        let current = context.current;
        let selected = self.selected == Some(Node::Context(name.clone()));
        let hint = current.then(|| "current".into());
        let row = kube_row(
            TreeRow {
                id: selector.clone().into(),
                depth: 2,
                label: context.name.clone().into(),
                hint,
                icon: "nodes/module",
                expandable: false,
                open: false,
                selected,
            },
            theme,
        )
        .debug_selector({
            let selector = selector.clone();
            move || selector.clone()
        });
        // The current context carries a mark of its own after its name.
        let row = if current {
            let mark = format!("{selector}-current");
            row.child(
                div()
                    .ml(px(SPACE_S))
                    .flex_none()
                    .debug_selector(move || mark.clone())
                    .child(icon("general/check", theme)),
            )
        } else {
            row
        };
        let row = row.on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
            this.kube_select(Node::Context(clicked.clone()), cx);
            if !current && event.click_count() >= 2 {
                this.kube.switch_context(clicked.clone(), cx);
            }
        }));
        let row = row.on_mouse_down(
            MouseButton::Right,
            cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                this.kube_select(Node::Context(menu_name.clone()), cx);
                this.kube
                    .show_context_menu(menu_name.clone(), event.position, cx);
            }),
        );
        // A dead cluster names the reason at its current context, the way the node does
        // when the kubeconfig holds one context only.
        let row = match (current, self.problem()) {
            (true, Some(problem)) => row
                .tooltip(move |_, cx| tooltip(problem.clone(), None, cx))
                .tooltip_show_delay(TOOLTIP_CREATE_DELAY),
            _ => row,
        };
        row.into_any_element()
    }

    fn namespace_row(
        &self,
        namespace: &str,
        theme: &Theme,
        cx: &mut Context<DockerPanel>,
    ) -> AnyElement {
        let current = self.snapshot.namespace.as_str();
        let selector = format!("kube-namespace-{namespace}");
        let chosen = namespace == current;
        let clicked = namespace.to_owned();
        kube_row(
            TreeRow {
                id: selector.clone().into(),
                depth: 2,
                label: namespace.to_owned().into(),
                hint: chosen.then(|| "current".into()),
                icon: "nodes/package",
                expandable: false,
                open: false,
                selected: self.selected == Some(Node::Namespace(namespace.to_owned())),
            },
            theme,
        )
        .debug_selector(move || selector.clone())
        .on_click(
            cx.listener(move |this, _, _, cx| this.kube.switch_namespace(clicked.clone(), cx)),
        )
        .into_any_element()
    }

    fn resource_row(
        &self,
        resource: &Resource,
        theme: &Theme,
        cx: &mut Context<DockerPanel>,
    ) -> AnyElement {
        let key = resource_key(resource.kind, &resource.name);
        let open = resource.is_expandable() && !self.collapsed.contains(&key);
        let selector = format!("kube-resource-{}-{}", resource.kind.plural(), resource.name);
        let hint = if resource.keys.is_empty() {
            resource_hint(resource)
        } else {
            format!("{} · {} keys", resource.state, resource.keys.len())
        };
        let id = resource_key(resource.kind, &resource.name);
        let node = Node::Resource {
            kind: resource.kind,
            name: resource.name.clone(),
        };
        let click_node = node.clone();
        let expandable = resource.is_expandable();
        let toggle_key = id.clone();
        let menu_node = node.clone();
        let state = resource.state.clone();
        let icon = state_icon(&state, resource.kind);
        let row = kube_row(
            TreeRow {
                id: selector.clone().into(),
                depth: 2,
                label: resource.name.clone().into(),
                hint: Some(hint.into()),
                icon,
                expandable,
                open,
                selected: self.selected == Some(node),
            },
            theme,
        )
        .debug_selector(move || selector.clone())
        .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
            this.kube_select(click_node.clone(), cx);
            if expandable && event.click_count() >= 2 {
                this.kube.toggle(&toggle_key, cx);
            }
        }))
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                this.kube_select(menu_node.clone(), cx);
                this.kube
                    .show_resource_menu(menu_node.clone(), event.position, cx);
            }),
        );
        let mut elements = vec![row.into_any_element()];
        if open {
            for key in &resource.keys {
                elements.push(self.key_row(resource, key, theme, cx));
            }
        }
        if elements.len() == 1 {
            elements.pop().expect("the row")
        } else {
            div()
                .flex()
                .flex_col()
                .children(elements)
                .into_any_element()
        }
    }

    fn key_row(
        &self,
        resource: &Resource,
        key: &str,
        theme: &Theme,
        cx: &mut Context<DockerPanel>,
    ) -> AnyElement {
        let node = Node::Key {
            name: resource.name.clone(),
            key: key.to_owned(),
        };
        let shown = self
            .revealed
            .as_ref()
            .is_some_and(|(name, shown)| *name == resource.name && shown == key);
        let selector = format!("kube-key-{}-{}", resource.name, key);
        let click_node = node.clone();
        let menu_node = node.clone();
        kube_row(
            TreeRow {
                id: selector.clone().into(),
                depth: 3,
                label: key.to_owned().into(),
                hint: Some(if shown { "shown" } else { "hidden" }.into()),
                icon: "general/lock",
                expandable: false,
                open: false,
                selected: self.selected == Some(node),
            },
            theme,
        )
        .debug_selector(move || selector.clone())
        .on_click(cx.listener(move |this, _, _, cx| this.kube_select(click_node.clone(), cx)))
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                this.kube_select(menu_node.clone(), cx);
                this.kube
                    .show_resource_menu(menu_node.clone(), event.position, cx);
            }),
        )
        .into_any_element()
    }

    fn forward_row(
        &self,
        index: usize,
        forward: &PortForward,
        theme: &Theme,
        cx: &mut Context<DockerPanel>,
    ) -> AnyElement {
        let selector = format!("kube-forward-{index}");
        let node = Node::PortForward(index);
        let click_node = node.clone();
        let menu_node = node.clone();
        let hint = format!("{}:{}", forward.target, forward.ports);
        kube_row(
            TreeRow {
                id: selector.clone().into(),
                depth: 2,
                label: forward.target.clone().into(),
                hint: Some(hint.into()),
                icon: "general/link",
                expandable: false,
                open: false,
                selected: self.selected == Some(node),
            },
            theme,
        )
        .debug_selector(move || selector.clone())
        .on_click(cx.listener(move |this, _, _, cx| this.kube_select(click_node.clone(), cx)))
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                this.kube_select(menu_node.clone(), cx);
                this.kube
                    .show_resource_menu(menu_node.clone(), event.position, cx);
            }),
        )
        .into_any_element()
    }

    fn toggle_root(&mut self, cx: &mut Context<DockerPanel>) {
        if !self.collapsed.remove(ROOT) {
            self.collapsed.insert(ROOT.to_owned());
        }
        self.open = !self.collapsed.contains(ROOT);
        if self.open {
            self.start(cx);
        } else {
            self.stop(cx);
        }
        cx.notify();
    }

    fn toggle(&mut self, key: &str, cx: &mut Context<DockerPanel>) {
        if !self.collapsed.remove(key) {
            self.collapsed.insert(key.to_owned());
        }
        cx.notify();
    }

    /// Writes the context into the kubeconfig in the background, then asks the tree again.
    /// The write never reaches the cluster, so a dead cluster does not keep the user on it.
    fn switch_context(&mut self, name: String, cx: &mut Context<DockerPanel>) {
        if self.snapshot.current_context == name {
            return;
        }
        let Some(cli) = self.cli.clone() else {
            self.missing_cli(cx);
            return;
        };
        let label = format!("use-context {name}");
        self.busy.push(label.clone());
        let task_name = name.clone();
        let task = cx
            .background_executor()
            .spawn(async move { cli.use_context(&task_name) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.kube.finish_context(label, name, result, cx);
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn finish_context(
        &mut self,
        label: String,
        name: String,
        result: Result<(), String>,
        cx: &mut Context<DockerPanel>,
    ) {
        if let Some(index) = self.busy.iter().position(|busy| *busy == label) {
            self.busy.remove(index);
        }
        match result {
            Ok(()) => {
                // The new context may carry another namespace: the tree starts from it.
                self.namespace = None;
                self.selected = Some(Node::Context(name));
                self.stop(cx);
                self.restart(cx);
            }
            Err(error) => self.tell("Use Context".to_owned(), error, cx),
        }
        cx.notify();
    }

    fn switch_namespace(&mut self, name: String, cx: &mut Context<DockerPanel>) {
        if self.snapshot.namespace == name {
            return;
        }
        self.namespace = Some(name);
        self.restart(cx);
        cx.notify();
    }

    pub(crate) fn selection_title(&self) -> (String, String) {
        match &self.selected {
            Some(Node::Root) | None => {
                let (label, hint) = match &self.snapshot.cluster {
                    ClusterState::NoCli => ("Kubernetes", String::new()),
                    ClusterState::Unreachable { .. } => ("Kubernetes", "unreachable".to_owned()),
                    ClusterState::Up { version } => (
                        "Kubernetes",
                        format!("{version} · {}", self.snapshot.current_context),
                    ),
                };
                (label.to_owned(), hint)
            }
            Some(Node::Context(name)) => {
                let hint = if self.snapshot.current_context == *name && self.problem().is_some() {
                    "unreachable".to_owned()
                } else {
                    String::new()
                };
                (format!("Context {name}"), hint)
            }
            Some(Node::Namespace(name)) => (format!("Namespace {name}"), String::new()),
            Some(Node::Kind(kind)) => (
                kind.title().to_owned(),
                format!(
                    "{} in {}",
                    self.resources_of(*kind).len(),
                    self.snapshot.namespace
                ),
            ),
            Some(Node::Resource { kind, name }) => match self.resource(*kind, name) {
                Some(resource) => (resource.name.clone(), resource_hint(&resource)),
                None => (name.clone(), String::new()),
            },
            Some(Node::Key { name, key }) => (format!("{name}/{key}"), "Secret".to_owned()),
            Some(Node::PortForward(index)) => match self.forwards.get(*index) {
                Some(forward) => (
                    format!("Port Forward {}", forward.target),
                    format!("{}:{}", forward.namespace, forward.ports),
                ),
                None => ("Port Forward".to_owned(), String::new()),
            },
        }
    }

    fn header_actions(&self) -> Vec<(&'static str, &'static str, &'static str, bool)> {
        let Some(node) = &self.selected else {
            return vec![("refresh", "run/restart", "Refresh", true)];
        };
        match node {
            Node::Root => {
                let label = match &self.snapshot.cluster {
                    ClusterState::Unreachable { .. } => "Retry",
                    _ => "Refresh",
                };
                vec![("refresh", "run/restart", label, true)]
            }
            Node::Context(name) => {
                if self.snapshot.current_context == *name && self.problem().is_some() {
                    vec![("refresh", "run/restart", "Retry", true)]
                } else {
                    Vec::new()
                }
            }
            Node::Namespace(_) | Node::Kind(_) => Vec::new(),
            Node::Key { .. } => vec![
                ("show-value", "general/show", "Show Value", true),
                ("copy-value", "general/copy", "Copy Value", true),
            ],
            Node::PortForward(_) => vec![("stop-forward", "run/stop", "Stop", true)],
            Node::Resource { kind, name } => {
                if self.resource(*kind, name).is_none() {
                    return Vec::new();
                }
                match kind {
                    ResourceKind::Pod => vec![
                        ("logs", "general/logs", "Show Logs", true),
                        ("describe", "fileTypes/text", "Describe", true),
                        ("yaml", "fileTypes/yaml", "View YAML", true),
                        ("port-forward", "general/link", "Port Forward", true),
                        ("exec", "toolwindows/terminal", "Exec", true),
                        ("delete", "general/trash", "Delete", true),
                    ],
                    ResourceKind::Deployment => vec![
                        ("describe", "fileTypes/text", "Describe", true),
                        ("yaml", "fileTypes/yaml", "View YAML", true),
                        ("restart", "run/restart", "Restart", true),
                        ("scale", "general/add", "Scale", true),
                        ("delete", "general/trash", "Delete", true),
                    ],
                    ResourceKind::Service => vec![
                        ("describe", "fileTypes/text", "Describe", true),
                        ("yaml", "fileTypes/yaml", "View YAML", true),
                        ("port-forward", "general/link", "Port Forward", true),
                        ("delete", "general/trash", "Delete", true),
                    ],
                    _ => {
                        let mut actions = vec![
                            ("describe", "fileTypes/text", "Describe", true),
                            ("yaml", "fileTypes/yaml", "View YAML", true),
                        ];
                        actions.push(("delete", "general/trash", "Delete", true));
                        actions
                    }
                }
            }
        }
    }

    pub(crate) fn header(
        &self,
        theme: &Theme,
        show_labels: bool,
        cx: &mut Context<DockerPanel>,
    ) -> AnyElement {
        let (title, hint) = self.selection_title();
        let actions = self.header_actions();
        div()
            .flex()
            .items_center()
            .gap(px(SPACE_S))
            .flex_none()
            .h(px(CONTROL_HEIGHT))
            .px(px(SPACE_M))
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .id("kube-title")
                    .debug_selector(|| "kube-title".to_owned())
                    .flex()
                    .items_center()
                    .gap(px(SPACE_S))
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .truncate()
                            .font_weight(FontWeight::MEDIUM)
                            .child(title),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_color(theme.text_secondary)
                            .child(hint),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(SPACE_XS))
                    .flex_none()
                    .children(actions.into_iter().map(|(action, icon, label, enabled)| {
                        crate::docker::action_button(
                            action,
                            icon,
                            label,
                            enabled,
                            show_labels,
                            theme,
                        )
                        .debug_selector(move || format!("kube-action-{action}"))
                        .when(enabled, |this| {
                            this.on_click(cx.listener(move |this, _, window, cx| {
                                this.kube.action(action, window, cx)
                            }))
                        })
                    })),
            )
            .into_any_element()
    }

    pub(crate) fn tabs(&self, theme: &Theme, cx: &mut Context<DockerPanel>) -> Option<Div> {
        Some(
            div()
                .flex()
                .items_center()
                .gap(px(SPACE_XS))
                .flex_none()
                .h(px(CONTROL_HEIGHT))
                .px(px(SPACE_M))
                .child(crate::docker::tab_button(
                    "kube-tab-dashboard",
                    "Dashboard",
                    self.tab == Tab::Dashboard,
                    theme,
                    cx.listener(|this, _, _, cx| {
                        this.kube.tab = Tab::Dashboard;
                        cx.notify();
                    }),
                ))
                .child(crate::docker::tab_button(
                    "kube-tab-console",
                    "Console",
                    self.tab == Tab::Console,
                    theme,
                    cx.listener(|this, _, _, cx| {
                        this.kube.tab = Tab::Console;
                        cx.notify();
                    }),
                )),
        )
    }

    pub(crate) fn body(
        &self,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<DockerPanel>,
    ) -> Div {
        if self.tab == Tab::Console {
            return self.console(theme, window, cx);
        }
        if let Some((name, value)) = &self.revealed {
            return revealed_view(name, value, &self.scroll_value, theme, window, cx);
        }
        if self.lines.is_empty() {
            let explains_cluster = match &self.selected {
                Some(Node::Root) => true,
                Some(Node::Context(name)) => self.snapshot.current_context == *name,
                _ => false,
            };
            if explains_cluster && let Some(problem) = self.problem() {
                return crate::docker::problem_view(problem, theme);
            }
            let text = match &self.selected {
                Some(Node::Resource { kind, name }) => match self.resource(*kind, name) {
                    Some(resource) if resource.kind == ResourceKind::Secret => {
                        "The values of a Secret stay hidden until Show Value"
                    }
                    _ if *kind == ResourceKind::Pod => "Waiting for the log of the Pod",
                    _ => "Select a Pod for its log",
                },
                Some(Node::Key { .. }) => "The value stays hidden until Show Value",
                Some(Node::Context(name)) if self.snapshot.current_context == *name => {
                    "This context is in use"
                }
                Some(Node::Context(_)) => "Double-click a context to use it",
                _ => "Select a Pod for its log",
            };
            return crate::docker::empty_state(text, theme);
        }
        log_view(self, theme, window, cx)
    }

    fn console(&self, theme: &Theme, window: &mut Window, cx: &mut Context<DockerPanel>) -> Div {
        if self.console_lines.is_empty() {
            return crate::docker::empty_state("No actions yet", theme);
        }
        let theme = *theme;
        div().flex().flex_1().min_h_0().child(
            scrollable(
                "kube-console",
                &self.scroll_console,
                div()
                    .id("kube-console")
                    .flex()
                    .flex_col()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll_console)
                    .font_family(MONO_FONT)
                    .text_size(px(CODE_SIZE))
                    .text_color(theme.code)
                    .children(
                        self.console_lines
                            .iter()
                            .map(|line| wrapped_line_element(line, &theme)),
                    ),
                &theme,
                window,
                cx,
            )
            .flex_1()
            .min_h_0(),
        )
    }

    fn action(&mut self, action: &str, window: &mut Window, cx: &mut Context<DockerPanel>) {
        let node = self.selected.clone();
        match (action, node) {
            ("refresh", _) => {
                if let Some(watch) = &self.watch {
                    watch.refresh();
                } else {
                    self.restart(cx);
                }
            }
            (
                "logs",
                Some(Node::Resource {
                    kind: ResourceKind::Pod,
                    name,
                }),
            ) => {
                self.start_pod_log(&name, None, false, cx);
            }
            (
                "previous",
                Some(Node::Resource {
                    kind: ResourceKind::Pod,
                    name,
                }),
            ) => {
                self.start_pod_log(&name, None, true, cx);
            }
            ("describe", Some(Node::Resource { kind, name })) => {
                self.describe(kind, name, cx);
            }
            ("yaml", Some(Node::Resource { kind, name })) => {
                self.view_yaml(kind, name, cx);
            }
            ("delete", Some(Node::Resource { kind, name })) => {
                self.ask_delete(kind, name, window, cx);
            }
            (
                "restart",
                Some(Node::Resource {
                    kind: ResourceKind::Deployment,
                    name,
                }),
            ) => {
                let label = format!("restart {name}");
                let name_job = name.clone();
                self.act(label, cx, move |cli| {
                    cli.restart(ResourceKind::Deployment, &name_job)
                        .map(|()| String::new())
                });
            }
            (
                "scale",
                Some(Node::Resource {
                    kind: ResourceKind::Deployment,
                    name,
                }),
            ) => {
                self.ask_scale(name, window, cx);
            }
            ("port-forward", Some(Node::Resource { kind, name }))
                if matches!(kind, ResourceKind::Pod | ResourceKind::Service) =>
            {
                self.ask_forward(kind, name, window, cx);
            }
            (
                "exec",
                Some(Node::Resource {
                    kind: ResourceKind::Pod,
                    name,
                }),
            ) => {
                self.exec(&name, cx);
            }
            ("show-value", Some(Node::Key { name, key })) => {
                self.show_value(name, key, cx);
            }
            ("copy-value", Some(Node::Key { name, key })) => {
                self.copy_value(name, key, cx);
            }
            ("stop-forward", Some(Node::PortForward(index))) if index < self.forwards.len() => {
                let forward = self.forwards.remove(index);
                // The process may wait for its pipes: the drop goes off the frame.
                cx.background_executor()
                    .spawn(async move { drop(forward) })
                    .detach();
                self.selected = None;
                cx.notify();
            }
            ("use-context", Some(Node::Context(name))) => {
                self.switch_context(name, cx);
            }
            _ => {}
        }
    }

    fn act(
        &mut self,
        name: String,
        cx: &mut Context<DockerPanel>,
        work: impl FnOnce(Kubectl) -> Result<String, String> + Send + 'static,
    ) {
        let Some(cli) = self.cli() else {
            self.missing_cli(cx);
            return;
        };
        self.busy.push(name.clone());
        self.tab = Tab::Console;
        self.console_lines
            .push_back(info(format!("{name}: started")));
        let task = cx.background_executor().spawn(async move { work(cli) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.kube.finish(name, result, cx);
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn finish(
        &mut self,
        name: String,
        result: Result<String, String>,
        cx: &mut Context<DockerPanel>,
    ) {
        if let Some(index) = self.busy.iter().position(|busy| *busy == name) {
            self.busy.remove(index);
        }
        match result {
            Ok(output) => {
                self.console_lines.push_back(info(format!("{name}: done")));
                for line in output.lines().filter(|line| !line.trim().is_empty()) {
                    self.console_lines
                        .push_back(Line::plain(line, Default::default()));
                }
            }
            Err(error) => {
                self.console_lines
                    .push_back(Line::plain(format!("{name}: {error}"), error_style()));
                self.tell(name.clone(), error, cx);
            }
        }
        let extra = self.console_lines.len().saturating_sub(MAX_LINES);
        self.console_lines.drain(..extra);
        self.scroll_console.scroll_to_bottom();
        if let Some(watch) = &self.watch {
            watch.refresh();
        }
        cx.notify();
    }

    fn describe(&mut self, kind: ResourceKind, name: String, cx: &mut Context<DockerPanel>) {
        let Some(cli) = self.cli() else {
            self.missing_cli(cx);
            return;
        };
        let title = format!("Describe {} {name}", kind.one());
        let task = cx
            .background_executor()
            .spawn(async move { cli.describe(kind, &name) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| match result {
                Ok(text) => this.kube.open_read_only(title, text, cx),
                Err(error) => {
                    this.kube
                        .console_lines
                        .push_back(Line::plain(format!("describe: {error}"), error_style()));
                    this.kube.tab = Tab::Console;
                    this.kube.tell("Describe".to_owned(), error, cx);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn view_yaml(&mut self, kind: ResourceKind, name: String, cx: &mut Context<DockerPanel>) {
        let Some(cli) = self.cli() else {
            self.missing_cli(cx);
            return;
        };
        let folder = self.state_dir.join("kube").join(kind.plural());
        let path = folder.join(format!("{name}.yaml"));
        let open = self.workspace.clone();
        let exists = open
            .read_with(cx, |workspace, _| workspace.view_of(&path).is_some())
            .unwrap_or(false);
        if exists {
            let window = self.window;
            window
                .update(cx, |_, window, cx| {
                    open.update(cx, |workspace, cx| {
                        workspace.open_file(path.clone(), window, cx);
                    })
                    .ok();
                })
                .ok();
            return;
        }
        let task_path = path.clone();
        let task_name = name.clone();
        let task = cx.background_executor().spawn(async move {
            let text = cli.yaml(kind, &task_name)?;
            std::fs::create_dir_all(&folder)
                .map_err(|error| format!("{}: {error}", folder.display()))?;
            std::fs::write(&task_path, text)
                .map_err(|error| format!("{}: {error}", task_path.display()))?;
            Ok::<PathBuf, String>(task_path)
        });
        let window = self.window;
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| match result {
                Ok(path) => {
                    window
                        .update(cx, |_, window, cx| {
                            open.update(cx, |workspace, cx| {
                                workspace.open_file(path.clone(), window, cx);
                            })
                            .ok();
                        })
                        .ok();
                }
                Err(error) => {
                    this.kube.tell("View YAML".to_owned(), error, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    fn ask_delete(
        &mut self,
        kind: ResourceKind,
        name: String,
        window: &mut Window,
        cx: &mut Context<DockerPanel>,
    ) {
        let question = format!("Delete {} {name}?", kind.one());
        let panel = cx.weak_entity();
        let workspace = self.workspace.clone();
        let label = format!("delete {name}");
        workspace
            .update(cx, |workspace, cx| {
                Prompt::ask(
                    "Delete Resource",
                    &question,
                    move |cx| {
                        let (kind, label, name) = (kind, label.clone(), name.clone());
                        panel
                            .update(cx, |this, cx| {
                                this.kube.act(label.clone(), cx, move |cli| {
                                    cli.delete(kind, &name).map(|()| String::new())
                                })
                            })
                            .ok();
                    },
                    workspace,
                    window,
                    cx,
                );
            })
            .ok();
    }

    fn ask_scale(&mut self, name: String, window: &mut Window, cx: &mut Context<DockerPanel>) {
        let replicas = self
            .resource(ResourceKind::Deployment, &name)
            .and_then(|resource| resource.ready.split('/').next().map(str::to_owned))
            .unwrap_or_else(|| "1".to_owned());
        let panel = cx.weak_entity();
        let workspace = self.workspace.clone();
        let label = format!("scale {name}");
        workspace
            .update(cx, |workspace, cx| {
                Prompt::show_with(
                    "Scale Deployment",
                    &replicas,
                    move |text, cx| {
                        let Ok(value) = text.trim().parse::<u32>() else {
                            return;
                        };
                        panel
                            .update(cx, |this, cx| {
                                let name = name.clone();
                                this.kube.act(label.clone(), cx, move |cli| {
                                    cli.scale(ResourceKind::Deployment, &name, value)
                                        .map(|()| String::new())
                                })
                            })
                            .ok();
                    },
                    workspace,
                    window,
                    cx,
                );
            })
            .ok();
    }

    fn ask_forward(
        &mut self,
        kind: ResourceKind,
        name: String,
        window: &mut Window,
        cx: &mut Context<DockerPanel>,
    ) {
        let panel = cx.weak_entity();
        let workspace = self.workspace.clone();
        workspace
            .update(cx, |workspace, cx| {
                Prompt::show_with(
                    "Port Forward",
                    "8080:80",
                    move |text, cx| {
                        let ports = text.trim().to_owned();
                        if ports.is_empty() {
                            return;
                        }
                        panel
                            .update(cx, |this, cx| {
                                this.kube.start_forward(kind, &name, &ports, cx)
                            })
                            .ok();
                    },
                    workspace,
                    window,
                    cx,
                );
            })
            .ok();
    }

    fn start_forward(
        &mut self,
        kind: ResourceKind,
        name: &str,
        ports: &str,
        cx: &mut Context<DockerPanel>,
    ) {
        let Some(cli) = self.cli() else {
            self.missing_cli(cx);
            return;
        };
        let target = format!("{}/{name}", kind.plural().trim_end_matches('s'));
        let target = if kind == ResourceKind::Pod {
            format!("pod/{name}")
        } else if kind == ResourceKind::Service {
            format!("service/{name}")
        } else {
            target
        };
        let ports = ports.to_owned();
        let task_target = target.clone();
        let task = cx
            .background_executor()
            .spawn(async move { cli.port_forward(&task_target, &ports) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| match result {
                Ok(forward) => {
                    this.kube.forwards.push(forward);
                    this.kube
                        .console_lines
                        .push_back(info(format!("port-forward {target}: started")));
                    this.kube.tab = Tab::Console;
                    cx.notify();
                }
                Err(error) => {
                    this.kube.console_lines.push_back(Line::plain(
                        format!("port-forward {target}: {error}"),
                        error_style(),
                    ));
                    this.kube.tab = Tab::Console;
                    this.kube.tell("Port Forward".to_owned(), error, cx);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn show_value(&mut self, name: String, key: String, cx: &mut Context<DockerPanel>) {
        let Some(cli) = self.cli() else {
            self.missing_cli(cx);
            return;
        };
        let task_name = name.clone();
        let task_key = key.clone();
        let task = cx
            .background_executor()
            .spawn(async move { cli.secret_value(&task_name, &task_key) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| match result {
                Ok(value) => {
                    this.kube.revealed = Some((format!("{name}/{key}"), value));
                    this.kube.tab = Tab::Dashboard;
                    cx.notify();
                }
                Err(error) => {
                    this.kube.tell("Show Value".to_owned(), error, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    fn copy_value(&mut self, name: String, key: String, cx: &mut Context<DockerPanel>) {
        let Some(cli) = self.cli() else {
            self.missing_cli(cx);
            return;
        };
        let task = cx
            .background_executor()
            .spawn(async move { cli.secret_value(&name, &key) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| match result {
                // A secret goes where the system keeps it out of the clipboard history and the
                // cloud clipboard. Where the system has no such formats, the usual copy follows;
                // a failure on Windows is shown and never copied the usual way.
                Ok(value) => match ::platform::copy_secret(&value) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::Unsupported => {
                        cx.write_to_clipboard(ClipboardItem::new_string(value));
                    }
                    Err(error) => this.kube.tell(
                        "Copy Value".to_owned(),
                        format!("The value was not copied: {error}"),
                        cx,
                    ),
                },
                Err(error) => this.kube.tell("Copy Value".to_owned(), error, cx),
            })
            .ok();
        })
        .detach();
    }

    fn exec(&mut self, pod: &str, cx: &mut Context<DockerPanel>) {
        let Some(cli) = self.cli() else {
            self.missing_cli(cx);
            return;
        };
        let dialect = crate::terminal_panel::active_dialect(cx);
        match cli.exec_command_line(pod, SHELL, dialect) {
            Ok(line) => self.in_terminal(line, cx),
            Err(error) => self.tell("Exec".to_owned(), error.to_string(), cx),
        }
    }

    fn in_terminal(&self, line: String, cx: &mut Context<DockerPanel>) {
        let (workspace, window) = (self.workspace.clone(), self.window);
        cx.defer(move |cx| {
            let terminal = cx.global::<ActiveTerminal>().0.clone();
            let sent = terminal.read(cx).run_command(&line);
            window
                .update(cx, |_, window, cx| {
                    workspace.update(cx, |workspace, cx| match sent {
                        Ok(()) => workspace.show_tool(TERMINAL_TOOL, window, cx),
                        Err(error) => workspace.notify_error("Terminal", error, cx),
                    })
                })
                .ok();
        });
    }

    fn open_read_only(&mut self, title: String, text: String, cx: &mut Context<DockerPanel>) {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let serial = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = self
            .state_dir
            .join("kube-tabs")
            .join(format!("describe-{serial}.txt"));
        let style = crate::editor_tab::style_of(cx);
        let options = crate::editor_tab::options_of(cx);
        let view = cx.new(|cx| {
            let mut view = EditorView::open_text_document(
                path.clone(),
                text,
                text_core::FileFormat::default(),
                true,
                false,
                style,
                cx,
            );
            view.set_options(options, cx);
            view
        });
        let focus = view.read(cx).focus_handle(cx);
        let workspace = self.workspace.clone();
        let window = self.window;
        window
            .update(cx, |_, window, cx| {
                workspace
                    .update(cx, |workspace, cx| {
                        workspace.open_view_tab(
                            path,
                            title,
                            "toolwindows/services",
                            view.into(),
                            focus,
                            window,
                            cx,
                        );
                    })
                    .ok();
            })
            .ok();
    }

    fn missing_cli(&mut self, cx: &mut Context<DockerPanel>) {
        self.console_lines.push_back(Line::plain(
            "Kubernetes is not installed: no kubectl CLI found",
            error_style(),
        ));
        self.tab = Tab::Console;
        cx.notify();
    }

    fn tell(&self, title: String, text: String, cx: &mut Context<DockerPanel>) {
        let workspace = self.workspace.clone();
        cx.defer(move |cx| {
            workspace
                .update(cx, |workspace, cx| workspace.notify_error(title, text, cx))
                .ok();
        });
    }

    /// The menu of a context that is not the current one: writing it into the kubeconfig.
    fn show_context_menu(
        &mut self,
        name: String,
        position: Point<Pixels>,
        cx: &mut Context<DockerPanel>,
    ) {
        if self.snapshot.current_context == name {
            return;
        }
        let panel = cx.weak_entity();
        let entry = MenuEntry::new("Use Context", move |_, cx| {
            panel
                .update(cx, |this, cx| this.kube.switch_context(name.clone(), cx))
                .ok();
        })
        .icon("general/check");
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.show_menu(position, vec![entry], cx)
            })
            .ok();
    }

    fn show_resource_menu(
        &mut self,
        node: Node,
        position: Point<Pixels>,
        cx: &mut Context<DockerPanel>,
    ) {
        let panel = cx.weak_entity();
        let mut entries = Vec::new();
        if let Node::Resource { kind, .. } = &node
            && *kind == ResourceKind::Deployment
        {
            entries.push(menu_entry(
                panel.clone(),
                node.clone(),
                "restart",
                "Restart",
                "run/restart",
            ));
            entries.push(menu_entry(
                panel.clone(),
                node.clone(),
                "scale",
                "Scale",
                "general/add",
            ));
            entries.push(MenuEntry::separator());
        }
        let pod_containers = match &node {
            Node::Resource { kind, name } if *kind == ResourceKind::Pod => self
                .resource(ResourceKind::Pod, name)
                .map(|resource| (name.clone(), resource.containers))
                .filter(|(_, containers)| !containers.is_empty()),
            _ => None,
        };
        if let Some((pod, containers)) = pod_containers {
            let mut items = Vec::new();
            for container in containers {
                let panel = panel.clone();
                let pod = pod.clone();
                let label = container.clone();
                items.push(
                    MenuEntry::new(container, move |_, cx| {
                        panel
                            .update(cx, |this, cx| {
                                this.kube.start_container_log(&pod, &label, cx)
                            })
                            .ok();
                    })
                    .icon("general/logs"),
                );
            }
            entries.push(MenuEntry::submenu("Show Logs", items).icon("general/logs"));
        }
        let actions: Vec<(&'static str, &'static str)> = match &node {
            Node::Resource { kind, .. } if *kind == ResourceKind::Pod => vec![
                ("previous", "Show Previous Logs"),
                ("describe", "Describe"),
                ("yaml", "View YAML"),
                ("port-forward", "Port Forward"),
                ("exec", "Exec"),
                ("delete", "Delete"),
            ],
            Node::Resource { .. } => vec![
                ("describe", "Describe"),
                ("yaml", "View YAML"),
                ("delete", "Delete"),
            ],
            Node::Key { .. } => vec![("show-value", "Show Value"), ("copy-value", "Copy Value")],
            Node::PortForward(_) => vec![("stop-forward", "Stop")],
            _ => Vec::new(),
        };
        for (action, label) in actions {
            entries.push(menu_entry(
                panel.clone(),
                node.clone(),
                action,
                label,
                icon_of(action),
            ));
        }
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.show_menu(position, entries, cx)
            })
            .ok();
    }
}

/// Opens a view in a tab of the editor, after the window has finished its current update.
fn menu_entry(
    panel: WeakEntity<DockerPanel>,
    _node: Node,
    action: &'static str,
    label: &'static str,
    icon: &'static str,
) -> MenuEntry {
    MenuEntry::new(label, move |window, cx| {
        panel
            .update(cx, |this, cx| this.kube.action(action, window, cx))
            .ok();
    })
    .icon(icon)
}

fn icon_of(action: &str) -> &'static str {
    match action {
        "logs" | "previous" => "general/logs",
        "describe" => "fileTypes/text",
        "yaml" => "fileTypes/yaml",
        "exec" => "toolwindows/terminal",
        "port-forward" => "general/link",
        "delete" | "stop-forward" => "general/trash",
        "show-value" => "general/show",
        "copy-value" => "general/copy",
        _ => "general/locate",
    }
}

/// A row of the Kubernetes tree. It looks like `tree_row` of ui-kit, but a tree longer
/// than its pane paints its last row only in part; the layout probe of a control would
/// then cross the pane in the check of the window, so these rows leave it out.
fn kube_row(row: TreeRow, theme: &Theme) -> Stateful<Div> {
    let hover = theme.selection_hover;
    div()
        .id(row.id)
        .flex()
        .items_center()
        .flex_none()
        .w_full()
        .h(px(ROW_HEIGHT))
        .pl(px(row.depth as f32 * 19. + ROW_PADDING_X))
        .pr(px(ROW_PADDING_X))
        .rounded(px(ROW_RADIUS))
        .when(row.selected, |this| this.bg(theme.selection_inactive))
        .when(!row.selected, |this| {
            this.hover(move |style| style.bg(hover))
        })
        .child(
            div()
                .size(px(16.))
                .flex_none()
                .when(row.expandable, |this| {
                    let name = if row.open {
                        "general/chevronDown"
                    } else {
                        "general/chevronRight"
                    };
                    this.child(icon(name, theme))
                }),
        )
        .child(icon(row.icon, theme).ml(px(SPACE_S)))
        .child(div().ml(px(SPACE_S)).flex_none().child(row.label))
        .children(row.hint.map(|hint| {
            div()
                .ml(px(SPACE_M))
                .text_color(theme.text_secondary)
                .flex_none()
                .child(hint)
        }))
}

fn log_view(
    section: &KubeSection,
    theme: &Theme,
    window: &mut Window,
    cx: &mut Context<DockerPanel>,
) -> Div {
    let theme = *theme;
    if section.wrap {
        return div().flex().flex_1().min_h_0().child(
            scrollable(
                "kube-log-wrapped",
                &section.scroll_text,
                div()
                    .id("kube-log-wrapped")
                    .flex()
                    .flex_col()
                    .overflow_y_scroll()
                    .track_scroll(&section.scroll_text)
                    .font_family(MONO_FONT)
                    .text_size(px(CODE_SIZE))
                    .text_color(theme.code)
                    .children(
                        section
                            .lines
                            .iter()
                            .map(|line| wrapped_line_element(line, &theme)),
                    ),
                &theme,
                window,
                cx,
            )
            .flex_1()
            .min_h_0(),
        );
    }
    let count = section.lines.len() + 1;
    div().flex().flex_1().min_h_0().child(
        scrollable(
            "kube-log",
            &section.scroll,
            div()
                .font_family(MONO_FONT)
                .text_size(px(CODE_SIZE))
                .text_color(theme.code)
                .child(
                    uniform_list(
                        "kube-log",
                        count,
                        cx.processor(move |this, range: std::ops::Range<usize>, _, _| {
                            let current = this.kube.output.current().cloned().unwrap_or_default();
                            range
                                .map(|index| {
                                    let line = this.kube.lines.get(index).unwrap_or(&current);
                                    line_element(line, &theme)
                                })
                                .collect::<Vec<_>>()
                        }),
                    )
                    .track_scroll(&section.scroll)
                    .size_full(),
                ),
            &theme,
            window,
            cx,
        )
        .flex_1()
        .min_h_0(),
    )
}

fn revealed_view(
    _name: &str,
    value: &str,
    scroll: &ScrollHandle,
    theme: &Theme,
    window: &mut Window,
    cx: &mut Context<DockerPanel>,
) -> Div {
    let theme = *theme;
    let lines: Vec<Line> = value
        .lines()
        .map(|line| Line::plain(line, Default::default()))
        .collect();
    div().flex().flex_1().min_h_0().child(
        scrollable(
            "kube-value",
            scroll,
            div()
                .id("kube-value")
                .flex()
                .flex_col()
                .overflow_y_scroll()
                .track_scroll(scroll)
                .font_family(MONO_FONT)
                .text_size(px(CODE_SIZE))
                .text_color(theme.code)
                .children(lines.iter().map(|line| line_element(line, &theme))),
            &theme,
            window,
            cx,
        )
        .flex_1()
        .min_h_0(),
    )
}

fn state_icon(state: &str, kind: ResourceKind) -> &'static str {
    if kind == ResourceKind::Secret {
        return "general/lock";
    }
    if state == "Running" || state == "Active" || state == "Completed" {
        "run/run"
    } else {
        "run/stop"
    }
}

fn resource_hint(resource: &Resource) -> String {
    let mut hint = resource.state.clone();
    if !resource.ready.is_empty() {
        hint.push(' ');
        hint.push_str(&resource.ready);
    }
    if resource.restarts > 0 {
        hint.push_str(&format!(" · {} restarts", resource.restarts));
    }
    if !resource.detail.is_empty() {
        hint.push_str(" · ");
        hint.push_str(&resource.detail);
    }
    hint
}

fn kind_key(kind: ResourceKind) -> String {
    format!("kube-kind-{}", kind.plural())
}

fn resource_key(kind: ResourceKind, name: &str) -> String {
    format!("kube-resource-{}-{name}", kind.plural())
}

/// Builds the CLI of the settings or of the scripted run, and `None` when the machine has
/// no `kubectl`.
fn find_cli(cx: &App) -> Option<Kubectl> {
    #[cfg(test)]
    if let Some(path) = cx.try_global::<TestCli>() {
        return Some(Kubectl::new(path.0.clone()));
    }
    if let Some(path) = std::env::var_os(CLI_VARIABLE) {
        return Some(Kubectl::new(PathBuf::from(path)));
    }
    let settings = cx.try_global::<workspace::Settings>();
    let program = settings
        .map(|settings| settings.kubectl_path.trim().to_owned())
        .filter(|path| !path.is_empty());
    let cli = match program {
        Some(path) => Kubectl::new(PathBuf::from(path)),
        None => Kubectl::find()?,
    };
    let kubeconfig = settings
        .map(|settings| settings.kubeconfig_path.trim().to_owned())
        .filter(|path| !path.is_empty())
        .map(PathBuf::from);
    Some(cli.with_kubeconfig(kubeconfig))
}

/// One Kubernetes document of a YAML file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Document {
    /// The line of the first key, from zero.
    pub start_line: u32,
    pub text: String,
    /// The `kind` of the manifest as the file writes it.
    pub kind_word: String,
    pub name: String,
    pub namespace: String,
}

/// The documents of a YAML file that carry both `apiVersion` and `kind`, each with its
/// text and its first line.
pub(crate) fn documents(text: &str) -> Vec<Document> {
    let mut documents = Vec::new();
    let mut start = 0;
    let mut block: Vec<&str> = Vec::new();
    for line in text.lines() {
        if line.trim() == "---" {
            push_document(&mut documents, start, &block);
            start += block.len() + 1;
            block.clear();
            continue;
        }
        block.push(line);
    }
    push_document(&mut documents, start, &block);
    documents
}

fn push_document(documents: &mut Vec<Document>, start: usize, lines: &[&str]) {
    let mut first_key = None;
    let mut api = false;
    let mut kind_word = None;
    let mut in_metadata = false;
    let mut name = String::new();
    let mut namespace = String::new();
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let top = !line.starts_with([' ', '\t']);
        if top {
            if trimmed.starts_with("apiVersion:") {
                api = true;
            }
            if let Some(value) = trimmed.strip_prefix("kind:") {
                kind_word = Some(value.trim().to_owned());
            }
            in_metadata = trimmed == "metadata:";
            if !trimmed.starts_with("apiVersion:")
                && !trimmed.starts_with("kind:")
                && !trimmed.starts_with("metadata:")
            {
                in_metadata = false;
            }
            first_key.get_or_insert(index);
        } else if in_metadata {
            if let Some(value) = trimmed.strip_prefix("name:") {
                name = unquote(value.trim());
            }
            if let Some(value) = trimmed.strip_prefix("namespace:") {
                namespace = unquote(value.trim());
            }
        }
    }
    let Some(kind_word) = kind_word else {
        return;
    };
    if !api {
        return;
    }
    let Some(first_key) = first_key else {
        return;
    };
    let mut text = lines.join("\n");
    text.push('\n');
    documents.push(Document {
        start_line: (start + first_key) as u32,
        text,
        kind_word,
        name,
        namespace,
    });
}

fn unquote(value: &str) -> String {
    value.trim_matches(|c| c == '"' || c == '\'').to_owned()
}

/// Recomputes the gutter badges of a YAML file: one per Kubernetes document.
pub(crate) fn refresh_marks(view: &mut EditorView, cx: &mut Context<EditorView>) {
    let path = view.path().to_path_buf();
    let yaml = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("yaml") || extension.eq_ignore_ascii_case("yml")
        });
    if !yaml {
        if !view.action_marks().is_empty() {
            view.set_action_marks(Vec::new(), cx);
        }
        return;
    }
    let Some(text) = view.text() else {
        return;
    };
    let theme = *cx.global::<Theme>();
    let color = theme.accent;
    let marks = documents(&text)
        .into_iter()
        .map(|document| ActionMark {
            line: document.start_line,
            icon: ui_kit::icon_file(theme.icon_set(), "toolwindows/services", theme.dark, 13.),
            color,
            hint: format!("{} {}", document.kind_word, document.name).into(),
        })
        .collect();
    view.set_action_marks(marks, cx);
}

/// The menu of a Kubernetes document in the gutter: Apply, Delete and Diff.
pub(crate) fn show_document_menu(
    view: &Entity<EditorView>,
    line: usize,
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(text) = view.read(cx).text() else {
        return;
    };
    let Some(document) = documents(&text)
        .into_iter()
        .find(|document| document.start_line as usize == line)
    else {
        return;
    };
    let path = view.read(cx).path().to_path_buf();
    let weak = cx.weak_entity();
    let entries = vec![
        MenuEntry::new("Apply to Cluster", {
            let weak = weak.clone();
            let document = document.clone();
            move |window, cx| {
                apply_document(weak.clone(), document.clone(), window, cx);
            }
        })
        .icon("general/arrowUp"),
        MenuEntry::new("Diff with Cluster", {
            let weak = weak.clone();
            let path = path.clone();
            let document = document.clone();
            move |window, cx| {
                diff_document(weak.clone(), path.clone(), document.clone(), window, cx);
            }
        })
        .icon("general/compare"),
        MenuEntry::separator(),
        MenuEntry::new("Delete from Cluster", {
            let weak = weak.clone();
            let document = document.clone();
            move |window, cx| {
                delete_document(weak.clone(), document.clone(), window, cx);
            }
        })
        .icon("general/trash"),
    ];
    workspace.show_menu(window.mouse_position(), entries, cx);
}

fn apply_document(
    workspace: WeakEntity<Workspace>,
    document: Document,
    window: &mut Window,
    cx: &mut App,
) {
    if document.name.is_empty() {
        notify(
            &workspace,
            NotificationLevel::Warning,
            "Kubernetes",
            "The document has no metadata.name",
            cx,
        );
        return;
    }
    let Some(cli) = cli_of(cx) else {
        notify(
            &workspace,
            NotificationLevel::Warning,
            "Kubernetes",
            NO_CLI,
            cx,
        );
        return;
    };
    let task = cx
        .background_executor()
        .spawn(async move { cli.apply_manifest(&document.text) });
    window
        .spawn(cx, async move |cx| {
            let result = task.await;
            cx.update(|_, cx| match result {
                Ok(output) => {
                    let text = first_line(&output).unwrap_or_else(|| "applied".to_owned());
                    notify(
                        &workspace,
                        NotificationLevel::Info,
                        "Apply to Cluster",
                        &text,
                        cx,
                    );
                }
                Err(error) => notify(
                    &workspace,
                    NotificationLevel::Error,
                    "Apply to Cluster",
                    &error,
                    cx,
                ),
            })
            .ok();
        })
        .detach();
}

/// Delete from the gutter asks first, as a manifest can name something that has data.
fn delete_document(
    workspace: WeakEntity<Workspace>,
    document: Document,
    window: &mut Window,
    cx: &mut App,
) {
    if document.name.is_empty() {
        notify(
            &workspace,
            NotificationLevel::Warning,
            "Kubernetes",
            "The document has no metadata.name",
            cx,
        );
        return;
    }
    let Some(cli) = cli_of(cx) else {
        notify(
            &workspace,
            NotificationLevel::Warning,
            "Kubernetes",
            NO_CLI,
            cx,
        );
        return;
    };
    let cli =
        cli.with_namespace((!document.namespace.is_empty()).then(|| document.namespace.clone()));
    let question = format!(
        "Delete {} {} from the cluster?",
        document.kind_word, document.name
    );
    let kind = document.kind_word.clone();
    let name = document.name.clone();
    let weak = workspace.clone();
    workspace
        .update(cx, |workspace, cx| {
            Prompt::ask(
                "Delete from Cluster",
                &question,
                move |cx| {
                    let (kind, name, cli) = (kind.clone(), name.clone(), cli.clone());
                    let weak = weak.clone();
                    let task = cx
                        .background_executor()
                        .spawn(async move { cli.delete_named(&kind, &name) });
                    cx.spawn(async move |cx| {
                        let result = task.await;
                        cx.update(|cx| match result {
                            Ok(()) => notify(
                                &weak,
                                NotificationLevel::Info,
                                "Delete from Cluster",
                                "deleted",
                                cx,
                            ),
                            Err(error) => notify(
                                &weak,
                                NotificationLevel::Error,
                                "Delete from Cluster",
                                &error,
                                cx,
                            ),
                        });
                    })
                    .detach();
                },
                workspace,
                window,
                cx,
            );
        })
        .ok();
}

fn diff_document(
    workspace: WeakEntity<Workspace>,
    path: PathBuf,
    document: Document,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(cli) = cli_of(cx) else {
        notify(
            &workspace,
            NotificationLevel::Warning,
            "Kubernetes",
            NO_CLI,
            cx,
        );
        return;
    };
    if document.name.is_empty() {
        notify(
            &workspace,
            NotificationLevel::Warning,
            "Kubernetes",
            "The document has no metadata.name",
            cx,
        );
        return;
    }
    let Some(git) = cx
        .try_global::<crate::git::ActiveGit>()
        .map(|git| git.0.clone())
    else {
        notify(
            &workspace,
            NotificationLevel::Warning,
            "Diff with Cluster",
            "Git is not ready",
            cx,
        );
        return;
    };
    let kind = document.kind_word.clone();
    let name = document.name.clone();
    let local = document.text.clone();
    let cli =
        cli.with_namespace((!document.namespace.is_empty()).then(|| document.namespace.clone()));
    let task = cx.background_executor().spawn(async move {
        let difference = cli.diff_manifest(&document.text)?;
        if difference.trim().is_empty() {
            return Ok::<Option<String>, String>(None);
        }
        let cluster = cli.yaml_named(&kind, &name)?;
        Ok(Some(cluster))
    });
    window
        .spawn(cx, async move |cx| match task.await {
            Ok(None) => {
                cx.update(|_, cx| {
                    notify(
                        &workspace,
                        NotificationLevel::Info,
                        "Diff with Cluster",
                        "No differences",
                        cx,
                    )
                })
                .ok();
            }
            Ok(Some(cluster)) => {
                let weak = workspace.clone();
                cx.update(|window, cx| {
                    weak.update(cx, |_, cx| {
                        DiffView::compare_texts(
                            git,
                            weak.clone(),
                            path.clone(),
                            cluster,
                            local.clone(),
                            window,
                            cx,
                        );
                    })
                    .ok();
                })
                .ok();
            }
            Err(error) => {
                cx.update(|_, cx| {
                    notify(
                        &workspace,
                        NotificationLevel::Error,
                        "Diff with Cluster",
                        &error,
                        cx,
                    )
                })
                .ok();
            }
        })
        .detach();
}

const NO_CLI: &str = "Kubernetes is not installed: no kubectl CLI found";

fn cli_of(cx: &App) -> Option<Kubectl> {
    find_cli(cx)
}

fn notify(
    workspace: &WeakEntity<Workspace>,
    level: NotificationLevel,
    title: &str,
    text: &str,
    cx: &mut App,
) {
    workspace
        .update(cx, |workspace, cx| match level {
            NotificationLevel::Info => workspace.notify_user(title, text, cx),
            NotificationLevel::Warning => workspace.notify_warning(title, text, cx),
            NotificationLevel::Error => workspace.notify_error(title, text, cx),
        })
        .ok();
}

fn first_line(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::documents;

    #[test]
    fn one_file_has_a_document_for_every_kubernetes_object() {
        let text = "apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: web\n  namespace: shop\n---\nnot: a manifest\n---\napiVersion: v1\nkind: Service\nmetadata:\n  name: web\n";

        let documents = documents(text);

        assert_eq!(documents.len(), 2);
        assert_eq!(documents[0].kind_word, "Deployment");
        assert_eq!(documents[0].name, "web");
        assert_eq!(documents[0].namespace, "shop");
        assert_eq!(documents[0].start_line, 0);
        assert_eq!(documents[1].kind_word, "Service");
        assert_eq!(documents[1].start_line, 8);
        assert!(documents[1].text.contains("name: web"));
    }

    #[test]
    fn a_document_without_api_version_or_kind_is_not_marked() {
        assert!(documents("kind: ConfigMap\nmetadata:\n  name: x\n").is_empty());
        assert!(documents("apiVersion: v1\nmetadata:\n  name: x\n").is_empty());
        assert_eq!(documents("apiVersion: v1\nkind: Namespace\n").len(), 1);
    }
}
