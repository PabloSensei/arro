//! The sources of a class that comes from a library. The server says which jar the class
//! belongs to and whether a sources jar is attached; the repositories of the project say
//! where to fetch a missing one. The window only hears about a class of a library.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base::Cancel;
use lang_api::{
    Answer, Confidence, LibraryClass, LibraryClassQuery, Provider, Reply, SourceId, SourcesEvent,
    SourcesKind, SourcesRequest,
};
use project_model::{PathKind, ProjectModel};
use semantic_router::Router;
use serde_json::{Value, json};
use tokio::runtime::Handle;

use java_project::{LocalRepositories, SourceLocation};

const SOURCE: SourceId = SourceId::new("jdtls");
const TIMEOUT: Duration = Duration::from_secs(30);
const RESOLVE: &str = "java.project.resolveSourceAttachment";
const ATTACH: &str = "java.project.updateSourceAttachment";
/// The choices of the user and the Gradle downloads, by jar, so that a new server finds
/// them without asking again.
const REMEMBERED: &str = "source-attachments.json";

/// The jars of the project, for the request that fetches every source at once.
type Model = Arc<Mutex<Option<Arc<ProjectModel>>>>;

/// The sources attached to jars, by jar, kept in the state folder of the project.
#[derive(Default)]
struct Remembered {
    path: PathBuf,
    map: Mutex<HashMap<PathBuf, PathBuf>>,
}

impl Remembered {
    fn load(cache: &Path) -> Self {
        let path = cache.join(REMEMBERED);
        let map = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        Self {
            path,
            map: Mutex::new(map),
        }
    }

    fn get(&self, jar: &Path) -> Option<PathBuf> {
        let map = self.map.lock().ok()?;
        map.get(jar).filter(|path| path.is_file()).cloned()
    }

    fn remember(&self, jar: &Path, sources: &Path) {
        let Ok(mut map) = self.map.lock() else {
            return;
        };
        map.insert(jar.to_path_buf(), sources.to_path_buf());
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(text) = serde_json::to_string_pretty(&*map) {
            let _ = std::fs::write(&self.path, text);
        }
    }
}

/// The class the server knows behind a tab.
struct Resolved {
    jar: PathBuf,
    /// The sources the classpath already names, if any.
    source: Option<PathBuf>,
    artifact: String,
}

impl Resolved {
    fn has_source(&self) -> bool {
        self.source.as_ref().is_some_and(|path| path.is_file())
    }
}

/// The sources of the library classes of one open project.
pub(crate) struct LibrarySources {
    router: Arc<Router>,
    runtime: Handle,
    root: PathBuf,
    repositories: LocalRepositories,
    cache: PathBuf,
    model: Model,
    remembered: Arc<Remembered>,
}

impl LibrarySources {
    pub(crate) fn new(
        router: Arc<Router>,
        runtime: Handle,
        root: PathBuf,
        repositories: LocalRepositories,
        cache: PathBuf,
        model: Model,
    ) -> Self {
        Self {
            router,
            runtime,
            root,
            remembered: Arc::new(Remembered::load(&cache)),
            repositories,
            cache,
            model,
        }
    }

    fn repositories(&self) -> Vec<String> {
        // The model of the build tool carries no repositories today; the build files
        // name them, and the central repository is the fallback.
        java_project::repositories_of(&self.root, &[])
    }
}

/// The artifact name the line above the tab shows: the name of the jar, without its
/// extension.
fn artifact_of(jar: &Path) -> String {
    jar.file_stem()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| jar.display().to_string())
}

/// Asks the server for the jar of a class and the sources of that jar.
async fn resolve(router: &Router, uri: &str) -> Option<Resolved> {
    let answer = router
        .extension(
            "workspace/executeCommand",
            json!({"command": RESOLVE, "arguments": [{"classFileUri": uri}]}),
        )
        .await
        .ok()?;
    if answer
        .get("errorMessage")
        .and_then(Value::as_str)
        .is_some_and(|message| !message.is_empty())
    {
        return None;
    }
    let attributes = answer.get("attributes")?;
    let jar = PathBuf::from(attributes.get("jarPath")?.as_str()?);
    let source = attributes
        .get("sourceAttachmentPath")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from);
    Some(Resolved {
        artifact: artifact_of(&jar),
        jar,
        source,
    })
}

/// Asks the server for the jar of a class and the sources of that jar, attaching the
/// sources this project already chose when the server is new.
async fn resolve_attached(router: &Router, uri: &str, remembered: &Remembered) -> Option<Resolved> {
    let mut found = resolve(router, uri).await?;
    if !found.has_source()
        && let Some(sources) = remembered.get(&found.jar)
        && attach(router, uri, &sources).await
    {
        found.source = Some(sources);
    }
    Some(found)
}

