//! The Services tool window with Docker: the daemon, its containers and images, actions on
//! them, live logs in a console beside the tree, compose and exec in the terminal. Docker
//! is reached through its CLI, see `services/container`.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::Duration;

use container::{
    ComposeAction, Container, ContainerState, DaemonState, DockerCli, DockerSnapshot, DockerWatch,
};
use runner::{Console, ConsoleEvent, RunPlan, Stream};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::{SERVICES_TOOL, TERMINAL_TOOL, Workspace};

use crate::ansi::{AnsiParser, Line};
use crate::run::{error_style, info, line_element};
use crate::terminal_panel::ActiveTerminal;

/// How often the panel asks Docker, and how often it reads the log and the answers.
const EVERY: Duration = Duration::from_secs(2);
const POLL: Duration = Duration::from_millis(100);
const LOG_GRACE: Duration = Duration::from_secs(1);
const MAX_LINES: usize = 20_000;
const EVENTS_PER_POLL: usize = 2_000;
const GROUP: &str = "bottom-panel";
const TREE_WIDTH: f32 = 420.;
/// Points the panel at another `docker`, for scripted runs against a fake one.
const CLI_VARIABLE: &str = "IDE_DOCKER_CLI";
const COMPOSE_FILES: [&str; 4] = [
    "compose.yaml",
    "compose.yml",
    "docker-compose.yaml",
    "docker-compose.yml",
];
const EXEC_SHELL: &str = "sh";

/// The panel of the window, for scripts.
pub struct ActiveDocker(pub Entity<DockerPanel>);

impl Global for ActiveDocker {}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Selected {
    Container(String),
    Image(String),
}

pub struct DockerPanel {
    workspace: WeakEntity<Workspace>,
    window: AnyWindowHandle,
    focus: FocusHandle,
    scroll: UniformListScrollHandle,
    scroll_tree: ScrollHandle,
    cli: Option<DockerCli>,
    watch: Option<DockerWatch>,
    snapshot: DockerSnapshot,
    selected: Option<Selected>,
    compose: Option<PathBuf>,
    /// Actions running in the background, by their name.
    busy: Vec<String>,
    console: Option<Console>,
    log_title: SharedString,
    lines: VecDeque<Line>,
    parser: AnsiParser,
    /// The panel drains the watch and the log while this runs.
    polling: bool,
}

fn find_cli() -> Option<DockerCli> {
    match std::env::var_os(CLI_VARIABLE) {
        Some(path) => Some(DockerCli::new(PathBuf::from(path))),
        None => DockerCli::find(),
    }
}

fn no_cli() -> DockerSnapshot {
    DockerSnapshot {
        daemon: DaemonState::NoCli,
        contexts: Vec::new(),
        containers: Vec::new(),
        images: Vec::new(),
        problem: None,
    }
}

fn daemon_text(daemon: &DaemonState) -> String {
    match daemon {
        DaemonState::NoCli => "Docker is not installed: no docker CLI found".into(),
        DaemonState::Down { .. } => "Docker is not running".into(),
        DaemonState::Up { version, os } => format!("Docker {version}, {os}"),
    }
}

fn state_icon(state: ContainerState) -> &'static str {
    match state {
        ContainerState::Running => "run/run",
        _ => "run/stop",
    }
}

impl DockerPanel {
    pub fn create(
        root: PathBuf,
        workspace: WeakEntity<Workspace>,
        window: AnyWindowHandle,
        cx: &mut App,
    ) -> Entity<Self> {
        cx.new(|cx| {
            let cli = find_cli();
            let observer = workspace.clone();
            let compose = COMPOSE_FILES
                .iter()
                .map(|name| root.join(name))
                .find(|path| path.is_file());
            let panel = Self {
                workspace,
                window,
                focus: cx.focus_handle(),
                scroll: UniformListScrollHandle::new(),
                scroll_tree: ScrollHandle::new(),
                cli,
                // The watch starts when the tool window opens: a closed one asks nothing.
                watch: None,
                snapshot: no_cli(),
                selected: None,
                compose,
                busy: Vec::new(),
                console: None,
                log_title: "Logs".into(),
                lines: VecDeque::new(),
                parser: AnsiParser::default(),
                polling: false,
            };
            if let Some(workspace) = observer.upgrade() {
                cx.observe(&workspace, |this: &mut Self, _, cx| {
                    this.visibility_changed(cx)
                })
                .detach();
            }
            panel
        })
    }

