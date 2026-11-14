//! Where the sources of a library jar belong, the address to ask for them, and the
//! download itself. A Maven repository keeps them next to the jar, as Maven does; the
//! cache of the second build tool is not written, so its files land in the IDE cache.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use base::Cancel;
use roxmltree::Document;

use crate::lombok::LocalRepositories;

/// The place every project falls back to when its build names no repository.
pub const MAVEN_CENTRAL: &str = "https://repo1.maven.org/maven2";

/// The folder of the IDE cache that holds the sources of jars outside a local repository.
pub const CACHE_FOLDER: &str = "library-sources";

/// The folder of a Maven local repository the sources go to, or the IDE cache.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceStore {
    /// Next to the jar, in the local repository: Maven finds it without the IDE.
    Repository,
    /// In the cache of the IDE, for a jar whose cache must not be written.
    Cache,
}

/// One sources jar: the jar of the class, the file to fetch and where it goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceLocation {
    /// The jar the class came from.
    pub jar: PathBuf,
    /// The file to download, or the file that already exists.
    pub sources: PathBuf,
    /// `group/path/artifact/version/artifact-version-sources.jar`.
    pub relative: PathBuf,
    /// `artifact-version`, as the line above the tab names it.
    pub artifact: String,
    pub store: SourceStore,
}

impl SourceLocation {
    /// The address of the file on a repository whose root is `repository`.
    pub fn url(&self, repository: &str) -> String {
        let base = repository.trim_end_matches('/');
        let path = self
            .relative
            .components()
            .map(|part| part.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        format!("{base}/{path}")
    }
}

/// What a fetch did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fetched {
    Downloaded,
    /// The file was already there; nothing was asked of a server.
    AlreadyThere,
}

/// Why a fetch gave nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FetchError {
    /// Every repository answered that it has no such file.
    Missing,
    Cancelled,
    Io(String),
    Network(String),
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing => formatter.write_str("no repository has the sources"),
            Self::Cancelled => formatter.write_str("the download was stopped"),
            Self::Io(message) => write!(formatter, "cannot write the sources: {message}"),
            Self::Network(message) => write!(formatter, "the download failed: {message}"),
        }
    }
}

impl std::error::Error for FetchError {}

/// Where the sources of `jar` belong. A jar outside every known repository has no place.
pub fn source_location(
    jar: &Path,
    repositories: &LocalRepositories,
    cache: &Path,
) -> Option<SourceLocation> {
    maven_location(jar, repositories).or_else(|| gradle_location(jar, repositories, cache))
}

/// `…/group/path/artifact/version/artifact-version.jar` becomes
/// `…/group/path/artifact/version/artifact-version-sources.jar`. The longest repository
/// root wins: the environment names both a home and its `repository` folder.
fn maven_location(jar: &Path, repositories: &LocalRepositories) -> Option<SourceLocation> {
    let mut best: Option<(usize, String, PathBuf, PathBuf)> = None;
    for repository in &repositories.maven {
        let Ok(relative) = jar.strip_prefix(repository) else {
            continue;
        };
        let parts = parts_of(relative);
        if parts.len() < 4 {
            continue;
        }
        let file = &parts[parts.len() - 1];
        let version = &parts[parts.len() - 2];
        let artifact = &parts[parts.len() - 3];
        let expected = format!("{artifact}-{version}");
        if !file.starts_with(&expected) || !file.ends_with(".jar") {
            continue;
        }
        let name = format!("{expected}-sources.jar");
        let mut source_parts = parts[..parts.len() - 1].to_vec();
        source_parts.push(name);
        let source_relative = joined(&source_parts);
        let depth = source_relative.components().count();
        if best.as_ref().is_none_or(|(known, ..)| depth < *known) {
            best = Some((depth, expected, source_relative, repository.clone()));
        }
    }
    best.map(|(_, artifact, relative, repository)| SourceLocation {
        jar: jar.to_path_buf(),
        sources: repository.join(&relative),
        relative,
        artifact,
        store: SourceStore::Repository,
    })
}

/// `…/files-2.1/group/artifact/version/hash/artifact-version.jar` keeps its cache, so
/// the sources go to the IDE cache with the same layout a repository uses.
fn gradle_location(
    jar: &Path,
    repositories: &LocalRepositories,
    cache: &Path,
) -> Option<SourceLocation> {
    for repository in &repositories.gradle {
        let Ok(relative) = jar.strip_prefix(repository) else {
            continue;
        };
        let parts = parts_of(relative);
        if parts.len() < 5 {
            continue;
        }
        let file = &parts[parts.len() - 1];
        let version = &parts[parts.len() - 3];
        let artifact = &parts[parts.len() - 4];
        let group = &parts[parts.len() - 5];
        let expected = format!("{artifact}-{version}");
        if !file.starts_with(&expected) || !file.ends_with(".jar") {
            continue;
        }
        let name = format!("{expected}-sources.jar");
        let group_path = group.replace('.', "/");
        let relative = joined(&[
            group_path.clone(),
            artifact.clone(),
            version.clone(),
            name.clone(),
        ]);
        let sources = cache.join(CACHE_FOLDER).join(&relative);
        return Some(SourceLocation {
            jar: jar.to_path_buf(),
            sources,
            relative,
            artifact: expected,
            store: SourceStore::Cache,
        });
    }
    None
}

