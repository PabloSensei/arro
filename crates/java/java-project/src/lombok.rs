//! The Lombok jar of a project: the version from the project model, the file from the
//! local repositories, and the build files as a fallback when the model is older than
//! the dependency.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use project_model::{PathKind, ProjectModel};
use roxmltree::Document;

pub const LOMBOK_GROUP: &str = "org.projectlombok";
pub const LOMBOK_ARTIFACT: &str = "lombok";

/// The jar to give the language server, and the version it declares.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lombok {
    pub version: String,
    pub jar: PathBuf,
}

/// A Lombok dependency a build file mentions. The version is absent when the file
/// inherits it from a parent or a BOM.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LombokDependency {
    pub version: Option<String>,
}

/// Where the jars of the dependencies live: Maven repositories and the Gradle cache.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LocalRepositories {
    pub maven: Vec<PathBuf>,
    pub gradle: Vec<PathBuf>,
}

impl LocalRepositories {
    /// Reads the places from the environment and the settings. `settings` is the
    /// `settings.xml` a build passed explicitly; `gradle_home` is the Gradle user home
    /// when the project names one.
    pub fn detect(settings: Option<&Path>, gradle_home: Option<&Path>) -> Self {
        let mut maven = Vec::new();
        if let Some(home) = env::var_os("MAVEN_USER_HOME").map(PathBuf::from) {
            // The variable names the user home, whose repository is `repository`. A
            // caller may also point it straight at a repository.
            maven.push(home.clone());
            maven.push(home.join("repository"));
            if let Some(declared) = local_repository(&home.join("settings.xml")) {
                maven.push(declared);
            }
        }
        if let Some(home) = user_home() {
            let dot_m2 = home.join(".m2");
            if let Some(declared) = local_repository(&dot_m2.join("settings.xml")) {
                maven.push(declared);
            }
            maven.push(dot_m2.join("repository"));
        }
        if let Some(settings) = settings
            && let Some(declared) = local_repository(settings)
        {
            maven.push(declared);
        }
        dedup(&mut maven);

        let gradle_home = gradle_home
            .map(Path::to_path_buf)
            .or_else(|| env::var_os("GRADLE_USER_HOME").map(PathBuf::from))
            .or_else(|| user_home().map(|home| home.join(".gradle")));
        let gradle = gradle_home
            .into_iter()
            .map(|home| home.join("caches/modules-2/files-2.1"))
            .collect();
        Self { maven, gradle }
    }
}

fn user_home() -> Option<PathBuf> {
    base::program::home_dir()
}

fn dedup(paths: &mut Vec<PathBuf>) {
    let mut seen: Vec<PathBuf> = Vec::new();
    paths.retain(|path| {
        if seen.contains(path) {
            return false;
        }
        seen.push(path.clone());
        true
    });
}

/// `localRepository` of a Maven settings file, which may be outside the default place.
fn local_repository(settings: &Path) -> Option<PathBuf> {
    let text = fs::read_to_string(settings).ok()?;
    let document = Document::parse(&text).ok()?;
    let value = document
        .descendants()
        .find(|node| node.is_element() && node.tag_name().name() == "localRepository")?
        .text()?
        .trim();
    (!value.is_empty()).then(|| PathBuf::from(value))
}

/// The Lombok jar of a resolved project model. The path of the model wins; a version
/// without a file is looked up in the repositories, and without a version the newest
/// jar there is used.
pub fn find_lombok(model: &ProjectModel, repositories: &LocalRepositories) -> Option<Lombok> {
    let dependency = lombok_in_model(model)?;
    if let Some(jar) = dependency.jar.filter(|jar| jar.is_file()) {
        let version = dependency
            .version
            .or_else(|| version_of_jar(&jar))
            .unwrap_or_default();
        return Some(Lombok { version, jar });
    }
    resolve_lombok(repositories, dependency.version.as_deref())
}

/// The Lombok dependency of a build file on disk, for a model that has not caught up
/// with a dependency just added or removed.
pub fn find_lombok_in_build_files(root: &Path, repositories: &LocalRepositories) -> Option<Lombok> {
    let dependency = lombok_in_build_files(root)?;
    resolve_lombok(repositories, dependency.version.as_deref())
}

