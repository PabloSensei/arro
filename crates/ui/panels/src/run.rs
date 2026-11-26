//! The Run tool window: configurations found in the project, the console of the last run,
//! Run and Stop. What a configuration means belongs to its provider.

use std::collections::VecDeque;
use std::path::Path as StdPath;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::time::{Duration, Instant};

use ::base::Cancel;
use dap_client::ReloadOutcome;
use editor_view::{Semantic, Spot};
use futures_util::StreamExt;
use lang_api::{DumpQuery, ask};
use proc_sup::ProcessSpec;
use project_model::{
    BuildModule, BuildSelection, BuildSystem, EnvironmentProvider, ProjectModel, ProjectSession,
    SyncEvent, TaskOptions, ToolHome, ToolProgress, ToolSetup, open_project,
};
use runner::{
    BeforeLaunchStep, Console, ConsoleEvent, CoverageOwner, FindTests, LiveApplication,
    ProfileOwner, RunConfig, RunConfigProvider, RunPlan, STOPPED_BY_USER, ServiceRun, ServiceState,
    Step, Stream, TestProvider, TestRequest, TestRun,
};
use tool_gateway::{
    BuildOutcome, BuildProblem, BuildRequest, RunConfiguration, RunOutput, StartRunRequest,
    TestCase, TestOutcome, TestsOutcome, TestsRequest,
};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::{DEBUG_TOOL, RUN_TOOL, RunControls, RunState, STACK_TRACE_TOOL, Workspace};
use workspace_core::Delivery;

use crate::ansi::{Line, OutputBuffers, Style};
use crate::console_follow::ConsoleFollow;
use crate::console_input::{ConsoleInput, ConsoleKey};
use crate::console_links;
use crate::console_search::{ConsoleSearch, SearchToggle};
use crate::mcp_runs::{Answer, Job, Request as ToolRequest, RunBridge};
use crate::navigation;
use crate::stack_traces::ActiveStackTrace;

mod tests;

pub(crate) use tests::TestsChanged;
use tests::{TestView, title_of};

/// The lines the debugger's console printed for the live tab of a run under the
/// debugger. The debugger writes them without touching the Run panel; its poll reads them.
#[derive(Default)]
pub(crate) struct DebugOutput(pub(crate) std::sync::Mutex<Vec<String>>);

impl Global for DebugOutput {}

/// The list of run configurations changed, for the gutter marks of the open editors.
pub(crate) struct ConfigsChanged;

impl EventEmitter<ConfigsChanged> for RunPanel {}

/// The model of the project was read or replaced: the modules and the folders they
/// exclude are new.
pub(crate) struct ProjectModelChanged;

impl EventEmitter<ProjectModelChanged> for RunPanel {}

/// The coverage of the last run changed, or was hidden: the stripes of the editors and
/// the rows of the Coverage window follow it.
pub struct CoverageChanged;

impl EventEmitter<CoverageChanged> for RunPanel {}

/// The profile snapshot of the last run changed, or the recordings on disk did.
pub struct ProfileChanged;

impl EventEmitter<ProfileChanged> for RunPanel {}

/// The list of recordings on disk changed, without a new snapshot.
pub struct ProfileRecordingsChanged;

impl EventEmitter<ProfileRecordingsChanged> for RunPanel {}

/// How a project is built and run, and what it can create. The application fills it,
/// panels only pass it on.
#[derive(Clone)]
pub struct ProjectSetup {
    pub tool: Option<Arc<dyn BuildSystem>>,
    pub providers: Vec<Arc<dyn RunConfigProvider>>,
    /// Runs the tests of the project, `None` when the tool cannot.
    pub tests: Option<Arc<dyn TestProvider>>,
    /// Finds tests in the text of a source file, for the run marks.
    pub test_sites: FindTests,
    pub cache_root: PathBuf,
    /// New items the project tree offers besides a plain file and folder.
    pub templates: Vec<crate::tree_ops::FileTemplate>,
    /// Folders the TODO scan skips, the output folders of the build tool.
    pub skipped_folders: Vec<String>,
    /// Starts debug sessions; `None` when the language cannot debug.
    pub debug: Option<Arc<dyn dap_client::DebugService>>,
    /// The SDKs and the language level of the project, for Project Structure.
    pub environment: Option<Arc<dyn EnvironmentProvider>>,
}

/// How long a build step gets to finish after Stop before it is ended.
const TASK_GRACE: Duration = Duration::from_secs(5);

/// What the last build of the window was, so that Build window can run it again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum BuildKind {
    Project,
    /// The module of the file, or the whole project when no module holds it.
    Module(Option<PathBuf>),
    /// Recompile of the file.
    Compile(Option<PathBuf>),
    Rebuild,
    GenerateSources,
}

/// The last build as the Build window shows it.
#[derive(Clone, Debug, Default)]
pub(crate) struct BuildView {
    /// The build still runs.
    pub running: bool,
    /// A build ended, successfully or not.
    pub finished: bool,
    pub success: bool,
    /// The user stopped the build.
    pub stopped: bool,
    /// How long the build took, once it ended.
    pub duration: Option<Duration>,
    /// When the build started, so the window tells a new one from the last one.
    pub started: Option<Instant>,
    /// The title of the build, such as the tool and the command.
    pub title: String,
    /// The output of the build as the console shows it.
    pub lines: Vec<String>,
    /// What the build tool reported about the sources.
    pub problems: Vec<BuildProblem>,
}
const POLL: Duration = Duration::from_millis(50);
/// A build file that changes on disk waits this long for the others of the same batch,
/// as a branch switch makes many, and then the project is imported once.
const MODEL_RELOAD_DELAY: Duration = Duration::from_millis(500);
/// Console lines kept; older ones go first.
const MAX_LINES: usize = 50_000;
/// Tabs of earlier runs kept beside the current one.
const MAX_OLD_RUNS: usize = 4;
/// Temporary configurations kept: the oldest goes when another one appears.
const MAX_TEMPORARY: usize = 5;
/// Events handled in one poll, so that a flood of output does not freeze the window.
const EVENTS_PER_POLL: usize = 2_000;
/// How long a test holds the first look at a run, for a run that lasts that long.
#[cfg(test)]
const HOLD_LIMIT: Duration = Duration::from_secs(60);
/// The text a held console waits for. No program prints it, so the wait ends with the run.
#[cfg(test)]
const HOLD_MARKER: &str = "\u{0}the hold ends with the run";
const GROUP: &str = "bottom-panel";
const LINE_PAD: f32 = SPACE_L;

/// The field of a configuration that carries an override of a debug start, by the words
/// of its key. A provider that declares none refuses the override.
fn override_of(
    fields: &[runner::ConfigField],
    words: &[&str],
    value: &str,
    what: &str,
) -> Result<(String, String), String> {
    let found = fields.iter().find(|field| {
        let key = field.key.to_ascii_lowercase();
        words.iter().any(|word| key.contains(word))
    });
    match found {
        Some(field) => Ok((field.key.to_string(), value.to_string())),
        None => Err(format!(
            "the configuration does not allow {what} from a tool"
        )),
    }
}

pub struct RunPanel {
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    /// The console list and the rule that keeps it at the last line while the user
    /// reads the end.
    follow: ConsoleFollow,
    root: PathBuf,
    cache_root: PathBuf,
    /// The state folder of the window, for the file of a complete run log.
    state_dir: PathBuf,
    tool: Option<Arc<dyn BuildSystem>>,
    /// The answers of the user for the build: which named settings are on and which
    /// values it reads. The tool keeps them for the next command, the window shows them.
    selection: BuildSelection,
    providers: Vec<Arc<dyn RunConfigProvider>>,
    /// The SDKs of the project; it also carries a changed model to the language.
    environment: Option<Arc<dyn EnvironmentProvider>>,
    session: Option<ProjectSession>,
    sync: Option<workspace_core::Subscription<SyncEvent>>,
    loading: bool,
    /// The project may run its build tool and its configurations. A project that opens in
    /// Safe Mode keeps its tool and starts it once the user trusts the project.
    trusted: bool,
    /// The stored configurations of a project and the state of the window, also for what
    /// the import of another IDE remembers.
    store: Option<Arc<store::Store>>,
    configs: Vec<RunConfig>,
    /// Configurations from `.arro/run.toml`, in the file order.
    saved: Vec<RunConfig>,
    /// Configurations the providers found in the project.
    discovered: Vec<RunConfig>,
    /// The found configurations the list shows by itself, such as an application of a
    /// framework that starts a server. They are marked as temporary: the project has no
    /// configuration of that class yet.
    suggested: Vec<RunConfig>,
    /// The configurations of another IDE found in the project, as the last discovery read
    /// them, and the names of the ones the offer already named.
    foreign: Vec<runner::ForeignConfigs>,
    foreign_offered: Option<String>,
    /// Temporary configurations: started from a place or the keyboard and not written to
    /// the file. The oldest goes first.
    temporary: Vec<RunConfig>,
    /// Why the file could not be read. It is never overwritten while this is set.
    store_broken: Option<String>,
    selected: Option<usize>,
    /// The next run of a configuration with the overrides a tool asked for. It is not in
    /// the list and is not written to the file.
    pending_config: Option<RunConfig>,
    console: Option<Console>,
    /// The main process of the console is still running. Output may keep coming after it
    /// ends, but the panel is free for the next run.
    console_running: bool,
    /// Consoles whose process ended while something still holds their pipes. Their late
    /// output keeps filling the console of their own run.
    draining: Vec<DrainingConsole>,
    /// The old console the user is looking at; `None` is the run of this window.
    viewing: Option<usize>,
    /// The search bar of the console and the lines its filter keeps.
    console_search: ConsoleSearch,
    search_focus: FocusHandle,
    filter_focus: FocusHandle,
    /// Planning in the background, the console comes next.
    starting: bool,
    /// Cancels a debug test start that still waits for its JVM. Stop sets it.
    starting_tests: Option<Cancel>,
    /// Stop was asked for to run again.
    rerun: bool,
    /// Run was asked for while the project was still syncing: it starts when that ends.
    waiting: bool,
    /// What went wrong, kept for the console and for scripts.
    problems: Vec<String>,
    /// What the window offers while the build tool of the project is not installed.
    tool_setup: Option<ToolSetup>,
    /// The download of the missing build tool, until it ends.
    tool_download: Option<ToolDownload>,
    /// The bytes of the distribution of the wrapper the sync downloaded last shown.
    wrapper_written: u64,
    title: SharedString,
    lines: VecDeque<Line>,
    /// The unfinished lines of the writers of the main console, each process and stream
    /// on its own.
    output: OutputBuffers,
    /// The process whose output goes into the main console now.
    console_process: Option<u32>,
    /// The JVM of the program of the current run, when its step started. Dump Threads
    /// asks this process for the stacks of its threads.
    program_pid: Option<u32>,
    /// The last test run, see `run/tests.rs`.
    tests: TestView,
    test_provider: Option<Arc<dyn TestProvider>>,
    debug_service: Option<Arc<dyn dap_client::DebugService>>,
    find_tests: FindTests,
    /// The project files, to find the sources named in a failure.
    files: Option<Entity<Arc<Vec<PathBuf>>>>,
    /// Tests to run once what runs now has stopped.
    queued_tests: Option<TestRequest>,
    /// Tests to run once the project has loaded.
    waiting_tests: Option<TestRequest>,
    /// The last test run is under the debugger, so its output goes to the Debug console.
    debugging_tests: bool,
    /// The console runs Build Project, so a success may reload classes.
    building_project: bool,
    /// A Build Project of its own while a test run keeps the main console and its tree.
    build_console: Option<Console>,
    /// The build process is still running; its late output may come after it ends.
    build_running: bool,
    /// The last build action, for Restart Build in the Build window.
    last_build: Option<BuildKind>,
    /// What the Build window shows of the last build.
    build_view: BuildView,
    /// When the current or last build started.
    build_started: Option<Instant>,
    /// The verdict of the last build that ended and how long it took.
    build_verdict: Option<(bool, Duration)>,
    /// The user stopped the last build.
    build_stopped: bool,
    /// The lines of a build run with a console of its own: the test console keeps its
    /// own buffers, so an unfinished line of one process cannot join the other.
    build_output: OutputBuffers,
    /// The process whose output goes into the build console now.
    build_process: Option<u32>,
    /// The answer of a hot swap the last Build Project asked for.
    reload: Option<Receiver<ReloadOutcome>>,
    /// Bumped when a program of this panel ended and its console is fully read, or when
    /// a test run ended and its results are all in, for a test that waits for the
    /// process instead of a clock.
    #[cfg(test)]
    finished: u64,
    /// A test holds the first look of the poll at a run until the run is over; see `poll`.
    #[cfg(test)]
    observation_held: bool,
    /// Bumped on every build file that changed: only the import after the last of a
    /// batch runs.
    model_reload: u64,
    /// The line the user types into the console, when its program reads input.
    console_input: ConsoleInput,
    /// The program step of the console started; a build step before it reads nothing.
    program_started: bool,
    /// The coverage of the last run, when it has data and is not hidden.
    coverage: Option<Arc<lang_api::CoverageReport>>,
    /// Whether the stripes of the coverage are shown.
    coverage_visible: bool,
    /// A coverage run whose report comes when it ends.
    coverage_pending: Option<PendingCoverage>,
    /// A report of an older run that arrives after a newer one started is dropped.
    coverage_generation: u64,
    /// The profile snapshot of the last run, when one has been read.
    profile: Option<Arc<lang_api::ProfileSnapshot>>,
    /// Everything the recordings of the last profile run hold, the first one as the
    /// window shows it first.
    profile_report: Option<Arc<lang_api::ProfileReport>>,
    /// One entry per recording of the last run: a runtime may write several.
    profile_entries: Vec<ProfileEntry>,
    /// A profile run whose snapshot comes when it ends.
    profile_pending: Option<PendingProfile>,
    /// A snapshot of an older run that arrives after a newer one started is dropped.
    profile_generation: u64,
    /// Why the snapshot of the last profile run could not be read.
    profile_error: Option<String>,
    /// The recording of the last profile run, or the file the user opened.
    profile_recording: Option<PathBuf>,
    /// The recordings on disk the Profiler window lists.
    profile_recordings: Vec<lang_api::ProfileRecording>,
    /// The list of the recordings is being read in the background.
    recordings_loading: bool,
    /// Bumped when the snapshot, its file or its error changes, so a window that only
    /// hears about the list of recordings does not drop a snapshot it shows.
    profile_revision: u64,
    /// The live sections of the program of the current run, when its language has them.
    live: Option<Entity<crate::live_panel::LivePanel>>,
    /// The live tab is in front instead of the console.
    live_open: bool,
    /// The configuration whose console is in the window, for the Services window. It
    /// stays after the program ends, so the window shows the last state of that run.
    run_config: Option<String>,
    /// The state of the run of that configuration, fed by the events of its console.
    run_service: ServiceRun,
    /// A Run that stops the one that runs only after the user confirms it.
    confirm_rerun: Option<String>,
    /// Cancels a launch that is still planning in the background, as Stop does before
    /// the program of a configuration starts.
    launch: Cancel,
    /// The number of the latest launch. A plan of a launch that Stop cancelled, or one a
    /// newer launch overtook, starts nothing when it comes.
    launch_generation: u64,
    /// Tests run again by themselves after a project file is saved.
    auto_test: bool,
    /// The saves since the last automatic run: a burst of saves runs the tests once.
    auto_saves: u64,
    /// A save asked for a run while tests were going: it starts after they end.
    auto_pending: bool,
    /// The last test runs of the project, newest first, from the state folder.
    test_history: Vec<runner::TestHistoryRun>,
    /// The list of earlier runs is open in the test window.
    history_open: bool,
    /// The queue the MCP tools of the window leave their jobs in.
    tool_runs: Option<Arc<crate::mcp_runs::RunBridge>>,
    /// The build or test run an MCP tool asked for and still waits for.
    tool_job: Option<ToolJob>,
    /// The id of the output the window fills now or filled last, and whether it is a
    /// program rather than a build or a test run.
    tool_output: Option<(String, bool)>,
    /// The output of the runs an MCP tool asked for, the newest last, for `get_run_output`
    /// after the run has ended.
    tool_outputs: VecDeque<(String, tool_gateway::RunOutput)>,
    /// The exit code of the last process of a job an MCP tool asked for.
    tool_exit: Option<Option<i32>>,
    /// How many lines of the current job were written to its file last.
    tool_output_lines: usize,
}

/// The build or test run a tool of the MCP server asked for.
struct ToolJob {
    /// The id `get_run_output` reads this run by.
    output: String,
    kind: ToolJobKind,
    started: Instant,
    /// Whether the process of the job started. A plan that failed, and a program that could
    /// not start, leave none: the answer then says that the run did not start.
    process_started: bool,
    /// Why the process could not start, as the console reported it.
    start_error: Option<String>,
    /// The tool that waits for the end of this run.
    reply: std::sync::mpsc::Sender<Result<Answer, String>>,
}

enum ToolJobKind {
    Build,
    Tests,
}

/// The name of a recording file, for the list of several.
fn path_label(path: &StdPath) -> String {
    path.file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned())
}

/// One recording of a profile run as the Profiler window lists it.
#[derive(Clone)]
pub struct ProfileEntry {
    /// The name of the file, for the list of several recordings.
    pub label: String,
    pub report: Arc<lang_api::ProfileReport>,
}

/// The recording of a profile run and the provider that reads it once the run ends.
struct PendingProfile {
    recording: PathBuf,
    owner: ProfileOwner,
    generation: u64,
}

/// The data file of a coverage run and the provider that reads it once the run ends.
struct PendingCoverage {
    exec: PathBuf,
    owner: CoverageOwner,
    generation: u64,
}

/// A console whose main process ended, kept for the output that follows.
struct DrainingConsole {
    title: SharedString,
    /// `None` once its pipes have closed: the tab and its text stay.
    console: Option<Console>,
    lines: Vec<Line>,
    /// The last process of that console, for its unfinished line.
    process: Option<u32>,
    output: OutputBuffers,
    /// The configuration this run belongs to, for the Services window.
    config: Option<String>,
    /// The state of the run of that configuration.
    service: ServiceRun,
    /// Its main process is still going, as a child of a compound configuration or a
    /// second instance of one that allows them.
    running: bool,
}

/// The answer of a download of the missing build tool: the home it installed, or why it
/// did not.
type ToolDownloadAnswer = Result<Option<PathBuf>, String>;

/// What a job of the missing build tool does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MissingToolJob {
    /// Downloads the tool, or the distribution a wrapper names.
    Download,
    /// Writes the wrapper of the project with the tool.
    CreateWrapper,
    /// Uses the copy the IDE downloaded, without a wrapper.
    UseDownloaded,
}

/// A download of the missing build tool: the worker writes the bytes, the panel reads
/// them for the status line and the notification, and Stop cancels the transfer.
struct ToolDownload {
    name: String,
    job: MissingToolJob,
    progress: Arc<ToolProgress>,
    cancel: Cancel,
    done: Arc<AtomicBool>,
    result: Arc<Mutex<Option<ToolDownloadAnswer>>>,
    /// When the text of the notification was drawn last, so a fast network does not
    /// redraw it on every poll.
    drawn: Instant,
}

/// The text of a running download: the percentage when the server named the size, the
/// bytes otherwise.
fn download_text(name: &str, progress: &ToolProgress) -> String {
    match progress.total() {
        Some(total) if total > 0 => {
            let percent = (progress.written().saturating_mul(100) / total).min(100);
            format!("Downloading {name}: {percent}%")
        }
        _ => format!("Downloading {name}: {} KB", progress.written() / 1024),
    }
}

/// The commands behind the notification of a missing build tool. The buttons of the
/// balloon run them; they find the panel through the globals of the window.
pub(crate) fn bind_actions(workspace: &mut Workspace) {
    workspace.register_command(
        "build_tool.install",
        "Download Build Tool",
        |_workspace, _window, cx| {
            if let Some(panel) = cx.try_global::<crate::script::ActiveRunPanel>() {
                let panel = panel.0.clone();
                panel.update(cx, |panel, cx| panel.download_tool(cx));
            }
        },
    );
    workspace.register_command(
        "build_tool.cancel",
        "Cancel Build Tool Download",
        |_workspace, _window, cx| {
            if let Some(panel) = cx.try_global::<crate::script::ActiveRunPanel>() {
                let panel = panel.0.clone();
                panel.update(cx, |panel, cx| panel.cancel_tool_download(cx));
            }
        },
    );
    workspace.register_command(
        "build_tool.choose",
        "Choose Build Tool Home...",
        |_workspace, window, cx| {
            if let Some(panel) = cx.try_global::<crate::script::ActiveRunPanel>() {
                let panel = panel.0.clone();
                panel.update(cx, |panel, cx| panel.choose_tool_home(window, cx));
            }
        },
    );
    workspace.register_command(
        "build_tool.create_wrapper",
        "Create Build Tool Wrapper",
        |_workspace, _window, cx| {
            if let Some(panel) = cx.try_global::<crate::script::ActiveRunPanel>() {
                let panel = panel.0.clone();
                panel.update(cx, |panel, cx| panel.create_tool_wrapper(cx));
            }
        },
    );
    workspace.register_command(
        "build_tool.use_downloaded",
        "Use Downloaded Build Tool",
        |_workspace, _window, cx| {
            if let Some(panel) = cx.try_global::<crate::script::ActiveRunPanel>() {
                let panel = panel.0.clone();
                panel.update(cx, |panel, cx| panel.use_downloaded_tool(cx));
            }
        },
    );
    workspace.register_command(
        "settings.build_tools",
        "Build Tools Settings",
        |workspace, window, cx| {
            crate::settings::SettingsView::show_at(
                workspace,
                window,
                cx,
                Some("build_tool_first_home"),
            );
        },
    );
    workspace.register_command(
        "settings.build_tool_second",
        "Second Build Tool Settings",
        |workspace, window, cx| {
            crate::settings::SettingsView::show_at(
                workspace,
                window,
                cx,
                Some("build_tool_second_home"),
            );
        },
    );
}

impl RunPanel {
    pub fn create(
        root: PathBuf,
        setup: ProjectSetup,
        state_dir: PathBuf,
        store: Option<Arc<store::Store>>,
        workspace: WeakEntity<Workspace>,
        trusted: bool,
        cx: &mut App,
    ) -> Entity<Self> {
        cx.new(|cx| {
            let project_root = root.clone();
            let selection = setup
                .tool
                .as_ref()
                .map_or_else(BuildSelection::default, |tool| tool.selection(&root));
            let (session, sync) = match setup.tool.clone() {
                Some(tool) if trusted => {
                    let (session, sync) = open_project(tool, root, &setup.cache_root);
                    (Some(session), Some(sync))
                }
                _ => (None, None),
            };
            // The build tool window makes run configurations of its tasks on demand. The
            // provider writes none of them into the list by itself.
            let mut providers = setup.providers;
            if let Some(tool) = &setup.tool {
                providers.push(Arc::new(runner::BuildTaskProvider::new(tool.clone())));
            }
            // A compound configuration belongs to the window itself: its children are the
            // configurations of this list, whatever the language.
            providers.push(Arc::new(runner::CompoundProvider));
            // A line the user typed in the terminal runs here as a configuration too.
            providers.push(Arc::new(runner::CommandProvider));
            cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(POLL).await;
                    if this
                        .update(cx, |this: &mut Self, cx| this.poll(cx))
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .detach();
            let history_file = runner::test_history_file(&setup.cache_root);
            let read = cx
                .background_executor()
                .spawn(async move { runner::load_test_history(&history_file) });
            cx.spawn(async move |this, cx| {
                let loaded = read.await;
                this.update(cx, |this, cx| {
                    if let Ok(runs) = loaded {
                        this.test_history = runs;
                        cx.notify();
                    }
                })
                .ok();
            })
            .detach();
            let tool_bin = setup
                .tool
                .as_ref()
                .filter(|tool| tool.terminal_path())
                .and_then(|tool| tool.tool_bin());
            cx.set_global(crate::terminal_panel::ToolTerminalPath(tool_bin));
            Self {
                workspace,
                focus: cx.focus_handle(),
                follow: ConsoleFollow::new(),
                root: project_root,
                cache_root: setup.cache_root.clone(),
                state_dir,
                tool: setup.tool,
                selection,
                providers,
                environment: setup.environment.clone(),
                loading: session.is_some(),
                session,
                sync,
                trusted,
                store,
                configs: Vec::new(),
                saved: Vec::new(),
                discovered: Vec::new(),
                suggested: Vec::new(),
                foreign: Vec::new(),
                foreign_offered: None,
                temporary: Vec::new(),
                store_broken: None,
                selected: None,
                pending_config: None,
                console: None,
                console_running: false,
                draining: Vec::new(),
                viewing: None,
                console_search: ConsoleSearch::default(),
                search_focus: cx.focus_handle(),
                filter_focus: cx.focus_handle(),
                starting: false,
                starting_tests: None,
                rerun: false,
                waiting: false,
                problems: Vec::new(),
                tool_setup: None,
                tool_download: None,
                wrapper_written: 0,
                title: "Run".into(),
                lines: VecDeque::new(),
                output: OutputBuffers::default(),
                console_process: None,
                program_pid: None,
                tests: TestView::new(),
                test_provider: setup.tests,
                debug_service: setup.debug,
                find_tests: setup.test_sites,
                files: None,
                queued_tests: None,
                waiting_tests: None,
                debugging_tests: false,
                building_project: false,
                build_console: None,
                build_running: false,
                last_build: None,
                build_view: BuildView::default(),
                build_started: None,
                build_verdict: None,
                build_stopped: false,
                build_output: OutputBuffers::default(),
                build_process: None,
                reload: None,
                #[cfg(test)]
                finished: 0,
                #[cfg(test)]
                observation_held: false,
                model_reload: 0,
                console_input: ConsoleInput::new(),
                program_started: false,
                coverage: None,
                coverage_visible: false,
                coverage_pending: None,
                coverage_generation: 0,
                profile: None,
                profile_report: None,
                profile_entries: Vec::new(),
                profile_pending: None,
                profile_generation: 0,
                profile_error: None,
                profile_recording: None,
                profile_recordings: Vec::new(),
                recordings_loading: false,
                profile_revision: 0,
                live: None,
                live_open: false,
                run_config: None,
                run_service: ServiceRun::default(),
                confirm_rerun: None,
                launch: Cancel::new(),
                launch_generation: 0,
                auto_test: false,
                auto_saves: 0,
                auto_pending: false,
                test_history: Vec::new(),
                history_open: false,
                tool_runs: None,
                tool_job: None,
                tool_output: None,
                tool_outputs: VecDeque::new(),
                tool_exit: None,
                tool_output_lines: 0,
            }
        })
    }

    pub fn focus(&self) -> FocusHandle {
        self.focus.clone()
    }

