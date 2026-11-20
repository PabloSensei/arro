//! The AI sessions tool window: starts an agent in the project, switches between sessions, and
//! shows the active one as a terminal or a chat, next to a review of the files it changed.

use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex, MutexGuard};

use agent_acp::{AcpRuntime, Preset};
use agent_core::{
    AgentEvent, AgentRuntime, AgentSession, Harness, Installed, MockRuntime, MockStep, SessionSpec,
    SessionStatus, Surface, ToolEndpoint, detect,
};
use agent_pty::{Launch, PtyRuntime, input_box_visible};
use change_tracker::{ChangeSet, ChangeTracker, GitRunner};
use claude_bridge::{BridgeEvent, DiffDecision};
use crossbeam_channel::Receiver;
use editor_view::{EditorView, FileBar, GutterKind, GutterMark};
use lang_api::WorkspaceChange;
use mcp_discovery::{EntryPoint, EntryPointFile};
use mcp_server::McpServer;
use project_model::EnvironmentProvider;
use store::TrustList;
use terminal::TerminalSession;
use tool_gateway::{
    ApprovalRequest, AuditLog, EditOutcome, LanguageTools, SessionScope, ToolBackend, ToolGateway,
};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::{AI_TOOL, Settings, ToggleAi, Workspace};

use crate::agent_tools::{AgentTools, OpenDocument, WindowTools};
use crate::ai_review::{Action, AgentsMdNote, Review};
use crate::chat::text_button;
use crate::composer::{Composer, Selection};
use crate::mcp_approval::{McpApprovals, Reply};
use crate::mcp_edits::{EditKind, EditReply, EditRequests};
use crate::outline::OpenEditors;
use crate::proposal::{Proposal, Waiter};
use crate::term_grid::{GridPanel, GridView, Position, every_frame, grid, scroll_lines, send_key};
use crate::terminal_panel::message;
use crate::transcript::Transcript;
use crate::trust::TrustDialog;

pub(crate) const GROUP: &str = "right-panel";
const FIRST_SIZE: (u16, u16) = (80, 24);
/// The tooltip of the start button of Claude Code in a terminal.
const CLAUDE_CODE_TIP: &str = "Claude Code in a terminal. On Windows it runs without a sandbox. \
    Its LSP plugin, if on, starts a second language server next to the IDE's.";

/// The command of the editor menu and of Find Action: asks the agent about the selection.
pub const ASK_AI: &str = "ai.ask_selection";

/// What a mock run keeps alive: its home and its model API.
type Kept = Box<dyn Send>;
/// Variables of a prepared environment.
type Env = Vec<(String, String)>;
/// The runtime of a session, what a mock run keeps alive, and the name a scripted agent gives.
type Runtime = (Box<dyn AgentRuntime>, Option<Kept>, Option<String>);

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

/// The MCP server of the project window: one gateway and one policy for the sessions of
/// the AI tool window and for the clients that find the window by its entry point.
pub(crate) struct Mcp {
    pub(crate) gateway: Arc<ToolGateway>,
    pub(crate) server: McpServer,
    root: PathBuf,
    state_dir: PathBuf,
    /// The entry point of the window while the switch of the settings is on.
    published: Mutex<Option<Published>>,
}

struct Published {
    entry: EntryPoint,
    file: EntryPointFile,
}

impl Mcp {
    fn new(gateway: Arc<ToolGateway>, server: McpServer, root: &Path, state_dir: &Path) -> Self {
        Self {
            gateway,
            server,
            root: root.to_path_buf(),
            state_dir: state_dir.to_path_buf(),
            published: Mutex::new(None),
        }
    }

    /// Makes the server reachable for the clients outside the window: a token of its own
    /// and the entry point in the state folder. A second call changes nothing.
    fn publish(&self) -> Result<(), String> {
        let mut published = self.published();
        if published.is_some() {
            return Ok(());
        }
        let token = self
            .server
            .issue_token(SessionScope {
                session: "mcp".to_string(),
                project: self.root.clone(),
                cwd: self.root.clone(),
            })
            .map_err(|error| error.to_string())?;
        let entry = EntryPoint {
            root: self.root.clone(),
            url: self.server.url(),
            token: token.clone(),
            pid: std::process::id(),
        };
        match entry.write(&self.state_dir) {
            Ok(file) => {
                *published = Some(Published { entry, file });
                Ok(())
            }
            Err(error) => {
                self.server.revoke(&entry.token);
                Err(error.to_string())
            }
        }
    }

    /// Takes the entry point away and stops the token behind it. The sessions of the tool
    /// window keep their own tokens and go on working.
    fn unpublish(&self) {
        if let Some(published) = self.published().take() {
            self.server.revoke(&published.entry.token);
            drop(published.file);
        }
    }

    /// The entry point a client outside the window uses, for tests and scripts.
    #[cfg(all(test, windows))]
    pub(crate) fn entry_point(&self) -> Option<EntryPoint> {
        self.published()
            .as_ref()
            .map(|published| published.entry.clone())
    }

    /// The address and the token a client of the settings page copies, while the window
    /// publishes its entry point.
    pub(crate) fn endpoint(&self) -> Option<(String, String)> {
        self.published()
            .as_ref()
            .map(|published| (published.entry.url.clone(), published.entry.token.clone()))
    }

    fn published(&self) -> MutexGuard<'_, Option<Published>> {
        self.published
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        self.unpublish();
    }
}

/// What a session of the window shares: the MCP server and the tracker of its changes.
pub(crate) struct Services {
    pub mcp: Arc<Mcp>,
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
    /// The name of the session that the script of a mock agent gives, instead of its kind.
    name: Option<String>,
    /// The text that a drag or a click selected on the terminal of this session.
    selection: Option<crate::term_grid::Selection>,
    /// The place the button went down on, while a drag extends the selection from it.
    dragging: Option<Position>,
}

impl Entry {
    fn title(&self) -> String {
        let kind = self.name.clone().unwrap_or_else(|| self.kind.title());
        match &self.transcript.title {
            Some(title) => format!("{kind} {}: {title}", self.id),
            None => format!("{kind} {}", self.id),
        }
    }

    pub fn queued(&self) -> usize {
        self.sent.saturating_sub(self.started)
    }
}

