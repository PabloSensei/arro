//! The AI sessions tool window: starts a harness in the project, switches between sessions,
//! draws the terminal of the active one and lists the files the sessions changed.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use agent_core::{
    AgentEvent, AgentRuntime, AgentSession, Harness, Installed, SessionSpec, SessionStatus,
    Surface, ToolEndpoint, detect,
};
use agent_pty::{Launch, PtyRuntime, input_box_visible};
use change_tracker::{ChangeKind, ChangeSet, ChangeTracker, GitRunner, TrackerState};
use crossbeam_channel::Receiver;
use mcp_server::McpServer;
use project_model::EnvironmentProvider;
use store::TrustList;
use terminal::TerminalSession;
use tool_gateway::{AuditLog, LanguageTools, SessionScope, ToolBackend, ToolGateway};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::{ToggleAi, Workspace};

use crate::term_grid::{every_frame, grid, scroll_lines, send_key};
use crate::terminal_panel::message;
use crate::trust::TrustDialog;

const GROUP: &str = "right-panel";
const FIRST_SIZE: (u16, u16) = (80, 24);
const SHOWN_CHANGES: usize = 12;
/// The terminal of the session keeps the rest of the panel.
const CHANGES_HEIGHT: f32 = 200.;

/// How harnesses run in this window.
#[derive(Clone, Debug, Default)]
pub struct AgentOptions {
    pub state_dir: PathBuf,
    /// An empty home and a local model API instead of the user's own, for tests.
    pub mock: bool,
}

/// The panel of the window, for scripts.
pub struct ActiveAgents(pub Entity<AgentsPanel>);

/// Where a language registers once its server is up, so that agents can ask it through MCP:
/// `cx.global::<AgentLanguages>().0.register(support)`.
pub struct AgentLanguages(pub Arc<LanguageTools>);

impl Global for AgentLanguages {}

impl Global for ActiveAgents {}

/// Shared by every session of the window, made with the first one.
struct Services {
    gateway: Arc<ToolGateway>,
    server: McpServer,
    tracker: Result<Arc<ChangeTracker>, String>,
}

struct Entry {
    id: usize,
    harness: Harness,
    session: Arc<dyn AgentSession>,
    terminal: Option<Arc<TerminalSession>>,
    events: Receiver<AgentEvent>,
    status: SessionStatus,
    token: String,
    services: Arc<Services>,
    /// What a mock run needs to live as long as the session.
    _mock: Option<Box<dyn Send>>,
}

impl Entry {
    fn title(&self) -> String {
        format!("{} {}", self.harness.title(), self.id)
    }
}

impl Drop for Entry {
    fn drop(&mut self) {
        self.session.close();
        self.services.server.revoke(&self.token);
    }
}

pub struct AgentsPanel {
    root: PathBuf,
    options: AgentOptions,
    environment: Option<Arc<dyn EnvironmentProvider>>,
    installs: Vec<Installed>,
    services: Option<Arc<Services>>,
    languages: Arc<LanguageTools>,
    entries: Vec<Entry>,
    active: Option<usize>,
    next_id: usize,
    starting: Option<Harness>,
    error: Option<SharedString>,
    changes: Arc<ChangeSet>,
    trust: TrustList,
    focus: FocusHandle,
    workspace: WeakEntity<Workspace>,
    version: u64,
}

/// A short stable name for the state of one project.
fn project_key(root: &Path) -> String {
    let text = root.to_string_lossy().to_lowercase();
    let hash = text.bytes().fold(0xcbf29ce484222325u64, |hash, byte| {
        (hash ^ byte as u64).wrapping_mul(0x100000001b3)
    });
    format!("{hash:016x}")
}

fn open_services(
    root: &Path,
    options: &AgentOptions,
    languages: Arc<LanguageTools>,
) -> Result<Services, String> {
    let backend: Arc<dyn ToolBackend> = if options.mock {
        mock::tools()?
    } else {
        languages
    };
    let gateway = Arc::new(ToolGateway::new(backend, AuditLog::default()));
    let server = McpServer::start(gateway.clone()).map_err(|error| error.to_string())?;
    let path = std::env::var_os("PATH").unwrap_or_default();
    let state = options.state_dir.join("changes").join(project_key(root));
    let tracker = match GitRunner::find(&path) {
        Some(git) => ChangeTracker::open(git, &state, root)
            .map(Arc::new)
            .map_err(|error| error.to_string()),
        None => Err("git is not on PATH, changes are not tracked".to_string()),
    };
    Ok(Services {
        gateway,
        server,
        tracker,
    })
}