    /// The program the Dump Threads button would ask, for the window tests.
    #[cfg(test)]
    pub(crate) fn set_program_pid(&mut self, pid: Option<u32>) {
        self.program_pid = pid;
    }

    /// Holds the first look of the poll at a run until the run is over, for the window tests
    /// of the tools: a run that ends between two polls is then read by one poll.
    #[cfg(all(test, windows))]
    pub(crate) fn hold_observation(&mut self) {
        self.observation_held = true;
    }

    /// Whether the plan of the run has started its console, for the window tests.
    #[cfg(all(test, windows))]
    pub(crate) fn console_started(&self) -> bool {
        self.console.is_some()
    }

    /// The root of the build model cache of the project, for Invalidate Caches.
    pub fn cache_root(&self) -> &StdPath {
        &self.cache_root
    }

    /// The root of the project, for the panels that show its places.
    pub(crate) fn root(&self) -> &StdPath {
        &self.root
    }

    /// The toolbar widget on this panel.
    pub fn controls(panel: &Entity<Self>) -> RunControls {
        let (
            state,
            run,
            debug,
            stop,
            select,
            build,
            rerun,
            compile,
            build_module,
            rebuild,
            generate_sources,
            rerun_tests,
            background,
            save,
            coverage,
            run_tests_coverage,
            hide_coverage,
            toggle_coverage,
            profile,
            profile_tests,
            module_of,
        ) = (
            panel.clone(),
            panel.clone(),
            panel.clone(),
            panel.clone(),
            panel.clone(),
            panel.clone(),
            panel.clone(),
            panel.clone(),
            panel.clone(),
            panel.clone(),
            panel.clone(),
            panel.clone(),
            panel.clone(),
            panel.clone(),
            panel.clone(),
            panel.clone(),
            panel.clone(),
            panel.clone(),
            panel.clone(),
            panel.clone(),
            panel.clone(),
        );
        RunControls {
            state: Rc::new(move |cx| state.read(cx).state()),
            run: Rc::new(move |_, cx| run.update(cx, |this, cx| this.run(cx))),
            debug: Rc::new(move |window, cx| debug.update(cx, |this, cx| this.debug(window, cx))),
            stop: Rc::new(move |_, cx| stop.update(cx, |this, cx| this.stop(cx))),
            select: Rc::new(move |index, _, cx| {
                select.update(cx, |this, cx| this.select(index, cx))
            }),
            build: Rc::new(move |_, cx| build.update(cx, |this, cx| this.build_project(cx))),
            compile: Rc::new(move |path, _, cx| {
                compile.update(cx, |this, cx| this.compile_current(path, cx))
            }),
            build_module: Rc::new(move |path, _, cx| {
                build_module.update(cx, |this, cx| this.build_module(path, cx))
            }),
            rebuild: Rc::new(move |_, cx| rebuild.update(cx, |this, cx| this.rebuild_project(cx))),
            generate_sources: Rc::new(move |_, cx| {
                generate_sources.update(cx, |this, cx| this.generate_sources(cx))
            }),
            rerun: Rc::new(move |_, cx| rerun.update(cx, |this, cx| this.rerun_last(cx))),
            rerun_tests: Rc::new(move |_, cx| {
                rerun_tests.update(cx, |this, cx| this.rerun_tests(cx))
            }),
            stop_background: Rc::new(move |window, cx| {
                let position = window.mouse_position();
                background.update(cx, |this, cx| this.stop_background(position, cx))
            }),
            save: Rc::new(move |_, cx| save.update(cx, |this, cx| this.save_selected(cx))),
            coverage: Rc::new(move |_, cx| coverage.update(cx, |this, cx| this.run_coverage(cx))),
            run_tests_coverage: Rc::new(move |_, cx| {
                run_tests_coverage.update(cx, |this, cx| this.rerun_tests_coverage(cx))
            }),
            hide_coverage: Rc::new(move |_, cx| {
                hide_coverage.update(cx, |this, cx| this.hide_coverage(cx))
            }),
            toggle_coverage: Rc::new(move |_, cx| {
                toggle_coverage.update(cx, |this, cx| this.toggle_coverage(cx))
            }),
            profile: Rc::new(move |_, cx| profile.update(cx, |this, cx| this.run_profile(cx))),
            profile_tests: Rc::new(move |_, cx| {
                profile_tests.update(cx, |this, cx| this.rerun_tests_profile(cx))
            }),
            module_of: Rc::new(move |path, cx| {
                path.and_then(|path| module_of.read(cx).module_of(path))
                    .map(Into::into)
            }),
        }
    }

    /// The module of the project that holds `path`, for the grouping of the Commit
    /// window. `None` while the model is not there yet.
    pub(crate) fn module_of(&self, path: &StdPath) -> Option<String> {
        let model = self.session.as_ref().and_then(ProjectSession::model)?;
        let module = model
            .modules
            .iter()
            .filter(|module| path.starts_with(&module.dir))
            .max_by_key(|module| module.dir.components().count())?;
        Some(module.name.clone())
    }

    pub fn state(&self) -> RunState {
        let hint = if self.loading {
            Some("Loading project".into())
        } else if self.session.is_none() {
            Some("No build tool".into())
        } else {
            None
        };
        RunState {
            configs: self.configs.iter().map(|c| c.name.clone().into()).collect(),
            selected: self.selected,
            running: self.busy() || self.starting || self.waiting || self.waiting_tests.is_some(),
            hint,
            temporary: self
                .configs
                .iter()
                .map(|config| self.is_temporary(&config.name))
                .collect(),
            build_tool: self
                .session
                .as_ref()
                .and_then(ProjectSession::model)
                .map(|_| SharedString::from(self.tool_title())),
            generates_sources: self
                .tool
                .as_ref()
                .is_some_and(|tool| tool.generates_sources()),
        }
    }

    /// A configuration of the list that is not in the file: a temporary one, or a found
    /// application the list offers until the project has its own configuration of it.
    fn is_temporary(&self, name: &str) -> bool {
        self.temporary.iter().any(|config| config.name == name)
            || self.suggested.iter().any(|config| config.name == name)
    }

    /// Whether `config` is one of the found applications the list offers. An unchanged
    /// one stays a finding when the dialog hands the list back, instead of becoming
    /// temporary; an edited one is a configuration of the user now.
    fn is_suggested(&self, config: &RunConfig) -> bool {
        self.suggested
            .iter()
            .any(|suggested| suggested.name == config.name && suggested == config)
    }

    /// The found configurations the list shows by itself, from their providers.
    fn suggestions(&self) -> Vec<RunConfig> {
        let mut suggested = Vec::new();
        for provider in &self.providers {
            for config in provider.suggestions(&self.saved, &self.discovered) {
                if !suggested
                    .iter()
                    .any(|known: &RunConfig| known.name == config.name)
                {
                    suggested.push(config);
                }
            }
        }
        suggested
    }

    /// The fully qualified entry classes the providers found in the project, for the
    /// entry point choice of the configurations dialog. A class found twice is listed once.
    pub fn discovered_main_classes(&self) -> Vec<String> {
        let mut classes: Vec<String> = self
            .discovered
            .iter()
            .filter_map(|config| {
                self.providers
                    .iter()
                    .find(|provider| provider.id() == config.provider)
                    .and_then(|provider| provider.main_class(config))
            })
            .map(|class| class.trim().to_owned())
            .filter(|class| !class.is_empty())
            .collect();
        classes.sort();
        classes.dedup();
        classes
    }

    /// The name of the build tool, for the title of its tool window.
    pub fn tool_title(&self) -> String {
        let model = self.session.as_ref().and_then(ProjectSession::model);
        let name = model.map_or_else(String::new, |model| model.tool.clone());
        let mut chars = name.chars();
        chars.next().map_or_else(
            || "Build".to_string(),
            |first| first.to_uppercase().chain(chars).collect(),
        )
    }

    /// What the build tool offers to run.
    pub fn tasks(&self) -> Vec<String> {
        let model = self.session.as_ref().and_then(ProjectSession::model);
        match (&self.tool, model) {
            (Some(tool), Some(model)) => tool.tasks(&model),
            _ => Vec::new(),
        }
    }

    /// The modules and groups of tasks the build tool window shows.
    pub fn build_tree(&self) -> Vec<BuildModule> {
        let model = self.session.as_ref().and_then(ProjectSession::model);
        match (&self.tool, model) {
            (Some(tool), Some(model)) => tool.task_tree(&model),
            _ => Vec::new(),
        }
    }

    /// Whether the build tool lets a run leave the tests out.
    pub fn supports_skip_tests(&self) -> bool {
        self.tool
            .as_ref()
            .is_some_and(|tool| tool.supports_skip_tests())
    }

    /// The project model is being read again.
    pub fn is_loading(&self) -> bool {
        self.loading
    }

    /// Whether the project may run its build tool and its configurations.
    pub fn trusted(&self) -> bool {
        self.trusted
    }

    /// The user trusted the project: the build tool that Safe Mode held back starts now,
    /// without a restart of the window.
    pub fn trust_project(&mut self, cx: &mut Context<Self>) {
        self.trusted = true;
        if self.session.is_none()
            && let Some(tool) = self.tool.clone()
        {
            let (session, sync) = open_project(tool, self.root.clone(), &self.cache_root);
            self.session = Some(session);
            self.sync = Some(sync);
            self.loading = true;
        }
        self.changed(cx);
    }

    /// The user forgot the trust of the project: the model stops and no build or
    /// configuration runs until the project is trusted again.
    pub fn forget_trust(&mut self, cx: &mut Context<Self>) {
        self.trusted = false;
        self.stop(cx);
        self.stop_build(cx);
        self.session = None;
        self.sync = None;
        self.loading = false;
        self.changed(cx);
    }

    /// False when the project is in Safe Mode: nothing of the project may run, and the
    /// status bar and the Build window say how to trust it.
    fn may_run_project_code(&self, what: &str, cx: &mut Context<Self>) -> bool {
        if self.trusted {
            return true;
        }
        self.tell_warning(
            "Safe Mode",
            format!("{what} is off while the project is not trusted. Trust the project first."),
            cx,
        );
        false
    }

    /// Reads the model again and publishes it when the tool answers, the way a change of
    /// a build file does. An old model stays readable meanwhile.
    pub fn reload_project(&mut self, cx: &mut Context<Self>) {
        if !self.may_run_project_code("Reading the project", cx) {
            return;
        }
        let Some(tool) = self.tool.clone() else {
            self.tell_warning("Reload", "The project has no build tool", cx);
            return;
        };
        let (session, sync) = open_project(tool, self.root.clone(), &self.cache_root);
        self.session = Some(session);
        self.sync = Some(sync);
        self.loading = true;
        self.changed(cx);
    }

