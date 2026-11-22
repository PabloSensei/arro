//! The Services tool window: Docker with its daemon, its containers grouped by compose
//! project and its images, and the run configurations the language shows as services.
//! Actions on them, live logs in a console beside the tree, the console of a run
//! configuration and its live sections, compose and exec in the terminal. Docker is
//! reached through its CLI, see `services/container`; the runs belong to the Run window
//! and nothing of this panel starts a process of its own.

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::time::Duration;

use container::{
    ComposeAction, Container, ContainerState, DaemonState, DockerCli, DockerSnapshot, DockerWatch,
};
use runner::{
    Console, ConsoleEvent, LiveRequest, RunConfig, RunPlan, ServiceGroup, ServiceState, Stream,
};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::{SERVICES_TOOL, TERMINAL_TOOL, Workspace};

use crate::ansi::{Line, OutputBuffers, Style};
use crate::kubernetes::KubeSection;
use crate::prompt::Prompt;
use crate::run::{RunPanel, error_style, info, line_element, wrapped_line_element};
use crate::terminal_panel::ActiveTerminal;

/// How often the panel asks Docker, and how often it reads the log and the answers.
const EVERY: Duration = Duration::from_secs(2);
const POLL: Duration = Duration::from_millis(100);
const LOG_GRACE: Duration = Duration::from_secs(1);
const MAX_LINES: usize = 20_000;
const EVENTS_PER_POLL: usize = 2_000;
const GROUP: &str = "bottom-panel";
/// The width of the tree when the panel opens, the narrowest it can become and the least
/// room the log keeps beside it.
const TREE_WIDTH: f32 = 420.;
const MIN_TREE_WIDTH: f32 = 220.;
const MIN_LOG_WIDTH: f32 = 280.;
/// The area the pointer grabs between the tree and the log.
const SPLIT_WIDTH: f32 = 5.;
/// Below this width of the right side the action buttons of the header keep their icons
/// and drop their labels, so nothing of them overlaps.
const LABEL_MIN_WIDTH: f32 = 640.;
/// Points the panel at another `docker`, for scripted runs against a fake one.
const CLI_VARIABLE: &str = "IDE_DOCKER_CLI";
const COMPOSE_FILES: [&str; 4] = [
    "compose.yaml",
    "compose.yml",
    "docker-compose.yaml",
    "docker-compose.yml",
];
const EXEC_SHELL: &str = "sh";
/// The colours the names of the containers of a compose project take before their lines.
const LOG_COLORS: [u8; 6] = [2, 3, 4, 5, 6, 1];
/// The keys of the sections of the tree that are collapsed.
const DAEMON: &str = "docker-daemon";
const CONTAINERS: &str = "docker-containers";
const IMAGES: &str = "docker-images";

/// The panel of the window, for scripts.
pub struct ActiveDocker(pub Entity<DockerPanel>);

impl Global for ActiveDocker {}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Selected {
    Container(String),
    Compose(String),
    Image(String),
    /// A configuration of the run groups, by name.
    Run(String),
    /// A group of configurations, by the id its provider gave it.
    RunGroup(String),
    /// The Docker node of the tree. While the daemon does not answer, its reason is on the
    /// right side with a Retry button.
    Daemon,
}

impl Selected {
    /// The text the window keeps in the layout.
    fn key(&self) -> String {
        match self {
            Self::Container(id) => format!("container:{id}"),
            Self::Compose(project) => format!("compose:{project}"),
            Self::Image(id) => format!("image:{id}"),
            Self::Run(name) => format!("run:{name}"),
            Self::RunGroup(group) => format!("run-group:{group}"),
            Self::Daemon => "daemon:".to_owned(),
        }
    }

    fn from_key(key: &str) -> Option<Self> {
        let (kind, value) = key.split_once(':')?;
        match kind {
            "container" => Some(Self::Container(value.to_owned())),
            "compose" => Some(Self::Compose(value.to_owned())),
            "image" => Some(Self::Image(value.to_owned())),
            "run" => Some(Self::Run(value.to_owned())),
            "run-group" => Some(Self::RunGroup(value.to_owned())),
            "daemon" => Some(Self::Daemon),
            _ => None,
        }
    }
}

/// The tabs of the right side: the logs of the selection, the answers of its actions, the
/// console of a run configuration and its live sections.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RightTab {
    Dashboard,
    Console,
    RunConsole,
    Actuator,
}

/// One live `docker logs` and the name of the container it belongs to.
struct LogSource {
    label: String,
    /// The name goes before every line: the log of a compose project mixes containers.
    prefix: bool,
    color: u8,
    console: Console,
    process: Option<u32>,
}

pub struct DockerPanel {
    workspace: WeakEntity<Workspace>,
    window: AnyWindowHandle,
    focus: FocusHandle,
    /// The root of the project, for the compose file of its containers.
    root: PathBuf,
    scroll: UniformListScrollHandle,
    scroll_text: ScrollHandle,
    scroll_console: ScrollHandle,
    scroll_tree: ScrollHandle,
    cli: Option<DockerCli>,
    watch: Option<DockerWatch>,
    snapshot: DockerSnapshot,
    selected: Option<Selected>,
    compose: Option<PathBuf>,
    /// Actions running in the background, by their name.
    busy: Vec<String>,
    logs: Vec<LogSource>,
    lines: VecDeque<Line>,
    output: OutputBuffers,
    /// What the actions answered, for the Console tab.
    console_lines: VecDeque<Line>,
    tab: RightTab,
    tree_width: f32,
    /// The pointer and the width of the tree when the drag of the splitter began.
    resizing: Option<(f32, f32)>,
    collapsed: HashSet<String>,
    show_stopped: bool,
    wrap: bool,
    /// The Run window: the run configurations in the tree and the console of the run.
    runs: Option<Entity<RunPanel>>,
    /// The last state of the run of each configuration, by name.
    run_states: BTreeMap<String, ServiceState>,
    /// The optional groups list their configurations.
    show_all_runs: bool,
    /// The panel drains the watch and the logs while this runs.
    polling: bool,
    /// The Kubernetes node beside Docker, with its own watch and actions.
    pub(crate) kube: KubeSection,
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

/// The icon of a configuration of the run groups: a program that runs, one that is about
/// to, one that never started and one that stopped.
fn run_state_icon(state: &ServiceState) -> &'static str {
    match state {
        ServiceState::Starting => "run/restart",
        ServiceState::Running { .. } => "run/run",
        ServiceState::NotStarted => "run/run",
        ServiceState::Stopped { .. } => "run/stop",
    }
}

fn state_text(state: ContainerState) -> &'static str {
    match state {
        ContainerState::Running => "Running",
        ContainerState::Paused => "Paused",
        ContainerState::Restarting => "Restarting",
        ContainerState::Exited => "Exited",
        ContainerState::Created => "Created",
        ContainerState::Dead => "Dead",
        ContainerState::Other => "Unknown",
    }
}

/// The containers of a compose project, and those without a project, by their index in
/// the snapshot. Projects come in the order of their names.
fn group_containers(containers: &[Container]) -> (Vec<(String, Vec<usize>)>, Vec<usize>) {
    let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
    let mut loose = Vec::new();
    for (index, container) in containers.iter().enumerate() {
        match &container.compose_project {
            Some(project) => match groups.iter_mut().find(|(name, _)| name == project) {
                Some((_, members)) => members.push(index),
                None => groups.push((project.clone(), vec![index])),
            },
            None => loose.push(index),
        }
    }
    groups.sort_by(|left, right| left.0.cmp(&right.0));
    (groups, loose)
}

fn running_count(containers: &[Container], members: &[usize]) -> usize {
    members
        .iter()
        .filter(|index| containers[**index].state == ContainerState::Running)
        .count()
}

fn counter(containers: &[Container], members: &[usize]) -> String {
    format!(
        "Running: ({}/{})",
        running_count(containers, members),
        members.len()
    )
}

