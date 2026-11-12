//! The Run tool window: configurations found in the project, the console of the last run,
//! Run and Stop. What a configuration means belongs to its provider.

use std::collections::VecDeque;
use std::path::Path as StdPath;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::time::Duration;

use ::base::Cancel;
use dap_client::ReloadOutcome;
use project_model::{
    BuildModule, BuildSystem, EnvironmentProvider, ProjectModel, ProjectSession, SyncEvent,
    TaskOptions, open_project,
};
use runner::{
    Console, ConsoleEvent, FindTests, RunConfig, RunConfigProvider, RunPlan, STOPPED_BY_USER, Step,
    Stream, TestProvider, TestRequest, TestRun,
};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::{DEBUG_TOOL, RUN_TOOL, RunControls, RunState, Workspace};
use workspace_core::Delivery;

use crate::ansi::{AnsiParser, Line, Style};
use crate::console_input::{ConsoleInput, ConsoleKey};

mod tests;

pub(crate) use tests::TestsChanged;
use tests::{TestView, title_of};

/// The list of run configurations changed, for the gutter marks of the open editors.
pub(crate) struct ConfigsChanged;

impl EventEmitter<ConfigsChanged> for RunPanel {}

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
const GROUP: &str = "bottom-panel";
const LINE_PAD: f32 = SPACE_L;

/// The 16 terminal colors, dark theme first.
const ANSI_DARK: [u32; 16] = [
    0x5a5d63, 0xf75464, 0x5fb865, 0xe0bb65, 0x548af7, 0xc77dbb, 0x2aacb8, 0xbcbec4, 0x868a91,
    0xff7a85, 0x89d18e, 0xf2c55c, 0x7aa7ff, 0xe0a0d6, 0x58c9d4, 0xffffff,
];
const ANSI_LIGHT: [u32; 16] = [
    0x000000, 0xc7222d, 0x067d17, 0x9e880d, 0x0033b3, 0x871094, 0x0f7b8a, 0x6f737a, 0x818594,
    0xe55765, 0x2e9c3b, 0xb59b1b, 0x3574f0, 0xb350bf, 0x1996a6, 0x1e1f22,
];
const ERROR_DARK: u32 = 0xf75464;
const ERROR_LIGHT: u32 = 0xc7222d;

fn color(hex: u32) -> Hsla {
    rgb(hex).into()
}

pub struct RunPanel {
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    scroll: UniformListScrollHandle,
    root: PathBuf,
    cache_root: PathBuf,
    tool: Option<Arc<dyn BuildSystem>>,
    providers: Vec<Arc<dyn RunConfigProvider>>,
    /// The SDKs of the project; it also carries a changed model to the language.
    environment: Option<Arc<dyn EnvironmentProvider>>,
    session: Option<ProjectSession>,
    sync: Option<workspace_core::Subscription<SyncEvent>>,
    loading: bool,
    configs: Vec<RunConfig>,
    /// Configurations from `.arro/run.toml`, in the file order.
    saved: Vec<RunConfig>,
    /// Configurations the providers found in the project.
    discovered: Vec<RunConfig>,
    /// Temporary configurations: started from a place or the keyboard and not written to
    /// the file. The oldest goes first.
    temporary: Vec<RunConfig>,
    /// Why the file could not be read. It is never overwritten while this is set.
    store_broken: Option<String>,
    selected: Option<usize>,
    console: Option<Console>,
    /// The main process of the console is still running. Output may keep coming after it
    /// ends, but the panel is free for the next run.
    console_running: bool,
    /// Consoles whose process ended while something still holds their pipes. Their late
    /// output keeps filling the console of their own run.
    draining: Vec<DrainingConsole>,
    /// The old console the user is looking at; `None` is the run of this window.
    viewing: Option<usize>,
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
    title: SharedString,
    lines: VecDeque<Line>,
    parser: AnsiParser,
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
    /// The lines of a build run with a console of its own: the test console keeps its
    /// own parser, so an unfinished line of one process cannot join the other.
    build_parser: AnsiParser,
    /// The answer of a hot swap the last Build Project asked for.
    reload: Option<Receiver<ReloadOutcome>>,
    /// Bumped on every build file that changed: only the import after the last of a
    /// batch runs.
    model_reload: u64,
    /// The line the user types into the console, when its program reads input.
    console_input: ConsoleInput,
    /// The program step of the console started; a build step before it reads nothing.
    program_started: bool,
}