    pub fn focus(&self) -> FocusHandle {
        self.focus.clone()
    }

    /// Whether the panel holds a watch of Docker. A closed panel holds none, so no
    /// process of the CLI runs for it.
    #[cfg(test)]
    pub(crate) fn watching(&self) -> bool {
        self.watch.is_some()
    }

    /// How many processes the CLI started, for the idle test.
    #[cfg(test)]
    pub(crate) fn cli_calls(&self) -> u64 {
        self.cli.as_ref().map_or(0, DockerCli::calls)
    }

    /// Lines for the report of a script.
    pub fn report(&self) -> Vec<String> {
        let daemon = match &self.snapshot.daemon {
            DaemonState::NoCli => "docker daemon no-cli".to_string(),
            DaemonState::Down { reason } => format!("docker daemon down {reason}"),
            DaemonState::Up { version, .. } => format!("docker daemon up {version}"),
        };
        let mut lines = vec![daemon];
        for context in &self.snapshot.contexts {
            lines.push(format!(
                "docker context {} {}",
                context.name, context.current
            ));
        }
        for container in &self.snapshot.containers {
            lines.push(format!(
                "docker container {} {} {:?}",
                container.name, container.image, container.state
            ));
        }
        for image in &self.snapshot.images {
            lines.push(format!("docker image {}:{}", image.repository, image.tag));
        }
        if let Some(problem) = &self.snapshot.problem {
            lines.push(format!("docker problem {problem}"));
        }
        for busy in &self.busy {
            lines.push(format!("docker busy {busy}"));
        }
        lines.extend(
            self.lines
                .iter()
                .map(|line| format!("docker log {}", line.text)),
        );
        lines
    }

    /// For scripts: `run` an image, or `start`, `stop`, `restart`, `remove`, `logs` or
    /// `exec` a container by its name, or `compose-up` and `compose-down` with any name.
    /// False while the target is not there yet.
    pub fn act_named(&mut self, action: &str, name: &str, cx: &mut Context<Self>) -> bool {
        match action {
            "compose-up" => self.compose(ComposeAction::Up, cx),
            "compose-down" => self.compose(ComposeAction::Down, cx),
            _ => {}
        }
        if action.starts_with("compose-") {
            return true;
        }
        if action == "run" {
            let found = self
                .snapshot
                .images
                .iter()
                .any(|image| image.repository == name);
            if found {
                self.run_image(name.to_owned(), cx);
            }
            return found;
        }
        let Some(container) = self.container_named(name).cloned() else {
            return false;
        };
        self.selected = Some(Selected::Container(container.id.clone()));
        match action {
            "logs" => self.show_logs(&container, cx),
            "exec" => self.exec(&container, cx),
            _ => self.container_action(action, &container, cx),
        }
        true
    }

    fn container_named(&self, name: &str) -> Option<&Container> {
        self.snapshot
            .containers
            .iter()
            .find(|container| container.name == name)
    }

    /// The tool window opened or closed. Docker is asked only while it is visible, and a
    /// closed one leaves no thread behind.
    fn visibility_changed(&mut self, cx: &mut Context<Self>) {
        let open = self
            .workspace
            .read_with(cx, |workspace, _| workspace.tool_open(SERVICES_TOOL))
            .unwrap_or(false);
        if open {
            self.start(cx);
        } else {
            self.watch = None;
        }
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        if self.watch.is_none()
            && let Some(cli) = self.cli.clone()
        {
            self.watch = Some(DockerWatch::start(cli, EVERY));
        }
        if !self.polling {
            self.polling = true;
            self.drain_in_background(cx);
        }
    }