fn parts_of(relative: &Path) -> Vec<String> {
    relative
        .components()
        .map(|part| part.as_os_str().to_string_lossy().into_owned())
        .collect()
}

fn joined(parts: &[String]) -> PathBuf {
    let mut path = PathBuf::new();
    for part in parts {
        path.push(part);
    }
    path
}

/// The addresses to ask, in order: the project's own, then the central one. The model
/// comes first because the build tool knows every repository of the project.
pub fn repositories_of(root: &Path, model: &[String]) -> Vec<String> {
    let mut urls: Vec<String> = model
        .iter()
        .map(|url| url.trim().to_owned())
        .filter(|url| !url.is_empty())
        .collect();
    urls.extend(build_file_repositories(root));
    if urls.is_empty() {
        urls.push(MAVEN_CENTRAL.to_owned());
    }
    let mut seen = Vec::new();
    urls.retain(|url| {
        if seen.contains(url) {
            return false;
        }
        seen.push(url.clone());
        true
    });
    urls
}

/// The repository addresses written in the build files, for a model that has none.
pub(crate) fn build_file_repositories(root: &Path) -> Vec<String> {
    let mut urls = Vec::new();
    if let Ok(text) = fs::read_to_string(root.join("pom.xml"))
        && let Ok(document) = Document::parse(&text)
    {
        let found = document.descendants().filter(|node| {
            node.is_element()
                && node.tag_name().name() == "url"
                && node
                    .ancestors()
                    .any(|parent| parent.is_element() && parent.tag_name().name() == "repositories")
        });
        urls.extend(
            found
                .filter_map(|node| node.text())
                .map(str::trim)
                .filter_map(|url| {
                    url.starts_with("http://")
                        .then(|| url.to_owned())
                        .or_else(|| url.starts_with("https://").then(|| url.to_owned()))
                }),
        );
    }
    for name in ["build.gradle", "build.gradle.kts"] {
        let Ok(text) = fs::read_to_string(root.join(name)) else {
            continue;
        };
        for line in text.lines() {
            if !line.contains("maven") && !line.contains("url") {
                continue;
            }
            let Some(start) = line.find("https://").or_else(|| line.find("http://")) else {
                continue;
            };
            let rest = &line[start..];
            let end = rest
                .find(|character: char| {
                    character.is_whitespace() || character == '"' || character == '\''
                })
                .unwrap_or(rest.len());
            if end > 0 {
                urls.push(rest[..end].to_owned());
            }
        }
    }
    urls
}

/// Fetches the file of `location` from the first repository that has it. The file is
/// written beside its destination first, so a stop leaves no half a jar behind.
pub fn fetch(
    location: &SourceLocation,
    repositories: &[String],
    timeout: Duration,
    cancel: &Cancel,
) -> Result<Fetched, FetchError> {
    if location.sources.is_file() {
        return Ok(Fetched::AlreadyThere);
    }
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .timeout_connect(Some(timeout))
        .http_status_as_error(false)
        .max_redirects(5)
        .max_redirects_will_error(true)
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .build(),
        )
        .build()
        .into();
    let mut last_error = None;
    for repository in repositories {
        if cancel.is_cancelled() {
            return Err(FetchError::Cancelled);
        }
        let url = location.url(repository);
        let response = match agent.get(&url).call() {
            Ok(response) => response,
            Err(ureq::Error::Timeout(_)) => {
                last_error = Some(FetchError::Network(format!("{url} timed out")));
                continue;
            }
            Err(ureq::Error::Io(error)) if error.kind() == std::io::ErrorKind::TimedOut => {
                last_error = Some(FetchError::Network(format!("{url} timed out")));
                continue;
            }
            Err(error) => {
                last_error = Some(FetchError::Network(format!("{url}: {error}")));
                continue;
            }
        };
        let status = response.status();
        if status.as_u16() == 404 || status.as_u16() == 410 {
            continue;
        }
        if !status.is_success() {
            last_error = Some(FetchError::Network(format!(
                "{url} answered {}",
                status.as_u16()
            )));
            continue;
        }
        let mut bytes = Vec::new();
        let mut reader = response.into_body().into_reader();
        let mut buffer = [0u8; 16 * 1024];
        loop {
            if cancel.is_cancelled() {
                return Err(FetchError::Cancelled);
            }
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => bytes.extend_from_slice(&buffer[..read]),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    return Err(FetchError::Io(format!("{url}: {error}")));
                }
            }
        }
        write(&location.sources, &bytes)?;
        return Ok(Fetched::Downloaded);
    }
    Err(last_error.unwrap_or(FetchError::Missing))
}

