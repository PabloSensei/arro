//! The AI sessions tool window: starts an agent in the project, switches between sessions, and
//! shows the active one as a terminal or a chat, next to a review of the files it changed.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use agent_acp::{AcpRuntime, Preset};
use agent_core::{
    AgentEvent, AgentRuntime, AgentSession, Harness, Installed, MockRuntime, MockStep, SessionSpec,
    SessionStatus, Surface, ToolEndpoint, detect,
};
use agent_pty::{Launch, PtyRuntime, input_box_visible};
use change_tracker::{ChangeSet, ChangeTracker, GitRunner};
use crossbeam_channel::Receiver;
use editor_view::{EditorView, FileBar, GutterKind, GutterMark};
use mcp_server::McpServer;
use project_model::EnvironmentProvider;
use store::TrustList;
use terminal::TerminalSession;
use tool_gateway::{AuditLog, LanguageTools, SessionScope, ToolBackend, ToolGateway};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::{AI_TOOL, ToggleAi, Workspace};

use crate::ai_review::{Action, Review};
use crate::chat::text_button;
use crate::composer::{Composer, Selection};
use crate::outline::OpenEditors;
use crate::term_grid::{every_frame, grid, scroll_lines, send_key};
use crate::terminal_panel::message;
use crate::transcript::Transcript;
use crate::trust::TrustDialog;

pub(crate) const GROUP: &str = "right-panel";
const FIRST_SIZE: (u16, u16) = (80, 24);

/// The command of the editor menu and of Find Action: asks the agent about the selection.
pub const ASK_AI: &str = "ai.ask_selection";

/// What a mock run keeps alive: its home and its model API.
type Kept = Box<dyn Send>;
/// Variables of a prepared environment.
type Env = Vec<(String, String)>;

/// How harnesses run in this window.
#[derive(Clone, Debug, Default)]
pub struct AgentOptions {
    pub state_dir: PathBuf,
    /// An empty home and a local model API instead of the user's own, for tests.
    pub mock: bool,
}

/// The panel of the window, for scripts.
pub struct ActiveAgents(pub Entity<AgentsPanel>);

impl Global for ActiveAgents {}

/// Where a language registers once its server is up, so that agents can ask it through MCP:
/// `cx.global::<AgentLanguages>().0.register(support)`.
pub struct AgentLanguages(pub Arc<LanguageTools>);

impl Global for AgentLanguages {}

/// What a session runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentKind {
    /// The native TUI of a harness in our terminal.
    Tui(Harness),
    /// An agent over ACP, drawn as a chat.
    Acp(Preset),
    /// A scripted agent, for trying the chat without a model.
    Mock,
}

impl AgentKind {
    pub fn title(&self) -> String {
        match self {
            Self::Tui(harness) => harness.title().to_string(),
            Self::Acp(preset) => preset.title().to_string(),
            Self::Mock => "Mock agent".to_string(),
        }
    }

    pub fn stem(&self) -> &'static str {
        match self {
            Self::Tui(harness) => harness.stem(),
            Self::Acp(Preset::Codex) => "acp-codex",
            Self::Acp(Preset::ClaudeAgent) => "acp-claude",
            Self::Acp(Preset::Custom(_)) => "acp-custom",
            Self::Mock => "mock",
        }
    }

    /// The names scripts use: the stems above.
    pub fn parse(text: &str) -> Option<Self> {
        let tui = Harness::ALL.iter().find(|harness| harness.stem() == text);
        if let Some(harness) = tui {
            return Some(Self::Tui(*harness));
        }
        Some(match text {
            "acp-codex" => Self::Acp(Preset::Codex),
            "acp-claude" => Self::Acp(Preset::ClaudeAgent),
            "mock" => Self::Mock,
            _ => return None,
        })
    }
}

/// Shared by every session of the window, made with the first one.
pub(crate) struct Services {
    gateway: Arc<ToolGateway>,
    server: McpServer,
    pub tracker: Result<Arc<ChangeTracker>, String>,
}

pub(crate) struct Entry {
    pub id: usize,
    pub kind: AgentKind,
    pub session: Arc<dyn AgentSession>,
    pub terminal: Option<Arc<TerminalSession>>,
    events: Receiver<AgentEvent>,
    pub status: SessionStatus,
    pub transcript: Transcript,
    /// Messages sent and turns started, so the difference is the queue.
    pub sent: usize,
    started: usize,
    token: String,
    services: Arc<Services>,
    /// What a mock run needs to live as long as the session.
    _mock: Option<Kept>,
}

