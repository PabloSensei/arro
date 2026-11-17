//! What the console window needs from a Java project: the modules it offers and the
//! helper of a module started on the SDK of the project, with the module classpath.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use java_project::{BuildEnv, Jdk};
use project_model::{BuildSystem, ProjectModel, open_project};
use repl::{Launch, Support};

/// How often the support looks at the model the build tool is reading.
const MODEL_POLL: Duration = Duration::from_millis(500);
/// The unit whose outputs and dependencies a console sees.
const CONSOLE_UNIT: &str = "main";

/// The Java side of the console window. It owns the model session, so the module list and
/// the classpath follow the project without waiting for the window to ask.
pub struct JShellSupport {
    root: PathBuf,
    cache_root: PathBuf,
    model: Arc<Mutex<Option<Arc<ProjectModel>>>>,
}

impl JShellSupport {
    pub fn new(root: PathBuf, tool: Option<Arc<dyn BuildSystem>>, cache_root: PathBuf) -> Self {
        let model = Arc::new(Mutex::new(None));
        if let Some(tool) = tool {
            let slot = model.clone();
            let project = root.clone();
            let (session, mut events) = open_project(tool, project, &cache_root);
            std::thread::Builder::new()
                .name("console-model".into())
                .spawn(move || {
                    loop {
                        if let Some(found) = session.model()
                            && let Ok(mut slot) = slot.lock()
                        {
                            *slot = Some(found);
                        }
                        // The subscription wakes nothing by itself; the session keeps the
                        // model alive and the poll is cheap, a snapshot read.
                        let _ = events.try_next();
                        std::thread::sleep(MODEL_POLL);
                    }
                })
                .ok();
        }
        Self {
            root,
            cache_root,
            model,
        }
    }

    fn model(&self) -> Option<Arc<ProjectModel>> {
        self.model
            .lock()
            .ok()
            .and_then(|model| model.as_ref().cloned())
    }

    fn jdk(&self) -> Result<Jdk, String> {
        java_project::resolve_environment(&self.root)
            .selected
            .ok_or_else(|| "the project has no SDK to start the console".to_owned())
    }
}

impl Support for JShellSupport {
    fn modules(&self) -> Vec<String> {
        self.model()
            .map(|model| {
                model
                    .modules
                    .iter()
                    .map(|module| module.id.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn launch(&self, module: &str) -> Result<Launch, String> {
        let helpers = BuildEnv::default()
            .helpers_dir()
            .map_err(|error| format!("no helpers folder: {error}"))?;
        let helper = helpers.join("ide-jshell-helper.jar");
        if !helper.is_file() {
            return Err(format!(
                "{} is missing; build the helpers of the IDE first",
                helper.display()
            ));
        }
        let jdk = self.jdk()?;
        if jdk.feature < 9 {
            return Err(format!(
                "JShell needs JDK 9 or newer; the project SDK is Java {}",
                jdk.version
            ));
        }
        let mut paths = vec![helper];
        if let Some(model) = self.model() {
            paths.extend(model.runtime_paths(module, CONSOLE_UNIT));
        }
        let classpath = java_exec::classpath_jar(&self.cache_root, &self.root, &paths)
            .map_err(|error| format!("no classpath file: {error}"))?;
        Ok(Launch::new(jdk.java())
            .env("JAVA_HOME", jdk.home.clone())
            .arg("-cp")
            .arg(&classpath)
            .arg("ide.jshell.Helper")
            .arg("--classpath")
            .arg(classpath)
            .working_dir(self.root.clone()))
    }
}