impl DockerPanel {
    pub fn create(
        root: PathBuf,
        state_dir: PathBuf,
        workspace: &Workspace,
        weak: WeakEntity<Workspace>,
        window: AnyWindowHandle,
        cx: &mut App,
    ) -> Entity<Self> {
        let layout = workspace.layout();
        let collapsed: HashSet<String> = layout.services_collapsed.iter().cloned().collect();
        let selected = layout
            .services_selected
            .as_deref()
            .and_then(Selected::from_key);
        let show_all_runs = layout.services_show_all;
        cx.new(|cx| {
            let cli = find_cli();
            let observer = weak.clone();
            let compose = COMPOSE_FILES
                .iter()
                .map(|name| root.join(name))
                .find(|path| path.is_file());
            let kube = KubeSection::new(state_dir, weak.clone(), window, cx);
            let panel = Self {
                workspace: weak,
                window,
                focus: cx.focus_handle(),
                root,
                scroll: UniformListScrollHandle::new(),
                scroll_text: ScrollHandle::new(),
                scroll_console: ScrollHandle::new(),
                scroll_tree: ScrollHandle::new(),
                cli,
                // The watch starts when the tool window opens: a closed one asks nothing.
                watch: None,
                snapshot: no_cli(),
                selected,
                compose,
                busy: Vec::new(),
                logs: Vec::new(),
                lines: VecDeque::new(),
                output: OutputBuffers::default(),
                console_lines: VecDeque::new(),
                tab: RightTab::Dashboard,
                tree_width: TREE_WIDTH,
                resizing: None,
                collapsed,
                show_stopped: true,
                wrap: false,
                runs: None,
                run_states: BTreeMap::new(),
                show_all_runs,
                polling: false,
                kube,
            };
            if let Some(workspace) = observer.upgrade() {
                cx.observe(&workspace, |this: &mut Self, _, cx| {
                    this.visibility_changed(cx)
                })
                .detach();
            }
            cx.observe_global::<workspace::Settings>(|this: &mut Self, cx| {
                this.kube.settings_changed(cx)
            })
            .detach();
            panel
        })
    }

    /// The Run window of the same project: the configurations of the tree, their state
    /// and the console of the run. The Services window starts nothing of its own.
    pub fn set_runs(&mut self, runs: Entity<RunPanel>, cx: &mut Context<Self>) {
        cx.observe(&runs, |this, runs, cx| {
            let snapshots = runs.read(cx).service_snapshots();
            this.apply_runs(snapshots, cx);
        })
        .detach();
        let snapshots = runs.read(cx).service_snapshots();
        self.apply_runs(snapshots, cx);
        self.runs = Some(runs);
    }

    /// Keeps the last state of the run of every configuration the Run window shows. Its
    /// console is the one source of the state, so both windows always agree.
    fn apply_runs(&mut self, snapshots: Vec<(String, ServiceState)>, cx: &mut Context<Self>) {
        let mut changed = false;
        for (name, state) in snapshots {
            if self.run_states.get(&name) != Some(&state) {
                changed = true;
            }
            self.run_states.insert(name, state);
        }
        if changed {
            cx.notify();
        }
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

    /// The labels of the live logs, for the test of switching between nodes.
    #[cfg(test)]
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn log_targets(&self) -> Vec<String> {
        self.logs.iter().map(|log| log.label.clone()).collect()
    }

    /// The groups of the tree with their running and their whole count, for the test of
    /// the grouping.
    #[cfg(test)]
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn compose_groups(&self) -> Vec<(String, usize, usize)> {
        let (groups, _) = group_containers(&self.snapshot.containers);
        groups
            .iter()
            .map(|(project, members)| {
                (
                    project.clone(),
                    running_count(&self.snapshot.containers, members),
                    members.len(),
                )
            })
            .collect()
    }

    /// The containers without a compose project, by name.
    #[cfg(test)]
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn loose_containers(&self) -> Vec<String> {
        let (_, loose) = group_containers(&self.snapshot.containers);
        loose
            .iter()
            .map(|index| self.snapshot.containers[*index].name.clone())
            .collect()
    }

    /// The title and the hint of the header of the right side, for a test.
    #[cfg(test)]
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn header_text(&self) -> (String, String) {
        let (title, hint) = self.selection_title();
        (title.to_string(), hint.to_string())
    }

    /// The groups of run configurations and the state of each configuration, for a test.
    #[cfg(test)]
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn service_entries(&self, cx: &App) -> Vec<(String, String, String)> {
        let mut rows = Vec::new();
        for (group, members) in self.run_groups(cx) {
            rows.push((
                group.title.to_owned(),
                String::new(),
                self.group_counter(&members, cx),
            ));
            if group.optional && !self.show_all_runs {
                continue;
            }
            for config in members {
                rows.push((
                    group.title.to_owned(),
                    config.name.clone(),
                    self.run_state(&config.name, cx).text(),
                ));
            }
        }
        rows
    }

    /// The state of the run of one configuration, for a test.
    #[cfg(test)]
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn run_state_text(&self, name: &str, cx: &App) -> String {
        self.run_state(name, cx).text()
    }

    /// The tabs of the right side, for a test.
    #[cfg(test)]
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn right_tab(&self) -> &'static str {
        match self.tab {
            RightTab::Dashboard => "dashboard",
            RightTab::Console => "console",
            RightTab::RunConsole => "run-console",
            RightTab::Actuator => "actuator",
        }
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
        for (name, state) in &self.run_states {
            lines.push(format!("run {name} {}", state.text()));
        }
        lines.extend(
            self.lines
                .iter()
                .map(|line| format!("docker log {}", line.text)),
        );
        lines.extend(self.kube.report());
        lines
    }