impl Drop for Entry {
    fn drop(&mut self) {
        self.session.close();
        self.services.mcp.server.revoke(&self.token);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tab {
    Session,
    Changes,
    /// The MCP clients of the project, their calls and their permissions.
    Agents,
}

/// The unaccepted hunks of one file an agent changed, for the bar above its editor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PendingFile {
    /// Relative to the project, with forward slashes.
    pub path: String,
    pub hunks: usize,
}

/// A rename an MCP client asked for, waiting for the user to confirm it.
pub(crate) struct PendingRename {
    /// The tool call that waits for the answer.
    pub id: u64,
    pub change: WorkspaceChange,
    /// Relative to the project, with forward slashes.
    pub files: Vec<PathBuf>,
    pub edits: usize,
}

pub struct AgentsPanel {
    pub(crate) root: PathBuf,
    options: AgentOptions,
    environment: Option<Arc<dyn EnvironmentProvider>>,
    installs: Vec<Installed>,
    /// The server of the project, alive while the window is open.
    pub(crate) mcp: Option<Arc<Mcp>>,
    pub(crate) services: Option<Arc<Services>>,
    tools: Arc<WindowTools>,
    /// The questions an `Execute` tool asks the user, shown as a notification.
    pub(crate) approvals: Arc<McpApprovals>,
    shown_question: Option<ApprovalRequest>,
    pub(crate) entries: Vec<Entry>,
    pub(crate) active: Option<usize>,
    next_id: usize,
    starting: Option<AgentKind>,
    /// The transcript the next mock session plays, named by a window script.
    mock_transcript: Option<PathBuf>,
    pub(crate) error: Option<SharedString>,
    pub(crate) changes: Arc<ChangeSet>,
    /// The files with unaccepted agent hunks, by absolute path, for the bar above an
    /// editor and for the marks of its gutter.
    pub(crate) pending: HashMap<PathBuf, PendingFile>,
    /// The files a client offers through `propose_edit` or `openDiff`, waiting for the user.
    pub(crate) proposals: Vec<Proposal>,
    /// The next id of a proposal, unique among the proposals of the window.
    next_proposal: u64,
    /// The editors whose marks belong to a proposal, so they can be cleared again.
    proposal_marked: HashSet<PathBuf>,
    /// The requests of the edit tools, shared with the calls that wait on them.
    pub(crate) edits: Arc<EditRequests>,
    /// The window the panel opens a proposed file in.
    window: AnyWindowHandle,
    /// A rename an MCP client waits on, with the notification of its preview.
    pub(crate) pending_rename: Option<PendingRename>,
    /// The file the last delete of a tool removed, with its bytes, for Undo.
    last_deleted: Option<(PathBuf, Vec<u8>)>,
    /// The last view published to the IDE protocol, so an idle frame does no work.
    view_key: Option<u64>,
    /// Numbers the reads of the hunks, so an older answer does not replace a newer one.
    marks_generation: u64,
    trust: TrustList,
    pub(crate) focus: FocusHandle,
    pub(crate) workspace: WeakEntity<Workspace>,
    version: u64,
    pub(crate) tab: Tab,
    pub(crate) composer: Composer,
    /// A request waits for the chat session that is starting, and goes out when it is up.
    pending_message: Option<String>,
    pub(crate) files: Entity<Arc<Vec<PathBuf>>>,
    pub(crate) expanded: HashSet<String>,
    pub(crate) chat_scroll: ScrollHandle,
    pub(crate) review_scroll: ScrollHandle,
    pub(crate) review: Review,
    /// What the AGENTS.md button reported last, shown next to it for a while.
    pub(crate) agents_md: Option<AgentsMdNote>,
    /// Numbers the results of that button, so the fade of an older one leaves a newer one.
    pub(crate) agents_md_generation: u64,
    /// The client the Agents tab shows, by its place in the list.
    pub(crate) clients_selected: usize,
    /// The Agents tab shows only the calls that run something of the project.
    pub(crate) clients_only_actions: bool,
    pub(crate) clients_scroll: ScrollHandle,
}

/// The Exposed Tools table of the settings, as the gateway applies it.
fn tool_exposure(settings: &Settings) -> tool_gateway::ToolExposure {
    let mut exposure = tool_gateway::ToolExposure::default();
    for (name, choice) in &settings.mcp_tool_settings {
        exposure.set_state(
            name,
            tool_gateway::ToolState {
                enabled: choice.enabled,
                router_only: choice.router_only,
            },
        );
    }
    if settings.mcp_router_only_mode {
        exposure.set_router_only(Some(match settings.mcp_router_only_scope {
            workspace::McpRouterScope::AllAgents => tool_gateway::RouterScope::AllAgents,
            workspace::McpRouterScope::AcpOnly => tool_gateway::RouterScope::AcpOnly,
        }));
    }
    exposure
}

/// A short stable name for the state of one project.
fn project_key(root: &Path) -> String {
    let text = root.to_string_lossy().to_lowercase();
    let hash = text.bytes().fold(0xcbf29ce484222325u64, |hash, byte| {
        (hash ^ byte as u64).wrapping_mul(0x100000001b3)
    });
    format!("{hash:016x}")
}

/// The bridge of the IDE protocol of the window, when the application started one.
fn bridge_of(cx: &App) -> Option<Arc<claude_bridge::Bridge>> {
    cx.try_global::<crate::claude_ide::ActiveClaude>()
        .and_then(|active| active.bridge.clone())
}

/// Starts the server of the project: the gateway, the policy and the audit log of the
/// window. `publish` writes the entry point that lets a client outside the window find it.
fn open_mcp(
    root: &Path,
    options: &AgentOptions,
    tools: Arc<WindowTools>,
    approvals: Arc<McpApprovals>,
    publish: bool,
) -> Result<Mcp, String> {
    // A mock run has a stand-in model but not stand-in tools: a demo shows the answers the
    // window itself gives, from the index and the language server of the project.
    let backend: Arc<dyn ToolBackend> = tools;
    let gateway = Arc::new(ToolGateway::with_approval(
        backend,
        AuditLog::default(),
        approvals,
    ));
    let server = McpServer::start(gateway.clone()).map_err(|error| error.to_string())?;
    let mcp = Mcp::new(gateway, server, root, &options.state_dir);
    if publish {
        mcp.publish()?;
    }
    Ok(mcp)
}

/// Reads the changes of the project since the sessions started. Blocks on Git, so it
/// belongs on a background thread and not at the open of the window.
fn open_tracker(root: &Path, options: &AgentOptions) -> Result<Arc<ChangeTracker>, String> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let state = options.state_dir.join("changes").join(project_key(root));
    match GitRunner::find(&path) {
        Some(git) => ChangeTracker::open(git, &state, root)
            .map(Arc::new)
            .map_err(|error| error.to_string()),
        None => Err("git is not on PATH, changes are not tracked".to_string()),
    }
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

/// The turns of a mock session and the name its session takes: the transcript a window script
/// or the environment names, or the built-in chat, which has no name of its own.
fn mock_scenario(
    transcript: Option<&Path>,
    root: &Path,
) -> Result<(Option<String>, Vec<Vec<MockStep>>), String> {
    #[cfg(feature = "mock-agents")]
    if let Some(script) = crate::mock_transcript::named(transcript, root)? {
        return Ok((script.title, script.turns));
    }
    #[cfg(not(feature = "mock-agents"))]
    let _ = (transcript, root);
    Ok((None, mock_turns()))
}

/// What a preset that needs an API key says when the key is not in the environment of the IDE,
/// which the agent inherits. `None` when the preset needs no key or the key is set.
fn missing_api_key(preset: &Preset, key: Option<&OsStr>) -> Option<String> {
    let variable = preset.api_key_variable()?;
    if key.is_some_and(|key| !key.is_empty()) {
        return None;
    }
    Some(format!(
        "{} needs {variable}, and it is not set in the environment of the IDE. \
         Set it and start the agent again.",
        preset.title()
    ))
}

/// The runtime for `kind`, what a mock run keeps alive, and the name a scripted agent gives its
/// session.
#[allow(clippy::too_many_arguments)]
fn runtime_for(
    kind: &AgentKind,
    installed: Option<Installed>,
    root: &Path,
    options: &AgentOptions,
    environment: Option<&Arc<dyn EnvironmentProvider>>,
    gateway: &Arc<ToolGateway>,
    ide: &[(String, String)],
    id: usize,
    transcript: Option<&Path>,
) -> Result<Runtime, String> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    match kind {
        AgentKind::Tui(harness) => {
            let installed = installed.ok_or(format!("{} is not on PATH", harness.stem()))?;
            let (launch, home) = if options.mock {
                let (launch, home) = mock::launch(*harness, root, id)?;
                (launch, Some(home))
            } else {
                let mut variables = environment
                    .map(|environment| environment.describe().variables)
                    .unwrap_or_default();
                // The IDE protocol of the window, so a harness started here sees the
                // diagnostics and shows its diffs in the editor.
                variables.extend(ide.iter().cloned());
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
            Ok((Box::new(runtime), home, None))
        }
        AgentKind::Acp(preset) => {
            let mut command = preset.command(&path)?;
            let mut home = None;
            if options.mock {
                let (env, kept) = mock::acp_env(preset, root, id)?;
                command = command.with_env(env);
                home = Some(kept);
            } else if let Some(variable) = preset.api_key_variable() {
                let key = std::env::var_os(variable);
                if let Some(missing) = missing_api_key(preset, key.as_deref()) {
                    return Err(missing);
                }
            }
            let runtime = AcpRuntime::new(preset.title(), command);
            Ok((Box::new(runtime), home, None))
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
            let (name, turns) = mock_scenario(transcript, root)?;
            let runtime = MockRuntime::new(turns).with_tools(invoke);
            Ok((Box::new(runtime), None, name))
        }
    }
}

/// Runs off the UI thread: starting a server and a process may take a moment.
#[allow(clippy::too_many_arguments)]
fn launch(
    kind: AgentKind,
    installed: Option<Installed>,
    mcp: Option<Arc<Mcp>>,
    tools: Arc<WindowTools>,
    approvals: Arc<McpApprovals>,
    root: PathBuf,
    options: AgentOptions,
    environment: Option<Arc<dyn EnvironmentProvider>>,
    ide: Vec<(String, String)>,
    id: usize,
    transcript: Option<PathBuf>,
) -> Result<Entry, String> {
    let mcp = match mcp {
        Some(mcp) => mcp,
        None => Arc::new(open_mcp(&root, &options, tools, approvals, false)?),
    };
    let services = Arc::new(Services {
        mcp,
        tracker: open_tracker(&root, &options),
    });
    let (runtime, mock, name) = runtime_for(
        &kind,
        installed,
        &root,
        &options,
        environment.as_ref(),
        &services.mcp.gateway,
        &ide,
        id,
        transcript.as_deref(),
    )?;
    let token = services
        .mcp
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
            url: services.mcp.server.url(),
            token: token.clone(),
        }),
    };
    let session: Arc<dyn AgentSession> = match runtime.start(spec) {
        Ok(session) => Arc::from(session),
        Err(error) => {
            services.mcp.server.revoke(&token);
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
        name,
        selection: None,
        dragging: None,
    })
}