impl Entry {
    fn title(&self) -> String {
        match &self.transcript.title {
            Some(title) => format!("{} {}: {title}", self.kind.title(), self.id),
            None => format!("{} {}", self.kind.title(), self.id),
        }
    }

    pub fn queued(&self) -> usize {
        self.sent.saturating_sub(self.started)
    }
}

impl Drop for Entry {
    fn drop(&mut self) {
        self.session.close();
        self.services.server.revoke(&self.token);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tab {
    Session,
    Changes,
}

/// The unaccepted hunks of one file an agent changed, for the bar above its editor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PendingFile {
    /// Relative to the project, with forward slashes.
    pub path: String,
    pub hunks: usize,
}

pub struct AgentsPanel {
    pub(crate) root: PathBuf,
    options: AgentOptions,
    environment: Option<Arc<dyn EnvironmentProvider>>,
    installs: Vec<Installed>,
    pub(crate) services: Option<Arc<Services>>,
    languages: Arc<LanguageTools>,
    pub(crate) entries: Vec<Entry>,
    pub(crate) active: Option<usize>,
    next_id: usize,
    starting: Option<AgentKind>,
    pub(crate) error: Option<SharedString>,
    pub(crate) changes: Arc<ChangeSet>,
    /// The files with unaccepted agent hunks, by absolute path, for the bar above an
    /// editor and for the marks of its gutter.
    pub(crate) pending: HashMap<PathBuf, PendingFile>,
    /// Numbers the reads of the hunks, so an older answer does not replace a newer one.
    marks_generation: u64,
    trust: TrustList,
    pub(crate) focus: FocusHandle,
    pub(crate) workspace: WeakEntity<Workspace>,
    version: u64,
    pub(crate) tab: Tab,
    pub(crate) composer: Composer,
    pub(crate) files: Entity<Arc<Vec<PathBuf>>>,
    pub(crate) expanded: HashSet<String>,
    pub(crate) chat_scroll: ScrollHandle,
    pub(crate) review_scroll: ScrollHandle,
    pub(crate) review: Review,
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

/// A scripted chat that exercises every kind of item, for trying the panel without a model.
fn mock_turns() -> Vec<Vec<MockStep>> {
    let first = vec![
        MockStep::Reply("Let me look for it first.".to_string()),
        MockStep::Tool {
            name: "find_symbol".to_string(),
            input: serde_json::json!({"query": "Main"}),
        },
        MockStep::Permission {
            tool: "Edit".to_string(),
            description: "edit src/Main.txt".to_string(),
        },
        MockStep::Reply("Done, the edit is in place.".to_string()),
    ];
    vec![first]
}

/// The runtime for `kind`, and what a mock run keeps alive.
fn runtime_for(
    kind: &AgentKind,
    installed: Option<Installed>,
    root: &Path,
    options: &AgentOptions,
    environment: Option<&Arc<dyn EnvironmentProvider>>,
    gateway: &Arc<ToolGateway>,
    id: usize,
) -> Result<(Box<dyn AgentRuntime>, Option<Kept>), String> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    match kind {
        AgentKind::Tui(harness) => {
            let installed = installed.ok_or(format!("{} is not on PATH", harness.stem()))?;
            let (launch, home) = if options.mock {
                let (launch, home) = mock::launch(*harness, root, id)?;
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
            let mut runtime = PtyRuntime::new(installed, launch);
            if options.mock && *harness == Harness::Claude {
                // The mock asks for edits, and a permission screen would stop the run.
                let arguments = ["--permission-mode", "acceptEdits"].map(String::from);
                runtime = runtime.with_arguments(arguments.to_vec());
            }
            Ok((Box::new(runtime), home))
        }
        AgentKind::Acp(preset) => {
            let mut command = preset.command(&path)?;
            let mut home = None;
            if options.mock {
                let (env, kept) = mock::acp_env(preset, root, id)?;
                command = command.with_env(env);
                home = Some(kept);
            }
            let runtime = AcpRuntime::new(preset.title(), command);
            Ok((Box::new(runtime), home))
        }
        AgentKind::Mock => {
            let tools = gateway.clone();
            let scope = SessionScope {
                session: format!("mock-{id}"),
                project: root.to_path_buf(),
                cwd: root.to_path_buf(),
            };
            let invoke: agent_core::ToolInvoker = Arc::new(move |name, input| {
                let result = tools.call(&scope, name, input);
                result.map_err(|error| error.to_string())
            });
            let runtime = MockRuntime::new(mock_turns()).with_tools(invoke);
            Ok((Box::new(runtime), None))
        }
    }
}

/// Runs off the UI thread: starting a server and a process may take a moment.
#[allow(clippy::too_many_arguments)]
fn launch(
    kind: AgentKind,
    installed: Option<Installed>,
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
    let (runtime, mock) = runtime_for(
        &kind,
        installed,
        &root,
        &options,
        environment.as_ref(),
        &services.gateway,
        id,
    )?;
    let token = services
        .server
        .issue_token(SessionScope {
            session: format!("{}-{id}", kind.stem()),
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
    let session: Arc<dyn AgentSession> = match runtime.start(spec) {
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
        kind,
        events: session.events(),
        session,
        terminal,
        status: SessionStatus::Starting,
        transcript: Transcript::default(),
        sent: 0,
        started: 0,
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
        files: Entity<Arc<Vec<PathBuf>>>,
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
                pending: HashMap::new(),
                marks_generation: 0,
                focus: cx.focus_handle(),
                workspace: weak,
                version: 0,
                tab: Tab::Session,
                composer: Composer::default(),
                files,
                expanded: HashSet::new(),
                chat_scroll: ScrollHandle::new(),
                review_scroll: ScrollHandle::new(),
                review: Review::default(),
            }
        })
    }