    /// For scripts: `run` an image, or `start`, `stop`, `restart`, `remove`, `logs` or
    /// `exec` a container by its name, or `compose-up` and `compose-down` with any name.
    /// False while the target is not there yet.
    pub fn act_named(&mut self, action: &str, name: &str, cx: &mut Context<Self>) -> bool {
        match action {
            "compose-up" => self.compose_file(ComposeAction::Up, cx),
            "compose-down" => self.compose_file(ComposeAction::Down, cx),
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
            "logs" => {
                self.tab = RightTab::Dashboard;
                self.restart_logs(cx);
            }
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
            self.stop_logs(cx);
        }
        self.kube.set_visible(open, cx);
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
            if log.prefix {
                for line in &mut lines {
                    prefix_line(line, &log.label, log.color);
                }
            }
            done.extend(lines);
            if closed {
                ended.push(log);
            } else {
                self.logs.push(log);
            }
        }
        self.lines.extend(done);
        let extra = self.lines.len().saturating_sub(MAX_LINES);
        self.lines.drain(..extra);
        if !ended.is_empty() {
            // A console joins its thread when it is dropped, and that wait belongs to a
            // thread of the background, not to the frame.
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
        // Nothing to read once the panel is hidden and its log ended.
        self.watch.is_some() || !self.logs.is_empty()
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
        self.tab = RightTab::Console;
        self.console_lines
            .push_back(info(format!("{name}: started")));
        let task = cx.background_executor().spawn(async move { work(&cli) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                if let Some(index) = this.busy.iter().position(|busy| *busy == name) {
                    this.busy.remove(index);
                }
                match result {
                    Ok(()) => this.console_lines.push_back(info(format!("{name}: done"))),
                    Err(error) => {
                        this.console_lines
                            .push_back(Line::plain(format!("{name}: {error}"), error_style()));
                        this.tell(name, error, cx);
                    }
                }
                let extra = this.console_lines.len().saturating_sub(MAX_LINES);
                this.console_lines.drain(..extra);
                this.scroll_console.scroll_to_bottom();
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

    fn compose_project_action(
        &mut self,
        project: String,
        action: ComposeAction,
        cx: &mut Context<Self>,
    ) {
        let word = match action {
            ComposeAction::Up => "up",
            ComposeAction::Down => "down",
            ComposeAction::Restart => "restart",
            ComposeAction::Stop => "stop",
        };
        let name = format!("compose {word} {project}");
        let root = self.compose_directory(&project);
        self.act(name, cx, move |cli| {
            cli.compose_project(&project, action, &root)
        });
    }

    /// The directory the compose project was started from, as its containers name it, or
    /// the root of the workspace when the label is not there.
    fn compose_directory(&self, project: &str) -> PathBuf {
        let (groups, _) = group_containers(&self.snapshot.containers);
        groups
            .iter()
            .find(|(name, _)| name == project)
            .and_then(|(_, members)| {
                members.iter().find_map(|index| {
                    self.snapshot.containers[*index]
                        .compose_dir
                        .as_ref()
                        .map(PathBuf::from)
                })
            })
            .unwrap_or_else(|| self.root.clone())
    }

    fn compose_file(&mut self, action: ComposeAction, cx: &mut Context<Self>) {
        let Some(file) = self.compose.clone() else {
            return;
        };
        let word = match action {
            ComposeAction::Up => "up",
            ComposeAction::Down => "down",
            ComposeAction::Restart => "restart",
            ComposeAction::Stop => "stop",
        };
        let name = format!("compose {word} {}", file.display());
        self.act(name, cx, move |cli| cli.compose_file(&file, action));
    }

    /// The log of the selection replaces every log shown before. A log runs until the
    /// selection changes or the tool window closes.
    fn restart_logs(&mut self, cx: &mut Context<Self>) {
        self.stop_logs(cx);
        self.lines.clear();
        self.output = OutputBuffers::default();
        let Some(cli) = self.cli.clone() else {
            cx.notify();
            return;
        };
        let targets: Vec<(String, String, bool)> = match &self.selected {
            Some(Selected::Container(id)) => self
                .snapshot
                .containers
                .iter()
                .find(|container| container.id == *id)
                .map(|container| vec![(container.name.clone(), container.id.clone(), false)])
                .unwrap_or_default(),
            Some(Selected::Compose(project)) => {
                let (groups, _) = group_containers(&self.snapshot.containers);
                groups
                    .iter()
                    .find(|(name, _)| name == project)
                    .map(|(_, members)| {
                        members
                            .iter()
                            .map(|index| {
                                let container = &self.snapshot.containers[*index];
                                (container.name.clone(), container.id.clone(), true)
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            }
            Some(Selected::Image(_))
            | Some(Selected::Run(_))
            | Some(Selected::RunGroup(_))
            | Some(Selected::Daemon)
            | None => Vec::new(),
        };
        for (index, (label, id, prefix)) in targets.into_iter().enumerate() {
            self.logs.push(LogSource {
                label,
                prefix,
                color: LOG_COLORS[index % LOG_COLORS.len()],
                console: Console::start(RunPlan {
                    before: None,
                    program: cli.logs(&id),
                    grace: LOG_GRACE,
                }),
                process: None,
            });
        }
        cx.notify();
    }

    /// Stops every live log. The consoles are dropped away from the frame, because their
    /// drop joins the thread that waits for the process.
    fn stop_logs(&mut self, cx: &mut Context<Self>) {
        let logs = std::mem::take(&mut self.logs);
        if logs.is_empty() {
            return;
        }
        cx.background_executor()
            .spawn(async move { drop(logs) })
            .detach();
    }

    fn exec(&mut self, container: &Container, cx: &mut Context<Self>) {
        let Some(cli) = &self.cli else {
            return;
        };
        let line = cli.exec_command_line(&container.id, EXEC_SHELL);
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
                        Err(error) => workspace.notify_error("Terminal", error, cx),
                    })
                })
                .ok();
        });
    }

    fn tell(&self, title: String, text: String, cx: &mut Context<Self>) {
        let workspace = self.workspace.clone();
        cx.defer(move |cx| {
            workspace
                .update(cx, |workspace, cx| workspace.notify_error(title, text, cx))
                .ok();
        });
    }

    fn select(&mut self, selected: Selected, cx: &mut Context<Self>) {
        self.kube.clear_selection();
        let run = matches!(selected, Selected::Run(_));
        self.selected = Some(selected);
        self.tab = if run {
            RightTab::RunConsole
        } else {
            RightTab::Dashboard
        };
        self.restart_logs(cx);
        self.save_state(cx);
    }

    /// A node of the Kubernetes section was clicked: the Docker selection and its log go
    /// away, and the section shows the node on the right.
    pub(crate) fn kube_select(&mut self, node: crate::kubernetes::Node, cx: &mut Context<Self>) {
        self.selected = None;
        self.stop_logs(cx);
        self.lines.clear();
        self.output = OutputBuffers::default();
        self.kube.select(node, cx);
        cx.notify();
    }

    fn toggle_section(&mut self, key: &str, cx: &mut Context<Self>) {
        if !self.collapsed.remove(key) {
            self.collapsed.insert(key.to_owned());
        }
        self.save_state(cx);
        cx.notify();
    }

    /// Keeps the collapsed groups, the selected node and the optional groups in the
    /// layout of the window.
    fn save_state(&mut self, cx: &mut Context<Self>) {
        let mut collapsed: Vec<String> = self.collapsed.iter().cloned().collect();
        collapsed.sort();
        let selected = self.selected.as_ref().map(Selected::key);
        let show_all = self.show_all_runs;
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.set_services_state(collapsed, selected, show_all, cx)
            })
            .ok();
    }

    fn selected_run(&self) -> Option<&str> {
        match &self.selected {
            Some(Selected::Run(name)) => Some(name),
            _ => None,
        }
    }

    fn selected_run_group(&self) -> Option<&str> {
        match &self.selected {
            Some(Selected::RunGroup(group)) => Some(group),
            _ => None,
        }
    }

    /// The state of the run of a configuration: the one the Run window reports, or the
    /// last state it reported before another run took the console.
    fn run_state(&self, name: &str, cx: &App) -> ServiceState {
        let current = self.runs.as_ref().and_then(|runs| {
            let snapshot = runs.read(cx).service_snapshot()?;
            (snapshot.0 == name).then_some(snapshot.1)
        });
        current
            .or_else(|| self.run_states.get(name).cloned())
            .unwrap_or_default()
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

    fn selected_compose(&self) -> Option<String> {
        match &self.selected {
            Some(Selected::Compose(project)) => Some(project.clone()),
            _ => None,
        }
    }

    fn selected_image(&self) -> Option<String> {
        let Some(Selected::Image(id)) = &self.selected else {
            return None;
        };
        let image = self.snapshot.images.iter().find(|image| image.id == *id)?;
        Some(format!("{}:{}", image.repository, image.tag))
    }

    /// The reason on the right of the Docker node while Docker does not answer: the reason of
    /// a daemon that is down, or the line that the CLI is missing. It is read from the
    /// snapshot, so it follows Docker when Docker comes up or goes down.
    fn daemon_problem(&self) -> Option<String> {
        if self.selected != Some(Selected::Daemon) {
            return None;
        }
        match &self.snapshot.daemon {
            DaemonState::Up { .. } => None,
            DaemonState::NoCli => Some(daemon_text(&self.snapshot.daemon)),
            DaemonState::Down { reason } => Some(reason.clone()),
        }
    }

    /// The text of the right side when it has no log to show. Without a container there is
    /// no container to choose.
    fn empty_text(&self) -> &'static str {
        match &self.selected {
            Some(Selected::Container(_) | Selected::Compose(_) | Selected::Image(_)) => {
                "No log for the selection"
            }
            _ if self.snapshot.containers.is_empty() => "Select a service to see its details",
            _ => "Select a container or a compose project",
        }
    }

    /// Asks Docker again now, from the Retry button of a daemon that does not answer.
    fn ask_daemon(&mut self, cx: &mut Context<Self>) {
        if let Some(watch) = &self.watch {
            watch.refresh();
        } else {
            self.start(cx);
        }
        cx.notify();
    }

    /// The text the right side shows in place of a log, for a test.
    #[cfg(all(test, windows))]
    pub(crate) fn placeholder_text(&self) -> String {
        self.daemon_problem()
            .unwrap_or_else(|| self.empty_text().to_owned())
    }

    /// The name of the node on the right and its state.
    fn selection_title(&self) -> (SharedString, SharedString) {
        if self.kube.selected().is_some() {
            let (title, hint) = self.kube.selection_title();
            return (title.into(), hint.into());
        }
        if self.selected == Some(Selected::Daemon) {
            return (
                daemon_text(&self.snapshot.daemon).into(),
                String::new().into(),
            );
        }
        if let Some(name) = self.selected_run() {
            return (name.into(), String::new().into());
        }
        if let Some(group) = self.selected_run_group() {
            return (group.into(), String::new().into());
        }
        if let Some(container) = self.selected_container() {
            return (
                container.name.clone().into(),
                state_text(container.state).into(),
            );
        }
        if let Some(project) = self.selected_compose() {
            let (groups, _) = group_containers(&self.snapshot.containers);
            let hint = groups
                .iter()
                .find(|(name, _)| *name == project)
                .map(|(_, members)| counter(&self.snapshot.containers, members))
                .unwrap_or_default();
            return (format!("Docker-compose: {project}").into(), hint.into());
        }
        if let Some(image) = self.selected_image() {
            let size = self
                .selected_image_id()
                .and_then(|id| self.snapshot.images.iter().find(|image| image.id == id))
                .map(|image| image.size.clone())
                .unwrap_or_default();
            return (image.into(), size.into());
        }
        ("Services".into(), String::new().into())
    }

    fn selected_image_id(&self) -> Option<&str> {
        match &self.selected {
            Some(Selected::Image(id)) => Some(id),
            _ => None,
        }
    }

    /// The groups of run configurations of the tree with their members, the order the
    /// tree draws them in: a running configuration first, then by name. A group no
    /// provider claims is not there; the optional ones come last.
    fn run_groups(&self, cx: &App) -> Vec<(ServiceGroup, Vec<RunConfig>)> {
        let Some(runs) = &self.runs else {
            return Vec::new();
        };
        let (providers, configs) = {
            let runs = runs.read(cx);
            (runs.providers(), runs.configurations().to_vec())
        };
        let mut groups: Vec<(ServiceGroup, Vec<RunConfig>)> = Vec::new();
        for config in configs {
            let Some(provider) = providers
                .iter()
                .find(|provider| provider.id() == config.provider)
            else {
                continue;
            };
            let Some(group) = provider.service_group(&config) else {
                continue;
            };
            match groups.iter_mut().find(|(known, _)| known.id == group.id) {
                Some((_, members)) => members.push(config),
                None => groups.push((group, vec![config])),
            }
        }
        for (_, members) in &mut groups {
            members.sort_by(|left, right| {
                let left_state = self.run_state(&left.name, cx);
                let right_state = self.run_state(&right.name, cx);
                left_state
                    .rank()
                    .cmp(&right_state.rank())
                    .then_with(|| left.name.cmp(&right.name))
            });
        }
        groups.sort_by(|left, right| {
            left.0
                .optional
                .cmp(&right.0.optional)
                .then_with(|| left.0.title.cmp(right.0.title))
        });
        groups
    }

    fn run_group(&self, id: &str, cx: &App) -> Option<(ServiceGroup, Vec<RunConfig>)> {
        self.run_groups(cx)
            .into_iter()
            .find(|(group, _)| group.id == id)
    }

    /// The counter of a group of configurations, as the header of the group shows it.
    fn group_counter(&self, members: &[RunConfig], cx: &App) -> String {
        let running = members
            .iter()
            .filter(|config| self.run_state(&config.name, cx).active())
            .count();
        format!("Running: ({running}/{})", members.len())
    }

    fn tree(&self, theme: &Theme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut rows = Vec::new();
        for (group, members) in self.run_groups(cx) {
            let key = format!("run:{}", group.id);
            let open = !self.collapsed.contains(&key);
            let selector = format!("services-run-group-{}", group.id);
            let clicked = group.id.to_owned();
            let target = group.id.to_owned();
            let counter = self.group_counter(&members, cx);
            rows.push(
                tree_row(
                    TreeRow {
                        id: selector.clone().into(),
                        depth: 0,
                        label: group.title.into(),
                        hint: Some(counter.into()),
                        icon: "toolwindows/run",
                        expandable: true,
                        open,
                        selected: self.selected_run_group() == Some(group.id),
                    },
                    theme,
                )
                .debug_selector(move || selector.clone())
                .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                    this.select(Selected::RunGroup(clicked.clone()), cx);
                    if event.click_count() >= 2 {
                        this.toggle_section(&format!("run:{clicked}"), cx);
                    }
                }))
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                        this.select(Selected::RunGroup(target.clone()), cx);
                        this.show_run_group_menu(target.clone(), event.position, cx);
                    }),
                )
                .into_any_element(),
            );
            if !open || (group.optional && !self.show_all_runs) {
                continue;
            }
            for config in members {
                rows.push(self.run_row(&config, 1, theme, cx));
            }
        }
        let up = matches!(self.snapshot.daemon, DaemonState::Up { .. });
        // A daemon that does not answer has no branches. Its reason is in the tooltip of the
        // row and on the right side once the node is chosen, not cut beside the label.
        let reason = match &self.snapshot.daemon {
            DaemonState::Down { reason } => Some(reason.clone()),
            _ => None,
        };
        let daemon_open = up && !self.collapsed.contains(DAEMON);
        let daemon = tree_row(
            TreeRow {
                id: DAEMON.into(),
                depth: 0,
                label: daemon_text(&self.snapshot.daemon).into(),
                hint: None,
                icon: "toolwindows/services",
                expandable: up,
                open: daemon_open,
                selected: self.selected == Some(Selected::Daemon),
            },
            theme,
        )
        .debug_selector(|| DAEMON.to_owned())
        .on_click(cx.listener(move |this, _, _, cx| {
            if up {
                this.toggle_section(DAEMON, cx)
            } else {
                this.select(Selected::Daemon, cx)
            }
        }));
        let daemon = match reason {
            Some(reason) => daemon
                .tooltip(move |_, cx| tooltip(reason.clone(), None, cx))
                .tooltip_show_delay(TOOLTIP_CREATE_DELAY),
            None => daemon,
        };
        rows.push(daemon.into_any_element());
        if !up || !daemon_open {
            rows.extend(self.kube.tree(theme, cx));
            return rows;
        }
        if let Some(context) = self
            .snapshot
            .contexts
            .iter()
            .find(|context| context.current)
        {
            rows.push(
                section_row(
                    "docker-context",
                    format!("Context {}", context.name),
                    1,
                    "nodes/folder",
                    false,
                    true,
                    theme,
                )
                .into_any_element(),
            );
        }

        let (groups, loose) = group_containers(&self.snapshot.containers);
        for (project, members) in &groups {
            let key = format!("compose:{project}");
            let open = !self.collapsed.contains(&key);
            let selector = format!("docker-group-{project}");
            let target = project.clone();
            let clicked = project.clone();
            rows.push(
                tree_row(
                    TreeRow {
                        id: selector.clone().into(),
                        depth: 1,
                        label: format!("Docker-compose: {project}").into(),
                        hint: Some(counter(&self.snapshot.containers, members).into()),
                        icon: "toolwindows/services",
                        expandable: true,
                        open,
                        selected: self.selected == Some(Selected::Compose(project.clone())),
                    },
                    theme,
                )
                .debug_selector(move || selector.clone())
                .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                    this.select(Selected::Compose(clicked.clone()), cx);
                    if event.click_count() >= 2 {
                        this.toggle_section(&format!("compose:{clicked}"), cx);
                    }
                }))
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                        this.select(Selected::Compose(target.clone()), cx);
                        this.show_compose_menu(target.clone(), event.position, cx);
                    }),
                )
                .into_any_element(),
            );
            if !open {
                continue;
            }
            for index in members {
                let container = &self.snapshot.containers[*index];
                if !self.show_stopped && container.state != ContainerState::Running {
                    continue;
                }
                rows.push(self.container_row(container, 2, theme, cx));
            }
        }

        let open = !self.collapsed.contains(CONTAINERS);
        rows.push(
            tree_row(
                TreeRow {
                    id: CONTAINERS.into(),
                    depth: 1,
                    label: "Containers".into(),
                    hint: None,
                    icon: "nodes/folder",
                    expandable: true,
                    open,
                    selected: false,
                },
                theme,
            )
            .debug_selector(|| CONTAINERS.to_owned())
            .on_click(cx.listener(|this, _, _, cx| this.toggle_section(CONTAINERS, cx)))
            .into_any_element(),
        );
        if open {
            for index in loose {
                let container = &self.snapshot.containers[index];
                if !self.show_stopped && container.state != ContainerState::Running {
                    continue;
                }
                rows.push(self.container_row(container, 2, theme, cx));
            }
        }

        let open = !self.collapsed.contains(IMAGES);
        rows.push(
            tree_row(
                TreeRow {
                    id: IMAGES.into(),
                    depth: 1,
                    label: "Images".into(),
                    hint: None,
                    icon: "nodes/folder",
                    expandable: true,
                    open,
                    selected: false,
                },
                theme,
            )
            .debug_selector(|| IMAGES.to_owned())
            .on_click(cx.listener(|this, _, _, cx| this.toggle_section(IMAGES, cx)))
            .into_any_element(),
        );
        if open {
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
                    .debug_selector(move || format!("docker-image-{index}"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.select(Selected::Image(id.clone()), cx)
                    }))
                    .into_any_element(),
                );
            }
        }
        rows.extend(self.kube.tree(theme, cx));
        if rows.is_empty() {
            // The empty state of the list: the text and the action that adds a configuration.
            rows.push(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(SPACE_S))
                    .py(px(SPACE_M))
                    .text_color(theme.text_muted)
                    .child("No services configured")
                    .child(
                        div()
                            .id("services-empty-add")
                            .debug_selector(|| "services-empty-add".to_owned())
                            .cursor_pointer()
                            .text_color(theme.accent)
                            .child("Add service")
                            .on_click(cx.listener(|this, _, window, cx| {
                                let workspace = this.workspace.clone();
                                window.defer(cx, move |window, cx| {
                                    workspace
                                        .update(cx, |workspace, cx| {
                                            let panel = cx
                                                .try_global::<crate::script::ActiveRunPanel>()
                                                .map(|panel| panel.0.clone());
                                            if let Some(panel) = panel {
                                                crate::run_configs::RunConfigDialog::show(
                                                    panel, workspace, window, cx,
                                                );
                                            }
                                        })
                                        .ok();
                                });
                            })),
                    )
                    .into_any_element(),
            );
        }
        rows
    }

    fn container_row(
        &self,
        container: &Container,
        depth: usize,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selected = self.selected == Some(Selected::Container(container.id.clone()));
        let id = container.id.clone();
        let clicked = container.id.clone();
        let hint = match &container.compose_project {
            Some(project) => format!("{} · {} · {project}", container.image, container.status),
            None => format!("{} · {}", container.image, container.status),
        };
        let selector = format!("docker-container-{}", container.name);
        tree_row(
            TreeRow {
                id: selector.clone().into(),
                depth,
                label: container.name.clone().into(),
                hint: Some(hint.into()),
                icon: state_icon(container.state),
                expandable: false,
                open: false,
                selected,
            },
            theme,
        )
        .debug_selector(move || selector.clone())
        .on_click(
            cx.listener(move |this, _, _, cx| {
                this.select(Selected::Container(clicked.clone()), cx)
            }),
        )
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                this.select(Selected::Container(id.clone()), cx);
                this.show_container_menu(id.clone(), event.position, cx);
            }),
        )
        .into_any_element()
    }

    /// One configuration of a run group, with the state of its last run.
    fn run_row(
        &self,
        config: &RunConfig,
        depth: usize,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let name = config.name.clone();
        let state = self.run_state(&name, cx);
        let selector = format!("services-run-{}", config.name);
        let selected = self.selected_run() == Some(name.as_str());
        let clicked = name.clone();
        let target = name.clone();
        tree_row(
            TreeRow {
                id: selector.clone().into(),
                depth,
                label: config.name.clone().into(),
                hint: Some(state.text().into()),
                icon: run_state_icon(&state),
                expandable: false,
                open: false,
                selected,
            },
            theme,
        )
        .debug_selector(move || selector.clone())
        .on_click(
            cx.listener(move |this, _, _, cx| this.select(Selected::Run(clicked.clone()), cx)),
        )
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                this.select(Selected::Run(target.clone()), cx);
                this.show_run_menu(target.clone(), event.position, cx);
            }),
        )
        .into_any_element()
    }

    fn tree_toolbar(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let (stopped_icon, stopped_label) = if self.show_stopped {
            ("general/hide", "Hide Stopped Containers")
        } else {
            ("general/show", "Show Stopped Containers")
        };
        div()
            .flex()
            .items_center()
            .gap(px(SPACE_XS))
            .flex_none()
            .h(px(CONTROL_HEIGHT))
            .px(px(SPACE_M))
            .child(
                icon_button(
                    "docker-toolbar-stopped",
                    stopped_icon,
                    stopped_label,
                    None,
                    theme,
                )
                .debug_selector(|| "docker-toolbar-stopped".to_owned())
                .when(self.show_stopped, |this| this.bg(theme.selection_inactive))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.show_stopped = !this.show_stopped;
                    cx.notify();
                })),
            )
            .child(
                icon_button(
                    "docker-toolbar-expand",
                    "general/expandAll",
                    "Expand All",
                    None,
                    theme,
                )
                .debug_selector(|| "docker-toolbar-expand".to_owned())
                .on_click(cx.listener(|this, _, _, cx| {
                    this.collapsed.clear();
                    this.save_state(cx);
                    cx.notify();
                })),
            )
            .child(
                icon_button(
                    "docker-toolbar-collapse",
                    "general/collapseAll",
                    "Collapse All",
                    None,
                    theme,
                )
                .debug_selector(|| "docker-toolbar-collapse".to_owned())
                .on_click(cx.listener(|this, _, _, cx| {
                    this.collapsed.insert(DAEMON.to_owned());
                    this.collapsed.insert(CONTAINERS.to_owned());
                    this.collapsed.insert(IMAGES.to_owned());
                    for (project, _) in group_containers(&this.snapshot.containers).0 {
                        this.collapsed.insert(format!("compose:{project}"));
                    }
                    let groups: Vec<String> = this
                        .run_groups(cx)
                        .into_iter()
                        .map(|(group, _)| format!("run:{}", group.id))
                        .collect();
                    for key in groups {
                        this.collapsed.insert(key);
                    }
                    this.save_state(cx);
                    cx.notify();
                })),
            )
    }

    /// The actions of the selected node, each with its icon and its name. The name goes
    /// away when the window is narrow, so the buttons keep their place and never overlap.
    fn header_actions(&self, cx: &App) -> Vec<(&'static str, &'static str, &'static str, bool)> {
        if self.selected == Some(Selected::Daemon) {
            return match &self.snapshot.daemon {
                DaemonState::Down { .. } => vec![("retry", "run/restart", "Retry", true)],
                _ => Vec::new(),
            };
        }
        if let Some(name) = self.selected_run() {
            let state = self.run_state(name, cx);
            let running = state.active();
            let port = state.port().is_some();
            return vec![
                ("run", "run/run", "Run", !running),
                ("debug", "run/debug", "Debug", !running),
                ("stop-run", "run/stop", "Stop", running),
                ("rerun", "run/restart", "Rerun", true),
                (
                    "browser",
                    "general/openInToolWindow",
                    "Open in Browser",
                    port,
                ),
                ("http", "toolwindows/http", "Open in HTTP Client", port),
                ("edit", "general/settings", "Edit Configuration", true),
            ];
        }
        if self.selected_run_group().is_some() {
            return Vec::new();
        }
        if let Some(container) = self.selected_container() {
            let running = container.state == ContainerState::Running;
            return vec![
                ("start", "run/run", "Start", !running),
                ("stop", "run/stop", "Stop", running),
                ("restart", "run/restart", "Restart", running),
                ("remove", "general/trash", "Remove", true),
            ];
        }
        if self.selected_compose().is_some() {
            return vec![
                ("compose-up", "general/arrowUp", "Up", true),
                ("compose-down", "general/arrowDown", "Down", true),
                ("compose-restart", "run/restart", "Restart", true),
                ("compose-stop", "run/stop", "Stop", true),
            ];
        }
        if self.selected_image().is_some() {
            return vec![("run", "run/run", "Run", true)];
        }
        Vec::new()
    }

    fn header(
        &self,
        theme: &Theme,
        show_labels: bool,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.kube.selected().is_some() {
            return Some(self.kube.header(theme, show_labels, cx));
        }
        let (title, hint) = match (self.selected_run(), self.selected_run_group()) {
            (Some(name), _) => (name.into(), self.run_state(name, cx).text().into()),
            (None, Some(group)) => self.run_group(group, cx).map_or_else(
                || (group.into(), SharedString::default()),
                |(known, members)| (known.title.into(), self.group_counter(&members, cx).into()),
            ),
            (None, None) if self.selected.is_some() => self.selection_title(),
            (None, None) => (SharedString::default(), SharedString::default()),
        };
        let actions = self.header_actions(cx);
        if title.is_empty() && actions.is_empty() {
            return None;
        }
        Some(
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
                        .id("docker-title")
                        .debug_selector(|| "docker-title".to_owned())
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
                            action_button(action, icon, label, enabled, show_labels, theme).when(
                                enabled,
                                |this| {
                                    this.on_click(cx.listener(move |this, _, window, cx| {
                                        this.header_action(action, window, cx)
                                    }))
                                },
                            )
                        })),
                )
                .into_any_element(),
        )
    }

    fn header_action(&mut self, action: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected == Some(Selected::Daemon) {
            if action == "retry" {
                self.ask_daemon(cx);
            }
            return;
        }
        if let Some(name) = self.selected_run().map(str::to_owned) {
            self.run_action(action, &name, window, cx);
            return;
        }
        if self.selected_run_group().is_some() {
            return;
        }
        if let Some(container) = self.selected_container().cloned() {
            self.container_action(action, &container, cx);
            return;
        }
        if let Some(project) = self.selected_compose() {
            let action = match action {
                "compose-up" => ComposeAction::Up,
                "compose-down" => ComposeAction::Down,
                "compose-restart" => ComposeAction::Restart,
                "compose-stop" => ComposeAction::Stop,
                _ => return,
            };
            self.compose_project_action(project, action, cx);
            return;
        }
        if let Some(image) = self.selected_image() {
            self.run_image(image, cx);
        }
    }

    /// One action of a configuration. Every action goes through the Run window: the
    /// Services window has no console and no process of its own.
    fn run_action(
        &mut self,
        action: &str,
        name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(runs) = self.runs.clone() else {
            return;
        };
        match action {
            "run" | "rerun" => {
                runs.update(cx, |runs, cx| {
                    if runs.select_named(name, cx) {
                        runs.run(cx);
                    }
                });
            }
            "debug" => {
                runs.update(cx, |runs, cx| {
                    if runs.select_named(name, cx) {
                        runs.debug(window, cx);
                    }
                });
            }
            "stop-run" => {
                runs.update(cx, |runs, cx| {
                    let active = runs
                        .service_snapshot()
                        .is_some_and(|(_, state)| state.active());
                    if runs.run_config_name() == Some(name) && active {
                        runs.stop(cx);
                    }
                });
            }
            "browser" => self.open_browser(name, cx),
            "http" => self.open_in_http_client(name, window, cx),
            "edit" => {
                let Some(config) = runs
                    .read(cx)
                    .configurations()
                    .iter()
                    .find(|config| config.name == name)
                    .cloned()
                else {
                    return;
                };
                let workspace = self.workspace.clone();
                window.defer(cx, move |window, cx| {
                    workspace
                        .update(cx, |workspace, cx| {
                            crate::run_configs::RunConfigDialog::show_config(
                                runs, config, workspace, window, cx,
                            )
                        })
                        .ok();
                });
            }
            _ => {}
        }
    }

    /// The browser link of a running configuration, on the port its console named.
    fn open_browser(&mut self, name: &str, cx: &mut Context<Self>) {
        let Some(port) = self.run_state(name, cx).port() else {
            return;
        };
        let url = format!("http://localhost:{port}/");
        let opener = cx
            .try_global::<editor_view::ExternalLinks>()
            .and_then(|links| links.0.clone());
        match opener {
            Some(open) => open(&url),
            None => {
                ::platform::open_url(&url);
            }
        }
    }

    /// A scratch request of the HTTP client for the port of a running configuration.
    fn open_in_http_client(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(port) = self.run_state(name, cx).port() else {
            return;
        };
        let url = format!("http://localhost:{port}/");
        let request = LiveRequest {
            method: "GET".to_owned(),
            path: "/".to_owned(),
        };
        let workspace = self.workspace.clone();
        window.defer(cx, move |window, cx| {
            workspace
                .update(cx, |workspace, cx| {
                    crate::endpoints::generate_url(&request, &url, workspace, window, cx)
                })
                .ok();
        });
    }

    /// The menu of a configuration of a run group: the same actions as its header.
    fn show_run_menu(&mut self, name: String, position: Point<Pixels>, cx: &mut Context<Self>) {
        let state = self.run_state(&name, cx);
        let running = state.active();
        let port = state.port().is_some();
        let panel = cx.weak_entity();
        let mut entries = Vec::new();
        for (action, label, icon, enabled) in [
            ("run", "Run", "run/run", !running),
            ("debug", "Debug", "run/debug", !running),
            ("stop-run", "Stop", "run/stop", running),
            ("rerun", "Rerun", "run/restart", true),
            (
                "browser",
                "Open in Browser",
                "general/openInToolWindow",
                port,
            ),
            ("http", "Open in HTTP Client", "toolwindows/http", port),
            ("edit", "Edit Configuration...", "general/settings", true),
        ] {
            if !enabled {
                entries.push(MenuEntry::disabled(label).icon(icon));
                continue;
            }
            let panel = panel.clone();
            let target = name.clone();
            entries.push(
                MenuEntry::new(label, move |window, cx| {
                    panel
                        .update(cx, |this, cx| this.run_action(action, &target, window, cx))
                        .ok();
                })
                .icon(icon),
            );
        }
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.show_menu(position, entries, cx)
            })
            .ok();
    }

    /// The menu of a group of configurations: an optional group lists its configurations
    /// only after Show all run configurations.
    fn show_run_group_menu(&mut self, id: String, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Some((group, _)) = self.run_group(&id, cx) else {
            return;
        };
        if !group.optional {
            return;
        }
        let show = !self.show_all_runs;
        let label = if show {
            "Show all run configurations"
        } else {
            "Hide all run configurations"
        };
        let icon = if show { "general/show" } else { "general/hide" };
        let panel = cx.weak_entity();
        let entry = MenuEntry::new(label, move |_, cx| {
            panel
                .update(cx, |this, cx| {
                    this.show_all_runs = show;
                    this.save_state(cx);
                    cx.notify();
                })
                .ok();
        })
        .icon(icon);
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.show_menu(position, vec![entry], cx)
            })
            .ok();
    }

    fn tabs(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<Div> {
        if self.kube.selected().is_some() {
            return self.kube.tabs(theme, cx);
        }
        if self.selected_run().is_some() {
            let live = self.selected_live(cx).is_some();
            return Some(
                div()
                    .flex()
                    .items_center()
                    .gap(px(SPACE_XS))
                    .flex_none()
                    .h(px(CONTROL_HEIGHT))
                    .px(px(SPACE_M))
                    .child(tab_button(
                        "services-run-tab-console",
                        "Console",
                        self.tab != RightTab::Actuator,
                        theme,
                        cx.listener(|this, _, _, cx| {
                            this.tab = RightTab::RunConsole;
                            cx.notify();
                        }),
                    ))
                    .children(live.then(|| {
                        tab_button(
                            "services-run-tab-actuator",
                            "Actuator",
                            self.tab == RightTab::Actuator,
                            theme,
                            cx.listener(|this, _, _, cx| {
                                this.tab = RightTab::Actuator;
                                cx.notify();
                            }),
                        )
                    })),
            );
        }
        if self.selected_run_group().is_some() {
            return None;
        }
        Some(
            div()
                .flex()
                .items_center()
                .gap(px(SPACE_XS))
                .flex_none()
                .h(px(CONTROL_HEIGHT))
                .px(px(SPACE_M))
                .child(tab_button(
                    "docker-tab-dashboard",
                    "Dashboard",
                    self.tab == RightTab::Dashboard,
                    theme,
                    cx.listener(|this, _, _, cx| {
                        this.tab = RightTab::Dashboard;
                        cx.notify();
                    }),
                ))
                .child(tab_button(
                    "docker-tab-console",
                    "Console",
                    self.tab == RightTab::Console,
                    theme,
                    cx.listener(|this, _, _, cx| {
                        this.tab = RightTab::Console;
                        cx.notify();
                    }),
                )),
        )
    }

    fn body(&self, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> Div {
        if self.kube.selected().is_some() {
            return self.kube.body(theme, window, cx);
        }
        if self.selected_run().is_some() {
            if self.tab == RightTab::Actuator && self.selected_live(cx).is_some() {
                return self.run_live(theme, cx);
            }
            return self.run_console(theme, window, cx);
        }
        if self.selected_run_group().is_some() {
            return empty_state("Select a configuration of the group", theme);
        }
        if self.tab != RightTab::Console
            && let Some(problem) = self.daemon_problem()
        {
            return problem_view(problem, theme);
        }
        match self.tab {
            RightTab::Dashboard | RightTab::Actuator | RightTab::RunConsole => {
                self.dashboard(theme, window, cx)
            }
            RightTab::Console => self.console(theme, window, cx),
        }
    }

    /// The live tab of the run of the selected configuration, when its language has one.
    fn selected_live(&self, cx: &App) -> Option<Entity<crate::live_panel::LivePanel>> {
        let name = self.selected_run()?;
        let runs = self.runs.as_ref();
        let runs = runs?.read(cx);
        (runs.run_config_name() == Some(name))
            .then(|| runs.live_panel())
            .flatten()
    }

    /// The live sections of the same run the Run window shows.
    fn run_live(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        match self.selected_live(cx) {
            Some(live) => div()
                .flex()
                .flex_1()
                .min_h_0()
                .child(live.into_any_element()),
            None => empty_state("No live data for this run", theme),
        }
    }

    /// The console of the same run the Run window shows: the same lines, the same
    /// unfinished line and the same process. Nothing runs for the Services window.
    fn run_console(&self, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let Some(name) = self.selected_run() else {
            return empty_state("Select a configuration", theme);
        };
        let Some(runs) = self.runs.clone() else {
            return empty_state("No run configurations in this window", theme);
        };
        if !runs.read(cx).shows_console(name) {
            return empty_state("This configuration has not run in this window", theme);
        }
        let (count, scroll) = {
            let runs = runs.read(cx);
            (runs.console_rows(), runs.console_scroll())
        };
        let theme = *theme;
        div().flex().flex_1().min_h_0().child(
            scrollable(
                "services-run-console",
                &scroll,
                div().size_full().child(
                    uniform_list(
                        "services-run-console",
                        count,
                        cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                            let Some(runs) = this.runs.clone() else {
                                return Vec::new();
                            };
                            let runs = runs.read(cx);
                            range
                                .map(|index| runs.console_row(index, &theme))
                                .collect::<Vec<_>>()
                        }),
                    )
                    .track_scroll(&scroll)
                    .size_full(),
                ),
                &theme,
                window,
                cx,
            )
            .debug_selector(|| "services-run-console".to_owned())
            .flex_1()
            .min_h_0(),
        )
    }

    fn dashboard(&self, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> Div {
        if self.lines.is_empty() {
            return empty_state(self.empty_text(), theme);
        }
        let theme = *theme;
        let count = self.lines.len() + 1;
        let log = if self.wrap {
            scrollable(
                "docker-log-wrapped",
                &self.scroll_text,
                div()
                    .id("docker-log-wrapped")
                    .flex()
                    .flex_col()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll_text)
                    .font_family(MONO_FONT)
                    .text_size(px(CODE_SIZE))
                    .text_color(theme.code)
                    .children(
                        self.lines
                            .iter()
                            .map(|line| wrapped_line_element(line, &theme)),
                    ),
                &theme,
                window,
                cx,
            )
            .flex_1()
            .min_h_0()
        } else {
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
            .min_h_0()
        };
        div()
            .flex()
            .flex_1()
            .min_h_0()
            .child(log)
            .child(self.log_tools(&theme, cx))
    }

    fn log_tools(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(SPACE_XS))
            .p(px(SPACE_XS))
            .child(
                icon_button(
                    "docker-log-top",
                    "general/arrowUp",
                    "Scroll to Top",
                    None,
                    theme,
                )
                .debug_selector(|| "docker-log-top".to_owned())
                .on_click(cx.listener(|this, _, _, cx| {
                    this.scroll.scroll_to_item(0, ScrollStrategy::Top);
                    this.scroll_text.set_offset(point(px(0.), px(0.)));
                    cx.notify();
                })),
            )
            .child(
                icon_button(
                    "docker-log-bottom",
                    "general/arrowDown",
                    "Scroll to End",
                    None,
                    theme,
                )
                .debug_selector(|| "docker-log-bottom".to_owned())
                .on_click(cx.listener(|this, _, _, cx| {
                    this.scroll.scroll_to_bottom();
                    this.scroll_text.scroll_to_bottom();
                    cx.notify();
                })),
            )
            .child(
                icon_button(
                    "docker-log-wrap",
                    "general/wordWrap",
                    "Wrap Lines",
                    None,
                    theme,
                )
                .debug_selector(|| "docker-log-wrap".to_owned())
                .when(self.wrap, |this| this.bg(theme.selection_inactive))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.wrap = !this.wrap;
                    cx.notify();
                })),
            )
            .child(
                icon_button(
                    "docker-log-clear",
                    "general/clear",
                    "Clear Log",
                    None,
                    theme,
                )
                .debug_selector(|| "docker-log-clear".to_owned())
                .on_click(cx.listener(|this, _, _, cx| {
                    this.lines.clear();
                    cx.notify();
                })),
            )
    }

    fn console(&self, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> Div {
        if self.console_lines.is_empty() {
            return empty_state("No actions yet", theme);
        }
        let theme = *theme;
        div().flex().flex_1().min_h_0().child(
            scrollable(
                "docker-console",
                &self.scroll_console,
                div()
                    .id("docker-console")
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

    /// The area between the tree and the log. The pointer takes it, and the move of the
    /// pointer is followed by the root of the panel, so a fast drag keeps the grip.
    fn splitter(&self, theme: &Theme, cx: &mut Context<Self>) -> Stateful<Div> {
        let hover = theme.hover;
        div()
            .id("docker-split")
            .debug_selector(|| "docker-split".to_owned())
            .flex()
            .justify_center()
            .flex_none()
            .w(px(SPLIT_WIDTH))
            .h_full()
            .cursor_col_resize()
            .occlude()
            .hover(move |style| style.bg(hover))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    this.resizing = Some((f32::from(event.position.x), this.tree_width));
                    cx.notify();
                }),
            )
            .child(div().w(px(1.)).h_full().bg(theme.border))
    }

    fn drag_split(&mut self, event: &MouseMoveEvent, window: &Window, cx: &mut Context<Self>) {
        let Some((from, width)) = self.resizing else {
            return;
        };
        let most = (f32::from(window.viewport_size().width) - MIN_LOG_WIDTH).max(MIN_TREE_WIDTH);
        self.tree_width = (width + f32::from(event.position.x) - from).clamp(MIN_TREE_WIDTH, most);
        cx.notify();
    }

    fn rows(&self, range: std::ops::Range<usize>, theme: &Theme) -> Vec<AnyElement> {
        let current = self.output.current();
        let current = current.cloned().unwrap_or_default();
        range
            .map(|index| {
                let line = self.lines.get(index).unwrap_or(&current);
                line_element(line, theme)
            })
            .collect()
    }

    fn show_container_menu(&mut self, id: String, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(container) = self
            .snapshot
            .containers
            .iter()
            .find(|container| container.id == id)
            .cloned()
        else {
            return;
        };
        let running = container.state == ContainerState::Running;
        let mut entries = vec![
            container_entry(cx.weak_entity(), &container, "start", running),
            container_entry(cx.weak_entity(), &container, "stop", !running),
            container_entry(cx.weak_entity(), &container, "restart", !running),
            remove_entry(cx.weak_entity(), container.clone()),
            MenuEntry::separator(),
        ];
        let panel = cx.weak_entity();
        let target = container.clone();
        entries.push(
            MenuEntry::new("Show Logs", move |_, cx| {
                panel
                    .update(cx, |this, cx| {
                        this.select(Selected::Container(target.id.clone()), cx)
                    })
                    .ok();
            })
            .icon("general/logs"),
        );
        let panel = cx.weak_entity();
        let target = container.clone();
        entries.push(
            MenuEntry::new("Exec", move |_, cx| {
                panel.update(cx, |this, cx| this.exec(&target, cx)).ok();
            })
            .icon("toolwindows/terminal"),
        );
        entries.push(MenuEntry::separator());
        let id = container.id.clone();
        entries.push(
            MenuEntry::new("Copy Container ID", move |_, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(id.clone()))
            })
            .icon("general/copy"),
        );
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.show_menu(position, entries, cx)
            })
            .ok();
    }

    fn show_compose_menu(
        &mut self,
        project: String,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let panel = cx.weak_entity();
        let mut entries = Vec::new();
        for (label, icon, action) in [
            ("Up", "general/arrowUp", ComposeAction::Up),
            ("Down", "general/arrowDown", ComposeAction::Down),
            ("Restart", "run/restart", ComposeAction::Restart),
            ("Stop", "run/stop", ComposeAction::Stop),
        ] {
            let panel = panel.clone();
            let target = project.clone();
            entries.push(
                MenuEntry::new(label, move |_, cx| {
                    panel
                        .update(cx, |this, cx| {
                            this.compose_project_action(target.clone(), action, cx)
                        })
                        .ok();
                })
                .icon(icon),
            );
        }
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.show_menu(position, entries, cx)
            })
            .ok();
    }
}