    /// A file the language watches, such as a build script, appeared, vanished or
    /// changed: the project model may name other modules now. A batch of changes, as a
    /// branch switch makes, settles into one import after [`MODEL_RELOAD_DELAY`].
    pub fn model_file_changed(&mut self, cx: &mut Context<Self>) {
        if self.tool.is_none() || !self.trusted {
            return;
        }
        self.model_reload = self.model_reload.wrapping_add(1);
        let mine = self.model_reload;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(MODEL_RELOAD_DELAY).await;
            this.update(cx, |this, cx| {
                if this.model_reload == mine {
                    this.reload_project(cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// The modules of the imported project, for the scope of an inspection.
    pub fn modules(&self) -> Vec<project_model::Module> {
        self.session
            .as_ref()
            .and_then(ProjectSession::model)
            .map_or_else(Vec::new, |model| model.modules.clone())
    }

    /// The tabs of the console: the run of this window first, then the old runs.
    pub fn content_tabs(&self) -> Vec<String> {
        let current = if self.title.is_empty() {
            "Run".to_owned()
        } else {
            self.title.to_string()
        };
        let mut tabs = vec![current];
        tabs.extend(self.draining.iter().map(|drain| drain.title.to_string()));
        tabs
    }

    /// Shows the console tab at `index`, as a click on it does.
    pub fn select_content(&mut self, index: usize, cx: &mut Context<Self>) {
        if index == 0 {
            self.viewing = None;
        } else if index <= self.draining.len() {
            self.viewing = Some(index - 1);
        } else {
            return;
        }
        cx.notify();
    }

    /// The live tab of the current run, when its language has one.
    pub fn live_panel(&self) -> Option<Entity<crate::live_panel::LivePanel>> {
        self.live.clone()
    }

    /// Every configuration the Services window lists: the stored ones, every found one
    /// with its service group, and the temporary ones, whatever the run list itself
    /// shows. A found plain program keeps its group there.
    pub(crate) fn service_configurations(&self) -> Vec<RunConfig> {
        let mut configs = self.saved.clone();
        for config in self.discovered.iter().chain(self.temporary.iter()) {
            if !configs.iter().any(|known| known.name == config.name) {
                configs.push(config.clone());
            }
        }
        configs
    }

    /// The configuration whose console is in the window, and the state of its run, for
    /// the Services window. `None` while a task or tests run: they are no configuration.
    pub(crate) fn service_snapshot(&self) -> Option<(String, ServiceState)> {
        Some((self.run_config.clone()?, self.run_service.state().clone()))
    }

    /// The state of every run the window shows, including the children of a compound
    /// configuration and the instances beside the one that has the console.
    pub(crate) fn service_snapshots(&self) -> Vec<(String, ServiceState)> {
        let mut states = Vec::new();
        if let Some(name) = &self.run_config {
            states.push((name.clone(), self.run_service.state().clone()));
        }
        for drain in &self.draining {
            if let Some(name) = &drain.config {
                states.push((name.clone(), drain.service.state().clone()));
            }
        }
        states
    }

    /// The configuration of the run in the window, if the run is one.
    pub(crate) fn run_config_name(&self) -> Option<&str> {
        self.run_config.as_deref()
    }

    /// Whether the console in the window belongs to the run of `name`.
    pub(crate) fn shows_console(&self, name: &str) -> bool {
        self.run_config.as_deref() == Some(name)
    }

    /// The rows of the console of the current run, for the Services window: the same
    /// lines and the same unfinished line the Run window draws.
    pub(crate) fn console_rows(&self) -> usize {
        self.lines.len() + 1
    }

    pub(crate) fn console_row(&self, index: usize, theme: &Theme) -> AnyElement {
        let current = self.output.current().cloned().unwrap_or_default();
        line_element(self.lines.get(index).unwrap_or(&current), theme)
    }

    /// The scroll position of the console, shared with the Services window.
    pub(crate) fn console_scroll(&self) -> UniformListScrollHandle {
        self.follow.handle()
    }

    /// Shows the live tab, as a click on its tab does.
    pub fn show_live(&mut self, cx: &mut Context<Self>) {
        if self.live.is_none() {
            return;
        }
        self.live_open = true;
        cx.notify();
    }

    /// Shows the console again.
    pub fn show_console(&mut self, cx: &mut Context<Self>) {
        self.live_open = false;
        cx.notify();
    }

    /// The live tab is in front.
    pub fn live_open(&self) -> bool {
        self.live_open
    }

    /// The application of the live tab reported that it is ready.
    pub fn live_started(&self, cx: &App) -> bool {
        self.live
            .as_ref()
            .is_some_and(|live| live.read(cx).started())
    }

    /// The lines the program printed reach the live tab, and its report of the end
    /// reaches the tab too.
    fn feed_live(&mut self, done: &[Line], ended: bool, cx: &mut Context<Self>) {
        let Some(live) = self.live.clone() else {
            return;
        };
        let lines: Vec<String> = done
            .iter()
            .filter(|line| !line.text.is_empty())
            .map(|line| line.text.clone())
            .collect();
        if lines.is_empty() && !ended {
            return;
        }
        live.update(cx, |live, cx| {
            for line in &lines {
                live.observe(line, cx);
            }
            if ended {
                live.stop(cx);
            }
        });
    }

    /// A line of the console of the debugger reaches the live tab of the same run, which
    /// may have started under the debugger.
    pub fn observe_live_line(&mut self, line: &str, cx: &mut Context<Self>) {
        if let Some(live) = self.live.clone() {
            live.update(cx, |live, cx| live.observe(line, cx));
        }
    }

    /// Takes what the debugger's console wrote since the last poll.
    fn drain_debug_live(&mut self, cx: &mut Context<Self>) {
        let lines: Vec<String> = {
            let queue = cx.default_global::<DebugOutput>();
            let mut queue = queue.0.lock().unwrap_or_else(|poison| poison.into_inner());
            std::mem::take(&mut *queue)
        };
        if lines.is_empty() || self.live.is_none() {
            return;
        }
        for line in &lines {
            self.observe_live_line(line, cx);
        }
    }

    /// Starts the live tab of a configuration, or replaces the one of the last run.
    fn set_live(&mut self, app: Option<Arc<dyn LiveApplication>>, cx: &mut Context<Self>) {
        self.live_open = false;
        self.live =
            app.map(|app| crate::live_panel::LivePanel::create(app, self.workspace.clone(), cx));
    }

    fn stop_live(&mut self, cx: &mut Context<Self>) {
        if let Some(live) = self.live.clone() {
            live.update(cx, |live, cx| live.stop(cx));
        }
    }

    /// What went wrong so far, for scripts.
    pub fn problems(&self) -> &[String] {
        &self.problems
    }

    /// The text of the runs that already ended but still receive late output, for tests.
    #[cfg(test)]
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn draining_text(&self) -> Vec<String> {
        self.draining
            .iter()
            .flat_map(|drain| {
                let lines = drain.lines.iter().map(|line| line.text.clone());
                lines.chain(drain.output.unfinished().map(|line| line.text.clone()))
            })
            .collect()
    }

    /// The debug service of the language, for Attach to JVM.
    pub fn debug_service(&self) -> Option<Arc<dyn dap_client::DebugService>> {
        self.debug_service.clone()
    }

    /// Gives the panel the debug service of a server that started after the window.
    pub fn set_debug_service(&mut self, service: Arc<dyn dap_client::DebugService>) {
        self.debug_service = Some(service);
    }

    /// Console text, for scripts.
    pub fn text(&self) -> Vec<String> {
        let lines = self.lines.iter().map(|line| line.text.clone());
        let current = self.output.unfinished().map(|line| line.text.clone());
        lines.chain(current).collect()
    }

    pub fn select(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.configs.len() {
            self.selected = Some(index);
            self.changed(cx);
        }
    }

    pub fn select_named(&mut self, name: &str, cx: &mut Context<Self>) -> bool {
        if let Some(found) = self.configs.iter().position(|config| config.name == name) {
            self.select(found, cx);
            return true;
        }
        // A configuration that only the Services window lists, such as a found program,
        // becomes a temporary one of the list when a tool asks to run it.
        let Some(config) = self
            .discovered
            .iter()
            .find(|config| config.name == name)
            .cloned()
        else {
            return false;
        };
        self.choose_configuration(config, cx);
        true
    }

    /// Every configuration the dialog edits, with whether it lives in the file already
    /// and whether it is temporary.
    pub(crate) fn configuration_items(&self) -> Vec<(RunConfig, bool, bool)> {
        self.configs
            .iter()
            .map(|config| {
                let stored = self.saved.iter().any(|saved| saved.name == config.name);
                (config.clone(), stored, self.is_temporary(&config.name))
            })
            .collect()
    }

    /// Every configuration the providers found, the plain programs included. The dialog
    /// offers their entry points for the main class of a configuration.
    pub(crate) fn discovered_configurations(&self) -> Vec<RunConfig> {
        self.discovered.clone()
    }

    pub(crate) fn selected_configuration(&self) -> Option<usize> {
        self.selected
    }

    pub(crate) fn providers(&self) -> Vec<Arc<dyn RunConfigProvider>> {
        self.providers.clone()
    }

    /// The model the Add button needs to pick a module.
    pub(crate) fn model(&self) -> Option<Arc<ProjectModel>> {
        self.session.as_ref().and_then(ProjectSession::model)
    }

    /// The tool that reads and builds the project, for the windows that ask it a
    /// question of its own, such as the change of a build file.
    pub(crate) fn build_system(&self) -> Option<Arc<dyn BuildSystem>> {
        self.tool.clone()
    }

    /// The answers of the user for the build of the project.
    pub(crate) fn build_selection(&self) -> &BuildSelection {
        &self.selection
    }

    /// Whether the build tool takes named values on its command line, so that the build
    /// window offers to edit them.
    pub(crate) fn takes_properties(&self) -> bool {
        self.tool
            .as_ref()
            .is_some_and(|tool| tool.takes_properties())
    }

    /// The next answer for one named setting: the build file, on, off, and the build file
    /// again. The answer is kept for the commands that follow and the project is imported
    /// again in the background, as after a change of a build file.
    pub(crate) fn cycle_profile(&mut self, name: &str, cx: &mut Context<Self>) {
        let mut next = self.selection.clone();
        next.cycle(name);
        self.store_selection(next, cx);
    }

    /// Takes the named values of the build as the window edited them.
    pub(crate) fn set_build_properties(&mut self, text: &str, cx: &mut Context<Self>) {
        let mut next = self.selection.clone();
        next.set_properties_text(text);
        self.store_selection(next, cx);
    }

    /// Keeps the answers through the tool, which every later command of every window and
    /// provider asks before it starts.
    fn store_selection(&mut self, next: BuildSelection, cx: &mut Context<Self>) {
        let Some(tool) = self.tool.clone() else {
            return;
        };
        if let Err(error) = tool.choose(&self.root, &next) {
            self.tell_error("Build Settings", error.to_string(), cx);
            return;
        }
        self.selection = next;
        self.model_file_changed(cx);
        if let Some(semantic) = cx.try_global::<Semantic>()
            && let Some(server) = semantic.0.server()
        {
            server.reimport();
        }
        self.changed(cx);
    }

    /// Why `.arro/run.toml` cannot be read, if it is broken.
    pub(crate) fn store_error(&self) -> Option<String> {
        self.store_broken.clone()
    }

    /// Takes the list the dialog edited: the stored configurations go into the file, the
    /// discovered ones stay as they are found, and temporary ones stay temporary until
    /// the dialog changes them or Save is asked for. `changed` says whether the file has
    /// to be written at all.
    pub(crate) fn apply_configurations(
        &mut self,
        items: Vec<(RunConfig, bool, bool)>,
        selected: Option<usize>,
        changed: bool,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        if changed && let Some(error) = &self.store_broken {
            return Err(format!("The configurations are not saved: {error}"));
        }
        let saved: Vec<RunConfig> = items
            .iter()
            .filter(|(_, stored, temporary)| *stored && !*temporary)
            .map(|(config, _, _)| config.clone())
            .collect();
        // An unchanged found application stays a finding: the dialog did not make it a
        // configuration of the project.
        let temporary: Vec<RunConfig> = items
            .iter()
            .filter(|(config, _, temporary)| *temporary && !self.is_suggested(config))
            .map(|(config, _, _)| config.clone())
            .collect();
        let discovered: Vec<RunConfig> = items
            .iter()
            .filter(|(config, stored, temporary)| {
                !*stored && (!*temporary || self.is_suggested(config))
            })
            .map(|(config, _, _)| config.clone())
            .collect();
        self.saved = saved;
        self.temporary = temporary;
        self.discovered = discovered;
        self.suggested
            .retain(|config| !self.saved.iter().any(|saved| saved.name == config.name));
        self.configs = items.into_iter().map(|(config, _, _)| config).collect();
        self.selected = selected
            .filter(|index| *index < self.configs.len())
            .or_else(|| (!self.configs.is_empty()).then_some(0));
        if changed {
            self.write_saved(cx);
        }
        self.changed(cx);
        Ok(())
    }

    /// Writes the stored configurations to the file, in the background.
    fn write_saved(&mut self, cx: &mut Context<Self>) {
        let file = runner::configs_file(&self.root);
        let stored = self.saved.clone();
        let write = cx
            .background_executor()
            .spawn(async move { runner::save_configs(&file, &stored) });
        cx.spawn(async move |this, cx| {
            let result = write.await;
            this.update(cx, |this, cx| {
                if let Err(error) = result {
                    let text = format!("The configurations are not saved: {error}");
                    this.tell_error("Run configurations", text, cx);
                }
                this.changed(cx);
            })
            .ok();
        })
        .detach();
    }

    /// Whether one run of `config` may change its arguments, folder and variables: its
    /// provider declares all three fields and it starts no other configuration.
    fn supports_overrides(&self, config: &RunConfig) -> bool {
        if config.is_compound() {
            return false;
        }
        let Some(provider) = self
            .providers
            .iter()
            .find(|provider| provider.id() == config.provider)
        else {
            return false;
        };
        let fields = provider.fields(config);
        let has = |words: &[&str]| {
            fields.iter().any(|field| {
                let key = field.key.to_ascii_lowercase();
                words.iter().any(|word| key.contains(word))
            })
        };
        has(&["arg"]) && has(&["work", "dir"]) && has(&["env"])
    }

    /// A copy of the configuration with the overrides of one run. The stored
    /// configuration and the file do not change.
    fn configuration_with_overrides(
        &self,
        name: &str,
        program_arguments: Option<&str>,
        working_directory: Option<&StdPath>,
        envs: &[String],
    ) -> Result<RunConfig, String> {
        let mut config = self
            .configs
            .iter()
            .find(|config| config.name == name)
            .cloned()
            .ok_or_else(|| format!("the project has no run configuration {name}"))?;
        let provider = self
            .providers
            .iter()
            .find(|provider| provider.id() == config.provider)
            .cloned()
            .ok_or_else(|| "the configuration has no provider".to_string())?;
        let fields = provider.fields(&config);
        if let Some(arguments) = program_arguments
            && !arguments.trim().is_empty()
        {
            let (key, value) = override_of(&fields, &["arg"], arguments, "program arguments")?;
            provider.write_field(&mut config, &key, &value);
        }
        if let Some(directory) = working_directory {
            let (key, value) = override_of(
                &fields,
                &["work", "dir"],
                &directory.to_string_lossy(),
                "a working directory",
            )?;
            provider.write_field(&mut config, &key, &value);
        }
        if !envs.is_empty() {
            let (key, _) = override_of(&fields, &["env"], "", "environment variables")?;
            // The variables of the call add to the ones of the configuration, they do
            // not replace them.
            let mut value = provider.read_field(&config, &key);
            for pair in envs {
                if !value.is_empty() {
                    value.push(';');
                }
                value.push_str(pair);
            }
            provider.write_field(&mut config, &key, &value);
        }
        Ok(config)
    }

    /// Takes the overridden copy of the selected configuration for the launch now, or
    /// the configuration itself when the next run has no overrides.
    fn take_launch_config(&mut self, selected: &RunConfig) -> RunConfig {
        match self.pending_config.take() {
            Some(pending) if pending.name == selected.name => pending,
            _ => selected.clone(),
        }
    }

    /// Makes `config` the selected configuration. A name already in the list is selected
    /// as it is; a new one becomes temporary, and the oldest temporary gives way when
    /// there are five.
    pub fn choose_configuration(&mut self, config: RunConfig, cx: &mut Context<Self>) {
        let name = config.name.clone();
        if !self.configs.iter().any(|known| known.name == name) {
            self.temporary.push(config);
            while self.temporary.len() > MAX_TEMPORARY {
                self.temporary.remove(0);
            }
            self.merge_lists();
        }
        self.selected = self.configs.iter().position(|config| config.name == name);
        self.changed(cx);
    }

    /// Writes the selected temporary configuration into `.arro/run.toml`, as OK of the
    /// dialog does.
    pub fn save_selected(&mut self, cx: &mut Context<Self>) {
        let Some(config) = self
            .selected
            .and_then(|index| self.configs.get(index))
            .cloned()
        else {
            return;
        };
        if !self.is_temporary(&config.name) {
            return;
        }
        self.temporary
            .retain(|temporary| temporary.name != config.name);
        if !self.saved.iter().any(|saved| saved.name == config.name) {
            self.saved.push(config);
        }
        self.merge_lists();
        self.write_saved(cx);
        self.changed(cx);
    }

    /// Whether the provider of this configuration makes a coverage plan.
    pub(crate) fn supports_coverage(&self, provider_id: &str) -> bool {
        self.providers
            .iter()
            .find(|provider| provider.id() == provider_id)
            .is_some_and(|provider| provider.supports_coverage())
    }

    /// A configuration that runs a task of the build tool, made by its provider. The
    /// configuration is stored at once, as the build tool window creates it, and the
    /// dialog then edits it.
    pub fn create_task_configuration(
        &mut self,
        module: &str,
        task: &str,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(model) = self.model() else {
            return false;
        };
        let Some(mut config) = self
            .providers
            .iter()
            .find_map(|provider| provider.task_config(&model, module, task))
        else {
            return false;
        };
        if let Some(index) = self
            .configs
            .iter()
            .position(|known| known.name == config.name)
        {
            self.selected = Some(index);
            self.changed(cx);
            return true;
        }
        config.name = self.unique_name(&config.name);
        self.saved.push(config);
        self.merge_lists();
        let name = self
            .saved
            .last()
            .map(|config| config.name.clone())
            .unwrap_or_default();
        self.selected = self.configs.iter().position(|config| config.name == name);
        self.write_saved(cx);
        self.changed(cx);
        true
    }

    fn unique_name(&self, base: &str) -> String {
        if !self.configs.iter().any(|config| config.name == base) {
            return base.to_owned();
        }
        let mut number = 2;
        loop {
            let candidate = format!("{base} ({number})");
            if !self.configs.iter().any(|config| config.name == candidate) {
                return candidate;
            }
            number += 1;
        }
    }

    /// The places in the text of a file that start a program, from its providers.
    pub(crate) fn launch_points(&self, text: &str, path: &StdPath) -> Vec<runner::LaunchPoint> {
        let Some(model) = self.model() else {
            return Vec::new();
        };
        self.providers
            .iter()
            .flat_map(|provider| provider.launch_points(text, path, &model))
            .collect()
    }

    /// The configurations of another IDE that are not stored here and that the user has
    /// not put away with Not Now.
    fn foreign_pending(&self) -> Vec<runner::ForeignConfigs> {
        let dismissed = self.foreign_dismissed();
        let mut pending = Vec::new();
        for source in &self.foreign {
            let configs: Vec<RunConfig> = source
                .configs
                .iter()
                .filter(|config| {
                    !self.stored_config(config)
                        && !dismissed.contains(&runner::foreign_identity(config))
                })
                .cloned()
                .collect();
            if configs.is_empty() {
                continue;
            }
            pending.push(runner::ForeignConfigs {
                source: source.source,
                configs,
                unsupported: source.unsupported.clone(),
                warnings: source.warnings.clone(),
            });
        }
        pending
    }

    fn stored_config(&self, config: &RunConfig) -> bool {
        self.saved
            .iter()
            .any(|stored| stored.provider == config.provider && stored.name == config.name)
    }

    /// The configurations the user answered Not Now about, from the state folder of the
    /// window rather than from a file of the project.
    fn foreign_dismissed(&self) -> std::collections::HashSet<String> {
        let Some(store) = &self.store else {
            return std::collections::HashSet::new();
        };
        let key = self.foreign_dismissed_key();
        store
            .get(&key)
            .ok()
            .flatten()
            .map(|text| text.lines().map(str::to_owned).collect())
            .unwrap_or_default()
    }

    fn foreign_dismissed_key(&self) -> String {
        format!(
            "run-foreign-configs:{}",
            crate::project_state::project_key(&self.root)
        )
    }

    /// Offers the configurations of another IDE the project holds, once for the names it
    /// offers. A project in Safe Mode is never asked: nothing of it runs.
    fn offer_foreign_configs(&mut self, cx: &mut Context<Self>) {
        if !self.trusted {
            return;
        }
        let pending = self.foreign_pending();
        if pending.is_empty() {
            self.foreign_offered = None;
            return;
        }
        let signature = foreign_signature(&pending);
        if self.foreign_offered.as_deref() == Some(signature.as_str()) {
            return;
        }
        self.foreign_offered = Some(signature);
        let count: usize = pending.iter().map(|source| source.configs.len()).sum();
        let source = pending[0].source;
        let title = format!("{source} run configurations found: {count}");
        let text = "Import them into .arro/run.toml?";
        let workspace = self.workspace.clone();
        cx.defer(move |cx| {
            workspace
                .update(cx, |workspace, cx| {
                    workspace.notify_user_with_commands(
                        title,
                        text,
                        vec![
                            ("import", "Import".into(), "run.import_idea_configs"),
                            ("not-now", "Not Now".into(), "run.dismiss_idea_configs"),
                        ],
                        cx,
                    );
                })
                .ok();
        });
    }

    /// Run | Import Run Configurations from IntelliJ IDEA: reads the files again and
    /// stores every configuration whose kind and name are not in the file yet.
    pub(crate) fn import_foreign_configs(&mut self, cx: &mut Context<Self>) {
        if !self.may_run_project_code("Importing run configurations", cx) {
            return;
        }
        if self.store_broken.is_some() {
            let text = "The file of the run configurations cannot be read; fix it first";
            self.tell_error("Import run configurations", text, cx);
            return;
        }
        let Some(model) = self.session.as_ref().and_then(ProjectSession::model) else {
            let text = "The project is still loading";
            self.tell("Import run configurations", text, cx);
            return;
        };
        let providers = self.providers.clone();
        let found = cx
            .background_executor()
            .spawn(async move { foreign_sources(&providers, &model) });
        cx.spawn(async move |this, cx| {
            let sources = found.await;
            this.update(cx, |this, cx| this.finish_import(sources, cx))
                .ok();
        })
        .detach();
    }

    fn finish_import(&mut self, sources: Vec<runner::ForeignConfigs>, cx: &mut Context<Self>) {
        let mut imported: Vec<String> = Vec::new();
        let mut skipped: Vec<String> = Vec::new();
        let mut unsupported: Vec<(String, usize)> = Vec::new();
        let mut warnings: Vec<String> = Vec::new();
        for source in &sources {
            for (kind, count) in &source.unsupported {
                match unsupported.iter_mut().find(|(known, _)| known == kind) {
                    Some((_, total)) => *total += count,
                    None => unsupported.push((kind.clone(), *count)),
                }
            }
            warnings.extend(source.warnings.iter().cloned());
            for config in &source.configs {
                if self.stored_config(config) {
                    skipped.push(config.name.clone());
                    continue;
                }
                self.saved.push(config.clone());
                imported.push(config.name.clone());
            }
        }
        if !imported.is_empty() {
            self.foreign_offered = None;
            self.merge_lists();
            self.write_saved(cx);
        }
        let source = sources
            .first()
            .map_or("IntelliJ IDEA", |source| source.source);
        let title = format!("Import {source} run configurations");
        if sources.is_empty() {
            self.tell(
                &title,
                format!("No {source} run configurations were found in the project."),
                cx,
            );
            return;
        }
        let mut lines = vec![
            format!("Imported {}: {}", imported.len(), names_list(&imported)),
            format!(
                "Skipped, exists {}: {}",
                skipped.len(),
                names_list(&skipped)
            ),
        ];
        let unsupported_text = unsupported
            .iter()
            .map(|(kind, count)| format!("{kind} ({count})"))
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(format!(
            "Not supported {}: {}",
            unsupported.iter().map(|(_, count)| count).sum::<usize>(),
            if unsupported_text.is_empty() {
                "none".to_owned()
            } else {
                unsupported_text
            }
        ));
        if !warnings.is_empty() {
            lines.push(format!("Warnings: {}", warnings.join("; ")));
        }
        self.tell(&title, lines.join("\n"), cx);
        self.changed(cx);
    }

    /// Not Now: the configurations of this project stay out until new ones appear.
    pub(crate) fn dismiss_foreign_configs(&mut self, cx: &mut Context<Self>) {
        let mut keys: Vec<String> = self
            .foreign
            .iter()
            .flat_map(|source| source.configs.iter())
            .map(runner::foreign_identity)
            .collect();
        keys.sort();
        keys.dedup();
        if let Some(store) = &self.store {
            let _ = store.put(&self.foreign_dismissed_key(), &keys.join("\n"));
        }
        self.foreign_offered = None;
        self.changed(cx);
    }

    /// The list of the window: the stored configurations, then the found applications the
    /// providers offer by themselves, then the temporary ones that are not there already.
    /// A found program of the project is not in the list; its launch points and its
    /// Services entry stay.
    fn merge_lists(&mut self) {
        let previous = self
            .selected
            .and_then(|index| self.configs.get(index))
            .map(|config| config.name.clone());
        // A found application that was selected and is now covered by a stored
        // configuration of the same class, as after an import, hands the selection to it.
        let covered = previous.as_deref().and_then(|name| {
            let offered = self.suggested.iter().find(|config| config.name == name)?;
            let class = self.main_class_of(offered)?;
            self.saved
                .iter()
                .find(|saved| self.main_class_of(saved).as_deref() == Some(class.as_str()))
                .map(|saved| saved.name.clone())
        });
        let previous = covered.or(previous);
        self.suggested = self.suggestions();
        // A stored configuration of the same name is the one the list shows.
        self.suggested
            .retain(|config| !self.saved.iter().any(|saved| saved.name == config.name));
        let mut configs = self.saved.clone();
        for config in self.suggested.iter().chain(self.temporary.iter()) {
            if !configs.iter().any(|known| known.name == config.name) {
                configs.push(config.clone());
            }
        }
        self.configs = configs;
        let kept = previous
            .as_deref()
            .and_then(|name| self.configs.iter().position(|config| config.name == name));
        // Before the list stopped showing every found program, a user could select one
        // that has no configuration of its own. It stays selected as a temporary one.
        if kept.is_none()
            && let Some(name) = previous.as_deref()
            && let Some(found) = self
                .discovered
                .iter()
                .find(|config| config.name == name)
                .cloned()
        {
            if !self
                .temporary
                .iter()
                .any(|config| config.name == found.name)
            {
                self.temporary.push(found.clone());
                while self.temporary.len() > MAX_TEMPORARY {
                    self.temporary.remove(0);
                }
            }
            if !self.configs.iter().any(|config| config.name == name) {
                self.configs.push(found);
            }
        }
        let kept = previous
            .as_deref()
            .and_then(|name| self.configs.iter().position(|config| config.name == name));
        self.selected = kept.or_else(|| (!self.configs.is_empty()).then_some(0));
    }

    /// The class a configuration starts, as its provider reads it.
    fn main_class_of(&self, config: &RunConfig) -> Option<String> {
        self.providers
            .iter()
            .find(|provider| provider.id() == config.provider)
            .and_then(|provider| provider.main_class(config))
    }

    /// Something runs: a program, a task or tests.
    fn busy(&self) -> bool {
        self.console_running
            || self.tests.running()
            || (self.build_console.is_some() && self.build_running)
            || self.draining.iter().any(|drain| drain.running)
    }

    fn stop_running(&self) {
        if let Some(console) = &self.console {
            console.stop();
        }
        if let Some(console) = &self.build_console {
            console.stop();
        }
        for console in self
            .draining
            .iter()
            .filter_map(|drain| drain.console.as_ref())
        {
            console.stop();
        }
        if let Some(run) = &self.tests.run {
            run.stop();
        }
    }

    /// Moves the console of the last run aside, so its late output keeps going into the
    /// console of that run while the next one gets a clean one.
    fn archive_console(&mut self) {
        let Some(console) = self.console.take() else {
            return;
        };
        let title = std::mem::take(&mut self.title);
        let output = std::mem::take(&mut self.output);
        let process = self.console_process.take();
        let config = self.run_config.take();
        let service = std::mem::take(&mut self.run_service);
        let running = self.console_running;
        self.program_pid = None;
        let lines = self.lines.drain(..).collect();
        // Old runs whose output has ended are kept as tabs, the latest few of them.
        while self.draining.len() >= MAX_OLD_RUNS {
            let Some(oldest) = self
                .draining
                .iter()
                .position(|drain| drain.console.is_none() && !drain.running)
            else {
                break;
            };
            self.draining.remove(oldest);
        }
        self.draining.push(DrainingConsole {
            title,
            console: Some(console),
            lines,
            process,
            output,
            config,
            service,
            running,
        });
        self.viewing = None;
        self.console_running = false;
        self.program_started = false;
        self.console_input.restart();
    }

    /// The workspace shows the tool window: this may run inside a workspace update.
    pub fn run(&mut self, cx: &mut Context<Self>) {
        if self.starting {
            return;
        }
        if !self.may_run_project_code("Run", cx) {
            return;
        }
        if self.busy() {
            let config = self
                .selected
                .and_then(|index| self.configs.get(index))
                .cloned();
            let programs_run =
                self.console_running || self.draining.iter().any(|drain| drain.running);
            // A configuration that allows several instances opens another tab instead of
            // asking what to do with the one that runs.
            if programs_run
                && !self.tests.running()
                && config
                    .as_ref()
                    .is_some_and(|config| config.allow_multiple_instances)
            {
                self.start(cx);
                return;
            }
            let name = config.map_or_else(|| self.title.to_string(), |config| config.name);
            self.confirm_rerun = Some(name);
            self.changed(cx);
            return;
        }
        self.start(cx);
    }

    /// Stop and Rerun of the question a second Run asks: the program that runs is
    /// stopped and the selected configuration starts again.
    pub fn rerun_confirmed(&mut self, cx: &mut Context<Self>) {
        self.confirm_rerun = None;
        self.stop(cx);
        self.start(cx);
    }

    /// Cancel of that question: the program that runs keeps running.
    pub fn cancel_rerun(&mut self, cx: &mut Context<Self>) {
        self.confirm_rerun = None;
        self.changed(cx);
    }

    /// Plans the selected configuration in the background and starts its console.
    fn start(&mut self, cx: &mut Context<Self>) {
        if !self.may_run_project_code("Run", cx) {
            return;
        }
        let Some(selected) = self
            .selected
            .and_then(|index| self.configs.get(index))
            .cloned()
        else {
            return;
        };
        let model = self.session.as_ref().and_then(ProjectSession::model);
        let Some(model) = model.filter(|model| model.resolved || !self.loading) else {
            self.waiting = true;
            self.run_config = Some(selected.name.clone());
            self.run_service = ServiceRun::default();
            self.run_service.starting();
            self.title = selected.name.clone().into();
            self.lines.clear();
            self.note("Waiting for the project to load", false);
            self.changed(cx);
            return;
        };
        // The overrides a tool asked for belong to this run alone: they live in the
        // copy, never in the list or in the file.
        let config = self.take_launch_config(&selected);
        let provider = self.providers.iter().find(|p| p.id() == config.provider);
        let Some(provider) = provider.cloned() else {
            let text = format!("Nothing runs configurations of kind {}", config.provider);
            self.tell_error("Cannot run", text, cx);
            return;
        };
        // A configuration of tests takes the path of the Tests window instead of a
        // program of its own, so its result lands in the tree of the last test run.
        if let Some(request) = provider.test_request(&config) {
            self.run_tests(request, cx);
            return;
        }
        if config.is_compound() {
            self.start_compound(config, model, cx);
            return;
        }
        self.launch_config(config, provider, model, cx);
    }

    /// Plans one configuration in the background and starts its console with the before
    /// launch steps of the configuration.
    fn launch_config(
        &mut self,
        config: RunConfig,
        provider: Arc<dyn RunConfigProvider>,
        model: Arc<ProjectModel>,
        cx: &mut Context<Self>,
    ) {
        let title = config.name.clone();
        let (cancel, generation) = self.begin_launch();
        self.archive_console();
        self.confirm_rerun = None;
        self.live = None;
        self.live_open = false;
        self.run_config = Some(title.clone());
        self.run_service = ServiceRun::default();
        self.run_service.starting();
        crate::history::put_label(&self.root, format!("Before running {title}"), cx);
        self.title = title.into();
        self.lines.clear();
        self.console_search.clear();
        self.output = OutputBuffers::default();
        self.console_process = None;
        self.program_pid = None;
        self.console_input.restart();
        self.program_started = false;
        self.tests.clear();
        cx.emit(TestsChanged);
        let tool = self.tool.clone();
        let providers = self.providers.clone();
        let configs = self.configs.clone();
        let root = self.root.clone();
        let planned = cx.background_executor().spawn(async move {
            let mut plan = provider
                .plan(&config, &model)
                .map_err(|error| error.to_string())?;
            let live = provider.live_application(&config, &model);
            let steps = before_launch_specs(
                &config,
                tool.as_deref(),
                &model,
                &configs,
                &providers,
                &root,
            )?;
            let steps = apply_environment_file(&config, &root, &mut plan, steps)?;
            Ok((plan, live, steps))
        });
        cx.spawn(async move |this, cx| {
            let planned: Result<_, String> = planned.await;
            this.update(cx, |this, cx| {
                if !this.launch_still_wanted(&cancel, generation) {
                    this.changed(cx);
                    return;
                }
                this.starting = false;
                match planned {
                    Ok((plan, live, steps)) => {
                        this.set_live(live, cx);
                        this.console = Some(Console::start_with_steps(plan, steps, cancel));
                        this.console_running = true;
                    }
                    Err(message) => {
                        this.run_service.stopped();
                        this.note(&message, true);
                        this.tell_error("Cannot run", message, cx);
                    }
                }
                this.changed(cx);
            })
            .ok();
        })
        .detach();
        self.changed(cx);
    }

    /// Starts every configuration of a compound one, each in a tab of its own.
    fn start_compound(
        &mut self,
        config: RunConfig,
        model: Arc<ProjectModel>,
        cx: &mut Context<Self>,
    ) {
        let mut children = Vec::new();
        for name in &config.compound {
            let Some(child) = self
                .configs
                .iter()
                .find(|known| known.name == *name)
                .cloned()
            else {
                let message = format!("Compound names a configuration that is not there: {name}");
                self.note(&message, true);
                self.tell_error("Cannot run", message, cx);
                return;
            };
            let Some(provider) = self
                .providers
                .iter()
                .find(|provider| provider.id() == child.provider)
                .cloned()
            else {
                let message = format!("Nothing runs configurations of kind {}", child.provider);
                self.note(&message, true);
                self.tell_error("Cannot run", message, cx);
                return;
            };
            if child.is_compound() {
                let message = format!("A compound configuration cannot start {name}");
                self.note(&message, true);
                self.tell_error("Cannot run", message, cx);
                return;
            }
            children.push((child, provider));
        }
        if children.is_empty() {
            let message = "This compound configuration lists nothing to run".to_owned();
            self.note(&message, true);
            self.tell_error("Cannot run", message, cx);
            return;
        }
        let (cancel, generation) = self.begin_launch();
        self.archive_console();
        self.confirm_rerun = None;
        self.live = None;
        self.live_open = false;
        self.run_config = None;
        self.run_service = ServiceRun::default();
        self.title = config.name.clone().into();
        self.lines.clear();
        self.output = OutputBuffers::default();
        self.console_process = None;
        self.program_pid = None;
        self.console_input.restart();
        self.program_started = false;
        self.tests.clear();
        cx.emit(TestsChanged);
        let tool = self.tool.clone();
        let providers = self.providers.clone();
        let configs = self.configs.clone();
        let root = self.root.clone();
        let compound = config.clone();
        let planned = cx.background_executor().spawn(async move {
            // The steps of the compound configuration itself go before the first child,
            // in the console of that child.
            let own_steps = before_launch_specs(
                &compound,
                tool.as_deref(),
                &model,
                &configs,
                &providers,
                &root,
            )?;
            let own_variables = environment_variables(&compound, &root)?;
            let mut own_steps: Vec<ProcessSpec> = own_steps
                .into_iter()
                .map(|spec| runner::with_environment(spec, &own_variables))
                .collect();
            let mut starts = Vec::new();
            let mut first = true;
            for (child, provider) in children {
                let mut plan = provider
                    .plan(&child, &model)
                    .map_err(|error| format!("{}: {error}", child.name))?;
                let live = provider.live_application(&child, &model);
                let steps = before_launch_specs(
                    &child,
                    tool.as_deref(),
                    &model,
                    &configs,
                    &providers,
                    &root,
                )
                .map_err(|error| format!("{}: {error}", child.name))?;
                let mut steps = apply_environment_file(&child, &root, &mut plan, steps)
                    .map_err(|error| format!("{}: {error}", child.name))?;
                if first {
                    own_steps.append(&mut steps);
                    steps = std::mem::take(&mut own_steps);
                    first = false;
                }
                starts.push((child.name.clone(), plan, live, steps));
            }
            Ok(starts)
        });
        cx.spawn(async move |this, cx| {
            let planned: Result<_, String> = planned.await;
            this.update(cx, |this, cx| {
                if !this.launch_still_wanted(&cancel, generation) {
                    this.changed(cx);
                    return;
                }
                this.starting = false;
                match planned {
                    Ok(starts) => {
                        let mut starts = starts.into_iter();
                        if let Some((name, plan, live, steps)) = starts.next() {
                            this.set_live(live, cx);
                            this.title = name.clone().into();
                            this.run_config = Some(name);
                            this.run_service = ServiceRun::default();
                            this.run_service.starting();
                            this.console =
                                Some(Console::start_with_steps(plan, steps, cancel.clone()));
                            this.console_running = true;
                        }
                        for (name, plan, _live, steps) in starts {
                            let mut service = ServiceRun::default();
                            service.starting();
                            this.draining.push(DrainingConsole {
                                title: name.clone().into(),
                                console: Some(Console::start_with_steps(
                                    plan,
                                    steps,
                                    cancel.clone(),
                                )),
                                lines: Vec::new(),
                                process: None,
                                output: OutputBuffers::default(),
                                config: Some(name),
                                service,
                                running: true,
                            });
                        }
                    }
                    Err(message) => {
                        this.run_service.stopped();
                        this.note(&message, true);
                        this.tell_error("Cannot run", message, cx);
                    }
                }
                this.changed(cx);
            })
            .ok();
        })
        .detach();
        self.changed(cx);
    }

    /// Runs the selected configuration with the coverage agent. The old data goes at
    /// once; the new report replaces it when the run ends.
    pub fn run_coverage(&mut self, cx: &mut Context<Self>) {
        if !self.may_run_project_code("Coverage", cx) {
            return;
        }
        if self.starting {
            return;
        }
        if self.busy() {
            self.tell_warning("Busy", "Stop what is running first", cx);
            return;
        }
        let Some(config) = self
            .selected
            .and_then(|index| self.configs.get(index))
            .cloned()
        else {
            return;
        };
        let model = self.session.as_ref().and_then(ProjectSession::model);
        let Some(model) = model.filter(|model| model.resolved || !self.loading) else {
            self.tell_warning("Cannot run", "The project is still loading", cx);
            return;
        };
        let Some(provider) = self
            .providers
            .iter()
            .find(|provider| provider.id() == config.provider)
            .cloned()
        else {
            let text = format!("Nothing runs configurations of kind {}", config.provider);
            self.tell_error("Cannot run", text, cx);
            return;
        };
        self.begin_coverage();
        self.coverage_changed(cx);
        self.waiting = false;
        self.waiting_tests = None;
        self.queued_tests = None;
        self.rerun = false;
        self.archive_console();
        self.run_config = Some(config.name.clone());
        self.run_service = ServiceRun::default();
        self.run_service.starting();
        self.title = config.name.clone().into();
        self.lines.clear();
        self.output = OutputBuffers::default();
        self.console_process = None;
        self.program_pid = None;
        self.console_input.restart();
        self.program_started = false;
        self.tests.clear();
        cx.emit(TestsChanged);
        let owner = provider.clone();
        let generation = self.coverage_generation;
        let plan = cx
            .background_executor()
            .spawn(async move { provider.coverage_plan(&config, &model) });
        cx.spawn(async move |this, cx| {
            let plan = plan.await;
            this.update(cx, |this, cx| {
                this.starting = false;
                match plan {
                    Ok(coverage) => {
                        if let Some(note) = &coverage.note {
                            this.note(note, false);
                        }
                        this.coverage_pending = Some(PendingCoverage {
                            exec: coverage.exec,
                            owner: CoverageOwner::Configuration(owner),
                            generation,
                        });
                        this.console = Some(Console::start(coverage.plan));
                        this.console_running = true;
                    }
                    Err(error) => {
                        let message = error.to_string();
                        this.run_service.stopped();
                        this.note(&message, true);
                        this.tell_error("Cannot run with coverage", message, cx);
                    }
                }
                this.changed(cx);
            })
            .ok();
        })
        .detach();
        self.starting = true;
        self.changed(cx);
    }

    /// Alt+Shift+R with coverage: the test request of the last run.
    pub fn rerun_tests_coverage(&mut self, cx: &mut Context<Self>) {
        match self.tests.request.clone() {
            Some(request) => self.run_tests_coverage(request, cx),
            None => self.tell("Run Tests with Coverage", "No tests ran yet", cx),
        }
    }

    /// Runs the tests through the build tool with the coverage agent in their JVM.
    pub fn run_tests_coverage(&mut self, request: TestRequest, cx: &mut Context<Self>) {
        if !self.may_run_project_code("Tests", cx) {
            return;
        }
        if self.starting {
            return;
        }
        if self.busy() {
            self.tell_warning("Busy", "Stop what is running first", cx);
            return;
        }
        let Some(provider) = self.test_provider.clone() else {
            self.tell_warning("Cannot run tests", "The project has no build tool", cx);
            return;
        };
        let model = self.session.as_ref().and_then(ProjectSession::model);
        let Some(model) = model.filter(|model| model.resolved || !self.loading) else {
            self.tell_warning("Cannot run tests", "The project is still loading", cx);
            return;
        };
        self.begin_coverage();
        self.coverage_changed(cx);
        self.waiting_tests = None;
        self.queued_tests = None;
        self.rerun = false;
        self.archive_console();
        self.run_config = None;
        self.run_service = ServiceRun::default();
        self.title = title_of(&request).into();
        self.lines.clear();
        self.output = OutputBuffers::default();
        self.console_process = None;
        self.program_pid = None;
        self.console_input.restart();
        self.program_started = false;
        self.tests.begin(request.clone());
        self.starting = true;
        let owner = provider.clone();
        let generation = self.coverage_generation;
        let plan = cx.background_executor().spawn(async move {
            TestRun::coverage_start(&*provider, &model, &request).map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| {
            let started = plan.await;
            this.update(cx, |this, cx| {
                this.starting = false;
                match started {
                    Ok(started) => {
                        if let Some(note) = &started.note {
                            this.note(note, false);
                        }
                        this.coverage_pending = Some(PendingCoverage {
                            exec: started.exec,
                            owner: CoverageOwner::Tests(owner),
                            generation,
                        });
                        this.tests.run = Some(started.run);
                    }
                    Err(message) => {
                        this.note(&message, true);
                        this.tell_error("Cannot run tests with coverage", message, cx);
                        this.tests.finished = true;
                    }
                }
                cx.emit(TestsChanged);
                this.changed(cx);
            })
            .ok();
        })
        .detach();
        cx.emit(TestsChanged);
        self.changed(cx);
    }

    /// Runs the tests of the last run with their JVMs recorded. The recordings are read
    /// when the run ends.
    pub fn run_tests_profile(&mut self, request: TestRequest, cx: &mut Context<Self>) {
        if !self.may_run_project_code("Profiling", cx) {
            return;
        }
        if self.starting {
            return;
        }
        if self.busy() {
            self.tell_warning("Busy", "Stop what is running first", cx);
            return;
        }
        let Some(provider) = self.test_provider.clone() else {
            self.tell_warning("Cannot run tests", "The project has no build tool", cx);
            return;
        };
        if !provider.supports_profiler() {
            self.tell_warning(
                "Cannot profile the tests",
                "The build tool cannot record the test JVMs",
                cx,
            );
            return;
        }
        let model = self.session.as_ref().and_then(ProjectSession::model);
        let Some(model) = model.filter(|model| model.resolved || !self.loading) else {
            self.tell_warning("Cannot run tests", "The project is still loading", cx);
            return;
        };
        self.begin_profile(cx);
        self.waiting_tests = None;
        self.queued_tests = None;
        self.rerun = false;
        self.archive_console();
        self.run_config = None;
        self.run_service = ServiceRun::default();
        self.title = title_of(&request).into();
        self.lines.clear();
        self.output = OutputBuffers::default();
        self.console_process = None;
        self.console_input.restart();
        self.program_started = false;
        self.tests.begin(request.clone());
        self.starting = true;
        let owner = provider.clone();
        let generation = self.profile_generation;
        let plan = cx.background_executor().spawn(async move {
            TestRun::profile_start(&*provider, &model, &request).map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| {
            let started = plan.await;
            this.update(cx, |this, cx| {
                this.starting = false;
                match started {
                    Ok(started) => {
                        if let Some(note) = &started.note {
                            this.note(note, false);
                        }
                        this.profile_recording = Some(started.recording.clone());
                        this.profile_pending = Some(PendingProfile {
                            recording: started.recording,
                            owner: ProfileOwner::Tests(owner),
                            generation,
                        });
                        this.tests.run = Some(started.run);
                    }
                    Err(message) => {
                        this.note(&message, true);
                        this.tell_error("Cannot run tests with profiler", message, cx);
                        this.tests.finished = true;
                    }
                }
                cx.emit(TestsChanged);
                this.changed(cx);
            })
            .ok();
        })
        .detach();
        cx.emit(TestsChanged);
        self.changed(cx);
    }

    /// The action of the menu: the tests of the last run, recorded.
    pub fn rerun_tests_profile(&mut self, cx: &mut Context<Self>) {
        match self.tests.request.clone() {
            Some(request) => self.run_tests_profile(request, cx),
            None => self.tell(
                "Nothing to profile",
                "Run a test before profiling the tests",
                cx,
            ),
        }
    }

    /// Starts over the coverage of the window: the old data is dropped before the run, as
    /// a new run with coverage replaces it.
    fn begin_coverage(&mut self) {
        self.coverage_generation = self.coverage_generation.wrapping_add(1);
        self.coverage = None;
        self.coverage_visible = true;
        self.coverage_pending = None;
    }

    /// The data of the run that just ended, read in the background.
    fn finish_coverage(&mut self, cx: &mut Context<Self>) {
        let Some(pending) = self.coverage_pending.take() else {
            return;
        };
        let Some(model) = self.session.as_ref().and_then(ProjectSession::model) else {
            return;
        };
        let PendingCoverage {
            exec,
            owner,
            generation,
        } = pending;
        let report = cx
            .background_executor()
            .spawn(async move { owner.report(&exec, &model) });
        cx.spawn(async move |this, cx| {
            let report = report.await;
            this.update(cx, |this, cx| {
                if this.coverage_generation != generation {
                    return;
                }
                match report {
                    Ok(report) => {
                        this.coverage = Some(Arc::new(report));
                        this.coverage_visible = true;
                        this.coverage_changed(cx);
                    }
                    Err(error) => {
                        this.note(&format!("No coverage data: {error}"), true);
                        this.changed(cx);
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// Hides the coverage stripes, clears the window and stops waiting for a report.
    pub fn hide_coverage(&mut self, cx: &mut Context<Self>) {
        self.coverage_generation = self.coverage_generation.wrapping_add(1);
        self.coverage = None;
        self.coverage_visible = false;
        self.coverage_pending = None;
        self.coverage_changed(cx);
    }

    /// Ctrl+Alt+F6: the stripes go and come back without closing the window.
    pub fn toggle_coverage(&mut self, cx: &mut Context<Self>) {
        if self.coverage.is_none() {
            return;
        }
        self.coverage_visible = !self.coverage_visible;
        self.coverage_changed(cx);
    }

    /// The report of the last run, when its stripes are shown.
    pub fn coverage(&self) -> Option<Arc<lang_api::CoverageReport>> {
        self.coverage.clone().filter(|_| self.coverage_visible)
    }

    pub fn coverage_visible(&self) -> bool {
        self.coverage_visible
    }

    /// The percentage of the covered lines of a file, for the project tree.
    pub fn coverage_percent(&self, path: &StdPath) -> Option<u32> {
        let report = self.coverage()?;
        report.file(path)?.line_counters().percent()
    }

    fn coverage_changed(&mut self, cx: &mut Context<Self>) {
        cx.emit(CoverageChanged);
        let project = cx
            .try_global::<crate::project::ActiveProjectPanel>()
            .map(|project| project.0.clone());
        if let Some(project) = project {
            project.update(cx, |_, cx| cx.notify());
        }
        self.changed(cx);
    }

    /// Runs the selected configuration with its samples recorded. The old snapshot goes
    /// at once; the new one replaces it when the run ends.
    pub fn run_profile(&mut self, cx: &mut Context<Self>) {
        if !self.may_run_project_code("Profiling", cx) {
            return;
        }
        if self.starting {
            return;
        }
        if self.busy() {
            self.tell_warning("Busy", "Stop what is running first", cx);
            return;
        }
        let Some(config) = self
            .selected
            .and_then(|index| self.configs.get(index))
            .cloned()
        else {
            return;
        };
        let model = self.session.as_ref().and_then(ProjectSession::model);
        let Some(model) = model.filter(|model| model.resolved || !self.loading) else {
            self.tell_warning("Cannot profile", "The project is still loading", cx);
            return;
        };
        let Some(provider) = self
            .providers
            .iter()
            .find(|provider| provider.id() == config.provider)
            .cloned()
        else {
            let text = format!("Nothing runs configurations of kind {}", config.provider);
            self.tell_error("Cannot profile", text, cx);
            return;
        };
        self.begin_profile(cx);
        self.waiting = false;
        self.waiting_tests = None;
        self.queued_tests = None;
        self.rerun = false;
        self.archive_console();
        self.run_config = Some(config.name.clone());
        self.run_service = ServiceRun::default();
        self.run_service.starting();
        self.title = config.name.clone().into();
        self.lines.clear();
        self.output = OutputBuffers::default();
        self.console_process = None;
        self.program_pid = None;
        self.console_input.restart();
        self.program_started = false;
        self.tests.clear();
        cx.emit(TestsChanged);
        let owner = provider.clone();
        let generation = self.profile_generation;
        let plan = cx
            .background_executor()
            .spawn(async move { provider.profile_plan(&config, &model) });
        cx.spawn(async move |this, cx| {
            let plan = plan.await;
            this.update(cx, |this, cx| {
                this.starting = false;
                match plan {
                    Ok(profile) => {
                        if let Some(note) = &profile.note {
                            this.note(note, false);
                        }
                        this.profile_recording = Some(profile.recording.clone());
                        this.profile_pending = Some(PendingProfile {
                            recording: profile.recording,
                            owner: ProfileOwner::Configuration(owner),
                            generation,
                        });
                        this.console = Some(Console::start(profile.plan));
                        this.console_running = true;
                        this.profile_changed(cx);
                    }
                    Err(error) => {
                        let message = error.to_string();
                        this.run_service.stopped();
                        this.note(&message, true);
                        this.profile_error = Some(message.clone());
                        this.tell_error("Cannot run with profiler", message, cx);
                        this.profile_changed(cx);
                    }
                }
                this.changed(cx);
            })
            .ok();
        })
        .detach();
        self.starting = true;
        self.changed(cx);
    }

    /// Starts over the profile of the window: the old snapshot is dropped before the run,
    /// as a new run replaces it.
    fn begin_profile(&mut self, cx: &mut Context<Self>) {
        self.profile_generation = self.profile_generation.wrapping_add(1);
        self.profile = None;
        self.profile_report = None;
        self.profile_entries.clear();
        self.profile_pending = None;
        self.profile_error = None;
        self.profile_recording = None;
        self.profile_changed(cx);
    }

    /// The recordings of the run that just ended, read in the background.
    fn finish_profile(&mut self, cx: &mut Context<Self>) {
        let Some(pending) = self.profile_pending.take() else {
            return;
        };
        let Some(model) = self.session.as_ref().and_then(ProjectSession::model) else {
            return;
        };
        let PendingProfile {
            recording,
            owner,
            generation,
        } = pending;
        let recordings = owner.run_recordings(&recording);
        let reports = cx.background_executor().spawn(async move {
            let mut entries = Vec::new();
            let mut error = None;
            for path in recordings {
                let label = path
                    .file_name()
                    .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
                match owner.report(&path, &model) {
                    Ok(report) => entries.push(ProfileEntry {
                        label,
                        report: Arc::new(report),
                    }),
                    Err(failed) => {
                        if error.is_none() {
                            error = Some(failed.to_string());
                        }
                    }
                }
            }
            (entries, error)
        });
        cx.spawn(async move |this, cx| {
            let (entries, error) = reports.await;
            this.update(cx, |this, cx| {
                if this.profile_generation != generation {
                    return;
                }
                if entries.is_empty() {
                    let message = error.unwrap_or_else(|| "no recording was written".to_owned());
                    this.note(&format!("No profile: {message}"), true);
                    this.profile_error = Some(message);
                } else {
                    let first = entries[0].report.clone();
                    this.profile = Some(Arc::new(first.cpu_tree.clone()));
                    this.profile_report = Some(first);
                    this.profile_entries = entries;
                    this.profile_error = None;
                }
                this.refresh_profile_recordings(cx);
                this.profile_changed(cx);
            })
            .ok();
        })
        .detach();
    }

    /// Reads a recording from disk into the window, with the provider of the selected
    /// configuration, or the first provider that can read one.
    pub fn open_profile_file(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let Some(model) = self.session.as_ref().and_then(ProjectSession::model) else {
            self.tell_warning("Cannot read a profile", "The project is still loading", cx);
            return;
        };
        let Some(provider) = self.profiler_provider() else {
            self.tell_error(
                "Cannot read a profile",
                "The project has no profiler provider",
                cx,
            );
            return;
        };
        self.profile_generation = self.profile_generation.wrapping_add(1);
        let generation = self.profile_generation;
        self.profile_pending = None;
        self.profile = None;
        self.profile_report = None;
        self.profile_entries.clear();
        self.profile_error = None;
        self.profile_recording = Some(path.clone());
        self.profile_changed(cx);
        let owner = ProfileOwner::Configuration(provider);
        let label_path = path.clone();
        let report = cx
            .background_executor()
            .spawn(async move { owner.report(&path, &model) });
        cx.spawn(async move |this, cx| {
            let report = report.await;
            this.update(cx, |this, cx| {
                if this.profile_generation != generation {
                    return;
                }
                match report {
                    Ok(report) => {
                        let report = Arc::new(report);
                        this.profile = Some(Arc::new(report.cpu_tree.clone()));
                        this.profile_report = Some(report.clone());
                        this.profile_entries = vec![ProfileEntry {
                            label: path_label(&label_path),
                            report,
                        }];
                        this.profile_error = None;
                    }
                    Err(error) => {
                        let message = error.to_string();
                        this.note(&format!("No profile: {message}"), true);
                        this.profile_error = Some(message);
                    }
                }
                this.refresh_profile_recordings(cx);
                this.profile_changed(cx);
            })
            .ok();
        })
        .detach();
    }

    /// The provider that opens a recording: the one of the selected configuration, or the
    /// first provider of the project that can.
    fn profiler_provider(&self) -> Option<Arc<dyn RunConfigProvider>> {
        let selected = self
            .selected
            .and_then(|index| self.configs.get(index))
            .and_then(|config| {
                self.providers
                    .iter()
                    .find(|provider| provider.id() == config.provider)
            })
            .filter(|provider| provider.supports_profiler())
            .cloned();
        selected.or_else(|| {
            self.providers
                .iter()
                .find(|provider| provider.supports_profiler())
                .cloned()
        })
    }

    /// Asks the provider for the recordings on disk, in the background. The window lists
    /// the newest; the folder keeps them itself.
    pub fn refresh_profile_recordings(&mut self, cx: &mut Context<Self>) {
        if self.recordings_loading {
            return;
        }
        let Some(model) = self.session.as_ref().and_then(ProjectSession::model) else {
            return;
        };
        let Some(provider) = self.profiler_provider() else {
            return;
        };
        self.recordings_loading = true;
        let recordings = cx
            .background_executor()
            .spawn(async move { provider.profile_recordings(&model) });
        cx.spawn(async move |this, cx| {
            let recordings = recordings.await;
            this.update(cx, |this, cx| {
                this.recordings_loading = false;
                if let Ok(recordings) = recordings {
                    this.profile_recordings = recordings;
                }
                cx.emit(ProfileRecordingsChanged);
                cx.notify();
                this.changed(cx);
            })
            .ok();
        })
        .detach();
    }

    /// The snapshot of the last run, when one was read.
    pub fn profile(&self) -> Option<Arc<lang_api::ProfileSnapshot>> {
        self.profile.clone()
    }

    /// Everything the first recording of the last run holds.
    pub fn profile_report(&self) -> Option<Arc<lang_api::ProfileReport>> {
        self.profile_report.clone()
    }

    /// The recordings of the last run, one entry each, when there are several.
    pub fn profile_entries(&self) -> &[ProfileEntry] {
        &self.profile_entries
    }

    /// Why the last recording could not be read.
    pub fn profile_error(&self) -> Option<String> {
        self.profile_error.clone()
    }

    /// The recording of the last run, or the file the user opened.
    pub fn profile_recording(&self) -> Option<&StdPath> {
        self.profile_recording.as_deref()
    }

    /// The recordings the Profiler window lists, the newest first.
    pub fn profile_recordings(&self) -> &[lang_api::ProfileRecording] {
        &self.profile_recordings
    }

    /// The provider and the model that read `path` as a heap dump, when the language can.
    pub fn heap_reader(
        &self,
        path: &StdPath,
    ) -> Option<(Arc<dyn RunConfigProvider>, ProjectModel)> {
        let model = self
            .session
            .as_ref()
            .and_then(ProjectSession::model)
            .map_or_else(ProjectModel::default, |model| (*model).clone());
        let provider = self
            .providers
            .iter()
            .find(|provider| provider.is_heap_dump(path))?
            .clone();
        Some((provider, model))
    }

    /// What Capture Memory Snapshot needs from the running program, when there is one.
    pub fn heap_request(&self) -> Option<crate::heap_dump::HeapRequest> {
        self.heap_request_for(self.program_pid?)
    }

    /// The same request for a program whose process the caller knows, such as the
    /// debuggee the adapter reported.
    pub fn heap_request_for(&self, pid: u32) -> Option<crate::heap_dump::HeapRequest> {
        let model = self.session.as_ref().and_then(ProjectSession::model)?;
        let provider = self
            .providers
            .iter()
            .find(|provider| provider.supports_heap_dump())?
            .clone();
        let name = self.run_config.clone().unwrap_or_else(|| "heap".to_owned());
        let stem: String = name
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() {
                    character
                } else {
                    '-'
                }
            })
            .collect();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_millis());
        let file = self
            .root
            .join(".arro")
            .join("snapshots")
            .join(format!("{stem}-{stamp}.hprof"));
        Some(crate::heap_dump::HeapRequest {
            pid,
            provider,
            model: (*model).clone(),
            file,
        })
    }

    /// Whether a profile run records now: the window offers Stop and Show Snapshot.
    pub fn profile_running(&self) -> bool {
        self.profile_pending.is_some()
    }

    /// The button of the Run window: stops the profiled program gently, so the runtime
    /// writes the recording, and shows the Profiler window. The snapshot arrives when the
    /// process ends.
    pub fn stop_and_show_profile(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.stop(cx);
        let workspace = self.workspace.clone();
        workspace
            .update(cx, |workspace, cx| {
                workspace.show_tool(workspace::PROFILER_TOOL, window, cx)
            })
            .ok();
    }

    fn profile_changed(&mut self, cx: &mut Context<Self>) {
        self.profile_revision = self.profile_revision.wrapping_add(1);
        cx.emit(ProfileChanged);
        self.changed(cx);
    }

    /// The number of changes of the snapshot, its file and its error.
    pub fn profile_revision(&self) -> u64 {
        self.profile_revision
    }

    /// Runs a task of the root project of the build tool, `package` for one. The console
    /// takes it over.
    pub fn run_task(&mut self, task: &str, cx: &mut Context<Self>) {
        self.run_task_in(None, task, false, cx);
    }

    /// Runs a task of one module; `None` is the root project. The console takes it over.
    pub fn run_task_in(
        &mut self,
        module: Option<&str>,
        task: &str,
        skip_tests: bool,
        cx: &mut Context<Self>,
    ) {
        if self.starting || self.busy() {
            self.tell_warning("Busy", "Stop what is running first", cx);
            return;
        }
        let Some(tool) = self.tool.clone() else {
            return;
        };
        let module = module.and_then(|id| self.module_named(id));
        let (root, name) = (self.root.clone(), task.to_string());
        let options = TaskOptions { skip_tests };
        let title = format!("{} {task}", self.tool_title());
        self.launch(title, None, cx, move || {
            let program = tool
                .task_in(&root, module.as_ref(), &name, options)
                .map_err(|error| error.to_string())?;
            Ok((
                RunPlan {
                    before: None,
                    program,
                    grace: TASK_GRACE,
                },
                None,
            ))
        });
    }

    /// Runs a line typed in the build tool window, in the root project.
    pub fn run_goal(&mut self, line: &str, cx: &mut Context<Self>) {
        if self.starting || self.busy() {
            self.tell_warning("Busy", "Stop what is running first", cx);
            return;
        }
        let Some(tool) = self.tool.clone() else {
            return;
        };
        let (root, line) = (self.root.clone(), line.to_string());
        let title = format!("{} {line}", self.tool_title());
        self.launch(title, None, cx, move || {
            let program = tool
                .goal(&root, &line, TaskOptions::default())
                .map_err(|error| error.to_string())?;
            Ok((
                RunPlan {
                    before: None,
                    program,
                    grace: TASK_GRACE,
                },
                None,
            ))
        });
    }

    /// Runs a task that starts a program under the debugger of the language.
    pub fn debug_task_in(
        &mut self,
        module: Option<&str>,
        task: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(service) = self.debug_service.clone() else {
            self.tell_warning("Cannot debug", "The language server is not running", cx);
            return;
        };
        let Some(tool) = self.tool.clone() else {
            self.tell_warning("Cannot debug", "The project has no build tool", cx);
            return;
        };
        let module = module.and_then(|id| self.module_named(id));
        let (root, name) = (self.root.clone(), task.to_string());
        let title = format!("{} {task}", self.tool_title());
        let plan = cx.background_executor().spawn(async move {
            tool.debug_task_in(&root, module.as_ref(), &name, TaskOptions::default())
                .map(|program| RunPlan {
                    before: None,
                    program,
                    grace: TASK_GRACE,
                })
                .map_err(|error| error.to_string())
        });
        let panel = cx.global::<crate::debug::ActiveDebug>().0.clone();
        let workspace = self.workspace.clone();
        let handle = window.window_handle();
        let shown = title.clone();
        cx.spawn(async move |this, cx| {
            let plan = match plan.await {
                Ok(plan) => plan,
                Err(error) => {
                    this.update(cx, |this, cx| this.tell_error("Cannot debug", error, cx))
                        .ok();
                    return;
                }
            };
            cx.update(|cx| {
                let entry = workspace
                    .read_with(cx, |workspace, _| workspace.active_path())
                    .ok()
                    .flatten();
                panel.update(cx, |panel, cx| {
                    panel.start_plan(service, plan, shown, entry, cx)
                });
                handle
                    .update(cx, |_, window, cx| {
                        workspace
                            .update(cx, |workspace, cx| {
                                workspace.show_tool(DEBUG_TOOL, window, cx)
                            })
                            .ok();
                    })
                    .ok();
            });
        })
        .detach();
    }

    /// The module with this id, from the model the window shows.
    fn module_named(&self, id: &str) -> Option<project_model::Module> {
        self.session
            .as_ref()
            .and_then(ProjectSession::model)?
            .module(id)
            .cloned()
    }

    /// Ctrl+F9: builds the classes of the project with its build tool, the test classes
    /// too while a test session is under the debugger. During a debug session the changed
    /// classes go into the running program. A build during a debug test run gets a
    /// console of its own: the test run keeps its console and its tree.
    pub fn build_project(&mut self, cx: &mut Context<Self>) {
        if !self.may_run_project_code("Build", cx) {
            return;
        }
        if self.starting || (self.build_console.is_some() && self.build_running) {
            self.tell_warning("Busy", "Stop what is running first", cx);
            return;
        }
        let debugging_tests = self.debugging_tests && self.tests.running();
        if self.busy() && !debugging_tests {
            self.tell_warning("Busy", "Stop what is running first", cx);
            return;
        }
        let Some(tool) = self.tool.clone() else {
            self.tell_warning("Cannot build", "The project has no build tool", cx);
            return;
        };
        let (root, tests) = (self.root.clone(), self.debugging_tests);
        let title = format!("{} Build Project", self.tool_title());
        let make = move || {
            let program = tool
                .build(&root, tests)
                .map_err(|error| error.to_string())?;
            Ok(RunPlan {
                before: None,
                program,
                grace: TASK_GRACE,
            })
        };
        self.note_build_started(BuildKind::Project);
        if debugging_tests {
            self.launch_build(title, cx, make);
        } else {
            self.building_project = true;
            self.launch(title, None, cx, move || make().map(|plan| (plan, None)));
        }
        self.refresh_build_view();
    }

    /// Ctrl+Shift+F9: compiles the module of `file` with the build tool. Without a file,
    /// or when no module holds it, the whole project compiles.
    pub fn compile_current(&mut self, file: Option<PathBuf>, cx: &mut Context<Self>) {
        self.compile_in(file.clone(), BuildKind::Compile(file), "Recompile", cx);
    }

    /// Build Module: compiles the module that holds `file`.
    pub fn build_module(&mut self, file: Option<PathBuf>, cx: &mut Context<Self>) {
        self.compile_in(file.clone(), BuildKind::Module(file), "Build Module", cx);
    }

    fn compile_in(
        &mut self,
        file: Option<PathBuf>,
        kind: BuildKind,
        command: &str,
        cx: &mut Context<Self>,
    ) {
        if !self.may_run_project_code(command, cx) {
            return;
        }
        if self.starting || self.busy() {
            self.tell_warning("Busy", "Stop what is running first", cx);
            return;
        }
        let Some(tool) = self.tool.clone() else {
            self.tell_warning("Cannot compile", "The project has no build tool", cx);
            return;
        };
        let module = file.as_deref().and_then(|file| {
            let model = self.session.as_ref().and_then(ProjectSession::model)?;
            model
                .modules
                .iter()
                .filter(|module| file.starts_with(&module.dir))
                .max_by_key(|module| module.dir.components().count())
                .cloned()
        });
        let (root, title) = (
            self.root.clone(),
            format!("{} {command}", self.tool_title()),
        );
        let make = move || {
            let program = tool
                .compile(&root, module.as_ref())
                .map_err(|error| error.to_string())?;
            Ok((
                RunPlan {
                    before: None,
                    program,
                    grace: TASK_GRACE,
                },
                None,
            ))
        };
        self.note_build_started(kind);
        self.building_project = true;
        self.launch(title, None, cx, make);
        self.refresh_build_view();
    }

    /// Rebuild Project: builds the whole project again from scratch.
    pub fn rebuild_project(&mut self, cx: &mut Context<Self>) {
        if !self.may_run_project_code("Rebuild", cx) {
            return;
        }
        if self.starting || self.busy() {
            self.tell_warning("Busy", "Stop what is running first", cx);
            return;
        }
        let Some(tool) = self.tool.clone() else {
            self.tell_warning("Cannot rebuild", "The project has no build tool", cx);
            return;
        };
        let (root, tests) = (self.root.clone(), self.debugging_tests);
        let title = format!("{} Rebuild Project", self.tool_title());
        let make = move || {
            let program = tool
                .rebuild(&root, tests)
                .map_err(|error| error.to_string())?;
            Ok((
                RunPlan {
                    before: None,
                    program,
                    grace: TASK_GRACE,
                },
                None,
            ))
        };
        self.note_build_started(BuildKind::Rebuild);
        self.building_project = true;
        self.launch(title, None, cx, make);
        self.refresh_build_view();
    }

    /// Generate Sources and Update Folders: runs the source generator of the build tool.
    pub fn generate_sources(&mut self, cx: &mut Context<Self>) {
        if !self.may_run_project_code("Generating sources", cx) {
            return;
        }
        if self.starting || self.busy() {
            self.tell_warning("Busy", "Stop what is running first", cx);
            return;
        }
        let Some(tool) = self.tool.clone() else {
            self.tell_warning(
                "Cannot generate sources",
                "The project has no build tool",
                cx,
            );
            return;
        };
        let root = self.root.clone();
        let title = format!("{} Generate Sources", self.tool_title());
        let make = move || {
            let Some(program) = tool
                .generate_sources(&root)
                .map_err(|error| error.to_string())?
            else {
                return Err("The build tool has no source generator".to_string());
            };
            Ok((
                RunPlan {
                    before: None,
                    program,
                    grace: TASK_GRACE,
                },
                None,
            ))
        };
        self.note_build_started(BuildKind::GenerateSources);
        self.building_project = true;
        self.launch(title, None, cx, make);
        self.refresh_build_view();
    }

    /// Restart Build of the Build window: runs the last build again.
    pub fn restart_build(&mut self, cx: &mut Context<Self>) {
        let Some(kind) = self.last_build.clone() else {
            return;
        };
        match kind {
            BuildKind::Project => self.build_project(cx),
            BuildKind::Module(file) => self.build_module(file, cx),
            BuildKind::Compile(file) => self.compile_current(file, cx),
            BuildKind::Rebuild => self.rebuild_project(cx),
            BuildKind::GenerateSources => self.generate_sources(cx),
        }
    }

    /// The Stop button of the Build window: stops the build and keeps the output.
    pub fn stop_build(&mut self, cx: &mut Context<Self>) {
        if !self.building_project && self.build_console.is_none() {
            return;
        }
        if let Some(console) = &self.build_console {
            console.stop();
        }
        if self.building_project
            && let Some(console) = &self.console
        {
            console.stop();
        }
        self.build_console = None;
        self.building_project = false;
        self.build_stopped = true;
        self.build_verdict = Some((
            false,
            self.build_started
                .map_or(Duration::ZERO, |started| started.elapsed()),
        ));
        self.refresh_build_view();
        self.changed(cx);
    }

    /// The last build as the Build window shows it.
    pub(crate) fn build_view(&self) -> &BuildView {
        &self.build_view
    }

    /// Records the end of a build whose console is the one of the panel or one of its own.
    fn finish_build(&mut self, code: Option<i32>, cx: &mut Context<Self>) {
        if !std::mem::take(&mut self.building_project) {
            return;
        }
        let success = code == Some(0);
        let duration = self
            .build_started
            .map_or(Duration::ZERO, |started| started.elapsed());
        self.build_verdict = Some((success, duration));
        self.build_stopped = false;
        self.refresh_build_view();
        if success {
            self.mark_built();
            self.request_reload(cx);
        }
    }

    fn note_build_started(&mut self, kind: BuildKind) {
        self.last_build = Some(kind);
        self.build_started = Some(Instant::now());
        self.build_verdict = None;
        self.build_stopped = false;
    }

    /// Reads the problems of the build output again. It runs only while the console holds
    /// a build: a program run must not replace what the Build window shows.
    fn refresh_build_view(&mut self) {
        let lines: Vec<String> = self.lines.iter().map(|line| line.text.clone()).collect();
        let problems = tool_gateway::build_problems(&lines, &self.root);
        let (finished, success, duration) = match self.build_verdict {
            Some((success, duration)) => (true, success, Some(duration)),
            None => (false, false, None),
        };
        self.build_view = BuildView {
            running: self.building_project,
            finished,
            success,
            stopped: self.build_stopped,
            duration,
            started: self.build_started,
            title: self.title.to_string(),
            lines,
            problems,
        };
    }

    /// Alt+Shift+R: the tests of the last run again.
    pub fn rerun_tests(&mut self, cx: &mut Context<Self>) {
        match self.tests.request.clone() {
            Some(request) => self.run_tests(request, cx),
            None => self.tell("Rerun Tests", "No tests ran yet", cx),
        }
    }

    /// Connects the tools of the MCP server to this window: their builds, tests and runs
    /// go through the same buttons, consoles and trees as the ones of the user.
    pub(crate) fn set_tool_runs(&mut self, runs: Arc<RunBridge>) {
        self.tool_runs = Some(runs);
    }

    /// Takes the jobs the MCP tools left, starts them, and answers the ones that ended.
    fn poll_tool_jobs(&mut self, cx: &mut Context<Self>) -> bool {
        let mut changed = self.refresh_tool_output();
        changed |= self.complete_tool_job();
        let Some(bridge) = self.tool_runs.clone() else {
            return changed;
        };
        while let Some(job) = bridge.take_job() {
            changed = true;
            self.take_tool_job(&bridge, job, cx);
        }
        changed
    }

    fn take_tool_job(&mut self, bridge: &Arc<RunBridge>, job: Job, cx: &mut Context<Self>) {
        match job.request.clone() {
            ToolRequest::Configurations => {
                let configurations = self
                    .configs
                    .iter()
                    .map(|config| RunConfiguration {
                        name: config.name.clone(),
                        kind: config.provider.clone(),
                        command_line: config
                            .settings
                            .get("programArguments")
                            .and_then(|value| value.as_str())
                            .map(str::to_string),
                        working_directory: config
                            .settings
                            .get("workingDirectory")
                            .and_then(|value| value.as_str())
                            .map(str::to_string),
                        environment: Vec::new(),
                        supports_dynamic_launch_overrides: self.supports_overrides(config),
                    })
                    .collect();
                let _ = job.reply.send(Ok(Answer::Configurations(configurations)));
            }
            ToolRequest::Output(id) => {
                self.refresh_tool_output();
                match self.tool_outputs.iter().find(|(known, _)| *known == id) {
                    Some((_, output)) => {
                        let _ = job.reply.send(Ok(Answer::Output(output.clone())));
                    }
                    None => {
                        let _ = job.reply.send(Err(format!("no run has the id {id}")));
                    }
                }
            }
            ToolRequest::Stop(id) => {
                let active = self.tool_job.as_ref().is_some_and(|job| job.output == id)
                    || self
                        .tool_output
                        .as_ref()
                        .is_some_and(|(active, _)| *active == id);
                if active {
                    self.stop(cx);
                    let _ = job.reply.send(Ok(Answer::Stopped));
                } else {
                    let _ = job
                        .reply
                        .send(Err(format!("no run with the id {id} is running")));
                }
            }
            ToolRequest::Build(request) => self.start_tool_build(bridge, job, request, cx),
            ToolRequest::Tests(request) => self.start_tool_tests(bridge, job, request, cx),
            ToolRequest::Start(request) => self.start_tool_run(bridge, job, &request, cx),
        }
    }

    fn refuse_tool_job(&mut self, bridge: &Arc<RunBridge>, job: Job, message: impl Into<String>) {
        let _ = job.reply.send(Err(message.into()));
        bridge.release();
    }

    fn start_tool_build(
        &mut self,
        bridge: &Arc<RunBridge>,
        job: Job,
        request: BuildRequest,
        cx: &mut Context<Self>,
    ) {
        if self.starting || self.busy() {
            return self.refuse_tool_job(bridge, job, "the Run window is busy");
        }
        let Some(tool) = self.tool.clone() else {
            return self.refuse_tool_job(bridge, job, "the project has no build tool");
        };
        let module = match &request.module {
            Some(name) => {
                let model = self.session.as_ref().and_then(ProjectSession::model);
                match model.and_then(|model| {
                    model
                        .modules
                        .iter()
                        .find(|module| &module.name == name || &module.id == name)
                        .cloned()
                }) {
                    Some(module) => Some(module),
                    None => {
                        return self.refuse_tool_job(
                            bridge,
                            job,
                            format!("the project has no module {name}"),
                        );
                    }
                }
            }
            None => None,
        };
        let output = job.output_id();
        self.tool_exit = None;
        self.tool_output_lines = 0;
        self.tool_job = Some(ToolJob {
            output: output.clone(),
            kind: ToolJobKind::Build,
            started: Instant::now(),
            process_started: false,
            start_error: None,
            reply: job.reply.clone(),
        });
        self.tool_output = Some((output, false));
        match module {
            Some(module) => {
                let (root, title) = (self.root.clone(), format!("{} Compile", self.tool_title()));
                self.building_project = true;
                self.launch(title, None, cx, move || {
                    let program = tool
                        .compile(&root, Some(&module))
                        .map_err(|error| error.to_string())?;
                    Ok((
                        RunPlan {
                            before: None,
                            program,
                            grace: TASK_GRACE,
                        },
                        None,
                    ))
                });
            }
            None => self.build_project(cx),
        }
        if !(self.starting || self.console.is_some()) {
            self.tool_job = None;
            self.tool_output = None;
            return self.refuse_tool_job(bridge, job, "the build did not start");
        }
        self.note_tool_client(job.client.as_deref());
    }

    fn start_tool_tests(
        &mut self,
        bridge: &Arc<RunBridge>,
        job: Job,
        request: TestsRequest,
        cx: &mut Context<Self>,
    ) {
        if self.starting || self.busy() {
            return self.refuse_tool_job(bridge, job, "the Run window is busy");
        }
        if self.test_provider.is_none() {
            return self.refuse_tool_job(bridge, job, "the project has no test runner");
        }
        let output = job.output_id();
        self.tool_exit = None;
        self.tool_output_lines = 0;
        self.tool_job = Some(ToolJob {
            output: output.clone(),
            kind: ToolJobKind::Tests,
            started: Instant::now(),
            process_started: false,
            start_error: None,
            reply: job.reply.clone(),
        });
        self.tool_output = Some((output, false));
        let request = TestRequest {
            module: request.module,
            package: request.package,
            suite: request.suite,
            case: request.case,
            cases: Vec::new(),
        };
        self.run_tests(request, cx);
        let started = self.starting || self.waiting_tests.is_some() || self.tests.run.is_some();
        if !started {
            self.tool_job = None;
            self.tool_output = None;
            return self.refuse_tool_job(bridge, job, "the tests did not start");
        }
        self.note_tool_client(job.client.as_deref());
    }

    fn start_tool_run(
        &mut self,
        bridge: &Arc<RunBridge>,
        job: Job,
        request: &StartRunRequest,
        cx: &mut Context<Self>,
    ) {
        if self.starting || self.busy() {
            return self.refuse_tool_job(bridge, job, "the Run window is busy");
        }
        let Some(config) = self
            .configs
            .iter()
            .find(|config| config.name == request.name)
            .cloned()
        else {
            return self.refuse_tool_job(
                bridge,
                job,
                format!("the project has no run configuration {}", request.name),
            );
        };
        let pending = match self.configuration_with_overrides(
            &request.name,
            request.program_arguments.as_deref(),
            request.working_directory.as_deref(),
            &request.envs,
        ) {
            Ok(config) => config,
            Err(message) => return self.refuse_tool_job(bridge, job, message),
        };
        self.pending_config = Some(pending);
        let output = job.output_id();
        self.tool_exit = None;
        self.tool_output_lines = 0;
        self.tool_output = Some((output.clone(), true));
        self.choose_configuration(config, cx);
        self.run(cx);
        if !(self.starting || self.waiting || self.console.is_some()) {
            self.pending_config = None;
            self.tool_output = None;
            return self.refuse_tool_job(bridge, job, "the configuration did not start");
        }
        self.note_tool_client(job.client.as_deref());
        let _ = job.reply.send(Ok(Answer::Started(output)));
        bridge.release();
    }

    /// Names the client of the MCP server in the title and in the console of the run.
    fn note_tool_client(&mut self, client: Option<&str>) {
        let Some(client) = client else {
            return;
        };
        let title = self.title.to_string();
        self.title = format!("{title} · {client}").into();
        self.note(&format!("Started by {client}"), false);
    }

    fn line_tail(&self) -> Vec<String> {
        self.lines
            .iter()
            .rev()
            .take(tool_gateway::OUTPUT_TAIL)
            .rev()
            .map(|line| line.text.clone())
            .collect()
    }

    /// The file with the whole log of a job, in the state folder of the window.
    fn tool_output_file(&self, id: &str) -> PathBuf {
        self.state_dir
            .join("run-output")
            .join(crate::project_state::project_key(&self.root))
            .join(format!("{id}.log"))
    }

    /// Brings the file of the whole log up to date. Returns the file once it is there.
    fn write_tool_output(&mut self, id: &str) -> Option<PathBuf> {
        let path = self.tool_output_file(id);
        if let Some(parent) = path.parent()
            && std::fs::create_dir_all(parent).is_err()
        {
            return None;
        }
        let count = self.lines.len();
        let written = self.tool_output_lines;
        let rewrite = count < written || count >= MAX_LINES || !path.exists();
        if rewrite {
            let mut text = String::new();
            for line in &self.lines {
                text.push_str(&line.text);
                text.push('\n');
            }
            if std::fs::write(&path, text).is_err() {
                return None;
            }
            self.tool_output_lines = count;
        } else if count > written {
            let mut text = String::new();
            for line in self.lines.iter().skip(written) {
                text.push_str(&line.text);
                text.push('\n');
            }
            use std::io::Write as _;
            let Ok(mut file) = std::fs::OpenOptions::new().append(true).open(&path) else {
                return None;
            };
            if file.write_all(text.as_bytes()).is_err() {
                return None;
            }
            self.tool_output_lines = count;
        }
        Some(path)
    }

    /// Keeps the output of a job an MCP tool asked for up to date, so `get_run_output`
    /// sees the lines as they come.
    fn refresh_tool_output(&mut self) -> bool {
        let Some((id, program)) = self.tool_output.clone() else {
            return false;
        };
        let running = self.starting
            || self.waiting
            || self.console_running
            || self.tests.running()
            || (self.build_console.is_some() && self.build_running);
        let exit = if program {
            match self.run_service.state() {
                ServiceState::Stopped { code } if !running => *code,
                _ => None,
            }
        } else {
            None
        };
        let full_output_path = self.write_tool_output(&id);
        let output = RunOutput {
            id: id.clone(),
            running,
            exit_code: exit,
            lines: self.line_tail(),
            full_output_path,
        };
        self.store_tool_output(output);
        if program && !running {
            self.tool_output = None;
        }
        true
    }

    fn store_tool_output(&mut self, output: RunOutput) {
        if let Some((_, known)) = self
            .tool_outputs
            .iter_mut()
            .find(|(id, _)| id == &output.id)
        {
            *known = output;
            return;
        }
        self.tool_outputs.push_back((output.id.clone(), output));
        while self.tool_outputs.len() > 8 {
            self.tool_outputs.pop_front();
        }
    }

    /// Answers the build or test run of a job once the console is closed and every event
    /// is in.
    fn complete_tool_job(&mut self) -> bool {
        let done = self.tool_job.as_ref().is_some_and(|job| match job.kind {
            ToolJobKind::Build => !self.starting && self.console.is_none() && !self.console_running,
            ToolJobKind::Tests => {
                !self.starting && self.waiting_tests.is_none() && self.tests.run.is_none()
            }
        });
        if !done {
            return false;
        }
        let Some(job) = self.tool_job.take() else {
            return false;
        };
        let Some(bridge) = self.tool_runs.clone() else {
            return false;
        };
        let duration_ms = job.started.elapsed().as_millis() as u64;
        if !job.process_started {
            // The process never started. Its console gave the reason when it could not
            // start it; a plan that failed before a console came says it in the last line.
            let reason = job
                .start_error
                .or_else(|| self.lines.back().map(|line| line.text.clone()))
                .unwrap_or_default();
            let message = match reason.is_empty() {
                true => "the run did not start".to_string(),
                false => format!("the run did not start: {reason}"),
            };
            let _ = job.reply.send(Err(message));
            self.tool_output = None;
            self.tool_exit = None;
            bridge.release();
            return true;
        }
        match job.kind {
            ToolJobKind::Build => {
                let exit = self.tool_exit.flatten();
                let success = exit == Some(0);
                let text: Vec<String> = self.lines.iter().map(|line| line.text.clone()).collect();
                let problems = tool_gateway::build_problems(&text, &self.root);
                self.store_tool_output(RunOutput {
                    id: job.output.clone(),
                    running: false,
                    exit_code: Some(if success { 0 } else { 1 }),
                    lines: self.line_tail(),
                    full_output_path: None,
                });
                let outcome = BuildOutcome {
                    success,
                    problems,
                    duration_ms,
                    output: job.output.clone(),
                };
                let _ = job.reply.send(Ok(Answer::Build(outcome)));
            }
            ToolJobKind::Tests => {
                let counts = self.tests.tree.counts();
                let cases = self
                    .tests
                    .tree
                    .cases()
                    .filter_map(|node| {
                        let outcome = match &node.state {
                            runner::TestState::Passed => TestOutcome::Passed,
                            runner::TestState::Failed { .. } => TestOutcome::Failed,
                            runner::TestState::Aborted { .. }
                            | runner::TestState::Skipped { .. } => TestOutcome::Skipped,
                            runner::TestState::Running => return None,
                        };
                        let (message, trace) = match &node.state {
                            runner::TestState::Failed { message, trace } => (
                                message.clone(),
                                trace
                                    .as_deref()
                                    .map(|trace| {
                                        trace.lines().take(5).map(str::to_string).collect()
                                    })
                                    .unwrap_or_default(),
                            ),
                            runner::TestState::Aborted { message } => (message.clone(), Vec::new()),
                            _ => (None, Vec::new()),
                        };
                        Some(TestCase {
                            suite: node.node.suite.clone().unwrap_or_default(),
                            case: node.node.case.clone(),
                            outcome,
                            message,
                            trace,
                            duration_ms: node.duration_ms,
                        })
                    })
                    .collect();
                let failed = counts.failed;
                self.store_tool_output(RunOutput {
                    id: job.output.clone(),
                    running: false,
                    exit_code: Some(if failed > 0 { 1 } else { 0 }),
                    lines: self.line_tail(),
                    full_output_path: None,
                });
                let outcome = TestsOutcome {
                    passed: counts.passed,
                    failed,
                    skipped: counts.skipped + counts.aborted,
                    cases,
                    duration_ms,
                    output: job.output.clone(),
                };
                let _ = job.reply.send(Ok(Answer::Tests(outcome)));
            }
        }
        self.tool_output = None;
        self.tool_exit = None;
        bridge.release();
        true
    }

    /// Whether the console in front reads what the user types. The program of a run does;
    /// a build step, a test run and an old console tab do not.
    pub(crate) fn takes_input(&self) -> bool {
        self.viewing.is_none()
            && self.program_started
            && self.console_input.is_open()
            && self.console.as_ref().is_some_and(Console::takes_input)
    }

    /// The line typed into the console, for tests (their program is powershell.exe).
    #[cfg(all(test, windows))]
    pub(crate) fn input_text(&self) -> &str {
        self.console_input.text()
    }

    /// Enter in the console: the line goes to the program with the line break after it and
    /// stays in the output, as IDEA shows what was typed.
    pub fn execute_input(&mut self, cx: &mut Context<Self>) {
        if !self.takes_input() {
            return;
        }
        let line = self.console_input.take_line();
        self.send_console_line(&line, cx);
    }

    /// Ends the unfinished lines of the process the main console had now.
    fn finish_console_process(&mut self, done: &mut Vec<Line>) {
        match self.console_process.take() {
            Some(process) => self.output.finish(process, done),
            None => self.output.finish_all(done),
        }
    }

    fn send_console_line(&mut self, line: &str, cx: &mut Context<Self>) {
        if let Some(console) = &self.console {
            console.send_line(line);
        }
        // A prompt the program left unfinished ends before the line the user typed.
        let mut done = Vec::new();
        self.finish_console_process(&mut done);
        self.lines.extend(done);
        self.lines
            .push_back(Line::plain(line.to_owned(), input_style()));
        self.console_input.remember(line);
        self.changed(cx);
    }

    /// Send EOF: the program sees the end of its input, and nothing more is typed.
    pub fn send_eof(&mut self, cx: &mut Context<Self>) {
        if !self.takes_input() {
            return;
        }
        if let Some(console) = &self.console {
            console.send_eof();
        }
        self.console_input.close();
        self.changed(cx);
    }

    /// Up and Down in the input line: the lines sent to this console before, newest first.
    pub fn browse_history(&mut self, older: bool, cx: &mut Context<Self>) {
        if !self.takes_input() {
            return;
        }
        if self.console_input.browse_history(older) {
            cx.notify();
        }
    }

    /// The keys of the console input line while the console waits for one. True when the
    /// key was taken.
    fn console_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        if !self.takes_input() {
            return false;
        }
        match self.console_input.key(event, cx) {
            ConsoleKey::Ignored => false,
            ConsoleKey::Edited => {
                cx.stop_propagation();
                cx.notify();
                true
            }
            ConsoleKey::Lines(lines) => {
                cx.stop_propagation();
                for line in lines {
                    self.send_console_line(&line, cx);
                }
                true
            }
            ConsoleKey::Eof => {
                cx.stop_propagation();
                self.send_eof(cx);
                true
            }
        }
    }

    /// A key of the tool window: the console takes it while it waits for a line, the tree
    /// of tests otherwise.
    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let stroke = &event.keystroke;
        if stroke.modifiers.control
            && !stroke.modifiers.alt
            && !stroke.modifiers.shift
            && stroke.key == "f"
        {
            cx.stop_propagation();
            self.open_console_search(window, cx);
            return;
        }
        if self.console_search.open && stroke.key == "f3" {
            cx.stop_propagation();
            self.search_step(!stroke.modifiers.shift, cx);
            return;
        }
        if self.console_key(event, cx) {
            return;
        }
        self.test_key(event, window, cx);
    }

    /// The context menu of the console: Send EOF, as IDEA has it, and Explain with AI
    /// on an exception with its stack.
    fn console_menu(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let mut entries = Vec::new();
        if self.takes_input() {
            let panel = cx.weak_entity();
            entries.push(MenuEntry::new("Send EOF", move |_, cx| {
                panel.update(cx, |panel, cx| panel.send_eof(cx)).ok();
            }));
        }
        if crate::fix_with_ai::enabled(cx)
            && let Some((start, end)) = self
                .console_line_at(position)
                .and_then(|index| self.exception_at(index))
        {
            let text = self.exception_text(start, end);
            let workspace = self.workspace.clone();
            entries.push(MenuEntry::new("Explain with AI", move |window, cx| {
                let message = crate::fix_with_ai::exception(&text);
                workspace
                    .update(cx, |workspace, cx| {
                        crate::fix_with_ai::ask(workspace, message.clone(), window, cx)
                    })
                    .ok();
            }));
        }
        if entries.is_empty() {
            return;
        }
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.show_menu(position, entries, cx)
            })
            .ok();
    }

    /// The line of the console under `position`, from zero.
    fn console_line_at(&self, position: Point<Pixels>) -> Option<usize> {
        let handle = self.follow.handle();
        let state = handle.0.borrow();
        let viewport = state.base_handle.bounds();
        if !viewport.contains(&position) {
            return None;
        }
        // The rows of the list all have the height it measured on the last frame, and
        // the offset puts the first of them at the top of the viewport.
        let height = state
            .last_item_size
            .map_or_else(|| px(CODE_LINE_HEIGHT), |size| size.item.height);
        let top = viewport.top() + state.base_handle.offset().y;
        let row = ((position.y - top) / height).floor();
        if row < 0. {
            return None;
        }
        let total = self.console_line_count();
        let kept = self.console_search.kept_count(total);
        let row = row as usize;
        (row < kept).then(|| self.console_search.line_of_row(total, row))
    }

    /// The text of the line of the console on screen at `index`, from zero: the old run
    /// the user looks at, or this one.
    fn console_line(&self, index: usize) -> Option<&str> {
        let line = match self.viewing.and_then(|view| self.draining.get(view)) {
            Some(drain) => drain.lines.get(index)?,
            None => self.lines.get(index)?,
        };
        Some(line.text.as_str())
    }

    /// How many lines the console on screen has.
    fn console_line_count(&self) -> usize {
        self.viewing
            .and_then(|view| self.draining.get(view))
            .map_or(self.lines.len(), |drain| drain.lines.len())
    }

    /// Runs the query of the search bar over the lines on screen.
    fn rebuild_console_search(&mut self) {
        let Self {
            draining,
            lines,
            console_search,
            viewing,
            ..
        } = self;
        let texts = console_texts(viewing.and_then(|view| draining.get(view)), lines);
        console_search.run(&texts);
    }

    /// Applies the filter to the lines on screen and runs the query over the lines it
    /// keeps.
    fn rebuild_console_filter(&mut self) {
        let Self {
            draining,
            lines,
            console_search,
            viewing,
            ..
        } = self;
        let texts = console_texts(viewing.and_then(|view| draining.get(view)), lines);
        let current = console_search.current_line();
        console_search.apply_filter(&texts);
        if console_search.open {
            console_search.run(&texts);
            if let Some(line) = current {
                console_search.focus_line(line);
            }
        }
    }

    /// Ctrl+F: the search bar above the output, ready for a query.
    fn open_console_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.console_search.open = true;
        self.rebuild_console_search();
        window.focus(&self.search_focus, cx);
        self.scroll_to_match();
        cx.notify();
    }

    fn close_console_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.console_search.open = false;
        self.console_search.clear();
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// The keys of the search bar: Escape, Enter, F3 and the text.
    fn search_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let stroke = &event.keystroke;
        if stroke.key == "escape" {
            cx.stop_propagation();
            self.close_console_search(window, cx);
            return;
        }
        if stroke.key == "enter" || stroke.key == "f3" {
            cx.stop_propagation();
            self.search_step(!stroke.modifiers.shift, cx);
            return;
        }
        if stroke.modifiers.alt && !stroke.modifiers.control {
            let toggle = match stroke.key.as_str() {
                "c" => Some(SearchToggle::Case),
                "w" => Some(SearchToggle::Word),
                "x" => Some(SearchToggle::Regex),
                _ => None,
            };
            if let Some(toggle) = toggle {
                cx.stop_propagation();
                self.toggle_search_option(toggle, window, cx);
                return;
            }
        }
        if self
            .console_search
            .field
            .key_with_clipboard(event, false, cx)
        {
            cx.stop_propagation();
            self.rebuild_console_search();
            self.scroll_to_match();
            cx.notify();
        }
    }

    /// F3 and Shift+F3: the next match, or the one before it.
    fn search_step(&mut self, forward: bool, cx: &mut Context<Self>) {
        self.console_search.step(forward);
        self.scroll_to_match();
        cx.notify();
    }

    fn scroll_to_match(&self) {
        let Some(line) = self.console_search.current_line() else {
            return;
        };
        if let Some(row) = self.console_search.row_of_line(line) {
            self.follow.scroll_to(row, ScrollStrategy::Center);
        }
    }

    /// An option of the search bar: the case, the whole word or the regular expression.
    fn toggle_search_option(
        &mut self,
        toggle: SearchToggle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.console_search.toggle(toggle);
        self.rebuild_console_search();
        window.focus(&self.search_focus, cx);
        self.scroll_to_match();
        cx.notify();
    }

    /// The funnel of the console toolbar: the field that keeps only the lines with its
    /// text.
    fn toggle_console_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.console_search.filter_open = !self.console_search.filter_open;
        if self.console_search.filter_open {
            window.focus(&self.filter_focus, cx);
        } else {
            self.console_search.clear_filter();
            self.rebuild_console_filter();
            window.focus(&self.focus, cx);
        }
        cx.notify();
    }

    fn filter_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key == "escape" {
            cx.stop_propagation();
            self.toggle_console_filter(window, cx);
            return;
        }
        if self
            .console_search
            .filter
            .key_with_clipboard(event, false, cx)
        {
            cx.stop_propagation();
            self.rebuild_console_filter();
            cx.notify();
        }
    }

    /// Opens a frame of the console or a place of a compiler message.
    fn open_console_line(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = self.console_line(index).map(str::to_owned) else {
            return;
        };
        let files = self.file_list(cx);
        if let Some(site) = console_links::frame_site(&text) {
            if let Some(path) = site.project_file(&files) {
                self.open_console_place(path, site.line, window, cx);
            } else {
                console_links::open_frame(
                    &text,
                    site.extension(),
                    self.workspace.clone(),
                    window,
                    cx,
                );
            }
            return;
        }
        if let Some((candidate, line)) = console_links::compiler_site(&text)
            && let Some(path) = console_links::compiler_file(&candidate, &self.root, &files)
        {
            self.open_console_place(path, line, window, cx);
        }
    }

    fn open_console_place(
        &mut self,
        path: PathBuf,
        line: u32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let workspace = self.workspace.clone();
        let spot = Some(Spot::Line {
            line: line.saturating_sub(1) as usize,
            column: 0,
        });
        window.defer(cx, move |window, cx| {
            workspace
                .update(cx, |workspace, cx| {
                    navigation::go_to(workspace, path, spot, true, window, cx)
                })
                .ok();
        });
    }

    /// The exception line under `index` with its stack, when the line belongs to one.
    /// A click on a frame finds the exception above it.
    fn exception_at(&self, index: usize) -> Option<(usize, usize)> {
        let mut start = index;
        if !is_exception_head(self.console_line(start)?) {
            start = (0..index)
                .rev()
                .find(|index| self.console_line(*index).is_some_and(is_exception_head))?;
        }
        let mut end = start + 1;
        while self.console_line(end).is_some_and(is_stack_line) {
            end += 1;
        }
        Some((start, end))
    }

    /// The text of the exception and its stack, from the lines `start..end`.
    fn exception_text(&self, start: usize, end: usize) -> String {
        (start..end)
            .filter_map(|index| self.console_line(index))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The work that runs behind the console: the import of the project and a build.
    pub fn background_tasks(&self) -> Vec<&'static str> {
        let mut tasks = Vec::new();
        if self.loading {
            tasks.push("Project import");
        }
        if self.building_project || self.build_console.is_some() {
            tasks.push("Build");
        }
        tasks
    }

    /// Ctrl+Shift+F2: stops the only background task, or asks which one of several.
    pub fn stop_background(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let tasks = self.background_tasks();
        match tasks.as_slice() {
            [] => self.tell("Background Tasks", "Nothing runs in the background", cx),
            [task] => self.stop_background_task(task, cx),
            several => {
                let me = cx.weak_entity();
                let entries = several
                    .iter()
                    .map(|task| {
                        let me = me.clone();
                        let task = *task;
                        MenuEntry::new(task, move |_, cx| {
                            me.update(cx, |this, cx| this.stop_background_task(task, cx))
                                .ok();
                        })
                        .icon("run/stop")
                    })
                    .collect();
                self.workspace
                    .update(cx, |workspace, cx| {
                        workspace.show_menu(position, entries, cx)
                    })
                    .ok();
            }
        }
    }

    /// Stops one task of [`RunPanel::background_tasks`] by its name.
    pub fn stop_background_task(&mut self, task: &str, cx: &mut Context<Self>) {
        match task {
            "Project import" => {
                if let Some(session) = &self.session {
                    session.cancel_sync();
                }
                self.loading = false;
                self.waiting = false;
                self.tell("Project import", "The import was stopped", cx);
            }
            "Build" => {
                if let Some(console) = &self.build_console {
                    console.stop();
                }
                if let Some(console) = &self.console {
                    console.stop();
                }
                if self.building_project {
                    self.build_stopped = true;
                    self.build_verdict = Some((
                        false,
                        self.build_started
                            .map_or(Duration::ZERO, |started| started.elapsed()),
                    ));
                }
                self.build_console = None;
                self.building_project = false;
                self.refresh_build_view();
                self.tell("Build", "The build was stopped", cx);
            }
            _ => {}
        }
        self.changed(cx);
    }

    /// Starts a launch that plans in the background, and answers the cancel of it with the
    /// number of this launch. Stop cancels it, and a plan of a launch that comes after a
    /// newer one started starts nothing.
    fn begin_launch(&mut self) -> (Cancel, u64) {
        let cancel = Cancel::new();
        self.launch = cancel.clone();
        self.launch_generation = self.launch_generation.wrapping_add(1);
        self.starting = true;
        (cancel, self.launch_generation)
    }

    /// Whether the plan of that launch may still start something: Stop did not cancel it,
    /// and no newer launch overtook it.
    fn launch_still_wanted(&self, cancel: &Cancel, generation: u64) -> bool {
        self.launch_generation == generation && !cancel.is_cancelled()
    }

    /// A build beside a running test session: output goes to the same console, but the
    /// test run, its tree and its title stay as they are.
    fn launch_build(
        &mut self,
        title: String,
        cx: &mut Context<Self>,
        make: impl FnOnce() -> Result<RunPlan, String> + Send + 'static,
    ) {
        let (cancel, generation) = self.begin_launch();
        self.building_project = true;
        self.lines.push_back(info(&title));
        let plan = cx.background_executor().spawn(async move { make() });
        cx.spawn(async move |this, cx| {
            let plan = plan.await;
            this.update(cx, |this, cx| {
                if !this.launch_still_wanted(&cancel, generation) {
                    this.changed(cx);
                    return;
                }
                this.starting = false;
                match plan {
                    Ok(plan) => {
                        this.build_output = OutputBuffers::default();
                        this.build_process = None;
                        this.build_console = Some(Console::start(plan));
                        this.build_running = true;
                    }
                    Err(message) => {
                        this.building_project = false;
                        this.note(&message, true);
                        this.tell_error("Cannot run", message, cx);
                    }
                }
                this.changed(cx);
            })
            .ok();
        })
        .detach();
        self.changed(cx);
    }

    /// Planning can run a tool, so it happens in the background.
    fn launch(
        &mut self,
        title: String,
        config: Option<String>,
        cx: &mut Context<Self>,
        make: impl FnOnce() -> Result<(RunPlan, Option<Arc<dyn LiveApplication>>), String>
        + Send
        + 'static,
    ) {
        let (cancel, generation) = self.begin_launch();
        self.archive_console();
        self.live = None;
        self.live_open = false;
        self.run_config = config;
        self.run_service = ServiceRun::default();
        if self.run_config.is_some() {
            self.run_service.starting();
        }
        crate::history::put_label(&self.root, format!("Before running {title}"), cx);
        self.title = title.into();
        self.lines.clear();
        self.console_search.clear();
        self.output = OutputBuffers::default();
        self.console_process = None;
        self.program_pid = None;
        self.console_input.restart();
        self.program_started = false;
        self.tests.clear();
        cx.emit(TestsChanged);
        let plan = cx.background_executor().spawn(async move { make() });
        cx.spawn(async move |this, cx| {
            let plan = plan.await;
            this.update(cx, |this, cx| {
                if !this.launch_still_wanted(&cancel, generation) {
                    this.changed(cx);
                    return;
                }
                this.starting = false;
                match plan {
                    Ok((plan, live)) => {
                        this.set_live(live, cx);
                        this.console = Some(Console::start(plan));
                        this.console_running = true;
                    }
                    Err(message) => {
                        if this.building_project {
                            this.build_verdict = Some((
                                false,
                                this.build_started
                                    .map_or(Duration::ZERO, |started| started.elapsed()),
                            ));
                        }
                        this.building_project = false;
                        this.refresh_build_view();
                        this.run_service.stopped();
                        this.note(&message, true);
                        this.tell_error("Cannot run", message, cx);
                    }
                }
                this.changed(cx);
            })
            .ok();
        })
        .detach();
        self.changed(cx);
    }

    pub fn stop(&mut self, cx: &mut Context<Self>) {
        self.rerun = false;
        self.confirm_rerun = None;
        self.auto_pending = false;
        self.waiting = false;
        self.waiting_tests = None;
        self.queued_tests = None;
        self.debugging_tests = false;
        // A launch that still plans in the background starts nothing when it comes.
        if self.starting {
            self.launch.cancel();
            self.launch_generation = self.launch_generation.wrapping_add(1);
            if !self.building_project {
                self.note("Launch cancelled", false);
            }
        } else if self.console.is_some() && !self.program_started {
            // The before launch steps still run, so the program itself never starts.
            self.note("Launch cancelled", false);
        }
        self.starting = false;
        if self.building_project {
            self.build_stopped = true;
            self.build_verdict = Some((
                false,
                self.build_started
                    .map_or(Duration::ZERO, |started| started.elapsed()),
            ));
        }
        self.building_project = false;
        self.build_console = None;
        self.build_running = false;
        self.console_running = false;
        self.refresh_build_view();
        self.program_pid = None;
        self.reload = None;
        if self.run_service.state().active() {
            self.run_service.stopped();
        }
        for drain in &mut self.draining {
            if drain.running {
                if drain.service.state().active() {
                    drain.service.stopped();
                }
                drain.running = false;
            }
        }
        if let Some(starting) = &self.starting_tests {
            starting.cancel();
        }
        self.stop_running();
        self.stop_live(cx);
        self.changed(cx);
    }

    /// The Dump Threads button: asks the JDK of the project for the threads of the
    /// program that runs now. The answer fills the Stack Trace tool window.
    fn dump_threads(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pid) = self.program_pid else {
            self.note(
                "Nothing is running: start a program to dump its threads",
                true,
            );
            self.changed(cx);
            return;
        };
        let support = cx
            .try_global::<Semantic>()
            .map(|semantic| semantic.0.clone());
        let Some(provider) = support.as_ref().and_then(|support| support.thread_dump()) else {
            self.note("The language cannot dump the threads of a program", true);
            self.changed(cx);
            return;
        };
        let mut replies = ask(provider, DumpQuery(pid));
        let workspace = self.workspace.clone();
        let handle = window.window_handle();
        self.note("Dumping the threads of the program", false);
        self.changed(cx);
        cx.spawn(async move |this, cx| {
            let mut shown = false;
            while let Some(answer) = replies.next().await {
                let note = answer.note.clone();
                let dump = answer.value;
                if let Some(error) = dump.error {
                    this.update(cx, |this, cx| {
                        this.note(&error, true);
                        this.changed(cx);
                    })
                    .ok();
                    return;
                }
                let Some(report) = dump.report else {
                    return;
                };
                let first = !shown;
                shown = true;
                let workspace = workspace.clone();
                this.update(cx, |_, cx| {
                    cx.defer(move |cx| {
                        if first {
                            handle
                                .update(cx, |_, window, cx| {
                                    workspace
                                        .update(cx, |workspace, cx| {
                                            workspace.show_tool(STACK_TRACE_TOOL, window, cx)
                                        })
                                        .ok();
                                })
                                .ok();
                        }
                        if let Some(panel) = cx.try_global::<ActiveStackTrace>() {
                            panel.0.clone().update(cx, |panel, cx| {
                                if first {
                                    panel.show(report, cx);
                                } else {
                                    panel.refine(report, cx);
                                }
                                panel.set_note(note.clone(), cx);
                            });
                        }
                    });
                })
                .ok();
            }
        })
        .detach();
    }

    /// Shift+F9: starts the selected configuration under the debugger.
    pub fn debug(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.may_run_project_code("Debug", cx) {
            return;
        }
        let handle = window.window_handle();
        self.start_debug(Some(handle), None, cx);
    }

    /// Starts the configuration named `name` under the debugger for a tool call. The
    /// tool runs on a worker thread, so the window comes from the active one when there
    /// is one.
    pub(crate) fn debug_configuration_from_tool(
        &mut self,
        name: &str,
        client: Option<&str>,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        if self.debug_service.is_none() {
            return Err("the language server is not running".to_string());
        }
        if self
            .session
            .as_ref()
            .and_then(ProjectSession::model)
            .is_none()
        {
            return Err("the project is still loading".to_string());
        }
        if self.starting || self.busy() {
            return Err("the Run window is busy".to_string());
        }
        if !self.select_named(name, cx) {
            return Err(format!("the project has no run configuration {name}"));
        }
        let handle = cx.active_window();
        self.start_debug(handle, client, cx);
        Ok(())
    }

    /// Starts a configuration under the debugger for a tool call, with the arguments, the
    /// directory and the variables of the call in place of the ones the configuration
    /// keeps. A configuration whose provider does not declare a field refuses the change.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn debug_configuration_from_tool_with(
        &mut self,
        name: &str,
        program_arguments: Option<&str>,
        working_directory: Option<&std::path::Path>,
        envs: &[String],
        client: Option<&str>,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        if self.debug_service.is_none() {
            return Err("the language server is not running".to_string());
        }
        if self
            .session
            .as_ref()
            .and_then(ProjectSession::model)
            .is_none()
        {
            return Err("the project is still loading".to_string());
        }
        if self.starting || self.busy() {
            return Err("the Run window is busy".to_string());
        }
        if !self.select_named(name, cx) {
            return Err(format!("the project has no run configuration {name}"));
        }
        let config =
            self.configuration_with_overrides(name, program_arguments, working_directory, envs)?;
        // The overrides live in the copy of this one start, so the configuration of the
        // user keeps its arguments, folder and variables.
        self.pending_config = Some(config);
        let handle = cx.active_window();
        self.start_debug(handle, client, cx);
        Ok(())
    }

    fn start_debug(
        &mut self,
        handle: Option<ui_kit::gpui::AnyWindowHandle>,
        client: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        let Some(service) = self.debug_service.clone() else {
            self.tell_warning("Cannot debug", "The language server is not running", cx);
            return;
        };
        let Some(model) = self.session.as_ref().and_then(ProjectSession::model) else {
            self.tell_warning("Cannot debug", "The project is still loading", cx);
            return;
        };
        let Some(selected) = self.selected else {
            self.tell_warning("Cannot debug", "There is no configuration to debug", cx);
            return;
        };
        let Some(selected_config) = self.configs.get(selected).cloned() else {
            return;
        };
        let config = self.take_launch_config(&selected_config);
        let Some(provider) = self
            .providers
            .iter()
            .find(|provider| provider.id() == config.provider)
        else {
            self.tell_warning("Cannot debug", "The configuration has no provider", cx);
            return;
        };
        // A configuration of tests starts under the debugger through the test runner,
        // which puts the debug agent into the JVM of the tests.
        if let Some(request) = provider.test_request(&config) {
            let handle = handle.or_else(|| cx.active_window());
            self.debug_tests_with(request, handle, client, cx);
            return;
        }
        // The plan may look at every source file to see whether a build is due, so it is
        // made in the background.
        let provider = provider.clone();
        let name = config.name.clone();
        self.note_tool_client(client);
        let plan = cx.background_executor().spawn(async move {
            if let Some(attach) = provider.attach_plan(&config, &model)? {
                return Ok((Some(attach), None, None));
            }
            let live = provider.live_application(&config, &model);
            provider
                .debug_plan(&config, &model)
                .map(|plan| (None, Some(plan), live))
        });
        let panel = cx.global::<crate::debug::ActiveDebug>().0.clone();
        let workspace = self.workspace.clone();
        cx.spawn(async move |this, cx| {
            let plan = match plan.await {
                Ok(plan) => plan,
                Err(error) => {
                    this.update(cx, |this, cx| {
                        this.tell_error("Cannot debug", error.to_string(), cx)
                    })
                    .ok();
                    return;
                }
            };
            let live = plan.2.clone();
            this.update(cx, |this, cx| this.set_live(live, cx)).ok();
            cx.update(|cx| {
                if let (Some(attach), _, _) = &plan {
                    let started = panel.update(cx, |panel, cx| {
                        panel.start_attach(
                            service,
                            attach.host.clone(),
                            attach.port,
                            attach.name.clone(),
                            attach.project.clone(),
                            cx,
                        )
                    });
                    if let Err(error) = started {
                        workspace
                            .update(cx, |workspace, cx| {
                                workspace.notify_error("Cannot attach", error, cx)
                            })
                            .ok();
                        return;
                    }
                } else if let (_, Some(plan), _) = plan {
                    let entry = workspace
                        .read_with(cx, |workspace, _| workspace.active_path())
                        .ok()
                        .flatten();
                    panel.update(cx, |panel, cx| {
                        panel.start_plan(service, plan, name, entry, cx)
                    });
                }
                if let Some(handle) = handle {
                    handle
                        .update(cx, |_, window, cx| {
                            workspace
                                .update(cx, |workspace, cx| {
                                    workspace.show_tool(DEBUG_TOOL, window, cx)
                                })
                                .ok();
                        })
                        .ok();
                }
            });
        })
        .detach();
    }

    /// Starts a test run with a debug agent and attaches to its JVM.
    pub fn debug_tests(
        &mut self,
        request: TestRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let handle = window.window_handle();
        self.debug_tests_with(request, Some(handle), None, cx);
    }

    /// Starts a test under the debugger for a tool call. The tool runs on a worker
    /// thread, so the window comes from the active one when there is one.
    pub(crate) fn debug_tests_from_tool(
        &mut self,
        request: TestRequest,
        client: Option<&str>,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        if self.starting || self.busy() {
            return Err("the Run window is busy".to_string());
        }
        if self.debug_service.is_none() {
            return Err("the language server is not running".to_string());
        }
        if self.test_provider.is_none() {
            return Err("the project has no test runner".to_string());
        }
        if self
            .session
            .as_ref()
            .and_then(ProjectSession::model)
            .is_none()
        {
            return Err("the project is still loading".to_string());
        }
        let handle = cx.active_window();
        self.debug_tests_with(request, handle, client, cx);
        Ok(())
    }

    fn debug_tests_with(
        &mut self,
        request: TestRequest,
        handle: Option<ui_kit::gpui::AnyWindowHandle>,
        client: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        if self.starting || self.busy() {
            self.tell_warning("Busy", "Stop what is running first", cx);
            return;
        }
        let Some(service) = self.debug_service.clone() else {
            self.tell_warning("Cannot debug", "The language server is not running", cx);
            return;
        };
        let Some(provider) = self.test_provider.clone() else {
            self.tell_warning("Cannot debug tests", "The project has no build tool", cx);
            return;
        };
        let Some(model) = self.session.as_ref().and_then(ProjectSession::model) else {
            self.tell_warning("Cannot debug tests", "The project is still loading", cx);
            return;
        };
        self.waiting_tests = None;
        self.queued_tests = None;
        self.rerun = false;
        self.archive_console();
        self.run_config = None;
        self.run_service = ServiceRun::default();
        self.title = title_of(&request).into();
        self.note_tool_client(client);
        self.lines.clear();
        self.output = OutputBuffers::default();
        self.console_process = None;
        self.program_pid = None;
        self.tests.begin(request.clone());
        self.starting = true;
        self.debugging_tests = true;
        let title = title_of(&request);
        // The key that starts this may come from an editor inside the workspace update.
        let workspace = self.workspace.clone();
        if let Some(handle) = handle {
            cx.defer(move |cx| {
                handle
                    .update(cx, |_, window, cx| {
                        workspace
                            .update(cx, |workspace, cx| {
                                workspace.show_tool(DEBUG_TOOL, window, cx)
                            })
                            .ok();
                    })
                    .ok();
            });
        }
        let stop = Cancel::new();
        self.starting_tests = Some(stop.clone());
        let plan = cx.background_executor().spawn(async move {
            TestRun::debug_start_with_stop(&*provider, &model, &request, &stop)
                .map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| {
            let started = plan.await;
            this.update(cx, |this, cx| {
                let stopped = this
                    .starting_tests
                    .take()
                    .is_some_and(|stop| stop.is_cancelled());
                this.starting = false;
                match started {
                    Ok((run, _debug)) if stopped => {
                        drop(run);
                        this.debugging_tests = false;
                        this.note("Stopped by the user", false);
                        this.tests.finished = true;
                    }
                    Ok((run, debug)) => {
                        this.tests.run = Some(run);
                        let panel = cx.global::<crate::debug::ActiveDebug>().0.clone();
                        let attached = panel.update(cx, |panel, cx| {
                            panel.start_attach_ready(
                                service,
                                debug.host,
                                debug.port,
                                title.clone(),
                                None,
                                cx,
                            )
                        });
                        if let Err(error) = attached {
                            this.debugging_tests = false;
                            this.stop_running();
                            this.tell_error("Cannot debug tests", error, cx);
                        }
                    }
                    Err(message) if stopped || message == STOPPED_BY_USER => {
                        this.debugging_tests = false;
                        this.note("Stopped by the user", false);
                        this.tests.finished = true;
                    }
                    Err(message) => {
                        this.debugging_tests = false;
                        this.note(&message, true);
                        this.tell_error("Cannot debug tests", message, cx);
                        this.tests.finished = true;
                    }
                }
                cx.emit(TestsChanged);
                this.changed(cx);
            })
            .ok();
        })
        .detach();
        cx.emit(TestsChanged);
        self.changed(cx);
    }

    fn poll(&mut self, cx: &mut Context<Self>) {
        self.show_notice(cx);
        self.drain_debug_live(cx);
        let mut changed = self.poll_sync(cx);
        changed |= self.poll_tool_download(cx);
        changed |= self.poll_wrapper_progress(cx);
        let mut done = Vec::new();
        // The test console of a debug run shows the lines the parser has completed, so a
        // piece of a line held back here is not cut in two there.
        let debug_from = self.debugging_tests.then_some(done.len());
        let mut exited: Option<Option<i32>> = None;
        let mut program_ended = false;
        let mut closed = false;
        let mut built = false;
        // A test holds the first look at a run until the run is over, so this poll reads all
        // of its events at once, as a loaded machine makes it read them.
        #[cfg(test)]
        if self.observation_held
            && let Some(console) = &self.console
        {
            console.wait_for_output(HOLD_MARKER, HOLD_LIMIT, &Cancel::new());
        }
        let main_console = self.console.is_some();
        let tests = self.tests.run.as_ref().map(TestRun::console);
        if let Some(console) = self.console.as_ref().or(tests) {
            for _ in 0..EVENTS_PER_POLL {
                let Ok(event) = console.events().try_recv() else {
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
                            .push(process, stream == Stream::Stderr, &text, &mut done);
                    }
                    ConsoleEvent::Started { pid, step } => {
                        if let Some(job) = &mut self.tool_job {
                            job.process_started = true;
                        }
                        self.console_process = Some(pid);
                        if step == Step::Program {
                            self.program_pid = Some(pid);
                        }
                        if step == Step::Before {
                            done.push(info("Building the project"));
                        }
                        if step == Step::Program && main_console {
                            self.program_started = true;
                            self.run_service.running(None);
                        }
                    }
                    ConsoleEvent::Failed { message } => {
                        if let Some(job) = &mut self.tool_job {
                            job.start_error = Some(message.clone());
                        }
                        done.push(Line::plain(message, error_style()));
                    }
                    ConsoleEvent::Exited { code, step } => {
                        if step == Step::Program {
                            self.program_pid = None;
                        }
                        // A build that failed ends the run as well: the configuration is stopped,
                        // not starting. A build that Stop already ended keeps its stopped state.
                        if main_console
                            && (step == Step::Program
                                || (code != Some(0) && self.run_service.state().active()))
                        {
                            self.run_service.exited(code);
                        }
                        match self.console_process.take() {
                            Some(process) => self.output.finish(process, &mut done),
                            None => self.output.finish_all(&mut done),
                        }
                        done.push(Line::default());
                        if step == Step::Before && code != Some(0) {
                            done.push(Line::plain(build_failed_text(code), error_style()));
                            done.push(Line::plain(
                                "The before launch step failed, so the program did not start",
                                error_style(),
                            ));
                        } else {
                            done.push(info(exit_text(code, step)));
                        }
                        self.console_running = false;
                        if step == Step::Before && code == Some(0) {
                            built = true;
                        }
                        if step == Step::Program {
                            program_ended = true;
                        }
                        exited = Some(code);
                    }
                    // The pipes are closed: the last output, late as it was, is in.
                    ConsoleEvent::Closed => {
                        self.output.finish_all(&mut done);
                        self.console_process = None;
                        self.program_pid = None;
                        closed = true;
                        self.console_running = false;
                        break;
                    }
                }
            }
        }
        if let Some(console) = &self.console
            && let Some(message) = console.take_input_error()
        {
            changed = true;
            done.push(Line::plain(message, error_style()));
        }
        if let Some(from) = debug_from {
            let lines: Vec<(String, bool)> = done[from..]
                .iter()
                .filter(|line| !line.text.is_empty())
                .map(|line| {
                    let error = line.spans.first().is_some_and(|(_, style)| style.error);
                    (line.text.clone(), error)
                })
                .collect();
            if !lines.is_empty()
                && let Some(debug) = cx.try_global::<crate::debug::ActiveDebug>()
            {
                let panel = debug.0.clone();
                panel.update(cx, |panel, cx| {
                    for (text, error) in lines {
                        panel.push_output(text, error, cx);
                    }
                });
            }
        }
        let mut build_exited: Option<Option<i32>> = None;
        let mut build_closed = false;
        let mut build_done = Vec::new();
        if let Some(console) = self.build_console.as_ref() {
            for _ in 0..EVENTS_PER_POLL {
                let Ok(event) = console.events().try_recv() else {
                    break;
                };
                changed = true;
                match event {
                    ConsoleEvent::Output {
                        process,
                        stream,
                        text,
                    } => {
                        self.build_output.push(
                            process,
                            stream == Stream::Stderr,
                            &text,
                            &mut build_done,
                        );
                    }
                    ConsoleEvent::Started { pid, .. } => self.build_process = Some(pid),
                    ConsoleEvent::Failed { message } => {
                        build_done.push(Line::plain(message, error_style()));
                    }
                    ConsoleEvent::Exited { code, .. } => {
                        match self.build_process.take() {
                            Some(process) => self.build_output.finish(process, &mut build_done),
                            None => self.build_output.finish_all(&mut build_done),
                        }
                        build_done.push(Line::default());
                        build_done.push(info(exit_text(code, Step::Program)));
                        self.build_running = false;
                        build_exited = Some(code);
                    }
                    // The build is over; a daemon may still hold its pipes.
                    ConsoleEvent::Closed => {
                        self.build_output.finish_all(&mut build_done);
                        self.build_process = None;
                        build_closed = true;
                        break;
                    }
                }
            }
        }
        done.append(&mut build_done);
        // Late output of a run that has already been replaced keeps filling its own
        // console, not the one of the run that started after it.
        for drain in &mut self.draining {
            let mut closed = false;
            for _ in 0..EVENTS_PER_POLL {
                let Some(console) = &drain.console else {
                    break;
                };
                let Ok(event) = console.events().try_recv() else {
                    break;
                };
                changed = true;
                match event {
                    ConsoleEvent::Output {
                        process,
                        stream,
                        text,
                    } => {
                        drain.output.push(
                            process,
                            stream == Stream::Stderr,
                            &text,
                            &mut drain.lines,
                        );
                    }
                    ConsoleEvent::Started { pid, step } => {
                        drain.process = Some(pid);
                        if step == Step::Program {
                            drain.service.running(None);
                        }
                    }
                    ConsoleEvent::Exited { code, step } => {
                        match drain.process.take() {
                            Some(process) => drain.output.finish(process, &mut drain.lines),
                            None => drain.output.finish_all(&mut drain.lines),
                        }
                        drain.lines.push(Line::default());
                        drain.lines.push(info(exit_text(code, step)));
                        drain.running = false;
                        if step == Step::Program {
                            drain.service.exited(code);
                        }
                    }
                    ConsoleEvent::Failed { message } => {
                        drain.lines.push(Line::plain(message, error_style()));
                    }
                    ConsoleEvent::Closed => {
                        drain.output.finish_all(&mut drain.lines);
                        drain.process = None;
                        drain.running = false;
                        closed = true;
                        break;
                    }
                }
            }
            if closed {
                drain.console = None;
            }
        }
        self.feed_live(
            &done,
            program_ended || (closed && main_console) || exited.is_some(),
            cx,
        );
        self.lines.extend(done);
        let extra = self.lines.len().saturating_sub(MAX_LINES);
        self.lines.drain(..extra);
        if build_closed {
            self.build_console = None;
        }
        if let Some(code) = build_exited {
            self.finish_build(code, cx);
        }
        if closed && main_console {
            // The program is over and its pipes are closed.
            self.console = None;
        }
        if exited.is_some() && !main_console {
            self.tests.exited = Some(std::time::Instant::now());
        }
        if let Some(code) = exited
            && self.tool_job.is_some()
        {
            self.tool_exit = Some(code);
        }
        if built {
            self.mark_built();
        }
        if let Some(code) = exited {
            self.finish_build(code, cx);
        }
        #[cfg(test)]
        let tests_ran = self.tests.run.is_some();
        changed |= self.poll_tests(cx);
        let coverage_started =
            exited.is_some() || (self.console.is_none() && !self.console_running);
        if self.coverage_pending.is_some()
            && coverage_started
            && !self.starting
            && self.tests.run.is_none()
        {
            self.finish_coverage(cx);
        }
        let profile_started = program_ended || (self.console.is_none() && !self.console_running);
        if self.profile_pending.is_some()
            && profile_started
            && !self.starting
            && self.tests.run.is_none()
        {
            self.finish_profile(cx);
        }
        // A test that waits for a process waits for this, not for a clock: the console is
        // closed, so every line of the program is in, and a test run is over only when
        // its result stream is.
        #[cfg(test)]
        if (closed && main_console) || (tests_ran && self.tests.run.is_none()) {
            self.finished += 1;
        }
        changed |= self.poll_reload(cx);
        // The port a line of the program named reaches the Services window as the line
        // itself reaches the live tab.
        if let Some(port) = self.live.as_ref().and_then(|live| live.read(cx).app_port())
            && self.run_service.port(port)
        {
            changed = true;
        }
        if self.debugging_tests && !self.starting {
            let ended = cx
                .try_global::<crate::debug::ActiveDebug>()
                .is_none_or(|debug| !debug.0.read(cx).state().0);
            if ended {
                self.debugging_tests = false;
                self.stop_running();
                changed = true;
            }
        }
        changed |= self.poll_tool_jobs(cx);
        if changed && self.building_project {
            self.refresh_build_view();
        }
        // The lines of the console changed, so the filter and the query see the new ones.
        if changed && (self.console_search.open || self.console_search.filtering()) {
            self.rebuild_console_filter();
        }
        if changed {
            // The console waits for a line below the last one, so that row is in view too,
            // unless the reader scrolled up: then their place stays. A search keeps the
            // view on its match instead.
            if !self.console_search.open {
                let kept = self.console_search.kept_count(self.console_line_count());
                let last = kept + usize::from(self.takes_input());
                self.follow.follow_end(last);
            }
            self.changed(cx);
        }
        if exited.is_some()
            && let Some(request) = self.queued_tests.take()
        {
            self.run_tests(request, cx);
        } else if exited.is_some() && std::mem::take(&mut self.rerun) {
            self.start(cx);
        } else if closed && main_console {
            // Rerun was asked for after the exit code was on screen but before the last
            // output arrived; the console above is gone now, so the start can go.
            if let Some(request) = self.queued_tests.take() {
                self.run_tests(request, cx);
            } else if std::mem::take(&mut self.rerun) {
                self.start(cx);
            }
        }
    }

    /// A line the language sent once, such as a setting that made its server restart.
    fn show_notice(&self, cx: &mut Context<Self>) {
        let Some(message) = self
            .environment
            .as_ref()
            .and_then(|environment| environment.take_notice())
        else {
            return;
        };
        let _ = self.workspace.update(cx, |workspace, cx| {
            workspace.notify_warning("Project", message, cx);
        });
    }

    fn poll_sync(&mut self, cx: &mut Context<Self>) -> bool {
        let mut discover = false;
        let mut changed = false;
        let mut model_changed = false;
        let mut start_waiting = false;
        while let Some(delivery) = self.sync.as_mut().and_then(|sync| sync.try_next()) {
            changed = true;
            match delivery {
                Delivery::Event(SyncEvent::Preliminary) | Delivery::Lost(_) => {
                    discover = true;
                    model_changed = true;
                }
                Delivery::Event(SyncEvent::Synced) => {
                    self.loading = false;
                    self.tool_setup = None;
                    discover = true;
                    model_changed = true;
                    start_waiting = true;
                }
                Delivery::Event(SyncEvent::Failed(message)) => {
                    self.loading = false;
                    self.problems.push(message.clone());
                    match self
                        .tool
                        .as_ref()
                        .and_then(|tool| tool.missing_tool(&self.root))
                    {
                        Some(setup) => {
                            self.tool_setup = Some(setup.clone());
                            self.show_tool_setup(&setup, cx);
                        }
                        None => {
                            self.tool_setup = None;
                            self.tell_error("Project sync failed", message, cx);
                        }
                    }
                    start_waiting = true;
                }
            }
        }
        if model_changed
            && let Some(model) = self.session.as_ref().and_then(ProjectSession::model)
            && let Some(environment) = &self.environment
        {
            environment.project_model(&model);
        }
        if discover {
            self.discover(cx);
        }
        if start_waiting && let Some(request) = self.waiting_tests.take() {
            self.run_tests(request, cx);
        } else if start_waiting && std::mem::take(&mut self.waiting) {
            self.start(cx);
        }
        if model_changed {
            cx.emit(ProjectModelChanged);
        }
        changed
    }

    /// The notification of a missing build tool: its buttons run commands of the window,
    /// which reach this panel through the globals of the application.
    fn show_tool_setup(&self, setup: &ToolSetup, cx: &mut Context<Self>) {
        let mut buttons = Vec::new();
        if let Some(label) = &setup.download {
            buttons.push(("download", label.clone().into(), "build_tool.install"));
        }
        if let Some(label) = &setup.choose {
            buttons.push(("choose", label.clone().into(), "build_tool.choose"));
        }
        if let Some(label) = &setup.create_wrapper {
            buttons.push((
                "create-wrapper",
                label.clone().into(),
                "build_tool.create_wrapper",
            ));
        }
        if let Some(label) = &setup.use_downloaded {
            buttons.push((
                "use-downloaded",
                label.clone().into(),
                "build_tool.use_downloaded",
            ));
        }
        if let Some((label, row)) = &setup.settings {
            // A tool may live on the second page of Settings: its notification opens the
            // row of that page and not the first one.
            let command = match *row {
                "build_tool_second_home" => "settings.build_tool_second",
                _ => "settings.build_tools",
            };
            buttons.push(("settings", label.clone().into(), command));
        }
        let _ = self.workspace.update(cx, |workspace, cx| {
            workspace.notify_warning_with_commands(
                setup.title.clone(),
                setup.text.clone(),
                buttons,
                cx,
            );
        });
    }

    /// Downloads the missing build tool in a background thread. The status line and the
    /// notification follow its bytes, and Cancel stops it; the sync follows when it ends.
    pub(crate) fn download_tool(&mut self, cx: &mut Context<Self>) {
        self.start_tool_job(MissingToolJob::Download, cx);
    }

    /// Writes the wrapper of the project with the tool, in a background thread.
    pub(crate) fn create_tool_wrapper(&mut self, cx: &mut Context<Self>) {
        self.start_tool_job(MissingToolJob::CreateWrapper, cx);
    }

    /// Uses the copy of the tool the IDE downloaded, without writing a wrapper.
    pub(crate) fn use_downloaded_tool(&mut self, cx: &mut Context<Self>) {
        self.start_tool_job(MissingToolJob::UseDownloaded, cx);
    }

    fn start_tool_job(&mut self, job: MissingToolJob, cx: &mut Context<Self>) {
        if self.tool_download.is_some() {
            return;
        }
        let Some(setup) = self.tool_setup.clone() else {
            return;
        };
        let Some(tool) = self.tool.clone() else {
            return;
        };
        let progress = Arc::new(ToolProgress::new());
        let cancel = Cancel::new();
        let done = Arc::new(AtomicBool::new(false));
        let result: Arc<Mutex<Option<ToolDownloadAnswer>>> = Arc::new(Mutex::new(None));
        let job_progress = progress.clone();
        let job_cancel = cancel.clone();
        let job_done = done.clone();
        let job_result = result.clone();
        // The transfer blocks, so it lives on a background thread; the panel watches the
        // flag rather than the task.
        cx.background_executor()
            .spawn(async move {
                let answer = match job {
                    MissingToolJob::Download => tool.install_tool(&job_progress, &job_cancel),
                    MissingToolJob::CreateWrapper => {
                        tool.create_wrapper(&job_progress, &job_cancel)
                    }
                    MissingToolJob::UseDownloaded => {
                        tool.use_downloaded(&job_progress, &job_cancel)
                    }
                }
                .map_err(|error| error.to_string());
                if let Ok(mut slot) = job_result.lock() {
                    *slot = Some(answer);
                }
                job_done.store(true, Ordering::Release);
            })
            .detach();
        self.tool_download = Some(ToolDownload {
            name: setup.name.clone(),
            job,
            progress,
            cancel,
            done,
            result,
            drawn: Instant::now(),
        });
        self.tell_tool_download(&setup.name, cx);
    }

    /// Stops the transfer between its pieces. The job removes the half file it leaves.
    pub(crate) fn cancel_tool_download(&mut self, cx: &mut Context<Self>) {
        if let Some(job) = &self.tool_download {
            job.cancel.cancel();
            cx.notify();
        }
    }

    /// Asks the user for the home folder of the build tool and uses it from now on.
    pub(crate) fn choose_tool_home(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: None,
        });
        cx.spawn_in(window, async move |this, cx| {
            let chosen = paths
                .await
                .ok()
                .and_then(Result::ok)
                .flatten()
                .and_then(|mut paths| paths.pop());
            let Some(folder) = chosen else {
                return;
            };
            this.update(cx, |this, cx| this.apply_tool_home(folder, cx))
                .ok();
        })
        .detach();
    }

    fn apply_tool_home(&mut self, folder: PathBuf, cx: &mut Context<Self>) {
        let display = folder.display().to_string();
        let _ = self.workspace.update(cx, |workspace, cx| {
            workspace.update_settings(
                |settings| {
                    settings.build_tools.first.home = display;
                    settings.build_tools.first.home_kind = ToolHome::Custom;
                },
                cx,
            );
        });
        if let Some(tool) = &self.tool {
            tool.use_home(&folder);
        }
        self.tool_setup = None;
        self.reload_project(cx);
    }

    /// Writes the bytes of the transfer into the status line and the notification.
    fn tell_tool_download(&self, name: &str, cx: &mut Context<Self>) {
        let text = match &self.tool_download {
            Some(job) if job.job != MissingToolJob::Download && job.progress.written() == 0 => {
                format!("Preparing {name}\u{2026}")
            }
            Some(job) => download_text(name, &job.progress),
            None => return,
        };
        let title = match self.tool_download.as_ref().map(|job| job.job) {
            Some(MissingToolJob::CreateWrapper) => format!("Creating the {name} wrapper"),
            Some(MissingToolJob::UseDownloaded) => format!("Preparing {name}"),
            _ => format!("Downloading {name}"),
        };
        let workspace = self.workspace.clone();
        // A button of the notification reaches here while the workspace is being updated,
        // so the write waits for the end of that update.
        cx.defer(move |cx| {
            let shown = text.clone();
            let _ = workspace.update(cx, |workspace, cx| {
                workspace.notify_user_with_commands(
                    title,
                    shown.clone(),
                    vec![("cancel", "Cancel".into(), "build_tool.cancel")],
                    cx,
                );
                workspace.set_language_status(Some(shown.into()), cx);
            });
        });
    }

    /// Follows the transfer: redraws its text now and then, and finishes it once. True
    /// when the window changed.
    fn poll_tool_download(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(job) = self.tool_download.as_ref() else {
            return false;
        };
        if !job.done.load(Ordering::Acquire) {
            if job.drawn.elapsed() < Duration::from_millis(200) {
                return false;
            }
            let name = job.name.clone();
            if let Some(job) = self.tool_download.as_mut() {
                job.drawn = Instant::now();
            }
            self.tell_tool_download(&name, cx);
            return true;
        }
        let job = self.tool_download.take().expect("the job was checked");
        let answer = job.result.lock().ok().and_then(|mut slot| slot.take());
        let name = job.name;
        let (done_text, done_status) = match job.job {
            MissingToolJob::Download => (
                format!("{name} was downloaded. The project syncs again."),
                format!("{name} was downloaded"),
            ),
            MissingToolJob::CreateWrapper => (
                format!("The {name} wrapper was written. The project syncs again."),
                format!("The {name} wrapper was written"),
            ),
            MissingToolJob::UseDownloaded => (
                format!("The downloaded {name} is used. The project syncs again."),
                format!("The downloaded {name} is used"),
            ),
        };
        let retry_command = match job.job {
            MissingToolJob::Download => "build_tool.install",
            MissingToolJob::CreateWrapper => "build_tool.create_wrapper",
            MissingToolJob::UseDownloaded => "build_tool.use_downloaded",
        };
        match answer {
            Some(Ok(home)) => {
                self.tool_setup = None;
                if let Some(home) = home
                    && let Some(tool) = &self.tool
                {
                    tool.use_home(&home);
                }
                if let Some(bin) = self
                    .tool
                    .as_ref()
                    .filter(|tool| tool.terminal_path())
                    .and_then(|tool| tool.tool_bin())
                {
                    cx.set_global(crate::terminal_panel::ToolTerminalPath(Some(bin)));
                }
                let _ = self.workspace.update(cx, |workspace, cx| {
                    workspace.set_language_status(Some(done_status.into()), cx);
                    workspace.notify_user("Build tool", done_text, cx);
                });
                self.reload_project(cx);
            }
            Some(Err(_)) if job.cancel.is_cancelled() => {
                let _ = self.workspace.update(cx, |workspace, cx| {
                    workspace
                        .set_language_status(Some(format!("{name} download cancelled").into()), cx);
                });
                if let Some(setup) = self.tool_setup.clone() {
                    self.show_tool_setup(&setup, cx);
                }
            }
            Some(Err(text)) => {
                let setup = self.tool_setup.clone();
                let mut buttons = Vec::new();
                if setup.as_ref().is_some_and(|setup| setup.download.is_some()) {
                    buttons.push(("retry", "Try again".into(), retry_command));
                }
                if let Some((label, _)) = setup.as_ref().and_then(|setup| setup.settings.clone()) {
                    let command = match setup
                        .as_ref()
                        .and_then(|setup| setup.settings.as_ref())
                        .map(|(_, row)| *row)
                    {
                        Some("build_tool_second_home") => "settings.build_tool_second",
                        _ => "settings.build_tools",
                    };
                    buttons.push(("settings", label.into(), command));
                }
                let _ = self.workspace.update(cx, |workspace, cx| {
                    workspace
                        .set_language_status(Some(format!("{name} was not downloaded").into()), cx);
                    workspace.notify_error_with_commands(
                        format!("{name} download failed"),
                        text,
                        buttons,
                        cx,
                    );
                });
            }
            None => {}
        }
        true
    }

    /// Follows the distribution of the wrapper the sync downloads: the status line keeps
    /// the bytes it read last, so a slow sync redraws nothing.
    fn poll_wrapper_progress(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(progress) = self.tool.as_ref().and_then(|tool| tool.wrapper_progress()) else {
            self.wrapper_written = 0;
            return false;
        };
        let written = progress.written();
        if written == self.wrapper_written {
            return false;
        }
        self.wrapper_written = written;
        let text = download_text("Build tool wrapper", &progress);
        let workspace = self.workspace.clone();
        cx.defer(move |cx| {
            let _ = workspace.update(cx, |workspace, cx| {
                workspace.set_language_status(Some(text.into()), cx);
            });
        });
        true
    }

    /// The class folders of the project, where a build writes.
    fn output_dirs(&self) -> Vec<PathBuf> {
        let Some(model) = self.session.as_ref().and_then(ProjectSession::model) else {
            return Vec::new();
        };
        let mut dirs = Vec::new();
        for module in &model.modules {
            for unit in &module.units {
                dirs.extend(unit.output_dirs.iter().cloned());
            }
        }
        dirs.sort();
        dirs.dedup();
        dirs
    }

    /// Tells the build tool that the last build step succeeded, so an unchanged project
    /// needs no build before the next run.
    fn mark_built(&self) {
        if let Some(tool) = &self.tool {
            tool.built(&self.root);
        }
    }

    /// Asks the language to put the new classes into the program under the debugger.
    fn request_reload(&mut self, cx: &mut Context<Self>) {
        if self.reload.is_some() {
            return;
        }
        let Some(service) = self.debug_service.clone() else {
            return;
        };
        let Some(debug) = cx.try_global::<crate::debug::ActiveDebug>() else {
            return;
        };
        let panel = debug.0.clone();
        let (active, debugger) = {
            let panel = panel.read(cx);
            (panel.state().0, panel.debugger().cloned())
        };
        let Some(debugger) = debugger.filter(|_| active) else {
            return;
        };
        let (sender, receiver) = channel();
        if service.reload_classes(debugger, self.output_dirs(), sender) {
            self.reload = Some(receiver);
        }
    }

    /// Reads the answer of the hot swap the last successful Build Project started.
    fn poll_reload(&mut self, cx: &mut Context<Self>) -> bool {
        let answer = self.reload.as_ref().map(Receiver::try_recv);
        match answer {
            Some(Ok(outcome)) => {
                self.reload = None;
                self.report_reload(outcome, cx);
                true
            }
            Some(Err(TryRecvError::Empty)) => false,
            Some(Err(TryRecvError::Disconnected)) => {
                self.reload = None;
                true
            }
            None => false,
        }
    }

    fn report_reload(&mut self, outcome: ReloadOutcome, cx: &mut Context<Self>) {
        let (title, text, error) = match outcome.error {
            Some(error) => ("Hot swap failed", format!("Hot swap failed: {error}"), true),
            None if outcome.reloaded.is_empty() => return,
            None if outcome.reloaded.len() == 1 => {
                ("Hot swap", "Reloaded 1 class".to_owned(), false)
            }
            None => (
                "Hot swap",
                format!("Reloaded {} classes", outcome.reloaded.len()),
                false,
            ),
        };
        self.note(&text, error);
        if error {
            self.tell_error(title, text, cx);
        } else {
            self.tell(title, text, cx);
        }
    }

    fn discover(&mut self, cx: &mut Context<Self>) {
        let Some(model) = self.session.as_ref().and_then(ProjectSession::model) else {
            return;
        };
        let providers = self.providers.clone();
        let file = runner::configs_file(&self.root);
        let found = cx.background_executor().spawn(async move {
            let stored = read_store(&file);
            let (configs, problems) = discover(&providers, &model);
            let foreign = foreign_sources(&providers, &model);
            (stored, configs, problems, foreign)
        });
        cx.spawn(async move |this, cx| {
            let (stored, configs, mut problems, foreign) = found.await;
            this.update(cx, |this, cx| {
                this.discovered = configs;
                this.foreign = foreign;
                match stored.error {
                    Some(error) => {
                        this.saved.clear();
                        this.store_broken = Some(error.clone());
                        problems.push(error);
                    }
                    None => {
                        this.saved = stored.configs;
                        this.store_broken = None;
                    }
                }
                this.merge_lists();
                for problem in problems {
                    this.tell_error("Run configurations", problem, cx);
                }
                this.offer_foreign_configs(cx);
                this.changed(cx);
                cx.emit(ConfigsChanged);
            })
            .ok();
        })
        .detach();
    }

    /// The watcher of the project saw a change of `.arro/run.toml`, perhaps made outside
    /// the IDE, such as a git pull.
    pub(crate) fn store_changed(&mut self, cx: &mut Context<Self>) {
        self.reload_store(cx);
    }

    fn reload_store(&mut self, cx: &mut Context<Self>) {
        let file = runner::configs_file(&self.root);
        let read = cx
            .background_executor()
            .spawn(async move { read_store(&file) });
        cx.spawn(async move |this, cx| {
            let stored = read.await;
            this.update(cx, |this, cx| {
                match stored.error {
                    Some(error) => {
                        this.saved.clear();
                        this.store_broken = Some(error.clone());
                        this.tell_error("Run configurations", error, cx);
                    }
                    None => {
                        this.saved = stored.configs;
                        this.store_broken = None;
                    }
                }
                this.merge_lists();
                this.changed(cx);
                cx.emit(ConfigsChanged);
            })
            .ok();
        })
        .detach();
    }

    fn note(&mut self, text: &str, error: bool) {
        let style = if error { error_style() } else { info_style() };
        self.lines.push_back(Line::plain(text, style));
    }

    /// Deferred, like every call into the workspace: the toolbar may be what called.
    fn tell(&self, title: &str, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.tell_with_level(NotificationLevel::Info, title, text, cx);
    }

    /// The same, for something that went wrong: the notification stays until the user
    /// closes it.
    fn tell_error(&self, title: &str, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.tell_with_level(NotificationLevel::Error, title, text, cx);
    }

    /// The same, for a state the user has to know about: it stays until the user closes
    /// it.
    fn tell_warning(&self, title: &str, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.tell_with_level(NotificationLevel::Warning, title, text, cx);
    }

    fn tell_with_level(
        &self,
        level: NotificationLevel,
        title: &str,
        text: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) {
        let (title, text) = (title.to_string(), text.into());
        let workspace = self.workspace.clone();
        cx.defer(move |cx| {
            workspace
                .update(cx, |workspace, cx| match level {
                    NotificationLevel::Info => workspace.notify_user(title, text, cx),
                    NotificationLevel::Warning => workspace.notify_warning(title, text, cx),
                    NotificationLevel::Error => workspace.notify_error(title, text, cx),
                })
                .ok();
        });
    }

    /// The toolbar shows this panel's state, so it repaints too.
    fn changed(&self, cx: &mut Context<Self>) {
        cx.notify();
        let workspace = self.workspace.clone();
        cx.defer(move |cx| {
            workspace.update(cx, |_, cx| cx.notify()).ok();
        });
    }

    /// The buttons above the output: the search bar and the filter.
    fn console_tools(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let filtering = self.console_search.filter_open;
        div()
            .id("run-console-tools")
            .debug_selector(|| "run-console-tools".to_owned())
            .flex()
            .items_center()
            .gap(px(SPACE_XS))
            .flex_none()
            .px(px(SPACE_M))
            .pt(px(SPACE_S))
            .child(
                icon_button(
                    "run-console-find",
                    "general/search",
                    "Find in Console",
                    Some("Ctrl+F".into()),
                    theme,
                )
                .debug_selector(|| "run-console-find".to_owned())
                .on_click(cx.listener(|this, _, window, cx| this.open_console_search(window, cx))),
            )
            .child(
                icon_button(
                    "run-console-filter",
                    "general/filter",
                    "Filter Lines",
                    None,
                    theme,
                )
                .debug_selector(|| "run-console-filter".to_owned())
                .when(filtering, |button| button.bg(theme.selection))
                .on_click(
                    cx.listener(|this, _, window, cx| this.toggle_console_filter(window, cx)),
                ),
            )
            .into_any_element()
    }

    /// The search bar of the console: the query, its options, the counter and the moves
    /// between the matches.
    fn search_row(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let search = &self.console_search;
        let (status, error) = search.status();
        let case = search_toggle(
            "run-search-case",
            "general/caseSensitive",
            "Match Case",
            "Alt+C",
            search.case_sensitive,
            SearchToggle::Case,
            theme,
            cx,
        );
        let word = search_toggle(
            "run-search-word",
            "general/wholeWord",
            "Whole Words",
            "Alt+W",
            search.whole_word,
            SearchToggle::Word,
            theme,
            cx,
        );
        let regex = search_toggle(
            "run-search-regex",
            "general/regex",
            "Regex",
            "Alt+X",
            search.regex,
            SearchToggle::Regex,
            theme,
            cx,
        );
        let field = div()
            .id("run-search-field")
            .track_focus(&self.search_focus)
            .on_key_down(cx.listener(|this, event, window, cx| this.search_key(event, window, cx)))
            .flex()
            .items_center()
            .flex_1()
            .h(px(CONTROL_HEIGHT - SPACE_M))
            .px(px(SPACE_M))
            .rounded(px(CONTROL_RADIUS))
            .border_1()
            .border_color(theme.accent)
            .text_size(px(UI_SIZE))
            .text_color(theme.text)
            .overflow_hidden()
            .children(crate::console_search::field_parts(
                FieldMouse::new(cx, |this| Some(&mut this.console_search.field)),
                "run-search-field",
                &search.field,
                theme,
                true,
            ))
            .when(search.field.text.is_empty(), |this| {
                this.child(div().text_color(theme.text_secondary).child("Find"))
            });
        div()
            .id("run-console-search")
            .debug_selector(|| "run-console-search".to_owned())
            .flex()
            .items_center()
            .flex_none()
            .h(px(CONTROL_HEIGHT))
            .mx(px(SPACE_M))
            .mb(px(SPACE_S))
            .gap(px(SPACE_S))
            .child(field)
            .child(case)
            .child(word)
            .child(regex)
            .child(
                icon_button(
                    "run-search-prev",
                    "general/arrowUp",
                    "Previous Match",
                    Some("Shift+F3".into()),
                    theme,
                )
                .on_click(cx.listener(|this, _, _, cx| this.search_step(false, cx))),
            )
            .child(
                icon_button(
                    "run-search-next",
                    "general/arrowDown",
                    "Next Match",
                    Some("F3".into()),
                    theme,
                )
                .on_click(cx.listener(|this, _, _, cx| this.search_step(true, cx))),
            )
            .child(
                div()
                    .debug_selector(|| "run-search-status".to_owned())
                    .text_color(if error {
                        theme.error
                    } else {
                        theme.text_secondary
                    })
                    .child(status),
            )
            .child(
                icon_button(
                    "run-search-close",
                    "general/closeSmall",
                    "Close Search",
                    Some("Escape".into()),
                    theme,
                )
                .on_click(cx.listener(|this, _, window, cx| this.close_console_search(window, cx))),
            )
            .into_any_element()
    }

    /// The field of the filter: only the lines with its text stay on screen.
    fn filter_row(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id("run-console-filter-row")
            .debug_selector(|| "run-console-filter-row".to_owned())
            .flex()
            .items_center()
            .flex_none()
            .h(px(CONTROL_HEIGHT))
            .mx(px(SPACE_M))
            .mb(px(SPACE_S))
            .px(px(SPACE_M))
            .rounded(px(CONTROL_RADIUS))
            .border_1()
            .border_color(theme.accent)
            .track_focus(&self.filter_focus)
            .on_key_down(cx.listener(|this, event, window, cx| this.filter_key(event, window, cx)))
            .children(crate::console_search::field_parts(
                FieldMouse::new(cx, |this| Some(&mut this.console_search.filter)),
                "run-filter-field",
                &self.console_search.filter,
                theme,
                true,
            ))
            .when(self.console_search.filter.text.is_empty(), |this| {
                this.child(div().text_color(theme.text_secondary).child("Filter lines"))
            })
            .into_any_element()
    }

    /// The console of a window that has run nothing yet: the selected configuration, which
    /// Run starts, with the shortcut of Run, and the link to the configurations.
    fn idle_body(&self, name: String, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let platform = keymap::platform();
        let shortcut = workspace::window_key("workspace::Run", platform)
            .map(|keys| keymap::shortcut_label(keys, platform));
        // The link runs what Shift+F10 runs, so both starts take the same path.
        let run = cx.listener(|this, _, window, cx| {
            let workspace = this.workspace.clone();
            window.defer(cx, move |window, cx| {
                workspace
                    .update(cx, |workspace, cx| workspace.run_selected(window, cx))
                    .ok();
            });
        });
        let edit = cx.listener(|this, _, window, cx| {
            let workspace = this.workspace.clone();
            let panel = cx.entity();
            window.defer(cx, move |window, cx| {
                workspace
                    .update(cx, |workspace, cx| {
                        crate::run_configs::RunConfigDialog::show(panel, workspace, window, cx)
                    })
                    .ok();
            });
        });
        div()
            .debug_selector(|| "run-idle".to_owned())
            .flex()
            .flex_col()
            .flex_1()
            .items_center()
            .justify_center()
            .gap(px(SPACE_S))
            .text_color(theme.text_secondary)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(SPACE_M))
                    .child(
                        div()
                            .id("run-idle-run")
                            .debug_selector(|| "run-idle-run".to_owned())
                            .cursor_pointer()
                            .text_color(theme.accent)
                            .child(format!("Run '{name}'"))
                            .on_click(run),
                    )
                    .children(shortcut.map(|keys| div().child(keys))),
            )
            .child(
                div()
                    .id("run-idle-edit")
                    .debug_selector(|| "run-idle-edit".to_owned())
                    .cursor_pointer()
                    .text_color(theme.accent)
                    .child("Edit Configurations…")
                    .on_click(edit),
            )
            .into_any_element()
    }

    fn rows(
        &mut self,
        range: std::ops::Range<usize>,
        theme: &Theme,
        view: Option<usize>,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let total = self.console_line_count();
        let kept = self.console_search.kept_count(total);
        let drain = view.and_then(|index| self.draining.get(index));
        let current = match drain {
            Some(drain) => drain.output.current(),
            None => self.output.current(),
        };
        let current = current.cloned().unwrap_or_default();
        let typed = (view.is_none() && self.takes_input()).then_some(kept + 1);
        range
            .map(|row| {
                if typed == Some(row) {
                    return self.console_input.element(
                        FieldMouse::new(cx, |this| Some(&mut this.console_input.field))
                            .without_menu(),
                        "run-console-input",
                        theme,
                    );
                }
                if row >= kept {
                    // The line the program has not ended yet.
                    let selector = format!("run-console-line-{total}");
                    return div()
                        .debug_selector(move || selector)
                        .child(line_element(&current, theme))
                        .into_any_element();
                }
                let index = self.console_search.line_of_row(total, row);
                let line = match drain {
                    Some(drain) => drain.lines.get(index),
                    None => self.lines.get(index),
                };
                let Some(line) = line else {
                    return div().into_any_element();
                };
                let ranges = self.console_search.ranges(index);
                let text = line_element_with_matches(line, theme, ranges);
                let linked = console_links::looks_like_link(&line.text);
                let selector = format!("run-console-line-{index}");
                let row = div()
                    .id(("run-console-line", index))
                    .debug_selector(move || selector)
                    .child(text);
                if !linked {
                    return row.into_any_element();
                }
                row.cursor_pointer()
                    .hover(|style| style.underline())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_console_line(index, window, cx)
                    }))
                    .into_any_element()
            })
            .collect()
    }
}