/// What a start row says about its agent, as its tooltip: what it is and what it asks for.
fn start_tip(kind: &AgentKind) -> Option<&'static str> {
    match kind {
        AgentKind::Tui(Harness::Claude) => Some(CLAUDE_CODE_TIP),
        AgentKind::Acp(preset) => preset.note(),
        _ => None,
    }
}

/// The row of an agent in the New Session menu: a click starts the session, and the hint of the
/// agent shows as the tooltip of the row.
fn start_entry(panel: &Entity<AgentsPanel>, kind: AgentKind) -> MenuEntry {
    let id = format!("new-session-{}", kind.stem());
    let tip = start_tip(&kind);
    let panel = panel.clone();
    let start = kind.clone();
    let entry = MenuEntry::new(kind.title(), move |window, cx| {
        panel.update(cx, |this, cx| this.request(start.clone(), window, cx));
    })
    .id(id);
    match tip {
        Some(tip) => entry.tip(tip),
        None => entry,
    }
}

/// One tab of the session window: the height and the colours of the tabs of the Git window,
/// with an accent line under the chosen one. A count follows the name as a badge.
fn session_tab(
    id: &'static str,
    label: &'static str,
    count: Option<usize>,
    selected: bool,
    theme: &Theme,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let hover = theme.hover;
    let mut content = div().flex().items_center().gap(px(SPACE_S)).child(label);
    if let Some(count) = count {
        content = content.child(count_badge(count, theme));
    }
    div()
        .id(id)
        .debug_selector(move || id.to_owned())
        .flex()
        .flex_none()
        .items_center()
        .h(px(24.))
        .px(px(SPACE_M))
        .rounded(px(CONTROL_RADIUS))
        .border_b_2()
        .border_color(if selected {
            theme.accent
        } else {
            transparent_black()
        })
        .text_color(if selected {
            theme.text
        } else {
            theme.text_secondary
        })
        .when(selected, |this| this.bg(theme.selection_inactive))
        .when(!selected, |this| this.hover(move |style| style.bg(hover)))
        .on_click(on_click)
        .child(content)
}

/// A count in a tab, as a small badge.
fn count_badge(count: usize, theme: &Theme) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .min_w(px(18.))
        .h(px(16.))
        .px(px(SPACE_S))
        .rounded(px(8.))
        .text_size(px(TEXT_SMALL))
        .bg(theme.selection_hover)
        .text_color(theme.text_secondary)
        .child(count.to_string())
}

impl AgentsPanel {
    pub fn create(
        workspace: &Workspace,
        weak: WeakEntity<Workspace>,
        options: AgentOptions,
        files: Entity<Arc<Vec<PathBuf>>>,
        environment: Option<Arc<dyn EnvironmentProvider>>,
        window: AnyWindowHandle,
        cx: &mut App,
    ) -> Entity<Self> {
        let root = workspace.root().to_path_buf();
        let path = std::env::var_os("PATH").unwrap_or_default();
        let installs = detect(&path);
        let tools = cx.global::<AgentTools>().0.clone();
        let edits = tools.edits();
        let approvals = Arc::new(McpApprovals::new(
            options.state_dir.clone(),
            workspace.root().to_path_buf(),
        ));
        cx.new(|cx| {
            every_frame(cx, Self::refresh);
            // The switch of the settings starts and stops the entry point of the window.
            cx.observe_global::<Settings>(|this, cx| this.sync_mcp(cx))
                .detach();
            let mut panel = Self {
                root,
                environment,
                trust: TrustList::new(&options.state_dir),
                options,
                installs,
                mcp: None,
                services: None,
                tools,
                approvals,
                shown_question: None,
                entries: Vec::new(),
                active: None,
                next_id: 1,
                starting: None,
                mock_transcript: None,
                error: None,
                changes: Arc::new(ChangeSet::default()),
                pending: HashMap::new(),
                proposals: Vec::new(),
                next_proposal: 1,
                proposal_marked: HashSet::new(),
                edits,
                window,
                pending_rename: None,
                last_deleted: None,
                view_key: None,
                marks_generation: 0,
                focus: cx.focus_handle(),
                workspace: weak,
                version: 0,
                tab: Tab::Session,
                composer: Composer::default(),
                pending_message: None,
                files,
                expanded: HashSet::new(),
                chat_scroll: ScrollHandle::new(),
                review_scroll: ScrollHandle::new(),
                review: Review::default(),
                agents_md: None,
                agents_md_generation: 0,
                clients_selected: 0,
                clients_only_actions: false,
                clients_scroll: ScrollHandle::new(),
            };
            panel.sync_mcp(cx);
            panel
        })
    }

    /// Starts the server of the project when the settings switch is on, publishes its
    /// entry point or takes it away, and leaves the sessions of the tool window alone.
    fn sync_mcp(&mut self, cx: &mut Context<Self>) {
        let enabled = cx
            .try_global::<Settings>()
            .is_none_or(|settings| settings.mcp_enabled);
        let allow_trusted = cx.try_global::<Settings>().is_some_and(|settings| {
            settings.mcp_execute_permission == workspace::McpExecutePermission::TrustedProjects
        });
        let brave = cx.try_global::<Settings>().is_some_and(|settings| {
            settings.mcp_execute_permission == workspace::McpExecutePermission::WithoutConfirmation
        });
        self.approvals.set_allow_trusted(allow_trusted);
        self.approvals.set_brave(brave);
        match (enabled, self.mcp.clone()) {
            (true, None) => {
                let started = open_mcp(
                    &self.root,
                    &self.options,
                    self.tools.clone(),
                    self.approvals.clone(),
                    true,
                );
                match started {
                    Ok(mcp) => self.mcp = Some(Arc::new(mcp)),
                    Err(error) => {
                        self.error = Some(format!("The MCP server did not start: {error}").into())
                    }
                }
            }
            (true, Some(mcp)) => {
                if let Err(error) = mcp.publish() {
                    self.error = Some(format!("The MCP server is not shared: {error}").into());
                }
            }
            (false, Some(mcp)) => mcp.unpublish(),
            (false, None) => {}
        }
        // The Exposed Tools table of the settings decides what every client already
        // connected sees, so it reaches the gateway of the open server at once.
        if let Some(mcp) = &self.mcp {
            let exposure = cx
                .try_global::<Settings>()
                .map_or_else(tool_gateway::ToolExposure::default, tool_exposure);
            mcp.gateway.set_exposure(exposure);
        }
        cx.notify();
    }