/// One action of a container in the context menu. A row that does not apply to the
/// state of the container is shown but does nothing.
fn container_entry(
    panel: WeakEntity<DockerPanel>,
    container: &Container,
    action: &'static str,
    disabled: bool,
) -> MenuEntry {
    let label = match action {
        "start" => "Start",
        "stop" => "Stop",
        "restart" => "Restart",
        _ => "Remove",
    };
    let icon = match action {
        "start" => "run/run",
        "stop" => "run/stop",
        "restart" => "run/restart",
        _ => "general/trash",
    };
    if disabled {
        return MenuEntry::disabled(label);
    }
    let target = container.clone();
    MenuEntry::new(label, move |_, cx| {
        panel
            .update(cx, |this, cx| this.container_action(action, &target, cx))
            .ok();
    })
    .icon(icon)
}

/// Remove asks first, as the CLI forces a running container away.
fn remove_entry(panel: WeakEntity<DockerPanel>, container: Container) -> MenuEntry {
    let question = format!("Remove container {}?", container.name);
    MenuEntry::new("Remove", move |window, cx| {
        let ask_panel = panel.clone();
        let target = container.clone();
        panel
            .update(cx, |this, cx| {
                let workspace = this.workspace.clone();
                workspace
                    .update(cx, |workspace, cx| {
                        Prompt::ask(
                            "Remove Container",
                            &question,
                            move |cx| {
                                ask_panel
                                    .update(cx, |this, cx| {
                                        this.container_action("remove", &target, cx)
                                    })
                                    .ok();
                            },
                            workspace,
                            window,
                            cx,
                        );
                    })
                    .ok();
            })
            .ok();
    })
    .icon("general/trash")
}

