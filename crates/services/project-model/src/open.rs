use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;

use base::{Cancel, Result, Snapshot};
use proc_sup::ProcessSpec;
use workspace_core::{EventBus, Subscription};

use crate::{BuildModule, Module, ProjectModel, cache};

const EVENT_QUEUE: usize = 16;

/// One tool that knows how to read and build a project.
pub trait BuildSystem: Send + Sync {
    /// Whether this tool builds the project in `root`.
    fn detect(&self, root: &Path) -> bool;

    /// Reads project files without running anything, to have something to show at once.
    fn scan(&self, root: &Path) -> Result<ProjectModel>;

    /// Asks the tool for the full model. May take a long time and blocks.
    fn sync(&self, root: &Path, cancel: &Cancel) -> Result<ProjectModel>;

    /// The command that makes the outputs of the project current.
    fn assemble(&self, root: &Path) -> Result<ProcessSpec>;

    /// The command for a run, or `None` when the outputs are already current. A tool
    /// that writes a stamp after a successful build may skip an unchanged project.
    fn assemble_if_needed(&self, root: &Path) -> Result<Option<ProcessSpec>> {
        self.assemble(root).map(Some)
    }

    /// A build step of this tool finished with success, so the outputs are current.
    fn built(&self, root: &Path) {
        let _ = root;
    }

    /// The command of Build Project. Test classes compile too when `tests` is set.
    fn build(&self, root: &Path, tests: bool) -> Result<ProcessSpec> {
        let _ = tests;
        self.assemble(root)
    }

    /// The command of Compile: the module named, or the whole project when `module` is
    /// `None`. The outputs of the module are current when it ends.
    fn compile(&self, root: &Path, module: Option<&Module>) -> Result<ProcessSpec> {
        let _ = module;
        self.build(root, false)
    }

    /// The tasks of the root project as plain names, for scripts.
    fn tasks(&self, _model: &ProjectModel) -> Vec<String> {
        Vec::new()
    }

    /// The modules and their groups of tasks, in the order to show. The root module
    /// comes first.
    fn task_tree(&self, _model: &ProjectModel) -> Vec<BuildModule> {
        Vec::new()
    }

    /// The command that runs one of [`BuildSystem::tasks`] in `root`.
    fn task(&self, _root: &Path, task: &str) -> Result<ProcessSpec> {
        Err(format!("no task {task}").into())
    }

    /// The command that runs `task` of `module` in `root`; `None` is the root project.
    fn task_in(
        &self,
        root: &Path,
        _module: Option<&Module>,
        task: &str,
        _options: TaskOptions,
    ) -> Result<ProcessSpec> {
        self.task(root, task)
    }

    /// The command for a line typed by the user, such as `clean install`.
    fn goal(&self, _root: &Path, line: &str, _options: TaskOptions) -> Result<ProcessSpec> {
        Err(format!("no goal {line}").into())
    }

    /// The command that runs a task that starts a program under a debugger.
    fn debug_task_in(
        &self,
        root: &Path,
        module: Option<&Module>,
        task: &str,
        options: TaskOptions,
    ) -> Result<ProcessSpec> {
        self.task_in(root, module, task, options)
    }

    /// Whether [`TaskOptions::skip_tests`] changes what the tool runs.
    fn supports_skip_tests(&self) -> bool {
        false
    }
}

/// What a task run may change besides the task itself.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TaskOptions {
    /// Leaves the tests of the project out of this run.
    pub skip_tests: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SyncEvent {
    /// A model is available: an old one from the cache or a quick scan.
    Preliminary,
    Synced,
    /// The sync failed, the last model stays in place.
    Failed(String),
}

/// An open project. The model stays readable while a sync runs in the background.
pub struct ProjectSession {
    model: Arc<Snapshot<Option<Arc<ProjectModel>>>>,
    events: EventBus<SyncEvent>,
    cancel: Cancel,
}

impl ProjectSession {
    pub fn model(&self) -> Option<Arc<ProjectModel>> {
        (*self.model.get()).clone()
    }

    pub fn events(&self) -> Subscription<SyncEvent> {
        self.events.subscribe(EVENT_QUEUE)
    }

    /// Stops the sync that is reading the project. The model read so far stays.
    pub fn cancel_sync(&self) {
        self.cancel.cancel();
    }
}