/// The commands of the before launch list of a configuration, in order. A build step
/// asks the build tool whether its outputs are already current; a task and a
/// configuration are planned here, because their processes belong to the same console.
fn before_launch_specs(
    config: &RunConfig,
    tool: Option<&dyn BuildSystem>,
    model: &ProjectModel,
    configs: &[RunConfig],
    providers: &[Arc<dyn RunConfigProvider>],
    root: &StdPath,
) -> Result<Vec<ProcessSpec>, String> {
    let mut specs = Vec::new();
    for step in config.before_launch.clone().unwrap_or_default() {
        match step {
            BeforeLaunchStep::Build => {
                let tool = tool.ok_or("The project has no build tool to build it")?;
                let spec = tool
                    .assemble_if_needed(root)
                    .map_err(|error| error.to_string())?;
                specs.extend(spec);
            }
            BeforeLaunchStep::Task { target } => {
                let tool = tool.ok_or("The project has no build tool to run a task")?;
                let spec = tool
                    .goal(root, &target, TaskOptions::default())
                    .map_err(|error| error.to_string())?;
                specs.push(spec);
            }
            BeforeLaunchStep::Configuration { name } => {
                let other = configs
                    .iter()
                    .find(|known| known.name == name)
                    .ok_or_else(|| {
                        format!("Before launch names a configuration that is not there: {name}")
                    })?;
                if other.is_compound() {
                    return Err(format!(
                        "Before launch cannot start the compound configuration {name}"
                    ));
                }
                let provider = providers
                    .iter()
                    .find(|provider| provider.id() == other.provider)
                    .ok_or_else(|| {
                        format!("Nothing runs configurations of kind {}", other.provider)
                    })?;
                let plan = provider
                    .plan(other, model)
                    .map_err(|error| format!("{name}: {error}"))?;
                specs.extend(plan.before);
                specs.push(plan.program);
            }
        }
    }
    Ok(specs)
}

