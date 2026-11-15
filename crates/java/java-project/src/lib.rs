//! Maven and Gradle projects: finding the tool and the JDK, a quick scan of project files,
//! and the full model the tool reports through the helpers in `helpers/`.

mod coverage;
mod database;
mod dependencies;
mod env;
mod environment;
mod fingerprint;
mod frameworks;
mod gradle;
pub mod heap;
pub mod initializr;
mod jdk;
mod lombok;
mod maven;
mod migrations;
mod pom;
pub mod profile;
mod sources;
mod test_runs;
pub mod updates;
mod wrapper;

pub use coverage::CoverageTools;
pub use database::{drivers, kind_of_jar};
pub use env::{BuildEnv, detect_build_system};
pub use environment::{
    ProjectSettings, ResolvedEnvironment, language_level, load_settings, project_jdk,
    resolve_environment, resolve_with, runtime_info, save_settings, settings_file,
};
pub use frameworks::{Framework, FrameworkLookup, Frameworks, frameworks_of};
pub use gradle::{Gradle, gradle_supports, wrapper_version};
pub use jdk::{Jdk, Requested, choose_jdk, feature_of, find_jdks, requested_jdk};
pub use lombok::{
    LOMBOK_ARTIFACT, LOMBOK_GROUP, LocalRepositories, Lombok, LombokDependency, find_lombok,
    find_lombok_in_build_files, resolve_lombok,
};
pub use maven::{Maven, find_maven};
pub use migrations::{
    Action, ActionKind, CHANGED_MIGRATION, ChangeSet, Changelog, DUPLICATE_CHANGESET,
    DUPLICATE_VERSION, File, History, HistoryRows, INVALID_NAME, Include, Issue, Kind,
    MISSING_INCLUDE, MigrationVersion, Model, Name, NameError, Next, State, Tool, checksum,
    new_file_name, parse_changelog, parse_name, scan, snake_case, states,
};
pub use pom::{Pom, read_pom, scan_poms};
pub use sources::{
    FetchError, Fetched, MAVEN_CENTRAL, SourceLocation, SourceStore, fetch, repositories_of,
    source_location,
};
pub use test_runs::{TEST_LISTENER, TestHook, TestSelection};
pub use updates::{BuildFile, Metadata, Slot, Updates, Version, VersionSource, kind_of, slot_at};
pub use wrapper::{Wrapper, generate as generate_wrapper};