/// Runs off the UI thread: starting a server and a process may take a moment.
fn launch(
    installed: Installed,
    services: Option<Arc<Services>>,
    languages: Arc<LanguageTools>,
    root: PathBuf,
    options: AgentOptions,
    environment: Option<Arc<dyn EnvironmentProvider>>,
    id: usize,
) -> Result<Entry, String> {
    let services = match services {
        Some(services) => services,
        None => Arc::new(open_services(&root, &options, languages)?),
    };
    let harness = installed.harness;
    let (launch, mock) = if options.mock {
        let (launch, home) = mock::launch(harness, &root, id)?;
        (launch, Some(home))
    } else {
        let variables = environment
            .map(|environment| environment.describe().variables)
            .unwrap_or_default();
        let launch = Launch::UserHome {
            inherit: Vec::new(),
            set: variables,
        };
        (launch, None)
    };
    let token = services
        .server
        .issue_token(SessionScope {
            session: format!("{}-{id}", harness.stem()),
            project: root.clone(),
            cwd: root.clone(),
        })
        .map_err(|error| error.to_string())?;
    let (cols, rows) = FIRST_SIZE;
    let spec = SessionSpec {
        project: root.clone(),
        cwd: root,
        cols,
        rows,
        tools: Some(ToolEndpoint {
            url: services.server.url(),
            token: token.clone(),
        }),
    };
    let mut runtime = PtyRuntime::new(installed, launch);
    if options.mock && harness == Harness::Claude {
        // The mock asks for edits, and a permission screen would stop the run.
        runtime = runtime.with_arguments(
            ["--permission-mode", "acceptEdits"]
                .map(String::from)
                .to_vec(),
        );
    }
    let started = runtime.start(spec);
    let session: Arc<dyn AgentSession> = match started {
        Ok(session) => Arc::from(session),
        Err(error) => {
            services.server.revoke(&token);
            return Err(error.to_string());
        }
    };
    let terminal = match session.surface() {
        Surface::Terminal(terminal) => Some(terminal),
        Surface::Transcript => None,
    };
    Ok(Entry {
        id,
        harness,
        events: session.events(),
        session,
        terminal,
        status: SessionStatus::Starting,
        token,
        services,
        _mock: mock,
    })
}

impl AgentsPanel {
    pub fn create(
        workspace: &Workspace,
        weak: WeakEntity<Workspace>,
        options: AgentOptions,
        environment: Option<Arc<dyn EnvironmentProvider>>,
        cx: &mut App,
    ) -> Entity<Self> {
        let root = workspace.root().to_path_buf();
        let path = std::env::var_os("PATH").unwrap_or_default();
        let installs = detect(&path);
        let languages = cx.global::<AgentLanguages>().0.clone();
        cx.new(|cx| {
            every_frame(cx, Self::refresh);
            Self {
                root,
                environment,
                trust: TrustList::new(&options.state_dir),
                options,
                installs,
                services: None,
                languages,
                entries: Vec::new(),
                active: None,
                next_id: 1,
                starting: None,
                error: None,
                changes: Arc::new(ChangeSet::default()),
                focus: cx.focus_handle(),
                workspace: weak,
                version: 0,
            }
        })
    }

    pub fn focus(&self) -> FocusHandle {
        self.focus.clone()
    }

    /// Starts a session of `harness`, asking first when the project is not trusted yet.
    pub fn request(&mut self, harness: Harness, window: &mut Window, cx: &mut Context<Self>) {
        if self.trust.is_trusted(&self.root) {
            self.start(harness, window, cx);
            return;
        }
        let panel = cx.weak_entity();
        let on_trust = move |window: &mut Window, cx: &mut App| {
            panel
                .update(cx, |panel, cx| panel.start(harness, window, cx))
                .ok();
        };
        let (root, store) = (self.root.clone(), self.trust.clone());
        self.workspace
            .update(cx, |workspace, cx| {
                TrustDialog::show(root, store, workspace, on_trust, window, cx)
            })
            .ok();
    }