/// Reads the variables of the environment file of a configuration. Variables the
/// configuration set itself win over the file.
fn environment_variables(
    config: &RunConfig,
    root: &StdPath,
) -> Result<std::collections::BTreeMap<String, String>, String> {
    let Some(file) = &config.environment_file else {
        return Ok(std::collections::BTreeMap::new());
    };
    let path = if file.is_absolute() {
        file.clone()
    } else {
        root.join(file)
    };
    runner::read_environment_file(&path)
}

/// Adds the variables of the environment file of a configuration to the program and to
/// every before launch step.
fn apply_environment_file(
    config: &RunConfig,
    root: &StdPath,
    plan: &mut RunPlan,
    steps: Vec<ProcessSpec>,
) -> Result<Vec<ProcessSpec>, String> {
    let variables = environment_variables(config, root)?;
    if variables.is_empty() {
        return Ok(steps);
    }
    let program = std::mem::replace(&mut plan.program, ProcessSpec::new(""));
    plan.program = runner::with_environment(program, &variables);
    if let Some(before) = plan.before.take() {
        plan.before = Some(runner::with_environment(before, &variables));
    }
    Ok(steps
        .into_iter()
        .map(|spec| runner::with_environment(spec, &variables))
        .collect())
}

fn discover(
    providers: &[Arc<dyn RunConfigProvider>],
    model: &ProjectModel,
) -> (Vec<RunConfig>, Vec<String>) {
    let mut configs = Vec::new();
    let mut problems = Vec::new();
    for provider in providers {
        match provider.discover(model) {
            Ok(found) => configs.extend(found),
            Err(error) => problems.push(error.to_string()),
        }
    }
    (configs, problems)
}