    pub fn focus(&self) -> FocusHandle {
        self.focus.clone()
    }

    /// Ask AI from the editor: shows this tool window on the current session, or starts
    /// a new one, puts `@selection` in the message box and focuses it. The message stays
    /// unfinished: the user adds what to ask and sends it.
    pub(crate) fn ask_ai(
        &mut self,
        selection: Selection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.composer.keep_selection(selection);
        self.composer.mention_selection();
        // A note goes into the message box of a chat, not into the terminal of a TUI.
        let chats = |kind: &AgentKind| matches!(kind, AgentKind::Acp(_));
        let needs_session = self
            .active_entry()
            .is_none_or(|entry| entry.terminal.is_some());
        if needs_session
            && self.starting.is_none()
            && let Some(kind) = self.kinds().into_iter().find(chats)
        {
            self.start(kind, window, cx);
        }
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// Starts a session of `kind`, asking first when the project is not trusted yet.
    pub fn request(&mut self, kind: AgentKind, window: &mut Window, cx: &mut Context<Self>) {
        if self.trust.is_trusted(&self.root) {
            self.start(kind, window, cx);
            return;
        }
        let panel = cx.weak_entity();
        let on_trust = move |window: &mut Window, cx: &mut App| {
            panel
                .update(cx, |panel, cx| panel.start(kind, window, cx))
                .ok();
        };
        let (root, store) = (self.root.clone(), self.trust.clone());
        self.workspace
            .update(cx, |workspace, cx| {
                TrustDialog::show(root, store, workspace, on_trust, window, cx)
            })
            .ok();
    }

    fn start(&mut self, kind: AgentKind, window: &mut Window, cx: &mut Context<Self>) {
        if self.starting.is_some() {
            return;
        }
        let installed = match &kind {
            AgentKind::Tui(harness) => {
                let found = self.installs.iter().find(|it| it.harness == *harness);
                found.cloned()
            }
            _ => None,
        };
        self.starting = Some(kind.clone());
        self.error = None;
        let id = self.next_id;
        self.next_id += 1;
        let (services, root) = (self.services.clone(), self.root.clone());
        let languages = self.languages.clone();
        let options = self.options.clone();
        let environment = self.environment.clone();
        let task = cx.background_executor().spawn(async move {
            launch(
                kind,
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
                self.tab = Tab::Session;
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

    pub(crate) fn active_entry(&self) -> Option<&Entry> {
        self.entries.get(self.active?)
    }

    pub(crate) fn active_entry_mut(&mut self) -> Option<&mut Entry> {
        self.entries.get_mut(self.active?)
    }

    /// The active session shows its input box, the only screen a script may type into.
    pub fn input_ready(&self) -> bool {
        let Some(entry) = self.active_entry() else {
            return false;
        };
        match (&entry.terminal, &entry.kind) {
            (Some(terminal), AgentKind::Tui(harness)) => {
                let lines = terminal.screen().lines();
                input_box_visible(*harness, &lines, false, None)
            }
            // A chat takes input once the agent is up and not waiting on a question.
            _ => {
                entry.status != SessionStatus::Starting
                    && entry.transcript.pending_permission().is_none()
            }
        }
    }

    /// The active chat stopped: its turn ended, or it asks something, or it is gone.
    pub fn agent_settled(&self) -> bool {
        let Some(entry) = self.active_entry() else {
            return true;
        };
        let stopped = matches!(entry.status, SessionStatus::Error | SessionStatus::Done);
        let idle = !entry.transcript.running && entry.queued() == 0;
        let asking = entry.transcript.pending_permission().is_some();
        stopped || asking || (idle && entry.status == SessionStatus::WaitingForUser)
    }

    fn active_terminal(&self) -> Option<&Arc<TerminalSession>> {
        self.active_entry()?.terminal.as_ref()
    }

    pub(crate) fn tracker(&self) -> Option<&Arc<ChangeTracker>> {
        self.services.as_ref()?.tracker.as_ref().ok()
    }

    /// Reads the hunks of every open file a session changed, for the marks of its gutter
    /// and the bar above it. Files with unsaved edits keep the bar but no marks: their
    /// lines belong to the hunk ranges of the disk.
    pub(crate) fn refresh_agent_changes(&mut self, cx: &mut Context<Self>) {
        let editors = OpenEditors::all(cx);
        self.marks_generation = self.marks_generation.wrapping_add(1);
        let generation = self.marks_generation;
        let clear = |view: &Entity<EditorView>, cx: &mut App| {
            view.update(cx, |view, cx| view.set_agent_marks(Vec::new(), cx));
        };
        let session: HashSet<String> = self
            .changes
            .session
            .iter()
            .map(|change| change.path.clone())
            .collect();
        let mut targets = Vec::new();
        for view in &editors {
            let path = view.read(cx).path().to_path_buf();
            let relative = crate::git::relative_of(&self.root, &path);
            if session.contains(&relative) {
                targets.push((path, relative));
            } else {
                clear(view, cx);
            }
        }
        let Some(tracker) = self.tracker().cloned() else {
            let had = !self.pending.is_empty();
            self.pending.clear();
            for view in &editors {
                clear(view, cx);
                if had {
                    view.update(cx, |_, cx| cx.notify());
                }
            }
            return;
        };
        if targets.is_empty() {
            let had = !self.pending.is_empty();
            self.pending.clear();
            if had {
                for view in &editors {
                    view.update(cx, |_, cx| cx.notify());
                }
            }
            return;
        }
        let job = cx.background_executor().spawn(async move {
            targets
                .into_iter()
                .map(|(path, relative)| (path, tracker.review(&relative).ok()))
                .collect::<Vec<_>>()
        });
        cx.spawn(async move |this, cx| {
            let results = job.await;
            this.update(cx, |this, cx| {
                if this.marks_generation != generation {
                    return;
                }
                let theme = *cx.global::<Theme>();
                let mut pending = HashMap::new();
                for (path, review) in results {
                    let Some(view) = OpenEditors::get(cx, &path) else {
                        continue;
                    };
                    let Some(review) = review else {
                        clear(&view, cx);
                        continue;
                    };
                    let hunks = if review.binary { 1 } else { review.hunks.len() };
                    if hunks == 0 {
                        clear(&view, cx);
                        continue;
                    }
                    let relative = crate::git::relative_of(&this.root, &path);
                    pending.insert(
                        path,
                        PendingFile {
                            path: relative,
                            hunks,
                        },
                    );
                    if view.read(cx).is_dirty() {
                        clear(&view, cx);
                    } else {
                        let marks = agent_marks(&review, theme.stripe_agent);
                        view.update(cx, |view, cx| view.set_agent_marks(marks, cx));
                    }
                }
                this.pending = pending;
                // The bar above a file follows the pending list, so every editor of the
                // window draws again even when its own marks stayed the same.
                for view in OpenEditors::all(cx) {
                    view.update(cx, |_, cx| cx.notify());
                }
            })
            .ok();
        })
        .detach();
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let mut changed = false;
        let mut chat_grew = false;
        let tracker = self.tracker().cloned();
        let active = self.active;
        for (index, entry) in self.entries.iter_mut().enumerate() {
            for event in entry.events.try_iter() {
                if let Some(tracker) = &tracker {
                    tracker.observe(&event);
                }
                match &event {
                    AgentEvent::Status(status) => entry.status = *status,
                    AgentEvent::Exited(Some(0)) => entry.status = SessionStatus::Done,
                    AgentEvent::Exited(_) => entry.status = SessionStatus::Error,
                    AgentEvent::TurnStarted => entry.started += 1,
                    _ => {}
                }
                if entry.transcript.apply(&event) && Some(index) == active {
                    chat_grew = true;
                }
                changed = true;
            }
        }
        if let Some(tracker) = &tracker {
            let changes = tracker.changes();
            if changes.version != self.changes.version || changes.state != self.changes.state {
                self.changes_arrived(changes, cx);
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
        if chat_grew {
            self.chat_scroll.scroll_to_bottom();
        }
        if changed {
            cx.notify();
        }
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.tab == Tab::Changes {
            return;
        }
        if let Some(terminal) = self.active_terminal().cloned() {
            if !terminal.is_closed() && send_key(&terminal, &event.keystroke, cx) {
                cx.stop_propagation();
            }
            return;
        }
        if self.active_entry().is_some() && self.compose_key(&event.keystroke, window, cx) {
            cx.stop_propagation();
        }
    }

    pub(crate) fn show_tab(&mut self, tab: Tab, cx: &mut Context<Self>) {
        self.tab = tab;
        if tab == Tab::Changes {
            self.reload_review(cx);
        }
        cx.notify();
    }

    /// A text picture of the panel, for scripts.
    pub fn report(&self, cx: &App) -> Vec<String> {
        let mut lines = Vec::new();
        for entry in &self.entries {
            lines.push(format!(
                "agent {} {} {:?}",
                entry.id,
                entry.kind.stem(),
                entry.status
            ));
        }
        if let Some(error) = &self.error {
            lines.push(format!("agent-error {error}"));
        }
        if let Some(entry) = self.active_entry() {
            match &entry.terminal {
                Some(terminal) => {
                    let screen = terminal.screen();
                    let shown = screen.lines().into_iter().filter(|line| !line.is_empty());
                    lines.extend(shown.map(|line| format!("agent-screen {line}")));
                }
                None => lines.extend(entry.transcript.report()),
            }
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
        lines.extend(self.review.report(&self.root, cx));
        lines
    }

    pub(crate) fn status_text(status: SessionStatus, theme: &Theme) -> (&'static str, Hsla) {
        match status {
            SessionStatus::Starting => ("Starting", theme.text_secondary),
            SessionStatus::Running => ("Working", theme.accent),
            SessionStatus::WaitingForUser => ("Waiting for you", theme.warning_mark),
            SessionStatus::Done => ("Finished", theme.text_secondary),
            SessionStatus::Error => ("Ended with an error", theme.close_hover),
        }
    }

    /// What can be started here: the harnesses on PATH, the ACP presets, and the mock in tests.
    fn kinds(&self) -> Vec<AgentKind> {
        let mut kinds: Vec<AgentKind> = self
            .installs
            .iter()
            .map(|install| AgentKind::Tui(install.harness))
            .collect();
        kinds.push(AgentKind::Acp(Preset::Codex));
        kinds.push(AgentKind::Acp(Preset::ClaudeAgent));
        if let Ok(line) = std::env::var("IDE_ACP_COMMAND") {
            kinds.push(AgentKind::Acp(Preset::Custom(line)));
        }
        if self.options.mock {
            kinds.push(AgentKind::Mock);
        }
        kinds
    }

    fn start_buttons(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let mut row = div()
            .flex()
            .flex_wrap()
            .flex_none()
            .gap(px(SPACE_S))
            .px(px(SPACE_M))
            .pb(px(SPACE_S));
        for kind in self.kinds() {
            let id = SharedString::from(format!("start-{}", kind.stem()));
            let (hover, pressed) = (theme.hover, theme.pressed);
            let label = if self.starting.as_ref() == Some(&kind) {
                format!("Starting {}", kind.title())
            } else {
                kind.title()
            };
            let tip = match &kind {
                AgentKind::Acp(preset) => preset.note(),
                _ => None,
            };
            let mut button = div()
                .id(id)
                .flex()
                .items_center()
                .gap(px(4.))
                .h(px(26.))
                .pl(px(4.))
                .pr(px(SPACE_M))
                .rounded(px(CONTROL_RADIUS))
                .border_1()
                .border_color(theme.control_border)
                .hover(move |style| style.bg(hover))
                .active(move |style| style.bg(pressed))
                .child(icon("general/add", theme))
                .child(label);
            if let Some(tip) = tip {
                button = button.tooltip(move |_, cx| tooltip(tip, None, cx));
            }
            let start = kind.clone();
            row = row.child(button.on_click(
                cx.listener(move |this, _, window, cx| this.request(start.clone(), window, cx)),
            ));
        }
        row
    }

    /// Facts the user should know before an agent runs.
    fn notes(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let mut notes = vec!["Claude Agent (ACP) needs an Anthropic API key in ANTHROPIC_API_KEY."];
        if self
            .installs
            .iter()
            .any(|install| install.harness == Harness::Claude)
        {
            notes.push(
                "Claude Code runs without a sandbox on Windows. If its LSP plugin for the \
                 language of the project is on, it starts a second language server next to \
                 the IDE's.",
            );
        }
        div()
            .flex()
            .flex_col()
            .flex_none()
            .px(px(12.))
            .pb(px(SPACE_S))
            .gap(px(2.))
            .text_color(theme.text_secondary)
            .children(notes)
            .child(
                div().flex().child(
                    crate::chat::text_button("agents-md", "Add IDE tools to AGENTS.md", theme)
                        .on_click(cx.listener(|this, _, _, cx| this.add_agents_md(cx))),
                ),
            )
    }

    fn session_rows(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let mut list = div()
            .flex()
            .flex_col()
            .flex_none()
            .px(px(SPACE_S))
            .pb(px(SPACE_S));
        for (index, entry) in self.entries.iter().enumerate() {
            let selected = self.active == Some(index);
            let (status, color) = Self::status_text(entry.status, theme);
            let (hover, chosen) = (theme.selection_hover, theme.selection_inactive);
            let row = div()
                .id(("agent-row", entry.id))
                .group("agent-row")
                .flex()
                .items_center()
                .gap(px(8.))
                .h(px(ROW_HEIGHT))
                .px(px(8.))
                .rounded(px(CONTROL_RADIUS))
                .when(selected, |this| this.bg(chosen))
                .when(!selected, |this| this.hover(move |style| style.bg(hover)))
                .on_click(cx.listener(move |this, _, window, cx| this.select(index, window, cx)))
                .child(div().size(px(7.)).rounded_full().bg(color).flex_none())
                .child(div().flex_1().truncate().child(entry.title()))
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

    fn tabs(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let session = match self.active_entry() {
            Some(entry) if entry.terminal.is_some() => "Terminal",
            _ => "Chat",
        };
        let changes = format!("Changes ({})", self.changes.session.len());
        let tab = |id: &'static str, label: SharedString, which: Tab, cx: &mut Context<Self>| {
            let selected = self.tab == which;
            div()
                .id(id)
                .px(px(SPACE_M))
                .h(px(26.))
                .flex()
                .items_center()
                .rounded(px(CONTROL_RADIUS))
                .when(selected, |this| this.bg(theme.tab_selected))
                .on_click(cx.listener(move |this, _, _, cx| this.show_tab(which, cx)))
                .child(label)
        };
        div()
            .flex()
            .flex_none()
            .gap(px(4.))
            .px(px(8.))
            .py(px(4.))
            .border_t_1()
            .border_color(theme.border)
            .child(tab("tab-session", session.into(), Tab::Session, cx))
            .child(tab("tab-changes", changes.into(), Tab::Changes, cx))
    }

    /// Reads the hunks of `view` again and shows the window of the hunk that holds
    /// `line`, off the UI thread.
    pub(crate) fn open_agent_hunk(
        &mut self,
        view: Entity<EditorView>,
        line: u32,
        window: AnyWindowHandle,
        cx: &mut Context<Self>,
    ) {
        let Some(tracker) = self.tracker().cloned() else {
            return;
        };
        let path = view.read(cx).path().to_path_buf();
        let relative = crate::git::relative_of(&self.root, &path);
        let job_relative = relative.clone();
        let opened_tracker = tracker.clone();
        let job = cx
            .background_executor()
            .spawn(async move { opened_tracker.review(&job_relative).ok() });
        let workspace = self.workspace.clone();
        let root = self.root.clone();
        cx.spawn(async move |_this, cx| {
            let Some(review) = job.await else {
                return;
            };
            let Some(index) = hunk_at(&review, line) else {
                return;
            };
            _this
                .update(cx, |_, cx| {
                    window
                        .update(cx, |_, window, cx| {
                            workspace
                                .update(cx, |workspace, cx| {
                                    crate::agent_hunk::AgentHunkPopup::open(
                                        workspace, window, cx, root, relative, view, tracker,
                                        review, index,
                                    )
                                })
                                .ok()
                        })
                        .ok();
                })
                .ok();
        })
        .detach();
    }
}

/// The command of the editor menu and of Find Action. It runs only when the editor has a
/// selection: without one there is nothing to ask about.
pub(crate) fn ask_ai(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    // The command runs inside the update of the window, so the selection is read from the
    // editor itself and not through the panel, which would ask the window again.
    let Some(path) = workspace.active_path() else {
        return;
    };
    let Some((first, last, text)) = OpenEditors::selection(&path, cx) else {
        return;
    };
    let selection = (path, (first as u32, last as u32), text);
    let Some(panel) = cx.try_global::<ActiveAgents>().map(|panel| panel.0.clone()) else {
        return;
    };
    workspace.show_tool(AI_TOOL, window, cx);
    panel.update(cx, |panel, cx| panel.ask_ai(selection, window, cx));
}

/// A file finished loading or was saved: it may show hunks an agent changed.
pub(crate) fn refresh_marks(cx: &mut App) {
    let Some(panel) = cx.try_global::<ActiveAgents>().map(|panel| panel.0.clone()) else {
        return;
    };
    panel.update(cx, |panel, cx| panel.refresh_agent_changes(cx));
}

/// The marks of the hunks of an agent's review, in the colour of the theme.
fn agent_marks(review: &change_tracker::FileReview, color: Hsla) -> Vec<GutterMark> {
    review
        .hunks
        .iter()
        .map(|hunk| {
            let kind = if hunk.before.is_empty() {
                GutterKind::Added
            } else if hunk.after.is_empty() {
                GutterKind::Deleted
            } else {
                GutterKind::Modified
            };
            GutterMark {
                lines: hunk.after.clone(),
                kind,
                color,
            }
        })
        .collect()
}

/// The line of the hunk that holds `line`; a removal belongs to the line it sits before.
pub(crate) fn hunk_at(review: &change_tracker::FileReview, line: u32) -> Option<usize> {
    review.hunks.iter().position(|hunk| {
        if hunk.after.is_empty() {
            hunk.after.start == line
        } else {
            hunk.after.contains(&line)
        }
    })
}

/// The bar above a file an agent changed: the number of its unaccepted hunks and the
/// whole-file actions of the review.
fn change_bar(
    path: &Path,
    _window: &mut Window,
    cx: &mut Context<EditorView>,
) -> Option<AnyElement> {
    let panel = cx.try_global::<ActiveAgents>()?.0.clone();
    let pending = panel.read_with(cx, |panel, _| panel.pending.get(path).cloned())?;
    let theme = *cx.global::<Theme>();
    let workspace = panel.read_with(cx, |panel, _| panel.workspace.clone());
    let relative = pending.path;
    let count = pending.hunks;
    let word = if count == 1 { "change" } else { "changes" };
    let label = format!("Agent changed this file: {count} {word}");
    let accept = panel.clone();
    let accept_path = relative.clone();
    let accept = text_button("agent-bar-accept-all", "Accept All", &theme)
        .debug_selector(|| "agent-bar-accept-all".to_owned())
        .on_click(move |_, _, cx| {
            accept.update(cx, |panel, cx| {
                panel.act_on_file(accept_path.clone(), Action::AcceptFile, cx)
            })
        });
    let reject = panel.clone();
    let reject_path = relative.clone();
    let reject = text_button("agent-bar-reject-all", "Reject All", &theme)
        .debug_selector(|| "agent-bar-reject-all".to_owned())
        .on_click(move |_, _, cx| {
            reject.update(cx, |panel, cx| {
                panel.act_on_file(reject_path.clone(), Action::RevertFile, cx)
            })
        });
    let review = panel.clone();
    let review_path = relative;
    let review = text_button("agent-bar-review", "Review", &theme)
        .debug_selector(|| "agent-bar-review".to_owned())
        .on_click(move |_, window, cx| {
            review.update(cx, |panel, cx| panel.review_file(review_path.clone(), cx));
            workspace
                .update(cx, |workspace, cx| {
                    workspace.show_tool(workspace::AI_TOOL, window, cx)
                })
                .ok();
        });
    Some(
        div()
            .id("agent-change-bar")
            .debug_selector(|| "agent-change-bar".to_owned())
            .flex()
            .items_center()
            .flex_none()
            .gap(px(SPACE_S))
            .h(px(CONTROL_HEIGHT))
            .px(px(SPACE_M))
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.stripe_agent.opacity(0.12))
            .child(div().size(px(6.)).rounded_full().bg(theme.stripe_agent))
            .child(div().flex_1().truncate().child(label))
            .child(accept)
            .child(reject)
            .child(review)
            .into_any_element(),
    )
}

/// Puts the bar of an agent's changes over the bars of the application: a file without
/// unaccepted hunks gets the row of whoever else asks for one.
pub(crate) fn install(cx: &mut App) {
    let previous = cx.try_global::<FileBar>().map(|bar| bar.0.clone());
    cx.set_global(FileBar(Rc::new(
        move |path: &Path, window: &mut Window, cx: &mut Context<EditorView>| {
            change_bar(path, window, cx)
                .or_else(|| previous.as_ref().and_then(|bar| bar(path, window, cx)))
        },
    )));
}

impl Render for AgentsPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let hide = |_: &ClickEvent, window: &mut Window, cx: &mut App| {
            window.dispatch_action(Box::new(ToggleAi), cx);
        };
        let focused = self.focus.is_focused(window);
        let body = div()
            .id("agent-body")
            .key_context("AgentPanel")
            .track_focus(&self.focus)
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .on_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| window.focus(&this.focus, cx)),
            );
        let body = match (self.tab, self.active_terminal().cloned()) {
            (Tab::Changes, _) => body.child(self.changes_view(&theme, window, cx)),
            (Tab::Session, Some(terminal)) => {
                let scrolled = terminal.clone();
                let view = crate::term_grid::GridView {
                    session: terminal.clone(),
                    focused,
                    theme,
                    selection: None,
                    matches: Vec::new(),
                    current: None,
                    hover: None,
                    host: None,
                    pane: 0,
                    metrics: std::rc::Rc::new(std::cell::Cell::new(None)),
                };
                body.on_scroll(move |event, _, _| scrolled.scroll(scroll_lines(event)))
                    .child(grid(view))
            }
            (Tab::Session, None) if self.entries.is_empty() => {
                body.child(message("No sessions yet", &theme))
            }
            (Tab::Session, None) => body.child(self.chat_view(focused, &theme, window, cx)),
        };
        let error = self.error.clone().map(|error| {
            div()
                .px(px(16.))
                .pb(px(8.))
                .text_color(theme.close_hover)
                .child(error)
        });
        island(&theme)
            .group(GROUP)
            .h_full()
            .text_size(px(UI_SIZE))
            .child(panel_header("AI Sessions", GROUP, hide, &theme))
            .child(self.start_buttons(&theme, cx))
            .child(self.notes(&theme, cx))
            .children(error)
            .child(self.session_rows(&theme, cx))
            .child(self.tabs(&theme, cx))
            .child(body)
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

    fn home(name: &str, root: &Path) -> Result<(Sandbox, MockApi), String> {
        let error = |error: std::io::Error| error.to_string();
        let sandbox = Sandbox::for_project(name, root).map_err(error)?;
        let api = MockApi::start().map_err(error)?;
        Ok((sandbox, api))
    }

    pub fn launch(
        harness: Harness,
        root: &Path,
        id: usize,
    ) -> Result<(Launch, Box<dyn Send>), String> {
        let error = |error: std::io::Error| error.to_string();
        let (sandbox, api) = home(&format!("ide-{}{id}", harness.stem()), root)?;
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

    /// The environment of an ACP adapter on the mock: an empty home, and an npm cache that
    /// outlives the home so the adapter downloads once.
    pub fn acp_env(preset: &Preset, root: &Path, id: usize) -> Result<(Env, Kept), String> {
        let error = |error: std::io::Error| error.to_string();
        let (sandbox, api) = home(&format!("ide-acp{id}"), root)?;
        let mut env = match preset {
            Preset::Codex => {
                let env = codex_env(&sandbox, &api).map_err(error)?;
                // Ask before every command that is not known as safe, so the chat shows the
                // question as buttons.
                let config = sandbox.home.join(".codex/config.toml");
                let text = std::fs::read_to_string(&config).map_err(error)?;
                let text = text.replace(
                    "approval_policy = \"never\"",
                    "approval_policy = \"untrusted\"",
                );
                std::fs::write(&config, text).map_err(error)?;
                env
            }
            Preset::ClaudeAgent => {
                let mut env = claude_env(&sandbox, &api).map_err(error)?;
                env.push((
                    "ANTHROPIC_API_KEY".to_string(),
                    agent_testkit::FAKE_TOKEN.to_string(),
                ));
                env
            }
            Preset::Custom(_) => sandbox.env().map_err(error)?,
        };
        let cache = std::env::temp_dir().join("ide-t").join("npm-cache");
        env.push((
            "npm_config_cache".to_string(),
            cache.to_string_lossy().into_owned(),
        ));
        let home = Home {
            _sandbox: sandbox,
            _api: api,
        };
        Ok((env, Box::new(home)))
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

    pub fn acp_env(_: &Preset, _: &Path, _: usize) -> Result<(Env, Kept), String> {
        Err(MISSING.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::{AgentKind, project_key};
    use std::path::Path;

    #[test]
    fn a_project_key_ignores_the_case_of_the_path() {
        let key = project_key(Path::new(r"C:\Work\Demo"));
        assert_eq!(key, project_key(Path::new(r"c:\work\demo")));
        assert_ne!(key, project_key(Path::new(r"C:\Work\Other")));
        assert_eq!(key.len(), 16);
    }

    #[test]
    fn script_names_round_trip() {
        for name in ["claude", "codex", "acp-codex", "acp-claude", "mock"] {
            assert_eq!(AgentKind::parse(name).unwrap().stem(), name);
        }
        assert!(AgentKind::parse("vim").is_none());
    }
}