    fn start(&mut self, harness: Harness, window: &mut Window, cx: &mut Context<Self>) {
        if self.starting.is_some() {
            return;
        }
        let installed = self.installs.iter().find(|it| it.harness == harness);
        let Some(installed) = installed.cloned() else {
            self.error = Some(format!("{} is not on PATH", harness.stem()).into());
            cx.notify();
            return;
        };
        self.starting = Some(harness);
        self.error = None;
        let id = self.next_id;
        self.next_id += 1;
        let (services, root) = (self.services.clone(), self.root.clone());
        let languages = self.languages.clone();
        let options = self.options.clone();
        let environment = self.environment.clone();
        let task = cx.background_executor().spawn(async move {
            launch(
                installed,
                services,
                languages,
                root,
                options,
                environment,
                id,
            )
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, window, cx| this.started(result, window, cx))
                .ok();
        })
        .detach();
        cx.notify();
    }

    fn started(
        &mut self,
        result: Result<Entry, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.starting = None;
        match result {
            Ok(entry) => {
                self.services = Some(entry.services.clone());
                self.entries.push(entry);
                self.active = Some(self.entries.len() - 1);
                window.focus(&self.focus, cx);
            }
            Err(error) => self.error = Some(format!("The session did not start: {error}").into()),
        }
        cx.notify();
    }

    fn select(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.active = Some(index);
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn close(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.entries.len() {
            self.entries.remove(index);
        }
        self.active = match self.entries.len() {
            0 => None,
            count => Some(self.active.unwrap_or(0).min(count - 1)),
        };
        cx.notify();
    }

    /// The active session shows its input box, the only screen a script may type into.
    pub fn input_ready(&self) -> bool {
        let Some(entry) = self.active.and_then(|index| self.entries.get(index)) else {
            return false;
        };
        let Some(terminal) = &entry.terminal else {
            return false;
        };
        let lines = terminal.screen().lines();
        input_box_visible(entry.harness, &lines, false, None)
    }

    fn active_terminal(&self) -> Option<&Arc<TerminalSession>> {
        let entry = self.entries.get(self.active?)?;
        entry.terminal.as_ref()
    }

    fn tracker(&self) -> Option<&Arc<ChangeTracker>> {
        self.services.as_ref()?.tracker.as_ref().ok()
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let mut changed = false;
        let tracker = self.tracker().cloned();
        for entry in &mut self.entries {
            for event in entry.events.try_iter() {
                if let Some(tracker) = &tracker {
                    tracker.observe(&event);
                }
                match event {
                    AgentEvent::Status(status) => entry.status = status,
                    AgentEvent::Exited(Some(0)) => entry.status = SessionStatus::Done,
                    AgentEvent::Exited(_) => entry.status = SessionStatus::Error,
                    _ => continue,
                }
                changed = true;
            }
        }
        if let Some(tracker) = &tracker {
            let changes = tracker.changes();
            if changes.version != self.changes.version || changes.state != self.changes.state {
                self.changes = changes;
                changed = true;
            }
        }
        if let Some(terminal) = self.active_terminal() {
            let version = terminal.screen().version;
            if version != self.version {
                self.version = version;
                changed = true;
            }
        }
        if changed {
            cx.notify();
        }
    }

    fn key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let Some(terminal) = self.active_terminal().cloned() else {
            return;
        };
        if !terminal.is_closed() && send_key(&terminal, &event.keystroke, cx) {
            cx.stop_propagation();
        }
    }

    fn open_change(&self, path: &str, window: &mut Window, cx: &mut Context<Self>) {
        let file = self.root.join(path);
        self.workspace
            .update(cx, |workspace, cx| workspace.open_file(file, window, cx))
            .ok();
    }

    /// A text picture of the panel, for scripts.
    pub fn report(&self) -> Vec<String> {
        let mut lines = Vec::new();
        for entry in &self.entries {
            lines.push(format!(
                "agent {} {} {:?}",
                entry.id,
                entry.harness.stem(),
                entry.status
            ));
        }
        if let Some(error) = &self.error {
            lines.push(format!("agent-error {error}"));
        }
        if let Some(terminal) = self.active_terminal() {
            let screen = terminal.screen();
            let shown = screen.lines().into_iter().filter(|line| !line.is_empty());
            lines.extend(shown.map(|line| format!("agent-screen {line}")));
        }
        for change in &self.changes.session {
            lines.push(format!("changed {}", change.path));
        }
        if let Some(services) = &self.services {
            match &services.tracker {
                Ok(_) => lines.push(format!("changes {:?}", self.changes.state)),
                Err(error) => lines.push(format!("changes {error}")),
            }
            for call in services.gateway.audit().recent() {
                lines.push(format!("tool-call {} ok={}", call.tool, call.ok));
            }
        }
        lines
    }

    fn status_text(status: SessionStatus, theme: &Theme) -> (&'static str, Hsla) {
        match status {
            SessionStatus::Starting => ("Starting", theme.text_secondary),
            SessionStatus::Running => ("Working", theme.accent),
            SessionStatus::WaitingForUser => ("Waiting for you", theme.warning_mark),
            SessionStatus::Done => ("Finished", theme.text_secondary),
            SessionStatus::Error => ("Ended with an error", theme.close_hover),
        }
    }

    fn start_buttons(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let mut row = div()
            .flex()
            .flex_wrap()
            .flex_none()
            .gap(px(SPACE_S))
            .px(px(SPACE_M))
            .pb(px(SPACE_M));
        if self.installs.is_empty() {
            return row.child(
                div()
                    .text_color(theme.text_secondary)
                    .child("No harness on PATH: install claude or codex"),
            );
        }
        for install in &self.installs {
            let harness = install.harness;
            let id = SharedString::from(format!("start-{}", harness.stem()));
            let (hover, pressed) = (theme.hover, theme.pressed);
            let busy = self.starting == Some(harness);
            let label = if busy {
                format!("Starting {}", harness.title())
            } else {
                harness.title().to_string()
            };
            row = row.child(
                div()
                    .id(id)
                    .flex()
                    .items_center()
                    .gap(px(SPACE_XS))
                    .h(px(CONTROL_HEIGHT))
                    .pl(px(SPACE_S))
                    .pr(px(SPACE_M))
                    .rounded(px(CONTROL_RADIUS))
                    .border_1()
                    .border_color(theme.control_border)
                    .hover(move |style| style.bg(hover))
                    .active(move |style| style.bg(pressed))
                    .child(icon("general/add", theme))
                    .child(label)
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.request(harness, window, cx)),
                    ),
            );
        }
        row
    }

    fn session_rows(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let mut list = div()
            .flex()
            .flex_col()
            .flex_none()
            .px(px(SPACE_M))
            .pb(px(SPACE_M));
        for (index, entry) in self.entries.iter().enumerate() {
            let selected = self.active == Some(index);
            let (status, color) = Self::status_text(entry.status, theme);
            let (hover, chosen) = (theme.selection_hover, theme.selection_inactive);
            let row = div()
                .id(("agent-row", entry.id))
                .group("agent-row")
                .flex()
                .items_center()
                .gap(px(SPACE_M))
                .h(px(ROW_HEIGHT))
                .px(px(ROW_PADDING_X))
                .rounded(px(ROW_RADIUS))
                .when(selected, |this| this.bg(chosen))
                .when(!selected, |this| this.hover(move |style| style.bg(hover)))
                .on_click(cx.listener(move |this, _, window, cx| this.select(index, window, cx)))
                .child(div().size(px(7.)).rounded_full().bg(color).flex_none())
                .child(div().flex_1().child(entry.title()))
                .child(div().text_color(theme.text_secondary).child(status))
                .child(
                    icon_button(
                        ("agent-close", entry.id),
                        "general/closeSmall",
                        "Close Session",
                        None,
                        theme,
                    )
                    .size(px(20.))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.close(index, cx)
                    })),
                );
            list = list.child(row);
        }
        list
    }

    fn change_rows(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<Div> {
        let services = self.services.as_ref()?;
        let header = match (&services.tracker, &self.changes.state) {
            (Err(error), _) => error.clone(),
            (Ok(_), TrackerState::Preparing) => "Taking the first snapshot".to_string(),
            (Ok(_), TrackerState::Failed(error)) => format!("Changes are not tracked: {error}"),
            (Ok(_), TrackerState::Ready) => {
                format!("Changed files: {}", self.changes.session.len())
            }
        };
        let mut list = div()
            .flex()
            .flex_col()
            .flex_none()
            .border_t_1()
            .border_color(theme.border)
            .px(px(SPACE_M))
            .py(px(SPACE_M))
            .max_h(px(CHANGES_HEIGHT))
            .overflow_hidden()
            .child(
                div()
                    .px(px(SPACE_M))
                    .pb(px(SPACE_S))
                    .text_color(theme.text_secondary)
                    .truncate()
                    .child(header),
            );
        let hover = theme.selection_hover;
        for (index, change) in self.changes.session.iter().take(SHOWN_CHANGES).enumerate() {
            let mark = match change.kind {
                ChangeKind::Added => "A",
                ChangeKind::Modified => "M",
                ChangeKind::Deleted => "D",
            };
            let path = change.path.clone();
            list = list.child(
                div()
                    .id(("agent-change", index))
                    .flex()
                    .gap(px(SPACE_M))
                    .h(px(ROW_HEIGHT))
                    .items_center()
                    .px(px(ROW_PADDING_X))
                    .rounded(px(ROW_RADIUS))
                    .hover(move |style| style.bg(hover))
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.open_change(&path, window, cx)),
                    )
                    .child(
                        div()
                            .w(px(10.))
                            .text_color(theme.text_secondary)
                            .child(mark),
                    )
                    .child(change.path.clone()),
            );
        }
        Some(list)
    }
}