impl Drop for ProjectSession {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// Starts in the background and returns at once: the cached model first, then a scan if
/// there is none, then the real sync. An old model is better than a window that waits.
pub fn open_project(
    tool: Arc<dyn BuildSystem>,
    root: PathBuf,
    cache_root: &Path,
) -> (ProjectSession, Subscription<SyncEvent>) {
    let session = ProjectSession {
        model: Arc::new(Snapshot::new(None)),
        events: EventBus::new(),
        cancel: Cancel::new(),
    };
    // Subscribed before the thread starts, so that nothing can be missed.
    let events = session.events();
    let file = cache::cache_file(cache_root, &root);
    let (model, bus, cancel) = (
        session.model.clone(),
        session.events.clone(),
        session.cancel.clone(),
    );
    thread::spawn(move || {
        let publish = |found: ProjectModel| model.set(Some(Arc::new(found)));
        let quick = cache::load(&file).or_else(|| tool.scan(&root).ok());
        if let Some(quick) = quick {
            publish(quick);
            bus.publish(SyncEvent::Preliminary);
        }
        match tool.sync(&root, &cancel) {
            Ok(found) => {
                if let Err(error) = cache::store(&file, &found) {
                    bus.publish(SyncEvent::Failed(format!("cache: {error}")));
                }
                publish(found);
                bus.publish(SyncEvent::Synced);
            }
            Err(error) => bus.publish(SyncEvent::Failed(error.to_string())),
        }
    });
    (session, events)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::mpsc::{Receiver, Sender, channel};
    use std::time::{Duration, Instant};

    use workspace_core::Delivery;

    use super::*;

    const PATIENCE: Duration = Duration::from_secs(10);

    /// A tool whose sync waits for the test to let it go.
    struct Fake {
        release: Mutex<Receiver<std::result::Result<ProjectModel, String>>>,
    }

    fn fake() -> (Arc<Fake>, Sender<std::result::Result<ProjectModel, String>>) {
        let (sender, release) = channel();
        let release = Mutex::new(release);
        (Arc::new(Fake { release }), sender)
    }

    impl BuildSystem for Fake {
        fn detect(&self, _root: &Path) -> bool {
            true
        }

        fn scan(&self, root: &Path) -> Result<ProjectModel> {
            Ok(named(root, "scanned"))
        }

        fn sync(&self, _root: &Path, _cancel: &Cancel) -> Result<ProjectModel> {
            Ok(self.release.lock().unwrap().recv()??)
        }

        fn assemble(&self, _root: &Path) -> Result<ProcessSpec> {
            Err("not used".into())
        }
    }

    fn named(root: &Path, tool: &str) -> ProjectModel {
        ProjectModel {
            root: root.to_owned(),
            tool: tool.into(),
            ..ProjectModel::default()
        }
    }

    fn next(events: &mut Subscription<SyncEvent>) -> SyncEvent {
        let start = Instant::now();
        loop {
            match events.try_next() {
                Some(Delivery::Event(event)) => return event,
                Some(lost) => panic!("{lost:?}"),
                None => assert!(start.elapsed() < PATIENCE, "no event in time"),
            }
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("project-model-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        path
    }

    fn tool_of(session: &ProjectSession) -> String {
        session.model().unwrap().tool.clone()
    }

    #[test]
    fn a_scan_is_shown_while_the_sync_runs_and_the_sync_replaces_it() {
        let cache = temp_dir("scan");
        let (tool, release) = fake();
        let root = PathBuf::from("project");

        let (session, mut events) = open_project(tool, root.clone(), &cache);

        assert_eq!(next(&mut events), SyncEvent::Preliminary);
        assert_eq!(tool_of(&session), "scanned");
        release.send(Ok(named(&root, "synced"))).unwrap();
        assert_eq!(next(&mut events), SyncEvent::Synced);
        assert_eq!(tool_of(&session), "synced");
        let _ = std::fs::remove_dir_all(cache);
    }

    #[test]
    fn a_second_open_starts_from_the_cache() {
        let cache = temp_dir("reopen");
        let root = PathBuf::from("project");
        let (tool, release) = fake();
        let (session, mut events) = open_project(tool, root.clone(), &cache);
        release.send(Ok(named(&root, "synced"))).unwrap();
        assert_eq!(next(&mut events), SyncEvent::Preliminary);
        assert_eq!(next(&mut events), SyncEvent::Synced);
        drop(session);

        let (tool, _release) = fake();
        let (session, mut events) = open_project(tool, root, &cache);

        assert_eq!(next(&mut events), SyncEvent::Preliminary);
        assert_eq!(tool_of(&session), "synced");
        let _ = std::fs::remove_dir_all(cache);
    }

    #[test]
    fn a_failed_sync_keeps_the_last_model() {
        let cache = temp_dir("failed");
        let (tool, release) = fake();

        let (session, mut events) = open_project(tool, PathBuf::from("project"), &cache);
        release.send(Err("broken".into())).unwrap();

        assert_eq!(next(&mut events), SyncEvent::Preliminary);
        assert_eq!(next(&mut events), SyncEvent::Failed("broken".into()));
        assert_eq!(tool_of(&session), "scanned");
        let _ = std::fs::remove_dir_all(cache);
    }

    #[test]
    fn a_broken_cache_is_ignored() {
        let cache = temp_dir("broken");
        let root = PathBuf::from("project");
        let file = cache::cache_file(&cache, &root);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "not a model").unwrap();
        let (tool, _release) = fake();

        let (session, mut events) = open_project(tool, root, &cache);

        assert_eq!(next(&mut events), SyncEvent::Preliminary);
        assert_eq!(tool_of(&session), "scanned");
        let _ = std::fs::remove_dir_all(cache);
    }
}