/// The jar of `version`, or the newest one when no version is known.
pub fn resolve_lombok(repositories: &LocalRepositories, version: Option<&str>) -> Option<Lombok> {
    let mut found = Vec::new();
    for repository in &repositories.maven {
        maven_candidates(repository, version, &mut found);
    }
    for cache in &repositories.gradle {
        gradle_candidates(cache, version, &mut found);
    }
    found
        .into_iter()
        .max_by(|left, right| compare_versions(&left.0, &right.0))
        .map(|(version, jar)| Lombok { version, jar })
}

#[derive(Default)]
struct FoundDependency {
    version: Option<String>,
    jar: Option<PathBuf>,
}

/// A Lombok library in the classpath of any unit of the model. A version from another
/// entry of the same dependency does not override the newer one.
fn lombok_in_model(model: &ProjectModel) -> Option<FoundDependency> {
    let mut found: Option<FoundDependency> = None;
    for module in &model.modules {
        for unit in &module.units {
            for entry in unit.compile_paths.iter().chain(&unit.runtime_paths) {
                let PathKind::Library { coordinates } = &entry.kind else {
                    continue;
                };
                let Some(version) = lombok_coordinates(coordinates) else {
                    continue;
                };
                let current = found.get_or_insert_with(FoundDependency::default);
                if let Some(version) = version
                    && current
                        .version
                        .as_deref()
                        .is_none_or(|known| compare_versions(&version, known).is_gt())
                {
                    current.version = Some(version);
                    current.jar = None;
                }
                if entry.path.is_file() {
                    current.jar = Some(entry.path.clone());
                }
            }
        }
    }
    found
}

/// `Some(version)` for a Lombok coordinate, with the version empty when the string does
/// not carry one. Maven writes `group:artifact:type:version`, Gradle `group:artifact:version`.
fn lombok_coordinates(coordinates: &str) -> Option<Option<String>> {
    let parts: Vec<&str> = coordinates.split(':').collect();
    if parts.len() < 2 || parts[0] != LOMBOK_GROUP || parts[1] != LOMBOK_ARTIFACT {
        return None;
    }
    let last = *parts.last()?;
    let version = (parts.len() > 2 && !is_artifact_type(last)).then(|| last.to_owned());
    Some(version)
}

fn is_artifact_type(value: &str) -> bool {
    matches!(
        value.to_ascii_lowercase().as_str(),
        "jar" | "pom" | "war" | "ear" | "zip" | "test-jar" | "java-source"
    )
}

/// The version in `lombok-1.18.38.jar`, used when the model names no version.
fn version_of_jar(jar: &Path) -> Option<String> {
    let name = jar.file_stem()?.to_string_lossy();
    let version = name.strip_prefix("lombok-")?;
    (!version.is_empty()).then(|| version.to_owned())
}

fn maven_candidates(repository: &Path, version: Option<&str>, found: &mut Vec<(String, PathBuf)>) {
    let directory = repository
        .join(LOMBOK_GROUP.replace('.', "/"))
        .join(LOMBOK_ARTIFACT);
    if let Some(version) = version {
        let jar = directory
            .join(version)
            .join(format!("lombok-{version}.jar"));
        if jar.is_file() {
            found.push((version.to_owned(), jar));
        }
        return;
    }
    let Ok(entries) = fs::read_dir(&directory) else {
        return;
    };
    for entry in entries.flatten() {
        let version = entry.file_name().to_string_lossy().into_owned();
        if !entry.path().is_dir() || version.starts_with('.') {
            continue;
        }
        let jar = entry.path().join(format!("lombok-{version}.jar"));
        if jar.is_file() {
            found.push((version, jar));
        }
    }
}

fn gradle_candidates(cache: &Path, version: Option<&str>, found: &mut Vec<(String, PathBuf)>) {
    let directory = cache.join(LOMBOK_GROUP).join(LOMBOK_ARTIFACT);
    let versions: Vec<PathBuf> = match version {
        Some(version) => {
            let path = directory.join(version);
            path.is_dir().then_some(path).into_iter().collect()
        }
        None => fs::read_dir(&directory)
            .map(|entries| {
                entries
                    .flatten()
                    .map(|entry| entry.path())
                    .filter(|path| path.is_dir())
                    .collect()
            })
            .unwrap_or_default(),
    };
    for path in versions {
        let version = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let Ok(hashes) = fs::read_dir(&path) else {
            continue;
        };
        for hash in hashes.flatten() {
            let Ok(files) = fs::read_dir(hash.path()) else {
                continue;
            };
            for file in files.flatten() {
                let name = file.file_name().to_string_lossy().into_owned();
                if name == format!("lombok-{version}.jar") {
                    found.push((version.clone(), file.path()));
                }
            }
        }
    }
}