/// The configurations another IDE stored in the project, from every provider that reads
/// such files. Cutting the files is I/O, so the callers run it in the background.
fn foreign_sources(
    providers: &[Arc<dyn RunConfigProvider>],
    model: &ProjectModel,
) -> Vec<runner::ForeignConfigs> {
    providers
        .iter()
        .filter_map(|provider| provider.foreign_configs(model))
        .filter(|found| !found.is_empty())
        .collect()
}

/// What the offer already named, so the same configurations are not offered twice.
fn foreign_signature(sources: &[runner::ForeignConfigs]) -> String {
    let mut keys: Vec<String> = sources
        .iter()
        .flat_map(|source| source.configs.iter())
        .map(runner::foreign_identity)
        .collect();
    keys.sort();
    keys.join("\n")
}

/// The names of a summary, short enough for a notification.
fn names_list(names: &[String]) -> String {
    const SHOWN: usize = 8;
    if names.is_empty() {
        return "none".to_owned();
    }
    let shown = names.iter().take(SHOWN).cloned().collect::<Vec<_>>();
    let rest = names.len().saturating_sub(SHOWN);
    if rest == 0 {
        shown.join(", ")
    } else {
        format!("{}, and {rest} more", shown.join(", "))
    }
}

/// What `.arro/run.toml` held at one moment. Runs on a background thread.
struct StoreRead {
    configs: Vec<RunConfig>,
    /// The file is there but cannot be read: the list stays as the providers found it.
    error: Option<String>,
}