    /// The buttons of the notification of an `Execute` tool: the answer reaches the
    /// thread that waits for it.
    pub(crate) fn answer_mcp_question(&mut self, reply: Reply, cx: &mut Context<Self>) {
        let answered = match reply {
            Reply::Session => self.approvals.answer_for_session(),
            Reply::Once => self.approvals.answer(true),
            Reply::Deny => self.approvals.answer(false),
        };
        // A late click or a command reaches no question, the call being answered or over:
        // there is nothing to close and nothing to tell.
        if !answered {
            return;
        }
        self.shown_question = None;
        let workspace = self.workspace.clone();
        // The command runs while the window is being updated: the balloon closes after it.
        cx.defer(move |cx| {
            workspace
                .update(cx, |workspace, cx| workspace.close_notification(cx))
                .ok();
        });
        cx.notify();
    }

    /// Shows the newest question of an `Execute` tool, once per question.
    fn show_mcp_question(&mut self, cx: &mut Context<Self>) {
        let question = self.approvals.pending();
        if question == self.shown_question {
            return;
        }
        self.shown_question = question.clone();
        let Some(question) = question else {
            return;
        };
        let who = question
            .client
            .clone()
            .unwrap_or_else(|| question.session.clone());
        let action = match question.tool.as_str() {
            "run_tests" => "run tests",
            "build_project" => "build the project",
            "run_configuration" => "run a configuration",
            "stop_run" => "stop a run",
            "debug_run" => "start a debug session",
            "debug_evaluate" => "evaluate an expression in the debugger",
            "debug_step" => "step the debugger",
            "debug_stop" => "stop the debug session",
            other => other,
        };
        let detail = self.approvals.pending_detail().unwrap_or_default();
        let text = if detail.is_empty() {
            format!("{who} wants to {action}")
        } else {
            format!("{who} wants to {action}: {detail}")
        };
        let buttons = vec![
            ("allow", SharedString::from("Allow once"), "ai.mcp.allow"),
            (
                "allow-session",
                SharedString::from("Allow for this session"),
                "ai.mcp.allow_session",
            ),
            ("deny", SharedString::from("Deny"), "ai.mcp.deny"),
        ];
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.notify_user_with_commands("Allow an agent tool?", text, buttons, cx)
            })
            .ok();
    }

    /// Takes the requests of the edit tools: a proposed file the user decides, a rename
    /// preview, or a file action that runs at once.
    fn take_edit_requests(&mut self, cx: &mut Context<Self>) {
        for request in self.edits.take() {
            match request.kind {
                EditKind::Propose {
                    path,
                    base,
                    proposed,
                } => self.add_proposal(Waiter::Tool(request.id), path, base, proposed, cx),
                EditKind::Rename { new_name, change } => {
                    self.preview_rename(request.id, new_name, change, cx)
                }
                EditKind::Apply { change } => {
                    let changed = change.edits.iter().any(|source| !source.edits.is_empty())
                        || !change.renames.is_empty();
                    self.defer_apply(request.id, change, EditReply::Applied { changed }, cx);
                }
                EditKind::Open { path } => self.defer_open(request.id, path, cx),
            }
        }
    }

    /// The diffs of the harnesses: the new ones go to the editors, the ones the harness
    /// took back leave the screen. The bridge only says what happened; no file changes
    /// until the user decides.
    fn take_proposals(&mut self, cx: &mut Context<Self>) {
        let Some(bridge) = bridge_of(cx) else {
            return;
        };
        for event in bridge.events() {
            match event {
                BridgeEvent::Open(diff) => self.add_proposal(
                    Waiter::Bridge(diff.id),
                    diff.path,
                    diff.base,
                    Some(diff.proposed),
                    cx,
                ),
                BridgeEvent::Close(id) => self.drop_proposal(id, cx),
            }
        }
    }

    /// Shows a proposed file in the review of the editor. Nothing is written until every
    /// hunk is decided.
    fn add_proposal(
        &mut self,
        waiter: Waiter,
        path: PathBuf,
        base: Option<String>,
        proposed: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let relative = crate::git::relative_of(&self.root, &path);
        let id = self.next_proposal;
        self.next_proposal += 1;
        let proposal = Proposal::new(id, waiter, path.clone(), relative, base, proposed);
        if proposal.review.hunks.is_empty() {
            if proposal.review.current.is_none() && proposal.review.base.is_some() {
                self.proposals.push(proposal);
                self.resolve_proposal(self.proposals.len() - 1, cx);
            } else {
                self.answer_empty(&proposal, cx);
            }
            return;
        }
        self.proposals.push(proposal);
        let window = self.window;
        let workspace = self.workspace.clone();
        cx.defer(move |cx| {
            window
                .update(cx, |_, window, cx| {
                    workspace
                        .update(cx, |workspace, cx| workspace.open_file(path, window, cx))
                        .ok()
                })
                .ok();
        });
        cx.notify();
    }

    /// The proposal has nothing to show, for example the same text as the file has: the
    /// client hears it at once.
    fn answer_empty(&mut self, proposal: &Proposal, cx: &mut Context<Self>) {
        match proposal.waiter {
            Waiter::Tool(request) => self.edits.resolve(
                request,
                EditReply::Proposal {
                    outcome: EditOutcome::Accepted,
                    text: proposal.review.current.clone(),
                    hunks: 0,
                    kept: 0,
                },
            ),
            Waiter::Bridge(id) => {
                if let Some(bridge) = bridge_of(cx) {
                    bridge.resolve(id, DiffDecision::Reject);
                }
            }
        }
    }

    /// The harness took a proposal back: nothing is left to decide.
    fn drop_proposal(&mut self, bridge_id: u64, cx: &mut Context<Self>) {
        let Some(position) = self
            .proposals
            .iter()
            .position(|proposal| proposal.waiter == Waiter::Bridge(bridge_id))
        else {
            return;
        };
        self.proposals.remove(position);
        self.reload_review(cx);
        cx.notify();
    }

    /// Shows the files a rename changes and waits for Apply or Cancel.
    fn preview_rename(
        &mut self,
        id: u64,
        new_name: String,
        change: WorkspaceChange,
        cx: &mut Context<Self>,
    ) {
        if self.pending_rename.is_some() {
            self.edits.resolve(
                id,
                EditReply::Failed("another rename waits for the user".to_string()),
            );
            return;
        }
        let files = rename_files(&change, &self.root);
        let edits = change
            .edits
            .iter()
            .map(|source| source.edits.len())
            .sum::<usize>()
            + change.renames.len();
        let list = files
            .iter()
            .map(|file| format!("\n  {}", file.display()))
            .collect::<String>();
        let shown = files.len();
        let text = format!("Rename to `{new_name}`: {edits} changes in {shown} files{list}");
        self.pending_rename = Some(PendingRename {
            id,
            change,
            files,
            edits,
        });
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.notify_user_with_commands(
                    "Rename symbol",
                    text,
                    vec![
                        ("apply", SharedString::from("Apply"), "ai.mcp.rename-apply"),
                        (
                            "cancel",
                            SharedString::from("Cancel"),
                            "ai.mcp.rename-cancel",
                        ),
                    ],
                    cx,
                )
            })
            .ok();
        cx.notify();
    }

    /// The Apply or Cancel button of a rename preview.
    pub(crate) fn answer_rename(&mut self, apply: bool, cx: &mut Context<Self>) {
        let Some(pending) = self.pending_rename.take() else {
            return;
        };
        if !apply {
            self.edits.resolve(pending.id, EditReply::Cancelled);
            cx.notify();
            return;
        }
        let reply = EditReply::Rename {
            files: pending.files,
            edits: pending.edits,
        };
        self.defer_apply(pending.id, pending.change, reply, cx);
        cx.notify();
    }

    /// Opens a file of a tool after the current update: an editor cannot open while the
    /// window is being updated.
    fn defer_open(&self, id: u64, path: PathBuf, cx: &mut Context<Self>) {
        let window = self.window;
        let workspace = self.workspace.clone();
        let edits = self.edits.clone();
        cx.defer(move |cx| {
            let opened = window
                .update(cx, |_, window, cx| {
                    workspace
                        .update(cx, |workspace, cx| workspace.open_file(path, window, cx))
                        .ok()
                })
                .ok()
                .flatten()
                .is_some();
            let reply = if opened {
                EditReply::Applied { changed: false }
            } else {
                EditReply::Failed("the window is gone".to_string())
            };
            edits.resolve(id, reply);
        });
    }

    /// Applies a change of the language service after the current update is over: an edit
    /// of the window cannot open while the window is being updated.
    fn defer_apply(
        &self,
        id: u64,
        change: WorkspaceChange,
        reply: EditReply,
        cx: &mut Context<Self>,
    ) {
        let window = self.window;
        let workspace = self.workspace.clone();
        let edits = self.edits.clone();
        cx.defer(move |cx| {
            let applied = window.update(cx, |_, window, cx| {
                workspace
                    .update(cx, |workspace, cx| {
                        crate::changes::apply(workspace, change, window, cx);
                    })
                    .ok()
            });
            let reply = if applied.is_ok() {
                reply
            } else {
                EditReply::Failed("the window is gone".to_string())
            };
            edits.resolve(id, reply);
        });
    }

    /// The hunks of the code that still wait for the user, for the marks of the gutter.
    fn proposal_hunk_at(&self, path: &Path, line: u32) -> Option<(u64, usize)> {
        for proposal in &self.proposals {
            if proposal.path != path {
                continue;
            }
            let undecided = proposal.undecided();
            for (index, hunk) in proposal.review.hunks.iter().enumerate() {
                if !undecided.contains(&index) {
                    continue;
                }
                let hit = if hunk.before.is_empty() {
                    hunk.before.start == line
                } else {
                    hunk.before.contains(&line)
                };
                if hit {
                    return Some((proposal.id, index));
                }
            }
        }
        None
    }

    /// Paints the marks of the undecided hunks of a proposal and puts the bar above the
    /// file. A proposed file beats the marks of a written change.
    fn refresh_proposal_marks(&mut self, cx: &mut Context<Self>) {
        let theme = *cx.global::<Theme>();
        let mut targets: HashMap<PathBuf, (String, Vec<GutterMark>)> = HashMap::new();
        for proposal in &self.proposals {
            let marks: Vec<GutterMark> = proposal
                .undecided()
                .into_iter()
                .map(|index| {
                    let hunk = &proposal.review.hunks[index];
                    let kind = if hunk.before.is_empty() {
                        GutterKind::Added
                    } else if hunk.after.is_empty() {
                        GutterKind::Deleted
                    } else {
                        GutterKind::Modified
                    };
                    GutterMark {
                        lines: hunk.before.clone(),
                        kind,
                        color: theme.stripe_agent,
                    }
                })
                .collect();
            if !marks.is_empty() {
                targets.insert(proposal.path.clone(), (proposal.relative.clone(), marks));
            }
        }
        for view in OpenEditors::all(cx) {
            let path = view.read(cx).path().to_path_buf();
            match targets.get(&path) {
                Some((relative, marks)) => {
                    if !view.read(cx).is_dirty() {
                        view.update(cx, |view, cx| view.set_agent_marks(marks.clone(), cx));
                    }
                    self.pending.insert(
                        path.clone(),
                        PendingFile {
                            path: relative.clone(),
                            hunks: marks.len(),
                        },
                    );
                    self.proposal_marked.insert(path);
                }
                None => {
                    if self.proposal_marked.remove(&path) {
                        view.update(cx, |view, cx| view.set_agent_marks(Vec::new(), cx));
                        let relative = crate::git::relative_of(&self.root, &path);
                        if !self
                            .changes
                            .session
                            .iter()
                            .any(|change| change.path == relative)
                        {
                            self.pending.remove(&path);
                        }
                    }
                }
            }
        }
    }

    /// Accepts or rejects one hunk of a proposed file.
    pub(crate) fn decide_proposal_hunk(
        &mut self,
        id: u64,
        index: usize,
        accepted: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(position) = self.proposals.iter().position(|proposal| proposal.id == id) else {
            return;
        };
        if !self.proposals[position].decide(index, accepted) {
            return;
        }
        if self.proposals[position].decided() {
            self.resolve_proposal(position, cx);
        } else {
            self.reload_review(cx);
            cx.notify();
        }
    }

    fn decide_proposal_all(&mut self, id: u64, accepted: bool, cx: &mut Context<Self>) {
        let Some(position) = self.proposals.iter().position(|proposal| proposal.id == id) else {
            return;
        };
        self.proposals[position].decide_all(accepted);
        self.resolve_proposal(position, cx);
    }

    /// Every hunk is decided: the text the user kept goes to disk, and whoever waits hears
    /// what happened. A file changed under the review is never overwritten.
    fn resolve_proposal(&mut self, position: usize, cx: &mut Context<Self>) {
        let Some((id, waiter, hunks, kept, text, path, base, unchanged)) =
            self.proposals.get(position).map(|proposal| {
                (
                    proposal.id,
                    proposal.waiter,
                    proposal.review.hunks.len(),
                    proposal.kept(),
                    proposal.text(),
                    proposal.path.clone(),
                    proposal.review.base.clone().map(String::into_bytes),
                    proposal.review.base.clone(),
                )
            })
        else {
            return;
        };
        if hunks > 0 && kept == 0 {
            match waiter {
                Waiter::Tool(request) => self.edits.resolve(
                    request,
                    EditReply::Proposal {
                        outcome: EditOutcome::Rejected,
                        text: unchanged,
                        hunks,
                        kept: 0,
                    },
                ),
                Waiter::Bridge(bridge_id) => {
                    if let Some(bridge) = bridge_of(cx) {
                        bridge.resolve(bridge_id, DiffDecision::Reject);
                    }
                }
            }
            self.proposals.remove(position);
            self.reload_review(cx);
            cx.notify();
            return;
        }
        if OpenEditors::is_dirty(&path, cx) {
            self.review.notice =
                Some("Not written: the editor has unsaved edits. Save or undo them first.".into());
            match waiter {
                Waiter::Tool(request) => self.edits.resolve(
                    request,
                    EditReply::Failed(
                        "the editor has unsaved edits, nothing was written".to_string(),
                    ),
                ),
                Waiter::Bridge(bridge_id) => {
                    if let Some(bridge) = bridge_of(cx) {
                        bridge.resolve(bridge_id, DiffDecision::Reject);
                    }
                }
            }
            self.proposals.remove(position);
            self.reload_review(cx);
            cx.notify();
            return;
        }
        let outcome = if kept == hunks {
            EditOutcome::Accepted
        } else {
            EditOutcome::Partial
        };
        let written = text.clone();
        let write_path = path.clone();
        let undo_base = base.clone();
        let history = crate::history::get(cx);
        let deleting = written.is_none();
        let job = cx.background_executor().spawn(async move {
            let current = std::fs::read(&write_path).ok();
            if current != base {
                return Err("the file changed on disk".to_string());
            }
            // The Local History keeps what is about to be replaced or removed, so the
            // window can undo the edit or show it later.
            if let Some(history) = &history {
                let cause = if deleting {
                    local_history::Cause::BeforeDelete
                } else {
                    local_history::Cause::BeforeReplace
                };
                let _ = history.record_tree(&write_path, cause);
            }
            match written {
                Some(text) => {
                    if let Some(parent) = write_path.parent() {
                        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
                    }
                    std::fs::write(&write_path, text.as_bytes()).map_err(|error| error.to_string())
                }
                None => match std::fs::remove_file(&write_path) {
                    Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                        Err(error.to_string())
                    }
                    _ => {
                        if let Some(history) = &history {
                            let _ = history.record_removed(&write_path);
                        }
                        Ok(())
                    }
                },
            }
        });
        cx.spawn(async move |this, cx| {
            let result = job.await;
            this.update(cx, |this, cx| {
                if let Some(position) = this.proposals.iter().position(|proposal| proposal.id == id)
                {
                    this.proposals.remove(position);
                }
                match result {
                    Ok(()) => {
                        if text.is_none()
                            && let Some(base) = undo_base
                        {
                            let target = path.clone();
                            this.last_deleted = Some((target.clone(), base));
                            this.offer_undo_delete(&target, cx);
                        }
                        match waiter {
                            Waiter::Tool(request) => this.edits.resolve(
                                request,
                                EditReply::Proposal {
                                    outcome,
                                    text: text.clone(),
                                    hunks,
                                    kept,
                                },
                            ),
                            Waiter::Bridge(bridge_id) => {
                                if let Some(bridge) = bridge_of(cx) {
                                    bridge.resolve(
                                        bridge_id,
                                        DiffDecision::Accept(text.unwrap_or_default()),
                                    );
                                }
                            }
                        }
                        OpenEditors::reload(&path, cx);
                        if let Some(tracker) = this.tracker() {
                            tracker.refresh();
                        }
                    }
                    Err(error) => {
                        this.review.notice = Some(format!("Not written: {error}"));
                        match waiter {
                            Waiter::Tool(request) => {
                                this.edits.resolve(request, EditReply::Failed(error));
                            }
                            Waiter::Bridge(bridge_id) => {
                                if let Some(bridge) = bridge_of(cx) {
                                    bridge.resolve(bridge_id, DiffDecision::Reject);
                                }
                            }
                        }
                    }
                }
                this.reload_review(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Offers the Undo of a file the user just deleted, as the notification of the
    /// window does.
    fn offer_undo_delete(&mut self, path: &Path, cx: &mut Context<Self>) {
        let relative = crate::git::relative_of(&self.root, path);
        let text = format!("Deleted {relative}");
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.notify_user_with_commands(
                    "Delete",
                    text,
                    vec![("undo", SharedString::from("Undo"), "ai.mcp.undo-delete")],
                    cx,
                )
            })
            .ok();
    }

    /// Puts the file of the last delete back, as the Undo of the notification does.
    pub(crate) fn undo_delete(&mut self, cx: &mut Context<Self>) {
        let Some((path, bytes)) = self.last_deleted.take() else {
            return;
        };
        if path.exists() {
            return;
        }
        let history = crate::history::get(cx);
        let write_path = path.clone();
        let job = cx.background_executor().spawn(async move {
            if let Some(parent) = write_path.parent() {
                std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
            }
            std::fs::write(&write_path, &bytes).map_err(|error| error.to_string())?;
            if let Some(history) = &history {
                let _ = history.record(
                    &write_path,
                    Some(&bytes),
                    local_history::Cause::BeforeRestore,
                );
            }
            Ok::<(), String>(())
        });
        cx.spawn(async move |this, cx| {
            let written = job.await;
            this.update(cx, |this, cx| {
                if written.is_ok() {
                    OpenEditors::reload(&path, cx);
                    if let Some(panel) = this.tool_workspace_project(cx) {
                        panel.update(cx, |panel, cx| panel.reload(cx));
                    }
                    if let Some(tracker) = this.tracker() {
                        tracker.refresh();
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn tool_workspace_project(&self, cx: &App) -> Option<Entity<crate::project::ProjectPanel>> {
        cx.try_global::<crate::project::ActiveProjectPanel>()
            .map(|panel| panel.0.clone())
    }

    /// Accept, reject or walk the hunks of a proposed file.
    pub(crate) fn act_on_proposal(
        &mut self,
        position: usize,
        action: Action,
        cx: &mut Context<Self>,
    ) {
        let Some(proposal) = self.proposals.get(position) else {
            return;
        };
        let id = proposal.id;
        let undecided = proposal.undecided();
        match action {
            Action::AcceptHunk(index) => {
                if let Some(original) = undecided.get(index).copied() {
                    self.decide_proposal_hunk(id, original, true, cx);
                }
            }
            Action::RevertHunk(index) => {
                if let Some(original) = undecided.get(index).copied() {
                    self.decide_proposal_hunk(id, original, false, cx);
                }
            }
            Action::AcceptFile | Action::AcceptAll => self.decide_proposal_all(id, true, cx),
            Action::RevertFile | Action::RevertAll => self.decide_proposal_all(id, false, cx),
        }
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

    /// Names the transcript of the next mock session, for a demo without a model. A build
    /// without the mock agents ignores it.
    pub(crate) fn set_mock_transcript(&mut self, path: PathBuf) {
        self.mock_transcript = Some(path);
    }

    /// Sends a plain message to the active session for a script, as the message box sends
    /// text without mentions. `false` while no session takes it yet, an error when the
    /// session refuses it for good.
    pub(crate) fn prompt(&mut self, text: &str, cx: &mut Context<Self>) -> Result<bool, String> {
        let text = text.trim();
        if text.is_empty() {
            return Ok(true);
        }
        let Some(entry) = self.active_entry_mut() else {
            return Ok(false);
        };
        entry
            .session
            .prompt(text)
            .map_err(|error| error.to_string())?;
        entry.sent += 1;
        cx.notify();
        Ok(true)
    }

    /// Sends a finished message to the chat of the window, or starts the usual chat
    /// session and sends it when that one is up. A terminal of a TUI is not a chat, so
    /// the message opens a session of its own, as Ask AI does.
    pub(crate) fn ask_message(
        &mut self,
        message: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let chats = |kind: &AgentKind| matches!(kind, AgentKind::Acp(_));
        let needs_session = self
            .active_entry()
            .is_none_or(|entry| entry.terminal.is_some());
        // A chat that is already starting takes the message when it is up.
        let starting_chat = self.starting.as_ref().is_some_and(chats);
        if needs_session
            && (self.starting.is_none() || starting_chat)
            && let Some(kind) = self.kinds().into_iter().find(chats)
        {
            self.pending_message = Some(message);
            if self.starting.is_none() {
                self.start(kind, window, cx);
            }
        } else {
            // The caller may be an update of the window, which the message reads: it
            // goes out after that update.
            cx.defer_in(window, move |this, _, cx| this.send_finished(&message, cx));
        }
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// Starts a session of `kind`, asking first when the project is not trusted yet.
    pub fn request(&mut self, kind: AgentKind, window: &mut Window, cx: &mut Context<Self>) {
        self.agents_md = None;
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
        let (mcp, root) = (self.mcp.clone(), self.root.clone());
        let tools = self.tools.clone();
        let approvals = self.approvals.clone();
        let options = self.options.clone();
        let environment = self.environment.clone();
        let transcript = self.mock_transcript.clone();
        let ide = cx
            .try_global::<crate::claude_ide::ActiveClaude>()
            .map(crate::claude_ide::ActiveClaude::environment)
            .unwrap_or_default();
        let task = cx.background_executor().spawn(async move {
            launch(
                kind,
                installed,
                mcp,
                tools,
                approvals,
                root,
                options,
                environment,
                ide,
                id,
                transcript,
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
                self.mcp = Some(entry.services.mcp.clone());
                self.services = Some(entry.services.clone());
                self.entries.push(entry);
                self.active = Some(self.entries.len() - 1);
                self.tab = Tab::Session;
                window.focus(&self.focus, cx);
                if let Some(message) = self.pending_message.take() {
                    self.send_finished(&message, cx);
                }
            }
            Err(error) => {
                self.pending_message = None;
                self.error = Some(format!("The session did not start: {error}").into());
            }
        }
        cx.notify();
    }

    fn select(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.agents_md = None;
        self.active = Some(index);
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn close(&mut self, index: usize, cx: &mut Context<Self>) {
        self.agents_md = None;
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

    /// The entry point the window published for the clients outside it.
    #[cfg(all(test, windows))]
    pub(crate) fn entry_point(&self) -> Option<EntryPoint> {
        self.mcp.as_ref()?.entry_point()
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
                    if this.proposals.iter().any(|proposal| proposal.path == path) {
                        // A proposed file shows its own hunks until the user decides.
                        continue;
                    }
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

    /// Gives the tools of the agents the latest files, open buffers and project model.
    /// Called every frame, so a tool call from a worker sees the window as it is. The
    /// snapshot of an open document is its version, its unsaved state and the document
    /// itself: no text is copied here, and a tool reads it only when it asks.
    pub(crate) fn sync_tools(&mut self, cx: &mut Context<Self>) {
        let files = self.files.read(cx).clone();
        self.tools.update_files(files);
        let documents: Vec<OpenDocument> = OpenEditors::all(cx)
            .into_iter()
            .filter_map(|view| {
                let view = view.read(cx);
                let version = view.version()?;
                let document = view.document()?;
                Some(OpenDocument {
                    path: view.path().to_path_buf(),
                    version,
                    dirty: view.is_dirty(),
                    document,
                })
            })
            .collect();
        self.tools.update_open(documents);
        if let Some(runs) = cx.try_global::<crate::script::ActiveRunPanel>() {
            let model = runs.0.read(cx).model();
            self.tools.update_model(model);
        }
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.sync_tools(cx);
        self.show_mcp_question(cx);
        self.take_edit_requests(cx);
        self.take_proposals(cx);
        crate::claude_ide::publish_view(&self.workspace, cx, &mut self.view_key);
        self.refresh_proposal_marks(cx);
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
        self.refresh_proposal_marks(cx);
        if changed {
            cx.notify();
        }
        let mcp = self
            .services
            .as_ref()
            .map(|services| &services.mcp)
            .or(self.mcp.as_ref());
        let (clients, busy) = match mcp {
            Some(mcp) => (
                mcp.gateway.clients().connected(),
                mcp.gateway.executing() > 0,
            ),
            None => (0, false),
        };
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.set_agent_clients(clients, busy, cx)
            })
            .ok();
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let stroke = &event.keystroke;
        let modifiers = stroke.modifiers;
        // Ctrl+C copies what is selected, as in the terminal window; without a selection it
        // goes on to the program. The lines of the Changes tab are copied the same way.
        let copy = modifiers.control && !modifiers.alt && stroke.key == "c";
        if self.tab == Tab::Changes {
            if copy && self.copy_review_lines(cx) {
                cx.stop_propagation();
            }
            return;
        }
        if self.tab != Tab::Session {
            return;
        }
        if let Some(terminal) = self.active_terminal().cloned() {
            if modifiers.control && modifiers.shift && stroke.key == "c" {
                cx.stop_propagation();
                self.copy_grid_selection(cx);
                return;
            }
            if copy && !modifiers.shift && self.copy_grid_selection(cx) {
                cx.stop_propagation();
                return;
            }
            if !terminal.is_closed() && send_key(&terminal, stroke, keymap::platform(), cx) {
                // What the user types takes the selection away, as it does in a terminal.
                self.clear_grid_selection();
                cx.stop_propagation();
            }
            return;
        }
        if self.active_entry().is_some() && self.compose_key(&event.keystroke, window, cx) {
            cx.stop_propagation();
        }
    }

    pub(crate) fn show_tab(&mut self, tab: Tab, cx: &mut Context<Self>) {
        self.agents_md = None;
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
        }
        let mcp = self
            .services
            .as_ref()
            .map(|services| &services.mcp)
            .or(self.mcp.as_ref());
        if let Some(mcp) = mcp {
            for call in mcp.gateway.audit().recent() {
                let client = call.client.as_deref().unwrap_or("-");
                lines.push(format!(
                    "tool-call {} client={client} ok={}",
                    call.tool, call.ok
                ));
            }
        }
        if let Some(note) = &self.agents_md {
            lines.push(format!("agents-md {}", note.text));
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

    /// The row under the header: the button that starts a session, and on its right the button
    /// of AGENTS.md. The result of that button sits in a row of its own under them.
    fn start_row(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let row = div()
            .flex()
            .flex_wrap()
            .items_center()
            .justify_between()
            .gap(px(SPACE_M))
            .px(px(SPACE_M))
            .pb(px(SPACE_S))
            .child(self.new_session_button(theme, cx))
            .child(
                text_button("agents-md", "Add IDE tools to AGENTS.md", theme)
                    .debug_selector(|| "agents-md".to_owned())
                    .on_click(cx.listener(|this, _, _, cx| this.add_agents_md(cx))),
            );
        let result = self.agents_md.as_ref().map(|note| {
            let color = if note.failed {
                theme.close_hover
            } else {
                theme.text_secondary
            };
            div()
                .id("agents-md-result")
                .debug_selector(|| "agents-md-result".to_owned())
                .px(px(SPACE_M))
                .pb(px(SPACE_S))
                .min_w_0()
                .truncate()
                .text_color(color)
                .child(note.text.clone())
        });
        div()
            .flex()
            .flex_col()
            .flex_none()
            .child(row)
            .children(result)
    }

    /// The button that opens the menu of the agents that can start here.
    fn new_session_button(&self, theme: &Theme, cx: &mut Context<Self>) -> Stateful<Div> {
        let (hover, pressed) = (theme.hover, theme.pressed);
        div()
            .id("agent-new-session")
            .debug_selector(|| "agent-new-session".to_owned())
            .flex()
            .items_center()
            .gap(px(SPACE_S))
            .h(px(24.))
            .px(px(SPACE_M))
            .rounded(px(CONTROL_RADIUS))
            .border_1()
            .border_color(theme.control_border)
            .hover(move |style| style.bg(hover))
            .active(move |style| style.bg(pressed))
            .child(icon("general/add", theme))
            .child("New Session")
            .child(icon("general/chevronDown", theme))
            .on_click(cx.listener(|this, _, window, cx| this.new_session_menu(window, cx)))
    }

    /// Opens the menu of the agents at the pointer: the harnesses on PATH, a separator, and the
    /// ACP agents, with the mock of the tests last.
    fn new_session_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let panel = cx.entity();
        let (harnesses, others): (Vec<AgentKind>, Vec<AgentKind>) = self
            .kinds()
            .into_iter()
            .partition(|kind| matches!(kind, AgentKind::Tui(_)));
        let mut entries: Vec<MenuEntry> = harnesses
            .into_iter()
            .map(|kind| start_entry(&panel, kind))
            .collect();
        if !entries.is_empty() && !others.is_empty() {
            entries.push(MenuEntry::separator());
        }
        entries.extend(others.into_iter().map(|kind| start_entry(&panel, kind)));
        if entries.is_empty() {
            entries.push(MenuEntry::disabled("No agent can start here"));
        }
        let position = window.mouse_position();
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.show_menu(position, entries, cx)
            })
            .ok();
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

    /// The tabs of the session: the terminal or the chat, the changes and the agents of the
    /// window, with their counts as badges. The row has a line above and below it.
    fn tabs(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let session = match self.active_entry() {
            Some(entry) if entry.terminal.is_some() => "Terminal",
            _ => "Chat",
        };
        let changes = self.changes.session.len();
        let agents = self.agents_count();
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(SPACE_XS))
            .px(px(SPACE_M))
            .py(px(SPACE_XS))
            .border_t_1()
            .border_b_1()
            .border_color(theme.border)
            .child(session_tab(
                "tab-session",
                session,
                None,
                self.tab == Tab::Session,
                theme,
                cx.listener(|this, _, _, cx| this.show_tab(Tab::Session, cx)),
            ))
            .child(session_tab(
                "tab-changes",
                "Changes",
                Some(changes),
                self.tab == Tab::Changes,
                theme,
                cx.listener(|this, _, _, cx| this.show_tab(Tab::Changes, cx)),
            ))
            .child(session_tab(
                "tab-agents",
                "Agents",
                Some(agents),
                self.tab == Tab::Agents,
                theme,
                cx.listener(|this, _, _, cx| this.show_tab(Tab::Agents, cx)),
            ))
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
        let path = view.read(cx).path().to_path_buf();
        let relative = crate::git::relative_of(&self.root, &path);
        if let Some((id, index)) = self.proposal_hunk_at(&path, line) {
            let Some(review) = self
                .proposals
                .iter()
                .find(|proposal| proposal.id == id)
                .map(|proposal| proposal.review.clone())
            else {
                return;
            };
            let owner = crate::agent_hunk::Owner::Proposal {
                id,
                panel: cx.weak_entity(),
            };
            let workspace = self.workspace.clone();
            let root = self.root.clone();
            window
                .update(cx, |_, window, cx| {
                    workspace
                        .update(cx, |workspace, cx| {
                            crate::agent_hunk::AgentHunkPopup::open(
                                workspace, window, cx, root, relative, view, owner, review, index,
                            )
                        })
                        .ok()
                })
                .ok();
            return;
        }
        let Some(tracker) = self.tracker().cloned() else {
            return;
        };
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
                                        workspace,
                                        window,
                                        cx,
                                        root,
                                        relative,
                                        view,
                                        crate::agent_hunk::Owner::Tracker(tracker),
                                        review,
                                        index,
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

impl AgentsPanel {
    /// Copies the selection of the terminal of the active session; false without one.
    fn copy_grid_selection(&self, cx: &mut Context<Self>) -> bool {
        let Some(entry) = self.active_entry() else {
            return false;
        };
        let (Some(session), selection) = (entry.terminal.as_ref(), entry.selection) else {
            return false;
        };
        crate::term_grid::copy_selection(session, selection, cx)
    }

    fn clear_grid_selection(&mut self) {
        if let Some(entry) = self.active_entry_mut() {
            entry.selection = None;
        }
    }

    /// Sends the clipboard to the terminal of the active session, as Ctrl+V does.
    fn paste_grid_text(&mut self, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        let Some(session) = self.active_terminal().cloned() else {
            return;
        };
        let _ = session.paste(&text);
        self.clear_grid_selection();
        cx.notify();
    }

    /// The text that the terminal of the active session has selected, for tests.
    #[cfg(all(test, windows))]
    pub(crate) fn grid_selection_text(&self) -> Option<String> {
        let entry = self.active_entry()?;
        let session = entry.terminal.as_ref()?;
        let selection = entry.selection?;
        Some(crate::term_grid::selection_text(
            &session.screen(),
            selection,
        ))
    }

    /// Replaces the harnesses on PATH with `installs`, so that a test starts a stand-in program
    /// and never a real harness.
    #[cfg(test)]
    pub(crate) fn use_installs_for_test(&mut self, installs: Vec<Installed>) {
        self.installs = installs;
    }
}

impl GridPanel for AgentsPanel {
    fn pane_focus(&self, _pane: usize) -> FocusHandle {
        self.focus.clone()
    }

    fn grid_pressed(
        &mut self,
        _pane: usize,
        position: Position,
        clicks: usize,
        cx: &mut Context<Self>,
    ) {
        let Some(entry) = self.active_entry_mut() else {
            return;
        };
        let Some(session) = entry.terminal.clone() else {
            return;
        };
        (entry.selection, entry.dragging) =
            crate::term_grid::pressed(&session.screen(), position, clicks);
        cx.notify();
    }

    fn grid_moved(
        &mut self,
        _pane: usize,
        position: Option<Position>,
        _control: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(entry) = self.active_entry_mut() else {
            return false;
        };
        if let (Some(pressed), Some(position)) = (entry.dragging, position) {
            entry.selection = Some(crate::term_grid::Selection::new(pressed, position));
            cx.notify();
        }
        false
    }

    fn grid_released(&mut self, _pane: usize, cx: &mut Context<Self>) {
        if let Some(entry) = self.active_entry_mut() {
            entry.dragging = None;
        }
        cx.notify();
    }

    fn grid_context_menu(&mut self, _pane: usize, position: Point<Pixels>, cx: &mut Context<Self>) {
        let panel = cx.entity();
        let copy = panel.clone();
        let paste = panel.clone();
        let has_selection = self
            .active_entry()
            .is_some_and(|entry| entry.selection.is_some());
        let copy_entry = if has_selection {
            MenuEntry::new("Copy", move |_, cx| {
                copy.update(cx, |this, cx| {
                    this.copy_grid_selection(cx);
                });
            })
        } else {
            MenuEntry::disabled("Copy")
        };
        let entries = vec![
            copy_entry,
            MenuEntry::new("Paste", move |_, cx| {
                paste.update(cx, |this, cx| this.paste_grid_text(cx));
            }),
        ];
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.show_menu(position, entries, cx)
            })
            .ok();
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

/// The files a rename edits or moves, relative to the project and without repeats.
fn rename_files(change: &WorkspaceChange, root: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = Vec::new();
    let mut add = |uri: &str| {
        let Some(path) = editor_view::uri_path(uri) else {
            return;
        };
        let relative = PathBuf::from(crate::git::relative_of(root, &path));
        if !files.contains(&relative) {
            files.push(relative);
        }
    };
    for source in &change.edits {
        if !source.edits.is_empty() {
            add(&source.uri);
        }
    }
    for rename in &change.renames {
        add(&rename.from);
        add(&rename.to);
    }
    files
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
        let options =
            crate::tool_menu::options_menu(self.workspace.clone(), workspace::AI_TOOL, |_| {
                Vec::new()
            });
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
            (Tab::Agents, _) => body.child(self.clients_view(&theme, window, cx)),
            (Tab::Changes, _) => body.child(self.changes_view(&theme, window, cx)),
            (Tab::Session, Some(terminal)) => {
                let scrolled = terminal.clone();
                let view = GridView {
                    session: terminal.clone(),
                    focused,
                    theme,
                    selection: self.active_entry().and_then(|entry| entry.selection),
                    matches: Vec::new(),
                    current: None,
                    hover: None,
                    host: Some(cx.entity()),
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
            .child(panel_header(
                "AI Sessions",
                GROUP,
                "ai",
                options,
                hide,
                &theme,
            ))
            .child(self.start_row(&theme, cx))
            .children(error)
            .child(self.session_rows(&theme, cx))
            .child(self.tabs(&theme, cx))
            .child(body)
    }
}

#[cfg(feature = "mock-agents")]
mod mock {
    use super::*;
    use agent_testkit::{MOCK_MODEL, MockApi, Sandbox, claude_env, codex_env};

    struct Home {
        _sandbox: Sandbox,
        _api: MockApi,
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

    pub fn launch(_: Harness, _: &Path, _: usize) -> Result<(Launch, Box<dyn Send>), String> {
        Err(MISSING.to_string())
    }

    pub fn acp_env(_: &Preset, _: &Path, _: usize) -> Result<(Env, Kept), String> {
        Err(MISSING.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::{AgentKind, missing_api_key, project_key};
    use agent_acp::Preset;
    use std::ffi::OsStr;
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

    #[test]
    fn an_acp_agent_that_needs_a_key_says_so_without_one() {
        let message = missing_api_key(&Preset::ClaudeAgent, None).unwrap();
        assert!(message.contains("ANTHROPIC_API_KEY"), "{message}");
        assert!(missing_api_key(&Preset::ClaudeAgent, Some(OsStr::new(""))).is_some());
        assert!(missing_api_key(&Preset::ClaudeAgent, Some(OsStr::new("key"))).is_none());
        assert!(missing_api_key(&Preset::Codex, None).is_none());
    }
}