/// Compares dotted versions numerically. Anything that is not a number counts as zero,
/// which is enough to tell the releases of one artifact apart.
fn compare_versions(left: &str, right: &str) -> std::cmp::Ordering {
    let key = |value: &str| -> Vec<u64> {
        value
            .split(['.', '-', '_'])
            .map(|part| {
                let digits: String = part
                    .chars()
                    .take_while(|character| character.is_ascii_digit())
                    .collect();
                digits.parse().unwrap_or(0)
            })
            .collect()
    };
    let mut left = key(left);
    let mut right = key(right);
    let length = left.len().max(right.len());
    left.resize(length, 0);
    right.resize(length, 0);
    left.cmp(&right)
}

/// Lombok in the build files, root only: a dependency just added there is the case the
/// model cannot see yet.
fn lombok_in_build_files(root: &Path) -> Option<LombokDependency> {
    let pom = fs::read_to_string(root.join("pom.xml"))
        .ok()
        .and_then(|text| lombok_in_pom(&text));
    if pom.is_some() {
        return pom;
    }
    ["build.gradle", "build.gradle.kts"]
        .into_iter()
        .find_map(|name| {
            fs::read_to_string(root.join(name))
                .ok()
                .and_then(|text| lombok_in_gradle(&text))
        })
}

fn lombok_in_pom(text: &str) -> Option<LombokDependency> {
    let document = Document::parse(text).ok()?;
    let dependency = document
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == "dependency")
        .find(|node| is_lombok_pom_node(*node))?;
    let raw = dependency
        .children()
        .find(|child| child.is_element() && child.tag_name().name() == "version")
        .and_then(|node| node.text())
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let version = raw.and_then(|value| resolve_property(value, text));
    Some(LombokDependency { version })
}

fn is_lombok_pom_node(node: roxmltree::Node<'_, '_>) -> bool {
    let text = |name: &str| {
        node.children()
            .find(|child| child.is_element() && child.tag_name().name() == name)
            .and_then(|child| child.text())
            .map(str::trim)
    };
    text("groupId") == Some(LOMBOK_GROUP) && text("artifactId") == Some(LOMBOK_ARTIFACT)
}

fn lombok_in_gradle(text: &str) -> Option<LombokDependency> {
    let marker = format!("{LOMBOK_GROUP}:{LOMBOK_ARTIFACT}");
    let line = text.lines().find(|line| line.contains(&marker))?;
    let after = &line[line.find(&marker)? + marker.len()..];
    let version = after
        .strip_prefix(':')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .and_then(|value| {
            let value = value.trim_start_matches(['"', '\'']);
            let end = value
                .find(|character: char| {
                    !(character.is_ascii_alphanumeric()
                        || matches!(character, '.' | '-' | '_' | '$' | '{' | '}'))
                })
                .unwrap_or(value.len());
            let value = &value[..end];
            resolve_property(value, text).filter(|value| value.chars().any(|c| c.is_ascii_digit()))
        });
    Some(LombokDependency { version })
}

/// `${name}`, `$name` or a plain value.
fn resolve_property(value: &str, text: &str) -> Option<String> {
    let name = if let Some(inner) = value.strip_prefix("${").and_then(|v| v.strip_suffix('}')) {
        inner
    } else if let Some(inner) = value.strip_prefix('$') {
        inner
    } else {
        return Some(value.to_owned());
    };
    let name = name.trim();
    for line in text.lines() {
        let line = line.trim().trim_end_matches(';');
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key
            .trim()
            .trim_start_matches("ext.")
            .trim_start_matches("val ")
            .trim_start_matches("def ")
            .trim();
        if key == name {
            let value = value.trim().trim_matches(['"', '\'']).trim();
            if !value.is_empty() {
                return Some(value.to_owned());
            }
        }
    }
    None
}