fn read_store(file: &StdPath) -> StoreRead {
    match runner::load_configs(file) {
        Ok(configs) => StoreRead {
            configs,
            error: None,
        },
        Err(error) => StoreRead {
            configs: Vec::new(),
            error: Some(format!("Cannot read {}: {error}", file.display())),
        },
    }
}

fn exit_text(code: Option<i32>, step: Step) -> String {
    let code = code.map_or_else(|| "unknown".to_string(), |code| code.to_string());
    match step {
        Step::Before => format!("Build finished with exit code {code}"),
        Step::Program => format!("Process finished with exit code {code}"),
    }
}

/// The line of a build step that failed, the same as the Debug window writes it.
fn build_failed_text(code: Option<i32>) -> String {
    let code = code.map_or_else(|| "unknown".to_string(), |code| code.to_string());
    format!("Build failed: exit code {code}")
}

/// Marks lines the IDE writes itself, not the program.
const INFO: u8 = 8;

fn info_style() -> Style {
    Style {
        color: Some(INFO),
        ..Style::default()
    }
}

/// A line the user typed into the console, not the program.
fn input_style() -> Style {
    Style {
        input: true,
        ..Style::default()
    }
}

pub(crate) fn error_style() -> Style {
    Style {
        error: true,
        ..Style::default()
    }
}