fn section_row(
    id: &'static str,
    label: String,
    depth: usize,
    icon: &'static str,
    expandable: bool,
    open: bool,
    theme: &Theme,
) -> Stateful<Div> {
    tree_row(
        TreeRow {
            id: id.into(),
            depth,
            label: label.into(),
            hint: None,
            icon,
            expandable,
            open,
            selected: false,
        },
        theme,
    )
}

/// A button of the header: an icon, its name while the window has room for it, and a
/// tooltip in every case. The name never takes the room of a neighbour.
pub(crate) fn action_button(
    action: &'static str,
    icon_name: &'static str,
    label: &'static str,
    enabled: bool,
    show_label: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let selector = format!("docker-action-{action}");
    let (hover, pressed) = (theme.hover, theme.pressed);
    div()
        .id(selector.clone())
        .debug_selector(move || selector.clone())
        .flex()
        .items_center()
        .justify_center()
        .gap(px(SPACE_XS))
        .flex_none()
        .h(px(CONTROL_HEIGHT))
        .px(px(SPACE_S))
        .rounded(px(CONTROL_RADIUS))
        .occlude()
        .when(enabled, |this| {
            this.hover(move |style| style.bg(hover))
                .active(move |style| style.bg(pressed))
        })
        .when(!enabled, |this| this.text_color(theme.text_secondary))
        .tooltip(move |_, cx| tooltip(label, None, cx))
        .tooltip_show_delay(TOOLTIP_CREATE_DELAY)
        .child(icon(icon_name, theme))
        .children(show_label.then(|| div().flex_none().child(label)))
}