/// Tells the server that the sources of the jar of a class are at `path`.
async fn attach(router: &Router, uri: &str, sources: &Path) -> bool {
    let path = sources.display().to_string();
    let Ok(answer) = router
        .extension(
            "workspace/executeCommand",
            json!({
                "command": ATTACH,
                "arguments": [{
                    "classFileUri": uri,
                    "attributes": {"sourceAttachmentPath": path},
                }],
            }),
        )
        .await
    else {
        return false;
    };
    !answer
        .get("errorMessage")
        .and_then(Value::as_str)
        .is_some_and(|message| !message.is_empty())
}

impl Provider<LibraryClassQuery, Option<LibraryClass>> for LibrarySources {
    fn start(&self, query: LibraryClassQuery, _: Cancel, mut reply: Reply<Option<LibraryClass>>) {
        let router = self.router.clone();
        let remembered = self.remembered.clone();
        self.runtime.spawn(async move {
            let class = resolve_attached(&router, &query.0, &remembered)
                .await
                .map(|found| {
                    let has_source = found.has_source();
                    LibraryClass {
                        artifact: found.artifact,
                        detail: None,
                        has_source,
                    }
                });
            let _ = reply.send(Answer::new(class, Confidence::Certain, SOURCE));
        });
    }
}

impl Provider<SourcesRequest, SourcesEvent> for LibrarySources {
    fn start(&self, request: SourcesRequest, _: Cancel, mut reply: Reply<SourcesEvent>) {
        let kind = request.kind;
        let cancel = request.cancel.clone();
        match kind {
            SourcesKind::Class(uri) => {
                let router = self.router.clone();
                let repositories = self.repositories.clone();
                let cache = self.cache.clone();
                let remembered = self.remembered.clone();
                let urls = self.repositories();
                self.runtime.spawn(async move {
                    let fetch = Fetch {
                        repositories: &repositories,
                        cache: &cache,
                        urls: &urls,
                        remembered: &remembered,
                    };
                    class_sources(&router, &fetch, uri, cancel, reply).await;
                });
            }
            SourcesKind::Attach { uri, path } => {
                let router = self.router.clone();
                let remembered = self.remembered.clone();
                self.runtime.spawn(async move {
                    let sources = std::fs::canonicalize(&path).unwrap_or(path);
                    let jar = resolve(&router, &uri).await.map(|found| found.jar);
                    if attach(&router, &uri, &sources).await {
                        if let Some(jar) = jar {
                            remembered.remember(&jar, &sources);
                        }
                        let _ = reply.send(answer(SourcesEvent::Attached { uri }));
                    } else {
                        let _ = reply.send(answer(SourcesEvent::Failed {
                            message: "The server did not take the chosen sources".to_owned(),
                        }));
                    }
                });
            }
            SourcesKind::All => {
                let jars = self.library_jars();
                let repositories = self.repositories.clone();
                let cache = self.cache.clone();
                let urls = self.repositories();
                let remembered = self.remembered.clone();
                let router = self.router.clone();
                self.runtime.spawn(async move {
                    let fetched = tokio::task::spawn_blocking(move || {
                        let fetch = Fetch {
                            repositories: &repositories,
                            cache: &cache,
                            urls: &urls,
                            remembered: &remembered,
                        };
                        all_sources(&jars, &fetch, cancel, reply)
                    })
                    .await;
                    if let Ok((fetched, cancelled)) = fetched
                        && fetched > 0
                        && !cancelled
                    {
                        // The server reads the new files of a local repository after an
                        // import, the way it reads a dependency that appeared.
                        let _ = router
                            .extension(
                                "workspace/executeCommand",
                                json!({"command": "java.project.import", "arguments": []}),
                            )
                            .await;
                    }
                });
            }
        }
    }
}

impl LibrarySources {
    /// Every library jar of the project model, once.
    fn library_jars(&self) -> Vec<PathBuf> {
        let model = self
            .model
            .lock()
            .map(|model| model.clone())
            .unwrap_or_default();
        let mut jars: Vec<PathBuf> = Vec::new();
        for module in model.iter().flat_map(|model| &model.modules) {
            for unit in &module.units {
                for entry in &unit.compile_paths {
                    if !matches!(&entry.kind, PathKind::Library { .. }) {
                        continue;
                    }
                    if entry
                        .path
                        .extension()
                        .is_some_and(|extension| extension == "jar")
                        && !jars.contains(&entry.path)
                    {
                        jars.push(entry.path.clone());
                    }
                }
            }
        }
        jars
    }
}

fn answer(event: SourcesEvent) -> Answer<SourcesEvent> {
    Answer::new(event, Confidence::Certain, SOURCE)
}

/// Where the files come from and where they go, for one request.
struct Fetch<'a> {
    repositories: &'a LocalRepositories,
    cache: &'a Path,
    urls: &'a [String],
    remembered: &'a Remembered,
}