/// A console whose main process ended, kept for the output that follows.
struct DrainingConsole {
    title: SharedString,
    /// `None` once its pipes have closed: the tab and its text stay.
    console: Option<Console>,
    lines: Vec<Line>,
    parser: AnsiParser,
}

impl RunPanel {
    pub fn create(
        root: PathBuf,
        setup: ProjectSetup,
        workspace: WeakEntity<Workspace>,
        cx: &mut App,
    ) -> Entity<Self> {
        cx.new(|cx| {
            let project_root = root.clone();
            let (session, sync) = match setup.tool.clone() {
                Some(tool) => {
                    let (session, sync) = open_project(tool, root, &setup.cache_root);
                    (Some(session), Some(sync))
                }
                None => (None, None),
            };
            // The build tool window makes run configurations of its tasks on demand. The
            // provider writes none of them into the list by itself.
            let mut providers = setup.providers;
            if let Some(tool) = &setup.tool {
                providers.push(Arc::new(runner::BuildTaskProvider::new(tool.clone())));
            }
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
            Self {
                workspace,
                focus: cx.focus_handle(),
                scroll: UniformListScrollHandle::new(),
                root: project_root,
                cache_root: setup.cache_root.clone(),
                tool: setup.tool,
                providers,
                environment: setup.environment.clone(),
                loading: session.is_some(),
                session,
                sync,
                configs: Vec::new(),
                saved: Vec::new(),
                discovered: Vec::new(),
                temporary: Vec::new(),
                store_broken: None,
                selected: None,
                console: None,
                console_running: false,
                draining: Vec::new(),
                viewing: None,
                starting: false,
                starting_tests: None,
                rerun: false,
                waiting: false,
                problems: Vec::new(),
                title: "Run".into(),
                lines: VecDeque::new(),
                parser: AnsiParser::default(),
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
                build_parser: AnsiParser::default(),
                reload: None,
                model_reload: 0,
                console_input: ConsoleInput::new(),
                program_started: false,
            }
        })
    }

    pub fn focus(&self) -> FocusHandle {
        self.focus.clone()
    }

    /// The root of the build model cache of the project, for Invalidate Caches.
    pub fn cache_root(&self) -> &StdPath {
        &self.cache_root
    }

    /// The toolbar widget on this panel.
    pub fn controls(panel: &Entity<Self>) -> RunControls {
        let (state, run, debug, stop, select, build, rerun, compile, rerun_tests, background, save) = (
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
            rerun: Rc::new(move |_, cx| rerun.update(cx, |this, cx| this.rerun_last(cx))),
            rerun_tests: Rc::new(move |_, cx| {
                rerun_tests.update(cx, |this, cx| this.rerun_tests(cx))
            }),
            stop_background: Rc::new(move |window, cx| {
                let position = window.mouse_position();
                background.update(cx, |this, cx| this.stop_background(position, cx))
            }),
            save: Rc::new(move |_, cx| save.update(cx, |this, cx| this.save_selected(cx))),
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
        }
    }

