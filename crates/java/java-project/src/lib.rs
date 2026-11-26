//! Maven and Gradle projects: finding the tool and the JDK, a quick scan of project files,
//! and the full model the tool reports through the helpers in `helpers/`.

mod build_selection;
mod coverage;
mod database;
mod dependencies;
mod env;
mod environment;
mod fingerprint;
mod frameworks;
mod gradle;
mod gradle_download;
mod gradle_wrapper;
pub mod heap;
pub mod initializr;
mod jdk;
mod level;
mod lombok;
mod maven;
mod maven_download;
mod migrations;
mod pom;
pub mod profile;
mod sources;
mod test_runs;
mod tool_download;
mod tool_home;
pub mod updates;
mod wrapper;

#[cfg(test)]
mod security_tests;

pub use coverage::CoverageTools;
pub use database::{drivers, kind_of_jar};
pub use env::{BuildEnv, detect_build_system};
pub use environment::{
    ProjectSettings, ResolvedEnvironment, language_level, language_level_at, load_settings,
    project_jdk, resolve_environment, resolve_with, runtime_info, save_settings, settings_file,
};
pub use frameworks::{Framework, FrameworkLookup, Frameworks, frameworks_of};
pub use gradle::{
    Gradle, GradleHome, GradleOrigin, MissingGradle, bundled_home as bundled_gradle, find_gradle,
    find_gradle_home, gradle_supports, wrapper_version,
};
pub use jdk::{
    Family, Jdk, Places, Requested, choose_jdk, feature_of, find_jdks, home_in, homes_under,
    requested_jdk, roots_for,
};
pub use level::set_language_level;
pub use lombok::{
    LOMBOK_ARTIFACT, LOMBOK_GROUP, LocalRepositories, Lombok, LombokDependency, find_lombok,
    find_lombok_in_build_files, resolve_lombok,
};
pub use maven::{
    Maven, MavenHome, MavenOrigin, MissingMaven, bundled_home, find_maven, find_maven_home,
};
pub use migrations::{
    Action, ActionKind, CHANGED_MIGRATION, ChangeSet, Changelog, DUPLICATE_CHANGESET,
    DUPLICATE_VERSION, File, History, HistoryRows, INVALID_NAME, Include, Issue, Kind,
    MISSING_INCLUDE, MigrationVersion, Model, Name, NameError, Next, State, Tool, checksum,
    new_file_name, parse_changelog, parse_name, scan, snake_case, states,
};
pub use pom::{Pom, PomProfile, read_pom, scan_poms};
pub use sources::{
    FetchError, Fetched, MAVEN_CENTRAL, SourceLocation, SourceStore, fetch, repositories_of,
    source_location,
};
pub use test_runs::{TEST_LISTENER, TestHook, TestSelection};
pub use tool_download::download_agent;
pub use updates::{BuildFile, Metadata, Slot, Updates, Version, VersionSource, kind_of, slot_at};
pub use wrapper::{Wrapper, generate as generate_wrapper, generate_with as generate_wrapper_with};