pub(crate) fn tab_button(
    id: &'static str,
    label: &'static str,
    active: bool,
    theme: &Theme,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    div()
        .id(id)
        .debug_selector(move || id.to_owned())
        .flex()
        .items_center()
        .flex_none()
        .h(px(24.))
        .px(px(SPACE_S))
        .rounded(px(CONTROL_RADIUS))
        .text_color(if active {
            theme.text
        } else {
            theme.text_secondary
        })
        .when(active, |this| this.bg(theme.selection_inactive))
        .when(!active, |this| {
            let hover = theme.hover;
            this.hover(move |style| style.bg(hover))
        })
        .on_click(on_click)
        .child(label)
}

pub(crate) fn empty_state(text: &'static str, theme: &Theme) -> Div {
    div()
        .flex()
        .flex_1()
        .items_center()
        .justify_center()
        .text_color(theme.text_secondary)
        .child(text)
}

/// The reason of a Docker daemon or of a Kubernetes cluster that does not answer. It is a
/// column of its own that fills the pane, so a reason of any length wraps inside the pane.
pub(crate) fn problem_view(text: impl Into<SharedString>, theme: &Theme) -> Div {
    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .p(px(SPACE_M))
        .text_color(theme.text_secondary)
        .debug_selector(|| "services-problem".to_owned())
        .child(text.into())
}