    fn drain_in_background(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(POLL).await;
                let keep = this.update(cx, |this, cx| this.poll(cx));
                if !matches!(keep, Ok(true)) {
                    break;
                }
            }
            this.update(cx, |this, _| this.polling = false).ok();
        })
        .detach();
    }

    fn poll(&mut self, cx: &mut Context<Self>) -> bool {
        let mut changed = false;
        if let Some(watch) = &self.watch {
            while let Ok(snapshot) = watch.snapshots().try_recv() {
                self.snapshot = snapshot;
                changed = true;
            }
        }
        let mut done = Vec::new();
        let mut ended = false;
        if let Some(console) = &self.console {
            for _ in 0..EVENTS_PER_POLL {
                let Ok(event) = console.events().try_recv() else {
                    break;
                };
                changed = true;
                match event {
                    ConsoleEvent::Output { stream, text } => {
                        self.parser.push(&text, stream == Stream::Stderr, &mut done);
                    }
                    ConsoleEvent::Started { .. } => {}
                    ConsoleEvent::Failed { message } => {
                        done.push(Line::plain(message, error_style()));
                    }
                    ConsoleEvent::Exited { .. } => {
                        self.parser.finish(&mut done);
                        done.push(info("The log ended"));
                        ended = true;
                        break;
                    }
                    ConsoleEvent::Closed => break,
                }
            }
        }
        self.lines.extend(done);
        let extra = self.lines.len().saturating_sub(MAX_LINES);
        self.lines.drain(..extra);
        if ended {
            self.console = None;
        }
        if changed {
            let last = self.lines.len();
            self.scroll.scroll_to_item(last, ScrollStrategy::Bottom);
            cx.notify();
        }
        // Nothing to read once the panel is hidden and its log ended.
        self.watch.is_some() || self.console.is_some()
    }

    /// A call of the CLI waits for Docker, so it runs in the background. The tree is read
    /// again when it ends.
    fn act(
        &mut self,
        name: String,
        cx: &mut Context<Self>,
        work: impl FnOnce(&DockerCli) -> Result<(), String> + Send + 'static,
    ) {
        let Some(cli) = self.cli.clone() else {
            return;
        };
        self.busy.push(name.clone());
        let task = cx.background_executor().spawn(async move { work(&cli) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                if let Some(index) = this.busy.iter().position(|busy| *busy == name) {
                    this.busy.remove(index);
                }
                if let Err(error) = result {
                    this.tell(name, error, cx);
                }
                if let Some(watch) = &this.watch {
                    watch.refresh();
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn run_image(&mut self, image: String, cx: &mut Context<Self>) {
        let name = format!("Run {image}");
        self.act(name, cx, move |cli| cli.run_detached(&image, &[]).map(drop));
    }

    fn container_action(&mut self, action: &str, container: &Container, cx: &mut Context<Self>) {
        let id = container.id.clone();
        let name = format!("{action} {}", container.name);
        match action {
            "start" => self.act(name, cx, move |cli| cli.start(&id)),
            "stop" => self.act(name, cx, move |cli| cli.stop(&id)),
            "restart" => self.act(name, cx, move |cli| cli.restart(&id)),
            "remove" => self.act(name, cx, move |cli| cli.remove(&id)),
            _ => {}
        }
    }

    /// The log of one container replaces the one shown before.
    fn show_logs(&mut self, container: &Container, cx: &mut Context<Self>) {
        let Some(cli) = &self.cli else {
            return;
        };
        if let Some(console) = &self.console {
            console.stop();
        }
        self.log_title = format!("Logs: {}", container.name).into();
        self.lines.clear();
        self.parser = AnsiParser::default();
        self.console = Some(Console::start(RunPlan {
            before: None,
            program: cli.logs(&container.id),
            grace: LOG_GRACE,
        }));
        cx.notify();
    }

    fn exec(&mut self, container: &Container, cx: &mut Context<Self>) {
        let Some(cli) = &self.cli else {
            return;
        };
        let line = cli.exec_command_line(&container.id, EXEC_SHELL);
        self.in_terminal(line, cx);
    }

    fn compose(&mut self, action: ComposeAction, cx: &mut Context<Self>) {
        let (Some(cli), Some(file)) = (&self.cli, &self.compose) else {
            return;
        };
        let line = cli.compose_command_line(file, action);
        self.in_terminal(line, cx);
    }

    /// The terminal is another panel that updates the workspace, so this runs deferred.
    fn in_terminal(&self, line: String, cx: &mut Context<Self>) {
        let (workspace, window) = (self.workspace.clone(), self.window);
        cx.defer(move |cx| {
            let terminal = cx.global::<ActiveTerminal>().0.clone();
            let sent = terminal.read(cx).run_command(&line);
            window
                .update(cx, |_, window, cx| {
                    workspace.update(cx, |workspace, cx| match sent {
                        Ok(()) => workspace.show_tool(TERMINAL_TOOL, window, cx),
                        Err(error) => workspace.notify_user("Terminal", error, cx),
                    })
                })
                .ok();
        });
    }

    fn tell(&self, title: String, text: String, cx: &mut Context<Self>) {
        let workspace = self.workspace.clone();
        cx.defer(move |cx| {
            workspace
                .update(cx, |workspace, cx| workspace.notify_user(title, text, cx))
                .ok();
        });
    }

    fn select(&mut self, selected: Selected, cx: &mut Context<Self>) {
        self.selected = Some(selected);
        cx.notify();
    }

    fn selected_container(&self) -> Option<&Container> {
        let Some(Selected::Container(id)) = &self.selected else {
            return None;
        };
        self.snapshot
            .containers
            .iter()
            .find(|container| container.id == *id)
    }

    fn selected_image(&self) -> Option<String> {
        let Some(Selected::Image(id)) = &self.selected else {
            return None;
        };
        let image = self.snapshot.images.iter().find(|image| image.id == *id)?;
        Some(format!("{}:{}", image.repository, image.tag))
    }

    fn tree(&self, theme: &Theme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut rows = Vec::new();
        let up = matches!(self.snapshot.daemon, DaemonState::Up { .. });
        let reason = match &self.snapshot.daemon {
            DaemonState::Down { reason } => Some(reason.clone().into()),
            _ => None,
        };
        rows.push(
            tree_row(
                TreeRow {
                    id: "docker-daemon".into(),
                    depth: 0,
                    label: daemon_text(&self.snapshot.daemon).into(),
                    hint: reason,
                    icon: "toolwindows/services",
                    expandable: up,
                    open: up,
                    selected: false,
                },
                theme,
            )
            .into_any_element(),
        );
        if let Some(context) = self
            .snapshot
            .contexts
            .iter()
            .find(|context| context.current)
        {
            rows.push(group_row(
                "docker-context",
                &format!("Context {}", context.name),
                theme,
            ));
        }
        if !up {
            return rows;
        }
        rows.push(group_row("docker-containers", "Containers", theme));
        for (index, container) in self.snapshot.containers.iter().enumerate() {
            let selected = self.selected == Some(Selected::Container(container.id.clone()));
            let id = container.id.clone();
            let hint = match &container.compose_project {
                Some(project) => format!("{} · {} · {project}", container.image, container.status),
                None => format!("{} · {}", container.image, container.status),
            };
            rows.push(
                tree_row(
                    TreeRow {
                        id: ("docker-container", index).into(),
                        depth: 2,
                        label: container.name.clone().into(),
                        hint: Some(hint.into()),
                        icon: state_icon(container.state),
                        expandable: false,
                        open: false,
                        selected,
                    },
                    theme,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.select(Selected::Container(id.clone()), cx)
                }))
                .into_any_element(),
            );
        }
        rows.push(group_row("docker-images", "Images", theme));
        for (index, image) in self.snapshot.images.iter().enumerate() {
            let selected = self.selected == Some(Selected::Image(image.id.clone()));
            let id = image.id.clone();
            rows.push(
                tree_row(
                    TreeRow {
                        id: ("docker-image", index).into(),
                        depth: 2,
                        label: format!("{}:{}", image.repository, image.tag).into(),
                        hint: Some(image.size.clone().into()),
                        icon: "fileTypes/text",
                        expandable: false,
                        open: false,
                        selected,
                    },
                    theme,
                )
                .on_click(
                    cx.listener(move |this, _, _, cx| this.select(Selected::Image(id.clone()), cx)),
                )
                .into_any_element(),
            );
        }
        rows
    }

    /// Buttons for what is selected, and compose when the project has a compose file.
    fn tools(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let mut bar = div()
            .flex()
            .items_center()
            .gap(px(SPACE_XS))
            .px(px(SPACE_M))
            .h(px(CONTROL_HEIGHT));
        if let Some(container) = self.selected_container().cloned() {
            let running = container.state == ContainerState::Running;
            let actions: [(&'static str, &str, bool); 6] = [
                ("start", "Start", !running),
                ("stop", "Stop", running),
                ("restart", "Restart", running),
                ("logs", "Logs", true),
                ("exec", "Exec", running),
                ("remove", "Remove", true),
            ];
            for (action, label, enabled) in actions {
                let target = container.clone();
                let button = text_button(
                    SharedString::from(format!("docker-{action}")),
                    label,
                    enabled,
                    theme,
                )
                .when(enabled, |this| {
                    this.on_click(cx.listener(move |this, _, _, cx| match action {
                        "logs" => this.show_logs(&target, cx),
                        "exec" => this.exec(&target, cx),
                        _ => this.container_action(action, &target, cx),
                    }))
                });
                bar = bar.child(button);
            }
        }
        if let Some(image) = self.selected_image() {
            bar = bar
                .child(text_button("docker-run", "Run", true, theme).on_click(
                    cx.listener(move |this, _, _, cx| this.run_image(image.clone(), cx)),
                ));
        }
        if self.compose.is_some() && self.cli.is_some() {
            bar = bar
                .child(
                    text_button("docker-compose-up", "Compose Up", true, theme).on_click(
                        cx.listener(|this, _, _, cx| this.compose(ComposeAction::Up, cx)),
                    ),
                )
                .child(
                    text_button("docker-compose-down", "Compose Down", true, theme).on_click(
                        cx.listener(|this, _, _, cx| this.compose(ComposeAction::Down, cx)),
                    ),
                );
        }
        if !self.busy.is_empty() {
            bar = bar.child(
                div()
                    .ml(px(SPACE_M))
                    .text_color(theme.text_secondary)
                    .child(format!("{}…", self.busy.join(", "))),
            );
        }
        bar
    }

    fn rows(&self, range: std::ops::Range<usize>, theme: &Theme) -> Vec<AnyElement> {
        let current = self.parser.current();
        range
            .map(|index| {
                let line = self.lines.get(index).unwrap_or(current);
                line_element(line, theme)
            })
            .collect()
    }
}

fn group_row(id: &'static str, label: &str, theme: &Theme) -> AnyElement {
    tree_row(
        TreeRow {
            id: id.into(),
            depth: 1,
            label: label.to_owned().into(),
            hint: None,
            icon: "nodes/folder",
            expandable: true,
            open: true,
            selected: false,
        },
        theme,
    )
    .into_any_element()
}

fn text_button(
    id: impl Into<ElementId>,
    label: &'static str,
    enabled: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let hover = theme.selection_hover;
    div()
        .id(id)
        .flex()
        .items_center()
        .h(px(CONTROL_HEIGHT))
        .px(px(SPACE_M))
        .rounded(px(CONTROL_RADIUS))
        .when(enabled, |this| this.hover(move |style| style.bg(hover)))
        .when(!enabled, |this| this.text_color(theme.text_secondary))
        .child(label)
}

impl Render for DockerPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let workspace = self.workspace.clone();
        let hide = move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
            workspace
                .update(cx, |workspace, cx| {
                    workspace.toggle_tool(SERVICES_TOOL, window, cx)
                })
                .ok();
        };
        let rows = self.tree(&theme, cx);
        let tools = self.tools(&theme, cx);
        let count = self.lines.len() + 1;
        let tree = div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(TREE_WIDTH))
            .h_full()
            .border_r_1()
            .border_color(theme.border)
            .child(tools)
            .child(
                scrollable(
                    "docker-tree",
                    &self.scroll_tree,
                    div()
                        .id("docker-tree")
                        .flex()
                        .flex_col()
                        .px(px(SPACE_M))
                        .overflow_y_scroll()
                        .track_scroll(&self.scroll_tree)
                        .children(rows),
                    &theme,
                    window,
                    cx,
                )
                .flex_1()
                .min_h_0(),
            );
        let log = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_none()
                    .h(px(30.))
                    .px(px(12.))
                    .text_color(theme.text_secondary)
                    .child(self.log_title.clone()),
            )
            .child(
                scrollable(
                    "docker-log",
                    &self.scroll,
                    div()
                        .font_family(MONO_FONT)
                        .text_size(px(CODE_SIZE))
                        .text_color(theme.code)
                        .child(
                            uniform_list(
                                "docker-log",
                                count,
                                cx.processor(move |this, range, _, _| this.rows(range, &theme)),
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
            );
        island(&theme)
            .id("docker-panel")
            .track_focus(&self.focus)
            .group(GROUP)
            .h_full()
            .child(panel_header("Services", GROUP, hide, &theme))
            .child(div().flex().flex_1().min_h_0().child(tree).child(log))
    }
}
