//! The environment of one project as the user may change it: the saved SDK, the folders
//! added by hand, the choice of the automatic rules and the level of the build.

use std::fs;
use std::path::{Path, PathBuf};

use base::Result;
use project_model::{LanguageLevelInfo, RuntimeInfo, RuntimeReason};
use serde::{Deserialize, Serialize};

use crate::jdk::{Family, Jdk, Requested, choose_jdk, feature_of, find_jdks, requested_jdk};
use crate::pom::read_pom;

/// The folder of the project that keeps the IDE's own files.
const METADATA_FOLDER: &str = ".arro";
/// The SDKs of the project, in the project and under git.
const SETTINGS_FILE: &str = "project.toml";
/// The newest language level the build files are known to name.
const NEWEST_LEVEL: u32 = 25;

/// What the user chose for one project.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjectSettings {
    /// The SDK the user picked, which wins over what the build files ask for.
    pub selected: Option<PathBuf>,
    /// SDK folders the user added by hand. Only these can be removed.
    pub added: Vec<PathBuf>,
}

/// Where the choices of the user live.
pub fn settings_file(root: &Path) -> PathBuf {
    root.join(METADATA_FOLDER).join(SETTINGS_FILE)
}

/// A missing or broken file is an empty choice: unlike the run configurations, there is
/// nothing here that a wrong file could lose, and the automatic rules still work.
pub fn load_settings(root: &Path) -> ProjectSettings {
    fs::read_to_string(settings_file(root))
        .ok()
        .and_then(|text| toml::from_str(&text).ok())
        .unwrap_or_default()
}

pub fn save_settings(root: &Path, settings: &ProjectSettings) -> Result {
    let file = settings_file(root);
    if let Some(directory) = file.parent() {
        fs::create_dir_all(directory)?;
    }
    fs::write(file, toml::to_string_pretty(settings)?)?;
    Ok(())
}

/// The SDKs of a project and the one it uses now.
#[derive(Clone, Debug, Default)]
pub struct ResolvedEnvironment {
    /// Found SDKs first, then the folders added by hand, with the mark of the latter.
    pub runtimes: Vec<(Jdk, bool)>,
    pub selected: Option<Jdk>,
    pub reason: RuntimeReason,
    /// What the user has to know, such as a saved folder that is gone.
    pub notice: Option<String>,
}

impl ResolvedEnvironment {
    pub fn jdks(&self) -> Vec<Jdk> {
        self.runtimes.iter().map(|(jdk, _)| jdk.clone()).collect()
    }
}

/// Reads the machine and the project. Blocks on the disk.
pub fn resolve_environment(root: &Path) -> ResolvedEnvironment {
    let default = std::env::var_os("JAVA_HOME").map(PathBuf::from);
    resolve_with(root, &find_jdks(), default.as_deref())
}

/// The same, with the SDKs already found, for a caller that scanned the machine itself
/// or for a test.
pub fn resolve_with(
    root: &Path,
    found: &[Jdk],
    default_home: Option<&Path>,
) -> ResolvedEnvironment {
    let settings = load_settings(root);
    let mut runtimes: Vec<(Jdk, bool)> = found.iter().cloned().map(|jdk| (jdk, false)).collect();
    for home in &settings.added {
        let home = project_path(root, home);
        if platform::is_remote_path(&home) {
            continue;
        }
        let Some(jdk) = Jdk::at(&home) else {
            continue;
        };
        if !runtimes
            .iter()
            .any(|(known, _)| same_home(&known.home, &jdk.home))
        {
            runtimes.push((jdk, true));
        }
    }
    let mut notice = None;
    let chosen = settings.selected.as_ref().and_then(|home| {
        let home = project_path(root, home);
        if platform::is_remote_path(&home) {
            notice = Some(format!("Network paths are not used: {}", home.display()));
            return None;
        }
        let jdk = Jdk::at(&home);
        if jdk.is_none() {
            notice = Some(format!(
                "The chosen SDK {} is no longer on disk; the project uses the automatic choice",
                home.display()
            ));
        }
        jdk
    });
    let mut resolved = ResolvedEnvironment {
        runtimes,
        selected: None,
        reason: RuntimeReason::Default,
        notice,
    };
    if let Some(jdk) = chosen {
        if !resolved
            .runtimes
            .iter()
            .any(|(known, _)| same_home(&known.home, &jdk.home))
        {
            resolved.runtimes.push((jdk.clone(), false));
        }
        resolved.selected = Some(jdk);
        resolved.reason = RuntimeReason::User;
        return resolved;
    }
    let requested = match requested_jdk(root) {
        Requested::Home(home) => Requested::Home(project_path(root, &home)),
        other => other,
    };
    let jdks: Vec<Jdk> = resolved.jdks();
    match choose_jdk(&requested, &jdks, default_home) {
        Ok(jdk) => {
            if !resolved
                .runtimes
                .iter()
                .any(|(known, _)| same_home(&known.home, &jdk.home))
            {
                resolved.runtimes.push((jdk.clone(), false));
            }
            resolved.reason = match requested {
                Requested::Nothing => RuntimeReason::Default,
                Requested::Feature(_) | Requested::Home(_) => RuntimeReason::Project,
            };
            resolved.selected = Some(jdk);
        }
        Err(error) => {
            resolved.notice = Some(error.to_string());
        }
    }
    resolved
}