/// Puts the name of the container in front of a line of the log of a compose project.
fn prefix_line(line: &mut Line, label: &str, color: u8) {
    let prefix = format!("{label}  ");
    let width = prefix.len();
    for (range, _) in &mut line.spans {
        range.start += width;
        range.end += width;
    }
    line.spans.insert(
        0,
        (
            0..width,
            Style {
                color: Some(color),
                bold: true,
                ..Style::default()
            },
        ),
    );
    line.text.insert_str(0, &prefix);
}

impl Render for DockerPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let workspace = self.workspace.clone();
        let options =
            crate::tool_menu::options_menu(workspace.clone(), workspace::SERVICES_TOOL, |_| {
                Vec::new()
            });
        let hide = move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
            workspace
                .update(cx, |workspace, cx| {
                    workspace.toggle_tool(SERVICES_TOOL, window, cx)
                })
                .ok();
        };
        let viewport_width = f32::from(window.viewport_size().width);
        let tree_width = self
            .tree_width
            .min((viewport_width - MIN_LOG_WIDTH).max(MIN_TREE_WIDTH));
        let show_labels = viewport_width - tree_width >= LABEL_MIN_WIDTH;
        let rows = self.tree(&theme, cx);
        let toolbar = self.tree_toolbar(&theme, cx);
        let header = self.header(&theme, show_labels, cx);
        let tabs = self.tabs(&theme, cx);
        let body = self.body(&theme, window, cx);
        let splitter = self.splitter(&theme, cx);
        let tree = div()
            .id("docker-tree-pane")
            .debug_selector(|| "docker-tree-pane".to_owned())
            .flex()
            .flex_col()
            .flex_none()
            .w(px(tree_width))
            .h_full()
            .child(toolbar)
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
        let right = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .children(header)
            .children(tabs)
            .child(body);
        island(&theme)
            .id("docker-panel")
            .key_context("ServicesPanel ToolWindow")
            .track_focus(&self.focus)
            .group(GROUP)
            .h_full()
            .child(
                panel_header("Services", GROUP, "services", options, hide, &theme)
                    .debug_selector(|| "services-header".to_owned()),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, window, cx| {
                        this.drag_split(event, window, cx)
                    }))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            this.resizing = None;
                            cx.notify();
                        }),
                    )
                    .on_mouse_up_out(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            this.resizing = None;
                            cx.notify();
                        }),
                    )
                    .child(tree)
                    .child(splitter)
                    .child(right),
            )
    }
}