impl Render for AgentsPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let hide = |_: &ClickEvent, window: &mut Window, cx: &mut App| {
            window.dispatch_action(Box::new(ToggleAi), cx);
        };
        let focused = self.focus.is_focused(window);
        let body = div()
            .id("agent-terminal")
            .key_context("AgentTerminal")
            .track_focus(&self.focus)
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .border_t_1()
            .border_color(theme.border)
            .on_key_down(cx.listener(|this, event, _, cx| this.key(event, cx)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| window.focus(&this.focus, cx)),
            );
        let body = match self.active_terminal() {
            Some(terminal) => {
                let scrolled = terminal.clone();
                body.on_scroll(move |event, _, _| scrolled.scroll(scroll_lines(event)))
                    .child(grid(terminal.clone(), focused, theme))
            }
            None if self.entries.is_empty() => body.child(message("No sessions yet", &theme)),
            None => body.child(message("This session has no terminal", &theme)),
        };
        let error = self.error.clone().map(|error| {
            div()
                .px(px(SPACE_XL))
                .pb(px(SPACE_M))
                .text_color(theme.close_hover)
                .child(error)
        });
        let changes = self.change_rows(&theme, cx);
        island(&theme)
            .group(GROUP)
            .h_full()
            .text_size(px(UI_SIZE))
            .child(panel_header("AI Sessions", GROUP, hide, &theme))
            .child(self.start_buttons(&theme, cx))
            .children(error)
            .child(self.session_rows(&theme, cx))
            .child(body)
            .children(changes)
    }
}