fn write(destination: &Path, bytes: &[u8]) -> Result<(), FetchError> {
    let Some(parent) = destination.parent() else {
        return Err(FetchError::Io(format!(
            "{} has no folder",
            destination.display()
        )));
    };
    fs::create_dir_all(parent).map_err(|error| FetchError::Io(error.to_string()))?;
    let name = destination
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "sources.jar".to_owned());
    let part = parent.join(format!("{name}.part"));
    fs::write(&part, bytes).map_err(|error| FetchError::Io(error.to_string()))?;
    fs::rename(&part, destination).map_err(|error| {
        let _ = fs::remove_file(&part);
        FetchError::Io(error.to_string())
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repositories(maven: &str, gradle: &str) -> LocalRepositories {
        LocalRepositories {
            maven: vec![PathBuf::from(maven)],
            gradle: vec![PathBuf::from(gradle)],
        }
    }

    #[test]
    fn the_longest_repository_root_names_the_file() {
        let home = Path::new("C:/m2");
        let repository = home.join("repository");
        let jar =
            repository.join("org/apache/commons/commons-lang3/3.20.0/commons-lang3-3.20.0.jar");
        let repositories = LocalRepositories {
            maven: vec![home.to_path_buf(), repository.clone()],
            gradle: Vec::new(),
        };

        let found = source_location(&jar, &repositories, Path::new("C:/state"))
            .expect("a place for the file");

        assert_eq!(
            found.url("https://repo1.maven.org/maven2"),
            "https://repo1.maven.org/maven2/org/apache/commons/commons-lang3/3.20.0/\
             commons-lang3-3.20.0-sources.jar",
            "the home itself is not a part of the address"
        );
    }

    #[test]
    fn a_maven_jar_gets_its_sources_beside_it() {
        let root = Path::new("C:/m2/repository");
        let jar = root.join("org/apache/commons/commons-lang3/3.20.0/commons-lang3-3.20.0.jar");
        let repositories = repositories("C:/m2/repository", "C:/gradle/files-2.1");
        let cache = Path::new("C:/state");

        let found = source_location(&jar, &repositories, cache).expect("a place for the file");

        assert_eq!(found.store, SourceStore::Repository);
        assert_eq!(found.artifact, "commons-lang3-3.20.0");
        assert_eq!(
            found.sources,
            root.join("org/apache/commons/commons-lang3/3.20.0/commons-lang3-3.20.0-sources.jar")
        );
        assert_eq!(
            found.url("https://repo1.maven.org/maven2"),
            "https://repo1.maven.org/maven2/org/apache/commons/commons-lang3/3.20.0/\
             commons-lang3-3.20.0-sources.jar"
        );
    }

    #[test]
    fn a_gradle_jar_keeps_its_cache_and_the_sources_go_to_the_ide() {
        let root = Path::new("C:/gradle/caches/modules-2/files-2.1");
        let jar = root.join("com.example/demo/1.2.3/abcdef012345/demo-1.2.3.jar");
        let repositories = repositories("C:/m2/repository", "C:/gradle/caches/modules-2/files-2.1");
        let cache = Path::new("C:/state");

        let found = source_location(&jar, &repositories, cache).expect("a place for the file");

        assert_eq!(found.store, SourceStore::Cache);
        assert_eq!(found.artifact, "demo-1.2.3");
        assert_eq!(
            found.sources,
            cache.join("library-sources/com/example/demo/1.2.3/demo-1.2.3-sources.jar")
        );
        assert_eq!(
            found.url("https://mirror.example/maven"),
            "https://mirror.example/maven/com/example/demo/1.2.3/demo-1.2.3-sources.jar"
        );
    }

    #[test]
    fn a_jar_outside_the_repositories_has_no_place() {
        let repositories = repositories("C:/m2/repository", "C:/gradle/files-2.1");
        assert!(
            source_location(
                Path::new("C:/somewhere/other.jar"),
                &repositories,
                Path::new("C:/state")
            )
            .is_none()
        );
    }

    #[test]
    fn an_existing_file_is_not_fetched_again() {
        let root = test_support::scratch("sources-existing");
        let jar = root.join("g/a/1.0/a-1.0.jar");
        let found = source_location(
            &jar,
            &LocalRepositories {
                maven: vec![root.clone()],
                gradle: Vec::new(),
            },
            &root,
        )
        .expect("a place for the file");
        fs::create_dir_all(found.sources.parent().unwrap()).unwrap();
        fs::write(&found.sources, b"source").unwrap();

        let fetched = fetch(&found, &[], Duration::from_secs(1), &Cancel::new());

        assert_eq!(fetched, Ok(Fetched::AlreadyThere));
    }
}
