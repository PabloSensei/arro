//! The Lombok agent of one project. The jar comes from the project dependencies; the
//! server restarts when the project starts to use Lombok or changes its version.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use java_project::{LocalRepositories, Lombok as ProjectLombok};
use jdtls_adapter::{Jdtls, Lombok};
use project_model::ProjectModel;
use sidecar::ReloadHandle;
use tokio::runtime::Handle;

/// The server restarts with the version it found. The line goes to the notification of
/// the window once.
const ENABLED: &str = "Lombok support enabled";

#[derive(Clone)]
struct Shared {
    spec: Arc<Jdtls>,
    reload: ReloadHandle,
    note: Arc<Mutex<Option<String>>>,
    generation: Arc<AtomicU64>,
}

pub(crate) struct LombokSupport {
    root: PathBuf,
    repositories: LocalRepositories,
    runtime: Handle,
    shared: Shared,
}

impl LombokSupport {
    pub(crate) fn new(
        root: PathBuf,
        spec: Arc<Jdtls>,
        reload: ReloadHandle,
        repositories: LocalRepositories,
    ) -> Self {
        Self {
            root,
            repositories,
            runtime: Handle::current(),
            shared: Shared {
                spec,
                reload,
                note: Arc::new(Mutex::new(None)),
                generation: Arc::new(AtomicU64::new(0)),
            },
        }
    }

    /// The model of the project arrived. The lookup reads the repository and copies the
    /// jar, so it runs off the UI thread.
    pub(crate) fn project_model(&self, model: Arc<ProjectModel>) {
        let generation = self.next_generation();
        let repositories = self.repositories.clone();
        let shared = self.shared.clone();
        self.runtime.spawn_blocking(move || {
            let found = java_project::find_lombok(&model, &repositories);
            apply(&shared, generation, found);
        });
    }

    /// A build file changed: a dependency may have been added or its version changed
    /// before the project model catches up.
    pub(crate) fn build_files_changed(&self) {
        let generation = self.next_generation();
        let root = self.root.clone();
        let repositories = self.repositories.clone();
        let shared = self.shared.clone();
        self.runtime.spawn_blocking(move || {
            let found = java_project::find_lombok_in_build_files(&root, &repositories);
            apply(&shared, generation, found);
        });
    }

    /// The Lombok jar of `model`, with the repositories this project uses.
    pub(crate) fn of(&self, model: &ProjectModel) -> Option<ProjectLombok> {
        java_project::find_lombok(model, &self.repositories)
    }

    pub(crate) fn repositories(&self) -> &LocalRepositories {
        &self.repositories
    }

    /// The line the window shows and has not seen yet.
    pub(crate) fn take_note(&self) -> Option<String> {
        self.shared
            .note
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .take()
    }

    fn next_generation(&self) -> u64 {
        self.shared.generation.fetch_add(1, Ordering::SeqCst) + 1
    }
}

fn apply(shared: &Shared, mine: u64, found: Option<ProjectLombok>) {
    if shared.generation.load(Ordering::SeqCst) != mine {
        return;
    }
    let agent = found.map(|lombok| Lombok {
        version: lombok.version,
        jar: lombok.jar,
    });
    if !matches!(shared.spec.set_lombok(agent), Ok(true)) {
        return;
    }
    if shared.reload.reload() {
        *shared
            .note
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = Some(ENABLED.to_owned());
    }
}

/// The repositories of the project, with the settings and the Gradle home the caller
/// knows when they are not the default ones.
pub(crate) fn repositories(
    maven_settings: Option<&Path>,
    gradle_home: Option<&Path>,
) -> LocalRepositories {
    LocalRepositories::detect(maven_settings, gradle_home)
}
