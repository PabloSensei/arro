//! The build settings of a project the window turns on and off, and the arguments that
//! carry them to the tool. The answers live in `.arro/build.toml` of the project, so every
//! copy of the tool reads the same ones, whichever window or provider builds the project.

use std::fs;
use std::path::{Path, PathBuf};

use base::Result;
use project_model::BuildSelection;

/// The folder of the project that keeps the IDE's own files.
const METADATA_FOLDER: &str = ".arro";
/// The answers of the user for the build of the project.
const SELECTION_FILE: &str = "build.toml";

pub fn selection_file(root: &Path) -> PathBuf {
    root.join(METADATA_FOLDER).join(SELECTION_FILE)
}

/// A missing or broken file is the defaults: the build files decide alone, and nothing
/// here can lose text.
pub fn load_selection(root: &Path) -> BuildSelection {
    fs::read_to_string(selection_file(root))
        .ok()
        .and_then(|text| toml::from_str(&text).ok())
        .unwrap_or_default()
}

pub fn save_selection(root: &Path, selection: &BuildSelection) -> Result {
    let file = selection_file(root);
    if let Some(directory) = file.parent() {
        fs::create_dir_all(directory)?;
    }
    let text = toml::to_string_pretty(selection).map_err(|error| error.to_string())?;
    // The write replaces the file in one step: a build that reads it meanwhile sees the
    // old answers or the new ones, never half of a file.
    let temporary = file.with_extension("tmp");
    fs::write(&temporary, text)?;
    fs::rename(temporary, file)?;
    Ok(())
}

/// The profiles Maven takes: one `-P` list, `!` before the names the user turned off.
pub fn profile_args(selection: &BuildSelection) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for name in &selection.enabled {
        let name = name.trim();
        if !name.is_empty() {
            names.push(name.to_owned());
        }
    }
    for name in &selection.disabled {
        let name = name.trim().trim_start_matches('!');
        if !name.is_empty() {
            names.push(format!("!{name}"));
        }
    }
    if names.is_empty() {
        return Vec::new();
    }
    vec!["-P".into(), names.join(",")]
}

/// The named values Gradle takes: one `-Pname=value` per value.
pub fn property_args(selection: &BuildSelection) -> Vec<String> {
    selection
        .properties
        .iter()
        .filter_map(|property| {
            let name = property.name.trim();
            (!name.is_empty()).then(|| format!("-P{name}={}", property.value.trim()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BuildEnv, Gradle, Jdk, Maven};
    use proc_sup::ProcessSpec;
    use project_model::{BuildProperty, ProfileChoice};

    #[cfg(windows)]
    const MVN_WRAPPER: &str = "mvnw.cmd";
    #[cfg(not(windows))]
    const MVN_WRAPPER: &str = "mvnw";
    #[cfg(windows)]
    const GRADLE_WRAPPER: &str = "gradlew.bat";
    #[cfg(not(windows))]
    const GRADLE_WRAPPER: &str = "gradlew";

    fn arguments(spec: &ProcessSpec) -> Vec<String> {
        spec.arguments()
            .iter()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect()
    }

    fn env(root: &Path) -> BuildEnv {
        BuildEnv {
            jdk: Some(Jdk {
                home: root.join("jdk"),
                version: "17.0.3".into(),
                feature: 17,
            }),
            cache_root: Some(root.join("cache")),
            ..BuildEnv::default()
        }
    }

    #[test]
    fn the_profile_list_names_the_ones_turned_off_with_a_bang() {
        let mut selection = BuildSelection::default();
        selection.set_choice("dev", ProfileChoice::On);
        selection.set_choice("prod", ProfileChoice::Off);

        assert_eq!(
            profile_args(&selection),
            ["-P".to_owned(), "dev,!prod".to_owned()],
            "one list goes to Maven, the turned-off names with !"
        );
    }

    #[test]
    fn a_selection_without_profiles_has_no_arguments() {
        assert!(profile_args(&BuildSelection::default()).is_empty());
    }

    #[test]
    fn a_turned_off_name_goes_to_the_list_once() {
        let selection = BuildSelection {
            disabled: vec!["!prod".into()],
            ..BuildSelection::default()
        };

        assert_eq!(
            profile_args(&selection),
            ["-P".to_owned(), "!prod".to_owned()]
        );
    }

    #[test]
    fn the_values_become_one_argument_each() {
        let selection = BuildSelection {
            properties: vec![
                BuildProperty {
                    name: "workers".into(),
                    value: "4".into(),
                },
                BuildProperty {
                    name: " ".into(),
                    value: "ignored".into(),
                },
                BuildProperty {
                    name: "mode".into(),
                    value: "fast".into(),
                },
            ],
            ..BuildSelection::default()
        };

        assert_eq!(property_args(&selection), ["-Pworkers=4", "-Pmode=fast"]);
    }

    #[test]
    fn the_answers_survive_a_round_trip() {
        let root = test_support::scratch("build-selection-round-trip");
        let mut selection = BuildSelection::default();
        selection.set_choice("dev", ProfileChoice::On);
        selection.set_properties_text("mode=fast");

        save_selection(&root, &selection).unwrap();
        let read = load_selection(&root);

        assert_eq!(read, selection);
        assert!(
            selection_file(&root).is_file(),
            "the file belongs to the project"
        );
    }

    #[test]
    fn a_broken_file_is_the_defaults() {
        let root = test_support::scratch("build-selection-broken");
        fs::create_dir_all(root.join(METADATA_FOLDER)).unwrap();
        fs::write(selection_file(&root), "[broken").unwrap();

        assert_eq!(load_selection(&root), BuildSelection::default());
    }

    #[test]
    fn a_maven_command_carries_the_chosen_profiles() {
        let root = test_support::scratch("build-selection-maven");
        fs::write(root.join(MVN_WRAPPER), "@echo off\n").unwrap();
        fs::write(root.join("pom.xml"), "<project/>").unwrap();
        let mut selection = BuildSelection::default();
        selection.set_choice("dev", ProfileChoice::On);
        selection.set_choice("prod", ProfileChoice::Off);
        save_selection(&root, &selection).unwrap();

        let command = Maven::new(env(&root)).command(&root).unwrap();

        assert_eq!(arguments(&command), ["-B", "-ntp", "-P", "dev,!prod"]);
    }

    #[test]
    fn a_gradle_command_carries_the_named_values() {
        let root = test_support::scratch("build-selection-gradle");
        fs::write(root.join(GRADLE_WRAPPER), "@echo off\n").unwrap();
        fs::create_dir_all(root.join("gradle/wrapper")).unwrap();
        fs::write(root.join("gradle/wrapper/gradle-wrapper.jar"), b"jar").unwrap();
        let selection = BuildSelection {
            properties: vec![BuildProperty {
                name: "mode".into(),
                value: "fast".into(),
            }],
            ..BuildSelection::default()
        };
        save_selection(&root, &selection).unwrap();

        let command = Gradle::new(env(&root)).command(&root).unwrap();

        assert!(
            arguments(&command).contains(&"-Pmode=fast".to_owned()),
            "{:?}",
            arguments(&command)
        );
    }

    #[test]
    fn another_choice_makes_the_last_build_stale() {
        let root = test_support::scratch("build-selection-fingerprint");
        let before = crate::fingerprint::fingerprint(&root).unwrap();
        let mut selection = BuildSelection::default();
        selection.set_choice("dev", ProfileChoice::On);

        save_selection(&root, &selection).unwrap();

        assert_ne!(
            crate::fingerprint::fingerprint(&root).unwrap(),
            before,
            "the stamp of the last build does not hold with another choice"
        );
    }
}