/// The JDK that builds and runs the project: the choice of the user first, then what the
/// build files ask for.
pub fn project_jdk(root: &Path) -> Result<Jdk> {
    let resolved = resolve_environment(root);
    resolved.selected.ok_or_else(|| {
        resolved
            .notice
            .clone()
            .unwrap_or_else(|| "no SDK found".to_owned())
            .into()
    })
}

fn same_home(one: &Path, other: &Path) -> bool {
    same_home_on(Family::current(), one, other)
}

/// A path of the project settings: a relative one counts from the root of the project,
/// not from the folder the process happens to run in.
fn project_path(root: &Path, path: &Path) -> PathBuf {
    if platform::is_remote_path(path) || path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

/// Windows and macOS ignore the case of a path, the other systems keep it: two folders
/// that differ in case only are two folders on Linux. `family` is a parameter, so a test
/// checks every system on any machine.
fn same_home_on(family: Family, one: &Path, other: &Path) -> bool {
    let canonical = |path: &Path| fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
    let key = |path: &Path| match family {
        Family::Windows | Family::MacOs => PathBuf::from(path.to_string_lossy().to_lowercase()),
        Family::Unix => path.to_path_buf(),
    };
    key(&canonical(one)) == key(&canonical(other))
}

/// What the SDKs page shows for one SDK.
pub fn runtime_info(jdk: &Jdk, added: bool) -> RuntimeInfo {
    let name = jdk
        .home
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| format!("JDK {}", jdk.feature));
    RuntimeInfo {
        name,
        version: jdk.version.clone(),
        feature: jdk.feature,
        home: jdk.home.clone(),
        added,
    }
}

/// The level of the build file, with the line that decides it. A language level written
/// in a script that cannot be read is not guessed.
pub fn language_level(root: &Path) -> Option<LanguageLevelInfo> {
    ["pom.xml", "build.gradle", "build.gradle.kts"]
        .into_iter()
        .map(|name| root.join(name))
        .find(|file| file.is_file())
        .and_then(|file| language_level_at(&file))
}

/// The level of one build file, for the page of a module. A build file that names no
/// level still answers, so that the page can offer to add one.
pub fn language_level_at(file: &Path) -> Option<LanguageLevelInfo> {
    let name = file.file_name()?.to_str()?;
    let found = if name.eq_ignore_ascii_case("pom.xml") {
        level_in_pom(file)?
    } else if name.eq_ignore_ascii_case("build.gradle")
        || name.eq_ignore_ascii_case("build.gradle.kts")
    {
        level_in_gradle(file)?
    } else {
        return None;
    };
    let mut values: Vec<String> = (8..=NEWEST_LEVEL).map(|level| level.to_string()).collect();
    if let Some(selected) = &found.selected
        && !values.contains(selected)
    {
        values.push(selected.clone());
    }
    Some(LanguageLevelInfo {
        values,
        selected: found.selected,
        file: Some(found.file),
        line: found.line,
    })
}