#[cfg(feature = "mock-agents")]
mod mock {
    use super::*;
    use agent_testkit::{FakeTools, MOCK_MODEL, MockApi, Sandbox, claude_env, codex_env};

    struct Home {
        _sandbox: Sandbox,
        _api: MockApi,
    }

    pub fn tools() -> Result<Arc<dyn ToolBackend>, String> {
        Ok(Arc::new(FakeTools))
    }

    pub fn launch(
        harness: Harness,
        root: &Path,
        id: usize,
    ) -> Result<(Launch, Box<dyn Send>), String> {
        let error = |error: std::io::Error| error.to_string();
        let name = format!("ide-{}{id}", harness.stem());
        let sandbox = Sandbox::for_project(&name, root).map_err(error)?;
        let api = MockApi::start().map_err(error)?;
        let launch = match harness {
            Harness::Claude => Launch::Prepared {
                env: claude_env(&sandbox, &api).map_err(error)?,
                marker: None,
            },
            Harness::Codex => Launch::Prepared {
                env: codex_env(&sandbox, &api).map_err(error)?,
                marker: Some(MOCK_MODEL.to_string()),
            },
            Harness::OpenCode => return Err("the mock has no setup for opencode".to_string()),
        };
        let home = Home {
            _sandbox: sandbox,
            _api: api,
        };
        Ok((launch, Box::new(home)))
    }
}

#[cfg(not(feature = "mock-agents"))]
mod mock {
    use super::*;

    const MISSING: &str = "this build has no mock agents";

    pub fn tools() -> Result<Arc<dyn ToolBackend>, String> {
        Err(MISSING.to_string())
    }

    pub fn launch(_: Harness, _: &Path, _: usize) -> Result<(Launch, Box<dyn Send>), String> {
        Err(MISSING.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::project_key;
    use std::path::Path;

    #[test]
    fn a_project_key_ignores_the_case_of_the_path() {
        let key = project_key(Path::new(r"C:\Work\Demo"));
        assert_eq!(key, project_key(Path::new(r"c:\work\demo")));
        assert_ne!(key, project_key(Path::new(r"C:\Work\Other")));
        assert_eq!(key.len(), 16);
    }
}
