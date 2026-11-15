//! The choices of the user for the checks of the project, in `.arro/inspections.toml`.
//! The file belongs to the project and travels with it, as the other project settings do.

use std::fs;
use std::path::{Path, PathBuf};

use lang_api::InspectionSettings;

/// The folder of the project that keeps the IDE's own files.
const METADATA_FOLDER: &str = ".arro";
/// The checks of the project.
const INSPECTIONS_FILE: &str = "inspections.toml";

pub fn inspections_file(root: &Path) -> PathBuf {
    root.join(METADATA_FOLDER).join(INSPECTIONS_FILE)
}

/// A missing or broken file is the defaults of the checks: nothing here can lose text.
pub fn load_inspections(root: &Path) -> InspectionSettings {
    fs::read_to_string(inspections_file(root))
        .ok()
        .and_then(|text| toml::from_str(&text).ok())
        .unwrap_or_default()
}

pub fn save_inspections(root: &Path, settings: &InspectionSettings) -> Result<(), String> {
    let file = inspections_file(root);
    if let Some(directory) = file.parent() {
        fs::create_dir_all(directory).map_err(|error| error.to_string())?;
    }
    let text = toml::to_string_pretty(settings).map_err(|error| error.to_string())?;
    fs::write(file, text).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use lang_api::{InspectionLevel, InspectionSetting};

    use super::*;

    #[test]
    fn the_choices_of_the_user_survive_a_round_trip() {
        let root = test_support::scratch("inspections-round-trip");
        let mut settings = InspectionSettings::default();
        settings.set(
            "spring.config.unknownProperty",
            true,
            Some(InspectionLevel::Error),
        );
        settings.set(
            "org.eclipse.jdt.core.compiler.problem.unusedImport",
            false,
            None,
        );

        save_inspections(&root, &settings).unwrap();
        let read = load_inspections(&root);

        assert_eq!(read, settings);
        assert_eq!(
            read.setting("org.eclipse.jdt.core.compiler.problem.unusedImport"),
            Some(InspectionSetting {
                enabled: false,
                level: None,
            })
        );
    }

    #[test]
    fn a_broken_file_is_the_defaults() {
        let root = test_support::scratch("inspections-broken");
        fs::create_dir_all(root.join(METADATA_FOLDER)).unwrap();
        fs::write(inspections_file(&root), "[broken").unwrap();

        assert_eq!(load_inspections(&root), InspectionSettings::default());
    }
}