struct FoundLevel {
    selected: Option<String>,
    file: PathBuf,
    line: Option<u32>,
}

/// `maven.compiler.release`, then `target` and `source`, then `java.version`, then the
/// configuration of the compiler plugin.
fn level_in_pom(file: &Path) -> Option<FoundLevel> {
    let text = fs::read_to_string(file).ok()?;
    let release = read_pom(file).ok()?.java_release;
    let mut selected = release.and_then(|value| feature_of(&value));
    let keys = [
        "maven.compiler.release",
        "maven.compiler.target",
        "maven.compiler.source",
        "java.version",
    ];
    let mut line = keys.iter().find_map(|key| {
        let marker = format!("<{key}>");
        text.lines()
            .position(|line| line.contains(&marker))
            .map(|line| line as u32)
    });
    if selected.is_none()
        && let Some((level, at)) = crate::level::plugin_level(&text)
    {
        selected = Some(level);
        line = Some(at);
    }
    Some(FoundLevel {
        selected: selected.map(|level| level.to_string()),
        file: file.to_owned(),
        line,
    })
}

/// A toolchain line, the older `JavaVersion` constants or a source compatibility line.
/// A readable script without one still answers, with no level and no line.
fn level_in_gradle(file: &Path) -> Option<FoundLevel> {
    let text = fs::read_to_string(file).ok()?;
    for (index, line) in text.lines().enumerate() {
        let number = ["JavaLanguageVersion.of(", "JavaVersion.VERSION_"]
            .iter()
            .find_map(|marker| digits_after(line, marker))
            .or_else(|| {
                line.contains("sourceCompatibility")
                    .then(|| after_equals(line))
            });
        if let Some(level) = number {
            return Some(FoundLevel {
                selected: Some(level.to_string()),
                file: file.to_owned(),
                line: Some(index as u32),
            });
        }
    }
    Some(FoundLevel {
        selected: None,
        file: file.to_owned(),
        line: None,
    })
}