async fn class_sources(
    router: &Router,
    fetch: &Fetch<'_>,
    uri: String,
    cancel: Cancel,
    mut reply: Reply<SourcesEvent>,
) {
    let Some(found) = resolve_attached(router, &uri, fetch.remembered).await else {
        let _ = reply.send(answer(SourcesEvent::Failed {
            message: "The server does not know this class".to_owned(),
        }));
        return;
    };
    if found.has_source() {
        let _ = reply.send(answer(SourcesEvent::Attached { uri }));
        return;
    }
    let Some(location) = java_project::source_location(&found.jar, fetch.repositories, fetch.cache)
    else {
        let _ = reply.send(answer(SourcesEvent::Failed {
            message: format!(
                "{} is outside the local repositories of the project",
                found.jar.display()
            ),
        }));
        return;
    };
    let _ = reply.send(answer(SourcesEvent::Started { total: 1 }));
    let outcome = download(location.clone(), fetch.urls.to_vec(), cancel.clone()).await;
    match outcome {
        JavaOutcome::Fetched | JavaOutcome::AlreadyThere => {}
        JavaOutcome::Missing => {
            let _ = reply.send(answer(SourcesEvent::Missing {
                name: location.artifact,
            }));
            return;
        }
        JavaOutcome::Cancelled => {
            let _ = reply.send(answer(SourcesEvent::Cancelled));
            return;
        }
        JavaOutcome::Failed(message) => {
            let _ = reply.send(answer(SourcesEvent::Failed { message }));
            return;
        }
    }
    let _ = reply.send(answer(SourcesEvent::Progress {
        done: 1,
        total: 1,
        name: location.artifact.clone(),
    }));
    if attach(router, &uri, &location.sources).await {
        fetch.remembered.remember(&found.jar, &location.sources);
        let _ = reply.send(answer(SourcesEvent::Attached { uri }));
    } else {
        let _ = reply.send(answer(SourcesEvent::Failed {
            message: "The server did not take the downloaded sources".to_owned(),
        }));
    }
}

/// What one fetch did, in the words of this module, for the blocking thread.
#[derive(Debug)]
enum JavaOutcome {
    Fetched,
    AlreadyThere,
    Missing,
    Cancelled,
    Failed(String),
}

async fn download(location: SourceLocation, urls: Vec<String>, cancel: Cancel) -> JavaOutcome {
    tokio::task::spawn_blocking(move || {
        match java_project::fetch(&location, &urls, TIMEOUT, &cancel) {
            Ok(java_project::Fetched::Downloaded) => JavaOutcome::Fetched,
            Ok(java_project::Fetched::AlreadyThere) => JavaOutcome::AlreadyThere,
            Err(java_project::FetchError::Missing) => JavaOutcome::Missing,
            Err(java_project::FetchError::Cancelled) => JavaOutcome::Cancelled,
            Err(java_project::FetchError::Io(message)) => JavaOutcome::Failed(message),
            Err(java_project::FetchError::Network(message)) => JavaOutcome::Failed(message),
        }
    })
    .await
    .unwrap_or_else(|error| JavaOutcome::Failed(error.to_string()))
}

/// Downloads every missing sources jar and reports each file through the reply.
fn all_sources(
    jars: &[PathBuf],
    fetch: &Fetch<'_>,
    cancel: Cancel,
    mut reply: Reply<SourcesEvent>,
) -> (usize, bool) {
    let mut pending = Vec::new();
    let mut skipped = 0;
    for jar in jars {
        let Some(location) = java_project::source_location(jar, fetch.repositories, fetch.cache)
        else {
            continue;
        };
        if location.sources.is_file() {
            skipped += 1;
        } else {
            pending.push(location);
        }
    }
    let total = pending.len();
    let _ = reply.send(answer(SourcesEvent::Started { total }));
    let mut fetched = 0;
    let mut failed = 0;
    for (index, location) in pending.iter().enumerate() {
        if cancel.is_cancelled() || reply.is_cancelled() {
            let _ = reply.send(answer(SourcesEvent::Cancelled));
            return (fetched, true);
        }
        match java_project::fetch(location, fetch.urls, TIMEOUT, &cancel) {
            Ok(java_project::Fetched::Downloaded) => {
                fetched += 1;
                fetch.remembered.remember(&location.jar, &location.sources);
            }
            Ok(java_project::Fetched::AlreadyThere) => {
                skipped += 1;
                fetch.remembered.remember(&location.jar, &location.sources);
            }
            Err(java_project::FetchError::Cancelled) => {
                let _ = reply.send(answer(SourcesEvent::Cancelled));
                return (fetched, true);
            }
            Err(_) => failed += 1,
        }
        let _ = reply.send(answer(SourcesEvent::Progress {
            done: index + 1,
            total,
            name: location.artifact.clone(),
        }));
    }
    let _ = reply.send(answer(SourcesEvent::Summary {
        fetched,
        skipped,
        failed,
    }));
    (fetched, false)
}