pub(crate) fn info(text: impl Into<String>) -> Line {
    Line::plain(text, info_style())
}

pub(crate) fn line_element(line: &Line, theme: &Theme) -> AnyElement {
    line_element_with_matches(line, theme, &[])
}

/// A line of a console with the places the search found painted over its own colours.
pub(crate) fn line_element_with_matches(
    line: &Line,
    theme: &Theme,
    matches: &[std::ops::Range<usize>],
) -> AnyElement {
    let text = crate::ansi::highlighted(line, theme, matches);
    div()
        .h(px(CODE_LINE_HEIGHT))
        .px(px(LINE_PAD))
        .whitespace_nowrap()
        .child(text)
        .into_any_element()
}

/// A line of a console that wraps at the width of its column: what the reader turns on
/// for long lines. The height follows the number of the wrapped parts.
pub(crate) fn wrapped_line_element(line: &Line, theme: &Theme) -> AnyElement {
    div()
        .min_h(px(CODE_LINE_HEIGHT))
        .px(px(LINE_PAD))
        .whitespace_normal()
        .child(crate::ansi::highlighted(line, theme, &[]))
        .into_any_element()
}

/// The finished output lines of the console on screen, in order.
fn console_texts<'a>(
    drain: Option<&'a DrainingConsole>,
    lines: &'a VecDeque<Line>,
) -> Vec<&'a str> {
    match drain {
        Some(drain) => drain.lines.iter().map(|line| line.text.as_str()).collect(),
        None => lines.iter().map(|line| line.text.as_str()).collect(),
    }
}

/// Whether a line of the console is the headline of an exception: a class name that
/// ends with `Exception` or `Error` and what it says.
fn is_exception_head(line: &str) -> bool {
    let text = line.trim();
    if text.starts_with("at ") || text.starts_with("Caused by:") || text.starts_with("Suppressed:")
    {
        return false;
    }
    let class = text
        .split(|character: char| character.is_whitespace() || character == ':')
        .next()
        .unwrap_or_default();
    class.ends_with("Exception") || class.ends_with("Error")
}

/// Whether a line of the console belongs to the stack of an exception.
fn is_stack_line(line: &str) -> bool {
    let text = line.trim();
    text.starts_with("at ")
        || text.starts_with("Caused by:")
        || text.starts_with("Suppressed:")
        || text.starts_with("... ")
}

/// A button of the search bar that turns one option on or off.
#[allow(clippy::too_many_arguments)]
fn search_toggle(
    id: &'static str,
    icon_name: &'static str,
    label: &'static str,
    hint: &'static str,
    on: bool,
    toggle: SearchToggle,
    theme: &Theme,
    cx: &mut Context<RunPanel>,
) -> Stateful<Div> {
    let button = icon_button(id, icon_name, label, Some(hint.into()), theme);
    let button = if on {
        button
            .bg(theme.tab_selected)
            .border_1()
            .border_color(theme.tab_selected_border)
    } else {
        button
    };
    button.on_click(
        cx.listener(move |this, _, window, cx| this.toggle_search_option(toggle, window, cx)),
    )
}

/// One console tab: the current run or an earlier one that still writes.
fn tab_button(
    id: impl Into<ElementId>,
    label: SharedString,
    selected: bool,
    theme: &Theme,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .flex()
        .items_center()
        .flex_none()
        .h(px(22.))
        .px(px(8.))
        .rounded(px(CONTROL_RADIUS))
        .truncate()
        .text_color(if selected {
            theme.text
        } else {
            theme.text_secondary
        })
        .when(selected, |this| this.bg(theme.selection))
        .when(!selected, |this| {
            this.hover(move |style| style.bg(theme.selection_hover))
        })
        .on_click(on_click)
        .child(label)
}

/// A tab of the main views of the window, the console and the live sections.
fn view_tab(
    id: &'static str,
    selected: bool,
    theme: &Theme,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    content: impl IntoElement,
) -> Stateful<Div> {
    div()
        .id(id)
        .debug_selector(move || id.to_owned())
        .flex()
        .items_center()
        .flex_none()
        .h(px(22.))
        .px(px(8.))
        .rounded(px(CONTROL_RADIUS))
        .text_size(px(UI_SIZE))
        .text_color(if selected {
            theme.text
        } else {
            theme.text_secondary
        })
        .when(selected, |this| this.bg(theme.selection))
        .when(!selected, |this| {
            this.hover(move |style| style.bg(theme.selection_hover))
        })
        .on_click(on_click)
        .child(content)
}

impl Render for RunPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(test)]
        workspace::note_view_render(cx, "run");
        let theme = *cx.global::<Theme>();
        let view = self.viewing.filter(|index| *index < self.draining.len());
        let kept = self.console_search.kept_count(self.console_line_count());
        let count = kept + 1 + usize::from(view.is_none() && self.takes_input());
        let workspace = self.workspace.clone();
        let options = crate::tool_menu::options_menu(workspace.clone(), RUN_TOOL, |_| Vec::new());
        let hide = move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
            workspace
                .update(cx, |workspace, cx| {
                    workspace.toggle_tool(RUN_TOOL, window, cx)
                })
                .ok();
        };
        let running = self.busy() || self.starting || self.waiting || self.waiting_tests.is_some();
        let profiling = self.profile_running();
        let can_run = self.selected.is_some() || self.tests.active();
        let rerun_icon = icon("run/restart", &theme);
        let rerun_icon = if can_run {
            rerun_icon
        } else {
            rerun_icon.grayscale(true).opacity(0.4)
        };
        let stop_icon = icon("run/stop", &theme);
        let stop_icon = if running {
            stop_icon
        } else {
            stop_icon.grayscale(true).opacity(0.4)
        };
        let dump_icon = icon("general/logs", &theme);
        let dump_icon = if self.program_pid.is_some() {
            dump_icon
        } else {
            dump_icon.grayscale(true).opacity(0.4)
        };
        let side = div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(SPACE_XS))
            .flex_none()
            .w(px(STRIPE_WIDTH))
            .child(
                button_frame("run-rerun", "Rerun", Some("Ctrl+F5".into()), &theme)
                    .debug_selector(|| "run-rerun".to_owned())
                    .on_click(cx.listener(|this, _, _, cx| this.rerun_last(cx)))
                    .child(rerun_icon),
            )
            .child(
                button_frame("run-stop", "Stop", Some("Ctrl+F2".into()), &theme)
                    .debug_selector(|| "run-stop".to_owned())
                    .on_click(cx.listener(|this, _, _, cx| this.stop(cx)))
                    .child(stop_icon),
            )
            .child(
                button_frame("run-dump-threads", "Dump Threads", None, &theme)
                    .debug_selector(|| "run-dump-threads".to_owned())
                    .on_click(cx.listener(|this, _, window, cx| this.dump_threads(window, cx)))
                    .child(dump_icon),
            )
            .child(
                button_frame("run-heap-dump", "Capture Memory Snapshot", None, &theme)
                    .debug_selector(|| "run-heap-dump".to_owned())
                    .on_click(cx.listener(|this, _, window, cx| {
                        let workspace = this.workspace.clone();
                        workspace
                            .update(cx, |workspace, cx| {
                                crate::heap_dump::capture(workspace, window, cx)
                            })
                            .ok();
                    }))
                    .child(icon("general/logs", &theme)),
            )
            .children(self.tests.has_failures().then(|| {
                button_frame("run-rerun-failed", "Rerun Failed Tests", None, &theme)
                    .debug_selector(|| "run-rerun-failed".to_owned())
                    .on_click(cx.listener(|this, _, _, cx| this.rerun_failed(cx)))
                    .child(icon("run/restart", &theme))
            }))
            .children(profiling.then(|| {
                button_frame("run-stop-snapshot", "Stop and Show Snapshot", None, &theme)
                    .debug_selector(|| "run-stop-snapshot".to_owned())
                    .on_click(
                        cx.listener(|this, _, window, cx| this.stop_and_show_profile(window, cx)),
                    )
                    .child(icon("run/profiler", &theme))
            }));
        let tabs = (!self.draining.is_empty()).then(|| {
            let current = tab_button(
                "run-tab-current",
                self.title.clone(),
                view.is_none(),
                &theme,
                cx.listener(|this, _, _, cx| {
                    this.viewing = None;
                    this.rebuild_console_filter();
                    cx.notify();
                }),
            );
            let old = self.draining.iter().enumerate().map(|(index, drain)| {
                let id = format!("run-tab-{index}");
                tab_button(
                    id,
                    drain.title.clone(),
                    view == Some(index),
                    &theme,
                    cx.listener(move |this, _, _, cx| {
                        this.viewing = Some(index);
                        this.rebuild_console_filter();
                        cx.notify();
                    }),
                )
            });
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(px(SPACE_S))
                .px(px(SPACE_M))
                .pt(px(SPACE_S))
                .overflow_hidden()
                .child(current)
                .children(old)
        });
        let tools = self.console_tools(&theme, cx);
        let search = self
            .console_search
            .open
            .then(|| self.search_row(&theme, cx));
        let filter = self
            .console_search
            .filter_open
            .then(|| self.filter_row(&theme, cx));
        let nothing_to_run = self.configs.is_empty()
            && self.lines.is_empty()
            && self.draining.is_empty()
            && !self.tests.active()
            && self.live.is_none()
            && !self.busy();
        // Nothing ran in this window yet and a configuration is selected: the window names it,
        // so Run starts it from here. A start in progress does not count as idle.
        let idle = (!running
            && self.lines.is_empty()
            && self.draining.is_empty()
            && !self.tests.active()
            && self.live.is_none())
        .then(|| {
            self.selected
                .and_then(|index| self.configs.get(index))
                .map(|config| config.name.clone())
        })
        .flatten();
        let console_body: AnyElement = if nothing_to_run {
            let add_config = cx.listener(|this, _, window, cx| {
                let workspace = this.workspace.clone();
                let panel = cx.entity();
                window.defer(cx, move |window, cx| {
                    workspace
                        .update(cx, |workspace, cx| {
                            crate::run_configs::RunConfigDialog::show_add(
                                panel, workspace, window, cx,
                            )
                        })
                        .ok();
                });
            });
            div()
                .debug_selector(|| "run-empty".to_owned())
                .flex()
                .flex_col()
                .flex_1()
                .items_center()
                .justify_center()
                .gap(px(SPACE_S))
                .text_color(theme.text_secondary)
                .child("Nothing to run")
                .child(
                    div()
                        .id("run-empty-configs")
                        .debug_selector(|| "run-empty-configs".to_owned())
                        .cursor_pointer()
                        .text_color(theme.accent)
                        .child("Add configuration…")
                        .on_click(add_config),
                )
                .into_any_element()
        } else if let Some(name) = idle {
            self.idle_body(name, &theme, cx)
        } else {
            scrollable(
                "run-console",
                self.follow.handle(),
                div().size_full().child(
                    uniform_list(
                        "run-console",
                        count,
                        cx.processor(move |this, range, _, cx| this.rows(range, &theme, view, cx)),
                    )
                    .track_scroll(self.follow.list_handle())
                    .size_full(),
                ),
                &theme,
                window,
                cx,
            )
            .flex_1()
            .min_h_0()
            .into_any_element()
        };
        let console = div()
            .flex()
            .flex_col()
            .size_full()
            .font_family(MONO_FONT)
            .text_size(px(CODE_SIZE))
            .text_color(theme.code)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| window.focus(&this.focus, cx)),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    this.console_menu(event.position, cx)
                }),
            )
            .children(tabs)
            .child(tools)
            .children(search)
            .children(filter)
            .child(console_body)
            .into_any_element();
        let body = if self.tests.active() {
            self.tests_body(console, &theme, window, cx)
        } else if let Some(live) = self.live.clone() {
            let started = live.read(cx).started();
            let stopped = live.read(cx).stopped();
            let title = live.read(cx).title();
            let labels = div()
                .flex()
                .items_center()
                .gap(px(SPACE_S))
                .px(px(SPACE_M))
                .pt(px(SPACE_S))
                .overflow_hidden()
                .child(view_tab(
                    "run-console-tab",
                    !self.live_open,
                    &theme,
                    cx.listener(|this, _, _, cx| this.show_console(cx)),
                    div().child("Console"),
                ))
                .child(view_tab(
                    "run-live-tab",
                    self.live_open,
                    &theme,
                    cx.listener(|this, _, _, cx| this.show_live(cx)),
                    crate::live_panel::live_tab_label(&title, started, stopped, &theme),
                ));
            let content = if self.live_open {
                live.into_any_element()
            } else {
                console
            };
            div()
                .flex()
                .flex_col()
                .size_full()
                .child(labels)
                .child(div().flex_1().min_h_0().child(content))
                .into_any_element()
        } else {
            console
        };
        let ready = self
            .live
            .as_ref()
            .is_some_and(|live| live.read(cx).started());
        let header = tool_header(
            div()
                .flex()
                .items_center()
                .gap(px(SPACE_XS))
                .font_weight(FontWeight::MEDIUM)
                .children(ready.then(|| {
                    div()
                        .debug_selector(|| "run-ready-dot".to_owned())
                        .flex_none()
                        .size(px(6.))
                        .rounded_full()
                        .bg(theme.semantic_type)
                }))
                .child(self.title.clone()),
            div(),
            GROUP,
            "run",
            options,
            hide,
            &theme,
        )
        .debug_selector(|| "run-header".to_owned());
        let panel = island(&theme)
            .id("run-panel")
            .key_context("RunPanel ToolWindow")
            .track_focus(&self.focus)
            .group(GROUP)
            .h_full()
            .on_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)))
            .child(header)
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(side)
                    .child(div().flex_1().min_w_0().h_full().child(body)),
            );
        let Some(name) = self.confirm_rerun.clone() else {
            return panel.into_any_element();
        };
        // The question of a second Run stays inside the tool window, so the toolbar and
        // the keys need no window of their own to ask it.
        let question = format!("{name} is already running. Stop and rerun?");
        let button = |id: &'static str, label: &'static str, primary: bool| {
            div()
                .id(id)
                .debug_selector(move || id.to_owned())
                .flex()
                .items_center()
                .h(px(CONTROL_HEIGHT))
                .px(px(SPACE_L))
                .rounded(px(CONTROL_RADIUS))
                .when(primary, |this| {
                    this.bg(theme.accent).text_color(theme.over_accent)
                })
                .when(!primary, |this| {
                    this.border_1()
                        .border_color(theme.control_border)
                        .hover(move |style| style.bg(theme.hover))
                })
                .child(label)
        };
        let dialog = div()
            .id("run-confirm")
            .debug_selector(|| "run-confirm".to_owned())
            .absolute()
            .left(px(SPACE_L))
            .top(px(CONTROL_HEIGHT + SPACE_L))
            .w(px(380.))
            .p(px(DIALOG_PADDING))
            .rounded(px(CONTROL_RADIUS))
            .border_1()
            .border_color(theme.border)
            .bg(theme.island_bg)
            .shadow_lg()
            .flex()
            .flex_col()
            .gap(px(DIALOG_GAP))
            .text_size(px(UI_SIZE))
            .text_color(theme.text)
            .child(question)
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(SPACE_M))
                    .child(
                        button("run-confirm-cancel", "Cancel", false)
                            .on_click(cx.listener(|this, _, _, cx| this.cancel_rerun(cx))),
                    )
                    .child(
                        button("run-confirm-rerun", "Stop and Rerun", true)
                            .on_click(cx.listener(|this, _, _, cx| this.rerun_confirmed(cx))),
                    ),
            );
        div()
            .relative()
            .size_full()
            .child(panel)
            .child(dialog)
            .into_any_element()
    }
}