    fn is_temporary(&self, name: &str) -> bool {
        self.temporary.iter().any(|config| config.name == name)
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

    /// Reads the model again and publishes it when the tool answers, the way a change of
    /// a build file does. An old model stays readable meanwhile.
    pub fn reload_project(&mut self, cx: &mut Context<Self>) {
        let Some(tool) = self.tool.clone() else {
            self.tell("Reload", "The project has no build tool", cx);
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
        if self.tool.is_none() {
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

    /// What went wrong so far, for scripts.
    pub fn problems(&self) -> &[String] {
        &self.problems
    }

    /// The text of the runs that already ended but still receive late output, for tests.
    #[cfg(test)]
    pub(crate) fn draining_text(&self) -> Vec<String> {
        self.draining
            .iter()
            .flat_map(|drain| {
                let lines = drain.lines.iter().map(|line| line.text.clone());
                lines.chain(Some(drain.parser.current().text.clone()))
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
        let current = Some(self.parser.current().text.clone()).filter(|text| !text.is_empty());
        lines.chain(current).collect()
    }

    pub fn select(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.configs.len() {
            self.selected = Some(index);
            self.changed(cx);
        }
    }

    pub fn select_named(&mut self, name: &str, cx: &mut Context<Self>) -> bool {
        let found = self.configs.iter().position(|config| config.name == name);
        if let Some(index) = found {
            self.select(index, cx);
        }
        found.is_some()
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
        self.saved = items
            .iter()
            .filter(|(_, stored, temporary)| *stored && !*temporary)
            .map(|(config, _, _)| config.clone())
            .collect();
        self.discovered = items
            .iter()
            .filter(|(_, stored, temporary)| !*stored && !*temporary)
            .map(|(config, _, _)| config.clone())
            .collect();
        self.temporary = items
            .iter()
            .filter(|(_, _, temporary)| *temporary)
            .map(|(config, _, _)| config.clone())
            .collect();
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
                    this.tell("Run configurations", text, cx);
                }
                this.changed(cx);
            })
            .ok();
        })
        .detach();
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

    /// The list of the window: the stored configurations, then the discovered ones whose
    /// names are not stored, then the temporary ones that are not there already.
    fn merge_lists(&mut self) {
        let name = self
            .selected
            .and_then(|index| self.configs.get(index))
            .map(|config| config.name.clone());
        self.configs = runner::merge_configs(&self.saved, &self.discovered);
        for config in &self.temporary {
            if !self.configs.iter().any(|known| known.name == config.name) {
                self.configs.push(config.clone());
            }
        }
        let kept = name.and_then(|name| self.configs.iter().position(|config| config.name == name));
        self.selected = kept.or_else(|| (!self.configs.is_empty()).then_some(0));
    }

    /// Something runs: a program, a task or tests.
    fn busy(&self) -> bool {
        self.console_running
            || self.tests.running()
            || (self.build_console.is_some() && self.build_running)
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
        let parser = std::mem::take(&mut self.parser);
        let lines = self.lines.drain(..).collect();
        // Old runs whose output has ended are kept as tabs, the latest few of them.
        while self.draining.len() >= MAX_OLD_RUNS {
            let Some(oldest) = self
                .draining
                .iter()
                .position(|drain| drain.console.is_none())
            else {
                break;
            };
            self.draining.remove(oldest);
        }
        self.draining.push(DrainingConsole {
            title,
            console: Some(console),
            lines,
            parser,
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
        if self.busy() {
            self.rerun = true;
            self.stop_running();
            return;
        }
        self.start(cx);
    }

    /// Plans the selected configuration in the background and starts its console.
    fn start(&mut self, cx: &mut Context<Self>) {
        let Some(config) = self
            .selected
            .and_then(|index| self.configs.get(index))
            .cloned()
        else {
            return;
        };
        let model = self.session.as_ref().and_then(ProjectSession::model);
        let Some(model) = model.filter(|model| model.resolved || !self.loading) else {
            self.waiting = true;
            self.title = config.name.clone().into();
            self.lines.clear();
            self.note("Waiting for the project to load", false);
            self.changed(cx);
            return;
        };
        let provider = self.providers.iter().find(|p| p.id() == config.provider);
        let Some(provider) = provider.cloned() else {
            let text = format!("Nothing runs configurations of kind {}", config.provider);
            self.tell("Cannot run", text, cx);
            return;
        };
        let title = config.name.clone();
        self.launch(title, cx, move || plan(&*provider, &config, &model));
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
            self.tell("Busy", "Stop what is running first", cx);
            return;
        }
        let Some(tool) = self.tool.clone() else {
            return;
        };
        let module = module.and_then(|id| self.module_named(id));
        let (root, name) = (self.root.clone(), task.to_string());
        let options = TaskOptions { skip_tests };
        let title = format!("{} {task}", self.tool_title());
        self.launch(title, cx, move || {
            let program = tool
                .task_in(&root, module.as_ref(), &name, options)
                .map_err(|error| error.to_string())?;
            Ok(RunPlan {
                before: None,
                program,
                grace: TASK_GRACE,
            })
        });
    }

    /// Runs a line typed in the build tool window, in the root project.
    pub fn run_goal(&mut self, line: &str, cx: &mut Context<Self>) {
        if self.starting || self.busy() {
            self.tell("Busy", "Stop what is running first", cx);
            return;
        }
        let Some(tool) = self.tool.clone() else {
            return;
        };
        let (root, line) = (self.root.clone(), line.to_string());
        let title = format!("{} {line}", self.tool_title());
        self.launch(title, cx, move || {
            let program = tool
                .goal(&root, &line, TaskOptions::default())
                .map_err(|error| error.to_string())?;
            Ok(RunPlan {
                before: None,
                program,
                grace: TASK_GRACE,
            })
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
            self.tell("Cannot debug", "The language server is not running", cx);
            return;
        };
        let Some(tool) = self.tool.clone() else {
            self.tell("Cannot debug", "The project has no build tool", cx);
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
                    this.update(cx, |this, cx| this.tell("Cannot debug", error, cx))
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
        if self.starting || (self.build_console.is_some() && self.build_running) {
            self.tell("Busy", "Stop what is running first", cx);
            return;
        }
        let debugging_tests = self.debugging_tests && self.tests.running();
        if self.busy() && !debugging_tests {
            self.tell("Busy", "Stop what is running first", cx);
            return;
        }
        let Some(tool) = self.tool.clone() else {
            self.tell("Cannot build", "The project has no build tool", cx);
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
        if debugging_tests {
            self.launch_build(title, cx, make);
        } else {
            self.building_project = true;
            self.launch(title, cx, make);
        }
    }

    /// Ctrl+Shift+F9: compiles the module of `file` with the build tool. Without a file,
    /// or when no module holds it, the whole project compiles.
    pub fn compile_current(&mut self, file: Option<PathBuf>, cx: &mut Context<Self>) {
        if self.starting || self.busy() {
            self.tell("Busy", "Stop what is running first", cx);
            return;
        }
        let Some(tool) = self.tool.clone() else {
            self.tell("Cannot compile", "The project has no build tool", cx);
            return;
        };
        let module = file.as_deref().and_then(|file| {
            let model = self.session.as_ref().and_then(ProjectSession::model)?;
            model
                .modules
                .iter()
                .find(|module| file.starts_with(&module.dir))
                .cloned()
        });
        let (root, title) = (self.root.clone(), format!("{} Compile", self.tool_title()));
        let make = move || {
            let program = tool
                .compile(&root, module.as_ref())
                .map_err(|error| error.to_string())?;
            Ok(RunPlan {
                before: None,
                program,
                grace: TASK_GRACE,
            })
        };
        self.building_project = true;
        self.launch(title, cx, make);
    }

    /// Alt+Shift+R: the tests of the last run again.
    pub fn rerun_tests(&mut self, cx: &mut Context<Self>) {
        match self.tests.request.clone() {
            Some(request) => self.run_tests(request, cx),
            None => self.tell("Rerun Tests", "No tests ran yet", cx),
        }
    }

    /// Whether the console in front reads what the user types. The program of a run does;
    /// a build step, a test run and an old console tab do not.
    pub(crate) fn takes_input(&self) -> bool {
        self.viewing.is_none()
            && self.program_started
            && self.console_input.is_open()
            && self.console.as_ref().is_some_and(Console::takes_input)
    }

    /// The line typed into the console, for tests.
    #[cfg(test)]
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

    fn send_console_line(&mut self, line: &str, cx: &mut Context<Self>) {
        if let Some(console) = &self.console {
            console.send_line(line);
        }
        // A prompt the program left unfinished ends before the line the user typed.
        let mut done = Vec::new();
        self.parser.finish(&mut done);
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
        if self.console_key(event, cx) {
            return;
        }
        self.test_key(event, window, cx);
    }

    /// The context menu of the console: Send EOF, as IDEA has it.
    fn console_menu(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        if !self.takes_input() {
            return;
        }
        let panel = cx.weak_entity();
        let entry = MenuEntry::new("Send EOF", move |_, cx| {
            panel.update(cx, |panel, cx| panel.send_eof(cx)).ok();
        });
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.show_menu(position, vec![entry], cx)
            })
            .ok();
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
                self.build_console = None;
                self.building_project = false;
                self.tell("Build", "The build was stopped", cx);
            }
            _ => {}
        }
        self.changed(cx);
    }

    /// A build beside a running test session: output goes to the same console, but the
    /// test run, its tree and its title stay as they are.
    fn launch_build(
        &mut self,
        title: String,
        cx: &mut Context<Self>,
        make: impl FnOnce() -> Result<RunPlan, String> + Send + 'static,
    ) {
        self.starting = true;
        self.building_project = true;
        self.lines.push_back(info(&title));
        let plan = cx.background_executor().spawn(async move { make() });
        cx.spawn(async move |this, cx| {
            let plan = plan.await;
            this.update(cx, |this, cx| {
                this.starting = false;
                match plan {
                    Ok(plan) => {
                        this.build_parser = AnsiParser::default();
                        this.build_console = Some(Console::start(plan));
                        this.build_running = true;
                    }
                    Err(message) => {
                        this.building_project = false;
                        this.note(&message, true);
                        this.tell("Cannot run", message, cx);
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
        cx: &mut Context<Self>,
        make: impl FnOnce() -> Result<RunPlan, String> + Send + 'static,
    ) {
        self.starting = true;
        self.archive_console();
        crate::history::put_label(&self.root, format!("Before running {title}"), cx);
        self.title = title.into();
        self.lines.clear();
        self.parser = AnsiParser::default();
        self.console_input.restart();
        self.program_started = false;
        self.tests.clear();
        cx.emit(TestsChanged);
        let plan = cx.background_executor().spawn(async move { make() });
        cx.spawn(async move |this, cx| {
            let plan = plan.await;
            this.update(cx, |this, cx| {
                this.starting = false;
                match plan {
                    Ok(plan) => {
                        this.console = Some(Console::start(plan));
                        this.console_running = true;
                    }
                    Err(message) => {
                        this.building_project = false;
                        this.note(&message, true);
                        this.tell("Cannot run", message, cx);
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
        self.waiting = false;
        self.waiting_tests = None;
        self.queued_tests = None;
        self.debugging_tests = false;
        self.building_project = false;
        self.build_console = None;
        self.build_running = false;
        self.console_running = false;
        self.reload = None;
        if let Some(starting) = &self.starting_tests {
            starting.cancel();
        }
        self.stop_running();
        self.changed(cx);
    }

    /// Shift+F9: starts the selected configuration under the debugger.
    pub fn debug(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(service) = self.debug_service.clone() else {
            self.tell("Cannot debug", "The language server is not running", cx);
            return;
        };
        let Some(model) = self.session.as_ref().and_then(ProjectSession::model) else {
            self.tell("Cannot debug", "The project is still loading", cx);
            return;
        };
        let Some(selected) = self.selected else {
            self.tell("Cannot debug", "There is no configuration to debug", cx);
            return;
        };
        let Some(config) = self.configs.get(selected).cloned() else {
            return;
        };
        let Some(provider) = self
            .providers
            .iter()
            .find(|provider| provider.id() == config.provider)
        else {
            self.tell("Cannot debug", "The configuration has no provider", cx);
            return;
        };
        // The plan may look at every source file to see whether a build is due, so it is
        // made in the background.
        let provider = provider.clone();
        let name = config.name.clone();
        let plan = cx
            .background_executor()
            .spawn(async move { provider.debug_plan(&config, &model) });
        let panel = cx.global::<crate::debug::ActiveDebug>().0.clone();
        let workspace = self.workspace.clone();
        let handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            let plan = match plan.await {
                Ok(plan) => plan,
                Err(error) => {
                    this.update(cx, |this, cx| {
                        this.tell("Cannot debug", error.to_string(), cx)
                    })
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
                    panel.start_plan(service, plan, name, entry, cx)
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

    /// Starts a test run with a debug agent and attaches to its JVM.
    pub fn debug_tests(
        &mut self,
        request: TestRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.starting || self.busy() {
            self.tell("Busy", "Stop what is running first", cx);
            return;
        }
        let Some(service) = self.debug_service.clone() else {
            self.tell("Cannot debug", "The language server is not running", cx);
            return;
        };
        let Some(provider) = self.test_provider.clone() else {
            self.tell("Cannot debug tests", "The project has no build tool", cx);
            return;
        };
        let Some(model) = self.session.as_ref().and_then(ProjectSession::model) else {
            self.tell("Cannot debug tests", "The project is still loading", cx);
            return;
        };
        self.waiting_tests = None;
        self.queued_tests = None;
        self.rerun = false;
        self.archive_console();
        self.title = title_of(&request).into();
        self.lines.clear();
        self.parser = AnsiParser::default();
        self.tests.begin(request.clone());
        self.starting = true;
        self.debugging_tests = true;
        let title = title_of(&request);
        // The key that starts this may come from an editor inside the workspace update.
        let workspace = self.workspace.clone();
        let handle = window.window_handle();
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
                            this.tell("Cannot debug tests", error, cx);
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
                        this.tell("Cannot debug tests", message, cx);
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
        let mut changed = self.poll_sync(cx);
        let mut done = Vec::new();
        let mut debug_lines: Vec<(String, bool)> = Vec::new();
        let mut exited: Option<Option<i32>> = None;
        let mut closed = false;
        let mut built = false;
        let main_console = self.console.is_some();
        let tests = self.tests.run.as_ref().map(TestRun::console);
        if let Some(console) = self.console.as_ref().or(tests) {
            for _ in 0..EVENTS_PER_POLL {
                let Ok(event) = console.events().try_recv() else {
                    break;
                };
                changed = true;
                match event {
                    ConsoleEvent::Output { stream, text } => {
                        let error = stream == Stream::Stderr;
                        if self.debugging_tests {
                            debug_lines.push((text.clone(), error));
                        }
                        self.parser.push(&text, error, &mut done);
                    }
                    ConsoleEvent::Started { step, .. } => {
                        if step == Step::Before {
                            done.push(info("Building the project"));
                        }
                        if step == Step::Program && main_console {
                            self.program_started = true;
                        }
                    }
                    ConsoleEvent::Failed { message } => {
                        done.push(Line::plain(message, error_style()));
                    }
                    ConsoleEvent::Exited { code, step } => {
                        self.parser.finish(&mut done);
                        done.push(Line::default());
                        done.push(info(exit_text(code, step)));
                        self.console_running = false;
                        if step == Step::Before && code == Some(0) {
                            built = true;
                        }
                        exited = Some(code);
                    }
                    // The pipes are closed: the last output, late as it was, is in.
                    ConsoleEvent::Closed => {
                        self.parser.finish(&mut done);
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
                    ConsoleEvent::Output { stream, text } => {
                        self.build_parser
                            .push(&text, stream == Stream::Stderr, &mut build_done);
                    }
                    ConsoleEvent::Started { .. } => {}
                    ConsoleEvent::Failed { message } => {
                        build_done.push(Line::plain(message, error_style()));
                    }
                    ConsoleEvent::Exited { code, .. } => {
                        self.build_parser.finish(&mut build_done);
                        build_done.push(Line::default());
                        build_done.push(info(exit_text(code, Step::Program)));
                        self.build_running = false;
                        build_exited = Some(code);
                    }
                    // The build is over; a daemon may still hold its pipes.
                    ConsoleEvent::Closed => {
                        self.build_parser.finish(&mut build_done);
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
                    ConsoleEvent::Output { stream, text } => {
                        drain
                            .parser
                            .push(&text, stream == Stream::Stderr, &mut drain.lines);
                    }
                    ConsoleEvent::Started { .. } | ConsoleEvent::Exited { .. } => {}
                    ConsoleEvent::Failed { message } => {
                        drain.lines.push(Line::plain(message, error_style()));
                    }
                    ConsoleEvent::Closed => {
                        drain.parser.finish(&mut drain.lines);
                        closed = true;
                        break;
                    }
                }
            }
            if closed {
                drain.console = None;
            }
        }
        self.lines.extend(done);
        let extra = self.lines.len().saturating_sub(MAX_LINES);
        self.lines.drain(..extra);
        if build_closed {
            self.build_console = None;
        }
        if let Some(code) = build_exited
            && std::mem::take(&mut self.building_project)
            && code == Some(0)
        {
            self.mark_built();
            self.request_reload(cx);
        }
        if closed && main_console {
            // The program is over and its pipes are closed.
            self.console = None;
        }
        if exited.is_some() && !main_console {
            self.tests.exited = Some(std::time::Instant::now());
        }
        if built {
            self.mark_built();
        }
        if let Some(code) = exited
            && std::mem::take(&mut self.building_project)
            && code == Some(0)
        {
            self.mark_built();
            self.request_reload(cx);
        }
        if !debug_lines.is_empty()
            && self.debugging_tests
            && let Some(debug) = cx.try_global::<crate::debug::ActiveDebug>()
        {
            let panel = debug.0.clone();
            panel.update(cx, |panel, cx| {
                for (text, error) in debug_lines {
                    panel.push_output(text, error, cx);
                }
            });
        }
        changed |= self.poll_tests(cx);
        changed |= self.poll_reload(cx);
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
        if changed {
            // The console waits for a line below the last one, so that row is in view too.
            let last = self.lines.len() + usize::from(self.takes_input());
            self.scroll.scroll_to_item(last, ScrollStrategy::Bottom);
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
            workspace.notify_user("Project", message, cx);
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
                    discover = true;
                    model_changed = true;
                    start_waiting = true;
                }
                Delivery::Event(SyncEvent::Failed(message)) => {
                    self.loading = false;
                    self.problems.push(message.clone());
                    self.tell("Project sync failed", message, cx);
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
        changed
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
        self.tell(title, text, cx);
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
            (stored, configs, problems)
        });
        cx.spawn(async move |this, cx| {
            let (stored, configs, mut problems) = found.await;
            this.update(cx, |this, cx| {
                this.discovered = configs;
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
                    this.tell("Run configurations", problem, cx);
                }
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
                        this.tell("Run configurations", error, cx);
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
        let (title, text) = (title.to_string(), text.into());
        let workspace = self.workspace.clone();
        cx.defer(move |cx| {
            workspace
                .update(cx, |workspace, cx| workspace.notify_user(title, text, cx))
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

    fn rows(
        &self,
        range: std::ops::Range<usize>,
        theme: &Theme,
        view: Option<usize>,
    ) -> Vec<AnyElement> {
        let drain = view.and_then(|index| self.draining.get(index));
        let current = match drain {
            Some(drain) => drain.parser.current(),
            None => self.parser.current(),
        };
        let typed = self.takes_input().then_some(self.lines.len() + 1);
        range
            .map(|index| {
                if typed == Some(index) {
                    return self.console_input.element("run-console-input", theme);
                }
                let line = match drain {
                    Some(drain) => drain.lines.get(index),
                    None => self.lines.get(index),
                };
                let line = line.unwrap_or(current);
                line_element(line, theme)
            })
            .collect()
    }
}

fn plan(
    provider: &dyn RunConfigProvider,
    config: &RunConfig,
    model: &ProjectModel,
) -> Result<RunPlan, String> {
    provider
        .plan(config, model)
        .map_err(|error| error.to_string())
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
    let (palette, error) = if theme.dark {
        (&ANSI_DARK, ERROR_DARK)
    } else {
        (&ANSI_LIGHT, ERROR_LIGHT)
    };
    let highlights = line.spans.iter().map(|(range, style)| {
        let color = match (style.input, style.color, style.error) {
            (true, _, _) => Some(theme.console_input),
            (false, Some(index), _) => Some(color(palette[usize::from(index)])),
            (false, None, true) => Some(color(error)),
            (false, None, false) => None,
        };
        let highlight = HighlightStyle {
            color,
            font_weight: style.bold.then_some(FontWeight::BOLD),
            ..HighlightStyle::default()
        };
        (range.clone(), highlight)
    });
    let text = StyledText::new(line.text.clone()).with_highlights(highlights);
    div()
        .h(px(CODE_LINE_HEIGHT))
        .px(px(LINE_PAD))
        .whitespace_nowrap()
        .child(text)
        .into_any_element()
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

impl Render for RunPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let view = self.viewing.filter(|index| *index < self.draining.len());
        let count = match view {
            Some(index) => self.draining[index].lines.len() + 1,
            None => self.lines.len() + 1 + usize::from(self.takes_input()),
        };
        let workspace = self.workspace.clone();
        let hide = move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
            workspace
                .update(cx, |workspace, cx| {
                    workspace.toggle_tool(RUN_TOOL, window, cx)
                })
                .ok();
        };
        let running = self.busy() || self.starting || self.waiting || self.waiting_tests.is_some();
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
            );
        let tabs = (!self.draining.is_empty()).then(|| {
            let current = tab_button(
                "run-tab-current",
                self.title.clone(),
                view.is_none(),
                &theme,
                cx.listener(|this, _, _, cx| {
                    this.viewing = None;
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
            .child(
                scrollable(
                    "run-console",
                    &self.scroll,
                    div().size_full().child(
                        uniform_list(
                            "run-console",
                            count,
                            cx.processor(move |this, range, _, _| this.rows(range, &theme, view)),
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
            .into_any_element();
        let body = if self.tests.active() {
            self.tests_body(console, &theme, window, cx)
        } else {
            console
        };
        island(&theme)
            .id("run-panel")
            .key_context("RunPanel ToolWindow")
            .track_focus(&self.focus)
            .group(GROUP)
            .h_full()
            .on_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)))
            .child(
                panel_header(self.title.clone(), GROUP, hide, &theme)
                    .debug_selector(|| "run-header".to_owned()),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(side)
                    .child(div().flex_1().min_w_0().h_full().child(body)),
            )
    }
}