fn digits_after(line: &str, marker: &str) -> Option<u32> {
    let (_, rest) = line.split_once(marker)?;
    let rest = rest.strip_prefix("1_").unwrap_or(rest);
    let end = rest
        .find(|character: char| !character.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

fn after_equals(line: &str) -> u32 {
    line.split_once('=')
        .map(|(_, value)| value.trim())
        .and_then(feature_of)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        test_support::scratch(&format!("project-env-{name}"))
    }

    #[cfg(windows)]
    fn jdk(root: &Path, name: &str, version: &str) -> Jdk {
        let home = root.join(name);
        fs::create_dir_all(home.join("bin")).unwrap();
        fs::write(home.join("bin/javac.exe"), "").unwrap();
        fs::write(home.join("bin/java.exe"), "").unwrap();
        fs::write(
            home.join("release"),
            format!("JAVA_VERSION=\"{version}\"\n"),
        )
        .unwrap();
        Jdk::at(&home).unwrap()
    }

    fn pom(root: &Path, release: u32) {
        let text = format!(
            "<project><properties><maven.compiler.release>{release}</maven.compiler.release>\
             </properties></project>"
        );
        fs::write(root.join("pom.xml"), text).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn the_choice_of_the_user_wins_over_the_build_file() {
        let root = scratch("user");
        pom(&root, 17);
        let seventeen = jdk(&root, "one", "17.0.2");
        let twenty_one = jdk(&root, "two", "21.0.2");
        save_settings(
            &root,
            &ProjectSettings {
                selected: Some(twenty_one.home.clone()),
                added: Vec::new(),
            },
        )
        .unwrap();

        let resolved = resolve_with(
            &root,
            &[seventeen.clone(), twenty_one.clone()],
            Some(&seventeen.home),
        );

        assert_eq!(resolved.selected.as_ref(), Some(&twenty_one));
        assert_eq!(resolved.reason, RuntimeReason::User);
        assert!(resolved.notice.is_none());
    }

    #[cfg(windows)]
    #[test]
    fn a_folder_that_is_gone_falls_back_and_says_so() {
        let root = scratch("gone");
        pom(&root, 17);
        let seventeen = jdk(&root, "one", "17.0.2");
        save_settings(
            &root,
            &ProjectSettings {
                selected: Some(root.join("missing")),
                added: Vec::new(),
            },
        )
        .unwrap();

        let resolved = resolve_with(&root, std::slice::from_ref(&seventeen), None);

        assert_eq!(resolved.selected.as_ref(), Some(&seventeen));
        assert_eq!(resolved.reason, RuntimeReason::Project);
        let notice = resolved.notice.unwrap_or_default();
        assert!(notice.contains("missing"), "{notice}");
    }

    #[cfg(windows)]
    #[test]
    fn the_build_file_names_the_reason() {
        let root = scratch("project");
        pom(&root, 21);
        let seventeen = jdk(&root, "one", "17.0.2");
        let twenty_one = jdk(&root, "two", "21.0.2");

        let resolved = resolve_with(&root, &[seventeen, twenty_one.clone()], None);

        assert_eq!(resolved.selected.as_ref(), Some(&twenty_one));
        assert_eq!(resolved.reason, RuntimeReason::Project);
    }

    #[cfg(windows)]
    #[test]
    fn without_build_files_the_default_reason_is_reported() {
        let root = scratch("default");
        let seventeen = jdk(&root, "one", "17.0.2");

        let resolved = resolve_with(
            &root,
            std::slice::from_ref(&seventeen),
            Some(&seventeen.home),
        );

        assert_eq!(resolved.selected.as_ref(), Some(&seventeen));
        assert_eq!(resolved.reason, RuntimeReason::Default);
    }

    #[cfg(windows)]
    #[test]
    fn a_folder_added_by_hand_keeps_its_mark() {
        let root = scratch("added");
        let added = jdk(&root, "one", "21.0.2");
        save_settings(
            &root,
            &ProjectSettings {
                selected: None,
                added: vec![added.home.clone()],
            },
        )
        .unwrap();

        let resolved = resolve_with(&root, &[], None);

        assert_eq!(resolved.runtimes.len(), 1);
        assert!(resolved.runtimes[0].1);
        assert_eq!(resolved.runtimes[0].0.home, added.home);
    }

    #[test]
    fn the_level_of_a_pom_comes_with_its_line() {
        let root = scratch("pom-level");
        pom(&root, 21);

        let level = language_level(&root).unwrap();

        assert_eq!(level.selected.as_deref(), Some("21"));
        assert!(level.values.iter().any(|value| value == "17"));
        assert!(level.file.unwrap().ends_with("pom.xml"));
        assert!(level.line.is_some());
    }

    #[test]
    fn the_level_of_the_compiler_plugin_comes_with_its_line() {
        let root = scratch("plugin-level");
        fs::write(
            root.join("pom.xml"),
            "<project><build><plugins><plugin>\
             <artifactId>maven-compiler-plugin</artifactId><configuration>\
             <release>21</release></configuration></plugin></plugins></build></project>",
        )
        .unwrap();

        let level = language_level(&root).unwrap();

        assert_eq!(level.selected.as_deref(), Some("21"));
        assert!(level.line.is_some());
    }

    #[test]
    fn the_level_of_a_gradle_script_comes_with_its_line() {
        let root = scratch("gradle-level");
        fs::write(
            root.join("build.gradle"),
            "plugins { id 'java' }\njava { toolchain { languageVersion = JavaLanguageVersion.of(17) } }\n",
        )
        .unwrap();

        let level = language_level(&root).unwrap();

        assert_eq!(level.selected.as_deref(), Some("17"));
        assert_eq!(level.line, Some(1));
    }

    #[test]
    fn a_project_without_build_files_has_no_level() {
        let root = scratch("no-level");

        assert!(language_level(&root).is_none());
    }

    #[test]
    fn only_windows_and_mac_ignore_the_case_of_a_folder() {
        let one = Path::new("/jdks/Temurin");
        let other = Path::new("/jdks/temurin");

        assert!(!same_home_on(Family::Unix, one, other));
        assert!(same_home_on(Family::Windows, one, other));
        assert!(same_home_on(Family::MacOs, one, other));
        assert!(same_home_on(Family::Unix, one, one));
    }
}