#[cfg(test)]
mod tests {
    use container::{Container, ContainerState};

    use super::{Selected, counter, group_containers, prefix_line};
    use crate::ansi::{Line, Style};

    fn container(name: &str, state: ContainerState, project: Option<&str>) -> Container {
        Container {
            id: name.to_owned(),
            name: name.to_owned(),
            image: "image:latest".to_owned(),
            state,
            status: String::new(),
            ports: String::new(),
            compose_project: project.map(str::to_owned),
            compose_dir: None,
        }
    }

    #[test]
    fn containers_are_grouped_by_their_compose_project() {
        let containers = vec![
            container("web", ContainerState::Running, Some("shop")),
            container("solo", ContainerState::Running, None),
            container("db", ContainerState::Exited, Some("shop")),
            container("cache", ContainerState::Running, Some("blog")),
        ];

        let (groups, loose) = group_containers(&containers);

        assert_eq!(
            groups,
            vec![
                ("blog".to_owned(), vec![3]),
                ("shop".to_owned(), vec![0, 2]),
            ]
        );
        assert_eq!(loose, vec![1]);
        assert_eq!(counter(&containers, &groups[1].1), "Running: (1/2)");
    }

    #[test]
    fn a_name_goes_before_every_line_of_a_mixed_log() {
        let mut line = Line::plain("ready", Style::default());

        prefix_line(&mut line, "petclinic-web-1", 2);

        assert_eq!(line.text, "petclinic-web-1  ready");
        assert_eq!(line.spans[0].0, 0..17);
        assert_eq!(line.spans[0].1.color, Some(2));
        assert_eq!(line.spans[1].0, 17..22);
    }

    #[test]
    fn the_selected_node_of_the_tree_survives_the_layout_as_a_key() {
        let nodes = [
            Selected::Container("web0001".to_owned()),
            Selected::Compose("petclinic".to_owned()),
            Selected::Image("sha256:aa".to_owned()),
            Selected::Run("App".to_owned()),
            Selected::RunGroup("boot".to_owned()),
        ];

        for node in nodes {
            assert_eq!(
                Selected::from_key(&node.key()),
                Some(node.clone()),
                "{node:?}"
            );
        }
        assert_eq!(Selected::from_key("unknown:node"), None);
        assert_eq!(Selected::from_key("run"), None);
    }
}
